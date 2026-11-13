//! # Weighted similarity, by a linear sketch of the colour
//!
//! Every colour is a probability vector over blocks, so for two of them
//! `sum_b min(x_b, y_b) = 1 - TV` and `sum_b max(x_b, y_b) = 1 + TV`, with
//! `TV = |x - y|_1 / 2`: the weighted Jaccard similarity is
//!
//! ```text
//! J_w = (1 - TV) / (1 + TV),
//! ```
//!
//! a function of the L1 distance alone.  And L1 has a *linear* sketch: with
//! `A` an `m x blocks` matrix of independent standard Cauchy draws, `v(C) =
//! A C` commutes with the recurrence exactly -- `v(sum_i s_i C_i) = sum_i s_i
//! v(C_i)` -- so the store below folds `v` through the DAG with no reduction,
//! no `k` and no per-record randomness, and at any depth holds `A C_t` up to
//! floating point.  By 1-stability each coordinate of `v(x) - v(y)` is
//! Cauchy with scale `|x - y|_1`, so the scale's MLE over the `m` coordinates
//! estimates `2 TV`, with relative SD about `sqrt(2/m)`.
//!
//! That is the reason this exists rather than a weighted MinHash.  A sample
//! by priority ranks `E_b / x_b` (P-MinHash, `coord`) composes under
//! coordinate-wise *max* -- the smallest rank of `max(x, y)` is the smaller
//! of the two sides' -- which is why the support's KMV ([`crate::kmv`]) folds
//! exactly.  It does not compose under *sums*: with `x = (1/2, 1/2, 0, 0)`,
//! `x' = (1/2, 0, 0, 1/2)` and `y = (0, 1/2, 1/2, 0)`, on the event `E1 < E2
//! < E3`, `E1 < E4`, `E2 < 2 E1` (positive probability) the size-1 samples of
//! `x` and `x'` are the same, `(1, 2 E1)`, and `y`'s is the same in both, yet
//! the samples of `(x + y)/2` and `(x' + y)/2` are blocks 2 and 1: no rule
//! can compute the child's sample from the parents' samples.  That is a
//! statement about this class of samplers (priority ranks, a sample of one);
//! nothing here claims it for every weighted sketch.
//!
//! ## Limits
//!
//! - A floor: the clipped estimate `(1 - min(TV,1))/(1 + min(TV,1))` of a
//!   pair with `J_w` near 0 is about `sigma_rel / (2 sqrt(2 pi))`, 0.0125 at
//!   `m = 512`.  Across time `J_w` of deep pairs is about 5e-5, far below it,
//!   so weighted maps mean something only inside a window of blocks.
//! - Memory is `m` values per *distinct live colour*, whatever its support:
//!   16 GB at `m = 512` f32 at the chain's peak, 64 GB at `m = 1024` f64.
//! - The fold accumulates in f64 and stores at the configured precision; the
//!   f32 drift over the whole DAG is measured, not proved (`prec=f64` beside
//!   it is the check).

use crate::algebra::{Algebra, Info, Usage, Work};
use crate::hash::cauchy;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prec {
    F32,
    F64,
}

impl Prec {
    pub fn name(self) -> &'static str {
        match self {
            Prec::F32 => "f32",
            Prec::F64 => "f64",
        }
    }
    pub fn bytes(self) -> u8 {
        match self {
            Prec::F32 => 4,
            Prec::F64 => 8,
        }
    }
}

/// `--l1 <m>[:seed=<u64>][:prec=f32|f64]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct L1Spec {
    pub m: usize,
    pub seed: u64,
    pub prec: Prec,
}

