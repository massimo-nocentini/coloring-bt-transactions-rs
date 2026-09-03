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
//! - by default, the color in full — each term as `(exponent . coefficient)`
//!   followed by a space, so a transaction with no color prints nothing after
//!   the tab.  That half of the line is byte for byte what Chicken's `(print*
//!   (car p*) " ")` produces, which is the point: `cut -f2` off this output and
//!   the reference can be diffed against it.
//! - under `--sum`, one number — `sum_b b . weight(b)`, the color's terms added
//!   up.  See [`Line::Sum`] for what that number is and why it takes weights.
//!
//! # A picture instead
//!
//! That output is enormous — a color of a thousand blocks is fourteen thousand
//! bytes of `(block . 1)` — and most of every line is punctuation.  `--png
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
//! coefficients print as fixed-point decimals rather than the integer 1.  That
//! makes the output *not* comparable with the Scheme's, which is why it is a
//! separate mode rather than a change to the existing one.
//!
//! Be aware of what the fixed format hides.  Weights decay by roughly a factor
//! per hop of ancestry, so on a long chain the great majority of a color's terms
//! fall below what [`PLACES`] decimals can show and print as `0.000000`.  They
//! are still there and still counted — the sum is still 1 — but they cannot be
//! read off the output.  See [`push_fixed`].
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

// The camera `--view`'s window is moved by, which is the viewers' own -- see the
// note at the top of the file for why it sits here rather than beside them.
#[cfg(feature = "gui")]
mod camera;
mod colorset;
mod image;
// The canvas `--pdf` and `--fold` fold the picture onto.  The fold itself is
// plain arithmetic and builds everywhere; only the Cairo surface `--pdf`
// paints it onto sits behind the feature, inside the module.  Declared here
// rather than beside the viewers because it is this binary's output, not
// theirs.
mod page;
mod poly;
mod sexp;
mod simd;
mod store;
mod weighted;
// The window `--view` opens on the same canvas, which needs GTK on top of Cairo.
#[cfg(feature = "gui")]
mod window;

use poly::Coeff;
use std::collections::HashMap;
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
}

/// What a line says about a color, once the transaction's id and the tab are
/// out of the way.  `--sum` is the choice between them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Line {
    /// Every term, as `(exponent . coefficient)` followed by a space.  The whole
    /// color, and the Scheme's own line.
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
    /// half from block 3 prints `1.500000`.
    ///
    /// Nothing here divides by the total to make that so.  It could, and an
    /// earlier `tx-mean` binary did; the measured drift from 1 over 42,000
    /// records is 5.6e-16, which moves a mean the size of a block id by around
    /// 1e-12 — six orders of magnitude below the last decimal [`PLACES`] prints.
    /// The division would buy nothing and would quietly turn a broken invariant
    /// into a plausible number, where a sum that is not a mean shows up as one.
    ///
    /// It is also the form that survives the fixed format best.  A weight too
    /// small to print as anything but `0.000000` still moves this by its full
    /// share, because the terms are added before they are rounded rather than
    /// each rounded on its own.
    Sum,
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
                match form {
                    Line::Terms => store.for_each_term(color, |exponent, coefficient| {
                        line.push(b'(');
                        push_int(line, exponent);
                        line.extend_from_slice(b" . ");
                        if S::WEIGHTED {
                            push_fixed(line, coefficient);
                        } else {
                            // Always exactly 1 here, and printed as the integer
                            // the Scheme prints, so an unweighted run stays byte
                            // for byte comparable.
                            push_int(line, coefficient as usize);
                        }
                        line.extend_from_slice(b") ");
                    }),
                    Line::Sum => {
                        let mut sum = 0.0f64;
                        store.for_each_term(color, |block, weight| {
                            sum += block as f64 * weight;
                        });
                        push_fixed(line, sum);
                    }
                }
                line.push(b'\n');
                out.write_all(line)
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
                     [--rings|--sets|--weighted] [--sum] \
                     [--png <file>|--pdf <file>|--fold <file>|--view] \
                     [--blocks <n>] [--bin <n>] [--rows <a>..<b>] [--gain <x>] < records";

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
    limit: usize,
) -> Result<(Output, Box<dyn io::Read>, usize, usize), String> {
    // Held as a file when standard input is one, so that `survey` can read the
    // records and put them back.
    let mut source = rewindable_stdin();

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
        return Ok((
            Output::text(Box::new(io::stdout().lock()), form),
            records_from(source),
            0,
            limit,
        ));
    };

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
    let file = source.as_mut().ok_or_else(|| {
        "standard input cannot be rewound, so the records cannot be counted before the \
         picture is drawn: both of a picture's dimensions are settled before the first \
         record — a PNG states how many rows it has in front of the first one, and a \
         page has to have its canvas before it can count anything into it — and the \
         rows are how many records there are.  Redirect the records from a file \
         (`< records`) rather than through a pipe"
            .to_string()
    })?;
    let start = file.stream_position().map_err(|e| e.to_string())?;
    let (seen, records) = survey(&*file, limit).map_err(|e| e.to_string())?;
    file.seek(SeekFrom::Start(start))
        .map_err(|e| e.to_string())?;

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
    Ok((output, records_from(source), skip, limit))
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

