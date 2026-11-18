//! # `surface-report`: every number §12 of the post reads off the surface
//!
//! Reads the surfaces `surface` wrote -- the sketched one (`--sketched`) and,
//! optionally, the exact one (`--exact`) -- and writes the tables and the
//! statistics the post quotes, plus the per-column and per-year series its
//! figures draw.  It replaces `figs_surface.py`'s printed numbers and
//! `gen_tables.py` of the evidence folder, and follows them to the
//! convention: same normalisation, same selections, and numpy's own rules for
//! sums, medians, quantiles and interpolation (below), so a number here is
//! the Python one to the last digit or within a rounding of it.
//!
//! ## What a surface is here
//!
//! A row of the sum matrices divided by its count is `P(s, .)` (minting
//! band) or `Q(s, .)` (age band); a row with no records is left out (`NaN` in
//! the scripts, which take `nan_to_num` of it, i.e. zero, wherever they sum).
//! Both are cut to the last nonempty row, `last`, so `L = last x band` blocks.
//! A column's position is its band's centre, `mid = (s + 1/2) band`; ages are
//! in years of [`NOMINAL_YEAR`] blocks, an age band's upper edge `(a + 1)
//! band` and its centre `mid`.
//!
//! Years come from `--years`, a TSV `year<TAB>first_block` (the first block of
//! each calendar year, ascending; the post's file starts at 2010).  A year's
//! columns are the nonempty ones whose `mid` lies in `[first_block, next
//! year's first_block)` (the last year runs to `L`), and a year's averages
//! weigh its columns by their records.  With `y0 < y1 < y2 < ...` the years
//! of the file, three choices of the post follow the file rather than being
//! written here: the ratio table's first two cuts are `y0` and `y1`
//! ("minted before 2010", "before 2011") and its rows start at `y2`; and the
//! check of §12.6 calls a column *early* when it is spent before `y1` and
//! *late* when spent from `y2` on.
//!
//! ## The tables
//!
//! - **The age of the coins spent** (`surface-age.tsv`, tab:surface-age):
//!   per year, the year-average of `Q`, its mean age (centres), and by linear
//!   interpolation of its CDF at the upper edges the median (from a CDF of 0
//!   at age 0, as the current `gen_tables.py`) and the shares under a week
//!   (1/52 y), a month (1/12 y), a year, and over four years.
//!   The published table is `surface = sketched`: ages from the blocks of the
//!   sketches' terms.  Two more readings sit beside it, both ages as
//!   differences of bands -- `sketched-bands` (`Q` rebuilt from `P` as
//!   `Q(s, s - m) = P(s, m)`) and `exact-bands` (the exact age surface,
//!   which the binned file can give no other way) -- and they are what §12.6
//!   compares.
//! - **What moves against what exists** (`surface-ratio.tsv`,
//!   tab:surface-ratio): for each year from `y2`, the year-average of `P`
//!   over the bands wholly minted before the year began (`top = first_block /
//!   band`), renormalised, against the subsidy those bands minted,
//!   `50 x 2^-floor(b / 210,000)` per block, also renormalised; summed below
//!   the cuts `y0`, `y1` and block 210,000, and over the year before.  That
//!   last column comes two ways.  `prev_year` is the calendar year before,
//!   the bands from the previous year's `first_block / band` to `top` (70 to
//!   91 bands); the post prints it as "minted in the year before".
//!   `gen_tables.py` used **a fixed window of the last [`window_bands`]
//!   bands** instead (70, 52,150 blocks, `floor(365.25 x 144 / 745)`), which
//!   the post's first version printed as "in the 12 months before the year";
//!   it is written as `last70`.  Then the ratios the prose quotes: before
//!   `y0` (`ratio_before_y0`), over the fixed window, the mean share/supply
//!   ratio of the last 6 bands (`ratio_last6`, about a month) and that of the
//!   last band alone; and the spread of the per-band ratio over the "well
//!   mixed" middle, the bands from `y2` to two years before the year spent
//!   (`mixed_*`).
//! - `surface-tables.md`: both tables in the post's markdown, each cell
//!   formatted as `gen_tables.py` formatted it; the ratio table twice, with
//!   the calendar year and with the fixed window as its last column.
//!
//! ## Sketched against exact (`surface-check.tsv`, with `--exact`)
//!
//! The two runs must have kept the same records in every row (`rows.f64`
//! bitwise equal), or the report refuses.  `statistic<TAB>value<TAB>what`:
//! the total variation between each sketched column and the exact one, over
//! all, early and late columns, and per year (`surface-tv-years.tsv`); the
//! largest cell difference and the 99th percentile over the cells where the
//! exact surface is not zero; the mean and median age of each column, read
//! both as `gen_tables.py` read them (sketched ages from blocks against exact
//! ages from bands, which differ by about half a band by construction) and as
//! the post's text reads them (both from bands); the largest difference of
//! the age table's shares between the two band readings; and a few
//! descriptive numbers of §12.2 (how much of the triangle below the diagonal
//! a late column covers, and at what heights; the step of each halving in the
//! last year's colour).
//!
//! ## Series for the figures
//!
//! - `surface-columns.tsv`: per spending band, its records, TV, and the mean
//!   and 10/25/50/75/90% quantiles of the age (Figure "surface-age" (b) and
//!   "surface-check" (a)); a quantile is interpolated inside its band as
//!   `figs_surface.py` does, `(k + (q - C[k-1]) / (C[k] - C[k-1])) band`.
//! - `surface-age-cdf.tsv`: per year, the CDF of the age at each band's upper
//!   edge (Figure "surface-age" (a)).
//! - `surface-ratio-curves.tsv`: per year from `y2` and band before `top`,
//!   share, supply and their ratio (Figure "surface-ratio").
//!
//! The heatmaps and the 3D view draw every cell, and read the `.f64` matrices
//! themselves.
//!
//! ## numpy's conventions, followed here
//!
//! - `np.sum` of a contiguous vector (and of a row, `axis=1`) is pairwise, in
//!   blocks of 8,192 ([`np_sum`]); a sum over rows (`axis=0`) and `cumsum`
//!   are sequential.
//! - `np.interp(x, xp, fp)` ([`np_interp`]): `fp[0]` below the range, `fp[-1]`
//!   above, else the segment `[xp[j], xp[j+1])` holding `x`.
//! - `np.median` and `np.quantile` with the default `linear` method
//!   ([`np_quantile`]: virtual index `(n - 1) q`, and numpy's two-sided lerp,
//!   `a + (b - a) g` below `g = 1/2` and `b - (b - a)(1 - g)` from it).
//! - `np.searchsorted(c, q)` (side left) for the quantiles of a column.

use crate::metrics::{fmt, np_sum};
use crate::surface::Surface;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};

