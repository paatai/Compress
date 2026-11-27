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

const LIMIT: u8 = 63;

/// Update shift after n updates: log2(n + 2) + 1, between 3 and 7.
static SHIFT: [u32; 64] = {
    let mut r = [0u32; 64];
    let mut i = 0;
    while i < 64 {
        let mut s = 0u32;
        while (1usize << (s + 1)) <= i + 2 {
            s += 1;
        }
        s += 1;
        r[i] = if s < 3 {
            3
        } else if s > 7 {
            7
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
        L::update(&mut self.0, s + 1, &TARGETS.lo[n & 15], &TARGETS.hi[n & 15], SHIFT[(*cnt & 63) as usize]);
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
const BIT_SHIFT: u32 = 6;

/// Moves P(0) towards the coded bit by 2^-BIT_SHIFT of the distance (never past the bounds).
#[inline(always)]
fn adapt_bit(p: Bit, b: u32) -> Bit {
    let q = p as i32;
    let t = if b != 0 { 1 } else { PROB_ONE as i32 - 1 };
    (q + ((t - q) >> BIT_SHIFT)) as Bit
}

/// Binary probabilities.
#[derive(Clone)]
pub struct Bits {
    p: Vec<Bit>,
    /// counts of zeros and ones per probability (training only)
    stats: Vec<u32>,
}

impl Bits {
    fn new(len: usize) -> Bits {
        Bits { p: vec![BIT_INIT; len], stats: if training() { vec![0; len * 2] } else { Vec::new() } }
    }
    /// Codes bit `b` with probability `i` (an index bounded by construction).
    #[inline(always)]
    fn bit<I: Io>(&mut self, io: &mut I, i: usize, b: u32) -> u32 {
        debug_assert!(i < self.p.len());
        if !I::DEC && !self.stats.is_empty() {
            self.stats[i * 2 + b as usize] += 1;
        }
        // SAFETY: every index is computed from components that are each masked or clamped to
        // the table's dimensions (checked by the debug assertion).
        io.bit(unsafe { self.p.get_unchecked_mut(i) }, b)
    }
    fn load(&mut self, p: &mut &[u8]) -> Option<()> {
        for x in self.p.iter_mut() {
            let (b, rest) = p.split_at_checked(2)?;
            *x = u16::from_le_bytes([b[0], b[1]]).clamp(1, PROB_ONE as u16 - 1);
            *p = rest;
        }
        Some(())
    }
}

/// The vector operations on a distribution: `find` counts the cumulative frequencies at or
/// below a slot, `update` moves lanes from `j` on towards `hi` and the others towards `lo`.
/// Every implementation computes the same integers.
pub trait Lanes {
    fn find(c: &[u16; 16], slot: u16) -> usize;
    fn update(c: &mut [u16; 16], j: usize, lo: &[u16; 16], hi: &[u16; 16], shift: u32);
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
}

/// Development aid: accumulate the cost of each category of symbols (encoder only).
const STATS: bool = false;
pub static mut COST: [f64; 8] = [0.0; 8];
pub static mut CAT: usize = 0;
#[inline(always)]
fn cost(freq: u32) {
    if STATS {
        unsafe { COST[CAT] += (PROB_ONE as f64 / freq as f64).log2() };
    }
}
#[inline(always)]
fn cat(c: usize) {
    if STATS {
        unsafe { CAT = c };
    }
}

/// Source and sink of coded symbols: the encoder codes the symbols it is given, the decoder
/// ignores them and returns what it decodes.
pub trait Io {
    const DEC: bool;
    /// Symbol `s` of an alphabet of `n` symbols; `cnt` counts the distribution's updates.
    fn sym(&mut self, c: &mut Cdf, cnt: &mut u8, n: usize, s: usize) -> usize;
    /// Bit `b` with adaptive probability `p`.
    fn bit(&mut self, p: &mut Bit, b: u32) -> u32;
    /// `n` raw bits (1 to 24), most significant first.
    fn raw(&mut self, v: u32, n: u32) -> u32;
}

pub struct Enc<L: Lanes = Base> {
    lanes: std::marker::PhantomData<L>,
    ops: Vec<u32>,
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
        Enc { lanes: std::marker::PhantomData, ops: Vec::with_capacity(1 << 16), raw: Vec::with_capacity(1 << 16), racc: 0, rbits: 0 }
    }
    pub fn symbols(&self) -> usize {
        self.ops.len()
    }
    /// Returns (rANS-coded bytes, raw bits).
    pub fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
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
        for &s in &[x[1], x[0]] {
            words.push(s as u16);
            words.push((s >> 16) as u16);
        }
        let mut out = Vec::with_capacity(words.len() * 2);
        for &w in words.iter().rev() {
            out.extend_from_slice(&w.to_be_bytes());
        }
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
    const DEC: bool = false;
    #[inline(always)]
    fn sym(&mut self, c: &mut Cdf, cnt: &mut u8, n: usize, s: usize) -> usize {
        debug_assert!(s < n);
        let (start, freq) = c.interval(s);
        cost(freq);
        self.ops.push(start | ((freq - 1) << 16));
        c.update::<L>(cnt, s, n);
        s
    }
    #[inline(always)]
    fn bit(&mut self, p: &mut Bit, b: u32) -> u32 {
        let q = *p as u32;
        cost(if b != 0 { PROB_ONE - q } else { q });
        self.ops.push(if b != 0 { q | ((PROB_ONE - q - 1) << 16) } else { (q - 1) << 16 });
        *p = adapt_bit(*p, b);
        b
    }
    #[inline(always)]
    fn raw(&mut self, v: u32, n: u32) -> u32 {
        debug_assert!((1..=24).contains(&n) && v >> n == 0);
        if STATS {
            unsafe { COST[CAT] += n as f64 };
        }
        self.racc = (self.racc << n) | v as u64;
        self.rbits += n;
        if self.rbits >= 32 {
            self.rbits -= 32;
            self.raw.extend_from_slice(&((self.racc >> self.rbits) as u32).to_be_bytes());
        }
        v
    }
}

pub struct Dec<'a, L: Lanes = Base> {
    lanes: std::marker::PhantomData<L>,
    x0: u32,
    x1: u32,
    data: &'a [u8],
    pos: usize,
    rdata: &'a [u8],
    rpos: usize,
    racc: u64,
    rbits: u32,
}

impl<'a, L: Lanes> Dec<'a, L> {
    pub fn new(data: &'a [u8], raw: &'a [u8]) -> Self {
        let mut d = Dec { lanes: std::marker::PhantomData, x0: 0, x1: 0, data, pos: 0, rdata: raw, rpos: 0, racc: 0, rbits: 0 };
        d.x0 = (d.word() << 16) | d.word();
        d.x1 = (d.word() << 16) | d.word();
        d
    }
    #[inline(always)]
    fn word(&mut self) -> u32 {
        let w = match self.data.get(self.pos..self.pos + 2) {
            Some(s) => u16::from_be_bytes([s[0], s[1]]),
            None => 0,
        };
        self.pos += 2;
        w as u32
    }
    /// True if the decoder read well past the end of either stream, which valid data never does.
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len() + 8 || self.rpos > self.rdata.len() + 8
    }
}

