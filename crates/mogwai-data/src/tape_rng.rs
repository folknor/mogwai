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
//! that buffered half words would move every tape. [`TapeRng::fill`] is the
//! batched form of `next_u64`, not a new consumption pattern: it writes the
//! same words at the same positions into a slice the caller owns. In a build
//! without debug assertions it computes up to sixteen blocks per kernel call,
//! through an AVX2 intrinsics kernel when the running x86-64 CPU has AVX2 and
//! through the portable [`philox4x32_10_lanes`] otherwise; with debug
//! assertions it computes one scalar block at a time, which an unoptimized
//! build runs fastest. Every backend is bit-identical.
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

/// The blocks one chain of the AVX2 kernel holds, one in each 64-bit lane of
/// a 256-bit register: [`TapeRng::fill`] computes up to four such chains per
/// kernel call, interleaved. It changes throughput only, never a word,
/// because every lane is the same function of its own counter as
/// [`philox4x32_10`].
pub const FILL_LANES: usize = 4;

/// `N` consecutive Philox4x32-10 blocks at once, blocks `block` through
/// `block + N - 1` of `domain` under `key`, in structure-of-arrays form:
/// `out[j][i]` is output word `j` of block `block + i`, exactly what
/// [`philox4x32_10`] returns for that block's counter.
///
/// Written in the shape an autovectorizer can use - round keys computed once
/// and shared, rounds as the outer loop and lanes as the inner one, `N` a
/// compile-time constant, nothing in the lane loop branching on data - but
/// that shape is no promise the compiler emits vector multiplies, and under
/// AVX2 on x86-64 it measured slower than its baseline build, which is why
/// the fill dispatches to an intrinsics kernel there. Each lane's counter low and high words
/// come from `block + i` in full 64-bit arithmetic, so a run crossing block
/// `2^32 - 1` carries into the high word per lane exactly as the scalar
/// addressing does. `block + N - 1` must not overflow `u64`; every caller here
/// addresses at most `u64::MAX / 2 + 1` blocks.
#[must_use]
#[inline(always)]
#[allow(
    clippy::needless_range_loop,
    reason = "indexed lane loops over fixed arrays are the shape the autovectorizer is meant to see"
)]
pub fn philox4x32_10_lanes<const N: usize>(
    block: u64,
    domain: u64,
    key: [u32; 2],
) -> [[u32; N]; 4] {
    let mut x0 = [0u32; N];
    let mut x1 = [0u32; N];
    let mut x2 = [domain as u32; N];
    let mut x3 = [(domain >> 32) as u32; N];
    for i in 0..N {
        let b = block + i as u64;
        x0[i] = b as u32;
        x1[i] = (b >> 32) as u32;
    }
    let mut k0 = [0u32; 10];
    let mut k1 = [0u32; 10];
    k0[0] = key[0];
    k1[0] = key[1];
    for round in 1..10 {
        k0[round] = k0[round - 1].wrapping_add(W0);
        k1[round] = k1[round - 1].wrapping_add(W1);
    }
    for round in 0..10 {
        let (rk0, rk1) = (k0[round], k1[round]);
        for i in 0..N {
            let p0 = u64::from(M0) * u64::from(x0[i]);
            let p1 = u64::from(M1) * u64::from(x2[i]);
            let y0 = ((p1 >> 32) as u32) ^ x1[i] ^ rk0;
            let y2 = ((p0 >> 32) as u32) ^ x3[i] ^ rk1;
            x1[i] = p1 as u32;
            x3[i] = p0 as u32;
            x0[i] = y0;
            x2[i] = y2;
        }
    }
    [x0, x1, x2, x3]
}

/// How a fill computes its blocks. Every backend is the same integer
/// arithmetic and writes the same words bit for bit, so nothing about a tape
/// can depend on which one ran; the choice is throughput only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FillBackend {
    /// One [`philox4x32_10`] block per pair of words, through the cache.
    Scalar,
    /// Lane batches through the portable [`philox4x32_10_lanes`].
    Portable,
    /// Lane batches through [`philox4x32_10_avx2`]. Only ever selected on a
    /// CPU that was checked for AVX2.
    #[cfg(target_arch = "x86_64")]
    Avx2,
}

