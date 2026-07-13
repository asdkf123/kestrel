# Kestrel 웹 표준 커버리지

Rust 로 처음부터 만드는 웹 브라우저. 이 문서는 웹 표준(HTML/CSS/ECMAScript/DOM)
중 무엇이 되고 무엇이 남았는지 **정직하게** 체크박스로 기록한다. 과장 없음 —
`[x]` 지원 범위 내 정상, `[~]` 부분(되는 것/안 되는 것 명시), `[ ]` 미구현.

기준 스펙: HTML(whatwg §13 파싱, §15 Rendering), CSS(drafts.csswg.org),
ECMAScript(tc39), DOM(dom.spec.whatwg.org). 검증: WPT + 실사이트 렌더.

**현재(2026-07): 651 단위 테스트 통과.** 정적 문서 웹은 상용 브라우저 근접 —
위키백과, HN, lobste.rs, MDN, go.dev, react.dev, Rust 블로그, Guardian, gov.uk,
old.reddit, Tailwind 정상 렌더. JS 무거운 SPA(GitHub 등)와 무거운 디자인시스템
(Stack Overflow)은 아직 부분.

---

## HTML / DOM

- [x] 관용적 HTML 파서 (void 요소, 속성, raw text, 주석)
- [x] 아레나 DOM (안정 NodeId, detach)
- [x] 문자 엔티티 디코딩 (`&amp; &lt; &#x...`)
- [x] 폼 컨트롤 UA 외형 + 값 텍스트
- [ ] 표준 §13 파싱 알고리즘 (삽입 모드, foster parenting, active formatting 재구성, quirks)
- [ ] `<template>`, `<svg>`/`<math>` 통합, `<noscript>` 세부

## CSS — 셀렉터

- [x] 타입 / id / class / 자손( ) / 자식(`>`) / 형제(`+` `~`)
- [x] 속성 `[a] [a=v]` + 연산자 `^= $= *= ~= |=` + `i/s` 플래그
- [x] 구조 의사클래스 `:nth-child/of-type` `:first/last-child` `:only-*` `:root` `:empty`
- [x] `:not() :is() :where()` — 명시도 정확 계산(:where=0)
- [x] `:checked :disabled :required` (폼 상태)
- [x] `::before ::after` + CSS 카운터
- [ ] `:hover :focus :active` (상호작용 필요), `:has()`
- [ ] `::marker ::placeholder ::first-line ::selection`

## CSS — 값 · 캐스케이드

- [x] 캐스케이드 · 특이도 · `!important` · 인라인 `style="..."`
- [x] 단위 전부: px/em/rem/% + vw/vh/vmin/vmax + pt/pc/in/cm/mm/Q + ch/ex
- [x] `calc()` (단위별 계수 보존 후 px 확정), `min() max() clamp()`
- [x] `var()` 커스텀 프로퍼티 + 폴백 (font-size 포함, em/rem 해석 전 치환)
- [x] 색: #hex(4/8자리 알파) / rgb / rgba / hsl / 이름색 / currentColor
- [x] `@media` (min/max-width, em/rem, print, 피처 평가, 미인식→불일치)
- [x] `@font-face` (ttf/otf), `@keyframes` (최종 상태 적용)
- [x] 단축: font / margin / padding / border / flex / grid / place-* / background(-position)
- [x] 논리 속성 (margin-inline, inline-size 등 → 물리 속성)
- [x] 상속 (color/font-*/line-height/letter-spacing/white-space/word-break/list-style/direction/visibility)
- [~] `@supports` — 되는 것: 피처 존재/논리 결합. 안 되는 것: 값 검증(과다 보고)
- [ ] `@import`, `@layer` (cascade layers), 컨테이너 쿼리, CSS 네스팅, woff2, 시간 기반 애니메이션/transition

## CSS — 레이아웃

