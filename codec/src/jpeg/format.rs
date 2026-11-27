//! JPEG file structure and bit-exact entropy coding.
//!
//! `parse` splits a file into its marker segments ("header blob": everything except the
//! entropy-coded scan data) and the scans. `decode_scans` turns the scan data into quantized DCT
//! coefficients and records everything the coefficients do not determine: the padding bits at
//! every flush, bytes between the last coded unit and the next marker, and where end-of-band runs
//! of progressive scans were cut. `encode_scan` regenerates the scan bytes from the coefficients
//! following libjpeg's encoder exactly, taking the recorded choices from a `SideCoder`, so that
//! the output can be compared byte for byte with the input.

use crate::coder::BitCoder;

/// Zigzag position -> natural (row-major) index within an 8x8 block.
pub const NATURAL: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22,
    15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// Largest image the recompressor accepts (in 8x8 blocks over all components); bigger files
/// are stored verbatim.
pub const MAX_BLOCKS: usize = 1 << 22;

#[derive(Clone, Debug)]
pub struct Huff {
    pub counts: [u8; 17],
    pub syms: Vec<u8>,
    pub code: [u16; 256],
    pub len: [u8; 256],
    look: Vec<u16>,
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
}

const LOOK_BITS: u32 = 9;

impl Huff {
    pub fn new(counts: [u8; 17], syms: Vec<u8>) -> Result<Huff, String> {
        let mut h = Huff { counts, syms, code: [0; 256], len: [0; 256], look: vec![0; 1 << LOOK_BITS], maxcode: [-1; 18], valptr: [0; 17], mincode: [0; 17] };
        let mut code = 0u32;
        let mut k = 0usize;
        for l in 1..=16usize {
            h.valptr[l] = k as i32;
            h.mincode[l] = code as i32;
            for _ in 0..counts[l] {
                if code >= 1 << l {
                    return Err("invalid Huffman table".into());
                }
                let s = h.syms[k] as usize;
                if h.len[s] == 0 {
                    h.code[s] = code as u16;
                    h.len[s] = l as u8;
                }
                if l as u32 <= LOOK_BITS {
                    let sh = LOOK_BITS - l as u32;
                    for j in (code << sh)..((code + 1) << sh) {
                        h.look[j as usize] = ((l as u16) << 8) | s as u16;
                    }
                }
                code += 1;
                k += 1;
            }
            if counts[l] > 0 {
                h.maxcode[l] = code as i32 - 1;
            }
            if code >= 1 << l {
                return Err("invalid Huffman table".into());
            }
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        Ok(h)
    }
}

#[derive(Clone, Debug)]
pub struct Comp {
    pub id: u8,
    pub h: usize,
    pub v: usize,
    pub tq: usize,
    /// blocks covering the component's samples
    pub bw: usize,
    pub bh: usize,
    /// blocks including the padding of interleaved MCUs
    pub pw: usize,
    pub ph: usize,
    /// quantization table (natural order) in effect at the component's first scan
    pub q: [u16; 64],
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub progressive: bool,
    pub width: usize,
    pub height: usize,
    pub comps: Vec<Comp>,
    pub hmax: usize,
    pub vmax: usize,
    pub mcux: usize,
    pub mcuy: usize,
}

#[derive(Clone, Debug)]
pub struct Scan {
    pub comps: Vec<usize>,
    pub dc: Vec<Option<Huff>>,
    pub ac: Vec<Option<Huff>>,
    pub ss: usize,
    pub se: usize,
    pub ah: u32,
    pub al: u32,
    /// restart interval in MCUs (0 = none)
    pub ri: usize,
    /// offset in the header blob just after this scan's SOS segment
    pub blob_pos: usize,
    /// byte range of the entropy-coded data in the parsed input (empty for a blob)
    pub seg: (usize, usize),
}

impl Scan {
    pub fn interleaved(&self) -> bool {
        self.comps.len() > 1
    }
}

pub struct Layout {
    pub frame: Frame,
    pub scans: Vec<Scan>,
    /// the input without entropy-coded data, up to and including EOI
    pub blob: Vec<u8>,
    /// offset of the first byte after EOI in the parsed input
    pub end: usize,
}

fn be16(b: &[u8], i: usize) -> usize {
    ((b[i] as usize) << 8) | b[i + 1] as usize
}

/// Parses the marker structure. `with_entropy`: the input is a JPEG file (scan data follows each
/// SOS); otherwise it is a header blob in which each SOS is directly followed by the next marker.
pub fn parse(b: &[u8], with_entropy: bool) -> Result<Layout, String> {
    if b.len() < 4 || b[0] != 0xFF || b[1] != 0xD8 {
        return Err("not a JPEG file".into());
    }
    let mut pos = 2;
    let mut blob: Vec<u8> = Vec::with_capacity(4096);
    let mut copied = 0usize;
    let mut frame: Option<Frame> = None;
    let mut qt: [Option<[u16; 64]>; 4] = [None; 4];
    let mut dht: [[Option<Huff>; 4]; 2] = Default::default();
    let mut ri = 0usize;
    let mut scans = Vec::new();
    loop {
        while pos < b.len() && b[pos] != 0xFF {
            pos += 1;
        }
        while pos < b.len() && b[pos] == 0xFF {
            pos += 1;
        }
        if pos >= b.len() {
            return Err("missing EOI marker".into());
        }
        let m = b[pos];
        pos += 1;
        match m {
            0xD9 => break,
            0xD8 | 0xD0..=0xD7 | 0x01 | 0x00 => return Err(format!("unexpected marker FF{m:02X}")),
            _ => {}
        }
        if pos + 2 > b.len() {
            return Err("truncated segment".into());
        }
        let len = be16(b, pos);
        if len < 2 || pos + len > b.len() {
            return Err("truncated segment".into());
        }
        let seg = &b[pos + 2..pos + len];
        pos += len;
        match m {
            0xC0 | 0xC1 | 0xC2 => {
                if frame.is_some() {
                    return Err("more than one frame".into());
                }
                frame = Some(parse_sof(seg, m == 0xC2)?);
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return Err("unsupported JPEG process (lossless, hierarchical or arithmetic coding)".into()),
            0xC4 => {
                let mut i = 0;
                while i < seg.len() {
                    let tc = (seg[i] >> 4) as usize;
                    let th = (seg[i] & 15) as usize;
                    if tc > 1 || th > 3 || i + 17 > seg.len() {
                        return Err("bad DHT".into());
                    }
                    let mut counts = [0u8; 17];
                    counts[1..17].copy_from_slice(&seg[i + 1..i + 17]);
                    let n: usize = counts.iter().map(|&c| c as usize).sum();
                    if n > 256 || i + 17 + n > seg.len() {
                        return Err("bad DHT".into());
                    }
                    dht[tc][th] = Some(Huff::new(counts, seg[i + 17..i + 17 + n].to_vec())?);
                    i += 17 + n;
                }
            }
            0xDB => {
                let mut i = 0;
                while i < seg.len() {
                    let pq = seg[i] >> 4;
                    let tq = (seg[i] & 15) as usize;
                    let n = if pq == 0 { 64 } else { 128 };
                    if pq > 1 || tq > 3 || i + 1 + n > seg.len() {
                        return Err("bad DQT".into());
                    }
                    let mut t = [0u16; 64];
                    for k in 0..64 {
                        let v = if pq == 0 { seg[i + 1 + k] as u16 } else { be16(seg, i + 1 + 2 * k) as u16 };
                        t[NATURAL[k]] = v.max(1);
                    }
                    qt[tq] = Some(t);
                    i += 1 + n;
                }
            }
            0xDD => {
                if seg.len() != 2 {
                    return Err("bad DRI".into());
                }
                ri = be16(seg, 0);
            }
            0xDC => return Err("DNL marker not supported".into()),
            0xDA => {
                let f = frame.as_mut().ok_or("scan before frame header")?;
                let mut sc = parse_sos(seg, f, &dht, ri)?;
                for &ci in &sc.comps {
                    let c = &mut f.comps[ci];
                    if c.q[0] == 0 {
                        c.q = qt[c.tq].ok_or("missing quantization table")?;
                    }
                }
                blob.extend_from_slice(&b[copied..pos]);
                sc.blob_pos = blob.len();
                let start = pos;
                if with_entropy {
                    while pos + 1 < b.len() {
                        if b[pos] == 0xFF {
                            let n = b[pos + 1];
                            if n == 0x00 || (0xD0..=0xD7).contains(&n) {
                                pos += 2;
                                continue;
                            }
                            break;
                        }
                        pos += 1;
                    }
                    if pos + 1 >= b.len() {
                        return Err("scan data runs to the end of the file".into());
                    }
                }
                sc.seg = (start, pos);
                copied = pos;
                scans.push(sc);
            }
            _ => {}
        }
    }
    blob.extend_from_slice(&b[copied..pos]);
    let frame = frame.ok_or("no frame header")?;
    if scans.is_empty() {
        return Err("no scans".into());
    }
    for c in &frame.comps {
        if c.q[0] == 0 && scans.iter().any(|s| s.comps.iter().any(|&ci| frame.comps[ci].id == c.id)) {
            return Err("missing quantization table".into());
        }
    }
    Ok(Layout { frame, scans, blob, end: pos })
}

fn parse_sof(s: &[u8], progressive: bool) -> Result<Frame, String> {
    if s.len() < 6 {
        return Err("bad SOF".into());
    }
    if s[0] != 8 {
        return Err(format!("{}-bit JPEG not supported", s[0]));
    }
    let height = be16(s, 1);
    let width = be16(s, 3);
    let n = s[5] as usize;
    if width == 0 || height == 0 {
        return Err("image size missing (DNL) or zero".into());
    }
    if n == 0 || n > 4 || s.len() != 6 + 3 * n {
        return Err("bad SOF".into());
    }
    let mut comps = Vec::with_capacity(n);
    for i in 0..n {
        let id = s[6 + 3 * i];
        let h = (s[7 + 3 * i] >> 4) as usize;
        let v = (s[7 + 3 * i] & 15) as usize;
        let tq = s[8 + 3 * i] as usize;
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || tq > 3 || comps.iter().any(|c: &Comp| c.id == id) {
            return Err("bad SOF component".into());
        }
        comps.push(Comp { id, h, v, tq, bw: 0, bh: 0, pw: 0, ph: 0, q: [0; 64] });
    }
    let hmax = comps.iter().map(|c| c.h).max().unwrap();
    let vmax = comps.iter().map(|c| c.v).max().unwrap();
    let mcux = width.div_ceil(8 * hmax);
    let mcuy = height.div_ceil(8 * vmax);
    let mut total = 0usize;
    for c in comps.iter_mut() {
        c.bw = (width * c.h).div_ceil(hmax).div_ceil(8);
        c.bh = (height * c.v).div_ceil(vmax).div_ceil(8);
        c.pw = mcux * c.h;
        c.ph = mcuy * c.v;
        total += c.pw * c.ph;
    }
    if total > MAX_BLOCKS {
        return Err("image too large for recompression".into());
    }
    Ok(Frame { progressive, width, height, comps, hmax, vmax, mcux, mcuy })
}

fn parse_sos(s: &[u8], f: &Frame, dht: &[[Option<Huff>; 4]; 2], ri: usize) -> Result<Scan, String> {
    let n = *s.first().ok_or("bad SOS")? as usize;
    if n == 0 || n > 4 || s.len() != 4 + 2 * n {
        return Err("bad SOS".into());
    }
    let mut comps = Vec::with_capacity(n);
    let mut td = Vec::with_capacity(n);
    let mut ta = Vec::with_capacity(n);
    for j in 0..n {
        let id = s[1 + 2 * j];
        let ci = f.comps.iter().position(|c| c.id == id).ok_or("scan refers to an unknown component")?;
        if comps.contains(&ci) {
            return Err("component twice in one scan".into());
        }
        comps.push(ci);
        td.push((s[2 + 2 * j] >> 4) as usize);
        ta.push((s[2 + 2 * j] & 15) as usize);
    }
    let (mut ss, mut se) = (s[1 + 2 * n] as usize, s[2 + 2 * n] as usize);
    let (mut ah, mut al) = ((s[3 + 2 * n] >> 4) as u32, (s[3 + 2 * n] & 15) as u32);
    if f.progressive {
        let ok = if ss == 0 { se == 0 } else { n == 1 && se >= ss && se <= 63 };
        if !ok || ah > 13 || al > 13 {
            return Err("invalid progressive scan".into());
        }
    } else {
        ss = 0;
        se = 63;
        ah = 0;
        al = 0;
    }
    if n > 1 && comps.iter().map(|&ci| f.comps[ci].h * f.comps[ci].v).sum::<usize>() > 10 {
        return Err("too many blocks per MCU".into());
    }
    let need_dc = ss == 0 && ah == 0;
    let need_ac = se > 0;
    let mut dc = Vec::with_capacity(n);
    let mut ac = Vec::with_capacity(n);
    for j in 0..n {
        dc.push(if need_dc { Some(dht[0][td[j].min(3)].clone().ok_or("missing Huffman table")?) } else { None });
        ac.push(if need_ac { Some(dht[1][ta[j].min(3)].clone().ok_or("missing Huffman table")?) } else { None });
        if td[j] > 3 || ta[j] > 3 {
            return Err("bad Huffman table index".into());
        }
    }
    Ok(Scan { comps, dc, ac, ss, se, ah, al, ri, blob_pos: 0, seg: (0, 0) })
}

/// Block coordinates (component, x, y) of every block coded in each MCU of a scan.
pub struct McuMap {
    pub count: usize,
    interleaved: bool,
    mcux: usize,
    units: Vec<(usize, usize, usize)>,
}

impl McuMap {
    pub fn new(f: &Frame, s: &Scan) -> McuMap {
        if s.interleaved() {
            let mut units = Vec::new();
            for (j, &ci) in s.comps.iter().enumerate() {
                let c = &f.comps[ci];
                for v in 0..c.v {
                    for h in 0..c.h {
                        units.push((j, h, v));
                    }
                }
            }
            McuMap { count: f.mcux * f.mcuy, interleaved: true, mcux: f.mcux, units }
        } else {
            let c = &f.comps[s.comps[0]];
            McuMap { count: c.bw * c.bh, interleaved: false, mcux: c.bw, units: vec![(0, 0, 0)] }
        }
    }
    /// Calls `f(scan component index, block x, block y)` for every block of MCU `m`.
    #[inline]
    pub fn blocks(&self, f: &Frame, s: &Scan, m: usize, mut cb: impl FnMut(usize, usize, usize)) {
        let (mx, my) = (m % self.mcux, m / self.mcux);
        if !self.interleaved {
            cb(0, mx, my);
            return;
        }
        for &(j, h, v) in &self.units {
            let c = &f.comps[s.comps[j]];
            cb(j, mx * c.h + h, my * c.v + v);
        }
    }
}

/// Choices recorded while decoding one scan.
#[derive(Clone, Debug, Default)]
pub struct ScanSide {
    /// padding bits written at each flush (restart interval end and scan end)
    pub pads: Vec<u8>,
    /// bytes after the final flush, before the next marker
    pub garbage: Vec<u8>,
    /// for every end-of-band event of a progressive AC scan: did the run end there?
    pub eob_end: Vec<bool>,
    /// some padding bits were not all ones
    pub pads_not_ones: bool,
}

struct BitReader<'a> {
    seg: &'a [u8],
    pos: usize,
    acc: u64,
    nbits: u32,
    hist: [u8; 16],
    hn: usize,
    fake: u32,
}

impl<'a> BitReader<'a> {
    fn new(seg: &'a [u8]) -> Self {
        BitReader { seg, pos: 0, acc: 0, nbits: 0, hist: [0; 16], hn: 0, fake: 0 }
    }
    #[inline]
    fn fill(&mut self) {
        while self.nbits <= 56 {
            let (byte, l) = match self.seg.get(self.pos) {
                Some(&0xFF) => {
                    if self.seg.get(self.pos + 1) == Some(&0) {
                        (0xFFu8, 2u8)
                    } else {
                        (0, 0)
                    }
                }
                Some(&v) => (v, 1),
                None => (0, 0),
            };
            if l == 0 {
                self.fake += 1;
            }
            self.pos += l as usize;
            self.hist[self.hn & 15] = l;
            self.hn += 1;
            self.acc |= (byte as u64) << (56 - self.nbits);
            self.nbits += 8;
        }
    }
    #[inline]
    fn bits(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.nbits < n {
            self.fill();
        }
        let v = (self.acc >> (64 - n)) as u32;
        self.acc <<= n;
        self.nbits -= n;
        v
    }
    #[inline]
    fn huff(&mut self, h: &Huff) -> Result<u8, String> {
        if self.nbits < 16 {
            self.fill();
        }
        let p = (self.acc >> 48) as u32;
        let e = h.look[(p >> (16 - LOOK_BITS)) as usize];
        if e != 0 {
            let l = (e >> 8) as u32;
            self.acc <<= l;
            self.nbits -= l;
            return Ok(e as u8);
        }
        for l in (LOOK_BITS + 1)..=16 {
            let code = (p >> (16 - l)) as i32;
            if code <= h.maxcode[l as usize] {
                self.acc <<= l;
                self.nbits -= l;
                let idx = h.valptr[l as usize] + code - h.mincode[l as usize];
                return h.syms.get(idx as usize).copied().ok_or_else(|| "corrupt Huffman data".to_string());
            }
        }
        Err("corrupt Huffman data".into())
    }
    /// Reads the padding bits of the current byte and moves to the next whole byte.
    /// Fails if the data consumed so far ran past the end of the segment.
    fn align(&mut self) -> Result<(u8, bool), String> {
        let n = self.nbits % 8;
        let pad = self.bits(n) as u8;
        let ones = pad as u32 == (1 << n) - 1;
        let unread = (self.nbits / 8) as usize;
        let mut fake_unread = 0;
        for i in 0..unread {
            let l = self.hist[(self.hn - 1 - i) & 15];
            if l == 0 {
                fake_unread += 1;
            }
            self.pos -= l as usize;
        }
        if fake_unread != self.fake {
            return Err("scan data ends early".into());
        }
        self.fake = 0;
        self.acc = 0;
        self.nbits = 0;
        self.hn = 0;
        Ok((pad, ones))
    }
}

#[inline]
fn extend(v: u32, s: u32) -> i32 {
    if s == 0 {
        0
    } else if v < (1 << (s - 1)) {
        v as i32 - (1 << s) + 1
    } else {
        v as i32
    }
}

fn put_coef(c: &mut i16, v: i32) -> Result<(), String> {
    if !(-32767..=32767).contains(&v) {
        return Err("coefficient out of range".into());
    }
    *c = v as i16;
    Ok(())
}

/// Decodes all scans into coefficients (per component, `pw * ph` blocks of 64 in natural order).
pub fn decode_scans(l: &Layout, data: &[u8]) -> Result<(Vec<Vec<i16>>, Vec<ScanSide>), String> {
    let f = &l.frame;
    let mut coefs: Vec<Vec<i16>> = f.comps.iter().map(|c| vec![0i16; c.pw * c.ph * 64]).collect();
    let mut sides = Vec::with_capacity(l.scans.len());
    for s in &l.scans {
        sides.push(decode_scan(f, s, &data[s.seg.0..s.seg.1], &mut coefs)?);
    }
    Ok((coefs, sides))
}

fn decode_scan(f: &Frame, s: &Scan, seg: &[u8], coefs: &mut [Vec<i16>]) -> Result<ScanSide, String> {
    let map = McuMap::new(f, s);
    let mut r = BitReader::new(seg);
    let mut side = ScanSide::default();
    let mut pred = [0i32; 4];
    let mut eobrun = 0u32;
    let mut rst = 0usize;
    let pw: Vec<usize> = s.comps.iter().map(|&ci| f.comps[ci].pw).collect();
    for m in 0..map.count {
        if s.ri > 0 && m > 0 && m % s.ri == 0 {
            if eobrun > 0 {
                return Err("end-of-band run crosses a restart marker".into());
            }
            let (pad, ones) = r.align()?;
            side.pads.push(pad);
            side.pads_not_ones |= !ones;
            if r.seg.get(r.pos) != Some(&0xFF) || r.seg.get(r.pos + 1) != Some(&(0xD0 + (rst % 8) as u8)) {
                return Err("missing or unexpected restart marker".into());
            }
            r.pos += 2;
            rst += 1;
            pred = [0; 4];
        }
        let mut err: Result<(), String> = Ok(());
        map.blocks(f, s, m, |j, bx, by| {
            if err.is_err() {
                return;
            }
            let ci = s.comps[j];
            let off = (by * pw[j] + bx) * 64;
            let blk = &mut coefs[ci][off..off + 64];
            err = if !f.progressive {
                decode_seq(&mut r, s, j, blk, &mut pred[j])
            } else if s.ss == 0 {
                if s.ah == 0 {
                    decode_dc_first(&mut r, s, j, blk, &mut pred[j])
                } else {
                    if r.bits(1) != 0 {
                        blk[0] |= 1 << s.al;
                    }
                    Ok(())
                }
            } else if s.ah == 0 {
                decode_ac_first(&mut r, s, blk, &mut eobrun, &mut side.eob_end)
            } else {
                decode_ac_refine(&mut r, s, blk, &mut eobrun, &mut side.eob_end)
            };
        });
        err?;
    }
    if eobrun > 0 {
        return Err("end-of-band run past the end of the scan".into());
    }
    let (pad, ones) = r.align()?;
    side.pads.push(pad);
    side.pads_not_ones |= !ones;
    side.garbage = seg[r.pos..].to_vec();
    Ok(side)
}

fn decode_seq(r: &mut BitReader, s: &Scan, j: usize, blk: &mut [i16], pred: &mut i32) -> Result<(), String> {
    let dc = s.dc[j].as_ref().unwrap();
    let ac = s.ac[j].as_ref().unwrap();
    let t = r.huff(dc)? as u32;
    if t > 15 {
        return Err("bad DC code".into());
    }
    let d = extend(r.bits(t), t);
    *pred += d;
    put_coef(&mut blk[0], *pred)?;
    let mut k = 1;
    while k < 64 {
        let rs = r.huff(ac)? as usize;
        let (run, size) = (rs >> 4, (rs & 15) as u32);
        if size == 0 {
            if run == 15 {
                k += 16;
                continue;
            }
            break;
        }
        k += run;
        if k > 63 {
            return Err("coefficient index out of range".into());
        }
        put_coef(&mut blk[NATURAL[k]], extend(r.bits(size), size))?;
        k += 1;
    }
    if k > 64 {
        return Err("coefficient index out of range".into());
    }
    Ok(())
}

fn decode_dc_first(r: &mut BitReader, s: &Scan, j: usize, blk: &mut [i16], pred: &mut i32) -> Result<(), String> {
    let t = r.huff(s.dc[j].as_ref().unwrap())? as u32;
    if t > 15 {
        return Err("bad DC code".into());
    }
    *pred += extend(r.bits(t), t);
    put_coef(&mut blk[0], (*pred as i64 * (1i64 << s.al)).clamp(-1 << 20, 1 << 20) as i32)
}

fn decode_ac_first(r: &mut BitReader, s: &Scan, blk: &mut [i16], eobrun: &mut u32, ends: &mut Vec<bool>) -> Result<(), String> {
    if *eobrun > 0 {
        *eobrun -= 1;
        ends.push(*eobrun == 0);
        return Ok(());
    }
    let ac = s.ac[0].as_ref().unwrap();
    let mut k = s.ss;
    while k <= s.se {
        let rs = r.huff(ac)? as u32;
        let (run, size) = (rs >> 4, rs & 15);
        if size == 0 {
            if run == 15 {
                k += 16;
                if k > s.se + 1 {
                    return Err("zero run past the end of the band".into());
                }
                continue;
            }
            let n = (1u32 << run) + r.bits(run) - 1;
            *eobrun = n;
            ends.push(n == 0);
            return Ok(());
        }
        k += run as usize;
        if k > s.se {
            return Err("coefficient index out of range".into());
        }
        put_coef(&mut blk[NATURAL[k]], extend(r.bits(size), size) * (1 << s.al))?;
        k += 1;
    }
    Ok(())
}

fn decode_ac_refine(r: &mut BitReader, s: &Scan, blk: &mut [i16], eobrun: &mut u32, ends: &mut Vec<bool>) -> Result<(), String> {
    let p1: i32 = 1 << s.al;
    let m1: i32 = -p1;
    let mut k = s.ss;
    let refine = |r: &mut BitReader, c: &mut i16| {
        if r.bits(1) != 0 && (*c as i32 & p1) == 0 {
            *c = (*c as i32 + if *c >= 0 { p1 } else { m1 }) as i16;
        }
    };
    if *eobrun == 0 {
        let ac = s.ac[0].as_ref().unwrap();
        while k <= s.se {
            let rs = r.huff(ac)? as u32;
            let (mut run, size) = ((rs >> 4) as i32, rs & 15);
            let mut newv = 0i32;
            if size != 0 {
                if size != 1 {
                    return Err("bad refinement code".into());
                }
                newv = if r.bits(1) != 0 { p1 } else { m1 };
            } else if run != 15 {
                *eobrun = (1u32 << run) + r.bits(run as u32);
                break;
            }
            loop {
                let c = &mut blk[NATURAL[k]];
                if *c != 0 {
                    refine(r, c);
                } else {
                    run -= 1;
                    if run < 0 {
                        break;
                    }
                }
                k += 1;
                if k > s.se {
                    break;
                }
            }
            if newv != 0 {
                if k > s.se {
                    return Err("coefficient index out of range".into());
                }
                blk[NATURAL[k]] = newv as i16;
            }
            k += 1;
        }
    }
    if *eobrun > 0 {
        while k <= s.se {
            let c = &mut blk[NATURAL[k]];
            if *c != 0 {
                refine(r, c);
            }
            k += 1;
        }
        *eobrun -= 1;
        ends.push(*eobrun == 0);
    }
    Ok(())
}

/// Source of the choices that coefficients do not determine. While compressing it replays the
/// recorded `ScanSide` and codes each choice; while decompressing it decodes them.
pub struct SideCoder<'a, C: BitCoder> {
    pub coder: &'a mut C,
    rec: Option<&'a ScanSide>,
    /// bit counts per context (Krichevsky-Trofimov estimate), shared by all scans of a file
    pub counts: &'a mut [[u32; 2]; 8],
    pad_i: usize,
    eob_i: usize,
    /// upper bound for decoded byte counts (the size of the original file)
    pub limit: usize,
}

