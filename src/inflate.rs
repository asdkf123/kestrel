// DEFLATE (RFC 1951) + zlib (RFC 1950) 압축 해제 — 직접 구현. PNG 의 IDAT 가 zlib 스트림.

struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, pos: 0, bit: 0 }
    }
    fn bit(&mut self) -> Option<u32> {
        if self.pos >= self.data.len() {
            return None;
        }
        let b = (self.data[self.pos] >> self.bit) & 1;
        self.bit += 1;
        if self.bit == 8 {
            self.bit = 0;
            self.pos += 1;
        }
        Some(b as u32)
    }
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..n {
            v |= self.bit()? << i;
        }
        Some(v)
    }
    fn align_byte(&mut self) {
        if self.bit != 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }
    fn read_byte(&mut self) -> Option<u8> {
        if self.pos >= self.data.len() {
            return None;
        }
        let b = self.data[self.pos];
        self.pos += 1;
        Some(b)
    }
    fn read_u16_le(&mut self) -> Option<u16> {
        let lo = self.read_byte()? as u16;
        let hi = self.read_byte()? as u16;
        Some(lo | (hi << 8))
    }
}

// 정규(canonical) 허프만 — puff.c 방식 디코드.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Huffman {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        let mut sum = 0u16;
        for len in 1..16 {
            offsets[len] = sum;
            sum += counts[len];
        }
        let n = lengths.iter().filter(|&&l| l != 0).count();
        let mut symbols = vec![0u16; n];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = sym as u16;
                offsets[l as usize] += 1;
            }
        }
        Huffman { counts, symbols }
    }
    fn decode(&self, r: &mut BitReader) -> Option<u16> {
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..16 {
            code |= r.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return self.symbols.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn fixed_lit() -> Huffman {
    let mut lengths = [0u8; 288];
    for l in lengths.iter_mut().take(144) {
        *l = 8;
    }
    for l in lengths.iter_mut().take(256).skip(144) {
        *l = 9;
    }
    for l in lengths.iter_mut().take(280).skip(256) {
        *l = 7;
    }
    for l in lengths.iter_mut().take(288).skip(280) {
        *l = 8;
    }
    Huffman::new(&lengths)
}

fn fixed_dist() -> Huffman {
    Huffman::new(&[5u8; 30])
}

fn read_dynamic(r: &mut BitReader) -> Option<(Huffman, Huffman)> {
    let hlit = r.bits(5)? as usize + 257;
    let hdist = r.bits(5)? as usize + 1;
    let hclen = r.bits(4)? as usize + 4;
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut cl_lengths = [0u8; 19];
    for i in 0..hclen {
        cl_lengths[ORDER[i]] = r.bits(3)? as u8;
    }
    let cl = Huffman::new(&cl_lengths);
    let mut lengths: Vec<u8> = Vec::with_capacity(hlit + hdist);
    while lengths.len() < hlit + hdist {
        let sym = cl.decode(r)?;
        match sym {
            0..=15 => lengths.push(sym as u8),
            16 => {
                let prev = *lengths.last()?;
                let rep = r.bits(2)? + 3;
                for _ in 0..rep {
                    lengths.push(prev);
                }
            }
            17 => {
                let rep = r.bits(3)? + 3;
                for _ in 0..rep {
                    lengths.push(0);
                }
            }
            18 => {
                let rep = r.bits(7)? + 11;
                for _ in 0..rep {
                    lengths.push(0);
                }
            }
            _ => return None,
        }
    }
    let lit = Huffman::new(&lengths[..hlit]);
    let dist = Huffman::new(&lengths[hlit..hlit + hdist]);
    Some((lit, dist))
}

fn inflate_block(r: &mut BitReader, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman) -> Option<()> {
    loop {
        let sym = lit.decode(r)?;
        if sym == 256 {
            return Some(());
        }
        if sym < 256 {
            out.push(sym as u8);
        } else {
            let s = (sym - 257) as usize;
            if s >= 29 {
                return None;
            }
            let len = LEN_BASE[s] as usize + r.bits(LEN_EXTRA[s] as u32)? as usize;
            let dsym = dist.decode(r)? as usize;
            if dsym >= 30 {
                return None;
            }
            let d = DIST_BASE[dsym] as usize + r.bits(DIST_EXTRA[dsym] as u32)? as usize;
            if d == 0 || d > out.len() {
                return None;
            }
            let start = out.len() - d;
            for i in 0..len {
                out.push(out[start + i]);
            }
        }
    }
}

pub fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut r = BitReader::new(data);
    let mut out = Vec::new();
    loop {
        let bfinal = r.bit()?;
        let btype = r.bits(2)?;
        match btype {
            0 => {
                r.align_byte();
                let len = r.read_u16_le()? as usize;
                let _nlen = r.read_u16_le()?;
                for _ in 0..len {
                    out.push(r.read_byte()?);
                }
            }
            1 => inflate_block(&mut r, &mut out, &fixed_lit(), &fixed_dist())?,
            2 => {
                let (lit, dist) = read_dynamic(&mut r)?;
                inflate_block(&mut r, &mut out, &lit, &dist)?;
            }
            _ => return None,
        }
        if bfinal == 1 {
            break;
        }
    }
    Some(out)
}

/// zlib 스트림(2바이트 헤더 + DEFLATE + adler32). 헤더/체크섬은 건너뜀.
pub fn zlib_decompress(data: &[u8]) -> Option<Vec<u8>> {
    if data.len() < 2 {
        return None;
    }
    inflate(&data[2..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn inflate_simple() {
        // python3: zlib.compress(b"hello hello hello world").hex()
        let z = hex("789ccb48cdc9c957c84022cbf38b725200687d08c5");
        let out = zlib_decompress(&z).unwrap();
        assert_eq!(out, b"hello hello hello world");
    }

    #[test]
    fn inflate_repeated_backrefs() {
        // python3: zlib.compress(b"a"*300).hex()  — 길이/거리 백레퍼런스
        let z = hex("789c4b4c1c05c40200d8a871ad");
        let out = zlib_decompress(&z).unwrap();
        assert_eq!(out, vec![b'a'; 300]);
    }
}
