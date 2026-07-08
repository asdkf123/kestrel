# M4c: DOM 아레나 + 생성 API — 설계

날짜: 2026-07-09
상태: 승인됨 (M4b 후속, 사용자 "ok")

## 목표

JS 가 페이지 구조를 만들 수 있다. 완료 기준 데모: 버튼 클릭 →
createElement + appendChild 로 리스트 항목이 계속 추가되어 렌더된다.

## 왜 아레나인가

경로(자식 인덱스) 핸들은 형제 삽입/삭제 시 어긋난다 (M4a/M4b 스펙에서
두 번 미룬 숙제). 아레나로 전환하면:
- NodeId 가 구조 변형과 무관하게 안정
- parent 링크 → 버블링이 경로 접두사 대신 조상 체인으로 정확
- O(1) 핸들 해석, 이후 증분 레이아웃의 기반

## 구조

- dom.rs: `Dom { nodes: Vec<NodeData>, root: NodeId }`,
  `NodeData { parent, children: Vec<NodeId>, node_type }` (NodeType 재사용)
- 파서는 기존 트리 빌더 유지 → `Dom::from_tree` 로 1회 변환 (html.rs 재작성 회피)
- `html::parse_dom(src)` 편의 함수, `html::parse_fragment(src) -> Vec<Node>`
  (innerHTML 용 — 다중 루트를 감싸지 않고 반환)
- 삭제/교체된 노드는 detach 만 (아레나 재사용 없음 — 페이지 수명 동안
  고아 노드 누수 감수, 단순성 우선. 문서화)
- StyledNode: path 제거, `id: NodeId` + `node: &NodeData`. 요소 히트 영역은
  (Rect, NodeId). JS 핸들 Value::Dom(NodeId). 핸들러 레지스트리 키 NodeId.
- 버블링: 타깃의 조상 체인([target, parent, ..., root])에 등록 id 가 있으면 실행

## DOM API (2차 커밋)

- document.createElement(tag) → 고아 요소 핸들
- el.appendChild(child) — 기존 부모에서 떼어 마지막 자식으로. 순환은 무시
- el.remove() — 부모에서 detach
- el.setAttribute(name, value) / el.getAttribute(name) — class/id 는 스타일
  매칭에 반영 (rebuild 가 다시 계산하므로 자동)
- el.innerHTML = "<li>..." — parse_fragment 로 파싱해 자식 교체
- 기존: getElementById, textContent (아레나 기반으로 재구현)

## 검증

- 단위: 아레나 변환/append(재부모화·순환 무시)/detach/텍스트, 스타일·레이아웃
  기존 테스트 전부 그린 (동작 보존)
- 통합: 클릭 → createElement/appendChild → rebuild 후 히트 영역·렌더에 반영,
  구조 변형 후에도 기존 핸들 유효 (아레나의 존재 이유)
- E2E: 리스트 추가 데모 헤드리스 클릭 렌더
