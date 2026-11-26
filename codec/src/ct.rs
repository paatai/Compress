//! Continuous-tone model for photographs and other images with many colours.
//!
//! Per sample: about two dozen predictors (spatial and cross-channel), a least-squares
//! predictor fitted on a 2-D causal window, an online least-squares "stack" that combines the
//! strongest of them, and context-based bias cancellation give an integer prediction P.
//! The residual x - P is coded as binary decisions (zero? sign? magnitude bucket? bits inside
//! the bucket). Each decision is predicted by context counters, one "expert" counter per
//! predictor (indexed by how far that predictor is from P and how reliable it has been
//! locally), three gated logistic mixers, a final mixer and two APMs.

use crate::coder::BitCoder;
use crate::primitives::*;
use crate::wls::Wls2d;
use crate::ols::Ols;

pub const PAD: usize = 4;
const RING: usize = 4;
const SLOTS: usize = 16;
const SLOT_BITS: u32 = 4;
const SLOT_ZERO: usize = 0;
const SLOT_SIGN: usize = 1;
const NPRED: usize = 26;
const NCTX: usize = 11;
const NEXP: usize = 12;
const EXP_CONF: usize = 8;
const EXP_QD: usize = 24;
const EXP_BLOCK: usize = EXP_CONF * EXP_QD * 2 * SLOTS;
const NIN: usize = NCTX + NEXP + 1;
const NSYN: usize = 10;
const NIN_MAX: usize = NIN + 2 * NSYN;
const BLEND: usize = usize::MAX;

#[inline(always)]
fn slot_unary(i: u32) -> usize {
    2 + (i.min(6) as usize)
}

#[inline(always)]
fn slot_mant(b: u32, j: u32) -> usize {
    match b {
        1 => 9,
        2 => 10 + j as usize,
        3 => 12 + j.min(1) as usize,
        _ => 14 + j.min(1) as usize,
    }
}

/// ~4 steps per octave pseudo-logarithm.
#[inline(always)]
fn qlog(v: u32) -> u32 {
    let v = v + 1;
    let lz = 31 - v.leading_zeros();
    let frac = if lz >= 2 { (v >> (lz - 2)) & 3 } else { (v << (2 - lz)) & 3 };
    lz * 4 + frac
}

/// Signed residual class in 0..=8.
#[inline(always)]
fn qr(r: i32, shift: u32) -> u32 {
    let a = (r.unsigned_abs()) >> shift;
    let q = if a == 0 {
        0
    } else if a == 1 {
        1
    } else if a <= 3 {
        2
    } else if a <= 7 {
        3
    } else {
        4
    };
    if r < 0 {
        4 - q
    } else {
        4 + q
    }
}

/// Signed, finely quantised offset of a prediction from the integer prediction (x16 units), 1..=46.
#[inline(always)]
fn qdev(d: i32, shift: u32) -> usize {
    let u = d.unsigned_abs() >> shift;
    let l = if u < 8 {
        u >> 1
    } else if u < 32 {
        4 + ((u - 8) >> 2)
    } else if u < 128 {
        10 + ((u - 32) >> 4)
    } else if u < 512 {
        16 + ((u - 128) >> 6)
    } else {
        22
    } as usize;
    if d < 0 {
        23 - l
    } else {
        24 + l
    }
}

#[inline(always)]
fn med(a: i32, b: i32, c: i32) -> i32 {
    let mx = a.max(b);
    let mn = a.min(b);
    if c >= mx {
        mn
    } else if c <= mn {
        mx
    } else {
        a + b - c
    }
}

#[derive(Clone, Debug)]
pub struct Plan {
    /// coding position -> interleaved channel index
    pub src: Vec<usize>,
    /// coding position -> earlier coding positions used for inter-channel prediction
    pub refs: Vec<Vec<usize>>,
    /// coding position of an alpha channel, if any
    pub alpha: Option<usize>,
}

impl Plan {
    /// Green first, then red from green, then blue from green and red; alpha before colour.
    pub fn for_channels(nc: usize) -> Plan {
        match nc {
            1 => Plan { src: vec![0], refs: vec![vec![]], alpha: None },
            2 => Plan { src: vec![1, 0], refs: vec![vec![], vec![]], alpha: Some(0) },
            3 => Plan { src: vec![1, 0, 2], refs: vec![vec![], vec![0], vec![0, 1]], alpha: None },
            _ => Plan { src: vec![3, 1, 0, 2], refs: vec![vec![], vec![], vec![1], vec![1, 2]], alpha: Some(0) },
        }
    }
}

