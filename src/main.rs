//! # Bitcoin transaction colors
//!
//! A port of the driver at the bottom of `src/test/circular-polynomial.scm`.
//!
//! Transaction records stream in on stdin (see [`sexp`]).  Each transaction is
//! given a **color**: the polynomial whose exponents are the ids of the blocks
//! its coins descend from.  A coinbase transaction is colored by the block that
//! minted it, `x^block`; every other transaction is colored by the bitwise-or of
//! the colors of the transactions its inputs spend.  Because `ior` on the
//! coefficients only ever combines 1s, a color is in effect a *set* of block
//! ids, held as a sorted ring — see [`poly`] for why that shape.
//!
//! `colors` holds one ring per transaction with unspent outputs, along with how
//! many of those outputs are still unspent.  Spending the last one drops the
//! entry, which is also what tells the arena the ring can be reused: the count
//! is the whole memory-management story here, and it is why the working set
//! tracks the UTXO set rather than the whole chain.
//!
//! One line goes out per record: each term as `(exponent . coefficient)`
//! followed by a space, so a transaction with no color prints an empty line.
//! That is byte for byte what Chicken's `(print* (car p*) " ")` produces, which
//! is the point — the reference output can be diffed against.
//!
//! # A picture instead
//!
//! That output is enormous — a color of a thousand blocks is fourteen thousand
//! bytes of `(block . 1)` — and most of every line is punctuation.  `--pbm
//! <file>` and `--svg <file>` draw the same answer instead: one row per record
//! in the order the records arrive, one column per block id counting up from 0,
//! black where the block is in the color.  The bitmap packs the pixels; the SVG
//! strokes a line per run of adjacent blocks and pays nothing for the white.
//! Which comes out smaller depends on the records, and `--stats` measures it —
//! see [`image`].
//!
//! Two more knobs, both about size:
//!
//! - `--bin <n>` puts `n` consecutive transactions on one row, black where any
//!   of them reaches that block.  A million rows is a picture nothing will show
//!   you whole; binning is how it becomes one that will.
//! - `--blocks <n>` says how many columns to draw, which is the one thing the
//!   image cannot discover as it goes — it is the distance from one row to the
//!   next.  Left out, [`survey`] reads the records once to count the blocks
//!   before drawing them, which needs an input that can be rewound.
//!
//! # Backends
//!
//! Three representations of a color, chosen at the command line, all driven by
//! the one loop in [`run`] through [`store::ColorStore`]:
//!
//! - `--rings` (the default) — [`poly`], the Knuth exercise.
//! - `--sets` — [`colorset`], the same answer from sorted arrays, several times
//!   faster.  Its output is byte-identical to `--rings`, which is what makes the
//!   pair a usable cross-check.
//! - `--weighted` — [`weighted`], which answers a different question.
//!
//! # Weighted colors
//!
//! Plain coloring says *which* blocks a transaction's coins came from and gives
//! each a coefficient of 1.  `--weighted` says *how much* came from each: an
//! input spending `amount` out of a transaction's `total` carries that fraction
//! of its ancestor's color, so
//!
//! ```text
//!     C  =  sum_i  (amount_i / total) . C_i
//! ```
//!
//! Every color is then a distribution over block ids summing to 1, and
//! coefficients print as fixed-point decimals rather than the integer 1.  That
//! makes the output *not* comparable with the Scheme's, which is why it is a
//! separate mode rather than a change to the existing one.
//!
//! Be aware of what the fixed format hides.  Weights decay by roughly a factor
//! per hop of ancestry, so on a long chain the great majority of a color's terms
//! fall below what [`WEIGHT_PLACES`] decimals can show and print as `0.000000`.
//! They are still there and still counted — the sum is still 1 — but they cannot
//! be read off the output.  See [`push_weight`].
//!
//! # The other binaries
//!
//! This is the driver, and the rest of the crate is four programs that do
//! something else with the same colouring or the same layout.  Each is its own
//! page in these docs; what they have in common is here.
//!
//! - [`tx-mean`](../tx_mean/index.html) — the weighted colouring collapsed to
//!   one `f64` per record, `<tx-id>,<mean>` a line.  A colour is a set and a set
//!   does not fit in a column; its centre of mass does, and it reads on the same
//!   scale as a block id.
//! - [`tree-jp2`](../tree_jp2/index.html) — a webgraph laid out as a tree and
//!   written as a lossless JPEG 2000, one pixel per node.  Nothing to do with
//!   transactions; it shares the layout the viewers use, not the colouring.
//! - `tree-view` and `tx-view` — the same two drawings in a window one can pan
//!   and zoom, the second coloured by what this file computes.  They are behind
//!   the `gui` feature, since GTK is a C library the rest of the crate has no
//!   reason to want installed, so they are absent from a default `cargo doc`
//!   and from these pages; `cargo doc --features gui` builds them.

