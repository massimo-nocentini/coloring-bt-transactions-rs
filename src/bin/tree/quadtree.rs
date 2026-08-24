//! # The nodes, sorted by where they are
//!
//! A window shows a few hundred thousand pixels and the drawings this indexes
//! run to millions of nodes, so the only affordable question to ask before a
//! frame is *which nodes are on screen* — and asking it of a `Vec` means
//! touching every node, every frame, however few of them are visible.  A
//! quadtree answers it in time proportional to the answer: quarter the drawing's
//! square, keep quartering wherever more than [`CAPACITY`] nodes land together,
//! and a query walks only the cells the window actually overlaps.
//!
//! # It indexes points it does not hold
//!
//! The tree stores no coordinates.  It stores a permutation — [`Quadtree::over`]
//! is given the points, sorts a `Vec<u32>` of *indices* into them, and every
//! query takes the points back as an argument.  Two reasons, and the second is
//! the one that matters:
//!
//! - The points are already in the scene beside the node numbers and the parent
//!   links that the drawing needs; a second copy would be 16 bytes a node for
//!   nothing.
//! - The answer to a query has to name nodes, not places.  A picture where
//!   clicking a circle tells you which transaction it is needs the index, so the
//!   index is what the leaves hold.
//!
//! # Two kinds of answer
//!
//! What a query wants back depends on how far out the camera is, and it is the
//! same walk either way, stopped at a different depth.  [`Quadtree::visit`] takes
//! a `resolution` — the size, in node units, below which a cell is not worth
//! opening — and hands back a [`Patch`] for each cell it stops at:
//!
//! - Zoomed in, `resolution` is a fraction of a node, the walk runs to the
//!   leaves, and every patch is [`Patch::Nodes`]: these particular nodes, draw
//!   them as circles.
//! - Zoomed out, `resolution` is a pixel's worth of nodes, the walk stops high,
//!   and a patch is [`Patch::Cluster`]: *this* many nodes somewhere in this
//!   square, which is a dot of a shade.
//!
//! The second is what makes a drawing of ten million nodes cost a frame rather
//! than a coffee: the work is bounded by the pixels in the window, not by the
//! nodes in the graph, because a cell smaller than a pixel is summarised instead
//! of opened.  Counts are kept on every cell precisely so that summary is free.
//!
//! # Why the walks carry their own stack
//!
//! Same reason [`forest`](super) asks the layout crate for its flat entry point:
//! the drawings are chains a million levels deep.  A quadtree over them is not
//! that deep — subdivision is spatial, so the depth is bounded by
//! [`MAX_DEPTH`] — but the habit is cheap to keep and the build, which
//! partitions cell by cell, would otherwise recurse once per level anyway.

// These three files are a small library with no `lib.rs` to live in -- `src/*.rs`
// belong to the main binary, so `tree-view` reaches them by `#[path]` and
// `tests/viewer_geometry.rs` does the same.  Compiled into a binary crate,
// anything one frame does not happen to call reads as dead; the surface is the
// point, so the lint goes rather than the surface.
#![allow(dead_code)]

use crate::camera::Rect;

/// How many nodes a cell may hold before it is quartered.
///
/// Small enough that a leaf scan is short, large enough that the tree over a
/// million nodes is some hundred thousand cells rather than a million.
pub const CAPACITY: usize = 16;

/// How far the subdivision will go before it gives up and leaves a cell large.
///
/// Quartering separates points that differ; points that do *not* differ can be
/// quartered forever and stay together.  The layout never places two nodes in
/// the same spot, so this is a backstop rather than a working limit — at depth
/// 48 a cell is 2^-48 of the drawing, which is past what an `f64` coordinate a
/// million wide can distinguish anyway.
pub const MAX_DEPTH: usize = 48;

/// Marks a cell with nothing under it: it is a leaf.
const NO_CHILDREN: u32 = u32::MAX;

/// A cell of the tree.
///
/// `start` and `len` are a range of [`Quadtree::order`], and they describe the
/// whole subtree rather than the cell alone: a cell's points are exactly its
/// children's points, laid end to end, which is what the partitioning build
/// arranges and what makes `len` a running count for free.
#[derive(Clone, Copy, Debug)]
struct Cell {
    bounds: Rect,
    start: u32,
    len: u32,
    /// The first of four consecutive cells, or [`NO_CHILDREN`].
    children: u32,
}

/// What a walk stopped at.
#[derive(Clone, Copy, Debug)]
pub enum Patch<'a> {
    /// These nodes, by index into the points the query was given.
    Nodes(&'a [u32]),
    /// This many nodes, somewhere in this square, not worth naming one by one.
    Cluster { bounds: Rect, count: u32 },
}

/// The nodes of a drawing, in a tree of squares.
pub struct Quadtree {
    cells: Vec<Cell>,
    /// The points' indices, permuted so that each cell's are contiguous.
    order: Vec<u32>,
    depth: usize,
}

