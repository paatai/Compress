//! Fast regeneration of JPEG scan data from coefficients.
//!
//! Produces exactly the bytes of `format::encode_scan` (libjpeg's encoder rules), but walks the
//! nonzero coefficients of each block through a bit mask in zigzag order instead of testing all
//! 64, writes each code together with its value bits, and moves whole 32-bit words to the
//! output when they contain no 0xFF byte. The choices the coefficients do not determine come
//! from a `Side`, which can be decoded before any scan (unlike the adaptive side coder of the
//! default mode), so scans and stripes can be regenerated independently.

use super::format::{Frame, Huff, Scan, ScanSide, NATURAL};

/// What a scan's coefficients do not determine, as the fast container stores it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Side {
    /// padding bits of every flush; empty if they were all ones (what libjpeg writes)
    pub pads: Vec<u8>,
    /// bytes after the final flush, before the next marker
    pub garbage: Vec<u8>,
    /// end-of-band events of a progressive AC scan (numbered over the scan) at which the
    /// encoder had a choice and decided against libjpeg's rule
    pub eob_flips: Vec<u32>,
}

/// Zigzag index of each natural (row-major) position.
pub const ZIGZAG: [u8; 64] = {
    let mut t = [0u8; 64];
    let mut k = 0;
    while k < 64 {
        t[NATURAL[k]] = k as u8;
        k += 1;
    }
    t
};

/// `ZZ[b][m]`: the zigzag-order bits of natural positions 8b..8b+8 whose bits are set in m.
static ZZ: [[u64; 256]; 8] = {
    let mut t = [[0u64; 256]; 8];
    let mut b = 0;
    while b < 8 {
        let mut m = 0;
        while m < 256 {
            let mut v = 0u64;
            let mut j = 0;
            while j < 8 {
                if m & (1 << j) != 0 {
                    v |= 1u64 << ZIGZAG[b * 8 + j];
                }
                j += 1;
            }
            t[b][m] = v;
            m += 1;
        }
        b += 1;
    }
    t
};

/// Bit k (zigzag order) set where |coefficient| >= `thr` (1 <= thr <= 2^14).
#[inline(always)]
pub fn zz_mask(blk: &[i16; 64], thr: i16) -> u64 {
    let m = nat_mask(blk, thr);
    let mut z = 0u64;
    for b in 0..8 {
        z |= ZZ[b][((m >> (8 * b)) & 0xFF) as usize];
    }
    z
}

#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn nat_mask(blk: &[i16; 64], thr: i16) -> u64 {
    use core::arch::x86_64::*;
    // SAFETY: SSE2 is part of x86-64; `blk` is 64 i16 (eight unaligned 128-bit loads).
    unsafe {
        let p = blk.as_ptr() as *const __m128i;
        let hi = _mm_set1_epi16(thr - 1);
        let lo = _mm_set1_epi16(1 - thr);
        let mut m = 0u64;
        for j in 0..4 {
            let a = _mm_loadu_si128(p.add(2 * j));
            let b = _mm_loadu_si128(p.add(2 * j + 1));
            let ma = _mm_or_si128(_mm_cmpgt_epi16(a, hi), _mm_cmplt_epi16(a, lo));
            let mb = _mm_or_si128(_mm_cmpgt_epi16(b, hi), _mm_cmplt_epi16(b, lo));
            m |= (_mm_movemask_epi8(_mm_packs_epi16(ma, mb)) as u32 as u64) << (16 * j);
        }
        m
    }
}

#[cfg(all(target_arch = "wasm32", target_feature = "simd128"))]
#[inline(always)]
fn nat_mask(blk: &[i16; 64], thr: i16) -> u64 {
    use core::arch::wasm32::*;
    // SAFETY: `blk` is 64 i16 (eight unaligned 128-bit loads).
    unsafe {
        let p = blk.as_ptr() as *const v128;
        let hi = i16x8_splat(thr - 1);
        let lo = i16x8_splat(1 - thr);
        let mut m = 0u64;
        for j in 0..8 {
            let a = v128_load(p.add(j));
            let ma = v128_or(i16x8_gt(a, hi), i16x8_lt(a, lo));
            m |= (i16x8_bitmask(ma) as u64) << (8 * j);
        }
        m
    }
}

