//! Palette-index model for images with at most 256 distinct colours. Indices are coded
//! MSB-first; each bit is predicted from hashed contexts of neighbouring indices.

use crate::coder::BitCoder;
use crate::primitives::*;

const NT: usize = 12;
const NIN: usize = NT + 1;
const OOB: i32 = 256;

pub struct PalModel {
    t: &'static Tables,
    w: usize,
    nbits: u32,
    tabs: Vec<CtxTable>,
    h: [u32; NT],
    mixer: Mixer,
    mixer2: Mixer,
    apm1: Apm,
    apm2: Apm,
    x: [i32; NIN],
}

impl PalModel {
    pub fn new(w: usize, h: usize, palette_size: usize) -> Self {
        let t = tables();
        let px = (w * h) as u64;
        let lb = (64 - px.leading_zeros()).clamp(10, 20);
        let mut nbits = 1;
        while (1usize << nbits) < palette_size {
            nbits += 1;
        }
        PalModel {
            t,
            w,
            nbits,
            tabs: (0..NT).map(|i| CtxTable::new(if i == 10 { 8 } else { lb }, 4, 255)).collect(),
            h: [0; NT],
            mixer: Mixer::new(NIN, 16 * 64, 700, 1 << 14),
            mixer2: Mixer::new(NIN, 16 * 256, 700, 1 << 14),
            apm1: Apm::new(256 * 64, 6, t),
            apm2: Apm::new(256 * 256, 6, t),
            x: [0; NIN],
        }
    }

    /// Codes palette index `value` at (x, y); `idx` holds already decoded indices
    /// (row-major, u64 keys), `m` the match model prediction if any, `eqp`/`hits` pixel-model context.
    pub fn code<C: BitCoder>(&mut self, coder: &mut C, idx: &[u64], x: usize, y: usize, value: u32, eqp: u32) -> u32 {
        let w = self.w;
        let g = |dx: isize, dy: isize| -> i32 {
            let xx = x as isize + dx;
            let yy = y as isize + dy;
            if xx < 0 || yy < 0 || xx >= w as isize {
                OOB
            } else {
                idx[yy as usize * w + xx as usize] as i32
            }
        };
        let (pw, pn, pnw, pne, pww, pnn, pnne, pnnw, pnww) = (g(-1, 0), g(0, -1), g(-1, -1), g(1, -1), g(-2, 0), g(0, -2), g(1, -2), g(-1, -2), g(-2, -1));
        self.h = [
            hash_slice(&[1, pw]),
            hash_slice(&[2, pn]),
            hash_slice(&[3, pw, pn]),
            hash_slice(&[4, pw, pn, pnw]),
            hash_slice(&[5, pw, pn, pne]),
            hash_slice(&[6, pw, pn, pnw, pne]),
            hash_slice(&[7, pw, pww, pn, pnn, pne, pnw]),
            hash_slice(&[8, pn, pne, pnne, pnn]),
            hash_slice(&[9, pw, pnw, pnww, pww]),
            hash_slice(&[10, pw, pn, pne, pnw, pnn, pnne, pnnw, pww]),
            hash_slice(&[11, eqp as i32]),
            hash_slice(&[12, pne, pnne, pw]),
        ];
        let nb = self.nbits;
        let hi_bits = nb.min(4);
        let mut c0: u32 = 1;
        let mut v: u32 = 0;
        for (tb, &hh) in self.tabs.iter_mut().zip(self.h.iter()) {
            tb.set(hh);
        }
        let mut sel2 = (pw as usize).min(255);
        for b in 0..nb {
            if b == hi_bits {
                let hv = v;
                for (i, tb) in self.tabs.iter_mut().enumerate() {
                    tb.set(hash2(self.h[i], hv + 1));
                }
                c0 = 1;
                sel2 = (pn as usize).min(255);
            }
            let shift = nb - 1 - b;
            let bit_enc = (value >> shift) & 1;
            let slot = c0 as usize;
            let t = self.t;
            for (i, tb) in self.tabs.iter().enumerate() {
                self.x[i] = t.stretch(tb.p(slot));
            }
            self.x[NT] = 256;
            let node = ((b as usize) << 4 | slot) & 255;
            let d1 = self.mixer.mix(&self.x, (b as usize) * 64 + (eqp as usize & 63), t);
            let d2 = self.mixer2.mix(&self.x, (slot & 15) * 256 + sel2, t);
            let d = (d1 + d2) / 2;
            let pa1 = self.apm1.pp(d, node * 64 + (eqp as usize & 63));
            let pa2 = self.apm2.pp(d, node * 256 + (pw as usize).min(255));
            let p = clamp_p((2 * t.squash(d) + pa1 + pa2) >> 2);
            let bit = coder.code(bit_enc, p);
            let rc = &t.recip;
            for tb in self.tabs.iter_mut() {
                tb.update(slot, bit, rc);
            }
            let xx = self.x;
            self.mixer.update(&xx, bit);
            self.mixer2.update(&xx, bit);
            self.apm1.update(bit);
            self.apm2.update(bit);
            c0 = (c0 << 1) | bit;
            v = (v << 1) | bit;
        }
        v
    }
}
