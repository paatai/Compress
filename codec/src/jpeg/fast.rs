//! Fast coefficient model for lossless JPEG recompression.
//!
//! Each coefficient is one symbol (zero, or a magnitude class that includes the top mantissa
//! bit) from an adaptive distribution chosen by its context, so a coefficient costs one decoding
//! step instead of a chain of binary decisions. Each block is coded as: the number of nonzero AC
//! coefficients (two symbols, conditioned on the neighbours' counts), the AC coefficients in
//! zigzag order until all nonzero ones are coded (context: position, number of nonzero
//! coefficients left, magnitude of the same coefficient in the blocks above and to the left),
//! then DC as the difference from a prediction from the neighbouring DC values. Signs and lower
//! mantissa bits, which are close to random, go to a separate stream of raw bits.
//!
//! Symbols are coded with two interleaved rANS states (the decoder finds a symbol by comparing
//! the state's slot with the cumulative frequencies, without division). The encoder runs the
//! model forward, records each symbol's frequency interval, and encodes them in reverse.

use super::format::NATURAL;

const PROB_BITS: u32 = 15;
const PROB_ONE: u32 = 1 << PROB_BITS;
const RANS_L: u32 = 1 << 16;

/// Adaptive distribution over up to 15 symbols: `c[j]` is the cumulative frequency of the
/// symbols before symbol j (out of 2^15), so symbol s covers `c[s]..c[s + 1]`; `c[0] = 0` and
/// `c[j] = 2^15` from the end of the last symbol on. Every symbol keeps a frequency of at least
/// 1. The number of updates is kept separately (see `Table`).
#[derive(Clone, Copy)]
#[repr(C, align(32))]
pub struct Cdf([u16; 16]);

const LIMIT: u8 = 62;

/// Update shift after n updates: log2(n + 2) + 1, between 3 and 8 (at most 7 with the counts
/// capped at `LIMIT`).
static SHIFT: [u32; 128] = {
    let mut r = [0u32; 128];
    let mut i = 0;
    while i < 128 {
        let mut s = 0u32;
        while (1usize << (s + 1)) <= i + 2 {
            s += 1;
        }
        s += 1;
        r[i] = if s < 3 {
            3
        } else if s > 8 {
            8
        } else {
            s
        };
        i += 1;
    }
    r
};

/// Targets of the update for an alphabet of n symbols: the starts of symbols up to the coded one
/// move towards LO, the others towards HI, which keeps one unit of frequency for every symbol.
struct Targets {
    lo: [[u16; 16]; 16],
    hi: [[u16; 16]; 16],
}
static TARGETS: Targets = {
    let mut lo = [[0u16; 16]; 16];
    let mut hi = [[0u16; 16]; 16];
    let mut n = 1;
    while n < 16 {
        let mut j = 1;
        while j < 16 {
            if j < n {
                lo[n][j] = j as u16;
                hi[n][j] = (PROB_ONE - (n - j) as u32) as u16;
            } else {
                lo[n][j] = PROB_ONE as u16;
                hi[n][j] = PROB_ONE as u16;
            }
            j += 1;
        }
        n += 1;
    }
    Targets { lo, hi }
};

impl Cdf {
    /// A distribution over `n` symbols with initial weights `w` (any scale).
    pub fn new(n: usize, w: &[u32]) -> Cdf {
        assert!((2..=15).contains(&n) && w.len() == n);
        let total: u64 = w.iter().map(|&x| x as u64 + 1).sum();
        let mut c = [PROB_ONE as u16; 16];
        c[0] = 0;
        let room = (PROB_ONE - n as u32) as u64;
        let mut acc = 0u64;
        for j in 1..n {
            acc += w[j - 1] as u64 + 1;
            c[j] = (acc * room / total) as u16 + j as u16;
        }
        Cdf(c)
    }
    /// The symbol whose interval contains `slot` (< 2^15): one less than the number of
    /// cumulative frequencies at or below it, which form a prefix because they increase.
    #[inline(always)]
    fn find<L: Lanes>(&self, slot: u32) -> usize {
        L::find(&self.0, slot as u16) - 1
    }
    #[inline(always)]
    fn interval(&self, s: usize) -> (u32, u32) {
        let start = self.0[s & 15] as u32;
        (start, self.0[(s + 1) & 15] as u32 - start)
    }
    /// Moves every cumulative frequency towards its target by 2^-shift of the distance; `cnt`
    /// counts the updates.
    #[inline(always)]
    fn update<L: Lanes>(&mut self, cnt: &mut u8, s: usize, n: usize) {
        L::update(&mut self.0, s + 1, &TARGETS.lo[n & 15], &TARGETS.hi[n & 15], SHIFT[(*cnt & 127) as usize]);
        *cnt += (*cnt < LIMIT) as u8;
    }
}

/// Collect symbol statistics for training priors (`Model::stats`).
pub static TRAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn training() -> bool {
    TRAIN.load(std::sync::atomic::Ordering::Relaxed)
}

/// Distributions with their update counts.
#[derive(Clone)]
pub struct Table {
    cdf: Vec<Cdf>,
    cnt: Vec<u8>,
    n: usize,
    /// symbol counts per distribution (training only)
    stats: Vec<u32>,
}

impl Table {
    fn new(len: usize, n: usize, init: Cdf) -> Table {
        Table { cdf: vec![init; len], cnt: vec![0; len], n, stats: if training() { vec![0; len * n] } else { Vec::new() } }
    }
    /// Codes symbol `s` of `n` with distribution `i` (an index bounded by construction).
    #[inline(always)]
    fn sym<I: Io>(&mut self, io: &mut I, i: usize, n: usize, s: usize) -> usize {
        debug_assert!(i < self.cdf.len() && n == self.n);
        if !I::DEC && !self.stats.is_empty() {
            self.stats[i * n + s] += 1;
        }
        // SAFETY: every index is computed from components that are each masked or clamped to
        // the table's dimensions (checked by the debug assertion); `cnt` has the same length.
        let (c, k) = unsafe { (self.cdf.get_unchecked_mut(i), self.cnt.get_unchecked_mut(i)) };
        io.sym(c, k, n, s)
    }
    /// Codes symbol `s` of `n` with the mean of distribution `i` and distribution `j` of `t2`
    /// (indices bounded by construction); both adapt to it.
    #[inline(always)]
    fn sym2<I: Io>(&mut self, t2: &mut Table, io: &mut I, i: usize, j: usize, n: usize, s: usize) -> usize {
        debug_assert!(i < self.cdf.len() && j < t2.cdf.len() && n == self.n && n == t2.n);
        if !I::DEC && !self.stats.is_empty() {
            self.stats[i * n + s] += 1;
            t2.stats[j * n + s] += 1;
        }
        // SAFETY: as in `sym`.
        let (c1, k1, c2, k2) = unsafe { (self.cdf.get_unchecked_mut(i), self.cnt.get_unchecked_mut(i), t2.cdf.get_unchecked_mut(j), t2.cnt.get_unchecked_mut(j)) };
        let s = io.code(&Cdf(I::L::mean2(&c1.0, &c2.0)), s);
        c1.update::<I::L>(k1, s, n);
        c2.update::<I::L>(k2, s, n);
        s
    }
    /// The mean of distribution `i`, distribution `j` of `t2` and `k` of `t3` (see `Lanes::mean3`).
    #[inline(always)]
    fn mean3<L: Lanes>(&self, t2: &Table, t3: &Table, i: usize, j: usize, k: usize) -> Cdf {
        debug_assert!(i < self.cdf.len() && j < t2.cdf.len() && k < t3.cdf.len());
        // SAFETY: as in `sym`.
        unsafe { Cdf(L::mean3(&self.cdf.get_unchecked(i).0, &t2.cdf.get_unchecked(j).0, &t3.cdf.get_unchecked(k).0)) }
    }
    /// As `sym2` with three distributions, coding with their `mean3` `m` (which may be computed
    /// before it is known whether the symbol is coded, so that it is ready).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn sym3<I: Io>(&mut self, t2: &mut Table, t3: &mut Table, io: &mut I, m: &Cdf, i: usize, j: usize, k: usize, n: usize, s: usize) -> usize {
        debug_assert!(i < self.cdf.len() && j < t2.cdf.len() && k < t3.cdf.len() && n == self.n && n == t2.n && n == t3.n);
        if !I::DEC && !self.stats.is_empty() {
            self.stats[i * n + s] += 1;
            t2.stats[j * n + s] += 1;
            t3.stats[k * n + s] += 1;
        }
        // SAFETY: as in `sym`.
        let (c1, k1, c2, k2, c3, k3) = unsafe { (self.cdf.get_unchecked_mut(i), self.cnt.get_unchecked_mut(i), t2.cdf.get_unchecked_mut(j), t2.cnt.get_unchecked_mut(j), t3.cdf.get_unchecked_mut(k), t3.cnt.get_unchecked_mut(k)) };
        let s = io.code(m, s);
        c1.update::<I::L>(k1, s, n);
        c2.update::<I::L>(k2, s, n);
        c3.update::<I::L>(k3, s, n);
        s
    }
    fn load(&mut self, p: &mut &[u8]) -> Option<()> {
        for (c, k) in self.cdf.iter_mut().zip(self.cnt.iter_mut()) {
            let (b, rest) = p.split_at_checked(2 * (self.n - 1) + 1)?;
            for j in 1..self.n {
                c.0[j] = u16::from_le_bytes([b[2 * j - 2], b[2 * j - 1]]);
            }
            *k = b[2 * (self.n - 1)].min(LIMIT);
            *p = rest;
        }
        Some(())
    }
}

/// Adaptive binary probability: P(0) out of 2^15, within [1, 2^15 - 1] (see `adapt_bit`).
pub type Bit = u16;
const BIT_INIT: Bit = 16384;
// fast: fixed-rate probabilities only code the header and trailer bytes, which are few
const BIT_SHIFT: u32 = 4;

/// Moves P(0) towards the coded bit by 2^-BIT_SHIFT of the distance (never past the bounds).
#[inline(always)]
fn adapt_bit(p: Bit, b: u32) -> Bit {
    let q = p as i32;
    let t = std::hint::select_unpredictable(b != 0, 1, PROB_ONE as i32 - 1);
    (q + ((t - q) >> BIT_SHIFT)) as Bit
}

/// Binary probabilities.
#[derive(Clone)]
pub struct Bits {
    p: Vec<Bit>,
}

impl Bits {
    fn new(len: usize) -> Bits {
        Bits { p: vec![BIT_INIT; len] }
    }
    /// Codes bit `b` with the mean of probability `i` and probability `j` of `t2`.
    #[inline(always)]
    fn bit2<I: Io>(&mut self, t2: &mut Bits, io: &mut I, i: usize, j: usize, b: u32) -> u32 {
        let (p, q) = (&mut self.p[i], &mut t2.p[j]);
        let r = io.bit(&mut (((*p as u32 + *q as u32 + 1) >> 1) as Bit), b);
        *p = adapt_bit(*p, r);
        *q = adapt_bit(*q, r);
        r
    }
}

/// Update shift of a counted binary probability after n updates: log2(n + 2) + 1, between 4
/// and 8.
static BIT_SHIFTS: [u32; 128] = {
    let mut r = [0u32; 128];
    let mut i = 0;
    while i < 128 {
        let mut s = 0u32;
        while (1usize << (s + 1)) <= i + 2 {
            s += 1;
        }
        s += 1;
        r[i] = if s < 4 {
            4
        } else if s > 8 {
            8
        } else {
            s
        };
        i += 1;
    }
    r
};
const BIT_LIMIT: u8 = 126;

/// Binary probabilities that adapt more slowly the more often they have been updated.
#[derive(Clone)]
pub struct CountedBits {
    p: Vec<Bit>,
    cnt: Vec<u8>,
    /// counts of zeros and ones per probability (training only)
    stats: Vec<u32>,
}

impl CountedBits {
    fn new(len: usize) -> CountedBits {
        CountedBits { p: vec![BIT_INIT; len], cnt: vec![0; len], stats: if training() { vec![0; len * 2] } else { Vec::new() } }
    }
    /// Codes bit `b` with probability `i` (an index bounded by construction).
    #[inline(always)]
    fn bit<I: Io>(&mut self, io: &mut I, i: usize, b: u32) -> u32 {
        debug_assert!(i < self.p.len());
        if !I::DEC && !self.stats.is_empty() {
            self.stats[i * 2 + b as usize] += 1;
        }
        // SAFETY: every index is computed from components that are each masked or clamped to
        // the table's dimensions (checked by the debug assertion); `cnt` has the same length.
        let (p, k) = unsafe { (self.p.get_unchecked_mut(i), self.cnt.get_unchecked_mut(i)) };
        // the coder's own update (at the fixed rate) goes to a copy
        let r = io.bit(&mut { *p }, b);
        let t = std::hint::select_unpredictable(r != 0, 1, PROB_ONE as i32 - 1);
        *p = (*p as i32 + ((t - *p as i32) >> BIT_SHIFTS[(*k & 127) as usize])) as Bit;
        *k += (*k < BIT_LIMIT) as u8;
        r
    }
    /// Codes bit `b` with the mean of probability `i` and probability `j` of `t2` (indices
    /// bounded by construction), `w2` quarters of it from the second (2 or 3); both adapt to it.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn bit2<I: Io>(&mut self, t2: &mut CountedBits, io: &mut I, i: usize, j: usize, w2: u32, b: u32) -> u32 {
        debug_assert!(i < self.p.len() && j < t2.p.len() && (w2 == 2 || w2 == 3));
        if !I::DEC && !self.stats.is_empty() {
            self.stats[i * 2 + b as usize] += 1;
            t2.stats[j * 2 + b as usize] += 1;
        }
        // SAFETY: as in `bit`.
        let (p, k, q, l) = unsafe { (self.p.get_unchecked_mut(i), self.cnt.get_unchecked_mut(i), t2.p.get_unchecked_mut(j), t2.cnt.get_unchecked_mut(j)) };
        let m = (*p as u32 + *q as u32 + 1) >> 1;
        let m = if w2 == 3 { (m + *q as u32 + 1) >> 1 } else { m };
        let r = io.bit(&mut (m as Bit), b);
        let t = std::hint::select_unpredictable(r != 0, 1, PROB_ONE as i32 - 1);
        *p = (*p as i32 + ((t - *p as i32) >> BIT_SHIFTS[(*k & 127) as usize])) as Bit;
        *k += (*k < BIT_LIMIT) as u8;
        *q = (*q as i32 + ((t - *q as i32) >> BIT_SHIFTS[(*l & 127) as usize])) as Bit;
        *l += (*l < BIT_LIMIT) as u8;
        r
    }
    fn load(&mut self, p: &mut &[u8]) -> Option<()> {
        for (x, k) in self.p.iter_mut().zip(self.cnt.iter_mut()) {
            let (b, rest) = p.split_at_checked(3)?;
            *x = u16::from_le_bytes([b[0], b[1]]).clamp(1, PROB_ONE as u16 - 1);
            *k = b[2].min(BIT_LIMIT);
            *p = rest;
        }
        Some(())
    }
}

/// The vector operations on a distribution: `find` counts the cumulative frequencies at or
/// below a slot, `update` moves lanes from `j` on towards `hi` and the others towards `lo`,
/// `mean2` averages two distributions over the same alphabet, rounding up (so every symbol
/// keeps a frequency of at least 1). Every implementation computes the same integers.
pub trait Lanes {
    fn find(c: &[u16; 16], slot: u16) -> usize;
    fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32);
    fn mean2(a: &[u16; 16], b: &[u16; 16]) -> [u16; 16];
    /// 3/8 of `a` and of `b` and 1/4 of `c`: `mean2(ab, mean2(ab, c))` with `ab = mean2(a, b)`
    /// (about as good as thirds, and quicker).
    fn mean3(a: &[u16; 16], b: &[u16; 16], c: &[u16; 16]) -> [u16; 16];
    /// For each column u (`rows` false) the sums over the rows g of x[g][u] w[0][g][u] -
    /// y[g][u] w[2][g][u] and of x[g][u] w[1][g][u] - y[g][u] w[3][g][u], or the same for each
    /// row over the columns (`rows` true), wrapping (natural order).
    #[inline(always)]
    fn sums(x: &[i16; 64], y: &[i16; 64], w: [&[i32; 64]; 4], rows: bool) -> [[i32; 8]; 2] {
        let mut s = [[0i32; 8]; 2];
        for k in 0..64 {
            let (a, b, i) = (x[k] as i32, y[k] as i32, if rows { k / 8 } else { k % 8 });
            s[0][i] = s[0][i].wrapping_add(a.wrapping_mul(w[0][k])).wrapping_sub(b.wrapping_mul(w[2][k]));
            s[1][i] = s[1][i].wrapping_add(a.wrapping_mul(w[1][k])).wrapping_sub(b.wrapping_mul(w[3][k]));
        }
        s
    }
}

/// SSE2 on x86-64, SIMD128 on WebAssembly, scalar elsewhere.
pub struct Base;