#[cfg(not(any(target_arch = "x86_64", all(target_arch = "wasm32", target_feature = "simd128"))))]
#[inline(always)]
fn nat_mask(blk: &[i16; 64], thr: i16) -> u64 {
    let mut m = 0u64;
    for (i, &v) in blk.iter().enumerate() {
        m |= (((v as i32).unsigned_abs() >= thr as u32) as u64) << i;
    }
    m
}

#[inline(always)]
fn bitlen(v: u32) -> u32 {
    32 - v.leading_zeros()
}

/// Huffman codes for encoding: code << 8 | length (0 if the symbol has no code).
pub struct Codes([u32; 256]);

impl Codes {
    pub fn new(h: &Huff) -> Codes {
        let mut t = [0u32; 256];
        for (s, e) in t.iter_mut().enumerate() {
            *e = ((h.code[s] as u32) << 8) | h.len[s] as u32;
        }
        Codes(t)
    }
}

/// The value bits of a coefficient of bit length n (JPEG: negative values as v - 1).
#[inline(always)]
fn vbits(v: i32, n: u32) -> u32 {
    (v + (v >> 31)) as u32 & ((1u32 << n) - 1)
}

/// Bit writer with JPEG byte stuffing. Pending bits sit at the top of `c` (fewer than 8 between
/// calls); every `put` stores all 8 bytes of `c` at the end of the output and keeps the
/// complete ones, so no branch depends on how many bits have accumulated.
struct Bw<'a> {
    out: &'a mut Vec<u8>,
    c: u64,
    n: u32,
}

/// Bytes `put` may write past the end of the output (8 stored, up to 4 kept, and stuffing).
const SLACK: usize = 16;
/// The top n bytes of a u64.
const TOP: [u64; 9] = [0, 0xFF << 56, 0xFFFF << 48, 0xFF_FFFF << 40, 0xFFFF_FFFF << 32, 0xFF_FFFF_FFFF << 24, 0xFFFF_FFFF_FFFF << 16, 0xFF_FFFF_FFFF_FFFF << 8, u64::MAX];

impl Bw<'_> {
    fn new(out: &mut Vec<u8>) -> Bw<'_> {
        out.reserve(4096);
        Bw { out, c: 0, n: 0 }
    }
    /// Makes room for at least `bytes` more output.
    #[inline(always)]
    fn room(&mut self, bytes: usize) {
        if self.out.capacity() - self.out.len() < bytes + SLACK {
            self.out.reserve(bytes + SLACK + self.out.len() / 2);
        }
    }
    /// Appends `len` (1 to 32) bits `v` (< 2^len); needs `room` for 8 bytes.
    #[inline(always)]
    fn put(&mut self, v: u32, len: u32) {
        debug_assert!((1..=32).contains(&len) && self.n < 8 && (v as u64) >> len == 0);
        self.n += len;
        self.c |= (v as u64) << (64 - self.n);
        let nb = (self.n >> 3) as usize;
        let len0 = self.out.len();
        debug_assert!(self.out.capacity() - len0 >= 8);
        // SAFETY: `room` keeps at least SLACK (> 8) bytes of spare capacity, and only the first
        // `nb` stored bytes become part of the vector.
        unsafe {
            std::ptr::copy_nonoverlapping(self.c.to_be_bytes().as_ptr(), self.out.as_mut_ptr().add(len0), 8);
        }
        // flags bytes that are 0xFF (and possibly bytes above one), only in the kept bytes
        let x = !self.c;
        let ff = x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080 & TOP[nb];
        if ff == 0 {
            // SAFETY: the `nb` bytes were written above, within capacity.
            unsafe { self.out.set_len(len0 + nb) };
        } else {
            for &b in &self.c.to_be_bytes()[..nb] {
                self.out.push(b);
                if b == 0xFF {
                    self.out.push(0);
                }
            }
        }
        self.c <<= 8 * nb;
        self.n &= 7;
    }
    #[inline(always)]
    fn sym(&mut self, t: &Codes, s: usize) -> Result<(), String> {
        let e = t.0[s & 255];
        if e & 0xFF == 0 {
            return Err("symbol missing from Huffman table".into());
        }
        self.put(e >> 8, e & 0xFF);
        Ok(())
    }
    /// A symbol followed by `n` value bits (`v` < 2^n).
    #[inline(always)]
    fn sym_bits(&mut self, t: &Codes, s: usize, v: u32, n: u32) -> Result<(), String> {
        let e = t.0[s & 255];
        if e & 0xFF == 0 {
            return Err("symbol missing from Huffman table".into());
        }
        self.put(((e >> 8) << n) | v, (e & 0xFF) + n);
        Ok(())
    }
    fn pad_len(&self) -> u32 {
        (8 - self.n) % 8
    }
    /// Pads to a byte boundary with `pad` (`pad_len` bits).
    fn flush(&mut self, pad: u32) {
        let n = self.pad_len();
        if n > 0 {
            self.room(8);
            self.put(pad, n);
        }
    }
}

