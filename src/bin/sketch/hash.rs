//! # Where every random draw comes from
//!
//! A sketch is a random object, and a run of `sketch` must still be a function
//! of its input: the same seed over the same records has to write the same
//! bytes, or a `check` of a stored file could never reproduce the numbers the
//! `chain` run printed, and two runs could not be told apart from a bug.  So
//! nothing here touches an RNG with state.  Every draw is a hash of *where* it
//! is drawn -- the seed, the variant, the record, the step inside the record,
//! and for the per-block draws the block -- and the same coordinates always
//! give the same number.
//!
//! That also makes the draws exactly as independent as the variants need them
//! to be, and no more:
//!
//! - `varopt` and `prio` want a *fresh* draw at every reduction.  Their
//!   coordinate includes the record index `rec` and the step `j`, so no two
//!   reductions in a run share one.  (Sharing them is what made the
//!   coordinated HT variants of the designs diverge: an estimate that is
//!   re-weighted by inclusion probabilities that are correlated with the
//!   previous selection is no longer unbiased, and the error compounds.)
//! - `coord` wants the *opposite*: one draw per block for the whole run, so
//!   that a block's rank means the same thing in every sketch that holds it.
//!   Its coordinate is the block alone.
//!
//! The step index lives in the low 20 bits of `x = (rec << 20) | j`.  `j` is 0
//! when a record is reduced once (`reduce=record`) and the 1-based fold step
//! under `reduce=step`; [`IDEAL_STEP`], the largest value, is kept back for
//! `eval`'s single-shot IDEAL reduction, so an IDEAL draw is never one the
//! recursion also made.
//!
//! `splitmix64` is Vigna's finaliser: cheap, a bijection on `u64`, and good
//! enough that consecutive integers hash to unrelated outputs, which is all a
//! counter-based generator needs.  Three nested calls fold the seed, the salt
//! and two coordinates in; the salts keep the variants' streams apart even at
//! equal seeds.

