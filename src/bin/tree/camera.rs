//! # Where the drawing is looked at from
//!
//! The layout in [`forest`](super) puts every node at a place measured in
//! *nodes*: level `d` sits at `x = d`, and two nodes at the same depth are at
//! least a diameter and a margin apart.  A window, meanwhile, is measured in
//! pixels and is a few hundred of them across, while the drawings this is for
//! are millions of nodes wide.  A camera is the one thing standing between the
//! two: it says which rectangle of the drawing the window is showing, and
//! everything else — what to cull, what to draw, what a click landed on — is
//! read off it.
//!
//! It is deliberately a plain value with no idea that GTK exists.  That is what
//! lets the whole of the interaction — zooming, panning, framing a subtree — be
//! tested without opening a window, which is the only way any of it *can* be
//! tested here.
//!
//! # The one invariant
//!
//! [`Camera::scale`] is pixels per node, and it is kept inside
//! [`MIN_SCALE`]`..=`[`MAX_SCALE`].  The bounds are not a policy about how far
//! one may zoom — [`Camera::frame`] will happily fit a drawing at either end —
//! they are there so that a scale can never become zero, infinite, or NaN, since
//! every other method divides by it.
//!
//! # Zooming is anchored
//!
//! [`Camera::zoom`] takes the point on the screen to hold still.  Given the
//! pointer, the node under the cursor stays under the cursor and the drawing
//! grows around it, which is the gesture that lets one dive into a subtree
//! without also having to chase it across the window.  Passing the centre of the
//! viewport gives the other reading, zooming into the middle.
//!
//! Because the scale is clamped, the anchor is honoured against the scale the
//! camera *ended up with* rather than the one it was asked for: at the stops,
//! zooming further does nothing at all instead of sliding the drawing sideways.

// These three files are a small library with no `lib.rs` to live in -- `src/*.rs`
// belong to the main binary, so `tree-view` reaches them by `#[path]` and
// `tests/viewer_geometry.rs` does the same.  Compiled into a binary crate,
// anything one frame does not happen to call reads as dead; the surface is the
// point, so the lint goes rather than the surface.
#![allow(dead_code)]

/// An axis-aligned rectangle in node units.
///
/// Half-open in neither direction and empty when a maximum is below its minimum,
/// which is what [`Rect::nothing`] builds and what lets a bounding box be
/// accumulated by [`Rect::add`] from no points at all.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl Rect {
    pub fn new(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Self {
        Rect { min_x, min_y, max_x, max_y }
    }

    /// The empty rectangle: it contains no point and intersects nothing, and
    /// growing it by [`Rect::add`] gives the bounding box of what was added.
    pub fn nothing() -> Self {
        Rect::new(f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY)
    }

    /// The square of side `side` centred on `(x, y)`.
    pub fn square(x: f64, y: f64, side: f64) -> Self {
        let half = side / 2.0;
        Rect::new(x - half, y - half, x + half, y + half)
    }

    pub fn is_empty(self) -> bool {
        self.max_x < self.min_x || self.max_y < self.min_y
    }

    pub fn width(self) -> f64 {
        self.max_x - self.min_x
    }

    pub fn height(self) -> f64 {
        self.max_y - self.min_y
    }

    pub fn centre(self) -> (f64, f64) {
        ((self.min_x + self.max_x) / 2.0, (self.min_y + self.max_y) / 2.0)
    }

    /// Grows the rectangle to hold `(x, y)`.
    pub fn add(&mut self, x: f64, y: f64) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }

    pub fn contains(self, x: f64, y: f64) -> bool {
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    /// Whether the two rectangles share any point.  Touching edges count, which
    /// costs a node on a boundary being visited by both sides of a split and
    /// buys never dropping one that sits exactly on it.
    pub fn intersects(self, other: Rect) -> bool {
        self.min_x <= other.max_x
            && other.min_x <= self.max_x
            && self.min_y <= other.max_y
            && other.min_y <= self.max_y
    }

    /// The same rectangle grown by `by` on every side.
    pub fn grown(self, by: f64) -> Self {
        Rect::new(self.min_x - by, self.min_y - by, self.max_x + by, self.max_y + by)
    }

    /// The smallest square holding this rectangle, centred on it.
    ///
    /// What a quadtree wants for a root: quartering a square gives squares, so
    /// the cells stay square all the way down and one number describes a cell's
    /// size on both axes.
    pub fn to_square(self) -> Self {
        let (cx, cy) = self.centre();
        Rect::square(cx, cy, self.width().max(self.height()))
    }
}

