//! # `describe`: a few sketches, read one at a time against their binned colours
//!
//! `decode --select` cuts the production sketches of selected records out of
//! a whole-chain `SKETCH01` file, one text line each.  This reads those lines
//! and says, record by record, what each sketch is: how many terms and how
//! many distinct coefficients, how many entries the last reduction kept with
//! their own weight, the largest coefficient and the block that carries it,
//! the sketch's mean and newest block.  With `--bands` it sets every sketch
//! against the record's exact colour as `colors.bands1024.zst` holds it; with
//! `--records` it writes a few of them out in full, as polynomials.
//!
//! ```text
//! zstd -dc --long=27 colors.sketch-varopt512.zst | sketch decode --select sel.txt > sel.sketches.txt
//! sketch describe --in sel.sketches.txt --bands bands.sel.txt --counts sel.counts.tsv \
//!                 --select sel.txt --records 51126,99576 --out <dir>
//! ```
//!
//! Nothing is folded: every input is a text file of a few MB, and a run takes
//! well under a second.  The output is a function of the inputs alone.
//!
//! ## Against the exact colour: at the band edges only
//!
//! The exact colours of the whole chain are on disk only binned, into the
//! 1024 bands of 745 blocks of `--bands 1024` ([`crate::metrics::band`]).  So
//! the sketch is binned the same way and the two are compared on that grid:
//! `tv1024` is the total variation of the two binned distributions,
//! `ks_edges` the largest gap between the two CDFs *at the 1024 band edges*,
//! and `w1_edges` the area between the CDFs read on the band grid, in blocks
//! ([`crate::metrics::band_distances`]).  An error that stays inside one band
//! is invisible to all three: each is a lower bound on its block-level
//! counterpart of [`crate::metrics::distances`], not an estimate of it.  The
//! column names say so, and so does the comment line that opens
//! `describe.tsv` whenever it has them.
//!
//! ## Conventions, the ones `ten-colours/{stats,exact,gen}.py` used
//!
//! These are the scripts that first computed the post's section 11; the
//! conventions are theirs, so that the numbers come out the same to the last
//! digit printed, and each is a choice a reader may want to know about:
//!
//! - A coefficient is read as `f64` from the shortest decimal `decode` prints
//!   for the stored `f32` (what Python's `float()` does with it); `tau` too.
//! - The exact band masses are divided by their sum (they are `f32` and sum
//!   to one only to about seven digits).  The sketch is used as stored, *not*
//!   renormalised: its coefficients sum to one to about seven digits as well,
//!   and the band-edge KS of a sketch made of heavy entries is of that order
//!   (`3e-9` to `4e-7` on the post's ten), so normalising one side and not the
//!   other is visible there and nowhere else.
//! - `mean_block` is `sum_b c_b b` over the stored coefficients, again not
//!   divided by their mass.
//! - An entry is *heavy* when its coefficient exceeds `tau (1 + 1e-6)`, `tau`
//!   being the threshold of the last reduction: the lights were all set to
//!   `tau`, and the factor absorbs the `f32` rounding of the two.
//! - A sketch is *flat* when it was reduced and has one distinct coefficient:
//!   every entry light, every one at `tau = 1/k`.
//! - The largest coefficient's block is the oldest of the blocks that carry
//!   it; `top_band` is the first band of largest exact mass.
//! - Sums run in the order of the line (blocks decreasing) and over the bands
//!   in increasing order, as the scripts' loops did.  Where a script called
//!   Python's `sum()` -- the masses, the means, the TV, the exact mass past a
//!   band -- this calls [`sum_py`], the compensated (Neumaier) summation that
//!   `sum()` of floats has used since Python 3.12; where it accumulated in a
//!   loop -- the binning and the two CDFs of KS and W1 -- this does too.  The
//!   difference is in the last bits only, but it is the difference between
//!   reproducing `exact.json` to the bit and to fifteen digits.  The W1 of a
//!   sketch made of heavy entries is in fact mostly that: the sketch's mass
//!   falls short of one by about `4e-9`, and past the colour's newest band the
//!   two CDFs stay that far apart for every band to the last.
//!
//! ## Inputs
//!
//! - `--in`: `decode --select` lines, `rec reduced tau tx w:b,...`.
//! - `--bands`: `rec tx w:band,...` lines, the selected records' lines of
//!   `colors.bands1024.zst` with the record index in front -- what `pick
//!   --bands-ref` writes ([`crate::pick`]).  A line's tx must be the sketch's.
//! - `--counts`: a TSV whose header (after any `#` lines) names `rec` and any
//!   of `block`, `n_hat` and `group`: `decode --select` of a `KMVCNT01` count
//!   stream is one, and gives each record's block and support size.
//! - `--select`: the selection the sketches were cut by.  Every record it
//!   names must have a line in `--in`, only those are described, and its
//!   groups name them (over `--counts`'s `group`, when both have one).
//! - `--records`: the records to write out in full, in the order given.
//!
//! ## Files in `--out`
//!
//! - `describe.tsv`: one row per record, in the order of `--in`.
//! - `summary.tsv`: `scope stat value`, over every record (`all`) and over
//!   `--records` (`records`): how many were reduced and flat, the extremes of
//!   the term counts and of the mass, and of the band-edge distances.
//! - With `--records`: `colours.txt`, every term of each, as stored, equal
//!   coefficients grouped heaviest first; `displays.md`, each as a LaTeX
//!   display with its first four groups written out; `table.md`, one Markdown
//!   table row each.  The formatting is `gen.py`'s, Python's `%g` and
//!   round-half-even included ([`fmt_g`]).

