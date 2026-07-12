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
- [~] 테이블 **auto 폭 shrink-to-fit(§17.5.2) + border-spacing** 완료 — 작은 표는 내용 폭,
  separate 표는 셀 간격. border-collapse 테두리 중첩, rowspan 높이 분배는 후속. (mod.rs)
- [ ] 페인트 **둥근+투명배경=사각 테두리**. (paint.rs:1384)
- [x] 페인트 **점선/파선 테두리**(dashed/dotted). double/groove 는 근사. (7bbcf70)
- [x] 페인트 **그라디언트 프리멀티플라이 보간**(gradient_color_at 이미 반영, b957a7d). 확인 완료.
- [x] 페인트 **박스섀도 가우시안(erf 전이) + 필터/backdrop 블러 3패스 가우시안**. (bdc27e9, 2d31c45)
- [x] 페인트 **방사그라디언트 ellipse(기본)/circle 구분** — 축별 반경. 크기/위치는 아직 근사. (1d402ff)
- [x] 페인트 **overflow 사각클립이 글리프/폴리곤 픽셀클립** — 경계 걸치면 사각 ClipShape 로 래핑. (e622fc2)
- [x] 페인트 **폴리곤 AA**(세로 서브스캔라인 4 + 가로 부분커버리지). (bc07013)
- [x] 페인트 **이미지 바이리니어 스케일링**(프리멀티플라이, 투명가장자리 안전). 타일은 최근접 유지. (d15516c)
- [ ] 페인트 **select 화살표 14px/progress·meter 하드코딩**. (paint.rs:1344,1329)
- [x] 페인트 **SVG line=방향맞춘 quad + arc(A) 정확 평탄화**(F.6 중심 파라미터화). (d887cec, f7a093c)
- [ ] 레이아웃 **인라인 레벨 SVG 미배치** — width/height 속성은 블록일 때만 반영(mod.rs:255).
  기본 display 의 `<svg>`(인라인)는 크기/렌더 안 됨. display:block/inline-block 필요.
  인라인 대체요소(img/inline-block/svg) 전반 문제와 동류(mod.rs 인라인). (검증 중 발견)
- [x] 페인트 **grayscale/saturate BT.709**(이미 709 계수 사용, b957a7d 에서 반영됨). 확인 완료.
- [x] CSS **:where/:is/:not 명시도 정확 계산**(:where=0, :is/:not=인자 최대). (5e77316, 중복 항목)
- [x] CSS **무단위 line-height 배수(Lh)로 상속** — 요소별 font-size 곱. %/길이는 길이 상속. (69f728a)
- [x] CSS **calc() em/rem/vw 단위별 계수 보존 후 style 에서 px 확정**. (5951826)
- [ ] CSS **@supports 값검증 없이 과다보고**. (css/supports.rs:47)
- [ ] CSS **:not/:is 첫 심플셀렉터만**. (css/mod.rs:796)
- [x] CSS **속성선택자 i/s 플래그 + 기본 대소문자 구분**. (4a38252)
- [x] CSS **상속 화이트리스트에 word-break/overflow-wrap/word-wrap 추가**(소비되나 미상속이던 것). (6885dd8)
- [x] JS **instanceof** — function 생성자/Object.create 체인/내장/원시값 모두 정확(프로토타입 링크로 해소, 1899093). 확인 완료.
- [x] JS **인스턴스 Object.prototype 폴백**(hasOwnProperty/toString/valueOf 등). (6a8dc70)
- [~] JS **정규식 named group (?<n>) 지원**(번호/.groups/치환). 룩비하인드는 명시적 에러. step-limit 은 후속. (a95bcea)
- [ ] JS **제너레이터 즉시 전체평가**. (mod.rs:2784)
- [ ] JS **객체리터럴 계산 Symbol 키 불일치**(for-of 사용자 이터러블 안됨). (mod.rs:1962)
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
- [~] JS **Map/Set SameValueZero(c090180) + const 재대입 금지(bfbd894) + 네이티브함수 ===(1003a26)** 완료. typeof Symbol 은 후속.

## 저심각 (스킵 가능)
epsilon fudge, 곡선 고정분할, accent-color, 등.
