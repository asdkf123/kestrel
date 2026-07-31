# Kestrel CSS 적합성 — anchor positioning 현황 (2026-07-31)

측정: WPT `css/css-anchor-position` 전량(testharness 기준, reftest 제외), 헤드리스 렌더 러너.
"이번 세션"은 HEAD `c51a86d` 직후(baseline)부터 `97750d5`까지.

## 전체

| 영역 | 통과 / 전체 | % | 비고 |
|---|---|---|---|
| **CSS WPT 전량** | 64,219 / 125,685 | **51.1%** | 세션 시작 baseline |
| **CSS WPT 전량(현재)** | ~71,019 / 122,910 | **~57.8%** | anchor/position-area 작업 후 |
| **css-anchor-position** | 9,225 / 12,913 | **71.4%** | 이번 세션 후(baseline ~51%) |

## css-anchor-position 파일별

| 테스트 | 통과 / 전체 | % | 이번 세션 |
|---|---|---|---|
| anchor-size-parse-valid | 4,295 / 4,305 | **99.8%** | 0 → 4,295 ✅ |
| anchor-parse-valid | 2,354 / 2,359 | **99.8%** | 0 → 2,354 ✅ |
| position-area-parsing | 2,125 / 2,125 | **100%** | 325 → 2,125 ✅ |
| anchor-parse-invalid | 21 / 25 | 84.0% | 남은 4=calc 내부 |
| anchor-size-parse-invalid | 18 / 22 | 81.8% | 남은 4=calc 내부 |
| position-area-computed | 0 / 633 | 0.0% | 미착수(박스 문맥 재직렬화 필요) |
| anchor-position-writing-modes-001 | 0 / 1,296 | 0.0% | 미착수(writing-mode 해석 필요) |

## 이번 세션 커밋 (푸시 안 함)

| 커밋 | 내용 |
|---|---|
| `c51a86d` | background-clip 계산값 캐논 |
| `36b715d` | anchor-size() — sizing 프로퍼티 |
| `99a90fc` | anchor-size() — inset/margin |
| `f3d2a17` | anchor() — inset 전용 |
| `6324d8e` | anchor 지정값 캐논 직렬화(flip-order) + fallback 타입 제한 |
| `97750d5` | anchor-size() 완성 — size optional + 4값 단축 + length 단독 |
| `da9b3a3` | position-area 문법 검증 + 캐논 (parsing 15%→100%) |

`anchor()`/`anchor-size()` **파싱은 사실상 완료(99.8%)**. 전부 실제 문법(grammar) 검증 +
name-first 캐논 + 정확한 무효 거부로 구현(요행/관대수용 제거).

## 남은 것 (우선순위)

1. **position-area-computed** (0%) — parsing 은 완료(100%). computed 는 박스 문맥
   재직렬화 필요(gradient computed 의 resolve_gradient_computed 패턴 재사용 가능).
2. **writing-modes** (0%) — 논리 좌표의 writing-mode 해석 필요(깊음).
3. anchor invalid 잔여 8개 — `calc()/min()` **내부**의 무효 anchor 관대수용(별개 calc-엔진 이슈:
   나눗셈→곱셈 canon, 항 재정렬, min에 anchor 인자). anchor 문법 자체 아님.
