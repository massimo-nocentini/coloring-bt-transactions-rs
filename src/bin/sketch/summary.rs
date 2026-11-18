//! # `summary`: what stored count streams say, without a new fold
//!
//! `count` writes every record's KMV count as a fixed-width stream
//! ([`crate::kmv::CNT_MAGIC`]) and, as it goes, the tables it can fill on the
//! fly: the hard counters and strata against the exact sizes, and quantiles
//! per bucket of records.  What a whole-chain run is judged by after that --
//! the seeds against each other where no reference exists, the depth
//! profile of the counts, the RSE the production file actually carries, the
//! records counted exactly -- needs the streams side by side or a quantile
//! over a span longer than a bucket, and was first computed outside the
//! crate, by a `mawk` pass over three decoded streams and by Python over the
//! `count-rows` sample.  This mode reads the streams back and computes all
//! of it, so a count file on disk, `colors.kmv1024.counts.zst` among them,
//! can be summarised as often as wanted for the price of a decode:
//!
//! ```text
//! sketch summary --in counts.kmv-k1024-s1-sets.bin.zst [--in ... ] --out <dir>
//!                [--bucket 10000000] [--range <first>:<end> ...] [--every 1]
//!                [--limit <n>|all] [--sets-counts <path>]
//! ```
//!
//! ## Inputs
//!
//! Each `--in` is a count stream, plain, `.zst` (read through `zstd -dc`),
//! or `-` for standard input, which is also what no `--in` means.  Every
//! record is decoded by [`CntDecoder`], so both counts are the ones `decode`
//! prints, bit for bit ([`crate::kmv::stream_counts`] is the one function
//! both call), and the trailer is verified unless `--limit` stops the read
//! first.  Several streams must be the streams of one `count` run: they are
//! read in lock-step, and a record whose tx differs between them, or a
//! stream that ends before the others, is an error.  The same spec twice is
//! refused (its rows would be indistinguishable).
//!
//! `--sets-counts` is the exact `--sets` size of a prefix of the records,
//! one per line ([`crate::feed::spawn_counts`]), lined up by record index.
//! Where it exists it adds the reference's quantiles beside the estimates
//! and a check: `ref_bad` counts the records that break one of `count`'s
//! hard counters (a whole sketch whose length is not the reference -- for
//! `pos`, above it, since `pos` is a subset of `sets` -- or a full sketch of
//! a support of at most `k`).  A nonzero `ref_bad` fails the run, after the
//! tables are written: it means the file is not this stream's reference.
//!
//! `--every <n>` keeps only the records with `rec % n == 0`, `count
//! --every`'s sample: `--every 4001` over R4's streams summarises exactly the
//! rows of its `count-rows` files.  Everything, the seed comparison
//! included, is then over the sample.
//!
//! ## `summary.tsv`: one line per stream and scope
//!
//! The scopes are `all` (every record read), each `--range <first>:<end>`
//! (records `[first, end)`, `<first>:` to the end; clipped to what was read),
//! then each bucket of `--bucket` records.  Per scope:
//!
//! - `records` (summarised, so sampled), `full` and `whole` (a whole sketch
//!   holds its support: its count *is* `|S|`), `frac_whole`.
//! - `over_univ`: records whose restricted count exceeds the universe,
//!   `n_hat > beta + 1`.  Never, by construction (`R'' >= k - 1`): a check of
//!   the decoding.  `beta_first`, `beta_last`: the universe minus one at the
//!   scope's first and last record, the newest block seen.
//! - `n_p10 .. n_p99`: quantiles of `n_hat` over every record; `univ_p50`,
//!   the median universe `beta + 1`; `fill_p50`, the median of `n_hat /
//!   (beta + 1)` per record (not the ratio of the medians).
//! - `rse_p10, rse_p50, rse_p90`: the *relative* standard error `se / n_hat`
//!   over the full records.  `decode`'s `se` column is absolute, in blocks;
//!   this is the ratio, computed per record.  `rse_M_p50` is `se_M / n_hat_M`
//!   and `ratio_M_p50` the median `n_hat_M / n_hat`, both over full records.
//! - `whole_p50, whole_p90, whole_max`: the exact sizes of the whole records.
//! - `ref_records`, `ref_p10 .. ref_p99`, `ref_bad`: the reference, where
//!   `--sets-counts` covers the scope.
//!
//! ## Quantiles
//!
//! Linear interpolation between order statistics at `h = (n - 1) q`
//! (Hyndman and Fan's type 7), with numpy's own two-sided `lerp`, so that a
//! quantile here is numpy's `percentile(x, 100 q)` to the last bit: the
//! Python checks this mode replaces used numpy's default, and the two can
//! be compared without a convention in between.  `count`'s
//! `count-buckets.tsv` uses another one, the lower order statistic
//! `x[ceil(q n) - 1]` over `f32` values (type 1), so its quantiles and these
//! differ in about the sixth significant digit; neither is wrong.
//!
//! ## `dispersion.tsv`: seeds against each other
//!
//! Past the reference there is nothing to measure an error against, but two
//! seeds of one spec are independent replicates.  For every pair of streams
//! that differ only by seed, per bucket, over the records full in both: `x
//! = (n_a - n_b) / ((n_a + n_b) / 2)`, and against it the theory, the RMS
//! over records of `sqrt(((se_a / n_a)^2 + (se_b / n_b)^2) / 2)`.  Two
//! independent estimates with the theory's RSE give `RMS(x) / sqrt 2` equal
//! to it, so `rms_ratio` is about 1; `sd_ratio` uses the SD of `x` around
//! its bucket mean (population form) instead.  A bucket of fewer than two
//! such records has no line.  The `all` line pools the sums of the buckets
//! that have one (its `rms_ratio` is the `mawk` pass's pooled ratio; its
//! `sd_ratio` is around the pooled mean, not the buckets' means), and
//! carries the smallest and largest per-bucket ratios.
//!
//! This is reported, never gated, for the reason [`crate::count`] gives:
//! the records of one stream share one permutation, so within a bucket the
//! seed difference is mostly one offset shared by millions of records, and
//! a bucket (or the chain) is about one draw of it.  The gap between
//! `rms_ratio` and `sd_ratio` measures exactly that offset.
//!
//! ## Cost
//!
//! Twelve bytes a record a stream are held until the end ([`Kept`]): 9.3 GB
//! for one whole-chain stream at `--every 1`.  The quantiles are then taken
//! one thread per stream, each with a scratch of 8 bytes a record of its
//! largest scope.  `select_nth_unstable` makes a quantile linear in the
//! scope; reading is bound by `zstd -dc` and the decoder's Fenwick query.

