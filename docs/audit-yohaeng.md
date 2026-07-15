# 요행(luck-based) 코드 감사 — 표준 기반으로 교체 대상

6개 서브시스템 병렬 감사 종합. "테스트엔 통과하지만 표준 메커니즘이 아니라
우연/땜빵으로 맞아떨어져 실제 유효 입력에서 깨지는" 코드 목록. 심각도·빈도순.

상태: [ ] 미착수  [~] 진행중  [x] 완료

## 구조적 뿌리 (크고, 여러 곳에 영향)

- [x] **JS 객체 프로퍼티 순서** — HashMap → 삽입 순서 유지 ObjMap(정수키 오름차순 먼저).
  for-in/Object.keys/JSON 이 삽입 순서(JSON 정렬 제거). delete 실제 제거도 구현. (20029e5)
- [x] **JS 프로토타입 링크** — new F() 가 prototype 을 __proto__ 로 링크(스냅샷 아님).
  체인 조회 + function-ctor instanceof + __proto__ 비열거(keys/for-in/JSON/hasOwnProp). (1899093)
- [x] **JS ToPrimitive** — 템플릿/`+`/산술은 이미 valueOf/toString 호출. String() 도 ToString(hint string)로 수정. (4284467)
- [x] **JS Promise 거부 의미론** — .catch/then(,onR)/거부 전파/throw→거부/async throw→거부
  /await 거부→throw/Promise.all 거부/allSettled/race 거부 채택. (19874bc)
- [x] **JS 누락 표준 메서드 보강** — Object.values/entries/fromEntries, Array/String.at,
  flatMap, structuredClone, Array.from/of. (fc28720, 580ac2c, 62d4a25)
- [x] **float in nearest-BFC(§9.5)** — 비BFC 블록은 float 을 담지 않고 trailing_floats 로
  부모 BFC 까지 버블. 중첩 래퍼 밖 형제·조상이 우회. 루트는 BFC 로 담음. (layout/mod.rs)

## 티어1 — 고빈도 + 요행 통과 (우선 수정)

- [x] CSS **`!important`** — 캐스케이드 우선순위 구현. (8854de4)
- [x] CSS **`font` 단축** + #rgba/#rrggbbaa hex. (fa0532a)
- [x] JS **문자열 이스케이프 `\u \x \b \f \v` + 줄이음**. (41cf1fb)
- [~] JS **옵셔널체이닝 `obj?.method()` 단락** — 결과는 lenient 로 이미 맞음(undefined). 메커니즘만 비표준 → 후순위.
- [x] 폰트 **합성 글리프** — é/ñ/CJK 렌더. cmap fmt12 는 아직(대형 CJK). (dd17ee0)
- [x] CSS **미디어쿼리 em/rem + 특성평가 + 미인식→불일치**. (ba15d97)
- [x] CSS **rem 루트 font-size 기준**. (1e6c0eb)
- [x] CSS **조상 구조 의사클래스 정확 평가** — zebra 수정. (f1e87ca)
- [x] Flex/CSS **flex-basis** — flex:1 등폭. (c8b8006)
- [x] Grid **auto/% 트랙 사이징** — auto 1fr. (0bb6735)
- [x] JS **`let` 반복별 바인딩** — 클로저 [0,1,2]. (faa2dc6)
- [x] JS **구조분해 할당 `[a,b]=arr`/`({x,y}=o)`**. (c0bbd9d)
- [x] JS **new Promise(executor) + finally**. (a5711f3)
- [x] JS **Math.round 음수/min·max NaN + NaN/Infinity 전역**. (75a8049)
- [x] JS **String indexOf(fromIndex)/split(limit)/lastIndexOf**. (ea9608a)

### ★ 티어1 전부 완료(15+ASI, 466 테스트 그린, 모두 회귀테스트+실사이트 검증).
### 남은 것: 티어2(작은 항목 다수) + 구조적 뿌리(대공사: JS 객체순서/프로토타입
### 링크/ToPrimitive/진짜async/float-in-BFC). 티어2 다음 우선: 페인트(둥근투명
### 테두리 사각, 점선/파선 실선, 그라디언트 프리멀티플라이), 레이아웃(줄높이 혼합폰트,
### vertical-align, 부모자식 margin상쇄), CSS(:where/:is 명시도, calc em/rem).

## 티어2 — 눈에 띄지만 빈도 낮음

- [x] 레이아웃 **줄 높이 혼합폰트 반영** — 최대 글자크기 기준(문단 근사). (9f1c3a3)
- [x] 페인트 **그라디언트 premultiplied 보간 + 필터 BT.709**. (b957a7d)
- [x] 페인트 **둥근 투명배경 테두리 링**. (adeaefb)
- [ ] 레이아웃 **줄 높이 혼합폰트 미반영(줄별 정밀화)** — 문단 최대치 근사는 됨, 줄별은 후속. (inline.rs:248)
- [ ] 레이아웃 **vertical-align 폰트크기 마법배수**. (inline.rs:132)
- [~] 레이아웃 **부모-자식 margin 상쇄 구현(§8.3.1)** 완료 — 첫/마지막 블록 자식 margin
  hoisting, flex/grid 아이템은 BFC 로 제외. 빈블록 상쇄-통과는 후속. (mod.rs)
