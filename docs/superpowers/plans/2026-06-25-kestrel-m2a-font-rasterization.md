# Kestrel M2a — 폰트 파싱 + 글리프 래스터화 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 번들된 TrueType 폰트를 직접 파싱해 임의의 ASCII 글자를 안티앨리어싱된 그레이스케일 커버리지 비트맵으로 래스터화한다.

**Architecture:** 새 모듈 `font.rs`(TrueType 테이블 파싱 → 글리프 외곽선 + 메트릭)와 `raster.rs`(외곽선 → 커버리지 스캔라인 래스터화 + 글리프 캐시). M1 모듈(layout/paint)은 건드리지 않는다(통합은 M2b).

**Tech Stack:** Rust(edition 2021). 외부 폰트 라이브러리 없음(전부 직접). 검증용으로 기존 PPM 덤프 방식 재사용.

## Global Constraints

- 프로젝트 위치: `~/Documents/Projects/kestrel/`. 다른 저장소 건드리지 않는다.
- 외부 폰트/파싱 크레이트 금지(`ttf-parser`, `rusttype`, `fontdue` 등). 전부 직접 구현.
- TrueType `glyf` **단순 글리프만**. 합성 글리프(numberOfContours<0)는 빈 외곽선으로 처리(패닉 금지).
- cmap은 **format 4**(BMP). 라틴/ASCII 대상.
- 래스터화는 **커버리지 스캔라인**(분석적 수평 + 수직 오버샘플 N=5). 슈퍼샘플링 아님.
- M1의 `layout.rs`/`paint.rs`/`style.rs`는 수정하지 않는다.
- 번들 폰트는 `assets/fonts/Kestrel.ttf` 경로로 고정(테스트가 이 경로를 읽음). 오픈 라이선스(OFL 등) + `glyf` 테이블 보유.
- 모듈 계약 타입은 스펙 3.1을 그대로 따른다.
- 커밋 메시지 끝에: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: 폰트 에셋 + `font.rs` 코어 (리더 + 테이블 디렉터리 + 전역 메트릭)

**Files:**
- Create: `assets/fonts/Kestrel.ttf` (+ 라이선스 파일)
- Create: `src/font.rs`
- Modify: `src/main.rs` (`mod font;` 추가)

**Interfaces:**
- Consumes: 없음
- Produces:
  - `pub enum FontError { TooShort, MissingTable(&'static str), UnsupportedCmap }`
  - `pub struct Font { ... }`
  - `impl Font { pub fn from_bytes(data: Vec<u8>) -> Result<Font, FontError>; pub fn units_per_em(&self)->u16; pub fn ascent(&self)->i16; pub fn descent(&self)->i16; pub fn line_gap(&self)->i16; }`
  - 내부 헬퍼 `be_u16/be_i16/be_u32`

- [ ] **Step 1: 오픈 라이선스 glyf TTF를 `assets/fonts/Kestrel.ttf`로 확보**

OFL/permissive 라이선스이면서 `glyf` 테이블을 가진 TrueType 폰트를 받아 `assets/fonts/Kestrel.ttf`로 저장한다. (DejaVu Sans 또는 Roboto 계열. 가변폰트/CFF(.otf)는 안 됨.) 받은 뒤 `glyf` 테이블 존재를 확인:

```bash
cd ~/Documents/Projects/kestrel
mkdir -p assets/fonts
# (구현자가 OFL glyf TTF를 assets/fonts/Kestrel.ttf 로 저장)
# glyf 테이블 존재 확인 (바이너리에서 태그 검색):
grep -c glyf assets/fonts/Kestrel.ttf || echo "WARNING: glyf 태그 미발견 — 다른 폰트 필요"
```
Expected: `grep -c`가 1 이상. 0이면 CFF 폰트이므로 다른 TTF로 교체.

- [ ] **Step 2: 실패 테스트 먼저 작성**

`src/font.rs`에 테스트 모듈만 먼저:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn load() -> Font {
        let bytes = std::fs::read("assets/fonts/Kestrel.ttf").expect("read font");
        Font::from_bytes(bytes).expect("parse font")
    }

    #[test]
    fn parses_global_metrics() {
        let f = load();
        assert!((16..=16384).contains(&f.units_per_em()), "upm={}", f.units_per_em());
        assert!(f.ascent() > 0);
        assert!(f.descent() < 0);
    }
}
```

- [ ] **Step 3: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: 컴파일 실패 — `Font` 미정의.

- [ ] **Step 4: 구현**

`src/font.rs`의 테스트 위쪽에:

```rust
use std::collections::HashMap;

