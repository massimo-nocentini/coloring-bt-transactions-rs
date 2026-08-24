//! # The drawing, flattened
//!
//! Between the layout and the window there is one more shape to give the tree.
//! The arena the layout works in is built for the layout: every node carries a
//! level, a child number, contour links, a `Vec` of children.  A window wants
//! none of that.  It wants, a hundred times a second, "which nodes are on
//! screen", and then for each of them a place, a colour, and something to say
//! when it is clicked.
//!
//! So the arena is walked once, into flat arrays, and dropped.  What is kept is
//! two vectors and a [`Quadtree`] over them:
//!
//! - `xy`, a centre per node, which is what the index and the ink both want and
//!   the only thing either of them wants often.
//! - `placed`, a node number and two links per node, touched only for the few
//!   nodes a frame actually draws.
//!
//! The split is not tidiness.  A coarse frame reads *every* visible node's
//! position and none of its metadata, so keeping the positions apart is the
//! difference between walking 16 bytes a node and walking 32.
//!
//! # Pre-order, and what it buys
//!
//! The walk is depth first, and a node's descendants are therefore the entries
//! that follow it, contiguously, up to [`Placed::subtree_end`].  A subtree is a
//! *range*.  That is the whole of the machinery behind selecting one: framing a
//! subtree is a scan of a slice, asking whether a node is in the selection is
//! two comparisons, and neither needs the arena, which by then is gone.
//!
//! Ranges also mean the ends can be filled in by one backward sweep instead of
//! by the walk: in pre-order a parent always precedes its children, so passing
//! down the array from the end and pushing each node's end up to its parent
//! settles every one of them.
//!
//! # The node the graph does not have
//!
//! [`forest`](super) may add a root standing for nothing, to give the layout the
//! single root it needs.  It is not in the drawing, so it is not in the scene:
//! the walk starts at its children instead, which become parentless nodes here
//! exactly as the several roots of the graph are.
//!
//! # The walk carries its own stack
//!
//! A chain of transactions spending each other is a tree a million levels deep,
//! and a depth-first walk that recurses is a million stack frames.  The stack
//! here is a `Vec`, which has no such ceiling — the same reason `forest` asks
//! the layout crate for its flat entry point.

// These three files are a small library with no `lib.rs` to live in -- `src/*.rs`
// belong to the main binary, so `tree-view` reaches them by `#[path]` and
// `tests/viewer_geometry.rs` does the same.  Compiled into a binary crate,
// anything one frame does not happen to call reads as dead; the surface is the
// point, so the lint goes rather than the surface.
#![allow(dead_code)]

use non_layered_tidy_trees::{Arena, NodeId};

use crate::camera::Rect;
use crate::quadtree::{Patch, Quadtree};

/// What a root has instead of a parent.
pub const NO_PARENT: u32 = u32::MAX;

/// Everything about a node that is not its position.
#[derive(Clone, Copy, Debug)]
pub struct Placed {
    /// The node's number in the graph that was read.
    pub graph: u32,
    /// The scene index of the node's parent, or [`NO_PARENT`].
    pub parent: u32,
    /// One past the last scene index of this node's subtree.
    ///
    /// Equal to the node's own index plus one exactly when it is a leaf, which
    /// is how [`Scene::is_leaf`] answers without looking at any children.
    pub subtree_end: u32,
}

/// A laid-out drawing, ready to be looked at.
pub struct Scene {
    xy: Vec<[f64; 2]>,
    placed: Vec<Placed>,
    index: Quadtree,
    bounds: Rect,
    radius: f64,
}

impl Scene {
    /// Flattens `arena`, laid out and rooted at `root`, and indexes it.
    ///
    /// Refused rather than truncated when the drawing has more nodes than a
    /// `u32` can name: the links here are indices, and a silently wrapped one
    /// would draw a picture of the wrong tree.
    pub fn of(arena: &Arena, root: NodeId) -> Result<Scene, String> {
        if arena.len() > u32::MAX as usize {
            return Err(format!(
                "{} nodes is more than this can index; the limit is {}",
                arena.len(),
                u32::MAX
            ));
        }

        let mut xy: Vec<[f64; 2]> = Vec::with_capacity(arena.len());
        let mut placed: Vec<Placed> = Vec::with_capacity(arena.len());

        // (node, the scene index of the nearest drawn ancestor).
        let mut stack: Vec<(NodeId, u32)> = vec![(root, NO_PARENT)];

        while let Some((id, parent)) = stack.pop() {
            let node = &arena[id];

            let parent = if node.isdummy {
                // It stands for nothing, so it is neither drawn nor a parent:
                // its children inherit whatever it inherited.
                parent
            } else {
                let here = placed.len() as u32;
                xy.push([node.x + node.w / 2.0, node.y + node.h / 2.0]);
                placed.push(Placed {
                    // `forest` numbers the arena one-based, keeping 0 for the
                    // node standing for nothing, which never reaches here.
                    graph: (node.idx - 1) as u32,
                    parent,
                    subtree_end: here + 1,
                });
                here
            };

            // Reversed, so that the first child is the first one off the stack
            // and the array comes out in the order the tree reads.
            for &child in node.children().iter().rev() {
                stack.push((child, parent));
            }
        }

        // A parent precedes its children, so one pass from the end settles every
        // subtree's extent: each node hands its own end up, already final.
        for i in (0..placed.len()).rev() {
            let parent = placed[i].parent;
            if parent != NO_PARENT {
                let end = placed[i].subtree_end;
                let up = &mut placed[parent as usize].subtree_end;
                *up = (*up).max(end);
            }
        }

        // Every real node is the same box, so one radius describes them all; it
        // is read off the drawing rather than assumed so that this file does not
        // have to agree with `forest` about a constant.
        let radius = arena
            .iter()
            .find(|n| !n.isdummy)
            .map_or(0.5, |n| n.w.max(n.h) / 2.0);

        let mut bounds = Rect::nothing();
        for p in &xy {
            bounds.add(p[0], p[1]);
        }
        let bounds = bounds.grown(radius);

        let index = Quadtree::over(&xy);

        Ok(Scene { xy, placed, index, bounds, radius })
    }

