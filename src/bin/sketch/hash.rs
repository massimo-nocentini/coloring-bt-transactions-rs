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
//! - The side stores want what `coord` wants, one draw per block for the
//!   whole run, and nothing per record: `kmv` a uniformly random permutation
//!   of the blocks ([`Perm`], Fisher-Yates driven by the hash), `l1` a Cauchy
//!   draw per block and coordinate ([`cauchy`]).  Neither is ever redrawn, so
//!   a sketch of a colour is the same whichever path through the DAG made it.
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

/// The KMV store's permutation of the blocks ([`Perm`]).
pub const SALT_KMV: u64 = 0x6b6d_7600_0000_0004;
/// The L1 store's Cauchy projection ([`cauchy`]).
pub const SALT_L1: u64 = 0x6c31_0000_0000_0005;

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

/// A uniformly random permutation of the block ids `[0, 2^bits)`: the rank
/// function of the KMV store ([`crate::kmv`]).
///
/// Why a table and not a hash.  The count estimator `(k-1) N / R` is
/// *exactly* unbiased when the ranks of a set are a uniformly random subset
/// of a finite universe -- a theorem about a permutation, not about a hash
/// that is merely "random enough".  A table drawn by Fisher-Yates is that
/// model up to the quality of the draws, and it is what lets the estimator
/// restrict itself to the blocks a support can lie in (`[0, beta]`, see
/// [`crate::kmv::Universe`]): the ranks of any fixed subset of the universe
/// are then a uniformly random subset of the ranks, which is what the
/// restricted estimator's proof needs.  A 32-bit Feistel (the prototype's)
/// is a bijection too, but nothing makes its restriction to `[0, beta]`
/// uniform.
///
/// The draw is `j_i = h3(seed, SALT_KMV, i, 0) % (i + 1)` for `i = M-1` down
/// to 1; the modulo bias is below `M / 2^64`.  Both directions are stored
/// (8 MB at the default 20 bits), since the store works in ranks and every
/// output names blocks.  One table per `(seed, bits)` is built in `main` and
/// shared through an `Arc`, like the coord [`Keys`].
pub struct Perm {
    bits: u32,
    seed: u64,
    fwd: Box<[u32]>,
    inv: Box<[u32]>,
}

impl Perm {
    pub const MIN_BITS: u32 = 12;
    pub const MAX_BITS: u32 = 26;

    pub fn new(seed: u64, bits: u32) -> Perm {
        assert!(
            (Self::MIN_BITS..=Self::MAX_BITS).contains(&bits),
            "bits {} is outside {}..={}",
            bits,
            Self::MIN_BITS,
            Self::MAX_BITS
        );
        let m = 1usize << bits;
        let mut fwd: Box<[u32]> = (0..m as u32).collect();
        for i in (1..m).rev() {
            let j = (h3(seed, SALT_KMV, i as u64, 0) % (i as u64 + 1)) as usize;
            fwd.swap(i, j);
        }
        let mut inv = vec![0u32; m].into_boxed_slice();
        for (b, &r) in fwd.iter().enumerate() {
            inv[r as usize] = b as u32;
        }
        Perm { bits, seed, fwd, inv }
    }

    /// The rank of block `b`.  A block outside the universe is a hard error
    /// that names the option to raise: a silent wrap would merge two blocks.
    #[inline]
    pub fn rank(&self, b: u32) -> u32 {
        match self.fwd.get(b as usize) {
            Some(&r) => r,
            None => panic!("block {} >= 2^{}; raise :bits=", b, self.bits),
        }
    }

    /// The block of rank `r`.
    #[inline]
    pub fn block(&self, r: u32) -> u32 {
        self.inv[r as usize]
    }

    /// `M = 2^bits`, the size of the universe.
    pub fn universe(&self) -> u64 {
        1u64 << self.bits
    }

    pub fn bits(&self) -> u32 {
        self.bits
    }

    pub fn seed(&self) -> u64 {
        self.seed
    }
}

