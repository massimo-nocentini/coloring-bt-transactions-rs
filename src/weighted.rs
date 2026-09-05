//! # Colors as weighted distributions over block ids
//!
//! The backend behind `--weighted`.  Where [`crate::colorset`] answers *which*
//! blocks a transaction's coins came from, this answers *how much of it* came
//! from each.
//!
//! ## What the weights mean
//!
//! A transaction spending inputs of amounts `a_1..a_n` from transactions with
//! colors `C_1..C_n` is colored
//!
//! ```text
//!     C  =  sum_i  (a_i / sum_j a_j) . C_i
//! ```
//!
//! so each ancestor contributes in proportion to how much of this transaction's
//! value flowed through it.  A coinbase is `{ block: 1 }` — all of it was minted
//! in one place.
//!
//! Two things follow, and both are worth stating because they are what make the
//! representation checkable:
//!
//! - **Every color sums to 1.**  The weights of a fold sum to 1 by construction
//!   and each `C_i` sums to 1 by induction, so the result does too.  This is an
//!   invariant, not an aspiration: `--stats` reports the worst drift it saw.
//! - **The support is unchanged.**  Which blocks appear is exactly what the
//!   unweighted backend computes; only the coefficients differ.  So colors grow
//!   no faster here than there, and nothing is pruned — same as
//!   [`crate::poly`], which never drops a cancelled term either.
//!
//! ## Layout: two arrays, not one array of pairs
//!
//! A color is a sorted `u32` of block ids beside an `f64` of weights, at the
//! same indices.  Interleaving them into one array of `(u32, f64)` would be one
//! allocation instead of three, but it would pad each pair to 16 bytes and, more
//! to the point, put a block id between every two weights.  The arithmetic here
//! runs over weights alone; keeping them contiguous is what lets
//! [`crate::simd::scale_into`] and [`crate::simd::scale_add_into`] work a vector
//! at a time.
//!
//! ## Where the vector work actually is
//!
//! Not in the merge.  Deciding which of two sorted key arrays advances is a
//! data-dependent branch per element, and a measured attempt at a vector merge
//! for the unweighted backend came out *slower* than the branchless scalar one.
//! What weighting adds is arithmetic, and the arithmetic is elementwise:
//!
//! - a run of blocks only one side has is that side's weights times a scalar;
//! - a run both sides have is `wa * A + wb * B`.
//!
//! Both are flat loops over contiguous `f64`, which vectorise.  The merge finds
//! the runs; the vector unit does them.
//!
//! That the runs are long is not an assumption: counted over records 1.4-1.5 M
//! of the 2022 chain, 97% of the terms a merge writes arrive in runs, the
//! one-sided ones averaging 1,900 blocks and the shared ones 140.  Colours at
//! that depth are near-copies of a few common ancestors, not independent
//! mixtures.  A block-at-a-time gear for the short runs was built and measured
//! -- 12 to 9 ns a term on synthetic colours that *were* independent mixtures,
//! and nothing on the real chain, where it could reach 3% of the terms -- and
//! dropped.
//!
//! ## Where the memory goes, and how it is allocated
//!
//! A colour is two boxed slices, allocated exactly once at their final length
//! -- or at an upper bound and shrunk in place, for a merge, whose length is
//! not known until it has run.  Nothing is staged and copied.  That relies on
//! the allocator shrinking without moving and without waste, which jemalloc
//! does and glibc's malloc and mimalloc were measured not to (see the features
//! in `Cargo.toml`): on the same 1.5 M records the resident set was 32.0 GB
//! under jemalloc, 37.4 GB under glibc and 39.8 GB under mimalloc.  The store
//! is what runs into this machine's memory, so the allocator is not a detail.

use crate::simd;
use std::mem::MaybeUninit;
use crate::store::ColorStore;
use std::collections::HashSet;
use std::rc::Rc;

/// Blocks ascending, weights alongside.  Always the same length.
pub struct Terms {
    blocks: Box<[u32]>,
    weights: Box<[f64]>,
}

