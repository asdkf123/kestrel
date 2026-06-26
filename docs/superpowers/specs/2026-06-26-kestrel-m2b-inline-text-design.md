# Kestrel M2b — 인라인 텍스트 레이아웃 + 페인트 통합: 설계 문서

- 날짜: 2026-06-26
- 상태: 승인됨 (브레인스토밍 완료)
- 범위: M2b 만. (M2a에서 만든 `font`/`raster`를 소비)

## 1. 맥락

M2a에서 TrueType 폰트를 직접 파싱하고 글리프를 안티앨리어싱 커버리지 비트맵으로 래스터화하는 엔진(`font.rs`, `raster.rs`)을 완성했다. M2b는 그 글리프를 **실제 렌더 파이프라인에 통합**해, 텍스트가 든 HTML이 창에 진짜 글자로 그려지게 한다.

M1/M2a까지 `layout.rs`는 텍스트/인라인 노드를 건너뛰었다. M2b는 이를 확장한다(M2a의 "layout/paint 불변" 제약은 M2b에서 해제됨 — M2b가 바로 그 통합 단계).

## 2. 목표 (한 줄)

텍스트가 든 HTML(`<p>...</p>` 등)을 주면, 직접 만든 글리프가 콘텐츠 폭에서 워드랩되며 지정 색으로 창에 렌더링된다.

## 3. 범위 / 비범위

**범위**: 잎(leaf) 텍스트 블록(`<p>text</p>`, `<div>text</div>`)의 인라인 레이아웃(워드랩, 줄간격), 단일 폰트·단일 크기, `font-size`/`color`, 왼쪽 정렬, 공백 접기, 글리프 알파 블렌딩.

**비범위**: 혼합 인라인(한 박스에 블록+텍스트 동시), 인라인 요소(`<b>`,`<a>` 중첩) 정렬, 커닝, 양끝맞춤, bidi/RTL, 줄 내 수직정렬, 하이픈. (후속)

## 4. 핵심 결정

1. **잎 텍스트만 처리.** 블록 자식이 있는 박스는 M1대로, 텍스트 자식이 있는 박스는 인라인 레이아웃. 한 박스에 둘이 섞이면 M2b는 텍스트만(또는 블록만) 처리. YAGNI.
2. **폰트를 파이프라인에 전달.** `layout`은 텍스트 폭 측정을 위해, `paint`는 래스터화를 위해 `&Font`를 받는다(시그니처 변경).

## 5. 아키텍처 변경

```
main: Font 로드(1회) + GlyphCache 생성
   │
   ├─ layout_tree(style_root, viewport, &Font)
   │     블록 박스에 텍스트 자식이 있으면 인라인 레이아웃 →
   │     LayoutBox.glyphs: Vec<GlyphInstance> 채우고 content.height 설정
   │
   └─ paint(layout_root, bounds, &Font, &mut GlyphCache)
         배경(M1) 후 각 박스의 glyphs를 커버리지×색으로 알파 블렌딩
```

### 5.1 계약 타입 (변경/추가)

```
// layout.rs
pub struct GlyphInstance {
    pub glyph_id: u16,
    pub x: f32,         // 펜 원점 x (베이스라인 기준; paint 에서 + bitmap.left)
    pub baseline_y: f32,
    pub px: f32,        // font-size (px)
    pub color: crate::css::Color,
}
// LayoutBox 에 필드 추가: pub glyphs: Vec<GlyphInstance>
pub fn layout_tree<'a>(node: &'a StyledNode<'a>, containing_block: Dimensions, font: &Font) -> LayoutBox<'a>;

// paint.rs
pub fn paint(layout_root: &LayoutBox, bounds: Rect, font: &Font, cache: &mut GlyphCache) -> Canvas;
```

`layout()`/`layout_children()` 등 내부 메서드도 `font: &Font`를 전달받도록 변경.

## 6. 인라인 레이아웃 알고리즘 (`layout.rs`)

