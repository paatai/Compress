//! Context-mixing model for the quantized DCT coefficients of a JPEG.
//!
//! Components are coded one after another (all luma blocks of a stripe, then the chroma blocks,
//! which can therefore look at the luma blocks around them), blocks in raster order. Inside a
//! block the order is chosen for prediction, not as in the JPEG stream:
//!
//! 1. the number of nonzero coefficients in the 7x7 block of AC coefficients with u, v >= 1,
//! 2. those 49 coefficients in zigzag order until the count is exhausted,
//! 3. the first row (u, 0) and first column (0, v): each edge's nonzero count, then its values.
//!    They are predicted from the block above / to the left by asking that the two blocks join
//!    smoothly at the shared boundary, which (given everything else already coded in both
//!    blocks) leaves one unknown per coefficient,
//! 4. DC, predicted from the boundary pixels of the neighbouring blocks and of this block's AC.
//!
//! Every value is binarised (zero?, exponent in unary, mantissa bits, sign) and every binary
//! decision is predicted by a dozen adaptive context models combined by gated logistic mixers,
//! a second mixing layer and adaptive probability maps. Only integer arithmetic is used.

use super::format::{Frame, NATURAL};
use crate::coder::BitCoder;
use crate::primitives::*;

const SLOT_BITS: u32 = 4;
const SLOTS: usize = 1 << SLOT_BITS;
const S_ZERO: usize = 0;
const S_SIGN: usize = 1;
const S_EXP: usize = 2;
const S_M1: usize = 10;
const S_M2: usize = 11;
const S_M3: usize = 14;

const NCTX: usize = 12;
const NIN: usize = NCTX + 1;

/// cos(m * pi / 16), m = 0..8
const COS16: [f64; 9] = [
    1.0,
    0.980_785_280_403_230_4,
    0.923_879_532_511_286_7,
    0.831_469_612_302_545_2,
    0.707_106_781_186_547_6,
    0.555_570_233_019_602_2,
    0.382_683_432_365_089_8,
    0.195_090_322_016_128_25,
    0.0,
];

fn cos_m(m: usize) -> f64 {
    let m = m % 32;
    match m {
        0..=8 => COS16[m],
        9..=16 => -COS16[16 - m],
        17..=24 => -COS16[m - 16],
        _ => COS16[32 - m],
    }
}

/// B[x][u] = C(u)/2 * cos((2x+1) u pi / 16) * 2^13
fn basis() -> [[i32; 8]; 8] {
    let mut b = [[0i32; 8]; 8];
    for (x, row) in b.iter_mut().enumerate() {
        for (u, e) in row.iter_mut().enumerate() {
            let cu = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
            *e = (cu / 2.0 * cos_m((2 * x + 1) * u) * 8192.0).round() as i32;
        }
    }
    b
}

/// Pseudo-logarithmic magnitude bucket: 0,1,2,3,4-5,6-7,8-11,12-15,16-23,... (cap 15).
#[inline(always)]
fn mb(v: u32) -> u32 {
    if v < 4 {
        v
    } else {
        let l = 31 - v.leading_zeros();
        (2 * l + ((v >> (l - 1)) & 1)).min(15)
    }
}

#[inline(always)]
fn clip(v: i32, m: i32) -> i32 {
    v.clamp(-m, m)
}

/// Bucket for the number of nonzero coefficients still to come.
#[inline(always)]
fn nzb(n: u32) -> u32 {
    match n {
        0..=5 => n,
        6..=7 => 6,
        8..=10 => 7,
        11..=14 => 8,
        15..=19 => 9,
        20..=26 => 10,
        _ => 11,
    }
}

/// Signed bucket of a prediction in 1/8 units of the quantization step, 0..=30.
#[inline(always)]
fn pb(p8: i32) -> u32 {
    let a = p8.unsigned_abs();
    let m = if a < 4 { 0 } else { (mb(a >> 2)).min(14) + 1 };
    if p8 < 0 {
        15 - m
    } else {
        15 + m
    }
}

