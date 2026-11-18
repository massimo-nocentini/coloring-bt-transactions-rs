//! # `select`: which records `pick` dumps, chosen from a count stream
//!
//! `pick --select <file>` folds its stores once and dumps the sketches of the
//! records the file names; this mode writes that file.  It reads one `count`
//! stream ([`crate::kmv::CNT_MAGIC`]) -- in practice the seed-1 `k = 1024`
//! `sets` stream -- and chooses records by depth and by position alone.
//! Nothing here looks at a colour, and nothing is drawn at random: within a
//! block the choice falls to one fixed hash of the record index, so the same
//! stream always gives the same bytes.
//!
//! It is the port of `select.py` (`cj/runs-final/select.py` in the evidence
//! folder), which wrote `sel/prefix.txt` for R3 and `sel/chain.txt` for R5 of
//! the post's section 10, and it writes those files again byte for byte from
//! the same streams.  The script decoded R4's 778,613,440 records into numpy
//! columns before choosing anything, 55 min and 49 GB; `select chain` keeps
//! one candidate per block and the deep records of the last few blocks, and
//! read the same stream in 40 s and 72 MB (2026-11-18).  `select prefix`
//! holds its prefix whole: 0.5 s and 148 MB over R2's first 3M records.
//!
//! Only the `.tsv` beside each selection is not the script's to the byte:
//! the script read `decode`'s text with pandas' default float parser, which
//! is not correctly rounded, and 192 of the prefix's 1,511 `n_hat` and 214 of
//! the chain's 1,280 came out one ulp off the double `decode` had printed.
//! This writes the double itself.
//!
//! ## The words
//!
//! - **Coinbase**: the first record of each block.  A coinbase descends from
//!   its own block only, so its support is one block; a first-of-block record
//!   whose support is larger is reported and treated as an ordinary record
//!   (on the 2022 chain there is none).
//! - **Deep**: a non-coinbase record with a large support.  The prefix has the
//!   exact `--sets` sizes on disk (`--sets-counts`) and takes `|S| >= k`; the
//!   chain has only the stream, and takes the records whose `k`-sketch is
//!   *full*, which is `|S| > k` exactly ([`crate::kmv`]'s T3: a sketch is
//!   whole iff the support fits in `k`), with no estimate involved.  The two
//!   differ at `|S| = k`, as the script's did.
//! - **Groups** are disjoint and taken in a fixed order: a record an earlier
//!   group took is not a candidate of a later one.  `pick` would otherwise keep
//!   such a record under its first group only, and the later group would come
//!   out one short without saying so.
//! - **By blocks** ([`by_blocks`]): `n` target blocks evenly spaced over
//!   `[lo, hi]` ([`targets`]); each takes the deep record of the first block at
//!   or after the target -- and after the block of the previous pick -- that
//!   has a candidate, and within that block the one with the smallest
//!   `splitmix64(rec ^ 0x5e1ec7)` ([`tie_key`]).  A pick from a later block than
//!   its target has *moved*; a target past the last candidate takes nothing.
//!   One record per block, so a group is spread over the chain however the
//!   deep records cluster.
//!
//! ## `select prefix` (R3)
//!
//! The first `--limit` records (3M), with the exact sizes beside them; a
//! whole sketch (`full = 0`) is its support, and must agree with the size on
//! disk, or the two files are not of the same records and nothing is written.
//! In this order:
//!
//! - `window`: the last `--window` (256) deep records before `--limit`, a
//!   window of deep rows a few blocks wide (the post's figure 4).
//! - `deep`: `--deep` (256) targets over `[b0, b1 - 1]`, `b0` the block of the
//!   first deep record and `b1` the block of record `limit - 1` (whose block may
//!   go on past the limit), by blocks.
//! - `even`: record `floor((2i + 1) limit / (2 even))` for `i < --even` (1000),
//!   or the next non-coinbase record not yet taken; no depth condition.
//!
//! ## `select chain` (R5)
//!
//! The whole stream, to its verified trailer.  In this order:
//!
//! - `w2013`, `w2017`: the first `--window` (256) deep records at or after the
//!   first record of block 240,000 and of block 470,000.
//! - `w2022`: the last `--window` deep records of the chain not already taken.
//! - `chain`: `--chain` (512) targets over `[50,000, 762,260]`, by blocks.
//!
//! The windows are found as the records go by: the first two open at their
//! block, and the last is a queue of the deep records of the newest blocks, cut
//! a whole block at a time once it holds `--window` of them.  The per-block
//! candidates of `chain` are kept as the stream goes, one per block, and at
//! the end the blocks the queue covers are recomputed without `w2022`'s
//! records ([`ChainScan`]).
//!
//! ## Files in `--out`
//!
//! - `<what>.txt` (`prefix.txt` or `chain.txt`): a `#` line naming the stream
//!   (`--counts`, as given), the depth rule and the stream's spec line, then
//!   `rec TAB group` in record order -- what `pick --select` reads
//!   ([`crate::pick::Selection`]).
//! - `<what>.tsv`: the same records with `block full m n_hat` from the stream
//!   (the columns of `decode`) and, for the prefix, the exact size `sc`.
//! - `<what>.groups.tsv`: one line per group in order of first record --
//!   records, pairs (`N (N-1) / 2`, what `pairs` will compute), the record and
//!   block ranges, the size (`sc` for the prefix, `n_hat` for the chain) at
//!   min, median and max, and for a by-blocks group its target span and how
//!   many targets, picks and moves it had (`NA` for the others).
//!
//! The same facts go to stderr in the script's words, one line per group,
//! and after them one the script did not print: the first deep record of
//! the stream, before any group took anything.
//!
//! ## What is kept of the Python, to the bit
//!
//! - The targets are numpy's: `linspace(lo, hi, n)` is `i * step + lo` with
//!   `step = (hi - lo) / (n - 1)` and the last point set to `hi`; `np.round`
//!   rounds half to even; `np.unique` sorts and drops repeats.
//! - The median of an even count is the mean of the two middle values, as
//!   pandas' (numpy's); stderr prints min, median and max as `%.0f` does, half
//!   to even ([`fmt0`]).
//! - The `.tsv`'s `n_hat` is spelled as pandas spells a double, Python's
//!   `repr` ([`py_float`]): `10.0`, not `10`, and an exponent as `1e+16`.
//!   The double is `decode`'s own (see above for the ulp the script lost).