/// Model parameters. The defaults are what the file format uses; other values are only for
/// experiments (encoder and decoder must agree).
#[derive(Clone, Debug)]
pub struct Tuning {
    pub mixer_lr: i32,
    pub mixer_lr_final: i32,
    pub ctr_limit: u32,
    pub ctr_limit_exact: u32,
    pub blend_eps: f32,
    pub apm_rate: u32,
    pub wls_lh: f64,
    pub wls_lv: f64,
    pub wls_nu: f64,
    pub wls_interval: u32,
    pub wls_interval_ref: u32,
    pub stack_lambda: f64,
    pub stack_nu: f64,
    pub stack_interval: u32,
    pub vw_c: f64,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            mixer_lr: 400,
            mixer_lr_final: 300,
            ctr_limit: 255,
            ctr_limit_exact: 60,
            blend_eps: 0.125,
            apm_rate: 6,
            wls_lh: 0.8,
            wls_lv: 0.8,
            wls_nu: 1.0,
            wls_interval: 1,
            wls_interval_ref: 1,
            stack_lambda: 0.999,
            stack_nu: 0.5,
            stack_interval: 4,
            vw_c: 0.5,
        }
    }
}

struct Chan {
    vals: Vec<i32>,
    res: Vec<i32>,
    aerr: Vec<i32>,
    perr: Vec<u32>,
    np: usize,
    wls: Wls2d,
    stack: Ols,
    stack_in: Vec<f64>,
    stack_idx: Vec<usize>,
    exp_idx: Vec<usize>,
    bias: Vec<[i32; 2]>,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub n: u64,
    pub abs_mu: f64,
    pub cost_slot: [f64; SLOTS],
    pub cnt_slot: [u64; SLOTS],
    pub cost_ch: [f64; 4],
    pub abs_ch: [f64; 4],
    pub n_ch: [u64; 4],
    pub cur_ch: usize,
}

impl Stats {
    pub fn report(&self) -> String {
        let n = self.n.max(1) as f64;
        let mut s = format!("samples {} mean |e| {:.3}\n per channel (bits, |e|):", self.n, self.abs_mu / n / 16.0);
        for c in 0..4 {
            if self.n_ch[c] > 0 {
                s += &format!(" [{c}: {:.4} {:.3}]", self.cost_ch[c] / self.n_ch[c] as f64, self.abs_ch[c] / self.n_ch[c] as f64 / 16.0);
            }
        }
        let total: f64 = self.cost_slot.iter().sum();
        s += &format!("\n bits/sample {:.4}  by slot:", total / n);
        for (i, (c, k)) in self.cost_slot.iter().zip(self.cnt_slot.iter()).enumerate() {
            if *k > 0 {
                s += &format!(" [{i}: {:.3}b/s {:.3}b/dec]", c / n, c / *k as f64);
            }
        }
        s
    }
}

pub struct CtModel {
    t: &'static Tables,
    width: usize,
    maxv: i32,
    dshift: u32,
    stride: usize,
    plan: Plan,
    ch: Vec<Chan>,
    tabs: Vec<CtxTable>,
    syn: Vec<HistTable>,
    m: [Mixer; 3],
    mf: Mixer,
    apm1: Apm,
    apm2: Apm,
    x: [i32; NIN_MAX],
    sel: [usize; 3],
    self_f: usize,
    actx1: usize,
    actx2: usize,
    r0: usize,
    r1: usize,
    r2: usize,
    r3: usize,
    y: usize,
    cur_val: [i32; 4],
    cur_res: [i32; 4],
    exp_tab: Vec<u32>,
    exp_base: [usize; NEXP],
    nexp: usize,
    side: usize,
    tune: Tuning,
    pub stats: Option<Box<Stats>>,
}

