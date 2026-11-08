//! The numbers behind `Random`: `SplitMix64`, drawn by position. The `n`th number of a seed's
//! sequence depends only on the seed and `n`, so it's the same on every machine and can be
//! drawn without keeping any state.

/// The `n`th 64-bit number of `seed`'s sequence.
pub(crate) fn splitmix(seed: i64, n: i64) -> u64 {
    const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;
    let step = n.cast_unsigned().wrapping_add(1).wrapping_mul(GOLDEN);
    let mut z = seed.cast_unsigned().wrapping_add(step);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    /// The reference `SplitMix64`'s first outputs for seed 0, so every build draws the same.
    #[test]
    fn splitmix_matches_the_reference() {
        assert_eq!(super::splitmix(0, 0), 0xE220_A839_7B1D_CDAF);
        assert_eq!(super::splitmix(0, 1), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(super::splitmix(0, 2), 0x06C4_5D18_8009_454F);
    }
}