/// A standard Cauchy draw for coordinate `j` of block `b`: `tan(pi (u -
/// 1/2))` with `u = unit(h3(seed, SALT_L1, b, j))`.  [`unit()`] is strictly
/// inside `(0, 1)`, so the draw is always finite (`|x| <= 2.9e15`), and an
/// L1 sketch never holds an infinity it would then have to sum.
#[inline]
pub fn cauchy(seed: u64, b: u32, j: u32) -> f64 {
    let u = unit(h3(seed, SALT_L1, b as u64, j as u64));
    (std::f64::consts::PI * (u - 0.5)).tan()
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

    /// T-K1: a bijection both ways on the whole universe, at three sizes and
    /// three seeds; seeds give different permutations; the golden first ranks
    /// of seed 1 at 20 bits are pinned, so a change to the draw is noticed
    /// (and must rerun T-K1b, T-K6 and the count runs).
    #[test]
    fn the_permutation_is_a_bijection() {
        for bits in [12u32, 16, 20] {
            let m = 1u32 << bits;
            let ps: Vec<Perm> = [1u64, 2, 3].iter().map(|&s| Perm::new(s, bits)).collect();
            for p in &ps {
                let mut seen = vec![false; m as usize];
                for b in 0..m {
                    let r = p.rank(b);
                    assert!(!seen[r as usize]);
                    seen[r as usize] = true;
                    assert_eq!(p.block(r), b);
                }
                assert_eq!(p.universe(), m as u64);
            }
            let same = (0..m).filter(|&b| ps[0].rank(b) == ps[1].rank(b)).count();
            assert!(same < (m / 100) as usize, "{} shared fixed points at {} bits", same, bits);
        }
        let p = Perm::new(1, 20);
        let first: Vec<u32> = (0..4).map(|b| p.rank(b)).collect();
        assert_eq!(first, GOLDEN_RANKS, "the permutation of seed 1 changed");
    }

    const GOLDEN_RANKS: [u32; 4] = [127_629, 579_107, 395_603, 546_508];

    #[test]
    #[should_panic(expected = "block 4096 >= 2^12; raise :bits=")]
    fn a_block_outside_the_universe_is_an_error() {
        Perm::new(1, 12).rank(4096);
    }

    /// T-K1b: over 2^16 seeds at 12 bits, the rank of a fixed block is
    /// uniform (chi-square on 64 cells), and the ranks of two blocks are not
    /// correlated beyond 4 SE.  Seeds are the unit here: one draw per seed.
    #[test]
    fn ranks_are_uniform_across_seeds() {
        let n = 1u64 << 16;
        let mut cells = [0u64; 64];
        let (mut sx, mut sy, mut sxy, mut sxx, mut syy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for seed in 0..n {
            let p = Perm::new(seed, 12);
            let (x, y) = (p.rank(0) as f64 / 4096.0, p.rank(1) as f64 / 4096.0);
            cells[(p.rank(0) >> 6) as usize] += 1;
            sx += x;
            sy += y;
            sxy += x * y;
            sxx += x * x;
            syy += y * y;
        }
        let e = n as f64 / 64.0;
        let chi2: f64 = cells.iter().map(|&c| (c as f64 - e).powi(2) / e).sum();
        // 63 degrees of freedom: the spec's p > 1e-4 is chi2 < 113.5.
        let crit = crate::metrics::chi2_quantile(1.0 - 1e-4, 63.0);
        assert!((crit - 113.5).abs() < 0.1, "chi2 quantile {}", crit);
        assert!(chi2 < crit, "chi2 {} on 63 dof (critical {})", chi2, crit);
        let nf = n as f64;
        let cov = sxy / nf - sx / nf * sy / nf;
        let r = cov / ((sxx / nf - (sx / nf).powi(2)) * (syy / nf - (sy / nf).powi(2))).sqrt();
        assert!(r.abs() < 4.0 / nf.sqrt(), "correlation {} of rank(0) and rank(1)", r);
    }

    #[test]
    fn cauchy_draws_are_finite_and_seeded() {
        let mut worst = 0.0f64;
        for b in 0..1000u32 {
            for j in 0..64 {
                let x = cauchy(1, b, j);
                assert!(x.is_finite());
                worst = worst.max(x.abs());
            }
        }
        assert!(worst > 100.0, "a Cauchy sample of 64000 has a large maximum");
        assert_ne!(cauchy(1, 5, 7), cauchy(2, 5, 7));
        assert!((std::f64::consts::PI * (unit(u64::MAX) - 0.5)).tan().abs() < 3e15);
    }
}