const S_PAD: usize = 0;
const S_EOB: usize = 2;
const S_GARBAGE: usize = 4;

impl<'a, C: BitCoder> SideCoder<'a, C> {
    pub fn new(coder: &'a mut C, rec: Option<&'a ScanSide>, counts: &'a mut [[u32; 2]; 8]) -> Self {
        SideCoder { coder, rec, counts, pad_i: 0, eob_i: 0, limit: usize::MAX }
    }
    fn bit(&mut self, ctx: usize, bit: u32) -> u32 {
        let [n0, n1] = self.counts[ctx];
        let p = (((2 * n1 as u64 + 1) << 16) / (2 * (n0 as u64 + n1 as u64) + 2)).clamp(1, 65535) as u32;
        let b = self.coder.code(bit, p);
        let c = &mut self.counts[ctx][b as usize];
        *c = c.saturating_add(1);
        b
    }
    fn pad(&mut self, n: u32) -> u32 {
        let ones = (1u32 << n) - 1;
        let v = self.rec.map_or(0, |r| r.pads.get(self.pad_i).copied().unwrap_or(0xFF) as u32 & ones);
        self.pad_i += 1;
        if n == 0 {
            return 0;
        }
        if self.bit(S_PAD, (v == ones) as u32) != 0 {
            ones
        } else {
            self.coder.code_raw(v, n)
        }
    }
    /// Whether an end-of-band run ends at a point where the encoder was free to continue it.
    fn eob_end(&mut self, libjpeg_flushes: bool) -> bool {
        let v = self.rec.map_or(false, |r| r.eob_end.get(self.eob_i).copied().unwrap_or(true));
        self.bit(S_EOB + libjpeg_flushes as usize, v as u32) != 0
    }
    fn garbage(&mut self) -> Result<Vec<u8>, String> {
        let g: &[u8] = self.rec.map_or(&[], |r| &r.garbage);
        if self.bit(S_GARBAGE, !g.is_empty() as u32) == 0 {
            return Ok(Vec::new());
        }
        let n = self.coder.code_raw(g.len() as u32, 32) as usize;
        if n > self.limit {
            return Err("corrupt side information".into());
        }
        Ok((0..n).map(|i| self.coder.code_raw(g.get(i).copied().unwrap_or(0) as u32, 8) as u8).collect())
    }
}

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    nbits: u32,
}

