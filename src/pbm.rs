//! # Colors as a picture
//!
//! The text output names every block in every color, which on a real run is
//! tens of gigabytes of `(block . 1)` pairs.  The same answer drawn instead of
//! spelled: one **row** per transaction, in the order the transactions arrive,
//! one **column** per block id, counting up from 0, and a black pixel wherever
//! that block is in that transaction's color.  A color of a thousand blocks is a
//! thousand bits rather than fourteen thousand bytes, and how far back a
//! transaction's coins reach is something you can then look at.
//!
//! What the picture drops is the coefficients.  For the unweighted backends
//! there is nothing to drop — every coefficient is 1, so the color *is* its set
//! of blocks and the bitmap is the whole answer.  Under `--weighted` a pixel
//! says only that some of the transaction's value came from that block, not how
//! much; a weight too small to print as anything but `0.000000` is a black pixel
//! like any other, which is one thing the picture shows better than the text.
//!
//! ## The format
//!
//! Netpbm's binary bitmap, `P4`: a short text header, then the rows packed eight
//! pixels to the byte with the leftmost pixel in the *most* significant bit and
//! each row padded out to a whole byte.  A 1 bit is black, which is what makes
//! "the ones in the output" and "the black pixels" the same statement.  Every
//! image tool reads it.
//!
//! ## Why the header is padded, and why this wants a file
//!
//! `P4` puts the width and the height in front of the raster, but the height is
//! the number of transactions and that is not known until the input has ended —
//! by which time the raster is written and gigabytes long.  So the row count is
//! written into a field of [`HEIGHT_DIGITS`] blanks, and [`Writer::finish`] seeks
//! back and stamps the real number into the hole.  A `usize` never needs more
//! than 20 digits, so the header's length cannot depend on what lands in it, and
//! netpbm lets any run of whitespace separate two header tokens, so the padding
//! is not something a reader has to be told about.
//!
//! That seek is why this writes to a file rather than to stdout: a pipe cannot
//! be rewound.  The width has no such escape — it fixes the distance from one
//! row to the next, so it has to be known before the first row is written, which
//! is what `--blocks` is for.

use std::io::{self, BufWriter, Seek, SeekFrom, Write};

/// Blanks reserved for the row count in the placeholder header.
///
/// `usize::MAX` is 20 digits, so a field this wide fits any number of rows a run
/// could produce and the header's length is a function of the width alone.  That
/// is the whole trick — see the module docs.
const HEIGHT_DIGITS: usize = 20;

/// `P4`, `width` columns, `rows` rows, with the row count right-aligned in its
/// fixed-width field.
fn header(width: usize, rows: usize) -> String {
    format!("P4\n{} {:>field$}\n", width, rows, field = HEIGHT_DIGITS)
}

/// A bitmap being written a row at a time.
///
/// One row is built up by [`Writer::set`], one call per block in the color, and
/// closed by [`Writer::end_row`].  The rows are streamed out as they are
/// finished, so the memory here is one row's worth however tall the image gets.
pub struct Writer<W: Write + Seek> {
    out: BufWriter<W>,
    /// Columns, i.e. how many block ids the image has room for.
    width: usize,
    /// The row being built, eight pixels to the byte and rounded up to a whole
    /// one.  Zero outside `..dirty` at all times, which is the
    /// invariant that lets [`Writer::end_row`] clear only a prefix.
    row: Vec<u8>,
    dirty: usize,
    rows: usize,
    /// The first block id seen that the image has no column for, if any.
    escaped: Option<usize>,
    /// Length of the placeholder header, which is also where the raster starts.
    header_len: usize,
}

impl<W: Write + Seek> Writer<W> {
    /// Start a bitmap `width` columns wide, writing the placeholder header.
    pub fn new(inner: W, width: usize) -> io::Result<Self> {
        let mut out = BufWriter::with_capacity(1 << 20, inner);
        let placeholder = header(width, 0);
        out.write_all(placeholder.as_bytes())?;
        Ok(Writer {
            out,
            width,
            row: vec![0u8; width.div_ceil(8)],
            dirty: 0,
            rows: 0,
            escaped: None,
            header_len: placeholder.len(),
        })
    }

    /// Blacken the pixel for `block` in the row being built.
    ///
    /// A block the image has no column for is remembered rather than reported:
    /// this runs inside the store's walk over the color's terms, which has
    /// nowhere to put an error.  [`Writer::end_row`] raises it, naming the first
    /// one — the first is the informative one, since the rest are whatever the
    /// run went on to see afterwards.
    #[inline]
    pub fn set(&mut self, block: usize) {
        if block >= self.width {
            self.escaped.get_or_insert(block);
            return;
        }
        let byte = block / 8;
        // Leftmost pixel in the most significant bit, which is P4's order.
        self.row[byte] |= 0x80 >> (block % 8);
        if byte >= self.dirty {
            self.dirty = byte + 1;
        }
    }

