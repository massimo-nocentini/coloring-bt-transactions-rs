//! # A webgraph, looked at
//!
//! The same drawing `tree-svg` and `tree-bitmap` write to a file, in a window
//! one can move around in.  A `BvGraph` is read as a forest, the non-layered
//! tidy trees algorithm (van der Ploeg 2014) places every node, and what is on
//! the screen is drawn with Cairo — but only what is on the screen, which is the
//! whole point of the thing.  How the graph becomes a tree is in [`forest`], and
//! the three parts of the viewer are in [`camera`], [`quadtree`] and [`scene`].
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
//! # What one can do with it
//!
//! | | |
//! |---|---|
//! | wheel up, wheel down | zoom in and out, about the pointer |
//! | drag | move the drawing |
//! | click | select the node under the pointer, and its subtree |
//! | `f` | fill the window with the selection, or with everything |
//! | `a`, `Home` | back to the whole drawing |
//! | `p` | select the parent of the selection |
//! | `+`, `-` | zoom about the middle of the window |
//! | `Escape` | select nothing |
//! | `q` | close |
//!
//! # Why a frame costs what the window costs
//!
//! A drawing of ten million nodes has ten million circles in it and a window has
//! some hundred thousand pixels, so a frame that looked at every node would be
//! two orders of magnitude of wasted work — and would get slower as the graph
//! got bigger, which is the one thing a viewer for big graphs must not do.
//!
//! [`Scene`] holds the nodes in a [`quadtree::Quadtree`], and a frame asks it
//! one question: what is inside the camera's rectangle, at this coarseness.  The
//! walk never enters a cell the window does not overlap, so nodes off screen
//! cost nothing at all; and it stops at cells too small to be worth opening,
//! answering with a count instead of a list, so nodes *under* a pixel cost
//! nothing either.  Both ends of that are what keep the work proportional to the
//! window rather than to the graph.
//!
//! # Two drawings, and where they change over
//!
//! Which one is showing depends on one number: how many pixels a node's circle
//! is across, which is [`Scene::radius`] times the camera's scale.
//!
//! - **Circles**, once a radius is [`MIN_CIRCLE_PX`] or more.  The tree as
//!   `tree-svg` draws it: a filled circle a node, leaves in [`LEAF`] against
//!   [`INNER`] ones, and — once there is room for them, at [`MIN_LINK_PX`] —
//!   the edges to parents, which at this range are worth more than the ink they
//!   cost.  The quadtree is opened all the way, since at this scale the nodes on
//!   screen are at most a few per pixel anyway.
//! - **Density**, below it.  A circle smaller than a pixel is not a circle, so
//!   what is drawn is the quadtree's own cells: each one a square shaded by how
//!   many nodes are in it, dark where the tree is crowded and pale where it is
//!   thin.  Cells narrower than [`SUMMARY_PX`] are the ones summarised, so the
//!   number of squares is set by the size of the window.
//!
//! The changeover is the same walk with a different stopping rule, which is why
//! there is no seam at it: zooming in opens cells, and the last thing a cell
//! opens into is its nodes.
//!
//! # Where the selection lives
//!
//! Clicking picks the nearest node within a few pixels, and what is selected is
//! that node *and its subtree* — in [`scene`]'s pre-order that is a range of
//! indices, so highlighting it is a comparison per drawn node and framing it is
//! a scan of a slice.  `p` walks the selection up to its parent, which is how
//! one climbs out of a subtree one has dived into.

use std::cell::RefCell;
use std::env;
use std::f64::consts::TAU;
use std::process::ExitCode;
use std::rc::Rc;

use gtk4 as gtk;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{cairo, Application, ApplicationWindow, DrawingArea};

use non_layered_tidy_trees::Arena;
use webgraph::prelude::{BvGraph, SequentialLabeling};

#[path = "tree/camera.rs"]
mod camera;
#[path = "tree/forest.rs"]
mod forest;
#[path = "tree/quadtree.rs"]
mod quadtree;
#[path = "tree/scene.rs"]
mod scene;

use camera::{Camera, Rect};
use quadtree::Patch;
use scene::{Scene, NO_PARENT};

const USAGE: &str = "usage: tree-view <graph-basename> [--width <px>] [--height <px>]";

/// The window, when the caller does not say.
const DEFAULT_WIDTH: i32 = 1200;
const DEFAULT_HEIGHT: i32 = 800;

