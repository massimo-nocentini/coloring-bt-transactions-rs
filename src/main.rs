//! # Bitcoin transaction colors
//!
//! A port of the driver at the bottom of `src/test/circular-polynomial.scm`.
//!
//! Transaction records stream in on stdin (see [`sexp`]).  Each transaction is
//! given a **color**: the polynomial whose exponents are the ids of the blocks
//! its coins descend from.  A coinbase transaction is colored by the block that
//! minted it, `x^block`; every other transaction is colored by the bitwise-or of
//! the colors of the transactions its inputs spend.  Because `ior` on the
//! coefficients only ever combines 1s, a color is in effect a *set* of block
//! ids, held as a sorted ring — see [`poly`] for why that shape.
//!
//! `colors` holds one ring per transaction with unspent outputs, along with how
//! many of those outputs are still unspent.  Spending the last one drops the
//! entry, which is also what tells the arena the ring can be reused: the count
//! is the whole memory-management story here, and it is why the working set
//! tracks the UTXO set rather than the whole chain.
//!
//! One line goes out per record: the transaction's id, a tab, and then the
//! color.  What follows the tab is one of two things, and `--sum` is which:
//!
//! - by default, the color in full — each term as `coefficient:exponent`, terms
//!   separated by a comma and nothing trailing the last of them, so a
//!   transaction with no color prints nothing after the tab.  The colon is what
//!   takes a term apart: a weighted coefficient carries dots of its own, and
//!   `0.5:3` is half a unit of block 3.
//! - under `--sum`, one number — `sum_b b . weight(b)`, the color's terms added
//!   up.  See [`Line::Sum`] for what that number is and why it takes weights.
//!
//! # A picture instead
//!
//! That output is enormous — a color of a thousand blocks is some nine thousand
//! bytes of `1:<block>` — and a good part of every line is punctuation.  `--png
//! <file>` draws the same answer instead: one row per record in the order the
//! records arrive, one column per block id counting up from 0, and a pixel
//! saying what the color says about that block — black where the block is in it
//! and white where it is not, or, under `--weighted`, the grey that stands for
//! how much of the transaction's value came through it.  A lossless greyscale
//! PNG, one bit a sample where two tones are all there is and eight where they
//! are not; see [`image`] for why that format and what it costs.
//!
//! Two more knobs, both about size:
//!
//! - `--bin <n>` puts `n` consecutive transactions on one row, black where any
//!   of them reaches that block.  A million rows is a picture nothing will show
//!   you whole; binning is how it becomes one that will.
//! - `--blocks <n>` says how many columns to draw, overriding the count
//!   [`survey`] arrives at.  It is not a way of avoiding that pass: a PNG
//!   states its height in front of its first scanline too, and the height is the
//!   number of records, so a picture always reads the records once before it
//!   colors any of them and always wants an input that can be rewound.
//!
//! `--palette` draws that weighted picture through a colour ramp rather than as
//! a grey.  The samples are the same numbers and the file is the same size —
//! colour type 3 and a `PLTE` chunk, so the difference on disk is 780 bytes and
//! the image data is byte for byte what the greyscale run writes.  What it buys
//! is levels: grey has 254 of them and an eye reads perhaps thirty, and the
//! weights here live in a fraction of a percent, so most of what the picture
//! distinguishes is invisible in it.  The ramp is built in a perceptual space
//! and carries the magnitude on lightness, monotonically, so it stays readable
//! photocopied and to a colour-deficient eye; see [`oklch`].
//!
//! It needs a quantity to spend the levels on, so it wants `--weighted`, and it
//! colours pixels, so it wants `--png` — the folded outputs shade a cell by how
//! much of it is inked and have no sample to look up.  Both are refused rather
//! than ignored.
//!
//! # A page instead
//!
//! `--bin` shrinks the picture down one axis, and nothing shrinks it along the
//! other: the default run draws 135,659 columns by 1,000,001 rows, which is a
//! well-formed 1.8 GB PNG that no reader will open, because a hundred and
//! thirty-five gigapixels is more raster than anything will allocate.
//!
//! `--pdf <file>` draws the same answer onto a Cairo canvas of bounded size and
//! writes it as one page: the picture's pixels folded into the cells that cover
//! them, each cell shaded by how much of its rectangle is inked — where a
//! weighted pixel is worth its weight rather than a whole one, so the two
//! drawings say the same thing at either size.  The canvas is 1024 cells each
//! way, which is also the page in points.  `--blocks` and `--bin` mean what they
//! mean for a PNG and apply before any of this.
//!
//! It is behind the `pdf` feature, since Cairo is a C library the rest of the
//! crate has no reason to want installed: `cargo run --release --features pdf`.
//! The module that draws it, `page`, is feature-gated with it and so is absent
//! from a default `cargo doc` the way the viewers are; `cargo doc --features
//! pdf` builds its page, which is where what a cell says — and what the shading
//! does and does not promise — is written down.
//!
//! # A window instead
//!
//! `--view` shows that canvas rather than writing it: a GTK window one can move
//! and zoom over the picture, with `e` writing what is on screen to a page of
//! its own.  Zoomed in far enough for a cell to have room to be a shape rather
//! than a sample, a cell is drawn as a filled disc one cell across; zoomed out
//! it is the same image `--pdf` writes.
//!
//! It is the third thing that can be done with the one drawing, so it
//! contradicts `--png` and `--pdf` the way they contradict each other; `--blocks`
//! and `--bin` shape the canvas for it exactly as they do for a page.
//!
//! It needs GTK on top of Cairo and so sits behind the `gui` feature, with the
//! viewers: `cargo run --release --features gui -- --view < records`.  See
//! `window`, which `cargo doc --features gui` builds.
//!
//! # Backends
//!
//! Three representations of a color, chosen at the command line, all driven by
//! the one loop in [`run`] through [`store::ColorStore`]:
//!
//! - `--rings` (the default) — [`poly`], the Knuth exercise.
//! - `--sets` — [`colorset`], the same answer from sorted arrays, several times
//!   faster.  Its output is byte-identical to `--rings`, which is what makes the
//!   pair a usable cross-check.
//! - `--weighted` — [`weighted`], which answers a different question.
//!
//! # Weighted colors
//!
//! Plain coloring says *which* blocks a transaction's coins came from and gives
//! each a coefficient of 1.  `--weighted` says *how much* came from each: an
//! input spending `amount` out of a transaction's `total` carries that fraction
//! of its ancestor's color, so
//!
//! ```text
//!     C  =  sum_i  (amount_i / total) . C_i
//! ```
//!
//! Every color is then a distribution over block ids summing to 1, and
//! coefficients print as decimals rather than the integer 1.  That makes the
//! output *not* comparable with the Scheme's, which is why it is a separate mode
//! rather than a change to the existing one.
//!
//! Those decimals are printed for all the `f64` is worth: the shortest text that
//! reads back as the same bit pattern, which is exact and no wider than it has
//! to be — see [`push_f64`].  Weights decay by roughly a factor per hop of
//! ancestry, so a deep color's terms get very small, and small is where a fixed
//! format loses them; here they survive, at the cost of a column that does not
//! line up and of terms that vary in width.  [`Line::Sum`]'s one number is
//! printed the same way, for the same reason.
//!
//! # One number instead
//!
//! `--sum` prints `sum_b b . weight(b)` in place of the terms: the whole color
//! collapsed to one `f64`, which is what makes a color fit in a column of a CSV
//! or a plot.  Because the weights sum to 1 that sum is the **weighted mean
//! block id** — the centre of mass of the blocks the coins came from — so it
//! reads on the same scale as a block id and the distance between two of them is
//! a distance along the chain.  It needs weights to mean anything, so it selects
//! [`weighted`] the way `--weighted` does; see [`Line::Sum`].
//!
//! # Threads
//!
//! `--threads <n>` spends `n` threads on formatting lines and one more on
//! reading records, leaving this thread nothing but the fold.  The output is
//! byte-identical at every width — which it has to be, since diffing `--rings`
//! against `--sets` is how the fast backend is checked against the exercise,
//! and a threaded run has to stay usable for that.
//!
//! It is worth having because formatting is most of what a text run does.
//! Holding the fold and the parse constant by comparing against `--sum`, which
//! walks exactly the same terms and prints one number rather than all of them,
//! over the first 150,000 records of `cargo run --release --example records --
//! --window 4000` (see `examples/records.rs`, which is what makes these
//! checkable):
//!
//! ```text
//!                      serial   threaded          the fold alone
//!     --weighted       50.44s     5.22s (x16)              3.37s
//!     --sets            4.67s     1.67s (x8)
//!     --rings          11.16s     6.48s (x8)
//! ```
//!
//! So a weighted run is ten times faster and lands within two seconds of the
//! fold it cannot go below.  What crosses the thread boundary is a *copy* of
//! the color's terms rather than a handle on it — twelve bytes a term against
//! the hundred and thirty nanoseconds one costs to format — which is why the
//! backends keep their `Rc` and their layouts, and why [`poly`]'s arena, which
//! could not have been shared at all, is in the table too.  See [`emit`].
//!
//! Two things it is not for.  `--sum` collapses a color to one number, so there
//! is nothing to spread and copying the terms to spread it costs more than
//! adding them up here; it is refused rather than obeyed.  A picture has no
//! lines at all, so `--png`, `--pdf`, `--fold` and `--view` refuse it too.
//!
//! `--threads auto` asks the machine, less one for the fold.  Wider is not
//! always better: the pool saturates once this thread is the bottleneck, which
//! is around 8 for `--sets` and 16 for `--weighted` on the records measured
//! above, and past that the extra threads only contend.
//!
//! # Three numbers instead
//!
//! `--sum` is the colour's first moment and nothing else, which loses more than
//! it looks like: a transaction whose coins all came from block 500,000 and one
//! that took half its value from block 0 and half from block 1,000,000 print
//! the same number, and they could hardly be less alike.
//!
//! `--moments` prints the smallest summary that tells them apart — the mean
//! block id, the spread about it, and the effective number of blocks, three
//! tab-separated fields so a line is four columns:
//!
//! ```text
//!     199999   39946.98043478284   4640.739930886573   459.99999999999346
//!     200000   65570.37794351605  10102.826758127023    11.286387658100137
//!     200001   94406               0                     1
//! ```
//!
//! The first of those rests on some four hundred and sixty blocks, the second
//! on eleven despite reaching twice as far across the chain, and the third is a
//! coinbase — one block, no spread. `--sum` gives the first column alone and
//! calls the three of them comparable.
//!
//! The spread and the effective count are not the same measurement and neither
//! implies the other: a colour split evenly between two far-apart blocks has a
//! huge spread and an effective count of 2, and one spread thinly over a
//! thousand adjacent blocks has a small spread and an effective count of 1000.
//! One is a distance along the chain, the other a count of what carries the
//! weight — see [`Line::Moments`].
//!
//! It needs weights to mean anything, so it selects [`weighted`] the way
//! `--sum` does, and contradicts it. Four running sums in one pass over the
//! terms, so it costs nearly what `--sum` costs and not quite: over the records
//! the [`emit`] numbers are taken on, `--sum` takes 3.69s, 3.71s and 3.72s
//! where this takes 3.85s, 3.86s and 3.88s — about 4%, a gap eight runs apiece
//! never closed. The three extra multiply-adds are not where it goes; the line
//! is. Four columns against two writes 7,606,880 bytes against 3,241,478 over
//! those records, and two of the three extra fields are another shortest
//! round-tripping decimal each, which is the most expensive thing this program
//! formats. (An earlier reading had the two equal, at 3.58s against 3.57s.)
//!
//! # The records, already taken apart
//!
//! Only five things in a record are ever read — the header's block id and its
//! transaction id, each input's amount and the transaction it spends, and how
//! many outputs there are — and the file they are read out of is 150 GB of
//! parenthesised text.  `--matrix <file>` reads them out of a [`matrix`] file
//! instead: the same five fields, written down once in the shape the fold
//! wants them, so that every solve after the first is a sweep over 10.6 GB
//! rather than a re-parse of 150 GB.  `cargo run --release --example matrix`
//! writes one, and that page is where the format is, and what each of its
//! decisions cost.
//!
//! It is a swap of readers and nothing under them, so a run with `--matrix`
//! prints byte for byte what the same run over the records prints.  Over the
//! first 2,000,000 records of the 2022 chain — 283,104,637 bytes of text
//! against a 16,121,691-byte matrix — with every pair below held against the
//! other by `cmp`:
//!
//! ```text
//!                             records     matrix    and they agree on
//!     --bands 1024 --sum         2.70s      2.15s    42,873,474 bytes
//!     --bands64 256 --moments    3.20s      2.60s    83,959,641
//!     --bands 1024 --moments     5.40s      4.69s    86,128,731
//!     --weighted --sum         278.65s    273.73s    43,054,523
//!     --weighted --moments     302.73s    301.63s    90,754,069
//! ```
//!
//! Those five are the collapsed forms, whose output is small enough to put on a
//! disk twice.  `--sets` prints the whole colour, and over these records that is
//! 294,578,757,229 bytes a side; it was compared as a stream rather than as a
//! pair of files, and the two are the same byte for byte.  `--rings`, `--sets
//! --threads 8` and `--weighted` were compared the same way over the first
//! 200,000 records.  Two sizes there, not one: `--rings` and `--sets` print
//! block ids and come to 3,497,920,545 bytes, and `--weighted` prints a decimal
//! coefficient in front of every one of them and comes to 14,700,438,387 --
//! 4.20x the same colours, and 1:09 to produce, which is why the weighted
//! stream is the one that cost something to compare.  `--rings` and `--sets`
//! still hash alike there, so the crate's oldest cross-check survives the
//! change of reader.
//!
//! The first three rows are what the reader is worth and the last two are what
//! it is not, and both are worth keeping.  Getting these records in costs about
//! 0.6s more as text than as a matrix; where the colours are still small enough
//! for the fold to be cheap that is a fifth of the run, and where they have
//! grown into thousands of blocks the fold is five minutes and the reader is
//! inside the noise.  What `--matrix` buys at chain length is not that
//! half-second, it is the 150 GB that no longer has to come off a disk and
//! through a parser to get at 10.6 GB of content.
//!
//! Two things it does not change.  `--threads` still spends its `n` threads on
//! formatting lines, which is where a text run spends most of itself, and not
//! the extra one on reading: [`prefetch`] parses text, so a matrix is decoded
//! here between one colour and the next — worth the half-second the table
//! above prices it at, against a second definition of what a record is, which
//! is what a `Records::ahead` for matrices would be.  And the records on
//! standard input are not read at all: `--matrix` replaces them rather than
//! adding to them, so a run given both is refused rather than quietly reading
//! neither -- the usage line ended with an unconditional `< records` for as
//! long as the flag existed, and copying it put 150 GB next to a matrix that
//! made it dead weight.
//!
//! A matrix written with `--structure` carries the shape of `A` and none of its
//! amounts, so a weighted backend is refused on it by name rather than quietly
//! folding equal shares.  Its record count is not held against the run's at
//! all: a matrix that ends before the record limit prints the rows it has, the
//! way the text reader prints the records it has and then stops at the end of
//! the file.  [`oracle`]'s inequality was copied here once and did not carry --
//! a short oracle answers `NEVER` where the truth is "later" and silently frees
//! a colour the rest of the run still reads, where a short matrix simply ends,
//! with no wrong answer anywhere -- and it cost the runs `--matrix` is cheapest
//! on: the default limit reads 1,000,001 records, so `--sets --matrix` over a
//! 500,000-row matrix was refused outright while the same run over the same
//! 500,000 records as text printed its 500,000 lines.  A run that stops short
//! of a matrix says so on stderr, where it cannot get into the bytes the two
//! runs are compared on.
//!
//! # The other binaries
//!
//! This is the driver, and the rest of the crate is three programs that do
//! something else with the same colouring or the same layout.  Each is its own
//! page in these docs; what they have in common is here.
//!
//! - [`tree-jp2`](../tree_jp2/index.html) — a webgraph laid out as a tree and
//!   written as a lossless JPEG 2000, one pixel per node.  Nothing to do with
//!   transactions; what it has in common with the viewers is the layout, never
//!   the colouring.  It draws three tones rather than two, which at eight bits a
//!   sample is a precision every reader takes — see [`image`] for why the
//!   picture here does not.
//! - `tree-view` and `tx-view` — the same two drawings in a window one can pan
//!   and zoom, the second coloured by what this file computes.  They are behind
//!   the `gui` feature, since GTK is a C library the rest of the crate has no
//!   reason to want installed, so they are absent from a default `cargo doc`
//!   and from these pages; `cargo doc --features gui` builds them.

