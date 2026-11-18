//! # `emit`, `decode`, `chain` and `check`: wiring the pieces into threads
//!
//! - **emit**: a reader thread parses, this thread folds one config and
//!   hands its estimates over in snapshots of at most ~4 MB of terms, and an
//!   encoder thread turns those into bytes and writes them to stdout.  Three
//!   stages, so parsing, sketching and the pipe to `zstd` overlap.
//! - **chain**: a reader thread broadcasts to one store thread per config;
//!   each reference file has a thread of its own broadcasting its batches to
//!   the same stores; each store compares its sampled records as it goes (the
//!   comparison is `O(k)` and cheaper than the merge it follows, so it does
//!   not get threads of its own).
//! - **check**: the decoder on this thread, the references on theirs, the
//!   same [`Buckets`] as `chain`.
//! - **decode**: binary to text, `SKETCH01` or `KMVCNT01` by the magic.

use crate::algebra::{Algebra, Config, Sketcher, Variant};
use crate::chain::{self, snapshot, BucketCost, Buckets, Refs};
use crate::driver::Driver;
use crate::encode::{self, Decoded, Decoder, Encoder};
use crate::eval::{write_cost, Cost};
use crate::feed::{self, Batch, RefBatch, RefKind, BATCH};
use crate::hash::Keys;
use crate::oracle::Oracle;
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread::JoinHandle;

const CHUNK: usize = 4 << 20;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Bin,
    Text,
}

/// The references a `chain` or `check` run compares against.
#[derive(Clone, Default, Debug)]
pub struct RefPaths {
    pub laplace: Option<String>,
    pub moments: Option<String>,
    pub bands: Option<String>,
}

impl RefPaths {
    fn list(&self) -> Vec<(String, RefKind)> {
        let mut v = Vec::new();
        if let Some(p) = &self.laplace {
            v.push((p.clone(), RefKind::Laplace));
        }
        if let Some(p) = &self.moments {
            v.push((p.clone(), RefKind::Moments));
        }
        if let Some(p) = &self.bands {
            v.push((p.clone(), RefKind::Bands));
        }
        v
    }

    /// One reader thread per reference, each broadcasting to `consumers`
    /// receivers; answers the receivers grouped per consumer.
    fn spawn(
        &self,
        every: u64,
        limit: u64,
        consumers: usize,
    ) -> (Vec<Vec<Receiver<Arc<RefBatch>>>>, Vec<JoinHandle<io::Result<u64>>>) {
        let mut per: Vec<Vec<Receiver<Arc<RefBatch>>>> = (0..consumers).map(|_| Vec::new()).collect();
        let mut handles = Vec::new();
        for (path, kind) in self.list() {
            let mut to: Vec<SyncSender<Arc<RefBatch>>> = Vec::new();
            for slot in per.iter_mut() {
                let (tx, rx) = sync_channel(8);
                to.push(tx);
                slot.push(rx);
            }
            handles.push(feed::spawn_ref(path, kind, every, limit, to));
        }
        (per, handles)
    }
}

fn sketcher(cfg: Config, keys: &HashMap<u64, Arc<Keys>>) -> Sketcher {
    let k = if cfg.variant == Variant::Coord { keys.get(&cfg.seed).cloned() } else { None };
    Sketcher::new(cfg, k)
}

/// A run of records' estimates, as the fold thread hands them to the encoder:
/// everything `emit` writes, with the `Rc` handles already read out of it.
#[derive(Default)]
struct Snapshots {
    tx: Vec<u64>,
    meta: Vec<encode::Meta>,
    ends: Vec<u32>,
    p: Vec<(u32, f32)>,
}

/// Entries a [`Snapshots`] collects before it is sent: 4 MB of `(u32, f32)`
/// terms.  The check runs after every record, so one snapshot ends up with
/// at most `SNAP_ENTRIES + k + 1` terms (about 6 MB at `k = 2^18`), and its
/// `u32` offsets cannot wrap for any `k` [`Config::parse`] accepts.
const SNAP_ENTRIES: usize = 1 << 19;
/// Records a [`Snapshots`] collects at most (24 B each of tx, meta and end).
const SNAP_RECORDS: usize = 1 << 16;
const _: () = assert!(SNAP_ENTRIES + (1 << 20) + 1 < u32::MAX as usize);