#[derive(Debug)]
pub enum FontError {
    TooShort,
    MissingTable(&'static str),
    UnsupportedCmap,
}

fn be_u16(d: &[u8], off: usize) -> u16 {
    ((d[off] as u16) << 8) | d[off + 1] as u16
}
fn be_i16(d: &[u8], off: usize) -> i16 {
    be_u16(d, off) as i16
}
fn be_u32(d: &[u8], off: usize) -> u32 {
    ((be_u16(d, off) as u32) << 16) | be_u16(d, off + 2) as u32
}

struct Cmap4 {
    seg_count: usize,
    end_code: Vec<u16>,
    start_code: Vec<u16>,
    id_delta: Vec<i16>,
    id_range_offset: Vec<u16>,
    id_range_offset_pos: usize,
}

pub struct Font {
    data: Vec<u8>,
    tables: HashMap<[u8; 4], (usize, usize)>,
    units_per_em: u16,
    index_to_loc_format: i16,
    num_glyphs: u16,
    ascent: i16,
    descent: i16,
    line_gap: i16,
    num_h_metrics: u16,
    cmap: Cmap4,
}

impl Font {
    pub fn from_bytes(data: Vec<u8>) -> Result<Font, FontError> {
        if data.len() < 12 {
            return Err(FontError::TooShort);
        }
        let num_tables = be_u16(&data, 4) as usize;
        let mut tables = HashMap::new();
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            if rec + 16 > data.len() {
                return Err(FontError::TooShort);
            }
            let mut tag = [0u8; 4];
            tag.copy_from_slice(&data[rec..rec + 4]);
            let off = be_u32(&data, rec + 8) as usize;
            let len = be_u32(&data, rec + 12) as usize;
            tables.insert(tag, (off, len));
        }

        let get = |t: &'static [u8; 4]| tables.get(t).copied().ok_or(FontError::MissingTable(
            match t { b"head" => "head", b"maxp" => "maxp", b"hhea" => "hhea",
                      b"hmtx" => "hmtx", b"loca" => "loca", b"glyf" => "glyf",
                      b"cmap" => "cmap", _ => "?" }));

        let (head, _) = get(b"head")?;
        let units_per_em = be_u16(&data, head + 18);
        let index_to_loc_format = be_i16(&data, head + 50);

        let (maxp, _) = get(b"maxp")?;
        let num_glyphs = be_u16(&data, maxp + 4);

        let (hhea, _) = get(b"hhea")?;
        let ascent = be_i16(&data, hhea + 4);
        let descent = be_i16(&data, hhea + 6);
        let line_gap = be_i16(&data, hhea + 8);
        let num_h_metrics = be_u16(&data, hhea + 34);

        get(b"hmtx")?;
        get(b"loca")?;
        get(b"glyf")?;

        let cmap = parse_cmap(&data, &tables)?;

        Ok(Font {
            data,
            tables,
            units_per_em,
            index_to_loc_format,
            num_glyphs,
            ascent,
            descent,
            line_gap,
            num_h_metrics,
            cmap,
        })
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
    pub fn ascent(&self) -> i16 {
        self.ascent
    }
    pub fn descent(&self) -> i16 {
        self.descent
    }
    pub fn line_gap(&self) -> i16 {
        self.line_gap
    }
}

