//! # Colors as a picture
//!
//! The text output names every block in every color, which on a real run is
//! tens of gigabytes of `(block . 1)` pairs.  The same answer drawn instead of
//! spelled: one **row** per transaction, in the order the transactions arrive,
//! one **column** per block id, counting up from 0, and an inked pixel wherever
//! that block is in that transaction's color.  A color of a thousand blocks is a
//! thousand bits rather than fourteen thousand bytes, and how far back a
//! transaction's coins reach is something you can then look at.
//!
//! A pixel carries the coefficient too, which is to say it carries whatever the
//! backend had to say about the block:
//!
//! - unweighted, every coefficient is 1 — the color *is* its set of blocks — so
//!   a pixel is black where the block is in the color and white where it is not,
//!   and that is the whole answer.
//! - under `--weighted` a coefficient is *how much* of the transaction's value
//!   came through that block, so a pixel is a grey between the two: [`shade`]
//!   turns the weight into it, black at 1 and white at 0.
//!
//! [`Ink`] is which of the two a picture is drawn in, settled when it is opened
//! because it is the depth of every sample in the file.
//!
//! ## A greyscale PNG, one or eight bits a sample
//!
//! One channel, and a sample of 0 is black while the largest it can be is white.
//! At one bit a sample that is [`INK`] and [`PAPER`], the whole of an unweighted
//! answer, and "the ones in the output" and "the black pixels" stay the same
//! statement.  At eight it is [`INK`] and [`PAPER_8`] with 254 greys between
//! them, which is what a weight needs and eight times the file.
//!
//! Lossless, and not as a preference.  A pixel here is a whole fact — these
//! coins did or did not come through that block — so there is no approximation
//! of one that is still an answer, and at one bit a sample there is nothing to
//! approximate with in any case.  With PNG that costs nothing to arrange:
//! deflate is the only compression the format has, and it gives back what it was
//! given.  `the_picture_round_trips_losslessly` asserts that against a decoder
//! this file shares no code with, rather than taking the format's word for it.
//!
//! ## Why this and not a wavelet codec
//!
//! This drew a lossless bilevel JPEG 2000 first, for the reduced resolutions
//! such a file carries, and what it wrote was a picture almost nothing would
//! open.  At one bit a sample the codestream says `Ssiz = 0`, and while OpenJPEG
//! reads that back happily — which is why the round-trip test passed all along —
//! macOS's ImageIO refuses the file outright, `sips` and Preview with it.
//! OpenJPEG's own `opj_compress` will not even produce one: hand it a netpbm
//! with a maxval of 1 and it promotes the samples to eight bits on the way in.
//! One bit a sample is a corner of that standard the readers did not follow it
//! into.
//!
//! Raising the precision to something they do take costs what the wavelet costs.
//! The reversible 5/3 filter scales its coefficients with the range of the
//! samples, so each extra bit of precision is roughly another bitplane for EBCOT
//! to code — over the paper as much as over the ink, and the paper is most of
//! it.  On a synthetic run of 42,000 records over 2,000 blocks — 422 pixels in a
//! thousand inked — the same picture came to
//!
//! ```text
//!     packed raster              10.5 MB
//!     JPEG 2000, 1 bit a sample   667 kB   and no common reader opens it
//!     JPEG 2000, 2 bits a sample  2.0 MB
//!     JPEG 2000, 8 bits a sample  8.2 MB
//!     this, one bit a sample      309 kB
//! ```
//!
//! so the encoding that could not be read was not the small one either.  A row
//! here is a stretch of ink and then the white past the block its transaction
//! was mined in, and a row resembles the row above it; that is the shape LZ77
//! and Huffman are good at, and they are good at it without spending a bitplane
//! on the paper.
//!
//! What this gives up is the one thing the wavelet was here for.  A JPEG 2000
//! holds reduced resolutions in the file, so a viewer can show a picture a
//! hundred thousand rows tall at 1/32 scale without decoding the full size; a
//! PNG has no such thing, and whatever opens one of these decodes all of it.
//! That is a real loss, taken deliberately: a picture a viewer can open at one
//! scale beats a picture it cannot open at six.
//!
//! ## Folding transactions together
//!
//! A row per transaction is a row per record, and a real run has more records
//! than any screen has pixels — a million rows is already a picture nothing will
//! show you whole.  So a row may stand for more than one transaction:
//! [`Writer::new`]'s `bin` is how many consecutive transactions share a row, and
//! the row they share is the union of their colors, inked where *any* of them
//! reaches that block — and, where more than one of them reaches it, inked the
//! darker of the shades, which is that union again once a pixel is a weight
//! rather than a bit.  Union is what the driver is computing anyway, so a binned
//! picture is the same picture at a coarser scale rather than a sampled or
//! averaged one, and nothing that was inked can go lighter.  The last bin is
//! whatever is left over, and it is drawn like any other.
//!
//! ## Why both dimensions are settled before the first row
//!
//! A PNG states its size in `IHDR`, in the twenty-five bytes that come before
//! the first scanline, so neither number can be discovered along the way.  The
//! width is the number of blocks and the height is the number of records divided
//! by the bin, and the driver reads the records once to count both before it
//! colors any of them — see `main`'s `survey`.  That is why a picture wants an
//! input it can rewind and a pipe will not do, and why `--blocks <n>`, which
//! still overrides the width, does not excuse that pass: the height needs it
//! too.
//!
//! `IHDR` does sit at a fixed offset, so a writer willing to seek could stamp
//! the height in at the end and let the records arrive down a pipe.  This one
//! does not, and what that would cost is the reason: a height counted in front
//! of the picture is a height the rows are then *checked against*.
//! [`Writer::end_transaction`] refuses a row past the last, and
//! [`Writer::finish`] pads the picture out with blank rows if the records ran
//! out early — so a run whose two passes disagree is an error rather than a
//! picture quietly of the wrong shape.
//!
//! ## One scanline at a time
//!
//! A PNG's pixels are a single deflate stream over the scanlines, each preceded
//! by the byte naming the filter it was written with, and deflate takes them as
//! they come.  So a row is packed into the writer's `row` as it is drawn, handed
//! to the compressor when it closes, and the compressed bytes go out as an
//! `IDAT` every [`IDAT`] of them.  The working set is one scanline and one
//! chunk's worth of output — `width / 8 + 1 MiB` bytes, or `width + 1 MiB` for a
//! weighted picture, however tall the picture — so a drawing far past the size
//! of memory is still written a megabyte at a time.
//!
//! That row buffer holds the scanline itself rather than something to be turned
//! into one.  Paper is the all-ones byte at either depth — a byte of eight
//! [`PAPER`] bits, or one [`PAPER_8`] sample — so a row starts as all 1s and
//! [`Writer::set`] only ever darkens it; the bytes the compressor sees are the
//! bytes `set` wrote, and nothing is unpacked or copied on the way.
//!
//! Every row is filtered with `None`.  PNG's filters predict a byte from its
//! neighbours, and these rows do resemble the row above — but at one bit a
//! sample a row is packed eight pixels to the byte, and subtracting one packed
//! byte from another turns two nearly identical rows into a difference deflate
//! can no longer match against anything.  On the run above, filtering every row
//! with `Up` instead came to 402 kB against `None`'s 309 kB.  A weighted row is
//! a byte a pixel and would take a filter more kindly; it is left on `None` all
//! the same, so that what a scanline is does not depend on what is in it.
//!
//! Nothing here is written twice — unlike the codestream this replaces, which
//! went back to stamp lengths into markers it had already emitted — so the
//! output could be any sink the caller has.  It is a path because that is where
//! the file gets created and nothing has yet wanted otherwise.