use crate::count::unique_labels;
use crate::feed::{self, CountBatch, BATCH};
use crate::kmv::{stream_counts, CntDecoder, CntRow, KmvSpec, Support, CNT_MAGIC, FULL_BIT};
use crate::metrics;
use std::fs::File;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::Child;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

pub struct SummaryOpts {
    /// Count streams; `-` is standard input.  Empty means `-`.
    pub inputs: Vec<String>,
    pub out: String,
    pub limit: u64,
    pub every: u64,
    pub bucket: u64,
    pub ranges: Vec<Range>,
    pub sets_counts: Option<String>,
}

/// `--range <first>:<end>`: records `[first, end)`; `<first>:` runs to the
/// end of what is read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Range {
    pub first: u64,
    pub end: u64,
}

impl Range {
    pub fn parse(text: &str) -> Result<Range, String> {
        let bad = || format!("--range {:?}: expected <first>:<end> or <first>:, record indices", text);
        let (a, b) = text.split_once(':').ok_or_else(bad)?;
        let first: u64 = a.parse().map_err(|_| bad())?;
        let end: u64 = if b.is_empty() { u64::MAX } else { b.parse().map_err(|_| bad())? };
        if end <= first {
            return Err(format!("--range {:?}: empty, its end must be above its first record", text));
        }
        Ok(Range { first, end })
    }
}

// ------------------------------------------------------------------ quantiles

/// numpy's `_lerp`: from `a` below `t = 1/2` and from `b` above, which is
/// what makes it exact at both ends and monotone in `t`.
fn lerp(a: f64, b: f64, t: f64) -> f64 {
    let d = b - a;
    if t >= 0.5 {
        b - d * (1.0 - t)
    } else {
        a + d * t
    }
}

