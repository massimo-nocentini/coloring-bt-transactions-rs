//! # `surface`: every colour of the chain averaged into one surface
//!
//! A colour is a distribution over the blocks its coins were minted in, and so
//! is its sketch.  Set side by side in the order of the chain, the colours make
//! a surface over two time axes: the block a transaction is in (when its coins
//! are spent) and the block they were minted in.  There are far too many
//! transactions to draw it one at a time, so this mode draws it as an average:
//! with `B_m` band `m` of `--band` blocks and `T_s` the transactions in the
//! blocks of band `s`, coinbases left out,
//!
//! ```text
//! P(s, m) = 1/|T_s| sum_{t in T_s} sum_{b in B_m} c_t(b) / sum_b c_t(b)
//! ```
//!
//! Each row `s` is a distribution over minting bands.  The surface is linear
//! in the colours, so over the varopt sketches it is an unbiased estimate of
//! the same average of the exact colours -- which the mode computes too, from
//! `colors.bands1024.zst`, so the two can be compared ([`crate::surface_report`]).
//!
//! ## Inputs
//!
//! Neither colour file says which block a record is in; the count stream of
//! `count` does (`--counts`, [`crate::kmv::CNT_MAGIC`], read in its binary
//! form and through `zstd -dc` when the path ends in `.zst`; only its `tx` and
//! `block` are read, [`crate::kmv::CntDecoder::next_rec`]).  Every record's tx
//! id is checked against it, so a colour file of another chain, or one that
//! skipped a record, fails at the first record that disagrees.
//!
//! - **Sketched** (no `--bands-ref`): stdin is a `SKETCH01` stream, the
//!   production `colors.sketch-varopt512.zst` after `zstd -dc --long=27`, read
//!   with [`crate::encode::Decoder`] on one thread.  A term at block `b` of a
//!   record in block `B` goes to column `b / band` of the minting surface and
//!   to column `(B - b) / band` of the age surface (age measured in blocks,
//!   then binned).
//! - **Exact** (`--bands-ref <zst>`): the exact colours binned in bands,
//!   `tx<TAB>w:band,...` per record, read through `zstd -dc` by this thread
//!   and parsed by `--threads` others ([`crate::feed::parse_line`]), since
//!   parsing a terabyte of floats is most of the work.  stdin is not read.
//!   The bands are the file's, so `--band`/`--bands` must be the ones it was
//!   binned with (745 and 1024 for the 2022 chain); the age is a difference
//!   of bands, `s - m`.  The parsed records are added in record order whatever
//!   thread parsed them, so the output is the same bytes at any thread count.
//!
//! The first record of each block is its coinbase, whose colour is its own
//! block; it says nothing about how coins move and is skipped (the surface of
//! §12 of the post).  A record whose terms sum to 0 is skipped and counted.
//!
//! The arithmetic is that of the two programs that first drew the surface
//! (`surface-aggb` and `surface-aggx` of the evidence folder): each term is
//! `f32` widened to `f64`, the record's mass summed in the order the record
//! lists its terms (ascending blocks for a sketch; descending bands, the
//! file's order, for the exact line), each share `w / mass` added to its cell.
//! A sketched run is bit for bit `surface-aggb`'s; an exact run is bit for bit
//! the single-threaded `surface-aggx`'s, and agrees with the 16-thread
//! `surface-aggxp` (which summed in whatever order its threads finished) to
//! rounding.
//!
//! ## Outputs
//!
//! - `<prefix>.mint.f64`: `bands x bands`, row-major little-endian `f64`, row
//!   = spending band, column = minting band; the sum of the shares (divide a
//!   row by its count for `P(s, .)`).  numpy: `np.fromfile(p).reshape(n, n)`.
//! - `<prefix>.age.f64`: the same with column = age band (the last column
//!   holds anything older).
//! - `<prefix>.rows.f64`: the number of records added to each row.
//! - `<prefix>.surface.tsv`: one line of counters -- records read, coinbases
//!   skipped, records kept, empty records, terms at a block later than their
//!   record's (`late_terms`, which must be 0: a colour cannot hold coins
//!   minted after it, so a nonzero count says the two files do not line up),
//!   the last block, and `complete` (1 when both streams were read to their
//!   verified ends, 0 for a `--limit` prefix).
//!
//! The three matrices are the files `surface-aggb` and `surface-aggxp` wrote,
//! so `surface-report` and the plotting scripts read either.

