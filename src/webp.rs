// WebP (VP8 lossy) 디코더. RFC 6386.
//
// 왜 필요한가: 실제 사이트가 .webp 를 URL 에 박아 쓴다 (react.dev 만 해도 17곳).
// Accept 헤더로 png/jpeg 만 광고해도, 하드코딩된 .webp 는 그대로 온다. 디코드 못 하면
// 그림이 통째로 빈다.
//
// 표(확률/양자화)는 손으로 옮기지 않았다 — RFC 6386 본문에서 기계적으로 추출했다
// (src/vp8_tables.rs). 한 숫자만 틀려도 비트스트림 해석이 통째로 어긋난다.

use crate::vp8_tables::*;

// ── RIFF 컨테이너 ──────────────────────────────────────────────────────────
// RIFF....WEBP + 청크들. VP8 (lossy) / VP8L (lossless) / VP8X (확장) + ALPH.
pub fn decode(data: &[u8]) -> Option<crate::png::Image> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return None;
    }
    let mut i = 12;
    let mut vp8: Option<&[u8]> = None;
    let mut alph: Option<&[u8]> = None;
    while i + 8 <= data.len() {
        let fourcc = &data[i..i + 4];
        let size = u32::from_le_bytes([data[i + 4], data[i + 5], data[i + 6], data[i + 7]]) as usize;
        let body_start = i + 8;
        let body_end = body_start.checked_add(size)?;
        if body_end > data.len() {
            break;
        }
        match fourcc {
            b"VP8 " => vp8 = Some(&data[body_start..body_end]),
            b"ALPH" => alph = Some(&data[body_start..body_end]),
            // VP8L(무손실)은 별도 포맷 — 아직 미구현이면 정직하게 실패한다.
            b"VP8L" => return None,
            _ => {}
        }
        i = body_end + (size & 1); // 청크는 짝수 정렬
    }
    let frame = vp8?;
    let mut img = decode_vp8(frame)?;
    if let Some(a) = alph {
        apply_alpha(&mut img, a);
    }
    Some(img)
}

// ALPH 청크: 압축 방식 0(무압축)만 처리. 1(무손실 압축)이면 알파를 불투명으로 둔다
// (그림은 나오고 투명도만 없다 — 조용히 틀린 색을 내는 것보다 낫다).
fn apply_alpha(img: &mut crate::png::Image, alph: &[u8]) {
    if alph.is_empty() {
        return;
    }
    let method = alph[0] & 0x03;
    if method != 0 {
        return;
    }
    let n = img.width * img.height;
    if alph.len() < 1 + n {
        return;
    }
    for p in 0..n {
        img.rgba[p * 4 + 3] = alph[1 + p];
    }
}

// ── 불리언 산술 디코더 (RFC 6386 §7) ───────────────────────────────────────
struct BoolDec<'a> {
    buf: &'a [u8],
    pos: usize,
    range: u32,
    value: u32,
    bit_count: i32,
}

impl<'a> BoolDec<'a> {
    fn new(buf: &'a [u8]) -> Self {
        let mut d = BoolDec { buf, pos: 0, range: 255, value: 0, bit_count: -8 };
        d.value = (d.byte() as u32) << 8 | d.byte() as u32;
        d.bit_count = 0;
        d
    }

    fn byte(&mut self) -> u8 {
        let b = self.buf.get(self.pos).copied().unwrap_or(0);
        self.pos += 1;
        b
    }

    fn get(&mut self, prob: u8) -> u32 {
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        let big_split = split << 8;
        let bit = if self.value >= big_split {
            self.range -= split;
            self.value -= big_split;
            1
        } else {
            self.range = split;
            0
        };
        // 정규화
        while self.range < 128 {
            self.value <<= 1;
            self.range <<= 1;
            self.bit_count += 1;
            if self.bit_count == 8 {
                self.bit_count = 0;
                self.value |= self.byte() as u32;
            }
        }
        bit
    }

    fn get_bit(&mut self) -> u32 {
        self.get(128)
    }

    fn get_uint(&mut self, bits: u32) -> u32 {
        let mut v = 0;
        for _ in 0..bits {
            v = (v << 1) | self.get_bit();
        }
        v
    }

    // 부호 있는 값: 크기 비트 뒤에 부호 비트
    fn get_signed(&mut self, bits: u32) -> i32 {
        let v = self.get_uint(bits) as i32;
        if self.get_bit() == 1 {
            -v
        } else {
            v
        }
    }

    // 트리 디코딩 (RFC 6386 §8). 음수 잎, 양수 내부 노드 인덱스.
    fn tree(&mut self, tree: &[i8], probs: &[u8], start: usize) -> i32 {
        let mut i = start as i32;
        loop {
            let b = self.get(probs[(i >> 1) as usize]) as usize;
            let t = tree[i as usize + b];
            if t <= 0 {
                return -(t as i32);
            }
            i = t as i32;
        }
    }
}

// ── 트리 (RFC 6386 §8, §11) ────────────────────────────────────────────────
// 잎은 음수(-mode), 내부 노드는 다음 인덱스.
const KF_YMODE_TREE: [i8; 8] = [-4, 2, 4, 6, -0, -1, -2, -3]; // B_PRED=4, DC,V,H,TM = 0..3
const UV_MODE_TREE: [i8; 6] = [-0, 2, -1, 4, -2, -3]; // DC, V, H, TM
// 모드 번호: DC=0 TM=1 VE=2 HE=3 LD=4 RD=5 VR=6 VL=7 HD=8 HU=9 (RFC 열거 순서)
const BMODE_TREE: [i8; 18] = [
    -0, 2, // B_DC_PRED = "0"
    -1, 4, // B_TM_PRED = "10"
    -2, 6, // B_VE_PRED = "110"
    8, 12, //
    -3, 10, // B_HE_PRED = "11100"
    -5, -6, // B_RD_PRED, B_VR_PRED  ← 잎은 RD(5), VR(6) 이다 (LD 가 아니다)
    -4, 14, // B_LD_PRED = "111110"
    -7, 16, // B_VL_PRED
    -8, -9, // B_HD_PRED, B_HU_PRED
];

// DCT 토큰 트리 (RFC 6386 §13.2). 토큰 값: 0=DCT_0, 1..4=리터럴, 5..10=cat1..cat6, 11=EOB.
// 잎은 -토큰값이다. (0 = DCT_0 도 잎 — 내부 노드 인덱스 0 은 루트뿐이라 모호하지 않다.)
const TOKEN_TREE: [i8; 22] = [
    -11, 2, // eob = "0"
    -0, 4, // DCT_0 = "10"
    -1, 6, // DCT_1 = "110"
    8, 12, //
    -2, 10, // DCT_2
    -3, -4, // DCT_3, DCT_4
    14, 16, //
    -5, -6, // cat1, cat2
    18, 20, //
    -7, -8, // cat3, cat4
    -9, -10, // cat5, cat6
];

