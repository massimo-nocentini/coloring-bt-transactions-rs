//! # A webgraph, drawn as pixels
//!
//! The same drawing `tree-svg` makes, with the ink spent as sparingly as it can
//! be: one **pixel** per node, placed where the non-layered tidy trees algorithm
//! (van der Ploeg 2014) puts it, written as a netpbm bitmap.  How the graph
//! becomes a tree and what the layout is asked for are in [`forest`]; this file
//! is only about turning coordinates into a raster.
//!
//! ```text
//! tree-bitmap <graph-basename> <transpose-basename> [-o <file>]
//!             [--format pbm|pgm] [--zoom <n>]
//! ```
//!
//! # A pixel per node, and not a pixel more
//!
//! The layout's units are nodes, so a pixel grid is a matter of picking how many
//! pixels a unit is worth — and the answer is different on the two axes, because
//! the layout treats them differently:
//!
//! - Along **depth** nothing separates one level from the next: a child's near
//!   edge is its parent's far edge, so level `d` sits at `x = d` and
//!   [`DEPTH_PX`] of 1 makes column `d` that level, exactly.
//! - Along **breadth** every node carries [`forest::SUBTREE_MARGIN`], so two
//!   nodes at the same depth have centres at least two units apart — one for the
//!   node, one for the clear space beside it.  [`BREADTH_PX`] of 1/2 spends a
//!   pixel on the pair, which is what closes up the blank row that would
//!   otherwise sit under every node.
//!
//! Together they are the tightest grid the drawing fits in: every node lands on
//! its own pixel, no pixel is wasted on the margins, and a picture of `n` nodes
//! is about as many pixels.  Since the separation is a lower bound rather than
//! an equality — the algorithm pushes subtrees further apart when their shapes
//! demand it — the grid has white in it wherever the tree does, which is the
//! shape one wants to see.  Nothing *guarantees* two nodes cannot round into one
//! pixel, so [`Picture::collisions`] counts the ones that did and the run says
//! so rather than quietly drawing `n - 1` nodes.
//!
//! `--zoom n` then repeats every pixel `n` times on each axis, for the small
//! graphs where a picture a few pixels across is not a picture.  It is a whole
//! number because a bitmap has no way to be given a fractional one: `tree-svg`'s
//! `--scale` can be 2.5 since it only sets a display size over geometry that
//! stays exact, and here the geometry *is* the pixels.
//!
//! # Two formats, and what the smaller one drops
//!
//! [`Format::Pbm`] is netpbm's binary bitmap, `P4`: a short text header, then the
//! rows packed eight pixels to the byte with the leftmost pixel in the *most*
//! significant bit.  A 1 bit is black, so a bit says "a node is here" and nothing
//! else — the leaf-versus-inner shading `tree-svg` draws is the thing one bit has
//! no room for.
//!
//! [`Format::Pgm`] is `P5`, a byte a pixel, which buys that shading back: leaves
//! are [`LEAF_INK`] against [`INNER_INK`] nodes on [`PAPER`], the same three
//! tones the SVG uses.  It costs exactly eight times the bytes, since neither
//! format compresses anything — both compress well afterwards, and that
//! composes with either choice.
//!
//! Netpbm has no way to say "and the rest of this row is white", so a picture
//! costs `width * height` whatever is in it.  That is the trade against the SVG:
//! a drawing 3 000 levels deep and a million nodes wide is 375 MB as a `P4` and a
//! few tens of MB as circles, while a bushy one — few levels, many nodes — is
//! smaller here than any list of circles could be.  The run prints the size
//! before it writes it.
//!
//! # Why this streams from a sorted list
//!
//! The whole raster is `width * height` and the nodes are only `n`, so the
//! picture is held as one `(row, column, ink)` per node — twelve bytes, against a
//! byte or a bit for every pixel of a picture that is mostly white.  Sorting that
//! list puts the nodes in the order the rows are written, so the raster is built
//! one row at a time and never exists whole.

use std::env;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::process::ExitCode;

use non_layered_tidy_trees::Arena;
use webgraph::prelude::{BvGraph, SequentialLabeling};

#[path = "tree/forest.rs"]
mod forest;

/// Pixels per node unit along the depth axis: a column is a level.
const DEPTH_PX: f64 = 1.0;

/// Pixels per node unit along the breadth axis: a row is a node and the margin
/// beside it.  See the module docs for why the two axes differ.
const BREADTH_PX: f64 = 0.5;

/// The three tones of a `P5`, matching the SVG's `#000000`, `#808080`, `#ffffff`.
const INNER_INK: u8 = 0x00;
const LEAF_INK: u8 = 0x80;
const PAPER: u8 = 0xff;

