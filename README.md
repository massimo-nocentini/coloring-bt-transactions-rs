# coloring-bt-transactions

Where did a Bitcoin transaction's coins come from?

Give every transaction a **colour**: the set of blocks its coins descend from.
A coinbase is coloured by the block that minted it; every other transaction is
coloured by the union of the colours of the transactions its inputs spend.  That
one rule, run over a stream of transaction records, is what this crate computes —
and then prints, draws as a bitmap, writes as a PNG, or shows in a window you
can pan and zoom around.

It began as a port of the driver at the bottom of `src/test/circular-polynomial.scm`,
a Chicken Scheme program built on Knuth's circular-list exercise (*TAOCP* §2.2.4).
A colour is a polynomial there — `x^b` per block, combined with `ior` — and since
`ior` on coefficients that are only ever 1 is union on exponents, a colour is a
*set*.  Both readings are still in here, and the fact that they agree byte for
byte is what checks the fast one.

Full API documentation, rendered by rustdoc from the source:
**<https://massimo-nocentini.github.io/coloring-bt-transactions-rs/>**

## The records

Read from standard input, whitespace agnostic, one s-expression per record:

```text
((timestamp block-id tx-id _ _ _ _) (input ...) (output ...))
input  = (addr-id amount prev-tx-id vout)
output = (addr-id amount _)
```

Five things are ever used — the header's `block-id` and `tx-id`, each input's
`amount` and `prev-tx-id`, and how many outputs there are.  Everything else is
skipped without being converted.  The reader is streaming and allocates nothing
per record; a colour is dropped as soon as its transaction's last unspent output
is spent, so what a run holds tracks the UTXO set rather than the whole chain.

## The binaries

| | |
|---|---|
| `coloring-bt-transactions` | the colouring itself: one line per record, or a picture |
| `tree-jp2` | a webgraph laid out as a tree and written as a lossless JPEG 2000 |
| `tree-pdf` | one node's subtree, laid out the same way and written as a vector PDF |
| `tree-view` | that drawing in a window, pannable and zoomable (needs GTK) |
| `tx-view` | the transactions themselves in that window, coloured (needs GTK) |

### `coloring-bt-transactions`

```text
coloring-bt-transactions [<record-limit>|all] [--stats]
                         [--rings|--sets|--weighted] [--sum]
                         [--png <file>|--pdf <file>|--fold <file>|--view]
                         [--blocks <n>] [--bin <n>] [--rows <a>..<b>] [--gain <x>]
                         < records
```

One line out per record: the transaction's id, a tab, and then its colour.  By
default the colour is spelled out in full — each term as `(exponent .
coefficient)` followed by a space, which is byte for byte what the Scheme's
`(print* (car p*) " ")` produces, so `cut -f2` off this output is what the
reference can be diffed against.  The default limit is 1,000,001 records, where
the Scheme stops; `all` removes it.

`--sum` prints one number after the tab instead: `sum_b b * weight(b)`, the whole
colour collapsed to an `f64`.  A colour is a set and a set does not fit in a
column; this does, and because a weighted colour's terms sum to 1 the number is
the **weighted mean block id** — the centre of mass of the blocks the coins came
from.  A coinbase minted in block `b` prints exactly `b`; half the value from
block 0 and half from block 3 prints `1.500000`.  So it reads on the same scale
as a block id, and the distance between two of them is a distance along the
chain.  It adds up weights, so it selects `--weighted` and contradicts the other
two backends.  (This was a separate `tx-mean` binary, which divided the sum by
the total weight; the measured drift from 1 is 5.6e-16, six orders of magnitude
below the last decimal printed, so the division is gone and the output is
unchanged.)

Three representations of a colour, all driven by the one loop:

- `--rings` (the default) — circular linked lists, the Knuth exercise.
- `--sets` — sorted arrays of block ids, several times faster.  Its output is
  byte-identical to `--rings`, which is what makes the pair a cross-check.
- `--weighted` — sorted arrays carrying a weight per block, so a colour says
  *how much* of the value came from each block rather than merely which ones:
  an input spending `amount` of a transaction's `total` carries that fraction of
  its ancestor's colour, and every colour sums to 1.  Different output, on
  purpose, so it is a separate mode rather than a flag on the others.

