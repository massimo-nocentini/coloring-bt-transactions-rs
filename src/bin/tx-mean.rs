//! # One number per transaction: where its coins came from, on average
//!
//! [`main`](../coloring_bt_transactions/index.html) prints a transaction's
//! **colour** in full — every block its coins descend from, each with its share
//! of the value under `--weighted`.  That is the whole answer, and it is also
//! why a line of it can run to tens of thousands of bytes: a colour is a *set*,
//! and a set does not fit in a column.
//!
//! This binary prints the same colouring collapsed to one `f64` per record:
//!
//! ```text
//!     mean  =  sum_b  b . weight(b)
//! ```
//!
//! the **weighted mean block id** — the centre of mass of the blocks the coins
//! came from.  A coinbase minted in block `b` prints exactly `b`; a transaction
//! spending half its value from block 0 and half from block 3 prints `1.5`.  So
//! the number reads on the same scale as a block id, and the distance between
//! two of them is a distance along the chain.
//!
//! ```text
//! tx-mean [<record-limit>|all] < records
//! ```
//!
//! Output is `<tx-id>,<mean>` a line, in the order the records arrive, and each
//! line is written the moment that record's colour is finished — nothing is held
//! back to the end, so this is usable in a pipe over a chain that has not
//! stopped arriving.  (Standard output is still buffered a megabyte at a time;
//! that is an I/O detail, not a batching one.)
//!
//! # Why this needs the weighted backend
//!
//! A mean needs weights to be a mean.  The unweighted backends give every block
//! in a colour a coefficient of 1, so the best they could offer is the *plain*
//! mean of the block ids — which counts a block that one satoshi passed through
//! the same as the block the whole balance was minted in.  So the store here is
//! [`weighted::WeightedSets`] and there is no flag to change it: this program
//! asks a question only that backend can answer.
//!
//! What that costs is the caveat the main driver states about `--weighted` — a
//! weight decays by roughly a factor per hop of ancestry, so a colour's older
//! terms carry very little.  It costs this program *less* than it costs printing
//! the terms, though: a weight too small to show in six decimals still moves the
//! mean by its full share, because the mean sums the weights rather than
//! rounding each one on its own.
//!
//! # The loop
//!
//! It is `main::run`'s, with the emit step replaced.  A transaction's colour is
//! the fold of its inputs' colours, each scaled by the fraction of value it
//! carries, and an entry is dropped when its last unspent output is spent — so
//! the colours in flight track the UTXO set rather than the chain, exactly as
//! they do in the main driver.  That bookkeeping is the reason a run over a
//! million records fits in memory, and it is why it is copied here rather than
//! simplified away.

use std::collections::HashMap;
use std::io::{self, Write};
use std::process::ExitCode;

// The colouring itself, straight out of the main binary rather than
// reimplemented beside it -- `src/*.rs` belong to that binary, so they are
// reached by `#[path]`, the way `tx-view` reaches them.  Only part of what they
// offer is wanted here, so the rest reads as dead, and their prose points at
// modules this binary does not include; both lints are about that and neither is
// about a mistake.
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../poly.rs"]
mod poly;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../sexp.rs"]
mod sexp;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../simd.rs"]
mod simd;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../store.rs"]
mod store;
#[allow(dead_code, rustdoc::broken_intra_doc_links)]
#[path = "../weighted.rs"]
mod weighted;

use store::ColorStore;
use weighted::{Color, WeightedSets};

const USAGE: &str = "usage: tx-mean [<record-limit>|all] < records";

/// How many decimal places the mean is printed to.
///
/// The same six as `main`'s weights, and for the same reason: enough to separate
/// two transactions that differ, few enough to hold a column.  Six is also well
/// inside what an `f64` can say about a number the size of a block id — a mean
/// near a million still has nine significant digits to spare.
const PLACES: u32 = 6;

fn main() -> ExitCode {
    // No limit unless one is asked for.  `main`'s default stops at 1,000,001
    // records because that is where the Scheme it ports stops and the recorded
    // output has to be reproducible; this program has no output to reproduce, so
    // the useful default is "all of them".
    let mut limit = usize::MAX;

    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "all" => limit = usize::MAX,
            _ => match arg.parse::<usize>() {
                Ok(n) => limit = n,
                Err(_) => {
                    eprintln!("{}", USAGE);
                    return ExitCode::FAILURE;
                }
            },
        }
    }

    let out = io::BufWriter::with_capacity(1 << 20, io::stdout().lock());
    match run(io::stdin().lock(), out, limit) {
        Ok(()) => ExitCode::SUCCESS,
        // Downstream went away (`| head`); that is not our failure.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("tx-mean: {}", e);
            ExitCode::FAILURE
        }
    }
}

