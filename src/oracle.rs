//! # Knowing when a colour is dead rather than guessing
//!
//! The driver holds a colour for every transaction with an unspent output,
//! because any of those outputs might still be spent.  That is the only answer
//! available to a program reading a stream, and over the 2022 chain it means
//! holding 92,735,490 colours at the end and 92,753,575 at peak — the
//! multiplier in the memory wall, since the store is that count times the size
//! of a colour.
//!
//! It is also the wrong question.  What the fold needs is not *could this be
//! spent* but *will anything later in this file read it again*, and the two
//! differ enormously: 51,815,075 of the 136 million outputs unspent at the end
//! of the chain are `OP_RETURN`, provably unspendable and untouched by any of
//! the file's 2,044,897,328 inputs, and millions more are simply never spent
//! within it.  Every colour held for one of those is held for nothing.
//!
//! A file has a future, so the right question can be answered — by reading the
//! records once before colouring them.  `examples/lastspend.rs` does that and
//! writes one `u32` a transaction: the record that spends it last, or
//! [`NEVER`].  This reads that back.  The peak drops 4.79x, which is what makes
//! `--bands 1024` comfortable and `--bands 4096` possible at all.
//!
//! ## It must not change a single byte of output
//!
//! This decides *when a colour is freed*, never what it contains, so a run with
//! the oracle and one without have to agree exactly.  Two things protect that.
//!
//! The oracle is refused unless it covers at least the records the run will
//! read.  Longer is safe -- an oracle for the whole file, used on a prefix,
//! only says a colour is read later than the prefix reaches, so the fold holds
//! it longer than it had to -- which is what lets one oracle serve every prefix
//! of the same file.  Shorter is not: it answers `NEVER` where the truth is
//! "later", and the driver would then either fail its lookup loudly or, worse,
//! be handed a displaced entry quietly.  And [`Oracle::last_reads`] is deliberately
//! conservative in the one place it could be wrong: it frees on the *lowest*
//! input index that names a transaction, because the fold walks its inputs from
//! the last to the first, so the lowest index is the one it reaches last.
//! Freeing on the highest would drop the colour while inputs below it still
//! wanted it.

use std::io::{self, Read};

/// A transaction nothing in the file ever spends.
pub const NEVER: u32 = u32::MAX;

const MAGIC: &[u8; 8] = b"LASTSPN1";

/// For each transaction, the record that spends it last.
pub struct Oracle {
    last: Vec<u32>,
    records: u64,
}

