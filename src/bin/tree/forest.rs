//! # A webgraph, read as a tree
//!
//! The part of a drawing that is about the *graph* rather than about the ink:
//! turning a `BvGraph` into an arena of the non-layered tidy trees crate
//! (van der Ploeg 2014), and running the layout over it.  What the coordinates
//! it produces are then drawn *with* is the caller's business — `tree-svg`
//! makes them circles, `tree-bitmap` makes them pixels — and the two agree
//! about the picture because they agree about this file.
//!
//! It is a module rather than a library because this crate's `src/*.rs` belong
//! to the main binary; the two drawing binaries reach it by `#[path]` instead,
//! which is why it lives under `src/bin/` next to the only things that use it.
//!
//! # Where the roots come from
//!
//! A root is a node the walk has not already reached.  Node order is swept once
//! and every node still unvisited when its turn comes starts a walk of its own;
//! marking as we go is what makes that enough, since a node reached from an
//! earlier root is already in the forest and is passed over.
//!
//! That is *nearly* the graph's sources, and it costs nothing.  Asking a
//! `RandomAccessGraph` "does anything point at this?" means a pass over every arc
//! to build the in-degrees, or a second graph — the transpose, whose out-degrees
//! are the in-degrees — loaded and carried beside the first.  Sweeping asks
//! nothing the walk was not going to do anyway, and every true source is still a
//! root: nothing points at one, so no walk can reach it.
//!
//! Where the two differ is a node that comes *before* its parent in node order:
//! given `1 -> 0`, the sweep makes 0 a root of its own and drops the arc, where
//! in-degrees would have rooted both at 1.  On a graph whose node order follows
//! its arcs — transactions numbered as they are spent, which is the input this
//! draws — the two agree.
//!
//! # The node the graph does not have
//!
//! A graph of transactions has many roots, and the layout algorithm lays out one
//! tree.  So when there is more than one root, a node that stands for nothing is
//! made the parent of all of them, purely to give the algorithm its root.  It is
//! marked `isdummy` and never drawn.
//!
//! It is also given **zero width and zero height**, which is what keeps it from
//! showing up in the drawing anyway: the depth coordinate of a child is the far
//! edge of its parent, so a root of zero width leaves the real roots at depth
//! 0 where they would have been, and a box of zero height takes no room along the
//! breadth axis for the layout to route around.  A one-by-one invisible root
//! would push the whole picture over by one column and open a gap its own size in
//! the middle of the fringe; this way the drawing is the same one a single-rooted
//! graph would have produced.
//!
//! When the sweep finds exactly one root there is no such node — that node is the
//! root, and it is drawn like any other.
//!
//! # From a graph to a tree
//!
//! Nothing says the input is a tree.  A node reachable along two different paths
//! has two parents, which the algorithm has no way to draw, so what is laid out
//! is a *spanning* forest: a breadth-first walk from each root in turn, where
//! the first arc to reach a node is the one that becomes its edge and every later
//! arc into it is dropped.  How many were dropped is in [`Forest::dropped_arcs`],
//! and the callers report it on stderr, because a picture that quietly stands for
//! two thirds of the arcs is a lie the file itself cannot tell you about.
//!
//! Nodes on a cycle need no special handling: no walk from outside reaches one, so
//! the first of them in node order is still unvisited when the sweep arrives, and
//! becomes a root like any other node the sweep finds.  Nothing goes undrawn, and
//! the arc that closes the cycle is dropped like any other second parent.
//!
//! # The shape the layout is asked for
//!
//! - `vertically: false`, so depth runs left to right and the breadth axis is `y`.
//! - Every real node is a [`DIAMETER`] by [`DIAMETER`] box, so the drawing's
//!   units are nodes.
//! - **No margin along the depth axis** is not a setting: the algorithm puts a
//!   child's near edge exactly on its parent's far edge, so with unit boxes level
//!   `d` sits at `x = d`.
//! - [`SUBTREE_MARGIN`] on every node, which is the separation the algorithm keeps
//!   between a node and the sibling subtree to its right — one clear node between
//!   neighbouring subtrees, at every level.
//!
//! Two consequences the drawing code leans on: a node's `x` *is* its depth, and
//! two nodes at the same depth have centres at least `DIAMETER + SUBTREE_MARGIN`
//! apart along the breadth axis.  Coordinates come back as top-left corners.
//!
//! # Why the flat entry point
//!
//! The crate offers the same layout twice: [`non_layered_tidy_trees::layout`],
//! whose three walks recurse once per level, and
//! [`non_layered_tidy_trees::flat::layout_flat`], which mirrors the tree into
//! arrays and sweeps over them.  They compute the same coordinates — the crate
//! checks that bit for bit — and the difference that matters here is the stack:
//! a chain of transactions can be a million nodes long, and a walk that recurses
//! once per level overflows on it, where a sweep has no such ceiling.  So the
//! sweeps are what this module asks for.
//!
//! The one thing the flat path cannot do is report itself node by node, because
//! it visits independent subtrees in a different order; it takes no callbacks.
//! Nothing here wants any.