use crate::encode::{Decoded, Decoder};
use crate::feed::{self, RefKind, RefVal};
use crate::kmv::{CntDecoder, CntRec};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Write};
use std::process::Child;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct SurfaceOpts {
    pub counts: String,
    pub bands_ref: Option<String>,
    pub out: String,
    pub width: u32,
    pub bands: usize,
    pub limit: u64,
    pub threads: usize,
    pub stats: bool,
}

/// The header of `<prefix>.surface.tsv`.
pub const TALLY_HEADER: &str = "source\tband\tbands\trecords\tcoinbases\tkept\tempty\tlate_terms\tlast_block\tcomplete";

/// The three sums of a surface: `mint` and `age` are `bands x bands`,
/// row-major, row = spending band.
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    pub width: u32,
    pub bands: usize,
    pub mint: Vec<f64>,
    pub age: Vec<f64>,
    pub rows: Vec<f64>,
}

/// What a run read and kept.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    pub records: u64,
    pub coinbases: u64,
    pub kept: u64,
    pub empty: u64,
    pub late: u64,
    pub last_block: Option<u32>,
    pub complete: bool,
}

fn invalid(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

impl Surface {
    pub fn new(width: u32, bands: usize) -> Surface {
        Surface { width, bands, mint: vec![0.0; bands * bands], age: vec![0.0; bands * bands], rows: vec![0.0; bands] }
    }

    fn row_of(&self, block: u32) -> io::Result<usize> {
        let row = (block / self.width) as usize;
        if row >= self.bands {
            return Err(invalid(format!(
                "block {} is in band {}, past the {} bands of {} blocks",
                block, row, self.bands, self.width
            )));
        }
        Ok(row)
    }

    /// One sketch of a record in `block`: `p` its terms, ascending blocks.
    /// The mass is summed in that order, as `surface-aggb` did.
    pub fn add_sketch(&mut self, block: u32, p: &[(u32, f32)], t: &mut Tally) -> io::Result<()> {
        let mut s = 0f64;
        for &(_, w) in p {
            s += w as f64;
        }
        // Not `s <= 0` alone: a NaN mass is empty too.
        if s.is_nan() || s <= 0.0 {
            t.empty += 1;
            return Ok(());
        }
        let row = self.row_of(block)?;
        let n = self.bands;
        if let Some(&(b, _)) = p.iter().find(|x| (x.0 / self.width) as usize >= n) {
            return Err(invalid(format!("a term at block {} is past the {} bands of {} blocks", b, n, self.width)));
        }
        self.rows[row] += 1.0;
        t.kept += 1;
        for &(b, w) in p {
            if b > block {
                t.late += 1;
            }
            let share = w as f64 / s;
            self.mint[row * n + (b / self.width) as usize] += share;
            let a = ((block.saturating_sub(b) / self.width) as usize).min(n - 1);
            self.age[row * n + a] += share;
        }
        Ok(())
    }

    /// One exact record of spending band `row`, its terms already divided
    /// by their mass ([`bands_terms`]).
    pub fn add_terms(&mut self, row: usize, terms: &[(u16, f64)], t: &mut Tally) {
        let n = self.bands;
        self.rows[row] += 1.0;
        t.kept += 1;
        for &(band, share) in terms {
            let band = band as usize;
            if band > row {
                t.late += 1;
            }
            self.mint[row * n + band] += share;
            self.age[row * n + row.saturating_sub(band).min(n - 1)] += share;
        }
    }

    /// Write the three matrices under `prefix`.
    pub fn write(&self, prefix: &str) -> io::Result<()> {
        for (name, v) in [("mint", &self.mint), ("age", &self.age), ("rows", &self.rows)] {
            let path = format!("{}.{}.f64", prefix, name);
            let mut w = BufWriter::with_capacity(1 << 20, File::create(&path)?);
            for x in v.iter() {
                w.write_all(&x.to_le_bytes())?;
            }
            w.flush()?;
        }
        Ok(())
    }

    /// Read the matrices under `prefix`; the number of bands is the length of
    /// `rows.f64`, and the two matrices must be its square.
    pub fn read(prefix: &str, width: u32) -> io::Result<Surface> {
        let load = |name: &str| -> io::Result<Vec<f64>> {
            let path = format!("{}.{}.f64", prefix, name);
            let bytes = std::fs::read(&path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
            if bytes.len() % 8 != 0 {
                return Err(invalid(format!("{}: {} bytes is not a whole number of f64", path, bytes.len())));
            }
            Ok(bytes.as_chunks::<8>().0.iter().map(|c| f64::from_le_bytes(*c)).collect())
        };
        let rows = load("rows")?;
        let n = rows.len();
        let mint = load("mint")?;
        let age = load("age")?;
        if mint.len() != n * n || age.len() != n * n {
            return Err(invalid(format!(
                "{}: rows.f64 has {} bands but mint.f64 and age.f64 hold {} and {} cells, not {}",
                prefix,
                n,
                mint.len(),
                age.len(),
                n * n
            )));
        }
        Ok(Surface { width, bands: n, mint, age, rows })
    }
}

/// The terms of one exact line (ascending bands, as
/// [`crate::feed::parse_line`] answers them) divided by their mass, which is
/// summed in the file's order, descending bands, as `surface-aggx` did.
/// `false` when the mass is not positive.
pub fn bands_terms(v: &[(u16, f32)], out: &mut Vec<(u16, f64)>) -> bool {
    out.clear();
    let mut s = 0f64;
    for &(_, w) in v.iter().rev() {
        s += w as f64;
    }
    if s.is_nan() || s <= 0.0 {
        return false;
    }
    out.extend(v.iter().map(|&(b, w)| (b, w as f64 / s)));
    true
}

/// Which records are coinbases: the first of each block.
#[derive(Default)]
pub struct Coinbases {
    prev: Option<u32>,
}

impl Coinbases {
    pub fn first(&mut self, block: u32) -> bool {
        let first = self.prev != Some(block);
        self.prev = Some(block);
        first
    }
}

/// A file, or the stdout of `zstd -dc` on it when it ends in `.zst`.
struct Source {
    path: String,
    child: Option<Child>,
}

fn open(path: &str) -> io::Result<(Box<dyn Read + Send>, Source)> {
    if path.ends_with(".zst") {
        let mut child = feed::decompress(path)?;
        let out = child.stdout.take().expect("piped");
        Ok((Box::new(out), Source { path: path.to_string(), child: Some(child) }))
    } else {
        let f = File::open(path).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
        Ok((Box::new(f), Source { path: path.to_string(), child: None }))
    }
}

impl Source {
    /// Reap the child: killed when the run stopped before the end (a
    /// prefix), and its exit status checked when it did not.  The reader must
    /// have been dropped already.
    fn close(self, finished: bool) -> io::Result<()> {
        if let Some(mut c) = self.child {
            if !finished {
                let _ = c.kill();
                let _ = c.wait();
            } else {
                let st = c.wait()?;
                if !st.success() {
                    return Err(io::Error::other(format!("zstd -dc {} exited with {}", self.path, st)));
                }
            }
        }
        Ok(())
    }
}

type CountReader = CntDecoder<BufReader<Box<dyn Read + Send>>>;

fn open_counts(path: &str) -> io::Result<(CountReader, Source)> {
    let (r, src) = open(path)?;
    let mut r = BufReader::with_capacity(1 << 22, r);
    let mut magic = [0u8; 8];
    r.read_exact(&mut magic).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
    let dec = CntDecoder::new(r, magic, None).map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))?;
    Ok((dec, src))
}

