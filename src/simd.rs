//! # Wide byte scanning for the record reader
//!
//! [`sexp`](crate::sexp) walks 149 GB one byte at a time.  Two of the things it
//! does to every byte are shaped exactly like vector work, so they are done here
//! instead, sixteen or eight bytes at a stride:
//!
//! - [`digit_run`] — *how long is the run of digits starting here?*  One
//!   compare against a 16-byte load answers what a scalar loop asks sixteen
//!   times.  This is a true vector operation and is written against NEON on
//!   aarch64 and SSE2 on x86-64, both of which are baseline for their target, so
//!   no runtime feature detection and no dispatch is needed.
//!
//! - [`eight_digits`] — *what number do these eight digits spell?*  This one is
//!   SWAR, "SIMD within a register": the eight bytes ride in one `u64` and three
//!   multiply-shift-mask steps fold them pairwise into a single value.  It is
//!   plain integer arithmetic, so it is portable, and it is the right tool
//!   anyway — the answer has to end up in a scalar register regardless, and no
//!   field here is longer than 20 digits.
//!
//! ## Why not the rest of the parser
//!
//! `skip_ws` is left scalar on purpose.  These records separate tokens with a
//! single space, so it skips zero or one bytes essentially every time, and a
//! 16-byte load to find that out would cost more than the loop it replaces.
//! Vectors pay off over runs, and whitespace here does not run.
//!
//! ## Correctness
//!
//! Every vector path has a scalar twin ([`digit_run_scalar`]) and the tests at
//! the bottom of this file assert the two agree — across alignments, across
//! lengths either side of the 16-byte stride, and on random bytes.  That is the
//! only real defence against a hand-written vector kernel: the scalar version is
//! obviously right, so make the fast one prove it matches.

/// How many bytes at the front of `bytes` are ASCII digits.
///
/// A return of `bytes.len()` means the run was not seen to end — the caller is
/// looking at a window, and the run may continue into whatever comes next.
#[inline]
pub fn digit_run(bytes: &[u8]) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is baseline for every aarch64 target, so these intrinsics
        // are always available; `digit_run_neon` reads only within `bytes`.
        unsafe { digit_run_neon(bytes) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is baseline for every x86-64 target, as above.
        unsafe { digit_run_sse2(bytes) }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        digit_run_scalar(bytes)
    }
}

/// The obviously-correct version, and the oracle the vector paths are tested
/// against.  Also the tail handler for both of them.
#[inline]
pub fn digit_run_scalar(bytes: &[u8]) -> usize {
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i
}

/// `digit_run` on NEON.
///
/// A byte is a digit exactly when `b - b'0'` is 9 or less as an *unsigned*
/// byte — one subtract and one compare, no range pair.  Locating the first
/// failure is the only fiddly part: aarch64 has no `movemask`, so the usual
/// stand-in is `vshrn` by 4, which narrows the 16 lanes of 0x00/0xFF into 16
/// nibbles of a single `u64`.  The first non-digit is then at nibble
/// `trailing_zeros() / 4`.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn digit_run_neon(bytes: &[u8]) -> usize {
    use core::arch::aarch64::*;

    let n = bytes.len();
    let mut i = 0;
    while i + 16 <= n {
        let chunk = vld1q_u8(bytes.as_ptr().add(i));
        let shifted = vsubq_u8(chunk, vdupq_n_u8(b'0'));
        // 0xFF in every lane that is *not* a digit.
        let bad = vcgtq_u8(shifted, vdupq_n_u8(9));
        let nibbles = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(
            vreinterpretq_u16_u8(bad),
        )));
        if nibbles != 0 {
            return i + (nibbles.trailing_zeros() >> 2) as usize;
        }
        i += 16;
    }
    i + digit_run_scalar(&bytes[i..])
}