The text is enormous — a colour of a thousand blocks is fourteen thousand bytes
of `(block . 1)`, and most of it punctuation.  `--png` draws the same answer
instead: one row per record, one column per block id, and a pixel saying what the
colour says about that block, written as a lossless greyscale PNG.

- unweighted, that is black where the block is in the colour and white where it
  is not — every coefficient is 1, so two tones are the whole answer, and the
  file is one bit a pixel: both the smallest this compresses to and something
  every viewer opens.
- under `--weighted` a pixel is the **grey the weight comes to**: black for the
  whole of the transaction's value, white for none of it, and 254 shades
  between, at eight bits a pixel.  The scale is `weight^(1/2.2)` rather than the
  weight itself — weights decay by roughly a factor per hop of ancestry, so most
  of a real colour is a fraction of a percent and a linear grey would draw it as
  blank paper.  Darker is heavier; twice as dark is not twice the value.

`--bin <n>`
puts `n` consecutive transactions on a row — the union of their colours, and the
darker of two shades where they meet — which is how a million rows becomes a
picture something will show you whole; `--blocks <n>` says how many columns to
draw, overriding the count the records are read for.  A PNG states both of its
dimensions in front of its first scanline and the height is the number of
records, so a picture always reads the records once before colouring them, and so
always wants an input that can be rewound.  `--stats` reports throughput and
memory.

`--png` is the whole answer at full size, and at full size the default run is
135,659 columns by 1,000,001 rows: a well-formed 1.8 GB file that no reader will
open, because a hundred and thirty-five gigapixels is more raster than anything
will allocate.  `--pdf <file>` writes the same picture at a size a page can hold.
It folds the pixels onto a Cairo canvas of at most 1024 cells each way (which is
also the page in points) and shades each cell by how much of the rectangle it
covers is inked — coverage counted exactly, then through the same gamma so that
sparse ink is visible, so a darker cell means more ink but not proportionally
more.  A weighted pixel is worth its weight rather than a whole one there too, so
a weighted page is the lighter of the two wherever the ink genuinely is lighter.
Each axis is capped on its own, since a block id and a
position in the record stream are not the same kind of quantity and there is no
aspect ratio to keep.  `--blocks` and `--bin` mean what they mean for a PNG and
apply first.

It needs Cairo, which is a C library, so it is behind a feature — but only
Cairo, not a toolkit:

```sh
cargo run --release --features pdf --bin coloring-bt-transactions -- \
    all --sets --pdf out.pdf < records
make pdf RECORDS=<records-file> PDF=out.pdf          # the same
```

`--fold <file>` writes the same folded canvas as an 8-bit greyscale PNG rather
than a page: the one folded output a build without Cairo can produce, since the
fold is arithmetic and the PNG is the crate's own.  Two knobs go with the
folded outputs.  `--rows <a>..<b>` draws only that window of records as
picture rows — every record before the window is still coloured, because a
colour is the whole history of its coins, but only the window is drawn, which
is how a page gets one row per *transaction* around something interesting
instead of a thousand.  `--gain <x>` multiplies every cell's ink before the
gamma, clamped at full coverage: a weighted colour's mass is a distribution
over its whole width, so a folded weighted page is genuinely — and uselessly —
near white without it; the gain is the declared correction, and a caption that
quotes it is telling the truth about the picture.

`--view` shows that canvas instead of writing it: a GTK window over the picture
that one can move and zoom, with the panel reading out which block and which
record the pointer is over, and `e` writing what is on screen to a page of its
own.  It is the third thing that can be done with the one drawing, so it
contradicts `--png` and `--pdf` the way those contradict each other, and
`--blocks` and `--bin` shape the canvas for it exactly as they do for a page.
The fold is the resolution and a window is where that is felt: zooming past one
pixel a cell magnifies cells rather than uncovering finer ones, since refolding
would mean reading the records again.

| | |
|---|---|
| wheel up, wheel down | zoom in and out, about the pointer |
| drag | move the picture |
| `a`, `f`, `Home` | fit the whole picture in the window |
| `1` | one pixel a cell, about the pointer |
| `+`, `-` | zoom about the middle of the window |
| arrow keys | move the camera a tenth of the window |
| `e` | write what is on screen to a PDF beside the program |
| `q` | close |

