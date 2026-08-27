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
//! ## A lossless, bilevel JPEG 2000
//!
//! One component, one bit a sample: [`INK`] where the block is in the color and
//! [`PAPER`] where it is not.  A greyscale sample of 0 is black and the largest
//! it can be is white, so at a precision of one bit those two numbers *are*
//! black on white, and "the ones in the output" and "the black pixels" stay the
//! same statement.
//!
//! Lossless, and not as a preference.  A pixel here is a whole fact — these
//! coins did or did not come through that block — so there is no approximation
//! of one that is still an answer, and at one bit a sample there is nothing to
//! approximate with in any case.  The encoder is set up for the reversible path
//! accordingly: the 5/3 integer wavelet, no quantisation, one quality layer at
//! no rate cap.  `the_picture_round_trips_losslessly` asserts that against the
//! decoder rather than trusting the setting.
//!
//! What that buys over the packed raster this used to write is the compression a
//! bitmap has no way to express.  A row is a stretch of ink and then the white
//! past the block its transaction was mined in, and the wavelet plus EBCOT
//! charge almost nothing for a flat stretch of either tone, so on a synthetic
//! run of 42,000 records over 2,000 blocks — 447 pixels in a thousand inked —
//! the same picture came to
//!
//! ```text
//!     packed raster       10.5 MB
//!     the same, gzip -9   56.7 kB
//!     JPEG 2000          103.0 kB
//! ```
//!
//! which is the shape of the trade rather than a win outright.  A raster whose
//! rows resemble one another is exactly what a general-purpose compressor is
//! good at, and on records as regular as those it beats this comfortably.  What
//! it does not leave behind is an image.  This is one, every tool opens it, and
//! [`RESOLUTIONS`] reduced resolutions are in the file: a viewer can show a
//! picture a hundred thousand rows tall at 1/32 scale without decoding the full
//! size, which for these is the difference between looking at it and not.
//!
//! What it costs is time.  Writing a raster is a memcpy; this runs a wavelet and
//! an arithmetic coder over every sample, paper included, and the paper is most
//! of them — on the run above, 84 million samples for about 0.07s over what the
//! same colouring cost written as text.  The number to watch is that one: the
//! picture is `width * rows` samples whatever is drawn in it, so it is `--bin`,
//! which is the only thing that changes how many samples there are, that decides
//! what a long run pays here.
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
//! A JPEG 2000 states its size in front of its first sample and never revisits
//! it, so neither number can be discovered along the way and neither can be
//! stamped in afterwards the way a netpbm header's height could be.  The width
//! is the number of blocks and the height is the number of records divided by
//! the bin, and the driver reads the records once to count both before it colors
//! any of them — see `main`'s `survey`.  That is why a picture wants an input it
//! can rewind and a pipe will not do, and why `--blocks <n>`, which still
//! overrides the width, no longer excuses that pass: the height needs it too.
//!
//! Having promised a height, the writer keeps it.  [`Writer::finish`] pads the
//! picture out with blank rows if the records ran out early, and
//! [`Writer::end_transaction`] refuses a row past the last rather than handing
//! the encoder a tile it did not ask for and writing a file no reader will open.
//!
//! ## Bands and tiles
//!
//! Rows arrive one at a time and the encoder takes whole tiles, so the rows are
//! held a band of [`TILE`] of them at a time, packed eight pixels to the byte,
//! and the band's tiles go out in the order the encoder counts them — across,
//! then down — as soon as the band is full.  The working set is that band and
//! the one tile being unpacked into samples, `TILE * (width / 8 + TILE)` bytes,
//! however tall the picture: a drawing far past the size of memory is still
//! written a megabyte at a time.
//!
//! The codestream is not written straight through either — the encoder goes back
//! to stamp lengths into markers it has already emitted — so the output is a
//! file it can seek in rather than a sink of the caller's choosing.

use std::io;

use openjp2::image::{opj_image, opj_image_comptparm};
use openjp2::openjpeg::{opj_cparameters_t, OPJ_CLRSPC_GRAY, OPJ_CODEC_JP2, OPJ_LRCP};
use openjp2::{Codec, Stream};

/// The side of the square the picture is written in.
///
/// This is the working set: a tile is unpacked whole before it is handed over,
/// so the number trades memory against how much context the wavelet has to
/// compress with.  1024 is a mebibyte of samples a tile, and the same value
/// `tree-jp2` writes its pictures in.
const TILE: usize = 1024;

/// Wavelet decomposition levels.  Six is the encoder's own default, and it is
/// also what lets a viewer open one of these at 1/32 scale without decoding the
/// full-size picture — which, for a picture whose rows outnumber a screen's by
/// three orders of magnitude, is how it gets looked at at all.
const RESOLUTIONS: i32 = 6;

