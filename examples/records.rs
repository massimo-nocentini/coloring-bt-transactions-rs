//! # A corpus of transaction records, for measuring against
//!
//! Every performance claim in this crate — the split between folding and
//! formatting that `emit` exists because of, the batch bounds it picked, the
//! `--sum` refusal, the reader thread in `prefetch` — was measured over records
//! of a particular shape.  Without the records those numbers are assertions.
//! This writes them.
//!
//! It is not a Bitcoin simulator and does not try to be.  What it reproduces is
//! the one property the measurements turn on: **how big a colour gets**.  A
//! colour is the set of blocks a transaction's coins descend from, so its size
//! is decided by how far back into the unspent set a transaction reaches when
//! it spends.  That is `--window`, and it is the whole knob:
//!
//! - `--window 0` — a transaction spends only outputs minted in its own block,
//!   so every colour is a single block and stays that way.  The fold is almost
//!   free and a line is ten bytes, which is the regime where the *channel* is
//!   the cost: dispatching per record here was 2.4x slower than serial, and it
//!   is what the batch bounds in `emit` were chosen against.
//! - `--window 4000` — a transaction reaches back across some hundreds of
//!   earlier transactions, so ancestry mixes, colours grow through the run and
//!   reach a few thousand blocks by the end.  This is the regime the formatting
//!   split was measured in, and the one a real chain resembles.
//!
//! Real records are neither exactly; they are the reason to have both.  A
//! design justified only at one end of this knob is a design that has not been
//! measured.
//!
//! ## Determinism
//!
//! The generator is a plain xorshift seeded from `--seed`, so a given
//! `(records, window, per-block, seed)` is one exact file on every machine and
//! every version of this crate.  That is the point: a number in a docstring
//! names the corpus it came from, and anyone can make that corpus again.  No
//! `rand` dependency, for the same reason the rest of the crate avoids them.
//!
//! ## Use
//!
//! ```text
//! cargo run --release --example records -- --window 4000 > records
//! cargo run --release --example records -- --window 0     > flat
//! ```
//!
//! or `make corpus`, which writes both at the size the measurements used.
//!
//! Every transaction spends only transactions that appear earlier in the
//! stream and have an unspent output left, which is what the driver requires —
//! it refuses a record that spends a transaction it has not seen.

use std::io::{self, BufWriter, Write};

/// Transactions per block, counting the coinbase.  Around the ratio the
//  default 1,000,001-record run shows against its block count.
const DEFAULT_PER_BLOCK: usize = 7;

/// Records to write unless told otherwise: the driver's own default limit, so
/// a corpus of this size is exactly one full default run.
const DEFAULT_RECORDS: usize = 1_000_001;

const USAGE: &str = "usage: records [--records <n>] [--window <n>] \
                     [--per-block <n>] [--seed <n>] > file";

/// xorshift64, so a corpus is a function of its seed and nothing else.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Uniform on `0..n`.  Modulo, whose bias is around one part in 2^64 for
    /// any `n` this is asked for and so is beneath noticing here.
    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

/// A transaction with outputs nobody has spent yet, and how many are left.
struct Unspent {
    tx: usize,
    left: usize,
}

fn option(args: &[String], i: usize, name: &str) -> Option<(String, usize)> {
    let rest = args[i].strip_prefix(name)?;
    if rest.is_empty() {
        return args.get(i + 1).map(|v| (v.clone(), 2));
    }
    rest.strip_prefix('=').map(|v| (v.to_string(), 1))
}

