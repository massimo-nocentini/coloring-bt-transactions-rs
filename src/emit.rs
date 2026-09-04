//! # Formatting lines on more than one thread
//!
//! Turning a color into a line is the most expensive thing a text run does, and
//! by a long way.  Measured over the first 150,000 records of
//! `cargo run --release --example records -- --window 4000` — colors of some
//! three thousand blocks by the end of it — with the fold and the parse held
//! constant by comparing against `--sum`, which walks exactly the same terms
//! and prints one number instead of all of them:
//!
//! ```text
//!     --sets --fold /dev/null    2.43s      parse and fold, no lines
//!     --sets                     4.70s      + 2.3 GB of terms
//!     --weighted --sum           3.37s      parse and weighted fold
//!     --weighted                50.18s      + 9.9 GB of terms
//! ```
//!
//! Every number in this file names that corpus; `examples/records.rs` is what
//! makes them checkable rather than merely stated.
//!
//! So formatting is 47% of an unweighted text run and 94% of a weighted one.
//! The gap is [`crate::push_f64`]: the shortest round-tripping decimal costs
//! about 133ns a term against 15ns for the integer path, and a weighted color
//! spends one on every block it names.
//!
//! It is also the one part of the driver that is embarrassingly parallel.  A
//! line is a pure function of `(tx_id, color)` — nothing about it looks at the
//! store, at the records still to come, or at the line before it.
//!
//! ## What crosses the thread boundary
//!
//! A copy of the terms, not a handle on the color.
//!
//! Sharing the color itself is the obvious move and it is the wrong one here.
//! It would mean `Rc` becoming `Arc` in both set backends, atomics on a
//! refcount the fold thread touches several times a record, and — the real
//! cost — an ownership question the stores currently answer exactly:
//! [`crate::store::ColorStore::release`] decides whether an allocation is gone
//! by asking whether it holds the last handle, and that question has no
//! race-free answer once a second thread can hold one too.
//!
//! Copying sidesteps all of it.  A term is 12 bytes and a formatted term is
//! about 22, so the copy is a memcpy against a decimal conversion: roughly 1ns
//! a term against 133.  The stores keep their `Rc`, keep their layouts, and do
//! not learn that threads exist; [`crate::poly`]'s arena, which could not have
//! been shared at all, gets the same speedup as the others.
//!
//! ## Order, and why it costs nothing
//!
//! Record `i` goes to worker `i % threads` and the writer collects from worker
//! `i % threads`, so the lines are written in the order they were dispatched
//! without a sequence number, a reorder buffer, or a sort.  The output is
//! byte-identical to the serial path — which matters beyond tidiness, since
//! diffing `--rings` against `--sets` is how this crate checks one backend
//! against the other, and a threaded run has to stay usable for that.
//! `parallel_output_is_byte_identical` at the bottom of this file is the test.
//!
//! ## Closing it is not optional
//!
//! [`Pool::finish`] drains the pipeline and reports what the writer made of it,
//! and `run` calls it — but only on the path that reaches the end of the
//! records.  Every early return skips it, and a detached writer thread takes
//! its buffer to the grave with it.  So the closing is in [`Drop`] as well,
//! where the language runs it; `finish` is then the way to get at the *error*,
//! not the way to make the output happen.  See the `impl Drop` below for what
//! this used to cost.
//!
//! ## What bounds the memory
//!
//! Every channel into the pipeline is bounded, so a fold thread that outruns
//! the formatters blocks instead of queueing.  The two channels *backwards* —
//! the ones that hand a line buffer back to the worker that filled it and a
//! snapshot back to the fold thread — are best-effort `try_send`: dropping a
//! recycled buffer costs an allocation, where blocking on one would close a
//! cycle in the wait graph and deadlock.  Nothing waits on recycling.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::Arc;
use std::thread;

use crate::{push_f64, push_int, Line};