fn parse_cmap(data: &[u8], tables: &HashMap<[u8; 4], (usize, usize)>) -> Result<Cmap4, FontError> {
    let (cmap_off, _) = *tables.get(b"cmap").ok_or(FontError::MissingTable("cmap"))?;
    let num_sub = be_u16(data, cmap_off + 2) as usize;
    let mut best: Option<(i32, usize)> = None;
    for i in 0..num_sub {
        let rec = cmap_off + 4 + i * 8;
        let plat = be_u16(data, rec);
        let enc = be_u16(data, rec + 2);
        let sub_off = be_u32(data, rec + 4) as usize;
        let sub = cmap_off + sub_off;
        let format = be_u16(data, sub);
        if format == 4 {
            let score = match (plat, enc) {
                (3, 1) => 3,
                (0, _) => 2,
                (3, 0) => 1,
                _ => 0,
            };
            if best.map_or(true, |(s, _)| score > s) {
                best = Some((score, sub));
            }
        }
    }
    let sub = best.ok_or(FontError::UnsupportedCmap)?.1;
    let seg_x2 = be_u16(data, sub + 6) as usize;
    let seg_count = seg_x2 / 2;
    let end_base = sub + 14;
    let start_base = end_base + seg_x2 + 2;
    let delta_base = start_base + seg_x2;
    let range_base = delta_base + seg_x2;
    let end_code = (0..seg_count).map(|i| be_u16(data, end_base + i * 2)).collect();
    let start_code = (0..seg_count).map(|i| be_u16(data, start_base + i * 2)).collect();
    let id_delta = (0..seg_count).map(|i| be_i16(data, delta_base + i * 2)).collect();
    let id_range_offset = (0..seg_count).map(|i| be_u16(data, range_base + i * 2)).collect();
    Ok(Cmap4 {
        seg_count,
        end_code,
        start_code,
        id_delta,
        id_range_offset,
        id_range_offset_pos: range_base,
    })
}
```

- [ ] **Step 5: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: `parses_global_metrics` PASS. (`main.rs`에 `mod font;`를 추가해야 컴파일됨 — 추가할 것.)

- [ ] **Step 6: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add assets/fonts src/font.rs src/main.rs
git commit -m "$(printf 'feat(font): TTF 테이블 디렉터리 + 전역 메트릭 파싱 + 폰트 번들\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 2: cmap format 4 → `glyph_index`

**Files:**
- Modify: `src/font.rs`

**Interfaces:**
- Consumes: `Font.cmap` (Task 1)
- Produces: `impl Font { pub fn glyph_index(&self, c: char) -> u16 }`

- [ ] **Step 1: 실패 테스트 추가**

`src/font.rs` 테스트 모듈에:

```rust
    #[test]
    fn maps_ascii_to_glyphs() {
        let f = load();
        let a = f.glyph_index('A');
        let b = f.glyph_index('B');
        assert_ne!(a, 0, "'A' should map to a real glyph");
        assert_ne!(b, 0);
        assert_ne!(a, b, "'A' and 'B' should differ");
        assert_eq!(f.glyph_index('\u{1F600}'), 0, "non-BMP -> .notdef");
    }
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: 컴파일 실패 — `glyph_index` 미정의.

- [ ] **Step 3: 구현**

`impl Font`에 추가:

```rust
    pub fn glyph_index(&self, c: char) -> u16 {
        let cp = c as u32;
        if cp > 0xFFFF {
            return 0;
        }
        let c = cp as u16;
        let cm = &self.cmap;
        for i in 0..cm.seg_count {
            if c <= cm.end_code[i] && c >= cm.start_code[i] {
                if cm.id_range_offset[i] == 0 {
                    return (c as i32 + cm.id_delta[i] as i32) as u16;
                }
                let addr = cm.id_range_offset_pos
                    + 2 * i
                    + cm.id_range_offset[i] as usize
                    + 2 * (c - cm.start_code[i]) as usize;
                let g = be_u16(&self.data, addr);
                if g == 0 {
                    return 0;
                }
                return (g as i32 + cm.id_delta[i] as i32) as u16;
            }
        }
        0
    }
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: `maps_ascii_to_glyphs` 포함 PASS.

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/font.rs
git commit -m "$(printf 'feat(font): cmap format 4 글자→글리프 매핑\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 3: hmtx → `advance_width`

**Files:**
- Modify: `src/font.rs`

**Interfaces:**
- Consumes: `Font.tables`, `Font.num_h_metrics`
- Produces: `impl Font { pub fn advance_width(&self, glyph_id: u16) -> u16 }`

- [ ] **Step 1: 실패 테스트 추가**

```rust
    #[test]
    fn advance_widths_are_positive() {
        let f = load();
        let a = f.advance_width(f.glyph_index('A'));
        let space = f.advance_width(f.glyph_index(' '));
        assert!(a > 0);
        assert!(space > 0, "space should still advance the pen");
    }
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: 컴파일 실패 — `advance_width` 미정의.

- [ ] **Step 3: 구현**

