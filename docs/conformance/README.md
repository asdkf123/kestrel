# Kestrel 표준 적합성 현황

측정일: 2026-08-02 (아래 CSS 표 중 이번 세션에 재측정한 행은 최신 — 표에 ★ 표시. 나머지 행과 축별 종합은 2026-07-31 측정이라 현재보다 과소). 러너: 헤드리스 렌더 + WPT testharness / test262. WPT 는 testharness 기반 서브테스트 기준(reftest·수동 테스트 제외, 하네스 못 돈 파일은 분모에서 빠짐). test262 는 실행분(module/onlyStrict 건너뜀) 기준. % = 통과/전체.

**★ 행의 분모가 예전보다 큰 이유**: JS 실행 예산을 넉넉히(스크립트 30초/페이지 60초) 주고 측정했다. 기본값(5초/10초)은 벽시계 기준이라 무거운 파일이 중간에 잘리고, 잘리는 지점이 머신 부하에 따라 달라져 같은 바이너리로 같은 파일이 344~406 서브테스트를 오갔다. 비교 측정은 반드시 같은 예산에서 하고, 통과 수와 함께 **분모와 "하네스 못 돈 파일" 수**를 봐야 한다 (분모가 줄면서 %가 오르는 건 개선이 아니라 잘린 것이다).

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
> - **mediaqueries 7.8→88.3%, cssom 69.1→72.5%, selectors 69.7→76.3%, css-syntax 18.9→87.1%**(이전 세션 포함).
> - **★색 스윕 세션: css-color 84.4→92.4% (+753)**. 상대색(origin 계산색·중첩 origin 재귀·트리그/hsl bare 퍼센트/hue [0,360) 정규화·achromatic none-hue·lab↔lch·색역 밖 보존), color-mix(색공간 캐논 xyz-d65/oklab 생략·shorter hue 생략·none 운반·wide-gamut none 직렬화), color()·color-layers()(신규)·display-p3-linear·rgb calc 채널 등. 아래 개별 bullet 참조.
> - **css-values url-modifiers (+51)**: `url("…" cross-origin()/integrity()/referrer-policy())` request modifier 를 background-image 에서 보존 직렬화(Value::Url 우회) + 유효성 검증(무효 modifier 거부).
> - **css-color +136 (color-layers)**: `color-layers([<blend-mode>,]? <color>#)`(§CSS Color 6)
>   파싱·직렬화 신규 구현. 기본 블렌드 `normal` 생략, 각 레이어 색 검증(임의 ident·후행 콤마
>   거부)+직렬화. interpret_value/serialize_decl 배선. 남은 실패는 중첩 relative-color/color-mix
>   를 지정값 형태로 보존(현재 계산색으로 과다 해석).
> - **css-color +25 (color-mix)**: `color-mix()` 직렬화가 기본 hue 보간법 `shorter hue` 를 생략
>   (§CSS Color 5 — `in hsl shorter hue` → `in hsl`). longer/increasing/decreasing 은 유지.
> - **css-color +139 (relative-color)**: `rgb(from <origin> …)` 등 상대색의 origin 을 계산색으로
>   직렬화. 예전엔 origin 정규화 체인이 normalize_relative_color/color_function/lab_like/color_mix
>   만 시도해 레거시 `rgb(20%,40%,60%,80%)`/`hsl(120deg …)` origin 을 못 다뤄 원문을 냈다. 이제
>   `serialize_mix_input_color` 폴백으로 색 파싱→계산색(`rgba(51, 102, 153, 0.8)` 등)으로 직렬화.
>   남은 실패는 채널 calc 재정렬(`calc(g*2)`→`calc(2*g)`)·none-origin 엣지.
> - **css-color +36 (alpha() 함수)**: `alpha(from <origin> [/ <alpha>])`(§CSS Color 5 §relative-alpha)
>   신규 구현. 검증(alpha_color_valid: calc 는 alpha 외 채널 키워드 참조 금지), 계산(origin rgb 유지+
>   알파 교체, `alpha` 키워드/calc 치환), 지정값 직렬화(normalize_alpha_color: origin 캐논). 남은 8건은
>   calc 기호 재정렬(`calc(alpha*0.5)`→`calc(0.5*alpha)`)·sibling-index()·중첩 origin 지정값 보존(후속).
> - **css-color +16 (alpha() computed 지원)**: `alpha` 를 @supports 함수 화이트리스트(FUNCS)에 추가 —
>   CSS.supports("color", "alpha(...)")=true 로 alpha-color-computed 잠금 해제. 남은 16 은 computed 가
>   origin 색공간 보존(`alpha(from color(srgb 1 0 0)/0.5)`→`color(srgb 1 0 0 / 0.5)`)을 요구(후속).
> - **css-color +34 (color-layers 지정값 내부 색)**: `color-layers()` 지정값 직렬화가 각 레이어의
>   지정형을 보존 — 상대색 `rgb(from black r g b / 0.5)`·`color-mix(…)` 를 계산색(rgba/color(srgb))으로
>   접지 않고 normalize_relative_color/normalize_color_mix 등 지정 정규화기로 직렬화.
> - **css-color +8 (alpha() computed 색공간 보존)**: origin 이 모던 함수(color/color-mix/lab/lch/oklab/
>   oklch)면 그 계산 직렬화에 알파만 주입(set_alpha_on_serial), 레거시 srgb+none 알파는 color(srgb …) 폴백.
>   rgb/hsl 및 이들 상대색은 rgba() 로. 남은 9 는 currentcolor 해석(cascade)·중첩 u8 알파 정밀도(후속).
> - **css-color +19 (상대색 채널 calc 직렬화 재정렬)**: `rgb(from … calc(g * 2) …)`→`calc(2 * g)`,
>   `calc(a / 3)`→`calc(0.333333 * a)`, `calc(l - 20)`→`calc(-20 + l)`, `calc(g*.5 + g*.5)`→`calc((0.5 * g) + (0.5 * g))`
>   등 채널 calc 를 §CSS Values 4 calc 직렬화(sum-of-products: 계수 먼저·순수 수항 먼저·다항 곱항 괄호)로
>   캐논. 함수/그룹/단위 등 복잡형은 verbatim 폴백, var()(pending-substitution)는 재정렬 안 함. 격리 구현(css-values 무관).
> - **css-color +21 (상대색 채널 calc 타입 검사)**: `rgb(from … calc(r + 1%) …)`·`hsl(from … calc(h + 1deg) …)`
>   등 채널 keyword 를 수(1)로 치환 후 calc 타입 검사(mdim_of) — 채널은 number(hue 는 angle 도)|percentage
>   만 허용, number+percentage/angle 혼합은 무효로 거부(§CSS Color 5). 예전엔 col_is_calc 로 무조건 수용.
> - **css-color +6 (hsl/hwb hue calc)**: `hsl(calc(infinity) 100% 50%)`·`hwb(calc(0/0) …)` 등 hue 채널의
>   calc 를 comp_angle 에 각도 인식 평가기로 추가(비유한 ±inf/NaN → 0 = red). parse_hsl_func 가 hue 를
>   조잡한 `trim("deg")` 대신 comp_angle 사용 → grad/rad/turn/calc 도 처리.
> - **css-color +6 (hwb bare number)**: `hwb(120 30 50)` 의 벌거벗은 whiteness/blackness `<number>`
>   를 `<percentage>` 와 같은 스케일(30 = 30% = 0.30)로 해석(comp_num). 예전엔 30 을 raw 로 써
>   w+b>1 정규화로 회색이 됐다. §CSS Color 4: number 는 percentage 상당값.
> - **css-color +4 (상대색 hue calc)**: `oklch(from blue .5 .3 calc(pi * 1rad))` 등 hue 채널의
>   calc 를 각도 인식 스칼라 평가기로 계산(rad/turn/grad/deg 혼합·pi·그룹). 기존 deg-strip 경로는 폴백 유지.
> - **css-color +32 (color-function)**: `color(<space> …)` 지정값 직렬화가 성분 안 calc 를
>   캐논화. 예전엔 `split_whitespace` 로 나눠 `calc(0.5 + 1)` 의 내부 공백에서 깨지고 `/`
>   치환이 calc 내부 `/` 를 망가뜨려 통째로 bail(원문 반환)했다. 이제 괄호 균형 `color_parts`
>   로 나누고 각 math 성분을 `canon_calc_serialize`(calc 형태 유지, `calc(0.5+1)`→`calc(1.5)`,
>   `calc(0/0)`→`calc(NaN)`)로 캐논화. 비-calc 성분은 기존 로직 그대로 → 회귀 0. 남은 실패는
>   calc 곱셈 피연산자 재정렬(`sign()*10%`→`10%*sign()`)뿐(canon_calc 심화 규칙).
> - **dom nodes +25 (createElementNS +16 등)**: 요소 네임스페이스 인코딩 수정. 예전엔
>   `namespace: None` 이 **HTML 과 null 네임스페이스를 둘 다** 뜻해, `createElementNS(null,"foo")`
>   가 HTML 요소처럼 tagName 대문자화·namespaceURI=HTML 로 나왔다. 이제 None=null-ns,
>   Some(NS_HTML)=HTML 로 분리(HTML 요소 생성부는 전부 Some(NS_HTML), 대문자화·attr
>   소문자화·isHTML 은 `is_html_ns()` 기준). 이름 기준 dom fixed 25/regr 0, html/dom regr 0.
> - **css-syntax +78 (68.7→87.1%)**: `unicode-range` 프로퍼티 `<urange>` 엄격 검증+캐논 직렬화
>   (`U+a?`→`U+A0-AF`, 7자리·잘못된 `?` 위치·비hex·`>10FFFF` 무효). urange 특수 토큰화(주석은
>   분리 없이 제거 — 공유 strip_comments 의 공백 치환과 달리 빈 문자열 제거). CSSOM setProperty
>   무효 값은 **no-op**(이전 값 유지) — 기존엔 무효 값이 선언을 지웠음.
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
| css-text-decor ★ | 1,203 / 1,276 | 94.3% |
| CSS2 | 592 / 653 | 90.7% |
| css-images ★ | 3,344 / 3,580 | 93.4% |
| mediaqueries | 272 / 308 | 88.3% |
| css-color-adjust | 137 / 157 | 87.3% |
| compositing | 144 / 167 | 86.2% |
| cssom ★ | 2,980 / 3,437 | 86.7% |
| css-color | 8,944 / 9,521 | 93.9% |
| css-content | 176 / 211 | 83.4% |
| css-size-adjust | 170 / 207 | 82.1% |
| css-conditional | 2,143 / 2,602 | 82.4% |
| css-flexbox | 1,125 / 1,379 | 81.6% |
| css-nesting | 95 / 117 | 81.2% |
| css-backgrounds ★ | 4,991 / 6,181 | 80.7% |
| css-anchor-position ★ | 10,462 / 13,180 | 79.4% |
| css-position | 1,078 / 1,412 | 76.3% |
| selectors | 3,143 / 4,118 | 76.3% |
| css-viewport | 369 / 490 | 75.3% |
| css-display ★ | 284 / 384 | 74.0% |
| css-break | 452 / 609 | 74.2% |
| css-ui | 1,390 / 1,888 | 73.6% |
| css-fonts | 5,571 / 7,565 | 73.6% |
| css-transforms ★ | 4,039 / 5,130 | 78.7% |
| css-ruby ★ | 54 / 76 | 71.1% |
| css-sizing | 2,237 / 3,212 | 69.6% |
| css-syntax | 373 / 428 | 87.1% |
| css-text | 2,078 / 3,029 | 68.6% |
| css-scroll-snap | 458 / 690 | 66.4% |
| css-properties-values-api | 611 / 923 | 66.2% |
| css-shapes ★ | 4,969 / 6,149 | 80.8% |
| css-cascade | 458 / 717 | 63.9% |
| css-multicol ★ | 1,098 / 1,543 | 71.2% |
| css-grid | 2,156 / 3,567 | 60.4% |
| css-env | 5 / 9 | 55.6% |
| css-box ★ | 618 / 957 | 64.6% |
| css-animations ★ | 704 / 1,059 | 66.5% |
| css-contain | 194 / 360 | 53.9% |
| css-variables | 272 / 511 | 53.2% |
| css-values | 4,164 / 7,764 | 53.6% |
| css-writing-modes | 188 / 356 | 52.8% |
| css-overscroll-behavior ★ | 63 / 97 | 64.9% |
| css-page | 49 / 97 | 50.5% |
| css-easing | 77 / 156 | 49.4% |
| css-logical ★ | 689 / 1,364 | 50.5% |
| css-counter-styles | 57 / 118 | 48.3% |
| css-scrollbars | 22 / 46 | 47.8% |
| css-rhythm | 72 / 155 | 46.5% |
| css-inline ★ | 425 / 635 | 66.9% |
| css-lists ★ | 623 / 960 | 64.9% |
| css-borders ★ | 822 / 1,151 | 71.4% |
| css-overflow ★ | 469 / 986 | 47.6% |
| css-tables | 191 / 557 | 34.3% |
| filter-effects ★ | 1,880 / 2,452 | 76.7% |
| css-layout-api | 4 / 13 | 30.8% |
| css-transitions ★(큰 파일 3개는 실행되나 러너 타임아웃 초과로 분모 제외) | 1,069 / 1,406 | 76.0% |
| fill-stroke | 104 / 371 | 28.0% |
| motion ★ | 3,256 / 4,955 | 65.7% |
| css-link-params | 6 / 25 | 24.0% |
| css-masking ★ | 5,320 / 6,354 | 83.7% |
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
| **dom (전체)** ★ | 4,244 / 7,031 | **60.4%** |
| abort | 0 / 2 | 0.0% |
| collections | 17 / 48 | 35.4% |
| events | 277 / 580 | 47.8% |
| lists | 168 / 189 | 88.9% |
| nodes | 3,406 / 5,748 | 59.3% |
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
| dom | ✅ 측정됨 58.8% | 위 DOM 표 |
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

