# Kestrel 웹 표준 커버리지 · 로드맵

Kestrel 은 Rust 로 처음부터 만드는 **완전한 웹 브라우저**를 목표로 한다. 이 문서는
웹 표준(HTML/CSS/ECMAScript/DOM/…) 중 무엇을 구현했고 무엇이 남았는지 정직하게
기록해, 남은 작업을 **순서 있는 마일스톤**으로 지어 나가기 위한 것이다.

이건 실현 가능한 엔지니어링 프로젝트다. Ladybird(from-scratch, 실사이트 렌더),
Servo 가 같은 길을 실제로 걷고 있다. 방대하지만 유한하고, 순서가 있다.

기준 스펙:
- HTML: https://html.spec.whatwg.org/ (특히 §13 파싱, §15 Rendering=UA 스타일시트)
- CSS: https://drafts.csswg.org/ (모듈별), 검증은 https://github.com/web-platform-tests/wpt
- ECMAScript: https://tc39.es/ecma262/
- DOM: https://dom.spec.whatwg.org/

---

## 1. 구현됨 (대략)

- **HTML 파싱**: 관용적 토크나이저, void 요소, 속성, script/style raw, 트리 구성(근사).
- **CSS 선택자**: 태그/id/class/자손( ) 결합자/속성([a], [a=v]), 특이도, 캐스케이드.
- **CSS 값**: px/em/rem/%, #hex/rgb/rgba/이름색, url(), 키워드. 다수 단축값.
- **@media**: min-width/max-width/print.
- **레이아웃**: 블록 흐름, 인라인/텍스트 줄바꿈, inline-block(가로 흐름), float(좌우),
  position relative/absolute/fixed(부분), flexbox(방향/wrap/justify/align/grow),
  CSS Grid(px/fr/repeat/auto-fill/gap), 테이블(태그 기반 근사 + 셀 width 존중).
- **박스 시각**: 배경색, border(단축/변별/다색), border-radius(AA), box-shadow(SDF),
  이미지(png/jpeg), 리스트 마커(list-style-type).
- **폼 컨트롤**: UA 스타일시트 기반(캐스케이드로 저작자 CSS 가 덮음). input 값 텍스트.
- **폰트**: 자체 TrueType/CFF 렌더링, 한글+라틴, 글리프 캐시.
- **JS**: 자체 인터프리터(언어 서브셋) + 일부 DOM 핸들(querySelector 등), 타이머 일부.
- **네트워크**: HTTP fetch, 외부 CSS/이미지 로드, inflate(gzip).

---

## 2. 미구현 (완벽 렌더링에 필요한 것)

### HTML
- 표준 §13 파싱 알고리즘 전체: 삽입 모드, 오류 복구(foster parenting, 암묵 태그,
  active formatting 재구성), quirks 모드.
- 문자 참조(엔티티) 디코딩: `&amp; &lt; &#x...`.
- `<template>`, `<svg>`/`<math>` 통합, form 연결 규칙, `<noscript>` 처리 세부.

### CSS — 선택자
- 자식 `>`, 형제 `+`/`~`.
- 의사 클래스: `:hover :focus :active :first-child :last-child :nth-child()
  :not() :checked :disabled :root` 등.
- 의사 요소: `::before ::after ::marker ::placeholder ::first-line`.
- 속성 연산자 `^= $= *= ~= |=`, 대소문자 플래그.

### CSS — 값·캐스케이드
- `calc()`, 커스텀 속성 `var()`, `min()/max()/clamp()`.
- 단위: `vw/vh/vmin/vmax ch ex pt`.
- `!important`, `@import`, `@supports`, `@font-face`, `@keyframes`, cascade layers.
- **인라인 `style="..."` 속성** (지금 미반영 — 매우 흔함).
- 상속 확대: 현재 color/font-size/text-align 만. font-family/weight/style/
  line-height/letter-spacing/white-space/visibility/list-style/direction 등 다수 미상속.

### CSS — 레이아웃
- **진짜 테이블 레이아웃**: auto/fixed 알고리즘, 열 폭 계산, colspan/rowspan,
  border-collapse. (지금은 태그 기반 근사 + 균등/지정폭)
- **position: sticky**, 절대 위치 컨테이닝 블록 체인 정확화, **z-index/스태킹 컨텍스트**.
- **인라인 포매팅 정식화**: 라인 박스, vertical-align, 베이스라인 정렬,
  text + inline-block 같은 줄(현재는 분리됨), float 주위 텍스트 흐름, `clear`.
- flexbox 잔여: flex-shrink, flex-basis 정식, align-self/content, order, min/max.
- grid 잔여: template-rows/areas, 명시 배치(grid-row/column/span), auto-flow, subgrid.
- multi-column, `box-sizing: border-box`(현재 무시), min/max-width·height,
  `overflow`(스크롤/클리핑), aspect-ratio, object-fit.
- writing-mode(세로), **direction: rtl**(양방향 텍스트).