/// Colour at most `limit` records off `input`, writing a line to `out` per
/// record as each colour is finished.
///
/// The writer is a parameter rather than standard output taken here, so that a
/// test can read back what a run produced; `main` hands it the buffered stdout
/// it would otherwise have opened.
fn run(input: impl io::Read, mut out: impl Write, limit: usize) -> io::Result<()> {
    let mut reader = sexp::Reader::new(input);

    let mut store = WeightedSets::new();
    // A transaction's colour, and how many of its outputs are still unspent.
    // The count is the whole memory-management story: spending the last one
    // drops the entry, and with it the colour.
    let mut colors: HashMap<usize, (Color, usize)> = HashMap::new();
    let mut inputs: Vec<sexp::Input> = Vec::new();
    // Reused across records so the formatting does not allocate per line.
    let mut line: Vec<u8> = Vec::new();

    let mut records: usize = 0;
    while records < limit {
        let record = match reader.next_record(&mut inputs)? {
            Some(r) => r,
            None => break,
        };

        let color = if inputs.is_empty() {
            // Coinbase: the block that minted it holds all of the weight, so the
            // mean of this one comes out as the block id itself.
            store.singleton(record.block_id)
        } else {
            // `foldr` over the inputs, right to left.  The order does not change
            // the sum -- the merge is sorted and addition is commutative -- but
            // it decides which input hits an entry's last unspent output, which
            // is the order `main` folds in.
            let mut accumulator: Option<Color> = None;

            // Each input contributes its ancestor's colour in proportion to the
            // amount it spends.  A total of zero has no proportions to speak of,
            // and nothing forbids a zero-value input, so those share equally
            // rather than dividing by it.
            let total: f64 = inputs.iter().map(|i| i.amount as f64).sum();
            let share = |input: &sexp::Input| -> f64 {
                if total > 0.0 {
                    input.amount as f64 / total
                } else {
                    1.0 / inputs.len() as f64
                }
            };

            for i in (0..inputs.len()).rev() {
                let previous = inputs[i].prev_tx_id;
                let weight = share(&inputs[i]);
                let entry = colors.get_mut(&previous).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "transaction {} spends unknown transaction {}",
                            record.tx_id, previous
                        ),
                    )
                })?;
                let unspent = entry.1;

                if unspent > 1 {
                    // Others can still reach this colour, so it has to survive
                    // the fold: merge from a borrow.
                    entry.1 = unspent - 1;
                    let held = &entry.0;
                    accumulator = Some(match accumulator.take() {
                        // First step: the accumulator *is* this input's share of
                        // it.  A single-input transaction has weight 1 and
                        // `scale` shares rather than rebuilding, so the
                        // commonest shape costs nothing.
                        None => store.scale(held, weight),
                        // The accumulator already carries its own share, so it
                        // comes in at full strength.
                        Some(acc) => {
                            let combined = store.combine(held, weight, &acc, 1.0);
                            store.release(acc);
                            combined
                        }
                    });
                } else {
                    // The last unspent output is being spent right now, so
                    // nobody can reach this colour again and it may be taken
                    // outright.
                    let (root, _) = colors.remove(&previous).expect("just looked it up");
                    accumulator = Some(match accumulator.take() {
                        None => {
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

        line.clear();
        push_int(&mut line, record.tx_id);
        line.push(b',');
        push_fixed(&mut line, mean(&store, &color));
        line.push(b'\n');
        out.write_all(&line)?;

        if record.outputs > 0 {
            if let Some((displaced, _)) = colors.insert(record.tx_id, (color, record.outputs)) {
                store.release(displaced);
            }
        } else {
            store.release(color);
        }

        records += 1;
    }

    out.flush()
}

/// `sum_b b . weight(b)`, the block id a colour sits at on average.
///
/// Divided by the weights rather than trusting them to sum to 1.  They do sum to
/// 1 -- that is the invariant `--stats` reports drift against -- but the
/// division is one instruction per *record* against a walk of the whole colour,
/// and it makes the answer a weighted mean by construction instead of by
/// argument.
///
/// A colour with no terms cannot come out of the fold above: every colour starts
/// as a single block and a union never empties one.  Answering 0 rather than a
/// NaN is what to do with a case that does not arise.
fn mean(store: &WeightedSets, color: &Color) -> f64 {
    let (mut moment, mut total) = (0.0f64, 0.0f64);
    store.for_each_term(color, |block, weight| {
        moment += block as f64 * weight;
        total += weight;
    });
    if total > 0.0 {
        moment / total
    } else {
        0.0
    }
}

/// A non-negative value to [`PLACES`] fixed decimals, straight into the line
/// buffer.
///
/// Fixed rather than shortest-round-trip so the column lines up, and done in
/// integers for the reason [`push_int`] exists: `write!`'s formatting machinery
/// is a real cost once it runs a million times.  Scaling by a power of ten and
/// printing two integers is one multiply and one rounding.
///
/// The scaled value has to fit a `u64`, which at six places leaves room for
/// block ids up to about 1.8e13 -- some ten million times the length of the
/// chain, so the cast is not a limit anything will reach.
fn push_fixed(out: &mut Vec<u8>, value: f64) {
    let scale = 10u64.pow(PLACES);
    let units = (value * scale as f64).round() as u64;

    push_int(out, (units / scale) as usize);
    out.push(b'.');

    // Zero-padded to a fixed width, which `push_int` will not do -- it prints 5
    // as "5" where this needs "000005".
    let mut fraction = units % scale;
    let mut digits = [b'0'; PLACES as usize];
    let mut i = digits.len();
    while fraction > 0 {
        i -= 1;
        digits[i] = b'0' + (fraction % 10) as u8;
        fraction /= 10;
    }
    out.extend_from_slice(&digits);
}

/// Decimal, straight into the line buffer.  `main`'s, for the same reason it has
/// one.
fn push_int(out: &mut Vec<u8>, value: usize) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A record in the shape [`sexp`] reads: the header's block and tx id, then
    /// `(prev-tx, amount)` an input, then how many outputs.
    fn record(block: usize, tx: usize, spends: &[(usize, usize)], outputs: usize) -> String {
        let inputs: String = spends
            .iter()
            .map(|(prev, amount)| format!("(7 {} {} 0)", amount, prev))
            .collect();
        let outs: String = (0..outputs).map(|_| "(7 1 0)".to_string()).collect();
        format!("((1 {} {} 0 0 0 0) ({}) ({}))\n", block, tx, inputs, outs)
    }

    fn means(records: &str, limit: usize) -> Vec<String> {
        let mut out = Vec::new();
        run(records.as_bytes(), &mut out, limit).expect("the records are well formed");
        String::from_utf8(out)
            .expect("the output is decimal digits and punctuation")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// Nothing was spent, so the block that minted it holds all of the weight
    /// and the mean is that block, exactly.
    #[test]
    fn a_coinbase_sits_on_its_own_block() {
        assert_eq!(means(&record(7, 0, &[], 1), usize::MAX), ["0,7.000000"]);
    }

    /// Half the value from block 0 and half from block 3 puts the mean halfway
    /// between them -- the case the whole program exists to compute.
    #[test]
    fn two_equal_inputs_land_between_their_blocks() {
        let records = record(0, 0, &[], 1) + &record(3, 1, &[], 1) + &record(5, 2, &[(0, 50), (1, 50)], 1);
        assert_eq!(means(&records, usize::MAX).last().unwrap(), "2,1.500000");
    }

    /// Weighting is by amount, not by input count: nine tenths of the value
    /// coming from block 0 pulls the mean nine tenths of the way to it.
    #[test]
    fn the_mean_follows_the_value_not_the_inputs() {
        let records = record(0, 0, &[], 1) + &record(10, 1, &[], 1) + &record(11, 2, &[(0, 90), (1, 10)], 1);
        assert_eq!(means(&records, usize::MAX).last().unwrap(), "2,1.000000");
    }

    /// A chain of single-input transactions carries its ancestor's mean along
    /// unchanged: one input takes all of the weight, so there is nothing to mix
    /// with.
    #[test]
    fn a_single_input_inherits_the_mean() {
        let mut records = record(0, 0, &[], 1) + &record(4, 1, &[], 1) + &record(5, 2, &[(0, 50), (1, 50)], 1);
        for tx in 3..8 {
            records += &record(5 + tx, tx, &[(tx - 1, 100)], 1);
        }
        let lines = means(&records, usize::MAX);
        assert!(lines[3..].iter().all(|l| l.ends_with(",2.000000")), "{:?}", lines);
    }

    /// Nothing forbids a record whose inputs are all worth nothing, and it must
    /// not divide by the total: the inputs share equally instead.
    #[test]
    fn inputs_worth_nothing_share_equally() {
        let records = record(0, 0, &[], 1) + &record(6, 1, &[], 1) + &record(9, 2, &[(0, 0), (1, 0)], 1);
        assert_eq!(means(&records, usize::MAX).last().unwrap(), "2,3.000000");
    }

    /// The limit is a record count, and it stops the reader rather than the
    /// printing -- so a short run is a prefix of a long one.
    #[test]
    fn the_limit_takes_a_prefix() {
        let records = record(0, 0, &[], 1) + &record(1, 1, &[], 1) + &record(2, 2, &[], 1);
        assert_eq!(means(&records, 2), ["0,0.000000", "1,1.000000"]);
        assert!(means(&records, 0).is_empty());
    }

    /// Spending a transaction nobody has seen is the input's mistake, not a
    /// colour this program can invent.
    #[test]
    fn spending_an_unknown_transaction_is_an_error() {
        let mut out = Vec::new();
        let error = run(record(0, 9, &[(42, 5)], 1).as_bytes(), &mut out, usize::MAX)
            .expect_err("transaction 42 was never read");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("unknown transaction 42"), "{}", error);
    }

    /// The fixed format pads the fraction and does not pad the integer part.
    #[test]
    fn the_fixed_format_pads_only_the_fraction() {
        let mut line = Vec::new();
        push_fixed(&mut line, 0.5);
        push_fixed(&mut line, 12.0);
        push_fixed(&mut line, 900_000.000_001_5);
        assert_eq!(
            String::from_utf8(line).unwrap(),
            "0.50000012.000000900000.000002"
        );
    }
}