## 2026-08-02 세션 (가상 시계 + CSS.supports 게이트)

같은 예산·같은 분모로 전후를 직접 측정한 순증(기준선 커밋 `35155c8`):

| 영역 | 기준선 | 현재 | 증감 |
|---|---|---|---|
| css-masking | 1,223 | 5,320 | +4,097 |
| css/motion | 1,334 | 3,256 | +1,922 |
| css-shapes | 3,510 | 4,969 | +1,459 |
| css-anchor-position | 9,997 | 10,462 | +465 |
| css-lists | 439 | 623 | +184 |
| css-transitions | 886 | 1,069 | +183 |
| css-inline | 282 | 425 | +143 |
| css-multicol | 972 | 1,098 | +126 |
| css-box | 546 | 618 | +72 |
| css-borders | 426 | 822 | +396 |
| css-logical | 669 | 689 | +20 |
| filter-effects | 833 | 1,880 | +1,047 |
| 그 외(overflow/text-decor/ruby/images/backgrounds/display/dom/animations) | | | +38 |

**주요 변경**
- **가상 시계 이벤트 루프**: 타이머가 지연을 지킨다(§HTML timer initialization steps —
  발화 시각 순서, 동시각은 등록 순서, 중첩 5단계 초과 4ms 클램프). requestAnimationFrame
  을 setTimeout 별칭에서 분리(60Hz 격자, 실행 전 큐 비움). cancelAnimationFrame 이 이제
  실제로 취소한다. performance.now/Date.now/new Date 가 한 시계.