/// The next record of the count stream, or `None` at its verified end.
fn next_count<R: Read>(cnt: &mut CntDecoder<R>, path: &str) -> io::Result<Option<CntRec>> {
    cnt.next_rec().map_err(|e| io::Error::new(e.kind(), format!("{}: {}", path, e)))
}

fn misaligned(rec: u64, what: &str, tx: u64, count_tx: u64) -> io::Error {
    invalid(format!("record {}: the {} names tx {} but the count stream tx {}", rec, what, tx, count_tx))
}

fn progress(stats: bool, records: u64, t0: Instant) {
    if stats && records.is_multiple_of(50_000_000) {
        eprintln!("sketch surface: {} records, {:.0} s", records, t0.elapsed().as_secs_f64());
    }
}

/// `surface` without `--bands-ref`: the sketches of `input`.
pub fn run_sketched(opts: &SurfaceOpts, input: Box<dyn Read + Send>) -> io::Result<(Surface, Tally)> {
    let t0 = Instant::now();
    let (mut cnt, src) = open_counts(&opts.counts)?;
    let mut dec = Decoder::new(BufReader::with_capacity(1 << 22, input))?;
    let mut d = Decoded::default();
    let mut sf = Surface::new(opts.width, opts.bands);
    let mut t = Tally::default();
    let mut cb = Coinbases::default();
    while t.records < opts.limit {
        let r = match next_count(&mut cnt, &opts.counts)? {
            Some(r) => r,
            None => {
                if dec.next(&mut d)? {
                    return Err(invalid(format!("the sketch stream holds more than the {} records of the count stream", t.records)));
                }
                t.complete = true;
                break;
            }
        };
        if !dec.next(&mut d)? {
            return Err(invalid(format!("the sketch stream ended at record {}, before the count stream", t.records)));
        }
        if d.tx != r.tx {
            return Err(misaligned(t.records, "sketch", d.tx, r.tx));
        }
        t.records += 1;
        t.last_block = Some(r.block);
        if cb.first(r.block) {
            t.coinbases += 1;
            continue;
        }
        sf.add_sketch(r.block, &d.p, &mut t)?;
        progress(opts.stats, t.records, t0);
    }
    drop(cnt);
    src.close(t.complete)?;
    Ok((sf, t))
}