fn main() -> std::process::ExitCode {
    let mut records = DEFAULT_RECORDS;
    let mut window = 4_000usize;
    let mut per_block = DEFAULT_PER_BLOCK;
    let mut seed = 0x9E37_79B9_7F4A_7C15u64;

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        let numbers: [(&str, &mut usize); 3] = [
            ("--records", &mut records),
            ("--window", &mut window),
            ("--per-block", &mut per_block),
        ];
        let mut taken = 0;
        for (name, slot) in numbers {
            if let Some((v, used)) = option(&args, i, name) {
                match v.parse::<usize>() {
                    Ok(n) => *slot = n,
                    Err(_) => {
                        eprintln!("records: {} wants a count, got {:?}", name, v);
                        return std::process::ExitCode::FAILURE;
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
        if let Some((v, used)) = option(&args, i, "--seed") {
            match v.parse::<u64>() {
                Ok(n) => seed = n.max(1),
                Err(_) => {
                    eprintln!("records: --seed wants a number, got {:?}", v);
                    return std::process::ExitCode::FAILURE;
                }
            }
            i += used;
            continue;
        }
        eprintln!("{}", USAGE);
        return std::process::ExitCode::FAILURE;
    }

    if per_block == 0 {
        eprintln!("records: --per-block has to be at least 1, for the coinbase");
        return std::process::ExitCode::FAILURE;
    }

    let mut rng = Rng(seed);
    let mut out = BufWriter::with_capacity(1 << 20, io::stdout().lock());
    // Transactions with outputs left to spend, oldest first.  A spend picks
    // from the last `window` of these, which is what decides how far back
    // ancestry reaches and so how large a colour grows.
    let mut unspent: Vec<Unspent> = Vec::new();
    // Where this block's own transactions start in `unspent`, for `--window 0`:
    // there, a spend may only reach transactions minted in the same block, so
    // every colour is that one block and never grows.
    // Set at the top of every block, before the inner loop can read it.
    let mut block_starts_at;

    let (mut tx, mut block) = (0usize, 0usize);
    while tx < records {
        // The coinbase, which has no inputs and is coloured by its own block.
        // Enough outputs that the rest of the block has something to spend even
        // when the window is closed to it.
        let coinbase_outputs = if window == 0 { per_block * 2 } else { 2 };
        writeln!(
            out,
            "((1 {} {} 0 0 0 0) () ({}))",
            block,
            tx,
            outputs(coinbase_outputs)
        )
        .expect("stdout");
        block_starts_at = unspent.len();
        unspent.push(Unspent {
            tx,
            left: coinbase_outputs,
        });
        tx += 1;

        for _ in 1..per_block {
            if tx >= records {
                break;
            }
            // The floor a spend may reach back to: this block only when the
            // window is closed, otherwise `window` transactions back.
            let floor = if window == 0 {
                block_starts_at
            } else {
                unspent.len().saturating_sub(window)
            };
            if floor >= unspent.len() {
                break;
            }

            // One input usually, two or three often enough that the fold has
            // real merges to do and the weighted shares are not all 1.
            let inputs = 1 + rng.below(4).min(rng.below(3));
            let mut spends: Vec<(usize, usize)> = Vec::new();
            for _ in 0..inputs {
                if floor >= unspent.len() {
                    break;
                }
                let at = floor + rng.below(unspent.len() - floor);
                let entry = &mut unspent[at];
                let prev = entry.tx;
                entry.left -= 1;
                if entry.left == 0 {
                    // Spent out.  Swapping with the last entry keeps the
                    // removal O(1); it disturbs the order, which only decides
                    // which transactions a window reaches, not whether the
                    // records are well formed.
                    unspent.swap_remove(at);
                    if block_starts_at > unspent.len() {
                        block_starts_at = unspent.len();
                    }
                }
                // An amount worth weighting by: never zero, and spread widely
                // enough that the shares of a multi-input fold differ.
                spends.push((prev, 1 + rng.below(100_000_000)));
            }
            if spends.is_empty() {
                break;
            }

            let ins: String = spends
                .iter()
                .map(|(prev, amount)| format!("(7 {} {} 0)", amount, prev))
                .collect();
            let n = 2 + rng.below(3);
            writeln!(out, "((1 {} {} 0 0 0 0) ({}) ({}))", block, tx, ins, outputs(n))
                .expect("stdout");
            unspent.push(Unspent { tx, left: n });
            tx += 1;
        }
        block += 1;
    }

    out.flush().expect("stdout");
    eprintln!(
        "records: {} transactions in {} blocks, window {}",
        tx, block, window
    );
    std::process::ExitCode::SUCCESS
}

/// `n` outputs, which the driver reads only for there being one.
fn outputs(n: usize) -> String {
    "(7 1 0)".repeat(n)
}