pub struct ReportOpts {
    pub sketched: String,
    pub exact: Option<String>,
    pub years: String,
    pub out: String,
    pub width: u32,
}

/// Blocks in a nominal year, `365.25 x 144`, as the scripts used.
pub const NOMINAL_YEAR: f64 = 365.25 * 144.0;

/// Blocks between two halvings of the subsidy.
pub const HALVING: u64 = 210_000;

/// The "12 months before the year" of the ratio table: `floor(365.25 x 144 /
/// band)` bands, 70 at 745 blocks.
pub fn window_bands(width: u32) -> usize {
    (NOMINAL_YEAR / width as f64).floor() as usize
}

// ------------------------------------------------------------------ numpy

/// `np.interp(x, xp, fp)` for a scalar `x` and a nondecreasing `xp`.
pub fn np_interp(x: f64, xp: &[f64], fp: &[f64]) -> f64 {
    let n = xp.len();
    if x > xp[n - 1] {
        return fp[n - 1];
    }
    if x < xp[0] {
        return fp[0];
    }
    // The last j with xp[j] <= x.
    let j = xp.partition_point(|&v| v <= x) - 1;
    if j == n - 1 || xp[j] == x {
        return fp[j];
    }
    let slope = (fp[j + 1] - fp[j]) / (xp[j + 1] - xp[j]);
    slope * (x - xp[j]) + fp[j]
}

/// `np.quantile(v, q)`, method `linear`.  NaN for an empty `v`, a NaN `q`
/// or a NaN in `v`, as numpy.
pub fn np_quantile(v: &[f64], q: f64) -> f64 {
    if v.is_empty() || q.is_nan() || v.iter().any(|x| x.is_nan()) {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    let virt = (n - 1) as f64 * q;
    let (prev, next) = if virt >= (n - 1) as f64 {
        (n - 1, n - 1)
    } else if virt < 0.0 {
        (0, 0)
    } else {
        (virt.floor() as usize, virt.floor() as usize + 1)
    };
    let gamma = virt - virt.floor();
    let (a, b) = (s[prev], s[next]);
    let d = b - a;
    if gamma >= 0.5 {
        b - d * (1.0 - gamma)
    } else {
        a + d * gamma
    }
}

/// `np.median(v)`: the middle value, or the mean of the two middle ones; NaN
/// for an empty `v` or one holding a NaN.
pub fn np_median(v: &[f64]) -> f64 {
    if v.is_empty() || v.iter().any(|x| x.is_nan()) {
        return f64::NAN;
    }
    let mut s = v.to_vec();
    s.sort_by(|a, b| a.total_cmp(b));
    let n = s.len();
    if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2.0
    }
}

fn cumsum(v: &[f64]) -> Vec<f64> {
    let mut s = 0.0;
    v.iter()
        .map(|&x| {
            s += x;
            s
        })
        .collect()
}

fn max_of(v: impl IntoIterator<Item = f64>) -> f64 {
    v.into_iter().fold(f64::NAN, |m, x| if m.is_nan() || x > m { x } else { m })
}

fn min_of(v: impl IntoIterator<Item = f64>) -> f64 {
    v.into_iter().fold(f64::NAN, |m, x| if m.is_nan() || x < m { x } else { m })
}

// ------------------------------------------------------------------ inputs

/// `year<TAB>first_block`, ascending, with a header line; `#` lines are
/// comments.  At least three years.
pub fn read_years(path: &str) -> io::Result<Vec<(i64, u64)>> {
    let bad = |what: String| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {}", path, what));
    let f = File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
    let mut out: Vec<(i64, u64)> = Vec::new();
    let mut header = false;
    for (i, line) in BufReader::new(f).lines().enumerate() {
        let line = line?;
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if !header {
            if t != "year\tfirst_block" {
                return Err(bad(format!("the header is {:?}, not \"year<TAB>first_block\"", t)));
            }
            header = true;
            continue;
        }
        let mut it = t.split('\t');
        let y = it.next().and_then(|s| s.parse::<i64>().ok());
        let b = it.next().and_then(|s| s.parse::<u64>().ok());
        match (y, b, it.next()) {
            (Some(y), Some(b), None) => {
                if let Some(&(py, pb)) = out.last() {
                    if y <= py || b <= pb {
                        return Err(bad(format!("line {}: years and blocks must ascend", i + 1)));
                    }
                }
                out.push((y, b));
            }
            _ => return Err(bad(format!("line {}: not year<TAB>first_block: {:?}", i + 1, t))),
        }
    }
    if out.len() < 3 {
        return Err(bad(format!("{} years; the report needs at least three", out.len())));
    }
    Ok(out)
}

/// The geometry every computation shares.
struct Ctx {
    width: f64,
    last: usize,
    ok: Vec<bool>,
    rows: Vec<f64>,
    mid: Vec<f64>,
    age_mid: Vec<f64>,
    age_hi: Vec<f64>,
    years: Vec<(i64, u64)>,
}

/// `sum / rows` row by row, cut to `last x last`; empty rows are zero.
fn normalise(sum: &[f64], bands: usize, rows: &[f64], last: usize) -> Vec<f64> {
    let mut m = vec![0.0; last * last];
    for i in 0..last {
        if rows[i] > 0.0 {
            for j in 0..last {
                m[i * last + j] = sum[i * bands + j] / rows[i];
            }
        }
    }
    m
}

/// The age by band differences, `Q(s, s - m) = P(s, m)`: what the exact file
/// gives, read off a minting surface.  A term minted after its row (none on
/// this chain) is age 0, as `surface` bins it.
fn band_ages(p: &[f64], last: usize) -> Vec<f64> {
    let mut q = vec![0.0; last * last];
    for s in 0..last {
        for m in 0..=s {
            q[s * last + (s - m)] = p[s * last + m];
        }
        for m in s + 1..last {
            q[s * last] += p[s * last + m];
        }
    }
    q
}

impl Ctx {
    fn new(sf: &Surface, years: Vec<(i64, u64)>) -> io::Result<Ctx> {
        let last = match sf.rows.iter().rposition(|&r| r > 0.0) {
            Some(i) => i + 1,
            None => return Err(io::Error::new(io::ErrorKind::InvalidData, "the surface has no records")),
        };
        let w = sf.width as f64;
        let mid: Vec<f64> = (0..last).map(|i| (i as f64 + 0.5) * w).collect();
        Ok(Ctx {
            width: w,
            last,
            ok: sf.rows[..last].iter().map(|&r| r > 0.0).collect(),
            rows: sf.rows[..last].to_vec(),
            age_mid: mid.iter().map(|&m| m / NOMINAL_YEAR).collect(),
            age_hi: (0..last).map(|i| ((i + 1) as f64 * w) / NOMINAL_YEAR).collect(),
            mid,
            years,
        })
    }

