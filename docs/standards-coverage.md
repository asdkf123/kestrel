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

## 1. 구현 상태 (된다 / 부분)

"대략" 같은 표현은 안 쓴다. 되는 건 되는 것, 부분이면 **되는 것과 안 되는 것을 명시**한다.
(각 기능이 지원하는 범위는 여기, 그 밖의 미구현은 §2.)

### 1a. 완전히 되는 것 (지원 범위 내에서 정상 동작)

- CSS 캐스케이드·특이도, **인라인 `style="..."`**, `@media`(min/max-width/print).
- 선택자: 태그·id·class·자손( )·속성(`[a]`,`[a=v]`).
- 값: px/em/rem/%, #hex/rgb/rgba/이름색, url(), 키워드, 다수 단축값.
- 레이아웃: 블록 흐름, inline-block(가로 흐름+줄바꿈), flexbox(방향/wrap/
  justify/align/grow), CSS Grid(px/fr/repeat/auto-fill/gap).
- 시각: 배경색, border(단축/변별/다색), border-radius(AA), box-shadow(SDF),
  리스트 마커(list-style-type), **z-index 스택 순서**, **box-sizing: border-box**,
  **overflow 클리핑**, **position: sticky**(스크롤 고정).
- 텍스트: **white-space**(nowrap/pre 계열).
- 테이블: **공통 열 폭 + 내용 기반 사이징** (열이 행마다 정렬).
- 이미지: png/jpeg(baseline). 폼 컨트롤 UA 기본 외형(캐스케이드로 저작자 CSS 우선).
- 폰트: 자체 TrueType/CFF 렌더 + 글리프 캐시 (라틴+한글), **볼드/이탤릭(faux 합성)**.
- **상속**: 표준 상속 속성 다수 (color/font-*/line-height/white-space/list-style 등).

### 1b. 부분만 되는 것 (→ 되는 것 / ✗ 안 되는 것)

- **HTML 파싱**: → 흔한 HTML 트리화, void/속성/raw. ✗ 표준 §13 파싱 알고리즘,
  문자 엔티티(`&amp;`), 오류 복구/quirks.
- **position**: → relative(offset 이동). ✗ absolute/fixed 는 top/left 배치만 되고
  containing-block 체인·z-index/스태킹 컨텍스트 안 됨.
- **float**: → left/right 배치. ✗ `clear`, 텍스트가 float 주위로 흐르기.
- **테이블**: → 행 배치 + 셀 지정폭(width). ✗ auto 열 폭 계산, colspan/rowspan,
  border-collapse.
- **인라인 포매팅**: → 텍스트 줄바꿈·정렬. ✗ text 와 inline-block 이 같은 줄(분리됨),
  vertical-align, 베이스라인 정렬.
- **상속**: → 표준 상속 속성 다수. ✗ em/rem 값은 아직 드롭(미해석), 일부 속성 미적용.
- **폼 컨트롤**: → UA 외형 + 값 텍스트. ✗ 실제 위젯(체크박스/라디오/드롭다운),
  입력/포커스/제출.
- **폰트 커버리지**: → 라틴+한글 + 볼드/이탤릭(faux). ✗ CJK(일/중)·아랍(두부 □),
  전용 볼드/이탤릭 폰트(현재는 합성).
- **JS**: 상세 현황은 아래 **§1c** 참조 (언어 코어 대부분 + 내장 상당수 + DOM 일부).
- **네트워크**: → HTTP GET + gzip(inflate), 외부 CSS/이미지/스크립트 로드. ✗ HTTPS 세부,
  쿠키/리다이렉트/캐시, CORS.

### 1c. JavaScript / DOM 상세 현황 (2026-07 기준, 코드 확인)

**JS 언어 (문법) — 체감 85~90%**
- 된다: var/let/const, 함수(선언/식/화살표/기본값 파라미터), 클래스(extends/super/
  static), if/while/**do-while**/for/for-in/switch/try-catch-finally/throw, 모든 연산자
  (`**`/`>>>=`/`??`/`?.`/비트/논리), 템플릿 리터럴, 스프레드/구조분해(부분),
  **async/await**(간이), **반복자 프로토콜(Symbol.iterator)**.
- 남음: **제너레이터 `function*`/`yield`**, **`for...of` 구문**(반복자 프로토콜은 있음),
  **정규식 매칭 엔진**(리터럴만 파싱), 객체 게터/세터 `{get x(){}}`, 태그드 템플릿,
  라벨 문 실제 의미, 구조분해 기본값.

**내장 객체/메서드 — 체감 60~70%**
- 된다: **함수-객체**(prototype/정적/call/apply/bind), **Object**(defineProperty·접근자/
  keys/entries/assign/create/freeze/prototype), **Array**(대부분 + Array.prototype),
  **Map/Set/WeakMap/WeakSet**, **Function 생성자**, Math, JSON, **Promise(간이)**,
  **Reflect**, **Symbol(경량)**, **Error 계열**, 문자열 11종, **webpack 청크 런타임(사이트 자체)**.
- 남음: **`Date` 전무**, **정규식 엔진**, **`Array.sort`**/flat/at/fill, 문자열 padStart/
  repeat/matchAll, 숫자 toFixed/Number.isInteger, **Proxy**, Promise.all/race,
  structuredClone/queueMicrotask.

