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
//! ## Two formats, and which one is smaller
//!
//! [`Format::Pbm`] is netpbm's binary bitmap, `P4`: a short text header, then the
//! rows packed eight pixels to the byte with the leftmost pixel in the *most*
//! significant bit and each row padded out to a whole byte.  A 1 bit is black,
//! which is what makes "the ones in the output" and "the black pixels" the same
//! statement.  Every image tool reads it.  A row costs `width / 8` bytes and
//! nothing else: the same whether it is empty or solid.
//!
//! [`Format::Svg`] draws the same rows as horizontal strokes, one per *run* of
//! adjacent blocks.  A row then costs about [`SVG_BYTES_PER_RUN`] bytes per run
//! and nothing at all for the white between them, so the two formats scale
//! against different things:
//!
//! ```text
//!     SVG is smaller  <=>  runs per row  <  width / 56
//! ```
//!
//! Which side of that a real chain falls on is a question about the records
//! rather than about the formats, so `--stats` measures it and says what the
//! other format would have cost — see [`Writer::runs`].  Colors that reach back
//! in a few long stretches are a handful of runs however wide the image, and
//! drawing those is far cheaper than packing rows that are mostly white; on
//! synthetic runs here the same records came out
//!
//! ```text
//!     width   runs/row     PBM        SVG    smaller by
//!     50,000       6.3   625 MB     5.2 MB       120x
//!     10,000     176.0   125 MB   112.4 MB       1.11x
//!      5,000      48.1   125 MB    60.4 MB       2.07x
//! ```
//!
//! and it takes a color shot through with gaps — a run every fifty-six columns,
//! all the way across — before the bitmap starts to win.
//!
//! What the vector picture gives up is that it is one enormous `<path>`.  Every
//! image tool reads a `P4`; an SVG of a million rows is a file a browser has to
//! be willing to parse before it will show anything.
//!
//! Neither is compressed, and both compress well; the bitmap of a 200,000-record
//! run goes from 125 MB to 8.8 MB through `gzip` and 6.5 MB through `zstd`.  If
//! the output is only ever going to be read back by a program, that is a bigger
//! win than the choice of format, and it composes with either.
//!
//! ## Folding transactions together
//!
//! A row per transaction is a row per record, and a real run has more records
//! than any screen has pixels — a million rows is already a picture nothing will
//! show you whole.  So a row may stand for more than one transaction:
//! [`Writer::new`]'s `bin` is how many consecutive transactions share a row, and
//! the row they share is the union of their colors, black where *any* of them
//! reaches that block.  Union is what the driver is computing anyway, so a
//! binned picture is the same picture at a coarser scale rather than a sampled
//! or averaged one, and nothing that was black can go white.  The last bin is
//! whatever is left over, and it is written like any other.
//!
//! ## Why the header is padded, and why this wants a file
//!
//! Both formats put the height in front of the picture, and the height is the
//! number of transactions — not known until the input has ended, by which time
//! the picture is written and gigabytes long.  So the header is written with a
//! placeholder and padded out to a length that cannot change, and
//! [`Writer::finish`] seeks back and stamps the real number into the hole.  A
//! `usize` never needs more than 20 digits, so what each format pads *with* is
//! the only difference: netpbm lets any run of whitespace separate two header
//! tokens, and XML lets any run of it separate two attributes.
//!
//! That seek is why this writes to a file rather than to stdout: a pipe cannot
//! be rewound.  The width has no such escape — it fixes the distance from one
//! row to the next, so it cannot be discovered along the way the height can, and
//! has to be settled before the first row is written.  `--blocks` says it
//! outright; without it the driver counts the blocks first, by reading the
//! records twice — see `main`'s `survey`.

use std::io::{self, BufWriter, Seek, SeekFrom, Write};

/// Blanks reserved for the row count in a `P4` header.
///
/// `usize::MAX` is 20 digits, so a field this wide fits any number of rows a run
/// could produce and the header's length is a function of the width alone.  That
/// is the whole trick — see the module docs.
const HEIGHT_DIGITS: usize = 20;