- [x] 레이아웃 **인라인 대체요소(img 등) 원자적 인라인 흐름** — UA img를 inline-block으로,
  대체요소를 자식 박스로, 미로드 이미지 공간 예약. (98f962d)
- [ ] 레이아웃 **리스트마커·밑줄·폼컨트롤 크기 마법상수**. (mod.rs:371, inline.rs:434)
- [x] 레이아웃 **max-height 항상 사용높이 클램프**(overflow 무관, CSS §10.7). visible 이면 내용은 넘침. (ce2a50c)
- [x] 레이아웃 **인라인 요소 가로 margin/border/padding 반영(§10.3.1)** — 요소 경계에 spacer 삽입.
  내비게이션 붙음(Reddit/HN/Guardian) 해소. 테두리 링 3px 근사는 잔존. (inline.rs)
- [~] Grid **라인배치·span·auto-placement(§8) + 셀 자기정렬(§11) + grid-auto-rows + minmax(px,fr)
  하한(§11.5)** 완료 — go.dev 히어로 복원. 명명 라인, justify/align-content(트랙 분배)는 후속. (grid.rs)
- [~] Flex **shrink 시 min-content 하한 적용(§4.5)** 완료. min/max 덮어씀, align-content 는 후속. (flex.rs)
- [~] 테이블 **border-spacing** 완료(separate 표 셀 간격). auto 폭 shrink-to-fit 은 보류
  (used_width 근사가 중첩 표에서 오측정 → min/max-content 정식 측정 필요). (mod.rs)
- [x] 페인트 **둥근+투명배경 테두리** — 렌더로 재확인, 이미 링으로 정상(중복 항목, adeaefb).
- [x] 페인트 **점선/파선 테두리**(dashed/dotted). double/groove 는 근사. (7bbcf70)
- [x] 페인트 **그라디언트 프리멀티플라이 보간**(gradient_color_at 이미 반영, b957a7d). 확인 완료.
- [x] 페인트 **박스섀도 가우시안(erf 전이) + 필터/backdrop 블러 3패스 가우시안**. (bdc27e9, 2d31c45)
- [x] 페인트 **방사그라디언트 ellipse(기본)/circle 구분** — 축별 반경. 크기/위치는 아직 근사. (1d402ff)
- [x] 페인트 **overflow 사각클립이 글리프/폴리곤 픽셀클립** — 경계 걸치면 사각 ClipShape 로 래핑. (e622fc2)
- [x] 페인트 **폴리곤 AA**(세로 서브스캔라인 4 + 가로 부분커버리지). (bc07013)
- [x] 페인트 **이미지 바이리니어 스케일링**(프리멀티플라이, 투명가장자리 안전). 타일은 최근접 유지. (d15516c)
- [~] 페인트 **select 화살표/progress 하드코딩** — 렌더 확인 결과 표시는 정상.
  UA 기본 위젯 크기는 원래 구현 정의라 요행 아님. 값 자체는 유지.
- [x] 페인트 **SVG line=방향맞춘 quad + arc(A) 정확 평탄화**(F.6 중심 파라미터화). (d887cec, f7a093c)
- [ ] 레이아웃 **인라인 레벨 SVG 미배치** — width/height 속성은 블록일 때만 반영(mod.rs:255).
  기본 display 의 `<svg>`(인라인)는 크기/렌더 안 됨. display:block/inline-block 필요.
  인라인 대체요소(img/inline-block/svg) 전반 문제와 동류(mod.rs 인라인). (검증 중 발견)
- [x] 페인트 **grayscale/saturate BT.709**(이미 709 계수 사용, b957a7d 에서 반영됨). 확인 완료.
- [x] CSS **:where/:is/:not 명시도 정확 계산**(:where=0, :is/:not=인자 최대). (5e77316, 중복 항목)
- [x] CSS **무단위 line-height 배수(Lh)로 상속** — 요소별 font-size 곱. %/길이는 길이 상속. (69f728a)
- [x] CSS **calc() em/rem/vw 단위별 계수 보존 후 style 에서 px 확정**. (5951826)
- [x] CSS **@supports 과다보고** — 2단계로 해소: 프로퍼티 이름 검증(535f98d 이전) +
  값 검증(31bfe54). 열거형 미구현 값(position:sticky, display:table-cell/flow-root)과
  미구현 값 함수(color-mix/oklch/env/transform:rotate)를 이제 거짓으로 보고한다.