impl Quadtree {
    /// Indexes `points`, which are node centres in node units.
    ///
    /// The root is the smallest square holding them all, so quartering gives
    /// squares all the way down and a cell's size is one number.
    pub fn over(points: &[[f64; 2]]) -> Quadtree {
        let mut bounds = Rect::nothing();
        for p in points {
            bounds.add(p[0], p[1]);
        }
        let mut bounds = bounds.to_square();
        // A point on the far edge of the square is *in* the square, and the
        // quadrant test sends it right and down, off the end.  Widening by a
        // sliver puts it back inside without moving anything that was already
        // comfortably in.  The sliver is relative, since the drawings run from
        // one node across to a million.
        let sliver = (bounds.width() * 1e-9).max(f64::MIN_POSITIVE);
        bounds = bounds.grown(sliver);

        let mut tree = Quadtree {
            cells: Vec::new(),
            order: (0..points.len() as u32).collect(),
            depth: 0,
        };

        if points.is_empty() {
            return tree;
        }

        tree.cells.push(Cell {
            bounds,
            start: 0,
            len: points.len() as u32,
            children: NO_CHILDREN,
        });

        // One scratch buffer for the whole build: partitioning a cell reads its
        // slice of `order` into here in quadrant order and copies it back.
        let mut scratch: Vec<u32> = vec![0; points.len()];
        let mut stack: Vec<(u32, usize)> = vec![(0, 0)];

        while let Some((at, depth)) = stack.pop() {
            tree.depth = tree.depth.max(depth);

            let cell = tree.cells[at as usize];
            if (cell.len as usize) <= CAPACITY || depth >= MAX_DEPTH {
                continue;
            }

            let (cx, cy) = cell.bounds.centre();
            let start = cell.start as usize;
            let len = cell.len as usize;
            let slice = &mut tree.order[start..start + len];

            // Counting sort into the four quadrants, numbered so that bit 0 is
            // "right of centre" and bit 1 is "below it".
            let mut counts = [0u32; 4];
            for &i in slice.iter() {
                counts[quadrant(points[i as usize], cx, cy)] += 1;
            }
            let mut offsets = [0u32; 4];
            let mut running = 0;
            for q in 0..4 {
                offsets[q] = running;
                running += counts[q];
            }
            let bases = offsets;
            for &i in slice.iter() {
                let q = quadrant(points[i as usize], cx, cy);
                scratch[offsets[q] as usize] = i;
                offsets[q] += 1;
            }
            slice.copy_from_slice(&scratch[..len]);

            let first = tree.cells.len() as u32;
            tree.cells[at as usize].children = first;
            for q in 0..4 {
                let child = Cell {
                    bounds: quarter(cell.bounds, cx, cy, q),
                    start: cell.start + bases[q],
                    len: counts[q],
                    children: NO_CHILDREN,
                };
                tree.cells.push(child);
                // An empty quadrant is kept — the four are consecutive, which is
                // what lets a cell name them with one number — but never opened.
                if counts[q] > 0 {
                    stack.push((first + q as u32, depth + 1));
                }
            }
        }

        tree
    }

    /// The square the whole drawing is in, or an empty rectangle if there is no
    /// drawing.
    pub fn bounds(&self) -> Rect {
        self.cells.first().map_or(Rect::nothing(), |c| c.bounds)
    }

    /// How many nodes are indexed.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// How many cells the tree has, and how deep it got: what a run reports so
    /// that an index gone wrong is visible rather than merely slow.
    pub fn cells(&self) -> usize {
        self.cells.len()
    }

    pub fn depth(&self) -> usize {
        self.depth
    }

