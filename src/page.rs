//! # The same picture, on a page
//!
//! [`image`](crate::image) draws one pixel per (transaction, block) pair, which
//! is the whole answer and nothing but it — and on a real run that is a picture
//! no reader will open.  The million records the driver stops at, over the
//! hundred and thirty-five thousand blocks they reach back through, come to
//! 135,659 x 1,000,001: a hundred and thirty-five *gigapixels*, 1.8 GB of PNG,
//! and every decoder that meets it tries to allocate the raster and gives up.
//! The file is well formed; it is simply larger than the thing it was drawn to
//! be looked at with.
//!
//! So this draws the same answer at a size a page can hold.  A canvas of at most
//! [`DEFAULT_PAGE`] cells each way, one PDF page of it, and the picture's pixels
//! folded into the cells that cover them.  Nothing else changes: the rows are
//! still the records in the order they arrive, the columns are still block ids
//! counting up from 0, and `--bin` still folds transactions together before any
//! of this sees them.
//!
//! ## What a cell says
//!
//! A cell covers a rectangle of the full picture — some span of block ids by
//! some span of records — and it is shaded by how much of that rectangle is
//! inked.  Coverage is counted exactly: the spans are the ones the cell actually
//! covers (they differ by one from cell to cell when the picture does not divide
//! evenly), a block inked twice in one row counts once because a row is a union,
//! and a row the records never reached counts as blank rather than as absent.
//!
//! A pixel need not be all ink or none, and under `--weighted` it is not: it
//! carries the share of the transaction's value that came through the block, and
//! a cell adds up the shares rather than counting the pixels.  Unweighted every
//! share is 1, so the sum *is* the count and this is the same page it always
//! was; weighted, a cell of the same shape comes out lighter, because the ink in
//! it genuinely is.
//!
//! Coverage is not shade, though.  A colour of a thousand blocks in a cell
//! eighty-five blocks by six hundred records is a coverage of a few percent, and
//! a few percent of black on white is white as far as the eye is concerned.  So
//! the shade is `coverage^(1/TONE)` — a plain gamma curve, which lifts sparse
//! ink into something visible while keeping the ordering, so a darker cell still
//! means more ink than a lighter one.  What it does *not* mean is twice the ink
//! for twice the darkness; read the page for shape, and the text output or a
//! `--blocks`-narrowed PNG for quantities.  It is
//! [`image::shade`](crate::image::shade), which is the curve a weighted *pixel*
//! is drawn with as well.
//!
//! ## Cells are not square, on purpose
//!
//! Each axis is capped on its own, so a picture larger than the page both ways
//! fills the page both ways.  A cell of the 135,659 x 1,000,001 run above is
//! then 133 blocks by 977 records, which is not a square of anything.
//!
//! That is the right distortion to take here, because the two axes are not the
//! same kind of quantity to begin with — one is a block id and the other is a
//! position in the record stream — so there is no aspect ratio to preserve.
//! Scaling both by the smaller factor instead would draw that run as a strip 135
//! cells wide down a page a thousand tall, which throws away the axis with the
//! most in it.  `--bin` already rescales one axis and not the other for the same
//! reason.
//!
//! ## Why a page rather than a bigger raster
//!
//! Cairo measures a page in points, and this gives it one point per cell, the
//! way the viewers' `e` gives it one point per window pixel: a 1024-cell canvas
//! becomes a 1024-point page, a big sheet at the shape of the drawing.  What
//! goes on the page is a single greyscale image at exactly that size — one
//! device pixel per point — so the reader does no resampling and the file is a
//! few hundred kilobytes whatever the picture behind it was.
//!
//! A vector page was the other option and is the wrong one at this size: a
//! rectangle per inked cell is a million paths, which is a slower page and a
//! larger file than the image, and buys sharpness a picture of *coverage* has no
//! use for.
//!
//! ## What it costs to draw
//!
//! One `f32` per cell — four megabytes at the default page, whatever the
//! picture — plus a bit and a weight per column for the row being built, so that
//! a bin's transactions union rather than accumulate.  Both dimensions are still settled
//! before the first record, because the canvas has to be allocated; that is the
//! same rewind-and-count pass a PNG needs, and `main`'s `survey` is the same
//! walk for both.

use std::io;
use std::io::Write as _;