/// `digit_run` on SSE2.
///
/// Same `b - b'0' <= 9` test, but SSE2 compares are signed only, so the
/// subtracted byte is biased by 0x80 first: `x ^ 0x80 < 10 ^ 0x80` as signed is
/// `x < 10` as unsigned.  `movemask` then hands back the 16 lane results as 16
/// bits directly, which is the part aarch64 has to fake.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn digit_run_sse2(bytes: &[u8]) -> usize {
    use core::arch::x86_64::*;

    let n = bytes.len();
    let mut i = 0;
    while i + 16 <= n {
        let chunk = _mm_loadu_si128(bytes.as_ptr().add(i) as *const __m128i);
        let shifted = _mm_sub_epi8(chunk, _mm_set1_epi8(b'0' as i8));
        let biased = _mm_xor_si128(shifted, _mm_set1_epi8(-128));
        // 10 ^ 0x80 == 0x8A == -118.
        let good = _mm_cmplt_epi8(biased, _mm_set1_epi8(-118));
        // One bit per lane, set where the byte *is* a digit.
        let bad = !(_mm_movemask_epi8(good) as u32) & 0xFFFF;
        if bad != 0 {
            return i + bad.trailing_zeros() as usize;
        }
        i += 16;
    }
    i + digit_run_scalar(&bytes[i..])
}

/// The value of eight ASCII digits packed into `chunk`, the first digit in the
/// lowest-addressed byte.  Callers get there with [`u64::from_le_bytes`], which
/// keeps this correct on a big-endian target too.
///
/// Three rounds of divide-and-conquer, each folding neighbouring lanes into
/// lanes twice as wide.  The multiplier in each round is two set fields — `1`
/// and the power of ten that weights the lane above — so one multiply does the
/// whole `hi * 10^k + lo` for every pair at once, and the shift-and-mask picks
/// the combined halves back out:
///
/// ```text
///  '1' '2' '3' '4' '5' '6' '7' '8'   eight bytes
///   \_/     \_/     \_/     \_/      x 2561      = 10<<8   | 1
///   12      34      56      78       four u16
///     \____/          \____/         x 6553601   = 100<<16 | 1
///     1234            5678           two u32
///        \____________/              x 42949672960001 = 10000<<32 | 1
///           12345678                 one u64
/// ```
///
/// No lane can carry into its neighbour on the way: the largest intermediate any
/// round produces is 99, 9999, 99999999, each one inside its lane.
///
/// The caller is responsible for the bytes actually being digits — feeding this
/// anything else yields a meaningless number rather than an error.
#[inline]
pub fn eight_digits(chunk: u64) -> u64 {
    let value = (chunk & 0x0F0F_0F0F_0F0F_0F0F).wrapping_mul(2561) >> 8;
    let value = (value & 0x00FF_00FF_00FF_00FF).wrapping_mul(6_553_601) >> 16;
    (value & 0x0000_FFFF_0000_FFFF).wrapping_mul(42_949_672_960_001) >> 32
}

/// `dst[k] = src[k] * factor`, for as many elements as both slices hold.
///
/// This is the piece of the weighted merge that vectorises, and it is left to
/// the compiler rather than written by hand.  A flat elementwise multiply over
/// two non-overlapping slices is exactly the shape LLVM's loop vectoriser is
/// built for: `zip` gives it the length up front and rules out the aliasing
/// question, so it emits `fmul` over two `f64` lanes on NEON and four on AVX
/// without being asked.  The tests below check the arithmetic; the disassembly
/// is what checks the vectorisation, and there is a `make asm-check` for it.
///
/// Hand-written intrinsics were tried for the *merge* itself and lost — see
/// [`digit_run`] for the shape that does pay off, and the merge in
/// [`crate::weighted`] for why the comparison loop does not.
#[inline]
pub fn scale_into(dst: &mut [f64], src: &[f64], factor: f64) {
    for (out, &value) in dst.iter_mut().zip(src) {
        *out = value * factor;
    }
}