블록 박스 `layout()`에서 width/position 계산 후, 자신의 styled 노드 자식 중 **텍스트 노드**(`NodeType::Text`)를 모은다(StyledNode.node 사용 — 지금까지 미사용이던 필드를 여기서 씀). 텍스트가 있으면:

1. `font_size` = style `font-size`의 px값, 없으면 기본 16px.
2. `color` = style `color`(Value::Color), 없으면 검정(0,0,0).
3. `scale` = font_size / units_per_em. `line_height` = (ascent − descent + line_gap) × scale.
4. 첫 줄 베이스라인 = content.y + ascent × scale. 펜 x = content.x.
5. 공백으로 단어 분리(연속 공백 접기). 각 단어:
   - 폭 = Σ advance_width(glyph_index(c)) × scale.
   - 줄 시작이 아니고 pen_x + 폭 > content.x + content.width 이면 줄바꿈(pen_x = content.x, baseline += line_height).
   - 단어의 각 글자를 `GlyphInstance{glyph_id, x: pen_x, baseline_y, px: font_size, color}`로 추가하며 pen_x += advance.
   - 단어 뒤 공백 advance 추가.
6. content.height = 줄 수 × line_height (박스 높이에 반영).

## 7. 페인트 (`paint.rs`)

각 LayoutBox에서 배경 사각형(M1) 후, `glyphs`를 순회:
- `cache.get(font, gi.glyph_id, gi.px)` → CoverageBitmap.
- 캔버스에 (gi.x + bm.left, gi.baseline_y − bm.top) 위치로 블렌딩.
- 픽셀별 알파 a = coverage/255, 결과 = bg×(1−a) + color×a.
- 클리핑: 캔버스 범위 밖 픽셀은 건너뜀.

paint는 `&Font`와 `&mut GlyphCache`를 받아 모든 박스를 재귀 처리.

## 8. CSS

기존 파서 재사용(변경 없음). `font-size`(길이)·`color`는 이미 파싱됨. 기본값: font-size 16px, color 검정.

## 9. 에러 처리

- 글리프 인덱스 0(.notdef)도 그대로 래스터화(누락 글자 표시). 패닉 없음.
- 콘텐츠 폭이 0이거나 음수면 워드랩 없이 한 줄로(클리핑은 paint가 처리).

## 10. 테스트 / 검증

- `layout.rs` 단위 테스트:
  - 텍스트가 있는 박스가 glyphs를 채우고, content.height > 0.
  - 좁은 콘텐츠 폭에서 긴 텍스트가 2줄 이상으로 워드랩(마지막 글리프 baseline_y > 첫 글리프 baseline_y).
- `paint.rs` 단위 테스트:
  - 텍스트 색 픽셀이 캔버스에 실제로 칠해짐(텍스트 영역에 잉크 존재).
- 통합: `<p>` 문단 예제를 헤드리스 렌더(PPM) → 글자가 박스 안에서 워드랩되며 보임. 이어서 `cargo run` 창.

## 11. 완료 기준 (M2b Definition of Done)

1. 텍스트가 든 예제 HTML/CSS를 렌더하면 창(및 PPM)에 안티앨리어싱된 글자가 나타난다.
2. 콘텐츠 폭에서 워드랩되고 줄간격이 적용된다.
3. `font-size`와 `color`가 반영된다.
4. 위 단위 테스트 + M1/M2a 기존 테스트가 모두 통과.

## 12. 결정 기록

| 질문 | 결정 |
|------|------|
| 인라인 통합 범위 | 잎 텍스트 블록만 (혼합 미지원) |
| 폰트 전달 | layout/paint 시그니처에 `&Font` 추가, paint에 `&mut GlyphCache` |
| 글리프 저장 | LayoutBox.glyphs: Vec<GlyphInstance>(gid,x,baseline_y,px,color) |
| 줄간격 | 폰트 메트릭 (ascent−descent+line_gap)×scale |
| 기본값 | font-size 16px, color 검정 |
