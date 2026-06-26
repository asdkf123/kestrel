# Kestrel M3b — 관용적 HTML 파서: 설계 문서

- 날짜: 2026-06-26
- 상태: 승인됨 (브레인스토밍 완료)
- 범위: M3b 만. (M3c = fetch→render 통합은 별도)

## 1. 맥락

M3a로 실제 사이트의 HTML을 가져오게 됐지만, 현재 `html.rs`(M1 토이 파서)는 `assert!`로 동작해 실제 HTML(doctype, void 태그, script/style, 엔티티, 안 닫힌 태그)을 만나면 **즉시 패닉**한다. M3b는 파서를 **관용적(tolerant)**으로 다시 써서 실제 페이지를 패닉 없이 파싱한다.

`parse(source: String) -> Node` 인터페이스와 DOM 타입은 **그대로 유지**한다(나머지 파이프라인 불변). DOM 변경 없음 — script/style은 텍스트 자식을 가진 Element로, 주석/doctype은 버린다.

## 2. 목표 (한 줄)

실제 웹페이지의 HTML을 입력해도 **절대 패닉하지 않고** 합리적인 DOM 트리를 만든다.

## 3. 아키텍처

`html.rs`를 두 부분으로 재작성:

- **토크나이저**: 입력을 토큰으로 — 시작태그/끝태그/텍스트/주석/doctype/raw-text(script·style).
- **스택 기반 트리 빌더**: 열린 요소 스택 `Vec<(ElementData, Vec<Node>)>`로 트리를 만든다. 끝태그는 스택에서 매칭 요소까지 pop하며 닫고(중간 미닫힘 자동 복구), 매칭 없으면 무시. EOF에서 남은 요소 전부 닫음.

소유권: 각 스택 항목이 자기 자식 `Vec<Node>`를 들고, 닫힐 때 부모의 자식으로 push. 인덱스/아레나 없이 소유 Node를 바로 생성.

## 4. 처리 규칙

| 입력 | 처리 |
|------|------|
| `<!doctype ...>` , `<!...>` | `>`까지 건너뜀 |
| `<!-- ... -->` | `-->`까지 건너뜀 |
| void 태그 (`area base br col embed hr img input link meta param source track wbr`) | 자식 없는 Element, 스택에 push 안 함 |
| self-closing `<x/>` | 자식 없는 Element |
| raw-text (`script`, `style`) | `</tag>`까지 내용을 **HTML 파싱 없이** 텍스트 자식으로. (style 내용 보존 → M3c가 CSS 추출, script 보존하되 무시) |
| 시작태그 | 태그명 소문자화, 속성(따옴표 있음/없음/값 없음) 파싱, 스택 push |
| 끝태그 | 스택에서 매칭까지 pop(자동 닫기), 매칭 없으면 무시 |
| 텍스트 | `<`까지. **엔티티 디코딩**(`&amp; &lt; &gt; &quot; &#39; &nbsp;` + 숫자 `&#NNN; &#xHH;`) |
| 잘못된 `<` (태그 시작 아님) | 리터럴 텍스트로 |
| EOF | 열린 요소 전부 자동 닫기 |

## 5. 범위 / 비범위

**범위**: 위 규칙으로 패닉 없는 파싱 + 합리적 DOM.

**비범위**: 완전한 HTML5 트리 구성 알고리즘(삽입 모드, 암묵적 `<tbody>`/`<head>`/`<body>` 생성, foster parenting 등), 모든 명명 엔티티(흔한 것 + 숫자만), `<template>`/foreign content(SVG/MathML). (필요해지면 후속)

## 6. 에러 처리

- 어떤 입력에도 패닉 금지. 모호하면 관용적으로 복구(자동 닫기, 무시, 리터럴화).
- 잘린 입력(EOF 도중)도 안전하게 마무리.

## 7. 테스트 / 검증

- **헤르메틱 단위 테스트**:
  - doctype + `<meta>`(void) + 중첩 + 텍스트가 패닉 없이 파싱되고 구조가 맞음.
  - 안 닫힌 태그(`<p>a<p>b`)가 자동 복구돼 DOM이 나옴.
  - `<style>`/`<script>` 내용이 raw 텍스트 자식으로 보존되고, 그 안의 `<`가 파서를 안 깸.
  - 엔티티(`&amp;` → `&`, `&#65;` → `A`)가 디코딩됨.
  - 대문자 태그(`<DIV>`)가 소문자로.
- **통합 검증**: M3a `--fetch`로 받은 **naver.com 240KB HTML을 parse()에 통과시켜 패닉 없이 DOM 생성**(요소 개수 출력). example.com도 동일.

## 8. 완료 기준 (M3b Definition of Done)

1. 위 단위 테스트 통과.
2. 실제 페이지(example.com, naver.com) HTML을 parse()해도 패닉하지 않고 DOM이 나온다.
3. 기존 M1/M2/M3a 테스트 모두 통과(`parse` 인터페이스 불변이라 영향 없음).

## 9. 결정 기록

| 질문 | 결정 |
|------|------|
| 접근 | 토크나이저 + 스택 기반 트리 빌더(소유 Node 직접 생성) |
| HTML5 완전성 | 미추구 — "패닉 없음 + 합리적 DOM" 수준(YAGNI) |
| script/style | raw-text로 소비, 텍스트 자식 보존(style은 M3c가 CSS 추출) |
| 주석/doctype | DOM에 안 넣고 버림 |
| 엔티티 | 흔한 명명 + 숫자 참조만 |
| DOM 타입 | 변경 없음 |
