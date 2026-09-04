//! # Reading the records on a thread of its own
//!
//! [`sexp::Reader`](crate::sexp) is the other half of what a run does to every
//! byte, and unlike the fold it depends on nothing the fold produces: a record
//! can be parsed at any time after the bytes arrive and before it is coloured.
//! So it does not have to happen between two colours, and this is where it goes
//! instead.
//!
//! What that is worth depends entirely on how big the colours are, which is why
//! it is a switch rather than a change:
//!
//! - Colours of a few blocks — `--example records -- --window 0`, whose
//!   ancestry stays inside a block — and the fold is almost nothing, so parsing
//!   is nearly the whole run: 1,000,001 records of 95 MB in 0.58s, and the fold
//!   behind them is noise.
//! - Colours of a few thousand blocks — `--window 4000`, which is what a real
//!   chain grows — and the merge dwarfs it: at 150,000 records the parse is
//!   around 0.1s of a 3.4s fold, so hiding it is worth a few percent and no
//!   more.
//!
//! Both are real workloads and neither number is the interesting one on its
//! own.  What matters is that the cost is *off the critical path* either way,
//! and that hiding it costs one thread and one bounded channel.
//!
//! ## Batches, and one arena for their inputs
//!
//! A record is three `usize` and a slice of inputs, so sending them one at a
//! time would spend more on the channel than on the record.  They go in batches
//! of [`BATCH`], and a batch is three flat vectors rather than a vector of
//! records each owning a vector of inputs: one allocation a field instead of
//! one a record, and the whole thing is refilled rather than rebuilt once the
//! fold thread hands it back.
//!
//! ## The SIMD is exactly where it was
//!
//! [`crate::simd::digit_run`] and [`crate::simd::eight_digits`] run inside
//! `next_record`, and `next_record` is called here the way it was called from
//! the loop — same reader, same buffer, same 16-byte stride.  A batch boundary
//! falls between records and never inside one, so no run of digits is ever cut
//! in half by it; the vector kernels cannot tell which thread they are on.

use std::io;
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread;

use crate::sexp;

/// Records per batch.
///
/// Large enough that the channel costs nothing per record, small enough that
/// the fold thread is not left waiting on a batch at the start of a run and
/// that the pipeline holds a bounded and modest number of records.
const BATCH: usize = 1024;

/// How many batches may be waiting.  Two is enough to keep the reader working
/// while the fold thread consumes; more only buys latency nobody is watching.
const QUEUE: usize = 4;

/// A run of records, with all of their inputs in one arena.
pub struct Batch {
    records: Vec<sexp::Record>,
    /// Every record's inputs, back to back.
    inputs: Vec<sexp::Input>,
    /// `ends[i]` is where record `i`'s inputs stop in `inputs`; record `i`'s
    /// inputs start where record `i - 1`'s stopped, and the first at 0.
    ends: Vec<usize>,
}

impl Batch {
    fn new() -> Self {
        Batch {
            records: Vec::with_capacity(BATCH),
            inputs: Vec::new(),
            ends: Vec::with_capacity(BATCH),
        }
    }

    fn clear(&mut self) {
        self.records.clear();
        self.inputs.clear();
        self.ends.clear();
    }

    fn len(&self) -> usize {
        self.records.len()
    }

    /// Record `i` and the slice of inputs that go with it.
    #[inline]
    fn get(&self, i: usize) -> (&sexp::Record, &[sexp::Input]) {
        let start = if i == 0 { 0 } else { self.ends[i - 1] };
        (&self.records[i], &self.inputs[start..self.ends[i]])
    }
}

/// Either the records, one at a time off the reader, or the same records off a
/// thread that is already several batches ahead.
///
/// An enum rather than a trait because the choice is made once, before the
/// first record, and what it decides is where a `&[Input]` comes from — a
/// borrow that outlives neither arm and so is easier to hand back from one
/// method than to hide behind a `dyn`.
pub enum Records {
    /// Parsed here, between one colour and the next.  What runs without
    /// `--threads`, and exactly what the driver has always done.
    Here {
        reader: sexp::Reader<Box<dyn io::Read + Send>>,
        inputs: Vec<sexp::Input>,
        record: Option<sexp::Record>,
    },
    /// Parsed on a thread that keeps [`QUEUE`] batches ahead of the fold.
    Ahead {
        batches: Receiver<io::Result<Batch>>,
        /// Batches the fold thread has finished with, to be refilled.
        spare: SyncSender<Batch>,
        batch: Batch,
        at: usize,
        /// Set when the reader has said there are no more, so a run that stops
        /// on the record limit does not go looking for another batch.
        done: bool,
    },
}

impl Records {
    pub fn here(input: Box<dyn io::Read + Send>) -> Self {
        Records::Here {
            reader: sexp::Reader::new(input),
            inputs: Vec::new(),
            record: None,
        }
    }

