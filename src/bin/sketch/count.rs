//! # `count`: every record's support size, by KMV, against the exact sizes
//!
//! A reader thread broadcasts the records to one store thread per `--kmv`
//! ([`crate::kmv::KmvStore`]); with `--sets-counts` a second reader cuts the
//! exact `--sets` sizes into batches on the same boundaries
//! ([`crate::feed::spawn_counts`]).  Each store thread writes its count
//! stream (`counts.<label>.bin`, [`crate::kmv::CNT_MAGIC`]) and compares as
//! it goes.  Nothing of the colour is folded, so a run costs the parse and a
//! capped merge per record -- about a sixth of a varopt store at the same `k`.
//!
//! ## Two counts per record
//!
//! The primary count is over the universe of blocks the support can lie in,
//! `[0, beta]` with `beta` the newest block seen ([`crate::kmv::Universe`]);
//! the secondary is the textbook one over the whole permutation, `M =
//! 2^bits`.  Both are exactly unbiased under the permutation; the first has a
//! finite-population factor `(beta+1-n)/beta` where the second has
//! `(M-n)/(M-1)`, and on this chain's dense supports that halves the RSE.
//! Every table gives both, as `estimator = univ | M`.
//!
//! ## What is checked, and how
//!
//! - **Hard counters**, which must all be 0 or the run fails: a whole sketch
//!   whose length is not the exact size (`exact_region_bad`), a full sketch
//!   of a support of at most `k` (`full_but_small`), a whole sketch of a
//!   larger one (`whole_but_large`).  For `support=pos` the reference is the
//!   `sets` size, an upper bound: `pos_exceeds_sets` and `full_but_small`
//!   are its counters.
//! - **Per-run strata** (`count-strata.tsv`): mean and RMS relative error by
//!   size stratum against the theory RSE.  These are descriptive only.  All
//!   the records of one run share one permutation, so their errors are
//!   correlated -- within a 10M bucket the error is mostly one shared offset
//!   -- and a stratum's RMS is about one draw of the theory, not an average
//!   of many.
//! - **Across seeds** (`count-seeds.tsv`), the only gates: seeds are the unit.
//!   For specs that differ only by `seed=`, each seed's stratum mean is one
//!   draw, and the bias test is Student's `t` over the seeds with the
//!   Bonferroni threshold for the strata tested; the variance test averages,
//!   over records, each record's variance *across seeds* and compares it with
//!   the theory, inside the chi-square band that would hold even if every
//!   record of a stratum moved together ([`SeedStrata`]).

use crate::algebra::Algebra;
use crate::driver::Driver;
use crate::eval::{write_cost, Cost};
use crate::feed::{self, Batch, CountBatch};
use crate::hash::Perm;
use crate::kmv::{self, count_se, CntEncoder, CntRec, KmvSpec, KmvStore, Support, Universe, N_STRATA};
use crate::metrics::{self, chi2_quantile, t_quantile, BlockMeans};
use crate::oracle::Oracle;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender};
use std::sync::Arc;

pub struct CountOpts {
    pub kmv: Vec<KmvSpec>,
    pub out: String,
    pub limit: u64,
    pub sets_counts: Option<String>,
    pub every: Option<u64>,
    pub stream: bool,
    pub zstd: bool,
    pub bucket: u64,
    pub stats: bool,
}

/// The two estimators every table reports.
pub const ESTIMATORS: [&str; 2] = ["univ", "M"];

/// The family-wise level of the seed gates.
pub const ALPHA: f64 = 0.01;

// ------------------------------------------------------------------ seeds

#[derive(Clone, Default)]
struct SeedStratum {
    records: u64,
    sum_var: f64,
    sum_theory2: f64,
    seed_sum: Vec<f64>,
    seed_n: u64,
}

/// Seed-replicate statistics by stratum, for one family and one estimator.
///
/// Seeds are the unit.  [`SeedStrata::add`] takes one record's relative
/// error under every seed and its theory RSE; per stratum it keeps each
/// seed's sum (so a seed's stratum mean is one draw) and the record's
/// variance across seeds (unbiased for its true variance whatever the
/// correlation between records).
pub struct SeedStrata {
    pub seeds: usize,
    st: Vec<SeedStratum>,
}