```rust
    pub fn advance_width(&self, glyph_id: u16) -> u16 {
        let (hmtx, _) = self.tables[b"hmtx"];
        let n = self.num_h_metrics as usize;
        let i = (glyph_id as usize).min(n.saturating_sub(1));
        be_u16(&self.data, hmtx + i * 4)
    }
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: PASS.

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/font.rs
git commit -m "$(printf 'feat(font): hmtx advance width\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 4: loca + glyf 단순 글리프 외곽선

**Files:**
- Modify: `src/font.rs`

**Interfaces:**
- Consumes: `Font.tables`, `Font.index_to_loc_format`
- Produces:
  - `pub struct Point { pub x: f32, pub y: f32, pub on_curve: bool }`
  - `pub struct Contour { pub points: Vec<Point> }`
  - `pub struct Glyph { pub contours: Vec<Contour>, pub x_min: i16, pub y_min: i16, pub x_max: i16, pub y_max: i16 }`
  - `impl Font { pub fn outline(&self, glyph_id: u16) -> Glyph }`

- [ ] **Step 1: 실패 테스트 추가**

```rust
    #[test]
    fn outlines_have_contours() {
        let f = load();
        let o = f.outline(f.glyph_index('o'));
        assert!(o.contours.len() >= 1, "'o' should have at least one contour");
        let total_pts: usize = o.contours.iter().map(|c| c.points.len()).sum();
        assert!(total_pts > 0);
        // space is an empty glyph (advance only, no contours)
        let sp = f.outline(f.glyph_index(' '));
        assert_eq!(sp.contours.len(), 0);
    }
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: 컴파일 실패 — `outline`/`Glyph` 미정의.

- [ ] **Step 3: 구현**

`src/font.rs` 상단(타입)과 `impl Font`에 추가:

```rust
#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub on_curve: bool,
}

#[derive(Clone, Debug)]
pub struct Contour {
    pub points: Vec<Point>,
}

#[derive(Clone, Debug)]
pub struct Glyph {
    pub contours: Vec<Contour>,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}
```

```rust
    fn glyph_range(&self, gid: u16) -> (usize, usize) {
        let (loca, _) = self.tables[b"loca"];
        if self.index_to_loc_format == 0 {
            let a = be_u16(&self.data, loca + gid as usize * 2) as usize * 2;
            let b = be_u16(&self.data, loca + (gid as usize + 1) * 2) as usize * 2;
            (a, b)
        } else {
            let a = be_u32(&self.data, loca + gid as usize * 4) as usize;
            let b = be_u32(&self.data, loca + (gid as usize + 1) * 4) as usize;
            (a, b)
        }
    }

    pub fn outline(&self, glyph_id: u16) -> Glyph {
        let (glyf, _) = self.tables[b"glyf"];
        let (start, end) = self.glyph_range(glyph_id);
        if end <= start {
            return Glyph { contours: vec![], x_min: 0, y_min: 0, x_max: 0, y_max: 0 };
        }
        let g = glyf + start;
        let num_contours = be_i16(&self.data, g);
        let x_min = be_i16(&self.data, g + 2);
        let y_min = be_i16(&self.data, g + 4);
        let x_max = be_i16(&self.data, g + 6);
        let y_max = be_i16(&self.data, g + 8);
        if num_contours <= 0 {
            // composite (<0) is out of scope; 0 contours = empty
            return Glyph { contours: vec![], x_min, y_min, x_max, y_max };
        }
        let num_contours = num_contours as usize;
        let mut p = g + 10;
        let mut end_pts = Vec::with_capacity(num_contours);
        for _ in 0..num_contours {
            end_pts.push(be_u16(&self.data, p));
            p += 2;
        }
        let num_points = *end_pts.last().unwrap() as usize + 1;
        let instr_len = be_u16(&self.data, p) as usize;
        p += 2 + instr_len;

        // flags (with repeat)
        let mut flags = Vec::with_capacity(num_points);
        while flags.len() < num_points {
            let f = self.data[p];
            p += 1;
            flags.push(f);
            if f & 0x08 != 0 {
                let repeat = self.data[p];
                p += 1;
                for _ in 0..repeat {
                    if flags.len() < num_points {
                        flags.push(f);
                    }
                }
            }
        }

        // x coords
        let mut xs = Vec::with_capacity(num_points);
        let mut x: i32 = 0;
        for &f in &flags {
            if f & 0x02 != 0 {
                let dx = self.data[p] as i32;
                p += 1;
                x += if f & 0x10 != 0 { dx } else { -dx };
            } else if f & 0x10 == 0 {
                x += be_i16(&self.data, p) as i32;
                p += 2;
            }
            xs.push(x);
        }
        // y coords
        let mut ys = Vec::with_capacity(num_points);
        let mut y: i32 = 0;
        for &f in &flags {
            if f & 0x04 != 0 {
                let dy = self.data[p] as i32;
                p += 1;
                y += if f & 0x20 != 0 { dy } else { -dy };
            } else if f & 0x20 == 0 {
                y += be_i16(&self.data, p) as i32;
                p += 2;
            }
            ys.push(y);
        }

        // split into contours
        let mut contours = Vec::with_capacity(num_contours);
        let mut s = 0usize;
        for &e in &end_pts {
            let e = e as usize;
            let mut points = Vec::with_capacity(e - s + 1);
            for i in s..=e {
                points.push(Point { x: xs[i] as f32, y: ys[i] as f32, on_curve: flags[i] & 0x01 != 0 });
            }
            contours.push(Contour { points });
            s = e + 1;
        }
        Glyph { contours, x_min, y_min, x_max, y_max }
    }
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test font`
Expected: PASS. (만약 `' '`가 빈 글리프가 아니면, 폰트별 차이이므로 테스트의 빈 글리프 단언을 그 폰트의 빈 글리프에 맞춰 조정.)

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/font.rs
git commit -m "$(printf 'feat(font): loca + glyf 단순 글리프 외곽선 추출\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 5: `raster.rs` — 평탄화 + 커버리지 스캔라인 래스터화