// A colour as `K` weights over bands of block ids, for the runs the exact
// polynomial does not fit in memory for.
mod bands;
// The camera `--view`'s window is moved by, which is the viewers' own -- see the
// note at the top of the file for why it sits here rather than beside them.
#[cfg(feature = "gui")]
mod camera;
mod colorset;
// Formatting lines on threads of their own, which is where a text run spends
// most of itself.  Beside the backends rather than inside one because it works
// off a copy of a color and so serves all three of them.
mod emit;
mod image;
// The five fields of a record, written down once in the shape the fold reads
// them, so that a solve is a sweep over 10.6 GB rather than a re-parse of 150.
// What `--matrix` reads; `examples/matrix.rs` is what writes one.
mod matrix;
// The perceptual space the palette ink is built in, and the ramp itself.
mod oklch;
// Reading the records once to learn when a colour is dead, rather than holding
// it for every output that merely could be spent.
mod oracle;
// The canvas `--pdf` and `--fold` fold the picture onto.  The fold itself is
// plain arithmetic and builds everywhere; only the Cairo surface `--pdf`
// paints it onto sits behind the feature, inside the module.  Declared here
// rather than beside the viewers because it is this binary's output, not
// theirs.
mod page;
mod poly;
// Reading the records on a thread of its own, which is what `--threads` spends
// its extra one on.
mod prefetch;
mod sexp;
mod simd;
mod store;
mod weighted;
// The window `--view` opens on the same canvas, which needs GTK on top of Cairo.
#[cfg(feature = "gui")]
mod window;

use poly::Coeff;
use std::collections::HashMap;

/// The fold's heap is a few hundred thousand blocks of tens of kilobytes to a
/// megabyte each, freed and reallocated at every merge, and glibc's malloc
/// degrades on exactly that shape: measured at 1.5 M records of the 2022 chain
/// a call had gone from 47 ns to 26 us and was a quarter of the fold.  Either of
/// these allocators holds the cost flat; the features pick one.
#[cfg(feature = "jemalloc")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

// jemalloc reads `_RJEM_MALLOC_CONF` from the environment (the `_RJEM_` is the
// prefix this crate builds it with).  One setting was tried and is worth
// recording rather than shipping: `thp:always`, transparent huge pages on every
// extent.  The fold's heap is tens of gigabytes touched once and kept, one page
// fault per 4 KiB -- 9-12 million faults at 1.5 M records of the 2022 chain --
// and 2 MiB pages cut that by 92%.  But on this machine (`defrag=madvise`,
// other work resident) the system time did not fall, because the kernel then
// compacts memory synchronously to find the huge pages, and the resident set
// grew 4%.  So it stays an environment variable for a quiet machine, not a
// default:
//
//     _RJEM_MALLOC_CONF=thp:always ./target/release/coloring-bt-transactions ...

#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::process::ExitCode;
use std::time::Instant;
use store::{ColorStore, RingStore};

/// The Scheme stops at `(> i 1000000)`, i.e. after 1,000,001 records.  Kept as
/// the default so a run reproduces the recorded output; override with `argv[1]`,
/// or pass `all` for no limit.
const DEFAULT_LIMIT: usize = 1_000_001;

/// How often `--stats` reports, in records.
const STATS_EVERY: usize = 100_000;

/// Which representation of a color to run with.  See [`store`] for what the
/// three have in common and why more than one of them exists.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// Circular linked lists: the Knuth exercise, and the default.
    Rings,
    /// Sorted sets of block ids.  Same output, several times faster.
    Sets,
    /// Sorted sets carrying a weight per block.  Different output.
    Weighted,
    /// A fixed vector of weights, one per band of block ids, in `f32` lanes.
    /// The binned form of `--weighted`; see [`bands`].
    Bands,
    /// The same in `f64` lanes: twice the memory, for measuring what the
    /// narrower lane costs.
    Bands64,
}

impl Backend {
    /// Whether the store carries a weight per term, which is what `--sum`,
    /// `--moments` and `--palette` need.
    fn weighted(self) -> bool {
        matches!(self, Backend::Weighted | Backend::Bands | Backend::Bands64)
    }
}

/// What a line says about a color, once the transaction's id and the tab are
/// out of the way.  `--sum` is the choice between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    /// Every term, as `coefficient:exponent`, with a comma between one term and
    /// the next.  The whole color, on one line.
    ///
    /// The two punctuation marks do not overlap, which is the point of using
    /// two: under `--weighted` the coefficient is a decimal and brings a dot of
    /// its own, so `0.5:3` reads as half a unit of block 3 and `split_once(':')`
    /// takes a term apart wherever the coefficient's dots fall.  A whole color
    /// splits on `','` and then each term on `':'`, with no rule about which
    /// occurrence to look at.
    Terms,
    /// One number: `sum_b b . weight(b)`.
    ///
    /// A color is a set and a set does not fit in a column; this is the number
    /// that does.  Since every weighted color is a distribution — the weights of
    /// a fold sum to 1 by construction and each operand sums to 1 by induction,
    /// which is the invariant [`weighted`] states and `--stats` reports drift
    /// against — the sum *is* the weighted mean of the block ids, and so reads
    /// on the same scale as a block id: a coinbase minted in block `b` prints
    /// exactly `b`, and a transaction taking half its value from block 0 and
    /// half from block 3 prints `1.5`.
    ///
    /// Printed the way a coefficient is — [`push_f64`], the shortest text that
    /// reads back as the same bits — so the number on the line *is* the `f64`
    /// the fold arrived at, and two sums that differ are two lines that differ.
    /// The column no longer lines up, which a fixed format used to buy; that
    /// went the way the terms went, since a reader that parses a field cares
    /// about the bits and one that eyeballs it can pad.
    ///
    /// Nothing here divides by the total to make the sum a mean.  It could, and
    /// an earlier `tx-mean` binary did; the measured drift from 1 over 42,000
    /// records is 5.6e-16, which moves a mean the size of a block id by around
    /// 1e-12 — the last few digits of what now prints, and nothing a reader of
    /// this number is asking about.  The division would buy nothing and would
    /// quietly turn a broken invariant into a plausible number, where a sum that
    /// is not a mean shows up as one.
    ///
    /// # Under `--bands`
    ///
    /// This is the first moment, which is linear in the colour, so [`bands`]
    /// carries it exactly beside its lanes and hands it over — the same
    /// hand-over [`Line::Moments`] takes, and for the same reason.  Summed back
    /// off the band indices a term would be worth the *centre* of its band, and
    /// a block-0 coinbase printed 372 at `--bands 1024` and 1,488.5 at `--bands
    /// 256`; handed over it prints 0.  Over the first 200,000 records of the
    /// 2022 chain -- the corpus [`Line::Moments`]'s 1.1e-8 is measured on --
    /// the worst disagreement between `--bands K --sum` and `--weighted --sum`
    /// is 6.10e-9 blocks, at either width, which is the same bound and for the
    /// same reason: it is the one number, arrived at the one way.
    Sum,
    /// Three numbers: the mean block id, the spread about it, and the effective
    /// number of blocks.
    ///
    /// [`Line::Sum`] is the first moment of the colour and nothing else, which
    /// is a real loss: a transaction whose coins all came from block 500,000
    /// and one that took half its value from block 0 and half from block
    /// 1,000,000 print the same number, and they could hardly be less alike.
    /// This is the smallest summary that tells them apart.
    ///
    /// - **mean** — `sum_b b . weight(b)`, exactly [`Line::Sum`]'s number, up
    ///   to the division by the total weight that makes it a mean rather than
    ///   a sum.  The weights sum to 1 by construction, so the two agree to
    ///   within the accumulated drift from that: over the first 50,000 records
    ///   of the 2022 chain, 237 lines differ in their last digit or two and the
    ///   worst relative difference is 9.8e-15.  The division is what makes this
    ///   a mean rather than a sum when the invariant is not exact, so the two
    ///   flags are cross-checkable but not byte-identical.
    /// - **spread** — the standard deviation of the block ids under those
    ///   weights.  Zero for a coinbase, and large for coins that have been
    ///   through a long history.  It is in blocks, so it reads on the same
    ///   scale as the mean.
    /// - **effective** — the participation ratio `1 / sum_b weight(b)^2`, the
    ///   number of blocks the colour *really* rests on.  It is 1 for a
    ///   coinbase, 2 for two blocks at half each, and stays near 1 for a colour
    ///   that is nearly all one block however long its tail of small terms.
    ///   That is what distinguishes it from the support size, which counts
    ///   every block that appears at any weight at all.
    ///
    /// The two say different things and neither implies the other: a colour
    /// split evenly between two far-apart blocks has a huge spread and an
    /// effective count of 2, and one spread thinly over a thousand adjacent
    /// blocks has a small spread and an effective count of 1000.
    ///
    /// Three tab-separated fields after the transaction's own, so a line is
    /// four columns and `cut -f2,3,4` is the triple.  Each printed the way
    /// [`Line::Sum`]'s number is, through [`push_f64`].
    ///
    /// One pass over the terms and four running sums, so this costs nearly what
    /// [`Line::Sum`] costs — about 4% more, and the extra is the two further
    /// decimals on the line rather than the arithmetic; see [`emit::Body`].
    ///
    /// # Under `--bands`
    ///
    /// The mean and the spread stay **exact, in blocks**.  They are linear in
    /// the colour, so [`bands`] carries them alongside its band lanes and hands
    /// them over rather than letting them be summed back off band indices,
    /// which would put the mean at the centre of a band -- out by up to 372
    /// blocks at `--bands 1024` and 1,488 at `--bands 256`.  Measured over the
    /// first 200,000 records of the 2022 chain the worst disagreement with the
    /// exact fold is 1.1e-8 blocks, whatever `K` is.
    ///
    /// The effective count is **not** exact there, and cannot be: it is
    /// `mass^2 / sum_b weight(b)^2`, quadratic in the colour, and no bounded
    /// per-colour state folds a quadratic.  Under `--bands` it counts the
    /// *bands* the colour rests on rather than the blocks, which is a smaller
    /// number -- over those same records it runs from 0.01 to 1.0 times the
    /// exact value, averaging 0.41.  It is still the right shape of quantity
    /// for comparing binned colours with each other; it is not comparable with
    /// an exact run's fourth column.
    Moments,
}

/// Where a finished color goes.
///
/// An enum rather than a trait because the choice is made once and the match is
/// per *record*: inside each arm the walk over the color's terms is still a
/// monomorphic closure, which is the loop that has to stay cheap.
enum Output {
    /// A line per record, on stdout.  The buffer is reused across records;
    /// `line` is a field rather than a local for that reason alone.
    ///
    /// The sink is boxed so that a test can read back what a run wrote, which
    /// costs a virtual call per *flush* rather than per record: the buffering
    /// happens on this side of the box.
    Text {
        out: io::BufWriter<Box<dyn Write>>,
        line: Vec<u8>,
        form: Line,
    },
    /// The same lines, formatted on a pool of threads and written in order by
    /// one more.  What `--threads` selects; see [`emit`] for why this is the
    /// part of a run worth parallelising and what crosses the thread boundary.
    Threaded(emit::Pool),
    /// A row of pixels per record, in a file.  See [`image`].
    Picture(image::Writer),
    /// The same picture folded onto a canvas and written as one PDF page.  See
    /// [`page`].
    #[cfg(feature = "pdf")]
    Page(page::Writer),
    /// The same canvas, written as a greyscale PNG instead of a page: the one
    /// folded output a build without Cairo can produce.
    Fold(page::Writer),
    /// The same canvas again, shown in a window rather than written anywhere.
    /// See [`window`].
    #[cfg(feature = "gui")]
    Window {
        canvas: page::Writer,
        /// What a page exported from the window is named after.
        stem: String,
    },
}

impl Output {
    fn emit<S: ColorStore>(
        &mut self,
        store: &S,
        color: &S::Color,
        tx_id: usize,
    ) -> io::Result<()> {
        match self {
            Output::Text { out, line, form } => {
                line.clear();
                // Which transaction this is, which the Scheme leaves to the
                // reader to count out.  A line that names itself is one that can
                // be sorted, joined and sampled; `cut -f2` puts back exactly the
                // line the Scheme printed.
                push_int(line, tx_id);
                line.push(b'\t');
                // The body is `emit`'s, and so is the threaded path's: one
                // definition of what a line says, driven here straight off the
                // store and there off a copy of the color.
                let mut body = emit::Body::new(&mut *line, *form, S::WEIGHTED)
                    .placed(store.placement())
                    .narrow(S::NARROW);
                // A store that carries the first two moments hands them over
                // rather than having them summed back off its terms.  Only
                // `bands` does, and only there do the two differ: its terms are
                // band indices, so summing them puts the mean at a band's
                // centre.  See `ColorStore::exact_moments`.
                //
                // Both collapsed forms take the hand-over, not just
                // `--moments`.  `--sum` is that same first moment printed on
                // its own, and while it was left out of this it was summed off
                // the band indices instead: a block-0 coinbase printed 372
                // under `--bands 1024` and 1,488.5 under `--bands 256` -- half
                // a band, the exact error the hand-over exists to remove --
                // and nothing on the command line refused the pair.  Extended
                // rather than refused, because the store already carries the
                // number `--sum` wants, exactly, beside its lanes.
                //
                // Whether the terms are walked at all is then a third thing.
                // `--moments` still walks them for the effective count, which
                // is `mass^2 / sum w^2`, quadratic, and so carried by nothing;
                // `--sum` has been handed its whole answer and walking them
                // would add it a second time, since `Body::term`'s `--sum` arm
                // has no exact moments to skip past.  So a handed-over `--sum`
                // skips the walk, which is also why a binned `--sum` run is
                // faster than a binned `--moments` one: 2.70s against 5.40s
                // over the first 2,000,000 records of the 2022 chain.
                let mut walk = true;
                if *form == Line::Moments || *form == Line::Sum {
                    if let Some(moments) = store.exact_moments(color) {
                        body = body.from_exact_moments(moments);
                        walk = *form != Line::Sum;
                    }
                }
                if walk {
                    store.for_each_term(color, |exponent, coefficient| {
                        body.term(exponent, coefficient)
                    });
                }
                body.finish();
                out.write_all(line)
            }
            // The same line, made somewhere else.  All this thread does is copy
            // the terms out of the store -- about a nanosecond each, against the
            // hundred and thirty a weighted one costs to format -- and hand them
            // on.
            Output::Threaded(pool) => {
                let mut snapshot = pool.stage(tx_id);
                if S::WEIGHTED {
                    store.for_each_term(color, |exponent, coefficient| {
                        snapshot.push_weighted(exponent, coefficient)
                    });
                } else {
                    // Every coefficient is 1, so only the block is worth
                    // carrying: four bytes a term rather than sixteen.
                    store.for_each_term(color, |exponent, _| snapshot.push_flat(exponent));
                }
                pool.dispatch(snapshot)
            }
            // The coefficient goes with the block: under the unweighted
            // backends it is 1 for every term there is and the pixel is simply
            // black, and under `--weighted` it is the shade the pixel is drawn
            // in.  Which of the two the picture was opened for is settled in
            // `plan`, since it is the depth of every sample in the file.
            Output::Picture(picture) => {
                store.for_each_term(color, |exponent, weight| picture.set(exponent, weight));
                picture.end_transaction()
            }
            // The same two calls: `page::Writer` wears `image::Writer`'s
            // interface precisely so that this loop does not know which of them
            // it is feeding.
            #[cfg(feature = "pdf")]
            Output::Page(sheet) => {
                store.for_each_term(color, |exponent, weight| sheet.set(exponent, weight));
                sheet.end_transaction()
            }
            Output::Fold(sheet) => {
                store.for_each_term(color, |exponent, weight| sheet.set(exponent, weight));
                sheet.end_transaction()
            }
            // The window folds the picture the way the page does and then shows
            // it, so up to here the two are the same run.
            #[cfg(feature = "gui")]
            Output::Window { canvas, .. } => {
                store.for_each_term(color, |exponent, weight| canvas.set(exponent, weight));
                canvas.end_transaction()
            }
        }
    }