/// One line of a seeds table.
#[derive(Clone, Debug)]
pub struct SeedLine {
    pub stratum: &'static str,
    pub records: u64,
    pub bias: f64,
    pub seed_sd: f64,
    pub t: f64,
    pub t_crit: f64,
    pub bias_ok: Option<bool>,
    pub var_ratio: f64,
    pub var_lo: f64,
    pub var_hi: f64,
    pub var_ok: Option<bool>,
}

impl SeedStrata {
    pub fn new(seeds: usize) -> SeedStrata {
        SeedStrata { seeds, st: vec![SeedStratum { seed_sum: vec![0.0; seeds], ..Default::default() }; N_STRATA] }
    }

    /// One record of true size `n`: `es[s]` its relative error under seed
    /// `s`, `theory` its relative SE.
    pub fn add(&mut self, n: u64, k: usize, es: &[f64], theory: f64) {
        debug_assert_eq!(es.len(), self.seeds);
        if es.iter().any(|e| e.is_nan()) {
            return;
        }
        let s = self.seeds as f64;
        let mean = es.iter().sum::<f64>() / s;
        let var = es.iter().map(|e| (e - mean).powi(2)).sum::<f64>() / (s - 1.0);
        let mut idx = [0usize; 3];
        let n_idx = kmv::strata_of(n, k, &mut idx);
        for &i in &idx[..n_idx] {
            let x = &mut self.st[i];
            x.records += 1;
            x.sum_var += var;
            x.sum_theory2 += theory * theory;
            x.seed_n += 1;
            for (a, e) in x.seed_sum.iter_mut().zip(es) {
                *a += e;
            }
        }
    }

    /// The tests, per stratum with a nonzero theory (a stratum counted
    /// exactly has nothing to test).  `alpha` is family-wise over those.  A
    /// stratum whose theory is infinite (`k = 2`) gets its bias tested and
    /// its variance gate `NA`.
    pub fn lines(&self, alpha: f64) -> Vec<SeedLine> {
        let testable: Vec<usize> = (0..N_STRATA).filter(|&i| self.st[i].records > 0 && self.st[i].sum_theory2 > 0.0).collect();
        let tests = testable.len().max(1) as f64;
        let dof = self.seeds as f64 - 1.0;
        let t_crit = t_quantile(1.0 - alpha / (2.0 * tests), dof);
        let var_lo = chi2_quantile(alpha / (2.0 * tests), dof) / dof;
        let var_hi = chi2_quantile(1.0 - alpha / (2.0 * tests), dof) / dof;
        let mut out = Vec::new();
        for i in 0..N_STRATA {
            let x = &self.st[i];
            if x.records == 0 {
                continue;
            }
            let means: Vec<f64> = x.seed_sum.iter().map(|s| s / x.seed_n as f64).collect();
            let (bias, se, sd) = metrics::across(&means);
            let ok = testable.contains(&i);
            let t = if se > 0.0 { bias / se } else { f64::NAN };
            // At k = 2 the theory formula diverges (kmv::count_se): the bias
            // is still tested, the variance is not.
            let var_testable = ok && x.sum_theory2.is_finite();
            let var_ratio = if x.sum_theory2 > 0.0 && x.sum_theory2.is_finite() { x.sum_var / x.sum_theory2 } else { f64::NAN };
            out.push(SeedLine {
                stratum: kmv::stratum_name(i),
                records: x.records,
                bias,
                seed_sd: sd,
                t,
                t_crit,
                bias_ok: ok.then(|| t.abs() < t_crit),
                var_ratio,
                var_lo,
                var_hi,
                var_ok: var_testable.then(|| var_ratio >= var_lo && var_ratio <= var_hi),
            });
        }
        out
    }
}

pub const SEEDS_HEADER: &str =
    "family\testimator\tstratum\tseeds\trecords\tbias\tseed_sd\tseed_se\tt\tt_crit\tbias_ok\tvar_ratio\tvar_lo\tvar_hi\tvar_ok";

