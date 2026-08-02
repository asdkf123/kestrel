# 가상 시계 이벤트 루프 (애니메이션 서브시스템 1/4)

작성 2026-08-02. 대상 커밋 `35155c8`.

## 문제

kestrel 에는 **시간이 흐르지 않는다**. 타이머는 지연을 정렬 키로만 쓰고 즉시 전부 실행되며,
JS 가 읽는 시계(`performance.now`)는 벽시계라 항상 0 근처다. 그 결과 시간에 의존하는 표준
동작 전체 — CSS 트랜지션 진행, CSS 애니메이션 재생, Web Animations 타임라인, 타이머 순서 —
가 원리적으로 불가능하다.

측정으로 확인한 현재 상태(HEAD `35155c8`):

| 사실 | 위치 |
|---|---|
| 헤드리스 타이머 루프가 `delay_ms` 를 정렬에만 쓰고 시각을 전진시키지 않는다 | `src/window.rs:1364` `flush_timers_headless` |
| `Timer` 에 발화 시각이 없다(`id`/`callback`/`delay_ms`/`repeat` 뿐) | `src/js/interp/natives.rs:434` |
| `performance.now()` 가 프렐류드 JS 의 `Date.now() - t0` | `src/js/mod.rs:964` |
| `requestAnimationFrame` 이 `setTimeout` 별칭(프레임 개념 없음) | `src/js/interp/mod.rs:901`, `:1143` |
| 애니메이션 진행률이 객체의 `currentTime` 프로퍼티를 그대로 읽는다(아무도 전진시키지 않음) | `src/js/interp/mod.rs:8092` |
| CSS 애니메이션 경과 시간을 **음수 `animation-delay`** 에서만 얻는다 | `src/js/interp/mod.rs:9805` |
| `getAnimations()` 가 항상 빈 배열 | `src/js/interp/builtins.rs:5203` |

WPT 실패 분포가 이를 그대로 반영한다(2026-08-02 실측).

| 서브트리 | 통과/전체 | 비고 |
|---|---|---|
| css-transitions | 886 / 3,086 (28.7%) | 실패 2,200 중 **1,802 가 `properties-value-*` 4개 파일** |
| css-animations | 703 / 1,059 (66.4%) | |
| css/motion | 1,244 / 4,185 (29.7%) | 성격이 다름 — 값 검증 게이트 실패가 지배 |

`properties-value-001.html` 은 2초짜리 트랜지션을 걸고 `setTimeout` 으로 중간값을 표집한 뒤
값과 이벤트를 각각 검사한다. 시계가 없으면 표집 시점이 전부 t=0 이라 통과가 불가능하다.

## 목표

시간이 흐르는 결정적 이벤트 루프를 만든다. 구체적으로:

1. 타이머가 **지연을 지킨다** — `setTimeout(f, 2000)` 은 가상 시각 2000ms 에 실행된다.
2. 타이머 실행 순서가 §HTML 타이머 초기화 절차를 따른다(발화 시각 오름차순, 동시각은 등록 순서, 중첩 5단계 초과 시 최소 4ms 클램프).
3. `requestAnimationFrame` 이 타이머와 분리된 프레임 큐가 되고, 콜백 인자로 프레임 시각을 받는다.
4. JS 가 보는 시계(`performance.now`, `Date.now`, `new Date()`)가 **하나의 가상 시계**에서 나온다.
5. 진행 중인 애니메이션의 `currentTime` 이 시계 전진에 따라 갱신되어, 콜백 안에서 부른 `getComputedStyle` 이 그 시각의 보간값을 돌려준다.

### 비목표 (후속 하위 프로젝트)

- 진짜 `Animation`/`KeyframeEffect` 객체 모델, `getAnimations()` (2번)
- `transitionrun`/`transitionstart`/`transitionend`/`animationstart` 등 이벤트 발화 (3번)
- 프로퍼티별 보간 타입 레지스트리, 퍼센트 키프레임 (4번)
- 실시간(벽시계) 애니메이션 재생. kestrel 은 헤드리스 정적 렌더가 기준이며 시계는 가상이다.

