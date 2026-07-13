// VP8L — WebP 무손실 (WebP Lossless Bitstream Specification).
//
// 왜 필요한가: 두 가지가 이것 없이는 안 된다.
//   1. .webp 무손실 이미지 (로고·아이콘). 지금은 디코드 실패로 빈다.
//   2. **투명 lossy webp 의 알파**. ALPH 청크의 압축 방식 1 이 이 포맷이다.
//      알파를 못 읽으면 투명 이미지를 불투명하게 그린다 — 조용히 틀린 그림이다.
//
// 구성: LSB 비트리더 → 변환 목록 → 허프만 그룹(메타 허프만) → LZ77+컬러캐시 픽셀 복호
//       → 역변환(예측/교차색/녹색빼기/색인). 상수는 libwebp 에서 기계 추출(vp8l_tables).

use crate::vp8l_tables::{CODE_LENGTH_CODE_ORDER, CODE_TO_PLANE};

const NUM_LITERAL: usize = 256;
const NUM_LENGTH: usize = 24;
const NUM_DISTANCE: usize = 40;
const NUM_CODE_LENGTH_CODES: usize = 19;
const CODE_LENGTH_LITERALS: usize = 16;

// ── 비트리더 (LSB 우선) ────────────────────────────────────────────────────
struct Br<'a> {
    d: &'a [u8],
    pos: usize, // 비트 위치
}

impl<'a> Br<'a> {
    fn new(d: &'a [u8]) -> Self {
        Br { d, pos: 0 }
    }

    fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let byte = self.d.get(self.pos / 8).copied().unwrap_or(0);
            let bit = (byte >> (self.pos % 8)) & 1;
            v |= (bit as u32) << i;
            self.pos += 1;
        }
        v
    }

    fn eof(&self) -> bool {
        self.pos > self.d.len() * 8
    }
}

// ── 정준 허프만 ────────────────────────────────────────────────────────────
struct Huff {
    counts: [u32; 16],
    symbols: Vec<u16>,
    single: Option<u16>, // 심볼이 하나뿐이면 비트를 소비하지 않는다 (표준)
}

