//! # The recurrence, as `run()` drives it
//!
//! Every mode of `sketch` colours the records with the loop in `src/main.rs`'s
//! `run()`, copied here rather than shared because `run()` is welded to a
//! `ColorStore` and to its output, and the sketch stores need neither.  What
//! must not differ is everything that decides *which* colour is combined with
//! *what*, so this keeps `run()`'s semantics to the letter:
//!
//! - **The fold order.**  Inputs are walked from the last to the first --
//!   `foldr` -- and handed to the algebra in that order.  For a sketch the
//!   order is the merge's summation order and the step index of
//!   `reduce=step`; for [`crate::exact::Exact`] it is what makes the colours
//!   bit-identical to `--weighted`.
//! - **The shares.**  `amount_i / total`, the total summed in input order as
//!   an `f64`; equal shares `1/n` when the total is zero.
//! - **When a colour dies.**  With the release oracle, an input takes (moves)
//!   its parent's colour out of the map iff [`Oracle::last_reads`] says it is
//!   the last read, and a new colour is kept iff [`Oracle::ever_read`].
//!   Without one, the unspent-output count answers both, exactly as `run()`
//!   does: an input takes the colour at `unspent <= 1`, otherwise decrements
//!   it, and a colour is kept iff the transaction has outputs.  Over the 2022
//!   chain the oracle is the difference between 92.7M live colours and 19.4M.
//! - **Duplicate tx ids.**  The two BIP-30 coinbases reuse an earlier id; the
//!   insert displaces the older colour, which is released.
//! - **Errors.**  A record past the oracle's end, and an input that spends a
//!   transaction with no colour, fail with `run()`'s messages.
//!
//! The parents a record spends are held as owned handles in `ops` until the
//! algebra has made the colour, then released -- a moved colour is freed right
//! there, a shared one only loses a reference.

use crate::algebra::Algebra;
use crate::feed::Batch;
use crate::oracle::Oracle;
use crate::sexp::Input;
use std::collections::HashMap;
use std::hash::{BuildHasherDefault, Hasher};
use std::io;
use std::sync::Arc;
use std::time::Instant;

/// A multiplicative hash for tx ids.  The map is the driver's hot lookup, and
/// SipHash's resistance to chosen keys buys nothing over a file of sequential
/// integers; the product's high bits, folded down, are well mixed for
/// hashbrown's bucket index and its 7-bit tag alike.
#[derive(Default, Clone, Copy)]
pub struct TxHasher(u64);

impl Hasher for TxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x100_0000_01b3);
        }
    }
    #[inline]
    fn write_usize(&mut self, x: usize) {
        let h = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        self.0 = h ^ (h >> 29);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }
}

pub type TxMap<V> = HashMap<usize, V, BuildHasherDefault<TxHasher>>;

/// How often `--stats` reports, in records.
pub const STATS_EVERY: u64 = 1_000_000;

pub struct Driver<A: Algebra> {
    pub alg: A,
    colors: TxMap<(A::C, usize)>,
    last_reads: Vec<bool>,
    ops: Vec<(A::C, f64)>,
    oracle: Option<Arc<Oracle>>,
    /// Records coloured so far; the next record's index.
    pub records: u64,
    /// `Some(label)` prints progress to stderr under it.
    stats: Option<String>,
    started: Instant,
    since: Instant,
    /// Seconds spent inside [`Driver::step`].
    pub busy: f64,
}

impl<A: Algebra> Driver<A> {
    pub fn new(alg: A, oracle: Option<Arc<Oracle>>, stats: Option<String>) -> Self {
        let now = Instant::now();
        Driver {
            alg,
            colors: TxMap::default(),
            last_reads: Vec::new(),
            ops: Vec::new(),
            oracle,
            records: 0,
            stats,
            started: now,
            since: now,
            busy: 0.0,
        }
    }

    pub fn map_len(&self) -> usize {
        self.colors.len()
    }

    /// Colour every record of `batch`, handing each colour to `sink` as
    /// `(record index, tx id, colour, algebra)` before it is kept or dropped.
    pub fn batch(
        &mut self,
        batch: &Batch,
        mut sink: impl FnMut(u64, usize, &A::C, &A) -> io::Result<()>,
    ) -> io::Result<()> {
        assert_eq!(batch.first, self.records, "batches arrive in order");
        for (i, r) in batch.recs.iter().enumerate() {
            self.step(r.tx, r.block, r.outputs, batch.inputs_of(i), &mut sink)?;
        }
        Ok(())
    }

