# Kestrel M2b — 인라인 텍스트 레이아웃 + 페인트 통합 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 텍스트가 든 HTML이 직접 만든 글리프로, 콘텐츠 폭에서 워드랩되며 지정 색으로 창에 렌더링되게 한다.

**Architecture:** `layout.rs`에 인라인 텍스트 레이아웃(워드랩)을 추가해 `LayoutBox.glyphs`를 채우고, `paint.rs`가 글리프 커버리지를 텍스트 색으로 알파 블렌딩한다. `layout`/`paint`는 폰트를 인자로 받는다.

**Tech Stack:** Rust(edition 2021). M2a의 `font`/`raster` 사용.

## Global Constraints

- 프로젝트 위치: `~/Documents/Projects/kestrel/`. 다른 저장소 건드리지 않는다.
- 외부 폰트/텍스트 셰이핑 크레이트 금지. 전부 직접.
- 잎(leaf) 텍스트 블록만 처리(혼합 인라인/중첩 인라인 요소 미지원).
- 단일 폰트(`assets/fonts/Kestrel.ttf`), 단일 크기, 왼쪽 정렬, 공백 접기. 커닝/양끝맞춤/bidi 없음.
- 기본값: `font-size` 16px, `color` 검정(0,0,0).
- CSS 파서는 변경하지 않는다(`font-size`/`color`는 이미 파싱됨).
- 계약 타입은 스펙 5.1을 따른다.
- 커밋 메시지 끝에: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: `layout.rs` — 인라인 텍스트 레이아웃 + 폰트 전달

**Files:**
- Modify: `src/layout.rs`
- Modify: `src/style.rs` (`node` 필드의 `#[allow(dead_code)]` 제거 — 이제 사용됨)

**Interfaces:**
- Consumes: `crate::font::Font`, `crate::css::{Color, Value}`, `crate::dom::NodeType`, `crate::style::StyledNode`
- Produces:
  - `pub struct GlyphInstance { pub glyph_id: u16, pub x: f32, pub baseline_y: f32, pub px: f32, pub color: Color }`
  - `LayoutBox` 에 `pub glyphs: Vec<GlyphInstance>` 필드
  - `pub fn layout_tree<'a>(node: &'a StyledNode<'a>, containing_block: Dimensions, font: &Font) -> LayoutBox<'a>`

- [ ] **Step 1: 실패 테스트 먼저 — 기존 테스트 시그니처 갱신 + 텍스트 테스트 추가**

