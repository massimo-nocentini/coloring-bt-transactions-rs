//! # `summarize`: the tables of the results, from what `chain` and `eval` wrote
//!
//! `chain` and `eval` measure; they do not conclude.  Between their outputs
//! and a sentence like "`varopt`'s error halves with every doubling of `k`"
//! there is a layer of grouping, averaging, fitting and testing, and for the
//! 2022 sweep that layer was a handful of Python scripts.  This mode is that
//! layer, so that every number of the results can be derived from this crate
//! and the files the other modes wrote, without rerunning a fold.
//!
//! It reads, and never writes, the run directories:
//!
//! - **`--chain <dir>`** (any number): a `chain` output -- one
//!   `rows.<config>.tsv` per store (every `--rows-every`th record; 256 in the
//!   sweep), `buckets.tsv`, `seeds.tsv` and `cost.tsv`.  Configs must be
//!   distinct across the directories (pass A and pass B of the sweep are).
//! - **`--eval <dir>`**: an `eval` output -- `rows.tsv`, `summary.tsv`.
//! - **`--moments <path>`**: the exact moments of every transaction
//!   (`colors.moments.zst`, read through `zstd -dc`, or the text itself), for
//!   the binned participation ratio in its 4th column; only the floors need it.
//!
//! ## What is computed, and with which conventions
//!
//! The conventions are the ones the published tables were made with, so that
//! the two agree to the last printed digit; where one is a choice, it says so.
//!
//! - **Rows statistics** (`stats.tsv`).  Per config, per *group* -- the whole
//!   file (`pooled`), each `--stretch` of records (`S0`, `S1`, ...), and the
//!   first `--head` stretches together (`S0-2` by default: what pass B
//!   covered when the analysis was first written) -- and per subset (`all`,
//!   and `reduced`: the rows whose colour is a sketch; a larger `k` keeps more
//!   early colours exact, which lowers an all-rows average for a reason that
//!   has nothing to do with how well a sketch approximates).  Every metric is
//!   taken over its finite values only.  The mean is NumPy's: pairwise sums of
//!   blocks of 8,192 values, added in order ([`np_sum`]), so that a mean is
//!   the same `f64` as `x.mean()`.  The quantiles are NumPy's default
//!   (`method='linear'`, [`quantile_linear`]), not the nearest-rank ones the
//!   other modes write.  `abs_dmean_over_sd` is `|dmean| / ref_sd` over the
//!   rows whose exact spread is at least one block (`ref_sd * L >= 1`), and
//!   `dlap_max` the largest `|dlap_s|` of a row.
//! - **Headline errors** (`errors.tsv`): the reduced rows' mean and p90
//!   `|dmean|` in blocks (and the p90 in days of 144 blocks), mean
//!   `|dmean|/sd` and `k` times it, mean `dlap_max`, mean `|dsd|/L`, the mean
//!   exact spread in blocks, and the mean `|dmean|` over that of `varopt` at
//!   the same `k`, seed 1, record mode.  Read straight from `stats.tsv`.
//! - **Slopes against `k`** (`slopes.tsv`): for every family of configs that
//!   differ only by `k`, every run of consecutive `k` values, and every group:
//!   the OLS slope of `ln(mean error)` on `ln k`, reduced rows.  Two points
//!   give the local slope; the whole run the global one.  A run is skipped
//!   when one of its means is missing or not positive.
//! - **Depth** (`depth.tsv`): the reduced mean of the last stretch over that
//!   of stretch 1 (stretch 0 is skipped: it still holds many exact rows), and
//!   the depth exponent -- the OLS slope of `ln(bucket mean)` on
//!   `ln((rec_from + rec_to) / 2)` over the `buckets.tsv` buckets past the
//!   first stretch.
//! - **Bias** (`bias.tsv`, `signed.tsv`, `bias_buckets.tsv`).  A config's
//!   signed mean over a group is the count-weighted mean of its
//!   `buckets.tsv` buckets (every `--every`th record; a bucket whose mean is
//!   NA still counts in the weights, as it did in the scripts).  Rows of one
//!   run share sketches and ancestry, so per-row standard errors understate
//!   the spread of a mean many times over: the test is across seeds only.
//!   For configs that differ only by seed, `t = mean / (sd / sqrt(n))` over
//!   the seeds' means, with `n - 1` degrees of freedom; `ci95_over_abs` is
//!   `t_{0.975} SE` over the mean absolute error (the seeds' mean of the
//!   count-weighted `abs_*` of subset `all`) and `bound_over_abs` is
//!   `(|mean| + t_{0.975} SE)` over it, the bound on `|bias|` the text
//!   quotes.  `signed.tsv` gives every config's signed mean over its own mean
//!   absolute error (1 is pure bias), and the RMS and the largest absolute
//!   value over buckets of its signed bucket means; `bias_buckets.tsv` counts the buckets of `seeds.tsv` whose
//!   `|z|` exceeds `t_{0.975}` at `seeds - 1` degrees of freedom.
//! - **Step mode** (`step.tsv`): a `reduce=step` config against the mean of
//!   its `reduce=record` seeds, reduced rows, every group: the ratio, and
//!   `z = (step - rec) / (sd_rec sqrt(1 + 1/n))`.
//! - **Cost** (`cost.tsv`): every `cost.tsv` line, with fold time and
//!   reductions over those of `varopt` at the same `k`, seed 1, record mode.
//! - **The recursion against one reduction** (`ideal.tsv`,
//!   `ideal_slopes.tsv`), from `eval`: for every metric `m` that `eval`
//!   measures beside its single-shot `ideal_m`, per config and stratum, the
//!   two means, the excess `(m - ideal) / ideal`, and the paired mean of
//!   `m - ideal_m` with its batch-means SE, from `summary.tsv`.  `|dmean|`
//!   has no `ideal` line in `summary.tsv`, so its row is computed from
//!   `rows.tsv` (`abs_dmean_L` against `|ideal_dmean_L|`, on the fixed strata
//!   of the exact support size, `(0, 256]` to `> 32768`).  The slopes are
//!   per family and stratum over every `k`, on the positive means; the excess
//!   slope only when the paired excess is positive at every `k`.
//! - **Floors** (`floors_eval.tsv`, `floors_chain.tsv`, `floor_curve.tsv`),
//!   with `--moments`.  A perfect VarOpt sketch with threshold `tau` of a
//!   colour with 1024-band masses `m_b` has `E[TV1024] >= 1 - S2/tau`, `S2 =
//!   sum_b m_b^2 = 1/PR`; `floor_lb` is that bound with the row's `tau` (`1/n`
//!   when the row was not reduced at its own record, the scripts' choice), and
//!   `floor_lb_own_tau` with the row's `tau` as written, `0` where it is `0`
//!   (the per-row figure the text quotes; `varopt` only, since `prio` and
//!   `coord` write other thresholds in that column).  `floor_cal` reads each row off
//!   the curve of `eval`'s measured single-shot TV1024 against `x = PR/k`
//!   (per variant, bins of 1/8 decade with at least 20 rows, linear in
//!   between, `1 - (1 - g0) x0/x` past the last bin).  The band-edge KS floor
//!   is `r_k` times the mean `tau`, `r_k` = `eval`'s single-shot KS over its
//!   `tau` at the same `k` (the `3 tau / 4` of the KS remark in [`crate::metrics`]).
//!
//! The output is a pure function of the input files: configs are written in
//! (variant, options, `k`, seed) order, and the work split over `--threads`
//! is gathered in that order.

use crate::bands::CHAIN_BLOCKS;
use crate::metrics::{self, fmt, inc_beta, t_quantile};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

const L: f64 = CHAIN_BLOCKS as f64;
/// Blocks in a day, for the tolerance column.
const DAY: f64 = 144.0;
/// The Laplace `s` as the column names spell them ([`crate::bands::LAPLACE`]).
const LAPS: [&str; 7] = ["0.5", "1", "2", "4", "8", "16", "32"];

pub struct SummarizeOpts {
    pub chain: Vec<String>,
    pub eval: Option<String>,
    pub moments: Option<String>,
    pub out: String,
    pub stretch: u64,
    pub head: u64,
    pub threads: usize,
}

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

// ------------------------------------------------------------------ numerics

/// NumPy's `np.add.reduce` of a contiguous `f64` array: pairwise sums of
/// blocks of 8,192 values (its buffer), added left to right.
pub fn np_sum(a: &[f64]) -> f64 {
    a.chunks(8192).fold(0.0, |r, c| r + pairwise(c))
}

/// NumPy's `pairwise_sum`: eight accumulators below 128 values, halves above.
fn pairwise(a: &[f64]) -> f64 {
    let n = a.len();
    if n < 8 {
        let mut r = 0.0;
        for x in a {
            r += x;
        }
        r
    } else if n <= 128 {
        let mut r = [0.0f64; 8];
        r.copy_from_slice(&a[..8]);
        let mut i = 8;
        while i < n - n % 8 {
            for (j, rj) in r.iter_mut().enumerate() {
                *rj += a[i + j];
            }
            i += 8;
        }
        let mut res = ((r[0] + r[1]) + (r[2] + r[3])) + ((r[4] + r[5]) + (r[6] + r[7]));
        while i < n {
            res += a[i];
            i += 1;
        }
        res
    } else {
        let mut n2 = n / 2;
        n2 -= n2 % 8;
        pairwise(&a[..n2]) + pairwise(&a[n2..])
    }
}

pub fn np_mean(a: &[f64]) -> f64 {
    if a.is_empty() {
        f64::NAN
    } else {
        np_sum(a) / a.len() as f64
    }
}

/// The mean of the finite values, as pandas' `Series.mean()` skips NaN.
fn nan_mean(a: impl Iterator<Item = f64>) -> f64 {
    let v: Vec<f64> = a.filter(|x| !x.is_nan()).collect();
    np_mean(&v)
}

/// NumPy's default quantile (`method='linear'`): the virtual index `(n - 1) q`,
/// and its two neighbours interpolated as `_lerp` does, from the nearer end.
/// Permutes `v`; NaN-free input.
pub fn quantile_linear(v: &mut [f64], q: f64) -> f64 {
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    let virt = (n - 1) as f64 * q;
    if virt >= (n - 1) as f64 {
        return v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    }
    if virt < 0.0 {
        return v.iter().copied().fold(f64::INFINITY, f64::min);
    }
    let lo = virt.floor();
    let t = virt - lo;
    let (_, a, right) = v.select_nth_unstable_by(lo as usize, f64::total_cmp);
    let a = *a;
    let b = right.iter().copied().fold(f64::INFINITY, f64::min);
    let d = b - a;
    if t >= 0.5 {
        b - d * (1.0 - t)
    } else {
        a + d * t
    }
}

/// `np.median`: the middle value, or the mean of the two middle ones.
pub fn np_median(v: &mut [f64]) -> f64 {
    let n = v.len();
    if n == 0 {
        return f64::NAN;
    }
    let a = *v.select_nth_unstable_by(n / 2, f64::total_cmp).1;
    if n % 2 == 1 {
        return a;
    }
    let b = v[..n / 2].iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (b + a) / 2.0
}