impl L1Spec {
    pub fn parse(text: &str) -> Result<L1Spec, String> {
        let mut parts = text.split(':');
        let m: usize = parts
            .next()
            .and_then(|m| m.parse().ok())
            .ok_or_else(|| format!("--l1 {:?}: expected <m>[:seed=..][:prec=f32|f64]", text))?;
        if !(16..=4096).contains(&m) {
            return Err(format!("--l1 {:?}: m must be in 16..=4096", text));
        }
        let mut s = L1Spec { m, seed: 1, prec: Prec::F32 };
        for opt in parts {
            let (key, value) =
                opt.split_once('=').ok_or_else(|| format!("--l1 {:?}: option {:?} is not key=value", text, opt))?;
            let bad = || format!("--l1 {:?}: bad value in {:?}", text, opt);
            match key {
                "seed" => s.seed = value.parse().map_err(|_| bad())?,
                "prec" => {
                    s.prec = match value {
                        "f32" => Prec::F32,
                        "f64" => Prec::F64,
                        _ => return Err(bad()),
                    }
                }
                _ => return Err(format!("--l1 {:?}: unknown option {:?}", text, key)),
            }
        }
        Ok(s)
    }

    /// `l1-m1024-s1-f32`.
    pub fn label(&self) -> String {
        format!("l1-m{}-s{}-{}", self.m, self.seed, self.prec.name())
    }
}

/// One colour's projection, at the store's precision.
#[derive(Debug, PartialEq)]
pub enum Vector {
    F32(Box<[f32]>),
    F64(Box<[f64]>),
}

impl Vector {
    pub fn len(&self) -> usize {
        match self {
            Vector::F32(v) => v.len(),
            Vector::F64(v) => v.len(),
        }
    }
    #[inline]
    pub fn get(&self, j: usize) -> f64 {
        match self {
            Vector::F32(v) => v[j] as f64,
            Vector::F64(v) => v[j],
        }
    }
    pub fn to_f64(&self) -> Vec<f64> {
        (0..self.len()).map(|j| self.get(j)).collect()
    }
}

pub type LHandle = Rc<Vector>;

thread_local! {
    static USAGE: Cell<Usage> = const { Cell::new(Usage {
        live_entries: 0, peak_entries: 0, live_sketches: 0, peak_sketches: 0,
    }) };
}

/// A stored vector; the counters count values and distinct vectors.
fn stored(acc: &[f64], prec: Prec) -> LHandle {
    let v = match prec {
        Prec::F32 => Vector::F32(acc.iter().map(|&x| x as f32).collect()),
        Prec::F64 => Vector::F64(acc.into()),
    };
    USAGE.with(|u| {
        let mut x = u.get();
        x.live_entries += acc.len() as u64;
        x.live_sketches += 1;
        x.peak_entries = x.peak_entries.max(x.live_entries);
        x.peak_sketches = x.peak_sketches.max(x.live_sketches);
        u.set(x);
    });
    Rc::new(v)
}

/// The store's own handle type would need a `Drop` on `Vector`; keeping the
/// counters here instead lets `pairs` hold plain vectors read from a file.
pub struct L1Store {
    pub spec: L1Spec,
    groups: Vec<(usize, f64)>,
    group_of: HashMap<usize, usize>,
    acc: Vec<f64>,
}

impl L1Store {
    pub fn new(spec: L1Spec) -> L1Store {
        L1Store { spec, groups: Vec::new(), group_of: HashMap::new(), acc: vec![0.0; spec.m] }
    }

    /// The operands with a positive share, by pointer, shares summed:
    /// `s1 v + s2 v = (s1 + s2) v`, as `Sketcher::group`.
    fn group(&mut self, ops: &[(LHandle, f64)]) {
        self.groups.clear();
        if ops.len() <= 16 {
            for (i, (h, s)) in ops.iter().enumerate() {
                if *s == 0.0 {
                    continue;
                }
                match self.groups.iter_mut().find(|g| Rc::ptr_eq(&ops[g.0].0, h)) {
                    Some(g) => g.1 += *s,
                    None => self.groups.push((i, *s)),
                }
            }
        } else {
            self.group_of.clear();
            for (i, (h, s)) in ops.iter().enumerate() {
                if *s == 0.0 {
                    continue;
                }
                let p = Rc::as_ptr(h) as usize;
                match self.group_of.get(&p) {
                    Some(&g) => self.groups[g].1 += *s,
                    None => {
                        self.group_of.insert(p, self.groups.len());
                        self.groups.push((i, *s));
                    }
                }
            }
        }
    }

