//! # The blocks of a neighbourhood, written as a page
//!
//! The sibling of `tree-pdf` whose unit is the **block** K(I, O) of the
//! payments graph — one transaction, its inputs and its outputs, every arc
//! between them — rather than the node:
//!
//! ```text
//! block-pdf <graph-basename> <transpose-basename> --root <id>[,<id>...] -o <file>
//!           [--depth <n>] [--max-blocks <n>] [--max-nodes <n>]
//!           [--fanout <n>] [--fanout-in <n>] [--fanout-out <n>]
//!           [--seed producing|consuming]
//!           [--horizontal] [--fill] [--width <pt>] [--max-height <pt>]
//!           [--mark <id>[,<id>...]] [--labels [compact]] [--label-base <id>]
//!           [--dash-cross] [--no-check]
//! ```
//!
//! A spanning tree keeps one arc per node, so of a block with two inputs it
//! draws one input feeding everything and the other a bare leaf, its arcs
//! dropped and counted; three arcs in four of this graph go that way, and the
//! picture structurally hides that the graph is made of complete bipartite
//! pieces.  This binary walks the graph block by block — [`blocks`] says how —
//! and draws each one it admits as a gadget: its inputs on one row, its
//! outputs on the row below, all |I|·|O| arcs between them.  A node is drawn
//! once, so consecutive blocks chain through the nodes they share; nothing is
//! ever dropped, and the report says `dropped=0` so that a caption can quote
//! it.
//!
//! The transpose is mandatory: a block cannot be found without knowing what
//! points at a node, and that is what the transpose's successors are.
//!
//! # The ink
//!
//! The same ink as `tree-pdf`, from the same file, so that a reader carries
//! one legend across the figures: a filled disc has drawn out-arcs, a hollow
//! grey ring is a sink, a hollow ring in the warning colour is a *frontier*
//! — its out-arcs exist and its consuming block is not on the page, whether
//! the depth stopped there or a budget did — and a marked node wears the
//! caller's blue over anything else.  Three things are this binary's own.  An
//! *outside input* — an input no drawn block produced — sits on the row above
//! the outputs it feeds like every other input, beside the block that fed the
//! others, and carries a short dashed stub from the side it came from when
//! something did produce it, and no stub when it is a true source.  A run of
//! inputs or outputs the fanout hid is three dots in the warning colour at a
//! slot of their own, with the hidden count at their side under `--labels`.
//! And the one arc between two drawn nodes that no drawn block owns — from a
//! frontier output to an outside input, an arc of a block the scissors cut —
//! is dotted in the warning colour and counted as *stray*.
//!
//! An arc entering a block from a drawn block other than the one above it is
//! a *cross* arc.  It is inked exactly like every other arc — two arcs of one
//! block must never look different because of a layout accident — and
//! counted, and `--dash-cross` dashes it for a reader who wants to see the
//! quotient's non-tree edges.  It is also the one kind of arc that can skip a
//! row, so the layout counts the arcs that cross a row over a node they do
//! not end at and reports them as `over_nodes=`; a caption can quote the 0.
//!
//! # The labels
//!
//! A label is written *beside* its node, on the row, in the direction the
//! row runs: arcs meet a row only at its nodes, so the paper between two
//! nodes of a row is the one place near a node that no arc of the gadget can
//! cross, whereas above or below the node is exactly where its arcs go.  A
//! row label under `--labels compact` sits beside the row's last slot for the
//! same reason, and the hidden count of an ellipsis on the outward side of
//! the row, where the dots have no arcs of their own.  The page keeps a
//! margin on that side wide enough for the longest label.
//!
//! # The page
//!
//! Sized, scaled and written as `tree-pdf` does it: one scale that hugs, or
//! `--fill` for two scales to the page asked for; the same PDF writer, the
//! same nothing to install.

use std::collections::HashSet;
use std::env;
use std::process::ExitCode;

use webgraph::prelude::BvGraph;

#[path = "tree/blocks.rs"]
mod blocks;
#[path = "../camera.rs"]
mod camera;
#[path = "tree/forest.rs"]
mod forest;
#[path = "tree/ink.rs"]
mod ink;
#[path = "tree/pdf.rs"]
mod pdf;