    /// One record.
    pub fn step(
        &mut self,
        tx: usize,
        block: usize,
        outputs: usize,
        inputs: &[Input],
        sink: &mut impl FnMut(u64, usize, &A::C, &A) -> io::Result<()>,
    ) -> io::Result<()> {
        let t0 = Instant::now();
        let rec = self.records;
        if let Some(o) = &self.oracle {
            if rec >= o.records() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the release oracle covers {} records and this run reached {}; \
                         it was written for different input",
                        o.records(),
                        rec + 1
                    ),
                ));
            }
        }

        let color = if inputs.is_empty() {
            let block = u32::try_from(block)
                .unwrap_or_else(|_| panic!("block id {} does not fit in a u32", block));
            self.alg.singleton(block)
        } else {
            let total: f64 = inputs.iter().map(|i| i.amount as f64).sum();
            let n = inputs.len() as f64;
            let share = |i: &Input| if total > 0.0 { i.amount as f64 / total } else { 1.0 / n };
            if let Some(o) = &self.oracle {
                o.last_reads(rec, inputs, &mut self.last_reads);
            }
            debug_assert!(self.ops.is_empty());
            for i in (0..inputs.len()).rev() {
                let prev = inputs[i].prev_tx_id;
                let entry = match self.colors.get_mut(&prev) {
                    Some(e) => e,
                    None => {
                        // Give back what was taken so far before failing.
                        for (h, _) in self.ops.drain(..) {
                            self.alg.release(h);
                        }
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("transaction {} spends unknown transaction {}", tx, prev),
                        ));
                    }
                };
                let taken = match &self.oracle {
                    Some(_) => self.last_reads[i],
                    None => entry.1 <= 1,
                };
                let s = share(&inputs[i]);
                if !taken {
                    entry.1 = entry.1.saturating_sub(1);
                    let h = clone_handle(&entry.0);
                    self.ops.push((h, s));
                } else {
                    let (h, _) = self.colors.remove(&prev).expect("just looked it up");
                    self.ops.push((h, s));
                }
            }
            let c = self.alg.record(&self.ops, rec);
            for (h, _) in self.ops.drain(..) {
                self.alg.release(h);
            }
            c
        };

        self.busy += t0.elapsed().as_secs_f64();
        sink(rec, tx, &color, &self.alg)?;
        let t1 = Instant::now();

        let keep = match &self.oracle {
            Some(o) => o.ever_read(tx),
            None => outputs > 0,
        };
        if keep {
            if let Some((displaced, _)) = self.colors.insert(tx, (color, outputs)) {
                self.alg.release(displaced);
            }
        } else {
            self.alg.release(color);
        }

        self.records += 1;
        self.busy += t1.elapsed().as_secs_f64();
        if self.records % STATS_EVERY == 0 {
            if let Some(label) = &self.stats {
                let now = Instant::now();
                let u = self.alg.usage();
                let w = self.alg.work();
                eprintln!(
                    "{:>9.1}s {:>11} rec {:>9.0} rec/s  [{}] live {} peak {} sketches {} map {} reductions {} k+-1 {}",
                    self.started.elapsed().as_secs_f64(),
                    self.records,
                    STATS_EVERY as f64 / now.duration_since(self.since).as_secs_f64(),
                    label,
                    u.live_entries,
                    u.peak_entries,
                    u.live_sketches,
                    self.colors.len(),
                    w.reductions,
                    w.off_by_one
                );
                self.since = now;
            }
        }
        Ok(())
    }

    /// Release every colour still held, so a store's counters end at zero.
    pub fn finish(mut self) -> A {
        for (_, (c, _)) in self.colors.drain() {
            self.alg.release(c);
        }
        self.alg
    }
}

/// A second owned handle on a colour.  Every `C` here is an `Rc`, so this is
/// a refcount bump; a trait bound rather than `ColorStore::share` keeps the
/// driver free of the store.
fn clone_handle<C: Clone>(c: &C) -> C {
    c.clone()
}
