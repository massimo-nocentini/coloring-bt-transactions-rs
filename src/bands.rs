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
//! ## What is carried exactly beside the bands
//!
//! The same argument says what *can* be kept exactly: any number that is linear
//! in the colour folds through the convex combination the way the bands do, so
//! a store can carry it beside them in one `f64` and never derive it from band
//! indices at all.  Two sets of such numbers are carried.
//!
//! - The first two moments, always: `sum w`, `sum w . b` and `sum w . b^2`,
//!   which is what keeps `--moments` and `--sum` exact in blocks.  See
//!   [`Tinted`].
//! - The seven values of `--laplace`, when it asks: `sum w . exp(-s . b / L)`
//!   for each `s` in [`LAPLACE`], with `L` fixed at [`CHAIN_BLOCKS`].  A
//!   coinbase starts at [`laplace_at`] of its own block, and from there they
//!   are scaled and combined with exactly the weights the moments are.
//!
//! The second set is a type parameter, [`CARRY_LAPLACE`] against
//! [`CARRY_MOMENTS`], rather than something every colour holds.  Seven `f64` a
//! colour is 56 bytes across ninety million live colours, five gigabytes at the
//! end of the 2022 chain and a twentieth of the store at `--bands 256` -- worth
//! paying for a run that prints them and for no other.  Only `--laplace` makes
//! the wider store, so every other run is the one it was, byte for byte in
//! its output and to the byte in memory.
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
use std::sync::OnceLock;

/// One past the largest block id in the 2022 chain, which is what a band's
/// width is measured against unless `--blocks` says otherwise.  Fixing the
/// range up front is what makes a band mean the same thing at every record of a
/// run and across runs of different lengths.
pub const CHAIN_BLOCKS: usize = 762_261;

/// The `s` at which `--laplace` evaluates a colour, `F(s) = sum_b w_b .
/// exp(-s . b / L)` with `L` = [`CHAIN_BLOCKS`].
///
/// Doublings, because what `s` sets is the stretch of history `F(s)` resolves:
/// `exp(-s . b / L)` falls by a factor `e` every `L / s` blocks, so `s = 0.5`
/// barely moves across the whole chain -- it is the mean, nearly linearly --
/// and `s = 32` has spent all of its range in the first 24,000 blocks and says
/// how much of a colour is that old.  Each step halves the stretch.  If one
/// column is wanted rather than seven, it is `s = 4`: over block ids spread
/// evenly across the chain, `F(s)` varies most at `s` = 3.25, and 4 is the
/// grid point nearest.
///
/// `L` is the 2022 chain and not the run's block count, so a value means the
/// same thing in every run, the way a band does.
pub const LAPLACE: [f64; 7] = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0, 32.0];

/// `exp(-s . block / L)` for every `s` in [`LAPLACE`]: what a coinbase minted
/// in `block` is worth at each, and what every term of a colour is weighted by.
///
/// One definition for both of the stores that compute it -- [`crate::emit`]
/// sums it over the terms of an exact colour, and a [`BandStore`] seeds a
/// coinbase with it -- so the two agree bit for bit wherever a colour is a
/// single block.  The order of the arithmetic is not a choice to worry about:
/// every `s` is a power of two and `block` an integer, so `s . block` is exact
/// and `s . (block / L)` would round to the same bits.
///
/// Every value is in `[exp(-32), 1]` for a block inside the chain, positive and
/// far from either end of an `f64`, so the sums `--laplace` prints are of
/// positive terms and lose nothing to cancellation.
#[inline]
pub fn laplace_at(block: f64) -> [f64; 7] {
    let mut at = [0.0f64; 7];
    for (value, s) in at.iter_mut().zip(LAPLACE) {
        *value = (-(s * block) / CHAIN_BLOCKS as f64).exp();
    }
    at
}

