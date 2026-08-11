//! # What the driver needs a color to be
//!
//! [`crate::poly`] is a port of a Knuth exercise and the circular list is its
//! whole subject, so it stays exactly as it is.  But the *driver* does not
//! actually need polynomials.  It needs sets of block ids: every coefficient it
//! ever handles is 1, and `ior` on 1s is 1, so a color is a set and `ior` is
//! union — `main` says as much in its own docs.
//!
//! That gap is worth something.  A set of small integers can live in a flat
//! sorted array, where a union is a sequential scan of two runs of `u32` rather
//! than a chase down two chains of 24-byte nodes, and where a vector unit has
//! something to work on.  A ring cannot be walked that way: the merge in
//! [`crate::poly::Arena::op`] loads `next` to find out where to load next, so it
//! is latency-bound, and no vector instruction shortens a dependent load.
//!
//! So there are two implementations, and this trait is the line between them:
//!
//! - [`RingStore`] — the exercise, wrapping [`crate::poly::Arena`] untouched.
//! - [`crate::colorset::SetStore`] — sorted arrays, for speed.
//!
//! Keeping both is not sentiment.  The driver is deterministic and its output is
//! byte for byte comparable, so running the same records through both backends
//! and diffing is an equivalence check between two implementations that share no
//! code — a stronger statement about the fast one than any test it could be
//! given on its own.
//!
//! ## Why the trait is shaped this way
//!
//! [`ColorStore::combine`] takes its operands **by reference** and neither is
//! consumed, which is already [`crate::poly::Arena::op`]'s contract.  What the
//! driver must be explicit about instead is wanting a *second handle* on a
//! color, [`ColorStore::share`], because that is a ring copy for one backend and
//! a refcount bump for the other — the whole difference between them, in one
//! method.
//!
//! Nothing here is dyn-safe, on purpose: `run` is generic over this trait and
//! instantiated once per backend, so the calls compile to what they would if the
//! backend were named directly.

use crate::poly::{Arena, Idx};

/// A place colors live, and everything the driver does to them.
///
/// A [`ColorStore::Color`] is a handle whose meaning is private to the store
/// that made it; handing one to a different store is a bug this trait cannot
/// prevent.  Handles are owned — [`ColorStore::release`] each exactly once —
/// which is why `Color` is deliberately not `Copy`.
pub trait ColorStore {
    type Color;

    /// Whether [`ColorStore::combine`] and [`ColorStore::scale`] do anything
    /// with the weights they are handed.
    ///
    /// A constant rather than a method so the driver can skip working the
    /// weights out at all for stores that ignore them: `run` is instantiated per
    /// backend, so `if S::WEIGHTED` folds away at compile time and the
    /// unweighted path costs exactly what it did before weights existed.
    const WEIGHTED: bool;

    fn new() -> Self;

    /// The color of a coinbase transaction: the one block that minted it, with
    /// all of the weight on it.
    fn singleton(&mut self, block: usize) -> Self::Color;

    /// `wa * a + wb * b`: union of the two supports, with the weights of each
    /// side scaled on the way through and matching blocks summed.
    ///
    /// Borrows both operands and leaves them untouched.  Stores with
    /// [`ColorStore::WEIGHTED`] false ignore `wa` and `wb` and answer the plain
    /// union, which is the same thing when every weight is 1 and the driver only
    /// ever asks for `ior`.
    fn combine(&mut self, a: &Self::Color, wa: f64, b: &Self::Color, wb: f64) -> Self::Color;

    /// Every weight multiplied by `w`.  Ignored, and equivalent to
    /// [`ColorStore::share`], when [`ColorStore::WEIGHTED`] is false.
    fn scale(&mut self, color: &Self::Color, w: f64) -> Self::Color;

    /// A second owned handle on the same color.  A copy for [`RingStore`], a
    /// refcount bump for a sharing store; callers should assume it is expensive.
    fn share(&mut self, color: &Self::Color) -> Self::Color;

    /// Give a handle back.  It must not be used again.
    fn release(&mut self, color: Self::Color);

