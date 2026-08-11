//! # Polynomials as circular linked lists
//!
//! An exercise from Knuth's *TAOCP* §2.2.4, "Circular Lists", ported from
//! `src/test/circular-polynomial.scm`.
//!
//! ## Representation
//!
//! A polynomial is a **ring** of nodes.  Each node carries a term
//! `(exponent, coefficient)` and a link to the next node.  Terms are kept sorted
//! by *strictly* decreasing exponent, so an exponent occurs at most once.
//!
//! The node a polynomial is named by is not one of its terms: it is a
//! **sentinel** carrying an impossible term, and the last real term links back
//! to it.  So `x^5 + 2x^2 + 1` is
//!
//! ```text
//!    ┌──────────────────────────────────────────┐
//!    │                                          │
//!    └─> sentinel ─> (5, 1) ─> (2, 2) ─> (0, 1)
//! ```
//!
//! and the zero polynomial is a sentinel pointing at itself.
//!
//! ## Why the sentinel
//!
//! The sentinel's exponent is chosen to sort *below* every real term.  That
//! single fact removes every end-of-list test from the merge loop:
//!
//! - a walk can never run off the end — it wraps around instead;
//! - when one operand is exhausted and the other is not, the exhausted one sits
//!   on its sentinel and loses every comparison, so the remaining terms of the
//!   other are emitted by the ordinary "smaller exponent" branch;
//! - the loop stops on the one situation that cannot arise between two real
//!   terms: *equal* exponents that are both impossible, i.e. both operands
//!   standing on their sentinel at the same time.  Equal exponents otherwise
//!   mean "combine the two coefficients".
//!
//! So [`Arena::op`] is a bare three-way comparison with no boundary cases.
//!
//! ## Exponents are stored biased by one
//!
//! The Scheme spells that impossible exponent `-1`.  Nothing below 0 exists in
//! an unsigned world, and 0 itself is a perfectly ordinary exponent here — block
//! 0 is the genesis block, and `(0 . 1)` is the first line the driver prints.
//! So the *stored* exponent, called a **key** below, is the exponent plus one,
//! which frees 0 to be the sentinel and leaves the ordering untouched.
//!
//! The bias never escapes this module: [`Arena::make`] applies it and
//! [`Arena::for_each_term`] undoes it.  [`Arena::op`] copies keys across without
//! ever looking at what they mean, so the merge is written exactly as it would
//! be against raw exponents.
//!
//! ## Links are indices, not pointers
//!
//! Rings are cycles, and a cycle of owning pointers is exactly what Rust's
//! ownership model refuses to express (and what reference counting fails to
//! reclaim).  So the nodes live in one flat [`Arena`] and a link is an index
//! into it.  The topology is the Scheme original's, node for node; only the
//! representation of "next" changed.
//!
//! The arena also has to do the job Scheme's collector was doing: a ring that
//! falls out of use is handed back with [`Arena::free_ring`], which pushes its
//! nodes onto a free list for [`Arena::alloc`] to reuse.  Nothing here detects a
//! leaked ring — a caller that drops a root on the floor grows the arena
//! forever.
//!
//! ## Caveat
//!
//! Combining does not prune cancelled terms: a term whose coefficients combine
//! to 0 stays in the ring, so the representation is not canonical.  This matches
//! the Scheme.

/// Exponents.  In the bitcoin-colors driver these are block ids.
pub type Exp = usize;

/// Coefficients.  Unsigned, so [`Arena::add`] is only meaningful for
/// polynomials that never need a negative term — which the colors driver, whose
/// coefficients are all 1, does not.
pub type Coeff = usize;

/// A link: an index into [`Arena::nodes`], *not* a pointer.
pub type Idx = usize;

/// A stored exponent: the exponent plus one, so that 0 is free to mean "no
/// exponent at all" and sorts below every real term.  See the module docs.
type Key = Exp;

/// Below every legal key, which is what makes the merge free of boundary tests.
const SENTINEL_KEY: Key = 0;

#[inline]
fn key_of(exp: Exp) -> Key {
    exp + 1
}

#[inline]
fn exp_of(key: Key) -> Exp {
    key - 1
}

struct Node {
    coeff: Coeff,
    key: Key,
    next: Idx,
}

pub struct Arena {
    nodes: Vec<Node>,
    free: Vec<Idx>,
}

impl Arena {
    pub fn new() -> Self {
        Arena {
            nodes: Vec::new(),
            free: Vec::new(),
        }
    }

    #[inline]
    fn at(&self, i: Idx) -> &Node {
        &self.nodes[i]
    }

    #[inline]
    fn at_mut(&mut self, i: Idx) -> &mut Node {
        &mut self.nodes[i]
    }

