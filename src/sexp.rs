//! A streaming reader for the transaction records the driver eats.
//!
//! The Scheme reads them with plain `(read)`, so the format is whitespace
//! agnostic and a record may be split across lines; this reader is too, rather
//! than assuming the one-record-per-line the sample files happen to use.
//!
//! Records look like
//!
//! ```text
//! ((timestamp block-id tx-id _ _ _ _) (input ...) (output ...))
//! input  = (addr-id amount prev-tx-id vout)
//! output = (addr-id amount _)
//! ```
//!
//! Only five things are ever used: the header's `block-id` and `tx-id`, each
//! input's `amount` and `prev-tx-id`, and how many outputs there are.  Fields
//! are picked out by position and the rest of each list is skipped, so a record
//! with extra trailing fields still reads — unlike the Scheme's fixed-arity
//! `match/first`.  Nothing is allocated per record.
//!
//! The `amount` is there for `--weighted`, which shares each input's color out
//! in proportion to it; the unweighted driver reads it and ignores it.
//!
//! The buffer is ours rather than a `BufReader`, because at 149 GB the hot path
//! is "look at one byte" and it needs to inline down to a bounds check.
//!
//! ## Skipped means skipped
//!
//! Those four values are a small minority of what a record spells out: the
//! header gives 2 of its 7 fields to the driver, an input 2 of its 4, an output
//! none of its 3 — an output is read only for existing.  So the wanted
//! positions are named in a bitmask ([`HEADER_FIELDS`] and friends) and
//! everything else goes through [`Reader::skip_int`], which walks the digits
//! without converting them.  Turning a number into a `usize` costs a multiply
//! and an add per digit, and there is no point paying it for a number that is
//! dropped on the next line.
//!
//! ## Digits are scanned wide
//!
//! Finding where a run of digits ends is the one thing this reader does to
//! nearly every byte, and it is a poor fit for a loop and a good fit for a
//! vector compare.  [`simd::digit_run`] answers it sixteen bytes at a time, and
//! [`simd::eight_digits`] folds eight digits into a value in three
//! multiply-shift steps, so [`Reader::read_int`] checks for overflow once per
//! eight digits rather than once per digit.  [`Reader::digit_run`] is the seam
//! between those kernels and this buffer: it is what handles a run of digits
//! that straddles a refill, so neither kernel has to know the buffer exists.

use crate::simd;
use std::io::{self, Read};

const BUF_SIZE: usize = 1 << 20;

/// Positions the record header is read for: the block id at 1, the tx id at 2.
const HEADER_FIELDS: u32 = (1 << 1) | (1 << 2);

/// Positions an input is read for: the amount at 1 and the previous tx id at 2.
///
/// The amount is only wanted by `--weighted`, but it is read either way. Making
/// the mask depend on the mode would push a branch into the innermost parsing
/// loop to save one field of four on runs that do not use it, and the
/// measurement that matters here is that reading a field costs a `digit_run` and
/// eight-digit folding, not a per-field decision.
const INPUT_FIELDS: u32 = (1 << 1) | (1 << 2);

/// An output is read for nothing at all — only for there being one.
const OUTPUT_FIELDS: u32 = 0;

pub struct Record {
    pub block_id: usize,
    pub tx_id: usize,
    pub outputs: usize,
}

/// One input of a record: the transaction it spends, and for how much.
///
/// The amount is what `--weighted` shares a color out in proportion to; the
/// unweighted driver ignores it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Input {
    pub prev_tx_id: usize,
    pub amount: usize,
}