impl Lanes for Base {
    #[inline(always)]
    fn find(c: &[u16; 16], slot: u16) -> usize {
        simd::find(c, slot)
    }
    #[inline(always)]
    fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32) {
        simd::update(c, j, lo, hi, shift)
    }
    #[inline(always)]
    fn mean2(a: &[u16; 16], b: &[u16; 16]) -> [u16; 16] {
        simd::mean2(a, b)
    }
    #[inline(always)]
    fn mean3(a: &[u16; 16], b: &[u16; 16], c: &[u16; 16]) -> [u16; 16] {
        simd::mean3(a, b, c)
    }
}

/// AVX2 (only used where the CPU has it, see `encode_stripe` and `decode_stripe`).
#[cfg(target_arch = "x86_64")]
pub struct Avx2;

#[cfg(target_arch = "x86_64")]
impl Lanes for Avx2 {
    #[inline(always)]
    fn find(c: &[u16; 16], slot: u16) -> usize {
        // SAFETY: only instantiated in code that runs after detecting AVX2.
        unsafe { avx2::find(c, slot) }
    }
    #[inline(always)]
    fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32) {
        // SAFETY: as above.
        unsafe { avx2::update(c, j, lo, hi, shift) }
    }
    #[inline(always)]
    fn mean2(a: &[u16; 16], b: &[u16; 16]) -> [u16; 16] {
        // SAFETY: as above.
        unsafe { avx2::mean2(a, b) }
    }
    #[inline(always)]
    fn mean3(a: &[u16; 16], b: &[u16; 16], c: &[u16; 16]) -> [u16; 16] {
        // SAFETY: as above.
        unsafe { avx2::mean3(a, b, c) }
    }
    #[inline(always)]
    fn sums(x: &[i16; 64], y: &[i16; 64], w: [&[i32; 64]; 4], rows: bool) -> [[i32; 8]; 2] {
        // SAFETY: as above.
        unsafe { avx2::sums(x, y, w, rows) }
    }
}

#[cfg(target_arch = "x86_64")]
mod avx2 {
    use core::arch::x86_64::*;

    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn find(c: &[u16; 16], slot: u16) -> usize {
        let v = _mm256_loadu_si256(c.as_ptr() as *const __m256i);
        let le = _mm256_cmpeq_epi16(_mm256_subs_epu16(v, _mm256_set1_epi16(slot as i16)), _mm256_setzero_si256());
        let bits = _mm256_movemask_epi8(le) as u32;
        ((!bits).trailing_zeros() / 2) as usize
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32) {
        let p = c.as_mut_ptr() as *mut __m256i;
        let v = _mm256_loadu_si256(p);
        let m = _mm256_loadu_si256(super::GE_MASK[j & 15].as_ptr() as *const __m256i);
        let t = _mm256_blendv_epi8(_mm256_loadu_si256(lo.as_ptr() as *const __m256i), _mm256_loadu_si256(hi.as_ptr() as *const __m256i), m);
        let d = _mm256_sra_epi16(_mm256_sub_epi16(t, v), _mm_cvtsi32_si128(shift as i32));
        _mm256_storeu_si256(p, _mm256_add_epi16(v, d));
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn mean2(a: &[u16; 16], b: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        let m = _mm256_avg_epu16(_mm256_loadu_si256(a.as_ptr() as *const __m256i), _mm256_loadu_si256(b.as_ptr() as *const __m256i));
        _mm256_storeu_si256(r.as_mut_ptr() as *mut __m256i, m);
        r
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn mean3(a: &[u16; 16], b: &[u16; 16], c: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        let ab = _mm256_avg_epu16(_mm256_loadu_si256(a.as_ptr() as *const __m256i), _mm256_loadu_si256(b.as_ptr() as *const __m256i));
        let m = _mm256_avg_epu16(ab, _mm256_avg_epu16(ab, _mm256_loadu_si256(c.as_ptr() as *const __m256i)));
        _mm256_storeu_si256(r.as_mut_ptr() as *mut __m256i, m);
        r
    }

    #[target_feature(enable = "avx2")]
    #[inline]
    pub unsafe fn sums(x: &[i16; 64], y: &[i16; 64], w: [&[i32; 64]; 4], rows: bool) -> [[i32; 8]; 2] {
        let mut p = [[_mm256_setzero_si256(); 8]; 2];
        for r in 0..8 {
            let a = _mm256_cvtepi16_epi32(_mm_loadu_si128(x.as_ptr().add(r * 8) as *const __m128i));
            let b = _mm256_cvtepi16_epi32(_mm_loadu_si128(y.as_ptr().add(r * 8) as *const __m128i));
            for s in 0..2 {
                let wa = _mm256_loadu_si256(w[s].as_ptr().add(r * 8) as *const __m256i);
                let wb = _mm256_loadu_si256(w[s + 2].as_ptr().add(r * 8) as *const __m256i);
                p[s][r] = _mm256_sub_epi32(_mm256_mullo_epi32(a, wa), _mm256_mullo_epi32(b, wb));
            }
        }
        let mut out = [[0i32; 8]; 2];
        for s in 0..2 {
            let q = &p[s];
            let v = if rows {
                // pairwise: rows 0-3 and 4-7 by halves of the columns, then the halves
                let lo = _mm256_hadd_epi32(_mm256_hadd_epi32(q[0], q[1]), _mm256_hadd_epi32(q[2], q[3]));
                let hi = _mm256_hadd_epi32(_mm256_hadd_epi32(q[4], q[5]), _mm256_hadd_epi32(q[6], q[7]));
                _mm256_add_epi32(_mm256_permute2x128_si256::<0x20>(lo, hi), _mm256_permute2x128_si256::<0x31>(lo, hi))
            } else {
                let mut v = q[0];
                for r in 1..8 {
                    v = _mm256_add_epi32(v, q[r]);
                }
                v
            };
            _mm256_storeu_si256(out[s].as_mut_ptr() as *mut __m256i, v);
        }
        out
    }
}

/// Lanes at or after `j` take `hi`, the others `lo`.
#[cfg_attr(not(any(target_arch = "x86_64", all(target_arch = "wasm32", target_feature = "simd128"))), allow(dead_code))]
static GE_MASK: [[u16; 16]; 16] = {
    let mut t = [[0u16; 16]; 16];
    let mut s = 0;
    while s < 16 {
        let mut i = s;
        while i < 16 {
            t[s][i] = 0xFFFF;
            i += 1;
        }
        s += 1;
    }
    t
};

#[cfg(target_arch = "x86_64")]
mod simd {
    use core::arch::x86_64::*;

    #[inline(always)]
    pub fn find(c: &[u16; 16], slot: u16) -> usize {
        // SAFETY: SSE2 is part of x86-64; `c` is 16 u16 (two unaligned 128-bit loads).
        unsafe {
            let p = c.as_ptr() as *const __m128i;
            let sv = _mm_set1_epi16(slot as i16);
            let z = _mm_setzero_si128();
            let a = _mm_cmpeq_epi16(_mm_subs_epu16(_mm_loadu_si128(p), sv), z);
            let b = _mm_cmpeq_epi16(_mm_subs_epu16(_mm_loadu_si128(p.add(1)), sv), z);
            let bits = _mm_movemask_epi8(_mm_packs_epi16(a, b)) as u32;
            (!bits).trailing_zeros() as usize
        }
    }

    #[inline(always)]
    pub fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32) {
        // SAFETY: SSE2 is part of x86-64; all pointers address 16 u16.
        unsafe {
            let p = c.as_mut_ptr() as *mut __m128i;
            let m = super::GE_MASK[j & 15].as_ptr() as *const __m128i;
            let (l, h) = (lo.as_ptr() as *const __m128i, hi.as_ptr() as *const __m128i);
            let sh = _mm_cvtsi32_si128(shift as i32);
            for j in 0..2 {
                let v = _mm_loadu_si128(p.add(j));
                let mk = _mm_loadu_si128(m.add(j));
                let t = _mm_or_si128(_mm_and_si128(mk, _mm_loadu_si128(h.add(j))), _mm_andnot_si128(mk, _mm_loadu_si128(l.add(j))));
                let d = _mm_sra_epi16(_mm_sub_epi16(t, v), sh);
                _mm_storeu_si128(p.add(j), _mm_add_epi16(v, d));
            }
        }
    }

    #[inline(always)]
    pub fn mean2(a: &[u16; 16], b: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        // SAFETY: SSE2 is part of x86-64; all pointers address 16 u16.
        unsafe {
            let (pa, pb, pr) = (a.as_ptr() as *const __m128i, b.as_ptr() as *const __m128i, r.as_mut_ptr() as *mut __m128i);
            for j in 0..2 {
                _mm_storeu_si128(pr.add(j), _mm_avg_epu16(_mm_loadu_si128(pa.add(j)), _mm_loadu_si128(pb.add(j))));
            }
        }
        r
    }

    #[inline(always)]
    pub fn mean3(a: &[u16; 16], b: &[u16; 16], c: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        // SAFETY: SSE2 is part of x86-64; all pointers address 16 u16.
        unsafe {
            let (pa, pb, pc, pr) = (a.as_ptr() as *const __m128i, b.as_ptr() as *const __m128i, c.as_ptr() as *const __m128i, r.as_mut_ptr() as *mut __m128i);
            for j in 0..2 {
                let ab = _mm_avg_epu16(_mm_loadu_si128(pa.add(j)), _mm_loadu_si128(pb.add(j)));
                _mm_storeu_si128(pr.add(j), _mm_avg_epu16(ab, _mm_avg_epu16(ab, _mm_loadu_si128(pc.add(j)))));
            }
        }
        r
    }

    #[inline(always)]
    pub fn luma_down(b: [&[i16; 64]; 4], fx: usize, fy: usize, ql: &[i16; 64]) -> [i16; 64] {
        // SAFETY: SSE2 is part of x86-64; the blocks are eight 128-bit rows.
        unsafe {
            let row = |p: &[i16; 64], i: usize| _mm_loadu_si128((p.as_ptr() as *const __m128i).add(i));
            let (lo, hi) = (_mm_set1_epi16(-16383), _mm_set1_epi16(16383));
            let deq = |bb: &[i16; 64]| {
                let mut x = [_mm_setzero_si128(); 8];
                for (i, x) in x.iter_mut().enumerate() {
                    let (c, q) = (row(bb, i), row(ql, i));
                    let (pl, ph) = (_mm_mullo_epi16(c, q), _mm_mulhi_epi16(c, q));
                    let p = _mm_packs_epi32(_mm_unpacklo_epi16(pl, ph), _mm_unpackhi_epi16(pl, ph));
                    *x = _mm_min_epi16(_mm_max_epi16(p, lo), hi);
                }
                x
            };
            // (a + 2048) >> 12 within +-16383, as in `luma_down_scalar`
            let fin = |a: __m128i, c: __m128i| {
                let r = _mm_set1_epi32(2048);
                let p = _mm_packs_epi32(_mm_srai_epi32(_mm_add_epi32(a, r), 11), _mm_srai_epi32(_mm_add_epi32(c, r), 11));
                _mm_max_epi16(_mm_srai_epi16(p, 1), lo)
            };
            let col = |dx: usize| {
                let x0 = deq(b[dx]);
                if fy == 1 {
                    return x0;
                }
                let x1 = deq(b[fx + dx]);
                let il: [__m128i; 8] = std::array::from_fn(|v| _mm_unpacklo_epi16(x0[v], x1[v]));
                let ih: [__m128i; 8] = std::array::from_fn(|v| _mm_unpackhi_epi16(x0[v], x1[v]));
                let mut t = [_mm_setzero_si128(); 8];
                for (s, t) in t.iter_mut().enumerate() {
                    let (mut a, mut c) = (_mm_setzero_si128(), _mm_setzero_si128());
                    for v in 0..8 {
                        if super::DOWN_V[s][v] != 0 {
                            let k = _mm_set1_epi32(super::DOWN_V[s][v]);
                            a = _mm_add_epi32(a, _mm_madd_epi16(il[v], k));
                            c = _mm_add_epi32(c, _mm_madd_epi16(ih[v], k));
                        }
                    }
                    *t = fin(a, c);
                }
                t
            };
            let rows = |t: [__m128i; 8]| {
                let mut out = [0i16; 64];
                for (i, r) in t.iter().enumerate() {
                    _mm_storeu_si128((out.as_mut_ptr() as *mut __m128i).add(i), *r);
                }
                out
            };
            let t0 = rows(col(0));
            if fx == 1 {
                return t0;
            }
            super::down_h(&t0, &rows(col(1)))
        }
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
mod simd {
    use core::arch::wasm32::*;

    #[inline(always)]
    pub fn find(c: &[u16; 16], slot: u16) -> usize {
        // SAFETY: `c` is 16 u16 (two unaligned 128-bit loads).
        unsafe {
            let p = c.as_ptr() as *const v128;
            let sv = u16x8_splat(slot);
            let z = u16x8_splat(0);
            let a = i16x8_eq(u16x8_sub_sat(v128_load(p), sv), z);
            let b = i16x8_eq(u16x8_sub_sat(v128_load(p.add(1)), sv), z);
            let bits = (i16x8_bitmask(a) as u32) | ((i16x8_bitmask(b) as u32) << 8);
            (!bits).trailing_zeros() as usize
        }
    }

    #[inline(always)]
    pub fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32) {
        // SAFETY: all pointers address 16 u16.
        unsafe {
            let p = c.as_mut_ptr() as *mut v128;
            let m = super::GE_MASK[j & 15].as_ptr() as *const v128;
            let (l, h) = (lo.as_ptr() as *const v128, hi.as_ptr() as *const v128);
            for j in 0..2 {
                let v = v128_load(p.add(j));
                let t = v128_bitselect(v128_load(h.add(j)), v128_load(l.add(j)), v128_load(m.add(j)));
                let d = i16x8_shr(i16x8_sub(t, v), shift);
                v128_store(p.add(j), i16x8_add(v, d));
            }
        }
    }

    #[inline(always)]
    pub fn mean2(a: &[u16; 16], b: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        // SAFETY: all pointers address 16 u16.
        unsafe {
            let (pa, pb, pr) = (a.as_ptr() as *const v128, b.as_ptr() as *const v128, r.as_mut_ptr() as *mut v128);
            for j in 0..2 {
                v128_store(pr.add(j), u16x8_avgr(v128_load(pa.add(j)), v128_load(pb.add(j))));
            }
        }
        r
    }

    #[inline(always)]
    pub fn mean3(a: &[u16; 16], b: &[u16; 16], c: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        // SAFETY: all pointers address 16 u16.
        unsafe {
            let (pa, pb, pc, pr) = (a.as_ptr() as *const v128, b.as_ptr() as *const v128, c.as_ptr() as *const v128, r.as_mut_ptr() as *mut v128);
            for j in 0..2 {
                let ab = u16x8_avgr(v128_load(pa.add(j)), v128_load(pb.add(j)));
                v128_store(pr.add(j), u16x8_avgr(ab, u16x8_avgr(ab, v128_load(pc.add(j)))));
            }
        }
        r
    }

    #[inline(always)]
    pub fn luma_down(b: [&[i16; 64]; 4], fx: usize, fy: usize, ql: &[i16; 64]) -> [i16; 64] {
        // SAFETY: the blocks are eight 128-bit rows, the table rows four i32.
        unsafe {
            let row = |p: &[i16; 64], i: usize| v128_load((p.as_ptr() as *const v128).add(i));
            let (lo, hi) = (i16x8_splat(-16383), i16x8_splat(16383));
            let deq = |bb: &[i16; 64]| {
                let mut x = [i16x8_splat(0); 8];
                for (i, x) in x.iter_mut().enumerate() {
                    let (c, q) = (row(bb, i), row(ql, i));
                    let p = i16x8_narrow_i32x4(i32x4_extmul_low_i16x8(c, q), i32x4_extmul_high_i16x8(c, q));
                    *x = i16x8_min(i16x8_max(p, lo), hi);
                }
                x
            };
            // (a + 2048) >> 12 within +-16383, as in `luma_down_scalar`
            let fin = |a: v128, c: v128| {
                let r = i32x4_splat(2048);
                let p = i16x8_narrow_i32x4(i32x4_shr(i32x4_add(a, r), 11), i32x4_shr(i32x4_add(c, r), 11));
                i16x8_max(i16x8_shr(p, 1), lo)
            };
            let col = |dx: usize| {
                let x0 = deq(b[dx]);
                if fy == 1 {
                    return x0;
                }
                let x1 = deq(b[fx + dx]);
                let il: [v128; 8] = std::array::from_fn(|v| i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(x0[v], x1[v]));
                let ih: [v128; 8] = std::array::from_fn(|v| i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(x0[v], x1[v]));
                let mut t = [i16x8_splat(0); 8];
                for (s, t) in t.iter_mut().enumerate() {
                    let (mut a, mut c) = (i32x4_splat(0), i32x4_splat(0));
                    for v in 0..8 {
                        if super::DOWN_V[s][v] != 0 {
                            let k = i32x4_splat(super::DOWN_V[s][v]);
                            a = i32x4_add(a, i32x4_dot_i16x8(il[v], k));
                            c = i32x4_add(c, i32x4_dot_i16x8(ih[v], k));
                        }
                    }
                    *t = fin(a, c);
                }
                t
            };
            let t0 = col(0);
            let r = if fx == 1 {
                t0
            } else {
                let t1 = col(1);
                let mut r = [i16x8_splat(0); 8];
                for (s, r) in r.iter_mut().enumerate() {
                    let l = i16x8_shuffle::<0, 8, 1, 9, 2, 10, 3, 11>(t0[s], t1[s]);
                    let h = i16x8_shuffle::<4, 12, 5, 13, 6, 14, 7, 15>(t0[s], t1[s]);
                    let p = [i32x4_shuffle::<0, 0, 0, 0>(l, l), i32x4_shuffle::<1, 1, 1, 1>(l, l), i32x4_shuffle::<2, 2, 2, 2>(l, l), i32x4_shuffle::<3, 3, 3, 3>(l, l), i32x4_shuffle::<0, 0, 0, 0>(h, h), i32x4_shuffle::<1, 1, 1, 1>(h, h), i32x4_shuffle::<2, 2, 2, 2>(h, h), i32x4_shuffle::<3, 3, 3, 3>(h, h)];
                    let (mut a, mut c) = (i32x4_splat(0), i32x4_splat(0));
                    for (u, p) in p.iter().enumerate() {
                        let k = &super::DOWN_H[u];
                        if *k != [[0; 4]; 2] {
                            a = i32x4_add(a, i32x4_dot_i16x8(*p, v128_load(k[0].as_ptr() as *const v128)));
                            c = i32x4_add(c, i32x4_dot_i16x8(*p, v128_load(k[1].as_ptr() as *const v128)));
                        }
                    }
                    *r = fin(a, c);
                }
                r
            };
            let mut out = [0i16; 64];
            for (i, r) in r.iter().enumerate() {
                v128_store((out.as_mut_ptr() as *mut v128).add(i), *r);
            }
            out
        }
    }
}

#[cfg(not(any(target_arch = "x86_64", all(target_arch = "wasm32", target_feature = "simd128"))))]
mod simd {
    #[inline(always)]
    pub fn find(c: &[u16; 16], slot: u16) -> usize {
        c.iter().take_while(|&&x| x <= slot).count()
    }

    #[inline(always)]
    pub fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32) {
        for i in 0..16 {
            let t = if i >= j { hi[i] } else { lo[i] };
            let d = (t.wrapping_sub(c[i]) as i16) >> shift;
            c[i] = c[i].wrapping_add(d as u16);
        }
    }

    #[inline(always)]
    pub fn mean2(a: &[u16; 16], b: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        for i in 0..16 {
            r[i] = ((a[i] as u32 + b[i] as u32 + 1) >> 1) as u16;
        }
        r
    }

    #[inline(always)]
    pub fn mean3(a: &[u16; 16], b: &[u16; 16], c: &[u16; 16]) -> [u16; 16] {
        let mut r = [0u16; 16];
        for i in 0..16 {
            let ab = (a[i] as u32 + b[i] as u32 + 1) >> 1;
            r[i] = ((ab + ((ab + c[i] as u32 + 1) >> 1) + 1) >> 1) as u16;
        }
        r
    }

    #[inline(always)]
    pub fn luma_down(b: [&[i16; 64]; 4], fx: usize, fy: usize, ql: &[i16; 64]) -> [i16; 64] {
        super::luma_down_scalar(b, fx, fy, ql)
    }
}

/// Source and sink of coded symbols: the encoder codes the symbols it is given, the decoder
/// ignores them and returns what it decodes.
pub trait Io {
    const DEC: bool;
    type L: Lanes;
    /// Symbol `s` of an alphabet of `n` symbols; `cnt` counts the distribution's updates.
    fn sym(&mut self, c: &mut Cdf, cnt: &mut u8, n: usize, s: usize) -> usize;
    /// Symbol `s` with a distribution that does not adapt (a mean of others, see `Table::sym2`).
    fn code(&mut self, c: &Cdf, s: usize) -> usize;
    /// Bit `b` with adaptive probability `p`.
    fn bit(&mut self, p: &mut Bit, b: u32) -> u32;
    /// `n` raw bits (1 to 24), most significant first.
    fn raw(&mut self, v: u32, n: u32) -> u32;
    /// Called before the symbols of each block (or byte): chunks of the rANS stream end only
    /// between blocks.
    fn block(&mut self);
}

/// Symbols after which the encoder ends a rANS chunk at the next block: it holds at most one
/// chunk of symbols, and the decoder starts from new states (stored at the head of each chunk,
/// after the chunk's number of blocks).
const CHUNK: usize = 1 << 18;

pub struct Enc<L: Lanes = Base> {
    lanes: std::marker::PhantomData<L>,
    ops: Vec<u32>,
    /// blocks in the current chunk
    blocks: u32,
    out: Vec<u8>,
    raw: Vec<u8>,
    racc: u64,
    rbits: u32,
}

impl<L: Lanes> Default for Enc<L> {
    fn default() -> Self {
        Self::new()
    }
}

impl<L: Lanes> Enc<L> {
    pub fn new() -> Self {
        Enc { lanes: std::marker::PhantomData, ops: Vec::with_capacity(CHUNK + 4096), blocks: 0, out: Vec::new(), raw: Vec::new(), racc: 0, rbits: 0 }
    }
    /// Codes the buffered symbols in reverse as one chunk: its number of blocks and two final
    /// states, then the renormalization words in the order the decoder reads them.
    fn flush_chunk(&mut self) {
        let mut x = [RANS_L; 2];
        let mut words: Vec<u16> = Vec::with_capacity(self.ops.len() / 2 + 8);
        for (i, &op) in self.ops.iter().enumerate().rev() {
            let st = &mut x[i & 1];
            let start = op & 0xFFFF;
            let freq = (op >> 16) + 1;
            if *st >= (freq << (32 - PROB_BITS)) {
                words.push(*st as u16);
                *st >>= 16;
            }
            *st = ((*st / freq) << PROB_BITS) + (*st % freq) + start;
        }
        for &s in &[x[1], x[0], self.blocks] {
            words.push(s as u16);
            words.push((s >> 16) as u16);
        }
        self.out.reserve(words.len() * 2);
        for &w in words.iter().rev() {
            self.out.extend_from_slice(&w.to_be_bytes());
        }
        self.ops.clear();
        self.blocks = 0;
    }
    #[inline(always)]
    fn push(&mut self, op: u32) {
        self.ops.push(op);
    }
    /// Returns (rANS-coded bytes, raw bits).
    pub fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
        if !self.ops.is_empty() || self.out.is_empty() {
            self.flush_chunk();
        }
        let out = std::mem::take(&mut self.out);
        while self.rbits >= 8 {
            self.rbits -= 8;
            self.raw.push((self.racc >> self.rbits) as u8);
        }
        if self.rbits > 0 {
            self.raw.push((self.racc << (8 - self.rbits)) as u8);
        }
        (out, self.raw)
    }
}