/// A block that is in the color: at one bit a sample, black.
pub const INK: u8 = 0;

/// One that is not: white.
pub const PAPER: u8 = 1;

/// The eight samples a packed byte stands for, leftmost pixel in the most
/// significant bit.
///
/// A table rather than eight shifts and tests, because this is the one thing
/// unpacking a tile does to every pixel of it, and a tile is a million pixels of
/// which the great majority come out of the two entries for `0x00` and `0xff`.
const SAMPLES: [[u8; 8]; 256] = {
    let mut table = [[PAPER; 8]; 256];
    let mut byte = 0usize;
    while byte < 256 {
        let mut bit = 0usize;
        while bit < 8 {
            if byte & (0x80 >> bit) != 0 {
                table[byte][bit] = INK;
            }
            bit += 1;
        }
        byte += 1;
    }
    table
};

/// A picture being drawn a transaction at a time.
///
/// A transaction is drawn by [`Writer::set`], once per block in its color, and
/// closed by [`Writer::end_transaction`]; every `bin` of those makes a row, and
/// every [`TILE`] rows make a band that goes out to the encoder.
pub struct Writer {
    /// Boxed, and set up only once it is in the box.  The encoder keeps
    /// pointers into itself — the tile coder's `cp` is the address of the
    /// codec's own coding parameters — so an encoder that has been started and
    /// is then moved is an encoder reading whatever now lives where it used to
    /// be.  A box is an address that does not move when this struct does, and
    /// putting it there *before* [`Codec::setup_encoder`] is what makes the
    /// pointers it takes stay true.  Left on the stack it fails the way that
    /// costs the most to find: the debug build works.
    codec: Box<Codec>,
    stream: Stream,
    /// The encoder was set up against this and reads it as it goes, so it
    /// outlives the setup call rather than being a local of it.
    image: Box<opj_image>,
    /// Columns, i.e. how many block ids the picture has room for.
    width: usize,
    /// Rows, as promised to the encoder before the first sample.
    height: usize,
    /// Transactions to a row.  One is a row each, which is the plain picture.
    bin: usize,
    /// Bytes a packed row: the distance from one row of `band` to the next.
    stride: usize,
    /// Tiles across the picture, which is also the stride of a tile index.
    across: usize,
    /// Up to [`TILE`] rows, packed eight pixels to the byte.  Zero outside
    /// `..dirty` of each row at all times, which is the invariant that lets
    /// [`Writer::flush_band`] clear only a prefix of each.
    band: Vec<u8>,
    dirty: usize,
    /// Rows finished in the band so far, always below [`TILE`].
    filled: usize,
    /// Transactions drawn into the row being built, always below `bin`.
    pending: usize,
    /// Rows finished in the picture so far, at most `height`.
    rows: usize,
    /// Bands sent, which is the tile row the next one will be.
    bands: usize,
    /// The first block id seen that the picture has no column for, if any.
    escaped: Option<usize>,
}