impl Huff {
    fn build(lengths: &[u8]) -> Option<Huff> {
        let mut counts = [0u32; 16];
        let mut nonzero = 0;
        let mut only = 0u16;
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                if l as usize > 15 {
                    return None;
                }
                counts[l as usize] += 1;
                nonzero += 1;
                only = s as u16;
            }
        }
        if nonzero == 0 {
            return None;
        }
        if nonzero == 1 {
            return Some(Huff { counts, symbols: vec![only], single: Some(only) });
        }
        // 오프셋 계산 후 심볼을 코드 길이 순으로 정렬
        let mut offs = [0usize; 16];
        for i in 1..15 {
            offs[i + 1] = offs[i] + counts[i] as usize;
        }
        let mut symbols = vec![0u16; nonzero];
        for (s, &l) in lengths.iter().enumerate() {
            if l > 0 {
                symbols[offs[l as usize]] = s as u16;
                offs[l as usize] += 1;
            }
        }
        Some(Huff { counts, symbols, single: None })
    }

    fn decode(&self, br: &mut Br) -> Option<u16> {
        if let Some(s) = self.single {
            return Some(s);
        }
        let (mut code, mut first, mut index) = (0i32, 0i32, 0usize);
        for len in 1..16 {
            code |= br.bits(1) as i32;
            let count = self.counts[len] as i32;
            if code - first < count {
                return self.symbols.get(index + (code - first) as usize).copied();
            }
            index += count as usize;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

// 허프만 코드 하나 읽기 (단순 코드 또는 코드길이 코딩)
fn read_huffman(br: &mut Br, alphabet_size: usize) -> Option<Huff> {
    let mut lengths = vec![0u8; alphabet_size];
    if br.bits(1) == 1 {
        // 단순 코드: 심볼 1~2개
        let num = br.bits(1) + 1;
        let first_len_code = br.bits(1);
        let s0 = br.bits(if first_len_code == 0 { 1 } else { 8 }) as usize;
        if s0 >= alphabet_size {
            return None;
        }
        lengths[s0] = 1;
        if num == 2 {
            let s1 = br.bits(8) as usize;
            if s1 >= alphabet_size {
                return None;
            }
            lengths[s1] = 1;
        }
        return Huff::build(&lengths);
    }
    // 코드길이 코딩 (19개 코드길이 코드 → 심볼 코드길이)
    let mut cl_lengths = [0u8; NUM_CODE_LENGTH_CODES];
    let num_codes = br.bits(4) as usize + 4;
    if num_codes > NUM_CODE_LENGTH_CODES {
        return None;
    }
    for i in 0..num_codes {
        cl_lengths[CODE_LENGTH_CODE_ORDER[i]] = br.bits(3) as u8;
    }
    let cl_huff = Huff::build(&cl_lengths)?;

    let mut max_symbol = if br.bits(1) == 1 {
        let nbits = 2 + 2 * br.bits(3);
        2 + br.bits(nbits) as usize
    } else {
        alphabet_size
    };

    let mut prev_len = 8u8; // DEFAULT_CODE_LENGTH
    let mut sym = 0usize;
    while sym < alphabet_size {
        if max_symbol == 0 {
            break;
        }
        max_symbol -= 1;
        if br.eof() {
            return None;
        }
        let code_len = cl_huff.decode(br)? as usize;
        if code_len < CODE_LENGTH_LITERALS {
            lengths[sym] = code_len as u8;
            sym += 1;
            if code_len != 0 {
                prev_len = code_len as u8;
            }
        } else {
            // 16: 직전 길이 반복(3~6), 17: 0 반복(3~10), 18: 0 반복(11~138)
            let slot = code_len - CODE_LENGTH_LITERALS;
            let (extra, offset) = match slot {
                0 => (2u32, 3usize),
                1 => (3, 3),
                _ => (7, 11),
            };
            let mut repeat = br.bits(extra) as usize + offset;
            if sym + repeat > alphabet_size {
                return None;
            }
            let val = if slot == 0 { prev_len } else { 0 };
            while repeat > 0 {
                lengths[sym] = val;
                sym += 1;
                repeat -= 1;
            }
        }
    }
    Huff::build(&lengths)
}

// ── 접두 코드 (길이/거리 공용) ─────────────────────────────────────────────
fn copy_distance(sym: u16, br: &mut Br) -> usize {
    let s = sym as usize;
    if s < 4 {
        return s + 1;
    }
    let extra = ((s - 2) >> 1) as u32;
    let offset = (2 + (s & 1)) << extra;
    offset + br.bits(extra) as usize + 1
}

fn plane_to_distance(xsize: usize, plane_code: usize) -> usize {
    if plane_code > 120 {
        return plane_code - 120;
    }
    let dc = CODE_TO_PLANE[plane_code - 1] as usize;
    let yoff = dc >> 4;
    let xoff = 8 - (dc & 0xf);
    let dist = yoff * xsize + xoff;
    // xsize 가 아주 작으면 음수가 될 수 있다 (libwebp 와 동일하게 1 로 보정)
    if dist >= 1 {
        dist
    } else {
        1
    }
}

// ── 변환 ──────────────────────────────────────────────────────────────────
enum Transform {
    Predictor { bits: u32, xsize: usize, ysize: usize, data: Vec<u32> },
    CrossColor { bits: u32, xsize: usize, ysize: usize, data: Vec<u32> },
    SubtractGreen,
    ColorIndex { bits: u32, xsize: usize, ysize: usize, map: Vec<u32> },
}

fn subsample(size: usize, bits: u32) -> usize {
    (size + (1 << bits) - 1) >> bits
}

// ── 공개 진입점 ────────────────────────────────────────────────────────────
// VP8L 청크 본문 (5바이트 헤더 포함)
pub fn decode(data: &[u8]) -> Option<crate::png::Image> {
    let mut br = Br::new(data);
    if br.bits(8) != 0x2f {
        return None; // 시그니처
    }
    let w = br.bits(14) as usize + 1;
    let h = br.bits(14) as usize + 1;
    let _alpha_used = br.bits(1);
    if br.bits(3) != 0 {
        return None; // 버전
    }
    let argb = decode_image_stream(&mut br, w, h, true)?;
    if argb.len() < w * h {
        return None;
    }
    let mut rgba = Vec::with_capacity(w * h * 4);
    for p in argb.iter().take(w * h) {
        rgba.push(((p >> 16) & 0xff) as u8);
        rgba.push(((p >> 8) & 0xff) as u8);
        rgba.push((p & 0xff) as u8);
        rgba.push((p >> 24) as u8);
    }
    Some(crate::png::Image { width: w, height: h, rgba })
}

// ALPH 청크의 압축 방식 1: 헤더 없는 무손실 스트림. 알파는 **녹색 채널**에 들어 있다.
pub fn decode_alpha(data: &[u8], w: usize, h: usize) -> Option<Vec<u8>> {
    let mut br = Br::new(data);
    let argb = decode_image_stream(&mut br, w, h, true)?;
    if argb.len() < w * h {
        return None;
    }
    Some(argb.iter().take(w * h).map(|p| ((p >> 8) & 0xff) as u8).collect())
}

// ── 이미지 스트림 ──────────────────────────────────────────────────────────
fn decode_image_stream(br: &mut Br, xsize: usize, ysize: usize, level0: bool) -> Option<Vec<u32>> {
    let mut xs = xsize;
    let mut transforms: Vec<Transform> = Vec::new();

    if level0 {
        while br.bits(1) == 1 {
            let t = br.bits(2);
            match t {
                0 | 1 => {
                    let bits = br.bits(3) + 2;
                    let bw = subsample(xs, bits);
                    let bh = subsample(ysize, bits);
                    let data = decode_image_stream(br, bw, bh, false)?;
                    let tr = if t == 0 {
                        Transform::Predictor { bits, xsize: xs, ysize, data }
                    } else {
                        Transform::CrossColor { bits, xsize: xs, ysize, data }
                    };
                    transforms.push(tr);
                }
                2 => transforms.push(Transform::SubtractGreen),
                _ => {
                    let num_colors = br.bits(8) as usize + 1;
                    let bits = if num_colors > 16 {
                        0
                    } else if num_colors > 4 {
                        1
                    } else if num_colors > 2 {
                        2
                    } else {
                        3
                    };
                    let table = decode_image_stream(br, num_colors, 1, false)?;
                    // 색표는 성분별 누적합으로 저장돼 있다 (delta 코딩)
                    let final_colors = 1usize << (8 >> bits);
                    let mut map = vec![0u32; final_colors];
                    let mut bytes = vec![0u8; final_colors * 4];
                    for (i, c) in table.iter().enumerate().take(num_colors) {
                        bytes[i * 4] = (c >> 24) as u8; // a
                        bytes[i * 4 + 1] = ((c >> 16) & 0xff) as u8; // r
                        bytes[i * 4 + 2] = ((c >> 8) & 0xff) as u8; // g
                        bytes[i * 4 + 3] = (c & 0xff) as u8; // b
                    }
                    for i in 4..(4 * num_colors) {
                        bytes[i] = bytes[i].wrapping_add(bytes[i - 4]);
                    }
                    for (i, m) in map.iter_mut().enumerate() {
                        *m = ((bytes[i * 4] as u32) << 24)
                            | ((bytes[i * 4 + 1] as u32) << 16)
                            | ((bytes[i * 4 + 2] as u32) << 8)
                            | bytes[i * 4 + 3] as u32;
                    }
                    transforms.push(Transform::ColorIndex { bits, xsize: xs, ysize, map });
                    xs = subsample(xs, bits);
                }
            }
            if transforms.len() > 4 {
                return None; // 변환은 종류당 한 번 (최대 4)
            }
        }
    }

    // 컬러 캐시
    let mut cache_bits = 0u32;
    if br.bits(1) == 1 {
        cache_bits = br.bits(4);
        if !(1..=11).contains(&cache_bits) {
            return None;
        }
    }
    let cache_size = if cache_bits > 0 { 1usize << cache_bits } else { 0 };

    // 메타 허프만 (level0 에서만)
    let mut huff_bits = 0u32;
    let mut huff_image: Vec<u32> = Vec::new();
    let mut huff_xs = 0usize;
    let mut num_groups = 1usize;
    if level0 && br.bits(1) == 1 {
        huff_bits = br.bits(3) + 2;
        huff_xs = subsample(xs, huff_bits);
        let hh = subsample(ysize, huff_bits);
        let hi = decode_image_stream(br, huff_xs, hh, false)?;
        // 메타 인덱스 = (빨강 << 8) | 초록
        huff_image = hi.iter().map(|p| (p >> 8) & 0xffff).collect();
        num_groups = huff_image.iter().max().copied().unwrap_or(0) as usize + 1;
        if num_groups > 4096 {
            return None;
        }
    }

    // 그룹당 허프만 트리 5개 (초록+길이+캐시, 빨강, 파랑, 알파, 거리)
    let mut groups: Vec<[Huff; 5]> = Vec::with_capacity(num_groups);
    for _ in 0..num_groups {
        let g = read_huffman(br, NUM_LITERAL + NUM_LENGTH + cache_size)?;
        let r = read_huffman(br, NUM_LITERAL)?;
        let b = read_huffman(br, NUM_LITERAL)?;
        let a = read_huffman(br, NUM_LITERAL)?;
        let d = read_huffman(br, NUM_DISTANCE)?;
        groups.push([g, r, b, a, d]);
        if br.eof() {
            return None;
        }
    }

    // 픽셀 복호 (LZ77 + 컬러 캐시)
    let total = xs.checked_mul(ysize)?;
    if total > 64 * 1024 * 1024 {
        return None; // 병적으로 큰 이미지 방어
    }
    let mut pixels = vec![0u32; total];
    let mut cache = vec![0u32; cache_size];
    let insert = |cache: &mut Vec<u32>, argb: u32| {
        if cache_bits > 0 {
            let key = (0x1e35_a7bdu32.wrapping_mul(argb)) >> (32 - cache_bits);
            cache[key as usize] = argb;
        }
    };

    let mut pos = 0usize;
    while pos < total {
        if br.eof() {
            return None;
        }
        let (x, y) = (pos % xs, pos / xs);
        let gi = if num_groups > 1 {
            let hx = x >> huff_bits;
            let hy = y >> huff_bits;
            *huff_image.get(hy * huff_xs + hx)? as usize
        } else {
            0
        };
        let g = groups.get(gi)?;
        let s = g[0].decode(br)? as usize;
        if s < NUM_LITERAL {
            // 리터럴 (초록은 이미 읽음)
            let red = g[1].decode(br)? as u32;
            let blue = g[2].decode(br)? as u32;
            let alpha = g[3].decode(br)? as u32;
            let argb = (alpha << 24) | (red << 16) | ((s as u32) << 8) | blue;
            pixels[pos] = argb;
            insert(&mut cache, argb);
            pos += 1;
        } else if s < NUM_LITERAL + NUM_LENGTH {
            // 역참조 복사
            let length = copy_distance((s - NUM_LITERAL) as u16, br);
            let dsym = g[4].decode(br)?;
            let dcode = copy_distance(dsym, br);
            let dist = plane_to_distance(xs, dcode);
            if dist > pos || pos + length > total {
                return None;
            }
            for i in 0..length {
                let v = pixels[pos - dist + i];
                pixels[pos + i] = v;
                insert(&mut cache, v);
            }
            pos += length;
        } else {
            // 컬러 캐시 조회
            let key = s - NUM_LITERAL - NUM_LENGTH;
            let v = *cache.get(key)?;
            pixels[pos] = v;
            pos += 1;
        }
    }

    // 역변환 (읽은 역순)
    for t in transforms.iter().rev() {
        pixels = inverse_transform(t, pixels)?;
    }
    Some(pixels)
}

// ── 역변환 구현 ────────────────────────────────────────────────────────────
fn avg2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefe_fefe) >> 1).wrapping_add(a & b)
}

