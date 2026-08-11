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
//! Only four things are ever used: the header's `block-id` and `tx-id`, each
//! input's `prev-tx-id`, and how many outputs there are.  Fields are picked out
//! by position and the rest of each list is skipped, so a record with extra
//! trailing fields still reads — unlike the Scheme's fixed-arity `match/first`.
//! Nothing is allocated per record.
//!
//! The buffer is ours rather than a `BufReader`, because at 149 GB the hot path
//! is "look at one byte" and it needs to inline down to a bounds check.

use std::io::{self, Read};

const BUF_SIZE: usize = 1 << 20;

pub struct Record {
    pub block_id: usize,
    pub tx_id: usize,
    pub outputs: usize,
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

    /// Read one non-negative integer.  Nothing in these records is ever
    /// negative — the fields are timestamps, ids, counts and satoshi amounts —
    /// so a leading `-` is a malformed record rather than a number to accept.
    fn read_int(&mut self) -> io::Result<usize> {
        let first = match self.skip_ws()? {
            Some(b) => b,
            None => return self.err("expected an integer, found end of input".into()),
        };
        if first == b'-' {
            return self.err("negative integer, but every field here is unsigned".into());
        }

        let mut value: usize = 0;
        let mut digits = 0;
        while let Some(b) = self.peek()? {
            if !b.is_ascii_digit() {
                break;
            }
            value = match value
                .checked_mul(10)
                .and_then(|v| v.checked_add((b - b'0') as usize))
            {
                Some(v) => v,
                None => return self.err("integer does not fit in a usize".into()),
            };
            self.pos += 1;
            digits += 1;
        }
        if digits == 0 {
            return self.err(format!("expected an integer, found {:?}", first as char));
        }

        Ok(value)
    }

    /// Read `( int int ... )`, handing each element to `visit` with its index,
    /// and answer how many there were.
    fn read_flat_list(&mut self, mut visit: impl FnMut(usize, usize)) -> io::Result<usize> {
        self.expect(b'(')?;
        let mut n = 0;
        loop {
            match self.skip_ws()? {
                Some(b')') => {
                    self.pos += 1;
                    return Ok(n);
                }
                Some(_) => {
                    let v = self.read_int()?;
                    visit(n, v);
                    n += 1;
                }
                None => return self.err("unterminated list".into()),
            }
        }
    }

    /// The next record, or `None` at end of input.  `inputs` is cleared and
    /// refilled with the previous-transaction id of each input; pass the same
    /// vector every time to keep this allocation-free.
    pub fn next_record(&mut self, inputs: &mut Vec<usize>) -> io::Result<Option<Record>> {
        inputs.clear();

        if self.skip_ws()?.is_none() {
            return Ok(None);
        }
        self.expect(b'(')?;

        let (mut block_id, mut tx_id) = (None, None);
        let fields = self.read_flat_list(|i, v| match i {
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
                    let mut prev_tx_id = None;
                    let fields = self.read_flat_list(|i, v| {
                        if i == 2 {
                            prev_tx_id = Some(v)
                        }
                    })?;
                    match prev_tx_id {
                        Some(p) => inputs.push(p),
                        None => {
                            return self.err(format!(
                                "input has {} fields, expected the previous tx id at position 2",
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
                    self.read_flat_list(|_, _| {})?;
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