impl Writer {
    /// Open `path` for a picture `width` columns by `height` rows, with `bin`
    /// transactions to the row, and start the codestream.
    ///
    /// Both dimensions are final here: see the module docs for why a JPEG 2000
    /// cannot be told either of them later.
    pub fn new(path: &str, width: usize, height: usize, bin: usize) -> io::Result<Self> {
        assert!(bin > 0, "a row has to stand for at least one transaction");
        if width == 0 || height == 0 {
            return Err(io::Error::other(
                "an image needs a column and a row, and this one has none",
            ));
        }

        let comp = opj_image_comptparm {
            dx: 1,
            dy: 1,
            w: width as u32,
            h: height as u32,
            x0: 0,
            y0: 0,
            prec: 1,
            bpp: 1,
            sgnd: 0,
        };
        // `tile_create` rather than `create`: it describes the picture without
        // allocating a sample for every pixel of it, which is the whole point of
        // handing the encoder one tile at a time.  The extent is not in the
        // component parameters, so it is set here.
        let mut image = opj_image::tile_create(&[comp], OPJ_CLRSPC_GRAY)
            .ok_or_else(|| io::Error::other("could not describe the image"))?;
        image.x1 = width as u32;
        image.y1 = height as u32;

        let mut params = opj_cparameters_t::default();
        params.tile_size_on = 1;
        params.cp_tdx = TILE as i32;
        params.cp_tdy = TILE as i32;
        params.numresolution = RESOLUTIONS;
        params.prog_order = OPJ_LRCP;
        // Lossless: the reversible 5/3 wavelet, and one quality layer whose rate
        // is 0 — the encoder's spelling of "do not throw anything away".
        params.irreversible = 0;
        params.tcp_numlayers = 1;
        params.tcp_rates[0] = 0.0;
        params.cp_disto_alloc = 1;

        let mut codec = Box::new(
            Codec::new_encoder(OPJ_CODEC_JP2)
                .ok_or_else(|| io::Error::other("could not open a JPEG 2000 encoder"))?,
        );
        // The encoder seeks back over what it has written, so this is a file
        // rather than the caller's choice of sink.
        let mut stream = Stream::new_file(path, 1 << 20, false)?;

        if codec.setup_encoder(&mut params, &mut image) == 0 {
            return Err(io::Error::other("the encoder would not take these settings"));
        }
        if codec.start_compress(&mut image, &mut stream) == 0 {
            return Err(io::Error::other("could not start the codestream"));
        }

        let stride = width.div_ceil(8);
        Ok(Writer {
            codec,
            stream,
            image,
            width,
            height,
            bin,
            stride,
            across: width.div_ceil(TILE),
            // A band, and one row of slack past it.  Rows are drawn into
            // before they are closed, so a record arriving past the last row of
            // a picture shorter than a band needs somewhere to land in the
            // moment before `end_transaction` refuses it.
            band: vec![0u8; stride * (height + 1).min(TILE)],
            dirty: 0,
            filled: 0,
            pending: 0,
            rows: 0,
            bands: 0,
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
        // left to right across the row.
        self.band[self.filled * self.stride + byte] |= 0x80 >> (block % 8);
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
            // Nothing is cleared until the band goes out, so a bin's
            // transactions have been drawing into the same row all along: the
            // row is their union without any of them being asked to compute one.
            self.end_row()?;
        }
        Ok(())
    }

    /// Close the row being built and start the next one, sending the band if
    /// that filled it.
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
        self.pending = 0;
        self.filled += 1;
        self.rows += 1;
        if self.filled == TILE {
            self.flush_band()?;
        }
        Ok(())
    }

    /// Hand the finished band to the encoder, a tile at a time, and clear it.
    ///
    /// The band is as tall as the tile row it stands for — full bands are
    /// [`TILE`] rows and the last one is whatever is left of the height, which
    /// [`Writer::finish`] arranges by padding — so its rows are exactly the
    /// samples the encoder is expecting for these tile indices.
    fn flush_band(&mut self) -> io::Result<()> {
        let rows = self.filled;
        debug_assert_eq!(
            rows,
            TILE.min(self.height - self.bands * TILE),
            "a band is the height of the tile row it is"
        );
        for tx in 0..self.across {
            let x0 = tx * TILE;
            let x1 = ((tx + 1) * TILE).min(self.width);
            let tile = self.tile_bytes(x0, x1, rows);
            // Tiles go out across and then down, which is the order the encoder
            // counts them in and the only one it accepts.
            let index = (self.bands * self.across + tx) as u32;
            if self.codec.write_tile(index, &tile, &mut self.stream) == 0 {
                return Err(io::Error::other(format!("could not write tile {}", index)));
            }
        }
        // Bits are only ever set below `dirty`, so that prefix is the only part
        // of a row that can be non-zero and the rest is still the zeroes it
        // started as.  Colors are sets of *ancestor* blocks and the chain only
        // grows, so early bands reach nowhere near the right-hand edge and this
        // is the difference between clearing a band and clearing the picture.
        for row in 0..rows {
            let at = row * self.stride;
            self.band[at..at + self.dirty].fill(0);
        }
        self.dirty = 0;
        self.filled = 0;
        self.bands += 1;
        Ok(())
    }

    /// One tile of the band as the samples the encoder wants: row-major, a byte
    /// a sample, `[x0, x1)` of each of the band's first `rows` rows.
    ///
    /// `x0` is a multiple of [`TILE`] and so of 8, which is what lets a whole
    /// packed byte become eight samples in one copy; only the far edge of the
    /// picture, where `x1` need not be a multiple of anything, is trimmed.
    fn tile_bytes(&self, x0: usize, x1: usize, rows: usize) -> Vec<u8> {
        let w = x1 - x0;
        let mut tile = vec![PAPER; w * rows];
        for row in 0..rows {
            // `dirty` bounds every row of the band, so a tile past it is paper
            // and is never looked at.
            let packed = &self.band[row * self.stride..row * self.stride + self.dirty];
            let out = &mut tile[row * w..(row + 1) * w];
            for at in (0..w).step_by(8) {
                let byte = x0 / 8 + at / 8;
                if byte >= packed.len() {
                    break;
                }
                let bits = packed[byte];
                if bits == 0 {
                    continue;
                }
                let n = (w - at).min(8);
                out[at..at + n].copy_from_slice(&SAMPLES[bits as usize][..n]);
            }
        }
        tile
    }