- [x] CSS **:not/:is/:where 가 첫 compound 만 보고 결합자를 버림** — 이제 인자를 전체
  복합 선택자(Vec<Selector>)로 파싱하고, 앵커(DOM 위치)로 조상/형제까지 정확히 매칭
  (element_matches 재사용). :is(.a .b)/:where(.x > .y)/:not(.p .q) 동작. 특이도도 전체
  선택자 기준.
- [x] CSS **속성선택자 i/s 플래그 + 기본 대소문자 구분**. (4a38252)
- [x] CSS **상속 화이트리스트에 word-break/overflow-wrap/word-wrap 추가**(소비되나 미상속이던 것). (6885dd8)
- [x] JS **instanceof** — function 생성자/Object.create 체인/내장/원시값 모두 정확(프로토타입 링크로 해소, 1899093). 확인 완료.
- [x] JS **인스턴스 Object.prototype 폴백**(hasOwnProperty/toString/valueOf 등). (6a8dc70)
- [~] JS **정규식 named group (?<n>) 지원**(번호/.groups/치환). 룩비하인드는 명시적 에러. step-limit 은 후속. (a95bcea)
- [x] JS **제너레이터 지연 실행** — 재개가능 인터프리터(제어흐름 위치 저장/복원)로 중단·재개.
  무한 제너레이터/양방향 next(v)/yield* 위임/return·throw/try 재개. yield 없는 문장은 기존
  평가기 재사용(의미론 동일). 디슈가 패스로 식 내부 yield(`a+(yield b)`, `f(yield x)`,
  삼항/단락평가)까지 완전 지원 — 평가순서·단락평가·메서드 this 보존. (generator.rs)
- [x] JS **Symbol 진짜 원시값** — 가짜 객체(__key) 폴리필 제거. typeof 'symbol', 고유성/
  Symbol.for/keyFor, 계산 프로퍼티 키(멤버·객체·클래스 메서드), 비열거. 반복자 프로토콜
  일반화로 사용자 정의 [Symbol.iterator] 이터러블(for-of/스프레드/Array.from) 지원. (generator.rs/mod.rs)
- [~] JS **Date.parse/Date.UTC 구현(4568092) + JSON toJSON(ISO, 25aa6fd)** 완료. UTC전용(로컬시간대 미구현)은 후속.
- [x] JS **문자열 UTF-16 코드 유닛**(length/charAt/charCodeAt/codePointAt/indexOf/slice/[i]/search/for-in).
  반복·스프레드는 코드 포인트. 짝없는 서로게이트만 U+FFFD(Rust String 한계). (2014792)
- [~] JS **엔진 내부 마커 비열거 + Date toJSON(ISO)** 완료. promise 메서드도 비열거(프로토타입 격).
  JSON replacer/space 는 후속. (25aa6fd)
- [~] JS **Number→문자열 ECMAScript 7.1.12.1**(지수 임계 n>21/n≤-6, "de+X"). toFixed 는 후속. (5e9c022)
- [~] JS **정규식 vs 나눗셈: 제어문 헤더 `)` 뒤 정규식 허용**(if(x)/re/). 그룹/호출 `)`는 나눗셈. `}` 뒤는 후속(블록/객체 구분). (289b7b8)
- [~] JS **클래스 제너레이터(*)/async 메서드** 지원. 계산된 이름[expr]/객체리터럴 메서드는 후속(동적키 필요). (ccc73f8)
- [x] JS **레이블 break/continue + 레이블 문**(중첩 루프 탈출, 레이블 블록 break). (e806035)
- [x] JS **유니코드 식별자**(ID_Start≈is_alphabetic, ID_Continue≈is_alphanumeric). (9f21ee7)
- [x] JS **Map/Set SameValueZero(c090180) + const 재대입 금지(bfbd894) + 네이티브함수 ===(1003a26)** 완료. typeof Symbol('symbol') 도 진짜 원시값으로 완료.

## DOM/렌더 상호작용

- [x] **getComputedStyle 빈 스텁 → 실제 계산 스타일**. 스타일 엔진의 해석값을 브리지,
  카멜·대시·getPropertyValue, width/height 는 레이아웃 used value(px). 프렐류드 스텁 제거.
  (mod.rs/window.rs/style.rs)
- [x] **측정 API 가 강제 레이아웃을 흘린다(CSSOM View)** — 스크립트가 첫 레이아웃보다 먼저
  전부 실행돼서, 파싱 중이나 load 에서 잰 값이 항상 0/빈 문자열이었다. 이제 CSS·폰트·
  이미지가 준비된 뒤 스크립트가 돌고(HTML 표준의 script-blocking stylesheet 순서),
  getBoundingClientRect/offset*/client*/scroll*/getComputedStyle 은 읽는 순간 보류된
  스타일·레이아웃을 흘린다. Dom 에 변형 카운터(version)를 두어 깨끗하면 재계산하지 않는다.
  (aef2963)
- [x] **getComputedStyle 이 미설정 프로퍼티에 빈 문자열** → CSS 명세의 초기값/상속값.
  `getComputedStyle(el).position === 'static'` 류 검사가 전부 실패하던 문제. (881d199)