/// How many batches may be in flight per worker.
///
/// Deep enough that a worker delayed on one long color does not stall the
/// dispatcher, shallow enough that the pipeline holds a bounded and small
/// multiple of a batch rather than an unbounded backlog of them.
const DEPTH: usize = 4;

/// Records a batch carries, and terms it carries, whichever it reaches first.
///
/// A batch rather than a record at a time because the channel costs the same
/// whatever it carries, and what it carries varies by four orders of magnitude:
/// a color of one block is a ten-byte line, and one of three thousand is fifty
/// kilobytes.  Dispatching per record made a threaded run *slower than serial*
/// on records whose colors stay small -- a microsecond of channel to hand over
/// a line worth a hundred nanoseconds of work.
///
/// Over `--example records -- --window 0`, a million records whose every color
/// is one block, where serial is 0.58s:
///
/// ```text
///                     per record   batched
///     --threads 1          1.59s     0.43s
///     --threads 2          0.89s     0.43s
///     --threads 4          1.19s     0.43s
///     --threads 8          1.36s     0.43s
/// ```
///
/// So both bounds, and neither alone.  [`LINES`] is what batches the small
/// colors, where the record count is the cost; [`TERMS`] is what stops a batch
/// of large ones from holding megabytes of formatted output per worker, where
/// the byte count is.  A color of three thousand blocks fills a batch on its
/// own, which is the right answer -- there the channel was never the problem.
const LINES: usize = 256;
const TERMS: usize = 8192;

/// What follows the transaction id and the tab on a line.
///
/// One definition of it, driven either straight off the store on the fold
/// thread or off a [`Snapshot`] on a worker, so the serial and threaded paths
/// cannot drift into writing different bytes.
pub struct Body<'a> {
    line: &'a mut Vec<u8>,
    form: Line,
    weighted: bool,
    /// Whether a term has been written, which is what puts the comma *between*
    /// terms rather than after each of them.  `for_each_term` does not say
    /// which term is last, so it is this rather than a truncation at the end.
    written: bool,
    /// The four running sums both of the collapsed forms are built from, in one
    /// pass over the terms and no memory at all:
    /// `sum w`, `sum w.b`, `sum w.b^2`, `sum w^2`.
    ///
    /// [`Line::Sum`] prints the second on its own; [`Line::Moments`] turns all
    /// four into a mean, a spread and an effective count.  Kept here rather
    /// than in the driver's loop so the serial and threaded paths accumulate
    /// them the same way, as they format the terms the same way.
    mass: f64,
    first: f64,
    second: f64,
    squares: f64,
}

impl<'a> Body<'a> {
    pub fn new(line: &'a mut Vec<u8>, form: Line, weighted: bool) -> Self {
        Body {
            line,
            form,
            weighted,
            written: false,
            mass: 0.0,
            first: 0.0,
            second: 0.0,
            squares: 0.0,
        }
    }

    #[inline]
    pub fn term(&mut self, exponent: usize, coefficient: f64) {
        match self.form {
            Line::Terms => {
                if self.written {
                    self.line.push(b',');
                }
                self.written = true;
                if self.weighted {
                    push_f64(self.line, coefficient);
                } else {
                    // Always exactly 1 here, and an integer is what `push_f64`
                    // would print for it anyway -- this just does not pay the
                    // formatter for it.
                    push_int(self.line, coefficient as usize);
                }
                self.line.push(b':');
                push_int(self.line, exponent);
            }
            // Both collapsed forms want the same running sums, and `--sum`
            // wants a strict subset of them.  Kept as one arm rather than two
            // so there is one definition of what is accumulated; the three
            // `--sum` does not read fold away to nothing next to the multiply
            // it does.
            Line::Sum | Line::Moments => {
                let block = exponent as f64;
                self.mass += coefficient;
                self.first += block * coefficient;
                self.second += block * block * coefficient;
                self.squares += coefficient * coefficient;
            }
        }
    }

