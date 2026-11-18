//! # How far an estimate is from the colour
//!
//! Every metric compares two distributions over block ids, both ascending and
//! summing to one: the estimate `Q` a sketch emits and the exact colour `P`
//! (or, in `chain` mode, the exact summaries of `P` on disk).  `L` is the
//! 2022 chain's block count, so errors divided by it are fractions of the
//! chain and mean the same thing at every record.
//!
//! What each one is for:
//!
//! - **mean** and **sd** -- where the coins came from and how spread out, in
//!   blocks.  Two-pass, so the spread does not lose digits to cancellation.
//! - **Laplace** `sum_b p_b exp(-s b / L)` at the seven `s` of
//!   [`crate::bands::LAPLACE`] -- the same transform `--laplace` prints, from
//!   [`crate::bands::laplace_of`], the very table that wrote
//!   `colors.laplace.zst`, so the exact values agree bit for bit.
//! - **KS** -- the largest gap between the two CDFs.  A `k`-point sketch of a
//!   diffuse colour has a sawtooth CDF error of amplitude about `1/k` even when
//!   it is perfect.  For `varopt` the error at a cut is `tau * (floor(U + S) -
//!   S)`, in `(tau * (U - 1), tau * U]`, where `S` is the light mass up to the
//!   cut in units of `tau`; over the many cuts of a diffuse colour its largest
//!   size is about `tau * max(U, 1 - U)`, whose mean over the offset `U` is
//!   `3*tau/4`.  So KS has a floor of about `3*tau/4` -- measured at 0.741 to
//!   0.750 `tau` on the exact colours of the first 3M records, `k` = 16 to
//!   4096 -- and the IDEAL columns report that floor next to it.
//! - **W1/L** -- the area between the CDFs: how many blocks of history the
//!   estimate misplaces, as a fraction of the chain.  No `1/k` floor.
//! - **TV1024** -- total variation after binning both into the 1024 bands of
//!   745 blocks `--bands 1024` uses, which is what `colors.bands1024.zst`
//!   holds exactly for every transaction; band-edge KS and binned W1 are the
//!   other two read off the same bins.
//! - **top32_l1** -- the L1 error on the 32 heaviest exact blocks.
//!
//! The quantiles of `chain` mode come from a fixed log histogram, so a bucket
//! of ten million records is a few kilobytes of counts and two runs of the
//! same records produce the same bins exactly.

use crate::bands::{laplace_of, CHAIN_BLOCKS, LAPLACE};

pub const L: f64 = CHAIN_BLOCKS as f64;
pub const BANDS: usize = 1024;
pub const BAND_WIDTH: u32 = 745;

/// The band of a block, as `--bands 1024` lays them out.
#[inline]
pub fn band(b: u32) -> usize {
    ((b / BAND_WIDTH) as usize).min(BANDS - 1)
}

/// `(mean, sd)` in blocks, two-pass, normalised by the sum of `p`.
pub fn moments(p: &[(u32, f64)]) -> (f64, f64) {
    let total: f64 = p.iter().map(|x| x.1).sum();
    if total <= 0.0 {
        return (0.0, 0.0);
    }
    let mean = p.iter().map(|&(b, w)| w * b as f64).sum::<f64>() / total;
    let var = p.iter().map(|&(b, w)| w * (b as f64 - mean).powi(2)).sum::<f64>() / total;
    (mean, var.max(0.0).sqrt())
}

/// The seven Laplace values, normalised by the sum of `p`.
pub fn laplace(p: &[(u32, f64)]) -> [f64; 7] {
    let mut out = [0.0; 7];
    let mut total = 0.0;
    for &(b, w) in p {
        total += w;
        let at = laplace_of(b as usize);
        for (o, a) in out.iter_mut().zip(at) {
            *o += w * a;
        }
    }
    if total > 0.0 {
        for o in &mut out {
            *o /= total;
        }
    }
    out
}

/// `(KS, W1/L, TV1024)` between `p` and `q`, both ascending and normalised.
pub fn distances(p: &[(u32, f64)], q: &[(u32, f64)]) -> (f64, f64, f64) {
    let (mut i, mut j) = (0, 0);
    let (mut fp, mut fq) = (0.0f64, 0.0f64);
    let (mut ks, mut w1) = (0.0f64, 0.0f64);
    let mut last: Option<u32> = None;
    while i < p.len() || j < q.len() {
        let bp = p.get(i).map_or(u32::MAX, |x| x.0);
        let bq = q.get(j).map_or(u32::MAX, |x| x.0);
        let x = bp.min(bq);
        if let Some(l) = last {
            w1 += (fp - fq).abs() * (x - l) as f64;
        }
        if bp == x {
            fp += p[i].1;
            i += 1;
        }
        if bq == x {
            fq += q[j].1;
            j += 1;
        }
        ks = ks.max((fp - fq).abs());
        last = Some(x);
    }
    let tv = tv_bins(&bin(p), &bin(q));
    (ks, w1 / L, tv)
}