/// The encoder and writer: turns snapshots into bytes and writes them.
fn encode_thread(
    rx: Receiver<Option<Snapshots>>,
    back: SyncSender<Snapshots>,
    cfg: Config,
    format: Format,
    mut out: Box<dyn Write + Send>,
) -> io::Result<()> {
    let mut enc = Encoder::default();
    let mut buf = Vec::with_capacity(CHUNK + (1 << 16));
    if format == Format::Bin {
        buf.extend_from_slice(&encode::header(&cfg));
    }
    let mut finished = false;
    for msg in rx.iter() {
        let Some(mut snap) = msg else {
            finished = true;
            break;
        };
        let mut lo = 0usize;
        for i in 0..snap.tx.len() {
            let hi = snap.ends[i] as usize;
            let p = &snap.p[lo..hi];
            match format {
                Format::Bin => enc.record(&mut buf, snap.tx[i], p, snap.meta[i]),
                Format::Text => encode::text_line(&mut buf, snap.tx[i], p),
            }
            lo = hi;
            if buf.len() >= CHUNK {
                out.write_all(&buf)?;
                buf.clear();
            }
        }
        snap.tx.clear();
        snap.meta.clear();
        snap.ends.clear();
        snap.p.clear();
        // Hand the buffers back for reuse; if the fold has finished, drop them.
        let _ = back.try_send(snap);
    }
    // `None` is the fold saying it finished; a channel closed without it is a
    // fold that failed, and its stream gets no trailer.
    if finished && format == Format::Bin {
        enc.trailer(&mut buf);
    }
    out.write_all(&buf)?;
    out.flush()
}

/// `emit`: every record's estimate, in record order, to `out`.
///
/// Three threads: the reader parses, this one folds and reads each record's
/// estimate out of its sketch (which must happen here, since a sketch is an
/// `Rc` and never leaves its thread), and an encoder turns those into bytes
/// and writes them.  Encoding is a third of the work at `k = 256` -- the
/// shared-value search and the varints touch every term -- and on its own
/// thread it overlaps the fold instead of adding to it.
///
/// A trailer is written only after the last record of a run that succeeded:
/// a stream cut short by an error has none, and `decode`/`check` refuse it.
#[allow(clippy::too_many_arguments)]
pub fn emit(
    cfg: Config,
    format: Format,
    input: Box<dyn Read + Send>,
    limit: u64,
    oracle: Option<Arc<Oracle>>,
    keys: &HashMap<u64, Arc<Keys>>,
    stats: bool,
    out: Box<dyn Write + Send>,
) -> io::Result<()> {
    let (tx, rx) = sync_channel::<Arc<Batch>>(8);
    let reader = feed::spawn_reader(input, limit, vec![tx]);
    let (stx, srx) = sync_channel::<Option<Snapshots>>(4);
    let (back_tx, back_rx) = sync_channel::<Snapshots>(8);
    let encoder = std::thread::Builder::new()
        .name("encoder".into())
        .spawn(move || encode_thread(srx, back_tx, cfg, format, out))?;

    let mut d = Driver::new(sketcher(cfg, keys), oracle, stats.then(|| cfg.label()));
    let (mut p64, mut p32) = (Vec::new(), Vec::new());
    let mut snap = Snapshots::default();
    let mut result: io::Result<()> = Ok(());
    let closed = || io::Error::new(io::ErrorKind::BrokenPipe, "the encoder stopped");
    for batch in rx.iter() {
        let r = d.batch(&batch, |_rec, tx, c, alg| {
            let meta = snapshot(alg, c, &mut p64, &mut p32);
            snap.tx.push(tx as u64);
            snap.meta.push(meta);
            snap.p.extend_from_slice(&p32);
            snap.ends.push(snap.p.len() as u32);
            // Checked per record, so a snapshot holds at most SNAP_ENTRIES
            // plus one record's `k + 1` terms, whatever `k`.
            if snap.p.len() >= SNAP_ENTRIES || snap.tx.len() >= SNAP_RECORDS {
                let next = back_rx.try_recv().unwrap_or_default();
                stx.send(Some(std::mem::replace(&mut snap, next))).map_err(|_| closed())?;
            }
            Ok(())
        });
        if let Err(e) = r {
            result = Err(e);
            break;
        }
    }
    drop(rx);
    let read = reader.join().expect("the reader panicked");
    if result.is_ok() {
        result = read.map(|_| ());
    }
    if result.is_ok() && (stx.send(Some(snap)).is_err() || stx.send(None).is_err()) {
        result = Err(closed());
    }
    drop(stx);
    let written = encoder.join().expect("the encoder panicked");
    if stats {
        let u = d.alg.usage();
        eprintln!(
            "sketch emit [{}]: {} records, busy {:.1}s, peak entries {}, peak sketches {}, reductions {}, k+-1 {}, VmHWM {}",
            cfg.label(),
            d.records,
            d.busy,
            u.peak_entries,
            u.peak_sketches,
            d.alg.work().reductions,
            d.alg.work().off_by_one,
            crate::metrics::vm_hwm()
        );
    }
    // A writer error (a closed pipe) is the more informative of the two.
    written?;
    result
}

