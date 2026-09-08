# Colouring the 2022 chain

Notes from an attempt to colour `/data/bitcoin/2022/finalBCUTXO_2022.scm` in
full, weighted, on this machine. Written 2026-09-04; updated 2026-09-08, when
the matrix and the per-transaction byte arrived.

The short version, in three parts.

**The disk problem is solved.** Printing every term of every colour would have
been 102.5 TB against 4.3 TB free; `--moments` collapses each colour to the
three numbers a colour is made of and brings that to 37.1 GB, and 8.5 GB
compressed at the level the command below uses. That is the command below.

**The memory problem is not.** The fold holds one colour per transaction with
unspent outputs, and both the number of those and the size of a colour grow
through the chain, so the store reaches this machine's 503 GB at roughly **1% of
the file** — whatever is being written at the far end. So the command below is
the right command and it still will not run to the end of the chain. What
follows is the measurements that say so, and what does fit.

**The parse is now paid once.** Each of those runs re-read 150 GB of text to
reach five numbers a record; `examples/matrix.rs` writes those five down in one
401.2 s pass, into 10.6 GB that every later run takes with `--matrix` and gets
byte-identical output from. It does not move the memory wall an inch — it is the
parse it removes, not the store.

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
| records | 778,613,440 — two more than the transactions, the two BIP-30 duplicate coinbases |
| transactions | 778,613,438 (the last record's tx id is 778,613,437) |
| compressors present | `zstd` 1.5.5, `xz` 5.4.5. No `lz4`, `pigz` or `brotli`. |

## The command

`--moments` collapses each colour to the three numbers a colour is *made* of, so
this is the form that both fits the disk and says something a swatch would say:

```bash
./target/release/coloring-bt-transactions all --weighted --moments \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm \
| zstd -12 -T16 -o /data/bitcoin/2022/colors.zstd
```

Four tab-separated columns: the transaction id, the mean block its coins came
from, the spread about that mean, and the effective number of blocks it rests
on. Measured over records 1,400,000–1,500,000, where the colours are mature
rather than the near-trivial ones at the head of the chain:

```
  47.6 B/record raw, 9.5 at zstd -19   ->  37.1 GB  ->  7.4 GB whole chain
                     10.9 at zstd -12                    8.5 GB whole chain
```

against 4.3 TB free. The disk problem is gone.

Those two numbers do not come from the same level, which this file did not say
and should have: the 9.5 is `zstd -19`, the level the reproduction below runs,
while the command above is `-12` for the reason given two sections down. Rerun
this session over the same 100,000 mature records — off the matrix, which
prints the same bytes — the tail is 4,758,888 bytes raw, 948,745 at `-19` and
1,088,863 at `-12` — so `-12` is 10.9 B/record and **8.5 GB for the chain**
rather than 7.4. The `-12` section below measures 12.28 B/record on a different
and later sample, which extrapolates to 9.6 GB; the two samples bracket the
answer. A gigabyte or two is what the level costs and two hours is what it
saves, and no figure here is anywhere near 4.3 TB.

If this file is going to be read more than once — a second projection, another
`K`, anything changed about what is emitted rather than what is folded — write
the matrix first and give the same run `--matrix` instead of the redirect. It
prints the same bytes and skips the 150 GB parse; see *The 150 GB parse* below.

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
palette index per transaction is a third output, and it now exists: `tint
--index`, a pass over the `--moments` file this run writes rather than anything
the driver does. See the end of this file.

### No `--threads`

It is refused with `--moments`, and correctly. A pool of formatters earns its
keep when formatting a line is expensive; here a line is four numbers, and
handing a worker a copy of the terms to collapse costs more than collapsing them
in place. Measured over 150,000 records of `make corpus`: 3.58s serial against
3.95s at two threads and 4.22s at eight.

So this run is one fold thread and nothing else, and the fold is the whole cost.

### Why `-12 -T16` and no `--long`

Both differ from what the terms output wanted, and for the same reason: this is
a trickle rather than a torrent.

The level is set by the knee, not by taste.  Measured on a 2,000,000-line tail
of a whole-chain moments run with `-T16`: `-3` 14.64 B/record at 627 MB/s, `-9`
12.42 at 151 MB/s, **`-12` 12.28 at 66 MB/s**, `-15` 12.19 at 22 MB/s, `-19`
9.98 at 4.4 MB/s.  The fold produces this output at about 48 MB/s, so `-12` is
the strongest level that still consumes faster than the fold produces — free —
and `-19`, which this file used to recommend, would turn a twelve-minute run
into two and a half hours for a further 19%.  That was a mistake: the earlier
reasoning measured `zstd -19` at 2.2 MB/s on a 4.7 MB sample held in cache and
concluded it was fifteen times faster than the producer, when against the real
byte rate it is fourteen times *slower*.

`--long=27` is dropped because it earns nothing on this data: 1,191,944 bytes
with it against 1,192,447 without, on a 4.7 MB sample. The terms output repeated
across enormous lines and wanted a big window; four short numbers repeat locally
and the default window already catches it.

`-T16` because the level is high enough that threads now matter; at `-12` a
single thread does not keep up with the fold.

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

## The 150 GB parse, which every run pays and need not

A third cost, and unlike the two walls above this one is now gone. Only five
things in a record are ever read — the block, the transaction id, each input's
amount and the transaction it spends, and how many outputs there are — and every
run reaches them by parsing 149,968,404,213 bytes of parenthesised text again.
`examples/matrix.rs` writes those five down once; `--matrix <file>` reads them
back. One streaming pass over the whole 2022 chain, 401.2 s:

```
  records, finalBCUTXO_2022.scm            149,968,404,213 B    1.0x   exact
  --csr (u32 row_ptr + u32 col + f32 val)   19,473,632,380 B    7.7x   LOSSY
  --compact (the default)                   10,601,433,514 B   14.1x   exact
  --structure, no amounts                    6,126,089,724 B   24.5x   shape only
```

778,613,440 records, 778,613,438 transactions, 2,044,897,328 nonzeros, 762,261
blocks. Where the compact file's bytes go: columns 5,317,886,057 (50.2%),
amounts 4,475,343,790 (42.2%), heads 804,399,854 (7.6%), blocks 762,261, and
header + `TXFIX` + `GROUPS` 3,041,488 — 0.03% of the file to make it
self-describing.

```bash
cargo build --release --example matrix
./target/release/examples/matrix --compact /data/bitcoin/2022/chain2022.foldmat \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm
```

Every command in this file then takes `--matrix
/data/bitcoin/2022/chain2022.foldmat` in place of its redirect — in place of,
not beside it: `--matrix` replaces standard input rather than adding to it, and
a run given both is refused rather than quietly leaving 150 GB unread.

```bash
./target/release/coloring-bt-transactions all --weighted --moments \
  --matrix /data/bitcoin/2022/chain2022.foldmat \
| zstd -12 -T16 -o /data/bitcoin/2022/colors.zstd
```

### What it saves, and what it does not

Not the fold, and the honest number is the smaller one. Over 20,000,000 records
with the fold as cheap as it goes (`--bands 1 --sum`), 17.8 s off the text
against 11.7 s off the matrix; at `--bands 1024 --moments`, where the fold is
most of the work, 70.5 s against 63.9 s. Both were run with the 150 GB warm in
this machine's 503 GB page cache, which is the case that favours the text. Cold,
the two artefacts are 12.5 minutes of reading against 50 seconds, at the
205–270 MB/s this array gives.

**It does not touch the memory wall.** The blow-up is in the store, and a matrix
changes the reader and nothing under it: a `--matrix` run holds the same colours
and dies at the same ~1.14% of the chain. What it removes is the *parse*, which
is the cost paid again on every re-run, every change of `K`, and every change to
what is emitted rather than to what is folded.

A run off a matrix prints byte for byte what the same run over the records
prints, which is the only claim that matters here. Checked at 50,000 records
(the three pictures, a `--rows` window, a run under the release oracle), 200,000
(`--rings`, `--sets --threads 8`, `--weighted`'s terms), 500,000 (six
backend/line combinations, capped and uncapped), 2,000,000 (both banded
backends, `--weighted --sum`, `--weighted --moments`, and a `--sets` pair of
294.6 GB a side) and 20,000,000 (`--bands 1024 --moments`, 1,149,593,609 bytes a
side). `--rings` and `--sets` still hash alike off a matrix.

### Checking it, since everything now depends on it

`--check` re-reads the 150 GB and compares field for field: 461.6 s at 8 MB
resident, every block id reconstructed from the coinbase rows alone, content
hash agreeing. `--verify` asks the same question without the records — a sweep
of the matrix alone against its content hash and every count, 108.6 s over the
chain at 5 MB resident, 98 MB/s.

`--verify` exists because a fold checks no whole-file invariant. Of 347,416
single-bit flips in a 20,000-record matrix, 102,220 are accepted by the per-row
invariants; and a flip at byte 700,007 of a 500,000-record matrix folds to exit
0, with an empty stderr and a different answer.

```bash
./target/release/examples/matrix --stats  /data/bitcoin/2022/chain2022.foldmat
./target/release/examples/matrix --verify /data/bitcoin/2022/chain2022.foldmat
./target/release/examples/matrix --check  /data/bitcoin/2022/chain2022.foldmat \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm
```

### Why the CSR — the obvious representation — is the lossy one

`--csr` writes exactly the `u32 row_ptr` + `u32 col` + `f32 val` that a sparse
matrix is normally written as, and it is still written, because an argument
about lossiness that is never run is only an argument. What the run says: an
`f32` weight has a worst relative error of 5.96e-8, only 28.10% of the chain's
weights round-trip at all, the per-row `|sum(w) - 1|` degrades from 1.02e-13 to
5.96e-8 before the fold composes anything, and replaying the fold on those
weights changes 44.71% of `--sum` lines and 98.92% of the coefficients in the
terms output. Half the answers moving is not a rounding difference.

The fix is not a wider float. A weight is an amount over the row's total, both
integers, so storing the raw `u64` satoshi amounts is exact *and* smaller —
4.48 GB against 8.18 GB of `f32`. The exact file is 1.84x smaller than the lossy
one; nothing was bought by the `f32`.

Two transforms were measured and refused, and are reserved as flag bits a reader
refuses so that neither is re-derived later and quietly believed. Sorting each
row's parents would save 0.89 GB and change 4.06% of lines, because the fold
walks a row's inputs in reverse file order and reassociating an `f64` sum is not
free. Deduplicating a row that spends the same transaction twice would save
0.13 GB and free that colour at the wrong instant.

## What does fit

### 1. A bounded prefix, which is the command above with a limit

```bash
./target/release/coloring-bt-transactions 5000000 --weighted --moments \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm \
| zstd -12 -T16 -o /data/bitcoin/2022/colors.zstd
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

A picture of the *whole* chain does exist, and it is the other object: `tint
--png` over a `--moments` file lays one pixel per transaction on a square, with
no block axis to run out of. See the swatch below.

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

**`--bands K --sum` was silently wrong until this session**, and this file
sends a reader to `--sum` in three places, so anyone who reached for a banded
backend to make one of them cheaper got the bug. The hand-over of the carried
moments was gated on `--moments` alone; `--sum` is the same first moment and
was not given it, so a binned `--sum` summed *band centres*: a block-0 coinbase
printed 372 at `--bands 1024` and 1,488.5 at `--bands 256` — half a band each —
and nothing refused it. Both forms take the carried moments now. A binned
`--sum` agrees with an exact `--weighted --sum` to 6.1e-9 blocks over the first
200,000 records, and it got *faster* — 2.70 s against 5.40 s over 2,000,000
records — because a handed-over first moment does not walk the terms at all.
`--weighted --sum`, which is what this section recommends, was never affected:
it has no bands to take centres of.

### And if the whole chain is really the goal

Then the blocker is the working set and not the encoding, and no compressor
choice touches it. It would need either a machine with far more memory, or a
different representation — the level-parallel windowing idea, which lets a
window of the chain be coloured and retired rather than held. That is a project,
not a flag. Writing the matrix does not help here either, and is not offered as
help: it removes the parse and leaves the store exactly as large.

### A per-transaction swatch, which now exists

`tint --index` — `examples/tint.rs`, backed by `oklch::tints()`. One *palette
index* a transaction: a byte, quantising the (mean, spread) pair onto a
256-entry table of 32 hues by 8 concentrations. It is not a fourth backend but a
pass over a `--moments` file that already exists, so it is run after the command
at the top of this file rather than instead of it:

```bash
zstd -dc /data/bitcoin/2022/colors.zstd \
| ./target/release/examples/tint --index /data/bitcoin/2022/colors.tint \
    --palette /data/bitcoin/2022/colors.plte
```

For the whole chain that is **778,614,320 bytes**, 1.0000011 a record: one byte
a record, an 80-byte header carrying the axis constants the colour was
quantised against, the 768-byte palette itself, and a 32-byte table naming every
place the transaction-id run breaks — four entries chain-wide, the BIP-30
duplicate coinbases, which are why the record ordinal and the transaction id are
not the same number. `--palette <file>` dumps the 768 bytes bare, in the order a
PNG `PLTE` chunk wants them. (That is `tint`'s `--palette`, not the driver's,
which colours the picture and is a different object.)

The split of the 256 was measured rather than chosen. Over 60,000,000 real
records, 32x8 gives 0.005328 mean and 0.014707 worst dE in Oklab against the
continuous tint, and a just-noticeable difference is about 0.02, so nothing
visibly moves. Quantising the spread rather than the concentration, and one bin
per doubling the way `tx-view` does — 0.007344 mean, 0.015578 worst — are worse
on both statistics; 85x3 wins the mean and loses the worst at 0.027193, past a
JND, and the worst is what was minimised.

Two things this section used to say were wrong, and are worth keeping visible.
It said the swatch "would also still hit the memory wall, being the same fold".
It is **not** the same fold, and not a fold at all: `--index` reads a line of
`--moments` and writes a byte, holds no colours, and peaks at 5.4 MB resident
over 60,000,000 records. The wall is paid once, by the run that wrote the
moments; nothing is paid again here. And "778 MB before compression" was an
estimate, which the header, palette and break table make exact at 778,614,320.

The reasoning that held up is the reason it is a third output rather than a
flag: it has no block axis. `--png` draws a pixel per (transaction, block) and
this draws a pixel per transaction, so the width, the `--blocks` count and
`--bin` all mean something else. And a byte is derived from the moments file and
reproducible from it without replacing it — it says which of 256 boxes the mean
and the spread fell in, and nothing goes back the other way, so the moments are
what is kept.

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

which gives 47.6 bytes a record raw and 9.5 compressed — at `-19`, note, and not
at the `-12` the command at the top of this file recommends. The same `mature`
file compresses to 1,088,863 bytes at `-12 -T16`, which is the 10.9 B/record the
first section now quotes beside the 9.5:

```bash
zstd -12 -T16 -f mature -o mature.z12   # 1,088,863 B, 10.9 B/record
zstd -19 -T8  -f mature -o mature.z19   #   948,745 B,  9.5 B/record
```

Both of those runs re-parse a prefix of the 150 GB. If the same prefix is going
to be walked more than once, write the matrix first and point every run at it —
same bytes out, and `--matrix` takes the place of the redirect rather than
sitting beside it:

```bash
./target/release/examples/matrix --compact /data/bitcoin/2022/chain2022.foldmat \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm      # one pass, 401.2 s, 10,601,433,514 B
./target/release/examples/matrix --verify /data/bitcoin/2022/chain2022.foldmat
                                                 # 108.6 s, 5 MB resident, 98 MB/s
./target/release/coloring-bt-transactions 1500000 --weighted --sum --stats \
  --matrix /data/bitcoin/2022/chain2022.foldmat > /dev/null
```

A matrix that ends before the record limit prints the rows it has and says so on
stderr, the way the text reader stops at the end of the file, so a short matrix
is not a refusal.

The swatch, from the moments file that already exists — it reports the bytes
written, how many of the 256 entries were reached, how many breaks it found in
the transaction-id run, and the mean and worst dE it cost:

```bash
zstd -dc /data/bitcoin/2022/colors.zstd \
| ./target/release/examples/tint --index colors.tint --palette colors.plte
```

The binned `--sum` that was wrong, against the exact one, which is the check
that now stands behind it:

```bash
./target/release/coloring-bt-transactions 200000 --bands 1024 --sum \
  --matrix /data/bitcoin/2022/chain2022.foldmat > binned
./target/release/coloring-bt-transactions 200000 --weighted --sum \
  --matrix /data/bitcoin/2022/chain2022.foldmat > exact
```

They agree to 6.1e-9 blocks; before this session the first printed band centres,
372 blocks out at `--bands 1024`.

For the synthetic corpora the crate's own performance numbers are taken over,
see `make corpus` and `examples/records.rs`.