use std::fs::File;
use std::io::{self, Write};

use crate::oklch;
#[cfg(test)]
use crate::oklch::Rgb;
use flate2::write::ZlibEncoder;
use flate2::{Compression, Crc};

/// A block that is in the color, drawn as fully as the depth allows: 0, black at
/// either depth.
pub const INK: u8 = 0;

/// One that is not, at one bit a sample: 1, the largest such a sample can be,
/// and so white.
pub const PAPER: u8 = 1;

/// The same at eight bits a sample, where the greys between it and [`INK`] are
/// what a weight is drawn in.
pub const PAPER_8: u8 = 255;

/// The exponent that turns a fraction of ink into a shade: `shade =
/// fraction^(1/TONE)`.
///
/// 2.2 is the usual display gamma, chosen here for what it does to the low end
/// rather than for anything about a monitor — one percent comes out at twelve
/// percent of the ink, a grey one can see, where one percent black is not.
pub const TONE: f64 = 2.2;

/// How much ink `fraction` of it comes to, as a byte: 0 is paper and 255 the
/// full ink.
///
/// The two ends are not curved — none is none and all is all, whatever [`TONE`]
/// is — and in between the curve climbs and never falls, so more ink is always
/// at least as dark.  What it does *not* say is that twice the darkness is twice
/// the ink; read a shaded picture for shape, and the text output for quantities.
///
/// Both things that shade use this: a weighted *pixel*, whose fraction is the
/// weight of that block in that color, and a folded *cell*, whose fraction is
/// how much of the rectangle it covers is inked.  The lift is wanted in both —
/// weights decay by roughly a factor per hop of ancestry, so most of a real
/// color is a fraction of a percent and a linear map would draw it as paper.
pub fn shade(fraction: f64) -> u8 {
    (fraction.clamp(0.0, 1.0).powf(1.0 / TONE) * 255.0).round() as u8
}

/// What a pixel says, which is what the backend behind it had to say about a
/// block — and, since it is the depth of every sample in the file, something a
/// picture is opened with rather than told later.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    /// One bit: the block is in the color or it is not.  Every coefficient of an
    /// unweighted color is 1, so that is the whole term.
    Flat,
    /// Eight bits: how much of the transaction's value came through the block,
    /// [`shade`]d.
    Weighted,
    /// The same eight bits, read through a palette rather than as a grey.
    ///
    /// The sample is exactly [`Ink::Weighted`]'s -- same [`shade`], same
    /// counting up to paper, so same union when two transactions share a row --
    /// and what changes is only that the file says colour type 3 and carries a
    /// `PLTE` chunk, so the reader looks the number up instead of taking it for
    /// a grey.  The picture is the same size to the byte.
    ///
    /// It is worth the chunk because grey has 254 steps and an eye reads some
    /// thirty of them, and the quantity here lives in a fraction of a percent:
    /// see [`crate::oklch`], which builds the ramp and says why it is built in
    /// a perceptual space rather than in HSL.
    Palette,
}