pub struct Reader<R: Read> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    end: usize,
    /// Bytes consumed strictly before `buf[pos]`, for error messages.
    consumed: usize,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Reader {
            inner,
            buf: vec![0u8; BUF_SIZE].into_boxed_slice(),
            pos: 0,
            end: 0,
            consumed: 0,
        }
    }

    #[inline]
    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.pos < self.end {
            return Ok(Some(self.buf[self.pos]));
        }
        self.refill()
    }

    #[cold]
    fn refill(&mut self) -> io::Result<Option<u8>> {
        self.consumed += self.end;
        self.pos = 0;
        self.end = 0;
        loop {
            match self.inner.read(&mut self.buf) {
                Ok(0) => return Ok(None),
                Ok(n) => {
                    self.end = n;
                    return Ok(Some(self.buf[0]));
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
    }

    fn offset(&self) -> usize {
        self.consumed + self.pos
    }

    fn err<T>(&self, msg: String) -> io::Result<T> {
        Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{} at byte offset {}", msg, self.offset()),
        ))
    }

    /// The next non-whitespace byte, left unconsumed.
    #[inline]
    fn skip_ws(&mut self) -> io::Result<Option<u8>> {
        loop {
            match self.peek()? {
                Some(b) if b.is_ascii_whitespace() => self.pos += 1,
                other => return Ok(other),
            }
        }
    }

    fn expect(&mut self, want: u8) -> io::Result<()> {
        match self.skip_ws()? {
            Some(b) if b == want => {
                self.pos += 1;
                Ok(())
            }
            Some(b) => self.err(format!(
                "expected {:?}, found {:?}",
                want as char, b as char
            )),
            None => self.err(format!("expected {:?}, found end of input", want as char)),
        }
    }

    /// Walk the run of digits at the cursor, handing each contiguous slice of it
    /// to `absorb`, and answer how many digits there were altogether.
    ///
    /// `absorb` is called more than once when the run straddles a refill.  The
    /// buffer is a window onto the stream and [`simd::digit_run`] can only speak
    /// for what is inside it, so a run reaching the end of the window is not
    /// known to have ended: refill and scan again.  Keeping that here is what
    /// lets the vector kernel work in whole 16-byte blocks without any caller
    /// having to think about the buffer boundary.
    #[inline]
    fn digit_run(&mut self, mut absorb: impl FnMut(&[u8])) -> io::Result<usize> {
        let mut digits = 0;
        loop {
            let run = simd::digit_run(&self.buf[self.pos..self.end]);
            if run > 0 {
                absorb(&self.buf[self.pos..self.pos + run]);
                self.pos += run;
                digits += run;
            }
            if self.pos < self.end {
                // Stopped on a byte that is not a digit: the run is over.
                return Ok(digits);
            }
            // The window is spent.  `peek` refills it, or says the stream has
            // ended and the run with it.
            if self.peek()?.is_none() {
                return Ok(digits);
            }
        }
    }

    /// Read one non-negative integer.  Nothing in these records is ever
    /// negative — the fields are timestamps, ids, counts and satoshi amounts —
    /// so a leading `-` is a malformed record rather than a number to accept.
    ///
    /// The digits are folded eight at a time, which is also how often the
    /// overflow check runs; the remainder is finished off one at a time.
    fn read_int(&mut self) -> io::Result<usize> {
        let first = match self.skip_ws()? {
            Some(b) => b,
            None => return self.err("expected an integer, found end of input".into()),
        };
        if first == b'-' {
            return self.err("negative integer, but every field here is unsigned".into());
        }

        let mut value: usize = 0;
        let mut overflowed = false;
        let digits = self.digit_run(|run| {
            if overflowed {
                return;
            }
            let mut chunks = run.chunks_exact(8);
            for chunk in &mut chunks {
                let eight = simd::eight_digits(u64::from_le_bytes(chunk.try_into().unwrap()));
                match value
                    .checked_mul(100_000_000)
                    .and_then(|v| v.checked_add(eight as usize))
                {
                    Some(v) => value = v,
                    None => {
                        overflowed = true;
                        return;
                    }
                }
            }
            for &b in chunks.remainder() {
                match value
                    .checked_mul(10)
                    .and_then(|v| v.checked_add((b - b'0') as usize))
                {
                    Some(v) => value = v,
                    None => {
                        overflowed = true;
                        return;
                    }
                }
            }
        })?;

        if overflowed {
            return self.err("integer does not fit in a usize".into());
        }
        if digits == 0 {
            return self.err(format!("expected an integer, found {:?}", first as char));
        }

        Ok(value)
    }

    /// Consume one integer without working out what it is.
    ///
    /// The cheapest way to read a number nobody wants is not to convert it, and
    /// most of the numbers in a record are numbers nobody wants — see the module
    /// docs.  Every malformed-input check that does not need the value is still
    /// made: a leading `-` is refused, and so is an empty field.
    ///
    /// The one check given up is the overflow test, which cannot be made without
    /// doing the arithmetic this exists to avoid.  A field too wide for a
    /// `usize` therefore passes here where [`Reader::read_int`] would reject it.
    /// Nothing downstream ever sees the value, so that is a rejection the run
    /// did not depend on — and satoshi amounts, the widest fields in the format,
    /// have four digits of headroom against a 64-bit `usize` anyway.
    fn skip_int(&mut self) -> io::Result<()> {
        let first = match self.skip_ws()? {
            Some(b) => b,
            None => return self.err("expected an integer, found end of input".into()),
        };
        if first == b'-' {
            return self.err("negative integer, but every field here is unsigned".into());
        }
        if self.digit_run(|_| {})? == 0 {
            return self.err(format!("expected an integer, found {:?}", first as char));
        }
        Ok(())
    }

    /// Read `( int int ... )`, handing the elements whose position is set in
    /// `wanted` to `visit`, skipping the rest, and answering how many elements
    /// there were in total.
    ///
    /// `wanted` is a bitmask over positions: position `n` is wanted when bit `n`
    /// is set.  Positions from 32 up are never wanted, which is the existing
    /// rule that fields beyond the named ones are ignored, arriving at the same
    /// place by a cheaper route.
    fn read_flat_list(
        &mut self,
        wanted: u32,
        mut visit: impl FnMut(usize, usize),
    ) -> io::Result<usize> {
        self.expect(b'(')?;
        let mut n = 0;
        loop {
            match self.skip_ws()? {
                Some(b')') => {
                    self.pos += 1;
                    return Ok(n);
                }
                Some(_) => {
                    if n < 32 && wanted & (1 << n) != 0 {
                        let v = self.read_int()?;
                        visit(n, v);
                    } else {
                        self.skip_int()?;
                    }
                    n += 1;
                }
                None => return self.err("unterminated list".into()),
            }
        }
    }

    /// The next record, or `None` at end of input.  `inputs` is cleared and
    /// refilled with one [`Input`] per input of the record; pass the same vector
    /// every time to keep this allocation-free.
    pub fn next_record(&mut self, inputs: &mut Vec<Input>) -> io::Result<Option<Record>> {
        inputs.clear();

        if self.skip_ws()?.is_none() {
            return Ok(None);
        }
        self.expect(b'(')?;

        let (mut block_id, mut tx_id) = (None, None);
        let fields = self.read_flat_list(HEADER_FIELDS, |i, v| match i {
            1 => block_id = Some(v),
            2 => tx_id = Some(v),
            _ => {}
        })?;
        let (block_id, tx_id) = match (block_id, tx_id) {
            (Some(b), Some(t)) => (b, t),
            _ => {
                return self.err(format!(
                    "record header has {} fields, expected the block id at position 1 and the tx id at position 2",
                    fields
                ))
            }
        };

        self.expect(b'(')?;
        loop {
            match self.skip_ws()? {
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b'(') => {
                    let (mut amount, mut prev_tx_id) = (None, None);
                    let fields = self.read_flat_list(INPUT_FIELDS, |i, v| match i {
                        1 => amount = Some(v),
                        2 => prev_tx_id = Some(v),
                        _ => {}
                    })?;
                    match (amount, prev_tx_id) {
                        (Some(amount), Some(prev_tx_id)) => inputs.push(Input {
                            prev_tx_id,
                            amount,
                        }),
                        _ => {
                            return self.err(format!(
                                "input has {} fields, expected the amount at position 1 and \
                                 the previous tx id at position 2",
                                fields
                            ))
                        }
                    }
                }
                _ => return self.err("expected an input".into()),
            }
        }

        self.expect(b'(')?;
        let mut outputs = 0;
        loop {
            match self.skip_ws()? {
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b'(') => {
                    self.read_flat_list(OUTPUT_FIELDS, |_, _| {})?;
                    outputs += 1;
                }
                _ => return self.err("expected an output".into()),
            }
        }

        self.expect(b')')?;
        Ok(Some(Record {
            block_id,
            tx_id,
            outputs,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a record boils down to, for comparing two reads of the same bytes.
    type Parsed = (usize, usize, Vec<Input>, usize);

    fn inp(prev_tx_id: usize, amount: usize) -> Input {
        Input {
            prev_tx_id,
            amount,
        }
    }

    /// A `Read` that hands back one byte per call, so the reader refills its
    /// window at every single position in the stream.
    ///
    /// This is the point of the whole test module.  [`Reader::digit_run`] has to
    /// stitch a run of digits back together when it straddles a refill, and with
    /// a 1 MiB buffer that happens about once per million bytes — far too rare
    /// for a corpus to be trusted to cover it, and the corpus cannot say *which*
    /// digit the split fell on.  Reading a byte at a time makes every number in
    /// the input take the straddling path, at every offset within itself.
    struct Dribble<'a>(&'a [u8]);

    impl Read for Dribble<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.0.is_empty() || out.is_empty() {
                return Ok(0);
            }
            out[0] = self.0[0];
            self.0 = &self.0[1..];
            Ok(1)
        }
    }

    fn read_all(inner: impl Read) -> io::Result<Vec<Parsed>> {
        let mut reader = Reader::new(inner);
        let mut inputs = Vec::new();
        let mut records = Vec::new();
        while let Some(r) = reader.next_record(&mut inputs)? {
            records.push((r.block_id, r.tx_id, inputs.clone(), r.outputs));
        }
        Ok(records)
    }

    /// Read `text` both in one gulp and a byte at a time, insist the two agree,
    /// and answer what they said.
    fn parse(text: &str) -> io::Result<Vec<Parsed>> {
        let whole = read_all(text.as_bytes());
        let dribbled = read_all(Dribble(text.as_bytes()));
        match (&whole, &dribbled) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "buffered and dribbled reads disagree"),
            (Err(a), Err(b)) => assert_eq!(
                a.to_string(),
                b.to_string(),
                "buffered and dribbled reads disagree on the error"
            ),
            _ => panic!("one read succeeded and the other did not: {:?}", whole),
        }
        whole
    }

    const SAMPLE: &str =
        "((1231006505 0 9 1 2 3 4) () ((5 6 7)))\n\
         ((1231006506 1 10 1 2 3 4) ((11 12 9 0)) ((5 6 7) (8 9 10)))\n";

    #[test]
    fn reads_the_four_fields_the_driver_wants() {
        assert_eq!(
            parse(SAMPLE).unwrap(),
            vec![(0, 9, vec![], 1), (1, 10, vec![inp(9, 12)], 2)]
        );
    }

    /// Digits are folded eight at a time with a scalar remainder, so the widths
    /// that matter are the ones either side of eight and either side of sixteen.
    /// Position 2 is the tx id, which is read; everything around it is skipped,
    /// so this pins both paths at once.
    ///
    /// Nineteen 9s is where this has to stop — twenty of them is larger than
    /// `usize::MAX`, so the width-20 case is the neighbouring test's business.
    #[test]
    fn reads_fields_of_every_width_around_the_eight_digit_stride() {
        for width in 1..=19usize {
            let wide = "9".repeat(width);
            let text = format!("((1 7 {} 1 2 3 4) () ((5 6 7)))", wide);
            let expected: usize = wide.parse().unwrap();
            assert_eq!(
                parse(&text).unwrap(),
                vec![(7, expected, vec![], 1)],
                "width {}",
                width
            );
        }
    }

    /// `usize::MAX` is 20 digits, so it exercises two full chunks plus a
    /// four-digit remainder and lands exactly on the limit.
    #[test]
    fn reads_the_largest_usize_and_refuses_the_next_one() {
        let text = format!("((1 7 {} 1 2 3 4) () ((5 6 7)))", usize::MAX);
        assert_eq!(parse(&text).unwrap(), vec![(7, usize::MAX, vec![], 1)]);

        let text = "((1 7 18446744073709551616 1 2 3 4) () ((5 6 7)))";
        let e = parse(text).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(e.to_string().contains("does not fit in a usize"), "{}", e);
    }

    /// A field nobody reads is no longer converted, so it is no longer checked
    /// for fitting in a `usize` — see [`Reader::skip_int`].  This is the one
    /// behaviour the skipping gives up, so it is pinned deliberately rather than
    /// left to be discovered.
    #[test]
    fn a_skipped_field_too_wide_for_a_usize_is_accepted() {
        let text = "((1 7 9 99999999999999999999999999 2 3 4) () ((5 6 7)))";
        assert_eq!(parse(text).unwrap(), vec![(7, 9, vec![], 1)]);
    }

    /// The reader is whitespace agnostic: `(read)` in the Scheme original is,
    /// and the sample files' one-record-per-line is a convention, not a rule.
    #[test]
    fn a_record_may_be_split_across_lines() {
        let text = "((1231006506\n  1 10\n  1 2 3 4)\n ((11 12 9 0))\n ((5 6 7)\n  (8 9 10)))\n";
        assert_eq!(parse(text).unwrap(), vec![(1, 10, vec![inp(9, 12)], 2)]);
    }

    /// Fields past the ones named are skipped, and the bitmask stops at 32
    /// positions, so a header longer than that must still give up its block id
    /// and tx id.
    #[test]
    fn extra_trailing_fields_are_ignored_however_many() {
        let padding = (0..60).map(|i| format!(" {}", i)).collect::<String>();
        let text = format!("((1 7 9{}) () ((5 6 7)))", padding);
        assert_eq!(parse(&text).unwrap(), vec![(7, 9, vec![], 1)]);
    }

    #[test]
    fn several_inputs_arrive_in_order() {
        let text = "((1 2 3 0 0 0 0) ((0 0 40 0) (0 0 41 1) (0 0 42 2)) ((1 2 3)))";
        assert_eq!(parse(text).unwrap(), vec![(2, 3, vec![inp(40, 0), inp(41, 0), inp(42, 0)], 1)]);
    }

    #[test]
    fn no_outputs_is_a_record_too() {
        let text = "((1 2 3 0 0 0 0) ((0 0 40 0)) ())";
        assert_eq!(parse(text).unwrap(), vec![(2, 3, vec![inp(40, 0)], 0)]);
    }

    #[test]
    fn empty_input_is_no_records_rather_than_an_error() {
        assert_eq!(parse("").unwrap(), vec![]);
        assert_eq!(parse("  \n\t ").unwrap(), vec![]);
    }

    /// Both the read path and the skip path have to refuse these, and the two
    /// checks are written out separately in each, so both get a case.  Position
    /// 2 is read; position 3 is skipped.
    #[test]
    fn refuses_a_negative_field_read_or_skipped() {
        for text in [
            "((1 7 -9 1 2 3 4) () ((5 6 7)))",
            "((1 7 9 -1 2 3 4) () ((5 6 7)))",
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.to_string().contains("negative integer"), "{}", e);
        }
    }

    #[test]
    fn refuses_a_field_that_is_not_a_number_read_or_skipped() {
        for text in [
            "((1 7 x 1 2 3 4) () ((5 6 7)))",
            "((1 7 9 x 2 3 4) () ((5 6 7)))",
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.to_string().contains("expected an integer"), "{}", e);
        }
    }

    #[test]
    fn refuses_a_truncated_record() {
        for text in [
            "((1 7 9 1 2 3 4) () ((5 6 7))",
            "((1 7 9 1 2 3 4) () ((5 6 7)",
            "((1 7 9 1 2 3 4",
            "((1 7 9 1 2 3 4) (",
            "((1 7",
        ] {
            let e = parse(text).unwrap_err();
            assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{}: {}", text, e);
        }
    }

    /// The header must actually carry the two fields the driver names.
    #[test]
    fn refuses_a_header_too_short_to_hold_the_ids() {
        let e = parse("((1 7) () ((5 6 7)))").unwrap_err();
        assert!(e.to_string().contains("record header has 2 fields"), "{}", e);
    }

    #[test]
    fn refuses_an_input_too_short_to_hold_the_previous_tx_id() {
        let e = parse("((1 7 9 1 2 3 4) ((0 0)) ((5 6 7)))").unwrap_err();
        assert!(e.to_string().contains("input has 2 fields"), "{}", e);
    }

    /// A long run of records with fields of mixed width, read both ways.  The
    /// widths are chosen so digit runs land on every offset relative to the
    /// eight-digit stride, and the whole thing is long enough that the buffered
    /// read does several thousand records between refills while the dribbled one
    /// refills constantly.
    #[test]
    fn a_long_mixed_width_stream_reads_the_same_both_ways() {
        let mut text = String::new();
        let mut expected = Vec::new();
        for i in 0..400usize {
            let block = 10usize.pow((i % 19) as u32 / 2);
            let tx = 10usize.pow((i % 18) as u32) + i;
            let prev = 10usize.pow((i % 17) as u32) + i;
            let amount = "7".repeat(i % 19 + 1);
            text.push_str(&format!(
                "(({} {} {} {} 0 {} 0) ((3 {} {} 0) (4 {} {} 1)) ((1 {} 2) (2 {} 3)))\n",
                i, block, tx, amount, amount, amount, prev, amount, prev, amount, amount
            ));
            let paid: usize = amount.parse().unwrap();
            expected.push((block, tx, vec![inp(prev, paid), inp(prev, paid)], 2));
        }
        assert_eq!(parse(&text).unwrap(), expected);
    }
}
