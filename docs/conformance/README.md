# Kestrel 표준 적합성 현황

측정일: 2026-07-31 (CSS 표는 2026-08 CSSOM 스윕분 부분 갱신 — css-nesting/conditional/cssom/mediaqueries/syntax/borders/selectors/values/cascade/properties-values-api/color 행은 최신, 종합(축별) CSS 총계는 미갱신이라 실제보다 과소). 러너: 헤드리스 렌더 + WPT testharness / test262. WPT 는 testharness 기반 서브테스트 기준(reftest·수동 테스트 제외, 하네스 못 돈 파일은 분모에서 빠짐). test262 는 실행분(module/onlyStrict 건너뜀) 기준. % = 통과/전체.

> ⚠️ 이 수치는 **명세 적합성**(파싱/API 정확도)이지 시각적 렌더 완성도가 아니다. 상용 브라우저 대비가 아니라 스펙 스위트 대비 진척도다.

## 종합 (축별)

| 축 | 통과 / 전체 | % |
|---|---|---|
| **JS** (test262, intl402 제외) | 36,470 / 48,605 | **75.0%** |
| **CSS** (WPT css) | 71,019 / 122,910 | **57.8%** |
| **DOM** (WPT dom) | 3,983 / 6,811 | **58.5%** |
| **HTML** (WPT html) | 64,058 / 85,181 | **75.2%** |
| JS intl402 (Intl 미구현) | 179 / 3,357 | 5.3% |

## JS (test262)

### 최상위
| 영역 | 통과 / 전체 | % |
|---|---|---|
| language | 19,842 / 22,340 | 88.8% |
| built-ins | 15,323 / 23,700 | 64.7% |
| annexB | 494 / 1,084 | 45.6% |
| staging | 811 / 1,481 | 54.8% |
| intl402 | 179 / 3,357 | 5.3% |

### built-ins 객체별 (통과율 내림차순)
| 객체 | 통과 / 전체 | % |
|---|---|---|
| Reflect | 148 / 153 | 96.7% |
| Date | 566 / 594 | 95.3% |
| Object | 3,238 / 3,402 | 95.2% |
| JSON | 157 / 165 | 95.2% |
| Math | 311 / 327 | 95.1% |
| String | 1,161 / 1,223 | 94.9% |
| Number | 320 / 340 | 94.1% |
| Boolean | 47 / 50 | 94.0% |
| Set | 354 / 382 | 92.7% |
| Array | 2,776 / 3,071 | 90.4% |
| WeakMap | 126 / 140 | 90.0% |
| WeakSet | 76 / 85 | 89.4% |
| Map | 177 / 202 | 87.6% |
| Function | 409 / 472 | 86.7% |
| RegExp | 1,545 / 1,878 | 82.3% |
| Proxy | 234 / 307 | 76.2% |
| DataView | 426 / 561 | 75.9% |
| TypedArray | 1,031 / 1,438 | 71.7% |
| Symbol | 60 / 96 | 62.5% |
| TypedArrayConstructors | 448 / 724 | 61.9% |
| Iterator | 397 / 654 | 60.7% |
| Promise | 439 / 726 | 60.5% |
| Error | 48 / 93 | 51.6% |
| ArrayBuffer | 80 / 221 | 36.2% |
| BigInt | 26 / 77 | 33.8% |
| Temporal | 0 / 4,603 | 0.0% |
| Atomics | 0 / 389 | 0.0% |

## CSS (WPT, 서브트리별, 통과율 내림차순)

