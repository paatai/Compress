//! Context-mixing model for the quantized DCT coefficients of a JPEG.
//!
//! Components are coded one after another (all luma blocks of a stripe, then the chroma blocks,
//! which can therefore look at the luma blocks around them), blocks in raster order. Inside a
//! block the order is chosen for prediction, not as in the JPEG stream:
//!
//! 1. the number of nonzero coefficients in the 7x7 block of AC coefficients with u, v >= 1,
//! 2. those 49 coefficients in zigzag order until the count is exhausted,
//! 3. the first row (u, 0) and first column (0, v): each edge's nonzero count, then its values.
//!    Each edge coefficient is predicted from the neighbouring blocks: by asking that the two
//!    blocks join smoothly at the shared boundary (with and without extrapolating the gradient),
//!    and from the same coefficient in the blocks above and to the left. The predictions are
//!    blended with weights from their recent errors.
//! 4. DC, predicted in the same way from the boundary pixels of the neighbouring blocks, given
//!    this block's AC, and from the neighbours' DC values.
//!
//! Every value is binarised (zero?, exponent in unary, mantissa bits, sign) and every binary
//! decision is predicted by a dozen adaptive context models, plus one that compares the
//! prediction with the threshold the decision tests, combined by gated logistic mixers, a second
//! mixing layer and adaptive probability maps. Only integer arithmetic is used.

use super::format::{Frame, NATURAL};
use crate::coder::BitCoder;
use crate::ols::Ols;
use crate::primitives::*;

const SLOT_BITS: u32 = 4;
const SLOTS: usize = 1 << SLOT_BITS;
const S_ZERO: usize = 0;
const S_SIGN: usize = 1;
const S_EXP: usize = 2;
const S_M1: usize = 10;
const S_M2: usize = 11;
const S_M3: usize = 14;

const NCTX: usize = 16;
const NIN: usize = NCTX + 3;

const PART_INTERIOR: usize = 0;
const PART_EDGE: usize = 1;
const PART_DC: usize = 2;
const DYN_B: usize = 24;
const DYN_C: usize = 8;

/// Model parameters; KOMLA_JTUNE="i=v,..." overrides them for experiments (both sides must agree).
const TUNE_DEFAULT: [i32; 24] = [255, 300, 200, 7, 255, 400, 0, 1, 0, 1, 8192, 800, 4000, 12, 12, -90, 4096, 64, 32, 1, 16, 1, 2, 0];

fn tune() -> &'static [i32; 24] {
    static T: std::sync::OnceLock<[i32; 24]> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = TUNE_DEFAULT;
        if let Ok(s) = std::env::var("KOMLA_JTUNE") {
            for kv in s.split(',').filter(|s| !s.is_empty()) {
                if let Some((k, v)) = kv.split_once('=') {
                    if let (Ok(k), Ok(v)) = (k.parse::<usize>(), v.parse::<i32>()) {
                        if k < 24 {
                            t[k] = v;
                        }
                    }
                }
            }
        }
        t
    })
}

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

/// Signed distance bucket (1/8 units) for the decision context, 0..DYN_B.
#[inline(always)]
fn dbucket(d: i64) -> usize {
    let a = d.unsigned_abs().min(1 << 30) as u32;
    let m = if a < 2 { 0 } else { mb(a >> 1).min(11) } as usize;
    if d < 0 {
        11 - m
    } else {
        12 + m
    }
}

/// Rounded division by 8.
#[inline(always)]
fn r8(p: i32) -> i32 {
    if p >= 0 {
        (p + 4) >> 3
    } else {
        -((-p + 4) >> 3)
    }
}

/// Weighted blend of the available predictions with weights 1/err^2. Returns the blend and its
/// expected error (same units as `errs`).
fn blend(preds: &[i32], avail: &[bool], errs: &[i32]) -> (i32, i32) {
    let (mut ws, mut wp, mut we) = (0i64, 0i64, 0i64);
    for j in 0..preds.len() {
        if !avail[j] {
            continue;
        }
        let e = errs[j].max(1) as i64;
        let w = (1i64 << 40) / (e * e + 16);
        ws += w;
        wp += w * preds[j] as i64;
        we += w * e;
    }
    if ws == 0 {
        return (0, 1 << 12);
    }
    ((wp / ws) as i32, (we / ws) as i32)
}

#[inline(always)]
fn track(err: &mut i32, e8: i32) {
    let target = 4 * e8.unsigned_abs().min(1 << 20) as i32;
    *err += (target - *err) >> 4;
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
    /// error of the blended prediction of each edge coefficient, in quantization steps
    eerr: Vec<[u8; 14]>,
    /// nonzero coefficients by zigzag position
    mask: Vec<u64>,
    /// sum of absolute AC values
    act: Vec<u16>,
    /// reconstructed pixels (x8, level shifted), kept for the first component only
    pix: Vec<[i16; 64]>,
    /// chroma: per block sums for the scale from luma (and from the first chroma component)
    cfl: Vec<[i64; 4]>,
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
    m: [Mixer; 4],
    mf: Mixer,
    apm1: Apm,
    apm2: Apm,
    x: [i32; NIN],
    sel: [usize; 3],
    actx: [usize; 2],
    nzm: Mixer,
    nzapm: Apm,
    dyn_t: Vec<u32>,
    dyn_base: usize,
    dyn_idx: usize,
    pred8: i32,
    /// parametric model of the current value: logistic with centre `pmu8` and scale `ps8`
    /// (1/8 quantization steps); `ps8 == 0` disables it
    pmu8: i32,
    ps8: i32,
    /// stretched probability of the current decision under the parametric model
    pin: i32,
    /// running mean of |value| x 64 per (class, position, nonzeros left, neighbour magnitude)
    emag: Vec<i32>,
    /// 7x7 coefficients predicted by online least squares (per class and position) from the
    /// blocks above and to the left, the corner blocks, the coefficients of this block already
    /// coded in the same row and column, and for chroma the predictions from luma / Cb
    ls: Vec<Ols>,
    ls_feats: Vec<Vec<(u8, u8)>>,
    ls_err: Vec<i32>,
    ls_on: bool,
    /// prediction for the sign decision (defaults to `pred8`)
    spred8: i32,
    edge_err: Vec<[i32; 7]>,
    /// edge coefficients predicted by online least squares (per class, edge and position)
    els: Vec<Ols>,
    els_feats: Vec<Vec<(u8, u8)>>,
    dc_err: [[i32; 6]; 2],
    /// DC predicted by online least squares from the DC predictors and the neighbours' DC
    dls: Vec<Ols>,
    gcfl: [[i64; 4]; 4],
    planes: Vec<Plane>,
    /// cost accounting (KOMLA_STATS): bits per decision class, current class base
    stats: Option<Box<[f64; 16]>>,
    part: usize,
    dbg: [[f64; 8]; 8],
}

