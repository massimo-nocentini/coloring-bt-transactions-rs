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
| `block-pdf` | the complete bipartite blocks around a node, every arc of each, as a vector PDF |
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
default the colour is spelled out in full — each term as `coefficient:exponent`,
a comma between one term and the next and nothing after the last, so a colour
with no terms is an empty half-line rather than a missing one.  The colon is what
separates a term's two halves, and the two marks do not overlap: under
`--weighted` the coefficient is a decimal and brings dots of its own, so `0.5:3`
is half a unit of block 3, and a colour splits on `','` and each term on `':'`.
Coefficients are printed for all the `f64` is worth — the shortest text that
reads back as the same bit pattern — so nothing is rounded off the line.  The
default limit is 1,000,001 records, where the Scheme stops; `all` removes it.

`--sum` prints one number after the tab instead: `sum_b b * weight(b)`, the whole
colour collapsed to an `f64`.  A colour is a set and a set does not fit in a
column; this does, and because a weighted colour's terms sum to 1 the number is
the **weighted mean block id** — the centre of mass of the blocks the coins came
from.  A coinbase minted in block `b` prints exactly `b`; half the value from
block 0 and half from block 3 prints `1.5`.  So it reads on the same scale as a
block id, and the distance between two of them is a distance along the chain.
It is printed the way a coefficient is — the shortest text that reads back as
the same bits — so the number on the line is the `f64` the fold arrived at, with
no column to line up and nothing rounded off.  It adds up weights, so it selects
`--weighted` and contradicts the other two backends.  (This was a separate
`tx-mean` binary, which divided the sum by the total weight; the measured drift
from 1 is 5.6e-16, which moves a mean the size of a block id by around 1e-12, so
the division is gone.)

Three representations of a colour, all driven by the one loop:

- `--rings` (the default) — circular linked lists, the Knuth exercise.
- `--sets` — sorted arrays of block ids, several times faster.  Its output is
  byte-identical to `--rings`, which is what makes the pair a cross-check.
- `--weighted` — sorted arrays carrying a weight per block, so a colour says
  *how much* of the value came from each block rather than merely which ones:
  an input spending `amount` of a transaction's `total` carries that fraction of
  its ancestor's colour, and every colour sums to 1.  Different output, on
  purpose, so it is a separate mode rather than a flag on the others.

### Three numbers instead

`--sum` is the colour's first moment and nothing else, which loses more than it
looks like: a transaction whose coins all came from block 500,000 and one that
took half its value from block 0 and half from block 1,000,000 print the same
number, and they could hardly be less alike.

`--moments` prints the smallest summary that tells them apart — the mean block
id, the spread about it, and the **effective number of blocks** — as three
tab-separated fields, so a line is four columns and `cut -f2,3,4` is the triple:

```
199999   39946.98043478284   4640.739930886573   459.99999999999346
200000   65570.37794351605  10102.826758127023    11.286387658100137
200001   94406               0                     1
```

The first rests on some four hundred and sixty blocks; the second on eleven,
despite reaching twice as far across the chain; the third is a coinbase — one
block, no spread.

The last column is the participation ratio, `1 / sum_b weight(b)^2`. It counts
the blocks that *carry* the colour rather than the blocks that merely appear in
it: a colour that is 99% one block and 1% another answers 1.02, where the
support size would answer 2. So the spread and the effective count say
different things and neither implies the other — one is a distance along the
chain, the other a count of what holds the weight.

Four running sums in one pass, so it costs what `--sum` costs (3.58s against
3.57s over the same records).  Like `--sum` it needs weights, so it selects
`--weighted` and contradicts the other two backends, and it contradicts `--sum`.

### Threads

`--threads <n>` puts the formatting of lines on `n` threads and the reading of
records on one more, leaving the main thread nothing but the fold.  The output
is byte-identical at every width, which it has to be: diffing `--rings` against
`--sets` is how the fast backend is checked against the exercise.

Formatting is most of what a text run does, and by a long way.  Holding the fold
and the parse constant by comparing against `--sum` — which walks exactly the
same terms and prints one number instead of all of them — over the first 150,000
records of `make corpus`:

| | serial | threaded | the fold alone |
|---|---|---|---|
| `--weighted` | 50.44s | **5.22s** (16 threads) | 3.37s |
| `--sets` | 4.67s | **1.67s** (8 threads) | |
| `--rings` | 11.16s | **6.48s** (8 threads) | |

