//! # The matrix, written once and swept over
//!
//! The article ends on a promise: *"a binary CSR of A, about 19.5 GB, would
//! make every future solve a sweep over it rather than a re-parse of 150 GB."*
//! At 200 MB/s that re-parse is twelve and a half minutes of nothing but
//! reading, before a single colour is merged, and it is paid again by every run
//! — every `--bands`, every projection, every experiment that changes what is
//! emitted and not what is folded.
//!
//! This is that file.  It is called FOLDMAT, it holds exactly what the fold
//! reads and nothing else, and over the 2022 chain it is about **10.6 GB** —
//! not 19.5, and exact where the CSR is not.
//!
//! ## What the fold actually reads
//!
//! Five things, and [`sexp`] says so in its own doc comment: the
//! header's `block-id` and `tx-id`, each input's `amount` and `prev-tx-id`, and
//! how many outputs there are.  A record is 193 bytes of text on average —
//! 149,968,404,213 bytes over 778,613,440 records is 192.61 — and four of those
//! five are a small minority of it.  So the budget, measured over the whole
//! chain rather than guessed:
//!
//! | section | bytes | what it is |
//! |---|---:|---|
//! | columns | 5,317,886,057 | 2,044,897,328 parents, as within-row zigzag chain deltas in file order |
//! | amounts | 4,475,343,790 | 1,475,397,681 satoshi values (rows with >= 2 inputs), zigzag deltas |
//! | heads | 778,613,440 + escapes | one byte a row: input count and output count, a nibble each |
//! | blocks | ~762,261 | one varint a coinbase row, and nothing on the other 777.8 million |
//! | groups | 3,041,472 | 16 bytes per 4096 rows, 0.03% |
//!
//! ## Why not the CSR the article costed
//!
//! `u32 row_ptr[n+1] + u32 col + f32 val` over 778,613,438 transactions and
//! 2,044,897,328 nonzeros is 19,473,632,380 bytes, and every one of those
//! numbers was checked by writing it — `--csr` below writes it still, because
//! an argument about lossiness that is never run is just an argument.  What the
//! run says:
//!
//! - an `f32` weight has a worst relative error of 5.96e-8, and only **28.10%**
//!   of the chain's weights round-trip at all;
//! - a row's weights are supposed to sum to one, and `|sum(w) - 1|` degrades
//!   from 1.02e-13 to 5.96e-8;
//! - replaying the fold with `f32` weights changes **44.71%** of `--sum` lines
//!   and **98.92%** of `--terms` coefficients.
//!
//! A colour is a distribution, and 44.71% of the answers moving is not a
//! rounding difference, it is a different file.  The fix is not a wider float:
//! it is to stop storing the weight at all.  The weight is `amount / total`,
//! both integers, and storing the raw `u64` satoshi amounts is **exact and
//! smaller** — 4.48 GB of zigzag deltas against 8.18 GB of `f32` — because a
//! chain of amounts inside one row is nothing like uniformly distributed and a
//! float is 32 bits of entropy by construction.  Ten decimal digits of a
//! satoshi amount cost fewer bits than a float that cannot represent it.
//!
//! ## Two transforms that were measured and refused
//!
//! Both would have made the file smaller.  Both are reserved as flag bits that
//! a reader must refuse, so that nobody re-derives them later and quietly gets
//! different colours:
//!
//! - **Sorted columns.** Sorting each row's parents ascending would shorten the
//!   delta chain.  It also changes the output: the fold walks a row's inputs
//!   from the last to the first (`src/main.rs`, `for i in (0..inputs.len()).rev()`)
//!   and reassociating an `f64` sum is not a no-op — **4.06%** of `--sum` lines
//!   change.  File order and multiplicity are part of the answer.
//! - **Deduplicated parents.** A row that spends the same transaction twice
//!   could store it once with a count.  But the multiplicity is what the fold
//!   counts down: without an oracle a colour is freed when its unspent count
//!   reaches zero, and with one [`oracle::Oracle::last_reads`](crate::oracle::Oracle::last_reads)
//!   frees on the *lowest* input index naming a transaction precisely because
//!   the reverse walk reaches it last.  Collapsing the duplicates frees the
//!   colour at the wrong instant, which is a lookup failure at best.
//!
//! ## Only a coinbase row carries a block
//!
//! There are 762,261 blocks and 762,261 zero-input rows, one per block, records
//! arrive in block order and `block_id` never decreases — so every other
//! record's block is simply the block the last coinbase opened.  Storing it per
//! record would cost a varint on 777.8 million rows to repeat what the reader
//! already knows.  The writer refuses a record whose block is not the open one
//! rather than dropping it silently, and `--check` proves the reconstruction
//! field for field against the source.
//!
//! ## A single-input row stores no amount
//!
//! 73.14% of rows have exactly one input, and for those the amount is dead
//! weight.  The `share` closure in `run` (`src/main.rs`) computes it as —
//! named rather than pointed at with a line range, which rots the next time
//! anything above it moves:
//!
//! ```text
//! let total = inputs.iter().map(|i| i.amount as f64).sum();
//! if total > 0.0 { input.amount / total } else { 1.0 / inputs.len() as f64 }
//! ```
//!
//! With one input, `total` *is* that input's amount: a non-zero amount takes
//! the first branch and gives `a / a == 1.0` exactly, and a zero amount takes
//! the second and gives `1.0 / 1.0 == 1.0`.  Both branches are exactly 1.0, for
//! every amount, so the value is not recoverable and does not need to be.  The
//! reader fills [`FILLED_AMOUNT`] — 1, not 0, so that anything else summing a
//! row's amounts sees a total it can divide by rather than falling into the
//! equal-share branch by accident.  This saves 569,499,647 values, the majority
//! of the file's inputs, at the cost of one claim that `--check` re-tests on
//! every record it verifies.
//!
//! ## The in-flight magic
//!
//! The magic is written `FOLDMATW` and overwritten with `FOLDMAT1` as the
//! writer's last act, so a killed write is refused rather than half-trusted.
//! `examples/lastspend.rs` writes a 3.11 GB file and does no such thing, and
//! that is not an inconsistency:
//!
//! - `lastspend` knows its counts before it writes anything, writes the header
//!   first and then a fixed-size array, and [`oracle::Oracle::load`](crate::oracle::Oracle::load)
//!   reads that array with one `read_exact` — a truncated oracle fails on the
//!   spot, in the first millisecond, by construction.
//! - FOLDMAT is a stream of self-delimiting rows.  A truncated one parses
//!   perfectly for however many million rows did get written and fails at the
//!   byte where they stop, which may be an hour into a fold, and its counts and
//!   its `GROUPS` index will all look plausible until then.
//! - And it costs 20 minutes and 10.6 GB to make, which is exactly the pressure
//!   that makes someone want to believe a file that is nearly right.
//!
//! ## One pass, and the copy it costs
//!
//! `TXFIX` and `GROUPS` are indexes over the rows, and they sit *before* the
//! rows — that is what the header's rows offset is for.  Neither is knowable
//! until the last record has been read, and the record count is not known in
//! advance either (the input is a pipe), so the two honest choices are:
//!
//! - read the 150 GB twice, once to size the indexes and once to write the
//!   rows: 12.5 minutes of extra reading; or
//! - write the rows to a spill file beside the destination, then write the
//!   header and the indexes and copy the rows in behind them.
//!
//! [`Writer`] does the second.  The copy is `std::io::copy` between two `File`s,
//! which on Linux is `copy_file_range` and never leaves the kernel.  On the
//! 20,000,000-record prefix below it moves 207,073,872 bytes — the spill alone,
//! which is the finished file's 207,152,079 less the 78,207 bytes of header and
//! indexes written in front of it — and the same file
//! through `cp(1)` on the same filesystem takes 0.19s, 0.43s and 0.63s over
//! three runs — so the whole chain's 10.6 GB is on the order of ten to thirty
//! seconds, against a pass whose parse alone took 7.0s for 3.19 GB.  What it
//! does cost is disk: the rows exist twice for the length of the copy, 21.2 GB
//! at the peak, and the spill is removed only on success — a killed run leaves
//! `<file>.rows` behind on purpose, next to a destination that still says
//! `FOLDMATW`.
//!
//! ## GROUPS is for the thread that is already there
//!
//! Varints are serial: byte *n* cannot be decoded without knowing where row
//! *r* began.  `GROUPS` breaks that every 4096 rows, giving the byte offset of
//! the group's first row and the nonzeros before it, for 16 bytes a group — 3
//! MB over the chain, 0.03% of the file.  With it the decode can be split
//! across threads, or hidden entirely on the one thread
//! [`prefetch`](crate::prefetch) already spends on parsing: a batch boundary
//! falls between records there, and a group boundary is a record boundary with
//! its offsets written down.  Every group's offset and nonzero count is also
//! *asserted* on the way past by [`Reader::next_record`], so the index is
//! checked by the sweep that does not need it rather than trusted by the one
//! that does.
//!
//! ## What the prefix runs said
//!
//! Everything above was written against the whole-chain survey's numbers and
//! then run on prefixes of the file it surveyed, `head -n <n>` of
//! `finalBCUTXO_2022.scm`, because the 150 GB pass is not this module's to
//! launch.  What those runs printed:
//!
//! - **20,000,000 records**, 3,189,416,993 bytes of text, written in 8.8s:
//!   **207,152,079 bytes**, 10.36 a record and 4.90 a nonzero, 15.4x smaller
//!   than the text it came from.  Columns are 40.7% of it, amounts 49.3%,
//!   heads 9.8%, blocks 0.1%.  `GROUPS` is 4,883 entries and 78,128 bytes,
//!   0.038%.
//! - `--check` over the same 20,000,000 records: **field for field in 10.4s**,
//!   every block id reconstructed from the coinbase rows alone agreeing with
//!   the file's own, and the content hash agreeing with the header's.
//! - `TXFIX` on any prefix past record 142,842 is **4 entries**, at rows
//!   142,783, 142,784, 142,841 and 142,842 — the two BIP-30 duplicate
//!   coinbases and the two steps back onto the sequence.  That is the whole
//!   chain's four non-consecutive steps, in 12 bytes.
//! - `--structure` over 5,000,000 records is 24,417,836 bytes against the
//!   compact form's 47,267,272: the amounts really are half of it.
//! - `--csr` over the same 20,000,000: 418,253,692 bytes, **2.02x this
//!   format**, with 31.87% of its weights round-tripping, a worst relative
//!   error of 5.960e-8, and a row's weights drifting from 1.976e-14 off one in
//!   `f64` to 5.914e-8 in `f32`.  The chain-wide figure is 28.10%; a prefix is
//!   kinder because early rows have fewer inputs and simpler ratios.
//! - The parse is most of the cost and always was: `examples/lastspend.rs`,
//!   which parses the same 20,000,000 records and does almost nothing with
//!   them, takes 7.0s against this writer's 8.8s.  The encoding, the spill and
//!   the copy together are 1.8s on top of a 7.0s parse.
//! - Killed 2s into a write with `SIGKILL`: a 64-byte destination reading
//!   `FOLDMATW`, a 46,137,244-byte `<file>.rows` beside it, and both `--stats`
//!   and `--check` refusing it by name.
//!
//! ## What checks the matrix when the records are not there
//!
//! `--check` is field for field against the source, and it needs the source:
//! 150 GB and twelve and a half minutes of reading, which is the cost this file
//! exists to stop paying.  A run that folds a matrix pays none of it and, until
//! [`Reader::verify`], checked none of it either — the header has carried a
//! content hash since the first version and no reader ever read it.  Measured on
//! a 20,000-record matrix: **102,220 of the 347,416 single-bit flips inside
//! `ROWS`** are accepted by the reader, folded, and answered with exit 0, an
//! empty standard error and a different answer.  `--verify` is the sweep that
//! closes that with the matrix alone, and it refuses all 102,220; over the whole
//! chain it is 110s and 5.4 MB resident against `--check`'s 150 GB.
//! [`Reader::verify`] says what the hashing costs and why the fold does not pay
//! it.
//!
//! ## Use
//!
//! ```text
//! cargo run --release --example matrix -- /data/bitcoin/2022/A.foldmat \
//!   < /data/bitcoin/2022/finalBCUTXO_2022.scm
//! cargo run --release --example matrix -- --check /data/bitcoin/2022/A.foldmat \
//!   < /data/bitcoin/2022/finalBCUTXO_2022.scm
//! cargo run --release --example matrix -- --verify /data/bitcoin/2022/A.foldmat
//! ```

