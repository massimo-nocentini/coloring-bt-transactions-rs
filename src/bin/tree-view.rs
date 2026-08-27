//! # A webgraph, looked at
//!
//! The same drawing `tree-jp2` writes to a file, in a window one can move
//! around in.  A `BvGraph` is read as a forest, the non-layered tidy trees
//! algorithm (van der Ploeg 2014) places every node, and what is on the screen
//! is drawn with Cairo — but only what is on the screen, which is the whole
//! point of the thing.  How the graph becomes a tree is in [`forest`]; the
//! window, the camera and the index under it are in [`viewer`], [`camera`],
//! [`quadtree`] and [`scene`], and this file is what is left: the command line,
//! and the two tones a graph with nothing else to say is drawn in.
//!
//! ```text
//! tree-view <graph-basename> [--width <px>] [--height <px>]
//! ```
//!
//! Built only with the `gui` feature, since GTK is a C library and the rest of
//! this crate has no reason to want it installed:
//!
//! ```text
//! cargo run --release --features gui --bin tree-view -- <graph-basename>
//! ```
//!
//! GTK's current major version is 4; there is no GTK 5, so 4 is what this binds
//! to, through the `gtk4` crate and the `cairo-rs` it re-exports.
//!
//! What one can do in the window, and what a frame of it costs, are in
//! [`viewer`].  `tx-view` is the same window over transactions, and the two
//! differ only in what they hand it to draw.

use std::env;
use std::process::ExitCode;

use non_layered_tidy_trees::Arena;
use webgraph::prelude::{BvGraph, SequentialLabeling};

#[path = "../camera.rs"]
mod camera;
#[path = "tree/forest.rs"]
mod forest;
#[path = "tree/quadtree.rs"]
mod quadtree;
#[path = "tree/scene.rs"]
mod scene;
#[path = "tree/viewer.rs"]
mod viewer;

use scene::Scene;
use viewer::{Paint, Rgb, View, DEFAULT_HEIGHT, DEFAULT_WIDTH};

const USAGE: &str = "usage: tree-view <graph-basename> [--width <px>] [--height <px>]";

/// The two tones the nodes are drawn in.
const INNER: Rgb = (0.0, 0.0, 0.0);
const LEAF: Rgb = (0.5, 0.5, 0.5);

/// A graph is a shape and nothing more, so the only thing there is to say about
/// a node in ink is whether anything hangs off it.
///
/// Two buckets, which is as few fills a frame as a drawing can have.
struct TwoTone;

impl Paint for TwoTone {
    fn buckets(&self) -> usize {
        2
    }

    fn bucket(&self, scene: &Scene, i: u32) -> usize {
        usize::from(scene.is_leaf(i))
    }

    fn colour(&self, bucket: usize) -> Rgb {
        if bucket == 0 {
            INNER
        } else {
            LEAF
        }
    }
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = env::args().skip(1).collect();