/// A distribution's mass per band.
pub fn bin(p: &[(u32, f64)]) -> Vec<f64> {
    let mut out = vec![0.0; BANDS];
    for &(b, w) in p {
        out[band(b)] += w;
    }
    out
}

pub fn tv_bins(a: &[f64], b: &[f64]) -> f64 {
    0.5 * a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f64>()
}

/// `(TV1024, band-edge KS, binned W1/L)` between two binned distributions.
pub fn band_distances(a: &[f64], b: &[f64]) -> (f64, f64, f64) {
    let (mut fa, mut fb, mut ks, mut w1) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        fa += x;
        fb += y;
        let d = (fa - fb).abs();
        ks = ks.max(d);
        w1 += d;
    }
    (tv_bins(a, b), ks, w1 * BAND_WIDTH as f64 / L)
}

/// L1 error on the 32 heaviest blocks of `p`; absent from `q` counts as 0.
pub fn top32_l1(p: &[(u32, f64)], q: &[(u32, f64)]) -> f64 {
    let mut idx: Vec<usize> = (0..p.len()).collect();
    let m = idx.len().min(32);
    if idx.len() > 32 {
        idx.select_nth_unstable_by(31, |&a, &b| p[b].1.total_cmp(&p[a].1));
    }
    idx[..m]
        .iter()
        .map(|&i| {
            let (b, w) = p[i];
            let e = q.binary_search_by_key(&b, |x| x.0).map_or(0.0, |k| q[k].1);
            (e - w).abs()
        })
        .sum()
}

/// The largest per-coefficient absolute difference.
pub fn max_abs(p: &[(u32, f64)], q: &[(u32, f64)]) -> f64 {
    let (mut i, mut j, mut m) = (0, 0, 0.0f64);
    while i < p.len() || j < q.len() {
        let bp = p.get(i).map_or(u32::MAX, |x| x.0);
        let bq = q.get(j).map_or(u32::MAX, |x| x.0);
        let x = bp.min(bq);
        let mut d = 0.0;
        if bp == x {
            d += p[i].1;
            i += 1;
        }
        if bq == x {
            d -= q[j].1;
            j += 1;
        }
        m = m.max(d.abs());
    }
    m
}

/// A fixed log histogram: one bin for zero, 32 per decade over [1e-12, 10],
/// one for everything above.  A quantile answers its bin's upper edge.
#[derive(Clone)]
pub struct LogHist {
    counts: Vec<u64>,
}

const DECADES: i32 = 13; // 1e-12 .. 1e1
const PER_DECADE: i32 = 32;
const LOG_BINS: usize = (DECADES * PER_DECADE) as usize;

impl Default for LogHist {
    fn default() -> Self {
        LogHist { counts: vec![0; LOG_BINS + 3] }
    }
}

impl LogHist {
    /// Bin 0 holds zeros, bin 1 everything in (0, 1e-12], bin `t + 1` the
    /// values in (10^((t-1)/32 - 12), 10^(t/32 - 12)] for `t` in 1..=416, and
    /// the last bin everything above 10.
    fn index(x: f64) -> usize {
        if x <= 0.0 || x.is_nan() {
            return 0;
        }
        let t = ((x.log10() + 12.0) * PER_DECADE as f64).ceil();
        if t <= 0.0 {
            1
        } else if t > LOG_BINS as f64 {
            LOG_BINS + 2
        } else {
            t as usize + 1
        }
    }

    fn upper(i: usize) -> f64 {
        match i {
            0 => 0.0,
            _ if i > LOG_BINS + 1 => f64::INFINITY,
            _ => 10f64.powf((i - 1) as f64 / PER_DECADE as f64 - 12.0),
        }
    }

    pub fn add(&mut self, x: f64) {
        self.counts[Self::index(x.abs())] += 1;
    }