    /// A line-per-record output over `sink`, buffered a megabyte at a time.
    fn text(sink: Box<dyn Write>, form: Line) -> Self {
        Output::Text {
            out: io::BufWriter::with_capacity(1 << 20, sink),
            line: Vec::new(),
            form,
        }
    }

    /// Close the output.  For the picture this is not a formality — the last
    /// rows, the end of the deflate stream and `IEND` are only written here, a
    /// page is not written at all until here, and for a window this is the
    /// window: it opens, and the call comes back when it is closed.
    fn finish(self) -> io::Result<()> {
        match self {
            Output::Text { mut out, .. } => out.flush(),
            // Closes the dispatch channels, lets the pipeline drain, and answers
            // whatever the writing thread made of it.
            Output::Threaded(pool) => pool.finish(),
            Output::Picture(picture) => picture.finish(),
            #[cfg(feature = "pdf")]
            Output::Page(sheet) => sheet.finish(),
            Output::Fold(sheet) => sheet.finish_png(),
            #[cfg(feature = "gui")]
            Output::Window { canvas, stem } => {
                window::show(canvas, &stem).map_err(io::Error::other)
            }
        }
    }
}

const USAGE: &str = "usage: circular-polynomial [<record-limit>|all] [--stats] \
                     [--rings|--sets|--weighted|--bands <K>|--bands64 <K>] [--sum|--moments] \
                     [--threads <n>|auto] \
                     [--png <file>|--pdf <file>|--fold <file>|--view] \
                     [--blocks <n>] [--bin <n>] [--rows <a>..<b>] [--gain <x>] [--palette] \
                     [--release-oracle <file>] [--matrix <file>] \
                     [< records, unless --matrix]";

/// Which of the three pictures was asked for.
///
/// The same drawing every time — the same rows, the same columns, the same ink —
/// so this decides where it goes and nothing about what it says.  The last two
/// share the folding as well and differ only in what becomes of it; the `page`
/// module says why the folding exists and `window` what a screen adds to it.
/// Both are feature-gated, as their writers are.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Sheet {
    /// `--png`: one pixel per (transaction, block), at whatever size that comes
    /// to.  See [`image`].
    Raster,
    /// `--pdf`: the same, folded onto a canvas that fits on a page.  See the
    /// `page` module.
    Folded,
    /// `--fold`: the same canvas, written as a greyscale PNG --- the folded
    /// picture a build without Cairo can still produce.
    FoldedPng,
    /// `--view`: the same canvas, in a window one can move and zoom.  See the
    /// `window` module.
    Shown,
}

impl Sheet {
    /// The option that asked for it, for the messages that have to name one.
    fn flag(self) -> &'static str {
        match self {
            Sheet::Raster => "--png",
            Sheet::Folded => "--pdf",
            Sheet::FoldedPng => "--fold",
            Sheet::Shown => "--view",
        }
    }
}

/// Standard input, as something that can be read a second time — when the
/// platform and the shell allow it.
///
/// A `dup` of the descriptor rather than the descriptor itself, because a `File`
/// closes what it holds when it drops and standard input is not ours to close.
/// The copy shares its file offset with the original, which is what a program
/// that reads the records and then puts them back should do.
///
/// Asking where we are is the question a pipe refuses, and refusing is how it
/// says it cannot be rewound.
#[cfg(unix)]
fn rewindable_stdin() -> Option<File> {
    use std::mem::ManuallyDrop;
    use std::os::fd::FromRawFd;

    let borrowed = unsafe { ManuallyDrop::new(File::from_raw_fd(0)) };
    let mut own = borrowed.try_clone().ok()?;
    own.stream_position().ok()?;
    Some(own)
}

#[cfg(not(unix))]
fn rewindable_stdin() -> Option<File> {
    None
}

/// How many bytes of records are still waiting on standard input, for the one
/// refusal that is about what was *not* asked for.
///
/// `--matrix` replaces the records, so a run that is given both has a redirect
/// nothing will read; [`plan`] refuses that, and this is the number the refusal
/// names.  Answering it costs no read and cannot block: the handle is already
/// dup'd and already positioned, so a regular file's remainder is arithmetic on
/// its metadata.
///
/// Zero for everything else, and that is the point of the `is_file` test rather
/// than a length test alone.  A pipe never reaches here -- [`rewindable_stdin`]
/// hands back nothing for one -- and `< /dev/null` is a character device, whose
/// length is a fiction; both of those are the shapes an empty redirect takes,
/// and neither is somebody handing this run records it asked to ignore.
fn waiting_records(source: Option<&mut File>) -> u64 {
    let Some(file) = source else { return 0 };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {
            let at = file.stream_position().unwrap_or(0);
            meta.len().saturating_sub(at)
        }
        _ => 0,
    }
}

/// Read the records once without coloring them, for the two numbers the picture
/// has to know before it can write anything.
///
/// Answers `(blocks, records)` — one past the largest block id the records
/// carry, which is how many columns the image needs, and how many records there
/// were, which is how many rows it needs.
///
/// One pass is enough because a color is a set of the blocks its transaction's
/// coins *descend* from, and an ancestor cannot be mined later than its
/// descendant: no color names a block beyond the one its own record sits in, so
/// the largest block id in the records bounds every pixel in the picture.
///
/// Only the records the run will actually reach are looked at, so a record limit
/// narrows the image rather than padding it out to a chain the run stops short
/// of.  This is the same walk the run itself makes and stops on the same
/// conditions, which is what makes the count it arrives at the count the picture
/// is drawn to.
fn survey(input: impl io::Read, limit: usize) -> io::Result<(usize, usize)> {
    let mut reader = sexp::Reader::new(input);
    let mut inputs: Vec<sexp::Input> = Vec::new();
    let (mut blocks, mut records) = (0, 0);

    while records < limit {
        match reader.next_record(&mut inputs)? {
            Some(record) => blocks = blocks.max(record.block_id + 1),
            None => break,
        }
        records += 1;
    }
    Ok((blocks, records))
}

/// The same two numbers off a matrix, for a run that stops short of the end of
/// one.
///
/// A matrix's header already holds both for the whole file, so the survey pass
/// is retired outright whenever the run reaches the end of it.  This is the
/// other case: a run stopping at record `limit` colours only the blocks the
/// first `limit` rows reach, and the header's count is one past the largest
/// block id in *all* of them.  The picture would come out wider than the run
/// that draws in it, which is a different picture and not the same answer.
///
/// It is bounded by the run rather than by the matrix, which is what makes it
/// cheap: `limit` rows and not a byte more.  The default limit's 1,000,001
/// rows of a whole-chain matrix are some ten megabytes, against the pass over
/// the records this replaces, and a run with no limit never gets here at all.
fn survey_matrix(path: &str, limit: usize) -> io::Result<(usize, usize)> {
    let mut reader = matrix::Reader::open(path, None)?;
    let mut inputs: Vec<sexp::Input> = Vec::new();
    let (mut blocks, mut records) = (0, 0);

    while records < limit {
        match reader.next_record(&mut inputs)? {
            Some(record) => blocks = blocks.max(record.block_id + 1),
            None => break,
        }
        records += 1;
    }
    Ok((blocks, records))
}

/// Refuse a matrix that cannot be opened twice, before the second open.
///
/// [`survey_matrix`] opens the path again and reads it from the start, which is
/// a thing only a file can do.  A fifo is the case that matters: the first open
/// consumed the bytes, and opening it a second time blocks in `File::open`
/// until somebody writes again -- so a run whose writer has already finished
/// hangs there forever, with no message and nothing to wait for.  Held against
/// a `stat` rather than against a timeout, since the question is what the path
/// *is*, and the same answer covers a socket, a device and a directory.
///
/// Only the short runs get here: a run that reaches the end of a matrix takes
/// both of the picture's dimensions off the header and never opens anything a
/// second time, so a matrix on a fifo still draws the same picture as one in a
/// file for every run that reads all of it.
fn opens_twice(path: &str) -> Result<(), &'static str> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() => Ok(()),
        Ok(meta) => Err(kind_of(meta.file_type())),
        // Nothing to stat is not this function's complaint to make: the matrix
        // was opened above, so whatever this is, the reader will say it better.
        Err(_) => Ok(()),
    }
}

/// What a path is, for the refusal above to name.
#[cfg(unix)]
fn kind_of(file_type: std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt;
    if file_type.is_fifo() {
        "a named pipe"
    } else if file_type.is_socket() {
        "a socket"
    } else if file_type.is_char_device() {
        "a character device"
    } else if file_type.is_block_device() {
        "a block device"
    } else if file_type.is_dir() {
        "a directory"
    } else {
        "not a file"
    }
}

#[cfg(not(unix))]
fn kind_of(file_type: std::fs::FileType) -> &'static str {
    if file_type.is_dir() {
        "a directory"
    } else {
        "not a file"
    }
}

/// Where the fold's records come from.
///
/// An enum for the reason [`Output`] is one: the choice is made once, before
/// the first record, and what it decides is where a `&[sexp::Input]` is
/// borrowed from — a borrow that outlives neither arm and so is easier to hand
/// back from one method than to hide behind a `dyn`.  It is also the whole of
/// what `--matrix` changes: the fold below this call does not know which of the
/// two it is being fed, which is what makes the two runs comparable byte for
/// byte.
enum Source {
    /// The records as text, parsed here between one colour and the next or on a
    /// thread several batches ahead.  See [`prefetch`].
    Records(prefetch::Records),
    /// The same records, decoded out of a [`matrix`] file.
    ///
    /// Decoded here and never on a thread of its own: [`prefetch`] parses text,
    /// and a second copy of its batching would be a second definition of what a
    /// record is.  What that costs is measurable and small — over the first
    /// 2,000,000 records of the 2022 chain the whole run comes in 0.55s to
    /// 0.71s *faster* off a matrix than off the records under the three
    /// `--bands` forms, so the decode is already cheaper than the parse the
    /// reading thread was hiding, and there is that much less left to hide.
    /// `--threads` still spends the rest of itself on the formatting, which is
    /// most of a text run.
    Matrix {
        reader: matrix::Reader<File>,
        inputs: Vec<sexp::Input>,
        record: Option<sexp::Record>,
    },
}

impl Source {
    /// The next record and its inputs, or `None` at the end.
    ///
    /// [`prefetch::Records::next`]'s shape, since one of the arms is that call
    /// and the other has to be substitutable for it.
    #[inline]
    fn next(&mut self) -> io::Result<Option<(&sexp::Record, &[sexp::Input])>> {
        match self {
            Source::Records(records) => records.next(),
            Source::Matrix {
                reader,
                inputs,
                record,
            } => {
                *record = reader.next_record(inputs)?;
                Ok(record.as_ref().map(|r| (r, inputs.as_slice())))
            }
        }
    }
}

