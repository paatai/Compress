//! Continuous-tone model: per-sample prediction (blend of many predictors weighted by
//! their recent local errors, plus online least squares), bias cancellation, and
//! residual coding with binary context mixing.

use crate::coder::BitCoder;
use crate::ols::Ols;
use crate::wls::Wls2d;
use crate::primitives::*;

pub const PAD: usize = 4;
const RING: usize = 4;
const SLOTS: usize = 16;
const SLOT_BITS: u32 = 4;
const SLOT_ZERO: usize = 0;
const SLOT_SIGN: usize = 1;
const NPRED: usize = 26;
const NCTX: usize = 11;
const NEXP: usize = 24;
const EXP_BLOCK: usize = 16 * 48 * 2 * SLOTS;
const NIN: usize = NCTX + NEXP + 1;

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
    pub fn for_channels(nc: usize) -> Plan {
        match nc {
            1 => Plan { src: vec![0], refs: vec![vec![]], alpha: None },
            2 => Plan { src: vec![1, 0], refs: vec![vec![], vec![]], alpha: Some(0) },
            3 => Plan { src: vec![1, 0, 2], refs: vec![vec![], vec![0], vec![0, 1]], alpha: None },
            _ => Plan { src: vec![3, 1, 0, 2], refs: vec![vec![], vec![], vec![1], vec![1, 2]], alpha: Some(0) },
        }
    }
}

#[derive(Clone, Debug)]
pub struct Tuning {
    pub mixer_lr: i32,
    pub mixer_lr_final: i32,
    pub ctr_limit: u32,
    pub ctr_limit_exact: u32,
    pub ols_lambda: f64,
    pub ols_nu: f64,
    pub ols_interval: u32,
    pub blend_eps: f32,
    pub param_scale: f64,
    pub apm_rate: u32,
    pub disable: u32,
    pub blend_pow: u32,
    pub wls_on: u32,
    pub wls_lh: f64,
    pub wls_lv: f64,
    pub wls_nu: f64,
    pub stack_on: u32,
    pub stack_lambda: f64,
    pub stack_nu: f64,
    pub vw_mode: u32,
    pub vw_c: f64,
    pub stack_all: u32,
    pub ols_on: u32,
    pub wls_interval: u32,
    pub exp_more: u32,
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning {
            mixer_lr: 400,
            mixer_lr_final: 300,
            ctr_limit: 255,
            ctr_limit_exact: 60,
            ols_lambda: 0.998,
            ols_nu: 0.5,
            ols_interval: 4,
            blend_eps: 0.125,
            param_scale: 1.0,
            apm_rate: 6,
            disable: 0,
            blend_pow: 2,
            wls_on: 15,
            wls_lh: 0.8,
            wls_lv: 0.8,
            wls_nu: 1.0,
            stack_on: 1,
            stack_lambda: 0.999,
            stack_nu: 0.5,
            vw_mode: 7,
            vw_c: 0.5,
            stack_all: 0,
            ols_on: 0,
            wls_interval: 1,
            exp_more: 0,
        }
    }
}

struct Chan {
    vals: Vec<i32>,
    res: Vec<i32>,
    aerr: Vec<i32>,
    perr: Vec<u32>,
    np: usize,
    ols: Ols,
    nols: usize,
    wls: Option<Wls2d>,
    stack: Ols,
    stack_in: Vec<f64>,
    stack_idx: Vec<usize>,
    exp_idx: Vec<usize>,
    bias: Vec<[i32; 2]>,
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
    m: [Mixer; 3],
    mf: Mixer,
    apm1: Apm,
    apm2: Apm,
    x: [i32; NIN],
    sel: [usize; 3],
    self_f: usize,
    actx1: usize,
    actx2: usize,
    // per-row ring offsets
    r0: usize,
    r1: usize,
    r2: usize,
    r3: usize,
    y: usize,
    // per-pixel state of already coded channels
    cur_val: [i32; 4],
    cur_res: [i32; 4],
    ols_in: [f64; 32],
    exp_tab: Vec<u32>,
    exp_base: [usize; NEXP],
    nexp: usize,
    side: usize,
    tune: Tuning,
    pub stats: Option<Box<Stats>>,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub n: u64,
    pub abs_mu: f64,
    pub abs_blend: f64,
    pub abs_ols: f64,
    pub abs_pred: [f64; NPRED],
    pub cost_slot: [f64; SLOTS],
    pub cnt_slot: [u64; SLOTS],
    pub cost_ch: [f64; 4],
    pub abs_ch: [f64; 4],
    pub n_ch: [u64; 4],
    pub cur_ch: usize,
    pub abs_pred0: [f64; NPRED],
    pub abs_blend0: f64,
}

