// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! CPython's `random.Random` to the extent `mnq_fit.py` uses it: seeding
//! from a small non-negative integer and `choice` over a sequence.
//!
//! This exists for exactly one estimator - `minute_range_envelope`, which
//! draws `RESAMPLE_SESSIONS_PER_REPLICATE` session labels with replacement
//! for each of `RESAMPLE_REPLICATES` replicates under `random.Random(1)` -
//! and its output lands in the committed `analysis/mnq-fit.json` as the
//! one-sided upper bounds the minute-range gates judge against. Nothing
//! short of the exact Mersenne Twister stream reproduces those bounds, so
//! the generator is ported rather than approximated.
//!
//! Two CPython details the port has to carry, both surprising:
//!
//! - `Random.seed(n)` for an integer does not call `init_genrand(n)`. It
//!   calls `init_by_array` with the key being `abs(n)` split into 32-bit
//!   little-endian words, so `Random(1)` is `init_by_array([1])`.
//! - `choice` is not `int(random() * len)`. Since 3.2 it is
//!   `seq[self._randbelow(len(seq))]`, a rejection sampler over
//!   `getrandbits(n.bit_length())` - so it consumes a variable number of
//!   32-bit outputs per call.

const N: usize = 624;
const M: usize = 397;
const MATRIX_A: u32 = 0x9908_b0df;
const UPPER_MASK: u32 = 0x8000_0000;
const LOWER_MASK: u32 = 0x7fff_ffff;

/// The Mersenne Twister behind `random.Random`, seeded CPython's way.
pub struct PyRandom {
    mt: [u32; N],
    index: usize,
}