/// Work out where the records come from and where the colors go.
///
/// The two are settled together because a picture states its size before its
/// first sample, so both of its dimensions have to be read off the records
/// before the first row can be written, and that costs the input a rewind.  The
/// error is the message to print, since every one of these is a complaint about
/// the command line rather than something to recover from.
fn plan(
    picture: Option<(Sheet, String)>,
    blocks: Option<usize>,
    bin: Option<usize>,
    window: Option<(usize, usize)>,
    gain: Option<f64>,
    ink: image::Ink,
    form: Line,
    threads: usize,
    narrow: bool,
    limit: usize,
    matrix: Option<&str>,
    weighted: bool,
) -> Result<(Output, Source, usize, usize), String> {
    // Held as a file when standard input is one, so that `survey` can read the
    // records and put them back.
    let mut source = rewindable_stdin();

    // The matrix, opened in front of every refusal that needs a record read,
    // because one of them is about what its header says and a run that is going
    // to be refused should be refused before it has folded anything.  The one
    // refusal ahead of even that costs no read at all.
    let reader = match matrix {
        None => None,
        Some(path) => {
            // `--matrix` replaces the records rather than adding to them, so
            // records redirected next to it are read by nothing at all.
            // Refused rather than ignored, the way every other flag that would
            // quietly do nothing here is -- and the usage line ended with an
            // unconditional `< records` for as long as this arm existed, so the
            // redirect is exactly what somebody copying that line would type
            // next to a 150 GB file.
            //
            // It costs no read: `rewindable_stdin` has already dup'd the
            // descriptor and asked it where it is, so what is still waiting on
            // a regular file is its length less that offset.  Nothing else
            // answers -- a pipe refuses the position question and so never gets
            // a handle at all -- and `< /dev/null` is a character device rather
            // than a file, so the empty redirect a wrapper script leaves behind
            // does not fire this.
            let waiting = waiting_records(source.as_mut());
            if waiting > 0 {
                return Err(format!(
                    "--matrix reads the records out of {}, so the {} bytes still on standard \
                     input would be read by nothing at all; drop one of the two",
                    path, waiting
                ));
            }
            // No record count is passed, and the run's own is not computed:
            // longer than the run and shorter than it are both fine, because a
            // matrix that runs out of rows ends there, which is what the text
            // reader does at the end of the file.  The release oracle's
            // inequality was copied here once and does not carry -- a short
            // oracle answers `NEVER` where the truth is "later" and silently
            // corrupts the fold, where a short matrix prints a prefix and
            // stops -- and what it cost was the cheap runs: the default limit
            // reads 1,000,001 records, so a 500,000-row matrix was refused
            // where the same 500,000 records as text printed 500,000 lines,
            // and only `all` escaped it, which is backwards.  A run that ends
            // short of its limit says so on stderr, in `run`, where the run
            // actually ends and where it cannot reach the bytes the matrix and
            // the records are compared on.
            let reader = matrix::Reader::open(path, None)
                .map_err(|e| format!("{}: {}", path, e))?;
            // A `--structure` matrix keeps the shape of `A` and none of its
            // amounts, and a reader has to fill the field with something: it
            // fills a 1, which is an equal share.  That is exactly right for
            // `--rings` and `--sets`, whose `S::WEIGHTED` is a constant `false`
            // and which never look at it, and quietly wrong for every backend
            // that does.  Refused rather than folded, as every other flag that
            // would silently do something else here is.
            if weighted {
                reader
                    .require_amounts()
                    .map_err(|e| format!("{}: {}", path, e))?;
            }
            Some(reader)
        }
    };

    let Some((sheet, path)) = picture else {
        if let Some(name) = blocks
            .map(|_| "--blocks")
            .or(bin.map(|_| "--bin"))
            .or(window.map(|_| "--rows"))
        {
            return Err(format!(
                "{} describes the picture, so it needs --png <file>, --pdf <file>, \
                 --fold <file> or --view",
                name
            ));
        }
        // `io::stdout()` rather than a lock of it, because the writing happens
        // on a thread of the pool's own and a `StdoutLock` does not cross one.
        // Nothing is lost: the megabyte of buffering is on this side of it, so
        // the lock is taken once a megabyte either way.
        let output = match threads {
            0 => Output::text(Box::new(io::stdout().lock()), form),
            n => Output::Threaded(emit::Pool::with_lanes(
                Box::new(io::stdout()),
                form,
                n,
                narrow,
            )),
        };
        return Ok((output, feed(reader, source, threads), 0, limit));
    };

    // A picture has no lines to format, so there is nothing for a pool of
    // formatters to do with one.  Refused rather than ignored, for the reason
    // `--blocks` without a picture is: a flag that quietly does nothing is a run
    // doing something other than what was asked.
    if threads > 0 {
        return Err(format!(
            "--threads spreads the work of writing lines, and {} draws a picture \
             instead; drop one of them",
            sheet.flag()
        ));
    }

    // A row window bounds the run as well as the picture: the colors of every
    // record before the window still have to be computed --- a color is the
    // whole history of its coins --- but nothing past its end is wanted.
    let (skip, limit) = match window {
        None => (0, limit),
        Some((a, b)) if a < b => (a, limit.min(b)),
        Some((a, b)) => {
            return Err(format!(
                "--rows {}..{} is a window with nothing in it: the start has to come first",
                a, b
            ))
        }
    };

    // Said before the survey rather than after it, so that a build missing what
    // one of these draws with refuses in front of a whole pass over the records
    // rather than behind one.
    #[cfg(not(feature = "pdf"))]
    if sheet == Sheet::Folded {
        return Err("--pdf draws with Cairo, and this was built without it: \
                    rebuild with `--features pdf`"
            .into());
    }
    #[cfg(not(feature = "gui"))]
    if sheet == Sheet::Shown {
        return Err("--view opens a GTK window, and this was built without one: \
                    rebuild with `--features gui`"
            .into());
    }

    // `--sum` says what goes after the tab on a line, and a picture has no
    // lines.  Drawing one and quietly ignoring the other is the reading nobody
    // wants.
    if form == Line::Sum {
        return Err(format!(
            "--sum says what a line says, and a picture has no lines: \
             drop one of --sum and {}",
            sheet.flag()
        ));
    }

    let bin = match bin {
        Some(0) => return Err("--bin 0 asks a row to stand for no transactions".into()),
        Some(n) => n,
        None => 1,
    };
    if blocks == Some(0) {
        return Err("--blocks 0 leaves the picture no columns to draw in".into());
    }
    // Gain shades the folded canvas, so a picture that is not folded has
    // nothing for it to do; refused rather than quietly dropped.
    let gain = match gain {
        None => 1.0,
        Some(g) if sheet == Sheet::Raster => {
            let _ = g;
            return Err("--gain shades the folded canvas; it goes with --pdf, --fold or --view, \
                        not --png"
                .into());
        }
        Some(g) if g > 0.0 && g.is_finite() => g,
        Some(g) => return Err(format!("--gain {} is not a usable factor", g)),
    };

    // Both dimensions, before a single pixel: the header names the size in
    // front of the picture and is not gone back to afterwards, so the records
    // are counted first even when `--blocks` has already settled the width.
    // See `image`.
    let (seen, records) = match matrix {
        // A matrix states both numbers about itself in its header, so a run
        // that reaches the end of one settles the picture's size with two `u64`
        // reads where the records cost a whole pass.  That is the article's
        // fourth item arriving somewhere the fold is not: nothing is rewound,
        // nothing is read twice, and a matrix arriving down a pipe draws the
        // same picture as one in a file -- checked, over a 50,000-record
        // matrix on a fifo, against the same run over the file.
        //
        // A run stopping short of the end still has to know which blocks *its*
        // rows reach, and `survey_matrix` reads exactly those -- which means
        // opening the path a second time, so that case does want a file.
        // Anything else is refused here, in front of the second open rather
        // than inside it: a fifo whose writer has closed does not fail there,
        // it blocks in `File::open` and the run hangs with nothing on the
        // screen.  See `opens_twice`.
        Some(path) => {
            let header = reader
                .as_ref()
                .expect("the matrix was opened above")
                .header();
            if limit as u64 >= header.records {
                (header.blocks as usize, header.records as usize)
            } else {
                if let Err(kind) = opens_twice(path) {
                    return Err(format!(
                        "{} is {}, and this run stops at record {} -- short of the {} the \
                         matrix holds, so the blocks its own rows reach have to be read off \
                         it a second time, and a second read from the start is what a stream \
                         cannot give: opening a spent pipe again does not fail, it waits.  \
                         Give --matrix a file, or let the run reach the end of this one",
                        path, kind, limit, header.records
                    ));
                }
                survey_matrix(path, limit).map_err(|e| format!("{}: {}", path, e))?
            }
        }
        // Standard input, which has to be read twice and so has to be a file.
        // Unchanged: this is the pass `--matrix` exists to retire, and it is
        // still what a run over the records makes.
        None => {
            let file = source.as_mut().ok_or_else(|| {
                "standard input cannot be rewound, so the records cannot be counted before the \
                 picture is drawn: both of a picture's dimensions are settled before the first \
                 record — a PNG states how many rows it has in front of the first one, and a \
                 page has to have its canvas before it can count anything into it — and the \
                 rows are how many records there are.  Redirect the records from a file \
                 (`< records`) rather than through a pipe, or write them once as a matrix and \
                 pass --matrix <file>, whose header says both numbers outright"
                    .to_string()
            })?;
            let start = file.stream_position().map_err(|e| e.to_string())?;
            let surveyed = survey(&*file, limit).map_err(|e| e.to_string())?;
            file.seek(SeekFrom::Start(start))
                .map_err(|e| e.to_string())?;
            surveyed
        }
    };

    // Every record carries a block, so no blocks means no records.  There is no
    // picture of nothing — a PNG has to have a column and a row — so this is
    // refused rather than written.
    if records == 0 {
        return Err(format!("no records, so there is no picture to draw in {}", path));
    }
    // The rows are the records inside the window; records the window starts
    // after do not exist as far as the picture is concerned, and a window the
    // records never reach is refused the way no records at all is.
    if records <= skip {
        return Err(format!(
            "the records end at {} — before the row window starting at {}, so there is \
             no picture to draw in {}",
            records, skip, path
        ));
    }
    let width = blocks.unwrap_or(seen);
    let rows = (records - skip).div_ceil(bin);
    // Worth a line on stderr: it is a whole pass over the input, so a long run
    // is otherwise silent for a while before anything happens.
    eprintln!(
        "circular-polynomial: {} records over {} blocks, so a {} x {} picture",
        records - skip, seen, width, rows
    );

    let output = match sheet {
        Sheet::Raster => {
            let writer = image::Writer::new(&path, width, rows, bin, ink)
                .map_err(|e| format!("{}: {}", path, e))?;
            Output::Picture(writer)
        }
        Sheet::FoldedPng => Output::Fold(fold(&path, width, rows, bin, gain)?),
        #[cfg(feature = "pdf")]
        Sheet::Folded => Output::Page(fold(&path, width, rows, bin, gain)?),
        #[cfg(feature = "gui")]
        Sheet::Shown => Output::Window {
            canvas: fold(&path, width, rows, bin, gain)?,
            stem: path,
        },
        // The refusals above are what run in a build without the feature; these
        // arms exist so that the match is still exhaustive there.
        #[cfg(not(feature = "pdf"))]
        Sheet::Folded => unreachable!("refused before the records were counted"),
        #[cfg(not(feature = "gui"))]
        Sheet::Shown => unreachable!("refused before the records were counted"),
    };
    Ok((output, feed(reader, source, threads), skip, limit))
}

/// The canvas both folding sheets accumulate into, and a line on stderr saying
/// how much of the picture each of its cells is standing for.
///
/// One function because a page, a folded PNG and a window differ in nothing
/// until the records have all been read: the same fold, and then written one
/// way or the other, or shown.
fn fold(
    path: &str,
    width: usize,
    rows: usize,
    bin: usize,
    gain: f64,
) -> Result<page::Writer, String> {
    let mut writer = page::Writer::new(path, width, rows, bin, page::DEFAULT_PAGE)
        .map_err(|e| format!("{}: {}", path, e))?;
    writer.set_gain(gain);
    // The second pair is the one to know: it is how much of the picture each
    // cell is standing for, and so what the drawing can no longer tell apart.
    let (across, down) = writer.canvas();
    eprintln!(
        "circular-polynomial: folded onto {} x {} cells, each covering {} x {} of it",
        across,
        down,
        width.div_ceil(across),
        rows.div_ceil(down)
    );
    Ok(writer)
}

/// The records, from whichever of the two `plan` settled on.
///
/// `--threads` spends one more thread than it is given: the pool formats and
/// this one parses, so neither is on the fold's critical path.  There is no
/// third arm for a matrix, and the doc on [`Source::Matrix`] says what that
/// costs and why the alternative was not taken.
fn feed(reader: Option<matrix::Reader<File>>, source: Option<File>, threads: usize) -> Source {
    match reader {
        Some(reader) => Source::Matrix {
            reader,
            inputs: Vec::new(),
            record: None,
        },
        None => {
            let input = records_from(source);
            Source::Records(if threads > 0 {
                prefetch::Records::ahead(input)
            } else {
                prefetch::Records::here(input)
            })
        }
    }
}

/// The records, from the rewindable handle if there is one and from standard
/// input as it comes otherwise.
///
/// Boxed rather than made another type parameter of [`run`]: this is read a
/// megabyte at a time, so a virtual call per refill is nothing, where another
/// parameter would be another copy of the driver per backend.
fn records_from(source: Option<File>) -> Box<dyn io::Read + Send> {
    match source {
        Some(file) => Box::new(file),
        // The handle rather than a lock of it, so that it can be given to the
        // reading thread `--threads` starts -- a `StdinLock` does not cross
        // one.  `sexp::Reader` fills a megabyte before it asks again, so this
        // is one lock a megabyte either way.
        None => Box::new(io::stdin()),
    }
}

/// The value of a `--name <value>` or `--name=<value>` option at `args[i]`, with
/// how many arguments it took.
///
/// Both spellings, because one of these options is a path and the other a count
/// and neither reads well glued to its name.  A prefix match alone would accept
/// `--blocksy`, so the character after the name has to be an `=` or nothing.
///
/// The separate word is not taken if it looks like another option, so `--png
/// --stats` is a missing filename rather than a file called `--stats`.  Nothing
/// here wants a value of that shape, and swallowing the next flag would leave a
/// run silently doing something else.
fn option<'a>(args: &'a [String], i: usize, name: &str) -> Option<(&'a str, usize)> {
    let rest = args[i].strip_prefix(name)?;
    if rest.is_empty() {
        return args
            .get(i + 1)
            .filter(|v| !v.starts_with("--"))
            .map(|v| (v.as_str(), 2));
    }
    rest.strip_prefix('=').map(|v| (v, 1))
}

/// Records which picture was asked for, refusing a second one that disagrees.
///
/// Three options for one output, so a later one quietly winning would be a run
/// drawing something nobody asked for.  Saying the same one twice is not a
/// disagreement and the last `goes` — the file, or what an exported page is
/// named after — is the one that stands.
fn choose(picture: &mut Option<(Sheet, String)>, sheet: Sheet, goes: &str) -> Result<(), String> {
    if let Some((chosen, _)) = picture {
        if *chosen != sheet {
            return Err(format!(
                "{} and {} are two ways of drawing the one picture; drop one of them",
                chosen.flag(),
                sheet.flag()
            ));
        }
    }
    *picture = Some((sheet, goes.to_string()));
    Ok(())
}

