//! # The support of a colour, by a coordinated bottom-k (KMV)
//!
//! The colour recurrence `C = sum_i s_i C_i` has a set-valued shadow, the
//! support recurrence `S_t = U_i S_{p_i}`: the blocks a transaction's coins
//! descend from.  Two questions are asked of it -- *how many* blocks (a
//! count-distinct) and *how much do two transactions share* (a Jaccard
//! similarity, and the containment of one support in another) -- and the
//! varopt sketches cannot answer either: their randomness is fresh at every
//! reduction, so two sketches are not coordinated, and a block dropped at an
//! ancestor is gone for every descendant.
//!
//! This store keeps, for every colour, `K_k(S)`: the `k` blocks of its
//! support with the smallest rank under one permutation of the blocks
//! ([`crate::hash::Perm`]), shared by every sketch of a run.  It is folded by
//! the same driver as every other store, with union in place of the sum.
//!
//! ## Why the fold is exact
//!
//! - **L1 (union).**  `K_k(A u B) = K_k(K_k(A) u K_k(B))`: the `k` smallest
//!   of a union are among the `k` smallest of each side.  By induction any
//!   tree of capped merges, in any order and grouping, is `K_k` of the union.
//! - **L2 (subsets).**  If `S_i` is a subset of `S`, `b` is in `K_k(S)` and
//!   `b` is in `S_i`, then `b` is in `K_k(S_i)` (fewer than `k` elements of
//!   `S_i` rank below it).  So for `x` in `K_k(A u B)`, `x` is in `A` iff it is
//!   in `K_k(A)`: the pair estimators below read membership off the sketches.
//! - **T3 (exact through the DAG).**  Every stored sketch *is* `K_k(S_t)`, and
//!   `full` holds iff `|S_t| > k`, whatever the fold order, the grouping or
//!   repeated operands.  There is no recursion error: an estimate at depth
//!   has exactly the law of a one-shot KMV of the support.
//!
//! ## Which support
//!
//! `support=sets` (the default) is the support `--sets` computes: every
//! input's blocks, zero-amount inputs included.  `/data/bitcoin/2022/
//! finalBCUTXO_2022.sets-counts.*.txt` holds its exact size for the first
//! 31.2M records, which is why it is the default.  `support=pos` skips the
//! operands of share 0 and is the support of the *weighted* colour, `{b :
//! c_t(b) > 0}`, in exact arithmetic: in f64 a coefficient can underflow to
//! 0 at depth while its block stays in `pos` ([`crate::exact`]).  On this
//! chain the two are almost the same set: over all 31,187,785 records of
//! the 2022 file the `k = 1024` seed-1 streams of `sets` and `pos` differ
//! in 104 records.  Every output still names which one in its label
//! (`kmv-k1024-s1-sets`, `-pos`).
//!
//! ## Counting: the universe is `[0, beta]`, not `[0, M)`
//!
//! With `R` the 0-based `k`-th smallest rank of a set of `n` drawn from a
//! universe of `N`, `(k-1) N / R` is exactly unbiased: `P(R = r) = C(r, k-1)
//! C(N-1-r, n-k) / C(N, n)`, and `(k-1)/r . C(r, k-1) = C(r-1, k-2)`, so the
//! expectation is `N . C(N-1, n-1) / C(N, n) = n` by Vandermonde.  Its
//! variance is about `n (n-k+1)/(k-2) . (N-n)/(N-1)`.
//!
//! The universe that matters is not the permutation's `M = 2^bits` but the
//! blocks the support *can* contain: every block of `S_t` is at most
//! `beta_t`, the newest block seen so far (the record's own block, on a chain
//! read in block order).  The ranks of `[0, beta]` are a uniformly random
//! subset of the ranks, so the restricted rank `R'' = #{b <= beta : rank(b)
//! < r_k}` is the same `R` over a universe of `N = beta + 1`, and
//! `(k-1)(beta+1)/R''` is exactly unbiased too.  Supports on this chain are
//! dense -- 70% of `[0, beta]` at record 3M, 84% at 31.2M -- so the
//! finite-population factor `(N-n)/(N-1)` is 0.16 where `M`'s is 0.78: the
//! RSE falls by half, the same as a `k` almost five times larger, for one
//! Fenwick query per full record ([`Universe`]).  `(k-1) M / R` is kept as a
//! second column so the two can be compared; it is the textbook KMV.
//!
//! ## Comparing two supports
//!
//! [`pair`] gives the Jaccard similarity, exact when both sketches hold whole
//! supports (the textbook formula over `K_k(A u B)` is *not* exact then: it
//! deviates by up to 0.0035 when the union exceeds `k`), else `hits / k` over
//! `K_k(A u B)`, which is hypergeometric with mean `J`.  Containment of `A` in
//! `B` is read from the sample of `A` at ranks up to `B`'s threshold, which
//! is consistent but not proved unbiased (the threshold depends on `B`), and
//! undefined when that sample is empty.
//!
//! ## Cost
//!
//! A rank is 4 bytes, plus about 40 bytes of `Rc` box per distinct sketch.  A
//! record of `G` operand groups merges in `O(G k)` by a balanced tree of
//! two-pointer merges, each capped at `k`, or with a heap that stops at `k`
//! when `G` is in the thousands ([`union_cap`], [`HEAP_FROM`]).  The
//! commonest record, one parent (perhaps spent several times), is a refcount
//! bump.  Measured over the first 3M records of the 2022 chain at `k = 1024`:
//! 4.6 s of fold for the store, parse-bound at about 400k records a second;
//! 77M ranks (310 MB) at the peak.

use crate::algebra::{Algebra, Info, Usage, Work};
use crate::hash::Perm;
use std::cell::Cell;
use std::cmp::Reverse;
use std::collections::binary_heap::PeekMut;
use std::collections::{BinaryHeap, HashMap};
use std::io::{self, Read, Write};
use std::rc::Rc;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Support {
    Sets,
    Pos,
}

impl Support {
    pub fn name(self) -> &'static str {
        match self {
            Support::Sets => "sets",
            Support::Pos => "pos",
        }
    }
    pub fn code(self) -> u8 {
        match self {
            Support::Sets => 0,
            Support::Pos => 1,
        }
    }
    pub fn from_code(c: u8) -> io::Result<Support> {
        match c {
            0 => Ok(Support::Sets),
            1 => Ok(Support::Pos),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, format!("unknown support code {}", c))),
        }
    }
}

pub const DEFAULT_BITS: u32 = 20;

/// `--kmv <k>[:seed=<u64>][:support=sets|pos][:bits=<n>]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct KmvSpec {
    pub k: usize,
    pub seed: u64,
    pub support: Support,
    pub bits: u32,
}

impl KmvSpec {
    pub fn parse(text: &str) -> Result<KmvSpec, String> {
        let mut parts = text.split(':');
        let k: usize = parts
            .next()
            .and_then(|k| k.parse().ok())
            .ok_or_else(|| format!("--kmv {:?}: expected <k>[:seed=..][:support=sets|pos][:bits=..]", text))?;
        // k >= 2: the count of a full sketch is (k-1) N / R, and k = 1 has none.
        if !(2..=1 << 20).contains(&k) {
            return Err(format!("--kmv {:?}: k must be in 2..=1048576", text));
        }
        let mut s = KmvSpec { k, seed: 1, support: Support::Sets, bits: DEFAULT_BITS };
        for opt in parts {
            let (key, value) =
                opt.split_once('=').ok_or_else(|| format!("--kmv {:?}: option {:?} is not key=value", text, opt))?;
            let bad = || format!("--kmv {:?}: bad value in {:?}", text, opt);
            match key {
                "seed" => s.seed = value.parse().map_err(|_| bad())?,
                "support" => {
                    s.support = match value {
                        "sets" => Support::Sets,
                        "pos" => Support::Pos,
                        _ => return Err(bad()),
                    }
                }
                "bits" => {
                    s.bits = value.parse().map_err(|_| bad())?;
                    if !(Perm::MIN_BITS..=Perm::MAX_BITS).contains(&s.bits) {
                        return Err(format!("--kmv {:?}: bits must be in {}..={}", text, Perm::MIN_BITS, Perm::MAX_BITS));
                    }
                }
                _ => return Err(format!("--kmv {:?}: unknown option {:?}", text, key)),
            }
        }
        if s.k as u64 >= 1u64 << s.bits {
            return Err(format!("--kmv {:?}: k must be below 2^bits", text));
        }
        Ok(s)
    }

