//! # The ink the pages share
//!
//! The palette and the few sizes that decide how a page looks: what colour a
//! node with children is, how small a circle may get, how much paper is kept
//! round the drawing.  `tree-pdf` and `block-pdf` draw different things — one
//! a spanning tree, the other the bipartite gadgets of the graph's blocks —
//! and they must draw them in the *same* ink, so that a reader who has learnt
//! that orange means "the cut took something here" from one figure can read
//! the next one without a second legend.  One file both compile in is how the
//! two are kept from drifting.
//!
//! It is a module rather than a library for the reason `forest` and `pdf`
//! are: the crate's `src/*.rs` belong to the main binary, so the drawing
//! binaries reach it by `#[path]` from `src/bin/`.

// Every binary uses most of these and none uses all of them; the surface is
// the point, so the lint goes rather than the surface.
#![allow(dead_code)]

/// A colour as the page takes them, and the tones of the viewers, plus one.
pub type Rgb = (f64, f64, f64);
pub const PAPER: Rgb = (1.0, 1.0, 1.0);
pub const INNER: Rgb = (0.0, 0.0, 0.0);
pub const LEAF: Rgb = (0.5, 0.5, 0.5);
pub const LINK: Rgb = (0.78, 0.78, 0.78);
/// A node whose successors the cut left out: drawn apart, so that the pruned
/// frontier cannot pass for a fringe of true leaves.
pub const CUT: Rgb = (0.82, 0.24, 0.10);

/// The arcs a spanning tree *cannot* draw: the second parents it dropped,
/// restored to the page behind everything, in a tone a reader reads as
/// absent.  A tree of this graph hides three arcs in four
/// (`sum_b (|I_b| - 1) |O_b|`, three quarters of every arc there is), and a
/// figure that draws only what the tree kept is a figure of the renderer, not
/// of the graph; drawn in this ink the same page is both --- the tree in
/// [`LINK`] and, under it, the bipartite block the tree turned into a star.
pub const GHOST: Rgb = (0.62, 0.72, 0.86);

/// The dash a ghost arc is stroked with, in points at the page's own scale:
/// on and off.  Dashed rather than merely paler because a reduction in
/// lightness survives neither a photocopier nor a colour-blind reader,
/// whereas a broken line stays broken.
pub const GHOST_DASH: (f64, f64) = (1.6, 1.3);

/// A node the caller named with `--mark`: the subject of the drawing --- a
/// stolen output, a payout leg --- inked apart from everything structural.
pub const MARK: Rgb = (0.16, 0.47, 0.84);

/// The ink labels are written in, when `--labels` asks for them.
pub const LABEL: Rgb = (0.32, 0.32, 0.32);

/// How wide the drawing may be, in points, when the caller does not say.
/// A little under a typical `\textwidth`, so the figure drops straight in.
pub const DEFAULT_WIDTH: f64 = 420.0;

/// How tall it may grow before the height binds instead.
pub const DEFAULT_MAX_HEIGHT: f64 = 620.0;

/// Nodes the walk may place when the caller does not say.  A page is legible
/// into the tens of thousands of nodes; past that the raster of `tree-jp2` is
/// the better picture anyway.
pub const DEFAULT_MAX_NODES: usize = 100_000;

/// Clear paper kept around the drawing, in points.
pub const MARGIN: f64 = 4.0;

/// Below this radius a circle would be finer than any press: the floor the
/// nodes are held at, whatever the scale.
pub const MIN_RADIUS: f64 = 0.3;

/// How much of its box a node inks.  The layout puts neighbouring levels edge
/// to edge, so circles drawn at the full box fuse into a bar along every
/// chain; a shade under it leaves a sliver of paper between the beads.
pub const INK: f64 = 0.85;

/// And above this radius a circle is a balloon: a drawing of a dozen nodes
/// scaled to a page would ink them shoulder to shoulder, where holding the
/// circles down turns the same page into an airy diagram whose edges do the
/// talking.
pub const MAX_RADIUS: f64 = 5.0;

/// A leaf is hollow — its rim inked, paper inside — from this radius up, as in
/// the windowed viewer; below it a ring closes into a smudge and the leaf is
/// filled like everything else.
pub const MIN_HOLLOW: f64 = 1.0;

/// How many shapes go into one path before it is painted.  One fill per
/// colour is the idea; a bound per path is kindness to readers that walk a
/// path recursively.
pub const BATCH: usize = 4096;