fn main() -> ExitCode {
    let mut limit = DEFAULT_LIMIT;
    let mut stats = false;
    // The circular list is the exercise, so it is what runs unless asked
    // otherwise: a plain run of this program is still the Knuth port.
    let mut backend = Backend::Rings;
    let mut chose_backend = false;
    let mut form = Line::Terms;
    let mut picture: Option<(Sheet, String)> = None;
    let mut blocks: Option<usize> = None;
    let mut bin: Option<usize> = None;
    let mut window: Option<(usize, usize)> = None;
    let mut gain: Option<f64> = None;
    // Whether a weighted picture is drawn through the colour ramp rather than
    // as a grey.
    let mut palette = false;
    // A file saying, per transaction, which record spends it last -- so a
    // colour can be freed when nothing will read it again.  See `oracle`.
    let mut release_oracle: Option<String> = None;
    // The records themselves, already taken apart, in place of the text on
    // standard input.  See `matrix`.
    let mut matrix: Option<String> = None;
    // 0 is the serial path, which is what runs unless a count is asked for.
    let mut threads: usize = 0;
    // How many bands a `--bands` colour has, once one is asked for.
    let mut bands: Option<usize> = None;

    // What a page exported from `--view`'s window is named after: the program,
    // the way the viewers name theirs, so that two windows open in one directory
    // do not write over each other's pages.
    let program = std::env::args().next().unwrap_or_default();
    let stem = std::path::Path::new(&program)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "picture".to_string());

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--stats" => stats = true,
            // The same weighted picture, read through `oklch`'s ramp.  A flag
            // rather than a backend, because it changes nothing the run
            // computes -- only what the file says a sample means.
            "--palette" => palette = true,
            "--rings" => {
                backend = Backend::Rings;
                chose_backend = true;
            }
            "--sets" => {
                backend = Backend::Sets;
                chose_backend = true;
            }
            // Weighting needs coefficients to weight, and the ring backend has
            // none it can reach — see `RingStore::WEIGHTED`.  So this selects a
            // backend rather than modifying one, and saying `--rings` too is a
            // contradiction rather than a refinement.
            "--weighted" => {
                if chose_backend && backend == Backend::Rings {
                    eprintln!(
                        "circular-polynomial: --weighted cannot run on --rings; \
                         weights need the set representation"
                    );
                    return ExitCode::FAILURE;
                }
                backend = Backend::Weighted;
                chose_backend = true;
            }
            // Which of the two line forms, rather than which backend -- but a
            // sum of weights needs weights, so it settles that too, below.
            // Two ways of collapsing a color to numbers, so a run does one or
            // the other; a later one quietly winning would be a run printing
            // something nobody asked for, which is what `choose` refuses for
            // the pictures.
            "--sum" | "--moments" => {
                let asked = if args[i] == "--sum" {
                    Line::Sum
                } else {
                    Line::Moments
                };
                if form != Line::Terms && form != asked {
                    eprintln!(
                        "circular-polynomial: --sum and --moments are two ways of \
                         collapsing a color; drop one of them"
                    );
                    return ExitCode::FAILURE;
                }
                form = asked;
            }
            // The one picture option with nothing after it: a window is not
            // somewhere, so there is no path to take.
            "--view" => {
                if let Err(clash) = choose(&mut picture, Sheet::Shown, &stem) {
                    eprintln!("circular-polynomial: {}", clash);
                    return ExitCode::FAILURE;
                }
            }
            "all" => limit = usize::MAX,
            _ => {
                let sheets = [
                    ("--png", Sheet::Raster),
                    ("--pdf", Sheet::Folded),
                    ("--fold", Sheet::FoldedPng),
                ];
                let mut taken = 0;
                for (name, sheet) in sheets {
                    if let Some((path, used)) = option(&args, i, name) {
                        if let Err(clash) = choose(&mut picture, sheet, path) {
                            eprintln!("circular-polynomial: {}", clash);
                            return ExitCode::FAILURE;
                        }
                        taken = used;
                        break;
                    }
                }
                if taken > 0 {
                    i += taken;
                    continue;
                }
                let counts = [("--blocks", &mut blocks), ("--bin", &mut bin)];
                for (name, slot) in counts {
                    if let Some((n, used)) = option(&args, i, name) {
                        match n.parse::<usize>() {
                            Ok(n) => *slot = Some(n),
                            Err(_) => {
                                eprintln!(
                                    "circular-polynomial: {} wants a count, got {:?}",
                                    name, n
                                );
                                return ExitCode::FAILURE;
                            }
                        }
                        taken = used;
                        break;
                    }
                }
                if taken > 0 {
                    i += taken;
                    continue;
                }
                // `--bands <K>` and `--bands64 <K>`: the binned weighted
                // store, in `f32` or `f64` lanes.  A backend of its own, as
                // `--weighted` is, since its output says something different
                // -- a term is a band and not a block.
                let mut chose_bands = false;
                for (name, which) in [("--bands64", Backend::Bands64), ("--bands", Backend::Bands)] {
                    if let Some((k, used)) = option(&args, i, name) {
                        match k.parse::<usize>() {
                            Ok(k) if k > 0 => bands = Some(k),
                            _ => {
                                eprintln!(
                                    "circular-polynomial: {} wants a positive count of bands, got {:?}",
                                    name, k
                                );
                                return ExitCode::FAILURE;
                            }
                        }
                        if chose_backend && !backend.weighted() {
                            eprintln!(
                                "circular-polynomial: {} bins the weighted colour, and the \
                                 unweighted backends have no weights to bin; drop --rings or --sets",
                                name
                            );
                            return ExitCode::FAILURE;
                        }
                        backend = which;
                        chose_backend = true;
                        chose_bands = true;
                        taken = used;
                        break;
                    }
                }
                if chose_bands {
                    i += taken;
                    continue;
                }
                if let Some((path, used)) = option(&args, i, "--release-oracle") {
                    release_oracle = Some(path.to_string());
                    i += used;
                    continue;
                }
                if let Some((path, used)) = option(&args, i, "--matrix") {
                    matrix = Some(path.to_string());
                    i += used;
                    continue;
                }
                if let Some((n, used)) = option(&args, i, "--threads") {
                    // `auto` is what the machine says, less one for the fold
                    // thread that feeds the pool -- it is the producer, and
                    // oversubscribing it is how a pipeline goes slower.
                    let parsed = if n == "auto" {
                        std::thread::available_parallelism()
                            .map(|p| usize::from(p).saturating_sub(1).max(1))
                            .ok()
                    } else {
                        n.parse::<usize>().ok()
                    };
                    match parsed {
                        Some(n) => threads = n,
                        None => {
                            eprintln!(
                                "circular-polynomial: --threads wants a count or `auto`, got {:?}",
                                n
                            );
                            return ExitCode::FAILURE;
                        }
                    }
                    i += used;
                    continue;
                }
                if let Some((g, used)) = option(&args, i, "--gain") {
                    match g.parse::<f64>() {
                        Ok(g) => gain = Some(g),
                        Err(_) => {
                            eprintln!("circular-polynomial: --gain wants a factor, got {:?}", g);
                            return ExitCode::FAILURE;
                        }
                    }
                    i += used;
                    continue;
                }
                if let Some((range, used)) = option(&args, i, "--rows") {
                    let parsed = range.split_once("..").and_then(|(a, b)| {
                        Some((a.parse::<usize>().ok()?, b.parse::<usize>().ok()?))
                    });
                    match parsed {
                        Some(w) => window = Some(w),
                        None => {
                            eprintln!(
                                "circular-polynomial: --rows wants a window like 830000..834096, \
                                 got {:?}",
                                range
                            );
                            return ExitCode::FAILURE;
                        }
                    }
                    i += used;
                    continue;
                }
                match args[i].parse::<usize>() {
                    Ok(n) => limit = n,
                    Err(_) => {
                        eprintln!("{}", USAGE);
                        return ExitCode::FAILURE;
                    }
                }
            }
        }
        i += 1;
    }

    // Checked after the whole command line rather than as the flag is read, so
    // that it holds whichever order the flags came in.
    //
    // The two collapsed forms are one case: both add up weights, so both need a
    // backend that has weights, and both put so little on a line that a pool of
    // formatters would cost more than it saves.
    if let Some(flag) = match form {
        Line::Sum => Some("--sum"),
        Line::Moments => Some("--moments"),
        Line::Terms => None,
    } {
        if chose_backend && !backend.weighted() {
            eprintln!(
                "circular-polynomial: {} adds up the weights of a color, and the \
                 unweighted backends have none to add up; drop --rings or --sets",
                flag
            );
            return ExitCode::FAILURE;
        }
        if !chose_backend {
            backend = Backend::Weighted;
        }
        // A pool of formatters is worth having because formatting a color is
        // expensive, and these two are the line forms for which it is not: the
        // whole color collapses to a number or three, and handing a worker a
        // copy of the terms to collapse costs more than collapsing them here
        // does.  Measured over the corpus `emit` names -- 150,000 records of
        // `--example records -- --window 4000` -- `--sum` goes from 3.34s
        // serial to 5.20s at two threads, 4.54s at four and 4.51s at eight, and
        // `--moments` from 3.58s serial to 3.95s, 4.18s and 4.22s.  Batching
        // the dispatch, which fixed the same loss for small colors, does not
        // help: the copy is the cost, not the channel.  So this is refused
        // rather than obeyed.
        //
        // Worth recording that the second row costs nearly what the first does
        // and not quite: 3.69/3.71/3.72s against 3.85/3.86/3.88s over the same
        // records, about 4%, measured eight runs apiece on an idle machine.
        // Four running sums rather than one is three more multiply-adds a term
        // and those really are free -- the loop is bound by walking the color.
        // What is not free is the line: two of the three extra fields are
        // another shortest round-tripping decimal each.  (An earlier reading
        // had the two equal, at 3.58s against 3.57s.)
        if threads > 0 {
            eprintln!(
                "circular-polynomial: {} prints a few numbers a line, so there is \
                 nothing for --threads to spread; drop one of them",
                flag
            );
            return ExitCode::FAILURE;
        }
    }

    // A picture is drawn in whichever ink the backend has to offer, and that is
    // the depth of every sample in it: settled here, where the backend is, and
    // handed to the writer rather than discovered a term at a time.
    let ink = match backend {
        b if b.weighted() && palette => image::Ink::Palette,
        b if b.weighted() => image::Ink::Weighted,
        _ => image::Ink::Flat,
    };

    // A flat pixel is in the colour or it is not, and a ramp between two states
    // is a ramp with nothing on it: the palette wants a quantity to spend its
    // levels on, which is what the weighted backend has and the other two do
    // not.  Refused rather than ignored, as `--blocks` without a picture is.
    if palette && !backend.weighted() {
        eprintln!(
            "circular-polynomial: --palette draws the weight of a block as a colour, \
             and the unweighted backends have only whether it is there; add --weighted"
        );
        return ExitCode::FAILURE;
    }
    // The ink is `image::Writer`'s, and only `--png` draws through that one.
    // The other three fold the picture onto a canvas with `page`, which shades
    // a cell by how much of it is inked and has no palette to look a sample up
    // in -- so `--palette` there would be a flag that quietly did nothing,
    // which is what every other contradiction in this file refuses.
    match &picture {
        None if palette => {
            eprintln!(
                "circular-polynomial: --palette describes the picture, so it needs \
                 --png <file>"
            );
            return ExitCode::FAILURE;
        }
        Some((sheet, _)) if palette && *sheet != Sheet::Raster => {
            eprintln!(
                "circular-polynomial: --palette colours the pixels of a picture, and {} \
                 folds it onto a canvas of shaded cells instead; use --png",
                sheet.flag()
            );
            return ExitCode::FAILURE;
        }
        _ => {}
    }

    // A band's exponent is a band and not a block, and every picture puts its
    // exponents on a block axis, so the two do not meet yet.  Refused rather
    // than drawn `K` columns wide with `--blocks` meaning something else.
    if bands.is_some() && picture.is_some() {
        eprintln!(
            "circular-polynomial: --bands colours by band and the pictures draw by \
             block; drop one of them"
        );
        return ExitCode::FAILURE;
    }
    // Under `--bands`, `--blocks` says how many block ids the bands cover
    // rather than how wide a picture is, and the 2022 chain's count is the
    // default.  Taken here so that `plan` does not see it as a picture flag.
    let band_blocks = match bands {
        Some(_) => blocks.take().unwrap_or(bands::CHAIN_BLOCKS),
        None => 0,
    };

    let (output, source, skip, limit) =
        match plan(
            picture,
            blocks,
            bin,
            window,
            gain,
            ink,
            form,
            threads,
            // The `f32` bands are the one store whose coefficients print
            // narrower than the `f64` they travel as.
            backend == Backend::Bands,
            limit,
            matrix.as_deref(),
            // Which of the two refusals a `--structure` matrix gets: a store
            // that shares a colour out in proportion to the amounts needs them
            // to be there.
            backend.weighted(),
        ) {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("circular-polynomial: {}", message);
            return ExitCode::FAILURE;
        }
    };

    // The oracle is checked against the run before a record is read, since a
    // stale one frees colours that are still wanted and the failure that
    // follows is a wrong answer rather than a crash.
    let oracle = match &release_oracle {
        None => None,
        Some(path) => {
            let expected = if limit == usize::MAX { None } else { Some(limit as u64) };
            match oracle::Oracle::load(path, expected) {
                Ok(o) => {
                    if stats {
                        eprintln!(
                            "circular-polynomial: release oracle over {} records",
                            o.records()
                        );
                    }
                    Some(o)
                }
                Err(e) => {
                    eprintln!("circular-polynomial: {}: {}", path, e);
                    return ExitCode::FAILURE;
                }
            }
        }
    };

    // One instantiation of the loop per backend, so none of them pays for the
    // others existing.
    let outcome = match backend {
        Backend::Rings => run::<RingStore>(limit, skip, stats, output, source, oracle),
        Backend::Sets => run::<colorset::SetStore>(limit, skip, stats, output, source, oracle),
        Backend::Weighted => {
            run::<weighted::WeightedSets>(limit, skip, stats, output, source, oracle)
        }
        Backend::Bands | Backend::Bands64 => {
            let k = bands.expect("the backend was chosen by --bands");
            bands::configure(bands::Layout::new(k, band_blocks));
            if backend == Backend::Bands {
                run::<bands::BandStore<f32>>(limit, skip, stats, output, source, oracle)
            } else {
                run::<bands::BandStore<f64>>(limit, skip, stats, output, source, oracle)
            }
        }
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        // Downstream went away (`| head`); that is not our failure.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("circular-polynomial: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run<S: ColorStore>(
    limit: usize,
    skip: usize,
    stats: bool,
    mut out: Output,
    mut source: Source,
    oracle: Option<oracle::Oracle>,
) -> io::Result<()> {
    let mut store = S::new();
    // Which of the record's inputs may take the colour it spends, when an
    // oracle says.  A field rather than a local so it is not reallocated per
    // record.
    let mut last_reads: Vec<bool> = Vec::new();
    let mut colors: HashMap<usize, (S::Color, usize)> = HashMap::new();

    // Only read when `--stats` is on, but started unconditionally: the clock has
    // to be running before the first record, and one `Instant::now()` per
    // process is not worth branching over.
    let started = Instant::now();
    let mut since_report = started;

    let mut records: usize = 0;
    while records < limit {
        // The record and its inputs, borrowed from whichever buffer they were
        // parsed into -- this thread's, or the reading thread's batch.  The
        // borrow ends at the next call, which is after the fold has turned the
        // inputs into a colour.
        let (record, inputs) = match source.next()? {
            Some(pair) => pair,
            None => break,
        };

        // An oracle covers a fixed number of records, and a record past its end
        // would look up `NEVER` and free colours the rest of the run still
        // wants.  Checked here rather than against the record limit, because
        // `all` has no count to compare against before the run starts.
        if let Some(o) = &oracle {
            if records as u64 >= o.records() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "the release oracle covers {} records and this run reached {}; \
                         it was written for different input",
                        o.records(),
                        records + 1
                    ),
                ));
            }
        }

        let color = if inputs.is_empty() {
            // Coinbase: the block that minted it is the whole color.
            store.singleton(record.block_id)
        } else {
            // `foldr` over the inputs, so right to left.  The order does not
            // change the result — union is commutative and the merge is sorted —
            // but it decides which input hits an entry's last unspent output,
            // and the Scheme's order is the one to match.
            //
            // `None` is the seed.  The Scheme folds from `0/polynomial` and so
            // did this until the backends were split, but an empty operand is
            // something every union has to carry through the merge, and the
            // whole first step of the fold is then a copy of one input for no
            // reason.  Not having a seed says the same thing and does no work.
            let mut accumulator: Option<S::Color> = None;

            // With an oracle the driver knows which inputs are the final read
            // of what they spend, so a colour is freed when nothing will read
            // it again rather than when its last output happens to be spent.
            // Without one, the unspent count answers the question as well as a
            // stream can.
            if let Some(o) = &oracle {
                o.last_reads(records as u64, inputs, &mut last_reads);
            }

            // Each input contributes its ancestor's color in proportion to the
            // amount it spends, so the shares are `amount / total`.  Computed
            // once per record and only when the backend will use them; for the
            // unweighted stores `S::WEIGHTED` is a constant `false`, so this and
            // every weight below fold away at compile time.
            //
            // A total of zero has no proportions to speak of — it happens, since
            // nothing forbids a zero-value input — so the inputs share equally
            // instead of dividing by it.
            let total: f64 = if S::WEIGHTED {
                inputs.iter().map(|i| i.amount as f64).sum()
            } else {
                0.0
            };
            let share = |input: &sexp::Input| -> f64 {
                if !S::WEIGHTED {
                    1.0
                } else if total > 0.0 {
                    input.amount as f64 / total
                } else {
                    1.0 / inputs.len() as f64
                }
            };

            for i in (0..inputs.len()).rev() {
                let previous = inputs[i].prev_tx_id;
                let weight = share(&inputs[i]);
                let entry = match colors.get_mut(&previous) {
                    Some(e) => e,
                    None => {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!(
                                "transaction {} spends unknown transaction {}",
                                record.tx_id, previous
                            ),
                        ))
                    }
                };
                // The oracle's answer is exact and the count is a bound: a
                // colour with unspent outputs that nothing ever spends is dead
                // either way, and only the oracle knows it.
                let taken = match &oracle {
                    Some(_) => last_reads[i],
                    None => entry.1 <= 1,
                };
                let unspent = entry.1;

                if !taken {
                    // Others can still reach this color, so it has to survive
                    // the fold: merge from a borrow, and on the first step take
                    // a second handle rather than the color itself.
                    entry.1 = unspent.saturating_sub(1);
                    let held = &entry.0;
                    accumulator = Some(match accumulator.take() {
                        // First step: the accumulator *is* this input's share of
                        // it.  A single-input transaction has weight 1 and
                        // `scale` shares rather than rebuilding, so the
                        // commonest shape still costs nothing.
                        None => store.scale(held, weight),
                        Some(acc) => {
                            // The accumulator already carries its own share, so
                            // it comes in at full strength.
                            let combined = store.combine(held, weight, &acc, 1.0);
                            store.release(acc);
                            combined
                        }
                    });
                } else {
                    // The last unspent output is being spent right now, so
                    // nobody can reach this color again and the driver may take
                    // it outright.  This is the case that used to cost a full
                    // copy: `union(previous, empty)` followed by freeing the
                    // original on the very next line.  It is the driver's UTXO
                    // bookkeeping that makes the move safe, not anything a store
                    // could work out for itself, which is why it lives here.
                    let (root, _) = colors.remove(&previous).expect("just looked it up");
                    accumulator = Some(match accumulator.take() {
                        None => {
                            // Taking the ring outright is only the whole answer
                            // when this input is the whole transaction; at any
                            // other weight it still has to be scaled, and then
                            // the taken color is released like any other.
                            if weight == 1.0 {
                                root
                            } else {
                                let scaled = store.scale(&root, weight);
                                store.release(root);
                                scaled
                            }
                        }
                        Some(acc) => {
                            let combined = store.combine(&root, weight, &acc, 1.0);
                            store.release(acc);
                            store.release(root);
                            combined
                        }
                    });
                }
            }
            accumulator.expect("inputs is non-empty, so the fold ran at least once")
        };

        if stats {
            store.observe(&color);
        }

        // A record before a `--rows` window is colored --- its color is history
        // every later record may inherit --- and not shown: the window is about
        // the picture, not about the fold that feeds it.
        if records >= skip {
            out.emit::<S>(&store, &color, record.tx_id)?;
        }

        // Worth keeping only if something will read it.  The unspent count is
        // the stream's answer; the oracle's is exact, and it is the difference
        // between holding 92.7 million colours over the 2022 chain and holding
        // 19.4 million.
        let keep = match &oracle {
            Some(o) => o.ever_read(record.tx_id),
            None => record.outputs > 0,
        };
        if keep {
            if let Some((displaced, _)) = colors.insert(record.tx_id, (color, record.outputs)) {
                // The Scheme leaks the displaced color; we can afford not to.
                store.release(displaced);
            }
        } else {
            store.release(color);
        }

        records += 1;
        if stats && records.is_multiple_of(STATS_EVERY) {
            let now = Instant::now();
            let (live, committed) = store.usage();
            let (live_label, committed_label) = store.usage_labels();
            eprintln!(
                "{:>10.2}s {:>10} records {:>11}  {:>12} {}  {:>12} {}  {:>10} colored txs",
                started.elapsed().as_secs_f64(),
                records,
                // Rate over this interval rather than the whole run so far.  A
                // cumulative average is smoother but hides the thing worth
                // watching: colors grow as the chain does, so the merge gets
                // slower as the run goes on, and only the interval rate shows it.
                rate(STATS_EVERY, now.duration_since(since_report).as_secs_f64()),
                live,
                live_label,
                committed,
                committed_label,
                colors.len()
            );
            since_report = now;
        }
    }

    // A matrix that ends before the record limit prints the rows it had and
    // stops, which is what the records do at the end of the file and why the
    // limit is no longer held against the header (see `plan`).  It is still
    // worth saying: a matrix is a derived file, and one that is short is
    // usually one written for a shorter run than the one being made now.  Said
    // here because here is where the run actually ends -- the count is known
    // nowhere else -- and said on stderr because stdout is the thing a matrix
    // run is held against a text run byte for byte, and a line of this on it
    // would be the difference.  `all` asks for everything and so is never
    // short of anything.
    if records < limit && limit != usize::MAX {
        if let Source::Matrix { reader, .. } = &source {
            eprintln!(
                "circular-polynomial: the matrix holds {} records and this run asked for {}, \
                 so it coloured the {} it had",
                reader.records(),
                limit,
                records
            );
        }
    }

    if stats {
        let elapsed = started.elapsed().as_secs_f64();
        eprintln!(
            "{:>10.2}s {:>10} records {:>11}  total",
            elapsed,
            records,
            rate(records, elapsed)
        );
        eprintln!("{}", store.audit(&mut colors.values().map(|(c, _)| c)));
        // Worth saying out loud in a picture run, where stdout stays empty and
        // the file's own header is the only other place the size is written
        // down.
        match &out {
            Output::Picture(picture) => {
                let (columns, rows) = picture.dimensions();
                eprintln!("picture: {} columns x {} rows", columns, rows);
            }
            #[cfg(feature = "pdf")]
            Output::Page(sheet) => folded(sheet),
            Output::Fold(sheet) => folded(sheet),
            #[cfg(feature = "gui")]
            Output::Window { canvas, .. } => folded(canvas),
            Output::Text { .. } | Output::Threaded(_) => {}
        }
    }

    out.finish()
}

