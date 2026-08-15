// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

/// Every random stream in one run, derived from the run's single seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSeeds {
    /// The seed as drawn or configured. The value reported and reproduced.
    pub run: u64,
    /// Root of the tape generator's stream.
    pub tape: u64,
    /// Root of the fill band's draw stream. Separate from `tape` so the number
    /// of orders a client places cannot move the tape.
    pub fill: u64,
}

// Changing this derivation or either domain changes generated tape identity and
// therefore requires a `mogwai_data::TAPE_PROTOCOL_VERSION` bump in the same
// commit. The constant lives in the downstream crate because mogwai-protocol
// deliberately has no workspace dependencies; see the repository versioning
// rule in AGENTS.md.
//
// Domain separation rather than `seed` and `seed + 1`, so adjacent run seeds do
// not alias one another's streams. Little-endian is the one free choice in "the
// ASCII of a name", so the hex is written out beside each constant and the
// derivation is pinned by `derived_streams_differ_and_are_stable`.
const DOMAIN_TAPE: u64 = u64::from_le_bytes(*b"tape_gen"); // 0x6e65675f65706174
const DOMAIN_FILL: u64 = u64::from_le_bytes(*b"fill_bnd"); // 0x646e625f6c6c6966

pub const fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl RunSeeds {
    #[must_use]
    pub const fn from_run_seed(run: u64) -> Self {
        Self {
            run,
            tape: splitmix64(run ^ DOMAIN_TAPE),
            fill: splitmix64(run ^ DOMAIN_FILL),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_streams_differ_and_are_stable() {
        let expected = [
            (0, 0x31ad_0d8b_b6c2_a429, 0xe800_e9a6_2035_1b1b),
            (1, 0x97cf_101c_51b0_7fa5, 0xf019_ac7a_e519_ce08),
            (u64::MAX, 0xa74e_19e5_da36_6019, 0xac14_3574_b13c_7a54),
        ];
        for (run, tape, fill) in expected {
            let seeds = RunSeeds::from_run_seed(run);
            assert_eq!((seeds.tape, seeds.fill), (tape, fill));
            assert_ne!(seeds.tape, seeds.fill);
        }
        assert_ne!(
            RunSeeds::from_run_seed(0).tape,
            RunSeeds::from_run_seed(1).tape
        );
        assert_ne!(
            RunSeeds::from_run_seed(0).fill,
            RunSeeds::from_run_seed(1).fill
        );
    }

    #[test]
    fn splitmix64_matches_its_stable_vectors() {
        let vectors = [
            (0, 0xe220_a839_7b1d_cdaf),
            (1, 0x910a_2dec_8902_5cc1),
            (u64::MAX, 0xe4d9_7177_1b65_2c20),
            (0x6d6f_6777_6169_3132, 0xb1d6_30a3_eb8b_9f7f),
        ];
        for (input, expected) in vectors {
            assert_eq!(splitmix64(input), expected, "input {input:#018x}");
        }
    }
}