/// Write `lines` under `family` and `estimator`; answers how many gates failed.
pub fn write_seed_lines(w: &mut impl Write, family: &str, estimator: &str, seeds: usize, lines: &[SeedLine]) -> io::Result<usize> {
    let f = metrics::fmt;
    let flag = |x: Option<bool>| x.map_or("NA".to_string(), |b| (b as u8).to_string());
    let mut failed = 0;
    for l in lines {
        failed += (l.bias_ok == Some(false)) as usize + (l.var_ok == Some(false)) as usize;
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            family,
            estimator,
            l.stratum,
            seeds,
            l.records,
            f(l.bias),
            f(l.seed_sd),
            f(l.seed_sd / (seeds as f64).sqrt()),
            f(l.t),
            f(l.t_crit),
            flag(l.bias_ok),
            f(l.var_ratio),
            f(l.var_lo),
            f(l.var_hi),
            flag(l.var_ok)
        )?;
    }
    Ok(failed)
}

/// What a store thread sends its family's seed aggregator, per batch.
struct SeedMsg {
    first: u64,
    /// Per record: the reference size (0 = none), the universe size, and the
    /// relative error of each estimator.
    refs: Vec<u32>,
    univ: Vec<u64>,
    e: Vec<[f64; 2]>,
}

fn seed_aggregator(k: usize, m: u64, rxs: Vec<Receiver<SeedMsg>>) -> [SeedStrata; 2] {
    let s = rxs.len();
    let mut out = [SeedStrata::new(s), SeedStrata::new(s)];
    let mut es = [vec![0.0f64; s], vec![0.0f64; s]];
    'outer: loop {
        let mut msgs = Vec::with_capacity(s);
        for rx in &rxs {
            match rx.recv() {
                Ok(x) => msgs.push(x),
                Err(_) => break 'outer,
            }
        }
        let first = msgs[0].first;
        assert!(msgs.iter().all(|x| x.first == first && x.refs.len() == msgs[0].refs.len()), "seed replicates in step");
        for i in 0..msgs[0].refs.len() {
            let n = msgs[0].refs[i];
            if n == 0 {
                continue;
            }
            let u = msgs[0].univ[i];
            for (est, univ) in [(0usize, u), (1, m)] {
                for (j, msg) in msgs.iter().enumerate() {
                    es[est][j] = msg.e[i][est];
                }
                let theory = if n as usize > k { count_se(n as f64, k, univ) / n as f64 } else { 0.0 };
                out[est].add(n as u64, k, &es[est], theory);
            }
        }
    }
    out
}

// ------------------------------------------------------------------ one store

/// Where a count stream goes: a file, or a `zstd` child writing one.
enum Sink {
    File(BufWriter<File>),
    Zstd(Child, BufWriter<ChildStdin>),
}

impl Sink {
    fn open(path: &str, zstd: bool) -> io::Result<Sink> {
        if !zstd {
            return Ok(Sink::File(BufWriter::with_capacity(1 << 22, File::create(path)?)));
        }
        let out = format!("{}.zst", path);
        let mut child = Command::new("zstd")
            .args(["-q", "-f", "-T8", "-12", "-o", &out])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|e| io::Error::new(e.kind(), format!("cannot run zstd for {}: {}", out, e)))?;
        let stdin = child.stdin.take().expect("piped");
        Ok(Sink::Zstd(child, BufWriter::with_capacity(1 << 22, stdin)))
    }
    fn w(&mut self) -> &mut dyn Write {
        match self {
            Sink::File(w) => w,
            Sink::Zstd(_, w) => w,
        }
    }
    fn finish(self) -> io::Result<()> {
        match self {
            Sink::File(mut w) => w.flush(),
            Sink::Zstd(mut child, mut w) => {
                w.flush()?;
                drop(w);
                let st = child.wait()?;
                if st.success() {
                    Ok(())
                } else {
                    Err(io::Error::other(format!("zstd exited with {}", st)))
                }
            }
        }
    }
}

/// The hard counters, and how much was compared.
#[derive(Clone, Copy, Debug, Default)]
pub struct Check {
    pub records: u64,
    pub compared: u64,
    pub exact_region: u64,
    pub exact_region_bad: u64,
    pub full_but_small: u64,
    pub whole_but_large: u64,
    pub pos_exceeds_sets: u64,
    pub full: u64,
    pub max_len: u64,
}

impl Check {
    pub fn hard(&self) -> u64 {
        self.exact_region_bad + self.full_but_small + self.whole_but_large + self.pos_exceeds_sets
    }
}