    fn blocks(&self) -> f64 {
        self.last as f64 * self.width
    }

    fn band(&self, block: u64) -> usize {
        (block as f64 / self.width).floor() as usize
    }

    /// The nonempty columns spent in year `i` of the file.
    fn year_sel(&self, i: usize) -> Vec<usize> {
        let lo = self.years[i].1 as f64;
        let hi = self.years.get(i + 1).map_or(self.blocks(), |y| y.1 as f64);
        (0..self.last).filter(|&s| self.ok[s] && self.mid[s] >= lo && self.mid[s] < hi).collect()
    }

    /// Columns spent from `from` (inclusive) to `to` (exclusive), nonempty.
    fn span(&self, from: f64, to: f64) -> Vec<usize> {
        (0..self.last).filter(|&s| self.ok[s] && self.mid[s] >= from && self.mid[s] < to).collect()
    }

    /// The record-weighted average of the rows `sel` of `z`, and the records.
    fn year_avg(&self, z: &[f64], sel: &[usize]) -> (Vec<f64>, f64) {
        let n = self.last;
        let tot = np_sum(&sel.iter().map(|&s| self.rows[s]).collect::<Vec<_>>());
        let mut dist = vec![0.0; n];
        for &s in sel {
            let w = self.rows[s] / tot;
            for j in 0..n {
                dist[j] += z[s * n + j] * w;
            }
        }
        (dist, tot)
    }

    /// `(sum_j z[i, j] mid_j) / year`, per row.
    fn mean_ages(&self, z: &[f64]) -> Vec<f64> {
        let n = self.last;
        (0..n).map(|i| np_sum(&(0..n).map(|j| z[i * n + j] * self.mid[j]).collect::<Vec<_>>()) / NOMINAL_YEAR).collect()
    }

    /// `gen_tables.py`'s median age per row: the CDF normalised by its last
    /// value, interpolated at the upper edges.  NaN for an empty row.
    fn median_ages(&self, z: &[f64]) -> Vec<f64> {
        let n = self.last;
        (0..n)
            .map(|i| {
                if !self.ok[i] {
                    return f64::NAN;
                }
                let c = cumsum(&z[i * n..(i + 1) * n]);
                let t = c[n - 1];
                let c: Vec<f64> = c.iter().map(|x| x / t).collect();
                np_interp(0.5, &c, &self.age_hi)
            })
            .collect()
    }

    /// `figs_surface.py`'s quantiles per row, in blocks: the band holding `q`
    /// and a linear position inside it.  NaN for an empty row.
    fn quantiles(&self, z: &[f64], qs: &[f64]) -> Vec<Vec<f64>> {
        let n = self.last;
        (0..n)
            .map(|i| {
                if !self.ok[i] {
                    return vec![f64::NAN; qs.len()];
                }
                let c = cumsum(&z[i * n..(i + 1) * n]);
                let t = c[n - 1];
                let c: Vec<f64> = c.iter().map(|x| x / t).collect();
                qs.iter()
                    .map(|&q| {
                        let k = c.partition_point(|&v| v < q);
                        let lo = if k > 0 { c[k - 1] } else { 0.0 };
                        (k as f64 + (q - lo) / (c[k] - lo).max(1e-300)) * self.width
                    })
                    .collect()
            })
            .collect()
    }
}

// ------------------------------------------------------------------ tables

#[derive(Clone, Debug)]
struct AgeRow {
    year: i64,
    cols: usize,
    tx: f64,
    mean: f64,
    median: f64,
    week: f64,
    month: f64,
    year1: f64,
    over4: f64,
    /// The year's average age distribution's CDF, per age band.
    cdf: Vec<f64>,
}

fn age_rows(c: &Ctx, q: &[f64]) -> Vec<AgeRow> {
    let mut out = Vec::new();
    for i in 0..c.years.len() {
        let sel = c.year_sel(i);
        if sel.is_empty() {
            continue;
        }
        let (dist, tot) = c.year_avg(q, &sel);
        let cdf = cumsum(&dist);
        let mean = np_sum(&dist.iter().zip(&c.age_mid).map(|(d, a)| d * a).collect::<Vec<_>>());
        // The CDF is 0 at age 0: when the first band already holds half the
        // colour (2011), the median lies inside it, not at its upper edge.
        // gen_tables.py's `np.interp(0.5, np.r_[0, c], np.r_[0, age_hi])`.
        let cdf0: Vec<f64> = std::iter::once(0.0).chain(cdf.iter().copied()).collect();
        let hi0: Vec<f64> = std::iter::once(0.0).chain(c.age_hi.iter().copied()).collect();
        out.push(AgeRow {
            year: c.years[i].0,
            cols: sel.len(),
            tx: tot,
            mean,
            median: np_interp(0.5, &cdf0, &hi0),
            week: np_interp(1.0 / 52.0, &c.age_hi, &cdf),
            month: np_interp(1.0 / 12.0, &c.age_hi, &cdf),
            year1: np_interp(1.0, &c.age_hi, &cdf),
            over4: (1.0 - np_interp(4.0, &c.age_hi, &cdf)).max(0.0),
            cdf,
        });
    }
    out
}

/// The subsidy minted in each band of the surface's `L` blocks.
fn subsidy_bands(c: &Ctx) -> Vec<f64> {
    let w = c.width as u64;
    let mut sub = vec![0.0; c.last];
    for b in 0..(c.last as u64 * w) {
        sub[(b / w) as usize] += 50.0 * 0.5f64.powi((b / HALVING) as i32);
    }
    sub
}

#[derive(Clone, Debug)]
struct RatioRow {
    year: i64,
    top: usize,
    /// (colour, supply) before `y0`, before `y1`, before block 210,000, over
    /// the last [`window_bands`], over the previous calendar year.
    cuts: [(f64, f64); 5],
    ratio_y0: f64,
    ratio_window: f64,
    ratio_last6: f64,
    ratio_last_band: f64,
    /// Bands of the middle, and the min, 5%, 95% and max of their ratio.
    mixed: (usize, f64, f64, f64, f64),
    share: Vec<f64>,
    supply: Vec<f64>,
}