    /// Visit the blocks in decreasing order as `(exponent, coefficient)` pairs.
    ///
    /// The coefficient is an `f64` whichever store this is.  For the unweighted
    /// ones it is always exactly `1.0` and the driver prints it as the integer
    /// the Scheme prints; carrying it as a float costs nothing there and saves
    /// the trait an associated type that only one backend would use.
    fn for_each_term(&self, color: &Self::Color, f: impl FnMut(usize, f64));

    /// Offered each finished color, so a store with an invariant can watch it.
    ///
    /// Only called under `--stats`, because checking one generally means a pass
    /// over the color and that is not worth doing on every record of a real run.
    /// The default does nothing.
    fn observe(&mut self, _color: &Self::Color) {}

    /// `(live, committed)` in whatever unit the store counts, for `--stats`,
    /// with [`ColorStore::usage_labels`] naming them.  The backends count
    /// different things, so these compare across a run, not across backends.
    fn usage(&self) -> (usize, usize);

    fn usage_labels(&self) -> (&'static str, &'static str);

    /// One line saying whether everything the store thinks is live is reachable
    /// from the colors the driver still holds.  A leak is invisible in the
    /// output — a leaked color is still a *correct* color — so it needs its own
    /// check.
    fn audit(&self, live: &mut dyn Iterator<Item = &Self::Color>) -> String;
}

/// The circular-list backend: [`crate::poly::Arena`] plus the one thing the
/// arena has no room for, a shared empty ring to copy against.
///
/// The empty ring is what the driver used to call `zero` and use as its fold
/// seed.  The fold no longer needs a seed, but [`ColorStore::share`] does need
/// something to merge against in order to produce a copy, and allocating a fresh
/// empty ring per call would be silly.  It is one node, which is exactly what
/// `zero` cost before, so `--stats` reads the same as it always did.
pub struct RingStore {
    arena: Arena,
    empty: Idx,
}

impl ColorStore for RingStore {
    type Color = Idx;

    /// The exercise's `ior` has no room for a weight: `poly::op` copies a
    /// coefficient straight across whenever only one side carries the term, so a
    /// weight would reach the shared blocks and miss all the others.  Saying so
    /// here is what keeps the driver from computing weights this backend would
    /// quietly drop.
    const WEIGHTED: bool = false;

    fn new() -> Self {
        let mut arena = Arena::new();
        let empty = arena.make(&[]);
        RingStore { arena, empty }
    }

    fn singleton(&mut self, block: usize) -> Idx {
        self.arena.make(&[(block, 1)])
    }

    fn combine(&mut self, a: &Idx, _wa: f64, b: &Idx, _wb: f64) -> Idx {
        self.arena.ior(*a, *b)
    }

    fn scale(&mut self, color: &Idx, _w: f64) -> Idx {
        self.share(color)
    }

    fn share(&mut self, color: &Idx) -> Idx {
        // No refcount to bump, so a second handle is a second ring.  Union with
        // the empty ring is the identity, so it is also a term-for-term copy.
        self.arena.ior(*color, self.empty)
    }

    fn release(&mut self, color: Idx) {
        self.arena.free_ring(color);
    }

    fn for_each_term(&self, color: &Idx, mut f: impl FnMut(usize, f64)) {
        // `poly` counts in integers and always hands back 1 here, since the
        // driver only ever asks it for `ior`.  The widening is exact.
        self.arena
            .for_each_term(*color, |exponent, coefficient| {
                f(exponent, coefficient as f64)
            });
    }

    fn usage(&self) -> (usize, usize) {
        (self.arena.live(), self.arena.capacity())
    }

    fn usage_labels(&self) -> (&'static str, &'static str) {
        ("live nodes", "arena nodes")
    }

    fn audit(&self, live: &mut dyn Iterator<Item = &Idx>) -> String {
        let reachable: usize = self.arena.ring_len(self.empty)
            + live.map(|&root| self.arena.ring_len(root)).sum::<usize>();
        let held = self.arena.live();
        if reachable == held {
            format!("audit: {} live nodes, all reachable", held)
        } else {
            format!(
                "audit: LEAK -- {} live nodes but only {} reachable ({} lost)",
                held,
                reachable,
                held - reachable
            )
        }
    }
}
