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

/// The spread, in blocks, at which half the chroma is gone.  About 6% of the
/// 2022 chain: measured colours run from 0 for a coinbase to some 260,000 at
/// the tip, so this puts the interesting range across the whole saturation
/// axis rather than crushing it at one end.
pub const SPREAD_HALF: f64 = 45_000.0;

/// Held constant across every tint, so neither thing being shown is confounded
/// with lightness and the picture is readable at any size.
pub const LIGHTNESS: f64 = 0.72;

/// The chroma of a perfectly concentrated colour, before the gamut mapping.
pub const CHROMA: f64 = 0.15;

/// The hue of block 0, in degrees: cold, and the start of the arc.
pub const HUE_FROM: f64 = 250.0;

/// How far round the circle the chain runs, stopping short of a full turn so
/// the ends stay apart.
pub const HUE_ARC: f64 = 250.0;

/// How concentrated a colour is, in `(0, 1]`: 1 for a coinbase, a half at
/// [`SPREAD_HALF`], and never quite 0.
///
/// This is the mix axis, and the reason one division has a name is that
/// [`tint`], [`tint_index`] and [`tints`] must agree about it exactly, or a
/// quantised colour is not a quantisation of anything.
#[allow(dead_code)]
pub fn concentration(spread_blocks: f64) -> f64 {
    1.0 / (1.0 + spread_blocks.max(0.0) / SPREAD_HALF)
}