fn ratio_rows(c: &Ctx, p: &[f64], sub: &[f64]) -> Vec<RatioRow> {
    let k = window_bands(c.width as u32);
    let mut out = Vec::new();
    for i in 2..c.years.len() {
        let sel = c.year_sel(i);
        if sel.is_empty() {
            continue;
        }
        let (dm, _) = c.year_avg(p, &sel);
        let top = c.band(c.years[i].1).min(c.last);
        let ds = np_sum(&dm[..top]);
        let ss = np_sum(&sub[..top]);
        let share: Vec<f64> = dm[..top].iter().map(|x| x / ds).collect();
        let supply: Vec<f64> = sub[..top].iter().map(|x| x / ss).collect();
        let below = |cut: usize| {
            let cut = cut.min(top);
            (np_sum(&share[..cut]), np_sum(&supply[..cut]))
        };
        let from = |start: usize| {
            let start = start.min(top);
            (np_sum(&share[start..]), np_sum(&supply[start..]))
        };
        let cuts = [
            below(c.band(c.years[0].1)),
            below(c.band(c.years[1].1)),
            below(c.band(HALVING)),
            from(top.saturating_sub(k)),
            from(c.band(c.years[i - 1].1)),
        ];
        let r: Vec<f64> = share.iter().zip(&supply).map(|(a, b)| a / b).collect();
        let last6 = np_sum(&r[top.saturating_sub(6)..]) / r.len().min(6) as f64;
        let mixed = if i >= 4 {
            let (a, b) = (c.band(c.years[2].1).min(top), c.band(c.years[i - 2].1).min(top));
            let v = &r[a..b.max(a)];
            (v.len(), min_of(v.iter().copied()), np_quantile(v, 0.05), np_quantile(v, 0.95), max_of(v.iter().copied()))
        } else {
            (0, f64::NAN, f64::NAN, f64::NAN, f64::NAN)
        };
        out.push(RatioRow {
            year: c.years[i].0,
            top,
            cuts,
            ratio_y0: cuts[0].0 / cuts[0].1,
            ratio_window: cuts[3].0 / cuts[3].1,
            ratio_last6: last6,
            ratio_last_band: r.last().copied().unwrap_or(f64::NAN),
            mixed,
            share,
            supply,
        });
    }
    out
}

fn pct(x: f64) -> String {
    format!("{:.1}%", 100.0 * x)
}

fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn age_table_md(rows: &[AgeRow]) -> String {
    rows.iter()
        .map(|r| {
            format!(
                "| {} | {} | {:.2} | {:.2} | {} | {} | {} |\n",
                r.year,
                commas(r.tx as u64),
                r.mean,
                r.median,
                pct(r.week),
                pct(r.year1),
                pct(r.over4)
            )
        })
        .collect()
}

/// The ratio table, its last column the calendar year before (`calendar`)
/// or the fixed window.
fn ratio_table_md(rows: &[RatioRow], calendar: bool) -> String {
    rows.iter()
        .map(|r| {
            let last = if calendar { r.cuts[4] } else { r.cuts[3] };
            let cells: Vec<String> =
                r.cuts[..3].iter().chain([&last]).map(|(a, b)| format!("{} / {}", pct(*a), pct(*b))).collect();
            format!("| {} | {} |\n", r.year, cells.join(" | "))
        })
        .collect()
}

// ------------------------------------------------------------------ the run