/// What `--stats` says about a folded picture: the size it would have had, and
/// the size it was drawn at.
fn folded(canvas: &page::Writer) {
    let (columns, rows) = canvas.dimensions();
    let (across, down) = canvas.canvas();
    eprintln!(
        "picture: {} columns x {} rows, on {} x {} cells",
        columns, rows, across, down
    );
}

/// `records` over `seconds`, short enough to hold a fixed column.
///
/// Answers `--` rather than an infinity when no time has passed, which happens
/// for real: a short run under `--stats` can reach its first checkpoint inside
/// the clock's resolution.
fn rate(records: usize, seconds: f64) -> String {
    if seconds <= 0.0 {
        return "-- rec/s".to_string();
    }
    let per_second = records as f64 / seconds;
    if per_second >= 1e6 {
        format!("{:.1}M rec/s", per_second / 1e6)
    } else if per_second >= 1e3 {
        format!("{:.0}k rec/s", per_second / 1e3)
    } else {
        format!("{:.0} rec/s", per_second)
    }
}

/// An `f64` at full precision: the shortest decimal that reads back as exactly
/// this number, which is what Rust's `Display` gives and what every float this
/// program prints — a term's coefficient, and `--sum`'s one number — goes
/// through.
///
/// Shortest-round-trip rather than a fixed width because a coefficient is a
/// weight, weights decay by roughly a factor per hop of ancestry, and a fixed
/// six places turns everything below a millionth into `0.000000` — present in
/// the sum, unreadable on the line.  Nothing here is rounded away: parse a term
/// back and the same bits come out.  [`Line::Sum`]'s number goes through here
/// too, so that is true of a whole line either way it is printed.
///
/// This is the formatting machinery [`push_int`] exists to avoid.  Only
/// `--weighted` pays it per term; an unweighted coefficient is the integer 1 and
/// takes the cheap path, and `--sum` pays it once a line.  `Display` never
/// switches to an exponent, so a denormal weight would print its leading zeros
/// in full -- far below what a chain of any length produces, and the alternative
/// is an `e` in a field a reader is splitting on punctuation.
fn push_f64(out: &mut Vec<u8>, value: f64) {
    // Writing to a `Vec` cannot fail, and there is no error here to report.
    let _ = write!(out, "{}", value);
}

/// The same for an `f32`: the shortest decimal that reads back as this `f32`,
/// which is what a `--bands` weight is.  Printing it through [`push_f64`] would
/// be correct and twice as long -- `0.35835019` against `0.3583501875400543`,
/// the latter being what it takes to read back as the same `f64`.
fn push_f32(out: &mut Vec<u8>, value: f32) {
    let _ = write!(out, "{}", value);
}