impl CtModel {
    /// `synth` enables extra exact-value contexts for screenshots and graphics.
    pub fn new(width: usize, height: usize, depth: u32, plan: Plan, tune: Tuning, synth: bool) -> Self {
        let t = tables();
        let maxv = ((1u32 << depth) - 1) as i32;
        let stride = width + 2 * PAD;
        let nc = plan.src.len();
        let mut ch = Vec::with_capacity(nc);
        for c in 0..nc {
            let nref = plan.refs[c].len();
            let np = 14 + if nref >= 1 { 7 } else { 0 } + if nref >= 2 { 3 } else { 0 } + 1;
            let wls_idx = np - 1;
            let mut stack_idx: Vec<usize> = vec![4, 10, 11];
            if nref >= 1 {
                stack_idx.extend_from_slice(&[14, 15, 16, 19]);
            }
            if nref >= 2 {
                stack_idx.extend_from_slice(&[21, 22, 23]);
            }
            stack_idx.push(wls_idx);
            let mut exp_idx = vec![0usize, 1, 2, 4, 6, 10, 11, wls_idx, BLEND];
            exp_idx.extend_from_slice(if nref >= 1 { &[14, 15, 16] } else { &[5, 7, 9] });
            let nw = match nref {
                0 => 12,
                1 => 13,
                _ => 16,
            };
            let interval = if nref > 0 { tune.wls_interval_ref } else { tune.wls_interval };
            ch.push(Chan {
                vals: vec![(maxv + 1) / 2; RING * stride],
                res: vec![0; RING * stride],
                aerr: vec![32; RING * stride],
                perr: vec![32; RING * stride * NPRED],
                np,
                wls: Wls2d::new(nw, width, tune.wls_lh, tune.wls_lv, tune.wls_nu, interval),
                stack: Ols::new(stack_idx.len() + 1, tune.stack_lambda, tune.stack_nu, tune.stack_interval),
                stack_in: vec![0.0; stack_idx.len() + 1],
                stack_idx,
                exp_idx,
                bias: vec![[0, 0]; 4096],
            });
        }
        let lb = [12u32, 16, 16, 18, 16, 16, 18, 18, 18, 14, 16];
        let samples = width as u64 * height as u64 * nc as u64;
        let cap = (64 - samples.leading_zeros()).max(10);
        let tabs = lb
            .iter()
            .enumerate()
            .map(|(i, &b)| CtxTable::new(b.min(cap), SLOT_BITS, if (6..=8).contains(&i) { tune.ctr_limit_exact } else { tune.ctr_limit }))
            .collect();
        let nin = if synth { NIN_MAX } else { NIN };
        let syn: Vec<HistTable> = if synth { (0..NSYN).map(|_| HistTable::new(cap.min(18), SLOT_BITS)).collect() } else { Vec::new() };
        let m = [
            Mixer::new(nin, SLOTS * 64, tune.mixer_lr, 1 << 13),
            Mixer::new(nin, SLOTS * 64, tune.mixer_lr, 1 << 13),
            Mixer::new(nin, SLOTS * 64, tune.mixer_lr, 1 << 13),
        ];
        CtModel {
            t,
            width,
            maxv,
            dshift: depth.saturating_sub(8),
            stride,
            plan,
            ch,
            tabs,
            syn,
            m,
            mf: Mixer::new(4, SLOTS * 4, tune.mixer_lr_final, 22000),
            apm1: Apm::new(SLOTS * 64, tune.apm_rate, t),
            apm2: Apm::new(SLOTS * 1024, tune.apm_rate, t),
            x: [0; NIN_MAX],
            sel: [0; 3],
            self_f: 0,
            actx1: 0,
            actx2: 0,
            r0: 0,
            r1: 0,
            r2: 0,
            r3: 0,
            y: 0,
            cur_val: [0; 4],
            cur_res: [0; 4],
            exp_tab: vec![CTR_INIT; nc * NEXP * EXP_BLOCK],
            exp_base: [0; NEXP],
            nexp: 0,
            side: 0,
            tune,
            stats: if std::env::var_os("KOLMO_STATS").is_some() { Some(Box::default()) } else { None },
        }
    }

