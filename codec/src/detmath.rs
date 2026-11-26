//! exp/ln built only from IEEE-754 basic operations (+, -, *, /), which are
//! correctly rounded on every platform. Platform libm implementations differ in
//! the last bit between macOS, Linux and WebAssembly; any such difference would
//! desynchronise encoder and decoder models, so nothing in the codec may call them.

const LN2_HI: f64 = 6.931_471_803_691_238_164_90e-01;
const LN2_LO: f64 = 1.908_214_929_270_587_700_02e-10;
const LN2: f64 = std::f64::consts::LN_2;
const INV_LN2: f64 = std::f64::consts::LOG2_E;
const SQRT2: f64 = std::f64::consts::SQRT_2;

#[inline]
fn pow2i(k: i32) -> f64 {
    f64::from_bits(((k + 1023) as u64) << 52)
}

pub fn exp(x: f64) -> f64 {
    if x.is_nan() || x < -740.0 {
        return 0.0;
    }
    if x > 709.0 {
        return f64::MAX;
    }
    let k = (x * INV_LN2).round();
    let r = (x - k * LN2_HI) - k * LN2_LO;
    let mut p = 1.0 / 6_227_020_800.0;
    p = p * r + 1.0 / 479_001_600.0;
    p = p * r + 1.0 / 39_916_800.0;
    p = p * r + 1.0 / 3_628_800.0;
    p = p * r + 1.0 / 362_880.0;
    p = p * r + 1.0 / 40_320.0;
    p = p * r + 1.0 / 5_040.0;
    p = p * r + 1.0 / 720.0;
    p = p * r + 1.0 / 120.0;
    p = p * r + 1.0 / 24.0;
    p = p * r + 1.0 / 6.0;
    p = p * r + 0.5;
    p = p * r + 1.0;
    p = p * r + 1.0;
    let ki = k as i32;
    if ki < -1000 {
        p * pow2i(-1000) * pow2i(ki + 1000)
    } else {
        p * pow2i(ki)
    }
}

/// Natural log for finite x > 0 (subnormals are treated as the smallest normal).
pub fn ln(x: f64) -> f64 {
    let x = if x < f64::MIN_POSITIVE { f64::MIN_POSITIVE } else { x };
    let bits = x.to_bits();
    let mut e = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let mut m = f64::from_bits((bits & 0x000f_ffff_ffff_ffff) | 0x3ff0_0000_0000_0000);
    if m > SQRT2 {
        m *= 0.5;
        e += 1;
    }
    let s = (m - 1.0) / (m + 1.0);
    let s2 = s * s;
    let mut p = 1.0 / 23.0;
    p = p * s2 + 1.0 / 21.0;
    p = p * s2 + 1.0 / 19.0;
    p = p * s2 + 1.0 / 17.0;
    p = p * s2 + 1.0 / 15.0;
    p = p * s2 + 1.0 / 13.0;
    p = p * s2 + 1.0 / 11.0;
    p = p * s2 + 1.0 / 9.0;
    p = p * s2 + 1.0 / 7.0;
    p = p * s2 + 1.0 / 5.0;
    p = p * s2 + 1.0 / 3.0;
    p = p * s2 + 1.0;
    let ef = e as f64;
    2.0 * s * p + ef * LN2_HI + ef * LN2_LO
}

#[allow(dead_code)]
pub fn ln2() -> f64 {
    LN2
}

#[cfg(test)]
mod tests {
    #[test]
    fn accuracy() {
        for i in -2000..2000 {
            let x = i as f64 * 0.05;
            let a = super::exp(x);
            let b = x.exp();
            assert!(((a - b) / b).abs() < 1e-14, "exp({x}) {a} {b}");
        }
        for i in 1..5000 {
            let x = i as f64 * 0.013;
            let a = super::ln(x);
            let b = x.ln();
            assert!((a - b).abs() < 1e-14 * (1.0 + b.abs()), "ln({x}) {a} {b}");
        }
    }
}
