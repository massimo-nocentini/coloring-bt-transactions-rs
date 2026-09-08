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
//! ## The merge's kernels
//!
//! The rest of this file serves the weighted merge in [`crate::weighted`]:
//! the elementwise scaling loops that the compiler vectorises on its own
//! ([`scale_into_uninit`], [`scale_add_into_uninit`], and their initialised
//! twins that the tests hold them against) and its two run finders
//! ([`leading_below`], [`common_prefix`]).  [`common_prefix`] is the file's
//! second hand-written kernel and, like [`digit_run`], it is written twice —
//! NEON and SSE2 — because both are baseline and neither needs dispatch.
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

use std::mem::MaybeUninit;

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
///
/// Since the merge started writing into fresh allocations it goes through
/// [`scale_into_uninit`], and this is the plain twin the tests hold that one
/// against — hence the allowance below outside of test builds.
#[cfg_attr(not(test), allow(dead_code))]
#[inline]
pub fn scale_into(dst: &mut [f64], src: &[f64], factor: f64) {
    for (out, &value) in dst.iter_mut().zip(src) {
        *out = value * factor;
    }
}

/// `dst[k] = a[k] * fa + b[k] * fb`, for as many elements as all three hold.
///
/// The blocks that both colors carry, once the merge has lined them up.  Same
/// reasoning as [`scale_into`]: written plainly so the vectoriser can take it.
///
/// It does *not* fuse into a multiply-add, and the doc used to say it did.
/// aarch64 has `fmla` and LLVM declines to use it: contracting `x * fa + y * fb`
/// changes the rounding, and nothing in this crate turns on the fast-math flag
/// that would licence that.  The cross-built aarch64 binary of 2026-09-07
/// (rustc 1.98.0, `--release`, `aarch64-unknown-linux-gnu`) contains zero `fmla`
/// and zero `fmls` in its whole text, jemalloc and all — 0 matches in 203,593
/// disassembled lines.  On the default x86-64 target the question does not
/// arise; that is plain SSE2 and has no FMA to fuse into.  What the vectoriser
/// does take is the separate two-lane multiply and add, which is what
/// `make asm-check` counts.
#[cfg_attr(not(test), allow(dead_code))]
#[inline]
pub fn scale_add_into(dst: &mut [f64], a: &[f64], fa: f64, b: &[f64], fb: f64) {
    for ((out, &x), &y) in dst.iter_mut().zip(a).zip(b) {
        *out = x * fa + y * fb;
    }
}

/// [`scale_into`] writing into memory that has not been initialised yet.
///
/// The merge in [`crate::weighted`] used to build into a zero-filled staging
/// buffer and copy the result into its final allocation, which at depth in the
/// 2022 chain was half its time: the zero fill, the copy, and a `malloc` per
/// colour.  Writing straight into the fresh allocation needs a kernel that is
/// allowed to see uninitialised destination memory, which is the whole
/// difference between this and [`scale_into`] — the loop body is the same store
/// of the same product and vectorises the same way.  Every element of `dst` that
/// is covered by `src` is initialised on return.
#[inline]
pub fn scale_into_uninit(dst: &mut [MaybeUninit<f64>], src: &[f64], factor: f64) {
    for (out, &value) in dst.iter_mut().zip(src) {
        out.write(value * factor);
    }
}

/// [`scale_add_into`] writing into uninitialised memory, as
/// [`scale_into_uninit`] does for [`scale_into`].
#[inline]
pub fn scale_add_into_uninit(
    dst: &mut [MaybeUninit<f64>],
    a: &[f64],
    fa: f64,
    b: &[f64],
    fb: f64,
) {
    for ((out, &x), &y) in dst.iter_mut().zip(a).zip(b) {
        out.write(x * fa + y * fb);
    }
}