    pub fn start_row(&mut self, y: usize) {
        self.y = y;
        let s = self.stride;
        self.r0 = (y % RING) * s;
        self.r1 = ((y + RING - 1) % RING) * s;
        self.r2 = ((y + RING - 2) % RING) * s;
        self.r3 = ((y + RING - 3) % RING) * s;
        let (r0, r1) = (self.r0, self.r1);
        for c in self.ch.iter_mut() {
            c.wls.start_row();
            if y == 0 {
                continue;
            }
            // left padding of this row repeats the first pixel of the row above
            for k in 1..=PAD {
                c.vals[r0 + PAD - k] = c.vals[r1 + PAD];
                c.res[r0 + PAD - k] = c.res[r1 + PAD];
                c.aerr[r0 + PAD - k] = c.aerr[r1 + PAD];
                for p in 0..NPRED {
                    c.perr[(r0 + PAD - k) * NPRED + p] = c.perr[(r1 + PAD) * NPRED + p];
                }
            }
        }
    }

    pub fn end_row(&mut self) {
        let r0 = self.r0;
        let w = self.width;
        for c in self.ch.iter_mut() {
            c.wls.end_row();
            let last = r0 + PAD + w - 1;
            for k in 1..=PAD {
                c.vals[last + k] = c.vals[last];
                c.res[last + k] = c.res[last];
                c.aerr[last + k] = c.aerr[last];
                for p in 0..NPRED {
                    c.perr[(last + k) * NPRED + p] = c.perr[last * NPRED + p];
                }
            }
        }
    }

    /// For row 0, replicate the coded sample into the virtual rows above so that
    /// N/NE/NW of the next sample equal W.
    fn fill_top(&mut self, ci: usize, xi: usize) {
        if self.y != 0 {
            return;
        }
        let s = self.stride;
        let c = &mut self.ch[ci];
        let src = self.r0 + xi;
        let (v, r, a) = (c.vals[src], c.res[src], c.aerr[src]);
        for row in 1..RING {
            let base = row * s;
            for dx in 0..4 {
                let i = base + xi + dx;
                if i >= base + s {
                    break;
                }
                c.vals[i] = v;
                c.res[i] = r;
                c.aerr[i] = a;
                for p in 0..NPRED {
                    c.perr[i * NPRED + p] = c.perr[src * NPRED + p];
                }
            }
        }
    }

