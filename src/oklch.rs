//! # A perceptual colour space, and the ramp the picture is drawn in
//!
//! [`crate::image`]'s weighted pixel is a grey: `weight^(1/2.2)`, dark for a
//! block that carried most of a transaction's value and pale for one that
//! carried a trace.  Grey has 254 steps between paper and ink and the eye reads
//! perhaps thirty of them, which for a quantity whose interesting range is a
//! fraction of a percent is most of the picture thrown away.
//!
//! A colour ramp reads further, but only if it is built in a space where equal
//! steps *look* equal.  That is the whole reason this module exists and the
//! reason it is not HSL: HSL's "lightness" is not luminance and its hue circle
//! is wildly non-uniform — yellow at 60° and blue at 240° are nominally the
//! same lightness and are nothing of the sort — so a ramp built in it has
//! bands where the eye sees a step that is not there and stretches where it
//! misses steps that are.  For a picture whose whole purpose is comparing
//! magnitudes that is not a cosmetic problem.
//!
//! [Oklab](https://bottosson.github.io/posts/oklab/) is a perceptual space of
//! the CIELAB family, fitted to modern colour-matching data and cheap: two 3x3
//! matrices and a cube root each way.  `OkLCh` is its polar form — lightness,
//! chroma, hue — which is the one to build a ramp in, since those are the three
//! things a ramp wants to vary independently.
//!
//! ## Lightness carries the magnitude
//!
//! [`ramp`] varies all three, but the load is on lightness, monotonically.
//! That is what makes it readable photocopied, readable by the eight percent of
//! men with a colour deficiency, and readable at all where the picture is
//! folded so small that a cell is a sample rather than a shape.  Hue and chroma
//! come along to separate levels that lightness alone leaves adjacent; they are
//! not carrying the signal on their own.
//!
//! ## Gamut
//!
//! `OkLCh` can name colours sRGB cannot show, and most of the chroma one would
//! like at the dark end is outside it.  Clamping the channels afterwards is the
//! obvious fix and the wrong one: it shifts the hue, so a ramp meant to sweep
//! evenly gets a flat spot where several entries clamp to the same face of the
//! cube.  [`in_gamut`] instead keeps the lightness and the hue and takes the
//! chroma down until the colour fits, by bisection — the standard move, and it
//! is 256 colours computed once at startup, so its cost is nothing.

/// Entries in the ramp, which is one for every value a sample can take.
pub const RAMP_LEN: usize = 256;

/// An sRGB colour, gamma-encoded, as a PNG palette holds it.
pub type Rgb = [u8; 3];

