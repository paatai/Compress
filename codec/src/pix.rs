//! Pixel-level model for synthetic images (screenshots, graphics): before any channel is
//! coded, a few binary flags say whether the pixel equals one of a short list of candidate
//! pixels (its neighbours and the prediction of a 2-D match model). Pixels are compared as
//! packed keys (all channels at once).

use crate::coder::BitCoder;
use crate::primitives::*;

const NCAND: usize = 5;
const NT: usize = 17;
const NIN: usize = 2 * NT + 1;

pub const KIND_NONE: u8 = 0;
const KIND_W: u8 = 1;
const KIND_N: u8 = 2;
const KIND_NE: u8 = 3;
const KIND_NW: u8 = 4;
const KIND_M: u8 = 5;

pub struct PixModel {
    t: &'static Tables,
    w: usize,
    h: usize,
    pub keys: Vec<u64>,
    hit: Vec<u8>,
    mtab: Vec<u32>,
    mmask: u32,
    mptr: usize,
    mlen: u32,
    mhash: u32,
    run: u32,
    tabs: Vec<HistTable>,
    mixer: Mixer,
    mixer2: Mixer,
    mf: Mixer,
    apm: Apm,
    apm2: Apm,
    x: [i32; NIN],
    // per-pixel state
    cands: [(u64, u8); NCAND],
    ncand: usize,
    pub flags_off: bool,
    stats: bool,
    pub cost: f64,
    pub cost_by: [[f64; 2]; 8],
    pub cnt_by: [[u64; 2]; 8],
    pub resolved: u64,
    pub total: u64,
    pub mpred: u64,
    eqp: u32,
    sel: usize,
}

const NONE: u64 = u64::MAX;

impl PixModel {
    pub fn new(w: usize, h: usize, flags: bool) -> Self {
        let t = tables();
        let px = (w * h) as u64;
        let lb = (64 - px.leading_zeros()).clamp(10, 20);
        let mbits = (64 - px.leading_zeros()).clamp(10, 22);
        PixModel {
            t,
            w,
            h,
            keys: vec![NONE; w * h],
            hit: vec![KIND_NONE; w * h],
            mtab: vec![u32::MAX; 1 << mbits],
            mmask: (1 << mbits) - 1,
            mptr: usize::MAX,
            mlen: 0,
            mhash: 0,
            run: 0,
            tabs: (0..NT).map(|i| HistTable::new(if i < 3 { 12 } else { lb.min(18) }, 3)).collect(),
            mixer: Mixer::new(NIN, 8 * 256, 800, 1 << 13),
            mixer2: Mixer::new(NIN, 8 * 64, 800, 1 << 13),
            mf: Mixer::new(3, 8 * 8, 500, 30000),
            apm: Apm::new(8 * 1024, 6, t),
            apm2: Apm::new(8 * 4096, 6, t),
            x: [0; NIN],
            cands: [(NONE, 0); NCAND],
            ncand: 0,
            flags_off: !flags,
            stats: std::env::var_os("KOLMO_STATS").is_some(),
            cost: 0.0,
            cost_by: [[0.0; 2]; 8],
            cnt_by: [[0; 2]; 8],
            resolved: 0,
            total: 0,
            mpred: NONE,
            eqp: 0,
            sel: 0,
        }
    }

    #[inline(always)]
    fn key(&self, x: isize, y: isize) -> u64 {
        if x < 0 || y < 0 || x >= self.w as isize {
            NONE
        } else {
            self.keys[y as usize * self.w + x as usize]
        }
    }

    #[inline(always)]
    fn hitk(&self, x: isize, y: isize) -> u32 {
        if x < 0 || y < 0 || x >= self.w as isize {
            7
        } else {
            self.hit[y as usize * self.w + x as usize] as u32
        }
    }

