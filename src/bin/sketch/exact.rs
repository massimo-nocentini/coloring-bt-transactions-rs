//! # The exact colour, as an [`Algebra`]
//!
//! `eval` measures every sketch against the truth, and the truth is what
//! `--weighted` computes.  So this does not reimplement it: it wraps
//! [`crate::weighted::WeightedSets`], the very store `run()` drives, and folds
//! a record exactly the way `run()` does -- `acc = scale(op_0, s_0)`, then
//! `acc = combine(op_i, s_i, acc, 1.0)` for every later operand, in fold
//! order, releasing each spent accumulator.  The colours are bit-identical to
//! `--weighted`'s.
//!
//! Unlike the sketches it does *not* group operands or skip zero shares: a
//! zero-share parent is merged at weight 0, as `run()` merges it, and its
//! blocks sit in the exact colour with coefficient 0.  [`Exact::emit`] leaves
//! those out, so a support count here (`n_exact`) is the nonzero support.
//!
//! ## The two supports
//!
//! Every term of the colour, zeros included, is the `sets` support.  The
//! `pos` support -- the blocks some path of positive shares reaches, which
//! is what a `support=pos` KMV store folds -- is *not* the nonzero terms:
//! a coefficient is a product of shares along a path, and at depth it can
//! underflow to 0 in f64 although every share on the path is positive.  On
//! the 2022 chain record 2,499,382 has 9,392 zero coefficients, and its
//! `pos` sketch at `k = 1024` keeps 109 of them.  So the store
//! folds the `pos` support as well, exactly, by set union through the
//! operands of nonzero share: a colour whose `pos` support equals its
//! `sets` support (all but a handful) holds `None`, and the others the
//! ascending blocks of `pos`.  [`Exact::off`] gives the difference.

use crate::algebra::{Algebra, Info, Usage, Work};
use crate::store::ColorStore;
use crate::weighted::{Color, WeightedSets};
use std::rc::Rc;

pub struct Exact {
    store: WeightedSets,
}

/// The exact colour and its `pos` support, `None` when that is every term.
#[derive(Clone)]
pub struct XColor {
    c: Color,
    pos: Option<Rc<[u32]>>,
}

impl Exact {
    pub fn new() -> Exact {
        Exact { store: WeightedSets::new() }
    }

    /// The colour's terms exactly as `--weighted` holds them: ascending,
    /// unnormalised, zero coefficients included.  The support it lists is
    /// the `--sets` one (zero-amount inputs' blocks sit at coefficient 0), so
    /// it is the ground truth of the KMV store as well as of the colour.
    pub fn terms(&self, c: &XColor) -> Vec<(u32, f64)> {
        let mut out = Vec::new();
        self.store.for_each_term(&c.c, |b, w| out.push((b as u32, w)));
        out.reverse();
        out
    }

    /// The blocks of the `sets` support that no path of positive shares
    /// reaches, ascending: `sets` minus `pos`.  Empty for almost every
    /// colour.
    pub fn off(&self, c: &XColor) -> Vec<u32> {
        let Some(pos) = &c.pos else { return Vec::new() };
        let mut out = Vec::new();
        let mut i = pos.len();
        // `for_each_term` walks descending, so does `i`.
        self.store.for_each_term(&c.c, |b, _| {
            let b = b as u32;
            if i > 0 && pos[i - 1] == b {
                i -= 1;
            } else {
                out.push(b);
            }
        });
        debug_assert_eq!(i, 0, "pos is a subset of sets");
        out.reverse();
        out
    }

    /// The colour's blocks, ascending.
    fn blocks(&self, c: &Color) -> Vec<u32> {
        let mut out = Vec::new();
        self.store.for_each_term(c, |b, _| out.push(b as u32));
        out.reverse();
        out
    }

    /// The `pos` support of a record: `None` when every operand has a
    /// nonzero share and a `None` support, or when the union of the
    /// positive operands' supports turns out to be the whole colour.
    fn pos_of(&self, ops: &[(XColor, f64)], acc: &Color) -> Option<Rc<[u32]>> {
        if ops.iter().all(|(h, s)| *s != 0.0 && h.pos.is_none()) {
            return None;
        }
        let mut u: Vec<u32> = Vec::new();
        for (h, s) in ops {
            if *s == 0.0 {
                continue;
            }
            let side = match &h.pos {
                Some(p) => p.to_vec(),
                None => self.blocks(&h.c),
            };
            u = union(&u, &side);
        }
        let mut n = 0;
        self.store.for_each_term(acc, |_, _| n += 1);
        (u.len() != n).then(|| u.into())
    }
}

/// The union of two ascending lists, ascending and without repeats.
fn union(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i] < b[j] {
            out.push(a[i]);
            i += 1;
        } else if b[j] < a[i] {
            out.push(b[j]);
            j += 1;
        } else {
            out.push(a[i]);
            i += 1;
            j += 1;
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

impl Algebra for Exact {
    type C = XColor;

    fn singleton(&mut self, block: u32) -> XColor {
        XColor { c: self.store.singleton(block as usize), pos: None }
    }

    fn record(&mut self, ops: &[(XColor, f64)], _rec: u64) -> XColor {
        let mut acc = self.store.scale(&ops[0].0.c, ops[0].1);
        for (h, s) in &ops[1..] {
            let next = self.store.combine(&h.c, *s, &acc, 1.0);
            self.store.release(acc);
            acc = next;
        }
        let pos = self.pos_of(ops, &acc);
        XColor { c: acc, pos }
    }

    /// Ascending, zero coefficients left out, normalised by the sum (which is
    /// one to within 1e-9 by the fold's own invariant).
    fn emit(&self, c: &XColor, out: &mut Vec<(u32, f64)>) {
        out.clear();
        let mut total = 0.0;
        self.store.for_each_term(&c.c, |b, w| {
            total += w;
            if w != 0.0 {
                out.push((b as u32, w));
            }
        });
        out.reverse();
        if total > 0.0 {
            for x in out.iter_mut() {
                x.1 /= total;
            }
        }
    }

    fn raw_mass(&self, c: &XColor) -> f64 {
        let mut total = 0.0;
        self.store.for_each_term(&c.c, |_, w| total += w);
        total
    }

    fn info(&self, c: &XColor) -> Info {
        let mut n = 0;
        self.store.for_each_term(&c.c, |_, w| n += (w != 0.0) as usize);
        Info { n, tau: 0.0, reduced: false }
    }

    fn work(&self) -> Work {
        Work::default()
    }

    fn usage(&self) -> Usage {
        let (live, peak) = self.store.usage();
        Usage {
            live_entries: live as u64,
            peak_entries: peak as u64,
            live_sketches: 0,
            peak_sketches: 0,
        }
    }

    fn release(&mut self, c: XColor) {
        self.store.release(c.c)
    }
}