    pub fn quantile(&self, q: f64) -> f64 {
        let n: u64 = self.counts.iter().sum();
        if n == 0 {
            return f64::NAN;
        }
        let want = ((q * n as f64).ceil() as u64).clamp(1, n);
        let mut seen = 0;
        for (i, &c) in self.counts.iter().enumerate() {
            seen += c;
            if seen >= want {
                return Self::upper(i);
            }
        }
        f64::INFINITY
    }
}

/// Count, sum, sum of squares and a log histogram of absolute values.
#[derive(Clone, Default)]
pub struct Agg {
    pub n: u64,
    pub sum: f64,
    pub sumsq: f64,
    pub hist: LogHist,
}

impl Agg {
    pub fn add(&mut self, x: f64) {
        if x.is_nan() {
            return;
        }
        self.n += 1;
        self.sum += x;
        self.sumsq += x * x;
        self.hist.add(x);
    }
    pub fn mean(&self) -> f64 {
        if self.n == 0 {
            f64::NAN
        } else {
            self.sum / self.n as f64
        }
    }
    pub fn rms(&self) -> f64 {
        if self.n == 0 {
            f64::NAN
        } else {
            (self.sumsq / self.n as f64).sqrt()
        }
    }
    /// Standard error of the mean.
    pub fn se(&self) -> f64 {
        if self.n < 2 {
            return f64::NAN;
        }
        let n = self.n as f64;
        let m = self.sum / n;
        ((self.sumsq / n - m * m).max(0.0) * n / (n - 1.0) / n).sqrt()
    }
}

/// NumPy's `np.sum` (`np.add.reduce`) of a contiguous `f64` array: pairwise
/// sums of blocks of 8,192 values (its buffer), added left to right.  What
/// `summarize` and `surface-report` sum with, so that a mean or a share is
/// the same `f64` as the scripts'.
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

/// Exact quantile of a sorted slice (nearest rank).
pub fn quantile_sorted(v: &[f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
    v[i]
}

/// The median as numpy's `median` takes it: the middle value, or the mean of
/// the two middle values when the count is even; `NaN` if the sample is empty
/// or holds a NaN.  Not [`quantile_sorted`]`(v, 0.5)`, which is nearest rank and
/// answers the lower of the two middle values.
pub fn median_mid(v: &[f64]) -> f64 {
    if v.is_empty() || v.iter().any(|x| x.is_nan()) {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

/// `(mean, SE)` of a sample.
pub fn mean_se(v: &[f64]) -> (f64, f64) {
    let n = v.len() as f64;
    if v.is_empty() {
        return (f64::NAN, f64::NAN);
    }
    let m = v.iter().sum::<f64>() / n;
    if v.len() < 2 {
        return (m, f64::NAN);
    }
    let var = v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (n - 1.0);
    (m, (var / n).sqrt())
}

/// Records per block of [`BlockMeans`].
pub const SE_BLOCK: u64 = 50_000;

/// A batch-means standard error of a mean over rows that arrive in record
/// order: the rows are cut into blocks of [`SE_BLOCK`] consecutive records and
/// the SE is that of the (row-weighted) mean of the block means, i.e.
/// `sqrt(G/(G-1) . sum_g (S_g - n_g m)^2) / N` over `G` blocks of `n_g` rows
/// summing to `S_g`.  Constant memory: the block sums are folded in as each
/// block closes.
///
/// Why it exists: sampled rows are not independent draws.  Descendants share
/// sketches with their ancestors (often the very same `Rc`) and inherit their
/// sampling error, so the naive per-row SE understates the real one -- by 3x
/// to 35x on the first 600k records, measured against 16 seed replicates.
/// Block means absorb the short-range part of that correlation and come out
/// at about half to three quarters of the across-seed SD there, so they are
/// still a *lower* bound: a long-lived ancestor's error reaches across blocks.
/// Only seed replicates (`seeds.tsv`) answer a bias question.
#[derive(Clone, Default)]
pub struct BlockMeans {
    block: Option<u64>,
    cur_n: f64,
    cur_s: f64,
    blocks: u64,
    n: f64,
    s: f64,
    s2: f64,
    ns: f64,
    n2: f64,
}

impl BlockMeans {
    pub fn add(&mut self, rec: u64, x: f64) {
        if x.is_nan() {
            return;
        }
        let b = rec / SE_BLOCK;
        if self.block != Some(b) {
            self.close();
            self.block = Some(b);
        }
        self.cur_n += 1.0;
        self.cur_s += x;
    }

    fn close(&mut self) {
        if self.cur_n > 0.0 {
            self.blocks += 1;
            self.n += self.cur_n;
            self.s += self.cur_s;
            self.s2 += self.cur_s * self.cur_s;
            self.ns += self.cur_n * self.cur_s;
            self.n2 += self.cur_n * self.cur_n;
        }
        self.cur_n = 0.0;
        self.cur_s = 0.0;
    }

    /// `(SE, blocks)`; the SE is NaN below two blocks.
    pub fn se(&self) -> (f64, u64) {
        let mut t = self.clone();
        t.close();
        if t.blocks < 2 {
            return (f64::NAN, t.blocks);
        }
        let m = t.s / t.n;
        let g = t.blocks as f64;
        let ss = (t.s2 - 2.0 * m * t.ns + m * m * t.n2).max(0.0);
        ((ss * g / (g - 1.0)).sqrt() / t.n, t.blocks)
    }
}

/// A config label (`varopt-k64-s12-rec`) with its seed replaced by `*`
/// (`varopt-k64-s*-rec`): configs sharing one are seed replicates of each
/// other.
pub fn family(label: &str) -> String {
    label
        .split('-')
        .enumerate()
        .map(|(i, t)| if i == 2 && t.starts_with('s') { "s*" } else { t })
        .collect::<Vec<_>>()
        .join("-")
}

/// `(mean, SE, SD)` across seed replicates' means, each one a value of `xs`.
pub fn across(xs: &[f64]) -> (f64, f64, f64) {
    let (m, se) = mean_se(xs);
    (m, se, se * (xs.len() as f64).sqrt())
}

/// The process's peak resident set, from `/proc/self/status`.
pub fn vm_hwm() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM:"))
                .map(|l| l["VmHWM:".len()..].trim().to_string())
        })
        .unwrap_or_else(|| "NA".to_string())
}

