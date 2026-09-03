//! # A page, written by hand
//!
//! One PDF page of vector ink — lines and circles — produced with no Cairo, no
//! toolkit, no C library at all.  `tree-pdf` wants to run where nothing is
//! installed but a Rust toolchain, and a single page of strokes and fills is a
//! small enough corner of PDF to write directly, the way [`image`](../../image.rs)
//! writes its PNGs: a header, four objects, a cross-reference table, and a
//! content stream deflated by the `flate2` this crate already carries.
//!
//! # What a page holds
//!
//! A PDF content stream is a program for a very small machine: operands, then
//! an operator.  The five this file ever emits are
//!
//! - `w`, `RG`, `rg` — line width, stroke colour, fill colour;
//! - `m`/`l` … `S` — a polyline path, stroked;
//! - `m`/`c` … `f` — a path of cubic Béziers, filled (`b` closes, fills and
//!   strokes in one, for the hollow rings).
//!
//! A circle is not a PDF primitive, so [`Page::circle`] spells one as four
//! cubic Béziers with the usual magic handle length `k = 0.5523` of the
//! radius, which bows each quarter-arc to under 0.03% of a true circle —
//! far below anything a printer resolves.
//!
//! Batching is the caller's affair and the reason the path operators are
//! separate from the paint ones: naming a colour once and filling ten thousand
//! circles in one `f` is what keeps a page of a whole subtree in the hundreds
//! of kilobytes.
//!
//! # Coordinates
//!
//! PDF measures a page in points from the *bottom-left* corner, y running up.
//! This file takes what it is given: the caller owns the flip from whatever
//! screen-like frame its camera thinks in, so that this file has one job.
//!
//! # Why the stream is deflated
//!
//! The operators are decimal text, and a drawing of `n` nodes is `n` circles
//! of four Béziers each — sixty-odd bytes of digits a curve.  `/FlateDecode`
//! is the compression every PDF reader must implement, `flate2` is already a
//! dependency (the PNGs go through it), and digits compress well; the pages
//! this writes come out several times smaller than they are spelled.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::io::Write as _;

use flate2::write::ZlibEncoder;
use flate2::Compression;

/// One page being drawn: its size in points, and the content stream so far.
pub struct Page {
    width: f64,
    height: f64,
    ops: String,
}

/// Numbers are written with two decimals — a fiftieth of a point, which is a
/// two-hundredth of a millimetre — and the trailing noise of `f64` printing
/// kept out of the file.
fn num(v: f64) -> String {
    let s = format!("{v:.2}");
    // "0.50" not "0.5000000001", but also "3" not "3.00": the digits are most
    // of the stream, so the ones that say nothing go.
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" {
        "0".to_string()
    } else {
        s.to_string()
    }
}

/// The Bézier handle that makes four quarter-arcs a circle.
const K: f64 = 0.552_284_749_830_793_4;

impl Page {
    pub fn new(width: f64, height: f64) -> Page {
        Page { width, height, ops: String::new() }
    }

    pub fn width(&self) -> f64 {
        self.width
    }

    pub fn height(&self) -> f64 {
        self.height
    }

    pub fn set_line_width(&mut self, w: f64) {
        let _ = writeln!(self.ops, "{} w", num(w));
    }

    pub fn set_stroke(&mut self, (r, g, b): (f64, f64, f64)) {
        let _ = writeln!(self.ops, "{} {} {} RG", num(r), num(g), num(b));
    }

    pub fn set_fill(&mut self, (r, g, b): (f64, f64, f64)) {
        let _ = writeln!(self.ops, "{} {} {} rg", num(r), num(g), num(b));
    }

    /// Adds one segment to the path being gathered.
    pub fn segment(&mut self, x0: f64, y0: f64, x1: f64, y1: f64) {
        let _ = writeln!(
            self.ops,
            "{} {} m {} {} l",
            num(x0),
            num(y0),
            num(x1),
            num(y1)
        );
    }