/// Which of the two pictures to draw.  The module docs say when each is smaller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    /// Netpbm's binary bitmap: `width / 8` bytes a row, whatever is in it.
    Pbm,
    /// One stroke per run of adjacent blocks: nothing for the white.
    Svg,
}

/// What one run costs as a stroke, near enough for the note `--stats` prints
/// about the format that was not chosen.
///
/// Measured rather than counted up from the grammar: `m-4930 1h6` is ten bytes
/// and a run at the start of a row is six, and across four synthetic runs of
/// very different shapes the mean came out between 6.3 and 8.3.  It stays that
/// short however big the picture gets because every move but the first says how
/// far the pen went rather than where it landed, and a step is a few digits even
/// when the coordinates are seven.
const SVG_BYTES_PER_RUN: usize = 7;

impl Format {
    /// Roughly what a row of `runs` runs costs in this format.
    fn row_bytes(self, width: usize, runs: usize) -> usize {
        match self {
            Format::Pbm => width.div_ceil(8),
            Format::Svg => SVG_BYTES_PER_RUN * runs,
        }
    }
}

/// `P4`, `width` columns, `rows` rows, with the row count right-aligned in its
/// fixed-width field.
fn pbm_header(width: usize, rows: usize) -> String {
    format!("P4\n{} {:>field$}\n", width, rows, field = HEIGHT_DIGITS)
}

/// The opening of an SVG `width` by `rows`, padded to a length that does not
/// depend on `rows`.
///
/// The padding is spaces before the tag's `>`, which is whitespace between
/// attributes as far as any XML parser is concerned — nothing has to be told
/// about it, and unlike padding a number it cannot change what the number means.
///
/// Rows are strokes rather than rectangles, which is what keeps a run down to
/// about twelve bytes: a stroke is a move and a horizontal line, where a
/// rectangle is four numbers and a tag.  `stroke-width` 1 on a path shifted down
/// half a pixel puts the stroke exactly over the row, so every coordinate in the
/// path stays a whole number.
fn svg_header(width: usize, rows: usize) -> String {
    let tag = |height: usize| {
        format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{0}\" height=\"{1}\" \
             viewBox=\"0 0 {0} {1}\" shape-rendering=\"crispEdges\"",
            width, height
        )
    };
    let mut open = tag(rows);
    // Every height that could replace this one is at most as long as the widest
    // a `usize` gets, so padding to that is padding to a fixed length.
    let padding = tag(usize::MAX).len() - open.len();
    open.extend(std::iter::repeat_n(' ', padding));
    format!(
        "{}><rect width=\"100%\" height=\"100%\" fill=\"#fff\"/>\
         <path fill=\"none\" stroke=\"#000\" stroke-width=\"1\" \
         transform=\"translate(0,.5)\" d=\"",
        open
    )
}

/// What closes an SVG, once the last stroke is in.
const SVG_TAIL: &str = "\"/></svg>\n";

/// Visit the maximal runs of black pixels in `row` as `(start, length)`.
///
/// Uniform bytes are stepped over eight pixels at a time, which is what makes
/// this cheap on the rows that matter: a color that reaches back a long way is
/// mostly solid `0xff` and empty `0x00`, and only the ends of its runs cost a
/// look at individual bits.
fn for_each_run(row: &[u8], mut visit: impl FnMut(usize, usize)) {
    let bits = row.len() * 8;
    let black = |x: usize| row[x / 8] & (0x80 >> (x % 8)) != 0;

    // The next pixel at or after `from` that is black, or is not.
    let find = |mut x: usize, want: bool| {
        let skip = if want { 0x00 } else { 0xff };
        while x < bits {
            if x.is_multiple_of(8) && row[x / 8] == skip {
                x += 8;
            } else if black(x) == want {
                return x;
            } else {
                x += 1;
            }
        }
        bits
    };

    let mut x = find(0, true);
    while x < bits {
        let end = find(x, false);
        visit(x, end - x);
        x = find(end, true);
    }
}

