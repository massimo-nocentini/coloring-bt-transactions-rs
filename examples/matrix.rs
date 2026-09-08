//! # Writing the matrix down, so the next solve is a sweep and not a re-parse
//!
//! The article's last bullet: *"a binary CSR of A, about 19.5 GB, would make
//! every future solve a sweep over it rather than a re-parse of 150 GB."*
//! [`matrix`] is the format that keeps that promise at 10.6 GB and exactly,
//! and this is the tool that writes it, reads it back against the records it
//! came from, and says what one on disk claims to be.
//!
//! ```text
//! matrix [--compact] <file>     < records   the compact exact form (default)
//! matrix --structure <file>     < records   compact, no amounts
//! matrix --csr <basename>       < records   the article's u32/u32/f32 CSR
//! matrix --check <file>         < records   re-parse and verify field for field
//! matrix --verify <file>                    sweep the matrix against its own header
//! matrix --stats <file>                     what a written file says about itself
//! ```
//!
//! ## Why `--csr` is still here
//!
//! Because the article costed it, and because writing it is how its lossiness
//! stops being an argument and becomes a measurement.  The run prints what the
//! `f32` values cost: how many weights round-trip, the worst relative error,
//! and how far a row's weights drift from summing to one before and after the
//! narrowing.  Those are the numbers in [`matrix`]'s doc comment, and they were
//! produced here rather than reasoned about.
//!
//! It is three raw arrays and no header — `<basename>.rowptr`, `<basename>.col`,
//! `<basename>.val` — because the point is to be exactly the thing that was
//! costed: `4(n+1) + 4z + 4z` with `n` transactions and `z` nonzeros, which over
//! the 2022 chain is 19,473,632,380 bytes against this format's 10.6 GB.  The
//! rows are indexed by transaction, not by record, which is where the two
//! BIP-30 duplicate coinbases go: a duplicate names a transaction whose row is
//! already closed, and since both the original and the duplicate are coinbases
//! with no inputs the row is empty either way and no nonzero is lost.  The
//! writer asserts that rather than assuming it.
//!
//! ## What `--check` checks
//!
//! Everything, from both ends.  It reads the matrix and re-parses the records
//! side by side and compares block, transaction, output count, every column in
//! file order, and every amount that is stored; it recomputes the fold's weight
//! from each side and compares those too, which is what tests the claim that a
//! one-input row needs no amount; and it hashes both sides with
//! [`matrix::hash_record`] and holds the two against the header's own.
//!
//! The block ids are the interesting half.  A FOLDMAT row carries a block only
//! when it has no inputs, on the argument that every other record is in the
//! block the last coinbase opened — so `--check` comparing 20,000,000
//! reconstructed block ids against the file's own and finding no difference,
//! which is what it printed on the 20M-record prefix in 10.4s, is the evidence
//! for that rather than the argument for it.
//!
//! ## What `--verify` checks, and why it is not `--check`
//!
//! `--check` needs the records.  Over the whole chain that is 150 GB and twelve
//! and a half minutes of reading before it can say anything, and re-reading them
//! is the cost [`matrix`] exists to remove — so in practice a matrix gets
//! checked once, on the machine that wrote it, and then travels.  Everything
//! that can happen to it afterwards happens to it alone.
//!
//! Measured, by flipping every bit of the `ROWS` of a 20,000-record matrix one
//! at a time and reading the result back: **102,220 of 347,416 flips are
//! accepted**.  And measured again through the driver itself, since a reader
//! accepting a file is not yet a wrong answer — every bit of a 2,000-record
//! matrix flipped in turn and folded with `--matrix ... --sum`: **9,975 of
//! 34,736 flips exit 0 with an empty standard error, and 1,438 of those print a
//! different answer**.  The structural checks a fold gets for free do not see
//! them, and nothing else looked: the header has always carried a content hash
//! and no reader ever read it.
//!
//! `--verify` sweeps the matrix and nothing else.  It recomputes
//! [`matrix::hash_record`] over the rows and holds it against the header, and
//! reading to the end runs every tail check with it — the nonzero, transaction
//! and block counts, `TXFIX` exhausted, and no trailing bytes.  It refuses all
//! 102,220 of those flips.  It reads no records and refuses to be handed any.
//!
//! Over the whole chain that is **113.7s cold and 110s warm, at 5.4 MB
//! resident**, for 778,613,440 rows and 2,044,897,328 nonzeros out of a
//! 10,601,433,514-byte file — against `--check`'s 150 GB and twelve and a half
//! minutes of reading before it starts.  [`matrix::Reader::verify`] says what
//! the hashing half of that costs and why a fold does not pay it.

