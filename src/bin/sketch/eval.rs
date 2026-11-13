//! # `eval`: every sketch beside the exact colour, on a prefix
//!
//! The only place a sketch can be compared with the whole colour it stands
//! for, rather than with summaries of it: the exact store
//! ([`crate::exact::Exact`], bit-identical to `--weighted`) runs on its own
//! thread beside one thread per config, all fed by one parse, and every
//! `--every`-th record each config's estimate is measured against the exact
//! colour by every metric in [`crate::metrics`].  The price is the exact
//! store's memory -- about 43 GB at 1.5M records -- which is why the prefix is
//! capped at 3M records without `--force-exact`.
//!
//! ## The IDEAL columns
//!
//! Next to each error is the same variant's reduction applied *once* to the
//! exact colour (`varopt` and `prio` at the draw reserved for it,
//! [`crate::hash::IDEAL_STEP`]; `coord` with the coordinated keys).  Its error
//! is what a `k`-point sketch costs even with no recursion at all -- KS, for
//! one, has a floor of about `tau/2` for any `k`-point sketch of a diffuse
//! colour -- so the paired difference `metric - ideal` estimates the *excess
//! expected error* of building the sketch up through the DAG.  For `varopt`
//! and `prio` the IDEAL draw is independent of the recursion's, so the spread
//! of that difference holds both sampling errors; its mean and paired SE are
//! what to read.
//!
//! ## Threads, and why nothing deadlocks
//!
//! Each metric thread has exactly one channel, unbounded, and does nothing but
//! drain it into a map of pending records: a record's row for a config is
//! computed as soon as both its exact colour and that config's estimate have
//! arrived, in either order.  No producer ever waits on a metric thread, so the
//! only back-pressure in the pipeline is the record broadcast's, and that
//! waits only on the slowest store.  Sketch stores run ahead of the exact one,
//! so what is pending is a few batches' worth of small sketch payloads.
//!
//! ## Outputs
//!
//! `rows.tsv` (one row per sampled record and config), `summary.tsv` (per
//! config and support stratum: count, mean, p50/p90/p99 and two SEs of each
//! error, the paired excess over IDEAL, the fraction of exact rows and the
//! worst coefficient error among them), `seeds.tsv` (when configs differ only
//! by `:seed=`), and `cost.tsv`.
//!
//! ## Which SE to believe
//!
//! None of the per-config ones, for a bias question.  Sampled rows share
//! sketches -- a single-input transaction's colour *is* its parent's `Rc` --
//! and every descendant inherits its ancestors' sampling error, so the rows
//! are strongly correlated.  `naive_se` (rows as independent draws) came out
//! 3x to 35x below the true spread of a mean on the first 600k records, enough
//! to show "biases" at z = 24 that 16 seeds showed were not there.
//! `block_se` (batch means over blocks of [`metrics::SE_BLOCK`] records) is
//! closer -- about half to three quarters of the truth there -- and is still a
//! lower bound.  The honest SE is across independent seeds: give the same
//! config at several `:seed=`s (one pass runs them all) and read `seeds.tsv`,
//! whose `seed_se` is the SE of the mean over seeds and whose `z` is the bias
//! test.  The strata come in two
//! families: relative to `k` (`<= k`, `(k, 4k]`, `(4k, 32768]`, `> 32768`),
//! which say where in its life a colour is, and fixed (`<= 256`, `(256, 4096]`,
//! `(4096, 32768]`, `> 32768`), which are the same population at every `k` and
//! so are the ones to fit an error-against-`k` slope over.

use crate::algebra::{Algebra, Config, Info, Sketcher, Variant};
use crate::coord;
use crate::driver::Driver;
use crate::exact::Exact;
use crate::feed::{self, Batch, RefBatch, RefKind, RefVal};
use crate::hash::{self, Keys};
use crate::metrics::{self, mean_se, quantile_sorted, L};
use crate::oracle::Oracle;
use crate::prio;
use crate::varopt;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::sync::mpsc::{channel, sync_channel, Receiver, Sender};
use std::sync::Arc;

