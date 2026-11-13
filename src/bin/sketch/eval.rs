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
//! one, has a floor of about `3*tau/4` for a `varopt` sketch of a diffuse
//! colour (see [`crate::metrics`]) -- so the paired difference `metric - ideal` estimates the *excess
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
//! ## The side stores
//!
//! With `--kmv` and `--l1` the same exact store checks the stores of
//! [`crate::kmv`] and [`crate::l1`], and `--config` becomes optional:
//!
//! - **KMV**: on every sampled record the sketch must *equal* the `k`
//!   lowest-ranked blocks of the exact support -- every term of the exact
//!   colour for `support=sets`, zero coefficients included, and for `pos`
//!   the blocks a path of positive shares reaches, which the exact store
//!   folds by set union ([`crate::exact`]; not the nonzero terms, since a
//!   coefficient underflows to 0 in f64 at depth) -- with `full` iff the
//!   support exceeds `k` (`set_mismatch`, which must be 0; T3 says it
//!   is).  Both counts are compared with
//!   the exact size (`kmv-rows.tsv`, `kmv-summary.tsv`), and seed replicates
//!   are tested in `kmv-seeds.tsv` with the seeds as the unit.
//! - **`--sets-counts`**: the exact support size of every sampled record
//!   against the file of `--sets` sizes (`support_ref_mismatch`, must be 0),
//!   which validates the reference and the support semantics at once.
//! - **L1**: one record in `--l1-every`, the store's vector against `A C`
//!   computed afresh from the exact terms and the hash (`m n` Cauchy draws,
//!   shared by the specs of one seed), as the largest deviation relative to
//!   `sum_b |A_jb| c_b` (`l1-rows.tsv`, `l1-summary.tsv`).
//!
//! Without them every file and column is what it always was.
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
use crate::feed::{self, Batch, CountBatch, RefBatch, RefKind, RefVal};
use crate::hash::{self, Keys, Perm};
use crate::kmv::{self, KmvSpec, KmvStore, Support, Universe};
use crate::l1::{L1Spec, L1Store};
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
    /// The side stores: KMV sketches of the support and L1 projections,
    /// checked against the exact store; and the sets-counts reference.
    pub kmv: Vec<KmvSpec>,
    pub perms: HashMap<(u64, u32), Arc<Perm>>,
    pub l1: Vec<L1Spec>,
    pub l1_every: u64,
    pub sets_counts: Option<String>,
}

/// One config's estimate of one sampled record, on its way to a metric thread.
pub struct SketchPayload {
    pub cfg: usize,
    pub p: Vec<(u32, f64)>,
    pub raw_mass: f64,
    pub info: Info,
}

enum Msg {
    /// The exact colour's estimate (for the configs) and its terms (for the
    /// side stores), each when someone needs it.
    Exact { rec: u64, tx: usize, p: Option<Arc<Vec<(u32, f64)>>>, terms: Option<Arc<ExactTerms>> },
    Sketch { rec: u64, payload: SketchPayload },
    Kmv { rec: u64, payload: KmvPayload },
    L1 { rec: u64, spec: usize, v: Vec<f64> },
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
    terms: Option<Arc<ExactTerms>>,
    waiting: Vec<Payload>,
    done: usize,
    /// `A . C_exact` and its denominators, per L1 seed, once computed.
    l1_truth: HashMap<u64, Arc<(Vec<f64>, Vec<f64>)>>,
}

/// What every metric thread needs to read the side stores.
pub struct Side {
    pub kmv: Vec<KmvSpec>,
    pub perms: Vec<Arc<Perm>>,
    pub l1: Vec<L1Spec>,
    pub every: u64,
    pub l1_every: u64,
}