    /// A spec read from a file header, checked as [`KmvSpec::parse`] checks
    /// one from the command line: damaged input is an error, not a panic in
    /// [`Perm::new`] or a shift past 64 bits.
    pub fn check_read(&self, what: &str) -> io::Result<()> {
        if !(Perm::MIN_BITS..=Perm::MAX_BITS).contains(&self.bits) {
            return Err(corrupt(format!("{} header: bits {} is not in {}..={}", what, self.bits, Perm::MIN_BITS, Perm::MAX_BITS)));
        }
        if !(2..=1 << 20).contains(&self.k) || self.k as u64 >= 1u64 << self.bits {
            return Err(corrupt(format!("{} header: k {} is not in 2..=1048576 and below 2^{}", what, self.k, self.bits)));
        }
        Ok(())
    }

    /// `kmv-k1024-s1-sets`, with `-b<bits>` only when not the default.
    /// [`crate::metrics::family`] turns it into `kmv-k1024-s*-sets`.
    pub fn label(&self) -> String {
        let mut s = format!("kmv-k{}-s{}-{}", self.k, self.seed, self.support.name());
        if self.bits != DEFAULT_BITS {
            s.push_str(&format!("-b{}", self.bits));
        }
        s
    }
}

/// One bottom-k sketch of a support.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Kmv {
    /// Strictly ascending ranks, at most `k` of them.
    pub r: Box<[u32]>,
    /// `|S| > k`; then `r.len() == k` exactly and `r[k-1]` is the `k`-th
    /// smallest rank of `S`.  When `false`, `r` is the whole support.
    pub full: bool,
}

pub type KHandle = Rc<Kmv>;

thread_local! {
    static USAGE: Cell<Usage> = const { Cell::new(Usage {
        live_entries: 0, peak_entries: 0, live_sketches: 0, peak_sketches: 0,
    }) };
}

/// This thread's KMV usage (live and peak ranks and sketches).
pub fn usage() -> Usage {
    USAGE.with(|u| u.get())
}

impl Kmv {
    pub fn new(r: Box<[u32]>, full: bool) -> KHandle {
        let n = r.len() as u64;
        USAGE.with(|u| {
            let mut x = u.get();
            x.live_entries += n;
            x.live_sketches += 1;
            x.peak_entries = x.peak_entries.max(x.live_entries);
            x.peak_sketches = x.peak_sketches.max(x.live_sketches);
            u.set(x);
        });
        Rc::new(Kmv { r, full })
    }

    pub fn len(&self) -> usize {
        self.r.len()
    }

    /// The 0-based `k`-th smallest rank, when full.
    pub fn rk(&self) -> Option<u32> {
        self.full.then(|| *self.r.last().expect("a full sketch has k ranks"))
    }
}

/// A sketch as the estimators read it: ranks and the `full` flag, borrowed.
/// The stored [`Kmv`] counts itself in the store's usage on creation and
/// drop; sketches read back from a file (`pairs`) or built in a test are
/// plain [`Owned`] values, and both lend a `View`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct View<'a> {
    pub r: &'a [u32],
    pub full: bool,
}

impl<'a> View<'a> {
    pub fn len(&self) -> usize {
        self.r.len()
    }
    /// The 0-based `k`-th smallest rank, when full.
    pub fn rk(&self) -> Option<u32> {
        self.full.then(|| *self.r.last().expect("a full sketch has k ranks"))
    }
}

/// A sketch held as a plain value, outside any store.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Owned {
    pub r: Vec<u32>,
    pub full: bool,
}

impl Owned {
    pub fn view(&self) -> View<'_> {
        View { r: &self.r, full: self.full }
    }
}

impl Kmv {
    pub fn view(&self) -> View<'_> {
        View { r: &self.r, full: self.full }
    }
}

impl Drop for Kmv {
    fn drop(&mut self) {
        let n = self.r.len() as u64;
        USAGE.with(|u| {
            let mut x = u.get();
            x.live_entries -= n;
            x.live_sketches -= 1;
            u.set(x);
        });
    }
}

// ------------------------------------------------------------------ merging

/// `K_k(a u b)` of two ascending runs into `out` (cleared first), equal ranks
/// once; answers whether anything was left over, i.e. whether the union has
/// more than `k` elements.  It stops as soon as `out` holds `k`.
pub fn merge_cap(a: &[u32], b: &[u32], k: usize, out: &mut Vec<u32>) -> bool {
    out.clear();
    out.reserve(k.min(a.len() + b.len()));
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if out.len() == k {
            return true;
        }
        let (x, y) = (a[i], b[j]);
        if x < y {
            out.push(x);
            i += 1;
        } else if y < x {
            out.push(y);
            j += 1;
        } else {
            out.push(x);
            i += 1;
            j += 1;
        }
    }
    let rest = if i < a.len() { &a[i..] } else { &b[j..] };
    let room = k - out.len();
    if rest.len() > room {
        out.extend_from_slice(&rest[..room]);
        true
    } else {
        out.extend_from_slice(rest);
        false
    }
}

/// Groups from which [`union_cap`] merges with a heap rather than a tree.
///
/// Both stop at `k`.  The tree reads about `2 G k` ranks in predictable
/// two-pointer passes; the heap pops `k` distinct ranks *plus every duplicate
/// below the cut*, at `log G` each, through unpredictable sift branches.
/// Supports here are nested, so the parents' sketches nearly coincide and the
/// duplicates are most of the work: measured over the first 15M records at
/// `k = 1024` (busy seconds of one store), a heap from 3 groups took 106 s,
/// from 8 groups 65, from 32 49, from 256 33, and the tree alone 26-28 --
/// the same as a heap from 2048.  The heap is kept for the consolidations of
/// thousands of parents with *disjoint* sketches, where `O(k log G)` beats
/// `O(G k)`; on the 2022 chain it is all but never reached.
pub const HEAP_FROM: usize = 2048;

/// Scratch for [`union_cap`], kept by its owner so that a record allocates
/// nothing but its output.
#[derive(Default)]
pub struct MergeScratch {
    heap: BinaryHeap<Reverse<u64>>,
    pos: Vec<u32>,
    level: Vec<Vec<u32>>,
    next: Vec<Vec<u32>>,
    pool: Vec<Vec<u32>>,
}

/// `K_k` of the union of `g` ascending runs, `run(i)` the `i`-th, into `out`;
/// answers whether the union has more than `k` elements.
pub fn union_cap<'a>(g: usize, run: impl Fn(usize) -> &'a [u32], k: usize, out: &mut Vec<u32>, s: &mut MergeScratch) -> bool {
    match g {
        0 => {
            out.clear();
            false
        }
        1 => {
            let a = run(0);
            out.clear();
            out.extend_from_slice(&a[..a.len().min(k)]);
            a.len() > k
        }
        2 => merge_cap(run(0), run(1), k, out),
        _ if g < HEAP_FROM => tree_cap(g, run, k, out, s),
        _ => heap_cap(g, run, k, out, s),
    }
}

/// The balanced tree: runs pairwise, then the results pairwise, every merge
/// capped at `k`, so each level costs at most `G/2^l . 2k`.
pub fn tree_cap<'a>(g: usize, run: impl Fn(usize) -> &'a [u32], k: usize, out: &mut Vec<u32>, s: &mut MergeScratch) -> bool {
    let mut cut = false;
    let mut level = std::mem::take(&mut s.level);
    let mut next = std::mem::take(&mut s.next);
    level.clear();
    let mut i = 0;
    while i < g {
        let mut v = s.pool.pop().unwrap_or_default();
        if i + 1 < g {
            cut |= merge_cap(run(i), run(i + 1), k, &mut v);
        } else {
            let a = run(i);
            v.clear();
            v.extend_from_slice(&a[..a.len().min(k)]);
            cut |= a.len() > k;
        }
        level.push(v);
        i += 2;
    }
    while level.len() > 1 {
        next.clear();
        let mut it = level.drain(..);
        while let Some(a) = it.next() {
            match it.next() {
                Some(b) => {
                    let mut v = s.pool.pop().unwrap_or_default();
                    cut |= merge_cap(&a, &b, k, &mut v);
                    s.pool.push(a);
                    s.pool.push(b);
                    next.push(v);
                }
                None => next.push(a),
            }
        }
        drop(it);
        std::mem::swap(&mut level, &mut next);
    }
    let last = level.pop().expect("at least one run");
    out.clear();
    out.extend_from_slice(&last);
    s.pool.push(last);
    s.level = level;
    s.next = next;
    cut
}

/// The heap: the smallest head of all runs, `k` times, skipping repeats.
/// Once `k` are out, the next distinct head (if any) is what was left over.
pub fn heap_cap<'a>(g: usize, run: impl Fn(usize) -> &'a [u32], k: usize, out: &mut Vec<u32>, s: &mut MergeScratch) -> bool {
    out.clear();
    s.heap.clear();
    s.pos.clear();
    s.pos.resize(g, 0);
    for i in 0..g {
        if let Some(&x) = run(i).first() {
            s.heap.push(Reverse(((x as u64) << 32) | i as u64));
        }
    }
    while let Some(mut top) = s.heap.peek_mut() {
        let v = top.0;
        let (x, i) = ((v >> 32) as u32, (v & 0xffff_ffff) as usize);
        if out.last() != Some(&x) {
            if out.len() == k {
                return true;
            }
            out.push(x);
        }
        let p = s.pos[i] as usize + 1;
        s.pos[i] = p as u32;
        match run(i).get(p) {
            Some(&y) => top.0 = ((y as u64) << 32) | i as u64,
            None => {
                PeekMut::pop(top);
            }
        }
    }
    false
}

