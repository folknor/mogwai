// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! [`TapeRng`], the counter-based generator every generated-tape stream draws
//! from (tape protocol 38): Philox4x32-10, the published Random123 parameters,
//! keyed by the river and addressed by a stream domain and a position.
//!
//! It replaced `ChaCha12Rng`, which in an unoptimized dev build cost about
//! 300 ns per `u64` - most of a legacy parent's dev cost, and what kept the
//! heavy test walks past the per-test watchdog. A Philox block is ten rounds
//! of two widening 32-bit multiplies, with no buffer to regenerate on a seek.
//!
//! The mapping:
//!
//! - The key is the two halves of `splitmix64(river_seed)`. A river selects a
//!   member of Philox's keyed permutation family; `splitmix64` is a bijection,
//!   so distinct river seeds give distinct keys.
//! - The 128-bit counter is `[block low, block high, domain low, domain high]`.
//!   A stream role (the main stream, the cadence stream, the sizes, ...) is a
//!   fixed domain constant, so within one river the roles occupy disjoint
//!   counter addresses by construction, not by probability. That is a statement
//!   about addresses: a `u64` output of one domain can equal one of another's,
//!   as any two draws can, and nothing here is a proof of independence -
//!   Philox's evidence for that use is statistical.
//! - A block's four 32-bit outputs are read as two `u64`, lane 0 `x0 | x1 << 32`
//!   and lane 1 `x2 | x3 << 32`. Position `n` is block `n / 2`, lane `n % 2`, so
//!   a position counts `u64` outputs.
//!
//! Consumption is fixed per call and must stay so, since every sampler's draw
//! count rides on it: `next_u64` takes one position; `next_u32` takes one
//! position and returns its low half; `fill_bytes` takes one position per eight
//! bytes, little-endian, discarding the tail of the last. A later "optimization"
//! that buffered half words would move every tape.
//!
//! The position space is a `u64` per domain. Advancing past `u64::MAX` panics in
//! every build rather than wrapping onto the domain's first output: at the
//! rates the walks draw it is unreachable, and if it were reached a wrap would
//! silently repeat the river.

use std::convert::Infallible;

use mogwai_protocol::seeds::splitmix64;
use rand::rand_core::{SeedableRng, TryRng};

const M0: u32 = 0xD251_1F53;
const M1: u32 = 0xCD9E_8D57;
const W0: u32 = 0x9E37_79B9;
const W1: u32 = 0xBB67_AE85;

/// The domain a bare [`SeedableRng`] construction draws from, for tests and
/// tools that want a seeded stream and no particular role.
pub const UNTAGGED_DOMAIN: u64 = 0;

/// One Philox4x32-10 block: the published round function, ten rounds, the key
/// bumped between rounds.
#[must_use]
pub fn philox4x32_10(counter: [u32; 4], key: [u32; 2]) -> [u32; 4] {
    let [mut x0, mut x1, mut x2, mut x3] = counter;
    let [mut k0, mut k1] = key;
    for round in 0..10 {
        if round > 0 {
            k0 = k0.wrapping_add(W0);
            k1 = k1.wrapping_add(W1);
        }
        let p0 = u64::from(M0) * u64::from(x0);
        let p1 = u64::from(M1) * u64::from(x2);
        let (hi0, lo0) = ((p0 >> 32) as u32, p0 as u32);
        let (hi1, lo1) = ((p1 >> 32) as u32, p1 as u32);
        x0 = hi1 ^ x1 ^ k0;
        x1 = lo1;
        x2 = hi0 ^ x3 ^ k1;
        x3 = lo0;
    }
    [x0, x1, x2, x3]
}

/// One generated-tape stream: a river key, a domain and a position. Cloning
/// copies the cached block with its validity, so a clone continues exactly
/// where the original would.
#[derive(Clone, Debug)]
pub struct TapeRng {
    key: [u32; 2],
    domain: u64,
    /// The next position to read.
    position: u64,
    /// The last block computed, with its block address. A read uses it only
    /// when the address matches, so a seek anywhere - forward, backward, to an
    /// odd position - can leave it in place without ever serving a stale lane.
    cached: Option<(u64, [u32; 4])>,
}

impl TapeRng {
    /// The `domain` stream of the river seeded by `river_seed`, at position 0.
    #[must_use]
    pub fn new(river_seed: u64, domain: u64) -> Self {
        let key = splitmix64(river_seed);
        Self {
            key: [key as u32, (key >> 32) as u32],
            domain,
            position: 0,
            cached: None,
        }
    }

    /// The next position this stream reads, in `u64` outputs.
    #[must_use]
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Move to `position`; the next output is the one at that position.
    pub fn seek(&mut self, position: u64) {
        self.position = position;
    }

    fn block(&mut self, block: u64) -> [u32; 4] {
        if let Some((at, words)) = self.cached
            && at == block
        {
            return words;
        }
        let words = philox4x32_10(
            [
                block as u32,
                (block >> 32) as u32,
                self.domain as u32,
                (self.domain >> 32) as u32,
            ],
            self.key,
        );
        self.cached = Some((block, words));
        words
    }

    /// The `u64` at the current position, advancing past it.
    #[inline]
    pub fn next_word(&mut self) -> u64 {
        let position = self.position;
        self.position = position
            .checked_add(1)
            .expect("a tape stream exhausted its u64 position space");
        let words = self.block(position >> 1);
        if position & 1 == 0 {
            u64::from(words[0]) | (u64::from(words[1]) << 32)
        } else {
            u64::from(words[2]) | (u64::from(words[3]) << 32)
        }
    }
}