mod colorset;
mod image;
mod poly;
mod sexp;
mod simd;
mod store;
mod weighted;

use poly::Coeff;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::process::ExitCode;
use std::time::Instant;
use store::{ColorStore, RingStore};

/// The Scheme stops at `(> i 1000000)`, i.e. after 1,000,001 records.  Kept as
/// the default so a run reproduces the recorded output; override with `argv[1]`,
/// or pass `all` for no limit.
const DEFAULT_LIMIT: usize = 1_000_001;

/// How often `--stats` reports, in records.
const STATS_EVERY: usize = 100_000;

/// Which representation of a color to run with.  See [`store`] for what the
/// three have in common and why more than one of them exists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// Circular linked lists: the Knuth exercise, and the default.
    Rings,
    /// Sorted sets of block ids.  Same output, several times faster.
    Sets,
    /// Sorted sets carrying a weight per block.  Different output.
    Weighted,
}

/// Where a finished color goes.
///
/// An enum rather than a trait because the choice is made once and the match is
/// per *record*: inside each arm the walk over the color's terms is still a
/// monomorphic closure, which is the loop that has to stay cheap.
enum Output {
    /// A line of `(block . coefficient)` pairs per record, on stdout.  The
    /// buffer is reused across records; `line` is a field rather than a local
    /// for that reason alone.
    Text {
        out: io::BufWriter<io::StdoutLock<'static>>,
        line: Vec<u8>,
    },
    /// A row of pixels per record, in a file.  See [`image`].
    Picture(image::Writer<File>),
}

impl Output {
    fn emit<S: ColorStore>(&mut self, store: &S, color: &S::Color) -> io::Result<()> {
        match self {
            Output::Text { out, line } => {
                line.clear();
                store.for_each_term(color, |exponent, coefficient| {
                    line.push(b'(');
                    push_int(line, exponent);
                    line.extend_from_slice(b" . ");
                    if S::WEIGHTED {
                        push_weight(line, coefficient);
                    } else {
                        // Always exactly 1 here, and printed as the integer the
                        // Scheme prints, so an unweighted run stays byte for
                        // byte comparable.
                        push_int(line, coefficient as usize);
                    }
                    line.extend_from_slice(b") ");
                });
                line.push(b'\n');
                out.write_all(line)
            }
            // The coefficient is dropped: a pixel says the block is in the
            // color, which under the unweighted backends is everything the term
            // had to say.
            Output::Picture(picture) => {
                store.for_each_term(color, |exponent, _| picture.set(exponent));
                picture.end_transaction()
            }
        }
    }

    /// Close the output.  For the bitmap this is not a formality — the height
    /// only goes into the header here.
    fn finish(self) -> io::Result<()> {
        match self {
            Output::Text { mut out, .. } => out.flush(),
            Output::Picture(picture) => picture.finish().map(|_| ()),
        }
    }
}

const USAGE: &str = "usage: circular-polynomial [<record-limit>|all] [--stats] \
                     [--rings|--sets|--weighted] \
                     [--pbm <file>|--svg <file> [--blocks <n>] [--bin <n>]] < records";

/// Standard input, as something that can be read a second time — when the
/// platform and the shell allow it.
///
/// A `dup` of the descriptor rather than the descriptor itself, because a `File`
/// closes what it holds when it drops and standard input is not ours to close.
/// The copy shares its file offset with the original, which is what a program
/// that reads the records and then puts them back should do.
///
/// Asking where we are is the question a pipe refuses, and refusing is how it
/// says it cannot be rewound.
#[cfg(unix)]
fn rewindable_stdin() -> Option<File> {
    use std::mem::ManuallyDrop;
    use std::os::fd::FromRawFd;

    let borrowed = unsafe { ManuallyDrop::new(File::from_raw_fd(0)) };
    let mut own = borrowed.try_clone().ok()?;
    own.stream_position().ok()?;
    Some(own)
}

#[cfg(not(unix))]
fn rewindable_stdin() -> Option<File> {
    None
}