/// The OLS slope of `y` on `x` (what `np.polyfit(x, y, 1)[0]` returns).
pub fn ols_slope(x: &[f64], y: &[f64]) -> f64 {
    let n = x.len() as f64;
    if x.len() < 2 {
        return f64::NAN;
    }
    let mx = x.iter().sum::<f64>() / n;
    let my = y.iter().sum::<f64>() / n;
    let sxy: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum();
    let sxx: f64 = x.iter().map(|a| (a - mx) * (a - mx)).sum();
    sxy / sxx
}

/// The slope of `ln y` on `ln k` over the positive finite points, and how
/// many there were.
fn loglog(ks: &[f64], ys: &[f64]) -> (f64, usize) {
    let (x, y): (Vec<f64>, Vec<f64>) =
        ks.iter().zip(ys).filter(|(_, y)| y.is_finite() && **y > 0.0).map(|(k, y)| (k.ln(), y.ln())).unzip();
    (ols_slope(&x, &y), x.len())
}

/// `(mean, sd)` with `ddof = 1`.
fn mean_sd(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    let m = v.iter().sum::<f64>() / n;
    if v.len() < 2 {
        return (m, f64::NAN);
    }
    (m, (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / (n - 1.0)).sqrt())
}

/// Two-sided p of Student's t with `nu` degrees of freedom.
fn t_p(t: f64, nu: f64) -> f64 {
    inc_beta(nu / (nu + t * t), nu / 2.0, 0.5)
}

#[derive(Clone, Copy, Debug)]
pub struct Stat {
    pub count: usize,
    pub mean: f64,
    pub rms: f64,
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
}

/// Count, mean, RMS and the three quantiles of finite values in row order.
pub fn stat(mut v: Vec<f64>) -> Option<Stat> {
    if v.is_empty() {
        return None;
    }
    let mean = np_mean(&v);
    let sq: Vec<f64> = v.iter().map(|x| x * x).collect();
    let rms = np_mean(&sq).sqrt();
    let p50 = quantile_linear(&mut v, 0.5);
    let p90 = quantile_linear(&mut v, 0.9);
    let p99 = quantile_linear(&mut v, 0.99);
    Some(Stat { count: v.len(), mean, rms, p50, p90, p99 })
}

// ------------------------------------------------------------------ tsv

struct Tsv {
    path: String,
    cols: HashMap<String, usize>,
    lines: io::Lines<BufReader<File>>,
}

impl Tsv {
    fn open(path: &str) -> io::Result<Tsv> {
        let f = File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
        let mut lines = BufReader::with_capacity(1 << 20, f).lines();
        let head = lines.next().transpose()?.ok_or_else(|| bad(format!("{}: empty", path)))?;
        let cols = head.split('\t').enumerate().map(|(i, c)| (c.to_string(), i)).collect();
        Ok(Tsv { path: path.to_string(), cols, lines })
    }

    fn col(&self, name: &str) -> io::Result<usize> {
        self.cols.get(name).copied().ok_or_else(|| bad(format!("{}: no column {:?}", self.path, name)))
    }

    /// Every data line, split, with the line's length checked.
    fn each(self, mut f: impl FnMut(&[&str]) -> io::Result<()>) -> io::Result<()> {
        let width = self.cols.len();
        let path = self.path;
        for line in self.lines {
            let line = line?;
            if line.is_empty() {
                continue;
            }
            let c: Vec<&str> = line.split('\t').collect();
            if c.len() != width {
                return Err(bad(format!("{}: {} fields, not {}, in {:?}", path, c.len(), width, line)));
            }
            f(&c).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
        }
        Ok(())
    }
}

fn num(s: &str) -> io::Result<f64> {
    if s == "NA" {
        return Ok(f64::NAN);
    }
    s.parse().map_err(|_| bad(format!("not a number: {:?}", s)))
}

fn int(s: &str) -> io::Result<u64> {
    s.parse().map_err(|_| bad(format!("not an integer: {:?}", s)))
}

fn create(out: &str, name: &str) -> io::Result<BufWriter<File>> {
    let path = format!("{}/{}", out, name);
    Ok(BufWriter::new(File::create(&path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?))
}

// ------------------------------------------------------------------ labels

/// A config label split into its parts: `coord-k64-s1-rec-min-f0`.
#[derive(Clone, Debug, PartialEq)]
pub struct Label {
    pub text: String,
    pub variant: String,
    pub k: u64,
    pub seed: u64,
    pub reduce: String,
    pub rest: String,
}

impl Label {
    pub fn parse(text: &str) -> Option<Label> {
        let t: Vec<&str> = text.split('-').collect();
        if t.len() < 4 {
            return None;
        }
        Some(Label {
            text: text.to_string(),
            variant: t[0].to_string(),
            k: t[1].strip_prefix('k')?.parse().ok()?,
            seed: t[2].strip_prefix('s')?.parse().ok()?,
            reduce: t[3].to_string(),
            rest: t[4..].join("-"),
        })
    }

    /// The configs that differ from this one only by `k`.
    pub fn k_family(&self) -> String {
        let mut t: Vec<&str> = self.text.split('-').collect();
        t[1] = "k*";
        t.join("-")
    }

    /// The order every table is written in.
    fn key(&self) -> (u8, String, String, u64, u64) {
        let v = match self.variant.as_str() {
            "varopt" => 0,
            "prio" => 1,
            "coord" => 2,
            _ => 3,
        };
        (v, self.rest.clone(), self.reduce.clone(), self.k, self.seed)
    }

    /// `varopt` at this `k`, seed 1, record mode: the reference of the ratios.
    fn varopt_ref(&self) -> String {
        format!("varopt-k{}-s1-rec", self.k)
    }
}

fn label(text: &str) -> io::Result<Label> {
    Label::parse(text).ok_or_else(|| bad(format!("not a config label: {:?}", text)))
}

fn sort_labels(v: &mut [Label]) {
    v.sort_by_key(|l| l.key());
}

// ------------------------------------------------------------------ chain rows

/// The columns of one `rows.<config>.tsv` the tables use.
struct Rows {
    label: Label,
    rec: Vec<u64>,
    tx: Vec<u64>,
    n: Vec<f64>,
    tau: Vec<f64>,
    reduced: Vec<bool>,
    dmean: Vec<f64>,
    dsd: Vec<f64>,
    dlap: Vec<[f64; 7]>,
    ref_sd: Vec<f64>,
    tv: Vec<f64>,
    band_ks: Vec<f64>,
    band_w1: Vec<f64>,
}

fn load_rows(path: &str, label: Label) -> io::Result<Rows> {
    let t = Tsv::open(path)?;
    let c = [
        t.col("rec")?,
        t.col("tx_id")?,
        t.col("n")?,
        t.col("tau")?,
        t.col("reduced")?,
        t.col("dmean_L")?,
        t.col("dsd_L")?,
        t.col("ref_sd_L")?,
        t.col("tv1024")?,
        t.col("band_ks")?,
        t.col("band_w1_L")?,
    ];
    let lap: Vec<usize> = LAPS.iter().map(|s| t.col(&format!("dlap_{}", s))).collect::<io::Result<_>>()?;
    let mut r = Rows {
        label,
        rec: Vec::new(),
        tx: Vec::new(),
        n: Vec::new(),
        tau: Vec::new(),
        reduced: Vec::new(),
        dmean: Vec::new(),
        dsd: Vec::new(),
        dlap: Vec::new(),
        ref_sd: Vec::new(),
        tv: Vec::new(),
        band_ks: Vec::new(),
        band_w1: Vec::new(),
    };
    t.each(|f| {
        let rec = int(f[c[0]])?;
        if r.rec.last().is_some_and(|&p| p >= rec) {
            return Err(bad(format!("records out of order at {}", rec)));
        }
        r.rec.push(rec);
        r.tx.push(int(f[c[1]])?);
        r.n.push(num(f[c[2]])?);
        r.tau.push(num(f[c[3]])?);
        r.reduced.push(f[c[4]] == "1");
        r.dmean.push(num(f[c[5]])?);
        r.dsd.push(num(f[c[6]])?);
        r.ref_sd.push(num(f[c[7]])?);
        r.tv.push(num(f[c[8]])?);
        r.band_ks.push(num(f[c[9]])?);
        r.band_w1.push(num(f[c[10]])?);
        let mut d = [0.0; 7];
        for (x, &i) in d.iter_mut().zip(&lap) {
            *x = num(f[i])?;
        }
        r.dlap.push(d);
        Ok(())
    })?;
    if r.rec.is_empty() {
        return Err(bad(format!("{}: no rows", path)));
    }
    Ok(r)
}

/// A metric of the rows tables, one value per row (NaN where undefined).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Metric {
    AbsDmean,
    AbsDmeanOverSd,
    AbsDsd,
    AbsDlap(usize),
    DlapMax,
    Tv,
    BandKs,
    BandW1,
    RefSd,
    Dmean,
    Dlap(usize),
}

impl Metric {
    fn all() -> Vec<Metric> {
        let mut v = vec![Metric::AbsDmean, Metric::AbsDmeanOverSd, Metric::AbsDsd];
        v.extend((0..7).map(Metric::AbsDlap));
        v.extend([Metric::DlapMax, Metric::Tv, Metric::BandKs, Metric::BandW1, Metric::RefSd, Metric::Dmean]);
        v.extend((0..7).map(Metric::Dlap));
        v
    }

    fn name(self) -> String {
        match self {
            Metric::AbsDmean => "abs_dmean_L".into(),
            Metric::AbsDmeanOverSd => "abs_dmean_over_sd".into(),
            Metric::AbsDsd => "abs_dsd_L".into(),
            Metric::AbsDlap(i) => format!("abs_dlap_{}", LAPS[i]),
            Metric::DlapMax => "dlap_max".into(),
            Metric::Tv => "tv1024".into(),
            Metric::BandKs => "band_ks".into(),
            Metric::BandW1 => "band_w1_L".into(),
            Metric::RefSd => "ref_sd_L".into(),
            Metric::Dmean => "dmean_L_signed".into(),
            Metric::Dlap(i) => format!("dlap_{}_signed", LAPS[i]),
        }
    }

    /// An error size: what the slopes, depth and step tables compare.
    fn is_error(self) -> bool {
        !matches!(self, Metric::RefSd | Metric::Dmean | Metric::Dlap(_))
    }

    /// The same metric's name in `buckets.tsv`.
    fn bucket_name(self) -> Option<String> {
        match self {
            Metric::AbsDmeanOverSd => Some("dmean_over_sd".into()),
            Metric::RefSd | Metric::Dmean | Metric::Dlap(_) => None,
            m => Some(m.name()),
        }
    }

    fn values(self, r: &Rows) -> Vec<f64> {
        let n = r.rec.len();
        (0..n)
            .map(|i| match self {
                Metric::AbsDmean => r.dmean[i].abs(),
                Metric::AbsDmeanOverSd => {
                    if r.ref_sd[i] * L >= 1.0 {
                        r.dmean[i].abs() / r.ref_sd[i]
                    } else {
                        f64::NAN
                    }
                }
                Metric::AbsDsd => r.dsd[i].abs(),
                Metric::AbsDlap(j) => r.dlap[i][j].abs(),
                Metric::DlapMax => r.dlap[i].iter().fold(0.0f64, |m, x| if m.is_nan() || x.is_nan() { f64::NAN } else { m.max(x.abs()) }),
                Metric::Tv => r.tv[i],
                Metric::BandKs => r.band_ks[i],
                Metric::BandW1 => r.band_w1[i],
                Metric::RefSd => r.ref_sd[i],
                Metric::Dmean => r.dmean[i],
                Metric::Dlap(j) => r.dlap[i][j],
            })
            .collect()
    }
}