use crate::feed;
use crate::hash::splitmix64;
use crate::kmv::{CntDecoder, CntRow};
use crate::metrics;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::{Child, Command, Stdio};

/// The salt of the within-block choice.
pub const TIE_SALT: u64 = 0x5E_1EC7;

/// `select chain`'s fixed blocks: the two early windows open at these, and
/// `chain`'s targets span `[CHAIN_LO, CHAIN_HI]` (the last block of 2022).
pub const W2013: u32 = 240_000;
pub const W2017: u32 = 470_000;
pub const CHAIN_LO: i64 = 50_000;
pub const CHAIN_HI: i64 = 762_260;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum What {
    Prefix,
    Chain,
}

impl What {
    pub fn parse(s: &str) -> Result<What, String> {
        match s {
            "prefix" => Ok(What::Prefix),
            "chain" => Ok(What::Chain),
            _ => Err(format!("select prefix|chain, not {:?}", s)),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            What::Prefix => "prefix",
            What::Chain => "chain",
        }
    }
}

pub struct SelectOpts {
    pub what: What,
    pub counts: String,
    pub out: String,
    pub sets_counts: Option<String>,
    pub limit: u64,
    pub window: usize,
    pub deep: usize,
    pub even: usize,
    pub chain: usize,
}

/// One record of the stream, as far as a selection cares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Row {
    pub rec: u64,
    pub block: u32,
    pub full: bool,
    /// The sketch's length (`k` when full).
    pub m: u32,
    pub n_hat: f64,
    /// The exact `--sets` size, for the prefix.
    pub sc: Option<u32>,
}

impl Row {
    pub fn of(r: &CntRow) -> Row {
        Row { rec: r.rec, block: r.r.block, full: r.r.full(), m: r.r.len(), n_hat: r.n_hat, sc: None }
    }
}

/// The within-block order of the by-blocks groups: smallest first.  A
/// bijection of the record index, so there are no ties.
#[inline]
pub fn tie_key(rec: u64) -> u64 {
    splitmix64(rec ^ TIE_SALT)
}

/// `np.unique(np.round(np.linspace(lo, hi, n)).astype(np.int64))`, to the bit.
pub fn targets(lo: i64, hi: i64, n: usize) -> Vec<i64> {
    let (start, stop) = (lo as f64, hi as f64);
    let mut y: Vec<f64> = (0..n).map(|i| i as f64).collect();
    if n > 1 {
        let div = (n - 1) as f64;
        let delta = stop - start;
        let step = delta / div;
        for v in y.iter_mut() {
            *v = if step == 0.0 { *v / div * delta } else { *v * step } + start;
        }
        y[n - 1] = stop;
    } else {
        // numpy: no step, `y * delta + start`, and `y` is `[0]`.
        for v in y.iter_mut() {
            *v = *v * (stop - start) + start;
        }
    }
    let mut t: Vec<i64> = y.iter().map(|v| v.round_ties_even() as i64).collect();
    t.sort_unstable();
    t.dedup();
    t
}

/// The coinbase rule and the chain-order check, record by record.
#[derive(Default)]
struct Blocks {
    prev: Option<u32>,
    /// First-of-block records whose support is more than one block.
    odd: u64,
}

impl Blocks {
    fn coinbase(&mut self, r: &Row) -> io::Result<bool> {
        let first = match self.prev {
            Some(p) if r.block < p => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("record {}: block {} after block {}: the stream is not in chain order", r.rec, r.block, p),
                ))
            }
            Some(p) => r.block != p,
            None => true,
        };
        self.prev = Some(r.block);
        if first && r.m != 1 {
            self.odd += 1;
        }
        Ok(first && r.m == 1)
    }

    fn note(&self, notes: &mut Vec<String>) {
        if self.odd > 0 {
            notes.push(format!(
                "warning: {} first-of-block records have a support of more than one block; treating them as non-coinbase",
                self.odd
            ));
        }
    }
}

/// The candidates of a by-blocks group, one per block: the eligible record
/// with the smallest [`tie_key`], in block order.  Offered records must come
/// in chain order.
#[derive(Default)]
pub struct Best {
    pub v: Vec<(u64, Row)>,
}

impl Best {
    pub fn offer(&mut self, r: Row) {
        let key = tie_key(r.rec);
        match self.v.last_mut() {
            Some(last) if last.1.block == r.block => {
                if key < last.0 {
                    *last = (key, r);
                }
            }
            _ => self.v.push((key, r)),
        }
    }

    /// Forget every block from `block` on.
    pub fn truncate_from(&mut self, block: u32) {
        let i = self.v.partition_point(|c| c.1.block < block);
        self.v.truncate(i);
    }
}

/// What a by-blocks group did.
#[derive(Clone, Debug, PartialEq)]
pub struct ByBlocks {
    pub group: String,
    pub lo: i64,
    pub hi: i64,
    pub targets: usize,
    pub picks: Vec<Row>,
    pub moved: usize,
}

