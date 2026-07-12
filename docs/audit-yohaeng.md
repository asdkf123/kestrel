# 요행(luck-based) 코드 감사 — 표준 기반으로 교체 대상

6개 서브시스템 병렬 감사 종합. "테스트엔 통과하지만 표준 메커니즘이 아니라
우연/땜빵으로 맞아떨어져 실제 유효 입력에서 깨지는" 코드 목록. 심각도·빈도순.

상태: [ ] 미착수  [~] 진행중  [x] 완료

## 구조적 뿌리 (크고, 여러 곳에 영향)

- [ ] **JS 객체 프로퍼티 순서** — HashMap 백엔드라 for-in/Object.keys 순서 랜덤,
  JSON.stringify 는 정렬로 가림. 삽입 순서(정수키 먼저) 보장 필요. (interp/mod.rs:441, value.rs:645)
- [ ] **JS 프로토타입 링크** — new F() 가 prototype 을 스냅샷 복사(링크 아님).
  instanceof, 인스턴스 상속, constructor 다 깨짐. (interp/mod.rs:2867)
- [ ] **JS ToPrimitive** — 강제변환 시 toString/valueOf 안 부름. `${obj}`→[object Object]. (value.rs:421, mod.rs:3018)
- [ ] **JS 진짜 Promise/async** — new Promise 미구현, reject/.catch/await-pending 삼킴. (mod.rs:2882, 2042)
- [ ] **float in nearest-BFC** — float 이 직속 부모에 갇힘. 다중 float·타 블록 우회 불가. (layout/mod.rs:1049)

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
- [ ] 레이아웃 **부모-자식/빈블록 margin 상쇄 없음**. (mod.rs:2033)
- [ ] 레이아웃 **인라인 텍스트 내 img/inline-block 무시** (인라인박스 안 만듦). (inline.rs:631)
- [ ] 레이아웃 **리스트마커·밑줄·폼컨트롤 크기 마법상수**. (mod.rs:371, inline.rs:434)
- [ ] 레이아웃 **max-height 가 overflow clip 시에만 적용**. (mod.rs:1598)
- [ ] 레이아웃 **인라인 테두리 3px 하드코딩 패딩** (내 코드). (inline.rs:477)
- [ ] Grid **정렬 전부 무시**(place-items 등), 명시배치/span 무시, template-rows 무시, minmax min 버림. (grid.rs)
- [ ] Flex **shrink 0까지, min-content 무시, min/max 덮어씀, align-content 없음**. (flex.rs:132,184)
- [ ] 테이블 **auto 폭 알고리즘 근사(항상 컨테이너 채움), border-collapse/spacing 미구현, rowspan h/n**. (mod.rs:1407)
- [ ] 페인트 **둥근+투명배경=사각 테두리**. (paint.rs:1384)
- [x] 페인트 **점선/파선 테두리**(dashed/dotted). double/groove 는 근사. (7bbcf70)
- [ ] 페인트 **그라디언트 프리멀티플라이 아님**(투명 페이드 탁함). (paint.rs:523)
- [ ] 페인트 **박스섀도 선형(가우시안 아님), 필터블러 박스1패스**. (paint.rs:310,119)
- [x] 페인트 **방사그라디언트 ellipse(기본)/circle 구분** — 축별 반경. 크기/위치는 아직 근사. (1d402ff)
- [ ] 페인트 **overflow 사각클립이 글리프 픽셀클립 안함**. (paint.rs:1530)
- [x] 페인트 **폴리곤 AA**(세로 서브스캔라인 4 + 가로 부분커버리지). (bc07013)
- [x] 페인트 **이미지 바이리니어 스케일링**(프리멀티플라이, 투명가장자리 안전). 타일은 최근접 유지. (d15516c)
- [ ] 페인트 **select 화살표 14px/progress·meter 하드코딩**. (paint.rs:1344,1329)
- [~] 페인트 **SVG line=방향맞춘 quad(대각선 정확)** 완료. arc=현 근사는 후속. (d887cec)
- [ ] 레이아웃 **인라인 레벨 SVG 미배치** — width/height 속성은 블록일 때만 반영(mod.rs:255).
  기본 display 의 `<svg>`(인라인)는 크기/렌더 안 됨. display:block/inline-block 필요.
  인라인 대체요소(img/inline-block/svg) 전반 문제와 동류(mod.rs 인라인). (검증 중 발견)
- [ ] 페인트 **grayscale/saturate BT.601(스펙 709)**. (paint.rs:1937)
- [ ] CSS **:where/:is/:not 명시도 오류**. (css/mod.rs:180)
- [x] CSS **무단위 line-height 배수(Lh)로 상속** — 요소별 font-size 곱. %/길이는 길이 상속. (69f728a)
- [x] CSS **calc() em/rem/vw 단위별 계수 보존 후 style 에서 px 확정**. (5951826)
- [ ] CSS **@supports 값검증 없이 과다보고**. (css/supports.rs:47)
- [ ] CSS **:not/:is 첫 심플셀렉터만**. (css/mod.rs:796)
- [x] CSS **속성선택자 i/s 플래그 + 기본 대소문자 구분**. (4a38252)
- [x] CSS **상속 화이트리스트에 word-break/overflow-wrap/word-wrap 추가**(소비되나 미상속이던 것). (6885dd8)
- [ ] JS **instanceof 하드코딩표**(new F, Date, Map 다 false). (mod.rs:3042)
- [ ] JS **인스턴스 Object.prototype 폴백 없음**. (mod.rs:2635)
- [ ] JS **정규식 named group/lookbehind 미지원, step-limit 무음 no-match**. (regex.rs:226,530)
- [ ] JS **제너레이터 즉시 전체평가**. (mod.rs:2784)
- [ ] JS **객체리터럴 계산 Symbol 키 불일치**(for-of 사용자 이터러블 안됨). (mod.rs:1962)
- [ ] JS **Date UTC전용/parse·UTC 없음/JSON 내부누출**. (builtins.rs:769)
- [ ] JS **문자열 UTF-16 아님**(astral length). (mod.rs:2545)
- [ ] JS **JSON replacer/space/toJSON 무시, __marker 누출**. (value.rs:628)
- [ ] JS **Number→문자열 Rust규칙**(1e21, toFixed 반올림). (value.rs:7)
- [ ] JS **정규식 vs 나눗셈 `)`/`}` 뒤 오판**. (lexer.rs:174)
- [ ] JS **계산된/제너레이터/async 멤버명 미지원**. (parser.rs:1176)
- [ ] JS **레이블 break/continue 무시, 문 종료 강제 안함**. (parser.rs:142,232)
- [ ] JS **유니코드 식별자 미지원**. (lexer.rs:454)
- [ ] JS **strict_eq 동일 네이티브함수 false, Map/Set NaN, const 재대입 허용, typeof Symbol=object**. (value.rs:470)

## 저심각 (스킵 가능)
epsilon fudge, 곡선 고정분할, accent-color, 등.