/// A group of rows: a contiguous range, since rows are in record order.
#[derive(Clone, Debug, PartialEq)]
struct Group {
    name: String,
    /// The records it spans, `[from, to)`.
    from: u64,
    to: u64,
    lo: usize,
    hi: usize,
}

/// `pooled`, then one group per stretch, then the first `head` stretches.
fn groups(rec: &[u64], stretch: u64, head: u64) -> Vec<Group> {
    let end = rec.last().map_or(0, |r| r + 1);
    let at = |x: u64| rec.partition_point(|&r| r < x);
    let mut g = vec![Group { name: "pooled".into(), from: 0, to: end, lo: 0, hi: rec.len() }];
    let ns = end.div_ceil(stretch);
    for j in 0..ns {
        let (a, b) = (j * stretch, ((j + 1) * stretch).min(end));
        g.push(Group { name: format!("S{}", j), from: a, to: b, lo: at(a), hi: at(b) });
    }
    if head >= 2 && head < ns {
        let b = head * stretch;
        g.push(Group { name: format!("S0-{}", head - 1), from: 0, to: b, lo: 0, hi: at(b) });
    }
    g
}

/// The stretches alone (`S<j>`), as the bucket tables cut them.
fn stretch_of(rec_from: u64, stretch: u64) -> String {
    format!("S{}", rec_from / stretch)
}

struct StatRow {
    config: Label,
    group: Group,
    subset: &'static str,
    metric: String,
    stat: Stat,
}

/// The rows a floor needs, kept past the statistics.
struct FloorRows {
    label: Label,
    rec: Vec<u64>,
    tx: Vec<u64>,
    n: Vec<f64>,
    tau: Vec<f64>,
    reduced: Vec<bool>,
    tv: Vec<f64>,
    band_ks: Vec<f64>,
}

fn rows_stats(r: &Rows, stretch: u64, head: u64) -> Vec<StatRow> {
    let gs = groups(&r.rec, stretch, head);
    let mut out = Vec::new();
    let metrics = Metric::all();
    let vals: Vec<Vec<f64>> = metrics.iter().map(|m| m.values(r)).collect();
    for g in &gs {
        for subset in ["all", "reduced"] {
            let keep = |i: usize| subset == "all" || r.reduced[i];
            for (m, v) in metrics.iter().zip(&vals) {
                let x: Vec<f64> = (g.lo..g.hi).filter(|&i| keep(i)).map(|i| v[i]).filter(|x| x.is_finite()).collect();
                if let Some(s) = stat(x) {
                    out.push(StatRow { config: r.label.clone(), group: g.clone(), subset, metric: m.name(), stat: s });
                }
            }
        }
        let n = g.hi - g.lo;
        if n > 0 {
            let red = r.reduced[g.lo..g.hi].iter().filter(|&&b| b).count();
            let frac = 1.0 - red as f64 / n as f64;
            let nan = f64::NAN;
            out.push(StatRow {
                config: r.label.clone(),
                group: g.clone(),
                subset: "all",
                metric: "frac_exact".into(),
                stat: Stat { count: n, mean: frac, rms: nan, p50: nan, p90: nan, p99: nan },
            });
        }
    }
    out
}

/// What one rows file gives: its statistics, and its floor columns if kept.
type Loaded = io::Result<(Vec<StatRow>, Option<FloorRows>)>;

/// Every `rows.*.tsv` of the chain directories, loaded and summarised on
/// `threads` threads, in file order.
fn load_chain(opts: &SummarizeOpts, keep_floors: bool) -> io::Result<(Vec<StatRow>, Vec<FloorRows>)> {
    let mut files = Vec::new();
    let mut seen = BTreeSet::new();
    for dir in &opts.chain {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {}", dir, e)))?
            .filter_map(|e| e.ok().and_then(|e| e.file_name().into_string().ok()))
            .filter(|n| n.starts_with("rows.") && n.ends_with(".tsv"))
            .collect();
        names.sort();
        for n in names {
            let l = label(&n["rows.".len()..n.len() - ".tsv".len()])?;
            if !seen.insert(l.text.clone()) {
                return Err(bad(format!("config {} is in two --chain directories", l.text)));
            }
            files.push((format!("{}/{}", dir, n), l));
        }
    }
    let next = AtomicUsize::new(0);
    let slots: Mutex<Vec<Option<Loaded>>> =
        Mutex::new((0..files.len()).map(|_| None).collect());
    std::thread::scope(|s| {
        for _ in 0..opts.threads.min(files.len()).max(1) {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((path, l)) = files.get(i) else { break };
                let res = load_rows(path, l.clone()).map(|r| {
                    let st = rows_stats(&r, opts.stretch, opts.head);
                    let fl = (keep_floors && r.tv.iter().any(|x| x.is_finite())).then_some(FloorRows {
                        label: r.label,
                        rec: r.rec,
                        tx: r.tx,
                        n: r.n,
                        tau: r.tau,
                        reduced: r.reduced,
                        tv: r.tv,
                        band_ks: r.band_ks,
                    });
                    (st, fl)
                });
                eprintln!("summarize: {} {}", path, if res.is_ok() { "done" } else { "failed" });
                slots.lock().unwrap()[i] = Some(res);
            });
        }
    });
    let mut stats = Vec::new();
    let mut floors = Vec::new();
    for r in slots.into_inner().unwrap() {
        let (st, fl) = r.expect("every file is taken")?;
        stats.extend(st);
        floors.extend(fl);
    }
    stats.sort_by_key(|a| a.config.key());
    floors.sort_by_key(|a| a.label.key());
    Ok((stats, floors))
}

/// `stats.tsv` by (config, group, subset, metric).
struct Index<'a> {
    map: HashMap<(String, String, &'static str, String), &'a StatRow>,
    configs: Vec<Label>,
    /// Each config's groups in table order.
    groups: HashMap<String, Vec<Group>>,
}

impl<'a> Index<'a> {
    fn new(rows: &'a [StatRow]) -> Index<'a> {
        let mut map = HashMap::new();
        let mut configs: Vec<Label> = Vec::new();
        let mut groups: HashMap<String, Vec<Group>> = HashMap::new();
        for r in rows {
            if configs.last().is_none_or(|l| l.text != r.config.text) {
                configs.push(r.config.clone());
            }
            let g = groups.entry(r.config.text.clone()).or_default();
            if !g.iter().any(|x| x.name == r.group.name) {
                g.push(r.group.clone());
            }
            map.insert((r.config.text.clone(), r.group.name.clone(), r.subset, r.metric.clone()), r);
        }
        Index { map, configs, groups }
    }

    fn get(&self, config: &str, group: &str, subset: &'static str, metric: &str) -> Option<Stat> {
        self.map.get(&(config.to_string(), group.to_string(), subset, metric.to_string())).map(|r| r.stat)
    }

    fn mean(&self, config: &str, group: &str, metric: &str) -> f64 {
        self.get(config, group, "reduced", metric).map_or(f64::NAN, |s| s.mean)
    }
}

fn write_stats(out: &str, rows: &[StatRow]) -> io::Result<()> {
    let mut w = create(out, "stats.tsv")?;
    writeln!(w, "config\tvariant\tk\tseed\treduce\tgroup\trec_from\trec_to\tsubset\tmetric\tcount\tmean\trms\tp50\tp90\tp99")?;
    for r in rows {
        let (c, g, s) = (&r.config, &r.group, &r.stat);
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            c.text,
            c.variant,
            c.k,
            c.seed,
            c.reduce,
            g.name,
            g.from,
            g.to,
            r.subset,
            r.metric,
            s.count,
            fmt(s.mean),
            fmt(s.rms),
            fmt(s.p50),
            fmt(s.p90),
            fmt(s.p99)
        )?;
    }
    w.flush()
}

fn write_errors(out: &str, ix: &Index) -> io::Result<()> {
    let mut w = create(out, "errors.tsv")?;
    writeln!(
        w,
        "config\tvariant\tk\tgroup\trec_from\trec_to\trows\tmean_abs_dmean_blocks\tp90_abs_dmean_blocks\tp90_abs_dmean_days\t\
         mean_abs_dmean_over_sd\tk_times_mean_abs_dmean_over_sd\tmean_dlap_max\tmean_abs_dsd_L\tmean_ref_sd_blocks\tabs_dmean_over_varopt"
    )?;
    for c in &ix.configs {
        for g in &ix.groups[&c.text] {
            let Some(d) = ix.get(&c.text, &g.name, "reduced", "abs_dmean_L") else { continue };
            let over_sd = ix.mean(&c.text, &g.name, "abs_dmean_over_sd");
            let vo = ix.mean(&c.varopt_ref(), &g.name, "abs_dmean_L");
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                c.text,
                c.variant,
                c.k,
                g.name,
                g.from,
                g.to,
                d.count,
                fmt(d.mean * L),
                fmt(d.p90 * L),
                fmt(d.p90 * L / DAY),
                fmt(over_sd),
                fmt(over_sd * c.k as f64),
                fmt(ix.mean(&c.text, &g.name, "dlap_max")),
                fmt(ix.mean(&c.text, &g.name, "abs_dsd_L")),
                fmt(ix.mean(&c.text, &g.name, "ref_sd_L") * L),
                fmt(d.mean / vo)
            )?;
        }
    }
    w.flush()
}

/// Each `k` family's name and configs, in table order.
type Families = BTreeMap<(u8, String, String, u64, u64), (String, Vec<Label>)>;

/// The configs of each `k` family, in `k` order.
fn k_families(configs: &[Label]) -> Families {
    let mut f: BTreeMap<_, (String, Vec<Label>)> = BTreeMap::new();
    for c in configs {
        let (v, rest, red, _, seed) = c.key();
        f.entry((v, rest, red, seed, 0)).or_insert_with(|| (c.k_family(), Vec::new())).1.push(c.clone());
    }
    for (_, v) in f.values_mut() {
        v.sort_by_key(|l| l.k);
    }
    f
}

fn write_slopes(out: &str, ix: &Index) -> io::Result<()> {
    let mut w = create(out, "slopes.tsv")?;
    writeln!(w, "family\tgroup\tmetric\tk_from\tk_to\tpoints\tslope")?;
    for (fam, cs) in k_families(&ix.configs).values() {
        if cs.len() < 2 {
            continue;
        }
        let gnames: Vec<String> = ix.groups[&cs[0].text].iter().map(|g| g.name.clone()).collect();
        for g in &gnames {
            for m in Metric::all().into_iter().filter(|m| m.is_error()) {
                let ys: Vec<f64> = cs.iter().map(|c| ix.mean(&c.text, g, &m.name())).collect();
                for i in 0..cs.len() {
                    for j in i + 1..cs.len() {
                        let y = &ys[i..=j];
                        if y.iter().any(|v| !(v.is_finite() && *v > 0.0)) {
                            continue;
                        }
                        let k: Vec<f64> = cs[i..=j].iter().map(|c| c.k as f64).collect();
                        let (s, n) = loglog(&k, y);
                        writeln!(w, "{}\t{}\t{}\t{}\t{}\t{}\t{}", fam, g, m.name(), cs[i].k, cs[j].k, n, fmt(s))?;
                    }
                }
            }
        }
    }
    w.flush()
}