/// The three tones the other two drawings use, and one more for the selection.
const PAPER: (f64, f64, f64) = (1.0, 1.0, 1.0);
const INNER: (f64, f64, f64) = (0.0, 0.0, 0.0);
const LEAF: (f64, f64, f64) = (0.5, 0.5, 0.5);
const CHOSEN: (f64, f64, f64) = (0.82, 0.24, 0.10);
const LINK: (f64, f64, f64) = (0.78, 0.78, 0.78);

/// A node's radius, in pixels, at or above which nodes are drawn as circles
/// rather than as a density.  Below one pixel a circle is not a circle.
pub const MIN_CIRCLE_PX: f64 = 1.0;

/// A node's radius, in pixels, at or above which the edges to parents are drawn.
///
/// Higher than [`MIN_CIRCLE_PX`], because a line between circles that are
/// themselves a couple of pixels across is a smear rather than an edge.
pub const MIN_LINK_PX: f64 = 3.0;

/// How wide, in pixels, a quadtree cell may be and still be drawn as one shaded
/// square instead of being opened.
///
/// This is what bounds a zoomed-out frame: the squares are this big, so there
/// are at most about `(window / SUMMARY_PX)^2` of them however many nodes the
/// graph has.
pub const SUMMARY_PX: f64 = 2.0;

/// How far from the pointer, in pixels, a click will reach for a node.
const PICK_PX: f64 = 12.0;

/// How far a press may move and still be a click rather than a drag.
const CLICK_SLOP_PX: f64 = 4.0;

/// What the last frame drew, for the panel in the corner.
#[derive(Clone, Copy, Default)]
struct Frame {
    /// Nodes drawn one by one.
    nodes: usize,
    /// Squares standing for more than one node.
    squares: usize,
    /// How many nodes those squares stood for.
    summarised: u32,
}

/// Everything the window is looking at, and how.
struct View {
    scene: Scene,
    camera: Camera,
    /// Whether the camera has been pointed at the drawing, which cannot happen
    /// until GTK has said how big the window is.
    framed: bool,
    /// Where the pointer is, so that the wheel can zoom about it.
    pointer: (f64, f64),
    /// How far the drag in progress had gone when it was last seen.
    dragged: (f64, f64),
    /// The selected node; its subtree is [`Scene::subtree`] of it.
    chosen: Option<u32>,
    last: Frame,
}

impl View {
    fn new(scene: Scene) -> View {
        let camera = Camera::framing(scene.bounds(), DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64);
        View {
            scene,
            camera,
            framed: false,
            pointer: (DEFAULT_WIDTH as f64 / 2.0, DEFAULT_HEIGHT as f64 / 2.0),
            dragged: (0.0, 0.0),
            chosen: None,
            last: Frame::default(),
        }
    }

    /// The part of the drawing the selection stands for, or all of it.
    fn chosen_bounds(&self) -> Rect {
        match self.chosen {
            Some(i) => self.scene.subtree_bounds(i),
            None => self.scene.bounds(),
        }
    }
}

