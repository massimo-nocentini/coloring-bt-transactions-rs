//! # The colours themselves, from what `--moments` wrote
//!
//! `--moments` answers three numbers a transaction: the mean block its coins
//! came from, the spread about that mean, and how many bands carry it.  A
//! colour is a function of the first two — [`oklch::tint`] — so turning a whole
//! chain's moments into a whole chain's colours needs no second fold, only a
//! pass over the file that already exists.
//!
//! ```text
//! zstd -dc colors.moments.zst | tint --hex   > colors.hex
//! zstd -dc colors.moments.zst | tint --rgb   > colors.rgb
//! zstd -dc colors.moments.zst | tint --index colors.tint --palette colors.plte
//! zstd -dc colors.moments.zst | tint --png colors.png --side 4096
//! ```
//!
//! Which representation depends on what the colours are *for*, and they differ
//! by three orders of magnitude in size:
//!
//! - `--hex` — `<tx> <TAB> #rrggbb`, one line a transaction.  Joins against
//!   anything keyed by transaction id, greps, sorts, pastes into a spreadsheet.
//!   About 18 bytes a record, so 14 GB over the 2022 chain and a tenth of that
//!   compressed.
//! - `--rgb` — three bytes a transaction, no ids, in record order.  2.3 GB for
//!   the chain.  For feeding something that already knows which record is which
//!   and wants the pixels rather than the text.
//! - `--index` — **one** byte a transaction: which of [`oklch::tints`]'s 256
//!   entries the colour falls in.  778 MB for the chain, a third of `--rgb`,
//!   and it is the form that fits in memory beside something else.  The rest of
//!   this page is about it.
//! - `--png` — the picture: one pixel a transaction, laid out row-major on a
//!   square.  778 million pixels is a 28,000-square image that nothing will
//!   open, so `--side` bins consecutive transactions into one pixel and
//!   averages their *moments* before colouring, which is the right way round:
//!   averaging colours mixes hues that mean different things, where averaging
//!   the mean block and the spread first asks "what does this stretch of the
//!   chain look like" and answers it once.
//!
//! The chain height is `--blocks`, defaulting to the 2022 chain's, and it only
//! sets where the hue arc starts and ends.
//!
//! # A byte a transaction
//!
//! `--index` is a third output and not a flag on the others, because it has no
//! block axis: `--png`'s pixel is a transaction at a block and this is a
//! transaction and nothing else.  It is also not a smaller `--rgb`.  Three
//! bytes name a colour; one byte names a *bin*, and a bin is only a colour
//! while the table it indexes is at hand and the axes that cut it are known.
//!
//! **The byte is derived from the moments file and never replaces it.** Every
//! index in a `--index` run can be recomputed from the three columns of
//! `--moments` in a single pass, and nothing can go the other way: a byte does
//! not carry the mean block, or the spread, or the effective count, only which
//! of 256 boxes the first two landed in.  Delete the moments and the colours
//! are all that is left of the fold.
//!
//! ## Three ways a bare byte stream is useless in six months
//!
//! A raw file of 778,613,440 bytes answers no question at all on its own, and
//! the three things it does not say are not the same kind of thing:
//!
//! 1. **What the byte indexes.**  Fixed by writing the table into the file — 3
//!    bytes an entry, [`oklch::TINTS_LEN`] entries, 768 bytes — so a reader
//!    that has never heard of this crate can turn bytes into pixels.
//!    `--palette <file>` writes the same 768 bytes on their own, in the order a
//!    PNG `PLTE` chunk wants them.
//! 2. **What the axes are.**  A palette says what entry 137 looks like and not
//!    what it *means*.  The header carries `--blocks` and the five constants
//!    the colour is built from — [`oklch::SPREAD_HALF`], [`oklch::LIGHTNESS`],
//!    [`oklch::CHROMA`], [`oklch::HUE_FROM`], [`oklch::HUE_ARC`] — as `f64`, so
//!    a file written by a build whose arc was 250° is not silently read by one
//!    whose arc is 300°.
//! 3. **Which record each byte is.**  This is the subtle one.  A byte's
//!    position in the stream is its *record ordinal*, and `--moments`' first
//!    column is its *transaction id*, and the two are not the same number: the
//!    2022 chain has two BIP-30 duplicate coinbases, so its transaction ids go
//!    backwards twice and there are exactly four non-consecutive steps in
//!    778,613,440 records.  A `u32` id a record would fix it and would also
//!    make the file 3.1 GB, which is `--rgb` again and worse.  So the header
//!    carries the first id and a list of every place the run of `+1` breaks —
//!    four entries, 32 bytes, chain-wide — and the id of the record at ordinal
//!    `k` is `tx` of the last break at or before `k`, plus the distance to it.
//!    Confirmed over the first 60,000,000 records of the 2022 chain, where the
//!    breaks are at ordinals 142,783, 142,784, 142,841 and 142,842 and nowhere
//!    else.
//!
//! ## The file
//!
//! Little-endian throughout, and every offset is fixed except the two tables:
//!
//! ```text
//!    0    8  "TINTIDX1"
//!    8    2  u16  hues                        (oklch::TINT_HUES)
//!   10    2  u16  mixes                       (oklch::TINT_MIXES)
//!   12    8  f64  blocks                      (--blocks)
//!   20    8  f64  spread at half chroma       (oklch::SPREAD_HALF)
//!   28    8  f64  lightness                   (oklch::LIGHTNESS)
//!   36    8  f64  chroma                      (oklch::CHROMA)
//!   44    8  f64  hue of block 0, degrees     (oklch::HUE_FROM)
//!   52    8  f64  degrees of arc              (oklch::HUE_ARC)
//!   60    8  u64  records
//!   68    4  u32  transaction id of record 0
//!   72    4  u32  breaks in the id run
//!   76    4  u32  palette bytes (3 * hues * mixes)
//!   80  768  the palette, RGB triples in index order
//!  848    n  one byte a record
//! 848+n   8  each break: u32 record ordinal, u32 transaction id
//! ```
//!
//! `records` and the two counts at 60 are patched in at the end rather than
//! guessed at the start, and the break table is at the *end* for the same
//! reason: neither is known until the input runs out, and the alternative is
//! holding 778 MB of bytes in memory to find out how long they were.  So the
//! header is a fixed 80 bytes and the file is written in one streaming pass.
//!
//! Over the whole chain that is `80 + 768 + 778,613,440 + 32 = 778,614,320`
//! bytes, or 1.0000011 bytes a record — the header is 0.00011% of it, which is
//! the whole argument for making the file say what it is.
//!
//! ## What it costs to run, and what it was checked against
//!
//! `--index` colours every transaction twice: once through
//! [`oklch::tint_index`], which is four multiplications, and once through
//! [`oklch::tint`], which is the gamut bisection, so that the run can report
//! what the byte actually cost rather than assert it.  Over the first
//! 60,000,000 records of the 2022 chain that is 41.40s against `--rgb`'s
//! 22.57s — the difference is the pair of Oklab conversions the report costs
//! and not the index — at 5.4 MB of peak resident against 2.7 MB, both of them
//! a line in and a few bytes out.  It seemed worth 19 seconds a run that no
//! `--index` file exists without a measurement of what it lost, and if it ever
//! stops seeming so the second `tint` call is the line to delete.
//!
//! That run wrote 60,000,880 bytes — `80 + 768 + 60,000,000 + 4 * 8` — and
//! reported 0.005328 mean and 0.014707 worst against the continuous colour,
//! reaching 96 of the 256 entries.  Which 96 is worth reading: fifteen of the
//! thirty-two hue bins, because a prefix whose largest mean block is 344,031 of
//! 762,261 never gets past the fifteenth; and seven of the eight mix slots,
//! because slot 7 wants a spread past 315,000 blocks and no transaction in the
//! chain has one.  See [`oklch::tints`] on why that slot is kept anyway.
//!
//! It was then read back by a program that had only this page to go on:
//!
//! - all five axis constants and `--blocks` came out of the header as written;
//! - all **60,000,000** transaction ids rebuilt exactly from `first_tx` and the
//!   four breaks, checked against column one of the moments file line by line;
//!   the breaks are `(142783, 142726)`, `(142784, 142783)`, `(142841, 142572)`,
//!   `(142842, 142840)` — the two BIP-30 duplicate coinbases, arriving and
//!   departing;
//! - the 768 bytes `--palette` wrote are the 768 bytes in the header;
//! - and byte -> palette -> RGB against what `--rgb` wrote for the same records
//!   is 0.005328 mean and 0.014707 worst in Oklab, 2.21% of them identical, with
//!   **no record anywhere past 0.015** — 96.77% under 0.010 and 99.52% under
//!   0.0125.  That is the guarantee [`oklch::tints`] argues for, met exactly:
//!   the quantised colour is never a just-noticeable difference from the true
//!   one.
//!
//! The same four checks over 2,000,000 records of `--weighted --moments`, whose
//! moments are exact rather than binned into 1024 bands, give 2,000,880 bytes,
//! the same four breaks, 34 of the 256 entries reached, and 0.004765 mean
//! against 0.014703 worst.  936,288 of those 2,000,000 lines carry a different
//! mean or spread from the binned run over the same records and the worst case
//! is the same to five digits, because it is a property of the geometry and not
//! of the data.
//!
//! Adding all this changed nothing that was already here: `--hex`, `--rgb` and
//! `--png --side 512` over 4,000,000 records are byte for byte what the build
//! before it wrote, 62,888,888 and 12,000,000 and 296,684 bytes, held against
//! each other with `cmp`.