// ------------------------------------------------------------------ buckets, seeds, cost

#[derive(Clone)]
struct Bucket {
    config: String,
    rec_from: u64,
    rec_to: u64,
    metric: String,
    subset: String,
    count: f64,
    mean: f64,
}

fn load_buckets(dirs: &[String]) -> io::Result<Vec<Bucket>> {
    let mut v = Vec::new();
    for d in dirs {
        let path = format!("{}/buckets.tsv", d);
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        let t = Tsv::open(&path)?;
        let c = [
            t.col("config")?,
            t.col("rec_from")?,
            t.col("rec_to")?,
            t.col("metric")?,
            t.col("subset")?,
            t.col("count")?,
            t.col("mean")?,
        ];
        t.each(|f| {
            v.push(Bucket {
                config: f[c[0]].to_string(),
                rec_from: int(f[c[1]])?,
                rec_to: int(f[c[2]])?,
                metric: f[c[3]].to_string(),
                subset: f[c[4]].to_string(),
                count: num(f[c[5]])?,
                mean: num(f[c[6]])?,
            });
            Ok(())
        })?;
    }
    Ok(v)
}

fn write_depth(out: &str, ix: &Index, buckets: &[Bucket], stretch: u64) -> io::Result<()> {
    let mut by: HashMap<(&str, &str), Vec<&Bucket>> = HashMap::new();
    for b in buckets.iter().filter(|b| b.subset == "reduced" && b.rec_from >= stretch) {
        by.entry((b.config.as_str(), b.metric.as_str())).or_default().push(b);
    }
    let mut w = create(out, "depth.tsv")?;
    writeln!(w, "config\tmetric\tfirst\tlast\tfirst_mean\tlast_mean\tratio\tdepth_buckets\tdepth_exponent")?;
    for c in &ix.configs {
        let gs: Vec<&Group> = ix.groups[&c.text].iter().filter(|g| g.name.starts_with('S') && !g.name.contains('-')).collect();
        if gs.len() < 3 {
            continue;
        }
        let (first, last) = (&gs[1].name, &gs[gs.len() - 1].name);
        for m in Metric::all().into_iter().filter(|m| m.is_error() || *m == Metric::RefSd) {
            let (a, b) = (ix.mean(&c.text, first, &m.name()), ix.mean(&c.text, last, &m.name()));
            if a.is_nan() && b.is_nan() {
                continue;
            }
            let bn = m.bucket_name().unwrap_or_default();
            let (n, slope) = match by.get(&(c.text.as_str(), bn.as_str())) {
                Some(bs) => {
                    let x: Vec<f64> = bs.iter().map(|b| ((b.rec_from + b.rec_to) as f64 / 2.0).ln()).collect();
                    let y: Vec<f64> = bs.iter().map(|b| b.mean.ln()).collect();
                    let s = if y.iter().all(|v| v.is_finite()) { ols_slope(&x, &y) } else { f64::NAN };
                    (bs.len(), s)
                }
                None => (0, f64::NAN),
            };
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                c.text,
                m.name(),
                first,
                last,
                fmt(a),
                fmt(b),
                fmt(b / a),
                n,
                fmt(slope)
            )?;
        }
    }
    w.flush()
}

/// The count-weighted mean over buckets, by (config, group, metric, subset):
/// `sum(count * mean) / sum(count)`, a bucket whose mean is NA left out of
/// the numerator only.  Groups: `pooled` and the stretches.
type Weighted = BTreeMap<(String, String, String, String), (f64, f64, Vec<f64>)>;

fn weighted(buckets: &[Bucket], stretch: u64) -> Weighted {
    let mut m: Weighted = BTreeMap::new();
    for b in buckets {
        for g in ["pooled".to_string(), stretch_of(b.rec_from, stretch)] {
            let e = m.entry((b.config.clone(), g, b.metric.clone(), b.subset.clone())).or_insert((0.0, 0.0, Vec::new()));
            if !b.mean.is_nan() {
                e.0 += b.count * b.mean;
                e.2.push(b.mean);
            }
            e.1 += b.count;
        }
    }
    m
}

fn wmean(w: &Weighted, config: &str, group: &str, metric: &str, subset: &str) -> f64 {
    w.get(&(config.into(), group.into(), metric.into(), subset.into())).map_or(f64::NAN, |e| e.0 / e.1)
}

/// The absolute metric beside a signed one in `buckets.tsv`.
fn abs_of(signed: &str) -> String {
    if signed == "dmean_L" {
        "abs_dmean_L".into()
    } else {
        format!("abs_{}", signed)
    }
}

fn group_order(g: &str) -> (u8, u64) {
    match g.strip_prefix('S').and_then(|s| s.parse().ok()) {
        Some(j) => (1, j),
        None => (0, 0),
    }
}

fn write_bias(out: &str, buckets: &[Bucket], stretch: u64) -> io::Result<()> {
    let w8 = weighted(buckets, stretch);
    let mut labels: Vec<Label> = buckets
        .iter()
        .map(|b| b.config.clone())
        .collect::<BTreeSet<_>>()
        .iter()
        .map(|s| label(s))
        .collect::<io::Result<_>>()?;
    sort_labels(&mut labels);
    // (config, group, metric) of the signed lines, in table order.
    let mut keys: Vec<(String, String)> = w8
        .keys()
        .filter(|k| k.3 == "signed")
        .map(|k| (k.1.clone(), k.2.clone()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    keys.sort_by(|a, b| group_order(&a.0).cmp(&group_order(&b.0)).then(metric_order(&a.1).cmp(&metric_order(&b.1))));

    let mut s = create(out, "signed.tsv")?;
    writeln!(s, "config\tgroup\tmetric\tbuckets\tsigned_mean\tabs_mean\tsigned_over_abs\tbucket_rms\tbucket_max_abs")?;
    for c in &labels {
        for (g, m) in &keys {
            let Some(e) = w8.get(&(c.text.clone(), g.clone(), m.clone(), "signed".into())) else { continue };
            let sm = e.0 / e.1;
            let am = wmean(&w8, &c.text, g, &abs_of(m), "all");
            let rms = (e.2.iter().map(|x| x * x).sum::<f64>() / e.2.len() as f64).sqrt();
            let max = e.2.iter().fold(0.0f64, |a, x| a.max(x.abs()));
            writeln!(
                s,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                c.text,
                g,
                m,
                e.2.len(),
                fmt(sm),
                fmt(am),
                fmt(sm / am),
                fmt(rms),
                fmt(max)
            )?;
        }
    }
    s.flush()?;

    let mut fams: BTreeMap<String, Vec<&Label>> = BTreeMap::new();
    for c in &labels {
        fams.entry(metrics::family(&c.text)).or_default().push(c);
    }
    let mut w = create(out, "bias.tsv")?;
    writeln!(w, "family\tgroup\tmetric\tseeds\tmean\tseed_sd\tse\tt\tp\tt_crit\tabs_mean\tci95_over_abs\tbound_over_abs")?;
    let mut fam_list: Vec<(&String, &Vec<&Label>)> = fams.iter().filter(|(_, v)| v.len() >= 2).collect();
    fam_list.sort_by_key(|(_, v)| v[0].key());
    for (fam, cs) in fam_list {
        for (g, m) in &keys {
            let v: Vec<f64> = cs.iter().map(|c| wmean(&w8, &c.text, g, m, "signed")).collect();
            if v.iter().any(|x| x.is_nan()) {
                continue;
            }
            let a: Vec<f64> = cs.iter().map(|c| wmean(&w8, &c.text, g, &abs_of(m), "all")).collect();
            let n = v.len() as f64;
            let (mu, sd) = mean_sd(&v);
            let se = sd / n.sqrt();
            let t = mu / se;
            let tc = t_quantile(0.975, n - 1.0);
            let am = a.iter().sum::<f64>() / n;
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                fam,
                g,
                m,
                v.len(),
                fmt(mu),
                fmt(sd),
                fmt(se),
                fmt(t),
                fmt(t_p(t, n - 1.0)),
                fmt(tc),
                fmt(am),
                fmt(tc * se / am),
                fmt((mu.abs() + tc * se) / am)
            )?;
        }
    }
    w.flush()
}

fn metric_order(m: &str) -> (u8, String) {
    match m.strip_prefix("dlap_").and_then(|s| s.parse::<f64>().ok()) {
        Some(s) => (1, format!("{:012.3}", s)),
        None => (0, m.to_string()),
    }
}

fn write_bias_buckets(out: &str, dirs: &[String]) -> io::Result<()> {
    // family -> metric -> (seeds, z values)
    let mut by: BTreeMap<String, BTreeMap<String, (f64, Vec<f64>)>> = BTreeMap::new();
    for d in dirs {
        let path = format!("{}/seeds.tsv", d);
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        let t = Tsv::open(&path)?;
        let c = [t.col("family")?, t.col("metric")?, t.col("subset")?, t.col("seeds")?, t.col("z")?];
        t.each(|f| {
            if f[c[2]] == "signed" {
                let e = by.entry(f[c[0]].into()).or_default().entry(f[c[1]].into()).or_insert((0.0, Vec::new()));
                e.0 = e.0.max(num(f[c[3]])?);
                let z = num(f[c[4]])?;
                if z.is_finite() {
                    e.1.push(z);
                }
            }
            Ok(())
        })?;
    }
    let mut w = create(out, "bias_buckets.tsv")?;
    writeln!(w, "family\tmetric\tseeds\tbuckets\tt_crit\trejected\tmax_abs_z")?;
    let mut fams: Vec<(&String, _)> = by.iter().collect();
    fams.sort_by_key(|(f, _)| Label::parse(&f.replace("s*", "s0")).map(|l| l.key()));
    for (fam, ms) in fams {
        let mut mk: Vec<&String> = ms.keys().collect();
        mk.sort_by_key(|m| metric_order(m));
        for m in mk {
            let (seeds, z) = &ms[m];
            let tc = t_quantile(0.975, seeds - 1.0);
            let rej = z.iter().filter(|x| x.abs() > tc).count();
            let mx = z.iter().fold(0.0f64, |a, x| a.max(x.abs()));
            writeln!(w, "{}\t{}\t{}\t{}\t{}\t{}\t{}", fam, m, seeds, z.len(), fmt(tc), rej, fmt(mx))?;
        }
    }
    w.flush()
}

fn write_step(out: &str, ix: &Index) -> io::Result<()> {
    let mut w = create(out, "step.tsv")?;
    writeln!(w, "config\tgroup\tmetric\trec_seeds\trec_mean\trec_seed_sd\tstep\tstep_over_rec\tz")?;
    for c in ix.configs.iter().filter(|c| c.reduce == "step") {
        let rec_fam = metrics::family(&c.text.replacen("-step", "-rec", 1));
        let recs: Vec<&Label> = ix.configs.iter().filter(|r| r.reduce == "rec" && metrics::family(&r.text) == rec_fam).collect();
        if recs.is_empty() {
            continue;
        }
        for g in &ix.groups[&c.text] {
            for m in Metric::all().into_iter().filter(|m| m.is_error()) {
                let st = ix.mean(&c.text, &g.name, &m.name());
                let r: Vec<f64> = recs.iter().map(|r| ix.mean(&r.text, &g.name, &m.name())).collect();
                if st.is_nan() || r.iter().any(|x| x.is_nan()) {
                    continue;
                }
                let (mu, sd) = mean_sd(&r);
                let z = (st - mu) / (sd * (1.0 + 1.0 / r.len() as f64).sqrt());
                writeln!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    c.text,
                    g.name,
                    m.name(),
                    r.len(),
                    fmt(mu),
                    fmt(sd),
                    fmt(st),
                    fmt(st / mu),
                    fmt(z)
                )?;
            }
        }
    }
    w.flush()
}

