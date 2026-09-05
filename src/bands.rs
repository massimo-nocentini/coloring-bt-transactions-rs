//! # Colors as fixed vectors of weights over bands of block ids
//!
//! The backend behind `--bands <K>`.  [`crate::weighted`] keeps one term per
//! block a transaction's coins descend from, which is exact and is what makes
//! the store grow without bound: a colour deep in the chain names most of the
//! blocks before it.  This keeps one weight per *band* of block ids instead --
//! `K` bands over the whole chain, each `ceil(blocks / K)` blocks wide -- so a
//! colour is a vector of `K` numbers however many blocks it spans.
//!
//! ## What is kept and what is lost
//!
//! A weighted colour is a distribution over blocks.  Binning it is summing the
//! weights within each band, and that commutes with the fold: `wa . A + wb . B`
//! binned is `wa . bin(A) + wb . bin(B)`, because binning is linear.  So the
//! vector this store arrives at for a transaction is *exactly* the binned form of
//! the exact polynomial [`crate::weighted`] would have produced -- not an
//! approximation of the fold, an exact fold of an approximation of the leaves.
//! Every colour still sums to 1, and every moment computed from the bands is
//! the exact moment to within a band width, since each block sits at most half a
//! band from its band's centre.
//!
//! What is lost is the position of a block inside its band, and any structure
//! finer than a band.  With `K = 1024` over the 2022 chain a band is 745 blocks
//! -- about five days.
//!
//! ## Why this is flat in memory
//!
//! The working set of the exact fold is (transactions holding an unspent
//! output) x (blocks per colour), and the second factor grows through the chain.
//! Here it is (transactions holding an unspent output) x `K`, and `K` is a
//! command-line constant.  The multiplier is still the UTXO-bearing set, which
//! is measured at 92.7 million transactions at the end of the 2022 chain; the
//! store is then `92.7M x K x 4` bytes in `f32` -- 95 GB at `K = 256`, 380 GB at
//! `K = 1024`.  Nothing here makes the multiplier smaller.
//!
//! ## Why the combine is the cheap part
//!
//! [`crate::weighted`]'s merge finds the runs two sorted arrays share and scales
//! them, and the finding is a data-dependent branch per element.  Two vectors of
//! the same length have nothing to find: `out[k] = wa . a[k] + wb . b[k]` for
//! every `k`, a flat loop the vectoriser takes whole.  `make asm-check` counts
//! the vector multiplies in this module as it does the weighted store's.
//!
//! ## `f32` or `f64`
//!
//! Generic over the lane type, because the choice is the whole memory budget:
//! `f32` halves the store against `f64`.  The weights of a colour deep in the
//! chain are spread over hundreds of bands and the small ones are around `1e-4`,
//! which `f32` carries to seven digits; the drift of the sum from 1 is what
//! `--stats` reports, so the cost of the narrower lane is measured rather than
//! assumed.

use crate::store::ColorStore;
use std::cell::Cell;
use std::collections::HashSet;
use std::ops::{Add, Mul};
use std::rc::Rc;

/// One past the largest block id in the 2022 chain, which is what a band's
/// width is measured against unless `--blocks` says otherwise.  Fixing the
/// range up front is what makes a band mean the same thing at every record of a
/// run and across runs of different lengths.
pub const CHAIN_BLOCKS: usize = 762_261;

/// A number a band's weight can be held in.
pub trait Lane: Copy + Default + Add<Output = Self> + Mul<Output = Self> + 'static {
    const NAME: &'static str;
    /// Whether the lane is narrower than the `f64` a term is handed on as, so
    /// that a line prints the shortest text that reads back as the *lane* --
    /// nine digits for an `f32` rather than the seventeen its widening needs.
    const NARROW: bool;
    fn from_f64(x: f64) -> Self;
    fn to_f64(self) -> f64;
}

impl Lane for f32 {
    const NAME: &'static str = "f32";
    const NARROW: bool = true;
    #[inline]
    fn from_f64(x: f64) -> Self {
        x as f32
    }
    #[inline]
    fn to_f64(self) -> f64 {
        self as f64
    }
}

impl Lane for f64 {
    const NAME: &'static str = "f64";
    const NARROW: bool = false;
    #[inline]
    fn from_f64(x: f64) -> Self {
        x
    }
    #[inline]
    fn to_f64(self) -> f64 {
        self
    }
}

