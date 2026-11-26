//! Binary range coder (LZMA-style carry propagation, 32-bit range) driven by
//! 16-bit probabilities. `p1` is always P(bit = 1) scaled to (0, 65536).

pub trait BitCoder {
    /// Encoder: codes `bit` and returns it. Decoder: ignores `bit`, returns the decoded bit.
    fn code(&mut self, bit: u32, p1: u32) -> u32;
    /// Codes `nbits` raw bits (MSB first) with probability 1/2.
    fn code_raw(&mut self, value: u32, nbits: u32) -> u32 {
        let mut v = 0;
        for i in (0..nbits).rev() {
            v = (v << 1) | self.code((value >> i) & 1, 32768);
        }
        v
    }
}

const TOP: u32 = 1 << 24;

pub struct Encoder {
    low: u64,
    range: u32,
    cache: u8,
    cache_size: u64,
    pub out: Vec<u8>,
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Encoder {
    pub fn new() -> Self {
        Encoder { low: 0, range: 0xFFFF_FFFF, cache: 0, cache_size: 1, out: Vec::new() }
    }

    #[inline]
    fn shift_low(&mut self) {
        if (self.low as u32) < 0xFF00_0000 || (self.low >> 32) != 0 {
            let carry = (self.low >> 32) as u8;
            let mut temp = self.cache;
            loop {
                self.out.push(temp.wrapping_add(carry));
                temp = 0xFF;
                self.cache_size -= 1;
                if self.cache_size == 0 {
                    break;
                }
            }
            self.cache = ((self.low >> 24) & 0xFF) as u8;
        }
        self.cache_size += 1;
        self.low = (self.low & 0x00FF_FFFF) << 8;
    }

    pub fn finish(mut self) -> Vec<u8> {
        for _ in 0..5 {
            self.shift_low();
        }
        // The first byte is always 0 by construction; the decoder skips it.
        self.out.remove(0);
        self.out
    }
}

impl BitCoder for Encoder {
    #[inline]
    fn code(&mut self, bit: u32, p1: u32) -> u32 {
        debug_assert!(p1 > 0 && p1 < 65536);
        let bound = (self.range >> 16) * p1;
        if bit != 0 {
            self.range = bound;
        } else {
            self.low += bound as u64;
            self.range -= bound;
        }
        while self.range < TOP {
            self.range <<= 8;
            self.shift_low();
        }
        bit
    }
}

pub struct Decoder<'a> {
    code: u32,
    range: u32,
    data: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut d = Decoder { code: 0, range: 0xFFFF_FFFF, data, pos: 0 };
        for _ in 0..4 {
            d.code = (d.code << 8) | d.next_byte();
        }
        d
    }

    #[inline]
    fn next_byte(&mut self) -> u32 {
        let b = if self.pos < self.data.len() { self.data[self.pos] } else { 0 };
        self.pos += 1;
        b as u32
    }
}

impl BitCoder for Decoder<'_> {
    #[inline]
    fn code(&mut self, _bit: u32, p1: u32) -> u32 {
        let bound = (self.range >> 16) * p1;
        let bit = if self.code < bound {
            self.range = bound;
            1
        } else {
            self.code -= bound;
            self.range -= bound;
            0
        };
        while self.range < TOP {
            self.range <<= 8;
            self.code = (self.code << 8) | self.next_byte();
        }
        bit
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let mut rng = 12345u32;
        let mut bits = Vec::new();
        let mut probs = Vec::new();
        for _ in 0..200_000 {
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            let p = 1 + (rng >> 16) % 65535;
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            let b = ((rng >> 16) % 65536 < p) as u32;
            bits.push(b);
            probs.push(p);
        }
        let mut e = Encoder::new();
        for (b, p) in bits.iter().zip(&probs) {
            e.code(*b, *p);
        }
        let out = e.finish();
        let mut d = Decoder::new(&out);
        for (b, p) in bits.iter().zip(&probs) {
            assert_eq!(d.code(0, *p), *b);
        }
    }
}
