//! # `pairs-stats`: what a `pairs` run says, seeds as the unit
//!
//! `pairs` writes, per group of a selection, every pair by every measure a
//! `pick` dumped -- `pairs.<group>.tsv` in f64, the `m.*.f32` matrices, the
//! `n.*.f32` counts and `rows.<group>.tsv` -- and stops there.  This reads
//! those outputs back and reduces them to the tables an account of the
//! estimators needs: is a store biased, does its spread match the theory,
//! what do the estimates look like across a window or across the chain.
//! Nothing is refolded and nothing is resampled, so a run takes seconds.
//!
//! ## Inputs
//!
//! `--in` names a `pairs` output directory, and may be repeated: one `pick`
//! cannot hold every seed a bias question wants (R3 folded 16 seeds of KMV at
//! `k = 1024`, R3b 48 more on the same selection), so the seeds of several
//! runs over the *same rows* are pooled.  The rows of a group must agree
//! record for record in every directory that has the group, or the run fails.
//! A label (`kmv-k1024-s17-sets`) is taken from the first directory that has
//! it; the exact columns (`X_*`) likewise.
//!
//! Each column is read from `pairs.<group>.tsv` when the directory has it,
//! and otherwise from the matrices: the TSV holds the f64 values the matrices
//! round to f32, and some columns exist only there (`n_union`, `se`,
//! `exact`, the containment fallbacks, the L1 `tv`, the varopt `jw_plugin`,
//! `X_tv`).  A table that needs a column some source lacks leaves that source
//! out, or the whole row out, and `sources.tsv` says which source came from
//! where.  Over f32 the statistics below move in the fifth or sixth digit.
//!
//! `--group <g>` restricts the groups (default: every `rows.<g>.tsv` of the
//! first `--in`, in name order); `--group a+b` pools groups `a` and `b` into
//! one, the way the error figure pools `deep` and `even`.  A pooled group has
//! the labels and exact columns that all of its parts have.
//!
//! ## Conventions
//!
//! - Pairs are the `pairs` order, `(i, j)` with `i < j`, row-major.  "Older"
//!   and "newer" go by record index, so `cont_old_in_new` is `cont_ij` when
//!   `rec_i < rec_j` and `cont_ji` otherwise, whatever the selection order.
//! - Quantiles interpolate linearly between order statistics, numpy's
//!   default (`np.quantile`, type 7), over the finite values; a median is
//!   the 0.5 quantile.  `metrics::quantile_sorted` (nearest rank) is *not*
//!   used here, because every published number this checks came from numpy.
//! - Seeds are the unit of every bias and variance test, as in `count`: one
//!   run shares one permutation (or projection), so a group's pairs move
//!   together and their spread within a seed says little.  A bias is the
//!   mean over seeds of each seed's mean error, its SE the across-seed SD
//!   (ddof 1) over `sqrt(S)`, and the threshold the two-sided Bonferroni
//!   Student quantile at family-wise level [`ALPHA`] over the strata tested,
//!   `t_{S-1}(1 - ALPHA / (2 m))`.  An SE of 0 gives `t = 0` when the bias is
//!   0 too (a stratum that is exact in every seed) and infinity otherwise;
//!   one seed has no SE and no `t` (`NA`), and is written for its bias and
//!   RMS alone (the `f64` L1 store, the varopt plug-in).
//! - Exact values read from f32 matrices are snapped back to the rationals
//!   they round where the counts allow it (`X_J`, `X_cont_*`, `X_J_pos`,
//!   from the integer intersection), and an estimate read from an f32
//!   matrix is compared with the truth rounded to f32, so that an exact
//!   estimate has error 0 (see [`Group::est_err`]).
//! - The hypergeometric SD of a KMV Jaccard is that of `prop:kmv-jaccard`,
//!   `sqrt(J (1 - J)/k . clip((U - k)/max(U - 1, 1), 0, 1))`, with the exact
//!   union `U = (n_i + n_j)/(1 + J)` against the exact values, and with the
//!   seed means of `J^` and `n_union` where there is none.
//! - The L1 floor of a pair with `J_w = 0` is `sqrt(2/m) / (2 sqrt(2 pi))`
//!   ([`crate::l1`]): 0.0176, 0.0125 and 0.0088 at `m = 256`, 512 and 1024.
//! - `W1` is `pairs`' normalised distance times `metrics::L` blocks, in years
//!   of `365.25 x 144` blocks.
//!
//! ## Outputs in `--out`, one row per group and source
//!
//! - `groups.tsv`: rows, pairs, record and block ranges, and the support
//!   sizes per source (`X_n` and the lowest seed of every KMV family).
//! - `sources.tsv`: every label, its kind, the directory it came from, and
//!   whether from the TSV or the f32 matrices.
//! - `quantiles.tsv`: per stratum (`all`, and block gaps of `[0, 5000)`,
//!   `[5000, 20000)`, `[20000, 60000)`, `[60000, inf)`), per measure, of the
//!   lowest seed of each family and of the exact values: count, undefined,
//!   mean, p1 ... p99 and the shares at 0, at 1, at least 0.9, at least
//!   0.999 and (L1) below the floor.  Measures: `J`, `cont_old_in_new`,
//!   `log10(1-cont)` over `cont < 1` and over `cont < 0.999` (above the
//!   resolution of `k = 1024`), `n_min/n_max` of the pair, `jw`, `w1_years`.
//! - `errors.tsv` (exact values needed): per label and per family, `J^ - J`
//!   (`sets` against `X_J`, `pos` against `X_J_pos`) and `J_w^ - J_w`: mean,
//!   RMS, p1, p99, largest absolute error, share beyond 0.05, beyond two
//!   hypergeometric SDs, beyond two of `pairs`' plug-in `se` (all pairs, and
//!   among those with `se > 0`); for KMV `sets`, the pairs of two whole
//!   sketches (both supports within `k`), how many of them have `J^` equal
//!   to the exact `J`, and how many a union beyond `k` (where the textbook
//!   formula would have estimated).  A family row averages its seeds' values
//!   (RMS as the root of the mean square; the fewest equal pairs over seeds).
//! - `jaccard-seeds.tsv` (exact needed): per KMV `sets` family with at least
//!   two seeds, per stratum of the exact `J` in `(0, 0.05]`, `(0.05, 0.2]`,
//!   `(0.2, 0.5]`, `(0.5, 0.9]`, `(0.9, 1)`: the bias test, the pooled RMS
//!   over the hypergeometric SD (and its square with an across-seed SE), the
//!   range of the per-seed ratio, and the share of (seed, pair) cells off by
//!   more than two SDs.
//! - `jaccard-bins.tsv` (exact needed): the same in 13 bins of `J`, edges
//!   0, .01, .03, .1, .2, ..., .9, .97, 1 (right-closed), over the pairs with
//!   `0 < J < 1` and at least one support larger than `k` (the others are
//!   exact); with the RMS's own SE.
//! - `containment.tsv` (exact needed): per KMV `sets` family with at least
//!   two seeds and per side (`cont_ij`, `cont_ji`), per stratum of the exact
//!   containment (`< 0.5`, `[0.5, 0.9)`, `[0.9, 1)`, `1`): the share of pairs
//!   where the estimate is defined, the bias test and the RMS.
//! - `jw-strata.tsv` (exact needed): per L1 family, per stratum of the exact
//!   `J_w` (`>= 0.05`, `< 0.01`, `[0.01, 0.05)`, `[0.05, 0.2)`, `[0.2,
//!   0.5)`, `[0.5, 0.9)`, `>= 0.9`): bias test, RMS, mean estimate and share
//!   below the floor; and the varopt plug-in `J_w` of every varopt label
//!   whose `jw_plugin` column was read.
//! - `jw-bins.tsv` (exact needed): mean `J_w^` in 11 bins of the exact `J_w`
//!   (edges 0, 1e-3, 3e-3, .01, .03, .1, .2, .3, .4, .5, .7, 1; left-closed,
//!   the last, `[0.7, 1]`, closed at 1), with SE across seeds and RMS, and the mean plug-in per bin.
//! - `seed-agreement.tsv` (`n_union` and `exact` needed, no exact values):
//!   per KMV family with at least two seeds, per cell the across-seed SD `s`
//!   of `J^` against the hypergeometric `sigma` at the seed means: the share
//!   of cells with `s <= sqrt(3) sigma` and the mean `(s/sigma)^2`, over the
//!   cells with `sigma > 0`, over the gated ones (`k Jbar` and `k (1 -
//!   Jbar)` both at least 10, where the hit counts are near normal) and by
//!   stratum of `Jbar`; `expected` is the share for independent normal
//!   estimates, `P(chi^2_{S-1} <= 3 (S - 1))`, `1 - e^-3 = 0.950` at three
//!   seeds.  Cells exact in every seed are counted apart (they must agree).
//! - `seed-pairs.tsv`: the same per pair of seeds `(a, b)`: the mean of
//!   `(J^_a - J^_b)^2 / (2 sigma^2)` (1 if the SD is right) and the mean
//!   difference over `sqrt(2)` RMS sigma, which shows a shared offset.
//! - `l1-precision.tsv` (the L1 `tv`, so TSV only): every `f32` store against
//!   its `f64` twin: largest and p99 relative difference of `TV^` over the
//!   pairs with `TV^(f64) > 0`, largest absolute difference of `TV^` and of
//!   `J_w^`, and the pairs at 0 in `f64` and in both.
//! - `support.tsv`: `pos` against `sets` at the same `k` and seed (share of
//!   pairs with the same `J^`, largest difference), and `X_J_pos` against
//!   `X_J`.
//! - `cont-rows.tsv`: per row, for the lowest seed of each KMV family, the
//!   containment of the row's support in the newer rows': how many are
//!   defined, undefined (no sampled element of the older support ranks below
//!   the newer one's threshold), not nested (`cont < 0.999`), the median of
//!   the defined ones, the oldest block among the newer rows of an undefined
//!   cell, and the largest fallback `I/n_a` there.  Not written for a pooled
//!   group.
//! - `spearman.tsv`: Spearman's rank correlation, ties at their mean rank,
//!   of the lowest-seed varopt `W1` with the block gap and with `1 - J^`, and
//!   of `1 - J^` with the block gap.
//!
//! Every number is written in full (`metrics::fmt`), rows in a fixed order:
//! the output is a function of the inputs alone.

use crate::metrics::{self, fmt, inc_gamma, t_quantile};
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};

pub struct StatsOpts {
    pub inputs: Vec<String>,
    pub out: String,
    pub groups: Vec<String>,
}

/// The family-wise level of every bias test (as in `count`).
pub const ALPHA: f64 = 0.01;

/// Nominal blocks a year: one every ten minutes.
pub const BLOCKS_PER_YEAR: f64 = 365.25 * 144.0;

/// The quantiles `quantiles.tsv` writes.
pub const QS: [f64; 7] = [0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99];

/// Block-gap strata, `[lo, hi)`.
pub const GAPS: [(f64, f64); 4] = [(0.0, 5_000.0), (5_000.0, 20_000.0), (20_000.0, 60_000.0), (60_000.0, f64::INFINITY)];

/// Strata of the exact `J`, `(lo, hi]` and `J < 1`.
pub const J_STRATA: [(f64, f64); 5] = [(0.0, 0.05), (0.05, 0.2), (0.2, 0.5), (0.5, 0.9), (0.9, 1.0)];

/// Edges of the 13 bins of the exact `J` (right-closed: `(e_i, e_{i+1}]`).
pub const J_BINS: [f64; 14] = [0.0, 0.01, 0.03, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 0.97, 1.0];

/// Edges of the 11 bins of the exact `J_w` (left-closed: `[e_i, e_{i+1})`).
pub const JW_BINS: [f64; 12] = [0.0, 1e-3, 3e-3, 1e-2, 3e-2, 0.1, 0.2, 0.3, 0.4, 0.5, 0.7, 1.0001];

/// Strata of the exact `J_w`, `[lo, hi)`; the first is the union of the
/// last four.
pub const JW_STRATA: [(&str, f64, f64); 7] = [
    ("[0.05,1]", 0.05, f64::INFINITY),
    ("[0,0.01)", f64::NEG_INFINITY, 0.01),
    ("[0.01,0.05)", 0.01, 0.05),
    ("[0.05,0.2)", 0.05, 0.2),
    ("[0.2,0.5)", 0.2, 0.5),
    ("[0.5,0.9)", 0.5, 0.9),
    ("[0.9,1]", 0.9, f64::INFINITY),
];