const ZERO_BLOCK: [i16; 64] = [0; 64];
const STAT_NAMES: [&str; 14] = [
    "nz77 count", "7x7 zero", "7x7 exponent", "7x7 mantissa", "7x7 sign", "edge count", "edge zero", "edge exponent", "edge mantissa", "edge sign", "DC zero", "DC exponent", "DC mantissa", "DC sign",
];

fn cost(p1: u32, bit: u32) -> f64 {
    let p = if bit != 0 { p1 } else { 65536 - p1 } as f64 / 65536.0;
    -p.log2()
}

impl Model {
    pub fn new(f: &Frame, rows: &[(usize, usize)], coded: &[(bool, bool)]) -> Self {
        let t = tables();
        let tu = tune();
        let total: usize = rows.iter().zip(f.comps.iter()).map(|(r, c)| (r.1 - r.0) * c.pw).sum();
        let cap = (usize::BITS - total.max(1).leading_zeros()).clamp(12, if tu[6] > 0 { tu[6] as u32 } else { 16 });
        let lb = [14u32, 16, 16, 16, 16, 16, 16, 16, 20, 20, 10, 16, 16, 18, 16, 16];
        let tabs = lb.iter().map(|&b| CtxTable::with_init(b.min(cap), SLOT_BITS, tu[0] as u32, CTR_INIT | tu[9] as u32)).collect();
        let nzt = (0..5).map(|_| CtxTable::with_init(12.min(cap), 6, tu[4] as u32, CTR_INIT | tu[9] as u32)).collect();
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
                    eerr: vec![[0; 14]; n],
                    mask: vec![0; n],
                    act: vec![0; n],
                    pix: if i == 0 && f.comps.len() > 1 { vec![[0; 64]; n] } else { Vec::new() },
                    cfl: if i > 0 { vec![[0; 4]; n] } else { Vec::new() },
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
            m: [
                Mixer::new(NIN, SLOTS * 8, tu[1], tu[10]),
                Mixer::new(NIN, SLOTS * 130, tu[1], tu[10]),
                Mixer::new(NIN, SLOTS * 80, tu[1], tu[10]),
                Mixer::new(NIN, SLOTS * 3 * 16, tu[1], tu[10]),
            ],
            mf: Mixer::new(5, SLOTS * 8, tu[2], 18000),
            apm1: Apm::new(SLOTS * 256, tu[3] as u32, t),
            apm2: Apm::new(SLOTS * 512, tu[3] as u32, t),
            x: [0; NIN],
            sel: [0; 3],
            actx: [0; 2],
            nzm: Mixer::new(6, 64 * 8, tu[5], 1 << 14),
            nzapm: Apm::new(64 * 32, tu[3] as u32, t),
            dyn_t: vec![CTR_INIT; 3 * SLOTS * DYN_B * DYN_C],
            dyn_base: 0,
            dyn_idx: 0,
            pred8: 0,
            pmu8: 0,
            ps8: 0,
            pin: 0,
            emag: vec![64; 2 * 64 * 12 * 16],
            ls: (0..2 * 49).map(|i| Ols::new(ls_feats(i / 49, i % 49).len(), 1.0 - 1.0 / tu[16].max(16) as f64, tu[17] as f64, tu[18].max(1) as u32)).collect(),
            ls_feats: (0..2 * 49).map(|i| ls_feats(i / 49, i % 49)).collect(),
            ls_err: vec![256; 2 * 49],
            ls_on: tu[19] != 0,
            spred8: 0,
            edge_err: vec![[256; 7]; 3 * 2 * 8],
            els: (0..2 * 2 * 8).map(|i| Ols::new(edge_ls_feats(i / 16, (i / 8) % 2, (i % 8).max(1)).len(), 1.0 - 1.0 / tu[16].max(16) as f64, tu[17] as f64, tu[18].max(1) as u32)).collect(),
            els_feats: (0..2 * 2 * 8).map(|i| edge_ls_feats(i / 16, (i / 8) % 2, (i % 8).max(1))).collect(),
            dc_err: [[256; 6]; 2],
            dls: (0..2).map(|_| Ols::new(10, 1.0 - 1.0 / tu[16].max(16) as f64, tu[17] as f64, tu[18].max(1) as u32)).collect(),
            gcfl: [[0; 4]; 4],
            planes,
            stats: if std::env::var_os("KOMLA_STATS").is_some() { Some(Box::new([0.0; 16])) } else { None },
            part: 1,
            dbg: [[0.0; 8]; 8],
        }
    }

    fn report(&self) {
        if let Some(s) = &self.stats {
            let total: f64 = s.iter().sum();
            let mut out = format!("coefficient model: {:.0} bytes\n", total / 8.0);
            for (i, name) in STAT_NAMES.iter().enumerate() {
                out += &format!("  {name:<14} {:>10.0} B {:>6.2}%\n", s[i] / 8.0, 100.0 * s[i] / total.max(1.0));
            }
            eprint!("{out}");
            for (i, d) in self.dbg.iter().enumerate().skip(1) {
                let n = d[6].max(1.0) * 8.0;
                eprintln!("  edge i={i}: |F| {:.3}  |F-P| lak {:.3} grad {:.3} nb {:.3} side {:.3} blend {:.3}", d[0] / n, d[1] / n, d[2] / n, d[3] / n, d[4] / n, d[5] / n);
            }
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

    /// Selects the decision context for `slot` from the signed distance `d` (1/8 units) between
    /// the prediction and the decision's threshold.
    #[inline(always)]
    fn set_dyn(&mut self, slot: usize, d: i64) {
        self.dyn_idx = self.dyn_base + (slot * DYN_B + dbucket(d)) * DYN_C;
    }

    #[inline(always)]
    fn bit<C: BitCoder>(&mut self, coder: &mut C, slot: usize, bit: u32) -> u32 {
        let t = self.t;
        for i in 0..NCTX {
            self.x[i] = t.stretch(self.tabs[i].p(slot));
        }
        self.x[NCTX] = t.stretch(ctr_p16(self.dyn_t[self.dyn_idx]));
        self.x[NCTX + 1] = 256;
        self.x[NCTX + 2] = self.pin;
        let d0 = self.m[0].mix(&self.x, self.sel[0] * SLOTS + slot, t);
        let d1 = self.m[1].mix(&self.x, self.sel[1] * SLOTS + slot, t);
        let d2 = self.m[2].mix(&self.x, self.sel[2] * SLOTS + slot, t);
        let s3 = (self.dyn_base / (SLOTS * DYN_B * DYN_C)) * 16 + ((self.pin + 2048) >> 8).clamp(0, 15) as usize;
        let d3 = self.m[3].mix(&self.x, s3 * SLOTS + slot, t);
        let y = [d0, d1, d2, d3, 256];
        let df = self.mf.mix(&y, slot * 8 + self.sel[0], t);
        let p0 = t.squash(df);
        let pa = self.apm1.pp(df, self.actx[0] * SLOTS + slot);
        let pc = self.apm2.pp(df, self.actx[1] * SLOTS + slot);
        let p = clamp_p((2 * p0 + pa + pc) >> 2);
        let b = coder.code(bit, p);
        if let Some(s) = self.stats.as_mut() {
            let kind = match slot {
                S_ZERO => 0,
                S_SIGN => 3,
                2..=9 => 1,
                _ => 2,
            };
            s[self.part + kind] += cost(p, b);
        }
        let r = &t.recip;
        for i in 0..NCTX {
            self.tabs[i].update(slot, b, r);
        }
        ctr_update(&mut self.dyn_t[self.dyn_idx], b, 255, r);
        self.m[0].update(&self.x, b);
        self.m[1].update(&self.x, b);
        self.m[2].update(&self.x, b);
        self.m[3].update(&self.x, b);
        self.mf.update(&y, b);
        self.apm1.update(b);
        self.apm2.update(b);
        b
    }

    /// Logistic CDF of the parametric model at x8 (1/8 units), 16-bit.
    #[inline(always)]
    fn cdf(&self, x8: i64) -> i64 {
        let d = ((x8 - self.pmu8 as i64) * 256 / self.ps8 as i64).clamp(-2047, 2047) as i32;
        self.t.squash(d) as i64
    }

    /// Parametric probability mass of |value| in [a, b) (b = None: unbounded), a >= 1.
    #[inline(always)]
    fn mass(&self, a: i64, b: Option<i64>) -> i64 {
        let (hp, ln) = match b {
            Some(b) => (self.cdf(8 * b - 4), self.cdf(-8 * b + 4)),
            None => (65536, 0),
        };
        (hp - self.cdf(8 * a - 4)) + (self.cdf(-8 * a + 4) - ln)
    }

    /// Sets the parametric input for a decision with probability num/den of a 1 bit.
    #[inline(always)]
    fn set_param(&mut self, num: i64, den: i64) {
        self.pin = if self.ps8 == 0 || den <= 0 {
            0
        } else {
            self.t.stretch(((num << 16) / den).clamp(32, 65504) as u32)
        };
    }

    /// Codes a value given contexts already selected in all tables. `self.pred8` is the
    /// prediction (1/8 units; for magnitudes only its absolute value matters); `pmu8`/`ps8`
    /// describe the parametric model.
    fn value<C: BitCoder>(&mut self, coder: &mut C, v: i32, may_be_zero: bool) -> i32 {
        let a = v.unsigned_abs();
        let pa = self.pred8.unsigned_abs() as i64;
        let on = self.ps8 != 0;
        if may_be_zero {
            self.set_dyn(S_ZERO, pa - 4);
            if on {
                self.set_param(self.mass(1, None), 65536);
            }
            if self.bit(coder, S_ZERO, (a != 0) as u32) == 0 {
                self.pin = 0;
                return 0;
            }
        }
        let e_true = if a == 0 { 0 } else { 31 - a.leading_zeros() };
        let mut e = 0u32;
        while e < 14 {
            let slot = S_EXP + (e as usize).min(7);
            self.set_dyn(slot, pa - ((16i64 << e) - 4));
            if on {
                self.set_param(self.mass(2 << e, None), self.mass(1 << e, None));
            }
            if self.bit(coder, slot, (e_true > e) as u32) == 0 {
                break;
            }
            e += 1;
        }
        let mut lo = 1i64 << e;
        for i in (0..e).rev() {
            let bit = (a >> i) & 1;
            let slot = match e {
                1 => S_M1,
                2 => {
                    if i == 1 {
                        S_M2
                    } else {
                        S_M2 + 1 + (lo >> 1 & 1) as usize
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
            let mid = lo + (1i64 << i);
            self.set_dyn(slot, pa - (8 * mid - 4));
            if on {
                let top = lo + (2i64 << i);
                self.set_param(self.mass(mid, Some(top)), self.mass(lo, Some(top)));
            }
            if self.bit(coder, slot, bit) != 0 {
                lo = mid;
            }
        }
        let m = lo as i32;
        let close = ((self.pred8.unsigned_abs() as i64 - 8 * m as i64).unsigned_abs() >> 3).min(255) as u32;
        self.dyn_idx = self.dyn_base / (SLOTS * DYN_B * DYN_C) * (SLOTS * DYN_B * DYN_C) + (S_SIGN * DYN_B + dbucket(self.spred8 as i64)) * DYN_C + mb(close).min(7) as usize;
        if on {
            let m8 = 8 * m as i64;
            let fp = self.cdf(m8 + 4) - self.cdf(m8 - 4);
            let fneg = self.cdf(-m8 + 4) - self.cdf(-m8 - 4);
            self.set_param(fneg, fp + fneg);
        }
        let neg = self.bit(coder, S_SIGN, (v < 0) as u32);
        self.pin = 0;
        if neg != 0 {
            -m
        } else {
            m
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
            if let Some(s) = self.stats.as_mut() {
                s[if max > 7 { 0 } else { 5 }] += cost(p, got);
            }
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
        let tu = tune();
        let (lr0, boost, span) = (tu[1], tu[11], tu[12].max(1) as usize);
        for ci in 0..self.planes.len() {
            let (w, h) = (self.planes[ci].w, self.planes[ci].h);
            for by in 0..h {
                // mixers learn faster at the start of a stripe
                let lr = lr0 + (boost * span.saturating_sub(done) as i32) / span as i32;
                for m in self.m.iter_mut() {
                    m.set_lr(lr);
                }
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
        self.report();
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
        let (has_a, has_l) = (by > 0, bx > 0);
        let a = if has_a { get(bx, by - 1) } else { ZERO_BLOCK };
        let l = if has_l { get(bx - 1, by) } else { ZERO_BLOCK };
        let al = if has_l && has_a { get(bx - 1, by - 1) } else { ZERO_BLOCK };
        let ar = if bx + 1 < w && has_a { get(bx + 1, by - 1) } else { ZERO_BLOCK };

        // chroma: statistics of the co-located luma blocks, the luma DCT at this component's
        // resolution, and predictions of every coefficient from it (and, for the second
        // chroma component, from the first) with locally fitted scales
        let (mut lnz, mut lact) = (0u32, 0u32);
        let mut lc = [0i64; 64];
        let mut cc = [0i64; 64];
        let mut pc8 = [0i32; 64];
        let mut pr8 = [0i32; 64];
        let has_cc = ci == 2 && self.planes[1].w == w && self.planes[1].h == self.planes[ci].h;
        if !luma {
            let lp = &self.planes[0];
            let lw = lp.w;
            let mut cnt = 0;
            let mut px = [0i32; 64];
            for dy in 0..sy {
                for dx in 0..sx {
                    let (x, y) = (bx * sx + dx, by * sy + dy);
                    if x < lw && y < lp.h {
                        let li = y * lw + x;
                        lnz += lp.nz77[li] as u32;
                        let lb = &prev[0][li * 64..li * 64 + 64];
                        for k in 1..64 {
                            lact += (lb[k] as i32).unsigned_abs();
                        }
                        let lpix = &lp.pix[li];
                        for yy in 0..8 {
                            for xx in 0..8 {
                                px[((dy * 8 + yy) / sy) * 8 + (dx * 8 + xx) / sx] += lpix[yy * 8 + xx] as i32;
                            }
                        }
                        cnt += 1;
                    }
                }
            }
            if cnt > 0 {
                lnz /= cnt;
                lact /= cnt;
                let div = (sx * sy) as i32;
                for v in px.iter_mut() {
                    *v /= div;
                }
                lc = self.fdct(&px);
            }
            let q = &self.planes[ci].q;
            let scale = |own: [i64; 2], g: [i64; 2]| -> i64 {
                let num = own[0] + g[0] / 8;
                let den = own[1] + g[1] / 8 + 1;
                ((num as i128 * 65536) / den as i128).clamp(-(1 << 20), 1 << 20) as i64
            };
            let pl = &self.planes[ci];
            let mut own = [0i64; 4];
            for nb in [if has_l { Some(bi - 1) } else { None }, if has_a { Some(bi - w) } else { None }].into_iter().flatten() {
                for j in 0..4 {
                    own[j] += pl.cfl[nb][j];
                }
            }
            let g = self.gcfl[ci];
            let a1 = scale([own[0], own[1]], [g[0], g[1]]);
            for n in 1..64 {
                pc8[n] = ((8 * lc[n] * a1) / (65536 * q[n] as i64)).clamp(-(1 << 20), 1 << 20) as i32;
            }
            if has_cc {
                let cb = &prev[1][bi * 64..bi * 64 + 64];
                let q1 = &self.planes[1].q;
                for n in 1..64 {
                    cc[n] = cb[n] as i64 * q1[n] as i64;
                }
                let a2 = scale([own[2], own[3]], [g[2], g[3]]);
                for n in 1..64 {
                    pr8[n] = ((8 * cc[n] * a2) / (65536 * q[n] as i64)).clamp(-(1 << 20), 1 << 20) as i32;
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
        let ucls = cls as usize;

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
            self.count(coder, nz_true, &hs, 49, ucls * 2 + (has_a && has_l) as usize, nzb(pred) as usize * 2 + ucls)
        } else {
            0
        };

        // ---- 2. interior coefficients
        let act_a = if has_a { self.planes[ci].act[bi - w] as u32 } else { 0 };
        let act_l = if has_l { self.planes[ci].act[bi - 1] as u32 } else { 0 };
        let mask_a = if has_a { self.planes[ci].mask[bi - w] } else { 0 };
        let mask_l = if has_l { self.planes[ci].mask[bi - 1] } else { 0 };
        let mut left = nz77;
        let mut rem = 49u32;
        let mut act = 0u32;
        let mut run = 0u32;
        let mut last_nz = 0u32;
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
            let px = if has_cc { pr8[n] } else { pc8[n] };
            let lsi = ucls * 49 + j;
            let use_ls = self.ls_on && (tune()[23] == 0 || (j as i32) < tune()[23]);
            let pls = if use_ls {
                let mut x = [0f64; 44];
                for (i, &(src, idx)) in self.ls_feats[lsi].iter().enumerate() {
                    let idx = idx as usize;
                    x[i] = match src {
                        0 => (a[idx] as i32 * q[idx]) as f64,
                        1 => (l[idx] as i32 * q[idx]) as f64,
                        2 => (al[idx] as i32 * q[idx]) as f64,
                        3 => (ar[idx] as i32 * q[idx]) as f64,
                        4 => (cur[idx] as i32 * q[idx]) as f64,
                        5 => lc[idx] as f64,
                        6 => cc[idx] as f64,
                        _ => 64.0,
                    };
                }
                let pr = self.ls[lsi].predict(&x) * 8.0 / q[n] as f64;
                if pr.is_finite() { pr.round().clamp(-1e6, 1e6) as i32 } else { 0 }
            } else {
                0
            };
            let lsconf = mb((self.ls_err[lsi] >> 5) as u32).min(7);
            let prev_c = if j > 0 { cur[INTERIOR[j - 1]] as i32 } else { 0 };
            let (tail_a, tail_l) = ((mask_a >> k).count_ones().min(7), (mask_l >> k).count_ones().min(7));
            let hs: [u32; NCTX] = [
                (k << 9) | (nzb(left) << 5) | (mb(wavg) << 1) | cls,
                hash2(10 + k, (mb(c1.unsigned_abs()) << 12) | (mb(c8.unsigned_abs()) << 8) | (left.min(7) << 1) | cls),
                if self.ls_on {
                    hash2(20 + k, (pb(pls) << 8) | (mb(wavg) << 3) | (lsconf.min(3) << 1) | cls)
                } else {
                    hash2(20 + k, (((clip(va, 4) + 4) as u32) << 8) | (((clip(vl, 4) + 4) as u32) << 4) | (cls << 1))
                },
                hash2(30 + k, (nz77.min(49) << 8) | ((nz77 - left).min(31) << 2) | cls),
                if self.ls_on && tune()[7] == 1 {
                    hash2(40 + k, (((clip(r8(pls), 15) + 16) as u32) << 10) | (nzb(left) << 5) | (lsconf << 1) | cls)
                } else {
                    hash2(40 + k, (mb(var.unsigned_abs()) << 12) | (mb(val.unsigned_abs()) << 8) | (mb(wavg) << 2) | cls)
                },
                hash2(50 + k, (mb(act) << 8) | (nzb(left) << 2) | cls),
                hash2(60 + k, if luma { (mb(rem) << 8) | (run.min(15) << 2) } else { (pb(px) << 8) | (nzb(left) << 3) | 1 }),
                hash2(70 + k, (run.min(15) << 8) | (nzb(left) << 3) | cls),
                hash2(80 + k, (((clip(va, 8) + 8) as u32) << 15) | (((clip(vl, 8) + 8) as u32) << 10) | (((clip(val, 8) + 8) as u32) << 5) | cls),
                hash2(90 + k, (((clip(c1, 8) + 8) as u32) << 15) | (((clip(c8, 8) + 8) as u32) << 10) | (((clip(c9, 8) + 8) as u32) << 5) | cls),
                (k << 1) | cls,
                hash2(110 + k, if luma { (tail_a << 9) | (tail_l << 6) | (nzb(left) << 2) } else { (pb(pc8[n]) << 8) | (pb(pr8[n]) << 2) | 1 }),
                hash2(120 + k, (mb(act_a) << 10) | (mb(act_l) << 6) | (nzb(left) << 2) | cls),
                hash2(130 + k, (((clip(prev_c, 8) + 8) as u32) << 12) | (mb(last_nz) << 8) | (run.min(15) << 2) | cls),
                hash2(140 + k, (mb(va.unsigned_abs() + vl.unsigned_abs()) << 10) | (mb(c1.unsigned_abs() + c8.unsigned_abs()) << 6) | (nzb(left) << 2) | cls),
                if self.ls_on {
                    hash2(150 + k, (pb(pls) << 10) | (nzb(left) << 5) | (lsconf << 1) | cls)
                } else {
                    hash2(150 + k, (tail_a << 12) | (tail_l << 9) | (mb(wavg) << 5) | (left.min(7) << 1) | cls)
                },
            ];
            for (i, &h) in hs.iter().enumerate() {
                self.tabs[i].set(h);
            }
            self.sel = [ucls, k as usize * 2 + ucls, (nzb(left) as usize) * 4 + (mb(wavg) as usize).min(3)];
            self.actx = [k as usize * 2 + ucls, (mb(wavg) as usize) * 16 + nzb(left) as usize];
            self.pred8 = if luma { 2 * wavg as i32 } else { px };
            self.pmu8 = if use_ls { pls } else if luma { 0 } else { px };
            self.spred8 = if use_ls { pls } else { self.pred8 };
            let ei = ((ucls * 64 + k as usize) * 12 + nzb(left) as usize) * 16 + mb(wavg) as usize;
            self.ps8 = match tune()[15] {
                0 => 0,
                f if f > 0 => (wavg as i32 * f / 16).max(2),
                f => (self.emag[ei] * -f / 64 / 11).max(2),
            };
            if use_ls && tune()[20] > 0 {
                self.ps8 = (self.ls_err[lsi] * tune()[20] / 64).max(2);
            }
            self.dyn_base = PART_INTERIOR * SLOTS * DYN_B * DYN_C + (k as usize >> 3).min(7);
            let forced = left == rem;
            self.part = 1;
            let val = self.value(coder, cur[n] as i32, !forced);
            cur[n] = val as i16;
            let em = &mut self.emag[ei];
            *em += (64 * val.unsigned_abs().min(1 << 16) as i32 - *em) >> 5;
            if use_ls {
                self.ls[lsi].update((val * q[n]) as f64);
                track(&mut self.ls_err[lsi], 8 * val - pls);
            }
            if val != 0 {
                left -= 1;
                act += val.unsigned_abs();
                run = 0;
                last_nz = val.unsigned_abs();
            } else {
                run += 1;
            }
            rem -= 1;
        }

        // ---- 3. edges: first row from the block above, first column from the block to the left
        let mut nze_total = 0u32;
        let mut eerr = [0u8; 14];
        for edge in 0..2usize {
            let has_n = if edge == 0 { has_a } else { has_l };
            let has_s = if edge == 0 { has_l } else { has_a };
            let nb = if edge == 0 { &a } else { &l };
            let side = if edge == 0 { &l } else { &a };
            let idx = |i: usize| if edge == 0 { i } else { i * 8 };
            // predictions of all seven coefficients of this edge (1/8 quantization steps)
            let mut pr = [[0i32; 7]; 8];
            let pcls = ci.min(2);
            let avail = [has_n, has_n, has_n, has_s, !luma, has_cc, (has_n || has_s) && self.ls_on];
            let mut bl = [(0i32, 0i32); 8];
            let mut npred = 0u32;
            let mut spred = 0u32;
            for i in 1..8 {
                let n = idx(i);
                let (p1, p2) = if has_n { self.edge_pred(&cur, nb, edge, i, &q) } else { (0, 0) };
                let pls = if self.ls_on {
                    let ei = (ucls * 2 + edge) * 8 + i;
                    let mut x = [0f64; 40];
                    for (f, &(src, idx)) in self.els_feats[ei].iter().enumerate() {
                        let idx = idx as usize;
                        x[f] = match src {
                            0 => (a[idx] as i32 * q[idx]) as f64,
                            1 => (l[idx] as i32 * q[idx]) as f64,
                            2 => (al[idx] as i32 * q[idx]) as f64,
                            3 => (ar[idx] as i32 * q[idx]) as f64,
                            4 => (cur[idx] as i32 * q[idx]) as f64,
                            5 => lc[idx] as f64,
                            6 => cc[idx] as f64,
                            _ => 64.0,
                        };
                    }
                    let pr = self.els[ei].predict(&x) * 8.0 / q[n] as f64;
                    if pr.is_finite() { pr.round().clamp(-1e6, 1e6) as i32 } else { 0 }
                } else {
                    0
                };
                pr[i] = [p1, p2, 8 * nb[n] as i32, 8 * side[n] as i32, pc8[n], pr8[n], pls];
                bl[i] = blend(&pr[i], &avail, &self.edge_err[(pcls * 2 + edge) * 8 + i]);
                if tune()[21] == 3 && self.ls_on {
                    bl[i].0 = pls;
                }
                npred += (bl[i].0.unsigned_abs() >= 4) as u32;
                spred += bl[i].0.unsigned_abs() >> 3;
            }
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
                    hash2(110 + edge as u32, (npred << 8) | (mb(spred) << 2) | cls),
                    hash2(120 + edge as u32, (nb_cnt << 8) | (npred << 4) | cls),
                    hash2(130 + edge as u32, (nze_total << 8) | (nzb(nz77) << 2) | cls),
                    hash2(140 + edge as u32, (mb(act) << 8) | (npred << 3) | cls),
                ];
                self.count(coder, cnt_true, &hs, 7, 4 + edge * 2 + ucls, 24 + edge * 4 + npred.min(3) as usize)
            } else {
                0
            };
            nze_total += cnt;
            let mut left = cnt;
            let ea_blk = if has_a { self.planes[ci].eerr[bi - w] } else { [0; 14] };
            let el_blk = if has_l { self.planes[ci].eerr[bi - 1] } else { [0; 14] };
            for i in 1..8 {
                let n = idx(i);
                if left == 0 {
                    cur[n] = 0;
                    continue;
                }
                let [p1, p2, pn, ps, pc, pcc, _] = pr[i];
                let px = if has_cc { pcc } else { pc };
                let (pbl, econf) = bl[i];
                let vn = nb[n] as i32;
                let k = ZZ[n] as u32;
                let inner = if edge == 0 { cur[n + 8] as i32 } else { cur[n + 1] as i32 };
                let conf = mb((econf >> 5) as u32).min(7);
                let lconf = mb(ea_blk[edge * 7 + i - 1] as u32 + el_blk[edge * 7 + i - 1] as u32).min(15);
                let hs: [u32; NCTX] = [
                    hash2(200 + k, (pb(pbl) << 8) | (conf << 4) | (left.min(7) << 1) | cls),
                    hash2(210 + k, (pb(p2) << 8) | (pb(p1) << 2) | cls),
                    hash2(220 + k, (pb(pbl) << 8) | (mb(inner.unsigned_abs()) << 2) | cls),
                    hash2(230 + k, (pb(pn) << 8) | (pb(ps) << 2) | cls),
                    hash2(240 + k, (nzb(nz77) << 8) | (cnt << 4) | (left << 1) | cls),
                    hash2(250 + k, (pb(pbl) << 8) | (lconf << 2) | cls),
                    hash2(260 + k, if luma { (pb(pbl) << 6) | (mb(act) << 1) } else { (pb(px) << 8) | (pb(pbl) << 2) | 1 }),
                    hash2(270 + k, ((clip(r8(pbl), 15) + 16) as u32) << 8 | (conf << 4) | (left.min(7) << 1) | cls),
                    hash2(280 + k, (((clip(vn, 8) + 8) as u32) << 12) | ((clip(inner, 8) + 8) as u32) << 6 | cls),
                    hash2(290 + k, ((clip(pbl >> 2, 63) + 64) as u32) << 2 | cls),
                    hash2(300 + k, (lconf << 4) | (conf << 1) | cls),
                    hash2(310 + k, ((clip(r8(p1), 15) + 16) as u32) << 8 | ((clip(r8(p2), 15) + 16) as u32) << 2 | cls),
                    hash2(320 + k, (pb(pbl) << 10) | (mb(if edge == 0 { act_a } else { act_l }) << 4) | (left.min(7) << 1) | cls),
                    hash2(330 + k, (pb(pn) << 10) | (pb(pbl) << 4) | (left.min(7) << 1) | cls),
                    hash2(340 + k, (((clip(vn, 8) + 8) as u32) << 10) | (pb(pbl) << 4) | cls),
                    hash2(350 + k, (mb(act) << 10) | (conf << 6) | (nzb(nz77) << 2) | cls),
                ];
                for (j, &h) in hs.iter().enumerate() {
                    self.tabs[j].set(h);
                }
                self.sel = [(1 + edge) * 2 + ucls, k as usize * 2 + ucls, 48 + pb(pbl) as usize / 2];
                self.actx = [128 + (pb(pbl) as usize) * 2 + ucls, 256 + (pb(pbl) as usize) * 4 + (conf as usize).min(3)];
                let pc_ = if tune()[21] >= 1 && self.ls_on { pr[i][6] } else if tune()[21] == 2 && self.ls_on { (pbl + pr[i][6]) / 2 } else { pbl };
                self.pred8 = pc_;
                self.spred8 = pc_;
                self.pmu8 = pc_;
                self.ps8 = (econf * tune()[13] / 64).max(2);
                self.dyn_base = PART_EDGE * SLOTS * DYN_B * DYN_C + conf as usize;
                self.part = 6;
                let val = self.value(coder, cur[n] as i32, left != 8 - i as u32);
                cur[n] = val as i16;
                if val != 0 {
                    left -= 1;
                    act += val.unsigned_abs();
                }
            }
            if self.stats.is_some() && has_n && has_s && luma {
                for i in 1..8 {
                    let v8 = 8 * cur[idx(i)] as i32;
                    let d = &mut self.dbg[i];
                    d[0] += v8.unsigned_abs() as f64;
                    for j in 0..4 {
                        d[1 + j] += (v8 - pr[i][j]).unsigned_abs() as f64;
                    }
                    d[5] += (v8 - bl[i].0).unsigned_abs() as f64;
                    d[6] += 1.0;
                }
            }
            if self.ls_on {
                for i in 1..8 {
                    let n = idx(i);
                    self.els[(ucls * 2 + edge) * 8 + i].update((cur[n] as i32 * q[n]) as f64);
                }
            }
            // learn the predictors' errors and record this block's prediction errors
            let errs = &mut self.edge_err[(pcls * 2 + edge) * 8..(pcls * 2 + edge) * 8 + 8];
            for i in 1..8 {
                let v8 = 8 * cur[idx(i)] as i32;
                for j in 0..7 {
                    if avail[j] {
                        track(&mut errs[i][j], v8 - pr[i][j]);
                    }
                }
                eerr[edge * 7 + i - 1] = ((v8 - bl[i].0).unsigned_abs() >> 3).min(255) as u8;
            }
        }

        // ---- 4. DC from the neighbours' boundary pixels and this block's AC
        let pix0 = self.idct(&cur, &q, false);
        let q0 = q[0];
        let mut dp = [0i32; 6];
        let mut dav = [false; 6];
        if has_a {
            let ba = self.planes[ci].border[bi - w];
            let (mut sg, mut s0) = (0i64, 0i64);
            for x in 0..8 {
                let (a7, a6) = (ba[x] as i64, ba[8 + x] as i64);
                let (c0, c1) = (pix0[x] as i64, pix0[8 + x] as i64);
                sg += (3 * a7 - a6) - (3 * c0 - c1);
                s0 += a7 - c0;
            }
            dp[0] = (sg / (2 * q0 as i64)) as i32;
            dp[2] = (s0 / q0 as i64) as i32;
            dav[0] = true;
            dav[2] = true;
        }
        if has_l {
            let bl = self.planes[ci].border[bi - 1];
            let (mut sg, mut s0) = (0i64, 0i64);
            for y in 0..8 {
                let (l7, l6) = (bl[16 + y] as i64, bl[24 + y] as i64);
                let (c0, c1) = (pix0[y * 8] as i64, pix0[y * 8 + 1] as i64);
                sg += (3 * l7 - l6) - (3 * c0 - c1);
                s0 += l7 - c0;
            }
            dp[1] = (sg / (2 * q0 as i64)) as i32;
            dp[3] = (s0 / q0 as i64) as i32;
            dav[1] = true;
            dav[3] = true;
        }
        if has_a || has_l {
            let (da, dl, dal) = (a[0] as i32, l[0] as i32, al[0] as i32);
            dp[4] = 8 * match (has_a, has_l) {
                (true, true) => {
                    let (mx, mn) = (da.max(dl), da.min(dl));
                    let g = da + dl - dal;
                    g.clamp(mn, mx)
                }
                (true, false) => da,
                _ => dl,
            };
            dav[4] = true;
        }
        let dmode = if self.ls_on { tune()[22] } else { 0 };
        if dmode > 0 && (has_a || has_l) {
            let (b0, _) = blend(&dp[..5], &dav[..5], &self.dc_err[ucls][..5]);
            let f = |avail: bool, v: i32| if avail { v as f64 } else { b0 as f64 };
            let x = [
                f(dav[0], dp[0]),
                f(dav[1], dp[1]),
                f(dav[2], dp[2]),
                f(dav[3], dp[3]),
                f(dav[4], dp[4]),
                f(has_a, 8 * a[0] as i32),
                f(has_l, 8 * l[0] as i32),
                f(has_a && has_l, 8 * al[0] as i32),
                f(has_a && bx + 1 < w, 8 * ar[0] as i32),
                64.0,
            ];
            let pr = self.dls[ucls].predict(&x);
            dp[5] = if pr.is_finite() { pr.round().clamp(-1e7, 1e7) as i32 } else { b0 };
            dav[5] = true;
        }
        let (mut pdc, econf) = blend(&dp, &dav, &self.dc_err[ucls]);
        if dmode == 1 && dav[5] {
            pdc = dp[5];
        }
        let pred_dc = r8(pdc);
        let (mut mx, mut mn) = (i32::MIN, i32::MAX);
        for j in 0..4 {
            if dav[j] {
                mx = mx.max(dp[j]);
                mn = mn.min(dp[j]);
            }
        }
        let spread = if mx >= mn { ((mx - mn) >> 3) as u32 } else { 0 };
        let dcv = if dc_on {
            let ra = if has_a { self.planes[ci].dcres[bi - w] as i32 } else { 0 };
            let rl = if has_l { self.planes[ci].dcres[bi - 1] as i32 } else { 0 };
            let ctxr = mb(ra.unsigned_abs() + rl.unsigned_abs());
            let conf = mb((econf >> 5) as u32).min(15);
            let navail = dav.iter().filter(|&&x| x).count() as u32;
            let hs: [u32; NCTX] = [
                hash2(400, (mb(spread) << 8) | (conf << 3) | (navail.min(3) << 1) | cls),
                hash2(401, (ctxr << 8) | (conf << 2) | cls),
                hash2(402, (mb(spread) << 8) | (ctxr << 2) | cls),
                hash2(403, ((clip(ra, 8) + 8) as u32) << 8 | ((clip(rl, 8) + 8) as u32) << 2 | cls),
                hash2(404, (nzb(nz77) << 8) | (nze_total << 2) | cls),
                hash2(405, (mb(act) << 6) | (mb(spread) << 2) | cls),
                hash2(406, if luma { conf << 1 } else { (mb(lact) << 6) | (conf << 1) | 1 }),
                hash2(407, ((pdc - 8 * pred_dc + 4) as u32) << 6 | (conf << 2) | cls),
                hash2(408, ((clip(r8(dp[4]) - pred_dc, 15) + 16) as u32) << 6 | (conf << 2) | cls),
                hash2(409, ((clip(r8(dp[2]) - pred_dc, 15) + 16) as u32) << 8 | ((clip(r8(dp[3]) - pred_dc, 15) + 16) as u32) << 2 | cls),
                hash2(410, ((clip(r8(dp[0]) - pred_dc, 15) + 16) as u32) << 8 | ((clip(r8(dp[1]) - pred_dc, 15) + 16) as u32) << 2 | cls),
                hash2(411, (mb(spread) << 2) | cls),
                hash2(412, (mb(act_a + act_l) << 8) | (conf << 2) | cls),
                hash2(413, (mb(spread) << 8) | (nzb(nz77) << 4) | cls),
                hash2(414, (((pdc - 8 * pred_dc + 4) as u32) << 8) | (mb(spread) << 2) | cls),
                hash2(415, (ctxr << 8) | (mb(act) << 2) | cls),
            ];
            for (j, &h) in hs.iter().enumerate() {
                self.tabs[j].set(h);
            }
            self.sel = [6 + ucls, 128 + ucls, 64 + mb(spread) as usize];
            self.actx = [192 + ucls * 16 + conf as usize, 384 + (ctxr as usize) * 2 + ucls];
            self.pred8 = pdc - 8 * pred_dc;
            self.spred8 = self.pred8;
            self.pmu8 = pdc - 8 * pred_dc;
            self.ps8 = (econf * tune()[14] / 64).max(2);
            self.dyn_base = PART_DC * SLOTS * DYN_B * DYN_C + (conf as usize).min(7);
            self.part = 10;
            let r = self.value(coder, cur[0] as i32 - pred_dc, true);
            let v = pred_dc + r;
            self.planes[ci].dcres[bi] = r.clamp(-32767, 32767) as i16;
            let v8 = 8 * v;
            for j in 0..6 {
                if dav[j] {
                    track(&mut self.dc_err[ucls][j], v8 - dp[j]);
                }
            }
            if dav[5] {
                self.dls[ucls].update(v8 as f64);
            }
            v
        } else {
            cur[0] as i32
        };
        cur[0] = dcv as i16;

        // ---- store
        let pl = &mut self.planes[ci];
        pl.nz77[bi] = nz77 as u8;
        pl.nze[bi] = nze_total as u8;
        pl.eerr[bi] = eerr;
        let dc8 = dcv * q0;
        let mut bd = [0i16; 32];
        for i in 0..8 {
            bd[i] = (pix0[56 + i] + dc8).clamp(-32768, 32767) as i16;
            bd[8 + i] = (pix0[48 + i] + dc8).clamp(-32768, 32767) as i16;
            bd[16 + i] = (pix0[i * 8 + 7] + dc8).clamp(-32768, 32767) as i16;
            bd[24 + i] = (pix0[i * 8 + 6] + dc8).clamp(-32768, 32767) as i16;
        }
        pl.border[bi] = bd;
        let mut mask = 0u64;
        for (k, &n) in NATURAL.iter().enumerate() {
            mask |= ((cur[n] != 0) as u64) << k;
        }
        pl.mask[bi] = mask;
        pl.act[bi] = act.min(65535) as u16;
        if !pl.pix.is_empty() {
            let px = &mut pl.pix[bi];
            for i in 0..64 {
                px[i] = (pix0[i] + dc8).clamp(-32768, 32767) as i16;
            }
        }
        if !luma {
            // this block's contribution to the scales of later blocks
            let mut sums = [0i64; 4];
            for n in 1..64 {
                let c = cur[n] as i64 * q[n] as i64;
                sums[0] += c * lc[n] >> 4;
                sums[1] += lc[n] * lc[n] >> 4;
                sums[2] += c * cc[n] >> 4;
                sums[3] += cc[n] * cc[n] >> 4;
            }
            pl.cfl[bi] = sums;
            let g = &mut self.gcfl[ci];
            for j in 0..4 {
                g[j] += sums[j] - (g[j] >> 4);
            }
        }
        plane[bi * 64..bi * 64 + 64].copy_from_slice(&cur);
    }

    /// Forward DCT of 8x8 pixels (x8 units) into dequantized coefficient units.
    fn fdct(&self, px: &[i32; 64]) -> [i64; 64] {
        let b = &self.b;
        let mut tmp = [0i64; 64];
        for y in 0..8 {
            for u in 0..8 {
                let mut s = 0i64;
                for x in 0..8 {
                    s += b[x][u] as i64 * px[y * 8 + x] as i64;
                }
                tmp[y * 8 + u] = s;
            }
        }
        let mut out = [0i64; 64];
        for v in 0..8 {
            for u in 0..8 {
                let mut s = 0i64;
                for y in 0..8 {
                    s += b[y][v] as i64 * tmp[y * 8 + u];
                }
                out[v * 8 + u] = (s + (1 << 28)) >> 29;
            }
        }
        out
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

/// Sources of the least-squares features of interior position `j` (class 0 luma, 1 chroma):
/// 0 block above, 1 block to the left, 2 above-left, 3 above-right, 4 this block, 5 luma
/// prediction, 6 first-chroma prediction, 7 constant.
fn ls_feats(class: usize, j: usize) -> Vec<(u8, u8)> {
    let n = INTERIOR[j];
    let (u, v) = (n % 8, n / 8);
    let mut f = Vec::new();
    for g in 0..8 {
        f.push((0, (g * 8 + u) as u8));
        f.push((1, (v * 8 + g) as u8));
    }
    for g in 1..8 {
        if ZZ[g * 8 + u] < ZZ[n] {
            f.push((4, (g * 8 + u) as u8));
        }
        if ZZ[v * 8 + g] < ZZ[n] {
            f.push((4, (v * 8 + g) as u8));
        }
    }
    // diagonal neighbours in frequency already coded in this block
    for (du, dv) in [(-1i32, -1i32), (1, -1), (-1, 1)] {
        let (uu, vv) = (u as i32 + du, v as i32 + dv);
        if (1..8).contains(&uu) && (1..8).contains(&vv) && ZZ[(vv * 8 + uu) as usize] < ZZ[n] {
            f.push((4, (vv * 8 + uu) as u8));
        }
    }
    f.push((2, n as u8));
    f.push((3, n as u8));
    if class == 1 {
        f.push((5, n as u8));
        f.push((6, n as u8));
    }
    f.push((7, 0));
    f
}

/// Least-squares features of edge coefficient `i` (edge 0: first row, predicted across the
/// top boundary; edge 1: first column): the neighbour's coefficients in the same column/row,
/// the other neighbour's edge, this block's 7x7 coefficients in that column/row, the corner
/// blocks, chroma predictions and a constant (sources as in `ls_feats`).
fn edge_ls_feats(class: usize, edge: usize, i: usize) -> Vec<(u8, u8)> {
    let at = |f: usize, g: usize| (if edge == 0 { g * 8 + f } else { f * 8 + g }) as u8;
    let (across, along) = if edge == 0 { (0, 1) } else { (1, 0) };
    let mut v = Vec::new();
    for g in 0..8 {
        v.push((across, at(i, g)));
        v.push((along, at(g, 0)));
    }
    for g in 1..8 {
        v.push((4, at(i, g)));
    }
    v.push((2, at(i, 0)));
    v.push((3, at(i, 0)));
    if class == 1 {
        v.push((5, at(i, 0)));
        v.push((6, at(i, 0)));
    }
    v.push((7, 0));
    v
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