/// The `q`-quantile of `v` by linear interpolation at `h = (n - 1) q`,
/// numpy's default (`method="linear"`, type 7), computed the way numpy does
/// it: the order statistics `floor(h)` and the next, and [`lerp`] at the
/// fractional part.  Reorders `v`; NaN for an empty slice, a NaN `q` or a
/// NaN in `v`, as numpy.  Values are compared by `total_cmp`.
pub fn quantile(v: &mut [f64], q: f64) -> f64 {
    let n = v.len();
    if n == 0 || q.is_nan() || v.iter().any(|x| x.is_nan()) {
        return f64::NAN;
    }
    let h = (n - 1) as f64 * q;
    if h >= (n - 1) as f64 {
        return v.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    }
    let lo = h.floor() as usize;
    let (_, a, right) = v.select_nth_unstable_by(lo, |x, y| x.total_cmp(y));
    let a = *a;
    let b = right.iter().copied().fold(f64::INFINITY, f64::min);
    lerp(a, b, h - lo as f64)
}

const QS: [f64; 4] = [0.1, 0.5, 0.9, 0.99];

fn quantiles(v: &mut [f64], qs: &[f64]) -> Vec<f64> {
    qs.iter().map(|&q| quantile(v, q)).collect()
}

// ------------------------------------------------------------------ reading

/// What a record keeps until the end: the universe, the restricted rank
/// `R''` (full, with [`FULL_BIT`]) or the length (whole), and `rk`.  With
/// the stream's `k` and `M` that is all of [`stream_counts`]'s input, so
/// every count is recomputed exactly as the decoder computed it.  `R''` is
/// at most the universe, below `2^26` ([`crate::hash::Perm::MAX_BITS`]), so
/// bit 31 is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Kept {
    univ: u32,
    a: u32,
    rk: u32,
}

impl Kept {
    pub fn of(r: &CntRow) -> Kept {
        let a = if r.r.full() { r.below as u32 | FULL_BIT } else { r.r.len() };
        Kept { univ: r.n_univ as u32, a, rk: r.r.rk }
    }
    fn full(&self) -> bool {
        self.a & FULL_BIT != 0
    }
    /// `[n_hat, se, n_hat_M, se_M]`, as [`CntDecoder::next`] gave them.
    pub fn counts(&self, k: usize, m: u64) -> [f64; 4] {
        let v = (self.a & !FULL_BIT) as u64;
        stream_counts(k, m, self.univ as u64, self.full(), v as u32, self.rk, v)
    }
}

type Dec = CntDecoder<BufReader<Box<dyn Read + Send>>>;

fn named(path: &str, e: io::Error) -> io::Error {
    io::Error::new(e.kind(), format!("{}: {}", path, e))
}

/// Open a count stream and read its header: the decoder, and the `zstd`
/// child behind it when the path ends in `.zst`.
fn open(path: &str) -> io::Result<(Dec, Option<Child>)> {
    let (src, child): (Box<dyn Read + Send>, Option<Child>) = if path == "-" {
        (Box::new(io::stdin()), None)
    } else if path.ends_with(".zst") {
        let mut c = feed::decompress(path)?;
        let out = c.stdout.take().expect("piped");
        (Box::new(out), Some(c))
    } else {
        (Box::new(File::open(path).map_err(|e| named(path, e))?), None)
    };
    let mut r = BufReader::with_capacity(1 << 20, src);
    let mut magic = [0u8; 8];
    let not_counts = || {
        io::Error::new(io::ErrorKind::InvalidData, format!("{}: not a count stream (no KMVCNT01 header)", path))
    };
    r.read_exact(&mut magic).map_err(|_| not_counts())?;
    if &magic != CNT_MAGIC {
        return Err(not_counts());
    }
    let dec = CntDecoder::new(r, magic, None).map_err(|e| named(path, e))?;
    Ok((dec, child))
}

/// How a stream's read ended: records read, and whether its trailer was
/// reached and verified (not when `--limit` stopped it first).
struct ReadEnd {
    records: u64,
    trailer: bool,
}