/// A bitmap being written a row at a time.
///
/// A transaction is drawn by [`Writer::set`], once per block in its color, and
/// closed by [`Writer::end_transaction`]; every `bin` of those makes a row.  The
/// rows are streamed out as they are finished, so the memory here is one row's
/// worth however tall the image gets.
pub struct Writer<W: Write + Seek> {
    out: BufWriter<W>,
    /// Columns, i.e. how many block ids the image has room for.
    width: usize,
    /// Transactions to a row.  One is a row each, which is the plain picture.
    bin: usize,
    /// The row being built, eight pixels to the byte and rounded up to a whole
    /// one.  Zero outside `..dirty` at all times, which is the invariant that
    /// lets [`Writer::flush_row`] clear only a prefix.
    row: Vec<u8>,
    dirty: usize,
    /// Transactions drawn into the row so far, always below `bin`.
    pending: usize,
    rows: usize,
    /// The first block id seen that the image has no column for, if any.
    escaped: Option<usize>,
    /// Length of the placeholder header, which is also where the picture starts.
    header_len: usize,
    format: Format,
    /// Where the SVG path's pen is, once it has been put down.  Every move after
    /// the first is relative to it, which is what keeps the numbers short in an
    /// image whose coordinates run to seven digits.
    pen: Option<(usize, usize)>,
    /// The strokes of one row, built up before going out in a single write.
    /// Reused across rows, like the driver's line buffer.
    strokes: Vec<u8>,
    /// Runs counted so far, and whether to bother counting them.  Only `--stats`
    /// wants this, and for [`Format::Pbm`] counting means a scan of the row that
    /// writing it does not otherwise need.
    runs: u64,
    measure: bool,
}

impl<W: Write + Seek> Writer<W> {
    /// Start a picture `width` columns wide with `bin` transactions to the row,
    /// writing the placeholder header.
    pub fn new(
        inner: W,
        format: Format,
        width: usize,
        bin: usize,
        measure: bool,
    ) -> io::Result<Self> {
        assert!(bin > 0, "a row has to stand for at least one transaction");
        let mut out = BufWriter::with_capacity(1 << 20, inner);
        let placeholder = match format {
            Format::Pbm => pbm_header(width, 0),
            Format::Svg => svg_header(width, 0),
        };
        out.write_all(placeholder.as_bytes())?;
        Ok(Writer {
            out,
            width,
            bin,
            row: vec![0u8; width.div_ceil(8)],
            dirty: 0,
            pending: 0,
            rows: 0,
            escaped: None,
            header_len: placeholder.len(),
            format,
            pen: None,
            strokes: Vec::new(),
            runs: 0,
            measure: measure || format == Format::Svg,
        })
    }