/// Read the records once without coloring them, for the number the bitmap has
/// to know before it can write anything.
///
/// Answers `(blocks, records)` — one past the largest block id the records
/// carry, which is how many columns the image needs, and how many records there
/// were, which is only for saying so out loud.
///
/// One pass is enough because a color is a set of the blocks its transaction's
/// coins *descend* from, and an ancestor cannot be mined later than its
/// descendant: no color names a block beyond the one its own record sits in, so
/// the largest block id in the records bounds every pixel in the picture.
///
/// Only the records the run will actually reach are looked at, so a record limit
/// narrows the image rather than padding it out to a chain the run stops short
/// of.
fn survey(input: impl io::Read, limit: usize) -> io::Result<(usize, usize)> {
    let mut reader = sexp::Reader::new(input);
    let mut inputs: Vec<sexp::Input> = Vec::new();
    let (mut blocks, mut records) = (0, 0);

    while records < limit {
        match reader.next_record(&mut inputs)? {
            Some(record) => blocks = blocks.max(record.block_id + 1),
            None => break,
        }
        records += 1;
    }
    Ok((blocks, records))
}

/// Work out where the records come from and where the colors go.
///
/// The two are settled together because the bitmap's width may have to be read
/// off the records before the first row can be written, and that costs the input
/// a rewind.  The error is the message to print, since every one of these is a
/// complaint about the command line rather than something to recover from.
fn plan(
    bitmap: Option<(image::Format, String)>,
    blocks: Option<usize>,
    bin: Option<usize>,
    limit: usize,
    stats: bool,
) -> Result<(Output, Box<dyn io::Read>), String> {
    // Held as a file when standard input is one, so that `survey` can read the
    // records and put them back.
    let mut source = rewindable_stdin();

    let (format, path) = match bitmap {
        Some(picture) => picture,
        None => {
            if let Some(name) = blocks.map(|_| "--blocks").or(bin.map(|_| "--bin")) {
                return Err(format!(
                    "{} describes the picture, so it needs --pbm <file> or --svg <file>",
                    name
                ));
            }
            return Ok((
                Output::Text {
                    out: io::BufWriter::with_capacity(1 << 20, io::stdout().lock()),
                    line: Vec::new(),
                },
                records_from(source),
            ));
        }
    };

    let bin = match bin {
        Some(0) => return Err("--bin 0 asks a row to stand for no transactions".into()),
        Some(n) => n,
        None => 1,
    };

    let width = match blocks {
        Some(0) => return Err("--blocks 0 leaves the picture no columns to draw in".into()),
        Some(n) => n,
        None => {
            let file = source.as_mut().ok_or_else(|| {
                "standard input cannot be rewound, so the blocks cannot be counted before \
                 the rows are written: redirect the records from a file (`< records`) \
                 rather than through a pipe, or say how many there are with --blocks <n>"
                    .to_string()
            })?;
            let start = file.stream_position().map_err(|e| e.to_string())?;
            let (blocks, records) = survey(&*file, limit).map_err(|e| e.to_string())?;
            file.seek(SeekFrom::Start(start))
                .map_err(|e| e.to_string())?;
            // Worth a line on stderr: it is a whole pass over the input, so a
            // long run is otherwise silent for a while before anything happens.
            if records == 0 {
                // Every record carries a block, so no blocks means no records.
                // The image is then 0 x 0, which is a header a reader will
                // parse and an image no reader will show.
                eprintln!("circular-polynomial: no records, so {} is empty", path);
            } else {
                eprintln!(
                    "circular-polynomial: {} records over {} blocks, so a {} x {} picture",
                    records,
                    blocks,
                    blocks,
                    records.div_ceil(bin)
                );
            }
            blocks
        }
    };

    let writer = File::create(&path)
        .and_then(|f| image::Writer::new(f, format, width, bin, stats))
        .map_err(|e| format!("{}: {}", path, e))?;
    Ok((Output::Picture(writer), records_from(source)))
}

/// The records, from the rewindable handle if there is one and from standard
/// input as it comes otherwise.
///
/// Boxed rather than made another type parameter of [`run`]: this is read a
/// megabyte at a time, so a virtual call per refill is nothing, where another
/// parameter would be another copy of the driver per backend.
fn records_from(source: Option<File>) -> Box<dyn io::Read> {
    match source {
        Some(file) => Box::new(file),
        None => Box::new(io::stdin().lock()),
    }
}