/// Number formatting for the TSVs: shortest round-trip, NaN as `NA`.
pub fn fmt(x: f64) -> String {
    if x.is_nan() {
        "NA".to_string()
    } else {
        format!("{}", x)
    }
}

/// `ln Gamma(x)` for `x > 0` (Lanczos, g = 7, 9 terms): about 15 digits,
/// which is far more than the quantiles below need.
pub fn ln_gamma(x: f64) -> f64 {
    const G: [f64; 9] = [
        0.999_999_999_999_809_9,
        676.520_368_121_885_1,
        -1_259.139_216_722_402_8,
        771.323_428_777_653_1,
        -176.615_029_162_140_6,
        12.507_343_278_686_905,
        -0.138_571_095_265_720_12,
        9.984_369_578_019_572e-6,
        1.505_632_735_149_311_6e-7,
    ];
    if x < 0.5 {
        let pi = std::f64::consts::PI;
        return (pi / (pi * x).sin()).ln() - ln_gamma(1.0 - x);
    }
    let x = x - 1.0;
    let mut a = G[0];
    let t = x + 7.5;
    for (i, g) in G.iter().enumerate().skip(1) {
        a += g / (x + i as f64);
    }
    0.5 * (2.0 * std::f64::consts::PI).ln() + (x + 0.5) * t.ln() - t + a.ln()
}

/// The regularised incomplete beta `I_x(a, b)`, by Lentz's continued
/// fraction on whichever side converges.
pub fn inc_beta(x: f64, a: f64, b: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    if x >= 1.0 {
        return 1.0;
    }
    if x > (a + 1.0) / (a + b + 2.0) {
        return 1.0 - inc_beta(1.0 - x, b, a);
    }
    let front = (ln_gamma(a + b) - ln_gamma(a) - ln_gamma(b) + a * x.ln() + b * (1.0 - x).ln()).exp() / a;
    let tiny = 1e-300;
    let (mut f, mut c, mut d) = (1.0f64, 1.0f64, 0.0f64);
    for i in 0..400 {
        let m = (i / 2) as f64;
        let num = if i == 0 {
            1.0
        } else if i % 2 == 0 {
            m * (b - m) * x / ((a + 2.0 * m - 1.0) * (a + 2.0 * m))
        } else {
            -(a + m) * (a + b + m) * x / ((a + 2.0 * m) * (a + 2.0 * m + 1.0))
        };
        d = 1.0 + num * d;
        if d.abs() < tiny {
            d = tiny;
        }
        d = 1.0 / d;
        c = 1.0 + num / c;
        if c.abs() < tiny {
            c = tiny;
        }
        let cd = c * d;
        f *= cd;
        if (1.0 - cd).abs() < 1e-15 {
            break;
        }
    }
    front * (f - 1.0)
}