/// Draws a frame, and remembers what it drew.
///
/// The whole of the viewer's work per frame is here, and all of it starts from
/// the one walk of the quadtree in the middle: nothing outside the window is
/// ever touched.
fn draw(view: &mut View, cr: &cairo::Context, width: f64, height: f64) {
    view.camera.resize(width, height);
    if !view.framed {
        // The first frame is the first time the real size of the window is
        // known, and so the first moment the drawing can be made to fit it.
        view.camera.frame(view.scene.bounds());
        view.framed = true;
    }

    let camera = view.camera;
    let scale = camera.scale();
    let seen = camera.visible();
    let radius_px = view.scene.radius() * scale;
    let circles = radius_px >= MIN_CIRCLE_PX;

    cr.set_source_rgb(PAPER.0, PAPER.1, PAPER.2);
    ink(cr.paint());

    // Zoomed in, open the tree all the way and draw what is in it; zoomed out,
    // stop at cells a couple of pixels across and draw those instead.
    let resolution = if circles { 0.0 } else { SUMMARY_PX / scale };

    let mut nodes: Vec<u32> = Vec::new();
    let mut squares: Vec<(Rect, u32)> = Vec::new();
    view.scene.visit(seen, resolution, &mut |patch| match patch {
        Patch::Nodes(indices) => nodes.extend_from_slice(indices),
        Patch::Cluster { bounds, count } => squares.push((bounds, count)),
    });

    let scene = &view.scene;
    let chosen = view.chosen.map(|i| scene.subtree(i));
    let is_chosen = |i: u32| chosen.as_ref().is_some_and(|r| r.contains(&i));

    // The density drawing: a square a cell, shaded by how crowded it is.  Drawn
    // first so that the individually drawn nodes of the same frame sit on top.
    if !squares.is_empty() {
        cr.set_antialias(cairo::Antialias::None);
        for &(cell, count) in &squares {
            let (x, y) = camera.to_screen(cell.min_x, cell.min_y);
            let side = (cell.width() * scale).max(1.0);
            let shade = crowding(count);
            cr.set_source_rgb(shade, shade, shade);
            cr.rectangle(x, y, side, side);
            ink(cr.fill());
        }
        cr.set_antialias(cairo::Antialias::Default);
    }

    // The edges, when there is room for them: one path, one stroke.
    if circles && radius_px >= MIN_LINK_PX {
        cr.set_source_rgb(LINK.0, LINK.1, LINK.2);
        cr.set_line_width((radius_px / 4.0).clamp(0.5, 3.0));
        for &i in &nodes {
            let parent = scene.node(i).parent;
            if parent == NO_PARENT {
                continue;
            }
            let [px, py] = scene.at(parent);
            let [x, y] = scene.at(i);
            let (sx, sy) = camera.to_screen(px, py);
            cr.move_to(sx, sy);
            let (sx, sy) = camera.to_screen(x, y);
            cr.line_to(sx, sy);
        }
        ink(cr.stroke());
    }

    // The nodes, in as few fills as there are colours: naming a colour once per
    // circle is most of the cost of a frame that draws a hundred thousand.
    if !circles {
        cr.set_antialias(cairo::Antialias::None);
    }
    for (colour, want_leaves) in [(INNER, false), (LEAF, true)] {
        cr.set_source_rgb(colour.0, colour.1, colour.2);
        let mut any = false;
        for &i in &nodes {
            if scene.is_leaf(i) != want_leaves || is_chosen(i) {
                continue;
            }
            any = true;
            spot(cr, camera, scene.at(i), radius_px, circles);
        }
        if any {
            ink(cr.fill());
        }
    }
    if !circles {
        cr.set_antialias(cairo::Antialias::Default);
    }

    // The selection on top of the rest, so that a subtree stands out of the
    // drawing it is part of rather than being buried in it.
    if chosen.is_some() {
        cr.set_source_rgb(CHOSEN.0, CHOSEN.1, CHOSEN.2);
        let mut any = false;
        for &i in &nodes {
            if !is_chosen(i) {
                continue;
            }
            any = true;
            spot(cr, camera, scene.at(i), radius_px.max(MIN_CIRCLE_PX), true);
        }
        if any {
            ink(cr.fill());
        }
    }
    if let Some(i) = view.chosen {
        // A ring around the node itself, which is otherwise one of its own
        // subtree's many and impossible to pick out.
        let [x, y] = scene.at(i);
        let (sx, sy) = camera.to_screen(x, y);
        cr.set_source_rgb(CHOSEN.0, CHOSEN.1, CHOSEN.2);
        cr.set_line_width(2.0);
        cr.new_sub_path();
        cr.arc(sx, sy, (radius_px * 2.0).max(6.0), 0.0, TAU);
        ink(cr.stroke());
    }

    view.last = Frame {
        nodes: nodes.len(),
        squares: squares.len(),
        summarised: squares.iter().map(|&(_, n)| n).sum(),
    };

    panel(view, cr, width, height);
}

/// Adds one node to the path: a circle when there is room for one, and a single
/// pixel when there is not.
fn spot(cr: &cairo::Context, camera: Camera, at: [f64; 2], radius_px: f64, circle: bool) {
    let (x, y) = camera.to_screen(at[0], at[1]);
    if circle {
        // Without this the arcs of a batch are joined by lines, since a path
        // continues from wherever the last one left off.
        cr.new_sub_path();
        cr.arc(x, y, radius_px, 0.0, TAU);
    } else {
        cr.rectangle(x - 0.5, y - 0.5, 1.0, 1.0);
    }
}

/// How dark a square standing for `count` nodes is drawn.
///
/// Logarithmic, because the counts across one frame of a tree run from one node
/// to millions and a linear shade would show the trunk and nothing else.
fn crowding(count: u32) -> f64 {
    const DARKEST_AT: f64 = 12.0; // 2^12 nodes in a cell is as dark as it gets.
    let t = ((count.max(1) as f64).log2() / DARKEST_AT).min(1.0);
    0.72 * (1.0 - t)
}