/// Vigna's SplitMix64 output function.  `splitmix64(0) == 0xE220A8397B1DCDAF`.
#[inline]
pub fn splitmix64(x: u64) -> u64 {
    let mut z = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A hash as a uniform draw strictly inside `(0, 1)`.
///
/// The top 52 bits plus a half, over `2^52`: the smallest value is `2^-53`
/// and the largest `1 - 2^-53`, both exactly representable, so neither end is
/// ever reached.  The obvious 53-bit form, `((h >> 11) + 0.5) / 2^53`, is not
/// safe: `2^53 - 0.5` is not an `f64` and rounds to `2^53`, which makes the
/// largest hash exactly 1.0.  A 0 would be worse -- `prio` divides by the draw
/// -- and a 1 is harmless to every sampler here, but "never 0 or 1" is what
/// the tests assert and what the samplers' docs lean on, so it should be true.
#[inline]
pub fn unit(h: u64) -> f64 {
    ((h >> 12) as f64 + 0.5) * (1.0 / (1u64 << 52) as f64)
}

/// The hash of the coordinates `(seed, salt, x, y)`.
#[inline]
pub fn h3(seed: u64, salt: u64, x: u64, y: u64) -> u64 {
    splitmix64(splitmix64(splitmix64(seed ^ salt) ^ x) ^ y)
}

pub const SALT_VAROPT: u64 = 0x7661_726f_7074_0001;
pub const SALT_PRIO: u64 = 0x7072_696f_0000_0002;
pub const SALT_COORD: u64 = 0x636f_6f72_6400_0003;

/// The step index `eval`'s IDEAL reduction draws at; the recursion asserts it
/// never reaches it.
pub const IDEAL_STEP: u64 = (1 << 20) - 1;

/// `(rec << 20) | j`, the coordinate of one reduction.
#[inline]
pub fn coordinate(rec: u64, j: u64) -> u64 {
    assert!(j <= IDEAL_STEP, "step {} does not fit in the 20 bits kept for it", j);
    (rec << 20) | j
}

/// `varopt`'s systematic offset for the reduction at `x`.
#[inline]
pub fn varopt_offset(seed: u64, x: u64) -> f64 {
    unit(h3(seed, SALT_VAROPT, x, 0))
}

/// `prio`'s priority draw for block `b` in the reduction at `x`.
#[inline]
pub fn prio_draw(seed: u64, x: u64, b: u32) -> f64 {
    unit(h3(seed, SALT_PRIO, x, b as u64))
}

/// `coord`'s coordinated key for block `b`: the same in every sketch of a run.
#[inline]
pub fn coord_key(seed: u64, b: u32) -> f64 {
    unit(h3(seed, SALT_COORD, 0, b as u64))
}

/// [`coord_key`] for every block of the 2022 chain and a few past it, computed
/// once per seed and shared read-only between threads.  A block beyond the
/// table is hashed on the spot by [`Keys::get`], so a longer chain is slower
/// rather than wrong.
pub struct Keys {
    seed: u64,
    table: Box<[f64]>,
}

impl Keys {
    pub fn new(seed: u64, blocks: usize) -> Keys {
        Keys {
            seed,
            table: (0..blocks as u32).map(|b| coord_key(seed, b)).collect(),
        }
    }

    #[inline]
    pub fn get(&self, b: u32) -> f64 {
        match self.table.get(b as usize) {
            Some(&k) => k,
            None => coord_key(self.seed, b),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// T1: the golden value, so a typo in a constant cannot pass unnoticed.
    #[test]
    fn splitmix_has_its_golden_value() {
        assert_eq!(splitmix64(0), 0xE220_A839_7B1D_CDAF);
    }

    /// T1: strictly inside (0, 1) at both ends and over a million draws.
    #[test]
    fn unit_is_strictly_inside_the_interval() {
        for h in [0u64, 1, u64::MAX, u64::MAX - 1, 1 << 63, (1 << 12) - 1] {
            let u = unit(h);
            assert!(u > 0.0 && u < 1.0, "unit({:#x}) = {}", h, u);
        }
        let mut lo = 1.0f64;
        let mut hi = 0.0f64;
        for i in 0..1_000_000u64 {
            let u = unit(splitmix64(i));
            assert!(u > 0.0 && u < 1.0);
            lo = lo.min(u);
            hi = hi.max(u);
        }
        assert!(lo < 1e-4 && hi > 1.0 - 1e-4, "a million draws should nearly span (0,1)");
    }

    /// T1: deterministic, and different salts or seeds are different streams.
    #[test]
    fn streams_are_deterministic_and_distinct() {
        assert_eq!(h3(1, SALT_VAROPT, 5, 7), h3(1, SALT_VAROPT, 5, 7));
        let n = 100_000u64;
        let a: HashSet<u64> = (0..n).map(|x| h3(1, SALT_VAROPT, x, 0)).collect();
        let by_salt = (0..n).filter(|&x| a.contains(&h3(1, SALT_PRIO, x, 0))).count();
        let by_seed = (0..n).filter(|&x| a.contains(&h3(2, SALT_VAROPT, x, 0))).count();
        assert!(by_salt < (n / 100) as usize, "{} collisions across salts", by_salt);
        assert!(by_seed < (n / 100) as usize, "{} collisions across seeds", by_seed);
        // Position by position, too: the same coordinate under another salt.
        let same = (0..n).filter(|&x| h3(1, SALT_VAROPT, x, 0) == h3(1, SALT_COORD, x, 0)).count();
        assert_eq!(same, 0);
    }

    #[test]
    fn the_key_table_agrees_with_the_hash_past_its_end() {
        let keys = Keys::new(9, 100);
        for b in [0u32, 50, 99, 100, 1_000_000] {
            assert_eq!(keys.get(b), coord_key(9, b));
        }
    }
}
