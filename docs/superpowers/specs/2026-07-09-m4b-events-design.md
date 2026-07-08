# M4b: 이벤트 (클릭) — 설계

날짜: 2026-07-09
상태: 승인됨 (M4a 후속, 사용자 "ok")

## 목표

클릭하면 페이지가 반응한다. 완료 기준 데모: 버튼 클릭 → 핸들러 실행 →
DOM 변형 → 재레이아웃/재렌더로 화면 갱신 (카운터 데모).

## 핵심 구조 변화

Page 가 렌더 산출물만 들던 것에서, 원본(dom/stylesheet/이미지 맵/JS 런타임)을
소유하고 `rebuild()` 로 산출물(디스플레이 리스트/링크/요소 히트 영역/문서 높이)을
재생성하는 구조로. 스타일/레이아웃 트리는 rebuild 안에서만 사는 일시 산물 —
borrow 가 밖으로 나가지 않아 아레나 리팩터링 없이 재렌더 가능.

## 핸들러 등록 (3가지 경로)

- `el.onclick = fn` → dom_set 이 (경로, "click", Fn) 을 Interp.handlers 에 등록
- `el.addEventListener(type, fn)` → Native 디스패치로 동일 등록
- `<button onclick="...">` 속성 → 디스패치 시점에 소스 평가 (등록 불필요)

## 디스패치 (버블링)

- rebuild 가 요소 히트 영역 (border box, DOM 경로) 목록을 수집
  (StyledNode 에 path 추가; 익명 박스는 부모 경로 공유 → 텍스트 클릭도 매칭)
- 클릭 → 포함하는 요소 중 가장 깊은 경로 = 타깃
- 등록 핸들러: 등록 경로가 타깃 경로의 접두사면 실행 (조상 = 버블링)
- onclick 속성: 타깃부터 조상 순서로 평가
- 핸들러가 하나라도 실행되면 rebuild + 다시 그리기
- 핸들러 에러는 [js error] 로 격리, 링크 기본 동작(내비게이션)은 핸들러 후 수행

## 한계 (정직하게)

- 인라인 요소(<span> 등) 자체의 히트 영역 없음 — 블록 조상이 받는다.
  버튼은 UA 에서 display:block 처리 (inline-block 미지원 대체)
- 경로 핸들은 형제 구조 변형에 취약 — createElement/appendChild 가 오는
  M4c 에서 DOM 아레나(NodeId) 리팩터링 전제 (M4a 스펙과 동일)
- 이벤트 객체(e.target 등) 미전달, stopPropagation 없음

## 검증

- 단위: 핸들러 등록(3경로)/버블링 접두사 매칭/디스패치 후 DOM 반영
- 통합: Page::dispatch_click 로 버튼 클릭 → rebuild → 텍스트 갱신 확인
- E2E: KESTREL_CLICK=x,y 헤드리스 클릭 → 렌더 비교, 창에서 수동 확인