use crate::metrics::{self, band, BANDS, BAND_WIDTH};
use crate::pick::Selection;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};

pub struct DescribeOpts {
    pub input: String,
    pub out: String,
    pub bands: Option<String>,
    pub counts: Option<String>,
    pub select: Option<String>,
    pub records: Vec<u64>,
}

fn bad(m: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, m)
}

fn open(path: &str) -> io::Result<BufReader<File>> {
    File::open(path).map(|f| BufReader::with_capacity(1 << 20, f)).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))
}

/// One line of `decode --select`.
#[derive(Clone, Debug, PartialEq)]
pub struct SelSketch {
    pub rec: u64,
    pub reduced: bool,
    pub tau: f64,
    pub tx: u64,
    /// `(coefficient as printed, its value, block)`, in the line's order
    /// (blocks decreasing).
    pub terms: Vec<(String, f64, u32)>,
}

/// `rec<TAB>reduced<TAB>tau<TAB>tx<TAB>w:b,...`.
pub fn parse_sketch_line(line: &str) -> Result<SelSketch, String> {
    let c: Vec<&str> = line.splitn(5, '\t').collect();
    if c.len() < 4 {
        return Err(format!("not a decode --select line: {:?}", line));
    }
    let int = |s: &str, what: &str| s.parse::<u64>().map_err(|_| format!("bad {} {:?}", what, s));
    let rec = int(c[0], "record index")?;
    let reduced = match c[1] {
        "0" => false,
        "1" => true,
        x => return Err(format!("record {}: reduced is {:?}, not 0 or 1", rec, x)),
    };
    let tau = c[2].parse::<f64>().map_err(|_| format!("record {}: bad tau {:?}", rec, c[2]))?;
    let tx = int(c[3], "tx")?;
    let mut terms = Vec::new();
    if let Some(t) = c.get(4).filter(|t| !t.is_empty()) {
        for term in t.split(',') {
            let (w, b) = term.split_once(':').ok_or_else(|| format!("record {}: bad term {:?}", rec, term))?;
            let v = w.parse::<f64>().map_err(|_| format!("record {}: bad coefficient {:?}", rec, w))?;
            let b = b.parse::<u32>().map_err(|_| format!("record {}: bad block {:?}", rec, b))?;
            terms.push((w.to_string(), v, b));
        }
    }
    Ok(SelSketch { rec, reduced, tau, tx, terms })
}

pub fn read_sketches(path: &str) -> io::Result<Vec<SelSketch>> {
    let mut out: Vec<SelSketch> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for line in open(path)?.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let s = parse_sketch_line(&line).map_err(|e| bad(format!("{}: {}", path, e)))?;
        if !seen.insert(s.rec) {
            return Err(bad(format!("{}: record {} appears twice", path, s.rec)));
        }
        out.push(s);
    }
    Ok(out)
}

/// An exact colour binned into the [`BANDS`] bands.
#[derive(Clone, Debug, PartialEq)]
pub struct Binned {
    pub tx: u64,
    /// The sum of the masses as stored, before normalising.
    pub raw_mass: f64,
    /// Mass per band, normalised to sum one.
    pub p: Vec<f64>,
}

/// `rec<TAB>tx<TAB>w:band,...`; a band named twice adds up.
pub fn parse_bands_line(line: &str) -> Result<(u64, Binned), String> {
    let c: Vec<&str> = line.splitn(3, '\t').collect();
    if c.len() < 2 {
        return Err(format!("not a bands line: {:?}", line));
    }
    let rec = c[0].parse::<u64>().map_err(|_| format!("bad record index {:?}", c[0]))?;
    let tx = c[1].parse::<u64>().map_err(|_| format!("record {}: bad tx {:?}", rec, c[1]))?;
    let mut p = vec![0.0f64; BANDS];
    if let Some(t) = c.get(2).filter(|t| !t.is_empty()) {
        for term in t.split(',') {
            let (w, b) = term.split_once(':').ok_or_else(|| format!("record {}: bad term {:?}", rec, term))?;
            let w = w.parse::<f64>().map_err(|_| format!("record {}: bad mass {:?}", rec, w))?;
            let b = b.parse::<usize>().ok().filter(|&b| b < BANDS).ok_or_else(|| format!("record {}: bad band {:?}", rec, b))?;
            p[b] += w;
        }
    }
    let raw_mass = sum_py(p.iter().copied());
    if raw_mass > 0.0 {
        for x in &mut p {
            *x /= raw_mass;
        }
    }
    Ok((rec, Binned { tx, raw_mass, p }))
}

/// Every line of a bands file, by record index.
pub fn read_bands(path: &str) -> io::Result<HashMap<u64, Binned>> {
    let mut out = HashMap::new();
    for line in open(path)?.lines() {
        let line = line?;
        if line.is_empty() {
            continue;
        }
        let (rec, b) = parse_bands_line(&line).map_err(|e| bad(format!("{}: {}", path, e)))?;
        if out.insert(rec, b).is_some() {
            return Err(bad(format!("{}: record {} appears twice", path, rec)));
        }
    }
    Ok(out)
}

