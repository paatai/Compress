//! Modelling primitives: logistic tables, adaptive counters, mixers and APMs.
//! All arithmetic that influences coding is integer or deterministic floating point.

use crate::detmath;
use std::sync::OnceLock;

pub struct Tables {
    squash: Vec<u16>,
    stretch: Vec<i16>,
    pub recip: [u32; 1024],
}

static TABLES: OnceLock<Tables> = OnceLock::new();

pub fn tables() -> &'static Tables {
    TABLES.get_or_init(|| {
        let mut squash = vec![0u16; 4096];
        for (i, s) in squash.iter_mut().enumerate() {
            let d = (i as f64 - 2048.0) / 256.0;
            let p = 65536.0 / (1.0 + detmath::exp(-d));
            *s = (p.round() as i64).clamp(1, 65535) as u16;
        }
        let mut stretch = vec![0i16; 4096];
        for (i, s) in stretch.iter_mut().enumerate() {
            let p = (i as f64 + 0.5) / 4096.0;
            let d = 256.0 * detmath::ln(p / (1.0 - p));
            *s = (d.round() as i64).clamp(-2047, 2047) as i16;
        }
        let mut recip = [0u32; 1024];
        for (i, r) in recip.iter_mut().enumerate() {
            *r = (65536.0 * 2.0 / (2.0 * i as f64 + 3.0)) as u32;
        }
        Tables { squash, stretch, recip }
    })
}

impl Tables {
    /// Logistic function: stretch domain (logit * 256, clamped to +-2047) -> 16-bit probability.
    #[inline(always)]
    pub fn squash(&self, d: i32) -> u32 {
        let i = (d.clamp(-2047, 2047) + 2048) as usize;
        unsafe { *self.squash.get_unchecked(i) as u32 }
    }
    /// Inverse of squash for a 16-bit probability.
    #[inline(always)]
    pub fn stretch(&self, p16: u32) -> i32 {
        unsafe { *self.stretch.get_unchecked((p16 >> 4) as usize & 4095) as i32 }
    }
}

/// Hint the CPU to fetch the cache line containing `p` (no-op where unsupported).
#[inline(always)]
pub fn prefetch<T>(p: *const T) {
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_mm_prefetch(p as *const i8, core::arch::x86_64::_MM_HINT_T0);
    }
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, readonly, preserves_flags));
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    let _ = p;
}