    /// The mean block id, the spread about it, and the effective number of
    /// blocks -- what [`Line::Moments`] prints, from the running sums.
    ///
    /// A colour with no terms answers three zeroes rather than three NaNs: it
    /// is what [`Line::Sum`] already prints for one, and a NaN in a column is
    /// worse than a zero for every reader of it.
    fn moments(&self) -> (f64, f64, f64) {
        if self.mass <= 0.0 {
            return (0.0, 0.0, 0.0);
        }
        let mean = self.first / self.mass;
        // `E[b^2] - E[b]^2` rather than a second pass, which a streaming line
        // cannot make.  The cancellation is real but bounded: block ids reach
        // some hundreds of thousands, so `mean^2` is around 1e11 and an `f64`
        // still resolves a variance of 1e-4 -- a spread of a hundredth of a
        // block, far below anything this number is read at.  What it can do is
        // come out a hair below zero for a colour that sits on one block, and
        // that is what the clamp is for; a negative under the root would be a
        // NaN on the line.
        let variance = (self.second / self.mass - mean * mean).max(0.0);
        // The participation ratio, which is `1 / sum w^2` for weights that sum
        // to one: two blocks at half each answers 2, a thousand blocks at a
        // thousandth each answers 1000, and a colour that is nearly all one
        // block answers nearly 1 however many blocks trail behind it.  That is
        // the sense in which it counts blocks that *matter*, where the support
        // size counts blocks that merely appear.
        let effective = self.mass * self.mass / self.squares;
        (mean, variance.sqrt(), effective)
    }

    /// Close the line, newline and all.
    pub fn finish(self) {
        match self.form {
            Line::Terms => {}
            Line::Sum => push_f64(self.line, self.first),
            Line::Moments => {
                let (mean, spread, effective) = self.moments();
                push_f64(self.line, mean);
                self.line.push(b'\t');
                push_f64(self.line, spread);
                self.line.push(b'\t');
                push_f64(self.line, effective);
            }
        }
        self.line.push(b'\n');
    }
}

/// A color copied out of the store, which is everything a line needs and
/// nothing the store owns.
///
/// The two arrays are kept apart rather than interleaved into pairs for the
/// unweighted backends' sake: they have no coefficients worth carrying — every
/// one of them is the integer 1 — so `weights` stays empty and the copy is four
/// bytes a term rather than sixteen.
pub struct Snapshot {
    tx_id: usize,
    /// Exponents in the order [`crate::store::ColorStore::for_each_term`]
    /// yields them, which is the order they print in.
    blocks: Vec<usize>,
    /// Coefficients at the same indices, or empty under an unweighted backend.
    weights: Vec<f64>,
}

impl Snapshot {
    fn new() -> Self {
        Snapshot {
            tx_id: 0,
            blocks: Vec::new(),
            weights: Vec::new(),
        }
    }

    /// A term of an unweighted color, whose coefficient is 1 and is not stored.
    #[inline]
    pub fn push_flat(&mut self, exponent: usize) {
        self.blocks.push(exponent);
    }

    #[inline]
    pub fn push_weighted(&mut self, exponent: usize, coefficient: f64) {
        self.blocks.push(exponent);
        self.weights.push(coefficient);
    }

    fn reset(&mut self, tx_id: usize) {
        self.tx_id = tx_id;
        self.blocks.clear();
        self.weights.clear();
    }

    /// Whether this snapshot carries coefficients, which is a fact about the
    /// snapshot rather than about the pool.
    ///
    /// Asking the snapshot rather than being told is what makes the mismatch
    /// unrepresentable.  The pool used to be handed a `weighted` flag of its
    /// own, settled way back in `plan` from the backend; the two agreed by
    /// construction, and nothing checked.  Had they ever disagreed, a flat
    /// snapshot reaching a weighted pool would have indexed an empty `weights`
    /// and panicked in a formatter thread -- which is the one failure this
    /// design cannot report well.  Now there is nothing to disagree with.
    fn weighted(&self) -> bool {
        !self.weights.is_empty()
    }