- [x] 블록 흐름 + 세로 margin 상쇄 (형제 §8.3.1 + **부모-자식** hoisting)
- [x] 인라인 포매팅: text + inline-block 같은 줄, **인라인 박스 가로 margin/border/padding**(§10.3.1), 공백 접기, 줄바꿈/정렬(justify), text-indent
- [x] Flexbox: 방향/wrap/justify-content/align-items/align-self/**align-content**/grow/shrink/**min-content**(§4.5)/order/gap
- [x] Grid: **라인 배치·span·음수라인·auto-placement**(§8), **셀 자기정렬**(§11), **justify/align-content**(§10.1), **minmax(px,fr)**(§11.5), grid-auto-rows, template-areas, repeat/auto-fill
- [x] Float: 좌/우 패킹 + **text-wrap**(줄 상자 단축) + `clear` + **최근접 BFC 탈출**(§9.5) + **% 폭**(컨테이너 기준)
- [x] Table: 행/tbody/thead + colspan/rowspan + 공통 열 폭(내용 기반) + **border-spacing** + caption
- [x] position: relative / absolute / fixed / **sticky**, z-index / 스태킹 컨텍스트
- [x] overflow 클리핑, box-sizing: border-box, min/max-width·height, aspect-ratio, object-fit
- [x] 대체 요소: img / **인라인 SVG** / 폼컨트롤 (원자적 인라인 흐름, 미로드 공간 예약)
- [x] transform: translate / scale (레이아웃 후 시각 변환)
- [~] vertical-align — 되는 것: baseline/top/bottom/middle/sub/super 배치. 안 되는 것: 실측 폰트 메트릭(현재 폰트크기 배수 근사)
- [~] Flexbox 잔여 — 안 되는 것: align-content stretch(줄 확대), min/max 덮어씀 세부
- [~] Grid 잔여 — 안 되는 것: 명명 라인, subgrid
- [~] Table 잔여 — 안 되는 것: auto 폭 shrink-to-fit(min/max-content 정식 측정 필요), border-collapse 테두리 중첩, fixed 알고리즘
- [ ] transform rotate/matrix/3D, multi-column, writing-mode(세로쓰기)

## CSS — 타이포그래피

- [x] font-family 매칭/폴백, @font-face 웹폰트, faux 볼드/이탤릭 합성
- [x] line-height (normal/무단위 배수/%/px, half-leading 중앙)
- [x] letter-spacing, word-spacing, text-indent
- [x] text-decoration (밑줄/취소선/윗줄 + 색), text-transform
- [x] white-space (normal/nowrap/pre/pre-wrap/pre-line), word-break/overflow-wrap
- [x] text-overflow: ellipsis
- [x] 합성 글리프 (é/ñ 등 결합), 한글 + 라틴
- [x] bidi (UAX#9 기본 레벨/재정렬), direction: rtl
- [~] CJK — 되는 것: 한글. 안 되는 것: 일/중 대형 cmap fmt12 일부
- [ ] 복합 셰이핑 (리가처, 아랍 접합, 인도계 재배열 — HarfBuzz 급)

## CSS — 페인트 · 효과

- [x] 배경색 / 배경 이미지(size cover/contain, position)
- [x] border: solid/dashed/dotted + radius(AA), outline
- [x] 그라디언트: linear / radial(ellipse/circle, premultiplied 보간)
- [x] box-shadow (가우시안 erf 전이, inset), text-shadow
- [x] filter / backdrop blur (3패스 가우시안), grayscale/saturate(BT.709), opacity
- [x] 이미지: png / jpeg(baseline), 바이리니어 스케일, object-fit
- [x] 클리핑: overflow 사각/둥근, 글리프/폴리곤 픽셀 클립
- [x] SVG: rect/circle/ellipse/line/path/polygon, arc(A) 정확 평탄화, viewBox, fill/stroke
- [~] 그라디언트 conic, radial 크기/위치 정밀 — 근사
- [ ] mix-blend-mode, clip-path, mask, border-image, SVG 그라디언트/텍스트
- [ ] canvas 그라디언트/이미지/변환, `<video>/<audio>/<iframe>`
- [ ] 이미지 포맷: webp / avif / gif(애니) / 프로그레시브 JPEG

## JavaScript — 언어

- [x] var/let/const (반복별 바인딩, const 재대입 금지)
- [x] 함수 (선언/식/화살표/기본값 파라미터), 클래스 (extends/super/static/getter/setter/제너레이터·async 메서드)
- [x] 객체 리터럴 메서드 단축 — 일반/`*gen()`/`async fn()`/`async *s()`/계산 키 `[Symbol.iterator]()`
- [x] **`new.target`** (new 호출 판별 — "new 강제" 가드 패턴)
- [x] 제어문 (if/while/do-while/for/for-in/switch/try-catch-finally/throw), **라벨 break/continue**
- [x] 연산자 전부 (`**` `>>>` `??` `?.` 비트/논리/비교), 템플릿 리터럴
- [x] 스프레드 / 구조분해 (`[a,b]=arr`, `({x,y}=o)`)
- [x] async/await, Promise/마이크로태스크, 반복자 프로토콜(GetIterator/@@iterator) + for-of/스프레드/Array.from — 사용자 정의 이터러블(클래스·객체 `[Symbol.iterator]()`) 포함
- [x] 유니코드 식별자, ToPrimitive (valueOf/toString hint)
- [x] 프로토타입 링크 (`new F()` → `__proto__`, instanceof, 체인 조회)
- [~] 정규식 — 되는 것: 백트래킹 VM, test/exec/match/replace/split, **명명 그룹** `(?<n>)`. 안 되는 것: lookbehind, step-limit
- [x] **지연 제너레이터** — 재개가능 인터프리터(generator.rs). 무한 제너레이터/양방향 `next(v)`/`yield*` 위임/`return`값/`it.return`·`it.throw`/try 재개. 디슈가 패스로 식 내부 yield(`a+(yield b)`, `f(yield x)`, 삼항/단락평가)까지 완전 지원(평가순서·단락·this 보존)
- [x] **Symbol 타입** — 진짜 원시값(Value::Symbol). typeof 'symbol', 고유성/`Symbol.for` 레지스트리/`keyFor`, 잘 알려진 심볼(iterator 등), 계산 프로퍼티 키(멤버·객체·클래스 메서드), 비열거. (동적 런타임 계산 메서드 키만 후속)
- [ ] ES 모듈 그래프, BigInt, Intl, 태그드 템플릿, `Object.getOwnPropertySymbols` 실값

## JavaScript — 내장 객체

- [x] Object (defineProperty/접근자/keys/values/entries/fromEntries/assign/create), **삽입 순서 유지**(정수키 먼저)
- [x] Object **무결성**: freeze/seal/preventExtensions 실동작(대입 차단) + isFrozen/isSealed/isExtensible
- [x] Array (대부분 + from/of/at/flatMap/findLast/findLastIndex/fill/reduceRight)
- [x] String (**UTF-16 코드유닛** length/charAt/[i]/slice, at/localeCompare/pad*/repeat/matchAll)
- [x] Map/Set/WeakMap/WeakSet (SameValueZero, NaN 키)
- [x] Math, JSON (Date→ISO, 삽입순서, 내부마커 필터, **순환 참조 → TypeError**(신원 기반 탐지, 공유참조 DAG 는 정상)), **Number→문자열** (ECMAScript 7.1.12.1)
- [x] Date (now/생성자/get*/toISOString/parse/UTC)
- [x] Reflect, structuredClone, Function 생성자, Error 계열
- [ ] Proxy(트랩 세부), BigInt, Intl, WeakRef, Date 로컬 시간대