/// `decode`: a binary stream to text, by its magic.
///
/// - `SKETCH01` (what `emit` writes) to `--weighted`'s terms lines, byte for
///   byte what it always was.  With `select`, only those record indices, each
///   line prefixed by `rec<TAB>reduced<TAB>tau<TAB>`: this is how the
///   production varopt sketches of selected records are read without a new
///   fold.  It stops after the last selected record, so the trailer of a
///   whole-chain file is not reached and not verified.
/// - `KMVCNT01` (what `count` writes) to a TSV with a header, both counts
///   recomputed ([`crate::kmv::CntDecoder`]); `select` filters it the same
///   way.  A first comment line names the stream's spec and its support,
///   `# kmv-k1024-s1-pos k=1024 seed=1 support=pos bits=20 M=1048576`.
///   `se` and `se_M` are absolute standard errors, in blocks like the counts
///   beside them; the relative error is `se / n_hat`, which `summary`
///   ([`crate::summary`]) reports by depth.
pub fn decode(mut input: Box<dyn Read + Send>, mut out: Box<dyn Write + Send>, select: Option<&[u64]>) -> io::Result<()> {
    let mut magic = [0u8; 8];
    let mut got = 0;
    while got < 8 {
        let n = input.read(&mut magic[got..])?;
        if n == 0 {
            break;
        }
        got += n;
    }
    if got == 8 && &magic == crate::kmv::CNT_MAGIC {
        return decode_counts(input, out, magic, select);
    }
    let input = io::Cursor::new(magic[..got].to_vec()).chain(input);
    let mut dec = Decoder::new(io::BufReader::with_capacity(1 << 20, input))?;
    let mut r = Decoded::default();
    let mut buf = Vec::with_capacity(CHUNK + (1 << 16));
    let mut rec = 0u64;
    let mut next = 0usize;
    while dec.next(&mut r)? {
        match select {
            None => encode::text_line(&mut buf, r.tx, &r.p),
            Some(sel) => {
                if next < sel.len() && sel[next] == rec {
                    write!(buf, "{}\t{}\t{}\t", rec, r.meta.reduced as u8, r.meta.tau)?;
                    encode::text_line(&mut buf, r.tx, &r.p);
                    next += 1;
                }
                if next == sel.len() {
                    break;
                }
            }
        }
        rec += 1;
        if buf.len() >= CHUNK {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    out.write_all(&buf)?;
    out.flush()?;
    if let Some(sel) = select {
        if next < sel.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("the selection names record {} but the stream holds {}", sel[next], rec),
            ));
        }
    }
    Ok(())
}

fn decode_counts(input: Box<dyn Read + Send>, mut out: Box<dyn Write + Send>, magic: [u8; 8], select: Option<&[u64]>) -> io::Result<()> {
    let mut dec = crate::kmv::CntDecoder::new(io::BufReader::with_capacity(1 << 20, input), magic, None)?;
    let mut buf = Vec::with_capacity(CHUNK + (1 << 16));
    // The stream's spec first, as a comment line: a sets stream and a pos
    // stream decode to the same columns, and only this line tells them apart.
    let s = dec.spec;
    writeln!(buf, "# {} k={} seed={} support={} bits={} M={}", s.label(), s.k, s.seed, s.support.name(), s.bits, dec.m)?;
    writeln!(buf, "rec\ttx\tblock\tuniv\tfull\tm\tn_hat\tse\tn_hat_M\tse_M")?;
    let f = crate::metrics::fmt;
    let mut next = 0usize;
    while let Some(r) = dec.next()? {
        if let Some(sel) = select {
            if next >= sel.len() {
                break;
            }
            if sel[next] != r.rec {
                continue;
            }
            next += 1;
        }
        writeln!(
            buf,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.rec,
            r.r.tx,
            r.r.block,
            r.n_univ,
            r.r.full() as u8,
            r.r.len(),
            f(r.n_hat),
            f(r.se),
            f(r.n_hat_m),
            f(r.se_m)
        )?;
        if buf.len() >= CHUNK {
            out.write_all(&buf)?;
            buf.clear();
        }
    }
    out.write_all(&buf)?;
    out.flush()
}