/// Decimal, straight into the line buffer.  `write!` would do it too, but its
/// formatting machinery is a real cost across a million lines of thousands of
/// terms each.
fn push_int(out: &mut Vec<u8>, value: Coeff) {
    let mut digits = [0u8; 20];
    let mut i = digits.len();
    let mut magnitude = value;
    loop {
        i -= 1;
        digits[i] = b'0' + (magnitude % 10) as u8;
        magnitude /= 10;
        if magnitude == 0 {
            break;
        }
    }
    out.extend_from_slice(&digits[i..]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A sink a test can read back.  [`Output`] owns its writer, so the buffer
    /// is shared with it rather than handed over.
    #[derive(Clone, Default)]
    struct Shared(Rc<RefCell<Vec<u8>>>);

    impl Write for Shared {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A record in the shape [`sexp`] reads: the header's block and tx id, then
    /// `(prev-tx, amount)` an input, then how many outputs.
    fn record(block: usize, tx: usize, spends: &[(usize, usize)], outputs: usize) -> String {
        let inputs: String = spends
            .iter()
            .map(|(prev, amount)| format!("(7 {} {} 0)", amount, prev))
            .collect();
        let outs: String = (0..outputs).map(|_| "(7 1 0)".to_string()).collect();
        format!("((1 {} {} 0 0 0 0) ({}) ({}))\n", block, tx, inputs, outs)
    }

    /// A sink a threaded run can write to.  `Send`, unlike [`Shared`], because
    /// `Output::Threaded` writes from a thread of the pool's own.
    #[derive(Clone, Default)]
    struct SharedSend(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl Write for SharedSend {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// The whole driver, run through the formatter pool rather than the serial
    /// writer.
    ///
    /// This is the seam nothing else covers: `emit`'s own tests drive `Pool`
    /// directly and every other test here goes through `Output::text`, so the
    /// arm of `Output::emit` that fills a `Snapshot` -- and its choice of
    /// `push_flat` against `push_weighted` -- was only ever exercised by
    /// running the binary.
    fn threaded_lines<S: ColorStore>(
        records: &str,
        form: Line,
        limit: usize,
        threads: usize,
    ) -> Vec<String> {
        let sink = SharedSend::default();
        let out = Output::Threaded(emit::Pool::with_lanes(
            Box::new(sink.clone()),
            form,
            threads,
            S::NARROW,
        ));
        run::<S>(limit, 0, false, out, source(records), None).expect("the records are well formed");
        let written = sink.0.lock().unwrap().clone();
        String::from_utf8(written)
            .expect("the output is digits and punctuation")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The records as something `run` can read, parsed on this thread.
    fn source(records: &str) -> Source {
        Source::Records(prefetch::Records::here(Box::new(io::Cursor::new(
            records.to_string().into_bytes(),
        ))))
    }

    /// Color `records` with the backend `form` implies and answer the lines.
    ///
    /// Run twice, once against each reader, and the two are required to agree:
    /// reading ahead is a thread and a batch and nothing about what a record
    /// says, so every assertion in this file is an assertion about both.
    fn lines<S: ColorStore>(records: &str, form: Line, limit: usize) -> Vec<String> {
        let read = |source| {
            let sink = Shared::default();
            let out = Output::text(Box::new(sink.clone()), form);
            run::<S>(limit, 0, false, out, source, None).expect("the records are well formed");
            let written = sink.0.borrow().clone();
            String::from_utf8(written)
                .expect("the output is digits and punctuation")
                .lines()
                .map(str::to_string)
                .collect::<Vec<String>>()
        };
        let here = read(source(records));
        let ahead = read(Source::Records(prefetch::Records::ahead(Box::new(
            io::Cursor::new(records.to_string().into_bytes()),
        ))));
        assert_eq!(ahead, here, "the two readers coloured the records differently");
        here
    }

    /// A corpus whose colours actually mix blocks.
    ///
    /// The obvious loop -- one coinbase and then every transaction spending the
    /// last few -- looks varied and is not: all ancestry funnels back to the
    /// single coinbase, so every colour is that one block, every spread is
    /// exactly zero and every effective count is exactly one.  A test written
    /// over it passes whatever the spread is computed as, which is how the
    /// first version of `the_three_numbers_are_what_the_definitions_say` came
    /// to survive a mutation that returned a spread of zero for everything.
    ///
    /// So: a fresh coinbase every `mint` transactions, each in its own block,
    /// and the rest spending a spread of recent ancestors.  That is what makes
    /// a colour rest on several blocks at unequal weights.
    fn mixed(n: usize, mint: usize) -> String {
        let mut out = String::new();
        for tx in 0..n {
            let spends: Vec<(usize, usize)> = if tx % mint == 0 {
                Vec::new()
            } else {
                [1usize, 2, 5]
                    .iter()
                    .filter(|k| tx >= **k)
                    .map(|k| (tx - k, 1 + (tx * 7 + k) % 11))
                    .collect()
            };
            out.push_str(&record(tx * 3, tx, &spends, 4));
        }
        out
    }

    /// [`mixed`], with the block ids a real chain has: a block is opened by the
    /// coinbase that mints it and every record after that one is in it until
    /// the next coinbase.
    ///
    /// [`mixed`] puts every transaction in a block of its own, which no chain
    /// does and which a [`matrix`] cannot even write down: a FOLDMAT row
    /// carries a block only when it has no inputs, on the argument that every
    /// other record is in the block the last coinbase opened.  So the matrix
    /// tests need a corpus that argument is true of, and this is it -- same
    /// spends, same amounts, same mixing of ancestry, and the blocks stepping
    /// only where a block really steps.
    fn blocked(n: usize, mint: usize) -> String {
        let mut out = String::new();
        for tx in 0..n {
            let spends: Vec<(usize, usize)> = if tx % mint == 0 {
                Vec::new()
            } else {
                [1usize, 2, 5]
                    .iter()
                    .filter(|k| tx >= **k)
                    .map(|k| (tx - k, 1 + (tx * 7 + k) % 11))
                    .collect()
            };
            out.push_str(&record((tx - tx % mint) * 3, tx, &spends, 4));
        }
        out
    }

    /// The three moments of a run, as `(mean, spread, effective)` triples.
    fn moments(records: &str, limit: usize) -> Vec<(f64, f64, f64)> {
        lines::<weighted::WeightedSets>(records, Line::Moments, limit)
            .iter()
            .map(|line| {
                let f: Vec<f64> = line
                    .split('\t')
                    .skip(1)
                    .map(|v| v.parse().expect("three numbers after the tx id"))
                    .collect();
                assert_eq!(f.len(), 3, "a moments line is four columns: {}", line);
                (f[0], f[1], f[2])
            })
            .collect()
    }

    /// The sums of a run, which is `--sum`'s backend and no other.
    fn sums(records: &str, limit: usize) -> Vec<String> {
        lines::<weighted::WeightedSets>(records, Line::Sum, limit)
    }

    /// Every line names its transaction and then says what it has to say, with a
    /// tab between the two -- and what follows the tab under [`Line::Terms`] is
    /// `coefficient:exponent` terms with a comma between them and nothing after
    /// the last.
    #[test]
    fn a_line_is_the_transaction_then_a_tab_then_the_color() {
        let records = record(3, 0, &[], 1) + &record(4, 1, &[(0, 50)], 0);
        let terms = lines::<RingStore>(&records, Line::Terms, usize::MAX);
        assert_eq!(terms, ["0\t1:3", "1\t1:3"]);
        for line in &terms {
            let (id, color) = line.split_once('\t').expect("a tab in every line");
            assert!(id.bytes().all(|b| b.is_ascii_digit()), "{:?}", id);
            assert!(!color.ends_with(','), "the comma separates, it does not trail: {:?}", color);
        }
    }

    /// A weighted coefficient is written out for all the `f64` is worth, and the
    /// colon that separates it from the block is the only one in the term --
    /// which is the only thing a reader has to know to take the two apart.
    #[test]
    fn a_weighted_term_is_the_whole_coefficient_then_the_block() {
        let records = record(0, 0, &[], 1) + &record(3, 1, &[], 1)
            + &record(5, 2, &[(0, 1), (1, 2)], 1);
        let line = lines::<weighted::WeightedSets>(&records, Line::Terms, usize::MAX)
            .pop()
            .expect("three records in, three lines out");
        let (_, color) = line.split_once('\t').unwrap();
        let mut terms = color.split(',').map(|t| {
            let (coefficient, block) = t.split_once(':').expect("a colon in every term");
            (block.parse::<usize>().unwrap(), coefficient.parse::<f64>().unwrap())
        });
        // Decreasing block order, which is the order the store walks a color in.
        assert_eq!(terms.next(), Some((3, 2.0 / 3.0)), "two thirds of the value, exactly");
        assert_eq!(terms.next(), Some((0, 1.0 / 3.0)));
        assert_eq!(terms.next(), None);
    }

    /// The three backends are three ways to the same answer, and two of them
    /// promise the same bytes.  A color with no terms is an empty half-line
    /// rather than a missing one.
    /// A threaded run and a serial one are the same run.
    ///
    /// Every backend, both line forms, several widths -- and the width is
    /// varied because dispatch is round-robin over the workers, so a count that
    /// divides the number of batches and one that does not take different paths
    /// through the writer's collection order.
    #[test]
    fn the_pool_colours_exactly_what_the_serial_writer_does() {
        let mut records = String::new();
        for tx in 0..400 {
            let block = tx / 5;
            let spends: Vec<(usize, usize)> = if tx == 0 {
                Vec::new()
            } else {
                // A couple of recent ancestors, so colours grow and the lines
                // get long enough to be worth batching.
                (1..=2usize)
                    .filter(|k| tx >= *k)
                    .map(|k| (tx - k, 1 + tx % 7))
                    .collect()
            };
            records.push_str(&record(block, tx, &spends, 4));
        }

        for threads in [1usize, 2, 3, 8] {
            assert_eq!(
                threaded_lines::<colorset::SetStore>(&records, Line::Terms, usize::MAX, threads),
                lines::<colorset::SetStore>(&records, Line::Terms, usize::MAX),
                "--sets disagreed at {} threads",
                threads
            );
            assert_eq!(
                threaded_lines::<RingStore>(&records, Line::Terms, usize::MAX, threads),
                lines::<RingStore>(&records, Line::Terms, usize::MAX),
                "--rings disagreed at {} threads",
                threads
            );
            assert_eq!(
                threaded_lines::<weighted::WeightedSets>(&records, Line::Terms, usize::MAX, threads),
                lines::<weighted::WeightedSets>(&records, Line::Terms, usize::MAX),
                "--weighted disagreed at {} threads",
                threads
            );
        }
    }

    #[test]
    fn the_unweighted_backends_agree_line_for_line() {
        let records = record(0, 0, &[], 2)
            + &record(5, 1, &[], 1)
            + &record(6, 2, &[(0, 10), (1, 90)], 1)
            + &record(7, 3, &[(0, 10), (2, 90)], 0);
        assert_eq!(
            lines::<RingStore>(&records, Line::Terms, usize::MAX),
            lines::<colorset::SetStore>(&records, Line::Terms, usize::MAX)
        );
        assert_eq!(
            lines::<RingStore>(&records, Line::Terms, usize::MAX).last().unwrap(),
            "3\t1:5,1:0"
        );
    }

    /// Nothing was spent, so the block that minted it holds all of the weight
    /// and the sum is that block, exactly.
    #[test]
    fn a_coinbase_sits_on_its_own_block() {
        assert_eq!(sums(&record(7, 0, &[], 1), usize::MAX), ["0\t7"]);
    }

    /// Half the value from block 0 and half from block 3 puts the sum halfway
    /// between them -- the case [`Line::Sum`] exists to compute.
    #[test]
    fn two_equal_inputs_land_between_their_blocks() {
        let records =
            record(0, 0, &[], 1) + &record(3, 1, &[], 1) + &record(5, 2, &[(0, 50), (1, 50)], 1);
        assert_eq!(sums(&records, usize::MAX).last().unwrap(), "2\t1.5");
    }

    /// Weighting is by amount, not by input count: nine tenths of the value
    /// coming from block 0 pulls the sum nine tenths of the way to it.
    #[test]
    fn the_sum_follows_the_value_not_the_inputs() {
        let records =
            record(0, 0, &[], 1) + &record(10, 1, &[], 1) + &record(11, 2, &[(0, 90), (1, 10)], 1);
        assert_eq!(sums(&records, usize::MAX).last().unwrap(), "2\t1");
    }

    /// A chain of single-input transactions carries its ancestor's sum along
    /// unchanged: one input takes all of the weight, so there is nothing to mix
    /// with -- and nothing for the drift to accumulate out of, either.
    #[test]
    fn a_single_input_inherits_the_sum() {
        let mut records =
            record(0, 0, &[], 1) + &record(4, 1, &[], 1) + &record(5, 2, &[(0, 50), (1, 50)], 1);
        for tx in 3..8 {
            records += &record(5 + tx, tx, &[(tx - 1, 100)], 1);
        }
        let lines = sums(&records, usize::MAX);
        assert!(
            lines[3..].iter().all(|l| l.ends_with("\t2")),
            "{:?}",
            lines
        );
    }

    /// Nothing forbids a record whose inputs are all worth nothing, and it must
    /// not divide by the total: the inputs share equally instead.
    #[test]
    fn inputs_worth_nothing_share_equally() {
        let records =
            record(0, 0, &[], 1) + &record(6, 1, &[], 1) + &record(9, 2, &[(0, 0), (1, 0)], 1);
        assert_eq!(sums(&records, usize::MAX).last().unwrap(), "2\t3");
    }

    /// The claim [`Line::Sum`] rests on: a color's weights sum to 1, so the sum
    /// of `b . weight(b)` is the weighted mean without anything dividing by
    /// them.  Asserted over a fold deep enough to accumulate drift if there were
    /// any -- and against the mean computed the other way, in full `f64`.
    /// A coinbase rests on the one block that minted it: the mean is that
    /// block, there is nothing to spread over, and one block is the whole of
    /// what it rests on.
    #[test]
    fn a_coinbase_has_no_spread_and_rests_on_one_block() {
        let records = record(7, 0, &[], 1);
        assert_eq!(moments(&records, 10), vec![(7.0, 0.0, 1.0)]);
    }

    /// The case `--sum` cannot tell apart, which is the reason this exists.
    ///
    /// One transaction takes everything from block 500; another takes half from
    /// block 0 and half from block 1000.  Both have a mean of 500.  The spread
    /// and the effective count are what say they are nothing alike.
    #[test]
    fn two_colors_with_the_same_mean_are_told_apart() {
        // tx 0 mints block 500; tx 1 spends it whole.
        let concentrated = format!("{}{}", record(500, 0, &[], 1), record(501, 1, &[(0, 10)], 1));
        // tx 0 mints block 0, tx 1 mints block 1000, tx 2 takes half of each.
        let split = format!(
            "{}{}{}",
            record(0, 0, &[], 1),
            record(1000, 1, &[], 1),
            record(1001, 2, &[(0, 10), (1, 10)], 1)
        );

        let (m0, s0, e0) = *moments(&concentrated, 10).last().unwrap();
        let (m1, s1, e1) = *moments(&split, 10).last().unwrap();

        assert!((m0 - 500.0).abs() < 1e-9, "concentrated mean {}", m0);
        assert!((m1 - 500.0).abs() < 1e-9, "split mean {}", m1);
        assert_eq!(
            sums(&concentrated, 10).last().unwrap().split('\t').nth(1),
            sums(&split, 10).last().unwrap().split('\t').nth(1),
            "--sum has to agree on these two, or the test is not about anything"
        );

        assert!(s0 < 1e-6, "all of it from one block, so no spread: {}", s0);
        assert!((s1 - 500.0).abs() < 1e-9, "half at each end of 0..1000: {}", s1);
        assert!((e0 - 1.0).abs() < 1e-9, "one block carries it: {}", e0);
        assert!((e1 - 2.0).abs() < 1e-9, "two blocks carry it: {}", e1);
    }

    /// The effective count is not the support size: a colour that is nearly all
    /// one block rests on nearly one block, however many others trail behind
    /// it at small weight.  That is the whole difference between counting
    /// blocks that matter and counting blocks that appear.
    #[test]
    fn the_effective_count_weighs_the_blocks_rather_than_counting_them() {
        // tx 2 takes 99% of its value from block 0 and 1% from block 1000.
        let records = format!(
            "{}{}{}",
            record(0, 0, &[], 1),
            record(1000, 1, &[], 1),
            record(1001, 2, &[(0, 99), (1, 1)], 1)
        );
        let (_, _, effective) = *moments(&records, 10).last().unwrap();
        // 1 / (0.99^2 + 0.01^2) = 1.0203...
        assert!(
            (effective - 1.0 / (0.99f64.powi(2) + 0.01f64.powi(2))).abs() < 1e-9,
            "two blocks appear but barely more than one carries it: {}",
            effective
        );
        assert!(effective < 1.03, "and it is nowhere near the support size 2");
    }

    /// The mean is `--sum`'s number, since a colour's weights sum to 1.  Stated
    /// as a test because it is what lets the two flags be compared.
    #[test]
    fn the_mean_agrees_with_the_sum() {
        let records = mixed(60, 7);
        let means: Vec<f64> = moments(&records, usize::MAX).iter().map(|m| m.0).collect();
        let summed: Vec<f64> = sums(&records, usize::MAX)
            .iter()
            .map(|l| l.split('\t').nth(1).unwrap().parse().unwrap())
            .collect();
        assert_eq!(means.len(), summed.len());
        for (k, (mean, sum)) in means.iter().zip(&summed).enumerate() {
            assert!(
                (mean - sum).abs() <= 1e-9 * sum.abs().max(1.0),
                "record {}: mean {} against sum {}",
                k,
                mean,
                sum
            );
        }
    }

    /// Every triple, against the definitions written out slowly over the terms
    /// the same run prints under `--weighted`.
    #[test]
    fn the_three_numbers_are_what_the_definitions_say() {
        let records = mixed(80, 7);

        let got = moments(&records, usize::MAX);
        // The corpus has to be worth testing over, or the assertions below hold
        // for reasons that have nothing to do with the arithmetic.  See
        // `mixed`.
        assert!(
            got.iter().filter(|(_, spread, _)| *spread > 1.0).count() > 20,
            "the corpus has to produce colours spread over several blocks"
        );
        assert!(
            got.iter().filter(|(_, _, eff)| *eff > 2.0).count() > 20,
            "the corpus has to produce colours resting on several blocks"
        );
        let terms = lines::<weighted::WeightedSets>(&records, Line::Terms, usize::MAX);
        assert_eq!(got.len(), terms.len());

        for (k, (line, &(mean, spread, effective))) in terms.iter().zip(&got).enumerate() {
            let body = line.split('\t').nth(1).unwrap();
            let pairs: Vec<(f64, f64)> = if body.is_empty() {
                Vec::new()
            } else {
                body.split(',')
                    .map(|t| {
                        let (c, e) = t.rsplit_once(':').unwrap();
                        (c.parse().unwrap(), e.parse::<f64>().unwrap())
                    })
                    .collect()
            };
            let mass: f64 = pairs.iter().map(|(w, _)| w).sum();
            let want_mean: f64 = pairs.iter().map(|(w, b)| w * b).sum::<f64>() / mass;
            let want_var: f64 =
                pairs.iter().map(|(w, b)| w * (b - want_mean).powi(2)).sum::<f64>() / mass;
            let want_eff: f64 = mass * mass / pairs.iter().map(|(w, _)| w * w).sum::<f64>();

            let tol = |x: f64| 1e-7 * x.abs().max(1.0);
            assert!((mean - want_mean).abs() < tol(want_mean), "record {} mean", k);
            assert!(
                (spread - want_var.max(0.0).sqrt()).abs() < tol(want_var.sqrt()),
                "record {} spread: {} against {}",
                k,
                spread,
                want_var.sqrt()
            );
            assert!((effective - want_eff).abs() < tol(want_eff), "record {} effective", k);
        }
    }

    #[test]
    fn a_color_sums_to_one_so_the_sum_is_the_mean() {
        let mut records = String::new();
        for block in 0..8 {
            records += &record(block, block, &[], 4);
        }
        // Ten rounds of transactions, each spending four earlier ones at
        // lopsided amounts, so the weights are anything but equal.
        let mut tx = 8;
        for round in 0..10 {
            for k in 0..4 {
                let spends: Vec<(usize, usize)> = (0..4)
                    .map(|i| (tx - 8 + i, 1 + (i * 7 + k * 3 + round) % 23))
                    .collect();
                records += &record(20 + round, tx, &spends, 4);
                tx += 1;
            }
        }

        let mut store = weighted::WeightedSets::new();
        let mut colors: HashMap<usize, (weighted::Color, usize)> = HashMap::new();
        let mut reader = sexp::Reader::new(records.as_bytes());
        let mut inputs: Vec<sexp::Input> = Vec::new();
        let (mut worst_drift, mut worst_gap) = (0.0f64, 0.0f64);

        // The colouring again, in the shape a test can look inside: what `run`
        // does per record, minus the emit.
        while let Some(record) = reader.next_record(&mut inputs).unwrap() {
            let color = if inputs.is_empty() {
                store.singleton(record.block_id)
            } else {
                let total: f64 = inputs.iter().map(|i| i.amount as f64).sum();
                let mut accumulator: Option<weighted::Color> = None;
                for i in (0..inputs.len()).rev() {
                    let weight = inputs[i].amount as f64 / total;
                    let (root, _) = colors.remove(&inputs[i].prev_tx_id).expect("an ancestor");
                    accumulator = Some(match accumulator.take() {
                        None => store.scale(&root, weight),
                        Some(acc) => store.combine(&root, weight, &acc, 1.0),
                    });
                    colors.insert(inputs[i].prev_tx_id, (root, 1));
                }
                accumulator.expect("a fold over at least one input")
            };

            let (mut sum, mut mass) = (0.0f64, 0.0f64);
            store.for_each_term(&color, |block, weight| {
                sum += block as f64 * weight;
                mass += weight;
            });
            worst_drift = worst_drift.max((mass - 1.0).abs());
            // The sum against the mean it is claimed to be.
            worst_gap = worst_gap.max((sum - sum / mass).abs());
            colors.insert(record.tx_id, (color, 1));
        }

        // A few ULPs, over a fold forty deep.  The invariant holds, so the
        // division `Line::Sum` does not do would move the number by a few of the
        // last digits it prints and by nothing a reader of a block id reads.
        assert!(worst_drift < 1e-12, "weights drifted from 1 by {}", worst_drift);
        assert!(
            worst_gap < 1e-9,
            "the sum and the mean differ by {}, far enough up to be worth dividing",
            worst_gap
        );
    }

    /// The limit is a record count, and it stops the reader rather than the
    /// printing -- so a short run is a prefix of a long one.
    #[test]
    fn the_limit_takes_a_prefix() {
        let records = record(0, 0, &[], 1) + &record(1, 1, &[], 1) + &record(2, 2, &[], 1);
        assert_eq!(sums(&records, 2), ["0\t0", "1\t1"]);
        assert!(sums(&records, 0).is_empty());
    }

    /// Spending a transaction nobody has seen is the input's mistake, not a
    /// color this program can invent.
    #[test]
    fn spending_an_unknown_transaction_is_an_error() {
        let records = record(0, 9, &[(42, 5)], 1);
        let out = Output::text(Box::new(Shared::default()), Line::Terms);
        let error = run::<RingStore>(usize::MAX, 0, false, out, source(&records), None)
            .expect_err("transaction 42 was never read");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("unknown transaction 42"),
            "{}",
            error
        );
    }

    /// A float is written for all it is worth and no wider: no padding, no
    /// trailing zeros, and nothing rounded away -- what comes out reads back as
    /// the bits that went in.  This is both a coefficient's format and, since
    /// the fixed one went, a sum's.
    #[test]
    fn a_float_is_the_shortest_text_that_reads_back() {
        for value in [0.5, 12.0, 900_000.000_001_5, 2.0 / 3.0, 1e-300] {
            let mut line = Vec::new();
            push_f64(&mut line, value);
            let text = String::from_utf8(line).unwrap();
            assert_eq!(text.parse::<f64>().unwrap(), value, "{} did not survive", text);
        }

        let mut line = Vec::new();
        push_f64(&mut line, 0.5);
        push_f64(&mut line, 12.0);
        push_f64(&mut line, 900_000.000_001_5);
        assert_eq!(String::from_utf8(line).unwrap(), "0.512900000.0000015");
    }

    /// A terms line taken apart into `(exponent, coefficient)` pairs.
    fn terms_of(line: &str) -> Vec<(usize, f64)> {
        let (_, body) = line.split_once('\t').expect("a tab after the tx id");
        if body.is_empty() {
            return Vec::new();
        }
        body.split(',')
            .map(|term| {
                let (coefficient, exponent) = term.split_once(':').expect("coefficient:exponent");
                (exponent.parse().unwrap(), coefficient.parse().unwrap())
            })
            .collect()
    }

    /// The property the binned store rests on: binning is linear, so the fold
    /// over binned leaves is the binned exact fold.  Checked end to end -- the
    /// exact weighted lines, summed within each band, against the bands the
    /// binned store prints -- in `f64` lanes, where the only slack is rounding.
    #[test]
    fn the_bands_are_the_exact_colour_summed_within_each_band() {
        let records = mixed(400, 7);
        let exact = lines::<weighted::WeightedSets>(&records, Line::Terms, usize::MAX);
        // 400 transactions in blocks 0..1200, so 12 bands of 100 blocks.
        bands::configure(bands::Layout::new(12, 1200));
        let binned = lines::<bands::BandStore<f64>>(&records, Line::Terms, usize::MAX);
        assert_eq!(exact.len(), binned.len());
        let mut mixed_colours = 0;
        for (e, b) in exact.iter().zip(&binned) {
            let mut want = vec![0.0f64; 12];
            for (block, weight) in terms_of(e) {
                want[block / 100] += weight;
            }
            let got = terms_of(b);
            // Highest band first, as the exact lines print highest block first.
            let mut last = usize::MAX;
            for &(band, _) in &got {
                assert!(band < last, "bands out of order: {}", b);
                last = band;
            }
            let mut got_dense = vec![0.0f64; 12];
            for (band, weight) in got {
                got_dense[band] = weight;
            }
            for band in 0..12 {
                assert!(
                    (got_dense[band] - want[band]).abs() < 1e-12,
                    "band {} of {}: {} against {}",
                    band,
                    e,
                    got_dense[band],
                    want[band]
                );
            }
            if want.iter().filter(|w| **w > 0.0).count() > 1 {
                mixed_colours += 1;
            }
        }
        assert!(mixed_colours > 50, "the corpus should mix bands: {}", mixed_colours);
    }

    /// The mean read off the bands sits within half a band of the exact one,
    /// since every block is within half a band of its band's centre; and both
    /// lanes hold the colour to sum 1 closely enough for that to be the only
    /// error.
    #[test]
    fn the_binned_mean_lands_within_half_a_band_of_the_exact_one() {
        let records = mixed(400, 7);
        let exact = moments(&records, usize::MAX);
        for width in [50usize, 100, 300] {
            let k = 1200usize.div_ceil(width);
            bands::configure(bands::Layout::new(k, 1200));
            let f32_lines = lines::<bands::BandStore<f32>>(&records, Line::Moments, usize::MAX);
            let f64_lines = lines::<bands::BandStore<f64>>(&records, Line::Moments, usize::MAX);
            for binned in [f32_lines, f64_lines] {
                assert_eq!(exact.len(), binned.len());
                let mut worst = 0.0f64;
                for ((mean, _, _), line) in exact.iter().zip(&binned) {
                    let fields: Vec<f64> = line
                        .split('\t')
                        .skip(1)
                        .map(|v| v.parse().unwrap())
                        .collect();
                    worst = worst.max((fields[0] - mean).abs());
                }
                assert!(
                    worst <= width as f64 / 2.0 + 1e-3,
                    "width {}: worst mean disagreement {}",
                    width,
                    worst
                );
            }
        }
    }

    /// The pool formats a copy of the bands exactly as it does a copy of the
    /// terms, so the lines are the same at every width.
    #[test]
    fn the_bands_print_the_same_lines_at_every_thread_count() {
        let records = mixed(400, 7);
        bands::configure(bands::Layout::new(16, 1200));
        let serial = lines::<bands::BandStore<f32>>(&records, Line::Terms, usize::MAX);
        for threads in [1usize, 2, 3, 8] {
            assert_eq!(
                threaded_lines::<bands::BandStore<f32>>(&records, Line::Terms, usize::MAX, threads),
                serial,
                "--bands disagreed at {} threads",
                threads
            );
        }
    }

    /// A matrix of `records`, written the way `examples/matrix.rs` writes one.
    fn write_matrix(records: &str, path: &str, amounts: bool) {
        let mut writer = matrix::Writer::create(path, amounts).expect("a matrix to write into");
        let mut reader = sexp::Reader::new(records.as_bytes());
        let mut inputs: Vec<sexp::Input> = Vec::new();
        while let Some(record) = reader.next_record(&mut inputs).expect("well formed records") {
            writer.push(&record, &inputs).expect("the matrix takes every record");
        }
        writer.finish().expect("the matrix closes");
    }

    /// The same lines as [`lines`], off a matrix rather than off the text.
    fn matrix_lines<S: ColorStore>(path: &str, form: Line, limit: usize) -> Vec<String> {
        let sink = Shared::default();
        let out = Output::text(Box::new(sink.clone()), form);
        let source = Source::Matrix {
            reader: matrix::Reader::open(path, None).expect("the matrix opens"),
            inputs: Vec::new(),
            record: None,
        };
        run::<S>(limit, 0, false, out, source, None).expect("the matrix folds");
        let written = sink.0.borrow().clone();
        String::from_utf8(written)
            .expect("the output is digits and punctuation")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A directory of this process's own, so two test binaries do not write
    /// over each other -- `image` and `page` name their files the same way.
    fn scratch(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!("foldmat-driver-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("a directory to write matrices into");
        dir.join(name).to_string_lossy().into_owned()
    }

    /// `--matrix` is a swap of readers and nothing below it, so the lines a run
    /// makes off a matrix are the lines the same run makes off the records the
    /// matrix was written from -- every backend, every line form, and a prefix
    /// as well as the whole of it.
    ///
    /// This is the crate's verification habit applied to the reader rather than
    /// to the fold: two implementations of "what a record is", one diff.
    /// `matrix`'s own `--check` compares the two field by field; this compares
    /// what the fold makes of them, which is the thing anybody actually reads.
    #[test]
    fn a_matrix_folds_to_exactly_what_the_records_do() {
        let records = blocked(400, 7);
        let path = scratch("blocked.fm");
        write_matrix(&records, &path, true);

        // Whole runs, and a prefix, since a matrix stops on the record limit
        // the way the reader stops on the end of the text.
        for limit in [usize::MAX, 137] {
            assert_eq!(
                matrix_lines::<RingStore>(&path, Line::Terms, limit),
                lines::<RingStore>(&records, Line::Terms, limit),
                "--rings over a matrix, limit {}",
                limit
            );
            assert_eq!(
                matrix_lines::<colorset::SetStore>(&path, Line::Terms, limit),
                lines::<colorset::SetStore>(&records, Line::Terms, limit),
                "--sets over a matrix, limit {}",
                limit
            );
            for form in [Line::Terms, Line::Sum, Line::Moments] {
                assert_eq!(
                    matrix_lines::<weighted::WeightedSets>(&path, form, limit),
                    lines::<weighted::WeightedSets>(&records, form, limit),
                    "--weighted over a matrix, limit {}",
                    limit
                );
            }
            bands::configure(bands::Layout::new(12, 1200));
            for form in [Line::Terms, Line::Sum, Line::Moments] {
                assert_eq!(
                    matrix_lines::<bands::BandStore<f32>>(&path, form, limit),
                    lines::<bands::BandStore<f32>>(&records, form, limit),
                    "--bands over a matrix, limit {}",
                    limit
                );
            }
        }
        // And the corpus has to be worth comparing over.
        assert_eq!(
            matrix_lines::<weighted::WeightedSets>(&path, Line::Terms, usize::MAX).len(),
            400
        );
        std::fs::remove_file(&path).ok();
    }

    /// A `--structure` matrix keeps the shape of `A` and none of its amounts,
    /// so the unweighted backends fold it exactly and the weighted ones are
    /// refused by name rather than handed equal shares.
    #[test]
    fn a_structure_matrix_is_refused_where_the_weights_matter() {
        let records = blocked(200, 7);
        let path = scratch("structure.fm");
        write_matrix(&records, &path, false);

        // No weights are read, so these are the same lines.
        assert_eq!(
            matrix_lines::<colorset::SetStore>(&path, Line::Terms, usize::MAX),
            lines::<colorset::SetStore>(&records, Line::Terms, usize::MAX)
        );

        // And the refusal, through the place the command line is settled.
        let refused = plan(
            None,
            None,
            None,
            None,
            None,
            image::Ink::Weighted,
            Line::Sum,
            0,
            false,
            usize::MAX,
            Some(&path),
            true,
        )
        .err()
        .expect("a weighted fold cannot run on a matrix with no amounts");
        assert!(
            refused.contains("--structure"),
            "the refusal has to name the flag that wrote it: {}",
            refused
        );
        // The same file under an unweighted backend is not refused at all.
        assert!(plan(
            None, None, None, None, None, image::Ink::Flat, Line::Terms, 0, false, usize::MAX,
            Some(&path), false,
        )
        .is_ok());
        std::fs::remove_file(&path).ok();
    }

    /// The same lines again, but taken through `plan` -- the call the binary
    /// makes -- rather than by opening the matrix here.
    ///
    /// [`matrix_lines`] asks the reader for a matrix directly and so says
    /// nothing about the command line in front of it.  That is where the
    /// record count was decided, and where it was decided wrongly, so the claim
    /// about a capped run has to be made on this side of `plan` to mean
    /// anything.
    fn planned_lines<S: ColorStore>(
        path: &str,
        form: Line,
        limit: usize,
        weighted: bool,
    ) -> Vec<String> {
        let ink = if weighted {
            image::Ink::Weighted
        } else {
            image::Ink::Flat
        };
        // The `Output` this hands back writes to standard output, which is not
        // where a test reads from; dropped here and replaced with a sink, since
        // what is under test is the `Source` beside it and the limit beside
        // that.
        let (_, source, skip, limit) = plan(
            None, None, None, None, None, ink, form, 0, false, limit, Some(path), weighted,
        )
        .expect("the command line settles");
        let sink = Shared::default();
        let out = Output::text(Box::new(sink.clone()), form);
        run::<S>(limit, skip, false, out, source, None).expect("the matrix folds");
        let written = sink.0.borrow().clone();
        String::from_utf8(written)
            .expect("the output is digits and punctuation")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// A matrix that ends before the run's record limit prints the rows it has
    /// and stops -- exactly what the text reader does at the end of the file.
    ///
    /// This is the central claim under a cap, and the cap is the case it was
    /// broken in.  `plan` used to derive a record count from the limit and
    /// refuse any matrix written for fewer, so `--sets --matrix 500k.foldmat`
    /// printed nothing at all -- "written for 500000 records and this run reads
    /// 1000001" -- where `--sets < 500k-records` printed its 500,000 lines.
    /// Only `all` escaped the check, which is backwards: the hour-long
    /// whole-chain run went unchecked and the cheap capped ones were refused.
    ///
    /// [`oracle`]'s inequality, which that was copied from, does not carry: a
    /// short oracle answers `NEVER` where the truth is "later" and silently
    /// frees a colour the rest of the run still reads, where a short matrix
    /// simply ends, with no wrong answer anywhere.  So the assertion is not
    /// that the short matrix is refused, it is that it prints the same prefix
    /// the records do -- at the default limit, which is the run somebody
    /// actually types.
    #[test]
    fn a_matrix_shorter_than_the_run_prints_the_prefix_it_has() {
        let records = blocked(50, 7);
        let path = scratch("short.fm");
        write_matrix(&records, &path, true);

        // 200 and 1,000,001 rows asked of 50, and the whole of it asked for
        // exactly: every one of them is the 50 rows the file holds.
        for limit in [200usize, DEFAULT_LIMIT, 50] {
            for form in [Line::Terms, Line::Sum, Line::Moments] {
                assert_eq!(
                    planned_lines::<weighted::WeightedSets>(&path, form, limit, true),
                    lines::<weighted::WeightedSets>(&records, form, limit),
                    "--weighted off a 50-row matrix at limit {}",
                    limit
                );
            }
            assert_eq!(
                planned_lines::<colorset::SetStore>(&path, Line::Terms, limit, false),
                lines::<colorset::SetStore>(&records, Line::Terms, limit),
                "--sets off a 50-row matrix at limit {}",
                limit
            );
            assert_eq!(
                planned_lines::<RingStore>(&path, Line::Terms, limit, false).len(),
                50,
                "a 50-row matrix has 50 rows to print at limit {}",
                limit
            );
        }
        // And a limit inside the matrix, which is the case that always worked:
        // a prefix of the rows, and the same prefix of the records.
        assert_eq!(
            planned_lines::<colorset::SetStore>(&path, Line::Terms, 20, false),
            lines::<colorset::SetStore>(&records, Line::Terms, 20)
        );
        std::fs::remove_file(&path).ok();
    }

    /// `--matrix` replaces the records, so a redirect next to it is bytes
    /// nothing in the run will read, and the run says so rather than ignoring
    /// them.
    ///
    /// The number in that refusal is what [`waiting_records`] answers, and this
    /// is what it has to get right: a regular file counts from where the handle
    /// stands, and everything an empty redirect looks like counts zero.  The
    /// refusal itself needs a redirect on descriptor 0, which is not a thing a
    /// test in this process can arrange; it was checked by hand instead --
    /// `--sets --matrix d500k.fm < d500k.scm` answers "so the 55090609 bytes
    /// still on standard input would be read by nothing at all" and exits 1,
    /// and `< /dev/null` beside the same matrix prints its lines.
    #[test]
    fn a_redirect_beside_a_matrix_is_records_nobody_would_read() {
        let path = scratch("waiting.scm");
        let records = blocked(20, 7);
        std::fs::write(&path, &records).expect("a file of records");

        let mut file = File::open(&path).expect("the records open");
        assert_eq!(waiting_records(Some(&mut file)), records.len() as u64);
        // Half read is half waiting: the offset is the handle's, not zero.
        file.seek(SeekFrom::Start(10)).expect("a seekable file");
        assert_eq!(waiting_records(Some(&mut file)), records.len() as u64 - 10);
        file.seek(SeekFrom::End(0)).expect("a seekable file");
        assert_eq!(waiting_records(Some(&mut file)), 0);

        // The two shapes an empty redirect takes.  `< /dev/null` is a character
        // device, not a file, so it is zero by kind rather than by length --
        // which is the test that keeps the refusal from firing on a wrapper
        // script that redirects standard input away.
        assert_eq!(waiting_records(None), 0, "a pipe hands back no handle at all");
        if let Ok(mut null) = File::open("/dev/null") {
            assert_eq!(waiting_records(Some(&mut null)), 0, "/dev/null holds no records");
        }
        std::fs::remove_file(&path).ok();
    }

    /// A run that stops short of the end of a matrix reads it a second time to
    /// find which blocks its own rows reach, and a path that cannot be read
    /// twice is refused before that second open rather than hung on.
    ///
    /// The fifo is the case: its writer has closed, the bytes are gone, and
    /// `File::open` on it does not fail -- it blocks until somebody writes
    /// again, so the run stops dead with nothing on the screen and nothing to
    /// wait for.  Confirmed by hand before this guard existed, with a
    /// `--png` run over a fifo that had to be killed.  A socket and a character
    /// device are the same answer for the same reason, and a plain file is the
    /// only thing that passes.
    #[test]
    fn a_matrix_a_short_run_cannot_read_twice_is_refused() {
        let file = scratch("twice.fm");
        write_matrix(&blocked(20, 7), &file, true);
        assert!(opens_twice(&file).is_ok(), "a file can be opened twice");
        // Nothing to stat says nothing here: the matrix is opened before this
        // is asked, so a missing path is the reader's complaint to make.
        assert!(opens_twice(&scratch("no-such-matrix.fm")).is_ok());

        assert_eq!(opens_twice("/dev/null"), Err("a character device"));
        assert_eq!(
            opens_twice(&scratch("")),
            Err("a directory"),
            "the scratch directory is not a matrix"
        );

        #[cfg(unix)]
        {
            let socket = scratch("twice.sock");
            std::fs::remove_file(&socket).ok();
            let listener =
                std::os::unix::net::UnixListener::bind(&socket).expect("a socket to stat");
            assert_eq!(opens_twice(&socket), Err("a socket"));
            drop(listener);
            std::fs::remove_file(&socket).ok();

            // The one that hangs.  `mkfifo` rather than a binding, since this
            // crate has no libc dependency and one named pipe is not a reason
            // to take one.
            let fifo = scratch("twice.fifo");
            std::fs::remove_file(&fifo).ok();
            let made = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if made {
                assert_eq!(opens_twice(&fifo), Err("a named pipe"));
                std::fs::remove_file(&fifo).ok();
            }
        }
        std::fs::remove_file(&file).ok();
    }

    /// `--sum` under `--bands` is the *block* mean, not the band's centre.
    ///
    /// The first moment is linear in the colour, so [`bands`] carries it beside
    /// its lanes exactly as it carries the second, and the collapsed forms take
    /// it from there rather than summing it back off the band indices.  Summed
    /// off the indices a term is worth the centre of its band, which is the
    /// same error `--moments` was given the hand-over to avoid: half a band,
    /// 372 blocks at `--bands 1024` over the 2022 chain's block count and
    /// 1,488.5 at `--bands 256`.  Those two numbers are what a block-0 coinbase
    /// printed before the hand-over was extended past `Line::Moments`, with
    /// nothing on the command line refusing the combination.
    ///
    /// So: the coinbase, at both widths, and then the whole corpus against the
    /// exact fold at the precision the carried moments are claimed to hold --
    /// 1.1e-8 blocks, measured over the first 200,000 records of the 2022 chain
    /// and quoted in `Line::Moments`.
    #[test]
    fn the_summed_bands_are_the_exact_first_moment_and_not_a_band_centre() {
        let coinbase = record(0, 0, &[], 1);
        for k in [1024usize, 256] {
            bands::configure(bands::Layout::new(k, bands::CHAIN_BLOCKS));
            assert_eq!(
                lines::<bands::BandStore<f32>>(&coinbase, Line::Sum, usize::MAX),
                vec!["0\t0".to_string()],
                "a block-0 coinbase over {} bands",
                k
            );
        }

        // And the same hand-over on colours that actually mix blocks: `--sum`
        // is `--moments`'s first column, so the two have to agree with each
        // other and both with the exact fold.
        let records = mixed(400, 7);
        let exact: Vec<f64> = sums(&records, usize::MAX)
            .iter()
            .map(|l| l.split('\t').nth(1).unwrap().parse().unwrap())
            .collect();
        for k in [4usize, 12, 64] {
            bands::configure(bands::Layout::new(k, 1200));
            let binned = lines::<bands::BandStore<f64>>(&records, Line::Sum, usize::MAX);
            let means: Vec<f64> = lines::<bands::BandStore<f64>>(&records, Line::Moments, usize::MAX)
                .iter()
                .map(|l| l.split('\t').nth(1).unwrap().parse().unwrap())
                .collect();
            assert_eq!(binned.len(), exact.len());
            let mut worst = 0.0f64;
            for (r, line) in binned.iter().enumerate() {
                let got: f64 = line.split('\t').nth(1).unwrap().parse().unwrap();
                worst = worst.max((got - exact[r]).abs());
                assert!(
                    (got - means[r]).abs() <= 1e-9 * means[r].abs().max(1.0),
                    "{} bands, record {}: --sum {} against --moments {}",
                    k, r, got, means[r]
                );
            }
            assert!(
                worst <= 1.1e-8,
                "{} bands: --sum drifted {} blocks from the exact fold",
                k,
                worst
            );
        }
    }

    /// `--bands` prints the band, not the block: a coinbase in block 250 over
    /// bands of 100 is all of band 2.
    #[test]
    fn a_band_line_names_the_band() {
        bands::configure(bands::Layout::new(8, 800));
        let records = record(250, 0, &[], 1);
        assert_eq!(
            lines::<bands::BandStore<f32>>(&records, Line::Terms, usize::MAX),
            vec!["0\t1:2".to_string()]
        );
        // Its moments read on the block axis and are *exact*: block 250, not
        // the 249.5 centre of the band it falls in.  The band store carries
        // them rather than letting them be summed back off the band index --
        // see `ColorStore::exact_moments`.  The fourth column is the effective
        // count, which is quadratic and so cannot be carried: it counts bands.
        assert_eq!(
            lines::<bands::BandStore<f32>>(&records, Line::Moments, usize::MAX),
            vec!["0\t250\t0\t1".to_string()]
        );
    }
}
