//! # One parse, many consumers; and the reference files read alongside
//!
//! Parsing the 150 GB of records runs at about 336k records a second, and a
//! sketch store at depth runs at a fraction of that, so the multi-config modes
//! parse once and broadcast: a reader thread turns records into `Arc<Batch>`es
//! of [`BATCH`] records and sends a clone of the `Arc` to every store thread,
//! each through its own `sync_channel(8)`.  The bound is what keeps memory
//! flat: the fastest store can run at most eight batches ahead of the slowest,
//! and the reader waits for the slowest.  `prefetch.rs` is not reused because
//! its batch is private to it and serves one consumer.
//!
//! ## References in lock-step
//!
//! The exact Laplace values, moments and binned colours of every transaction
//! of the chain are on disk as zstd'd text, one line per record in record
//! order.  A reference thread runs `zstd -dc` on one of them and cuts its
//! lines into batches on the same [`BATCH`] boundaries as the records, so a
//! consumer pairs batch `i` of the records with batch `i` of each reference.
//! Every line's tx id is parsed and checked against the record's -- a
//! reference for a different file, or one that skipped a line, fails at the
//! first record that disagrees, naming it -- and the numbers on a line are
//! parsed only for the records that are sampled (`rec % every == 0`), since
//! parsing floats is most of the cost of reading text.

use crate::sexp::{Input, Reader};
use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::SyncSender;
use std::sync::Arc;
use std::thread::JoinHandle;

pub const BATCH: usize = 8192;

/// The three fields of a record header the driver reads.
#[derive(Clone, Copy, Debug)]
pub struct Rec {
    pub tx: usize,
    pub block: usize,
    pub outputs: usize,
}

/// Up to [`BATCH`] consecutive records, their inputs flattened.
pub struct Batch {
    /// The record index of `recs[0]`.
    pub first: u64,
    pub recs: Vec<Rec>,
    pub inputs: Vec<Input>,
    /// `inputs[ends[i-1]..ends[i]]` are record `i`'s.
    pub ends: Vec<u32>,
}

impl Batch {
    pub fn inputs_of(&self, i: usize) -> &[Input] {
        let lo = if i == 0 { 0 } else { self.ends[i - 1] as usize };
        &self.inputs[lo..self.ends[i] as usize]
    }
    pub fn len(&self) -> usize {
        self.recs.len()
    }
}

/// Parse `input` into batches, handing each to `f` until it answers `false`
/// or `limit` records have been read.  Answers how many were read.
pub fn read_batches<R: Read>(input: R, limit: u64, mut f: impl FnMut(Arc<Batch>) -> bool) -> io::Result<u64> {
    let mut reader = Reader::new(input);
    let mut inputs = Vec::new();
    let mut count = 0u64;
    loop {
        let mut batch = Batch {
            first: count,
            recs: Vec::with_capacity(BATCH),
            inputs: Vec::new(),
            ends: Vec::with_capacity(BATCH),
        };
        while batch.recs.len() < BATCH && count < limit {
            let r = match reader.next_record(&mut inputs)? {
                Some(r) => r,
                None => break,
            };
            batch.recs.push(Rec { tx: r.tx_id, block: r.block_id, outputs: r.outputs });
            batch.inputs.extend_from_slice(&inputs);
            batch.ends.push(batch.inputs.len() as u32);
            count += 1;
        }
        if batch.recs.is_empty() {
            return Ok(count);
        }
        let full = batch.recs.len() == BATCH;
        if !f(Arc::new(batch)) || !full {
            return Ok(count);
        }
    }
}

/// [`read_batches`] on a thread of its own, broadcasting to `to`.  Stops early,
/// without error, when every consumer has gone.
pub fn spawn_reader(
    input: Box<dyn Read + Send>,
    limit: u64,
    to: Vec<SyncSender<Arc<Batch>>>,
) -> JoinHandle<io::Result<u64>> {
    std::thread::Builder::new()
        .name("records".into())
        .spawn(move || {
            read_batches(input, limit, |b| {
                let mut any = false;
                for t in &to {
                    any |= t.send(b.clone()).is_ok();
                }
                any
            })
        })
        .expect("spawn the record reader")
}

/// Which reference file a stream is, which decides how a sampled line parses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    /// `colors.laplace.zst`: tx, then the 7 exact Laplace values.
    Laplace,
    /// `colors.moments.zst`: tx, mean, spread, (binned participation ratio,
    /// not exact, not read).
    Moments,
    /// `colors.bands1024.zst`: tx, then `w:band,...` in decreasing band order.
    Bands,
}