// The driver reads a matrix now -- that is what `--matrix` is -- but it never
// writes one: the writer half exists for `examples/matrix.rs`, which is a
// separate crate that includes this file by path and so compiles its own copy
// of every item rather than sharing the binary's.  Thirteen dead-code warnings
// without this, every one of them the writer, and every one about which crate
// an item was compiled into rather than about the code.
#![allow(dead_code)]

use crate::sexp;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

/// What a reader looks for before believing a byte of the file.
pub const MAGIC: &[u8; 8] = b"FOLDMAT1";

/// What the file says while it is being written.
pub const MAGIC_IN_FLIGHT: &[u8; 8] = b"FOLDMATW";

/// The header, fixed.
pub const HEADER_BYTES: u64 = 64;

/// Rows per entry of the `GROUPS` index.
pub const GROUP: u64 = 4096;

/// Bit 0 of the flags: amounts are stored.  Clear under `--structure`.
pub const FLAG_AMOUNTS: u64 = 1 << 0;

/// Bit 1, reserved and refused: a row's columns sorted ascending rather than in
/// file order.  Measured, and it changes 4.06% of `--sum` lines.
pub const FLAG_SORTED_COLUMNS: u64 = 1 << 1;

/// Bit 2, reserved and refused: repeated parents collapsed to one column with a
/// multiplicity.  Measured, and it frees colours at the wrong instant.
pub const FLAG_DEDUPED_PARENTS: u64 = 1 << 2;

/// Everything a reader here knows how to honour.  A file setting anything else
/// is refused whole; see [`Header::decode`].
const FLAGS_KNOWN: u64 = FLAG_AMOUNTS;

/// The amount a reader fills in for an input whose row stored none.
///
/// Any value at all gives a weight of exactly 1.0 on a one-input row (see the
/// module doc), so this is chosen for what it does to a reader that was not
/// asking about weights: a total of 1 divides, a total of 0 does not.
pub const FILLED_AMOUNT: usize = 1;

fn bad<T>(msg: String) -> io::Result<T> {
    Err(io::Error::new(io::ErrorKind::InvalidData, msg))
}

/// The most a reader will reserve up front on a count the file handed it.
///
/// A count in a header or in an index is a number the *file* chose, and
/// `Vec::with_capacity` on a number the file chose is an abort no caller can
/// catch.  Measured on the release binary, on files of 72 and 73 bytes: a
/// `records` field of 2^50 dies with "memory allocation of 18014398509481984
/// bytes failed" and SIGABRT, and one of 2^60 + 7 dies with a capacity-overflow
/// panic out of `RawVec` -- both of them before a single row has been read.
///
/// The loops that fill these vectors all bail cleanly at end of file, so a count
/// is never more than a hint about how much to allocate at once, and a clamp
/// keeps exactly the hint and drops exactly the trust.  What it costs on a
/// genuine file is a handful of doublings: the whole chain's `GROUPS` is 190,092
/// entries, so three reallocations of a vector that was going to reach 3 MB
/// anyway, against a 20-minute write and a 10.6 GB sweep.
const RESERVE: usize = 1 << 16;

// -------------------------------------------------------------------------
// Varints
// -------------------------------------------------------------------------

/// Append `v` as unsigned LEB128.
#[inline]
pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

/// Fold a signed value onto the non-negative ones, so that a small negative
/// delta is a small number: `(n << 1) ^ (n >> 63)`.
#[inline]
pub fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

/// The inverse of [`zigzag`].
#[inline]
pub fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

/// Append `v` zigzagged and then as a varint.
#[inline]
pub fn put_zigzag(out: &mut Vec<u8>, v: i64) {
    put_varint(out, zigzag(v));
}

// -------------------------------------------------------------------------
// The content hash
// -------------------------------------------------------------------------

/// FNV-1a over 64-bit fields.
///
/// Not a checksum of the bytes: of the *values*.  A checksum of the bytes would
/// say the file was written intact, which the length and the magic already say;
/// this says the file means what the records meant, which is the only question
/// `--check` is asking.  It is therefore computable from either side without
/// encoding anything, and one line of `--check` compares three of them.
#[derive(Clone, Copy)]
pub struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv::new()
    }
}

impl Fnv {
    pub fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    /// Absorb one field, little-endian, a byte at a time.
    #[inline]
    pub fn field(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub fn value(&self) -> u64 {
        self.0
    }
}

/// Absorb one record's stored fields, in the order a row stores them.
///
/// The order is the reader's own: the transaction, its shape, the block if it
/// opens one, the parents in file order, the amounts if this file has them.
/// What is *not* stored is not hashed — a `--structure` file and a compact one
/// over the same records have different hashes on purpose, since they do not
/// hold the same thing.
#[inline]
pub fn hash_record(h: &mut Fnv, record: &sexp::Record, inputs: &[sexp::Input], amounts: bool) {
    h.field(record.tx_id as u64);
    h.field(inputs.len() as u64);
    h.field(record.outputs as u64);
    if inputs.is_empty() {
        h.field(record.block_id as u64);
    }
    for input in inputs {
        h.field(input.prev_tx_id as u64);
    }
    if amounts && inputs.len() >= 2 {
        for input in inputs {
            h.field(input.amount as u64);
        }
    }
}

// -------------------------------------------------------------------------
// The header
// -------------------------------------------------------------------------

/// The 64 bytes at the front, and everything a file says about itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Rows in the file.
    pub records: u64,
    /// One past the largest transaction id.  Differs from `records` by the
    /// BIP-30 duplicate coinbases: 778,613,438 against 778,613,440.
    pub transactions: u64,
    /// Inputs altogether, which is the nonzero count of `A`.
    pub nonzeros: u64,
    /// One past the largest block id.
    pub blocks: u64,
    pub flags: u64,
    /// FNV-1a over every stored field in fold order; see [`hash_record`].
    pub hash: u64,
    /// Where `ROWS` starts, which is 64 plus `TXFIX` plus `GROUPS`.
    pub rows_offset: u64,
}

impl Header {
    pub fn amounts(&self) -> bool {
        self.flags & FLAG_AMOUNTS != 0
    }

    /// The 64 bytes, under whichever magic the caller is at.
    pub fn encode(&self, magic: &[u8; 8]) -> [u8; 64] {
        let mut out = [0u8; 64];
        out[0..8].copy_from_slice(magic);
        out[8..16].copy_from_slice(&self.records.to_le_bytes());
        out[16..24].copy_from_slice(&self.transactions.to_le_bytes());
        out[24..32].copy_from_slice(&self.nonzeros.to_le_bytes());
        out[32..40].copy_from_slice(&self.blocks.to_le_bytes());
        out[40..48].copy_from_slice(&self.flags.to_le_bytes());
        out[48..56].copy_from_slice(&self.hash.to_le_bytes());
        out[56..64].copy_from_slice(&self.rows_offset.to_le_bytes());
        out
    }

    /// Read the header back, refusing everything that cannot be honoured.
    ///
    /// `records_expected` is how many records the run that is about to use this
    /// will read, when the caller genuinely knows the count -- which is the
    /// whole of the parameter's contract, and the reason most callers pass
    /// `None`.  Longer than the run is safe and shorter is not, so this is an
    /// inequality rather than a match, the same reasoning as
    /// [`oracle::Oracle::load`](crate::oracle::Oracle::load): a matrix written
    /// for the whole file, used on a prefix, is simply stopped early, and that
    /// is what lets one matrix serve every prefix of the same file.
    ///
    /// What a shorter matrix does *not* do is run out of rows in the middle of
    /// the fold, which is what this doc used to say.  Running out is exactly
    /// what the text reader does at end of file, and the fold answers over what
    /// it read; a short matrix is a short run and nothing worse.  So the refusal
    /// below is right only for a caller holding a real number -- one that has
    /// counted the records or been told them -- and a caller passing on a
    /// *limit* it has not reached is asking this to refuse a matrix that would
    /// have answered.  A run capped at 1,000,001 records over a nine-row matrix
    /// is a nine-row answer, not an error.
    pub fn decode(bytes: &[u8; 64], records_expected: Option<u64>) -> io::Result<Header> {
        if &bytes[..8] == MAGIC_IN_FLIGHT {
            return bad(
                "the magic still says the write is in flight: this file was left \
                 behind by a run that was killed before it finished, and the rows \
                 it does have stop somewhere in the middle"
                    .to_string(),
            );
        }
        if &bytes[..8] != MAGIC {
            return bad("not a FOLDMAT matrix".to_string());
        }
        let field = |a: usize| u64::from_le_bytes(bytes[a..a + 8].try_into().unwrap());
        let header = Header {
            records: field(8),
            transactions: field(16),
            nonzeros: field(24),
            blocks: field(32),
            flags: field(40),
            hash: field(48),
            rows_offset: field(56),
        };

        let unknown = header.flags & !FLAGS_KNOWN;
        if unknown != 0 {
            // Named rather than numbered where we know the name, because the
            // two we know about are the two that were deliberately not done and
            // a file setting one of them was written by something that decided
            // otherwise.
            let what = if unknown & FLAG_SORTED_COLUMNS != 0 {
                "bit 1, which would mean the columns were sorted rather than in \
                 file order -- a transform that was measured and refused, since \
                 it changes 4.06% of --sum lines"
                    .to_string()
            } else if unknown & FLAG_DEDUPED_PARENTS != 0 {
                "bit 2, which would mean repeated parents were collapsed -- \
                 measured and refused, since it frees colours at the wrong instant"
                    .to_string()
            } else {
                format!("bits {:#x}, which this build has no name for", unknown)
            };
            return bad(format!(
                "the matrix sets flag {}; a reader that ignored it would fold \
                 something other than what the file holds",
                what
            ));
        }

        if header.rows_offset < HEADER_BYTES {
            return bad(format!(
                "the rows are said to start at byte {}, inside the {}-byte header",
                header.rows_offset, HEADER_BYTES
            ));
        }
        if header.records > 0 && header.transactions == 0 {
            return bad("a matrix with rows and no transactions".to_string());
        }

        if let Some(expected) = records_expected {
            if header.records < expected {
                return bad(format!(
                    "written for {} records and this run reads {}; the caller \
                     asked for a matrix that covers the run and this one stops \
                     short of it",
                    header.records, expected
                ));
            }
        }
        Ok(header)
    }
}

// -------------------------------------------------------------------------
// Writing
// -------------------------------------------------------------------------

/// The rows, and the two indexes over them, built in one pass.
///
/// This is the half of the writer that does not care where the bytes go:
/// [`Writer`] points it at a spill file, and the tests point it at a `Vec<u8>`
/// so that a round trip needs no filesystem at all.
pub struct Encoder<W: Write> {
    rows: W,
    /// One row's bytes, reused.  A row is at most a few tens of kilobytes (the
    /// widest on the chain has 20,000 inputs) and this is refilled rather than
    /// reallocated.
    row: Vec<u8>,
    amounts: bool,

    records: u64,
    nonzeros: u64,
    transactions: u64,
    blocks: u64,
    rows_bytes: u64,

    /// Where each group of [`GROUP`] rows starts, and the nonzeros before it.
    groups: Vec<(u64, u64)>,
    /// The rows whose tx id is not one past the previous row's.
    txfix: Vec<(u64, i64)>,

    prev_tx: Option<usize>,
    open_block: Option<usize>,
    hash: Fnv,
    sections: Sections,
}

/// Where the rows' bytes went.
///
/// Not stored in the file -- it is derivable from the rows and nothing reads
/// it during a fold -- but it is what turns the budget in this module's doc
/// comment from an estimate into a run's own accounting, so the writer counts
/// it and `examples/matrix.rs` prints it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Sections {
    /// The head byte of every row, and the varints of the counts that escaped.
    pub heads: u64,
    /// The block delta on the coinbase rows.
    pub blocks: u64,
    /// The columns, which is where a structure-only matrix's bytes all are.
    pub columns: u64,
    /// The amounts, on rows with two inputs or more.
    pub amounts: u64,
}

