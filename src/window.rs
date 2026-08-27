//! # The picture in a window
//!
//! [`page`] folds the colouring onto a canvas and writes it to a
//! sheet of paper.  This shows the same canvas on a screen instead: `--view`
//! opens a GTK window over it, one can move and zoom, and `e` writes the page —
//! so the picture becomes something to look *into* rather than something to open
//! once at whatever size it came out.
//!
//! # What is being looked at
//!
//! The canvas, and only the canvas.  It is a real raster of `--page` cells each
//! way, drawn by the run exactly as `--pdf` would have drawn it, and this window
//! is a camera over it: zooming in past one pixel a cell magnifies cells rather
//! than uncovering finer ones, because there are no finer ones — refolding at a
//! higher resolution would mean reading the records again, and the records are a
//! stream this program has already spent its one rewind on.
//!
//! Which is to say `--page` is the resolution knob and it is worth turning up
//! here.  A page is read at the size it is printed and 1024 cells is a generous
//! sheet; a window is zoomed, so the cells one can climb into are the ones
//! `--page` put there.  They cost four bytes each while the run is going — a
//! 4096-cell canvas is 67 MB — which is the whole of what raising it costs.
//!
//! # What one can do with it
//!
//! | | |
//! |---|---|
//! | wheel up, wheel down | zoom in and out, about the pointer |
//! | drag | move the picture |
//! | `a`, `f`, `Home` | fit the whole picture in the window |
//! | `1` | one pixel a cell, about the pointer |
//! | `+`, `-` | zoom about the middle of the window |
//! | arrow keys | move the camera a tenth of the window |
//! | `e` | write what is on screen to a PDF beside the program |
//! | `q` | close |
//!
//! The keys are the viewers' keys where the two windows mean the same thing by
//! them, which is most of them; what is missing is everything about a
//! *selection*, since a cell is a rectangle of the picture and not a thing one
//! can pick.  What stands in for it is the readout: the panel says which block
//! and which record the pointer is over, which is the question one opens this to
//! ask.
//!
//! # Two drawings, and where they change over
//!
//! Which one is showing depends on one number: how many pixels a cell is across,
//! which is the camera's scale.
//!
//! - **Samples**, below [`MIN_CIRCLE_PX`].  The canvas as an image, one sample a
//!   cell, painted the shade of how much of the rectangle it stands for is
//!   inked.  A cell around a pixel across can be nothing else.
//! - **Circles**, at [`MIN_CIRCLE_PX`] and above.  A filled disc of one cell
//!   across, in the same shade, centred on the cell.  Once there is room for a
//!   cell to be a shape rather than a sample, being one says something a square
//!   of the same colour does not: the paper between the discs is the grid, so a
//!   cell that is dark because it is *full* is told apart from a run of cells
//!   that are dark together, and the diagonal edge of the drawing stops being a
//!   staircase of squares.
//!
//! Both draw the same shades, so there is no seam at the changeover beyond the
//! marks changing shape — nothing appears or disappears.
//!
//! `--pdf` never reaches the second: it draws at one point a cell, and one point
//! is below [`MIN_CIRCLE_PX`] whatever the page is looked at on.  A disc of one
//! point and a square of one point are the same mark at that size and the disc
//! costs a path apiece, which for a canvas of a million cells is a page nothing
//! wants.  The way to a page of circles is the window's own `e`, zoomed in —
//! which exports what is on the screen, so the circles on it are bounded by the
//! window rather than by the canvas.
//!
//! # Why a frame costs nothing
//!
//! The tree viewers walk a quadtree per frame to keep the work proportional to
//! the window rather than to the drawing.  Nothing of the sort is needed here.
//! Zoomed out, a frame is one `cairo_mask` of one surface, and Cairo clips it to
//! the window before it composites anything: a canvas of a million cells and a
//! canvas of sixteen million cost the same frame.  Zoomed in, only the cells the
//! camera can see are looked at at all — the visible rectangle is arithmetic on
//! the canvas, not a search of it — and there are at most
//! `(window / MIN_CIRCLE_PX)^2` of those, however large the canvas.
//!
//! The circles are gathered by shade and filled a shade at a time, so a frame is
//! at most 256 fills rather than one per cell.  That is the viewers' own trick,
//! for the same reason: a fill apiece is what makes a drawing of many marks slow,
//! not the marks.
//!
//! The camera is [`Camera`], the viewers' own, measured
//! in cells rather than in nodes.  That is the whole of what this shares with
//! them, and it is why `--view` needs GTK but nothing of the tree.