/// One record per target: the candidate of the first block at or after the
/// target and after the previous pick's block.  Stops at the first target
/// with nothing left.
pub fn by_blocks(group: &str, best: &[(u64, Row)], lo: i64, hi: i64, n: usize, notes: &mut Vec<String>) -> ByBlocks {
    let t = targets(lo, hi, n);
    let mut picks = Vec::new();
    let (mut prev, mut moved) = (-1i64, 0usize);
    for &target in &t {
        let start = target.max(prev + 1);
        let i = best.partition_point(|c| (c.1.block as i64) < start);
        if i >= best.len() {
            break;
        }
        let b = best[i].1.block as i64;
        picks.push(best[i].1);
        moved += (b != target) as usize;
        prev = b;
    }
    if picks.len() < t.len() {
        notes.push(format!("{}: only {} of {} targets have a record", group, picks.len(), t.len()));
    }
    notes.push(format!("{}: {} of {} picks moved past their target block", group, moved, picks.len()));
    ByBlocks { group: group.to_string(), lo, hi, targets: t.len(), picks, moved }
}

/// A selection: every taken record with its group, in record order, the groups
/// in the order they were taken, and what the by-blocks groups did.
#[derive(Debug, Default)]
pub struct Outcome {
    pub rows: Vec<(Row, String)>,
    pub by_blocks: Vec<ByBlocks>,
    pub notes: Vec<String>,
    /// The first deep record of the stream, taken or not.
    pub first_deep: Option<Row>,
}

impl Outcome {
    fn take(&mut self, rows: impl IntoIterator<Item = Row>, group: &str) {
        self.rows.extend(rows.into_iter().map(|r| (r, group.to_string())));
    }

    /// Sort by record and refuse a record taken twice, the script's assertion.
    fn seal(&mut self) -> io::Result<()> {
        self.rows.sort_by_key(|r| r.0.rec);
        if let Some(w) = self.rows.windows(2).find(|w| w[0].0.rec == w[1].0.rec) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("record {} is in both {} and {}: the groups overlap", w[0].0.rec, w[0].1, w[1].1),
            ));
        }
        Ok(())
    }
}

// ------------------------------------------------------------------ prefix

/// The prefix's knobs.
#[derive(Clone, Copy, Debug)]
pub struct PrefixParams {
    pub k: usize,
    pub window: usize,
    pub deep: usize,
    pub even: usize,
    pub limit: u64,
}

/// `select prefix` over the first `limit` rows, each with its `sc`: all of
/// them in memory (the prefix is short; 3M rows are 120 MB).
pub fn select_prefix(rows: &[Row], p: PrefixParams) -> io::Result<Outcome> {
    let bad = |m: String| io::Error::new(io::ErrorKind::InvalidData, m);
    let n = rows.len();
    if n as u64 != p.limit {
        return Err(bad(format!("the stream has {} records below {}", n, p.limit)));
    }
    let sc = |r: &Row| r.sc.expect("the prefix has a size for every record");
    let disagree = rows.iter().filter(|r| !r.full && r.m != sc(r)).count();
    if disagree > 0 {
        return Err(bad(format!("{} whole sketches disagree with the sets-counts file", disagree)));
    }
    let mut blocks = Blocks::default();
    let mut cb = Vec::with_capacity(n);
    for r in rows {
        cb.push(blocks.coinbase(r)?);
    }
    let mut o = Outcome::default();
    blocks.note(&mut o.notes);
    let deep: Vec<bool> = rows.iter().zip(&cb).map(|(r, &c)| sc(r) as usize >= p.k && !c).collect();
    let mut taken = vec![false; n];

    let deep_idx: Vec<usize> = (0..n).filter(|&i| deep[i]).collect();
    let win = &deep_idx[deep_idx.len().saturating_sub(p.window)..];
    for &i in win {
        taken[i] = true;
    }
    o.take(win.iter().map(|&i| rows[i]), "window");

    let first = *deep_idx.first().ok_or_else(|| bad(format!("no record of the first {} is deep", n)))?;
    o.first_deep = Some(rows[first]);
    let (b0, b1) = (rows[first].block as i64, rows[n - 1].block as i64);
    let mut best = Best::default();
    for &i in deep_idx.iter().filter(|&&i| !taken[i]) {
        best.offer(rows[i]);
    }
    let g = by_blocks("deep", &best.v, b0, b1 - 1, p.deep, &mut o.notes);
    for r in &g.picks {
        taken[r.rec as usize] = true;
    }
    o.take(g.picks.iter().copied(), "deep");
    o.by_blocks.push(g);

    let mut even: Vec<usize> = Vec::with_capacity(p.even);
    for i in 0..p.even as u128 {
        let mut r = ((2 * i + 1) * p.limit as u128 / (2 * p.even as u128)) as usize;
        while r < n && (cb[r] || taken[r] || even.contains(&r)) {
            r += 1;
        }
        if r < n {
            even.push(r);
        }
    }
    o.take(even.iter().map(|&i| rows[i]), "even");
    o.seal()?;
    Ok(o)
}

// ------------------------------------------------------------------ chain

/// The chain's knobs: the early windows' blocks, `chain`'s span.
#[derive(Clone, Debug)]
pub struct ChainParams {
    pub window: usize,
    pub opens: Vec<(String, u32)>,
    pub last: String,
    pub targets: usize,
    pub lo: i64,
    pub hi: i64,
}