    /// Blacken the pixel for `block` in the row being built.
    ///
    /// A block the image has no column for is remembered rather than reported:
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
        // Leftmost pixel in the most significant bit, which is P4's order.
        self.row[byte] |= 0x80 >> (block % 8);
        if byte >= self.dirty {
            self.dirty = byte + 1;
        }
    }

    /// Finish one transaction, emitting the row if it completes a bin.
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
            // Nothing is cleared until here, so a bin's transactions have been
            // drawing into the same row all along: the row is their union
            // without any of them being asked to compute one.
            self.flush_row()?;
        }
        Ok(())
    }

    /// Write the row being built and start the next one.
    fn flush_row(&mut self) -> io::Result<()> {
        match self.format {
            Format::Pbm => {
                self.out.write_all(&self.row)?;
                if self.measure {
                    // Nothing here needs the runs; this is only so `--stats` can
                    // say what the row would have cost drawn the other way.
                    let runs = &mut self.runs;
                    for_each_run(&self.row[..self.dirty], |_, _| *runs += 1);
                }
            }
            Format::Svg => {
                // Split the borrow: the runs come out of `row` while the strokes
                // go into `strokes`, and both live in `self`.
                let Writer {
                    row,
                    dirty,
                    strokes,
                    pen,
                    rows,
                    runs,
                    ..
                } = self;
                strokes.clear();
                for_each_run(&row[..*dirty], |start, length| {
                    *runs += 1;
                    match pen.replace((start + length, *rows)) {
                        // The pen has to be put down somewhere absolute once.
                        None => {
                            strokes.push(b'M');
                            push_int(strokes, start);
                            strokes.push(b' ');
                            push_int(strokes, *rows);
                        }
                        // Everywhere after that, say how far it moved.  Within a
                        // row that is the width of the gap; between rows it is a
                        // long step back and one down, and either way it is far
                        // fewer digits than saying where it landed.
                        Some((x, y)) => {
                            strokes.push(b'm');
                            push_delta(strokes, start as isize - x as isize);
                            strokes.push(b' ');
                            push_delta(strokes, *rows as isize - y as isize);
                        }
                    }
                    strokes.push(b'h');
                    push_int(strokes, length);
                });
                self.out.write_all(&self.strokes)?;
            }
        }
        // Bits are only ever set below `dirty`, so that prefix is the only part
        // that can be non-zero and the rest is still the zeroes it started as.
        // Colors are sets of *ancestor* blocks and the chain only grows, so
        // early rows reach nowhere near the right-hand edge and this is the
        // difference between clearing the row and clearing the image.
        self.row[..self.dirty].fill(0);
        self.dirty = 0;
        self.pending = 0;
        self.rows += 1;
        Ok(())
    }

    /// `(columns, rows)` as the header will end up reading, counting the bin in
    /// progress as the row it is going to become.
    pub fn dimensions(&self) -> (usize, usize) {
        (self.width, self.rows + usize::from(self.pending > 0))
    }

    /// How many runs of adjacent blocks the picture is made of, and what it
    /// would have cost in each format — the numbers behind the module docs'
    /// question of which one is smaller.
    ///
    /// Only meaningful when the writer was asked to `measure`; the runs of a
    /// bitmap are not otherwise counted, since nothing about writing one needs
    /// to know them.
    pub fn runs(&self) -> (u64, usize, usize) {
        let per_row = |format: Format| {
            let (_, rows) = self.dimensions();
            let mean = if rows == 0 {
                0
            } else {
                (self.runs / rows as u64) as usize
            };
            format.row_bytes(self.width, mean)
        };
        (self.runs, per_row(Format::Pbm), per_row(Format::Svg))
    }

    /// Close the picture, stamp the real height into the header, and answer the
    /// writer it was stamped into.
    pub fn finish(mut self) -> io::Result<W> {
        // A bin that never filled up is still a row: the run does not owe the
        // image a whole bin's worth of transactions at the end of the records.
        if self.pending > 0 {
            self.flush_row()?;
        }
        if self.format == Format::Svg {
            self.out.write_all(SVG_TAIL.as_bytes())?;
        }
        self.out.flush()?;

        let final_header = match self.format {
            Format::Pbm => pbm_header(self.width, self.rows),
            Format::Svg => svg_header(self.width, self.rows),
        };
        // The padding exists precisely so this holds; if it ever did not, the
        // write below would shift the picture by a byte and quietly shear the
        // whole image, so it is worth saying out loud.
        assert_eq!(
            final_header.len(),
            self.header_len,
            "the padded header changed length, which would shift the picture"
        );
        // Past the buffer rather than through it: the picture has just been
        // flushed, and a buffered write here would be a write at the end of the
        // file, not at the front of it.
        let inner = self.out.get_mut();
        inner.seek(SeekFrom::Start(0))?;
        inner.write_all(final_header.as_bytes())?;
        inner.flush()?;
        self.out.into_inner().map_err(|e| e.into_error())
    }
}

/// Decimal, straight into the buffer — the driver's [`crate::push_int`], which
/// exists for the same reason: this runs once per number in a path with tens of
/// millions of them.
fn push_int(out: &mut Vec<u8>, value: usize) {
    crate::push_int(out, value);
}

