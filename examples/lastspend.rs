//! # When is each transaction spent for the last time?
//!
//! The driver holds a colour for every transaction with an unspent output,
//! because any of those outputs might still be spent.  Over the 2022 chain that
//! is 92,735,490 colours at the end and 92,753,575 at peak, and it is the
//! multiplier in the memory wall: the store is that count times the size of a
//! colour, and no representation of a colour makes the product fit.
//!
//! But "has an unspent output" is the wrong question when the whole file is in
//! hand.  What the fold actually needs is *will anything later in this file
//! read this colour again* — and 51,815,075 of the 136 million unspent outputs
//! at the end of the chain are `OP_RETURN`, provably unspendable and never
//! spent by any of the 2,044,897,328 inputs in the file, while millions more
//! are simply never spent within it.  Every colour held for one of those is
//! held for nothing.
//!
//! Answering the right question drops the peak from 92,753,575 colours to
//! 19,361,261 — **4.79x** — which is what makes `--bands 1024` comfortable and
//! `--bands 4096` possible at all.
//!
//! ## What this writes
//!
//! One `u32` per transaction: the index of the record that spends it last, or
//! [`NEVER`] if nothing in the file ever spends it.  778,613,438 transactions
//! is 3.11 GB, which sounds like a lot and is nothing beside the tens or
//! hundreds of gigabytes it saves.
//!
//! The obvious alternative is a bit per input — "is this input the last read of
//! its source" — at 2.045e9 bits, 256 MB, twelve times smaller.  It is not
//! worth it: producing it needs a second pass over the 150 GB file, because a
//! bit cannot be written until the future is known, where this needs one; and
//! the driver has to answer the same question either way, so the only thing the
//! bitmap buys is 2.9 GB of address space in a program whose problem is that it
//! wants hundreds.
//!
//! ## Why one pass is enough
//!
//! `last[t]` is simply the largest record index that spends `t`, and records
//! arrive in order, so assigning `last[t] = i` at every spend leaves exactly
//! that.  Nothing has to be revisited and nothing has to be sorted.
//!
//! ## Use
//!
//! ```text
//! cargo run --release --example lastspend -- /data/bitcoin/2022/lastspend.u32 \
//!   < /data/bitcoin/2022/finalBCUTXO_2022.scm
//! ```
//!
//! then hand the file to the driver as `--release-oracle <file>`.  The driver
//! checks that its record count matches and refuses a file written for a
//! different input, since a stale oracle would free a colour that is still
//! wanted and quietly produce wrong answers.

use std::io::{self, BufWriter, Write};

// The driver's own reader, so that this tool and the fold agree about what a
// record is by construction rather than by a second implementation that has to
// be kept in step.  Most of what they bring is unused here -- an oracle needs
// the ids and nothing else -- which is what the allow is for.
#[allow(dead_code)]
#[path = "../src/sexp.rs"]
mod sexp;
#[allow(dead_code)]
#[path = "../src/simd.rs"]
mod simd;

/// A transaction nothing in the file ever spends.
pub const NEVER: u32 = u32::MAX;

/// What the driver looks for before trusting the file.
const MAGIC: &[u8; 8] = b"LASTSPN1";

fn main() -> io::Result<()> {
    let path = match std::env::args().nth(1) {
        Some(p) => p,
        None => {
            eprintln!("usage: lastspend <file.u32> < records");
            return Ok(());
        }
    };

    let mut reader = sexp::Reader::new(io::stdin().lock());
    let mut inputs: Vec<sexp::Input> = Vec::new();
    // Indexed by transaction id, which is dense over the file.  Grown as ids
    // arrive rather than sized up front, so this works on a prefix too.
    let mut last: Vec<u32> = Vec::new();
    let mut records: u64 = 0;

    while let Some(record) = reader.next_record(&mut inputs)? {
        let index = u32::try_from(records).map_err(|_| {
            io::Error::other("more than 4 billion records; the oracle would need a u64")
        })?;
        for input in &inputs {
            let t = input.prev_tx_id;
            if t >= last.len() {
                // A record that spends a transaction the file has not defined
                // is the driver's error to report, not this tool's -- it only
                // has to not panic on the way past.
                last.resize(t + 1, NEVER);
            }
            last[t] = index;
        }
        if record.tx_id >= last.len() {
            last.resize(record.tx_id + 1, NEVER);
        }
        records += 1;
        if records % 50_000_000 == 0 {
            eprintln!("  {} records", records);
        }
    }

    let never = last.iter().filter(|&&v| v == NEVER).count();
    let mut out = BufWriter::with_capacity(1 << 22, std::fs::File::create(&path)?);
    out.write_all(MAGIC)?;
    out.write_all(&records.to_le_bytes())?;
    out.write_all(&(last.len() as u64).to_le_bytes())?;
    // Little-endian on every target this is read on, and the driver checks the
    // magic before believing any of it.
    for &v in &last {
        out.write_all(&v.to_le_bytes())?;
    }
    out.flush()?;

    eprintln!(
        "lastspend: {} records, {} transactions, {} never spent ({:.1}%), {} bytes",
        records,
        last.len(),
        never,
        100.0 * never as f64 / last.len().max(1) as f64,
        24 + 4 * last.len()
    );
    Ok(())
}