impl Side {
    /// How many payloads record `rec` gets from the stores besides `configs`.
    fn expected(&self, configs: usize, rec: u64) -> usize {
        let mut n = 0;
        if rec % self.every == 0 {
            n += configs + self.kmv.len();
        }
        if rec % self.l1_every == 0 {
            n += self.l1.len();
        }
        n
    }
    /// Whether the exact store must send the record's terms, not just its
    /// estimate.
    fn wants_terms(&self, rec: u64) -> bool {
        (!self.kmv.is_empty() && rec % self.every == 0) || (!self.l1.is_empty() && rec % self.l1_every == 0)
    }
}

/// The metric thread a record's messages go to.  Any function of `rec`
/// would do -- the rows are sorted at the end -- as long as every producer
/// uses the same one.
fn slot(rec: u64, t: usize) -> usize {
    (hash::splitmix64(rec) % t as u64) as usize
}

/// A sampled record's exact terms, zeros included (the `sets` support), and
/// those of their blocks that the `pos` support leaves out ([`Exact::off`]).
pub struct ExactTerms {
    pub terms: Vec<(u32, f64)>,
    pub off: Vec<u32>,
}

/// One KMV store's sketch of a sampled record and its two counts.
pub struct KmvPayload {
    pub spec: usize,
    pub r: Vec<u32>,
    pub full: bool,
    pub n_hat: f64,
    pub n_hat_m: f64,
    pub univ: u64,
}

/// One row of `kmv-rows.tsv`.
#[derive(Clone, Debug)]
pub struct KmvRow {
    pub rec: u64,
    pub tx: usize,
    pub spec: usize,
    pub n_sets: usize,
    pub n_pos: usize,
    pub len: usize,
    pub full: bool,
    pub n_hat: f64,
    pub n_hat_m: f64,
    pub univ: u64,
    /// The sketch is exactly `K_k` of the exact support, and `full` iff the
    /// support is larger than `k`.
    pub set_ok: bool,
}

impl KmvRow {
    /// The exact size of the support this row's spec sketches.
    pub fn n(&self, side: &Side) -> usize {
        match side.kmv[self.spec].support {
            Support::Sets => self.n_sets,
            Support::Pos => self.n_pos,
        }
    }
}

/// One row of `l1-rows.tsv`.
#[derive(Clone, Debug)]
pub struct L1Row {
    pub rec: u64,
    pub spec: usize,
    pub n: usize,
    pub max_rel_dev: f64,
}

enum Payload {
    Sketch(SketchPayload),
    Kmv(KmvPayload),
    L1(usize, Vec<f64>),
}

/// The KMV check: the bottom-k of the exact support under the spec's
/// permutation, against the sketch.
fn kmv_row(side: &Side, rec: u64, tx: usize, t: &ExactTerms, p: &KmvPayload) -> KmvRow {
    let spec = &side.kmv[p.spec];
    let perm = &side.perms[p.spec];
    let n_sets = t.terms.len();
    let n_pos = n_sets - t.off.len();
    let mut r: Vec<u32> = t
        .terms
        .iter()
        .filter(|x| spec.support == Support::Sets || t.off.binary_search(&x.0).is_err())
        .map(|x| perm.rank(x.0))
        .collect();
    let n = r.len();
    if n > spec.k {
        r.select_nth_unstable(spec.k - 1);
        r.truncate(spec.k);
    }
    r.sort_unstable();
    KmvRow {
        rec,
        tx,
        spec: p.spec,
        n_sets,
        n_pos,
        len: p.r.len(),
        full: p.full,
        n_hat: p.n_hat,
        n_hat_m: p.n_hat_m,
        univ: p.univ,
        set_ok: r == p.r && p.full == (n > spec.k),
    }
}

/// `(A C)_j` and `sum_b |A_jb| c_b` for `j < m`, straight from the exact
/// terms and the hash: `m n` Cauchy draws.
fn l1_truth(seed: u64, m: usize, terms: &[(u32, f64)]) -> (Vec<f64>, Vec<f64>) {
    let mut ac = vec![0.0; m];
    let mut den = vec![0.0; m];
    for &(b, c) in terms {
        if c == 0.0 {
            continue;
        }
        for j in 0..m {
            let a = hash::cauchy(seed, b, j as u32);
            ac[j] += c * a;
            den[j] += (c * a).abs();
        }
    }
    (ac, den)
}

