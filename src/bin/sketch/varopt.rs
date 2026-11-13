//! # `varopt`: VarOpt_k by systematic sampling in block order (the default)
//!
//! Given the `n > k` candidates of a merge, keep `k` of them and re-weight so
//! the result is an unbiased estimate of the whole, with the smallest variance
//! any `k`-point scheme can have.  This is Chao's and Cohen-Duffield-Kaplan-
//! Lund-Thorup's VarOpt, with the light items chosen by *systematic* sampling
//! along the block axis rather than by a sequence of random evictions:
//!
//! 1. Find the threshold `tau` with `sum_b min(1, v_b / tau) = k`.  Items with
//!    `v > tau` are *heavy*: kept with their exact weight, probability 1.
//! 2. Walk the rest in ascending block order, adding `v / tau` to an
//!    accumulator that starts at a uniform offset `U` in `(0,1)`, and keep an
//!    item -- at weight `tau` -- whenever the accumulator crosses an integer.
//!    Each light item is kept with probability `v / tau` and then estimates
//!    `v` exactly in expectation; the lights' total `k - h` crossings are
//!    spread evenly through their mass.
//!
//! Why this rather than a priority sample: the estimate has *exactly* the
//! input's mass (no ratio estimator at emission), and because the points are
//! one per `tau` of mass taken in block order, the estimate's CDF never strays
//! from the input's by more than `tau` at any cut.  A colour is read through
//! its CDF -- the mean block, the Laplace values, "how much of this is older
//! than block x" -- so that bound is the property that matters.  The designs
//! measured it 4.7x better than a fresh priority sample at k=64 and ~8x at
//! k=256, with the error falling as 1/k.
//!
//! A fresh `U` per reduction (see [`crate::hash`]) is what keeps the recursion
//! unbiased: every node is an unbiased estimate of its exact colour
//! *conditional on its parents' sketches*, and the tower property carries that
//! up the DAG.  A global offset was measured 3x worse and systematically so.
//!
//! ## Computing `tau` without cancellation
//!
//! The textbook loop starts from `rest = W` and subtracts each heavy.  On this
//! chain that is wrong in a way that matters: a merge can have a few items
//! holding nearly all the mass and a tail of thousands of tiny ones, and
//! `W - heavies` is then the cancellation residue of two nearly equal numbers
//! -- it came out *negative* in half of a critique's simulated trials, which
//! made every candidate "heavy" and the output longer than `k + 1`.  So the
//! light mass is always summed directly, over the lights themselves, and
//! never obtained by a difference: [`threshold`] does that in a few linear
//! passes (and is most of why a reduction costs what a merge does), and its
//! fallback [`threshold_sorted`] builds the tail sums of the sorted top `k`
//! from the small end.
//!
//! `tau` is then clamped to at least the largest light weight (only a rounding
//! can make it smaller), which keeps every `v / tau <= 1` -- at most one
//! crossing per item -- and any `tau` at or above the largest light keeps the
//! scheme unbiased.  A `tau <= 0` would mean the heavies hold everything; the
//! heavies are then the answer.

/// Reused buffers, so a reduction allocates nothing once warm.
#[derive(Default)]
pub struct Scratch {
    vals: Vec<f64>,
    suffix: Vec<f64>,
}

/// What one reduction did: its threshold and how the output splits.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Outcome {
    pub tau: f64,
    pub heavies: usize,
    pub lights: usize,
}

impl Outcome {
    /// Whether rounding gave `k - 1` or `k + 1` points rather than `k`.  The
    /// output is never trimmed to `k` -- trimming would bias it -- so this is
    /// only counted.
    pub fn off_by_one(&self, k: usize) -> bool {
        self.heavies + self.lights != k
    }
}