/// Strata of the exact containment, `[lo, hi)`, and `{1}` apart.
pub const C_STRATA: [(&str, f64, f64); 4] =
    [("[0,0.5)", f64::NEG_INFINITY, 0.5), ("[0.5,0.9)", 0.5, 0.9), ("[0.9,1)", 0.9, 1.0), ("{1}", 1.0, 1.0)];

/// The `Jbar` strata of `seed-agreement.tsv`, `[lo, hi)`, the last closed.
pub const JBAR_STRATA: [(f64, f64); 4] = [(0.0, 0.05), (0.05, 0.5), (0.5, 0.9), (0.9, 1.0001)];

// ------------------------------------------------------------------ numbers

/// The `p`-quantile of an ascending slice, linear between order statistics
/// (numpy's default).  NaN for an empty slice.
pub fn quantile_linear(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return f64::NAN;
    }
    let h = (sorted.len() - 1) as f64 * p;
    let lo = h.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    sorted[lo] + (h - lo as f64) * (sorted[hi] - sorted[lo])
}

/// The first and last of an ascending slice; NaN for an empty one.
fn ends(sorted: &[f64]) -> (f64, f64) {
    (sorted.first().copied().unwrap_or(f64::NAN), sorted.last().copied().unwrap_or(f64::NAN))
}

/// The finite values of `v`, ascending.
fn sorted_finite(v: impl Iterator<Item = f64>) -> Vec<f64> {
    let mut s: Vec<f64> = v.filter(|x| x.is_finite()).collect();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    s
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        f64::NAN
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

/// The SD with `ddof = 1`; NaN below two values.
fn sd1(v: &[f64]) -> f64 {
    if v.len() < 2 {
        return f64::NAN;
    }
    let m = mean(v);
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

/// The values that are not NaN.
fn defined(v: &[f64]) -> Vec<f64> {
    v.iter().copied().filter(|x| !x.is_nan()).collect()
}

/// `(bias, SE, t)` of per-seed means: seeds as the unit.
pub fn seed_test(means: &[f64]) -> (f64, f64, f64) {
    let m = defined(means);
    let b = mean(&m);
    let se = sd1(&m) / (m.len() as f64).sqrt();
    let t = if m.len() < 2 {
        f64::NAN
    } else if se > 0.0 {
        b / se
    } else if b == 0.0 {
        0.0
    } else {
        f64::INFINITY
    };
    (b, se, t)
}

/// The two-sided Bonferroni threshold over `m` tests with `s` seeds.
pub fn t_crit(s: usize, m: usize) -> f64 {
    if s < 2 || m == 0 {
        return f64::NAN;
    }
    t_quantile(1.0 - ALPHA / (2.0 * m as f64), (s - 1) as f64)
}

/// The hypergeometric variance of a KMV Jaccard estimate.
pub fn hyper_var(j: f64, u: f64, k: usize) -> f64 {
    let k = k as f64;
    j * (1.0 - j) / k * ((u - k) / (u - 1.0).max(1.0)).clamp(0.0, 1.0)
}

/// The L1 estimate of `J_w` at `J_w = 0`.
pub fn jw_floor(m: usize) -> f64 {
    (2.0 / m as f64).sqrt() / (2.0 * (2.0 * std::f64::consts::PI).sqrt())
}

/// Ranks from 1, ties at their mean rank.
pub fn ranks(v: &[f64]) -> Vec<f64> {
    let mut idx: Vec<usize> = (0..v.len()).collect();
    idx.sort_by(|&a, &b| v[a].partial_cmp(&v[b]).unwrap());
    let mut r = vec![0.0; v.len()];
    let mut i = 0;
    while i < idx.len() {
        let mut j = i;
        while j + 1 < idx.len() && v[idx[j + 1]] == v[idx[i]] {
            j += 1;
        }
        let avg = (i + j) as f64 / 2.0 + 1.0;
        for &x in &idx[i..=j] {
            r[x] = avg;
        }
        i = j + 1;
    }
    r
}

pub fn pearson(x: &[f64], y: &[f64]) -> f64 {
    let (mx, my) = (mean(x), mean(y));
    let (mut sxy, mut sxx, mut syy) = (0.0, 0.0, 0.0);
    for (a, b) in x.iter().zip(y) {
        sxy += (a - mx) * (b - my);
        sxx += (a - mx).powi(2);
        syy += (b - my).powi(2);
    }
    sxy / (sxx * syy).sqrt()
}

/// Spearman's rho over the pairs where both values are finite.
pub fn spearman(x: &[f64], y: &[f64]) -> (usize, f64) {
    let (a, b): (Vec<f64>, Vec<f64>) = x.iter().zip(y).filter(|(a, b)| a.is_finite() && b.is_finite()).map(|(a, b)| (*a, *b)).unzip();
    (a.len(), pearson(&ranks(&a), &ranks(&b)))
}

/// The index of `x` among the bins of `edges` (interior edges only), as
/// `np.digitize(x, edges[1:-1], right)`.
pub fn digitize(x: f64, edges: &[f64], right: bool) -> usize {
    let inner = &edges[1..edges.len() - 1];
    if right {
        inner.iter().filter(|&&e| e < x).count()
    } else {
        inner.iter().filter(|&&e| e <= x).count()
    }
}

// ------------------------------------------------------------------ labels

/// What a label names.
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Kmv { k: usize, seed: u64, support: String },
    L1 { m: usize, seed: u64, prec: String },
    Varopt { seed: u64 },
}

impl Kind {
    fn name(&self) -> &'static str {
        match self {
            Kind::Kmv { .. } => "kmv",
            Kind::L1 { .. } => "l1",
            Kind::Varopt { .. } => "varopt",
        }
    }
    fn seed(&self) -> u64 {
        match self {
            Kind::Kmv { seed, .. } | Kind::L1 { seed, .. } | Kind::Varopt { seed } => *seed,
        }
    }
}

/// `kmv-k<k>-s<seed>-<support>`, `l1-m<m>-s<seed>-<prec>`, `varopt-...`.
pub fn parse_label(label: &str) -> Option<Kind> {
    let t: Vec<&str> = label.split('-').collect();
    let num = |s: &str, p: char| s.strip_prefix(p).and_then(|x| x.parse::<u64>().ok());
    match t.first().copied() {
        Some("kmv") if t.len() == 4 => {
            Some(Kind::Kmv { k: num(t[1], 'k')? as usize, seed: num(t[2], 's')?, support: t[3].to_string() })
        }
        Some("l1") if t.len() == 4 => Some(Kind::L1 { m: num(t[1], 'm')? as usize, seed: num(t[2], 's')?, prec: t[3].to_string() }),
        Some("varopt") => Some(Kind::Varopt { seed: t.get(2).and_then(|s| num(s, 's')).unwrap_or(0) }),
        _ => None,
    }
}

/// One label of one group, and where its columns come from.
#[derive(Clone, Debug)]
struct Src {
    label: String,
    kind: Kind,
    dir: String,
    tsv: bool,
}

/// The columns read from a TSV, per kind; anything else is ignored.
const KMV_COLS: [&str; 10] = ["n_i", "n_j", "exact", "J", "se", "n_union", "cont_ij", "cont_ji", "cont_ij_fb", "cont_ji_fb"];
const L1_COLS: [&str; 3] = ["tv", "jw", "jw_se"];
const VO_COLS: [&str; 2] = ["w1", "jw_plugin"];
const X_COLS: [&str; 10] = ["X_n_i", "X_n_j", "X_J", "X_cont_ij", "X_cont_ji", "X_tv", "X_jw", "X_n_i_pos", "X_n_j_pos", "X_J_pos"];

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

fn num(s: &str, what: &str) -> io::Result<f64> {
    if s == "NA" {
        return Ok(f64::NAN);
    }
    s.parse::<f64>().map_err(|_| bad(format!("{}: not a number: {:?}", what, s)))
}

// ------------------------------------------------------------------ one group

/// One group of one selection, over every `--in` that has it.
struct Part {
    name: String,
    n: usize,
    rec: Vec<u64>,
    block: Vec<u32>,
    /// `rows.<g>.tsv`'s count columns (`<label>:n`, `X_n`, `X_n_pos`).
    rowcols: HashMap<String, Vec<f64>>,
    srcs: Vec<Src>,
    /// Where the exact columns come from, and whether from the TSV.
    exact: Option<(String, bool)>,
    /// Columns preloaded from the TSVs, by `<label>:<col>` or `X_<col>`.
    cols: HashMap<String, Vec<f64>>,
}

fn header(path: &str) -> io::Result<Vec<String>> {
    let mut line = String::new();
    BufReader::new(File::open(path)?).read_line(&mut line)?;
    Ok(line.trim_end_matches('\n').split('\t').map(String::from).collect())
}

/// A `rows.<g>.tsv`: records, blocks, and the count columns by name.
type Rows = (Vec<u64>, Vec<u32>, Vec<(String, Vec<f64>)>);

fn read_rows(path: &str) -> io::Result<Rows> {
    let f = BufReader::new(File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?);
    let mut lines = f.lines();
    let head: Vec<String> = lines.next().ok_or_else(|| bad(format!("{}: empty", path)))??.split('\t').map(String::from).collect();
    let at = |name: &str| head.iter().position(|h| h == name).ok_or_else(|| bad(format!("{}: no column {}", path, name)));
    let (ir, ib) = (at("rec")?, at("block")?);
    let counts: Vec<usize> = (0..head.len()).filter(|&i| head[i].ends_with(":n") || head[i].starts_with("X_n")).collect();
    let (mut rec, mut block) = (Vec::new(), Vec::new());
    let mut cols: Vec<(String, Vec<f64>)> = counts.iter().map(|&i| (head[i].clone(), Vec::new())).collect();
    for line in lines {
        let line = line?;
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() != head.len() {
            return Err(bad(format!("{}: {} fields, header has {}", path, c.len(), head.len())));
        }
        rec.push(c[ir].parse().map_err(|_| bad(format!("{}: bad rec {:?}", path, c[ir])))?);
        block.push(c[ib].parse().map_err(|_| bad(format!("{}: bad block {:?}", path, c[ib])))?);
        for (slot, &i) in cols.iter_mut().zip(&counts) {
            slot.1.push(num(c[i], path)?);
        }
    }
    if rec.is_empty() {
        return Err(bad(format!("{}: no rows, only the header", path)));
    }
    Ok((rec, block, cols))
}

/// The labels and matrix columns `m.<g>.<label>.<col>.f32` of a directory,
/// and whether it has the exact matrices.
fn matrices(dir: &str, g: &str) -> io::Result<(Vec<(String, String)>, bool)> {
    let pre = format!("m.{}.", g);
    let mut out = Vec::new();
    let mut exact = false;
    for e in std::fs::read_dir(dir)? {
        let name = e?.file_name().into_string().unwrap_or_default();
        let Some(rest) = name.strip_prefix(&pre).and_then(|r| r.strip_suffix(".f32")) else { continue };
        if rest.starts_with("X_") {
            exact |= rest == "X_J";
        } else if let Some((label, col)) = rest.rsplit_once('.') {
            out.push((label.to_string(), col.to_string()));
        }
    }
    out.sort();
    Ok((out, exact))
}

/// Read the wanted columns of a `pairs.<g>.tsv` in one pass, checking that
/// its pairs are in `pairs`' order.
fn read_tsv(path: &str, n: usize, want: &[String]) -> io::Result<HashMap<String, Vec<f64>>> {
    let mut r = BufReader::with_capacity(1 << 22, File::open(path)?);
    let mut line = String::new();
    r.read_line(&mut line)?;
    let head: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
    let at = |name: &str| head.iter().position(|h| *h == name);
    let (ii, ij) = (at("i").ok_or_else(|| bad(format!("{}: no i", path)))?, at("j").ok_or_else(|| bad(format!("{}: no j", path)))?);
    let idx: Vec<(usize, String)> = want.iter().filter_map(|w| at(w).map(|i| (i, w.clone()))).collect();
    let npairs = n * n.saturating_sub(1) / 2;
    let mut vals: Vec<Vec<f64>> = vec![Vec::with_capacity(npairs); idx.len()];
    let (mut a, mut b) = (0usize, 1usize);
    let width = head.len();
    let mut count = 0usize;
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            break;
        }
        let c: Vec<&str> = line.trim_end_matches('\n').split('\t').collect();
        if c.len() != width {
            return Err(bad(format!("{}: line {} has {} fields, header {}", path, count + 2, c.len(), width)));
        }
        if c[ii] != a.to_string() || c[ij] != b.to_string() {
            return Err(bad(format!("{}: line {} is pair ({}, {}), expected ({}, {})", path, count + 2, c[ii], c[ij], a, b)));
        }
        for (v, (i, w)) in vals.iter_mut().zip(&idx) {
            v.push(num(c[*i], w)?);
        }
        count += 1;
        b += 1;
        if b == n {
            a += 1;
            b = a + 1;
        }
    }
    if count != npairs {
        return Err(bad(format!("{}: {} pairs for {} rows", path, count, n)));
    }
    Ok(idx.into_iter().map(|x| x.1).zip(vals).collect())
}