// ------------------------------------------------------------------ the store

/// The KMV store, one per `--kmv` spec, on a thread of its own.
pub struct KmvStore {
    pub spec: KmvSpec,
    pub perm: Arc<Perm>,
    work: Work,
    groups: Vec<usize>,
    group_of: HashMap<usize, usize>,
    out: Vec<u32>,
    scratch: MergeScratch,
}

impl KmvStore {
    pub fn new(spec: KmvSpec, perm: Arc<Perm>) -> KmvStore {
        assert_eq!((perm.seed(), perm.bits()), (spec.seed, spec.bits), "the permutation is the spec's");
        KmvStore {
            spec,
            perm,
            work: Work::default(),
            groups: Vec::new(),
            group_of: HashMap::new(),
            out: Vec::new(),
            scratch: MergeScratch::default(),
        }
    }

    /// The operands that have a say, by pointer, in order of first occurrence,
    /// into `self.groups` as indices of `ops`.  Under `sets` every operand
    /// does; under `pos` those of share 0 do not.  Linear search up to 16
    /// operands and a map above, as `Sketcher::group`.
    fn group(&mut self, ops: &[(KHandle, f64)]) {
        self.groups.clear();
        let pos = self.spec.support == Support::Pos;
        if ops.len() <= 16 {
            for (i, (h, s)) in ops.iter().enumerate() {
                if pos && *s == 0.0 {
                    continue;
                }
                if !self.groups.iter().any(|&g| Rc::ptr_eq(&ops[g].0, h)) {
                    self.groups.push(i);
                }
            }
        } else {
            self.group_of.clear();
            for (i, (h, s)) in ops.iter().enumerate() {
                if pos && *s == 0.0 {
                    continue;
                }
                let p = Rc::as_ptr(h) as usize;
                if !self.group_of.contains_key(&p) {
                    self.group_of.insert(p, self.groups.len());
                    self.groups.push(i);
                }
            }
        }
    }
}

impl Algebra for KmvStore {
    type C = KHandle;

    fn singleton(&mut self, block: u32) -> KHandle {
        Kmv::new(Box::new([self.perm.rank(block)]), false)
    }

    fn record(&mut self, ops: &[(KHandle, f64)], rec: u64) -> KHandle {
        self.group(ops);
        assert!(!self.groups.is_empty(), "record {} has no operand with a say in its support", rec);
        if self.groups.len() == 1 {
            // The union of one set with itself: the parent's own sketch.
            return Rc::clone(&ops[self.groups[0]].0);
        }
        let groups = &self.groups;
        let any_full = groups.iter().any(|&i| ops[i].0.full);
        let k = self.spec.k;
        let cut = union_cap(groups.len(), |g| &ops[groups[g]].0.r, k, &mut self.out, &mut self.scratch);
        if cut {
            self.work.reductions += 1;
        }
        let full = any_full || cut;
        debug_assert!(self.out.len() <= k && (!full || self.out.len() == k));
        Kmv::new(self.out.as_slice().into(), full)
    }

    /// The kept blocks, ascending by block, each at `1/len`.  This is the
    /// kept *sample*, not an estimate of the colour; no mode emits it as a
    /// distribution, and it is here only because the trait asks for it.
    fn emit(&self, c: &KHandle, out: &mut Vec<(u32, f64)>) {
        out.clear();
        let w = 1.0 / c.len().max(1) as f64;
        out.extend(c.r.iter().map(|&r| (self.perm.block(r), w)));
        out.sort_unstable_by_key(|x| x.0);
    }

    fn raw_mass(&self, _c: &KHandle) -> f64 {
        1.0
    }

    fn info(&self, c: &KHandle) -> Info {
        Info {
            n: c.len(),
            tau: c.rk().map_or(0.0, |r| r as f64 / self.perm.universe() as f64),
            reduced: c.full,
        }
    }

    fn work(&self) -> Work {
        self.work
    }

    fn usage(&self) -> Usage {
        usage()
    }
}

// ------------------------------------------------------------------ the universe

/// The blocks a support can lie in, `[0, n)`, as a Fenwick tree over rank
/// space: [`Universe::below`] answers `#{b < n : rank(b) < r}`, the restricted
/// rank `R''` of the count estimator.  Blocks are added in increasing order
/// as the chain's newest block advances ([`Universe::grow`]), so a whole run
/// costs one insertion per block and one query per full record.
pub struct Universe {
    perm: Arc<Perm>,
    tree: Box<[u32]>,
    n: u64,
}

impl Universe {
    pub fn new(perm: Arc<Perm>) -> Universe {
        let m = perm.universe() as usize;
        Universe { perm, tree: vec![0u32; m + 1].into_boxed_slice(), n: 0 }
    }

    /// `N`, the number of blocks inserted: `[0, N)`.
    pub fn size(&self) -> u64 {
        self.n
    }

    /// Insert every block below `n` not yet in.  A block past the
    /// permutation's universe is the same hard error as in [`Perm::rank`].
    pub fn grow(&mut self, n: u64) {
        while self.n < n {
            let r = self.perm.rank(u32::try_from(self.n).expect("a block id fits in a u32")) as usize;
            let mut i = r + 1;
            while i < self.tree.len() {
                self.tree[i] += 1;
                i += i & i.wrapping_neg();
            }
            self.n += 1;
        }
    }

    /// How many inserted blocks rank strictly below `r`.
    #[inline]
    pub fn below(&self, r: u32) -> u64 {
        let mut i = r as usize;
        let mut s = 0u64;
        while i > 0 {
            s += self.tree[i] as u64;
            i &= i - 1;
        }
        s
    }

    /// The restricted count of `s` over this universe ([`count`]).
    pub fn count(&self, s: View, k: usize) -> f64 {
        match s.rk() {
            None => s.len() as f64,
            Some(r) => count_full(k, self.n, self.below(r)),
        }
    }
}

/// `#{b < n : rank(b) < r}` for many `(n, r)` at once, offline: the queries
/// sorted by `n`, one sweep of a [`Universe`].  For `pairs`, whose universes
/// are the selected records' and their pairs', in no particular order.
pub fn below_many(perm: &Arc<Perm>, queries: &[(u64, u32)]) -> Vec<u64> {
    let mut order: Vec<usize> = (0..queries.len()).collect();
    order.sort_by_key(|&i| queries[i].0);
    let mut u = Universe::new(perm.clone());
    let mut out = vec![0u64; queries.len()];
    for i in order {
        u.grow(queries[i].0);
        out[i] = u.below(queries[i].1);
    }
    out
}

// ------------------------------------------------------------------ estimators

/// `(k-1) N / R''`: the count of a full sketch, `below = R''` the number of
/// the universe's `N` blocks that rank below its `k`-th smallest rank.
/// Exactly unbiased under a uniform permutation (module docs), and never
/// above `N`, since `R'' >= k - 1`.
#[inline]
pub fn count_full(k: usize, n_univ: u64, below: u64) -> f64 {
    (k as f64 - 1.0) * n_univ as f64 / below.max(1) as f64
}

/// The count of a sketch over the universe `[0, n_univ)`: exact (`len`) when
/// the sketch holds the whole support, else [`count_full`] with the
/// restricted rank `below`.
pub fn count(s: View, k: usize, n_univ: u64, below: u64) -> f64 {
    if !s.full {
        s.len() as f64
    } else {
        count_full(k, n_univ, below)
    }
}

/// The textbook count over the whole permutation, `(k-1) M / R`, with `R`
/// the 0-based `k`-th smallest rank.  Unbiased too, with the finite-universe
/// factor of `M` rather than `beta + 1`; kept as a second column.
pub fn count_m(s: View, k: usize, m: u64) -> f64 {
    match s.rk() {
        None => s.len() as f64,
        Some(r) => (k as f64 - 1.0) * m as f64 / r.max(1) as f64,
    }
}

/// The standard error of a full count of `n` over a universe of `u`:
/// `sqrt(n (n-k+1)/(k-2) . (u-n)/(u-1))`.  The formula diverges at `k = 2`,
/// and this answers +inf there; the estimator itself is bounded by `u` (by
/// `N` for the restricted count), so its variance is finite, only not given
/// by this formula.  A seed gate treats a stratum with an infinite theory as
/// untestable for its variance ([`crate::count::SeedStrata`]).  The caller
/// passes 0 for a whole sketch.
pub fn count_se(n: f64, k: usize, u: u64) -> f64 {
    if k <= 2 {
        return f64::INFINITY;
    }
    let u = u as f64;
    let fpc = if u > 1.0 { ((u - n) / (u - 1.0)).max(0.0) } else { 0.0 };
    (n * (n - k as f64 + 1.0) / (k as f64 - 2.0) * fpc).max(0.0).sqrt()
}