/// Source of the encoder's free choices: while compressing it replays the recorded `ScanSide`
/// and writes down the `Side` that reproduces it; while decompressing it reads the `Side`.
pub struct Choices<'a> {
    rec: Option<&'a ScanSide>,
    pub side: Side,
    pad_i: usize,
    eob_i: u32,
    flip_i: usize,
}

impl<'a> Choices<'a> {
    pub fn record(rec: &'a ScanSide) -> Self {
        let side = Side { pads: if rec.pads_not_ones { rec.pads.clone() } else { Vec::new() }, garbage: rec.garbage.clone(), eob_flips: Vec::new() };
        Choices { rec: Some(rec), side, pad_i: 0, eob_i: 0, flip_i: 0 }
    }
    pub fn replay(side: Side) -> Self {
        Choices { rec: None, side, pad_i: 0, eob_i: 0, flip_i: 0 }
    }
    fn pad(&mut self, n: u32) -> u32 {
        let ones = (1u32 << n) - 1;
        let v = if self.side.pads.is_empty() { ones } else { self.side.pads.get(self.pad_i).copied().unwrap_or(0xFF) as u32 & ones };
        self.pad_i += 1;
        v
    }
    /// At an end-of-band event where the run may continue: does it end here?
    fn eob_end(&mut self, libjpeg: bool) -> bool {
        let i = self.eob_i;
        match self.rec {
            Some(r) => {
                let v = r.eob_end.get(i as usize).copied().unwrap_or(true);
                if v != libjpeg {
                    self.side.eob_flips.push(i);
                }
                v
            }
            None => {
                let flip = self.side.eob_flips.get(self.flip_i) == Some(&i);
                self.flip_i += flip as usize;
                libjpeg ^ flip
            }
        }
    }
}

struct Prog {
    eobrun: u32,
    be: Vec<u8>,
}

fn emit_eobrun(w: &mut Bw, ac: &Codes, st: &mut Prog) -> Result<(), String> {
    if st.eobrun > 0 {
        let n = bitlen(st.eobrun) - 1;
        if n > 14 {
            return Err("end-of-band run too long".into());
        }
        w.sym_bits(ac, (n << 4) as usize, st.eobrun & ((1 << n) - 1), n)?;
        st.eobrun = 0;
        for &b in &st.be {
            w.put(b as u32, 1);
        }
        st.be.clear();
    }
    Ok(())
}

