// 베이스라인 JPEG(JFIF) 디코더 — 0부터 자작.
// 파이프라인: 마커 파싱 → 허프만 엔트로피 디코드 → 역양자화 → 역DCT →
//             크로마 업샘플링 → YCbCr→RGB → RGBA Image.
// 지원: 베이스라인 순차(SOF0), 8비트, 1~3 컴포넌트, 4:4:4/4:2:2/4:2:0 등 정수 샘플링.
// 미지원: 프로그레시브(SOF2), 산술 부호화, 12비트 → None 반환.

use std::sync::OnceLock;

// 지그재그 순서: ZIGZAG[k] = 지그재그 스캔 k 번째 계수의 래스터(행우선) 인덱스.
pub const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, //
    17, 24, 32, 25, 18, 11, 4, 5, //
    12, 19, 26, 33, 40, 48, 41, 34, //
    27, 20, 13, 6, 7, 14, 21, 28, //
    35, 42, 49, 56, 57, 50, 43, 36, //
    29, 22, 15, 23, 30, 37, 44, 51, //
    58, 59, 52, 45, 38, 31, 39, 46, //
    53, 60, 61, 54, 47, 55, 62, 63,
];

// 8x8 IDCT 코사인 기저: basis[x][u] = C(u)·cos((2x+1)uπ/16), C(0)=1/√2 else 1.
fn idct_basis() -> &'static [[f32; 8]; 8] {
    static B: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    B.get_or_init(|| {
        let mut t = [[0.0f32; 8]; 8];
        for (x, row) in t.iter_mut().enumerate() {
            for (u, cell) in row.iter_mut().enumerate() {
                let cu = if u == 0 { 1.0 / std::f32::consts::SQRT_2 } else { 1.0 };
                *cell = cu
                    * ((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / 16.0).cos();
            }
        }
        t
    })
}

// 8x8 역DCT + 레벨 시프트(+128) + [0,255] 클램프.
// 입력: 역양자화된 계수(래스터 순서). 출력: 공간영역 픽셀값.
pub fn idct_8x8(block: &[i32; 64]) -> [u8; 64] {
    let b = idct_basis();
    let mut out = [0u8; 64];
    for y in 0..8 {
        for x in 0..8 {
            let mut sum = 0.0f32;
            for v in 0..8 {
                for u in 0..8 {
                    sum += block[v * 8 + u] as f32 * b[x][u] * b[y][v];
                }
            }
            let val = (sum / 4.0).round() as i32 + 128;
            out[y * 8 + x] = val.clamp(0, 255) as u8;
        }
    }
    out
}

fn clamp8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

// JFIF 풀레인지 YCbCr → RGB.
pub fn ycbcr_to_rgb(yy: f32, cb: f32, cr: f32) -> (u8, u8, u8) {
    let r = yy + 1.402 * (cr - 128.0);
    let g = yy - 0.344136 * (cb - 128.0) - 0.714136 * (cr - 128.0);
    let b = yy + 1.772 * (cb - 128.0);
    (clamp8(r), clamp8(g), clamp8(b))
}

// ── 엔트로피 비트 리더 ──────────────────────────────────────────────
// 스캔 데이터의 MSB-first 비트 스트림. 0xFF 0x00 스터핑을 해제하고,
// 마커(0xFF + 비영)를 만나면 데이터 끝으로 취급한다.
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    buf: u32,
    cnt: u32,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> BitReader<'a> {
        BitReader { data, pos: 0, buf: 0, cnt: 0 }
    }

    fn read_bit(&mut self) -> Option<u32> {
        if self.cnt == 0 {
            let b = *self.data.get(self.pos)?;
            if b == 0xFF {
                if *self.data.get(self.pos + 1)? != 0x00 {
                    return None; // 마커 → 엔트로피 데이터 끝
                }
                self.pos += 2; // FF 00 → 데이터 바이트 FF
            } else {
                self.pos += 1;
            }
            self.buf = b as u32;
            self.cnt = 8;
        }
        self.cnt -= 1;
        Some((self.buf >> self.cnt) & 1)
    }

    fn read_bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit()?;
        }
        Some(v)
    }

    // 재시작 마커(RSTn) 소비: 바이트 정렬 후 FF Dn 을 기대한다.
    fn sync_restart(&mut self) -> Option<()> {
        self.cnt = 0;
        // 마커 앞 fill 바이트(FF FF ...) 허용
        while self.data.get(self.pos) == Some(&0xFF) && self.data.get(self.pos + 1) == Some(&0xFF)
        {
            self.pos += 1;
        }
        if *self.data.get(self.pos)? == 0xFF {
            let m = *self.data.get(self.pos + 1)?;
            if (0xD0..=0xD7).contains(&m) {
                self.pos += 2;
                return Some(());
            }
        }
        None
    }
}