/// What [`pair`] reads off two sketches, before a union count is chosen.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PairRaw {
    /// Both sketches hold whole supports: every number is exact.
    pub exact: bool,
    /// Exact: `|A n B|` and `|A u B|`.  Otherwise: hits in `K_k(A u B)`, and k.
    pub inter: u64,
    pub union: u64,
    /// The 0-based `k`-th smallest rank of `A u B`, when the union is full.
    pub union_rk: Option<u32>,
    /// `|T|` and `|T n B|` for `T` the sample of `A` at ranks up to `B`'s
    /// threshold; and the same for `B` in `A`.
    pub t_ab: u64,
    pub hit_ab: u64,
    pub t_ba: u64,
    pub hit_ba: u64,
}

/// Everything the pair estimators need, in one scan of the two sketches.
pub fn pair_raw(a: View, b: View, k: usize) -> PairRaw {
    let exact = !a.full && !b.full;
    let (ta, tb) = (a.rk().unwrap_or(u32::MAX), b.rk().unwrap_or(u32::MAX));
    let mut p = PairRaw { exact, ..PairRaw::default() };
    let (mut i, mut j) = (0, 0);
    let mut taken = 0usize;
    // One merge walk: the union in rank order, membership read off each side
    // (L2), the first k distinct for the Jaccard, and every element for the
    // containments.
    while i < a.r.len() || j < b.r.len() {
        let x = a.r.get(i).copied().unwrap_or(u32::MAX);
        let y = b.r.get(j).copied().unwrap_or(u32::MAX);
        let (v, in_a, in_b) = if i < a.r.len() && (j >= b.r.len() || x < y) {
            i += 1;
            (x, true, false)
        } else if j < b.r.len() && (i >= a.r.len() || y < x) {
            j += 1;
            (y, false, true)
        } else {
            i += 1;
            j += 1;
            (x, true, true)
        };
        if exact || taken < k {
            taken += 1;
            p.union += 1;
            if in_a && in_b {
                p.inter += 1;
            }
            if !exact && taken == k {
                p.union_rk = Some(v);
            }
        }
        if in_a && v <= tb {
            p.t_ab += 1;
            p.hit_ab += in_b as u64;
        }
        if in_b && v <= ta {
            p.t_ba += 1;
            p.hit_ba += in_a as u64;
        }
    }
    if !exact {
        // One side is full, so the union has more than k elements and its
        // first k are all here.
        debug_assert_eq!(p.union, k as u64);
    }
    p
}

/// The pair estimates, given the union count to use.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PairEst {
    pub exact: bool,
    pub j: f64,
    pub j_se: f64,
    pub n_union: f64,
    pub inter: f64,
    pub cont_ab: f64,
    pub t_ab: u64,
    pub cont_ba: f64,
    pub t_ba: u64,
}

/// Jaccard, intersection and containments from `raw`, with `n_union` the
/// union count (exact when `raw.exact`; else the caller's estimate, from the
/// restricted universe of [`union_count`] or from `M`).
///
/// - `J`: exact when both are whole; else `hits / k`, hypergeometric with
///   mean `J` and variance `J (1-J)/k . (n_u - k)/(n_u - 1)`.
/// - `inter = J . n_u`: consistent, not unbiased.
/// - containment of `A` in `B`: `|T n B| / |T|`, NaN when `T` is empty.
pub fn pair_est(raw: &PairRaw, k: usize, n_union: f64) -> PairEst {
    let (j, j_se, nu) = if raw.exact {
        let j = raw.inter as f64 / raw.union.max(1) as f64;
        (j, 0.0, raw.union as f64)
    } else {
        let j = raw.inter as f64 / k as f64;
        let fpc = if n_union > 1.0 { ((n_union - k as f64) / (n_union - 1.0)).clamp(0.0, 1.0) } else { 0.0 };
        (j, (j * (1.0 - j) / k as f64 * fpc).sqrt(), n_union)
    };
    let ratio = |hit: u64, t: u64| if t == 0 { f64::NAN } else { hit as f64 / t as f64 };
    PairEst {
        exact: raw.exact,
        j,
        j_se,
        n_union: nu,
        inter: j * nu,
        cont_ab: ratio(raw.hit_ab, raw.t_ab),
        t_ab: raw.t_ab,
        cont_ba: ratio(raw.hit_ba, raw.t_ba),
        t_ba: raw.t_ba,
    }
}

/// The union count of a pair over the universe `[0, n_univ)` -- `N =
/// max(beta_i, beta_j) + 1` -- given `below = #{b < N : rank(b) < union_rk}`.
pub fn union_count(raw: &PairRaw, k: usize, n_univ: u64, below: u64) -> f64 {
    match raw.union_rk {
        None => raw.union as f64,
        Some(_) => count_full(k, n_univ, below),
    }
}

/// The union count over the whole permutation, `(k-1) M / R_k(K)`.
pub fn union_count_m(raw: &PairRaw, k: usize, m: u64) -> f64 {
    match raw.union_rk {
        None => raw.union as f64,
        Some(r) => (k as f64 - 1.0) * m as f64 / r.max(1) as f64,
    }
}

/// [`pair_raw`] and [`pair_est`] with the union count over `M`: the
/// estimators as pure functions of two sketches, for tests and quick reads.
#[allow(dead_code)] // the tests' entry point, and a reader's
pub fn pair(a: View, b: View, k: usize, m: u64) -> PairEst {
    let raw = pair_raw(a, b, k);
    pair_est(&raw, k, union_count_m(&raw, k, m))
}

// ------------------------------------------------------------------ files

/// The count stream `count` writes and `decode` reads: fixed width, record
/// `i` at byte `48 + 20 i`, little-endian.
///
/// ```text
/// header (48 B): "KMVCNT01" | u32 version=1 | u32 k | u64 seed | u8 support (0 sets, 1 pos)
///                | u8 bits | u16 0 | u32 0 | u64 M | u64 0
/// record (20 B): u64 tx | u32 block | u32 m | u32 rk
///                m  = len, with bit 31 set when full
///                rk = the 0-based k-th smallest rank when full, else 0
/// trailer:       "KMVEND01" | u64 records | u64 sum of (tx ^ m ^ (rk << 32)), wrapping
/// ```
///
/// Both counts are lossless from it: `(m, rk)` and the header's seed and
/// bits rebuild the permutation, and the running maximum of `block` is the
/// universe ([`CntDecoder`] does both).  numpy reads the records as
/// `[('tx','<u8'),('block','<u4'),('m','<u4'),('rk','<u4')]`.
pub const CNT_MAGIC: &[u8; 8] = b"KMVCNT01";
pub const CNT_END: &[u8; 8] = b"KMVEND01";
pub const CNT_VERSION: u32 = 1;
pub const CNT_HEADER: usize = 48;
pub const CNT_RECORD: usize = 20;
pub const FULL_BIT: u32 = 1 << 31;

pub fn cnt_header(spec: &KmvSpec) -> [u8; CNT_HEADER] {
    let mut h = [0u8; CNT_HEADER];
    h[..8].copy_from_slice(CNT_MAGIC);
    h[8..12].copy_from_slice(&CNT_VERSION.to_le_bytes());
    h[12..16].copy_from_slice(&(spec.k as u32).to_le_bytes());
    h[16..24].copy_from_slice(&spec.seed.to_le_bytes());
    h[24] = spec.support.code();
    h[25] = spec.bits as u8;
    h[32..40].copy_from_slice(&(1u64 << spec.bits).to_le_bytes());
    h
}

/// The comment line `decode` puts first in a count stream's text, and
/// `select` copies into the selections it writes: the spec and its support,
/// `# kmv-k1024-s1-sets k=1024 seed=1 support=sets bits=20 M=1048576`.
pub fn cnt_spec_line(s: &KmvSpec, m: u64) -> String {
    format!("# {} k={} seed={} support={} bits={} M={}", s.label(), s.k, s.seed, s.support.name(), s.bits, m)
}

/// One record of the count stream.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CntRec {
    pub tx: u64,
    pub block: u32,
    /// `len | FULL_BIT` when full.
    pub m: u32,
    pub rk: u32,
}

