# Colouring the 2022 chain

Notes from an attempt to colour `/data/bitcoin/2022/finalBCUTXO_2022.scm` in
full, weighted, on this machine. Written 2026-09-04.

The short version, in two parts.

**The disk problem is solved.** Printing every term of every colour would have
been 102.5 TB against 4.3 TB free; `--moments` collapses each colour to the
three numbers a colour is made of and brings that to 37.1 GB, 7.4 GB
compressed. That is the command below.

**The memory problem is not.** The fold holds one colour per transaction with
unspent outputs, and both the number of those and the size of a colour grow
through the chain, so the store reaches this machine's 503 GB at roughly **1% of
the file** — whatever is being written at the far end. So the command below is
the right command and it still will not run to the end of the chain. What
follows is the measurements that say so, and what does fit.

Every command below is run from the root of this repository, against a
`cargo build --release` — they are written `./target/release/...` rather than
repeating a `cd` seven times.

## The machine and the input

| | |
|---|---|
| CPU | Intel Xeon Gold 6238R, 112 threads |
| RAM | 503 GB |
| `/data` | 6.5 TB, 4.3 TB free |
| input | `finalBCUTXO_2022.scm`, 149,968,404,213 B (139.7 GiB) |
| transactions | 778,613,438 (the last record's tx id is 778,613,437) |
| compressors present | `zstd` 1.5.5, `xz` 5.4.5. No `lz4`, `pigz` or `brotli`. |

## The command

`--moments` collapses each colour to the three numbers a colour is *made* of, so
this is the form that both fits the disk and says something a swatch would say:

```bash
./target/release/coloring-bt-transactions all --weighted --moments \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm \
| zstd -19 -T8 -o /data/bitcoin/2022/colors.zstd
```

Four tab-separated columns: the transaction id, the mean block its coins came
from, the spread about that mean, and the effective number of blocks it rests
on. Measured over records 1,400,000–1,500,000, where the colours are mature
rather than the near-trivial ones at the head of the chain:

```
  47.6 B/record raw, 9.5 B/record compressed  ->  37.1 GB -> 7.4 GB whole chain
```

against 4.3 TB free. The disk problem is gone.

### What "the colour" is, and what this is not

Hue from the mean, chroma from the concentration: a vivid colour means the coins
came from a narrow slice of history, a grey one means they were thoroughly
mixed. Those three columns are what such a colour is computed *from*, and they
are the right thing to store — a hex code cannot be sorted, bucketed, averaged
or joined, and these can, while the colour is a pure function of them and can be
recovered whenever it is wanted.

The driver does **not** emit a per-transaction swatch. `--palette` colours the
*picture* — a pixel per (transaction, block), read through a perceptual ramp
instead of as a grey — which is a different object with a block axis. A single
palette index per transaction would be a third output that does not exist; see
the end of this file.

### No `--threads`

It is refused with `--moments`, and correctly. A pool of formatters earns its
keep when formatting a line is expensive; here a line is four numbers, and
handing a worker a copy of the terms to collapse costs more than collapsing them
in place. Measured over 150,000 records of `make corpus`: 3.58s serial against
3.95s at two threads and 4.22s at eight.

So this run is one fold thread and nothing else, and the fold is the whole cost.

### Why `-19 -T8` and no `--long`

Both differ from what the terms output wanted, and for the same reason: this is
a trickle rather than a torrent.

The fold produces around 3,000 records a second at this depth and falling, which
at 47.6 bytes a record is some 140 KB/s. `zstd -19` compresses this data at
about 2.2 MB/s a thread — *fifteen times faster than it arrives* — so the
highest level is free here where it would have been the bottleneck on the terms
output. It buys 5.0:1 against `--adapt`'s 4.0:1, a quarter off the file.

`--long=27` is dropped because it earns nothing on this data: 1,191,944 bytes
with it against 1,192,447 without, on a 4.7 MB sample. The terms output repeated
across enormous lines and wanted a big window; four short numbers repeat locally
and the default window already catches it.

`-T8` is generosity — one thread would keep up — but it costs nothing and leaves
`--adapt` room if you switch to it.

### On zstd versus the alternatives

`zstd` is the right tool and nothing installed beats it. `lz4`, `pigz` and
`brotli` are not on this machine; `xz` is, but at 10–20× slower it would become
the bottleneck. The choice worth arguing about was never the compressor — it is
what to compute, which is the rest of this file.

## The command that does not work, and why it is worth writing down

This was the first attempt, and it is kept because the reason it fails is the
interesting part:

```bash
./target/release/coloring-bt-transactions all --weighted --threads 16 \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm \
| zstd --adapt --long=27 -T32 -o /data/bitcoin/2022/colors.zstd
```

Full terms, sixteen formatter threads, a big window for the long-range repeats
across fifty-kilobyte lines. Every part of that is the right call *for that
output*, and the output is 102.5 TB.

## Why it will not finish

Measured by running the fold over a prefix of the real file with
`--weighted --sum --stats`, which walks exactly the same terms as the full
output but prints one number a line, so the fold's cost is isolated from the
writer's:

```
  records      avg colour (blocks)   distinct terms held
    500,000           1,087            1.8 GB
  1,000,000           4,723           15.2 GB
  1,300,000           5,516           22.8 GB
  1,500,000           5,984           27.7 GB
```

Throughput over that span fell from 958k records/s to **3k records/s**, and
resident memory reached **32 GB at 1,500,000 records — 0.19% of the file.**

Two walls follow, and the first is the one that actually stops it.

### Memory, at about 1% of the file

Live terms grow as roughly `records^1.48` in this range. Extrapolating from 32
GB at 1.5 M records, the store reaches ~450 GB — the practical limit of this
machine — at about **8.9 million records, 1.14% of the file.**

This is not a property of the output format. The growth is in the *store*: the
driver holds one colour per transaction with unspent outputs, so the working set
is the UTXO set multiplied by how many blocks a colour names, and both of those
grow through the chain. `--sum`, `--png` and `--pdf` all run the same fold and
hit the same wall at the same place.

### Disk, by a factor of twenty-five — solved by `--moments`

A weighted term prints as about 22 bytes. If the average colour simply *froze*
at the 5,984 blocks measured at 1.5 M records — it will not; a colour can grow
toward the ~770,000 blocks in the chain — the full output would be

```
  778,613,438 records x 5,984 terms x 22 B  =  103 TB uncompressed
```

against 4.3 TB free. zstd would need 24:1 merely to break even, and the true
figure is far larger because the colours keep growing.

This wall is the one `--moments` removes: 37.1 GB rather than 102.5 TB, a factor
of about 2,800, because a line stops being proportional to the size of the
colour. **The memory wall above is untouched by it** — the blow-up is in the
store, not the writer, and every output mode runs the same fold.

## What does fit

### 1. A bounded prefix, which is the command above with a limit

```bash
./target/release/coloring-bt-transactions 5000000 --weighted --moments \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm \
| zstd -19 -T8 -o /data/bitcoin/2022/colors.zstd
```

Five million records stays under roughly 150 GB resident, and the output is
around 240 MB raw, 50 MB compressed. This is the one to actually run.

### 1b. The coloured picture, for a prefix

`--palette` reads the weighted picture's samples through a perceptual ramp
rather than as greys — the same bytes of image data, plus a 768-byte `PLTE`
chunk, so it is free. Grey has 254 levels and an eye reads perhaps thirty of
them, and these weights live in a fraction of a percent.

```bash
./target/release/coloring-bt-transactions 200000 --weighted --palette \
  --png /data/bitcoin/2022/colors.png --bin 64 \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm
```

The limit matters twice here. Memory, as ever; and geometry — the picture is one
column per block, so the whole chain would be some 770,000 columns by 778 M rows
before binning, which is more raster than anything will allocate. `--bin` folds
the rows and nothing folds the columns, so a raster of the whole chain is out
regardless of memory. `--pdf` and `--fold` do fold both onto a bounded canvas,
but they shade a cell by how much of it is inked and have no sample to look up
in a palette, so they refuse `--palette` rather than ignoring it.

### 2. `--sets` rather than `--weighted`

Four bytes a term instead of twelve, so about three times the reach for the same
RAM, and the lines are `1:<block>` rather than decimals, which compresses far
better. It answers a different question: *which* blocks the coins came from, not
how much came from each.

### 3. `--sum`, when a single column is what is wanted

One `f64` a line — about 20 bytes a record against roughly 130 KB — so the whole
chain's output would be some 16 GB rather than 103 TB. Note `--sum` refuses
`--threads`, deliberately: it collapses a colour to one number, so there is
nothing to spread and copying the terms to a worker costs more than adding them
up in place.

This fixes the *output* and not the memory, so it still wants a record limit.
Combined with one it is the option that gives a usable CSV column or a plot.

### And if the whole chain is really the goal

Then the blocker is the working set and not the encoding, and no compressor
choice touches it. It would need either a machine with far more memory, or a
different representation — the level-parallel windowing idea, which lets a
window of the chain be coloured and retired rather than held. That is a project,
not a flag.

### A per-transaction swatch, which does not exist yet

The smallest thing this could produce is one *palette index* a transaction: a
byte, quantising the (mean, spread, effective) triple onto a 256-entry ramp.
For the whole chain that is 778 MB before compression — fifty times smaller
again than `--moments`, and a picture of the entire chain at one pixel a
transaction.

It is a third output rather than a flag on an existing one, because it has no
block axis: `--png` draws a pixel per (transaction, block) and this would draw a
pixel per transaction, so the width, the `--blocks` count and `--bin` all mean
something else. It would also still hit the memory wall, being the same fold.
Worth building only once that wall is dealt with.

## Reproducing the measurements

The prefix run above, which is cheap because `--sum` writes almost nothing:

```bash
./target/release/coloring-bt-transactions 1500000 --weighted --sum --stats \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm > /dev/null
```

`--stats` reports, every 100,000 records, the elapsed time, the interval rate,
the live and peak term counts and how many transactions still hold a colour.
Average colour size is the live terms divided by that last column.

The output sizes quoted for `--moments` come from the mature end of that same
prefix, rather than the head of the chain where the colours are near-trivial and
a line is a third the width:

```bash
./target/release/coloring-bt-transactions 1500000 --weighted --moments \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm > moments
tail -100000 moments > mature          # records 1,400,000-1,500,000
zstd -19 -T8 -f mature -o mature.zst
```

which gives 47.6 bytes a record raw and 9.5 compressed.

For the synthetic corpora the crate's own performance numbers are taken over,
see `make corpus` and `examples/records.rs`.