> **★2026-08 CSSOM 스윕(조건부/중첩/컨테이너 그룹 규칙)** — 아래 표의 해당 행은 갱신됨,
> 나머지 행은 2026-07-31 측정(이후 갱신분 미반영, 실제 수치는 대개 더 높음):
> - **css-nesting 4.3→81.2%**: 중첩 규칙 다중 줄 직렬화, **지정값(specified) 보존**(cssText 는
>   계산값 아닌 지정값 — Declaration.raw), **라이브 addressing**(CssRule 에 중첩경로 np, insertRule/
>   deleteRule/selectorText/.style 이 중첩 규칙에 반영), 중첩 @media/@supports 매칭+selectorText
>   무효화 재desugar, selectorText 캐논화(암시적 & 명시), 선행 결합자 desugar, CSSNestedDeclarations.
> - **css-conditional 66.6→82.1%**: **최상위 @media/@supports/@container 를 CSSOM 컨테이너로 통일**
>   (파스시점 flatten 대신 CSSMediaRule/CSSSupportsRule/CSSContainerRule 보존, 매칭은 build_with
>   flatten 이 조건 평가), CSSRule 타입 상수, 인터페이스 상속 사슬, insertRule 검증(후행 garbage/
>   @import HierarchyRequestError), **container-type/name/container 파싱·검증**, **@container 쿼리
>   재귀 평가**(not/and/or·중첩·범위 `100px<width<200px`·`=`·calc·단위, size 특성 게이트, 무효 쿼리 드롭),
>   conditionText 캐논 직렬화.
> - **mediaqueries 7.8→88.3%, cssom 69.1→72.5%, selectors 69.7→72.1%, css-syntax 18.9→68.7%**(이전 세션 포함).
> - **회귀 함정(기록)**: @container 를 컨테이너화하면 has_containers()가 최상위만 스캔해 ContainerMap
>   미구축→매칭 붕괴(재귀+at_container 검사 필수). 컨테이너 쿼리 평가기는 미디어 평가기와 분리(mediaqueries
>   무위험). Value enum 크기 키우면 JS 재귀 가드 전 네이티브 스택 오버플로(np 는 Rc).
> - **되돌린 시도(회귀 실증)**: style() 스타일 쿼리는 @property 등록 타입 커스텀 프로퍼티의 타입 비교(범위·
>   calc·단위변환)를 요구 — 문자열 매칭 부분 구현은 fluke 통과분을 깨 net-negative라 되돌림. 전체 feature
>   의미론 필요.
>
> **값 파싱 검증 스윕(이전)**. 최근 작업:
> **calc 차원 타입 검사기**(`mdim_of`, [css-values-math.md](css-values-math.md)) 구현으로 길이/시간/수/각도 문맥의
> 타입 불일치(`max(0Hz)`/`calc(1/2px)`/축 불일치)를 원리적으로 거부. 값 검증기의 `contains("deg")` 편법 전부 제거.
> **검증 추가·강화된 프로퍼티**: content-visibility, scrollbar-width/color, caret-shape/input-security/overlay/
> caret-animation, accent-color, object-view-box, text-decoration(+thickness/offset), text-shadow, border-\*-width,
> margin-trim, flex-line-count, overflow-anchor, grid-auto-flow, grid-template-areas, interpolate-size,
> scroll-initial-target, transform(함수 타입 검사), opacity/font-weight(calc 타입), offset-path/ray(), shape() 구조,
> circle/ellipse radial-extent — 다수는 CSSOM 캐논 직렬화 포함.
>
> **중요(교차 게이트)**: 새 프로퍼티 검증 arm 을 추가하면 반드시 `is_known_property`(`src/css/supports.rs`)에도
> 등록할 것. 없으면 `CSS.supports(prop, val)` 가 false 를 반환하고 getComputedStyle 열거에서 빠져, "should
> not set unrelated longhands"(CSS.supports 단언) 등 **연쇄 테스트가 대량 막힌다**. animation-range 는 이
> 등록만으로 css-animations +86 이었다.
>
> **추가 완료(이후 세션)**: basic-shape 캐논 직렬화(normalize_shape 확장 — 아래), SVG path data 검증(A=7 등),
> clip-path shape(), gradient 스톱/from calc 타입, image-set 옵션/image(`<color>`) 검증, mask 단축(성분 개수),
> animation-range(검증+전개+캐논), offset 단축(전개)+offset-\* 롱핸드, corner-shape 조합(단일 코너·변·축·논리 —
> shape && radius{1,2} 문법, 전체 테스트로 확정), is_known_property 게이트에 검증분 전부 등록.
>
> **남은 큰 서브시스템**(각각 깊은 파서/정밀 스펙 필요):
> 1. color 상대색 `color(from … calc(r + 1%) …)` 채널 calc 타입(깊은 색 파서, r/g/b/x 채널=수 키워드)
> 2. calc 산술 단순화(`calc(100%/4)`→`calc(25%)`, `calc(100+100)`→`calc(200)` — 직렬화 시 상수 접기)
> 3. 미지 프로퍼티 거부: el.style 의 catch-all 이 미지 프로퍼티(`corners` 등)를 수용 — CSSOM 상 no-op 이어야.
>    완전한 프로퍼티 레지스트리 필요(현 SUPPORTED 는 불완전 → 그대로 쓰면 실제 프로퍼티 회귀).
> 4. 중첩 image-set/image(image-set(...)), url() request modifier(escaped-space url 회귀 주의)
> 5. basic-shape 나머지: calc 산술 단순화, coord-box 처리, text-decoration/animation-range 잔여 캐논
>
> **basic-shape 캐논 직렬화(해결됨)**: 지정값(el.style) 직렬화는 `normalize_shape`(serialize_decl 경유),
> 계산값은 `computed_value_string` 으로 경로가 다르다. 처음엔 별도 canon 을 serialize_decl 에 끼워 넣어
> 기존 `normalize_shape` 를 가려 -422 회귀했다 → **기존 normalize_shape 를 확장**하는 방식으로 정정:
> inset/rect/xywh 0→0px·round 전부-0 생략, polygon nonzero/round-0 생략, path SVG data 정규화(공백·콤마·
> z→Z), circle/ellipse at-위치 x 먼저, shape() 좌표 0→0px. 남은 것은 calc 산술 단순화(`calc(100%/4)`→
> `calc(25%)`)와 coord-box 처리.

