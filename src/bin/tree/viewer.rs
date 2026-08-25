//! # The window, and what one frame of it costs
//!
//! The part of a viewer that is about the *screen* rather than about what is
//! being looked at: a GTK window with a Cairo surface in it, a camera one can
//! move and zoom, a click that selects a subtree, and a frame that draws only
//! what is inside the window.  Two binaries share it — `tree-view` draws a
//! webgraph, `tx-view` draws transactions coloured by the blocks their coins
//! came from — and everything they disagree about is in one trait, [`Paint`].
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
//! | arrow keys | move the camera a tenth of the window |
//! | `+`, `-` | zoom about the middle of the window |
//! | `Escape` | select nothing |
//! | `q` | close |
//!
//! # Why a frame costs what the window costs
//!
//! A drawing of ten million nodes has ten million circles in it and a window has
//! some hundred thousand pixels, so a frame that looked at every node would be
//! two orders of magnitude of wasted work — and would get slower as the drawing
//! got bigger, which is the one thing a viewer for big drawings must not do.
//!
//! [`Scene`] holds the nodes in a [`quadtree::Quadtree`](crate::quadtree), and a
//! frame asks it one question: what is inside the camera's rectangle, at this
//! coarseness.  The walk never enters a cell the window does not overlap, so
//! nodes off screen cost nothing at all; and it stops at cells too small to be
//! worth opening, answering with a slice it does not place, so nodes *under* a
//! pixel cost nothing either.  Both ends of that are what keep the work
//! proportional to the window rather than to the drawing.
//!
//! # Two drawings, and where they change over
//!
//! Which one is showing depends on one number: how many pixels a node's circle
//! is across, which is [`Scene::radius`] times the camera's scale.
//!
//! - **Circles**, once a radius is [`MIN_CIRCLE_PX`] or more.  A filled circle a
//!   node, drawn at its full diameter in whatever colour [`Paint`] gives it,
//!   and — once there is room for them, at [`MIN_LINK_PX`] — the edges to
//!   parents, which at this range are worth more than the ink they cost.  The
//!   quadtree is opened all the way, since at this scale the nodes on screen are
//!   at most a few per pixel anyway.
//! - **Density**, below it.  A circle smaller than a pixel is not a circle, so
//!   what is drawn is the quadtree's own cells: each one a square, shaded by
//!   [`Paint::cluster`] from the nodes that fell in it.  Cells narrower than
//!   [`SUMMARY_PX`] are the ones summarised, so the number of squares is set by
//!   the size of the window.
//!
//! The changeover is the same walk with a different stopping rule, which is why
//! there is no seam at it: zooming in opens cells, and the last thing a cell
//! opens into is its nodes.
//!
//! # As few fills as there are colours
//!
//! Naming a colour to Cairo once per circle is most of the cost of a frame that
//! draws a hundred thousand of them, so the frame does not: [`Paint`] sorts its
//! nodes into *buckets*, one per colour it draws in, and a frame builds one path
//! per bucket and fills it once.  `tree-view` has two buckets, leaves and the
//! rest; `tx-view` has a few hundred, one per quantised colour.  Either way the
//! fills are counted in colours and the nodes are touched once each.
//!
//! # Where the selection lives
//!
//! Clicking picks the nearest node within a few pixels, and what is selected is
//! that node *and its subtree* — in [`scene`](crate::scene)'s pre-order that is
//! a range of indices, so highlighting it is a comparison per drawn node and
//! framing it is a scan of a slice.  `p` walks the selection up to its parent,
//! which is how one climbs out of a subtree one has dived into.

// This file is part of a small library with no `lib.rs` to live in -- `src/*.rs`
// belong to the main binary, so the viewers reach it by `#[path]`.  Compiled
// into a binary crate, anything one of them does not happen to call reads as
// dead; the surface is the point, so the lint goes rather than the surface.
#![allow(dead_code)]

use std::cell::RefCell;
use std::f64::consts::TAU;
use std::rc::Rc;

use gtk4 as gtk;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{cairo, Application, ApplicationWindow, DrawingArea};

use crate::camera::{Camera, Rect};
use crate::quadtree::Patch;
use crate::scene::{Scene, NO_PARENT};

/// The window, when the caller does not say.
pub const DEFAULT_WIDTH: i32 = 1200;
pub const DEFAULT_HEIGHT: i32 = 800;