// Cairo is only the last step -- the surface a finished canvas is painted onto
// -- so it is the only part of this file behind the `pdf` feature: the fold
// itself, and the PNG the canvas can be written as instead, build everywhere.
#[cfg(feature = "pdf")]
use cairo::{Context, Format, ImageSurface, PdfSurface};

use flate2::write::ZlibEncoder;
use flate2::{Compression, Crc};

use crate::image;

/// How many cells the canvas gets each way, and so how many points the page is
/// each way.
///
/// 1024 points is a fourteen-inch sheet: larger than anything one would print,
/// which is what one wants from a page that is being read on a screen and zoomed
/// into.  Every cell of it is a rectangle of the picture, so more cells is more
/// of the picture told apart and not more paper.
pub const DEFAULT_PAGE: usize = 1024;

/// The most cells a canvas will take each way: 14,400.
///
/// Two reasons that happen to be the same number.  A PDF states its page size in
/// `/MediaBox`, in points, and readers hold it to 200 inches — 14,400 points, and
/// one point is one cell here, so a larger canvas is a page that is out of spec
/// whatever cairo will happily write.  And a canvas costs four bytes a cell
/// while the run is going, so 14,400 each way is already 830 MB of counting; a
/// ceiling an order of magnitude higher would be one nothing could allocate
/// behind.
///
/// Nothing the driver asks for comes near it — it draws at [`DEFAULT_PAGE`] —
/// but a `Writer` is told its canvas, so the ceiling is stated where the canvas
/// is taken.
pub const MAX_PAGE: usize = 14_400;

/// A picture being drawn a transaction at a time, onto a canvas of fixed size.
///
/// The interface is [`image::Writer`](crate::image::Writer)'s, and deliberately:
/// the driver's `Output` holds one or the other and the loop that feeds them
/// does not know which.
pub struct Writer {
    /// Where the page goes.  Held rather than opened, since the surface is not
    /// started until [`Writer::finish`] has the counts to draw.
    path: String,
    /// Columns of the full picture, i.e. how many block ids it has room for.
    width: usize,
    /// Rows of the full picture: the records, divided by the bin.
    height: usize,
    /// Transactions to a row, exactly as the PNG means it.
    bin: usize,
    /// Cells across the canvas, at most `width`.
    across: usize,
    /// Cells down it, at most `height`.
    down: usize,
    /// `block -> cell column`, so that inking a block is a load rather than a
    /// division.  A colour's terms are the innermost loop of the whole program
    /// and there are billions of them in a run.
    column_of: Vec<u32>,
    /// How much ink the picture pixels a cell covers came to, `across * down` of
    /// them, row major.  A pixel's ink is its weight, which is 1 for every term
    /// an unweighted colour has, so this is a count of inked pixels there.
    counts: Vec<f32>,
    /// Where the row being built writes: `cell row * across`, recomputed once a
    /// row rather than once a term.
    band: usize,
    /// Which blocks the row being built has already inked, one bit each, so that
    /// a bin's transactions union.  All 0s outside `..dirty`, which is what lets
    /// a closed row be blanked by clearing a prefix.
    row: Vec<u8>,
    /// How much ink each of those blocks has in the row so far, one per column.
    ///
    /// Only read where `row` says the block is inked, which is why it is never
    /// blanked: a stale weight is unreachable until the bit beside it is set
    /// again, and setting it writes the weight in the same breath.
    weights: Vec<f32>,
    dirty: usize,
    /// Transactions drawn into the row being built, always below `bin`.
    pending: usize,
    /// Rows finished so far, at most `height`.
    rows: usize,
    /// The first block id seen that the picture has no column for, if any.
    escaped: Option<usize>,
    /// What every cell's ink is multiplied by before shading; see
    /// [`Writer::set_gain`].
    gain: f32,
}