fn end_of_band(w: &mut Bw, ac: &Codes, st: &mut Prog, free: bool, libjpeg: bool, ch: &mut Choices) -> Result<(), String> {
    let forced = st.eobrun == 0x7FFF;
    let flush = if forced || !free { forced } else { ch.eob_end(libjpeg) };
    ch.eob_i += 1;
    if flush {
        emit_eobrun(w, ac, st)?;
    }
    Ok(())
}

#[inline(always)]
fn encode_dc(w: &mut Bw, h: &Codes, v: i32, pred: &mut i32) -> Result<(), String> {
    let d = v - *pred;
    *pred = v;
    let n = bitlen(d.unsigned_abs());
    if n > 15 {
        return Err("DC difference out of range".into());
    }
    w.sym_bits(h, n as usize, vbits(d, n), n)
}

#[inline(always)]
fn encode_seq(w: &mut Bw, dc: &Codes, ac: &Codes, blk: &[i16; 64], pred: &mut i32) -> Result<(), String> {
    w.room(512);
    encode_dc(w, dc, blk[0] as i32, pred)?;
    let mut mask = zz_mask(blk, 1) & !1;
    let mut k = 0u32;
    while mask != 0 {
        let z = mask.trailing_zeros();
        let mut r = z - k - 1;
        while r > 15 {
            w.sym(ac, 0xF0)?;
            r -= 16;
        }
        let v = blk[NATURAL[z as usize]] as i32;
        let n = bitlen(v.unsigned_abs());
        w.sym_bits(ac, ((r << 4) + n) as usize, vbits(v, n), n)?;
        k = z;
        mask &= mask - 1;
    }
    if k < 63 {
        w.sym(ac, 0)?;
    }
    Ok(())
}

/// Bits ss..=se.
#[inline(always)]
fn band(ss: usize, se: usize) -> u64 {
    (u64::MAX >> (63 - se)) & (u64::MAX << ss)
}

/// Does a block emit no symbol in a progressive AC scan (so it can only extend a run)?
#[inline(always)]
fn pure(blk: &[i16; 64], s: &Scan) -> bool {
    let b = band(s.ss, s.se);
    if s.ah == 0 {
        zz_mask(blk, 1 << s.al) & b == 0
    } else {
        zz_mask(blk, 1 << s.al) & !zz_mask(blk, 2 << s.al) & b == 0
    }
}

fn encode_ac_first(w: &mut Bw, s: &Scan, ac: &Codes, blk: &[i16; 64], st: &mut Prog, free: bool, ch: &mut Choices) -> Result<(), String> {
    w.room(512 + st.be.len() / 2);
    let mut mask = zz_mask(blk, 1 << s.al) & band(s.ss, s.se);
    let mut k = s.ss as u32;
    while mask != 0 {
        let z = mask.trailing_zeros();
        emit_eobrun(w, ac, st)?;
        let mut r = z - k;
        while r > 15 {
            w.sym(ac, 0xF0)?;
            r -= 16;
        }
        let v = blk[NATURAL[z as usize]] as i32;
        let a = v.unsigned_abs() >> s.al;
        let n = bitlen(a);
        w.sym_bits(ac, ((r << 4) + n) as usize, (if v < 0 { !a } else { a }) & ((1 << n) - 1), n)?;
        k = z + 1;
        mask &= mask - 1;
    }
    if k <= s.se as u32 {
        st.eobrun += 1;
        end_of_band(w, ac, st, free, false, ch)?;
    }
    Ok(())
}

