//! # The viewer's geometry, tested without a window
//!
//! `tree-view` is three parts.  Two of them — the camera and the quadtree, and
//! the flat scene between them and the layout — are arithmetic, and the third is
//! GTK.  Only the third needs a toolkit installed, and it is behind the `gui`
//! feature for that reason; but a module that only the feature-gated binary
//! names is a module whose tests only run when the feature is on, which would
//! leave the arithmetic untested exactly where it is easiest to test.
//!
//! So this file names them too.  It draws nothing and asserts nothing itself:
//! it exists so that `cargo test`, with no features and no GTK anywhere on the
//! machine, compiles `camera.rs`, `quadtree.rs` and `scene.rs` and runs the
//! tests written inside them.
//!
//! The modules are declared at this crate's root, in the same order and under
//! the same names `tree-view` gives them, because they refer to each other as
//! `crate::camera` and `crate::quadtree`.

#[path = "../src/bin/tree/camera.rs"]
mod camera;

#[path = "../src/bin/tree/quadtree.rs"]
mod quadtree;

#[path = "../src/bin/tree/scene.rs"]
mod scene;

/// The one thing this file has to check for itself: that the three modules do
/// compose, since every other test in it lives one level down.
#[test]
fn the_three_parts_fit_together() {
    let points: Vec<[f64; 2]> = (0..1_000).map(|i| [(i % 40) as f64, (i / 40) as f64]).collect();
    let tree = quadtree::Quadtree::over(&points);

    let mut camera = camera::Camera::framing(tree.bounds(), 800.0, 600.0);
    camera.zoom_notches(-6.0, 400.0, 300.0);

    let mut drawn = 0;
    tree.visit(camera.visible(), 1.0 / camera.scale(), &mut |patch| match patch {
        quadtree::Patch::Nodes(indices) => drawn += indices.len() as u32,
        quadtree::Patch::Cluster { count, .. } => drawn += count,
    });

    assert!(drawn > 0, "a camera on the drawing sees some of it");
    assert!((drawn as usize) < points.len(), "and, zoomed in, not all of it");
}