// ── 허프만 테이블 (ITU-T T.81 Annex F 정준 코드) ────────────────────
struct Huffman {
    mincode: [i32; 17],
    maxcode: [i32; 17], // -1 = 해당 길이 코드 없음
    valptr: [usize; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn build(counts: &[u8; 16], values: Vec<u8>) -> Huffman {
        let mut mincode = [0i32; 17];
        let mut maxcode = [-1i32; 17];
        let mut valptr = [0usize; 17];
        let mut code = 0i32;
        let mut k = 0usize;
        for len in 1..=16 {
            let n = counts[len - 1] as usize;
            if n > 0 {
                valptr[len] = k;
                mincode[len] = code;
                code += n as i32;
                maxcode[len] = code - 1;
                k += n;
            }
            code <<= 1;
        }
        Huffman { mincode, maxcode, valptr, values }
    }

    fn decode(&self, r: &mut BitReader) -> Option<u8> {
        let mut code = 0i32;
        for len in 1..=16 {
            code = (code << 1) | r.read_bit()? as i32;
            if code <= self.maxcode[len] {
                let idx = self.valptr[len] + (code - self.mincode[len]) as usize;
                return self.values.get(idx).copied();
            }
        }
        None
    }
}

// s 비트 값 v 의 부호 확장 (T.81 F.2.2.1 EXTEND).
fn extend(v: i32, s: u32) -> i32 {
    if s == 0 {
        0
    } else if v < (1 << (s - 1)) {
        v - (1 << s) + 1
    } else {
        v
    }
}

#[derive(Clone, Copy)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
}

struct Plane {
    w: usize,
    h: usize,
    data: Vec<u8>,
}

// 8x8 블록 하나: 허프만 → 역양자화 → 역DCT.
fn decode_block(
    r: &mut BitReader,
    dc: &Huffman,
    ac: &Huffman,
    qt: &[u16; 64],
    dc_pred: &mut i32,
) -> Option<[u8; 64]> {
    let mut coef = [0i32; 64];
    // DC
    let t = dc.decode(r)? as u32;
    let diff = if t > 0 { extend(r.read_bits(t)? as i32, t) } else { 0 };
    *dc_pred += diff;
    coef[0] = *dc_pred * qt[0] as i32;
    // AC (런렝스: 상위 4비트 = 0 의 개수, 하위 4비트 = 값 비트수)
    let mut k = 1usize;
    while k <= 63 {
        let rs = ac.decode(r)?;
        let run = (rs >> 4) as usize;
        let size = (rs & 0x0F) as u32;
        if size == 0 {
            if run == 15 {
                k += 16; // ZRL: 0 열여섯 개
                continue;
            }
            break; // EOB
        }
        k += run;
        if k > 63 {
            return None;
        }
        let v = extend(r.read_bits(size)? as i32, size);
        coef[ZIGZAG[k]] = v * qt[k] as i32;
        k += 1;
    }
    Some(idct_8x8(&coef))
}