fn encode_ac_refine(w: &mut Bw, s: &Scan, ac: &Codes, blk: &[i16; 64], st: &mut Prog, free: bool, ch: &mut Choices, br: &mut Vec<u8>) -> Result<(), String> {
    w.room(512 + st.be.len() / 2);
    let b = band(s.ss, s.se);
    let m1 = zz_mask(blk, 1 << s.al) & b;
    let m2 = zz_mask(blk, 2 << s.al) & b;
    let newly = m1 & !m2;
    let eob = if newly != 0 { 63 - newly.leading_zeros() } else { 0 };
    let mut mask = m1;
    let mut r = 0u32;
    let mut next = s.ss as u32;
    br.clear();
    while mask != 0 {
        let z = mask.trailing_zeros();
        r += z - next;
        next = z + 1;
        while r > 15 && z <= eob {
            emit_eobrun(w, ac, st)?;
            w.sym(ac, 0xF0)?;
            r -= 16;
            for &x in br.iter() {
                w.put(x as u32, 1);
            }
            br.clear();
        }
        let v = blk[NATURAL[z as usize]] as i32;
        if m2 >> z & 1 != 0 {
            br.push(((v.unsigned_abs() >> s.al) & 1) as u8);
        } else {
            emit_eobrun(w, ac, st)?;
            w.sym_bits(ac, ((r << 4) + 1) as usize, (v >= 0) as u32, 1)?;
            for &x in br.iter() {
                w.put(x as u32, 1);
            }
            br.clear();
            r = 0;
        }
        mask &= mask - 1;
    }
    r += s.se as u32 + 1 - next;
    if r > 0 || !br.is_empty() {
        st.eobrun += 1;
        st.be.extend_from_slice(br);
        let libjpeg = st.be.len() > 937;
        end_of_band(w, ac, st, free, libjpeg, ch)?;
    }
    Ok(())
}

/// Incremental encoder of a sequential (baseline or extended) scan, fed MCU rows in order.
pub struct SeqScan<'a, 'c> {
    w: Bw<'a>,
    dcs: Vec<Codes>,
    acs: Vec<Codes>,
    /// interleaved scan: (scan component, component, h, v) of every block of an MCU
    units: Vec<(usize, usize, usize, usize)>,
    pred: [i32; 4],
    rst: usize,
    /// MCUs encoded so far
    m: usize,
    ch: &'a mut Choices<'c>,
}

impl<'a, 'c> SeqScan<'a, 'c> {
    pub fn new(s: &Scan, f: &Frame, ch: &'a mut Choices<'c>, out: &'a mut Vec<u8>) -> Result<Self, String> {
        let codes = |v: &[Option<Huff>]| -> Result<Vec<Codes>, String> { v.iter().map(|h| h.as_ref().map(Codes::new).ok_or_else(|| "missing Huffman table".to_string())).collect() };
        let mut units = Vec::new();
        if s.comps.len() > 1 {
            for (j, &ci) in s.comps.iter().enumerate() {
                let c = &f.comps[ci];
                for v in 0..c.v {
                    for h in 0..c.h {
                        units.push((j, ci, h, v));
                    }
                }
            }
        }
        Ok(SeqScan { w: Bw::new(out), dcs: codes(&s.dc)?, acs: codes(&s.ac)?, units, pred: [0; 4], rst: 0, m: 0, ch })
    }

    #[inline(always)]
    fn restart(&mut self, ri: usize) {
        if ri > 0 && self.m > 0 && self.m % ri == 0 {
            let n = self.w.pad_len();
            let p = self.ch.pad(n);
            self.w.flush(p);
            self.w.out.push(0xFF);
            self.w.out.push(0xD0 + (self.rst % 8) as u8);
            self.rst += 1;
            self.pred = [0; 4];
        }
    }