## 전체 분해와 순서

애니메이션 서브시스템은 한 스펙에 담기지 않아 넷으로 나눈다.

1. **가상 시계 이벤트 루프** (이 문서)
2. **Web Animations 객체 모델** — `DocumentTimeline`, `Animation`, `KeyframeEffect`, `getAnimations()`, 효과 스택 합성 순서
3. **트랜지션/CSS 애니메이션 실행과 이벤트** — 계산값 변화 감지로 트랜지션 생성(§CSS Transitions 4 역전 규칙, `transition-behavior: allow-discrete`), `@keyframes` 재생, 이벤트 발화
4. **보간 타입 레지스트리** — 프로퍼티별 애니메이션 타입 표, 기본값을 discrete 로, 퍼센트 키프레임

1번이 먼저인 이유는 2·3번이 전부 시계 위에 서기 때문이다. 시계 없이 얹은 애니메이션 모델은
지금의 스냅샷 근사를 한 겹 더 쌓는 것이 되고, 그것이 정확히 이 프로젝트가 배격하는 요행이다.

## 설계

### 시계의 소유자

가상 시각은 **`Interp`** 가 소유한다(`pub virtual_now_ms: f64`, 페이지 시작 = 0).

시각을 읽어야 하는 쪽이 전부 JS 네이티브(`performance.now`, `Date.now`, 애니메이션 샘플링)
이고, 이들은 `Interp` 안에서 실행되기 때문이다. 루프를 도는 주체는 지금처럼 `Page`
(`src/window.rs`)이며, `Page` 는 시계를 **전진시키는 쪽**이지 소유자가 아니다.

```
Interp {
    virtual_now_ms: f64,        // 페이지 시작 이후 가상 경과 ms
    time_origin_wall_ms: f64,   // 페이지 시작 시각의 실제 벽시계 (Date 계열 기준점)
    timers: Vec<Timer>,         // 발화 시각 포함
    raf_callbacks: Vec<RafCallback>,
    timer_nesting: u32,         // §HTML 중첩 단계 (4ms 클램프용)
}
```

### 타이머

`Timer` 에 두 필드를 추가한다.

```
struct Timer {
    id: u64,
    callback: Value,
    delay_ms: f64,     // interval 재장전용으로 유지
    repeat: bool,
    fire_at_ms: f64,   // 신규: virtual_now + 클램프된 지연
    seq: u64,          // 신규: 동시각 타이 브레이커 (등록 순서)
}
```

등록 시각 계산은 §HTML "timer initialization steps" 를 따른다. 중첩 단계가 5를 넘으면
지연을 최소 4ms 로 올린다. 중첩 단계는 타이머 콜백 실행 중에 등록된 타이머에서 1 증가한다.

`clearTimeout` 은 기존 `cleared` 집합 방식을 유지한다.

### 루프

`flush_timers_headless` 의 **본문**을 `run_event_loop(virtual_deadline_ms)` 로 대체한다.
`flush_timers_headless` 는 기본 데드라인으로 `run_event_loop` 를 부르는 얇은 래퍼로 남겨,
기존 호출부 7곳(`main.rs:231`, `main.rs:1484`, `window.rs` 의 5곳)은 그대로 둔다.

```
loop {
    if js 예산 소진 → 중단 (기존 가드 유지)
    if 콜백 실행 횟수 > MAX_CALLBACKS → 중단
    다음 이벤트 = min(가장 이른 타이머 fire_at, 가장 이른 rAF 프레임 시각)
    if 없음 → 중단
    if 다음 이벤트 시각 > virtual_deadline → 중단
    virtual_now = max(virtual_now, 다음 이벤트 시각)   // 시계는 뒤로 가지 않는다
    활성 애니메이션 currentTime 갱신(아래)
    콜백 실행 (interval 이면 fire_at += delay 로 재장전, 아니면 제거)
    마이크로태스크 드레인
    DOM 이 바뀌었으면 rebuild (기존 최적화 유지)
}
```

종료 조건은 셋이며 모두 필요하다.

