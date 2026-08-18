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
//! How the graph becomes a tree, why the transpose is a second argument, and what
//! the layout is asked for are all in [`forest`]; this file is only the ink.
//! `tree-bitmap` draws the same layout as pixels.
//!
//! # The shape of the drawing
//!
//! Every real node is a [`forest::DIAMETER`] box, so a node's circle is inscribed
//! in it and the drawing's units are nodes.  Levels touch along the depth axis —
//! a parent's circle touches its children's — and neighbouring subtrees are kept
//! [`forest::SUBTREE_MARGIN`], one clear diameter, apart at every level.
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

use non_layered_tidy_trees::Arena;
use webgraph::prelude::{BvGraph, SequentialLabeling};

#[path = "tree/forest.rs"]
mod forest;

use forest::DIAMETER;

/// Pixels per node unit, when the caller does not say.
const DEFAULT_SCALE: f64 = 10.0;

/// Fill of a node with children, and of a leaf.
const INNER_FILL: &str = "#000000";
const LEAF_FILL: &str = "#808080";

const USAGE: &str = "usage: tree-svg <graph-basename> <transpose-basename> \
                     [-o <file>] [--scale <px-per-node>]";

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
    let built = forest::build(&graph, &transpose, &mut arena)?;

    eprintln!("{}", built.summary(graph.num_nodes()));

    let arena = forest::lay_out(arena, built.root)?;

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

    /// The drawing is measured over the circles, not over the arena, so the added
    /// root neither shows up nor moves anything.
    #[test]
    fn the_added_root_is_not_in_the_picture() {
        let (g, t) = forest::pair(2, &[]);

        let mut arena = Arena::new();
        let built = forest::build(&g, &t, &mut arena).unwrap();
        assert!(built.synthetic_root);

        let arena = forest::lay_out(arena, built.root).unwrap();

        let mut svg = Vec::new();
        let drawn = write_svg(&arena, &mut svg, 10.0).unwrap();
        let svg = String::from_utf8(svg).unwrap();

        assert_eq!(drawn, 2, "two nodes, two circles");
        assert_eq!(svg.matches("<circle").count(), 2);
        // Two isolated nodes, one column wide and two circles plus a margin tall.
        assert!(svg.contains("viewBox=\"0 0 1 3\""), "{svg}");
        assert!(svg.contains("width=\"10\" height=\"30\""), "{svg}");
    }
}