// 베이스라인 JPEG 디코드. 실패/미지원이면 None.
pub fn decode(data: &[u8]) -> Option<crate::png::Image> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut pos = 2usize;
    let mut qtables = [[0u16; 64]; 4];
    let mut dc_tables: [Option<Huffman>; 4] = [None, None, None, None];
    let mut ac_tables: [Option<Huffman>; 4] = [None, None, None, None];
    let mut comps: Vec<Component> = Vec::new();
    let mut width = 0usize;
    let mut height = 0usize;
    let mut restart_interval = 0usize;
    let mut scan_order: Vec<usize> = Vec::new();
    let scan_start;

    // ── 세그먼트 루프: SOS 를 만날 때까지 ──
    loop {
        if pos + 2 > data.len() || data[pos] != 0xFF {
            return None;
        }
        let marker = data[pos + 1];
        pos += 2;
        if marker == 0xFF {
            pos -= 1; // fill 바이트
            continue;
        }
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue; // 길이 없는 독립 마커
        }
        if marker == 0xD9 {
            return None; // 스캔 전에 EOI
        }
        if pos + 2 > data.len() {
            return None;
        }
        let len = u16::from_be_bytes([data[pos], data[pos + 1]]) as usize;
        if len < 2 || pos + len > data.len() {
            return None;
        }
        let seg = &data[pos + 2..pos + len];
        let seg_end = pos + len;
        match marker {
            0xDB => {
                // DQT: 테이블 여러 개 가능
                let mut o = 0;
                while o < seg.len() {
                    let pq = (seg[o] >> 4) as usize;
                    let tq = (seg[o] & 15) as usize;
                    o += 1;
                    if tq >= 4 {
                        return None;
                    }
                    if pq == 0 {
                        if o + 64 > seg.len() {
                            return None;
                        }
                        for i in 0..64 {
                            qtables[tq][i] = seg[o + i] as u16;
                        }
                        o += 64;
                    } else {
                        if o + 128 > seg.len() {
                            return None;
                        }
                        for i in 0..64 {
                            qtables[tq][i] =
                                u16::from_be_bytes([seg[o + 2 * i], seg[o + 2 * i + 1]]);
                        }
                        o += 128;
                    }
                }
            }
            0xC0 | 0xC1 => {
                // SOF0/SOF1 (순차, 허프만)
                if seg.len() < 6 || seg[0] != 8 {
                    return None;
                }
                height = u16::from_be_bytes([seg[1], seg[2]]) as usize;
                width = u16::from_be_bytes([seg[3], seg[4]]) as usize;
                let n = seg[5] as usize;
                if width == 0 || height == 0 || !(n == 1 || n == 3) || seg.len() < 6 + 3 * n {
                    return None;
                }
                comps.clear();
                for i in 0..n {
                    let b = &seg[6 + 3 * i..];
                    let h = (b[1] >> 4) as usize;
                    let v = (b[1] & 15) as usize;
                    if h == 0 || h > 4 || v == 0 || v > 4 {
                        return None;
                    }
                    comps.push(Component {
                        id: b[0],
                        h,
                        v,
                        tq: (b[2] & 3) as usize,
                        td: 0,
                        ta: 0,
                    });
                }
            }
            // 프로그레시브(C2)/무손실/산술 부호화 프레임은 미지원
            0xC2 | 0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            0xC4 => {
                // DHT: 테이블 여러 개 가능
                let mut o = 0;
                while o + 17 <= seg.len() {
                    let tc = (seg[o] >> 4) as usize;
                    let th = (seg[o] & 15) as usize;
                    if tc > 1 || th >= 4 {
                        return None;
                    }
                    let mut counts = [0u8; 16];
                    counts.copy_from_slice(&seg[o + 1..o + 17]);
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    if o + 17 + total > seg.len() {
                        return None;
                    }
                    let table = Huffman::build(&counts, seg[o + 17..o + 17 + total].to_vec());
                    if tc == 0 {
                        dc_tables[th] = Some(table);
                    } else {
                        ac_tables[th] = Some(table);
                    }
                    o += 17 + total;
                }
            }
            0xDD => {
                if seg.len() < 2 {
                    return None;
                }
                restart_interval = u16::from_be_bytes([seg[0], seg[1]]) as usize;
            }
            0xDA => {
                // SOS: 인터리브드 단일 스캔만 지원
                if comps.is_empty() || seg.is_empty() {
                    return None;
                }
                let ns = seg[0] as usize;
                if ns != comps.len() || seg.len() < 1 + 2 * ns + 3 {
                    return None;
                }
                for i in 0..ns {
                    let cs = seg[1 + 2 * i];
                    let tdta = seg[2 + 2 * i];
                    let ci = comps.iter().position(|c| c.id == cs)?;
                    comps[ci].td = (tdta >> 4) as usize;
                    comps[ci].ta = (tdta & 15) as usize;
                    scan_order.push(ci);
                }
                scan_start = seg_end;
                break;
            }
            _ => {} // APPn/COM 등은 길이만큼 스킵
        }
        pos = seg_end;
    }

    // ── MCU 격자와 컴포넌트 평면 ──
    let hmax = comps.iter().map(|c| c.h).max()?;
    let vmax = comps.iter().map(|c| c.v).max()?;
    let mcus_x = width.div_ceil(hmax * 8);
    let mcus_y = height.div_ceil(vmax * 8);
    let mut planes: Vec<Plane> = comps
        .iter()
        .map(|c| {
            let w = mcus_x * c.h * 8;
            let h = mcus_y * c.v * 8;
            Plane { w, h, data: vec![0u8; w * h] }
        })
        .collect();

    // ── 엔트로피 디코드 ──
    let mut reader = BitReader::new(&data[scan_start..]);
    let mut dc_pred = vec![0i32; comps.len()];
    let mut mcu_count = 0usize;

    // 스캔에 컴포넌트가 하나뿐이면 **비인터리브**다: MCU = 데이터 유닛 1개이고,
    // 블록은 그 컴포넌트 자기 크기 기준 래스터 순서로 온다 (T.81 §A.2.2).
    // 그레이스케일 JPEG 이 h=2,v=2 로 인코딩돼 오는 경우가 흔한데, 이걸 인터리브로
    // 취급하면 한 MCU 에서 4블록을 읽어 데이터가 통째로 어긋난다 (디코드 실패).
    if scan_order.len() == 1 {
        let ci = scan_order[0];
        let c = comps[ci];
        let dc = dc_tables[c.td].as_ref()?;
        let ac = ac_tables[c.ta].as_ref()?;
        // 이 컴포넌트의 실제 샘플 크기 → 블록 격자
        let cw = (width * c.h).div_ceil(hmax);
        let ch = (height * c.v).div_ceil(vmax);
        let bw = cw.div_ceil(8);
        let bh_n = ch.div_ceil(8);
        for by in 0..bh_n {
            for bx in 0..bw {
                if restart_interval > 0 && mcu_count > 0 && mcu_count % restart_interval == 0 {
                    reader.sync_restart()?;
                    dc_pred.iter_mut().for_each(|p| *p = 0);
                }
                let px = decode_block(&mut reader, dc, ac, &qtables[c.tq], &mut dc_pred[ci])?;
                let plane = &mut planes[ci];
                let (ox, oy) = (bx * 8, by * 8);
                for y in 0..8 {
                    if oy + y >= plane.h {
                        break;
                    }
                    let row = (oy + y) * plane.w + ox;
                    let n = 8.min(plane.w.saturating_sub(ox));
                    plane.data[row..row + n].copy_from_slice(&px[y * 8..y * 8 + n]);
                }
                mcu_count += 1;
            }
        }
        return finish(width, height, &comps, &planes, hmax, vmax);
    }

    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart_interval > 0 && mcu_count > 0 && mcu_count % restart_interval == 0 {
                reader.sync_restart()?;
                dc_pred.iter_mut().for_each(|p| *p = 0);
            }
            for &ci in &scan_order {
                let c = comps[ci];
                let dc = dc_tables[c.td].as_ref()?;
                let ac = ac_tables[c.ta].as_ref()?;
                for bv in 0..c.v {
                    for bh in 0..c.h {
                        let px = decode_block(&mut reader, dc, ac, &qtables[c.tq], &mut dc_pred[ci])?;
                        let bx = (mx * c.h + bh) * 8;
                        let by = (my * c.v + bv) * 8;
                        let plane = &mut planes[ci];
                        for y in 0..8 {
                            let row = (by + y) * plane.w + bx;
                            plane.data[row..row + 8].copy_from_slice(&px[y * 8..y * 8 + 8]);
                        }
                    }
                }
            }
            mcu_count += 1;
        }
    }

    finish(width, height, &comps, &planes, hmax, vmax)
}

