//! # What `emit` writes
//!
//! Every record's estimate `p` (ascending blocks, `f32` values summing to one
//! within rounding), in record order, in one of two forms.
//!
//! **Text** is `--weighted`'s terms line -- `tx<TAB>w:b,w:b,...`, blocks in
//! decreasing order, each `w` the shortest text that reads back as the same
//! `f32` -- for prefixes, eyeballing and `diff`.
//!
//! **Binary** is what a whole-chain run writes, and it is shaped by two facts.
//! Block ids of a sketch are sampled from a colour hundreds of thousands of
//! blocks wide, so their gaps are close to incompressible and the only saving
//! is to write them as varint deltas.  And a `varopt` sketch's light terms all
//! carry the same weight, `tau / sum w`, so storing that weight once and then
//! the blocks that have it saves four bytes a light term -- most of a sketch.
//! The encoder does not know which variant made the record: it looks for the
//! most frequent `f32` bit pattern among the values and uses the shared form
//! when one occurs at least twice, which is lossless whatever wrote it.
//!
//! The format, integers as LEB128 varints, `z()` zigzag, floats little-endian:
//!
//! ```text
//! header:  "SKETCH01" | u32 version=1 | u8 variant (0 varopt, 1 prio, 2 coord)
//!          | u8 reduce (0 record, 1 step) | u8 cap (0 min, 1 harm) | u8 0
//!          | u32 k | u64 seed | u32 floor | u32 0
//! record:  varint z(tx - (prev_tx + 1))        prev_tx starts at u64::MAX
//!          u8 tag                              bit 0: shared form, bit 1: reduced
//!          [f32 tau | f32 (raw_mass - 1)]      only when reduced
//!          tag bit 0 clear:  varint n | n x varint db | n x f32 p
//!          tag bit 0 set:    f32 w* | varint m | m x varint db  (p bit-equal to w*)
//!                            | varint h | h x varint db | h x f32 p
//! trailer: "SKEND001" | u64 records | u64 sum over records of (tx ^ n), wrapping
//! ```
//!
//! Block deltas start from 0 in each list.  The `reduced` bit is what tells a
//! reader of the production file which colours are exact (a `false` record
//! *is* the colour up to `f32`), and with `tau` and the raw mass it is what
//! lets `check` reproduce every metric column of a `chain` run from the file
//! alone.  A trailer cannot be mistaken for a record: its `S` would be a
//! one-byte tx delta, and the `K` after it is not a valid tag.
//!
//! The output is uncompressed and meant for `zstd -T16 -12` (no `--long`: a
//! 1 GiB window needs `--long=30` again at every decoder, and -19 ran at a
//! fifteenth of -12's speed for a few percent).

use crate::algebra::{Cap, Config, Reduce, Variant};
use std::io::{self, Read};

pub const MAGIC: &[u8; 8] = b"SKETCH01";
pub const END: &[u8; 8] = b"SKEND001";
pub const VERSION: u32 = 1;
pub const TAG_SHARED: u8 = 1;
pub const TAG_REDUCED: u8 = 2;

pub fn varint(out: &mut Vec<u8>, mut x: u64) {
    while x >= 0x80 {
        out.push((x as u8) | 0x80);
        x >>= 7;
    }
    out.push(x as u8);
}

#[inline]
fn zigzag(x: i64) -> u64 {
    ((x << 1) ^ (x >> 63)) as u64
}

#[inline]
fn unzigzag(x: u64) -> i64 {
    ((x >> 1) as i64) ^ -((x & 1) as i64)
}

pub fn header(cfg: &Config) -> Vec<u8> {
    let mut h = Vec::with_capacity(40);
    h.extend_from_slice(MAGIC);
    h.extend_from_slice(&VERSION.to_le_bytes());
    h.push(match cfg.variant {
        Variant::VarOpt => 0,
        Variant::Prio => 1,
        Variant::Coord => 2,
    });
    h.push(match cfg.reduce {
        Reduce::Record => 0,
        Reduce::Step => 1,
    });
    h.push(match cfg.cap {
        Cap::Min => 0,
        Cap::Harm => 1,
    });
    h.push(0);
    h.extend_from_slice(&(cfg.k as u32).to_le_bytes());
    h.extend_from_slice(&cfg.seed.to_le_bytes());
    h.extend_from_slice(&(cfg.floor as u32).to_le_bytes());
    h.extend_from_slice(&0u32.to_le_bytes());
    h
}