impl TryRng for TapeRng {
    type Error = Infallible;

    fn try_next_u32(&mut self) -> Result<u32, Infallible> {
        Ok(self.next_word() as u32)
    }

    fn try_next_u64(&mut self) -> Result<u64, Infallible> {
        Ok(self.next_word())
    }

    fn try_fill_bytes(&mut self, dst: &mut [u8]) -> Result<(), Infallible> {
        for chunk in dst.chunks_mut(8) {
            let bytes = self.next_word().to_le_bytes();
            chunk.copy_from_slice(&bytes[..chunk.len()]);
        }
        Ok(())
    }
}

/// A bare seeded stream: the river seed little-endian, the untagged domain.
/// Stated here rather than left to the trait's default expansion, so no hidden
/// derivation stands between a seed and its stream.
impl SeedableRng for TapeRng {
    type Seed = [u8; 8];

    fn from_seed(seed: [u8; 8]) -> Self {
        Self::new(u64::from_le_bytes(seed), UNTAGGED_DOMAIN)
    }

    fn seed_from_u64(state: u64) -> Self {
        Self::new(state, UNTAGGED_DOMAIN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Random123 known-answer vectors for Philox4x32-10 (`kat_vectors`):
    /// counter and key all zeros, all ones, and the pi digits.
    #[test]
    fn philox_matches_the_published_known_answers() {
        assert_eq!(
            philox4x32_10([0; 4], [0; 2]),
            [0x6627_E8D5, 0xE169_C58D, 0xBC57_AC4C, 0x9B00_DBD8]
        );
        assert_eq!(
            philox4x32_10([u32::MAX; 4], [u32::MAX; 2]),
            [0x408F_276D, 0x41C8_3B0E, 0xA20B_C7C6, 0x6D54_51FD]
        );
        assert_eq!(
            philox4x32_10(
                [0x243F_6A88, 0x85A3_08D3, 0x1319_8A2E, 0x0370_7344],
                [0xA409_3822, 0x299F_31D0]
            ),
            [0xD16C_FE09, 0x94FD_CCEB, 0x5001_E420, 0x2412_6EA1]
        );
    }

    /// The addressing wrapper against the bare block function: both lanes,
    /// the domain in the counter's high half, the key from the river seed.
    #[test]
    fn positions_read_the_block_they_name() {
        let seed = 0x0123_4567_89AB_CDEF;
        let domain = 0xFEED_FACE_0000_0007;
        let key = splitmix64(seed);
        let key = [key as u32, (key >> 32) as u32];
        let mut rng = TapeRng::new(seed, domain);
        // Position 2^33 + 4 crosses the low counter word into the high one.
        for block in [0u64, 1, 2, (1 << 32) - 1, 1 << 32, (1 << 32) + 2] {
            let words = philox4x32_10(
                [
                    block as u32,
                    (block >> 32) as u32,
                    domain as u32,
                    (domain >> 32) as u32,
                ],
                key,
            );
            rng.seek(2 * block);
            assert_eq!(
                rng.next_word(),
                u64::from(words[0]) | (u64::from(words[1]) << 32)
            );
            assert_eq!(
                rng.next_word(),
                u64::from(words[2]) | (u64::from(words[3]) << 32)
            );
        }
    }

    /// An odd seek reads lane 1 of its block even when another block is
    /// cached; a sequential read and a seek agree; a clone continues where the
    /// original does; a `u32` call takes a whole position.
    #[test]
    fn seeks_clones_and_mixed_calls_keep_their_positions() {
        let mut sequential = TapeRng::new(7, 3);
        let words: Vec<u64> = (0..9).map(|_| sequential.next_word()).collect();
        let mut seeking = TapeRng::new(7, 3);
        for &at in &[5u64, 0, 8, 3, 4, 1] {
            seeking.seek(at);
            assert_eq!(seeking.next_word(), words[at as usize], "position {at}");
        }
        let mut a = TapeRng::new(7, 3);
        a.next_word();
        let mut b = a.clone();
        assert_eq!(a.next_word(), b.next_word());
        let mut c = TapeRng::new(7, 3);
        assert_eq!(c.try_next_u32().unwrap(), words[0] as u32);
        assert_eq!(c.position(), 1);
        let mut bytes = [0u8; 11];
        c.try_fill_bytes(&mut bytes).unwrap();
        assert_eq!(c.position(), 3);
        assert_eq!(bytes[..8], words[1].to_le_bytes());
        assert_eq!(bytes[8..], words[2].to_le_bytes()[..3]);
    }

    /// Different domains of one river and different rivers in one domain read
    /// different blocks.
    #[test]
    fn domains_and_rivers_select_different_streams() {
        let first = |seed, domain| TapeRng::new(seed, domain).next_word();
        assert_ne!(first(1, 0), first(1, 1));
        assert_ne!(first(1, 0), first(2, 0));
    }

    #[test]
    #[should_panic(expected = "exhausted its u64 position space")]
    fn the_last_position_refuses_rather_than_wraps() {
        let mut rng = TapeRng::new(1, 0);
        rng.seek(u64::MAX);
        rng.next_word();
    }
}