struct Plane {
    /// blocks per row (padded) and rows in this stripe
    w: usize,
    h: usize,
    q: [i32; 64],
    luma: bool,
    /// subsampling relative to the first component (luma): luma blocks per block, x and y
    sx: usize,
    sy: usize,
    nz77: Vec<u8>,
    nze: Vec<u8>,
    /// border pixels (x8, level shifted): row 7, row 6, column 7, column 6
    border: Vec<[i16; 32]>,
    dcres: Vec<i16>,
    dc_coded: bool,
    ac_coded: bool,
    dc_w: usize,
    dc_h: usize,
    ac_w: usize,
    ac_h: usize,
}

pub struct Model {
    t: &'static Tables,
    b: [[i32; 8]; 8],
    tabs: Vec<CtxTable>,
    nzt: Vec<CtxTable>,
    m: [Mixer; 3],
    mf: Mixer,
    apm1: Apm,
    apm2: Apm,
    x: [i32; NIN],
    sel: [usize; 3],
    actx: [usize; 2],
    nzm: Mixer,
    nzapm: Apm,
    planes: Vec<Plane>,
}

const ZERO_BLOCK: [i16; 64] = [0; 64];

impl Model {
    pub fn new(f: &Frame, rows: &[(usize, usize)], coded: &[(bool, bool)]) -> Self {
        let t = tables();
        let total: usize = rows.iter().zip(f.comps.iter()).map(|(r, c)| (r.1 - r.0) * c.pw).sum();
        let cap = (usize::BITS - total.max(1).leading_zeros()).clamp(12, 22);
        let lb = [14u32, 16, 16, 16, 16, 16, 16, 16, 20, 20, 10, 14];
        let tabs = lb.iter().map(|&b| CtxTable::new(b.min(cap), SLOT_BITS, 255)).collect();
        let nzt = (0..5).map(|_| CtxTable::new(14.min(cap), 6, 255)).collect();
        let l0 = &f.comps[0];
        let planes = f
            .comps
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let h = rows[i].1 - rows[i].0;
                let n = c.pw * h;
                let mut q = [0i32; 64];
                for k in 0..64 {
                    q[k] = c.q[k].max(1) as i32;
                }
                let (dc_c, ac_c) = coded[i];
                Plane {
                    w: c.pw,
                    h,
                    q,
                    luma: i == 0,
                    sx: (l0.h / c.h).max(1),
                    sy: (l0.v / c.v).max(1),
                    nz77: vec![0; n],
                    nze: vec![0; n],
                    border: vec![[0; 32]; n],
                    dcres: vec![0; n],
                    dc_coded: dc_c,
                    ac_coded: ac_c,
                    dc_w: c.pw,
                    dc_h: h,
                    ac_w: c.pw,
                    ac_h: h,
                }
            })
            .collect();
        Model {
            t,
            b: basis(),
            tabs,
            nzt,
            m: [Mixer::new(NIN, SLOTS * 8, 400, 1 << 13), Mixer::new(NIN, SLOTS * 130, 400, 1 << 13), Mixer::new(NIN, SLOTS * 80, 400, 1 << 13)],
            mf: Mixer::new(4, SLOTS * 8, 300, 22000),
            apm1: Apm::new(SLOTS * 256, 6, t),
            apm2: Apm::new(SLOTS * 512, 6, t),
            x: [0; NIN],
            sel: [0; 3],
            actx: [0; 2],
            nzm: Mixer::new(6, 64 * 8, 400, 1 << 14),
            nzapm: Apm::new(64 * 32, 6, t),
            planes,
        }
    }

    /// Restricts coding of component `ci` to the blocks that the scans actually cover.
    pub fn set_extent(&mut self, ci: usize, dc: (usize, usize), ac: (usize, usize)) {
        let p = &mut self.planes[ci];
        p.dc_w = dc.0.min(p.w);
        p.dc_h = dc.1.min(p.h);
        p.ac_w = ac.0.min(p.w);
        p.ac_h = ac.1.min(p.h);
    }

    #[inline(always)]
    fn bit<C: BitCoder>(&mut self, coder: &mut C, slot: usize, bit: u32) -> u32 {
        let t = self.t;
        for i in 0..NCTX {
            self.x[i] = t.stretch(self.tabs[i].p(slot));
        }
        self.x[NCTX] = 256;
        let d0 = self.m[0].mix(&self.x, self.sel[0] * SLOTS + slot, t);
        let d1 = self.m[1].mix(&self.x, self.sel[1] * SLOTS + slot, t);
        let d2 = self.m[2].mix(&self.x, self.sel[2] * SLOTS + slot, t);
        let y = [d0, d1, d2, 256];
        let df = self.mf.mix(&y, slot * 8 + self.sel[0], t);
        let p0 = t.squash(df);
        let pa = self.apm1.pp(df, self.actx[0] * SLOTS + slot);
        let pc = self.apm2.pp(df, self.actx[1] * SLOTS + slot);
        let p = clamp_p((2 * p0 + pa + pc) >> 2);
        let b = coder.code(bit, p);
        let r = &t.recip;
        for i in 0..NCTX {
            self.tabs[i].update(slot, b, r);
        }
        self.m[0].update(&self.x, b);
        self.m[1].update(&self.x, b);
        self.m[2].update(&self.x, b);
        self.mf.update(&y, b);
        self.apm1.update(b);
        self.apm2.update(b);
        b
    }

    /// Codes a value given contexts already selected in all tables.
    fn value<C: BitCoder>(&mut self, coder: &mut C, v: i32, may_be_zero: bool) -> i32 {
        let a = v.unsigned_abs();
        if may_be_zero && self.bit(coder, S_ZERO, (a != 0) as u32) == 0 {
            return 0;
        }
        let e_true = if a == 0 { 0 } else { 31 - a.leading_zeros() };
        let mut e = 0u32;
        while e < 14 {
            let slot = S_EXP + (e as usize).min(7);
            if self.bit(coder, slot, (e_true > e) as u32) == 0 {
                break;
            }
            e += 1;
        }
        let mut m = 1u32;
        for i in (0..e).rev() {
            let bit = (a >> i) & 1;
            let slot = match e {
                1 => S_M1,
                2 => {
                    if i == 1 {
                        S_M2
                    } else {
                        S_M2 + 1 + (m & 1) as usize
                    }
                }
                _ => {
                    if i == e - 1 {
                        S_M3
                    } else {
                        S_M3 + 1
                    }
                }
            };
            m = (m << 1) | self.bit(coder, slot, bit);
        }
        let neg = self.bit(coder, S_SIGN, (v < 0) as u32);
        if neg != 0 {
            -(m as i32)
        } else {
            m as i32
        }
    }

    /// Codes a count 0..=63 (6 bits, binary tree) with the count tables.
    fn count<C: BitCoder>(&mut self, coder: &mut C, n: u32, hs: &[u32; 5], max: u32, sel: usize, actx: usize) -> u32 {
        let t = self.t;
        for (i, &h) in hs.iter().enumerate() {
            self.nzt[i].set(h);
        }
        let bits = 32 - max.leading_zeros();
        let mut node = 1usize;
        for b in (0..bits).rev() {
            let bit = (n >> b) & 1;
            let mut x = [0i32; 6];
            for i in 0..5 {
                x[i] = t.stretch(self.nzt[i].p(node));
            }
            x[5] = 256;
            let d = self.nzm.mix(&x, sel * 64 + node, t);
            let pa = self.nzapm.pp(d, (actx & 31) * 64 + node);
            let p = clamp_p((t.squash(d) + pa) >> 1);
            let got = coder.code(bit, p);
            for i in 0..5 {
                self.nzt[i].update(node, got, &t.recip);
            }
            self.nzm.update(&x, got);
            self.nzapm.update(got);
            node = (node << 1) | got as usize;
        }
        (node as u32) & ((1 << bits) - 1)
    }

    /// Codes one stripe of all components. `planes[ci]` holds the coefficients of rows
    /// `rows[ci]` of component `ci` (`pw * rows * 64`, natural order); when decoding they are
    /// written.
    pub fn code<C: BitCoder>(&mut self, coder: &mut C, planes: &mut [&mut [i16]], progress: &mut dyn FnMut(f32)) {
        let total: usize = self.planes.iter().map(|p| p.w * p.h).sum::<usize>().max(1);
        let mut done = 0usize;
        for ci in 0..self.planes.len() {
            let (w, h) = (self.planes[ci].w, self.planes[ci].h);
            for by in 0..h {
                for bx in 0..w {
                    self.code_block(coder, planes, ci, bx, by);
                }
                done += w;
                if by % 8 == 7 {
                    progress(done as f32 / total as f32);
                }
            }
        }
        progress(1.0);
    }

    fn code_block<C: BitCoder>(&mut self, coder: &mut C, planes: &mut [&mut [i16]], ci: usize, bx: usize, by: usize) {
        let (w, sx, sy, luma) = (self.planes[ci].w, self.planes[ci].sx, self.planes[ci].sy, self.planes[ci].luma);
        let bi = by * w + bx;
        let (prev, rest) = planes.split_at_mut(ci);
        let plane: &mut [i16] = &mut rest[0][..];
        let mut cur = [0i16; 64];
        cur.copy_from_slice(&plane[bi * 64..bi * 64 + 64]);
        let get = |x: usize, y: usize| -> [i16; 64] {
            let mut b = [0i16; 64];
            b.copy_from_slice(&plane[(y * w + x) * 64..(y * w + x) * 64 + 64]);
            b
        };
        let a = if by > 0 { get(bx, by - 1) } else { ZERO_BLOCK };
        let l = if bx > 0 { get(bx - 1, by) } else { ZERO_BLOCK };
        let al = if bx > 0 && by > 0 { get(bx - 1, by - 1) } else { ZERO_BLOCK };
        let ar = if bx + 1 < w && by > 0 { get(bx + 1, by - 1) } else { ZERO_BLOCK };
        let (has_a, has_l) = (by > 0, bx > 0);

        // co-located luma statistics for chroma
        let (mut lnz, mut lact) = (0u32, 0u32);
        let mut lco = [0i32; 64];
        if !luma {
            let lp = &self.planes[0];
            let lw = lp.w;
            let mut cnt = 0;
            for dy in 0..sy {
                for dx in 0..sx {
                    let (x, y) = (bx * sx + dx, by * sy + dy);
                    if x < lw && y < lp.h {
                        let li = y * lw + x;
                        lnz += lp.nz77[li] as u32;
                        let lb = &prev[0][li * 64..li * 64 + 64];
                        for k in 1..64 {
                            let v = lb[k] as i32;
                            lact += v.unsigned_abs();
                            lco[k] += v * lp.q[k];
                        }
                        cnt += 1;
                    }
                }
            }
            if cnt > 0 {
                lnz /= cnt;
                lact /= cnt;
                for v in lco.iter_mut() {
                    *v /= cnt as i32;
                }
            }
        }
        let p = &self.planes[ci];
        let q = p.q;
        let ac_on = p.ac_coded && bx < p.ac_w && by < p.ac_h;
        let dc_on = p.dc_coded && bx < p.dc_w && by < p.dc_h;
        let nz_a = if has_a { p.nz77[bi - w] as u32 } else { 0 };
        let nz_l = if has_l { p.nz77[bi - 1] as u32 } else { 0 };
        let cls = (!luma) as u32;

        // ---- 1. nonzero count of the 7x7 block
        let mut nz_true = 0u32;
        for &n in INTERIOR.iter() {
            nz_true += (cur[n] != 0) as u32;
        }
        let nz77 = if ac_on {
            let pred = match (has_a, has_l) {
                (true, true) => (nz_a + nz_l + 1) / 2,
                (true, false) => nz_a,
                (false, true) => nz_l,
                _ => lnz,
            };
            let nz_al = if has_a && has_l { p.nz77[bi - w - 1] as u32 } else { 0 };
            let nz_ar = if has_a && bx + 1 < w { p.nz77[bi - w + 1] as u32 } else { 0 };
            let hs = [
                pred * 2 + cls,
                hash2(1, (nz_a.min(31) << 6) | (nz_l.min(31) << 1) | cls),
                hash2(2, (lnz.min(49) << 8) | (mb(lact) << 4) | (cls << 3) | (has_a as u32) << 1 | has_l as u32),
                hash2(3, (nz_al << 8) | (nz_ar << 1) | cls),
                hash2(4, (pred << 4) | (mb(lact) << 1) | cls),
            ];
            self.count(coder, nz_true, &hs, 49, (cls as usize) * 2 + (has_a && has_l) as usize, nzb(pred) as usize * 2 + cls as usize)
        } else {
            0
        };

        // ---- 2. interior coefficients
        let mut left = nz77;
        let mut rem = 49u32;
        let mut act = 0u32;
        let mut run = 0u32;
        for (j, &n) in INTERIOR.iter().enumerate() {
            if left == 0 {
                for &n2 in &INTERIOR[j..] {
                    cur[n2] = 0;
                }
                break;
            }
            let (u, v) = (n & 7, n >> 3);
            let k = ZZ[n] as u32;
            let (va, vl, val, var) = (a[n] as i32, l[n] as i32, al[n] as i32, ar[n] as i32);
            let wavg = ((2 * (va.unsigned_abs() + vl.unsigned_abs()) + val.unsigned_abs() + var.unsigned_abs()) * 4 + 3) / 6;
            // only interior neighbours are known here: the first row and column come later
            let c1 = if u > 1 { cur[n - 1] as i32 } else { 0 };
            let c8 = if v > 1 { cur[n - 8] as i32 } else { 0 };
            let c9 = if u > 1 && v > 1 { cur[n - 9] as i32 } else { 0 };
            let lum = if luma { 0 } else { (lco[n].unsigned_abs() / q[n] as u32).min(255) };
            let hs: [u32; NCTX] = [
                (k << 9) | (nzb(left) << 5) | (mb(wavg) << 1) | cls,
                hash2(10 + k, (mb(c1.unsigned_abs()) << 12) | (mb(c8.unsigned_abs()) << 8) | (left.min(7) << 1) | cls),
                hash2(20 + k, (((clip(va, 4) + 4) as u32) << 8) | (((clip(vl, 4) + 4) as u32) << 4) | (cls << 1)),
                hash2(30 + k, (nz77.min(49) << 8) | ((nz77 - left).min(31) << 2) | cls),
                hash2(40 + k, (mb(var.unsigned_abs()) << 12) | (mb(val.unsigned_abs()) << 8) | (mb(wavg) << 2) | cls),
                hash2(50 + k, (mb(act) << 8) | (nzb(left) << 2) | cls),
                hash2(60 + k, if luma { (mb(rem) << 8) | (run.min(15) << 2) } else { (mb(lum) << 8) | (mb(lact) << 3) | 1 }),
                hash2(70 + k, (run.min(15) << 8) | (nzb(left) << 3) | cls),
                hash2(80 + k, (((clip(va, 8) + 8) as u32) << 15) | (((clip(vl, 8) + 8) as u32) << 10) | (((clip(val, 8) + 8) as u32) << 5) | cls),
                hash2(90 + k, (((clip(c1, 8) + 8) as u32) << 15) | (((clip(c8, 8) + 8) as u32) << 10) | (((clip(c9, 8) + 8) as u32) << 5) | cls),
                (k << 1) | cls,
                (k << 7) | (wavg.min(63) << 1) | cls,
            ];
            for (i, &h) in hs.iter().enumerate() {
                self.tabs[i].set(h);
            }
            self.sel = [cls as usize, k as usize * 2 + cls as usize, (nzb(left) as usize) * 4 + (mb(wavg) as usize).min(3)];
            self.actx = [k as usize * 2 + cls as usize, (mb(wavg) as usize) * 16 + nzb(left) as usize];
            let forced = left == rem;
            let val = self.value(coder, cur[n] as i32, !forced);
            cur[n] = val as i16;
            if val != 0 {
                left -= 1;
                act += val.unsigned_abs();
                run = 0;
            } else {
                run += 1;
            }
            rem -= 1;
            let _ = (u, v);
        }

        // ---- 3. edges: first row from the block above, first column from the block to the left
        let mut nze_total = 0u32;
        for edge in 0..2usize {
            let has_n = if edge == 0 { has_a } else { has_l };
            let nb = if edge == 0 { &a } else { &l };
            let idx = |i: usize| if edge == 0 { i } else { i * 8 };
            let mut cnt_true = 0u32;
            for i in 1..8 {
                cnt_true += (cur[idx(i)] != 0) as u32;
            }
            let nb_cnt = if has_n {
                let mut c = 0;
                for i in 1..8 {
                    c += (nb[idx(i)] != 0) as u32;
                }
                c
            } else {
                0
            };
            let cnt = if ac_on {
                let hs = [
                    hash2(100 + edge as u32, (nzb(nz77) << 8) | (nb_cnt << 4) | (has_n as u32) << 1 | cls),
                    hash2(110 + edge as u32, (nz77.min(49) << 2) | cls),
                    hash2(120 + edge as u32, (nb_cnt << 8) | (mb(lact) << 2) | cls),
                    hash2(130 + edge as u32, (nze_total << 8) | (nzb(nz77) << 2) | cls),
                    hash2(140 + edge as u32, (mb(act) << 8) | (nb_cnt << 3) | cls),
                ];
                self.count(coder, cnt_true, &hs, 7, 4 + edge * 2 + cls as usize, 24 + edge * 4 + nb_cnt.min(3) as usize)
            } else {
                0
            };
            nze_total += cnt;
            let mut left = cnt;
            for i in 1..8 {
                let n = idx(i);
                if left == 0 {
                    cur[n] = 0;
                    continue;
                }
                // continuity prediction (x8 of the quantization step)
                let (p1, p2) = if has_n { self.edge_pred(&cur, nb, edge, i, &q) } else { (0, 0) };
                let vn = nb[n] as i32;
                let k = ZZ[n] as u32;
                let lum = if luma { 0 } else { (lco[n].unsigned_abs() / q[n] as u32).min(255) };
                let inner = if edge == 0 { cur[n + 8] as i32 } else { cur[n + 1] as i32 };
                let hs: [u32; NCTX] = [
                    hash2(200 + k, (pb(p1) << 6) | (left.min(7) << 2) | cls),
                    hash2(210 + k, (pb(p2) << 6) | (has_n as u32) << 1 | cls),
                    hash2(220 + k, (pb((p1 + p2) / 2) << 8) | (mb(inner.unsigned_abs()) << 2) | cls),
                    hash2(230 + k, (((clip(vn, 8) + 8) as u32) << 8) | (left.min(7) << 2) | cls),
                    hash2(240 + k, (nzb(nz77) << 8) | (cnt << 4) | (left << 1) | cls),
                    hash2(250 + k, (mb(act) << 8) | (pb(p1) << 2) | cls),
                    hash2(260 + k, if luma { mb(vn.unsigned_abs()) << 2 } else { (mb(lum) << 8) | (pb(p1) << 2) | 1 }),
                    hash2(270 + k, (pb(p1) << 10) | (pb(p2) << 4) | cls),
                    hash2(280 + k, (((clip(vn, 8) + 8) as u32) << 12) | ((clip(inner, 8) + 8) as u32) << 6 | cls),
                    hash2(290 + k, ((clip(p1 >> 3, 31) + 32) as u32) << 2 | cls),
                    hash2(300 + k, cls),
                    hash2(310 + k, ((clip((p1 + p2) >> 4, 15) + 16) as u32) << 6 | (left.min(7) << 2) | cls),
                ];
                for (j, &h) in hs.iter().enumerate() {
                    self.tabs[j].set(h);
                }
                self.sel = [(1 + edge) * 2 + cls as usize, k as usize * 2 + cls as usize, 48 + pb(p1) as usize / 2];
                self.actx = [128 + (pb(p1) as usize) * 2 + cls as usize, 256 + (pb(p2) as usize) * 4 + (left.min(3) as usize)];
                let val = self.value(coder, cur[n] as i32, left != 8 - i as u32);
                cur[n] = val as i16;
                if val != 0 {
                    left -= 1;
                    act += val.unsigned_abs();
                }
            }
        }

        // ---- 4. DC from the neighbours' boundary pixels and this block's AC
        let pix0 = self.idct(&cur, &q, false);
        let border_a = if has_a { Some(self.planes[ci].border[bi - w]) } else { None };
        let border_l = if has_l { Some(self.planes[ci].border[bi - 1]) } else { None };
        let q0 = q[0];
        let mut preds: Vec<i32> = Vec::with_capacity(4);
        let mut spread = 0i32;
        if let Some(ba) = border_a {
            let mut s = 0i32;
            for x in 0..8 {
                let ea = 2 * ba[x] as i32 + (ba[x] as i32 - ba[8 + x] as i32);
                let ec = 2 * pix0[x] - (pix0[8 + x] - pix0[x]);
                s += ea - ec;
            }
            preds.push(s);
        }
        if let Some(bl) = border_l {
            let mut s = 0i32;
            for y in 0..8 {
                let el = 2 * bl[16 + y] as i32 + (bl[16 + y] as i32 - bl[24 + y] as i32);
                let ec = 2 * pix0[y * 8] - (pix0[y * 8 + 1] - pix0[y * 8]);
                s += el - ec;
            }
            preds.push(s);
        }
        // preds are sums of 8 doubled boundary estimates of DC*q0 (x8 pixel units): /16
        let pred_dc = if preds.is_empty() {
            0
        } else {
            if preds.len() == 2 {
                spread = ((preds[0] - preds[1]).abs() / 16 / q0).min(1 << 12);
            }
            let s: i32 = preds.iter().sum();
            let n = preds.len() as i32 * 16 * q0;
            (2 * s + n * s.signum()) / (2 * n)
        };
        let dcv = if dc_on {
            let ra = if has_a { self.planes[ci].dcres[bi - w] as i32 } else { 0 };
            let rl = if has_l { self.planes[ci].dcres[bi - 1] as i32 } else { 0 };
            let ctxr = mb(ra.unsigned_abs() + rl.unsigned_abs());
            let hs: [u32; NCTX] = [
                hash2(400, (mb(spread as u32) << 6) | (preds.len() as u32) << 2 | cls),
                hash2(401, (ctxr << 6) | (preds.len() as u32) << 2 | cls),
                hash2(402, (mb(spread as u32) << 8) | (ctxr << 2) | cls),
                hash2(403, ((clip(ra, 8) + 8) as u32) << 8 | ((clip(rl, 8) + 8) as u32) << 2 | cls),
                hash2(404, (nzb(nz77) << 8) | (nze_total << 2) | cls),
                hash2(405, (mb(act) << 6) | (mb(spread as u32) << 2) | cls),
                hash2(406, if luma { 0 } else { (mb(lact) << 2) | 1 }),
                hash2(407, (preds.len() as u32) << 2 | cls),
                hash2(408, ((clip(pred_dc - if has_l { l[0] as i32 } else { 0 }, 15) + 16) as u32) << 2 | cls),
                hash2(409, ((clip(pred_dc - if has_a { a[0] as i32 } else { 0 }, 15) + 16) as u32) << 2 | cls),
                hash2(410, cls),
                hash2(411, (mb(spread as u32) << 2) | cls),
            ];
            for (j, &h) in hs.iter().enumerate() {
                self.tabs[j].set(h);
            }
            self.sel = [6 + cls as usize, 128 + cls as usize, 64 + mb(spread as u32) as usize];
            self.actx = [192 + cls as usize * 16 + mb(spread as u32) as usize, 384 + (ctxr as usize) * 2 + cls as usize];
            let r = self.value(coder, cur[0] as i32 - pred_dc, true);
            let v = pred_dc + r;
            self.planes[ci].dcres[bi] = r.clamp(-32767, 32767) as i16;
            v
        } else {
            cur[0] as i32
        };
        cur[0] = dcv as i16;

        // ---- store
        let pl = &mut self.planes[ci];
        pl.nz77[bi] = nz77 as u8;
        pl.nze[bi] = nze_total as u8;
        let dc8 = dcv * q0;
        let mut bd = [0i16; 32];
        for i in 0..8 {
            bd[i] = (pix0[56 + i] + dc8).clamp(-32768, 32767) as i16;
            bd[8 + i] = (pix0[48 + i] + dc8).clamp(-32768, 32767) as i16;
            bd[16 + i] = (pix0[i * 8 + 7] + dc8).clamp(-32768, 32767) as i16;
            bd[24 + i] = (pix0[i * 8 + 6] + dc8).clamp(-32768, 32767) as i16;
        }
        pl.border[bi] = bd;
        plane[bi * 64..bi * 64 + 64].copy_from_slice(&cur);
        let _ = (&al, &ar);
    }

    /// Predictions of edge coefficient `i` (row 0 if `edge == 0`, else column 0) of `cur` from
    /// neighbour `nb`, in 1/8 of the quantization step: plain continuity at the boundary, and
    /// continuity of linearly extrapolated values.
    fn edge_pred(&self, cur: &[i16; 64], nb: &[i16; 64], edge: usize, i: usize, q: &[i32; 64]) -> (i32, i32) {
        let b = &self.b;
        let at = |blk: &[i16; 64], f: usize, g: usize| -> i64 {
            // coefficient with frequency f along the edge and g across it
            let n = if edge == 0 { g * 8 + f } else { f * 8 + g };
            blk[n] as i64 * q[n] as i64
        };
        let mut nb7 = 0i64;
        let mut nb6 = 0i64;
        let mut c0 = 0i64;
        let mut c1 = 0i64;
        for g in 0..8 {
            let nv = at(nb, i, g);
            nb7 += b[7][g] as i64 * nv;
            nb6 += b[6][g] as i64 * nv;
            if g > 0 {
                let cv = at(cur, i, g);
                c0 += b[0][g] as i64 * cv;
                c1 += b[1][g] as i64 * cv;
            }
        }
        let n0 = if edge == 0 { i } else { i * 8 };
        let den = b[0][0] as i64 * q[n0] as i64;
        let p1 = (nb7 - c0) * 8 / den;
        let ext_nb = nb7 + (nb7 - nb6) / 2;
        let ext_c = c0 + (c0 - c1) / 2;
        let p2 = (ext_nb - ext_c) * 8 / den;
        (p1.clamp(-1 << 20, 1 << 20) as i32, p2.clamp(-1 << 20, 1 << 20) as i32)
    }

    /// Dequantized inverse DCT, in 1/8 pixel units, with or without DC.
    fn idct(&self, blk: &[i16; 64], q: &[i32; 64], with_dc: bool) -> [i32; 64] {
        let b = &self.b;
        let mut tmp = [0i64; 64];
        for v in 0..8 {
            for x in 0..8 {
                let mut s = 0i64;
                for u in 0..8 {
                    let n = v * 8 + u;
                    if n == 0 && !with_dc {
                        continue;
                    }
                    s += b[x][u] as i64 * (blk[n] as i64 * q[n] as i64);
                }
                tmp[v * 8 + x] = s;
            }
        }
        let mut out = [0i32; 64];
        for y in 0..8 {
            for x in 0..8 {
                let mut s = 0i64;
                for v in 0..8 {
                    s += b[y][v] as i64 * tmp[v * 8 + x];
                }
                out[y * 8 + x] = ((s + (1 << 22)) >> 23).clamp(-(1 << 24), 1 << 24) as i32;
            }
        }
        out
    }
}

/// Zigzag position of each natural index.
const ZZ: [u8; 64] = {
    let mut z = [0u8; 64];
    let mut k = 0;
    while k < 64 {
        z[NATURAL[k]] = k as u8;
        k += 1;
    }
    z
};

/// Natural indices with u >= 1 and v >= 1, in zigzag order.
const INTERIOR: [usize; 49] = {
    let mut r = [0usize; 49];
    let mut j = 0;
    let mut k = 0;
    while k < 64 {
        let n = NATURAL[k];
        if n % 8 != 0 && n / 8 != 0 {
            r[j] = n;
            j += 1;
        }
        k += 1;
    }
    r
};