use blocks::{ArcKind, BlockPrune, BlockScene, Placed, Producer, Role, Row, Seed};
use ink::*;
use pdf::Page;

const USAGE: &str = "usage: block-pdf <graph-basename> <transpose-basename> --root <id>[,<id>...] -o <file>\n\
       [--depth <n>] [--max-blocks <n>] [--max-nodes <n>]\n\
       [--fanout <n>] [--fanout-in <n>] [--fanout-out <n>]\n\
       [--seed producing|consuming]\n\
       [--horizontal] [--fill] [--width <pt>] [--max-height <pt>]\n\
       [--mark <id>[,<id>...]] [--labels [compact]] [--label-base <id>]\n\
       [--dash-cross] [--no-check]";

/// How much of a node unit an outside input's stub reaches back, along the
/// depth axis: past the box's edge, short of the row above.
const STUB: f64 = 0.6;

/// The largest a label is set, in points, and the width of a Helvetica digit
/// in ems — all a graph id is made of: enough metrics to place a label, and
/// to keep a margin for it, without carrying a font.
const LABEL_SIZE: f64 = 7.0;
const DIGIT: f64 = 0.556;

/// What `--labels` writes: nothing, every node's id, or one label per row.
#[derive(Clone, Copy, PartialEq)]
enum Labels {
    None,
    Each,
    /// The first id of each row and, for a contiguous output row, the last:
    /// one label a row instead of one a node, for the hubs where per-node
    /// labels would overprint.
    Compact,
}

/// How the page is settled: everything about the drawing that is not a block.
struct Layout {
    width: f64,
    max_height: f64,
    fill: bool,
    vertical: bool,
    labels: Labels,
    /// Printed ids are `id - base`, for nine-digit ids at seven points.
    label_base: Option<i64>,
    dash_cross: bool,
}

/// Where the baseline of a label of `width` points starts when it is set
/// beside the mark at `(x, y)` that reaches `gap` points from its centre: to
/// the right of it when depth runs down the page, under it when depth runs
/// across.
fn beside(vertical: bool, x: f64, y: f64, gap: f64, width: f64, size: f64) -> (f64, f64) {
    if vertical {
        (x + gap + 1.0, y - 0.36 * size)
    } else {
        (x - width / 2.0, y - gap - 1.0 - 0.72 * size)
    }
}

/// Where a label starts when it is set on the outward side of a row's mark:
/// the past's side for an input row, the future's for an output row.
fn outward(vertical: bool, row: Row, x: f64, y: f64, gap: f64, width: f64, size: f64) -> (f64, f64) {
    match (vertical, row) {
        (true, Row::Input) => (x - width / 2.0, y + gap + 1.0),
        (true, Row::Output) => (x - width / 2.0, y - gap - 1.0 - 0.72 * size),
        (false, Row::Input) => (x - gap - 1.0 - width, y - 0.36 * size),
        (false, Row::Output) => (x + gap + 1.0, y - 0.36 * size),
    }
}