    /// `(columns, rows)` as the picture was opened for, which is what it will be
    /// however many records turn up.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// Close the picture: finish the row in hand, pad out to the promised
    /// height, send what is left of the band, and close the codestream.
    pub fn finish(mut self) -> io::Result<()> {
        // A bin that never filled up is still a row: the run does not owe the
        // picture a whole bin's worth of transactions at the end of the records.
        if self.pending > 0 {
            self.end_row()?;
        }
        // The height went into the file before the first sample, so the encoder
        // is owed exactly that many rows.  Short of them the file would be a
        // codestream missing its last tiles, which is not an image; blank rows
        // are at least an honest picture of records that were not there.
        while self.rows < self.height {
            self.end_row()?;
        }
        if self.filled > 0 {
            self.flush_band()?;
        }
        if self.codec.end_compress(&mut self.stream) == 0 {
            return Err(io::Error::other("could not close the codestream"));
        }
        // The encoder points at the image for as long as it is writing, so it
        // goes first.  Nothing runs between here and the end of the scope that
        // would notice, but the order the two are in is not an accident and
        // saying so is cheaper than rediscovering it.
        drop(self.codec);
        drop(self.image);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openjp2::openjpeg::opj_dparameters_t;

    /// A path under the test runner's temporary directory, distinct per test so
    /// that the tests can run in the same directory at the same time.
    fn scratch(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("colors-{}-{name}.jp2", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    /// The picture at `path`, read back with the decoder: its size and its
    /// samples, row-major.
    fn decode(path: &str) -> (usize, usize, Vec<u8>) {
        let mut params = opj_dparameters_t::default();
        let mut codec = Codec::new_decoder(OPJ_CODEC_JP2).unwrap();
        assert!(codec.setup_decoder(&mut params) != 0);

        let mut stream = Stream::new_file(path, 1 << 20, true).unwrap();
        let mut image = codec.read_header(&mut stream).expect("a JP2 header");
        assert!(codec.decode(&mut stream, &mut image) != 0);
        assert!(codec.end_decompress(&mut stream) != 0);

        let (w, h) = (image.x1 as usize, image.y1 as usize);
        let comps = image.comps().unwrap();
        assert_eq!(comps.len(), 1, "one bilevel component");
        assert_eq!(comps[0].prec, 1, "one bit a sample");
        let samples = comps[0].data().unwrap().iter().map(|&s| s as u8).collect();
        (w, h, samples)
    }

    /// Draw `transactions` with one to a row and answer the rows the file
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

    /// Rows are cleared by a prefix, so the case that would catch a wrong prefix
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

    /// The height is a promise made before the first sample, so a record past
    /// the last row is refused rather than quietly dropped or half-written.
    #[test]
    fn a_record_past_the_last_row_is_refused() {
        let path = scratch("too-tall");
        let e = write(&path, 8, 2, 1, &[&[0], &[1], &[2]]).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("2 rows tall"), "{}", e);
        std::fs::remove_file(&path).ok();
    }

    /// And short of the promise the picture is padded rather than left as a
    /// codestream missing its last tiles.
    #[test]
    fn records_that_run_out_early_leave_blank_rows() {
        assert_eq!(
            draw("short", 8, 3, &[&[1]]),
            [".#......", "........", "........"]
        );
    }

    /// The claim the whole file rests on: what the writer inked is what the
    /// decoder gives back, pixel for pixel.  Nothing in the settings says
    /// lossless out loud, so it is asserted rather than assumed.
    #[test]
    fn the_picture_round_trips_losslessly() {
        // Deliberately awkward: a width that is not a multiple of eight, colors
        // that reach both edges, empty ones, and a lone pixel in the middle of
        // the paper — the first thing a quantiser would spend.
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

    /// A picture wider and taller than one tile, so that the tile loop runs more
    /// than once on each axis and the seams have to line up.
    #[test]
    fn a_picture_of_many_tiles_round_trips() {
        let width = TILE + 300;
        // One inked pixel a row, walking across and down, so that a tile drawn
        // from the wrong band or the wrong columns cannot pass.
        let colors: Vec<Vec<usize>> = (0..TILE + 200).map(|r| vec![r % width]).collect();
        let borrowed: Vec<&[usize]> = colors.iter().map(|c| c.as_slice()).collect();

        let picture = binned("tiles", width, colors.len(), 1, &borrowed);
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
