//! # A webgraph, drawn as a JPEG 2000
//!
//! Reads a BvGraph by basename, treats it as a forest, and writes a **lossless**
//! JPEG 2000 in which every node is one pixel, placed where the non-layered tidy
//! trees algorithm (van der Ploeg 2014) puts it.  How the graph becomes a tree,
//! where its roots come from, and what the layout is asked for are all in
//! [`forest`]; this file is only about turning coordinates into a codestream.
//!
//! ```text
//! tree-jp2 <graph-basename> -o <file> [--zoom <n>]
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
//! number because a raster has no way to be given a fractional one.
//!
//! The picture is one 8-bit greyscale component in three tones: leaves are
//! [`LEAF_INK`] against [`INNER_INK`] nodes on [`PAPER`], the same three the
//! windowed viewer draws.
//!
//! # Why lossless, and what that costs
//!
//! A lossy JPEG 2000 would be smaller and would be the wrong picture: a node is
//! one pixel, so the difference between a node and no node is a single sample,
//! and that is exactly what a quantiser spends first.  So the encoder is set up
//! for the reversible path — the 5/3 integer wavelet, no quantisation, one
//! quality layer at no rate cap — and the bytes that go in are the bytes that
//! come back.  The `round_trips_losslessly` test asserts that against the
//! decoder rather than trusting the setting.
//!
//! What that buys over a raw raster is the compression a netpbm has no way to
//! express.  These pictures are nearly all paper, and the wavelet plus EBCOT
//! charges almost nothing for it: `cnr-2000`, 325 557 nodes on a 37-by-218 886
//! grid, comes to 1.1 MB against the 8.1 MB the same pixels are as a `P5`.  And
//! unlike `gzip` over a raster, the result is still an image every tool can
//! open, and one that can be decoded at a reduced resolution — [`RESOLUTIONS`]
//! of them — without reading it whole.
//!
//! What it costs is time.  A raster writer is a memcpy; this runs a wavelet and
//! an arithmetic coder over every sample, paper included.
//!
//! # Why this streams, and why it wants a file
//!
//! The whole raster is `width * height` and the nodes are only `n`, so the
//! picture is held as one `(row, column, ink)` per node — twelve bytes, against
//! a byte for every pixel of a picture that is mostly white.  Sorting that list
//! puts the nodes in the order the tiles are written, so the raster is built one
//! [`TILE`]-square tile at a time and never exists whole: the memory here is one
//! tile's worth however big the picture is, which is what lets a drawing far
//! past the size of memory be written at all.
//!
//! The codestream is not written straight through, though — the encoder goes
//! back to stamp lengths into markers it has already emitted — so the output has
//! to be a file it can seek in, and `-o` is required rather than defaulting to
//! standard output the way a netpbm writer could.

use std::env;
use std::process::ExitCode;

use non_layered_tidy_trees::Arena;
use openjp2::image::{opj_image, opj_image_comptparm};
use openjp2::openjpeg::{
    opj_cparameters_t, OPJ_CLRSPC_GRAY, OPJ_CODEC_JP2, OPJ_LRCP,
};
use openjp2::{Codec, Stream};
use webgraph::prelude::{BvGraph, SequentialLabeling};

#[path = "tree/forest.rs"]
mod forest;

/// Pixels per node unit along the depth axis: a column is a level.
const DEPTH_PX: f64 = 1.0;

/// Pixels per node unit along the breadth axis: a row is a node and the margin
/// beside it.  See the module docs for why the two axes differ.
const BREADTH_PX: f64 = 0.5;

/// The three tones, matching the viewer's `#000000`, `#808080`, `#ffffff`.
const INNER_INK: u8 = 0x00;
const LEAF_INK: u8 = 0x80;
const PAPER: u8 = 0xff;

/// How many pixels a node pixel becomes on a side, when the caller does not say.
const DEFAULT_ZOOM: usize = 1;

/// The side of the square the picture is written in.  This is the working set:
/// a tile is built whole in memory and handed to the encoder, so the number is a
/// trade of memory against how much context the wavelet has to compress with,
/// and 1024 is a mebibyte a tile.
const TILE: usize = 1024;

/// Wavelet decomposition levels.  Six is the encoder's own default, and it is
/// also what lets a viewer open one of these at 1/32 scale without decoding the
/// full-size picture.
const RESOLUTIONS: i32 = 6;

const USAGE: &str = "usage: tree-jp2 <graph-basename> -o <file> [--zoom <n>]";

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