use std::cell::RefCell;
use std::f64::consts::TAU;
use std::rc::Rc;

use gtk4 as gtk;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow, DrawingArea};

use crate::camera::{Camera, Rect};
use crate::page;

/// The window, before anyone resizes it.  The viewers' own numbers.
pub const DEFAULT_WIDTH: i32 = 1200;
pub const DEFAULT_HEIGHT: i32 = 800;

/// How far a press may move and still be a click rather than a drag.  Only used
/// to keep a click from jerking the picture by a pixel.
const CLICK_SLOP_PX: f64 = 4.0;

/// How many pixels a cell must be across before it is drawn as a circle rather
/// than as a sample of the image.
///
/// Three, because a disc two pixels across is a square with its corners rubbed
/// off and not worth the path it costs, and because this is what bounds the work
/// of a frame: at most `(window / 3)^2` cells can be on the screen at once, so a
/// full window is around a hundred thousand circles in the worst case and the
/// usual case is a small fraction of that, most of a picture being paper.
pub const MIN_CIRCLE_PX: f64 = 3.0;

/// How many shades a cell can be drawn in, which is what an A8 sample holds.
/// The circles of a frame are gathered into this many batches and filled a batch
/// at a time.
const SHADES: usize = 256;

/// Everything the window is looking at, and how.
struct View {
    /// The canvas as an A8 surface: what the image drawing paints.
    ink: cairo::ImageSurface,
    /// The same shades unpadded, one byte a cell, row major: what the circle
    /// drawing reads a cell at a time.  Kept beside the surface rather than
    /// borrowed out of it, since a surface Cairo is painting from cannot also be
    /// handing out its bytes.
    shades: Vec<u8>,
    /// The circles of a frame, gathered by the shade they are drawn in.  Kept
    /// between frames so that drawing one allocates nothing, the way the viewers
    /// keep theirs.
    batches: Vec<Vec<(f64, f64)>>,
    /// The canvas in cells, which is what the camera measures in.
    across: f64,
    down: f64,
    /// The picture those cells stand for: block ids across, rows down.
    columns: usize,
    rows: usize,
    /// Transactions to a row, so that a row can be reported as a record.
    bin: usize,
    camera: Camera,
    /// Whether the camera has been pointed at the picture, which cannot happen
    /// until GTK has said how big the window is.
    framed: bool,
    /// Where the pointer is: the wheel zooms about it and the panel reads it out.
    pointer: (f64, f64),
    /// How far the drag in progress had gone when it was last seen.
    dragged: (f64, f64),
    /// The last thing the window has to say for itself — where a PDF went, or
    /// why it did not.
    note: Option<String>,
}

impl View {
    /// The whole canvas, in the units the camera works in.
    fn bounds(&self) -> Rect {
        Rect::new(0.0, 0.0, self.across, self.down)
    }

    /// Which block id and which record the cell at `(cx, cy)` stands for, or
    /// `None` when that is off the picture.
    ///
    /// The inverse of the folding, so it names the *first* of the blocks and the
    /// first of the records the cell covers rather than a fictitious middle one.
    fn at(&self, cx: f64, cy: f64) -> Option<(usize, usize)> {
        if cx < 0.0 || cy < 0.0 || cx >= self.across || cy >= self.down {
            return None;
        }
        let block = (cx / self.across * self.columns as f64) as usize;
        let row = (cy / self.down * self.rows as f64) as usize;
        Some((block.min(self.columns - 1), row.saturating_mul(self.bin)))
    }
}

/// Draws a frame with the panel over it: what the window shows.
fn draw(view: &mut View, cr: &cairo::Context, width: f64, height: f64) {
    render(view, cr, width, height);
    panel(view, cr, width, height);
}