impl RefKind {
    pub fn name(self) -> &'static str {
        match self {
            RefKind::Laplace => "laplace",
            RefKind::Moments => "moments",
            RefKind::Bands => "bands",
        }
    }
}

/// The parsed numbers of one sampled reference line.
#[derive(Clone, Debug, PartialEq)]
pub enum RefVal {
    Laplace([f64; 7]),
    Moments { mean: f64, sd: f64 },
    /// `(band, weight)`, ascending band.
    Bands(Vec<(u16, f32)>),
}

/// One batch of reference lines, on the same boundaries as a [`Batch`].
pub struct RefBatch {
    pub kind: RefKind,
    pub first: u64,
    pub tx: Vec<u64>,
    /// `Some` for the sampled records.
    pub vals: Vec<Option<RefVal>>,
}

fn bad(kind: RefKind, rec: u64, what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{} reference, line for record {}: {}", kind.name(), rec, what),
    )
}

/// One reference line: its tx id, and its numbers when `sampled`.  `rec`
/// only names the line in errors.  Public for `surface`, whose parser threads
/// call it on lines they were handed whole.
pub fn parse_line(kind: RefKind, rec: u64, line: &[u8], sampled: bool) -> io::Result<(u64, Option<RefVal>)> {
    let line = line.strip_suffix(b"\n").unwrap_or(line);
    let tab = line.iter().position(|&c| c == b'\t').unwrap_or(line.len());
    let tx = std::str::from_utf8(&line[..tab])
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .ok_or_else(|| bad(kind, rec, "no tx id"))?;
    if !sampled {
        return Ok((tx, None));
    }
    let rest = std::str::from_utf8(line.get(tab + 1..).unwrap_or(b"")).map_err(|_| bad(kind, rec, "not text"))?;
    let num = |s: &str| s.parse::<f64>().map_err(|_| bad(kind, rec, &format!("bad number {:?}", s)));
    let val = match kind {
        RefKind::Laplace => {
            let mut v = [0.0; 7];
            let mut it = rest.split('\t');
            for x in v.iter_mut() {
                *x = num(it.next().ok_or_else(|| bad(kind, rec, "fewer than 7 values"))?)?;
            }
            RefVal::Laplace(v)
        }
        RefKind::Moments => {
            let mut it = rest.split('\t');
            let mean = num(it.next().ok_or_else(|| bad(kind, rec, "no mean"))?)?;
            let sd = num(it.next().ok_or_else(|| bad(kind, rec, "no spread"))?)?;
            RefVal::Moments { mean, sd }
        }
        RefKind::Bands => {
            let mut v = Vec::new();
            if !rest.is_empty() {
                for term in rest.split(',') {
                    let (w, b) = term.split_once(':').ok_or_else(|| bad(kind, rec, "term without ':'"))?;
                    let w: f32 = w.parse().map_err(|_| bad(kind, rec, "bad weight"))?;
                    let b: u16 = b.parse().map_err(|_| bad(kind, rec, "bad band"))?;
                    v.push((b, w));
                }
            }
            v.sort_unstable_by_key(|x| x.0);
            RefVal::Bands(v)
        }
    };
    Ok((tx, Some(val)))
}

/// Cut the lines of `src` into [`RefBatch`]es, parsing the sampled ones, until
/// `limit` lines or `f` answers `false`.  Answers how many lines were read.
pub fn read_refs<B: BufRead>(
    mut src: B,
    kind: RefKind,
    every: u64,
    limit: u64,
    mut f: impl FnMut(Arc<RefBatch>) -> bool,
) -> io::Result<u64> {
    let mut line = Vec::with_capacity(1 << 16);
    let mut count = 0u64;
    loop {
        let mut b = RefBatch { kind, first: count, tx: Vec::with_capacity(BATCH), vals: Vec::with_capacity(BATCH) };
        while b.tx.len() < BATCH && count < limit {
            line.clear();
            if src.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let (tx, val) = parse_line(kind, count, &line, count % every == 0)?;
            b.tx.push(tx);
            b.vals.push(val);
            count += 1;
        }
        if b.tx.is_empty() {
            return Ok(count);
        }
        let full = b.tx.len() == BATCH;
        if !f(Arc::new(b)) || !full {
            return Ok(count);
        }
    }
}

/// A `zstd -dc <path>` child whose stdout is the reference text.
pub fn decompress(path: &str) -> io::Result<Child> {
    Command::new("zstd")
        .args(["-dc", "--", path])
        .stdout(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| io::Error::new(e.kind(), format!("cannot run zstd -dc {}: {}", path, e)))
}