/// A reader thread: decodes `dec` in batches of [`BATCH`] rows, on the same
/// boundaries as every other stream and as `--sets-counts`, until the
/// trailer or `limit`.  A `zstd` child is killed when the read stops early
/// and waited for otherwise, and its failure is the thread's.
fn spawn_reader(path: String, mut dec: Dec, child: Option<Child>, limit: u64, tx: SyncSender<Arc<Vec<CntRow>>>) -> JoinHandle<io::Result<ReadEnd>> {
    std::thread::Builder::new()
        .name(format!("read-{}", dec.spec.label()))
        .spawn(move || {
            let mut read = || -> io::Result<ReadEnd> {
                let mut b = Vec::with_capacity(BATCH);
                let mut trailer = false;
                while dec.records < limit {
                    match dec.next()? {
                        Some(r) => b.push(r),
                        None => {
                            trailer = true;
                            break;
                        }
                    }
                    if b.len() == BATCH && tx.send(Arc::new(std::mem::replace(&mut b, Vec::with_capacity(BATCH)))).is_err() {
                        return Ok(ReadEnd { records: dec.records, trailer: false });
                    }
                }
                if !b.is_empty() {
                    let _ = tx.send(Arc::new(b));
                }
                Ok(ReadEnd { records: dec.records, trailer })
            };
            let result = read();
            if let Some(mut c) = child {
                let finished = matches!(result, Ok(ReadEnd { trailer: true, .. }));
                if !finished {
                    let _ = c.kill();
                }
                let st = c.wait();
                if let (true, Ok(st)) = (finished, st) {
                    if !st.success() {
                        return Err(io::Error::other(format!("{}: zstd -dc exited with {}", path, st)));
                    }
                }
            }
            result.map_err(|e| named(&path, e))
        })
        .expect("spawn a count-stream reader")
}

// ------------------------------------------------------------------ seeds

#[derive(Clone, Copy, Default)]
struct Disp {
    c: u64,
    sx: f64,
    sxx: f64,
    th: f64,
}

/// Two streams that differ only by seed, and their sums per bucket.
struct Pair {
    a: usize,
    b: usize,
    family: String,
    name: String,
    buckets: Vec<Disp>,
}

impl Pair {
    /// One record of both streams; only records full in both count.
    fn add(&mut self, bucket: usize, x: &CntRow, y: &CntRow) {
        if !x.r.full() || !y.r.full() {
            return;
        }
        if self.buckets.len() <= bucket {
            self.buckets.resize(bucket + 1, Disp::default());
        }
        let d = &mut self.buckets[bucket];
        let (n1, n2) = (x.n_hat, y.n_hat);
        let e = (n1 - n2) / ((n1 + n2) / 2.0);
        d.c += 1;
        d.sx += e;
        d.sxx += e * e;
        d.th += ((x.se / n1).powi(2) + (y.se / n2).powi(2)) / 2.0;
    }
}

/// The seed-replicate key of a spec: everything but the seed.
fn family_key(s: &KmvSpec) -> (usize, Support, u32) {
    (s.k, s.support, s.bits)
}

pub const DISPERSION_HEADER: &str = "family\tpair\tbucket\tfirst\tend\trecords\tsd_over_sqrt2\trms_over_sqrt2\ttheory_rse\trms_ratio\tsd_ratio\tmin_rms_ratio\tmax_rms_ratio\tmin_sd_ratio\tmax_sd_ratio";

/// `(sd, rms, theory)` of `c` records with sums `sx`, `sxx`, `th`, in the
/// forms of the `mawk` pass this replaces: `sqrt((sxx/c - m^2) / 2)`,
/// `sqrt(sxx/c / 2)`, `sqrt(th/c)`.
fn disp_stats(c: f64, sx: f64, sxx: f64, th: f64) -> (f64, f64, f64) {
    let m = sx / c;
    ((((sxx / c) - m * m) / 2.0).sqrt(), (sxx / c / 2.0).sqrt(), (th / c).sqrt())
}