impl Writer {
    /// Open a page at `path` for a picture `width` columns by `height` rows,
    /// with `bin` transactions to the row, folded onto a canvas of at most
    /// `page` cells each way.
    ///
    /// Nothing is written here — a PDF is not a stream of scanlines and the page
    /// is drawn in one go at the end — but the canvas is allocated, which is why
    /// both dimensions are still wanted up front.
    pub fn new(
        path: &str,
        width: usize,
        height: usize,
        bin: usize,
        page: usize,
    ) -> io::Result<Self> {
        assert!(bin > 0, "a row has to stand for at least one transaction");
        assert!(page > 0, "a page of no cells has nothing to draw in");
        if page > MAX_PAGE {
            return Err(io::Error::other(format!(
                "a canvas {} cells each way is past the ceiling of {}",
                page, MAX_PAGE
            )));
        }
        if width == 0 || height == 0 {
            return Err(io::Error::other(
                "an image needs a column and a row, and this one has none",
            ));
        }

        // Each axis capped on its own -- see the module docs for why the shape
        // of the picture is not preserved.  Never enlarged: a picture smaller
        // than the page is drawn at one cell per pixel and stays exact.
        let across = width.min(page);
        let down = height.min(page);

        let column_of = (0..width).map(|b| (b * across / width) as u32).collect();
        let cells = across.checked_mul(down).ok_or_else(|| {
            io::Error::other(format!(
                "a {} x {} canvas is more than can be held",
                across, down
            ))
        })?;

        Ok(Writer {
            path: path.to_string(),
            width,
            height,
            bin,
            across,
            down,
            column_of,
            counts: vec![0.0; cells],
            band: 0,
            row: vec![0; width.div_ceil(8)],
            weights: vec![0.0; width],
            dirty: 0,
            pending: 0,
            rows: 0,
            escaped: None,
            gain: 1.0,
        })
    }

    /// Multiplies every cell's ink by `gain` before it is shaded, clamped at
    /// full coverage.
    ///
    /// The shade curve lifts sparse ink, and for the unweighted picture that
    /// is enough.  A *weighted* colour's mass, though, is a distribution over
    /// its whole width --- a cell of a hundred columns holds around a
    /// hundredth of one row's value however the row is drawn --- so a folded
    /// weighted page is genuinely, and uselessly, near white.  Gain is the
    /// declared correction: the shade stands for `gain` times the ink, the
    /// caller says how much, and a caption that quotes it is telling the
    /// truth about the picture.  At 1 nothing changes.
    pub fn set_gain(&mut self, gain: f64) {
        self.gain = gain as f32;
    }

    /// Ink the pixel for `block` in the row being built, with `weight` as its
    /// coefficient — 1 for every term an unweighted colour has, and the share of
    /// the transaction's value that came through the block for a weighted one.
    ///
    /// A block already inked in this row is not counted again: a row is the
    /// union of the transactions binned into it, so the second transaction to
    /// reach a block adds nothing to the row and must add nothing to the cell.
    /// Where the second one is *heavier*, the row takes the difference and no
    /// more — the union of two weighted pixels is the darker of them, which is
    /// the same rule as "counted once" when every weight is 1.
    ///
    /// A block the picture has no column for is remembered rather than reported,
    /// for the reason [`image::Writer::set`](crate::image::Writer::set) gives:
    /// this runs inside the store's walk over the colour's terms, which has
    /// nowhere to put an error.
    #[inline]
    pub fn set(&mut self, block: usize, weight: f64) {
        if block >= self.width {
            self.escaped.get_or_insert(block);
            return;
        }
        let byte = block / 8;
        let bit = 0x80 >> (block % 8);
        let weight = weight as f32;
        // Nothing there yet is nothing to keep: the bit is what says whether the
        // weight beside it belongs to this row at all.
        let held = if self.row[byte] & bit != 0 {
            self.weights[block]
        } else {
            self.row[byte] |= bit;
            if byte >= self.dirty {
                self.dirty = byte + 1;
            }
            self.weights[block] = 0.0;
            0.0
        };
        if weight <= held {
            return;
        }
        self.weights[block] = weight;
        self.counts[self.band + self.column_of[block] as usize] += weight - held;
    }