- **트랜지션을 스타일 변경 이벤트로**(§CSS Transitions 4): 인라인 style 쓰기 훅이 아니라
  재계산 전후 계산값 비교로 생성. 목록 뒤 항목 우선, currentcolor 예외,
  transitionrun/start/end 발화, document/element.getAnimations() 실구현.
- **★CSS.supports 게이트 해소가 최고 ROI**: 애니메이션/보간 테스트마다
  `CSS.supports(prop, value)` 선행조건이 있어 한 줄의 미등록이 수천 하위테스트를 막는다.
  basic-shape 함수 8종·repeating-*-gradient(FUNCS), mask-border-* 6종, 검증 arm 은 있는데
  등록이 빠진 롱핸드 16종, 전역 키워드×단축(property_known), 키워드/수 롱핸드 14종,
  border 논리 22종, position-anchor/position-try-fallbacks.
- **논리 프로퍼티는 단축이 아니라 별칭**: 확장 결과가 롱핸드 하나면 계산값 열거에 남겨야
  한다(빼면 getComputedStyle-listing 이 깨진다 — 실측 -10 회귀 후 정정).
- **필터 리스트 add 합성**: §Filter Effects 상 filter/backdrop-filter 의 `add` 합성은
  리스트 **이어붙이기**인데 수치 합산을 하고 있었다(blur(10px)+blur(15px) 가
  blur(25px) 로). accumulate 는 함수별 누적이라 기존 경로 유지.