// The driver's own reader and the format module, included by path so that this
// tool and the fold agree about what a record is by construction.  Most of what
// `sexp` and `simd` bring is unused here, which is what the allow is for.
#[allow(dead_code)]
#[path = "../src/sexp.rs"]
mod sexp;
#[allow(dead_code)]
#[path = "../src/simd.rs"]
mod simd;
#[allow(dead_code)]
#[path = "../src/matrix.rs"]
mod matrix;

use std::io::{self, BufWriter, IsTerminal, Seek, Write};
use std::process::ExitCode;

/// What the run is for.  Exactly one of them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Compact,
    Structure,
    Csr,
    Check,
    Verify,
    Stats,
}

impl Mode {
    fn flag(self) -> &'static str {
        match self {
            Mode::Compact => "--compact",
            Mode::Structure => "--structure",
            Mode::Csr => "--csr",
            Mode::Check => "--check",
            Mode::Verify => "--verify",
            Mode::Stats => "--stats",
        }
    }

    /// Whether this mode reads the records on stdin.
    fn reads_records(self) -> bool {
        !matches!(self, Mode::Stats | Mode::Verify)
    }
}

const USAGE: &str = "usage: matrix [--compact] <file>   < records\n       \
                     matrix --structure <file>   < records\n       \
                     matrix --csr <basename>     < records\n       \
                     matrix --check <file>       < records\n       \
                     matrix --verify <file>\n       \
                     matrix --stats <file>";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("matrix: {}", message);
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut mode: Option<Mode> = None;
    let mut structure = false;
    let mut csr = false;
    let mut path: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let arg = args[i].as_str();
        let named = match arg {
            "--compact" => Some(Mode::Compact),
            "--structure" => Some(Mode::Structure),
            "--csr" => Some(Mode::Csr),
            "--check" => Some(Mode::Check),
            "--verify" => Some(Mode::Verify),
            "--stats" => Some(Mode::Stats),
            _ => None,
        };
        match named {
            Some(m) => {
                // `--csr --structure` is the one pair worth naming, because
                // each is a decision about the weights and they are opposite
                // decisions.  Everything else is just two modes at once.
                structure |= m == Mode::Structure;
                csr |= m == Mode::Csr;
                if structure && csr {
                    return Err(
                        "--csr writes the article's CSR, which is nothing but weights, \
                         and --structure keeps the shape of A and no weights at all; \
                         drop one of them"
                            .into(),
                    );
                }
                if let Some(already) = mode {
                    if already != m {
                        return Err(format!(
                            "{} and {} are two different runs; drop one of them",
                            already.flag(),
                            m.flag()
                        ));
                    }
                }
                mode = Some(m);
            }
            None if arg.starts_with("--") => {
                return Err(format!("unknown option {:?}\n{}", arg, USAGE));
            }
            None => {
                if path.is_some() {
                    return Err(format!("two files, {:?} and {:?}", path.unwrap(), arg));
                }
                path = Some(arg.to_string());
            }
        }
        i += 1;
    }

    let mode = mode.unwrap_or(Mode::Compact);
    let path = match path {
        Some(p) => p,
        None => return Err(format!("no file to work on\n{}", USAGE)),
    };

    // A flag that quietly does nothing is a run doing something other than what
    // was asked, and so is a pipe that is quietly not read.  `--stats` and
    // `--verify` answer out of the matrix alone; if records were aimed at one of
    // them, the caller meant `--check`.
    let stdin = io::stdin();
    if mode.reads_records() {
        if stdin.is_terminal() {
            return Err(format!(
                "{} reads the records on stdin and stdin is a terminal; \
                 pipe the file in",
                mode.flag()
            ));
        }
    } else if let Some(waiting) = records_waiting() {
        return Err(format!(
            "{} reads what the matrix says about itself and no records at all, and \
             there are {} bytes of records on stdin; --check is the one that reads \
             both",
            mode.flag(),
            waiting
        ));
    }

    match mode {
        Mode::Compact => write(&path, true),
        Mode::Structure => write(&path, false),
        Mode::Csr => csr_write(&path),
        Mode::Check => check(&path),
        Mode::Verify => verify(&path),
        Mode::Stats => stats(&path),
    }
    .map_err(|e| e.to_string())
}