Two drawings, changing over at the one number — how many pixels a cell is
across.  Below three of them a cell is drawn as a sample of the canvas, a
rectangle painted its own shade, because a cell around a pixel across can be
nothing else.  At three and above it is drawn as a **filled disc one cell
across**, in the same shade: the paper between the discs is the grid, so a cell
that is dark because it is full reads differently from a run of cells that are
dark together, and the diagonal edge of the drawing stops being a staircase of
squares.  Nothing appears or disappears at the changeover — only the shape of the
mark.

`--pdf` never reaches the second, drawing at one point a cell, which is below the
changeover whatever the page is looked at on: a disc of one point and a square of
one point are the same mark and the disc costs a path apiece, which for a canvas
of a million cells is a page nothing wants.  The way to a page of circles is the
window's own `e`, zoomed in, whose circles are bounded by the window rather than
by the canvas.

It wants GTK on top of Cairo, so it is behind the `gui` feature with the
viewers:

```sh
cargo run --release --features gui --bin coloring-bt-transactions -- \
    all --sets --view < records
make picture RECORDS=<records-file>                  # the same
```

### `tree-jp2`

```text
tree-jp2 <graph-basename> -o <file> [--zoom <n>]
         [--root <id>[,<id>...] [--depth <n>] [--max-nodes <n>] [--fanout <n>]]
```

