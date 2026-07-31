# Kestrel CSS 적합성 — css-values 수학 함수 현황·계획 (2026-07-31)

측정: WPT `css/css-values` 전량(testharness). 현재 **31.5%** (2,385 / 7,560, 하네스 못 돈 파일 10).

## 실패 분포(파일별 상위)

| 파일 | 실패 | 성격 | 난이도 |
|---|---|---|---|
| calc-size/animation/* (다수) | ~1,470 | calc-size 보간(애니메이션) | 높음(보간 엔진 필요) |
| if-conditionals | 202 | `if()` 조건부 값 | 중(신규 파싱+평가) |
| signs-abs-computed | 149 | `abs()` `sign()` 계산값 | 중(함수 미구현) |
| sin-cos-tan-serialize | 147 | 삼각함수 지정/계산 직렬화 | 중(프로퍼티 수용+직렬화) |
| signed-zero | 123 | calc 의 -0 직렬화 | 중 |
| round-mod-rem-computed | 118 | round/mod/rem 계산값 | 아래 참조 |
| attr-all-types | 112 | `attr()` 타입 지정 | 중 |
| round-mod-rem-invalid | 102 | malformed 거부 | **낮음(구문 검증)** |
| line-break-ch-unit | 95 | `ch` 단위 | 중 |
| acos-asin-atan-atan2-serialize | 62 | 역삼각 직렬화 | 중 |
| progress-serialize | 52 | `progress()` | 중(함수 미구현) |

## 핵심 블로커: 값 타입 모델에 각도·시간 단위 없음

`Unit` enum(`src/css/mod.rs`)은 길이/뷰포트/number/lh 만 있고 **각도(deg/grad/rad/turn)·시간(s/ms)·주파수·해상도 단위가 없다.** 그래서:

- `round(10s,6s)` → `math_arg` 가 시간 단위를 Length 로 못 실어 미평가 → `round(10s, 6s)` 그대로.
- `round(10deg,6deg)` → 각도 미표현 → 미평가 → 테스트에서 parseFloat NaN.
- 스펙상 계산값은 **각도 → degree, 시간 → second 로 정규화 직렬화**(예: `round(10grad,6grad)`=12grad → `10.8deg`, `round(10ms,6ms)`=12ms → `0.012s`).

`Unit` 확장은 layout `to_px` 변환 등 9개 파일에 exhaustive match 로 얽혀 있어 침습적 — 별도 큰 작업으로 분리한다(각도/시간 단위는 transform rotate·transition duration 에도 이득).

## 혼합 단위 round/mod/rem

`round(10%,1px)` `round(2rem,5px)` `mod(18px,100% / 15)` 등은 파스 타임에 % 를 못 풀어 미해석 → 계산/사용값 시점(레이아웃 폭 확정 후)에 풀어야 한다. calc 의 px+% 혼합 해석 경로와 동일한 미구현 이슈.

## 착수한 것 (검증 대기)

- **malformed round/mod/rem/calc/min/max/clamp 구문 거부**(round-mod-rem-invalid 102): 프로퍼티 검증기가
  프리픽스만 보고 수용하던 것을 `math_function_valid`(함수명·괄호 균형·빈 인자·시작/끝 단독 연산자·
  연산자 없는 값 나열) 로 교체. 보수적이라 유효식은 거부하지 않음. `is_math_fn` 중앙 경유.
  → 빌드/self-test/css-values 재측정으로 회귀 없음 확인 후 커밋.

## 우선순위(권장)

1. **round-mod-rem-invalid 구문 거부**(착수, 낮은 위험) — 커밋 예정.
2. 각도/시간 `Unit` 도입(큰 작업) → round/mod/rem/삼각/역삼각의 각도·시간 케이스 대량 해결 + transform/transition 이득.
3. `abs()`/`sign()`/`progress()`/삼각함수의 **프로퍼티 수용 + 지정값 calc() 래핑 직렬화 + 계산값 평가**.
4. 혼합 단위(%+px) 계산값 해석(레이아웃 결합).
5. calc-size 보간(애니메이션 엔진 — 최대 버킷이나 별개 서브시스템).