/// How many bytes of records are aimed at a run that reads none, when that is
/// answerable without reading one.
///
/// It is answerable for a redirected regular file and for nothing else: the
/// descriptor has a length and a position, and the difference between them is
/// what is left.  On a pipe there is no such pair, and the only way to find out
/// whether anything is coming is to read -- which is exactly what this must not
/// do.  A one-byte blocking read on an inherited pipe with no data in it hangs
/// the run forever, before it has opened the matrix or printed a line, and
/// `io::Stdin` is buffered, so the read that does return drains up to 8 KB and
/// steals the head of somebody else's pipeline.  That is what used to be here.
///
/// So a pipe gets no diagnostic, which is the honest answer: nothing short of
/// consuming the records distinguishes a pipe with records in it from a pipe
/// that was inherited and forgotten about.  A terminal gets none either -- an
/// interactive standard input is not somebody's records.
#[cfg(unix)]
fn records_waiting() -> Option<u64> {
    use std::mem::ManuallyDrop;
    use std::os::fd::FromRawFd;

    // A `dup` of the descriptor rather than the descriptor itself, because a
    // `File` closes what it holds when it drops and standard input is not ours
    // to close.  The copy shares the file offset, which is half of the question.
    // The same idiom as `rewindable_stdin` in `src/main.rs`, for the same
    // reason.
    let borrowed = unsafe { ManuallyDrop::new(std::fs::File::from_raw_fd(0)) };
    let mut own = borrowed.try_clone().ok()?;
    let meta = own.metadata().ok()?;
    if !meta.is_file() {
        return None;
    }
    let here = own.stream_position().ok()?;
    match meta.len().saturating_sub(here) {
        0 => None,
        left => Some(left),
    }
}

#[cfg(not(unix))]
fn records_waiting() -> Option<u64> {
    None
}

/// Bytes, in the units a 10 GB file is talked about in.
fn gb(bytes: u64) -> String {
    format!("{} bytes ({:.2} GB)", bytes, bytes as f64 / 1e9)
}

// -------------------------------------------------------------------------
// Writing
// -------------------------------------------------------------------------

fn write(path: &str, amounts: bool) -> io::Result<()> {
    let start = std::time::Instant::now();
    let mut reader = sexp::Reader::new(io::stdin().lock());
    let mut inputs: Vec<sexp::Input> = Vec::new();
    let mut writer = matrix::Writer::create(path, amounts)?;

    let mut records: u64 = 0;
    while let Some(record) = reader.next_record(&mut inputs)? {
        writer.push(&record, &inputs)?;
        records += 1;
        if records % 50_000_000 == 0 {
            eprintln!("  {} records", records);
        }
    }
    let sections = writer.sections();
    let header = writer.finish()?;
    let bytes = std::fs::metadata(path)?.len();

    eprintln!(
        "matrix: {} records, {} transactions, {} nonzeros, {} blocks in {:.1}s",
        header.records,
        header.transactions,
        header.nonzeros,
        header.blocks,
        start.elapsed().as_secs_f64()
    );
    eprintln!(
        "  {} -- {} the rows, {:.2} bytes a record, {:.2} bytes a nonzero{}",
        gb(bytes),
        gb(bytes - header.rows_offset),
        bytes as f64 / header.records.max(1) as f64,
        bytes as f64 / header.nonzeros.max(1) as f64,
        if amounts { "" } else { ", no amounts" }
    );
    eprintln!(
        "  header 64, TXFIX + GROUPS {} bytes, content hash {:#018x}",
        header.rows_offset - matrix::HEADER_BYTES,
        header.hash
    );
    // The budget, as this run spent it rather than as the module doc predicts
    // it.  The two are the same table.
    let share = |part: u64| 100.0 * part as f64 / bytes.max(1) as f64;
    eprintln!(
        "  columns {} ({:.1}%), amounts {} ({:.1}%), heads {} ({:.1}%), blocks {} ({:.1}%)",
        sections.columns,
        share(sections.columns),
        sections.amounts,
        share(sections.amounts),
        sections.heads,
        share(sections.heads),
        sections.blocks,
        share(sections.blocks)
    );
    Ok(())
}

// -------------------------------------------------------------------------
// The article's CSR, for the record
// -------------------------------------------------------------------------