/// A reference reader on its own thread, broadcasting to `to`.  The child is
/// killed when the consumers stop early, so a prefix run does not wait for the
/// rest of a terabyte to decompress.  A `zstd` that exits with an error after
/// the text ran out (a missing or corrupt file) is an error of the thread's,
/// and every error names the file.
///
/// A consumer that finds a reference's channel closed reports
/// [`ref_closed`]; that is only the symptom, and [`root_cause`] swaps it for
/// the reader's own error once the reader thread has been joined.
pub fn spawn_ref(
    path: String,
    kind: RefKind,
    every: u64,
    limit: u64,
    to: Vec<SyncSender<Arc<RefBatch>>>,
) -> JoinHandle<io::Result<u64>> {
    std::thread::Builder::new()
        .name(format!("ref-{}", kind.name()))
        .spawn(move || {
            let mut child = decompress(&path)?;
            let out = child.stdout.take().expect("piped");
            let mut stopped = false;
            let result = read_refs(BufReader::with_capacity(1 << 22, out), kind, every, limit, |b| {
                let mut any = false;
                for t in &to {
                    any |= t.send(b.clone()).is_ok();
                }
                stopped |= !any;
                any
            });
            let ran_out = matches!(result, Ok(n) if n < limit) && !stopped;
            if !ran_out {
                let _ = child.kill();
            }
            let status = child.wait();
            let result = match (result, status) {
                (Ok(_), Ok(st)) if ran_out && !st.success() => {
                    Err(io::Error::other(format!("zstd -dc exited with {}", st)))
                }
                (r, _) => r,
            };
            result.map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))
        })
        .expect("spawn a reference reader")
}

/// The error a consumer reports when a reference's channel closes before the
/// records end: the reader either reached the end of its file (a short
/// reference) or failed, and only its thread knows which.
#[derive(Debug)]
struct RefClosed(u64);

impl std::fmt::Display for RefClosed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a reference ended before record {}", self.0)
    }
}

impl std::error::Error for RefClosed {}

pub fn ref_closed(rec: u64) -> io::Error {
    io::Error::new(io::ErrorKind::UnexpectedEof, RefClosed(rec))
}

/// Whether `e` is [`ref_closed`]'s symptom rather than a cause.
pub fn is_ref_closed(e: &io::Error) -> bool {
    e.get_ref().is_some_and(|x| x.is::<RefClosed>())
}

/// The error to report from a run whose consumers ended with `first` (if they
/// failed) and whose reference readers ended with `refs`: a reader's own
/// error rather than the consumer's "a reference ended", which is only its
/// consequence.  A reader stopped by its consumers is not an error.
pub fn root_cause(first: Option<io::Error>, refs: Vec<io::Result<u64>>) -> Option<io::Error> {
    let reader = refs.into_iter().find_map(|r| r.err());
    match (first, reader) {
        (Some(e), Some(r)) if is_ref_closed(&e) => Some(r),
        (Some(e), _) => Some(e),
        (None, r) => r,
    }
}

/// Check that a reference batch lines up with a record batch: the same first
/// record, and the same tx id on every line.
pub fn check_alignment(records: &Batch, refs: &RefBatch) -> io::Result<()> {
    if refs.first != records.first {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} reference batch starts at record {} where the records' starts at {}",
                refs.kind.name(),
                refs.first,
                records.first
            ),
        ));
    }
    if refs.tx.len() < records.len() {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!(
                "{} reference ended at record {}, before the records did",
                refs.kind.name(),
                refs.first + refs.tx.len() as u64
            ),
        ));
    }
    check_tx(refs, records.recs.iter().map(|r| r.tx as u64))
}

/// [`check_alignment`] against bare tx ids, for `check` mode's decoded records.
pub fn check_tx(refs: &RefBatch, tx: impl Iterator<Item = u64>) -> io::Result<()> {
    for (i, t) in tx.enumerate() {
        match refs.tx.get(i) {
            Some(&r) if r == t => {}
            Some(&r) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "{} reference is misaligned at record {}: its line names tx {} but the record is tx {}",
                        refs.kind.name(),
                        refs.first + i as u64,
                        r,
                        t
                    ),
                ))
            }
            None => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    format!("{} reference ended at record {}, before the records did", refs.kind.name(), refs.first + i as u64),
                ))
            }
        }
    }
    Ok(())
}

