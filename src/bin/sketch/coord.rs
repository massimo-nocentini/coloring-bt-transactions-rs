//! # `coord`: coordinated bottom-k with exact lower-bound weights (CPS-LB)
//!
//! The one design that does not re-randomise.  Every block has one key `u_b`
//! for the whole run ([`crate::hash::Keys`]), a weight `v` ranks it at
//! `r_b = u_b / v`, and a sketch holds the blocks of smallest rank together
//! with the threshold `tau` below which it holds *every* block of its colour.
//! Because the keys are shared, two sketches that both rank a block low agree
//! about it, and a merge of threshold samples is again a threshold sample --
//! on disjoint supports exactly so, which is test T5.
//!
//! The weights it stores are not estimates: `v_b` is the part of the true
//! weight the merge could see, `0 < v_b <= C_b`.  A block missing from one
//! operand's sketch contributes 0 from that side (the "zero rule"), which is
//! a lower bound.  The rules that tried to add an estimate of the missing part
//! back (half, bound, copy, frac, cond) were all measured to compound upward
//! through the DAG and are gone; the zero rule's error compounds *downward*,
//! and that is the bias floor the designs found: coord's error stops falling
//! with `k` -- k = 256 was worse than k = 64 at 60M records.  It stays here as
//! the measured alternative, not as the answer.
//!
//! ## The threshold after a merge
//!
//! A merged sketch can only vouch for blocks below what every operand vouched
//! for.  Operand `g` enters at share `S_g`, so its threshold in the merged
//! scale is `tau_g / S_g`, and the default cap is the smallest of those
//! (`cap=min`).  Under overlap that is not a complete threshold sample: a block
//! absent from every operand only satisfies `D_b <= u_b . sum_g S_g / tau_g`,
//! so its true rank can lie between the harmonic cap and the min cap.
//! `cap=harm`, `1 / sum_g (S_g / tau_g)`, is the cap that guarantees blocks
//! missing everywhere rank at or above it; it is a knob to measure.
//!
//! With `rk1` the `(k+1)`-th smallest candidate rank, `tau = min(cap, rk1)`
//! and the sketch keeps the candidates ranked strictly below.
//!
//! ## What is emitted
//!
//! An exact sketch (`tau = +INF`) is its colour.  Otherwise each kept block is
//! estimated by `max(v_b, 1/tau)` -- its HT weight under priority sampling --
//! and the estimate is normalised, which is mandatory here: the lower bounds
//! leave the raw total below one.

use crate::algebra::Cap;
use crate::hash::Keys;

#[derive(Default)]
pub struct Scratch {
    ranks: Vec<(f64, u32)>,
}

/// The merged threshold from each operand's `(share, tau)`, ignoring exact
/// operands (`tau = +INF`).
pub fn cap_of(groups: impl Iterator<Item = (f64, f64)>, mode: Cap) -> f64 {
    match mode {
        Cap::Min => groups
            .filter(|g| g.1.is_finite())
            .map(|(s, tau)| tau / s)
            .fold(f64::INFINITY, f64::min),
        Cap::Harm => {
            let inv: f64 = groups.filter(|g| g.1.is_finite()).map(|(s, tau)| s / tau).sum();
            if inv > 0.0 {
                1.0 / inv
            } else {
                f64::INFINITY
            }
        }
    }
}

#[inline]
pub fn rank(keys: &Keys, b: u32, v: f64) -> f64 {
    keys.get(b) / v
}