impl Terms {
    fn len(&self) -> usize {
        self.blocks.len()
    }
}

pub type Color = Rc<Terms>;

pub struct WeightedSets {
    live: usize,
    peak: usize,
    /// The largest `|sum of weights - 1|` seen in a finished color.  Every color
    /// is a distribution, so this is the accumulated floating-point drift and
    /// nothing else; if it ever stops being tiny, the arithmetic is wrong.
    drift: f64,
}

impl WeightedSets {
    /// Freeze a finished pair of arrays into a colour.
    ///
    /// The vectors arrive with their length set to what was written and their
    /// capacity at whatever upper bound the caller had, and `into_boxed_slice`
    /// gives the difference back — a `realloc` downward, which jemalloc does in
    /// place and to size (glibc and mimalloc do not; see the module docs).
    /// Nothing is copied here, and that is the point: the merge used to build
    /// into a staging buffer that was zeroed first and copied out after, and at
    /// depth in the 2022 chain the zeroing, the copy and the `malloc` behind it
    /// were half the fold.
    fn intern(&mut self, blocks: Vec<u32>, weights: Vec<f64>) -> Color {
        debug_assert_eq!(blocks.len(), weights.len());
        let color = Rc::new(Terms {
            blocks: blocks.into_boxed_slice(),
            weights: weights.into_boxed_slice(),
        });
        self.live += color.len();
        self.peak = self.peak.max(self.live);
        color
    }
}

/// `dst[..src.len()] = src`, into memory that has not been initialised.
///
/// What `copy_from_slice` is for the block ids once the destination is a fresh
/// allocation rather than a zeroed buffer.  The standard library has this as
/// `MaybeUninit::copy_from_slice`, but not on stable yet.
#[inline]
fn put(dst: &mut [MaybeUninit<u32>], src: &[u32]) {
    assert!(dst.len() >= src.len(), "run of {} into room for {}", src.len(), dst.len());
    // SAFETY: `src` is a live slice of `src.len()` initialised `u32`s, `dst` has
    // room for at least that many, and the two cannot overlap -- one borrows a
    // finished colour, the other the allocation being built.
    unsafe {
        std::ptr::copy_nonoverlapping(src.as_ptr(), dst.as_mut_ptr() as *mut u32, src.len());
    }
}