/// The emitted estimate rounded to what is written: `f32`, zeros left out.
pub fn round_phat(p: &[(u32, f64)], out: &mut Vec<(u32, f32)>) {
    out.clear();
    out.extend(p.iter().map(|&(b, w)| (b, w as f32)).filter(|x| x.1 != 0.0));
}

/// What a record says besides its estimate.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Meta {
    pub reduced: bool,
    pub tau: f32,
    /// `raw_mass - 1`, as written.
    pub mass_m1: f32,
}

pub struct Encoder {
    prev_tx: u64,
    pub records: u64,
    pub checksum: u64,
    bits: Vec<u32>,
}

impl Default for Encoder {
    fn default() -> Self {
        Encoder { prev_tx: u64::MAX, records: 0, checksum: 0, bits: Vec::new() }
    }
}

fn deltas(out: &mut Vec<u8>, blocks: impl Iterator<Item = u32>) {
    let mut prev = 0u32;
    for b in blocks {
        varint(out, (b - prev) as u64);
        prev = b;
    }
}

impl Encoder {
    pub fn record(&mut self, out: &mut Vec<u8>, tx: u64, p: &[(u32, f32)], meta: Meta) {
        let delta = tx.wrapping_sub(self.prev_tx.wrapping_add(1)) as i64;
        varint(out, zigzag(delta));
        self.prev_tx = tx;
        self.records += 1;
        self.checksum = self.checksum.wrapping_add(tx ^ p.len() as u64);

        let shared = self.mode(p);
        let mut tag = 0u8;
        if shared.is_some() {
            tag |= TAG_SHARED;
        }
        if meta.reduced {
            tag |= TAG_REDUCED;
        }
        out.push(tag);
        if meta.reduced {
            out.extend_from_slice(&meta.tau.to_le_bytes());
            out.extend_from_slice(&meta.mass_m1.to_le_bytes());
        }
        match shared {
            None => {
                varint(out, p.len() as u64);
                deltas(out, p.iter().map(|x| x.0));
                for x in p {
                    out.extend_from_slice(&x.1.to_le_bytes());
                }
            }
            Some(bits) => {
                out.extend_from_slice(&bits.to_le_bytes());
                let m = p.iter().filter(|x| x.1.to_bits() == bits).count();
                varint(out, m as u64);
                deltas(out, p.iter().filter(|x| x.1.to_bits() == bits).map(|x| x.0));
                varint(out, (p.len() - m) as u64);
                deltas(out, p.iter().filter(|x| x.1.to_bits() != bits).map(|x| x.0));
                for x in p.iter().filter(|x| x.1.to_bits() != bits) {
                    out.extend_from_slice(&x.1.to_le_bytes());
                }
            }
        }
    }

    /// The most frequent `f32` bit pattern among `p`'s values, ties to the
    /// smaller, when it occurs at least twice.
    ///
    /// A `varopt` sketch is mostly lights sharing one value, so the common case
    /// is a strict majority, which Boyer-Moore's vote finds and one count
    /// confirms in two linear passes; a majority is the unique mode, so no tie
    /// rule is needed then.  Anything else is sorted.
    fn mode(&mut self, p: &[(u32, f32)]) -> Option<u32> {
        if p.len() < 2 {
            return None;
        }
        let (mut cand, mut votes) = (0u32, 0usize);
        for x in p {
            let b = x.1.to_bits();
            if votes == 0 {
                cand = b;
                votes = 1;
            } else if b == cand {
                votes += 1;
            } else {
                votes -= 1;
            }
        }
        let count = p.iter().filter(|x| x.1.to_bits() == cand).count();
        if 2 * count > p.len() {
            return Some(cand);
        }
        self.bits.clear();
        self.bits.extend(p.iter().map(|x| x.1.to_bits()));
        self.bits.sort_unstable();
        let (mut best, mut best_n) = (0u32, 0usize);
        let mut i = 0;
        while i < self.bits.len() {
            let mut j = i + 1;
            while j < self.bits.len() && self.bits[j] == self.bits[i] {
                j += 1;
            }
            if j - i > best_n {
                best = self.bits[i];
                best_n = j - i;
            }
            i = j;
        }
        (best_n >= 2).then_some(best)
    }