- **corner-shape 보간**(§CSS Borders 4): superellipse 파라미터를 **정규화 half-corner
  공간 [0,1]** 으로 옮겨 선형 보간한 뒤 되돌린다 — `v = 0.5^(1/2^|s|)`(s<0 이면 1-v),
  역변환은 `c = max(v,1-v)`, `k = ln(0.5)/ln(c)`, `s = log2(k)`(v<0.5 면 부호 반전).
  키워드(round/bevel/scoop/notch/square/squircle)는 파라미터로 매핑해 함께 보간하고,
  결과는 항상 `superellipse(N)`(무한대는 infinity)로 직렬화한다.
  corner-shape-interpolation 478개 **전부 통과**(이전 실패 396).
- **기본 도형 합성**: clip-path/shape-outside/offset-path 의 add/accumulate 합성이
  같은 도형 함수의 수 성분을 짝지어 더한다(circle(50px at 10px 20px) 둘을 합성하면
  circle(100px at 20px 40px)). 키워드 토큰(at/closest-side 등)은 양쪽이 같을 때만 유지하고,
  단위가 다르면 합성하지 않는다.
- **필터 수 인자 계산값**: 수 인자 함수(brightness/contrast/saturate/opacity/grayscale/
  invert/sepia)의 계산값은 `<number>` 다 — 퍼센트를 수로 바꾸고(300% → 3) calc 을
  평가하며 음수는 0 으로 자른다. blur 의 calc 도 px 로 평가.