/// `wa * a + wb * b` into fresh, uninitialised output slices, answering how
/// many terms it wrote.  Every element below that count is initialised on
/// return; nothing above it is touched.
///
/// One pass over both inputs.  A run that only one side has is copied and scaled
/// through [`simd::scale_into_uninit`]; a run they share goes through
/// [`simd::scale_add_into_uninit`].  Finding those runs is the scalar part, and
/// it is the part that resists vectorising — which is why the loop below looks
/// for *runs* rather than stepping one block at a time: the longer the runs, the
/// more of the work lands in the vector helpers.
///
/// Two things about how the runs are found, both measured on the real chain
/// where colours reach a hundred thousand blocks:
///
/// - the one-sided run is found by galloping ([`simd::leading_below`]) rather
///   than bisecting the whole remainder, because bisection's probes land far
///   ahead of where the merge is streaming and miss the cache, and most runs are
///   short next to the colour;
/// - the shared run is found four blocks at a time ([`simd::common_prefix`]),
///   since a scalar walk of it was one dependent compare per block over stretches
///   that are exactly as long as the colours are shared, which is most of them.
fn combine_into(
    out_blocks: &mut [MaybeUninit<u32>],
    out_weights: &mut [MaybeUninit<f64>],
    a: &Terms,
    wa: f64,
    b: &Terms,
    wb: f64,
) -> usize {
    let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);

    while i < a.len() && j < b.len() {
        let (ka, kb) = (a.blocks[i], b.blocks[j]);

        if ka < kb {
            // A run of A that B does not reach yet: scale and copy it whole.
            let run = simd::leading_below(&a.blocks[i..], kb);
            put(&mut out_blocks[k..k + run], &a.blocks[i..i + run]);
            simd::scale_into_uninit(
                &mut out_weights[k..k + run],
                &a.weights[i..i + run],
                wa,
            );
            i += run;
            k += run;
        } else if kb < ka {
            let run = simd::leading_below(&b.blocks[j..], ka);
            put(&mut out_blocks[k..k + run], &b.blocks[j..j + run]);
            simd::scale_into_uninit(
                &mut out_weights[k..k + run],
                &b.weights[j..j + run],
                wb,
            );
            j += run;
            k += run;
        } else {
            // A run both sides carry, block for block.  Common: colors that
            // share an ancestor share whole stretches of its support.
            let run = simd::common_prefix(&a.blocks[i..], &b.blocks[j..]);
            put(&mut out_blocks[k..k + run], &a.blocks[i..i + run]);
            simd::scale_add_into_uninit(
                &mut out_weights[k..k + run],
                &a.weights[i..i + run],
                wa,
                &b.weights[j..j + run],
                wb,
            );
            i += run;
            j += run;
            k += run;
        }
    }

    // Whatever is left of one side is a single scaled run.
    let rest_a = a.len() - i;
    put(&mut out_blocks[k..k + rest_a], &a.blocks[i..]);
    simd::scale_into_uninit(&mut out_weights[k..k + rest_a], &a.weights[i..], wa);
    k += rest_a;

    let rest_b = b.len() - j;
    put(&mut out_blocks[k..k + rest_b], &b.blocks[j..]);
    simd::scale_into_uninit(&mut out_weights[k..k + rest_b], &b.weights[j..], wb);
    k + rest_b
}

impl ColorStore for WeightedSets {
    type Color = Color;

    const WEIGHTED: bool = true;

    fn new() -> Self {
        WeightedSets {
            live: 0,
            peak: 0,
            drift: 0.0,
        }
    }

    fn singleton(&mut self, block: usize) -> Color {
        let block = u32::try_from(block)
            .unwrap_or_else(|_| panic!("block id {} does not fit in a u32 — see weighted", block));
        self.intern(vec![block], vec![1.0])
    }

    fn combine(&mut self, a: &Color, wa: f64, b: &Color, wb: f64) -> Color {
        if a.len() == 0 {
            return self.scale(b, wb);
        }
        if b.len() == 0 {
            return self.scale(a, wa);
        }

        // Unlike the unweighted store there is no shortcut for `a` and `b` being
        // the same allocation: `wa * C + wb * C` is `(wa + wb) * C`, which is
        // still a scale rather than nothing.
        if Rc::ptr_eq(a, b) {
            return self.scale(a, wa + wb);
        }

        // The result has at most as many terms as the two sides together, and
        // exactly how many is not known until the merge has run.  So: allocate
        // the bound, merge straight into it, and let `intern` hand the unused
        // tail back.
        let bound = a.len() + b.len();
        let mut blocks: Vec<u32> = Vec::with_capacity(bound);
        let mut weights: Vec<f64> = Vec::with_capacity(bound);
        let written = combine_into(
            blocks.spare_capacity_mut(),
            weights.spare_capacity_mut(),
            a,
            wa,
            b,
            wb,
        );
        // SAFETY: `combine_into` initialised exactly the first `written`
        // elements of both, and `written <= bound`.
        unsafe {
            blocks.set_len(written);
            weights.set_len(written);
        }
        self.intern(blocks, weights)
    }

    fn scale(&mut self, color: &Color, w: f64) -> Color {
        // Weight 1 changes nothing, and a transaction with a single input is
        // exactly that case -- its one ancestor contributes all of the value.
        // Sharing rather than rebuilding keeps the commonest shape free.
        if w == 1.0 {
            return self.share(color);
        }
        // The length is known here, so both arrays are allocated exactly: the
        // blocks are a straight copy and the weights are scaled into place.
        let n = color.len();
        let blocks = color.blocks.to_vec();
        let mut weights: Vec<f64> = Vec::with_capacity(n);
        simd::scale_into_uninit(weights.spare_capacity_mut(), &color.weights, w);
        // SAFETY: `scale_into_uninit` wrote all `n` elements.
        unsafe { weights.set_len(n) };
        self.intern(blocks, weights)
    }

