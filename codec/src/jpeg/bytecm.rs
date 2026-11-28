//! General-purpose context-mixing coder for bytes: JPEG marker segments (EXIF, ICC profiles,
//! XMP, ...), data after the end of the image, and files that are stored verbatim. Orders 0-6
//! and a word context are hashed into nibble-oriented counter tables, a match model predicts
//! from the longest earlier repetition, and a gated mixer plus an APM combine them.
//! `encode_lite` is a faster variant for the fast mode's header bytes.

use crate::coder::{BitCoder, Decoder, Encoder};
use crate::primitives::*;

const NORD: usize = 8;
const NIN: usize = NORD + 2;
const MATCH_MIN: usize = 6;

/// Match model: the most recent earlier occurrence of the last MATCH_MIN bytes, followed for as
/// long as the bytes keep repeating.
struct Match {
    mt: Vec<u32>,
    ptr: usize,
    len: usize,
}

impl Match {
    fn new(log: u32) -> Self {
        Match { mt: vec![0; 1 << log], ptr: 0, len: 0 }
    }

    /// Moves on to the next byte of `h` (one byte longer than at the last call).
    fn next(&mut self, h: &[u8]) {
        let n = h.len();
        if self.len > 0 && self.ptr < n {
            self.ptr += 1;
            self.len += 1;
        } else {
            self.len = 0;
        }
        if n >= MATCH_MIN {
            let mut hv = 0u32;
            for j in 1..=MATCH_MIN {
                hv = hv.wrapping_mul(0x2F0F_3D27).wrapping_add(h[n - j] as u32 + 1);
            }
            let slot = (hv >> 8) as usize & (self.mt.len() - 1);
            if self.len == 0 {
                let cand = self.mt[slot] as usize;
                if cand > 0 && cand < n {
                    let mut l = 0;
                    while l < 32 && l < cand && h[cand - 1 - l] == h[n - 1 - l] {
                        l += 1;
                    }
                    if l >= MATCH_MIN {
                        self.ptr = cand;
                        self.len = l;
                    }
                }
            }
            self.mt[slot] = n as u32;
        }
    }

    /// The predicted next byte of `h`.
    fn expected(&self, h: &[u8]) -> Option<u32> {
        (self.len > 0 && self.ptr < h.len()).then(|| h[self.ptr] as u32)
    }

    /// Mixer input for bit `i` of a byte whose bits so far are `node` (with a leading 1), and
    /// the match length bucket (0 without a prediction).
    #[inline(always)]
    fn input(&self, expected: Option<u32>, i: usize, node: u32) -> (i32, usize) {
        match expected {
            Some(e) if (e + 256) >> (8 - i) == node => {
                let conf = (self.len.min(31) as i32) * 32 + 64;
                (if (e >> (7 - i)) & 1 != 0 { conf } else { -conf }, 1 + self.len.min(15))
            }
            _ => (0, 0),
        }
    }
}

struct Model {
    t: &'static Tables,
    tabs: Vec<CtxTable>,
    hashes: [u32; NORD],
    mix: Mixer,
    mix2: Mixer,
    mf: Mixer,
    apm: Apm,
    apm2: Apm,
    x: [i32; NIN],
    hist: Vec<u8>,
    m: Match,
    word: u32,
}

impl Model {
    fn new(n: usize) -> Self {
        let t = tables();
        let cap = (usize::BITS - n.max(1).leading_zeros() + 1).clamp(10, 17);
        Model {
            t,
            tabs: (0..NORD).map(|i| CtxTable::new(if i == 0 { 8 } else { cap }, 4, 255)).collect(),
            hashes: [0; NORD],
            mix: Mixer::new(NIN, 256, 500, 1 << 14),
            mix2: Mixer::new(NIN, 17 * 8, 500, 1 << 14),
            mf: Mixer::new(3, 1, 300, 1 << 15),
            apm: Apm::new(256 * 256, 6, t),
            apm2: Apm::new(1 << 16, 6, t),
            x: [0; NIN],
            hist: Vec::with_capacity(n),
            m: Match::new((cap - 2).min(20)),
            word: 0,
        }
    }