impl FillBackend {
    /// The backend [`TapeRng::fill`] uses. On x86-64 with AVX2, the
    /// intrinsics kernel, because the portable kernel compiled under AVX2
    /// measured slower than its baseline build - the autovectorizer did not
    /// turn the widening multiply into useful vector code. Elsewhere, the
    /// portable kernel. In a build with debug assertions, scalar blocks: an
    /// unoptimized build makes every intrinsic a call, and there the batch
    /// kernels cost more than the blocks they replace, as `vecmath`'s vector
    /// path does. `debug_assertions` is an assertion policy, not an
    /// optimization detector; it is the switch because it is what separates
    /// the dev lane from release here, and a consumer who optimizes with
    /// assertions on gets correct words either way.
    fn selected() -> Self {
        if cfg!(debug_assertions) {
            return Self::Scalar;
        }
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            return Self::Avx2;
        }
        Self::Portable
    }
}

/// The fill's lane batch: `N` consecutive blocks through a batch backend.
#[inline]
fn fill_lanes<const N: usize>(
    backend: FillBackend,
    block: u64,
    domain: u64,
    key: [u32; 2],
) -> [[u32; N]; 4] {
    match backend {
        #[cfg(target_arch = "x86_64")]
        // SAFETY: `Avx2` is only constructed after the running CPU was
        // checked for AVX2, the only feature the called function enables.
        FillBackend::Avx2 => unsafe { philox4x32_10_avx2::<N>(block, domain, key) },
        FillBackend::Portable => philox4x32_10_lanes::<N>(block, domain, key),
        FillBackend::Scalar => unreachable!("scalar fills never batch"),
    }
}

/// The most blocks one AVX2 call computes: four chains of four.
const MAX_CHAIN_BLOCKS: usize = 16;

/// `N` consecutive Philox4x32-10 blocks in AVX2 registers, `N` a multiple of
/// four up to [`MAX_CHAIN_BLOCKS`]: `N / 4` independent chains of four blocks,
/// their rounds interleaved. Within a chain, each of the four 64-bit lanes
/// holds one block, and each of the four state words sits in the low half of
/// its own register, so `_mm256_mul_epu32` - which multiplies the low
/// unsigned 32 bits of every 64-bit lane into a full 64-bit product - is the
/// round's widening multiply for all four blocks at once, two per round. The
/// high halves of every register stay zero through the rounds (the xors
/// combine zero-high values and the low halves are masked), so the state never
/// leaves the registers between the counter and the output.
///
/// One chain is bound by the multiply's latency, ten dependent rounds deep:
/// it measured about eight cycles per word. The chains share no state, so a
/// second, third and fourth chain fill the multiplier's idle issue slots;
/// every lane is still the same function of its own counter.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
fn philox4x32_10_avx2<const N: usize>(block: u64, domain: u64, key: [u32; 2]) -> [[u32; N]; 4] {
    use std::arch::x86_64::{
        __m256i, _mm256_and_si256, _mm256_mul_epu32, _mm256_set_epi64x, _mm256_set1_epi64x,
        _mm256_setzero_si256, _mm256_srli_epi64, _mm256_xor_si256,
    };
    const { assert!(N.is_multiple_of(4) && N > 0 && N <= MAX_CHAIN_BLOCKS) };
    let chains = N / 4;
    let low = _mm256_set1_epi64x(0xFFFF_FFFF);
    let zero = _mm256_setzero_si256();
    let mut x0 = [zero; MAX_CHAIN_BLOCKS / 4];
    let mut x1 = [zero; MAX_CHAIN_BLOCKS / 4];
    let mut x2 = [zero; MAX_CHAIN_BLOCKS / 4];
    let mut x3 = [zero; MAX_CHAIN_BLOCKS / 4];
    for c in 0..chains {
        let counter = |i: u64| block.wrapping_add(4 * c as u64 + i);
        let lanes = |f: &dyn Fn(u64) -> u64| {
            _mm256_set_epi64x(f(3) as i64, f(2) as i64, f(1) as i64, f(0) as i64)
        };
        x0[c] = lanes(&|i| counter(i) & 0xFFFF_FFFF);
        x1[c] = lanes(&|i| counter(i) >> 32);
        x2[c] = _mm256_set1_epi64x((domain & 0xFFFF_FFFF) as i64);
        x3[c] = _mm256_set1_epi64x((domain >> 32) as i64);
    }
    let m0 = _mm256_set1_epi64x(i64::from(M0));
    let m1 = _mm256_set1_epi64x(i64::from(M1));
    let (mut k0, mut k1) = (key[0], key[1]);
    for round in 0..10 {
        if round > 0 {
            k0 = k0.wrapping_add(W0);
            k1 = k1.wrapping_add(W1);
        }
        let k0v = _mm256_set1_epi64x(i64::from(k0));
        let k1v = _mm256_set1_epi64x(i64::from(k1));
        for c in 0..chains {
            let p0 = _mm256_mul_epu32(m0, x0[c]);
            let p1 = _mm256_mul_epu32(m1, x2[c]);
            let next0 = _mm256_xor_si256(_mm256_xor_si256(_mm256_srli_epi64::<32>(p1), x1[c]), k0v);
            let next2 = _mm256_xor_si256(_mm256_xor_si256(_mm256_srli_epi64::<32>(p0), x3[c]), k1v);
            x1[c] = _mm256_and_si256(p1, low);
            x3[c] = _mm256_and_si256(p0, low);
            x0[c] = next0;
            x2[c] = next2;
        }
    }
    let mut out = [[0_u32; N]; 4];
    for (word, registers) in out.iter_mut().zip([x0, x1, x2, x3]) {
        for c in 0..chains {
            // SAFETY: `__m256i` and `[u64; 4]` are both 32 bytes of plain
            // integer data with no invalid bit patterns.
            let wide: [u64; 4] = unsafe { std::mem::transmute::<__m256i, [u64; 4]>(registers[c]) };
            for (i, w) in wide.iter().enumerate() {
                word[4 * c + i] = *w as u32;
            }
        }
    }
    out
}