    /// Codes the candidate flags for pixel (x, y). Returns the pixel key if a candidate matched.
    pub fn code<C: BitCoder>(&mut self, coder: &mut C, x: usize, y: usize, value: u64) -> Option<u64> {
        let (xi, yi) = (x as isize, y as isize);
        let w = self.key(xi - 1, yi);
        let n = self.key(xi, yi - 1);
        let nw = self.key(xi - 1, yi - 1);
        let ne = self.key(xi + 1, yi - 1);
        let ww = self.key(xi - 2, yi);
        let nn = self.key(xi, yi - 2);
        let nne = self.key(xi + 1, yi - 2);
        let eqp = (w == n) as u32
            | (((w == nw) as u32) << 1)
            | (((n == ne) as u32) << 2)
            | (((n == nw) as u32) << 3)
            | (((w == ww) as u32) << 4)
            | (((n == nn) as u32) << 5)
            | (((ne == nne) as u32) << 6);
        self.eqp = eqp;

        // match model: follow the current match or look one up by the causal neighbourhood
        let pos = y * self.w + x;
        let hsh = (hash_slice(&[w as i32, (w >> 32) as i32, n as i32, (n >> 32) as i32, nw as i32, ne as i32, ww as i32]) & self.mmask) as u32;
        self.mhash = hsh;
        if self.mptr == usize::MAX || self.mlen == 0 {
            let cand = self.mtab[hsh as usize];
            self.mptr = if cand != u32::MAX { cand as usize } else { usize::MAX };
        }
        let m = if self.mptr != usize::MAX && self.mptr < pos { self.keys[self.mptr] } else { NONE };

        let mut nc = 0;
        let order: [(u64, u8); 5] = if self.mlen >= 8 {
            [(m, KIND_M), (w, KIND_W), (n, KIND_N), (ne, KIND_NE), (nw, KIND_NW)]
        } else {
            [(w, KIND_W), (n, KIND_N), (m, KIND_M), (ne, KIND_NE), (nw, KIND_NW)]
        };
        for &(k, kind) in order.iter() {
            if k == NONE {
                continue;
            }
            if self.cands[..nc].iter().any(|c| c.0 == k) {
                continue;
            }
            self.cands[nc] = (k, kind);
            nc += 1;
        }
        self.ncand = nc;
        let hw = self.hitk(xi - 1, yi);
        let hn = self.hitk(xi, yi - 1);
        let hnw = self.hitk(xi - 1, yi - 1);
        let hne = self.hitk(xi + 1, yi - 1);
        let mq = match self.mlen {
            0 => 0,
            1..=2 => 1,
            3..=7 => 2,
            8..=15 => 3,
            16..=31 => 4,
            _ => 5,
        };
        let rq = self.run.min(15);
        let ahead = |row: isize, refk: u64| -> u32 {
            if row < 0 || refk == NONE {
                return 0;
            }
            let base = row as usize * self.w;
            let mut k = 0u32;
            let mut xx = x + 1;
            while k < 31 && xx < self.w && self.keys[base + xx] == refk {
                k += 1;
                xx += 1;
            }
            k
        };
        let an = ahead(yi - 1, n);
        let ann = ahead(yi - 2, nn);
        let aq = |a: u32| if a < 4 { a } else { 4 + (31 - a.leading_zeros()).min(4) - 2 };
        let (anq, annq) = (aq(an), aq(ann));
        let rl = if self.run < 4 { self.run } else { 4 + (31 - self.run.leading_zeros()).min(8) - 2 };
        self.mpred = m;
        if self.flags_off {
            return None;
        }
        let mut found = None;
        for i in 0..nc {
            let (ck, kind) = self.cands[i];
            let kk = (i as u32) << 3 | kind as u32;
            let h = [
                hash2(kk, eqp),
                hash2(kk | 0x100, hw | (hn << 3) | (hnw << 6) | (hne << 9)),
                hash2(kk | 0x200, mq | ((m == w) as u32) << 3 | ((m == n) as u32) << 4 | (rq << 5)),
                hash_slice(&[kk as i32, w as i32, (w >> 32) as i32, n as i32, (n >> 32) as i32]),
                hash_slice(&[kk as i32 | 0x400, ck as i32, (ck >> 32) as i32, eqp as i32]),
                hash_slice(&[kk as i32 | 0x500, w as i32, n as i32, nw as i32, ne as i32]),
                hash_slice(&[kk as i32 | 0x600, eqp as i32, hw as i32, hn as i32, mq as i32, ww as i32, nn as i32]),
                hash2(kk | 0x700, (eqp << 8) | (hw << 4) | hn),
                hash_slice(&[kk as i32 | 0x800, ck as i32, (ck >> 32) as i32]),
                hash_slice(&[kk as i32 | 0x900, w as i32, ne as i32]),
                hash_slice(&[kk as i32 | 0xA00, n as i32, nn as i32, nne as i32]),
                hash_slice(&[kk as i32 | 0xB00, w as i32, ww as i32, nw as i32]),
                hash_slice(&[kk as i32 | 0xC00, x as i32, eqp as i32]),
                hash_slice(&[kk as i32 | 0xD00, ck as i32, w as i32, hw as i32]),
                hash2(kk | 0xE00, (anq << 12) | (annq << 8) | rl),
                hash2(kk | 0xF00, (anq << 12) | (eqp << 4) | hw),
                hash_slice(&[kk as i32 | 0x1000, anq as i32, rl as i32, w as i32, n as i32]),
            ];
            for (tb, &hh) in self.tabs.iter_mut().zip(h.iter()) {
                tb.set(hh);
            }
            self.sel = (i * 8 + kind as usize) * 32 + (mq as usize) * 4 + (hw.min(3) as usize);
            let bit = self.code_bit(coder, (value == ck) as u32, i, eqp);
            if bit == 1 {
                found = Some((ck, kind));
                break;
            }
        }
        self.total += 1;
        found.map(|(k, kind)| {
            self.hit[pos] = kind;
            self.resolved += 1;
            k
        })
    }