`make corpus` writes both corpora these numbers were taken over — see
[Measuring](#measuring) — so none of them has to be taken on trust.

The gap is the shortest round-tripping decimal a weighted coefficient prints as:
about 133ns a term against 15ns for the integer path, paid on every block a
colour names.  What crosses the thread boundary is a *copy* of the colour's
terms rather than a handle on it — twelve bytes a term against the hundred and
thirty nanoseconds one costs to format — so both set backends keep their `Rc`
and their layouts untouched, and the ring arena, which could not have been
shared at all, is in the table too.

The pool closes itself when it is dropped, not only when it is finished: `run`
returns early on a malformed record, on a colour spending an unknown transaction
and on a write error, and a detached writer thread would take up to a megabyte of
buffered lines with it.  An adversarial review of this code found exactly that —
2,088,890 bytes serial against about 1,250,000 threaded on a corpus with one bad
record at the end, ending mid-line and varying run to run — so it is now an
`impl Drop`, and a test asserts a dropped pool writes what a finished one would.

`--threads auto` asks the machine, less one for the fold.  Wider is not always
better: the pool saturates once the fold thread is the bottleneck, which is
around 8 for `--sets` and 16 for `--weighted` above.  `--sum` collapses a colour
to one number and so has nothing to spread; a picture has no lines at all.  Both
refuse the flag rather than ignoring it.

The text is enormous — a colour of a thousand blocks is some nine thousand bytes
of `1:<block>`, and a good part of it punctuation.  `--png` draws the same answer
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
         [--ghost]
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

`--ghost` draws the arcs the walk dropped, behind the tree it kept, dashed
and paler.  On this graph that is not a detail: the arcs are the disjoint
union of complete bipartite blocks `K(I, O)`, one per transaction, so when the
walk expands the first input of a block it meets, the whole of `O` hangs under
that one input and each later input finds its `|O|` successors drawn already
and keeps none — every block is flattened to a star `K(1, |O|)` with `|I| - 1`
childless nodes beside it, and three arcs in four are lost that way.  A ghosted
page carries both readings at once: the tree in its own grey, and under it the
biclique the tree turned into a star.  It is also *complete* — every arc the
graph has between two drawn nodes is on the page, as a tree arc if it was the
first to reach its head and as a ghost if it was not — with the one exception
the warning colour already names: a node the cut left unexpanded had its arcs
never looked at.  Only the kept pairs are capped, never the count, and a walk
that outruns the cap refuses to draw rather than hand back a page missing arcs
its caption would claim.

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

### `block-pdf`

```text
block-pdf <graph-basename> <transpose-basename> --root <id>[,<id>...] -o <file>
          [--depth <n>] [--max-blocks <n>] [--max-nodes <n>]
          [--fanout <n>] [--fanout-in <n>] [--fanout-out <n>]
          [--seed producing|consuming]
          [--horizontal] [--fill] [--width <pt>] [--max-height <pt>]
          [--mark <id>[,<id>...]] [--labels [compact]] [--label-base <id>]
          [--dash-cross] [--no-check]
```

The sibling of `tree-pdf` whose unit is the **block** rather than the node.
A payments graph is made of complete bipartite pieces K(I, O), one per
transaction — the outputs it spent on one side, the outputs it created on
the other, every arc between the two sides and no other arc into O — and a
spanning tree cannot show that: it keeps one arc per node, so a block with
two inputs is drawn as one input feeding everything and the other a bare
leaf, its arcs dropped and counted.  Three arcs in four of the graph go that
way.  `block-pdf` walks the graph block by block instead, breadth first from
the block a root names, and draws every block it admits as a two-row gadget
— inputs on one row, outputs on the row below, all |I|·|O| arcs between
them.  A node is drawn once, so consecutive blocks chain through the nodes
they share.  Nothing is ever dropped, and stderr says `dropped=0` on every
run so that a caption can quote it.

The transpose is mandatory, since a block cannot be found without knowing
what points at a node.  A root names the block that *produced* it by
default — the first output of a block is then the block's name, as the
tables key them — and falls back to the block it *feeds* when it is a
source; `--seed consuming` asks for the feeding block outright.  The reading
taken is said on stderr.

The scissors are `tree-pdf`'s, in blocks: `--depth` counts block levels
below the seed, `--max-blocks` and `--max-nodes` are budgets spent on the
nearest blocks, and `--fanout` (or one side at a time, `--fanout-in` and
`--fanout-out`) keeps the first and last inputs and outputs of a hub and
stands three dots in the warning colour for the rest, at a slot of their
own so the row keeps its spacing.  What the scissors leave out is declared:
an output whose consuming block is not on the page is a hollow ring in the
warning colour, a *frontier*, and the blocks the picture stops short of are
counted.  Even the hidden outputs behind an ellipsis are asked whether they
lead anywhere, so that the ellipsis never hides an unknown number of blocks.

Everything else is the same ink as `tree-pdf` — a filled disc has drawn
out-arcs, a hollow grey ring is a sink, `--mark` is blue over anything — plus
three readings of this binary's own.  Every input of a block sits on the
row above the block's outputs: the inputs some drawn block produced where
that block put them, and the others — the *outside inputs*, which no drawn
block produced — beside them, with a short dashed stub when something did
produce them and none when they are true sources.  An arc entering a block
from a drawn block other than the one above it is a *cross* arc, inked
exactly like every other arc — two arcs of one block must never look
different because of a layout accident — and counted; `--dash-cross` dashes
it for a reader who wants the quotient's non-tree edges visible.  A cross
arc is the one kind that can skip a row, so the arcs that cross a row over
a node they do not end at are counted too, as `over_nodes`, and the count is
0 on every figure of the paper.  And the one arc between two drawn nodes
that no drawn block owns — from a frontier output into an outside input, an
arc of a block the scissors cut — is dotted in the warning colour and
counted as *stray*.  Several roots are several seeds side by side, unless
one seed's outputs feed another, which is then drawn under it as the chain
it is.

Every block fetched is re-verified as complete bipartite before it is drawn
(outputs contiguous, every input's successors exactly the output side,
every output's predecessors exactly the input side, inputs before outputs),
and a block that fails is an error naming it, since a bipartite gadget of a
non-block would be the drawing lying.  The transpose half of the check is
what makes `dropped=0` certified rather than promised: no arc into a drawn
output comes from outside the drawn input side.  `--no-check` skips the
per-input and per-output probes on drawings of thousands of blocks; the
page is byte for byte the same.

`--labels` writes every node's id beside the node, along its row, where no
arc of the gadget can cross it (arcs meet a row only at its nodes; above and
below a node is exactly where they go); `--labels compact` writes one label
a row, beside the row's last slot — `first-last (n)` for an output row,
`first ... last (n)` for an input row, `+n` on the outward side of an
ellipsis — for the hubs where a label a node would overprint.  The number
in parentheses is always the block's side, |O| or |I|; an input row that
shows fewer than |I| because the rest are drawn where their producers put
them says `(k of n)`.  `--label-base` subtracts a base the caption states,
for nine-digit ids at seven points; the page keeps a margin on the label
side for the longest label.  Depth runs down the page by default, the past above the
event; `--horizontal` runs it across, for chains of small blocks.  `--fill`,
`--width` and `--max-height` are `tree-pdf`'s.

The report on stderr is one `key=value` per quantity under stable names —
blocks drawn and cut and beyond the cut, nodes by role, arcs by kind, the
frontier — and it is where every number a caption quotes comes from.

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

## Measuring

Every performance number above names a corpus, and `make corpus` writes it:

```sh
make corpus          # `records` and `flat`, about 180 MB the pair, under a second
```

`examples/records.rs` is the generator — a deterministic xorshift, no
dependencies, so a given `(records, window, per-block, seed)` is one exact file
on every machine.  It is not a Bitcoin simulator.  What it reproduces is the one
property the measurements turn on, how large a colour gets, and `--window` is the
whole knob:

- `--window 4000` (`records`) — a transaction reaches back across some hundreds
  of earlier ones, ancestry mixes, and colours grow to a few thousand blocks.
  This is the regime where formatting a line is most of the run.
- `--window 0` (`flat`) — a spend stays inside its own block, so every colour is
  one block and a line is ten bytes.  Nothing about the fold is interesting
  here; what it measures is the pipeline's own overhead, which is what chose the
  batch bounds `--threads` dispatches on.

Real records are neither exactly, which is why there are two.  A design
justified at only one end of that knob is a design that has not been measured.

```sh
cargo run --release --example records -- --window 4000 --records 200000 > small
```

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