pub struct EvalOpts {
    pub configs: Vec<Config>,
    pub out: String,
    pub limit: u64,
    pub every: u64,
    pub metric_threads: usize,
    pub check_ref: Option<String>,
    pub stats: bool,
}

/// One config's estimate of one sampled record, on its way to a metric thread.
pub struct SketchPayload {
    pub cfg: usize,
    pub p: Vec<(u32, f64)>,
    pub raw_mass: f64,
    pub info: Info,
}

enum Msg {
    Exact { rec: u64, tx: usize, p: Arc<Vec<(u32, f64)>> },
    Sketch { rec: u64, payload: SketchPayload },
}

/// One row of `rows.tsv`.
#[derive(Clone, Debug)]
pub struct Row {
    pub rec: u64,
    pub tx: usize,
    pub cfg: usize,
    pub n_exact: usize,
    pub n_sketch: usize,
    pub reduced: bool,
    pub tau: f64,
    pub mass_raw: f64,
    pub ks: f64,
    pub w1: f64,
    pub tv: f64,
    pub dmean: f64,
    pub dsd: f64,
    pub dlap: [f64; 7],
    pub dlap_max: f64,
    pub top32: f64,
    pub max_abs: f64,
    pub ideal_ks: f64,
    pub ideal_w1: f64,
    pub ideal_tv: f64,
    pub ideal_dmean: f64,
    pub ideal_dlap_max: f64,
}

/// The same variant's reduction applied once to the exact colour `p`.
pub fn ideal(cfg: &Config, keys: Option<&Keys>, rec: u64, p: &[(u32, f64)]) -> Vec<(u32, f64)> {
    let k = cfg.k;
    let x = hash::coordinate(rec, hash::IDEAL_STEP);
    let mut out = Vec::new();
    match cfg.variant {
        Variant::VarOpt => {
            if p.len() <= k {
                return p.to_vec();
            }
            let u = hash::varopt_offset(cfg.seed, x);
            varopt::reduce(p, k, u, &mut varopt::Scratch::default(), &mut out);
        }
        Variant::Prio => {
            if p.len() <= k {
                return p.to_vec();
            }
            prio::reduce(p, k, cfg.seed, x, &mut prio::Scratch::default(), &mut out);
        }
        Variant::Coord => {
            let keys = keys.expect("coord has keys");
            let tau = coord::reduce(p, k, f64::INFINITY, 0, keys, &mut coord::Scratch::default(), &mut out);
            if tau.is_finite() {
                for x in out.iter_mut() {
                    x.1 = x.1.max(1.0 / tau);
                }
            }
        }
    }
    let total: f64 = out.iter().map(|x| x.1).sum();
    for x in out.iter_mut() {
        x.1 /= total;
    }
    out
}

fn lap_err(a: &[f64; 7], b: &[f64; 7]) -> ([f64; 7], f64) {
    let mut d = [0.0; 7];
    let mut worst = 0.0f64;
    for s in 0..7 {
        d[s] = a[s] - b[s];
        worst = worst.max(d[s].abs());
    }
    (d, worst)
}

