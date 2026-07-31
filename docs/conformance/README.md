# Kestrel 표준 적합성 현황

측정일: 2026-07-31. 러너: 헤드리스 렌더 + WPT testharness / test262. WPT 는 testharness 기반 서브테스트 기준(reftest·수동 테스트 제외, 하네스 못 돈 파일은 분모에서 빠짐). test262 는 실행분(module/onlyStrict 건너뜀) 기준. % = 통과/전체.

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

| 서브트리 | 통과 / 전체 | % |
|---|---|---|
| css-device-adapt | 1 / 1 | 100.0% |
| css-will-change | 173 / 173 | 100.0% |
| css-align | 3,232 / 3,322 | 97.3% |
| css-forced-color-adjust | 13 / 14 | 92.9% |
| css-text-decor | 1,182 / 1,276 | 92.6% |
| CSS2 | 592 / 653 | 90.7% |
| css-images | 3,225 / 3,580 | 90.1% |
| css-color-adjust | 137 / 157 | 87.3% |
| compositing | 144 / 167 | 86.2% |
| css-content | 176 / 211 | 83.4% |
| css-color | 6,924 / 8,314 | 83.3% |
| css-size-adjust | 170 / 207 | 82.1% |
| css-flexbox | 1,073 / 1,315 | 81.6% |
| css-backgrounds | 3,788 / 4,914 | 77.1% |
| css-viewport | 369 / 490 | 75.3% |
| css-display | 287 / 384 | 74.7% |
| css-break | 452 / 609 | 74.2% |
| css-ui | 1,390 / 1,888 | 73.6% |
| css-transforms | 2,886 / 3,969 | 72.7% |
| css-anchor-position | 9,225 / 12,913 | 71.4% |
| css-ruby | 52 / 73 | 71.2% |
| css-position | 993 / 1,412 | 70.3% |
| selectors | 2,872 / 4,118 | 69.7% |
| css-sizing | 2,237 / 3,212 | 69.6% |
| css-text | 2,078 / 3,029 | 68.6% |
| cssom | 2,305 / 3,417 | 67.5% |
| css-fonts | 4,690 / 6,958 | 67.4% |
| css-conditional | 1,870 / 2,808 | 66.6% |
| css-scroll-snap | 458 / 690 | 66.4% |
| css-shapes | 3,336 / 5,093 | 65.5% |
| css-multicol | 850 / 1,356 | 62.7% |
| css-grid | 2,156 / 3,567 | 60.4% |
| css-env | 5 / 9 | 55.6% |
| css-box | 529 / 957 | 55.3% |
| css-animations | 574 / 1,059 | 54.2% |
| css-contain | 194 / 360 | 53.9% |
| css-variables | 272 / 511 | 53.2% |
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
| css-overflow | 369 / 983 | 37.5% |
| css-tables | 191 / 557 | 34.3% |
| filter-effects | 833 / 2,452 | 34.0% |
| css-borders | 365 / 1,151 | 31.7% |
| css-values | 2,388 / 7,651 | 31.2% |
| css-layout-api | 4 / 13 | 30.8% |
| css-transitions | 879 / 3,086 | 28.5% |
| fill-stroke | 104 / 371 | 28.0% |
| motion | 1,053 / 3,775 | 27.9% |
| css-link-params | 6 / 25 | 24.0% |
| css-cascade | 162 / 717 | 22.6% |
| css-masking | 831 / 4,312 | 19.3% |
| css-syntax | 81 / 428 | 18.9% |
| cssom-view | 300 / 1,827 | 16.4% |
| css-scroll-anchoring | 13 / 84 | 15.5% |
| css-pseudo | 77 / 532 | 14.5% |
| css-properties-values-api | 93 / 744 | 12.5% |
| css-color-hdr | 13 / 116 | 11.2% |
| css-shadow | 37 / 347 | 10.7% |
| css-gaps | 333 / 3,382 | 9.8% |
| css-mixins | 46 / 492 | 9.3% |
| mediaqueries | 24 / 308 | 7.8% |
| css-view-transitions | 54 / 1,208 | 4.5% |
| css-nesting | 5 / 117 | 4.3% |
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

## 참고

- **미구현 대형**: Temporal (0%, 4,603), Atomics (0%, 389), Intl (5%). 애니메이션/트랜지션 interpolation (css-animations/transitions/motion 등 다수) 은 실행 엔진 미구현.
- **JS 강세**: Object/Array/String/Date/Math/Number/Reflect/JSON/Set/Boolean 90~97%. 약점: ArrayBuffer(36%)/BigInt(34%)/Error(52%)/Symbol(62%)/Promise(60%, 진짜 async 미구현)/Iterator(61%).
- **CSS 강세**: css-align(97%)/css-text-decor(93%)/css-will-change(100%)/CSS2(91%)/css-images(90%)/css-color(83%). 약점: css-typed-om(1%)/geometry(2%)/css-nesting(4%)/css-view-transitions(5%)/mediaqueries(8%)/css-gaps(10%)/css-mixins(9%)/cssom-view(16%).
- 세부는 `docs/conformance/anchor-position.md` 등 영역별 문서 참조.