#[derive(Default)]
struct MetricOut {
    rows: Vec<Row>,
    kmv: Vec<KmvRow>,
    l1: Vec<L1Row>,
}

fn metric_thread(rx: Receiver<Msg>, configs: Arc<Vec<Config>>, keys: Arc<HashMap<u64, Arc<Keys>>>, side: Arc<Side>) -> MetricOut {
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut out = MetricOut::default();
    let key_of = |c: &Config| keys.get(&c.seed).map(|k| &**k);
    // The largest m per L1 seed: one projection serves every spec of a seed.
    let mut m_of: HashMap<u64, usize> = HashMap::new();
    for s in &side.l1 {
        let e = m_of.entry(s.seed).or_insert(0);
        *e = (*e).max(s.m);
    }
    let fresh = |tx| Pending { tx, exact: None, terms: None, waiting: Vec::new(), done: 0, l1_truth: HashMap::new() };
    while let Ok(msg) = rx.recv() {
        let (rec, ready) = match msg {
            Msg::Exact { rec, tx, p, terms } => {
                let e = pending.entry(rec).or_insert_with(|| fresh(tx));
                e.tx = tx;
                e.exact = p;
                e.terms = terms;
                (rec, std::mem::take(&mut e.waiting))
            }
            Msg::Sketch { rec, payload } => (rec, vec![Payload::Sketch(payload)]),
            Msg::Kmv { rec, payload } => (rec, vec![Payload::Kmv(payload)]),
            Msg::L1 { rec, spec, v } => (rec, vec![Payload::L1(spec, v)]),
        };
        let e = pending.entry(rec).or_insert_with(|| fresh(0));
        if e.exact.is_none() && e.terms.is_none() {
            e.waiting.extend(ready);
            continue;
        }
        for pl in ready {
            match pl {
                Payload::Sketch(sk) => {
                    let c = &configs[sk.cfg];
                    let exact = e.exact.as_ref().expect("the estimate came with the record");
                    out.rows.push(row(c, sk.cfg, key_of(c), rec, e.tx, exact, &sk));
                }
                Payload::Kmv(p) => {
                    let terms = e.terms.as_ref().expect("the terms came with the record");
                    out.kmv.push(kmv_row(&side, rec, e.tx, terms, &p));
                }
                Payload::L1(spec, v) => {
                    let s = &side.l1[spec];
                    let terms = e.terms.clone().expect("the terms came with the record");
                    let m_max = m_of[&s.seed];
                    let truth = e.l1_truth.entry(s.seed).or_insert_with(|| Arc::new(l1_truth(s.seed, m_max, &terms.terms))).clone();
                    let (ac, den) = &*truth;
                    let mut worst = 0.0f64;
                    for j in 0..s.m {
                        let d = (v[j] - ac[j]).abs();
                        let rel = if den[j] > 0.0 { d / den[j] } else { d };
                        worst = worst.max(rel);
                    }
                    out.l1.push(L1Row { rec, spec, n: terms.terms.len(), max_rel_dev: worst });
                }
            }
            e.done += 1;
        }
        if e.done == side.expected(configs.len(), rec) {
            pending.remove(&rec);
        }
    }
    out
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

#[allow(clippy::too_many_arguments)]
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
    let t = metric.len();
    while let Ok(batch) = rx.recv() {
        d.batch(&batch, |rec, _tx, c, alg| {
            if rec % every == 0 {
                let mut p = Vec::new();
                alg.emit(c, &mut p);
                let payload = SketchPayload { cfg: cfg_id, p, raw_mass: alg.raw_mass(c), info: alg.info(c) };
                let _ = metric[slot(rec, t)].send(Msg::Sketch { rec, payload });
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

/// A KMV store beside the exact one: on sampled records, the sketch and both
/// of its counts (the restricted universe kept here, where the records pass
/// in order).
#[allow(clippy::too_many_arguments)]
fn kmv_thread(
    id: usize,
    spec: KmvSpec,
    perm: Arc<Perm>,
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    metric: Vec<Sender<Msg>>,
    every: u64,
    stats: bool,
) -> io::Result<Cost> {
    let label = spec.label();
    let m = perm.universe();
    let mut d = Driver::new(KmvStore::new(spec, perm.clone()), oracle, stats.then(|| label.clone()));
    let mut uni = Universe::new(perm);
    let mut frontier: Option<usize> = None;
    let t = metric.len();
    while let Ok(batch) = rx.recv() {
        let first = batch.first;
        d.batch(&batch, |rec, _tx, c, _alg| {
            let b = batch.recs[(rec - first) as usize].block;
            let f = frontier.map_or(b, |f| f.max(b));
            frontier = Some(f);
            if rec % every == 0 {
                uni.grow(f as u64 + 1);
                let payload = KmvPayload {
                    spec: id,
                    r: c.r.to_vec(),
                    full: c.full,
                    n_hat: uni.count(c.view(), spec.k),
                    n_hat_m: kmv::count_m(c.view(), spec.k, m),
                    univ: uni.size(),
                };
                let _ = metric[slot(rec, t)].send(Msg::Kmv { rec, payload });
            }
            Ok(())
        })?;
    }
    let w = d.alg.work();
    let u = d.alg.usage();
    let (busy, records) = (d.busy, d.records);
    d.finish();
    Ok(Cost { label, busy, records, peak_entries: u.peak_entries, peak_sketches: u.peak_sketches, reductions: w.reductions, off_by_one: 0 })
}

/// An L1 store beside the exact one: its vector on the records sampled for it.
#[allow(clippy::too_many_arguments)]
fn l1_thread(
    id: usize,
    spec: L1Spec,
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    metric: Vec<Sender<Msg>>,
    every: u64,
    stats: bool,
) -> io::Result<Cost> {
    let label = spec.label();
    let mut d = Driver::new(L1Store::new(spec), oracle, stats.then(|| label.clone()));
    let t = metric.len();
    while let Ok(batch) = rx.recv() {
        d.batch(&batch, |rec, _tx, c, _alg| {
            if rec % every == 0 {
                let _ = metric[slot(rec, t)].send(Msg::L1 { rec, spec: id, v: c.to_f64() });
            }
            Ok(())
        })?;
    }
    let u = d.alg.usage();
    let (busy, records) = (d.busy, d.records);
    d.finish();
    Ok(Cost { label, busy, records, peak_entries: u.peak_entries, peak_sketches: u.peak_sketches, reductions: 0, off_by_one: 0 })
}

/// What the exact store found about the sets-counts reference.
#[derive(Clone, Copy, Debug, Default)]
pub struct RefCheck {
    pub checked: u64,
    pub mismatch: u64,
    pub first_bad: Option<(u64, usize, u32)>,
}

#[allow(clippy::too_many_arguments)]
fn exact_thread(
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    refs: Option<Receiver<Arc<RefBatch>>>,
    mut counts: Option<Receiver<Arc<CountBatch>>>,
    metric: Vec<Sender<Msg>>,
    every: u64,
    side: Arc<Side>,
    configs: usize,
    stats: bool,
) -> io::Result<(Cost, RefCheck)> {
    let mut d = Driver::new(Exact::new(), oracle, stats.then(|| "exact".to_string()));
    let t = metric.len();
    let mut worst = 0.0f64;
    let mut rc = RefCheck::default();
    let refs_rx: Vec<Receiver<Arc<RefBatch>>> = refs.into_iter().collect();
    while let Ok(batch) = rx.recv() {
        let r = feed::next_refs(&refs_rx, &batch)?;
        let first = batch.first;
        let cb = counts.as_ref().and_then(|c| c.recv().ok());
        if cb.is_none() {
            counts = None;
        }
        if let Some(cb) = &cb {
            if cb.first != first {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("sets-counts batch at {} for records at {}", cb.first, first)));
            }
        }
        d.batch(&batch, |rec, tx, c, alg| {
            let sampled = rec % every == 0;
            let want_terms = side.wants_terms(rec) || (cb.is_some() && sampled);
            // The exact estimate: for the configs' metrics, and for the
            // Laplace reference whenever one is open, configs or not.
            let want_p = sampled && (configs > 0 || !refs_rx.is_empty());
            if !want_p && !want_terms {
                return Ok(());
            }
            let terms = want_terms.then(|| ExactTerms { terms: alg.terms(c), off: alg.off(c) });
            if sampled {
                if let (Some(cb), Some(t)) = (&cb, &terms) {
                    if let Some(&want) = cb.vals.get((rec - first) as usize) {
                        rc.checked += 1;
                        if t.terms.len() != want as usize {
                            rc.mismatch += 1;
                            rc.first_bad.get_or_insert((rec, t.terms.len(), want));
                        }
                    }
                }
            }
            let p = want_p.then(|| {
                let mut p = Vec::new();
                alg.emit(c, &mut p);
                p
            });
            if let (Some(p), Some(rb)) = (&p, r.first()) {
                if let Some(RefVal::Laplace(want)) = &rb.vals[(rec - first) as usize] {
                    let (_, e) = lap_err(&metrics::laplace(p), want);
                    worst = worst.max(e);
                    if e > 1e-9 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("record {} (tx {}): the exact colour's Laplace values differ from the reference by {:e}", rec, tx, e),
                        ));
                    }
                }
            }
            if side.expected(configs, rec) > 0 {
                let terms = terms.filter(|_| side.wants_terms(rec)).map(Arc::new);
                let p = p.filter(|_| configs > 0).map(Arc::new);
                let _ = metric[slot(rec, t)].send(Msg::Exact { rec, tx, p, terms });
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
    Ok((Cost { label: "exact".into(), busy, records, peak_entries: u.peak_entries, peak_sketches: 0, reductions: 0, off_by_one: 0 }, rc))
}

pub fn run(opts: EvalOpts, input: Box<dyn io::Read + Send>, oracle: Option<Arc<Oracle>>, keys: HashMap<u64, Arc<Keys>>) -> io::Result<()> {
    // The side stores only: a repeated --config is what eval always accepted.
    crate::count::unique_labels("eval", "--kmv", opts.kmv.iter().map(|s| s.label()))?;
    crate::count::unique_labels("eval", "--l1", opts.l1.iter().map(|s| s.label()))?;
    std::fs::create_dir_all(&opts.out)?;
    let configs = Arc::new(opts.configs.clone());
    let keys = Arc::new(keys);
    let t = opts.metric_threads.max(1);
    let every = opts.every.max(1);
    let side = Arc::new(Side {
        kmv: opts.kmv.clone(),
        perms: opts
            .kmv
            .iter()
            .map(|s| opts.perms.get(&(s.seed, s.bits)).cloned().unwrap_or_else(|| Arc::new(Perm::new(s.seed, s.bits))))
            .collect(),
        l1: opts.l1.clone(),
        every,
        l1_every: opts.l1_every.max(1),
    });

    let mut metric_tx = Vec::new();
    let mut metric_handles = Vec::new();
    for i in 0..t {
        let (tx, rx) = channel::<Msg>();
        metric_tx.push(tx);
        let (c, k, s) = (configs.clone(), keys.clone(), side.clone());
        metric_handles.push(std::thread::Builder::new().name(format!("metric-{}", i)).spawn(move || metric_thread(rx, c, k, s))?);
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
    for (i, spec) in opts.kmv.iter().enumerate() {
        let (tx, rx) = sync_channel::<Arc<Batch>>(8);
        senders.push(tx);
        let (spec, perm, o, m, stats) = (*spec, side.perms[i].clone(), oracle.clone(), metric_tx.clone(), opts.stats);
        stores.push(std::thread::Builder::new().name(spec.label()).spawn(move || kmv_thread(i, spec, perm, o, rx, m, every, stats))?);
    }
    for (i, spec) in opts.l1.iter().enumerate() {
        let (tx, rx) = sync_channel::<Arc<Batch>>(8);
        senders.push(tx);
        let (spec, o, m, stats, le) = (*spec, oracle.clone(), metric_tx.clone(), opts.stats, side.l1_every);
        stores.push(std::thread::Builder::new().name(spec.label()).spawn(move || l1_thread(i, spec, o, rx, m, le, stats))?);
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
    let mut counts_handle = None;
    let counts_rx = match &opts.sets_counts {
        Some(path) => {
            let (ctx, crx) = sync_channel(8);
            counts_handle = Some(feed::spawn_counts(path.clone(), opts.limit, vec![ctx]));
            Some(crx)
        }
        None => None,
    };
    let (o, m, stats, s, nc) = (oracle.clone(), metric_tx.clone(), opts.stats, side.clone(), configs.len());
    let exact = std::thread::Builder::new().name("exact".into()).spawn(move || exact_thread(o, rx, ref_rx, counts_rx, m, every, s, nc, stats))?;
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
    let mut ref_check = RefCheck::default();
    match exact.join().expect("the exact store panicked") {
        Ok((c, rc)) => {
            costs.push(c);
            ref_check = rc;
        }
        Err(e) => {
            first_err.get_or_insert(e);
        }
    }
    let read = reader.join().expect("the reader panicked");
    // A reference stopped early by its consumer is not an error; one that
    // failed is the cause of its consumer's "a reference ended".
    let ref_results = ref_handle.into_iter().map(|h| h.join().expect("a reference reader panicked")).collect();
    let counts_result = counts_handle.map(|h| h.join().expect("the sets-counts reader panicked"));
    let mut rows: Vec<Row> = Vec::new();
    let mut kmv_rows: Vec<KmvRow> = Vec::new();
    let mut l1_rows: Vec<L1Row> = Vec::new();
    for h in metric_handles {
        let o = h.join().expect("a metric thread panicked");
        rows.extend(o.rows);
        kmv_rows.extend(o.kmv);
        l1_rows.extend(o.l1);
    }
    if let Some(e) = feed::root_cause(first_err, ref_results) {
        return Err(e);
    }
    if let Some(Err(e)) = counts_result {
        return Err(e);
    }
    let records = read?;
    rows.sort_by_key(|r| (r.rec, r.cfg));
    kmv_rows.sort_by_key(|r| (r.rec, r.spec));
    l1_rows.sort_by_key(|r| (r.rec, r.spec));

    let mut worst = Vec::new();
    if !configs.is_empty() {
        write_rows(&format!("{}/rows.tsv", opts.out), &rows, &configs)?;
        worst = write_summary(&format!("{}/summary.tsv", opts.out), &format!("{}/seeds.tsv", opts.out), &rows, &configs)?;
    }
    let mut kmv_bad = 0;
    if !side.kmv.is_empty() || opts.sets_counts.is_some() {
        kmv_bad = write_kmv(&opts.out, &kmv_rows, &side, &ref_check, opts.sets_counts.is_some())?;
    }
    if !side.l1.is_empty() {
        write_l1(&opts.out, &l1_rows, &side)?;
    }
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
    if kmv_bad > 0 || ref_check.mismatch > 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} KMV sketch(es) differ from the bottom-k of the exact support and {} support size(s) from the sets-counts reference (first: {:?}); see {}/kmv-summary.tsv",
                kmv_bad, ref_check.mismatch, ref_check.first_bad, opts.out
            ),
        ));
    }
    Ok(())
}

/// `kmv-rows.tsv`, `kmv-summary.tsv` and `kmv-seeds.tsv`; answers the number
/// of set mismatches.
///
/// The summary's strata are descriptive (one run, one permutation, correlated
/// rows); `kmv-seeds.tsv` is where a bias or a variance is tested, with the
/// seeds as the unit ([`crate::count::SeedStrata`]).
fn write_kmv(out: &str, rows: &[KmvRow], side: &Side, rc: &RefCheck, have_ref: bool) -> io::Result<u64> {
    let f = metrics::fmt;
    let labels: Vec<String> = side.kmv.iter().map(|s| s.label()).collect();
    let mut w = BufWriter::new(File::create(format!("{}/kmv-rows.tsv", out))?);
    writeln!(w, "rec\ttx\tlabel\tn_sets\tn_pos\tlen\tfull\tuniv\tn_hat\trel_err\tn_hat_M\trel_err_M\tset_ok")?;
    for r in rows {
        let n = r.n(side) as f64;
        writeln!(
            w,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.rec,
            r.tx,
            labels[r.spec],
            r.n_sets,
            r.n_pos,
            r.len,
            r.full as u8,
            r.univ,
            f(r.n_hat),
            f(r.n_hat / n - 1.0),
            f(r.n_hat_m),
            f(r.n_hat_m / n - 1.0),
            r.set_ok as u8
        )?;
    }
    w.flush()?;

    let mut w = BufWriter::new(File::create(format!("{}/kmv-summary.tsv", out))?);
    writeln!(w, "label\tsupport\testimator\tstratum\trecords\tmean_rel_err\trms_rel_err\ttheory_rse\trms_over_theory\tblock_se\tblocks")?;
    let mut bad_total = 0;
    let mut idx = [0usize; 3];
    for (si, spec) in side.kmv.iter().enumerate() {
        let mine: Vec<&KmvRow> = rows.iter().filter(|r| r.spec == si).collect();
        for (e, est) in crate::count::ESTIMATORS.iter().enumerate() {
            let mut acc = vec![(0u64, 0.0f64, 0.0f64, 0.0f64, metrics::BlockMeans::default()); kmv::N_STRATA];
            for r in &mine {
                let n = r.n(side);
                let (x, u) = if e == 0 { (r.n_hat, r.univ) } else { (r.n_hat_m, side.perms[si].universe()) };
                let err = x / n as f64 - 1.0;
                let th = if n > spec.k { kmv::count_se(n as f64, spec.k, u) / n as f64 } else { 0.0 };
                let k = kmv::strata_of(n as u64, spec.k, &mut idx);
                for &s in &idx[..k] {
                    let a = &mut acc[s];
                    a.0 += 1;
                    a.1 += err;
                    a.2 += err * err;
                    a.3 += th * th;
                    a.4.add(r.rec, err);
                }
            }
            for (s, a) in acc.iter().enumerate() {
                if a.0 == 0 {
                    continue;
                }
                let nf = a.0 as f64;
                let (rms, th) = ((a.2 / nf).sqrt(), (a.3 / nf).sqrt());
                let (bse, blocks) = a.4.se();
                writeln!(
                    w,
                    "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    labels[si],
                    spec.support.name(),
                    est,
                    kmv::stratum_name(s),
                    a.0,
                    f(a.1 / nf),
                    f(rms),
                    f(th),
                    f(if th > 0.0 { rms / th } else { f64::NAN }),
                    f(bse),
                    blocks
                )?;
            }
        }
        let bad = mine.iter().filter(|r| !r.set_ok).count() as u64;
        bad_total += bad;
        writeln!(w, "{}\t{}\tNA\tset_mismatch\t{}\t{}\tNA\tNA\tNA\tNA\tNA", labels[si], spec.support.name(), mine.len(), bad)?;
    }
    if have_ref {
        writeln!(w, "sets-counts\tsets\tNA\tsupport_ref_mismatch\t{}\t{}\tNA\tNA\tNA\tNA\tNA", rc.checked, rc.mismatch)?;
    }
    w.flush()?;

    // Seed families: rows grouped by record, the seeds side by side.
    let seeds_path = format!("{}/kmv-seeds.tsv", out);
    let mut fams: Vec<(String, Vec<usize>)> = Vec::new();
    for (si, l) in labels.iter().enumerate() {
        let fam = metrics::family(l);
        match fams.iter_mut().find(|x| x.0 == fam) {
            Some(x) => x.1.push(si),
            None => fams.push((fam, vec![si])),
        }
    }
    fams.retain(|x| x.1.len() >= 2);
    if fams.is_empty() {
        let _ = std::fs::remove_file(&seeds_path);
        return Ok(bad_total);
    }
    let mut w = BufWriter::new(File::create(&seeds_path)?);
    writeln!(w, "{}", crate::count::SEEDS_HEADER)?;
    for (fam, members) in &fams {
        let spec = side.kmv[members[0]];
        let s = members.len();
        let mut tables = [crate::count::SeedStrata::new(s), crate::count::SeedStrata::new(s)];
        let mut by_rec: std::collections::BTreeMap<u64, Vec<Option<&KmvRow>>> = std::collections::BTreeMap::new();
        for r in rows {
            if let Some(pos) = members.iter().position(|&m| m == r.spec) {
                by_rec.entry(r.rec).or_insert_with(|| vec![None; s])[pos] = Some(r);
            }
        }
        for v in by_rec.values() {
            if v.iter().any(|x| x.is_none()) {
                continue;
            }
            let r0 = v[0].expect("checked");
            let n = r0.n(side);
            for (e, table) in tables.iter_mut().enumerate() {
                let es: Vec<f64> = v
                    .iter()
                    .map(|x| {
                        let x = x.expect("checked");
                        (if e == 0 { x.n_hat } else { x.n_hat_m }) / n as f64 - 1.0
                    })
                    .collect();
                let u = if e == 0 { r0.univ } else { side.perms[members[0]].universe() };
                let th = if n > spec.k { kmv::count_se(n as f64, spec.k, u) / n as f64 } else { 0.0 };
                table.add(n as u64, spec.k, &es, th);
            }
        }
        for (e, t) in tables.iter().enumerate() {
            crate::count::write_seed_lines(&mut w, fam, crate::count::ESTIMATORS[e], s, &t.lines(crate::count::ALPHA))?;
        }
    }
    w.flush()?;
    Ok(bad_total)
}

/// `l1-rows.tsv` and `l1-summary.tsv` (max and p99 of the worst relative
/// deviation per record, per label).
fn write_l1(out: &str, rows: &[L1Row], side: &Side) -> io::Result<()> {
    let f = metrics::fmt;
    let labels: Vec<String> = side.l1.iter().map(|s| s.label()).collect();
    let mut w = BufWriter::new(File::create(format!("{}/l1-rows.tsv", out))?);
    writeln!(w, "rec\tlabel\tn\tmax_rel_dev")?;
    for r in rows {
        writeln!(w, "{}\t{}\t{}\t{}", r.rec, labels[r.spec], r.n, f(r.max_rel_dev))?;
    }
    w.flush()?;
    let mut w = BufWriter::new(File::create(format!("{}/l1-summary.tsv", out))?);
    writeln!(w, "label\trecords\tmax_rel_dev\tp99_rel_dev")?;
    for (si, l) in labels.iter().enumerate() {
        let mut v: Vec<f64> = rows.iter().filter(|r| r.spec == si).map(|r| r.max_rel_dev).collect();
        v.sort_by(|a, b| a.total_cmp(b));
        writeln!(w, "{}\t{}\t{}\t{}", l, v.len(), f(v.last().copied().unwrap_or(f64::NAN)), f(quantile_sorted(&v, 0.99)))?;
    }
    w.flush()
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