/// What `--counts` says of a record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CountRow {
    pub block: Option<u32>,
    pub n_hat: Option<f64>,
    pub group: Option<String>,
}

/// A TSV with a header naming `rec` and any of `block`, `n_hat`, `group`;
/// lines before the header that start with `#` are skipped.
pub fn parse_counts_tsv(text: &str) -> Result<HashMap<u64, CountRow>, String> {
    let mut lines = text.lines().filter(|l| !l.starts_with('#') && !l.is_empty());
    let head: Vec<&str> = lines.next().ok_or("no header")?.split('\t').collect();
    let col = |n: &str| head.iter().position(|h| *h == n);
    let rec = col("rec").ok_or("the header names no rec column")?;
    let (block, n_hat, group) = (col("block"), col("n_hat"), col("group"));
    let mut out = HashMap::new();
    for l in lines {
        let c: Vec<&str> = l.split('\t').collect();
        let get = |i: Option<usize>| i.and_then(|i| c.get(i).copied());
        let r: u64 = get(Some(rec)).and_then(|x| x.parse().ok()).ok_or_else(|| format!("bad line {:?}", l))?;
        let row = CountRow {
            block: match get(block) {
                None => None,
                Some(x) => Some(x.parse().map_err(|_| format!("record {}: bad block {:?}", r, x))?),
            },
            n_hat: get(n_hat).and_then(|x| x.parse().ok()),
            group: get(group).map(|g| g.to_string()),
        };
        out.insert(r, row);
    }
    Ok(out)
}

/// What a sketch is, on its own.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub terms: usize,
    pub mass: f64,
    /// The distinct coefficients, heaviest first: as printed, value, and
    /// their blocks ascending.
    pub groups: Vec<(String, f64, Vec<u32>)>,
    pub heavy: usize,
    pub flat: bool,
    /// `sum_b c_b b`, not divided by the mass.
    pub mean: f64,
    pub oldest: Option<u32>,
    pub newest: Option<u32>,
}

impl Shape {
    pub fn distinct(&self) -> usize {
        self.groups.len()
    }
}

pub fn shape(s: &SelSketch) -> Shape {
    let mut by: HashMap<u64, usize> = HashMap::new();
    let mut groups: Vec<(String, f64, Vec<u32>)> = Vec::new();
    for (text, v, b) in &s.terms {
        let i = *by.entry(v.to_bits()).or_insert_with(|| {
            groups.push((text.clone(), *v, Vec::new()));
            groups.len() - 1
        });
        groups[i].2.push(*b);
    }
    groups.sort_by(|a, b| b.1.total_cmp(&a.1));
    for g in &mut groups {
        g.2.sort_unstable();
    }
    let heavy = s.terms.iter().filter(|t| t.1 > s.tau * 1.000001).count();
    Shape {
        terms: s.terms.len(),
        mass: sum_py(s.terms.iter().map(|t| t.1)),
        heavy,
        flat: s.reduced && groups.len() == 1,
        mean: sum_py(s.terms.iter().map(|t| t.1 * t.2 as f64)),
        oldest: s.terms.iter().map(|t| t.2).min(),
        newest: s.terms.iter().map(|t| t.2).max(),
        groups,
    }
}

/// The sketch against its exact colour, both binned (see the module docs:
/// at the band edges only).
#[derive(Clone, Debug, PartialEq)]
pub struct VsBands {
    pub tv: f64,
    pub ks_edges: f64,
    /// In blocks.
    pub w1_edges: f64,
    /// The first band of largest exact mass, and the two masses there.
    pub top_band: usize,
    pub top_exact: f64,
    pub top_sketch: f64,
    pub newest_band_exact: Option<usize>,
    pub newest_band_sketch: Option<usize>,
    /// The exact mass in bands after the sketch's newest.
    pub newer_exact: f64,
    pub nonzero_exact: usize,
    pub nonzero_sketch: usize,
    /// The exact colour's mean, each band at its middle block.
    pub mean_exact_bands: f64,
}

/// The sketch binned, in the order of its line.
pub fn bin_sketch(s: &SelSketch) -> Vec<f64> {
    let mut v = vec![0.0f64; BANDS];
    for t in &s.terms {
        v[band(t.2)] += t.1;
    }
    v
}

/// `(TV, KS, W1 in blocks)` of two binned distributions, read at the band
/// edges: [`crate::metrics::band_distances`], with W1 accumulated in blocks
/// band by band and the TV by [`sum_py`], as `exact.py` computed them.
pub fn edge_distances(e: &[f64], v: &[f64]) -> (f64, f64, f64) {
    let (mut fe, mut fs, mut ks, mut w1) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for (a, b) in e.iter().zip(v) {
        fe += a;
        fs += b;
        ks = ks.max((fe - fs).abs());
        w1 += (fe - fs).abs() * BAND_WIDTH as f64;
    }
    (0.5 * sum_py(e.iter().zip(v).map(|(a, b)| (a - b).abs())), ks, w1)
}