**Files:**
- Create: `src/raster.rs`
- Modify: `src/main.rs` (`mod raster;` 추가)

**Interfaces:**
- Consumes: `crate::font::{Font, Glyph, Point}`
- Produces:
  - `pub struct CoverageBitmap { pub width: usize, pub height: usize, pub data: Vec<u8>, pub left: i32, pub top: i32, pub advance: f32 }`
  - `pub fn rasterize_glyph(font: &Font, glyph_id: u16, px_per_em: f32) -> CoverageBitmap`

- [ ] **Step 1: 실패 테스트 먼저 작성**

`src/raster.rs` 맨 아래:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::font::Font;

    fn load() -> Font {
        Font::from_bytes(std::fs::read("assets/fonts/Kestrel.ttf").unwrap()).unwrap()
    }

    #[test]
    fn rasterizes_glyph_with_antialiasing() {
        let f = load();
        let bm = rasterize_glyph(&f, f.glyph_index('A'), 64.0);
        assert!(bm.width > 0 && bm.height > 0);
        let ink: u32 = bm.data.iter().map(|&v| v as u32).sum();
        assert!(ink > 0, "glyph should have ink");
        // 안티앨리어싱: 0도 255도 아닌 중간 커버리지 픽셀이 존재
        assert!(bm.data.iter().any(|&v| v > 0 && v < 255), "expected AA edge pixels");
        assert!(bm.advance > 0.0);
    }

    #[test]
    fn empty_glyph_has_advance_but_no_pixels() {
        let f = load();
        let bm = rasterize_glyph(&f, f.glyph_index(' '), 64.0);
        assert_eq!(bm.width * bm.height, 0);
        assert!(bm.advance > 0.0);
    }
}
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test raster`
Expected: 컴파일 실패 — `rasterize_glyph` 미정의.

- [ ] **Step 3: 구현**

`src/raster.rs` 위쪽에:

```rust
use crate::font::{Font, Glyph, Point};

pub struct CoverageBitmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,
    pub left: i32,
    pub top: i32,
    pub advance: f32,
}