/// Every column of a row, from the exact colour and one config's estimate.
pub fn row(cfg: &Config, cfg_id: usize, keys: Option<&Keys>, rec: u64, tx: usize, exact: &[(u32, f64)], sk: &SketchPayload) -> Row {
    let (ks, w1, tv) = metrics::distances(exact, &sk.p);
    let (m_e, s_e) = metrics::moments(exact);
    let (m_s, s_s) = metrics::moments(&sk.p);
    let lap_e = metrics::laplace(exact);
    let (dlap, dlap_max) = lap_err(&metrics::laplace(&sk.p), &lap_e);
    let id = ideal(cfg, keys, rec, exact);
    let (iks, iw1, itv) = metrics::distances(exact, &id);
    let (m_i, _) = metrics::moments(&id);
    let (_, idl) = lap_err(&metrics::laplace(&id), &lap_e);
    Row {
        rec,
        tx,
        cfg: cfg_id,
        n_exact: exact.len(),
        n_sketch: sk.info.n,
        reduced: sk.info.reduced,
        tau: sk.info.tau,
        mass_raw: sk.raw_mass - 1.0,
        ks,
        w1,
        tv,
        dmean: (m_s - m_e) / L,
        dsd: (s_s - s_e) / L,
        dlap,
        dlap_max,
        top32: metrics::top32_l1(exact, &sk.p),
        max_abs: metrics::max_abs(exact, &sk.p),
        ideal_ks: iks,
        ideal_w1: iw1,
        ideal_tv: itv,
        ideal_dmean: (m_i - m_e) / L,
        ideal_dlap_max: idl,
    }
}

struct Pending {
    tx: usize,
    exact: Option<Arc<Vec<(u32, f64)>>>,
    waiting: Vec<SketchPayload>,
    done: usize,
}

fn metric_thread(rx: Receiver<Msg>, configs: Arc<Vec<Config>>, keys: Arc<HashMap<u64, Arc<Keys>>>) -> Vec<Row> {
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut rows = Vec::new();
    let n = configs.len();
    let key_of = |c: &Config| keys.get(&c.seed).map(|k| &**k);
    while let Ok(msg) = rx.recv() {
        let (rec, ready) = match msg {
            Msg::Exact { rec, tx, p } => {
                let e = pending.entry(rec).or_insert(Pending { tx, exact: None, waiting: Vec::new(), done: 0 });
                e.tx = tx;
                e.exact = Some(p);
                (rec, std::mem::take(&mut e.waiting))
            }
            Msg::Sketch { rec, payload } => {
                let e = pending.entry(rec).or_insert(Pending { tx: 0, exact: None, waiting: Vec::new(), done: 0 });
                if e.exact.is_none() {
                    e.waiting.push(payload);
                    continue;
                }
                (rec, vec![payload])
            }
        };
        let e = pending.get_mut(&rec).expect("just inserted");
        let exact = e.exact.clone().expect("present");
        for sk in ready {
            let c = &configs[sk.cfg];
            rows.push(row(c, sk.cfg, key_of(c), rec, e.tx, &exact, &sk));
            e.done += 1;
        }
        if e.done == n {
            pending.remove(&rec);
        }
    }
    rows
}

/// What a store thread reports at the end.
pub struct Cost {
    pub label: String,
    pub busy: f64,
    pub records: u64,
    pub peak_entries: u64,
    pub peak_sketches: u64,
    pub reductions: u64,
    pub off_by_one: u64,
}

fn sketch_thread(
    cfg: Config,
    cfg_id: usize,
    keys: Option<Arc<Keys>>,
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    metric: Vec<Sender<Msg>>,
    every: u64,
    stats: bool,
) -> io::Result<Cost> {
    let label = cfg.label();
    let mut d = Driver::new(Sketcher::new(cfg, keys), oracle, stats.then(|| label.clone()));
    let t = metric.len() as u64;
    while let Ok(batch) = rx.recv() {
        d.batch(&batch, |rec, _tx, c, alg| {
            if rec % every == 0 {
                let mut p = Vec::new();
                alg.emit(c, &mut p);
                let payload = SketchPayload { cfg: cfg_id, p, raw_mass: alg.raw_mass(c), info: alg.info(c) };
                let _ = metric[((rec / every) % t) as usize].send(Msg::Sketch { rec, payload });
            }
            Ok(())
        })?;
    }
    let w = d.alg.work();
    let u = d.alg.usage();
    let (busy, records) = (d.busy, d.records);
    d.finish();
    Ok(Cost {
        label,
        busy,
        records,
        peak_entries: u.peak_entries,
        peak_sketches: u.peak_sketches,
        reductions: w.reductions,
        off_by_one: w.off_by_one,
    })
}