impl<L: Lanes> Io for Enc<L> {
    type L = L;
    const DEC: bool = false;
    #[inline(always)]
    fn sym(&mut self, c: &mut Cdf, cnt: &mut u8, n: usize, s: usize) -> usize {
        debug_assert!(s < n);
        self.code(c, s);
        c.update::<L>(cnt, s, n);
        s
    }
    #[inline(always)]
    fn code(&mut self, c: &Cdf, s: usize) -> usize {
        let (start, freq) = c.interval(s);
        self.push(start | ((freq - 1) << 16));
        s
    }
    #[inline(always)]
    fn bit(&mut self, p: &mut Bit, b: u32) -> u32 {
        let q = *p as u32;
        self.push(std::hint::select_unpredictable(b != 0, q | ((PROB_ONE - q - 1) << 16), (q - 1) << 16));
        *p = adapt_bit(*p, b);
        b
    }
    #[inline(always)]
    fn raw(&mut self, v: u32, n: u32) -> u32 {
        debug_assert!((1..=24).contains(&n) && v >> n == 0);
        self.racc = (self.racc << n) | v as u64;
        self.rbits += n;
        if self.rbits >= 32 {
            self.rbits -= 32;
            self.raw.extend_from_slice(&((self.racc >> self.rbits) as u32).to_be_bytes());
        }
        v
    }
    #[inline(always)]
    fn block(&mut self) {
        if self.ops.len() >= CHUNK {
            self.flush_chunk();
        }
        self.blocks += 1;
    }
}

pub struct Dec<'a, L: Lanes = Base> {
    lanes: std::marker::PhantomData<L>,
    x0: u32,
    x1: u32,
    /// blocks left in the current chunk
    left: u32,
    data: &'a [u8],
    pos: usize,
    rdata: &'a [u8],
    rpos: usize,
    racc: u64,
    rbits: u32,
}

impl<'a, L: Lanes> Dec<'a, L> {
    pub fn new(data: &'a [u8], raw: &'a [u8]) -> Self {
        let mut d = Dec { lanes: std::marker::PhantomData, x0: 0, x1: 0, left: 0, data, pos: 0, rdata: raw, rpos: 0, racc: 0, rbits: 0 };
        d.next_chunk();
        d
    }
    #[cold]
    fn next_chunk(&mut self) {
        self.left = (self.word() << 16) | self.word();
        self.x0 = (self.word() << 16) | self.word();
        self.x1 = (self.word() << 16) | self.word();
    }
    #[inline(always)]
    fn peek(&self) -> u32 {
        match self.data.get(self.pos..self.pos + 2) {
            Some(s) => u16::from_be_bytes([s[0], s[1]]) as u32,
            None => 0,
        }
    }
    #[inline(always)]
    fn word(&mut self) -> u32 {
        let w = self.peek();
        self.pos += 2;
        w
    }
    /// Renormalizes a state without a branch: below RANS_L it takes the next word.
    #[inline(always)]
    fn renorm(&mut self, x: u32) -> u32 {
        let r = (x < RANS_L) as u32;
        let w = std::hint::select_unpredictable(r != 0, self.peek(), 0);
        self.pos += 2 * r as usize;
        (x << (r << 4)) | w
    }
    /// True if the decoder read well past the end of either stream, which valid data never does.
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len() + 8 || self.rpos > self.rdata.len() + 8
    }
}

impl<L: Lanes> Io for Dec<'_, L> {
    type L = L;
    const DEC: bool = true;
    #[inline(always)]
    fn sym(&mut self, c: &mut Cdf, cnt: &mut u8, n: usize, s: usize) -> usize {
        let s = self.code(c, s);
        c.update::<L>(cnt, s, n);
        s
    }
    #[inline(always)]
    fn code(&mut self, c: &Cdf, _s: usize) -> usize {
        let x = self.x0;
        let slot = x & (PROB_ONE - 1);
        let s = c.find::<L>(slot);
        let (start, freq) = c.interval(s);
        let x = self.renorm(freq * (x >> PROB_BITS) + slot - start);
        self.x0 = self.x1;
        self.x1 = x;
        s
    }
    #[inline(always)]
    fn bit(&mut self, p: &mut Bit, _b: u32) -> u32 {
        let x = self.x0;
        let slot = x & (PROB_ONE - 1);
        let q = *p as u32;
        let b = (slot >= q) as u32;
        let (start, freq) = std::hint::select_unpredictable(b != 0, (q, PROB_ONE - q), (0, q));
        let x = self.renorm(freq * (x >> PROB_BITS) + slot - start);
        self.x0 = self.x1;
        self.x1 = x;
        *p = adapt_bit(*p, b);
        b
    }
    #[inline(always)]
    fn raw(&mut self, _v: u32, n: u32) -> u32 {
        debug_assert!((1..=24).contains(&n));
        if self.rbits < n {
            let w = match self.rdata.get(self.rpos..self.rpos + 4) {
                Some(s) => u32::from_be_bytes(s.try_into().unwrap()),
                None => {
                    let mut b = [0u8; 4];
                    for (i, x) in b.iter_mut().enumerate() {
                        *x = self.rdata.get(self.rpos + i).copied().unwrap_or(0);
                    }
                    u32::from_be_bytes(b)
                }
            };
            self.rpos += 4;
            self.racc |= (w as u64) << (32 - self.rbits);
            self.rbits += 32;
        }
        let v = (self.racc >> (64 - n)) as u32;
        self.racc <<= n;
        self.rbits -= n;
        v
    }
    #[inline(always)]
    fn block(&mut self) {
        if self.left == 0 {
            self.next_chunk();
        }
        self.left = self.left.wrapping_sub(1);
    }
}

// ---------------------------------------------------------------------------------------------
// tokens and contexts

/// Magnitude classes: token t >= 1 covers [BASE[t], BASE[t] + 2^LOW[t]); token 14 escapes to an
/// explicit bit length.
const NTOK: usize = 15;
const ESC: usize = 14;
const BASE: [u32; 14] = [0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96];
const LOW: [u32; 14] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5];
/// A typical magnitude of each token (in the lower half of its range), known before the raw bits.
const TMAG: [u16; 16] = [0, 1, 2, 3, 4, 6, 9, 13, 19, 27, 39, 55, 79, 111, 255, 255];

#[inline(always)]
fn bitlen(v: u32) -> u32 {
    32 - v.leading_zeros()
}

static TOKEN: [u8; 128] = {
    let mut t = [0u8; 128];
    let mut a = 1;
    while a < 128 {
        t[a] = if a < 4 {
            a as u8
        } else {
            let e = 32 - (a as u32).leading_zeros();
            (4 + 2 * (e - 3) + ((a as u32 >> (e - 2)) & 1)) as u8
        };
        a += 1;
    }
    t
};

/// Token of a magnitude (0 for zero).
#[inline(always)]
fn token(a: u32) -> usize {
    if a < 128 {
        TOKEN[a as usize] as usize
    } else {
        ESC
    }
}

/// Bucket of the number of nonzero coefficients still to code.
const REMB: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = match i {
            0..=1 => 0,
            2 => 1,
            3 => 2,
            4 => 3,
            5..=6 => 4,
            7..=8 => 5,
            9..=11 => 6,
            12..=15 => 7,
            16..=20 => 8,
            21..=27 => 9,
            28..=36 => 10,
            _ => 11,
        };
        i += 1;
    }
    t
};
const NREM: usize = 12;

/// Bucket of the neighbours' average nonzero count.
const NZB: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = match i {
            0..=4 => i as u8,
            5..=6 => 5,
            7..=8 => 6,
            9..=10 => 7,
            11..=13 => 8,
            14..=16 => 9,
            17..=20 => 10,
            21..=25 => 11,
            26..=31 => 12,
            32..=39 => 13,
            40..=49 => 14,
            _ => 15,
        };
        i += 1;
    }
    t
};
const NNZB: usize = 16;
/// Bucket of the magnitude estimate of an interior coefficient (about 1.45 times wider each, in
/// units of 3/4 of the estimate).
const EQ: [u8; 341] = {
    let mut t = [0u8; 341];
    let mut i = 0;
    while i < 341 {
        t[i] = match i * 3 / 4 {
            0 => 0,
            1..=2 => 1,
            3..=5 => 2,
            6..=8 => 3,
            9..=12 => 4,
            13..=18 => 5,
            19..=26 => 6,
            27..=38 => 7,
            39..=55 => 8,
            56..=80 => 9,
            81..=115 => 10,
            116..=170 => 11,
            171..=250 => 12,
            _ => 13,
        };
        i += 1;
    }
    t
};
const NEQ: usize = 14;
/// Zone of an interior position (index into INTERIOR) for magnitudes: its anti-diagonal (the
/// last six merged), times NEQ.
const IZE: [u8; 49] = {
    let mut t = [0u8; 49];
    let mut i = 0;
    while i < 49 {
        let p = INTERIOR[i] as usize;
        let d = p / 8 + p % 8 - 2;
        let z = if d < 7 { d } else { 7 };
        t[i] = (z * NEQ) as u8;
        i += 1;
    }
    t
};
const NKZ: usize = 8;

/// Bucket of the number of nonzero coefficients still to code, for magnitudes, times NKZ * NEQ.
const REM8: [u16; 64] = {
    let mut t = [0u16; 64];
    let mut i = 0;
    while i < 64 {
        let b = match i {
            0..=1 => 0,
            2 => 1,
            3 => 2,
            4..=5 => 3,
            6..=8 => 4,
            9..=13 => 5,
            14..=21 => 6,
            _ => 7,
        };
        t[i] = (b * NKZ * NEQ) as u16;
        i += 1;
    }
    t
};
const NREM8: usize = 8;

