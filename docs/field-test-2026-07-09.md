# 실전 테스트 라운드 1 — 2026-07-09

방법: 실사이트 8곳을 헤드리스 렌더 (`KESTREL_RENDER_TO`), 로그에서
js error / 문서 높이 / 링크 수 / 이미지 디코드 / 시간 수집.
목적: 다음 마일스톤 우선순위를 추측이 아니라 데이터로 정하기.

## 결과 (수정 전 → 퀵윈 3건 적용 후)

| 사이트 | 전 | 후 | 비고 |
|---|---|---|---|
| example.com | 정상 | 정상 | 기준점 |
| news.ycombinator.com | **0px, 링크 0** | **1520px, 링크 516** | 뉴스 목록 전체 읽힘 |
| MDN (JS 문서) | **0px, 링크 0** | **3856px, 링크 446** | 본문 렌더 |
| en.wikipedia (kestrel) | 7484px, 에러 4 | 8197px, 에러 2 | 테이블 셀 추가 렌더 |
| naver.com | 320px, 에러 8 | 320px, 에러 4 | JS 렌더 사이트 한계 |
| github.com | 208px, 에러 5 | (동일 계열) | Colon 파스 에러 5 |
| google.com | **fetch 실패** | 동일 | TLS close_notify 미수신 |
| info.cern.ch | 타임아웃 | 동일 | 서버 측 문제로 보임 |

크래시/패닉: **0** (관용 원칙 유지). 로드 시간 1~4초 (이미지 병렬 후).

## 근본 원인 2건 (0px 렌더)

1. **미지 display 값 → 인라인 폴백**: MDN 은 CSS 가 `body { display: grid }`.
   grid 를 몰라 Inline 처리 → body 가 인라인 → 인라인 수집기는 블록 자식을
   버림 → 페이지 전체 소멸. 수정: 미지 display 키워드는 Block 으로
   (inline/inline-block/inline-flex 만 인라인).
2. **UA 에 없는 컨테이너 요소**: HN 은 `<center><table>...`. center 가
   인라인 → 같은 경로로 테이블 전체 소멸. 수정: center/td/th/tbody 등
   테이블 계열을 UA block 에 추가 (진짜 테이블 레이아웃 전까지 세로 렌더).

구조적 빈틈 (미해결, 기록): **인라인 요소 안의 블록 자식은 여전히 버려진다**
(block-in-inline 분할 미구현). 위 두 수정으로 빈도는 크게 줄었지만
`<a><div>...</div></a>` 패턴은 여전히 사라짐.

## JS 에러 분류 (빈도순)

1. ~~`window 미정의`~~ → 전역 스텁 추가로 해소 (전역 연동은 없음, 문서화)
2. `식이 필요한데 Colon/Comma` (github ×5, naver, wiki) — switch/case,
   객체 메서드 단축(`{ foo() {} }`), 레이블 등 의심. **파서 갭 1순위**
3. `알 수 없는 문자 '\`'` (naver) — **템플릿 리터럴** (현대 JS 필수)
4. `알 수 없는 문자 '^'` (wiki, naver) — **비트 연산자** (^ & | << >> ~)
5. `try 미정의` (MDN) — **try/catch 미지원**: try 가 식별자로 파싱됨.
   실코드 어디에나 있음

## 다음 마일스톤 우선순위 (데이터 기반)

1. try/catch (+ throw) — 파서/인터프리터. 없으면 첫 줄에서 죽는 사이트 다수
2. 템플릿 리터럴 (보간 포함) — 렉서/파서
3. 비트 연산자 — 렉서/파서/인터프리터 (작음)
4. switch/case — Colon 에러 가설 검증 겸
5. TLS close_notify 관용 (google 전체가 안 열림) — http.rs
6. block-in-inline 분할 — 레이아웃 구조 작업 (별도 마일스톤)
7. 이미지: SVG(대량)/프로그레시브 JPEG/WebP — github 23중 1개만 디코드

## 이번 라운드에 적용한 퀵윈 (같은 커밋)

- style: 미지 display → Block 폴백
- UA: table 계열 + center + fieldset/hr/select/textarea block 추가
- js: window 전역 스텁

158 tests pass.