/// Receive the next batch of each reference and check it against `batch`.
pub fn next_refs(
    rxs: &[std::sync::mpsc::Receiver<Arc<RefBatch>>],
    batch: &Batch,
) -> io::Result<Vec<Arc<RefBatch>>> {
    let mut out = Vec::with_capacity(rxs.len());
    for rx in rxs {
        let r = rx.recv().map_err(|_| ref_closed(batch.first))?;
        check_alignment(batch, &r)?;
        out.push(r);
    }
    Ok(out)
}

/// One batch of a sets-counts file: the exact `--sets` support size of each
/// record, on the same [`BATCH`] boundaries as the records.  `vals` is short
/// (or the batch never comes) past the file's end, which is how a reference
/// that covers only a prefix of the chain stops being compared.
pub struct CountBatch {
    pub first: u64,
    pub vals: Vec<u32>,
}

/// Cut `src`, one integer per line, into [`CountBatch`]es until `limit` lines
/// or `f` answers `false`.  Answers how many lines were read.  The file
/// carries no tx ids, so alignment rests on the line count alone.
pub fn read_counts<B: BufRead>(mut src: B, limit: u64, mut f: impl FnMut(Arc<CountBatch>) -> bool) -> io::Result<u64> {
    let mut line = Vec::with_capacity(64);
    let mut count = 0u64;
    loop {
        let mut b = CountBatch { first: count, vals: Vec::with_capacity(BATCH) };
        while b.vals.len() < BATCH && count < limit {
            line.clear();
            if src.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            let t = std::str::from_utf8(&line).ok().map(|s| s.trim()).unwrap_or("");
            let v: u32 = t.parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, format!("sets-counts line {}: not a count: {:?}", count + 1, t))
            })?;
            b.vals.push(v);
            count += 1;
        }
        if b.vals.is_empty() {
            return Ok(count);
        }
        let full = b.vals.len() == BATCH;
        if !f(Arc::new(b)) || !full {
            return Ok(count);
        }
    }
}