    fn share(&mut self, color: &Color) -> Color {
        Rc::clone(color)
    }

    /// Every finished color is a distribution, so its weights sum to 1 and
    /// anything else is accumulated floating-point drift.  Tracking the worst of
    /// it turns the invariant into something a run can actually report.
    fn observe(&mut self, color: &Color) {
        let total: f64 = color.weights.iter().sum();
        self.drift = self.drift.max((total - 1.0).abs());
    }

    fn release(&mut self, color: Color) {
        if Rc::strong_count(&color) == 1 {
            self.live -= color.len();
        }
    }

    fn for_each_term(&self, color: &Color, mut f: impl FnMut(usize, f64)) {
        // Stored ascending because that is what a merge wants; printed in
        // decreasing exponent order, like the polynomial it stands in for.
        for k in (0..color.len()).rev() {
            f(color.blocks[k] as usize, color.weights[k]);
        }
    }

    fn usage(&self) -> (usize, usize) {
        (self.live, self.peak)
    }

    fn usage_labels(&self) -> (&'static str, &'static str) {
        ("live terms", "peak terms")
    }

    fn audit(&self, live: &mut dyn Iterator<Item = &Color>) -> String {
        let mut seen: HashSet<*const Terms> = HashSet::new();
        let mut reachable = 0;
        for color in live {
            if seen.insert(Rc::as_ptr(color)) {
                reachable += color.len();
            }
        }
        let leak = if reachable == self.live {
            format!("{} live terms, all reachable", self.live)
        } else {
            format!(
                "LEAK -- {} live terms but only {} reachable",
                self.live, reachable
            )
        };
        format!("audit: {}, worst drift from sum 1 was {:.3e}", leak, self.drift)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn color(store: &mut WeightedSets, terms: &[(u32, f64)]) -> Color {
        let (blocks, weights): (Vec<u32>, Vec<f64>) = terms.iter().copied().unzip();
        store.intern(blocks, weights)
    }

    fn dump(c: &Color) -> Vec<(u32, f64)> {
        c.blocks
            .iter()
            .copied()
            .zip(c.weights.iter().copied())
            .collect()
    }

    /// What a weighted union is, written out slowly: gather every block from
    /// both sides, scale, and add where they meet.
    fn oracle(a: &[(u32, f64)], wa: f64, b: &[(u32, f64)], wb: f64) -> Vec<(u32, f64)> {
        let mut all: std::collections::BTreeMap<u32, f64> = std::collections::BTreeMap::new();
        for &(block, weight) in a {
            *all.entry(block).or_insert(0.0) += weight * wa;
        }
        for &(block, weight) in b {
            *all.entry(block).or_insert(0.0) += weight * wb;
        }
        all.into_iter().collect()
    }

    fn check(a: &[(u32, f64)], wa: f64, b: &[(u32, f64)], wb: f64) {
        let mut store = WeightedSets::new();
        let ca = color(&mut store, a);
        let cb = color(&mut store, b);
        let combined = store.combine(&ca, wa, &cb, wb);
        let got = dump(&combined);
        let want = oracle(a, wa, b, wb);
        assert_eq!(got.len(), want.len(), "a={:?} b={:?}", a, b);
        for (g, w) in got.iter().zip(&want) {
            assert_eq!(g.0, w.0, "blocks differ: {:?} vs {:?}", got, want);
            assert!(
                (g.1 - w.1).abs() < 1e-12,
                "weight for block {} is {} not {}",
                g.0,
                g.1,
                w.1
            );
        }
    }

    #[test]
    fn combines_the_shapes_the_run_finder_is_about() {
        let a: Vec<(u32, f64)> = (0..10).map(|b| (b, 0.1)).collect();
        let b: Vec<(u32, f64)> = (5..15).map(|b| (b, 0.1)).collect();
        // fully shared, so one long matching run
        check(&a, 0.25, &a, 0.75);
        // partly shared
        check(&a, 0.5, &b, 0.5);
        // disjoint, either way round
        check(&a, 0.3, &(100..110).map(|b| (b, 0.1)).collect::<Vec<_>>(), 0.7);
        check(&(100..110).map(|b| (b, 0.1)).collect::<Vec<_>>(), 0.7, &a, 0.3);
        // interleaved, so every run is length one -- the worst case for the
        // vector helpers, and the one most likely to be mis-indexed
        check(
            &(0..20).filter(|b| b % 2 == 0).map(|b| (b, 0.1)).collect::<Vec<_>>(),
            0.5,
            &(0..20).filter(|b| b % 2 == 1).map(|b| (b, 0.1)).collect::<Vec<_>>(),
            0.5,
        );
        // one side a single block
        check(&a, 0.9, &[(7, 1.0)], 0.1);
        check(&[(7, 1.0)], 0.1, &a, 0.9);
    }

    #[test]
    fn combines_random_colors() {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..2000 {
            let universe = if next() % 2 == 0 { 24 } else { 5000 };
            let build = |n: u64, next: &mut dyn FnMut() -> u64| {
                let mut blocks: Vec<u32> =
                    (0..n).map(|_| (next() % universe) as u32).collect();
                blocks.sort_unstable();
                blocks.dedup();
                let count = blocks.len() as f64;
                blocks.into_iter().map(|b| (b, 1.0 / count)).collect::<Vec<_>>()
            };
            let na = next() % 40;
            let a = build(na, &mut next);
            let nb = next() % 40;
            let b = build(nb, &mut next);
            let wa = (next() % 1000) as f64 / 1000.0;
            check(&a, wa, &b, 1.0 - wa);
        }
    }

    /// Long enough, and dense enough, that every run length the finders see
    /// shows up: runs of one where the two sides interleave, long one-sided
    /// stretches, long shared ones.  The oracle knows nothing about runs, which
    /// is what makes it an oracle.
    #[test]
    fn combines_random_colors_of_every_run_length() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for round in 0..300 {
            // A block is kept with probability `p`, and `p` sweeps from sparse
            // to nearly full so every run length shows up.
            let mut build = |universe: u64, p: u64| -> Vec<(u32, f64)> {
                let blocks: Vec<u32> = (0..universe)
                    .filter(|_| next() % 100 < p)
                    .map(|b| b as u32)
                    .collect();
                let count = blocks.len().max(1) as f64;
                blocks.into_iter().map(|b| (b, 1.0 / count)).collect()
            };
            let universe = 200 + (round % 7) * 61;
            let a = build(universe, 5 + (round * 13) % 95);
            let b = build(universe, 5 + (round * 29) % 95);
            let wa = (next() % 1000) as f64 / 1000.0;
            check(&a, wa, &b, 1.0 - wa);
        }
    }