    #[inline]
    fn alloc(&mut self, key: Key, coeff: Coeff, next: Idx) -> Idx {
        match self.free.pop() {
            Some(i) => {
                *self.at_mut(i) = Node { coeff, key, next };
                i
            }
            None => {
                let i = self.nodes.len();
                self.nodes.push(Node { coeff, key, next });
                i
            }
        }
    }

    /// A fresh sentinel pointing at itself: the empty ring, ready to be filled.
    #[inline]
    fn empty_ring(&mut self) -> Idx {
        let root = self.alloc(SENTINEL_KEY, 0, 0);
        self.at_mut(root).next = root;
        root
    }

    /// Build a polynomial from terms that are already in strictly decreasing
    /// exponent order.  `make(&[])` is the zero polynomial.
    ///
    /// Every polynomial owns a fresh sentinel, so no two rings share structure.
    pub fn make(&mut self, terms: &[(Exp, Coeff)]) -> Idx {
        let root = self.empty_ring();
        let mut tail = root;
        for &(exp, coeff) in terms {
            assert!(exp < Exp::MAX, "exponent {} has no biased key", exp);
            let node = self.alloc(key_of(exp), coeff, root);
            self.at_mut(tail).next = node;
            tail = node;
        }
        root
    }

    /// Walk both rings from their sentinels, combining `p` and `q` term by term
    /// with `f`.  The result is a fresh ring whose nodes are freshly allocated
    /// too, so mutating or freeing it can never reach back into either operand,
    /// and both operands are left untouched.
    ///
    /// The Scheme builds this with non-tail recursion; here it is a loop with an
    /// explicit tail cursor, because a color can carry tens of thousands of
    /// terms and one stack frame per term would not survive that.  Since
    /// [`Arena::alloc`] links each new node straight back to `root`, the ring is
    /// closed at every step rather than only at the end.
    pub fn op(&mut self, p: Idx, q: Idx, f: impl Fn(Coeff, Coeff) -> Coeff) -> Idx {
        let root = self.empty_ring();
        let mut tail = root;
        let mut p_ = self.at(p).next;
        let mut q_ = self.at(q).next;

        loop {
            let (i, ci) = {
                let n = self.at(p_);
                (n.key, n.coeff)
            };
            let (j, cj) = {
                let n = self.at(q_);
                (n.key, n.coeff)
            };

            let (key, coeff) = if i < j {
                q_ = self.at(q_).next;
                (j, cj)
            } else if i > j {
                p_ = self.at(p_).next;
                (i, ci)
            } else if i == SENTINEL_KEY {
                // Both operands stand on their sentinel: the only way two keys
                // can be equal and impossible.  The ring is already closed onto
                // `root`.
                break;
            } else {
                p_ = self.at(p_).next;
                q_ = self.at(q_).next;
                (i, f(ci, cj))
            };

            let node = self.alloc(key, coeff, root);
            self.at_mut(tail).next = node;
            tail = node;
        }

        root
    }

    /// `+/polynomial`.  The colors driver only wants `ior`, but this is the
    /// instantiation the exercise is actually about, so it stays.  With [`Coeff`]
    /// unsigned it is addition only — the Scheme's cancelling-terms case needs
    /// coefficients this type cannot hold.
    #[allow(dead_code)]
    pub fn add(&mut self, p: Idx, q: Idx) -> Idx {
        self.op(p, q, |a, b| a + b)
    }

    pub fn ior(&mut self, p: Idx, q: Idx) -> Idx {
        self.op(p, q, |a, b| a | b)
    }

    /// Hand a ring back for reuse.  The caller must not hold `root`, or any
    /// index reachable from it, afterwards.
    pub fn free_ring(&mut self, root: Idx) {
        let mut cur = root;
        loop {
            let next = self.at(cur).next;
            self.free.push(cur);
            if next == root {
                break;
            }
            cur = next;
        }
    }

    /// Visit the terms in decreasing exponent order, skipping the sentinel.
    pub fn for_each_term(&self, root: Idx, mut f: impl FnMut(Exp, Coeff)) {
        let mut cur = self.at(root).next;
        while cur != root {
            let n = self.at(cur);
            f(exp_of(n.key), n.coeff);
            cur = n.next;
        }
    }

    /// Nodes in the ring, sentinel included.  Diverges on a broken ring, which
    /// the constructors here cannot produce.
    pub fn ring_len(&self, root: Idx) -> usize {
        let mut cur = self.at(root).next;
        let mut n = 1;
        while cur != root {
            cur = self.at(cur).next;
            n += 1;
        }
        n
    }

    /// Nodes the arena has committed to, including those on the free list.
    pub fn capacity(&self) -> usize {
        self.nodes.len()
    }

    /// Nodes currently reachable from some live ring — assuming no ring has been
    /// leaked, which is the point of watching this number.
    pub fn live(&self) -> usize {
        self.nodes.len() - self.free.len()
    }
}