/// [`read_counts`] on a thread of its own, broadcasting to `to`: plain text,
/// or through `zstd -dc` when the path ends in `.zst`.  The thread ends at
/// the file's end; a consumer then finds the channel closed and stops
/// comparing, and the thread's own result says whether that was the end of
/// the file or an error.
pub fn spawn_counts(path: String, limit: u64, to: Vec<SyncSender<Arc<CountBatch>>>) -> JoinHandle<io::Result<u64>> {
    std::thread::Builder::new()
        .name("sets-counts".into())
        .spawn(move || {
            let send = |b: Arc<CountBatch>| {
                let mut any = false;
                for t in &to {
                    any |= t.send(b.clone()).is_ok();
                }
                any
            };
            let result = if path.ends_with(".zst") {
                let mut child = decompress(&path)?;
                let out = child.stdout.take().expect("piped");
                let r = read_counts(BufReader::with_capacity(1 << 22, out), limit, send);
                let _ = child.kill();
                let _ = child.wait();
                r
            } else {
                let f = std::fs::File::open(&path)?;
                read_counts(BufReader::with_capacity(1 << 22, f), limit, send)
            };
            result.map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))
        })
        .expect("spawn the sets-counts reader")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn records(txs: &[usize]) -> String {
        txs.iter()
            .enumerate()
            .map(|(i, tx)| format!("((1 {} {} 0 0 0 0) () ((7 1 0)))\n", i, tx))
            .collect()
    }

    /// T12: a reference with a wrong tx id on its second line fails, naming the
    /// record.
    #[test]
    fn a_misaligned_reference_names_the_record() {
        let recs = records(&[0, 1, 2]);
        let mut batches = Vec::new();
        read_batches(Cursor::new(recs.into_bytes()), u64::MAX, |b| {
            batches.push(b);
            true
        })
        .unwrap();
        let text = "0\t1\t1\t1\t1\t1\t1\t1\n7\t1\t1\t1\t1\t1\t1\t1\n2\t1\t1\t1\t1\t1\t1\t1\n";
        let mut refs = Vec::new();
        read_refs(Cursor::new(text.as_bytes()), RefKind::Laplace, 1, u64::MAX, |b| {
            refs.push(b);
            true
        })
        .unwrap();
        let err = check_alignment(&batches[0], &refs[0]).unwrap_err().to_string();
        assert!(err.contains("record 1"), "{}", err);
        assert!(err.contains("tx 7"), "{}", err);
    }

    #[test]
    fn a_short_reference_is_an_error() {
        let recs = records(&[0, 1, 2]);
        let mut batches = Vec::new();
        read_batches(Cursor::new(recs.into_bytes()), u64::MAX, |b| {
            batches.push(b);
            true
        })
        .unwrap();
        let mut refs = Vec::new();
        read_refs(Cursor::new("0\t1\t0\t1\n1\t2\t0\t1\n".as_bytes()), RefKind::Moments, 1, u64::MAX, |b| {
            refs.push(b);
            true
        })
        .unwrap();
        assert!(check_alignment(&batches[0], &refs[0]).is_err());
    }

    #[test]
    fn sampled_lines_parse_and_others_do_not() {
        let text = "0\t1:0\n1\t0.25:3,0.75:1\n2\t\n3\t0.5:2,0.5:1\n";
        let mut refs = Vec::new();
        read_refs(Cursor::new(text.as_bytes()), RefKind::Bands, 2, u64::MAX, |b| {
            refs.push(b);
            true
        })
        .unwrap();
        let r = &refs[0];
        assert_eq!(r.tx, vec![0, 1, 2, 3]);
        assert_eq!(r.vals[0], Some(RefVal::Bands(vec![(0, 1.0)])));
        assert_eq!(r.vals[1], None);
        assert_eq!(r.vals[2], Some(RefVal::Bands(vec![])));
        let text = "5\t10.5\t2.25\t3\n";
        let mut refs = Vec::new();
        read_refs(Cursor::new(text.as_bytes()), RefKind::Moments, 1, u64::MAX, |b| {
            refs.push(b);
            true
        })
        .unwrap();
        assert_eq!(refs[0].vals[0], Some(RefVal::Moments { mean: 10.5, sd: 2.25 }));
    }

    /// A reader's own error replaces the consumer's "a reference ended";
    /// a consumer's real error stands; a reader alone that failed is reported.
    #[test]
    fn the_reader_error_is_the_root_cause() {
        let bad = || Err(io::Error::new(io::ErrorKind::InvalidData, "laplace (x): fewer than 7 values"));
        let e = root_cause(Some(ref_closed(0)), vec![Ok(5), bad()]).unwrap();
        assert!(e.to_string().contains("fewer than 7"), "{}", e);
        let e = root_cause(Some(io::Error::new(io::ErrorKind::Other, "unknown tx")), vec![bad()]).unwrap();
        assert_eq!(e.to_string(), "unknown tx");
        let e = root_cause(Some(ref_closed(8192)), vec![Ok(10)]).unwrap();
        assert_eq!(e.to_string(), "a reference ended before record 8192");
        assert!(root_cause(None, vec![bad()]).is_some());
        assert!(root_cause(None, vec![Ok(1)]).is_none());
    }

    /// A missing file is reported as zstd's failure, not as an early end.
    #[test]
    fn a_missing_reference_is_an_error_of_its_reader() {
        let (tx, rx) = std::sync::mpsc::sync_channel(8);
        let h = spawn_ref("/nonexistent/sketch-test.zst".into(), RefKind::Laplace, 1, u64::MAX, vec![tx]);
        assert!(rx.recv().is_err());
        let e = h.join().unwrap().unwrap_err().to_string();
        assert!(e.contains("/nonexistent/sketch-test.zst") && e.contains("zstd"), "{}", e);
    }

    #[test]
    fn batches_split_on_the_same_boundaries() {
        let n = BATCH * 2 + 5;
        let recs = records(&(0..n).collect::<Vec<_>>());
        let mut sizes = Vec::new();
        let read = read_batches(Cursor::new(recs.into_bytes()), u64::MAX, |b| {
            sizes.push((b.first, b.len()));
            true
        })
        .unwrap();
        assert_eq!(read, n as u64);
        assert_eq!(sizes, vec![(0, BATCH), (BATCH as u64, BATCH), (2 * BATCH as u64, 5)]);
        let text: String = (0..n).map(|i| format!("{}\t0\t0\t1\n", i)).collect();
        let mut rs = Vec::new();
        read_refs(Cursor::new(text.into_bytes()), RefKind::Moments, 3, u64::MAX, |b| {
            rs.push((b.first, b.tx.len()));
            true
        })
        .unwrap();
        assert_eq!(rs, sizes);
        // A limit cuts both at the same record.
        let recs = records(&(0..n).collect::<Vec<_>>());
        let read = read_batches(Cursor::new(recs.into_bytes()), 100, |_| true).unwrap();
        assert_eq!(read, 100);
    }
}
