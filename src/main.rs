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

mod colorset;
mod poly;
mod sexp;
mod simd;
mod store;
mod weighted;

use poly::Coeff;
use std::collections::HashMap;
use std::io::{self, Write};
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

fn main() -> ExitCode {
    let mut limit = DEFAULT_LIMIT;
    let mut stats = false;
    // The circular list is the exercise, so it is what runs unless asked
    // otherwise: a plain run of this program is still the Knuth port.
    let mut backend = Backend::Rings;
    let mut chose_backend = false;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
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
            _ => match arg.parse::<usize>() {
                Ok(n) => limit = n,
                Err(_) => {
                    eprintln!(
                        "usage: circular-polynomial [<record-limit>|all] [--stats] \
                         [--rings|--sets|--weighted] < records"
                    );
                    return ExitCode::FAILURE;
                }
            },
        }
    }

    // One instantiation of the loop per backend, so none of them pays for the
    // others existing.
    let outcome = match backend {
        Backend::Rings => run::<RingStore>(limit, stats),
        Backend::Sets => run::<colorset::SetStore>(limit, stats),
        Backend::Weighted => run::<weighted::WeightedSets>(limit, stats),
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

fn run<S: ColorStore>(limit: usize, stats: bool) -> io::Result<()> {
    let mut reader = sexp::Reader::new(io::stdin().lock());
    let mut out = io::BufWriter::with_capacity(1 << 20, io::stdout().lock());

    let mut store = S::new();
    let mut colors: HashMap<usize, (S::Color, usize)> = HashMap::new();
    let mut inputs: Vec<sexp::Input> = Vec::new();
    let mut line: Vec<u8> = Vec::new();

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

        line.clear();
        store.for_each_term(&color, |exponent, coefficient| {
            line.push(b'(');
            push_int(&mut line, exponent);
            line.extend_from_slice(b" . ");
            if S::WEIGHTED {
                push_weight(&mut line, coefficient);
            } else {
                // Always exactly 1 here, and printed as the integer the Scheme
                // prints, so an unweighted run stays byte for byte comparable.
                push_int(&mut line, coefficient as usize);
            }
            line.extend_from_slice(b") ");
        });
        line.push(b'\n');
        out.write_all(&line)?;

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
    }

    out.flush()
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