/// The regularised lower incomplete gamma `P(a, x)`: the series below `a+1`,
/// the continued fraction above.
pub fn inc_gamma(a: f64, x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    let lg = ln_gamma(a);
    if x < a + 1.0 {
        let (mut sum, mut term, mut ap) = (1.0 / a, 1.0 / a, a);
        for _ in 0..1000 {
            ap += 1.0;
            term *= x / ap;
            sum += term;
            if term.abs() < sum.abs() * 1e-16 {
                break;
            }
        }
        (sum.ln() - x + a * x.ln() - lg).exp()
    } else {
        let tiny = 1e-300;
        let mut b = x + 1.0 - a;
        let mut c = 1.0 / tiny;
        let mut d = 1.0 / b;
        let mut h = d;
        for i in 1..1000 {
            let an = -(i as f64) * (i as f64 - a);
            b += 2.0;
            d = an * d + b;
            if d.abs() < tiny {
                d = tiny;
            }
            c = b + an / c;
            if c.abs() < tiny {
                c = tiny;
            }
            d = 1.0 / d;
            let del = d * c;
            h *= del;
            if (del - 1.0).abs() < 1e-16 {
                break;
            }
        }
        1.0 - (-x + a * x.ln() - lg).exp() * h
    }
}

/// Student's t CDF with `nu` degrees of freedom.
pub fn t_cdf(t: f64, nu: f64) -> f64 {
    let tail = 0.5 * inc_beta(nu / (nu + t * t), nu / 2.0, 0.5);
    if t >= 0.0 {
        1.0 - tail
    } else {
        tail
    }
}