impl<L: Lanes> Io for Dec<'_, L> {
    const DEC: bool = true;
    #[inline(always)]
    fn sym(&mut self, c: &mut Cdf, cnt: &mut u8, n: usize, _s: usize) -> usize {
        let x = self.x0;
        let slot = x & (PROB_ONE - 1);
        let s = c.find::<L>(slot);
        let (start, freq) = c.interval(s);
        let mut x = freq * (x >> PROB_BITS) + slot - start;
        if x < RANS_L {
            x = (x << 16) | self.word();
        }
        self.x0 = self.x1;
        self.x1 = x;
        c.update::<L>(cnt, s, n);
        s
    }
    #[inline(always)]
    fn bit(&mut self, p: &mut Bit, _b: u32) -> u32 {
        let x = self.x0;
        let slot = x & (PROB_ONE - 1);
        let q = *p as u32;
        let b = (slot >= q) as u32;
        let (start, freq) = if b != 0 { (q, PROB_ONE - q) } else { (0, q) };
        let mut x = freq * (x >> PROB_BITS) + slot - start;
        if x < RANS_L {
            x = (x << 16) | self.word();
        }
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
}

// ---------------------------------------------------------------------------------------------
// tokens and contexts

/// Magnitude classes: token t >= 1 covers [BASE[t], BASE[t] + 2^LOW[t]); token 14 escapes to an
/// explicit bit length.
const NTOK: usize = 15;
const ESC: usize = 14;
const BASE: [u32; 14] = [0, 1, 2, 3, 4, 6, 8, 12, 16, 24, 32, 48, 64, 96];
const LOW: [u32; 14] = [0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5];

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
const NNB: usize = 8;
/// Bucket of the magnitude estimate of an interior coefficient (about 1.45 times wider each).
const EQ: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = match i {
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
/// Zone of an interior position (index into INTERIOR) for magnitudes, times NEQ.
const IZE: [u8; 49] = {
    let mut t = [0u8; 49];
    let mut i = 0;
    while i < 49 {
        t[i] = (IZ8[i] / NNB as u8) * NEQ as u8;
        i += 1;
    }
    t
};

const NKZ: usize = 8;

/// Coarse bucket of the number of nonzero coefficients still to code.
const REMQ: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut i = 0;
    while i < 64 {
        t[i] = match i {
            0..=2 => 0,
            3..=8 => 1,
            _ => 2,
        };
        i += 1;
    }
    t
};
const NREMQ: usize = 3;

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
/// Zone of an interior position (index into INTERIOR) for magnitudes, times NNB.
const IZ8: [u8; 49] = {
    let mut t = [0u8; 49];
    let mut i = 0;
    while i < 49 {
        let z = match i {
            0..=1 => 0,
            2..=4 => 1,
            5..=8 => 2,
            9..=13 => 3,
            14..=19 => 4,
            20..=26 => 5,
            27..=35 => 6,
            _ => 7,
        };
        t[i] = z * NNB as u8;
        i += 1;
    }
    t
};
/// Bucket of the magnitude of the interior coefficient next to an edge coefficient.
const AQ: [u8; 16] = [0, 1, 2, 3, 3, 4, 4, 4, 5, 5, 5, 5, 5, 5, 5, 5];
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
const NLUMA: usize = 4;
/// Bucket of the summed magnitudes of an edge coefficient in the neighbouring blocks.
const BEQ: [u8; 16] = [0, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3];
const NBE: usize = 4;
/// Zone of an edge position 1..7.
const EZ: [u8; 8] = [0, 0, 1, 2, 2, 3, 3, 3];
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
/// Edge prediction weights: each side's profile extrapolated to the boundary, (1 - g) times
/// the last sample plus g times the linear extrapolation from the last two (g = EDGE_G / 4).
const EDGE_G: i64 = 4;
/// The same for DC (g = DC_G / 4).
const DC_G: i64 = 4;
const DC_FAR: [i64; 8] = {
    let mut t = [0i64; 8];
    let mut j = 0;
    while j < 8 {
        t[j] = COS7[j] + DC_G * (COS7[j] - COS6[j]) / 8;
        j += 1;
    }
    t
};
const DC_NEAR: [i64; 8] = {
    let mut t = [0i64; 8];
    let mut j = 0;
    while j < 8 {
        t[j] = COS0[j] + DC_G * (COS0[j] - COS1[j]) / 8;
        j += 1;
    }
    t
};
const EDGE_FAR: [i64; 8] = {
    let mut t = [0i64; 8];
    let mut j = 0;
    while j < 8 {
        t[j] = COS7[j] + EDGE_G * (COS7[j] - COS6[j]) / 8;
        j += 1;
    }
    t
};
const EDGE_NEAR: [i64; 8] = {
    let mut t = [0i64; 8];
    let mut j = 0;
    while j < 8 {
        t[j] = COS0[j] + EDGE_G * (COS0[j] - COS1[j]) / 8;
        j += 1;
    }
    t
};