use std::io::{self, BufRead, BufWriter, Seek, SeekFrom, Write};
use std::process::ExitCode;

#[allow(dead_code)]
#[path = "../src/oklch.rs"]
mod oklch;

/// One past the largest block id, which fixes the hue arc.
const CHAIN_BLOCKS: f64 = 762_261.0;

const USAGE: &str = "usage: tint [--hex|--rgb|--index <file> [--palette <file>]\
                     |--png <file> [--side <n>] [--sample] [--records <n>]] \
                     [--blocks <n>] < moments";

/// What a reader looks for before believing a byte of the rest.
const MAGIC: &[u8; 8] = b"TINTIDX1";

/// Bytes before the palette, and the same for every file.
const HEADER: usize = 80;

/// How many breaks in the transaction-id run the file will carry.
///
/// The whole 2022 chain has four.  A stream with tens of thousands is not a run
/// of transactions in file order at all — it is sorted, or filtered, or two
/// files concatenated — and for such a stream the run-length trick is the wrong
/// representation and the table would grow without bound.  Refused rather than
/// written, since a `--index` file whose ids cannot be reconstructed is exactly
/// the thing this format exists to prevent.
const BREAKS_MAX: usize = 1 << 16;

enum Form {
    Hex,
    Rgb,
    Png { path: String, side: usize },
    Index { path: String, palette: Option<String> },
}