fn avg3(a: u32, b: u32, c: u32) -> u32 {
    avg2(avg2(a, c), b)
}

fn avg4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    avg2(avg2(a, b), avg2(c, d))
}

fn clip255(v: i32) -> u32 {
    v.clamp(0, 255) as u32
}

fn add_sub_full(a: i32, b: i32, c: i32) -> u32 {
    clip255(a + b - c)
}

fn clamped_add_sub_full(c0: u32, c1: u32, c2: u32) -> u32 {
    let comp = |sh: u32| {
        add_sub_full(
            ((c0 >> sh) & 0xff) as i32,
            ((c1 >> sh) & 0xff) as i32,
            ((c2 >> sh) & 0xff) as i32,
        )
    };
    (comp(24) << 24) | (comp(16) << 16) | (comp(8) << 8) | comp(0)
}

fn clamped_add_sub_half(c0: u32, c1: u32, c2: u32) -> u32 {
    let ave = avg2(c0, c1);
    let comp = |sh: u32| {
        let a = ((ave >> sh) & 0xff) as i32;
        let b = ((c2 >> sh) & 0xff) as i32;
        clip255(a + (a - b) / 2)
    };
    (comp(24) << 24) | (comp(16) << 16) | (comp(8) << 8) | comp(0)
}

fn sub3(a: i32, b: i32, c: i32) -> i32 {
    (b - c).abs() - (a - c).abs()
}

