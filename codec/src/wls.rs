//! Least-squares predictor trained on a 2-D causal window around the current pixel.
//!
//! The normal equations are accumulated with separable exponential weights:
//! previous rows are filtered horizontally in both directions (lambda_h^|dx|) and
//! accumulated vertically per column (lambda_v^dy); the current row contributes the
//! pixels to the left. This costs O(n^2) per pixel instead of O(n^2 * window).
//! Matrices are stored as packed lower triangles of the augmented (n+1)x(n+1)
//! system whose last row holds the cross terms with the target.

const MAXN: usize = 20;
const MAXTRI: usize = (MAXN + 1) * (MAXN + 2) / 2;

pub struct Wls2d {
    n: usize,
    m: usize,
    tri: usize,
    width: usize,
    lh: f64,
    lv: f64,
    nu: f64,
    vcol: Vec<f64>,
    rowz: Vec<f64>,
    tmp: Vec<f64>,
    h: Vec<f64>,
    w: [f64; MAXN],
    cur: [f64; MAXN + 1],
    ok: bool,
    interval: u32,
    cnt: u32,
}

/// Dot product with four fixed-order partial sums: the same operations in the same order on
/// every target (so results are bit-identical), but with independent chains the CPU can overlap.
#[inline(always)]
fn dot4(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let (a, b) = (&a[..n], &b[..n]);
    let mut s = [0.0f64; 4];
    let mut k = 0;
    while k + 4 <= n {
        s[0] += a[k] * b[k];
        s[1] += a[k + 1] * b[k + 1];
        s[2] += a[k + 2] * b[k + 2];
        s[3] += a[k + 3] * b[k + 3];
        k += 4;
    }
    let mut t = (s[0] + s[1]) + (s[2] + s[3]);
    while k < n {
        t += a[k] * b[k];
        k += 1;
    }
    t
}

#[inline(always)]
fn acc(dst: &mut [f64], lam: f64, z: &[f64], m: usize) {
    let mut k = 0;
    for i in 0..m {
        let zi = z[i];
        let row = &mut dst[k..k + i + 1];
        let zz = &z[..i + 1];
        for j in 0..=i {
            row[j] = lam * row[j] + zi * zz[j];
        }
        k += i + 1;
    }
}

impl Wls2d {
    pub fn new(n: usize, width: usize, lh: f64, lv: f64, nu: f64, interval: u32) -> Self {
        assert!(n <= MAXN);
        let m = n + 1;
        let tri = m * (m + 1) / 2;
        Wls2d {
            n,
            m,
            tri,
            width,
            lh,
            lv,
            nu,
            vcol: vec![0.0; width * tri],
            rowz: vec![0.0; width * m],
            tmp: vec![0.0; width * tri],
            h: vec![0.0; tri],
            w: [0.0; MAXN],
            cur: [0.0; MAXN + 1],
            ok: false,
            interval: interval.max(1),
            cnt: 0,
        }
    }

    pub fn start_row(&mut self) {
        self.h.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Predicts from regressors `inp` (length n) for column x. Returns None until trained.
    pub fn predict(&mut self, x: usize, inp: &[f64]) -> Option<f64> {
        let n = self.n;
        self.cur[..n].copy_from_slice(&inp[..n]);
        self.cnt += 1;
        if !(self.ok && self.cnt < self.interval) {
            self.cnt = 0;
            self.ok = self.solve(x);
            if !self.ok {
                return None;
            }
        }
        let mut p = 0.0;
        for i in 0..n {
            p += self.w[i] * self.cur[i];
        }
        Some(p)
    }

    fn solve(&mut self, x: usize) -> bool {
        let n = self.n;
        let tri = self.tri;
        let v = &self.vcol[x * tri..(x + 1) * tri];
        let h = &self.h[..tri];
        let ntri = n * (n + 1) / 2;
        let mut a = [0.0f64; MAXTRI];
        for k in 0..ntri {
            a[k] = v[k] + h[k];
        }
        let mut b = [0.0f64; MAXN];
        for j in 0..n {
            b[j] = v[ntri + j] + h[ntri + j];
        }
        let mut trace = 0.0;
        for i in 0..n {
            trace += a[i * (i + 1) / 2 + i];
        }
        if !(trace > 0.0) {
            return false;
        }
        let reg = self.nu + trace * 1e-6;
        let mut inv = [0.0f64; MAXN];
        for i in 0..n {
            let ri = i * (i + 1) / 2;
            for j in 0..=i {
                let rj = j * (j + 1) / 2;
                let mut s = a[ri + j] - dot4(&a[ri..ri + j], &a[rj..rj + j]);
                if i == j {
                    s += reg;
                    if !(s > 1e-12) {
                        return false;
                    }
                    let d = s.sqrt();
                    a[ri + i] = d;
                    inv[i] = 1.0 / d;
                } else {
                    a[ri + j] = s * inv[j];
                }
            }
        }
        let mut z = [0.0f64; MAXN];
        for i in 0..n {
            let ri = i * (i + 1) / 2;
            let mut s = b[i];
            for k in 0..i {
                s -= a[ri + k] * z[k];
            }
            z[i] = s * inv[i];
        }
        for i in (0..n).rev() {
            let mut s = z[i];
            for k in i + 1..n {
                s -= a[k * (k + 1) / 2 + i] * self.w[k];
            }
            self.w[i] = s * inv[i];
        }
        true
    }

    /// Registers the coded value of the sample at column x with a training weight.
    pub fn update(&mut self, x: usize, y: f64, wt: f64) {
        let m = self.m;
        self.cur[m - 1] = y;
        let sw = wt.sqrt();
        let row = &mut self.rowz[x * m..(x + 1) * m];
        for i in 0..m {
            row[i] = self.cur[i] * sw;
        }
        acc(&mut self.h, self.lh, row, m);
    }

    /// After a row: filter it horizontally (two-sided) and fold into the per-column accumulators.
    pub fn end_row(&mut self) {
        let (m, tri, w) = (self.m, self.tri, self.width);
        let lh = self.lh;
        let lv = self.lv;
        let mut run = [0.0f64; MAXTRI];
        for x in 0..w {
            let z = &self.rowz[x * m..(x + 1) * m];
            acc(&mut run[..tri], lh, z, m);
            self.tmp[x * tri..(x + 1) * tri].copy_from_slice(&run[..tri]);
        }
        run.iter_mut().for_each(|v| *v = 0.0);
        for x in (0..w).rev() {
            let t = &self.tmp[x * tri..(x + 1) * tri];
            let vc = &mut self.vcol[x * tri..(x + 1) * tri];
            for k in 0..tri {
                vc[k] = lv * vc[k] + t[k] + lh * run[k];
            }
            let z = &self.rowz[x * m..(x + 1) * m];
            acc(&mut run[..tri], lh, z, m);
        }
    }
}