/// [`laplace_at`] of every block in the chain, computed the first time an
/// exact colour is printed under `--laplace` and kept: 5.3 million `exp`s once,
/// about 40 ms, rather than seven for every term of every line.
///
/// That is the difference between the form costing what `--moments` costs and
/// costing ten times it.  Over the first 200,000 records of the 2022 chain,
/// `--weighted --laplace` took 22.25s with the `exp`s in the loop against
/// `--moments`' 2.08s; the terms of a colour are blocks, and the same blocks
/// recur in every colour downstream of them, so the work was the same few
/// hundred thousand values recomputed a billion times.  The table holds the
/// very values [`laplace_at`] returns, so a line is the same bits either way.
///
/// 42.7 MB, in a run whose store is gigabytes, and only in a run that walks
/// terms under `--laplace` -- a band store hands its values over and never
/// reaches here.  A block past the table, from a chain longer than 2022's, is
/// computed on the spot by [`laplace_of`] rather than refused.
fn laplace_table() -> &'static [[f64; 7]] {
    static TABLE: OnceLock<Box<[[f64; 7]]>> = OnceLock::new();
    TABLE.get_or_init(|| (0..CHAIN_BLOCKS).map(|b| laplace_at(b as f64)).collect())
}

/// [`laplace_at`] of a block id, out of [`laplace_table`] when it is in it.
#[inline]
pub fn laplace_of(block: usize) -> [f64; 7] {
    match laplace_table().get(block) {
        Some(at) => *at,
        None => laplace_at(block as f64),
    }
}

/// How many exact `f64` a colour carries beside its bands: the three moments,
/// or the three moments and the seven [`LAPLACE`] values.  See the module docs
/// for why the second is a type and not a flag.
pub const CARRY_MOMENTS: usize = 3;
pub const CARRY_LAPLACE: usize = CARRY_MOMENTS + LAPLACE.len();

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
pub type Color<T, const E: usize> = Rc<Tinted<T, E>>;

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
///
/// Under `--laplace` the same array runs on to the seven [`LAPLACE`] values,
/// which are linear in the colour for the same reason and so ride the same
/// combination: `E` is [`CARRY_LAPLACE`] there and [`CARRY_MOMENTS`] everywhere
/// else.
pub struct Tinted<T: Lane, const E: usize> {
    bands: Box<[T]>,
    /// `sum w`, `sum w . b`, `sum w . b^2`, over block ids; then, when `E` has
    /// room, `sum w . exp(-s . b / L)` for each `s` in [`LAPLACE`].
    ///
    /// Linear in the colour, so a convex combination of colours has the convex
    /// combination of their moments -- which is why they can be carried at all.
    /// The effective block count cannot: it is `mass^2 / sum w^2`, quadratic,
    /// and no bounded per-colour state folds it.  See [`crate::main`]'s
    /// `Line::Moments`.
    exact: [f64; E],
}

impl<T: Lane, const E: usize> Tinted<T, E> {
    #[inline]
    fn len(&self) -> usize {
        self.bands.len()
    }
}

/// `E` is how many exact `f64` each colour carries beside its lanes, and is
/// [`CARRY_MOMENTS`] unless a run asked for `--laplace`.
pub struct BandStore<T: Lane, const E: usize = CARRY_MOMENTS> {
    layout: Layout,
    /// Colours alive, in allocations, and the high-water mark of that.  The
    /// bytes are `live * bands * size_of::<T>()`, plus an `Rc` header each.
    live: usize,
    peak: usize,
    /// The largest `|sum of weights - 1|` seen in a finished colour.
    drift: f64,
    _lane: std::marker::PhantomData<T>,
}

impl<T: Lane, const E: usize> BandStore<T, E> {
    #[cfg(test)]
    pub fn layout(&self) -> Layout {
        self.layout
    }