impl PyRandom {
    /// `random.Random(seed)` for a non-negative integer seed.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        // CPython splits abs(seed) into 32-bit little-endian words; a zero
        // seed keys on a single zero word.
        let mut key: Vec<u32> = Vec::new();
        let mut n = seed;
        loop {
            key.push((n & 0xffff_ffff) as u32);
            n >>= 32;
            if n == 0 {
                break;
            }
        }
        let mut rng = Self {
            mt: [0; N],
            index: N,
        };
        rng.init_by_array(&key);
        rng
    }

    fn init_genrand(&mut self, s: u32) {
        self.mt[0] = s;
        for i in 1..N {
            let prev = self.mt[i - 1];
            self.mt[i] = 1_812_433_253u32
                .wrapping_mul(prev ^ (prev >> 30))
                .wrapping_add(i as u32);
        }
        self.index = N;
    }

    fn init_by_array(&mut self, key: &[u32]) {
        self.init_genrand(19_650_218);
        let mut i = 1usize;
        let mut j = 0usize;
        let mut k = N.max(key.len());
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_664_525))
                .wrapping_add(key[j])
                .wrapping_add(j as u32);
            i += 1;
            j += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            if j >= key.len() {
                j = 0;
            }
            k -= 1;
        }
        let mut k = N - 1;
        while k > 0 {
            let prev = self.mt[i - 1];
            self.mt[i] = (self.mt[i] ^ (prev ^ (prev >> 30)).wrapping_mul(1_566_083_941))
                .wrapping_sub(i as u32);
            i += 1;
            if i >= N {
                self.mt[0] = self.mt[N - 1];
                i = 1;
            }
            k -= 1;
        }
        self.mt[0] = 0x8000_0000;
        self.index = N;
    }

    fn generate(&mut self) {
        for i in 0..N {
            let y = (self.mt[i] & UPPER_MASK) | (self.mt[(i + 1) % N] & LOWER_MASK);
            let mut next = self.mt[(i + M) % N] ^ (y >> 1);
            if y & 1 != 0 {
                next ^= MATRIX_A;
            }
            self.mt[i] = next;
        }
        self.index = 0;
    }

    /// One tempered 32-bit output, `genrand_uint32`.
    pub fn next_u32(&mut self) -> u32 {
        if self.index >= N {
            self.generate();
        }
        let mut y = self.mt[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// `getrandbits(k)`, CPython's word layout: one 32-bit draw per word,
    /// the first draw carrying the least significant word, and a partial
    /// final word taken from the high bits of its draw.
    pub fn getrandbits(&mut self, k: u32) -> u64 {
        assert!(k > 0 && k <= 64, "only the widths this port needs");
        let mut remaining = k;
        let mut out: u64 = 0;
        let mut shift = 0u32;
        while remaining > 0 {
            let mut r = self.next_u32();
            if remaining < 32 {
                r >>= 32 - remaining;
            }
            out |= u64::from(r) << shift;
            shift += 32;
            remaining = remaining.saturating_sub(32);
        }
        out
    }

    /// `random.random()`: CPython's 53-bit double from two tempered draws.
    ///
    /// The composition is load-bearing and not interchangeable with any other
    /// way of building a double from 64 random bits - the first draw supplies
    /// the high 27 bits and the second the low 26, so a port that swaps them
    /// or takes one 64-bit draw consumes the same stream and produces a
    /// different sequence.
    pub fn random(&mut self) -> f64 {
        let a = f64::from(self.next_u32() >> 5);
        let b = f64::from(self.next_u32() >> 6);
        (a * 67_108_864.0 + b) * (1.0 / 9_007_199_254_740_992.0)
    }

    /// `random.weibullvariate(alpha, beta)`.
    ///
    /// CPython computes `alpha * (-log(1.0 - random())) ** (1.0 / beta)`. The
    /// `1.0 - u` is not cosmetic: it moves the open endpoint so the logarithm
    /// can never see zero. At `beta == 1.0` the exponent is exactly 1.0 and
    /// `powf` is the identity, which is the case the shipped feasibility path
    /// uses - so that path carries no dependence on libm `pow` at all.
    pub fn weibullvariate(&mut self, alpha: f64, beta: f64) -> f64 {
        let u = 1.0 - self.random();
        alpha * (-u.ln()).powf(1.0 / beta)
    }

    /// `_randbelow_with_getrandbits(n)`: rejection sampling over exactly
    /// `n.bit_length()` bits.
    pub fn randbelow(&mut self, n: u64) -> u64 {
        if n == 0 {
            return 0;
        }
        let k = 64 - n.leading_zeros();
        loop {
            let r = self.getrandbits(k);
            if r < n {
                return r;
            }
        }
    }

    /// `random.choice(seq)`: index by `randbelow(len(seq))`.
    pub fn choice_index(&mut self, len: usize) -> usize {
        assert!(len > 0, "cannot choose from an empty sequence");
        self.randbelow(len as u64) as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A prefix test for `random()`, pinned bit for bit against
    /// `random.Random(42)` in CPython 3.14.6. Six draws, because the failure
    /// mode this guards against - swapping which draw supplies the high bits -
    /// still produces plausible uniforms and only shows up as a different
    /// sequence.
    #[test]
    fn random_reproduces_the_cpython_stream() {
        let mut rng = PyRandom::new(42);
        let expected = [
            0x3fe4_762f_3072_00c5u64, // 0.6394267984578837
            0x3f99_9c6b_5eeb_2060,    // 0.025010755222666936
            0x3fd1_9a14_91f5_89dc,    // 0.27502931836911926
            0x3fcc_922b_623b_8c1c,    // 0.22321073814882275
            0x3fe7_912c_1468_f47e,    // 0.7364712141640124
            0x3fe5_a785_aef6_719a,    // 0.6766994874229113
        ];
        for (i, bits) in expected.into_iter().enumerate() {
            assert_eq!(rng.random().to_bits(), bits, "draw {i}");
        }
    }

    /// A prefix test for `weibullvariate(1.0, 1.0)`, the shape the shipped
    /// feasibility path uses. Same seed as above, so it also proves the
    /// helper consumes exactly one `random()` per call - a port that drew
    /// twice would still look exponential but would desynchronize the
    /// simulation from Python at the first state transition.
    #[test]
    fn weibullvariate_reproduces_the_cpython_stream_at_unit_shape() {
        let mut rng = PyRandom::new(42);
        let expected = [
            0x3ff0_522a_bc5c_b2bbu64, // 1.020060287274801
            0x3f99_efcd_9d56_3b89,    // 0.02532883904273889
            0x3fd4_957d_1933_bfde,    // 0.3216240640749656
            0x3fd0_2a5f_3fb7_f829,    // 0.25258618567011354
        ];
        for (i, bits) in expected.into_iter().enumerate() {
            assert_eq!(rng.weibullvariate(1.0, 1.0).to_bits(), bits, "draw {i}");
        }
    }

    /// Pinned against CPython: `random.Random(1).getrandbits(32)` and the
    /// two draws after it. Reproduced by running the stdlib generator, not
    /// by reading this implementation.
    #[test]
    fn random_one_matches_cpython_getrandbits_32() {
        let mut r = PyRandom::new(1);
        assert_eq!(r.getrandbits(32), 577_090_037);
        assert_eq!(r.getrandbits(32), 2_444_712_010);
        assert_eq!(r.getrandbits(32), 3_639_700_191);
    }

    /// Pinned against CPython:
    /// `[random.Random(1).choice(range(22)) for ...]` over one generator.
    #[test]
    fn random_one_choice_over_22_matches_cpython() {
        let mut r = PyRandom::new(1);
        let got: Vec<usize> = (0..10).map(|_| r.choice_index(22)).collect();
        assert_eq!(got, vec![4, 18, 2, 8, 3, 15, 14, 15, 20, 12]);
    }
}