fn write_dispersion(w: &mut impl Write, p: &Pair, bucket: u64, records: u64) -> io::Result<()> {
    let f = metrics::fmt;
    let (mut c, mut sx, mut sxx, mut th) = (0u64, 0.0, 0.0, 0.0);
    let (mut rmin, mut rmax, mut smin, mut smax) = (f64::INFINITY, f64::NEG_INFINITY, f64::INFINITY, f64::NEG_INFINITY);
    for (b, d) in p.buckets.iter().enumerate() {
        if d.c < 2 {
            continue;
        }
        let (sd, rms, t) = disp_stats(d.c as f64, d.sx, d.sxx, d.th);
        let first = b as u64 * bucket;
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\tNA\tNA\tNA\tNA",
            p.family,
            p.name,
            b,
            first,
            (first + bucket).min(records),
            d.c,
            f(sd),
            f(rms),
            f(t),
            f(rms / t),
            f(sd / t)
        )?;
        rmin = rmin.min(rms / t);
        rmax = rmax.max(rms / t);
        smin = smin.min(sd / t);
        smax = smax.max(sd / t);
        c += d.c;
        sx += d.sx;
        sxx += d.sxx;
        th += d.th;
    }
    if c == 0 {
        return writeln!(w, "{}\t{}\tall\t0\t{}\t0\tNA\tNA\tNA\tNA\tNA\tNA\tNA\tNA\tNA", p.family, p.name, records);
    }
    let (sd, rms, t) = disp_stats(c as f64, sx, sxx, th);
    writeln!(
        w,
        "{}\t{}\tall\t0\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        p.family,
        p.name,
        records,
        c,
        f(sd),
        f(rms),
        f(t),
        f(rms / t),
        f(sd / t),
        f(rmin),
        f(rmax),
        f(smin),
        f(smax)
    )
}

// ------------------------------------------------------------------ scopes

struct Stream {
    spec: KmvSpec,
    m: u64,
    kept: Vec<Kept>,
}

pub const SUMMARY_HEADER: &str = "label\tsupport\tscope\tfirst\tend\trecords\tfull\twhole\tfrac_whole\tover_univ\tbeta_first\tbeta_last\tn_p10\tn_p50\tn_p90\tn_p99\tuniv_p50\tfill_p50\trse_p10\trse_p50\trse_p90\trse_M_p50\tratio_M_p50\twhole_p50\twhole_p90\twhole_max\tref_records\tref_p10\tref_p50\tref_p90\tref_p99\tref_bad";

/// The kept index of the first sampled record at or after `rec`.
fn kept_index(rec: u64, every: u64) -> usize {
    rec.div_ceil(every) as usize
}

/// One line of `summary.tsv`, and its `ref_bad`: the records `[first,
/// end)` of `st`, which are its kept `[lo, hi)`; `refs` lines up with
/// `kept` over its prefix.
fn scope_line(st: &Stream, refs: &[u32], scope: &str, first: u64, end: u64, every: u64, scratch: &mut Vec<f64>) -> (String, u64) {
    let f = metrics::fmt;
    let (k, m) = (st.spec.k, st.m);
    let lo = kept_index(first, every).min(st.kept.len());
    let hi = kept_index(end, every).min(st.kept.len()).max(lo);
    let kept = &st.kept[lo..hi];
    let n = kept.len() as u64;
    let full = kept.iter().filter(|x| x.full()).count() as u64;
    let mut take = |vals: &mut dyn Iterator<Item = f64>, qs: &[f64]| {
        scratch.clear();
        scratch.extend(vals);
        quantiles(scratch, qs)
    };
    let over = kept.iter().filter(|x| x.counts(k, m)[0] > x.univ as f64).count();
    let nq = take(&mut kept.iter().map(|x| x.counts(k, m)[0]), &QS);
    let univ = take(&mut kept.iter().map(|x| x.univ as f64), &[0.5]);
    let fill = take(&mut kept.iter().map(|x| x.counts(k, m)[0] / x.univ as f64), &[0.5]);
    let fulls = || kept.iter().filter(|x| x.full()).map(|x| x.counts(k, m));
    let rse = take(&mut fulls().map(|c| c[1] / c[0]), &[0.1, 0.5, 0.9]);
    let rse_m = take(&mut fulls().map(|c| c[3] / c[2]), &[0.5]);
    let ratio_m = take(&mut fulls().map(|c| c[2] / c[0]), &[0.5]);
    let whole = take(&mut kept.iter().filter(|x| !x.full()).map(|x| x.counts(k, m)[0]), &[0.5, 0.9, 1.0]);
    let r = &refs[lo.min(refs.len())..hi.min(refs.len())];
    let refq = take(&mut r.iter().map(|&v| v as f64), &QS);
    let sets = st.spec.support == Support::Sets;
    let bad = r
        .iter()
        .zip(kept)
        .filter(|(&v, x)| {
            if x.full() {
                v as usize <= k
            } else {
                let len = x.a;
                if sets {
                    len != v
                } else {
                    len > v
                }
            }
        })
        .count() as u64;
    let beta = |x: Option<&Kept>| x.map_or("NA".to_string(), |x| (x.univ as u64 - 1).to_string());
    let mut line = format!(
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        st.spec.label(),
        st.spec.support.name(),
        scope,
        first,
        end,
        n,
        full,
        n - full,
        f((n - full) as f64 / n as f64),
        over,
        beta(kept.first()),
        beta(kept.last())
    );
    for v in nq.iter().chain(&univ).chain(&fill).chain(&rse).chain(&rse_m).chain(&ratio_m).chain(&whole) {
        line.push('\t');
        line.push_str(&f(*v));
    }
    line.push_str(&format!("\t{}", r.len()));
    for v in &refq {
        line.push('\t');
        line.push_str(&f(*v));
    }
    line.push_str(&format!("\t{}", if r.is_empty() { "NA".to_string() } else { bad.to_string() }));
    (line, bad)
}

