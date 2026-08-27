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
//! What the picture drops is the coefficients.  For the unweighted backends
//! there is nothing to drop — every coefficient is 1, so the color *is* its set
//! of blocks and the picture is the whole answer.  Under `--weighted` a pixel
//! says only that some of the transaction's value came from that block, not how
//! much; a weight too small to print as anything but `0.000000` is an inked
//! pixel like any other, which is one thing the picture shows better than the
//! text.
//!
//! ## A bilevel PNG
//!
//! One channel, one bit a sample: [`INK`] where the block is in the color and
//! [`PAPER`] where it is not.  A greyscale sample of 0 is black and the largest
//! it can be is white, so at a depth of one bit those two numbers *are* black on
//! white, and "the ones in the output" and "the black pixels" stay the same
//! statement.
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
//! reaches that block.  Union is what the driver is computing anyway, so a
//! binned picture is the same picture at a coarser scale rather than a sampled
//! or averaged one, and nothing that was inked can go white.  The last bin is
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
//! chunk's worth of output — `width / 8 + 1 MiB` bytes, however tall the picture
//! — so a drawing far past the size of memory is still written a megabyte at a
//! time.
//!
//! That row buffer holds the scanline itself rather than something to be turned
//! into one.  Paper is a 1 and ink a 0, so a row starts as all 1s and
//! [`Writer::set`] *clears* a bit; the bytes the compressor sees are the bytes
//! `set` wrote, and nothing is unpacked or copied on the way.
//!
//! Every row is filtered with `None`.  PNG's filters predict a byte from its
//! neighbours, and these rows do resemble the row above — but at one bit a
//! sample a row is packed eight pixels to the byte, and subtracting one packed
//! byte from another turns two nearly identical rows into a difference deflate
//! can no longer match against anything.  On the run above, filtering every row
//! with `Up` instead came to 402 kB against `None`'s 309 kB.
//!
//! Nothing here is written twice — unlike the codestream this replaces, which
//! went back to stamp lengths into markers it had already emitted — so the
//! output could be any sink the caller has.  It is a path because that is where
//! the file gets created and nothing has yet wanted otherwise.

use std::fs::File;
use std::io::{self, Write};

use flate2::write::ZlibEncoder;
use flate2::{Compression, Crc};

/// A block that is in the color: at one bit a sample, black.
pub const INK: u8 = 0;

/// One that is not: 1, the largest a one-bit sample can be, and so white.
pub const PAPER: u8 = 1;

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

/// A byte of blank paper: eight [`PAPER`] pixels.  What a row starts as, what a
/// closed row is blanked back to, and what the slack bits past the last column
/// stay as for the whole picture.
const BLANK: u8 = if PAPER == 0 { 0x00 } else { 0xff };

/// [`Writer::set`] inks a pixel by *clearing* its bit, and a row is blanked by
/// setting them all -- which is [`INK`] and [`PAPER`] only for as long as they
/// are these two numbers.
const _: () = assert!(INK == 0 && PAPER == 1);

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
    /// The row being drawn, as the compressor will take it: `row[0]` is
    /// [`NO_FILTER`] and never changes, and `row[1..]` is the packed samples,
    /// leftmost pixel in the most significant bit.  All 1s outside `..dirty` of
    /// the samples at all times, which is the invariant that lets a closed row
    /// be blanked by clearing a prefix.
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
    /// transactions to the row, and write everything up to the first scanline.
    ///
    /// Both dimensions are final here: see the module docs for why a PNG cannot
    /// be told either of them later.
    pub fn new(path: &str, width: usize, height: usize, bin: usize) -> io::Result<Self> {
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
        ihdr[8] = 1; // one bit a sample
        ihdr[9] = 0; // greyscale: no palette, no colour, no alpha
        ihdr[10] = 0; // deflate, which is the only compression PNG has
        ihdr[11] = 0; // the filtering PNG always uses; `NO_FILTER` is per row
        ihdr[12] = 0; // not interlaced
        chunk(&mut out, b"IHDR", &ihdr)?;

        let stride = width.div_ceil(8);
        let mut row = vec![BLANK; 1 + stride];
        row[0] = NO_FILTER;

        Ok(Writer {
            out,
            zip: ZlibEncoder::new(Vec::with_capacity(IDAT), Compression::best()),
            width,
            height,
            bin,
            row,
            dirty: 0,
            pending: 0,
            rows: 0,
            escaped: None,
        })
    }

    /// Ink the pixel for `block` in the row being built.
    ///
    /// A block the picture has no column for is remembered rather than reported:
    /// this runs inside the store's walk over the color's terms, which has
    /// nowhere to put an error.  [`Writer::end_transaction`] raises it, naming
    /// the first one — the first is the informative one, since the rest are
    /// whatever the run went on to see afterwards.
    #[inline]
    pub fn set(&mut self, block: usize) {
        if block >= self.width {
            self.escaped.get_or_insert(block);
            return;
        }
        let byte = block / 8;
        // Leftmost pixel in the most significant bit, so that block ids read
        // left to right across the row -- and cleared rather than set, because
        // ink is the 0 of the two.
        self.row[1 + byte] &= !(0x80 >> (block % 8));
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
        // Bits are only ever cleared below `dirty`, so that prefix is the only
        // part of the row that can have gone dark and the rest is still the
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
    /// with: its size and its samples, row-major, a byte a pixel.
    ///
    /// The bits are unpacked here rather than by asking the decoder to expand
    /// them, so that a sample of [`INK`] means what this file says it means and
    /// the decoder is left to do the inflating and the unfiltering — the two
    /// steps a writer cannot check itself.
    fn decode(path: &str) -> (usize, usize, Vec<u8>) {
        let file = io::BufReader::new(File::open(path).unwrap());
        let mut reader = png::Decoder::new(file).read_info().expect("a PNG header");
        {
            let info = reader.info();
            assert_eq!(info.bit_depth, png::BitDepth::One, "one bit a sample");
            assert_eq!(
                info.color_type,
                png::ColorType::Grayscale,
                "one greyscale channel"
            );
        }
        let mut buf = vec![0u8; reader.output_buffer_size().unwrap()];
        let frame = reader.next_frame(&mut buf).unwrap();
        let (w, h) = (frame.width as usize, frame.height as usize);
        let mut samples = Vec::with_capacity(w * h);
        for y in 0..h {
            let row = &buf[y * frame.line_size..(y + 1) * frame.line_size];
            for x in 0..w {
                samples.push((row[x / 8] >> (7 - x % 8)) & 1);
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

    /// Draw the colors into a file, leaving whatever error came of it to the
    /// caller — the tests that expect one need the writer's own message.
    fn write(
        path: &str,
        width: usize,
        rows: usize,
        bin: usize,
        colors: &[&[usize]],
    ) -> io::Result<()> {
        let mut w = Writer::new(path, width, rows, bin)?;
        for color in colors {
            for &block in *color {
                w.set(block);
            }
            w.end_transaction()?;
        }
        w.finish()
    }

    /// The decoded file as one string a row, checked against the size it was
    /// asked for.
    fn rendered(path: &str, width: usize, rows: usize) -> Vec<String> {
        let (w, h, samples) = decode(path);
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
        let mut w = Writer::new(&path, 10, 1, 1).unwrap();
        w.set(3);
        w.set(64);
        w.set(70);
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
        let Err(e) = Writer::new(&path, 1usize << 33, 1, 1) else {
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