- **필터 인자 생략 정규화**: `blur()`/`grayscale()` 처럼 인자를 뺀 형태를 각 함수의
  기본값으로 채운다(blur=0px, grayscale/invert/sepia/brightness/contrast/opacity/
  saturate=1, hue-rotate=0deg). 계산값 직렬화와 보간이 함께 맞아진다.
- **필터 범위 클램프 + accumulate 함수별 덧셈**: 보간·합성 결과가 범위를 벗어나던 것을
  각 함수 정의대로 자른다(blur/brightness/contrast/saturate ≥ 0, grayscale/invert/sepia/
  opacity 는 [0,1]). accumulate 합성은 같은 함수 순서일 때 인자를 함수별로 더하되
  곱셈형(brightness/contrast/saturate/opacity)은 항등값 1 을 빼고 더한다(§Web Animations).
- **각도 보간**: deg/rad/grad/turn 을 도로 환산해 보간(결과는 deg). hue-rotate 같은 함수
  인자가 이 경로를 탄다. **단, 그라디언트/이미지 함수는 일반 함수 보간에서 제외한다** —
  §CSS Images 의 보간은 스톱 위치 정규화·보간 색공간·캐논 직렬화를 요구해서 인자별 lerp
  로는 틀린 값이 나온다(실측 -46 회귀 후 제외로 정정). 틀린 값보다 불연속이 낫다.