pub struct ChainOpts {
    pub configs: Vec<Config>,
    pub out: String,
    pub limit: u64,
    pub every: u64,
    pub rows_every: u64,
    pub bucket: u64,
    pub refs: RefPaths,
    pub stats: bool,
}

/// A store's cost at the end of bucket `b`; `busy_before` is the busy time at
/// the end of the previous one.
fn cost_of(d: &Driver<Sketcher>, b: u64, busy_before: &mut f64) -> BucketCost {
    let u = d.alg.usage();
    let c = BucketCost {
        bucket: b,
        busy: d.busy - *busy_before,
        live_entries: u.live_entries,
        peak_entries: u.peak_entries,
        live_sketches: u.live_sketches,
        map: d.map_len(),
        reductions: d.alg.work().reductions,
    };
    *busy_before = d.busy;
    c
}

#[allow(clippy::too_many_arguments)]
fn chain_store(
    cfg: Config,
    alg: Sketcher,
    oracle: Option<Arc<Oracle>>,
    rx: Receiver<Arc<Batch>>,
    refs: Vec<Receiver<Arc<RefBatch>>>,
    mut buckets: Buckets,
    every: u64,
    bucket: u64,
    stats: bool,
) -> io::Result<(Buckets, Vec<BucketCost>, Cost)> {
    let label = cfg.label();
    let mut d = Driver::new(alg, oracle, stats.then(|| label.clone()));
    let (mut p64, mut p32) = (Vec::new(), Vec::new());
    let mut costs = Vec::new();
    let mut busy_before = 0.0;
    let mut next_bucket = bucket;
    while let Ok(batch) = rx.recv() {
        let r = feed::next_refs(&refs, &batch)?;
        let first = batch.first;
        d.batch(&batch, |rec, tx, c, alg| {
            if rec % every != 0 {
                return Ok(());
            }
            let meta = snapshot(alg, c, &mut p64, &mut p32);
            let i = (rec - first) as usize;
            let refs = Refs::from_vals(r.iter().map(|rb| rb.vals[i].as_ref()));
            buckets.add(rec, tx as u64, &p32, meta, refs)
        })?;
        while d.records >= next_bucket {
            costs.push(cost_of(&d, next_bucket / bucket - 1, &mut busy_before));
            next_bucket += bucket;
        }
    }
    if d.records + bucket > next_bucket && d.records > 0 {
        costs.push(cost_of(&d, next_bucket / bucket - 1, &mut busy_before));
    }
    buckets.finish()?;
    let w = d.alg.work();
    let u = d.alg.usage();
    let cost = Cost {
        label,
        busy: d.busy,
        records: d.records,
        peak_entries: u.peak_entries,
        peak_sketches: u.peak_sketches,
        reductions: w.reductions,
        off_by_one: w.off_by_one,
    };
    d.finish();
    Ok((buckets, costs, cost))
}