impl Ink {
    /// Bits a sample, which is what `IHDR` is told.
    fn depth(self) -> u8 {
        match self {
            Ink::Flat => 1,
            Ink::Weighted | Ink::Palette => 8,
        }
    }

    /// What `IHDR` calls the pixel: 0 is greyscale, 3 is a palette index.
    ///
    /// A palette makes the file say `PLTE` as well, which is the only other
    /// difference between this ink and [`Ink::Weighted`].
    fn colour_type(self) -> u8 {
        match self {
            Ink::Flat | Ink::Weighted => 0,
            Ink::Palette => 3,
        }
    }

    /// Bytes a scanline of `width` pixels comes to, the filter byte aside.
    fn stride(self, width: usize) -> usize {
        match self {
            Ink::Flat => width.div_ceil(8),
            Ink::Weighted | Ink::Palette => width,
        }
    }
}

/// How much compressed output to gather before it goes out as an `IDAT` chunk.
///
/// A chunk states its length in front of its data, so this is the one thing held
/// back at all: the compressor's output has to be in hand before the chunk it
/// goes in can be started.  It is a working set and nothing else — a PNG is the
/// same picture however its `IDAT`s are divided up — so the number is a megabyte
/// for the reason a buffer usually is.
const IDAT: usize = 1 << 20;

/// The eight bytes every PNG begins with.
///
/// The `\x89` and the `\r\n`, `\x1a`, `\n` are the format's own check that
/// nothing along the way stripped the high bit or rewrote the line endings.
const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// The filter byte in front of every scanline.  See the module docs for why it
/// is always this one.
const NO_FILTER: u8 = 0;

/// A byte of blank paper: eight [`PAPER`] pixels, or one [`PAPER_8`] sample.
/// What a row starts as, what a closed row is blanked back to, and what the
/// slack bits past the last column stay as for the whole picture.
///
/// The two depths agree on it, which is why a row is blanked the same way in
/// both.
const BLANK: u8 = 0xff;

/// [`Writer::set`] darkens a pixel -- it clears a bit, or lowers a sample --
/// and a row is blanked back to [`BLANK`], which is paper at either depth only
/// for as long as these are the three numbers they are.
const _: () = assert!(INK == 0 && PAPER == 1 && PAPER_8 == BLANK);

/// A picture being drawn a transaction at a time.
///
/// A transaction is drawn by [`Writer::set`], once per block in its color, and
/// closed by [`Writer::end_transaction`]; every `bin` of those makes a row, and
/// a row that closes goes to the compressor.
pub struct Writer {
    /// The file, from the signature to `IEND`, written straight through.
    out: File,
    /// The deflate stream the scanlines go into.  Its sink gathers compressed
    /// bytes until there are [`IDAT`] of them, at which point [`Writer::drain`]
    /// empties it into a chunk.
    zip: ZlibEncoder<Vec<u8>>,
    /// Columns, i.e. how many block ids the picture has room for.
    width: usize,
    /// Rows, as promised in `IHDR` before the first scanline.
    height: usize,
    /// Transactions to a row.  One is a row each, which is the plain picture.
    bin: usize,
    /// What a pixel says, and so how many bits one is.
    ink: Ink,
    /// The row being drawn, as the compressor will take it: `row[0]` is
    /// [`NO_FILTER`] and never changes, and `row[1..]` is the samples as the
    /// file holds them — packed eight to the byte with the leftmost pixel in the
    /// most significant bit under [`Ink::Flat`], one byte each under
    /// [`Ink::Weighted`].  All 1s outside `..dirty` *bytes* of them at all
    /// times, which is the invariant that lets a closed row be blanked by
    /// clearing a prefix.
    row: Vec<u8>,
    dirty: usize,
    /// Transactions drawn into the row being built, always below `bin`.
    pending: usize,
    /// Rows finished so far, at most `height`.
    rows: usize,
    /// The first block id seen that the picture has no column for, if any.
    escaped: Option<usize>,
}