#[derive(Clone, Default)]
struct Acc {
    n: u64,
    s: f64,
    s2: f64,
    th2: f64,
    bm: BlockMeans,
}

/// Quantiles per bucket of records: the estimate of every record, and the
/// reference where there is one.
#[derive(Default)]
struct BucketQ {
    id: u64,
    n_hat: Vec<f32>,
    refs: Vec<f32>,
    whole: u64,
}

fn quantiles(v: &mut [f32]) -> [f64; 4] {
    let mut out = [f64::NAN; 4];
    if v.is_empty() {
        return out;
    }
    for (o, q) in out.iter_mut().zip([0.1, 0.5, 0.9, 0.99]) {
        let i = ((q * v.len() as f64).ceil() as usize).clamp(1, v.len()) - 1;
        *o = *v.select_nth_unstable_by(i, |a, b| a.total_cmp(b)).1 as f64;
    }
    out
}

struct StoreOut {
    spec: KmvSpec,
    check: Check,
    strata: Vec<[Acc; 2]>,
    buckets: Vec<String>,
    cost: Cost,
}

#[allow(clippy::too_many_arguments)]
fn store(
    spec: KmvSpec,
    perm: Arc<Perm>,
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    mut refs: Option<Receiver<Arc<CountBatch>>>,
    seeds: Option<Sender<SeedMsg>>,
    mut sink: Option<Sink>,
    mut rows: Option<(BufWriter<File>, u64)>,
    bucket: u64,
    stats: bool,
) -> io::Result<StoreOut> {
    let label = spec.label();
    let k = spec.k;
    let m = perm.universe();
    let mut d = Driver::new(KmvStore::new(spec, perm.clone()), oracle, stats.then(|| label.clone()));
    let mut uni = Universe::new(perm.clone());
    let mut frontier: Option<u32> = None;
    let mut enc = CntEncoder::default();
    let mut check = Check::default();
    let mut strata: Vec<[Acc; 2]> = vec![Default::default(); N_STRATA];
    let mut bq = BucketQ::default();
    let mut buckets = Vec::new();
    let sets = spec.support == Support::Sets;
    if let Some((w, _)) = rows.as_mut() {
        writeln!(w, "rec\ttx\tblock\tuniv\tfull\tm\tn_hat\tse\tn_hat_M\tse_M\tref")?;
    }
    if let Some(s) = sink.as_mut() {
        s.w().write_all(&kmv::cnt_header(&spec))?;
    }
    let close_bucket = |bq: &mut BucketQ, buckets: &mut Vec<String>| {
        if bq.n_hat.is_empty() {
            return;
        }
        let n = bq.n_hat.len();
        let q = quantiles(&mut bq.n_hat);
        let r = quantiles(&mut bq.refs);
        let f = metrics::fmt;
        buckets.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            label,
            spec.support.name(),
            bq.id,
            n,
            f(bq.whole as f64 / n as f64),
            f(q[0]),
            f(q[1]),
            f(q[2]),
            f(q[3]),
            bq.refs.len(),
            f(r[0]),
            f(r[1]),
            f(r[2]),
            f(r[3])
        ));
        bq.n_hat.clear();
        bq.refs.clear();
        bq.whole = 0;
    };
    let mut idx = [0usize; 3];
    while let Ok(batch) = rx.recv() {
        let first = batch.first;
        let rb = match &refs {
            Some(r) => match r.recv() {
                Ok(b) => {
                    if b.first != first {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("sets-counts batch at {} for records at {}", b.first, first),
                        ));
                    }
                    Some(b)
                }
                Err(_) => None,
            },
            None => None,
        };
        if rb.is_none() {
            refs = None;
        }
        let mut msg = seeds.as_ref().map(|_| SeedMsg {
            first,
            refs: Vec::with_capacity(batch.len()),
            univ: Vec::with_capacity(batch.len()),
            e: Vec::with_capacity(batch.len()),
        });
        d.batch(&batch, |rec, tx, c, _alg| {
            let i = (rec - first) as usize;
            let block = batch.recs[i].block as u32;
            let fr = frontier.map_or(block, |f| f.max(block));
            frontier = Some(fr);
            uni.grow(fr as u64 + 1);
            let n_univ = uni.size();
            let (n_hat, n_hat_m) = (uni.count(c.view(), k), kmv::count_m(c.view(), k, m));
            let r = CntRec::of(c.view(), tx as u64, block);
            if let Some(s) = sink.as_mut() {
                enc.record(s.w(), &r)?;
            }
            check.records += 1;
            check.max_len = check.max_len.max(c.len() as u64);
            check.full += c.full as u64;
            let refv = rb.as_ref().and_then(|b| b.vals.get(i).copied());
            if bucket > 0 {
                let id = rec / bucket;
                if id != bq.id {
                    close_bucket(&mut bq, &mut buckets);
                    bq.id = id;
                }
                bq.n_hat.push(n_hat as f32);
                bq.whole += !c.full as u64;
                if let Some(v) = refv {
                    bq.refs.push(v as f32);
                }
            }
            let mut es = [f64::NAN; 2];
            if let Some(n) = refv {
                check.compared += 1;
                let n64 = n as u64;
                if sets {
                    if !c.full {
                        check.exact_region += 1;
                        check.exact_region_bad += (c.len() as u64 != n64) as u64;
                        check.whole_but_large += (n64 > k as u64) as u64;
                    } else {
                        check.full_but_small += (n64 <= k as u64) as u64;
                    }
                    es = [n_hat / n as f64 - 1.0, n_hat_m / n as f64 - 1.0];
                    let th = if n as usize > k {
                        [count_se(n as f64, k, n_univ) / n as f64, count_se(n as f64, k, m) / n as f64]
                    } else {
                        [0.0, 0.0]
                    };
                    let n_idx = kmv::strata_of(n64, k, &mut idx);
                    for &s in &idx[..n_idx] {
                        for e in 0..2 {
                            let a = &mut strata[s][e];
                            a.n += 1;
                            a.s += es[e];
                            a.s2 += es[e] * es[e];
                            a.th2 += th[e] * th[e];
                            a.bm.add(rec, es[e]);
                        }
                    }
                } else {
                    check.pos_exceeds_sets += (!c.full && c.len() as u64 > n64) as u64;
                    check.full_but_small += (c.full && n64 <= k as u64) as u64;
                }
            }
            if let Some(msg) = msg.as_mut() {
                msg.refs.push(refv.unwrap_or(0));
                msg.univ.push(n_univ);
                msg.e.push(es);
            }
            if let Some((w, every)) = rows.as_mut() {
                if rec % *every == 0 {
                    let (se, se_m) = if c.full { (count_se(n_hat, k, n_univ), count_se(n_hat_m, k, m)) } else { (0.0, 0.0) };
                    let f = metrics::fmt;
                    writeln!(
                        w,
                        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                        rec,
                        tx,
                        block,
                        n_univ,
                        c.full as u8,
                        c.len(),
                        f(n_hat),
                        f(se),
                        f(n_hat_m),
                        f(se_m),
                        refv.map_or("NA".to_string(), |v| v.to_string())
                    )?;
                }
            }
            Ok(())
        })?;
        if let (Some(tx), Some(msg)) = (seeds.as_ref(), msg) {
            let _ = tx.send(msg);
        }
    }
    close_bucket(&mut bq, &mut buckets);
    if let Some(mut s) = sink {
        enc.trailer(s.w())?;
        s.finish()?;
    }
    if let Some((mut w, _)) = rows {
        w.flush()?;
    }
    let w = d.alg.work();
    let u = d.alg.usage();
    let (busy, records) = (d.busy, d.records);
    if stats {
        eprintln!(
            "sketch count [{}]: {} records, busy {:.1}s, peak ranks {}, peak sketches {}, map {}, truncating merges {}",
            label,
            records,
            busy,
            u.peak_entries,
            u.peak_sketches,
            d.map_len(),
            w.reductions
        );
    }
    d.finish();
    Ok(StoreOut {
        spec,
        check,
        strata,
        buckets,
        cost: Cost {
            label,
            busy,
            records,
            peak_entries: u.peak_entries,
            peak_sketches: u.peak_sketches,
            reductions: w.reductions,
            off_by_one: 0,
        },
    })
}

