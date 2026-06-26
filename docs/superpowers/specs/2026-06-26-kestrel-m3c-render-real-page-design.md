# Kestrel M3c — 실제 페이지 렌더 통합: 설계 문서

- 날짜: 2026-06-26
- 상태: 승인됨 (브레인스토밍 완료)
- 범위: M3c. M3 마무리.

## 1. 맥락

M3a(fetch) + M3b(관용적 HTML 파싱)까지 됐다. M3c는 **가져온 페이지를 실제로 창에 그린다**: `kestrel <url>` → fetch → parse → CSS 추출 → style → layout → paint → 창.

남은 장애물 둘:
1. **CSS 파서(css.rs)도 토이라 실제 CSS에 패닉**한다(`@media`, `rgb()`, `10em`, `#fff`, 복합 셀렉터 등). HTML 때처럼 관용적으로 고쳐야 한다.
2. 실제 페이지는 `display:block`을 명시하지 않으므로, **UA 기본 스타일시트**가 없으면 모든 요소가 inline 취급되어 화면이 빈다.

## 2. 목표 (한 줄)

`kestrel <url>` 로 단순한 실제 페이지(example.com 등)를 직접 만든 브라우저 창에 렌더링한다.

## 3. 범위 / 비범위

**범위**: CSS 파서 견고화(패닉 금지), UA 기본 스타일시트, 인라인 `<style>` CSS 추출, fetch→render 통합, 헤드리스 PPM 검증.

**비범위**: 외부 `<link rel=stylesheet>` 가져오기, 이미지, JS, flexbox/grid/position/float, `<link>` 아이콘, 복합 셀렉터(자손/의사클래스/속성) 매칭. (후속) → 따라서 naver 같은 무거운 사이트는 여전히 깨짐(예상된 동작).

## 4. 컴포넌트

### 4.1 CSS 파서 견고화 (`css.rs` 재작성, 시그니처 불변)
- `parse(String) -> Stylesheet` 유지. assert/패닉 제거.
- **at-rule 스킵**: `@`로 시작하면 `;`(블록 없음) 또는 균형 잡힌 `{...}` 블록까지 건너뜀.
- **지원 못 하는 셀렉터의 규칙 스킵**: 단순 셀렉터(tag/id/class/`*`)와 `,`/`{`만 허용. 자손 결합자·의사클래스(`:`)·속성(`[`) 등을 만나면 그 규칙 전체를 `}`까지 건너뜀.
- **선언 단위 복구**: 선언은 `name: value;`. 값 텍스트를 `;`/`}`까지 모아 해석 — `px` 길이, 6자리 `#rrggbb` 색, 단순 키워드만 인정. `rgb()`/`em`/`%`/`#fff`/다중값 등은 그 선언만 스킵.
- 어떤 입력에도 패닉하지 않고 "이해 가능한 규칙만" 담은 Stylesheet 반환.

### 4.2 UA 기본 스타일시트
- 내장 CSS 문자열: 블록 레벨 요소를 기본 `display: block`으로.
  - 예: `html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, section, article, header, footer, nav, main, aside, blockquote, pre, table, form, figure { display: block; }`
- 페이지 CSS와 **합쳐서**(UA 규칙 먼저 → 페이지가 캐스케이드 순서로 우선) 하나의 Stylesheet로.
- `pub fn user_agent_stylesheet() -> Stylesheet` (css.rs).

### 4.3 인라인 CSS 추출
- `extract_css(node: &Node) -> String`: DOM을 순회해 모든 `<style>` 요소의 텍스트 자식을 이어붙임(M3b가 raw로 보존).

### 4.4 통합 (`main.rs`)
- `kestrel <url>` (플래그 아닌 위치 인자, `://` 포함 시 URL로 인식):
  - `http::fetch(url)` → body를 String으로 → `html::parse` → `extract_css` → UA + 페이지 CSS 규칙 합쳐 Stylesheet → `style_tree` → `layout_tree` → `paint` → 창.
  - `KESTREL_RENDER_TO` 설정 시 창 대신 PPM(헤드리스 검증). 뷰포트 폭 넓게(예: 1000).

## 5. 데이터 흐름

```
url → fetch → body(String) → html::parse → DOM
                                   │           └→ extract_css → 페이지 CSS
                                   │                              │
                          UA Stylesheet ── 합침 ──→ Stylesheet ──┘
                                   ▼
                       style_tree → layout_tree → paint → 창/PPM
```

## 6. 에러 처리

- fetch 실패 → 메시지 출력 후 종료(창 안 띄움).
- CSS/HTML 파싱은 패닉 없이 부분 결과.
- 본문이 UTF-8 아니어도 `from_utf8_lossy`.

## 7. 테스트 / 검증

- **헤르메틱 단위 테스트(css.rs)**:
  - `@media screen { p { color: #ff0000; } }` 같은 at-rule을 건너뛰고 패닉 안 함.
  - 복합 셀렉터(`.a .b { ... }`, `a:hover { ... }`) 규칙은 스킵, 단순 규칙은 유지.
  - `rgb()`/`10em`/`#fff` 값 선언은 스킵, `px`/`#rrggbb`/키워드는 유지.
  - `user_agent_stylesheet()`가 `div`에 display:block을 부여.
- **통합 검증(네트워크)**: `KESTREL_RENDER_TO=page.ppm kestrel https://example.com` → PPM에 example.com이 블록 레이아웃 + 텍스트로 렌더(눈 검증). 패닉 없음.

## 8. 완료 기준 (M3c Definition of Done)

1. CSS 파서가 실제 CSS(at-rule/복합 셀렉터/미지원 값 포함)에 패닉하지 않음(단위 테스트).
2. `kestrel <url>`로 example.com을 가져와 창/PPM에 텍스트가 있는 페이지로 렌더.
3. 기존 모든 테스트 통과.

## 9. 결정 기록

| 질문 | 결정 |
|------|------|
| CSS 범위 | 인라인 `<style>`만 (외부 `<link>`는 후속) |
| UA 스타일시트 | 내장, 블록 요소 display:block. 페이지 CSS와 합침(UA 먼저) |
| CSS 견고화 | at-rule·복합셀렉터·미지원값 스킵, 패닉 금지 |
| 복합 셀렉터 매칭 | 미지원(스킵). 단순 tag/id/class만 |
| 통합 인자 | `kestrel <url>` 위치 인자, KESTREL_RENDER_TO로 헤드리스 |