/// Positions with both frequencies nonzero, in zigzag order.
const INTERIOR: [u8; 49] = {
    let mut t = [0u8; 49];
    let (mut k, mut n) = (1, 0);
    while k < 64 {
        let p = NATURAL[k];
        if p % 8 != 0 && p / 8 != 0 {
            t[n] = p as u8;
            n += 1;
        }
        k += 1;
    }
    t
};
/// For each interior position (index into INTERIOR), the positions whose magnitude estimate it
/// adds to once coded: the next ones to the right and below (weight 2), diagonally, two to the
/// right and two below, and the next one on its anti-diagonal (weight 1). 0 where there is none
/// (the DC slot of the estimate, which is never read).
const FWD: [[u8; 6]; 49] = {
    let mut t = [[0u8; 6]; 49];
    let mut i = 0;
    while i < 49 {
        let p = INTERIOR[i] as usize;
        let (r, c) = (p / 8, p % 8);
        if c < 7 {
            t[i][0] = (p + 1) as u8;
        }
        if r < 7 {
            t[i][1] = (p + 8) as u8;
        }
        if r < 7 && c < 7 {
            t[i][2] = (p + 9) as u8;
        }
        if c < 6 {
            t[i][3] = (p + 2) as u8;
        }
        if r < 6 {
            t[i][4] = (p + 16) as u8;
        }
        if i < 48 {
            let q = INTERIOR[i + 1] as usize;
            if q / 8 + q % 8 == r + c {
                t[i][5] = q as u8;
            }
        }
        i += 1;
    }
    t
};
/// For each interior position (index into INTERIOR) but the last, the next one in zigzag order,
/// which it always adds to in FWD (to the right, below, or next on its anti-diagonal), and the
/// shift of its weight there.
const NEXT: [(u8, u8); 49] = {
    let mut t = [(0u8, 0u8); 49];
    let mut i = 0;
    while i < 48 {
        let q = INTERIOR[i + 1];
        assert!(FWD[i][0] == q || FWD[i][1] == q || FWD[i][5] == q);
        t[i] = (q, (FWD[i][5] != q) as u8);
        i += 1;
    }
    t
};
/// Bucket of the weighted magnitudes next to an edge coefficient (see `edge`), in half units.
const AQ: [u8; 32] = [0, 0, 1, 1, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 5];
const NAQ: usize = 6;
/// Bucket of the co-located luma blocks' mean 7x7 count, for chroma blocks.
const LUMAQ: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = match i {
            0 => 0,
            1..=3 => 1,
            4..=10 => 2,
            _ => 3,
        };
        i += 1;
    }
    t
};
const NLUMA: usize = 24;
/// Bucket of the earlier chroma component's edge count, for the later one.
const XEQ: [u8; 8] = [0, 1, 1, 2, 2, 2, 2, 2];
/// Contexts of a chroma edge count from the other components (see `Model::block`): none, the
/// earlier chroma component's count, the luma reference's magnitudes, or both.
const NXE: usize = 20;
/// Bucket of the earlier chroma component's 7x7 count, for the later one.
const XNZQ: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = match i {
            0..=2 => i as u8,
            3..=4 => 3,
            5..=7 => 4,
            8..=11 => 5,
            12..=17 => 6,
            _ => 7,
        };
        i += 1;
    }
    t
};
/// Bucket of the summed magnitudes of an edge coefficient in the neighbouring blocks.
const BEQ: [u8; 16] = [0, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3];
const NBE: usize = 4;
/// Zone of an edge position 1..7.
const EZ: [u8; 8] = [0, 0, 1, 2, 2, 3, 3, 3];
/// Bucket of the neighbours' mean DC error token.
const DCQ: [u8; 16] = [0, 1, 1, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3];
/// Bucket of a 7x7 nonzero count for the edge contexts.
const NZQ8: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = match i {
            0 => 0,
            1 => 1,
            2..=3 => 2,
            4..=6 => 3,
            7..=11 => 4,
            12..=19 => 5,
            20..=29 => 6,
            _ => 7,
        };
        i += 1;
    }
    t
};

/// round(4096 C(u) cos((2x+1) u pi / 16)) with C(0) = 1/sqrt(2), C(u) = 1, for x = 0 and x = 7:
/// the contribution of frequency u to the first and last sample of an 8-point inverse DCT.
const COS0: [i64; 8] = [2896, 4017, 3784, 3406, 2896, 2276, 1567, 799];
const COS7: [i64; 8] = [2896, -4017, 3784, -3406, 2896, -2276, 1567, -799];
const COS1: [i64; 8] = [2896, 3406, 1567, -799, -2896, -4017, -3784, -2276];
const COS6: [i64; 8] = [2896, -3406, 1567, 799, -2896, 4017, -3784, 2276];

/// What a block's right and lower neighbours use: its nonzero counts.
#[derive(Clone, Copy, Default)]
pub struct Info {
    nz77: u8,
    nzr: u8,
    nzc: u8,
    /// token of the DC prediction error
    dct: u8,
    /// signs that agreed minus signs that disagreed with the other components (see `Cross`)
    agree: i8,
    agree2: i8,
    /// sum of the AC magnitudes (each capped at 4095) if there was a `Cross`
    asum: u32,
}

/// Signs predicted from the block of an earlier component at the same place (`r`): its sign at
/// the same position, times the sign of the agreement so far (this block's coded coefficients
/// counted twice, plus the neighbours' totals `nb`). `xc`: 0 if `r` is the luma reference (in
/// half quantization steps), 1 if chroma; then `r2` may hold the luma reference as well, with
/// its own agreement.
struct Cross<'a> {
    r: &'a [i16; 64],
    xc: usize,
    /// magnitude of this component per unit of `r` and of `r2`, times 256 (see `code_stripe`)
    rho: [u32; 2],
    cum: i32,
    nb: i32,
    r2: Option<&'a [i16; 64]>,
    cum2: i32,
    nb2: i32,
    /// decaying sums of the agreement of the component's previous blocks with `r` and `r2`
    g: [i32; 2],
    /// sum of the magnitudes coded so far (see `Info::asum`)
    own: u32,
}

/// Contexts of `Cross::pred` per zone (interior, edge).
const NXS: usize = 180;

/// The agreement bucket and reference magnitude bucket (< 36) and the predicted sign (true:
/// negative) from reference `r` (in half quantization steps if `half`) with agreement `cum`,
/// `nb`, else `g` (see `Cross`).
#[inline(always)]
fn xpred(r: &[i16; 64], cum: i32, nb: i32, g: i32, pos: usize, half: bool) -> Option<(usize, bool)> {
    let rv = r[pos & 63] as i32;
    let sc = 2 * cum + nb;
    if rv == 0 || (sc == 0 && g == 0) {
        return None;
    }
    let a = rv.unsigned_abs() as usize;
    let rb = if half { [0, 0, 1, 2, 2, 3][a.min(5)] } else { a.min(3) };
    if sc == 0 {
        return Some((32 + rb, (rv < 0) != (g < 0)));
    }
    Some(((sc.unsigned_abs().min(8) as usize - 1) * 4 + rb, (rv < 0) != (sc < 0)))
}

/// The context (< NXA) of a sign predicted as `pneg` by how the sign predicted from `r` (see
/// `xpred`) relates to it: none, else agreeing or not by the confidence of the agreement score.
/// Without branches: the operands are unpredictable.
#[inline(always)]
fn xagree(r: &[i16; 64], cum: i32, nb: i32, g: i32, pos: usize, pneg: bool) -> usize {
    let rv = r[pos & 63] as i32;
    let sc = 2 * cum + nb;
    let s = std::hint::select_unpredictable(sc != 0, sc, g);
    let x = 1 + 3 * (((rv < 0) != (s < 0)) != pneg) as usize + XCONF[sc.unsigned_abs().min(8) as usize] as usize;
    x * ((rv != 0) & (s != 0)) as usize
}

impl Cross<'_> {
    /// The sign context (< NXS) and the predicted sign (true: negative), if there is a
    /// prediction: from `r`, with whether `r2` agrees; else from `r2`.
    #[inline(always)]
    fn pred(&self, pos: usize) -> Option<(usize, bool)> {
        let p1 = xpred(self.r, self.cum, self.nb, self.g[0], pos, self.xc == 0);
        let p2 = self.r2.and_then(|r| xpred(r, self.cum2, self.nb2, self.g[1], pos, true));
        match (p1, p2) {
            (Some((k, n)), _) if self.xc == 0 => Some((k, n)),
            (Some((k, n)), None) => Some((36 + k * 3, n)),
            (Some((k, n)), Some((_, n2))) => Some((36 + k * 3 + 1 + (n != n2) as usize, n)),
            (None, Some((k, n))) => Some((144 + k, n)),
            (None, None) => None,
        }
    }
    /// The context (< NXR) of a sign predicted as `pneg` otherwise (see `xagree`): by `r`, else
    /// by `r2`; with whether `r2` agrees as well (if both predict).
    #[inline(always)]
    fn agree(&self, pos: usize, pneg: bool) -> usize {
        let x = xagree(self.r, self.cum, self.nb, self.g[0], pos, pneg);
        match self.r2 {
            Some(r) => {
                let x2 = xagree(r, self.cum2, self.nb2, self.g[1], pos, pneg);
                let both = ((x != 0) & (x2 != 0)) as usize * (1 + (x2 > 3) as usize);
                std::hint::select_unpredictable(x != 0, x, x2) + NXA * both
            }
            None => x,
        }
    }
    #[inline(always)]
    fn update(&mut self, pos: usize, x: i32) {
        self.own += x.unsigned_abs().min(4095);
        let sx = x.signum();
        self.cum += (self.r[pos & 63] as i32).signum() * sx;
        if let Some(r) = self.r2 {
            self.cum2 += (r[pos & 63] as i32).signum() * sx;
        }
    }
}

/// Confidence bucket of an agreement score by its magnitude (8 and more alike; 0: from the
/// previous blocks, see `Cross::g`).
const XCONF: [u8; 9] = [0, 0, 0, 1, 1, 1, 2, 2, 2];
/// Contexts of a predicted sign from another component's (see `xagree`), and from both
/// references (see `Cross::agree`).
const NXA: usize = 7;
const NXR: usize = 3 * NXA;
/// Weight of the other component's magnitude in the estimate of an interior coefficient.
const XW: u16 = 8;
/// The same for the luma reference of the second chroma component.
const XW2: u16 = 4;
/// Decay of the magnitude sums behind `Cross::rho` (a window of about 2^RHO_DECAY blocks).
const RHO_DECAY: u32 = 3;

/// 2:1 downsampling in the DCT domain, times 2^12: row s gives frequency s of the 8-point DCT of
/// the pairwise means of two adjacent 8-sample blocks, from their coefficients (0..8 the first
/// block's, 8..16 the second's).
const DOWN: [[i32; 16]; 8] = [
    [2048, 0, 0, 0, 0, 0, 0, 0, 2048, 0, 0, 0, 0, 0, 0, 0],
    [1856, 835, -141, 39, 0, -26, 59, -166, -1856, 835, 141, 39, 0, -26, -59, -166],
    [0, 2009, 0, 0, 0, 0, 0, -400, 0, -2009, 0, 0, 0, 0, 0, 400],
    [-652, 1589, 971, -166, 0, 111, -402, -316, 652, 1589, -971, -166, 0, 111, 402, -316],
    [0, 0, 1892, 0, 0, 0, -784, 0, 0, 0, 1892, 0, 0, 0, -784, 0],
    [435, -708, 1453, 835, 0, -558, -602, 141, -435, -708, -1453, 835, 0, -558, 602, 141],
    [0, 0, 0, 1703, 0, -1138, 0, 0, 0, 0, 0, -1703, 0, 1138, 0, 0],
    [-369, 558, -710, 1474, 0, -985, 294, -111, 369, 558, 710, 1474, 0, -985, -294, -111],
];
#[cfg_attr(any(target_arch = "x86_64", all(target_arch = "wasm32", target_feature = "simd128")), allow(dead_code))]
const DOWN16: [[i16; 16]; 8] = {
    let mut t = [[0i16; 16]; 8];
    let mut s = 0;
    while s < 8 {
        let mut i = 0;
        while i < 16 {
            t[s][i] = DOWN[s][i] as i16;
            i += 1;
        }
        s += 1;
    }
    t
};
#[cfg_attr(all(target_arch = "wasm32", target_feature = "simd128"), allow(dead_code))]
const DOWN16_T: [[i16; 8]; 16] = {
    let mut t = [[0i16; 8]; 16];
    let mut i = 0;
    while i < 16 {
        let mut s = 0;
        while s < 8 {
            t[i][s] = DOWN[s][i] as i16;
            s += 1;
        }
        i += 1;
    }
    t
};
/// `DOWN[s][v]` and `DOWN[s][8 + v]` as the low and high halves of a 32-bit lane (the operand of
/// a 16-bit multiply-add of interleaved rows).
#[cfg_attr(not(any(target_arch = "x86_64", all(target_arch = "wasm32", target_feature = "simd128"))), allow(dead_code))]
static DOWN_V: [[i32; 8]; 8] = {
    let mut t = [[0i32; 8]; 8];
    let mut s = 0;
    while s < 8 {
        let mut v = 0;
        while v < 8 {
            t[s][v] = (DOWN[s][v] & 0xFFFF) | (DOWN[s][8 + v] << 16);
            v += 1;
        }
        s += 1;
    }
    t
};
/// The same for frequencies `4 * h..4 * h + 4` of input coefficient `u`: `DOWN_H[u][h]`.
#[cfg_attr(not(all(target_arch = "wasm32", target_feature = "simd128")), allow(dead_code))]
static DOWN_H: [[[i32; 4]; 2]; 8] = {
    let mut t = [[[0i32; 4]; 2]; 8];
    let mut u = 0;
    while u < 8 {
        let mut k = 0;
        while k < 8 {
            t[u][k / 4][k % 4] = (DOWN[k][u] & 0xFFFF) | (DOWN[k][8 + u] << 16);
            k += 1;
        }
        u += 1;
    }
    t
};

/// The luma blocks covering a chroma block (`fx` by `fy` of them, 1 or 2 each way, `b[dy * fx +
/// dx]`) at the chroma block's resolution: the DCT of the means of the luma samples it covers,
/// from the dequantized coefficients (`ql`: the luma quantization table capped at 16383), within
/// +-16383.
// inlined so that it is compiled with AVX2 on the AVX2 paths
#[inline(always)]
fn luma_down(b: [&[i16; 64]; 4], fx: usize, fy: usize, ql: &[i16; 64]) -> [i16; 64] {
    simd::luma_down(b, fx, fy, ql)
}

/// `luma_down` without SIMD instructions (every implementation computes the same integers).
#[cfg_attr(any(target_arch = "x86_64", all(target_arch = "wasm32", target_feature = "simd128")), allow(dead_code))]
fn luma_down_scalar(b: [&[i16; 64]; 4], fx: usize, fy: usize, ql: &[i16; 64]) -> [i16; 64] {
    // 16-bit operands with 32-bit products and sums keep the passes cheap without wider SIMD;
    // y / 2 (rounded down) within +-16383 as a 16-bit saturation of y, so that it stays one
    // pack instruction when vectorized
    let half = |y: i32| ((y.clamp(-32768, 32767) as i16) >> 1).max(-16383);
    let deq = |bb: &[i16; 64]| -> [i16; 64] { std::array::from_fn(|k| half(2 * bb[k] as i32 * ql[k] as i32)) };
    // vertical pass: t[s * 8 + u] of the blocks at dx
    let col = |dx: usize| -> [i16; 64] {
        let x0 = deq(b[dx]);
        if fy == 1 {
            return x0;
        }
        let x1 = deq(b[fx + dx]);
        let mut t = [0i16; 64];
        for s in 0..8 {
            let mut acc = [0i32; 8];
            for v in 0..8 {
                let (c0, c1) = (DOWN16[s][v] as i32, DOWN16[s][8 + v] as i32);
                for u in 0..8 {
                    acc[u] += c0 * x0[v * 8 + u] as i32 + c1 * x1[v * 8 + u] as i32;
                }
            }
            for u in 0..8 {
                t[s * 8 + u] = half((acc[u] + 2048) >> 11);
            }
        }
        t
    };
    let t0 = col(0);
    if fx == 1 {
        return t0;
    }
    down_h(&t0, &col(1))
}

/// The horizontal pass of `luma_down_scalar` (which the compiler vectorizes well, with AVX2 where
/// the caller has it).
#[cfg_attr(all(target_arch = "wasm32", target_feature = "simd128"), allow(dead_code))]
#[inline(always)]
fn down_h(t0: &[i16; 64], t1: &[i16; 64]) -> [i16; 64] {
    let half = |y: i32| ((y.clamp(-32768, 32767) as i16) >> 1).max(-16383);
    let mut r = [0i16; 64];
    for s in 0..8 {
        let mut acc = [0i32; 8];
        for u in 0..8 {
            let (a0, a1) = (t0[s * 8 + u] as i32, t1[s * 8 + u] as i32);
            for k in 0..8 {
                acc[k] += DOWN16_T[u][k] as i32 * a0 + DOWN16_T[8 + u][k] as i32 * a1;
            }
        }
        for k in 0..8 {
            r[s * 8 + k] = half((acc[k] + 2048) >> 11);
        }
    }
    r
}

/// Candidate weights g of the gradient in the edge extrapolation, in eighths: each side's profile
/// at the boundary is (1 - g) times its last sample plus g times the linear extrapolation from the
/// last two. Each edge position uses the one with the smallest recent error (decaying sum, by
/// EG_SHIFT).
const EGS: [i64; 5] = [0, 2, 4, 6, 8];
const NEG: usize = 5;
const EG_SHIFT: u32 = 5;
/// Sign contexts of the edges without another component's sign, then with each context of it
/// (see NXR; chroma only, fewer), then of DC.
const ESIGN_X: usize = 2 * 2 * NTOK * 7 * 4 * 2 * 2;
const ESIGN_XC: usize = 2 * NTOK * 7 * 2;
const ESIGN_DC: usize = ESIGN_X + (NXR - 1) * ESIGN_XC;
/// Mantissa contexts of the edges and DC: top bit, then second bit from EMANT2 on.
const EMANT2: usize = 6 * 5 * 7;
/// Mantissa contexts of `emant2` (by position and neighbourhood): top bit, then second bit from
/// EMANT2_2 on.
const EMANT2_2: usize = 2 * 2 * 7 * NAQ * 5 * 7;
/// The same for the interior's `imant2` (by token and estimate).
const IMANT2_2: usize = 2 * 11 * NEQ * 5;
/// The same for the DC extrapolation of each side.
const DGS: [i64; 5] = [0, 2, 3, 4, 6];
const NDG: usize = 5;

