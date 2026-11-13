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
//! those out, so a support count here (`n_exact`) is the nonzero support --
//! the one a sketch, which skips zero shares, is compared against.

use crate::algebra::{Algebra, Info, Usage, Work};
use crate::store::ColorStore;
use crate::weighted::{Color, WeightedSets};

pub struct Exact {
    store: WeightedSets,
}

impl Exact {
    pub fn new() -> Exact {
        Exact { store: WeightedSets::new() }
    }

    /// The colour's terms exactly as `--weighted` holds them: ascending,
    /// unnormalised, zero coefficients included.
    #[cfg(test)]
    pub fn terms(&self, c: &Color) -> Vec<(u32, f64)> {
        let mut out = Vec::new();
        self.store.for_each_term(c, |b, w| out.push((b as u32, w)));
        out.reverse();
        out
    }
}

impl Algebra for Exact {
    type C = Color;

    fn singleton(&mut self, block: u32) -> Color {
        self.store.singleton(block as usize)
    }

    fn record(&mut self, ops: &[(Color, f64)], _rec: u64) -> Color {
        let mut acc = self.store.scale(&ops[0].0, ops[0].1);
        for (h, s) in &ops[1..] {
            let next = self.store.combine(h, *s, &acc, 1.0);
            self.store.release(acc);
            acc = next;
        }
        acc
    }

    /// Ascending, zero coefficients left out, normalised by the sum (which is
    /// one to within 1e-9 by the fold's own invariant).
    fn emit(&self, c: &Color, out: &mut Vec<(u32, f64)>) {
        out.clear();
        let mut total = 0.0;
        self.store.for_each_term(c, |b, w| {
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

    fn raw_mass(&self, c: &Color) -> f64 {
        let mut total = 0.0;
        self.store.for_each_term(c, |_, w| total += w);
        total
    }

    fn info(&self, c: &Color) -> Info {
        let mut n = 0;
        self.store.for_each_term(c, |_, w| n += (w != 0.0) as usize);
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

    fn release(&mut self, c: Color) {
        self.store.release(c)
    }
}