`src/layout.rs`의 `mod tests`를 아래로 교체(기존 3개 테스트의 `layout_tree` 호출에 `&font` 추가 + 텍스트 테스트 2개 신규):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn font() -> crate::font::Font {
        crate::font::Font::from_bytes(std::fs::read("assets/fonts/Kestrel.ttf").unwrap()).unwrap()
    }

    fn layout_for(html: &str, css: &str, viewport_width: f32) -> Dimensions {
        let root = crate::html::parse(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = viewport_width;
        let f = font();
        let lb = layout_tree(&styled, viewport, &f);
        lb.dimensions
    }

    #[test]
    fn fixed_width_and_height_block() {
        let d = layout_for(
            "<div></div>",
            "div { display: block; width: 200px; height: 100px; }",
            800.0,
        );
        assert_eq!(d.content.width, 200.0);
        assert_eq!(d.content.height, 100.0);
        assert_eq!(d.content.x, 0.0);
        assert_eq!(d.content.y, 0.0);
    }

    #[test]
    fn auto_width_fills_containing_block_minus_padding() {
        let d = layout_for("<div></div>", "div { display: block; padding: 10px; }", 300.0);
        assert_eq!(d.content.width, 280.0);
        assert_eq!(d.content.x, 10.0);
    }

    #[test]
    fn children_stack_vertically() {
        let root = crate::html::parse(
            "<div class=\"outer\"><div class=\"inner\"></div><div class=\"inner\"></div></div>"
                .to_string(),
        );
        let ss = crate::css::parse(
            ".outer { display: block; } .inner { display: block; height: 50px; }".to_string(),
        );
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let f = font();
        let lb = layout_tree(&styled, viewport, &f);
        assert_eq!(lb.children.len(), 2);
        assert_eq!(lb.children[0].dimensions.content.y, 0.0);
        assert_eq!(lb.children[1].dimensions.content.y, 50.0);
        assert_eq!(lb.dimensions.content.height, 100.0);
    }

    #[test]
    fn text_box_produces_glyphs_and_height() {
        let root = crate::html::parse("<p>hello world</p>".to_string());
        let ss = crate::css::parse("p { display: block; font-size: 20px; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 400.0;
        let f = font();
        let lb = layout_tree(&styled, viewport, &f);
        assert!(!lb.glyphs.is_empty(), "text should produce glyphs");
        assert!(lb.dimensions.content.height > 0.0);
    }

    #[test]
    fn long_text_wraps_to_multiple_lines() {
        let root = crate::html::parse(
            "<p>aaaa bbbb cccc dddd eeee ffff gggg hhhh</p>".to_string(),
        );
        let ss = crate::css::parse("p { display: block; font-size: 20px; }".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: Dimensions = Default::default();
        viewport.content.width = 120.0; // 좁게 → 줄바꿈 강제
        let f = font();
        let lb = layout_tree(&styled, viewport, &f);
        let first = lb.glyphs.first().unwrap().baseline_y;
        let last = lb.glyphs.last().unwrap().baseline_y;
        assert!(last > first, "later glyphs should be on lower lines");
    }
}
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test layout`
Expected: 컴파일 실패 — `layout_tree`가 인자 3개를 받지 않음 / `glyphs` 필드 없음.

- [ ] **Step 3: 구현 — 타입/필드/폰트 전달/텍스트 레이아웃**

`src/layout.rs` 상단 import 교체:

```rust
use crate::css::Unit::Px;
use crate::css::Value::{Keyword, Length};
use crate::css::{Color, Value};
use crate::dom::NodeType;
use crate::font::Font;
use crate::style::{Display, StyledNode};
```

`GlyphInstance` 추가(예: `Dimensions` 정의 근처):

```rust
#[derive(Clone, Copy, Debug)]
pub struct GlyphInstance {
    pub glyph_id: u16,
    pub x: f32,
    pub baseline_y: f32,
    pub px: f32,
    pub color: Color,
}
```

`LayoutBox`에 필드 추가 + `new` 갱신:

```rust
pub struct LayoutBox<'a> {
    pub dimensions: Dimensions,
    pub styled_node: &'a StyledNode<'a>,
    pub children: Vec<LayoutBox<'a>>,
    pub glyphs: Vec<GlyphInstance>,
}

impl<'a> LayoutBox<'a> {
    fn new(styled_node: &'a StyledNode<'a>) -> LayoutBox<'a> {
        LayoutBox {
            dimensions: Default::default(),
            styled_node,
            children: Vec::new(),
            glyphs: Vec::new(),
        }
    }
```

`layout`/`layout_children`에 `font` 전달, `layout_text` 호출 추가:

```rust
    fn layout(&mut self, containing_block: Dimensions, font: &Font) {
        self.calculate_width(containing_block);
        self.calculate_position(containing_block);
        self.layout_children(font);
        self.layout_text(font);
        self.calculate_height();
    }

    fn layout_children(&mut self, font: &Font) {
        let d = &mut self.dimensions;
        for child in &mut self.children {
            child.layout(*d, font);
            d.content.height += child.dimensions.margin_box().height;
        }
    }
```

`layout_text` 메서드 추가(`impl<'a> LayoutBox<'a>` 안):

```rust
    fn layout_text(&mut self, font: &Font) {
        let mut text = String::new();
        for child in &self.styled_node.children {
            if let NodeType::Text(t) = &child.node.node_type {
                text.push_str(t);
            }
        }
        let text = text.trim();
        if text.is_empty() {
            return;
        }

        let style = self.styled_node;
        let font_size = style
            .value("font-size")
            .map(|v| v.to_px())
            .filter(|&p| p > 0.0)
            .unwrap_or(16.0);
        let color = match style.value("color") {
            Some(Value::Color(c)) => c,
            _ => Color { r: 0, g: 0, b: 0, a: 255 },
        };

        let scale = font_size / font.units_per_em() as f32;
        let line_height =
            (font.ascent() as f32 - font.descent() as f32 + font.line_gap() as f32) * scale;
        let ascent_px = font.ascent() as f32 * scale;
        let space_adv = font.advance_width(font.glyph_index(' ')) as f32 * scale;

        let content_x = self.dimensions.content.x;
        let content_w = self.dimensions.content.width;
        let mut pen_x = content_x;
        let mut baseline = self.dimensions.content.y + ascent_px;
        let mut lines = 1;

        for word in text.split_whitespace() {
            let word_w: f32 = word
                .chars()
                .map(|c| font.advance_width(font.glyph_index(c)) as f32 * scale)
                .sum();
            if pen_x > content_x && pen_x + word_w > content_x + content_w {
                pen_x = content_x;
                baseline += line_height;
                lines += 1;
            }
            for c in word.chars() {
                let gid = font.glyph_index(c);
                self.glyphs.push(GlyphInstance {
                    glyph_id: gid,
                    x: pen_x,
                    baseline_y: baseline,
                    px: font_size,
                    color,
                });
                pen_x += font.advance_width(gid) as f32 * scale;
            }
            pen_x += space_adv;
        }

        self.dimensions.content.height = lines as f32 * line_height;
    }
```

`layout_tree` 시그니처 변경:

```rust
pub fn layout_tree<'a>(
    node: &'a StyledNode<'a>,
    mut containing_block: Dimensions,
    font: &Font,
) -> LayoutBox<'a> {
    containing_block.content.height = 0.0;
    let mut root_box = build_layout_tree(node);
    root_box.layout(containing_block, font);
    root_box
}
```

`src/style.rs`에서 `node` 필드 위의 `#[allow(dead_code)]`와 그 주석을 제거(이제 layout이 사용).

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test layout`
Expected: 5개 PASS (기존 3 + 텍스트 2). (참고: `calculate_height`는 명시 height가 없으면 텍스트가 정한 height를 유지.)

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/layout.rs src/style.rs
git commit -m "$(printf 'feat(layout): 인라인 텍스트 워드랩 → GlyphInstance + 폰트 전달\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 2: `paint.rs` — 글리프 알파 블렌딩 + 폰트/캐시 전달

**Files:**
- Modify: `src/paint.rs`

**Interfaces:**
- Consumes: `crate::layout::{LayoutBox, Rect, GlyphInstance}`, `crate::css::Color`, `crate::font::Font`, `crate::raster::{GlyphCache, CoverageBitmap}`
- Produces: `pub fn paint(layout_root: &LayoutBox, bounds: Rect, font: &Font, cache: &mut GlyphCache) -> Canvas`

- [ ] **Step 1: 실패 테스트 먼저 — 기존 paint 테스트 갱신 + 텍스트 페인트 테스트 추가**

`src/paint.rs`의 `mod tests`를 아래로 교체:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::css::Color;

    fn font() -> crate::font::Font {
        crate::font::Font::from_bytes(std::fs::read("assets/fonts/Kestrel.ttf").unwrap()).unwrap()
    }

    fn canvas_for(html: &str, css: &str, w: f32, h: f32) -> Canvas {
        let root = crate::html::parse(html.to_string());
        let ss = crate::css::parse(css.to_string());
        let styled = crate::style::style_tree(&root, &ss);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = w;
        let f = font();
        let layout_root = crate::layout::layout_tree(&styled, viewport, &f);
        let mut cache = crate::raster::GlyphCache::new();
        paint(&layout_root, crate::layout::Rect { x: 0.0, y: 0.0, width: w, height: h }, &f, &mut cache)
    }

    #[test]
    fn fills_background_rect_red_over_white() {
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 2px; height: 2px; background-color: #ff0000; }",
            4.0,
            4.0,
        );
        assert_eq!(canvas.pixels[0], Color { r: 255, g: 0, b: 0, a: 255 });
        assert_eq!(canvas.pixels[3 * 4 + 3], Color { r: 255, g: 255, b: 255, a: 255 });
    }

    #[test]
    fn to_u32_packs_rgb() {
        let canvas = canvas_for(
            "<div></div>",
            "div { display: block; width: 1px; height: 1px; background-color: #ff0000; }",
            1.0,
            1.0,
        );
        let buf = canvas.to_u32_buffer();
        assert_eq!(buf[0], 0x00_ff_00_00);
    }

    #[test]
    fn text_paints_colored_pixels() {
        let canvas = canvas_for(
            "<p>Illi</p>",
            "p { display: block; font-size: 40px; color: #ff0000; }",
            200.0,
            80.0,
        );
        // 굵은 세로획 내부에 완전 커버리지 픽셀 → 정확한 텍스트 색
        assert!(
            canvas.pixels.iter().any(|p| *p == Color { r: 255, g: 0, b: 0, a: 255 }),
            "expected fully-covered red text pixel"
        );
    }
}
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test paint`
Expected: 컴파일 실패 — `paint`가 인자 4개를 받지 않음.