/// A colour, as Cairo takes them: red, green and blue in `[0, 1]`.
pub type Rgb = (f64, f64, f64);

/// The tones the window itself owns, as opposed to the ones [`Paint`] chooses.
pub const PAPER: Rgb = (1.0, 1.0, 1.0);
pub const TEXT: Rgb = (0.0, 0.0, 0.0);
pub const CHOSEN: Rgb = (0.82, 0.24, 0.10);
pub const LINK: Rgb = (0.78, 0.78, 0.78);

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
/// drawing has.
pub const SUMMARY_PX: f64 = 2.0;

/// How far from the pointer, in pixels, a click will reach for a node.
const PICK_PX: f64 = 12.0;

/// How far a press may move and still be a click rather than a drag.
const CLICK_SLOP_PX: f64 = 4.0;

/// What a drawing looks like: everything the window leaves to its caller.
///
/// The nodes are sorted into [`Paint::buckets`] buckets and each bucket is
/// filled in one go, so a paint that wants a hundred colours costs a hundred
/// fills a frame rather than one per node — see the module's own docs.
pub trait Paint {
    /// How many colours the nodes are drawn in.
    fn buckets(&self) -> usize;

    /// Which of them node `i` is drawn in.  Must be below [`Paint::buckets`].
    fn bucket(&self, scene: &Scene, i: u32) -> usize;

    /// The colour of a bucket.  Asked once a frame per bucket that has anything
    /// in it, so it may be worked out rather than looked up.
    fn colour(&self, bucket: usize) -> Rgb;

    /// The colour of a square standing for the nodes of a summarised cell.
    ///
    /// `nodes` is there to be *sampled* — a few of them say what colour the
    /// crowd is — and walking all of it undoes the summary it came from.  The
    /// default ignores it and shades by how many there are.
    fn cluster(&self, nodes: &[u32]) -> Rgb {
        let shade = crowding(nodes.len() as u32);
        (shade, shade, shade)
    }

    /// What the panel says about the selection, a line to a string.
    fn describe(&self, scene: &Scene, chosen: Option<u32>) -> Vec<String> {
        vec![match chosen {
            Some(i) => {
                let subtree = scene.subtree(i);
                format!(
                    "node {} — {} in its subtree",
                    scene.node(i).graph,
                    subtree.end - subtree.start
                )
            }
            None => "nothing selected — click a node".to_string(),
        }]
    }

    /// Anything else the paint wants on the finished frame — a legend, say.
    /// Drawn over the drawing and under nothing, with the whole window to itself.
    fn overlay(&self, _cr: &cairo::Context, _width: f64, _height: f64) {}
}

/// What the last frame drew, for the panel in the corner.
#[derive(Clone, Copy, Default)]
pub struct Frame {
    /// Nodes drawn one by one.
    pub nodes: usize,
    /// Squares standing for more than one node.
    pub squares: usize,
    /// How many nodes those squares stood for.
    pub summarised: u32,
}

/// Everything the window is looking at, and how.
pub struct View<P: Paint> {
    pub scene: Scene,
    pub paint: P,
    pub camera: Camera,
    /// Whether the camera has been pointed at the drawing, which cannot happen
    /// until GTK has said how big the window is.
    pub framed: bool,
    /// Where the pointer is, so that the wheel can zoom about it.
    pub pointer: (f64, f64),
    /// How far the drag in progress had gone when it was last seen.
    dragged: (f64, f64),
    /// The selected node; its subtree is [`Scene::subtree`] of it.
    pub chosen: Option<u32>,
    pub last: Frame,
    /// The three working buffers of a frame, kept between frames so that drawing
    /// one allocates nothing: what the quadtree named, what it summarised, and
    /// the named nodes sorted by the colour they are drawn in.
    nodes: Vec<u32>,
    squares: Vec<(Rect, Rgb)>,
    batches: Vec<Vec<u32>>,
}

impl<P: Paint> View<P> {
    pub fn new(scene: Scene, paint: P) -> View<P> {
        let camera = Camera::framing(scene.bounds(), DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64);
        View {
            scene,
            paint,
            camera,
            framed: false,
            pointer: (DEFAULT_WIDTH as f64 / 2.0, DEFAULT_HEIGHT as f64 / 2.0),
            dragged: (0.0, 0.0),
            chosen: None,
            last: Frame::default(),
            nodes: Vec::new(),
            squares: Vec::new(),
            batches: Vec::new(),
        }
    }