    pub fn trailer(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(END);
        out.extend_from_slice(&self.records.to_le_bytes());
        out.extend_from_slice(&self.checksum.to_le_bytes());
    }
}

/// `tx<TAB>w:b,...`, blocks decreasing, as `--weighted` prints its terms.
pub fn text_line(out: &mut Vec<u8>, tx: u64, p: &[(u32, f32)]) {
    use std::io::Write;
    write!(out, "{}\t", tx).unwrap();
    for (i, &(b, w)) in p.iter().rev().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        write!(out, "{}:{}", w, b).unwrap();
    }
    out.push(b'\n');
}

/// One decoded record.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Decoded {
    pub tx: u64,
    pub meta: Meta,
    pub p: Vec<(u32, f32)>,
}

/// A buffered reader of the binary form.
pub struct Decoder<R: Read> {
    inner: R,
    buf: Box<[u8]>,
    pos: usize,
    end: usize,
    prev_tx: u64,
    pub config: Config,
    pub records: u64,
    checksum: u64,
    scratch: Vec<u32>,
}

fn corrupt(what: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what)
}

impl<R: Read> Decoder<R> {
    pub fn new(inner: R) -> io::Result<Decoder<R>> {
        let mut d = Decoder {
            inner,
            buf: vec![0u8; 1 << 20].into_boxed_slice(),
            pos: 0,
            end: 0,
            prev_tx: u64::MAX,
            config: Config::parse("varopt:1").unwrap(),
            records: 0,
            checksum: 0,
            scratch: Vec::new(),
        };
        let mut h = [0u8; 36];
        d.bytes(&mut h)?;
        if &h[..8] != MAGIC {
            return Err(corrupt("not a sketch file (no SKETCH01 header)".into()));
        }
        let version = u32::from_le_bytes(h[8..12].try_into().unwrap());
        if version != VERSION {
            return Err(corrupt(format!("sketch file version {}, this reads {}", version, VERSION)));
        }
        let variant = match h[12] {
            0 => Variant::VarOpt,
            1 => Variant::Prio,
            2 => Variant::Coord,
            v => return Err(corrupt(format!("unknown variant {}", v))),
        };
        let reduce = if h[13] == 1 { Reduce::Step } else { Reduce::Record };
        let cap = if h[14] == 1 { Cap::Harm } else { Cap::Min };
        d.config = Config {
            variant,
            reduce,
            cap,
            k: u32::from_le_bytes(h[16..20].try_into().unwrap()) as usize,
            seed: u64::from_le_bytes(h[20..28].try_into().unwrap()),
            floor: u32::from_le_bytes(h[28..32].try_into().unwrap()) as usize,
        };
        Ok(d)
    }

    /// Make at least `n` bytes available if the input has them; answers how
    /// many are.
    fn fill(&mut self, n: usize) -> io::Result<usize> {
        if self.end - self.pos >= n {
            return Ok(self.end - self.pos);
        }
        self.buf.copy_within(self.pos..self.end, 0);
        self.end -= self.pos;
        self.pos = 0;
        while self.end < n {
            let got = self.inner.read(&mut self.buf[self.end..])?;
            if got == 0 {
                break;
            }
            self.end += got;
        }
        Ok(self.end - self.pos)
    }

    fn bytes(&mut self, dst: &mut [u8]) -> io::Result<()> {
        if self.fill(dst.len())? < dst.len() {
            return Err(corrupt(format!("sketch file truncated after {} records", self.records)));
        }
        dst.copy_from_slice(&self.buf[self.pos..self.pos + dst.len()]);
        self.pos += dst.len();
        Ok(())
    }

