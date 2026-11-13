//! # A sketch, and the one recurrence every variant runs on it
//!
//! The colour recurrence is `C = sum_i s_i C_i`: a transaction's colour is the
//! share-weighted mixture of the colours its inputs spend.  Exactly, that is
//! [`crate::weighted`], and over the whole chain it does not fit -- colours
//! reach hundreds of thousands of blocks and millions of them are live at
//! once.  A *sketch* keeps about `k` of a colour's terms and re-weights them so
//! that what it says about the colour is right on average, and this module is
//! everything the three sketching variants share: the state, the n-way merge
//! that forms a record's candidates, and the bookkeeping.  What differs between
//! them is only the reduction -- [`crate::varopt`], [`crate::prio`],
//! [`crate::coord`] -- and how a sketch is read back as a distribution.
//!
//! ## Once per record, not once per input
//!
//! `run()` folds a record's inputs pairwise, right to left.  With a sketch in
//! place of the exact colour that would mean a reduction after every pairwise
//! step, so a consolidation of a thousand inputs would be resampled a thousand
//! times, each one adding variance to everything folded in before it.  By
//! default ([`Reduce::Record`]) the record's operands are merged *exactly* --
//! their f32 weights times their shares, summed in f64 -- and reduced once.
//! The recurrence is unchanged; for `varopt` and `prio` this is literally
//! "foldr with an exact combine, then reduce".  `reduce=step` is the literal
//! pairwise fold with a reduction after every combine, kept as a knob so the
//! difference can be measured.
//!
//! ## The free paths
//!
//! Most transactions have one input, or spend one parent several times.  Their
//! operands are grouped by `Rc` pointer -- `s1.C + s2.C = (s1 + s2).C` exactly
//! -- and a record whose operands are one group answers the parent's own `Rc`:
//! no copy, no allocation, no reduction, and `sum s_i = 1` makes the scale the
//! identity.  That is the commonest record of the chain and it costs a
//! refcount bump.
//!
//! Operands with share 0 are skipped entirely -- a zero-value input inside a
//! record of positive total.  The chain has tens of thousands of them, and a
//! zero-share parent merged anyway adds up to `k` zero-weight candidates: that
//! pushes `n` past `k` when the real support is smaller, forcing a reduction of
//! an exact colour, and in the textbook VarOpt loop it made the threshold
//! negative.  Skipping them also keeps a reduced parent with no say in the
//! child from marking the child `reduced`.
//!
//! ## The n-way merge
//!
//! Two groups merge with two pointers.  Three or more go through a balanced
//! tree of two-pointer merges: the groups pairwise, then the results pairwise,
//! `ceil(log2 G)` levels in all.  That is `O(N log G)` rather than the
//! `O(N log N)` of sorting the union, which is what the prototype did, and it
//! beats a heap-based k-way merge with the same bound by a wide margin in
//! practice: measured over the first 6M records at `k = 256`, a heap cost 21
//! ns a candidate for 3-8 groups and 50 ns above that, where a two-pointer
//! pass costs 4.5 -- the heap's sift is a chain of unpredictable branches,
//! and the pass is one predictable compare.  The summation order is fixed by
//! the tree, so it is deterministic.  Candidates whose weight rounds to zero
//! are dropped before `n` is counted.
//!
//! ## Storage
//!
//! Weights are `f32` in a sketch and `f64` inside a merge or a reduction: 8
//! bytes an entry, `(u32 block, f32 weight)`, and a relative error of 6e-8 per
//! store, which is far below anything a `k`-point estimate can resolve.  A
//! weight that rounds to zero or to a subnormal is dropped; it is below 1.2e-38
//! of a unit of value.  Every sketch is allocated at its exact length.
//!
//! The live-entry and live-sketch counters are thread-local and maintained by
//! `Sketch`'s constructor and `Drop`.  Each store runs on its own thread, so
//! they count per store with no atomics.

use crate::coord;
use crate::hash::{self, Keys};
use crate::prio;
use crate::varopt;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// One term of a sketch: 8 bytes.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Entry {
    pub b: u32,
    pub w: f32,
}