/// One block's lane 0 and lane 1 as the two `u64` positions they hold.
#[inline]
fn lane_words(words: [u32; 4]) -> (u64, u64) {
    (
        u64::from(words[0]) | (u64::from(words[1]) << 32),
        u64::from(words[2]) | (u64::from(words[3]) << 32),
    )
}

const EXHAUSTED: &str = "a tape stream exhausted its u64 position space";

/// The scalar block at `block`, served from `cache` when the address matches
/// and stored there otherwise.
#[inline]
fn cached_block(
    key: [u32; 2],
    domain: u64,
    cache: &mut Option<(u64, [u32; 4])>,
    block: u64,
) -> [u32; 4] {
    if let Some((at, words)) = *cache
        && at == block
    {
        return words;
    }
    let words = philox4x32_10(
        [
            block as u32,
            (block >> 32) as u32,
            domain as u32,
            (domain >> 32) as u32,
        ],
        key,
    );
    *cache = Some((block, words));
    words
}

/// The first `dst.len()` words (at most `2 * N`) of the `N` blocks from
/// `block`, leaving the cache on the block that holds the last word written,
/// whose lane 1 a following odd read wants. Always correct for its address.
#[inline]
fn write_batch<const N: usize>(
    backend: FillBackend,
    key: [u32; 2],
    domain: u64,
    cache: &mut Option<(u64, [u32; 4])>,
    block: u64,
    dst: &mut [u64],
) {
    debug_assert!(!dst.is_empty() && dst.len() <= 2 * N);
    let [x0, x1, x2, x3] = fill_lanes::<N>(backend, block, domain, key);
    for (index, word) in dst.iter_mut().enumerate() {
        let i = index / 2;
        *word = if index % 2 == 0 {
            u64::from(x0[i]) | (u64::from(x1[i]) << 32)
        } else {
            u64::from(x2[i]) | (u64::from(x3[i]) << 32)
        };
    }
    let last = (dst.len() - 1) / 2;
    *cache = Some((
        block + last as u64,
        [x0[last], x1[last], x2[last], x3[last]],
    ));
}