    /// This color's line, onto the end of `line` -- a batch's lines share one
    /// buffer, so the writer makes one `write_all` of the lot.
    fn append(&self, line: &mut Vec<u8>, form: Line) {
        push_int(line, self.tx_id);
        line.push(b'\t');
        let weighted = self.weighted();
        let mut body = Body::new(line, form, weighted);
        for k in 0..self.blocks.len() {
            // A color with no terms at all prints none either way, so the two
            // shapes cannot be told apart there and do not need to be.
            let coefficient = if weighted { self.weights[k] } else { 1.0 };
            body.term(self.blocks[k], coefficient);
        }
        body.finish();
    }
}

/// A batch's lines and the snapshots they were formatted from, travelling
/// together so that both can be handed back and filled again.
type Done = (Vec<u8>, Vec<Snapshot>);

/// A pool of formatter threads and the one writer that puts their lines in
/// order.
pub struct Pool {
    /// Dispatch, one channel a worker.  Batch `b` goes to `to[b % to.len()]`,
    /// and the writer collects from `b % to.len()`, which is what puts the
    /// lines back in the order they were staged.
    to: Vec<SyncSender<Vec<Snapshot>>>,
    /// Batches the writer has finished with, for their snapshots and for the
    /// vector that held them.
    spare: Receiver<Vec<Snapshot>>,
    /// The batch being filled, and how many terms are already in it.
    batch: Vec<Snapshot>,
    terms: usize,
    /// Snapshots and batch vectors that have come back, to be filled again.
    free: Vec<Snapshot>,
    containers: Vec<Vec<Snapshot>>,
    next: usize,
    /// Set by the writer when a write fails, so the fold thread stops feeding a
    /// pipeline whose far end has gone away — a closed `| head`, most often.
    failed: Arc<AtomicBool>,
    threads: Vec<thread::JoinHandle<()>>,
    /// Taken by the first shutdown, so joining twice is not a panic: a write
    /// error shuts the pool down from `dispatch`, and `finish` still runs.
    writer: Option<thread::JoinHandle<io::Result<()>>>,
}

impl Pool {
    /// `threads` formatter threads over `sink`, buffered a megabyte at a time
    /// the way the serial path buffers it.
    pub fn new(sink: Box<dyn Write + Send>, form: Line, threads: usize) -> Pool {
        let threads = threads.max(1);
        let failed = Arc::new(AtomicBool::new(false));
        // The writer keeps one receiver a worker and reads them round-robin,
        // which is what puts the lines back in dispatch order.
        let mut collect = Vec::with_capacity(threads);
        let mut recycle = Vec::with_capacity(threads);
        let mut to = Vec::with_capacity(threads);
        let mut handles = Vec::with_capacity(threads);

        for _ in 0..threads {
            let (send_work, work) = sync_channel::<Vec<Snapshot>>(DEPTH);
            let (send_done, done) = sync_channel::<Done>(DEPTH);
            let (send_spare_line, spare_line) = sync_channel::<Vec<u8>>(DEPTH);
            to.push(send_work);
            collect.push(done);
            recycle.push(send_spare_line);
            handles.push(thread::spawn(move || {
                for batch in work {
                    // A buffer the writer has finished with, or a fresh one:
                    // `try_recv` rather than `recv` because a worker must never
                    // wait on recycling, only benefit from it.
                    let mut lines = spare_line.try_recv().unwrap_or_default();
                    lines.clear();
                    for snapshot in &batch {
                        snapshot.append(&mut lines, form);
                    }
                    if send_done.send((lines, batch)).is_err() {
                        break;
                    }
                }
            }));
        }

        let (send_spare, spare) = sync_channel::<Vec<Snapshot>>(threads * DEPTH + 1);
        let flag = Arc::clone(&failed);
        let writer = thread::spawn(move || {
            let mut out = io::BufWriter::with_capacity(1 << 20, sink);
            let mut result = Ok(());
            let mut k = 0usize;
            // Stops at the first worker whose channel is both empty and closed.
            // Dispatch was round-robin, so that is exactly the point where the
            // records ran out: for every earlier `k` the worker it names still
            // owes a line.
            while let Ok((lines, batch)) = collect[k % threads].recv() {
                if result.is_ok() {
                    // One call for the whole batch, which is the other thing
                    // batching buys.
                    if let Err(e) = out.write_all(&lines) {
                        // Kept, and reported from `finish`.  The loop carries on
                        // draining: a writer that stopped receiving would block
                        // the workers, which would block the fold thread, and
                        // the run would hang instead of failing.
                        result = Err(e);
                        flag.store(true, Ordering::Release);
                    }
                }
                let _ = recycle[k % threads].try_send(lines);
                let _ = send_spare.try_send(batch);
                k += 1;
            }
            result.and_then(|()| out.flush())
        });

        Pool {
            to,
            spare,
            batch: Vec::with_capacity(LINES),
            terms: 0,
            free: Vec::new(),
            containers: Vec::new(),
            next: 0,
            failed,
            threads: handles,
            writer: Some(writer),
        }
    }

