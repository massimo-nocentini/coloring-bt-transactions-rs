//! # A subtree, written as a page
//!
//! The drawing `tree-view` shows and `tree-jp2` rasters, cut down to the
//! subtree of a node one names, and written as a vector PDF a paper can take:
//!
//! ```text
//! tree-pdf <graph-basename> --root <id>[,<id>...] -o <file>
//!          [--depth <n>] [--max-nodes <n>] [--fanout <n>]
//!          [--vertical] [--fill] [--width <pt>] [--max-height <pt>]
//!          [--mark <id>[,<id>...]] [--labels] [--spine <id>[,<id>...]]
//!          [--ancestors <transpose-basename> [--depth-up <n>] [--fanout-up <n>] [--max-nodes-up <n>]]
//! ```
//!
//! The graph this exists for has two billion nodes, and no page holds two
//! billion of anything.  What a page *can* hold is a neighbourhood: the walk
//! starts at the named roots instead of sweeping the whole graph, and the
//! three scissors of [`forest::Prune`] — `--depth`, `--max-nodes`, `--fanout`
//! — say where it stops.  Run on the transpose graph the same command draws a
//! node's *ancestors*: the transpose's successors are the graph's
//! predecessors, so `--root` there reads as "where did this come from" rather
//! than "where did this go".
//!
//! # The ink
//!
//! The layout and the shapes are the viewers': depth runs left to right, a
//! node is a circle, a node with children is filled and a leaf is hollow once
//! there is room for a ring, and the arcs to parents are drawn behind them in
//! a grey that stays behind.  One tone is this binary's own — a node whose
//! successors the cut left out is filled in a warning colour, because a
//! pruned frontier drawn like a fringe of true leaves would be the drawing
//! lying about the graph.  The count of such nodes is on stderr too, and a
//! caption quoting it is telling the truth about the picture.
//!
//! # The page
//!
//! Written by [`pdf`], which is this crate and `flate2` and nothing else — no
//! Cairo, no toolkit, nothing to install.  Cairo's pages and these agree
//! about what a drawing is; this one exists so that a machine with a graph
//! and a Rust toolchain can make a figure.
//!
//! The page is sized to the drawing rather than the drawing to a page: the
//! scale is whatever fits `--width` points across (and `--max-height` down,
//! whichever binds), and the page then hugs the drawing.  A node never
//! shrinks below a third of a point — on a page of thousands of levels the
//! circles are the drawing's *texture*, and a reader zooming into the PDF
//! still finds every one of them a circle, because a page description has no
//! resolution to run out of.

use std::collections::HashSet;
use std::env;
use std::process::ExitCode;

use non_layered_tidy_trees::Arena;
use webgraph::prelude::BvGraph;

#[path = "../camera.rs"]
mod camera;
#[path = "tree/forest.rs"]
mod forest;
#[path = "tree/pdf.rs"]
mod pdf;
#[path = "tree/quadtree.rs"]
mod quadtree;
#[path = "tree/scene.rs"]
mod scene;

use pdf::Page;
use scene::{Scene, NO_PARENT};

const USAGE: &str = "usage: tree-pdf <graph-basename> --root <id>[,<id>...] -o <file>\n\
       [--depth <n>] [--max-nodes <n>] [--fanout <n>]\n\
       [--vertical] [--fill] [--width <pt>] [--max-height <pt>]\n\
       [--mark <id>[,<id>...]] [--labels] [--spine <id>[,<id>...]]\n\
       [--ancestors <transpose-basename> [--depth-up <n>] [--fanout-up <n>] [--max-nodes-up <n>]]";

/// A colour as the page takes them, and the tones of the viewers, plus one.
type Rgb = (f64, f64, f64);
const PAPER: Rgb = (1.0, 1.0, 1.0);
const INNER: Rgb = (0.0, 0.0, 0.0);
const LEAF: Rgb = (0.5, 0.5, 0.5);
const LINK: Rgb = (0.78, 0.78, 0.78);
/// A node whose successors the cut left out: drawn apart, so that the pruned
/// frontier cannot pass for a fringe of true leaves.
const CUT: Rgb = (0.82, 0.24, 0.10);

/// A node the caller named with `--mark`: the subject of the drawing --- a
/// stolen output, a payout leg --- inked apart from everything structural.
const MARK: Rgb = (0.16, 0.47, 0.84);