- **가상 데드라인** — 기본 30,000ms(가상). `KESTREL_VIRTUAL_TIME_MS` 로 조정. `setInterval(f, 16)`
  같은 반복 타이머는 큐가 절대 비지 않으므로 시각 상한이 유일한 종료 보장이다.
- **JS 예산** — 기존 `budget_exhausted()` 그대로. 폭주 콜백 방어.
- **콜백 횟수 상한** — 기본 20,000. 가상 시간은 싸게 흐르므로(16ms 인터벌 × 30초 = 1,875회)
  시각 상한만으로는 병리적 케이스를 못 막는다.

데드라인을 30초로 잡은 근거: 대상 WPT 트랜지션 테스트가 2초 트랜지션을 60초 하네스 타임아웃
안에서 표집한다. 30초면 테스트의 자체 타이머는 전부 소화하고 하네스 타임아웃(10초/60초)보다
크거나 비슷해 결과 보고를 막지 않는다. 실사이트는 대개 수백 ms 안에 큐가 빈다.

### requestAnimationFrame

`Native::SetTimeout` 별칭을 끊고 전용 큐를 만든다.

```
struct RafCallback { id: u64, callback: Value }
```

프레임 경계는 60Hz 로 본다. 다음 프레임 시각 = `virtual_now` 를 16.667ms 격자로 올림한 값
(이미 격자 위면 다음 격자). 대기 중인 rAF 콜백은 그 시각에 **한 번에** 실행하고, 각 콜백은
프레임 시각을 인자로 받는다(§HTML "run the animation frame callbacks"). 실행 직전에 큐를
비워, 콜백 안에서 다시 등록한 rAF 는 **다음** 프레임으로 간다(현재는 즉시 재실행되어 무한
루프가 되기 쉽다).

`cancelAnimationFrame` 은 rAF 큐에서 제거한다(현재는 `ClearTimer` 별칭이라 타이머 집합만 건드려
실제로 취소되지 않는다).

### JS 가 보는 시계

단일 소스로 통일한다.

- `performance.now()` → 네이티브로 교체, `virtual_now_ms` 반환. 프렐류드 JS 구현은 제거하되
  `timeOrigin`/`timing`/`navigation`/`mark` 등 나머지 표면은 유지한다(실사이트가 읽는다).
- `now_millis()`(`Date.now`, `new Date()` 의 기준) → `time_origin_wall_ms + virtual_now_ms`.
  `Date.now()` 와 `performance.now()` 가 같은 시계에서 나와야 경과 시간을 재는 코드가 일관된다.

Rust 쪽 비-JS 시간 사용처(HTTP 날짜 헤더 등)는 건드리지 않는다.

### 애니메이션 currentTime 전진

이 하위 프로젝트가 애니메이션에 대해 하는 일은 하나뿐이다. 시계를 전진시킬 때, 각 활성
애니메이션 항목의 `currentTime` 을 `virtual_now - start_time` 으로 갱신한다.

이를 위해 `element_animations` 항목에 시작 시각을 넣는다. 현재 튜플
`(Rc<RefCell<ObjMap>>, f64, HashMap<..>)` 은 필드 의미가 위치로만 구분되어 확장할수록
읽기 어렵다. 이름 있는 구조체로 바꾼다.

```
struct ActiveAnimation {
    obj: Rc<RefCell<ObjMap>>,       // JS 가 보는 Animation 객체 (currentTime 이 여기 산다)
    start_time_ms: Option<f64>,     // None = 아직 시작 안 함(2번에서 pending 상태로 확장)
    duration_ms: f64,
    props: HashMap<String, (String, String, String)>,
}
```

`animated_value()` 의 샘플 로직(`mod.rs:8085`)은 그대로 두고 읽는 경로만 새 필드명으로 바꾼다.
스크립트가 `animation.currentTime` 을 직접 대입하는 기존 동작(WPT 보간 하네스가 쓴다)은
그대로 살아 있어야 하므로, 시계가 갱신하는 것은 `start_time_ms` 가 있는 항목뿐이다.

이 최소 훅만으로 `properties-value-*` 의 `/ values` 서브테스트가 열린다. `/ events` 는 3번에서
이벤트를 붙여야 열린다.