/// A sketched colour.
#[derive(Debug)]
pub struct Sketch {
    /// Strictly ascending `b`; every `w` positive, finite and normal.
    pub entries: Box<[Entry]>,
    /// varopt: the light weight of this node's own reduction, or 0.  prio: its
    /// `tau_q`, or 0.  coord: the threshold in this sketch's own scale, `+INF`
    /// when exact.
    pub tau: f64,
    /// A reduction happened here or in an ancestor that had a say.  `false`
    /// means the sketch *is* the exact colour, up to f32 storage.
    pub reduced: bool,
}

pub type Handle = Rc<Sketch>;

/// What a store holds right now and has held at most.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Usage {
    pub live_entries: u64,
    pub peak_entries: u64,
    pub live_sketches: u64,
    pub peak_sketches: u64,
}

thread_local! {
    static USAGE: Cell<Usage> = const { Cell::new(Usage {
        live_entries: 0, peak_entries: 0, live_sketches: 0, peak_sketches: 0,
    }) };
}

/// This thread's sketch usage.
pub fn usage() -> Usage {
    USAGE.with(|u| u.get())
}

impl Sketch {
    pub fn new(entries: Box<[Entry]>, tau: f64, reduced: bool) -> Handle {
        let n = entries.len() as u64;
        USAGE.with(|u| {
            let mut x = u.get();
            x.live_entries += n;
            x.live_sketches += 1;
            x.peak_entries = x.peak_entries.max(x.live_entries);
            x.peak_sketches = x.peak_sketches.max(x.live_sketches);
            u.set(x);
        });
        Rc::new(Sketch { entries, tau, reduced })
    }