    fn track(&mut self, color: Color<T, E>) -> Color<T, E> {
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
fn combine_into<T: Lane, const E: usize>(
    a: &Tinted<T, E>,
    wa: f64,
    b: &Tinted<T, E>,
    wb: f64,
) -> Color<T, E> {
    debug_assert_eq!(a.len(), b.len());
    let (la, lb) = (T::from_f64(wa), T::from_f64(wb));
    let bands: Box<[T]> = a
        .bands
        .iter()
        .zip(&b.bands)
        .map(|(&x, &y)| x * la + y * lb)
        .collect();
    // The same combination, on the exact lanes.  Done in `f64` and from the
    // `f64` weights rather than the lane-rounded ones, so the moments -- and
    // the `--laplace` values after them, when there are any -- do not inherit
    // the lanes' precision.
    let mut exact = [0.0f64; E];
    for k in 0..E {
        exact[k] = a.exact[k] * wa + b.exact[k] * wb;
    }
    Rc::new(Tinted { bands, exact })
}

#[inline]
fn scale_into<T: Lane, const E: usize>(a: &Tinted<T, E>, w: f64) -> Color<T, E> {
    let lane = T::from_f64(w);
    let bands: Box<[T]> = a.bands.iter().map(|&x| x * lane).collect();
    let mut exact = a.exact;
    for m in &mut exact {
        *m *= w;
    }
    Rc::new(Tinted { bands, exact })
}

impl<T: Lane, const E: usize> ColorStore for BandStore<T, E> {
    type Color = Color<T, E>;

    const WEIGHTED: bool = true;
    const NARROW: bool = T::NARROW;

    fn new() -> Self {
        const {
            assert!(
                E == CARRY_MOMENTS || E == CARRY_LAPLACE,
                "a band colour carries the moments, or the moments and --laplace"
            )
        };
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

    fn singleton(&mut self, block: usize) -> Color<T, E> {
        let mut bands = vec![T::default(); self.layout.bands].into_boxed_slice();
        bands[self.layout.band(block)] = T::from_f64(1.0);
        // A coinbase is a point mass on one block, so its moments are exact and
        // trivial -- and they are the block itself, not the band's centre.  So
        // are its `--laplace` values, which are seven `exp`s of that block: the
        // only place this store calls one, since everything after a coinbase is
        // a combination of what coinbases started with.
        let b = block as f64;
        let mut exact = [0.0f64; E];
        exact[..CARRY_MOMENTS].copy_from_slice(&[1.0, b, b * b]);
        if E == CARRY_LAPLACE {
            exact[CARRY_MOMENTS..].copy_from_slice(&laplace_at(b));
        }
        let color = Rc::new(Tinted { bands, exact });
        self.track(color)
    }

    fn combine(&mut self, a: &Color<T, E>, wa: f64, b: &Color<T, E>, wb: f64) -> Color<T, E> {
        // Two handles on one vector: `wa . C + wb . C` is `(wa + wb) . C`, a
        // scale rather than nothing, as in the weighted store.
        if Rc::ptr_eq(a, b) {
            return self.scale(a, wa + wb);
        }
        let color = combine_into(a, wa, b, wb);
        self.track(color)
    }

    fn scale(&mut self, color: &Color<T, E>, w: f64) -> Color<T, E> {
        // Weight 1 changes nothing, and a single-input transaction is exactly
        // that case; sharing keeps the commonest shape free.
        if w == 1.0 {
            return self.share(color);
        }
        let scaled = scale_into(color, w);
        self.track(scaled)
    }

    fn share(&mut self, color: &Color<T, E>) -> Color<T, E> {
        Rc::clone(color)
    }

    fn release(&mut self, color: Color<T, E>) {
        if Rc::strong_count(&color) == 1 {
            self.live -= 1;
        }
    }

    /// The non-zero bands, highest first, as `(band, weight)`.  The exponent is
    /// the band's index and not a block id; [`ColorStore::placement`] says how to
    /// put it back on the block axis.
    fn for_each_term(&self, color: &Color<T, E>, mut f: impl FnMut(usize, f64)) {
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
    fn exact_moments(&self, color: &Color<T, E>) -> Option<[f64; 3]> {
        let mut moments = [0.0f64; CARRY_MOMENTS];
        moments.copy_from_slice(&color.exact[..CARRY_MOMENTS]);
        Some(moments)
    }

    /// The seven `--laplace` values carried beside the bands, when this store
    /// was made to carry them, which `main` does exactly when `--laplace` is
    /// asked for.  Derived from the bands they would put every block of a band
    /// at its centre, as the moments would.
    fn exact_laplace(&self, color: &Color<T, E>) -> Option<[f64; 7]> {
        if E != CARRY_LAPLACE {
            return None;
        }
        let mut values = [0.0f64; 7];
        values.copy_from_slice(&color.exact[CARRY_MOMENTS..]);
        Some(values)
    }

    fn observe(&mut self, color: &Color<T, E>) {
        let total: f64 = color.bands.iter().map(|w| w.to_f64()).sum();
        self.drift = self.drift.max((total - 1.0).abs());
    }

    fn usage(&self) -> (usize, usize) {
        (self.live * self.layout.bands, self.peak * self.layout.bands)
    }

    fn usage_labels(&self) -> (&'static str, &'static str) {
        ("live lanes", "peak lanes")
    }

    fn audit(&self, live: &mut dyn Iterator<Item = &Color<T, E>>) -> String {
        let mut seen: HashSet<*const Tinted<T, E>> = HashSet::new();
        let mut reachable = 0;
        for color in live {
            // `&**color` rather than `Rc::as_ptr`, which cannot be named here:
            // the lane parameter is also called `T`, so `Rc::as_ptr` resolves
            // against `Rc<T>` and not `Rc<Tinted<T>>`.
            let at: *const Tinted<T, E> = &**color;
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

    fn dump<T: Lane>(store: &BandStore<T>, c: &Color<T, CARRY_MOMENTS>) -> Vec<(usize, f64)> {
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

    /// A store that carries the `--laplace` values, for the tests about them.
    fn laplace_store<T: Lane>(bands: usize, blocks: usize) -> BandStore<T, CARRY_LAPLACE> {
        configure(Layout::new(bands, blocks));
        BandStore::<T, CARRY_LAPLACE>::new()
    }

    /// A coinbase in block `b` is worth exactly `exp(-s . b / L)` at every `s`,
    /// in either lane, at any banding -- the one place the store calls `exp`,
    /// and the value the exact fold sums for the same block.
    #[test]
    fn a_coinbase_starts_at_its_own_laplace_values() {
        for block in [0usize, 1, 37, 381_130, CHAIN_BLOCKS - 1] {
            let mut s = laplace_store::<f32>(4, CHAIN_BLOCKS);
            let c = s.singleton(block);
            let got = s.exact_laplace(&c).expect("carried");
            for (k, sk) in LAPLACE.iter().enumerate() {
                let want = (-sk * block as f64 / CHAIN_BLOCKS as f64).exp();
                assert_eq!(got[k], want, "s = {} at block {}", sk, block);
            }
            // The moments ride in front, unmoved.
            let b = block as f64;
            assert_eq!(s.exact_moments(&c), Some([1.0, b, b * b]));
        }
    }

    /// The narrower carry answers no values, which is what tells `main`'s emit
    /// path it has nothing handed over -- and `main` never asks it to.
    #[test]
    fn only_the_wider_store_carries_them() {
        let mut s = store::<f64>(4, 40);
        let c = s.singleton(5);
        assert_eq!(s.exact_laplace(&c), None);
    }

    /// Scaling and combining carry the seven values through the same convex
    /// combination as the moments and the lanes: half from one coinbase and
    /// half from another is the average of their values, and a deeper mix is
    /// the weighted average written out.
    #[test]
    fn the_laplace_values_follow_the_same_combination() {
        let mut s = laplace_store::<f32>(8, 800_000);
        let (ba, bb, bc) = (100usize, 500_000, 700_000);
        let a = s.singleton(ba);
        let b = s.singleton(bb);
        let c = s.singleton(bc);
        let at = |block: usize| laplace_at(block as f64);

        let ab = s.combine(&a, 0.5, &b, 0.5);
        let got = s.exact_laplace(&ab).unwrap();
        for k in 0..7 {
            let want = (at(ba)[k] + at(bb)[k]) / 2.0;
            assert!((got[k] - want).abs() <= 1e-15 * want, "half and half, s = {}", LAPLACE[k]);
        }

        let ab = s.combine(&a, 0.25, &b, 0.75);
        let abc = s.combine(&ab, 0.5, &c, 0.5);
        let scaled = s.scale(&abc, 0.5);
        let got = s.exact_laplace(&abc).unwrap();
        let halved = s.exact_laplace(&scaled).unwrap();
        for k in 0..7 {
            let want = 0.125 * at(ba)[k] + 0.375 * at(bb)[k] + 0.5 * at(bc)[k];
            assert!((got[k] - want).abs() <= 1e-15 * want, "s = {}: {} against {}", LAPLACE[k], got[k], want);
            assert_eq!(halved[k], got[k] * 0.5, "a scale is a scale, s = {}", LAPLACE[k]);
        }
        // And the moments in front of them are the ones the narrower store
        // would have carried, bit for bit: the wider array is the same loop.
        let mut narrow = store::<f32>(8, 800_000);
        let (na, nb, nc) = (narrow.singleton(ba), narrow.singleton(bb), narrow.singleton(bc));
        let nab = narrow.combine(&na, 0.25, &nb, 0.75);
        let nabc = narrow.combine(&nab, 0.5, &nc, 0.5);
        assert_eq!(s.exact_moments(&abc), narrow.exact_moments(&nabc));
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
