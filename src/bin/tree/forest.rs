//! # A webgraph, read as a tree
//!
//! The part of a drawing that is about the *graph* rather than about the ink:
//! turning a `BvGraph` into an arena of the non-layered tidy trees crate
//! (van der Ploeg 2014), and running the layout over it.  What the coordinates
//! it produces are then drawn *with* is the caller's business — `tree-jp2`
//! makes them pixels, `tree-view` makes them circles in a window — and the two
//! agree about the picture because they agree about this file.
//!
//! It is a module rather than a library because this crate's `src/*.rs` belong
//! to the main binary; the drawing binaries reach it by `#[path]` instead,
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

// Shared by the drawing binaries the way `scene` and `camera` are, and like
// them compiled into each: whichever entry points a binary does not call read
// as dead there -- `tree-pdf` never sweeps the whole graph, `tree-jp2` may
// never prune -- so the lint goes rather than the surface.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

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

/// Where a walk from a chosen root is told to stop.
///
/// The whole graph is billions of nodes and a page is a few hundred points
/// across, so a drawing of *one node's subtree* is only as good as its cut.
/// Three independent scissors, each `None`/`MAX` by default:
///
/// - `depth`: levels drawn below the root, the root itself being level 0.  A
///   node on the last level is drawn, and what hangs under it is not.
/// - `max_nodes`: how many nodes the whole drawing may hold, roots included.
///   The walk is breadth first, so what a budget buys is the *nearest* part of
///   the subtree rather than one long arm of it.
/// - `fanout`: how many children a node may have drawn under it.  A hub with a
///   million successors is a fact worth stating and not worth a million arcs of
///   ink; the first and last of them in successor order stand for the rest,
///   half the allowance each.  Both ends rather than one head, because a
///   block's outputs are a contiguous id range and what hangs off its *last*
///   output is a different thing from what hangs off its first — the long
///   2001-output fans of this graph chain into each other through their final
///   output, and a head-only cut would draw every one of them as a dead star.
///
/// Whatever gets cut, the node whose successors were left out is reported in
/// [`Sampled::truncated`], so a drawing can say where it stops short rather
/// than passing a pruned frontier off as leaves.
pub struct Prune {
    pub depth: Option<usize>,
    pub max_nodes: usize,
    pub fanout: Option<usize>,
    /// When set, only these nodes are expanded; everything else is drawn and
    /// left unexpanded (and reported cut when that hides successors).  This is
    /// the shape of a *chain*: a caller that knows the spine of a peeling
    /// chain --- knowledge the graph alone does not carry, since telling the
    /// change output from the payment takes amounts --- names it, and the
    /// drawing becomes the spine with every leg drawn one node deep, instead
    /// of a walk that chases each leg into the open economy and drowns.
    pub expand: Option<HashSet<usize>>,
}

impl Default for Prune {
    fn default() -> Self {
        Prune { depth: None, max_nodes: usize::MAX, fanout: None, expand: None }
    }
}

/// What a rooted walk produced, beyond the arena itself.
pub struct Sampled {
    pub root: NodeId,
    /// Whether a root standing for no node was added over several chosen roots.
    pub synthetic_root: bool,
    /// Nodes drawn, roots included.
    pub nodes: usize,
    /// Arcs into nodes already drawn: the DAG's second parents, dropped exactly
    /// as [`build`] drops them.
    pub dropped_arcs: u64,
    /// Graph ids of drawn nodes with successors the cut left out — by depth, by
    /// fanout, or by the node budget.  They are drawn like any node, but they
    /// are *not* leaves of the graph, and a caller can ink them apart.
    pub truncated: HashSet<usize>,
}