/// Records handed to a parser thread: whole lines, with each one's record
/// index and spending band.
struct LineBatch {
    seq: u64,
    recs: Vec<u64>,
    rows: Vec<u32>,
    text: Vec<u8>,
    ends: Vec<usize>,
}

/// What a parser thread hands back: each kept record's band and its
/// normalised terms.
struct Parsed {
    seq: u64,
    rows: Vec<u32>,
    ends: Vec<usize>,
    terms: Vec<(u16, f64)>,
    empty: u64,
}

const LINE_BATCH_BYTES: usize = 8 << 20;

fn parse_batch(b: LineBatch, bands: usize) -> io::Result<Parsed> {
    let mut p = Parsed { seq: b.seq, rows: Vec::with_capacity(b.rows.len()), ends: Vec::new(), terms: Vec::new(), empty: 0 };
    let mut one = Vec::new();
    let mut start = 0;
    for (i, &end) in b.ends.iter().enumerate() {
        let line = &b.text[start..end];
        start = end;
        let rec = b.recs[i];
        let v = match feed::parse_line(RefKind::Bands, rec, line, true)?.1 {
            Some(RefVal::Bands(v)) => v,
            _ => unreachable!("a sampled bands line parses to bands"),
        };
        if let Some(&(band, _)) = v.iter().find(|x| x.0 as usize >= bands) {
            return Err(invalid(format!("record {}: band {} is past the {} bands asked for", rec, band, bands)));
        }
        if !bands_terms(&v, &mut one) {
            p.empty += 1;
            continue;
        }
        p.rows.push(b.rows[i]);
        p.terms.extend_from_slice(&one);
        p.ends.push(p.terms.len());
    }
    Ok(p)
}