fn create(dir: &str, name: &str) -> io::Result<BufWriter<File>> {
    let path = format!("{}/{}", dir, name);
    Ok(BufWriter::new(File::create(&path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?))
}

/// The statistics of `surface-check.tsv`, in order.
struct Stats(Vec<(String, f64, String)>);

impl Stats {
    fn put(&mut self, name: &str, v: f64, what: &str) {
        self.0.push((name.to_string(), v, what.to_string()));
    }
}

/// The sketched surface against the exact one.
struct Exact {
    p: Vec<f64>,
    q: Vec<f64>,
}

pub fn run(opts: ReportOpts) -> io::Result<()> {
    let years = read_years(&opts.years)?;
    let sk = Surface::read(&opts.sketched, opts.width)?;
    let c = Ctx::new(&sk, years)?;
    let n = c.last;
    let p = normalise(&sk.mint, sk.bands, &sk.rows, n);
    let q = normalise(&sk.age, sk.bands, &sk.rows, n);
    let qb = band_ages(&p, n);
    let ex = match &opts.exact {
        None => None,
        Some(pre) => {
            let x = Surface::read(pre, opts.width)?;
            if x.rows != sk.rows {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} and {} kept different records in some row (rows.f64 differ)", opts.sketched, pre),
                ));
            }
            Some(Exact { p: normalise(&x.mint, x.bands, &x.rows, n), q: normalise(&x.age, x.bands, &x.rows, n) })
        }
    };
    std::fs::create_dir_all(&opts.out)?;
    let (y0, y1, y2) = (c.years[0], c.years[1], c.years[2]);

    // ---- the age table, three readings
    let age_sk = age_rows(&c, &q);
    let age_skb = age_rows(&c, &qb);
    let age_ex = ex.as_ref().map(|e| age_rows(&c, &e.q));
    let mut w = create(&opts.out, "surface-age.tsv")?;
    writeln!(w, "surface\tyear\tcolumns\ttransactions\tmean_age\tmedian_age\tunder_week\tunder_month\tunder_year\tover_4y")?;
    for (name, rows) in [("sketched", Some(&age_sk)), ("sketched-bands", Some(&age_skb)), ("exact-bands", age_ex.as_ref())] {
        for r in rows.into_iter().flatten() {
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                name,
                r.year,
                r.cols,
                r.tx,
                fmt(r.mean),
                fmt(r.median),
                fmt(r.week),
                fmt(r.month),
                fmt(r.year1),
                fmt(r.over4)
            )?;
        }
    }
    w.flush()?;
    let mut w = create(&opts.out, "surface-age-cdf.tsv")?;
    writeln!(w, "surface\tyear\tage_band\tage_years\tcdf")?;
    for (name, rows) in [("sketched", Some(&age_sk)), ("exact-bands", age_ex.as_ref())] {
        for r in rows.into_iter().flatten() {
            for (a, v) in r.cdf.iter().enumerate() {
                writeln!(w, "{}\t{}\t{}\t{}\t{}", name, r.year, a, fmt(c.age_hi[a]), fmt(*v))?;
            }
        }
    }
    w.flush()?;

    // ---- the ratio table
    let sub = subsidy_bands(&c);
    let rat_sk = ratio_rows(&c, &p, &sub);
    let rat_ex = ex.as_ref().map(|e| ratio_rows(&c, &e.p, &sub));
    let k = window_bands(opts.width);
    let mut w = create(&opts.out, "surface-ratio.tsv")?;
    let cut_names = [
        format!("before_{}", y0.0),
        format!("before_{}", y1.0),
        format!("before_b{}", HALVING),
        format!("last{}", k),
        "prev_year".to_string(),
    ];
    let mut head = vec!["surface".to_string(), "year".into(), "top_band".into()];
    for cn in &cut_names {
        head.push(format!("colour_{}", cn));
        head.push(format!("supply_{}", cn));
    }
    head.extend([
        format!("ratio_before_{}", y0.0),
        format!("ratio_last{}", k),
        "ratio_last6".into(),
        "ratio_last_band".into(),
        "mixed_bands".into(),
        "mixed_min".into(),
        "mixed_q05".into(),
        "mixed_q95".into(),
        "mixed_max".into(),
    ]);
    writeln!(w, "{}", head.join("\t"))?;
    for (name, rows) in [("sketched", Some(&rat_sk)), ("exact", rat_ex.as_ref())] {
        for r in rows.into_iter().flatten() {
            let mut f = vec![name.to_string(), r.year.to_string(), r.top.to_string()];
            for (a, b) in &r.cuts {
                f.push(fmt(*a));
                f.push(fmt(*b));
            }
            f.extend([
                fmt(r.ratio_y0),
                fmt(r.ratio_window),
                fmt(r.ratio_last6),
                fmt(r.ratio_last_band),
                r.mixed.0.to_string(),
                fmt(r.mixed.1),
                fmt(r.mixed.2),
                fmt(r.mixed.3),
                fmt(r.mixed.4),
            ]);
            writeln!(w, "{}", f.join("\t"))?;
        }
    }
    w.flush()?;
    let mut w = create(&opts.out, "surface-ratio-curves.tsv")?;
    writeln!(w, "surface\tyear\tband\tmid\tshare\tsupply\tratio")?;
    for (name, rows) in [("sketched", Some(&rat_sk)), ("exact", rat_ex.as_ref())] {
        for r in rows.into_iter().flatten() {
            for j in 0..r.top {
                let (a, b) = (r.share[j], r.supply[j]);
                writeln!(w, "{}\t{}\t{}\t{}\t{}\t{}\t{}", name, r.year, j, fmt(c.mid[j]), fmt(a), fmt(b), fmt(a / b))?;
            }
        }
    }
    w.flush()?;
    let mut w = create(&opts.out, "surface-tables.md")?;
    writeln!(w, "<!-- tab:surface-age (sketched, ages from blocks) -->")?;
    write!(w, "{}", age_table_md(&age_sk))?;
    writeln!(w, "\n<!-- tab:surface-ratio (sketched; the last column is the calendar year before) -->")?;
    write!(w, "{}", ratio_table_md(&rat_sk, true))?;
    writeln!(w, "\n<!-- tab:surface-ratio as gen_tables.py made it (the last column is the fixed {}-band window) -->", k)?;
    write!(w, "{}", ratio_table_md(&rat_sk, false))?;
    w.flush()?;

    // ---- per column
    let qs = [0.1, 0.25, 0.5, 0.75, 0.9];
    let quant = c.quantiles(&q, &qs);
    let mean_sk = c.mean_ages(&q);
    let med_sk = c.median_ages(&q);
    let mean_skb = c.mean_ages(&qb);
    let med_skb = c.median_ages(&qb);
    let tv: Option<Vec<f64>> = ex.as_ref().map(|e| {
        (0..n).map(|i| 0.5 * np_sum(&(0..n).map(|j| (p[i * n + j] - e.p[i * n + j]).abs()).collect::<Vec<_>>())).collect()
    });
    let (mean_ex, med_ex, quant_ex) = match &ex {
        Some(e) => (Some(c.mean_ages(&e.q)), Some(c.median_ages(&e.q)), Some(c.quantiles(&e.q, &qs))),
        None => (None, None, None),
    };
    let mut w = create(&opts.out, "surface-columns.tsv")?;
    writeln!(
        w,
        "band\tfirst_block\tmid\trecords\ttv\tmean_age\tq10\tq25\tq50\tq75\tq90\tmedian_interp\tmean_age_bands\tmedian_bands\texact_mean_age\texact_q50\texact_median_interp"
    )?;
    let na = |o: &Option<Vec<f64>>, i: usize| o.as_ref().map_or("NA".to_string(), |v| fmt(v[i]));
    for i in 0..n {
        let okf = |x: f64| if c.ok[i] { fmt(x) } else { "NA".to_string() };
        let qy: Vec<String> = quant[i].iter().map(|x| fmt(x / NOMINAL_YEAR)).collect();
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            i,
            i as u64 * opts.width as u64,
            fmt(c.mid[i]),
            c.rows[i],
            if c.ok[i] { na(&tv, i) } else { "NA".into() },
            okf(mean_sk[i]),
            qy.join("\t"),
            fmt(med_sk[i]),
            okf(mean_skb[i]),
            fmt(med_skb[i]),
            if c.ok[i] { na(&mean_ex, i) } else { "NA".into() },
            match &quant_ex {
                Some(v) => fmt(v[i][2] / NOMINAL_YEAR),
                None => "NA".into(),
            } + "\t"
                + &na(&med_ex, i)
        )?;
    }
    w.flush()?;

    // ---- statistics
    let mut st = Stats(Vec::new());
    let ok: Vec<usize> = (0..n).filter(|&i| c.ok[i]).collect();
    let late = c.span(y2.1 as f64, f64::INFINITY);
    let early = c.span(f64::NEG_INFINITY, y1.1 as f64);
    let before_late = c.span(f64::NEG_INFINITY, y2.1 as f64);
    st.put("records_kept", np_sum(&c.rows), "transactions in the surface (coinbases left out)");
    st.put("columns", ok.len() as f64, "nonempty spending bands");
    st.put("last_band", n as f64, "one past the last nonempty spending band");
    st.put("max_p", max_of(ok.iter().flat_map(|&i| p[i * n..(i + 1) * n].iter().copied())), "largest cell of the sketched P");
    st.put("cells_positive", p.iter().filter(|&&x| x > 0.0).count() as f64, "cells of the sketched P above 0");
    // §12.2: how much of the triangle a late column covers, and how high.
    let mut below = 0usize;
    let mut pos = Vec::new();
    for &i in &late {
        for j in 0..i {
            below += 1;
            if p[i * n + j] > 0.0 {
                pos.push(p[i * n + j]);
            }
        }
    }
    let tag = format!("late columns (spent from {})", y2.0);
    st.put("late_below_diagonal_positive", pos.len() as f64 / below as f64, &format!("share of the cells below the diagonal above 0, {}", tag));
    for (nm, qq) in [("q05", 0.05), ("q50", 0.5), ("q95", 0.95)] {
        st.put(&format!("late_below_diagonal_{}", nm), np_quantile(&pos, qq), &format!("quantile of those positive cells, {}", tag));
    }
    let inside = pos.iter().filter(|&&x| (1e-4..=1e-2).contains(&x)).count();
    st.put("late_below_diagonal_in_1e-4_1e-2", inside as f64 / pos.len() as f64, "share of those positive cells between 1e-4 and 1e-2");
    // The halving steps, in the last year's colour.
    if let Some(i) = (0..c.years.len()).rev().find(|&i| !c.year_sel(i).is_empty()) {
        let (dm, _) = c.year_avg(&p, &c.year_sel(i));
        let mut h = HALVING;
        // Ten bands each side of the halving's band; a wide --band may leave
        // fewer than ten before the first halving, and then there is no step.
        while c.band(h) >= 10 && c.band(h) + 11 < n {
            let hb = c.band(h);
            let before: f64 = dm[hb - 10..hb].iter().sum::<f64>() / 10.0;
            let after: f64 = dm[hb + 1..hb + 11].iter().sum::<f64>() / 10.0;
            st.put(
                &format!("halving_step_{}_{}", h, c.years[i].0),
                before / after,
                "mean share of the 10 bands before the halving's band over the 10 after it, year-averaged colour",
            );
            h += HALVING;
        }
    }
    for r in &rat_sk {
        if r.year == c.years.last().unwrap().0 {
            st.put(&format!("ratio_before_{}_{}", y0.0, r.year), r.ratio_y0, "colour/supply before y0, sketched");
        }
    }
    let r13: Vec<&RatioRow> = rat_sk.iter().filter(|r| r.year > y2.0).collect();
    st.put(&format!("ratio_before_{}_min_from_{}", y0.0, y2.0 + 1), min_of(r13.iter().map(|r| r.ratio_y0)), "smallest pre-y0 ratio, years after y2");
    st.put(&format!("ratio_before_{}_max_from_{}", y0.0, y2.0 + 1), max_of(r13.iter().map(|r| r.ratio_y0)), "largest pre-y0 ratio, years after y2");
    st.put("ratio_last6_min", min_of(rat_sk.iter().map(|r| r.ratio_last6)), "smallest last-month ratio over the ratio years");
    st.put("ratio_last6_max", max_of(rat_sk.iter().map(|r| r.ratio_last6)), "largest last-month ratio over the ratio years");
    st.put("ratio_last_band_min", min_of(rat_sk.iter().map(|r| r.ratio_last_band)), "smallest ratio of the last band before a year");
    st.put("ratio_last_band_max", max_of(rat_sk.iter().map(|r| r.ratio_last_band)), "largest ratio of the last band before a year");
    st.put(
        &format!("records_before_{}", y0.0),
        np_sum(&c.span(f64::NEG_INFINITY, y0.1 as f64).iter().map(|&i| c.rows[i]).collect::<Vec<_>>()),
        &format!("transactions spent in columns before {} (left out of the age table)", y0.0),
    );

    if let (Some(e), Some(tv), Some(age_ex), Some(rat_ex)) = (&ex, &tv, &age_ex, &rat_ex) {
        let tv_of = |sel: &[usize]| sel.iter().map(|&i| tv[i]).collect::<Vec<f64>>();
        let tv_ok = tv_of(&ok);
        let tv_late = tv_of(&late);
        st.put("rows_equal", 1.0, "the two surfaces kept the same records in every row");
        st.put("tv_med", np_median(&tv_ok), "median TV of a column, sketched against exact");
        st.put("tv_p90", np_quantile(&tv_ok, 0.9), "90th percentile of the column TV");
        st.put("tv_max", max_of(tv_ok.iter().copied()), "largest column TV");
        let rw: Vec<f64> = ok.iter().map(|&i| c.rows[i]).collect();
        st.put(
            "tv_weighted_mean",
            np_sum(&ok.iter().map(|&i| tv[i] * c.rows[i]).collect::<Vec<_>>()) / np_sum(&rw),
            "record-weighted mean column TV",
        );
        st.put("tv_early_max", max_of(tv_of(&early)), &format!("largest TV, columns spent before {}", y1.0));
        st.put("tv_late_min", min_of(tv_late.iter().copied()), &format!("smallest TV, columns spent from {}", y2.0));
        st.put("tv_late_med", np_median(&tv_late), &format!("median TV, columns spent from {}", y2.0));
        st.put("tv_late_max", max_of(tv_late.iter().copied()), &format!("largest TV, columns spent from {}", y2.0));
        st.put("rows_late_min", min_of(late.iter().map(|&i| c.rows[i])), &format!("fewest records of a column spent from {}", y2.0));
        let d = |i: usize, j: usize| p[i * n + j] - e.p[i * n + j];
        st.put("cell_max", max_of(ok.iter().flat_map(|&i| (0..n).map(move |j| d(i, j).abs()))), "largest |sketched - exact| of a cell");
        let nz: Vec<f64> =
            ok.iter().flat_map(|&i| (0..n).filter(move |&j| e.p[i * n + j] > 0.0).map(move |j| d(i, j).abs())).collect();
        st.put("cell_p99", np_quantile(&nz, 0.99), "99th percentile of |sketched - exact| over the cells where exact > 0");
        let big: Vec<usize> = ok.iter().copied().filter(|&i| c.rows[i] >= 100_000.0).collect();
        st.put("big_columns", big.len() as f64, "columns of at least 100,000 records");
        st.put("big_tv_med", np_median(&tv_of(&big)), "median TV over them");
        st.put("big_tv_max", max_of(tv_of(&big)), "largest TV over them");
        st.put("big_cell_max", max_of(big.iter().flat_map(|&i| (0..n).map(move |j| d(i, j).abs()))), "largest cell difference over them");
        // Ages, as gen_tables.py read them: sketched from blocks, exact from bands.
        let mean_ex = mean_ex.as_ref().unwrap();
        let med_ex = med_ex.as_ref().unwrap();
        let dmean: Vec<f64> = ok.iter().map(|&i| (mean_sk[i] - mean_ex[i]).abs()).collect();
        let dmed: Vec<f64> = ok.iter().map(|&i| (med_sk[i] - med_ex[i]).abs()).collect();
        st.put("dmean_max", max_of(dmean.iter().copied()), "largest |mean age| difference, sketched from blocks vs exact from bands (gen_tables.py)");
        st.put("dmean_med", np_median(&dmean), "its median");
        st.put("dmed_max", max_of(dmed.iter().copied()), "largest |median age| difference, the same readings (gen_tables.py)");
        st.put("dmed_med", np_median(&dmed), "its median");
        let quant_ex = quant_ex.as_ref().unwrap();
        st.put(
            "dmed_quantile_max",
            max_of(ok.iter().map(|&i| (quant[i][2] - quant_ex[i][2]).abs() / NOMINAL_YEAR)),
            "largest |median age| difference by figs_surface.py's quantiles, same readings",
        );
        // Ages as the text reads them: both as differences of bands.
        let dmean_b: Vec<f64> = ok.iter().map(|&i| (mean_skb[i] - mean_ex[i]).abs()).collect();
        st.put("dmean_bands_max", max_of(dmean_b.iter().copied()), "largest |mean age| difference, both ages from bands (the text's reading)");
        st.put(
            "dmed_bands_late_max",
            max_of(late.iter().map(|&i| (med_skb[i] - med_ex[i]).abs())),
            &format!("largest |median age| difference, both from bands, columns spent from {}", y2.0),
        );
        st.put(
            "dmed_bands_before_max",
            max_of(before_late.iter().map(|&i| (med_skb[i] - med_ex[i]).abs())),
            &format!("the same, columns spent before {}", y2.0),
        );
        // The worst of those columns, and the rest without it.
        let mut by: Vec<(f64, usize)> = before_late.iter().map(|&i| ((med_skb[i] - med_ex[i]).abs(), i)).collect();
        by.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        if let Some(&(_, i)) = by.first() {
            st.put("dmed_bands_before_worst_band", i as f64, "the column of dmed_bands_before_max");
            st.put("dmed_bands_before_worst_records", c.rows[i], "its records");
            st.put("dmed_bands_before_worst_cell_max", max_of((0..n).map(|j| d(i, j).abs())), "its largest cell difference");
            st.put("dmed_bands_before_worst_tv", tv[i], "its TV");
            st.put("dmed_bands_before_max_rest", by.get(1).map_or(f64::NAN, |x| x.0), "dmed_bands_before_max over the other columns");
        }
        let share_diff = age_skb
            .iter()
            .zip(age_ex.iter())
            .flat_map(|(a, b)| [(a.week - b.week).abs(), (a.year1 - b.year1).abs(), (a.over4 - b.over4).abs()])
            .fold(0.0, f64::max);
        st.put("age_table_bands_share_maxdiff", share_diff, "largest difference of the age table's three shares, both from bands");
        for (a, b) in age_skb.iter().zip(age_ex.iter()) {
            if a.year == y1.0 {
                st.put(&format!("under_week_{}_sketched_bands", a.year), a.week, "share under a week, sketched, ages from bands");
                st.put(&format!("under_week_{}_exact_bands", b.year), b.week, "share under a week, exact, ages from bands");
            }
        }
        st.put(
            &format!("colour_before_{}_maxdiff", y0.0),
            max_of(rat_sk.iter().zip(rat_ex.iter()).map(|(a, b)| (a.cuts[0].0 - b.cuts[0].0).abs())),
            "largest |sketched - exact| of the colour share before y0, over the ratio years",
        );
        if let (Some(a), Some(b)) = (rat_sk.last(), rat_ex.last()) {
            st.put(&format!("ratio_before_{}_{}_exact", y0.0, b.year), b.ratio_y0, "colour/supply before y0, exact");
            st.put(&format!("ratio_before_{}_{}_sketched", y0.0, a.year), a.ratio_y0, "colour/supply before y0, sketched");
        }
        // Per year.
        let mut w = create(&opts.out, "surface-tv-years.tsv")?;
        writeln!(w, "year\tcolumns\tmin_records\ttv_min\ttv_median\ttv_max\ttv_year")?;
        let mut tv_year = Vec::new();
        for i in 0..c.years.len() {
            let sel = c.year_sel(i);
            if sel.is_empty() {
                continue;
            }
            let (a, _) = c.year_avg(&p, &sel);
            let (b, _) = c.year_avg(&e.p, &sel);
            let ty = 0.5 * np_sum(&a.iter().zip(&b).map(|(x, y)| (x - y).abs()).collect::<Vec<_>>());
            if i >= 2 {
                tv_year.push(ty);
            }
            let t = tv_of(&sel);
            writeln!(
                w,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                c.years[i].0,
                sel.len(),
                min_of(sel.iter().map(|&s| c.rows[s])),
                fmt(min_of(t.iter().copied())),
                fmt(np_median(&t)),
                fmt(max_of(t.iter().copied())),
                fmt(ty)
            )?;
        }
        w.flush()?;
        st.put("tv_year_max", max_of(tv_year.iter().copied()), &format!("largest TV of a year-averaged colour, years from {}", y2.0));
        st.put("tv_year_min", min_of(tv_year.iter().copied()), &format!("smallest TV of a year-averaged colour, years from {}", y2.0));
    }
    let mut w = create(&opts.out, "surface-check.tsv")?;
    writeln!(w, "statistic\tvalue\twhat")?;
    for (k, v, what) in &st.0 {
        writeln!(w, "{}\t{}\t{}", k, fmt(*v), what)?;
    }
    w.flush()?;
    eprintln!(
        "sketch surface-report: {} columns, {} years, {} into {}",
        ok.len(),
        c.years.len(),
        if ex.is_some() { "with the exact surface" } else { "sketched only" },
        opts.out
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// numpy's own answers, from numpy 1.26.
    #[test]
    fn numpys_conventions() {
        // np.sum: pairwise above 8 values differs from a running sum.
        let v: Vec<f64> = (0..300).map(|i| 1.0 / (1.0 + i as f64).powi(3) * if i % 3 == 0 { 1e8 } else { 1.0 }).collect();
        let pw = np_sum(&v);
        assert!((pw - v.iter().sum::<f64>()).abs() < 1e-6);
        assert_eq!(np_sum(&[1.0, 2.0, 3.0]), 6.0);
        assert_eq!(np_sum(&[]), 0.0);
        // np.interp
        let xp = [0.0, 0.25, 0.25, 1.0];
        let fp = [1.0, 2.0, 3.0, 4.0];
        assert_eq!(np_interp(-1.0, &xp, &fp), 1.0);
        assert_eq!(np_interp(2.0, &xp, &fp), 4.0);
        assert_eq!(np_interp(1.0, &xp, &fp), 4.0);
        assert_eq!(np_interp(0.25, &xp, &fp), 3.0); // the last of equal xp
        assert_eq!(np_interp(0.125, &xp, &fp), 1.5);
        assert_eq!(np_interp(0.625, &xp, &fp), 3.5);
        // np.median, np.quantile(linear)
        assert_eq!(np_median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(np_median(&[4.0, 1.0, 2.0, 3.0]), 2.5);
        let v = [1.0, 2.0, 4.0, 8.0, 16.0];
        assert_eq!(np_quantile(&v, 0.9), 12.8); // np.quantile([1,2,4,8,16], 0.9)
        assert_eq!(np_quantile(&v, 0.0), 1.0);
        assert_eq!(np_quantile(&v, 1.0), 16.0);
        assert_eq!(np_quantile(&v, 0.3), 2.4);
        assert!(np_quantile(&[], 0.5).is_nan());
        // A NaN value or a NaN q gives NaN, as numpy (not the NaN sorted last).
        assert!(np_quantile(&[1.0, f64::NAN, 3.0], 0.0).is_nan());
        assert!(np_quantile(&v, f64::NAN).is_nan());
        assert!(np_median(&[1.0, 2.0, f64::NAN]).is_nan());
        assert_eq!(window_bands(745), 70);
    }

    /// The pairwise sum is numpy's: a vector whose running sum and pairwise
    /// sum differ, against `np.sum` (numpy 1.26 answers 1.0000000000000016).
    #[test]
    fn the_sum_is_pairwise_like_numpys() {
        let mut v = vec![1.0];
        v.extend(std::iter::repeat_n(1e-16, 16));
        // Running: 1 + 1e-16 rounds back to 1 sixteen times.
        assert_eq!(v.iter().sum::<f64>(), 1.0);
        // Pairwise: the eight lanes collect 1e-16s before meeting the 1.
        assert_eq!(np_sum(&v), 1.0000000000000016);
    }

    fn surface(rows: Vec<f64>, mint: Vec<f64>, age: Vec<f64>) -> Surface {
        Surface { width: 10, bands: rows.len(), mint, age, rows }
    }

    #[test]
    fn year_selection_and_tables_on_a_small_surface() {
        // Four bands of 10 blocks; rows 1..3 nonempty.
        let rows = vec![0.0, 2.0, 1.0, 4.0];
        #[rustfmt::skip]
        let mint = vec![
            0.0, 0.0, 0.0, 0.0,
            1.0, 1.0, 0.0, 0.0,
            0.5, 0.25, 0.25, 0.0,
            1.0, 1.0, 1.0, 1.0,
        ];
        let sf = surface(rows, mint, vec![0.0; 16]);
        let c = Ctx::new(&sf, vec![(1, 0), (2, 10), (3, 20)]).unwrap();
        assert_eq!(c.last, 4);
        assert_eq!(c.year_sel(0), Vec::<usize>::new());
        assert_eq!(c.year_sel(1), vec![1]);
        assert_eq!(c.year_sel(2), vec![2, 3]);
        let p = normalise(&sf.mint, 4, &sf.rows, 4);
        let (avg, tot) = c.year_avg(&p, &c.year_sel(2));
        assert_eq!(tot, 5.0);
        // (1 x [0.5 .25 .25 0] + 4 x [.25 .25 .25 .25]) / 5
        for (a, b) in avg.iter().zip([0.3, 0.25, 0.25, 0.2]) {
            assert!((a - b).abs() < 1e-15, "{:?}", avg);
        }
        let qb = band_ages(&p, 4);
        assert_eq!(&qb[4..8], &[0.5, 0.5, 0.0, 0.0]);
        assert_eq!(&qb[8..12], &[0.25, 0.25, 0.5, 0.0]);
        // Subsidy of 40 blocks, all before the first halving.
        assert_eq!(subsidy_bands(&c), vec![500.0; 4]);
        let rr = ratio_rows(&c, &p, &subsidy_bands(&c));
        assert_eq!(rr.len(), 1);
        assert_eq!(rr[0].top, 2);
        // Over bands 0 and 1: shares 0.3/0.55 and 0.25/0.55 against 1/2 each.
        assert!((rr[0].cuts[0].0 - 0.0).abs() < 1e-15 && rr[0].cuts[1].0 > 0.54 && rr[0].cuts[1].0 < 0.55);
        assert!((rr[0].cuts[1].1 - 0.5).abs() < 1e-15);
        assert!((rr[0].cuts[3].0 - 1.0).abs() < 1e-15 && rr[0].cuts[3].1 == 1.0);
        let md = ratio_table_md(&rr, false);
        assert_eq!(md, "| 3 | 0.0% / 0.0% | 54.5% / 50.0% | 100.0% / 100.0% | 100.0% / 100.0% |\n");
        // The calendar year before (year 2, bands 1..2) is band 1 alone.
        let md = ratio_table_md(&rr, true);
        assert_eq!(md, "| 3 | 0.0% / 0.0% | 54.5% / 50.0% | 100.0% / 100.0% | 45.5% / 50.0% |\n");
        assert_eq!(commas(117_811), "117,811");
        assert_eq!(commas(79_113_540), "79,113,540");
        assert_eq!(commas(12), "12");
    }

    #[test]
    fn quantiles_interpolate_inside_a_band() {
        let sf = surface(vec![1.0, 1.0], vec![1.0, 0.0, 0.5, 0.5], vec![1.0, 0.0, 0.5, 0.5]);
        let c = Ctx::new(&sf, vec![(1, 0), (2, 5), (3, 15)]).unwrap();
        let q = normalise(&sf.age, 2, &sf.rows, 2);
        let qs = c.quantiles(&q, &[0.25, 0.5, 0.75]);
        assert_eq!(qs[0], vec![2.5, 5.0, 7.5]);
        assert_eq!(qs[1], vec![5.0, 10.0, 15.0]);
        // The median by interpolation at the upper edges: 0.5 is the first CDF point.
        let m = c.median_ages(&q);
        assert_eq!(m[1], 10.0 / NOMINAL_YEAR);
    }

    /// A year whose first age band holds more than half its colour: the
    /// median lies inside that band, interpolated from 0 at age 0.
    #[test]
    fn the_year_median_starts_from_age_zero() {
        let sf = surface(vec![1.0, 1.0], vec![0.0; 4], vec![1.0, 0.0, 0.8, 0.2]);
        let c = Ctx::new(&sf, vec![(1, 0), (2, 10), (3, 20)]).unwrap();
        let q = normalise(&sf.age, 2, &sf.rows, 2);
        let r = age_rows(&c, &q);
        let y2 = r.iter().find(|r| r.year == 2).unwrap();
        // CDF 0.8 at the first upper edge, 10 blocks: the median at 0.5 / 0.8 of it.
        assert_eq!(y2.median, np_interp(0.5, &[0.0, 0.8, 1.0], &[0.0, 10.0 / NOMINAL_YEAR, 20.0 / NOMINAL_YEAR]));
        assert!((y2.median - 6.25 / NOMINAL_YEAR).abs() < 1e-18);
    }

    #[test]
    fn the_years_file_is_checked() {
        let d = std::env::temp_dir().join(format!("sketch-years-test-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("years.tsv");
        let p = p.to_str().unwrap();
        std::fs::write(p, "# the post's\nyear\tfirst_block\n2010\t32490\n2011\t100410\n2012\t160038\n").unwrap();
        assert_eq!(read_years(p).unwrap(), vec![(2010, 32490), (2011, 100410), (2012, 160038)]);
        std::fs::write(p, "year\tfirst_block\n2010\t32490\n2011\t100410\n").unwrap();
        assert!(read_years(p).is_err());
        std::fs::write(p, "year\tfirst_block\n2010\t32490\n2012\t100410\n2011\t160038\n").unwrap();
        assert!(read_years(p).is_err());
        std::fs::write(p, "year block\n2010\t32490\n").unwrap();
        assert!(read_years(p).is_err());
    }
}