impl BitWriter {
    fn new(cap: usize) -> Self {
        BitWriter { out: Vec::with_capacity(cap), acc: 0, nbits: 0 }
    }
    #[inline]
    fn put(&mut self, v: u32, n: u32) {
        if n == 0 {
            return;
        }
        self.acc = (self.acc << n) | (v as u64 & ((1u64 << n) - 1));
        self.nbits += n;
        while self.nbits >= 8 {
            let b = (self.acc >> (self.nbits - 8)) as u8;
            self.out.push(b);
            if b == 0xFF {
                self.out.push(0);
            }
            self.nbits -= 8;
        }
    }
    #[inline]
    fn sym(&mut self, h: &Huff, s: usize) -> Result<(), String> {
        let l = h.len[s];
        if l == 0 {
            return Err("symbol missing from Huffman table".into());
        }
        self.put(h.code[s] as u32, l as u32);
        Ok(())
    }
    fn pad_len(&self) -> u32 {
        (8 - self.nbits % 8) % 8
    }
}

#[inline]
fn bitlen(v: u32) -> u32 {
    32 - v.leading_zeros()
}

struct ProgState {
    eobrun: u32,
    be: Vec<u8>,
}

/// Regenerates the entropy-coded bytes of one scan.
pub fn encode_scan<C: BitCoder>(f: &Frame, s: &Scan, coefs: &[Vec<i16>], side: &mut SideCoder<C>) -> Result<Vec<u8>, String> {
    let map = McuMap::new(f, s);
    let mut w = BitWriter::new(64 * 1024);
    let mut pred = [0i32; 4];
    let mut rst = 0usize;
    let pw: Vec<usize> = s.comps.iter().map(|&ci| f.comps[ci].pw).collect();
    let ac_scan = f.progressive && s.ss > 0;
    let mut st = ProgState { eobrun: 0, be: Vec::new() };
    // For progressive AC scans: does each block emit no Huffman symbol (it can only extend a run)?
    let pure: Vec<bool> = if ac_scan {
        let c = &coefs[s.comps[0]];
        (0..map.count)
            .map(|m| {
                let (bx, by) = (m % f.comps[s.comps[0]].bw, m / f.comps[s.comps[0]].bw);
                let blk = &c[(by * pw[0] + bx) * 64..][..64];
                (s.ss..=s.se).all(|k| {
                    let a = (blk[NATURAL[k]] as i32).unsigned_abs() >> s.al;
                    if s.ah == 0 {
                        a == 0
                    } else {
                        a != 1
                    }
                })
            })
            .collect()
    } else {
        Vec::new()
    };
    for m in 0..map.count {
        if s.ri > 0 && m > 0 && m % s.ri == 0 {
            emit_eobrun(&mut w, s, &mut st)?;
            let n = w.pad_len();
            let v = side.pad(n);
            w.put(v, n);
            w.out.push(0xFF);
            w.out.push(0xD0 + (rst % 8) as u8);
            rst += 1;
            pred = [0; 4];
        }
        let interval_end = if s.ri > 0 { (m / s.ri + 1) * s.ri } else { map.count }.min(map.count);
        let free = ac_scan && m + 1 < interval_end && pure[m + 1];
        let mut err: Result<(), String> = Ok(());
        map.blocks(f, s, m, |j, bx, by| {
            if err.is_err() {
                return;
            }
            let ci = s.comps[j];
            let blk = &coefs[ci][(by * pw[j] + bx) * 64..][..64];
            err = if !f.progressive {
                encode_seq(&mut w, s, j, blk, &mut pred[j])
            } else if s.ss == 0 {
                if s.ah == 0 {
                    encode_dc(&mut w, s.dc[j].as_ref().unwrap(), (blk[0] as i32) >> s.al, &mut pred[j])
                } else {
                    w.put(((blk[0] as i32) >> s.al) as u32 & 1, 1);
                    Ok(())
                }
            } else if s.ah == 0 {
                encode_ac_first(&mut w, s, blk, &mut st, free, side)
            } else {
                encode_ac_refine(&mut w, s, blk, &mut st, free, side)
            };
        });
        err?;
    }
    emit_eobrun(&mut w, s, &mut st)?;
    let n = w.pad_len();
    let v = side.pad(n);
    w.put(v, n);
    let g = side.garbage()?;
    w.out.extend_from_slice(&g);
    Ok(w.out)
}

