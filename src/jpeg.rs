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

// JPEG 디코드 (베이스라인 SOF0/SOF1 + **프로그레시브 SOF2**).
//
// 프로그레시브는 스캔이 여러 번 온다: 각 스캔이 계수의 일부(스펙트럼 밴드)나 하위 비트를
// 채운다 (T.81 부속서 G). 그래서 계수를 버퍼에 누적하고, 모든 스캔이 끝난 뒤에 한 번만
// 역양자화 + 역DCT 한다. 예전엔 SOF2 를 만나면 그냥 None 이었다 — 프로그레시브 JPEG 은
// 웹에서 아주 흔하다 (tailwindcss.com 의 이미지가 그래서 조용히 사라졌다).
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
    let mut progressive = false;
    // 컴포넌트별 계수 버퍼 (블록당 64, 자연순서). 스캔들이 여기에 누적된다.
    let mut coeffs: Vec<Vec<i32>> = Vec::new();
    let mut blocks_per_line: Vec<usize> = Vec::new();
    let mut blocks_per_col: Vec<usize> = Vec::new();
    let mut comp_blocks: Vec<(usize, usize)> = Vec::new();
    let (mut hmax, mut vmax) = (1usize, 1usize);
    let (mut mcus_x, mut mcus_y) = (0usize, 0usize);

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
            break; // EOI
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
            // SOF0/SOF1 (순차) / SOF2 (프로그레시브) — 전부 허프만 부호화
            0xC0 | 0xC1 | 0xC2 => {
                progressive = marker == 0xC2;
                if seg.len() < 6 || seg[0] != 8 {
                    return None; // 8비트 정밀도만
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
                hmax = comps.iter().map(|c| c.h).max()?;
                vmax = comps.iter().map(|c| c.v).max()?;
                mcus_x = width.div_ceil(hmax * 8);
                mcus_y = height.div_ceil(vmax * 8);
                // 블록 격자: MCU 격자에 맞춰 컴포넌트마다 (MCU 경계까지 패딩)
                coeffs.clear();
                blocks_per_line.clear();
                blocks_per_col.clear();
                comp_blocks.clear();
                for c in &comps {
                    let bl = mcus_x * c.h;
                    let bc = mcus_y * c.v;
                    blocks_per_line.push(bl);
                    blocks_per_col.push(bc);
                    // 컴포넌트의 실제 샘플 크기 → 실제 블록 수 (비인터리브 스캔용)
                    let cw = (width * c.h).div_ceil(hmax);
                    let ch = (height * c.v).div_ceil(vmax);
                    comp_blocks.push((cw.div_ceil(8), ch.div_ceil(8)));
                    coeffs.push(vec![0i32; bl * bc * 64]);
                }
            }
            // 산술 부호화/무손실/계층 프레임은 여전히 미지원 (정직하게 실패)
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => return None,
            0xC4 => {
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
                if comps.is_empty() || seg.is_empty() {
                    return None;
                }
                let ns = seg[0] as usize;
                if ns == 0 || ns > comps.len() || seg.len() < 1 + 2 * ns + 3 {
                    return None;
                }
                let mut scan: Vec<usize> = Vec::new();
                for i in 0..ns {
                    let cs = seg[1 + 2 * i];
                    let tdta = seg[2 + 2 * i];
                    let ci = comps.iter().position(|c| c.id == cs)?;
                    comps[ci].td = (tdta >> 4) as usize;
                    comps[ci].ta = (tdta & 15) as usize;
                    scan.push(ci);
                }
                let ss = seg[1 + 2 * ns] as usize;
                let se = seg[2 + 2 * ns] as usize;
                let a = seg[3 + 2 * ns];
                let (ah, al) = ((a >> 4) as u32, (a & 15) as u32);
                if se > 63 || ss > se {
                    return None;
                }
                // 이 스캔의 엔트로피 데이터를 계수 버퍼에 채운다
                let consumed = decode_scan(
                    &data[seg_end..],
                    &scan,
                    &comps,
                    &dc_tables,
                    &ac_tables,
                    &mut coeffs,
                    &blocks_per_line,
                    &blocks_per_col,
                    &comp_blocks,
                    mcus_x,
                    mcus_y,
                    restart_interval,
                    progressive,
                    ss,
                    se,
                    ah,
                    al,
                )?;
                pos = seg_end + consumed;
                continue;
            }
            _ => {} // APPn/COM 등
        }
        pos = seg_end;
    }

    if comps.is_empty() || width == 0 {
        return None;
    }

    // ── 역양자화 + 역DCT → 평면 ──
    let mut planes: Vec<Plane> = comps
        .iter()
        .enumerate()
        .map(|(i, _)| {
            let w = blocks_per_line[i] * 8;
            let h = blocks_per_col[i] * 8;
            Plane { w, h, data: vec![0u8; w * h] }
        })
        .collect();
    for (ci, c) in comps.iter().enumerate() {
        let qt = &qtables[c.tq];
        let bl = blocks_per_line[ci];
        let bc = blocks_per_col[ci];
        for by in 0..bc {
            for bx in 0..bl {
                let base = (by * bl + bx) * 64;
                let mut blk = [0i32; 64];
                // 역양자화: 계수는 자연순서, 양자화표는 지그재그 순서
                for k in 0..64 {
                    blk[ZIGZAG[k]] = coeffs[ci][base + ZIGZAG[k]] * qt[k] as i32;
                }
                let px = idct_8x8(&blk);
                let plane = &mut planes[ci];
                let (ox, oy) = (bx * 8, by * 8);
                for y in 0..8 {
                    let row = (oy + y) * plane.w + ox;
                    plane.data[row..row + 8].copy_from_slice(&px[y * 8..y * 8 + 8]);
                }
            }
        }
    }

    finish(width, height, &comps, &planes, hmax, vmax)
}