fn csr_write(basename: &str) -> io::Result<()> {
    let start = std::time::Instant::now();
    let mut reader = sexp::Reader::new(io::stdin().lock());
    let mut inputs: Vec<sexp::Input> = Vec::new();

    let open = |suffix: &str| -> io::Result<BufWriter<std::fs::File>> {
        Ok(BufWriter::with_capacity(
            1 << 22,
            std::fs::File::create(format!("{}.{}", basename, suffix))?,
        ))
    };
    let mut row_ptr = open("rowptr")?;
    let mut col = open("col")?;
    let mut val = open("val")?;

    // `row_ptr[0]`, and then one entry a transaction as its row closes.
    row_ptr.write_all(&0u32.to_le_bytes())?;
    let mut next_row: u64 = 0;
    let mut nonzeros: u64 = 0;
    let mut records: u64 = 0;
    let mut duplicates: u64 = 0;

    // What the narrowing costs, counted rather than argued.
    let mut exact: u64 = 0;
    let mut worst_relative = 0.0f64;
    let mut worst_drift_f64 = 0.0f64;
    let mut worst_drift_f32 = 0.0f64;

    while let Some(record) = reader.next_record(&mut inputs)? {
        let tx = record.tx_id as u64;
        if tx < next_row {
            // A BIP-30 duplicate: the row is already closed.  It is a coinbase
            // both times, so the row is empty both times and nothing is lost --
            // which is a claim, so it is checked.
            if !inputs.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "record {} redefines transaction {} with {} inputs, and its \
                         CSR row is already closed",
                        records,
                        tx,
                        inputs.len()
                    ),
                ));
            }
            duplicates += 1;
            records += 1;
            continue;
        }
        // Transactions no record defines are empty rows.
        while next_row < tx {
            row_ptr.write_all(&(u32::try_from(nonzeros).map_err(too_wide)?).to_le_bytes())?;
            next_row += 1;
        }

        // The same weights `src/main.rs` computes, zero-total fallback and all,
        // narrowed to `f32` the way the article's CSR would hold them.
        let total: f64 = inputs.iter().map(|i| i.amount as f64).sum();
        let (mut sum64, mut sum32) = (0.0f64, 0.0f64);
        for input in inputs.iter() {
            let weight = if total > 0.0 {
                input.amount as f64 / total
            } else {
                1.0 / inputs.len() as f64
            };
            let narrow = weight as f32;
            let back = narrow as f64;
            if back == weight {
                exact += 1;
            } else if weight > 0.0 {
                worst_relative = worst_relative.max((back - weight).abs() / weight);
            }
            sum64 += weight;
            sum32 += back;
            col.write_all(&(u32::try_from(input.prev_tx_id as u64).map_err(too_wide)?).to_le_bytes())?;
            val.write_all(&narrow.to_le_bytes())?;
        }
        if !inputs.is_empty() {
            worst_drift_f64 = worst_drift_f64.max((sum64 - 1.0).abs());
            worst_drift_f32 = worst_drift_f32.max((sum32 - 1.0).abs());
        }

        nonzeros += inputs.len() as u64;
        row_ptr.write_all(&(u32::try_from(nonzeros).map_err(too_wide)?).to_le_bytes())?;
        next_row = tx + 1;
        records += 1;
        if records % 50_000_000 == 0 {
            eprintln!("  {} records", records);
        }
    }
    row_ptr.flush()?;
    col.flush()?;
    val.flush()?;

    let transactions = next_row;
    let bytes = 4 * (transactions + 1) + 8 * nonzeros;
    eprintln!(
        "matrix --csr: {} records, {} transactions, {} nonzeros ({} duplicate rows) in {:.1}s",
        records,
        transactions,
        nonzeros,
        duplicates,
        start.elapsed().as_secs_f64()
    );
    eprintln!(
        "  {} -- rowptr {}, col {}, val {}",
        gb(bytes),
        4 * (transactions + 1),
        4 * nonzeros,
        4 * nonzeros
    );
    eprintln!(
        "  f32 weights: {} of {} round-trip exactly ({:.2}%), worst relative error {:.3e}",
        exact,
        nonzeros,
        100.0 * exact as f64 / nonzeros.max(1) as f64,
        worst_relative
    );
    eprintln!(
        "  worst |sum(w) - 1| over a row: {:.3e} in f64, {:.3e} in f32",
        worst_drift_f64, worst_drift_f32
    );
    Ok(())
}

fn too_wide(_: std::num::TryFromIntError) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "a column or a row pointer is past 2^32, which the article's CSR stores as a u32",
    )
}

// -------------------------------------------------------------------------
// Checking
// -------------------------------------------------------------------------