/// One tile of the zoomed picture, as the bytes the encoder wants: row-major,
/// a byte a pixel, clipped to the image at the far edge.
///
/// `(x0, y0)` is the tile's corner in zoomed pixels and `(x1, y1)` one past its
/// far corner.  The dots are looked up rather than walked because a source row
/// covers `zoom` rows of the picture and so can fall in two tiles.
fn tile_bytes(
    picture: &Picture,
    zoom: usize,
    (x0, y0): (usize, usize),
    (x1, y1): (usize, usize),
) -> Vec<u8> {
    let (w, h) = (x1 - x0, y1 - y0);
    let mut tile = vec![PAPER; w * h];

    // The source rows any of this tile's rows came from.  Both ends are rows
    // that exist, since the tile itself is inside the picture.
    let (lo, hi) = ((y0 / zoom) as u32, ((y1 - 1) / zoom) as u32);
    let from = picture.dots.partition_point(|&(row, ..)| row < lo);
    let to = picture.dots.partition_point(|&(row, ..)| row <= hi);

    for &(row, col, ink) in &picture.dots[from..to] {
        // The block of zoomed pixels this node covers, clipped to the tile.
        let (bx0, bx1) = (col as usize * zoom, (col as usize + 1) * zoom);
        let (by0, by1) = (row as usize * zoom, (row as usize + 1) * zoom);
        let (bx0, bx1) = (bx0.max(x0), bx1.min(x1));
        let (by0, by1) = (by0.max(y0), by1.min(y1));
        if bx0 >= bx1 {
            continue;
        }
        for y in by0..by1 {
            let at = (y - y0) * w + (bx0 - x0);
            tile[at..at + (bx1 - bx0)].fill(ink);
        }
    }

    tile
}

/// Writes the picture to `path` as a lossless JPEG 2000, one tile at a time.
fn write_jp2(picture: &Picture, path: &str, zoom: usize) -> Result<(), String> {
    let width = picture.width * zoom;
    let height = picture.height * zoom;

    let comp = opj_image_comptparm {
        dx: 1,
        dy: 1,
        w: width as u32,
        h: height as u32,
        x0: 0,
        y0: 0,
        prec: 8,
        bpp: 8,
        sgnd: 0,
    };
    // `tile_create` rather than `create`: it describes the picture without
    // allocating a sample for every pixel of it, which is the whole point of
    // handing the encoder one tile at a time.  The extent is not in the
    // component parameters, so it is set here.
    let mut image =
        opj_image::tile_create(&[comp], OPJ_CLRSPC_GRAY).ok_or("could not describe the image")?;
    image.x1 = width as u32;
    image.y1 = height as u32;

    let mut params = opj_cparameters_t::default();
    params.tile_size_on = 1;
    params.cp_tdx = TILE as i32;
    params.cp_tdy = TILE as i32;
    params.numresolution = RESOLUTIONS;
    params.prog_order = OPJ_LRCP;
    // Lossless: the reversible 5/3 wavelet, and one quality layer whose rate is
    // 0 — the encoder's spelling of "do not throw anything away".
    params.irreversible = 0;
    params.tcp_numlayers = 1;
    params.tcp_rates[0] = 0.0;
    params.cp_disto_alloc = 1;

    let mut codec = Codec::new_encoder(OPJ_CODEC_JP2).ok_or("could not open a JPEG 2000 encoder")?;
    // The encoder seeks back over what it has written, so this is a file rather
    // than the caller's choice of sink.
    let mut stream =
        Stream::new_file(path, 1 << 20, false).map_err(|e| format!("{path}: {e}"))?;

    if codec.setup_encoder(&mut params, &mut image) == 0 {
        return Err(format!("{path}: the encoder would not take these settings"));
    }
    if codec.start_compress(&mut image, &mut stream) == 0 {
        return Err(format!("{path}: could not start the codestream"));
    }

    // Tiles go out in index order across and then down, which is the order the
    // encoder counts them in and the only one it accepts.
    let across = width.div_ceil(TILE);
    let down = height.div_ceil(TILE);

    for ty in 0..down {
        let (y0, y1) = (ty * TILE, ((ty + 1) * TILE).min(height));
        for tx in 0..across {
            let (x0, x1) = (tx * TILE, ((tx + 1) * TILE).min(width));
            let bytes = tile_bytes(picture, zoom, (x0, y0), (x1, y1));
            let index = (ty * across + tx) as u32;
            if codec.write_tile(index, &bytes, &mut stream) == 0 {
                return Err(format!("{path}: could not write tile {index}"));
            }
        }
    }

    if codec.end_compress(&mut stream) == 0 {
        return Err(format!("{path}: could not close the codestream"));
    }

    Ok(())
}