    /// Emit the row being built and start the next one.
    pub fn end_row(&mut self) -> io::Result<()> {
        if let Some(block) = self.escaped {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "block {} has no column in a {}-column bitmap; \
                     rerun with --blocks {} or more",
                    block,
                    self.width,
                    block + 1
                ),
            ));
        }
        self.out.write_all(&self.row)?;
        // Bits are only ever set below `dirty`, so that prefix is the only part
        // that can be non-zero and the rest is still the zeroes it started as.
        // Colors are sets of *ancestor* blocks and the chain only grows, so
        // early rows reach nowhere near the right-hand edge and this is the
        // difference between clearing the row and clearing the image.
        self.row[..self.dirty].fill(0);
        self.dirty = 0;
        self.rows += 1;
        Ok(())
    }

    /// `(columns, rows)` as the header will end up reading.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.rows)
    }

    /// Stamp the real height into the header, and answer the writer it was
    /// stamped into.
    pub fn finish(mut self) -> io::Result<W> {
        self.out.flush()?;
        let final_header = header(self.width, self.rows);
        // The padding exists precisely so this holds; if it ever did not, the
        // write below would shift the raster by a byte and quietly shear the
        // whole image, so it is worth saying out loud.
        assert_eq!(
            final_header.len(),
            self.header_len,
            "the padded header changed length, which would shift the raster"
        );
        // Past the buffer rather than through it: the raster has just been
        // flushed, and a buffered write here would be a write at the end of the
        // file, not at the front of it.
        let inner = self.out.get_mut();
        inner.seek(SeekFrom::Start(0))?;
        inner.write_all(final_header.as_bytes())?;
        inner.flush()?;
        self.out.into_inner().map_err(|e| e.into_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Drive a writer over a `Cursor` and answer the finished image, header
    /// stamping and all.
    fn draw(width: usize, rows: &[&[usize]]) -> io::Result<Vec<u8>> {
        let mut w = Writer::new(Cursor::new(Vec::new()), width)?;
        for row in rows {
            for &block in *row {
                w.set(block);
            }
            w.end_row()?;
        }
        Ok(w.finish()?.into_inner())
    }

    fn split(image: &[u8]) -> (&[u8], &[u8]) {
        let end = image.iter().position(|&b| b == b'\n').unwrap();
        let end = image[end + 1..].iter().position(|&b| b == b'\n').unwrap() + end + 2;
        image.split_at(end)
    }

    #[test]
    fn the_header_names_the_width_and_the_rows_it_actually_wrote() {
        let image = draw(12, &[&[0], &[1], &[2]]).unwrap();
        let (header, _) = split(&image);
        assert_eq!(
            std::str::from_utf8(header).unwrap(),
            format!("P4\n12 {:>20}\n", 3)
        );
    }

    /// The height is not known when the header is written, so the placeholder
    /// has to be exactly as long as anything that could replace it.
    #[test]
    fn the_placeholder_header_is_the_length_of_the_finished_one() {
        for &width in &[1usize, 12, 170_000, usize::MAX] {
            assert_eq!(header(width, 0).len(), header(width, usize::MAX).len());
        }
    }

    /// Column 0 is the top bit of byte 0: block ids read left to right across
    /// the row, which is what "columns are ordered incrementally" means.
    #[test]
    fn the_lowest_block_is_the_leftmost_pixel() {
        let image = draw(16, &[&[0], &[7], &[8], &[15]]).unwrap();
        let (_, raster) = split(&image);
        assert_eq!(raster, &[0x80, 0x00, 0x01, 0x00, 0x00, 0x80, 0x00, 0x01]);
    }

    /// A width that is not a multiple of eight leaves slack bits at the end of
    /// each row, and they have to stay white.
    #[test]
    fn the_padding_bits_of_a_row_stay_clear() {
        let image = draw(3, &[&[0, 1, 2]]).unwrap();
        let (_, raster) = split(&image);
        assert_eq!(raster, &[0b1110_0000]);
    }

    /// Rows are cleared by a prefix, so the case that would catch a wrong
    /// prefix is a wide row followed by a narrow one.
    #[test]
    fn a_row_does_not_inherit_the_one_before_it() {
        let image = draw(24, &[&[0, 23], &[1], &[]]).unwrap();
        let (_, raster) = split(&image);
        assert_eq!(
            raster,
            &[0x80, 0x00, 0x01, 0x40, 0x00, 0x00, 0x00, 0x00, 0x00]
        );
    }

    /// The bitmap cannot silently drop a block: an image too narrow for the run
    /// is a truncated answer that looks like a whole one.
    #[test]
    fn a_block_past_the_last_column_is_an_error_naming_it() {
        let mut w = Writer::new(Cursor::new(Vec::new()), 10).unwrap();
        w.set(3);
        w.set(64);
        w.set(70);
        let e = w.end_row().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        // The first one over the edge, not the largest and not the last.
        assert!(e.to_string().contains("block 64"), "{}", e);
        assert!(e.to_string().contains("--blocks 65"), "{}", e);
    }

    #[test]
    fn an_empty_color_is_a_blank_row_rather_than_no_row() {
        let image = draw(8, &[&[], &[]]).unwrap();
        let (header, raster) = split(&image);
        assert_eq!(
            std::str::from_utf8(header).unwrap(),
            format!("P4\n8 {:>20}\n", 2)
        );
        assert_eq!(raster, &[0x00, 0x00]);
    }
}