fn check(path: &str) -> io::Result<()> {
    let start = std::time::Instant::now();
    let mut source = sexp::Reader::new(io::stdin().lock());
    let mut written = matrix::Reader::open(path, None)?;
    let amounts = written.amounts();
    let expected = *written.header();

    let mut from_source: Vec<sexp::Input> = Vec::new();
    let mut from_matrix: Vec<sexp::Input> = Vec::new();
    let mut hash_source = matrix::Fnv::new();
    let mut hash_matrix = matrix::Fnv::new();
    let mut records: u64 = 0;
    let mut nonzeros: u64 = 0;

    let fail = |records: u64, what: String| -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("record {}: {}", records, what),
        )
    };

    loop {
        let a = source.next_record(&mut from_source)?;
        let b = written.next_record(&mut from_matrix)?;
        let (a, b) = match (a, b) {
            (Some(a), Some(b)) => (a, b),
            (None, _) => break,
            (Some(_), None) => {
                return Err(fail(
                    records,
                    format!(
                        "the matrix holds {} records and the source has more",
                        expected.records
                    ),
                ))
            }
        };

        if a.block_id != b.block_id {
            return Err(fail(
                records,
                format!("block {} in the source, {} in the matrix", a.block_id, b.block_id),
            ));
        }
        if a.tx_id != b.tx_id {
            return Err(fail(
                records,
                format!("transaction {} in the source, {} in the matrix", a.tx_id, b.tx_id),
            ));
        }
        if a.outputs != b.outputs {
            return Err(fail(
                records,
                format!("{} outputs in the source, {} in the matrix", a.outputs, b.outputs),
            ));
        }
        if from_source.len() != from_matrix.len() {
            return Err(fail(
                records,
                format!(
                    "{} inputs in the source, {} in the matrix",
                    from_source.len(),
                    from_matrix.len()
                ),
            ));
        }
        for (i, (x, y)) in from_source.iter().zip(&from_matrix).enumerate() {
            if x.prev_tx_id != y.prev_tx_id {
                return Err(fail(
                    records,
                    format!(
                        "input {} spends {} in the source and {} in the matrix",
                        i, x.prev_tx_id, y.prev_tx_id
                    ),
                ));
            }
            if amounts && from_source.len() >= 2 && x.amount != y.amount {
                return Err(fail(
                    records,
                    format!(
                        "input {} is {} satoshi in the source and {} in the matrix",
                        i, x.amount, y.amount
                    ),
                ));
            }
        }

        // The weights, from each side, by the fold's own formula.  For a
        // one-input row this is the whole of the argument that its amount need
        // not be stored, re-tested on every record this ever runs over.
        if amounts {
            let share = |inputs: &[sexp::Input]| -> Vec<f64> {
                let total: f64 = inputs.iter().map(|i| i.amount as f64).sum();
                inputs
                    .iter()
                    .map(|i| {
                        if total > 0.0 {
                            i.amount as f64 / total
                        } else {
                            1.0 / inputs.len() as f64
                        }
                    })
                    .collect()
            };
            let (x, y) = (share(&from_source), share(&from_matrix));
            if x != y {
                return Err(fail(
                    records,
                    format!("the weights differ: {:?} against {:?}", x, y),
                ));
            }
        }

        matrix::hash_record(&mut hash_source, &a, &from_source, amounts);
        matrix::hash_record(&mut hash_matrix, &b, &from_matrix, amounts);
        nonzeros += from_source.len() as u64;
        records += 1;
        if records % 50_000_000 == 0 {
            eprintln!("  {} records", records);
        }
    }

    if hash_source.value() != hash_matrix.value() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the two sides hash differently: {:#018x} against {:#018x}",
                hash_source.value(),
                hash_matrix.value()
            ),
        ));
    }

    // The header's hash is over the whole file, so it can only be held against
    // a check that read the whole file.  A prefix of the records is a legitimate
    // thing to check -- it is the same inequality the reader's record count is
    // an inequality for -- and it verifies everything but that one number.
    let whole = records == expected.records;
    if whole && hash_source.value() != expected.hash {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "the records hash to {:#018x} and the header says {:#018x}",
                hash_source.value(),
                expected.hash
            ),
        ));
    }

    eprintln!(
        "matrix --check: {} records, {} nonzeros verified field for field in {:.1}s",
        records,
        nonzeros,
        start.elapsed().as_secs_f64()
    );
    eprintln!(
        "  block ids reconstructed from the coinbase rows alone: {} of {} agree",
        records, records
    );
    if whole {
        eprintln!(
            "  content hash {:#018x}, and the header agrees",
            hash_source.value()
        );
    } else {
        eprintln!(
            "  the records are a prefix: {} of the matrix's {} rows, so the header's \
             content hash {:#018x} is not the one this run computed",
            records, expected.records, expected.hash
        );
    }
    if !amounts {
        eprintln!("  written with --structure, so there were no amounts to check");
    }
    Ok(())
}