/// The value of a `--name <value>` or `--name=<value>` option at `args[i]`, with
/// how many arguments it took.
///
/// Both spellings, because one of these options is a path and the other a count
/// and neither reads well glued to its name.  A prefix match alone would accept
/// `--blocksy`, so the character after the name has to be an `=` or nothing.
///
/// The separate word is not taken if it looks like another option, so `--pbm
/// --stats` is a missing filename rather than a file called `--stats`.  Nothing
/// here wants a value of that shape, and swallowing the next flag would leave a
/// run silently doing something else.
fn option<'a>(args: &'a [String], i: usize, name: &str) -> Option<(&'a str, usize)> {
    let rest = args[i].strip_prefix(name)?;
    if rest.is_empty() {
        return args
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .map(|v| (v.as_str(), 2));
    }
    rest.strip_prefix('=').map(|v| (v, 1))
}

fn main() -> ExitCode {
    let mut limit = DEFAULT_LIMIT;
    let mut stats = false;
    // The circular list is the exercise, so it is what runs unless asked
    // otherwise: a plain run of this program is still the Knuth port.
    let mut backend = Backend::Rings;
    let mut chose_backend = false;
    let mut bitmap: Option<(image::Format, String)> = None;
    let mut blocks: Option<usize> = None;
    let mut bin: Option<usize> = None;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stats" => stats = true,
            "--rings" => {
                backend = Backend::Rings;
                chose_backend = true;
            }
            "--sets" => {
                backend = Backend::Sets;
                chose_backend = true;
            }
            // Weighting needs coefficients to weight, and the ring backend has
            // none it can reach — see `RingStore::WEIGHTED`.  So this selects a
            // backend rather than modifying one, and saying `--rings` too is a
            // contradiction rather than a refinement.
            "--weighted" => {
                if chose_backend && backend == Backend::Rings {
                    eprintln!(
                        "circular-polynomial: --weighted cannot run on --rings; \
                         weights need the set representation"
                    );
                    return ExitCode::FAILURE;
                }
                backend = Backend::Weighted;
                chose_backend = true;
            }
            "all" => limit = usize::MAX,
            _ => {
                // Two spellings of one picture; the last one asked for wins,
                // rather than one silently drawing over the other's file.
                let formats = [("--pbm", image::Format::Pbm), ("--svg", image::Format::Svg)];
                let mut drawn = 0;
                for (name, format) in formats {
                    if let Some((path, used)) = option(&args, i, name) {
                        bitmap = Some((format, path.to_string()));
                        drawn = used;
                        break;
                    }
                }
                if drawn > 0 {
                    i += drawn;
                    continue;
                }
                let counts = [("--blocks", &mut blocks), ("--bin", &mut bin)];
                let mut taken = 0;
                for (name, slot) in counts {
                    if let Some((n, used)) = option(&args, i, name) {
                        match n.parse::<usize>() {
                            Ok(n) => *slot = Some(n),
                            Err(_) => {
                                eprintln!(
                                    "circular-polynomial: {} wants a count, got {:?}",
                                    name, n
                                );
                                return ExitCode::FAILURE;
                            }
                        }
                        taken = used;
                        break;
                    }
                }
                if taken > 0 {
                    i += taken;
                    continue;
                }
                match args[i].parse::<usize>() {
                    Ok(n) => limit = n,
                    Err(_) => {
                        eprintln!("{}", USAGE);
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
        i += 1;
    }

    let (output, input) = match plan(bitmap, blocks, bin, limit, stats) {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("circular-polynomial: {}", message);
            return ExitCode::FAILURE;
        }
    };

    // One instantiation of the loop per backend, so none of them pays for the
    // others existing.
    let outcome = match backend {
        Backend::Rings => run::<RingStore>(limit, stats, output, input),
        Backend::Sets => run::<colorset::SetStore>(limit, stats, output, input),
        Backend::Weighted => run::<weighted::WeightedSets>(limit, stats, output, input),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        // Downstream went away (`| head`); that is not our failure.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("circular-polynomial: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run<S: ColorStore>(
    limit: usize,
    stats: bool,
    mut out: Output,
    input: Box<dyn io::Read>,
) -> io::Result<()> {
    let mut reader = sexp::Reader::new(input);

    let mut store = S::new();
    let mut colors: HashMap<usize, (S::Color, usize)> = HashMap::new();
    let mut inputs: Vec<sexp::Input> = Vec::new();

    // Only read when `--stats` is on, but started unconditionally: the clock has
    // to be running before the first record, and one `Instant::now()` per
    // process is not worth branching over.
    let started = Instant::now();
    let mut since_report = started;

    let mut records: usize = 0;
    while records < limit {
        let record = match reader.next_record(&mut inputs)? {
            Some(r) => r,
            None => break,
        };

        let color = if inputs.is_empty() {
            // Coinbase: the block that minted it is the whole color.
            store.singleton(record.block_id)
        } else {
            // `foldr` over the inputs, so right to left.  The order does not
            // change the result — union is commutative and the merge is sorted —
            // but it decides which input hits an entry's last unspent output,
            // and the Scheme's order is the one to match.
            //
            // `None` is the seed.  The Scheme folds from `0/polynomial` and so
            // did this until the backends were split, but an empty operand is
            // something every union has to carry through the merge, and the
            // whole first step of the fold is then a copy of one input for no
            // reason.  Not having a seed says the same thing and does no work.
            let mut accumulator: Option<S::Color> = None;

            // Each input contributes its ancestor's color in proportion to the
            // amount it spends, so the shares are `amount / total`.  Computed
            // once per record and only when the backend will use them; for the
            // unweighted stores `S::WEIGHTED` is a constant `false`, so this and
            // every weight below fold away at compile time.
            //
            // A total of zero has no proportions to speak of — it happens, since
            // nothing forbids a zero-value input — so the inputs share equally
            // instead of dividing by it.
            let total: f64 = if S::WEIGHTED {
                inputs.iter().map(|i| i.amount as f64).sum()
            } else {
                0.0
            };
            let share = |input: &sexp::Input| -> f64 {
                if !S::WEIGHTED {
                    1.0
                } else if total > 0.0 {
                    input.amount as f64 / total
                } else {
                    1.0 / inputs.len() as f64
                }
            };

            for i in (0..inputs.len()).rev() {
                let previous = inputs[i].prev_tx_id;
                let weight = share(&inputs[i]);
                let entry = match colors.get_mut(&previous) {
                    Some(e) => e,
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "transaction {} spends unknown transaction {}",
                                record.tx_id, previous
                            ),
                        ))
                    }
                };
                let unspent = entry.1;

                if unspent > 1 {
                    // Others can still reach this color, so it has to survive
                    // the fold: merge from a borrow, and on the first step take
                    // a second handle rather than the color itself.
                    entry.1 = unspent - 1;
                    let held = &entry.0;
                    accumulator = Some(match accumulator.take() {
                        // First step: the accumulator *is* this input's share of
                        // it.  A single-input transaction has weight 1 and
                        // `scale` shares rather than rebuilding, so the
                        // commonest shape still costs nothing.
                        None => store.scale(held, weight),
                        Some(acc) => {
                            // The accumulator already carries its own share, so
                            // it comes in at full strength.
                            let combined = store.combine(held, weight, &acc, 1.0);
                            store.release(acc);
                            combined
                        }
                    });
                } else {
                    // The last unspent output is being spent right now, so
                    // nobody can reach this color again and the driver may take
                    // it outright.  This is the case that used to cost a full
                    // copy: `union(previous, empty)` followed by freeing the
                    // original on the very next line.  It is the driver's UTXO
                    // bookkeeping that makes the move safe, not anything a store
                    // could work out for itself, which is why it lives here.
                    let (root, _) = colors.remove(&previous).expect("just looked it up");
                    accumulator = Some(match accumulator.take() {
                        None => {
                            // Taking the ring outright is only the whole answer
                            // when this input is the whole transaction; at any
                            // other weight it still has to be scaled, and then
                            // the taken color is released like any other.
                            if weight == 1.0 {
                                root
                            } else {
                                let scaled = store.scale(&root, weight);
                                store.release(root);
                                scaled
                            }
                        }
                        Some(acc) => {
                            let combined = store.combine(&root, weight, &acc, 1.0);
                            store.release(acc);
                            store.release(root);
                            combined
                        }
                    });
                }
            }
            accumulator.expect("inputs is non-empty, so the fold ran at least once")
        };

        if stats {
            store.observe(&color);
        }

        out.emit::<S>(&store, &color)?;

        if record.outputs > 0 {
            if let Some((displaced, _)) = colors.insert(record.tx_id, (color, record.outputs)) {
                // The Scheme leaks the displaced color; we can afford not to.
                store.release(displaced);
            }
        } else {
            store.release(color);
        }

        records += 1;
        if stats && records.is_multiple_of(STATS_EVERY) {
            let now = Instant::now();
            let (live, committed) = store.usage();
            let (live_label, committed_label) = store.usage_labels();
            eprintln!(
                "{:>10.2}s {:>10} records {:>11}  {:>12} {}  {:>12} {}  {:>10} colored txs",
                started.elapsed().as_secs_f64(),
                records,
                // Rate over this interval rather than the whole run so far.  A
                // cumulative average is smoother but hides the thing worth
                // watching: colors grow as the chain does, so the merge gets
                // slower as the run goes on, and only the interval rate shows it.
                rate(STATS_EVERY, now.duration_since(since_report).as_secs_f64()),
                live,
                live_label,
                committed,
                committed_label,
                colors.len()
            );
            since_report = now;
        }
    }

    if stats {
        let elapsed = started.elapsed().as_secs_f64();
        eprintln!(
            "{:>10.2}s {:>10} records {:>11}  total",
            elapsed,
            records,
            rate(records, elapsed)
        );
        eprintln!("{}", store.audit(&mut colors.values().map(|(c, _)| c)));
        // Worth saying out loud in a bitmap run, where stdout stays empty and
        // the header is the only other place the size is written down.
        if let Output::Picture(picture) = &out {
            let (columns, rows) = picture.dimensions();
            let (runs, pbm_row, svg_row) = picture.runs();
            eprintln!("picture: {} columns x {} rows", columns, rows);
            // Which format is smaller is a question about the records rather
            // than about the formats — see `image` — so it is answered here,
            // from the run count the run actually produced, rather than guessed
            // at in advance.
            eprintln!(
                "picture: {} runs, {} a row: {} bytes a row as --pbm, {} as --svg",
                runs,
                runs / rows.max(1) as u64,
                pbm_row,
                svg_row
            );
        }
    }

    out.finish()
}