/// `surface --bands-ref`: the exact binned colours of the reference file.
pub fn run_exact(opts: &SurfaceOpts) -> io::Result<(Surface, Tally)> {
    let t0 = Instant::now();
    let path = opts.bands_ref.clone().expect("run_exact wants --bands-ref");
    let (mut cnt, cnt_src) = open_counts(&opts.counts)?;
    let (text, text_src) = open(&path)?;
    let mut text = BufReader::with_capacity(1 << 24, text);
    let threads = opts.threads.max(1);
    let bands = opts.bands;
    let failed = Arc::new(AtomicBool::new(false));

    let (to_parse, parse_rx) = sync_channel::<LineBatch>(2 * threads);
    let parse_rx: Arc<Mutex<Receiver<LineBatch>>> = Arc::new(Mutex::new(parse_rx));
    let (to_add, add_rx) = sync_channel::<Parsed>(2 * threads);
    let mut parsers = Vec::new();
    for i in 0..threads {
        let rx = parse_rx.clone();
        let tx = to_add.clone();
        let failed = failed.clone();
        parsers.push(
            std::thread::Builder::new()
                .name(format!("surface-parse-{}", i))
                .spawn(move || -> io::Result<()> {
                    loop {
                        let b = match rx.lock().unwrap().recv() {
                            Ok(b) => b,
                            Err(_) => return Ok(()),
                        };
                        match parse_batch(b, bands) {
                            Ok(p) => {
                                if tx.send(p).is_err() {
                                    return Ok(());
                                }
                            }
                            Err(e) => {
                                failed.store(true, Ordering::Relaxed);
                                return Err(e);
                            }
                        }
                    }
                })
                .expect("spawn a surface parser"),
        );
    }
    // Only the parsers hold the receiver: if every one of them fails, a send
    // fails instead of waiting for a reader that is gone.
    drop(parse_rx);
    drop(to_add);
    let (width, n) = (opts.width, opts.bands);
    let adder = std::thread::Builder::new()
        .name("surface-add".into())
        .spawn(move || -> (Surface, Tally, u64) {
            // Batches come back in any order and are added in record order.
            let mut sf = Surface::new(width, n);
            let mut t = Tally::default();
            let mut waiting: BTreeMap<u64, Parsed> = BTreeMap::new();
            let mut next = 0u64;
            for p in add_rx {
                waiting.insert(p.seq, p);
                while let Some(p) = waiting.remove(&next) {
                    let mut start = 0;
                    for (i, &end) in p.ends.iter().enumerate() {
                        sf.add_terms(p.rows[i] as usize, &p.terms[start..end], &mut t);
                        start = end;
                    }
                    t.empty += p.empty;
                    next += 1;
                }
            }
            (sf, t, next)
        })
        .expect("spawn the surface adder");

    let mut t = Tally::default();
    let mut cb = Coinbases::default();
    let mut seq = 0u64;
    let new_batch = |seq: u64| LineBatch { seq, recs: Vec::new(), rows: Vec::new(), text: Vec::with_capacity(LINE_BATCH_BYTES + (1 << 16)), ends: Vec::new() };
    let mut batch = new_batch(seq);
    let mut line = Vec::with_capacity(1 << 16);
    let mut result: io::Result<()> = Ok(());
    while t.records < opts.limit && !failed.load(Ordering::Relaxed) {
        let r = match next_count(&mut cnt, &opts.counts) {
            Ok(Some(r)) => r,
            Ok(None) => {
                line.clear();
                match text.read_until(b'\n', &mut line) {
                    Ok(0) => t.complete = true,
                    Ok(_) => {
                        result = Err(invalid(format!("{} holds more than the {} records of the count stream", path, t.records)))
                    }
                    Err(e) => result = Err(e),
                }
                break;
            }
            Err(e) => {
                result = Err(e);
                break;
            }
        };
        line.clear();
        match text.read_until(b'\n', &mut line) {
            Ok(0) => {
                result = Err(invalid(format!("{} ended at record {}, before the count stream", path, t.records)));
                break;
            }
            Ok(_) => {}
            Err(e) => {
                result = Err(e);
                break;
            }
        }
        let tx = match feed::parse_line(RefKind::Bands, t.records, &line, false) {
            Ok((tx, _)) => tx,
            Err(e) => {
                result = Err(e);
                break;
            }
        };
        if tx != r.tx {
            result = Err(misaligned(t.records, "bands reference", tx, r.tx));
            break;
        }
        let rec = t.records;
        t.records += 1;
        t.last_block = Some(r.block);
        if cb.first(r.block) {
            t.coinbases += 1;
            continue;
        }
        let row = (r.block / opts.width) as usize;
        if row >= opts.bands {
            result = Err(invalid(format!("block {} is in band {}, past the {} bands of {} blocks", r.block, row, opts.bands, opts.width)));
            break;
        }
        batch.recs.push(rec);
        batch.rows.push(row as u32);
        batch.text.extend_from_slice(&line);
        batch.ends.push(batch.text.len());
        if batch.text.len() >= LINE_BATCH_BYTES {
            seq += 1;
            if to_parse.send(std::mem::replace(&mut batch, new_batch(seq))).is_err() {
                break;
            }
        }
        progress(opts.stats, t.records, t0);
    }
    if result.is_ok() && !batch.ends.is_empty() {
        seq += 1;
        let _ = to_parse.send(batch);
    }
    drop(to_parse);
    let mut first_err = None;
    for h in parsers {
        if let Err(e) = h.join().expect("a surface parser panicked") {
            first_err.get_or_insert(e);
        }
    }
    let (sf, added, batches) = adder.join().expect("the surface adder panicked");
    drop(cnt);
    drop(text);
    let closed = cnt_src.close(t.complete).and(text_src.close(t.complete));
    if let Some(e) = first_err {
        return Err(e);
    }
    result?;
    closed?;
    if batches != seq {
        return Err(io::Error::other(format!("{} of {} batches were added", batches, seq)));
    }
    t.kept = added.kept;
    t.empty = added.empty;
    t.late = added.late;
    Ok((sf, t))
}

