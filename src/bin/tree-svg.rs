//! # A webgraph, drawn as circles
//!
//! Reads a BvGraph by basename, treats it as a forest, and writes an SVG in which
//! every node is a circle of diameter 1 placed where the non-layered tidy trees
//! algorithm (van der Ploeg 2014) puts it.  No edges are drawn: the geometry is
//! the whole message, and at the sizes these graphs reach the links would be more
//! ink than the nodes they connect.
//!
//! ```text
//! tree-svg <graph-basename> <transpose-basename> [-o <file>] [--scale <px>]
//! ```
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
//! access to successors will do — the BvGraph loaded from disk is only the
//! caller `main` happens to supply.
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
//! arc into it is dropped.  How many were dropped is reported on stderr, because
//! a picture that quietly stands for two thirds of the arcs is a lie the file
//! itself cannot tell you about.
//!
//! Nodes on a cycle are reached by no source at all.  Rather than drop them —
//! they are nodes, and leaving them out would make the drawing silently smaller
//! than the graph — each one left over after the sources are exhausted is
//! promoted to a root of its own, in node order, and its walk runs like any
//! other.  That count is reported too.
//!
//! # The shape of the drawing
//!
//! - `vertically: false`, so depth runs left to right and the breadth axis is `y`.
//! - Every real node is a `1.0` by `1.0` box, so a node's circle is inscribed in
//!   it and the drawing's units are nodes.
//! - **No margin along the depth axis** is not a setting: the algorithm puts a
//!   child's near edge exactly on its parent's far edge, so with unit boxes level
//!   `d` sits at `x = d` and a parent's circle touches its children's.
//! - `margin: 1.0` on every node, which is the separation the algorithm keeps
//!   between a node and the sibling subtree to its right — one clear diameter
//!   between neighbouring subtrees, at every level.
//!
//! Coordinates come back as top-left corners, so a circle's centre is the box
//! plus a half in each direction.
//!
//! # Output
//!
//! The `viewBox` is in node units and `--scale` (default 10) only sets the pixel
//! width and height, so the geometry in the file never depends on it.  Circles
//! are emitted in two groups, leaves and the rest, so that `fill` is written
//! twice rather than once per node — on a drawing of a million circles that is
//! most of the file.

use std::env;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use non_layered_tidy_trees::{layout, Arena, LayoutInput, NodeId};
use webgraph::prelude::{BvGraph, RandomAccessGraph, SequentialLabeling};

/// Diameter of a node's circle, and so the side of its box.
const DIAMETER: f64 = 1.0;

/// Separation kept between neighbouring sibling subtrees.
const SUBTREE_MARGIN: f64 = 1.0;

/// Pixels per node unit, when the caller does not say.
const DEFAULT_SCALE: f64 = 10.0;

/// Fill of a node with children, and of a leaf.
const INNER_FILL: &str = "#000000";
const LEAF_FILL: &str = "#808080";

/// Stack for the thread the layout runs on.
///
/// The three walks of the algorithm recurse once per level, and a graph of
/// transactions is deep — the crate's own depth test gives a 10 000-level chain a
/// 64 MiB stack.  This is virtual address space and only the pages actually
/// touched are ever committed, so a shallow tree pays nothing for the headroom.
const LAYOUT_STACK: usize = 1 << 30;

const USAGE: &str = "usage: tree-svg <graph-basename> <transpose-basename> \
                     [-o <file>] [--scale <px-per-node>]";

/// What the walk over the graph produced, beyond the arena itself.
struct Forest {
    root: NodeId,
    /// Sources of the graph — nodes the transpose says nothing points at.
    sources: usize,
    /// Nodes no source could reach, each promoted to a root of its own.
    promoted: usize,
    /// Arcs that would have given a node a second parent, and were dropped.
    dropped_arcs: u64,
    /// Whether a root standing for no node of the graph was added.
    synthetic_root: bool,
}