impl ChainParams {
    pub fn new(window: usize, targets: usize) -> ChainParams {
        ChainParams {
            window,
            opens: vec![("w2013".into(), W2013), ("w2017".into(), W2017)],
            last: "w2022".into(),
            targets,
            lo: CHAIN_LO,
            hi: CHAIN_HI,
        }
    }
}

/// `select chain` as the records go by, in memory one candidate per block
/// plus the deep records of the newest blocks.
///
/// A deep record goes to every early window that is open (its block is at or
/// past the window's) and not yet full; one taken by any is no candidate of
/// the rest.  The others go to `tail`, the deep records of the blocks from
/// `cut` on, where `cut` is the block of the `window`-th newest -- whole
/// blocks are dropped from its front, so every block in it is complete -- and
/// to `best`.  At the end `w2022` is the last `window` of the tail, and the
/// tail's blocks are recomputed in `best` without them: those are the only
/// blocks `w2022` can touch.
pub struct ChainScan {
    p: ChainParams,
    blocks: Blocks,
    open: Vec<Vec<Row>>,
    first_deep: Option<Row>,
    tail: VecDeque<Row>,
    best: Best,
}

impl ChainScan {
    pub fn new(p: ChainParams) -> ChainScan {
        let open = vec![Vec::new(); p.opens.len()];
        ChainScan { p, blocks: Blocks::default(), open, first_deep: None, tail: VecDeque::new(), best: Best::default() }
    }

    pub fn push(&mut self, r: Row) -> io::Result<()> {
        let cb = self.blocks.coinbase(&r)?;
        if !r.full || cb {
            return Ok(());
        }
        self.first_deep.get_or_insert(r);
        let mut claimed = 0;
        for (w, (_, start)) in self.open.iter_mut().zip(&self.p.opens) {
            if r.block >= *start && w.len() < self.p.window {
                w.push(r);
                claimed += 1;
            }
        }
        if claimed > 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("record {} falls in two windows: the windows overlap", r.rec),
            ));
        }
        if claimed == 1 {
            return Ok(());
        }
        self.best.offer(r);
        self.tail.push_back(r);
        if self.tail.len() > self.p.window {
            let cut = self.tail[self.tail.len() - self.p.window].block;
            while self.tail.front().is_some_and(|f| f.block < cut) {
                self.tail.pop_front();
            }
        }
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<Outcome> {
        let mut o = Outcome::default();
        self.blocks.note(&mut o.notes);
        o.first_deep = self.first_deep;
        for (w, (name, _)) in self.open.into_iter().zip(&self.p.opens) {
            o.take(w, name);
        }
        let keep = self.tail.len().saturating_sub(self.p.window);
        if let Some(front) = self.tail.front() {
            self.best.truncate_from(front.block);
        }
        for r in self.tail.iter().take(keep) {
            self.best.offer(*r);
        }
        o.take(self.tail.iter().skip(keep).copied(), &self.p.last);
        let g = by_blocks("chain", &self.best.v, self.p.lo, self.p.hi, self.p.targets, &mut o.notes);
        o.take(g.picks.iter().copied(), "chain");
        o.by_blocks.push(g);
        o.seal()?;
        Ok(o)
    }
}

// ------------------------------------------------------------------ formats

/// Python's `repr` of a double, which is what pandas writes: the shortest
/// round trip, `.0` on an integer, an exponent as `e+16` / `e-05` outside
/// `[1e-4, 1e16)`, and nothing for NaN (pandas' `na_rep`).  Rust's `{:?}`
/// has the same digits and the same switch to an exponent; only the
/// exponent is spelled differently.
pub fn py_float(x: f64) -> String {
    if x.is_nan() {
        return String::new();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let s = format!("{:?}", x);
    match s.split_once('e') {
        None => s,
        Some((mant, exp)) => {
            let (sign, digits) = match exp.strip_prefix('-') {
                Some(d) => ('-', d),
                None => ('+', exp),
            };
            format!("{}e{}{:0>2}", mant, sign, digits)
        }
    }
}

/// C's `%.0f`: the double rounded half to even (glibc rounds the exact binary
/// value, so only a true tie is a tie).
pub fn fmt0(x: f64) -> String {
    format!("{:.0}", x.round_ties_even())
}

/// The median as numpy takes it: the middle value, or the mean of the two.
pub fn median(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.total_cmp(b));
    let n = v.len();
    if n == 0 {
        f64::NAN
    } else if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    }
}

/// One group's line of the summary.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupLine {
    pub group: String,
    pub records: usize,
    pub rec: (u64, u64),
    pub block: (u32, u32),
    pub size: (f64, f64, f64),
}

/// The groups in order of their first record, as pandas' `groupby(sort=False)`
/// meets them.  `size` is the column summarised.
pub fn group_lines(o: &Outcome, size: impl Fn(&Row) -> f64) -> Vec<GroupLine> {
    let mut order: Vec<&str> = Vec::new();
    for (_, g) in &o.rows {
        if !order.contains(&g.as_str()) {
            order.push(g);
        }
    }
    order
        .into_iter()
        .map(|g| {
            let rows: Vec<&Row> = o.rows.iter().filter(|r| r.1 == g).map(|r| &r.0).collect();
            let mut s: Vec<f64> = rows.iter().map(|r| size(r)).collect();
            let min = s.iter().copied().fold(f64::INFINITY, f64::min);
            let max = s.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let med = median(&mut s);
            GroupLine {
                group: g.to_string(),
                records: rows.len(),
                rec: (rows.iter().map(|r| r.rec).min().unwrap(), rows.iter().map(|r| r.rec).max().unwrap()),
                block: (rows.iter().map(|r| r.block).min().unwrap(), rows.iter().map(|r| r.block).max().unwrap()),
                size: (min, med, max),
            }
        })
        .collect()
}