/// The same, signed, since every move but the first is a step that can go back
/// up the row or back to the left of it.
fn push_delta(out: &mut Vec<u8>, value: isize) {
    if value < 0 {
        out.push(b'-');
    }
    push_int(out, value.unsigned_abs());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Draw one transaction per row and answer the finished bitmap, header
    /// stamping and all.
    fn draw(width: usize, transactions: &[&[usize]]) -> io::Result<Vec<u8>> {
        binned(width, 1, transactions)
    }

    /// The same, with `bin` transactions to the row.
    fn binned(width: usize, bin: usize, transactions: &[&[usize]]) -> io::Result<Vec<u8>> {
        drawn(Format::Pbm, width, bin, transactions)
    }

    fn drawn(
        format: Format,
        width: usize,
        bin: usize,
        transactions: &[&[usize]],
    ) -> io::Result<Vec<u8>> {
        let mut w = Writer::new(Cursor::new(Vec::new()), format, width, bin, true)?;
        for color in transactions {
            for &block in *color {
                w.set(block);
            }
            w.end_transaction()?;
        }
        Ok(w.finish()?.into_inner())
    }

    /// The `d` attribute of the one path in an SVG.
    fn path_of(image: &[u8]) -> String {
        let text = std::str::from_utf8(image).unwrap();
        let start = text.find(" d=\"").unwrap() + 4;
        text[start..].split('"').next().unwrap().to_string()
    }

    /// Read a path back into the pixels it draws, so an SVG can be compared with
    /// the bitmap of the same records.
    ///
    /// Only the two commands this writer emits are understood, which is the
    /// point: it re-derives the picture from the file rather than from anything
    /// the writer still has in hand.
    fn rasterize(image: &[u8], width: usize) -> Vec<Vec<bool>> {
        let mut rows: Vec<Vec<bool>> = Vec::new();
        let (mut x, mut y) = (0isize, 0isize);
        let d = path_of(image);
        let mut tokens = d
            .replace('M', " M ")
            .replace('m', " m ")
            .replace('h', " h ")
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>()
            .into_iter()
            .peekable();
        while let Some(token) = tokens.next() {
            let mut number = || tokens.next().unwrap().parse::<isize>().unwrap();
            match token.as_str() {
                "M" => {
                    x = number();
                    y = number();
                }
                "m" => {
                    x += number();
                    y += number();
                }
                "h" => {
                    let length = number();
                    while rows.len() <= y as usize {
                        rows.push(vec![false; width]);
                    }
                    for column in x..x + length {
                        rows[y as usize][column as usize] = true;
                    }
                    x += length;
                }
                other => panic!("unexpected path command {:?}", other),
            }
        }
        rows
    }

    /// The rows of a `P4` image as booleans, for comparing against a path.
    fn unpack(image: &[u8], width: usize) -> Vec<Vec<bool>> {
        let (head, raster) = split(image);
        let rows: usize = std::str::from_utf8(head)
            .unwrap()
            .split_whitespace()
            .nth(2)
            .unwrap()
            .parse()
            .unwrap();
        let stride = width.div_ceil(8);
        (0..rows)
            .map(|r| {
                (0..width)
                    .map(|c| raster[r * stride + c / 8] & (0x80 >> (c % 8)) != 0)
                    .collect()
            })
            .collect()
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
    /// has to be exactly as long as anything that could replace it — in both
    /// formats, which pad with different things for different reasons.
    #[test]
    fn the_placeholder_header_is_the_length_of_the_finished_one() {
        for &width in &[1usize, 12, 170_000, usize::MAX] {
            assert_eq!(
                pbm_header(width, 0).len(),
                pbm_header(width, usize::MAX).len()
            );
            assert_eq!(
                svg_header(width, 0).len(),
                svg_header(width, usize::MAX).len()
            );
        }
    }

    /// The two formats are two spellings of one picture, so the surest thing to
    /// ask of the vector one is that it draws the raster one — read back out of
    /// the file, not out of the writer.
    #[test]
    fn the_svg_draws_the_same_pixels_as_the_bitmap() {
        let colors: &[&[usize]] = &[
            &[0, 1, 2, 9],
            &[],
            &[19],
            &[0, 19],
            &[3, 4, 5, 6, 7, 8, 9, 10],
            &[0, 2, 4, 6, 8, 10, 12, 14, 16, 18],
        ];
        for bin in [1usize, 2, 4, 7] {
            let svg = drawn(Format::Svg, 20, bin, colors).unwrap();
            let pbm = drawn(Format::Pbm, 20, bin, colors).unwrap();
            let (mut drawn_rows, wanted) = (rasterize(&svg, 20), unpack(&pbm, 20));
            // A picture ending in blank rows draws nothing for them, so the path
            // stops early and the missing rows are white.
            drawn_rows.resize(wanted.len(), vec![false; 20]);
            assert_eq!(drawn_rows, wanted, "bin {}", bin);
        }
    }

    /// Runs are strokes, and a stroke is a move and a length: the first move is
    /// absolute and every one after it says how far the pen went, which is what
    /// keeps a run to about a dozen bytes however big the coordinates get.
    #[test]
    fn the_path_moves_relative_after_the_first_stroke() {
        let image = drawn(Format::Svg, 40, 1, &[&[2, 3, 4, 30], &[], &[1]]).unwrap();
        assert_eq!(path_of(&image), "M2 0h3m25 0h1m-30 2h1");
    }

    /// An SVG says its size in three places and they have to agree, or a viewer
    /// scales the picture to a box the wrong shape.
    #[test]
    fn the_svg_header_states_the_finished_size() {
        let image = drawn(Format::Svg, 12, 2, &[&[0], &[5], &[7]]).unwrap();
        let text = std::str::from_utf8(&image).unwrap();
        assert!(text.contains("width=\"12\""), "{}", text);
        assert!(text.contains("height=\"2\""), "{}", text);
        assert!(text.contains("viewBox=\"0 0 12 2\""), "{}", text);
        assert!(text.ends_with("\"/></svg>\n"), "{}", text);
    }

    /// An empty picture is still a well-formed one: a viewer should open it and
    /// show nothing, rather than choke on a path that was never closed.
    #[test]
    fn an_svg_with_nothing_in_it_is_still_closed() {
        let image = drawn(Format::Svg, 8, 1, &[]).unwrap();
        let text = std::str::from_utf8(&image).unwrap();
        assert!(text.contains("d=\"\"/>"), "{}", text);
        assert!(text.contains("height=\"0\""), "{}", text);
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

    /// A bin's row is the union of its transactions' colors, which is the one
    /// thing that makes a compacted picture a coarser picture rather than a
    /// sampled one: a block black at bin `n` is black at every coarser bin too.
    #[test]
    fn a_binned_row_is_the_union_of_the_transactions_in_it() {
        let image = binned(24, 3, &[&[0], &[9], &[23], &[1], &[2], &[3]]).unwrap();
        let (header, raster) = split(&image);
        assert_eq!(
            std::str::from_utf8(header).unwrap(),
            format!("P4\n24 {:>20}\n", 2)
        );
        assert_eq!(
            raster,
            &[0x80, 0x40, 0x01, /* {0,9,23} */ 0x70, 0x00, 0x00 /* {1,2,3} */]
        );
    }

    /// The records do not owe the image a whole bin at the end of the run.
    #[test]
    fn a_bin_left_part_full_is_still_a_row() {
        let image = binned(8, 4, &[&[0], &[1], &[2], &[3], &[6], &[7]]).unwrap();
        let (header, raster) = split(&image);
        assert_eq!(
            std::str::from_utf8(header).unwrap(),
            format!("P4\n8 {:>20}\n", 2)
        );
        assert_eq!(raster, &[0b1111_0000, 0b0000_0011]);
    }

    /// Binning must not turn a picture into a smeared one: what a bin
    /// accumulates has to stop at the bin's edge.
    #[test]
    fn a_bin_does_not_leak_into_the_next_one() {
        let image = binned(16, 2, &[&[0, 15], &[1], &[8], &[]]).unwrap();
        let (_, raster) = split(&image);
        assert_eq!(raster, &[0xC0, 0x01, 0x00, 0x80]);
    }

    /// Binning by one is the plain picture, not a special case of it.
    #[test]
    fn binning_by_one_draws_what_no_binning_draws() {
        let colors: &[&[usize]] = &[&[0, 5], &[3], &[], &[7]];
        assert_eq!(binned(8, 1, colors).unwrap(), draw(8, colors).unwrap());
    }

    /// The bitmap cannot silently drop a block: an image too narrow for the run
    /// is a truncated answer that looks like a whole one.
    #[test]
    fn a_block_past_the_last_column_is_an_error_naming_it() {
        let mut w = Writer::new(Cursor::new(Vec::new()), Format::Pbm, 10, 1, false).unwrap();
        w.set(3);
        w.set(64);
        w.set(70);
        let e = w.end_transaction().unwrap_err();
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