/// Bucket of a predicted magnitude.
#[inline(always)]
fn pbucket(p: i32) -> usize {
    bitlen(p.unsigned_abs()).min(NPB as u32 - 1) as usize
}
const NPB: usize = 8;

/// Inputs of the interior value prediction of each position (index into INTERIOR, row v and
/// column u >= 1), in dequantized units: the above block's column u at its last pixel row (the
/// frequency-u profile there), the gradient of its last rows at frequency u, and the sums of
/// this block's coefficients (g, u), g < v, with g even and odd (all coded before it); then the
/// same for row v from the block to the left. Weights in units of 2^-12, learned by
/// `komla jtrain`.
pub const NLSW: usize = 8;
const NLS: usize = 2 * 49 * NLSW;

/// Bucket of the magnitude of a predicted interior value (1/16 quantization steps): below
/// 1/8, then by octaves up to 16 steps and more.
const NPS: usize = 9;
/// Bucket of a coded interior magnitude by token: 1, 2, 3, 4-7, 8-15, 16 and more.
const SAB: [u8; 16] = [0, 0, 1, 2, 3, 3, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5];
const NSAB: usize = 6;
/// Interior sign contexts without another component's sign, then with each context of it (see
/// NXR; chroma only).
const ISIGN_X: usize = 2 * 8 * NPS * NSAB;
const ISIGN_XC: usize = 8 * NPS * NSAB;
const MU_SHIFT: u32 = 6;

/// The interior value prediction of one component with its quantization table folded in.
pub struct Pred {
    /// q[v][u] times basis v (u) at the last sample, in 2^-12 (natural order)
    qc: [i32; 64],
    /// q[v][u] times the gradient of basis v (u) at the last sample, in 2^-12
    gc: [i32; 64],
    gr: [i32; 64],
    qr: [i32; 64],
    /// dequantization factors
    q: [i32; 64],
    /// q[v][u] times basis v (u) at the first sample, and its gradient there, in 2^-12, none for
    /// v = 0 (u = 0): the weights of a block's own coefficients in the edge extrapolation
    qc0: [i32; 64],
    gc0: [i32; 64],
    qr0: [i32; 64],
    gr0: [i32; 64],
    /// per interior position, the weights of its inputs (in the order of NLSW) in units of
    /// 2^-16 of 1/16 quantization step of the predicted coefficient per dequantized unit
    w: [[i32; NLSW]; 49],
    /// steps of the sign-sign adaptation of `w` to the image (a fraction of the learned weights)
    mu: [i32; 49],
}

impl Pred {
    fn new(lsw: &[i16], cls: usize, q: &[u16; 64]) -> Pred {
        let mut p = Pred { qc: [0; 64], gc: [0; 64], gr: [0; 64], qr: [0; 64], q: [0; 64], qc0: [0; 64], gc0: [0; 64], qr0: [0; 64], gr0: [0; 64], w: [[0; NLSW]; 49], mu: [0; 49] };
        for k in 0..64 {
            let x = q[k] as i64;
            p.qc[k] = (x * COS7[k / 8]) as i32;
            p.qr[k] = (x * COS7[k % 8]) as i32;
            p.gc[k] = (x * (COS7[k / 8] - COS6[k / 8])) as i32;
            p.gr[k] = (x * (COS7[k % 8] - COS6[k % 8])) as i32;
            p.q[k] = x as i32;
            let (v, u) = (k / 8, k % 8);
            if v > 0 {
                p.qc0[k] = (x * COS0[v]) as i32;
                p.gc0[k] = (x * (COS0[v] - COS1[v])) as i32;
            }
            if u > 0 {
                p.qr0[k] = (x * COS0[u]) as i32;
                p.gr0[k] = (x * (COS0[u] - COS1[u])) as i32;
            }
        }
        for j in 0..49 {
            let qt = q[INTERIOR[j] as usize].max(1) as i64;
            for k in 0..NLSW {
                let n = (lsw[(cls * 49 + j) * NLSW + k] as i64) << 8;
                p.w[j][k] = (n + qt / 2).div_euclid(qt) as i32;
            }
            let m = p.w[j].iter().map(|&w| w.unsigned_abs()).sum::<u32>() / NLSW as u32;
            p.mu[j] = (m >> MU_SHIFT).max(1) as i32;
        }
        p
    }
}

/// Least-squares fit of the interior prediction weights from the summed normal equations of
/// `Model::ls_sums` (training only).
pub fn solve_weights(sums: &[f64]) -> Vec<i16> {
    const N: usize = NLSW;
    let mut out = vec![0i16; NLS];
    for (k, s) in sums.chunks(N * N + N).enumerate() {
        let mut a = vec![0f64; N * N];
        let mut b = s[N * N..].to_vec();
        a.copy_from_slice(&s[..N * N]);
        let tr: f64 = (0..N).map(|i| a[i * N + i]).sum();
        for i in 0..N {
            a[i * N + i] += 1e-3 * tr / N as f64 + 1e-6;
        }
        // Gaussian elimination with partial pivoting
        for c in 0..N {
            let piv = (c..N).max_by(|&x, &y| a[x * N + c].abs().total_cmp(&a[y * N + c].abs())).unwrap();
            if piv != c {
                for j in 0..N {
                    a.swap(c * N + j, piv * N + j);
                }
                b.swap(c, piv);
            }
            let d = a[c * N + c];
            if d.abs() < 1e-300 {
                continue;
            }
            for r in c + 1..N {
                let f = a[r * N + c] / d;
                if f != 0.0 {
                    for j in c..N {
                        a[r * N + j] -= f * a[c * N + j];
                    }
                    b[r] -= f * b[c];
                }
            }
        }
        let mut w = vec![0f64; N];
        for c in (0..N).rev() {
            let d = a[c * N + c];
            let s: f64 = (c + 1..N).map(|j| a[c * N + j] * w[j]).sum();
            w[c] = if d.abs() < 1e-300 { 0.0 } else { (b[c] - s) / d };
        }
        for i in 0..N {
            out[k * N + i] = (w[i] * 4096.0).round().clamp(-32767.0, 32767.0) as i16;
        }
    }
    out
}

/// Weights for the model while training (set between the passes of `komla jtrain`).
pub static TRAIN_LSW: std::sync::Mutex<Vec<i16>> = std::sync::Mutex::new(Vec::new());

/// Initial distributions learned from the training set (see `komla jtrain`); empty: flat.
static PRIORS: &[u8] = include_bytes!("fast_priors.bin");

/// Update count at which the distributions handed from the first stripe to the others start.
const SEED_COUNT: u8 = 30;

/// Adaptive distributions of one stripe.
#[derive(Clone)]
pub struct Model {
    nz_hi: Table,
    nz_lo: Table,
    zero: CountedBits,
    mag: Table,
    nze: Table,
    ezero: CountedBits,
    emag: Table,
    esign: CountedBits,
    esc: Table,
    dc: Table,
    emant: CountedBits,
    isign: CountedBits,
    imant: CountedBits,
    csign: CountedBits,
    /// interior prediction weights (see NLSW)
    lsw: Vec<i16>,
    /// per component, the adapted weights of its `Pred` at the end of the stripe
    pw: Vec<[[i32; NLSW]; 49]>,
    /// normal equations of the interior prediction (training only)
    ls_acc: Vec<f64>,
    /// recent errors of the candidate edge extrapolations (see EGS) per class, edge and position
    eg: Vec<[u32; NEG]>,
    /// the same for DC (see DGS) per class and side, and for the weights of the two sides
    edc: [[u32; NDG]; 16],
    edw: [[u32; NDG]; 8],
    edm: [[u32; NDG]; 8],
    // second (and third) distributions or probabilities of the same decisions in other
    // contexts, averaged with the first
    ezero2: CountedBits,
    emag2: Table,
    emag3: Table,
    mag2: Table,
    mag3: Table,
    dc2: Table,
    nz_hi2: Table,
    nz_lo2: Table,
    nze2: Table,
    emant2: CountedBits,
    imant2: CountedBits,
}

impl Default for Model {
    fn default() -> Self {
        Self::new()
    }
}

static ZERO_BLOCK: [i16; 64] = [0; 64];

impl Model {
    /// A model with the learned initial distributions (flat ones while training).
    pub fn new() -> Model {
        static TEMPLATE: std::sync::OnceLock<Model> = std::sync::OnceLock::new();
        if training() {
            return Model::build();
        }
        TEMPLATE.get_or_init(Model::build).clone()
    }