fn exact_thread(
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    refs: Option<Receiver<Arc<RefBatch>>>,
    metric: Vec<Sender<Msg>>,
    every: u64,
    stats: bool,
) -> io::Result<Cost> {
    let mut d = Driver::new(Exact::new(), oracle, stats.then(|| "exact".to_string()));
    let t = metric.len() as u64;
    let mut worst = 0.0f64;
    let refs_rx: Vec<Receiver<Arc<RefBatch>>> = refs.into_iter().collect();
    while let Ok(batch) = rx.recv() {
        let r = feed::next_refs(&refs_rx, &batch)?;
        let first = batch.first;
        d.batch(&batch, |rec, tx, c, alg| {
            if rec % every == 0 {
                let mut p = Vec::new();
                alg.emit(c, &mut p);
                if let Some(rb) = r.first() {
                    if let Some(RefVal::Laplace(want)) = &rb.vals[(rec - first) as usize] {
                        let (_, e) = lap_err(&metrics::laplace(&p), want);
                        worst = worst.max(e);
                        if e > 1e-9 {
                            return Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("record {} (tx {}): the exact colour's Laplace values differ from the reference by {:e}", rec, tx, e),
                            ));
                        }
                    }
                }
                let _ = metric[((rec / every) % t) as usize].send(Msg::Exact { rec, tx, p: Arc::new(p) });
            }
            Ok(())
        })?;
    }
    if !refs_rx.is_empty() {
        eprintln!("sketch eval: the exact colours agree with the Laplace reference to {:.3e} on every sampled record", worst);
    }
    let u = d.alg.usage();
    let (busy, records) = (d.busy, d.records);
    d.finish();
    Ok(Cost { label: "exact".into(), busy, records, peak_entries: u.peak_entries, peak_sketches: 0, reductions: 0, off_by_one: 0 })
}

pub fn run(opts: EvalOpts, input: Box<dyn io::Read + Send>, oracle: Option<Arc<Oracle>>, keys: HashMap<u64, Arc<Keys>>) -> io::Result<()> {
    std::fs::create_dir_all(&opts.out)?;
    let configs = Arc::new(opts.configs.clone());
    let keys = Arc::new(keys);
    let t = opts.metric_threads.max(1);
    let every = opts.every.max(1);

    let mut metric_tx = Vec::new();
    let mut metric_handles = Vec::new();
    for i in 0..t {
        let (tx, rx) = channel::<Msg>();
        metric_tx.push(tx);
        let (c, k) = (configs.clone(), keys.clone());
        metric_handles.push(std::thread::Builder::new().name(format!("metric-{}", i)).spawn(move || metric_thread(rx, c, k))?);
    }

    let mut senders = Vec::new();
    let mut stores = Vec::new();
    for (i, cfg) in opts.configs.iter().enumerate() {
        let (tx, rx) = sync_channel::<Arc<Batch>>(8);
        senders.push(tx);
        let (cfg, k, o, m) = (*cfg, keys.get(&cfg.seed).cloned(), oracle.clone(), metric_tx.clone());
        let k = if cfg.variant == Variant::Coord { k } else { None };
        let stats = opts.stats;
        stores.push(
            std::thread::Builder::new()
                .name(cfg.label())
                .spawn(move || sketch_thread(cfg, i, k, o, rx, m, every, stats))?,
        );
    }
    let (tx, rx) = sync_channel::<Arc<Batch>>(8);
    senders.push(tx);
    let mut ref_handle = None;
    let ref_rx = match &opts.check_ref {
        Some(path) => {
            let (rtx, rrx) = sync_channel(8);
            ref_handle = Some(feed::spawn_ref(path.clone(), RefKind::Laplace, every, opts.limit, vec![rtx]));
            Some(rrx)
        }
        None => None,
    };
    let (o, m, stats) = (oracle.clone(), metric_tx.clone(), opts.stats);
    stores.push(std::thread::Builder::new().name("exact".into()).spawn(move || exact_thread(o, rx, ref_rx, m, every, stats))?);
    drop(metric_tx);

    let reader = feed::spawn_reader(input, opts.limit, senders);
    let mut costs = Vec::new();
    let mut first_err: Option<io::Error> = None;
    for h in stores {
        match h.join().expect("a store thread panicked") {
            Ok(c) => costs.push(c),
            Err(e) => {
                first_err.get_or_insert(e);
            }
        }
    }
    let read = reader.join().expect("the reader panicked");
    // A reference stopped early by its consumer is not an error; one that
    // failed is the cause of its consumer's "a reference ended".
    let ref_results = ref_handle.into_iter().map(|h| h.join().expect("a reference reader panicked")).collect();
    let mut rows: Vec<Row> = Vec::new();
    for h in metric_handles {
        rows.extend(h.join().expect("a metric thread panicked"));
    }
    if let Some(e) = feed::root_cause(first_err, ref_results) {
        return Err(e);
    }
    let records = read?;
    rows.sort_by_key(|r| (r.rec, r.cfg));

    write_rows(&format!("{}/rows.tsv", opts.out), &rows, &configs)?;
    let worst = write_summary(&format!("{}/summary.tsv", opts.out), &format!("{}/seeds.tsv", opts.out), &rows, &configs)?;
    write_cost(&format!("{}/cost.tsv", opts.out), &costs)?;
    eprintln!(
        "sketch eval: {} records, {} rows over {} configs written to {}",
        records,
        rows.len(),
        configs.len(),
        opts.out
    );
    for (c, w) in configs.iter().zip(&worst) {
        if *w > 1e-6 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: a row not marked reduced is off the exact colour by {:e} in one coefficient", c.label(), w),
            ));
        }
    }
    Ok(())
}

