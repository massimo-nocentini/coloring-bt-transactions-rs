# Colouring the 2022 chain

Notes from an attempt to colour `/data/bitcoin/2022/finalBCUTXO_2022.scm` in
full, weighted, on this machine. Written 2026-09-04.

The short version: **the proposed command will not finish.** It runs out of
memory at roughly 1% of the file, and the output it would have written does not
fit on the disk by a factor of about twenty-five. What follows is the command,
why its thread split is what it is, and the measurements that say it cannot run.

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

```bash
cd /home/mnocentini/Developer/working-copies/coloring-bt-transactions-rs && \
./target/release/coloring-bt-transactions all --weighted --threads 16 \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm \
| zstd --adapt --long=27 -T32 -o /data/bitcoin/2022/colors.zstd
```

### Why `--threads 16` and not `auto`

`--threads n` spends `n` threads formatting lines and one more reading records,
leaving the main thread the fold. The fold is serial — record *N* may spend
record *N-1* — so it is the ceiling, and on these records it is reached early.

At the colour sizes measured below, formatting one line costs about
`6,000 terms x 133 ns = 800 us`, and the merge that produced it about `330 us`.
So three or four workers already cover the formatting, and past eight to sixteen
the extra threads wait on the fold. Sixteen leaves headroom as colours grow;
`auto` would start 111 workers to no purpose. With its reader and fold thread
that is 18 of the 112.

### Why `-T32` for zstd, and `--adapt`, and `--long=27`

The producer tops out around 300–400 MB/s (the fold's rate times the bytes a
line comes to), which eight zstd threads would already absorb. The reason to
give it 32 is `--adapt`: it spends spare capacity raising the *compression
level* rather than the throughput, so the extra cores buy ratio rather than
speed on a pipe that is not the bottleneck. Past 32 there is nothing left to
buy.

`--long=27` is a 128 MB window. It is worth it on this output specifically: a
line is a long run of `<coefficient>:<block>` terms and consecutive
transactions share most of their ancestry, so the repeats this catches sit far
apart — much further than zstd's default window reaches. Cost is
`workers x window x ~3` ≈ 12 GB, which is nothing against 503 GB.

### On zstd versus the alternatives

`zstd` is the right tool here and nothing installed beats it. `lz4`, `pigz` and
`brotli` are not on this machine; `xz` is, but at 10–20× slower it would become
the bottleneck and turn a producer-bound pipeline into a compressor-bound one.
The choice worth arguing about is not the compressor — it is what to compute,
which is the rest of this file.

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

### Disk, by a factor of twenty-five

A weighted term prints as about 22 bytes. If the average colour simply *froze*
at the 5,984 blocks measured at 1.5 M records — it will not; a colour can grow
toward the ~770,000 blocks in the chain — the full output would be

```
  778,613,438 records x 5,984 terms x 22 B  =  103 TB uncompressed
```

against 4.3 TB free. zstd would need 24:1 merely to break even, and the true
figure is far larger because the colours keep growing. The pipeline fills
`/data` and then the fold dies.

## What does fit

### 1. A bounded prefix — the honest form of the command

```bash
./target/release/coloring-bt-transactions 5000000 --weighted --threads 16 \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm \
| zstd --adapt --long=27 -T32 -o /data/bitcoin/2022/colors.zstd
```

Five million records stays under roughly 150 GB resident. The output is still
large — order 600 GB uncompressed, perhaps 30–60 GB after zstd — but it is
survivable, and it is a real answer about a real prefix of the chain.

### 2. `--sets` rather than `--weighted`

Four bytes a term instead of twelve, so about three times the reach for the same
RAM, and the lines are `1:<block>` rather than decimals, which compresses far
better. It answers a different question: *which* blocks the coins came from, not
how much came from each.

### 3. `--sum`, when a column is what is wanted

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

## Reproducing the measurements

The prefix run above, which is cheap because `--sum` writes almost nothing:

```bash
./target/release/coloring-bt-transactions 1500000 --weighted --sum --stats \
  < /data/bitcoin/2022/finalBCUTXO_2022.scm > /dev/null
```

`--stats` reports, every 100,000 records, the elapsed time, the interval rate,
the live and peak term counts and how many transactions still hold a colour.
Average colour size is the live terms divided by that last column.

For the synthetic corpora the crate's own performance numbers are taken over,
see `make corpus` and `examples/records.rs`.