    fn start_byte(&mut self) {
        let h = &self.hist;
        let n = h.len();
        let b = |i: usize| -> u32 { if n >= i { h[n - i] as u32 } else { 0 } };
        self.hashes[0] = 0;
        let mut v = 0u32;
        for ord in 1..=6usize {
            v = hash2(v.wrapping_add(ord as u32), b(ord));
            self.hashes[ord] = hash2(v, ord as u32 * 0x9E37);
        }
        self.hashes[7] = hash2(self.word, 0x77);
        self.m.next(h);
    }

    fn set_nibble(&mut self, nib: u32) {
        for i in 0..NORD {
            let h = if i == 0 { nib } else { hash2(self.hashes[i], nib) };
            self.tabs[i].set(h);
        }
    }

    fn code_byte<C: BitCoder>(&mut self, coder: &mut C, byte: u32) -> u8 {
        self.start_byte();
        let t = self.t;
        let c1 = self.hist.last().copied().unwrap_or(0) as usize;
        let expected = self.m.expected(&self.hist);
        let mut node = 1u32;
        for i in 0..8 {
            if i == 0 || i == 4 {
                let nib = if i == 0 { 0 } else { 16 | (node & 15) };
                self.set_nibble(nib);
            }
            let slot = if i < 4 { node as usize } else { (1usize << (i - 4)) | (node as usize & ((1 << (i - 4)) - 1)) };
            for k in 0..NORD {
                self.x[k] = t.stretch(self.tabs[k].p(slot));
            }
            let (xm, mctx) = self.m.input(expected, i, node);
            self.x[NORD] = xm;
            self.x[NORD + 1] = 256;
            let d1 = self.mix.mix(&self.x, node as usize, t);
            let d2 = self.mix2.mix(&self.x, mctx * 8 + i, t);
            let y = [d1, d2, 256];
            let d = self.mf.mix(&y, 0, t);
            let pa = self.apm.pp(d, c1 * 256 + node as usize);
            let pb = self.apm2.pp(d, (self.hashes[2] as usize & 0xFF) * 256 + node as usize);
            let p = clamp_p((2 * t.squash(d) + pa + pb) >> 2);
            let bit = coder.code((byte >> (7 - i)) & 1, p);
            for k in 0..NORD {
                self.tabs[k].update(slot, bit, &t.recip);
            }
            self.mix.update(&self.x, bit);
            self.mix2.update(&self.x, bit);
            self.mf.update(&y, bit);
            self.apm.update(bit);
            self.apm2.update(bit);
            node = (node << 1) | bit;
        }
        let b = (node & 255) as u8;
        if let Some(e) = expected {
            if e != b as u32 {
                self.m.len = 0;
            }
        }
        if b.is_ascii_alphanumeric() {
            self.word = hash2(self.word, b.to_ascii_lowercase() as u32);
        } else {
            self.word = 0;
        }
        self.hist.push(b);
        b
    }
}

/// Orders of the light model's hashed contexts.
const LITE_ORDERS: [usize; 5] = [0, 1, 2, 3, 4];
const NLITE: usize = LITE_ORDERS.len();

/// The light model: fewer orders in smaller tables (they stay in cache), one mixer selected by
/// the match length and the bit position, one APM on the bits of the byte so far.
struct Lite {
    t: &'static Tables,
    tabs: Vec<CtxTable>,
    hashes: [u32; NLITE],
    mix: Mixer,
    apm: Apm,
    x: [i32; NLITE + 2],
    hist: Vec<u8>,
    m: Match,
}

impl Lite {
    fn new(n: usize) -> Self {
        let t = tables();
        let cap = (usize::BITS - n.max(1).leading_zeros() + 1).clamp(10, 13);
        Lite {
            t,
            tabs: LITE_ORDERS.iter().map(|&o| CtxTable::new(if o == 0 { 5 } else { cap }, 4, 255)).collect(),
            hashes: [0; NLITE],
            mix: Mixer::new(NLITE + 2, 17 * 8, 500, 1 << 14),
            apm: Apm::new(256, 6, t),
            x: [0; NLITE + 2],
            hist: Vec::with_capacity(n),
            m: Match::new(cap - 2),
        }
    }