fn write_rows(path: &str, rows: &[Row], configs: &[Config]) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    write!(w, "rec\ttx_id\tconfig\tn_exact\tn_sketch\treduced\ttau\tmass_raw\tks\tw1_L\ttv1024\tdmean_L\tdsd_L")?;
    for s in metrics::LAPLACE_NAMES {
        write!(w, "\tdlap_{}", s)?;
    }
    writeln!(w, "\tdlap_max\ttop32_l1\tmax_abs\tideal_ks\tideal_w1_L\tideal_tv1024\tideal_dmean_L\tideal_dlap_max")?;
    let labels: Vec<String> = configs.iter().map(|c| c.label()).collect();
    let f = metrics::fmt;
    for r in rows {
        write!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.rec,
            r.tx,
            labels[r.cfg],
            r.n_exact,
            r.n_sketch,
            r.reduced as u8,
            f(r.tau),
            f(r.mass_raw),
            f(r.ks),
            f(r.w1),
            f(r.tv),
            f(r.dmean),
            f(r.dsd)
        )?;
        for d in r.dlap {
            write!(w, "\t{}", f(d))?;
        }
        writeln!(
            w,
            "\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            f(r.dlap_max),
            f(r.top32),
            f(r.max_abs),
            f(r.ideal_ks),
            f(r.ideal_w1),
            f(r.ideal_tv),
            f(r.ideal_dmean),
            f(r.ideal_dlap_max)
        )?;
    }
    w.flush()
}

/// The strata a row belongs to.
fn strata(k: usize, n: usize) -> [&'static str; 3] {
    let rel = if n <= k {
        "rel:<=k"
    } else if n <= 4 * k {
        "rel:(k,4k]"
    } else if n <= 32768 {
        "rel:(4k,32768]"
    } else {
        "rel:>32768"
    };
    let fix = if n <= 256 {
        "fix:<=256"
    } else if n <= 4096 {
        "fix:(256,4096]"
    } else if n <= 32768 {
        "fix:(4096,32768]"
    } else {
        "fix:>32768"
    };
    ["all", rel, fix]
}

