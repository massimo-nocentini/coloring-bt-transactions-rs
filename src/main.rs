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

mod colorset;
mod poly;
mod sexp;
mod simd;
mod store;

use poly::Coeff;
use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ExitCode;
use std::time::Instant;
use store::{ColorStore, RingStore};

/// The Scheme stops at `(> i 1000000)`, i.e. after 1,000,001 records.  Kept as
/// the default so a run reproduces the recorded output; override with argv[1],
/// or pass `all` for no limit.
const DEFAULT_LIMIT: usize = 1_000_001;

/// How often `--stats` reports, in records.
const STATS_EVERY: usize = 100_000;

fn main() -> ExitCode {
    let mut limit = DEFAULT_LIMIT;
    let mut stats = false;
    // The circular list is the exercise, so it is what runs unless asked
    // otherwise: a plain run of this program is still the Knuth port.
    let mut rings = true;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--stats" => stats = true,
            "--rings" => rings = true,
            "--sets" => rings = false,
            "all" => limit = usize::MAX,
            _ => match arg.parse::<usize>() {
                Ok(n) => limit = n,
                Err(_) => {
                    eprintln!(
                        "usage: circular-polynomial [<record-limit>|all] [--stats] \
                         [--rings|--sets] < records"
                    );
                    return ExitCode::FAILURE;
                }
            },
        }
    }

    // Two instantiations of the same loop, so neither backend pays for the
    // other's existence at run time.
    let outcome = if rings {
        run::<RingStore>(limit, stats)
    } else {
        run::<colorset::SetStore>(limit, stats)
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
    let mut inputs: Vec<usize> = Vec::new();
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

            for i in (0..inputs.len()).rev() {
                let previous = inputs[i];
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
                        None => store.share(held),
                        Some(acc) => {
                            let combined = store.union(held, &acc);
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
                        None => root,
                        Some(acc) => {
                            let combined = store.union(&root, &acc);
                            store.release(acc);
                            store.release(root);
                            combined
                        }
                    });
                }
            }
            accumulator.expect("inputs is non-empty, so the fold ran at least once")
        };

        line.clear();
        store.for_each_term(&color, |exponent, coefficient| {
            line.push(b'(');
            push_int(&mut line, exponent);
            line.extend_from_slice(b" . ");
            push_int(&mut line, coefficient);
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