/// `records` over `seconds`, short enough to hold a fixed column.
///
/// Answers `--` rather than an infinity when no time has passed, which happens
/// for real: a short run under `--stats` can reach its first checkpoint inside
/// the clock's resolution.
fn rate(records: usize, seconds: f64) -> String {
    if seconds <= 0.0 {
        return "-- rec/s".to_string();
    }
    let per_second = records as f64 / seconds;
    if per_second >= 1e6 {
        format!("{:.1}M rec/s", per_second / 1e6)
    } else if per_second >= 1e3 {
        format!("{:.0}k rec/s", per_second / 1e3)
    } else {
        format!("{:.0} rec/s", per_second)
    }
}

/// How many decimal places a weight is printed to.
const WEIGHT_PLACES: u32 = 6;

/// A weight in `[0, 1]`, to [`WEIGHT_PLACES`] fixed decimals.
///
/// Fixed rather than shortest-round-trip, and done in integers rather than with
/// `{}`, for the same reason [`push_int`] exists: this runs once per term, tens
/// of millions of times, and float formatting is not cheap.  Scaling by a power
/// of ten and printing two integers costs one multiply and one rounding.
///
/// What that gives up is resolution.  A weight below half of the smallest
/// representable place prints as `0.000000` — the term is still there, and still
/// counts toward the sum, it just cannot be read off the output.  Deep enough
/// ancestry will do that to a weight.
fn push_weight(out: &mut Vec<u8>, value: f64) {
    let scale = 10u64.pow(WEIGHT_PLACES);
    let units = (value * scale as f64).round() as u64;

    push_int(out, (units / scale) as usize);
    out.push(b'.');

    // The fraction is zero-padded to a fixed width, which `push_int` will not do
    // -- it prints 5 as "5" where this needs "000005".
    let mut fraction = units % scale;
    let mut digits = [b'0'; WEIGHT_PLACES as usize];
    let mut i = digits.len();
    while fraction > 0 {
        i -= 1;
        digits[i] = b'0' + (fraction % 10) as u8;
        fraction /= 10;
    }
    out.extend_from_slice(&digits);
}

/// Decimal, straight into the line buffer.  `write!` would do it too, but its
/// formatting machinery is a real cost across a million lines of thousands of
/// terms each.
fn push_int(out: &mut Vec<u8>, value: Coeff) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut magnitude = value;
    loop {
        i -= 1;
        digits[i] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}