- [ ] **Step 3: 구현 — paint 재구성(재귀) + 글리프 블렌딩**

`src/paint.rs`의 구현부(테스트 모듈 위)를 아래로 교체:

```rust
use crate::css::{Color, Value};
use crate::font::Font;
use crate::layout::{GlyphInstance, LayoutBox, Rect};
use crate::raster::{CoverageBitmap, GlyphCache};

pub struct Canvas {
    pub pixels: Vec<Color>,
    pub width: usize,
    pub height: usize,
}

impl Canvas {
    fn new(width: usize, height: usize) -> Canvas {
        let white = Color { r: 255, g: 255, b: 255, a: 255 };
        Canvas { pixels: vec![white; width * height], width, height }
    }

    fn fill_rect(&mut self, color: Color, rect: Rect) {
        let x0 = rect.x.clamp(0.0, self.width as f32) as usize;
        let y0 = rect.y.clamp(0.0, self.height as f32) as usize;
        let x1 = (rect.x + rect.width).clamp(0.0, self.width as f32) as usize;
        let y1 = (rect.y + rect.height).clamp(0.0, self.height as f32) as usize;
        for y in y0..y1 {
            for x in x0..x1 {
                self.pixels[y * self.width + x] = color;
            }
        }
    }

    pub fn to_u32_buffer(&self) -> Vec<u32> {
        self.pixels
            .iter()
            .map(|c| (c.r as u32) << 16 | (c.g as u32) << 8 | (c.b as u32))
            .collect()
    }
}

fn get_color(layout_box: &LayoutBox, name: &str) -> Option<Color> {
    match layout_box.styled_node.value(name) {
        Some(Value::Color(c)) => Some(c),
        _ => None,
    }
}

fn blend(bg: Color, fg: Color, a: u8) -> Color {
    let af = a as f32 / 255.0;
    let mix = |b: u8, f: u8| (b as f32 * (1.0 - af) + f as f32 * af).round() as u8;
    Color { r: mix(bg.r, fg.r), g: mix(bg.g, fg.g), b: mix(bg.b, fg.b), a: 255 }
}

fn blit_glyph(canvas: &mut Canvas, bm: &CoverageBitmap, gi: &GlyphInstance) {
    let ox = (gi.x + bm.left as f32).round() as i32;
    let oy = (gi.baseline_y - bm.top as f32).round() as i32;
    for y in 0..bm.height {
        let cy = oy + y as i32;
        if cy < 0 || cy as usize >= canvas.height {
            continue;
        }
        for x in 0..bm.width {
            let a = bm.data[y * bm.width + x];
            if a == 0 {
                continue;
            }
            let cx = ox + x as i32;
            if cx < 0 || cx as usize >= canvas.width {
                continue;
            }
            let idx = cy as usize * canvas.width + cx as usize;
            canvas.pixels[idx] = blend(canvas.pixels[idx], gi.color, a);
        }
    }
}

fn paint_box(canvas: &mut Canvas, layout_box: &LayoutBox, font: &Font, cache: &mut GlyphCache) {
    if let Some(color) = get_color(layout_box, "background-color") {
        canvas.fill_rect(color, layout_box.dimensions.border_box());
    }
    for gi in &layout_box.glyphs {
        let bm = cache.get(font, gi.glyph_id, gi.px);
        blit_glyph(canvas, bm, gi);
    }
    for child in &layout_box.children {
        paint_box(canvas, child, font, cache);
    }
}

pub fn paint(layout_root: &LayoutBox, bounds: Rect, font: &Font, cache: &mut GlyphCache) -> Canvas {
    let mut canvas = Canvas::new(bounds.width as usize, bounds.height as usize);
    paint_box(&mut canvas, layout_root, font, cache);
    canvas
}
```