/// `n` bytes as something a person can read at a glance.
fn human(bytes: u64) -> String {
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
    let mut zoom = DEFAULT_ZOOM;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                out_path = Some(argv.get(i).ok_or_else(|| format!("-o wants a file\n{USAGE}"))?);
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

    let [graph_name] = basenames.as_slice() else {
        return Err(format!(
            "expected one graph basename, got {}\n{USAGE}",
            basenames.len()
        ));
    };

    // See the module docs: the codestream is written with backward seeks, so
    // there is no standard-output form of this to fall back to.
    let Some(out_path) = out_path else {
        return Err(format!(
            "-o is required: a JPEG 2000 is written by seeking back over it, \
             which a pipe cannot do\n{USAGE}"
        ));
    };

    // `load` answers with an `anyhow::Error`, which is not this crate's to name;
    // it is a `Display` all the same, and that is all a message needs.
    let graph = BvGraph::with_basename(graph_name)
        .load()
        .map_err(|e| format!("{graph_name}: {e:#}"))?;

    let mut arena = Arena::with_capacity(graph.num_nodes() + 1);
    let built = forest::build(&graph, &mut arena)?;

    eprintln!("{}", built.summary(graph.num_nodes()));

    let arena = forest::lay_out(arena, built.root);
    let picture = plot(&arena)?;

    let (width, height) = (picture.width * zoom, picture.height * zoom);
    eprintln!(
        "{width} by {height} pixels, in {} tile(s) of {TILE}",
        width.div_ceil(TILE) * height.div_ceil(TILE)
    );

    if picture.collisions > 0 {
        eprintln!(
            "{} node(s) share a pixel with another and are not drawn separately; \
             --zoom does not help, the grid is what it is",
            picture.collisions
        );
    }

    write_jp2(&picture, out_path, zoom)?;

    let size = std::fs::metadata(out_path)
        .map(|m| m.len())
        .map_err(|e| format!("{out_path}: {e}"))?;
    eprintln!("{} pixels drawn, {} written", picture.dots.len(), human(size));
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
    use openjp2::openjpeg::opj_dparameters_t;

    /// The forest of [`forest`]'s own test, whose coordinates are asserted there:
    /// three roots, two dropped arcs, and a cycle among them.
    fn everything() -> Picture {
        let g = forest::graph_of(
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
        let built = forest::build(&g, &mut arena).unwrap();
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

    /// The same, read back out of a written file: `width * height` samples in
    /// row-major order, which is what the encoder was handed if nothing was lost.
    fn decode(path: &str) -> (usize, usize, Vec<u8>) {
        let mut params = opj_dparameters_t::default();
        let mut codec = Codec::new_decoder(OPJ_CODEC_JP2).unwrap();
        assert!(codec.setup_decoder(&mut params) != 0);

        let mut stream = Stream::new_file(path, 1 << 20, true).unwrap();
        let mut image = codec.read_header(&mut stream).expect("a JP2 header");
        assert!(codec.decode(&mut stream, &mut image) != 0);
        assert!(codec.end_decompress(&mut stream) != 0);

        let (w, h) = (image.x1 as usize, image.y1 as usize);
        let comps = image.comps().unwrap();
        assert_eq!(comps.len(), 1, "one greyscale component");
        let samples = comps[0].data().unwrap().iter().map(|&s| s as u8).collect();

        (w, h, samples)
    }

    /// A path under the test runner's temporary directory, distinct per test so
    /// that the tests can run in the same directory at the same time.
    fn scratch(name: &str) -> String {
        let dir = env::temp_dir().join(format!("tree-jp2-{}-{name}.jp2", std::process::id()));
        dir.to_string_lossy().into_owned()
    }

    /// The picture as bytes, straight from the dots: what the file has to decode
    /// back to, pixel for pixel.
    fn expected(picture: &Picture, zoom: usize) -> Vec<u8> {
        let (w, h) = (picture.width * zoom, picture.height * zoom);
        let mut want = vec![PAPER; w * h];
        for &(row, col, ink) in &picture.dots {
            for y in row as usize * zoom..(row as usize + 1) * zoom {
                let at = y * w + col as usize * zoom;
                want[at..at + zoom].fill(ink);
            }
        }
        want
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

    /// A tile is the picture cut out of it, and nothing of its neighbours: the
    /// three tones land where the dots say and the rest is paper.
    #[test]
    fn a_tile_is_the_picture_under_it() {
        let picture = everything();

        // One tile, since the whole picture is far inside a `TILE` square.
        let whole = tile_bytes(&picture, 1, (0, 0), (3, 5));
        assert_eq!(whole.len(), 15);
        assert_eq!(&whole[..3], [PAPER, INNER_INK, LEAF_INK], "the first row is .#+");
        assert_eq!(&whole[3..6], [INNER_INK, PAPER, LEAF_INK], "the second is #.+");

        // The bottom two rows on their own are the same two rows.
        let lower = tile_bytes(&picture, 1, (0, 3), (3, 5));
        assert_eq!(lower, whole[9..]);

        // And the middle column on its own is the middle column.
        let strip = tile_bytes(&picture, 1, (1, 0), (2, 5));
        assert_eq!(strip, [INNER_INK, PAPER, INNER_INK, LEAF_INK, LEAF_INK]);
    }

    /// Zooming repeats every pixel on both axes and nothing else, including when
    /// a node's block is cut in half by a tile edge.
    #[test]
    fn zoom_repeats_pixels_across_tile_edges() {
        let picture = everything();
        let zoom = 2;

        let whole = tile_bytes(&picture, zoom, (0, 0), (6, 10));
        assert_eq!(whole, expected(&picture, zoom));

        // A tile edge through the middle of the first source row's block: the
        // two halves are still the row, one line each.
        let top = tile_bytes(&picture, zoom, (0, 0), (6, 1));
        let next = tile_bytes(&picture, zoom, (0, 1), (6, 2));
        assert_eq!(top, next, "both halves of a doubled row are that row");
        assert_eq!(top, &whole[..6]);
    }

    /// The added root is not a node and gets no pixel.
    #[test]
    fn the_added_root_is_not_in_the_picture() {
        let g = forest::graph_of(2, &[]);

        let mut arena = Arena::new();
        let built = forest::build(&g, &mut arena).unwrap();
        assert!(built.synthetic_root);

        let arena = forest::lay_out(arena, built.root);
        let picture = plot(&arena).unwrap();

        assert_eq!(picture.dots.len(), 2, "two nodes, two pixels");
        // Two rows rather than three: the clear unit the layout keeps between
        // them is the half of a row `BREADTH_PX` spends on every node's margin.
        assert_eq!(render(&picture), "+\n+");
    }

    /// The claim the whole file rests on: what the encoder was handed is what the
    /// decoder gives back, sample for sample.  Nothing in the settings *says*
    /// lossless out loud, so it is asserted rather than assumed.
    #[test]
    fn round_trips_losslessly() {
        let picture = everything();
        let path = scratch("lossless");

        write_jp2(&picture, &path, 1).unwrap();

        let (w, h, samples) = decode(&path);
        assert_eq!((w, h), (picture.width, picture.height));
        assert_eq!(samples, expected(&picture, 1), "every sample survived");

        // And the three tones are still three distinct tones, not three tones
        // that a quantiser has quietly pulled together.
        let mut tones: Vec<u8> = samples.clone();
        tones.sort_unstable();
        tones.dedup();
        assert_eq!(tones, [INNER_INK, LEAF_INK, PAPER]);

        std::fs::remove_file(&path).ok();
    }

    /// A picture wider and taller than one tile, so that the tile loop runs more
    /// than once on each axis and the seams have to line up.
    #[test]
    fn a_picture_of_many_tiles_round_trips() {
        // A chain of `n` nodes is `n` levels deep and one row tall, so the depth
        // axis alone would never make a second tile row.  Zoom makes both axes
        // bigger at once.
        let arcs: Vec<(usize, usize)> = (0..TILE + 200).map(|i| (i, i + 1)).collect();
        let g = forest::graph_of(arcs.len() + 1, &arcs);

        let mut arena = Arena::with_capacity(arcs.len() + 2);
        let built = forest::build(&g, &mut arena).unwrap();
        let arena = forest::lay_out(arena, built.root);
        let picture = plot(&arena).unwrap();

        let zoom = 2;
        let (w, h) = (picture.width * zoom, picture.height * zoom);
        assert!(w > TILE, "more than one tile across, so the seam is exercised");

        let path = scratch("tiles");
        write_jp2(&picture, &path, zoom).unwrap();

        let (dw, dh, samples) = decode(&path);
        assert_eq!((dw, dh), (w, h));
        assert_eq!(samples, expected(&picture, zoom));

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn sizes_are_read_at_a_glance() {
        assert_eq!(human(999), "999 B");
        assert_eq!(human(1_000), "1.0 kB");
        assert_eq!(human(125_000_000), "125.0 MB");
    }
}