    /// How many nodes are drawn.
    pub fn len(&self) -> usize {
        self.placed.len()
    }

    pub fn is_empty(&self) -> bool {
        self.placed.is_empty()
    }

    /// The whole drawing, circles included, which is what to frame to see it all.
    pub fn bounds(&self) -> Rect {
        self.bounds
    }

    /// Half the side of a node's box: a circle's radius, in node units.
    pub fn radius(&self) -> f64 {
        self.radius
    }

    /// The index, for a caller that wants to report on it.
    pub fn index(&self) -> &Quadtree {
        &self.index
    }

    pub fn at(&self, i: u32) -> [f64; 2] {
        self.xy[i as usize]
    }

    pub fn node(&self, i: u32) -> Placed {
        self.placed[i as usize]
    }

    /// Whether the node has children, told from the extent of its subtree.
    pub fn is_leaf(&self, i: u32) -> bool {
        self.placed[i as usize].subtree_end == i + 1
    }

    /// The nodes of `i`'s subtree, `i` itself first: a contiguous range, which
    /// is what the pre-order walk was for.
    pub fn subtree(&self, i: u32) -> std::ops::Range<u32> {
        i..self.placed[i as usize].subtree_end
    }

    /// The box holding a subtree's circles, for a camera to frame.
    pub fn subtree_bounds(&self, i: u32) -> Rect {
        let mut bounds = Rect::nothing();
        let subtree = self.subtree(i);
        for p in &self.xy[subtree.start as usize..subtree.end as usize] {
            bounds.add(p[0], p[1]);
        }
        bounds.grown(self.radius)
    }