/// What a block's right and lower neighbours use: its nonzero counts.
#[derive(Clone, Copy, Default)]
pub struct Info {
    nz77: u8,
    nzr: u8,
    nzc: u8,
}

#[inline(always)]
fn div_round(n: i64, r: i64) -> i32 {
    ((((n >> 8).clamp(-(1 << 40), 1 << 40) * r) + (1 << 23)) >> 24).clamp(-32767, 32767) as i32
}

/// Bucket of a predicted magnitude.
#[inline(always)]
fn pbucket(p: i32) -> usize {
    bitlen(p.unsigned_abs()).min(7) as usize
}

/// Initial distributions learned from the training set (see `komla jtrain`); empty: flat.
static PRIORS: &[u8] = include_bytes!("fast_priors.bin");

/// Adaptive distributions of one stripe.
#[derive(Clone)]
pub struct Model {
    nz_hi: Table,
    nz_lo: Table,
    zero: Bits,
    mag: Table,
    nze: Table,
    ezero: Bits,
    emag: Table,
    esign: Bits,
    esc: Table,
    dc: Table,
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
            nz_hi: Table::new(2 * NNZB * NLUMA, 7, Cdf::new(7, &[8; 7])),
            nz_lo: Table::new(2 * NNZB * NLUMA * 7, 8, flat8),
            zero: Bits::new(2 * NREM * 49 * NEQ),
            mag: Table::new(2 * NREMQ * NKZ * NEQ, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            nze: Table::new(2 * 2 * 8 * 8, 8, Cdf::new(8, &[64, 32, 16, 8, 4, 2, 1, 1])),
            ezero: Bits::new(2 * 2 * 7 * 4 * 8 * NAQ * NBE),
            emag: Table::new(2 * 2 * 4 * 8 * NAQ * NBE, NTOK - 1, Cdf::new(NTOK - 1, &geo[1..])),
            esign: Bits::new(2 * 2 * 8 * 2),
            esc: Table::new(4, 8, flat8),
            dc: Table::new(2 * 8 * 4, NTOK, Cdf::new(NTOK, &geo)),
        };
        if !training() && !PRIORS.is_empty() {
            let mut p = PRIORS;
            let ok = m.nz_hi.load(&mut p).and_then(|_| m.nz_lo.load(&mut p)).and_then(|_| m.zero.load(&mut p)).and_then(|_| m.mag.load(&mut p)).and_then(|_| m.nze.load(&mut p)).and_then(|_| m.ezero.load(&mut p)).and_then(|_| m.emag.load(&mut p)).and_then(|_| m.esign.load(&mut p)).and_then(|_| m.esc.load(&mut p)).and_then(|_| m.dc.load(&mut p));
            assert!(ok.is_some() && p.is_empty(), "priors do not match the model");
        }
        m
    }

    /// Symbol statistics of every table, in the order of the priors file: for each table its
    /// alphabet size (2 for binary probabilities) and the counts per context and symbol.
    pub fn stats(&self) -> Vec<(usize, Vec<u32>)> {
        let t = |x: &Table| (x.n, x.stats.clone());
        let b = |x: &Bits| (2, x.stats.clone());
        vec![t(&self.nz_hi), t(&self.nz_lo), b(&self.zero), t(&self.mag), t(&self.nze), b(&self.ezero), t(&self.emag), b(&self.esign), t(&self.esc), t(&self.dc)]
    }

    /// Codes a magnitude class and the raw bits of a value (`t` = its token, `v` the value);
    /// `sign`: a probability for the sign bit (`Some((index, predicted negative))`) or raw.
    #[inline(always)]
    fn rest<I: Io>(&mut self, io: &mut I, t: usize, v: i32, esc_ctx: usize, sign: Option<(usize, bool)>) -> i32 {
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
            Some((ctx, pneg)) => {
                let flip = self.esign.bit(io, ctx, neg ^ pneg as u32);
                let m = if low > 0 { io.raw(a.wrapping_sub(base) & ((1 << low) - 1), low) } else { 0 };
                (flip ^ pneg as u32, base + m)
            }
            None => {
                let r = io.raw((neg << low) | (a.wrapping_sub(base) & ((1 << low) - 1)), low + 1);
                (r >> low, base + (r & ((1 << low) - 1)))
            }
        };
        if neg != 0 {
            -(m as i32)
        } else {
            m as i32
        }
    }

    /// Codes the first row (`dir` 0, continuing the block above `nbr`) or first column (`dir` 1,
    /// continuing the block to the left) of a block.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    fn edge<I: Io>(&mut self, io: &mut I, cls: usize, dir: usize, blk: &mut [i16; 64], nbr: Option<&[i16; 64]>, near: (&[i16; 64], &[i16; 64]), q: &[i64; 64], rq: &[i64; 64], nzq: usize, pnz: usize) -> u8 {
        let step = if dir == 0 { 1 } else { 8 };
        let across = if dir == 0 { 8 } else { 1 };
        let actual = if I::DEC { 0 } else { (1..8).filter(|&i| blk[i * step] != 0).count() };
        let n = self.nze.sym(io, ((cls * 2 + dir) * 8 + nzq) * 8 + pnz, 8, actual);
        let mut rem = n;
        let mut i = 1;
        while rem > 0 && i < 8 {
            let pos = i * step;
            // the inverse DCT of this frequency along the edge must continue the neighbour's
            // profile at its far side
            let p = match nbr {
                Some(nb) => {
                    let mut acc = nb[pos] as i64 * q[pos] * EDGE_FAR[0];
                    for j in 1..8 {
                        let o = pos + j * across;
                        acc += (nb[o] as i64 * EDGE_FAR[j] - blk[o] as i64 * EDGE_NEAR[j]) * q[o];
                    }
                    div_round(acc, rq[pos])
                }
                None => 0,
            };
            let pb = pbucket(p);
            let v = blk[pos] as i32;
            // the interior coefficient next to it (same frequency along the edge)
            let adj = AQ[((blk[pos + across] as i32).unsigned_abs()).min(15) as usize] as usize;
            // the same coefficient in the blocks above and to the left
            let nm = BEQ[((near.0[pos] as i32).unsigned_abs() + (near.1[pos] as i32).unsigned_abs()).min(15) as usize] as usize;
            let zctx = (((((cls * 2 + dir) * 7 + i - 1) * 4 + rem.min(4) - 1) * 8 + pb) * NAQ + adj) * NBE + nm;
            if self.ezero.bit(io, zctx, (v != 0) as u32) != 0 {
                let t = 1 + self.emag.sym(io, ((((cls * 2 + dir) * 4 + EZ[i] as usize) * 8 + pb) * NAQ + adj) * NBE + nm, NTOK - 1, if I::DEC { 0 } else { token(v.unsigned_abs()) - 1 });
                let sign = if p != 0 { Some((((cls * 2 + dir) * 8 + pb) * 2 + (p < 0) as usize, p < 0)) } else { None };
                let x = self.rest(io, t, v, cls * 2, sign);
                if I::DEC {
                    blk[pos] = x as i16;
                }
                rem -= 1;
            }
            i += 1;
        }
        n as u8
    }

    /// Codes one block. `a`, `l`: the blocks above and to the left with their `Info` (`None` if
    /// outside the stripe); `q`: the component's quantization table. Returns the block's `Info`.
    #[inline(always)]
    #[allow(clippy::too_many_arguments)]
    pub fn block<I: Io>(&mut self, io: &mut I, cls: usize, q: &[i64; 64], rq: &[i64; 64], blk: &mut [i16; 64], a: Option<(&[i16; 64], &Info)>, l: Option<(&[i16; 64], &Info)>, diag: (Option<&[i16; 64]>, Option<&[i16; 64]>), luma: usize, code_dc: bool, code_ac: bool) -> Info {
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
            let actual = if I::DEC { 0 } else { INTERIOR.iter().filter(|&&p| blk[p as usize] != 0).count() };
            let ctx = (cls * NNZB + NZB[avg(|i| i.nz77) & 63] as usize) * NLUMA + luma;
            cat(0);
            let hi = self.nz_hi.sym(io, ctx, 7, actual >> 3);
            let lo = self.nz_lo.sym(io, ctx * 7 + hi, 8, actual & 7);
            let nz77 = hi * 8 + lo;
            info.nz77 = nz77 as u8;
            let mut rem = nz77;
            let zbase = |rem: usize| (cls * NREM + REMB[rem & 63] as usize) * 49 * NEQ;
            let mbase = |rem: usize| (cls * NREMQ + REMQ[rem & 63] as usize) * NKZ * NEQ;
            let (mut zrow, mut mrow) = (zbase(rem), mbase(rem));
            let mut i = 0usize;
            while rem > 0 && i < 49 {
                let pos = INTERIOR[i] as usize & 63;
                let s = 2 * ((na[pos] as i32).unsigned_abs() + (nl[pos] as i32).unsigned_abs()) + (nal[pos] as i32).unsigned_abs() + (nar[pos] as i32).unsigned_abs();
                // the interior coefficients above and to the left in this block (coded before it;
                // first-row and first-column coefficients come later)
                let inner = if pos & 7 > 1 { (blk[pos - 1] as i32).unsigned_abs() } else { 0 } + if pos > 15 { (blk[pos - 8] as i32).unsigned_abs() } else { 0 };
                // a weighted sum of the magnitudes around the coefficient, inside and outside
                let nb = EQ[(s + 2 * inner).min(255) as usize] as usize;
                let v = blk[pos] as i32;
                cat(1);
                if self.zero.bit(io, zrow + i * NEQ + nb, (v != 0) as u32) != 0 {
                    cat(2);
                    let t = 1 + self.mag.sym(io, mrow + IZE[i] as usize + nb, NTOK - 1, if I::DEC { 0 } else { token(v.unsigned_abs()) - 1 });
                    cat(3);
                    let x = self.rest(io, t, v, cls * 2, None);
                    if I::DEC {
                        blk[pos] = x as i16;
                    }
                    rem -= 1;
                    zrow = zbase(rem);
                    mrow = mbase(rem);
                }
                i += 1;
            }
            let nzq = NZQ8[nz77 & 63] as usize;
            cat(6);
            info.nzr = self.edge(io, cls, 0, blk, a.map(|x| x.0), (na, nl), q, rq, nzq, avg(|i| i.nzr));
            info.nzc = self.edge(io, cls, 1, blk, l.map(|x| x.0), (na, nl), q, rq, nzq, avg(|i| i.nzc));
        }
        if code_dc {
            // the mean of the edge column (row) continues the neighbour's last column (row)
            let from_left = l.map(|(lb, _)| {
                let acc: i64 = lb[0] as i64 * q[0] * DC_FAR[0] + (1..8).map(|u| (lb[u] as i64 * DC_FAR[u] - blk[u] as i64 * DC_NEAR[u]) * q[u]).sum::<i64>();
                div_round(acc, rq[0])
            });
            let from_above = a.map(|(ab, _)| {
                let acc: i64 = ab[0] as i64 * q[0] * DC_FAR[0] + (1..8).map(|v| (ab[v * 8] as i64 * DC_FAR[v] - blk[v * 8] as i64 * DC_NEAR[v]) * q[v * 8]).sum::<i64>();
                div_round(acc, rq[0])
            });
            let (pred, ub) = match (from_left, from_above) {
                (Some(x), Some(y)) => ((x + y) >> 1, 1 + bitlen((x - y).unsigned_abs()).min(6) as usize),
                (Some(x), None) | (None, Some(x)) => (x, 7),
                _ => (0, 0),
            };
            let nz = (info.nz77 + info.nzr + info.nzc) as u32;
            let nzq = match nz {
                0 => 0,
                1..=3 => 1,
                4..=10 => 2,
                _ => 3,
            };
            let d = blk[0] as i32 - pred;
            cat(4);
            let t = self.dc.sym(io, cls * 32 + ub * 4 + nzq, NTOK, if I::DEC { 0 } else { token(d.unsigned_abs()) });
            cat(5);
            let x = if t != 0 { self.rest(io, t, d, cls * 2 + 1, None) } else { 0 };
            if I::DEC {
                blk[0] = (pred + x) as i16;
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

/// Codes the blocks of one stripe in MCU-row order. `planes[c]`: the stripe's block rows of
/// component c (`width[c]` blocks of 64 coefficients per row); `v[c]`: block rows per MCU row;
/// `q[c]`: quantization table (natural order).
#[allow(clippy::too_many_arguments)]
#[inline(always)]
pub fn code_stripe<I: Io>(io: &mut I, planes: &mut [&mut [i16]], width: &[usize], v: &[usize], ext: &[Extent], q: &[[u16; 64]], mcu_rows: usize) -> Model {
    let mut m = Model::new();
    let qs: Vec<[i64; 64]> = q.iter().map(|t| t.map(|x| x as i64)).collect();
    let rqs: Vec<[i64; 64]> = q.iter().map(|t| t.map(|x| (1i64 << 32) / (COS0[0] * x.max(1) as i64))).collect();
    let mut infos: Vec<Vec<Info>> = width.iter().map(|&w| vec![Info::default(); 2 * w]).collect();
    for my in 0..mcu_rows {
        for c in 0..planes.len() {
            let cls = (c > 0) as usize;
            let w = width[c];
            let e = ext[c];
            for r in 0..v[c] {
                let by = my * v[c] + r;
                let row_dc = e.dc && by < e.dc_h;
                let row_ac = e.ac && by < e.ac_h;
                if !row_dc && !row_ac {
                    continue;
                }
                let (before, cur) = planes[c].split_at_mut(by * w * 64);
                let above: Option<&[i16]> = if by > 0 { Some(&before[(by - 1) * w * 64..]) } else { None };
                // chroma: mean 7x7 count of the luma blocks at the same place (coded before)
                let mut hints = Vec::new();
                if c > 0 && v[0] % v[c] == 0 && width[0] % w == 0 {
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
                            LUMAQ[(sum / (fx * fy)).min(63)]
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
                    let info = m.block(io, cls, &qs[c], &rqs[c], blk, a, l, (al, ar), luma, cd, ca);
                    now[bx] = info;
                }
            }
        }
    }
    m
}

/// Geometry of a stripe for `encode_stripe` and `decode_stripe`.
pub struct Geometry<'a> {
    pub width: &'a [usize],
    pub v: &'a [usize],
    pub ext: &'a [Extent],
    pub q: &'a [[u16; 64]],
    pub mcu_rows: usize,
}

fn encode_with<L: Lanes>(planes: &mut [&mut [i16]], g: &Geometry) -> (Vec<u8>, Vec<u8>) {
    let mut e = Enc::<L>::new();
    code_stripe(&mut e, planes, g.width, g.v, g.ext, g.q, g.mcu_rows);
    e.finish()
}

fn decode_with<L: Lanes>(rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]], g: &Geometry) -> bool {
    let mut d = Dec::<L>::new(rans, raw);
    code_stripe(&mut d, planes, g.width, g.v, g.ext, g.q, g.mcu_rows);
    !d.overrun()
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt")]
unsafe fn encode_avx2(planes: &mut [&mut [i16]], g: &Geometry) -> (Vec<u8>, Vec<u8>) {
    encode_with::<Avx2>(planes, g)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt")]
unsafe fn decode_avx2(rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]], g: &Geometry) -> bool {
    decode_with::<Avx2>(rans, raw, planes, g)
}