/// How many bands, and how many blocks each covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub bands: usize,
    pub width: usize,
}

impl Layout {
    /// `bands` bands over `blocks` block ids, the last band possibly short.
    pub fn new(bands: usize, blocks: usize) -> Layout {
        assert!(bands > 0, "a colour needs at least one band");
        assert!(blocks > 0, "a band needs at least one block");
        Layout {
            bands,
            width: blocks.div_ceil(bands),
        }
    }

    #[inline]
    fn band(&self, block: usize) -> usize {
        let band = block / self.width;
        assert!(
            band < self.bands,
            "block {} is past the {} blocks the bands cover; pass --blocks",
            block,
            self.bands * self.width
        );
        band
    }

    /// Where the middle of band `k` is, on the block axis.
    fn centre(&self, band: usize) -> f64 {
        band as f64 * self.width as f64 + (self.width - 1) as f64 / 2.0
    }
}

thread_local! {
    /// [`ColorStore::new`] takes no arguments, so the layout reaches the store
    /// through here: `main` sets it before `run` makes the store, on the thread
    /// that will.  Thread-local rather than global so that tests, which run
    /// concurrently, can each choose their own.
    static LAYOUT: Cell<Option<Layout>> = const { Cell::new(None) };
}

/// Say how the bands are laid out before a [`BandStore`] is made on this thread.
pub fn configure(layout: Layout) {
    LAYOUT.with(|l| l.set(Some(layout)));
}

/// `K` weights, one per band, summing to 1.  Shared like the other stores'
/// colours: a second handle is a refcount bump.
pub type Color<T> = Rc<Tinted<T>>;

/// A colour: the band weights, and beside them the exact first two moments of
/// the block distribution the bands are an approximation of.
///
/// The bands say *shape* at the resolution [`Layout`] fixes; the moments say
/// where the distribution sits and how wide it is, exactly, in blocks.  Both
/// fold through the same convex combination, so carrying the moments costs
/// three `f64` a colour and no arithmetic worth measuring -- and it removes the
/// only error in the binned `--moments` line, which otherwise reports the
/// *centre of a band* as the mean and is wrong by up to half a band.
///
/// They are `f64` whatever the lanes are.  The lanes are narrow to save memory
/// across ninety million live colours; there are three of these, so precision
/// is free and exactness is the whole point of them.
pub struct Tinted<T: Lane> {
    bands: Box<[T]>,
    /// `sum w`, `sum w . b`, `sum w . b^2`, over block ids.
    ///
    /// Linear in the colour, so a convex combination of colours has the convex
    /// combination of their moments -- which is why they can be carried at all.
    /// The effective block count cannot: it is `mass^2 / sum w^2`, quadratic,
    /// and no bounded per-colour state folds it.  See [`crate::main`]'s
    /// `Line::Moments`.
    moments: [f64; 3],
}

impl<T: Lane> Tinted<T> {
    #[inline]
    fn len(&self) -> usize {
        self.bands.len()
    }
}

pub struct BandStore<T: Lane> {
    layout: Layout,
    /// Colours alive, in allocations, and the high-water mark of that.  The
    /// bytes are `live * bands * size_of::<T>()`, plus an `Rc` header each.
    live: usize,
    peak: usize,
    /// The largest `|sum of weights - 1|` seen in a finished colour.
    drift: f64,
    _lane: std::marker::PhantomData<T>,
}

impl<T: Lane> BandStore<T> {
    #[cfg(test)]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    fn track(&mut self, color: Color<T>) -> Color<T> {
        self.live += 1;
        self.peak = self.peak.max(self.live);
        color
    }
}