    fn build() -> Model {
        let geo: Vec<u32> = (0..NTOK as u32).map(|t| 1 << (14 - t)).collect();
        let flat8 = Cdf::new(8, &[8; 8]);
        let mut m = Model {
            nz_hi: Table::new(NNZB * (1 + NLUMA), 7, Cdf::new(7, &[8; 7])),
            nz_lo: Table::new(NNZB * (1 + NLUMA) * 7, 8, flat8),
            zero: CountedBits::new(2 * NREM * 49 * NEQ),
            mag: Table::new(2 * NREM8 * NKZ * NEQ, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            nze: Table::new(2 * 8 * 8 * (1 + NXE), 8, Cdf::new(8, &[64, 32, 16, 8, 4, 2, 1, 1])),
            ezero: CountedBits::new(2 * 2 * 7 * 4 * NPB * NAQ * NBE),
            emag: Table::new(2 * 2 * 4 * NTOK * NAQ * NBE, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            esign: CountedBits::new(ESIGN_DC + 2 * 4 * 5 * 5 * 2),
            esc: Table::new(4, 8, flat8),
            dc: Table::new(2 * 8 * 4 * 4, NTOK, Cdf::new(NTOK, &geo)),
            emant: CountedBits::new(EMANT2 * 2),
            isign: CountedBits::new(ISIGN_X + (NXR - 1) * ISIGN_XC),
            imant: CountedBits::new(2 * 11 * 5 * 2),
            csign: CountedBits::new(2 * NXS * 3),
            lsw: vec![0; NLS],
            pw: Vec::new(),
            ls_acc: if training() { vec![0.0; 2 * 49 * (NLSW * NLSW + NLSW)] } else { Vec::new() },
            eg: vec![[1, 1, 0, 1, 1]; 64],
            edc: [[1, 1, 0, 1, 1]; 16],
            edw: [[1, 1, 0, 1, 1]; 8],
            edm: [[0, 1, 1, 1, 1]; 8],
            ezero2: CountedBits::new(2 * 2 * 7 * 8 * 8 * 4),
            emag2: Table::new(2 * 2 * 7 * NTOK * NAQ * 4, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            emag3: Table::new(2 * 2 * 7 * NTOK * 12 * 4, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            mag2: Table::new(2 * 49 * NEQ * 8, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            mag3: Table::new(2 * 49 * NREM, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            dc2: Table::new(2 * 16 * 4, NTOK, Cdf::new(NTOK, &geo)),
            nz_hi2: Table::new(2 * NNZB * NNZB, 7, Cdf::new(7, &[8; 7])),
            nz_lo2: Table::new(2 * NNZB * NNZB * 7, 8, flat8),
            nze2: Table::new(2 * 2 * 8 * 8, 8, Cdf::new(8, &[64, 32, 16, 8, 4, 2, 1, 1])),
            emant2: CountedBits::new(2 * EMANT2_2),
            imant2: CountedBits::new(2 * IMANT2_2),
        };
        if training() {
            let w = TRAIN_LSW.lock().unwrap();
            if w.len() == NLS {
                m.lsw.copy_from_slice(&w);
            }
        } else if !PRIORS.is_empty() {
            let mut p = PRIORS;
            let ok = m.nz_hi.load(&mut p).and_then(|_| m.nz_lo.load(&mut p)).and_then(|_| m.zero.load(&mut p)).and_then(|_| m.mag.load(&mut p)).and_then(|_| m.nze.load(&mut p)).and_then(|_| m.ezero.load(&mut p)).and_then(|_| m.emag.load(&mut p)).and_then(|_| m.esign.load(&mut p)).and_then(|_| m.esc.load(&mut p)).and_then(|_| m.dc.load(&mut p)).and_then(|_| m.emant.load(&mut p)).and_then(|_| m.isign.load(&mut p)).and_then(|_| m.imant.load(&mut p)).and_then(|_| m.csign.load(&mut p));
            let ok = ok.and_then(|_| m.ezero2.load(&mut p)).and_then(|_| m.emag2.load(&mut p)).and_then(|_| m.emag3.load(&mut p)).and_then(|_| m.mag2.load(&mut p)).and_then(|_| m.mag3.load(&mut p)).and_then(|_| m.dc2.load(&mut p)).and_then(|_| m.nz_hi2.load(&mut p)).and_then(|_| m.nz_lo2.load(&mut p)).and_then(|_| m.nze2.load(&mut p)).and_then(|_| m.emant2.load(&mut p)).and_then(|_| m.imant2.load(&mut p));
            assert!(ok.is_some() && p.len() == 2 * NLS, "priors do not match the model");
            for (w, b) in m.lsw.iter_mut().zip(p.chunks_exact(2)) {
                *w = i16::from_le_bytes([b[0], b[1]]);
            }
        }
        m
    }

    /// The summed normal equations of the interior prediction (training only).
    pub fn ls_sums(&self) -> &[f64] {
        &self.ls_acc
    }

    /// Adds a coded interior value `y` (dequantized) with inputs `x` to the normal equations.
    #[cold]
    fn ls_add(&mut self, cls: usize, j: usize, x: &[f64; NLSW], y: f64) {
        let s = &mut self.ls_acc[(cls * 49 + j) * (NLSW * NLSW + NLSW)..][..NLSW * NLSW + NLSW];
        for a in 0..NLSW {
            if x[a] != 0.0 {
                for b in 0..NLSW {
                    s[a * NLSW + b] += x[a] * x[b];
                }
                s[NLSW * NLSW + a] += x[a] * y;
            }
        }
    }

    /// Symbol statistics of every table, in the order of the priors file: for each table its
    /// alphabet size (2 for binary probabilities) and the counts per context and symbol.
    pub fn stats(&self) -> Vec<(usize, Vec<u32>)> {
        let t = |x: &Table| (x.n, x.stats.clone());
        let c = |x: &CountedBits| (2, x.stats.clone());
        vec![t(&self.nz_hi), t(&self.nz_lo), c(&self.zero), t(&self.mag), t(&self.nze), c(&self.ezero), t(&self.emag), c(&self.esign), t(&self.esc), t(&self.dc), c(&self.emant), c(&self.isign), c(&self.imant), c(&self.csign), c(&self.ezero2), t(&self.emag2), t(&self.emag3), t(&self.mag2), t(&self.mag3), t(&self.dc2), t(&self.nz_hi2), t(&self.nz_lo2), t(&self.nze2), c(&self.emant2), c(&self.imant2)]
    }

    /// The model the later stripes of an image start from (see `code_stripe`): this one (the
    /// first stripe's at its end) with the update counts capped, so that it keeps adapting.
    pub fn into_seed(mut self) -> Model {
        for t in [&mut self.nz_hi, &mut self.nz_lo, &mut self.mag, &mut self.nze, &mut self.emag, &mut self.esc, &mut self.dc, &mut self.emag2, &mut self.emag3, &mut self.mag2, &mut self.mag3, &mut self.dc2, &mut self.nz_hi2, &mut self.nz_lo2, &mut self.nze2] {
            t.cnt.iter_mut().for_each(|k| *k = (*k).min(SEED_COUNT));
        }
        for t in [&mut self.zero, &mut self.ezero, &mut self.esign, &mut self.emant, &mut self.isign, &mut self.imant, &mut self.csign, &mut self.ezero2, &mut self.emant2, &mut self.imant2] {
            t.cnt.iter_mut().for_each(|k| *k = (*k).min(SEED_COUNT));
        }
        self
    }

    /// Codes the sign of an interior value (`t` = its token, `v` the value), as agreement with
    /// the sign of the prediction `pneg` in context `ctx`, the top mantissa bit by where the
    /// predicted magnitude `pa` (1/16 steps) falls in the token's range (and by the token and the
    /// estimate bucket `nb`), and the other mantissa bits raw.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn irest<I: Io>(&mut self, io: &mut I, t: usize, v: i32, esc_ctx: usize, cls: usize, ctx: usize, pneg: bool, pa: u32, nb: usize) -> i32 {
        let a = v.unsigned_abs();
        let (base, low) = if t == ESC {
            let e = if I::DEC { 0 } else { bitlen(a) as usize };
            let e = 8 + self.esc.sym(io, esc_ctx, 8, e.saturating_sub(8));
            (1u32 << (e - 1), e as u32 - 1)
        } else {
            (BASE[t], LOW[t])
        };
        let flip = self.isign.bit(io, ctx, (v < 0) as u32 ^ pneg as u32);
        let m = if low > 0 {
            // where the prediction (1/16 steps) falls in the class and then in the chosen half
            // (2^sh steps each), if the sign agreed with it
            let rel = |lo: u32, sh: u32| ((2 + ((pa as i32 - (lo << 4) as i32) >> (sh + 4)).min(2)).max(1) as usize) * (flip ^ 1) as usize;
            let half = 1u32 << (low - 1);
            let d = a.wrapping_sub(base);
            let mb = (cls * 11 + t.min(14) - 4) * 5;
            let mc = ((cls * 11 + t.min(14) - 4) * NEQ + nb) * 5;
            let r1 = rel(base, low - 1);
            let top = self.imant.bit2(&mut self.imant2, io, mb + r1, mc + r1, 2, (d >> (low - 1)) & 1);
            if low > 1 {
                let r2 = rel(base + (top << (low - 1)), low - 2);
                let sec = self.imant.bit2(&mut self.imant2, io, 110 + mb + r2, IMANT2_2 + mc + r2, 2, (d >> (low - 2)) & 1);
                let r = if low > 2 { io.raw(d & ((half >> 1) - 1), low - 2) } else { 0 };
                base + ((top << (low - 1)) | (sec << (low - 2)) | r)
            } else {
                base + (top << (low - 1))
            }
        } else {
            base
        };
        let s = -((flip ^ pneg as u32) as i32);
        (m as i32 ^ s) - s
    }

    /// Codes a magnitude class and the raw bits of a value (`t` = its token, `v` the value);
    /// `sign`: raw, or predicted (`Some((sign context, predicted negative, predicted magnitude in
    /// 1/16 steps, mantissa context, second mantissa context))`), in which case the top two
    /// mantissa bits are also coded with a probability (with the mean of two if there is a second
    /// context); without that, `xs`: the sign predicted from another component (`Some((context,
    /// predicted negative))`).
    #[inline(always)]
    fn rest<I: Io>(&mut self, io: &mut I, t: usize, v: i32, esc_ctx: usize, sign: Option<(usize, bool, u32, usize, Option<usize>)>, xs: Option<(usize, bool)>) -> i32 {
        let a = v.unsigned_abs();
        let neg = (v < 0) as u32;
        let (base, low) = if t == ESC {
            let e = if I::DEC { 0 } else { bitlen(a) as usize };
            let e = 8 + self.esc.sym(io, esc_ctx, 8, e.saturating_sub(8));
            (1u32 << (e - 1), e as u32 - 1)
        } else {
            (BASE[t], LOW[t])
        };
        let (neg, m) = match sign {
            Some((ctx, pneg, pm, mctx, second)) => {
                let flip = self.esign.bit(io, ctx, neg ^ pneg as u32);
                let m = if low > 0 {
                    // where the prediction (1/16 steps) falls in the class and then in the
                    // chosen half, by halves of the halves (2^sh steps each), if the sign agreed
                    // with it
                    let rel = |lo: u32, sh: u32| ((2 + ((pm as i32 - (lo << 4) as i32) >> (sh + 3)).min(4)).max(1) as usize) * (flip ^ 1) as usize;
                    let half = 1u32 << (low - 1);
                    let d = a.wrapping_sub(base);
                    let lc = (low as usize).min(5) - 1;
                    let (lb, lb2) = ((mctx * 5 + lc) * 7, second.map(|mctx2| (mctx2 * 5 + lc) * 7));
                    let r1 = rel(base, low - 1);
                    let top = match lb2 {
                        Some(l2) => self.emant.bit2(&mut self.emant2, io, lb + r1, l2 + r1, 2, (d >> (low - 1)) & 1),
                        None => self.emant.bit(io, lb + r1, (d >> (low - 1)) & 1),
                    };
                    if low > 1 {
                        let r2 = rel(base + (top << (low - 1)), low - 2);
                        let sec = match lb2 {
                            Some(l2) => self.emant.bit2(&mut self.emant2, io, EMANT2 + lb + r2, EMANT2_2 + l2 + r2, 2, (d >> (low - 2)) & 1),
                            None => self.emant.bit(io, EMANT2 + lb + r2, (d >> (low - 2)) & 1),
                        };
                        let r = if low > 2 { io.raw(d & ((half >> 1) - 1), low - 2) } else { 0 };
                        (top << (low - 1)) | (sec << (low - 2)) | r
                    } else {
                        top << (low - 1)
                    }
                } else {
                    0
                };
                (flip ^ pneg as u32, base + m)
            }
            None => match xs {
                Some((ctx, pneg)) => {
                    let flip = self.csign.bit(io, ctx * 3 + t.min(3) - 1, neg ^ pneg as u32);
                    let r = if low > 0 { io.raw(a.wrapping_sub(base) & ((1 << low) - 1), low) } else { 0 };
                    (flip ^ pneg as u32, base + r)
                }
                None => {
                    let r = io.raw((neg << low) | (a.wrapping_sub(base) & ((1 << low) - 1)), low + 1);
                    (r >> low, base + (r & ((1 << low) - 1)))
                }
            },
        };
        let s = -(neg as i32);
        (m as i32 ^ s) - s
    }

    /// Codes the first row (`dir` 0, continuing the block above `nbr`) or first column (`dir` 1,
    /// continuing the block to the left) of a block; `pnz`: the neighbours' mean count of nonzero
    /// coefficients in that edge, `nnz`: the count of `nbr` (if there is one).
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn edge<I: Io>(&mut self, io: &mut I, cls: usize, dir: usize, blk: &mut [i16; 64], nbr: Option<&[i16; 64]>, near: (&[i16; 64], &[i16; 64]), pr: &Pred, rq: &[i64; 64], (nzq, xe): (usize, usize), pnz: usize, nnz: usize, cross: &mut Option<Cross>) -> u8 {
        let step = if dir == 0 { 1 } else { 8 };
        let across = if dir == 0 { 8 } else { 1 };
        let actual = if I::DEC { 0 } else { (1..8).filter(|&i| blk[i * step] != 0).count() };
        // (luma blocks have no other component's counts)
        let ctx = (dir * 8 + nzq) * 8 + pnz;
        // and by the count of the block that the edge continues and the number of nonzero
        // coefficients in the interior row (column) next to the edge
        let adjc = (1..8).filter(|&i| blk[i * step + across] != 0).count();
        let n = self.nze.sym2(&mut self.nze2, io, if cls == 0 { ctx } else { 2 * 8 * 8 + ctx * NXE + xe }, ((cls * 2 + dir) * 8 + nnz) * 8 + adjc, 8, actual);
        // the sums of the extrapolation below for all positions at once (position 0, DC, is not
        // coded here), in 32 bits, wrapping: exact for the coefficients of 8- and 12-bit images
        let [e0, ed] = match (n > 0, nbr) {
            (true, Some(nb)) if dir == 0 => I::L::sums(nb, blk, [&pr.qc, &pr.gc, &pr.qc0, &pr.gc0], false),
            (true, Some(nb)) => I::L::sums(nb, blk, [&pr.qr, &pr.gr, &pr.qr0, &pr.gr0], true),
            _ => [[0; 8]; 2],
        };
        let mut rem = n;
        let mut i = 1;
        // magnitude of the previous edge coefficient, by token (DC is coded later)
        let mut prev = 0;
        while rem > 0 && i < 8 {
            let pos = i * step;
            // the inverse DCT of this frequency along the edge must continue the neighbour's
            // profile at its far side
            // (the last samples, and the gradient), extrapolated by the weight that predicted
            // this position best recently
            let ge = ((cls * 2 + dir) * 8 + i) * 2 + (nzq >= 3) as usize;
            let (p16, accs, eb) = match nbr {
                Some(_) => {
                    // with gradient weight g (eighths): (b0 + g bd) / 2^20 in 1/16 steps
                    let (b0, bd) = ((e0[i] >> 8) as i64 * rq[pos], ((ed[i] >> 8) as i64 * rq[pos]) >> 3);
                    let e = &self.eg[ge & 63];
                    // (the first smallest: the minimum of error * 8 + index, without branches)
                    let k = (1..NEG).fold(e[0] << 3, |m, c| m.min(e[c] << 3 | c as u32)) as usize & 7;
                    // and its recent mean error: below 1/2 step, 1, 2, more
                    (((b0 + EGS[k] * bd + (1 << 19)) >> 20).clamp(-(1 << 20), 1 << 20) as i32, Some((b0, bd)), bitlen(e[k] >> 8).min(3) as usize)
                }
                None => (0, None, 0),
            };
            let p = ((p16 + 8) >> 4).clamp(-32767, 32767);
            let pb = pbucket(p);
            let v = blk[pos] as i32;
            // the interior coefficient next to it (same frequency along the edge)
            // and the next one inwards, and the previous edge coefficient
            let av = 2 * (blk[pos + across] as i32).unsigned_abs() + (blk[pos + 2 * across] as i32).unsigned_abs() + prev;
            let adj = AQ[av.min(31) as usize] as usize;
            // the same coefficient in the blocks above and to the left
            // (and in the other component)
            let xm = cross.as_ref().map_or(0, |c| ((c.r[pos] as i32).unsigned_abs().min(4095) * c.rho[0] * 2 + 128) >> 8);
            let nm = BEQ[((near.0[pos] as i32).unsigned_abs() + (near.1[pos] as i32).unsigned_abs() + xm).min(15) as usize] as usize;
            let cd = (cls * 2 + dir) * 7 + i - 1;
            let zctx = (((cd * 4 + rem.min(4) - 1) * NPB + pb) * NAQ + adj) * NBE + nm;
            let tp = token(p.unsigned_abs());
            let r4 = rem.min(4) - 1;
            // the same coefficient of the block that the prediction continues
            let nn = if dir == 0 { near.0[pos] } else { near.1[pos] };
            let tn = token(nn.unsigned_abs() as u32).min(11);
            let m1 = ((((cls * 2 + dir) * 4 + EZ[i] as usize) * NTOK + tp) * NAQ + adj) * NBE + nm;
            let (m2, m3) = (((cd * NTOK + tp) * NAQ + adj) * 4 + r4, ((cd * NTOK + tp) * 12 + tn) * 4 + r4);
            let mean = self.emag.mean3::<I::L>(&self.emag2, &self.emag3, m1, m2, m3);
            // also by position, the remaining count, the block's count and the recent error of
            // the extrapolation (3/4 of the mean)
            let z2 = ((cd * 8 + rem.min(8) - 1) * 8 + nzq) * 4 + eb;
            if self.ezero.bit2(&mut self.ezero2, io, zctx, z2, 3, (v != 0) as u32) != 0 {
                let t = 1 + self.emag.sym3(&mut self.emag2, &mut self.emag3, io, &mean, m1, m2, m3, NTOK - 1, if I::DEC { 0 } else { token(v.unsigned_abs()) - 1 });
                // the sign follows the prediction more reliably when the magnitudes agree
                let dt = (t as i32 - tp as i32).clamp(-3, 3) as usize + 3;
                // whether the extrapolations without and with the full gradient agree in sign
                let ag = accs.map_or(0, |(b0, bd)| ((b0 < 0) == (b0 + 8 * bd < 0)) as usize);
                // and whether the other component's sign agrees with it
                let ectx = |pn: bool, tp: usize, dt: usize| {
                    let x = cross.as_ref().map_or(0, |c| c.agree(pos, pn));
                    let full = (((((cls * 2 + dir) * NTOK + tp) * 7 + dt) * 4 + eb) * 2 + ag) * 2 + pn as usize;
                    std::hint::select_unpredictable(x == 0, full, ESIGN_X + x.wrapping_sub(1).wrapping_mul(ESIGN_XC) + ((dir * NTOK + tp) * 7 + dt) * 2 + pn as usize)
                };
                // (the mantissa also by position and neighbourhood)
                let (sign, xs) = if p != 0 {
                    (Some((ectx(p < 0, tp, dt), p < 0, p16.unsigned_abs(), cls * 2 + dir, Some(cd * NAQ + adj))), None)
                } else if p16 != 0 {
                    // a prediction below half a step, by sixteenths (in place of dt)
                    (Some((ectx(p16 < 0, 0, (p16.unsigned_abs() as usize - 1).min(6)), p16 < 0, p16.unsigned_abs(), cls * 2 + dir, Some(cd * NAQ + adj))), None)
                } else {
                    (None, cross.as_ref().and_then(|c| c.pred(pos)).map(|(k, pn)| (NXS + k, pn)))
                };
                let x = self.rest(io, t, v, cls * 2, sign, xs);
                if I::DEC {
                    blk[pos] = x as i16;
                }
                if let Some(c) = cross.as_mut() {
                    c.update(pos, x);
                }
                prev = TMAG[t & 15] as u32;
                rem -= 1;
            } else {
                prev = 0;
            }
            if let Some((b0, bd)) = accs {
                let v = ((blk[pos] as i64) << 24) - b0;
                let e = &mut self.eg[ge & 63];
                for (c, x) in e.iter_mut().enumerate() {
                    let d = ((v - EGS[c] * bd).unsigned_abs() >> 20).min(1024) as u32;
                    *x = *x - (*x >> EG_SHIFT) + d;
                }
            }
            i += 1;
        }
        n as u8
    }

    /// Codes one block. `a`, `l`: the blocks above and to the left with their `Info` (`None` if
    /// outside the stripe); `q`: the component's quantization table; `xref`: the block of an
    /// earlier component at the same place, the kind of relation and the luma reference (see
    /// `Cross`); `xinfo`: the `Info` of the earlier chroma component's block at the same place.
    /// Returns the block's `Info`.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub fn block<I: Io>(&mut self, io: &mut I, cls: usize, q: &[i64; 64], rq: &[i64; 64], pr: &mut Pred, blk: &mut [i16; 64], a: Option<(&[i16; 64], &Info)>, l: Option<(&[i16; 64], &Info)>, diag: (Option<&[i16; 64]>, Option<&[i16; 64]>), luma: usize, xref: Option<(&[i16; 64], usize, Option<&[i16; 64]>, [u32; 2], [i32; 2])>, xinfo: Option<Info>, code_dc: bool, code_ac: bool) -> Info {
        let (na, nl) = match (a, l) {
            (Some(a), Some(l)) => (a.0, l.0),
            (Some(a), None) => (a.0, a.0),
            (None, Some(l)) => (l.0, l.0),
            (None, None) => (&ZERO_BLOCK, &ZERO_BLOCK),
        };
        let nal = diag.0.unwrap_or(na);
        let nar = diag.1.unwrap_or(na);
        let avg = |f: fn(&Info) -> u8| -> usize {
            match (a, l) {
                (Some(a), Some(l)) => (f(a.1) as usize + f(l.1) as usize + 1) >> 1,
                (Some(a), None) => f(a.1) as usize,
                (None, Some(l)) => f(l.1) as usize,
                _ => 0,
            }
        };
        let mut info = Info::default();
        if code_ac {
            let nb = a.map_or(0, |x| x.1.agree as i32) + l.map_or(0, |x| x.1.agree as i32);
            let nb2 = a.map_or(0, |x| x.1.agree2 as i32) + l.map_or(0, |x| x.1.agree2 as i32);
            let mut cross = xref.map(|(r, xc, r2, rho, g)| Cross { r, xc, rho, cum: 0, nb, r2, cum2: 0, nb2, g, own: 0 });
            let actual = if I::DEC { 0 } else { INTERIOR.iter().filter(|&&p| blk[p as usize] != 0).count() };
            // (luma blocks have no hint)
            let nzb = NZB[avg(|i| i.nz77) & 63] as usize;
            let ctx = if cls == 0 { nzb } else { NNZB + nzb * NLUMA + luma };
            // and by the larger and the smaller of the neighbours' counts
            let (za, zl) = match (a, l) {
                (Some(a), Some(l)) => (a.1.nz77 as usize, l.1.nz77 as usize),
                (Some(x), None) | (None, Some(x)) => (x.1.nz77 as usize, x.1.nz77 as usize),
                _ => (0, 0),
            };
            let c2 = (cls * NNZB + NZB[za.max(zl) & 63] as usize) * NNZB + NZB[za.min(zl) & 63] as usize;
            let hi = self.nz_hi.sym2(&mut self.nz_hi2, io, ctx, c2, 7, actual >> 3);
            let lo = self.nz_lo.sym2(&mut self.nz_lo2, io, ctx * 7 + hi, c2 * 7 + hi, 8, actual & 7);
            let nz77 = hi * 8 + lo;
            info.nz77 = nz77 as u8;
            let mut rem = nz77;
            let zbase = |rem: usize| (cls * NREM + REMB[rem & 63] as usize) * 49 * NEQ;
            let mbase = cls * NREM8 * NKZ * NEQ;
            let mut zrow = zbase(rem);
            // magnitude estimate of each coefficient: a weighted sum of the same coefficient in the
            // neighbouring blocks (capped so that the sum fits; any capped one saturates the
            // bucket) and of the interior coefficients of this block coded before it, by token,
            // added as they are coded (first-row and first-column coefficients come later)
            let mut est = [0u16; 64];
            // the inputs of the interior value prediction (see NLSW) of each column, then of each
            // row, dequantized (in 32 bits, wrapping: exact for the coefficients of 8- and 12-bit
            // images, and they vectorize)
            let mut colv = [[0i32; 4]; 8];
            let mut rowv = [[0i32; 4]; 8];
            if rem > 0 {
                let m = |b: &[i16; 64], k: usize| b[k].unsigned_abs().min(4095);
                for (k, e) in est.iter_mut().enumerate() {
                    *e = 2 * (m(na, k) + m(nl, k)) + m(nal, k) + m(nar, k);
                }
                if let Some(c) = &cross {
                    // min(1023, m * w >> 8) from the halves of the 16-bit products (m, w <= 4096)
                    let term = |m: u16, w: u16| (((((m as u32 * w as u32) >> 16) as u16).min(4) << 8) | (m.wrapping_mul(w) >> 8)).min(1023);
                    let w = XW * c.rho[0] as u16;
                    for (k, e) in est.iter_mut().enumerate() {
                        *e += term(m(c.r, k), w);
                    }
                    if let Some(r2) = c.r2 {
                        let w = XW2 * c.rho[1] as u16;
                        for (k, e) in est.iter_mut().enumerate() {
                            *e += term(m(r2, k), w);
                        }
                    }
                }
                if let Some((ab, _)) = a {
                    let [sa, ga] = I::L::sums(ab, &ZERO_BLOCK, [&pr.qc, &pr.gc, &pr.qc0, &pr.gc0], false);
                    for u in 0..8 {
                        colv[u] = [sa[u] >> 12, ga[u] >> 12, 0, 0];
                    }
                }
                if let Some((lb, _)) = l {
                    let [s, t] = I::L::sums(lb, &ZERO_BLOCK, [&pr.qr, &pr.gr, &pr.qr0, &pr.gr0], true);
                    for v in 0..8 {
                        rowv[v] = [s[v] >> 12, t[v] >> 12, 0, 0];
                    }
                }
            }
            let nzq = NZQ8[nz77 & 63] as usize;
            let mut i = 0usize;
            // the estimate of the next position is carried over in a register (read before this
            // position's update of it), so that it does not wait for the update to be stored
            let mut e = est[INTERIOR[0] as usize];
            while rem > 0 && i < 49 {
                let pos = INTERIOR[i] as usize & 63;
                let (np, ns) = NEXT[i];
                let mut en = est[np as usize & 63];
                let nb = EQ[e.min(340) as usize] as usize;
                let v = blk[pos] as i32;
                if self.zero.bit(io, zrow + i * NEQ + nb, (v != 0) as u32) != 0 {
                    let (pv, pu) = (pos / 8, pos % 8);
                    let (c, r) = (colv[pu], rowv[pv]);
                    let f = [c[0], c[1], c[2], c[3], r[0], r[1], r[2], r[3]];
                    let w = &pr.w[i];
                    let pw = (0..NLSW).fold(0i64, |s, k| s.wrapping_add(w[k] as i64 * f[k] as i64));
                    let p = pw >> 16;
                    let pa = p.unsigned_abs().min(1 << 20) as u32;
                    // the magnitude by the estimate with the predicted magnitude added
                    let nm = EQ[(e as u32 + (pa >> 1)).min(340) as usize] as usize;
                    let m1 = mbase + REM8[rem & 63] as usize + IZE[i] as usize + nm;
                    // also by position, that bucket and the block's count, and by position and
                    // the remaining count
                    let (m2, m3) = (((cls * 49 + i) * NEQ + nm) * 8 + nzq, (cls * 49 + i) * NREM + REMB[rem & 63] as usize);
                    let mean = self.mag.mean3::<I::L>(&self.mag2, &self.mag3, m1, m2, m3);
                    let t = 1 + self.mag.sym3(&mut self.mag2, &mut self.mag3, io, &mean, m1, m2, m3, NTOK - 1, if I::DEC { 0 } else { token(v.unsigned_abs()) - 1 });
                    let pb = (bitlen(pa >> 1) as usize).min(NPS - 1);
                    // and whether the other component's sign agrees with the prediction's
                    let xr = cross.as_ref().map_or(0, |c| c.agree(pos, p < 0));
                    let sc = (IZE[i] as usize / NEQ * NPS + pb) * NSAB + SAB[t & 15] as usize;
                    let sctx = std::hint::select_unpredictable(xr == 0, cls * ISIGN_XC + sc, ISIGN_X + xr.wrapping_sub(1).wrapping_mul(ISIGN_XC) + sc);
                    let x = self.irest(io, t, v, cls * 2, cls, sctx, p < 0, pa, nb);
                    if I::DEC {
                        blk[pos] = x as i16;
                    }
                    if let Some(c) = cross.as_mut() {
                        c.update(pos, x);
                    }
                    let dq = x.wrapping_mul(pr.q[pos]);
                    let e = ((x as i64) << 20).wrapping_sub(pw);
                    // the step with the sign of the error, 0 below half a step
                    let mu = std::hint::select_unpredictable(e < 0, -pr.mu[i], pr.mu[i]);
                    let d = std::hint::select_unpredictable(e.unsigned_abs() >= 1 << 19, mu, 0);
                    let w = &mut pr.w[i];
                    for k in 0..NLSW {
                        // d with the sign of the input, 0 for a zero input
                        let m = f[k] >> 31;
                        w[k] = w[k].wrapping_add(((d ^ m) - m) & -((f[k] != 0) as i32));
                    }
                    if !I::DEC && !self.ls_acc.is_empty() {
                        self.ls_add(cls, i, &f.map(|x| x as f64), dq as f64);
                    }
                    let (k, j) = (2 + (pv & 1), 2 + (pu & 1));
                    colv[pu][k] = colv[pu][k].wrapping_add(dq);
                    rowv[pv][j] = rowv[pv][j].wrapping_add(dq);
                    let a = TMAG[t & 15];
                    let f = &FWD[i];
                    for (j, &k) in f.iter().enumerate() {
                        let e = &mut est[k as usize & 63];
                        *e = e.wrapping_add(a << (j < 2) as u32);
                    }
                    en = en.wrapping_add(a << ns);
                    rem -= 1;
                    zrow = zbase(rem);
                }
                e = en;
                i += 1;
            }
            // the edge's count in the earlier chroma component's block and the bucket of the edge's
            // magnitude sum in the luma reference
            let lb = |r: &[i16; 64], step: usize| {
                let s: u32 = (1..8).map(|i| (r[i * step] as i32).unsigned_abs().min(64)).sum();
                (s >= 6) as usize + (s >= 16) as usize + (s >= 40) as usize
            };
            let xe = |n: u8, step: usize| match (&cross, xinfo) {
                (Some(c), None) if c.xc == 0 => 4 + lb(c.r, step),
                (Some(Cross { r2: Some(r2), .. }), Some(_)) => 8 + XEQ[n as usize & 7] as usize * 4 + lb(r2, step),
                (_, Some(_)) => 1 + XEQ[n as usize & 7] as usize,
                _ => 0,
            };
            let (xr, xc) = (xe(xinfo.map_or(0, |i| i.nzr), 1), xe(xinfo.map_or(0, |i| i.nzc), 8));
            let (zr, zc) = (avg(|i| i.nzr), avg(|i| i.nzc));
            info.nzr = self.edge(io, cls, 0, blk, a.map(|x| x.0), (na, nl), pr, rq, (nzq, xr), zr, a.map_or(zr, |x| x.1.nzr as usize & 7), &mut cross);
            info.nzc = self.edge(io, cls, 1, blk, l.map(|x| x.0), (na, nl), pr, rq, (nzq, xc), zc, l.map_or(zc, |x| x.1.nzc as usize & 7), &mut cross);
            if let Some(c) = cross {
                info.agree = c.cum.clamp(-64, 64) as i8;
                info.agree2 = c.cum2.clamp(-64, 64) as i8;
                info.asum = c.own;
            }
        }
        if code_dc {
            // the mean of the edge column (row) continues the neighbour's last column (row),
            // extrapolated by the gradient weight that predicted that side best recently
            let side = |nb: &[i16; 64], st: usize| {
                let mut a0 = nb[0] as i64 * q[0] * COS7[0];
                let mut ad = 0i64;
                for j in 1..8 {
                    let o = j * st;
                    let (x, y) = (nb[o] as i64 * q[o], blk[o] as i64 * q[o]);
                    a0 += x * COS7[j] - y * COS0[j];
                    ad += x * (COS7[j] - COS6[j]) - y * (COS0[j] - COS1[j]);
                }
                ((a0 >> 8) * rq[0], ((ad >> 8) * rq[0]) >> 3)
            };
            let sides = [l.map(|(lb, _)| side(lb, 1)), a.map(|(ab, _)| side(ab, 8))];
            let nz = (info.nz77 + info.nzr + info.nzc) as u32;
            let nzq = match nz {
                0 => 0,
                1..=3 => 1,
                4..=10 => 2,
                _ => 3,
            };
            // the recent errors by class and activity
            let ec = cls * 4 + nzq;
            // both sides weighted by quarters (`w` for the left one)
            let comb = |xy: (Option<i32>, Option<i32>), w: i32| match xy {
                (Some(x), Some(y)) => Some((x * w + y * (4 - w) + 2) >> 2),
                (Some(x), None) | (None, Some(x)) => Some(x),
                _ => None,
            };
            let best = |e: &[u32; NDG]| (1..NDG).fold(e[0] << 3, |m, c| m.min(e[c] << 3 | c as u32)) as usize & 7;
            let mut ps = [None; 2];
            for (s, p) in ps.iter_mut().enumerate() {
                if let Some((a0, ad)) = sides[s] {
                    *p = Some(((a0 + DGS[best(&self.edc[ec * 2 + s])] * ad + (1 << 19)) >> 20).clamp(-(1 << 20), 1 << 20) as i32);
                }
            }
            let xy = (ps[0], ps[1]);
            let kw = best(&self.edw[ec]);
            // the median predictor from the neighbours' DC, mixed in by quarters
            let pm = match (a, l) {
                (Some((ab, _)), Some((lb, _))) => {
                    let (da, dl) = (ab[0] as i32, lb[0] as i32);
                    let g = da + dl - diag.0.map_or((da + dl) >> 1, |b| b[0] as i32);
                    Some(g.clamp(da.min(dl), da.max(dl)) << 4)
                }
                (Some((b, _)), None) | (None, Some((b, _))) => Some((b[0] as i32) << 4),
                _ => None,
            };
            let mix = |p: i32, w: i32| match pm {
                Some(m) => (p * (4 - w) + m * w + 2) >> 2,
                None => p,
            };
            let pb = comb(xy, kw as i32).unwrap_or(0);
            let p16 = mix(pb, best(&self.edm[ec]) as i32);
            let ub = match xy {
                (Some(x), Some(y)) => 1 + bitlen(((x - y + 8) >> 4).unsigned_abs()).min(6) as usize,
                (Some(_), None) | (None, Some(_)) => 7,
                _ => 0,
            };
            let pred = ((p16 + 8) >> 4).clamp(-32767, 32767);
            let d = blk[0] as i32 - pred;
            // how well the neighbours' DC was predicted
            let dq = DCQ[avg(|i| i.dct) & 15] as usize;
            // and by the difference of the two extrapolations
            let dif = match xy {
                (Some(x), Some(y)) => token(((x - y + 8) >> 4).unsigned_abs()),
                _ => 15,
            };
            let t = self.dc.sym2(&mut self.dc2, io, ((cls * 8 + ub) * 4 + nzq) * 4 + dq, (cls * 16 + dif) * 4 + nzq, NTOK, if I::DEC { 0 } else { token(d.unsigned_abs()) });
            info.dct = t as u8;
            // the sign by the rounding of the prediction
            let f = p16 - (pred << 4);
            // and by the sides of the rounded prediction the two extrapolations fall on
            let side_of = |x: Option<i32>| x.map_or(0, |x| (x > pred << 4) as i32 - (x < pred << 4) as i32);
            let vote = (side_of(xy.0) + side_of(xy.1) + 2) as usize;
            let sctx = ESIGN_DC + (((cls * 4 + t.min(4) - 1) * 5 + (f.unsigned_abs() as usize + 1) / 2) * 5 + vote) * 2 + (f < 0) as usize;
            let x = if t != 0 { self.rest(io, t, d, cls * 2 + 1, Some((sctx, f < 0, f.unsigned_abs(), 4 + cls, None)), None) } else { 0 };
            if I::DEC {
                blk[0] = (pred + x) as i16;
            }
            let v16 = (blk[0] as i64) << 4;
            for (s, side) in sides.iter().enumerate() {
                if let Some((b0, bd)) = *side {
                    let v = (v16 << 20) - b0;
                    for (c, x) in self.edc[ec * 2 + s].iter_mut().enumerate() {
                        let d = ((v - DGS[c] * bd).unsigned_abs() >> 20).min(1024) as u32;
                        *x = *x - (*x >> EG_SHIFT) + d;
                    }
                }
            }
            if pm.is_some() {
                for (c, x) in self.edm[ec].iter_mut().enumerate() {
                    let d = (v16 - mix(pb, c as i32) as i64).unsigned_abs().min(1024) as u32;
                    *x = *x - (*x >> EG_SHIFT) + d;
                }
            }
            if let (Some(_), Some(_)) = xy {
                for (c, x) in self.edw[ec].iter_mut().enumerate() {
                    let d = (v16 - comb(xy, c as i32).unwrap() as i64).unsigned_abs().min(1024) as u32;
                    *x = *x - (*x >> EG_SHIFT) + d;
                }
            }
        }
        info
    }
}

/// Which blocks of a component a stripe codes.
#[derive(Clone, Copy, Debug)]
pub struct Extent {
    /// DC and AC coded at all
    pub dc: bool,
    pub ac: bool,
    /// coded blocks: columns, and rows relative to the stripe's first row
    pub dc_w: usize,
    pub dc_h: usize,
    pub ac_w: usize,
    pub ac_h: usize,
}

/// Codes the blocks of one stripe in MCU-row order. `planes[c]` holds `rows[c]` block rows of
/// component c (`width[c]` blocks of 64 coefficients each): block row `by` of the stripe lives
/// in row `by % rows[c]`, so a ring of two MCU rows is enough (the decoder clears a row before
/// reusing it). `v[c]`: block rows per MCU row; `q[c]`: quantization table (natural order).
/// `emit(my, planes)` is called after every MCU row. `start`: the model to start from (the
/// first stripe's, see `Model::into_seed`), else the learned initial one. Returns the model at
/// the end.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
pub fn code_stripe<I: Io>(io: &mut I, start: Option<&Model>, planes: &mut [&mut [i16]], rows: &[usize], width: &[usize], v: &[usize], ext: &[Extent], q: &[[u16; 64]], mcu_rows: usize, mut emit: impl FnMut(usize, &[&mut [i16]]) -> Result<(), String>) -> Result<Model, String> {
    let mut m = start.cloned().unwrap_or_else(Model::new);
    let qs: Vec<[i64; 64]> = q.iter().map(|t| t.map(|x| x as i64)).collect();
    let rqs: Vec<[i64; 64]> = q.iter().map(|t| t.map(|x| (1i64 << 32) / (COS0[0] * x.max(1) as i64))).collect();
    let mut preds: Vec<Pred> = q.iter().enumerate().map(|(c, t)| Pred::new(&m.lsw, (c > 0) as usize, t)).collect();
    for (p, w) in preds.iter_mut().zip(&m.pw) {
        p.w = *w;
    }
    // a step above 16383 saturates the dequantized luma of any nonzero coefficient anyway
    let ql: [i16; 64] = q.first().map_or([0; 64], |t| t.map(|x| x.min(16383) as i16));
    // the reciprocals of half the chroma steps, 65536 / q, as 32768 k + l (both 16-bit)
    let rcs: Vec<([i16; 64], [i16; 64])> = q.iter().map(|t| (t.map(|x| ((65536 / x.max(1) as i32) >> 15) as i16), t.map(|x| ((65536 / x.max(1) as i32) & 32767) as i16))).collect();
    let mut infos: Vec<Vec<Info>> = width.iter().map(|&w| vec![Info::default(); 2 * w]).collect();
    // the downsampled luma of this MCU row's chroma block rows, by (block row, width, rows per MCU)
    let mut down: Vec<((usize, usize, usize), Vec<[i16; 64]>)> = Vec::new();
    // and the same requantized for a chroma quantization table, with the AC magnitude sum of
    // every block (as `Info::asum`)
    #[allow(clippy::type_complexity)]
    let mut lrefs: Vec<((usize, usize, usize), [u16; 64], Vec<[i16; 64]>, Vec<u32>)> = Vec::new();
    // per component, decaying sums of the AC magnitudes of its blocks and of their references
    // (`Cross::rho`), starting at a ratio of 1 for a chroma reference, 1/2 for the luma one
    let mut mags: Vec<[u32; 4]> = (0..planes.len())
        .map(|c| {
            let chroma_ref = (1..c).any(|k| width[k] == width[c] && v[k] == v[c]);
            [if chroma_ref { 1 << 12 } else { 1 << 11 }, 1 << 12, 1 << 11, 1 << 12]
        })
        .collect();
    let mut gs: Vec<[i32; 2]> = vec![[0; 2]; planes.len()];
    for my in 0..mcu_rows {
        down.clear();
        lrefs.clear();
        for c in 0..planes.len() {
            let cls = (c > 0) as usize;
            let w = width[c];
            let e = ext[c];
            let len = w * 64;
            for r in 0..v[c] {
                let by = my * v[c] + r;
                let row_dc = e.dc && by < e.dc_h;
                let row_ac = e.ac && by < e.ac_h;
                if !row_dc && !row_ac {
                    continue;
                }
                let slot = by % rows[c];
                let (earlier, later) = planes.split_at_mut(c);
                let plane = &mut *later[0];
                if I::DEC && by >= rows[c] {
                    plane[slot * len..(slot + 1) * len].fill(0);
                }
                let (cur, above): (&mut [i16], Option<&[i16]>) = if by == 0 {
                    (&mut plane[slot * len..(slot + 1) * len], None)
                } else {
                    let up = (by - 1) % rows[c];
                    if up < slot {
                        let (a, b) = plane.split_at_mut(slot * len);
                        (&mut b[..len], Some(&a[up * len..(up + 1) * len]))
                    } else {
                        let (a, b) = plane.split_at_mut(up * len);
                        (&mut a[slot * len..(slot + 1) * len], Some(&b[..len]))
                    }
                };
                // chroma: the luma blocks at the same place (coded before this row) at the
                // component's resolution and in half its quantization steps
                let mut li = None;
                if c > 0 && width[0] % w == 0 && v[0] % v[c] == 0 && matches!((width[0] / w, v[0] / v[c]), (1 | 2, 1 | 2)) {
                    li = lrefs.iter().position(|d| d.0 == (by, w, v[c]) && d.1 == q[c]);
                }
                if li.is_none() && c > 0 && width[0] % w == 0 && v[0] % v[c] == 0 && matches!((width[0] / w, v[0] / v[c]), (1 | 2, 1 | 2)) {
                    let (fx, fy) = (width[0] / w, v[0] / v[c]);
                    let at = match down.iter().position(|d| d.0 == (by, w, v[c])) {
                        Some(i) => i,
                        None => {
                            let w0 = width[0];
                            let luma = &*earlier[0];
                            let blk = |bx: usize, i: usize| -> &[i16; 64] {
                                let (dx, dy) = (i % fx, (i / fx).min(fy - 1));
                                let s = (by * fy + dy) % rows[0];
                                luma[(s * w0 + bx * fx + dx) * 64..][..64].try_into().unwrap()
                            };
                            down.push(((by, w, v[c]), (0..w).map(|bx| luma_down([blk(bx, 0), blk(bx, 1), blk(bx, 2), blk(bx, 3)], fx, fy, &ql)).collect()));
                            down.len() - 1
                        }
                    };
                    let (rk, rl) = &rcs[c];
                    let lr: Vec<[i16; 64]> = down[at].1.iter().map(|d| std::array::from_fn(|k| d[k] * rk[k] + ((d[k] as i32 * rl[k] as i32 + 16384) >> 15) as i16)).collect();
                    let sums = lr.iter().map(|b| b[1..].iter().map(|&x| (x as i32).unsigned_abs().min(4095)).sum()).collect();
                    lrefs.push(((by, w, v[c]), q[c], lr, sums));
                    li = Some(lrefs.len() - 1);
                }
                let (lref, lsum): (&[[i16; 64]], &[u32]) = li.map_or((&[], &[]), |i| (&lrefs[i].2, &lrefs[i].3));
                // and the nearest earlier chroma component with the same block grid
                let xrow = (1..c).rev().find(|&k| width[k] == w && v[k] == v[c]).map(|k| {
                    let s = by % rows[k];
                    &earlier[k][s * len..(s + 1) * len]
                });
                // chroma: the 7x7 count of the earlier chroma component's block at the same place,
                // else the mean 7x7 count of the luma blocks there (coded before) with the bucket of
                // the luma reference's magnitude sum
                let lsb = |bx: usize| lsum.get(bx).map_or(0, |&s| (s >= 16) as u8 + (s >= 64) as u8 + (s >= 256) as u8);
                let mut hints = Vec::new();
                let mut xinfos: Vec<Info> = Vec::new();
                if let Some(k) = (1..c).rev().find(|&k| width[k] == w && v[k] == v[c]) {
                    xinfos = infos[k][(by & 1) * w..][..w].to_vec();
                    hints = xinfos.iter().map(|i| 16 + XNZQ[i.nz77 as usize & 63]).collect();
                } else if c > 0 && v[0] % v[c] == 0 && width[0] % w == 0 {
                    let (fx, fy) = (width[0] / w, v[0] / v[c]);
                    let (luma, _) = infos.split_at(1);
                    let w0 = width[0];
                    hints = (0..w)
                        .map(|bx| {
                            let mut sum = 0usize;
                            for dy in 0..fy {
                                let ly = by * fy + dy;
                                for dx in 0..fx {
                                    sum += luma[0][(ly & 1) * w0 + bx * fx + dx].nz77 as usize;
                                }
                            }
                            LUMAQ[(sum / (fx * fy)).min(63)] * 4 + lsb(bx)
                        })
                        .collect();
                }
                let (i0, i1) = infos[c].split_at_mut(w);
                let (prev, now) = if by & 1 == 0 { (&*i1, i0) } else { (&*i0, i1) };
                for bx in 0..w {
                    let cd = row_dc && bx < e.dc_w;
                    let ca = row_ac && bx < e.ac_w;
                    if !cd && !ca {
                        now[bx] = Info::default();
                        continue;
                    }
                    let (left, rest) = cur.split_at_mut(bx * 64);
                    let blk: &mut [i16; 64] = (&mut rest[..64]).try_into().unwrap();
                    let l = if bx > 0 { Some((<&[i16; 64]>::try_from(&left[(bx - 1) * 64..bx * 64]).unwrap(), &now[bx - 1])) } else { None };
                    let a = above.map(|ab| (<&[i16; 64]>::try_from(&ab[bx * 64..bx * 64 + 64]).unwrap(), &prev[bx]));
                    let al = if bx > 0 { above.map(|ab| <&[i16; 64]>::try_from(&ab[(bx - 1) * 64..bx * 64]).unwrap()) } else { None };
                    let ar = if bx + 1 < w.min(e.ac_w.max(e.dc_w)) { above.map(|ab| <&[i16; 64]>::try_from(&ab[(bx + 1) * 64..bx * 64 + 128]).unwrap()) } else { None };
                    let luma = hints.get(bx).copied().unwrap_or(0) as usize;
                    let mg = &mags[c];
                    let rho = |sc: u32, sr: u32| ((sc << 8) / (sr + 1)).min(512);
                    let rhos = [rho(mg[0], mg[1]), rho(mg[2], mg[3])];
                    let xref = match (xrow, lref.get(bx)) {
                        (Some(r), lr) => Some((<&[i16; 64]>::try_from(&r[bx * 64..bx * 64 + 64]).unwrap(), 1, lr, rhos, gs[c])),
                        (None, Some(lr)) => Some((lr, 0, None, rhos, gs[c])),
                        (None, None) => None,
                    };
                    io.block();
                    let info = m.block(io, cls, &qs[c], &rqs[c], &mut preds[c], blk, a, l, (al, ar), luma, xref, xinfos.get(bx).copied(), cd, ca);
                    if let Some((r, xc, r2, _, _)) = xref {
                        let g = &mut gs[c];
                        g[0] = g[0] - g[0] / 16 + info.agree as i32;
                        g[1] = g[1] - g[1] / 16 + info.agree2 as i32;
                        let own = info.asum;
                        // the earlier chroma block's sum is in its Info if it had a reference
                        let rs = match (xc, xinfos.get(bx)) {
                            (0, _) => lsum[bx],
                            (_, Some(i)) if !lref.is_empty() => i.asum,
                            _ => r[1..].iter().map(|&x| (x as i32).unsigned_abs().min(4095)).sum::<u32>(),
                        };
                        let mg = &mut mags[c];
                        mg[0] = mg[0] - (mg[0] >> RHO_DECAY) + own;
                        mg[1] = mg[1] - (mg[1] >> RHO_DECAY) + rs;
                        if r2.is_some() {
                            mg[2] = mg[2] - (mg[2] >> RHO_DECAY) + own;
                            mg[3] = mg[3] - (mg[3] >> RHO_DECAY) + lsum[bx];
                        }
                    }
                    now[bx] = info;
                }
            }
        }
        emit(my, planes)?;
    }
    m.pw = preds.iter().map(|p| p.w).collect();
    Ok(m)
}

/// Geometry of a stripe for `encode_stripe` and `decode_stripe`; `rows[c]`: block rows held
/// per component (see `code_stripe`).
pub struct Geometry<'a> {
    pub rows: &'a [usize],
    pub width: &'a [usize],
    pub v: &'a [usize],
    pub ext: &'a [Extent],
    pub q: &'a [[u16; 64]],
    pub mcu_rows: usize,
}

#[inline(always)]
fn encode_with<L: Lanes>(planes: &mut [&mut [i16]], g: &Geometry, start: Option<&Model>) -> (Vec<u8>, Vec<u8>, Model) {
    let mut e = Enc::<L>::new();
    let m = code_stripe(&mut e, start, planes, g.rows, g.width, g.v, g.ext, g.q, g.mcu_rows, |_, _| Ok(())).unwrap_or_default();
    let (r, w) = e.finish();
    (r, w, m)
}

#[inline(always)]
fn decode_with<L: Lanes>(rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]], g: &Geometry, start: Option<&Model>, emit: impl FnMut(usize, &[&mut [i16]]) -> Result<(), String>) -> Result<Model, String> {
    let mut d = Dec::<L>::new(rans, raw);
    let m = code_stripe(&mut d, start, planes, g.rows, g.width, g.v, g.ext, g.q, g.mcu_rows, emit)?;
    if d.overrun() {
        return Err("corrupt .komla file".into());
    }
    Ok(m)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt")]
unsafe fn encode_avx2(planes: &mut [&mut [i16]], g: &Geometry, start: Option<&Model>) -> (Vec<u8>, Vec<u8>, Model) {
    encode_with::<Avx2>(planes, g, start)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt")]
unsafe fn decode_avx2(rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]], g: &Geometry, start: Option<&Model>, emit: impl FnMut(usize, &[&mut [i16]]) -> Result<(), String>) -> Result<Model, String> {
    decode_with::<Avx2>(rans, raw, planes, g, start, emit)
}

#[cfg(target_arch = "x86_64")]
fn has_avx2() -> bool {
    std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("bmi1") && std::is_x86_feature_detected!("bmi2") && std::is_x86_feature_detected!("lzcnt") && std::is_x86_feature_detected!("popcnt")
}

/// Codes the coefficients of one stripe, starting from model `start` (see `code_stripe`);
/// returns (rANS bytes, raw bits, final model).
pub fn encode_stripe(planes: &mut [&mut [i16]], g: &Geometry, start: Option<&Model>) -> (Vec<u8>, Vec<u8>, Model) {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: the CPU has every feature the function is compiled for.
        return unsafe { encode_avx2(planes, g, start) };
    }
    encode_with::<Base>(planes, g, start)
}