fn select(a: u32, b: u32, c: u32) -> u32 {
    let comp = |sh: u32| {
        sub3(
            ((a >> sh) & 0xff) as i32,
            ((b >> sh) & 0xff) as i32,
            ((c >> sh) & 0xff) as i32,
        )
    };
    let pa_minus_pb = comp(24) + comp(16) + comp(8) + comp(0);
    if pa_minus_pb <= 0 {
        a
    } else {
        b
    }
}

// 성분별 덧셈 (mod 256)
fn add_pixels(a: u32, b: u32) -> u32 {
    let alpha = ((a >> 24) & 0xff).wrapping_add((b >> 24) & 0xff) & 0xff;
    let red = ((a >> 16) & 0xff).wrapping_add((b >> 16) & 0xff) & 0xff;
    let green = ((a >> 8) & 0xff).wrapping_add((b >> 8) & 0xff) & 0xff;
    let blue = (a & 0xff).wrapping_add(b & 0xff) & 0xff;
    (alpha << 24) | (red << 16) | (green << 8) | blue
}

fn predict(mode: u32, left: u32, top: &[u32], ti: usize) -> u32 {
    // top[ti-1] = 왼쪽위, top[ti] = 위, top[ti+1] = 오른쪽위
    let tl = top[ti - 1];
    let t = top[ti];
    let tr = top[ti + 1];
    match mode {
        0 => 0xff00_0000, // 검정
        1 => left,
        2 => t,
        3 => tr,
        4 => tl,
        5 => avg3(left, t, tr),
        6 => avg2(left, tl),
        7 => avg2(left, t),
        8 => avg2(tl, t),
        9 => avg2(t, tr),
        10 => avg4(left, tl, t, tr),
        11 => select(t, left, tl),
        12 => clamped_add_sub_full(left, t, tl),
        _ => clamped_add_sub_half(left, t, tl),
    }
}