| 서브트리 | 통과 / 전체 | % |
|---|---|---|
| css-device-adapt | 1 / 1 | 100.0% |
| css-will-change | 173 / 173 | 100.0% |
| css-align | 3,232 / 3,322 | 97.3% |
| css-forced-color-adjust | 13 / 14 | 92.9% |
| css-text-decor | 1,182 / 1,276 | 92.6% |
| CSS2 | 592 / 653 | 90.7% |
| css-images | 3,225 / 3,580 | 90.1% |
| mediaqueries | 272 / 308 | 88.3% |
| css-color-adjust | 137 / 157 | 87.3% |
| compositing | 144 / 167 | 86.2% |
| cssom | 2,942 / 3,437 | 85.6% |
| css-color | 7,930 / 9,399 | 84.4% |
| css-content | 176 / 211 | 83.4% |
| css-size-adjust | 170 / 207 | 82.1% |
| css-conditional | 2,136 / 2,602 | 82.1% |
| css-flexbox | 1,125 / 1,379 | 81.6% |
| css-nesting | 95 / 117 | 81.2% |
| css-backgrounds | 3,788 / 4,914 | 77.1% |
| css-anchor-position | 9,972 / 13,012 | 76.6% |
| css-position | 1,078 / 1,412 | 76.3% |
| css-viewport | 369 / 490 | 75.3% |
| css-display | 287 / 384 | 74.7% |
| css-break | 452 / 609 | 74.2% |
| css-ui | 1,390 / 1,888 | 73.6% |
| css-fonts | 5,571 / 7,565 | 73.6% |
| selectors | 3,011 / 4,118 | 73.1% |
| css-transforms | 2,886 / 3,969 | 72.7% |
| css-ruby | 52 / 73 | 71.2% |
| css-sizing | 2,237 / 3,212 | 69.6% |
| css-syntax | 294 / 428 | 68.7% |
| css-text | 2,078 / 3,029 | 68.6% |
| css-scroll-snap | 458 / 690 | 66.4% |
| css-properties-values-api | 611 / 923 | 66.2% |
| css-shapes | 3,336 / 5,093 | 65.5% |
| css-cascade | 458 / 717 | 63.9% |
| css-multicol | 850 / 1,356 | 62.7% |
| css-grid | 2,156 / 3,567 | 60.4% |
| css-env | 5 / 9 | 55.6% |
| css-box | 529 / 957 | 55.3% |
| css-animations | 574 / 1,059 | 54.2% |
| css-contain | 194 / 360 | 53.9% |
| css-variables | 272 / 511 | 53.2% |
| css-values | 4,113 / 7,764 | 53.0% |
| css-writing-modes | 188 / 356 | 52.8% |
| css-overscroll-behavior | 51 / 97 | 52.6% |
| css-page | 49 / 97 | 50.5% |
| css-easing | 77 / 156 | 49.4% |
| css-logical | 664 / 1,364 | 48.7% |
| css-counter-styles | 57 / 118 | 48.3% |
| css-scrollbars | 22 / 46 | 47.8% |
| css-rhythm | 72 / 155 | 46.5% |
| css-inline | 288 / 635 | 45.4% |
| css-lists | 362 / 852 | 42.5% |
| css-borders | 464 / 1,151 | 40.3% |
| css-overflow | 369 / 983 | 37.5% |
| css-tables | 191 / 557 | 34.3% |
| filter-effects | 833 / 2,452 | 34.0% |
| css-layout-api | 4 / 13 | 30.8% |
| css-transitions | 879 / 3,086 | 28.5% |
| fill-stroke | 104 / 371 | 28.0% |
| motion | 1,053 / 3,775 | 27.9% |
| css-link-params | 6 / 25 | 24.0% |
| css-masking | 831 / 4,312 | 19.3% |
| cssom-view | 300 / 1,827 | 16.4% |
| css-scroll-anchoring | 13 / 84 | 15.5% |
| css-pseudo | 77 / 532 | 14.5% |
| css-color-hdr | 13 / 116 | 11.2% |
| css-shadow | 37 / 347 | 10.7% |
| css-gaps | 333 / 3,382 | 9.8% |
| css-mixins | 46 / 492 | 9.3% |
| css-view-transitions | 54 / 1,208 | 4.5% |
| css-highlight-api | 1 / 41 | 2.4% |
| geometry | 14 / 672 | 2.1% |
| css-typed-om | 3 / 306 | 1.0% |
| css-exclusions | 0 / 4 | 0.0% |
| css-font-loading | 0 / 70 | 0.0% |
| css-forms | 0 / 65 | 0.0% |
| css-image-animation | 0 / 5 | 0.0% |
| css-navigation | 0 / 1 | 0.0% |
| css-paint-api | 0 / 1 | 0.0% |
| css-parser-api | 0 / 1 | 0.0% |
| fetching | 0 / 4 | 0.0% |