    /// The part of the drawing the selection stands for, or all of it.
    pub fn chosen_bounds(&self) -> Rect {
        match self.chosen {
            Some(i) => self.scene.subtree_bounds(i),
            None => self.scene.bounds(),
        }
    }

    /// Points the camera at the whole drawing in a window of this size, as the
    /// first real frame would.  For a caller drawing without a window.
    pub fn framing(&mut self, width: f64, height: f64) {
        self.camera.resize(width, height);
        self.camera.frame(self.scene.bounds());
        self.framed = true;
    }
}

/// Draws a frame, and remembers what it drew.
///
/// The whole of the viewer's work per frame is here, and all of it starts from
/// the one walk of the quadtree in the middle: nothing outside the window is
/// ever touched.
pub fn draw<P: Paint>(view: &mut View<P>, cr: &cairo::Context, width: f64, height: f64) {
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

    // Taken out of the view so that the walk below can fill them while it reads
    // the scene and the paint, and put back at the end.  They are the same
    // allocations every frame, which is the point of their living in the view.
    let mut nodes = std::mem::take(&mut view.nodes);
    let mut squares = std::mem::take(&mut view.squares);
    let mut batches = std::mem::take(&mut view.batches);

    nodes.clear();
    squares.clear();
    batches.resize_with(view.paint.buckets(), Vec::new);
    for batch in batches.iter_mut() {
        batch.clear();
    }

    let scene = &view.scene;
    let paint = &view.paint;

    let mut summarised = 0u32;
    scene.visit(seen, resolution, &mut |patch| match patch {
        Patch::Nodes(indices) => nodes.extend_from_slice(indices),
        Patch::Cluster { bounds, nodes: held } => {
            summarised += held.len() as u32;
            squares.push((bounds, paint.cluster(held)));
        }
    });

    let chosen = view.chosen.map(|i| scene.subtree(i));
    let is_chosen = |i: u32| chosen.as_ref().is_some_and(|r| r.contains(&i));

    // The density drawing: a square a cell, in whatever colour the paint made of
    // what fell in it.  Drawn first so that the individually drawn nodes of the
    // same frame sit on top.
    if !squares.is_empty() {
        cr.set_antialias(cairo::Antialias::None);
        for &(cell, colour) in squares.iter() {
            let (x, y) = camera.to_screen(cell.min_x, cell.min_y);
            let side = (cell.width() * scale).max(1.0);
            cr.set_source_rgb(colour.0, colour.1, colour.2);
            cr.rectangle(x, y, side, side);
            ink(cr.fill());
        }
        cr.set_antialias(cairo::Antialias::Default);
    }

    // The edges, when there is room for them: one path, one stroke.
    if circles && radius_px >= MIN_LINK_PX {
        cr.set_source_rgb(LINK.0, LINK.1, LINK.2);
        cr.set_line_width((radius_px / 4.0).clamp(0.5, 3.0));
        for &i in nodes.iter() {
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
    // circle is most of the cost of a frame that draws a hundred thousand.  The
    // selection is left out of the batches and drawn after them.
    for &i in nodes.iter() {
        if !is_chosen(i) {
            batches[paint.bucket(scene, i)].push(i);
        }
    }
    if !circles {
        cr.set_antialias(cairo::Antialias::None);
    }
    for (bucket, batch) in batches.iter().enumerate() {
        if batch.is_empty() {
            continue;
        }
        let colour = paint.colour(bucket);
        cr.set_source_rgb(colour.0, colour.1, colour.2);
        for &i in batch.iter() {
            spot(cr, camera, scene.at(i), radius_px, circles);
        }
        ink(cr.fill());
    }
    if !circles {
        cr.set_antialias(cairo::Antialias::Default);
    }

    // The selection on top of the rest, so that a subtree stands out of the
    // drawing it is part of rather than being buried in it.
    if chosen.is_some() {
        cr.set_source_rgb(CHOSEN.0, CHOSEN.1, CHOSEN.2);
        let mut any = false;
        for &i in nodes.iter() {
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
        summarised,
    };

    view.nodes = nodes;
    view.squares = squares;
    view.batches = batches;

    view.paint.overlay(cr, width, height);
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

/// How dark a square standing for `count` nodes is drawn, on a scale where 0 is
/// black: the shade [`Paint::cluster`] falls back on, and the one a colour can
/// be dimmed by when it has a colour to dim.
///
/// Logarithmic, because the counts across one frame of a tree run from one node
/// to millions and a linear shade would show the trunk and nothing else.
pub fn crowding(count: u32) -> f64 {
    const DARKEST_AT: f64 = 12.0; // 2^12 nodes in a cell is as dark as it gets.
    let t = ((count.max(1) as f64).log2() / DARKEST_AT).min(1.0);
    0.72 * (1.0 - t)
}

/// The panel in the corner: what is on screen, and what is selected.
fn panel<P: Paint>(view: &View<P>, cr: &cairo::Context, width: f64, height: f64) {
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
    lines.extend(view.paint.describe(scene, view.chosen));
    lines.push(
        "wheel zoom · drag/arrows move · f fit · a all · p parent · esc clear · q quit".into(),
    );

    let leading = 16.0;
    let top = height - leading * lines.len() as f64 - 12.0;

    cr.set_source_rgba(PAPER.0, PAPER.1, PAPER.2, 0.86);
    cr.rectangle(0.0, top - 6.0, width.min(640.0), height - top + 6.0);
    ink(cr.fill());

    cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(12.0);
    cr.set_source_rgb(TEXT.0, TEXT.1, TEXT.2);
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
pub fn ink<T>(_: Result<T, cairo::Error>) {}

/// Builds the window and wires it to the view.
fn open<P: Paint + 'static>(
    app: &Application,
    view: &Rc<RefCell<View<P>>>,
    width: i32,
    height: i32,
    title: &str,
) {
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
                // An arrow moves the camera a tenth of the window, whatever the
                // zoom; holding one down repeats, which is the glide.
                let step = (view.camera.width() / 10.0, view.camera.height() / 10.0);
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
                    // The camera goes where the arrow points, so the drawing
                    // slides the other way: right brings in what was off the
                    // right edge, which is [`Camera::pan`] of a negative dx.
                    gdk::Key::Left | gdk::Key::KP_Left => view.camera.pan(step.0, 0.0),
                    gdk::Key::Right | gdk::Key::KP_Right => view.camera.pan(-step.0, 0.0),
                    gdk::Key::Up | gdk::Key::KP_Up => view.camera.pan(0.0, step.1),
                    gdk::Key::Down | gdk::Key::KP_Down => view.camera.pan(0.0, -step.1),
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

/// Opens a window on `view` and runs it until it is closed.
///
/// `id` is the application id GTK wants, which is per program rather than per
/// window; every one of ours asks for `NON_UNIQUE`, so that two drawings are two
/// windows of their own rather than one window asked for twice.
pub fn show<P: Paint + 'static>(
    view: View<P>,
    id: &str,
    title: &str,
    width: i32,
    height: i32,
    argv0: &str,
) -> Result<(), String> {
    let view = Rc::new(RefCell::new(view));
    let title = title.to_string();

    let app = Application::builder()
        .application_id(id)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_activate(move |app| open(app, &view, width, height, &title));

    // The arguments the program was given are its own, and GTK would refuse
    // them; it is given a command line with nothing in it but the name.
    let code = app.run_with_args(&[argv0]);

    if code.get() == 0 {
        Ok(())
    } else {
        Err(format!("the window exited with {}", code.get()))
    }
}

/// Draws one frame onto paper of its own, and hands back the pixels.
///
/// A `cairo::Context` is a `cairo::Context` whether GTK made it or a test did,
/// and [`draw`] wants nothing else, so the whole of a drawing can be looked at
/// without a screen to put it on.  Here rather than in either binary's tests
/// because both look at their frames the same way.
#[cfg(test)]
pub fn frame<P: Paint>(view: &mut View<P>, width: i32, height: i32) -> Vec<[u8; 4]> {
    let mut surface =
        cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).expect("paper to draw on");
    {
        let cr = cairo::Context::new(&surface).expect("a context on it");
        draw(view, &cr, width as f64, height as f64);
    }
    surface.flush();
    let data = surface.data().expect("the frame, once nothing else holds it");
    // ARGB32 on a little-endian machine is blue, green, red, alpha.
    data.chunks_exact(4).map(|p| [p[0], p[1], p[2], p[3]]).collect()
}