    /// Codes (or decodes) one sample of coding position `ci` at column `x`. With `known`
    /// the value is not coded (it was resolved by the pixel model) but all state is updated.
    #[inline(never)]
    pub fn code_sample<C: BitCoder>(&mut self, coder: &mut C, x: usize, ci: usize, value: i32, known: bool) -> i32 {
        let xi = x + PAD;
        let (r0, r1, r2, r3) = (self.r0, self.r1, self.r2, self.r3);
        let maxv = self.maxv;
        let ds = self.dshift;
        let nref = self.plan.refs[ci].len();
        let alpha_pos = self.plan.alpha;

        // ---- neighbourhood
        let (w, n, nw, ne, ww, nn, nne, nnw, nww, nee, nnn, www) = {
            let v = &self.ch[ci].vals;
            (
                v[r0 + xi - 1],
                v[r1 + xi],
                v[r1 + xi - 1],
                v[r1 + xi + 1],
                v[r0 + xi - 2],
                v[r2 + xi],
                v[r2 + xi + 1],
                v[r2 + xi - 1],
                v[r1 + xi - 2],
                v[r1 + xi + 2],
                v[r3 + xi],
                v[r0 + xi - 3],
            )
        };

        // ---- predictors (fixed point x16)
        let m16 = maxv * 16;
        let mut p = [0i32; NPRED];
        p[0] = w * 16;
        p[1] = n * 16;
        p[2] = ne * 16;
        p[3] = nw * 16;
        p[4] = (w + n - nw) * 16;
        p[5] = (w + ne - n) * 16;
        p[6] = (n + ne - nne) * 16;
        p[7] = (w + ne) * 8;
        p[8] = (2 * n - nn) * 16;
        p[9] = (2 * w - ww) * 16;
        p[10] = med(w, n, nw) * 16;
        p[11] = {
            // CALIC gradient-adjusted prediction
            let th = 1 << ds;
            let dh = (w - ww).abs() + (n - nw).abs() + (n - ne).abs();
            let dv = (w - nw).abs() + (n - nn).abs() + (ne - nne).abs();
            if dv - dh > 80 * th {
                w * 16
            } else if dh - dv > 80 * th {
                n * 16
            } else {
                let mut g = (w + n) * 8 + (ne - nw) * 4;
                if dv - dh > 32 * th {
                    g = (g + w * 16) / 2;
                } else if dv - dh > 8 * th {
                    g = (3 * g + w * 16) / 4;
                } else if dh - dv > 32 * th {
                    g = (g + n * 16) / 2;
                } else if dh - dv > 8 * th {
                    g = (3 * g + n * 16) / 4;
                }
                g
            }
        };
        p[12] = (n + nw - nnw) * 16;
        p[13] = (w + nw - nww) * 16;
        let mut k = 14;
        let mut refv = [[0i32; 5]; 2];
        for (ri, &rp) in self.plan.refs[ci].iter().take(2).enumerate() {
            let v = &self.ch[rp].vals;
            let xr = self.cur_val[rp];
            let (wr, nr, nwr, ner) = (v[r0 + xi - 1], v[r1 + xi], v[r1 + xi - 1], v[r1 + xi + 1]);
            refv[ri] = [xr, wr, nr, nwr, ner];
            p[k] = (w + xr - wr) * 16;
            p[k + 1] = (n + xr - nr) * 16;
            p[k + 2] = (w + n - nw + xr - (wr + nr - nwr)) * 16;
            if ri == 0 {
                p[k + 3] = (ne + xr - ner) * 16;
                p[k + 4] = (nw + xr - nwr) * 16;
                p[k + 5] = (xr + med(w - wr, n - nr, nw - nwr)) * 16;
                p[k + 6] = (w + ne - wr - ner) * 8 + xr * 16;
                k += 7;
            } else {
                k += 3;
            }
        }
        // 2-D windowed least squares
        let inp: [f64; 16] = if nref == 0 {
            [w as f64, n as f64, nw as f64, ne as f64, ww as f64, nn as f64, nnw as f64, nne as f64, nww as f64, nee as f64, www as f64, nnn as f64, 0.0, 0.0, 0.0, 0.0]
        } else {
            let (a, b) = (&refv[0], &refv[1]);
            [w as f64, n as f64, nw as f64, ne as f64, ww as f64, nn as f64, nne as f64, nww as f64, a[0] as f64, a[1] as f64, a[2] as f64, a[3] as f64, a[4] as f64, b[0] as f64, b[1] as f64, b[2] as f64]
        };
        p[k] = match self.ch[ci].wls.predict(x, &inp) {
            Some(v) => (v * 16.0).clamp(0.0, m16 as f64) as i32,
            None => p[10],
        };
        k += 1;
        let np = k;
        debug_assert_eq!(np, self.ch[ci].np);
        for v in p[..np].iter_mut() {
            *v = (*v).clamp(0, m16);
        }

        // ---- blend by local errors (weights 1 / (error + eps)^2)
        let chn = &self.ch[ci];
        let pe = &chn.perr;
        let iw = (r0 + xi - 1) * NPRED;
        let in_ = (r1 + xi) * NPRED;
        let inw = (r1 + xi - 1) * NPRED;
        let ine = (r1 + xi + 1) * NPRED;
        let iww = (r0 + xi - 2) * NPRED;
        let inn = (r2 + xi) * NPRED;
        let eps = self.tune.blend_eps * 16.0;
        let mut wts = [0.0f32; NPRED];
        let mut errs = [0u32; NPRED];
        let mut sw = 0.0f32;
        let mut swp = 0.0f32;
        for j in 0..np {
            let e = 2 * pe[iw + j] + 2 * pe[in_ + j] + pe[inw + j] + pe[ine + j] + pe[iww + j] + pe[inn + j];
            errs[j] = e;
            let ef = e as f32 * 0.125 + eps;
            let wgt = 1.0 / (ef * ef);
            wts[j] = wgt;
            sw += wgt;
            swp += wgt * p[j] as f32;
        }
        let blend = (swp / sw) as i32;

        // ---- stacking: online least squares over the strongest predictors and the blend
        let base16 = {
            let c = &mut self.ch[ci];
            let ns = c.stack_idx.len();
            for j in 0..ns {
                c.stack_in[j] = p[c.stack_idx[j]] as f64 * (1.0 / 16.0);
            }
            c.stack_in[ns] = blend as f64 * (1.0 / 16.0);
            (c.stack.predict(&c.stack_in) * 16.0).clamp(0.0, m16 as f64) as i32
        };
        let chn = &self.ch[ci];
        let mut spread = 0.0f32;
        for j in 0..np {
            let d = (p[j] - base16) as f32;
            spread += wts[j] * d * d;
        }
        let spread16 = (spread / sw).sqrt() as u32;

        // ---- local error energy of the final predictor
        let ae = &chn.aerr;
        let rs = &chn.res;
        let s16 = ((2 * ae[r0 + xi - 1] + 2 * ae[r1 + xi] + ae[r1 + xi - 1] + ae[r1 + xi + 1] + ae[r0 + xi - 2] + ae[r2 + xi]) / 8) as u32;
        let (rw, rn, rnw, rne, rww, rnn) = (rs[r0 + xi - 1], rs[r1 + xi], rs[r1 + xi - 1], rs[r1 + xi + 1], rs[r0 + xi - 2], rs[r2 + xi]);

        // ---- bias cancellation
        let qs = qlog(s16 >> ds).min(47);
        let qsc = qs / 6;
        let sb = ((w * 16 > base16) as u32)
            | (((n * 16 > base16) as u32) << 1)
            | (((nw * 16 > base16) as u32) << 2)
            | (((ne * 16 > base16) as u32) << 3)
            | (((ww * 16 > base16) as u32) << 4)
            | (((nn * 16 > base16) as u32) << 5);
        let ref_q = if nref > 0 { qr(self.cur_res[self.plan.refs[ci][0]], ds) } else { 4 };
        let bctx = ((qsc * 64 + sb) * 9 + ref_q) as usize & 4095;
        let bb = chn.bias[bctx];
        let bias = if bb[1] > 0 { bb[0] / bb[1] } else { 0 };
        let mu16 = (base16 + bias).clamp(0, m16);
        let pv = ((mu16 + 8) >> 4).min(maxv);
        let frac = (mu16 - pv * 16 + 8).clamp(0, 15) as u32 >> 1;
        let cq = (qs as usize).min(63);

        // ---- contexts
        let c32 = ci as u32;
        let qsp = qlog(spread16 >> ds).min(31);
        let dev = {
            let th = (s16 as i32 / 2).max(8);
            let mut d = 0u32;
            for &j in &[0usize, 1, 2, 4, 11, np - 1] {
                let dd = p[j] - mu16;
                d = d * 3 + if dd < -th { 0 } else if dd > th { 2 } else { 1 };
            }
            d
        };
        let aq = match alpha_pos {
            Some(a) if a != ci => {
                let av = self.cur_val[a];
                if av == 0 {
                    1
                } else if av == maxv {
                    2
                } else {
                    3
                }
            }
            _ => 0,
        };
        let (rq0, rq1) = if nref > 0 {
            let r0v = self.cur_res[self.plan.refs[ci][0]];
            let r1v = if nref > 1 { self.cur_res[self.plan.refs[ci][1]] } else { 0 };
            (qr(r0v, ds), qr(r1v, ds))
        } else {
            (qr(rnn, ds), 4)
        };
        let refx = if nref > 0 { self.cur_val[self.plan.refs[ci][0]] } else { 0 };
        let h = [
            hash2(c32, qs),
            hash2(c32 | 0x100, (qsc << 12) | (frac << 8) | sb | (aq << 16)),
            hash2(c32 | 0x200, (qsc << 12) | (qr(rw, ds) << 8) | (qr(rn, ds) << 4) | (frac >> 1)),
            hash2(c32 | 0x300, (qsc << 16) | (qr(rne, ds) << 12) | (qr(rnw, ds) << 8) | (qr(rww, ds) << 4) | qr(rnn, ds)),
            hash2(c32 | 0x400, (qsc << 12) | (rq0 << 8) | (rq1 << 4) | (frac >> 1) | (aq << 16)),
            hash2(c32 | 0x500, (qsc << 16) | dev),
            hash_slice(&[c32 as i32, w, n, nw, ne, pv]),
            hash_slice(&[c32 as i32 | 0x10, w, n, pv]),
            hash_slice(&[c32 as i32 | 0x20, ww, w, nw, n, ne, nn, refx, pv]),
            hash2(c32 | 0x900, (qs << 8) | qsp),
            hash2(c32 | 0xA00, (qs << 8) | (rq0 << 4) | qr(rw, ds)),
        ];
        for (tb, &hh) in self.tabs.iter_mut().zip(h.iter()) {
            tb.set(hh);
        }
        if !self.syn.is_empty() {
            // exact values together with the pixel's already coded channels
            let (p1, p2) = if nref > 0 {
                let a = self.cur_val[self.plan.refs[ci][0]];
                let b = if nref > 1 { self.cur_val[self.plan.refs[ci][1]] } else { -1 };
                (a, b)
            } else {
                (-1, -1)
            };
            let (wr, nr) = if nref > 0 { (refv[0][1], refv[0][2]) } else { (0, 0) };
            let c = ci as i32;
            let hs = [
                hash_slice(&[c, pv, p1, p2]),
                hash_slice(&[c | 0x10, pv, w, p1, p2]),
                hash_slice(&[c | 0x20, pv, n, p1, p2]),
                hash_slice(&[c | 0x30, pv, w, n, nw]),
                hash_slice(&[c | 0x40, pv, n, ne, p1]),
                hash_slice(&[c | 0x50, pv, w, nw, n, ne, p1, p2]),
                hash_slice(&[c | 0x60, pv, w, ww, p1]),
                hash_slice(&[c | 0x70, pv, n, nn, p1]),
                hash_slice(&[c | 0x80, pv, w - wr, n - nr, p1]),
                hash_slice(&[c | 0x90, pv, ne, nne, nw, p1]),
            ];
            for (tb, &hh) in self.syn.iter_mut().zip(hs.iter()) {
                tb.set(hh);
            }
        }
        self.sel = [qsc as usize, (frac as usize) | ((ci.min(3)) << 3), (rq0 as usize) + 9 * (qr(rw, ds) as usize % 7)];
        self.self_f = ci.min(3);
        self.actx1 = cq;
        self.actx2 = (hash2(c32 | 0xB00, (qr(rw, ds) << 8) | (qr(rn, ds) << 4) | rq0) & 1023) as usize;
        {
            // one expert per selected predictor: (its local error, its offset from P)
            let chn = &self.ch[ci];
            let ne_ = chn.exp_idx.len().min(NEXP);
            for e in 0..ne_ {
                let j = chn.exp_idx[e];
                let (pj, ej) = if j == BLEND { (base16, s16 * 8) } else { (p[j], errs[j]) };
                let conf = (qlog((ej >> 3) >> ds) / 6).min(EXP_CONF as u32 - 1) as usize;
                let qd = (qdev(pj - pv * 16, ds) - 1) / 2;
                let b = ((ci * NEXP + e) * EXP_CONF + conf) * (EXP_QD * 2 * SLOTS) + qd * 2 * SLOTS;
                self.exp_base[e] = b;
                prefetch(unsafe { self.exp_tab.as_ptr().add(b) });
                prefetch(unsafe { self.exp_tab.as_ptr().add(b + SLOTS) });
            }
            self.nexp = ne_;
        }
        self.side = 0;

        // ---- code the residual
        if let Some(st) = self.stats.as_mut() {
            st.cur_ch = ci;
        }
        let r_enc = value - pv;
        let r = if known { r_enc } else { self.code_residual(coder, r_enc, pv) };
        let v = pv + r;

        // ---- updates
        let chn = &mut self.ch[ci];
        let i0 = r0 + xi;
        chn.vals[i0] = v;
        chn.res[i0] = r;
        let v16 = v * 16;
        let ae = (v16 - mu16).abs();
        chn.aerr[i0] = ae;
        let pe = &mut chn.perr[i0 * NPRED..i0 * NPRED + np];
        for j in 0..np {
            pe[j] = (v16 - p[j]).unsigned_abs();
        }
        if let Some(st) = self.stats.as_mut() {
            st.n += 1;
            st.n_ch[ci] += 1;
            st.abs_ch[ci] += ae as f64;
            st.abs_mu += ae as f64;
        }
        // least-squares training weight: inverse local variance, so that smooth areas count
        // as much as edges (the coding cost is logarithmic in the error)
        let vw = {
            let sc = s16 as f64 / 16.0 / (1 << ds) as f64 + self.tune.vw_c;
            1.0 / (sc * sc)
        };
        chn.stack.update_w(v as f64, vw);
        chn.wls.update(x, v as f64, vw);
        let bbm = &mut chn.bias[bctx];
        bbm[0] += v16 - base16;
        bbm[1] += 1;
        if bbm[1] >= 64 {
            bbm[0] /= 2;
            bbm[1] /= 2;
        }
        self.cur_val[ci] = v;
        self.cur_res[ci] = r;
        self.fill_top(ci, xi);
        v
    }