// 토큰 → 값의 기준. cat1..cat6 은 추가 비트를 더한다 (폭 1,2,3,4,5,11).
const TOKEN_BASE: [i32; 11] = [0, 1, 2, 3, 4, 5, 7, 11, 19, 35, 67];

fn pcat(t: usize) -> &'static [u8] {
    match t {
        5 => &PCAT1,
        6 => &PCAT2,
        7 => &PCAT3,
        8 => &PCAT4,
        9 => &PCAT5,
        _ => &PCAT6,
    }
}

const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

// ── 프레임 ────────────────────────────────────────────────────────────────
struct Quant {
    y_dc: i32,
    y_ac: i32,
    y2_dc: i32,
    y2_ac: i32,
    uv_dc: i32,
    uv_ac: i32,
}

fn clamp_q(i: i32) -> usize {
    i.clamp(0, 127) as usize
}

fn make_quant(base: i32, y_dc_d: i32, y2_dc_d: i32, y2_ac_d: i32, uv_dc_d: i32, uv_ac_d: i32) -> Quant {
    Quant {
        y_dc: DC_QLOOKUP[clamp_q(base + y_dc_d)],
        y_ac: AC_QLOOKUP[clamp_q(base)],
        y2_dc: DC_QLOOKUP[clamp_q(base + y2_dc_d)] * 2,
        // Y2 AC 는 155/100 배, 최소 8 (RFC 6386 §14.1)
        y2_ac: (AC_QLOOKUP[clamp_q(base + y2_ac_d)] * 155 / 100).max(8),
        uv_dc: DC_QLOOKUP[clamp_q(base + uv_dc_d)].min(132), // UV DC 는 132 로 제한
        uv_ac: AC_QLOOKUP[clamp_q(base + uv_ac_d)],
    }
}

struct MacroBlock {
    ymode: usize,          // 0..3 = DC/V/H/TM, 4 = B_PRED
    bmodes: [usize; 16],   // B_PRED 일 때 4x4 모드
    uvmode: usize,
    coeffs: [i32; 25 * 16], // Y 16블록 + U 4 + V 4 + Y2 1
    nonzero_y2: bool,
    skip: bool,
}