/// Decodes the coefficients of one stripe into zeroed planes, calling `emit` after every MCU
/// row (see `code_stripe`); returns the final model.
pub fn decode_stripe(rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]], g: &Geometry, start: Option<&Model>, emit: impl FnMut(usize, &[&mut [i16]]) -> Result<(), String>) -> Result<Model, String> {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: the CPU has every feature the function is compiled for.
        return unsafe { decode_avx2(rans, raw, planes, g, start, emit) };
    }
    decode_with::<Base>(rans, raw, planes, g, start, emit)
}

/// Builds the priors file from summed `Model::stats` of a training set of `files` files and the
/// interior prediction weights `lsw`: distributions with at least `min` observations start from
/// the observed frequencies, with the update count of `count0` (binary: `bit_count0`) so that
/// they adapt from there.
pub fn build_priors(totals: &[(usize, Vec<u64>)], lsw: &[i16], min: u64, count0: u8, bit_count0: u8) -> Vec<u8> {
    let was = TRAIN.swap(true, std::sync::atomic::Ordering::Relaxed);
    let fresh = Model::new();
    TRAIN.store(was, std::sync::atomic::Ordering::Relaxed);
    let defaults: Vec<Vec<[u16; 16]>> = [&fresh.nz_hi, &fresh.nz_lo]
        .iter()
        .map(|t| t.cdf.iter().map(|c| c.0).collect())
        .chain(std::iter::once(Vec::new()))
        .chain([&fresh.mag, &fresh.nze].iter().map(|t| t.cdf.iter().map(|c| c.0).collect()))
        .chain(std::iter::once(Vec::new()))
        .chain([&fresh.emag].iter().map(|t| t.cdf.iter().map(|c| c.0).collect()))
        .chain(std::iter::once(Vec::new()))
        .chain([&fresh.esc, &fresh.dc].iter().map(|t| t.cdf.iter().map(|c| c.0).collect()))
        .chain(std::iter::once(Vec::new()))
        .chain(std::iter::once(Vec::new()))
        .chain(std::iter::once(Vec::new()))
        .chain(std::iter::once(Vec::new()))
        .chain(std::iter::once(Vec::new()))
        .chain([&fresh.emag2, &fresh.emag3, &fresh.mag2, &fresh.mag3, &fresh.dc2, &fresh.nz_hi2, &fresh.nz_lo2, &fresh.nze2].iter().map(|t| t.cdf.iter().map(|c| c.0).collect()))
        .chain(std::iter::once(Vec::new()))
        .chain(std::iter::once(Vec::new()))
        .collect();
    let mut out = Vec::new();
    for ((n, counts), def) in totals.iter().zip(&defaults) {
        let n = *n;
        for (ctx, c) in counts.chunks(n).enumerate() {
            let total: u64 = c.iter().sum();
            if n == 2 {
                let q = if total >= min { (((c[0] as f64 + 0.4) / (total as f64 + 0.8)) * PROB_ONE as f64).round() as u32 } else { PROB_ONE / 2 };
                out.extend_from_slice(&(q.clamp(1, PROB_ONE - 1) as u16).to_le_bytes());
                out.push(if total >= min { bit_count0 } else { 0 });
            } else {
                let cdf = if total >= min {
                    let scale = (1u64 << 20) as f64 / total as f64;
                    let w: Vec<u32> = c.iter().map(|&x| (x as f64 * scale).round() as u32).collect();
                    Cdf::new(n, &w).0
                } else {
                    def[ctx]
                };
                for j in 1..n {
                    out.extend_from_slice(&cdf[j].to_le_bytes());
                }
                out.push(if total >= min { count0 } else { 0 });
            }
        }
    }
    assert_eq!(lsw.len(), NLS);
    for w in lsw {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Codes bytes with the mean of an order-1 and an order-0 binary model (each byte as 8
/// decisions in the context of the previous byte, and without it); a leading 0 marks bytes
/// stored as they are when that is not larger.
pub fn encode_bytes(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return vec![0];
    }
    let mut e = Enc::<Base>::new();
    let (mut t, mut t0) = (Bits::new(256 * 256), Bits::new(256));
    let mut prev = 0usize;
    for &b in data {
        e.block();
        let mut node = 1usize;
        for i in (0..8).rev() {
            let bit = ((b >> i) & 1) as u32;
            t.bit2(&mut t0, &mut e, prev * 256 + node, node, bit);
            node = node * 2 + bit as usize;
        }
        prev = b as usize;
    }
    let (r, _) = e.finish();
    let mut out = Vec::with_capacity(r.len().min(data.len()) + 1);
    if r.len() < data.len() {
        out.push(1);
        out.extend_from_slice(&r);
    } else {
        out.push(0);
        out.extend_from_slice(data);
    }
    out
}

/// Inverse of `encode_bytes` for `n` bytes.
pub fn decode_bytes(c: &[u8], n: usize) -> Result<Vec<u8>, String> {
    match c.first() {
        Some(0) if c.len() == n + 1 => Ok(c[1..].to_vec()),
        Some(1) => {
            let mut d = Dec::<Base>::new(&c[1..], &[]);
            let (mut t, mut t0) = (Bits::new(256 * 256), Bits::new(256));
            let mut out = Vec::with_capacity(n);
            let mut prev = 0usize;
            for i in 0..n {
                d.block();
                let mut node = 1usize;
                for _ in 0..8 {
                    node = node * 2 + t.bit2(&mut t0, &mut d, prev * 256 + node, node, 0) as usize;
                }
                let b = (node - 256) as u8;
                out.push(b);
                prev = b as usize;
                if i % 4096 == 0 && d.overrun() {
                    return Err("corrupt .komla file".into());
                }
            }
            if d.overrun() {
                return Err("corrupt .komla file".into());
            }
            Ok(out)
        }
        _ => Err("corrupt .komla file".into()),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn luma_down_lanes() {
        let mut r = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = move || {
            r ^= r << 13;
            r ^= r >> 7;
            r ^= r << 17;
            r
        };
        for n in 0..4000 {
            let mut blocks = [[0i16; 64]; 4];
            for x in blocks.iter_mut().flatten() {
                let v = next();
                *x = match n % 4 {
                    0 => v as i16,
                    1 => (v % 65) as i16 - 32,
                    2 => [i16::MIN, i16::MAX, 0, -1][(v % 4) as usize],
                    _ => (v as i16) >> 5,
                };
            }
            let ql: [i16; 64] = std::array::from_fn(|_| [16383, 1 + (next() % 255) as i16, (next() % 16384) as i16][n % 3]);
            let b = [&blocks[0], &blocks[1], &blocks[2], &blocks[3]];
            for (fx, fy) in [(1, 1), (1, 2), (2, 1), (2, 2)] {
                assert_eq!(super::simd::luma_down(b, fx, fy, &ql), super::luma_down_scalar(b, fx, fy, &ql), "{n} {fx}x{fy}");
            }
        }
    }
}