pub fn rasterize_glyph(font: &Font, glyph_id: u16, px_per_em: f32) -> CoverageBitmap {
    let scale = px_per_em / font.units_per_em() as f32;
    let advance = font.advance_width(glyph_id) as f32 * scale;
    let glyph = font.outline(glyph_id);

    let empty = CoverageBitmap { width: 0, height: 0, data: vec![], left: 0, top: 0, advance };
    if glyph.contours.is_empty() {
        return empty;
    }

    // 1) 윤곽선을 폰트 단위 폴리라인으로 평탄화
    let polylines: Vec<Vec<(f32, f32)>> =
        glyph.contours.iter().map(|c| flatten_contour(&c.points)).collect();

    // 2) 바운드 계산
    let (mut minx, mut miny, mut maxx, mut maxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for poly in &polylines {
        for &(px, py) in poly {
            minx = minx.min(px);
            miny = miny.min(py);
            maxx = maxx.max(px);
            maxy = maxy.max(py);
        }
    }
    if !(maxx > minx && maxy > miny) {
        return empty;
    }

    let pad = 1.0f32;
    let width = ((maxx - minx) * scale).ceil() as usize + 2 * pad as usize;
    let height = ((maxy - miny) * scale).ceil() as usize + 2 * pad as usize;
    if width == 0 || height == 0 {
        return empty;
    }

    // device 변환 (Y 반전)
    let to_dev = |px: f32, py: f32| -> (f32, f32) {
        ((px - minx) * scale + pad, (maxy - py) * scale + pad)
    };

    // 3) 에지 목록 (수평 에지 제외)
    let mut edges: Vec<[f32; 4]> = Vec::new();
    for poly in &polylines {
        for w in poly.windows(2) {
            let (x0, y0) = to_dev(w[0].0, w[0].1);
            let (x1, y1) = to_dev(w[1].0, w[1].1);
            if y0 != y1 {
                edges.push([x0, y0, x1, y1]);
            }
        }
    }

    // 4) 커버리지 스캔라인 (분석적 수평 + 수직 오버샘플)
    const SUB: usize = 5;
    let mut cov = vec![0f32; width * height];
    for row in 0..height {
        for k in 0..SUB {
            let sy = row as f32 + (k as f32 + 0.5) / SUB as f32;
            let mut xs: Vec<(f32, i32)> = Vec::new();
            for e in &edges {
                let (y0, y1) = (e[1], e[3]);
                let (lo, hi) = if y0 < y1 { (y0, y1) } else { (y1, y0) };
                if sy >= lo && sy < hi {
                    let t = (sy - y0) / (y1 - y0);
                    let x = e[0] + t * (e[2] - e[0]);
                    let dir = if y1 > y0 { 1 } else { -1 };
                    xs.push((x, dir));
                }
            }
            if xs.len() < 2 {
                continue;
            }
            xs.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            let mut winding = 0;
            for w in 0..xs.len() - 1 {
                winding += xs[w].1;
                if winding != 0 {
                    let x0 = xs[w].0.max(0.0);
                    let x1 = xs[w + 1].0.min(width as f32);
                    if x1 > x0 {
                        add_span(&mut cov, row * width, width, x0, x1, 1.0 / SUB as f32);
                    }
                }
            }
        }
    }

    let data: Vec<u8> = cov.iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    let left = (minx * scale).round() as i32 - pad as i32;
    let top = (maxy * scale).round() as i32 + pad as i32;

    CoverageBitmap { width, height, data, left, top, advance }
}

fn add_span(cov: &mut [f32], row_off: usize, width: usize, x0: f32, x1: f32, weight: f32) {
    let c0 = x0.floor() as usize;
    let c1 = (x1.ceil() as usize).min(width);
    for c in c0..c1 {
        let cell0 = c as f32;
        let cell1 = c as f32 + 1.0;
        let overlap = (x1.min(cell1) - x0.max(cell0)).max(0.0);
        cov[row_off + c] += overlap * weight;
    }
}

fn flatten_quad(poly: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)) {
    const STEPS: usize = 8;
    for s in 1..=STEPS {
        let t = s as f32 / STEPS as f32;
        let mt = 1.0 - t;
        let x = mt * mt * p0.0 + 2.0 * mt * t * p1.0 + t * t * p2.0;
        let y = mt * mt * p0.1 + 2.0 * mt * t * p1.1 + t * t * p2.1;
        poly.push((x, y));
    }
}