/// The VarOpt threshold of `cand` for `k` (`len > k`): the `tau` with
/// `sum min(1, v / tau) = k`, clamped to at least the largest light weight.
///
/// Found by fixed-point iteration from above rather than by sorting, because
/// this is the inner loop of the whole run and the sort is most of it.  Start
/// at `W / k`, which is never below `tau` (if it were, `sum min(1, v/tau)`
/// would be at most `W / tau < k`), so everything above it is certainly heavy.
/// Each pass then counts the items above the current threshold and sums the
/// rest *directly* -- never `W` minus the heavies -- and moves the threshold to
/// `light mass / (k - heavies)`.  It falls monotonically to `tau`, and stops
/// when no light lies above the new value.  One to three passes is the rule;
/// a colour of geometrically decaying weights, the shape near-copy chains
/// make, drops the threshold by a factor of about `k (1 - r)` a pass.  After
/// [`MAX_PASSES`] it gives up and sorts ([`threshold_sorted`]), so the worst
/// case is bounded by the old cost.
pub fn threshold(cand: &[(u32, f64)], k: usize, scratch: &mut Scratch) -> f64 {
    let w: f64 = cand.iter().map(|c| c.1).sum();
    let mut thr = w / k as f64;
    for _ in 0..MAX_PASSES {
        let (mut heavy, mut light, mut max_light) = (0usize, 0.0f64, 0.0f64);
        for &(_, v) in cand {
            if v > thr {
                heavy += 1;
            } else {
                light += v;
                max_light = max_light.max(v);
            }
        }
        if heavy >= k {
            break; // only rounding can get here; the sort decides
        }
        let next = light / (k - heavy) as f64;
        if max_light <= next {
            return next.max(max_light);
        }
        thr = next;
    }
    threshold_sorted(cand, k, scratch)
}

/// Passes [`threshold`] makes before it sorts instead.
pub const MAX_PASSES: usize = 24;

/// [`threshold`] by sorting the top `k`: the textbook loop over the heavies in
/// descending order, with the tail sums built from the small end so that no
/// partial sum is a difference.
pub fn threshold_sorted(cand: &[(u32, f64)], k: usize, scratch: &mut Scratch) -> f64 {
    // The k largest, descending.  Only the top k can be heavy -- there are at
    // most k - 1 heavies -- so nothing below them needs ordering.
    let vals = &mut scratch.vals;
    vals.clear();
    vals.extend(cand.iter().map(|&(_, v)| v));
    vals.select_nth_unstable_by(k - 1, |a, b| b.total_cmp(a));
    let tail: f64 = vals[k..].iter().sum();
    let top = &mut vals[..k];
    top.sort_unstable_by(|a, b| b.total_cmp(a));

    let suffix = &mut scratch.suffix;
    suffix.clear();
    suffix.resize(k + 1, 0.0);
    suffix[k] = tail;
    for i in (0..k).rev() {
        suffix[i] = suffix[i + 1] + top[i];
    }

    let mut h = 0;
    while h < k - 1 && top[h] > suffix[h] / (k - h) as f64 {
        h += 1;
    }
    // The largest light is top[h]; only rounding can leave tau below it.
    (suffix[h] / (k - h) as f64).max(top[h])
}