    fn code_byte<C: BitCoder>(&mut self, coder: &mut C, byte: u32) -> u8 {
        let t = self.t;
        let h = &self.hist;
        let n = h.len();
        let mut v = 0u32;
        let mut o = 0;
        for (k, &ord) in LITE_ORDERS.iter().enumerate() {
            while o < ord {
                o += 1;
                v = hash2(v.wrapping_add(o as u32), if n >= o { h[n - o] as u32 } else { 0 });
            }
            self.hashes[k] = if ord == 0 { 0 } else { hash2(v, ord as u32 * 0x9E37) };
        }
        self.m.next(h);
        let expected = self.m.expected(h);
        let mut node = 1u32;
        for i in 0..8 {
            if i == 0 || i == 4 {
                let nib = if i == 0 { 0 } else { 16 | (node & 15) };
                for k in 0..NLITE {
                    self.tabs[k].set(if k == 0 { nib } else { hash2(self.hashes[k], nib) });
                }
            }
            let slot = if i < 4 { node as usize } else { (1usize << (i - 4)) | (node as usize & ((1 << (i - 4)) - 1)) };
            for k in 0..NLITE {
                self.x[k] = t.stretch(self.tabs[k].p(slot));
            }
            let (xm, mctx) = self.m.input(expected, i, node);
            self.x[NLITE] = xm;
            self.x[NLITE + 1] = 256;
            let d = self.mix.mix(&self.x, mctx * 8 + i, t);
            let p = clamp_p((t.squash(d) + self.apm.pp(d, node as usize)) >> 1);
            let bit = coder.code((byte >> (7 - i)) & 1, p);
            for k in 0..NLITE {
                self.tabs[k].update(slot, bit, &t.recip);
            }
            self.mix.update(&self.x, bit);
            self.apm.update(bit);
            node = (node << 1) | bit;
        }
        let b = (node & 255) as u8;
        if expected.is_some_and(|e| e != b as u32) {
            self.m.len = 0;
        }
        self.hist.push(b);
        b
    }
}

pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    let mut m = Model::new(data.len());
    for &b in data {
        m.code_byte(&mut e, b as u32);
    }
    e.finish()
}

/// Decodes `n` bytes; fails if the input runs out long before that (a damaged length).
pub fn decode(data: &[u8], n: usize) -> Result<Vec<u8>, String> {
    let mut d = Decoder::new(data);
    let mut m = Model::new(n);
    let mut out = Vec::with_capacity(n.min(1 << 24));
    for i in 0..n {
        out.push(m.code_byte(&mut d, 0));
        if i % 4096 == 4095 && d.overrun() {
            return Err("the .komla file is damaged or incomplete".into());
        }
    }
    Ok(out)
}

/// `encode` with the light model.
pub fn encode_lite(data: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    let mut m = Lite::new(data.len());
    for &b in data {
        m.code_byte(&mut e, b as u32);
    }
    e.finish()
}

/// `decode` with the light model.
pub fn decode_lite(data: &[u8], n: usize) -> Result<Vec<u8>, String> {
    let mut d = Decoder::new(data);
    let mut m = Lite::new(n);
    let mut out = Vec::with_capacity(n.min(1 << 24));
    for i in 0..n {
        out.push(m.code_byte(&mut d, 0));
        if i % 4096 == 4095 && d.overrun() {
            return Err("the .komla file is damaged or incomplete".into());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let mut v: Vec<u8> = b"<x:xmpmeta xmlns:x='adobe:ns:meta/'><rdf:RDF>hello hello hello world</rdf:RDF></x:xmpmeta>".to_vec();
        let mut s = 7u32;
        for _ in 0..3000 {
            s = s.wrapping_mul(1_103_515_245).wrapping_add(12345);
            v.push((s >> 16) as u8 & 0x3F);
        }
        v.extend_from_slice(&v.clone()[100..900]);
        let c = super::encode(&v);
        assert!(c.len() < v.len());
        assert_eq!(super::decode(&c, v.len()).unwrap(), v);
        assert_eq!(super::decode(&super::encode(&[]), 0).unwrap(), Vec::<u8>::new());
        let c = super::encode_lite(&v);
        assert!(c.len() < v.len());
        assert_eq!(super::decode_lite(&c, v.len()).unwrap(), v);
        assert_eq!(super::decode_lite(&super::encode_lite(&[]), 0).unwrap(), Vec::<u8>::new());
    }
}
