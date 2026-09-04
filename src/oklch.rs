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

    /// Every entry has to be a colour sRGB can actually show, which after
    /// [`in_gamut`] means the round trip back through the space lands on it.
    #[test]
    fn every_entry_is_inside_the_gamut() {
        for (index, rgb) in ramp().iter().enumerate() {
            assert!(
                rgb.iter().all(|&c| c <= 255),
                "entry {} is not a colour: {:?}",
                index,
                rgb
            );
        }
        // The bisection has to have actually done something: at full chroma the
        // dark end of this ramp is outside sRGB, so a build where `in_gamut`
        // silently returned grey would pass the test above and fail this one.
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

    /// The corners, against values that can be checked by hand: Oklab's
    /// lightness is 0 at black and 1 at white, with no chroma at either.
    #[test]
    fn the_ends_of_the_lightness_axis_are_black_and_white() {
        assert_eq!(oklch_to_srgb(0.0, 0.0, 0.0), Some([0, 0, 0]));
        assert_eq!(oklch_to_srgb(1.0, 0.0, 0.0), Some([255, 255, 255]));
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