## DOM (WPT, 서브트리별)

| 서브트리 | 통과 / 전체 | % |
|---|---|---|
| **dom (전체)** | 3,983 / 6,811 | **58.5%** |
| abort | 0 / 2 | 0.0% |
| collections | 17 / 48 | 35.4% |
| events | 277 / 580 | 47.8% |
| lists | 168 / 189 | 88.9% |
| nodes | 3,381 / 5,748 | 58.8% |
| ranges | 17 / 68 | 25.0% |
| traversal | 36 / 53 | 67.9% |

## HTML (WPT)

| 영역 | 통과 / 전체 | % |
|---|---|---|
| html (전체) | 64,058 / 85,181 | 75.2% |

## WPT 전 영역 로드맵 (브라우저 완성 기준)

> kestrel 목표는 **완전한 브라우저**다. 위 css/dom/html/JS 는 현재 러너가 실제로 돌리는 영역이고,
> 아래는 WPT 전체(약 210만 서브테스트)를 구성하는 **나머지 영역** 전부다. 브라우저로서 지원해야 하지만
> 아직 kestrel 이 손대지 않았거나 러너 범위 밖이라 미측정인 것들. 상태는 정직하게 3단계로 표기한다.
>
> - ✅ **측정됨** — 러너가 실제 실행, 위 표에 수치 있음
> - ◐ **미측정** — kestrel 에 부분 인프라가 있을 수 있으나 러너 미연결(수치 없음, 0% 아님)
> - ✗ **미구현(0%)** — 해당 API/서브시스템 자체가 kestrel 에 없음 → 측정해도 0% 예상

### 렌더링 · 레이아웃