impl Form {
    /// The flag that chose it, for the messages that refuse a combination.
    fn flag(&self) -> &'static str {
        match self {
            Form::Hex => "--hex",
            Form::Rgb => "--rgb",
            Form::Png { .. } => "--png",
            Form::Index { .. } => "--index",
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // An unknown flag used to fall out of here as `Ok(())` with nothing
            // written and a zero exit, so a typo in a pipeline looked exactly
            // like an empty input and the next stage read a file of no bytes.
            eprintln!("tint: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn run() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut form = Form::Hex;
    let mut blocks = CHAIN_BLOCKS;
    let mut side = 2048usize;
    let mut png: Option<String> = None;
    let mut index: Option<String> = None;
    let mut palette: Option<String> = None;
    // How many records are coming, when the caller knows.  With it the picture
    // is one streaming pass; without it the moments have to be held to work out
    // the bin width, which is 8 bytes a record -- 6.2 GB over the 2022 chain.
    let mut total: Option<usize> = None;
    // Whether a cell shows one transaction or the average of the run it covers.
    let mut sample = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--hex" => form = Form::Hex,
            "--rgb" => form = Form::Rgb,
            "--png" => {
                png = Some(path_after(&args, i, "--png")?);
                i += 1;
            }
            "--index" => {
                index = Some(path_after(&args, i, "--index")?);
                i += 1;
            }
            "--palette" => {
                palette = Some(path_after(&args, i, "--palette")?);
                i += 1;
            }
            "--side" => {
                side = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(side);
                i += 1;
            }
            "--sample" => sample = true,
            "--records" => {
                total = args.get(i + 1).and_then(|v| v.parse().ok());
                i += 1;
            }
            "--blocks" => {
                blocks = args.get(i + 1).and_then(|v| v.parse().ok()).unwrap_or(blocks);
                i += 1;
            }
            other => {
                return Err(io::Error::other(format!(
                    "unknown option {:?}\n{}",
                    other, USAGE
                )));
            }
        }
        i += 1;
    }

    // Two forms that both want a path, and one output: settled after the whole
    // command line rather than by whichever came last, because a run that
    // quietly drew a picture when it was asked for a byte stream would leave
    // the `--index <file>` on the line looking like it had been obeyed.
    match (png, index) {
        (Some(_), Some(_)) => {
            return Err(io::Error::other(
                "--png draws the chain as a picture and --index writes one byte a \
                 transaction; drop one of them",
            ))
        }
        (Some(path), None) => form = Form::Png { path, side },
        (None, Some(path)) => form = Form::Index { path, palette: palette.take() },
        (None, None) => {}
    }