impl Stats {
    pub fn report(&self) -> String {
        let n = self.n.max(1) as f64;
        let mut s = format!(
            "samples {} |e| mu {:.3} blend {:.3} ols {:.3}\n preds:",
            self.n,
            self.abs_mu / n / 16.0,
            self.abs_blend / n / 16.0,
            self.abs_ols / n / 16.0
        );
        for v in self.abs_pred.iter() {
            s += &format!(" {:.2}", v / n / 16.0);
        }
        s += "\n ch0 preds:";
        let n0 = self.n_ch[0].max(1) as f64;
        for v in self.abs_pred0.iter().take(16) {
            s += &format!(" {:.3}", v / n0 / 16.0);
        }
        s += &format!(" blend {:.3}", self.abs_blend0 / n0 / 16.0);
        s += "\n per channel (bits, |e|):";
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

impl CtModel {
    pub fn new(width: usize, height: usize, depth: u32, plan: Plan, tune: Tuning) -> Self {
        let t = tables();
        let maxv = ((1u32 << depth) - 1) as i32;
        let stride = width + 2 * PAD;
        let nc = plan.src.len();
        let mut ch = Vec::with_capacity(nc);
        for c in 0..nc {
            let nref = plan.refs[c].len();
            let use_wls = tune.wls_on & (1 << c) != 0;
            let np = 14 + if nref >= 1 { 7 } else { 0 } + if nref >= 2 { 3 } else { 0 } + 1 + use_wls as usize;
            let mut stack_idx: Vec<usize> = vec![4, 10, 11];
            if nref >= 1 {
                stack_idx.extend_from_slice(&[14, 15, 16, 19]);
            }
            if nref >= 2 {
                stack_idx.extend_from_slice(&[21, 22, 23]);
            }
            let ols_idx = np - 1 - use_wls as usize;
            stack_idx.push(ols_idx);
            if use_wls {
                stack_idx.push(np - 1);
            }
            if tune.stack_all != 0 {
                stack_idx = (0..np).collect();
            }
            let nols = 10 + 5 * nref.min(2) + if plan.alpha.is_some() && plan.alpha != Some(c) { 1 } else { 0 };
            ch.push(Chan {
                vals: vec![(maxv + 1) / 2; RING * stride],
                res: vec![0; RING * stride],
                aerr: vec![32; RING * stride],
                perr: vec![32; RING * stride * NPRED],
                np,
                ols: Ols::new(nols, tune.ols_lambda, tune.ols_nu * (maxv as f64 / 255.0).powi(2), tune.ols_interval),
                nols,
                stack: Ols::new(stack_idx.len() + 1, tune.stack_lambda, tune.stack_nu, tune.ols_interval),
                stack_in: vec![0.0; stack_idx.len() + 1],
                stack_idx,
                exp_idx: {
                    let wl = if use_wls { np - 1 } else { 11 };
                    let mut e = vec![0usize, 1, 2, 4, 6, 10, 11, wl, usize::MAX];
                    if nref >= 1 {
                        e.extend_from_slice(&[14, 15, 16]);
                    } else {
                        e.extend_from_slice(&[5, 7, 9]);
                    }
                    if tune.exp_more != 0 {
                        for j in 0..np {
                            if !e.contains(&j) && e.len() < NEXP {
                                e.push(j);
                            }
                        }
                    }
                    e
                },
                wls: if use_wls {
                    let nw = match nref {
                        0 => 12,
                        1 => 13,
                        _ => 16,
                    };
                    Some(Wls2d::new(nw, width, tune.wls_lh, tune.wls_lv, tune.wls_nu, tune.wls_interval))
                } else {
                    None
                },
                bias: vec![[0, 0]; 4096],
            });
        }
        let lb = if std::env::var_os("KOLMO_SMALL").is_some() { [10u32; 11] } else { [12u32, 16, 16, 18, 16, 16, 18, 18, 18, 14, 16] };
        let samples = (width * height * nc) as u64;
        let cap = (64 - samples.leading_zeros()).max(10);
        let mut tabs = Vec::with_capacity(NCTX);
        for (i, &b) in lb.iter().enumerate() {
            let b = b.min(cap);
            let lim = if (6..=8).contains(&i) { tune.ctr_limit_exact } else { tune.ctr_limit };
            tabs.push(CtxTable::new(b, SLOT_BITS, lim));
        }
        let m = [
            Mixer::new(NIN, SLOTS * 64, tune.mixer_lr, 1 << 13),
            Mixer::new(NIN, SLOTS * 64, tune.mixer_lr, 1 << 13),
            Mixer::new(NIN, SLOTS * 64, tune.mixer_lr, 1 << 13),
        ];
        let mf = Mixer::new(4, SLOTS * 4, tune.mixer_lr_final, 22000);
        let apm1 = Apm::new(SLOTS * 64, tune.apm_rate, t);
        let apm2 = Apm::new(SLOTS * 1024, tune.apm_rate, t);
        CtModel {
            t,
            width,
            maxv,
            dshift: depth.saturating_sub(8),
            stride,
            plan,
            ch,
            tabs,
            m,
            mf,
            apm1,
            apm2,
            x: [0; NIN],
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
            ols_in: [0.0; 32],
            exp_tab: vec![CTR_INIT; nc * NEXP * EXP_BLOCK],
            exp_base: [0; NEXP],
            nexp: 0,
            side: 0,
            tune,
            stats: if std::env::var_os("KOLMO_STATS").is_some() { Some(Box::default()) } else { None },
        }
    }

    fn qlog_inv(q: usize) -> u32 {
        let mut v = 0u32;
        while v < (1 << 24) && (qlog(v) as usize) < q {
            v = v + 1 + v / 8;
        }
        v
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
            if let Some(wl) = c.wls.as_mut() {
                wl.start_row();
            }
            for k in 1..=PAD {
                if y == 0 {
                    continue;
                }
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
            if let Some(wl) = c.wls.as_mut() {
                wl.end_row();
            }
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

    /// Codes (or decodes) one sample of coding position `ci` at column `x`.
    #[inline(never)]
    pub fn code_sample<C: BitCoder>(&mut self, coder: &mut C, x: usize, ci: usize, value: i32, known: bool) -> i32 {
        let xi = x + PAD;
        let (r0, r1, r2, r3) = (self.r0, self.r1, self.r2, self.r3);
        let maxv = self.maxv;
        let ds = self.dshift;
        let nref = self.plan.refs[ci].len();
        let alpha_pos = self.plan.alpha;

        // ---- neighbourhood
        let (w, n, nw, ne, ww, nn, nne, nnw, nww, nee, nnn) = {
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
        {
            let th = 1 << ds;
            let dh = (w - ww).abs() + (n - nw).abs() + (n - ne).abs();
            let dv = (w - nw).abs() + (n - nn).abs() + (ne - nne).abs();
            let g = if dv - dh > 80 * th {
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
            };
            p[11] = g;
        }
        p[12] = (n + nw - nnw) * 16;
        p[13] = (w + nw - nww) * 16;
        let mut k = 14;
        let mut refv = [[0i32; 6]; 2];
        for (ri, &rp) in self.plan.refs[ci].iter().take(2).enumerate() {
            let v = &self.ch[rp].vals;
            let xr = self.cur_val[rp];
            let (wr, nr, nwr, ner) = (v[r0 + xi - 1], v[r1 + xi], v[r1 + xi - 1], v[r1 + xi + 1]);
            refv[ri] = [xr, wr, nr, nwr, ner, 0];
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
        // OLS
        let nols = self.ch[ci].nols;
        {
            let o = &mut self.ols_in;
            o[0] = n as f64;
            o[1] = w as f64;
            o[2] = nw as f64;
            o[3] = ne as f64;
            o[4] = nn as f64;
            o[5] = ww as f64;
            o[6] = nne as f64;
            o[7] = nnw as f64;
            o[8] = nww as f64;
            o[9] = nee as f64;
            let mut j = 10;
            for rv in refv.iter().take(nref.min(2)) {
                for &v in rv.iter().take(5) {
                    o[j] = v as f64;
                    j += 1;
                }
            }
            if j < nols {
                o[j] = match alpha_pos {
                    Some(a) => self.cur_val[a] as f64,
                    None => 0.0,
                };
            }
        }
        let po16 = if self.tune.ols_on != 0 {
            let po = self.ch[ci].ols.predict(&self.ols_in[..nols]);
            (po * 16.0).clamp(0.0, m16 as f64) as i32
        } else {
            p[11]
        };
        p[k] = po16;
        k += 1;
        let www = self.ch[ci].vals[r0 + xi - 3];
        if let Some(wl) = self.ch[ci].wls.as_mut() {
            let inp: [f64; 16] = match nref {
                0 => [w as f64, n as f64, nw as f64, ne as f64, ww as f64, nn as f64, nnw as f64, nne as f64, nww as f64, nee as f64, www as f64, nnn as f64, 0.0, 0.0, 0.0, 0.0],
                _ => {
                    let a = &refv[0];
                    let b = &refv[1];
                    [w as f64, n as f64, nw as f64, ne as f64, ww as f64, nn as f64, nne as f64, nww as f64, a[0] as f64, a[1] as f64, a[2] as f64, a[3] as f64, a[4] as f64, b[0] as f64, b[1] as f64, b[2] as f64]
                }
            };
            p[k] = match wl.predict(x, &inp) {
                Some(v) => (v * 16.0).clamp(0.0, m16 as f64) as i32,
                None => p[10],
            };
            k += 1;
        }
        let np = k;
        debug_assert_eq!(np, self.ch[ci].np);
        for v in p[..np].iter_mut() {
            *v = (*v).clamp(0, m16);
        }

        // ---- blend by local errors
        let chn = &self.ch[ci];
        let pe = &chn.perr;
        let iw = (r0 + xi - 1) * NPRED;
        let in_ = (r1 + xi) * NPRED;
        let inw = (r1 + xi - 1) * NPRED;
        let ine = (r1 + xi + 1) * NPRED;
        let iww = (r0 + xi - 2) * NPRED;
        let inn = (r2 + xi) * NPRED;
        let eps = self.tune.blend_eps * 16.0;
        let bpow = self.tune.blend_pow;
        let mut wts = [0.0f32; NPRED];
        let mut errs = [0u32; NPRED];
        let mut sw = 0.0f32;
        let mut swp = 0.0f32;
        let mut best_e = u32::MAX;
        let dis = self.tune.disable;
        for j in 0..np {
            if (dis & 1 != 0 && j == np - 1 - (self.ch[ci].wls.is_some() as usize)) || (dis & 2 != 0 && j >= 14 && j < np - 1) {
                continue;
            }
            let e = 2 * pe[iw + j] + 2 * pe[in_ + j] + pe[inw + j] + pe[ine + j] + pe[iww + j] + pe[inn + j];
            errs[j] = e;
            best_e = best_e.min(e);
            let ef = e as f32 * 0.125 + eps;
            let wgt = match bpow {
                1 => 1.0 / ef,
                2 => 1.0 / (ef * ef),
                3 => 1.0 / (ef * ef * ef),
                _ => {
                    let e2 = ef * ef;
                    1.0 / (e2 * e2)
                }
            };
            wts[j] = wgt;
            sw += wgt;
            swp += wgt * p[j] as f32;
        }
        let blend16 = (swp / sw) as i32;
        let blend16 = if self.tune.stack_on != 0 {
            let c = &mut self.ch[ci];
            let ns = c.stack_idx.len();
            for j in 0..ns {
                c.stack_in[j] = p[c.stack_idx[j]] as f64 * (1.0 / 16.0);
            }
            c.stack_in[ns] = blend16 as f64 * (1.0 / 16.0);
            let sp = c.stack.predict(&c.stack_in);
            (sp * 16.0).clamp(0.0, m16 as f64) as i32
        } else {
            blend16
        };
        let chn = &self.ch[ci];
        let mut spread = 0.0f32;
        for j in 0..np {
            let d = (p[j] - blend16) as f32;
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
        let sb = {
            let b = blend16;
            ((w * 16 > b) as u32)
                | (((n * 16 > b) as u32) << 1)
                | (((nw * 16 > b) as u32) << 2)
                | (((ne * 16 > b) as u32) << 3)
                | (((ww * 16 > b) as u32) << 4)
                | (((nn * 16 > b) as u32) << 5)
        };
        let ref_q = if nref > 0 { qr(self.cur_res[self.plan.refs[ci][0]], ds) } else { 4 };
        let bctx = ((qsc * 64 + sb) * 9 + ref_q) as usize & 4095;
        let bb = chn.bias[bctx];
        let bias = if bb[1] > 0 { bb[0] / bb[1] } else { 0 };
        let mu16 = (blend16 + bias).clamp(0, m16);
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
        self.sel = [qsc as usize, (frac as usize) | ((ci.min(3)) << 3), (rq0 as usize) + 9 * (qr(rw, ds) as usize % 7)];
        self.self_f = ci.min(3);
        self.actx1 = cq.min(63);
        self.actx2 = (hash2(c32 | 0xB00, (qr(rw, ds) << 8) | (qr(rn, ds) << 4) | rq0) & 1023) as usize;
        let _ = best_e;
        {
            let chn = &self.ch[ci];
            let ne_ = chn.exp_idx.len().min(NEXP);
            let blend_err = s16 * 8;
            for e in 0..ne_ {
                let j = chn.exp_idx[e];
                let (pj, ej) = if j == usize::MAX { (blend16, blend_err) } else { (p[j], errs[j]) };
                let conf = (qlog((ej >> 3) >> ds) / 3).min(15) as usize;
                let qd = qdev(pj - pv * 16, ds);
                let b = ((ci * NEXP + e) * 16 + conf) * (48 * 2 * SLOTS) + qd * 2 * SLOTS;
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
            st.abs_blend += (v16 - blend16).abs() as f64;
            st.abs_ols += (v16 - po16).abs() as f64;
            for j in 0..np.min(NPRED) {
                st.abs_pred[j] += pe[j] as f64;
                if ci == 0 {
                    st.abs_pred0[j] += pe[j] as f64;
                }
            }
            if ci == 0 {
                st.abs_blend0 += (v16 - blend16).abs() as f64;
            }
        }
        let vw = {
            let sc = s16 as f64 / 16.0 / (1 << ds) as f64 + self.tune.vw_c;
            1.0 / (sc * sc)
        };
        let vm = self.tune.vw_mode;
        if self.tune.ols_on != 0 {
            chn.ols.update_w(v as f64, if vm & 2 != 0 { vw } else { 1.0 });
        }
        if self.tune.stack_on != 0 {
            chn.stack.update_w(v as f64, if vm & 1 != 0 { vw } else { 1.0 });
        }
        if let Some(wl) = chn.wls.as_mut() {
            wl.update(x, v as f64, if vm & 4 != 0 { vw } else { 1.0 });
        }
        let bbm = &mut chn.bias[bctx];
        bbm[0] += v16 - blend16;
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
        let dis = self.tune.disable;
        if dis & 4 != 0 {
            self.x[6] = 0;
            self.x[7] = 0;
            self.x[8] = 0;
        }
        if dis & 16 != 0 {
            for i in [1usize, 2, 3, 4, 5, 9, 10] {
                self.x[i] = 0;
            }
        }
        let sl = self.side * SLOTS + slot;
        for e in 0..NEXP {
            self.x[NCTX + e] = if e < self.nexp { t.stretch(ctr_p16(unsafe { *self.exp_tab.get_unchecked(self.exp_base[e] + sl) })) } else { 0 };
        }
        if dis & 32 != 0 {
            for e in 0..NEXP {
                self.x[NCTX + e] = 0;
            }
        }
        self.x[NCTX + NEXP] = 256;
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
            let c = st.cur_ch;
            st.cost_ch[c] -= pp.log2();
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
        self.mf.update(&fin, bit);
        self.apm1.update(bit);
        self.apm2.update(bit);
        bit
    }

    fn code_residual<C: BitCoder>(&mut self, coder: &mut C, r_enc: i32, pv: i32) -> i32 {
        let maxv = self.maxv;
        let z = self.code_bit(coder, SLOT_ZERO, (r_enc == 0) as u32);
        if z == 1 {
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
            let bit = self.code_bit(coder, slot_unary(i), (mag_enc >= next_lo) as u32);
            if bit == 0 {
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
            let bit = self.code_bit(coder, slot_mant(i, i - 1 - j), (mag_enc >= mid) as u32);
            if bit == 1 {
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
