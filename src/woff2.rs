// WOFF2 → sfnt(TTF) 복원 (W3C WOFF2 명세).
//
// 모던 사이트의 웹폰트는 사실상 전부 woff2 다. 이게 없으면 브라우저라고 말하는 순간
// 서버가 woff2 를 내려주고 우리는 글자를 하나도 못 그린다.
//
// 두 단계:
//   1. brotli 로 압축 해제 (brotli.rs)
//   2. 테이블 복원. glyf/loca 는 woff2 전용 변환이 걸려 있어 되돌려야 한다
//      (윤곽선 좌표가 삼중항(triplet)으로 재인코딩돼 있다).

use crate::brotli;

const SIGNATURE: u32 = 0x774F_4632; // 'wOF2'

// 알려진 테이블 태그 63개 (인덱스 = 플래그 하위 6비트)
const KNOWN_TAGS: [&[u8; 4]; 63] = [
    b"cmap", b"head", b"hhea", b"hmtx", b"maxp", b"name", b"OS/2", b"post", b"cvt ", b"fpgm",
    b"glyf", b"loca", b"prep", b"CFF ", b"VORG", b"EBDT", b"EBLC", b"gasp", b"hdmx", b"kern",
    b"LTSH", b"PCLT", b"VDMX", b"vhea", b"vmtx", b"BASE", b"GDEF", b"GPOS", b"GSUB", b"EBSC",
    b"JSTF", b"MATH", b"CBDT", b"CBLC", b"COLR", b"CPAL", b"SVG ", b"sbix", b"acnt", b"avar",
    b"bdat", b"bloc", b"bsln", b"cvar", b"fdsc", b"feat", b"fmtx", b"fvar", b"gvar", b"hsty",
    b"just", b"lcar", b"mort", b"morx", b"opbd", b"prop", b"trak", b"Zapf", b"Silf", b"Glat",
    b"Gloc", b"Feat", b"Sill",
];

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8]) -> Self {
        Reader { d, p: 0 }
    }
    fn u8(&mut self) -> Option<u8> {
        let v = *self.d.get(self.p)?;
        self.p += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let v = u16::from_be_bytes(self.d.get(self.p..self.p + 2)?.try_into().ok()?);
        self.p += 2;
        Some(v)
    }
    fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }
    fn u32(&mut self) -> Option<u32> {
        let v = u32::from_be_bytes(self.d.get(self.p..self.p + 4)?.try_into().ok()?);
        self.p += 4;
        Some(v)
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let v = self.d.get(self.p..self.p + n)?;
        self.p += n;
        Some(v)
    }
    // UIntBase128 (7비트 × 최대 5바이트, 최상위 비트가 연속 표시)
    fn base128(&mut self) -> Option<u32> {
        let mut v: u32 = 0;
        for i in 0..5 {
            let b = self.u8()?;
            if i == 0 && b == 0x80 {
                return None; // 선행 0 금지
            }
            if v > (u32::MAX >> 7) {
                return None; // 오버플로
            }
            v = (v << 7) | (b & 0x7f) as u32;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    // 255UInt16 (woff2 §6.1.1)
    fn u255(&mut self) -> Option<u16> {
        let code = self.u8()?;
        match code {
            253 => self.u16(),
            254 => Some(self.u8()? as u16 + 253 * 2),
            255 => Some(self.u8()? as u16 + 253),
            c => Some(c as u16),
        }
    }
}

struct TableEntry {
    tag: [u8; 4],
    transform_version: u8,
    transform_length: u32, // 변환이 없으면 orig_length 와 같다
}

/// woff2 바이트 → sfnt(TTF/OTF) 바이트. 형식이 아니거나 못 풀면 None.
pub fn decode(data: &[u8]) -> Option<Vec<u8>> {
    let mut r = Reader::new(data);
    if r.u32()? != SIGNATURE {
        return None;
    }
    let flavor = r.u32()?;
    let _length = r.u32()?;
    let num_tables = r.u16()? as usize;
    let _reserved = r.u16()?;
    let _total_sfnt_size = r.u32()?;
    let total_compressed = r.u32()? as usize;
    let _major = r.u16()?;
    let _minor = r.u16()?;
    let _meta_off = r.u32()?;
    let _meta_len = r.u32()?;
    let _meta_orig = r.u32()?;
    let _priv_off = r.u32()?;
    let _priv_len = r.u32()?;

    // 테이블 디렉터리
    let mut entries = Vec::with_capacity(num_tables);
    for _ in 0..num_tables {
        let flags = r.u8()?;
        let idx = (flags & 0x3f) as usize;
        let tv = flags >> 6;
        let tag: [u8; 4] = if idx == 63 {
            r.take(4)?.try_into().ok()?
        } else {
            **KNOWN_TAGS.get(idx)?
        };
        let orig_length = r.base128()?;
        let _ = orig_length; // 복원 후 길이는 우리가 다시 계산한다
        // 변환 여부: glyf/loca 는 null 변환이 3, 나머지는 0
        let is_glyf_loca = &tag == b"glyf" || &tag == b"loca";
        let transformed = if is_glyf_loca { tv != 3 } else { tv != 0 };
        let transform_length = if transformed { r.base128()? } else { orig_length };
        entries.push(TableEntry { tag, transform_version: tv, transform_length });
    }

    // ttcf(폰트 컬렉션)는 미지원 — 조용히 틀리게 그리느니 실패로 알린다
    if flavor == 0x7474_6366 {
        return None;
    }

    // 압축 데이터 → 모든 테이블 데이터의 연결
    let comp = r.d.get(r.p..r.p + total_compressed)?;
    let raw = brotli::decompress(comp)?;

    // 각 테이블의 (변환된) 원본 바이트 잘라내기
    let mut slices: Vec<&[u8]> = Vec::with_capacity(num_tables);
    let mut off = 0usize;
    for e in &entries {
        let n = e.transform_length as usize;
        slices.push(raw.get(off..off + n)?);
        off += n;
    }

    // glyf/loca 변환 되돌리기
    let mut tables: Vec<([u8; 4], Vec<u8>)> = Vec::with_capacity(num_tables);
    let mut glyf_out: Option<Vec<u8>> = None;
    let mut loca_out: Option<Vec<u8>> = None;
    for (i, e) in entries.iter().enumerate() {
        let is_glyf_loca = &e.tag == b"glyf" || &e.tag == b"loca";
        if is_glyf_loca && e.transform_version != 3 {
            if &e.tag == b"glyf" {
                let (g, l) = reconstruct_glyf(slices[i])?;
                glyf_out = Some(g);
                loca_out = Some(l);
            }
            // 변환된 loca 는 데이터가 비어 있다 (glyf 복원이 함께 만든다)
            continue;
        }
        if is_glyf_loca {
            // null 변환 — 그대로
            if &e.tag == b"glyf" {
                glyf_out = Some(slices[i].to_vec());
            } else {
                loca_out = Some(slices[i].to_vec());
            }
            continue;
        }
        if e.transform_version != 0 {
            // hmtx 변환 등 — 아직 구현하지 않았다. 조용히 깨진 폰트를 내놓지 않는다.
            return None;
        }
        tables.push((e.tag, slices[i].to_vec()));
    }
    if let (Some(g), Some(l)) = (glyf_out, loca_out) {
        tables.push((*b"glyf", g));
        tables.push((*b"loca", l));
    }
    Some(build_sfnt(flavor, tables))
}

// 표준 sfnt 조립 (태그 오름차순 + 4바이트 정렬)
fn build_sfnt(flavor: u32, mut tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    tables.sort_by(|a, b| a.0.cmp(&b.0));
    let n = tables.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&flavor.to_be_bytes());
    out.extend_from_slice(&n.to_be_bytes());
    // searchRange / entrySelector / rangeShift
    let mut es = 0u16;
    while (1u32 << (es + 1)) <= n as u32 {
        es += 1;
    }
    let sr = (1u16 << es) * 16;
    out.extend_from_slice(&sr.to_be_bytes());
    out.extend_from_slice(&es.to_be_bytes());
    out.extend_from_slice(&(n * 16 - sr).to_be_bytes());

    let mut offset = 12 + 16 * tables.len() as u32;
    let mut records = Vec::new();
    for (tag, data) in &tables {
        records.push((*tag, checksum(data), offset, data.len() as u32));
        offset += ((data.len() as u32) + 3) & !3;
    }
    for (tag, sum, off, len) in &records {
        out.extend_from_slice(tag);
        out.extend_from_slice(&sum.to_be_bytes());
        out.extend_from_slice(&off.to_be_bytes());
        out.extend_from_slice(&len.to_be_bytes());
    }
    for (_, data) in &tables {
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    out
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i < data.len() {
        let mut w = [0u8; 4];
        for k in 0..4 {
            if i + k < data.len() {
                w[k] = data[i + k];
            }
        }
        sum = sum.wrapping_add(u32::from_be_bytes(w));
        i += 4;
    }
    sum
}

struct Point {
    x: i32,
    y: i32,
    on_curve: bool,
}

// 변환된 glyf → (glyf, loca)
fn reconstruct_glyf(data: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut h = Reader::new(data);
    let _reserved = h.u16()?;
    let _option_flags = h.u16()?;
    let num_glyphs = h.u16()? as usize;
    let index_format = h.u16()?;
    let n_contour_size = h.u32()? as usize;
    let n_points_size = h.u32()? as usize;
    let flag_size = h.u32()? as usize;
    let glyph_size = h.u32()? as usize;
    let composite_size = h.u32()? as usize;
    let bbox_size = h.u32()? as usize;
    let instruction_size = h.u32()? as usize;

    let base = h.p;
    let mut cur = base;
    let mut sub = |n: usize| -> Option<&[u8]> {
        let s = data.get(cur..cur + n)?;
        cur += n;
        Some(s)
    };
    let n_contour = sub(n_contour_size)?;
    let n_points = sub(n_points_size)?;
    let flags = sub(flag_size)?;
    let glyphs = sub(glyph_size)?;
    let composites = sub(composite_size)?;
    let bboxes = sub(bbox_size)?;
    let instructions = sub(instruction_size)?;

    let mut r_contour = Reader::new(n_contour);
    let mut r_points = Reader::new(n_points);
    let mut r_flags = Reader::new(flags);
    let mut r_glyph = Reader::new(glyphs);
    let mut r_comp = Reader::new(composites);
    let mut r_instr = Reader::new(instructions);

    // bbox 비트맵 + 값
    let bitmap_len = (num_glyphs + 7) / 8;
    let bbox_bitmap = bboxes.get(..bitmap_len)?;
    let mut r_bbox = Reader::new(bboxes.get(bitmap_len..)?);

    let mut glyf: Vec<u8> = Vec::new();
    let mut loca: Vec<u32> = Vec::with_capacity(num_glyphs + 1);
    loca.push(0);

    for gid in 0..num_glyphs {
        let n_cont = r_contour.i16()?;
        let has_bbox = (bbox_bitmap[gid / 8] >> (7 - (gid % 8))) & 1 == 1;
        let start = glyf.len();

        if n_cont == 0 {
            // 빈 글리프 — 데이터 없음
            if has_bbox {
                return None; // 빈 글리프에 명시 bbox 는 금지
            }
        } else if n_cont < 0 {
            // 합성 글리프: 데이터는 이미 sfnt 형식 → 그대로 복사
            if !has_bbox {
                return None; // 합성 글리프는 bbox 가 반드시 있어야 한다
            }
            let (x0, y0, x1, y1) = (r_bbox.i16()?, r_bbox.i16()?, r_bbox.i16()?, r_bbox.i16()?);
            let comp_start = r_comp.p;
            let mut have_instr = false;
            loop {
                let flags = r_comp.u16()?;
                let _glyph_index = r_comp.u16()?;
                have_instr |= flags & 0x0100 != 0; // WE_HAVE_INSTRUCTIONS
                let arg_words = flags & 0x0001 != 0; // ARG_1_AND_2_ARE_WORDS
                r_comp.take(if arg_words { 4 } else { 2 })?;
                if flags & 0x0008 != 0 {
                    r_comp.take(2)?; // WE_HAVE_A_SCALE
                } else if flags & 0x0040 != 0 {
                    r_comp.take(4)?; // X_AND_Y_SCALE
                } else if flags & 0x0080 != 0 {
                    r_comp.take(8)?; // TWO_BY_TWO
                }
                if flags & 0x0020 == 0 {
                    break; // MORE_COMPONENTS 없음
                }
            }
            let comp_data = composites.get(comp_start..r_comp.p)?;

            glyf.extend_from_slice(&(-1i16).to_be_bytes());
            for v in [x0, y0, x1, y1] {
                glyf.extend_from_slice(&v.to_be_bytes());
            }
            glyf.extend_from_slice(comp_data);
            if have_instr {
                let ilen = r_glyph.u255()? as usize;
                glyf.extend_from_slice(&(ilen as u16).to_be_bytes());
                glyf.extend_from_slice(r_instr.take(ilen)?);
            }
        } else {
            // 단순 글리프
            let ncont = n_cont as usize;
            let mut end_pts: Vec<u16> = Vec::with_capacity(ncont);
            let mut total = 0usize;
            for _ in 0..ncont {
                let np = r_points.u255()? as usize;
                total += np;
                if total == 0 || total > 65535 {
                    return None;
                }
                end_pts.push((total - 1) as u16);
            }
            // 좌표: 삼중항 인코딩 (woff2 §5.2)
            let mut pts: Vec<Point> = Vec::with_capacity(total);
            let (mut x, mut y) = (0i32, 0i32);
            for _ in 0..total {
                let f = r_flags.u8()?;
                let on_curve = f >> 7 == 0;
                let flag = (f & 0x7f) as i32;
                let (dx, dy) = if flag < 10 {
                    let b0 = r_glyph.u8()? as i32;
                    (0, with_sign(flag, ((flag & 14) << 7) + b0))
                } else if flag < 20 {
                    let b0 = r_glyph.u8()? as i32;
                    (with_sign(flag, (((flag - 10) & 14) << 7) + b0), 0)
                } else if flag < 84 {
                    let b0 = flag - 20;
                    let b1 = r_glyph.u8()? as i32;
                    (
                        with_sign(flag, 1 + (b0 & 0x30) + (b1 >> 4)),
                        with_sign(flag >> 1, 1 + ((b0 & 0x0c) << 2) + (b1 & 0x0f)),
                    )
                } else if flag < 120 {
                    let b0 = flag - 84;
                    let b1 = r_glyph.u8()? as i32;
                    let b2 = r_glyph.u8()? as i32;
                    (
                        with_sign(flag, 1 + ((b0 / 12) << 8) + b1),
                        with_sign(flag >> 1, 1 + (((b0 % 12) >> 2) << 8) + b2),
                    )
                } else if flag < 124 {
                    let b1 = r_glyph.u8()? as i32;
                    let b2 = r_glyph.u8()? as i32;
                    let b3 = r_glyph.u8()? as i32;
                    (with_sign(flag, (b1 << 4) + (b2 >> 4)), with_sign(flag >> 1, ((b2 & 0x0f) << 8) + b3))
                } else {
                    let b1 = r_glyph.u8()? as i32;
                    let b2 = r_glyph.u8()? as i32;
                    let b3 = r_glyph.u8()? as i32;
                    let b4 = r_glyph.u8()? as i32;
                    (with_sign(flag, (b1 << 8) + b2), with_sign(flag >> 1, (b3 << 8) + b4))
                };
                x += dx;
                y += dy;
                pts.push(Point { x, y, on_curve });
            }
            let ilen = r_glyph.u255()? as usize;
            let instr = r_instr.take(ilen)?;

            // bbox: 명시값이 있으면 그걸, 없으면 좌표에서 계산
            let (x0, y0, x1, y1) = if has_bbox {
                (r_bbox.i16()?, r_bbox.i16()?, r_bbox.i16()?, r_bbox.i16()?)
            } else {
                let xs: Vec<i32> = pts.iter().map(|p| p.x).collect();
                let ys: Vec<i32> = pts.iter().map(|p| p.y).collect();
                (
                    *xs.iter().min()? as i16,
                    *ys.iter().min()? as i16,
                    *xs.iter().max()? as i16,
                    *ys.iter().max()? as i16,
                )
            };

            glyf.extend_from_slice(&(ncont as i16).to_be_bytes());
            for v in [x0, y0, x1, y1] {
                glyf.extend_from_slice(&v.to_be_bytes());
            }
            for e in &end_pts {
                glyf.extend_from_slice(&e.to_be_bytes());
            }
            glyf.extend_from_slice(&(ilen as u16).to_be_bytes());
            glyf.extend_from_slice(instr);

            // 플래그/좌표를 표준 형식으로 (반복 압축 없이 — 정확성 우선)
            let mut xs: Vec<i32> = Vec::with_capacity(total);
            let mut ys: Vec<i32> = Vec::with_capacity(total);
            let (mut px, mut py) = (0i32, 0i32);
            for p in &pts {
                xs.push(p.x - px);
                ys.push(p.y - py);
                px = p.x;
                py = p.y;
            }
            let mut flag_bytes = Vec::with_capacity(total);
            for (i, p) in pts.iter().enumerate() {
                let mut f = 0u8;
                if p.on_curve {
                    f |= 0x01;
                }
                let dx = xs[i];
                let dy = ys[i];
                if dx == 0 {
                    f |= 0x10; // X_SAME (0 delta)
                } else if (-255..=255).contains(&dx) {
                    f |= 0x02; // X_SHORT
                    if dx > 0 {
                        f |= 0x10; // 양수 표시
                    }
                }
                if dy == 0 {
                    f |= 0x20;
                } else if (-255..=255).contains(&dy) {
                    f |= 0x04;
                    if dy > 0 {
                        f |= 0x20;
                    }
                }
                flag_bytes.push(f);
            }
            glyf.extend_from_slice(&flag_bytes);
            for (i, f) in flag_bytes.iter().enumerate() {
                let dx = xs[i];
                if f & 0x02 != 0 {
                    glyf.push(dx.unsigned_abs() as u8);
                } else if f & 0x10 == 0 {
                    glyf.extend_from_slice(&(dx as i16).to_be_bytes());
                }
            }
            for (i, f) in flag_bytes.iter().enumerate() {
                let dy = ys[i];
                if f & 0x04 != 0 {
                    glyf.push(dy.unsigned_abs() as u8);
                } else if f & 0x20 == 0 {
                    glyf.extend_from_slice(&(dy as i16).to_be_bytes());
                }
            }
        }

        // 글리프는 4바이트 경계로 정렬
        while (glyf.len() - start) % 4 != 0 {
            glyf.push(0);
        }
        loca.push(glyf.len() as u32);
    }

    // loca 직렬화
    let mut loca_bytes = Vec::new();
    if index_format == 0 {
        for v in &loca {
            if v % 2 != 0 || v / 2 > 0xffff {
                return None;
            }
            loca_bytes.extend_from_slice(&((v / 2) as u16).to_be_bytes());
        }
    } else {
        for v in &loca {
            loca_bytes.extend_from_slice(&v.to_be_bytes());
        }
    }
    Some((glyf, loca_bytes))
}

fn with_sign(flag: i32, base: i32) -> i32 {
    if flag & 1 != 0 {
        base
    } else {
        -base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_woff2() {
        assert!(decode(b"\x00\x01\x00\x00abcd").is_none());
        assert!(decode(&[]).is_none());
    }

    #[test]
    fn base128_reads_multibyte() {
        let mut r = Reader::new(&[0x81, 0x00]);
        assert_eq!(r.base128(), Some(128));
        let mut r = Reader::new(&[0x3f]);
        assert_eq!(r.base128(), Some(63));
        // 선행 0x80 은 금지 (선행 0)
        let mut r = Reader::new(&[0x80, 0x01]);
        assert_eq!(r.base128(), None);
    }

    #[test]
    fn u255_variants() {
        assert_eq!(Reader::new(&[100]).u255(), Some(100));
        assert_eq!(Reader::new(&[253, 0x12, 0x34]).u255(), Some(0x1234));
        assert_eq!(Reader::new(&[255, 10]).u255(), Some(263)); // 10 + 253
        assert_eq!(Reader::new(&[254, 10]).u255(), Some(516)); // 10 + 506
    }
}

#[cfg(test)]
mod real_font_tests {
    use super::*;

    // 실제 구글폰트 woff2 (Roboto 400, latin subset) — assets/test.woff2
    #[test]
    fn decodes_material_icons_woff2() {
        // 합성 글리프/큰 글리프 수를 가진 실제 아이콘 폰트 (go.dev 가 쓴다)
        let Ok(data) = std::fs::read("/tmp/mi.woff2") else { return };
        let sfnt = decode(&data).expect("Material Icons woff2 복원");
        let font = crate::font::Font::from_bytes(sfnt).expect("폰트 파싱");
        assert!(font.glyph_index('\u{e5c8}') > 0 || font.glyph_index('a') > 0);
    }

    #[test]
    fn decodes_real_google_fonts_woff2() {
        let Ok(data) = std::fs::read("assets/test.woff2") else {
            return; // 자산이 없으면 건너뜀 (CI 등)
        };
        let sfnt = decode(&data).expect("woff2 → sfnt 복원");
        assert_eq!(&sfnt[0..4], &[0x00, 0x01, 0x00, 0x00], "TrueType 시그니처");
        // 우리 폰트 파서가 실제로 읽을 수 있어야 한다 (이게 최종 검증)
        let font = crate::font::Font::from_bytes(sfnt).expect("복원된 sfnt 를 폰트로 파싱");
        // 글리프가 실제로 나와야 한다 ('A' 의 윤곽선)
        let gid = font.glyph_index('A');
        assert!(gid > 0, "'A' 글리프가 있어야 한다");
    }
}