Reads a [webgraph](https://github.com/vigna/webgraph-rs) BvGraph as a forest,
places every node with the non-layered tidy trees algorithm (van der Ploeg 2014),
and writes one pixel per node as a **lossless** JPEG 2000 — lossless because a
node *is* a single sample, and a quantiser spends that first.  The raster is
built a tile at a time and never exists whole, so a drawing far past the size of
memory can still be written.  `--zoom` repeats each pixel for graphs small enough
that a picture a few pixels across is not a picture.

`--root` starts the walk at the nodes one names instead of sweeping the whole
graph: the picture is those nodes' subtrees and nothing else, cut by the three
scissors of `tree-pdf` below, which mean here what they mean there.

### `tree-pdf`

```text
tree-pdf <graph-basename> --root <id>[,<id>...] -o <file>
         [--depth <n>] [--max-nodes <n>] [--fanout <n>]
         [--vertical] [--fill] [--width <pt>] [--max-height <pt>]
         [--mark <id>[,<id>...]] [--labels] [--spine <id>[,<id>...]]
         [--ancestors <transpose-basename>
          [--depth-up <n>] [--fanout-up <n>] [--max-nodes-up <n>]]
```

The drawing `tree-view` shows, cut down to the subtree of a node one names and
written as a vector page a paper can take.  The graph this exists for has two
billion nodes and no page holds two billion of anything, so the walk — the same
breadth-first spanning walk as everywhere else, first arc in wins, later arcs
dropped and counted — is pruned by three explicit scissors: `--depth` stops it
so many levels below the root, `--max-nodes` is a budget the breadth-first
order spends on the *nearest* part of the subtree, and `--fanout` caps how many
children a node may show, keeping the first and last of a big fan (half the
allowance each — a block's outputs are a contiguous id range, and what hangs
off its last output is not what hangs off its first).  A node the cut robbed of
successors is drawn in a warning colour, filled if it kept drawn children and
hollow if not, so a pruned frontier cannot pass itself off as a fringe of true
leaves; everything else is the viewers' ink — filled discs where something
hangs, hollow rings where nothing does.

Run against the **transpose** graph the same command draws a node's
*ancestors* — where its value came from rather than where it went — since the
transpose's successors are the graph's predecessors.

`--vertical` runs depth down the page instead of across it, which is what a
fan wants (one level deep, a thousand siblings broad) and a chain does not.
The scale is whatever fits `--width` points across and `--max-height` down,
whichever binds; the page then hugs the drawing.  `--fill` scales the two
axes *independently* to exactly the page asked for instead — the way
`tree-jp2` spends different pixels per unit on depth and breadth — because a
subtree of this graph is routinely a hundred times broader than it is deep,
and a uniform scale renders it as a ribbon eleven points tall on a page meant
to hold a plate.  What it costs is that a distance along one axis stops being
comparable to a distance along the other, which is why it is a flag and not
the default.

Three things exist for pointing at *one* object in a drawing of many.
`--mark` inks the named nodes in a colour of their own — the subject of a
figure, over whatever else those nodes are.  `--labels` writes each node's
graph id at its shoulder, neighbours taking turns above and below; for
drawings of tens of nodes, where a reader can be told which node is which.
`--spine` names the only nodes the walk may *expand*: everything else is
drawn one node deep and reported cut.  That is the shape of a chain — a coin
spent hop after hop, something peeled off each time — whose spine the graph
alone cannot follow (telling the continuing output from the peeled one takes
amounts, which a webgraph does not carry), so the caller computes it
elsewhere and hands it over, and the drawing becomes the chain with its legs
as fringe instead of a walk that chases every leg into the open economy.

`--ancestors <transpose>` composes both directions about one root — the
**hourglass**: the root's ancestry on the transpose mirrored on one side,
its descendants on the other, meeting at the one node they share.  On a
vertical page that puts where the coin came from above the event and where
it went below, which is the two questions a forensic figure is asked.  The
ancestor side takes its own scissors (`--depth-up`, `--fanout-up`,
`--max-nodes-up`), defaulting to the descendant side's.

The page is written by a small PDF writer of this crate's own — the header,
four objects, a deflated content stream, a cross-reference table, one
standard font for the labels — so like everything here but the windows it
needs no Cairo, no toolkit, nothing installed: a machine with a graph and a
Rust toolchain can make a figure.

### `tree-view` and `tx-view`

```text
tree-view <graph-basename> [--width <px>] [--height <px>]
tx-view   <records-file> [--limit <n>|all] [--width <px>] [--height <px>]
```

The same drawing in a GTK 4 window that draws only what is on screen: a quadtree
over the layout answers *which nodes are visible*, so a frame costs what the
window costs rather than what the drawing does.  `tree-view` shows a webgraph in
two tones; `tx-view` shows the transactions, each hung under the one its first
input spends, with hue for the oldest block in its colour and paleness for how
many blocks are in it — what mixing looks like, over a chain.

| | |
|---|---|
| wheel up, wheel down | zoom in and out, about the pointer |
| drag | move the drawing |
| click | select the node under the pointer, and its subtree |
| `f` | fill the window with the selection, or with everything |
| `a`, `Home` | back to the whole drawing |
| `p` | select the parent of the selection |
| arrow keys | move the camera a tenth of the window |
| `+`, `-` | zoom about the middle of the window |
| `c` | put the selection in the middle, without moving the zoom |
| `e` | write what is on screen to `tree-view-NNN.pdf` (`tx-view-NNN.pdf`) |
| `Escape` | select nothing |
| `q` | close |

A node drawn big enough to have an inside is filled when something hangs off it
and hollow — its colour on the rim, paper in the middle — when nothing does, so
the leaves of the tree read as a fringe rather than as more of the trunk.

`e` writes the camera, not the drawing: Cairo's PDF is a page description, so
every node on screen becomes an arc that a reader can zoom into without it going
soft, and every node off screen costs nothing because the same quadtree walk
never reaches it.  The way to get more of the tree onto a page is to zoom out
before pressing it.  The file is the first free number in the working directory,
so nothing is ever written over.

## Building

```sh
make build                     # cargo build --release
make test                      # the whole suite, no toolkit needed
make docs                      # rustdoc into docs/, as GitHub Pages serves it
```

The two windowed viewers are behind the `gui` feature, because GTK is a C library
and needs its headers and `pkg-config` on the machine — `libgtk-4-dev` on Debian
and Ubuntu, `gtk4-devel` on Fedora, `brew install gtk4` on macOS.  Everything else,
`cargo test` and `cargo doc` included, builds without it:

```sh
make view GRAPH=<graph-basename>     # tree-view
make tx-view RECORDS=<records-file>  # tx-view
make test-gui                        # the suite including the viewer's own tests
```

`make help` lists the rest, among them `docs-strict` (any rustdoc warning is an
error) and `asm-check` (the weight-scaling loops are still vectorised — nothing
in the source *says* they must be, so only the disassembly can confirm it).

The camera, the quadtree and the flat scene are arithmetic rather than GTK, and
`tests/viewer_geometry.rs` names those three modules so their tests run with no
toolkit anywhere on the machine.

## License

MIT.  See [LICENSE](LICENSE).