    /// Finish one transaction, closing the row if it completes a bin.
    pub fn end_transaction(&mut self) -> io::Result<()> {
        if let Some(block) = self.escaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "block {} has no column in a {}-column picture; \
                     rerun with --blocks {} or more",
                    block,
                    self.width,
                    block + 1
                ),
            ));
        }
        self.pending += 1;
        if self.pending == self.bin {
            self.end_row()?;
        }
        Ok(())
    }

    /// Close the row being built and start the next, which may or may not be in
    /// the same band of cells.
    fn end_row(&mut self) -> io::Result<()> {
        if self.rows == self.height {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "the picture was opened {} rows tall and the records have filled it; \
                     the count they were measured by is not the count they are arriving at",
                    self.height
                ),
            ));
        }
        // Bits are only ever set below `dirty`, so that prefix is the only part
        // of the row that can have gone dark -- see `image::Writer::end_row` for
        // why that is the difference it is.
        self.row[..self.dirty].fill(0);
        self.dirty = 0;
        self.pending = 0;
        self.rows += 1;
        self.band = self.rows * self.down / self.height * self.across;
        Ok(())
    }

    /// `(columns, rows)` of the full picture, which is what it would be as a
    /// PNG.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// `(cells across, cells down)`, which is also the page in points.
    pub fn canvas(&self) -> (usize, usize) {
        (self.across, self.down)
    }

    /// How many transactions a row of the picture stands for, which is `--bin`.
    ///
    /// Only the window asks: a page says nothing about itself, while the panel
    /// turns a row back into the record it started at.
    #[cfg(feature = "gui")]
    pub fn bin(&self) -> usize {
        self.bin
    }

    /// Close the row in hand, if the records stopped part way through one.
    ///
    /// A bin that never filled up is still a row: the run does not owe the
    /// picture a whole bin's worth of transactions at the end of the records.
    /// Separate from [`Writer::finish`] because the window closes the picture
    /// without writing one.
    pub fn close(&mut self) -> io::Result<()> {
        if self.pending > 0 {
            self.end_row()?;
        }
        Ok(())
    }

    /// The canvas as ink: one byte a cell, row major, [`shade`]d from the ink in
    /// them.
    ///
    /// 255 is a cell that is all ink and 0 is one that is all paper, so this is
    /// a *mask* rather than a picture — whatever draws it chooses the colour,
    /// and both things that draw it choose black.
    ///
    /// Rows the records never reached are shaded by the ink that *is* in them
    /// over the whole rectangle they stand for, blank rows included.  That is
    /// the honest reading of a picture opened taller than the records filled.
    pub fn shades(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.across * self.down];
        for cy in 0..self.down {
            // How many picture rows and columns this row of cells covers.  They
            // differ by one across the canvas whenever the picture does not
            // divide evenly, and a denominator that ignored that would shade the
            // odd cell wrong.
            let tall = span(self.height, self.down, cy);
            let band = cy * self.across;
            for cx in 0..self.across {
                let ink = self.counts[band + cx];
                if ink <= 0.0 {
                    continue;
                }
                let area = span(self.width, self.across, cx) * tall;
                // Gain lifts the ink and full coverage caps it: a cell cannot
                // claim to be more than all ink.
                out[band + cx] = shade((ink * self.gain).min(area as f32), area);
            }
        }
        out
    }

    /// Close the picture, shade every cell, and write the canvas as an 8-bit
    /// greyscale PNG: the same drawing `--pdf` puts on a page, as a raster any
    /// build can write --- no Cairo, no feature, nothing installed.
    ///
    /// The shades are a mask (255 is ink), a PNG sample is a brightness (255
    /// is paper), so the byte is flipped on the way through; everything else
    /// --- the fold, the gamma, the cell spans --- is exactly the page's.
    pub fn finish_png(mut self) -> io::Result<()> {
        self.close()?;
        let mut samples = self.shades();
        for s in samples.iter_mut() {
            *s = 255 - *s;
        }
        write_png_gray8(&self.path, self.across, self.down, &samples)
    }

    /// Close the picture, shade every cell, and write the page.
    #[cfg(feature = "pdf")]
    pub fn finish(mut self) -> io::Result<()> {
        self.close()?;
        let ink = mask(&self.shades(), self.across, self.down)?;

        // One point a cell, the way the viewers' export gives one point a window
        // pixel.
        let page = PdfSurface::new(self.across as f64, self.down as f64, &self.path).map_err(
            |e| io::Error::other(format!("{}: the page could not be started ({})", self.path, e)),
        )?;
        {
            let cr = Context::new(&page).map_err(|e| {
                io::Error::other(format!("{}: nothing to draw with ({})", self.path, e))
            })?;
            paper(&cr).map_err(|e| {
                io::Error::other(format!("{}: the paper could not be laid ({})", self.path, e))
            })?;
            stamp(&cr, &ink, 0.0, 0.0, 1.0).map_err(|e| {
                io::Error::other(format!("{}: the ink could not be laid ({})", self.path, e))
            })?;
        }
        page.finish();
        Ok(())
    }
}