/// How many leading elements `a` and `b` have in common, position for position.
///
/// The merge's other run finder.  Two colours that share an ancestor share whole
/// stretches of its support, and once the merge has lined one up it has to find
/// where it ends: a scalar loop does that a block at a time, one dependent
/// compare per element, and at the depths where colours run to a hundred
/// thousand blocks that loop was a fifth of the merge.  Four `u32` lanes at a
/// time is the same compare four times wider, and finding the first lane that
/// differs is one mask and a count of trailing bits.
///
/// Until 2026-09-07 only x86-64 had a kernel here and aarch64 fell through to
/// [`common_prefix_scalar`] — the paragraph above says what that costs, and it
/// said it while one of the two targets was paying it.  `common_prefix_neon`
/// closes that; it is the arm the tests at the bottom of this file exercise
/// under `qemu-aarch64`.
#[inline]
pub fn common_prefix(a: &[u32], b: &[u32]) -> usize {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is baseline for every aarch64 target; the kernel reads
        // only within both slices.
        unsafe { common_prefix_neon(a, b) }
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is baseline for every x86-64 target, as above.
        unsafe { common_prefix_sse2(a, b) }
    }
    #[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
    {
        common_prefix_scalar(a, b)
    }
}

/// The oracle for [`common_prefix`], and its tail.
#[inline]
pub fn common_prefix_scalar(a: &[u32], b: &[u32]) -> usize {
    let n = a.len().min(b.len());
    let mut i = 0;
    while i < n && a[i] == b[i] {
        i += 1;
    }
    i
}

/// `common_prefix` on SSE2: four lanes compared for equality, a mask of the
/// lanes that matched, and the first zero bit of the mask is where the prefix
/// ends.  Two 16-byte loads per step against the eight bytes a scalar step
/// compares.
#[cfg(target_arch = "x86_64")]
#[inline]
unsafe fn common_prefix_sse2(a: &[u32], b: &[u32]) -> usize {
    use core::arch::x86_64::*;

    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 4 <= n {
        let x = _mm_loadu_si128(a.as_ptr().add(i) as *const __m128i);
        let y = _mm_loadu_si128(b.as_ptr().add(i) as *const __m128i);
        // One bit per byte, set where the bytes agree; a lane agrees when all
        // four of its bits do.
        let same = _mm_movemask_epi8(_mm_cmpeq_epi32(x, y)) as u32;
        if same != 0xFFFF {
            return i + (same.trailing_ones() >> 2) as usize;
        }
        i += 4;
    }
    i + common_prefix_scalar(&a[i..], &b[i..])
}

/// `common_prefix` on NEON.
///
/// `vceqq_u32` fills each of the four lanes with all ones where the two words
/// agree, and then the mask has to come back out of the vector register the
/// same way `digit_run_neon` gets its own out: aarch64 has no `movemask`, so
/// `vshrn` by 4 narrows the eight `u16` halves of the register into the eight
/// bytes of one `u64`, one nibble per byte of the input.
///
/// The trap is the divisor.  The nibbles count *input bytes*, and a `u32` lane
/// is four of them, so a lane that matches sets sixteen bits here where a
/// matching byte in `digit_run_neon` sets four: the first differing lane is at
/// `trailing_ones() / 16`, not `/ 4`.  Copying either twin's `>> 2` across
/// unchanged reports four times the answer and the merge reads past the run.
///
/// Reading the matches rather than the mismatches, and so `trailing_ones`
/// against `u64::MAX` rather than `trailing_zeros` against zero, is a deliberate
/// copy of `common_prefix_sse2`: it saves the `vmvnq_u32` that inverting the
/// mask would cost, and the two kernels then read as the same kernel twice.
///
/// ## What the tests actually pin, and what they do not
///
/// Nine mutants of this function, each built and run against the four
/// `common_prefix` tests under `qemu-aarch64` on 2026-09-07:
///
/// ```text
///   trailing_ones() >> 2, the SSE2 divisor      3 of 4 tests fail
///   trailing_ones() >> 3                        3 of 4 tests fail
///   trailing_zeros() vs 0, not ones vs MAX      3 of 4 tests fail
///   i += 8 in place of i += 4                   4 of 4 tests fail
///   vceqq_u8 in place of vceqq_u32              all pass
///   vshrn_n_u16::<1>, ::<2>, ::<8> for ::<4>    all pass
///   vshrn_n_u16::<9> and ::<16>                 do not compile
/// ```
///
/// The survivors survive for a reason rather than for want of a test, and both
/// reasons are worth knowing.
///
/// The shift amount is inert.  After a compare every lane is `0x0000` or
/// `0xFFFF`, so any narrow leaves the truncated byte `0x00` or `0xFF` — and
/// `vshrn_n_u16` accepts only 1 through 8, so *every legal* argument gives this
/// function the same behaviour.  LLVM sees it too: the emitted instruction is
/// `xtn v0.8b, v0.8h`, a plain narrow with no shift at all.
///
/// The compare width is inert for a different reason: byte equality decides
/// word equality here, because the first differing byte and the first differing
/// word are the same quotient by four, which is the divisor already being
/// applied.  `::<4>` and `vceqq_u32` are kept because they say what is meant —
/// but no test distinguishes them from the alternatives, since the alternatives
/// are not behavioural differences, and claiming the tests pin them would be
/// worse than writing this down.
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn common_prefix_neon(a: &[u32], b: &[u32]) -> usize {
    use core::arch::aarch64::*;

    let n = a.len().min(b.len());
    let mut i = 0;
    while i + 4 <= n {
        let x = vld1q_u32(a.as_ptr().add(i));
        let y = vld1q_u32(b.as_ptr().add(i));
        // Sixteen set bits per lane that agrees, zero per lane that does not.
        let same = vget_lane_u64::<0>(vreinterpret_u64_u8(vshrn_n_u16::<4>(
            vreinterpretq_u16_u32(vceqq_u32(x, y)),
        )));
        if same != u64::MAX {
            return i + (same.trailing_ones() >> 4) as usize;
        }
        i += 4;
    }
    i + common_prefix_scalar(&a[i..], &b[i..])
}