pub fn decode_vp8(frame: &[u8]) -> Option<crate::png::Image> {
    if frame.len() < 10 {
        return None;
    }
    // 프레임 태그 (3바이트, 리틀엔디언 비트필드)
    let tag = frame[0] as u32 | (frame[1] as u32) << 8 | (frame[2] as u32) << 16;
    let key_frame = (tag & 1) == 0;
    let _version = (tag >> 1) & 7;
    let show_frame = (tag >> 4) & 1;
    let part0_size = (tag >> 5) as usize;
    if !key_frame || show_frame == 0 {
        return None; // 정지 이미지는 키프레임 하나뿐이다
    }
    // 시작 코드 + 크기
    if frame[3] != 0x9d || frame[4] != 0x01 || frame[5] != 0x2a {
        return None;
    }
    let w = (frame[6] as usize | (frame[7] as usize) << 8) & 0x3fff;
    let h = (frame[8] as usize | (frame[9] as usize) << 8) & 0x3fff;
    if w == 0 || h == 0 || w > 16383 || h > 16383 {
        return None;
    }
    let mb_w = w.div_ceil(16);
    let mb_h = h.div_ceil(16);

    let part0_start: usize = 10;
    let part0_end = part0_start.checked_add(part0_size)?;
    if part0_end > frame.len() {
        return None;
    }
    let mut bd = BoolDec::new(&frame[part0_start..part0_end]);

    // 키프레임 헤더
    let _color_space = bd.get_bit();
    let _clamping = bd.get_bit();

    // 세그먼트
    let mut seg_enabled = false;
    let mut seg_quant = [0i32; 4];
    let mut seg_lf = [0i32; 4];
    let mut seg_abs = false;
    let mut seg_tree_probs = [255u8; 3];
    let mut seg_update_map = false;
    if bd.get_bit() == 1 {
        seg_enabled = true;
        let update_map = bd.get_bit();
        seg_update_map = update_map == 1;
        let update_data = bd.get_bit();
        if update_data == 1 {
            seg_abs = bd.get_bit() == 1;
            for q in seg_quant.iter_mut() {
                *q = if bd.get_bit() == 1 { bd.get_signed(7) } else { 0 };
            }
            for l in seg_lf.iter_mut() {
                *l = if bd.get_bit() == 1 { bd.get_signed(6) } else { 0 };
            }
        }
        if update_map == 1 {
            for p in seg_tree_probs.iter_mut() {
                *p = if bd.get_bit() == 1 { bd.get_uint(8) as u8 } else { 255 };
            }
        }
    }

    // 루프 필터
    let filter_simple = bd.get_bit() == 1;
    let filter_level = bd.get_uint(6) as i32;
    let sharpness = bd.get_uint(3) as i32;
    let mut lf_delta_ref = [0i32; 4];
    let mut lf_delta_mode = [0i32; 4];
    let mut lf_deltas = false;
    if bd.get_bit() == 1 {
        lf_deltas = true;
        if bd.get_bit() == 1 {
            for d in lf_delta_ref.iter_mut() {
                if bd.get_bit() == 1 {
                    *d = bd.get_signed(6);
                }
            }
            for d in lf_delta_mode.iter_mut() {
                if bd.get_bit() == 1 {
                    *d = bd.get_signed(6);
                }
            }
        }
    }

    // 파티션 (계수용)
    let nparts = 1usize << bd.get_uint(2);
    let mut parts: Vec<BoolDec> = Vec::with_capacity(nparts);
    {
        let rest = &frame[part0_end..];
        let sizes_len = 3 * (nparts - 1);
        if rest.len() < sizes_len {
            return None;
        }
        let mut off = sizes_len;
        for p in 0..nparts {
            let sz = if p + 1 < nparts {
                let b = &rest[3 * p..3 * p + 3];
                b[0] as usize | (b[1] as usize) << 8 | (b[2] as usize) << 16
            } else {
                rest.len().saturating_sub(off)
            };
            let end = off.checked_add(sz)?;
            if end > rest.len() {
                return None;
            }
            parts.push(BoolDec::new(&rest[off..end]));
            off = end;
        }
    }

    // 양자화기
    let base_q = bd.get_uint(7) as i32;
    let d = |bd: &mut BoolDec| -> i32 {
        if bd.get_bit() == 1 {
            bd.get_signed(4)
        } else {
            0
        }
    };
    let y_dc_d = d(&mut bd);
    let y2_dc_d = d(&mut bd);
    let y2_ac_d = d(&mut bd);
    let uv_dc_d = d(&mut bd);
    let uv_ac_d = d(&mut bd);

    // 키프레임: refresh entropy probs
    let refresh_entropy = bd.get_bit();
    let _ = refresh_entropy;

    // 계수 확률 갱신
    let mut coeff_probs = DEFAULT_COEFF_PROBS;
    for i in 0..4 {
        for j in 0..8 {
            for k in 0..3 {
                for t in 0..11 {
                    let idx = ((i * 8 + j) * 3 + k) * 11 + t;
                    if bd.get(COEFF_UPDATE_PROBS[idx]) == 1 {
                        coeff_probs[idx] = bd.get_uint(8) as u8;
                    }
                }
            }
        }
    }

    let mb_no_coeff_skip = bd.get_bit() == 1;
    let prob_skip = if mb_no_coeff_skip { bd.get_uint(8) as u8 } else { 0 };
    if std::env::var("KESTREL_VP8_DEBUG").is_ok() {
        eprintln!(
            "[vp8] {}x{} mb {}x{} parts={} q={} filter={} simple={} seg={} skip={}({})",
            w, h, mb_w, mb_h, nparts, base_q, filter_level, filter_simple, seg_enabled,
            mb_no_coeff_skip, prob_skip
        );
    }

    // 세그먼트별 양자화기
    let quants: Vec<Quant> = (0..4)
        .map(|s| {
            let q = if !seg_enabled {
                base_q
            } else if seg_abs {
                seg_quant[s]
            } else {
                base_q + seg_quant[s]
            };
            make_quant(q.clamp(0, 127), y_dc_d, y2_dc_d, y2_ac_d, uv_dc_d, uv_ac_d)
        })
        .collect();

    // ── 매크로블록 디코드 ──
    let mut mbs: Vec<MacroBlock> = Vec::with_capacity(mb_w * mb_h);
    let mut segments: Vec<usize> = vec![0; mb_w * mb_h];
    // 4x4 모드 문맥: 위/왼쪽 (mb 격자 × 4)
    let mut above_bmodes = vec![0usize; mb_w * 4];
    let mut left_bmodes;
    // 계수 비영 문맥 (Y 4열 + U 2 + V 2 + Y2 1 = 9)
    let mut above_nz = vec![[false; 9]; mb_w];
    let mut left_nz;

    for my in 0..mb_h {
        left_bmodes = [0; 4];
        left_nz = [false; 9];
        for mx in 0..mb_w {
            let mut mb = MacroBlock {
                ymode: 0,
                bmodes: [0; 16],
                uvmode: 0,
                coeffs: [0; 25 * 16],
                nonzero_y2: false,
                skip: false,
            };
            // 세그먼트 id: **맵 갱신이 켜져 있을 때만** 코딩된다 (RFC 6386 §10, §19.3).
            // seg_enabled 만 보고 무조건 읽으면 비트스트림 동기가 깨진다.
            let seg = if seg_update_map {
                if bd.get(seg_tree_probs[0]) == 0 {
                    bd.get(seg_tree_probs[1]) as usize
                } else {
                    2 + bd.get(seg_tree_probs[2]) as usize
                }
            } else {
                0
            };
            segments[my * mb_w + mx] = seg;

            mb.skip = mb_no_coeff_skip && bd.get(prob_skip) == 1;

            // 예측 모드 (키프레임)
            let ymode = bd.tree(&KF_YMODE_TREE, &KF_YMODE_PROB, 0) as usize;
            mb.ymode = ymode;
            if ymode == 4 {
                // B_PRED: 16개 4x4 모드, 위/왼 문맥
                for b in 0..16 {
                    let (bx, by) = (b % 4, b / 4);
                    let a = if by == 0 {
                        above_bmodes[mx * 4 + bx]
                    } else {
                        mb.bmodes[b - 4]
                    };
                    let l = if bx == 0 {
                        left_bmodes[by]
                    } else {
                        mb.bmodes[b - 1]
                    };
                    let base = (a * 10 + l) * 9;
                    let probs = &KF_BMODE_PROBS[base..base + 9];
                    let m = bd.tree(&BMODE_TREE, probs, 0) as usize;
                    mb.bmodes[b] = m;
                    if by == 3 {
                        above_bmodes[mx * 4 + bx] = m;
                    }
                    if bx == 3 {
                        left_bmodes[by] = m;
                    }
                }
            } else {
                // 16x16 모드 → 등가 4x4 모드로 문맥 채움 (RFC: DC=B_DC, V=B_VE, H=B_HE, TM=B_TM)
                let equiv = [0usize, 2, 3, 1][ymode];
                for bx in 0..4 {
                    above_bmodes[mx * 4 + bx] = equiv;
                }
                left_bmodes = [equiv; 4];
                mb.bmodes = [equiv; 16];
            }
            mb.uvmode = bd.tree(&UV_MODE_TREE, &KF_UV_MODE_PROB, 0) as usize;

            // 계수
            if !mb.skip {
                let part = &mut parts[my % nparts];
                decode_coeffs(
                    part,
                    &coeff_probs,
                    &quants[seg],
                    &mut mb,
                    &mut above_nz[mx],
                    &mut left_nz,
                );
            } else {
                // 스킵: 비영 문맥 리셋 (Y2 는 16x16 모드일 때만 유지)
                let keep_y2 = mb.ymode != 4;
                above_nz[mx] = [false; 9];
                left_nz = [false; 9];
                if keep_y2 {
                    // Y2 문맥은 스킵해도 유지된다 (RFC 6386 §13.1)
                    above_nz[mx][8] = false;
                    left_nz[8] = false;
                }
            }
            mbs.push(mb);
        }
    }

    // ── 재구성 ──
    let yw = mb_w * 16;
    let yh = mb_h * 16;
    let cw = mb_w * 8;
    let ch = mb_h * 8;
    let mut yp = vec![129u8; yw * yh];
    let mut up = vec![129u8; cw * ch];
    let mut vp = vec![129u8; cw * ch];
    // 위쪽 경계 행 (127) / 왼쪽 열 (129) 은 예측 함수에서 처리한다.

    for my in 0..mb_h {
        for mx in 0..mb_w {
            let mb = &mbs[my * mb_w + mx];
            reconstruct_mb(mb, mx, my, mb_w, &mut yp, yw, &mut up, &mut vp, cw);
        }
    }

    // ── 루프 필터 ──
    if filter_level > 0 {
        loop_filter(
            &mbs, &segments, mb_w, mb_h, filter_level, sharpness, filter_simple, seg_enabled,
            seg_abs, &seg_lf, lf_deltas, &lf_delta_ref, &lf_delta_mode, &mut yp, yw, &mut up,
            &mut vp, cw,
        );
    }

    // ── YUV → RGB ──
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let yv = yp[y * yw + x] as i32;
            let uv = up[(y / 2) * cw + x / 2] as i32;
            let vv = vp[(y / 2) * cw + x / 2] as i32;
            let (r, g, b) = yuv_to_rgb(yv, uv, vv);
            rgba.extend_from_slice(&[r, g, b, 255]);
        }
    }
    Some(crate::png::Image { width: w, height: h, rgba })
}

