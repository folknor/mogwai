// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

/// Every random stream in one run, derived from the run's single seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunSeeds {
    /// The seed as drawn or configured. The value reported and reproduced.
    pub run: u64,
    /// Root of the fill band's draw stream, run-level and symbol-free: the
    /// band's key already mixes `order.symbol` in `draw_key`.
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
// ASCII of a name", so the hex is written out beside each constant. The fill
// derivation is pinned by `derived_streams_differ_and_are_stable` and the
// per-symbol tape derivation by `symbol_tape_roots_are_stable`.
//
// The fill root retains the structural account: splitmix64 is a bijection on
// u64 (an alternating chain of xor-shifts and odd multiplies, each invertible),
// so distinct run seeds produce distinct fill roots. Per-symbol tape roots are
// related by a hash over variable-length input instead, so collisions exist in
// principle. The pairwise-distinctness assertions below are samples supporting
// the narrower claim that no realistic symbol pair collides, not a proof.
const DOMAIN_TAPE: u64 = u64::from_le_bytes(*b"tape_gen"); // 0x6e65675f65706174
const DOMAIN_FILL: u64 = u64::from_le_bytes(*b"fill_bnd"); // 0x646e625f6c6c6966
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

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
            fill: splitmix64(run ^ DOMAIN_FILL),
        }
    }

    /// Root of the tape generator's stream for one requested symbol label.
    ///
    /// The label, not the resolved shape's symbol: two labels that resolve to
    /// the same default shape are different rivers and must not share a path.
    #[must_use]
    pub fn tape_for(&self, symbol: &str) -> u64 {
        let mut hash = FNV_OFFSET;
        let mut feed = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        };
        feed(&splitmix64(self.run ^ DOMAIN_TAPE).to_le_bytes());
        // This separates one fixed-width root from its one variable-width
        // suffix. It is not length framing and does not make this extensible.
        feed(&[0]);
        feed(symbol.as_bytes());
        splitmix64(hash)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_streams_differ_and_are_stable() {
        let expected = [
            (0, 0xe800_e9a6_2035_1b1b),
            (1, 0xf019_ac7a_e519_ce08),
            (u64::MAX, 0xac14_3574_b13c_7a54),
        ];
        for (run, fill) in expected {
            let seeds = RunSeeds::from_run_seed(run);
            assert_eq!(seeds.fill, fill);
        }
        assert_ne!(
            RunSeeds::from_run_seed(0).fill,
            RunSeeds::from_run_seed(1).fill
        );
    }

    #[test]
    fn tape_roots_differ_by_symbol_under_one_run_seed() {
        let symbols = ["MNQ", "MES", "BTCUSDT", "FOOBAR", ""];
        for run in [0, 1, u64::MAX] {
            let seeds = RunSeeds::from_run_seed(run);
            let roots: Vec<_> = symbols
                .iter()
                .map(|symbol| seeds.tape_for(symbol))
                .collect();
            for (index, root) in roots.iter().enumerate() {
                assert_ne!(*root, seeds.fill, "run {run}, symbol {}", symbols[index]);
                for (offset, other) in roots[index + 1..].iter().enumerate() {
                    assert_ne!(
                        root,
                        other,
                        "run {run}, symbols {} and {}",
                        symbols[index],
                        symbols[index + 1 + offset]
                    );
                }
            }
        }
    }

    #[test]
    fn tape_roots_differ_by_run_seed_under_one_symbol() {
        assert_ne!(
            RunSeeds::from_run_seed(0).tape_for("MNQ"),
            RunSeeds::from_run_seed(1).tape_for("MNQ")
        );
    }

    #[test]
    fn symbol_tape_roots_are_stable() {
        assert_eq!(
            RunSeeds::from_run_seed(0).tape_for("BTCUSDT"),
            0x46b9_8c59_d130_6ed5
        );
        assert_eq!(
            RunSeeds::from_run_seed(1).tape_for("MNQ"),
            0x9551_64ca_01de_10a1
        );
        assert_eq!(
            RunSeeds::from_run_seed(u64::MAX).tape_for("FOOBAR"),
            0x8382_8adb_1025_3100
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