/// Keep the candidates ranked below `min(cap, rk1)`, ascending, into `out`.
/// `floor > 0` is the optional guard: if fewer than `min(floor, n)` survive,
/// keep that many of the smallest rank instead.  Answers the new `tau`.
pub fn reduce(
    cand: &[(u32, f64)],
    k: usize,
    cap: f64,
    floor: usize,
    keys: &Keys,
    scratch: &mut Scratch,
    out: &mut Vec<(u32, f64)>,
) -> f64 {
    let n = cand.len();
    out.clear();
    let ranks = &mut scratch.ranks;
    ranks.clear();
    let rk1 = if n > k {
        ranks.extend(cand.iter().enumerate().map(|(i, &(b, v))| (rank(keys, b, v), i as u32)));
        ranks.select_nth_unstable_by(k, |a, b| a.0.total_cmp(&b.0));
        ranks[k].0
    } else {
        f64::INFINITY
    };
    let mut tau = cap.min(rk1);
    for &(b, v) in cand {
        if rank(keys, b, v) < tau {
            out.push((b, v));
        }
    }
    let want = floor.min(n);
    if floor > 0 && out.len() < want {
        ranks.clear();
        ranks.extend(cand.iter().enumerate().map(|(i, &(b, v))| (rank(keys, b, v), i as u32)));
        ranks.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        tau = if want < n { ranks[want].0 } else { f64::INFINITY };
        let mut idx: Vec<u32> = ranks[..want].iter().map(|r| r.1).collect();
        idx.sort_unstable();
        out.clear();
        out.extend(idx.iter().map(|&i| cand[i as usize]));
    }
    tau
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::algebra::{Algebra, Config, Handle, Reduce, Sketch, Sketcher, Variant};
    use crate::hash::{splitmix64, unit};
    use std::collections::BTreeMap;
    use std::sync::Arc;

    const K: usize = 32;

    fn keys() -> Arc<Keys> {
        Arc::new(Keys::new(1, 1 << 20))
    }

    fn config(reduce: Reduce, cap: Cap) -> Config {
        Config { variant: Variant::Coord, k: K, seed: 1, reduce, cap, floor: 0 }
    }

    /// An exact colour on `len` blocks starting at `base`, with weights that
    /// are f32-representable so the f32 world and the f64 one agree exactly.
    fn colour(seed: u64, base: u32, len: usize) -> BTreeMap<u32, f64> {
        let mut c = BTreeMap::new();
        let mut b = base;
        for i in 0..len as u64 {
            let h = splitmix64(seed * 7919 + i);
            b += 1 + (h % 3) as u32;
            // Dyadic weights: exactly representable in f32, and products with
            // dyadic shares are exact in f64 and round once to f32.
            c.insert(b, (1 + h % 1000) as f64 / 1024.0);
        }
        c
    }

    /// IDEAL coordinated bottom-k of an exact colour, stored as a sketch.
    fn ideal(keys: &Keys, c: &BTreeMap<u32, f64>, k: usize) -> Handle {
        let cand: Vec<(u32, f64)> = c.iter().map(|(&b, &v)| (b, v)).collect();
        let mut out = Vec::new();
        let tau = reduce(&cand, k, f64::INFINITY, 0, keys, &mut Scratch::default(), &mut out);
        Sketch::stored(&out, tau, tau.is_finite())
    }

    /// The ground truth D, computed as the merge computes it: f32 weights
    /// widened, times the share, in f64.
    fn truth(parts: &[(&BTreeMap<u32, f64>, f64)]) -> BTreeMap<u32, f64> {
        let mut d = BTreeMap::new();
        for (c, s) in parts {
            for (&b, &v) in c.iter() {
                *d.entry(b).or_insert(0.0) += s * (v as f32) as f64;
            }
        }
        d
    }

    fn rk1(keys: &Keys, d: &BTreeMap<u32, f64>, k: usize) -> f64 {
        let mut r: Vec<f64> = d.iter().map(|(&b, &v)| rank(keys, b, v)).collect();
        if r.len() <= k {
            return f64::INFINITY;
        }
        r.sort_by(|a, b| a.total_cmp(b));
        r[k]
    }

    /// Dyadic shares summing to exactly one.
    fn shares(seed: u64, n: usize) -> Vec<f64> {
        let mut raw: Vec<u64> = (0..n as u64).map(|i| 1 + splitmix64(seed + i) % 60).collect();
        let total: u64 = raw.iter().sum();
        // Round to multiples of 1/1024, give the remainder to the first.
        let mut s: Vec<f64> = raw.iter_mut().map(|r| ((*r * 1024) / total) as f64 / 1024.0).collect();
        for x in &mut s {
            if *x == 0.0 {
                *x = 1.0 / 1024.0;
            }
        }
        let rest: f64 = s[1..].iter().sum();
        s[0] = 1.0 - rest;
        assert!(s.iter().all(|&x| x > 0.0));
        s
    }

    fn check_threshold_sample(keys: &Keys, got: &Sketch, d: &BTreeMap<u32, f64>, tau: f64) {
        let want: Vec<(u32, f32)> = d
            .iter()
            .filter(|(&b, &v)| rank(keys, b, v) < tau)
            .map(|(&b, &v)| (b, v as f32))
            .collect();
        let have: Vec<(u32, f32)> = got.entries.iter().map(|e| (e.b, e.w)).collect();
        assert_eq!(have.len(), want.len(), "kept set size");
        for (h, w) in have.iter().zip(&want) {
            assert_eq!(h.0, w.0, "kept set");
            assert_eq!(h.1.to_bits(), w.1.to_bits(), "v_b == D_b as f32, bitwise");
        }
    }

    /// T5, restated in the f32 world: on disjoint supports with dyadic shares,
    /// a record-mode merge of IDEAL threshold samples is exactly the threshold
    /// sample of the mixture at `tau = min(cap, r_(k+1)(D))`, and when the cap
    /// does not bind, the IDEAL bottom-k of D itself.  Repeated nested.
    #[test]
    fn a_merge_of_threshold_samples_on_disjoint_supports_is_exact() {
        let keys = keys();
        let mut unbound = 0;
        for trial in 0..60u64 {
            let mut alg = Sketcher::new(config(Reduce::Record, Cap::Min), Some(keys.clone()));
            let colours: Vec<BTreeMap<u32, f64>> = (0..5)
                .map(|g| {
                    let len = 20 + (splitmix64(trial * 5 + g) % 781) as usize;
                    colour(trial * 5 + g, g as u32 * 10_000, len)
                })
                .collect();
            let sketches: Vec<Handle> = colours.iter().map(|c| ideal(&keys, c, K)).collect();
            let s = shares(trial, 5);
            let ops: Vec<(Handle, f64)> = sketches.iter().cloned().zip(s.iter().copied()).collect();
            let got = alg.record(&ops, trial);
            let parts: Vec<(&BTreeMap<u32, f64>, f64)> = colours.iter().zip(s.iter().copied()).collect();
            let d = truth(&parts);
            let cap = cap_of(sketches.iter().zip(&s).map(|(h, &s)| (s, h.tau)), Cap::Min);
            let r = rk1(&keys, &d, K);
            let tau = cap.min(r);
            assert!((got.tau - tau).abs() <= 1e-6 * tau, "tau {} vs {}", got.tau, tau);
            check_threshold_sample(&keys, &got, &d, got.tau);
            if cap > r {
                unbound += 1;
                let id = ideal(&keys, &d, K);
                assert_eq!(id.entries.len(), got.entries.len());
                for (a, b) in id.entries.iter().zip(got.entries.iter()) {
                    assert_eq!(a.b, b.b);
                }
                assert!((id.tau - got.tau).abs() <= 1e-6 * id.tau);
            }

            // Nested: the merged result is itself an exact threshold sample of
            // D (on its own disjoint support), so merge it again with another.
            let other = colour(trial + 999, 900_000, 300);
            let os = ideal(&keys, &other, K);
            let ops2 = vec![(got.clone(), 0.5), (os.clone(), 0.5)];
            let got2 = alg.record(&ops2, trial + 1);
            // Ground truth from what the first level stored is the honest
            // statement: the first sketch's kept weights *are* f32(D_b).
            let d_as_stored: BTreeMap<u32, f64> = d.clone();
            let d2 = truth(&[(&d_as_stored, 0.5), (&other, 0.5)]);
            let cap2 = cap_of([(0.5, got.tau), (0.5, os.tau)].into_iter(), Cap::Min);
            let tau2 = cap2.min(rk1(&keys, &d2, K));
            assert!((got2.tau - tau2).abs() <= 1e-6 * tau2);
            check_threshold_sample(&keys, &got2, &d2, got2.tau);
        }
        assert!(unbound > 0, "no trial exercised the unbound-cap case");
    }

    /// T5 in step mode: the binary cap, folded pair by pair, still yields a
    /// threshold sample of the mixture at its final tau.
    #[test]
    fn step_mode_with_the_binary_cap_is_a_threshold_sample() {
        let keys = keys();
        for trial in 0..40u64 {
            let mut alg = Sketcher::new(config(Reduce::Step, Cap::Min), Some(keys.clone()));
            let colours: Vec<BTreeMap<u32, f64>> =
                (0..4).map(|g| colour(trial * 4 + g + 7, g as u32 * 20_000, 50 + (g as usize) * 90)).collect();
            let sketches: Vec<Handle> = colours.iter().map(|c| ideal(&keys, c, K)).collect();
            let s = shares(trial + 50, 4);
            let ops: Vec<(Handle, f64)> = sketches.iter().cloned().zip(s.iter().copied()).collect();
            let got = alg.record(&ops, trial);
            let d = truth(&colours.iter().zip(s.iter().copied()).collect::<Vec<_>>());
            assert!(got.entries.len() <= K);
            check_threshold_sample(&keys, &got, &d, got.tau);
        }
    }

    /// `cap=harm` keeps a subset of what `cap=min` keeps, and a prefix of it in
    /// rank order.
    #[test]
    fn the_harmonic_cap_keeps_a_rank_prefix_of_the_min_cap() {
        let keys = keys();
        for trial in 0..60u64 {
            let colours: Vec<BTreeMap<u32, f64>> =
                (0..3).map(|g| colour(trial * 3 + g + 1000, g as u32 * 5_000, 100 + (g as usize) * 200)).collect();
            let sketches: Vec<Handle> = colours.iter().map(|c| ideal(&keys, c, K)).collect();
            let s = shares(trial + 3, 3);
            let ops: Vec<(Handle, f64)> = sketches.iter().cloned().zip(s.iter().copied()).collect();
            let a = Sketcher::new(config(Reduce::Record, Cap::Min), Some(keys.clone())).record(&ops, trial);
            let b = Sketcher::new(config(Reduce::Record, Cap::Harm), Some(keys.clone())).record(&ops, trial);
            assert!(b.tau <= a.tau);
            for e in b.entries.iter() {
                assert!(a.entries.iter().any(|x| x.b == e.b && x.w == e.w), "harm keeps a subset");
            }
            let max_b = b.entries.iter().map(|e| rank(&keys, e.b, e.w as f64)).fold(0.0, f64::max);
            for e in a.entries.iter() {
                if !b.entries.iter().any(|x| x.b == e.b) {
                    assert!(rank(&keys, e.b, e.w as f64) >= max_b, "the dropped ones rank above the kept");
                }
            }
        }
    }

    #[test]
    fn caps_ignore_exact_operands() {
        assert_eq!(cap_of([(0.5, f64::INFINITY)].into_iter(), Cap::Min), f64::INFINITY);
        assert_eq!(cap_of([(0.5, f64::INFINITY)].into_iter(), Cap::Harm), f64::INFINITY);
        assert_eq!(cap_of([(0.5, 2.0), (0.25, 4.0)].into_iter(), Cap::Min), 4.0);
        assert_eq!(cap_of([(0.5, 2.0), (0.5, 2.0)].into_iter(), Cap::Harm), 2.0);
        let _ = unit(0);
    }
}