/// How many pixels a node pixel becomes on a side, when the caller does not say.
const DEFAULT_ZOOM: usize = 1;

const USAGE: &str = "usage: tree-bitmap <graph-basename> <transpose-basename> \
                     [-o <file>] [--format pbm|pgm] [--zoom <n>]";

/// Which of the two pictures to draw.  The module docs say what each costs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Format {
    /// One bit a pixel: a node is there, or it is not.
    Pbm,
    /// One byte a pixel, which has room to say whether the node is a leaf.
    Pgm,
}

impl Format {
    /// The header, which both formats put in front of the picture as text.
    fn header(self, width: usize, height: usize) -> String {
        match self {
            Format::Pbm => format!("P4\n{width} {height}\n"),
            Format::Pgm => format!("P5\n{width} {height}\n255\n"),
        }
    }

    /// Bytes one row of `width` pixels takes.
    fn row_bytes(self, width: usize) -> usize {
        match self {
            Format::Pbm => width.div_ceil(8),
            Format::Pgm => width,
        }
    }
}

/// The drawing as pixels: what row and column each node landed on, and how big
/// the grid holding them is.
struct Picture {
    /// Columns and rows, in node pixels — before any `--zoom`.
    width: usize,
    height: usize,
    /// One entry per drawn node, sorted by `(row, column)`, with the darker ink
    /// first so that a node with children wins a pixel a leaf also wants.
    dots: Vec<(u32, u32, u8)>,
    /// Nodes that landed on a pixel another node already had.  Zero on every
    /// forest seen so far; see the module docs for why it is counted anyway.
    collisions: usize,
}

/// Puts every node of a laid-out arena on a pixel.
fn plot(arena: &Arena) -> Result<Picture, String> {
    // The bounding box is measured over the nodes actually drawn rather than
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
        return Err("nothing to draw".to_string());
    }

    // Rounded up, so that the last column and row are whole ones: the grid is
    // the smallest that has room for every box the layout placed.
    let width = (((max_x - min_x) * DEPTH_PX).ceil() as usize).max(1);
    let height = (((max_y - min_y) * BREADTH_PX).ceil() as usize).max(1);

    // A coordinate is a `u32` so that the list costs twelve bytes a node rather
    // than twenty-four; 4 294 967 295 columns is past what any of this could
    // draw, and past it the message should be about the picture, not a panic.
    if width.max(height) > u32::MAX as usize {
        return Err(format!(
            "the drawing is {width} by {height} pixels, which is more than this can raster"
        ));
    }

    let mut dots = Vec::with_capacity(drawn);

    for node in arena.iter() {
        if node.isdummy {
            continue;
        }
        // The centre of the box rather than its corner, so that a node whose
        // breadth coordinate is a half — every parent centred over its children
        // has one — falls in the row it looks like it is in.  `min` guards the
        // far edge, where a centre can round onto the column past the last.
        let col = (((node.x + node.w / 2.0 - min_x) * DEPTH_PX) as usize).min(width - 1);
        let row = (((node.y + node.h / 2.0 - min_y) * BREADTH_PX) as usize).min(height - 1);
        let ink = if node.children().is_empty() {
            LEAF_INK
        } else {
            INNER_INK
        };
        dots.push((row as u32, col as u32, ink));
    }

    // Sorting by the tuple sorts by row, then column, then ink -- and `INNER_INK`
    // is the smaller number, which is what makes the first of two nodes on one
    // pixel the darker one.
    dots.sort_unstable();
    let before = dots.len();
    dots.dedup_by_key(|&mut (row, col, _)| (row, col));

    Ok(Picture {
        width,
        height,
        collisions: before - dots.len(),
        dots,
    })
}

/// Writes the picture, every pixel repeated `zoom` times on each axis.
///
/// Rows go out as they are built, so the memory here is one row's worth however
/// big the picture is.
fn write_bitmap(
    picture: &Picture,
    out: &mut impl Write,
    format: Format,
    zoom: usize,
) -> io::Result<()> {
    let width = picture.width * zoom;
    let height = picture.height * zoom;

    out.write_all(format.header(width, height).as_bytes())?;

    // Blank at all times outside the pixels of the row being drawn, which is what
    // lets a row be cleared by walking its own dots again rather than by being
    // filled from end to end.
    let mut row = match format {
        Format::Pbm => vec![0u8; format.row_bytes(width)],
        Format::Pgm => vec![PAPER; format.row_bytes(width)],
    };

    let mut dots = picture.dots.as_slice();

    for r in 0..picture.height {
        let end = dots.partition_point(|&(row, ..)| (row as usize) <= r);
        let (here, rest) = dots.split_at(end);
        dots = rest;

        for &(_, col, ink) in here {
            let from = col as usize * zoom;
            match format {
                // Leftmost pixel in the most significant bit, which is P4's order.
                Format::Pbm => {
                    for x in from..from + zoom {
                        row[x / 8] |= 0x80 >> (x % 8);
                    }
                }
                Format::Pgm => row[from..from + zoom].fill(ink),
            }
        }

        for _ in 0..zoom {
            out.write_all(&row)?;
        }

        for &(_, col, _) in here {
            let from = col as usize * zoom;
            match format {
                Format::Pbm => {
                    for x in from..from + zoom {
                        row[x / 8] &= !(0x80 >> (x % 8));
                    }
                }
                Format::Pgm => row[from..from + zoom].fill(PAPER),
            }
        }
    }

    Ok(())
}