fn read_f32(path: &str, len: usize) -> io::Result<Vec<f32>> {
    let mut bytes = Vec::new();
    File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?.read_to_end(&mut bytes)?;
    if bytes.len() != 4 * len {
        return Err(bad(format!("{}: {} bytes, expected {}", path, bytes.len(), 4 * len)));
    }
    Ok(bytes.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect())
}

/// The pairs of an `N x N` matrix in `pairs`' order: cell `(a, b)` above the
/// diagonal, or `(b, a)` below it.
fn matrix_pairs(path: &str, n: usize, lower: bool) -> io::Result<Vec<f64>> {
    let m = read_f32(path, n * n)?;
    let mut out = Vec::with_capacity(n * n.saturating_sub(1) / 2);
    for a in 0..n {
        for b in a + 1..n {
            out.push(if lower { m[b * n + a] } else { m[a * n + b] } as f64);
        }
    }
    Ok(out)
}

impl Part {
    fn load(g: &str, dirs: &[String]) -> io::Result<Option<Part>> {
        let mut part: Option<Part> = None;
        for dir in dirs {
            let rows = format!("{}/rows.{}.tsv", dir, g);
            if !std::path::Path::new(&rows).exists() {
                continue;
            }
            let (rec, block, rowcols) = read_rows(&rows)?;
            let p = part.get_or_insert_with(|| Part {
                name: g.to_string(),
                n: rec.len(),
                rec: rec.clone(),
                block: block.clone(),
                rowcols: HashMap::new(),
                srcs: Vec::new(),
                exact: None,
                cols: HashMap::new(),
            });
            if rec != p.rec {
                return Err(bad(format!("{} and {}/rows.{}.tsv disagree on the rows of group {}", dirs[0], dir, g, g)));
            }
            for (k, v) in rowcols {
                p.rowcols.entry(k).or_insert(v);
            }
            let tsv_path = format!("{}/pairs.{}.tsv", dir, g);
            let tsv_head = if std::path::Path::new(&tsv_path).exists() { header(&tsv_path)? } else { Vec::new() };
            let mut labels: Vec<(String, bool)> = Vec::new();
            for h in &tsv_head {
                if let Some((l, _)) = h.split_once(':') {
                    if !labels.iter().any(|x| x.0 == l) {
                        labels.push((l.to_string(), true));
                    }
                }
            }
            let (mats, mx) = matrices(dir, g)?;
            for (l, _) in mats {
                if !labels.iter().any(|x| x.0 == l) {
                    labels.push((l, false));
                }
            }
            let mut from_tsv = Vec::new();
            for (label, tsv) in labels {
                let Some(kind) = parse_label(&label) else { continue };
                if p.srcs.iter().any(|s| s.label == label) {
                    continue;
                }
                if tsv {
                    let cols: &[&str] = match kind {
                        Kind::Kmv { .. } => &KMV_COLS,
                        Kind::L1 { .. } => &L1_COLS,
                        Kind::Varopt { .. } => &VO_COLS,
                    };
                    from_tsv.extend(cols.iter().map(|c| format!("{}:{}", label, c)));
                }
                p.srcs.push(Src { label, kind, dir: dir.clone(), tsv });
            }
            let x_tsv = tsv_head.iter().any(|h| h == "X_J");
            if p.exact.is_none() && (x_tsv || mx) {
                p.exact = Some((dir.clone(), x_tsv));
                if x_tsv {
                    from_tsv.extend(X_COLS.iter().map(|c| c.to_string()));
                }
            }
            if !from_tsv.is_empty() {
                let n = p.n;
                p.cols.extend(read_tsv(&tsv_path, n, &from_tsv)?);
            }
        }
        if let Some(p) = &mut part {
            p.srcs.sort_by(|a, b| {
                (metrics::family(&a.label), a.kind.seed()).cmp(&(metrics::family(&b.label), b.kind.seed()))
            });
        }
        Ok(part)
    }

    fn npairs(&self) -> usize {
        self.n * self.n.saturating_sub(1) / 2
    }

    /// The pairs of a row vector, `f(v_i, v_j)`.
    fn row_pairs(&self, v: &[f64], f: impl Fn(f64, f64) -> f64) -> Vec<f64> {
        let mut out = Vec::with_capacity(self.npairs());
        for a in 0..self.n {
            for b in a + 1..self.n {
                out.push(f(v[a], v[b]));
            }
        }
        out
    }

    /// The exact intersection and union behind an f32 Jaccard: `I = J (n_a +
    /// n_b)/(1 + J)` is an integer, and the f32 rounding of `J` (relative
    /// `2^-24`) moves it by at most `s J/(1 + J)^2 2^-24 <= s 2^-26`, `s =
    /// n_a + n_b`, so while that is below 0.05 rounding it recovers the exact
    /// rational `I/(s - I)` the matrix rounded.  NaN where that is not
    /// clear-cut (`s` past about 3.3M, or off an integer by more than 0.05),
    /// and the f32 value stays.  Without this a pair at `J = 0.05` exactly
    /// would cross a stratum edge as `0.050000001`.
    fn snap(&self, j: &[f64], xn: &[f64]) -> (Vec<f64>, Vec<f64>) {
        let (mut inter, mut union) = (Vec::with_capacity(j.len()), Vec::with_capacity(j.len()));
        let mut p = 0;
        for a in 0..self.n {
            for b in a + 1..self.n {
                let s = xn[a] + xn[b];
                let i = j[p] * s / (1.0 + j[p]);
                let r = i.round();
                let sure = s * (-26f64).exp2() < 0.05;
                if sure && (i - r).abs() < 0.05 && r >= 0.0 && r <= xn[a].min(xn[b]) && s - r > 0.0 {
                    inter.push(r);
                    union.push(s - r);
                } else {
                    inter.push(f64::NAN);
                    union.push(f64::NAN);
                }
                p += 1;
            }
        }
        (inter, union)
    }

    /// Whether `key` is read from an f32 matrix.
    fn is_f32(&self, key: &str) -> bool {
        if self.cols.contains_key(key) {
            return false;
        }
        if key.starts_with("X_") {
            return matches!(self.exact, Some((_, false)));
        }
        let label = key.split_once(':').map_or(key, |x| x.0);
        self.srcs.iter().any(|s| s.label == label && !s.tsv)
    }

    /// A column by `<label>:<col>` or `X_<col>`, from the TSV if it was
    /// read, else from the matrices; `None` if neither has it.
    fn col(&self, key: &str) -> io::Result<Option<Vec<f64>>> {
        if let Some(v) = self.cols.get(key) {
            return Ok(Some(v.clone()));
        }
        let (n, g) = (self.n, &self.name);
        if key.starts_with("X_") {
            let Some((dir, false)) = &self.exact else { return Ok(None) };
            let m = |name: &str, lower: bool| matrix_pairs(&format!("{}/m.{}.{}.f32", dir, g, name), n, lower).map(Some);
            let ratio = |inter: &[f64], den: &dyn Fn(usize) -> f64, raw: &[f64]| -> Vec<f64> {
                (0..raw.len()).map(|p| if inter[p].is_nan() { raw[p] } else { inter[p] / den(p) }).collect()
            };
            return match key {
                "X_jw" => m(key, false),
                "X_J" | "X_cont_ij" | "X_cont_ji" => {
                    let (Some(j), Some(xn)) = (m("X_J", false)?, self.rowcols.get("X_n")) else { return m(key, false) };
                    let (inter, union) = self.snap(&j, xn);
                    if key == "X_J" {
                        return Ok(Some(ratio(&inter, &|p| union[p], &j)));
                    }
                    let raw = m("X_cont", key == "X_cont_ji")?.unwrap_or_default();
                    let rows: Vec<(usize, usize)> = (0..n).flat_map(|a| (a + 1..n).map(move |b| (a, b))).collect();
                    let side = |p: usize| if key == "X_cont_ij" { xn[rows[p].0] } else { xn[rows[p].1] };
                    Ok(Some(ratio(&inter, &side, &raw)))
                }
                "X_J_pos" => {
                    let (Some(j), Some(xn)) = (m("X_J_pos", false)?, self.rowcols.get("X_n_pos")) else { return m(key, false) };
                    let (inter, union) = self.snap(&j, xn);
                    Ok(Some(ratio(&inter, &|p| union[p], &j)))
                }
                "X_n_i" | "X_n_j" => Ok(self.rowcols.get("X_n").map(|v| self.row_pairs(v, |a, b| if key == "X_n_i" { a } else { b }))),
                _ => Ok(None),
            };
        }
        let Some((label, c)) = key.split_once(':') else { return Ok(None) };
        let Some(src) = self.srcs.iter().find(|s| s.label == label) else { return Ok(None) };
        if src.tsv {
            return Ok(None);
        }
        let m = |col: &str, lower: bool| {
            let path = format!("{}/m.{}.{}.{}.f32", src.dir, g, label, col);
            if std::path::Path::new(&path).exists() {
                matrix_pairs(&path, n, lower).map(Some)
            } else {
                Ok(None)
            }
        };
        match c {
            "J" => m("J", false),
            "cont_ij" => m("cont", false),
            "cont_ji" => m("cont", true),
            "jw" => m("jw", false),
            "w1" => m("w1", false),
            "n_i" | "n_j" => {
                Ok(self.rowcols.get(&format!("{}:n", label)).map(|v| self.row_pairs(v, |a, b| if c == "n_i" { a } else { b })))
            }
            _ => Ok(None),
        }
    }
}

/// `(J, U, n_i, n_j)` per pair.
type ExactUnion = (Vec<f64>, Vec<f64>, Vec<f64>, Vec<f64>);

/// A group, or several pooled (`a+b`).
struct Group {
    name: String,
    parts: Vec<Part>,
}

impl Group {
    fn npairs(&self) -> usize {
        self.parts.iter().map(|p| p.npairs()).sum()
    }

    fn col(&self, key: &str) -> io::Result<Option<Vec<f64>>> {
        let mut out = Vec::with_capacity(self.npairs());
        for p in &self.parts {
            match p.col(key)? {
                Some(v) => out.extend(v),
                None => return Ok(None),
            }
        }
        Ok(Some(out))
    }

    /// The estimates of `key` and their errors against `x`.  Where a part
    /// read the estimate from an f32 matrix the truth is rounded to f32
    /// first: an exact estimate then has error 0, not the rounding of the
    /// truth, which would count as an error beyond an SD of 0.
    fn est_err(&self, key: &str, x: &[f64]) -> io::Result<Option<(Vec<f64>, Vec<f64>)>> {
        let (mut est, mut err) = (Vec::with_capacity(x.len()), Vec::with_capacity(x.len()));
        let mut at = 0;
        for p in &self.parts {
            let Some(v) = p.col(key)? else { return Ok(None) };
            let f32 = p.is_f32(key);
            for (i, e) in v.into_iter().enumerate() {
                let t = x[at + i];
                err.push(e - if f32 { t as f32 as f64 } else { t });
                est.push(e);
            }
            at += p.npairs();
        }
        Ok(Some((est, err)))
    }

    fn has_exact(&self) -> bool {
        self.parts.iter().all(|p| p.exact.is_some())
    }

    /// The labels every part has, in family then seed order.
    fn srcs(&self) -> Vec<Src> {
        self.parts[0].srcs.iter().filter(|s| self.parts[1..].iter().all(|p| p.srcs.iter().any(|t| t.label == s.label))).cloned().collect()
    }

    /// The families of the labels, each with its labels by seed.
    fn families(&self) -> Vec<(String, Vec<Src>)> {
        let mut out: Vec<(String, Vec<Src>)> = Vec::new();
        for s in self.srcs() {
            let f = metrics::family(&s.label);
            match out.iter_mut().find(|x| x.0 == f) {
                Some(x) => x.1.push(s),
                None => out.push((f, vec![s])),
            }
        }
        out
    }

    /// Per pair over the parts.
    fn per_pair<T>(&self, f: impl Fn(&Part, usize, usize) -> T) -> Vec<T> {
        let mut out = Vec::with_capacity(self.npairs());
        for p in &self.parts {
            for a in 0..p.n {
                for b in a + 1..p.n {
                    out.push(f(p, a, b));
                }
            }
        }
        out
    }