## 인터페이스 경계

- `Interp` — 가상 시각을 소유하고, 타이머/rAF 큐를 보유하며, "다음 이벤트 시각"과 "그 시각의
  콜백 목록"을 꺼내주는 API 를 제공한다. 시계를 스스로 전진시키지 않는다.
- `Page`(`window.rs`) — 루프를 돌며 `Interp` 에 전진을 지시하고, 콜백을 실행하고, DOM 변경 시
  레이아웃을 다시 짓는다. 시각 계산 로직을 갖지 않는다.

이 분리를 지키면 나중에 실시간 재생(벽시계 구동)을 붙일 때 `Page` 의 루프만 바뀐다.

## 회귀 위험

1. **지연 콘텐츠 노출.** 현재는 지연을 무시하고 전부 실행해서 `setTimeout(…, 3000)` 로 드러나는
   콘텐츠가 우연히 렌더된다. 새 루프도 큐가 빌 때까지(또는 30초 가상 데드라인까지) 돌리므로
   유지되지만, 데드라인 초과분은 잘린다. 3사이트 렌더 검증으로 확인한다.
2. **반복 타이머 폭주.** 가상 시간이 흐르면 `setInterval` 이 이전보다 훨씬 많이 실행된다.
   콜백 횟수 상한과 JS 예산으로 막고, 실사이트 렌더 시간을 전후 비교한다.
3. **rAF 재등록 루프.** 프레임 큐를 콜백 실행 전에 비우는 규칙이 없으면 무한 루프가 된다.
   단위 테스트로 고정한다.
4. **`Date.now` 고정.** 가상 시계 기준이 되면 실행 중 벽시계가 흐르지 않는다. 실제 경과를
   재는 코드가 0을 볼 수 있으나, 타이머가 시각을 전진시키므로 대부분의 사용처는 오히려 정확해진다.
   test262 Date 서브셋으로 확인한다.
5. **하네스 타임아웃 발화.** 가상 시각이 10초를 넘으면 testharness 의 타임아웃 타이머가 실제로
   발화한다. 결과가 이미 보고된 뒤면 무해하지만, 느린 비동기 테스트에서 오탐이 될 수 있다.
   css-transitions/css-animations 전후 측정으로 순증을 확인한다.

## 검증

커밋마다 전부 통과해야 한다.

- `cargo test --release` 전량 그린 (현재 1,028개, 신규 단위 테스트 포함)
- 실사이트 3곳 헤드리스 렌더 rc=0, PPM 크기 4200017B
- WPT 전후 측정: `css/css-transitions`, `css/css-animations`, `html` 일부, test262 `built-ins/Date`

신규 단위 테스트(최소):

- 타이머가 발화 시각 오름차순으로 실행된다(등록 순서와 무관)
- 동시각 타이머는 등록 순서로 실행된다
- 중첩 5단계 초과 시 지연이 4ms 로 클램프된다
- `setInterval` 이 `delay` 간격으로 재장전된다
- `setTimeout(f, 2000)` 실행 시점에 `performance.now() >= 2000`
- rAF 콜백이 16.667ms 격자에서 실행되고, 콜백 안의 재등록은 다음 프레임으로 간다
- `cancelAnimationFrame` 이 실제로 취소한다
- 가상 데드라인 초과 시 루프가 멈춘다
- 시계는 단조 증가한다(과거 시각 타이머가 시계를 되돌리지 않는다)
- `start_time_ms` 가 있는 애니메이션의 `currentTime` 이 시계 전진에 따라 갱신된다

## 성공 기준

1. 자체 테스트 전량 그린, 실사이트 3곳 렌더 무회귀.
2. `css/css-transitions/properties-value-001.html` 의 `/ values` 서브테스트가 0 에서 유의미하게
   증가한다(`/ events` 는 3번 전까지 계속 실패하는 것이 정상이며, 그렇게 보고한다).
3. `css/css-transitions` 와 `css/css-animations` 통과 수가 순증하고, `html` 과 test262 에 순감이 없다.

수치는 구현 후 실측해 기록한다. 사전 예측치는 적지 않는다.