/// `n` bytes as something a person can read at a glance.
fn human(bytes: usize) -> String {
    const UNITS: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit + 1 < UNITS.len() {
        size /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit])
    }
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = env::args().skip(1).collect();

    let mut basenames: Vec<&str> = Vec::new();
    let mut out_path: Option<&str> = None;
    let mut format = Format::Pbm;
    let mut zoom = DEFAULT_ZOOM;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                out_path = Some(argv.get(i).ok_or_else(|| format!("-o wants a file\n{USAGE}"))?);
            }
            "--format" => {
                i += 1;
                format = match argv.get(i).map(String::as_str) {
                    Some("pbm") => Format::Pbm,
                    Some("pgm") => Format::Pgm,
                    Some(other) => return Err(format!("--format {other}: pbm or pgm")),
                    None => return Err(format!("--format wants pbm or pgm\n{USAGE}")),
                };
            }
            "--zoom" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--zoom wants a number\n{USAGE}"))?;
                zoom = v.parse::<usize>().map_err(|_| format!("--zoom {v}: not a number"))?;
                if zoom == 0 {
                    return Err("--zoom 0: a node is at least one pixel".to_string());
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

    let arena = forest::lay_out(arena, built.root);
    let picture = plot(&arena)?;

    let (width, height) = (picture.width * zoom, picture.height * zoom);
    eprintln!(
        "{width} by {height} pixels, {} to write",
        human(format.row_bytes(width) * height)
    );

    if picture.collisions > 0 {
        eprintln!(
            "{} node(s) share a pixel with another and are not drawn separately; \
             --zoom does not help, the grid is what it is",
            picture.collisions
        );
    }

    match out_path {
        Some(path) => {
            let file = File::create(path).map_err(|e| format!("{path}: {e}"))?;
            let mut out = BufWriter::with_capacity(1 << 20, file);
            write_bitmap(&picture, &mut out, format, zoom).map_err(|e| format!("{path}: {e}"))?;
            out.flush().map_err(|e| format!("{path}: {e}"))?;
        }
        None => {
            let stdout = io::stdout();
            let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());
            write_bitmap(&picture, &mut out, format, zoom).map_err(|e| format!("stdout: {e}"))?;
            out.flush().map_err(|e| format!("stdout: {e}"))?;
        }
    }

    eprintln!("{} pixels drawn", picture.dots.len());
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

    /// The forest of [`forest`]'s own test, whose coordinates are asserted there:
    /// two sources, a dropped arc, and a cycle promoted to a root.
    fn everything() -> Picture {
        let (g, t) = forest::pair(
            10,
            &[
                (0, 1),
                (0, 2),
                (1, 3),
                (1, 4),
                (2, 5),
                (6, 7),
                (7, 5),
                (8, 9),
                (9, 8),
            ],
        );

        let mut arena = Arena::new();
        let built = forest::build(&g, &t, &mut arena).unwrap();
        let arena = forest::lay_out(arena, built.root);
        plot(&arena).unwrap()
    }

    /// The grid a picture is drawn on, as one string a row at a time: `#` for a
    /// node with children, `+` for a leaf, `.` for paper.
    fn render(picture: &Picture) -> String {
        let mut grid = vec![vec!['.'; picture.width]; picture.height];
        for &(row, col, ink) in &picture.dots {
            grid[row as usize][col as usize] = if ink == LEAF_INK { '+' } else { '#' };
        }
        grid.into_iter()
            .map(|r| r.into_iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ten nodes on a three by five grid: the layout, at one pixel a node.
    ///
    /// Reading it against the coordinates `forest`'s test asserts: the three
    /// levels are the three columns, and every node has a pixel to itself.
    #[test]
    fn a_pixel_per_node() {
        let picture = everything();

        assert_eq!((picture.width, picture.height), (3, 5));
        assert_eq!(picture.dots.len(), 10, "ten nodes, ten pixels");
        assert_eq!(picture.collisions, 0);

        assert_eq!(
            render(&picture),
            ".#+\n\
             #.+\n\
             .#+\n\
             #+.\n\
             #+."
        );
    }

    /// `P4` packs the leftmost pixel into the most significant bit, and says
    /// nothing about which nodes are leaves.
    #[test]
    fn the_bitmap_is_the_grid_bit_by_bit() {
        let picture = everything();

        let mut out = Vec::new();
        write_bitmap(&picture, &mut out, Format::Pbm, 1).unwrap();

        let (header, rows) = out.split_at("P4\n3 5\n".len());
        assert_eq!(header, b"P4\n3 5\n");
        // A row is a byte, three of its bits used: .#+ #.+ .#+ ##+ ##+
        assert_eq!(rows, [0b0110_0000, 0b1010_0000, 0b0110_0000, 0b1100_0000, 0b1100_0000]);
    }

    /// `P5` has room for the leaf shading, and spends a byte a pixel to get it.
    #[test]
    fn the_greymap_keeps_the_leaves_apart() {
        let picture = everything();

        let mut out = Vec::new();
        write_bitmap(&picture, &mut out, Format::Pgm, 1).unwrap();

        let (header, rows) = out.split_at("P5\n3 5\n255\n".len());
        assert_eq!(header, b"P5\n3 5\n255\n");
        assert_eq!(rows.len(), 3 * 5);
        assert_eq!(
            rows[..3],
            [PAPER, INNER_INK, LEAF_INK],
            "the first row is .#+"
        );
        assert_eq!(rows[3..6], [INNER_INK, PAPER, LEAF_INK], "the second is #.+");
    }

    /// Zooming repeats every pixel on both axes and nothing else: the picture is
    /// the same one, in bigger pixels.
    #[test]
    fn zoom_repeats_pixels() {
        let picture = everything();

        let mut out = Vec::new();
        write_bitmap(&picture, &mut out, Format::Pgm, 2).unwrap();

        let (header, rows) = out.split_at("P5\n6 10\n255\n".len());
        assert_eq!(header, b"P5\n6 10\n255\n");
        assert_eq!(rows.len(), 6 * 10);
        // `.#+` twice as wide, and the row itself written twice.
        let first = [PAPER, PAPER, INNER_INK, INNER_INK, LEAF_INK, LEAF_INK];
        assert_eq!(rows[..6], first);
        assert_eq!(rows[6..12], first);
        assert_ne!(rows[12..18], first, "the third row is the next one, doubled");
    }

    /// Between one row and the next the buffer goes back to paper, so a pixel
    /// cannot smear down the picture.
    #[test]
    fn a_row_does_not_leak_into_the_next() {
        let (g, t) = forest::pair(3, &[(0, 1), (1, 2)]);

        let mut arena = Arena::new();
        let built = forest::build(&g, &t, &mut arena).unwrap();
        let arena = forest::lay_out(arena, built.root);
        let picture = plot(&arena).unwrap();

        // A chain is one row deep and as wide as it is long.
        assert_eq!((picture.width, picture.height), (3, 1));

        let (g, t) = forest::pair(4, &[(0, 1), (0, 2), (0, 3)]);
        let mut arena = Arena::new();
        let built = forest::build(&g, &t, &mut arena).unwrap();
        let arena = forest::lay_out(arena, built.root);
        let picture = plot(&arena).unwrap();

        assert_eq!(
            render(&picture),
            ".+\n\
             #+\n\
             .+",
            "a root over three leaves, centred on the middle one"
        );

        let mut out = Vec::new();
        write_bitmap(&picture, &mut out, Format::Pbm, 1).unwrap();
        assert_eq!(&out[out.len() - 3..], [0b0100_0000, 0b1100_0000, 0b0100_0000]);
    }

    /// The added root is not a node and gets no pixel.
    #[test]
    fn the_added_root_is_not_in_the_picture() {
        let (g, t) = forest::pair(2, &[]);

        let mut arena = Arena::new();
        let built = forest::build(&g, &t, &mut arena).unwrap();
        assert!(built.synthetic_root);

        let arena = forest::lay_out(arena, built.root);
        let picture = plot(&arena).unwrap();

        assert_eq!(picture.dots.len(), 2, "two nodes, two pixels");
        // Two rows rather than three: the clear unit the layout keeps between
        // them is the half of a row `BREADTH_PX` spends on every node's margin.
        assert_eq!(render(&picture), "+\n+");
    }

    #[test]
    fn sizes_are_read_at_a_glance() {
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1_000), "1.0 kB");
        assert_eq!(human(125_000_000), "125.0 MB");
    }
}