    fn gaps(&self) -> Vec<f64> {
        self.per_pair(|p, a, b| (p.block[a] as f64 - p.block[b] as f64).abs())
    }

    /// 1 where row `i` of the pair is the older (smaller record index).
    fn i_older(&self) -> Vec<bool> {
        self.per_pair(|p, a, b| p.rec[a] < p.rec[b])
    }

    /// The containment of the older support in the newer, from the two
    /// sides `<prefix>cont_ij` and `<prefix>cont_ji`.
    fn old_in_new(&self, prefix: &str) -> io::Result<Option<Vec<f64>>> {
        let (Some(ij), Some(ji)) = (self.col(&format!("{}cont_ij", prefix))?, self.col(&format!("{}cont_ji", prefix))?) else {
            return Ok(None);
        };
        Ok(Some(self.i_older().iter().zip(ij.iter().zip(&ji)).map(|(&o, (&a, &b))| if o { a } else { b }).collect()))
    }

    /// `min(n_i, n_j) / max(n_i, n_j)` of a row-count column.
    fn n_ratio(&self, rowcol: &str) -> Option<Vec<f64>> {
        let mut out = Vec::with_capacity(self.npairs());
        for p in &self.parts {
            let v = p.rowcols.get(rowcol)?;
            out.extend(p.row_pairs(v, |a, b| a.min(b) / a.max(b)));
        }
        Some(out)
    }

    /// The exact `J`, the exact union `U = (n_i + n_j)/(1 + J)`, and `n_i`,
    /// `n_j`, per pair.
    fn exact_union(&self) -> io::Result<Option<ExactUnion>> {
        let (Some(j), Some(ni), Some(nj)) = (self.col("X_J")?, self.col("X_n_i")?, self.col("X_n_j")?) else { return Ok(None) };
        let u = (0..j.len()).map(|p| (ni[p] + nj[p]) / (1.0 + j[p])).collect();
        Ok(Some((j, u, ni, nj)))
    }
}

// ------------------------------------------------------------------ output

struct Table {
    w: BufWriter<File>,
}

impl Table {
    fn create(dir: &str, name: &str, head: &[&str]) -> io::Result<Table> {
        let mut w = BufWriter::new(File::create(format!("{}/{}", dir, name))?);
        writeln!(w, "{}", head.join("\t"))?;
        Ok(Table { w })
    }
    fn row(&mut self, cells: &[String]) -> io::Result<()> {
        writeln!(self.w, "{}", cells.join("\t"))
    }
}

/// Shorthands for a row's cells.
fn s(x: impl ToString) -> String {
    x.to_string()
}

fn f(x: f64) -> String {
    fmt(x)
}

fn share(v: &[f64], p: impl Fn(f64) -> bool) -> f64 {
    if v.is_empty() {
        f64::NAN
    } else {
        v.iter().filter(|&&x| p(x)).count() as f64 / v.len() as f64
    }
}

/// The value of a stratum's mask.
fn masked(v: &[f64], m: &[bool]) -> Vec<f64> {
    v.iter().zip(m).filter(|x| *x.1).map(|x| *x.0).collect()
}

// ------------------------------------------------------------------ tables

struct Tables {
    groups: Table,
    sources: Table,
    quantiles: Table,
    errors: Table,
    jseeds: Table,
    jbins: Table,
    cont: Table,
    jw_strata: Table,
    jw_bins: Table,
    agree: Table,
    seed_pairs: Table,
    l1p: Table,
    support: Table,
    cont_rows: Table,
    spearman: Table,
}

impl Tables {
    fn create(out: &str) -> io::Result<Tables> {
        let q: Vec<String> = QS.iter().map(|p| format!("p{}", (p * 100.0).round())).collect();
        let mut qh: Vec<&str> = vec!["group", "stratum", "measure", "label", "n", "undefined", "mean"];
        qh.extend(q.iter().map(|x| x.as_str()));
        qh.extend(["eq0", "eq1", "ge0.9", "ge0.999", "below_floor"]);
        Ok(Tables {
            groups: Table::create(
                out,
                "groups.tsv",
                &[
                    "group", "part", "rows", "pairs", "rec_min", "rec_max", "block_min", "block_max", "n_source", "n_min", "n_median",
                    "n_max", "n_distinct", "log10_n_min", "log10_n_max",
                ],
            )?,
            sources: Table::create(out, "sources.tsv", &["group", "part", "label", "kind", "family", "seed", "dir", "from"])?,
            quantiles: Table::create(out, "quantiles.tsv", &qh)?,
            errors: Table::create(
                out,
                "errors.tsv",
                &[
                    "group", "label", "seeds", "truth", "stratum", "n", "mean", "rms", "p1", "p99", "max_abs", "gt0.05", "gt2sd", "gt2se",
                    "se_pos", "gt2se_pos", "whole", "whole_equal", "whole_union_gt_k",
                ],
            )?,
            jseeds: Table::create(
                out,
                "jaccard-seeds.tsv",
                &[
                    "group", "family", "k", "seeds", "stratum", "pairs", "bias", "seed_se", "t", "t_crit", "bias_ok", "pooled_ratio",
                    "ratio2", "ratio2_se", "seed_ratio_lo", "seed_ratio_hi", "frac_gt_2sd",
                ],
            )?,
            jbins: Table::create(
                out,
                "jaccard-bins.tsv",
                &[
                    "group", "family", "k", "seeds", "bin", "lo", "hi", "pairs", "x", "bias", "se", "t", "t_crit", "rms", "rms_se",
                    "theory", "ratio",
                ],
            )?,
            cont: Table::create(
                out,
                "containment.tsv",
                &["group", "family", "k", "seeds", "side", "stratum", "pairs", "defined", "bias", "se", "t", "t_crit", "rms"],
            )?,
            jw_strata: Table::create(
                out,
                "jw-strata.tsv",
                &["group", "family", "m", "seeds", "stratum", "pairs", "bias", "se", "t", "rmse", "mean_est", "floor", "below_floor"],
            )?,
            jw_bins: Table::create(
                out,
                "jw-bins.tsv",
                &["group", "family", "seeds", "bin", "lo", "hi", "pairs", "x", "mean_est", "se", "rms"],
            )?,
            agree: Table::create(
                out,
                "seed-agreement.tsv",
                &["group", "family", "k", "seeds", "subset", "cells", "share_s_le_sqrt3_sigma", "expected", "mean_s2_over_sigma2"],
            )?,
            seed_pairs: Table::create(
                out,
                "seed-pairs.tsv",
                &["group", "family", "seed_a", "seed_b", "gated", "sigma_pos", "mean_d2_gated", "mean_d2_all", "mean_d_gated"],
            )?,
            l1p: Table::create(
                out,
                "l1-precision.tsv",
                &["group", "f32", "f64", "pairs", "max_rel_tv", "p99_rel_tv", "max_abs_tv", "max_abs_jw", "f64_zero", "both_zero"],
            )?,
            support: Table::create(out, "support.tsv", &["group", "a", "b", "pairs", "share_equal", "max_abs_diff"])?,
            cont_rows: Table::create(
                out,
                "cont-rows.tsv",
                &[
                    "group", "label", "row", "rec", "block", "n_hat", "newer", "defined", "undefined", "not_nested", "median_cont",
                    "undefined_newer_block_min", "undefined_fb_max",
                ],
            )?,
            spearman: Table::create(out, "spearman.tsv", &["group", "x", "y", "n", "rho"])?,
        })
    }

    fn flush(&mut self) -> io::Result<()> {
        for t in [
            &mut self.groups,
            &mut self.sources,
            &mut self.quantiles,
            &mut self.errors,
            &mut self.jseeds,
            &mut self.jbins,
            &mut self.cont,
            &mut self.jw_strata,
            &mut self.jw_bins,
            &mut self.agree,
            &mut self.seed_pairs,
            &mut self.l1p,
            &mut self.support,
            &mut self.cont_rows,
            &mut self.spearman,
        ] {
            t.w.flush()?;
        }
        Ok(())
    }
}

fn groups_and_sources(g: &Group, t: &mut Tables) -> io::Result<()> {
    for p in &g.parts {
        let mut sources: Vec<(String, Vec<f64>)> = Vec::new();
        if let Some(v) = p.rowcols.get("X_n") {
            sources.push(("X_n".into(), v.clone()));
        }
        let mut seen = Vec::new();
        for src in &p.srcs {
            let fam = metrics::family(&src.label);
            if matches!(src.kind, Kind::Kmv { .. }) && !seen.contains(&fam) {
                if let Some(v) = p.rowcols.get(&format!("{}:n", src.label)) {
                    sources.push((src.label.clone(), v.clone()));
                    seen.push(fam);
                }
            }
        }
        // read_rows refuses a group without rows; a count column may still
        // be all NA, and then its sizes are NA.
        let rec = ends(&sorted_finite(p.rec.iter().map(|&x| x as f64)));
        let blk = ends(&sorted_finite(p.block.iter().map(|&x| x as f64)));
        for (name, v) in sources {
            let sv = sorted_finite(v.into_iter());
            let (lo, hi) = ends(&sv);
            let mut distinct = sv.clone();
            distinct.dedup();
            t.groups.row(&[
                s(&g.name),
                s(&p.name),
                s(p.n),
                s(p.npairs()),
                f(rec.0),
                f(rec.1),
                f(blk.0),
                f(blk.1),
                name,
                f(lo),
                f(quantile_linear(&sv, 0.5)),
                f(hi),
                s(distinct.len()),
                f(lo.log10()),
                f(hi.log10()),
            ])?;
        }
        for src in &p.srcs {
            t.sources.row(&[
                s(&g.name),
                s(&p.name),
                s(&src.label),
                s(src.kind.name()),
                metrics::family(&src.label),
                s(src.kind.seed()),
                s(&src.dir),
                s(if src.tsv { "tsv" } else { "f32 matrix" }),
            ])?;
        }
        if let Some((dir, tsv)) = &p.exact {
            t.sources.row(&[
                s(&g.name),
                s(&p.name),
                s("X"),
                s("exact"),
                s("X"),
                s(""),
                s(dir),
                s(if *tsv { "tsv" } else { "f32 matrix" }),
            ])?;
        }
    }
    Ok(())
}

/// One `quantiles.tsv` row.
fn quantile_row(t: &mut Tables, g: &str, stratum: &str, measure: &str, label: &str, v: &[f64], floor: Option<f64>) -> io::Result<()> {
    let sv = sorted_finite(v.iter().copied());
    let mut row = vec![s(g), s(stratum), s(measure), s(label), s(sv.len()), s(v.len() - sv.len()), f(mean(&sv))];
    row.extend(QS.iter().map(|&p| f(quantile_linear(&sv, p))));
    row.push(f(share(&sv, |x| x == 0.0)));
    row.push(f(share(&sv, |x| x == 1.0)));
    row.push(f(share(&sv, |x| x >= 0.9)));
    row.push(f(share(&sv, |x| x >= 0.999)));
    row.push(match floor {
        Some(fl) => f(share(&sv, |x| x < fl)),
        None => s("NA"),
    });
    t.quantiles.row(&row)
}

fn log10_1m(v: &[f64], below: f64) -> Vec<f64> {
    v.iter().filter(|&&c| c < below).map(|&c| (1.0 - c).log10()).collect()
}

