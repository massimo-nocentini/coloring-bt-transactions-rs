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
//! # Why the transpose is a second argument
//!
//! A root is a node nothing points at, and a `RandomAccessGraph` can only be asked
//! what a node points *to*.  Answering "is this a source?" from the graph alone
//! means a pass over every arc to build the in-degrees; the transpose has them
//! already, as its out-degrees, and `webgraph` builds one with a single command.
//! So the transpose is asked instead: `transpose.outdegree(v) == 0` is the whole
//! test, and it costs one lookup per node rather than one per arc.
//!
//! Both graphs are taken as `impl RandomAccessGraph`, so anything with random
//! access to successors will do — the BvGraph loaded from disk is only what the
//! callers' `main` happens to supply.
//!
//! # The node the graph does not have
//!
//! A graph of transactions has many sources, and the layout algorithm lays out
//! one tree.  So when there is more than one source, a node that stands for
//! nothing is made the parent of all of them, purely to give the algorithm its
//! root.  It is marked `isdummy` and never drawn.
//!
//! It is also given **zero width and zero height**, which is what keeps it from
//! showing up in the drawing anyway: the depth coordinate of a child is the far
//! edge of its parent, so a root of zero width leaves the real sources at depth
//! 0 where they would have been, and a box of zero height takes no room along the
//! breadth axis for the layout to route around.  A one-by-one invisible root
//! would push the whole picture over by one column and open a gap its own size in
//! the middle of the fringe; this way the drawing is the same one a single-source
//! graph would have produced.
//!
//! When the graph has exactly one source there is no such node — that source is
//! the root, and it is drawn like any other.
//!
//! # From a graph to a tree
//!
//! Nothing says the input is a tree.  A node reachable along two different paths
//! has two parents, which the algorithm has no way to draw, so what is laid out
//! is a *spanning* forest: a breadth-first walk from each source in turn, where
//! the first arc to reach a node is the one that becomes its edge and every later
//! arc into it is dropped.  How many were dropped is in [`Forest::dropped_arcs`],
//! and the callers report it on stderr, because a picture that quietly stands for
//! two thirds of the arcs is a lie the file itself cannot tell you about.
//!
//! Nodes on a cycle are reached by no source at all.  Rather than drop them —
//! they are nodes, and leaving them out would make the drawing silently smaller
//! than the graph — each one left over after the sources are exhausted is
//! promoted to a root of its own, in node order, and its walk runs like any
//! other.  That count is reported too.
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

use non_layered_tidy_trees::{layout, Arena, LayoutInput, NodeId};
use webgraph::prelude::RandomAccessGraph;

/// The side of a node's box, and so the unit the whole drawing is measured in.
pub const DIAMETER: f64 = 1.0;

/// Separation kept between neighbouring sibling subtrees.
pub const SUBTREE_MARGIN: f64 = 1.0;

/// Stack for the thread the layout runs on.
///
/// The three walks of the algorithm recurse once per level, and a graph of
/// transactions is deep — the crate's own depth test gives a 10 000-level chain a
/// 64 MiB stack.  This is virtual address space and only the pages actually
/// touched are ever committed, so a shallow tree pays nothing for the headroom.
const LAYOUT_STACK: usize = 1 << 30;