    /// The property the whole representation rests on.
    #[test]
    fn a_convex_combination_of_distributions_is_a_distribution() {
        let mut store = WeightedSets::new();
        let a = color(&mut store, &[(1, 0.25), (4, 0.75)]);
        let b = color(&mut store, &[(4, 0.5), (9, 0.5)]);
        for wa in [0.0, 0.1, 0.5, 0.9, 1.0] {
            let c = store.combine(&a, wa, &b, 1.0 - wa);
            let total: f64 = c.weights.iter().sum();
            assert!((total - 1.0).abs() < 1e-15, "wa={} total={}", wa, total);
        }
    }

    #[test]
    fn scaling_by_one_shares_rather_than_rebuilding() {
        let mut store = WeightedSets::new();
        let a = color(&mut store, &[(1, 0.5), (2, 0.5)]);
        let same = store.scale(&a, 1.0);
        assert!(Rc::ptr_eq(&a, &same), "weight 1 should not rebuild");
    }

    #[test]
    fn combining_a_color_with_itself_scales_it() {
        let mut store = WeightedSets::new();
        let a = color(&mut store, &[(1, 0.25), (4, 0.75)]);
        let c = store.combine(&a, 0.4, &a, 0.6);
        assert_eq!(dump(&c), vec![(1, 0.25), (4, 0.75)]);
    }