- **필터 리스트 중립값 패딩**(§Filter Effects 5.2): `none` 또는 짧은 리스트를 상대
  리스트의 항등값(blur(0px)/brightness(1)/grayscale(0)/hue-rotate(0deg) 등)으로 채워
  길이를 맞춘 뒤 함수별 보간. none↔blur(10px) 가 blur(0px)↔blur(10px) 가 된다.
- **함수 인자 보간(일반)**: `name(args)` 두 값의 이름이 같고 최상위 콤마 인자 수가 같으면
  인자별로 보간한다(blur(10px)→blur(40px), drop-shadow(...), grayscale(...)). 예전엔 함수
  토큰을 보간하지 못해 필터 리스트가 끝값에 머물렀다.
- **성능**: StyledNode.specified_values 를 Rc 공유로, 트랜지션 감지를 Value 직접 비교로
  (문자열 포매팅·할당 제거). 계산 스타일 요청 시 해석은 설계만 완료
  (`docs/superpowers/specs/2026-08-02-kestrel-computed-style-on-demand-design.md`).
- **엔진 일반 버그**: private 이름을 "#x" 문자열로 표현해 `o["#sel"] = v` 같은 평범한
  대입이 private 필드 쓰기로 오인됐다(CSS 선택자·URL 조각). 렉서가 NUL 접두를 붙여
  문법으로만 만들어지게 수정.

**되돌린 시도(실증)**: rect()/xywh() 를 계산값에서 inset() 으로 변환(§CSS Shapes 2)을
구현했으나 css-masking -98 회귀로 되돌렸다. 기대값이 단순 변환이 아니라 **보간 결과를
calc 로 표현한 형태**(예: `inset(-30px calc(100% - 0px) 90% -30%)`)라, 변환만으로는
맞지 않고 inset 보간의 calc 처리까지 함께 있어야 한다. 부분 구현은 기존에 우연히 맞던
불연속 결과를 깨서 net-negative 였다.

**되돌린 시도 2(실증)**: 보간 결과의 calc 직렬화를 §CSS Values 4 캐논 순서(퍼센트→차원)
로 바꾸고 음수 항을 `+ -X` 대신 `- X` 로 쓰도록 했다. shape()/offset 좌표는 맞아졌지만
(motion +40) border-radius 계열이 깨져(backgrounds -32) 합계가 음수였다. 조건을 좁혀도
(motion +8 / backgrounds -8 / masking -10), 부호 표기만 바꿔도(motion +8 / backgrounds -8 /
masking -17) 마찬가지였다. **calc 항 순서·부호는 프로퍼티마다 기대가 달라 단일 규칙으로는
안 된다** — 프로퍼티별 직렬화 규칙 표가 필요하다. 코드에 주석으로 실측치를 남겼다.

**남은 큰 것**: 계산 스타일 요청 시 해석(최대 핫스팟 collect_computed_styles),
Web Animations 객체 모델(Animation 상태기계·다중 키프레임), 보간 타입 레지스트리,
미지 프로퍼티 57개(대부분 실험 기능).

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

## 측정 주의 (JS 예산 = 벽시계)

WPT 통과 수가 실행마다 흔들리면 러너 병렬성이 아니라 **JS 실행 예산이 벽시계 기준**인지
먼저 의심할 것. `SCRIPT_BUDGET_MS=5s`(스크립트/핸들러 하나), `TOTAL_BUDGET_MS=10s`(페이지
전체)라 무거운 파일이 중간에 잘리고, 잘리는 지점이 머신 부하에 따라 달라진다. 같은
바이너리로 같은 파일이 344~406 서브테스트를 오간 사례가 있다. 둘 다 환경변수
(`KESTREL_SCRIPT_BUDGET_MS`, `KESTREL_TOTAL_BUDGET_MS`)로 조절되므로 **비교 측정은 넉넉한
예산(30초/60초)에서** 하고 subprocess 타임아웃도 함께 올린다.

**분모와 "하네스 못 돈 파일" 수를 통과 수와 함께 볼 것** — 분모가 줄면서 %가 오르는 건
개선이 아니라 잘린 것이다.

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