/// `wa . a + wb . b`, lane by lane, into a fresh colour.
///
/// The two inputs are the same length, so `zip` gives the compiler the trip
/// count up front, and the collect into an `Rc<[T]>` is a single allocation --
/// the iterator knows its length exactly -- filled in place with no staging
/// buffer and no copy.  This is the loop the vector unit is for.
#[inline]
fn combine_into<T: Lane>(a: &Tinted<T>, wa: f64, b: &Tinted<T>, wb: f64) -> Color<T> {
    debug_assert_eq!(a.len(), b.len());
    let (la, lb) = (T::from_f64(wa), T::from_f64(wb));
    let bands: Box<[T]> = a
        .bands
        .iter()
        .zip(&b.bands)
        .map(|(&x, &y)| x * la + y * lb)
        .collect();
    // The same combination, on the three exact lanes.  Done in `f64` and from
    // the `f64` weights rather than the lane-rounded ones, so the moments do
    // not inherit the lanes' precision.
    let mut moments = [0.0f64; 3];
    for k in 0..3 {
        moments[k] = a.moments[k] * wa + b.moments[k] * wb;
    }
    Rc::new(Tinted { bands, moments })
}

#[inline]
fn scale_into<T: Lane>(a: &Tinted<T>, w: f64) -> Color<T> {
    let lane = T::from_f64(w);
    let bands: Box<[T]> = a.bands.iter().map(|&x| x * lane).collect();
    let mut moments = a.moments;
    for m in &mut moments {
        *m *= w;
    }
    Rc::new(Tinted { bands, moments })
}

impl<T: Lane> ColorStore for BandStore<T> {
    type Color = Color<T>;

    const WEIGHTED: bool = true;
    const NARROW: bool = T::NARROW;

    fn new() -> Self {
        let layout = LAYOUT.with(|l| l.get()).unwrap_or_else(|| {
            panic!("bands::configure must be called on this thread before a BandStore is made")
        });
        BandStore {
            layout,
            live: 0,
            peak: 0,
            drift: 0.0,
            _lane: std::marker::PhantomData,
        }
    }

    fn singleton(&mut self, block: usize) -> Color<T> {
        let mut bands = vec![T::default(); self.layout.bands].into_boxed_slice();
        bands[self.layout.band(block)] = T::from_f64(1.0);
        // A coinbase is a point mass on one block, so its moments are exact and
        // trivial -- and they are the block itself, not the band's centre.
        let b = block as f64;
        let color = Rc::new(Tinted {
            bands,
            moments: [1.0, b, b * b],
        });
        self.track(color)
    }

    fn combine(&mut self, a: &Color<T>, wa: f64, b: &Color<T>, wb: f64) -> Color<T> {
        // Two handles on one vector: `wa . C + wb . C` is `(wa + wb) . C`, a
        // scale rather than nothing, as in the weighted store.
        if Rc::ptr_eq(a, b) {
            return self.scale(a, wa + wb);
        }
        let color = combine_into(a, wa, b, wb);
        self.track(color)
    }

    fn scale(&mut self, color: &Color<T>, w: f64) -> Color<T> {
        // Weight 1 changes nothing, and a single-input transaction is exactly
        // that case; sharing keeps the commonest shape free.
        if w == 1.0 {
            return self.share(color);
        }
        let scaled = scale_into(color, w);
        self.track(scaled)
    }

    fn share(&mut self, color: &Color<T>) -> Color<T> {
        Rc::clone(color)
    }

    fn release(&mut self, color: Color<T>) {
        if Rc::strong_count(&color) == 1 {
            self.live -= 1;
        }
    }

    /// The non-zero bands, highest first, as `(band, weight)`.  The exponent is
    /// the band's index and not a block id; [`ColorStore::placement`] says how to
    /// put it back on the block axis.
    fn for_each_term(&self, color: &Color<T>, mut f: impl FnMut(usize, f64)) {
        for k in (0..color.len()).rev() {
            let w = color.bands[k].to_f64();
            if w != 0.0 {
                f(k, w);
            }
        }
    }

    fn placement(&self) -> (f64, f64) {
        (self.layout.width as f64, self.layout.centre(0))
    }

    /// The moments carried beside the bands, which are exact in blocks.
    ///
    /// This is what keeps `--bands --moments` honest: derived from the bands
    /// the mean would be a band centre, out by up to half a band -- 372 blocks
    /// at `--bands 1024`, 1,488 at `--bands 256`.  Carried, it is the same
    /// number the exact fold prints, to floating-point drift.
    fn exact_moments(&self, color: &Color<T>) -> Option<[f64; 3]> {
        Some(color.moments)
    }

    fn observe(&mut self, color: &Color<T>) {
        let total: f64 = color.bands.iter().map(|w| w.to_f64()).sum();
        self.drift = self.drift.max((total - 1.0).abs());
    }