impl CntRec {
    pub fn of(s: View, tx: u64, block: u32) -> CntRec {
        CntRec { tx, block, m: s.len() as u32 | if s.full { FULL_BIT } else { 0 }, rk: s.rk().unwrap_or(0) }
    }
    pub fn full(&self) -> bool {
        self.m & FULL_BIT != 0
    }
    pub fn len(&self) -> u32 {
        self.m & !FULL_BIT
    }
    pub fn bytes(&self) -> [u8; CNT_RECORD] {
        let mut b = [0u8; CNT_RECORD];
        b[..8].copy_from_slice(&self.tx.to_le_bytes());
        b[8..12].copy_from_slice(&self.block.to_le_bytes());
        b[12..16].copy_from_slice(&self.m.to_le_bytes());
        b[16..20].copy_from_slice(&self.rk.to_le_bytes());
        b
    }
    fn check(&self) -> u64 {
        self.tx ^ self.m as u64 ^ ((self.rk as u64) << 32)
    }
}

/// The count stream's writer side: the records and their checksum.
#[derive(Default)]
pub struct CntEncoder {
    pub records: u64,
    pub checksum: u64,
}

impl CntEncoder {
    pub fn record<W: Write + ?Sized>(&mut self, out: &mut W, r: &CntRec) -> io::Result<()> {
        self.records += 1;
        self.checksum = self.checksum.wrapping_add(r.check());
        out.write_all(&r.bytes())
    }
    pub fn trailer<W: Write + ?Sized>(&self, out: &mut W) -> io::Result<()> {
        out.write_all(CNT_END)?;
        out.write_all(&self.records.to_le_bytes())?;
        out.write_all(&self.checksum.to_le_bytes())
    }
}

fn corrupt(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

/// Reads a count stream back, verifying its trailer, and recomputes both
/// counts: [`CntDecoder::next`] keeps the universe as the running maximum of
/// the blocks.  `magic` is the 8 bytes already read by whoever dispatched on
/// them.
pub struct CntDecoder<R: Read> {
    inner: R,
    pub spec: KmvSpec,
    pub m: u64,
    pub records: u64,
    checksum: u64,
    universe: Option<Universe>,
    frontier: Option<u32>,
}

/// One decoded record with both counts and their SEs.  `se` and `se_M` are
/// *absolute*, in blocks, like the counts they go with (0 for a whole
/// sketch); a relative error is `se / n_hat`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CntRow {
    pub rec: u64,
    pub r: CntRec,
    pub n_univ: u64,
    /// The restricted rank `R''` of a full sketch, 0 for a whole one: with
    /// `n_univ` and the record it is all [`stream_counts`] needs, so a
    /// reader can keep three `u32`s of a record and recompute the rest.
    pub below: u64,
    pub n_hat: f64,
    pub se: f64,
    pub n_hat_m: f64,
    pub se_m: f64,
}

/// A count-stream record's two counts and their SEs, `[n_hat, se, n_hat_M,
/// se_M]`, over the universe `[0, n_univ)` with restricted rank `below`
/// (only read when `full`) and over `M`.  The one place they are computed
/// from a stored record: [`CntDecoder::next`] (and so `decode`) and
/// `summary` both call it, so the two cannot drift apart.
pub fn stream_counts(k: usize, m: u64, n_univ: u64, full: bool, len: u32, rk: u32, below: u64) -> [f64; 4] {
    if !full {
        return [len as f64, 0.0, len as f64, 0.0];
    }
    let n_hat = count_full(k, n_univ, below);
    let n_hat_m = (k as f64 - 1.0) * m as f64 / rk.max(1) as f64;
    [n_hat, count_se(n_hat, k, n_univ), n_hat_m, count_se(n_hat_m, k, m)]
}

impl<R: Read> CntDecoder<R> {
    /// `perm`: the permutation of the header's seed and bits, or `None` to
    /// build it.
    pub fn new(mut inner: R, magic: [u8; 8], perm: Option<Arc<Perm>>) -> io::Result<CntDecoder<R>> {
        if &magic != CNT_MAGIC {
            return Err(corrupt("not a count stream (no KMVCNT01 header)".into()));
        }
        let mut h = [0u8; CNT_HEADER - 8];
        inner.read_exact(&mut h)?;
        let version = u32::from_le_bytes(h[0..4].try_into().unwrap());
        if version != CNT_VERSION {
            return Err(corrupt(format!("count stream version {}, this reads {}", version, CNT_VERSION)));
        }
        let spec = KmvSpec {
            k: u32::from_le_bytes(h[4..8].try_into().unwrap()) as usize,
            seed: u64::from_le_bytes(h[8..16].try_into().unwrap()),
            support: Support::from_code(h[16])?,
            bits: h[17] as u32,
        };
        spec.check_read("count stream")?;
        let m = u64::from_le_bytes(h[24..32].try_into().unwrap());
        if m != 1u64 << spec.bits {
            return Err(corrupt(format!("count stream header: M {} is not 2^{}", m, spec.bits)));
        }
        let perm = perm.unwrap_or_else(|| Arc::new(Perm::new(spec.seed, spec.bits)));
        Ok(CntDecoder { inner, spec, m, records: 0, checksum: 0, universe: Some(Universe::new(perm)), frontier: None })
    }

    /// The next record, or `None` at the verified trailer.
    pub fn next(&mut self) -> io::Result<Option<CntRow>> {
        let r = match self.next_rec()? {
            Some(r) => r,
            None => return Ok(None),
        };
        let rec = self.records - 1;
        let f = self.frontier.expect("set by next_rec");
        let u = self.universe.as_mut().expect("present");
        u.grow(f as u64 + 1);
        let below = if r.full() { u.below(r.rk) } else { 0 };
        let [n_hat, se, n_hat_m, se_m] = stream_counts(self.spec.k, self.m, u.size(), r.full(), r.len(), r.rk, below);
        Ok(Some(CntRow { rec, r, n_univ: u.size(), below, n_hat, se, n_hat_m, se_m }))
    }

    /// The next record as it is stored, or `None` at the verified trailer:
    /// what a reader that wants only `tx` and `block` calls (`surface`), at
    /// the cost of the read and the checksum.  The counts are not computed,
    /// but the frontier is kept, so [`CntDecoder::next`] can follow it.
    pub fn next_rec(&mut self) -> io::Result<Option<CntRec>> {
        let mut b = [0u8; CNT_RECORD];
        let mut got = 0;
        while got < CNT_RECORD {
            let n = self.inner.read(&mut b[got..])?;
            if n == 0 {
                break;
            }
            got += n;
            if got >= 8 && &b[..8] == CNT_END {
                break;
            }
        }
        if got >= 8 && &b[..8] == CNT_END {
            let mut t = [0u8; 24];
            t[..got].copy_from_slice(&b[..got]);
            self.inner
                .read_exact(&mut t[got..])
                .map_err(|_| corrupt("damaged count-stream trailer".into()))?;
            let n = u64::from_le_bytes(t[8..16].try_into().unwrap());
            let sum = u64::from_le_bytes(t[16..24].try_into().unwrap());
            if n != self.records {
                return Err(corrupt(format!("trailer counts {} records, the stream holds {}", n, self.records)));
            }
            if sum != self.checksum {
                return Err(corrupt(format!("checksum {:#x} in the trailer, {:#x} over the records", sum, self.checksum)));
            }
            return Ok(None);
        }
        if got < CNT_RECORD {
            return Err(corrupt(format!("count stream ends after {} records without its trailer", self.records)));
        }
        let r = CntRec {
            tx: u64::from_le_bytes(b[0..8].try_into().unwrap()),
            block: u32::from_le_bytes(b[8..12].try_into().unwrap()),
            m: u32::from_le_bytes(b[12..16].try_into().unwrap()),
            rk: u32::from_le_bytes(b[16..20].try_into().unwrap()),
        };
        self.checksum = self.checksum.wrapping_add(r.check());
        let rec = self.records;
        self.records += 1;
        if r.block as u64 >= self.m {
            return Err(corrupt(format!("record {}: block {} is past the 2^{} blocks of the header", rec, r.block, self.spec.bits)));
        }
        self.frontier = Some(self.frontier.map_or(r.block, |f| f.max(r.block)));
        Ok(Some(r))
    }
}

/// The sketches `pick` dumps for one `--kmv`:
///
/// ```text
/// "KMVSEL01" | u32 version=1 | u32 k | u64 seed | u8 support | u8 bits | u16 0 | u32 N
/// N x (u32 len | u32 flags (bit 0: full) | len x u32 rank)
/// ```
pub const SEL_MAGIC: &[u8; 8] = b"KMVSEL01";

pub fn sel_header(spec: &KmvSpec, n: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(32);
    h.extend_from_slice(SEL_MAGIC);
    h.extend_from_slice(&1u32.to_le_bytes());
    h.extend_from_slice(&(spec.k as u32).to_le_bytes());
    h.extend_from_slice(&spec.seed.to_le_bytes());
    h.push(spec.support.code());
    h.push(spec.bits as u8);
    h.extend_from_slice(&0u16.to_le_bytes());
    h.extend_from_slice(&n.to_le_bytes());
    h
}