fn quantiles(g: &Group, t: &mut Tables) -> io::Result<()> {
    // (measure, label, values, floor)
    let mut ms: Vec<(String, String, Vec<f64>, Option<f64>)> = Vec::new();
    if g.has_exact() {
        if let Some(j) = g.col("X_J")? {
            ms.push(("J".into(), "X".into(), j, None));
        }
        if let Some(c) = g.old_in_new("X_")? {
            ms.push(("log10(1-cont)|cont<1".into(), "X".into(), log10_1m(&c, 1.0), None));
            ms.push(("log10(1-cont)|cont<0.999".into(), "X".into(), log10_1m(&c, 0.999), None));
            ms.push(("cont_old_in_new".into(), "X".into(), c, None));
        }
        if let Some(r) = g.n_ratio("X_n") {
            ms.push(("n_min/n_max".into(), "X".into(), r, None));
        }
        if let Some(w) = g.col("X_jw")? {
            ms.push(("jw".into(), "X".into(), w, None));
        }
    }
    for (_, srcs) in g.families() {
        let src = &srcs[0];
        let l = &src.label;
        match src.kind {
            Kind::Kmv { .. } => {
                if let Some(j) = g.col(&format!("{}:J", l))? {
                    ms.push(("J".into(), l.clone(), j, None));
                }
                if let Some(c) = g.old_in_new(&format!("{}:", l))? {
                    ms.push(("log10(1-cont)|cont<1".into(), l.clone(), log10_1m(&c, 1.0), None));
                    ms.push(("log10(1-cont)|cont<0.999".into(), l.clone(), log10_1m(&c, 0.999), None));
                    ms.push(("cont_old_in_new".into(), l.clone(), c, None));
                }
                if let Some(r) = g.n_ratio(&format!("{}:n", l)) {
                    ms.push(("n_min/n_max".into(), l.clone(), r, None));
                }
            }
            Kind::L1 { m, .. } => {
                if let Some(w) = g.col(&format!("{}:jw", l))? {
                    ms.push(("jw".into(), l.clone(), w, Some(jw_floor(m))));
                }
            }
            Kind::Varopt { .. } => {
                if let Some(w) = g.col(&format!("{}:w1", l))? {
                    ms.push(("w1_years".into(), l.clone(), w.iter().map(|x| x * metrics::L / BLOCKS_PER_YEAR).collect(), None));
                }
            }
        }
    }
    let gaps = g.gaps();
    for (measure, label, v, floor) in &ms {
        quantile_row(t, &g.name, "all", measure, label, v, *floor)?;
    }
    for &(lo, hi) in &GAPS {
        let m: Vec<bool> = gaps.iter().map(|&x| x >= lo && x < hi).collect();
        if !m.iter().any(|&x| x) {
            continue;
        }
        let name = format!("gap [{},{})", lo, if hi.is_finite() { hi.to_string() } else { "inf".into() });
        for (measure, label, v, floor) in &ms {
            if measure.starts_with("log10") {
                // Recompute over the stratum from the containment itself.
                let base = ms.iter().find(|x| x.0 == "cont_old_in_new" && &x.1 == label).map(|x| &x.2);
                if let Some(c) = base {
                    let below = if measure.ends_with("<1") { 1.0 } else { 0.999 };
                    quantile_row(t, &g.name, &name, measure, label, &log10_1m(&masked(c, &m), below), *floor)?;
                }
                continue;
            }
            quantile_row(t, &g.name, &name, measure, label, &masked(v, &m), *floor)?;
        }
    }
    Ok(())
}

/// The truth a label's estimate is compared with.
fn truth_of(kind: &Kind) -> Option<(&'static str, &'static str)> {
    match kind {
        Kind::Kmv { support, .. } if support == "sets" => Some(("J", "X_J")),
        Kind::Kmv { support, .. } if support == "pos" => Some(("J", "X_J_pos")),
        Kind::L1 { .. } => Some(("jw", "X_jw")),
        _ => None,
    }
}

/// `errors.tsv`: per label, and per family as the mean of its seeds.
fn errors(g: &Group, t: &mut Tables) -> io::Result<()> {
    if !g.has_exact() {
        return Ok(());
    }
    let hv = g.exact_union()?;
    for (fam, srcs) in g.families() {
        let Some((col, xname)) = truth_of(&srcs[0].kind) else { continue };
        let Some(x) = g.col(xname)? else { continue };
        let k = match srcs[0].kind {
            Kind::Kmv { k, .. } => Some(k),
            _ => None,
        };
        let mut strata: Vec<(String, Vec<bool>)> = vec![("all".into(), vec![true; x.len()])];
        if col == "jw" {
            strata.push(("X_jw>=0.05".into(), x.iter().map(|&v| v >= 0.05).collect()));
        }
        for (sname, mask) in &strata {
            // per seed: n, mean, rms, p1, p99, max_abs, gt0.05, gt2sd, gt2se, se_pos, gt2se_pos,
            // whole, whole_equal, whole_union_gt_k
            let mut rows: Vec<[f64; 14]> = Vec::new();
            for src in &srcs {
                let Some((_, err)) = g.est_err(&format!("{}:{}", src.label, col), &x)? else { continue };
                let se = if col == "J" { g.col(&format!("{}:se", src.label))? } else { None };
                let idx: Vec<usize> = (0..x.len()).filter(|&p| mask[p]).collect();
                let e: Vec<f64> = idx.iter().map(|&p| err[p]).collect();
                let sv = sorted_finite(e.iter().copied());
                let rms = (sv.iter().map(|v| v * v).sum::<f64>() / sv.len() as f64).sqrt();
                let gt2sd = match (&hv, k, xname) {
                    (Some((_, u, _, _)), Some(k), "X_J") => {
                        share(&idx.iter().map(|&p| err[p].abs() - 2.0 * hyper_var(x[p], u[p], k).sqrt()).collect::<Vec<_>>(), |d| d > 0.0)
                    }
                    _ => f64::NAN,
                };
                let (gt2se, se_pos, gt2se_pos) = match &se {
                    Some(se) => {
                        let off: Vec<bool> = idx.iter().map(|&p| err[p].abs() > 2.0 * se[p]).collect();
                        let pos: Vec<bool> = idx.iter().map(|&p| se[p] > 0.0).collect();
                        let npos = pos.iter().filter(|&&b| b).count();
                        let off_pos = off.iter().zip(&pos).filter(|(o, p)| **o && **p).count();
                        (
                            off.iter().filter(|&&b| b).count() as f64 / idx.len() as f64,
                            npos as f64 / idx.len() as f64,
                            off_pos as f64 / npos as f64,
                        )
                    }
                    None => (f64::NAN, f64::NAN, f64::NAN),
                };
                // Pairs of two whole sketches (both supports within k): the
                // estimate must be the exact J; and how many of them have a
                // union beyond k, where the textbook formula would estimate.
                let (whole, whole_eq, whole_big) = match (&hv, k, xname) {
                    (Some((_, u, ni, nj)), Some(k), "X_J") => {
                        let w: Vec<usize> = idx.iter().copied().filter(|&p| ni[p] <= k as f64 && nj[p] <= k as f64).collect();
                        (
                            w.len() as f64,
                            w.iter().filter(|&&p| err[p] == 0.0).count() as f64,
                            w.iter().filter(|&&p| u[p] > k as f64 + 0.5).count() as f64,
                        )
                    }
                    _ => (f64::NAN, f64::NAN, f64::NAN),
                };
                let r = [
                    sv.len() as f64,
                    mean(&sv),
                    rms,
                    quantile_linear(&sv, 0.01),
                    quantile_linear(&sv, 0.99),
                    sv.iter().fold(0.0f64, |a, v| a.max(v.abs())),
                    share(&sv, |v| v.abs() > 0.05),
                    gt2sd,
                    gt2se,
                    se_pos,
                    gt2se_pos,
                    whole,
                    whole_eq,
                    whole_big,
                ];
                let mut row = vec![s(&g.name), s(&src.label), s(1), s(xname), s(sname), s(r[0] as usize)];
                row.extend(r[1..11].iter().map(|&v| f(v)));
                row.extend(r[11..].iter().map(|&v| if v.is_nan() { s("NA") } else { s(v as usize) }));
                t.errors.row(&row)?;
                rows.push(r);
            }
            if rows.len() > 1 {
                let avg = |i: usize| mean(&defined(&rows.iter().map(|r| r[i]).collect::<Vec<_>>()));
                let mut row = vec![s(&g.name), s(&fam), s(rows.len()), s(xname), s(sname), s(rows[0][0] as usize)];
                row.extend([
                    f(avg(1)),
                    f(mean(&rows.iter().map(|r| r[2] * r[2]).collect::<Vec<_>>()).sqrt()),
                    s("NA"),
                    s("NA"),
                    f(rows.iter().fold(0.0f64, |a, r| a.max(r[5]))),
                    f(avg(6)),
                    f(avg(7)),
                    f(avg(8)),
                    f(avg(9)),
                    f(avg(10)),
                ]);
                // Whole pairs are the same in every seed; equal: the fewest over the seeds.
                row.extend([11, 12, 13].iter().map(|&i| {
                    let v = defined(&rows.iter().map(|r| r[i]).collect::<Vec<_>>());
                    if v.is_empty() {
                        s("NA")
                    } else {
                        s(v.iter().cloned().fold(f64::INFINITY, f64::min) as usize)
                    }
                }));
                t.errors.row(&row)?;
            }
        }
    }
    Ok(())
}

/// The KMV `sets` families with at least two seeds, with their `k`.
fn kmv_sets_families(g: &Group) -> Vec<(String, usize, Vec<Src>)> {
    g.families()
        .into_iter()
        .filter_map(|(fam, srcs)| match &srcs[0].kind {
            Kind::Kmv { k, support, .. } if support == "sets" && srcs.len() >= 2 => Some((fam, *k, srcs)),
            _ => None,
        })
        .collect()
}

/// The estimates of a family and their errors against `x`, one pair of
/// vectors per seed, leaving out the seeds that lack the column.
fn seed_errs(g: &Group, srcs: &[Src], col: &str, x: &[f64]) -> io::Result<Vec<(Vec<f64>, Vec<f64>)>> {
    let mut out = Vec::new();
    for s in srcs {
        if let Some(v) = g.est_err(&format!("{}:{}", s.label, col), x)? {
            out.push(v);
        }
    }
    Ok(out)
}