    /// Hands `patch` every node inside `seen`, at the coarseness asked for.
    ///
    /// A cell no wider than `resolution` is reported whole, as a
    /// [`Patch::Cluster`] of a count, rather than opened; `resolution` of zero
    /// opens everything and every patch is a [`Patch::Nodes`].  Cells outside
    /// `seen` are not walked at all, which is the point of the whole structure.
    ///
    /// Patches may name nodes just outside `seen`: a leaf is reported entire
    /// once it is reached, since testing sixteen points costs more than drawing
    /// the few that fall off the edge of a window that is clipping them anyway.
    pub fn visit(
        &self,
        seen: Rect,
        resolution: f64,
        patch: &mut impl FnMut(Patch<'_>),
    ) {
        if self.cells.is_empty() {
            return;
        }

        let mut stack: Vec<u32> = vec![0];
        while let Some(at) = stack.pop() {
            let cell = self.cells[at as usize];
            if cell.len == 0 || !cell.bounds.intersects(seen) {
                continue;
            }

            let range = cell.start as usize..(cell.start + cell.len) as usize;

            if cell.children == NO_CHILDREN {
                patch(Patch::Nodes(&self.order[range]));
            } else if cell.bounds.width() <= resolution {
                patch(Patch::Cluster { bounds: cell.bounds, count: cell.len });
            } else {
                for q in 0..4 {
                    stack.push(cell.children + q);
                }
            }
        }
    }

    /// The indexed node nearest `(x, y)`, if one is within `within` of it.
    ///
    /// What a click asks: the walk is the same one, opened all the way, over the
    /// square the tolerance describes.
    pub fn nearest(&self, points: &[[f64; 2]], x: f64, y: f64, within: f64) -> Option<u32> {
        let mut best: Option<(f64, u32)> = None;
        let limit = within * within;

        self.visit(Rect::square(x, y, 2.0 * within), 0.0, &mut |patch| {
            let Patch::Nodes(indices) = patch else { return };
            for &i in indices {
                let p = points[i as usize];
                let d = (p[0] - x) * (p[0] - x) + (p[1] - y) * (p[1] - y);
                if d <= limit && best.is_none_or(|(b, _)| d < b) {
                    best = Some((d, i));
                }
            }
        });

        best.map(|(_, i)| i)
    }
}

/// Which quarter of a cell a point falls in: bit 0 right of centre, bit 1 below.
///
/// A point exactly on a split goes right, or down, so that every point lands in
/// exactly one child.  Queries are the other way round — [`Rect::intersects`]
/// counts a touch — so nothing on a boundary is ever missed by a search.
fn quadrant(p: [f64; 2], cx: f64, cy: f64) -> usize {
    usize::from(p[0] >= cx) | (usize::from(p[1] >= cy) << 1)
}

/// The `q`th quarter of `bounds`, split at its centre.
fn quarter(bounds: Rect, cx: f64, cy: f64, q: usize) -> Rect {
    let (min_x, max_x) = if q & 1 == 0 { (bounds.min_x, cx) } else { (cx, bounds.max_x) };
    let (min_y, max_y) = if q & 2 == 0 { (bounds.min_y, cy) } else { (cy, bounds.max_y) };
    Rect::new(min_x, min_y, max_x, max_y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A grid, which is the shape a drawing of a tree roughly is: many nodes,
    /// none on top of another.
    fn grid(side: usize) -> Vec<[f64; 2]> {
        (0..side * side)
            .map(|i| [(i % side) as f64, (i / side) as f64])
            .collect()
    }

    /// Every point named by a walk, whatever kind of patch it came in.
    fn nodes_in(tree: &Quadtree, seen: Rect, resolution: f64) -> (Vec<u32>, u32) {
        let mut named = Vec::new();
        let mut counted = 0;
        tree.visit(seen, resolution, &mut |patch| match patch {
            Patch::Nodes(indices) => {
                named.extend_from_slice(indices);
                counted += indices.len() as u32;
            }
            Patch::Cluster { count, .. } => counted += count,
        });
        named.sort_unstable();
        (named, counted)
    }

    /// Nothing to index is not an error, and answers nothing.
    #[test]
    fn a_drawing_of_no_nodes() {
        let tree = Quadtree::over(&[]);
        assert!(tree.is_empty());
        assert!(tree.bounds().is_empty());
        assert_eq!(nodes_in(&tree, Rect::new(-1e9, -1e9, 1e9, 1e9), 0.0).0, Vec::<u32>::new());
        assert_eq!(tree.nearest(&[], 0.0, 0.0, 10.0), None);
    }

    /// Fewer nodes than a cell holds is one cell, never split.
    #[test]
    fn a_drawing_that_fits_in_the_root() {
        let points = grid(3);
        let tree = Quadtree::over(&points);
        assert_eq!(tree.cells(), 1);
        assert_eq!(tree.depth(), 0);
        assert_eq!(tree.len(), 9);
    }

    /// Every node is reachable, exactly once, and the index says which.
    #[test]
    fn the_whole_drawing_comes_back_once() {
        let points = grid(40);
        let tree = Quadtree::over(&points);
        assert!(tree.cells() > 1, "1600 nodes do not fit in one cell");

        let (named, counted) = nodes_in(&tree, tree.bounds(), 0.0);
        assert_eq!(counted as usize, points.len());
        assert_eq!(named, (0..points.len() as u32).collect::<Vec<_>>());
    }

    /// The walk answers with the nodes in the window, and does not answer with
    /// the rest of the drawing.
    #[test]
    fn a_window_gets_what_is_in_it() {
        let points = grid(40);
        let tree = Quadtree::over(&points);

        let seen = Rect::new(10.0, 10.0, 12.0, 12.0);
        let (named, _) = nodes_in(&tree, seen, 0.0);

        // Everything asked for is there...
        for x in 10..=12 {
            for y in 10..=12 {
                let i = (y * 40 + x) as u32;
                assert!(named.contains(&i), "node at ({x}, {y}) went missing");
            }
        }
        // ...and the answer is a neighbourhood, not the drawing.  A leaf is
        // reported entire, so a little slack around the window is expected; two
        // thirds of 1 600 nodes is not slack.
        assert!(named.len() < 100, "{} nodes for a 3x3 window", named.len());
    }

    /// A window over nothing walks nothing.
    #[test]
    fn a_window_off_the_edge_is_empty() {
        let points = grid(20);
        let tree = Quadtree::over(&points);
        let (named, counted) = nodes_in(&tree, Rect::new(1e6, 1e6, 1e6 + 1.0, 1e6 + 1.0), 0.0);
        assert!(named.is_empty() && counted == 0);
    }

    /// Coarsening trades names for counts and keeps the total: a summarised
    /// drawing stands for every node of it.
    #[test]
    fn coarsening_keeps_the_count_and_drops_the_names() {
        let points = grid(64);
        let tree = Quadtree::over(&points);
        let whole = tree.bounds();

        let (fine, fine_total) = nodes_in(&tree, whole, 0.0);
        assert_eq!(fine.len(), points.len());
        assert_eq!(fine_total as usize, points.len());

        let (coarse, coarse_total) = nodes_in(&tree, whole, whole.width() / 4.0);
        assert_eq!(coarse_total as usize, points.len(), "no node is lost");
        assert!(coarse.len() < fine.len(), "and most are no longer named");
    }

    /// The reason the structure exists: a coarse walk costs what the *window*
    /// costs, not what the drawing costs.
    ///
    /// Two drawings of the same size, one sixteen times as crowded as the other.
    /// Walked at the same coarseness they take about the same number of patches,
    /// because the walk stops at cells of that size whatever is inside them —
    /// which is what a frame needs: sixteen times the nodes must not be sixteen
    /// times the work when they land on the same pixels.
    #[test]
    fn a_coarse_walk_costs_what_the_window_costs() {
        let sparse: Vec<[f64; 2]> = (0..64 * 64).map(|i| [(i % 64) as f64, (i / 64) as f64]).collect();
        let crowded: Vec<[f64; 2]> = (0..256 * 256)
            .map(|i| [(i % 256) as f64 / 4.0, (i / 256) as f64 / 4.0])
            .collect();

        let patches = |points: &[[f64; 2]]| {
            let tree = Quadtree::over(points);
            let mut n = 0;
            tree.visit(tree.bounds(), 5.0, &mut |_| n += 1);
            n
        };

        assert_eq!(crowded.len(), 16 * sparse.len());
        let (thin, thick) = (patches(&sparse), patches(&crowded));
        assert!(thick <= 2 * thin, "{thin} patches thin, {thick} thick");
    }

    /// A click lands on the node under it, or on nothing.
    #[test]
    fn a_click_finds_the_node_under_it() {
        let points = grid(30);
        let tree = Quadtree::over(&points);

        assert_eq!(tree.nearest(&points, 7.1, 12.2, 0.5), Some(12 * 30 + 7));
        // Between four nodes, the nearest of them.
        assert_eq!(tree.nearest(&points, 7.4, 12.4, 1.0), Some(12 * 30 + 7));
        // Nothing within the tolerance is nothing at all.
        assert_eq!(tree.nearest(&points, 7.5, 12.5, 0.1), None);
        assert_eq!(tree.nearest(&points, -50.0, -50.0, 5.0), None);
    }

    /// A chain — one node per level, all on one line — is the drawing this is
    /// really for, and it is the worst case for a structure that splits on both
    /// axes at once.  It still indexes, and it still answers.
    #[test]
    fn a_long_thin_drawing() {
        let points: Vec<[f64; 2]> = (0..20_000).map(|i| [i as f64, 0.0]).collect();
        let tree = Quadtree::over(&points);

        assert_eq!(tree.len(), points.len());
        assert!(tree.depth() <= MAX_DEPTH);

        let (named, _) = nodes_in(&tree, Rect::new(9_000.0, -1.0, 9_010.0, 1.0), 0.0);
        for i in 9_000..=9_010u32 {
            assert!(named.contains(&i));
        }
        assert!(named.len() < 200, "{} nodes for eleven levels", named.len());
    }

    /// Nodes on top of one another cannot be told apart by splitting, and the
    /// depth limit is what stops the build from trying forever.
    #[test]
    fn nodes_in_the_same_place_do_not_split_forever() {
        let points = vec![[3.0, 4.0]; 100];
        let tree = Quadtree::over(&points);

        assert_eq!(tree.depth(), MAX_DEPTH);
        let (_, counted) = nodes_in(&tree, Rect::new(0.0, 0.0, 10.0, 10.0), 0.0);
        assert_eq!(counted, 100, "all of them are still there");
    }
}