/// [`Writer::shades`] as an A8 surface, which is the form Cairo will paint.
///
/// A copy rather than a view because an image surface pads its rows out to a
/// multiple of four bytes and a canvas of shades does not; `stride` is where
/// that padding is accounted for, and it is the only reason this is a loop.
#[cfg(feature = "pdf")]
pub fn mask(shades: &[u8], across: usize, down: usize) -> io::Result<ImageSurface> {
    debug_assert_eq!(shades.len(), across * down);
    let mut ink = ImageSurface::create(Format::A8, across as i32, down as i32)
        .map_err(|e| io::Error::other(format!("the canvas could not be made ({})", e)))?;
    {
        let stride = ink.stride() as usize;
        let mut data = ink
            .data()
            .map_err(|e| io::Error::other(format!("the canvas could not be drawn on ({})", e)))?;
        for cy in 0..down {
            let line = cy * stride;
            let band = cy * across;
            data[line..line + across].copy_from_slice(&shades[band..band + across]);
        }
    }
    Ok(ink)
}

/// Covers whatever `cr` draws on in paper.
///
/// A PDF page is transparent until something covers it, and a picture of black
/// ink on nothing is a picture of black ink on whatever the reader happens to
/// put behind it.  A window has the same hole in it, filled the same way.
#[cfg(feature = "pdf")]
pub fn paper(cr: &Context) -> Result<(), cairo::Error> {
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint()
}

/// Draws the canvas in black, with its top-left corner at `(x, y)` and each cell
/// `scale` units across.
///
/// A cell is a *sample* here — a rectangle of the picture, painted the shade of
/// how much of it is inked — which is what a cell has to be while it is around a
/// pixel across.  Once there is room for a cell to be a shape rather than a
/// sample the window draws it as one instead; see `window`'s own docs for where
/// the two change over and why the page never reaches it.
#[cfg(feature = "pdf")]
pub fn stamp(
    cr: &Context,
    ink: &ImageSurface,
    x: f64,
    y: f64,
    scale: f64,
) -> Result<(), cairo::Error> {
    cr.save()?;
    cr.translate(x, y);
    cr.scale(scale, scale);
    cr.set_source_rgb(0.0, 0.0, 0.0);

    let mask = cairo::SurfacePattern::create(ink);
    // A cell blown up past a pixel is a cell and should look like one: at a
    // pixel a cell and above, the samples are drawn as the squares they are,
    // rather than smeared into each other by an interpolation that invents
    // coverage the records never had.  Shrinking is the other way round — there
    // the neighbours have to be averaged or most of them are simply not looked
    // at, which is how a picture loses the thin diagonal that is the whole shape
    // of it.
    mask.set_filter(if scale >= 1.0 {
        cairo::Filter::Nearest
    } else {
        cairo::Filter::Good
    });
    cr.mask(&mask)?;

    cr.restore()
}

/// How much of a cell's black gets through, given that the `area` picture pixels
/// it covers came to `ink` between them.
///
/// An A8 surface is a mask, so 255 is the ink and 0 is the paper, which is the
/// way round [`image::shade`] answers; the curve and what it does to the low end
/// are written down there.  A cell every pixel of which is fully inked is the
/// ink itself, and one with nothing in it is paper.
fn shade(ink: f32, area: usize) -> u8 {
    debug_assert!(
        ink as f64 <= area as f64 + 1e-3,
        "a cell cannot hold more ink than it has room for"
    );
    image::shade(ink as f64 / area as f64)
}

/// How many of `total` things fall in cell `i` of `cells`, under the mapping
/// `thing -> thing * cells / total` that [`Writer::set`] and [`Writer::end_row`]
/// place things by.
///
/// The inverse of that floor, counted rather than approximated by `total /
/// cells`: `i` holds the things from `ceil(i * total / cells)` up to but not
/// including `ceil((i + 1) * total / cells)`.  With `cells <= total` — which is
/// how the canvas is sized — that is never empty, so it is never a denominator
/// of zero.
fn span(total: usize, cells: usize, i: usize) -> usize {
    ((i + 1) * total).div_ceil(cells) - (i * total).div_ceil(cells)
}