    // A flag that quietly did nothing, refused instead: the other three forms
    // colour every transaction continuously and have no table for a byte to
    // point into, so a palette written beside them would describe nothing that
    // was written.
    if let Some(path) = palette {
        return Err(io::Error::other(format!(
            "--palette {} writes the table the --index bytes point into, so it says \
             nothing about a run that has no index bytes: {} colours straight through \
             the continuous tint; drop one of --palette and {}",
            path,
            form.flag(),
            form.flag()
        )));
    }

    let stdin = io::stdin();
    let input = stdin.lock();

    match form {
        Form::Hex => stream(
            input,
            |tx, rgb, out| writeln!(out, "{}\t{}", tx, oklch::hex(rgb)),
            blocks,
        ),
        Form::Rgb => stream(input, |_, rgb, out| out.write_all(&rgb), blocks),
        Form::Png { path, side } => picture(input, &path, side, blocks, total, sample),
        Form::Index { path, palette } => indices(input, &path, palette.as_deref(), blocks),
    }
}

/// The path a flag wants, refused rather than defaulted to the empty string.
///
/// A word beginning `--` is taken as the next flag rather than as a file name,
/// so `--index --blocks 5` is refused instead of creating a file called
/// `--blocks`.  A path that really does start with two dashes is unreachable
/// this way, and `./--blocks` reaches it; the trade is worth it, because the
/// hole this closes is the one where a mistyped line writes a file nobody
/// asked for and exits 0.
fn path_after(args: &[String], i: usize, name: &str) -> io::Result<String> {
    match args.get(i + 1) {
        Some(v) if !v.starts_with("--") => Ok(v.clone()),
        _ => Err(io::Error::other(format!(
            "{} wants a file to write and nothing followed it",
            name
        ))),
    }
}

/// A line at a time, colour at a time.
fn stream<R: BufRead, F>(input: R, mut emit: F, blocks: f64) -> io::Result<()>
where
    F: FnMut(&str, oklch::Rgb, &mut BufWriter<io::StdoutLock>) -> io::Result<()>,
{
    let stdout = io::stdout();
    let mut out = BufWriter::with_capacity(1 << 20, stdout.lock());
    let mut records: u64 = 0;
    for line in input.lines() {
        let line = line?;
        let mut fields = line.split('\t');
        let tx = fields.next().unwrap_or("");
        let mean: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let spread: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        emit(tx, oklch::tint(mean, spread, blocks), &mut out)?;
        records += 1;
    }
    out.flush()?;
    eprintln!("tint: {} records", records);
    Ok(())
}