fn encode_dc(w: &mut BitWriter, h: &Huff, v: i32, pred: &mut i32) -> Result<(), String> {
    let d = v - *pred;
    *pred = v;
    let a = d.unsigned_abs();
    let n = bitlen(a);
    if n > 15 {
        return Err("DC difference out of range".into());
    }
    w.sym(h, n as usize)?;
    w.put(if d < 0 { (d - 1) as u32 } else { d as u32 }, n);
    Ok(())
}

fn encode_seq(w: &mut BitWriter, s: &Scan, j: usize, blk: &[i16], pred: &mut i32) -> Result<(), String> {
    encode_dc(w, s.dc[j].as_ref().unwrap(), blk[0] as i32, pred)?;
    let ac = s.ac[j].as_ref().unwrap();
    let mut r = 0usize;
    for &nat in &NATURAL[1..] {
        let v = blk[nat] as i32;
        if v == 0 {
            r += 1;
            continue;
        }
        while r > 15 {
            w.sym(ac, 0xF0)?;
            r -= 16;
        }
        let n = bitlen(v.unsigned_abs());
        w.sym(ac, (r << 4) + n as usize)?;
        w.put(if v < 0 { (v - 1) as u32 } else { v as u32 }, n);
        r = 0;
    }
    if r > 0 {
        w.sym(ac, 0)?;
    }
    Ok(())
}