    /// Release a handle, keeping the counters right: the last reference to a
    /// vector takes its values off the live count.
    fn drop_handle(h: LHandle) {
        if let Ok(v) = Rc::try_unwrap(h) {
            USAGE.with(|u| {
                let mut x = u.get();
                x.live_entries -= v.len() as u64;
                x.live_sketches -= 1;
                u.set(x);
            });
        }
    }
}

impl Algebra for L1Store {
    type C = LHandle;

    /// `v_j = cauchy(seed, b, j)`: `m` tangents per coinbase.
    fn singleton(&mut self, block: u32) -> LHandle {
        for (j, a) in self.acc.iter_mut().enumerate() {
            *a = cauchy(self.spec.seed, block, j as u32);
        }
        stored(&self.acc, self.spec.prec)
    }

    /// `acc_j = sum_g S_g v_g[j]` in f64, in group order (so deterministic),
    /// stored at the configured precision.  One group is its parent's own
    /// vector: the shares sum to one.
    fn record(&mut self, ops: &[(LHandle, f64)], rec: u64) -> LHandle {
        self.group(ops);
        assert!(!self.groups.is_empty(), "record {} has no operand with a positive share", rec);
        if self.groups.len() == 1 {
            return Rc::clone(&ops[self.groups[0].0].0);
        }
        self.acc.iter_mut().for_each(|a| *a = 0.0);
        for &(i, s) in &self.groups {
            match &*ops[i].0 {
                Vector::F32(v) => {
                    for (a, &x) in self.acc.iter_mut().zip(v.iter()) {
                        *a += s * x as f64;
                    }
                }
                Vector::F64(v) => {
                    for (a, &x) in self.acc.iter_mut().zip(v.iter()) {
                        *a += s * x;
                    }
                }
            }
        }
        stored(&self.acc, self.spec.prec)
    }

    /// Nothing: a projection is not a distribution over blocks, and no mode
    /// asks an L1 store for one.
    fn emit(&self, _c: &LHandle, out: &mut Vec<(u32, f64)>) {
        out.clear();
    }

    fn raw_mass(&self, _c: &LHandle) -> f64 {
        1.0
    }

    fn info(&self, c: &LHandle) -> Info {
        Info { n: c.len(), tau: 0.0, reduced: false }
    }

    fn work(&self) -> Work {
        Work::default()
    }

    fn usage(&self) -> Usage {
        USAGE.with(|u| u.get())
    }

    fn release(&mut self, c: LHandle) {
        L1Store::drop_handle(c)
    }
}

// ------------------------------------------------------------------ estimators