fn flatten_contour(pts: &[Point]) -> Vec<(f32, f32)> {
    let n = pts.len();
    if n == 0 {
        return vec![];
    }
    // 1) 연속 off-curve 사이 암묵 중점(on-curve) 삽입
    let mut exp: Vec<(f32, f32, bool)> = Vec::with_capacity(n * 2);
    for i in 0..n {
        let p = pts[i];
        let q = pts[(i + 1) % n];
        exp.push((p.x, p.y, p.on_curve));
        if !p.on_curve && !q.on_curve {
            exp.push(((p.x + q.x) / 2.0, (p.y + q.y) / 2.0, true));
        }
    }
    // 2) on-curve 점에서 시작하도록 회전
    let start = match exp.iter().position(|e| e.2) {
        Some(s) => s,
        None => return vec![],
    };
    let m = exp.len();
    let mut ring: Vec<(f32, f32, bool)> = (0..m).map(|i| exp[(start + i) % m]).collect();
    ring.push(ring[0]); // 닫기 (마지막은 on-curve 시작점)

    // 3) 폴리라인 생성
    let mut poly = vec![(ring[0].0, ring[0].1)];
    let mut cur = (ring[0].0, ring[0].1);
    let mut i = 1;
    while i < ring.len() {
        let p = ring[i];
        if p.2 {
            poly.push((p.0, p.1));
            cur = (p.0, p.1);
            i += 1;
        } else {
            let end = ring[i + 1]; // off-curve 다음은 항상 on-curve (확장으로 보장, 마지막도 on-curve)
            flatten_quad(&mut poly, cur, (p.0, p.1), (end.0, end.1));
            cur = (end.0, end.1);
            i += 2;
        }
    }
    poly
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test raster`
Expected: 2개 PASS.

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/raster.rs src/main.rs
git commit -m "$(printf 'feat(raster): 윤곽선 평탄화 + 커버리지 스캔라인 글리프 래스터화\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 6: 글리프 캐시 + PPM 덤프로 눈 검증

**Files:**
- Modify: `src/raster.rs` (GlyphCache 추가)
- Modify: `src/main.rs` (글리프 덤프 모드)

**Interfaces:**
- Consumes: `rasterize_glyph`, `crate::font::Font`
- Produces:
  - `pub struct GlyphCache { ... }`
  - `impl GlyphCache { pub fn new() -> GlyphCache; pub fn get(&mut self, font: &Font, glyph_id: u16, px_per_em: f32) -> &CoverageBitmap }`

- [ ] **Step 1: 실패 테스트 추가 (`src/raster.rs`)**

```rust
    #[test]
    fn cache_returns_consistent_bitmap() {
        let f = load();
        let mut cache = GlyphCache::new();
        let (w1, h1) = {
            let bm = cache.get(&f, f.glyph_index('g'), 48.0);
            (bm.width, bm.height)
        };
        let (w2, h2) = {
            let bm = cache.get(&f, f.glyph_index('g'), 48.0);
            (bm.width, bm.height)
        };
        assert_eq!((w1, h1), (w2, h2));
    }
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test raster`
Expected: 컴파일 실패 — `GlyphCache` 미정의.

- [ ] **Step 3: GlyphCache 구현 (`src/raster.rs`)**

```rust
use std::collections::HashMap;

pub struct GlyphCache {
    map: HashMap<(u16, u32), CoverageBitmap>,
}

impl GlyphCache {
    pub fn new() -> GlyphCache {
        GlyphCache { map: HashMap::new() }
    }

    pub fn get(&mut self, font: &Font, glyph_id: u16, px_per_em: f32) -> &CoverageBitmap {
        let key = (glyph_id, px_per_em.to_bits());
        self.map
            .entry(key)
            .or_insert_with(|| rasterize_glyph(font, glyph_id, px_per_em))
    }
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test raster`
Expected: PASS.

- [ ] **Step 5: 글리프 덤프 모드 추가 (`src/main.rs`)**

`src/main.rs` 맨 위 `mod` 목록에 `mod font;`, `mod raster;`가 있는지 확인하고, `main()` 함수 시작부에 아래 분기를 추가(기존 동작보다 먼저 검사):

```rust
    // 글리프 덤프 모드: KESTREL_GLYPH 문자열을 래스터화해 그레이스케일 PPM으로.
    if let Ok(text) = std::env::var("KESTREL_GLYPH") {
        let out = std::env::var("KESTREL_GLYPH_OUT").unwrap_or_else(|_| "glyphs.ppm".to_string());
        dump_glyphs(&text, &out);
        return;
    }
```

그리고 `main.rs` 하단에 헬퍼 추가:

```rust
fn dump_glyphs(text: &str, path: &str) {
    let bytes = std::fs::read("assets/fonts/Kestrel.ttf").expect("read font");
    let font = font::Font::from_bytes(bytes).expect("parse font");
    let px = 96.0f32;

    // 각 글자 래스터화
    let mut cache = raster::GlyphCache::new();
    let gap = 8usize;
    let mut cells: Vec<&raster::CoverageBitmap> = Vec::new();
    for ch in text.chars() {
        let gid = font.glyph_index(ch);
        cells.push(cache.get(&font, gid, px));
    }

    let canvas_h = cells.iter().map(|b| b.height).max().unwrap_or(1).max(1);
    let canvas_w: usize = cells.iter().map(|b| b.width + gap).sum::<usize>().max(1);

    // 어두운 배경 + 흰 글자 (커버리지=밝기)
    let mut img = vec![20u8; canvas_w * canvas_h * 3];
    let mut pen = 0usize;
    for bm in &cells {
        for y in 0..bm.height {
            for x in 0..bm.width {
                let v = bm.data[y * bm.width + x];
                let px_x = pen + x;
                if px_x < canvas_w && y < canvas_h {
                    let idx = (y * canvas_w + px_x) * 3;
                    let g = 20u8.saturating_add(v); // 배경 위에 글자 밝기
                    img[idx] = g;
                    img[idx + 1] = g;
                    img[idx + 2] = g;
                }
            }
        }
        pen += bm.width + gap;
    }

    let mut data = format!("P6\n{} {}\n255\n", canvas_w, canvas_h).into_bytes();
    data.extend_from_slice(&img);
    std::fs::write(path, data).expect("write ppm");
    println!("glyphs rendered to {}", path);
}
```

- [ ] **Step 6: 눈 검증 (헤드리스)**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
SCRATCH="$(pwd)/target"
KESTREL_GLYPH="Ago@Rk" KESTREL_GLYPH_OUT="$SCRATCH/glyphs.ppm" cargo run
sips -s format png "$SCRATCH/glyphs.ppm" --out "$SCRATCH/glyphs.png"
```
Expected: `glyphs.png`를 열면 "Ago@Rk" 글자들이 안티앨리어싱되어 또렷하게 보임. (특히 'o','g'의 구멍이 비어 있어야 함 = nonzero winding 정상.) 글자가 깨지거나 채워지면 `flatten_contour`/winding을 디버그.

- [ ] **Step 7: 전체 테스트 + 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
cargo test
git add src/raster.rs src/main.rs
git commit -m "$(printf 'feat(raster): 글리프 캐시 + PPM 덤프 검증 (M2a 완성)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Self-Review

**1. Spec coverage** (스펙 vs 태스크):
- font.rs 테이블 파싱(head/maxp/hhea/hmtx/cmap/loca/glyf) → Task 1~4. cmap format 4 → Task 2. advance → Task 3. glyf 단순 글리프 외곽선 → Task 4. raster 커버리지 스캔라인 + CoverageBitmap → Task 5. 글리프 캐시 → Task 6. 폰트 에셋 → Task 1. PPM 눈 검증 → Task 6. 모든 M2a 스펙 항목 커버.
- 비범위(합성 글리프/CFF/커닝/한글/레이아웃 통합)는 코드에서 명시적으로 제외(합성=빈 외곽선).

**2. Placeholder scan:** "TBD/적절히" 없음. 모든 코드 단계 완전. 폰트 확보(Task 1 Step 1)만 구현자가 OFL TTF를 고르는 수동 단계지만, 검증 명령과 경로(`assets/fonts/Kestrel.ttf`)를 명시.

**3. Type consistency 점검:**
- `Font::from_bytes`→`Result<Font,FontError>`, `units_per_em()->u16`, `ascent/descent/line_gap()->i16`, `glyph_index(char)->u16`, `advance_width(u16)->u16`, `outline(u16)->Glyph` — Task 1~4 정의, Task 5/6에서 동일 사용. ✓
- `Glyph{contours:Vec<Contour>, x_min..y_max:i16}`, `Contour{points:Vec<Point>}`, `Point{x,y:f32,on_curve:bool}` — Task 4 정의, Task 5 `flatten_contour(&[Point])`에서 사용. ✓
- `CoverageBitmap{width,height:usize,data:Vec<u8>,left,top:i32,advance:f32}`, `rasterize_glyph(&Font,u16,f32)->CoverageBitmap` — Task 5 정의, Task 6 GlyphCache/덤프에서 사용. ✓
- `GlyphCache::new()`, `get(&mut,&Font,u16,f32)->&CoverageBitmap` — Task 6 정의/사용. ✓

불일치 없음.
