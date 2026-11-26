//! Palette-index model for images with at most 256 distinct colours. Indices are coded
//! MSB-first; each bit is predicted from hashed contexts of neighbouring indices.

use crate::coder::BitCoder;
use crate::primitives::*;

const NT: usize = 26;
const NIN: usize = 2 * NT + 3;
const OOB: i32 = 65535;

pub struct PalModel {
    t: &'static Tables,
    w: usize,
    nbits: u32,
    tabs: Vec<HistTable>,
    h: [u32; NT],
    mixer: Mixer,
    mixer2: Mixer,
    mixer3: Mixer,
    mf: Mixer,
    apm1: Apm,
    apm2: Apm,
    apm3: Apm,
    mm: Vec<u32>,
    x: [i32; NIN],
}

impl PalModel {
    pub fn new(w: usize, h: usize, palette_size: usize) -> Self {
        let t = tables();
        let px = (w * h) as u64;
        let lb = (64 - px.leading_zeros()).clamp(10, 18);
        let mut nbits = 1;
        while (1usize << nbits) < palette_size {
            nbits += 1;
        }
        PalModel {
            t,
            w,
            nbits,
            tabs: (0..NT).map(|_| HistTable::new(lb, 4)).collect(),
            h: [0; NT],
            mixer: Mixer::new(NIN, 16 * 64, 900, 1 << 13),
            mixer2: Mixer::new(NIN, 16 * 257, 900, 1 << 13),
            mixer3: Mixer::new(NIN, 16 * 64, 900, 1 << 13),
            mf: Mixer::new(4, 16 * 8, 500, 22000),
            apm1: Apm::new(256 * 64, 6, t),
            apm2: Apm::new(256 * 257, 6, t),
            apm3: Apm::new(256 * 64, 6, t),
            mm: vec![CTR_INIT; 64 * 2],
            x: [0; NIN],
        }
    }

    /// Codes palette index `value` at (x, y); `idx` holds already decoded indices
    /// (row-major), `mpred` the match-model prediction (or u64::MAX), `mlen` its length.
    #[allow(clippy::too_many_arguments)]
    pub fn code<C: BitCoder>(&mut self, coder: &mut C, idx: &[u64], x: usize, y: usize, value: u32, eqp: u32, mpred: u64, mlen: u32) -> u32 {
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
        let (pw, pn, pnw, pne, pww, pnn) = (g(-1, 0), g(0, -1), g(-1, -1), g(1, -1), g(-2, 0), g(0, -2));
        let (pnne, pnnw, pnww, pnee, pwww, pnnn) = (g(1, -2), g(-1, -2), g(-2, -1), g(2, -1), g(-3, 0), g(0, -3));
        let (pnnww, pnnee, pneee) = (g(-2, -2), g(2, -2), g(3, -1));
        let m = if mpred == u64::MAX { OOB } else { mpred as i32 };
        let mq = mlen.min(15) as i32;
        let col = (x as i32).min(65535);
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
            hash_slice(&[13, pnw]),
            hash_slice(&[14, pne]),
            hash_slice(&[15, pw, pne]),
            hash_slice(&[16, pn, pnn, pnnn]),
            hash_slice(&[17, pw, pww, pwww]),
            hash_slice(&[18, pne, pnee, pneee]),
            hash_slice(&[19, pnw, pnnww, pn]),
            hash_slice(&[20, pne, pnnee, pn]),
            hash_slice(&[21, m, mq]),
            hash_slice(&[22, m, pw, pn]),
            hash_slice(&[23, col]),
            hash_slice(&[24, col, pn]),
            hash_slice(&[25, col, pw]),
            hash_slice(&[26, pw, pn, pnw, pne, pww, pnn, pnne, pnnw, pnww, pnee, pwww, pnnn]),
        ];
        let nb = self.nbits;
        let _ = nb;
        let mut c0: u32 = 1;
        let mut v: u32 = 0;
        for (tb, &hh) in self.tabs.iter_mut().zip(self.h.iter()) {
            tb.set(hh);
        }
        let mut sel2 = (pw as usize).min(256);
        let t = self.t;
        let mbucket = (mlen.min(31) as usize) * 2;
        for b in 0..nb {
            if b > 0 && b % 4 == 0 {
                let hv = v;
                for (i, tb) in self.tabs.iter_mut().enumerate() {
                    tb.set(hash2(self.h[i], hv + 1));
                }
                c0 = 1;
                sel2 = (pn as usize).min(256);
            }
            let shift = nb - 1 - b;
            let bit_enc = (value >> shift) & 1;
            let slot = c0 as usize;
            for (i, tb) in self.tabs.iter().enumerate() {
                let (a, r) = tb.inputs(slot, t);
                self.x[2 * i] = a;
                self.x[2 * i + 1] = r;
            }
            // match model: the predicted index agrees with the bits coded so far?
            let mexp = if m != OOB && (m as u32 >> (shift + 1)) == (v) { Some((m as u32 >> shift) & 1) } else { None };
            let mslot = mbucket + mexp.map_or(0, |_| 1);
            let mst = if mexp.is_some() { t.stretch(ctr_p16(self.mm[mslot])) } else { 0 };
            self.x[2 * NT] = match mexp {
                Some(1) => mst,
                Some(_) => -mst,
                None => 0,
            };
            self.x[2 * NT + 1] = 256;
            self.x[2 * NT + 2] = if mexp.is_some() { 0 } else { 128 };
            let node = ((b as usize) << 4 | slot) & 255;
            let d1 = self.mixer.mix(&self.x, (b as usize) * 64 + (eqp as usize & 63), t);
            let d2 = self.mixer2.mix(&self.x, (slot & 15) * 257 + sel2, t);
            let msel = match mexp {
                None => 0,
                Some(e) => 1 + e as usize + 2 * (mlen.min(15) as usize),
            };
            let d3 = self.mixer3.mix(&self.x, (slot & 15) * 64 + msel.min(63), t);
            let fin = [d1, d2, d3, 256];
            let d = self.mf.mix(&fin, (slot & 15) * 8 + (b as usize).min(7), t);
            let pa1 = self.apm1.pp(d, node * 64 + (eqp as usize & 63));
            let pa2 = self.apm2.pp(d, node * 257 + (pw as usize).min(256));
            let pa3 = self.apm3.pp(d, node * 64 + msel.min(63));
            let p = clamp_p((self.mf.pr + pa1 + pa2 + pa3) >> 2);
            let bit = coder.code(bit_enc, p);
            for tb in self.tabs.iter_mut() {
                tb.update(slot, bit, &t.recip);
            }
            if let Some(e) = mexp {
                ctr_update(&mut self.mm[mslot], (bit == e) as u32, 1023, &t.recip);
            }
            let xx = self.x;
            self.mixer.update(&xx, bit);
            self.mixer2.update(&xx, bit);
            self.mixer3.update(&xx, bit);
            self.mf.update(&fin, bit);
            self.apm1.update(bit);
            self.apm2.update(bit);
            self.apm3.update(bit);
            c0 = (c0 << 1) | bit;
            v = (v << 1) | bit;
        }
        v
    }
}