    let mut basenames: Vec<&str> = Vec::new();
    let mut width = DEFAULT_WIDTH;
    let mut height = DEFAULT_HEIGHT;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            flag @ ("--width" | "--height") => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("{flag} wants a number\n{USAGE}"))?;
                let px = v
                    .parse::<i32>()
                    .ok()
                    .filter(|&px| px > 0)
                    .ok_or_else(|| format!("{flag} {v}: a window needs a positive size"))?;
                if flag == "--width" {
                    width = px;
                } else {
                    height = px;
                }
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other if other.starts_with('-') => return Err(format!("unknown flag {other}\n{USAGE}")),
            other => basenames.push(other),
        }
        i += 1;
    }

    let [graph_name] = basenames.as_slice() else {
        return Err(format!(
            "expected one graph basename, got {}\n{USAGE}",
            basenames.len()
        ));
    };

    // All of this happens before the window opens: a graph of any size takes
    // long enough to lay out that an empty window would look like a hung one,
    // and stderr can say what is going on where a window not yet drawn cannot.
    eprintln!("reading {graph_name}");
    let graph = BvGraph::with_basename(graph_name)
        .load()
        .map_err(|e| format!("{graph_name}: {e:#}"))?;

    let mut arena = Arena::with_capacity(graph.num_nodes() + 1);
    let built = forest::build(&graph, &mut arena)?;
    eprintln!("{}", built.summary(graph.num_nodes()));

    eprintln!("laying out");
    let arena = forest::lay_out(arena, built.root);

    eprintln!("indexing");
    let scene = Scene::of(&arena, built.root)?;
    // The arena is the layout's working shape and is much the larger of the two;
    // nothing after this point wants it, so it goes before the window opens.
    drop(arena);
    eprintln!(
        "{} nodes in {} cells, {} deep",
        scene.len(),
        scene.index().cells(),
        scene.index().depth()
    );

    let title = format!("{graph_name} — {} nodes", scene.len());
    viewer::show(
        View::new(scene, TwoTone),
        "it.unifi.coloring-bt-transactions.tree-view",
        &title,
        width,
        height,
        "tree-view",
    )
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

    use camera::Camera;
    use viewer::{export, frame, MIN_CIRCLE_PX, MIN_HOLLOW_PX};

    /// A view on the forest of `graph_of(n, arcs)`, laid out and indexed, with
    /// the camera framing all of it in a window `width` by `height`.
    ///
    /// The whole of what the program does before GTK is involved, which is why
    /// the tests below start from a graph rather than from a scene.
    fn viewing(n: usize, arcs: &[(usize, usize)], width: f64, height: f64) -> View<TwoTone> {
        let graph = forest::graph_of(n, arcs);
        let mut arena = Arena::with_capacity(n + 1);
        let built = forest::build(&graph, &mut arena).unwrap();
        let arena = forest::lay_out(arena, built.root);
        let scene = Scene::of(&arena, built.root).unwrap();

        let mut view = View::new(scene, TwoTone);
        view.framing(width, height);
        view
    }

    /// A five-node tree: a root, a child with two of its own, and a second child.
    fn small() -> (usize, Vec<(usize, usize)>) {
        (5, vec![(0, 1), (0, 4), (1, 2), (1, 3)])
    }

    /// Framed, a small drawing is drawn as circles, every node of it, and the
    /// paper is no longer blank.
    #[test]
    fn a_small_drawing_is_circles() {
        let (n, arcs) = small();
        let mut view = viewing(n, &arcs, 400.0, 300.0);

        let pixels = frame(&mut view, 400, 300);

        assert_eq!(view.last.nodes, 5, "all five, one by one");
        assert_eq!(view.last.squares, 0, "and none of them summarised");
        assert!(view.scene.radius() * view.camera.scale() >= MIN_CIRCLE_PX);

        let inked = pixels.iter().filter(|p| p[0] < 200).count();
        assert!(inked > 100, "{inked} pixels of ink is not five circles");
    }

    /// Zoomed far enough out, the same drawing is squares of a shade and the
    /// frame's cost stops following the nodes.
    #[test]
    fn a_large_drawing_is_a_density() {
        // A chain, which is what a run of transactions spending each other is.
        let n = 5_000;
        let arcs: Vec<(usize, usize)> = (0..n - 1).map(|v| (v, v + 1)).collect();
        let mut view = viewing(n, &arcs, 400.0, 300.0);

        let pixels = frame(&mut view, 400, 300);

        assert!(view.scene.radius() * view.camera.scale() < MIN_CIRCLE_PX);
        assert!(view.last.squares > 0, "something was summarised");
        assert!(
            view.last.squares + view.last.nodes < n / 4,
            "{} patches for {n} nodes is not a summary",
            view.last.squares + view.last.nodes
        );
        assert_eq!(
            view.last.summarised as usize + view.last.nodes,
            n,
            "and every node is still standing for itself somewhere"
        );

        let inked = pixels.iter().filter(|p| p[0] < 240).count();
        assert!(inked > 50, "{inked} pixels: the chain went undrawn");
    }

    /// Zooming in shows fewer nodes, which is the point of only drawing what is
    /// on screen.
    #[test]
    fn zooming_in_leaves_most_of_the_drawing_alone() {
        let n = 5_000;
        let arcs: Vec<(usize, usize)> = (0..n - 1).map(|v| (v, v + 1)).collect();
        let mut view = viewing(n, &arcs, 400.0, 300.0);

        frame(&mut view, 400, 300);
        let whole = view.last.summarised as usize + view.last.nodes;

        for _ in 0..30 {
            view.camera.zoom_notches(-1.0, 200.0, 150.0);
        }
        frame(&mut view, 400, 300);
        let part = view.last.summarised as usize + view.last.nodes;

        assert_eq!(whole, n);
        assert!(part < n / 10, "{part} of {n} nodes touched, zoomed in");
    }

    /// A click selects the node under it and its subtree; the subtree is drawn
    /// in a colour of its own, and framing it is closer in than the whole.
    #[test]
    fn the_selection_is_drawn_apart() {
        // A balanced binary tree, so that a subtree is a large and obvious part
        // of the drawing rather than three circles of it.
        let n = 127;
        let arcs: Vec<(usize, usize)> =
            (0..n / 2).flat_map(|i| [(i, 2 * i + 1), (i, 2 * i + 2)]).collect();
        let mut view = viewing(n, &arcs, 400.0, 300.0);

        let plain = frame(&mut view, 400, 300);

        // Red where the blue is not.  The drawing's own ink is grey, which has
        // as much of one as of the other; the count is compared rather than
        // required to be zero because the panel's text is drawn with whatever
        // antialiasing the paper came with, and that can be a coloured fringe.
        let reddish = |p: &&[u8; 4]| i16::from(p[2]) - i16::from(p[0]) > 80;
        let before = plain.iter().filter(reddish).count();

        // Where the root's first child is on the screen, asked for the way a
        // click asks.  In pre-order that node is scene index 1.
        let [x, y] = view.scene.at(1);
        let chosen = view.scene.pick(x, y, view.scene.radius()).expect("a node is there");
        assert_eq!(chosen, 1);
        assert_eq!(view.scene.subtree(1).len(), (n - 1) / 2, "half the tree hangs off it");
        view.chosen = Some(chosen);

        let picked = frame(&mut view, 400, 300);
        let after = picked.iter().filter(reddish).count();
        assert!(
            after > before + 100,
            "{before} red pixels before, {after} after: the selection is not standing out"
        );

        // And the camera can be sent to it, which is closer in than the whole.
        let was = view.camera.scale();
        let subject = view.chosen_bounds();
        view.camera.frame(subject);
        assert!(view.camera.scale() > was, "{was} then {}", view.camera.scale());
        for i in view.scene.subtree(1) {
            let [x, y] = view.scene.at(i);
            assert!(view.camera.visible().contains(x, y), "node {i} is off screen");
        }
    }

    /// A graph with several roots draws them all, and having nothing selected is
    /// having everything framed.
    #[test]
    fn a_forest_of_several_roots_is_all_drawn() {
        let mut view = viewing(4, &[(0, 1), (2, 3)], 400.0, 300.0);

        frame(&mut view, 400, 300);
        assert_eq!(view.last.nodes, 4, "no node was lost to the added root");
        assert_eq!(view.chosen_bounds(), view.scene.bounds(), "nothing selected is everything");
    }

    /// A window GTK has not sized yet is drawn on rather than divided by.
    #[test]
    fn a_window_of_no_size_is_survivable() {
        let (n, arcs) = small();
        let mut view = viewing(n, &arcs, 400.0, 300.0);
        let pixels = frame(&mut view, 1, 1);
        assert_eq!(pixels.len(), 1);
    }

    /// A node with nothing hanging off it is a ring: white in the middle, its
    /// own colour on the rim.  A node with something hanging off it is solid.
    #[test]
    fn a_leaf_is_a_circle_with_nothing_in_it() {
        let (n, arcs) = small();
        let mut view = viewing(n, &arcs, 400.0, 300.0);

        let pixels = frame(&mut view, 400, 300);
        let radius_px = view.scene.radius() * view.camera.scale();
        assert!(radius_px >= MIN_HOLLOW_PX, "{radius_px} px: too small to be hollow");

        // The pixels are blue, green, red, alpha; everything here is grey, so
        // the first of them is the whole story.
        let sample = |pixels: &[[u8; 4]], camera: Camera, x: f64, y: f64| {
            let (sx, sy) = camera.to_screen(x, y);
            pixels[sy.round() as usize * 400 + sx.round() as usize][0]
        };

        let (mut leaves, mut inner) = (0, 0);
        for i in 0..view.scene.len() as u32 {
            let [x, y] = view.scene.at(i);
            // The panel is drawn over the bottom of the window, and a node
            // under it says nothing about how nodes are drawn.
            if view.camera.to_screen(x, y).1 > 190.0 {
                continue;
            }
            if view.scene.is_leaf(i) {
                leaves += 1;
                assert_eq!(sample(&pixels, view.camera, x, y), 255, "leaf {i} is filled in");
                // On the rim, half an outline either side of the radius: the
                // ink that makes it a circle rather than nothing at all.
                let rim = (radius_px - 0.5) / view.camera.scale();
                assert!(sample(&pixels, view.camera, x + rim, y) < 220, "leaf {i} has no edge");
            } else {
                inner += 1;
                assert!(sample(&pixels, view.camera, x, y) < 64, "node {i} is not filled in");
            }
        }
        assert!(leaves > 0 && inner > 0, "{leaves} leaves and {inner} others were looked at");

        // Zoomed out until a ring would close up into a smudge, a leaf goes
        // back to being a dot of its own colour.
        view.camera.zoom(1.5 / radius_px, 200.0, 150.0);
        let pixels = frame(&mut view, 400, 300);
        let small = view.scene.radius() * view.camera.scale();
        assert!((MIN_CIRCLE_PX..MIN_HOLLOW_PX).contains(&small), "{small} px");

        let leaf = (0..view.scene.len() as u32).find(|&i| view.scene.is_leaf(i)).unwrap();
        let [x, y] = view.scene.at(leaf);
        assert!(sample(&pixels, view.camera, x, y) < 220, "a leaf too small to be hollow is still a node");
    }

    /// `e` writes the camera to a PDF, and never over one already there.
    #[test]
    fn the_camera_can_be_written_to_a_page() {
        let (n, arcs) = small();
        let mut view = viewing(n, &arcs, 400.0, 300.0);

        // Somewhere of its own: the name is `<stem>-NNN.pdf`, and a stem may be
        // a path as well as a word.
        let dir = env::temp_dir().join(format!("tree-view-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stem = dir.join("page");
        let stem = stem.to_str().unwrap();

        let first = export(&mut view, stem).expect("a page");
        let second = export(&mut view, stem).expect("another page");
        assert_ne!(first, second, "the second export wrote over the first");

        for path in [&first, &second] {
            let bytes = std::fs::read(path).unwrap();
            assert!(bytes.starts_with(b"%PDF"), "{path} is not a PDF");
            // A five-node drawing is a few objects and a font; anything much
            // smaller than this is a page with nothing on it.
            assert!(bytes.len() > 512, "{path} is {} bytes", bytes.len());
        }

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Leaves and the rest are drawn apart, which is the whole of what this
    /// paint has to say.
    #[test]
    fn the_two_tones_are_leaves_and_the_rest() {
        let (n, arcs) = small();
        let view = viewing(n, &arcs, 400.0, 300.0);

        assert_eq!(TwoTone.buckets(), 2);
        for i in 0..view.scene.len() as u32 {
            let bucket = TwoTone.bucket(&view.scene, i);
            assert_eq!(bucket == 1, view.scene.is_leaf(i));
            assert_eq!(TwoTone.colour(bucket), if view.scene.is_leaf(i) { LEAF } else { INNER });
        }
    }
}