    /// The same records, read on a thread of its own.
    pub fn ahead(input: Box<dyn io::Read + Send>) -> Self {
        let (send_batch, batches) = sync_channel::<io::Result<Batch>>(QUEUE);
        let (spare, used) = sync_channel::<Batch>(QUEUE + 1);

        thread::spawn(move || {
            let mut reader = sexp::Reader::new(input);
            let mut inputs: Vec<sexp::Input> = Vec::new();
            loop {
                // A batch the fold thread has finished with, or a fresh one:
                // `try_recv`, because waiting on recycling would be waiting on
                // the consumer, and the queue is what paces this already.
                let mut batch = used.try_recv().unwrap_or_else(|_| Batch::new());
                batch.clear();

                let mut ended = false;
                while batch.len() < BATCH {
                    match reader.next_record(&mut inputs) {
                        Err(e) => {
                            // The error goes down the channel rather than up a
                            // return, so that the fold thread reports it in the
                            // order it would have hit it -- and *behind* the
                            // records already in hand, which is why the partial
                            // batch is sent first.  Dropping it here would lose
                            // every good record between the last batch boundary
                            // and the bad one, and a run that fails on record
                            // 1,000,000 would print nine hundred and ninety-nine
                            // thousand fewer lines than the serial reader does.
                            if !batch.records.is_empty() {
                                let _ = send_batch.send(Ok(batch));
                            }
                            let _ = send_batch.send(Err(e));
                            return;
                        }
                        Ok(None) => {
                            ended = true;
                            break;
                        }
                        Ok(Some(record)) => {
                            batch.records.push(record);
                            batch.inputs.extend_from_slice(&inputs);
                            batch.ends.push(batch.inputs.len());
                        }
                    }
                }

                let last = ended;
                if !batch.records.is_empty() && send_batch.send(Ok(batch)).is_err() {
                    // The fold thread stopped early -- a record limit, or a
                    // closed pipe.  Nothing left to read for.
                    return;
                }
                if last {
                    return;
                }
            }
        });

        Records::Ahead {
            batches,
            spare,
            batch: Batch::new(),
            at: 0,
            done: false,
        }
    }

    /// The next record and its inputs, or `None` at the end of the stream.
    ///
    /// The borrow is of whichever buffer the record was parsed into, so it ends
    /// at the next call — which is what the fold wants anyway, since it turns
    /// the inputs into a colour before asking for another record.
    #[inline]
    pub fn next(&mut self) -> io::Result<Option<(&sexp::Record, &[sexp::Input])>> {
        match self {
            Records::Here {
                reader,
                inputs,
                record,
            } => {
                *record = reader.next_record(inputs)?;
                Ok(record.as_ref().map(|r| (r, inputs.as_slice())))
            }
            Records::Ahead {
                batches,
                spare,
                batch,
                at,
                done,
            } => {
                if *at == batch.len() {
                    if *done {
                        return Ok(None);
                    }
                    match batches.recv() {
                        // Every sender is gone, so the reader finished and said
                        // so with the last batch it sent.
                        Err(_) => {
                            *done = true;
                            return Ok(None);
                        }
                        Ok(Err(e)) => {
                            *done = true;
                            return Err(e);
                        }
                        Ok(Ok(next)) => {
                            // The one we have just finished with goes back to be
                            // refilled; best-effort, since a full queue only
                            // means the reader has spares already.
                            let spent = std::mem::replace(batch, next);
                            let _ = spare.try_send(spent);
                            *at = 0;
                        }
                    }
                }
                let i = *at;
                *at += 1;
                Ok(Some(batch.get(i)))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The records the driver's own tests use, in the shape [`sexp`] reads.
    fn records(n: usize) -> String {
        (0..n)
            .map(|tx| {
                let inputs: String = (0..tx % 4)
                    .map(|k| format!("(7 {} {} 0)", 100 + k, tx.saturating_sub(k + 1)))
                    .collect();
                format!(
                    "((1 {} {} 0 0 0 0) ({}) ((7 1 0)(7 1 0)))\n",
                    tx / 7,
                    tx,
                    inputs
                )
            })
            .collect()
    }

    /// Everything a record carries, so the two readers can be compared as
    /// values rather than by eye.
    fn drain(mut source: Records) -> Vec<(usize, usize, usize, Vec<sexp::Input>)> {
        let mut all = Vec::new();
        while let Some((record, inputs)) = source.next().expect("the records are well formed") {
            all.push((
                record.block_id,
                record.tx_id,
                record.outputs,
                inputs.to_vec(),
            ));
        }
        all
    }

    /// The property: a thread changes when the records are read and nothing at
    /// all about what they say.  Lengths either side of a batch boundary,
    /// because that is the seam the arena has to get right.
    #[test]
    fn reading_ahead_yields_exactly_the_same_records() {
        for n in [0usize, 1, 2, 1023, 1024, 1025, 2048, 2049, 5000] {
            let text = records(n);
            let here = drain(Records::here(Box::new(io::Cursor::new(text.clone()))));
            let ahead = drain(Records::ahead(Box::new(io::Cursor::new(text.clone()))));
            assert_eq!(here.len(), n, "{} records read one at a time", n);
            assert_eq!(ahead, here, "{} records disagree across the two readers", n);
        }
    }

    /// A malformed record has to come back as an error from the call that
    /// reaches it, not be swallowed by the thread that found it first.
    #[test]
    fn a_bad_record_is_reported_by_both_readers() {
        let text = format!("{}((1 0 nonsense", records(3));
        for mut source in [
            Records::here(Box::new(io::Cursor::new(text.clone()))),
            Records::ahead(Box::new(io::Cursor::new(text.clone()))),
        ] {
            let mut good = 0;
            let outcome = loop {
                match source.next() {
                    Ok(Some(_)) => good += 1,
                    Ok(None) => break Ok(()),
                    Err(e) => break Err(e),
                }
            };
            assert_eq!(good, 3, "the records before the bad one still arrive");
            assert!(outcome.is_err(), "the bad record has to be reported");
        }
    }

    /// Stopping early -- a record limit -- must not hang: the reader thread is
    /// blocked on a full queue and has to notice the receiver going away.
    #[test]
    fn stopping_early_lets_the_reader_thread_go() {
        let text = records(200_000);
        let mut source = Records::ahead(Box::new(io::Cursor::new(text)));
        for _ in 0..10 {
            assert!(source.next().expect("well formed").is_some());
        }
        drop(source);
    }
}