impl Writer {
    /// Open `path` for a picture `width` columns by `height` rows, with `bin`
    /// transactions to the row and `ink` in its pixels, and write everything up
    /// to the first scanline.
    ///
    /// All of that is final here: see the module docs for why a PNG cannot be
    /// told its size later, and `IHDR` names the depth in the same breath.
    pub fn new(path: &str, width: usize, height: usize, bin: usize, ink: Ink) -> io::Result<Self> {
        assert!(bin > 0, "a row has to stand for at least one transaction");
        if width == 0 || height == 0 {
            return Err(io::Error::other(
                "an image needs a column and a row, and this one has none",
            ));
        }
        // `IHDR` has four bytes for each of them, and a picture that does not
        // fit in those is not one this format can state the size of at all.
        if width > u32::MAX as usize || height > u32::MAX as usize {
            return Err(io::Error::other(format!(
                "a {} x {} picture is larger than a PNG can say it is",
                width, height
            )));
        }

        let mut out = File::create(path)?;
        out.write_all(&SIGNATURE)?;

        let mut ihdr = [0u8; 13];
        ihdr[..4].copy_from_slice(&(width as u32).to_be_bytes());
        ihdr[4..8].copy_from_slice(&(height as u32).to_be_bytes());
        ihdr[8] = ink.depth(); // one bit a sample, or eight for a weighted one
        ihdr[9] = ink.colour_type(); // greyscale, or an index into `PLTE`
        ihdr[10] = 0; // deflate, which is the only compression PNG has
        ihdr[11] = 0; // the filtering PNG always uses; `NO_FILTER` is per row
        ihdr[12] = 0; // not interlaced
        chunk(&mut out, b"IHDR", &ihdr)?;

        // `PLTE` is required for colour type 3 and has to come before the first
        // `IDAT`.  One entry a sample, so the indices the rows already carry
        // need no translating.
        if ink == Ink::Palette {
            let mut plte = Vec::with_capacity(3 * oklch::RAMP_LEN);
            for entry in oklch::ramp() {
                plte.extend_from_slice(&entry);
            }
            chunk(&mut out, b"PLTE", &plte)?;
        }

        let stride = ink.stride(width);
        let mut row = vec![BLANK; 1 + stride];
        row[0] = NO_FILTER;

        Ok(Writer {
            out,
            zip: ZlibEncoder::new(Vec::with_capacity(IDAT), Compression::best()),
            width,
            height,
            bin,
            ink,
            row,
            dirty: 0,
            pending: 0,
            rows: 0,
            escaped: None,
        })
    }

    /// Ink the pixel for `block` in the row being built, with `weight` as its
    /// coefficient.
    ///
    /// The weight is what the term said and nothing else is done to it here:
    /// under [`Ink::Flat`] it is 1 for every term there is, so the pixel is
    /// simply black, and under [`Ink::Weighted`] it is the share of the
    /// transaction's value that came through the block and [`shade`] is the grey
    /// it comes to.
    ///
    /// A pixel a bin has already drawn keeps the darker of the two, which is
    /// what makes a row the union of its transactions at both depths.
    ///
    /// A block the picture has no column for is remembered rather than reported:
    /// this runs inside the store's walk over the color's terms, which has
    /// nowhere to put an error.  [`Writer::end_transaction`] raises it, naming
    /// the first one — the first is the informative one, since the rest are
    /// whatever the run went on to see afterwards.
    #[inline]
    pub fn set(&mut self, block: usize, weight: f64) {
        if block >= self.width {
            self.escaped.get_or_insert(block);
            return;
        }
        let byte = match self.ink {
            Ink::Flat => {
                let byte = block / 8;
                // Leftmost pixel in the most significant bit, so that block ids
                // read left to right across the row -- and cleared rather than
                // set, because ink is the 0 of the two.  Clearing a bit that is
                // already clear is the union, at no cost.
                self.row[1 + byte] &= !(0x80 >> (block % 8));
                byte
            }
            // The palette's sample is the weighted one: the ramp is ordered so
            // that a smaller index is heavier ink, exactly as a smaller grey is,
            // which is what lets the two share this arithmetic and the `min`
            // that makes a binned row the union of its transactions.
            Ink::Weighted | Ink::Palette => {
                // A sample counts *up* to white, so the darker of two is the
                // smaller, and the row keeps that one.
                let sample = PAPER_8 - shade(weight);
                let cell = &mut self.row[1 + block];
                *cell = (*cell).min(sample);
                block
            }
        };
        if byte >= self.dirty {
            self.dirty = byte + 1;
        }
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
            // Nothing is blanked until the row closes, so a bin's transactions
            // have been drawing into the same one all along: the row is their
            // union without any of them being asked to compute one.
            self.end_row()?;
        }
        Ok(())
    }

    /// Hand the row being built to the compressor, blank it, and start the next.
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
        self.zip.write_all(&self.row)?;
        // Samples are only ever darkened below `dirty`, so that prefix is the
        // only part of the row that can have gone dark and the rest is still the
        // paper it started as.  Colors are sets of *ancestor* blocks and the
        // chain only grows, so early rows reach nowhere near the right-hand edge
        // and this is the difference between blanking a prefix and blanking the
        // width.
        self.row[1..1 + self.dirty].fill(BLANK);
        self.dirty = 0;
        self.pending = 0;
        self.rows += 1;
        self.drain()
    }

    /// Send what the compressor has produced, if there is a chunk's worth of it.
    ///
    /// A chunk needs its length in front of it, which is what holds any of the
    /// output back at all; the tail below [`IDAT`] is left for
    /// [`Writer::finish`], which has the end of the deflate stream to add to it
    /// anyway.
    fn drain(&mut self) -> io::Result<()> {
        if self.zip.get_ref().len() < IDAT {
            return Ok(());
        }
        chunk(&mut self.out, b"IDAT", self.zip.get_ref())?;
        // Emptied rather than replaced: the capacity is the buffer.
        self.zip.get_mut().clear();
        Ok(())
    }

    /// `(columns, rows)` as the picture was opened for, which is what it will be
    /// however many records turn up.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Close the picture: finish the row in hand, pad out to the promised
    /// height, end the deflate stream and write `IEND`.
    pub fn finish(mut self) -> io::Result<()> {
        // A bin that never filled up is still a row: the run does not owe the
        // picture a whole bin's worth of transactions at the end of the records.
        if self.pending > 0 {
            self.end_row()?;
        }
        // The height went into `IHDR` before the first scanline, so the picture
        // owes exactly that many rows.  Short of them the file is a truncated
        // image every reader complains about; blank rows are at least an honest
        // picture of records that were not there.
        while self.rows < self.height {
            self.end_row()?;
        }
        // Ending the stream puts deflate's last block and its checksum into the
        // same buffer, on top of the tail the last `drain` left in it, and hands
        // the buffer back.  A zlib stream always ends with something, so this is
        // never the empty chunk the check below would otherwise be about; the
        // check is there because a chunk of nothing is not worth writing.
        let held = self.zip.finish()?;
        if !held.is_empty() {
            chunk(&mut self.out, b"IDAT", &held)?;
        }
        chunk(&mut self.out, b"IEND", &[])?;
        self.out.flush()
    }
}

