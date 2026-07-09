# M5a: 폼 입력 — 설계

날짜: 2026-07-09. 상태: 승인 (사용자 "진행해")

## 목표

검색창이 동작한다: `<input>` 렌더 → 클릭 포커스 → 타이핑 → Enter 로
GET 폼 제출 → 결과 페이지 이동. 데모: 로컬 폼 + 실사이트 검색 시도.

## 설계

- 레이아웃: input 은 대체 요소 박스. 폭 = CSS width > size 속성(×0.55em)
  > 기본 180px. 높이 = font-size × 1.5. value 속성 텍스트를 글리프로.
  type=hidden 은 0 크기 (구글 폼에 다수)
- 페인트: 외곽선(회색 rect) + 내부(흰 rect) + value 글리프.
  캐럿은 창이 래스터 후 오버레이 (포커스 시, value 폭 측정해 끝에)
- 상태: 값은 DOM 의 value 속성이 단일 진실 (JS el.value 와 공유).
  Page.focused_input: Option<NodeId>
- 입력: 클릭한 요소가 input 이면 포커스. 타이핑 → value 속성 수정 + rebuild.
  Backspace 삭제, Escape 포커스 해제. 스크롤 키(Space 등)보다 입력 우선
- 제출: Enter → 조상 form 의 input[name] 들을 수집해 쿼리스트링
  (percent 인코딩, 공백 +) → action 해석(빈 action = 현재 URL) → GET 이동
  (링크 내비게이션과 동일 경로: 히스토리/주소창 갱신). POST 는 미지원(무시)
- JS: el.value 읽기/쓰기 ↔ value 속성
- UA: input { display: block } 추가

## 검증

단위: 레이아웃 크기/hidden, 제출 URL 빌드(인코딩/action 해석/다중 필드),
타이핑 시뮬레이션(Page 메서드), JS value 왕복.
E2E: 로컬 폼 페이지 타이핑+제출 → 이동. 창에서 실검색 수동 확인.