/// Every line of one stream: `all`, the ranges, the buckets.
fn stream_lines(st: &Stream, refs: &[u32], records: u64, opts: &SummaryOpts) -> (Vec<String>, u64) {
    let every = opts.every.max(1);
    let mut scratch = Vec::new();
    let mut lines = Vec::new();
    let (line, bad) = scope_line(st, refs, "all", 0, records, every, &mut scratch);
    lines.push(line);
    for r in &opts.ranges {
        let (first, end) = (r.first.min(records), r.end.min(records));
        lines.push(scope_line(st, refs, "range", first, end, every, &mut scratch).0);
    }
    let bucket = opts.bucket.max(1);
    for b in 0..records.div_ceil(bucket) {
        let first = b * bucket;
        lines.push(scope_line(st, refs, "bucket", first, (first + bucket).min(records), every, &mut scratch).0);
    }
    (lines, bad)
}

// ------------------------------------------------------------------ run

pub fn run(opts: SummaryOpts) -> io::Result<()> {
    let invalid = |m: String| io::Error::new(io::ErrorKind::InvalidInput, m);
    let inputs = if opts.inputs.is_empty() { vec!["-".to_string()] } else { opts.inputs.clone() };
    if inputs.iter().filter(|p| p.as_str() == "-").count() > 1 {
        return Err(invalid("summary: standard input (--in -) can be read only once".into()));
    }
    let every = opts.every.max(1);
    let bucket = opts.bucket.max(1);
    let mut opened = Vec::new();
    for p in &inputs {
        opened.push(open(p)?);
    }
    unique_labels("summary", "--in", opened.iter().map(|(d, _)| d.spec.label()))?;
    std::fs::create_dir_all(&opts.out)?;

    let mut streams = Vec::new();
    let mut rxs: Vec<Receiver<Arc<Vec<CntRow>>>> = Vec::new();
    let mut readers = Vec::new();
    for ((dec, child), path) in opened.into_iter().zip(&inputs) {
        streams.push(Stream { spec: dec.spec, m: dec.m, kept: Vec::new() });
        let (tx, rx) = sync_channel(8);
        rxs.push(rx);
        readers.push(spawn_reader(path.clone(), dec, child, opts.limit, tx));
    }
    let mut pairs = Vec::new();
    for a in 0..streams.len() {
        for b in a + 1..streams.len() {
            let (x, y) = (&streams[a].spec, &streams[b].spec);
            if family_key(x) == family_key(y) {
                pairs.push(Pair {
                    a,
                    b,
                    family: metrics::family(&x.label()),
                    name: format!("s{}-s{}", x.seed, y.seed),
                    buckets: Vec::new(),
                });
            }
        }
    }
    let (mut ref_rx, ref_handle) = match &opts.sets_counts {
        Some(p) => {
            let (tx, rx) = sync_channel::<Arc<CountBatch>>(8);
            (Some(rx), Some(feed::spawn_counts(p.clone(), opts.limit, vec![tx])))
        }
        None => (None, None),
    };

    // Lock-step: batch i of every stream, and of the reference, together.
    let mut refs: Vec<u32> = Vec::new();
    let mut first = 0u64;
    let mut uneven = false;
    let mut failed = None;
    loop {
        let got: Vec<Arc<Vec<CntRow>>> = rxs.iter().filter_map(|rx| rx.recv().ok()).collect();
        if got.is_empty() {
            break;
        }
        if got.len() < rxs.len() || got.iter().any(|b| b.len() != got[0].len()) {
            uneven = true;
            break;
        }
        let rb = ref_rx.as_ref().and_then(|rx| rx.recv().ok());
        if rb.is_none() {
            ref_rx = None;
        }
        if let Some(b) = &rb {
            if b.first != first {
                failed = Some(io::Error::new(io::ErrorKind::InvalidData, format!("sets-counts batch at {} for records at {}", b.first, first)));
                break;
            }
        }
        for i in 0..got[0].len() {
            let rec = first + i as u64;
            let tx = got[0][i].r.tx;
            if let Some(s) = got.iter().position(|b| b[i].r.tx != tx) {
                failed = Some(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "record {}: tx {} in {} but {} in {}; --in takes the streams of one count run",
                        rec, tx, inputs[0], got[s][i].r.tx, inputs[s]
                    ),
                ));
                break;
            }
            if !rec.is_multiple_of(every) {
                continue;
            }
            if let Some(v) = rb.as_ref().and_then(|b| b.vals.get(i)) {
                if refs.len() == streams[0].kept.len() {
                    refs.push(*v);
                }
            }
            for (s, st) in streams.iter_mut().enumerate() {
                st.kept.push(Kept::of(&got[s][i]));
            }
            let b = (rec / bucket) as usize;
            for p in pairs.iter_mut() {
                p.add(b, &got[p.a][i], &got[p.b][i]);
            }
        }
        if failed.is_some() {
            break;
        }
        first += got[0].len() as u64;
    }
    // Closing the channels stops readers that are still going; then a
    // reader's own error explains a stream that "ended early".
    drop(rxs);
    drop(ref_rx);
    let mut ends = Vec::new();
    for h in readers {
        match h.join().expect("a count-stream reader panicked") {
            Ok(e) => ends.push(e),
            Err(e) => {
                failed.get_or_insert(e);
            }
        }
    }
    let ref_result = ref_handle.map(|h| h.join().expect("the sets-counts reader panicked"));
    if let Some(e) = failed {
        return Err(e);
    }
    if let Some(Err(e)) = ref_result {
        return Err(e);
    }
    if uneven {
        let lens: Vec<String> = inputs.iter().zip(&ends).map(|(p, e)| format!("{} {}", p, e.records)).collect();
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the streams hold different numbers of records ({}); --in takes the streams of one count run", lens.join(", ")),
        ));
    }
    let records = first;

    let results: Vec<(Vec<String>, u64)> = std::thread::scope(|sc| {
        let hs: Vec<_> = streams.iter().map(|st| sc.spawn(|| stream_lines(st, &refs, records, &opts))).collect();
        hs.into_iter().map(|h| h.join().expect("a summary thread panicked")).collect()
    });
    let mut w = BufWriter::new(File::create(format!("{}/summary.tsv", opts.out))?);
    writeln!(w, "{}", SUMMARY_HEADER)?;
    let mut bad = 0;
    for (lines, b) in &results {
        bad += b;
        for l in lines {
            writeln!(w, "{}", l)?;
        }
    }
    w.flush()?;

    let disp_path = format!("{}/dispersion.tsv", opts.out);
    if pairs.is_empty() {
        let _ = std::fs::remove_file(&disp_path);
    } else {
        let mut w = BufWriter::new(File::create(&disp_path)?);
        writeln!(w, "{}", DISPERSION_HEADER)?;
        for p in &pairs {
            write_dispersion(&mut w, p, bucket, records)?;
        }
        w.flush()?;
    }
    let verified = ends.iter().all(|e| e.trailer);
    eprintln!(
        "sketch summary: {} records of {} stream(s), {} summarised (every {}), {} seed pair(s){}, written to {}",
        records,
        streams.len(),
        streams[0].kept.len(),
        every,
        pairs.len(),
        if verified { "; every trailer verified" } else { "; stopped by --limit, trailers not read" },
        opts.out
    );
    if bad > 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} record(s) disagree with --sets-counts; see ref_bad in {}/summary.tsv", bad, opts.out),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// numpy 1.26.4's `percentile`, method "linear", on the same inputs.
    #[test]
    fn quantile_is_numpys_linear() {
        let mut v = vec![1.0, 2.0, 3.0, 4.0];
        let got: Vec<f64> = [0.1, 0.5, 0.9, 0.99, 1.0, 0.0].iter().map(|&q| quantile(&mut v, q)).collect();
        assert_eq!(got, vec![1.3, 2.5, 3.7, 3.9699999999999998, 4.0, 1.0]);
        let mut v = vec![5.0, 1.0, 4.0, 2.0, 3.0, 9.0, 7.0];
        let got: Vec<f64> = QS.iter().map(|&q| quantile(&mut v, q)).collect();
        assert_eq!(got, vec![1.6, 4.0, 7.800000000000001, 8.879999999999999]);
        assert!(quantile(&mut [], 0.5).is_nan());
        assert_eq!(quantile(&mut [7.0], 0.9), 7.0);
        assert!(quantile(&mut [7.0, f64::NAN], 0.0).is_nan());
        assert!(quantile(&mut [7.0, 8.0], f64::NAN).is_nan());
    }

    /// Selection gives what a full sort gives, at every size and quantile.
    #[test]
    fn quantile_by_selection_is_quantile_by_sort() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        for n in [1usize, 2, 3, 10, 101, 1000, 4001] {
            let v: Vec<f64> = (0..n)
                .map(|_| {
                    x = crate::hash::splitmix64(x);
                    (x % 1000) as f64 / 7.0
                })
                .collect();
            let mut s = v.clone();
            s.sort_by(|a, b| a.total_cmp(b));
            for q in [0.0, 0.01, 0.1, 0.25, 0.5, 0.75, 0.9, 0.99, 1.0] {
                let h = (n - 1) as f64 * q;
                let lo = h.floor() as usize;
                let want = if lo + 1 >= n { s[n - 1] } else { lerp(s[lo], s[lo + 1], h - lo as f64) };
                assert_eq!(quantile(&mut v.clone(), q), want, "n {} q {}", n, q);
            }
        }
    }

    #[test]
    fn ranges_parse_and_refuse() {
        assert_eq!(Range::parse("0:31187785"), Ok(Range { first: 0, end: 31_187_785 }));
        assert_eq!(Range::parse("700000000:"), Ok(Range { first: 700_000_000, end: u64::MAX }));
        for bad in ["5", "5:5", "9:3", "a:4", "1:b", ":4"] {
            assert!(Range::parse(bad).is_err(), "{}", bad);
        }
    }

    #[test]
    fn kept_index_is_the_first_sampled_record() {
        assert_eq!(kept_index(0, 4001), 0);
        assert_eq!(kept_index(1, 4001), 1);
        assert_eq!(kept_index(4001, 4001), 1);
        assert_eq!(kept_index(4002, 4001), 2);
        assert_eq!(kept_index(10, 1), 10);
    }

    /// The pooled and per-bucket forms are the `mawk` pass's: on three
    /// records of differences 0.01, 0.03 and -0.02 the mean is 0.02/3.
    #[test]
    fn dispersion_stats_are_the_mawk_forms() {
        let xs = [0.01f64, 0.03, -0.02];
        let (sx, sxx) = (xs.iter().sum::<f64>(), xs.iter().map(|x| x * x).sum::<f64>());
        let (sd, rms, t) = disp_stats(3.0, sx, sxx, 3.0 * 1e-4);
        let m = sx / 3.0;
        let var = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / 3.0;
        assert!((sd - (var / 2.0).sqrt()).abs() < 1e-15);
        assert!((rms - (sxx / 6.0).sqrt()).abs() < 1e-15);
        assert!((t - 0.01).abs() < 1e-15);
    }
}
