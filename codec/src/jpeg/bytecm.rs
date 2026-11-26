//! General-purpose context-mixing coder for bytes: JPEG marker segments (EXIF, ICC profiles,
//! XMP, ...), data after the end of the image, and files that are stored verbatim. Orders 0-6
//! and a word context are hashed into nibble-oriented counter tables, a match model predicts
//! from the longest earlier repetition, and a gated mixer plus an APM combine them.

use crate::coder::{BitCoder, Decoder, Encoder};
use crate::primitives::*;

const NORD: usize = 8;
const NIN: usize = NORD + 2;
const MATCH_MIN: usize = 6;

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
    mt: Vec<u32>,
    mptr: usize,
    mlen: usize,
    word: u32,
}

impl Model {
    fn new(n: usize) -> Self {
        let t = tables();
        let cap = (usize::BITS - n.max(1).leading_zeros() + 1).clamp(10, 22);
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
            mt: vec![0; 1 << (cap - 2).min(20)],
            mptr: 0,
            mlen: 0,
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
        // match model: hash of the last MATCH_MIN bytes -> last position
        if self.mlen > 0 && self.mptr < n {
            self.mptr += 1;
            self.mlen += 1;
        } else {
            self.mlen = 0;
        }
        if n >= MATCH_MIN {
            let mut hv = 0u32;
            for j in 1..=MATCH_MIN {
                hv = hv.wrapping_mul(0x2F0F_3D27).wrapping_add(b(j) + 1);
            }
            let slot = (hv >> 8) as usize & (self.mt.len() - 1);
            if self.mlen == 0 {
                let cand = self.mt[slot] as usize;
                if cand > 0 && cand < n {
                    let mut l = 0;
                    while l < 32 && l < cand && h[cand - 1 - l] == h[n - 1 - l] {
                        l += 1;
                    }
                    if l >= MATCH_MIN {
                        self.mptr = cand;
                        self.mlen = l;
                    }
                }
            }
            self.mt[slot] = n as u32;
        }
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
        let expected = if self.mlen > 0 && self.mptr < self.hist.len() { Some(self.hist[self.mptr] as u32) } else { None };
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
            let mut mctx = 0usize;
            self.x[NORD] = match expected {
                Some(e) if (e + 256) >> (8 - i) == node => {
                    let bit = (e >> (7 - i)) & 1;
                    let conf = (self.mlen.min(31) as i32) * 32 + 64;
                    mctx = 1 + self.mlen.min(15);
                    if bit != 0 {
                        conf
                    } else {
                        -conf
                    }
                }
                _ => 0,
            };
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
                self.mlen = 0;
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

pub fn encode(data: &[u8]) -> Vec<u8> {
    let mut e = Encoder::new();
    let mut m = Model::new(data.len());
    for &b in data {
        m.code_byte(&mut e, b as u32);
    }
    e.finish()
}

pub fn decode(data: &[u8], n: usize) -> Vec<u8> {
    let mut d = Decoder::new(data);
    let mut m = Model::new(n);
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(m.code_byte(&mut d, 0));
    }
    out
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
        assert_eq!(super::decode(&c, v.len()), v);
        assert_eq!(super::decode(&super::encode(&[]), 0), Vec::<u8>::new());
    }
}