    /// Adds one circle to the path being gathered: four cubic Béziers.
    pub fn circle(&mut self, cx: f64, cy: f64, r: f64) {
        let k = K * r;
        let _ = writeln!(
            self.ops,
            "{} {} m {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c {} {} {} {} {} {} c h",
            num(cx + r), num(cy),
            num(cx + r), num(cy + k), num(cx + k), num(cy + r), num(cx), num(cy + r),
            num(cx - k), num(cy + r), num(cx - r), num(cy + k), num(cx - r), num(cy),
            num(cx - r), num(cy - k), num(cx - k), num(cy - r), num(cx), num(cy - r),
            num(cx + k), num(cy - r), num(cx + r), num(cy - k), num(cx + r), num(cy),
        );
    }

    /// Strokes the gathered path and starts a new one.
    pub fn stroke(&mut self) {
        self.ops.push_str("S\n");
    }

    /// Fills the gathered path and starts a new one.
    pub fn fill(&mut self) {
        self.ops.push_str("f\n");
    }

    /// Fills the gathered path in the fill colour and strokes it in the stroke
    /// colour, in one operator: what a hollow ring with a rim is.
    pub fn fill_and_stroke(&mut self) {
        self.ops.push_str("B\n");
    }

    /// Writes `s` at `(x, y)` --- the baseline's left end --- in `size` points
    /// of Helvetica, in the current fill colour.
    ///
    /// Helvetica because it is one of the fourteen fonts every PDF reader
    /// carries, so a label costs the file nothing but its own characters; the
    /// font resource is declared on the page whether or not any text is drawn,
    /// which is a few dozen constant bytes.  The three characters PDF strings
    /// quote --- backslash and the parentheses --- are escaped; everything
    /// this crate labels with is digits, but a writer that only *usually*
    /// writes well-formed files is not one.
    pub fn text(&mut self, x: f64, y: f64, size: f64, s: &str) {
        let mut escaped = String::with_capacity(s.len());
        for c in s.chars() {
            if matches!(c, '\\' | '(' | ')') {
                escaped.push('\\');
            }
            escaped.push(c);
        }
        let _ = writeln!(
            self.ops,
            "BT /F1 {} Tf {} {} Td ({escaped}) Tj ET",
            num(size),
            num(x),
            num(y)
        );
    }