/// The three files, and the summary lines for stderr.
pub fn write(out: &str, what: What, header: &str, o: &Outcome) -> io::Result<Vec<String>> {
    std::fs::create_dir_all(out)?;
    let stem = format!("{}/{}", out, what.name());
    let with_sc = what == What::Prefix;
    let mut w = BufWriter::new(File::create(format!("{}.txt", stem))?);
    writeln!(w, "# {}", header)?;
    for (r, g) in &o.rows {
        writeln!(w, "{}\t{}", r.rec, g)?;
    }
    w.flush()?;

    let mut w = BufWriter::new(File::create(format!("{}.tsv", stem))?);
    writeln!(w, "rec\tgroup\tblock\tfull\tm\tn_hat{}", if with_sc { "\tsc" } else { "" })?;
    for (r, g) in &o.rows {
        write!(w, "{}\t{}\t{}\t{}\t{}\t{}", r.rec, g, r.block, r.full as u8, r.m, py_float(r.n_hat))?;
        if with_sc {
            write!(w, "\t{}", r.sc.map_or(String::new(), |s| s.to_string()))?;
        }
        writeln!(w)?;
    }
    w.flush()?;

    let size_name = if with_sc { "sc" } else { "n_hat" };
    let lines = group_lines(o, |r| if with_sc { r.sc.map_or(f64::NAN, |s| s as f64) } else { r.n_hat });
    let f = metrics::fmt;
    let mut w = BufWriter::new(File::create(format!("{}.groups.tsv", stem))?);
    writeln!(
        w,
        "group\trecords\tpairs\trec_min\trec_max\tblock_min\tblock_max\tsize\tsize_min\tsize_median\tsize_max\ttarget_lo\ttarget_hi\ttargets\tpicks\tmoved"
    )?;
    let mut summary = Vec::new();
    for l in &lines {
        let n = l.records as u64;
        write!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            l.group,
            n,
            n * n.saturating_sub(1) / 2,
            l.rec.0,
            l.rec.1,
            l.block.0,
            l.block.1,
            size_name,
            f(l.size.0),
            f(l.size.1),
            f(l.size.2)
        )?;
        match o.by_blocks.iter().find(|b| b.group == l.group) {
            Some(b) => writeln!(w, "\t{}\t{}\t{}\t{}\t{}", b.lo, b.hi, b.targets, b.picks.len(), b.moved)?,
            None => writeln!(w, "\tNA\tNA\tNA\tNA\tNA")?,
        }
        summary.push(format!(
            "{:<7} {:>4} records, rec {}..{}, blocks {}..{}, {} min/median/max {}/{}/{}",
            l.group,
            l.records,
            l.rec.0,
            l.rec.1,
            l.block.0,
            l.block.1,
            size_name,
            fmt0(l.size.0),
            fmt0(l.size.1),
            fmt0(l.size.2)
        ));
    }
    w.flush()?;
    Ok(summary)
}

// ------------------------------------------------------------------ the run

/// A count stream on disk, through `zstd -dc --long=31` when it ends in
/// `.zst` (the long window costs nothing on a stream written without one,
/// and reads one written with it).
struct Source {
    child: Option<Child>,
    dec: CntDecoder<BufReader<Box<dyn Read + Send>>>,
}

impl Source {
    fn open(path: &str) -> io::Result<Source> {
        let (child, raw): (Option<Child>, Box<dyn Read + Send>) = if path.ends_with(".zst") {
            let mut c = Command::new("zstd")
                .args(["-dc", "--long=31", "--", path])
                .stdout(Stdio::piped())
                .stdin(Stdio::null())
                .spawn()
                .map_err(|e| io::Error::new(e.kind(), format!("cannot run zstd -dc {}: {}", path, e)))?;
            let out = c.stdout.take().expect("piped");
            (Some(c), Box::new(out))
        } else {
            (None, Box::new(File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?))
        };
        let mut r = BufReader::with_capacity(1 << 22, raw);
        let mut magic = [0u8; 8];
        let dec = r
            .read_exact(&mut magic)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: no count-stream header: {}", path, e)))
            .and_then(|_| CntDecoder::new(r, magic, None).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e))));
        match dec {
            Ok(dec) => Ok(Source { child, dec }),
            Err(e) => {
                if let Some(mut c) = child {
                    let _ = c.kill();
                    let _ = c.wait();
                }
                Err(e)
            }
        }
    }

    /// `decode`'s first line for this stream.
    fn spec_line(&self) -> String {
        crate::kmv::cnt_spec_line(&self.dec.spec, self.dec.m)
    }

    /// Stop reading: kill `zstd` when the stream was not read to its end, else
    /// wait for it and fail on its failure.
    fn finish(mut self, to_end: bool) -> io::Result<()> {
        if let Some(mut c) = self.child.take() {
            if !to_end {
                let _ = c.kill();
                let _ = c.wait();
            } else {
                let st = c.wait()?;
                if !st.success() {
                    return Err(io::Error::other(format!("zstd -dc exited with {}", st)));
                }
            }
        }
        Ok(())
    }
}