fn write_cost(out: &str, dirs: &[String]) -> io::Result<()> {
    let mut rows: Vec<(String, Vec<String>, f64, f64)> = Vec::new();
    for d in dirs {
        let path = format!("{}/cost.tsv", d);
        if !std::path::Path::new(&path).exists() {
            continue;
        }
        let t = Tsv::open(&path)?;
        let names = ["config", "busy_s", "records", "peak_entries", "peak_sketches", "reductions", "vm_hwm"];
        let c: Vec<usize> = names.iter().map(|n| t.col(n)).collect::<io::Result<_>>()?;
        t.each(|f| {
            rows.push((d.clone(), c.iter().map(|&i| f[i].to_string()).collect(), num(f[c[1]])?, num(f[c[5]])?));
            Ok(())
        })?;
    }
    let base: HashMap<String, (f64, f64)> = rows.iter().map(|r| (r.1[0].clone(), (r.2, r.3))).collect();
    let mut w = create(out, "cost.tsv")?;
    writeln!(
        w,
        "dir\tconfig\tbusy_s\trecords\tpeak_entries\tpeak_sketches\treductions\tvm_hwm\treference\tbusy_over_reference\treductions_over_reference"
    )?;
    for (d, f, busy, red) in &rows {
        let r = Label::parse(&f[0]).map(|l| l.varopt_ref()).unwrap_or_default();
        let (bb, br) = base.get(&r).copied().unwrap_or((f64::NAN, f64::NAN));
        writeln!(w, "{}\t{}\t{}\t{}\t{}", d, f.join("\t"), r, fmt(busy / bb), fmt(red / br))?;
    }
    w.flush()
}

// ------------------------------------------------------------------ eval

struct EvalRow {
    config: String,
    tx: u64,
    n_exact: f64,
    n_sketch: f64,
    reduced: bool,
    tau: f64,
    ks: f64,
    w1: f64,
    tv: f64,
    dmean: f64,
    ideal_ks: f64,
    ideal_w1: f64,
    ideal_tv: f64,
    ideal_dmean: f64,
}

fn load_eval_rows(dir: &str) -> io::Result<Vec<EvalRow>> {
    let t = Tsv::open(&format!("{}/rows.tsv", dir))?;
    let names = [
        "config",
        "tx_id",
        "n_exact",
        "n_sketch",
        "reduced",
        "tau",
        "ks",
        "w1_L",
        "tv1024",
        "dmean_L",
        "ideal_ks",
        "ideal_w1_L",
        "ideal_tv1024",
        "ideal_dmean_L",
    ];
    let c: Vec<usize> = names.iter().map(|n| t.col(n)).collect::<io::Result<_>>()?;
    let mut v = Vec::new();
    t.each(|f| {
        v.push(EvalRow {
            config: f[c[0]].to_string(),
            tx: int(f[c[1]])?,
            n_exact: num(f[c[2]])?,
            n_sketch: num(f[c[3]])?,
            reduced: f[c[4]] == "1",
            tau: num(f[c[5]])?,
            ks: num(f[c[6]])?,
            w1: num(f[c[7]])?,
            tv: num(f[c[8]])?,
            dmean: num(f[c[9]])?,
            ideal_ks: num(f[c[10]])?,
            ideal_w1: num(f[c[11]])?,
            ideal_tv: num(f[c[12]])?,
            ideal_dmean: num(f[c[13]])?,
        });
        Ok(())
    })?;
    Ok(v)
}

/// The fixed stratum of an exact support size, as `eval` names it; `None`
/// for an empty support (which no fixed stratum holds).
pub fn fixed_stratum(n: f64) -> Option<&'static str> {
    if n.is_nan() || n <= 0.0 {
        None
    } else if n <= 256.0 {
        Some("fix:<=256")
    } else if n <= 4096.0 {
        Some("fix:(256,4096]")
    } else if n <= 32768.0 {
        Some("fix:(4096,32768]")
    } else {
        Some("fix:>32768")
    }
}

/// One cell of the recursion-against-ideal table.
#[derive(Clone, Copy)]
struct Cell {
    count: f64,
    measured: f64,
    ideal: f64,
    paired: f64,
    paired_se: f64,
}

type Cells = BTreeMap<(String, String, String), Cell>;

/// (config, stratum, metric) -> cell, from `summary.tsv` and, for `|dmean|`,
/// from `rows.tsv`.
fn ideal_cells(dir: &str, rows: &[EvalRow]) -> io::Result<(Cells, Vec<String>)> {
    let t = Tsv::open(&format!("{}/summary.tsv", dir))?;
    let c = [t.col("config")?, t.col("stratum")?, t.col("metric")?, t.col("count")?, t.col("mean")?, t.col("block_se")?];
    let mut s: HashMap<(String, String, String), (f64, f64, f64)> = HashMap::new();
    let mut strata: Vec<String> = Vec::new();
    t.each(|f| {
        if !strata.iter().any(|x| x == f[c[1]]) {
            strata.push(f[c[1]].to_string());
        }
        s.insert((f[c[0]].into(), f[c[1]].into(), f[c[2]].into()), (num(f[c[3]])?, num(f[c[4]])?, num(f[c[5]])?));
        Ok(())
    })?;
    let mut cells = BTreeMap::new();
    for ((cfg, st, m), &(_, a, _)) in &s {
        let Some(base) = m.strip_prefix("ideal_") else { continue };
        let Some(&(cnt, meas, _)) = s.get(&(cfg.clone(), st.clone(), base.to_string())) else { continue };
        let (pm, pse) = s.get(&(cfg.clone(), st.clone(), format!("{}-{}", base, m))).map_or((f64::NAN, f64::NAN), |x| (x.1, x.2));
        cells.insert((cfg.clone(), st.clone(), base.to_string()), Cell { count: cnt, measured: meas, ideal: a, paired: pm, paired_se: pse });
    }
    // |dmean| from the rows, by config and fixed stratum.
    // (config, stratum) -> (|dmean|, |ideal dmean|, their difference) by row.
    type Paired = (Vec<f64>, Vec<f64>, Vec<f64>);
    let mut acc: BTreeMap<(String, String), Paired> = BTreeMap::new();
    for r in rows {
        for st in [Some("all"), fixed_stratum(r.n_exact)].into_iter().flatten() {
            let e = acc.entry((r.config.clone(), st.to_string())).or_default();
            e.0.push(r.dmean.abs());
            e.1.push(r.ideal_dmean.abs());
            e.2.push(r.dmean.abs() - r.ideal_dmean.abs());
        }
    }
    for ((cfg, st), (a, i, d)) in acc {
        if !strata.contains(&st) {
            strata.push(st.clone());
        }
        let cnt = a.iter().filter(|x| !x.is_nan()).count() as f64;
        cells.insert(
            (cfg, st, "abs_dmean_L".into()),
            Cell {
                count: cnt,
                measured: nan_mean(a.into_iter()),
                ideal: nan_mean(i.into_iter()),
                paired: nan_mean(d.into_iter()),
                paired_se: f64::NAN,
            },
        );
    }
    Ok((cells, strata))
}

fn eval_labels(cells: &Cells) -> io::Result<Vec<Label>> {
    let mut v: Vec<Label> =
        cells.keys().map(|k| k.0.clone()).collect::<BTreeSet<_>>().iter().map(|s| label(s)).collect::<io::Result<_>>()?;
    sort_labels(&mut v);
    Ok(v)
}

fn write_ideal(out: &str, cells: &Cells, strata: &[String]) -> io::Result<()> {
    let labels = eval_labels(cells)?;
    let metrics: Vec<String> = cells.keys().map(|k| k.2.clone()).collect::<BTreeSet<_>>().into_iter().collect();
    let mut w = create(out, "ideal.tsv")?;
    writeln!(w, "config\tvariant\tk\tstratum\tmetric\tcount\tmeasured\tideal\texcess\tpaired\tpaired_block_se")?;
    for c in &labels {
        for st in strata {
            for m in &metrics {
                let Some(x) = cells.get(&(c.text.clone(), st.clone(), m.clone())) else { continue };
                writeln!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    c.text,
                    c.variant,
                    c.k,
                    st,
                    m,
                    x.count,
                    fmt(x.measured),
                    fmt(x.ideal),
                    fmt((x.measured - x.ideal) / x.ideal),
                    fmt(x.paired),
                    fmt(x.paired_se)
                )?;
            }
        }
    }
    w.flush()?;

    let mut w = create(out, "ideal_slopes.tsv")?;
    writeln!(w, "family\tstratum\tmetric\tk_from\tk_to\tpoints\tmeasured\tideal\texcess")?;
    for (fam, cs) in k_families(&labels).values() {
        if cs.len() < 2 {
            continue;
        }
        for st in strata {
            for m in &metrics {
                let cl: Vec<Option<&Cell>> = cs.iter().map(|c| cells.get(&(c.text.clone(), st.clone(), m.clone()))).collect();
                if cl.iter().any(|x| x.is_none()) {
                    continue;
                }
                let cl: Vec<&Cell> = cl.into_iter().flatten().collect();
                let k: Vec<f64> = cs.iter().map(|c| c.k as f64).collect();
                let (sm, n) = loglog(&k, &cl.iter().map(|c| c.measured).collect::<Vec<_>>());
                let (si, _) = loglog(&k, &cl.iter().map(|c| c.ideal).collect::<Vec<_>>());
                let e: Vec<f64> = cl.iter().map(|c| c.paired).collect();
                let se = if e.iter().all(|x| *x > 0.0) { loglog(&k, &e).0 } else { f64::NAN };
                writeln!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    fam,
                    st,
                    m,
                    cs[0].k,
                    cs[cs.len() - 1].k,
                    n,
                    fmt(sm),
                    fmt(si),
                    fmt(se)
                )?;
            }
        }
    }
    w.flush()
}

// ------------------------------------------------------------------ floors