// -------------------------------------------------------------------------
// The matrix against itself
// -------------------------------------------------------------------------

fn verify(path: &str) -> io::Result<()> {
    let start = std::time::Instant::now();
    let mut reader = matrix::Reader::open(path, None)?;
    let header = *reader.header();
    let found = reader.verify()?;
    let seconds = start.elapsed().as_secs_f64();

    eprintln!(
        "matrix --verify: {} records, {} nonzeros, {} transactions, {} blocks \
         swept in {:.1}s",
        found.records, found.nonzeros, found.transactions, found.blocks, seconds
    );
    eprintln!(
        "  content hash {:#018x}, and the header agrees",
        found.hash
    );
    // The tail checks are not a separate pass: reading to the end is what runs
    // them, so saying which ones ran is saying what reaching this line means.
    eprintln!("  and the counts, TXFIX exhausted, and nothing after the last row");
    if !header.amounts() {
        eprintln!("  written with --structure, so the hash is over the shape alone");
    }
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.is_file() && seconds > 0.0 {
            eprintln!(
                "  {} at {:.0} MB/s",
                gb(meta.len()),
                meta.len() as f64 / seconds / 1e6
            );
        }
    }
    Ok(())
}

// -------------------------------------------------------------------------
// What a file says about itself
// -------------------------------------------------------------------------

fn stats(path: &str) -> io::Result<()> {
    // The rows' length here is the file's length less where the rows start, and
    // only a regular file has a length that means that: a FIFO and a character
    // device both stat as zero bytes, and zero less a rows offset is a wrapped
    // number in release and a panic in debug.  Refused rather than printed.
    let meta = std::fs::metadata(path)?;
    if !meta.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{} is not a regular file, and --stats measures a matrix against \
                 the length of the file holding it; --verify is the one that reads \
                 the rows rather than measuring them, and it takes a FIFO",
                path
            ),
        ));
    }
    let reader = matrix::Reader::open(path, None)?;
    let header = *reader.header();
    // The reader has already refused a file whose rows start past its end, which
    // is what makes this subtraction one and not the other kind.
    let bytes = meta.len();
    let rows = bytes - header.rows_offset;

    println!("{}", path);
    println!("  records      {}", header.records);
    println!("  transactions {}", header.transactions);
    println!("  nonzeros     {}", header.nonzeros);
    println!("  blocks       {}", header.blocks);
    println!(
        "  flags        {:#x}{}",
        header.flags,
        if header.amounts() {
            " (amounts)"
        } else {
            " (no amounts: written with --structure)"
        }
    );
    println!("  content hash {:#018x}", header.hash);
    println!();
    println!("  file         {}", gb(bytes));
    println!("  header       {} bytes", matrix::HEADER_BYTES);
    println!(
        "  TXFIX        {} entries{}",
        reader.txfix().len(),
        if reader.txfix().is_empty() {
            String::new()
        } else {
            format!(
                " -- rows {:?}",
                reader.txfix().iter().map(|e| e.0).collect::<Vec<_>>()
            )
        }
    );
    println!(
        "  GROUPS       {} entries of {} rows, {} bytes ({:.3}% of the file)",
        reader.groups().len(),
        matrix::GROUP,
        16 * reader.groups().len(),
        100.0 * (16 * reader.groups().len()) as f64 / bytes.max(1) as f64
    );
    println!("  ROWS         {} at byte {}", gb(rows), header.rows_offset);
    println!();
    println!(
        "  {:.2} bytes a record, {:.2} bytes a nonzero",
        bytes as f64 / header.records.max(1) as f64,
        bytes as f64 / header.nonzeros.max(1) as f64
    );
    // The comparison the whole file exists for.
    let csr = 4 * (header.transactions + 1) + 8 * header.nonzeros;
    println!(
        "  against the article's u32/u32/f32 CSR at {}: {:.2}x smaller, and exact",
        gb(csr),
        csr as f64 / bytes.max(1) as f64
    );
    Ok(())
}