/// What the walk over the graph produced, beyond the arena itself.
pub struct Forest {
    pub root: NodeId,
    /// Sources of the graph — nodes the transpose says nothing points at.
    pub sources: usize,
    /// Nodes no source could reach, each promoted to a root of its own.
    pub promoted: usize,
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
            "{} nodes, {} sources{}{}",
            nodes,
            self.sources,
            if self.promoted > 0 {
                format!(", {} node(s) on cycles promoted to roots", self.promoted)
            } else {
                String::new()
            },
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
/// `transpose` is consulted only for `outdegree`, which is `graph`'s in-degree
/// and so the test for a source.
pub fn build<G: RandomAccessGraph, T: RandomAccessGraph>(
    graph: &G,
    transpose: &T,
    arena: &mut Arena,
) -> Result<Forest, String> {
    let n = graph.num_nodes();

    if n != transpose.num_nodes() {
        return Err(format!(
            "the graph has {n} nodes and the transpose {}; they are not a pair",
            transpose.num_nodes()
        ));
    }

    if n == 0 {
        return Err("the graph has no nodes to draw".to_string());
    }

    // `None` until the walk reaches a node, which is also what says whether it has
    // been visited: the arena node is created exactly when the first arc arrives.
    let mut ids: Vec<Option<NodeId>> = vec![None; n];

    let mut sources = 0;
    let mut promoted = 0;
    let mut dropped_arcs = 0u64;
    let mut roots: Vec<NodeId> = Vec::new();

    // Breadth first rather than depth first so that no walk of the input can
    // outgrow the stack — the drawing itself may be 10^6 levels deep, and this
    // queue holds one frontier instead of one frame per level.
    let mut queue: Vec<usize> = Vec::new();

    let grow = |arena: &mut Arena,
                    ids: &mut Vec<Option<NodeId>>,
                    queue: &mut Vec<usize>,
                    dropped: &mut u64,
                    root: usize| {
        let id = arena.add_node(root + 1, DIAMETER, DIAMETER, SUBTREE_MARGIN, false);
        ids[root] = Some(id);
        queue.clear();
        queue.push(root);

        let mut head = 0;
        while head < queue.len() {
            let v = queue[head];
            head += 1;
            let parent = ids[v].expect("queued nodes have been given an arena node");

            for w in graph.successors(v) {
                if ids[w].is_some() {
                    // Already has a parent — a second one is what a tree cannot
                    // hold, so this arc is not drawn.
                    *dropped += 1;
                    continue;
                }
                let child = arena.add_node(w + 1, DIAMETER, DIAMETER, SUBTREE_MARGIN, false);
                ids[w] = Some(child);
                arena.push_child(parent, child);
                queue.push(w);
            }
        }
        id
    };

    for v in 0..n {
        if transpose.outdegree(v) != 0 {
            continue;
        }
        sources += 1;
        if ids[v].is_some() {
            // A source reached from an earlier source: the graph has an arc into
            // it after all, and it is already in the forest under that parent.
            continue;
        }
        roots.push(grow(arena, &mut ids, &mut queue, &mut dropped_arcs, v));
    }

    // Whatever is left lies on a cycle, where every node has a parent and none has
    // one outside the cycle.  Each becomes a root, so that nothing goes undrawn.
    for v in 0..n {
        if ids[v].is_none() {
            promoted += 1;
            roots.push(grow(arena, &mut ids, &mut queue, &mut dropped_arcs, v));
        }
    }

    let synthetic_root = roots.len() > 1;

    let root = if synthetic_root {
        // Zero by zero, so that it occupies neither a column of depth nor a slot
        // of breadth: the real sources land where they would have landed on their
        // own.  `idx` 0 is outside the one-based labels the real nodes carry.
        let r = arena.add_node(0, 0.0, 0.0, 0.0, true);
        arena.set_children(r, &roots);
        r
    } else {
        roots[0]
    };

    Ok(Forest {
        root,
        sources,
        promoted,
        dropped_arcs,
        synthetic_root,
    })
}

/// Places every node of `arena`, depth running left to right.
///
/// The layout is what the module's last section describes; this is only where it
/// is asked for, and it is asked for on a thread of its own because of
/// [`LAYOUT_STACK`].  The arena goes over and comes back rather than being
/// borrowed, so nothing here is shared.
pub fn lay_out(mut arena: Arena, root: NodeId) -> Result<Arena, String> {
    std::thread::Builder::new()
        .stack_size(LAYOUT_STACK)
        .spawn(move || {
            let mut input = LayoutInput::new(root);
            // Depth left to right, which is what makes the drawing horizontal.
            input.vertically = false;
            layout(&mut arena, &input);
            arena
        })
        .map_err(|e| format!("could not start the layout thread: {e}"))?
        .join()
        .map_err(|_| "the layout thread panicked".to_string())
}

/// The two graphs a run is given, built by hand so that the shape under test is
/// the one written in the test rather than one a fixture file happens to hold.
///
/// Here rather than in either binary's tests because both draw the same forests.
#[cfg(test)]
pub fn pair(
    n: usize,
    arcs: &[(usize, usize)],
) -> (webgraph::prelude::VecGraph, webgraph::prelude::VecGraph) {
    use webgraph::prelude::VecGraph;
    let mut g = VecGraph::empty(n);
    let mut t = VecGraph::empty(n);
    for &(u, v) in arcs {
        g.add_arc(u, v);
        t.add_arc(v, u);
    }
    (g, t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use webgraph::prelude::VecGraph;

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
        let (g, t) = pair(
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
        let forest = build(&g, &t, &mut arena).unwrap();

        assert_eq!(forest.sources, 2, "0 and 6 are pointed at by nothing");
        assert_eq!(forest.promoted, 1, "the 8-9 cycle contributes one root");
        assert_eq!(forest.dropped_arcs, 2, "7->5 and 9->8");
        assert!(forest.synthetic_root, "three roots need a node to hang from");

        let root = forest.root;
        let arena = lay_out(arena, root).unwrap();

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

    /// One source and no added root: that source is the root and is drawn.
    #[test]
    fn a_single_source_is_the_root_itself() {
        let (g, t) = pair(3, &[(0, 1), (0, 2)]);

        let mut arena = Arena::new();
        let forest = build(&g, &t, &mut arena).unwrap();

        assert_eq!(forest.sources, 1);
        assert!(!forest.synthetic_root);
        assert!(!arena[forest.root].isdummy, "the root stands for node 0");

        let arena = lay_out(arena, forest.root).unwrap();

        assert_eq!(
            drawing(&arena),
            [(0, 0.0, 1.0, false), (1, 1.0, 0.0, true), (2, 1.0, 2.0, true)]
        );
    }

    /// A graph that is one cycle has no source at all, and still gets drawn.
    #[test]
    fn a_graph_of_only_cycles_still_has_roots() {
        let (g, t) = pair(3, &[(0, 1), (1, 2), (2, 0)]);

        let mut arena = Arena::new();
        let forest = build(&g, &t, &mut arena).unwrap();

        assert_eq!(forest.sources, 0);
        assert_eq!(forest.promoted, 1, "node 0 is promoted, and reaches the rest");
        assert!(!forest.synthetic_root, "one root needs no help");

        let arena = lay_out(arena, forest.root).unwrap();

        assert_eq!(
            drawing(&arena),
            [(0, 0.0, 0.0, false), (1, 1.0, 0.0, false), (2, 2.0, 0.0, true)]
        );
    }

    #[test]
    fn a_graph_and_a_transpose_of_different_sizes_are_refused() {
        let mut g = VecGraph::empty(3);
        g.add_arc(0, 1);
        let t = VecGraph::empty(2);

        let mut arena = Arena::new();
        assert!(build(&g, &t, &mut arena).is_err());
    }

    /// The summary is one line, and a second only when arcs went undrawn.
    #[test]
    fn what_a_run_reports() {
        let (g, t) = pair(2, &[]);
        let mut arena = Arena::new();
        let forest = build(&g, &t, &mut arena).unwrap();

        assert_eq!(
            forest.summary(2),
            "2 nodes, 2 sources, one added root standing for no node"
        );

        let (g, t) = pair(3, &[(0, 1), (0, 2), (1, 2)]);
        let mut arena = Arena::new();
        let forest = build(&g, &t, &mut arena).unwrap();

        assert_eq!(
            forest.summary(3),
            "3 nodes, 1 sources\n\
             1 arc(s) not drawn: they would have given a node a second parent"
        );
    }
}