/// Pixels per node the camera will not go below, or above.
///
/// The low end is a drawing a thousand million nodes wide shown in a thousand
/// pixels; the high end is a single node filling a window.  Neither is a limit
/// anybody meets by scrolling — they are there so the scale stays a number one
/// can divide by.
pub const MIN_SCALE: f64 = 1e-6;
pub const MAX_SCALE: f64 = 1e3;

/// How much one notch of the wheel multiplies or divides the scale by.
///
/// Small enough that a zoom reads as a movement rather than a jump, large enough
/// that crossing four decades of scale — which a graph of transactions asks for —
/// is a few flicks of the wheel and not a minute of scrolling.
pub const ZOOM_PER_NOTCH: f64 = 1.25;

/// The fraction of the window [`Camera::frame`] leaves clear around what it is
/// framing, so that the outermost nodes are not cut in half by the edge.
const FRAME_MARGIN: f64 = 0.04;

/// The smallest extent [`Camera::frame`] will pretend a rectangle has.
///
/// A single node, or a chain, is a rectangle with no width on one axis, and
/// dividing the window by that gives an infinite scale.  Framing it as if it
/// were this many nodes across shows it at a size one can see.
const MIN_FRAMED_EXTENT: f64 = 8.0;

/// Which rectangle of the drawing a window of `width` by `height` pixels shows.
#[derive(Clone, Copy, Debug)]
pub struct Camera {
    /// The node coordinate at the centre of the window.
    x: f64,
    y: f64,
    /// Pixels per node unit, in `MIN_SCALE..=MAX_SCALE`.
    scale: f64,
    width: f64,
    height: f64,
}

impl Camera {
    /// A camera on a window of `width` by `height`, framing `subject`.
    pub fn framing(subject: Rect, width: f64, height: f64) -> Self {
        let mut camera = Camera { x: 0.0, y: 0.0, scale: 1.0, width, height };
        camera.resize(width, height);
        camera.frame(subject);
        camera
    }

    pub fn scale(self) -> f64 {
        self.scale
    }

    pub fn width(self) -> f64 {
        self.width
    }

    pub fn height(self) -> f64 {
        self.height
    }

    /// Follows the window changing size, keeping the same point in the middle.
    ///
    /// A zero-sized window is what GTK reports before it has laid the drawing
    /// area out, and a camera whose window has no extent divides by zero in
    /// [`Camera::visible`]; one pixel is the smallest lie that avoids it.
    pub fn resize(&mut self, width: f64, height: f64) {
        self.width = width.max(1.0);
        self.height = height.max(1.0);
    }

    /// The rectangle of the drawing now on screen.
    pub fn visible(self) -> Rect {
        let half_w = self.width / (2.0 * self.scale);
        let half_h = self.height / (2.0 * self.scale);
        Rect::new(self.x - half_w, self.y - half_h, self.x + half_w, self.y + half_h)
    }

    /// Where a node's coordinate lands in the window.
    pub fn to_screen(self, x: f64, y: f64) -> (f64, f64) {
        (
            (x - self.x) * self.scale + self.width / 2.0,
            (y - self.y) * self.scale + self.height / 2.0,
        )
    }

    /// Which node coordinate a point of the window stands for.
    pub fn to_node(self, sx: f64, sy: f64) -> (f64, f64) {
        (
            (sx - self.width / 2.0) / self.scale + self.x,
            (sy - self.height / 2.0) / self.scale + self.y,
        )
    }

    /// Moves the drawing by a distance measured in pixels.
    ///
    /// The arguments are how far the *pointer* went, and the drawing follows it:
    /// dragging right brings what was off the left edge into view.
    pub fn pan(&mut self, dx: f64, dy: f64) {
        self.x -= dx / self.scale;
        self.y -= dy / self.scale;
    }