pub fn sel_record(out: &mut impl Write, s: View) -> io::Result<()> {
    out.write_all(&(s.len() as u32).to_le_bytes())?;
    out.write_all(&(s.full as u32).to_le_bytes())?;
    for r in s.r.iter() {
        out.write_all(&r.to_le_bytes())?;
    }
    Ok(())
}

/// A `KMVSEL01` file read back: its spec and every sketch, in selection
/// order.  The sketches are plain values here (no usage counters), since
/// `pairs` holds them on several threads.
pub fn read_sel(mut input: impl Read) -> io::Result<(KmvSpec, Vec<Owned>)> {
    let mut h = [0u8; 32];
    input.read_exact(&mut h)?;
    if &h[..8] != SEL_MAGIC {
        return Err(corrupt("not a KMVSEL01 file".into()));
    }
    let version = u32::from_le_bytes(h[8..12].try_into().unwrap());
    if version != 1 {
        return Err(corrupt(format!("KMVSEL01 version {}, this reads 1", version)));
    }
    let spec = KmvSpec {
        k: u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize,
        seed: u64::from_le_bytes(h[16..24].try_into().unwrap()),
        support: Support::from_code(h[24])?,
        bits: h[25] as u32,
    };
    spec.check_read("KMVSEL01")?;
    let n = u32::from_le_bytes(h[28..32].try_into().unwrap()) as usize;
    let mut out = Vec::with_capacity(n);
    let mut w = [0u8; 4];
    for _ in 0..n {
        input.read_exact(&mut w)?;
        let len = u32::from_le_bytes(w) as usize;
        if len > spec.k {
            return Err(corrupt(format!("KMVSEL01: a sketch of {} ranks at k = {}", len, spec.k)));
        }
        input.read_exact(&mut w)?;
        let full = u32::from_le_bytes(w) & 1 != 0;
        let mut raw = vec![0u8; 4 * len];
        input.read_exact(&mut raw)?;
        let r: Vec<u32> = raw.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect();
        out.push(Owned { r, full });
    }
    Ok((spec, out))
}

// ------------------------------------------------------------------ strata

/// The strata every count table is cut into, by the true size `n` against
/// `k`: `all>k`, `<=k` (counted exactly), `lg2:j` for `n` in `[k 2^j, k
/// 2^(j+1))` above `k`, and the fixed families, which are the same records
/// at every `k`.
pub const N_STRATA: usize = 2 + 32 + 6;

pub fn stratum_name(i: usize) -> &'static str {
    const LG2: [&str; 32] = [
        "lg2:0", "lg2:1", "lg2:2", "lg2:3", "lg2:4", "lg2:5", "lg2:6", "lg2:7", "lg2:8", "lg2:9", "lg2:10", "lg2:11",
        "lg2:12", "lg2:13", "lg2:14", "lg2:15", "lg2:16", "lg2:17", "lg2:18", "lg2:19", "lg2:20", "lg2:21", "lg2:22",
        "lg2:23", "lg2:24", "lg2:25", "lg2:26", "lg2:27", "lg2:28", "lg2:29", "lg2:30", "lg2:31",
    ];
    const FIX: [&str; 6] =
        ["fix:<=256", "fix:(256,4096]", "fix:(4096,32768]", "fix:(32768,131072]", "fix:(131072,262144]", "fix:>262144"];
    match i {
        0 => "all>k",
        1 => "<=k",
        2..=33 => LG2[i - 2],
        _ => FIX[i - 34],
    }
}

