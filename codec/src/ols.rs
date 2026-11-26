//! Online least-squares linear predictor with exponential forgetting.

pub struct Ols {
    n: usize,
    lambda: f64,
    nu: f64,
    a: Vec<f64>,
    b: Vec<f64>,
    w: Vec<f64>,
    l: Vec<f64>,
    x: Vec<f64>,
    z: Vec<f64>,
    cnt: u32,
    interval: u32,
}

impl Ols {
    pub fn new(n: usize, lambda: f64, nu: f64, interval: u32) -> Self {
        let mut w = vec![0.0; n];
        if n > 0 {
            w[0] = 0.5;
        }
        if n > 1 {
            w[1] = 0.5;
        }
        Ols {
            n,
            lambda,
            nu,
            a: vec![0.0; n * n],
            b: vec![0.0; n],
            w,
            l: vec![0.0; n * n],
            x: vec![0.0; n],
            z: vec![0.0; n],
            cnt: 0,
            interval,
        }
    }

    #[inline]
    pub fn predict(&mut self, inp: &[f64]) -> f64 {
        let n = self.n;
        self.x[..n].copy_from_slice(&inp[..n]);
        let mut s = 0.0;
        for i in 0..n {
            s += self.w[i] * self.x[i];
        }
        s
    }

    #[inline]
    pub fn update(&mut self, y: f64) {
        self.update_w(y, 1.0);
    }

    /// Update with a per-sample weight (e.g. inverse local variance).
    #[inline]
    pub fn update_w(&mut self, y: f64, wt: f64) {
        let n = self.n;
        let lam = self.lambda;
        for i in 0..n {
            let xi = self.x[i] * wt;
            let row = &mut self.a[i * n..i * n + i + 1];
            let xs = &self.x[..i + 1];
            for j in 0..=i {
                row[j] = lam * row[j] + xi * xs[j];
            }
            self.b[i] = lam * self.b[i] + y * xi;
        }
        self.cnt += 1;
        if self.cnt >= self.interval {
            self.cnt = 0;
            self.solve();
        }
    }

    fn solve(&mut self) {
        let n = self.n;
        let mut inv = [0.0f64; 64];
        for i in 0..n {
            for j in 0..=i {
                let mut s = self.a[i * n + j];
                if i == j {
                    s += self.nu;
                }
                let li = &self.l[i * n..i * n + j];
                let lj = &self.l[j * n..j * n + j];
                for k in 0..j {
                    s -= li[k] * lj[k];
                }
                if i == j {
                    if !(s > 1e-9) {
                        return;
                    }
                    let d = s.sqrt();
                    self.l[i * n + i] = d;
                    inv[i] = 1.0 / d;
                } else {
                    self.l[i * n + j] = s * inv[j];
                }
            }
        }
        for i in 0..n {
            let mut s = self.b[i];
            for k in 0..i {
                s -= self.l[i * n + k] * self.z[k];
            }
            self.z[i] = s * inv[i];
        }
        for i in (0..n).rev() {
            let mut s = self.z[i];
            for k in i + 1..n {
                s -= self.l[k * n + i] * self.w[k];
            }
            self.w[i] = s * inv[i];
        }
    }
}