    /// Multiplies the scale by `factor`, holding the point `(sx, sy)` of the
    /// window over the same node it was over.
    ///
    /// The anchor is honoured against the scale actually reached, so at the
    /// stops this does nothing rather than sliding the drawing sideways.
    pub fn zoom(&mut self, factor: f64, sx: f64, sy: f64) {
        let (nx, ny) = self.to_node(sx, sy);
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        // Put the node that was under (sx, sy) back under it: the offset from the
        // centre of the window is fixed in pixels, so it is this many nodes now.
        self.x = nx - (sx - self.width / 2.0) / scale;
        self.y = ny - (sy - self.height / 2.0) / scale;
    }

    /// One wheel gesture: `notches` positive scrolls down, and zooms out.
    pub fn zoom_notches(&mut self, notches: f64, sx: f64, sy: f64) {
        self.zoom(ZOOM_PER_NOTCH.powf(-notches), sx, sy);
    }

    /// Puts `subject` in the middle of the window, as large as it will go.
    ///
    /// An empty rectangle is not something one can look at, and leaves the
    /// camera alone.
    pub fn frame(&mut self, subject: Rect) {
        if subject.is_empty() {
            return;
        }
        let (cx, cy) = subject.centre();
        self.x = cx;
        self.y = cy;

        let w = subject.width().max(MIN_FRAMED_EXTENT);
        let h = subject.height().max(MIN_FRAMED_EXTENT);
        let clear = 1.0 - 2.0 * FRAME_MARGIN;
        self.scale = (self.width * clear / w)
            .min(self.height * clear / h)
            .clamp(MIN_SCALE, MAX_SCALE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bounding box built from nothing is empty, and holds whatever is added.
    #[test]
    fn a_box_grown_from_nothing() {
        let mut r = Rect::nothing();
        assert!(r.is_empty());
        assert!(!r.contains(0.0, 0.0));

        r.add(1.0, 2.0);
        assert!(!r.is_empty());
        assert_eq!((r.width(), r.height()), (0.0, 0.0));

        r.add(-1.0, 6.0);
        assert_eq!(r, Rect::new(-1.0, 2.0, 1.0, 6.0));
        assert_eq!(r.centre(), (0.0, 4.0));
        assert_eq!(r.to_square(), Rect::new(-2.0, 2.0, 2.0, 6.0));
    }

    /// Touching counts as intersecting: a node exactly on a split belongs to
    /// both sides rather than to neither.
    #[test]
    fn rectangles_that_only_touch_still_meet() {
        let left = Rect::new(0.0, 0.0, 1.0, 1.0);
        let right = Rect::new(1.0, 0.0, 2.0, 1.0);
        assert!(left.intersects(right));
        assert!(!left.intersects(Rect::new(1.5, 0.0, 2.0, 1.0)));
        assert!(!left.intersects(Rect::nothing()));
    }

    /// The two coordinate systems are inverses of each other.
    #[test]
    fn screen_and_node_coordinates_agree() {
        let camera = Camera::framing(Rect::new(0.0, 0.0, 100.0, 50.0), 800.0, 600.0);

        for &(x, y) in &[(0.0, 0.0), (100.0, 50.0), (-30.0, 12.5)] {
            let (sx, sy) = camera.to_screen(x, y);
            let (bx, by) = camera.to_node(sx, sy);
            assert!((bx - x).abs() < 1e-9 && (by - y).abs() < 1e-9);
        }

        // The centre of what is framed is the centre of the window.
        let (sx, sy) = camera.to_screen(50.0, 25.0);
        assert!((sx - 400.0).abs() < 1e-9 && (sy - 300.0).abs() < 1e-9);
    }

    /// What is framed fits, with the margin clear around it, and it is the
    /// *tighter* axis that sets the scale.
    #[test]
    fn framing_fits_the_tighter_axis() {
        let subject = Rect::new(0.0, 0.0, 200.0, 100.0);
        let camera = Camera::framing(subject, 400.0, 400.0);

        // 400 px over 200 nodes, less the margin: the wide axis is the tight one
        // in a square window.
        assert!((camera.scale() - 2.0 * 0.92).abs() < 1e-9);

        let seen = camera.visible();
        assert!(seen.min_x < 0.0 && seen.max_x > 200.0, "{seen:?}");
        assert!(seen.min_y < 0.0 && seen.max_y > 100.0, "{seen:?}");
    }

    /// A single node has no extent to divide the window by, and is still shown.
    #[test]
    fn framing_something_with_no_extent() {
        let camera = Camera::framing(Rect::new(7.0, 7.0, 7.0, 7.0), 400.0, 400.0);
        assert!(camera.scale().is_finite() && camera.scale() > 0.0);
        assert!(camera.visible().contains(7.0, 7.0));
    }

    /// Zooming holds the anchored point still — that is the whole of the gesture.
    #[test]
    fn zooming_holds_the_pointer_still() {
        let mut camera = Camera::framing(Rect::new(0.0, 0.0, 100.0, 100.0), 640.0, 480.0);
        let (anchor_x, anchor_y) = (120.0, 400.0);
        let before = camera.to_node(anchor_x, anchor_y);

        for _ in 0..8 {
            camera.zoom_notches(-1.0, anchor_x, anchor_y);
        }
        let after = camera.to_node(anchor_x, anchor_y);

        assert!((after.0 - before.0).abs() < 1e-9, "{before:?} {after:?}");
        assert!((after.1 - before.1).abs() < 1e-9, "{before:?} {after:?}");
        assert!(camera.scale() > 1.0, "scrolling up brought us closer");
    }

    /// Wheel up excludes nodes from the view, wheel down includes more: the
    /// direction the spec asks for.
    #[test]
    fn the_wheel_takes_nodes_in_and_out_of_view() {
        let mut camera = Camera::framing(Rect::new(0.0, 0.0, 100.0, 100.0), 400.0, 400.0);
        let wide = camera.visible().width();

        camera.zoom_notches(-1.0, 200.0, 200.0);
        assert!(camera.visible().width() < wide, "up shows fewer nodes");

        camera.zoom_notches(2.0, 200.0, 200.0);
        assert!(camera.visible().width() > wide, "down shows more");
    }

    /// At the stops the drawing stays put: a clamped zoom is a zoom that did
    /// nothing, not one that moved the picture.
    #[test]
    fn a_zoom_that_cannot_happen_moves_nothing() {
        let mut camera = Camera::framing(Rect::new(0.0, 0.0, 10.0, 10.0), 400.0, 400.0);
        for _ in 0..200 {
            camera.zoom_notches(-1.0, 10.0, 390.0);
        }
        assert_eq!(camera.scale(), MAX_SCALE);
        let stuck = camera.visible();

        camera.zoom_notches(-1.0, 10.0, 390.0);
        assert_eq!(camera.visible(), stuck);
    }

    /// Panning moves the drawing with the pointer, in pixels.
    #[test]
    fn panning_follows_the_pointer() {
        let mut camera = Camera::framing(Rect::new(0.0, 0.0, 100.0, 100.0), 400.0, 400.0);
        let (before, _) = camera.to_screen(50.0, 50.0);

        camera.pan(30.0, 0.0);
        let (after, _) = camera.to_screen(50.0, 50.0);

        assert!((after - before - 30.0).abs() < 1e-9, "the node came along");
    }

    /// Resizing keeps the middle of the window on the same node.
    #[test]
    fn resizing_keeps_the_middle() {
        let mut camera = Camera::framing(Rect::new(0.0, 0.0, 100.0, 100.0), 400.0, 400.0);
        let middle = camera.to_node(200.0, 200.0);

        camera.resize(900.0, 300.0);
        let still = camera.to_node(450.0, 150.0);

        assert!((still.0 - middle.0).abs() < 1e-9 && (still.1 - middle.1).abs() < 1e-9);
    }

    /// A window GTK has not laid out yet has no size, and the camera survives it.
    #[test]
    fn a_window_of_no_size_is_survivable() {
        let mut camera = Camera::framing(Rect::new(0.0, 0.0, 100.0, 100.0), 0.0, 0.0);
        camera.resize(0.0, 0.0);
        assert!(camera.visible().width().is_finite());
    }
}