/// Reduce the candidates `cand` (ascending block, every weight positive and
/// finite, `cand.len() > k >= 1`) to about `k` points, written into `out`
/// in ascending block order with `f64` weights.  `u` is the systematic offset,
/// strictly inside `(0, 1)`.
///
/// Cost: a few linear passes for the threshold, one for the sample.
pub fn reduce(
    cand: &[(u32, f64)],
    k: usize,
    u: f64,
    scratch: &mut Scratch,
    out: &mut Vec<(u32, f64)>,
) -> Outcome {
    let n = cand.len();
    assert!(k >= 1 && n > k, "reduce wants more than k = {} candidates, got {}", k, n);
    out.clear();
    let tau = threshold(cand, k, scratch);

    if !(tau > 0.0) {
        // Everything is in the heavies: there is no light mass to sample.
        for &(b, v) in cand {
            if v > 0.0 {
                out.push((b, v));
            }
        }
        let heavies = out.len();
        return Outcome { tau: 0.0, heavies, lights: 0 };
    }

    let (mut heavies, mut lights) = (0, 0);
    // The accumulator is kept in [0, 1): subtracting the crossing rather than
    // comparing floors keeps its absolute precision at the scale of one item,
    // however many items the pass walks.
    let mut acc = u;
    // A multiply rather than a divide per item; v * (1/tau) and v / tau differ
    // by at most an ulp, which moves an inclusion probability by 1e-16.
    let inv = 1.0 / tau;
    for &(b, v) in cand {
        if v > tau {
            out.push((b, v));
            heavies += 1;
            continue;
        }
        acc += v * inv;
        if acc >= 1.0 {
            out.push((b, tau));
            lights += 1;
            acc -= 1.0;
        }
    }
    let outcome = Outcome { tau, heavies, lights };
    assert!(
        out.len() <= k + 1,
        "varopt kept {} points for k = {} ({:?})",
        out.len(),
        k,
        outcome
    );
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{splitmix64, unit};

    /// A weight vector with a heavy tail and a few huge entries, from a hash.
    fn weights(seed: u64, n: usize) -> Vec<(u32, f64)> {
        let mut out = Vec::with_capacity(n);
        let mut b = 0u32;
        for i in 0..n as u64 {
            let h = splitmix64(seed.wrapping_mul(1_000_003) ^ i);
            b += 1 + (h % 7) as u32;
            let u = unit(splitmix64(h));
            // Pareto(1.1) body, a few heavy spikes, and tiny weights.
            let mut v = u.powf(-1.0 / 1.1);
            if h % 53 == 0 {
                v *= 1e6;
            }
            if h % 31 == 0 {
                v *= 1e-9;
            }
            out.push((b, v));
        }
        out
    }

    fn cdf_gap(a: &[(u32, f64)], b: &[(u32, f64)]) -> f64 {
        let (mut i, mut j) = (0, 0);
        let (mut fa, mut fb, mut gap) = (0.0f64, 0.0f64, 0.0f64);
        while i < a.len() || j < b.len() {
            let ba = a.get(i).map_or(u32::MAX, |x| x.0);
            let bb = b.get(j).map_or(u32::MAX, |x| x.0);
            let x = ba.min(bb);
            if ba == x {
                fa += a[i].1;
                i += 1;
            }
            if bb == x {
                fb += b[j].1;
                j += 1;
            }
            gap = gap.max((fa - fb).abs());
        }
        gap
    }

    /// T2: the deterministic properties, on the f64 output before storage,
    /// with rounding's k +- 1 events exempted from the mass and CDF bounds.
    #[test]
    fn a_reduction_keeps_k_points_the_mass_and_the_cdf_within_tau() {
        let mut scratch = Scratch::default();
        let mut out = Vec::new();
        let mut off = 0;
        for case in 0..1000u64 {
            let h = splitmix64(case ^ 0xABCD);
            let n = 2 + (h % 499) as usize;
            let k = 1 + (splitmix64(h) % 64) as usize;
            let cand = weights(case, n);
            if n <= k {
                continue; // the caller stores these untouched; see algebra's tests
            }
            let w: f64 = cand.iter().map(|x| x.1).sum();
            for rep in 0..64u64 {
                let u = unit(splitmix64(case * 64 + rep));
                let o = reduce(&cand, k, u, &mut scratch, &mut out);
                assert!(out.windows(2).all(|p| p[0].0 < p[1].0), "ascending");
                // A subset of the input, heavies exact and lights at tau.
                let mut it = cand.iter();
                for &(b, v) in &out {
                    let src = it.find(|c| c.0 == b).expect("a subset of the input");
                    if src.1 > o.tau {
                        assert_eq!(v, src.1, "a heavy keeps its weight");
                    } else {
                        assert_eq!(v, o.tau, "a light carries tau");
                    }
                }
                for c in &cand {
                    if c.1 > o.tau {
                        assert!(out.iter().any(|x| x.0 == c.0), "every heavy is kept");
                    }
                }
                let incl: f64 = cand.iter().map(|c| (c.1 / o.tau).min(1.0)).sum();
                assert!((incl - k as f64).abs() <= 1e-9 * k as f64, "sum of inclusions {} vs k {}", incl, k);
                if o.off_by_one(k) {
                    off += 1;
                    assert!(out.len() + 1 >= k && out.len() <= k + 1);
                    continue;
                }
                assert_eq!(out.len(), k);
                let s: f64 = out.iter().map(|x| x.1).sum();
                assert!((s - w).abs() <= 1e-12 * w, "mass {} vs {}", s, w);
                assert!(cdf_gap(&cand, &out) < o.tau * (1.0 + 1e-9), "cdf gap above tau");
            }
        }
        assert!(off < 64, "{} k+-1 events in 64,000 reductions", off);
    }

    /// The fixed-point threshold and the sorted one agree, including on the
    /// geometric weights near-copy chains make and on many-way ties.
    #[test]
    fn the_iterated_threshold_is_the_sorted_one() {
        let mut scratch = Scratch::default();
        for case in 0..3000u64 {
            let h = splitmix64(case ^ 0x77);
            let n = 2 + (h % 2000) as usize;
            let k = 1 + (splitmix64(h) % 300) as usize;
            if n <= k {
                continue;
            }
            let cand: Vec<(u32, f64)> = match case % 3 {
                0 => weights(case, n),
                1 => (0..n as u32).map(|i| (i, 0.97f64.powi(i as i32))).collect(),
                _ => (0..n as u32).map(|i| (i, if i % 5 == 0 { 0.25 } else { 1e-3 })).collect(),
            };
            let a = threshold(&cand, k, &mut scratch);
            let b = threshold_sorted(&cand, k, &mut scratch);
            assert!((a - b).abs() <= 1e-12 * b, "case {} n {} k {}: {} vs {}", case, n, k, a, b);
        }
    }

    /// The case that broke the textbook loop: a few items with all but 1e-17
    /// of the mass, and the rest -- including exact zeros the caller filters
    /// before here -- rounding residue.  The output must stay within k + 1.
    #[test]
    fn heavies_holding_nearly_everything_do_not_overflow_the_output() {
        let mut scratch = Scratch::default();
        let mut out = Vec::new();
        for k in 2..40usize {
            let mut cand: Vec<(u32, f64)> = (0..k as u32 / 2).map(|b| (b, 0.3 / (k / 2) as f64 + b as f64 * 1e-3)).collect();
            for b in (k as u32 / 2)..(3 * k as u32) {
                cand.push((b, 1e-300 * (1 + b) as f64));
            }
            for rep in 0..50 {
                let o = reduce(&cand, k, unit(splitmix64(rep)), &mut scratch, &mut out);
                assert!(out.len() <= k + 1, "{} points for k {}", out.len(), k);
                assert!(o.tau >= 0.0 && o.tau.is_finite());
            }
        }
    }

    /// T3: over many independent offsets, each coordinate's mean estimate is
    /// its weight, each light's inclusion frequency is v / tau, and the mean
    /// Laplace value at s = 4 is the exact one -- all within 5 SE.
    #[test]
    fn the_estimate_is_unbiased_coordinate_by_coordinate() {
        let n = 60;
        let k = 12;
        let mut cand: Vec<(u32, f64)> = (0..n as u32)
            .map(|i| (i * 1000 + 7, 0.2 + unit(splitmix64(i as u64 + 77))))
            .collect();
        for &i in &[5usize, 23, 41] {
            cand[i].1 = 30.0; // three heavies
        }
        let mut scratch = Scratch::default();
        let mut out = Vec::new();
        let r = 200_000u64;
        let mut sum = vec![0.0f64; n];
        let mut sq = vec![0.0f64; n];
        let mut hits = vec![0u64; n];
        let lap = |b: u32| (-4.0 * b as f64 / 762_261.0).exp();
        let exact_lap: f64 = cand.iter().map(|&(b, v)| v * lap(b)).sum();
        let (mut ls, mut lq) = (0.0, 0.0);
        let mut tau = 0.0;
        for rep in 0..r {
            let o = reduce(&cand, k, unit(splitmix64(rep ^ 0x5151)), &mut scratch, &mut out);
            tau = o.tau;
            let mut j = 0;
            let mut l = 0.0;
            for (i, &(b, _)) in cand.iter().enumerate() {
                let v = if j < out.len() && out[j].0 == b {
                    j += 1;
                    hits[i] += 1;
                    out[j - 1].1
                } else {
                    0.0
                };
                sum[i] += v;
                sq[i] += v * v;
                l += v * lap(b);
            }
            ls += l;
            lq += l * l;
        }
        let rf = r as f64;
        for i in 0..n {
            let mean = sum[i] / rf;
            let se = ((sq[i] / rf - mean * mean).max(0.0) / rf).sqrt();
            assert!((mean - cand[i].1).abs() <= 5.0 * se + 1e-12, "coordinate {}: {} vs {} (se {})", i, mean, cand[i].1, se);
            if cand[i].1 <= tau {
                let p = cand[i].1 / tau;
                let f = hits[i] as f64 / rf;
                let se = (p * (1.0 - p) / rf).sqrt();
                assert!((f - p).abs() <= 5.0 * se, "inclusion {} vs {}", f, p);
            }
        }
        let lm = ls / rf;
        let lse = ((lq / rf - lm * lm).max(0.0) / rf).sqrt();
        assert!((lm - exact_lap).abs() <= 5.0 * lse + 1e-12, "laplace {} vs {}", lm, exact_lap);
    }
}