/// The ink labels are written in, when `--labels` asks for them.
const LABEL: Rgb = (0.32, 0.32, 0.32);

/// How wide the drawing may be, in points, when the caller does not say.
/// A little under a typical `\textwidth`, so the figure drops straight in.
const DEFAULT_WIDTH: f64 = 420.0;

/// How tall it may grow before the height binds instead.
const DEFAULT_MAX_HEIGHT: f64 = 620.0;

/// Nodes the walk may place when the caller does not say.  A page is legible
/// into the tens of thousands of nodes; past that the raster of `tree-jp2` is
/// the better picture anyway.
const DEFAULT_MAX_NODES: usize = 100_000;

/// Clear paper kept around the drawing, in points.
const MARGIN: f64 = 4.0;

/// Below this radius a circle would be finer than any press: the floor the
/// nodes are held at, whatever the scale.
const MIN_RADIUS: f64 = 0.3;

/// How much of its box a node inks.  The layout puts neighbouring levels edge
/// to edge, so circles drawn at the full box fuse into a bar along every
/// chain; a shade under it leaves a sliver of paper between the beads.
const INK: f64 = 0.85;

/// And above this radius a circle is a balloon: a drawing of a dozen nodes
/// scaled to a page would ink them shoulder to shoulder, where holding the
/// circles down turns the same page into an airy diagram whose edges do the
/// talking.
const MAX_RADIUS: f64 = 5.0;

/// A leaf is hollow — its rim inked, paper inside — from this radius up, as in
/// the windowed viewer; below it a ring closes into a smudge and the leaf is
/// filled like everything else.
const MIN_HOLLOW: f64 = 1.0;

/// How many shapes go into one path before it is painted.  One fill per
/// colour is the idea; a bound per path is kindness to readers that walk a
/// path recursively.
const BATCH: usize = 4096;

/// One tree of a drawing: the descendants of the root, or --- `flipped`, on
/// the other side of it --- its ancestors.
///
/// A plain subtree page is one part.  An *hourglass* (`--ancestors`) is two:
/// the same root walked forward on the graph and backward on its transpose,
/// composed about the one node they share, so that a single page answers both
/// of the forensic questions --- where did this come from, and where did it
/// go.  The flipped part's depth axis runs in the negative direction, which on
/// a vertical page puts the past above the event and the future below it.
struct Part<'a> {
    scene: &'a Scene,
    truncated: &'a HashSet<usize>,
    flipped: bool,
}

/// How the page is settled: everything about the drawing that is not a tree.
struct Layout {
    width: f64,
    max_height: f64,
    fill: bool,
    vertical: bool,
    labels: bool,
}