> 주의: `cache.get(...)`가 반환한 `bm`을 즉시 `blit_glyph`에 넘긴다. `bm`은 `cache`를 불변 차용하지만 `canvas`는 별개 객체라 충돌 없음.

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test paint`
Expected: 3개 PASS.

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/paint.rs
git commit -m "$(printf 'feat(paint): 글리프 커버리지 알파 블렌딩 + 폰트/캐시 전달\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 3: `main.rs` 연결 + 텍스트 예제 + 통합 검증

**Files:**
- Modify: `src/main.rs`
- Modify: `examples/test.html`, `examples/test.css`

**Interfaces:**
- Consumes: 위 모든 것
- Produces: 실제 텍스트 렌더링 파이프라인

- [ ] **Step 1: 예제에 텍스트 추가**

`examples/test.html`:

```html
<html>
    <body>
        <div class="outer">
            <p class="title">Kestrel</p>
            <p class="body">A small fast browser engine, built from scratch in Rust. This text is laid out and rendered by our own font engine.</p>
        </div>
    </body>
</html>
```

`examples/test.css`:

```css
html { display: block; }
body { display: block; padding: 20px; background-color: #101014; }
.outer { display: block; width: 460px; padding: 24px; background-color: #1e2a44; }
.title { display: block; font-size: 40px; color: #f4842c; }
.body { display: block; font-size: 20px; color: #d8dee9; }
```

- [ ] **Step 2: `main.rs` 파이프라인에 폰트 연결**

`src/main.rs`의 `main()`에서 레이아웃/페인트 호출부를 폰트/캐시를 넘기도록 수정. 기존:

```rust
    let layout_root = layout::layout_tree(&style_root, viewport);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect { x: 0.0, y: 0.0, width: viewport_width as f32, height: viewport_height as f32 },
    );
```

을 아래로 교체:

```rust
    let font_bytes = fs::read("assets/fonts/Kestrel.ttf").expect("read font");
    let font = font::Font::from_bytes(font_bytes).expect("parse font");
    let mut cache = raster::GlyphCache::new();

    let layout_root = layout::layout_tree(&style_root, viewport, &font);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect { x: 0.0, y: 0.0, width: viewport_width as f32, height: viewport_height as f32 },
        &font,
        &mut cache,
    );
```

- [ ] **Step 3: 전체 빌드 + 테스트**

Run: `source ~/.cargo/env && cargo build`
Expected: 성공.

Run: `source ~/.cargo/env && cargo test`
Expected: 모든 테스트 PASS (M1 + font + raster + 신규 텍스트 테스트).

- [ ] **Step 4: 헤드리스 렌더 눈 검증**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
KESTREL_RENDER_TO=target/page.ppm cargo run
sips -s format png target/page.ppm --out target/page.png
```
Expected: `target/page.png`를 열면 진파랑 박스 안에 주황색 큰 제목 "Kestrel"과, 그 아래 밝은 회색 본문이 콘텐츠 폭에서 **여러 줄로 워드랩**되어 안티앨리어싱된 글자로 보인다.

- [ ] **Step 5: 창 확인(선택) + 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
# cargo run   # 창에 동일 결과
git add src/main.rs examples/test.html examples/test.css
git commit -m "$(printf 'feat: 텍스트 렌더링 파이프라인 통합 + 예제 (M2b 완성)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Self-Review

**1. Spec coverage:** 인라인 워드랩 레이아웃 → Task 1. 글리프 알파 블렌딩 → Task 2. 폰트/캐시 전달(시그니처 변경) → Task 1+2. main 연결 + 텍스트 예제 → Task 3. font-size/color/줄간격 → Task 1 `layout_text`. 검증(단위 + 헤드리스) → 각 태스크. 스펙 항목 전부 커버.

**2. Placeholder scan:** 없음. 모든 코드 완전. 기존 M1 테스트의 시그니처 변경도 새 테스트 본문을 통째로 제시(부분 수정 모호성 제거).

**3. Type consistency:**
- `GlyphInstance{glyph_id:u16,x:f32,baseline_y:f32,px:f32,color:Color}` — Task 1 정의, Task 2 `blit_glyph`에서 동일 필드 사용. ✓
- `layout_tree(.., font:&Font)`, `LayoutBox.glyphs` — Task 1 정의, Task 2/3 사용. ✓
- `paint(.., font:&Font, cache:&mut GlyphCache)` — Task 2 정의, Task 3 호출. ✓
- `GlyphCache::get(font,gid,px)->&CoverageBitmap`, `CoverageBitmap{left,top:i32,...}` — M2a 정의와 일치. ✓
- `Color` Copy/PartialEq — css.rs에서 derive됨(블렌딩/비교 사용). ✓

불일치 없음.