/// Draws a laid-out block scene as one page, `marks` being the ids the
/// caller asked to have inked apart.
///
/// The scale is settled as `tree-pdf` settles it — one scale that hugs, or
/// two that fill — and the node radius keys on the tighter of the two
/// spacings exactly as there: rows sit a unit apart along depth and slots two
/// units apart along breadth.
fn draw(scene: &BlockScene, marks: &HashSet<usize>, opts: &Layout) -> Page {
    let bounds = scene.bounds;
    let id = |g: usize| match opts.label_base {
        None => g.to_string(),
        Some(base) => (g as i64 - base).to_string(),
    };

    // What a row says under compact labels: the block's range and side, or
    // for an input row what is on it out of what the block has.
    let row_text = |bi: usize, row: Row| -> Option<String> {
        let b = &scene.blocks[bi];
        match row {
            Row::Output => {
                let n = b.outputs.len();
                Some(if n == 1 {
                    id(b.first_out)
                } else {
                    format!("{}-{} ({})", id(b.outputs[0]), id(b.outputs[n - 1]), n)
                })
            }
            Row::Input => {
                let mut shown: Vec<usize> = scene
                    .nodes
                    .iter()
                    .filter(|p| matches!(p.role, Role::Outside { block, .. } if block as usize == bi))
                    .map(|p| p.graph)
                    .collect();
                if shown.is_empty() {
                    return None;
                }
                shown.sort_unstable();
                let n = b.inputs.len();
                // The kept inputs not on this row are drawn where their
                // producers put them; the hidden ones are on this row's
                // ellipsis, and so count as the row's.
                let elsewhere = b.kept_in.count(n) - shown.len();
                let here = n - elsewhere;
                let span = if shown.len() > 1 {
                    format!("{} ... {}", id(shown[0]), id(shown[shown.len() - 1]))
                } else {
                    id(shown[0])
                };
                Some(if n == 1 {
                    span
                } else if here == n {
                    format!("{span} ({n})")
                } else {
                    format!("{span} ({here} of {n})")
                })
            }
        }
    };

    // The longest label hangs out of the drawing on the breadth-positive
    // side; the page keeps room for it there, and a little along depth for
    // the hidden counts on the outward side of a cut row.
    let longest = match opts.labels {
        Labels::None => 0,
        Labels::Each => scene.nodes.iter().map(|p| id(p.graph).len()).max().unwrap_or(0),
        Labels::Compact => (0..scene.blocks.len())
            .flat_map(|bi| [Row::Input, Row::Output].into_iter().filter_map(move |row| row_text(bi, row)))
            .map(|t| t.len())
            .max()
            .unwrap_or(0),
    };
    let overhang =
        if longest > 0 { DIGIT * LABEL_SIZE * longest as f64 + MAX_RADIUS + 1.0 } else { 0.0 };
    let along_depth = if opts.labels != Labels::None && !scene.ellipses.is_empty() {
        0.72 * LABEL_SIZE + 3.0
    } else {
        0.0
    };
    let (ml, mr, mt, mb) = if opts.vertical {
        (MARGIN, MARGIN + overhang, MARGIN + along_depth, MARGIN + along_depth)
    } else {
        (MARGIN + along_depth, MARGIN + along_depth, MARGIN, MARGIN + overhang)
    };

    let (scale_x, scale_y, page_w, page_h) = if opts.fill {
        (
            (opts.width - ml - mr) / bounds.width(),
            (opts.max_height - mt - mb) / bounds.height(),
            opts.width,
            opts.max_height,
        )
    } else {
        let scale =
            ((opts.width - ml - mr) / bounds.width()).min((opts.max_height - mt - mb) / bounds.height());
        (scale, scale, bounds.width() * scale + ml + mr, bounds.height() * scale + mt + mb)
    };

    // PDF's y runs up the page and the layout's runs down the drawing, so the
    // flip lives here, in the one mapping every mark goes through.
    let at = |x: f64, y: f64| {
        (ml + (x - bounds.min_x) * scale_x, page_h - mt - (y - bounds.min_y) * scale_y)
    };
    let node_at = |p: &Placed| at(p.x, p.y);

    let (depth_scale, breadth_scale) = if opts.vertical {
        (scale_y, scale_x)
    } else {
        (scale_x, scale_y)
    };
    let spacing = depth_scale.min(2.0 * breadth_scale);
    let radius = (0.5 * spacing * INK).clamp(MIN_RADIUS, MAX_RADIUS);
    let hollow = radius >= MIN_HOLLOW;
    // An ellipsis: three dots along the row, far enough apart to stay three.
    let dot = radius / 3.0;
    let step = (0.5 * breadth_scale).max(2.0 * dot);

    // The depth-negative direction on the page: up when vertical, left when
    // not.  Stubs point that way.
    let (back_x, back_y) = if opts.vertical { (0.0, 1.0) } else { (-1.0, 0.0) };

    let mut page = Page::new(page_w, page_h);

    // The arcs first, behind everything: tree and cross arcs in one grey,
    // solid --- a cross arc dashed only when asked --- then the stubs, dashed,
    // then the stray arcs dotted in the cut's colour.
    page.set_stroke(LINK);
    page.set_line_width((radius / 3.0).clamp(0.16, 1.2));
    let batch = |page: &mut Page, kind: ArcKind| -> usize {
        let mut in_path = 0;
        let mut total = 0;
        for a in scene.arcs.iter().filter(|a| a.kind == kind) {
            let (x0, y0) = node_at(&scene.nodes[a.from as usize]);
            let (x1, y1) = node_at(&scene.nodes[a.to as usize]);
            page.segment(x0, y0, x1, y1);
            in_path += 1;
            total += 1;
            if in_path == BATCH {
                page.stroke();
                in_path = 0;
            }
        }
        if in_path > 0 {
            page.stroke();
        }
        total
    };
    batch(&mut page, ArcKind::Tree);
    if scene.arcs.iter().any(|a| a.kind == ArcKind::Cross) {
        if opts.dash_cross {
            page.set_dash(2.0 * radius, 1.5 * radius);
        }
        batch(&mut page, ArcKind::Cross);
        if opts.dash_cross {
            page.solid();
        }
    }

    let stubbed: Vec<&Placed> = scene
        .nodes
        .iter()
        .filter(|p| matches!(p.role, Role::Outside { producer: Producer::Undrawn, .. }))
        .collect();
    if !stubbed.is_empty() {
        page.set_dash(2.0 * radius, 1.5 * radius);
        let mut in_path = 0;
        for p in &stubbed {
            let (x, y) = node_at(p);
            page.segment(x + back_x * STUB * depth_scale, y + back_y * STUB * depth_scale, x, y);
            in_path += 1;
            if in_path == BATCH {
                page.stroke();
                in_path = 0;
            }
        }
        if in_path > 0 {
            page.stroke();
        }
        page.solid();
    }

    if scene.arcs.iter().any(|a| a.kind == ArcKind::Stray) {
        page.set_stroke(CUT);
        page.set_dash(0.5 * radius, 1.5 * radius);
        batch(&mut page, ArcKind::Stray);
        page.solid();
    }

    if !scene.ellipses.is_empty() {
        page.set_fill(CUT);
        let (sx, sy) = if opts.vertical { (1.0, 0.0) } else { (0.0, 1.0) };
        let mut in_path = 0;
        for e in &scene.ellipses {
            let (x, y) = at(e.x, e.y);
            for k in [-1.0, 0.0, 1.0] {
                page.circle(x + sx * k * step, y + sy * k * step, dot);
            }
            in_path += 3;
            if in_path >= BATCH {
                page.fill();
                in_path = 0;
            }
        }
        if in_path > 0 {
            page.fill();
        }
    }

    // The nodes, in the six classes `tree-pdf` sorts them into: filled says
    // the node has drawn out-arcs, hollow says it has none; the warning colour
    // says the graph goes on where the drawing does not; the caller's mark
    // outranks both.
    let class = |p: &Placed| {
        let leaf = usize::from(!p.consumed);
        if marks.contains(&p.graph) {
            4 + leaf
        } else if p.outdeg > 0 && !p.consumed {
            2 + leaf
        } else {
            leaf
        }
    };
    for wanted in 0..=5 {
        if !scene.nodes.iter().any(|p| class(p) == wanted) {
            continue;
        }
        let colour = [INNER, LEAF, CUT, CUT, MARK, MARK][wanted];
        let rings = hollow && wanted % 2 == 1;
        if rings {
            page.set_fill(PAPER);
            page.set_stroke(colour);
            page.set_line_width((radius / 3.0).clamp(0.3, 1.6));
        } else {
            page.set_fill(colour);
        }
        let mut in_path = 0;
        for p in scene.nodes.iter().filter(|p| class(p) == wanted) {
            let (x, y) = node_at(p);
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
        if in_path > 0 {
            if rings {
                page.fill_and_stroke();
            } else {
                page.fill();
            }
        }
    }

    // The labels last, over everything, beside what they name (the module's
    // third section); the size follows the circles and never drops below
    // legibility.
    if opts.labels != Labels::None {
        let size = (radius * 2.2).clamp(3.5, LABEL_SIZE);
        let digit = DIGIT * size;
        page.set_fill(LABEL);
        let write_beside = |page: &mut Page, x: f64, y: f64, gap: f64, text: &str| {
            let (lx, ly) = beside(opts.vertical, x, y, gap, digit * text.len() as f64, size);
            page.text(lx, ly, size, text);
        };
        let write_outward = |page: &mut Page, row: Row, x: f64, y: f64, gap: f64, text: &str| {
            let (lx, ly) = outward(opts.vertical, row, x, y, gap, digit * text.len() as f64, size);
            page.text(lx, ly, size, text);
        };
        for e in &scene.ellipses {
            let (x, y) = at(e.x, e.y);
            write_outward(&mut page, e.row, x, y, dot, &format!("+{}", e.hidden));
        }
        match opts.labels {
            Labels::Each => {
                for p in &scene.nodes {
                    let (x, y) = node_at(p);
                    write_beside(&mut page, x, y, radius, &id(p.graph));
                }
            }
            Labels::Compact => {
                // One label a row, beside its last slot --- the node or the
                // ellipsis furthest along the row.
                for (bi, _) in scene.blocks.iter().enumerate() {
                    for row in [Row::Input, Row::Output] {
                        let Some(text) = row_text(bi, row) else { continue };
                        // The row's marks as (along the row, x, y, reach).
                        let along = |x: f64, y: f64| if opts.vertical { x } else { -y };
                        let mut end: Option<(f64, f64, f64, f64)> = None;
                        let mut consider = |x: f64, y: f64, gap: f64| {
                            if end.is_none_or(|(a, ..)| along(x, y) > a) {
                                end = Some((along(x, y), x, y, gap));
                            }
                        };
                        for p in scene.nodes.iter().filter(|p| match p.role {
                            Role::Output { block } => row == Row::Output && block as usize == bi,
                            Role::Outside { block, .. } => row == Row::Input && block as usize == bi,
                        }) {
                            let (x, y) = node_at(p);
                            consider(x, y, radius);
                        }
                        for e in scene.ellipses.iter().filter(|e| e.block as usize == bi && e.row == row) {
                            let (x, y) = at(e.x, e.y);
                            consider(x, y, step + dot);
                        }
                        if let Some((_, x, y, gap)) = end {
                            write_beside(&mut page, x, y, gap, &text);
                        }
                    }
                }
            }
            Labels::None => {}
        }
    }

    page
}

fn run() -> Result<(), String> {
    let argv: Vec<String> = env::args().skip(1).collect();

    let mut basenames: Vec<&str> = Vec::new();
    let mut out_path: Option<&str> = None;
    let mut roots: Vec<usize> = Vec::new();
    let mut prune = BlockPrune { max_nodes: DEFAULT_MAX_NODES, ..BlockPrune::default() };
    let mut seed = Seed::Producing;
    let mut width = DEFAULT_WIDTH;
    let mut max_height = DEFAULT_MAX_HEIGHT;
    let mut vertical = true;
    let mut fill = false;
    let mut labels = Labels::None;
    let mut label_base: Option<i64> = None;
    let mut dash_cross = false;
    let mut check = true;
    let mut marks: HashSet<usize> = HashSet::new();

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
                    roots.push(
                        part.parse::<usize>().map_err(|_| format!("--root {part}: not a node id"))?,
                    );
                }
            }
            "--depth" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--depth wants a number\n{USAGE}"))?;
                prune.depth =
                    Some(v.parse::<usize>().map_err(|_| format!("--depth {v}: not a number"))?);
            }
            flag @ ("--max-blocks" | "--max-nodes") => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("{flag} wants a number\n{USAGE}"))?;
                let n = v
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("{flag} {v}: wants a positive number"))?;
                if flag == "--max-blocks" {
                    prune.max_blocks = n;
                } else {
                    prune.max_nodes = n;
                }
            }
            flag @ ("--fanout" | "--fanout-in" | "--fanout-out") => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("{flag} wants a number\n{USAGE}"))?;
                let n = v
                    .parse::<usize>()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| format!("{flag} {v}: wants a positive number"))?;
                // `--fanout` sets both sides; a side's own flag overrides it
                // whichever order they come in.
                match flag {
                    "--fanout-in" => prune.fanout_in = Some(n),
                    "--fanout-out" => prune.fanout_out = Some(n),
                    _ => {
                        prune.fanout_in.get_or_insert(n);
                        prune.fanout_out.get_or_insert(n);
                    }
                }
            }
            "--seed" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--seed wants producing or consuming\n{USAGE}"))?;
                seed = match v.as_str() {
                    "producing" => Seed::Producing,
                    "consuming" => Seed::Consuming,
                    _ => return Err(format!("--seed {v}: producing or consuming\n{USAGE}")),
                };
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
            // Depth across the page instead of down it: what a chain of
            // K(1,k) blocks wants.  Down is the default here, the past above
            // the event, unlike `tree-pdf`, whose subjects are chains.
            "--horizontal" => vertical = false,
            "--vertical" => vertical = true,
            "--fill" => fill = true,
            "--labels" => {
                labels = Labels::Each;
                if argv.get(i + 1).is_some_and(|v| v == "compact") {
                    labels = Labels::Compact;
                    i += 1;
                }
            }
            "--label-base" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--label-base wants a node id\n{USAGE}"))?;
                label_base =
                    Some(v.parse::<i64>().map_err(|_| format!("--label-base {v}: not a node id"))?);
            }
            "--mark" => {
                i += 1;
                let v = argv.get(i).ok_or_else(|| format!("--mark wants a node id\n{USAGE}"))?;
                for part in v.split(',') {
                    marks.insert(
                        part.parse::<usize>().map_err(|_| format!("--mark {part}: not a node id"))?,
                    );
                }
            }
            "--dash-cross" => dash_cross = true,
            "--no-check" => check = false,
            "-h" | "--help" => return Err(USAGE.to_string()),
            other if other.starts_with('-') => {
                return Err(format!("unknown flag {other}\n{USAGE}"))
            }
            other => basenames.push(other),
        }
        i += 1;
    }

    let [graph_name, transpose_name] = basenames.as_slice() else {
        return Err(format!(
            "expected a graph basename and its transpose's, got {}\n{USAGE}",
            basenames.len()
        ));
    };
    let Some(out_path) = out_path else {
        return Err(format!("-o is required\n{USAGE}"));
    };
    if roots.is_empty() {
        return Err(format!("--root is required\n{USAGE}"));
    }

    eprintln!("reading {graph_name}");
    let pg = BvGraph::with_basename(graph_name)
        .load()
        .map_err(|e| format!("{graph_name}: {e:#}"))?;
    eprintln!("reading {transpose_name}");
    let pgt = BvGraph::with_basename(transpose_name)
        .load()
        .map_err(|e| format!("{transpose_name}: {e:#}"))?;

    let walked = blocks::walk(&pg, &pgt, &roots, &prune, seed, check)?;
    for line in &walked.seeds {
        eprintln!("{line}");
    }
    eprintln!("{}", prune.summary());
    let scene = blocks::lay_out_blocks(&pg, &pgt, walked, vertical)?;
    eprintln!("{}", scene.report.lines());

    let opts = Layout { width, max_height, fill, vertical, labels, label_base, dash_cross };
    let page = draw(&scene, &marks, &opts);
    let (w, h) = (page.width(), page.height());
    page.write(out_path)?;

    let size = std::fs::metadata(out_path)
        .map(|m| m.len())
        .map_err(|e| format!("{out_path}: {e}"))?;
    eprintln!(
        "{} nodes, {} arcs on a {:.0} by {:.0} pt page, {} bytes written to {out_path}",
        scene.report.nodes_drawn, scene.report.arcs_drawn, w, h, size
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

    fn scene_of(n: usize, arcs: &[(usize, usize)], roots: &[usize], prune: &BlockPrune) -> BlockScene {
        let pg = forest::graph_of(n, arcs);
        let pgt = blocks::transpose_of(n, arcs);
        let w = blocks::walk(&pg, &pgt, roots, prune, Seed::Producing, true).unwrap();
        blocks::lay_out_blocks(&pg, &pgt, w, true).unwrap()
    }

    fn layout(labels: Labels, dash_cross: bool) -> Layout {
        Layout {
            width: 300.0,
            max_height: 300.0,
            fill: false,
            vertical: true,
            labels,
            label_base: None,
            dash_cross,
        }
    }

    /// The deflated content stream of a written page, as text.
    fn ops_of(page: Page, name: &str) -> String {
        use std::io::Read;
        let path = std::env::temp_dir()
            .join(format!("block-pdf-ops-{}-{name}.pdf", std::process::id()));
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

    const CHAIN: [(usize, usize); 8] = [(0, 1), (0, 2), (1, 3), (1, 4), (2, 3), (2, 4), (3, 5), (4, 5)];

    /// U7, the page half: a chain with nothing dashed carries no dash
    /// operator; a diamond's cross arc is dashed only when asked; a source
    /// gets no stub and a produced outside input gets one.
    #[test]
    fn dashes_only_where_something_is_dashed() {
        let scene = scene_of(6, &CHAIN, &[1], &BlockPrune::default());
        let ops = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::None, false)), "chain");
        assert!(!ops.contains(" d\n"), "nothing to dash: {ops}");
        assert_eq!(ops.matches("c h").count(), 6, "six nodes: {ops}");
        assert_eq!(ops.matches(" l\n").count(), 8, "eight arcs");
        assert!(!ops.contains("0.82 0.24 0.1"), "nothing was cut");

        let diamond = [(0, 1), (0, 2), (1, 3), (2, 4), (3, 5), (4, 5)];
        let scene = scene_of(6, &diamond, &[1], &BlockPrune::default());
        let solid = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::None, false)), "solid");
        assert!(!solid.contains(" d\n"), "a cross arc is solid by default: {solid}");
        let dashed = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::None, true)), "dashed");
        assert_eq!(dashed.matches("] 0 d\n").count(), 2, "one dashed batch, then solid: {dashed}");

        // Rooted one block down, the seed's inputs come from an undrawn
        // block: stubs, dashed, and the frontier colour nowhere since
        // nothing leads on from the drawn nodes.
        let scene = scene_of(6, &CHAIN, &[3], &BlockPrune { depth: Some(0), ..BlockPrune::default() });
        assert_eq!(scene.report.outside_from_undrawn, 2);
        let ops = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::None, false)), "stub");
        assert_eq!(ops.matches("] 0 d\n").count(), 2, "the stubs' dash and its undoing: {ops}");
        assert!(ops.contains("0.82 0.24 0.1"), "3 and 4 lead on to a block the depth stopped");
    }

    /// Labels are opt-in, name every node when asked, and one row at a time
    /// in compact mode; a mark wears the caller's colour.
    #[test]
    fn labels_and_marks() {
        let scene = scene_of(6, &CHAIN, &[1], &BlockPrune::default());

        let plain = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::None, false)), "plain");
        assert!(!plain.contains("Tj") && !plain.contains("0.16 0.47 0.84"));

        let marks = HashSet::from([3]);
        let each = ops_of(draw(&scene, &marks, &layout(Labels::Each, false)), "each");
        for g in 0..6 {
            assert!(each.contains(&format!("({g}) Tj")), "{g} is labelled: {each}");
        }
        assert!(each.contains("0.16 0.47 0.84"), "node 3 wears the mark");

        let compact = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::Compact, false)), "compact");
        assert!(compact.contains("(0) Tj"), "{compact}");
        assert!(compact.contains("(1-2 \\(2\\)) Tj"), "one label for the output row: {compact}");
        assert!(compact.contains("(3-4 \\(2\\)) Tj"), "{compact}");
        assert!(compact.contains("(5) Tj"));
        assert_eq!(compact.matches("Tj").count(), 4, "one label a row: {compact}");

        // A row that shows one input of three says so; the count in
        // parentheses is always the block's side, never the row's.
        let merge = [(0, 1), (0, 2), (1, 3), (2, 4), (2, 5), (3, 7), (4, 7), (6, 7)];
        let scene = scene_of(8, &merge, &[1], &BlockPrune::default());
        let compact = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::Compact, false)), "merge");
        assert!(compact.contains("(6 \\(1 of 3\\)) Tj"), "{compact}");
        let scene = scene_of(8, &merge, &[7], &BlockPrune { depth: Some(0), ..BlockPrune::default() });
        let compact = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::Compact, false)), "k31");
        assert!(compact.contains("(3 ... 6 \\(3\\)) Tj"), "{compact}");
    }

    /// A label sits beside its node along the row --- right of it when depth
    /// runs down, under it when depth runs across --- and never on the
    /// node's column, where its arcs are.
    #[test]
    fn labels_sit_beside_their_nodes() {
        let (x, y, size) = (100.0, 200.0, 7.0);
        let (lx, ly) = beside(true, x, y, 5.0, 20.0, size);
        assert_eq!(lx, 106.0, "past the circle");
        assert!(ly < y && ly > y - size, "the baseline a little under the centre line");
        let (lx, ly) = beside(false, x, y, 5.0, 20.0, size);
        assert_eq!(lx, 90.0, "centred");
        assert!(ly < y - 5.0 - size * 0.72, "under the circle");

        // Outward of an input row is the past's side: up when vertical, left
        // when horizontal.
        assert!(outward(true, Row::Input, x, y, 2.0, 20.0, size).1 > y);
        assert!(outward(true, Row::Output, x, y, 2.0, 20.0, size).1 < y);
        assert!(outward(false, Row::Input, x, y, 2.0, 20.0, size).0 < x - 20.0);
        assert!(outward(false, Row::Output, x, y, 2.0, 20.0, size).0 > x);

        // On the page: every node's label starts right of that node's
        // circle, on its own row.
        let scene = scene_of(6, &CHAIN, &[1], &BlockPrune::default());
        let ops = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::Each, false)), "beside");
        // A circle's path starts at its rightmost point, (cx + r, cy).
        let circles: Vec<(f64, f64)> = ops
            .lines()
            .filter(|l| l.ends_with(" c h"))
            .map(|l| {
                let v: Vec<f64> = l.split(' ').take(2).map(|s| s.parse().unwrap()).collect();
                (v[0], v[1])
            })
            .collect();
        assert_eq!(circles.len(), 6);
        let labels: Vec<(f64, f64)> = ops
            .lines()
            .filter(|l| l.starts_with("BT"))
            .map(|l| {
                let v: Vec<&str> = l.split(' ').collect();
                (v[4].parse().unwrap(), v[5].parse().unwrap())
            })
            .collect();
        assert_eq!(labels.len(), 6);
        for (lx, ly) in labels {
            assert!(
                circles.iter().any(|&(rx, cy)| (cy - ly - 0.36 * 7.0).abs() < 0.05 && (lx - rx - 1.0).abs() < 0.05),
                "label at ({lx}, {ly}) starts one point right of a circle: {circles:?}"
            );
        }
    }

    /// A fan cut by the fanout draws its ellipsis in the cut's colour and
    /// labels it with the hidden count.
    #[test]
    fn an_ellipsis_says_how_many() {
        let arcs: Vec<(usize, usize)> = (1..=9).map(|v| (0, v)).collect();
        let prune = BlockPrune { fanout_out: Some(4), ..BlockPrune::default() };
        let scene = scene_of(10, &arcs, &[1], &prune);
        let ops = ops_of(draw(&scene, &HashSet::new(), &layout(Labels::Each, false)), "fan");
        assert!(ops.contains("0.82 0.24 0.1"), "the ellipsis is in the cut's colour");
        assert!(ops.contains("(+5) Tj"), "{ops}");
        assert_eq!(ops.matches("c h").count(), 5 + 3, "five nodes and three dots");
    }
}