use non_layered_tidy_trees::{flat::layout_flat, Arena, LayoutInput, NodeId};
use webgraph::prelude::RandomAccessGraph;

/// The side of a node's box, and so the unit the whole drawing is measured in.
pub const DIAMETER: f64 = 1.0;

/// Separation kept between neighbouring sibling subtrees.
pub const SUBTREE_MARGIN: f64 = 1.0;

/// What the walk over the graph produced, beyond the arena itself.
pub struct Forest {
    pub root: NodeId,
    /// Nodes that started a walk of their own: those no earlier walk had reached.
    pub roots: usize,
    /// Arcs that would have given a node a second parent, and were dropped.
    pub dropped_arcs: u64,
    /// Whether a root standing for no node of the graph was added.
    pub synthetic_root: bool,
}

impl Forest {
    /// What a run has to say about the graph it just read, for stderr.
    ///
    /// One line about the shape of the forest, and a second only when arcs went
    /// undrawn — a picture that stands for fewer arcs than the graph has is
    /// something the file itself cannot tell you about.
    pub fn summary(&self, nodes: usize) -> String {
        let mut out = format!(
            "{} nodes, {} root(s){}",
            nodes,
            self.roots,
            if self.synthetic_root {
                ", one added root standing for no node"
            } else {
                ""
            }
        );
        if self.dropped_arcs > 0 {
            out.push_str(&format!(
                "\n{} arc(s) not drawn: they would have given a node a second parent",
                self.dropped_arcs
            ));
        }
        out
    }
}

/// Builds the spanning forest of `graph` into an arena, rooted at a single node.
///
/// One sweep over node order: whatever is unvisited when its turn comes is a
/// root, which is why nothing but `graph` is needed to find them.
pub fn build<G: RandomAccessGraph>(graph: &G, arena: &mut Arena) -> Result<Forest, String> {
    let n = graph.num_nodes();

    if n == 0 {
        return Err("the graph has no nodes to draw".to_string());
    }

    // `None` until the walk reaches a node, which is also what says whether it has
    // been visited: the arena node is created exactly when the first arc arrives.
    let mut ids: Vec<Option<NodeId>> = vec![None; n];

    let mut dropped_arcs = 0u64;
    let mut roots: Vec<NodeId> = Vec::new();

    // Breadth first rather than depth first so that no walk of the input can
    // outgrow the stack — the drawing itself may be 10^6 levels deep, and this
    // queue holds one frontier instead of one frame per level.
    let mut queue: Vec<usize> = Vec::new();

    for v in 0..n {
        if ids[v].is_some() {
            // Reached from an earlier root, and so already in the forest under a
            // parent of its own: not a root.
            continue;
        }

        let id = arena.add_node(v + 1, DIAMETER, DIAMETER, SUBTREE_MARGIN, false);
        ids[v] = Some(id);
        roots.push(id);

        queue.clear();
        queue.push(v);

        let mut head = 0;
        while head < queue.len() {
            let u = queue[head];
            head += 1;
            let parent = ids[u].expect("queued nodes have been given an arena node");

            for w in graph.successors(u) {
                if ids[w].is_some() {
                    // Already has a parent — a second one is what a tree cannot
                    // hold, so this arc is not drawn.
                    dropped_arcs += 1;
                    continue;
                }
                let child = arena.add_node(w + 1, DIAMETER, DIAMETER, SUBTREE_MARGIN, false);
                ids[w] = Some(child);
                arena.push_child(parent, child);
                queue.push(w);
            }
        }
    }

    let synthetic_root = roots.len() > 1;

    let root = if synthetic_root {
        // Zero by zero, so that it occupies neither a column of depth nor a slot
        // of breadth: the real roots land where they would have landed on their
        // own.  `idx` 0 is outside the one-based labels the real nodes carry.
        let r = arena.add_node(0, 0.0, 0.0, 0.0, true);
        arena.set_children(r, &roots);
        r
    } else {
        roots[0]
    };

    Ok(Forest {
        root,
        roots: roots.len(),
        dropped_arcs,
        synthetic_root,
    })
}