    #[inline(always)]
    fn code_bit<C: BitCoder>(&mut self, coder: &mut C, bit: u32, slot: usize, eqp: u32) -> u32 {
        let t = self.t;
        for (i, tb) in self.tabs.iter().enumerate() {
            let (a, r) = tb.inputs(slot, t);
            self.x[2 * i] = a;
            self.x[2 * i + 1] = r;
        }
        self.x[2 * NT] = 256;
        let d1 = self.mixer.mix(&self.x, self.sel % (8 * 256), t);
        let d2 = self.mixer2.mix(&self.x, slot * 64 + (eqp as usize & 63), t);
        let fin = [d1, d2, 256];
        let d = self.mf.mix(&fin, slot * 8 + (self.sel & 7), t);
        let pa = self.apm.pp(d, (slot * 128 + eqp as usize) % (8 * 1024));
        let pa2 = self.apm2.pp(d, slot * 4096 + (self.sel & 4095));
        let p = clamp_p((2 * self.mf.pr + pa + pa2) >> 2);
        let bit = coder.code(bit, p);
        if self.stats {
            let pp = if bit != 0 { p as f64 / 65536.0 } else { 1.0 - p as f64 / 65536.0 };
            self.cost -= pp.log2();
            self.cost_by[slot][bit as usize] -= pp.log2();
            self.cnt_by[slot][bit as usize] += 1;
        }
        let rc = &t.recip;
        for tb in self.tabs.iter_mut() {
            tb.update(slot, bit, rc);
        }
        let x = self.x;
        self.mixer.update(&x, bit);
        self.mixer2.update(&x, bit);
        self.mf.update(&fin, bit);
        self.apm.update(bit);
        self.apm2.update(bit);
        bit
    }

    pub fn eqp(&self) -> u32 {
        self.eqp
    }

    pub fn mlen(&self) -> u32 {
        self.mlen
    }

    /// Records the final value of pixel (x, y) (after channel coding if no candidate matched).
    pub fn finish(&mut self, x: usize, y: usize, key: u64) {
        let pos = y * self.w + x;
        let w = if x > 0 { self.keys[pos - 1] } else { NONE };
        self.run = if key == w { self.run + 1 } else { 0 };
        let m = if self.mptr != usize::MAX && self.mptr < pos { self.keys[self.mptr] } else { NONE };
        self.keys[pos] = key;
        if m == key && m != NONE {
            self.mlen += 1;
            self.mptr += 1;
        } else {
            self.mlen = 0;
            self.mptr = usize::MAX;
        }
        self.mtab[self.mhash as usize] = pos as u32;
        let _ = self.h;
    }
}