    /// A snapshot to fill for `tx_id`: one that has come back from the writer
    /// if there is one, a fresh one otherwise.
    ///
    /// Recycling is what keeps a run of thousand-term colors from allocating a
    /// pair of vectors per record; once the pipeline is full it comes back
    /// every time.
    pub fn stage(&mut self, tx_id: usize) -> Snapshot {
        if self.free.is_empty() {
            self.reclaim();
        }
        let mut snapshot = self.free.pop().unwrap_or_else(Snapshot::new);
        snapshot.reset(tx_id);
        snapshot
    }

    /// Take back whatever the writer has finished with, without waiting for it.
    fn reclaim(&mut self) {
        while let Ok(mut batch) = self.spare.try_recv() {
            self.free.append(&mut batch);
            self.containers.push(batch);
        }
    }

    /// Add a filled snapshot to the batch, and send the batch once it is full.
    ///
    /// Full is whichever of [`LINES`] and [`TERMS`] it reaches first — see
    /// those for why one bound is not enough.
    pub fn dispatch(&mut self, snapshot: Snapshot) -> io::Result<()> {
        self.terms += snapshot.blocks.len();
        self.batch.push(snapshot);
        if self.batch.len() >= LINES || self.terms >= TERMS {
            return self.send();
        }
        Ok(())
    }

    /// Hand the batch being filled to the next worker, in turn.
    fn send(&mut self) -> io::Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        if self.failed.load(Ordering::Acquire) {
            // The far end has gone; stop here and report what it said rather
            // than filling a pipeline nobody is reading.
            return self.shutdown().and(Err(io::Error::other(
                "the output failed and the reason was lost",
            )));
        }
        if self.to.is_empty() {
            // Shut down already, and not because of a write failure -- there is
            // no path here today, and this is what keeps the `%` below from
            // being a division by zero if one ever appears.
            return Err(io::Error::other("the formatters have already stopped"));
        }
        let container = self.containers.pop().unwrap_or_else(|| Vec::with_capacity(LINES));
        let batch = std::mem::replace(&mut self.batch, container);
        self.terms = 0;
        let worker = self.next;
        self.next = (self.next + 1) % self.to.len();
        // A worker only disconnects by panicking, since the pool owns them all
        // and drops the senders in `shutdown`.
        self.to[worker]
            .send(batch)
            .map_err(|_| io::Error::other("a formatter thread stopped"))
    }

    /// Close the dispatch channels, let the pipeline drain, and answer whatever
    /// the writer made of it.
    ///
    /// Idempotent, because a write error shuts the pool down from `send` and
    /// `finish` is still called on the way out.
    fn shutdown(&mut self) -> io::Result<()> {
        self.to.clear();
        let mut panicked = false;
        for handle in self.threads.drain(..) {
            panicked |= handle.join().is_err();
        }
        let written = match self.writer.take() {
            None => Ok(()),
            Some(writer) => writer
                .join()
                .unwrap_or_else(|_| Err(io::Error::other("the writing thread panicked"))),
        };
        // The writer's own complaint first, since it names a cause -- a closed
        // pipe, a full disk.
        written?;
        if panicked {
            // A worker that panicked dropped its channel, and the writer stops
            // at the first channel that is closed and empty.  So the lines
            // behind it were never written, and the flush that followed still
            // returned `Ok`: without this, the one path in this design that can
            // truncate the output *and* exit 0.
            return Err(io::Error::other(
                "a formatter thread panicked, so the output is incomplete",
            ));
        }
        Ok(())
    }

    pub fn finish(mut self) -> io::Result<()> {
        // The last batch is almost never a full one, and it holds real lines.
        let sent = self.send();
        let drained = self.shutdown();
        sent.and(drained)
    }
}