    /// Encodes the MCUs of frame MCU rows `my0..my1`; `blk(c, bx, by)` returns a block of
    /// component c (absolute block coordinates).
    pub fn rows<'b>(&mut self, f: &Frame, s: &Scan, my0: usize, my1: usize, blk: impl Fn(usize, usize, usize) -> &'b [i16; 64]) -> Result<(), String> {
        #[cfg(target_arch = "x86_64")]
        if std::is_x86_feature_detected!("bmi2") && std::is_x86_feature_detected!("lzcnt") && std::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU has every feature the function is compiled for.
            return unsafe { self.rows_bmi(f, s, my0, my1, blk) };
        }
        self.rows_impl(f, s, my0, my1, blk)
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt")]
    unsafe fn rows_bmi<'b>(&mut self, f: &Frame, s: &Scan, my0: usize, my1: usize, blk: impl Fn(usize, usize, usize) -> &'b [i16; 64]) -> Result<(), String> {
        self.rows_impl(f, s, my0, my1, blk)
    }

    #[inline(always)]
    fn rows_impl<'b>(&mut self, f: &Frame, s: &Scan, my0: usize, my1: usize, blk: impl Fn(usize, usize, usize) -> &'b [i16; 64]) -> Result<(), String> {
        if !self.units.is_empty() {
            for my in my0..my1 {
                for mx in 0..f.mcux {
                    self.restart(s.ri);
                    for &(j, ci, h, v) in &self.units {
                        let c = &f.comps[ci];
                        encode_seq(&mut self.w, &self.dcs[j], &self.acs[j], blk(ci, mx * c.h + h, my * c.v + v), &mut self.pred[j])?;
                    }
                    self.m += 1;
                }
            }
        } else {
            let ci = s.comps[0];
            let c = &f.comps[ci];
            for by in (my0 * c.v).min(c.bh)..(my1 * c.v).min(c.bh) {
                for bx in 0..c.bw {
                    self.restart(s.ri);
                    encode_seq(&mut self.w, &self.dcs[0], &self.acs[0], blk(ci, bx, by), &mut self.pred[0])?;
                    self.m += 1;
                }
            }
        }
        Ok(())
    }

    /// The complete bytes written so far (pending bits stay in the encoder); the caller may
    /// move them out and clear the vector.
    pub fn bytes(&mut self) -> &mut Vec<u8> {
        self.w.out
    }

    /// Final padding and the bytes after it.
    pub fn finish(mut self) {
        let n = self.w.pad_len();
        let p = self.ch.pad(n);
        self.w.flush(p);
        self.w.out.extend_from_slice(&self.ch.side.garbage);
    }
}

/// Incremental encoder of a progressive scan, fed the coefficients of consecutive MCU rows.
/// It writes into its own buffer (`finish` returns it), so all scans of a file can be fed
/// stripe by stripe.
pub struct ProgScan<'a, 'c> {
    out: Vec<u8>,
    c: u64,
    n: u32,
    dcs: Vec<Option<Codes>>,
    acs: Vec<Option<Codes>>,
    /// interleaved DC scan: (scan component, component, h, v) of every block of an MCU
    units: Vec<(usize, usize, usize, usize)>,
    pred: [i32; 4],
    rst: usize,
    /// MCUs (interleaved) or blocks encoded so far
    m: usize,
    st: Prog,
    br: Vec<u8>,
    /// AC scan: the last block fed so far, waiting for the next one (its successor decides
    /// whether an end-of-band run may continue)
    pending: Option<Box<[i16; 64]>>,
    ch: &'a mut Choices<'c>,
}

impl<'a, 'c> ProgScan<'a, 'c> {
    pub fn new(s: &Scan, f: &Frame, ch: &'a mut Choices<'c>, out: Vec<u8>) -> Self {
        let mut units = Vec::new();
        if s.comps.len() > 1 {
            for (j, &ci) in s.comps.iter().enumerate() {
                let c = &f.comps[ci];
                for v in 0..c.v {
                    for h in 0..c.h {
                        units.push((j, ci, h, v));
                    }
                }
            }
        }
        ProgScan {
            out,
            c: 0,
            n: 0,
            dcs: s.dc.iter().map(|h| h.as_ref().map(Codes::new)).collect(),
            acs: s.ac.iter().map(|h| h.as_ref().map(Codes::new)).collect(),
            units,
            pred: [0; 4],
            rst: 0,
            m: 0,
            st: Prog { eobrun: 0, be: Vec::new() },
            br: Vec::new(),
            pending: None,
            ch,
        }
    }