**DOM API — 체감 30~40% (가장 큰 남은 덩어리)**
- 된다: getElementById/createElement/**createTextNode**, querySelector(All),
  appendChild/**insertBefore**/removeChild/remove, setAttribute/getAttribute/
  removeAttribute/hasAttribute, children/parentNode/siblings, innerHTML/textContent,
  className/id/value, addEventListener(요소+문서/window), **body/head/documentElement**,
  **DOMContentLoaded/load 발화 + readyState**.
- 남음(프레임워크 즉효 순): **`element.style`**(CSSStyleDeclaration), **`classList`**,
  **레이아웃 측정**(getBoundingClientRect/offset*/client*), **이벤트 객체 모델**
  (event.target/preventDefault/stopPropagation/dispatchEvent), **`XMLHttpRequest`**,
  getElementsByClassName/TagName, cloneNode/closest/matches/contains, DocumentFragment,
  dataset, nodeType.

**요약**: 가벼운 페이지는 대부분 렌더됨. 프레임워크 SPA(naver)는 **DOM API(style/
classList/이벤트/XHR)**가 최대 남은 덩어리 + JS 쪽 독립 큰 항목 **정규식 엔진**과 **Date**.

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
1. ~~`style="..."` 인라인 속성 파싱~~ ✓ 완료 (ac5ea9b).
2. ~~상속 속성 확대~~ ✓ 완료 (4dcef16).
3. ~~font-weight(볼드)·font-style(이탤릭) 렌더~~ ✓ 완료 (4dcef16, faux 합성).
4. 라인 박스 정식화: text+inline-block 같은 줄, vertical-align, white-space.
5. CJK 등 폰트 폴백.

**B. 레이아웃 정확화** ✓ 완료
6. ~~진짜 테이블 레이아웃(공통 열 폭 + 내용 기반)~~ ✓ (7429c25). colspan/rowspan/border-collapse 는 후속.
7. ~~position: sticky~~ ✓ (073d346), ~~z-index/스태킹~~ ✓ (63ac308).
8. ~~overflow(hidden 클리핑)~~ ✓ (5109a73), ~~box-sizing~~ ✓ (fabec85). 스크롤바/스크롤 동작은 후속.

**C. 시각 완성도** — 모던 사이트 외형
9. linear-gradient(그다음 radial).
10. transform/transition/animation.
11. 배경 position/repeat/size, opacity.
- ~~`background:` 단축(색/url)~~ ✓ — 여태 색이 안 나오던 큰 공백 메움.

**C-layout. 레이아웃 정확도** (실사이트 검증 기반)
- ~~float 다단: float 을 클리어하는 블록을 옆에 배치~~ ✓ (float 사이드바+본문).
- 남음: **float text-wrap**(이미지 주위 텍스트 흐름 — 줄 상자 단축), **grid-template-areas**
  (Wikipedia 3단 겹침 원인), text+inline-block 같은 줄, BFC(overflow) 옆 축소.
- 실측: HN(2178px/513링크)·Wikipedia(16660px/2864링크) JS 에러 0, 콘텐츠 완전.
  Wikipedia 3단 겹침만 grid-areas 미지원으로 잔존.

**D. 동적 웹(가장 무거운 단계)** — 앱/프레임워크 사이트

이번 라운드에 채운 것(전부 표준 플랫폼, 프레임워크 비종속):
- ~~함수-객체(prototype/call/apply/bind), Map/Set, defineProperty(접근자)~~ ✓
- ~~내장 프로토타입(Function/Array/Object.prototype), Reflect, 반복자~~ ✓
- ~~webpack 청크 런타임 → 배열을 표준 객체로 만들어 **사이트 자체 런타임**이 동작~~ ✓
- ~~외부 `<script src>` 실행, DOMContentLoaded/load 발화, document.body/head~~ ✓
- ~~async/await, Promise/마이크로태스크, fetch~~ ✓

**D 남은 것 — 파급 큰 순:**
12. ~~**`element.style` (CSSStyleDeclaration)**~~ ✓ (eaab20b) 라이브 프록시, 렌더 반영.
13. ~~**`classList`** (add/remove/toggle/contains)~~ ✓ (e8a37f3) 라이브, CSS 재매칭.
14. ~~**정규식 매칭 엔진**~~ ✓ (c49432d) 백트래킹 VM + test/exec/match/replace/split/search.
15. ~~**이벤트 객체 모델**~~ ✓ (d976fe8) target/currentTarget/preventDefault/
    stopPropagation + 버블링. (dispatchEvent/Event 생성자, 콜백 후 리플로우는 후속)
16. ~~**`XMLHttpRequest`**~~ ✓ (f3952bd) 동기 open/send/onreadystatechange/onload.
17. ~~**`Date`**~~ ✓ (a7ae04f) now/생성자/get*/toISOString/파싱(UTC).
18. **남은 빌트인/DOM**: String/Number/Boolean ✓ (a6df353). 레이아웃 측정
    (getBoundingClientRect/offset*/client*), 제너레이터/for-of, Proxy, Array.sort,
    dispatchEvent/CustomEvent, DocumentFragment 등.

**naver 잔여 블로커**: polyfill.js(core-js)가 내부 `e.call`(e=undefined)에서
크래시 → 모듈 미등록. 이건 넓은 플랫폼 이슈가 아니라 **미니파이 core-js의 특정
내부 지점**이라, 위 항목들과 별개로 정밀 추적이 필요.

**E. 대체 콘텐츠**
16. SVG, canvas, 웹폰트(@font-face), webp/gif.

각 항목은 유한하고 구체적이다. 한 번에 하나씩, 실제 사이트로 검증하며 지어 올라간다.
이게 완전한 브라우저로 가는 길이다.