/// One byte a transaction, into a file that says what its bytes are.
///
/// The layout is in the module docs.  The order here is header, palette, body,
/// break table, then a seek back over sixteen bytes to write the three counts
/// that were not knowable until the input ended — which is what buys a
/// streaming pass over a file the size of the chain.
fn indices<R: BufRead>(
    input: R,
    path: &str,
    palette_path: Option<&str>,
    blocks: f64,
) -> io::Result<()> {
    let palette = oklch::tints();

    let mut head = [0u8; HEADER];
    head[..8].copy_from_slice(MAGIC);
    head[8..10].copy_from_slice(&(oklch::TINT_HUES as u16).to_le_bytes());
    head[10..12].copy_from_slice(&(oklch::TINT_MIXES as u16).to_le_bytes());
    for (at, value) in [
        (12, blocks),
        (20, oklch::SPREAD_HALF),
        (28, oklch::LIGHTNESS),
        (36, oklch::CHROMA),
        (44, oklch::HUE_FROM),
        (52, oklch::HUE_ARC),
    ] {
        head[at..at + 8].copy_from_slice(&value.to_le_bytes());
    }
    head[76..80].copy_from_slice(&(3 * oklch::TINTS_LEN as u32).to_le_bytes());

    let mut out = BufWriter::with_capacity(1 << 22, std::fs::File::create(path)?);
    out.write_all(&head)?;
    for entry in &palette {
        out.write_all(entry)?;
    }

    let mut records: u64 = 0;
    let mut first_tx: u32 = 0;
    // What the id would be if the run of `+1` held, which chain-wide it does
    // for all but four records.
    let mut expected: u32 = 0;
    let mut breaks: Vec<(u32, u32)> = Vec::new();
    let mut reached = [false; oklch::TINTS_LEN];
    let (mut worst, mut sum) = (0.0f64, 0.0f64);

    for line in input.lines() {
        let line = line?;
        let mut fields = line.split('\t');
        let column = fields.next().unwrap_or("");
        let tx: u32 = column.parse().map_err(|_| {
            io::Error::other(format!(
                "--index has to say which transaction each byte is, and column one of \
                 line {} is {:?} rather than a transaction id; the input is not a \
                 --moments file",
                records + 1,
                column
            ))
        })?;
        let mean: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
        let spread: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);

        if records == 0 {
            first_tx = tx;
        } else if tx != expected {
            let ordinal = u32::try_from(records).map_err(|_| {
                io::Error::other(
                    "the transaction-id run breaks past record 4,294,967,295, which the \
                     break table cannot name; drop --index and keep the moments file",
                )
            })?;
            if breaks.len() == BREAKS_MAX {
                return Err(io::Error::other(format!(
                    "--index names a record by counting from the first transaction id and \
                     listing where the count breaks; the 2022 chain breaks 4 times and \
                     this input has broken {}, so it is not a run of transactions in file \
                     order; drop --index and keep the moments file",
                    BREAKS_MAX
                )));
            }
            breaks.push((ordinal, tx));
        }
        expected = tx.wrapping_add(1);

        let byte = oklch::tint_index(mean, spread, blocks);
        reached[byte as usize] = true;
        // The continuous colour beside the quantised one, so the run reports
        // what the byte cost rather than asserting it.  See the module docs.
        let true_colour = oklch::tint(mean, spread, blocks);
        let difference = oklch::difference(true_colour, palette[byte as usize]);
        sum += difference;
        worst = worst.max(difference);

        out.write_all(&[byte])?;
        records += 1;
    }

    for &(ordinal, tx) in &breaks {
        out.write_all(&ordinal.to_le_bytes())?;
        out.write_all(&tx.to_le_bytes())?;
    }
    out.flush()?;

    let mut file = out
        .into_inner()
        .map_err(|e| io::Error::other(e.to_string()))?;
    let mut patch = [0u8; 16];
    patch[..8].copy_from_slice(&records.to_le_bytes());
    patch[8..12].copy_from_slice(&first_tx.to_le_bytes());
    patch[12..16].copy_from_slice(&(breaks.len() as u32).to_le_bytes());
    file.seek(SeekFrom::Start(60))?;
    file.write_all(&patch)?;
    file.flush()?;

    if let Some(path) = palette_path {
        let mut out = BufWriter::new(std::fs::File::create(path)?);
        for entry in &palette {
            out.write_all(entry)?;
        }
        out.flush()?;
        eprintln!("tint: {} bytes of palette -> {}", 3 * oklch::TINTS_LEN, path);
    }

    let bytes = HEADER + 3 * oklch::TINTS_LEN + records as usize + 8 * breaks.len();
    // An empty input still writes a well-formed file -- a header, a palette and
    // no records -- but there is no bytes-a-record and no dE to report, and a
    // line saying "848.0 a record" would be arithmetic rather than a fact.
    if records == 0 {
        eprintln!(
            "tint: no records -> {}, {} bytes of header and palette and nothing after them",
            path, bytes
        );
        return Ok(());
    }
    eprintln!(
        "tint: {} records -> {}, {} bytes, {:.7} a record; {} of {} entries reached, \
         {} breaks in the transaction-id run; dE against the continuous tint is \
         {:.6} mean, {:.6} worst",
        records,
        path,
        bytes,
        bytes as f64 / records as f64,
        reached.iter().filter(|&&r| r).count(),
        oklch::TINTS_LEN,
        breaks.len(),
        sum / records as f64,
        worst,
    );
    Ok(())
}