    fn restart(&mut self, w: &mut Bw, s: &Scan, m: usize) -> Result<(), String> {
        if s.ri > 0 && m > 0 && m % s.ri == 0 {
            if s.ss > 0 {
                w.room(512 + self.st.be.len() / 2);
                emit_eobrun(w, self.acs[0].as_ref().ok_or("missing Huffman table")?, &mut self.st)?;
            }
            let n = w.pad_len();
            let p = self.ch.pad(n);
            w.flush(p);
            w.out.push(0xFF);
            w.out.push(0xD0 + (self.rst % 8) as u8);
            self.rst += 1;
            self.pred = [0; 4];
        }
        Ok(())
    }

    /// AC scan: block `m` with its successor `next` (None at the end of the scan).
    fn ac_block(&mut self, w: &mut Bw, s: &Scan, count: usize, m: usize, blk: &[i16; 64], next: Option<&[i16; 64]>) -> Result<(), String> {
        self.restart(w, s, m)?;
        let interval_end = if s.ri > 0 { (m / s.ri + 1) * s.ri } else { count }.min(count);
        let free = m + 1 < interval_end && next.is_some_and(|b| pure(b, s));
        let ac = self.acs[0].as_ref().ok_or("missing Huffman table")?;
        if s.ah == 0 {
            encode_ac_first(w, s, ac, blk, &mut self.st, free, self.ch)?;
        } else {
            encode_ac_refine(w, s, ac, blk, &mut self.st, free, self.ch, &mut self.br)?;
        }
        self.m = m + 1;
        Ok(())
    }