/// The spread [`concentration`] answers `wanted` for — its inverse, and the
/// only reason it exists is so [`tints`] can evaluate [`tint`] at a bin centre
/// rather than keep a second copy of the colour.
fn spread_at(wanted: f64) -> f64 {
    SPREAD_HALF * (1.0 / wanted.clamp(f64::MIN_POSITIVE, 1.0) - 1.0)
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
/// - **chroma** carries the [`concentration`], `1 / (1 + spread /
///   SPREAD_HALF)`.  A coinbase has a spread of zero and comes out fully
///   saturated; a colour smeared across a third of chain history comes out
///   nearly grey.  Mixing literally desaturates, which is the property worth
///   having: a washed-out pixel is a transaction whose coins have been through
///   everything.
/// - **lightness** is held constant, so neither of the two things being shown
///   is confounded with it and the picture is readable at any size.
///
/// Gamut-mapped by [`in_gamut`], so a saturated hue keeps its hue.
#[allow(dead_code)]
pub fn tint(mean_block: f64, spread_blocks: f64, chain_blocks: f64) -> Rgb {
    let where_ = if chain_blocks > 0.0 {
        (mean_block / chain_blocks).clamp(0.0, 1.0)
    } else {
        0.0
    };
    in_gamut(
        LIGHTNESS,
        CHROMA * concentration(spread_blocks),
        HUE_FROM - HUE_ARC * where_,
    )
}

/// Hue bins along the chain.  See [`tints`] for why it is 32.
pub const TINT_HUES: usize = 32;

/// Mixing levels across the [`concentration`] axis, counting *up* with mixing:
/// slot 0 is a coinbase and slot 7 a colour that has been through everything.
/// That is the direction `src/bin/tx-view.rs`'s `slot` counts in, and keeping
/// it means the two pictures cannot be read backwards against each other.
pub const TINT_MIXES: usize = 8;

/// Entries in the tint table, which has to fit in a byte.
pub const TINTS_LEN: usize = TINT_HUES * TINT_MIXES;

/// Which entry of [`tints`] stands for this transaction's colour.
///
/// The hue axis is cut into [`TINT_HUES`] equal slices of the chain and the mix
/// axis into [`TINT_MIXES`] equal slices of the [`concentration`] — *not* of
/// the spread.
///
/// Spread is the wrong thing to cut evenly, and not by a little.  The chroma is
/// proportional to `1 / (1 + spread / 45000)`, so the first eighth of a spread
/// axis running to 260,000 blocks — 0 to 32,500 — covers chroma from 0.150 down
/// to 0.087, which is *half of the whole colour axis in one bin*: a worst case
/// of 0.031 in Oklab, past the 0.02 an eye can find, before the other seven
/// bins have said anything.  The measurements agree from the other side: over
/// the first 60,000,000 records of the 2022 chain, 87.70% of transactions have
/// a spread between 16,384 and 262,144 blocks and 5.78% have none at all, so
/// even bins of spread would sort almost the whole chain into two of them.  The
/// concentration is the compressive coordinate already, and it is exactly the
/// one the chroma is linear in, so equal slices of it are equal slices of what
/// the eye is shown.
///
/// The index is `hue * TINT_MIXES + mix`, so consecutive entries are one hue at
/// eight degrees of mixing — the order [`tints`] writes them in, and the order
/// a PNG `PLTE` chunk wants them.
#[allow(dead_code)]
pub fn tint_index(mean_block: f64, spread_blocks: f64, chain_blocks: f64) -> u8 {
    let where_ = if chain_blocks > 0.0 {
        (mean_block / chain_blocks).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let hue = ((where_ * TINT_HUES as f64) as usize).min(TINT_HUES - 1);
    // Rounds down, so a concentration of exactly 1 -- which takes a spread of
    // exactly zero, nothing near it -- would fall one past the end without the
    // clamp.  That is 5.78% of the first 60,000,000 records: 3,466,043 of them
    // in the `--bands 1024 --moments` run cited below.  It is not the coinbase
    // count, which is 344,032 lines with no inputs at all, 0.5734% of the same
    // prefix; the rest of the 5.78% is every other transaction that drew all
    // of its value from a single block.  Nor is it the 6.02% whose spread is
    // under one block: the 147,561 records between the two come out at a
    // concentration just short of 1, land in the last slot unaided, and are no
    // part of what the clamp is for.
    let step = ((concentration(spread_blocks) * TINT_MIXES as f64) as usize).min(TINT_MIXES - 1);
    (hue * TINT_MIXES + (TINT_MIXES - 1 - step)) as u8
}

/// The [`TINTS_LEN`] colours a [`tint_index`] byte points at.
///
/// **This is not [`ramp`] and must not be confused with it.**  `ramp` is keyed
/// by a *weight* sample on the block axis and is what the `PLTE` of the
/// (transaction, block) picture holds; this is keyed by a *whole colour*
/// quantised on two axes, and there is no block axis in it at all.  A tint
/// index fed through `ramp` would draw a legend that lies about what every
/// pixel means, which is why this is a second table under a second name rather
/// than a second use of the first.
///
/// Every entry is [`tint`] evaluated at the centre of its bin, so there stays
/// exactly one definition of the colour: change [`HUE_ARC`] or [`LIGHTNESS`]
/// and the table moves with it, because the table is not a table — it is the
/// same arithmetic, run 256 times at startup.  It takes no `chain_blocks`,
/// because `tint` reads the mean block only as a fraction of the chain: the
/// height moves which transactions land in which bin, never what a bin looks
/// like.
///
/// ## Why 32 by 8, and what it costs
///
/// The two axes compete for one byte, and half a bin of either is worth about
/// the same thing in Oklab, so the split that minimises the worst case is the
/// one that balances them.  A hue bin is `250 / H` degrees wide, so half of one
/// is `125 / H`, and at the full chroma of 0.15 that is a distance of
/// `0.15 * (125 / H) * pi / 180 = 0.327 / H`.  A mix bin is `1 / M` of the
/// concentration, so half of one is `0.15 / (2 M) = 0.075 / M` of chroma
/// directly.  With `H * M = 256` the two are equal at `H = 33.4`, predicting a
/// worst case of 0.0139 in quadrature; 32 is the power of two beside it.
///
/// Measured over the first 60,000,000 records of the 2022 chain
/// (`--bands 1024 --moments`, 4m23s, 11.3 GB peak), as the mean and the worst
/// Oklab distance between a transaction's true `tint` and the entry its index
/// points at:
///
/// ```text
///       H    M   mix axis         mean dE    worst dE
///     128    2   concentration   0.020889    0.039607
///      85    3   concentration   0.014990    0.027193
///      64    4   concentration   0.007640    0.021206
///      51    5   concentration   0.007976    0.017383
///      42    6   concentration   0.007443    0.015563
///      32    8   concentration   0.005328    0.014707
///      25   10   concentration   0.005081    0.015956
///      16   16   concentration   0.005891    0.023166
///      64    4   doublings       0.009728    0.039572
///      32    8   doublings       0.007344    0.015578
///      16   16   doublings       0.008407    0.023166
/// ```
///
/// 32 by 8 has the smallest worst case of the lot, and 0.0139 predicted against
/// 0.014707 measured says the balance argument is the reason rather than a
/// coincidence.  Only 25 by 10 beats it on the mean, by 0.0002, and pays 0.0012
/// of worst case for it.  It matters that the *worst* case is what was
/// minimised: 0.02 in Oklab is about a just-noticeable difference, a picture is
/// read one transaction at a time, and at 32 by 8 no transaction anywhere moves
/// by a difference an eye can find.
///
/// The same table over 2,000,000 records of `--weighted --moments`, where the
/// moments are exact rather than binned into 1024 bands — 936,288 of those
/// 2,000,000 lines carry a different mean or spread from the binned run over
/// the same records — puts 32 by 8 at 0.004765 mean and **0.014703** worst.
/// The same worst case to five digits off half a million different numbers is
/// the point: it is a property of the geometry and not of the data, so it is a
/// guarantee and not an observation.
///
/// ## The mix ladder that was rejected
///
/// `src/bin/tx-view.rs` gives one slot per doubling — 1 block, 2 or 3, 4 to 7 —
/// which is the right ladder for a *count of blocks* starting at one.  Tried
/// here as bins doubling out from `SPREAD_HALF / 2^(M-4)`, it is worse on both
/// statistics at every split that fits in a byte: 0.007344 mean and 0.015578
/// worst at 32 by 8, against 0.005328 and 0.014707.  The reason is in the
/// paragraph above `tint_index`.  A doubling ladder is compressive in the
/// spread; the chroma is not a function of the spread but of the
/// [`concentration`], which is compressive already.  Compressing a compressed
/// axis crowds the concentrated end — where 5.78% of those 60,000,000 records
/// sit at a spread of exactly zero — and stretches the mixed end, where there
/// is nothing.
///
/// ## The 32 entries that never come up
///
/// Mix slot 7 is every concentration under 1/8, which needs a spread past
/// 315,000 blocks; the largest in the 2022 chain is about 260,000, so 32 of the
/// 256 entries stand for a colour that chain never shows.  They are kept.
/// Fitting the axis to the widest spread actually seen would buy about an
/// eighth of the mix error and would make the byte a function of the file it
/// was computed over — the same transaction would get one index in a run over a
/// prefix and another in a run over the whole chain, which is exactly the
/// property that makes a byte stream unreadable six months later.
#[allow(dead_code)]
pub fn tints() -> [Rgb; TINTS_LEN] {
    let mut out = [[0u8; 3]; TINTS_LEN];
    for (index, slot) in out.iter_mut().enumerate() {
        let (hue, mix) = (index / TINT_MIXES, index % TINT_MIXES);
        // The centre of the bin, on both axes.  `tint` reads its first argument
        // only as `mean / chain`, so a chain of 1 makes that argument the
        // fraction itself and the table independent of the chain height.
        let where_ = (hue as f64 + 0.5) / TINT_HUES as f64;
        let step = TINT_MIXES - 1 - mix;
        let middle = (step as f64 + 0.5) / TINT_MIXES as f64;
        *slot = tint(where_, spread_at(middle), 1.0);
    }
    out
}

/// An sRGB colour as Oklab: lightness, then the two opponent axes.
///
/// The forward half of what [`oklch_to_srgb`] undoes, and it is here so that
/// the cost of quantising a colour can be *measured* rather than asserted — a
/// distance in this space is the only number available that means "how
/// different do these look".
#[allow(dead_code)]
pub fn oklab(rgb: Rgb) -> [f64; 3] {
    let linear = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let (r, g, b) = (linear(rgb[0]), linear(rgb[1]), linear(rgb[2]));
    let l = (0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b).cbrt();
    let m = (0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b).cbrt();
    let s = (0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b).cbrt();
    [
        0.210_454_255_3 * l + 0.793_617_785_0 * m - 0.004_072_046_8 * s,
        1.977_998_495_1 * l - 2.428_592_205_0 * m + 0.450_593_709_9 * s,
        0.025_904_037_1 * l + 0.782_771_766_2 * m - 0.808_675_766_0 * s,
    ]
}

/// How far apart two colours look: the Euclidean distance in [`oklab`], which
/// is the whole reason for being in that space.
///
/// About 0.02 is a just-noticeable difference for a pair of large patches, and
/// rather less for two colours touching.  The quantisation [`tints`] does is
/// held under it, which is what the numbers on that page are.
#[allow(dead_code)]
pub fn difference(one: Rgb, other: Rgb) -> f64 {
    let (a, b) = (oklab(one), oklab(other));
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
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

    /// The one property the whole byte rests on: the entry a transaction's
    /// index points at is a colour it can be mistaken for.
    ///
    /// The bound is the worst case `tints` claims -- 0.014707 measured over
    /// 60,000,000 real records, 0.014703 over 2,000,000 exact ones -- and this
    /// sweeps a grid finer than the bins to catch the corners the chain never
    /// happened to visit.  A change to `TINT_HUES`, `TINT_MIXES`, `HUE_ARC` or
    /// `CHROMA` that spent more than a just-noticeable difference would fail
    /// here rather than in six months in a picture.
    #[test]
    fn a_tint_index_points_at_a_colour_the_tint_can_be_mistaken_for() {
        let chain = 762_261.0;
        let table = tints();
        let mut worst = 0.0f64;
        let (mut where_worst, mut spread_worst) = (0.0, 0.0);
        for step in 0..=400 {
            let mean = chain * step as f64 / 400.0;
            // Zero, then out past the widest spread the 2022 chain shows.
            for spread in [0.0, 1.0, 500.0, 5_000.0, 22_500.0, 45_000.0, 90_000.0,
                           150_000.0, 260_000.0, 315_000.0, 1_000_000.0] {
                let index = tint_index(mean, spread, chain) as usize;
                let d = difference(tint(mean, spread, chain), table[index]);
                if d > worst {
                    worst = d;
                    where_worst = mean;
                    spread_worst = spread;
                }
            }
        }
        assert!(
            worst <= 0.0148,
            "quantising cost {} at mean {} spread {}, past the 0.014707 the docs claim",
            worst,
            where_worst,
            spread_worst
        );
    }

    /// Each entry is the tint of its own bin's centre, so the centre has to map
    /// back to the entry.  Without this the table and the index are two
    /// descriptions of two different partitions and every byte is off by one
    /// somewhere.
    #[test]
    fn every_entry_is_the_colour_of_a_bin_that_indexes_back_to_it() {
        for index in 0..TINTS_LEN {
            let (hue, mix) = (index / TINT_MIXES, index % TINT_MIXES);
            let where_ = (hue as f64 + 0.5) / TINT_HUES as f64;
            let middle = ((TINT_MIXES - 1 - mix) as f64 + 0.5) / TINT_MIXES as f64;
            assert_eq!(
                tint_index(where_, spread_at(middle), 1.0) as usize,
                index,
                "entry {} is drawn for a bin that indexes elsewhere",
                index
            );
        }
    }

    /// [`concentration`] and [`spread_at`] are the mix axis and its inverse, and
    /// `tints` evaluates `tint` through the pair.  A drift between them would
    /// move every entry off its bin centre without moving the index, which the
    /// test above would catch only once it had grown past half a bin.
    #[test]
    fn the_mix_axis_and_its_inverse_are_the_same_axis() {
        for spread in [0.0, 1.0, 45_000.0, 260_000.0, 1e9] {
            let there_and_back = spread_at(concentration(spread));
            assert!(
                (there_and_back - spread).abs() <= 1e-6 * spread.max(1.0),
                "spread {} came back as {}",
                spread,
                there_and_back
            );
        }
        assert_eq!(concentration(0.0), 1.0, "a coinbase is perfectly concentrated");
        assert!(concentration(SPREAD_HALF) - 0.5 < 1e-12, "half the chroma at SPREAD_HALF");
        // Monotone, which is what makes an ordered cut of it an ordered cut of
        // the mixing.
        let mut previous = f64::INFINITY;
        for k in 0..1000 {
            let c = concentration(k as f64 * 500.0);
            assert!(c < previous, "concentration must fall with spread");
            previous = c;
        }
    }

    /// The byte's two axes read the way the tint's two channels do: slot 0 of
    /// the mix axis is a coinbase, the last slot is a colour that has been
    /// everywhere, and the hue slot climbs with the chain.
    #[test]
    fn the_index_counts_the_way_the_colour_does() {
        let chain = 762_261.0;
        let mix = |mean, spread| tint_index(mean, spread, chain) as usize % TINT_MIXES;
        let hue = |mean, spread| tint_index(mean, spread, chain) as usize / TINT_MIXES;

        assert_eq!(mix(400_000.0, 0.0), 0, "a coinbase is the unmixed end");
        assert!(mix(400_000.0, 45_000.0) > 0, "a spread of SPREAD_HALF is mixed");
        assert!(
            mix(400_000.0, 260_000.0) > mix(400_000.0, 45_000.0),
            "more spread is more mixed"
        );
        assert_eq!(
            mix(400_000.0, 1e12),
            TINT_MIXES - 1,
            "and it saturates rather than overflowing"
        );

        assert_eq!(hue(0.0, 0.0), 0, "block 0 is the cold end");
        assert_eq!(hue(chain, 0.0), TINT_HUES - 1, "the tip is the warm end");
        assert_eq!(
            hue(chain * 10.0, 0.0),
            TINT_HUES - 1,
            "past the tip clamps rather than wrapping"
        );
        let mut previous = 0;
        for k in 0..=100 {
            let h = hue(chain * k as f64 / 100.0, 0.0);
            assert!(h >= previous, "the hue slot must not go backwards along the chain");
            previous = h;
        }
    }

    /// The mistake this table exists to prevent.
    ///
    /// [`ramp`] is keyed by a weight on the block axis and carries its signal in
    /// lightness; [`tints`] is keyed by a whole colour and is at constant
    /// lightness by construction.  Running an index through `ramp` would draw a
    /// legend that lies, and the two are different sizes of different things --
    /// so they must not be interchangeable, and the numbers say so.
    #[test]
    fn the_tint_table_is_not_the_weight_ramp() {
        let (table, ramp) = (tints(), ramp());
        assert_ne!(&table[..], &ramp[..], "two tables, two meanings");

        let lightness = |c: Rgb| oklab(c)[0];
        let spanned = |t: &[Rgb]| {
            let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
            for &c in t {
                lo = lo.min(lightness(c));
                hi = hi.max(lightness(c));
            }
            hi - lo
        };
        // The ramp spends nearly the whole lightness axis; the tints spend the
        // gamut mapping's leftovers and nothing else.
        assert!(
            spanned(&ramp) > 0.5,
            "the ramp carries its signal in lightness: spanned {}",
            spanned(&ramp)
        );
        assert!(
            spanned(&table) < 0.1,
            "the tints are at one lightness, so hue and chroma carry both axes: spanned {}",
            spanned(&table)
        );
    }

    /// A byte is all there is, so the table has to fit in one and fill it.
    #[test]
    fn the_table_fills_a_byte_exactly() {
        assert_eq!(TINTS_LEN, 256, "one byte, every value of it");
        assert_eq!(TINT_HUES * TINT_MIXES, TINTS_LEN);
        assert_eq!(tints().len(), TINTS_LEN);
        // And the largest index a transaction can be given is inside it, which
        // is what the `as u8` in `tint_index` is trusting.
        assert_eq!(tint_index(f64::MAX, f64::MAX, 762_261.0) as usize, TINTS_LEN - 1);
    }

    /// [`oklab`] is the inverse of [`oklch_to_srgb`], and if it is not then
    /// every dE in this file is a number about nothing.
    #[test]
    fn the_two_directions_of_the_space_agree() {
        let corners = [(0.72, 0.0, 0.0), (0.72, 0.05, 30.0), (0.5, 0.1, 250.0), (0.9, 0.02, 150.0)];
        for &(l, c, h) in &corners {
            let rgb = oklch_to_srgb(l, c, h).expect("chosen to be inside sRGB");
            let back = oklab(rgb);
            let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
            // 8-bit channels, so the round trip is only good to a step of them.
            for (got, want) in back.iter().zip(&[l, a, b]) {
                assert!(
                    (got - want).abs() < 3e-3,
                    "L={} C={} h={} came back as {:?}",
                    l, c, h, back
                );
            }
        }
        assert_eq!(difference([17, 34, 51], [17, 34, 51]), 0.0, "a colour is itself");
        assert!(
            difference([0, 0, 0], [255, 255, 255]) > 0.9,
            "black to white is the whole axis"
        );
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
