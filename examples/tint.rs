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

use std::io::{self, BufRead, BufWriter, Write};

#[allow(dead_code)]
#[path = "../src/oklch.rs"]
mod oklch;

/// One past the largest block id, which fixes the hue arc.
const CHAIN_BLOCKS: f64 = 762_261.0;

enum Form {
    Hex,
    Rgb,
    Png { path: String, side: usize },
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut form = Form::Hex;
    let mut blocks = CHAIN_BLOCKS;
    let mut side = 2048usize;
    let mut png: Option<String> = None;
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
                png = Some(args.get(i + 1).cloned().unwrap_or_default());
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
                eprintln!("tint: unknown option {:?}", other);
                eprintln!("usage: tint [--hex|--rgb|--png <file> [--side <n>]] [--blocks <n>] < moments");
                return Ok(());
            }
        }
        i += 1;
    }
    if let Some(path) = png {
        form = Form::Png { path, side };
    }

    let stdin = io::stdin();
    let input = stdin.lock();

    match form {
        Form::Hex => stream(input, |tx, rgb, out| {
            writeln!(out, "{}\t{}", tx, oklch::hex(rgb))
        }, blocks),
        Form::Rgb => stream(input, |_, rgb, out| out.write_all(&rgb), blocks),
        Form::Png { path, side } => picture(input, &path, side, blocks, total, sample),
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