pub fn vs_bands(s: &SelSketch, x: &Binned) -> VsBands {
    let v = bin_sketch(s);
    let e = &x.p;
    let (tv, ks, w1) = edge_distances(e, &v);
    let mut top = 0;
    for i in 1..BANDS {
        if e[i] > e[top] {
            top = i;
        }
    }
    let newest_sk = s.terms.iter().map(|t| t.2).max().map(band);
    let newer = sum_py(e[newest_sk.map_or(0, |nb| nb + 1)..].iter().copied());
    let half = (BAND_WIDTH - 1) as f64 / 2.0;
    VsBands {
        tv,
        ks_edges: ks,
        w1_edges: w1,
        top_band: top,
        top_exact: e[top],
        top_sketch: v[top],
        newest_band_exact: (0..BANDS).rev().find(|&i| e[i] > 0.0),
        newest_band_sketch: newest_sk,
        newer_exact: newer,
        nonzero_exact: e.iter().filter(|&&a| a > 0.0).count(),
        nonzero_sketch: v.iter().filter(|&&a| a > 0.0).count(),
        mean_exact_bands: sum_py(e.iter().enumerate().map(|(i, a)| a * ((i as u32 * BAND_WIDTH) as f64 + half))),
    }
}

// ------------------------------------------------------------ Python's formats

/// Python's `sum()` of floats (3.12 and later): Neumaier's compensated
/// summation, in order, the compensation added at the end -- CPython's
/// `builtin_sum_impl` step for step, so the result is the same double.
pub fn sum_py(xs: impl IntoIterator<Item = f64>) -> f64 {
    let (mut s, mut c) = (0.0f64, 0.0f64);
    for x in xs {
        let t = s + x;
        if s.abs() >= x.abs() {
            c += (s - t) + x;
        } else {
            c += (x - t) + s;
        }
        s = t;
    }
    if c != 0.0 && c.is_finite() {
        s + c
    } else {
        s
    }
}

/// Python's `'%.{p}g' % x`: `p` significant digits, trailing zeros dropped,
/// exponent form below `1e-4` and from `10^p` up, a two-digit exponent.
pub fn fmt_g(x: f64, p: usize) -> String {
    let p = p.max(1);
    if x == 0.0 || !x.is_finite() {
        return if x == 0.0 { "0".into() } else { format!("{}", x) };
    }
    let e = format!("{:.*e}", p - 1, x);
    let (m, exp) = e.split_once('e').expect("an exponent");
    let exp: i32 = exp.parse().expect("an integer exponent");
    let trim = |s: &str| -> String {
        if s.contains('.') {
            s.trim_end_matches('0').trim_end_matches('.').to_string()
        } else {
            s.to_string()
        }
    };
    if exp < -4 || exp >= p as i32 {
        format!("{}e{}{:02}", trim(m), if exp < 0 { '-' } else { '+' }, exp.abs())
    } else {
        trim(&format!("{:.*}", (p as i32 - 1 - exp) as usize, x))
    }
}

/// [`fmt_g`] with an exponent written as `m \times 10^{e}`.
pub fn tex_g(x: f64, p: usize) -> String {
    let s = fmt_g(x, p);
    match s.split_once('e') {
        Some((m, e)) => format!("{} \\times 10^{{{}}}", m, e.parse::<i32>().expect("an exponent")),
        None => s,
    }
}