fn yuv_to_rgb(y: i32, u: i32, v: i32) -> (u8, u8, u8) {
    // BT.601 (libwebp 의 고정소수 계수)
    let y = 1192 * (y - 16);
    let u = u - 128;
    let v = v - 128;
    let r = (y + 1634 * v) >> 10;
    let g = (y - 400 * u - 833 * v) >> 10;
    let b = (y + 2066 * u) >> 10;
    (r.clamp(0, 255) as u8, g.clamp(0, 255) as u8, b.clamp(0, 255) as u8)
}

// ── 계수 디코딩 (RFC 6386 §13) ────────────────────────────────────────────
#[allow(clippy::too_many_arguments)]
fn decode_coeffs(
    bd: &mut BoolDec,
    probs: &[u8; 1056],
    q: &Quant,
    mb: &mut MacroBlock,
    above: &mut [bool; 9],
    left: &mut [bool; 9],
) {
    let has_y2 = mb.ymode != 4;

    if has_y2 {
        // Y2 블록 (타입 1)
        let ctx = above[8] as usize + left[8] as usize;
        let n = decode_block(
            bd,
            probs,
            1,
            ctx,
            0,
            q.y2_dc,
            q.y2_ac,
            &mut mb.coeffs[24 * 16..24 * 16 + 16],
        );
        let nz_y2 = n > 0;
        above[8] = nz_y2;
        left[8] = nz_y2;
        mb.nonzero_y2 = nz_y2;
    }

    // Y 블록 16개: 타입 0(Y2 있음, DC 제외) / 3(Y2 없음)
    let (btype, first) = if has_y2 { (0usize, 1usize) } else { (3usize, 0usize) };
    for b in 0..16 {
        let (bx, by) = (b % 4, b / 4);
        let ctx = above[bx] as usize + left[by] as usize;
        let n = decode_block(
            bd,
            probs,
            btype,
            ctx,
            first,
            q.y_dc,
            q.y_ac,
            &mut mb.coeffs[b * 16..b * 16 + 16],
        );
        above[bx] = n > 0;
        left[by] = n > 0;
    }

    // U(4) + V(4): 타입 2
    for c in 0..2 {
        for b in 0..4 {
            let (bx, by) = (b % 2, b / 2);
            let ai = 4 + c * 2 + bx;
            let li = 4 + c * 2 + by;
            let ctx = above[ai] as usize + left[li] as usize;
            let idx = 16 + c * 4 + b;
            let n = decode_block(
                bd,
                probs,
                2,
                ctx,
                0,
                q.uv_dc,
                q.uv_ac,
                &mut mb.coeffs[idx * 16..idx * 16 + 16],
            );
            above[ai] = n > 0;
            left[li] = n > 0;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn decode_block(
    bd: &mut BoolDec,
    probs: &[u8; 1056],
    btype: usize,
    mut ctx: usize,
    first: usize,
    dq_dc: i32,
    dq_ac: i32,
    out: &mut [i32],
) -> usize {
    let mut nonzero = 0;
    let mut i = first;
    let mut prev_zero = false;
    while i < 16 {
        let band = COEFF_BANDS[i];
        let base = ((btype * 8 + band) * 3 + ctx) * 11;
        let p = &probs[base..base + 11];
        // 직전 계수가 0 이면 EOB 검사를 건너뛴다 (트리 시작 노드 2)
        let start = if prev_zero { 2 } else { 0 };
        let tok = bd.tree(&TOKEN_TREE, p, start);
        if tok == 11 {
            break; // dct_eob
        }
        if tok == 0 {
            // DCT_0 — 값 0. EOB 는 0 뒤에 올 수 없으므로 다음엔 트리 첫 가지를 건너뛴다.
            ctx = 0;
            prev_zero = true;
            i += 1;
            continue;
        }
        prev_zero = false;
        let mut val = TOKEN_BASE[tok as usize];
        if tok >= 5 {
            // cat1..cat6: 추가 비트를 붙여 범위 안의 값을 만든다
            let cat = pcat(tok as usize);
            let mut extra = 0i32;
            for &pp in cat.iter().take_while(|&&x| x != 0) {
                extra = (extra << 1) | bd.get(pp) as i32;
            }
            val += extra;
        }
        ctx = if val == 1 { 1 } else { 2 };
        let sign = bd.get_bit();
        let signed = if sign == 1 { -val } else { val };
        let dq = if i == 0 { dq_dc } else { dq_ac };
        out[ZIGZAG[i]] = signed * dq;
        nonzero = i + 1;
        i += 1;
    }
    nonzero
}

// ── 역변환 (RFC 6386 §14.3, §14.4) ────────────────────────────────────────
fn iwht4x4(input: &[i32], out: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let a1 = input[i] + input[12 + i];
        let b1 = input[4 + i] + input[8 + i];
        let c1 = input[4 + i] - input[8 + i];
        let d1 = input[i] - input[12 + i];
        tmp[i] = a1 + b1;
        tmp[4 + i] = c1 + d1;
        tmp[8 + i] = a1 - b1;
        tmp[12 + i] = d1 - c1;
    }
    for i in 0..4 {
        let a1 = tmp[i * 4] + tmp[i * 4 + 3];
        let b1 = tmp[i * 4 + 1] + tmp[i * 4 + 2];
        let c1 = tmp[i * 4 + 1] - tmp[i * 4 + 2];
        let d1 = tmp[i * 4] - tmp[i * 4 + 3];
        let a2 = a1 + b1;
        let b2 = c1 + d1;
        let c2 = a1 - b1;
        let d2 = d1 - c1;
        out[i * 4] = (a2 + 3) >> 3;
        out[i * 4 + 1] = (b2 + 3) >> 3;
        out[i * 4 + 2] = (c2 + 3) >> 3;
        out[i * 4 + 3] = (d2 + 3) >> 3;
    }
}

const C1: i64 = 20091; // sqrt(2)*cos(pi/8) - 1 (16.16 고정소수)
const C2: i64 = 35468; // sqrt(2)*sin(pi/8)

fn idct4x4(input: &[i32], out: &mut [i32; 16]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let a1 = input[i] + input[8 + i];
        let b1 = input[i] - input[8 + i];
        let t1 = (input[4 + i] as i64 * C2) >> 16;
        let t2 = input[12 + i] as i64 + ((input[12 + i] as i64 * C1) >> 16);
        let c1 = (t1 - t2) as i32;
        let t1 = input[4 + i] as i64 + ((input[4 + i] as i64 * C1) >> 16);
        let t2 = (input[12 + i] as i64 * C2) >> 16;
        let d1 = (t1 + t2) as i32;
        tmp[i] = a1 + d1;
        tmp[12 + i] = a1 - d1;
        tmp[4 + i] = b1 + c1;
        tmp[8 + i] = b1 - c1;
    }
    for i in 0..4 {
        let r = i * 4;
        let a1 = tmp[r] + tmp[r + 2];
        let b1 = tmp[r] - tmp[r + 2];
        let t1 = (tmp[r + 1] as i64 * C2) >> 16;
        let t2 = tmp[r + 3] as i64 + ((tmp[r + 3] as i64 * C1) >> 16);
        let c1 = (t1 - t2) as i32;
        let t1 = tmp[r + 1] as i64 + ((tmp[r + 1] as i64 * C1) >> 16);
        let t2 = (tmp[r + 3] as i64 * C2) >> 16;
        let d1 = (t1 + t2) as i32;
        out[r] = (a1 + d1 + 4) >> 3;
        out[r + 3] = (a1 - d1 + 4) >> 3;
        out[r + 1] = (b1 + c1 + 4) >> 3;
        out[r + 2] = (b1 - c1 + 4) >> 3;
    }
}

// ── 재구성: 인트라 예측 + 잔차 ────────────────────────────────────────────
#[allow(clippy::too_many_arguments)]
fn reconstruct_mb(
    mb: &MacroBlock,
    mx: usize,
    my: usize,
    _mb_w: usize,
    yp: &mut [u8],
    yw: usize,
    up: &mut [u8],
    vp: &mut [u8],
    cw: usize,
) {
    // Y2 → 각 Y 블록의 DC
    let mut coeffs = mb.coeffs;
    if mb.ymode != 4 {
        let mut dc = [0i32; 16];
        iwht4x4(&coeffs[24 * 16..24 * 16 + 16], &mut dc);
        for b in 0..16 {
            coeffs[b * 16] = dc[b];
        }
    }

    let (px, py) = (mx * 16, my * 16);
    // 휘도 예측
    if mb.ymode == 4 {
        for b in 0..16 {
            let (bx, by) = (b % 4, b / 4);
            let (x0, y0) = (px + bx * 4, py + by * 4);
            predict_4x4(yp, yw, x0, y0, mb.bmodes[b], mx, my, bx, by);
            add_residual(yp, yw, x0, y0, &coeffs[b * 16..b * 16 + 16], 4);
        }
    } else {
        predict_16x16(yp, yw, px, py, mb.ymode, mx, my);
        for b in 0..16 {
            let (bx, by) = (b % 4, b / 4);
            add_residual(yp, yw, px + bx * 4, py + by * 4, &coeffs[b * 16..b * 16 + 16], 4);
        }
    }
    // 색차 예측
    let (cx, cy) = (mx * 8, my * 8);
    predict_chroma(up, cw, cx, cy, mb.uvmode, mx, my);
    predict_chroma(vp, cw, cx, cy, mb.uvmode, mx, my);
    for b in 0..4 {
        let (bx, by) = (b % 2, b / 2);
        add_residual(up, cw, cx + bx * 4, cy + by * 4, &coeffs[(16 + b) * 16..(16 + b) * 16 + 16], 4);
        add_residual(vp, cw, cx + bx * 4, cy + by * 4, &coeffs[(20 + b) * 16..(20 + b) * 16 + 16], 4);
    }
}

fn add_residual(plane: &mut [u8], stride: usize, x0: usize, y0: usize, coeffs: &[i32], n: usize) {
    if coeffs.iter().all(|&c| c == 0) {
        return;
    }
    let mut res = [0i32; 16];
    idct4x4(coeffs, &mut res);
    for y in 0..n {
        for x in 0..n {
            let p = (y0 + y) * stride + x0 + x;
            if p < plane.len() {
                plane[p] = (plane[p] as i32 + res[y * 4 + x]).clamp(0, 255) as u8;
            }
        }
    }
}

// 위/왼쪽 화소 읽기 (경계 밖은 표준 기본값: 위 127, 왼쪽 129)
fn above_px(plane: &[u8], stride: usize, x: usize, y: usize, mb_y: usize) -> u8 {
    if mb_y == 0 {
        127
    } else {
        plane[(y - 1) * stride + x]
    }
}

fn left_px(plane: &[u8], stride: usize, x: usize, y: usize, mb_x: usize) -> u8 {
    if mb_x == 0 {
        129
    } else {
        plane[y * stride + x - 1]
    }
}

fn predict_16x16(plane: &mut [u8], stride: usize, px: usize, py: usize, mode: usize, mx: usize, my: usize) {
    predict_square(plane, stride, px, py, 16, mode, mx, my);
}

fn predict_chroma(plane: &mut [u8], stride: usize, px: usize, py: usize, mode: usize, mx: usize, my: usize) {
    predict_square(plane, stride, px, py, 8, mode, mx, my);
}

// DC/V/H/TM 예측 (16x16, 8x8 공용)
#[allow(clippy::too_many_arguments)]
fn predict_square(
    plane: &mut [u8],
    stride: usize,
    px: usize,
    py: usize,
    n: usize,
    mode: usize,
    mx: usize,
    my: usize,
) {
    let above: Vec<u8> = (0..n).map(|i| above_px(plane, stride, px + i, py, my)).collect();
    let left: Vec<u8> = (0..n).map(|i| left_px(plane, stride, px, py + i, mx)).collect();
    let corner = if mx == 0 || my == 0 {
        if my == 0 { 127 } else { 129 }
    } else {
        plane[(py - 1) * stride + px - 1]
    };
    let fill = |plane: &mut [u8], f: &dyn Fn(usize, usize) -> u8| {
        for y in 0..n {
            for x in 0..n {
                let p = (py + y) * stride + px + x;
                if p < plane.len() {
                    plane[p] = f(x, y);
                }
            }
        }
    };
    match mode {
        0 => {
            // DC: 위+왼 평균. 경계면 있는 쪽만, 둘 다 없으면 128.
            let has_above = my > 0;
            let has_left = mx > 0;
            let dc = if has_above && has_left {
                let s: u32 = above.iter().map(|&v| v as u32).sum::<u32>()
                    + left.iter().map(|&v| v as u32).sum::<u32>();
                ((s + n as u32) / (2 * n as u32)) as u8
            } else if has_above {
                let s: u32 = above.iter().map(|&v| v as u32).sum();
                ((s + (n as u32 / 2)) / n as u32) as u8
            } else if has_left {
                let s: u32 = left.iter().map(|&v| v as u32).sum();
                ((s + (n as u32 / 2)) / n as u32) as u8
            } else {
                128
            };
            fill(plane, &|_, _| dc);
        }
        1 => fill(plane, &|x, _| above[x]),  // V
        2 => fill(plane, &|_, y| left[y]),   // H
        _ => {
            // TM: left + above - corner
            let c = corner as i32;
            fill(plane, &|x, y| {
                (left[y] as i32 + above[x] as i32 - c).clamp(0, 255) as u8
            });
        }
    }
}

// 4x4 B_PRED (RFC 6386 §12.3). 위 4 + 위-오른쪽 4 + 왼 4 + 코너.
#[allow(clippy::too_many_arguments)]
fn predict_4x4(
    plane: &mut [u8],
    stride: usize,
    x0: usize,
    y0: usize,
    mode: usize,
    mx: usize,
    my: usize,
    bx: usize,
    by: usize,
) {
    // A[0..8]: 위 4화소 + 위-오른쪽 4화소. L[0..4]: 왼 4. P: 코너.
    //
    // VP8 의 함정: 위-오른쪽 4화소는 **오른쪽 끝 서브블록(bx==3)일 때 항상 매크로블록
    // 위쪽 행(py-1)에서** 가져온다 — 아래 행(by>0)이어도 그렇다. 오른쪽 MB 는 아직
    // 복원되지 않았기 때문이다. 이걸 (y0-1) 행에서 그냥 읽으면 현재 MB 안의 값을 읽어
    // 예측이 어긋난다 (그림은 나오지만 값이 전부 틀린다).
    let (px, py) = (mx * 16, my * 16);
    let mut a = [0i32; 8];
    for i in 0..4 {
        a[i] = if my == 0 && by == 0 {
            127
        } else {
            plane[(y0 - 1) * stride + x0 + i] as i32
        };
    }
    for i in 0..4 {
        a[4 + i] = if bx == 3 {
            // MB 위쪽 행에서 오른쪽 이웃 MB 의 아래 끝 4화소
            if my == 0 {
                127
            } else if px + 16 + i < stride {
                plane[(py - 1) * stride + px + 16 + i] as i32
            } else {
                // 마지막 MB 열: 위쪽 행의 마지막 화소를 복제 (libwebp 와 동일)
                plane[(py - 1) * stride + stride - 1] as i32
            }
        } else if my == 0 && by == 0 {
            127
        } else {
            let x = x0 + 4 + i;
            if x < stride {
                plane[(y0 - 1) * stride + x] as i32
            } else {
                plane[(y0 - 1) * stride + stride - 1] as i32
            }
        };
    }
    let a: Vec<i32> = a.to_vec();
    let l: Vec<i32> = (0..4)
        .map(|i| {
            if mx == 0 && bx == 0 {
                129
            } else {
                plane[(y0 + i) * stride + x0 - 1] as i32
            }
        })
        .collect();
    let p: i32 = if (mx == 0 && bx == 0) || (my == 0 && by == 0) {
        if my == 0 && by == 0 {
            127
        } else {
            129
        }
    } else {
        plane[(y0 - 1) * stride + x0 - 1] as i32
    };

    let avg3 = |x: i32, y: i32, z: i32| (x + 2 * y + z + 2) >> 2;
    let avg2 = |x: i32, y: i32| (x + y + 1) >> 1;
    let mut b = [[0i32; 4]; 4];
    match mode {
        0 => {
            // B_DC_PRED
            let s: i32 = a[0..4].iter().sum::<i32>() + l.iter().sum::<i32>();
            let dc = (s + 4) >> 3;
            for r in b.iter_mut() {
                *r = [dc; 4];
            }
        }
        1 => {
            // B_TM_PRED
            for y in 0..4 {
                for x in 0..4 {
                    b[y][x] = (l[y] + a[x] - p).clamp(0, 255);
                }
            }
        }
        2 => {
            // B_VE_PRED
            let v = [
                avg3(p, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg3(a[2], a[3], a[4]),
            ];
            for r in b.iter_mut() {
                *r = v;
            }
        }
        3 => {
            // B_HE_PRED
            let h = [
                avg3(p, l[0], l[1]),
                avg3(l[0], l[1], l[2]),
                avg3(l[1], l[2], l[3]),
                avg3(l[2], l[3], l[3]),
            ];
            for y in 0..4 {
                b[y] = [h[y]; 4];
            }
        }
        4 => {
            // B_LD_PRED
            let e = |i: usize| -> i32 {
                match i {
                    0 => avg3(a[0], a[1], a[2]),
                    1 => avg3(a[1], a[2], a[3]),
                    2 => avg3(a[2], a[3], a[4]),
                    3 => avg3(a[3], a[4], a[5]),
                    4 => avg3(a[4], a[5], a[6]),
                    5 => avg3(a[5], a[6], a[7]),
                    _ => avg3(a[6], a[7], a[7]),
                }
            };
            for y in 0..4 {
                for x in 0..4 {
                    b[y][x] = e(x + y);
                }
            }
        }
        5 => {
            // B_RD_PRED
            let e = [
                avg3(l[3], l[2], l[1]),
                avg3(l[2], l[1], l[0]),
                avg3(l[1], l[0], p),
                avg3(l[0], p, a[0]),
                avg3(p, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg3(a[1], a[2], a[3]),
            ];
            for y in 0..4 {
                for x in 0..4 {
                    b[y][x] = e[x + 3 - y];
                }
            }
        }
        6 => {
            // B_VR_PRED
            let e = [
                avg3(l[2], l[1], l[0]),
                avg3(l[1], l[0], p),
                avg3(l[0], p, a[0]),
                avg2(p, a[0]),
                avg3(p, a[0], a[1]),
                avg2(a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg2(a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg2(a[2], a[3]),
            ];
            let idx = [
                [3, 5, 7, 9],
                [2, 4, 6, 8],
                [1, 3, 5, 7],
                [0, 2, 4, 6],
            ];
            for y in 0..4 {
                for x in 0..4 {
                    b[y][x] = e[idx[y][x]];
                }
            }
        }
        7 => {
            // B_VL_PRED
            let e = [
                avg2(a[0], a[1]),
                avg3(a[0], a[1], a[2]),
                avg2(a[1], a[2]),
                avg3(a[1], a[2], a[3]),
                avg2(a[2], a[3]),
                avg3(a[2], a[3], a[4]),
                avg2(a[3], a[4]),
                avg3(a[3], a[4], a[5]),
                avg3(a[4], a[5], a[6]),
                avg3(a[5], a[6], a[7]),
            ];
            // RFC 6386 §12.3 B_VL_PRED (짝수 인덱스=avg2, 홀수=avg3)
            let idx = [
                [0, 2, 4, 6],
                [1, 3, 5, 7],
                [2, 4, 6, 8],
                [3, 5, 7, 9],
            ];
            for y in 0..4 {
                for x in 0..4 {
                    b[y][x] = e[idx[y][x]];
                }
            }
        }
        8 => {
            // B_HD_PRED
            let e = [
                avg2(l[3], l[2]),
                avg3(l[3], l[2], l[1]),
                avg2(l[2], l[1]),
                avg3(l[2], l[1], l[0]),
                avg2(l[1], l[0]),
                avg3(l[1], l[0], p),
                avg2(l[0], p),
                avg3(l[0], p, a[0]),
                avg3(p, a[0], a[1]),
                avg3(a[0], a[1], a[2]),
            ];
            let idx = [
                [6, 7, 8, 9],
                [4, 5, 6, 7],
                [2, 3, 4, 5],
                [0, 1, 2, 3],
            ];
            for y in 0..4 {
                for x in 0..4 {
                    b[y][x] = e[idx[y][x]];
                }
            }
        }
        _ => {
            // B_HU_PRED
            let e = [
                avg2(l[0], l[1]),
                avg3(l[0], l[1], l[2]),
                avg2(l[1], l[2]),
                avg3(l[1], l[2], l[3]),
                avg2(l[2], l[3]),
                avg3(l[2], l[3], l[3]),
                l[3],
                l[3],
                l[3],
                l[3],
            ];
            let idx = [
                [0, 1, 2, 3],
                [2, 3, 4, 5],
                [4, 5, 6, 7],
                [6, 7, 8, 9],
            ];
            for y in 0..4 {
                for x in 0..4 {
                    b[y][x] = e[idx[y][x]];
                }
            }
        }
    }
    for y in 0..4 {
        for x in 0..4 {
            let pos = (y0 + y) * stride + x0 + x;
            if pos < plane.len() {
                plane[pos] = b[y][x].clamp(0, 255) as u8;
            }
        }
    }
}

// ── 루프 필터 (RFC 6386 §15) ──────────────────────────────────────────────
#[allow(clippy::too_many_arguments)]
fn loop_filter(
    mbs: &[MacroBlock],
    segments: &[usize],
    mb_w: usize,
    mb_h: usize,
    base_level: i32,
    sharpness: i32,
    simple: bool,
    seg_enabled: bool,
    seg_abs: bool,
    seg_lf: &[i32; 4],
    lf_deltas: bool,
    lf_delta_ref: &[i32; 4],
    _lf_delta_mode: &[i32; 4],
    yp: &mut [u8],
    yw: usize,
    up: &mut [u8],
    vp: &mut [u8],
    cw: usize,
) {
    for my in 0..mb_h {
        for mx in 0..mb_w {
            let mb = &mbs[my * mb_w + mx];
            let seg = segments[my * mb_w + mx];
            let mut level = if !seg_enabled {
                base_level
            } else if seg_abs {
                seg_lf[seg]
            } else {
                base_level + seg_lf[seg]
            };
            if lf_deltas {
                level += lf_delta_ref[0]; // 인트라 프레임
            }
            let level = level.clamp(0, 63);
            if level == 0 {
                continue;
            }
            // 필터 세기 파라미터
            let mut interior = level;
            if sharpness > 0 {
                interior >>= if sharpness > 4 { 2 } else { 1 };
                interior = interior.min(9 - sharpness);
            }
            let interior = interior.max(1);
            let hev_thresh = if level >= 40 {
                2
            } else if level >= 15 {
                1
            } else {
                0
            };
            let mb_edge_limit = ((level + 2) * 2 + interior) as i32;
            let sub_edge_limit = (level * 2 + interior) as i32;
            // 스킵 + 계수 없음 + 16x16 모드면 내부 엣지는 필터하지 않는다
            let filter_inner = !(mb.skip && mb.ymode != 4);

            // 세로 엣지 (왼쪽 경계 + 내부 4,8,12)
            if mx > 0 {
                filter_edge_v(yp, yw, mx * 16, my * 16, 16, mb_edge_limit, interior, hev_thresh, simple, true);
                if !simple {
                    filter_edge_v(up, cw, mx * 8, my * 8, 8, mb_edge_limit, interior, hev_thresh, false, true);
                    filter_edge_v(vp, cw, mx * 8, my * 8, 8, mb_edge_limit, interior, hev_thresh, false, true);
                }
            }
            if filter_inner {
                for i in 1..4 {
                    filter_edge_v(yp, yw, mx * 16 + i * 4, my * 16, 16, sub_edge_limit, interior, hev_thresh, simple, false);
                }
                if !simple {
                    filter_edge_v(up, cw, mx * 8 + 4, my * 8, 8, sub_edge_limit, interior, hev_thresh, false, false);
                    filter_edge_v(vp, cw, mx * 8 + 4, my * 8, 8, sub_edge_limit, interior, hev_thresh, false, false);
                }
            }
            // 가로 엣지
            if my > 0 {
                filter_edge_h(yp, yw, mx * 16, my * 16, 16, mb_edge_limit, interior, hev_thresh, simple, true);
                if !simple {
                    filter_edge_h(up, cw, mx * 8, my * 8, 8, mb_edge_limit, interior, hev_thresh, false, true);
                    filter_edge_h(vp, cw, mx * 8, my * 8, 8, mb_edge_limit, interior, hev_thresh, false, true);
                }
            }
            if filter_inner {
                for i in 1..4 {
                    filter_edge_h(yp, yw, mx * 16, my * 16 + i * 4, 16, sub_edge_limit, interior, hev_thresh, simple, false);
                }
                if !simple {
                    filter_edge_h(up, cw, mx * 8, my * 8 + 4, 8, sub_edge_limit, interior, hev_thresh, false, false);
                    filter_edge_h(vp, cw, mx * 8, my * 8 + 4, 8, sub_edge_limit, interior, hev_thresh, false, false);
                }
            }
        }
    }
}

fn c(v: i32) -> i32 {
    v.clamp(-128, 127)
}

fn u2s(v: u8) -> i32 {
    v as i32 - 128
}

fn s2u(v: i32) -> u8 {
    (c(v) + 128) as u8
}

// 한 화소열(수직 엣지 기준)에 대한 필터. px[-4..4] 를 인덱스로 받는다.
#[allow(clippy::too_many_arguments)]
fn filter_pixels(
    plane: &mut [u8],
    idx: [usize; 8], // p3 p2 p1 p0 q0 q1 q2 q3
    edge_limit: i32,
    interior: i32,
    hev_thresh: i32,
    simple: bool,
    mb_edge: bool,
) {
    let g = |plane: &[u8], i: usize| u2s(plane[i]);
    let (p3, p2, p1, p0) = (g(plane, idx[0]), g(plane, idx[1]), g(plane, idx[2]), g(plane, idx[3]));
    let (q0, q1, q2, q3) = (g(plane, idx[4]), g(plane, idx[5]), g(plane, idx[6]), g(plane, idx[7]));

    if simple {
        // 단순 필터: |p0-q0|*2 + |p1-q1|/2 <= limit
        let mask = (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= edge_limit;
        if !mask {
            return;
        }
        let a = c(c(p1 - q1) + 3 * (q0 - p0));
        let f1 = c(a + 4) >> 3;
        let f2 = c(a + 3) >> 3;
        plane[idx[4]] = s2u(q0 - f1);
        plane[idx[3]] = s2u(p0 + f2);
        return;
    }

    // 필터 마스크 (RFC 6386 §15.2)
    let mask = (p3 - p2).abs() <= interior
        && (p2 - p1).abs() <= interior
        && (p1 - p0).abs() <= interior
        && (q3 - q2).abs() <= interior
        && (q2 - q1).abs() <= interior
        && (q1 - q0).abs() <= interior
        && (p0 - q0).abs() * 2 + (p1 - q1).abs() / 2 <= edge_limit;
    if !mask {
        return;
    }
    let hev = (p1 - p0).abs() > hev_thresh || (q1 - q0).abs() > hev_thresh;

    if !mb_edge {
        // 서브블록 필터
        let a = c(c(p1 - q1) + 3 * (q0 - p0));
        if hev {
            let f1 = c(a + 4) >> 3;
            let f2 = c(a + 3) >> 3;
            plane[idx[4]] = s2u(q0 - f1);
            plane[idx[3]] = s2u(p0 + f2);
        } else {
            let a = c(3 * (q0 - p0)); // hev 없으면 p1-q1 항 제외
            let f1 = c(a + 4) >> 3;
            let f2 = c(a + 3) >> 3;
            plane[idx[4]] = s2u(q0 - f1);
            plane[idx[3]] = s2u(p0 + f2);
            let f3 = (f1 + 1) >> 1;
            plane[idx[5]] = s2u(q1 - f3);
            plane[idx[2]] = s2u(p1 + f3);
        }
        return;
    }

    // 매크로블록 엣지 필터
    if hev {
        let a = c(c(p1 - q1) + 3 * (q0 - p0));
        let f1 = c(a + 4) >> 3;
        let f2 = c(a + 3) >> 3;
        plane[idx[4]] = s2u(q0 - f1);
        plane[idx[3]] = s2u(p0 + f2);
    } else {
        let w = c(c(p1 - q1) + 3 * (q0 - p0));
        let a = (27 * w + 63) >> 7;
        plane[idx[4]] = s2u(q0 - a);
        plane[idx[3]] = s2u(p0 + a);
        let a = (18 * w + 63) >> 7;
        plane[idx[5]] = s2u(q1 - a);
        plane[idx[2]] = s2u(p1 + a);
        let a = (9 * w + 63) >> 7;
        plane[idx[6]] = s2u(q2 - a);
        plane[idx[1]] = s2u(p2 + a);
    }
}

#[allow(clippy::too_many_arguments)]
fn filter_edge_v(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    n: usize,
    edge_limit: i32,
    interior: i32,
    hev: i32,
    simple: bool,
    mb_edge: bool,
) {
    if x < 4 {
        return;
    }
    for i in 0..n {
        let row = (y + i) * stride;
        if row + x + 3 >= plane.len() {
            break;
        }
        let idx = [
            row + x - 4,
            row + x - 3,
            row + x - 2,
            row + x - 1,
            row + x,
            row + x + 1,
            row + x + 2,
            row + x + 3,
        ];
        filter_pixels(plane, idx, edge_limit, interior, hev, simple, mb_edge);
    }
}

#[allow(clippy::too_many_arguments)]
fn filter_edge_h(
    plane: &mut [u8],
    stride: usize,
    x: usize,
    y: usize,
    n: usize,
    edge_limit: i32,
    interior: i32,
    hev: i32,
    simple: bool,
    mb_edge: bool,
) {
    if y < 4 {
        return;
    }
    for i in 0..n {
        let col = x + i;
        if (y + 3) * stride + col >= plane.len() {
            break;
        }
        let idx = [
            (y - 4) * stride + col,
            (y - 3) * stride + col,
            (y - 2) * stride + col,
            (y - 1) * stride + col,
            y * stride + col,
            (y + 1) * stride + col,
            (y + 2) * stride + col,
            (y + 3) * stride + col,
        ];
        filter_pixels(plane, idx, edge_limit, interior, hev, simple, mb_edge);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_real_vp8_photo() {
        // 실제 사이트(react.dev)의 VP8 lossy WebP. 기준값은 macOS 의 참조 디코더(sips)가
        // 만든 PNG 에서 뽑았다 — 우리 디코더가 맞는지 **밖에서** 검증한 것이다.
        // 표본 픽셀이 오차 8 이내면 비트스트림 해석·역변환·인트라 예측이 다 맞다는 뜻이다.
        // (남는 미세 오차는 크로마 업샘플링/루프필터 세부 차이다.)
        let bytes = std::fs::read("assets/test/photo.webp").unwrap();
        let img = decode(&bytes).expect("VP8 디코드");
        assert_eq!((img.width, img.height), (800, 735));
        let samples: [((usize, usize), (u8, u8, u8)); 6] = [
            ((10, 10), (35, 29, 24)),
            ((400, 100), (46, 33, 26)),
            ((200, 300), (192, 172, 152)),
            ((600, 500), (86, 43, 42)),
            ((750, 700), (36, 30, 27)),
            ((50, 650), (39, 30, 29)),
        ];
        for ((x, y), (r, g, b)) in samples {
            let o = (y * img.width + x) * 4;
            let (dr, dg, db) = (img.rgba[o], img.rgba[o + 1], img.rgba[o + 2]);
            let err = (dr as i32 - r as i32).abs()
                + (dg as i32 - g as i32).abs()
                + (db as i32 - b as i32).abs();
            assert!(
                err <= 24,
                "({}, {}): 우리 ({}, {}, {}) vs 참조 ({}, {}, {})",
                x, y, dr, dg, db, r, g, b
            );
        }
    }

    #[test]
    fn rejects_non_webp() {
        assert!(decode(b"not a webp file at all").is_none());
        assert!(decode(&[0x89, b'P', b'N', b'G', 0, 0, 0, 0, 0, 0, 0, 0]).is_none());
    }
}