fn emit_eobrun(w: &mut BitWriter, s: &Scan, st: &mut ProgState) -> Result<(), String> {
    if st.eobrun > 0 {
        let n = bitlen(st.eobrun) - 1;
        if n > 14 {
            return Err("end-of-band run too long".into());
        }
        w.sym(s.ac[0].as_ref().unwrap(), (n << 4) as usize)?;
        w.put(st.eobrun, n);
        st.eobrun = 0;
        for &b in &st.be {
            w.put(b as u32, 1);
        }
        st.be.clear();
    }
    Ok(())
}

fn encode_ac_first<C: BitCoder>(w: &mut BitWriter, s: &Scan, blk: &[i16], st: &mut ProgState, free: bool, side: &mut SideCoder<C>) -> Result<(), String> {
    let ac = s.ac[0].as_ref().unwrap();
    let mut r = 0usize;
    for &nat in &NATURAL[s.ss..=s.se] {
        let v = blk[nat] as i32;
        let a = v.unsigned_abs() >> s.al;
        if a == 0 {
            r += 1;
            continue;
        }
        emit_eobrun(w, s, st)?;
        while r > 15 {
            w.sym(ac, 0xF0)?;
            r -= 16;
        }
        let n = bitlen(a);
        w.sym(ac, (r << 4) + n as usize)?;
        w.put(if v < 0 { !a } else { a }, n);
        r = 0;
    }
    if r > 0 {
        st.eobrun += 1;
        end_of_band(w, s, st, free, false, side)?;
    }
    Ok(())
}