#[cfg(target_arch = "x86_64")]
fn has_avx2() -> bool {
    std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("bmi1") && std::is_x86_feature_detected!("bmi2") && std::is_x86_feature_detected!("lzcnt") && std::is_x86_feature_detected!("popcnt")
}

/// Codes the coefficients of one stripe; returns (rANS bytes, raw bits).
pub fn encode_stripe(planes: &mut [&mut [i16]], g: &Geometry) -> (Vec<u8>, Vec<u8>) {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: the CPU has every feature the function is compiled for.
        return unsafe { encode_avx2(planes, g) };
    }
    encode_with::<Base>(planes, g)
}

/// Decodes the coefficients of one stripe into zeroed planes; false if the data ran out.
pub fn decode_stripe(rans: &[u8], raw: &[u8], planes: &mut [&mut [i16]], g: &Geometry) -> bool {
    #[cfg(target_arch = "x86_64")]
    if has_avx2() {
        // SAFETY: the CPU has every feature the function is compiled for.
        return unsafe { decode_avx2(rans, raw, planes, g) };
    }
    decode_with::<Base>(rans, raw, planes, g)
}

/// Builds the priors file from summed `Model::stats` of a training set of `files` files:
/// distributions with at least `min` observations start from the observed frequencies, with the
/// update count of `count0` (binary: `bit_count0`) so that they adapt from there.
pub fn build_priors(totals: &[(usize, Vec<u64>)], min: u64, count0: u8, bit_count0: u8) -> Vec<u8> {
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
        .collect();
    let mut out = Vec::new();
    for ((n, counts), def) in totals.iter().zip(&defaults) {
        let n = *n;
        for (ctx, c) in counts.chunks(n).enumerate() {
            let total: u64 = c.iter().sum();
            if n == 2 {
                let q = if total >= min { (((c[0] as f64 + 0.4) / (total as f64 + 0.8)) * PROB_ONE as f64).round() as u32 } else { PROB_ONE / 2 };
                out.extend_from_slice(&(q.clamp(1, PROB_ONE - 1) as u16).to_le_bytes());
                let _ = bit_count0;
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
    out
}

/// Codes bytes with an order-1 binary model (each byte as 8 decisions in the context of the
/// previous byte); a leading 0 marks bytes stored as they are when that is not larger.
pub fn encode_bytes(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return vec![0];
    }
    let mut e = Enc::<Base>::new();
    let mut t = Bits::new(256 * 256);
    let mut prev = 0usize;
    for &b in data {
        let mut node = 1usize;
        for i in (0..8).rev() {
            let bit = ((b >> i) & 1) as u32;
            t.bit(&mut e, prev * 256 + node, bit);
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
            let mut t = Bits::new(256 * 256);
            let mut out = Vec::with_capacity(n);
            let mut prev = 0usize;
            for i in 0..n {
                let mut node = 1usize;
                for _ in 0..8 {
                    node = node * 2 + t.bit(&mut d, prev * 256 + node, 0) as usize;
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