    /// Store f64 candidates as a sketch: rounded to f32, zero and subnormal
    /// weights dropped, allocated at the exact length.
    pub fn stored(cand: &[(u32, f64)], tau: f64, reduced: bool) -> Handle {
        let mut v: Vec<Entry> = Vec::with_capacity(cand.len());
        for &(b, w) in cand {
            let w = w as f32;
            if w.is_normal() && w > 0.0 {
                v.push(Entry { b, w });
            }
        }
        Sketch::new(v.into_boxed_slice(), tau, reduced)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

impl Drop for Sketch {
    fn drop(&mut self) {
        let n = self.entries.len() as u64;
        USAGE.with(|u| {
            let mut x = u.get();
            x.live_entries -= n;
            x.live_sketches -= 1;
            u.set(x);
        });
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variant {
    VarOpt,
    Prio,
    Coord,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reduce {
    Record,
    Step,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cap {
    Min,
    Harm,
}

/// One sketch configuration: `<variant>:<k>[:seed=<u64>][:reduce=record|step]
/// [:cap=min|harm][:floor=<n>]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub variant: Variant,
    pub k: usize,
    pub seed: u64,
    pub reduce: Reduce,
    pub cap: Cap,
    pub floor: usize,
}

impl Config {
    pub fn parse(text: &str) -> Result<Config, String> {
        let mut parts = text.split(':');
        let variant = match parts.next() {
            Some("varopt") => Variant::VarOpt,
            Some("prio") => Variant::Prio,
            Some("coord") => Variant::Coord,
            Some(side @ ("kmv" | "l1")) => {
                return Err(format!("config {:?}: {} is a side store: pass --{}", text, side, side))
            }
            other => return Err(format!("config {:?}: unknown variant {:?} (varopt, prio, coord)", text, other.unwrap_or(""))),
        };
        let k: usize = parts
            .next()
            .and_then(|k| k.parse().ok())
            .ok_or_else(|| format!("config {:?}: expected <variant>:<k>", text))?;
        if !(1..=1 << 20).contains(&k) {
            return Err(format!("config {:?}: k must be in 1..=1048576", text));
        }
        let mut c = Config { variant, k, seed: 1, reduce: Reduce::Record, cap: Cap::Min, floor: 0 };
        for opt in parts {
            let (key, value) = opt
                .split_once('=')
                .ok_or_else(|| format!("config {:?}: option {:?} is not key=value", text, opt))?;
            let bad = || format!("config {:?}: bad value in {:?}", text, opt);
            match key {
                "seed" => c.seed = value.parse().map_err(|_| bad())?,
                "reduce" => {
                    c.reduce = match value {
                        "record" => Reduce::Record,
                        "step" => Reduce::Step,
                        _ => return Err(bad()),
                    }
                }
                "cap" => {
                    c.cap = match value {
                        "min" => Cap::Min,
                        "harm" => Cap::Harm,
                        _ => return Err(bad()),
                    }
                }
                "floor" => c.floor = value.parse().map_err(|_| bad())?,
                _ => return Err(format!("config {:?}: unknown option {:?}", text, key)),
            }
        }
        if c.variant != Variant::Coord && (c.cap != Cap::Min || c.floor != 0) {
            return Err(format!("config {:?}: cap and floor are coord's options", text));
        }
        Ok(c)
    }

    /// The canonical label used in file names and columns:
    /// `varopt-k256-s1-rec`, `coord-k64-s1-rec-min-f0`.
    pub fn label(&self) -> String {
        let v = match self.variant {
            Variant::VarOpt => "varopt",
            Variant::Prio => "prio",
            Variant::Coord => "coord",
        };
        let r = match self.reduce {
            Reduce::Record => "rec",
            Reduce::Step => "step",
        };
        let mut s = format!("{}-k{}-s{}-{}", v, self.k, self.seed, r);
        if self.variant == Variant::Coord {
            let c = match self.cap {
                Cap::Min => "min",
                Cap::Harm => "harm",
            };
            s.push_str(&format!("-{}-f{}", c, self.floor));
        }
        s
    }
}

/// What a colour says about itself, for the metric columns.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Info {
    pub n: usize,
    pub tau: f64,
    pub reduced: bool,
}

/// Counters a store keeps about its own work.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Work {
    pub reductions: u64,
    /// Reductions that kept `k - 1` or `k + 1` points (varopt rounding).
    pub off_by_one: u64,
}

/// The operations the driver needs from a colour representation.
pub trait Algebra {
    /// A handle on a colour; cloning one is a refcount bump.
    type C: Clone;
    fn singleton(&mut self, block: u32) -> Self::C;
    /// `ops` in FOLD ORDER (index 0 = the rightmost input, as `run()` walks
    /// them), each an owned handle and its share.  `rec` is the record index.
    fn record(&mut self, ops: &[(Self::C, f64)], rec: u64) -> Self::C;
    /// The normalised estimate, ascending by block, into `out` (cleared first).
    fn emit(&self, c: &Self::C, out: &mut Vec<(u32, f64)>);
    /// The stored total before normalisation (a diagnostic; 1 for exact).
    fn raw_mass(&self, c: &Self::C) -> f64;
    fn info(&self, c: &Self::C) -> Info;
    fn work(&self) -> Work;
    /// `(live entries, peak entries, live distinct colours, peak)`.
    fn usage(&self) -> Usage;
    /// Give a handle back.  Plain `drop` for sketches; the exact store keeps a
    /// count of its own.
    fn release(&mut self, c: Self::C) {
        drop(c)
    }
}

/// Candidate entries (16 B each) the merge tree's pool of scratch runs may
/// keep between records: 32 MB.  Ordinary merges stay well inside it.
pub const SPARE_BUDGET: usize = 1 << 21;

/// The three sketching variants behind one [`Algebra`].
pub struct Sketcher {
    pub cfg: Config,
    keys: Option<Arc<Keys>>,
    work: Work,
    // Scratch, reused by every record.
    groups: Vec<(usize, f64)>,
    group_of: HashMap<usize, usize>,
    cand: Vec<(u32, f64)>,
    out: Vec<(u32, f64)>,
    runs: Vec<Vec<(u32, f64)>>,
    next: Vec<Vec<(u32, f64)>>,
    spare: Vec<Vec<(u32, f64)>>,
    caps: Vec<(f64, f64)>,
    vo: varopt::Scratch,
    pr: prio::Scratch,
    co: coord::Scratch,
}

impl Sketcher {
    /// `keys` is required for `coord` and ignored otherwise.
    pub fn new(cfg: Config, keys: Option<Arc<Keys>>) -> Sketcher {
        if cfg.variant == Variant::Coord {
            assert!(keys.is_some(), "coord needs its key table");
        }
        Sketcher {
            cfg,
            keys,
            work: Work::default(),
            groups: Vec::new(),
            group_of: HashMap::new(),
            cand: Vec::new(),
            out: Vec::new(),
            runs: Vec::new(),
            next: Vec::new(),
            spare: Vec::new(),
            caps: Vec::new(),
            vo: varopt::Scratch::default(),
            pr: prio::Scratch::default(),
            co: coord::Scratch::default(),
        }
    }

    fn exact_tau(&self) -> f64 {
        match self.cfg.variant {
            Variant::Coord => f64::INFINITY,
            _ => 0.0,
        }
    }

    /// Group the nonzero-share operands by pointer, in order of first
    /// occurrence, into `self.groups` as `(first op index, summed share)`.
    fn group(&mut self, ops: &[(Handle, f64)]) {
        self.groups.clear();
        if ops.len() <= 16 {
            for (i, (h, s)) in ops.iter().enumerate() {
                if *s == 0.0 {
                    continue;
                }
                match self.groups.iter_mut().find(|g| Rc::ptr_eq(&ops[g.0].0, h)) {
                    Some(g) => g.1 += *s,
                    None => self.groups.push((i, *s)),
                }
            }
        } else {
            self.group_of.clear();
            for (i, (h, s)) in ops.iter().enumerate() {
                if *s == 0.0 {
                    continue;
                }
                match self.group_of.get(&(Rc::as_ptr(h) as usize)) {
                    Some(&g) => self.groups[g].1 += *s,
                    None => {
                        self.group_of.insert(Rc::as_ptr(h) as usize, self.groups.len());
                        self.groups.push((i, *s));
                    }
                }
            }
        }
    }

    /// Merge the grouped operands into `self.cand`: ascending, weights
    /// `sum_g S_g . w_g[b]` in f64, zeros dropped.
    fn merge_groups(&mut self, ops: &[(Handle, f64)]) {
        let cand = &mut self.cand;
        cand.clear();
        let g = &self.groups;
        if g.len() == 2 {
            let (a, sa) = (&ops[g[0].0].0.entries, g[0].1);
            let (b, sb) = (&ops[g[1].0].0.entries, g[1].1);
            merge_two(a, sa, b, sb, cand);
            return;
        }
        // Three or more: a balanced tree of two-pointer merges.  The first
        // level reads the sketches themselves, two at a time; every level
        // after merges the previous level's runs pairwise, until one is left.
        let mut runs = std::mem::take(&mut self.runs);
        let mut next = std::mem::take(&mut self.next);
        runs.clear();
        for pair in g.chunks(2) {
            let mut v = self.spare.pop().unwrap_or_default();
            v.clear();
            let (a, sa) = (&ops[pair[0].0].0.entries, pair[0].1);
            match pair.get(1) {
                Some(&(j, sb)) => merge_two(a, sa, &ops[j].0.entries, sb, &mut v),
                None => merge_two(a, sa, &[], 0.0, &mut v),
            }
            runs.push(v);
        }
        while runs.len() > 1 {
            next.clear();
            let mut it = runs.drain(..);
            while let Some(a) = it.next() {
                match it.next() {
                    Some(b) => {
                        let mut v = self.spare.pop().unwrap_or_default();
                        v.clear();
                        merge_runs(&a, &b, &mut v);
                        self.spare.push(a);
                        self.spare.push(b);
                        next.push(v);
                    }
                    None => next.push(a),
                }
            }
            drop(it);
            std::mem::swap(&mut runs, &mut next);
        }
        let merged = runs.pop().expect("at least one run");
        let old = std::mem::replace(&mut self.cand, merged);
        self.spare.push(old);
        self.runs = runs;
        self.next = next;
        // The pool now holds about (inputs x levels) of capacity.  After a
        // consolidation of thousands of parents that is hundreds of MB, which
        // would otherwise stay with the store for the rest of the run.
        let inputs: usize = g.iter().map(|&(i, _)| ops[i].0.entries.len()).sum();
        let levels = (usize::BITS - g.len().leading_zeros()) as usize;
        if inputs.saturating_mul(levels) > SPARE_BUDGET / 4 {
            let mut left = SPARE_BUDGET;
            self.spare.retain(|v| {
                let keep = v.capacity() <= left;
                if keep {
                    left -= v.capacity();
                }
                keep
            });
            self.spare.shrink_to_fit();
        }
    }

    /// Turn the merged candidates into a sketch: store them as they are when
    /// the variant allows, otherwise reduce.  `caps` is coord's
    /// `(share, tau)` per operand, `j` the step index.
    fn finish(&mut self, reduced_in: bool, caps: &[(f64, f64)], rec: u64, j: u64) -> Handle {
        let k = self.cfg.k;
        let n = self.cand.len();
        match self.cfg.variant {
            Variant::VarOpt => {
                if n <= k {
                    return Sketch::stored(&self.cand, 0.0, reduced_in);
                }
                let u = hash::varopt_offset(self.cfg.seed, hash::coordinate(rec, j));
                let o = varopt::reduce(&self.cand, k, u, &mut self.vo, &mut self.out);
                self.work.reductions += 1;
                if o.off_by_one(k) {
                    self.work.off_by_one += 1;
                }
                Sketch::stored(&self.out, o.tau, true)
            }
            Variant::Prio => {
                if n <= k {
                    return Sketch::stored(&self.cand, 0.0, reduced_in);
                }
                let x = hash::coordinate(rec, j);
                let tau = prio::reduce(&self.cand, k, self.cfg.seed, x, &mut self.pr, &mut self.out);
                self.work.reductions += 1;
                Sketch::stored(&self.out, tau, true)
            }
            Variant::Coord => {
                let cap = coord::cap_of(caps.iter().copied(), self.cfg.cap);
                let keys = self.keys.as_ref().expect("coord has keys");
                let tau = coord::reduce(&self.cand, k, cap, self.cfg.floor, keys, &mut self.co, &mut self.out);
                if tau.is_finite() && (n > k || cap.is_finite()) {
                    self.work.reductions += 1;
                }
                Sketch::stored(&self.out, tau, reduced_in || tau.is_finite())
            }
        }
    }

    /// `c . h`, never reducing.  Shares the handle when `c == 1`.
    pub fn scale(&mut self, h: &Handle, c: f64) -> Handle {
        if c == 1.0 {
            return Rc::clone(h);
        }
        self.cand.clear();
        self.cand.extend(h.entries.iter().map(|e| (e.b, c * e.w as f64)));
        let tau = match self.cfg.variant {
            Variant::Coord => h.tau / c,
            _ => h.tau,
        };
        Sketch::stored(&self.cand, tau, h.reduced)
    }

    fn record_mode(&mut self, ops: &[(Handle, f64)], rec: u64) -> Handle {
        self.group(ops);
        assert!(!self.groups.is_empty(), "record {} has no operand with a positive share", rec);
        if self.groups.len() == 1 {
            // sum s_i = 1: the parent's colour is the colour.
            return Rc::clone(&ops[self.groups[0].0].0);
        }
        self.merge_groups(ops);
        let reduced_in = self.groups.iter().any(|&(i, _)| ops[i].0.reduced);
        let mut caps = std::mem::take(&mut self.caps);
        caps.clear();
        if self.cfg.variant == Variant::Coord {
            caps.extend(self.groups.iter().map(|&(i, s)| (s, ops[i].0.tau)));
        }
        let out = self.finish(reduced_in, &caps, rec, 0);
        self.caps = caps;
        out
    }

    fn step_mode(&mut self, ops: &[(Handle, f64)], rec: u64) -> Handle {
        let mut live = ops.iter().filter(|op| op.1 != 0.0);
        let first = live.next().unwrap_or_else(|| panic!("record {} has no operand with a positive share", rec));
        let mut acc = self.scale(&first.0, first.1);
        for (j, (h, s)) in live.enumerate() {
            let j = j as u64 + 1;
            assert!(j < hash::IDEAL_STEP, "record {} has more fold steps than the step index holds", rec);
            self.cand.clear();
            if Rc::ptr_eq(h, &acc) {
                let c = s + 1.0;
                self.cand.extend(h.entries.iter().map(|e| (e.b, c * e.w as f64)));
            } else {
                merge_two(&h.entries, *s, &acc.entries, 1.0, &mut self.cand);
            }
            let caps = [(*s, h.tau), (1.0, acc.tau)];
            let reduced_in = h.reduced || acc.reduced;
            acc = self.finish(reduced_in, &caps, rec, j);
        }
        acc
    }
}

/// `a + b` over ascending candidate runs, into `out` (appended).  Both sides
/// are positive, so nothing sums to zero.
pub fn merge_runs(a: &[(u32, f64)], b: &[(u32, f64)], out: &mut Vec<(u32, f64)>) {
    out.reserve(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        if x.0 < y.0 {
            out.push(x);
            i += 1;
        } else if y.0 < x.0 {
            out.push(y);
            j += 1;
        } else {
            out.push((x.0, x.1 + y.1));
            i += 1;
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

/// `sa . a + sb . b` over ascending entries, into `out` (appended), zeros
/// dropped.
pub fn merge_two(a: &[Entry], sa: f64, b: &[Entry], sb: f64, out: &mut Vec<(u32, f64)>) {
    out.reserve(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        let (bk, v) = if x.b < y.b {
            i += 1;
            (x.b, sa * x.w as f64)
        } else if y.b < x.b {
            j += 1;
            (y.b, sb * y.w as f64)
        } else {
            i += 1;
            j += 1;
            (x.b, sa * x.w as f64 + sb * y.w as f64)
        };
        if v > 0.0 {
            out.push((bk, v));
        }
    }
    for x in &a[i..] {
        let v = sa * x.w as f64;
        if v > 0.0 {
            out.push((x.b, v));
        }
    }
    for y in &b[j..] {
        let v = sb * y.w as f64;
        if v > 0.0 {
            out.push((y.b, v));
        }
    }
}

impl Algebra for Sketcher {
    type C = Handle;

    fn singleton(&mut self, block: u32) -> Handle {
        Sketch::new(Box::new([Entry { b: block, w: 1.0 }]), self.exact_tau(), false)
    }

    fn record(&mut self, ops: &[(Handle, f64)], rec: u64) -> Handle {
        match self.cfg.reduce {
            Reduce::Record => self.record_mode(ops, rec),
            Reduce::Step => self.step_mode(ops, rec),
        }
    }

    fn emit(&self, c: &Handle, out: &mut Vec<(u32, f64)>) {
        out.clear();
        let finite_coord = self.cfg.variant == Variant::Coord && c.tau.is_finite();
        let lift = if finite_coord { 1.0 / c.tau } else { 0.0 };
        let mut total = 0.0;
        for e in c.entries.iter() {
            let w = (e.w as f64).max(lift);
            total += w;
            out.push((e.b, w));
        }
        if total > 0.0 {
            for x in out.iter_mut() {
                x.1 /= total;
            }
        }
    }

    fn raw_mass(&self, c: &Handle) -> f64 {
        let finite_coord = self.cfg.variant == Variant::Coord && c.tau.is_finite();
        let lift = if finite_coord { 1.0 / c.tau } else { 0.0 };
        c.entries.iter().map(|e| (e.w as f64).max(lift)).sum()
    }

    fn info(&self, c: &Handle) -> Info {
        Info { n: c.len(), tau: c.tau, reduced: c.reduced }
    }

    fn work(&self) -> Work {
        self.work
    }

    fn usage(&self) -> Usage {
        usage()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{splitmix64, unit};

    #[test]
    fn configs_parse_and_label_canonically() {
        let c = Config::parse("varopt:256").unwrap();
        assert_eq!(c.label(), "varopt-k256-s1-rec");
        let c = Config::parse("coord:64:seed=7:reduce=step:cap=harm:floor=3").unwrap();
        assert_eq!(c.label(), "coord-k64-s7-step-harm-f3");
        assert_eq!(Config::parse("coord:64").unwrap().label(), "coord-k64-s1-rec-min-f0");
        assert!(Config::parse("varopt:0").is_err());
        assert!(Config::parse("varopt:2000000").is_err());
        assert!(Config::parse("foo:3").is_err());
        assert!(Config::parse("kmv:64").unwrap_err().contains("kmv is a side store: pass --kmv"));
        assert!(Config::parse("varopt:3:cap=harm").is_err());
        assert!(Config::parse("varopt:3:bogus=1").is_err());
    }

    fn sketch_of(seed: u64, n: usize, reduced: bool) -> Handle {
        let mut c = Vec::new();
        let mut b = 0;
        for i in 0..n as u64 {
            b += 1 + (splitmix64(seed ^ i) % 5) as u32;
            c.push((b, unit(splitmix64(seed + i))));
        }
        Sketch::stored(&c, 0.0, reduced)
    }

    /// The heap merge and the two-pointer merge agree with a plain sum.
    #[test]
    fn the_k_way_merge_sums_like_a_map() {
        let mut alg = Sketcher::new(Config::parse("varopt:100000").unwrap(), None);
        for trial in 0..50u64 {
            let g = 2 + (trial % 7) as usize;
            let hs: Vec<Handle> = (0..g).map(|i| sketch_of(trial * 31 + i as u64, 5 + (i * 37) % 200, false)).collect();
            let mut ops: Vec<(Handle, f64)> = hs.iter().enumerate().map(|(i, h)| (h.clone(), (i + 1) as f64)).collect();
            let tot: f64 = ops.iter().map(|o| o.1).sum();
            for o in &mut ops {
                o.1 /= tot;
            }
            let mut want = std::collections::BTreeMap::new();
            for (h, s) in &ops {
                for e in h.entries.iter() {
                    *want.entry(e.b).or_insert(0.0) += s * e.w as f64;
                }
            }
            alg.group(&ops);
            alg.merge_groups(&ops);
            assert_eq!(alg.cand.len(), want.len());
            for (c, (b, v)) in alg.cand.iter().zip(want.iter()) {
                assert_eq!(c.0, *b);
                assert!((c.1 - v).abs() <= 1e-15);
            }
        }
    }

    /// After a consolidation of thousands of parents, the scratch runs kept
    /// for reuse fit in [`SPARE_BUDGET`] rather than holding the whole merge
    /// tree's worth for the rest of the run.
    #[test]
    fn a_huge_merge_does_not_pin_its_scratch() {
        let mut alg = Sketcher::new(Config::parse("varopt:256").unwrap(), None);
        let g = 10_000usize;
        // Interleaved blocks, so the union is all 2.56M of them.
        let hs: Vec<Handle> = (0..g)
            .map(|i| {
                let c: Vec<(u32, f64)> = (0..256u32).map(|j| (j * g as u32 + i as u32, 1.0 / 256.0)).collect();
                Sketch::stored(&c, 0.0, true)
            })
            .collect();
        let ops: Vec<(Handle, f64)> = hs.iter().map(|h| (h.clone(), 1.0 / g as f64)).collect();
        let r = alg.record(&ops, 0);
        assert!(r.len() <= 257);
        let kept: usize = alg.spare.iter().map(|v| v.capacity()).sum();
        eprintln!("spare after a {}-way merge: {} vectors, {} entries of capacity", g, alg.spare.len(), kept);
        assert!(kept <= SPARE_BUDGET, "{} entries kept", kept);
        // And the pool still serves the small merges it is there for.
        let r = alg.record(&ops[..5], 1);
        assert!(r.len() <= 257);
    }

    /// One group answers the parent's own allocation; zero shares are skipped,
    /// neither merged nor allowed to mark the child reduced.
    #[test]
    fn one_group_is_free_and_zero_shares_have_no_say() {
        let mut alg = Sketcher::new(Config::parse("varopt:8").unwrap(), None);
        let a = sketch_of(1, 5, false);
        let big = sketch_of(2, 50, true);
        let r = alg.record(&[(a.clone(), 0.25), (a.clone(), 0.75)], 0);
        assert!(Rc::ptr_eq(&r, &a));
        let r = alg.record(&[(big.clone(), 0.0), (a.clone(), 1.0)], 1);
        assert!(Rc::ptr_eq(&r, &a), "the zero-share operand is not a group");
        let b = sketch_of(3, 2, false);
        let r = alg.record(&[(big.clone(), 0.0), (a.clone(), 0.5), (b.clone(), 0.5)], 2);
        assert!(!r.reduced, "a reduced parent with share 0 does not make the child reduced");
        assert!(r.len() <= 7);
        assert_eq!(alg.work().reductions, 0, "no reduction of an exact 7-term colour");
        // And in step mode too.
        let mut step = Sketcher::new(Config::parse("varopt:8:reduce=step").unwrap(), None);
        let r = step.record(&[(big.clone(), 0.0), (a.clone(), 0.5), (b.clone(), 0.5)], 2);
        assert!(!r.reduced);
        assert_eq!(step.work().reductions, 0);
    }

    #[test]
    fn live_counts_follow_construction_and_drop() {
        let before = usage();
        let a = sketch_of(9, 10, false);
        let b = Rc::clone(&a);
        let mid = usage();
        assert_eq!(mid.live_entries, before.live_entries + 10);
        assert_eq!(mid.live_sketches, before.live_sketches + 1);
        drop(a);
        drop(b);
        let after = usage();
        assert_eq!(after.live_entries, before.live_entries);
        assert_eq!(after.live_sketches, before.live_sketches);
    }

    #[test]
    fn storage_drops_zero_and_subnormal_weights() {
        let s = Sketch::stored(&[(1, 0.5), (2, 0.0), (3, 1e-40), (4, 1e-300), (5, 0.25)], 0.0, false);
        let bs: Vec<u32> = s.entries.iter().map(|e| e.b).collect();
        assert_eq!(bs, vec![1, 5]);
    }
}