fn inverse_transform(t: &Transform, src: Vec<u32>) -> Option<Vec<u32>> {
    match t {
        Transform::SubtractGreen => Some(
            src.iter()
                .map(|&p| {
                    let g = (p >> 8) & 0xff;
                    let r = ((p >> 16) & 0xff).wrapping_add(g) & 0xff;
                    let b = (p & 0xff).wrapping_add(g) & 0xff;
                    (p & 0xff00_ff00) | (r << 16) | b
                })
                .collect(),
        ),
        Transform::Predictor { bits, xsize, ysize, data } => {
            let (w, h) = (*xsize, *ysize);
            if src.len() < w * h {
                return None;
            }
            let bw = subsample(w, *bits);
            let mut out = vec![0u32; w * h];
            // 위쪽 행을 가장자리 포함해 들고 다닌다 (top[-1], top[w] 접근)
            let mut top = vec![0u32; w + 2];
            for y in 0..h {
                let mut left;
                for x in 0..w {
                    let residual = src[y * w + x];
                    let pred = if x == 0 && y == 0 {
                        0xff00_0000
                    } else if y == 0 {
                        out[y * w + x - 1] // 첫 행: 왼쪽
                    } else if x == 0 {
                        out[(y - 1) * w] // 첫 열: 위
                    } else {
                        let mode =
                            (data.get((y >> bits) * bw + (x >> bits)).copied().unwrap_or(0) >> 8)
                                & 0xf;
                        left = out[y * w + x - 1];
                        // top 슬라이스: 인덱스 1 = 위, 0 = 왼쪽위, 2 = 오른쪽위
                        top[0] = out[(y - 1) * w + x - 1];
                        top[1] = out[(y - 1) * w + x];
                        // 마지막 열의 "오른쪽위"는 **현재 행의 첫 화소**로 감긴다.
                        // libwebp 가 top 포인터를 out + x - width 로 잡아서 생기는 규칙인데,
                        // 이걸 이전 행의 첫 화소로 착각하면 예측이 어긋난다.
                        top[2] = if x + 1 < w { out[(y - 1) * w + x + 1] } else { out[y * w] };
                        predict(mode, left, &top, 1)
                    };
                    out[y * w + x] = add_pixels(residual, pred);
                }
            }
            Some(out)
        }
        Transform::CrossColor { bits, xsize, ysize, data } => {
            let (w, h) = (*xsize, *ysize);
            if src.len() < w * h {
                return None;
            }
            let bw = subsample(w, *bits);
            let delta = |pred: i8, color: i8| -> i32 { ((pred as i32) * (color as i32)) >> 5 };
            let mut out = vec![0u32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let argb = src[y * w + x];
                    let m = data.get((y >> bits) * bw + (x >> bits)).copied().unwrap_or(0);
                    let g2r = m as i8; // 하위 8비트
                    let g2b = (m >> 8) as i8;
                    let r2b = (m >> 16) as i8;
                    let green = (argb >> 8) as i8;
                    let mut new_red = ((argb >> 16) & 0xff) as i32;
                    let mut new_blue = (argb & 0xff) as i32;
                    new_red += delta(g2r, green);
                    new_red &= 0xff;
                    new_blue += delta(g2b, green);
                    new_blue += delta(r2b, new_red as i8);
                    new_blue &= 0xff;
                    out[y * w + x] =
                        (argb & 0xff00_ff00) | ((new_red as u32) << 16) | (new_blue as u32);
                }
            }
            Some(out)
        }
        Transform::ColorIndex { bits, xsize, ysize, map } => {
            let (w, h) = (*xsize, *ysize);
            let bits_per_pixel = 8 >> bits;
            let packed_w = subsample(w, *bits);
            let mut out = vec![0u32; w * h];
            for y in 0..h {
                if bits_per_pixel < 8 {
                    let per_byte = 1usize << bits;
                    let mask = (1u32 << bits_per_pixel) - 1;
                    let mut packed = 0u32;
                    for x in 0..w {
                        if x % per_byte == 0 {
                            let s = src.get(y * packed_w + x / per_byte).copied().unwrap_or(0);
                            packed = (s >> 8) & 0xff; // 인덱스는 녹색 채널
                        }
                        let idx = (packed & mask) as usize;
                        out[y * w + x] = map.get(idx).copied().unwrap_or(0);
                        packed >>= bits_per_pixel;
                    }
                } else {
                    for x in 0..w {
                        let s = src.get(y * w + x).copied().unwrap_or(0);
                        let idx = ((s >> 8) & 0xff) as usize;
                        out[y * w + x] = map.get(idx).copied().unwrap_or(0);
                    }
                }
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn huffman_single_symbol_consumes_no_bits() {
        // 심볼이 하나뿐인 트리는 비트를 소비하지 않는다 (표준). 이걸 틀리면
        // 이후 모든 비트가 밀려서 그림이 통째로 뭉개진다.
        let mut lengths = vec![0u8; 8];
        lengths[3] = 1;
        let h = Huff::build(&lengths).unwrap();
        let data = [0xffu8; 4];
        let mut br = Br::new(&data);
        assert_eq!(h.decode(&mut br), Some(3));
        assert_eq!(br.pos, 0, "비트를 소비하면 안 된다");
    }

    #[test]
    fn distance_mapping_matches_spec() {
        // 근거리 코드는 (x, y) 오프셋 평면으로 매핑된다 (거리 1..120)
        assert_eq!(plane_to_distance(100, 1), 100); // 0x18 → y=1, x=0 → 100
        assert_eq!(plane_to_distance(100, 121), 1); // 120 초과는 그대로 - 120
        assert_eq!(plane_to_distance(100, 122), 2);
    }
}