    #[inline(always)]
    fn code_bit<C: BitCoder>(&mut self, coder: &mut C, slot: usize, bit: u32) -> u32 {
        let t = self.t;
        for (i, tb) in self.tabs.iter().enumerate() {
            self.x[i] = t.stretch(tb.p(slot));
        }
        let sl = self.side * SLOTS + slot;
        for e in 0..NEXP {
            self.x[NCTX + e] = if e < self.nexp { t.stretch(ctr_p16(unsafe { *self.exp_tab.get_unchecked(self.exp_base[e] + sl) })) } else { 0 };
        }
        self.x[NCTX + NEXP] = 256;
        for (i, tb) in self.syn.iter().enumerate() {
            let (a, r) = tb.inputs(slot, t);
            self.x[NIN + 2 * i] = a;
            self.x[NIN + 2 * i + 1] = r;
        }
        let d0 = self.m[0].mix(&self.x, self.sel[0] * SLOTS + slot, t);
        let d1 = self.m[1].mix(&self.x, self.sel[1] * SLOTS + slot, t);
        let d2 = self.m[2].mix(&self.x, self.sel[2] * SLOTS + slot, t);
        let fin = [d0, d1, d2, 256];
        let df = self.mf.mix(&fin, self.self_f * SLOTS + slot, t);
        let pa1 = self.apm1.pp(df, self.actx1 * SLOTS + slot);
        let pa2 = self.apm2.pp(df, self.actx2 * SLOTS + slot);
        let p = clamp_p((2 * self.mf.pr + pa1 + pa2) >> 2);
        let bit = coder.code(bit, p);
        if let Some(st) = self.stats.as_mut() {
            let pp = if bit != 0 { p as f64 / 65536.0 } else { 1.0 - p as f64 / 65536.0 };
            st.cost_slot[slot] -= pp.log2();
            st.cost_ch[st.cur_ch] -= pp.log2();
            st.cnt_slot[slot] += 1;
        }
        let rc = &t.recip;
        for tb in self.tabs.iter_mut() {
            tb.update(slot, bit, rc);
        }
        let lim = self.tune.ctr_limit;
        for e in 0..self.nexp {
            unsafe { ctr_update(self.exp_tab.get_unchecked_mut(self.exp_base[e] + sl), bit, lim, rc) };
        }
        let x = self.x;
        self.m[0].update(&x, bit);
        self.m[1].update(&x, bit);
        self.m[2].update(&x, bit);
        for tb in self.syn.iter_mut() {
            tb.update(slot, bit, rc);
        }
        self.mf.update(&fin, bit);
        self.apm1.update(bit);
        self.apm2.update(bit);
        bit
    }