/// `jaccard-seeds.tsv` and `jaccard-bins.tsv`.
fn jaccard(g: &Group, t: &mut Tables) -> io::Result<()> {
    let Some((x, u, ni, nj)) = g.exact_union()? else { return Ok(()) };
    for (fam, k, srcs) in kmv_sets_families(g) {
        let est = seed_errs(g, &srcs, "J", &x)?;
        let sn = est.len();
        if sn < 2 {
            continue;
        }
        let var: Vec<f64> = (0..x.len()).map(|p| hyper_var(x[p], u[p], k)).collect();
        // Strata of the exact J.
        let strata: Vec<(String, Vec<bool>)> = J_STRATA
            .iter()
            .map(|&(lo, hi)| {
                let name = format!("({},{}{}", lo, hi, if hi == 1.0 { ")" } else { "]" });
                (name, x.iter().map(|&j| j > lo && j <= hi && j < 1.0).collect::<Vec<_>>())
            })
            .filter(|(_, m)| m.iter().any(|&b| b))
            .collect();
        let tc = t_crit(sn, strata.len());
        for (name, m) in &strata {
            let idx: Vec<usize> = (0..x.len()).filter(|&p| m[p]).collect();
            let th = mean(&idx.iter().map(|&p| var[p]).collect::<Vec<_>>()).sqrt();
            let mut mu = Vec::new();
            let mut rms = Vec::new();
            let mut off = 0usize;
            for (_, e) in &est {
                let err: Vec<f64> = idx.iter().map(|&p| e[p]).collect();
                mu.push(mean(&err));
                rms.push(mean(&err.iter().map(|v| v * v).collect::<Vec<_>>()).sqrt());
                off += idx.iter().filter(|&&p| e[p].abs() > 2.0 * var[p].sqrt()).count();
            }
            let (b, se, tt) = seed_test(&mu);
            let r2: Vec<f64> = rms.iter().map(|r| (r / th).powi(2)).collect();
            let pooled = mean(&r2).sqrt();
            let ratios: Vec<f64> = rms.iter().map(|r| r / th).collect();
            t.jseeds.row(&[
                s(&g.name),
                s(&fam),
                s(k),
                s(sn),
                s(name),
                s(idx.len()),
                f(b),
                f(se),
                f(tt),
                f(tc),
                s(tt.abs() < tc),
                f(pooled),
                f(pooled * pooled),
                f(sd1(&r2) / (sn as f64).sqrt()),
                f(ratios.iter().cloned().fold(f64::INFINITY, f64::min)),
                f(ratios.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
                f(off as f64 / (idx.len() * sn) as f64),
            ])?;
        }
        // Bins over the inexact pairs.
        let nb = J_BINS.len() - 1;
        let keep: Vec<bool> = (0..x.len()).map(|p| (ni[p] > k as f64 || nj[p] > k as f64) && x[p] > 0.0 && x[p] < 1.0).collect();
        let bin: Vec<usize> = x.iter().map(|&j| digitize(j, &J_BINS, true)).collect();
        let bins: Vec<Vec<usize>> = (0..nb).map(|bi| (0..x.len()).filter(|&p| keep[p] && bin[p] == bi).collect()).collect();
        let nonempty = bins.iter().filter(|b| !b.is_empty()).count();
        let tc = t_crit(sn, nonempty);
        for (bi, idx) in bins.iter().enumerate() {
            if idx.is_empty() {
                continue;
            }
            let mu: Vec<f64> = est.iter().map(|(_, e)| mean(&idx.iter().map(|&p| e[p]).collect::<Vec<_>>())).collect();
            let r2: Vec<f64> = est.iter().map(|(_, e)| mean(&idx.iter().map(|&p| e[p].powi(2)).collect::<Vec<_>>())).collect();
            let (b, se, tt) = seed_test(&mu);
            let rms = mean(&r2).sqrt();
            let theory = mean(&idx.iter().map(|&p| var[p]).collect::<Vec<_>>()).sqrt();
            t.jbins.row(&[
                s(&g.name),
                s(&fam),
                s(k),
                s(sn),
                s(bi),
                f(J_BINS[bi]),
                f(J_BINS[bi + 1]),
                s(idx.len()),
                f(mean(&idx.iter().map(|&p| x[p]).collect::<Vec<_>>())),
                f(b),
                f(se),
                f(tt),
                f(tc),
                f(rms),
                f(sd1(&r2) / (sn as f64).sqrt() / (2.0 * rms)),
                f(theory),
                f(rms / theory),
            ])?;
        }
    }
    Ok(())
}

fn containment(g: &Group, t: &mut Tables) -> io::Result<()> {
    if !g.has_exact() {
        return Ok(());
    }
    for (fam, k, srcs) in kmv_sets_families(g) {
        for side in ["cont_ij", "cont_ji"] {
            let Some(x) = g.col(&format!("X_{}", side))? else { continue };
            let est = seed_errs(g, &srcs, side, &x)?;
            let sn = est.len();
            if sn < 2 {
                continue;
            }
            let def = mean(&est.iter().map(|(e, _)| share(e, |v| !v.is_nan())).collect::<Vec<_>>());
            let strata: Vec<(&str, Vec<bool>)> = C_STRATA
                .iter()
                .map(|&(name, lo, hi)| {
                    (name, x.iter().map(|&c| if name == "{1}" { c == 1.0 } else { c >= lo && c < hi }).collect::<Vec<_>>())
                })
                .filter(|(_, m)| m.iter().any(|&b| b))
                .collect();
            let tc = t_crit(sn, strata.len());
            for (name, m) in &strata {
                let mut mu = Vec::new();
                let mut ms = Vec::new();
                for (_, e) in &est {
                    let err: Vec<f64> = (0..x.len()).filter(|&p| m[p]).map(|p| e[p]).filter(|v| !v.is_nan()).collect();
                    mu.push(mean(&err));
                    ms.push(mean(&err.iter().map(|v| v * v).collect::<Vec<_>>()));
                }
                let (b, se, tt) = seed_test(&mu);
                t.cont.row(&[
                    s(&g.name),
                    s(&fam),
                    s(k),
                    s(sn),
                    s(side),
                    s(name),
                    s(m.iter().filter(|&&b| b).count()),
                    f(def),
                    f(b),
                    f(se),
                    f(tt),
                    f(tc),
                    f(mean(&defined(&ms)).sqrt()),
                ])?;
            }
        }
    }
    Ok(())
}

fn weighted(g: &Group, t: &mut Tables) -> io::Result<()> {
    if !g.has_exact() {
        return Ok(());
    }
    let Some(x) = g.col("X_jw")? else { return Ok(()) };
    let bin: Vec<usize> = x.iter().map(|&v| digitize(v, &JW_BINS, false)).collect();
    let nb = JW_BINS.len() - 1;
    for (fam, srcs) in g.families() {
        let (m, est, col) = match srcs[0].kind {
            Kind::L1 { m, .. } => (Some(m), seed_errs(g, &srcs, "jw", &x)?, "jw"),
            Kind::Varopt { .. } => (None, seed_errs(g, &srcs[..1], "jw_plugin", &x)?, "jw_plugin"),
            _ => continue,
        };
        if est.is_empty() {
            continue;
        }
        let sn = est.len();
        let label = if col == "jw_plugin" { format!("{}:jw_plugin", srcs[0].label) } else { fam.clone() };
        for &(name, lo, hi) in &JW_STRATA {
            let idx: Vec<usize> = (0..x.len()).filter(|&p| x[p] >= lo && x[p] < hi).collect();
            if idx.is_empty() {
                continue;
            }
            let mut mu = Vec::new();
            let mut ms = Vec::new();
            let mut me = Vec::new();
            let mut below = Vec::new();
            for (e, d) in &est {
                let err = defined(&idx.iter().map(|&p| d[p]).collect::<Vec<_>>());
                mu.push(mean(&err));
                ms.push(mean(&err.iter().map(|v| v * v).collect::<Vec<_>>()));
                let v = defined(&idx.iter().map(|&p| e[p]).collect::<Vec<_>>());
                me.push(mean(&v));
                if let Some(m) = m {
                    below.push(share(&v, |y| y < jw_floor(m)));
                }
            }
            let (b, se, tt) = seed_test(&mu);
            t.jw_strata.row(&[
                s(&g.name),
                s(&label),
                m.map_or("NA".into(), s),
                s(sn),
                s(name),
                s(idx.len()),
                f(b),
                f(se),
                f(tt),
                f(mean(&ms).sqrt()),
                f(mean(&me)),
                m.map_or("NA".into(), |m| f(jw_floor(m))),
                f(mean(&below)),
            ])?;
        }
        for bi in 0..nb {
            let idx: Vec<usize> = (0..x.len()).filter(|&p| bin[p] == bi).collect();
            if idx.is_empty() {
                continue;
            }
            let me: Vec<f64> = est.iter().map(|(e, _)| mean(&defined(&idx.iter().map(|&p| e[p]).collect::<Vec<_>>()))).collect();
            let ms: Vec<f64> = est.iter().map(|(_, d)| mean(&defined(&idx.iter().map(|&p| d[p].powi(2)).collect::<Vec<_>>()))).collect();
            t.jw_bins.row(&[
                s(&g.name),
                s(&label),
                s(sn),
                s(bi),
                f(JW_BINS[bi]),
                f(JW_BINS[bi + 1].min(1.0)),
                s(idx.len()),
                f(mean(&idx.iter().map(|&p| x[p]).collect::<Vec<_>>())),
                f(mean(&me)),
                f(sd1(&me) / (sn as f64).sqrt()),
                f(mean(&ms).sqrt()),
            ])?;
        }
    }
    Ok(())
}

/// `seed-agreement.tsv` and `seed-pairs.tsv`: no exact values needed.
fn agreement(g: &Group, t: &mut Tables) -> io::Result<()> {
    for (fam, srcs) in g.families() {
        let Kind::Kmv { k, .. } = srcs[0].kind else { continue };
        let mut used = Vec::new();
        let (mut js, mut us, mut ex) = (Vec::new(), Vec::new(), Vec::new());
        for src in &srcs {
            let l = &src.label;
            if let (Some(j), Some(u), Some(e)) =
                (g.col(&format!("{}:J", l))?, g.col(&format!("{}:n_union", l))?, g.col(&format!("{}:exact", l))?)
            {
                used.push(src.kind.seed());
                js.push(j);
                us.push(u);
                ex.push(e);
            }
        }
        let sn = js.len();
        if sn < 2 {
            continue;
        }
        let kf = k as f64;
        let np = js[0].len();
        let all_exact: Vec<bool> = (0..np).map(|p| ex.iter().all(|e| e[p] == 1.0)).collect();
        let mixed = (0..np).filter(|&p| !all_exact[p] && ex.iter().any(|e| e[p] == 1.0)).count();
        let ident = (0..np).filter(|&p| all_exact[p]).all(|p| js.iter().all(|j| j[p] == js[0][p]));
        let cells: Vec<usize> = (0..np).filter(|&p| !all_exact[p]).collect();
        let jb: Vec<f64> = cells.iter().map(|&p| js.iter().map(|j| j[p]).sum::<f64>() / sn as f64).collect();
        let ub: Vec<f64> = cells.iter().map(|&p| us.iter().map(|u| u[p]).sum::<f64>() / sn as f64).collect();
        let sig: Vec<f64> = jb.iter().zip(&ub).map(|(&j, &u)| hyper_var(j, u, k).sqrt()).collect();
        let sd: Vec<f64> = cells.iter().map(|&p| sd1(&js.iter().map(|j| j[p]).collect::<Vec<_>>())).collect();
        let agree: Vec<bool> = sd.iter().zip(&sig).map(|(&s, &g)| s <= 3f64.sqrt() * g * (1.0 + 1e-9) + 1e-15).collect();
        let pos: Vec<bool> = sig.iter().map(|&g| g > 0.0).collect();
        let gated: Vec<bool> = (0..cells.len()).map(|c| pos[c] && kf * jb[c] >= 10.0 && kf * (1.0 - jb[c]) >= 10.0).collect();
        let r2: Vec<f64> = (0..cells.len()).map(|c| if pos[c] { (sd[c] / sig[c]).powi(2) } else { f64::NAN }).collect();
        let expected = inc_gamma((sn - 1) as f64 / 2.0, 3.0 * (sn - 1) as f64 / 2.0);
        t_row_exact(t, &g.name, &fam, k, sn, all_exact.iter().filter(|&&b| b).count(), ident, mixed)?;
        let mut emit = |subset: String, m: &[bool], with_r2: bool| -> io::Result<()> {
            let c = m.iter().filter(|&&b| b).count();
            let a = (0..m.len()).filter(|&i| m[i] && agree[i]).count();
            let r = if with_r2 { mean(&(0..m.len()).filter(|&i| m[i]).map(|i| r2[i]).collect::<Vec<_>>()) } else { f64::NAN };
            t.agree.row(&[
                s(&g.name),
                s(&fam),
                s(k),
                s(sn),
                subset,
                s(c),
                f(if c > 0 { a as f64 / c as f64 } else { f64::NAN }),
                f(expected),
                f(r),
            ])
        };
        let zero: Vec<bool> = pos.iter().map(|&b| !b).collect();
        emit("sigma=0".into(), &zero, false)?;
        emit("sigma>0".into(), &pos, true)?;
        emit("gated".into(), &gated, true)?;
        for &(lo, hi) in &JBAR_STRATA {
            let m: Vec<bool> = (0..cells.len()).map(|c| pos[c] && jb[c] >= lo && jb[c] < hi).collect();
            if m.iter().any(|&b| b) {
                emit(format!("sigma>0, Jbar [{},{})", lo, hi.min(1.0)), &m, true)?;
            }
        }
        let sig1: Vec<f64> = (0..cells.len()).map(|c| if pos[c] { sig[c] } else { 1.0 }).collect();
        let ngated = gated.iter().filter(|&&b| b).count();
        let msig2 = mean(&(0..cells.len()).filter(|&c| gated[c]).map(|c| sig[c] * sig[c]).collect::<Vec<_>>());
        for a in 0..sn {
            for b in a + 1..sn {
                let d: Vec<f64> = cells.iter().map(|&p| js[a][p] - js[b][p]).collect();
                let d2: Vec<f64> = (0..cells.len()).map(|c| d[c] * d[c] / (2.0 * sig1[c] * sig1[c])).collect();
                let pick = |m: &[bool], v: &[f64]| mean(&(0..m.len()).filter(|&c| m[c]).map(|c| v[c]).collect::<Vec<_>>());
                t.seed_pairs.row(&[
                    s(&g.name),
                    s(&fam),
                    s(used[a]),
                    s(used[b]),
                    s(ngated),
                    s(pos.iter().filter(|&&b| b).count()),
                    f(pick(&gated, &d2)),
                    f(pick(&pos, &d2)),
                    f(pick(&gated, &d) / (2.0 * msig2).sqrt()),
                ])?;
            }
        }
    }
    Ok(())
}

/// The `exact` row of `seed-agreement.tsv`: cells exact in every seed, and
/// whether their estimates are identical across seeds (1) or not (0).
#[allow(clippy::too_many_arguments)]
fn t_row_exact(t: &mut Tables, g: &str, fam: &str, k: usize, sn: usize, cells: usize, ident: bool, mixed: usize) -> io::Result<()> {
    t.agree.row(&[s(g), s(fam), s(k), s(sn), s("exact in every seed (share: identical)"), s(cells), s(ident as u8), s("1"), s("NA")])?;
    t.agree.row(&[s(g), s(fam), s(k), s(sn), s("exact in some seeds only (must be 0)"), s(mixed), s("NA"), s("NA"), s("NA")])
}

fn precision(g: &Group, t: &mut Tables) -> io::Result<()> {
    let srcs = g.srcs();
    for a in &srcs {
        let Kind::L1 { m, seed, prec } = &a.kind else { continue };
        if prec != "f32" {
            continue;
        }
        let twin = |b: &&Src| matches!(&b.kind, Kind::L1 { m: m2, seed: s2, prec: p2 } if m2 == m && s2 == seed && p2 == "f64");
        let Some(b) = srcs.iter().find(twin)
        else {
            continue;
        };
        let (Some(x), Some(y)) = (g.col(&format!("{}:tv", a.label))?, g.col(&format!("{}:tv", b.label))?) else { continue };
        let (wx, wy) = (g.col(&format!("{}:jw", a.label))?.unwrap_or_default(), g.col(&format!("{}:jw", b.label))?.unwrap_or_default());
        let ok: Vec<usize> = (0..y.len()).filter(|&p| y[p] > 0.0).collect();
        let rel = sorted_finite(ok.iter().map(|&p| (x[p] - y[p]).abs() / y[p]));
        let maxabs = |u: &[f64], v: &[f64]| u.iter().zip(v).fold(0.0f64, |acc, (a, b)| acc.max((a - b).abs()));
        t.l1p.row(&[
            s(&g.name),
            s(&a.label),
            s(&b.label),
            s(ok.len()),
            f(*rel.last().unwrap_or(&f64::NAN)),
            f(quantile_linear(&rel, 0.99)),
            f(maxabs(&x, &y)),
            f(if wx.is_empty() { f64::NAN } else { maxabs(&wx, &wy) }),
            s(y.len() - ok.len()),
            s((0..y.len()).filter(|&p| (y[p] <= 0.0 || y[p].is_nan()) && x[p] == 0.0).count()),
        ])?;
    }
    Ok(())
}

fn support(g: &Group, t: &mut Tables) -> io::Result<()> {
    let srcs = g.srcs();
    let mut cmp = |an: &str, bn: &str, a: Vec<f64>, b: Vec<f64>| -> io::Result<()> {
        let eq = a.iter().zip(&b).filter(|(x, y)| x == y).count();
        let d = a.iter().zip(&b).fold(0.0f64, |acc, (x, y)| acc.max((x - y).abs()));
        t.support.row(&[s(&g.name), s(an), s(bn), s(a.len()), f(eq as f64 / a.len() as f64), f(d)])
    };
    for a in &srcs {
        let Kind::Kmv { k, seed, support } = &a.kind else { continue };
        if support != "pos" {
            continue;
        }
        let twin =
            srcs.iter().find(|b| matches!(&b.kind, Kind::Kmv { k: k2, seed: s2, support: p2 } if k2 == k && s2 == seed && p2 == "sets"));
        if let Some(b) = twin {
            if let (Some(x), Some(y)) = (g.col(&format!("{}:J", a.label))?, g.col(&format!("{}:J", b.label))?) {
                cmp(&a.label, &b.label, x, y)?;
            }
        }
    }
    if g.has_exact() {
        if let (Some(x), Some(y)) = (g.col("X_J_pos")?, g.col("X_J")?) {
            cmp("X_J_pos", "X_J", x, y)?;
        }
    }
    Ok(())
}

fn cont_rows(g: &Group, t: &mut Tables) -> io::Result<()> {
    if g.parts.len() != 1 {
        return Ok(());
    }
    let p = &g.parts[0];
    let n = p.n;
    let idx = |a: usize, b: usize| a * n - a * (a + 1) / 2 + (b - a - 1);
    for (_, srcs) in g.families() {
        let src = &srcs[0];
        if !matches!(src.kind, Kind::Kmv { .. }) {
            continue;
        }
        let l = &src.label;
        let (Some(ij), Some(ji)) = (p.col(&format!("{}:cont_ij", l))?, p.col(&format!("{}:cont_ji", l))?) else { continue };
        let fb = (p.col(&format!("{}:cont_ij_fb", l))?, p.col(&format!("{}:cont_ji_fb", l))?);
        let nh = p.rowcols.get(&format!("{}:n", l));
        for r in 0..n {
            let (mut newer, mut undef, mut not_nested) = (0usize, 0usize, 0usize);
            let mut vals = Vec::new();
            let mut ublock = u32::MAX;
            let mut fbmax = f64::NEG_INFINITY;
            for q in 0..n {
                if q == r || p.rec[q] <= p.rec[r] {
                    continue;
                }
                newer += 1;
                // The containment of r in q, and its fallback.
                let (c, fbv) = if r < q {
                    (ij[idx(r, q)], fb.0.as_ref().map(|v| v[idx(r, q)]))
                } else {
                    (ji[idx(q, r)], fb.1.as_ref().map(|v| v[idx(q, r)]))
                };
                if c.is_nan() {
                    undef += 1;
                    ublock = ublock.min(p.block[q]);
                    if let Some(v) = fbv {
                        fbmax = fbmax.max(v);
                    }
                } else {
                    vals.push(c);
                    if c < 0.999 {
                        not_nested += 1;
                    }
                }
            }
            let sv = sorted_finite(vals.iter().copied());
            t.cont_rows.row(&[
                s(&g.name),
                s(l),
                s(r),
                s(p.rec[r]),
                s(p.block[r]),
                nh.map_or("NA".into(), |v| f(v[r])),
                s(newer),
                s(sv.len()),
                s(undef),
                s(not_nested),
                f(quantile_linear(&sv, 0.5)),
                if undef > 0 { s(ublock) } else { s("NA") },
                if undef > 0 && fb.0.is_some() { f(fbmax) } else { s("NA") },
            ])?;
        }
    }
    Ok(())
}

fn correlations(g: &Group, t: &mut Tables) -> io::Result<()> {
    let gaps = g.gaps();
    let fams = g.families();
    let jhat: Vec<(String, Vec<f64>)> = {
        let mut v = Vec::new();
        for (_, srcs) in &fams {
            if matches!(srcs[0].kind, Kind::Kmv { .. }) {
                if let Some(j) = g.col(&format!("{}:J", srcs[0].label))? {
                    v.push((srcs[0].label.clone(), j.iter().map(|x| 1.0 - x).collect()));
                }
            }
        }
        v
    };
    for (l, d) in &jhat {
        let (n, rho) = spearman(d, &gaps);
        t.spearman.row(&[s(&g.name), format!("1-J:{}", l), s("gap"), s(n), f(rho)])?;
    }
    for (_, srcs) in &fams {
        if !matches!(srcs[0].kind, Kind::Varopt { .. }) {
            continue;
        }
        let l = &srcs[0].label;
        let Some(w) = g.col(&format!("{}:w1", l))? else { continue };
        let (n, rho) = spearman(&w, &gaps);
        t.spearman.row(&[s(&g.name), format!("w1:{}", l), s("gap"), s(n), f(rho)])?;
        for (jl, d) in &jhat {
            let (n, rho) = spearman(&w, d);
            t.spearman.row(&[s(&g.name), format!("w1:{}", l), format!("1-J:{}", jl), s(n), f(rho)])?;
        }
    }
    Ok(())
}

/// The groups of the first `--in`, from its `rows.<g>.tsv`, in name order.
fn default_groups(dir: &str) -> io::Result<Vec<String>> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", dir, e)))?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_prefix("rows.").and_then(|r| r.strip_suffix(".tsv")).map(String::from))
        .collect();
    out.sort();
    if out.is_empty() {
        return Err(bad(format!("{}: no rows.<group>.tsv; is it a pairs output directory?", dir)));
    }
    Ok(out)
}

