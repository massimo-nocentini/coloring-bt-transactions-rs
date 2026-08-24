
I would like to implement a visualizer for very big trees based on GTK5 with a Cairo backend.

I would like to have a very simple GUI that presents a Cairo surface where to draw nodes in positions x and y
produced by the dependency `non-layered-trees-layout`.

I think that you have to tackle the following components:
- bindings to GTK5 in order to open the pane
- bindings to Cairo in order to draw shapes for the nodes
- starting from a Webgraph encoding, call the tidy tree layout and stores the x and y values in a quadtree
- efficiently show in the pane *just* the visible portion of the nodes
- implement a concept of camera that allows us to focus and select to a subset of nodes.
- on mouse wheel up and down, apply zoom in order to include more or exclude more nodes from the view