/// The Cauchy scale MLE of `d`: the root of `sum_j s^2/(s^2 + d_j^2) = m/2`.
///
/// The left side increases in `s` from (the number of zeros) to `m`, so the
/// root is unique, and 0 when at least half the `d_j` are exactly 0 (equal
/// colours give all zeros).  A non-finite `d_j` contributes 0, as an infinite
/// one would.  Newton on `t = ln s` from `ln median|d|`, safeguarded by a
/// bisection bracket; at most 200 steps, relative tolerance 1e-12.  The mean
/// or geometric mean of `|d|` would be the obvious estimators, and both are
/// wrong for Cauchy tails (the first has no expectation at all).
pub fn cauchy_scale_mle(d: &[f64]) -> f64 {
    let m = d.len();
    if m == 0 {
        return f64::NAN;
    }
    let half = m as f64 / 2.0;
    let mut a: Vec<f64> = d.iter().map(|x| if x.is_finite() { x.abs() } else { f64::INFINITY }).collect();
    let zeros = a.iter().filter(|&&x| x == 0.0).count();
    if zeros as f64 >= half {
        return 0.0;
    }
    let mid = a.len() / 2;
    let med = *a.select_nth_unstable_by(mid, |x, y| x.total_cmp(y)).1;
    // f(t) = sum 1/(1 + (a/s)^2) - m/2, f'(t) = sum 2 q (1 - q).
    let f = |t: f64| -> (f64, f64) {
        let inv = (-t).exp();
        let (mut v, mut dv) = (0.0f64, 0.0f64);
        for &x in &a {
            let r = x * inv;
            let q = 1.0 / (1.0 + r * r);
            v += q;
            dv += 2.0 * q * (1.0 - q);
        }
        (v - half, dv)
    };
    let mut t = if med > 0.0 && med.is_finite() {
        med.ln()
    } else {
        // Half zeros or half infinities: start from any finite positive one.
        a.iter().copied().find(|x| *x > 0.0 && x.is_finite()).unwrap_or(1.0).ln()
    };
    // A bracket [lo, hi] with f(lo) < 0 < f(hi).
    let (mut lo, mut hi) = (t, t);
    let mut step = 1.0;
    for _ in 0..2000 {
        if f(lo).0 < 0.0 {
            break;
        }
        lo -= step;
        step *= 2.0;
    }
    step = 1.0;
    for _ in 0..2000 {
        if f(hi).0 > 0.0 {
            break;
        }
        hi += step;
        step *= 2.0;
    }
    for _ in 0..200 {
        let (v, dv) = f(t);
        if v == 0.0 {
            break;
        }
        if v < 0.0 {
            lo = t;
        } else {
            hi = t;
        }
        let mut next = t - v / dv;
        if !(dv > 0.0) || !(next > lo && next < hi) {
            next = 0.5 * (lo + hi);
        }
        let done = (next - t).abs() <= 1e-12 || (hi - lo) <= 1e-12;
        t = next;
        if done {
            break;
        }
    }
    t.exp()
}

/// `TV^ = sigma^ / 2` between two projections (unclipped: an average over
/// many pairs should use this rather than the clipped `J_w`).  The same `Rc`
/// or bit-equal vectors give exactly 0.
pub fn tv_hat(x: &Vector, y: &Vector) -> f64 {
    assert_eq!(x.len(), y.len(), "projections of the same m");
    let d: Vec<f64> = (0..x.len()).map(|j| x.get(j) - y.get(j)).collect();
    cauchy_scale_mle(&d) / 2.0
}

/// `(1 - min(tv,1)) / (1 + min(tv,1))`.
pub fn jw_of_tv(tv: f64) -> f64 {
    let t = tv.clamp(0.0, 1.0);
    (1.0 - t) / (1.0 + t)
}

/// The delta-method SE of `J_w^`: `2 tv/(1+tv)^2 . sqrt(2/m)`.
pub fn jw_se(tv: f64, m: usize) -> f64 {
    let t = tv.clamp(0.0, 1.0);
    2.0 * t / (1.0 + t).powi(2) * (2.0 / m as f64).sqrt()
}

/// The L1 selection file `pick` writes for one `--l1`:
///
/// ```text
/// "L1SEL001" | u32 version=1 | u32 m | u64 seed | u8 prec (4|8) | u8[3] 0 | u32 N
/// N x m values (f32 or f64, little-endian)
/// ```
pub const SEL_MAGIC: &[u8; 8] = b"L1SEL001";

pub fn sel_header(spec: &L1Spec, n: u32) -> Vec<u8> {
    let mut h = Vec::with_capacity(32);
    h.extend_from_slice(SEL_MAGIC);
    h.extend_from_slice(&1u32.to_le_bytes());
    h.extend_from_slice(&(spec.m as u32).to_le_bytes());
    h.extend_from_slice(&spec.seed.to_le_bytes());
    h.push(spec.prec.bytes());
    h.extend_from_slice(&[0u8; 3]);
    h.extend_from_slice(&n.to_le_bytes());
    h
}