// 업샘플링(최근접) + 색변환 → RGBA. 컴포넌트가 1개면 그레이스케일.
fn finish(
    width: usize,
    height: usize,
    comps: &[Component],
    planes: &[Plane],
    hmax: usize,
    vmax: usize,
) -> Option<crate::png::Image> {
    let sample = |ci: usize, x: usize, y: usize| -> u8 {
        let c = comps[ci];
        let p = &planes[ci];
        let sx = (x * c.h / hmax).min(p.w - 1);
        let sy = (y * c.v / vmax).min(p.h - 1);
        p.data[sy * p.w + sx]
    };
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = if comps.len() == 1 {
                let v = sample(0, x, y);
                (v, v, v)
            } else {
                ycbcr_to_rgb(sample(0, x, y) as f32, sample(1, x, y) as f32, sample(2, x, y) as f32)
            };
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Some(crate::png::Image { width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grayscale_single_component_scan_decodes() {
        // 1채널(그레이스케일) JPEG 이 h=2,v=2 로 인코딩돼 오는 경우가 흔하다.
        // 스캔에 컴포넌트가 하나면 **비인터리브**라서 MCU = 데이터 유닛 1개다 (T.81 §A.2.2).
        // 인터리브로 취급하면 한 MCU 에서 4블록을 읽어 데이터가 어긋나고 디코드가 실패한다.
        // (위키피디아의 흑백 스캔 이미지가 통째로 안 나왔다.)
        let bytes = std::fs::read("assets/test/gray_h2v2.jpg").unwrap();
        let img = decode(&bytes).expect("그레이스케일 JPEG 디코드");
        assert_eq!((img.width, img.height), (250, 289));
        // 실제 계조가 있어야 한다 (단색이면 디코드가 깨진 것)
        let mut levels = std::collections::HashSet::new();
        for y in (0..img.height).step_by(7) {
            for x in (0..img.width).step_by(7) {
                levels.insert(img.rgba[(y * img.width + x) * 4]);
            }
        }
        assert!(levels.len() > 20, "계조가 있어야 (단색이면 실패): {}", levels.len());
        // 그레이스케일 → R=G=B
        let o = (100 * img.width + 100) * 4;
        assert_eq!(img.rgba[o], img.rgba[o + 1]);
        assert_eq!(img.rgba[o + 1], img.rgba[o + 2]);
    }

    #[test]
    fn zigzag_is_a_permutation() {
        let mut sorted = ZIGZAG;
        sorted.sort_unstable();
        let expected: Vec<usize> = (0..64).collect();
        assert_eq!(sorted.to_vec(), expected);
        assert_eq!(ZIGZAG[0], 0);
        assert_eq!(ZIGZAG[1], 1);
        assert_eq!(ZIGZAG[2], 8); // 첫 아래칸
        assert_eq!(ZIGZAG[63], 63);
    }

    #[test]
    fn idct_zero_block_is_mid_gray() {
        let block = [0i32; 64];
        let out = idct_8x8(&block);
        assert!(out.iter().all(|&p| p == 128), "0 계수 → 레벨시프트 128");
    }

    #[test]
    fn idct_dc_only_is_uniform() {
        let mut block = [0i32; 64];
        block[0] = 8; // DC 항 → f = 8/8 = 1, +128 = 129
        let out = idct_8x8(&block);
        assert!(out.iter().all(|&p| p == 129), "DC-only → 균일 129, got {:?}", &out[..8]);
    }

    #[test]
    fn ycbcr_gray_is_identity() {
        assert_eq!(ycbcr_to_rgb(128.0, 128.0, 128.0), (128, 128, 128));
    }

    #[test]
    fn ycbcr_red() {
        let (r, g, b) = ycbcr_to_rgb(76.0, 85.0, 255.0);
        assert!(r > 250 && g < 8 && b < 8, "순수 빨강 근사, got ({},{},{})", r, g, b);
    }

    #[test]
    fn rejects_garbage_and_truncated() {
        assert!(decode(b"not a jpeg").is_none());
        assert!(decode(&[0xFF, 0xD8]).is_none());
        let real = std::fs::read("assets/test/red16.jpg").unwrap();
        assert!(decode(&real[..real.len() / 2]).is_none(), "잘린 파일은 None");
    }

    // 픽셀 근거: 픽스처는 알려진 PPM 에서 인코딩됨 (red16: 단색 255,0,0 /
    // grad16: R=x*16, G=y*16, B=128). 손실 압축이므로 허용오차로 검증.
    #[test]
    fn decodes_solid_red_16x16() {
        let bytes = std::fs::read("assets/test/red16.jpg").unwrap();
        let img = decode(&bytes).expect("red16.jpg 디코드 실패");
        assert_eq!((img.width, img.height), (16, 16));
        for (x, y) in [(0, 0), (15, 0), (0, 15), (15, 15), (8, 8)] {
            let i = (y * 16 + x) * 4;
            let (r, g, b) = (img.rgba[i], img.rgba[i + 1], img.rgba[i + 2]);
            assert!(
                r >= 220 && g <= 60 && b <= 60,
                "({},{}) 은 빨강이어야 함, got ({},{},{})",
                x, y, r, g, b
            );
            assert_eq!(img.rgba[i + 3], 255);
        }
    }

    #[test]
    fn decodes_gradient_16x16() {
        let bytes = std::fs::read("assets/test/grad16.jpg").unwrap();
        let img = decode(&bytes).expect("grad16.jpg 디코드 실패");
        assert_eq!((img.width, img.height), (16, 16));
        let px = |x: usize, y: usize| {
            let i = (y * 16 + x) * 4;
            (img.rgba[i] as i32, img.rgba[i + 1] as i32, img.rgba[i + 2] as i32)
        };
        // 방향성: R 은 x 로, G 는 y 로 증가 (원본 R=x*16, G=y*16)
        assert!(px(15, 8).0 > px(0, 8).0 + 100, "R 은 오른쪽으로 증가: {:?} vs {:?}", px(15, 8), px(0, 8));
        assert!(px(8, 15).1 > px(8, 0).1 + 100, "G 는 아래로 증가: {:?} vs {:?}", px(8, 15), px(8, 0));
        // 중앙 근사: 원본 (128,128,128)
        let (r, g, b) = px(8, 8);
        for (name, v) in [("r", r), ("g", g), ("b", b)] {
            assert!((v - 128).abs() <= 48, "중앙 {} 은 128 근처여야 함, got {:?}", name, (r, g, b));
        }
    }
}