/// The records, from the rewindable handle if there is one and from standard
/// input as it comes otherwise.
///
/// Boxed rather than made another type parameter of [`run`]: this is read a
/// megabyte at a time, so a virtual call per refill is nothing, where another
/// parameter would be another copy of the driver per backend.
fn records_from(source: Option<File>) -> Box<dyn io::Read> {
    match source {
        Some(file) => Box::new(file),
        None => Box::new(io::stdin().lock()),
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
            "--sum" => form = Line::Sum,
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

    // Checked after the whole command line rather than as `--sum` is read, so
    // that it holds whichever order the two flags came in.
    if form == Line::Sum {
        if chose_backend && backend != Backend::Weighted {
            eprintln!(
                "circular-polynomial: --sum adds up the weights of a color, and the \
                 unweighted backends have none to add up; drop --rings or --sets"
            );
            return ExitCode::FAILURE;
        }
        backend = Backend::Weighted;
    }

    // A picture is drawn in whichever ink the backend has to offer, and that is
    // the depth of every sample in it: settled here, where the backend is, and
    // handed to the writer rather than discovered a term at a time.
    let ink = match backend {
        Backend::Weighted => image::Ink::Weighted,
        Backend::Rings | Backend::Sets => image::Ink::Flat,
    };

    let (output, input, skip, limit) =
        match plan(picture, blocks, bin, window, gain, ink, form, limit) {
        Ok(plan) => plan,
        Err(message) => {
            eprintln!("circular-polynomial: {}", message);
            return ExitCode::FAILURE;
        }
    };

    // One instantiation of the loop per backend, so none of them pays for the
    // others existing.
    let outcome = match backend {
        Backend::Rings => run::<RingStore>(limit, skip, stats, output, input),
        Backend::Sets => run::<colorset::SetStore>(limit, skip, stats, output, input),
        Backend::Weighted => run::<weighted::WeightedSets>(limit, skip, stats, output, input),
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
    input: Box<dyn io::Read>,
) -> io::Result<()> {
    let mut reader = sexp::Reader::new(input);

    let mut store = S::new();
    let mut colors: HashMap<usize, (S::Color, usize)> = HashMap::new();
    let mut inputs: Vec<sexp::Input> = Vec::new();

    // Only read when `--stats` is on, but started unconditionally: the clock has
    // to be running before the first record, and one `Instant::now()` per
    // process is not worth branching over.
    let started = Instant::now();
    let mut since_report = started;

    let mut records: usize = 0;
    while records < limit {
        let record = match reader.next_record(&mut inputs)? {
            Some(r) => r,
            None => break,
        };

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
                let unspent = entry.1;

                if unspent > 1 {
                    // Others can still reach this color, so it has to survive
                    // the fold: merge from a borrow, and on the first step take
                    // a second handle rather than the color itself.
                    entry.1 = unspent - 1;
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

        if record.outputs > 0 {
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
            Output::Text { .. } => {}
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

/// How many decimal places a weight, or a sum of them, is printed to.
///
/// Enough to separate two transactions that differ, few enough to hold a column.
/// Six is also well inside what an `f64` can say about a number the size of a
/// block id: a sum near a million still has nine significant digits to spare.
const PLACES: u32 = 6;

/// A non-negative value to [`PLACES`] fixed decimals.
///
/// Fixed rather than shortest-round-trip so a column lines up, and done in
/// integers rather than with `{}`, for the same reason [`push_int`] exists: this
/// runs once per term, tens of millions of times, and float formatting is not
/// cheap.  Scaling by a power of ten and printing two integers costs one
/// multiply and one rounding.
///
/// What that gives up is resolution.  A weight below half of the smallest
/// representable place prints as `0.000000` — the term is still there, and still
/// counts toward [`Line::Sum`], it just cannot be read off the output.  Deep
/// enough ancestry will do that to a weight.
///
/// The scaled value has to fit a `u64`, which at six places leaves room up to
/// about 1.8e13 — some ten million times the length of the chain, so the cast is
/// not a limit anything printed here will reach.
fn push_fixed(out: &mut Vec<u8>, value: f64) {
    let scale = 10u64.pow(PLACES);
    let units = (value * scale as f64).round() as u64;

    push_int(out, (units / scale) as usize);
    out.push(b'.');

    // The fraction is zero-padded to a fixed width, which `push_int` will not do
    // -- it prints 5 as "5" where this needs "000005".
    let mut fraction = units % scale;
    let mut digits = [b'0'; PLACES as usize];
    let mut i = digits.len();
    while fraction > 0 {
        i -= 1;
        digits[i] = b'0' + (fraction % 10) as u8;
        fraction /= 10;
    }
    out.extend_from_slice(&digits);
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

    /// Color `records` with the backend `form` implies and answer the lines.
    fn lines<S: ColorStore>(records: &str, form: Line, limit: usize) -> Vec<String> {
        let sink = Shared::default();
        let out = Output::text(Box::new(sink.clone()), form);
        run::<S>(limit, 0, false, out, Box::new(io::Cursor::new(records.to_string().into_bytes())))
            .expect("the records are well formed");
        let written = sink.0.borrow().clone();
        String::from_utf8(written)
            .expect("the output is digits and punctuation")
            .lines()
            .map(str::to_string)
            .collect()
    }

    /// The sums of a run, which is `--sum`'s backend and no other.
    fn sums(records: &str, limit: usize) -> Vec<String> {
        lines::<weighted::WeightedSets>(records, Line::Sum, limit)
    }

    /// Every line names its transaction and then says what it has to say, with a
    /// tab between the two -- and what follows the tab under [`Line::Terms`] is
    /// still exactly the Scheme's line, trailing space and all.
    #[test]
    fn a_line_is_the_transaction_then_a_tab_then_the_color() {
        let records = record(3, 0, &[], 1) + &record(4, 1, &[(0, 50)], 0);
        let terms = lines::<RingStore>(&records, Line::Terms, usize::MAX);
        assert_eq!(terms, ["0\t(3 . 1) ", "1\t(3 . 1) "]);
        for line in &terms {
            let (id, color) = line.split_once('\t').expect("a tab in every line");
            assert!(id.bytes().all(|b| b.is_ascii_digit()), "{:?}", id);
            assert!(color.ends_with(' '), "the Scheme's trailing space: {:?}", color);
        }
    }

    /// The three backends are three ways to the same answer, and two of them
    /// promise the same bytes.  A color with no terms is an empty half-line
    /// rather than a missing one.
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
            "3\t(5 . 1) (0 . 1) "
        );
    }

    /// Nothing was spent, so the block that minted it holds all of the weight
    /// and the sum is that block, exactly.
    #[test]
    fn a_coinbase_sits_on_its_own_block() {
        assert_eq!(sums(&record(7, 0, &[], 1), usize::MAX), ["0\t7.000000"]);
    }

    /// Half the value from block 0 and half from block 3 puts the sum halfway
    /// between them -- the case [`Line::Sum`] exists to compute.
    #[test]
    fn two_equal_inputs_land_between_their_blocks() {
        let records =
            record(0, 0, &[], 1) + &record(3, 1, &[], 1) + &record(5, 2, &[(0, 50), (1, 50)], 1);
        assert_eq!(sums(&records, usize::MAX).last().unwrap(), "2\t1.500000");
    }

    /// Weighting is by amount, not by input count: nine tenths of the value
    /// coming from block 0 pulls the sum nine tenths of the way to it.
    #[test]
    fn the_sum_follows_the_value_not_the_inputs() {
        let records =
            record(0, 0, &[], 1) + &record(10, 1, &[], 1) + &record(11, 2, &[(0, 90), (1, 10)], 1);
        assert_eq!(sums(&records, usize::MAX).last().unwrap(), "2\t1.000000");
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
            lines[3..].iter().all(|l| l.ends_with("\t2.000000")),
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
        assert_eq!(sums(&records, usize::MAX).last().unwrap(), "2\t3.000000");
    }

    /// The claim [`Line::Sum`] rests on: a color's weights sum to 1, so the sum
    /// of `b . weight(b)` is the weighted mean without anything dividing by
    /// them.  Asserted over a fold deep enough to accumulate drift if there were
    /// any -- and against the mean computed the other way, in full `f64`.
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

        // A few ULPs, over a fold forty deep.  The invariant holds, and the
        // division `Line::Sum` does not do would have changed nothing.
        assert!(worst_drift < 1e-12, "weights drifted from 1 by {}", worst_drift);
        assert!(
            worst_gap < 1e-9,
            "the sum and the mean differ by {}, which the printed decimals would show",
            worst_gap
        );
    }

    /// The limit is a record count, and it stops the reader rather than the
    /// printing -- so a short run is a prefix of a long one.
    #[test]
    fn the_limit_takes_a_prefix() {
        let records = record(0, 0, &[], 1) + &record(1, 1, &[], 1) + &record(2, 2, &[], 1);
        assert_eq!(sums(&records, 2), ["0\t0.000000", "1\t1.000000"]);
        assert!(sums(&records, 0).is_empty());
    }

    /// Spending a transaction nobody has seen is the input's mistake, not a
    /// color this program can invent.
    #[test]
    fn spending_an_unknown_transaction_is_an_error() {
        let records = record(0, 9, &[(42, 5)], 1);
        let out = Output::text(Box::new(Shared::default()), Line::Terms);
        let error = run::<RingStore>(usize::MAX, 0, false, out, Box::new(io::Cursor::new(records.into_bytes())))
            .expect_err("transaction 42 was never read");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(
            error.to_string().contains("unknown transaction 42"),
            "{}",
            error
        );
    }

    /// The fixed format pads the fraction and does not pad the integer part.
    #[test]
    fn the_fixed_format_pads_only_the_fraction() {
        let mut line = Vec::new();
        push_fixed(&mut line, 0.5);
        push_fixed(&mut line, 12.0);
        push_fixed(&mut line, 900_000.000_001_5);
        assert_eq!(
            String::from_utf8(line).unwrap(),
            "0.50000012.000000900000.000002"
        );
    }
}