/// `OkLCh` to sRGB: lightness in `0..=1`, chroma from 0, hue in degrees.
///
/// Answers `None` when the colour is outside what sRGB can show, which is what
/// [`in_gamut`] bisects on.
fn oklch_to_srgb(lightness: f64, chroma: f64, hue_degrees: f64) -> Option<Rgb> {
    let hue = hue_degrees.to_radians();
    let (a, b) = (chroma * hue.cos(), chroma * hue.sin());

    // Oklab to LMS, cubed back out of the cube root the forward direction takes.
    let l_ = lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_ = lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_ = lightness - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let (l, m, s) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);

    // LMS to linear sRGB.
    let linear = [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ];

    let mut out = [0u8; 3];
    for (slot, &value) in out.iter_mut().zip(&linear) {
        // Outside the cube is outside the gamut.  A hair of tolerance, because
        // the matrices are a fit and the primaries themselves land a few
        // billionths past 0 and 1.
        if !(-1e-6..=1.0 + 1e-6).contains(&value) {
            return None;
        }
        let clamped = value.clamp(0.0, 1.0);
        // The sRGB transfer function, which is a straight line near black and a
        // 2.4 power above it.
        let encoded = if clamped <= 0.003_130_8 {
            12.92 * clamped
        } else {
            1.055 * clamped.powf(1.0 / 2.4) - 0.055
        };
        *slot = (encoded * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    Some(out)
}

/// The colour at this lightness and hue with as much of `chroma` as sRGB can
/// show.
///
/// Bisection on the chroma, keeping lightness and hue exactly: sixteen steps
/// take the interval below a thousandth, which is far under a step of the
/// 8-bit channels this ends up in.  A lightness of 0 or 1 is black or white and
/// admits no chroma at all, which falls out of the same search rather than
/// needing a case.
fn in_gamut(lightness: f64, chroma: f64, hue_degrees: f64) -> Rgb {
    if let Some(rgb) = oklch_to_srgb(lightness, chroma, hue_degrees) {
        return rgb;
    }
    let (mut low, mut high) = (0.0, chroma);
    // Grey is always inside, so this is the answer if every step fails.
    let mut best = oklch_to_srgb(lightness, 0.0, hue_degrees)
        .unwrap_or([if lightness > 0.5 { 255 } else { 0 }; 3]);
    for _ in 0..16 {
        let middle = 0.5 * (low + high);
        match oklch_to_srgb(lightness, middle, hue_degrees) {
            Some(rgb) => {
                best = rgb;
                low = middle;
            }
            None => high = middle,
        }
    }
    best
}

/// The ramp, from paper to the heaviest ink, as [`RAMP_LEN`] sRGB colours.
///
/// `ramp()[0]` is what a sample of 0 draws and `ramp()[255]` what a sample of
/// 255 draws.  [`crate::image`] counts *up* to paper — a heavier pixel is a
/// smaller sample, so that the union of two transactions is the smaller of
/// their samples and a row can be blanked to all 1s — so index 255 is the paper
/// and index 0 the heaviest ink.  The ramp is written in that direction.
///
/// What it does between them:
///
/// - **lightness** falls from very nearly white to a dark that still has room
///   for chroma, and falls monotonically, which is the property the whole ramp
///   rests on. See the module docs.
/// - **hue** sweeps 250° of the circle and no more, from a cold blue through
///   green and gold to a warm red. It is an arc rather than the full circle
///   because the ends have to stay apart: a ramp that wraps puts its lightest
///   and darkest entries at the same hue, which is exactly the confusion it is
///   there to prevent.
/// - **chroma** rises off the paper and falls again into the dark, since
///   neither end of the lightness axis has gamut to spare. The peak sits
///   towards the heavy end, where the levels most need separating.
pub fn ramp() -> [Rgb; RAMP_LEN] {
    let mut out = [[0u8; 3]; RAMP_LEN];
    for (index, slot) in out.iter_mut().enumerate() {
        // 0 at paper, 1 at the heaviest ink -- the reverse of the index.
        let t = (RAMP_LEN - 1 - index) as f64 / (RAMP_LEN - 1) as f64;

        let lightness = 0.985 - 0.66 * t;
        let hue = 250.0 - 250.0 * t.powf(0.85);
        // Zero at the paper end so the lightest entries are true neutrals, and
        // eased off again into the dark where there is no gamut for it.
        let chroma = 0.16 * (t.powf(0.55)) * (1.0 - 0.45 * t * t);

        *slot = in_gamut(lightness, chroma, hue);
    }
    out
}

/// The colour of a transaction, from the three numbers `--moments` prints.
///
/// Used by `examples/tint.rs` rather than by the driver, which draws pictures
/// pixel-per-block through [`ramp`] instead; a binary crate cannot see an
/// example's use of its modules, hence the allow.
///
/// A colour in this crate has always been a distribution over block ids, and
/// this is the obvious picture of one: *when* the coins came from, and *how
/// concentrated* that was.
///
/// - **hue** carries the mean block over a 250 degree arc — early chain cold,
///   late chain warm.  An arc rather than the circle so that block 0 and the
///   chain tip do not land on the same hue, which is the one confusion a
///   cyclic channel invites.
/// - **chroma** carries the concentration, as `1 / (1 + spread / SPREAD_HALF)`.
///   A coinbase has a spread of zero and comes out fully saturated; a colour
///   smeared across a third of chain history comes out nearly grey.  Mixing
///   literally desaturates, which is the property worth having: a washed-out
///   pixel is a transaction whose coins have been through everything.
/// - **lightness** is held constant, so neither of the two things being shown
///   is confounded with it and the picture is readable at any size.
///
/// Gamut-mapped by [`in_gamut`], so a saturated hue keeps its hue.
#[allow(dead_code)]
pub fn tint(mean_block: f64, spread_blocks: f64, chain_blocks: f64) -> Rgb {
    /// The spread, in blocks, at which half the chroma is gone.  About 6% of
    /// the 2022 chain: measured colours run from 0 for a coinbase to some
    /// 260,000 at the tip, so this puts the interesting range across the whole
    /// saturation axis rather than crushing it at one end.
    const SPREAD_HALF: f64 = 45_000.0;
    const LIGHTNESS: f64 = 0.72;
    const CHROMA: f64 = 0.15;
    /// Cold to warm, stopping short of a full turn so the ends stay apart.
    const HUE_FROM: f64 = 250.0;
    const HUE_ARC: f64 = 250.0;

    let where_ = if chain_blocks > 0.0 {
        (mean_block / chain_blocks).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let concentration = 1.0 / (1.0 + spread_blocks.max(0.0) / SPREAD_HALF);
    in_gamut(LIGHTNESS, CHROMA * concentration, HUE_FROM - HUE_ARC * where_)
}

/// `#rrggbb`, which is what a colour is usually wanted as.
#[allow(dead_code)]
pub fn hex(rgb: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Relative luminance, which is what "lighter" means to an eye and what the
    /// ramp has to be monotone in.  Not the mean of the channels: green carries
    /// most of the luminance and blue almost none.
    fn luminance(rgb: Rgb) -> f64 {
        let linear = |c: u8| {
            let c = c as f64 / 255.0;
            if c <= 0.040_45 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
    }

    /// The property the ramp rests on: heavier is darker, every single step.
    /// Without it the picture cannot be read photocopied, by a colour-deficient
    /// eye, or where it is folded small enough that a cell is one sample.
    #[test]
    fn the_ramp_darkens_monotonically_from_paper_to_ink() {
        let ramp = ramp();
        for index in (1..RAMP_LEN).rev() {
            let lighter = luminance(ramp[index]);
            let darker = luminance(ramp[index - 1]);
            assert!(
                darker <= lighter + 1e-9,
                "entry {} is lighter than {}: {:?} against {:?}",
                index - 1,
                index,
                ramp[index - 1],
                ramp[index]
            );
        }
        // And it actually travels: paper to ink is most of the way down.
        assert!(luminance(ramp[RAMP_LEN - 1]) > 0.9, "the paper end is nearly white");
        assert!(luminance(ramp[0]) < 0.1, "the ink end is properly dark");
    }

    /// Adjacent entries have to differ, or the ramp is not using the levels it
    /// has and the picture cannot show what the samples distinguish.
    #[test]
    fn the_ramp_spends_the_levels_it_has() {
        let ramp = ramp();
        let distinct: std::collections::HashSet<Rgb> = ramp.iter().copied().collect();
        assert!(
            distinct.len() > 240,
            "only {} of {} entries are distinct",
            distinct.len(),
            RAMP_LEN
        );
    }

    /// What [`in_gamut`] owes its caller, in both directions.
    ///
    /// The loop this replaces asserted that every channel of every entry was
    /// `<= 255` -- of a `u8`, so it was true of any three bytes whatever and
    /// the compiler said so (`comparison is useless due to type limits`).  It
    /// would have passed for a ramp of pure grey, for a ramp of one repeated
    /// colour, and for an `in_gamut` that ignored its arguments.  These are the
    /// properties it was meant to be about.
    #[test]
    fn the_gamut_mapping_only_moves_what_it_has_to() {
        // A colour that fits is returned untouched: the mapping must not cost
        // chroma it did not have to.
        for &(l, c, h) in &[(0.6, 0.02, 30.0), (0.5, 0.05, 150.0), (0.7, 0.01, 250.0)] {
            let direct = oklch_to_srgb(l, c, h).expect("chosen to be inside sRGB");
            assert_eq!(
                in_gamut(l, c, h),
                direct,
                "an in-gamut colour was moved anyway: L={} C={} h={}",
                l,
                c,
                h
            );
        }

        // And a colour that does not fit is brought in without losing what it
        // was: at full chroma the dark end of this ramp is outside sRGB, so a
        // build whose `in_gamut` silently answered grey would pass every other
        // assertion in this file and fail here.
        assert!(
            oklch_to_srgb(0.35, 0.16, 30.0).is_none(),
            "this test is only about anything if that colour is out of gamut"
        );
        let mapped = in_gamut(0.35, 0.16, 30.0);
        assert!(
            mapped[0] > mapped[2],
            "a warm hue has to stay warm through the gamut mapping: {:?}",
            mapped
        );
    }

    /// The ramp has to be coloured.  Lightness carries the magnitude, but the
    /// whole reason for a palette over a grey is the chroma beside it, and
    /// nothing else here would notice if the gamut mapping took all of it.
    #[test]
    fn the_ramp_is_not_a_row_of_greys() {
        let ramp = ramp();
        let spread = |c: Rgb| {
            let (hi, lo) = (
                c.iter().copied().max().unwrap() as i32,
                c.iter().copied().min().unwrap() as i32,
            );
            hi - lo
        };
        let coloured = ramp.iter().filter(|&&c| spread(c) > 24).count();
        assert!(
            coloured > RAMP_LEN / 2,
            "only {} of {} entries carry real chroma",
            coloured,
            RAMP_LEN
        );
        // The paper end is deliberately neutral, so that is where the greys
        // belong and nowhere else.
        assert!(
            spread(ramp[RAMP_LEN - 1]) <= 8,
            "the paper end should be very nearly neutral: {:?}",
            ramp[RAMP_LEN - 1]
        );
    }

    /// The corners, against values that can be checked by hand: Oklab's
    /// lightness is 0 at black and 1 at white, with no chroma at either.
    #[test]
    fn the_ends_of_the_lightness_axis_are_black_and_white() {
        assert_eq!(oklch_to_srgb(0.0, 0.0, 0.0), Some([0, 0, 0]));
        assert_eq!(oklch_to_srgb(1.0, 0.0, 0.0), Some([255, 255, 255]));
    }

    /// The two things a tint is meant to show, shown separately.
    #[test]
    fn a_tint_reads_when_as_hue_and_how_mixed_as_chroma() {
        let chain = 762_261.0;
        let spread_of = |c: Rgb| {
            let (hi, lo) = (
                c.iter().copied().max().unwrap() as i32,
                c.iter().copied().min().unwrap() as i32,
            );
            hi - lo
        };

        // Early and late, both perfectly concentrated: different hues, cold and
        // warm, and both vivid.
        let early = tint(1_000.0, 0.0, chain);
        let late = tint(750_000.0, 0.0, chain);
        assert!(early[2] > early[0], "the early chain is cold: {:?}", early);
        assert!(late[0] > late[2], "the late chain is warm: {:?}", late);
        assert!(spread_of(early) > 24 && spread_of(late) > 24, "both are vivid");

        // The same instant in history, thoroughly mixed: the hue survives but
        // the colour washes out.
        let mixed = tint(750_000.0, 300_000.0, chain);
        assert!(
            spread_of(mixed) < spread_of(late),
            "mixing has to desaturate: {:?} against {:?}",
            mixed,
            late
        );
        assert!(mixed[0] >= mixed[2], "and it must not change which way it leans");
    }

    /// A coinbase is the extreme case and the one a reader will check by eye:
    /// one block, no spread, so as vivid as the ramp goes.
    #[test]
    fn a_coinbase_is_the_most_saturated_thing_in_the_picture() {
        let chain = 762_261.0;
        let pure = tint(400_000.0, 0.0, chain);
        for spread in [1_000.0, 10_000.0, 100_000.0] {
            let washed = tint(400_000.0, spread, chain);
            let vivid = |c: Rgb| {
                c.iter().copied().max().unwrap() as i32 - c.iter().copied().min().unwrap() as i32
            };
            assert!(
                vivid(pure) >= vivid(washed),
                "spread {} should not be more saturated than none",
                spread
            );
        }
    }

    #[test]
    fn hex_is_six_digits() {
        assert_eq!(hex([0, 0, 0]), "#000000");
        assert_eq!(hex([255, 255, 255]), "#ffffff");
        assert_eq!(hex([1, 171, 205]), "#01abcd");
    }

    /// A hue is a direction and the colour has to point that way: a hue of 30°
    /// is warm and one of 250° is cold, whatever the gamut mapping does to the
    /// chroma on the way.
    #[test]
    fn hue_survives_the_conversion() {
        let warm = in_gamut(0.6, 0.1, 30.0);
        let cold = in_gamut(0.6, 0.1, 250.0);
        assert!(warm[0] > warm[2], "30 degrees is red-ish: {:?}", warm);
        assert!(cold[2] > cold[0], "250 degrees is blue-ish: {:?}", cold);
    }
}