#[inline(always)]
pub fn hash2(a: u32, b: u32) -> u32 {
    let h = (a as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (b as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93);
    let h = h ^ (h >> 29);
    (h.wrapping_mul(0xBF58_476D_1CE4_E5B9) >> 32) as u32
}

#[inline(always)]
pub fn hash_slice(vals: &[i32]) -> u32 {
    let mut h: u64 = 0x243F_6A88_85A3_08D3;
    for &v in vals {
        h = (h ^ (v as u32 as u64)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        h ^= h >> 31;
    }
    (h.wrapping_mul(0xD6E8_FEB8_6659_FD93) >> 32) as u32
}

/// Probability counter packed in a u32: 22-bit probability, 10-bit hit count.
#[inline(always)]
pub fn ctr_p16(e: u32) -> u32 {
    e >> 16
}

pub const CTR_INIT: u32 = (1 << 31) | 0;

#[inline(always)]
pub fn ctr_update(e: &mut u32, bit: u32, limit: u32, recip: &[u32; 1024]) {
    let n = *e & 1023;
    let p = (*e >> 10) as i64;
    let target = (bit as i64) << 22;
    let r = recip[n as usize] as i64;
    let np = p + (((target - p) * r) >> 16);
    let nn = if n < limit { n + 1 } else { n };
    *e = ((np as u32) << 10) | nn;
}

/// Table of counter blocks. A context selects a block of `1 << block_bits` counters;
/// the binarisation node selects the counter inside the block, so all decisions of one
/// sample touch the same one or two cache lines.
pub struct CtxTable {
    t: Vec<u32>,
    mask: usize,
    block_bits: u32,
    base: usize,
    pub limit: u32,
}

impl CtxTable {
    pub fn new(log_blocks: u32, block_bits: u32, limit: u32) -> Self {
        let n = 1usize << (log_blocks + block_bits);
        CtxTable { t: vec![CTR_INIT; n], mask: (1usize << log_blocks) - 1, block_bits, base: 0, limit }
    }
    #[inline(always)]
    pub fn set(&mut self, h: u32) {
        self.base = ((h as usize) & self.mask) << self.block_bits;
        prefetch(unsafe { self.t.as_ptr().add(self.base) });
    }
    #[inline(always)]
    pub fn p(&self, slot: usize) -> u32 {
        unsafe { ctr_p16(*self.t.get_unchecked(self.base + slot)) }
    }
    #[inline(always)]
    pub fn entry(&self, slot: usize) -> u32 {
        unsafe { *self.t.get_unchecked(self.base + slot) }
    }
    #[inline(always)]
    pub fn update(&mut self, slot: usize, bit: u32, recip: &[u32; 1024]) {
        let lim = self.limit;
        unsafe { ctr_update(self.t.get_unchecked_mut(self.base + slot), bit, lim, recip) }
    }
}

/// Counter table that also keeps a short bit history per slot: entries hold a 16-bit
/// probability, an 8-bit count, a "only one bit value seen so far" flag and the last bit.
/// An adaptive map learns, per table, how reliable such histories are; that estimate is a
/// second mixer input, which makes deterministic contexts trusted after very few hits.
pub struct HistTable {
    t: Vec<u32>,
    mask: usize,
    block_bits: u32,
    base: usize,
    sm: [u32; 64],
}

const HT_INIT: u32 = (32768 << 16) | (1 << 7);

impl HistTable {
    pub fn new(log_blocks: u32, block_bits: u32) -> Self {
        HistTable {
            t: vec![HT_INIT; 1usize << (log_blocks + block_bits)],
            mask: (1usize << log_blocks) - 1,
            block_bits,
            base: 0,
            sm: [CTR_INIT; 64],
        }
    }
    #[inline(always)]
    pub fn set(&mut self, h: u32) {
        self.base = ((h as usize) & self.mask) << self.block_bits;
        prefetch(unsafe { self.t.as_ptr().add(self.base) });
    }
    #[inline(always)]
    fn state_of(e: u32) -> usize {
        let n = (e >> 8) & 255;
        if n == 0 {
            0
        } else if e & 0x80 != 0 {
            1 + ((e >> 6) & 1) as usize + 2 * (n.min(16) - 1) as usize
        } else {
            33 + (31 - n.leading_zeros()) as usize
        }
    }
    /// Returns (direct, history) stretched probabilities for `slot`.
    #[inline(always)]
    pub fn inputs(&self, slot: usize, t: &Tables) -> (i32, i32) {
        let e = unsafe { *self.t.get_unchecked(self.base + slot) };
        let s = Self::state_of(e);
        (t.stretch(e >> 16), if s == 0 { 0 } else { t.stretch(ctr_p16(self.sm[s])) })
    }
    #[inline(always)]
    pub fn update(&mut self, slot: usize, bit: u32, recip: &[u32; 1024]) {
        let e = unsafe { self.t.get_unchecked_mut(self.base + slot) };
        let s = Self::state_of(*e);
        if s != 0 {
            ctr_update(&mut self.sm[s], bit, 1023, recip);
        }
        let n = (*e >> 8) & 255;
        let p = (*e >> 16) as i32;
        let target = if bit != 0 { 65535 } else { 0 };
        let r = recip[n as usize] as i32;
        let np = (p as i64 + (((target - p) as i64 * r as i64) >> 16)).clamp(1, 65535) as u32;
        let det = if n == 0 || (((*e >> 6) & 1) == bit && *e & 0x80 != 0) { 0x80 } else { 0 };
        *e = (np << 16) | ((n + 1).min(255) << 8) | det | (bit << 6);
    }
}

/// Gated logistic mixer: one weight vector per selector value. Integer arithmetic only
/// (wrapping i32 products are exact and identical on every target).
pub struct Mixer {
    n: usize,
    w: Vec<i32>,
    base: usize,
    pub pr: u32,
    pub dot: i32,
    lr: i32,
    boost: i64,
    decay: i64,
    updates: i64,
}

/// Weights are clamped to +-4.0 so that `(w * x) >> 4` (|x| <= 2047) summed over up to 64
/// inputs fits in an i32; this keeps the dot product in 32-bit SIMD lanes everywhere.
const W_MAX: i32 = (1 << 18) - 1;

impl Mixer {
    pub fn new(n: usize, nsets: usize, lr: i32, init_w: i32) -> Self {
        Mixer { n, w: vec![init_w; n * nsets], base: 0, pr: 32768, dot: 0, lr, boost: 0, decay: 1, updates: 0 }
    }
    /// Learning rate starts at (1 + boost) times the base rate and decays towards it with
    /// time constant `decay` updates.
    pub fn with_schedule(mut self, boost: i64, decay: i64) -> Self {
        self.boost = boost;
        self.decay = decay.max(1);
        self
    }
    #[inline(always)]
    pub fn mix(&mut self, x: &[i32], set: usize, t: &Tables) -> i32 {
        self.base = set * self.n;
        let n = self.n;
        let w = &self.w[self.base..self.base + n];
        let x = &x[..n];
        let mut s: i32 = 0;
        for i in 0..n {
            s += (w[i] * x[i]) >> 4;
        }
        let d = (s >> 12).clamp(-2047, 2047);
        self.dot = d;
        self.pr = t.squash(d);
        d
    }
    #[inline(always)]
    pub fn update(&mut self, x: &[i32], bit: u32) {
        let err = ((bit as i32) << 16) - self.pr as i32;
        let lr = if self.boost > 0 {
            self.updates += 1;
            (self.lr as i64 + self.lr as i64 * self.boost * self.decay / (self.decay + self.updates)) as i32
        } else {
            self.lr
        };
        let e = (err * lr) >> 14;
        let n = self.n;
        let w = &mut self.w[self.base..self.base + n];
        let x = &x[..n];
        for i in 0..n {
            w[i] = (w[i] + ((x[i] * e) >> 10)).clamp(-W_MAX, W_MAX);
        }
    }
    pub fn set_lr(&mut self, lr: i32) {
        self.lr = lr;
    }
}

/// Adaptive probability map (secondary estimation): refines a probability given a context,
/// interpolating between 33 buckets in the stretch domain.
pub struct Apm {
    t: Vec<u16>,
    idx: usize,
    wt: u32,
    rate: u32,
}

impl Apm {
    pub fn new(n: usize, rate: u32, tb: &Tables) -> Self {
        let mut t = vec![0u16; n * 33];
        for i in 0..n {
            for j in 0..33 {
                t[i * 33 + j] = tb.squash((j as i32 - 16) * 128).min(65535) as u16;
            }
        }
        Apm { t, idx: 0, wt: 0, rate }
    }
    #[inline(always)]
    pub fn pp(&mut self, d: i32, ctx: usize) -> u32 {
        let dd = (d.clamp(-2047, 2047) + 2048) as u32;
        let lo = (dd >> 7) as usize;
        let w = dd & 127;
        let i = ctx * 33 + lo;
        self.idx = i;
        self.wt = w;
        unsafe { (*self.t.get_unchecked(i) as u32 * (128 - w) + *self.t.get_unchecked(i + 1) as u32 * w) >> 7 }
    }
    #[inline(always)]
    pub fn update(&mut self, bit: u32) {
        let target: i32 = if bit != 0 { 65535 } else { 0 };
        let i = self.idx;
        let w = self.wt as i32;
        unsafe {
            let a = self.t.get_unchecked_mut(i);
            *a = (*a as i32 + ((((target - *a as i32) * (128 - w)) >> 7) >> self.rate)) as u16;
            let b = self.t.get_unchecked_mut(i + 1);
            *b = (*b as i32 + ((((target - *b as i32) * w) >> 7) >> self.rate)) as u16;
        }
    }
}

#[inline(always)]
pub fn clamp_p(p: u32) -> u32 {
    p.clamp(16, 65520)
}