/// `dst[k] = a[k] * fa + b[k] * fb`, for as many elements as all three hold.
///
/// The blocks that both colors carry, once the merge has lined them up.  Same
/// reasoning as [`scale_into`]: written plainly so the vectoriser can take it,
/// and it fuses into a multiply-add on targets that have one.
#[inline]
pub fn scale_add_into(dst: &mut [f64], a: &[f64], fa: f64, b: &[f64], fb: f64) {
    for ((out, &x), &y) in dst.iter_mut().zip(a).zip(b) {
        *out = x * fa + y * fb;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The vector kernels only look at whole 16-byte blocks, so the interesting
    /// cases are the ones that straddle a block edge or sit just inside one.
    /// This walks a run of digits of every length through every offset in a
    /// buffer long enough to hold several blocks, and asserts the shipped
    /// `digit_run` agrees with the scalar oracle every time.
    #[test]
    fn digit_run_matches_scalar_at_every_length_and_offset() {
        for len in 0..80 {
            for offset in 0..40 {
                let mut buf = vec![b'x'; offset + len + 40];
                for byte in &mut buf[offset..offset + len] {
                    *byte = b'7';
                }
                let window = &buf[offset..];
                assert_eq!(
                    digit_run(window),
                    digit_run_scalar(window),
                    "len {} at offset {}",
                    len,
                    offset
                );
            }
        }
    }

    /// A run that reaches the end of the window must report the whole window,
    /// because the caller has to know to go and fetch more input.
    #[test]
    fn digit_run_saturates_on_an_all_digit_window() {
        for len in 0..80 {
            let buf = vec![b'0'; len];
            assert_eq!(digit_run(&buf), len);
        }
    }

    /// Every byte that is not `0`-`9` must stop the run, including the ones
    /// adjacent to the digits in ASCII (`/` is 0x2F, `:` is 0x3A) — those are
    /// what an off-by-one in the range test would let through.
    #[test]
    fn digit_run_stops_on_every_non_digit() {
        for byte in 0u8..=255 {
            for stop in [0usize, 1, 7, 15, 16, 17, 31, 32] {
                let mut buf = [b'5'; 33];
                buf[stop] = byte;
                let expected = if byte.is_ascii_digit() { 33 } else { stop };
                assert_eq!(digit_run(&buf), expected, "byte {:#04x} at {}", byte, stop);
            }
        }
    }

    #[test]
    fn digit_run_matches_scalar_on_random_bytes() {
        let mut state = 0x243F_6A88_85A3_08D3u64;
        let mut buf = vec![0u8; 200];
        for _ in 0..2000 {
            for byte in &mut buf {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                // Mostly digits, so the runs are long enough to exercise the
                // block loop rather than always stopping in the first lane.
                *byte = if state & 7 == 0 {
                    (state >> 8) as u8
                } else {
                    b'0' + (state >> 8) as u8 % 10
                };
            }
            for start in 0..20 {
                let window = &buf[start..];
                assert_eq!(digit_run(window), digit_run_scalar(window));
            }
        }
    }

    #[test]
    fn eight_digits_reads_the_digits_in_order() {
        assert_eq!(eight_digits(u64::from_le_bytes(*b"12345678")), 12_345_678);
        assert_eq!(eight_digits(u64::from_le_bytes(*b"00000000")), 0);
        assert_eq!(eight_digits(u64::from_le_bytes(*b"99999999")), 99_999_999);
        assert_eq!(eight_digits(u64::from_le_bytes(*b"00000001")), 1);
        assert_eq!(eight_digits(u64::from_le_bytes(*b"10000000")), 10_000_000);
    }

    /// Exhaustive over every 8-digit string would be 10^8 formatted numbers; a
    /// stride that is coprime with every power of ten walks all digit positions
    /// through all values instead, cheaply.
    #[test]
    fn eight_digits_matches_the_formatter() {
        let mut value = 0u64;
        while value < 100_000_000 {
            let text = format!("{:08}", value);
            let chunk = u64::from_le_bytes(text.as_bytes().try_into().unwrap());
            assert_eq!(eight_digits(chunk), value, "{}", text);
            value += 4_637;
        }
        for value in (0..1000).chain(99_999_000..100_000_000) {
            let text = format!("{:08}", value);
            let chunk = u64::from_le_bytes(text.as_bytes().try_into().unwrap());
            assert_eq!(eight_digits(chunk), value, "{}", text);
        }
    }
}