/// The first `n` exact sizes, one per line (plain, or `.zst`).
fn read_sizes(path: &str, n: u64) -> io::Result<Vec<u32>> {
    let mut out = Vec::with_capacity(n as usize);
    let push = |b: std::sync::Arc<feed::CountBatch>| {
        out.extend_from_slice(&b.vals);
        true
    };
    let r = if path.ends_with(".zst") {
        let mut c = Command::new("zstd")
            .args(["-dc", "--", path])
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("cannot run zstd -dc {}: {}", path, e)))?;
        let r = feed::read_counts(BufReader::with_capacity(1 << 22, c.stdout.take().expect("piped")), n, push);
        let _ = c.kill();
        let _ = c.wait();
        r
    } else {
        let f = File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
        feed::read_counts(BufReader::with_capacity(1 << 22, f), n, push)
    };
    let read = r.map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
    if read < n {
        return Err(io::Error::new(io::ErrorKind::InvalidData, format!("{} has {} sizes for {} records", path, read, n)));
    }
    Ok(out)
}

pub fn run(opts: SelectOpts) -> io::Result<()> {
    if opts.window == 0 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "select: --window must be at least 1".to_string()));
    }
    if opts.what == What::Prefix && opts.sets_counts.is_none() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "select prefix needs --sets-counts <path>".to_string()));
    }
    let mut src = Source::open(&opts.counts)?;
    let spec = src.spec_line();
    let k = src.dec.spec.k;
    let (o, header, records) = match opts.what {
        What::Prefix => {
            let sc_path = opts.sets_counts.clone().expect("checked above");
            let mut rows = Vec::with_capacity(opts.limit.min(1 << 26) as usize);
            while (rows.len() as u64) < opts.limit {
                match src.dec.next()? {
                    Some(r) => rows.push(Row::of(&r)),
                    None => break,
                }
            }
            let to_end = (rows.len() as u64) < opts.limit;
            src.finish(to_end)?;
            let sizes = read_sizes(&sc_path, rows.len() as u64)?;
            for (r, s) in rows.iter_mut().zip(sizes) {
                r.sc = Some(s);
            }
            let p = PrefixParams { k, window: opts.window, deep: opts.deep, even: opts.even, limit: opts.limit };
            let n = rows.len();
            let o = select_prefix(&rows, p)?;
            let header = format!(
                "prefix of {} records from {}; deep = sets-counts >= {}; {}",
                opts.limit, opts.counts, k, spec
            );
            (o, header, n as u64)
        }
        What::Chain => {
            let mut scan = ChainScan::new(ChainParams::new(opts.window, opts.chain));
            while let Some(r) = src.dec.next()? {
                scan.push(Row::of(&r))?;
            }
            let records = src.dec.records;
            src.finish(true)?;
            let header = format!("whole chain from {}; deep = full k={} sketch (|S| > {}); {}", opts.counts, k, k, spec);
            (scan.finish()?, header, records)
        }
    };
    eprintln!("sketch select: {}: {} records, {}", opts.counts, records, spec);
    for n in &o.notes {
        eprintln!("sketch select: {}", n);
    }
    for l in write(&opts.out, opts.what, &header, &o)? {
        eprintln!("sketch select: {}", l);
    }
    if let Some(r) = o.first_deep {
        eprintln!("sketch select: first deep record: rec {}, block {}, m {}, n_hat {}", r.rec, r.block, r.m, metrics::fmt(r.n_hat));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(rec: u64, block: u32, full: bool, m: u32) -> Row {
        Row { rec, block, full, m, n_hat: if full { 1000.0 + rec as f64 } else { m as f64 }, sc: None }
    }

    /// A chain of `blocks` blocks: a coinbase first in each, then a random
    /// number of records, deep (full) with probability `p_deep` percent.
    fn stream(blocks: u32, seed: u64, p_deep: u64) -> Vec<Row> {
        let mut s = seed;
        let mut next = || {
            s = s.wrapping_add(1);
            splitmix64(s)
        };
        let mut v = Vec::new();
        for b in 0..blocks {
            v.push(row(v.len() as u64, b, false, 1));
            for _ in 0..next() % 9 {
                let deep = next() % 100 < p_deep;
                let m = if deep { 8 } else { 1 + (next() % 8) as u32 };
                v.push(row(v.len() as u64, b, deep, m));
            }
        }
        v
    }

    /// The script's chain, dense, as numpy does it: the reference the
    /// streaming [`ChainScan`] must agree with.
    fn chain_reference(rows: &[Row], p: &ChainParams) -> Option<Vec<(u64, String)>> {
        let n = rows.len();
        let block: Vec<u32> = rows.iter().map(|r| r.block).collect();
        let mut cb = vec![false; n];
        for i in 0..n {
            cb[i] = (i == 0 || block[i] != block[i - 1]) && rows[i].m == 1;
        }
        let deep: Vec<bool> = (0..n).map(|i| rows[i].full && !cb[i]).collect();
        let mut code: Vec<Option<String>> = vec![None; n];
        for (name, start) in &p.opens {
            let i0 = block.partition_point(|&b| b < *start);
            for i in (i0..n).filter(|&i| deep[i]).take(p.window) {
                if code[i].is_some() {
                    return None; // the script's assertion
                }
                code[i] = Some(name.clone());
            }
        }
        let free: Vec<usize> = (0..n).filter(|&i| deep[i] && code[i].is_none()).collect();
        for &i in &free[free.len().saturating_sub(p.window)..] {
            code[i] = Some(p.last.clone());
        }
        let idx: Vec<usize> = (0..n).filter(|&i| deep[i] && code[i].is_none()).collect();
        let b: Vec<i64> = idx.iter().map(|&i| block[i] as i64).collect();
        let mut prev = -1i64;
        for t in targets(p.lo, p.hi, p.targets) {
            let start = t.max(prev + 1);
            let i = b.partition_point(|&x| x < start);
            if i >= b.len() {
                break;
            }
            let j = b.partition_point(|&x| x <= b[i]);
            let k = (i..j).min_by_key(|&k| tie_key(idx[k] as u64)).unwrap();
            code[idx[k]] = Some("chain".into());
            prev = b[i];
        }
        Some((0..n).filter_map(|i| code[i].clone().map(|g| (i as u64, g))).collect())
    }

    fn try_scan(rows: &[Row], p: &ChainParams) -> io::Result<Outcome> {
        let mut s = ChainScan::new(p.clone());
        for r in rows {
            s.push(*r)?;
        }
        s.finish()
    }

    fn scan(rows: &[Row], p: &ChainParams) -> Outcome {
        try_scan(rows, p).unwrap()
    }

    fn small(window: usize, targets: usize, opens: &[u32], lo: i64, hi: i64) -> ChainParams {
        ChainParams {
            window,
            opens: opens.iter().enumerate().map(|(i, &b)| (format!("w{}", i), b)).collect(),
            last: "last".into(),
            targets,
            lo,
            hi,
        }
    }

    /// The streaming chain selection is the dense one, whatever the window,
    /// the density of deep records, and however the last window and the
    /// targets crowd the newest blocks; and where the early windows overlap,
    /// both refuse.
    #[test]
    fn the_streaming_chain_is_the_dense_one() {
        let mut compared = 0;
        for seed in 0..40u64 {
            let p_deep = [5, 30, 70, 100][seed as usize % 4];
            let rows = stream(300, seed, p_deep);
            for (window, targets, opens, lo, hi) in [
                (4, 50, vec![60u32, 150], 10i64, 299i64),
                (16, 300, vec![100], 0, 299),
                (3, 20, vec![], 250, 320),
                (40, 30, vec![10, 200], 5, 290),
                (1000, 10, vec![], 0, 299),
            ] {
                let p = small(window, targets, &opens, lo, hi);
                let want = chain_reference(&rows, &p);
                let scanned = try_scan(&rows, &p).ok();
                if let Some(o) = &scanned {
                    let first = (0..rows.len()).find(|&i| {
                        rows[i].full && !((i == 0 || rows[i].block != rows[i - 1].block) && rows[i].m == 1)
                    });
                    assert_eq!(o.first_deep.map(|r| r.rec), first.map(|i| i as u64));
                }
                let got = scanned.map(|o| o.rows.into_iter().map(|(r, g)| (r.rec, g)).collect::<Vec<_>>());
                assert_eq!(got, want, "seed {} window {} targets {}", seed, window, targets);
                compared += want.is_some() as usize;
            }
        }
        assert!(compared >= 150, "only {} of 200 cases had disjoint windows", compared);
    }

    #[test]
    fn overlapping_windows_are_refused() {
        let rows = stream(50, 3, 100);
        let p = small(1000, 5, &[10, 20], 0, 49);
        let mut s = ChainScan::new(p);
        let err = rows.iter().try_for_each(|r| s.push(*r)).unwrap_err().to_string();
        assert!(err.contains("two windows"), "{}", err);
    }

    #[test]
    fn a_stream_out_of_chain_order_is_refused() {
        let mut s = ChainScan::new(small(4, 4, &[], 0, 10));
        s.push(row(0, 5, false, 1)).unwrap();
        let err = s.push(row(1, 4, true, 8)).unwrap_err().to_string();
        assert!(err.contains("not in chain order"), "{}", err);
    }

    /// numpy's targets, computed by numpy 1.26 (`select.py`'s
    /// `np.unique(np.round(np.linspace(lo, hi, n)).astype(np.int64))`),
    /// including the ties `np.round` sends to even.
    #[test]
    fn targets_are_numpys() {
        let a = targets(50_000, 762_260, 512);
        assert_eq!(a.len(), 512);
        assert_eq!(&a[..5], &[50000, 51394, 52788, 54182, 55575]);
        assert_eq!(&a[509..], &[759472, 760866, 762260]);
        assert_eq!(a.iter().sum::<i64>(), 207_938_560);
        let b = targets(44_006, 178_609, 256);
        assert_eq!(b.len(), 256);
        assert_eq!(&b[..5], &[44006, 44534, 45062, 45590, 46117]);
        assert_eq!(&b[253..], &[177553, 178081, 178609]);
        assert_eq!(b.iter().sum::<i64>(), 28_494_720);
        assert_eq!(targets(0, 10, 4), vec![0, 3, 7, 10]);
        assert_eq!(targets(0, 3, 7), vec![0, 1, 2, 3]);
        assert_eq!(targets(0, 5, 11), vec![0, 1, 2, 3, 4, 5]);
        assert_eq!(targets(5, 5, 3), vec![5]);
        assert_eq!(targets(7, 9, 1), vec![7]);
        assert!(targets(7, 9, 0).is_empty());
    }

    /// A target past the last candidate takes nothing, a pick never reuses a
    /// block, and within a block the smallest key wins.
    #[test]
    fn by_blocks_moves_forward_and_stops() {
        let mut best = Best::default();
        for (rec, block) in [(1u64, 2u32), (2, 2), (3, 2), (4, 5), (5, 9)] {
            best.offer(row(rec, block, true, 8));
        }
        let in2 = (1..=3).min_by_key(|&r| tie_key(r)).unwrap();
        assert_eq!(best.v[0].1.rec, in2);
        let mut notes = Vec::new();
        // Targets 0, 2, 4, 6, 8, 10: 0 -> block 2 (moved), 2 -> block 5 (after
        // the previous pick; moved), 4 -> block 9 (moved), 6 -> nothing.
        let g = by_blocks("g", &best.v, 0, 10, 6, &mut notes);
        assert_eq!(g.picks.iter().map(|r| r.block).collect::<Vec<_>>(), vec![2, 5, 9]);
        assert_eq!(g.moved, 3);
        assert_eq!(notes, vec!["g: only 3 of 6 targets have a record", "g: 3 of 3 picks moved past their target block"]);
    }

    fn prefix_rows(rows: &[Row], sc: impl Fn(&Row) -> u32) -> Vec<Row> {
        rows.iter().map(|r| Row { sc: Some(sc(r)), ..*r }).collect()
    }

    /// The prefix's three groups on a small stream, against the script's rules
    /// written out by hand.
    #[test]
    fn the_prefix_groups() {
        let base = stream(120, 7, 40);
        let n = base.len();
        // Exact sizes: a whole sketch's length; a full one's 8 or more.
        let rows = prefix_rows(&base, |r| if r.full { 8 + (r.rec % 3) as u32 } else { r.m });
        let p = PrefixParams { k: 9, window: 5, deep: 12, even: 30, limit: n as u64 };
        let o = select_prefix(&rows, p).unwrap();
        let by = |g: &str| o.rows.iter().filter(|r| r.1 == g).map(|r| r.0.rec).collect::<Vec<_>>();
        let cb = |i: usize| (i == 0 || rows[i].block != rows[i - 1].block) && rows[i].m == 1;
        let deep: Vec<usize> = (0..n).filter(|&i| rows[i].sc.unwrap() >= 9 && !cb(i)).collect();
        assert_eq!(o.first_deep.map(|r| r.rec), Some(deep[0] as u64));
        let window: Vec<u64> = deep[deep.len() - 5..].iter().map(|&i| i as u64).collect();
        assert_eq!(by("window"), window);
        let picks = by("deep");
        assert!(!picks.is_empty() && picks.len() <= 12);
        for w in picks.windows(2) {
            assert!(rows[w[0] as usize].block < rows[w[1] as usize].block);
        }
        for &r in &picks {
            assert!(deep.contains(&(r as usize)) && !window.contains(&r));
        }
        let even = by("even");
        assert_eq!(even.len(), 30);
        for (i, &r) in even.iter().enumerate() {
            let target = ((2 * i + 1) * n / 60) as u64;
            assert!(r >= target && !cb(r as usize) && !window.contains(&r) && !picks.contains(&r));
            // Nothing eligible is skipped between the target and the pick.
            for s in target..r {
                assert!(cb(s as usize) || window.contains(&s) || picks.contains(&s) || even.contains(&s));
            }
        }
    }

    #[test]
    fn the_prefix_checks_its_inputs() {
        let base = stream(30, 1, 50);
        let p = PrefixParams { k: 9, window: 2, deep: 2, even: 2, limit: base.len() as u64 };
        let wrong = prefix_rows(&base, |r| r.m + if r.full { 0 } else { 1 });
        assert!(select_prefix(&wrong, p).unwrap_err().to_string().contains("disagree"));
        let right = prefix_rows(&base, |r| r.m);
        let short = PrefixParams { limit: p.limit + 1, ..p };
        assert!(select_prefix(&right, short).unwrap_err().to_string().contains("records below"));
        // No size reaches k: nothing is deep.
        assert!(select_prefix(&right, p).unwrap_err().to_string().contains("is deep"));
    }

    #[test]
    fn floats_print_as_python_does() {
        assert_eq!(py_float(10.0), "10.0");
        assert_eq!(py_float(1506.3054360235355), "1506.3054360235355");
        assert_eq!(py_float(0.0001), "0.0001");
        assert_eq!(py_float(0.00001), "1e-05");
        assert_eq!(py_float(1.5e-7), "1.5e-07");
        assert_eq!(py_float(1e16), "1e+16");
        assert_eq!(py_float(9999999999999998.0), "9999999999999998.0");
        assert_eq!(py_float(1.25e300), "1.25e+300");
        assert_eq!(py_float(f64::NAN), "");
        assert_eq!(fmt0(2.5), "2");
        assert_eq!(fmt0(3.5), "4");
        assert_eq!(fmt0(1254.4999), "1254");
        assert_eq!(fmt0(354640.5), "354640");
        assert_eq!(median(&mut [3.0, 1.0, 2.0, 10.0]), 2.5);
        assert_eq!(median(&mut [3.0, 1.0, 2.0]), 2.0);
    }

    /// The `.txt` is what `pick --select` reads, groups and all.
    #[test]
    fn the_selection_file_reads_back() {
        let rows = stream(200, 11, 60);
        let o = scan(&rows, &small(5, 40, &[50], 0, 199));
        let dir = std::env::temp_dir().join(format!("sketch-select-test-{}", std::process::id()));
        let dir = dir.to_str().unwrap().to_string();
        let lines = write(&dir, What::Chain, "test header", &o).unwrap();
        let sel = crate::pick::Selection::load(&format!("{}/chain.txt", dir)).unwrap();
        assert_eq!(sel.recs, o.rows.iter().map(|r| r.0.rec).collect::<Vec<_>>());
        assert_eq!(sel.groups, o.rows.iter().map(|r| r.1.clone()).collect::<Vec<_>>());
        let tsv = std::fs::read_to_string(format!("{}/chain.tsv", dir)).unwrap();
        assert_eq!(tsv.lines().count(), o.rows.len() + 1);
        let groups = std::fs::read_to_string(format!("{}/chain.groups.tsv", dir)).unwrap();
        assert_eq!(groups.lines().count(), lines.len() + 1);
        let n = o.rows.iter().filter(|r| r.1 == "w0").count();
        assert!(groups.lines().any(|l| l.starts_with(&format!("w0\t{}\t{}\t", n, n * (n - 1) / 2))));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