const STRATA: [&str; 9] = [
    "all",
    "rel:<=k",
    "rel:(k,4k]",
    "rel:(4k,32768]",
    "rel:>32768",
    "fix:<=256",
    "fix:(256,4096]",
    "fix:(4096,32768]",
    "fix:>32768",
];

/// One summary line's values: a name, whether it is signed (a bias
/// candidate, so `seeds.tsv` gives it a z), and the rows' values in record
/// order.
type Column = (String, bool, Vec<f64>);

/// `(family, stratum, metric)` and `(signed, rows, each replicate's mean)`.
type FamKey = (String, &'static str, String);
type FamVal = (bool, usize, Vec<f64>);

/// A metric's name and how to read it off a row.
type Getter = (&'static str, fn(&Row) -> f64);

/// The values `summary.tsv` summarises, for the rows `sel` (in record order).
fn columns(sel: &[&Row]) -> Vec<Column> {
    let abs: [Getter; 10] = [
        ("ks", |r| r.ks),
        ("w1_L", |r| r.w1),
        ("tv1024", |r| r.tv),
        ("abs_dmean_L", |r| r.dmean.abs()),
        ("dlap_max", |r| r.dlap_max),
        ("top32_l1", |r| r.top32),
        ("ideal_ks", |r| r.ideal_ks),
        ("ideal_w1_L", |r| r.ideal_w1),
        ("ideal_tv1024", |r| r.ideal_tv),
        ("ideal_dlap_max", |r| r.ideal_dlap_max),
    ];
    let mut out: Vec<Column> = abs.iter().map(|(n, get)| (n.to_string(), false, sel.iter().map(|r| get(r)).collect())).collect();
    let signed: [Getter; 6] = [
        ("dmean_L", |r| r.dmean),
        ("mass_raw", |r| r.mass_raw),
        ("ks-ideal_ks", |r| r.ks - r.ideal_ks),
        ("w1_L-ideal_w1_L", |r| r.w1 - r.ideal_w1),
        ("tv1024-ideal_tv1024", |r| r.tv - r.ideal_tv),
        ("dlap_max-ideal_dlap_max", |r| r.dlap_max - r.ideal_dlap_max),
    ];
    out.extend(signed.iter().map(|(n, get)| (n.to_string(), true, sel.iter().map(|r| get(r)).collect())));
    for (s, name) in metrics::LAPLACE_NAMES.iter().enumerate() {
        out.push((format!("dlap_{}", name), true, sel.iter().map(|r| r.dlap[s]).collect()));
    }
    out
}

/// Writes `summary.tsv`, and `seeds.tsv` when some configs are seed
/// replicates of each other; answers each config's worst coefficient error
/// over its unreduced rows.
///
/// `naive_se` treats every row as an independent draw, which they are not
/// (see [`metrics::BlockMeans`]): it understates the uncertainty of a mean by
/// 3x to 35x and must not be read as significance.  `block_se` is the
/// batch-means SE over blocks of [`metrics::SE_BLOCK`] records (`blocks` of
/// them) -- closer, still a lower bound.  The SE to judge a bias by is the
/// between-seed one of `seeds.tsv`: run the same config at several
/// `:seed=`s in one pass.
fn write_summary(path: &str, seeds_path: &str, rows: &[Row], configs: &[Config]) -> io::Result<Vec<f64>> {
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "config\tstratum\tmetric\tcount\tmean\tnaive_se\tblock_se\tblocks\tp50\tp90\tp99")?;
    let mut worst = vec![0.0f64; configs.len()];
    let f = metrics::fmt;
    // (family, stratum, metric) -> (signed, rows, each replicate's mean)
    let mut fam: Vec<(FamKey, FamVal)> = Vec::new();
    let mut fam_at: HashMap<FamKey, usize> = HashMap::new();
    for (ci, cfg) in configs.iter().enumerate() {
        let label = cfg.label();
        let family = metrics::family(&label);
        for stratum in STRATA {
            let sel: Vec<&Row> =
                rows.iter().filter(|r| r.cfg == ci && strata(cfg.k, r.n_exact).contains(&stratum)).collect();
            if sel.is_empty() {
                continue;
            }
            for (name, signed, mut v) in columns(&sel) {
                let (m, se) = mean_se(&v);
                let mut bm = metrics::BlockMeans::default();
                for (r, x) in sel.iter().zip(&v) {
                    bm.add(r.rec, *x);
                }
                let (bse, blocks) = bm.se();
                let (p50, p90, p99) = if signed {
                    (f64::NAN, f64::NAN, f64::NAN)
                } else {
                    v.sort_by(|a, b| a.total_cmp(b));
                    (quantile_sorted(&v, 0.5), quantile_sorted(&v, 0.9), quantile_sorted(&v, 0.99))
                };
                writeln!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    label,
                    stratum,
                    name,
                    v.len(),
                    f(m),
                    f(se),
                    f(bse),
                    blocks,
                    f(p50),
                    f(p90),
                    f(p99)
                )?;
                let key = (family.clone(), stratum, name);
                let i = *fam_at.entry(key.clone()).or_insert_with(|| {
                    fam.push((key, (signed, 0, Vec::new())));
                    fam.len() - 1
                });
                fam[i].1 .1 += v.len();
                fam[i].1 .2.push(m);
            }
            let exact: Vec<&&Row> = sel.iter().filter(|r| !r.reduced).collect();
            let frac = exact.len() as f64 / sel.len() as f64;
            let max_abs = exact.iter().map(|r| r.max_abs).fold(0.0f64, f64::max);
            if stratum == "all" {
                worst[ci] = max_abs;
            }
            writeln!(w, "{}\t{}\tfrac_exact\t{}\t{}\tNA\tNA\tNA\tNA\tNA\tNA", label, stratum, sel.len(), f(frac))?;
            writeln!(w, "{}\t{}\texact_max_abs\t{}\t{}\tNA\tNA\tNA\tNA\tNA\tNA", label, stratum, exact.len(), f(max_abs))?;
        }
    }
    w.flush()?;
    write_seeds(seeds_path, &fam)?;
    Ok(worst)
}