/// Writes `samples` --- one byte a pixel, row major, 255 the brightest --- to
/// `path` as an 8-bit greyscale PNG.
///
/// A canvas is at most [`MAX_PAGE`] cells each way, so unlike
/// [`image`](crate::image)'s writer this neither streams nor filters: the
/// whole raster is deflated in one go behind a filter byte of 0 per row, which
/// for a picture this size is bytes nobody will miss.  The chunk machinery ---
/// length, type, CRC --- is the whole of what PNG asks for.
fn write_png_gray8(path: &str, width: usize, height: usize, samples: &[u8]) -> io::Result<()> {
    debug_assert_eq!(samples.len(), width * height);

    let mut raw = Vec::with_capacity(samples.len() + height);
    for row in samples.chunks(width) {
        raw.push(0u8); // filter: none
        raw.extend_from_slice(row);
    }
    let mut deflate = ZlibEncoder::new(Vec::new(), Compression::default());
    deflate.write_all(&raw)?;
    let idat = deflate.finish()?;

    let chunk = |out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]| {
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(kind);
        out.extend_from_slice(body);
        let mut crc = Crc::new();
        crc.update(kind);
        crc.update(body);
        out.extend_from_slice(&crc.sum().to_be_bytes());
    };

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    // Bit depth 8, colour type 0 (greyscale), deflate, no filter set, no
    // interlace.
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);

    let mut out = Vec::with_capacity(idat.len() + 128);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &idat);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, &out)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Every deflated stream in `bytes`, inflated and run together as text.
    ///
    /// The one place a PDF says how big its page is is `/MediaBox`, and cairo
    /// writes the page dictionary into a compressed `/ObjStm`: it is not there
    /// to be read off the file.  Streams that are not text — the picture itself,
    /// most of all — fail to come back as UTF-8 and are passed over, which is
    /// the filter as well as the decoder.
    ///
    /// `window`'s tests ask the same question of the page `e` writes, which is
    /// why this is not private to this one.
    pub fn inflated(bytes: &[u8]) -> String {
        use flate2::read::ZlibDecoder;
        use std::io::Read;

        fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
            hay.windows(needle.len()).position(|w| w == needle)
        }

        let mut out = String::new();
        let mut at = 0;
        while let Some(found) = find(&bytes[at..], b"stream") {
            // The keyword is followed by an end-of-line, which PDF allows to be
            // either a newline or a carriage return and one.
            let head = at + found + b"stream".len();
            let start = match bytes.get(head) {
                Some(b'\r') => head + 2,
                _ => head + 1,
            };
            let Some(found) = find(&bytes[start..], b"endstream") else {
                break;
            };
            let end = start + found;
            let mut text = String::new();
            if ZlibDecoder::new(&bytes[start..end])
                .read_to_string(&mut text)
                .is_ok()
            {
                out.push_str(&text);
            }
            // Past the closing keyword, whose own `stream` would otherwise be
            // the next thing found.
            at = end + b"endstream".len();
        }
        out
    }

    /// The spans of a row of cells are the whole picture, once each: no column
    /// counted twice and none left out, whether or not the one divides the
    /// other.
    #[test]
    fn the_spans_of_a_canvas_partition_the_picture() {
        for total in 1..40usize {
            for cells in 1..=total {
                let spans: Vec<usize> = (0..cells).map(|i| span(total, cells, i)).collect();
                assert_eq!(spans.iter().sum::<usize>(), total, "{total} over {cells}");
                assert!(spans.iter().all(|&n| n > 0), "{total} over {cells}");
                // And they are the cells the placement actually sends things to.
                let mut counted = vec![0usize; cells];
                for thing in 0..total {
                    counted[thing * cells / total] += 1;
                }
                assert_eq!(counted, spans, "{total} over {cells}");
            }
        }
    }

    /// The two ends of the curve are not curved: paper is paper and a cell every
    /// pixel of which is inked is the ink itself.  Between them it climbs and
    /// never falls, and lifts the low end -- a cell one percent covered is a
    /// visible grey rather than a rounding error.
    #[test]
    fn shading_runs_from_paper_to_ink_and_lifts_the_low_end() {
        assert_eq!(shade(0.0, 400), 0);
        assert_eq!(shade(400.0, 400), 255);
        assert_eq!(shade(4.0, 400), 31); // one percent of the cell, twelve of the ink

        let mut last = 0;
        for count in 0..=400u32 {
            let now = shade(count as f32, 400);
            assert!(now >= last, "{count} of 400 shaded {now} after {last}");
            last = now;
        }

        // Half the ink in every pixel of a cell is the same shade as all of it
        // in half of them: a cell adds ink up and does not care how it arrived.
        assert_eq!(shade(200.0, 400), shade(0.5 * 400.0, 400));
    }

    fn scratch(name: &str) -> String {
        let mut path = std::env::temp_dir();
        path.push(format!("coloring-bt-page-{}-{}.pdf", std::process::id(), name));
        path.to_string_lossy().into_owned()
    }

    /// A path for a writer that is never going to be finished.  Nothing is
    /// created before `finish`, so the tests about the counts touch no disk at
    /// all -- but a `Writer` still has to be told where it would have gone.
    const UNWRITTEN: &str = "/dev/null/never-opened.pdf";

    /// Draw `colors` -- one per transaction -- and hand back the writer's counts
    /// before they are shaded, which is the part worth asserting about.
    fn drawn(width: usize, rows: usize, bin: usize, page: usize, colors: &[&[usize]]) -> Writer {
        let colors: Vec<Vec<(usize, f64)>> = colors
            .iter()
            .map(|color| color.iter().map(|&block| (block, 1.0)).collect())
            .collect();
        let borrowed: Vec<&[(usize, f64)]> = colors.iter().map(|c| c.as_slice()).collect();
        weighed(width, rows, bin, page, &borrowed)
    }

    /// The same, with a weight on every term rather than the 1 an unweighted
    /// colour carries.
    fn weighed(
        width: usize,
        rows: usize,
        bin: usize,
        page: usize,
        colors: &[&[(usize, f64)]],
    ) -> Writer {
        let mut w = Writer::new(UNWRITTEN, width, rows, bin, page).unwrap();
        for color in colors {
            for &(block, weight) in *color {
                w.set(block, weight);
            }
            w.end_transaction().unwrap();
        }
        w
    }

    /// Eight transactions, the `n`th reaching block `n` and nothing else: a
    /// diagonal, which is the smallest picture whose folding one can read off by
    /// eye.
    fn diagonal(page: usize) -> Writer {
        let rows: Vec<Vec<usize>> = (0..8).map(|r| vec![r]).collect();
        let colors: Vec<&[usize]> = rows.iter().map(|r| r.as_slice()).collect();
        drawn(8, 8, 1, page, &colors)
    }

    /// A canvas no smaller than the picture is the picture: one cell a pixel,
    /// and a cell is inked or it is not.
    #[test]
    fn a_page_larger_than_the_picture_folds_nothing() {
        let w = drawn(4, 3, 1, 64, &[&[0, 3], &[], &[1]]);
        assert_eq!(w.canvas(), (4, 3));
        assert_eq!(
            w.counts,
            vec![1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0]
        );
    }

    /// Fold four columns into two and the cell counts what fell in it.
    #[test]
    fn a_cell_counts_the_pixels_it_covers() {
        let w = drawn(4, 1, 1, 2, &[&[0, 1, 3]]);
        assert_eq!(w.canvas(), (2, 1));
        assert_eq!(w.counts, vec![2.0, 1.0]);
    }

    /// Rows fold the same way columns do.
    #[test]
    fn a_cell_covers_a_span_of_rows_too() {
        let w = drawn(2, 4, 1, 2, &[&[0], &[0], &[1], &[]]);
        assert_eq!(w.canvas(), (2, 2));
        assert_eq!(w.counts, vec![2.0, 0.0, 0.0, 1.0]);
    }

    /// A row is the union of its bin, so the same block reached twice in one row
    /// is one inked pixel and one count -- exactly what the PNG would draw.
    #[test]
    fn a_block_two_transactions_of_a_bin_share_is_counted_once() {
        let w = drawn(2, 1, 2, 64, &[&[0, 1], &[0]]);
        assert_eq!(w.counts, vec![1.0, 1.0]);
    }

    /// The same block in two *rows* is two pixels, and a cell covering both
    /// counts both: rows do not union, they stack.
    #[test]
    fn the_same_block_in_two_rows_is_counted_twice() {
        let w = drawn(1, 2, 1, 1, &[&[0], &[0]]);
        assert_eq!(w.canvas(), (1, 1));
        assert_eq!(w.counts, vec![2.0]);
    }

    /// A row does not inherit the one before it.
    #[test]
    fn a_row_does_not_inherit_the_one_before_it() {
        let w = drawn(2, 2, 1, 64, &[&[0, 1], &[]]);
        assert_eq!(w.counts, vec![1.0, 1.0, 0.0, 0.0]);
    }

    /// A block past the last column is refused, and the message names it.
    #[test]
    fn a_block_past_the_last_column_is_an_error_naming_it() {
        let mut w = Writer::new(UNWRITTEN, 2, 1, 1, 64).unwrap();
        w.set(7, 1.0);
        let e = w.end_transaction().unwrap_err();
        assert!(e.to_string().contains("block 7"), "{e}");
        assert!(e.to_string().contains("--blocks 8"), "{e}");
    }

    /// More records than the picture was opened for is a disagreement between
    /// the two passes, not a picture.
    #[test]
    fn a_record_past_the_last_row_is_refused() {
        let mut w = Writer::new(UNWRITTEN, 1, 1, 1, 64).unwrap();
        w.end_transaction().unwrap();
        assert!(w.end_transaction().is_err());
    }

    /// The page is written, is a PDF, and is the canvas's size in points.
    #[cfg(feature = "pdf")]
    #[test]
    fn the_page_is_a_pdf_of_the_canvas() {
        let path = scratch("page");
        let mut w = diagonal(4);
        w.path = path.clone();
        w.finish().unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
        assert!(
            inflated(&bytes).contains("/MediaBox [ 0 0 4 4 ]"),
            "not a 4 by 4 page"
        );
        std::fs::remove_file(&path).ok();
    }

    /// The same canvas as a PNG: decodes to the canvas's size, with paper
    /// white, ink black, and the diagonal where the fold put it --- checked
    /// against a decoder that shares no code with the writer.
    #[test]
    fn the_fold_can_be_a_png() {
        let path = scratch("fold");
        let mut w = diagonal(4);
        w.path = path.clone();
        w.finish_png().unwrap();

        let decoder =
            png::Decoder::new(io::BufReader::new(std::fs::File::open(&path).unwrap()));
        let mut reader = decoder.read_info().unwrap();
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut buf).unwrap();
        assert_eq!((info.width, info.height), (4, 4));
        assert_eq!(info.color_type, png::ColorType::Grayscale);
        assert_eq!(info.bit_depth, png::BitDepth::Eight);

        let samples = &buf[..info.buffer_size()];
        // `diagonal` is an 8 x 8 picture on a 4-cell canvas: each diagonal
        // cell covers two inked pixels of its four, so its sample is half
        // coverage through the one shade curve, and everything else is paper.
        let half = 255 - image::shade(0.5);
        for cy in 0..4 {
            for cx in 0..4 {
                let want = if cx == cy { half } else { 255 };
                assert_eq!(samples[cy * 4 + cx], want, "cell ({cx}, {cy})");
            }
        }
        std::fs::remove_file(&path).ok();
    }

    /// A cell adds up the ink of the pixels it covers, and under weights a pixel
    /// is worth its weight rather than a whole one.
    #[test]
    fn a_cell_adds_up_the_weights_of_the_pixels_it_covers() {
        let w = weighed(4, 1, 1, 2, &[&[(0, 0.5), (1, 0.25), (3, 1.0)]]);
        assert_eq!(w.canvas(), (2, 1));
        assert_eq!(w.counts, vec![0.75, 1.0]);
    }

    /// The union of two weighted pixels is the darker of them, whichever order
    /// the bin's transactions arrive in -- and the cell holds that one, not the
    /// sum of the two.
    #[test]
    fn a_block_two_transactions_of_a_bin_share_keeps_the_heavier_weight() {
        let up = weighed(1, 1, 2, 64, &[&[(0, 0.25)], &[(0, 0.75)]]);
        let down = weighed(1, 1, 2, 64, &[&[(0, 0.75)], &[(0, 0.25)]]);
        assert_eq!(up.counts, vec![0.75]);
        assert_eq!(down.counts, vec![0.75]);
    }

    /// And a heavier weight in the *next* row is a second pixel, not a heavier
    /// one: the union is within a bin and stops at its edge.
    #[test]
    fn weights_in_two_rows_add_rather_than_take_the_larger() {
        let w = weighed(1, 2, 1, 64, &[&[(0, 0.25)], &[(0, 0.75)]]);
        assert_eq!(w.canvas(), (1, 2));
        assert_eq!(w.counts, vec![0.25, 0.75]);
    }

    /// Ink darkens a cell and paper leaves it alone: an eight by eight diagonal
    /// folded in half is a four by four diagonal, with nothing off it.
    #[test]
    fn a_cell_with_no_ink_is_left_as_paper() {
        let w = diagonal(4);
        assert_eq!(w.canvas(), (4, 4));
        for cy in 0..4 {
            for cx in 0..4 {
                let count = w.counts[cy * 4 + cx];
                assert_eq!(count > 0.0, cx == cy, "cell ({cx}, {cy}) counted {count}");
            }
        }
    }
}