/// One PNG chunk: its length, its four-byte name, its data, and a CRC-32 over
/// the name and the data — the length deliberately not among them, since it is
/// what a reader has to trust before it can check anything.
fn chunk(out: &mut File, name: &[u8; 4], data: &[u8]) -> io::Result<()> {
    debug_assert!(
        data.len() <= u32::MAX as usize,
        "a chunk states its length in four bytes"
    );
    let mut head = [0u8; 8];
    head[..4].copy_from_slice(&(data.len() as u32).to_be_bytes());
    head[4..].copy_from_slice(name);
    out.write_all(&head)?;
    out.write_all(data)?;

    let mut crc = Crc::new();
    crc.update(name);
    crc.update(data);
    out.write_all(&crc.sum().to_be_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path under the test runner's temporary directory, distinct per test so
    /// that the tests can run in the same directory at the same time.
    fn scratch(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("colors-{}-{name}.png", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    /// The picture at `path`, read back with a decoder this file shares no code
    /// with: its size and its samples, row-major, a byte a pixel — and its depth
    /// checked against the `ink` it was drawn in.
    ///
    /// The bits are unpacked here rather than by asking the decoder to expand
    /// them, so that a sample of [`INK`] means what this file says it means and
    /// the decoder is left to do the inflating and the unfiltering — the two
    /// steps a writer cannot check itself.
    fn decode(path: &str, ink: Ink) -> (usize, usize, Vec<u8>) {
        let file = io::BufReader::new(File::open(path).unwrap());
        let mut reader = png::Decoder::new(file).read_info().expect("a PNG header");
        {
            let info = reader.info();
            let depth = match ink {
                Ink::Flat => png::BitDepth::One,
                Ink::Weighted | Ink::Palette => png::BitDepth::Eight,
            };
            assert_eq!(info.bit_depth, depth, "the depth the picture was drawn at");
            // The ink decides this too: the palette one is the same samples
            // read through `PLTE`, so a decoder has to see colour type 3 there
            // and a plain greyscale channel everywhere else.
            let (colour, what) = match ink {
                Ink::Flat | Ink::Weighted => (png::ColorType::Grayscale, "one greyscale channel"),
                Ink::Palette => (png::ColorType::Indexed, "an index into PLTE"),
            };
            assert_eq!(info.color_type, colour, "{}", what);
        }
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut buf).unwrap();
        let (w, h) = (frame.width as usize, frame.height as usize);
        let mut samples = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = &buf[y * frame.line_size..(y + 1) * frame.line_size];
            for x in 0..w {
                samples.push(match ink {
                    Ink::Flat => (row[x / 8] >> (7 - x % 8)) & 1,
                    Ink::Weighted | Ink::Palette => row[x],
                });
            }
        }
        (w, h, samples)
    }

    /// Draw `colors` with one transaction to a row and answer the rows the file
    /// decodes back to, as `#` for ink and `.` for paper.
    fn draw(name: &str, width: usize, rows: usize, colors: &[&[usize]]) -> Vec<String> {
        binned(name, width, rows, 1, colors)
    }

    /// The same, with `bin` transactions to the row.
    fn binned(
        name: &str,
        width: usize,
        rows: usize,
        bin: usize,
        colors: &[&[usize]],
    ) -> Vec<String> {
        let path = scratch(name);
        write(&path, width, rows, bin, colors).unwrap();
        let picture = rendered(&path, width, rows);
        std::fs::remove_file(&path).ok();
        picture
    }

    /// Draw `colors` -- a `(block, weight)` a term, `bin` transactions to a row
    /// -- as a weighted picture, and answer its samples row-major.
    fn weighted(
        name: &str,
        width: usize,
        rows: usize,
        bin: usize,
        colors: &[&[(usize, f64)]],
    ) -> Vec<Vec<u8>> {
        let path = scratch(name);
        {
            let mut w = Writer::new(&path, width, rows, bin, Ink::Weighted).unwrap();
            for color in colors {
                for &(block, weight) in *color {
                    w.set(block, weight);
                }
                w.end_transaction().unwrap();
            }
            w.finish().unwrap();
        }
        let (cols, got, samples) = decode(&path, Ink::Weighted);
        assert_eq!((cols, got), (width, rows), "the size it was opened for");
        std::fs::remove_file(&path).ok();
        samples.chunks(width).map(|row| row.to_vec()).collect()
    }

    /// Draw the colors into a file, leaving whatever error came of it to the
    /// caller — the tests that expect one need the writer's own message.
    fn write(
        path: &str,
        width: usize,
        rows: usize,
        bin: usize,
        colors: &[&[usize]],
    ) -> io::Result<()> {
        let mut w = Writer::new(path, width, rows, bin, Ink::Flat)?;
        for color in colors {
            for &block in *color {
                w.set(block, 1.0);
            }
            w.end_transaction()?;
        }
        w.finish()
    }

    /// The decoded file as one string a row, checked against the size it was
    /// asked for.
    fn rendered(path: &str, width: usize, rows: usize) -> Vec<String> {
        let (w, h, samples) = decode(path, Ink::Flat);
        assert_eq!((w, h), (width, rows), "the size the picture was opened for");
        samples
            .chunks(width)
            .map(|row| {
                row.iter()
                    .map(|&s| if s == INK { '#' } else { '.' })
                    .collect()
            })
            .collect()
    }

    /// Column 0 is the leftmost pixel and the top bit of byte 0: block ids read
    /// left to right across the row.
    #[test]
    fn the_lowest_block_is_the_leftmost_pixel() {
        assert_eq!(
            draw("leftmost", 16, 4, &[&[0], &[7], &[8], &[15]]),
            [
                "#...............",
                ".......#........",
                "........#.......",
                "...............#",
            ]
        );
    }

    /// Rows are blanked by a prefix, so the case that would catch a wrong prefix
    /// is a wide row followed by a narrow one.
    #[test]
    fn a_row_does_not_inherit_the_one_before_it() {
        assert_eq!(
            draw("inherit", 24, 3, &[&[0, 23], &[1], &[]]),
            [
                "#......................#",
                ".#......................",
                "........................",
            ]
        );
    }

    /// A width that is not a multiple of eight leaves slack bits at the end of
    /// each packed row, and they have to stay out of the picture.
    #[test]
    fn the_padding_bits_of_a_row_are_not_pixels() {
        assert_eq!(draw("padding", 3, 1, &[&[0, 1, 2]]), ["###"]);
    }

    /// A bin's row is the union of its transactions' colors, which is the one
    /// thing that makes a compacted picture a coarser picture rather than a
    /// sampled one: a block inked at bin `n` is inked at every coarser bin too.
    #[test]
    fn a_binned_row_is_the_union_of_the_transactions_in_it() {
        assert_eq!(
            binned("union", 24, 2, 3, &[&[0], &[9], &[23], &[1], &[2], &[3]]),
            ["#........#.............#", ".###...................."]
        );
    }

    /// The records do not owe the picture a whole bin at the end of the run.
    #[test]
    fn a_bin_left_part_full_is_still_a_row() {
        assert_eq!(
            binned("part-full", 8, 2, 4, &[&[0], &[1], &[2], &[3], &[6], &[7]]),
            ["####....", "......##"]
        );
    }

    /// Binning must not turn a picture into a smeared one: what a bin
    /// accumulates has to stop at the bin's edge.
    #[test]
    fn a_bin_does_not_leak_into_the_next_one() {
        assert_eq!(
            binned("leak", 16, 2, 2, &[&[0, 15], &[1], &[8], &[]]),
            ["##.............#", "........#......."]
        );
    }

    /// Binning by one is the plain picture, not a special case of it.
    #[test]
    fn binning_by_one_draws_what_no_binning_draws() {
        let colors: &[&[usize]] = &[&[0, 5], &[3], &[], &[7]];
        assert_eq!(
            binned("bin-one", 8, 4, 1, colors),
            draw("no-bin", 8, 4, colors)
        );
    }

    #[test]
    fn an_empty_color_is_a_blank_row_rather_than_no_row() {
        assert_eq!(draw("blank", 8, 2, &[&[], &[]]), ["........", "........"]);
    }

    /// The picture cannot silently drop a block: one too narrow for the run is a
    /// truncated answer that looks like a whole one.
    #[test]
    fn a_block_past_the_last_column_is_an_error_naming_it() {
        let path = scratch("narrow");
        let mut w = Writer::new(&path, 10, 1, 1, Ink::Flat).unwrap();
        w.set(3, 1.0);
        w.set(64, 1.0);
        w.set(70, 1.0);
        let e = w.end_transaction().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        // The first one over the edge, not the largest and not the last.
        assert!(e.to_string().contains("block 64"), "{}", e);
        assert!(e.to_string().contains("--blocks 65"), "{}", e);
        drop(w);
        std::fs::remove_file(&path).ok();
    }

    /// The height is a promise made in `IHDR`, so a record past the last row is
    /// refused rather than quietly dropped or half-written.
    #[test]
    fn a_record_past_the_last_row_is_refused() {
        let path = scratch("too-tall");
        let e = write(&path, 8, 2, 1, &[&[0], &[1], &[2]]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("2 rows tall"), "{}", e);
        std::fs::remove_file(&path).ok();
    }

    /// And short of the promise the picture is padded rather than left a
    /// truncated image.
    #[test]
    fn records_that_run_out_early_leave_blank_rows() {
        assert_eq!(
            draw("short", 8, 3, &[&[1]]),
            [".#......", "........", "........"]
        );
    }

    /// A picture too wide for a `u32` cannot be described, and is refused rather
    /// than silently truncated into one that can be.
    ///
    /// Only where a `usize` is wider than the four bytes `IHDR` gives it, which
    /// is the only place the check can fire.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn a_picture_past_what_the_header_can_say_is_refused() {
        let path = scratch("enormous");
        // `Writer` is not `Debug`, so the `Ok` is named rather than unwrapped.
        let Err(e) = Writer::new(&path, 1usize << 33, 1, 1, Ink::Flat) else {
            panic!("a picture 2^33 columns wide was opened");
        };
        assert!(e.to_string().contains("larger than a PNG can say"), "{}", e);
        std::fs::remove_file(&path).ok();
    }

    /// The claim the whole file rests on: what the writer inked is what a
    /// decoder gives back, pixel for pixel.
    #[test]
    fn the_picture_round_trips_losslessly() {
        // Deliberately awkward: a width that is not a multiple of eight, colors
        // that reach both edges, empty ones, and a lone pixel in the middle of
        // the paper.
        let colors: &[&[usize]] = &[
            &[0, 1, 2, 9],
            &[],
            &[19],
            &[0, 19],
            &[3, 4, 5, 6, 7, 8, 9, 10],
            &[0, 2, 4, 6, 8, 10, 12, 14, 16, 18],
            &[10],
        ];
        let width = 21;
        let picture = draw("lossless", width, colors.len(), colors);

        let wanted: Vec<String> = colors
            .iter()
            .map(|color| {
                (0..width)
                    .map(|block| if color.contains(&block) { '#' } else { '.' })
                    .collect()
            })
            .collect();
        assert_eq!(picture, wanted, "every pixel survived");
    }

    /// The curve is the two ends and everything between them: paper is paper,
    /// the whole of the ink is the ink, and a fraction too small to see linearly
    /// is lifted into a grey that can be.
    #[test]
    fn a_shade_runs_from_paper_to_ink_and_lifts_the_low_end() {
        assert_eq!(shade(0.0), 0);
        assert_eq!(shade(1.0), 255);
        assert_eq!(shade(0.01), 31); // one percent of the value, twelve of the ink

        let mut last = 0;
        for step in 0..=1000 {
            let now = shade(step as f64 / 1000.0);
            assert!(now >= last, "{step} per mille shaded {now} after {last}");
            last = now;
        }
        // Nothing outside the unit interval comes out outside the two ends.
        assert_eq!(shade(-1.0), 0);
        assert_eq!(shade(7.5), 255);
    }

    /// A weighted pixel is the shade of its weight: the whole of the value is
    /// black, none of it is paper, and the greys in between order the way the
    /// weights do.
    #[test]
    fn a_weighted_pixel_is_the_shade_of_its_weight() {
        let rows = weighted(
            "weights",
            4,
            2,
            1,
            &[&[(0, 1.0), (1, 0.5), (2, 0.001)], &[(3, 0.0)]],
        );
        assert_eq!(rows[0][0], INK, "the whole of the value is black");
        assert_eq!(rows[0][1], PAPER_8 - shade(0.5));
        assert_eq!(rows[0][2], PAPER_8 - shade(0.001));
        assert_eq!(rows[0][3], PAPER_8, "a block not in the colour is paper");
        // A weight of nothing is paper, and so is a term that was never drawn:
        // the pixel says how much came through the block, and none did.
        assert_eq!(rows[1], vec![PAPER_8; 4]);

        assert!(
            rows[0][0] < rows[0][1] && rows[0][1] < rows[0][2] && rows[0][2] < rows[0][3],
            "heavier weights draw darker: {:?}",
            rows[0]
        );
    }

    /// A row is the union of its bin at eight bits too, and where two
    /// transactions of a bin reach the same block the darker of them stands --
    /// which is the same statement as the bilevel union, where every weight is 1.
    #[test]
    fn a_binned_weighted_row_keeps_the_darker_of_two_weights() {
        let rows = weighted(
            "weighted-union",
            3,
            1,
            2,
            &[&[(0, 0.25), (1, 0.75)], &[(1, 0.25), (2, 0.5)]],
        );
        assert_eq!(
            rows[0],
            vec![
                PAPER_8 - shade(0.25),
                PAPER_8 - shade(0.75),
                PAPER_8 - shade(0.5),
            ]
        );
    }

    /// The claim the weighted picture rests on, asserted the way the bilevel one
    /// is: every sample the writer laid down is the sample a decoder gives back.
    /// The palette picture has to be the weighted one with a `PLTE` chunk
    /// bolted on: same indices, same size, and a decoder that shares no code
    /// with the writer has to agree about both.
    #[test]
    fn a_palette_picture_is_the_weighted_one_plus_its_colours() {
        let weights = [0.0, 0.001, 0.02, 0.25, 0.5, 1.0];
        let path = scratch("palette-round-trip");
        let grey_path = scratch("palette-round-trip-grey");

        for (at, ink) in [(&path, Ink::Palette), (&grey_path, Ink::Weighted)] {
            let mut w = Writer::new(at, weights.len(), 1, 1, ink).unwrap();
            for (block, &weight) in weights.iter().enumerate() {
                w.set(block, weight);
            }
            w.end_transaction().unwrap();
            w.finish().unwrap();
        }

        // The samples are the same numbers under both inks, which is the claim
        // that lets the two share `set` and its `min`.
        let (_, _, palette_samples) = decode(&path, Ink::Palette);
        let (_, _, grey_samples) = decode(&grey_path, Ink::Weighted);
        assert_eq!(
            palette_samples, grey_samples,
            "a palette picture holds exactly the weighted picture's samples"
        );

        // And the file says so: colour type 3, carrying the ramp.
        let file = io::BufReader::new(File::open(&path).expect("the picture is there"));
        let reader = png::Decoder::new(file).read_info().expect("a PNG header");
        let info = reader.info();
        assert_eq!(
            info.color_type,
            png::ColorType::Indexed,
            "a palette picture is colour type 3"
        );
        let plte = info.palette.as_ref().expect("colour type 3 requires PLTE");
        assert_eq!(plte.len(), 3 * oklch::RAMP_LEN, "one RGB entry a sample");
        let ramp = oklch::ramp();
        for (index, entry) in ramp.iter().enumerate() {
            assert_eq!(
                &plte[3 * index..3 * index + 3],
                &entry[..],
                "palette entry {} is not the ramp's",
                index
            );
        }

        // Read back through the palette a viewer would use: no weight is paper,
        // and the heaviest pixel is darker than it.  That is the ordering the
        // `min` in `set` depends on, checked at the far end of the file.
        let paper = ramp[palette_samples[0] as usize];
        let heaviest = ramp[palette_samples[weights.len() - 1] as usize];
        let ink_of = |c: Rgb| c.iter().map(|&v| v as u32).sum::<u32>();
        assert_eq!(paper, ramp[PAPER_8 as usize], "no weight is paper");
        assert!(
            ink_of(heaviest) < ink_of(paper),
            "the heaviest pixel has to be darker than paper: {:?} against {:?}",
            heaviest,
            paper
        );

        std::fs::remove_file(&path).ok();
        std::fs::remove_file(&grey_path).ok();
    }

    #[test]
    fn a_weighted_picture_round_trips_losslessly() {
        let colors: Vec<Vec<(usize, f64)>> = (0..9)
            .map(|r| {
                (0..21)
                    .filter(|b| (b + r) % 3 == 0)
                    .map(|b| (b, (b + 1) as f64 / 21.0))
                    .collect()
            })
            .collect();
        let borrowed: Vec<&[(usize, f64)]> = colors.iter().map(|c| c.as_slice()).collect();
        let rows = weighted("weighted-lossless", 21, colors.len(), 1, &borrowed);

        let wanted: Vec<Vec<u8>> = colors
            .iter()
            .map(|color| {
                (0..21)
                    .map(|b| match color.iter().find(|(block, _)| *block == b) {
                        Some(&(_, w)) => PAPER_8 - shade(w),
                        None => PAPER_8,
                    })
                    .collect()
            })
            .collect();
        assert_eq!(rows, wanted, "every sample survived");
    }

    /// A picture whose scanlines run past one `IDAT`, so that the chunk loop
    /// runs more than once and the seam between two chunks has to be nothing at
    /// all to a reader.
    #[test]
    fn a_picture_of_many_chunks_round_trips() {
        let width = 4096;
        let rows = 16 * 1024;
        // One inked pixel a row, walking across and down, so that a row drawn
        // from the wrong place cannot pass -- and, being a diagonal rather than
        // a repeat, 8 MB of scanlines that do not all deflate to nothing.
        let colors: Vec<Vec<usize>> = (0..rows).map(|r| vec![r % width]).collect();
        let borrowed: Vec<&[usize]> = colors.iter().map(|c| c.as_slice()).collect();

        let picture = binned("chunks", width, rows, 1, &borrowed);
        for (r, row) in picture.iter().enumerate() {
            assert_eq!(row.len(), width);
            assert_eq!(
                row.find('#'),
                Some(r % width),
                "row {} has its pixel in the wrong column",
                r
            );
            assert_eq!(row.matches('#').count(), 1, "row {} has extra ink", r);
        }
    }
}