/// Write the words at positions `start..start + dst.len()` into `dst`. The
/// caller has checked the whole range is readable. `cache` is read and left
/// holding a block that was really computed at its address, so it is never
/// stale. Every word is the same function of its position whichever path
/// produced it - the odd head, a lane batch, or the scalar tail - so a word
/// never depends on the slice length or on where a batch boundary fell, nor
/// on the backend.
fn fill_range(
    backend: FillBackend,
    key: [u32; 2],
    domain: u64,
    cache: &mut Option<(u64, [u32; 4])>,
    start: u64,
    dst: &mut [u64],
) {
    let mut position = start;
    let mut rest = dst;
    if backend == FillBackend::Scalar {
        for word in rest {
            let (lo, hi) = lane_words(cached_block(key, domain, cache, position >> 1));
            *word = if position & 1 == 0 { lo } else { hi };
            position += 1;
        }
        return;
    }
    // An odd start reads lane 1 of its block, which the cache may already hold.
    if position & 1 == 1
        && let Some((first, tail)) = rest.split_first_mut()
    {
        *first = lane_words(cached_block(key, domain, cache, position >> 1)).1;
        position += 1;
        rest = tail;
    }
    let (batches, tail) = rest.as_chunks_mut::<{ 2 * MAX_CHAIN_BLOCKS }>();
    for batch in batches {
        write_batch::<MAX_CHAIN_BLOCKS>(backend, key, domain, cache, position >> 1, batch);
        position += (2 * MAX_CHAIN_BLOCKS) as u64;
    }
    // A tail spanning more than one block still runs one batch, the fewest
    // chains that cover it, and keeps only the words it needs: computing the
    // unused lanes is cheaper than scalar blocks, and they are never consumed,
    // so no position moves past the slice.
    if tail.len() > 2 {
        let block = position >> 1;
        match tail.len().div_ceil(2 * FILL_LANES) {
            1 => write_batch::<4>(backend, key, domain, cache, block, tail),
            2 => write_batch::<8>(backend, key, domain, cache, block, tail),
            3 => write_batch::<12>(backend, key, domain, cache, block, tail),
            _ => write_batch::<16>(backend, key, domain, cache, block, tail),
        }
        return;
    }
    // A tail of one block at most: through the cache, so a trailing lane 0
    // leaves its block cached for the next read's lane 1.
    for pair in tail.chunks_mut(2) {
        let (lo, hi) = lane_words(cached_block(key, domain, cache, position >> 1));
        pair[0] = lo;
        if let Some(second) = pair.get_mut(1) {
            *second = hi;
        }
        position += pair.len() as u64;
    }
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
        cached_block(self.key, self.domain, &mut self.cached, block)
    }

    /// The `u64` at the current position, advancing past it.
    #[inline]
    pub fn next_word(&mut self) -> u64 {
        let position = self.position;
        self.position = position.checked_add(1).expect(EXHAUSTED);
        let words = self.block(position >> 1);
        if position & 1 == 0 {
            u64::from(words[0]) | (u64::from(words[1]) << 32)
        } else {
            u64::from(words[2]) | (u64::from(words[3]) << 32)
        }
    }

    /// Fill `dst` with the next `dst.len()` words and advance past them:
    /// exactly what as many [`next_word`](Self::next_word) calls would return,
    /// leaving the same position, with whole runs of blocks computed up to
    /// sixteen per kernel call. The caller owns the scratch; the stream keeps
    /// no buffer, so its size and clone cost are unchanged.
    ///
    /// Exhaustion matches `next_word` call for call: position `u64::MAX` is
    /// never read, so when the slice would reach it, the readable prefix is
    /// filled, the position stops at `u64::MAX`, and the call panics with the
    /// same message - no earlier, because a batch extending past the space is
    /// never what refuses.
    pub fn fill(&mut self, dst: &mut [u64]) {
        self.fill_with(FillBackend::selected(), dst);
    }

    /// [`fill`](Self::fill) through a named backend, so the tests hold every
    /// backend to the whole contract whichever one the build selects.
    fn fill_with(&mut self, backend: FillBackend, dst: &mut [u64]) {
        let start = self.position;
        let readable = u64::MAX - start;
        let (now, refused) = if dst.len() as u64 > readable {
            dst.split_at_mut(readable as usize)
        } else {
            let len = dst.len();
            dst.split_at_mut(len)
        };
        fill_range(backend, self.key, self.domain, &mut self.cached, start, now);
        self.position = start + now.len() as u64;
        assert!(refused.is_empty(), "{EXHAUSTED}");
    }

    /// Fill `dst` with the words at positions `position..position + dst.len()`
    /// without moving the stream or touching its cache, for consumers that
    /// address the counter directly. Every position must be readable - below
    /// `u64::MAX`, as for `next_word` - and the call panics before writing
    /// anything otherwise, since there is no stream state for a partial fill
    /// to leave behind.
    pub fn fill_at(&self, position: u64, dst: &mut [u64]) {
        self.fill_at_with(FillBackend::selected(), position, dst);
    }

    /// [`fill_at`](Self::fill_at) through a named backend.
    fn fill_at_with(&self, backend: FillBackend, position: u64, dst: &mut [u64]) {
        assert!(dst.len() as u64 <= u64::MAX - position, "{EXHAUSTED}");
        let mut cache = self.cached;
        fill_range(backend, self.key, self.domain, &mut cache, position, dst);
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

    /// Every lane of the kernel, at both candidate widths, against the scalar
    /// block for its own counter: low starts, runs straddling the low-word
    /// carry at block `2^32 - 1`, high blocks, and the top of the addressable
    /// block range, under several keys and domains.
    #[test]
    fn lane_kernel_matches_the_scalar_block_on_every_lane() {
        fn check<const N: usize>(block: u64, domain: u64, key: [u32; 2]) {
            let lanes = philox4x32_10_lanes::<N>(block, domain, key);
            for i in 0..N {
                let b = block + i as u64;
                let scalar = philox4x32_10(
                    [
                        b as u32,
                        (b >> 32) as u32,
                        domain as u32,
                        (domain >> 32) as u32,
                    ],
                    key,
                );
                let lane = [lanes[0][i], lanes[1][i], lanes[2][i], lanes[3][i]];
                assert_eq!(lane, scalar, "N {N} block {block} lane {i}");
            }
        }
        let carry = (1u64 << 32) - 1;
        let mut starts: Vec<u64> = (0..64).collect();
        for back in 0..=8 {
            starts.push(carry - back);
        }
        starts.extend([1 << 32, (1 << 40) + 3, u64::MAX / 2 - 8, u64::MAX / 2 - 3]);
        for key in [[0, 0], [u32::MAX, u32::MAX], [0xA409_3822, 0x299F_31D0]] {
            for domain in [0, 7, 0xFEED_FACE_0000_0007, u64::MAX] {
                for &block in &starts {
                    check::<4>(block, domain, key);
                    check::<8>(block, domain, key);
                    check::<FILL_LANES>(block, domain, key);
                }
            }
        }
    }

    fn reference(seed: u64, domain: u64, start: u64, len: usize) -> Vec<u64> {
        let mut rng = TapeRng::new(seed, domain);
        rng.seek(start);
        (0..len).map(|_| rng.next_word()).collect()
    }

    /// Every way a fill can run: each backend this CPU can execute, named,
    /// and `None` for the public dispatch the build selects. The fill pins
    /// run their whole script once per entry, so a backend the build does
    /// not select is still held to the contract.
    fn fill_paths() -> Vec<Option<FillBackend>> {
        let base = [None, Some(FillBackend::Scalar), Some(FillBackend::Portable)];
        base.into_iter().chain(avx2_path()).collect()
    }

    #[cfg(target_arch = "x86_64")]
    fn avx2_path() -> Option<Option<FillBackend>> {
        if std::arch::is_x86_feature_detected!("avx2") {
            return Some(Some(FillBackend::Avx2));
        }
        eprintln!("no AVX2 on this CPU: the AVX2 fill path was not exercised");
        None
    }

    #[cfg(not(target_arch = "x86_64"))]
    fn avx2_path() -> Option<Option<FillBackend>> {
        None
    }

    impl TapeRng {
        /// The block at `block`, computed from the bare round function and
        /// never through the cache.
        fn block_uncached(&self, block: u64) -> [u32; 4] {
            philox4x32_10(
                [
                    block as u32,
                    (block >> 32) as u32,
                    self.domain as u32,
                    (self.domain >> 32) as u32,
                ],
                self.key,
            )
        }
    }

    fn fill_via(rng: &mut TapeRng, path: Option<FillBackend>, dst: &mut [u64]) {
        match path {
            Some(backend) => rng.fill_with(backend, dst),
            None => rng.fill(dst),
        }
    }

    fn fill_at_via(rng: &TapeRng, path: Option<FillBackend>, position: u64, dst: &mut [u64]) {
        match path {
            Some(backend) => rng.fill_at_with(backend, position, dst),
            None => rng.fill_at(position, dst),
        }
    }

    /// The selector names the backend its documentation names for this build
    /// and CPU. This pins the policy only: that `fill` and `fill_at` route
    /// through `selected()` is a one-line call each, and since every backend
    /// gives the same words, no value pin can observe which one ran.
    #[test]
    fn the_fill_selects_the_documented_backend() {
        let selected = FillBackend::selected();
        if cfg!(debug_assertions) {
            assert_eq!(selected, FillBackend::Scalar);
            return;
        }
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            assert_eq!(selected, FillBackend::Avx2);
            return;
        }
        assert_eq!(selected, FillBackend::Portable);
    }

    /// Fill against `next_word` for both start parities, every length from 0
    /// through three batches plus odd lengths past that, at starts near zero
    /// and around the low counter-word carry; the position lands where the
    /// same number of `next_word` calls would leave it, and `fill_at` reads
    /// the same words without moving the stream.
    #[test]
    fn fill_matches_next_word_for_every_start_and_length() {
        // Every tail width (one to four chains) and every full-batch boundary.
        let batch = 2 * MAX_CHAIN_BLOCKS;
        let carry_position = 2 * ((1u64 << 32) - 1);
        let mut starts: Vec<u64> = (0..=4).collect();
        for back in 0..=(2 * batch as u64 + 1) {
            starts.push(carry_position + 2 - back);
        }
        let mut lengths: Vec<usize> = (0..=3 * batch + 1).collect();
        lengths.extend([5 * batch - 1, 7 * batch + 3, 101]);
        for path in fill_paths() {
            for &start in &starts {
                for &len in &lengths {
                    let expected = reference(9, 4, start, len);
                    let mut rng = TapeRng::new(9, 4);
                    rng.seek(start);
                    let mut got = vec![0u64; len];
                    // Warm the cache on a block the fill does not touch, so an
                    // empty fill must leave exactly this entry behind.
                    let warm = start / 2 + 1000;
                    rng.cached = Some((warm, rng.block_uncached(warm)));
                    fill_via(&mut rng, path, &mut got);
                    assert_eq!(got, expected, "{path:?} start {start} len {len}");
                    assert_eq!(rng.position(), start + len as u64);
                    // The cache holds the block of the last word written, its
                    // words computed at its own address; untouched when empty.
                    let held = if len == 0 {
                        warm
                    } else {
                        (start + len as u64 - 1) >> 1
                    };
                    assert_eq!(
                        rng.cached,
                        Some((held, rng.block_uncached(held))),
                        "{path:?} cache after start {start} len {len}"
                    );
                    let before = rng.cached;
                    let mut at = vec![0u64; len];
                    fill_at_via(&rng, path, start, &mut at);
                    assert_eq!(at, expected, "{path:?} fill_at start {start} len {len}");
                    assert_eq!(rng.position(), start + len as u64);
                    assert_eq!(rng.cached, before, "{path:?} fill_at moved the cache");
                    // The read after a fill continues the stream, so the cache
                    // the fill left must name its block truthfully.
                    assert_eq!(
                        rng.next_word(),
                        reference(9, 4, start + len as u64, 1)[0],
                        "{path:?} after start {start} len {len}"
                    );
                }
            }
        }
    }

    /// Fills, single reads, seeks and clones interleaved in a deterministic
    /// pseudo-random script, against a stream that only ever calls
    /// `next_word`. Seeks land backward, forward, at odd positions and inside
    /// the block a fill last cached, so a stale cache entry would surface as
    /// a wrong lane.
    #[test]
    fn interleaved_fills_reads_seeks_and_clones_never_serve_a_stale_lane() {
        for path in fill_paths() {
            interleaved_script(path);
        }
    }

    fn interleaved_script(path: Option<FillBackend>) {
        let words = reference(11, 2, 0, 600);
        let mut rng = TapeRng::new(11, 2);
        let mut script = TapeRng::new(12345, 0);
        let mut position = 0u64;
        let mut buf = Vec::new();
        for _ in 0..2000 {
            let op = script.next_word();
            match op % 5 {
                0 | 1 => {
                    let len = ((op >> 8) % 40) as usize;
                    if position as usize + len > words.len() {
                        continue;
                    }
                    buf.resize(len, 0);
                    fill_via(&mut rng, path, &mut buf);
                    let at = position as usize;
                    assert_eq!(
                        buf[..],
                        words[at..at + len],
                        "{path:?} fill at {at} len {len}"
                    );
                    position += len as u64;
                }
                2 => {
                    if position as usize >= words.len() {
                        continue;
                    }
                    assert_eq!(rng.next_word(), words[position as usize]);
                    position += 1;
                }
                3 => {
                    // Seeks favour the neighbourhood of the current position,
                    // where the cached block lives.
                    let near = (op >> 8) % 8;
                    position = if (op >> 16) & 1 == 0 {
                        position.saturating_sub(near)
                    } else {
                        ((op >> 20) % 560) + near
                    };
                    rng.seek(position);
                }
                _ => {
                    let mut copy = rng.clone();
                    let len = ((op >> 8) % 20) as usize;
                    if position as usize + len > words.len() {
                        continue;
                    }
                    buf.resize(len, 0);
                    fill_via(&mut copy, path, &mut buf);
                    let at = position as usize;
                    assert_eq!(buf[..], words[at..at + len], "{path:?} clone fill at {at}");
                    // The original is untouched by its clone's fill.
                    assert_eq!(rng.position(), position);
                }
            }
        }
    }

    /// Exhaustion at the top of the position space: a fill ending exactly at
    /// `u64::MAX` succeeds, as that many `next_word` calls do; a fill reaching
    /// one further panics with the same message, after delivering the
    /// readable prefix and stopping the position at `u64::MAX`, exactly as the
    /// equivalent run of `next_word` calls leaves things; `fill_at` refuses
    /// the same ranges.
    #[test]
    fn fill_exhausts_exactly_where_next_word_does() {
        for path in fill_paths() {
            exhaustion_script(path);
        }
    }

    fn exhaustion_script(path: Option<FillBackend>) {
        use std::panic::{AssertUnwindSafe, catch_unwind};

        let top = u64::MAX - 3;
        let expected = reference(5, 1, top, 3);
        let mut rng = TapeRng::new(5, 1);
        rng.seek(top);
        let mut three = [0u64; 3];
        fill_via(&mut rng, path, &mut three);
        assert_eq!(three[..], expected[..], "{path:?}");
        assert_eq!(rng.position(), u64::MAX);
        let mut empty: [u64; 0] = [];
        fill_via(&mut rng, path, &mut empty);
        fill_at_via(&rng, path, u64::MAX, &mut empty);

        let mut by_words = TapeRng::new(5, 1);
        by_words.seek(top);
        let mut delivered = Vec::new();
        let words_panic = catch_unwind(AssertUnwindSafe(|| {
            for _ in 0..40 {
                delivered.push(by_words.next_word());
            }
        }));
        assert!(words_panic.is_err());

        let mut by_fill = TapeRng::new(5, 1);
        by_fill.seek(top);
        const SENTINEL: u64 = 0x5EE5_5EE5_5EE5_5EE5;
        let mut long = [SENTINEL; 40];
        let fill_panic = catch_unwind(AssertUnwindSafe(|| fill_via(&mut by_fill, path, &mut long)));
        let message = fill_panic.expect_err("a fill past the space must refuse");
        assert!(
            message
                .downcast_ref::<String>()
                .is_some_and(|m| m.contains("exhausted its u64 position space"))
        );
        assert_eq!(long[..3], delivered[..]);
        assert!(
            long[3..].iter().all(|w| *w == SENTINEL),
            "a refused fill wrote past its readable prefix"
        );
        assert_eq!(by_fill.position(), by_words.position());
        assert_eq!(by_fill.position(), u64::MAX);

        // Other readable prefixes, of both parities: each delivers exactly
        // what `next_word` delivers before refusing.
        // Prefixes reaching every tail width and a full 32-word batch.
        for readable in [0_u64, 1, 2, 5, 8, 9, 17, 24, 25, 31, 32, 33, 47] {
            let start = u64::MAX - readable;
            let mut words = TapeRng::new(5, 1);
            words.seek(start);
            let mut prefix = Vec::new();
            drop(catch_unwind(AssertUnwindSafe(|| {
                for _ in 0..80 {
                    prefix.push(words.next_word());
                }
            })));
            // A fill of exactly the readable words succeeds and ends at the
            // top, its widest tail's unused lanes past the space untouched.
            let mut exact_fill = TapeRng::new(5, 1);
            exact_fill.seek(start);
            let mut exact = vec![0_u64; readable as usize];
            fill_via(&mut exact_fill, path, &mut exact);
            assert_eq!(exact[..], prefix[..], "{path:?} exact readable {readable}");
            assert_eq!(exact_fill.position(), u64::MAX);
            let mut filled = TapeRng::new(5, 1);
            filled.seek(start);
            let mut out = [SENTINEL; 80];
            assert!(
                catch_unwind(AssertUnwindSafe(|| fill_via(&mut filled, path, &mut out))).is_err()
            );
            let delivered = prefix.len();
            assert_eq!(out[..delivered], prefix[..], "{path:?} readable {readable}");
            assert!(
                out[delivered..].iter().all(|w| *w == SENTINEL),
                "readable {readable}"
            );
            assert_eq!(filled.position(), words.position(), "readable {readable}");
        }

        // A refused `fill_at` writes nothing at all.
        let fresh = TapeRng::new(5, 1);
        let mut four = [SENTINEL; 4];
        assert!(
            catch_unwind(AssertUnwindSafe(|| fill_at_via(
                &fresh, path, top, &mut four
            )))
            .is_err()
        );
        assert!(
            four.iter().all(|w| *w == SENTINEL),
            "a refused fill_at wrote"
        );
    }

    #[test]
    #[should_panic(expected = "exhausted its u64 position space")]
    fn the_last_position_refuses_rather_than_wraps() {
        let mut rng = TapeRng::new(1, 0);
        rng.seek(u64::MAX);
        rng.next_word();
    }

    /// The AVX2 kernel against the scalar block, every lane of every chain
    /// width, across the low counter word's carry and the top of the block
    /// range. Skipped, and says so, on a CPU without AVX2, where the portable
    /// kernel's pin covers the fill instead.
    #[test]
    fn the_avx2_kernel_matches_the_scalar_block_on_every_lane() {
        #[cfg(target_arch = "x86_64")]
        if std::arch::is_x86_feature_detected!("avx2") {
            check_chains::<4>();
            check_chains::<8>();
            check_chains::<12>();
            check_chains::<16>();
            return;
        }
        eprintln!("no AVX2 on this CPU: the AVX2 kernel was not exercised");
    }

    #[cfg(target_arch = "x86_64")]
    fn check_chains<const N: usize>() {
        {
            for key in [[0, 0], [u32::MAX, u32::MAX], [0xA409_3822, 0x299F_31D0]] {
                for domain in [0, 7, 0xFEED_FACE_0000_0007, u64::MAX] {
                    let starts = (0..64).chain((1 << 32) - 18..(1 << 32) + 2);
                    for block in starts.chain([u64::MAX / 2 + 1 - N as u64]) {
                        // SAFETY: the caller checked for AVX2 on this CPU.
                        let lanes = unsafe { philox4x32_10_avx2::<N>(block, domain, key) };
                        for i in 0..N {
                            let b = block + i as u64;
                            let counter = [
                                b as u32,
                                (b >> 32) as u32,
                                domain as u32,
                                (domain >> 32) as u32,
                            ];
                            assert_eq!(
                                [lanes[0][i], lanes[1][i], lanes[2][i], lanes[3][i]],
                                philox4x32_10(counter, key),
                                "{N} blocks from {block}, lane {i}"
                            );
                        }
                    }
                }
            }
        }
    }

    /// Every backend in runs of 12 (a legacy packet), 17 and 64 (size fills),
    /// 120M words in a release build and 12M in a dev build, with checksums
    /// that must agree across backends. It prints walls; the dev and release
    /// rows are what `FillBackend::selected` is chosen on.
    #[test]
    #[ignore = "bench: prints wall measurements per fill backend"]
    fn fill_walls() {
        let total: usize = if cfg!(debug_assertions) {
            12_000_000
        } else {
            120_000_000
        };
        for run in [12_usize, 17, 64] {
            let mut sums = Vec::new();
            for path in fill_paths().into_iter().flatten() {
                let mut rng = TapeRng::new(3, 9);
                let mut buf = vec![0_u64; run];
                let mut sum = 0_u64;
                let start = std::time::Instant::now();
                for _ in 0..total / run {
                    rng.fill_with(path, &mut buf);
                    // The whole slice, so no word's work can be discarded.
                    std::hint::black_box(&buf);
                    sum = sum.wrapping_add(buf[run - 1]);
                }
                println!(
                    "{path:?} runs of {run}: {:?}, checksum {sum}",
                    start.elapsed()
                );
                sums.push(sum);
            }
            assert!(sums.windows(2).all(|w| w[0] == w[1]), "backends disagree");
        }
    }
}