/// A spec with its seed taken out: specs equal under it are seed replicates.
fn family_key(s: &KmvSpec) -> (usize, Support, u32) {
    (s.k, s.support, s.bits)
}

/// The permutation of every distinct `(seed, bits)` among `specs`.
pub fn perms_for(specs: &[KmvSpec]) -> HashMap<(u64, u32), Arc<Perm>> {
    let mut m = HashMap::new();
    for s in specs {
        m.entry((s.seed, s.bits)).or_insert_with(|| Arc::new(Perm::new(s.seed, s.bits)));
    }
    m
}

/// Refuse a store given twice: two threads would fold the same store and
/// write the same file, and two seed replicates of one seed have no spread.
pub fn unique_labels(mode: &str, option: &str, labels: impl Iterator<Item = String>) -> io::Result<()> {
    let mut seen = std::collections::HashSet::new();
    for l in labels {
        if !seen.insert(l.clone()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, format!("{}: a {} spec is given twice ({})", mode, option, l)));
        }
    }
    Ok(())
}

pub fn run(
    opts: CountOpts,
    input: Box<dyn io::Read + Send>,
    oracle: Option<Arc<Oracle>>,
    perms: &HashMap<(u64, u32), Arc<Perm>>,
) -> io::Result<()> {
    std::fs::create_dir_all(&opts.out)?;
    let n = opts.kmv.len();
    unique_labels("count", "--kmv", opts.kmv.iter().map(|s| s.label()))?;
    // Seed families: sets specs with a reference, at least two seeds.
    let mut fams: Vec<((usize, Support, u32), Vec<usize>)> = Vec::new();
    for (i, s) in opts.kmv.iter().enumerate() {
        match fams.iter_mut().find(|f| f.0 == family_key(s)) {
            Some(f) => f.1.push(i),
            None => fams.push((family_key(s), vec![i])),
        }
    }
    fams.retain(|f| f.1.len() >= 2 && f.0 .1 == Support::Sets && opts.sets_counts.is_some());
    let mut seed_tx: Vec<Option<Sender<SeedMsg>>> = (0..n).map(|_| None).collect();
    let mut aggregators = Vec::new();
    for (key, members) in &fams {
        let mut rxs = Vec::new();
        for &i in members {
            let (tx, rx) = channel();
            seed_tx[i] = Some(tx);
            rxs.push(rx);
        }
        let (k, m) = (key.0, 1u64 << key.2);
        let family = metrics::family(&opts.kmv[members[0]].label());
        aggregators.push((family, members.len(), std::thread::Builder::new().name("seeds".into()).spawn(move || seed_aggregator(k, m, rxs))?));
    }

    let mut ref_tx = Vec::new();
    let mut ref_rx: Vec<Option<Receiver<Arc<CountBatch>>>> = (0..n).map(|_| None).collect();
    if opts.sets_counts.is_some() {
        for slot in ref_rx.iter_mut() {
            let (tx, rx) = sync_channel(8);
            ref_tx.push(tx);
            *slot = Some(rx);
        }
    }
    let ref_handle = opts.sets_counts.clone().map(|p| feed::spawn_counts(p, opts.limit, ref_tx));

    let mut senders = Vec::new();
    let mut stores = Vec::new();
    for (i, spec) in opts.kmv.iter().enumerate() {
        let (tx, rx) = sync_channel::<Arc<Batch>>(8);
        senders.push(tx);
        let label = spec.label();
        let sink = if opts.stream { Some(Sink::open(&format!("{}/counts.{}.bin", opts.out, label), opts.zstd)?) } else { None };
        let rows = match opts.every {
            Some(e) => Some((BufWriter::new(File::create(format!("{}/count-rows.{}.tsv", opts.out, label))?), e.max(1))),
            None => None,
        };
        let perm = perms.get(&(spec.seed, spec.bits)).expect("a permutation per spec").clone();
        let (spec, o, refs, seeds, bucket, stats) =
            (*spec, oracle.clone(), ref_rx[i].take(), seed_tx[i].take(), opts.bucket, opts.stats);
        stores.push(
            std::thread::Builder::new()
                .name(label)
                .spawn(move || store(spec, perm, o, rx, refs, seeds, sink, rows, bucket, stats))?,
        );
    }
    let reader = feed::spawn_reader(input, opts.limit, senders);
    let mut done = Vec::new();
    let mut first_err = None;
    for h in stores {
        match h.join().expect("a store thread panicked") {
            Ok(x) => done.push(x),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    let read = reader.join().expect("the reader panicked");
    let ref_result = ref_handle.map(|h| h.join().expect("the sets-counts reader panicked"));
    let seed_tables: Vec<(String, usize, [SeedStrata; 2])> =
        aggregators.into_iter().map(|(f, s, h)| (f, s, h.join().expect("the seed aggregator panicked"))).collect();
    if let Some(e) = first_err {
        return Err(e);
    }
    if let Some(Err(e)) = ref_result {
        return Err(e);
    }
    let records = read?;

    let f = metrics::fmt;
    let mut w = BufWriter::new(File::create(format!("{}/count-check.tsv", opts.out))?);
    writeln!(w, "label\tsupport\trecords\tcompared\texact_region\texact_region_bad\tfull_but_small\twhole_but_large\tpos_exceeds_sets\tfull\tmax_len")?;
    let mut hard = 0;
    for o in &done {
        let c = &o.check;
        hard += c.hard();
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            o.spec.label(),
            o.spec.support.name(),
            c.records,
            c.compared,
            c.exact_region,
            c.exact_region_bad,
            c.full_but_small,
            c.whole_but_large,
            c.pos_exceeds_sets,
            c.full,
            c.max_len
        )?;
        if c.max_len > o.spec.k as u64 {
            return Err(io::Error::other(format!("{}: a sketch held {} ranks, more than k", o.spec.label(), c.max_len)));
        }
    }
    w.flush()?;

    let mut w = BufWriter::new(File::create(format!("{}/count-strata.tsv", opts.out))?);
    writeln!(w, "label\tsupport\testimator\tstratum\trecords\tmean_rel_err\trms_rel_err\ttheory_rse\trms_over_theory\tblock_se\tblocks")?;
    for o in done.iter().filter(|o| o.spec.support == Support::Sets) {
        for (s, accs) in o.strata.iter().enumerate() {
            for (e, a) in accs.iter().enumerate() {
                if a.n == 0 {
                    continue;
                }
                let nf = a.n as f64;
                let rms = (a.s2 / nf).sqrt();
                let th = (a.th2 / nf).sqrt();
                let (bse, blocks) = a.bm.se();
                writeln!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    o.spec.label(),
                    o.spec.support.name(),
                    ESTIMATORS[e],
                    kmv::stratum_name(s),
                    a.n,
                    f(a.s / nf),
                    f(rms),
                    f(th),
                    f(if th > 0.0 { rms / th } else { f64::NAN }),
                    f(bse),
                    blocks
                )?;
            }
        }
    }
    w.flush()?;

    let seeds_path = format!("{}/count-seeds.tsv", opts.out);
    let mut failed = 0;
    if seed_tables.is_empty() {
        let _ = std::fs::remove_file(&seeds_path);
    } else {
        let mut w = BufWriter::new(File::create(&seeds_path)?);
        writeln!(w, "{}", SEEDS_HEADER)?;
        for (family, s, tables) in &seed_tables {
            for (e, t) in tables.iter().enumerate() {
                failed += write_seed_lines(&mut w, family, ESTIMATORS[e], *s, &t.lines(ALPHA))?;
            }
        }
        w.flush()?;
    }

    let mut w = BufWriter::new(File::create(format!("{}/count-buckets.tsv", opts.out))?);
    writeln!(w, "label\tsupport\tbucket\trecords\tfrac_whole\tn_p10\tn_p50\tn_p90\tn_p99\tref_records\tref_p10\tref_p50\tref_p90\tref_p99")?;
    for o in &done {
        for line in &o.buckets {
            writeln!(w, "{}", line)?;
        }
    }
    w.flush()?;
    let costs: Vec<Cost> = done.into_iter().map(|o| o.cost).collect();
    write_cost(&format!("{}/cost.tsv", opts.out), &costs)?;
    eprintln!(
        "sketch count: {} records over {} stores, written to {}; {} seed gate(s) failed",
        records,
        costs.len(),
        opts.out,
        failed
    );
    if hard > 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} record(s) broke a hard counter; see {}/count-check.tsv", hard, opts.out),
        ));
    }
    Ok(())
}