/// The `p`-quantile of Student's t, `p` in (0, 1), by bisection on the CDF.
pub fn t_quantile(p: f64, nu: f64) -> f64 {
    let (mut lo, mut hi) = (-1e7f64, 1e7f64);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if t_cdf(mid, nu) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// The `p`-quantile of chi-square with `k` degrees of freedom.
pub fn chi2_quantile(p: f64, k: f64) -> f64 {
    let (mut lo, mut hi) = (0.0f64, k + 100.0 * k.sqrt() + 1000.0);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if inc_gamma(k / 2.0, mid / 2.0) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

pub const LAPLACE_NAMES: [&str; 7] = ["0.5", "1", "2", "4", "8", "16", "32"];
const _: () = assert!(LAPLACE.len() == LAPLACE_NAMES.len());

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-12 * (1.0 + a.abs() + b.abs())
    }

    /// The shared sum is numpy's beyond its 8,192-value buffer too: 1e16 and
    /// 9,999 ones, whose sum by one pairwise tree is 3 ulps off numpy's.
    #[test]
    fn the_sum_is_chunked_like_numpys() {
        let mut a = vec![1e16];
        a.extend(std::iter::repeat_n(1.0, 9999));
        // NumPy 1.26: np.sum(a), as its bits.
        assert_eq!(np_sum(&a).to_bits(), 0x4341c37937e09380, "{:x}", np_sum(&a).to_bits());
        assert_eq!(pairwise(&a).to_bits(), 0x4341c37937e09383, "one tree over all the values is not numpy's sum");
        assert_eq!(np_sum(&[]), 0.0);
    }

    /// T13: hand-computed tiny distributions.
    #[test]
    fn distances_on_tiny_distributions() {
        let p = vec![(0u32, 0.5), (10, 0.5)];
        let q = vec![(0u32, 0.25), (10, 0.75)];
        let (ks, w1, tv) = distances(&p, &q);
        assert!(close(ks, 0.25));
        assert!(close(w1, 0.25 * 10.0 / L));
        // Both blocks are in band 0, so the binned TV is 0.
        assert!(close(tv, 0.0));
        let p = vec![(0u32, 0.5), (1000, 0.5)];
        let q = vec![(0u32, 0.25), (1000, 0.75)];
        let (_, _, tv) = distances(&p, &q);
        assert!(close(tv, 0.25));
        let (m, s) = moments(&p);
        assert!(close(m, 500.0) && close(s, 500.0));
    }

    #[test]
    fn identical_inputs_are_zero_apart() {
        let p = vec![(3u32, 0.2), (40, 0.3), (5000, 0.5)];
        let (ks, w1, tv) = distances(&p, &p);
        assert_eq!((ks, w1, tv), (0.0, 0.0, 0.0));
        assert_eq!(max_abs(&p, &p), 0.0);
        assert_eq!(top32_l1(&p, &p), 0.0);
    }

    #[test]
    fn a_point_mass_shifted_by_d_is_d_over_l_away() {
        for d in [1u32, 17, 745, 100_000] {
            let (ks, w1, _) = distances(&[(1000, 1.0)], &[(1000 + d, 1.0)]);
            assert!(close(w1, d as f64 / L), "d {}", d);
            assert_eq!(ks, 1.0);
        }
    }

    #[test]
    fn laplace_of_a_point_is_the_table() {
        let l = laplace(&[(1234, 1.0)]);
        assert_eq!(l, laplace_of(1234));
    }

    #[test]
    fn band_distances_agree_with_their_definitions() {
        let mut a = vec![0.0; BANDS];
        let mut b = vec![0.0; BANDS];
        a[0] = 1.0;
        b[2] = 1.0;
        let (tv, ks, w1) = band_distances(&a, &b);
        assert!(close(tv, 1.0) && close(ks, 1.0) && close(w1, 2.0 * 745.0 / L));
        assert_eq!(band(762_260), 1023);
        assert_eq!(band(744), 0);
        assert_eq!(band(745), 1);
    }

    #[test]
    fn the_median_is_numpys() {
        assert_eq!(median_mid(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median_mid(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(quantile_sorted(&[1.0, 2.0, 3.0, 4.0], 0.5), 2.0);
        assert!(median_mid(&[]).is_nan() && median_mid(&[1.0, f64::NAN]).is_nan());
    }

    #[test]
    fn the_histogram_answers_upper_edges() {
        let mut h = LogHist::default();
        for x in [0.0, 1e-3, 1e-3, 2e-3, 0.5] {
            h.add(x);
        }
        assert_eq!(h.quantile(0.2), 0.0);
        let q = h.quantile(0.6);
        assert!(q >= 1e-3 && q < 1e-3 * 10f64.powf(1.0 / 32.0) * 1.0000001, "{}", q);
        assert!(h.quantile(1.0) >= 0.5);
        let mut h = LogHist::default();
        h.add(1e-20);
        h.add(100.0);
        assert_eq!(h.quantile(0.5), 1e-12);
        assert_eq!(h.quantile(1.0), f64::INFINITY);
    }

    /// Equal blocks reduce to the SE of the block means; one block has none.
    #[test]
    fn block_means_se_is_the_se_of_the_block_means() {
        let mut b = BlockMeans::default();
        let means = [1.0, 3.0, 2.0, 6.0];
        for (g, m) in means.iter().enumerate() {
            for i in 0..10u64 {
                let rec = g as u64 * SE_BLOCK + i * 7;
                b.add(rec, m + if i % 2 == 0 { 0.5 } else { -0.5 });
            }
        }
        let (se, g) = b.se();
        assert_eq!(g, 4);
        let (_, want) = mean_se(&means);
        assert!(close(se, want), "{} vs {}", se, want);
        let mut one = BlockMeans::default();
        one.add(3, 1.0);
        one.add(4, 2.0);
        assert!(one.se().0.is_nan());
        assert_eq!(family("varopt-k64-s12-rec"), "varopt-k64-s*-rec");
        assert_eq!(family("coord-k64-s3-step-min-f0"), "coord-k64-s*-step-min-f0");
    }

    #[test]
    fn top32_counts_missing_blocks_as_zero() {
        let p: Vec<(u32, f64)> = (0..40).map(|b| (b, 1.0 / 40.0)).collect();
        let q = vec![(0u32, 1.0)];
        let e = top32_l1(&p, &q);
        assert!(e > 0.0);
    }

    /// The quantiles the seed gates use, against tabulated values.
    #[test]
    fn t_and_chi2_quantiles_match_the_tables() {
        assert!((t_quantile(0.975, 7.0) - 2.364_624).abs() < 1e-5);
        assert!((t_quantile(0.995, 15.0) - 2.946_713).abs() < 1e-5);
        assert!((t_quantile(0.5, 3.0)).abs() < 1e-6);
        assert!((chi2_quantile(0.025, 7.0) - 1.689_869).abs() < 1e-5);
        assert!((chi2_quantile(0.975, 7.0) - 16.012_764).abs() < 1e-5);
        assert!((chi2_quantile(0.95, 63.0) - 82.528_727).abs() < 1e-4);
        assert!((ln_gamma(10.0) - 362_880f64.ln()).abs() < 1e-12);
    }
}