/// Builds the spanning forest of `graph` into an arena, rooted at a single node.
///
/// `transpose` is consulted only for `outdegree`, which is `graph`'s in-degree
/// and so the test for a source.
fn build<G: RandomAccessGraph, T: RandomAccessGraph>(
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

/// Writes the arena as circles, `w` having been sized by the caller.
fn write_svg(arena: &Arena, out: &mut impl Write, scale: f64) -> io::Result<usize> {
    let half = DIAMETER / 2.0;

    // The bounding box is measured over the circles actually drawn rather than
    // taken from the layout's normalization, because the node standing for
    // nothing is in the arena and is not in the picture.
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut drawn = 0usize;

    for node in arena.iter() {
        if node.isdummy {
            continue;
        }
        drawn += 1;
        min_x = min_x.min(node.x);
        min_y = min_y.min(node.y);
        max_x = max_x.max(node.x + node.w);
        max_y = max_y.max(node.y + node.h);
    }

    if drawn == 0 {
        return Err(io::Error::other("nothing to draw"));
    }

    let (w, h) = (max_x - min_x, max_y - min_y);

    writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"{min_x} {min_y} {w} {h}\" \
         width=\"{}\" height=\"{}\">",
        w * scale,
        h * scale
    )?;
    writeln!(
        out,
        "<rect x=\"{min_x}\" y=\"{min_y}\" width=\"{w}\" height=\"{h}\" fill=\"#ffffff\"/>"
    )?;

    // Two passes so that the fill is named twice rather than once per circle.
    for (fill, leaves) in [(INNER_FILL, false), (LEAF_FILL, true)] {
        writeln!(out, "<g fill=\"{fill}\">")?;
        for node in arena.iter() {
            if node.isdummy || node.children().is_empty() != leaves {
                continue;
            }
            writeln!(
                out,
                "<circle cx=\"{}\" cy=\"{}\" r=\"{half}\"/>",
                node.x + half,
                node.y + half
            )?;
        }
        writeln!(out, "</g>")?;
    }

    writeln!(out, "</svg>")?;
    Ok(drawn)
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = env::args().skip(1).collect();

    let mut basenames: Vec<&str> = Vec::new();
    let mut out_path: Option<&str> = None;
    let mut scale = DEFAULT_SCALE;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                out_path = Some(argv.get(i).ok_or_else(|| format!("-o wants a file\n{USAGE}"))?);
            }
            "--scale" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--scale wants a number\n{USAGE}"))?;
                scale = v.parse::<f64>().map_err(|_| format!("--scale {v}: not a number"))?;
                if !(scale > 0.0) {
                    return Err(format!("--scale {v}: a drawing needs a positive scale"));
                }
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other if other.starts_with('-') => return Err(format!("unknown flag {other}\n{USAGE}")),
            other => basenames.push(other),
        }
        i += 1;
    }

    let [graph_name, transpose_name] = basenames.as_slice() else {
        return Err(format!(
            "expected a graph and its transpose, got {} basename(s)\n{USAGE}",
            basenames.len()
        ));
    };

    // `load` answers with an `anyhow::Error`, which is not this crate's to name;
    // it is a `Display` all the same, and that is all a message needs.
    let graph = BvGraph::with_basename(graph_name)
        .load()
        .map_err(|e| format!("{graph_name}: {e:#}"))?;
    let transpose = BvGraph::with_basename(transpose_name)
        .load()
        .map_err(|e| format!("{transpose_name}: {e:#}"))?;

    let mut arena = Arena::with_capacity(graph.num_nodes() + 1);
    let forest = build(&graph, &transpose, &mut arena)?;

    eprintln!(
        "{} nodes, {} sources{}{}",
        graph.num_nodes(),
        forest.sources,
        if forest.promoted > 0 {
            format!(", {} node(s) on cycles promoted to roots", forest.promoted)
        } else {
            String::new()
        },
        if forest.synthetic_root {
            ", one added root standing for no node"
        } else {
            ""
        }
    );

    if forest.dropped_arcs > 0 {
        eprintln!(
            "{} arc(s) not drawn: they would have given a node a second parent",
            forest.dropped_arcs
        );
    }

    // The walks recurse once per level; see `LAYOUT_STACK`.  The arena goes over
    // and comes back rather than being borrowed, so nothing here is shared.
    let root = forest.root;
    let arena = std::thread::Builder::new()
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
        .map_err(|_| "the layout thread panicked".to_string())?;

    let drawn = match out_path {
        Some(path) => {
            let file = File::create(path).map_err(|e| format!("{path}: {e}"))?;
            let mut out = BufWriter::new(file);
            let drawn = write_svg(&arena, &mut out, scale).map_err(|e| format!("{path}: {e}"))?;
            out.flush().map_err(|e| format!("{path}: {e}"))?;
            drawn
        }
        None => {
            let stdout = io::stdout();
            let mut out = BufWriter::new(stdout.lock());
            let drawn = write_svg(&arena, &mut out, scale).map_err(|e| format!("stdout: {e}"))?;
            out.flush().map_err(|e| format!("stdout: {e}"))?;
            drawn
        }
    };

    eprintln!("{drawn} circles written");
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use webgraph::prelude::VecGraph;

    /// The two graphs a run is given, built by hand so that the shape under test
    /// is the one written here rather than one a fixture file happens to hold.
    fn pair(n: usize, arcs: &[(usize, usize)]) -> (VecGraph, VecGraph) {
        let mut g = VecGraph::empty(n);
        let mut t = VecGraph::empty(n);
        for &(u, v) in arcs {
            g.add_arc(u, v);
            t.add_arc(v, u);
        }
        (g, t)
    }

    /// `(x, y, leaf)` per drawn node, keyed by the graph node the circle stands
    /// for — `idx` is one-based, and the added root carries 0.
    fn drawing(arena: &Arena) -> Vec<(usize, f64, f64, bool)> {
        let mut out: Vec<_> = arena
            .iter()
            .filter(|n| !n.isdummy)
            .map(|n| (n.idx - 1, n.x, n.y, n.children().is_empty()))
            .collect();
        out.sort_by_key(|&(v, ..)| v);
        out
    }

    fn lay_out(arena: &mut Arena, root: NodeId) {
        let mut input = LayoutInput::new(root);
        input.vertically = false;
        layout(arena, &input);
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

        lay_out(&mut arena, forest.root);

        // The depth coordinate is the level, exactly: unit boxes and a root of
        // zero width leave level d at x = d, which is what "no margin between
        // parent and children" means once the circles are drawn.
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

        assert!(arena[forest.root].isdummy, "the root stands for no node");
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

        lay_out(&mut arena, forest.root);

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

        lay_out(&mut arena, forest.root);

        assert_eq!(
            drawing(&arena),
            [(0, 0.0, 0.0, false), (1, 1.0, 0.0, false), (2, 2.0, 0.0, true)]
        );
    }

    /// The drawing is measured over the circles, not over the arena, so the added
    /// root neither shows up nor moves anything.
    #[test]
    fn the_added_root_is_not_in_the_picture() {
        let (g, t) = pair(2, &[]);

        let mut arena = Arena::new();
        let forest = build(&g, &t, &mut arena).unwrap();
        assert!(forest.synthetic_root);

        lay_out(&mut arena, forest.root);

        let mut svg = Vec::new();
        let drawn = write_svg(&arena, &mut svg, 10.0).unwrap();
        let svg = String::from_utf8(svg).unwrap();

        assert_eq!(drawn, 2, "two nodes, two circles");
        assert_eq!(svg.matches("<circle").count(), 2);
        // Two isolated nodes, one column wide and two circles plus a margin tall.
        assert!(svg.contains("viewBox=\"0 0 1 3\""), "{svg}");
        assert!(svg.contains("width=\"10\" height=\"30\""), "{svg}");
    }

    #[test]
    fn a_graph_and_a_transpose_of_different_sizes_are_refused() {
        let mut g = VecGraph::empty(3);
        g.add_arc(0, 1);
        let t = VecGraph::empty(2);

        let mut arena = Arena::new();
        assert!(build(&g, &t, &mut arena).is_err());
    }
}
