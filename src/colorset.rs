//! # Colors as sorted sets of block ids
//!
//! The fast backend behind [`crate::store::ColorStore`].  See that module for
//! why there are two and what keeps them honest.
//!
//! A color here is an `Rc<[u32]>`: block ids, ascending, no duplicates.  Three
//! things follow from that shape, and they matter in roughly the reverse of the
//! order one expects.
//!
//! ## The representation is most of the speed
//!
//! Four bytes a block against the ring's 24-byte `Node`, laid out contiguously
//! instead of chased through `next`.  A union is then two sequential reads and
//! one sequential write, which the prefetcher handles and the vector unit can
//! work on; the ring's merge is a dependent load per term and can be neither.
//! Before reaching for intrinsics it is worth being clear that this is where the
//! bulk of the difference comes from.
//!
//! ## Sharing removes most of the unions
//!
//! `Rc` is what [`crate::store::ColorStore::share`] wants: a second handle on a
//! color is a refcount bump rather than a copy.  [`crate::poly`]'s objection to
//! reference counting — that it cannot reclaim a cycle — is about rings, and a
//! set is acyclic, so it does not apply here.
//!
//! ## The merge itself is the smallest of the three
//!
//! And before the merge runs at all, [`SetStore::combine`] tries to avoid it:
//! two handles on one allocation, an empty side, or two ranges that do not
//! overlap are each answered without comparing a single pair of elements.  The
//! last of those is common in this workload — a transaction's ancestry often
//! sits entirely above or below another's — and turns the union into a
//! `memcpy`.
//!
//! ## Limit
//!
//! Block ids must fit in a `u32`, which is the point of the representation.
//! Bitcoin is around 900,000 blocks, so the headroom is four thousandfold, but
//! the backend asserts rather than silently truncating.

use crate::store::ColorStore;
use std::collections::HashSet;
use std::rc::Rc;

/// One color: block ids ascending, no duplicates.
pub type Set = Rc<[u32]>;

pub struct SetStore {
    /// Union builds here and copies into the `Rc` once the length is known, so
    /// the merge itself never reallocates.
    scratch: Vec<u32>,
    /// Elements in allocations that are still referenced, and the high-water
    /// mark of that.  Not node counts — the two backends measure different
    /// things and `--stats` says which.
    live: usize,
    peak: usize,
}

impl SetStore {
    /// Wrap the scratch buffer's contents as a color, and count it.
    fn intern(&mut self, len_hint: usize) -> Set {
        debug_assert_eq!(self.scratch.len(), len_hint);
        let set: Set = Rc::from(&self.scratch[..]);
        self.live += set.len();
        self.peak = self.peak.max(self.live);
        set
    }

    fn track(&mut self, set: Set) -> Set {
        self.live += set.len();
        self.peak = self.peak.max(self.live);
        set
    }
}

/// Merge two ascending duplicate-free slices into `out`, which is cleared first.
///
/// Branchless in the sense that matters: the comparison decides how far each
/// cursor moves rather than which branch runs, so equal elements collapse to one
/// output without a separate dedup pass.  Since neither input repeats, the
/// output cannot either.
///
/// This is the control the vector kernel has to beat.
fn merge_into(out: &mut Vec<u32>, a: &[u32], b: &[u32]) {
    out.clear();
    out.reserve(a.len() + b.len());

    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        let (x, y) = (a[i], b[j]);
        out.push(x.min(y));
        i += (x <= y) as usize;
        j += (y <= x) as usize;
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
}

impl ColorStore for SetStore {
    type Color = Set;

    /// This backend stores no coefficients at all -- a block is in the set or it
    /// is not -- so there is nothing for a weight to multiply.  See
    /// [`crate::weighted`] for the one that does carry them.
    const WEIGHTED: bool = false;

    fn new() -> Self {
        SetStore {
            scratch: Vec::new(),
            live: 0,
            peak: 0,
        }
    }

    fn singleton(&mut self, block: usize) -> Set {
        let block = u32::try_from(block)
            .unwrap_or_else(|_| panic!("block id {} does not fit in a u32 — see colorset", block));
        let set: Set = Rc::from(&[block][..]);
        self.track(set)
    }

    fn combine(&mut self, a: &Set, _wa: f64, b: &Set, _wb: f64) -> Set {
        // Two handles on one allocation.  Common: several inputs of one
        // transaction often trace back to a single ancestor.
        if Rc::ptr_eq(a, b) {
            return self.share(a);
        }
        if a.is_empty() {
            return self.share(b);
        }
        if b.is_empty() {
            return self.share(a);
        }

        // Disjoint ranges: the union is a concatenation, so no element is ever
        // compared and the whole thing is two `memcpy`s.
        let (lo, hi) = if a[a.len() - 1] < b[0] {
            (a, b)
        } else if b[b.len() - 1] < a[0] {
            (b, a)
        } else {
            self.scratch.clear();
            merge_into(&mut self.scratch, a, b);
            let len = self.scratch.len();
            return self.intern(len);
        };

        self.scratch.clear();
        self.scratch.reserve(lo.len() + hi.len());
        self.scratch.extend_from_slice(lo);
        self.scratch.extend_from_slice(hi);
        let len = self.scratch.len();
        self.intern(len)
    }

    fn scale(&mut self, color: &Set, _w: f64) -> Set {
        self.share(color)
    }

    fn share(&mut self, color: &Set) -> Set {
        Rc::clone(color)
    }

    fn release(&mut self, color: Set) {
        // The allocation only goes away when the last handle does.
        if Rc::strong_count(&color) == 1 {
            self.live -= color.len();
        }
    }