## 2라운드 감사 — "있는 척만 하던" API (2026-07-13)

감사 방법: 어서션 HTML 페이지를 만들어 헤드리스 렌더 → [console] 출력 확인.
주석이나 이 문서를 믿지 않고 매번 실제로 측정했다(이 문서 자체가 여러 번 스테일이었다).

- [x] **matchMedia 가 항상 matches:false** — CSS 는 @media (min-width:768px) 를 참으로
  보고 데스크톱 규칙을 적용하는데 JS 는 거짓을 돌려줬다. 한 엔진 안에서 CSS 와 JS 의
  답이 달랐다. 이제 같은 평가기(media_matches_vp)로 실제 뷰포트에 대해 판정. (1565eb0)
- [x] **IntersectionObserver 무동작 스텁** — 콜백이 영영 오지 않아, 교차 시 콘텐츠를
  드러내는 사이트가 화면 안 요소까지 opacity:0 로 남았다. 이제 레이아웃의 실제 사각형으로
  뷰포트 교차를 계산하고 observe() 직후 초기 관측을 비동기 1회 전달. (1565eb0)
- [x] **ResizeObserver 무동작** — 표준은 observe() 시 현재 크기로 초기 관측을 준다. (1565eb0)
- [x] **MutationObserver 무동작** — "요소가 나타나면 처리" 패턴이 통째로 죽었다.
  DOM 아레나가 childList/attributes/characterData 기록을 쌓고(속성 쓰기는 set_attr/
  remove_attr 로 일원화), 첫 기록에서 마이크로태스크 배달을 1회 예약한다. subtree/
  attributeFilter 필터링. (9ebdcd2)
- [x] **el.removeEventListener 메서드 자체가 없음** → TypeError 로 스크립트 전체가 죽었다.
  document/window/XHR 의 removeEventListener 는 Noop 스텁이라 "제거했다"고 믿는 코드에서
  핸들러가 계속 발화. xhr.addEventListener 는 "요소 메서드"라며 던졌다. EventTarget 을
  객체 수신자까지 일반화하고 참조 동일성으로 제거. document/window.dispatchEvent 추가.
  (cc702b1)
- [x] **input.checked/select.value 등 폼 상태가 undefined/""** — `if (cb.checked)` 가 늘
  거짓. select.value 는 선택된 option 의 값(없으면 첫 option), option.value 는 value 속성
  없으면 텍스트(HTML 표준). selectedIndex/options/불리언 속성 반사. (35fcdca)
- [x] **insertAdjacentHTML/insertAdjacentElement 없음** → TypeError. (35fcdca)
- [x] **window.scrollTo/scrollBy/scrollIntoView 없음** → TypeError. scrollY 는 0 고정.
  이제 실제 스크롤 상태를 바꾸고 헤드리스 렌더도 그 위치에서 그린다. (35fcdca)
- [x] **history.pushState/replaceState 가 no-op** — SPA 라우터가 pushState 후 읽는
  location.pathname 이 그대로였다. 이제 상대 URL 을 결합해 location 을 갱신. (35fcdca)
- [x] **fetch/xhr.open 이 상대 URL 을 못 씀** — fetch('/api/x') 가 Url(NoScheme) 로 실패.
  SPA 는 거의 다 상대경로로 부른다. 문서 URL 기준으로 절대화. (35fcdca)
- [x] **<template>.content 없음** / **DOMParser 없음**. (35fcdca)
- [x] **display: contents 미구현** — 미지원 값이라 block 으로 떨어져 없어야 할 박스가
  생기고, 부모가 flex/grid 여도 자식이 아이템이 되지 못했다. (31bfe54)
- [x] **대문자 타입 선택자(`DIV SPAN`)가 아무것도 매칭 안 함** — HTML 타입 선택자는 ASCII
  대소문자 구분이 없다(선택자 표준 §6.1). (881d199)

## 3라운드 감사 — CSS 값/단축, 레이아웃, 페인트 (2026-07-13)

감사 방법은 같다: 어서션 페이지를 렌더해서 [console] 확인, 페인트는 PPM 픽셀 직접 probe.

- [x] **단축 프로퍼티 파서가 괄호를 무시** — 이번 감사 최대어. 값을 split_whitespace()/
  split(',') 로 잘라서 함수 인자 안의 공백·콤마까지 구분자로 봤다:
  `background: rgb(1,2,3)` → "rgb(1" 로 잘려 배경이 아예 안 칠해지고,
  `border: 1px solid rgba(0, 0, 0, .1)` → 색이 통째로 사라졌다. rgba(…, .1) 콤마+공백
  표기는 실제 사이트에서 압도적으로 흔하다. split_top_level 을 슬라이스 반환으로 바꿔
  단축 파서 13곳을 전부 교체. background 레이어는 괄호 밖 콤마로만 분리. (f60bd7c)