| 영역 | 상태 | 비고 |
|---|---|---|
| css/* (77개 서브트리) | ✅ 측정됨 57.8% | 위 CSS 표 |
| svg | ✗ 미구현(0%) | SVG 렌더 파이프라인 없음 |
| 2dcontext / html canvas | ✗ 미구현(0%) | Canvas 2D 컨텍스트 없음 |
| quirks | ◐ 미측정 | 쿼크 모드 일부 파싱만 |
| web-animations | ✗ 미구현(0%) | 애니메이션 실행 엔진 없음(interpolation 미구현) |

### DOM · HTML 코어 · 파싱

| 영역 | 상태 | 비고 |
|---|---|---|
| dom | ✅ 측정됨 58.5% | 위 DOM 표 |
| html | ✅ 측정됨 75.2% | 위 HTML 표 |
| domparsing (innerHTML/XML직렬화) | ◐ 미측정 | innerHTML 경로는 존재 |
| shadow-dom | ✗ 미구현(0%) | 섀도우 트리/슬롯 없음 |
| custom-elements | ✗ 미구현(0%) | CE 업그레이드/reactions 없음 |
| domxpath | ✗ 미구현(0%) | XPath 평가기 없음 |

### 이벤트 · 입력

| 영역 | 상태 | 비고 |
|---|---|---|
| uievents | ◐ 미측정 | 기본 이벤트 디스패치만 |
| pointerevents / touch-events | ✗ 미구현(0%) | 포인터/터치 이벤트 모델 없음 |
| input-events / editing / selection | ✗ 미구현(0%) | contenteditable/execCommand 없음 |
| clipboard-apis | ✗ 미구현(0%) | |
| pointerlock / fullscreen / gamepad | ✗ 미구현(0%) | |

### 스크립팅 · 실행

| 영역 | 상태 | 비고 |
|---|---|---|
| test262 (JS 언어/빌트인) | ✅ 측정됨 75.0% | 위 JS 표 (WPT js/ 대신 test262 사용) |
| wasm / WebAssembly | ✗ 미구현(0%) | Wasm 실행기 없음 |
| workers (Web/Shared) | ✗ 미구현(0%) | 워커 스레드/글로벌 없음 |
| service-workers | ✗ 미구현(0%) | SW 등록/fetch 가로채기 없음 |
| web-locks | ✗ 미구현(0%) | |

### 네트워킹

| 영역 | 상태 | 비고 |
|---|---|---|
| fetch (Fetch API) | ✗ 미구현(0%) | 렌더용 HTTP 클라이언트는 있으나 JS `fetch()` 미노출 |
| xhr (XMLHttpRequest) | ✗ 미구현(0%) | |
| websockets / webrtc / webtransport | ✗ 미구현(0%) | |
| eventsource / beacon | ✗ 미구현(0%) | |
| content-security-policy / cors / mixed-content | ✗ 미구현(0%) | |
| cookies / cookie-store | ✗ 미구현(0%) | |
| referrer-policy / upgrade-insecure-requests | ✗ 미구현(0%) | |

### 저장소

| 영역 | 상태 | 비고 |
|---|---|---|
| web-storage (local/sessionStorage) | ✗ 미구현(0%) | |
| IndexedDB | ✗ 미구현(0%) | |
| storage / quota / storage-access-api | ✗ 미구현(0%) | |
| FileAPI / file-system-access / entries-api | ✗ 미구현(0%) | |

### 미디어 · 그래픽

| 영역 | 상태 | 비고 |
|---|---|---|
| webaudio | ✗ 미구현(0%) | |
| webgpu | ✗ 미구현(0%) | |
| media-source / mediacapture-* / encrypted-media | ✗ 미구현(0%) | |
| webcodecs / webvtt / imagebitmap | ✗ 미구현(0%) | |

### 타이밍 · 성능 · 관찰자

| 영역 | 상태 | 비고 |
|---|---|---|
| hr-time | ◐ 미측정 | `performance.now` 일부 |
| *-timing (resource/navigation/user/paint/event/element) | ✗ 미구현(0%) | 성능 타임라인 없음 |
| intersection-observer / resize-observer | ✗ 미구현(0%) | |
| mutation-events | ◐ 미측정 | MutationObserver 여부 확인 필요 |
| requestidlecallback / page-visibility / visual-viewport | ✗ 미구현(0%) | |

### 보안 · 암호

| 영역 | 상태 | 비고 |
|---|---|---|
| WebCryptoAPI | ✗ 미구현(0%) | |
| webauthn / credential-management | ✗ 미구현(0%) | |
| permissions / permissions-policy / trusted-types | ✗ 미구현(0%) | |
| secure-contexts / subresource-integrity | ✗ 미구현(0%) | |

### 국제화 · 인코딩 · URL

| 영역 | 상태 | 비고 |
|---|---|---|
| intl402 (test262) | ✅ 측정됨 5.3% | Intl 거의 미구현 |
| url / URL | ◐ 미측정 | 내비게이션용 URL 파서는 존재 |
| encoding | ◐ 미측정 | UTF-8 디코딩 경로 존재 |
| i18n | ✗ 미구현(0%) | |

### 디바이스 · 센서

| 영역 | 상태 | 비고 |
|---|---|---|
| geolocation / sensors / generic-sensor | ✗ 미구현(0%) | |
| battery-status / vibration / device-memory | ✗ 미구현(0%) | |
| screen-orientation / screen-wake-lock | ✗ 미구현(0%) | |
| webhid / webusb / web-bluetooth / webnfc / serial / webxr | ✗ 미구현(0%) | |

### 기타 플랫폼 API

| 영역 | 상태 | 비고 |
|---|---|---|
| console | ◐ 미측정 | `console.log` 등 존재 |
| streams / compression | ✗ 미구현(0%) | |
| notifications / push-api / payment-request | ✗ 미구현(0%) | |
| background-fetch / background-sync | ✗ 미구현(0%) | |
| web-share / broadcastchannel / channel-messaging | ✗ 미구현(0%) | |
| webdriver / accname / wai-aria (접근성) | ✗ 미구현(0%) | |
| picture-in-picture / portals / fenced-frame | ✗ 미구현(0%) | |

> 요약: 현재 kestrel 은 **렌더링 코어(HTML/CSS/DOM) + JS 언어(test262)** 에 집중. 나머지 대부분 영역(네트워킹/워커/저장소/미디어/디바이스/보안 API)은 미구현(0%)이며 이게 "브라우저 완성"까지의 실제 남은 로드맵이다. 우선순위는 렌더링 정확도(css) → 코어 API(events/URL/encoding/console) → 나머지 순으로 잡는다.

## 확인된 구체적 버그

- ✅ **[해결] border-radius 계산값의 vertical 반경 소실 + 보간.** getComputedStyle 이
  세로 반경을 버리던 것을 수정: 확장이 코너별 (h,v) 파싱→v≠h 면 longhand `Keyword("h v")`,
  `radius_prop` 이 첫 토큰(가로) 사용(레이아웃 불변), 단축 계산 재조립을 가로/세로 축 분리
  (`window.rs`·`computed_shorthand_animated`, calc 안전 위해 `split_ws_depth0`). 보간까지
  완결: px↔% → calc(항 순서는 FROM 단위 기준), 음수 0 클램프(calc 내부는 보존), h↔"h v"
  토큰 정규화, 균일 결과 접기, getPropertyValue 경로도 재조립 우선. **border-radius-
  interpolation 78→0**, css-backgrounds ~76→78%. (커밋 903cad0·565c2d1·32f06f4·c97d304·
  7bb889e·692ddad·17eef9a)
- **box-shadow 가 blur/spread 에 calc()/math 미수용.** `inset 0 0 0 calc(max(10em,20px)/2)
  black` 를 "지원 안 함"으로 거부(box-shadow-interpolation 등). box-shadow 길이 성분 검증에
  수학함수 허용 필요.

## 측정 주의 (러너 과소보고)

일부 WPT 파싱 테스트(예: `selectors/parsing/parse-is-where`, `parse-has` 등)는
`parsing-testcommon.js` 의 "style 생성 → sheet 참조 → head 에서 제거 → `sheet.insertRule`
→ `cssRules.length` 확인" 패턴을 쓴다. 헤드리스 러너에서 이 패턴이 하네스 안에서 실패로
집계되지만, **엔진 자체는 정상**임을 프로브로 확인했다(`:is(div )`·`:is(div + bar)`·
`:is(:is(div))`·분리 시트 insertRule·50회 반복 모두 통과). 따라서 selectors 등 일부
영역의 실제 엔진 적합성은 표의 수치보다 높다(러너 아티팩트로 과소보고).

## 참고

- **미구현 대형**: Temporal (0%, 4,603), Atomics (0%, 389), Intl (5%). 애니메이션/트랜지션 interpolation (css-animations/transitions/motion 등 다수) 은 실행 엔진 미구현.
- **JS 강세**: Object/Array/String/Date/Math/Number/Reflect/JSON/Set/Boolean 90~97%. 약점: ArrayBuffer(36%)/BigInt(34%)/Error(52%)/Symbol(62%)/Promise(60%, 진짜 async 미구현)/Iterator(61%).
- **CSS 강세**: css-align(97%)/css-text-decor(93%)/css-will-change(100%)/CSS2(91%)/css-images(90%)/css-color(83%). 약점: css-typed-om(1%)/geometry(2%)/css-nesting(4%)/css-view-transitions(5%)/mediaqueries(8%)/css-gaps(10%)/css-mixins(9%)/cssom-view(16%).
- 세부는 `docs/conformance/anchor-position.md` 등 영역별 문서 참조.