/// The participation ratio (4th column) of every wanted transaction, from a
/// moments file: `.zst` through `zstd -dc`, anything else as text.  Stops
/// as soon as every wanted id is found.  The first line of an id wins.
fn read_pr(path: &str, wanted: &BTreeSet<u64>) -> io::Result<HashMap<u64, f64>> {
    let max = wanted.iter().next_back().copied().unwrap_or(0);
    let mut bits = vec![0u64; (max / 64 + 1) as usize];
    for &id in wanted {
        bits[(id / 64) as usize] |= 1 << (id % 64);
    }
    let (mut child, src): (Option<std::process::Child>, Box<dyn Read>) = if path.ends_with(".zst") {
        let mut ch = Command::new("zstd")
            .args(["-dc", "--", path])
            .stdout(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("cannot run zstd -dc {}: {}", path, e)))?;
        let out = ch.stdout.take().expect("piped");
        (Some(ch), Box::new(out))
    } else {
        (None, Box::new(File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?))
    };
    let mut r = BufReader::with_capacity(1 << 22, src);
    let mut pr = HashMap::with_capacity(wanted.len());
    let mut line = Vec::new();
    let mut ended = false;
    while pr.len() < wanted.len() {
        line.clear();
        if r.read_until(b'\n', &mut line)? == 0 {
            ended = true;
            break;
        }
        let mut id = 0u64;
        let mut digits = 0;
        for &b in &line {
            if b.is_ascii_digit() {
                id = id * 10 + (b - b'0') as u64;
                digits += 1;
            } else {
                break;
            }
        }
        if digits == 0 || id > max || bits[(id / 64) as usize] >> (id % 64) & 1 == 0 || pr.contains_key(&id) {
            continue;
        }
        let text = std::str::from_utf8(&line).map_err(|_| bad(format!("{}: not text", path)))?;
        let f = text.trim_end().split('\t').nth(3).ok_or_else(|| bad(format!("{}: no 4th field in {:?}", path, text)))?;
        pr.insert(id, num(f)?);
    }
    if let Some(ch) = child.as_mut() {
        if !ended {
            let _ = ch.kill();
        }
        let st = ch.wait()?;
        if ended && !st.success() {
            return Err(bad(format!("zstd -dc {} exited with {}", path, st)));
        }
    }
    if pr.len() < wanted.len() {
        let miss = wanted.iter().find(|i| !pr.contains_key(i)).copied().unwrap_or(0);
        return Err(bad(format!("{}: {} of {} ids missing, e.g. {}", path, wanted.len() - pr.len(), wanted.len(), miss)));
    }
    Ok(pr)
}

/// The single-shot lower bound on TV1024: `1 - 1/(PR tau)` on a reduced row,
/// clipped to `[0, 1]`, with `tau = 1/n` where the row's is `0`.
pub fn floor_lb(pr: f64, tau: f64, n: f64, reduced: bool) -> f64 {
    if !reduced {
        return 0.0;
    }
    let te = if tau > 0.0 { tau } else { 1.0 / n.max(1.0) };
    (1.0 - 1.0 / (pr * te)).clamp(0.0, 1.0)
}

/// The same bound with the row's own `tau`, `0` where it is `0`.
pub fn floor_lb_own_tau(pr: f64, tau: f64) -> f64 {
    if tau > 0.0 {
        (1.0 - 1.0 / (pr * tau)).max(0.0)
    } else {
        0.0
    }
}

/// `E[ideal TV1024 | log10(PR/k)]`: bin centres and means.
#[derive(Clone, Debug)]
pub struct Curve {
    pub centre: Vec<f64>,
    pub mean: Vec<f64>,
}

const EDGE_LO: f64 = -4.0;
const EDGE_STEP: f64 = 0.125;
const EDGES: usize = 49;

impl Curve {
    /// Bins of 1/8 decade over `[-4, 2)`, kept with at least 20 points
    /// (`np.digitize` on `np.arange(-4, 2.01, 0.125)`).
    pub fn fit(lx: &[f64], y: &[f64]) -> Curve {
        let edge = |i: usize| EDGE_LO + EDGE_STEP * i as f64;
        let mut bins: BTreeMap<usize, Vec<f64>> = BTreeMap::new();
        for (&x, &v) in lx.iter().zip(y) {
            let i = (0..EDGES).take_while(|&j| edge(j) <= x).count();
            bins.entry(i).or_default().push(v);
        }
        let (mut centre, mut mean) = (Vec::new(), Vec::new());
        for (i, v) in bins {
            if v.len() >= 20 && i > 0 && i < EDGES {
                centre.push(0.5 * (edge(i - 1) + edge(i)));
                mean.push(np_mean(&v));
            }
        }
        Curve { centre, mean }
    }

    /// The curve at `x = PR/k`: 0 left of the first bin, linear between
    /// centres (`np.interp`), `1 - (1 - g0) x0 / x` right of the last.
    pub fn at(&self, x: f64) -> f64 {
        let (c, m) = (&self.centre, &self.mean);
        if c.is_empty() {
            return f64::NAN;
        }
        let lx = x.max(1e-12).log10();
        let last = c.len() - 1;
        if lx > c[last] {
            return 1.0 - (1.0 - m[last]) * 10f64.powf(c[last]) / x;
        }
        if lx < c[0] {
            return 0.0;
        }
        let j = c.partition_point(|&v| v <= lx).saturating_sub(1);
        if j == last {
            return m[last];
        }
        let slope = (m[j + 1] - m[j]) / (c[j + 1] - c[j]);
        slope * (lx - c[j]) + m[j]
    }
}

fn floors(out: &str, moments: &str, eval: &[EvalRow], chain: &[FloorRows], stretch: u64) -> io::Result<()> {
    let mut wanted: BTreeSet<u64> = eval.iter().map(|r| r.tx).collect();
    for c in chain {
        wanted.extend(c.tx.iter().copied());
    }
    eprintln!("summarize: reading the participation ratio of {} transactions from {}", wanted.len(), moments);
    let pr = read_pr(moments, &wanted)?;

    let lab: HashMap<&str, Label> = eval
        .iter()
        .map(|r| r.config.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|c| -> io::Result<(&str, Label)> { Ok((c, label(c)?)) })
        .collect::<io::Result<_>>()?;
    // The calibration rows: reduced, seed 1, record mode, not the harmonic cap.
    let cal = |r: &EvalRow| r.reduced && r.config.contains("-s1-rec") && !r.config.contains("harm");
    let mut curves: BTreeMap<String, Curve> = BTreeMap::new();
    let variants: BTreeSet<String> = lab.values().map(|l| l.variant.clone()).collect();
    let mut cw = create(out, "floor_curve.tsv")?;
    writeln!(cw, "variant\tlog10_x\tideal_tv1024")?;
    for v in &variants {
        let rows: Vec<&EvalRow> = eval.iter().filter(|r| cal(r) && lab[r.config.as_str()].variant == *v).collect();
        let lx: Vec<f64> = rows.iter().map(|r| (pr[&r.tx] / lab[r.config.as_str()].k as f64).log10()).collect();
        let y: Vec<f64> = rows.iter().map(|r| r.ideal_tv).collect();
        let c = Curve::fit(&lx, &y);
        for (a, b) in c.centre.iter().zip(&c.mean) {
            writeln!(cw, "{}\t{}\t{}", v, fmt(*a), fmt(*b))?;
        }
        curves.insert(v.clone(), c);
    }
    cw.flush()?;
    // r_k = mean single-shot KS / mean tau, varopt, rows with tau > 0.
    let mut ksr: BTreeMap<u64, f64> = BTreeMap::new();
    let mut by_k: BTreeMap<u64, (Vec<f64>, Vec<f64>)> = BTreeMap::new();
    for r in eval.iter().filter(|r| cal(r) && lab[r.config.as_str()].variant == "varopt" && r.tau > 0.0) {
        let e = by_k.entry(lab[r.config.as_str()].k).or_default();
        e.0.push(r.ideal_ks);
        e.1.push(r.tau);
    }
    for (k, (a, t)) in by_k {
        ksr.insert(k, nan_mean(a.into_iter()) / nan_mean(t.into_iter()));
    }

    let mut configs: Vec<&Label> = lab.values().collect();
    configs.sort_by_key(|l| l.key());
    let mut w = create(out, "floors_eval.tsv")?;
    writeln!(
        w,
        "config\tvariant\tk\tn_red\ttv\tideal_tv\tfloor_lb\tfloor_lb_own_tau\tfloor_cal\tideal_over_tv\tks\tideal_ks\ttau_mean\t\
         ideal_ks_over_tau\tks_over_tau\tw1\tideal_w1\tideal_w1_over_w1"
    )?;
    for l in configs {
        let rows: Vec<&EvalRow> = eval.iter().filter(|r| r.config == l.text && r.reduced).collect();
        let vo = l.variant == "varopt";
        let curve = &curves[&l.variant];
        let m = |f: &dyn Fn(&EvalRow) -> f64| nan_mean(rows.iter().map(|r| f(r)));
        let lb = m(&|r| floor_lb(pr[&r.tx], if vo { r.tau } else { 0.0 }, r.n_sketch, true));
        let lbo = if vo { m(&|r| floor_lb_own_tau(pr[&r.tx], r.tau)) } else { f64::NAN };
        let cf = m(&|r| curve.at(pr[&r.tx] / l.k as f64));
        let (tv, itv, ks, iks, w1, iw1) = (m(&|r| r.tv), m(&|r| r.ideal_tv), m(&|r| r.ks), m(&|r| r.ideal_ks), m(&|r| r.w1), m(&|r| r.ideal_w1));
        let tp: Vec<&&EvalRow> = rows.iter().filter(|r| r.tau > 0.0).collect();
        let (taum, kst) = if vo {
            let t = nan_mean(tp.iter().map(|r| r.tau));
            (t, nan_mean(tp.iter().map(|r| r.ks)) / t)
        } else {
            (f64::NAN, f64::NAN)
        };
        let r_k = if vo { ksr.get(&l.k).copied().unwrap_or(f64::NAN) } else { f64::NAN };
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            l.text,
            l.variant,
            l.k,
            rows.len(),
            fmt(tv),
            fmt(itv),
            fmt(lb),
            fmt(lbo),
            fmt(cf),
            fmt(itv / tv),
            fmt(ks),
            fmt(iks),
            fmt(taum),
            fmt(r_k),
            fmt(kst),
            fmt(w1),
            fmt(iw1),
            fmt(iw1 / w1)
        )?;
    }
    w.flush()?;

    let mut w = create(out, "floors_chain.tsv")?;
    writeln!(
        w,
        "config\tvariant\tk\tgroup\tn_red\ttv\tfloor_lb\tfloor_lb_own_tau\tfloor_cal\texcess_cal\tfloor_frac_lb\tfloor_frac_lb_own_tau\t\
         floor_frac_cal\tpr_median\tx_median\tband_ks\ttau_mean\tband_ks_over_tau\tks_floor\tks_floor_frac"
    )?;
    for c in chain {
        let l = &c.label;
        let vo = l.variant == "varopt";
        let Some(curve) = curves.get(&l.variant) else { continue };
        let p: Vec<f64> = c.tx.iter().map(|t| pr[t]).collect();
        for g in groups(&c.rec, stretch, 0) {
            let idx: Vec<usize> = (g.lo..g.hi).filter(|&i| c.reduced[i]).collect();
            if idx.is_empty() {
                continue;
            }
            let mean = |f: &dyn Fn(usize) -> f64| np_mean(&idx.iter().map(|&i| f(i)).collect::<Vec<_>>());
            let tv = mean(&|i| c.tv[i]);
            let lb = mean(&|i| floor_lb(p[i], if vo { c.tau[i] } else { 0.0 }, c.n[i], true));
            let lbo = if vo { mean(&|i| floor_lb_own_tau(p[i], c.tau[i])) } else { f64::NAN };
            let cf = mean(&|i| curve.at(p[i] / l.k as f64));
            let mut pm: Vec<f64> = idx.iter().map(|&i| p[i]).collect();
            let mut xm: Vec<f64> = idx.iter().map(|&i| p[i] / l.k as f64).collect();
            let ks = mean(&|i| c.band_ks[i]);
            let (taum, kst, ksf, ksff) = if vo {
                let tp: Vec<usize> = idx.iter().copied().filter(|&i| c.tau[i] > 0.0).collect();
                let t = np_mean(&tp.iter().map(|&i| c.tau[i]).collect::<Vec<_>>());
                let k = np_mean(&tp.iter().map(|&i| c.band_ks[i]).collect::<Vec<_>>());
                let r = ksr.get(&l.k).copied().unwrap_or(f64::NAN);
                (t, k / t, r * t, r * t / k)
            } else {
                (f64::NAN, f64::NAN, f64::NAN, f64::NAN)
            };
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                l.text,
                l.variant,
                l.k,
                g.name,
                idx.len(),
                fmt(tv),
                fmt(lb),
                fmt(lbo),
                fmt(cf),
                fmt(tv - cf),
                fmt(lb / tv),
                fmt(lbo / tv),
                fmt(cf / tv),
                fmt(np_median(&mut pm)),
                fmt(np_median(&mut xm)),
                fmt(ks),
                fmt(taum),
                fmt(kst),
                fmt(ksf),
                fmt(ksff)
            )?;
        }
    }
    w.flush()
}