impl Oracle {
    /// Read an oracle, and refuse one that was not written for these records.
    pub fn load(path: &str, records_expected: Option<u64>) -> io::Result<Oracle> {
        let mut file = io::BufReader::with_capacity(1 << 22, std::fs::File::open(path)?);
        let mut head = [0u8; 24];
        file.read_exact(&mut head)?;
        if &head[..8] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a release oracle".to_string(),
            ));
        }
        let records = u64::from_le_bytes(head[8..16].try_into().unwrap());
        let count = u64::from_le_bytes(head[16..24].try_into().unwrap()) as usize;

        // Longer than the run is safe and shorter is not, so this is an
        // inequality rather than a match.  An oracle written for the whole file
        // says a colour's last read is at some record the run may never reach,
        // which only makes the fold hold it longer than it needed to -- the
        // conservative direction, and exactly what makes one oracle usable for
        // every prefix of the same file.  One written for fewer records answers
        // `NEVER` where the truth is "later", and frees colours the run still
        // wants.
        if let Some(expected) = records_expected {
            if records < expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "written for {} records and this run reads {}; \
                         it would free colours that are still wanted",
                        records, expected
                    ),
                ));
            }
        }

        let mut last = vec![0u32; count];
        // Read straight into the vector's bytes: 3 GB through a `u32`-at-a-time
        // loop is a minute of nothing.
        let bytes = unsafe {
            std::slice::from_raw_parts_mut(last.as_mut_ptr() as *mut u8, count * 4)
        };
        file.read_exact(bytes)?;
        if cfg!(target_endian = "big") {
            for v in &mut last {
                *v = v.swap_bytes();
            }
        }
        Ok(Oracle { last, records })
    }

    pub fn records(&self) -> u64 {
        self.records
    }

    /// The record that spends `tx` last, or [`NEVER`].
    #[inline]
    pub fn last(&self, tx: usize) -> u32 {
        self.last.get(tx).copied().unwrap_or(NEVER)
    }

    /// Whether a colour for `tx` is worth keeping once its record is emitted.
    #[inline]
    pub fn ever_read(&self, tx: usize) -> bool {
        self.last(tx) != NEVER
    }

    /// Which of this record's inputs is the last read of the transaction it
    /// spends, written into `out`.
    ///
    /// An input is the last read when this record is the last that spends its
    /// source *and* no earlier input of this same record spends it too.  The
    /// second half matters: the fold walks inputs from the last to the first,
    /// so the earliest index is the one it reaches last, and that is the one
    /// that may take the colour.  Marking the later index instead would free
    /// the colour while the earlier input still wanted it, and the lookup that
    /// followed would fail.
    pub fn last_reads(&self, record: u64, inputs: &[crate::sexp::Input], out: &mut Vec<bool>) {
        out.clear();
        out.resize(inputs.len(), false);
        let index = record as u32;
        for (k, input) in inputs.iter().enumerate() {
            if self.last(input.prev_tx_id) != index {
                continue;
            }
            // The commonest record has one or two inputs, so the scan below is
            // over nothing; a consolidation with thousands is rare enough that
            // its quadratic worst case never shows against the merge it feeds.
            let earlier = inputs[..k].iter().any(|e| e.prev_tx_id == input.prev_tx_id);
            out[k] = !earlier;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sexp::Input;

    fn oracle(last: &[u32]) -> Oracle {
        Oracle {
            last: last.to_vec(),
            records: 0,
        }
    }

    fn input(prev: usize) -> Input {
        Input {
            prev_tx_id: prev,
            amount: 1,
        }
    }

    /// The plain case: an input whose source nothing later reads is the last
    /// read; one whose source is read again is not.
    #[test]
    fn the_last_record_to_spend_a_transaction_may_take_its_colour() {
        // tx 0 is spent last by record 5, tx 1 by record 9.
        let o = oracle(&[5, 9]);
        let mut out = Vec::new();
        o.last_reads(5, &[input(0), input(1)], &mut out);
        assert_eq!(out, vec![true, false], "record 5 is the last read of tx 0 only");
        o.last_reads(9, &[input(1)], &mut out);
        assert_eq!(out, vec![true]);
    }

    /// The case the ordering comment is about.  A record that spends the same
    /// transaction twice must free it on the input the fold reaches *last*,
    /// which is the lowest index, because the fold walks inputs in reverse.
    #[test]
    fn a_record_spending_one_transaction_twice_frees_it_on_the_lowest_input() {
        let o = oracle(&[7]);
        let mut out = Vec::new();
        o.last_reads(7, &[input(0), input(0), input(0)], &mut out);
        assert_eq!(
            out,
            vec![true, false, false],
            "only the input the reverse fold reaches last may take the colour"
        );
    }

    /// A transaction nothing spends is dead the moment its own line is written.
    #[test]
    fn a_transaction_nothing_spends_is_never_read() {
        let o = oracle(&[NEVER, 3]);
        assert!(!o.ever_read(0));
        assert!(o.ever_read(1));
        // And one past the end of the table -- a prefix run whose oracle was
        // written for fewer transactions -- is treated as never read rather
        // than panicking.
        assert!(!o.ever_read(99));
    }

    /// An input whose source is spent again later is not a last read even when
    /// this record spends it more than once.
    #[test]
    fn a_source_read_again_later_is_never_taken_here() {
        let o = oracle(&[12]);
        let mut out = Vec::new();
        o.last_reads(7, &[input(0), input(0)], &mut out);
        assert_eq!(out, vec![false, false], "record 12 still wants tx 0");
    }
}