impl Sampled {
    /// What a run has to say about the cut, for stderr.
    pub fn summary(&self) -> String {
        let mut out = format!("{} node(s) drawn", self.nodes);
        if self.dropped_arcs > 0 {
            out.push_str(&format!(
                ", {} arc(s) into already-drawn nodes not drawn",
                self.dropped_arcs
            ));
        }
        if !self.truncated.is_empty() {
            out.push_str(&format!(
                ", {} node(s) cut before their successors",
                self.truncated.len()
            ));
        }
        out
    }
}

/// Builds the subtree hanging under each of `roots` into an arena, pruned by
/// `prune`, rooted at a single node.
///
/// The same breadth-first walk as [`build`], started at the nodes the caller
/// names instead of swept over node order: the first arc to reach a node is
/// its edge, every later arc into it is dropped and counted.  On the transpose
/// graph the same call draws the *ancestors* of a node — where its value came
/// from — since the transpose's successors are the graph's predecessors.
///
/// Visited nodes live in a map rather than in a vector a slot per node,
/// because a pruned walk touches a bounded neighbourhood of a graph whose node
/// count may be nine of memory's ten figures.
///
/// A root already drawn under an earlier root is passed over — it is in the
/// picture, under the parent that reached it first.  A root the node budget is
/// too spent to place is an error rather than a silently smaller picture: the
/// caller asked for that subtree by name.
pub fn build_rooted<G: RandomAccessGraph>(
    graph: &G,
    arena: &mut Arena,
    roots: &[usize],
    prune: &Prune,
) -> Result<Sampled, String> {
    let n = graph.num_nodes();

    if roots.is_empty() {
        return Err("no root to draw from".to_string());
    }
    if prune.max_nodes == 0 {
        return Err("a budget of 0 nodes draws nothing".to_string());
    }
    for &r in roots {
        if r >= n {
            return Err(format!("node {r} is not in a graph of {n} nodes"));
        }
    }

    let mut ids: HashMap<usize, NodeId> = HashMap::new();
    let mut dropped_arcs = 0u64;
    let mut truncated: HashSet<usize> = HashSet::new();
    let mut placed_roots: Vec<NodeId> = Vec::new();

    // (node, its level below its root); breadth first for the reason `build`
    // is, and for what it makes of a budget — see [`Prune::max_nodes`].
    let mut queue: Vec<(usize, usize)> = Vec::new();

    for &r in roots {
        if ids.contains_key(&r) {
            // Reached from an earlier root: already in the picture.
            continue;
        }
        if ids.len() >= prune.max_nodes {
            return Err(format!(
                "the node budget was spent before root {r} was reached; raise --max-nodes"
            ));
        }

        let id = arena.add_node(r + 1, DIAMETER, DIAMETER, SUBTREE_MARGIN, false);
        ids.insert(r, id);
        placed_roots.push(id);

        queue.clear();
        queue.push((r, 0));

        let mut head = 0;
        while head < queue.len() {
            let (u, level) = queue[head];
            head += 1;
            let parent = ids[&u];

            if prune.depth.is_some_and(|limit| level >= limit)
                || prune.expand.as_ref().is_some_and(|spine| !spine.contains(&u))
            {
                // Drawn, not expanded.  Whether anything was cut is one
                // outdegree probe, so the report never calls a true leaf cut.
                if graph.outdegree(u) > 0 {
                    truncated.insert(u);
                }
                continue;
            }

            // The successors that are not already in the picture, gathered so
            // that the fanout can be taken off both ends of them.
            let mut fresh: Vec<usize> = Vec::new();
            for w in graph.successors(u) {
                if ids.contains_key(&w) {
                    dropped_arcs += 1;
                } else {
                    fresh.push(w);
                }
            }

            if let Some(k) = prune.fanout {
                if fresh.len() > k {
                    // The first and last of the fan stand for the rest — see
                    // [`Prune::fanout`] for why not the head alone.
                    truncated.insert(u);
                    let head = k.div_ceil(2);
                    fresh.drain(head..fresh.len() - (k - head));
                }
            }

            for w in fresh {
                if ids.len() >= prune.max_nodes {
                    truncated.insert(u);
                    break;
                }
                let child = arena.add_node(w + 1, DIAMETER, DIAMETER, SUBTREE_MARGIN, false);
                ids.insert(w, child);
                arena.push_child(parent, child);
                queue.push((w, level + 1));
            }
        }
    }

    let synthetic_root = placed_roots.len() > 1;
    let root = if synthetic_root {
        // Zero by zero, exactly as in [`build`]: the chosen roots land where
        // they would have landed on their own.
        let r = arena.add_node(0, 0.0, 0.0, 0.0, true);
        arena.set_children(r, &placed_roots);
        r
    } else {
        placed_roots[0]
    };

    Ok(Sampled {
        root,
        synthetic_root,
        nodes: ids.len(),
        dropped_arcs,
        truncated,
    })
}