- [x] **단위 없는 수를 길이(px)로 저장** — opacity/z-index/order/flex-grow/flex-shrink 를
  Length(n, Px) 로 실어서 getComputedStyle 이 "0.5px" "5px" "1px" 을 돌려줬다.
  Unit::Number 도입. (657ee61)
- [x] **font-weight: inherit 이 상속을 끊음** — 파싱 실패로 보고 "normal" 로 눌렀다.
  react.dev 의 리셋 CSS 가 실제로 이걸 써서 굵어야 할 글자가 가늘게 나왔다.
  계산값도 표준대로 수(bold=700). (657ee61)
- [x] **중첩 calc()** — `calc(50% + calc(10px * 2))` 가 선언째 버려졌다. (657ee61)
- [x] **color: currentColor 자기참조** — 표준상 inherit. 키워드가 미해석으로 남았다. (657ee61)
- [x] **rgba 알파 직렬화 잡음** — 8비트 모델이라 0.5 가 "0.502" 로 샜다. (657ee61)
- [x] **인라인 요소에 박스가 없음** — span/a/b/em 의 getBoundingClientRect 가 전부 0,
  <span onclick> 도 발화 안 함(히트 목록에 없어서). 표준의 인라인 박스 = 조각들의
  경계 합집합. 인라인 조상 체인 전부에 조각을 적립해 합집합. (3ad7984)