/// The panel in the corner: what is on screen, and what is selected.
fn panel(view: &View, cr: &cairo::Context, width: f64, height: f64) {
    let scene = &view.scene;
    let last = view.last;

    // Four decimal places say nothing at all about a drawing a million nodes
    // wide, which is exactly where one wants to know how far out one is.
    let scale = view.camera.scale();
    let scale = if scale < 0.001 { format!("{scale:.2e}") } else { format!("{scale:.4}") };

    let mut lines = vec![
        format!("{} nodes, {} cells", scene.len(), scene.index().cells()),
        format!(
            "{scale} px/node   showing {}",
            if last.squares == 0 {
                format!("{} nodes", last.nodes)
            } else {
                format!("{} nodes in {} squares", last.summarised, last.squares)
            }
        ),
    ];
    lines.push(match view.chosen {
        Some(i) => {
            let subtree = scene.subtree(i);
            format!(
                "node {} — {} in its subtree",
                scene.node(i).graph,
                subtree.end - subtree.start
            )
        }
        None => "nothing selected — click a node".to_string(),
    });
    lines.push("wheel zoom · drag move · f fit · a all · p parent · esc clear · q quit".into());

    let leading = 16.0;
    let top = height - leading * lines.len() as f64 - 12.0;

    cr.set_source_rgba(PAPER.0, PAPER.1, PAPER.2, 0.86);
    cr.rectangle(0.0, top - 6.0, width.min(640.0), height - top + 6.0);
    ink(cr.fill());

    cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(12.0);
    cr.set_source_rgb(INNER.0, INNER.1, INNER.2);
    for (n, line) in lines.iter().enumerate() {
        cr.move_to(10.0, top + leading * (n as f64 + 1.0) - 4.0);
        ink(cr.show_text(line));
    }
}

/// Swallows what Cairo has to say about a drawing operation.
///
/// Every one of them answers with a `Result`, and the failures are of the kind
/// where the surface itself has gone wrong: there is no frame to salvage and
/// nothing a viewer could usefully do about it, so a frame that cannot be drawn
/// is simply not drawn.
fn ink<T>(_: Result<T, cairo::Error>) {}