/// Closing the pipeline is not something the driver can be trusted to remember.
///
/// [`Pool::finish`] is the ordinary way out, and `run` calls it -- but only when
/// it reaches the end.  Every early return skips it: `?` on a malformed record,
/// the `spends unknown transaction` error, a write failure, and a panic
/// unwinding out of the fold.  On those paths the pool was simply dropped, and
/// dropping a [`thread::JoinHandle`] *detaches* the thread rather than waiting
/// for it, so the process exited with the writer still holding up to a megabyte
/// of buffered lines and the last batch never dispatched at all.
///
/// Measured on a 200,000-record file with one bad record at the end: serial
/// wrote 2,088,890 bytes and `--threads 4` wrote around 1,250,000 of them, a
/// byte-exact prefix ending mid-line, varying run to run, with the same exit
/// code and the same message on stderr.  On a short enough file it wrote
/// nothing at all, the whole run having sat in the undispatched partial batch.
/// `Output::Text` never had the problem because `BufWriter` flushes on drop.
///
/// So the closing goes here, where the language guarantees it runs.  `send`
/// first, or the staged batch is still lost; both calls are idempotent, so the
/// ordinary path through `finish` is unaffected and this does nothing.
impl Drop for Pool {
    fn drop(&mut self) {
        let _ = self.send();
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A sink a test can read back afterwards.  `Send`, unlike the serial
    /// path's, because the writing happens on a thread of the pool's own.
    #[derive(Clone, Default)]
    struct Shared(Arc<Mutex<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// Colors of varying length, so the workers finish out of step with each
    /// other and the writer has real reordering to undo.
    ///
    /// `spread` is the longest color it will make.  It matters because the two
    /// batch bounds close on different corpora: [`LINES`] closes a batch of
    /// short colors and [`TERMS`] a batch of long ones, and a test that only
    /// ever trips one of them leaves the other's arithmetic unexercised.
    fn colors_upto(n: usize, spread: u64) -> Vec<(usize, Vec<(usize, f64)>)> {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        (0..n)
            .map(|tx| {
                let len = (next() % spread) as usize;
                let mut block = 0usize;
                let terms = (0..len)
                    .map(|_| {
                        block += 1 + (next() % 30) as usize;
                        (block, (next() >> 11) as f64 / (1u64 << 53) as f64)
                    })
                    .collect();
                (tx, terms)
            })
            .collect()
    }

    fn colors(n: usize) -> Vec<(usize, Vec<(usize, f64)>)> {
        colors_upto(n, 40)
    }

    /// What the serial path writes, from the same `Body` it uses.
    fn serially(records: &[(usize, Vec<(usize, f64)>)], form: Line, weighted: bool) -> Vec<u8> {
        let mut out = Vec::new();
        let mut line = Vec::new();
        for (tx, terms) in records {
            line.clear();
            push_int(&mut line, *tx);
            line.push(b'\t');
            let mut body = Body::new(&mut line, form, weighted);
            for &(exponent, coefficient) in terms {
                body.term(exponent, if weighted { coefficient } else { 1.0 });
            }
            body.finish();
            out.extend_from_slice(&line);
        }
        out
    }

    fn threaded(
        records: &[(usize, Vec<(usize, f64)>)],
        form: Line,
        weighted: bool,
        threads: usize,
    ) -> Vec<u8> {
        let sink = Shared::default();
        let mut pool = Pool::new(Box::new(sink.clone()), form, threads);
        for (tx, terms) in records {
            let mut snapshot = pool.stage(*tx);
            for &(exponent, coefficient) in terms {
                if weighted {
                    snapshot.push_weighted(exponent, coefficient);
                } else {
                    snapshot.push_flat(exponent);
                }
            }
            pool.dispatch(snapshot).expect("the sink accepts everything");
        }
        pool.finish().expect("the sink accepts everything");
        let written = sink.0.lock().unwrap().clone();
        written
    }

    /// The property the whole design rests on: threading changes how fast the
    /// lines are made and nothing whatever about what they say.  Byte for byte,
    /// at every width, because diffing one backend against another is how this
    /// crate checks itself and a threaded run has to stay usable for it.
    #[test]
    fn parallel_output_is_byte_identical() {
        // Three corpora, because the two batch bounds close on different ones.
        // 900 short colors trip `LINES` and wrap `next` several times over;
        // 300 long ones average ~200 terms, so `TERMS` closes a batch every
        // forty-odd records and the partial-batch arithmetic is exercised at a
        // different alignment; the third is neither, and is what the test used
        // to be.
        for &(count, spread) in &[(900usize, 6u64), (300, 400), (500, 40)] {
        let records = colors_upto(count, spread);
        for &weighted in &[false, true] {
            for &form in &[Line::Terms, Line::Sum] {
                let want = serially(&records, form, weighted);
                for threads in [1usize, 2, 3, 5, 8, 16] {
                    let got = threaded(&records, form, weighted, threads);
                    assert_eq!(
                        got.len(),
                        want.len(),
                        "weighted={} threads={} wrote {} bytes, not {}",
                        weighted,
                        threads,
                        got.len(),
                        want.len()
                    );
                    assert!(
                        got == want,
                        "weighted={} threads={} count={} spread={} wrote different bytes",
                        weighted,
                        threads,
                        count,
                        spread
                    );
                }
            }
        }
        }
    }

    /// The pool must close itself when it is dropped rather than finished.
    ///
    /// `run` calls [`Pool::finish`] only when it reaches the end of the
    /// records; every early return -- a malformed record, a colour spending an
    /// unknown transaction, a write error, a panic unwinding through the fold
    /// -- drops the pool instead.  Dropping a `JoinHandle` detaches the thread,
    /// so before [`Drop for Pool`](Pool) this lost the staged batch and up to a
    /// megabyte of the writer's buffer: measured at 2,088,890 bytes serial
    /// against about 1,250,000 threaded, ending mid-line and varying run to
    /// run.  On a short enough run it wrote nothing at all, which is what this
    /// asserts against -- 40 records fit inside one unsent batch.
    #[test]
    fn dropping_the_pool_writes_everything_finish_would_have() {
        let records = colors(40);
        let want = serially(&records, Line::Terms, true);
        assert!(!want.is_empty(), "the corpus has to produce lines to lose");

        for threads in [1usize, 4, 8] {
            let sink = Shared::default();
            {
                let mut pool = Pool::new(Box::new(sink.clone()), Line::Terms, threads);
                for (tx, terms) in &records {
                    let mut snapshot = pool.stage(*tx);
                    for &(exponent, coefficient) in terms {
                        snapshot.push_weighted(exponent, coefficient);
                    }
                    pool.dispatch(snapshot).expect("the sink accepts everything");
                }
                // No `finish`: the pool goes out of scope here, which is what
                // every early return in `run` does to it.
            }
            let got = sink.0.lock().unwrap().clone();
            assert_eq!(
                got.len(),
                want.len(),
                "threads={} wrote {} bytes on drop, not {}",
                threads,
                got.len(),
                want.len()
            );
            assert!(got == want, "threads={} wrote different bytes on drop", threads);
        }
    }

    /// More workers than records, so most of the pool never receives anything
    /// and the writer has to stop at the right one anyway.
    #[test]
    fn a_pool_wider_than_the_run_still_ends() {
        let records = colors(3);
        let want = serially(&records, Line::Terms, true);
        assert_eq!(threaded(&records, Line::Terms, true, 32), want);
    }

    #[test]
    fn no_records_at_all_writes_nothing() {
        assert!(threaded(&[], Line::Terms, true, 4).is_empty());
    }

    /// A sink that refuses everything, which is what a closed `| head` looks
    /// like from in here.  The run has to come back with the error rather than
    /// hang waiting on a pipeline nobody is draining.
    ///
    /// The corpus is deliberately larger than the writer's one-megabyte buffer.
    /// It used to be 2,000 short colors, about 891 KB, which never filled the
    /// buffer -- so `Broken::write` was not called until `finish` flushed, and
    /// the test passed through the tidy path rather than the mid-run failure it
    /// is named for.  `wrote` is what holds it to that now.
    #[test]
    fn a_failing_sink_is_reported_and_does_not_hang() {
        #[derive(Clone, Default)]
        struct Broken(Arc<Mutex<usize>>);
        impl Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                *self.0.lock().unwrap() += 1;
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let broken = Broken::default();
        let mut pool = Pool::new(Box::new(broken.clone()), Line::Terms, 4);
        let mut failed = None;
        // ~4,000 colors averaging 200 terms is some tens of megabytes, so the
        // writer reaches the sink long before the records run out.
        for (tx, terms) in colors_upto(4_000, 400) {
            let mut snapshot = pool.stage(tx);
            for (exponent, coefficient) in terms {
                snapshot.push_weighted(exponent, coefficient);
            }
            if let Err(e) = pool.dispatch(snapshot) {
                failed = Some(e);
                break;
            }
        }
        let noticed_during_the_run = failed.is_some();
        let outcome = failed.map_or_else(|| pool.finish(), Err);

        assert!(outcome.is_err(), "a sink that refuses everything must fail");
        assert!(
            *broken.0.lock().unwrap() > 0,
            "the sink was never written to, so nothing was actually refused"
        );
        assert!(
            noticed_during_the_run,
            "a sink failing mid-run has to stop the dispatcher, not wait for finish"
        );
    }

    /// A worker that panics must not be reported as a clean run.
    ///
    /// It is the one shape in this design that could truncate the output *and*
    /// exit 0: the panicking worker drops its channel, the writer stops at the
    /// first channel that is closed and empty, and the flush that follows
    /// succeeds.  `shutdown` joins the workers and turns a panic into an error
    /// for exactly this reason.
    #[test]
    fn a_panicking_formatter_is_an_error_rather_than_a_short_run() {
        struct Sink;
        impl Write for Sink {
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                Ok(buf.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        // `append` indexes `weights` for every block when the snapshot carries
        // any, so a snapshot with more blocks than weights panics in the
        // worker.  `push_flat`/`push_weighted` cannot build one -- this reaches
        // past them on purpose, to inject the fault.
        let mut pool = Pool::new(Box::new(Sink), Line::Terms, 2);
        let mut snapshot = pool.stage(1);
        snapshot.push_weighted(7, 0.5);
        snapshot.blocks.push(9);
        pool.dispatch(snapshot).expect("the batch is only staged");

        let outcome = pool.finish();
        assert!(
            outcome.is_err(),
            "a formatter thread that panicked has to be reported, not ignored"
        );
    }
}