    #[test]
    fn terms_come_out_in_decreasing_block_order() {
        let mut store = WeightedSets::new();
        let a = color(&mut store, &[(2, 0.25), (7, 0.75)]);
        let mut seen = Vec::new();
        store.for_each_term(&a, |exp, coeff| seen.push((exp, coeff)));
        assert_eq!(seen, vec![(7, 0.75), (2, 0.25)]);
    }

    /// Not a test: a microbenchmark of `combine` on the shapes the run finders
    /// see, so that a change to them can be timed without a four-minute run
    /// over the real chain -- and on a machine other people are using, whose
    /// wall clock is noisy where this is not.  `cargo test --release
    /// merge_bench -- --ignored --nocapture`, before and after.  Prints the
    /// best of several repeats in nanoseconds per output term.
    #[test]
    #[ignore]
    fn merge_bench() {
        use std::time::Instant;
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        // Every block of `0..universe` kept with probability `density`.
        let mut dense = |universe: u32, density: f64| -> Vec<(u32, f64)> {
            let kept: Vec<u32> = (0..universe)
                .filter(|_| (next() % 1_000_000) as f64 / 1_000_000.0 < density)
                .collect();
            let w = 1.0 / kept.len() as f64;
            kept.into_iter().map(|b| (b, w)).collect()
        };
        let shapes: Vec<(&str, Vec<(u32, f64)>, Vec<(u32, f64)>)> = vec![
            ("two mixed colours, 77% of 130k blocks each (L3)", dense(130_000, 0.77), dense(130_000, 0.77)),
            ("two mixed colours, 77% of 40k blocks each (L2)", dense(40_000, 0.77), dense(40_000, 0.77)),
            ("two well-mixed colours, 92% of 130k blocks each (runs ~5)", dense(130_000, 0.92), dense(130_000, 0.92)),
            ("mixed 77% of 130k against a half-dense 50% one", dense(130_000, 0.77), dense(130_000, 0.5)),
            ("mixed 77% of 130k against a sparse 4% one", dense(130_000, 0.77), dense(130_000, 0.04)),
            ("nearly identical 100k-block colours (long shared runs)", dense(130_000, 0.77), Vec::new()),
            ("disjoint interleaved 50k each (runs of one)", (0..100_000).filter(|b| b % 2 == 0).map(|b| (b, 2e-5)).collect(), (0..100_000).filter(|b| b % 2 == 1).map(|b| (b, 2e-5)).collect()),
        ];
        for (name, a, b) in shapes {
            let b = if b.is_empty() {
                // Nearly identical: drop every 500th block of `a`.
                a.iter().enumerate().filter(|(k, _)| k % 500 != 0).map(|(_, t)| *t).collect()
            } else {
                b
            };
            let mut store = WeightedSets::new();
            let ca = color(&mut store, &a);
            let cb = color(&mut store, &b);
            let mut best = f64::INFINITY;
            let mut out_len = 0;
            for _ in 0..7 {
                let started = Instant::now();
                let reps = 20;
                for _ in 0..reps {
                    let c = store.combine(&ca, 0.6, &cb, 0.4);
                    out_len = c.len();
                    store.release(c);
                }
                let per_term = started.elapsed().as_secs_f64() * 1e9 / (reps * out_len) as f64;
                best = best.min(per_term);
            }
            eprintln!(
                "merge_bench: {:<58} a={:>7} b={:>7} out={:>7}  {:.3} ns/term",
                name, a.len(), b.len(), out_len, best
            );
        }
    }
}