/// Draws the picture, and nothing that is about the window.
///
/// Everything but the panel, which has no business on an exported page — see
/// [`export`], which is this and a surface of its own.
fn render(view: &mut View, cr: &cairo::Context, width: f64, height: f64) {
    view.camera.resize(width, height);
    if !view.framed {
        // The first frame is the first time the real size of the window is
        // known, and so the first moment the picture can be made to fit it.
        view.camera.frame(view.bounds());
        view.framed = true;
    }

    ink(page::paper(cr));

    let camera = view.camera;
    let (x, y) = camera.to_screen(0.0, 0.0);
    // A cell too small to be a circle is drawn as what it is, a sample of the
    // canvas; big enough and it is drawn as the shape it stands for.  See the
    // module docs for what the second says that the first does not.
    if camera.scale() >= MIN_CIRCLE_PX {
        spots(view, cr);
    } else {
        ink(page::stamp(cr, &view.ink, x, y, camera.scale()));
    }

    // Where the paper ends.  Zoomed out the picture is a small dark shape in a
    // white window and there is no telling which white is the picture's own; a
    // hairline says.
    let (right, bottom) = camera.to_screen(view.across, view.down);
    cr.set_source_rgb(0.78, 0.78, 0.78);
    cr.set_line_width(1.0);
    cr.rectangle(x, y, right - x, bottom - y);
    ink(cr.stroke());
}

/// Draws every cell the camera can see as a filled disc one cell across.
///
/// Only the cells inside the visible rectangle are looked at, and only the ones
/// with ink in them are drawn: the rectangle is worked out from the camera, so
/// nothing off screen is touched however large the canvas is.
///
/// The discs are gathered by shade and filled a shade at a time, since a source
/// colour has to be set before a fill and setting one per cell is what would
/// make this slow.  A cell holds one of 256 shades, so that is 256 fills at the
/// very most and in practice a handful.
fn spots(view: &mut View, cr: &cairo::Context) {
    let camera = view.camera;
    let seen = camera.visible();

    // Half open, and clamped to the canvas: a camera can be pointed off the
    // edge, and past the edge there are no cells to ask about.
    // A cell holds the coordinates from its own index up to the next, so the
    // ones on screen run from the floor of one edge to the floor of the other.
    let x0 = seen.min_x.floor().clamp(0.0, view.across) as usize;
    let x1 = (seen.max_x.floor() + 1.0).clamp(0.0, view.across) as usize;
    let y0 = seen.min_y.floor().clamp(0.0, view.down) as usize;
    let y1 = (seen.max_y.floor() + 1.0).clamp(0.0, view.down) as usize;

    // Taken out of the view so that the loop below can fill them while it reads
    // the shades, and put back at the end.  They are the same allocations every
    // frame, which is the point of their living in the view.
    let mut batches = std::mem::take(&mut view.batches);
    for batch in &mut batches {
        batch.clear();
    }

    let across = view.across as usize;
    for cy in y0..y1 {
        let band = cy * across;
        for cx in x0..x1 {
            let shade = view.shades[band + cx];
            if shade == 0 {
                continue;
            }
            // The middle of the cell, so that the disc covers the rectangle the
            // cell stands for rather than sitting on its corner.
            batches[shade as usize].push(camera.to_screen(cx as f64 + 0.5, cy as f64 + 0.5));
        }
    }

    let radius = camera.scale() / 2.0;
    for (shade, batch) in batches.iter().enumerate() {
        if batch.is_empty() {
            continue;
        }
        // Black at the cell's own coverage, which is the shade the image would
        // have painted it: the two drawings differ in the mark and not in the
        // ink.
        cr.set_source_rgba(0.0, 0.0, 0.0, shade as f64 / 255.0);
        for &(sx, sy) in batch {
            // Without this the arcs of a batch are joined by lines, since a path
            // continues from wherever the last one left off.
            cr.new_sub_path();
            cr.arc(sx, sy, radius, 0.0, TAU);
        }
        ink(cr.fill());
    }

    view.batches = batches;
}