pub fn sel_record(out: &mut impl std::io::Write, v: &Vector) -> std::io::Result<()> {
    match v {
        Vector::F32(x) => x.iter().try_for_each(|y| out.write_all(&y.to_le_bytes())),
        Vector::F64(x) => x.iter().try_for_each(|y| out.write_all(&y.to_le_bytes())),
    }
}

pub fn read_sel(mut input: impl std::io::Read) -> std::io::Result<(L1Spec, Vec<Vector>)> {
    let bad = |m: &str| std::io::Error::new(std::io::ErrorKind::InvalidData, m.to_string());
    let mut h = [0u8; 32];
    input.read_exact(&mut h)?;
    if &h[..8] != SEL_MAGIC {
        return Err(bad("not an L1SEL001 file"));
    }
    let m = u32::from_le_bytes(h[12..16].try_into().unwrap()) as usize;
    let seed = u64::from_le_bytes(h[16..24].try_into().unwrap());
    let prec = match h[24] {
        4 => Prec::F32,
        8 => Prec::F64,
        _ => return Err(bad("bad precision byte")),
    };
    let n = u32::from_le_bytes(h[28..32].try_into().unwrap()) as usize;
    let w = prec.bytes() as usize;
    let mut raw = vec![0u8; m * w];
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        input.read_exact(&mut raw)?;
        out.push(match prec {
            Prec::F32 => Vector::F32(raw.chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect()),
            Prec::F64 => Vector::F64(raw.chunks_exact(8).map(|c| f64::from_le_bytes(c.try_into().unwrap())).collect()),
        });
    }
    Ok((L1Spec { m, seed, prec }, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hash::{splitmix64, unit};

    #[test]
    fn specs_parse_and_label() {
        assert_eq!(L1Spec::parse("1024").unwrap().label(), "l1-m1024-s1-f32");
        assert_eq!(L1Spec::parse("512:seed=2:prec=f64").unwrap().label(), "l1-m512-s2-f64");
        assert!(L1Spec::parse("8").is_err());
        assert!(L1Spec::parse("64:prec=f16").is_err());
    }

    /// `A x` for a sparse probability vector `x`, directly.
    fn project(x: &[(u32, f64)], m: usize, seed: u64) -> Vector {
        Vector::F64((0..m).map(|j| x.iter().map(|&(b, w)| w * cauchy(seed, b, j as u32)).sum()).collect())
    }

    /// Two probability vectors at total variation `tv`: `x` uniform on 50
    /// blocks, `y` moving mass `tv` of it to 50 fresh blocks.
    fn pair_at(tv: f64, salt: u64) -> (Vec<(u32, f64)>, Vec<(u32, f64)>) {
        let base = (salt * 1000) as u32;
        let x: Vec<(u32, f64)> = (0..50).map(|i| (base + i, 1.0 / 50.0)).collect();
        let mut y: Vec<(u32, f64)> = (0..50).map(|i| (base + i, (1.0 - tv) / 50.0)).collect();
        y.extend((0..50).map(|i| (base + 500 + i, tv / 50.0)));
        (x, y)
    }

    /// T-L2: on synthetic pairs with TV in {0.001, 0.01, 0.1, 0.5, 1}, m in
    /// {128, 512}, over 400 seeds (each an independent projection), the
    /// relative bias of TV^ is at most 2% and its relative SD times sqrt(m)
    /// lies in [1.30, 1.52] (sqrt 2 = 1.414 asymptotically).
    #[test]
    fn the_mle_is_calibrated_over_seeds() {
        for &m in &[128usize, 512] {
            for &tv in &[0.001, 0.01, 0.1, 0.5, 1.0] {
                let (x, y) = pair_at(tv, 3);
                let (mut s1, mut s2) = (0.0f64, 0.0f64);
                let seeds = 400;
                for seed in 0..seeds {
                    let t = tv_hat(&project(&x, m, seed + 77), &project(&y, m, seed + 77)) / tv;
                    s1 += t;
                    s2 += t * t;
                }
                let mean = s1 / seeds as f64;
                let sd = (s2 / seeds as f64 - mean * mean).sqrt();
                assert!((mean - 1.0).abs() <= 0.02, "m {} tv {}: relative bias {}", m, tv, mean - 1.0);
                let r = sd * (m as f64).sqrt();
                assert!((1.30..=1.52).contains(&r), "m {} tv {}: sd sqrt(m) = {}", m, tv, r);
            }
        }
    }

    /// T-L3: identical colours are exactly TV 0 and J_w 1; disjoint ones are
    /// TV 1 within 3 sqrt(2/m); injected infinities and huge values leave the
    /// MLE finite.
    #[test]
    fn identical_disjoint_and_wild_inputs() {
        let m = 256;
        let (x, _) = pair_at(0.0, 5);
        let a = project(&x, m, 1);
        let b = project(&x, m, 1);
        assert_eq!(tv_hat(&a, &a), 0.0);
        assert_eq!(tv_hat(&a, &b), 0.0);
        assert_eq!(jw_of_tv(tv_hat(&a, &b)), 1.0);
        let (y, _) = pair_at(0.0, 9);
        let t = tv_hat(&a, &project(&y, m, 1));
        assert!((t - 1.0).abs() <= 3.0 * (2.0 / m as f64).sqrt(), "disjoint: {}", t);
        assert_eq!(jw_of_tv(1.7), 0.0);
        let mut d: Vec<f64> = (0..m).map(|j| (std::f64::consts::PI * (unit(splitmix64(j as u64)) - 0.5)).tan()).collect();
        d[0] = f64::INFINITY;
        d[1] = f64::NEG_INFINITY;
        d[2] = 1e15;
        d[3] = -1e15;
        d[4] = f64::NAN;
        let s = cauchy_scale_mle(&d);
        assert!(s.is_finite() && s > 0.5 && s < 2.0, "{}", s);
        assert_eq!(cauchy_scale_mle(&[0.0, 0.0, 1.0, 2.0]), 0.0);
    }

    /// T-L4: TV^ under two seeds is uncorrelated over 500 pairs.
    #[test]
    fn seeds_are_independent() {
        let m = 128;
        let n = 500;
        let (mut s1, mut s2, mut s11, mut s22, mut s12) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        for p in 0..n {
            let (x, y) = pair_at(0.3, p + 10);
            let a = tv_hat(&project(&x, m, 1), &project(&y, m, 1));
            let b = tv_hat(&project(&x, m, 2), &project(&y, m, 2));
            s1 += a;
            s2 += b;
            s11 += a * a;
            s22 += b * b;
            s12 += a * b;
        }
        let nf = n as f64;
        let cov = s12 / nf - s1 / nf * s2 / nf;
        let r = cov / ((s11 / nf - (s1 / nf).powi(2)) * (s22 / nf - (s2 / nf).powi(2))).sqrt();
        assert!(r.abs() < 4.0 / nf.sqrt(), "correlation {}", r);
    }

    #[test]
    fn selections_round_trip() {
        for prec in [Prec::F32, Prec::F64] {
            let spec = L1Spec { m: 16, seed: 3, prec };
            let v: Vec<f64> = (0..16).map(|j| j as f64 * 0.5 - 3.0).collect();
            let h = stored(&v, prec);
            let mut b = sel_header(&spec, 1);
            sel_record(&mut b, &h).unwrap();
            let (s, back) = read_sel(&b[..]).unwrap();
            assert_eq!(s, spec);
            assert_eq!(back[0], *h);
            L1Store::drop_handle(h);
        }
    }
}