/// Places every node of `arena`, depth running left to right.
///
/// The layout is what the module's last section describes; this is only where it
/// is asked for.  The arena goes over and comes back rather than being borrowed,
/// so a caller cannot look at it half laid out: the arena it had is gone, and the
/// one it gets back is finished.
pub fn lay_out(mut arena: Arena, root: NodeId) -> Arena {
    let mut input = LayoutInput::new(root);
    // Depth left to right, which is what makes the drawing horizontal.
    input.vertically = false;
    layout_flat(&mut arena, &input);
    arena
}

/// The graph a run is given, built by hand so that the shape under test is the
/// one written in the test rather than one a fixture file happens to hold.
///
/// Here rather than in either binary's tests because both draw the same forests.
#[cfg(test)]
pub fn graph_of(n: usize, arcs: &[(usize, usize)]) -> webgraph::prelude::VecGraph {
    use webgraph::prelude::VecGraph;
    let mut g = VecGraph::empty(n);
    for &(u, v) in arcs {
        g.add_arc(u, v);
    }
    g
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `(node, x, y, leaf)` per drawn node — `idx` is one-based, and the added
    /// root carries 0.
    fn drawing(arena: &Arena) -> Vec<(usize, f64, f64, bool)> {
        let mut out: Vec<_> = arena
            .iter()
            .filter(|n| !n.isdummy)
            .map(|n| (n.idx - 1, n.x, n.y, n.children().is_empty()))
            .collect();
        out.sort_by_key(|&(v, ..)| v);
        out
    }

    /// Two sources, an arc that would give a node a second parent, and a
    /// two-node cycle no source reaches.
    #[test]
    fn a_forest_with_everything_in_it() {
        let g = graph_of(
            10,
            &[
                (0, 1),
                (0, 2),
                (1, 3),
                (1, 4),
                (2, 5),
                (6, 7),
                // 5 already belongs to 2's subtree by the time 7 offers itself.
                (7, 5),
                (8, 9),
                // Closes the cycle, and 8 already has 9 as a parent.
                (9, 8),
            ],
        );

        let mut arena = Arena::new();
        let forest = build(&g, &mut arena).unwrap();

        // 0 and 6 are pointed at by nothing; 8 lies on a cycle and is the first
        // of it the sweep reaches.
        assert_eq!(forest.roots, 3);
        assert_eq!(forest.dropped_arcs, 2, "7->5 and 9->8");
        assert!(forest.synthetic_root, "three roots need a node to hang from");

        let root = forest.root;
        let arena = lay_out(arena, root);

        // The depth coordinate is the level, exactly: unit boxes and a root of
        // zero width leave level d at x = d, which is what "no margin between
        // parent and children" means once the nodes are drawn.
        let expected = [
            //  node    x    y   leaf
            (0usize, 0.0, 2.5, false),
            (1, 1.0, 1.0, false),
            (2, 1.0, 4.0, false),
            (3, 2.0, 0.0, true),
            (4, 2.0, 2.0, true),
            (5, 2.0, 4.0, true),
            (6, 0.0, 6.0, false),
            (7, 1.0, 6.0, true),
            (8, 0.0, 8.0, false),
            (9, 1.0, 8.0, true),
        ];
        assert_eq!(drawing(&arena), expected);

        assert!(arena[root].isdummy, "the root stands for no node");
    }

    /// One root and no added node: that root is the whole tree's root and is
    /// drawn.
    #[test]
    fn a_single_root_is_the_root_itself() {
        let g = graph_of(3, &[(0, 1), (0, 2)]);

        let mut arena = Arena::new();
        let forest = build(&g, &mut arena).unwrap();

        assert_eq!(forest.roots, 1);
        assert!(!forest.synthetic_root);
        assert!(!arena[forest.root].isdummy, "the root stands for node 0");

        let arena = lay_out(arena, forest.root);

        assert_eq!(
            drawing(&arena),
            [(0, 0.0, 1.0, false), (1, 1.0, 0.0, true), (2, 1.0, 2.0, true)]
        );
    }

    /// A graph that is one cycle has no source at all, and still gets drawn: the
    /// first node of it in node order is unvisited when the sweep arrives.
    #[test]
    fn a_graph_of_only_cycles_still_has_roots() {
        let g = graph_of(3, &[(0, 1), (1, 2), (2, 0)]);

        let mut arena = Arena::new();
        let forest = build(&g, &mut arena).unwrap();

        assert_eq!(forest.roots, 1, "node 0 starts the walk, and reaches the rest");
        assert_eq!(forest.dropped_arcs, 1, "2->0 closes the cycle");
        assert!(!forest.synthetic_root, "one root needs no help");

        let arena = lay_out(arena, forest.root);

        assert_eq!(
            drawing(&arena),
            [(0, 0.0, 0.0, false), (1, 1.0, 0.0, false), (2, 2.0, 0.0, true)]
        );
    }

    /// What the sweep costs, in the one place it differs from the in-degrees: a
    /// node ahead of its parent in node order is a root, and the arc into it goes
    /// undrawn.  See the module's first section.
    #[test]
    fn a_node_before_its_parent_is_a_root_of_its_own() {
        let g = graph_of(2, &[(1, 0)]);

        let mut arena = Arena::new();
        let forest = build(&g, &mut arena).unwrap();

        assert_eq!(forest.roots, 2, "0 is swept up before 1 can claim it");
        assert_eq!(forest.dropped_arcs, 1, "1->0");
        assert!(forest.synthetic_root);

        let arena = lay_out(arena, forest.root);

        // Two roots side by side, a clear node apart, rather than a chain.
        assert_eq!(drawing(&arena), [(0, 0.0, 0.0, true), (1, 0.0, 2.0, true)]);
    }

    #[test]
    fn a_graph_without_nodes_is_refused() {
        let g = graph_of(0, &[]);

        let mut arena = Arena::new();
        assert!(build(&g, &mut arena).is_err());
    }

    /// The summary is one line, and a second only when arcs went undrawn.
    #[test]
    fn what_a_run_reports() {
        let g = graph_of(2, &[]);
        let mut arena = Arena::new();
        let forest = build(&g, &mut arena).unwrap();

        assert_eq!(
            forest.summary(2),
            "2 nodes, 2 root(s), one added root standing for no node"
        );

        let g = graph_of(3, &[(0, 1), (0, 2), (1, 2)]);
        let mut arena = Arena::new();
        let forest = build(&g, &mut arena).unwrap();

        assert_eq!(
            forest.summary(3),
            "3 nodes, 1 root(s)\n\
             1 arc(s) not drawn: they would have given a node a second parent"
        );
    }

    /// A chain of 200 000 nodes, laid out on an ordinary stack.
    ///
    /// The reason the module asks the crate for its flat entry point: this is
    /// 200 000 levels, the recursive walks take one frame per level, and this very
    /// test aborts with a stack overflow when `lay_out` calls them instead.  A
    /// chain is what a run of transactions spending each other's output *is*, so
    /// the depth is the shape of the input rather than a pathological case.
    #[test]
    fn a_chain_deeper_than_any_stack() {
        let n = 200_000;
        let arcs: Vec<(usize, usize)> = (0..n - 1).map(|v| (v, v + 1)).collect();
        let g = graph_of(n, &arcs);

        let mut arena = Arena::with_capacity(n);
        let forest = build(&g, &mut arena).unwrap();

        assert_eq!(forest.roots, 1, "node 0 reaches every other");
        assert!(!forest.synthetic_root);

        let arena = lay_out(arena, forest.root);

        // One node per level and nothing to sit beside: the chain is a straight
        // line along the depth axis, from the origin.
        assert_eq!(arena.len(), n);
        assert_eq!((arena[forest.root].x, arena[forest.root].y), (0.0, 0.0));

        let deepest = arena.iter().map(|node| node.x).fold(0.0f64, f64::max);
        assert_eq!(deepest, (n - 1) as f64, "level d sits at x = d");
        assert!(
            arena.iter().all(|node| node.y == 0.0),
            "a chain never has to step aside"
        );
    }
}