/// How many leading elements of the ascending `xs` are below `key`.
///
/// The merge asks this of the side that is behind: everything up to the other
/// side's next block is a run to copy whole.  A binary search over the rest of
/// the array answers in `log(remaining)` probes, but those probes land far ahead
/// of the position the merge is streaming through, so each is a cache miss on a
/// colour that does not fit in L2 — and most runs are short.  Galloping probes
/// at 1, 2, 4, ... from the current position instead, so the cost is
/// `2 log(run)` touches of memory that is about to be read anyway; only the last
/// doubling is bisected.  Same answer as `xs.partition_point(|&x| x < key)`,
/// which the tests assert.
#[inline]
pub fn leading_below(xs: &[u32], key: u32) -> usize {
    let n = xs.len();
    // `xs[..low]` are known to be below `key`; `high` is the next probe.
    let (mut low, mut high) = (0usize, 1usize);
    while high < n && xs[high] < key {
        low = high + 1;
        high *= 2;
    }
    let high = high.min(n);
    low + xs[low..high].partition_point(|&x| x < key)
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

    /// The uninitialised-destination kernels are the initialised ones with a
    /// different signature, and this is what says so: same inputs, same bits
    /// out, at lengths either side of the vector width.
    #[test]
    fn uninit_kernels_agree_with_the_plain_ones() {
        for n in 0..40usize {
            let a: Vec<f64> = (0..n).map(|k| 0.37 * k as f64 + 0.001).collect();
            let b: Vec<f64> = (0..n).map(|k| 1.0 / (k as f64 + 3.0)).collect();
            let mut plain = vec![0.0; n];
            scale_into(&mut plain, &a, 0.731);
            let mut fresh: Vec<f64> = Vec::with_capacity(n);
            scale_into_uninit(fresh.spare_capacity_mut(), &a, 0.731);
            // SAFETY: `scale_into_uninit` wrote every one of the `n` elements.
            unsafe { fresh.set_len(n) };
            assert_eq!(plain, fresh, "scale, n = {}", n);

            let mut plain = vec![0.0; n];
            scale_add_into(&mut plain, &a, 0.731, &b, 0.269);
            let mut fresh: Vec<f64> = Vec::with_capacity(n);
            scale_add_into_uninit(fresh.spare_capacity_mut(), &a, 0.731, &b, 0.269);
            // SAFETY: as above.
            unsafe { fresh.set_len(n) };
            assert_eq!(plain, fresh, "scale_add, n = {}", n);
        }
    }

    /// Every prefix length through every offset within and across the 4-lane
    /// stride, with the mismatch placed at each, against the scalar oracle.
    #[test]
    fn common_prefix_matches_scalar_at_every_length_and_offset() {
        for len in 0..40usize {
            for extra in 0..12usize {
                let a: Vec<u32> = (0..len + extra).map(|k| 3 * k as u32).collect();
                let mut b = a.clone();
                if extra > 0 {
                    // Differ at exactly `len`, so the answer must be `len`.
                    b[len] += 1;
                }
                for offset in 0..5usize.min(a.len() + 1) {
                    let want = common_prefix_scalar(&a[offset..], &b[offset..]);
                    assert_eq!(
                        common_prefix(&a[offset..], &b[offset..]),
                        want,
                        "len {} extra {} offset {}",
                        len,
                        extra,
                        offset
                    );
                }
            }
        }
    }

    /// Slices of different lengths stop at the shorter one, and equal slices
    /// answer their whole length.
    #[test]
    fn common_prefix_stops_at_the_shorter_side() {
        let a: Vec<u32> = (0..23).collect();
        assert_eq!(common_prefix(&a, &a[..7]), 7);
        assert_eq!(common_prefix(&a[..7], &a), 7);
        assert_eq!(common_prefix(&a, &a), 23);
        assert_eq!(common_prefix(&a, &[]), 0);
        assert_eq!(common_prefix(&[], &[]), 0);
    }

    /// Differences finer than a lane: the two arrays differ in exactly one
    /// *byte* of exactly one word, at every position through several 4-lane
    /// blocks and at each of the four bytes of the word.  A word whose bytes
    /// mostly agree is the case a vector compare could get wrong at some
    /// granularity between byte and word, and the merge sees it constantly —
    /// consecutive block ids differ only in their low byte.  It is one of the
    /// three tests that the wrong nibbles-per-lane divisor fails; the mutant
    /// table on `common_prefix_neon` says which mutants no test catches.
    #[test]
    fn common_prefix_finds_a_single_differing_byte() {
        let a: Vec<u32> = (0..24).map(|k| 0x0101_0101 * (k + 1)).collect();
        for len in 0..24usize {
            for byte in 0..4u32 {
                let mut b = a.clone();
                b[len] ^= 1 << (8 * byte);
                assert_eq!(
                    common_prefix(&a, &b),
                    len,
                    "differing byte {} of word {}",
                    byte,
                    len
                );
                assert_eq!(common_prefix(&a, &b), common_prefix_scalar(&a, &b));
            }
        }
    }

    #[test]
    fn common_prefix_matches_scalar_on_random_arrays() {
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..3000 {
            let n = (next() % 70) as usize;
            let a: Vec<u32> = (0..n).map(|_| (next() % 4) as u32).collect();
            // Mostly equal, so the prefixes are long enough to cross lanes.
            let b: Vec<u32> = a
                .iter()
                .map(|&x| if next() % 11 == 0 { x + 1 } else { x })
                .collect();
            assert_eq!(common_prefix(&a, &b), common_prefix_scalar(&a, &b));
        }
    }

    /// The gallop is a different search with the same answer as
    /// `partition_point`, on every key against every length of an ascending
    /// array with gaps, so that keys fall both on and between elements.
    #[test]
    fn leading_below_matches_partition_point() {
        for n in 0..70usize {
            let xs: Vec<u32> = (0..n).map(|k| 2 * k as u32 + 1).collect();
            for key in 0..(2 * n as u32 + 4) {
                assert_eq!(
                    leading_below(&xs, key),
                    xs.partition_point(|&x| x < key),
                    "n {} key {}",
                    n,
                    key
                );
            }
        }
    }

    #[test]
    fn leading_below_matches_partition_point_on_random_arrays() {
        let mut state = 0x3C6E_F372_FE94_F82Bu64;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..3000 {
            let n = (next() % 300) as usize;
            let mut xs: Vec<u32> = (0..n).map(|_| (next() % 1000) as u32).collect();
            xs.sort_unstable();
            xs.dedup();
            let key = (next() % 1010) as u32;
            assert_eq!(leading_below(&xs, key), xs.partition_point(|&x| x < key));
        }
    }
}