    #[inline]
    fn byte(&mut self) -> io::Result<u8> {
        if self.pos < self.end {
            let b = self.buf[self.pos];
            self.pos += 1;
            return Ok(b);
        }
        let mut b = [0u8];
        self.bytes(&mut b)?;
        Ok(b[0])
    }

    #[inline]
    fn varint(&mut self) -> io::Result<u64> {
        let mut x = 0u64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            x |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(x);
            }
            shift += 7;
            if shift > 63 {
                return Err(corrupt(format!("bad varint in record {}", self.records)));
            }
        }
    }

    fn f32(&mut self) -> io::Result<f32> {
        let mut b = [0u8; 4];
        self.bytes(&mut b)?;
        Ok(f32::from_le_bytes(b))
    }

    fn blocks(&mut self, n: usize) -> io::Result<()> {
        self.scratch.clear();
        let mut prev = 0u64;
        for _ in 0..n {
            prev += self.varint()?;
            let b = u32::try_from(prev).map_err(|_| corrupt(format!("block overflow in record {}", self.records)))?;
            self.scratch.push(b);
        }
        Ok(())
    }

    /// The next record into `rec`; `false` at the trailer, which is verified.
    pub fn next(&mut self, rec: &mut Decoded) -> io::Result<bool> {
        let avail = self.fill(2)?;
        if avail == 0 {
            return Err(corrupt(format!("sketch file ends after {} records without its trailer", self.records)));
        }
        if avail >= 2 && self.buf[self.pos] == END[0] && self.buf[self.pos + 1] == END[1] {
            let mut t = [0u8; 24];
            self.bytes(&mut t)?;
            if &t[..8] != END {
                return Err(corrupt("damaged trailer".into()));
            }
            let n = u64::from_le_bytes(t[8..16].try_into().unwrap());
            let sum = u64::from_le_bytes(t[16..24].try_into().unwrap());
            if n != self.records {
                return Err(corrupt(format!("trailer counts {} records, the file holds {}", n, self.records)));
            }
            if sum != self.checksum {
                return Err(corrupt(format!("checksum {:#x} in the trailer, {:#x} over the records", sum, self.checksum)));
            }
            return Ok(false);
        }
        let delta = unzigzag(self.varint()?);
        let tx = self.prev_tx.wrapping_add(1).wrapping_add(delta as u64);
        self.prev_tx = tx;
        let tag = self.byte()?;
        if tag > (TAG_SHARED | TAG_REDUCED) {
            return Err(corrupt(format!("bad tag {} in record {}", tag, self.records)));
        }
        rec.tx = tx;
        rec.meta = Meta::default();
        if tag & TAG_REDUCED != 0 {
            rec.meta.reduced = true;
            rec.meta.tau = self.f32()?;
            rec.meta.mass_m1 = self.f32()?;
        }
        rec.p.clear();
        if tag & TAG_SHARED == 0 {
            let n = self.varint()? as usize;
            self.blocks(n)?;
            for i in 0..n {
                let w = self.f32()?;
                rec.p.push((self.scratch[i], w));
            }
        } else {
            let w = self.f32()?;
            let m = self.varint()? as usize;
            self.blocks(m)?;
            let shared: Vec<u32> = std::mem::take(&mut self.scratch);
            let h = self.varint()? as usize;
            self.blocks(h)?;
            let mut own = Vec::with_capacity(h);
            for i in 0..h {
                own.push((self.scratch[i], self.f32()?));
            }
            // Merge the two ascending lists back into one.
            let (mut i, mut j) = (0, 0);
            while i < shared.len() || j < own.len() {
                if j >= own.len() || (i < shared.len() && shared[i] < own[j].0) {
                    rec.p.push((shared[i], w));
                    i += 1;
                } else {
                    rec.p.push(own[j]);
                    j += 1;
                }
            }
            self.scratch = shared;
        }
        self.records += 1;
        self.checksum = self.checksum.wrapping_add(tx ^ rec.p.len() as u64);
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// T11: a binary round trip through the decoder is bitwise the text written
    /// directly; both tags, a negative tx delta after a duplicate, the trailer
    /// count and the checksum.
    #[test]
    fn a_binary_round_trip_is_the_text() {
        let cfg = Config::parse("varopt:4").unwrap();
        let records: Vec<(u64, Vec<(u32, f32)>, Meta)> = vec![
            (0, vec![(0, 1.0)], Meta::default()),
            (1, vec![(3, 0.25), (9, 0.25), (12, 0.5)], Meta { reduced: true, tau: 0.25, mass_m1: -1e-7 }),
            (2, vec![(3, 0.1), (9, 0.2), (12, 0.7)], Meta::default()),
            (5, vec![], Meta::default()),
            // A duplicated id: a negative delta.
            (2, vec![(100_000, 0.5), (700_000, 0.5)], Meta { reduced: true, tau: 0.5, mass_m1: 0.0 }),
            (6, vec![(1, 1e-30), (2, 0.3), (3, 0.3), (4, 0.4 - 1e-30)], Meta::default()),
        ];
        let mut enc = Encoder::default();
        let mut bin = header(&cfg);
        let mut text = Vec::new();
        let mut tags = Vec::new();
        for (tx, p, m) in &records {
            let at = bin.len();
            enc.record(&mut bin, *tx, p, *m);
            let skip = varint_len(&bin[at..]);
            tags.push(bin[at + skip]);
            text_line(&mut text, *tx, p);
        }
        enc.trailer(&mut bin);
        assert!(tags.iter().any(|t| t & TAG_SHARED != 0) && tags.iter().any(|t| t & TAG_SHARED == 0));

        let mut dec = Decoder::new(&bin[..]).unwrap();
        assert_eq!(dec.config, cfg);
        let mut back = Vec::new();
        let mut r = Decoded::default();
        let mut i = 0;
        while dec.next(&mut r).unwrap() {
            assert_eq!(r.meta, records[i].2);
            text_line(&mut back, r.tx, &r.p);
            i += 1;
        }
        assert_eq!(i, records.len());
        assert_eq!(back, text);

        // A corrupted checksum is caught.
        let mut bad = bin.clone();
        let n = bad.len();
        bad[n - 1] ^= 1;
        let mut dec = Decoder::new(&bad[..]).unwrap();
        let mut res = Ok(true);
        while let Ok(true) = res {
            res = dec.next(&mut r);
        }
        assert!(res.is_err());
        // And so is a missing trailer.
        let cut = &bin[..bin.len() - 24];
        let mut dec = Decoder::new(cut).unwrap();
        let mut res = Ok(true);
        while let Ok(true) = res {
            res = dec.next(&mut r);
        }
        assert!(res.is_err());
    }

    fn varint_len(b: &[u8]) -> usize {
        b.iter().position(|&x| x < 0x80).unwrap() + 1
    }

    #[test]
    fn text_is_the_weighted_terms_line() {
        let mut out = Vec::new();
        text_line(&mut out, 7, &[(1, 0.25), (5, 0.75)]);
        assert_eq!(String::from_utf8(out).unwrap(), "7\t0.75:5,0.25:1\n");
    }

    #[test]
    fn the_shared_value_is_the_mode_with_ties_to_the_smaller() {
        let mut e = Encoder::default();
        let f = |v: &[f32]| v.iter().enumerate().map(|(i, &w)| (i as u32, w)).collect::<Vec<_>>();
        assert_eq!(e.mode(&f(&[0.5, 0.25, 0.25])), Some(0.25f32.to_bits()));
        assert_eq!(e.mode(&f(&[0.5, 0.25, 0.125])), None);
        // A tie between 0.5 and 0.25 goes to the smaller bit pattern.
        assert_eq!(e.mode(&f(&[0.5, 0.25, 0.5, 0.25, 0.1])), Some(0.25f32.to_bits()));
        assert_eq!(e.mode(&f(&[0.1, 0.2, 0.2, 0.3, 0.3, 0.3])), Some(0.3f32.to_bits()));
        assert_eq!(e.mode(&f(&[1.0])), None);
    }

    #[test]
    fn zigzag_round_trips() {
        for x in [0i64, 1, -1, 5, -6, i64::MAX, i64::MIN] {
            assert_eq!(unzigzag(zigzag(x)), x);
        }
    }
}