impl<W: Write> Encoder<W> {
    pub fn new(rows: W, amounts: bool) -> Encoder<W> {
        Encoder {
            rows,
            row: Vec::with_capacity(1 << 16),
            amounts,
            records: 0,
            nonzeros: 0,
            transactions: 0,
            blocks: 0,
            rows_bytes: 0,
            groups: Vec::new(),
            txfix: Vec::new(),
            prev_tx: None,
            open_block: None,
            hash: Fnv::new(),
            sections: Sections::default(),
        }
    }

    /// Where the bytes written so far went.
    pub fn sections(&self) -> Sections {
        self.sections
    }

    pub fn records(&self) -> u64 {
        self.records
    }

    pub fn nonzeros(&self) -> u64 {
        self.nonzeros
    }

    /// One record, in the order they arrive.
    pub fn push(&mut self, record: &sexp::Record, inputs: &[sexp::Input]) -> io::Result<()> {
        // The group index is taken before the row is written, so a group's
        // offset is where its first row begins.
        if self.records % GROUP == 0 {
            self.groups.push((self.rows_bytes, self.nonzeros));
        }

        // The tx id is implicit -- one past the previous row's -- and TXFIX is
        // what is left over.  Chain-wide that is four entries, because two
        // BIP-30 duplicate coinbases make the id go backwards twice and it has
        // to come forward again afterwards; but the section is written from
        // what is observed, since a prefix or a synthetic file has its own.
        let expected = match self.prev_tx {
            None => 0usize,
            Some(p) => p + 1,
        };
        if record.tx_id != expected {
            self.txfix
                .push((self.records, record.tx_id as i64 - expected as i64));
        }
        self.prev_tx = Some(record.tx_id);
        self.transactions = self.transactions.max(record.tx_id as u64 + 1);

        hash_record(&mut self.hash, record, inputs, self.amounts);

        let k = inputs.len();
        let row = &mut self.row;
        row.clear();

        // The head: a nibble each, 15 meaning "a varint follows", the input
        // count's first when both escape.  73.14% of rows have one input and
        // most have a handful of outputs, so the commonest row is one byte.
        let lo = if k >= 15 { 15u8 } else { k as u8 };
        let hi = if record.outputs >= 15 { 15u8 } else { record.outputs as u8 };
        row.push((hi << 4) | lo);
        if k >= 15 {
            put_varint(row, k as u64);
        }
        if record.outputs >= 15 {
            put_varint(row, record.outputs as u64);
        }
        self.sections.heads += row.len() as u64;
        // Where the head ended, so that what follows can be charged to the
        // section it belongs to.
        let mark = row.len();

        if k == 0 {
            // A zero-input row is a coinbase and opens a block.  Block ids
            // never decrease and there is exactly one of these a block, so a
            // second row in the same block would mean one of those two facts is
            // not a fact.
            match self.open_block {
                None => put_varint(row, record.block_id as u64),
                Some(open) => {
                    if record.block_id <= open {
                        return bad(format!(
                            "record {} has no inputs, so it opens a block, but its \
                             block {} is not past the open block {}",
                            self.records, record.block_id, open
                        ));
                    }
                    put_varint(row, (record.block_id - open) as u64);
                }
            }
            self.open_block = Some(record.block_id);
            self.blocks = self.blocks.max(record.block_id as u64 + 1);
            self.sections.blocks += (row.len() - mark) as u64;
        } else {
            // Every other row's block is the open one, which is why no other
            // row stores it.  Refused rather than dropped: a file that lost a
            // block id would colour a coinbase with the wrong singleton and
            // nothing downstream would ever know.
            match self.open_block {
                Some(open) if open == record.block_id => {}
                Some(open) => {
                    return bad(format!(
                        "record {} is in block {} but the open block is {}; only a \
                         zero-input row carries a block, so this one would be lost",
                        self.records, record.block_id, open
                    ))
                }
                None => {
                    return bad(format!(
                        "record {} has inputs and no block has been opened; the \
                         records have to start at a coinbase for a block id to \
                         mean anything",
                        self.records
                    ))
                }
            }

            // Columns, in file order, multiplicity kept.  The first is stored
            // as the reach back from this transaction, and the rest as the step
            // from the one before -- which is what makes a consolidation of a
            // thousand nearby outputs cost about a byte an input.
            //
            // The first column is the one this reach-back encoding is an
            // argument about, since every other column is a zigzag step from its
            // predecessor and never reaches anywhere.  Measured chain-wide:
            // 69.57% of *first* inputs reach back under 1e5 records, against
            // 52.85% of all spends.  Both numbers are here because the second is
            // the one that gets quoted and the first is the one that matters.
            let first = inputs[0].prev_tx_id;
            if first >= record.tx_id {
                return bad(format!(
                    "transaction {} spends {}, which is not earlier than it",
                    record.tx_id, first
                ));
            }
            put_varint(row, (record.tx_id - first) as u64);
            for i in 1..k {
                if inputs[i].prev_tx_id >= record.tx_id {
                    return bad(format!(
                        "transaction {} spends {}, which is not earlier than it",
                        record.tx_id, inputs[i].prev_tx_id
                    ));
                }
                put_zigzag(
                    row,
                    inputs[i].prev_tx_id as i64 - inputs[i - 1].prev_tx_id as i64,
                );
            }

            self.sections.columns += (row.len() - mark) as u64;

            // Amounts, and only where a weight depends on them.
            if self.amounts && k >= 2 {
                let mark = row.len();
                for input in inputs {
                    if input.amount > i64::MAX as usize {
                        return bad(format!(
                            "an input of transaction {} is {} satoshi, which does not \
                             fit the signed delta the amounts are stored as",
                            record.tx_id, input.amount
                        ));
                    }
                }
                put_varint(row, inputs[0].amount as u64);
                for i in 1..k {
                    put_zigzag(row, inputs[i].amount as i64 - inputs[i - 1].amount as i64);
                }
                self.sections.amounts += (row.len() - mark) as u64;
            }
        }

        self.rows.write_all(row)?;
        self.rows_bytes += row.len() as u64;
        self.nonzeros += k as u64;
        self.records += 1;
        Ok(())
    }

    /// The header and the bytes that go between it and the rows.
    ///
    /// The sink comes back with it, because the caller is the only one who
    /// knows whether it has to be flushed, reopened or thrown away.
    pub fn finish(self) -> io::Result<(Header, Vec<u8>, W)> {
        let mut prefix = Vec::with_capacity(16 * self.groups.len() + 16);

        put_varint(&mut prefix, self.txfix.len() as u64);
        let mut previous = 0u64;
        for &(row, delta) in &self.txfix {
            put_varint(&mut prefix, row - previous);
            put_zigzag(&mut prefix, delta);
            previous = row;
        }

        put_varint(&mut prefix, self.groups.len() as u64);
        for &(offset, nonzeros) in &self.groups {
            prefix.extend_from_slice(&offset.to_le_bytes());
            prefix.extend_from_slice(&nonzeros.to_le_bytes());
        }

        let header = Header {
            records: self.records,
            transactions: self.transactions,
            nonzeros: self.nonzeros,
            blocks: self.blocks,
            flags: if self.amounts { FLAG_AMOUNTS } else { 0 },
            hash: self.hash.value(),
            rows_offset: HEADER_BYTES + prefix.len() as u64,
        };
        Ok((header, prefix, self.rows))
    }
}

/// A FOLDMAT file, written in one pass over the records.
///
/// The rows go to `<path>.rows` while the pass runs and are copied in behind
/// the header and the indexes at the end; see the module doc for why the
/// indexes cannot be written last and what the copy costs.
pub struct Writer {
    encoder: Encoder<io::BufWriter<File>>,
    path: String,
    spill_path: String,
}

impl Writer {
    pub fn create(path: &str, amounts: bool) -> io::Result<Writer> {
        // The destination exists from the first moment, saying it is in flight,
        // so that a run killed in the middle leaves a file that is refused
        // rather than one that is missing and might be mistaken for a run that
        // was never started.
        let mut out = File::create(path)?;
        let placeholder = Header {
            records: 0,
            transactions: 0,
            nonzeros: 0,
            blocks: 0,
            flags: if amounts { FLAG_AMOUNTS } else { 0 },
            hash: 0,
            rows_offset: HEADER_BYTES,
        };
        out.write_all(&placeholder.encode(MAGIC_IN_FLIGHT))?;
        out.sync_data()?;

        let spill_path = format!("{}.rows", path);
        let spill = io::BufWriter::with_capacity(1 << 22, File::create(&spill_path)?);
        Ok(Writer {
            encoder: Encoder::new(spill, amounts),
            path: path.to_string(),
            spill_path,
        })
    }

    pub fn push(&mut self, record: &sexp::Record, inputs: &[sexp::Input]) -> io::Result<()> {
        self.encoder.push(record, inputs)
    }

    pub fn records(&self) -> u64 {
        self.encoder.records()
    }

    pub fn sections(&self) -> Sections {
        self.encoder.sections()
    }

    /// Assemble the file and hand back what its header says.
    ///
    /// The last write is the magic, and nothing before it is a file anyone is
    /// allowed to believe.
    pub fn finish(self) -> io::Result<Header> {
        let (path, spill_path) = (self.path, self.spill_path);
        let (header, prefix, spill) = self.encoder.finish()?;
        let mut spill = spill.into_inner().map_err(|e| e.into_error())?;
        spill.flush()?;
        drop(spill);

        let mut out = std::fs::OpenOptions::new().write(true).open(&path)?;
        out.seek(SeekFrom::Start(0))?;
        out.write_all(&header.encode(MAGIC_IN_FLIGHT))?;
        out.write_all(&prefix)?;

        // Two `File`s, so on Linux this is `copy_file_range` and the bytes
        // never come into this process at all.
        let mut rows = File::open(&spill_path)?;
        let copied = io::copy(&mut rows, &mut out)?;
        drop(rows);
        out.flush()?;
        out.sync_data()?;

        // Only now: the file is complete, so it may say so.
        out.seek(SeekFrom::Start(0))?;
        out.write_all(MAGIC)?;
        out.sync_data()?;
        drop(out);
        std::fs::remove_file(&spill_path)?;

        let _ = copied;
        Ok(header)
    }
}

// -------------------------------------------------------------------------
// Reading
// -------------------------------------------------------------------------

/// A window onto the file, and the varints read out of it.
///
/// Our own rather than a `BufReader` for the reason [`sexp`] gives
/// for its own: the hot path is "look at one byte" 10.6 billion times and it
/// needs to inline down to a bounds check.
struct Bytes<R: Read> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    end: usize,
    consumed: u64,
}

impl<R: Read> Bytes<R> {
    fn new(inner: R) -> Bytes<R> {
        Bytes {
            inner,
            buf: vec![0u8; 1 << 20].into_boxed_slice(),
            pos: 0,
            end: 0,
            consumed: 0,
        }
    }

    #[inline]
    fn byte(&mut self) -> io::Result<Option<u8>> {
        if self.pos < self.end {
            let b = self.buf[self.pos];
            self.pos += 1;
            return Ok(Some(b));
        }
        self.refill()
    }