/// The chain as a square, `side` by `side`, binning consecutive transactions.
///
/// Two passes are impossible on a pipe and the record count is not known in
/// advance, so the moments are accumulated into the grid as they arrive and the
/// grid is coloured at the end.  That is also why the *moments* are averaged
/// rather than the colours: a bin holds a running mean of the mean block and of
/// the spread, and is asked for its colour once.
fn picture<R: BufRead>(
    input: R,
    path: &str,
    side: usize,
    blocks: f64,
    total: Option<usize>,
    sample: bool,
) -> io::Result<()> {
    let cells = side * side;
    let mut sum_mean = vec![0.0f64; cells];
    let mut sum_spread = vec![0.0f64; cells];
    let mut counts = vec![0u32; cells];

    // With the record count in hand the bin a record belongs to is known as it
    // arrives, so nothing is held but the grid.  Without it the width cannot be
    // worked out until the records run out, and the moments have to be kept --
    // 8 bytes a record, 6.2 GB over the 2022 chain, which is why `--records`
    // exists and why the message below says which path was taken.
    let records;
    match total {
        Some(n) => {
            let per = n.div_ceil(cells).max(1);
            let mut seen = 0usize;
            for line in input.lines() {
                let line = line?;
                let mut fields = line.split('\t');
                let _tx = fields.next();
                let mean: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let spread: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let cell = (seen / per).min(cells - 1);
                // Averaging a run of transactions smooths away the very thing
                // the picture is about: the mean block climbs with the record
                // index, so an averaged cell is nearly a function of its
                // position and the image becomes a gradient.  Sampling keeps
                // one real transaction's colour instead, variance and all.
                if !sample || counts[cell] == 0 {
                    sum_mean[cell] += mean;
                    sum_spread[cell] += spread;
                    counts[cell] += 1;
                }
                seen += 1;
            }
            records = seen;
            if seen != n {
                eprintln!(
                    "tint: --records said {} and {} arrived; the picture is binned to the \
                     promise, so the tail may be short or crowded",
                    n, seen
                );
            }
        }
        None => {
            let mut all: Vec<(f32, f32)> = Vec::new();
            for line in input.lines() {
                let line = line?;
                let mut fields = line.split('\t');
                let _tx = fields.next();
                let mean: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                let spread: f64 = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0.0);
                all.push((mean as f32, spread as f32));
            }
            records = all.len();
            if records == 0 {
                return Err(io::Error::other("no records, so there is no picture"));
            }
            let per = records.div_ceil(cells).max(1);
            for (k, &(mean, spread)) in all.iter().enumerate() {
                let cell = (k / per).min(cells - 1);
                sum_mean[cell] += mean as f64;
                sum_spread[cell] += spread as f64;
                counts[cell] += 1;
            }
        }
    }
    if records == 0 {
        return Err(io::Error::other("no records, so there is no picture"));
    }
    let per = total.unwrap_or(records).div_ceil(cells).max(1);

    let mut rgb = Vec::with_capacity(cells * 3);
    for cell in 0..cells {
        let n = counts[cell];
        if n == 0 {
            // Past the end of the records: paper, so the tail of the last row
            // is visibly empty rather than black.
            rgb.extend_from_slice(&[255, 255, 255]);
        } else {
            let c = oklch::tint(
                sum_mean[cell] / n as f64,
                sum_spread[cell] / n as f64,
                blocks,
            );
            rgb.extend_from_slice(&c);
        }
    }

    write_png(path, side, side, &rgb)?;
    eprintln!(
        "tint: {} records over {} x {} cells, {} a cell -> {}",
        records, side, side, per, path
    );
    Ok(())
}

/// A truecolour PNG, which is `IHDR`, one `IDAT` and `IEND`.
///
/// The crate's own [`crate::image`] writer draws one channel; this needs three,
/// and it is twenty lines, so it is here rather than a mode there.
fn write_png(path: &str, width: usize, height: usize, rgb: &[u8]) -> io::Result<()> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;

    let mut scanlines = Vec::with_capacity(height * (1 + width * 3));
    for y in 0..height {
        scanlines.push(0u8); // no filter
        scanlines.extend_from_slice(&rgb[y * width * 3..(y + 1) * width * 3]);
    }
    let mut zip = ZlibEncoder::new(Vec::new(), Compression::new(6));
    zip.write_all(&scanlines)?;
    let idat = zip.finish()?;

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8 bits, truecolour

    let mut out = BufWriter::new(std::fs::File::create(path)?);
    out.write_all(b"\x89PNG\r\n\x1a\n")?;
    for (name, data) in [(b"IHDR", &ihdr[..]), (b"IDAT", &idat[..]), (b"IEND", &[][..])] {
        out.write_all(&(data.len() as u32).to_be_bytes())?;
        out.write_all(name)?;
        out.write_all(data)?;
        let mut crc = flate2::Crc::new();
        crc.update(name);
        crc.update(data);
        out.write_all(&crc.sum().to_be_bytes())?;
    }
    out.flush()
}