pub fn run(opts: StatsOpts) -> io::Result<()> {
    if opts.inputs.is_empty() {
        return Err(bad("pairs-stats needs --in <pairs dir>".into()));
    }
    let groups = if opts.groups.is_empty() { default_groups(&opts.inputs[0])? } else { opts.groups.clone() };
    std::fs::create_dir_all(&opts.out)?;
    let mut t = Tables::create(&opts.out)?;
    for spec in &groups {
        let mut parts = Vec::new();
        for name in spec.split('+') {
            match Part::load(name, &opts.inputs)? {
                Some(p) => parts.push(p),
                None => return Err(bad(format!("no rows.{}.tsv in any --in", name))),
            }
        }
        let g = Group { name: spec.clone(), parts };
        let t0 = std::time::Instant::now();
        groups_and_sources(&g, &mut t)?;
        quantiles(&g, &mut t)?;
        errors(&g, &mut t)?;
        jaccard(&g, &mut t)?;
        containment(&g, &mut t)?;
        weighted(&g, &mut t)?;
        agreement(&g, &mut t)?;
        precision(&g, &mut t)?;
        support(&g, &mut t)?;
        cont_rows(&g, &mut t)?;
        correlations(&g, &mut t)?;
        eprintln!(
            "sketch pairs-stats: group {}: {} rows, {} pairs, {} labels, exact {}, {:.1} s",
            g.name,
            g.parts.iter().map(|p| p.n).sum::<usize>(),
            g.npairs(),
            g.srcs().len(),
            if g.has_exact() { "yes" } else { "no" },
            t0.elapsed().as_secs_f64()
        );
    }
    t.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() <= tol * (1.0 + a.abs() + b.abs())
    }

    #[test]
    fn quantiles_interpolate_as_numpy() {
        // np.quantile([1, 2, 3, 4], [0, .1, .5, .99, 1]) = 1, 1.3, 2.5, 3.97, 4
        let v = [1.0, 2.0, 3.0, 4.0];
        for (p, want) in [(0.0, 1.0), (0.1, 1.3), (0.5, 2.5), (0.99, 3.97), (1.0, 4.0)] {
            assert!(close(quantile_linear(&v, p), want, 1e-12), "p {}", p);
        }
        assert!(quantile_linear(&[], 0.5).is_nan());
        assert_eq!(quantile_linear(&[7.0], 0.9), 7.0);
    }

    #[test]
    fn spearman_ranks_ties_at_their_mean() {
        assert_eq!(ranks(&[3.0, 1.0, 3.0, 2.0]), vec![3.5, 1.0, 3.5, 2.0]);
        // scipy.stats.spearmanr([1, 2, 2, 3], [1, 3, 2, 4]).correlation = 0.9486832980505138
        let (n, r) = spearman(&[1.0, 2.0, 2.0, 3.0], &[1.0, 3.0, 2.0, 4.0]);
        assert_eq!(n, 4);
        assert!(close(r, 0.948_683_298_050_513_8, 1e-12), "{}", r);
        // NaN pairs are left out; a monotone map gives 1.
        let (n, r) = spearman(&[1.0, f64::NAN, 3.0, 4.0], &[10.0, 0.0, 30.0, 400.0]);
        assert_eq!(n, 3);
        assert!(close(r, 1.0, 1e-12));
    }

    #[test]
    fn digitize_matches_numpy() {
        // np.digitize(J, JBINS[1:-1], right=True): 0.01 is in bin 0, 0.0100001 in bin 1.
        assert_eq!(digitize(0.01, &J_BINS, true), 0);
        assert_eq!(digitize(0.010_000_1, &J_BINS, true), 1);
        assert_eq!(digitize(0.99, &J_BINS, true), 12);
        // right=False: 1e-3 starts bin 1, and 0.7 the last.
        assert_eq!(digitize(0.0, &JW_BINS, false), 0);
        assert_eq!(digitize(1e-3, &JW_BINS, false), 1);
        assert_eq!(digitize(0.7, &JW_BINS, false), 10);
        assert_eq!(digitize(1.0, &JW_BINS, false), 10);
    }

    #[test]
    fn seed_tests_and_thresholds() {
        let (b, se, t) = seed_test(&[1.0, 2.0, 3.0, f64::NAN]);
        assert!(close(b, 2.0, 1e-15) && close(se, 1.0 / 3f64.sqrt(), 1e-12) && close(t, 2.0 * 3f64.sqrt(), 1e-12));
        assert_eq!(seed_test(&[0.0, 0.0]).2, 0.0);
        assert_eq!(seed_test(&[1.0, 1.0]).2, f64::INFINITY);
        // scipy.stats.t.ppf(1 - 0.01 / 10, 15) = 3.7328..., the R3b deep threshold at 16 seeds.
        assert!(close(t_crit(16, 5), 3.732_835, 1e-5), "{}", t_crit(16, 5));
        // The three-seed share of s <= sqrt(3) sigma for independent normal draws.
        assert!(close(inc_gamma(1.0, 3.0), 1.0 - (-3.0f64).exp(), 1e-12));
    }

    #[test]
    fn hypergeometric_variance_and_floor() {
        // A union within k is exact; otherwise the finite-population factor.
        assert_eq!(hyper_var(0.5, 100.0, 1024), 0.0);
        assert!(close(hyper_var(0.5, 2049.0, 1024), 0.25 / 1024.0 * 1025.0 / 2048.0, 1e-15));
        assert!(close(jw_floor(1024), 0.008_815, 1e-3) && close(jw_floor(512), 0.012_47, 1e-3) && close(jw_floor(256), 0.017_63, 1e-3));
    }

    #[test]
    fn f32_exact_values_snap_to_their_rationals() {
        let p = Part {
            name: "g".into(),
            n: 3,
            rec: vec![1, 2, 3],
            block: vec![0, 0, 0],
            rowcols: HashMap::new(),
            srcs: Vec::new(),
            exact: None,
            cols: HashMap::new(),
        };
        // Nested supports of 1, 20 and 7 blocks: J = 1/20, 1/7, 7/20.
        let xn = [1.0, 20.0, 7.0];
        let j32: Vec<f64> = [1.0 / 20.0, 1.0 / 7.0, 7.0 / 20.0].iter().map(|&x: &f64| x as f32 as f64).collect();
        assert!(j32[0] > 0.05, "f32 puts J = 0.05 above the stratum edge");
        let (i, u) = p.snap(&j32, &xn);
        assert_eq!(i, vec![1.0, 1.0, 7.0]);
        assert_eq!(u, vec![20.0, 7.0, 20.0]);
        assert_eq!(i[0] / u[0], 0.05);
        // A value that is no ratio of these counts is left alone (NaN: keep the f32).
        let (i, _) = p.snap(&[0.3, 1.0 / 7.0, 0.35], &xn);
        assert!(i[0].is_nan() && i[1] == 1.0);
        // Supports so large that f32 cannot pin the intersection: no snap.
        let big = [1e7, 2e7, 3e7];
        let jb: Vec<f64> = [0.5, 0.4, 0.6].iter().map(|&x: &f64| x as f32 as f64).collect();
        assert!(p.snap(&jb, &big).0.iter().all(|x| x.is_nan()));
    }

    #[test]
    fn labels_parse() {
        assert_eq!(parse_label("kmv-k1024-s17-sets"), Some(Kind::Kmv { k: 1024, seed: 17, support: "sets".into() }));
        assert_eq!(parse_label("l1-m512-s2-f32"), Some(Kind::L1 { m: 512, seed: 2, prec: "f32".into() }));
        assert_eq!(parse_label("varopt-k512-s1-rec"), Some(Kind::Varopt { seed: 1 }));
        assert_eq!(parse_label("varopt-text"), Some(Kind::Varopt { seed: 0 }));
        assert_eq!(parse_label("X_J"), None);
        assert_eq!(parse_label("kmv-kx-s1-sets"), None);
    }

    /// A fake `pairs` output: `n` rows, two KMV seeds in `a` (TSV and
    /// matrices) and a third in `b` (matrices only), and the exact columns.
    fn fake(n: usize, name: &str) -> (std::path::PathBuf, String, String) {
        let root = std::env::temp_dir().join(format!("sketch-pairstats-test-{}-{}", std::process::id(), name));
        let (a, b) = (root.join("a"), root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let k = 4usize;
        let xn: Vec<f64> = (0..n).map(|i| 3.0 + 2.0 * i as f64).collect();
        // Nested supports, row i the newer of i and i+1: J = min/max, cont of the smaller in the larger 1.
        let xj = |a: usize, b: usize| xn[a].min(xn[b]) / xn[a].max(xn[b]);
        let sign = |a: usize, b: usize| if (a + b) % 2 == 0 { 1.0 } else { -1.0 };
        let est = |seed: u64, a: usize, b: usize| (xj(a, b) + 0.01 * seed as f64 * sign(a, b)).clamp(0.0, 1.0);
        let mut rows = String::from("row\trec\ttx\tblock\tfrontier\tX_n\tX_n_pos\n");
        for i in 0..n {
            rows += &format!("{}\t{}\t{}\t{}\t{}\t{}\t{}\n", i, 100 - i, 100 - i, 1000 + 7000 * i, 1000 + 7000 * i, xn[i], xn[i]);
        }
        std::fs::write(a.join("rows.g.tsv"), &rows).unwrap();
        std::fs::write(b.join("rows.g.tsv"), &rows).unwrap();
        let mut tsv = String::from("i\tj\trec_i\trec_j\tblock_i\tblock_j");
        for s in [1, 2] {
            for c in ["J", "se", "n_union", "exact", "cont_ij", "cont_ji"] {
                tsv += &format!("\tkmv-k{}-s{}-sets:{}", k, s, c);
            }
        }
        tsv += "\tX_n_i\tX_n_j\tX_J\tX_cont_ij\tX_cont_ji\tX_jw\n";
        let mut mats: HashMap<String, Vec<f32>> = HashMap::new();
        for a_ in 0..n {
            for b_ in a_ + 1..n {
                tsv += &format!("{}\t{}\t{}\t{}\t{}\t{}", a_, b_, 100 - a_, 100 - b_, 1000 + 7000 * a_, 1000 + 7000 * b_);
                let x = xj(a_, b_);
                // a_ < b_ has the smaller support: it is contained in b_'s.
                let (cij, cji) = (1.0, xn[a_] / xn[b_]);
                for s in [1u64, 2] {
                    let e = est(s, a_, b_);
                    tsv += &format!("\t{}\t{}\t{}\t0\t{}\t{}", e, 0.01, (xn[a_] + xn[b_]) / (1.0 + x), cij, cji);
                }
                tsv += &format!("\t{}\t{}\t{}\t{}\t{}\t{}\n", xn[a_], xn[b_], x, cij, cji, 0.5 * x);
                for (name, v, w) in [
                    ("kmv-k4-s3-sets.J", est(3, a_, b_), est(3, a_, b_)),
                    ("kmv-k4-s3-sets.cont", cij, cji),
                    ("kmv-k4-s1-sets.J", est(1, a_, b_), est(1, a_, b_)),
                ] {
                    let m = mats.entry(name.into()).or_insert_with(|| vec![1.0; n * n]);
                    m[a_ * n + b_] = v as f32;
                    m[b_ * n + a_] = w as f32;
                }
            }
        }
        std::fs::write(a.join("pairs.g.tsv"), tsv).unwrap();
        for (name, m) in &mats {
            let dir = if name.contains("s3") { &b } else { &a };
            let bytes: Vec<u8> = m.iter().flat_map(|x| x.to_le_bytes()).collect();
            std::fs::write(dir.join(format!("m.g.{}.f32", name)), bytes).unwrap();
        }
        (root, a.to_string_lossy().into(), b.to_string_lossy().into())
    }

    #[test]
    fn a_fake_pairs_run_reduces_as_by_hand() {
        let n = 6;
        let (root, a, b) = fake(n, "reduce");
        let out = root.join("out").to_string_lossy().to_string();
        run(StatsOpts { inputs: vec![a.clone(), b.clone()], out: out.clone(), groups: vec![] }).unwrap();
        let read = |name: &str| std::fs::read_to_string(format!("{}/{}", out, name)).unwrap();

        // The TSV and the matrix of seed 1 agree to f32.
        let p = Part::load("g", &[a.clone(), b.clone()]).unwrap().unwrap();
        let from_tsv = p.col("kmv-k4-s1-sets:J").unwrap().unwrap();
        let m = matrix_pairs(&format!("{}/m.g.kmv-k4-s1-sets.J.f32", a), n, false).unwrap();
        assert!(from_tsv.iter().zip(&m).all(|(x, y)| (x - y).abs() < 1e-6));
        // Seed 3 comes from the second directory's matrices, the rest from the TSV.
        let srcs = read("sources.tsv");
        assert!(srcs.contains("kmv-k4-s3-sets\tkmv\tkmv-k4-s*-sets\t3") && srcs.contains("f32 matrix"));
        assert!(srcs.lines().filter(|l| l.contains("\ttsv")).count() == 3, "{}", srcs);

        // Three seeds of errors +-0.01 s by parity: the bias of a stratum is
        // the mean over seeds of each seed's mean error, computed by hand.
        let x: Vec<(usize, usize, f64)> =
            (0..n).flat_map(|a_| (a_ + 1..n).map(move |b_| (a_, b_, (3.0 + 2.0 * a_ as f64) / (3.0 + 2.0 * b_ as f64)))).collect();
        let stratum: Vec<&(usize, usize, f64)> = x.iter().filter(|p| p.2 > 0.5 && p.2 <= 0.9).collect();
        let seed_means: Vec<f64> = [1.0, 2.0, 3.0]
            .iter()
            .map(|&s| {
                let sign = |a: usize, b: usize| if (a + b) % 2 == 0 { 1.0 } else { -1.0 };
                mean(&stratum.iter().map(|p| (p.2 + 0.01 * s * sign(p.0, p.1)).clamp(0.0, 1.0) - p.2).collect::<Vec<_>>())
            })
            .collect();
        let line = read("jaccard-seeds.tsv").lines().find(|l| l.contains("\t(0.5,0.9]\t")).unwrap().to_string();
        let c: Vec<&str> = line.split('\t').collect();
        assert_eq!(c[3], "3");
        assert_eq!(c[5], stratum.len().to_string());
        assert!(close(c[6].parse::<f64>().unwrap(), mean(&seed_means), 1e-6), "{} vs {}", c[6], mean(&seed_means));

        // Rows are in reverse record order, so row j of a pair is the older,
        // and its support the larger: its containment in the newer is
        // X_cont_ji = n_i / n_j < 1, never 1.
        let q = read("quantiles.tsv");
        let cont = q.lines().find(|l| l.starts_with("g\tall\tcont_old_in_new\tX\t")).unwrap();
        let c: Vec<&str> = cont.split('\t').collect();
        assert_eq!(c[4], (n * (n - 1) / 2).to_string());
        assert_eq!(c[c.len() - 4], "0", "the older (larger) support is never inside the newer: {}", cont);

        // The cont rows: the newest row (0) has no newer rows, the oldest all the others.
        let cr = read("cont-rows.tsv");
        let oldest = cr.lines().find(|l| l.starts_with(&format!("g\tkmv-k4-s1-sets\t{}\t", n - 1))).unwrap();
        assert_eq!(oldest.split('\t').nth(6).unwrap(), (n - 1).to_string());

        // Seeds agree: the J estimates differ by 0.01 per seed, so s > 0 in every cell.
        assert!(read("seed-agreement.tsv").contains("sigma>0"));
        // Deterministic: a rerun writes the same bytes in every table.
        let again = root.join("again").to_string_lossy().to_string();
        run(StatsOpts { inputs: vec![a.clone(), b.clone()], out: again.clone(), groups: vec![] }).unwrap();
        for e in std::fs::read_dir(&out).unwrap() {
            let name = e.unwrap().file_name().into_string().unwrap();
            assert_eq!(read(&name), std::fs::read_to_string(format!("{}/{}", again, name)).unwrap(), "{} differs on a rerun", name);
        }
        // A pooled group.
        let out2 = root.join("out2").to_string_lossy().to_string();
        run(StatsOpts { inputs: vec![a, b], out: out2.clone(), groups: vec!["g+g".into()] }).unwrap();
        let pooled = std::fs::read_to_string(format!("{}/jaccard-seeds.tsv", out2)).unwrap();
        assert!(pooled.lines().nth(1).unwrap().starts_with("g+g\t"));
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A group whose rows file has only its header is an error, and a count
    /// column that is all NA gives NA sizes; neither panics.
    #[test]
    fn an_empty_group_or_an_all_na_count_does_not_panic() {
        let (root, a, b) = fake(4, "empty");
        let rows = format!("{}/rows.g.tsv", a);
        let full = std::fs::read_to_string(&rows).unwrap();
        // All NA in X_n (column 5).
        let na: String = full
            .lines()
            .enumerate()
            .map(|(i, l)| {
                let mut c: Vec<&str> = l.split('\t').collect();
                if i > 0 {
                    c[5] = "NA";
                }
                c.join("\t") + "\n"
            })
            .collect();
        std::fs::write(&rows, &na).unwrap();
        std::fs::write(format!("{}/rows.g.tsv", b), &na).unwrap();
        let out = root.join("out").to_string_lossy().to_string();
        run(StatsOpts { inputs: vec![a.clone(), b.clone()], out: out.clone(), groups: vec![] }).unwrap();
        let groups = std::fs::read_to_string(format!("{}/groups.tsv", out)).unwrap();
        let xn = groups.lines().find(|l| l.split('\t').nth(8) == Some("X_n")).unwrap();
        let c: Vec<&str> = xn.split('\t').collect();
        assert_eq!(&c[9..], ["NA", "NA", "NA", "0", "NA", "NA"], "{}", xn);
        // Header only.
        let head = full.lines().next().unwrap().to_string() + "\n";
        std::fs::write(&rows, &head).unwrap();
        let e = run(StatsOpts { inputs: vec![a], out, groups: vec![] }).unwrap_err();
        assert!(e.to_string().contains("no rows"), "{}", e);
        let _ = std::fs::remove_dir_all(&root);
    }
}