    #[cold]
    fn refill(&mut self) -> io::Result<Option<u8>> {
        self.consumed += self.end as u64;
        self.pos = 0;
        self.end = 0;
        loop {
            match self.inner.read(&mut self.buf) {
                Ok(0) => return Ok(None),
                Ok(n) => {
                    self.end = n;
                    self.pos = 1;
                    return Ok(Some(self.buf[0]));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn offset(&self) -> u64 {
        self.consumed + self.pos as u64
    }

    fn need(&mut self, what: &str) -> io::Result<u8> {
        match self.byte()? {
            Some(b) => Ok(b),
            None => bad(format!(
                "the file ends at byte {} in the middle of {}",
                self.offset(),
                what
            )),
        }
    }

    fn exact(&mut self, out: &mut [u8], what: &str) -> io::Result<()> {
        for slot in out.iter_mut() {
            *slot = self.need(what)?;
        }
        Ok(())
    }

    fn varint(&mut self, what: &str) -> io::Result<u64> {
        let mut value = 0u64;
        let mut shift = 0u32;
        loop {
            let b = self.need(what)?;
            if shift == 63 && b > 1 {
                return bad(format!(
                    "a varint at byte {} does not fit 64 bits ({})",
                    self.offset(),
                    what
                ));
            }
            value |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(value);
            }
            shift += 7;
            if shift > 63 {
                return bad(format!(
                    "a varint at byte {} does not fit 64 bits ({})",
                    self.offset(),
                    what
                ));
            }
        }
    }

    fn zigzag(&mut self, what: &str) -> io::Result<i64> {
        Ok(unzigzag(self.varint(what)?))
    }
}

/// What a sweep of a matrix alone found in it.
///
/// Every field is what the *rows* turned out to hold, not what the header said
/// -- [`Reader::verify`] has already refused the file if the two disagree, so
/// this is the header's numbers as re-derived rather than as read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verified {
    pub records: u64,
    pub nonzeros: u64,
    pub transactions: u64,
    pub blocks: u64,
    /// FNV-1a over every stored field, recomputed from the rows.
    pub hash: u64,
}

/// A FOLDMAT file, read the way the records were.
///
/// The surface is [`sexp::Reader`]'s on purpose: one `next_record` filling the
/// caller's `Vec<sexp::Input>` and answering a [`sexp::Record`], so that the
/// driver swaps a sweep over 10.6 GB for a re-parse of 150 GB by changing which
/// reader it constructs and nothing else.
pub struct Reader<R: Read> {
    src: Bytes<R>,
    header: Header,
    /// `(row, tx_id - expected)`, in row order.
    txfix: Vec<(u64, i64)>,
    fix: usize,
    groups: Vec<(u64, u64)>,
    produced: u64,
    nonzeros: u64,
    prev_tx: Option<usize>,
    open_block: Option<usize>,
    max_tx: u64,
    blocks_seen: u64,
    finished: bool,
}

impl Reader<File> {
    /// Open a matrix, refusing one that was not written for these records.
    ///
    /// A path has a length and a length is a bound, which is the one thing this
    /// knows that [`Reader::new`] over a pipe does not.  Every row costs at
    /// least its head byte and every nonzero at least the one byte of its
    /// column varint, so a file with `n` bytes after the rows offset cannot
    /// hold more than `n` rows or `n` nonzeros however large the header's
    /// counts say they are -- and the header's counts are what the reader
    /// reserves against.
    ///
    /// Only a regular file, though.  A FIFO and a character device both stat as
    /// zero bytes, and a bound of zero would refuse every matrix anyone ever
    /// piped through one; those get the same treatment as [`Reader::new`],
    /// which is the clamp at [`RESERVE`] and nothing else.
    pub fn open(path: &str, records_expected: Option<u64>) -> io::Result<Reader<File>> {
        let file = File::open(path)?;
        let bytes = match file.metadata() {
            Ok(meta) if meta.is_file() => Some(meta.len()),
            _ => None,
        };
        Reader::bounded(file, records_expected, bytes)
    }
}

impl<R: Read> Reader<R> {
    /// Read a matrix off anything at all, with no length to check it against.
    pub fn new(inner: R, records_expected: Option<u64>) -> io::Result<Reader<R>> {
        Reader::bounded(inner, records_expected, None)
    }

    /// The same, plus the source's length in bytes where there is one.
    ///
    /// `file_bytes` is `None` for a pipe, for a FIFO, and for anything else
    /// whose size a `stat` does not answer; see [`Reader::open`] for what it
    /// buys where it is `Some`.
    fn bounded(
        inner: R,
        records_expected: Option<u64>,
        file_bytes: Option<u64>,
    ) -> io::Result<Reader<R>> {
        let mut src = Bytes::new(inner);
        let mut head = [0u8; 64];
        src.exact(&mut head, "the header")?;
        let header = Header::decode(&head, records_expected)?;

        if let Some(bytes) = file_bytes {
            if header.rows_offset > bytes {
                return bad(format!(
                    "the header says the rows start at byte {} and the file is {} \
                     bytes long",
                    header.rows_offset, bytes
                ));
            }
            // A row is at least its head byte and a nonzero is at least the one
            // byte of a column varint, so the bytes left after the indexes bound
            // both counts.  This is the difference between a header that lies
            // and a header that lies about something a reader is going to
            // allocate against.
            let rows = bytes - header.rows_offset;
            if header.records > rows {
                return bad(format!(
                    "the header counts {} rows and there are {} bytes for them; a \
                     row costs at least the byte of its head",
                    header.records, rows
                ));
            }
            if header.nonzeros > rows {
                return bad(format!(
                    "the header counts {} nonzeros and there are {} bytes of rows; \
                     a column costs at least a byte",
                    header.nonzeros, rows
                ));
            }
        }

        let count = src.varint("the TXFIX count")?;
        if count > header.records {
            return bad(format!(
                "TXFIX names {} rows and the matrix has {}",
                count, header.records
            ));
        }
        let mut txfix = Vec::with_capacity(count.min(RESERVE as u64) as usize);
        let mut previous = 0u64;
        for i in 0..count {
            let step = src.varint("a TXFIX row")?;
            let delta = src.zigzag("a TXFIX delta")?;
            let row = match previous.checked_add(step) {
                Some(row) => row,
                None => {
                    return bad(format!(
                        "TXFIX steps {} rows on from row {}, which is past every \
                         row number there is",
                        step, previous
                    ))
                }
            };
            if i > 0 && step == 0 {
                return bad(format!("TXFIX names row {} twice", row));
            }
            if row >= header.records {
                return bad(format!(
                    "TXFIX names row {} and the matrix has {} rows",
                    row, header.records
                ));
            }
            if delta == 0 {
                return bad(format!("TXFIX fixes row {} to what it already was", row));
            }
            txfix.push((row, delta));
            previous = row;
        }

        let count = src.varint("the GROUPS count")?;
        let wanted = header.records.div_ceil(GROUP);
        if count != wanted {
            return bad(format!(
                "GROUPS has {} entries and {} rows want {}",
                count, header.records, wanted
            ));
        }
        let mut groups = Vec::with_capacity(count.min(RESERVE as u64) as usize);
        let mut buf = [0u8; 16];
        let (mut last_offset, mut last_nonzeros) = (0u64, 0u64);
        for g in 0..count {
            src.exact(&mut buf, "a GROUPS entry")?;
            let offset = u64::from_le_bytes(buf[0..8].try_into().unwrap());
            let nonzeros = u64::from_le_bytes(buf[8..16].try_into().unwrap());
            if g == 0 && (offset != 0 || nonzeros != 0) {
                return bad(format!(
                    "the first group starts {} bytes and {} nonzeros into the rows",
                    offset, nonzeros
                ));
            }
            if g > 0 && (offset <= last_offset || nonzeros < last_nonzeros) {
                return bad(format!("GROUPS entry {} does not move forward", g));
            }
            if nonzeros > header.nonzeros {
                return bad(format!(
                    "GROUPS entry {} counts {} nonzeros before it and the matrix has {}",
                    g, nonzeros, header.nonzeros
                ));
            }
            groups.push((offset, nonzeros));
            last_offset = offset;
            last_nonzeros = nonzeros;
        }

        if src.offset() != header.rows_offset {
            return bad(format!(
                "the indexes end at byte {} and the header says the rows start at {}",
                src.offset(),
                header.rows_offset
            ));
        }

        Ok(Reader {
            src,
            header,
            txfix,
            fix: 0,
            groups,
            produced: 0,
            nonzeros: 0,
            prev_tx: None,
            open_block: None,
            max_tx: 0,
            blocks_seen: 0,
            finished: false,
        })
    }

    pub fn header(&self) -> &Header {
        &self.header
    }

    pub fn records(&self) -> u64 {
        self.header.records
    }

    /// Whether the file holds the amounts a weighted fold shares colour by.
    pub fn amounts(&self) -> bool {
        self.header.amounts()
    }

    pub fn txfix(&self) -> &[(u64, i64)] {
        &self.txfix
    }

    pub fn groups(&self) -> &[(u64, u64)] {
        &self.groups
    }

    /// Refuse a structure-only matrix where the weights matter.
    ///
    /// A `--structure` file has no amounts, and a reader has to fill *something*
    /// -- it fills [`FILLED_AMOUNT`] for every input, which is an equal share.
    /// That is exactly right for `--rings` and `--sets`, whose `S::WEIGHTED` is
    /// a constant `false` and which never look at the field, and quietly wrong
    /// for anything that does.  So the weighted side asks first.
    pub fn require_amounts(&self) -> io::Result<()> {
        if self.amounts() {
            return Ok(());
        }
        bad(
            "this matrix was written with --structure, which keeps the shape of A \
             and none of its weights, and a weighted fold shares a colour out in \
             proportion to them; write one without --structure"
                .to_string(),
        )
    }

    /// The next record, or `None` once the file's own count is reached.
    ///
    /// [`sexp::Reader::next_record`]'s shape exactly: `inputs` is cleared and
    /// refilled, and the same three fields come back.
    pub fn next_record(&mut self, inputs: &mut Vec<sexp::Input>) -> io::Result<Option<sexp::Record>> {
        inputs.clear();
        if self.produced == self.header.records {
            if !self.finished {
                self.finished = true;
                self.check_tail()?;
            }
            return Ok(None);
        }

        // A group boundary is a record boundary with its offsets written down,
        // so the sweep that does not need the index is the one that checks it.
        if self.produced % GROUP == 0 {
            let g = (self.produced / GROUP) as usize;
            let (offset, nonzeros) = self.groups[g];
            let here = self.src.offset() - self.header.rows_offset;
            if here != offset || nonzeros != self.nonzeros {
                return bad(format!(
                    "GROUPS says row {} starts {} bytes and {} nonzeros into the rows, \
                     and it starts at {} and {}",
                    self.produced, offset, nonzeros, here, self.nonzeros
                ));
            }
        }

        let expected = match self.prev_tx {
            None => 0usize,
            Some(p) => p + 1,
        };
        let tx_id = if self.fix < self.txfix.len() && self.txfix[self.fix].0 == self.produced {
            let delta = self.txfix[self.fix].1;
            self.fix += 1;
            match i64::try_from(expected).ok().and_then(|e| e.checked_add(delta)) {
                Some(fixed) if fixed >= 0 => fixed as usize,
                Some(_) => {
                    return bad(format!("TXFIX puts row {} at a negative tx id", self.produced))
                }
                None => {
                    return bad(format!(
                        "TXFIX steps row {} by {} from transaction {}, which lands \
                         outside every transaction id there is",
                        self.produced, delta, expected
                    ))
                }
            }
        } else {
            expected
        };
        if tx_id as u64 >= self.header.transactions {
            return bad(format!(
                "row {} is transaction {} and the header says there are {}",
                self.produced, tx_id, self.header.transactions
            ));
        }
        self.prev_tx = Some(tx_id);
        self.max_tx = self.max_tx.max(tx_id as u64 + 1);

        let head = self.src.need("a row head")?;
        let mut k = (head & 0x0f) as usize;
        let mut outputs = (head >> 4) as usize;
        if k == 15 {
            k = self.src.varint("an escaped input count")? as usize;
        }
        if outputs == 15 {
            outputs = self.src.varint("an escaped output count")? as usize;
        }

        // A subtraction rather than `self.nonzeros + k > self.header.nonzeros`,
        // because that sum wraps.  Release sets `panic = "abort"` and so has no
        // overflow checks, and a spliced input count near `u64::MAX` used to
        // pass this guard by wrapping the left side to something small and then
        // reach `inputs.reserve` below as a request for every byte of address
        // space -- SIGABRT, exit 134, reproduced against a spliced copy of the
        // real whole-chain matrix.  The invariant this guard itself maintains,
        // `self.nonzeros <= self.header.nonzeros`, is what makes the
        // subtraction safe: it holds before the first row and every row that
        // passes here leaves it holding.
        if k as u64 > self.header.nonzeros - self.nonzeros {
            return bad(format!(
                "row {} takes the nonzeros past the {} the header counts",
                self.produced, self.header.nonzeros
            ));
        }

        let block_id = if k == 0 {
            let delta = self.src.varint("a block delta")?;
            let block = match self.open_block {
                None => delta as usize,
                Some(open) => {
                    if delta == 0 {
                        return bad(format!(
                            "row {} has no inputs, so it opens a block, and its delta \
                             leaves it in the open block {}",
                            self.produced, open
                        ));
                    }
                    // `open + delta` wraps in release, and this one is not a
                    // crash but a wrong answer: with `delta` at `u64::MAX` the
                    // sum is `open - 1`, so the block goes *backwards*, every
                    // count in the header still adds up, the file is accepted,
                    // and that block id becomes a colour.  Four coinbase rows
                    // spliced this way read back as blocks 0, 2, 1, 3.
                    match usize::try_from(delta).ok().and_then(|d| open.checked_add(d)) {
                        Some(block) => block,
                        None => {
                            return bad(format!(
                                "row {} steps {} blocks on from the open block {}, \
                                 which is past every block there is",
                                self.produced, delta, open
                            ))
                        }
                    }
                }
            };
            if block as u64 >= self.header.blocks {
                return bad(format!(
                    "row {} opens block {} and the header says there are {}",
                    self.produced, block, self.header.blocks
                ));
            }
            self.open_block = Some(block);
            self.blocks_seen = block as u64 + 1;
            block
        } else {
            match self.open_block {
                Some(open) => open,
                None => {
                    return bad(format!(
                        "row {} has inputs and no block has been opened",
                        self.produced
                    ))
                }
            }
        };

        if k > 0 {
            // Clamped, because `k` came out of the file: the loop below fills
            // the vector a push at a time and stops dead at end of file, so the
            // count is a hint about how much to allocate at once and nothing
            // more.  The widest row on the chain has 20,000 inputs, well inside
            // this, so a genuine file reallocates on no row at all.
            inputs.reserve(k.min(RESERVE));
            let gap = self.src.varint("a first column")?;
            if gap == 0 {
                return bad(format!(
                    "row {} spends transaction {} from itself",
                    self.produced, tx_id
                ));
            }
            if gap > tx_id as u64 {
                return bad(format!(
                    "row {} reaches {} back from transaction {}",
                    self.produced, gap, tx_id
                ));
            }
            let mut previous = tx_id - gap as usize;
            inputs.push(sexp::Input {
                prev_tx_id: previous,
                amount: FILLED_AMOUNT,
            });
            for _ in 1..k {
                let step = self.src.zigzag("a column step")?;
                // In `i128`, because both operands are the file's: `previous`
                // is bounded only by the header's transaction count and `step`
                // is a whole `i64`.  `previous` is never negative, so the sum
                // can only overflow upward, and in release it wraps to a
                // negative that the `next < 0` below happens to catch -- which
                // is the check doing the right thing for the wrong reason, and
                // in a debug build it does not get to do it at all: the add
                // panics first, so a malformed file aborts a checked build
                // instead of being refused by it.  Widening is what makes the
                // check the thing that decides.
                let next = previous as i128 + step as i128;
                if next < 0 || next as u128 >= tx_id as u128 {
                    return bad(format!(
                        "row {} spends {} from transaction {}",
                        self.produced, next, tx_id
                    ));
                }
                previous = next as usize;
                inputs.push(sexp::Input {
                    prev_tx_id: previous,
                    amount: FILLED_AMOUNT,
                });
            }

            if self.header.amounts() && k >= 2 {
                let first = self.src.varint("a first amount")?;
                if first > i64::MAX as u64 {
                    return bad(format!("row {} has an amount past 2^63", self.produced));
                }
                // `i128` for the running total, for the reason the columns
                // have it: the steps are the file's, the sum overflows upward
                // only, and release wraps it to a negative that the check below
                // catches by accident while debug panics before the check runs.
                // The writer refuses to store an amount past `i64::MAX`, so a
                // reader that arrives at one is reading a file this writer did
                // not write, and it should say so rather than abort.
                let mut amount = first as i128;
                inputs[0].amount = amount as usize;
                for i in 1..k {
                    let step = self.src.zigzag("an amount step")?;
                    amount += step as i128;
                    if amount < 0 {
                        return bad(format!(
                            "row {} steps to a negative amount at input {}",
                            self.produced, i
                        ));
                    }
                    if amount > i64::MAX as i128 {
                        return bad(format!(
                            "row {} steps to an amount past 2^63 at input {}",
                            self.produced, i
                        ));
                    }
                    inputs[i].amount = amount as usize;
                }
            }
        }

        self.nonzeros += k as u64;
        self.produced += 1;
        Ok(Some(sexp::Record {
            block_id,
            tx_id,
            outputs,
        }))
    }

    /// Sweep every row and hold the matrix against its own header.
    ///
    /// The one thing no fold does.  A fold reads the rows, believes them, and
    /// answers; every check it gets for free is a *structural* one -- a varint
    /// that does not fit, a column that reaches forward, a group whose offset is
    /// not where the sweep arrived -- and those are the checks a random bit flip
    /// mostly survives.  Measured on a 20,000-record matrix -- 43,427 bytes of
    /// rows -- by flipping every bit of `ROWS` one at a time and reading the
    /// result back: **102,220 of the 347,416 flips are accepted**, exit 0, empty
    /// standard error, a different answer, and this sweep refuses all 102,220.
    /// Through the driver rather than through the reader, on 2,000 records:
    /// **9,975 of 34,736 flips fold to exit 0 with an empty standard error, and
    /// 1,438 of those print a different `--sum`**.
    ///
    /// This closes that with the matrix alone and nothing else -- no 150 GB of
    /// source, which is what `--check` needs and what the format exists to stop
    /// re-reading.  It recomputes [`hash_record`] over the rows as they come
    /// back and holds it against the header's, and letting [`next_record`] run
    /// to the end runs the tail checks with it, so the nonzero, transaction and
    /// block counts, the exhaustion of `TXFIX` and the refusal of trailing bytes
    /// all happen on the way past.
    ///
    /// ## Why the fold does not pay for this and `--verify` does
    ///
    /// Not because the hash is expensive -- it is, and the numbers are below --
    /// but because it cannot save a fold from anything.  It is over every stored
    /// field of every row and so does not settle until the last one, and a
    /// fold's output is streamed: `--sum` and `--terms` write a line as each
    /// record closes.  A fold that hashed would print a whole wrong answer and
    /// then, after the last line of it, say the file was wrong.  Same bytes on
    /// standard output, same order, with a message underneath.  `--verify`
    /// prints nothing until it knows.
    ///
    /// And it is not free.  On the whole-chain matrix -- 10,601,433,514 bytes,
    /// 778,613,440 rows, 2,044,897,328 nonzeros -- with the page cache warm, in
    /// release, three runs of each and under a fifth of a second of spread:
    ///
    /// | sweep | wall |
    /// |---|---:|
    /// | rows decoded and thrown away | 63.62s, 63.64s, 63.49s |
    /// | rows decoded and hashed | 109.68s, 109.55s, 109.66s |
    ///
    /// The hash is **46.1s of it**: 72% on top of the decode, and 42% of what
    /// `--verify` costs altogether.  That is what absorbing a `u64` a field, a
    /// byte at a time, over about 5.86 billion fields comes to -- a nanosecond
    /// a round, which is FNV doing exactly what FNV does.  Paying it on every
    /// fold, to be told at the end what one sweep establishes once, is the trade
    /// this refuses.  The first run on this machine, cold, was 113.7s and 5.4 MB
    /// resident.
    ///
    /// [`next_record`]: Reader::next_record
    pub fn verify(&mut self) -> io::Result<Verified> {
        if self.produced != 0 {
            return bad(format!(
                "this reader is {} rows into the matrix, and a content hash is \
                 over all of them or over nothing",
                self.produced
            ));
        }
        let amounts = self.amounts();
        let mut hash = Fnv::new();
        let mut inputs: Vec<sexp::Input> = Vec::new();
        while let Some(record) = self.next_record(&mut inputs)? {
            hash_record(&mut hash, &record, &inputs, amounts);
        }
        if hash.value() != self.header.hash {
            return bad(format!(
                "the rows hash to {:#018x} and the header says {:#018x}; every \
                 count in the header adds up, so this is the file having changed \
                 under itself rather than having been written wrong",
                hash.value(),
                self.header.hash
            ));
        }
        Ok(Verified {
            records: self.produced,
            nonzeros: self.nonzeros,
            transactions: self.max_tx,
            blocks: self.blocks_seen,
            hash: hash.value(),
        })
    }

    /// What the header claimed, against what the rows turned out to be.
    fn check_tail(&mut self) -> io::Result<()> {
        if self.nonzeros != self.header.nonzeros {
            return bad(format!(
                "the header counts {} nonzeros and the rows hold {}",
                self.header.nonzeros, self.nonzeros
            ));
        }
        if self.max_tx != self.header.transactions {
            return bad(format!(
                "the header counts {} transactions and the rows reach {}",
                self.header.transactions, self.max_tx
            ));
        }
        if self.blocks_seen != self.header.blocks {
            return bad(format!(
                "the header counts {} blocks and the rows open {}",
                self.header.blocks, self.blocks_seen
            ));
        }
        if self.fix != self.txfix.len() {
            return bad(format!(
                "TXFIX has {} entries and {} were reached",
                self.txfix.len(),
                self.fix
            ));
        }
        if self.src.byte()?.is_some() {
            return bad(format!(
                "the rows end at byte {} and the file goes on",
                self.src.offset() - 1
            ));
        }
        Ok(())
    }
}

// -------------------------------------------------------------------------
// Tests
// -------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A record and its inputs, in the shape a test writes them.
    #[derive(Debug)]
    struct Row {
        block_id: usize,
        tx_id: usize,
        outputs: usize,
        inputs: Vec<sexp::Input>,
    }

    fn coinbase(block_id: usize, tx_id: usize, outputs: usize) -> Row {
        Row {
            block_id,
            tx_id,
            outputs,
            inputs: Vec::new(),
        }
    }

    fn row(block_id: usize, tx_id: usize, outputs: usize, spends: &[(usize, usize)]) -> Row {
        Row {
            block_id,
            tx_id,
            outputs,
            inputs: spends
                .iter()
                .map(|&(prev_tx_id, amount)| sexp::Input {
                    prev_tx_id,
                    amount,
                })
                .collect(),
        }
    }

    /// Write a set of rows to bytes, the way [`Writer`] assembles a file but
    /// with no filesystem in it.
    fn write(rows: &[Row], amounts: bool) -> Vec<u8> {
        let mut encoder = Encoder::new(Vec::new(), amounts);
        for r in rows {
            let record = sexp::Record {
                block_id: r.block_id,
                tx_id: r.tx_id,
                outputs: r.outputs,
            };
            encoder.push(&record, &r.inputs).expect("a writable row");
        }
        let (header, prefix, body) = encoder.finish().expect("a finished matrix");
        let mut file = Vec::new();
        file.extend_from_slice(&header.encode(MAGIC));
        file.extend_from_slice(&prefix);
        file.extend_from_slice(&body);
        file
    }

    /// Read a whole file back into the shape it was written from.
    fn read(file: &[u8]) -> io::Result<(Header, Vec<Row>)> {
        let mut reader = Reader::new(file, None)?;
        let header = *reader.header();
        let mut inputs = Vec::new();
        let mut out = Vec::new();
        while let Some(record) = reader.next_record(&mut inputs)? {
            out.push(Row {
                block_id: record.block_id,
                tx_id: record.tx_id,
                outputs: record.outputs,
                inputs: inputs.clone(),
            });
        }
        Ok((header, out))
    }

    /// A little chain: three blocks, a coinbase each, rows spending backwards
    /// with lopsided amounts, one row spending the same parent twice, one row
    /// with a zero amount, one with a huge one.
    fn synthetic() -> Vec<Row> {
        vec![
            coinbase(0, 0, 1),
            row(0, 1, 2, &[(0, 5_000_000_000)]),
            row(0, 2, 1, &[(1, 7), (0, 11)]),
            coinbase(1, 3, 1),
            row(1, 4, 3, &[(3, 1), (2, 2), (3, 3)]),
            row(1, 5, 1, &[(4, 0), (4, 0)]),
            coinbase(7, 6, 1),
            row(7, 7, 2, &[(6, 2_100_000_000_000_000), (5, 1)]),
            row(7, 8, 1, &[(7, 42)]),
        ]
    }

    /// The message a refusal gave, and a panic if it did not refuse.
    ///
    /// `unwrap_err` wants the success type to be `Debug`, and a `Reader` holds a
    /// megabyte of window that has nothing to say.
    fn refusal<T>(result: io::Result<T>) -> String {
        match result {
            Ok(_) => panic!("expected a refusal and got a file"),
            Err(e) => e.to_string(),
        }
    }

    /// Field for field, against what the format promises to keep.
    ///
    /// Everything but the amounts comes back identical.  The amounts come back
    /// identical where they are stored -- `amounts` set and two inputs or more
    /// -- and as [`FILLED_AMOUNT`] where they are not, which is the claim the
    /// module doc argues and this is the assertion of.
    fn same(a: &[Row], b: &[Row], amounts: bool) {
        assert_eq!(a.len(), b.len(), "record count");
        for (i, (x, y)) in a.iter().zip(b).enumerate() {
            assert_eq!(x.block_id, y.block_id, "block of record {}", i);
            assert_eq!(x.tx_id, y.tx_id, "tx id of record {}", i);
            assert_eq!(x.outputs, y.outputs, "outputs of record {}", i);
            let columns: Vec<usize> = x.inputs.iter().map(|i| i.prev_tx_id).collect();
            let back: Vec<usize> = y.inputs.iter().map(|i| i.prev_tx_id).collect();
            assert_eq!(columns, back, "columns of record {}", i);
            if amounts && x.inputs.len() >= 2 {
                assert_eq!(x.inputs, y.inputs, "amounts of record {}", i);
            } else {
                assert!(
                    y.inputs.iter().all(|i| i.amount == FILLED_AMOUNT),
                    "record {} stores no amount, so it reads back filled",
                    i
                );
            }
        }
    }

    /// Field for field, with the amounts in.
    #[test]
    fn a_written_matrix_reads_back_as_what_was_written() {
        let rows = synthetic();
        let file = write(&rows, true);
        let (header, back) = read(&file).expect("a readable matrix");

        assert_eq!(header.records, 9);
        assert_eq!(header.transactions, 9);
        assert_eq!(header.nonzeros, 11);
        assert_eq!(header.blocks, 8, "one past the largest block id, not the count");
        assert!(header.amounts());
        same(&rows, &back, true);
    }

    /// Where the bytes went, which is the module doc's budget on nine rows.
    #[test]
    fn the_sections_add_up_to_the_rows() {
        let rows = synthetic();
        let mut encoder = Encoder::new(Vec::new(), true);
        for r in &rows {
            let record = sexp::Record {
                block_id: r.block_id,
                tx_id: r.tx_id,
                outputs: r.outputs,
            };
            encoder.push(&record, &r.inputs).unwrap();
        }
        let sections = encoder.sections();
        let (header, prefix, body) = encoder.finish().unwrap();
        assert_eq!(
            sections.heads + sections.blocks + sections.columns + sections.amounts,
            body.len() as u64,
            "every byte of a row is in exactly one section"
        );
        assert_eq!(sections.heads, 9, "no count here reaches 15, so a head is a byte");
        assert_eq!(sections.blocks, 3, "one varint on each of the three coinbases");
        assert_eq!(
            header.rows_offset,
            HEADER_BYTES + prefix.len() as u64,
            "and the rows start where the header says"
        );
    }

    /// The one-input rows carry no amount, and the fold cannot tell.
    ///
    /// The claim in the module doc, tested as the fold states it: `total` is the
    /// single amount, so the share is 1.0 whichever branch it takes.
    #[test]
    fn a_single_input_rows_weight_is_one_whatever_its_amount_was() {
        let share = |amounts: &[usize]| -> Vec<f64> {
            let total: f64 = amounts.iter().map(|&a| a as f64).sum();
            amounts
                .iter()
                .map(|&a| {
                    if total > 0.0 {
                        a as f64 / total
                    } else {
                        1.0 / amounts.len() as f64
                    }
                })
                .collect()
        };
        // What the source said, against what the reader fills in.
        for source in [1usize, 7, 0, 5_000_000_000, 2_100_000_000_000_000] {
            assert_eq!(share(&[source]), vec![1.0], "amount {}", source);
        }
        assert_eq!(share(&[FILLED_AMOUNT]), vec![1.0]);

        // And end to end: a file whose one-input rows had wild amounts reads
        // back with the filler, and every weight is unchanged.
        let rows = vec![
            coinbase(0, 0, 1),
            row(0, 1, 1, &[(0, 5_000_000_000)]),
            row(0, 2, 1, &[(1, 0)]),
        ];
        let (_, back) = read(&write(&rows, true)).expect("a readable matrix");
        assert_eq!(back[1].inputs[0].amount, FILLED_AMOUNT);
        assert_eq!(back[2].inputs[0].amount, FILLED_AMOUNT);
        for r in &back[1..] {
            let amounts: Vec<usize> = r.inputs.iter().map(|i| i.amount).collect();
            assert_eq!(share(&amounts), vec![1.0]);
        }
    }

    /// `--structure`: the shape of `A` and none of its weights.
    #[test]
    fn a_structure_matrix_keeps_every_column_and_no_amount() {
        let rows = synthetic();
        let file = write(&rows, false);
        let (header, back) = read(&file).expect("a readable matrix");
        assert!(!header.amounts());
        assert!(file.len() < write(&rows, true).len(), "and it is smaller");

        for (before, after) in rows.iter().zip(&back) {
            assert_eq!(before.tx_id, after.tx_id);
            let columns: Vec<usize> = before.inputs.iter().map(|i| i.prev_tx_id).collect();
            let kept: Vec<usize> = after.inputs.iter().map(|i| i.prev_tx_id).collect();
            assert_eq!(columns, kept, "the columns survive, in file order");
            assert!(after.inputs.iter().all(|i| i.amount == FILLED_AMOUNT));
        }

        let reader = Reader::new(&file[..], None).unwrap();
        let refused = reader.require_amounts().unwrap_err().to_string();
        assert!(refused.contains("--structure"), "{}", refused);
    }

    /// The BIP-30 shape: two duplicate coinbases send the tx id backwards, and
    /// it has to come forward again after each.
    #[test]
    fn a_tx_id_that_goes_backwards_is_carried_by_txfix() {
        let rows = vec![
            coinbase(0, 0, 1),
            row(0, 1, 1, &[(0, 10)]),
            coinbase(1, 2, 1),
            // The duplicate: a coinbase whose transaction id is one already
            // used, which is what makes `tx_id == record index` false from
            // record 142,783 on.
            coinbase(2, 0, 1),
            // And the chain picks up where it left off.
            coinbase(3, 3, 1),
            row(3, 4, 1, &[(3, 1)]),
        ];
        let file = write(&rows, true);
        let (header, back) = read(&file).expect("a readable matrix");
        assert_eq!(header.records, 6);
        assert_eq!(header.transactions, 5, "one past the largest id, not the rows");
        same(&rows, &back, true);

        let reader = Reader::new(&file[..], None).unwrap();
        assert_eq!(
            reader.txfix(),
            &[(3, -3), (4, 2)],
            "one entry where the id jumps back and one where it returns"
        );
    }

    /// Both nibbles escape, separately and together.
    #[test]
    fn counts_of_fifteen_and_more_escape_into_a_varint() {
        // 15 is the escape value itself, so a row with exactly 15 of something
        // is the case that a naive `>` gets wrong.
        let mut rows = vec![coinbase(0, 0, 1)];
        let mut tx = 1;
        for &k in &[14usize, 15, 16, 300] {
            let spends: Vec<(usize, usize)> =
                (0..k).map(|i| (i % tx, 1 + i * 3)).collect();
            rows.push(row(0, tx, 1, &spends));
            tx += 1;
        }
        for &outs in &[14usize, 15, 16, 70_000] {
            rows.push(row(0, tx, outs, &[(0, 1)]));
            tx += 1;
        }
        // And both at once.
        let spends: Vec<(usize, usize)> = (0..40).map(|i| (i % tx, 7)).collect();
        rows.push(row(0, tx, 999, &spends));

        let file = write(&rows, true);
        let (header, back) = read(&file).expect("a readable matrix");
        assert_eq!(header.nonzeros, 14 + 15 + 16 + 300 + 4 + 40);
        same(&rows, &back, true);

        // Six of the ten rows escape a nibble and one escapes both, so the
        // heads are not one byte a row here.
        let mut encoder = Encoder::new(Vec::new(), true);
        for r in &rows {
            encoder
                .push(
                    &sexp::Record {
                        block_id: r.block_id,
                        tx_id: r.tx_id,
                        outputs: r.outputs,
                    },
                    &r.inputs,
                )
                .unwrap();
        }
        assert_eq!(
            encoder.sections().heads,
            rows.len() as u64 + (1 + 1 + 2) + (1 + 1 + 3) + (1 + 2),
            "a head byte a row, plus a varint for each count that reached 15"
        );
    }

    /// Multiplicity and file order are the answer, not an accident of it.
    #[test]
    fn repeated_parents_are_stored_as_many_times_as_they_are_spent() {
        let rows = vec![
            coinbase(0, 0, 3),
            // Descending, so nothing about this row is sorted, and the same
            // parent three times.
            row(0, 1, 1, &[(0, 5), (0, 4), (0, 3)]),
        ];
        let (_, back) = read(&write(&rows, true)).expect("a readable matrix");
        assert_eq!(back[1].inputs.len(), 3);
        assert_eq!(
            back[1].inputs.iter().map(|i| i.amount).collect::<Vec<_>>(),
            vec![5, 4, 3],
            "in the order the file gave them"
        );
    }

    /// The bytes of a row, spelled out, and the two refusals that need a
    /// hand-made file to reach.
    ///
    /// Nothing else in this module pins the layout: every other test writes and
    /// reads with the same code, which would agree with itself whatever it
    /// decided.  This one says what the format *is* -- two rows, four bytes --
    /// and then breaks each of two fields on purpose.
    #[test]
    fn a_row_is_the_bytes_the_format_says_it_is() {
        let rows = vec![coinbase(0, 0, 1), row(0, 1, 1, &[(0, 7)])];
        let file = write(&rows, true);
        let header = Header::decode(&file[..64].try_into().unwrap(), None).unwrap();

        // 64 header, then TXFIX (a count of 0) and GROUPS (a count of 1 and one
        // 16-byte entry of zeros).
        assert_eq!(header.rows_offset, 64 + 1 + 1 + 16);
        assert_eq!(file[64], 0, "no TXFIX entries");
        assert_eq!(file[65], 1, "one group");
        assert_eq!(
            &file[header.rows_offset as usize..],
            &[
                0x10, // one output, no inputs
                0x00, // and it opens block 0
                0x11, // one output, one input
                0x01, // which is one transaction back
            ],
            "four bytes for two records"
        );

        // A column that reaches back nothing is a row spending itself.
        let mut broken = file.clone();
        broken[header.rows_offset as usize + 3] = 0;
        let err = refusal(read(&broken));
        assert!(err.contains("from itself"), "{}", err);

        // And a GROUPS count that does not follow from the record count is a
        // file whose index is about some other rows.
        let mut broken = file.clone();
        broken[65] = 2;
        let err = refusal(read(&broken));
        assert!(err.contains("GROUPS has 2 entries"), "{}", err);
    }

    /// A killed write leaves a file that says so.
    #[test]
    fn the_in_flight_magic_is_refused() {
        let rows = synthetic();
        let mut file = write(&rows, true);
        file[..8].copy_from_slice(MAGIC_IN_FLIGHT);
        let err = refusal(read(&file));
        assert!(err.contains("in flight"), "{}", err);

        file[..8].copy_from_slice(b"LASTSPN1");
        let err = refusal(read(&file));
        assert!(err.contains("not a FOLDMAT matrix"), "{}", err);
    }

    /// And one cut short is refused at the row where the bytes stop, not
    /// believed as far as they go.
    #[test]
    fn a_truncated_matrix_is_refused() {
        let file = write(&synthetic(), true);
        // Cut in the rows: the header still counts nine.
        let short = &file[..file.len() - 3];
        let err = refusal(read(short));
        assert!(err.contains("ends at byte"), "{}", err);

        // Cut in the indexes, before a single row.
        let err = refusal(read(&file[..70]));
        assert!(err.contains("ends at byte"), "{}", err);

        // Cut inside the header.
        let err = refusal(read(&file[..40]));
        assert!(err.contains("ends at byte"), "{}", err);

        // And one byte too many is refused as well: the header's counts are
        // what says where the rows end.
        let mut long = file.clone();
        long.push(0);
        let err = refusal(read(&long));
        assert!(err.contains("the file goes on"), "{}", err);
    }

    /// A flag bit this build has no meaning for is a different format.
    #[test]
    fn an_unknown_flag_bit_is_refused() {
        let rows = synthetic();
        let base = write(&rows, true);

        for (bit, expect) in [
            (FLAG_SORTED_COLUMNS, "sorted"),
            (FLAG_DEDUPED_PARENTS, "collapsed"),
            (1u64 << 40, "no name for"),
        ] {
            let mut file = base.clone();
            let flags = u64::from_le_bytes(file[40..48].try_into().unwrap()) | bit;
            file[40..48].copy_from_slice(&flags.to_le_bytes());
            let err = refusal(read(&file));
            assert!(err.contains(expect), "bit {:#x}: {}", bit, err);
        }
    }

    /// The inequality [`oracle`](crate::oracle) argues for, in the other
    /// direction: longer than the run is what a whole-file matrix is.
    #[test]
    fn a_matrix_longer_than_the_run_serves_it_and_a_shorter_one_does_not() {
        let file = write(&synthetic(), true);
        assert!(Reader::new(&file[..], Some(9)).is_ok());
        assert!(Reader::new(&file[..], Some(4)).is_ok(), "a prefix of the file");
        let err = refusal(Reader::new(&file[..], Some(10)));
        assert!(err.contains("stops short of it"), "{}", err);
    }

    /// Every count in the header is checked against the rows, and the GROUPS
    /// index is checked on the way past.
    #[test]
    fn a_header_that_disagrees_with_the_rows_is_refused() {
        let rows = synthetic();
        let base = write(&rows, true);

        let mut file = base.clone();
        let n = u64::from_le_bytes(file[24..32].try_into().unwrap());
        file[24..32].copy_from_slice(&(n + 1).to_le_bytes());
        let err = refusal(read(&file));
        assert!(err.contains("nonzeros"), "{}", err);

        let mut file = base.clone();
        let t = u64::from_le_bytes(file[16..24].try_into().unwrap());
        file[16..24].copy_from_slice(&(t + 1).to_le_bytes());
        let err = refusal(read(&file));
        assert!(err.contains("transactions"), "{}", err);

        let mut file = base.clone();
        let b = u64::from_le_bytes(file[32..40].try_into().unwrap());
        file[32..40].copy_from_slice(&(b + 1).to_le_bytes());
        let err = refusal(read(&file));
        assert!(err.contains("blocks"), "{}", err);
    }

    /// The GROUPS index is not decoration: a wrong offset is caught by the
    /// sweep that never reads it.
    #[test]
    fn a_group_offset_that_lies_is_caught_on_the_way_past() {
        // 4097 rows, so there are two groups and the second one has an offset
        // worth getting wrong.
        let mut rows = vec![coinbase(0, 0, 1)];
        for tx in 1..4098 {
            rows.push(row(0, tx, 1, &[(tx - 1, tx)]));
        }
        let file = write(&rows, true);
        let (header, back) = read(&file).expect("a readable matrix");
        assert_eq!(header.records, 4098);
        same(&rows, &back, true);

        let reader = Reader::new(&file[..], None).unwrap();
        assert_eq!(reader.groups().len(), 2);
        let (offset, nonzeros) = reader.groups()[1];
        assert_eq!(nonzeros, 4095, "the first group holds 4096 rows, one of them a coinbase");

        // Move the second group's offset one byte on.  It is the last 16
        // bytes before the rows start.
        let mut broken = file.clone();
        let group_at = (header.rows_offset as usize) - 16;
        broken[group_at..group_at + 8].copy_from_slice(&(offset + 1).to_le_bytes());
        let err = refusal(read(&broken));
        assert!(err.contains("GROUPS says row 4096"), "{}", err);
    }

    /// What the writer refuses, since a matrix that dropped one of these would
    /// be wrong rather than unreadable.
    #[test]
    fn the_writer_refuses_what_the_format_cannot_hold() {
        let mut encoder = Encoder::new(Vec::new(), true);
        let record = sexp::Record {
            block_id: 3,
            tx_id: 0,
            outputs: 1,
        };
        let err = encoder
            .push(
                &record,
                &[sexp::Input {
                    prev_tx_id: 0,
                    amount: 1,
                }],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("no block has been opened"), "{}", err);

        // A record whose block is not the open one: only a coinbase carries a
        // block, so this one would be lost.
        let mut encoder = Encoder::new(Vec::new(), true);
        encoder
            .push(
                &sexp::Record {
                    block_id: 0,
                    tx_id: 0,
                    outputs: 1,
                },
                &[],
            )
            .unwrap();
        let err = encoder
            .push(
                &sexp::Record {
                    block_id: 4,
                    tx_id: 1,
                    outputs: 1,
                },
                &[sexp::Input {
                    prev_tx_id: 0,
                    amount: 1,
                }],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("the open block is 0"), "{}", err);

        // A coinbase that does not open a new block.
        let mut encoder = Encoder::new(Vec::new(), true);
        for (block, tx) in [(5usize, 0usize), (5, 1)] {
            let record = sexp::Record {
                block_id: block,
                tx_id: tx,
                outputs: 1,
            };
            let pushed = encoder.push(&record, &[]);
            if tx == 1 {
                let err = pushed.unwrap_err().to_string();
                assert!(err.contains("not past the open block"), "{}", err);
            } else {
                pushed.unwrap();
            }
        }

        // And a spend of something not earlier, which the whole delta scheme
        // rests on: prev_tx_id < tx_id, 0 violations chain-wide.
        let mut encoder = Encoder::new(Vec::new(), true);
        encoder
            .push(
                &sexp::Record {
                    block_id: 0,
                    tx_id: 0,
                    outputs: 1,
                },
                &[],
            )
            .unwrap();
        let err = encoder
            .push(
                &sexp::Record {
                    block_id: 0,
                    tx_id: 1,
                    outputs: 1,
                },
                &[sexp::Input {
                    prev_tx_id: 1,
                    amount: 1,
                }],
            )
            .unwrap_err()
            .to_string();
        assert!(err.contains("not earlier than it"), "{}", err);
    }

    /// The hash is over the values, so it is the same computed from either
    /// side -- which is the whole of what `--check` does.
    #[test]
    fn the_content_hash_is_computable_from_the_records_alone() {
        let rows = synthetic();
        for amounts in [true, false] {
            let file = write(&rows, amounts);
            let header = Header::decode(&file[..64].try_into().unwrap(), None).unwrap();

            let mut hash = Fnv::new();
            for r in &rows {
                let record = sexp::Record {
                    block_id: r.block_id,
                    tx_id: r.tx_id,
                    outputs: r.outputs,
                };
                hash_record(&mut hash, &record, &r.inputs, amounts);
            }
            assert_eq!(header.hash, hash.value(), "amounts: {}", amounts);
        }

        // And a file that holds different things hashes differently.
        let with = Header::decode(&write(&rows, true)[..64].try_into().unwrap(), None).unwrap();
        let without = Header::decode(&write(&rows, false)[..64].try_into().unwrap(), None).unwrap();
        assert_ne!(with.hash, without.hash);
    }

    /// Round trip of the primitives, over the values that break them.
    #[test]
    fn varints_and_zigzags_round_trip() {
        let mut cases: Vec<u64> = vec![0, 1, 126, 127, 128, 129, 16_383, 16_384, u64::MAX];
        for shift in 0..64 {
            cases.push(1u64 << shift);
        }
        let mut buf = Vec::new();
        for &v in &cases {
            buf.clear();
            put_varint(&mut buf, v);
            let mut src = Bytes::new(&buf[..]);
            assert_eq!(src.varint("a test varint").unwrap(), v);
            assert!(src.byte().unwrap().is_none(), "and nothing left over");
        }
        for &v in &[0i64, 1, -1, 63, -64, i64::MIN, i64::MAX] {
            assert_eq!(unzigzag(zigzag(v)), v);
            buf.clear();
            put_zigzag(&mut buf, v);
            let mut src = Bytes::new(&buf[..]);
            assert_eq!(src.zigzag("a test zigzag").unwrap(), v);
        }
        // Eleven continuation bytes is not a u64 however you read it.
        let mut src = Bytes::new(&[0xffu8; 12][..]);
        assert!(src.varint("an overlong varint").is_err());
    }


    // ---------------------------------------------------------------------
    // Files the writer would never write
    // ---------------------------------------------------------------------

    /// A file assembled byte by byte rather than by [`Encoder`].
    ///
    /// Everything the writer produces is by construction something the reader
    /// accepts, so the refusals that only a malformed file reaches can only be
    /// tested from bytes.  `rows` is the `ROWS` section verbatim, `txfix` is
    /// written as the `(step, delta)` pairs given without any of the writer's
    /// checking, and `GROUPS` gets the single all-zero entry that a file of at
    /// most one group's worth of rows wants.  `rows_offset` is filled in from
    /// what the prefix turned out to be, since a file that disagrees with itself
    /// about that is already tested elsewhere.
    fn crafted(header: Header, txfix: &[(u64, i64)], rows: &[u8]) -> Vec<u8> {
        assert!(header.records <= GROUP, "one group, written by hand");
        let mut prefix = Vec::new();
        put_varint(&mut prefix, txfix.len() as u64);
        for &(step, delta) in txfix {
            put_varint(&mut prefix, step);
            put_zigzag(&mut prefix, delta);
        }
        put_varint(&mut prefix, 1);
        prefix.extend_from_slice(&[0u8; 16]);

        let header = Header {
            rows_offset: HEADER_BYTES + prefix.len() as u64,
            ..header
        };
        let mut file = Vec::new();
        file.extend_from_slice(&header.encode(MAGIC));
        file.extend_from_slice(&prefix);
        file.extend_from_slice(rows);
        file
    }

    /// A coinbase row: one output, no inputs, and the block delta given.
    fn coinbase_row(delta: u64) -> Vec<u8> {
        let mut row = vec![0x10u8];
        put_varint(&mut row, delta);
        row
    }

    /// An input count that wraps the guard on the way past it.
    ///
    /// `self.nonzeros + k > header.nonzeros` is a sum, and release builds set
    /// `panic = "abort"` and so have no overflow checks: with one nonzero
    /// counted and `k` at `u64::MAX` the left side wraps to 0, the guard is
    /// satisfied, and the reservation underneath it asks for every byte of
    /// address space.  Reproduced on the release binary before the fix:
    /// "capacity overflow" out of `RawVec`, SIGABRT, exit 134.
    #[test]
    fn an_input_count_that_wraps_the_nonzero_guard_is_refused() {
        let mut rows = coinbase_row(0);
        // One row with one input, so the reader has a nonzero counted.
        rows.extend_from_slice(&[0x11, 0x01]);
        // And one whose input count escapes to u64::MAX.
        rows.push(0x1f);
        put_varint(&mut rows, u64::MAX);

        let file = crafted(
            Header {
                records: 3,
                transactions: 3,
                nonzeros: 1,
                blocks: 1,
                flags: FLAG_AMOUNTS,
                hash: 0,
                rows_offset: 0,
            },
            &[],
            &rows,
        );
        let err = refusal(read(&file));
        assert!(err.contains("takes the nonzeros past the 1"), "{}", err);
    }

    /// A block delta that wraps, which is the one of these that is a wrong
    /// answer rather than a dead process.
    ///
    /// `open + delta as usize` with `delta` at `u64::MAX` is `open - 1`, so the
    /// block goes *backwards* -- and nothing downstream notices, because every
    /// count in the header still adds up and the content hash is over whatever
    /// the reader decoded rather than over what was meant.  Before the fix these
    /// four coinbase rows read back as blocks 0, 2, 1 and 4, `--verify` said
    /// "the header agrees", and block 1 became a colour.
    #[test]
    fn a_block_delta_that_wraps_does_not_step_the_block_backwards() {
        let mut rows = coinbase_row(0);
        rows.extend_from_slice(&coinbase_row(2));
        rows.extend_from_slice(&coinbase_row(u64::MAX));
        rows.extend_from_slice(&coinbase_row(3));

        // The hash of what the broken reader produced, so that the file is
        // self-consistent in every way except the arithmetic.
        let mut hash = Fnv::new();
        for (tx_id, block_id) in [(0usize, 0usize), (1, 2), (2, 1), (3, 4)] {
            hash_record(
                &mut hash,
                &sexp::Record {
                    block_id,
                    tx_id,
                    outputs: 1,
                },
                &[],
                true,
            );
        }

        let file = crafted(
            Header {
                records: 4,
                transactions: 4,
                nonzeros: 0,
                blocks: 5,
                flags: FLAG_AMOUNTS,
                hash: hash.value(),
                rows_offset: 0,
            },
            &[],
            &rows,
        );
        let err = refusal(read(&file));
        assert!(err.contains("past every block there is"), "{}", err);

        // And the sweep that checks the hash refuses it too, at the same row --
        // the hash is not what catches this and was never going to be.
        let mut reader = Reader::new(&file[..], None).unwrap();
        let err = refusal(reader.verify());
        assert!(err.contains("past every block there is"), "{}", err);
    }

    /// A count in the file is a hint about an allocation, not an allocation.
    ///
    /// `Vec::with_capacity` on a number the file chose is an abort no caller can
    /// catch.  Over a pipe there is no length to check the number against, so
    /// what stands between a 73-byte file and SIGABRT is the clamp at
    /// [`RESERVE`] and the loops bailing at end of file.
    #[test]
    fn a_count_the_file_chose_is_not_a_count_the_reader_allocates() {
        // TXFIX, which is 16 bytes an entry: 2^50 of them is the 18-petabyte
        // request the release binary died on.
        for count in [1u64 << 50, (1 << 60) + 7] {
            let mut file = Vec::new();
            file.extend_from_slice(
                &Header {
                    records: count,
                    transactions: count,
                    nonzeros: 0,
                    blocks: 1,
                    flags: FLAG_AMOUNTS,
                    hash: 0,
                    rows_offset: HEADER_BYTES,
                }
                .encode(MAGIC),
            );
            put_varint(&mut file, count);
            let err = refusal(Reader::new(&file[..], None));
            assert!(err.contains("ends at byte"), "TXFIX {}: {}", count, err);
        }

        // GROUPS, which the count of rows fixes exactly, so a file gets to
        // choose it only by choosing the row count with it.
        let records = (1u64 << 50) * GROUP;
        let mut file = Vec::new();
        file.extend_from_slice(
            &Header {
                records,
                transactions: records,
                nonzeros: 0,
                blocks: 1,
                flags: FLAG_AMOUNTS,
                hash: 0,
                rows_offset: HEADER_BYTES,
            }
            .encode(MAGIC),
        );
        put_varint(&mut file, 0);
        put_varint(&mut file, records.div_ceil(GROUP));
        let err = refusal(Reader::new(&file[..], None));
        assert!(err.contains("ends at byte"), "{}", err);

        // And a row's own input count, which the nonzero guard bounds by the
        // header's nonzeros -- a number the file also chose.  16 bytes an input
        // and 2^50 of them is the same abort by a different door, and the guard
        // is satisfied: it is exactly the count the header promised.
        let mut rows = coinbase_row(0);
        rows.push(0x1f);
        put_varint(&mut rows, 1 << 50);
        let file = crafted(
            Header {
                records: 2,
                transactions: 2,
                nonzeros: 1 << 50,
                blocks: 1,
                flags: FLAG_AMOUNTS,
                hash: 0,
                rows_offset: 0,
            },
            &[],
            &rows,
        );
        let err = refusal(read(&file));
        assert!(err.contains("ends at byte"), "{}", err);
    }

    /// And where there *is* a length, it bounds what the header may claim.
    ///
    /// [`Reader::open`] holds a `File` and a regular file has a size; a row
    /// costs at least its head byte and a nonzero at least the byte of a column
    /// varint, so the bytes after the rows offset bound both counts before a
    /// single one of them is reserved against.
    #[test]
    fn the_length_of_a_file_bounds_what_its_header_may_claim() {
        let dir = std::env::temp_dir().join(format!("foldmat-bound-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("A.foldmat");
        let path = path.to_str().unwrap().to_string();

        let good = write(&synthetic(), true);
        for (at, value, expect) in [
            (8usize, 1u64 << 50, "bytes for them"),
            (24, 1u64 << 50, "bytes of rows"),
            (56, 1u64 << 50, "bytes long"),
        ] {
            let mut file = good.clone();
            file[at..at + 8].copy_from_slice(&value.to_le_bytes());
            std::fs::write(&path, &file).unwrap();
            let err = refusal(Reader::open(&path, None));
            assert!(err.contains(expect), "field at {}: {}", at, err);
        }

        // The honest file is still opened, and the bound is not off by one:
        // nine rows in the bytes nine rows take.
        std::fs::write(&path, &good).unwrap();
        assert!(Reader::open(&path, None).is_ok());
        std::fs::remove_file(&path).unwrap();
    }

    /// TXFIX steps and deltas are the file's numbers too.
    ///
    /// A row step that wraps names a row before the one before it, which the
    /// tail catches only by noticing an entry was never reached; a delta that
    /// wraps names a transaction that is not the one the arithmetic meant.
    /// Both are `+` on a file's operands, and a debug build panics on both.
    #[test]
    fn txfix_steps_and_deltas_that_wrap_are_refused() {
        let mut rows = coinbase_row(0);
        rows.extend_from_slice(&coinbase_row(1));
        let base = Header {
            records: 2,
            transactions: 3,
            nonzeros: 0,
            blocks: 2,
            flags: FLAG_AMOUNTS,
            hash: 0,
            rows_offset: 0,
        };

        // A first entry that is fine and a second whose step carries the sum
        // past 2^64 and back round to row 0 -- which is inside the matrix, so
        // the bounds check under it is satisfied by a row the arithmetic never
        // meant to name.
        let file = crafted(base, &[(1, 1), (u64::MAX, 1)], &rows);
        let err = refusal(read(&file));
        assert!(err.contains("past every row number there is"), "{}", err);

        // A delta applied to a transaction id that is not an i64 to begin with.
        // Row 0 is fixed to i64::MAX, which puts the *next* row's expected id at
        // 2^63; `expected as i64` is negative there, and a delta of -1 lands
        // back on a plausible id by wrapping rather than by counting.
        let file = crafted(
            Header {
                transactions: u64::MAX,
                ..base
            },
            &[(0, i64::MAX), (1, -1)],
            &rows,
        );
        let err = refusal(read(&file));
        assert!(
            err.contains("lands outside every transaction id there is"),
            "{}",
            err
        );
    }

    /// An amount chain that steps past what an amount can be.
    ///
    /// The writer refuses to store one past `i64::MAX`, so a reader that arrives
    /// at one is reading a file this writer did not write.  In release the sum
    /// wrapped to a negative and the negativity check happened to catch it; in
    /// debug it panicked before the check ran.  Neither is the check doing its
    /// job on purpose.
    #[test]
    fn an_amount_that_steps_past_what_an_amount_can_be_is_refused() {
        let mut rows = coinbase_row(0);
        // One row, two inputs, both spending transaction 0.
        rows.push(0x12);
        put_varint(&mut rows, 1); // the first column reaches one back
        put_zigzag(&mut rows, 0); // and the second is the same parent
        put_varint(&mut rows, i64::MAX as u64); // the first amount, at the ceiling
        put_zigzag(&mut rows, i64::MAX); // and a step that goes over it

        let file = crafted(
            Header {
                records: 2,
                transactions: 2,
                nonzeros: 2,
                blocks: 1,
                flags: FLAG_AMOUNTS,
                hash: 0,
                rows_offset: 0,
            },
            &[],
            &rows,
        );
        let err = refusal(read(&file));
        assert!(err.contains("past 2^63 at input 1"), "{}", err);
    }

    /// A column step that overflows the sum it is added into.
    ///
    /// `TXFIX` is what makes this reachable: it puts row 1 at transaction
    /// `i64::MAX`, so the row's first column lands one short of it and a step of
    /// `i64::MAX` carries the sum past what an `i64` holds.  Release wrapped it
    /// to -3 and the negativity check refused it for a reason that was not the
    /// reason; debug panicked before the check ran.  Widened, the refusal names
    /// the number the file actually asked for.
    #[test]
    fn a_column_step_that_overflows_is_refused_for_the_reason_it_is_wrong() {
        let mut rows = coinbase_row(0);
        rows.push(0x12); // one output, two inputs
        put_varint(&mut rows, 1); // the first reaches one transaction back
        put_zigzag(&mut rows, i64::MAX); // and the second steps off the end

        let file = crafted(
            Header {
                records: 2,
                transactions: u64::MAX,
                nonzeros: 2,
                blocks: 1,
                flags: FLAG_AMOUNTS,
                hash: 0,
                rows_offset: 0,
            },
            &[(1, i64::MAX - 1)],
            &rows,
        );
        let err = refusal(read(&file));
        assert!(
            err.contains("spends 18446744073709551613"),
            "the sum, not what it wrapped to: {}",
            err
        );
    }

    // ---------------------------------------------------------------------
    // The sweep that checks the matrix alone
    // ---------------------------------------------------------------------

    /// What `--verify` answers on a file that is what it says it is.
    #[test]
    fn a_sweep_of_a_good_matrix_agrees_with_its_header() {
        let file = write(&synthetic(), true);
        let header = Header::decode(&file[..64].try_into().unwrap(), None).unwrap();

        let mut reader = Reader::new(&file[..], None).unwrap();
        let found = reader.verify().expect("a matrix that is what it says");
        assert_eq!(found.records, header.records);
        assert_eq!(found.nonzeros, header.nonzeros);
        assert_eq!(found.transactions, header.transactions);
        assert_eq!(found.blocks, header.blocks);
        assert_eq!(found.hash, header.hash);

        // A hash is over all the rows or over none of them, so a reader that has
        // already been read from is refused rather than answered.
        let mut reader = Reader::new(&file[..], None).unwrap();
        reader.next_record(&mut Vec::new()).unwrap();
        let err = refusal(reader.verify());
        assert!(err.contains("1 rows into the matrix"), "{}", err);
    }

    /// Every single-bit flip of `ROWS` that the reader still accepts.
    ///
    /// This is the measurement the module doc quotes, run small: flip each bit
    /// of the rows in turn, and for every flipped file that reads to the end
    /// without a complaint -- a full fold's worth of structural checking, counts
    /// and `GROUPS` and all -- assert that the sweep refuses it.  On the nine
    /// synthetic rows a good few flips get through the structure, which is the
    /// whole point: the structure was never going to catch them, and until
    /// `verify` nothing read the header's hash.
    #[test]
    fn a_sweep_catches_the_flipped_bits_the_structure_lets_through() {
        let file = write(&synthetic(), true);
        let offset = Header::decode(&file[..64].try_into().unwrap(), None)
            .unwrap()
            .rows_offset as usize;

        let (mut accepted, mut refused) = (0u32, 0u32);
        for byte in offset..file.len() {
            for bit in 0..8 {
                let mut flipped = file.clone();
                flipped[byte] ^= 1 << bit;
                if read(&flipped).is_err() {
                    refused += 1;
                    continue;
                }
                accepted += 1;
                // The structure took it.  The hash must not.
                let mut reader = Reader::new(&flipped[..], None).unwrap();
                let err = refusal(reader.verify());
                assert!(
                    err.contains("the header says"),
                    "byte {} bit {}: {}",
                    byte,
                    bit,
                    err
                );
            }
        }
        assert!(
            accepted > 0,
            "no flip survived the structure, so this test proves nothing"
        );
        assert_eq!(
            accepted + refused,
            8 * (file.len() - offset) as u32,
            "every bit of every row was tried"
        );
    }

    /// The file on disk, with the magic written last.
    #[test]
    fn the_writer_assembles_a_file_the_reader_accepts() {
        let dir = std::env::temp_dir().join(format!("foldmat-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("A.foldmat");
        let path = path.to_str().unwrap();

        let rows = synthetic();
        let mut writer = Writer::create(path, true).unwrap();
        // While it is in flight the file says so, and a reader refuses it.
        let err = refusal(Reader::open(path, None));
        assert!(err.contains("in flight"), "{}", err);
        for r in &rows {
            writer
                .push(
                    &sexp::Record {
                        block_id: r.block_id,
                        tx_id: r.tx_id,
                        outputs: r.outputs,
                    },
                    &r.inputs,
                )
                .unwrap();
        }
        let header = writer.finish().unwrap();

        let on_disk = std::fs::read(path).unwrap();
        assert_eq!(on_disk, write(&rows, true), "the same bytes either way");
        assert_eq!(header.records, 9);
        assert!(
            !std::path::Path::new(&format!("{}.rows", path)).exists(),
            "the spill is gone"
        );

        let mut reader = Reader::open(path, Some(9)).unwrap();
        let mut inputs = Vec::new();
        let mut read_back = 0;
        while reader.next_record(&mut inputs).unwrap().is_some() {
            read_back += 1;
        }
        assert_eq!(read_back, 9);
        std::fs::remove_file(path).unwrap();
    }
}