    /// Writes the finished page to `path`: the four objects, the deflated
    /// stream, and a cross-reference table whose offsets are measured off the
    /// very bytes being written.
    pub fn write(self, path: &str) -> Result<(), String> {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder
            .write_all(self.ops.as_bytes())
            .and_then(|()| encoder.finish())
            .map_err(|e| format!("{path}: deflating the page ({e})"))
            .and_then(|stream| {
                let mut out: Vec<u8> = Vec::with_capacity(stream.len() + 512);
                out.extend_from_slice(b"%PDF-1.4\n");
                // A comment of bytes over 127, telling transports the file is
                // binary -- the convention every writer follows.
                out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

                let mut offsets = [0usize; 4];

                offsets[0] = out.len();
                out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

                offsets[1] = out.len();
                out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

                offsets[2] = out.len();
                out.extend_from_slice(
                    format!(
                        "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] \
                         /Contents 4 0 R /Resources << /Font << /F1 << /Type /Font \
                         /Subtype /Type1 /BaseFont /Helvetica >> >> >> >>\nendobj\n",
                        num(self.width),
                        num(self.height)
                    )
                    .as_bytes(),
                );

                offsets[3] = out.len();
                out.extend_from_slice(
                    format!(
                        "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
                        stream.len()
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(&stream);
                out.extend_from_slice(b"\nendstream\nendobj\n");

                let xref = out.len();
                out.extend_from_slice(b"xref\n0 5\n0000000000 65535 f \n");
                for offset in offsets {
                    // Twenty bytes an entry, exactly: ten digits, five digits,
                    // a keyword and a two-byte line end, as the format demands.
                    out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
                }
                out.extend_from_slice(
                    format!(
                        "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n"
                    )
                    .as_bytes(),
                );

                std::fs::write(path, &out).map_err(|e| format!("{path}: {e}"))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// The stream, un-deflated back out of a written file: the operators the
    /// page was given, if nothing was lost on the way.
    fn content_of(bytes: &[u8]) -> String {
        let start = bytes
            .windows(7)
            .position(|w| w == b"stream\n")
            .expect("a stream")
            + 7;
        let end = bytes
            .windows(10)
            .position(|w| w == b"\nendstream")
            .expect("an endstream");

        let mut decoder = flate2::read::ZlibDecoder::new(&bytes[start..end]);
        let mut ops = String::new();
        decoder.read_to_string(&mut ops).expect("a zlib stream");
        ops
    }

    fn scratch(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("tree-pdf-{}-{name}.pdf", std::process::id()))
            .to_string_lossy()
            .into_owned()
    }

    /// What goes in comes back: the operators survive the deflate round trip,
    /// and the page's size is on the page.
    #[test]
    fn the_operators_survive_the_file() {
        let mut page = Page::new(300.0, 200.0);
        page.set_line_width(0.5);
        page.set_stroke((0.78, 0.78, 0.78));
        page.segment(10.0, 20.0, 30.0, 40.0);
        page.stroke();
        page.set_fill((0.0, 0.0, 0.0));
        page.circle(50.0, 60.0, 2.0);
        page.fill();

        let path = scratch("ops");
        page.write(&path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        assert!(bytes.starts_with(b"%PDF-1.4"));
        assert!(bytes.ends_with(b"%%EOF\n"));

        let ops = content_of(&bytes);
        assert!(ops.contains("0.5 w\n"), "{ops}");
        assert!(ops.contains("0.78 0.78 0.78 RG\n"));
        assert!(ops.contains("10 20 m 30 40 l\nS\n"));
        assert!(ops.contains("52 60 m"), "the circle starts at (cx + r, cy)");
        assert!(ops.contains("c h\nf\n"), "four curves, closed and filled");

        let media = String::from_utf8_lossy(&bytes);
        assert!(media.contains("/MediaBox [0 0 300 200]"));

        std::fs::remove_file(&path).ok();
    }

    /// The cross-reference table points where the objects are: each offset in
    /// it lands on the `N 0 obj` it claims to.
    #[test]
    fn the_xref_points_at_the_objects() {
        let mut page = Page::new(100.0, 100.0);
        page.circle(50.0, 50.0, 10.0);
        page.fill();

        let path = scratch("xref");
        page.write(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let text = String::from_utf8_lossy(&bytes);

        let xref_at: usize = text
            .rsplit_once("startxref\n")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok())
            .expect("a startxref");
        assert!(bytes[xref_at..].starts_with(b"xref"));

        // Everything from the table on is plain ASCII; the deflated stream
        // before it is not, so the byte offset is honoured before the text is.
        let table = String::from_utf8_lossy(&bytes[xref_at..]).into_owned();
        for (entry, object) in table.lines().skip(3).take(4).zip(1..) {
            let offset: usize = entry[..10].parse().unwrap();
            let expected = format!("{object} 0 obj");
            assert!(
                bytes[offset..].starts_with(expected.as_bytes()),
                "object {object} is not at {offset}"
            );
        }

        std::fs::remove_file(&path).ok();
    }

    /// A label lands in the stream as a text object in the declared font, and
    /// the characters PDF strings quote come out escaped.
    #[test]
    fn text_is_written_and_escaped() {
        let mut page = Page::new(100.0, 100.0);
        page.set_fill((0.32, 0.32, 0.32));
        page.text(10.0, 20.0, 5.0, "123456789");
        page.text(10.0, 30.0, 5.0, r"a(b)c\d");

        let path = scratch("text");
        page.write(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        let ops = content_of(&bytes);
        assert!(ops.contains("BT /F1 5 Tf 10 20 Td (123456789) Tj ET"), "{ops}");
        assert!(ops.contains(r"(a\(b\)c\\d) Tj"), "{ops}");
        assert!(
            String::from_utf8_lossy(&bytes).contains("/BaseFont /Helvetica"),
            "the font the text names is declared on the page"
        );

        std::fs::remove_file(&path).ok();
    }

    /// Numbers are two decimals and no trailing noise: most of the file is
    /// digits, so the ones that say nothing are not written.
    #[test]
    fn numbers_are_short() {
        assert_eq!(num(3.0), "3");
        assert_eq!(num(0.5), "0.5");
        assert_eq!(num(0.503), "0.5");
        assert_eq!(num(1.25), "1.25");
        assert_eq!(num(-2.10), "-2.1");
        assert_eq!(num(0.001), "0");
        assert_eq!(num(-0.001), "0");
    }
}
