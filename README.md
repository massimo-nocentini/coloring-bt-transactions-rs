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
| `tree-view` | that drawing in a window, pannable and zoomable (needs GTK) |
| `tx-view` | the transactions themselves in that window, coloured (needs GTK) |

### `coloring-bt-transactions`

```text
coloring-bt-transactions [<record-limit>|all] [--stats]
                         [--rings|--sets|--weighted] [--sum]
                         [--png <file> [--blocks <n>] [--bin <n>]]
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
instead: one row per record, one column per block id, ink where the block is in
the colour, written as a lossless bilevel PNG — one bit a pixel, which is both
the smallest this compresses to and something every viewer opens.  `--bin <n>`
puts `n` consecutive transactions on a row, which is how a million rows becomes a
picture something will show you whole; `--blocks <n>` says how many columns to
draw, overriding the count the records are read for.  A PNG states both of its
dimensions in front of its first scanline and the height is the number of
records, so a picture always reads the records once before colouring them, and so
always wants an input that can be rewound.  `--stats` reports throughput and
memory.

### `tree-jp2`

```text
tree-jp2 <graph-basename> -o <file> [--zoom <n>]
```

Reads a [webgraph](https://github.com/vigna/webgraph-rs) BvGraph as a forest,
places every node with the non-layered tidy trees algorithm (van der Ploeg 2014),
and writes one pixel per node as a **lossless** JPEG 2000 — lossless because a
node *is* a single sample, and a quantiser spends that first.  The raster is
built a tile at a time and never exists whole, so a drawing far past the size of
memory can still be written.  `--zoom` repeats each pixel for graphs small enough
that a picture a few pixels across is not a picture.

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
