//! # `prio`: priority bottom-k with fresh draws and Horvitz-Thompson weights
//!
//! The literal weighted bottom-k sketch (Duffield, Lund and Thorup's priority
//! sampling), kept as a competitor to `varopt`.  Each candidate gets a priority
//! `q_b = v_b / u_b` with `u_b` a fresh uniform for this reduction and block;
//! the `k` largest priorities are kept, and with `tau_q` the `(k+1)`-th
//! largest, each kept item carries `max(v_b, tau_q)`.  That is its
//! Horvitz-Thompson weight: conditional on the other priorities, `b` is kept
//! with probability `min(1, v_b / tau_q)`.
//!
//! The stored weights stay raw HT and are never renormalised inside the
//! recursion.  Renormalising at every node would turn every node into a ratio
//! estimator, and ratio estimators do not compose unbiasedly; raw HT weights do,
//! by the tower property, so every node's *raw* weights are an unbiased
//! estimate of its exact colour.
//!
//! What is emitted is another matter.  The output is a distribution, so the
//! sketch is normalised at emission, `p_b = w_b / sum w` -- and that is a ratio
//! estimator: consistent, with a bias of order `1/k`, not unbiased.  At `k = 8`
//! a critique measured `E[p_heavy] = 0.269` against an exact `0.25`.  The
//! tests therefore assert unbiasedness of the raw weights, and `mass_raw` (the
//! raw total less one) is reported in `eval` and `chain` so the unnormalised
//! form is visible too.  Over a deep chain the raw mass wanders as a
//! martingale (p05-p95 of 0.66-1.48 in one simulation), which is a reason to
//! expect a worse constant than the per-node `1/sqrt(k)`.
//!
//! The ties rule -- fill with priorities equal to `tau_q` until `k` -- is what
//! the selection below does by construction: `select_nth_unstable` places the
//! `k` largest first whatever their ties.  Ties between `f64` priorities from
//! independent 52-bit draws do not happen in practice.

use crate::hash::prio_draw;

#[derive(Default)]
pub struct Scratch {
    q: Vec<(f64, u32)>,
    keep: Vec<u32>,
}

/// Reduce `cand` (ascending, positive, `len > k`) to exactly `k` points with HT
/// weights, written ascending into `out`.  `x` is the reduction's coordinate.
/// Answers `tau_q`.
pub fn reduce(
    cand: &[(u32, f64)],
    k: usize,
    seed: u64,
    x: u64,
    scratch: &mut Scratch,
    out: &mut Vec<(u32, f64)>,
) -> f64 {
    let n = cand.len();
    assert!(k >= 1 && n > k, "reduce wants more than k = {} candidates, got {}", k, n);
    out.clear();
    let q = &mut scratch.q;
    q.clear();
    q.extend(
        cand.iter()
            .enumerate()
            .map(|(i, &(b, v))| (v / prio_draw(seed, x, b), i as u32)),
    );
    // Descending by priority: the k largest land in q[..k], the (k+1)-th at k.
    q.select_nth_unstable_by(k, |a, b| b.0.total_cmp(&a.0));
    let tau = q[k].0;
    let keep = &mut scratch.keep;
    keep.clear();
    keep.extend(q[..k].iter().map(|&(_, i)| i));
    keep.sort_unstable();
    for &i in keep.iter() {
        let (b, v) = cand[i as usize];
        out.push((b, v.max(tau)));
    }
    tau
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{coordinate, splitmix64, unit};

    /// T4: over independent draws (varying the record), the mean HT weight of
    /// every coordinate is its exact weight within 5 SE, and the output is
    /// always exactly k points.
    #[test]
    fn the_ht_weights_are_unbiased_and_the_size_is_k() {
        let n = 60;
        let k = 12;
        let mut cand: Vec<(u32, f64)> =
            (0..n as u32).map(|i| (i * 999 + 3, 0.2 + unit(splitmix64(i as u64 + 1)))).collect();
        for &i in &[5usize, 23, 41] {
            cand[i].1 = 30.0;
        }
        let mut scratch = Scratch::default();
        let mut out = Vec::new();
        let r = 200_000u64;
        let mut sum = vec![0.0f64; n];
        let mut sq = vec![0.0f64; n];
        for rec in 0..r {
            reduce(&cand, k, 1, coordinate(rec, 0), &mut scratch, &mut out);
            assert_eq!(out.len(), k);
            assert!(out.windows(2).all(|p| p[0].0 < p[1].0));
            let mut j = 0;
            for (i, &(b, _)) in cand.iter().enumerate() {
                let v = if j < out.len() && out[j].0 == b {
                    j += 1;
                    out[j - 1].1
                } else {
                    0.0
                };
                sum[i] += v;
                sq[i] += v * v;
            }
        }
        let rf = r as f64;
        for i in 0..n {
            let mean = sum[i] / rf;
            let se = ((sq[i] / rf - mean * mean).max(0.0) / rf).sqrt();
            assert!((mean - cand[i].1).abs() <= 5.0 * se + 1e-12, "coordinate {}: {} vs {}", i, mean, cand[i].1);
        }
    }
}