// ------------------------------------------------------------------ run

pub fn run(opts: SummarizeOpts) -> io::Result<()> {
    std::fs::create_dir_all(&opts.out).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", opts.out, e)))?;
    let want_floors = opts.moments.is_some();
    let (stats, floor_rows) = load_chain(&opts, want_floors)?;
    if !opts.chain.is_empty() {
        write_stats(&opts.out, &stats)?;
        let ix = Index::new(&stats);
        write_errors(&opts.out, &ix)?;
        write_slopes(&opts.out, &ix)?;
        let buckets = load_buckets(&opts.chain)?;
        write_depth(&opts.out, &ix, &buckets, opts.stretch)?;
        write_bias(&opts.out, &buckets, opts.stretch)?;
        write_bias_buckets(&opts.out, &opts.chain)?;
        write_step(&opts.out, &ix)?;
        write_cost(&opts.out, &opts.chain)?;
    }
    if let Some(dir) = &opts.eval {
        let rows = load_eval_rows(dir)?;
        let (cells, strata) = ideal_cells(dir, &rows)?;
        write_ideal(&opts.out, &cells, &strata)?;
        if let Some(m) = &opts.moments {
            floors(&opts.out, m, &rows, &floor_rows, opts.stretch)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_are_numpys() {
        // np.sum of 1/(i+1)^2 + 1e8 (i < 20000) and of 0.1 * i (i < 300):
        // values printed by NumPy 1.26 as float.hex().
        let a: Vec<f64> = (0..20000).map(|i| 1.0 / ((i + 1) as f64).powi(2) + if i % 3 == 0 { 1e8 } else { 0.0 }).collect();
        assert_eq!(np_sum(&a), f64::from_bits(0x4263674d556034a2), "{:x}", np_sum(&a).to_bits());
        let b: Vec<f64> = (0..300).map(|i| 0.1 * i as f64).collect();
        assert_eq!(np_sum(&b), f64::from_bits(0x40b1850000000000), "{:x}", np_sum(&b).to_bits());
        assert_eq!(np_sum(&[]), 0.0);
        assert_eq!(np_mean(&[1.0, 2.0, 4.0]), 7.0 / 3.0);
    }

    #[test]
    fn quantiles_are_numpys_linear() {
        // np.quantile([...], q) for these q, NumPy 1.26.
        let v = [3.0, 1.0, 4.0, 1.0, 5.0, 9.0, 2.0, 6.0, 5.0, 3.0, 5.0];
        for (q, want) in [(0.5, 4.0), (0.9, 6.0), (0.99, 8.700000000000001), (0.1, 1.0), (0.25, 2.5), (1.0, 9.0), (0.0, 1.0)] {
            let mut x = v.to_vec();
            assert_eq!(quantile_linear(&mut x, q), want, "q = {}", q);
        }
        let mut x = vec![0.1, 0.7, 0.2];
        assert_eq!(quantile_linear(&mut x, 0.9), 0.6);
        assert!(quantile_linear(&mut [], 0.5).is_nan());
        let mut e = vec![4.0, 1.0, 3.0, 2.0];
        assert_eq!(np_median(&mut e), 2.5);
        let mut o = vec![4.0, 1.0, 3.0];
        assert_eq!(np_median(&mut o), 3.0);
    }

    #[test]
    fn stat_is_count_mean_rms_and_quantiles() {
        let s = stat(vec![3.0, 4.0]).unwrap();
        assert_eq!(s.count, 2);
        assert_eq!(s.mean, 3.5);
        assert_eq!(s.rms, 12.5f64.sqrt());
        assert_eq!(s.p50, 3.5);
        assert!(stat(Vec::new()).is_none());
    }

    #[test]
    fn slopes_and_t() {
        let k: [f64; 4] = [16.0, 32.0, 64.0, 128.0];
        let y: Vec<f64> = k.iter().map(|k| 3.0 * k.powf(-0.9)).collect();
        let (s, n) = loglog(&k, &y);
        assert!((s + 0.9).abs() < 1e-12 && n == 4);
        // Non-positive points are left out, as the scripts' fits did.
        let (s, n) = loglog(&k, &[y[0], -1.0, y[2], f64::NAN]);
        assert!((s + 0.9).abs() < 1e-12 && n == 2);
        // t_{0.975, 2} = 4.302652729911275 and the two-sided p of t = 1 at 2 df
        // is 1 - 1/sqrt(3).
        assert!((t_quantile(0.975, 2.0) - 4.302652729911275).abs() < 1e-9);
        assert!((t_p(1.0, 2.0) - (1.0 - 1.0 / 3f64.sqrt())).abs() < 1e-12);
        let (m, sd) = mean_sd(&[1.0, 2.0, 6.0]);
        assert_eq!(m, 3.0);
        assert!((sd - 7f64.sqrt()).abs() < 1e-12);
    }

    #[test]
    fn labels_parse_and_order() {
        let l = Label::parse("coord-k64-s1-rec-min-f0").unwrap();
        assert_eq!((l.variant.as_str(), l.k, l.seed, l.reduce.as_str(), l.rest.as_str()), ("coord", 64, 1, "rec", "min-f0"));
        assert_eq!(l.k_family(), "coord-k*-s1-rec-min-f0");
        assert_eq!(l.varopt_ref(), "varopt-k64-s1-rec");
        assert!(Label::parse("rows").is_none());
        let mut v: Vec<Label> = ["prio-k64-s1-rec", "varopt-k128-s1-rec", "varopt-k16-s2-rec", "varopt-k16-s1-rec", "varopt-k16-s1-step"]
            .iter()
            .map(|s| Label::parse(s).unwrap())
            .collect();
        sort_labels(&mut v);
        let t: Vec<&str> = v.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(t, ["varopt-k16-s1-rec", "varopt-k16-s2-rec", "varopt-k128-s1-rec", "varopt-k16-s1-step", "prio-k64-s1-rec"]);
    }

    #[test]
    fn groups_are_stretches_of_the_rows() {
        let rec: Vec<u64> = (0..25).map(|i| i * 4).collect(); // 0 .. 96
        let g = groups(&rec, 30, 2);
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["pooled", "S0", "S1", "S2", "S3", "S0-1"]);
        assert_eq!((g[1].lo, g[1].hi), (0, 8)); // 0..28
        assert_eq!((g[4].from, g[4].to, g[4].lo, g[4].hi), (90, 97, 23, 25));
        assert_eq!((g[5].to, g[5].hi), (60, 15));
        assert_eq!(groups(&rec, 30, 0).len(), 5);
    }

    #[test]
    fn metrics_follow_the_scripts() {
        let r = Rows {
            label: Label::parse("varopt-k16-s1-rec").unwrap(),
            rec: vec![0, 256],
            tx: vec![0, 256],
            n: vec![1.0, 16.0],
            tau: vec![0.0, 0.1],
            reduced: vec![false, true],
            dmean: vec![-0.002, 0.004],
            dsd: vec![0.0, -0.001],
            dlap: vec![[0.0; 7], [0.1, -0.3, 0.2, 0.0, 0.0, 0.0, f64::NAN]],
            ref_sd: vec![0.5 / L, 0.2],
            tv: vec![0.0, 0.5],
            band_ks: vec![0.0, 0.1],
            band_w1: vec![0.0, 0.01],
        };
        let sd = Metric::AbsDmeanOverSd.values(&r);
        assert!(sd[0].is_nan() && (sd[1] - 0.02).abs() < 1e-15);
        let mx = Metric::DlapMax.values(&r);
        assert_eq!(mx[0], 0.0);
        assert!(mx[1].is_nan(), "a NaN Laplace error makes the row's max NaN, as np.max does");
        let st = rows_stats(&r, 100, 3);
        let fe = st.iter().find(|s| s.metric == "frac_exact" && s.group.name == "pooled").unwrap();
        assert_eq!(fe.stat.mean, 0.5);
        let red = st.iter().find(|s| s.metric == "abs_dmean_L" && s.subset == "reduced" && s.group.name == "pooled").unwrap();
        assert_eq!((red.stat.count, red.stat.mean), (1, 0.004));
    }

    #[test]
    fn floors_follow_the_scripts() {
        // tau = 0 (not reduced at its own record): 1/n stands in for it.
        assert_eq!(floor_lb(100.0, 0.0, 16.0, true), 0.84);
        assert_eq!(floor_lb(10.0, 0.5, 16.0, true), 0.8);
        assert_eq!(floor_lb(10.0, 0.5, 16.0, false), 0.0);
        assert_eq!(floor_lb(1.0, 0.5, 16.0, true), 0.0);
        assert_eq!(floor_lb_own_tau(10.0, 0.0), 0.0);
        assert_eq!(floor_lb_own_tau(10.0, 0.5), 0.8);
        // A curve of two bins, 20 points each, at log10 x = -0.0625 and 0.0625.
        let lx: Vec<f64> = (0..40).map(|i| if i < 20 { -0.1 } else { 0.1 }).collect();
        let y: Vec<f64> = (0..40).map(|i| if i < 20 { 0.2 } else { 0.4 }).collect();
        let c = Curve::fit(&lx, &y);
        assert_eq!(c.centre, vec![-0.0625, 0.0625]);
        assert!((c.mean[0] - 0.2).abs() < 1e-15 && (c.mean[1] - 0.4).abs() < 1e-15);
        assert_eq!(c.at(10f64.powf(-0.5)), 0.0);
        assert!((c.at(1.0) - 0.3).abs() < 1e-12);
        let x = 10.0;
        assert!((c.at(x) - (1.0 - (1.0 - c.mean[1]) * 10f64.powf(0.0625) / x)).abs() < 1e-12);
        // Bins with fewer than 20 points are dropped.
        assert!(Curve::fit(&lx[..19], &y[..19]).centre.is_empty());
    }

    #[test]
    fn strata_are_evals() {
        assert_eq!(fixed_stratum(256.0), Some("fix:<=256"));
        assert_eq!(fixed_stratum(257.0), Some("fix:(256,4096]"));
        assert_eq!(fixed_stratum(32769.0), Some("fix:>32768"));
        assert_eq!(fixed_stratum(0.0), None);
    }

    /// The rows of a config whose `|dmean|` is `scale` times 1, 2, 3, ...: a
    /// reduced row every other record, every row's exact spread 0.1.
    fn rows_file(dir: &str, label: &str, scale: f64) {
        let mut s = String::from("rec\ttx_id\tn\ttau\treduced\tmass_raw\tdmean_L\tdsd_L");
        for l in LAPS {
            s.push_str(&format!("\tdlap_{}", l));
        }
        s.push_str("\tref_mean_L\tref_sd_L\ttv1024\tband_ks\tband_w1_L\n");
        for i in 0..8u64 {
            let d = scale * (i + 1) as f64 * if i % 2 == 0 { 1.0 } else { -1.0 };
            s.push_str(&format!("{}\t{}\t16\t0.0625\t{}\t0\t{}\t{}", i * 256, i * 256 + 1, (i % 2 == 1) as u8, d, d / 2.0));
            for _ in LAPS {
                s.push_str(&format!("\t{}", d / 4.0));
            }
            s.push_str("\t0.5\t0.1\t0.5\tNA\t0.01\n");
        }
        std::fs::write(format!("{}/rows.{}.tsv", dir, label), s).unwrap();
    }

    fn table(path: &str) -> Vec<HashMap<String, String>> {
        let text = std::fs::read_to_string(path).unwrap();
        let mut lines = text.lines();
        let head: Vec<&str> = lines.next().unwrap().split('\t').collect();
        lines.map(|l| head.iter().map(|h| h.to_string()).zip(l.split('\t').map(String::from)).collect()).collect()
    }

    fn find<'a>(t: &'a [HashMap<String, String>], want: &[(&str, &str)]) -> &'a HashMap<String, String> {
        t.iter().find(|r| want.iter().all(|(k, v)| r[*k] == *v)).unwrap_or_else(|| panic!("no row {:?}", want))
    }

    fn val(r: &HashMap<String, String>, k: &str) -> f64 {
        r[k].parse().unwrap()
    }

    #[test]
    fn a_small_sweep_end_to_end() {
        let base = std::env::temp_dir().join(format!("sketch-summarize-test-{}", std::process::id()));
        let (chain, eval, out) = (base.join("chain"), base.join("eval"), base.join("out"));
        for d in [&chain, &eval] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (c, e, o) = (chain.to_str().unwrap(), eval.to_str().unwrap(), out.to_str().unwrap());
        rows_file(c, "varopt-k16-s1-rec", 1e-3);
        rows_file(c, "varopt-k16-s2-rec", 2e-3);
        rows_file(c, "varopt-k64-s1-rec", 0.25e-3);
        rows_file(c, "varopt-k16-s1-step", 3e-3);
        // Two buckets of 1024 records; signed means of seeds 1 and 2, and their |.|.
        let mut b = String::from("config\tbucket\trec_from\trec_to\tmetric\tsubset\tcount\tmean\n");
        for (cfg, m0, m1) in [("varopt-k16-s1-rec", 1.0, -3.0), ("varopt-k16-s2-rec", 2.0, 2.0)] {
            for (i, m) in [m0, m1].into_iter().enumerate() {
                let (from, to) = (i * 1024, i * 1024 + 1023);
                b.push_str(&format!("{}\t{}\t{}\t{}\tdmean_L\tsigned\t{}\t{}\n", cfg, i, from, to, 1 + i, m));
                b.push_str(&format!("{}\t{}\t{}\t{}\tabs_dmean_L\tall\t{}\t10\n", cfg, i, from, to, 1 + i));
            }
        }
        std::fs::write(chain.join("buckets.tsv"), b).unwrap();
        std::fs::write(chain.join("seeds.tsv"), "family\tbucket\tmetric\tsubset\tseeds\tz\nvaropt-k16-s*-rec\t0\tdmean_L\tsigned\t3\t5\nvaropt-k16-s*-rec\t1\tdmean_L\tsigned\t3\t-1\n").unwrap();
        std::fs::write(
            chain.join("cost.tsv"),
            "config\tbusy_s\trecords\tpeak_entries\tpeak_sketches\treductions\toff_by_one\tvm_hwm\n\
             varopt-k16-s1-rec\t10.000\t2048\t1\t1\t100\t0\tNA\nvaropt-k16-s1-step\t9.000\t2048\t1\t1\t600\t0\tNA\n",
        )
        .unwrap();
        std::fs::write(
            eval.join("rows.tsv"),
            "rec\ttx_id\tconfig\tn_exact\tn_sketch\treduced\ttau\tks\tw1_L\ttv1024\tdmean_L\tideal_ks\tideal_w1_L\tideal_tv1024\tideal_dmean_L\n\
             0\t1\tvaropt-k16-s1-rec\t40000\t16\t1\t0.0625\t0.1\t0.2\t0.3\t-0.004\t0.05\t0.1\t0.3\t0.002\n\
             0\t1\tvaropt-k16-s1-rec\t100\t16\t1\t0.0625\t0.1\t0.2\t0.3\t0.001\t0.05\t0.1\t0.3\t0.001\n",
        )
        .unwrap();
        std::fs::write(
            eval.join("summary.tsv"),
            "config\tstratum\tmetric\tcount\tmean\tblock_se\n\
             varopt-k16-s1-rec\tall\tks\t2\t0.1\tNA\nvaropt-k16-s1-rec\tall\tideal_ks\t2\t0.05\tNA\n\
             varopt-k16-s1-rec\tall\tks-ideal_ks\t2\t0.05\t0.01\n",
        )
        .unwrap();
        run(SummarizeOpts {
            chain: vec![c.into()],
            eval: Some(e.into()),
            moments: None,
            out: o.into(),
            stretch: 1024,
            head: 0,
            threads: 3,
        })
        .unwrap();

        // Rows: |dmean| = 1e-3 * (1..8); reduced rows are the even counts.
        let st = table(&format!("{}/stats.tsv", o));
        let r = find(&st, &[("config", "varopt-k16-s1-rec"), ("group", "pooled"), ("subset", "reduced"), ("metric", "abs_dmean_L")]);
        assert_eq!(val(r, "count"), 4.0);
        assert!((val(r, "mean") - 5e-3).abs() < 1e-15);
        assert!((val(r, "p90") - 7.4e-3).abs() < 1e-15, "linear interpolation between 6e-3 and 8e-3");
        let r = find(&st, &[("config", "varopt-k16-s1-rec"), ("group", "S1"), ("subset", "all"), ("metric", "abs_dmean_L")]);
        assert_eq!((r["rec_from"].as_str(), r["rec_to"].as_str(), val(r, "count")), ("1024", "1793", 4.0));
        assert!(!st.iter().any(|r| r["metric"] == "band_ks"), "an all-NA metric has no line");
        // |dmean|/sd = |dmean| / 0.1; errors.tsv converts to blocks.
        let er = table(&format!("{}/errors.tsv", o));
        let r = find(&er, &[("config", "varopt-k64-s1-rec"), ("group", "pooled")]);
        assert!((val(r, "mean_abs_dmean_blocks") - 1.25e-3 * L).abs() < 1e-9);
        assert!((val(r, "k_times_mean_abs_dmean_over_sd") - 64.0 * 1.25e-2).abs() < 1e-12);
        assert!((val(r, "abs_dmean_over_varopt") - 1.0).abs() < 1e-15);
        // Slope from k = 16 to 64: the error falls by 4, so -1.
        let sl = table(&format!("{}/slopes.tsv", o));
        let r = find(&sl, &[("family", "varopt-k*-s1-rec"), ("group", "pooled"), ("metric", "abs_dmean_L")]);
        assert!((val(r, "slope") + 1.0).abs() < 1e-12);
        // Step against the mean of seeds 1 and 2: 3 / 1.5.
        let sp = table(&format!("{}/step.tsv", o));
        let r = find(&sp, &[("config", "varopt-k16-s1-step"), ("group", "pooled"), ("metric", "abs_dmean_L")]);
        assert_eq!(r["rec_seeds"], "2");
        assert!((val(r, "step_over_rec") - 2.0).abs() < 1e-12);
        // Bias: seed 1's count-weighted signed mean is (1 - 6)/3, seed 2's is 2.
        let bi = table(&format!("{}/bias.tsv", o));
        let r = find(&bi, &[("family", "varopt-k16-s*-rec"), ("group", "pooled"), ("metric", "dmean_L")]);
        let v = [-5.0 / 3.0, 2.0];
        let (mu, sd) = mean_sd(&v);
        assert!((val(r, "mean") - mu).abs() < 1e-12 && (val(r, "seed_sd") - sd).abs() < 1e-12);
        assert!((val(r, "t") - mu / (sd / 2f64.sqrt())).abs() < 1e-9);
        assert!((val(r, "bound_over_abs") - (mu.abs() + val(r, "t_crit") * sd / 2f64.sqrt()) / 10.0).abs() < 1e-9);
        let r = find(&bi, &[("family", "varopt-k16-s*-rec"), ("group", "S1"), ("metric", "dmean_L")]);
        assert!((val(r, "mean") - (-0.5)).abs() < 1e-12);
        let sg = table(&format!("{}/signed.tsv", o));
        let r = find(&sg, &[("config", "varopt-k16-s1-rec"), ("group", "pooled"), ("metric", "dmean_L")]);
        assert!((val(r, "bucket_rms") - 5f64.sqrt()).abs() < 1e-12 && val(r, "bucket_max_abs") == 3.0);
        let bb = table(&format!("{}/bias_buckets.tsv", o));
        assert_eq!(find(&bb, &[("metric", "dmean_L")])["rejected"], "1");
        let co = table(&format!("{}/cost.tsv", o));
        let r = find(&co, &[("config", "varopt-k16-s1-step")]);
        assert!((val(r, "reductions_over_reference") - 6.0).abs() < 1e-12 && (val(r, "busy_over_reference") - 0.9).abs() < 1e-12);
        // eval: the summary's ks and ideal_ks, and |dmean| from the rows.
        let id = table(&format!("{}/ideal.tsv", o));
        let r = find(&id, &[("stratum", "all"), ("metric", "ks")]);
        assert!((val(r, "excess") - 1.0).abs() < 1e-12 && val(r, "paired_block_se") == 0.01);
        let r = find(&id, &[("stratum", "fix:>32768"), ("metric", "abs_dmean_L")]);
        assert!((val(r, "measured") - 0.004).abs() < 1e-15 && (val(r, "excess") - 1.0).abs() < 1e-12);
        let r = find(&id, &[("stratum", "all"), ("metric", "abs_dmean_L")]);
        assert!((val(r, "ideal") - 0.0015).abs() < 1e-15);
        std::fs::remove_dir_all(&base).unwrap();
    }
}