/// `seeds.tsv`: for every family of seed replicates (configs equal but for
/// their seed, at least two of them), the mean over seeds of each config's
/// mean and the SE and SD of those means across seeds.  The seeds are
/// independent, so this SE is honest where the per-row ones are not; `z`
/// (mean over SE) is given for the signed metrics, where it is the bias test.
/// Not written when no family has two seeds.
fn write_seeds(path: &str, fam: &[(FamKey, FamVal)]) -> io::Result<()> {
    if !fam.iter().any(|(_, (_, _, m))| m.len() >= 2) {
        // Not a stale one from an earlier run into the same directory.
        let _ = std::fs::remove_file(path);
        return Ok(());
    }
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "family\tstratum\tmetric\tseeds\trows\tmean\tseed_se\tseed_sd\tz")?;
    let f = metrics::fmt;
    for ((family, stratum, name), (signed, rows, means)) in fam {
        if means.len() < 2 {
            continue;
        }
        let (m, se, sd) = metrics::across(means);
        let z = if *signed { m / se } else { f64::NAN };
        writeln!(w, "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}", family, stratum, name, means.len(), rows, f(m), f(se), f(sd), f(z))?;
    }
    w.flush()
}

pub fn write_cost(path: &str, costs: &[Cost]) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "config\tbusy_s\trecords\tpeak_entries\tpeak_sketches\treductions\toff_by_one\tvm_hwm")?;
    let hwm = metrics::vm_hwm();
    for c in costs {
        writeln!(
            w,
            "{}\t{:.3}\t{}\t{}\t{}\t{}\t{}\t{}",
            c.label, c.busy, c.records, c.peak_entries, c.peak_sketches, c.reductions, c.off_by_one, hwm
        )?;
    }
    w.flush()
}