    fn usage(&self) -> (usize, usize) {
        (self.live * self.layout.bands, self.peak * self.layout.bands)
    }

    fn usage_labels(&self) -> (&'static str, &'static str) {
        ("live lanes", "peak lanes")
    }

    fn audit(&self, live: &mut dyn Iterator<Item = &Color<T>>) -> String {
        let mut seen: HashSet<*const Tinted<T>> = HashSet::new();
        let mut reachable = 0;
        for color in live {
            // `&**color` rather than `Rc::as_ptr`, which cannot be named here:
            // the lane parameter is also called `T`, so `Rc::as_ptr` resolves
            // against `Rc<T>` and not `Rc<Tinted<T>>`.
            let at: *const Tinted<T> = &**color;
            if seen.insert(at) {
                reachable += 1;
            }
        }
        let leak = if reachable == self.live {
            format!(
                "{} live colours of {} {} lanes, all reachable",
                self.live, self.layout.bands, T::NAME
            )
        } else {
            format!(
                "LEAK -- {} live colours but only {} reachable",
                self.live, reachable
            )
        };
        format!(
            "audit: {}, worst drift from sum 1 was {:.3e}",
            leak, self.drift
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store<T: Lane>(bands: usize, blocks: usize) -> BandStore<T> {
        configure(Layout::new(bands, blocks));
        BandStore::<T>::new()
    }

    fn dump<T: Lane>(store: &BandStore<T>, c: &Color<T>) -> Vec<(usize, f64)> {
        let mut out = Vec::new();
        store.for_each_term(c, |band, w| out.push((band, w)));
        out
    }

    #[test]
    fn a_coinbase_is_all_of_its_band() {
        let mut s = store::<f64>(8, 800);
        assert_eq!(s.layout().width, 100);
        let c = s.singleton(250);
        assert_eq!(dump(&s, &c), vec![(2, 1.0)]);
        let c = s.singleton(799);
        assert_eq!(dump(&s, &c), vec![(7, 1.0)]);
        let c = s.singleton(0);
        assert_eq!(dump(&s, &c), vec![(0, 1.0)]);
    }

    #[test]
    fn the_last_band_may_be_short() {
        // 10 bands over 95 blocks: width 10, the last band holds 90..94.
        let mut s = store::<f32>(10, 95);
        assert_eq!(s.layout().width, 10);
        let c = s.singleton(94);
        assert_eq!(dump(&s, &c), vec![(9, 1.0)]);
    }

    #[test]
    #[should_panic(expected = "past the")]
    fn a_block_past_the_range_is_refused() {
        let mut s = store::<f32>(10, 100);
        s.singleton(100);
    }

    /// Binning is linear, so the fold of binned leaves is the binned fold.
    #[test]
    fn combine_is_a_weighted_vector_add() {
        let mut s = store::<f64>(4, 40);
        let a = s.singleton(5); // band 0
        let b = s.singleton(35); // band 3
        let ab = s.combine(&a, 0.25, &b, 0.75);
        assert_eq!(dump(&s, &ab), vec![(3, 0.75), (0, 0.25)]);
        let c = s.singleton(15); // band 1
        let abc = s.combine(&ab, 0.5, &c, 0.5);
        assert_eq!(dump(&s, &abc), vec![(3, 0.375), (1, 0.5), (0, 0.125)]);
        let total: f64 = abc.bands.iter().sum();
        assert!((total - 1.0).abs() < 1e-15);
    }

    #[test]
    fn combining_a_colour_with_itself_scales_it() {
        let mut s = store::<f64>(4, 40);
        let a = s.singleton(5);
        let aa = s.combine(&a, 0.4, &a, 0.6);
        assert_eq!(dump(&s, &aa), vec![(0, 1.0)]);
    }

    #[test]
    fn scaling_by_one_shares() {
        let mut s = store::<f32>(4, 40);
        let a = s.singleton(5);
        let same = s.scale(&a, 1.0);
        assert!(Rc::ptr_eq(&a, &same));
        let (live, _) = s.usage();
        assert_eq!(live, 4, "one colour of four lanes");
    }

    #[test]
    fn release_counts_allocations_not_handles() {
        let mut s = store::<f32>(4, 40);
        let a = s.singleton(5);
        let b = s.share(&a);
        assert_eq!(s.usage().0, 4);
        s.release(b);
        assert_eq!(s.usage().0, 4);
        s.release(a);
        assert_eq!(s.usage().0, 0);
    }

    /// The moment a reader computes from `(band, weight)` through `placement`
    /// must land within half a band of the exact one.
    /// The moments are carried, not derived, so they are in blocks and exact
    /// where the bands are a summary.  A colour half at block 5 and half at
    /// block 35 has mean 20 and spread 15 whatever the banding is -- derived
    /// from two bands of a four-band layout it would answer the band centres,
    /// 4.5 and 34.5, and a mean of 19.5.
    #[test]
    fn the_moments_are_exact_in_blocks_not_bands() {
        for bands in [2usize, 4, 8] {
            let mut s = store::<f64>(bands, 40);
            let a = s.singleton(5);
            let b = s.singleton(35);
            let ab = s.combine(&a, 0.5, &b, 0.5);
            let m = s.exact_moments(&ab).expect("the band store carries them");
            assert!((m[0] - 1.0).abs() < 1e-15, "mass at {} bands: {}", bands, m[0]);
            let mean = m[1] / m[0];
            let spread = (m[2] / m[0] - mean * mean).max(0.0).sqrt();
            assert!((mean - 20.0).abs() < 1e-12, "mean at {} bands: {}", bands, mean);
            assert!((spread - 15.0).abs() < 1e-12, "spread at {} bands: {}", bands, spread);
        }
    }

    /// A coinbase's moments are its own block, not the centre of the band it
    /// falls in -- the case that shows the difference most plainly, since the
    /// spread is zero and the mean has nowhere to hide.
    #[test]
    fn a_coinbase_carries_its_own_block() {
        let mut s = store::<f32>(4, 40);
        let c = s.singleton(37);
        let m = s.exact_moments(&c).expect("carried");
        assert_eq!(m, [1.0, 37.0, 37.0 * 37.0]);
        // Its single band is band 3, whose centre is 34.5 -- what the mean
        // would have been had it been derived from the terms.
        let mut seen = Vec::new();
        s.for_each_term(&c, |band, w| seen.push((band, w)));
        assert_eq!(seen, vec![(3, 1.0)]);
        assert!((s.layout().centre(3) - 34.5).abs() < 1e-12);
    }

    /// Scaling and combining have to carry the moments through the same convex
    /// combination the lanes get, or the two halves of a colour disagree.
    #[test]
    fn the_moments_follow_the_same_combination_as_the_lanes() {
        let mut s = store::<f32>(8, 800);
        let a = s.singleton(100);
        let b = s.singleton(500);
        let c = s.singleton(700);
        let ab = s.combine(&a, 0.25, &b, 0.75);
        let abc = s.combine(&ab, 0.5, &c, 0.5);
        let m = s.exact_moments(&abc).unwrap();
        // 0.125 at 100, 0.375 at 500, 0.5 at 700.
        let want_mass = 1.0;
        let want_first = 0.125 * 100.0 + 0.375 * 500.0 + 0.5 * 700.0;
        let want_second = 0.125 * 10_000.0 + 0.375 * 250_000.0 + 0.5 * 490_000.0;
        assert!((m[0] - want_mass).abs() < 1e-12, "mass {}", m[0]);
        assert!((m[1] - want_first).abs() < 1e-9, "first {} want {}", m[1], want_first);
        assert!((m[2] - want_second).abs() < 1e-6, "second {} want {}", m[2], want_second);
    }

    #[test]
    fn placement_puts_a_band_at_its_centre() {
        let s = store::<f64>(8, 800);
        let (scale, offset) = s.placement();
        assert_eq!(scale, 100.0);
        assert_eq!(offset, 49.5);
        // band 2 covers 200..299, centred on 249.5
        assert_eq!(2.0 * scale + offset, 249.5);
    }

    #[test]
    fn audit_sees_every_live_colour_once() {
        let mut s = store::<f32>(4, 40);
        let a = s.singleton(5);
        let b = s.share(&a);
        let c = s.singleton(15);
        let held = vec![a, b, c];
        let report = s.audit(&mut held.iter());
        assert!(report.contains("2 live colours"), "{}", report);
        assert!(report.contains("all reachable"), "{}", report);
    }
}
