# 계산 스타일 요청 시 해석 (성능)

작성 2026-08-02. 선행: `2026-08-02-kestrel-virtual-clock-design.md`.

## 문제

`getComputedStyle` 한 번이 문서 전체를 재스타일한 뒤, **모든 요소**의 계산 스타일을
`HashMap<String, String>` 으로 새로 만든다(`window.rs collect_computed_styles`).
요소당 프로퍼티가 **356개**다. 즉 재계산 한 번의 비용이

    (요소 수) × 356 × (키 String 복제 + 값 포매팅 + 해시 삽입)

이다. 강제 레이아웃을 반복하는 페이지에서 이게 제곱으로 든다.

`sample` 프로파일(WPT `css/css-transitions/properties-value-001.html`, 테스트 50개):
최대 자기 시간이 `collect_computed_styles` 이고, 그 뒤를 SipHash `write`/`hash_one` 과
`HashMap::insert`, `String::clone`, `format` 이 잇는다. 전부 이 맵을 만드는 비용이다.
테스트 2개 174ms → 10개 947ms → 50개 12.6초로 초선형이며, 파일 전체(280개)는 러너
타임아웃(150초)을 넘긴다.

이건 트랜지션 전용 문제가 아니다. `getBoundingClientRect`/`offsetWidth`/`getComputedStyle`
을 루프에서 읽는 실사이트 스크립트(스티키 헤더, 캐러셀, 측정 후 배치)가 전부 같은 비용을 문다.

## 목표

계산 스타일을 **요청 시 해석**한다. 재계산 시점에 모든 요소의 문자열 맵을 만들지 않는다.

성공 기준(측정 가능):

1. `properties-value-001.html` 전체(280 테스트)가 러너 예산 안에서 완주한다.
2. 자체 테스트 전량 그린, 실사이트 3곳 렌더 PPM md5 무변화.
3. WPT css/dom/html 전 영역 순감 없음.

## 설계

### 핵심: 스타일 트리를 살려둔다

현재 `flush_layout`/`rebuild` 는 `StyledNode` 트리를 지역 변수로 만들고 레이아웃 후 버린다.
그래서 계산값을 나중에 물어볼 수 없고, 그 순간 전부 문자열로 굳혀 둘 수밖에 없었다.

바꾼다: 스타일 트리(정확히는 `NodeId → 지정값 맵`)를 페이지가 보관하고, `getComputedStyle`
의 프로퍼티 조회가 그때 해석한다.

```
Page {
    // 재계산 때 채운다. Value 그대로라 문자열 포매팅 비용이 없다.
    computed_values: HashMap<NodeId, Rc<HashMap<String, Value>>>,
}
```

`StyledNode.specified_values` 를 `Rc` 로 감싸면 트리에서 맵으로 옮기는 비용이 Rc 복제뿐이다.

### 조회 경로

`ComputedGetProperty`(그리고 `item`/`length`/`ownKeys`)가 `computed_values` 에서 Value 를
찾아 그때 `computed_value_string` 한다. 요소당 실제로 읽히는 프로퍼티는 보통 한두 개다.

문자열 결과는 `(NodeId, prop) → String` 캐시에 넣되, 재계산 세대(`layout_version` +
`css_epoch`)가 바뀌면 통째로 비운다.

### 트랜지션 비교

`start_transitions_on_style_change` 는 전후 계산값을 비교한다. 문자열 맵이 없어지므로
**Value 단위로 비교**한다(`PartialEq`). 후보(트랜지션이 걸린 요소)에 한정하는 현재 규칙은
유지하고, 내용 해시도 Value 기준으로 매긴다.

`from`/`to` 문자열은 전이를 실제로 시작할 때만 만든다(요소당 한두 프로퍼티).

### 남기는 것

`js.computed_styles`(문자열 맵)를 읽는 곳이 여럿이다(`animated_value`, `css_animation_value`,
`resolve_wide_keyword`, 단축 재조립 등). 한 번에 갈아엎지 않는다:

1단계 — `computed_values`(Value 맵)를 추가하고, `computed_styles` 는 **요청된 요소만**
지연 생성하는 캐시로 바꾼다. 기존 독자는 "가져오되 없으면 만든다" 헬퍼를 거치게 한다.
2단계 — 독자를 하나씩 Value 조회로 옮기고, 문자열 맵을 지운다.

1단계만으로 목표 성능이 나오는지 먼저 측정한다.

## 회귀 위험

- **열거 순서**: `getComputedStyle` 의 `item(i)`/`ownKeys` 는 정렬된 롱핸드 목록을 낸다.
  지연 생성으로 바뀌어도 목록 생성 규칙(`computed_prop_names`)은 그대로 써야 한다.
- **stale 캐시**: 세대 무효화를 놓치면 옛 값을 돌려준다. `layout_version`/`css_epoch` 두
  축을 모두 봐야 한다(CSSOM 변경은 DOM 버전을 안 올린다).
- **Rc 공유**: 지정값 맵을 공유하면 이후 변형이 공유자에게 새어 나갈 수 있다. 재계산은
  항상 새 맵을 만들므로 공유는 읽기 전용이어야 한다.

## 검증

- 자체 테스트 전량 + 실사이트 3곳 md5
- WPT css/dom/html 전후(넉넉한 예산 30초/60초에서)
- `properties-value-001.html` 완주 시간과 통과 수