    /// Encodes what the scan codes in frame MCU rows `my0..my1` (`last`: these are the last
    /// rows); `blk(c, bx, by)` returns a block of component c (absolute block coordinates).
    pub fn rows<'b>(&mut self, f: &Frame, s: &Scan, my0: usize, my1: usize, last: bool, blk: impl Fn(usize, usize, usize) -> &'b [i16; 64]) -> Result<(), String> {
        let mut out = std::mem::take(&mut self.out);
        let mut w = Bw { out: &mut out, c: self.c, n: self.n };
        #[cfg(target_arch = "x86_64")]
        let res = if std::is_x86_feature_detected!("bmi2") && std::is_x86_feature_detected!("lzcnt") && std::is_x86_feature_detected!("avx2") {
            // SAFETY: the CPU has every feature the function is compiled for.
            unsafe { self.rows_bmi(&mut w, f, s, my0, my1, last, blk) }
        } else {
            self.rows_into(&mut w, f, s, my0, my1, last, blk)
        };
        #[cfg(not(target_arch = "x86_64"))]
        let res = self.rows_into(&mut w, f, s, my0, my1, last, blk);
        self.c = w.c;
        self.n = w.n;
        self.out = out;
        res
    }

    #[cfg(target_arch = "x86_64")]
    #[target_feature(enable = "avx2,bmi1,bmi2,lzcnt,popcnt")]
    #[allow(clippy::too_many_arguments)]
    unsafe fn rows_bmi<'b>(&mut self, w: &mut Bw, f: &Frame, s: &Scan, my0: usize, my1: usize, last: bool, blk: impl Fn(usize, usize, usize) -> &'b [i16; 64]) -> Result<(), String> {
        self.rows_into(w, f, s, my0, my1, last, blk)
    }

    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn rows_into<'b>(&mut self, w: &mut Bw, f: &Frame, s: &Scan, my0: usize, my1: usize, last: bool, blk: impl Fn(usize, usize, usize) -> &'b [i16; 64]) -> Result<(), String> {
        if !self.units.is_empty() {
            for my in my0..my1 {
                for mx in 0..f.mcux {
                    self.restart(w, s, self.m)?;
                    for k in 0..self.units.len() {
                        let (j, ci, h, v) = self.units[k];
                        let c = &f.comps[ci];
                        let b = blk(ci, mx * c.h + h, my * c.v + v);
                        w.room(64);
                        if s.ah == 0 {
                            encode_dc(w, self.dcs[j].as_ref().ok_or("missing Huffman table")?, (b[0] as i32) >> s.al, &mut self.pred[j])?;
                        } else {
                            w.put(((b[0] as i32) >> s.al) as u32 & 1, 1);
                        }
                    }
                    self.m += 1;
                }
            }
            return Ok(());
        }
        let ci = s.comps[0];
        let c = &f.comps[ci];
        let (r0, r1) = ((my0 * c.v).min(c.bh), (my1 * c.v).min(c.bh));
        if s.ss == 0 {
            for by in r0..r1 {
                for bx in 0..c.bw {
                    self.restart(w, s, self.m)?;
                    let b = blk(ci, bx, by);
                    w.room(64);
                    if s.ah == 0 {
                        encode_dc(w, self.dcs[0].as_ref().ok_or("missing Huffman table")?, (b[0] as i32) >> s.al, &mut self.pred[0])?;
                    } else {
                        w.put(((b[0] as i32) >> s.al) as u32 & 1, 1);
                    }
                    self.m += 1;
                }
            }
            return Ok(());
        }
        let (bw, count) = (c.bw, c.bw * c.bh);
        let (m0, m1) = (r0 * bw, r1 * bw);
        let at = |m: usize| blk(ci, m % bw, m / bw);
        if let Some(p) = self.pending.take() {
            if m0 < m1 {
                self.ac_block(w, s, count, self.m, &p, Some(at(m0)))?;
            } else if last {
                self.ac_block(w, s, count, self.m, &p, None)?;
            } else {
                self.pending = Some(p);
                return Ok(());
            }
        }
        for m in m0..m1 {
            if m + 1 < m1 {
                self.ac_block(w, s, count, m, at(m), Some(at(m + 1)))?;
            } else if last {
                self.ac_block(w, s, count, m, at(m), None)?;
            } else {
                self.pending = Some(Box::new(*at(m)));
            }
        }
        Ok(())
    }

    /// The complete bytes written so far (pending bits stay in the encoder); the caller may
    /// move them out and clear the vector.
    pub fn bytes(&mut self) -> &mut Vec<u8> {
        &mut self.out
    }

    /// Ends the scan and returns its bytes.
    pub fn finish(mut self, s: &Scan) -> Result<Vec<u8>, String> {
        let mut out = std::mem::take(&mut self.out);
        let mut w = Bw { out: &mut out, c: self.c, n: self.n };
        if s.ss > 0 {
            if let Some(p) = self.pending.take() {
                let count = usize::MAX;
                self.ac_block(&mut w, s, count, self.m, &p, None)?;
            }
            w.room(512 + self.st.be.len() / 2);
            emit_eobrun(&mut w, self.acs[0].as_ref().ok_or("missing Huffman table")?, &mut self.st)?;
        }
        let n = w.pad_len();
        let p = self.ch.pad(n);
        w.flush(p);
        w.out.extend_from_slice(&self.ch.side.garbage);
        Ok(out)
    }
}

/// Regenerates the entropy-coded bytes of one scan and appends them to `out`. `coefs[c]`: the
/// coefficients of component c (`pw * ph` blocks of 64, natural order).
pub fn encode_scan(f: &Frame, s: &Scan, coefs: &[&[i16]], ch: &mut Choices, out: &mut Vec<u8>) -> Result<(), String> {
    let blk = |ci: usize, bx: usize, by: usize| -> &[i16; 64] { coefs[ci][(by * f.comps[ci].pw + bx) * 64..][..64].try_into().unwrap() };
    if !f.progressive {
        let mut enc = SeqScan::new(s, f, ch, out)?;
        enc.rows(f, s, 0, f.mcuy, blk)?;
        enc.finish();
        return Ok(());
    }
    let mut enc = ProgScan::new(s, f, ch, std::mem::take(out));
    let res = enc.rows(f, s, 0, f.mcuy, true, blk);
    match res.and_then(|_| enc.finish(s)) {
        Ok(v) => {
            *out = v;
            Ok(())
        }
        Err(e) => Err(e),
    }
}