/// `chain`: every config over the records, against the references.
pub fn chain(
    opts: ChainOpts,
    input: Box<dyn Read + Send>,
    oracle: Option<Arc<Oracle>>,
    keys: &HashMap<u64, Arc<Keys>>,
) -> io::Result<()> {
    std::fs::create_dir_all(&opts.out)?;
    let every = opts.every.max(1);
    let (mut ref_rx, ref_handles) = opts.refs.spawn(every, opts.limit, opts.configs.len());
    let mut senders = Vec::new();
    let mut stores = Vec::new();
    for (i, cfg) in opts.configs.iter().enumerate() {
        let (tx, rx) = sync_channel::<Arc<Batch>>(8);
        senders.push(tx);
        let cfg = *cfg;
        let alg = sketcher(cfg, keys);
        let refs = std::mem::take(&mut ref_rx[i]);
        let buckets = Buckets::new(
            cfg.label(),
            opts.bucket,
            opts.rows_every,
            Some(format!("{}/rows.{}.tsv", opts.out, cfg.label())),
        )?;
        let (o, stats, bucket) = (oracle.clone(), opts.stats, opts.bucket.max(1));
        stores.push(
            std::thread::Builder::new()
                .name(cfg.label())
                .spawn(move || chain_store(cfg, alg, o, rx, refs, buckets, every, bucket, stats))?,
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
    let ref_results = ref_handles.into_iter().map(|h| h.join().expect("a reference reader panicked")).collect();
    if let Some(e) = feed::root_cause(first_err, ref_results) {
        return Err(e);
    }
    let records = read?;
    let (mut buckets, mut costs, mut totals) = (Vec::new(), Vec::new(), Vec::new());
    for (b, c, t) in done {
        costs.push((t.label.clone(), c));
        buckets.push(b);
        totals.push(t);
    }
    chain::write_buckets(&format!("{}/buckets.tsv", opts.out), &buckets)?;
    chain::write_seeds(&format!("{}/seeds.tsv", opts.out), &buckets)?;
    chain::write_bucket_costs(&format!("{}/bucket_cost.tsv", opts.out), &costs)?;
    write_cost(&format!("{}/cost.tsv", opts.out), &totals)?;
    eprintln!("sketch chain: {} records over {} configs, written to {}", records, totals.len(), opts.out);
    Ok(())
}

pub struct CheckOpts {
    pub out: String,
    pub every: u64,
    pub rows_every: u64,
    pub bucket: u64,
    pub refs: RefPaths,
}

/// `check`'s loop: decode in batches of [`BATCH`], line the references up
/// against them, and fold the sampled records into `buckets`.
fn check_fold<R: Read>(
    dec: &mut Decoder<R>,
    buckets: &mut Buckets,
    refs: &[Receiver<Arc<RefBatch>>],
    every: u64,
) -> io::Result<()> {
    let mut batch: Vec<Decoded> = (0..BATCH).map(|_| Decoded::default()).collect();
    let mut first = 0u64;
    let mut more = true;
    while more {
        let mut n = 0;
        while n < BATCH {
            if !dec.next(&mut batch[n])? {
                more = false;
                break;
            }
            n += 1;
        }
        if n == 0 {
            break;
        }
        let mut got = Vec::with_capacity(refs.len());
        for rx in refs {
            let r = rx.recv().map_err(|_| feed::ref_closed(first))?;
            if r.first != first {
                return Err(io::Error::new(io::ErrorKind::InvalidData, format!("reference batch at {} for records at {}", r.first, first)));
            }
            feed::check_tx(&r, batch[..n].iter().map(|d| d.tx))?;
            got.push(r);
        }
        for (i, d) in batch[..n].iter().enumerate() {
            let rec = first + i as u64;
            if rec % every != 0 {
                continue;
            }
            let refs = Refs::from_vals(got.iter().map(|rb| rb.vals[i].as_ref()));
            buckets.add(rec, d.tx, &d.p, d.meta, refs)?;
        }
        first += n as u64;
    }
    Ok(())
}

/// `check`: the same aggregates as `chain`, from a stored binary stream.
pub fn check(opts: CheckOpts, input: Box<dyn Read + Send>) -> io::Result<()> {
    std::fs::create_dir_all(&opts.out)?;
    let every = opts.every.max(1);
    let mut dec = Decoder::new(io::BufReader::with_capacity(1 << 20, input))?;
    let label = dec.config.label();
    let mut buckets = Buckets::new(label.clone(), opts.bucket, opts.rows_every, Some(format!("{}/rows.{}.tsv", opts.out, label)))?;
    let (mut ref_rx, ref_handles) = opts.refs.spawn(every, u64::MAX, 1);
    let refs = std::mem::take(&mut ref_rx[0]);
    let folded = check_fold(&mut dec, &mut buckets, &refs, every);
    // Closing the channels stops readers that are still going; then a reader's
    // own error, if it had one, explains a reference that "ended" early.
    drop(refs);
    let ref_results = ref_handles.into_iter().map(|h| h.join().expect("a reference reader panicked")).collect();
    if let Some(e) = feed::root_cause(folded.err(), ref_results) {
        return Err(e);
    }
    buckets.finish()?;
    chain::write_buckets(&format!("{}/buckets.tsv", opts.out), &[buckets])?;
    eprintln!("sketch check: {} records of {} verified against their trailer, written to {}", dec.records, label, opts.out);
    Ok(())
}