fn end_of_band<C: BitCoder>(w: &mut BitWriter, s: &Scan, st: &mut ProgState, free: bool, libjpeg_flushes: bool, side: &mut SideCoder<C>) -> Result<(), String> {
    let forced = st.eobrun == 0x7FFF;
    let flush = if forced || !free { forced } else { side.eob_end(libjpeg_flushes) };
    side.eob_i += 1;
    if flush {
        emit_eobrun(w, s, st)?;
    }
    Ok(())
}

fn encode_ac_refine<C: BitCoder>(w: &mut BitWriter, s: &Scan, blk: &[i16], st: &mut ProgState, free: bool, side: &mut SideCoder<C>) -> Result<(), String> {
    let ac = s.ac[0].as_ref().unwrap();
    let mut abs = [0u32; 64];
    let mut eob = 0usize;
    for k in s.ss..=s.se {
        abs[k] = (blk[NATURAL[k]] as i32).unsigned_abs() >> s.al;
        if abs[k] == 1 {
            eob = k;
        }
    }
    let mut r = 0usize;
    let mut br: Vec<u8> = Vec::new();
    for k in s.ss..=s.se {
        let a = abs[k];
        if a == 0 {
            r += 1;
            continue;
        }
        while r > 15 && k <= eob {
            emit_eobrun(w, s, st)?;
            w.sym(ac, 0xF0)?;
            r -= 16;
            for &b in &br {
                w.put(b as u32, 1);
            }
            br.clear();
        }
        if a > 1 {
            br.push((a & 1) as u8);
            continue;
        }
        emit_eobrun(w, s, st)?;
        w.sym(ac, (r << 4) + 1)?;
        w.put((blk[NATURAL[k]] >= 0) as u32, 1);
        for &b in &br {
            w.put(b as u32, 1);
        }
        br.clear();
        r = 0;
    }
    if r > 0 || !br.is_empty() {
        st.eobrun += 1;
        st.be.extend_from_slice(&br);
        let libjpeg = st.be.len() > 937;
        end_of_band(w, s, st, free, libjpeg, side)?;
    }
    Ok(())
}