// 스캔 하나를 디코드해 계수 버퍼를 채운다. 소비한 바이트 수를 돌려준다.
#[allow(clippy::too_many_arguments)]
fn decode_scan(
    data: &[u8],
    scan: &[usize],
    comps: &[Component],
    dc_tables: &[Option<Huffman>; 4],
    ac_tables: &[Option<Huffman>; 4],
    coeffs: &mut [Vec<i32>],
    blocks_per_line: &[usize],
    _blocks_per_col: &[usize],
    // 컴포넌트의 **실제** 블록 수 (MCU 패딩 제외) — 비인터리브 스캔이 쓴다
    comp_blocks: &[(usize, usize)],
    mcus_x: usize,
    mcus_y: usize,
    restart_interval: usize,
    progressive: bool,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
) -> Option<usize> {
    let mut r = BitReader::new(data);
    let mut dc_pred = vec![0i32; comps.len()];
    let mut eobrun: i32 = 0;
    let mut unit = 0usize; // MCU(인터리브) 또는 블록(비인터리브) 카운터

    // 블록 하나를 디코드 (progressive 여부에 따라 다른 규칙)
    #[allow(clippy::too_many_arguments)]
    fn one_block(
        r: &mut BitReader,
        coef: &mut [i32],
        dc: Option<&Huffman>,
        ac: Option<&Huffman>,
        dc_pred: &mut i32,
        eobrun: &mut i32,
        progressive: bool,
        ss: usize,
        se: usize,
        ah: u32,
        al: u32,
    ) -> Option<()> {
        if !progressive {
            // 베이스라인: DC + AC 를 한 번에 (계수는 자연순서로 저장)
            let dc = dc?;
            let ac = ac?;
            let t = dc.decode(r)? as u32;
            let diff = if t > 0 { extend(r.read_bits(t)? as i32, t) } else { 0 };
            *dc_pred += diff;
            coef[0] = *dc_pred;
            let mut k = 1usize;
            while k <= 63 {
                let rs = ac.decode(r)?;
                let run = (rs >> 4) as usize;
                let size = (rs & 0x0F) as u32;
                if size == 0 {
                    if run == 15 {
                        k += 16;
                        continue;
                    }
                    break; // EOB
                }
                k += run;
                if k > 63 {
                    return None;
                }
                coef[ZIGZAG[k]] = extend(r.read_bits(size)? as i32, size);
                k += 1;
            }
            return Some(());
        }
        // ── 프로그레시브 (T.81 G.1.2) ──
        if ss == 0 {
            if ah == 0 {
                // DC 첫 스캔
                let dc = dc?;
                let t = dc.decode(r)? as u32;
                let diff = if t > 0 { extend(r.read_bits(t)? as i32, t) } else { 0 };
                *dc_pred += diff;
                coef[0] = *dc_pred << al;
            } else {
                // DC 정제: 하위 비트 하나
                if r.read_bit()? == 1 {
                    coef[0] |= 1 << al;
                }
            }
            return Some(());
        }
        let ac = ac?;
        let p1: i32 = 1 << al;
        let m1: i32 = -1i32 << al;
        if ah == 0 {
            // AC 첫 스캔
            if *eobrun > 0 {
                *eobrun -= 1;
                return Some(());
            }
            let mut k = ss;
            while k <= se {
                let rs = ac.decode(r)?;
                let run = (rs >> 4) as usize;
                let size = (rs & 0x0F) as u32;
                if size == 0 {
                    if run < 15 {
                        *eobrun = (1i32 << run) - 1;
                        if run > 0 {
                            *eobrun += r.read_bits(run as u32)? as i32;
                        }
                        break;
                    }
                    k += 16;
                    continue;
                }
                k += run;
                if k > se {
                    return None;
                }
                coef[ZIGZAG[k]] = extend(r.read_bits(size)? as i32, size) << al;
                k += 1;
            }
            return Some(());
        }
        // AC 정제 스캔: 이미 0 이 아닌 계수는 정정 비트로 갱신하고,
        // 새로 0 이 아니게 되는 계수는 run 을 세어 자리를 찾는다.
        let mut k = ss;
        if *eobrun <= 0 {
            while k <= se {
                let rs = ac.decode(r)?;
                let mut run = (rs >> 4) as i32;
                let size = rs & 0x0F;
                let mut new_val = 0i32;
                if size == 0 {
                    if run < 15 {
                        *eobrun = (1i32 << run) - 1;
                        if run > 0 {
                            *eobrun += r.read_bits(run as u32)? as i32;
                        }
                        break;
                    }
                    // run == 15: 0 인 계수 16개를 건너뛴다
                } else {
                    // 정제 스캔의 크기는 항상 1
                    new_val = if r.read_bit()? == 1 { p1 } else { m1 };
                }
                while k <= se {
                    let z = ZIGZAG[k];
                    if coef[z] != 0 {
                        // 이미 0 이 아닌 계수 → 정정 비트
                        if r.read_bit()? == 1 && (coef[z] & p1) == 0 {
                            coef[z] += if coef[z] >= 0 { p1 } else { m1 };
                        }
                    } else {
                        if run == 0 {
                            if new_val != 0 {
                                coef[z] = new_val;
                            }
                            k += 1;
                            break;
                        }
                        run -= 1;
                    }
                    k += 1;
                }
            }
        }
        if *eobrun > 0 {
            // EOB 구간: 남은 밴드의 0 아닌 계수만 정정한다
            while k <= se {
                let z = ZIGZAG[k];
                if coef[z] != 0 && r.read_bit()? == 1 && (coef[z] & p1) == 0 {
                    coef[z] += if coef[z] >= 0 { p1 } else { m1 };
                }
                k += 1;
            }
            *eobrun -= 1;
        }
        Some(())
    }

    if scan.len() == 1 {
        // 비인터리브: 그 컴포넌트의 블록 격자를 래스터 순서로 (T.81 §A.2.2)
        let ci = scan[0];
        let c = comps[ci];
        let _ = (mcus_x, mcus_y);
        // 비인터리브 스캔의 블록 격자는 **컴포넌트의 실제 크기** 기준이다 (T.81 §A.2.2).
        // MCU 패딩까지 읽으면 데이터가 통째로 어긋난다.
        let bl_pad = blocks_per_line[ci];
        let bl = comp_blocks[ci].0;
        let bh = comp_blocks[ci].1;
        for by in 0..bh {
            for bx in 0..bl {
                if restart_interval > 0 && unit > 0 && unit % restart_interval == 0 {
                    r.sync_restart()?;
                    dc_pred.iter_mut().for_each(|p| *p = 0);
                    eobrun = 0;
                }
                let base = (by * bl_pad + bx) * 64;
                let coef = &mut coeffs[ci][base..base + 64];
                one_block(
                    &mut r,
                    coef,
                    dc_tables[c.td].as_ref(),
                    ac_tables[c.ta].as_ref(),
                    &mut dc_pred[ci],
                    &mut eobrun,
                    progressive,
                    ss,
                    se,
                    ah,
                    al,
                )?;
                unit += 1;
            }
        }
    } else {
        for my in 0..mcus_y {
            for mx in 0..mcus_x {
                if restart_interval > 0 && unit > 0 && unit % restart_interval == 0 {
                    r.sync_restart()?;
                    dc_pred.iter_mut().for_each(|p| *p = 0);
                    eobrun = 0;
                }
                for &ci in scan {
                    let c = comps[ci];
                    for bv in 0..c.v {
                        for bh in 0..c.h {
                            let bx = mx * c.h + bh;
                            let by = my * c.v + bv;
                            let bl = blocks_per_line[ci];
                            let base = (by * bl + bx) * 64;
                            let coef = &mut coeffs[ci][base..base + 64];
                            one_block(
                                &mut r,
                                coef,
                                dc_tables[c.td].as_ref(),
                                ac_tables[c.ta].as_ref(),
                                &mut dc_pred[ci],
                                &mut eobrun,
                                progressive,
                                ss,
                                se,
                                ah,
                                al,
                            )?;
                        }
                    }
                }
                unit += 1;
            }
        }
    }

    // 이 스캔이 소비한 바이트: 다음 마커까지 (엔트로피 데이터 안의 FF00/RSTn 은 건너뛴다)
    let mut i = r.pos;
    while i + 1 < data.len() {
        if data[i] == 0xFF {
            let m = data[i + 1];
            if m != 0x00 && m != 0xFF && !(0xD0..=0xD7).contains(&m) {
                break;
            }
        }
        i += 1;
    }
    Some(i)
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

    // 프로그레시브 JPEG (SOF2). 스캔이 여러 번 와서 계수의 일부/하위 비트를 채운다.
    // 예전엔 SOF2 를 만나면 그냥 None 이었다 — 프로그레시브는 웹에서 아주 흔하고,
    // 그런 이미지는 **조용히 사라졌다** (tailwindcss.com 이 그랬다).
    // 비트스트림을 손으로 세지 않고 기계적으로 조립한다.
    #[test]
    fn progressive_dc_first_refine_and_ac_scan() {
        fn seg(marker: u8, body: &[u8]) -> Vec<u8> {
            let mut v = vec![0xFF, marker];
            v.extend(((body.len() + 2) as u16).to_be_bytes());
            v.extend_from_slice(body);
            v
        }
        let mut j: Vec<u8> = vec![0xFF, 0xD8]; // SOI
        // DQT: 표 0, 전부 1 (계수 = 값)
        let mut dqt = vec![0x00];
        dqt.extend(std::iter::repeat_n(1u8, 64));
        j.extend(seg(0xDB, &dqt));
        // SOF2: 8비트, 8x8, 컴포넌트 1개 (h=v=1, tq=0)
        j.extend(seg(0xC2, &[8, 0, 8, 0, 8, 1, 1, 0x11, 0]));
        // DHT DC 표 0: 길이 1 짜리 코드 하나 → 심볼 4 (t=4: 4비트 읽기)
        let mut dht = vec![0x00];
        dht.extend([1u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        dht.push(4);
        j.extend(seg(0xC4, &dht));
        // DHT AC 표 1: 길이 1 → 심볼 0x04(run0,size4), 길이 2 → 심볼 0x00(EOB)
        let mut dhtac = vec![0x11];
        dhtac.extend([1u8, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        dhtac.extend([0x04, 0x00]);
        j.extend(seg(0xC4, &dhtac));

        // 스캔 1: DC 첫 (ss=0,se=0,ah=0,al=1). 비트: '0'(t=4) + 1101(=13) → coef=13<<1=26
        j.extend(seg(0xDA, &[1, 1, 0x00, 0, 0, 0x01]));
        j.push(0b0110_1111);
        // 스캔 2: DC 정제 (ah=1,al=0). 비트 '1' → coef |= 1 → 27
        j.extend(seg(0xDA, &[1, 1, 0x00, 0, 0, 0x10]));
        j.push(0b1000_0000);
        // 스캔 3: AC 첫 (ss=1,se=63,ah=0,al=0), AC 표 1.
        // 비트: '0'(run0,size4) + 1100(=12) + '10'(EOB) → coef[zigzag[1]] = 12
        j.extend(seg(0xDA, &[1, 1, 0x01, 1, 63, 0x00]));
        j.push(0b0110_0101);
        j.extend([0xFF, 0xD9]); // EOI

        let img = decode(&j).expect("프로그레시브 JPEG 디코드");
        assert_eq!((img.width, img.height), (8, 8));
        let px = |x: usize, y: usize| img.rgba[(y * 8 + x) * 4] as i32;
        // DC 27 (= 13<<1 | 1) → 평균 밝기는 128 + 27/8 ≈ 131
        let mean: i32 = (0..64).map(|i| img.rgba[i * 4] as i32).sum::<i32>() / 64;
        assert!(
            (mean - 131).abs() <= 1,
            "DC 첫 스캔 + 정제가 27 을 만들어야: 평균 {}",
            mean
        );
        // AC 계수(수평 주파수 u=1)가 실렸으면 좌우가 **대칭으로 반대** 방향이다.
        // 계수 12 → 화소 진폭은 ±2~3 정도다 (기저 함수 크기).
        assert!(
            px(0, 0) > mean && px(7, 0) < mean && (px(0, 0) - px(7, 0)) >= 3,
            "AC 스캔이 반영돼야: 좌 {} / 우 {} (평균 {})",
            px(0, 0),
            px(7, 0),
            mean
        );
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