/// Python's `f'{n:,}'`.
pub fn thousands(n: i64) -> String {
    let d = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in d.chars().enumerate() {
        if i > 0 && (d.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{}", out)
    } else {
        out
    }
}

/// Python's `round(x)`: to the nearest integer, ties to even.
pub fn round_py(x: f64) -> i64 {
    x.round_ties_even() as i64
}

// ------------------------------------------------------------ the write-ups

/// Every term of a sketch, equal coefficients grouped heaviest first.
pub fn colour_text(s: &SelSketch, sh: &Shape, block: Option<u32>, group: &str) -> String {
    let poly: Vec<String> = sh
        .groups
        .iter()
        .map(|(a, _, bs)| {
            if bs.len() == 1 {
                format!("{}*x^{}", a, bs[0])
            } else {
                format!("{}*({})", a, bs.iter().map(|b| format!("x^{}", b)).collect::<Vec<_>>().join(" + "))
            }
        })
        .collect();
    format!(
        "record {}, tx {}, block {}, group {}: {} terms, {} distinct coefficients\nC(x) = {}",
        s.rec,
        s.tx,
        block.map_or("NA".into(), |b| b.to_string()),
        group,
        sh.terms,
        sh.distinct(),
        poly.join("\n  + ")
    )
}

/// The LaTeX display: the four heaviest groups, then the rest as `eta(x)`, with
/// its mass `eta(1)` (the post's letter; `gen.py` wrote `rho`).
pub fn display(s: &SelSketch, sh: &Shape) -> String {
    let flat = sh.distinct() == 1;
    let mut lines = Vec::new();
    let mut shown = 0;
    for (_, v, bs) in sh.groups.iter().take(4) {
        let c = if flat { format!("\\tfrac{{1}}{{{}}}", sh.terms) } else { tex_g(*v, 6) };
        let x = |b: &u32| format!("x^{{{}}}", b);
        lines.push(match bs.len() {
            1 => format!("{}\\,{}", c, x(&bs[0])),
            2 | 3 => format!("{}\\,({})", c, bs.iter().map(x).collect::<Vec<_>>().join(" + ")),
            n => format!(
                "{}\\,\\underbrace{{({} + {} + \\dots + {})}}_{{{}\\ \\text{{terms}}}}",
                c,
                x(&bs[0]),
                x(&bs[1]),
                x(&bs[n - 1]),
                n
            ),
        });
        shown += bs.len();
    }
    let rest = sh.terms - shown;
    if rest > 0 {
        let m = sum_py(sh.groups.iter().skip(4).map(|(_, v, bs)| v * bs.len() as f64));
        lines.push(format!(
            "\\eta(x), \\quad \\text{{{} terms with {} coefficients}},\\ \\eta(1) = {}",
            rest,
            sh.distinct() - 4,
            tex_g(m, 6)
        ));
    }
    format!("$$\n\\begin{{aligned}}\n\\hat C_{{{}}}(x) ={{}}& {}\n\\end{{aligned}}\n$$", s.rec, lines.join(" \\\\\n&+ "))
}

/// `$m \times 10^{e}$` for two significant digits, or the plain number.
fn sci2(x: f64) -> String {
    let s = fmt_g(x, 2);
    match s.split_once('e') {
        Some((m, e)) => format!("${} \\times 10^{{{}}}$", m, e.parse::<i32>().expect("an exponent")),
        None => s,
    }
}

/// One row of the table: record, block, group, distinct coefficients, heavy
/// entries, largest coefficient, mean and newest block, `n_hat`, band-edge
/// KS and W1.
pub fn table_row(s: &SelSketch, sh: &Shape, c: &CountRow, group: &str, x: Option<&VsBands>) -> String {
    let na = || "NA".to_string();
    // An empty sketch (no terms) has no largest coefficient: NA, not a panic.
    let (cl, at) = match sh.groups.first() {
        None => (na(), na()),
        Some((_, cmax, bs)) => (
            if sh.distinct() == 1 { format!("$1/{}$", sh.terms) } else { format!("{:.4}", cmax) },
            if bs.len() == 1 { thousands(bs[0] as i64) } else { format!("each of {}", bs.len()) },
        ),
    };
    format!(
        "| {} | {} | `{}` | {} | {} | {} at {} | {} | {} | {} | {} | {} |",
        thousands(s.rec as i64),
        c.block.map_or_else(na, |b| thousands(b as i64)),
        group,
        sh.distinct(),
        sh.heavy,
        cl,
        at,
        thousands(round_py(sh.mean)),
        sh.newest.map_or_else(na, |b| thousands(b as i64)),
        c.n_hat.map_or_else(na, |n| thousands(round_py(n))),
        x.map_or_else(na, |x| sci2(x.ks_edges)),
        x.map_or_else(na, |x| round_py(x.w1_edges).to_string()),
    )
}

// ------------------------------------------------------------ the run

struct Row<'a> {
    s: &'a SelSketch,
    sh: Shape,
    c: CountRow,
    group: String,
    x: Option<VsBands>,
}

/// `(min, max)` of the finite values, `NaN` for none.
fn extremes(v: impl Iterator<Item = f64>) -> (f64, f64) {
    v.filter(|x| x.is_finite())
        .fold((f64::NAN, f64::NAN), |(lo, hi), x| (if lo.is_nan() { x } else { lo.min(x) }, if hi.is_nan() { x } else { hi.max(x) }))
}

fn summarise(w: &mut impl Write, scope: &str, rows: &[&Row]) -> io::Result<()> {
    let f = metrics::fmt;
    let mut put = |stat: &str, v: String| writeln!(w, "{}\t{}\t{}", scope, stat, v);
    put("records", rows.len().to_string())?;
    put("reduced", rows.iter().filter(|r| r.s.reduced).count().to_string())?;
    put("flat", rows.iter().filter(|r| r.sh.flat).count().to_string())?;
    let (lo, hi) = extremes(rows.iter().map(|r| r.sh.terms as f64));
    put("terms_min", f(lo))?;
    put("terms_max", f(hi))?;
    let (lo, hi) = extremes(rows.iter().map(|r| r.sh.mass));
    put("mass_min", f(lo))?;
    put("mass_max", f(hi))?;
    if rows.iter().any(|r| r.x.is_some()) {
        for (name, g) in [
            ("tv1024", (|x: &VsBands| x.tv) as fn(&VsBands) -> f64),
            ("ks_edges", |x: &VsBands| x.ks_edges),
            ("w1_edges", |x: &VsBands| x.w1_edges),
        ] {
            let (lo, hi) = extremes(rows.iter().filter_map(|r| r.x.as_ref()).map(g));
            put(&format!("{}_min", name), f(lo))?;
            put(&format!("{}_max", name), f(hi))?;
        }
    }
    Ok(())
}