/// The strata of a record of true size `n`: two or three indices.
pub fn strata_of(n: u64, k: usize, out: &mut [usize; 3]) -> usize {
    let fix = 34
        + match n {
            0..=256 => 0,
            257..=4096 => 1,
            4097..=32768 => 2,
            32769..=131072 => 3,
            131073..=262144 => 4,
            _ => 5,
        };
    if n <= k as u64 {
        out[0] = 1;
        out[1] = fix;
        2
    } else {
        let j = (n / k as u64).ilog2() as usize;
        out[0] = 0;
        out[1] = 2 + j.min(31);
        out[2] = fix;
        3
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::hash::splitmix64;
    use std::collections::BTreeSet;

    #[test]
    fn specs_parse_and_label() {
        let s = KmvSpec::parse("1024").unwrap();
        assert_eq!(s.label(), "kmv-k1024-s1-sets");
        let s = KmvSpec::parse("64:seed=7:support=pos:bits=23").unwrap();
        assert_eq!(s.label(), "kmv-k64-s7-pos-b23");
        assert_eq!(crate::metrics::family(&s.label()), "kmv-k64-s*-pos-b23");
        assert!(KmvSpec::parse("1").is_err());
        assert!(KmvSpec::parse("64:bits=40").is_err());
        assert!(KmvSpec::parse("64:support=all").is_err());
        assert!(KmvSpec::parse("8192:bits=12").is_err());
    }

    fn brute(runs: &[Vec<u32>], k: usize) -> (Vec<u32>, bool) {
        let all: BTreeSet<u32> = runs.iter().flatten().copied().collect();
        (all.iter().take(k).copied().collect(), all.len() > k)
    }

    fn random_run(seed: u64, len: usize, span: u32) -> Vec<u32> {
        let s: BTreeSet<u32> = (0..len as u64).map(|i| (splitmix64(seed ^ (i << 20)) % span as u64) as u32).collect();
        s.into_iter().collect()
    }

    /// T-K2: the capped two-way merge, the tree and the heap all equal a
    /// brute-force bottom-k of the union, with truncation reported exactly:
    /// overlapping, identical and empty runs, k in {1, 2, 3, 16, 64}, 2 to
    /// 2,000 runs.
    #[test]
    fn capped_merges_are_the_bottom_k_of_the_union() {
        let mut s = MergeScratch::default();
        let mut out = Vec::new();
        let mut trial = 0u64;
        for &g in &[2usize, 3, 5, 17, 31, 32, 33, 100, 2047, 2048, 2100] {
            for &k in &[1usize, 2, 3, 16, 64] {
                for shape in 0..4 {
                    trial += 1;
                    let runs: Vec<Vec<u32>> = (0..g)
                        .map(|i| match shape {
                            0 => random_run(trial * 7919 + i as u64, (splitmix64(trial + i as u64) % 90) as usize, 400),
                            1 => random_run(trial, 40, 1000),
                            2 if i % 3 == 0 => Vec::new(),
                            2 => random_run(trial + i as u64, 5, 50),
                            _ => (0..k as u32 + 3).map(|x| x * g as u32 + i as u32).collect(),
                        })
                        .map(|mut r: Vec<u32>| {
                            r.truncate(k.max(1));
                            r
                        })
                        .collect();
                    let want = brute(&runs, k);
                    let cut = union_cap(g, |i| &runs[i], k, &mut out, &mut s);
                    assert_eq!((out.clone(), cut), want, "g {} k {} shape {}", g, k, shape);
                    let cut = tree_cap(g, |i| &runs[i], k, &mut out, &mut s);
                    assert_eq!((out.clone(), cut), want, "tree g {} k {} shape {}", g, k, shape);
                    let cut = heap_cap(g, |i| &runs[i], k, &mut out, &mut s);
                    assert_eq!((out.clone(), cut), want, "heap g {} k {} shape {}", g, k, shape);
                    if g == 2 {
                        let cut = merge_cap(&runs[0], &runs[1], k, &mut out);
                        assert_eq!((out.clone(), cut), want);
                    }
                }
            }
        }
        // A union of exactly k is not truncated; k + 1 is.
        assert!(!merge_cap(&[1, 3], &[2, 3], 3, &mut out));
        assert!(merge_cap(&[1, 3], &[2, 4], 3, &mut out));
        assert_eq!(out, vec![1, 2, 3]);
    }

    pub fn kmv_of(set: &BTreeSet<u32>, perm: &Perm, k: usize) -> Owned {
        let mut r: Vec<u32> = set.iter().map(|&b| perm.rank(b)).collect();
        r.sort_unstable();
        let full = r.len() > k;
        r.truncate(k);
        Owned { r, full }
    }

    /// T-K6, with A1's restricted universe: on synthetic sets of n in {k+1,
    /// 4k, 0.3 M, 0.7 M} over 2,000 seeds (each seed one independent
    /// permutation, so one independent estimate), the restricted count over
    /// `[0, beta]` and the count over `M` are both unbiased within 4 SE and
    /// have the RMS their formulas predict, within 10%.  One case is a
    /// contiguous range, which is what supports look like, and one is a dense
    /// random subset of `[0, beta]` (A1).
    #[test]
    fn counts_are_unbiased_over_seeds() {
        let bits = 14;
        let m = 1u64 << bits;
        let seeds = 2000u64;
        for &k in &[16usize, 64] {
            let sizes = [k + 1, 4 * k, (0.3 * m as f64) as usize, (0.7 * m as f64) as usize];
            for (case, &n) in sizes.iter().enumerate() {
                // Case 0: a contiguous range [a, a+n) with beta = a+n-1.
                // Case 1: a sparse random set over the whole universe.
                // Cases 2-3: dense random subsets of [0, beta], beta = n / 0.8.
                let beta: u32;
                let set: BTreeSet<u32> = match case {
                    0 => {
                        beta = 777 + n as u32 - 1;
                        (777..=beta).collect()
                    }
                    1 => {
                        let mut s = BTreeSet::new();
                        let mut i = 0u64;
                        while s.len() < n {
                            s.insert((splitmix64(i + 99) % m) as u32);
                            i += 1;
                        }
                        beta = *s.iter().last().unwrap();
                        s
                    }
                    _ => {
                        let b = ((n as f64 / 0.8) as u32).min(m as u32 - 1);
                        beta = b;
                        let mut s = BTreeSet::new();
                        let mut i = 0u64;
                        while s.len() < n {
                            s.insert((splitmix64(i * 31 + case as u64) % (b as u64 + 1)) as u32);
                            i += 1;
                        }
                        s
                    }
                };
                assert_eq!(set.len(), n);
                let n_univ = beta as u64 + 1;
                let (mut s1, mut s2, mut m1, mut m2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
                for seed in 0..seeds {
                    let perm = Arc::new(Perm::new(seed + 1000 * k as u64, bits));
                    let sk = kmv_of(&set, &perm, k);
                    let sk = sk.view();
                    assert!(sk.full);
                    let below = below_many(&perm, &[(n_univ, sk.rk().unwrap())])[0];
                    let e = count(sk, k, n_univ, below) / n as f64 - 1.0;
                    let em = count_m(sk, k, m) / n as f64 - 1.0;
                    s1 += e;
                    s2 += e * e;
                    m1 += em;
                    m2 += em * em;
                }
                let sf = seeds as f64;
                for (name, sum, sq, u) in [("universe", s1, s2, n_univ), ("M", m1, m2, m)] {
                    let mean = sum / sf;
                    let rms = (sq / sf).sqrt();
                    let theory = count_se(n as f64, k, u) / n as f64;
                    let se = ((sq / sf - mean * mean) / sf).sqrt();
                    assert!(mean.abs() < 4.0 * se + 1e-12, "{} k {} n {}: mean rel err {} (se {})", name, k, n, mean, se);
                    assert!((rms / theory - 1.0).abs() < 0.1, "{} k {} n {}: rms {} theory {}", name, k, n, rms, theory);
                }
            }
        }
    }

    /// The restricted count never exceeds its universe, and a whole
    /// universe is counted exactly.
    #[test]
    fn the_restricted_count_is_bounded_by_its_universe() {
        let perm = Arc::new(Perm::new(3, 12));
        let set: BTreeSet<u32> = (0..500).collect();
        let sk = kmv_of(&set, &perm, 16);
        let sk = sk.view();
        let b = below_many(&perm, &[(500, sk.rk().unwrap())])[0];
        assert_eq!(b, 15, "S is the whole universe, so 15 of it rank below its 16th");
        assert_eq!(count(sk, 16, 500, b), 500.0);
    }

    /// T-K7 and T-K8.  (a) Exact when both sketches are whole, including a
    /// union larger than k.  (b) Over 2,000 seeds (one estimate each), the
    /// mean is within 4 SE of J and the SD within 10% of the hypergeometric
    /// one, for |A| = |B| = 2,000 and J in {0, 0.024, 0.2, 0.95, 1}.  (c)
    /// The union count is unbiased.  Containment: NaN exactly when T is empty, and its mean
    /// within 4 SE of the truth when T is large.
    #[test]
    fn jaccard_and_containment_over_seeds() {
        let bits = 14;
        let m = 1u64 << bits;
        // (a)
        let perm = Perm::new(5, bits);
        let a: BTreeSet<u32> = (0..60).collect();
        let b: BTreeSet<u32> = (30..90).collect();
        let (ka, kb) = (kmv_of(&a, &perm, 64), kmv_of(&b, &perm, 64));
        assert!(!ka.full && !kb.full);
        let p = pair(ka.view(), kb.view(), 64, m);
        assert!(p.exact);
        assert_eq!(p.j, 30.0 / 90.0, "exact although |A u B| = 90 > k");
        assert_eq!(p.n_union, 90.0);
        assert_eq!(p.cont_ab, 0.5);
        assert_eq!(p.cont_ba, 0.5);
        // The textbook estimate over K_64(A u B) is not exact here.
        let both: BTreeSet<u32> = a.union(&b).copied().collect();
        let k_union = kmv_of(&both, &perm, 64);
        let hits = k_union.r.iter().filter(|r| ka.r.contains(r) && kb.r.contains(r)).count();
        assert_ne!(hits as f64 / 64.0, p.j);
        // (b), (c), containment
        let k = 64usize;
        let seeds = 2000u64;
        let n = 2000u32;
        for &shared in &[0u32, 95, 667, 1949, 2000] {
            // |A| = |B| = n, |A n B| = shared: J = shared / (2n - shared).
            let a: BTreeSet<u32> = (0..n).collect();
            let b: BTreeSet<u32> = (n - shared..2 * n - shared).collect();
            let nu = (2 * n - shared) as f64;
            let j = shared as f64 / nu;
            let (mut s1, mut s2, mut u1, mut c1, mut c2, mut cn) = (0.0f64, 0.0f64, 0.0f64, 0.0f64, 0.0f64, 0usize);
            for seed in 0..seeds {
                let perm = Perm::new(seed + 50_000, bits);
                let (ka, kb) = (kmv_of(&a, &perm, k), kmv_of(&b, &perm, k));
                let p = pair(ka.view(), kb.view(), k, m);
                s1 += p.j;
                s2 += p.j * p.j;
                u1 += p.n_union / nu - 1.0;
                if !p.cont_ab.is_nan() {
                    c1 += p.cont_ab;
                    c2 += p.cont_ab * p.cont_ab;
                    cn += 1;
                } else {
                    assert_eq!(p.t_ab, 0);
                }
            }
            let sf = seeds as f64;
            let mean = s1 / sf;
            let sd = (s2 / sf - mean * mean).max(0.0).sqrt();
            let want_sd = (j * (1.0 - j) / k as f64 * (nu - k as f64) / (nu - 1.0)).sqrt();
            assert!((mean - j).abs() <= 4.0 * want_sd / sf.sqrt() + 1e-12, "J {}: mean {}", j, mean);
            if want_sd > 0.0 {
                assert!((sd / want_sd - 1.0).abs() < 0.1, "J {}: sd {} vs {}", j, sd, want_sd);
            } else {
                assert_eq!(sd, 0.0);
            }
            assert!((u1 / sf).abs() < 0.02, "union count bias {}", u1 / sf);
            let cont = shared as f64 / n as f64;
            let cm = c1 / cn as f64;
            let csd = (c2 / cn as f64 - cm * cm).max(0.0).sqrt();
            assert!(cn as f64 > 0.95 * sf);
            assert!((cm - cont).abs() <= 4.0 * csd / (cn as f64).sqrt() + 0.002, "containment {} vs {}", cm, cont);
        }
        // Small A against a huge B: T can be empty, and then it is NaN.
        let perm = Perm::new(9, bits);
        let mut nan = 0;
        for off in 0..200u32 {
            let a: BTreeSet<u32> = (off * 3..off * 3 + 3).collect();
            let b: BTreeSet<u32> = (0..10_000).collect();
            let p = pair(kmv_of(&a, &perm, 16).view(), kmv_of(&b, &perm, 16).view(), 16, m);
            assert_eq!(p.cont_ab.is_nan(), p.t_ab == 0);
            nan += p.cont_ab.is_nan() as usize;
            if !p.cont_ab.is_nan() {
                assert_eq!(p.cont_ab, 1.0);
            }
        }
        assert!(nan > 100);
    }

    /// T-K7(c) over A1's restricted universe: for two dense random subsets
    /// `A` of `[0, beta_A]` and `B` of `[0, beta_B]`, the pair's union count
    /// over `N = max(beta_A, beta_B) + 1` ([`union_count`], with `below_many`
    /// as `pairs` calls it) is unbiased within 4 SE over 2,000 seeds and has
    /// the RMS of `count_se(|A u B|, k, N)` within 10%; it never exceeds `N`,
    /// and the `M` column is unbiased too.  Seeds are the unit: one
    /// permutation, one estimate.
    #[test]
    fn the_restricted_union_count_is_unbiased_over_seeds() {
        let bits = 14;
        let m = 1u64 << bits;
        let seeds = 2000u64;
        let dense = |beta: u32, n: usize, salt: u64| -> BTreeSet<u32> {
            let mut s = BTreeSet::new();
            let mut i = 0u64;
            while s.len() < n {
                s.insert((splitmix64(i * 7919 + salt) % (beta as u64 + 1)) as u32);
                i += 1;
            }
            s
        };
        // (k, beta_A, |A|, beta_B, |B|): equal universes; a nested-looking
        // pair with B's universe four times A's; and a sparse B in a wide
        // universe beside a dense A.
        for &(k, ba, na, bb, nb) in &[
            (64usize, 3999u32, 3000usize, 3999u32, 3000usize),
            (64, 1999, 1800, 7999, 6000),
            (16, 4999, 4000, 12999, 1500),
        ] {
            let a = dense(ba, na, 1);
            let b = dense(bb, nb, 2);
            let n_u = a.union(&b).count();
            let n_univ = ba.max(bb) as u64 + 1;
            let (mut s1, mut s2, mut m1, mut m2) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            for seed in 0..seeds {
                let perm = Arc::new(Perm::new(seed + 70_000 + k as u64, bits));
                let (ka, kb) = (kmv_of(&a, &perm, k), kmv_of(&b, &perm, k));
                let raw = pair_raw(ka.view(), kb.view(), k);
                let rk = raw.union_rk.expect("the union is larger than k");
                let below = below_many(&perm, &[(n_univ, rk)])[0];
                let brute = (0..n_univ).filter(|&x| perm.rank(x as u32) < rk).count() as u64;
                assert_eq!(below, brute);
                let c = union_count(&raw, k, n_univ, below);
                assert!(c <= n_univ as f64, "a union count of {} over {} blocks", c, n_univ);
                let e = c / n_u as f64 - 1.0;
                let em = union_count_m(&raw, k, m) / n_u as f64 - 1.0;
                s1 += e;
                s2 += e * e;
                m1 += em;
                m2 += em * em;
            }
            let sf = seeds as f64;
            for (name, sum, sq, u) in [("universe", s1, s2, n_univ), ("M", m1, m2, m)] {
                let mean = sum / sf;
                let rms = (sq / sf).sqrt();
                let theory = count_se(n_u as f64, k, u) / n_u as f64;
                let se = ((sq / sf - mean * mean) / sf).sqrt();
                assert!(mean.abs() < 4.0 * se, "{} k {} |A u B| {}: mean rel err {} (se {})", name, k, n_u, mean, se);
                assert!((rms / theory - 1.0).abs() < 0.1, "{} k {} |A u B| {}: rms {} theory {}", name, k, n_u, rms, theory);
            }
        }
    }

    /// Damaged headers are errors, not panics: a `bits` byte past the
    /// permutation's range, a `k` of 0, a foreign KMVSEL01 version, and a
    /// record whose block lies past `2^bits`.
    #[test]
    fn damaged_headers_are_invalid_data() {
        let spec = KmvSpec::parse("3:seed=4:bits=12").unwrap();
        let good = cnt_header(&spec).to_vec();
        for (at, v) in [(25usize, 64u8), (25, 200), (25, 11), (12, 0), (12, 1)] {
            let mut h = good.clone();
            h[at] = v;
            if at == 12 {
                h[13] = 0;
            }
            let e = CntDecoder::new(&h[8..], *CNT_MAGIC, None).err().expect("a damaged header is refused");
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{}", e);
        }
        // A block past 2^12 in a record.
        let mut bytes = good.clone();
        let mut enc = CntEncoder::default();
        let perm = Arc::new(Perm::new(4, 12));
        let r = CntRec { tx: 0, block: 5000, m: 1, rk: 0 };
        enc.record(&mut bytes, &r).unwrap();
        let mut dec = CntDecoder::new(&bytes[8..], *CNT_MAGIC, Some(perm)).unwrap();
        assert_eq!(dec.next().unwrap_err().kind(), io::ErrorKind::InvalidData);
        // KMVSEL01: a foreign version, a bad bits byte, a sketch longer than k.
        let sel = sel_header(&spec, 1);
        for (at, v) in [(8usize, 2u8), (25, 40), (25, 0)] {
            let mut h = sel.clone();
            h[at] = v;
            h.extend_from_slice(&[0u8; 8]);
            assert_eq!(read_sel(&h[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        }
        let mut h = sel.clone();
        h.extend_from_slice(&4u32.to_le_bytes());
        h.extend_from_slice(&1u32.to_le_bytes());
        h.extend_from_slice(&[0u8; 16]);
        assert_eq!(read_sel(&h[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
        let mut h = sel;
        h.extend_from_slice(&3u32.to_le_bytes());
        h.extend_from_slice(&1u32.to_le_bytes());
        for r in [1u32, 2, 3] {
            h.extend_from_slice(&r.to_le_bytes());
        }
        assert_eq!(read_sel(&h[..]).unwrap().1.len(), 1, "an intact one still reads");
    }

    /// T-K9 (stream half): a round trip, the checksum, fixed width.
    #[test]
    fn the_count_stream_round_trips() {
        let spec = KmvSpec::parse("3:seed=4:bits=12").unwrap();
        let perm = Arc::new(Perm::new(4, 12));
        let mut bytes = cnt_header(&spec).to_vec();
        let mut enc = CntEncoder::default();
        let sets: Vec<(u64, u32, BTreeSet<u32>)> = vec![
            (0, 5, [5].into()),
            (1, 6, [5, 6].into()),
            (2, 6, [1, 2, 3, 4, 5, 6].into()),
            (7, 9, (0..10).collect()),
        ];
        for (tx, block, s) in &sets {
            enc.record(&mut bytes, &CntRec::of(kmv_of(s, &perm, 3).view(), *tx, *block)).unwrap();
        }
        enc.trailer(&mut bytes).unwrap();
        assert_eq!(bytes.len(), CNT_HEADER + CNT_RECORD * 4 + 24);
        assert_eq!(&bytes[CNT_HEADER + CNT_RECORD * 3..CNT_HEADER + CNT_RECORD * 3 + 8], &7u64.to_le_bytes());
        let mut dec = CntDecoder::new(&bytes[8..], *CNT_MAGIC, Some(perm.clone())).unwrap();
        assert_eq!(dec.spec, spec);
        let mut rows = Vec::new();
        while let Some(r) = dec.next().unwrap() {
            rows.push(r);
        }
        assert_eq!(rows.len(), 4);
        assert_eq!(rows[1].n_hat, 2.0);
        assert!(rows[2].r.full() && rows[3].r.full());
        // Record 3 is the whole universe [0, 10): counted exactly.
        assert_eq!(rows[3].n_univ, 10);
        assert_eq!(rows[3].n_hat, 10.0);
        let mut bad = bytes.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        let mut dec = CntDecoder::new(&bad[8..], *CNT_MAGIC, Some(perm.clone())).unwrap();
        let mut res = Ok(Some(rows[0]));
        while let Ok(Some(_)) = res {
            res = dec.next();
        }
        assert!(res.is_err(), "a bad checksum is caught");
        let mut dec = CntDecoder::new(&bytes[8..bytes.len() - 24], *CNT_MAGIC, Some(perm)).unwrap();
        let mut res = Ok(Some(rows[0]));
        while let Ok(Some(_)) = res {
            res = dec.next();
        }
        assert!(res.is_err(), "a missing trailer is caught");
    }

    #[test]
    fn selections_round_trip() {
        let spec = KmvSpec::parse("4:seed=2:support=pos").unwrap();
        let mut b = sel_header(&spec, 2);
        sel_record(&mut b, View { r: &[1, 5, 9], full: false }).unwrap();
        sel_record(&mut b, View { r: &[0, 2, 3, 8], full: true }).unwrap();
        let (s, v) = read_sel(&b[..]).unwrap();
        assert_eq!(s, spec);
        assert_eq!(v, vec![Owned { r: vec![1, 5, 9], full: false }, Owned { r: vec![0, 2, 3, 8], full: true }]);
    }

    #[test]
    fn strata_cover_every_size() {
        let mut s = [0usize; 3];
        assert_eq!(strata_of(10, 64, &mut s), 2);
        assert_eq!(stratum_name(s[0]), "<=k");
        assert_eq!(stratum_name(s[1]), "fix:<=256");
        assert_eq!(strata_of(65, 64, &mut s), 3);
        assert_eq!(stratum_name(s[1]), "lg2:0");
        assert_eq!(strata_of(128, 64, &mut s), 3);
        assert_eq!(stratum_name(s[1]), "lg2:1");
        assert_eq!(stratum_name(s[2]), "fix:<=256");
        strata_of(300_000, 64, &mut s);
        assert_eq!(stratum_name(s[2]), "fix:>262144");
    }
}