### 정상 확인 (요행 없음 — 렌더/픽셀로 검증)
- HTML 파서 오류 복구 10/10: 미닫힌 b/i, tbody 암묵 삽입, li/option 암묵 닫힘,
  엔티티(&amp; &#65; &nbsp; &#x42;), void 요소.
- z-index 스태킹 5/5 (픽셀 probe): 문서 순서, z-index 순서, positioned vs in-flow,
  opacity 가 만드는 스태킹 컨텍스트, 음수 z-index.
- 테이블: colspan/rowspan/폭 분배 정상.
- CSS 캐스케이드/선택자 16/16: 특이도, !important vs 인라인, 자식/인접/일반형제,
  :nth-child/:first/:last, 속성 선택자(=, ^=), :not, var/calc, 상속, 동일특이도 순서.
- CSS 값 25/25: hex 3/6, rgb/rgba/hsl/hsla, 명명색, transparent, %/vw/rem/pt/in,
  calc/min/clamp, var 폴백, 대문자 값·프로퍼티명, !important 공백 변형,
  rgb(a b c / d) 공백 표기.

### 남은 격차 (요행이 아니라 미구현 — 정직하게 보고 중)
- woff/woff2 웹폰트 디코더 없음 (ttf/otf 만). 모던 사이트는 woff2 만 싣는 곳이 많다.
- Shift_JIS/GBK/Big5 문자셋 테이블 (감지는 하고 Unsupported 로 보고).
- GIF/WebP 이미지 디코더 (PNG/JPEG 만).
- 타입드 배열(Uint8Array/ArrayBuffer), Intl, import.meta.
- position: sticky (@supports 는 이제 거짓으로 보고 → 사이트가 폴백을 준다).
- MutationObserver oldValue (attributeOldValue/characterDataOldValue).

## 저심각 (스킵 가능)
epsilon fudge, 곡선 고정분할, accent-color, 등.

## 2026-07 라운드 (실제 사이트를 어서션 페이지로 훑어 찾은 것)

방법: 브라우저에 T(name, actual, expected) 어서션 페이지를 띄워 렌더하고 [console] 을
읽는다. 픽셀은 PPM 을 직접 뒤진다. **주석·문서를 믿지 않는다** — 코드가 실제로
무엇을 하는지만 본다.

### CSS
- [x] **다중 토큰 값이 선언째 사라짐** — 일반 값 파서가 토큰 2개 이상이면 None 을 돌려주고
  선언이 통째로 버려졌다. `overflow: hidden auto`, `gap: 10px 20px`, `border-spacing: 2px 4px`,
  `background-size: 50% 25%`, `transform-origin: 0 0` 이 전부 조용히 무시됐다.
  `flex-flow` 는 아예 아무도 읽지 않았다. (d2e24f0)
- [x] **transform 은 translate 만 진짜였다** — rotate 는 페인트에서 아이템별 근사,
  skew/matrix 는 무시. 2D 아핀 행렬로 합성해 서브트리를 통째로 변환하도록 재작성.
  @supports 도 이제 3D 만 거짓으로 답한다. (b16ed6b)
- [x] **익명 박스가 부모 스타일을 공유** → absolute/sticky/transform 이 이중 적용.
  `position:absolute` 요소의 글자가 좌표 2배 위치에 그려졌다. (b16ed6b)
- [x] **initial/revert 미구현** — initial 은 "선언 삭제"로 처리해 상속 속성이 부모값을
  물려받았고(color: initial 이 검정이 아님), revert 는 키워드 문자열이 그대로 값이 됐다
  (display: revert). 원점(UA/저자)을 구분해 되돌린다. (e96059d)
- [x] **background 단축의 `/` 뒤 크기가 나머지를 삼킴** — url() 과 색이 통째로 사라졌다. (75c52d1)
- [x] **white-space: pre-wrap 이 절대 줄바꿈 안 됨** — 보존한 공백을 단어에 이어 붙여
  줄 전체가 한 단어가 됐다 (= pre 와 동일). (3231042)

### JS
- [x] **대입의 평가 순서** — 표준은 왼쪽 참조 먼저인데 오른쪽을 먼저 평가했다.
  jQuery 가 `(b = se.selectors = {…}).pseudos.nth = b.pseudos.eq` 로 그 순서에 의존해서
  jquery.js 가 통째로 죽었다 ($ 자체가 정의되지 않음). (db669ea)
- [x] **ESM 네임스페이스가 살아있는 뷰가 아님** — 본문 실행 후에 채워서, 자기 자신을
  import 하는 모듈(rspack/webpack 청크)이 자기 export 를 못 봤다. MDN 메인 모듈이 죽었다. (366bed1)
- [x] **옵셔널 체인 호출이 단락 안 됨** — `a?.m()` 이 a=null 이어도 호출을 시도해 죽었다. (b0015b0)
- [x] class static {} 블록에서 파서가 죽어 **스크립트 전체**가 날아갔다. (3a5340c)
- [x] super.x **읽기**, in 연산자의 프로토타입 체인, Proxy has/deleteProperty/ownKeys,
  단항 ToPrimitive, Symbol.toPrimitive, JSON replacer/indent. (3a5340c)
- [x] class extends 뒤 **호출식**(믹스인)에서 파서가 죽었다. (a83bbbe)
- [x] **BigInt 를 f64 로 근사** — 렉서가 n 접미를 버렸다. 2n**64n 이 조용히 틀렸다.
  임의 정밀도 정수로 구현(혼합 산술은 TypeError). (31091ee)
- [x] 정규식 **룩비하인드 미지원**, 룩어라운드 안의 캡처를 버림. (c9ef19d)
- [x] JS 호출 스택이 없어 오류 위치를 알 수 없었다 → 프레임 추적 추가. (366bed1)

### HTML / 이미지 / DOM
- [x] **script nomodule 을 실행** — 모듈 지원 브라우저는 실행하면 안 된다(HTML §4.12.1).
  react.dev 의 레거시 폴리필 번들이 최신 코드와 충돌해 React 훅 리스트를 깨뜨렸다. (a1497c5)
- [x] **focus()/blur()/activeElement 없음** — el?.focus() 가 "함수 아님" 으로 죽었다. (b0015b0)
- [x] **new URL(location)** 이 ToString 을 안 거쳐 "[object Object]" 를 파싱했다. (b0015b0)
- [x] **CSS url(*.svg) 미지원** — <img src=*.svg> 는 DOM 치환으로 그렸지만 배경은 못 그렸다.
  SVG 를 실제 픽셀로 래스터화. 겸사겸사 emit_svg 가 <g> 그룹을 순회하지 않던 것도 고침
  (실제 SVG 는 거의 다 <g> 로 감싼다). (5d8d9db)
- [x] **그레이스케일 JPEG 디코드 실패** — 1채널 스캔은 비인터리브인데(T.81 §A.2.2)
  인터리브로 취급해 4블록씩 읽었다. (5d8d9db)
- [x] **WebP 미지원** — 사이트가 .webp 를 하드코딩한다. VP8 lossy 디코더 구현(RFC 6386),
  참조 디코더와 픽셀 대조로 검증(평균오차 0.85). (1bf4b32)
- [x] **GIF 미지원** — HN 의 스페이서가 안 나왔다. LZW/팔레트/투명/인터레이스. (0ec61d9)
- [x] **임포트 맵 미지원** — 베어 명세자를 해석할 표준 메커니즘이 없었다. (0c0286e)
- [x] **CSSOM 값 직렬화** — el.style.color 를 원문 그대로 돌려줬다 (정규화 안 함). (0c0286e)
- [x] **window.getSelection 없음** — typeof 검사 후 부르는 코드가 죽었다. (0c0286e)

### 2026-07 3라운드 (fmkorea / naver 를 열어 추적)

- [x] **document.write 없음** (HTML §8.4.3) — 국내 포털·광고 스크립트가 통째로 죽었다. (08d7402)
- [x] **배열 콜백에 배열(3번째 인자)·thisArg 를 안 넘김** — a[i-1] 관용구가 죽는다
      (IntersectionObserver 폴리필). (08d7402)
- [x] **CSS url() 을 문서 URL 기준으로 해석** — 표준은 스타일시트 URL 기준. 배경이 404. (08d7402)
- [x] **background 단축의 슬래시 정규화가 url() 안 경로까지 벌림** → HTTP 400. (08d7402)
- [x] **모르는 charset 을 조용히 UTF-8 로** — WHATWG 단일바이트 26종 기계 추출로 구현. (08d7402)
- [x] **정적 setter 를 저장만 하고 호출 안 함** — Class.prop = v 가 검증을 우회. (08d7402)
- [x] **쿠키 없음** — document.cookie 가 빈 문자열 상수. HTTP 항아리 + Set-Cookie/Cookie 헤더. (0892b43)
- [x] **JS 내비게이션 무시** — location.href = "…" 가 문자열 대입. meta refresh 도. (0892b43)
- [x] **Date 에 setter 가 하나도 없음** — 쿠키 만료 계산이 죽는다. (0892b43)
- [x] **escape/unescape 없음** (Annex B). (0892b43)
- [x] **투명 webp 를 불투명하게 그림** — ALPH 무손실 알파를 못 읽어 알파를 버렸다.
      VP8L 디코더 구현, 참조 디코더와 **비트 정확** 일치. (8800888)
- [x] **이벤트 캡처 플래그를 통째로 버림** — 캡처 리스너가 타깃보다 늦게 불렸다. (9331787)
- [x] **이벤트 init 딕셔너리에서 detail/bubbles 만 베낌** — KeyboardEvent.key 가 사라진다. (9331787)
- [x] **getOwnPropertyDescriptor 가 폴리필** — 게터의 get 이 없고 enumerable 이 항상 true. (1cae077)
- [x] **defineProperty 의 enumerable 무시** — 숨긴 프로퍼티가 keys/for-in/JSON 에 샌다. (1cae077)

### 2026-07 4라운드 (naver 를 열어 추적 + 미감사 영역 훑기)

- [x] **naver 는 렌더가 끝나지도 않았다** (CPU 110초 → 메모리 폭주 SIGKILL). 원인 둘:
  - **Object.getPrototypeOf 가 거짓말**이었다 — __proto__ 링크가 없으면 무조건 null.
    평범한 객체·배열·인스턴스·제너레이터가 전부 null. (Reflect.getPrototypeOf 는 아예
    `return null` 상수 함수.) regenerator/babel 런타임이 이걸로 내장 이터레이터
    프로토타입을 캐낸다 — null 이면 무너진다. C.prototype 정체성도 흔들렸다(매번 새 객체).
  - **배열 길이 상한이 없었다** — core-js 의 Array.from({length: 2**32}) 가 40억 개
    할당을 시도했다. 표준은 RangeError. (180282b)
  → 110초 사망 → 4.6초 렌더. 단계별 계측(KESTREL_TIME=1)도 추가.
- [x] **keys/values/entries 가 배열을 돌려줬다** — 표준은 이터레이터. for-of 는 되니
  멀쩡해 보이지만 it.next() 를 직접 쓰는 코드(babel for-of 헬퍼/core-js)는 죽는다.
  strict_eq 가 제너레이터를 신원 비교 안 해 it[Symbol.iterator]() === it 도 거짓이었다. (f168cb0)
- [x] **el.dataset 이 스냅샷** — dataset.x = '1' 이 조용히 사라졌다. 살아있는 뷰로. (09cc66d)
- [x] **attributes 이름 접근 없음** (NamedNodeMap), **<a> 의 URL 분해 속성 없음**,
  **Blob/File/FileReader/createObjectURL 없음**. (09cc66d)
- [x] **localStorage.length/key 없음**, **fetch 응답에 headers/url 없음**,
  **URLSearchParams 가 이터러블이 아님**, **Storage 전역 없음**. (c47a34d)
- [x] **인라인 요소 블록화 안 함** (CSS Display §2.7) — position:absolute 인 <span> 의
  width/right 가 통째로 무시됐다. (8380bdb)
- [x] **다단이 자식 경계에서만 쪼갬** — 자식이 하나면(가장 흔한 모양) 전혀 안 나뉜다.
  줄 경계 박스 조각화로 다시 씀. (8380bdb)

## 라운드 5 — WebAssembly (bc4c2c2)

- [x] **WebAssembly 가 통째로 없었다.** 파서 + 스택 머신 + JS 바인딩을 새로 썼다(src/wasm.rs).
      검증은 rustc 로 컴파일한 진짜 모듈(1.5MB)을 V8(node)과 한 줄씩 대조 — fib / FNV 체크섬 /
      f64 평균 / i64(BigInt) / 메모리 왕복 / grow 후 옛 버퍼 분리까지 전부 동일.
- [x] **메모리를 사본으로 주면 조용히 틀린다.** wasm 선형 메모리를 JS ArrayBuffer 의
      바이트 배열과 **같은 배열**로 공유한다. 게다가 wasm 내부 memory.grow 는 배열을
      갈아끼우므로, JS 가 메모리를 볼 수 있는 모든 경계(호출 반환, 임포트 콜백 직전)에서
      buffer 를 다시 묶는다. 안 하면 grow 뒤로 wasm 이 쓴 값이 JS 에 **아예 안 보인다**.
- [x] **fetch 가 바이너리를 망가뜨렸다** — 본문을 lossy UTF-8 문자열로만 보관해
      (U+FFFD 로 덮어씀) wasm/이미지 바이트를 되돌릴 수 없었다. 원본 바이트 보존 +
      Response.arrayBuffer().
- [x] **ArrayBuffer 0 채우기가 JS 루프** — 1MB 버퍼면 100만 반복. new Uint8Array(1e6)
      조차 사실상 못 썼다. 네이티브로.
- [x] **타입 배열 length 가 박제** — 분리(detach)된 버퍼의 죽은 뷰가 살아있는 척했다.
      버퍼에서 파생하게 고쳤다 (wasm-bindgen 이 정확히 byteLength===0 으로 판별한다).
- [x] **i64 를 Number 로 주면 2^53 위에서 조용히 틀린다** → BigInt 로. 임포트 반환 타입은
      모듈 시그니처로 변환한다 (값의 모양으로 추측하지 않는다).
- [x] **ToInt32 가 포화(saturate)** — 표준은 2^32 랩. `n as i32` 가 범위 밖에서 포화한다.

## 라운드 6 — 어설션 페이지 감사 (33a0a3c … 0aabbe6)

브라우저 API/CSS 를 어설션 페이지로 훑어 찾은 것들. 전부 **조용히 틀리던** 것이다.

- [x] **filter/opacity 가 이미지에서만 무시됐다** — grayscale(1) 인 로고가 컬러로 나왔다.
- [x] **flex 가 cross 를 main 확정 전에 쟀다** (§9.4) — flex:1 + 텍스트라는 가장 흔한
      모양에서 컨테이너가 짜부라지고 stretch 가 틀린 높이로 늘렸다.
- [x] **CSSOM View 가 전부 테두리 박스 근사** — clientLeft 가 좌표를(테두리 두께여야 한다),
      scrollHeight 가 clientHeight 와 같아 "넘쳤나?" 검사가 **항상 거짓**이었다.
      offsetParent 는 아예 없었다. transform 은 원문 문자열(표준은 matrix(...)).
- [x] **innerText 가 textContent 별칭** — <script> 소스와 display:none 내용까지 돌려줬다.
- [x] **속성이 HashMap** — outerHTML/getAttributeNames 순서가 매번 달랐다 (DOM 은 순서 있는 목록).
- [x] **element.click() 이 없었다**, addEventListener 의 **once 를 무시**, eventPhase 없음,
      비버블 이벤트가 조상까지 올라감, input.type/form.method 의 IDL 기본값 없음.
- [x] **:has() 미지원** — 규칙이 조용히 사라졌다. 의사 클래스 인자를 첫 ')' 까지만 읽어
      :not(:has(.x)) 가 잘리던 파싱 버그도 함께.
- [x] **@layer / @container 미지원** — at-rule 을 스킵해 그 안의 규칙이 통째로 사라졌다.
      Tailwind v4 는 **모든 것**을 @layer 로 감싼다.
- [x] **@supports 가 subgrid 를 지원한다고 거짓말** (프로퍼티 이름만 보고 값을 안 봄).
- [x] **obj.m?.() 이 this 를 잃었다** — el.getAttribute?.('src') 가 죽었다.
- [x] **for await / 계산된 구조분해 키 파싱 실패** → 그 스크립트가 통째로 죽었다.
- [x] **document.currentScript 가 없었다** — 번들러 런타임이 청크 URL 을 못 구해 죽는다.
- [x] **스텝 한도가 시간이 아니라 스텝 수** — 무겁지만 정상인 번들도 잘렸다. 시간 예산
      (실행 단위 5초 / 페이지 전체 10초)으로. 낭비 레이아웃 2건도 제거 (측정으로 찾았다).

### 남은 것 (정직하게)
- [ ] **wasm SIMD (0xFD)** — 요즘 이미지·비디오 코덱 빌드가 쓴다. 지금은 CompileError 로
      정직하게 거절한다(조용히 틀리진 않는다). 다음 후보.
- [ ] **wasm 다중값 블록 타입 / 테이블 임포트** — 거절한다.
- [ ] **fmkorea 의 스크립트 하나는 60초를 줘도 끝나지 않는다.** 트리워킹 인터프리터가
      못 따라가는 작업량이거나 영영 참이 되지 않는 조건을 폴링하는 것 — 예산은 피해를
      가둘 뿐 고치지 못한다. **바이트코드 VM** 이 필요하다.
- [ ] naver 잔여 js 오류 3건 (사이트 자체 라이브러리 쪽). 본문은 클라이언트 렌더라
      앱이 완주해야 보인다.
- [ ] AVIF
- [ ] 3D transform (perspective/rotate3d) — @supports 는 거짓으로 답한다 (거짓말은 안 함)
- [ ] 임포트 맵의 scopes
- [ ] GIF 애니메이션 (첫 프레임만 그린다 — 정적 렌더에서는 의도된 동작)