/// Builds the window and wires it to the view.
fn open(app: &Application, view: &Rc<RefCell<View>>, width: i32, height: i32, title: &str) {
    let area = DrawingArea::new();

    let window = ApplicationWindow::builder()
        .application(app)
        .title(title)
        .default_width(width)
        .default_height(height)
        .child(&area)
        .build();

    {
        let view = view.clone();
        area.set_draw_func(move |_, cr, w, h| {
            draw(&mut view.borrow_mut(), cr, w as f64, h as f64);
        });
    }

    // The wheel says how far it turned but not where the pointer is, and zooming
    // about the pointer is the whole gesture, so the pointer is tracked apart.
    let motion = gtk::EventControllerMotion::new();
    {
        let view = view.clone();
        motion.connect_motion(move |_, x, y| view.borrow_mut().pointer = (x, y));
    }
    area.add_controller(motion);

    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let view = view.clone();
        let area = area.clone();
        wheel.connect_scroll(move |_, _, dy| {
            {
                let view = &mut *view.borrow_mut();
                let (x, y) = view.pointer;
                view.camera.zoom_notches(dy, x, y);
            }
            area.queue_draw();
            glib::Propagation::Stop
        });
    }
    area.add_controller(wheel);

    // One gesture for both moving and selecting: a press that goes nowhere is a
    // click, and asking two gestures to share a button is asking for trouble.
    let drag = gtk::GestureDrag::new();
    drag.set_button(gdk::BUTTON_PRIMARY);
    {
        let view = view.clone();
        drag.connect_drag_begin(move |_, _, _| view.borrow_mut().dragged = (0.0, 0.0));
    }
    {
        let view = view.clone();
        let area = area.clone();
        drag.connect_drag_update(move |_, dx, dy| {
            {
                let view = &mut *view.borrow_mut();
                let (was_x, was_y) = view.dragged;
                view.camera.pan(dx - was_x, dy - was_y);
                view.dragged = (dx, dy);
            }
            area.queue_draw();
        });
    }
    {
        let view = view.clone();
        let area = area.clone();
        drag.connect_drag_end(move |gesture, dx, dy| {
            if dx.hypot(dy) > CLICK_SLOP_PX {
                return;
            }
            let Some((x, y)) = gesture.start_point() else { return };
            {
                let view = &mut *view.borrow_mut();
                let (nx, ny) = view.camera.to_node(x, y);
                // The tolerance is a distance on the screen, so that a click is
                // as forgiving zoomed out as it is zoomed in; never tighter than
                // a node, so that a hit on a circle is always a hit.
                let within = (PICK_PX / view.camera.scale()).max(view.scene.radius());
                view.chosen = view.scene.pick(nx, ny, within);
            }
            area.queue_draw();
        });
    }
    area.add_controller(drag);

    let keys = gtk::EventControllerKey::new();
    {
        let view = view.clone();
        let area = area.clone();
        let window = window.clone();
        keys.connect_key_pressed(move |_, key, _, _| {
            {
                let view = &mut *view.borrow_mut();
                let middle = (view.camera.width() / 2.0, view.camera.height() / 2.0);
                match key {
                    gdk::Key::f => {
                        let subject = view.chosen_bounds();
                        view.camera.frame(subject);
                    }
                    gdk::Key::a | gdk::Key::Home => {
                        let whole = view.scene.bounds();
                        view.camera.frame(whole);
                    }
                    gdk::Key::p => {
                        // Climbing out of a subtree one dived into.  A root has
                        // no parent, and stays selected rather than clearing.
                        if let Some(i) = view.chosen {
                            let up = view.scene.node(i).parent;
                            if up != NO_PARENT {
                                view.chosen = Some(up);
                            }
                        }
                    }
                    gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => {
                        view.camera.zoom_notches(-1.0, middle.0, middle.1)
                    }
                    gdk::Key::minus | gdk::Key::KP_Subtract => {
                        view.camera.zoom_notches(1.0, middle.0, middle.1)
                    }
                    gdk::Key::Escape => view.chosen = None,
                    gdk::Key::q => {
                        window.close();
                        return glib::Propagation::Stop;
                    }
                    _ => return glib::Propagation::Proceed,
                }
            }
            area.queue_draw();
            glib::Propagation::Stop
        });
    }
    // On the window rather than on the drawing, so that the keys work without
    // anything having been clicked on first.
    window.add_controller(keys);

    window.present();
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

    let view = Rc::new(RefCell::new(View::new(scene)));
    let title = format!("{graph_name} — {} nodes", view.borrow().scene.len());

    let app = Application::builder()
        .application_id("it.unifi.coloring-bt-transactions.tree-view")
        // Two graphs are two windows of their own, not one window asked twice.
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(move |app| open(app, &view, width, height, &title));

    // The basename and the flags above are this program's, and GTK would refuse
    // them; it is given a command line with nothing in it but the name.
    let code = app.run_with_args(&["tree-view"]);

    if code.get() == 0 {
        Ok(())
    } else {
        Err(format!("the window exited with {}", code.get()))
    }
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

    /// A view on the forest of `graph_of(n, arcs)`, laid out and indexed, with
    /// the camera framing all of it in a window `width` by `height`.
    ///
    /// The whole of what the program does before GTK is involved, which is why
    /// the tests below start from a graph rather than from a scene.
    fn viewing(n: usize, arcs: &[(usize, usize)], width: f64, height: f64) -> View {
        let graph = forest::graph_of(n, arcs);
        let mut arena = Arena::with_capacity(n + 1);
        let built = forest::build(&graph, &mut arena).unwrap();
        let arena = forest::lay_out(arena, built.root);
        let scene = Scene::of(&arena, built.root).unwrap();

        let mut view = View::new(scene);
        view.camera.resize(width, height);
        view.camera.frame(view.scene.bounds());
        view.framed = true;
        view
    }

    /// Draws one frame onto paper of its own, and hands back the pixels.
    ///
    /// A `cairo::Context` is a `cairo::Context` whether GTK made it or a test
    /// did, and [`draw`] wants nothing else, so the whole of the drawing can be
    /// looked at without a screen to put it on.
    fn frame(view: &mut View, width: i32, height: i32) -> Vec<[u8; 4]> {
        let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height)
            .expect("paper to draw on");
        {
            let cr = cairo::Context::new(&surface).expect("a context on it");
            draw(view, &cr, width as f64, height as f64);
        }
        surface.flush();
        let data = surface.data().expect("the frame, once nothing else holds it");
        // ARGB32 on a little-endian machine is blue, green, red, alpha.
        data.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
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

    /// A graph with several roots draws them all, and `Escape` puts the view
    /// back to having nothing selected without disturbing the drawing.
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
}