/// Draws laid-out scenes as one page: each part's `truncated` being the graph
/// ids whose successors its cut left out, and `marks` the ids the caller asked
/// to have inked apart.
///
/// Two ways of settling the scale.  By default one scale serves both axes ---
/// whichever of `width` and `max_height` binds --- and the page then hugs the
/// drawing, so its shape is the drawing's shape.  With `fill` the two axes are
/// scaled *independently* to the page the caller asked for, exactly as
/// `tree-jp2` spends different pixels per unit on depth and breadth: the trees
/// of this graph are routinely a hundred times broader than they are deep, and
/// a uniform scale would render such a subtree as a ribbon eleven points tall
/// on a page meant to hold a plate.  Stretching the depth axis spreads the
/// generations down the page without moving any node relative to another in
/// its level; what it costs is that distances no longer mean the same thing on
/// the two axes, which is why it is the caller's flag and not the default.
///
/// `vertical` must say which axis is depth, because the node radius keys on
/// the tighter of the two spacings --- a level is 1 unit along depth, sibling
/// centres 2 units along breadth --- and under `fill` the axes no longer agree.
fn draw(parts: &[Part], marks: &HashSet<usize>, opts: &Layout) -> Page {
    // Every part is expressed in coordinates centred on its root --- scene
    // index 0, since a single named root is walked first --- with the depth
    // axis negated for a flipped part, so an hourglass's two trees meet at
    // one shared point.
    let centred = |part: &Part<'_>, i: u32| {
        let [rx, ry] = part.scene.at(0);
        let [x, y] = part.scene.at(i);
        let (mut dx, mut dy) = (x - rx, y - ry);
        if part.flipped {
            if opts.vertical {
                dy = -dy;
            } else {
                dx = -dx;
            }
        }
        (dx, dy)
    };

    let mut bounds = camera::Rect::nothing();
    for part in parts {
        for i in 0..part.scene.len() as u32 {
            let (x, y) = centred(part, i);
            bounds.add(x, y);
        }
    }
    let bounds = bounds.grown(parts[0].scene.radius());

    // Labels overhang the circles they name, so a labelled page keeps more
    // clear paper around the drawing than a plain one.
    let margin = if opts.labels { MARGIN + 12.0 } else { MARGIN };

    let (scale_x, scale_y, page_w, page_h) = if opts.fill {
        (
            (opts.width - 2.0 * margin) / bounds.width(),
            (opts.max_height - 2.0 * margin) / bounds.height(),
            opts.width,
            opts.max_height,
        )
    } else {
        let scale = ((opts.width - 2.0 * margin) / bounds.width())
            .min((opts.max_height - 2.0 * margin) / bounds.height());
        (
            scale,
            scale,
            bounds.width() * scale + 2.0 * margin,
            bounds.height() * scale + 2.0 * margin,
        )
    };

    // PDF's y runs up the page and the layout's runs down the drawing, so the
    // flip lives here, in the one mapping every mark goes through.
    let at = |part: &Part<'_>, i: u32| {
        let (x, y) = centred(part, i);
        (
            margin + (x - bounds.min_x) * scale_x,
            page_h - margin - (y - bounds.min_y) * scale_y,
        )
    };

    // The ink is a shade smaller than the tightest spacing on the page, so
    // that the beads of a chain part instead of fusing into a bar; the floor
    // keeps a node printable however far out the scale is.  Levels sit 1 unit
    // apart along the depth axis and sibling centres 2 apart along breadth,
    // which under `fill` are different numbers of points.
    let (depth_scale, breadth_scale) = if opts.vertical {
        (scale_y, scale_x)
    } else {
        (scale_x, scale_y)
    };
    let spacing = depth_scale.min(2.0 * breadth_scale);
    let radius = (0.5 * spacing * INK).clamp(MIN_RADIUS, MAX_RADIUS);
    let hollow = radius >= MIN_HOLLOW;

    // The root belongs to every part but is drawn once, by the first: what a
    // later part contributes at index 0 is its edges, not a second disc.
    let drawn = |p: usize, i: u32| p == 0 || i != 0;

    let mut page = Page::new(page_w, page_h);

    // The arcs first, behind everything: one grey, one width, one stroke per
    // batch.
    page.set_stroke(LINK);
    page.set_line_width((radius / 3.0).clamp(0.16, 1.2));
    let mut in_path = 0;
    for part in parts {
        for i in 0..part.scene.len() as u32 {
            let parent = part.scene.node(i).parent;
            if parent == NO_PARENT {
                continue;
            }
            let (px, py) = at(part, parent);
            let (x, y) = at(part, i);
            page.segment(px, py, x, y);
            in_path += 1;
            if in_path == BATCH {
                page.stroke();
                in_path = 0;
            }
        }
    }
    if in_path > 0 {
        page.stroke();
    }

    // The nodes, sorted into the few kinds they are drawn as, each colour
    // named once.  Filled says something hangs under the node *in this
    // drawing*, hollow says nothing does, exactly as in the viewers; the
    // warning colour says the cut took some of the node's successors, whether
    // or not it left it any --- so a filled warning node is an incomplete fan,
    // and a hollow one is a frontier the drawing stops at --- and a marked
    // node wears the caller's colour over whatever else it is: the subject of
    // the drawing outranks the bookkeeping of its cut.
    let class = |part: &Part<'_>, i: u32| {
        let graph = part.scene.node(i).graph as usize;
        let leaf = usize::from(part.scene.is_leaf(i));
        if marks.contains(&graph) {
            4 + leaf
        } else if part.truncated.contains(&graph) {
            2 + leaf
        } else {
            leaf
        }
    };

    for wanted in 0..=5 {
        if !parts.iter().enumerate().any(|(p, part)| {
            (0..part.scene.len() as u32).any(|i| drawn(p, i) && class(part, i) == wanted)
        }) {
            // A colour is named only over the nodes drawn in it: a page with
            // nothing cut and nothing marked has neither ink in its bytes.
            continue;
        }
        let colour = [INNER, LEAF, CUT, CUT, MARK, MARK][wanted];
        // The odd classes are the childless ones, drawn hollow when there is
        // room for a ring.
        let rings = hollow && wanted % 2 == 1;
        if rings {
            // A ring is paper filled and the colour stroked, in one operator.
            page.set_fill(PAPER);
            page.set_stroke(colour);
            page.set_line_width((radius / 3.0).clamp(0.3, 1.6));
        } else {
            page.set_fill(colour);
        }
        let mut in_path = 0;
        for (p, part) in parts.iter().enumerate() {
            for i in 0..part.scene.len() as u32 {
                if !drawn(p, i) || class(part, i) != wanted {
                    continue;
                }
                let (x, y) = at(part, i);
                page.circle(x, y, radius);
                in_path += 1;
                if in_path == BATCH {
                    if rings {
                        page.fill_and_stroke();
                    } else {
                        page.fill();
                    }
                    in_path = 0;
                }
            }
        }
        if in_path > 0 {
            if rings {
                page.fill_and_stroke();
            } else {
                page.fill();
            }
        }
    }

    // The labels last, over everything: each node's graph id centred above
    // its circle, where a chain has paper --- at its side it would lie across
    // the next bead.  Asked for by flag because labels only earn their ink on
    // a drawing of tens of nodes; the size follows the circles and never
    // drops below legibility.
    if opts.labels {
        let size = (radius * 2.2).clamp(3.5, 7.0);
        // Helvetica's digits are 0.556 em wide, which is all a graph id is
        // made of: enough metrics to centre a label without carrying a font.
        let digit = 0.556 * size;
        page.set_fill(LABEL);
        for (p, part) in parts.iter().enumerate() {
            for i in 0..part.scene.len() as u32 {
                if !drawn(p, i) {
                    continue;
                }
                let (x, y) = at(part, i);
                let label = part.scene.node(i).graph.to_string();
                let width = digit * label.len() as f64;
                // Alternating shoulders: siblings sit two units apart and an
                // id is wider than that, so neighbours take turns above and
                // below and each label gets its neighbour's clearance too.
                let ly = if i % 2 == 0 {
                    y + radius + 1.0
                } else {
                    y - radius - 1.0 - 0.72 * size
                };
                page.text(x - width / 2.0, ly, size, &label);
            }
        }
    }

    page
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = env::args().skip(1).collect();

    let mut basenames: Vec<&str> = Vec::new();
    let mut out_path: Option<&str> = None;
    let mut roots: Vec<usize> = Vec::new();
    let mut prune = forest::Prune { max_nodes: DEFAULT_MAX_NODES, ..forest::Prune::default() };
    let mut width = DEFAULT_WIDTH;
    let mut max_height = DEFAULT_MAX_HEIGHT;
    let mut vertical = false;
    let mut fill = false;
    let mut labels = false;
    let mut marks: HashSet<usize> = HashSet::new();
    let mut ancestors: Option<&str> = None;
    let mut depth_up: Option<usize> = None;
    let mut fanout_up: Option<usize> = None;
    let mut max_nodes_up: Option<usize> = None;

    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "-o" | "--out" => {
                i += 1;
                out_path = Some(argv.get(i).ok_or_else(|| format!("-o wants a file\n{USAGE}"))?);
            }
            "--root" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--root wants a node id\n{USAGE}"))?;
                for part in v.split(',') {
                    let id = part
                        .parse::<usize>()
                        .map_err(|_| format!("--root {part}: not a node id"))?;
                    roots.push(id);
                }
            }
            "--depth" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--depth wants a number\n{USAGE}"))?;
                prune.depth =
                    Some(v.parse::<usize>().map_err(|_| format!("--depth {v}: not a number"))?);
            }
            "--max-nodes" => {
                i += 1;
                let v =
                    argv.get(i).ok_or_else(|| format!("--max-nodes wants a number\n{USAGE}"))?;
                prune.max_nodes = v
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("--max-nodes {v}: wants a positive number"))?;
            }
            "--fanout" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--fanout wants a number\n{USAGE}"))?;
                prune.fanout = Some(
                    v.parse::<usize>()
                        .ok()
                        .filter(|&n| n > 0)
                        .ok_or_else(|| format!("--fanout {v}: wants a positive number"))?,
                );
            }
            flag @ ("--width" | "--max-height") => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("{flag} wants a number\n{USAGE}"))?;
                let pt = v
                    .parse::<f64>()
                    .ok()
                    .filter(|&pt| pt > 2.0 * MARGIN)
                    .ok_or_else(|| format!("{flag} {v}: a page needs a positive size"))?;
                if flag == "--width" {
                    width = pt;
                } else {
                    max_height = pt;
                }
            }
            // Depth down the page instead of across it: what a fan wants --
            // one level deep and a thousand siblings broad -- and what a chain
            // does not.
            "--vertical" => vertical = true,
            // Scale the two axes independently so the drawing fills the page
            // asked for, instead of one scale and a page that hugs the
            // drawing.  See [`draw`] for what that buys and what it costs.
            "--fill" => fill = true,
            // Each node's graph id at its shoulder; for drawings small enough
            // that a reader can be told which node is which.
            "--labels" => labels = true,
            "--mark" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--mark wants a node id\n{USAGE}"))?;
                for part in v.split(',') {
                    marks.insert(
                        part.parse::<usize>()
                            .map_err(|_| format!("--mark {part}: not a node id"))?,
                    );
                }
            }
            // Only these nodes (and the roots) are expanded: the drawing is
            // the named chain with every leg one node deep.  See
            // [`forest::Prune::expand`].
            "--spine" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--spine wants node ids\n{USAGE}"))?;
                let spine = prune.expand.get_or_insert_with(HashSet::new);
                for part in v.split(',') {
                    spine.insert(
                        part.parse::<usize>()
                            .map_err(|_| format!("--spine {part}: not a node id"))?,
                    );
                }
            }
            // The transpose graph, to hang the root's ancestry on the other
            // side of it: the hourglass of [`Part`].
            "--ancestors" => {
                i += 1;
                ancestors = Some(
                    argv.get(i).ok_or_else(|| format!("--ancestors wants a graph basename\n{USAGE}"))?,
                );
            }
            flag @ ("--depth-up" | "--fanout-up" | "--max-nodes-up") => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("{flag} wants a number\n{USAGE}"))?;
                let n = v
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0 || flag == "--depth-up")
                    .ok_or_else(|| format!("{flag} {v}: not a usable number"))?;
                match flag {
                    "--depth-up" => depth_up = Some(n),
                    "--fanout-up" => fanout_up = Some(n),
                    _ => max_nodes_up = Some(n),
                }
            }
            "-h" | "--help" => return Err(USAGE.to_string()),
            other if other.starts_with('-') => {
                return Err(format!("unknown flag {other}\n{USAGE}"))
            }
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
    let Some(out_path) = out_path else {
        return Err(format!("-o is required\n{USAGE}"));
    };
    if roots.is_empty() {
        return Err(format!(
            "--root is required: the whole graph is what tree-jp2 is for\n{USAGE}"
        ));
    }
    if let Some(spine) = prune.expand.as_mut() {
        // The roots expand whether or not the caller repeated them in the
        // spine: a chain that excluded its own head would draw one node.
        spine.extend(roots.iter().copied());
    }

    eprintln!("reading {graph_name}");
    let graph = BvGraph::with_basename(graph_name)
        .load()
        .map_err(|e| format!("{graph_name}: {e:#}"))?;

    let mut arena = Arena::new();
    let sampled = forest::build_rooted(&graph, &mut arena, &roots, &prune)?;
    eprintln!("{}", sampled.summary());

    let arena = forest::lay_out_oriented(arena, sampled.root, vertical);
    let scene = Scene::of(&arena, sampled.root)?;
    drop(arena);
    let mut truncated = sampled.truncated;

    // The other side of the hourglass: the same root walked on the transpose,
    // under its own scissors where the caller gave any.
    let ancestry: Option<(Scene, HashSet<usize>)> = match ancestors {
        None => None,
        Some(anc_name) => {
            let [root] = roots.as_slice() else {
                return Err("--ancestors wants exactly one --root: the hourglass has one waist".to_string());
            };
            let prune_up = forest::Prune {
                depth: depth_up.or(prune.depth),
                max_nodes: max_nodes_up.unwrap_or(prune.max_nodes),
                fanout: fanout_up.or(prune.fanout),
                // A spine names descendants; the ancestor side has its own
                // scissors and no spine to follow.
                expand: None,
            };
            eprintln!("reading {anc_name}");
            let tgraph = BvGraph::with_basename(anc_name)
                .load()
                .map_err(|e| format!("{anc_name}: {e:#}"))?;
            let mut arena = Arena::new();
            let up = forest::build_rooted(&tgraph, &mut arena, &roots, &prune_up)?;
            eprintln!("ancestors: {}", up.summary());
            let arena = forest::lay_out_oriented(arena, up.root, vertical);
            let up_scene = Scene::of(&arena, up.root)?;
            // The root is drawn by the descendant part, so a cut on its
            // ancestor side must reach it there or go unsaid.
            if up.truncated.contains(root) {
                truncated.insert(*root);
            }
            Some((up_scene, up.truncated))
        }
    };

    let mut parts = vec![Part { scene: &scene, truncated: &truncated, flipped: false }];
    if let Some((up_scene, up_truncated)) = &ancestry {
        parts.push(Part { scene: up_scene, truncated: up_truncated, flipped: true });
    }

    let opts = Layout { width, max_height, fill, vertical, labels };
    let page = draw(&parts, &marks, &opts);
    let (w, h) = (page.width(), page.height());
    page.write(out_path)?;

    let drawn = scene.len() + ancestry.as_ref().map_or(0, |(s, _)| s.len() - 1);
    let size = std::fs::metadata(out_path)
        .map(|m| m.len())
        .map_err(|e| format!("{out_path}: {e}"))?;
    eprintln!(
        "{} nodes on a {:.0} by {:.0} pt page, {} bytes written to {out_path}",
        drawn, w, h, size
    );
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

    /// A scene of the forest the other binaries' tests draw, pruned or not.
    fn scene_of(
        n: usize,
        arcs: &[(usize, usize)],
        roots: &[usize],
        prune: &forest::Prune,
    ) -> (Scene, HashSet<usize>) {
        let g = forest::graph_of(n, arcs);
        let mut arena = Arena::new();
        let sampled = forest::build_rooted(&g, &mut arena, roots, prune).unwrap();
        let arena = forest::lay_out(arena, sampled.root);
        (Scene::of(&arena, sampled.root).unwrap(), sampled.truncated)
    }

    /// One unflipped part, no marks, no labels: the plain page the older
    /// tests were written against.
    fn page_of(scene: &Scene, truncated: &HashSet<usize>, w: f64, h: f64, fill: bool) -> Page {
        draw(
            &[Part { scene, truncated, flipped: false }],
            &HashSet::new(),
            &Layout { width: w, max_height: h, fill, vertical: false, labels: false },
        )
    }

    /// The deflated content stream of a written page, as text.
    fn ops_of(page: Page, name: &str) -> String {
        use std::io::Read;
        let path = std::env::temp_dir()
            .join(format!("tree-pdf-ops-{}-{name}.pdf", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        page.write(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).ok();
        let start = bytes.windows(7).position(|w| w == b"stream\n").unwrap() + 7;
        let end = bytes.windows(10).position(|w| w == b"\nendstream").unwrap();
        let mut ops = String::new();
        flate2::read::ZlibDecoder::new(&bytes[start..end])
            .read_to_string(&mut ops)
            .unwrap();
        ops
    }

    /// Five nodes make five circles, four arcs, and a page that hugs them.
    #[test]
    fn a_page_holds_its_subtree() {
        let (scene, truncated) =
            scene_of(5, &[(0, 1), (0, 4), (1, 2), (1, 3)], &[0], &forest::Prune::default());

        let page = page_of(&scene, &truncated, 300.0, 300.0, false);
        assert!(page.width() <= 300.0 && page.height() <= 300.0);

        let path = std::env::temp_dir()
            .join(format!("tree-pdf-page-{}.pdf", std::process::id()));
        let path = path.to_string_lossy().into_owned();
        page.write(&path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF"), "a PDF header");
        assert!(bytes.len() > 256, "a page with something on it");

        std::fs::remove_file(&path).ok();
    }

    /// Under `--fill` the page is the page that was asked for, whatever the
    /// drawing's shape; without it the page hugs the drawing.
    #[test]
    fn fill_takes_the_whole_page() {
        // A star: one level deep, five siblings broad -- the shape that a
        // uniform scale renders as a ribbon.
        let (scene, truncated) =
            scene_of(6, &[(0, 1), (0, 2), (0, 3), (0, 4), (0, 5)], &[0], &forest::Prune::default());

        // Laid out horizontally the star is two levels wide and five siblings
        // tall, so the height binds and the width is what hugs the drawing.
        let hugged = page_of(&scene, &truncated, 400.0, 700.0, false);
        assert!(hugged.width() < 400.0, "a two-level tree does not need the width");

        let filled = page_of(&scene, &truncated, 400.0, 700.0, true);
        assert_eq!((filled.width(), filled.height()), (400.0, 700.0));
    }

    /// The cut frontier is drawn in its own colour: the page of a pruned walk
    /// names [`CUT`] where the page of the whole subtree does not.
    #[test]
    fn the_cut_is_inked_apart() {
        let arcs = [(0, 1), (1, 2), (2, 3), (3, 4)];

        let render = |prune: &forest::Prune, name: &str| {
            let (scene, truncated) = scene_of(5, &arcs, &[0], prune);
            ops_of(page_of(&scene, &truncated, 300.0, 300.0, false), name)
        };

        let whole = render(&forest::Prune::default(), "whole");
        let pruned = render(&forest::Prune { depth: Some(2), ..forest::Prune::default() }, "cut");

        // The warning colour's operands, as `pdf::num` spells them.
        assert!(!whole.contains("0.82 0.24 0.1"), "nothing was cut, so nothing warns");
        assert!(pruned.contains("0.82 0.24 0.1"), "the frontier of the cut is inked apart");
    }

    /// A marked node wears the caller's colour, and an unmarked page carries
    /// no marking ink; labels put the graph ids on the page, and only when
    /// asked.
    #[test]
    fn marks_and_labels_are_opt_in() {
        let (scene, truncated) =
            scene_of(3, &[(0, 1), (1, 2)], &[0], &forest::Prune::default());

        let plain = ops_of(page_of(&scene, &truncated, 300.0, 300.0, false), "plain");
        assert!(!plain.contains("0.16 0.47 0.84"), "no mark was asked for");
        assert!(!plain.contains("Tj"), "no label was asked for");

        let marks: HashSet<usize> = HashSet::from([1]);
        let opts =
            Layout { width: 300.0, max_height: 300.0, fill: false, vertical: false, labels: true };
        let page = draw(&[Part { scene: &scene, truncated: &truncated, flipped: false }], &marks, &opts);
        let ops = ops_of(page, "marked");
        assert!(ops.contains("0.16 0.47 0.84"), "node 1 wears the mark: {ops}");
        assert!(ops.contains("(0) Tj") && ops.contains("(2) Tj"), "every node is labelled: {ops}");
    }

    /// An hourglass draws both trees about one shared root: the root's disc
    /// appears once, every other node once, and the drawing extends to both
    /// sides of the waist.
    #[test]
    fn an_hourglass_meets_at_its_waist() {
        // The chain 0 -> 1 -> 2 and its transpose; the waist is node 1.
        let g = forest::graph_of(3, &[(0, 1), (1, 2)]);
        let t = forest::graph_of(3, &[(1, 0), (2, 1)]);

        let build = |g: &webgraph::prelude::VecGraph| {
            let mut arena = Arena::new();
            let s = forest::build_rooted(g, &mut arena, &[1], &forest::Prune::default()).unwrap();
            let arena = forest::lay_out(arena, s.root);
            (Scene::of(&arena, s.root).unwrap(), s.truncated)
        };
        let (down, down_cut) = build(&g);
        let (up, up_cut) = build(&t);
        assert_eq!((down.len(), up.len()), (2, 2), "each side is the waist and one more");

        let one_sided = page_of(&down, &down_cut, 300.0, 300.0, false);
        let parts = [
            Part { scene: &down, truncated: &down_cut, flipped: false },
            Part { scene: &up, truncated: &up_cut, flipped: true },
        ];
        let opts =
            Layout { width: 300.0, max_height: 300.0, fill: false, vertical: false, labels: false };
        let both = draw(&parts, &HashSet::new(), &opts);
        // The width binds in both cases, so the wider drawing shows up as a
        // smaller scale: the page that hugs it is shorter.
        assert!(both.height() < one_sided.height(), "the ancestry widens the drawing");

        let ops = ops_of(both, "hourglass");
        let circles = ops.matches("c h").count();
        assert_eq!(circles, 3, "three nodes, and the waist drawn once: {ops}");
    }
}