    /// Hands `patch` the nodes inside `seen`, summarising cells narrower than
    /// `resolution`.  See [`Quadtree::visit`].
    pub fn visit(&self, seen: Rect, resolution: f64, patch: &mut impl FnMut(Patch<'_>)) {
        self.index.visit(seen, resolution, patch);
    }

    /// The node nearest `(x, y)` within `within` of it: what a click asks.
    pub fn pick(&self, x: f64, y: f64, within: f64) -> Option<u32> {
        self.index.nearest(&self.xy, x, y, within)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use non_layered_tidy_trees::{layout, LayoutInput};

    /// Builds and lays out an arena from `(parent, child)` pairs over nodes
    /// `0..n`, with `root` the one nothing points at.  Node `i` of the graph is
    /// `idx` `i + 1`, as `forest` numbers them.
    fn laid_out(n: usize, arcs: &[(usize, usize)], roots: &[usize]) -> (Arena, NodeId) {
        let mut arena = Arena::new();
        let ids: Vec<NodeId> = (0..n).map(|v| arena.add_node(v + 1, 1.0, 1.0, 1.0, false)).collect();
        for &(p, c) in arcs {
            arena.push_child(ids[p], ids[c]);
        }

        let root = if roots.len() == 1 {
            ids[roots[0]]
        } else {
            let r = arena.add_node(0, 0.0, 0.0, 0.0, true);
            arena.set_children(r, &roots.iter().map(|&v| ids[v]).collect::<Vec<_>>());
            r
        };

        let mut input = LayoutInput::new(root);
        input.vertically = false;
        layout(&mut arena, &input);
        (arena, root)
    }

    /// The walk is pre-order, and subtrees are the ranges that follow.
    #[test]
    fn a_subtree_is_a_range() {
        //        0
        //      /   \
        //     1     4
        //    / \
        //   2   3
        let (arena, root) = laid_out(5, &[(0, 1), (0, 4), (1, 2), (1, 3)], &[0]);
        let scene = Scene::of(&arena, root).unwrap();

        assert_eq!(scene.len(), 5);
        let order: Vec<u32> = (0..5).map(|i| scene.node(i).graph).collect();
        assert_eq!(order, [0, 1, 2, 3, 4], "depth first, eldest child first");

        assert_eq!(scene.subtree(0), 0..5, "the root is the whole drawing");
        assert_eq!(scene.subtree(1), 1..4, "1 and the two under it");
        assert_eq!(scene.subtree(2), 2..3, "a leaf is itself");
        assert_eq!(scene.subtree(4), 4..5);

        assert!(!scene.is_leaf(0) && !scene.is_leaf(1));
        assert!(scene.is_leaf(2) && scene.is_leaf(3) && scene.is_leaf(4));

        assert_eq!(scene.node(0).parent, NO_PARENT);
        assert_eq!(scene.node(2).parent, 1);
        assert_eq!(scene.node(4).parent, 0);
    }

    /// The root standing for nothing is not drawn, and does not become anyone's
    /// parent: the graph's several roots stay roots.
    #[test]
    fn the_added_root_is_not_in_the_scene() {
        let (arena, root) = laid_out(4, &[(0, 1), (2, 3)], &[0, 2]);
        let scene = Scene::of(&arena, root).unwrap();

        assert_eq!(scene.len(), 4, "four nodes, and not a fifth");
        assert_eq!(scene.node(0).parent, NO_PARENT);
        assert_eq!(scene.node(2).parent, NO_PARENT);
        assert_eq!(scene.node(1).parent, 0);
        assert_eq!(scene.node(3).parent, 2);

        assert_eq!(scene.subtree(0), 0..2);
        assert_eq!(scene.subtree(2), 2..4);
    }

    /// Positions are centres, and the drawing's box has the circles inside it.
    #[test]
    fn positions_are_centres_and_the_box_holds_the_ink() {
        let (arena, root) = laid_out(3, &[(0, 1), (0, 2)], &[0]);
        let scene = Scene::of(&arena, root).unwrap();

        assert_eq!(scene.radius(), 0.5);
        // Unit boxes at the origin: the root's centre is half a node in.
        assert_eq!(scene.at(0), [0.5, 1.5]);

        let whole = scene.bounds();
        for i in 0..scene.len() as u32 {
            let [x, y] = scene.at(i);
            assert!(whole.contains(x - 0.5, y - 0.5) && whole.contains(x + 0.5, y + 0.5));
        }
    }

    /// Framing a subtree is a scan of its range, and it holds just that subtree.
    #[test]
    fn a_subtree_has_a_box_of_its_own() {
        let (arena, root) = laid_out(5, &[(0, 1), (0, 4), (1, 2), (1, 3)], &[0]);
        let scene = Scene::of(&arena, root).unwrap();

        let part = scene.subtree_bounds(1);
        assert!(part.width() < scene.bounds().width());
        for i in scene.subtree(1) {
            let [x, y] = scene.at(i);
            assert!(part.contains(x, y));
        }
        // Node 4 is in the drawing and not in this subtree.
        let [x, y] = scene.at(4);
        assert!(!part.contains(x, y));
    }

    /// The scene is indexed, and a click on a node finds it.
    #[test]
    fn the_scene_can_be_clicked_on() {
        let (arena, root) = laid_out(5, &[(0, 1), (0, 4), (1, 2), (1, 3)], &[0]);
        let scene = Scene::of(&arena, root).unwrap();

        for i in 0..scene.len() as u32 {
            let [x, y] = scene.at(i);
            assert_eq!(scene.pick(x, y, 0.5), Some(i));
        }
        assert_eq!(scene.pick(-100.0, -100.0, 0.5), None);
        assert_eq!(scene.index().len(), scene.len());
    }

    /// A chain a hundred thousand levels deep, walked on an ordinary stack.
    ///
    /// The shape a run of transactions spending each other's output has, and the
    /// reason the walk here holds its frontier in a `Vec`.
    #[test]
    fn a_chain_deeper_than_any_stack() {
        let n = 100_000;
        let mut arena = Arena::with_capacity(n);
        let ids: Vec<NodeId> = (0..n).map(|v| arena.add_node(v + 1, 1.0, 1.0, 1.0, false)).collect();
        for v in 0..n - 1 {
            arena.push_child(ids[v], ids[v + 1]);
        }
        let root = ids[0];

        let mut input = LayoutInput::new(root);
        input.vertically = false;
        non_layered_tidy_trees::flat::layout_flat(&mut arena, &input);

        let scene = Scene::of(&arena, root).unwrap();

        assert_eq!(scene.len(), n);
        assert_eq!(scene.subtree(0), 0..n as u32, "the whole chain hangs off the top");
        assert_eq!(scene.subtree(n as u32 - 1), n as u32 - 1..n as u32);
        assert_eq!(scene.node(n as u32 - 1).parent, n as u32 - 2);
    }
}