pub fn run(opts: DescribeOpts) -> io::Result<()> {
    let mut sketches = read_sketches(&opts.input)?;
    let sel = opts.select.as_deref().map(Selection::load).transpose()?;
    if let Some(sel) = &sel {
        let have: std::collections::HashSet<u64> = sketches.iter().map(|s| s.rec).collect();
        if let Some(r) = sel.recs.iter().find(|r| !have.contains(r)) {
            return Err(bad(format!("{}: the selection names record {}, which has no line", opts.input, r)));
        }
        sketches.retain(|s| sel.recs.binary_search(&s.rec).is_ok());
    }
    let counts = match &opts.counts {
        Some(p) => parse_counts_tsv(&std::fs::read_to_string(p).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", p, e)))?)
            .map_err(|e| bad(format!("{}: {}", p, e)))?,
        None => HashMap::new(),
    };
    let bands = opts.bands.as_deref().map(read_bands).transpose()?;

    let mut rows = Vec::with_capacity(sketches.len());
    for s in &sketches {
        let c = counts.get(&s.rec).cloned().unwrap_or_default();
        if opts.counts.is_some() && !counts.contains_key(&s.rec) {
            return Err(bad(format!("{}: no row for record {}", opts.counts.as_deref().unwrap_or(""), s.rec)));
        }
        let group = sel
            .as_ref()
            .and_then(|sel| sel.recs.binary_search(&s.rec).ok().map(|i| sel.groups[i].clone()))
            .or_else(|| c.group.clone())
            .unwrap_or_else(|| "NA".into());
        let x = match &bands {
            None => None,
            Some(b) => {
                let p = opts.bands.as_deref().unwrap_or("");
                let x = b.get(&s.rec).ok_or_else(|| bad(format!("{}: no line for record {}", p, s.rec)))?;
                if x.tx != s.tx {
                    return Err(bad(format!("{}: record {} is tx {} there but tx {} in {}", p, s.rec, x.tx, s.tx, opts.input)));
                }
                Some((vs_bands(s, x), x.raw_mass))
            }
        };
        rows.push((Row { s, sh: shape(s), c, group, x: x.as_ref().map(|x| x.0.clone()) }, x.map(|x| x.1)));
    }

    std::fs::create_dir_all(&opts.out)?;
    let f = metrics::fmt;
    let opt = |x: Option<f64>| x.map_or("NA".to_string(), f);
    let mut w = BufWriter::new(File::create(format!("{}/describe.tsv", opts.out))?);
    if bands.is_some() {
        writeln!(
            w,
            "# tv1024, ks_edges and w1_edges compare the sketch with the exact colour binned into {} bands of {} blocks, at the band edges only: lower bounds on the block-level TV, KS and W1, not estimates of them",
            BANDS, BAND_WIDTH
        )?;
    }
    write!(
        w,
        "rec\ttx\tgroup\tblock\treduced\ttau\tterms\tmass\tdistinct\theavy\tflat\tcmax\tcmax_block\tcmax_blocks\tmean_block\tage\toldest\tnewest\tn_hat"
    )?;
    if bands.is_some() {
        write!(w, "\tx_mass\ttv1024\tks_edges\tw1_edges\ttop_band\ttop_exact\ttop_sketch\tnewest_band_exact\tnewest_band_sketch\tnewer_exact\tbands_exact\tbands_sketch\tmean_exact_bands")?;
    }
    writeln!(w)?;
    for (r, xm) in &rows {
        let (_, cmax, bs) = &r.sh.groups.first().cloned().unwrap_or((String::new(), f64::NAN, vec![]));
        let ob = |b: Option<u32>| b.map_or("NA".to_string(), |b| b.to_string());
        write!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.s.rec,
            r.s.tx,
            r.group,
            ob(r.c.block),
            r.s.reduced as u8,
            f(r.s.tau),
            r.sh.terms,
            f(r.sh.mass),
            r.sh.distinct(),
            r.sh.heavy,
            r.sh.flat as u8,
            f(*cmax),
            ob(bs.first().copied()),
            bs.len(),
            f(r.sh.mean),
            opt(r.c.block.map(|b| b as f64 - r.sh.mean)),
            ob(r.sh.oldest),
            ob(r.sh.newest),
            opt(r.c.n_hat)
        )?;
        if let Some(x) = &r.x {
            let ou = |b: Option<usize>| b.map_or("NA".to_string(), |b| b.to_string());
            write!(
                w,
                "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                opt(*xm),
                f(x.tv),
                f(x.ks_edges),
                f(x.w1_edges),
                x.top_band,
                f(x.top_exact),
                f(x.top_sketch),
                ou(x.newest_band_exact),
                ou(x.newest_band_sketch),
                f(x.newer_exact),
                x.nonzero_exact,
                x.nonzero_sketch,
                f(x.mean_exact_bands)
            )?;
        }
        writeln!(w)?;
    }
    w.flush()?;

    let by_rec: HashMap<u64, usize> = rows.iter().enumerate().map(|(i, r)| (r.0.s.rec, i)).collect();
    let picked = opts
        .records
        .iter()
        .map(|r| by_rec.get(r).copied().ok_or_else(|| bad(format!("--records names {}, which is not described", r))))
        .collect::<io::Result<Vec<usize>>>()?;

    let mut w = BufWriter::new(File::create(format!("{}/summary.tsv", opts.out))?);
    writeln!(w, "scope\tstat\tvalue")?;
    let all: Vec<&Row> = rows.iter().map(|r| &r.0).collect();
    summarise(&mut w, "all", &all)?;
    if !picked.is_empty() {
        let some: Vec<&Row> = picked.iter().map(|&i| &rows[i].0).collect();
        summarise(&mut w, "records", &some)?;
    }
    w.flush()?;

    if !picked.is_empty() {
        let mut txt = BufWriter::new(File::create(format!("{}/colours.txt", opts.out))?);
        writeln!(
            txt,
            "Sketched colour polynomials of {} records, as `sketch decode --select` printed them (the f32\n\
             coefficients as stored).  The exponent of x is the block a coin was minted in; the coefficient\n\
             is the sketch's estimate of the share of the transaction's value minted there.  Terms with equal\n\
             coefficients are grouped, heaviest first.\n",
            picked.len()
        )?;
        let mut disp = BufWriter::new(File::create(format!("{}/displays.md", opts.out))?);
        let mut table = BufWriter::new(File::create(format!("{}/table.md", opts.out))?);
        writeln!(
            table,
            "| record | block | group | distinct coefficients | heavy entries | largest coefficient | mean block | newest block | $\\hat n$ | KS (band edges) | $W_1$ (band edges, blocks) |"
        )?;
        writeln!(table, "| ---: | ---: | :--- | ---: | ---: | :--- | ---: | ---: | ---: | ---: | ---: |")?;
        for (n, &i) in picked.iter().enumerate() {
            let r = &rows[i].0;
            if n > 0 {
                writeln!(txt)?;
                writeln!(disp)?;
            }
            writeln!(txt, "{}", colour_text(r.s, &r.sh, r.c.block, &r.group))?;
            writeln!(disp, "{}", display(r.s, &r.sh))?;
            writeln!(table, "{}", table_row(r.s, &r.sh, &r.c, &r.group, r.x.as_ref()))?;
        }
        txt.flush()?;
        disp.flush()?;
        table.flush()?;
    }
    eprintln!(
        "sketch describe: {} records{}, written to {}",
        rows.len(),
        if bands.is_some() { " against their binned colours" } else { "" },
        opts.out
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(rec: u64, reduced: u8, tau: &str, terms: &str) -> SelSketch {
        parse_sketch_line(&format!("{}\t{}\t{}\t{}\t{}", rec, reduced, tau, rec + 7, terms)).unwrap()
    }

    #[test]
    fn python_formats() {
        // '%.6g', '%.2g' and f'{:,}' as Python prints them.
        assert_eq!(fmt_g(0.994246123, 6), "0.994246");
        assert_eq!(fmt_g(8.254451e-5, 6), "8.25445e-05");
        assert_eq!(fmt_g(0.05, 6), "0.05");
        assert_eq!(fmt_g(0.0001, 6), "0.0001");
        assert_eq!(fmt_g(123456.7, 6), "123457");
        assert_eq!(fmt_g(1234567.0, 6), "1.23457e+06");
        assert_eq!(fmt_g(1.9726670963393644e-08, 2), "2e-08");
        assert_eq!(fmt_g(0.00028, 2), "0.00028");
        assert_eq!(fmt_g(0.0024999, 2), "0.0025");
        assert_eq!(fmt_g(0.0, 6), "0");
        assert_eq!(tex_g(8.254451e-5, 6), "8.25445 \\times 10^{-5}");
        assert_eq!(sci2(3.8279356467979975e-08), "$3.8 \\times 10^{-8}$");
        assert_eq!(sci2(0.0016), "0.0016");
        assert_eq!(thousands(778_613_383), "778,613,383");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(-1234), "-1,234");
        assert_eq!((round_py(0.5), round_py(1.5), round_py(2.5), round_py(2.6)), (0, 2, 2, 3));
        // sum(): Python 3.12 answers 1.0 and 1.0 where a plain loop answers
        // 0.9999999999999999 and 0.0.
        assert_eq!(sum_py([0.1; 10]), 1.0);
        assert_ne!([0.1f64; 10].iter().sum::<f64>(), 1.0);
        assert_eq!(sum_py([1e16, 1.0, -1e16]), 1.0);
        assert_eq!(sum_py(std::iter::empty()), 0.0);
    }

    #[test]
    fn shape_groups_counts_and_flags() {
        // Blocks decreasing, as decode prints them; tau 0.1.
        let s = line(5, 1, "0.1", "0.1:900,0.5:800,0.1:700,0.3:10");
        let sh = shape(&s);
        assert_eq!(sh.terms, 4);
        assert_eq!(sh.distinct(), 3);
        assert_eq!(sh.groups[0], ("0.5".to_string(), 0.5, vec![800]));
        assert_eq!(sh.groups[2], ("0.1".to_string(), 0.1, vec![700, 900]));
        assert_eq!(sh.heavy, 2);
        assert!(!sh.flat);
        assert!((sh.mass - 1.0).abs() < 1e-15);
        assert!((sh.mean - (90.0 + 400.0 + 70.0 + 3.0)).abs() < 1e-9);
        assert_eq!((sh.oldest, sh.newest), (Some(10), Some(900)));
        // A light entry equal to tau up to f32 rounding is not heavy.
        let flat = line(6, 1, "0.25", "0.25:4,0.25:3,0.25:2,0.25:1");
        let sh = shape(&flat);
        assert!(sh.flat && sh.heavy == 0 && sh.distinct() == 1);
        // Not reduced: equal coefficients do not make it flat.
        assert!(!shape(&line(7, 0, "0", "0.5:2,0.5:1")).flat);
    }

    #[test]
    fn against_bands_at_the_edges() {
        // Exact colour: band 0 and band 2, half each, stored unnormalised.
        let (rec, x) = parse_bands_line("5\t12\t1:2,0.5:0,0.5:0").unwrap();
        assert_eq!((rec, x.tx, x.raw_mass), (5, 12, 2.0));
        assert_eq!((x.p[0], x.p[2]), (0.5, 0.5));
        // Sketch: band 0 (block 744) and band 1 (blocks 745, 1000).
        let s = line(5, 1, "0.25", "0.25:1000,0.25:745,0.5:744");
        let v = vs_bands(&s, &x);
        assert_eq!(v.tv, 0.5);
        assert_eq!(v.ks_edges, 0.5);
        assert!((v.w1_edges - 0.5 * BAND_WIDTH as f64).abs() < 1e-9);
        assert_eq!((v.top_band, v.top_exact, v.top_sketch), (0, 0.5, 0.5));
        assert_eq!((v.newest_band_exact, v.newest_band_sketch), (Some(2), Some(1)));
        assert_eq!(v.newer_exact, 0.5);
        assert_eq!((v.nonzero_exact, v.nonzero_sketch), (2, 2));
        let half = (BAND_WIDTH - 1) as f64 / 2.0;
        assert!((v.mean_exact_bands - (0.5 * half + 0.5 * (2.0 * BAND_WIDTH as f64 + half))).abs() < 1e-9);
        // A sketch inside one band is invisible at the edges.
        let s = line(5, 1, "0.5", "0.5:700,0.5:3");
        let (_, x) = parse_bands_line("5\t12\t1:0").unwrap();
        let v = vs_bands(&s, &x);
        assert_eq!((v.tv, v.ks_edges, v.w1_edges), (0.0, 0.0, 0.0));
        assert!(parse_bands_line("5\t12\t1:1024").is_err());
    }

    #[test]
    fn counts_tsv_by_header() {
        let t = "# kmv-k1024-s1-sets k=1024\nrec\ttx\tblock\tuniv\tfull\tm\tn_hat\n3\t9\t100\t101\t0\t1024\t1506.5\n";
        let c = parse_counts_tsv(t).unwrap();
        assert_eq!(c[&3], CountRow { block: Some(100), n_hat: Some(1506.5), group: None });
        let c = parse_counts_tsv("rec\tgroup\tblock\tn_hat\n4\tchain\t7\tNA\n").unwrap();
        assert_eq!(c[&4], CountRow { block: Some(7), n_hat: None, group: Some("chain".into()) });
        assert!(parse_counts_tsv("tx\tblock\n").is_err());
    }

    #[test]
    fn write_ups_in_the_scripts_format() {
        let s = line(51126, 1, "0.002", "0.4:50,0.002:40,0.002:30,0.002:20,0.002:10,0.3:5,0.1:4,0.1:3,0.04:2,0.03:1,0.016:0");
        let sh = shape(&s);
        let c = CountRow { block: Some(50299), n_hat: Some(1506.49), group: None };
        let t = colour_text(&s, &sh, c.block, "chain");
        assert!(t.starts_with("record 51126, tx 51133, block 50299, group chain: 11 terms, 7 distinct coefficients\nC(x) = 0.4*x^50\n  + 0.3*x^5\n  + 0.1*(x^3 + x^4)\n"));
        let d = display(&s, &sh);
        assert!(d.contains("\\hat C_{51126}(x) ={}& 0.4\\,x^{50} \\\\\n&+ 0.3\\,x^{5} \\\\\n&+ 0.1\\,(x^{3} + x^{4}) \\\\\n&+ 0.04\\,x^{2} \\\\\n"), "{}", d);
        assert!(d.contains("\\eta(x), \\quad \\text{6 terms with 3 coefficients},\\ \\eta(1) = 0.054"), "{}", d);
        // Four blocks or more: the underbrace.
        let s4 = line(9, 1, "0.25", "0.25:4,0.25:3,0.25:2,0.25:1");
        let sh4 = shape(&s4);
        assert_eq!(
            display(&s4, &sh4),
            "$$\n\\begin{aligned}\n\\hat C_{9}(x) ={}& \\tfrac{1}{4}\\,\\underbrace{(x^{1} + x^{2} + \\dots + x^{4})}_{4\\ \\text{terms}}\n\\end{aligned}\n$$"
        );
        let r = table_row(&s4, &sh4, &CountRow::default(), "w2022", None);
        assert_eq!(r, "| 9 | NA | `w2022` | 1 | 0 | $1/4$ at each of 4 | 2 | 4 | NA | NA | NA |");
        let empty = line(10, 0, "0", "");
        assert_eq!(table_row(&empty, &shape(&empty), &CountRow::default(), "a", None), "| 10 | NA | `a` | 0 | 0 | NA at NA | 0 | NA | NA | NA | NA |");
        let r = table_row(&s, &sh, &c, "chain", None);
        assert!(r.starts_with("| 51,126 | 50,299 | `chain` | 7 | 7 | 0.4000 at 50 | 23 | 50 | 1,506 |"), "{}", r);
    }
}