/// The panel in the corner: what is on screen, and what the pointer is over.
fn panel(view: &View, cr: &cairo::Context, width: f64, height: f64) {
    // Four decimal places say nothing at all about a canvas being looked at from
    // far out, which is exactly where one wants to know how far out one is.
    let scale = view.camera.scale();
    let scale = if scale < 0.001 {
        format!("{scale:.2e}")
    } else {
        format!("{scale:.4}")
    };

    let seen = view.camera.visible();
    let (from, to) = (
        view.at(seen.min_x.max(0.0), seen.min_y.max(0.0)),
        view.at(
            seen.max_x.min(view.across - 1.0),
            seen.max_y.min(view.down - 1.0),
        ),
    );

    let (px, py) = view.camera.to_node(view.pointer.0, view.pointer.1);

    let mut lines = vec![
        format!(
            "{} blocks x {} rows on {} x {} cells, {} transactions to a row",
            view.columns, view.rows, view.across as usize, view.down as usize, view.bin
        ),
        format!(
            "{scale} px/cell   showing {}",
            match (from, to) {
                (Some((b0, r0)), Some((b1, r1))) =>
                    format!("blocks {b0}-{b1}, records {r0}-{r1}"),
                _ => "none of the picture".to_string(),
            }
        ),
        match view.at(px, py) {
            Some((block, record)) => format!("pointer over block {block}, record {record}"),
            None => "pointer off the picture".to_string(),
        },
    ];
    if let Some(note) = &view.note {
        lines.push(note.clone());
    }
    lines.push(
        "wheel zoom · drag/arrows move · a fit · 1 actual size · e pdf · q quit".into(),
    );

    let leading = 16.0;
    let top = height - leading * lines.len() as f64 - 12.0;

    cr.set_source_rgba(1.0, 1.0, 1.0, 0.86);
    cr.rectangle(0.0, top - 6.0, width.min(700.0), height - top + 6.0);
    ink(cr.fill());

    cr.select_font_face("monospace", cairo::FontSlant::Normal, cairo::FontWeight::Normal);
    cr.set_font_size(12.0);
    cr.set_source_rgb(0.0, 0.0, 0.0);
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
/// is simply not drawn.  The viewers say the same thing in their own file, for
/// want of anywhere two binaries can share four lines.
fn ink<T>(_: Result<T, cairo::Error>) {}

/// Writes what the camera is showing to a PDF of its own, and says where.
///
/// The page is the window: the same [`render`] onto a `PdfSurface` the size of
/// the drawing area, so that what comes out is what was being looked at, at the
/// zoom it was being looked at.  The panel stays behind, being about the window
/// rather than about the picture.
///
/// Not the same page `--pdf` writes, and deliberately: that one is the whole
/// canvas at one point a cell, this one is the part of it on the screen at the
/// size it is on the screen.  Fit the window to the whole picture first and the
/// two say the same thing at different resolutions.
fn export(view: &mut View, stem: &str) -> Result<String, String> {
    let width = view.camera.width();
    let height = view.camera.height();

    let path = free_name(stem);
    // Cairo measures a page in points, and is told the window's pixels: a
    // 1200-pixel window becomes a 1200-point page, which is a big sheet of
    // paper at the same shape and the same proportions.
    let surface = cairo::PdfSurface::new(width, height, &path)
        .map_err(|e| format!("{path}: the page could not be started ({e})"))?;
    {
        let cr = cairo::Context::new(&surface)
            .map_err(|e| format!("{path}: nothing to draw with ({e})"))?;
        render(view, &cr, width, height);
    }
    surface.finish();
    Ok(path)
}

/// The first `<stem>-NNN.pdf` that is not already there.
///
/// A race with anything else writing the same names, and worth nothing against
/// one; what it is for is the same window exporting twice, where the second page
/// silently replacing the first is the only real way to lose one.
fn free_name(stem: &str) -> String {
    for n in 1u32.. {
        let name = format!("{stem}-{n:03}.pdf");
        if !std::path::Path::new(&name).exists() {
            return name;
        }
    }
    unreachable!("a u32 of names is more than a working directory holds")
}

/// Builds the window and wires it to the view.
fn open(app: &Application, view: &Rc<RefCell<View>>, title: &str, stem: &str) {
    let area = DrawingArea::new();

    let window = ApplicationWindow::builder()
        .application(app)
        .title(title)
        .default_width(DEFAULT_WIDTH)
        .default_height(DEFAULT_HEIGHT)
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
    // The panel reads it out as well, so a frame is queued for the movement
    // alone.
    let motion = gtk::EventControllerMotion::new();
    {
        let view = view.clone();
        let area = area.clone();
        motion.connect_motion(move |_, x, y| {
            view.borrow_mut().pointer = (x, y);
            area.queue_draw();
        });
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
            // A press that has gone nowhere is somebody clicking on a picture
            // that has nothing to click on; moving by it would be a twitch.
            if dx.hypot(dy) <= CLICK_SLOP_PX {
                return;
            }
            {
                let view = &mut *view.borrow_mut();
                let (was_x, was_y) = view.dragged;
                view.camera.pan(dx - was_x, dy - was_y);
                view.dragged = (dx, dy);
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
        let stem = stem.to_string();
        keys.connect_key_pressed(move |_, key, _, _| {
            {
                let view = &mut *view.borrow_mut();
                let middle = (view.camera.width() / 2.0, view.camera.height() / 2.0);
                // An arrow moves the camera a tenth of the window, whatever the
                // zoom; holding one down repeats, which is the glide.
                let step = (view.camera.width() / 10.0, view.camera.height() / 10.0);
                match key {
                    // Three names for the one thing, since two of them are the
                    // viewers' and there is no selection here for them to mean
                    // anything narrower by.
                    gdk::Key::a | gdk::Key::f | gdk::Key::Home => {
                        let whole = view.bounds();
                        view.camera.frame(whole);
                    }
                    gdk::Key::_1 | gdk::Key::KP_1 => {
                        // A pixel a cell: the size at which the canvas is
                        // neither magnified nor averaged, and so the only zoom
                        // at which what is on the screen is what the run drew.
                        let (x, y) = view.pointer;
                        let factor = 1.0 / view.camera.scale();
                        view.camera.zoom(factor, x, y);
                    }
                    gdk::Key::e => {
                        // The window is not redrawn until this returns, so the
                        // page is of the frame that is still on the screen.
                        view.note = Some(match export(view, &stem) {
                            Ok(path) => format!("written to {path}"),
                            Err(why) => why,
                        });
                    }
                    gdk::Key::plus | gdk::Key::equal | gdk::Key::KP_Add => {
                        view.camera.zoom_notches(-1.0, middle.0, middle.1)
                    }
                    gdk::Key::minus | gdk::Key::KP_Subtract => {
                        view.camera.zoom_notches(1.0, middle.0, middle.1)
                    }
                    // The camera goes where the arrow points, so the picture
                    // slides the other way: right brings in what was off the
                    // right edge, which is `Camera::pan` of a negative dx.
                    gdk::Key::Left | gdk::Key::KP_Left => view.camera.pan(step.0, 0.0),
                    gdk::Key::Right | gdk::Key::KP_Right => view.camera.pan(-step.0, 0.0),
                    gdk::Key::Up | gdk::Key::KP_Up => view.camera.pan(0.0, step.1),
                    gdk::Key::Down | gdk::Key::KP_Down => view.camera.pan(0.0, -step.1),
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

/// Closes the canvas and turns it into something to look at.
///
/// The camera is pointed at the whole picture here and again on the first real
/// frame, since until GTK has laid the drawing area out the window's size is a
/// guess — see `framed`.
fn viewing(mut canvas: page::Writer) -> Result<View, String> {
    canvas.close().map_err(|e| e.to_string())?;

    let (columns, rows) = canvas.dimensions();
    let (across, down) = canvas.canvas();
    let shades = canvas.shades();
    let ink = page::mask(&shades, across, down).map_err(|e| e.to_string())?;
    let bounds = Rect::new(0.0, 0.0, across as f64, down as f64);

    Ok(View {
        ink,
        shades,
        batches: vec![Vec::new(); SHADES],
        across: across as f64,
        down: down as f64,
        columns,
        rows,
        bin: canvas.bin(),
        camera: Camera::framing(bounds, DEFAULT_WIDTH as f64, DEFAULT_HEIGHT as f64),
        framed: false,
        pointer: (DEFAULT_WIDTH as f64 / 2.0, DEFAULT_HEIGHT as f64 / 2.0),
        dragged: (0.0, 0.0),
        note: None,
    })
}

/// Opens a window on the canvas and runs until the window is closed.
///
/// The last thing a `--view` run does, in place of writing a file: the records
/// have all been read and folded by the time this is called, so what it shows is
/// finished and nothing arrives while it is up.
///
/// `stem` is what an exported page is named after — the program, so that two
/// windows open in one directory do not write over each other.
pub fn show(canvas: page::Writer, stem: &str) -> Result<(), String> {
    let view = viewing(canvas)?;
    let title = format!(
        "{} blocks x {} rows on {} x {} cells",
        view.columns, view.rows, view.across as usize, view.down as usize
    );
    let view = Rc::new(RefCell::new(view));

    // Per program rather than per window, and `NON_UNIQUE` so that two runs are
    // two windows of their own rather than one window asked for twice.
    let app = Application::builder()
        .application_id("it.unifi.coloring-bt-transactions.view")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let argv0 = stem.to_string();
    let stem = stem.to_string();
    app.connect_activate(move |app| open(app, &view, &title, &stem));

    // The arguments the program was given are its own — a record limit, a
    // backend, `--view` itself — and GTK would refuse every one of them; it is
    // given a command line with nothing in it but the name.
    let code = app.run_with_args(&[argv0.as_str()]);

    if code.get() == 0 {
        Ok(())
    } else {
        Err(format!("the window exited with {}", code.get()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `page::Writer` never finished writes nothing, so the tests about what
    /// is on the screen touch no disk at all -- but one still has to be told
    /// where its page would have gone.
    const UNWRITTEN: &str = "/dev/null/never-opened.pdf";

    /// `records` transactions over `blocks` blocks, the `n`th reaching block
    /// `n`: a diagonal, folded onto `cells` each way.
    fn diagonal(blocks: usize, records: usize, bin: usize, cells: usize) -> View {
        let rows = records.div_ceil(bin);
        let mut canvas = page::Writer::new(UNWRITTEN, blocks, rows, bin, cells).unwrap();
        for r in 0..records {
            canvas.set(r * blocks / records);
            canvas.end_transaction().unwrap();
        }
        viewing(canvas).unwrap()
    }

    /// A canvas of `cells` each way with every cell of it inked, which is the
    /// picture whose *paper* is what one measures.
    fn full(cells: usize) -> View {
        let mut canvas = page::Writer::new(UNWRITTEN, cells, cells, 1, cells).unwrap();
        for _ in 0..cells {
            for block in 0..cells {
                canvas.set(block);
            }
            canvas.end_transaction().unwrap();
        }
        viewing(canvas).unwrap()
    }

    /// How much of the picture's own rectangle came out dark, in a square window
    /// of `size` that the picture is refitted to.
    ///
    /// The measure that tells the two drawings apart without knowing which ran:
    /// a grid of squares fills its rectangle and a grid of discs cannot, whatever
    /// either is shaded.
    fn darkness(view: &mut View, size: i32) -> f64 {
        // Refit, since a camera already pointed at the picture only resizes.
        view.framed = false;
        let pixels = frame(view, size, size);

        let (x0, y0) = view.camera.to_screen(0.0, 0.0);
        let (x1, y1) = view.camera.to_screen(view.across, view.down);
        let (mut dark, mut all) = (0usize, 0usize);
        for y in y0.ceil() as usize..y1.floor() as usize {
            for x in x0.ceil() as usize..x1.floor() as usize {
                all += 1;
                dark += usize::from(pixels[y * size as usize + x] < 128);
            }
        }
        dark as f64 / all as f64
    }

    /// One frame onto paper of its own, as one byte a pixel.
    ///
    /// The picture is black on white and nothing else, so a single channel says
    /// everything a pixel has to say -- 255 is paper and 0 is ink.
    fn frame(view: &mut View, width: i32, height: i32) -> Vec<u8> {
        let mut surface =
            cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            render(view, &cr, width as f64, height as f64);
        }
        surface.flush();
        let data = surface.data().unwrap();
        // ARGB32 on a little-endian machine is blue, green, red, alpha.
        data.chunks_exact(4).map(|p| p[2]).collect()
    }

    /// The readout is the folding read backwards: a cell names the first of the
    /// blocks and the first of the records it covers, so that what it says is a
    /// place in the input rather than an average of one.
    #[test]
    fn the_readout_names_the_first_block_and_record_of_a_cell() {
        let view = diagonal(8, 8, 1, 4);
        assert_eq!(view.at(0.0, 0.0), Some((0, 0)));
        assert_eq!(view.at(1.0, 1.0), Some((2, 2)));
        assert_eq!(view.at(3.5, 3.5), Some((7, 7)));
    }

    /// A row stands for a bin of transactions, and the record it names is the
    /// first of them.
    #[test]
    fn a_binned_row_reads_out_as_the_record_it_starts_at() {
        // Twelve records, three to a row, so four rows -- and one cell a row.
        let view = diagonal(4, 12, 3, 4);
        assert_eq!(view.at(0.0, 0.0), Some((0, 0)));
        assert_eq!(view.at(0.0, 1.0), Some((0, 3)));
        assert_eq!(view.at(0.0, 3.0), Some((0, 9)));
    }

    /// Off the canvas is off the picture, on every side.
    #[test]
    fn a_pointer_off_the_picture_names_nothing() {
        let view = diagonal(8, 8, 1, 4);
        assert_eq!(view.at(-0.5, 2.0), None);
        assert_eq!(view.at(2.0, -0.5), None);
        assert_eq!(view.at(4.0, 2.0), None);
        assert_eq!(view.at(2.0, 4.0), None);
    }

    /// A frame draws the canvas where the camera says it is, and draws the ink
    /// where the ink is: the diagonal comes out on the screen as a diagonal.
    #[test]
    fn a_frame_draws_the_canvas_the_camera_is_over() {
        let mut view = diagonal(8, 8, 1, 8);
        let pixels = frame(&mut view, 200, 200);

        for cy in 0..8 {
            for cx in 0..8 {
                // The middle of the cell, so that neither the hairline round the
                // picture nor a cell's own edge is what gets sampled.
                let (x, y) = view.camera.to_screen(cx as f64 + 0.5, cy as f64 + 0.5);
                let pixel = pixels[y as usize * 200 + x as usize];
                assert_eq!(
                    pixel < 128,
                    cx == cy,
                    "cell ({cx}, {cy}) at ({x}, {y}) came out {pixel}"
                );
            }
        }
    }

    /// A cell with room to be a circle is drawn as one, and one without is drawn
    /// as the sample it has to be.
    ///
    /// Measured on a canvas that is all ink, where the difference is the whole
    /// difference: squares leave no paper between them and discs of one cell
    /// across leave the corners, which is `1 - pi/4` of the picture.
    #[test]
    fn a_cell_with_room_to_be_a_circle_is_drawn_as_one() {
        let mut view = full(8);

        // 400 pixels over 8 cells is far past the changeover.
        let circles = darkness(&mut view, 400);
        assert!(
            (circles - std::f64::consts::FRAC_PI_4).abs() < 0.05,
            "discs should cover pi/4 of it, covered {circles}"
        );

        // 24 over 8 is under three pixels a cell, which is under it.
        let samples = darkness(&mut view, 24);
        assert!(samples > 0.98, "samples should cover all of it, covered {samples}");
    }

    /// A camera pointed off the canvas has no cells to draw and says so rather
    /// than reading past the end of one.
    #[test]
    fn a_camera_off_the_picture_draws_nothing_and_stays_inside_the_canvas() {
        let mut view = diagonal(8, 8, 1, 8);
        view.framed = true; // do not refit; the point is to be looking elsewhere
        view.camera.resize(200.0, 200.0);
        view.camera.look_at(-500.0, -500.0);

        let pixels = frame(&mut view, 200, 200);
        assert!(pixels.iter().all(|&p| p == 255), "something was drawn off the picture");
    }

    /// `e` writes the window, not the canvas: a page the size of the drawing
    /// area, of whatever the camera was showing.
    #[test]
    fn the_export_is_a_page_the_size_of_the_window() {
        let mut view = diagonal(8, 8, 1, 8);
        view.camera.resize(320.0, 240.0);

        let mut stem = std::env::temp_dir();
        stem.push(format!("coloring-bt-window-{}", std::process::id()));
        let stem = stem.to_string_lossy().into_owned();

        let path = export(&mut view, &stem).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
        assert!(
            page::tests::inflated(&bytes).contains("/MediaBox [ 0 0 320 240 ]"),
            "not the size of the window"
        );

        // And a second press does not write over the first.
        let again = export(&mut view, &stem).unwrap();
        assert_ne!(again, path);

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&again).ok();
    }

    /// Zooming in and out about the same point puts the picture back where it
    /// was, which is what makes the wheel something one can undo.
    #[test]
    fn the_wheel_is_its_own_inverse() {
        let mut view = diagonal(8, 8, 1, 8);
        view.camera.resize(200.0, 200.0);
        let before = view.camera.to_screen(4.0, 4.0);

        view.camera.zoom_notches(-3.0, 20.0, 180.0);
        view.camera.zoom_notches(3.0, 20.0, 180.0);

        let after = view.camera.to_screen(4.0, 4.0);
        assert!((after.0 - before.0).abs() < 1e-9, "{before:?} then {after:?}");
        assert!((after.1 - before.1).abs() < 1e-9, "{before:?} then {after:?}");
    }
}
