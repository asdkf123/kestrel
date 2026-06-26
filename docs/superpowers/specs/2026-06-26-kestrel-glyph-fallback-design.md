# Kestrel — 글리프 폴백 체인: 설계 문서

- 날짜: 2026-06-26
- 상태: 승인됨 (실용적 from-scratch 폰트 경로 1단계)

## 1. 맥락 / 문제

지금은 폰트 1개만 쓴다. 그 폰트에 없는 글자는 .notdef(□)로 그려진다. 실제 브라우저는 **폰트 폴백**으로 없는 글자를 다른 폰트에서 가져온다. 이 단계에서 폴백 체인을 직접 구현한다(번들 1개 → 우선순위 목록). 이후 단계(CFF, 시스템 폰트)의 토대.

## 2. 목표

여러 폰트를 우선순위 목록으로 두고, **글자마다 그 글자를 가진 첫 폰트**를 선택해 렌더한다. □를 줄인다.

## 3. 설계

- `FontStack { fonts: Vec<Font> }` (font.rs)
  - `glyph_for(c) -> (font_index, glyph_id)`: 각 폰트의 `glyph_index`를 순서대로 시도, 0이 아닌 첫 결과. 없으면 `(0, 0)`(주 폰트 .notdef).
  - `primary() -> &Font`(메트릭용), `font(i) -> &Font`.
- 메트릭(줄간격/베이스라인/공백 advance)은 **주 폰트** 기준. 각 글자 advance는 **그 글자의 폰트** upm으로 스케일.
- `GlyphInstance`에 `font_index: usize` 추가 → 페인트가 올바른 폰트로 래스터화.
- `GlyphCache` 키에 `font_index` 포함. `get(stack, font_index, gid, px)`.
- `layout_tree`/`paint` 시그니처: `&Font` → `&FontStack`.
- 번들: `assets/fonts/Latin.ttf`(Hack, 라틴 주) + `assets/fonts/Kestrel.ttf`(Noto Sans KR, 폴백). FontStack = [Latin, Noto].

## 4. 범위 / 비범위

**범위**: 글자 단위 폴백, 폰트별 메트릭 스케일, 캐시 키에 폰트 인덱스.

**비범위**: 폰트 매칭(font-family/weight), 시스템 폰트 탐색, CFF, 셰이핑. (후속 단계)

## 5. 테스트 / 검증

- 단위: FontStack `glyph_for('A')` → 라틴(인덱스 0), `glyph_for('한')` → Noto(인덱스 1, 글리프≠0). 라틴 폰트엔 한글 없음 확인.
- 통합: "Hello 한글 World"를 렌더 → 라틴은 Hack, 한글은 Noto 폴백으로 나오고 □ 없음(헤드리스).

## 6. 완료 기준

1. 폴백 체인으로 라틴+한글이 각각 다른 폰트에서 렌더된다.
2. 기존 모든 테스트가 FontStack로 갱신되어 통과.