    /// Binarisation: zero flag, sign, unary over magnitude buckets [2^i, 2^(i+1)), then the
    /// bits inside the bucket MSB first. Decisions forced by the value range are skipped.
    fn code_residual<C: BitCoder>(&mut self, coder: &mut C, r_enc: i32, pv: i32) -> i32 {
        let maxv = self.maxv;
        if self.code_bit(coder, SLOT_ZERO, (r_enc == 0) as u32) == 1 {
            return 0;
        }
        let pos = if pv == 0 {
            true
        } else if pv == maxv {
            false
        } else {
            self.code_bit(coder, SLOT_SIGN, (r_enc > 0) as u32) == 1
        };
        let maxmag = if pos { maxv - pv } else { pv };
        self.side = pos as usize;
        let mag_enc = r_enc.abs();
        let mut i = 0u32;
        loop {
            let next_lo = 1i32 << (i + 1);
            if next_lo > maxmag {
                break;
            }
            if self.code_bit(coder, slot_unary(i), (mag_enc >= next_lo) as u32) == 0 {
                break;
            }
            i += 1;
        }
        let mut lo = 1i32 << i;
        let mut hi = ((1i32 << (i + 1)) - 1).min(maxmag);
        for j in (0..i).rev() {
            let mid = lo + (1 << j);
            if mid > hi {
                continue;
            }
            if self.code_bit(coder, slot_mant(i, i - 1 - j), (mag_enc >= mid) as u32) == 1 {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        if pos {
            lo
        } else {
            -lo
        }
    }
}