## DOM / Web API

- [x] 요소 생성/조작 (createElement/appendChild/insertBefore/removeChild/remove)
- [x] querySelector(All), getElementById/ByClassName/ByTagName
- [x] 속성 (get/set/remove/hasAttribute, className/id/value/dataset)
- [x] 트리 탐색 (children/parentNode/siblings), innerHTML/textContent, cloneNode
- [x] matches/closest/contains, DocumentFragment
- [x] **`element.style`** (CSSStyleDeclaration 라이브), **`classList`** (add/remove/toggle/contains + CSS 재매칭)
- [x] 이벤트 (addEventListener/버블/위임/dispatchEvent/Event/CustomEvent, target/preventDefault/stopPropagation)
- [x] 레이아웃 측정 (getBoundingClientRect/offset*/client*)
- [x] **getComputedStyle** — 실제 계산 스타일(스타일 엔진 브리지). 카멜·대시·getPropertyValue, width/height 는 used value(px). 리빌드 후 채워짐(초기 인라인 강제 리플로우는 후속)
- [x] **requestAnimationFrame** (setTimeout 별칭 → 헤드리스 settle 루프가 드레인), 타이머
- [x] XMLHttpRequest (open/send/onreadystatechange), fetch
- [x] DOMContentLoaded/load 발화 + readyState, body/head/documentElement
- [x] 전역: location, localStorage/sessionStorage, `top`/`parent`/`frames`/`self` = window, `window.Event`/CustomEvent 등
- [~] `history` — 객체/메서드 존재(pushState 등은 no-op, URL 미갱신)
- [ ] MutationObserver, 이벤트 캡처 단계, 초기 스크립트 시점 강제 동기 리플로우(측정 즉시 반영)
- [ ] 쿠키/IndexedDB, WebSocket

## 네트워킹 · 로딩

- [x] HTTP / HTTPS GET, gzip(inflate)
- [x] 외부 `<link>` CSS / 이미지 / `<script src>` 로드·실행
- [ ] 쿠키 / 리다이렉트 / 캐시, CORS/동일출처, HTTP/2, charset 감지, 지연 로딩

## 렌더링 · 합성

- [x] 소프트웨어 래스터라이저, 폴리곤/글리프 AA (서브스캔라인 + 부분 커버리지)
- [x] position:sticky 스크롤 고정, overflow 스크롤(헤드리스 오프셋)
- [x] 그라디언트 premultiplied 보간, 이미지 투명 가장자리 안전
- [ ] GPU 합성/레이어, 히트테스트 정식, 감마 보정 블렌딩, 고DPI 정식

---

## 남은 큰 덩어리 (파급 순)

1. **DOM 동적 갱신 잔여** — 초기 스크립트 시점 강제 동기 리플로우(측정 즉시 반영), MutationObserver — SPA 구동. (getComputedStyle/rAF/settle 루프는 완료)
2. **상호작용 의사클래스** `:hover/:focus/:active` + 이벤트 루프 — 동적 UI
3. **디자인시스템 캐스케이드 정밀화** — Stack Overflow(Stacks) 등 특정 사이트 line-height 붕괴
4. **정밀화**: vertical-align 실측, border-collapse 중첩, grid/flex align-content, @supports 값검증, 복합 셰이핑
5. **대체 콘텐츠**: video/audio/iframe, webp/gif, canvas 고급
6. **JS 잔여**: BigInt, Intl, 태그드 템플릿, ES 모듈 그래프

각 항목은 유한하고 구체적이다. 실제 사이트로 검증하며 하나씩 지어 올린다.