    fn for_each_term(&self, color: &Set, mut f: impl FnMut(usize, f64)) {
        // Stored ascending because that is what a merge wants; the output format
        // is decreasing exponents, so it is read back the other way.  Every
        // coefficient is 1 by construction.
        for &block in color.iter().rev() {
            f(block as usize, 1.0);
        }
    }

    fn usage(&self) -> (usize, usize) {
        (self.live, self.peak)
    }

    fn usage_labels(&self) -> (&'static str, &'static str) {
        ("live set elems", "peak set elems")
    }

    fn audit(&self, live: &mut dyn Iterator<Item = &Set>) -> String {
        // Colors share allocations, so summing lengths over the live handles
        // would count a shared set once per holder.  Count each allocation once,
        // by address.
        let mut seen: HashSet<*const u32> = HashSet::new();
        let mut reachable = 0;
        for set in live {
            if seen.insert(set.as_ptr()) {
                reachable += set.len();
            }
        }
        if reachable == self.live {
            format!("audit: {} live set elems, all reachable", self.live)
        } else {
            format!(
                "audit: LEAK -- {} live set elems but only {} reachable ({} lost)",
                self.live,
                reachable,
                self.live as isize - reachable as isize
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// `BTreeSet` is the oracle: it defines what a union is, slowly and
    /// obviously, so [`merge_into`] only has to agree with it.
    fn oracle(a: &[u32], b: &[u32]) -> Vec<u32> {
        a.iter()
            .chain(b.iter())
            .copied()
            .collect::<BTreeSet<u32>>()
            .into_iter()
            .collect()
    }

    fn check(a: &[u32], b: &[u32]) {
        let mut got = Vec::new();
        merge_into(&mut got, a, b);
        assert_eq!(got, oracle(a, b), "a={:?} b={:?}", a, b);
        // Union is commutative, and the two argument orders take different
        // routes through the cursor arithmetic.
        let mut swapped = Vec::new();
        merge_into(&mut swapped, b, a);
        assert_eq!(swapped, got, "not commutative: a={:?} b={:?}", a, b);
    }

    #[test]
    fn merges_the_shapes_the_fast_paths_are_about() {
        let dense: Vec<u32> = (0..40).collect();
        check(&[], &[]);
        check(&[], &dense);
        check(&dense, &[]);
        // identical
        check(&dense, &dense);
        // disjoint, either way round
        check(&(0..20).collect::<Vec<_>>(), &(20..40).collect::<Vec<_>>());
        check(&(20..40).collect::<Vec<_>>(), &(0..20).collect::<Vec<_>>());
        // touching at exactly one element
        check(&(0..21).collect::<Vec<_>>(), &(20..40).collect::<Vec<_>>());
        // one a subset of the other
        check(&dense, &[5, 6, 7]);
        // interleaved, no element shared
        check(
            &(0..40).filter(|v| v % 2 == 0).collect::<Vec<_>>(),
            &(0..40).filter(|v| v % 2 == 1).collect::<Vec<_>>(),
        );
        // wildly lopsided
        check(&dense, &[u32::MAX]);
        check(&[0], &(1..500).collect::<Vec<_>>());
    }

    /// Lengths either side of small strides, at every combination, because a
    /// merge's bugs live in its tails.
    #[test]
    fn merges_every_pair_of_short_lengths() {
        for la in 0..12u32 {
            for lb in 0..12u32 {
                // Overlapping halfway, so both tails are exercised.
                let a: Vec<u32> = (0..la).collect();
                let b: Vec<u32> = (la / 2..la / 2 + lb).collect();
                check(&a, &b);
            }
        }
    }

    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn set(&mut self, n: u64, universe: u64) -> Vec<u32> {
            let mut s: Vec<u32> = (0..n).map(|_| (self.next() % universe) as u32).collect();
            s.sort_unstable();
            s.dedup();
            s
        }
    }

    #[test]
    fn merges_random_sets() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..3000 {
            // A small universe makes collisions -- and so the dedup path --
            // common; a large one makes them rare.  Both matter.
            let universe = if rng.next().is_multiple_of(2) { 32 } else { 100_000 };
            let (na, nb) = (rng.next() % 60, rng.next() % 60);
            let a = rng.set(na, universe);
            let b = rng.set(nb, universe);
            check(&a, &b);
        }
    }

    /// The store's own bookkeeping: `live` must come back to zero once every
    /// color is released, or `--stats` would report a leak that is not there
    /// (or, worse, miss one that is).
    #[test]
    fn releasing_everything_brings_live_back_to_zero() {
        let mut store = SetStore::new();
        let a = store.singleton(3);
        let b = store.singleton(9);
        let c = store.combine(&a, 1.0, &b, 1.0);
        let shared = store.share(&c);

        assert_eq!(store.usage().0, 1 + 1 + 2);
        // A shared handle owns no new storage, so releasing it frees nothing.
        store.release(shared);
        assert_eq!(store.usage().0, 4);

        store.release(a);
        store.release(b);
        store.release(c);
        assert_eq!(store.usage().0, 0);
    }

    #[test]
    fn terms_come_out_in_decreasing_order_with_coefficient_one() {
        let mut store = SetStore::new();
        let a = store.singleton(2);
        let b = store.singleton(7);
        let c = store.combine(&a, 1.0, &b, 1.0);
        let mut seen = Vec::new();
        store.for_each_term(&c, |exp, coeff| seen.push((exp, coeff)));
        assert_eq!(seen, vec![(7, 1.0), (2, 1.0)]);
    }

    #[test]
    #[should_panic(expected = "does not fit in a u32")]
    fn a_block_id_too_wide_for_the_representation_is_refused_loudly() {
        SetStore::new().singleton(u32::MAX as usize + 1);
    }
}