/// Places every node of `arena`, depth running left to right.
///
/// The layout is what the module's last section describes; this is only where it
/// is asked for.  The arena goes over and comes back rather than being borrowed,
/// so a caller cannot look at it half laid out: the arena it had is gone, and the
/// one it gets back is finished.
pub fn lay_out(arena: Arena, root: NodeId) -> Arena {
    lay_out_oriented(arena, root, false)
}

/// [`lay_out`], with the axes the caller's to choose: depth runs left to right,
/// or down the page when `vertically` is set.
///
/// The choice matters once a drawing is a page rather than a window: a chain is
/// deep and narrow and reads left to right, while a hub's fan is one level deep
/// and thousands of siblings broad, and drawn horizontally it is a ribbon a few
/// points wide.  Turned on its side it is a figure.
pub fn lay_out_oriented(mut arena: Arena, root: NodeId, vertically: bool) -> Arena {
    let mut input = LayoutInput::new(root);
    input.vertically = vertically;
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

    /// The rooted walk draws the subtree the caller names and nothing else,
    /// counting the DAG's second parents exactly as the full sweep does.
    #[test]
    fn a_rooted_walk_draws_one_subtree() {
        //  0 -> 1 -> {3, 4}, 0 -> 2 -> 5, 1 -> 5 dropped as a second parent
        //  (2 comes first in successor order), 6 -> 7 untouched.
        let g = graph_of(8, &[(0, 1), (0, 2), (1, 3), (1, 4), (1, 5), (2, 5), (6, 7)]);

        let mut arena = Arena::new();
        let sampled = build_rooted(&g, &mut arena, &[0], &Prune::default()).unwrap();

        assert_eq!(sampled.nodes, 6, "0..=5 and neither of 6, 7");
        assert_eq!(sampled.dropped_arcs, 1, "the second parent of 5");
        assert!(sampled.truncated.is_empty(), "nothing was cut");
        assert!(!sampled.synthetic_root, "one root needs no help");

        let arena = lay_out(arena, sampled.root);
        let drawn: Vec<usize> = drawing(&arena).iter().map(|&(v, ..)| v).collect();
        assert_eq!(drawn, [0, 1, 2, 3, 4, 5]);
    }

    /// The three scissors: each cuts where it says, and the node whose
    /// successors were left out is named rather than passed off as a leaf.
    #[test]
    fn the_cut_is_reported_not_hidden() {
        // A chain with a fan in the middle: 0 -> 1 -> {2, 3, 4}, 2 -> 5 -> 6.
        let g = graph_of(7, &[(0, 1), (1, 2), (1, 3), (1, 4), (2, 5), (5, 6)]);

        // Depth: level 2 is drawn, level 3 is not, and 2 is named as cut.
        let mut arena = Arena::new();
        let sampled = build_rooted(
            &g,
            &mut arena,
            &[0],
            &Prune { depth: Some(2), ..Prune::default() },
        )
        .unwrap();
        assert_eq!(sampled.nodes, 5, "0, 1 and the fan");
        assert_eq!(sampled.truncated, HashSet::from([2]), "3 and 4 are true leaves");

        // Fanout: two of the three children — the first and the last of the
        // fan, not the head alone — and the parent named.
        let mut arena = Arena::new();
        let sampled = build_rooted(
            &g,
            &mut arena,
            &[1],
            &Prune { fanout: Some(2), ..Prune::default() },
        )
        .unwrap();
        assert_eq!(sampled.nodes, 5, "1, then 2 and 4, then 2's chain");
        assert_eq!(sampled.truncated, HashSet::from([1]));

        let arena_holds: Vec<usize> = {
            let mut v: Vec<usize> =
                arena.iter().filter(|n| !n.isdummy).map(|n| n.idx - 1).collect();
            v.sort_unstable();
            v
        };
        assert_eq!(arena_holds, [1, 2, 4, 5, 6], "3 is the one the cut took");

        // Budget: breadth first, so what three nodes buy is the nearest three.
        let mut arena = Arena::new();
        let sampled = build_rooted(
            &g,
            &mut arena,
            &[0],
            &Prune { max_nodes: 3, ..Prune::default() },
        )
        .unwrap();
        assert_eq!(sampled.nodes, 3, "0, 1 and 1's first child");
        // Both 1 and 2 still had successors when the budget ran out, and both
        // say so.
        assert_eq!(sampled.truncated, HashSet::from([1, 2]));
    }

    /// A named spine is expanded and nothing else is: the drawing is the
    /// chain with its legs one node deep, each leg with hidden successors
    /// reported cut.
    #[test]
    fn a_spine_walks_alone() {
        // A peel chain 0 -> {1, 2} -> {3, 4} -> {5, 6}, where every leg (1, 3)
        // also spends onward and would drag the walk with it.
        let g = graph_of(9, &[(0, 1), (0, 2), (2, 3), (2, 4), (4, 5), (4, 6), (1, 7), (3, 8)]);

        let mut arena = Arena::new();
        let sampled = build_rooted(
            &g,
            &mut arena,
            &[0],
            &Prune { expand: Some(HashSet::from([0, 2, 4])), ..Prune::default() },
        )
        .unwrap();

        assert_eq!(sampled.nodes, 7, "the spine and its legs, not the legs' spends");
        assert_eq!(
            sampled.truncated,
            HashSet::from([1, 3]),
            "the legs that spend onward are reported cut; the sinks are not"
        );
    }

    /// Several roots hang from an added node; a root inside an earlier root's
    /// subtree is already in the picture and is passed over.
    #[test]
    fn several_roots_and_a_swallowed_one() {
        let g = graph_of(5, &[(0, 1), (2, 3)]);

        let mut arena = Arena::new();
        let sampled = build_rooted(&g, &mut arena, &[0, 1, 2], &Prune::default()).unwrap();

        assert_eq!(sampled.nodes, 4, "1 was reached under 0, and 4 is nobody's");
        assert!(sampled.synthetic_root, "0 and 2 need a node to hang from");
        assert!(arena[sampled.root].isdummy);
    }

    /// What the walk refuses: no roots, a root the graph does not have, and a
    /// budget spent before a named root is reached.
    #[test]
    fn what_a_rooted_walk_refuses() {
        let g = graph_of(3, &[(0, 1), (0, 2)]);

        let mut arena = Arena::new();
        assert!(build_rooted(&g, &mut arena, &[], &Prune::default()).is_err());

        let mut arena = Arena::new();
        assert!(build_rooted(&g, &mut arena, &[3], &Prune::default()).is_err());

        let mut arena = Arena::new();
        let spent = build_rooted(
            &g,
            &mut arena,
            &[0, 1],
            &Prune { max_nodes: 1, ..Prune::default() },
        );
        // The budget holds one node, so 0 is drawn and 1 is never reached
        // under it; a named root that cannot be placed is an error rather
        // than a silently smaller picture.
        assert!(spent.is_err());
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