### CSS — 타이포그래피
- **font-family 매칭/폴백**, **@font-face 웹폰트**.
- **font-weight(볼드)**, **font-style(이탤릭)** 렌더 — 지금 굵기/기울기 없음.
- CSS `line-height`, `letter-spacing`, `word-spacing`, `text-transform`,
  `text-decoration`(현재 링크 밑줄만), `white-space`(pre/nowrap/pre-wrap),
  `word-break`/`overflow-wrap`, `text-overflow: ellipsis`, `text-indent`.
- **CJK/아랍/인도계 폰트 커버리지**(지금 두부 □) + **복합 텍스트 셰이핑**
  (리가처, 결합 문자, 아랍 접합, 인도계 재배열 — HarfBuzz 급).

### CSS — 페인트·효과
- **그라디언트**(linear/radial/conic), `opacity`, `hsl()`, currentColor 정식.
- 배경: position/repeat/size/다중 배경, background 단축.
- **transform**(translate/rotate/scale/matrix, 3D), **transition/animation**.
- filter, mix-blend-mode, clip-path, mask, border-image, outline.

### 대체·임베드 콘텐츠
- **SVG 렌더링**, `<canvas>`, `<video>/<audio>`, `<iframe>`, object/embed.
- 이미지 포맷: webp/avif/gif(애니), 순차 JPEG 외.
- 실제 폼 위젯: 체크박스/라디오/드롭다운 select/날짜 선택기, 제출/검증.

### JavaScript / DOM / Web API
- **ECMAScript 전체**: 클래스 완전, 모듈(import/export), Promise/async-await,
  제너레이터, 정규식 엔진, Proxy/Reflect, Symbol, BigInt, 이터레이터, Intl 등.
- **DOM 전체**: createElement/appendChild/removeChild, 속성 조작, **이벤트 모델**
  (addEventListener, 버블/캡처, 위임), MutationObserver.
- **CSSOM**: style 조작, getComputedStyle. JS DOM 변경 → **리플로우/리페인트** 반영.
- **fetch/XHR**(현 http 를 JS 에서 호출 불가), WebSocket.
- 타이머 전체, requestAnimationFrame, storage(localStorage/쿠키/IndexedDB),
  history/location/URL, JSON/Date/Math 완전.
- 이게 갖춰져야 React/Vue 같은 **프레임워크 기반 사이트(예: naver)** 렌더 가능.

### 네트워크·로딩
- HTTPS/TLS 세부, 리다이렉트/캐시/쿠키, HTTP/2, **CORS/동일 출처 정책**,
  charset 감지·디코딩, 지연 로딩.

### 렌더링·합성
- GPU 합성/레이어, **스크롤/뷰포트**, 히트테스트 정식, 서브픽셀 AA,
  감마 보정 블렌딩, 고DPI 정식.

### 접근성·보안
- ARIA/접근성 트리, CSP, 샌드박스, 믹스드 콘텐츠.

---

## 3. 만드는 순서 (마일스톤)

완전한 브라우저는 한 번에 완성되지 않고, 아래 순서로 지어 올라간다. 각 마일스톤은
그 자체로 유한하고 구체적이며, 매 단계 **실제 사이트로 검증**한다. 실사이트가 크게
두 부류라 로드맵도 그 순서를 따른다:

- **정적 문서 웹(SSR)** — 위키·블로그·뉴스·문서. 아래 A~C 로 상당수가 제대로 나온다.
- **동적 앱 웹** — 프레임워크 사이트(naver 등). D(JS/DOM/이벤트/fetch)가 갖춰지면 열린다.

둘 다 유한한 작업의 합이다. 순서대로 하나씩 완성한다.

---

## 4. 우선순위 로드맵 (파급 큰 순)

**A. 인라인·타이포그래피 정식화** — 거의 모든 페이지에 영향
1. `style="..."` 인라인 속성 파싱 (매우 흔함).
2. 상속 속성 확대(font-family/weight/line-height/white-space/direction…).
3. font-weight(볼드)·font-style(이탤릭) 렌더.
4. 라인 박스 정식화: text+inline-block 같은 줄, vertical-align, white-space.
5. CJK 등 폰트 폴백.

**B. 레이아웃 정확화** — 문서 웹 호환성
6. 진짜 테이블 레이아웃(열 폭/colspan/rowspan/border-collapse).
7. position: sticky, z-index/스태킹.
8. overflow(스크롤/클리핑), box-sizing.

**C. 시각 완성도** — 모던 사이트 외형
9. linear-gradient(그다음 radial).
10. transform/transition/animation.
11. 배경 position/repeat/size, opacity.

**D. 동적 웹(가장 무거운 단계)** — 앱/프레임워크 사이트
12. DOM API 확충 + 이벤트 모델.
13. JS DOM 변경 → 리플로우/리페인트.
14. fetch/XHR, 타이머/rAF, storage.
15. ECMAScript 커버리지 확대(Promise/async, 클래스, 정규식…).

**E. 대체 콘텐츠**
16. SVG, canvas, 웹폰트(@font-face), webp/gif.

각 항목은 유한하고 구체적이다. 한 번에 하나씩, 실제 사이트로 검증하며 지어 올라간다.
이게 완전한 브라우저로 가는 길이다.