/// `<prefix>.surface.tsv`.
pub fn write_tally(prefix: &str, source: &str, sf: &Surface, t: &Tally) -> io::Result<()> {
    let mut w = BufWriter::new(File::create(format!("{}.surface.tsv", prefix))?);
    writeln!(w, "{}", TALLY_HEADER)?;
    writeln!(
        w,
        "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
        source,
        sf.width,
        sf.bands,
        t.records,
        t.coinbases,
        t.kept,
        t.empty,
        t.late,
        t.last_block.map_or("NA".to_string(), |b| b.to_string()),
        t.complete as u8
    )?;
    w.flush()
}

/// The `surface` mode.
pub fn run(opts: SurfaceOpts, input: Box<dyn Read + Send>) -> io::Result<()> {
    let t0 = Instant::now();
    let (sf, t, source) = match &opts.bands_ref {
        None => {
            let (sf, t) = run_sketched(&opts, input)?;
            (sf, t, "sketched")
        }
        Some(_) => {
            let (sf, t) = run_exact(&opts)?;
            (sf, t, "exact")
        }
    };
    sf.write(&opts.out)?;
    write_tally(&opts.out, source, &sf, &t)?;
    eprintln!(
        "sketch surface: {} records read, {} coinbases skipped, {} kept, {} empty, {} terms minted after their spending block; last block {}{}",
        t.records,
        t.coinbases,
        t.kept,
        t.empty,
        t.late,
        t.last_block.map_or("NA".to_string(), |b| b.to_string()),
        if opts.stats { format!("; {:.1} s", t0.elapsed().as_secs_f64()) } else { String::new() }
    );
    if t.late > 0 {
        return Err(invalid(format!(
            "{} terms sit at a block later than their record's: the colour file and the count stream do not line up",
            t.late
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::Config;
    use crate::encode::{header, Encoder, Meta};
    use crate::kmv::{cnt_header, CntEncoder, KmvSpec, Support};

    fn scratch(name: &str) -> String {
        let d = std::env::temp_dir().join(format!("sketch-surface-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d.to_str().unwrap().to_string()
    }

    /// A tiny chain: per record, its tx, block and colour over blocks.
    fn chain() -> Vec<(u64, u32, Vec<(u32, f32)>)> {
        vec![
            (0, 0, vec![(0, 1.0)]),
            (1, 1, vec![(1, 1.0)]),
            (2, 1, vec![(0, 0.75), (1, 0.25)]),
            (3, 2, vec![(2, 1.0)]),
            (4, 2, vec![(0, 0.5), (1, 0.25), (2, 0.25)]),
            (5, 2, vec![(1, 0.5), (2, 0.5)]),
            (6, 3, vec![(3, 1.0)]),
            // Mass not one, as a sketch's may be within rounding: divided by it.
            (7, 3, vec![(0, 0.1), (3, 0.1)]),
            (8, 3, vec![]),
            (9, 4, vec![(4, 1.0)]),
            (10, 4, vec![(0, 0.25), (2, 0.25), (3, 0.25), (4, 0.25)]),
        ]
    }

    fn write_counts(path: &str, recs: &[(u64, u32, Vec<(u32, f32)>)]) {
        let spec = KmvSpec { k: 4, seed: 1, support: Support::Sets, bits: 20 };
        let mut out = cnt_header(&spec).to_vec();
        let mut e = CntEncoder::default();
        for (tx, block, _) in recs {
            e.record(&mut out, &CntRec { tx: *tx, block: *block, m: 1, rk: 0 }).unwrap();
        }
        e.trailer(&mut out).unwrap();
        std::fs::write(path, out).unwrap();
    }

    fn sketch_stream(recs: &[(u64, u32, Vec<(u32, f32)>)]) -> Vec<u8> {
        let mut out = header(&Config::parse("varopt:4").unwrap());
        let mut e = Encoder::default();
        for (tx, _, p) in recs {
            e.record(&mut out, *tx, p, Meta::default());
        }
        e.trailer(&mut out);
        out
    }

    /// The exact file: bands of `width` blocks, descending, `tx<TAB>w:band,...`.
    fn bands_text(recs: &[(u64, u32, Vec<(u32, f32)>)], width: u32) -> String {
        let mut s = String::new();
        for (tx, _, p) in recs {
            let mut by: BTreeMap<u32, f32> = BTreeMap::new();
            for &(b, w) in p {
                *by.entry(b / width).or_default() += w;
            }
            let terms: Vec<String> = by.iter().rev().map(|(b, w)| format!("{}:{}", w, b)).collect();
            s.push_str(&format!("{}\t{}\n", tx, terms.join(",")));
        }
        s
    }

    fn opts(dir: &str, width: u32, bands: usize, bands_ref: Option<String>, threads: usize) -> SurfaceOpts {
        SurfaceOpts {
            counts: format!("{}/counts.bin", dir),
            bands_ref,
            out: format!("{}/s", dir),
            width,
            bands,
            limit: u64::MAX,
            threads,
            stats: false,
        }
    }

    /// The sketched surface by hand: coinbases skipped, each record over its
    /// mass, age in blocks then binned; and the stream read to both trailers.
    #[test]
    fn the_sketched_surface_is_the_average_of_the_rows() {
        let dir = scratch("sketched");
        let recs = chain();
        write_counts(&format!("{}/counts.bin", dir), &recs);
        let o = opts(&dir, 2, 3, None, 1);
        let (sf, t) = run_sketched(&o, Box::new(io::Cursor::new(sketch_stream(&recs)))).unwrap();
        assert_eq!(
            t,
            Tally { records: 11, coinbases: 5, kept: 5, empty: 1, late: 0, last_block: Some(4), complete: true }
        );
        // Band 0 (blocks 0, 1): record 2.  Band 1 (blocks 2, 3): records 4, 5, 7.  Band 2: record 10.
        assert_eq!(sf.rows, vec![1.0, 3.0, 1.0]);
        let m = |r: usize, c: usize| sf.mint[r * 3 + c];
        let a = |r: usize, c: usize| sf.age[r * 3 + c];
        assert_eq!((m(0, 0), m(0, 1), m(0, 2)), (1.0, 0.0, 0.0));
        // Row 1: 4 -> 0.75 band 0, 0.25 band 1; 5 -> 0.5, 0.5; 7 -> 0.5, 0.5.
        assert!((m(1, 0) - 1.75).abs() < 1e-12 && (m(1, 1) - 1.25).abs() < 1e-12);
        assert_eq!(m(2, 0) + m(2, 1) + m(2, 2), 1.0);
        assert_eq!((m(2, 0), m(2, 1), m(2, 2)), (0.25, 0.5, 0.25));
        // Ages in blocks: record 4 in block 2: 0 -> 2 (band 1), 1 -> 1 (band 0), 2 -> 0.
        // Record 7 in block 3: 0 -> 3 (band 1), 3 -> 0.  Record 5: 1 -> 1, 2 -> 0.
        assert!((a(1, 0) - (0.5 + 1.0 + 0.5)).abs() < 1e-12 && (a(1, 1) - (0.5 + 0.5)).abs() < 1e-12);
        // Record 10 in block 4: ages 4, 2, 1, 0 -> bands 2, 1, 0, 0.
        assert_eq!((a(2, 0), a(2, 1), a(2, 2)), (0.5, 0.25, 0.25));
        for r in 0..3 {
            let s: f64 = (0..3).map(|c| sf.mint[r * 3 + c]).sum();
            assert!((s - sf.rows[r]).abs() < 1e-12);
        }

        // A prefix stops early and is not complete; the rows it saw agree.
        let mut o2 = opts(&dir, 2, 3, None, 1);
        o2.limit = 6;
        let (sf2, t2) = run_sketched(&o2, Box::new(io::Cursor::new(sketch_stream(&recs)))).unwrap();
        assert!(!t2.complete);
        assert_eq!(t2.records, 6);
        assert_eq!(sf2.rows, vec![1.0, 2.0, 0.0]);
    }

    #[test]
    fn a_misaligned_sketch_stream_names_the_record() {
        let dir = scratch("misaligned");
        let recs = chain();
        write_counts(&format!("{}/counts.bin", dir), &recs);
        let mut other = recs.clone();
        other[4].0 = 40;
        let e = run_sketched(&opts(&dir, 2, 3, None, 1), Box::new(io::Cursor::new(sketch_stream(&other)))).unwrap_err();
        assert!(e.to_string().contains("record 4"), "{}", e);
        // A short sketch stream is an error too.
        let e = run_sketched(&opts(&dir, 2, 3, None, 1), Box::new(io::Cursor::new(sketch_stream(&recs[..7])))).unwrap_err();
        assert!(e.to_string().contains("ended at record 7"), "{}", e);
        // And a band past the surface.
        let e = run_sketched(&opts(&dir, 1, 3, None, 1), Box::new(io::Cursor::new(sketch_stream(&recs)))).unwrap_err();
        assert!(e.to_string().contains("past the 3 bands"), "{}", e);
    }

    /// The exact side over binned colours is the sketched side over the same
    /// colours binned, ages as band differences; and the bytes do not depend
    /// on the number of parser threads.
    #[test]
    fn the_exact_surface_is_the_same_at_any_thread_count() {
        let dir = scratch("exact");
        let recs = chain();
        write_counts(&format!("{}/counts.bin", dir), &recs);
        let bpath = format!("{}/bands.txt", dir);
        std::fs::write(&bpath, bands_text(&recs, 2)).unwrap();
        let (one, t1) = run_exact(&opts(&dir, 2, 3, Some(bpath.clone()), 1)).unwrap();
        let (many, t4) = run_exact(&opts(&dir, 2, 3, Some(bpath.clone()), 4)).unwrap();
        assert_eq!(one, many);
        assert_eq!(t1, t4);
        assert_eq!(t1, Tally { records: 11, coinbases: 5, kept: 5, empty: 1, late: 0, last_block: Some(4), complete: true });
        let (sk, _) = run_sketched(&opts(&dir, 2, 3, None, 1), Box::new(io::Cursor::new(sketch_stream(&recs)))).unwrap();
        assert_eq!(one.rows, sk.rows);
        for (x, y) in one.mint.iter().zip(&sk.mint) {
            assert!((x - y).abs() < 1e-6, "{} {}", x, y);
        }
        // Row 1 by band differences: record 4 -> 0.75 at 1, 0.25 at 0; 5 and 7 -> 0.5, 0.5.
        assert!((one.age[3] - 1.25).abs() < 1e-6 && (one.age[4] - 1.75).abs() < 1e-6);

        // A misaligned line is caught, by this thread or a parser.
        let mut other = recs.clone();
        other[9].0 = 90;
        std::fs::write(&bpath, bands_text(&other, 2)).unwrap();
        let e = run_exact(&opts(&dir, 2, 3, Some(bpath.clone()), 2)).unwrap_err();
        assert!(e.to_string().contains("record 9"), "{}", e);
        std::fs::write(&bpath, bands_text(&recs[..10], 2)).unwrap();
        let e = run_exact(&opts(&dir, 2, 3, Some(bpath), 2)).unwrap_err();
        assert!(e.to_string().contains("ended at record 10"), "{}", e);
    }

    #[test]
    fn the_mass_is_summed_in_the_files_order() {
        let mut out = Vec::new();
        assert!(!bands_terms(&[], &mut out));
        assert!(!bands_terms(&[(0, 0.0)], &mut out));
        let v = [(0u16, 1e-8f32), (1, 0.3), (2, 0.7)];
        assert!(bands_terms(&v, &mut out));
        let s = 0.7f32 as f64 + 0.3f32 as f64 + 1e-8f32 as f64;
        assert_eq!(out, vec![(0, 1e-8f32 as f64 / s), (1, 0.3f32 as f64 / s), (2, 0.7f32 as f64 / s)]);
    }

    #[test]
    fn surfaces_round_trip_through_their_files() {
        let dir = scratch("files");
        let mut sf = Surface::new(745, 2);
        sf.mint = vec![1.0, 0.0, 0.25, 0.75];
        sf.age = vec![1.0, 0.0, 0.75, 0.25];
        sf.rows = vec![1.0, 1.0];
        let p = format!("{}/x", dir);
        sf.write(&p).unwrap();
        assert_eq!(Surface::read(&p, 745).unwrap(), sf);
        std::fs::write(format!("{}.rows.f64", p), [0u8; 24]).unwrap();
        assert!(Surface::read(&p, 745).is_err());
    }

    #[test]
    fn the_first_record_of_a_block_is_its_coinbase() {
        let mut c = Coinbases::default();
        let flags: Vec<bool> = [0u32, 1, 1, 1, 2, 5, 5].iter().map(|&b| c.first(b)).collect();
        assert_eq!(flags, vec![true, true, false, false, true, true, false]);
    }
}
