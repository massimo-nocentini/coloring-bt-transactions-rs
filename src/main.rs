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

mod poly;
mod sexp;

use poly::{Arena, Coeff, Idx};
use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ExitCode;

/// The Scheme stops at `(> i 1000000)`, i.e. after 1,000,001 records.  Kept as
/// the default so a run reproduces the recorded output; override with argv[1],
/// or pass `all` for no limit.
const DEFAULT_LIMIT: usize = 1_000_001;

/// How often `--stats` reports, in records.
const STATS_EVERY: usize = 100_000;

fn main() -> ExitCode {
    let mut limit = DEFAULT_LIMIT;
    let mut stats = false;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--stats" => stats = true,
            "all" => limit = usize::MAX,
            _ => match arg.parse::<usize>() {
                Ok(n) => limit = n,
                Err(_) => {
                    eprintln!(
                        "usage: circular-polynomial [<record-limit>|all] [--stats] < records"
                    );
                    return ExitCode::FAILURE;
                }
            },
        }
    }

    match run(limit, stats) {
        Ok(()) => ExitCode::SUCCESS,
        // Downstream went away (`| head`); that is not our failure.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("circular-polynomial: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run(limit: usize, stats: bool) -> io::Result<()> {
    let mut reader = sexp::Reader::new(io::stdin().lock());
    let mut out = io::BufWriter::with_capacity(1 << 20, io::stdout().lock());

    let mut arena = Arena::new();
    // The fold seed, shared by every transaction and never freed, exactly as the
    // Scheme's `0/polynomial` is.  `op` never mutates its operands, so sharing
    // it is safe.
    let zero = arena.make(&[]);

    let mut colors: HashMap<usize, (Idx, usize)> = HashMap::new();
    let mut inputs: Vec<usize> = Vec::new();
    let mut line: Vec<u8> = Vec::new();

    let mut records: usize = 0;
    while records < limit {
        let record = match reader.next_record(&mut inputs)? {
            Some(r) => r,
            None => break,
        };

        let color = if inputs.is_empty() {
            // Coinbase: the block that minted it is the whole color.
            arena.make(&[(record.block_id, 1)])
        } else {
            // `foldr` over the inputs, so right to left.  The order does not
            // change the result — `ior` is commutative and the merge is sorted —
            // but it decides which input hits an entry's last unspent output,
            // and the Scheme's order is the one to match.
            let mut accumulator = zero;
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
                let (previous_root, unspent) = *entry;

                let combined = arena.ior(previous_root, accumulator);
                if accumulator != zero {
                    arena.free_ring(accumulator);
                }
                if unspent == 1 {
                    // Last unspent output: nobody can reach this ring again.
                    colors.remove(&previous);
                    arena.free_ring(previous_root);
                } else {
                    entry.1 = unspent - 1;
                }
                accumulator = combined;
            }
            accumulator
        };

        line.clear();
        arena.for_each_term(color, |exponent, coefficient| {
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
                // The Scheme leaks the displaced ring; we can afford not to.
                arena.free_ring(displaced);
            }
        } else {
            arena.free_ring(color);
        }

        records += 1;
        if stats && records % STATS_EVERY == 0 {
            eprintln!(
                "{:>10} records  {:>12} live nodes  {:>12} arena nodes  {:>10} colored txs",
                records,
                arena.live(),
                arena.capacity(),
                colors.len()
            );
        }
    }

    if stats {
        audit(&arena, &colors, zero);
    }

    out.flush()
}

/// Every node the arena thinks is live must be reachable from a ring someone
/// still holds: the rings in `colors`, plus the shared zero.  A mismatch means a
/// ring was dropped without [`Arena::free_ring`] — a leak the output diff would
/// never show, since a leaked ring is still a *correct* ring.
fn audit(arena: &Arena, colors: &HashMap<usize, (Idx, usize)>, zero: Idx) {
    let reachable: usize = arena.ring_len(zero)
        + colors
            .values()
            .map(|&(root, _)| arena.ring_len(root))
            .sum::<usize>();
    let live = arena.live();
    if reachable == live {
        eprintln!("audit: {} live nodes, all reachable", live);
    } else {
        eprintln!(
            "audit: LEAK -- {} live nodes but only {} reachable ({} lost)",
            live,
            reachable,
            live - reachable
        );
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
