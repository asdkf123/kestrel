# M4a: JavaScript 엔진 1차 — 설계

날짜: 2026-07-07
상태: 승인됨 (사용자: "DOM 조작까지", 원칙 확인 후 진행)

## 목표

인라인 `<script>` 가 DOM 을 읽고 바꿔서 렌더에 반영되는 것. 완료 기준 데모:

```html
<p id="greet">JS is off</p>
<script>
  var el = document.getElementById("greet");
  el.textContent = "Hello from Kestrel JS! 1+2=" + (1+2);
</script>
```

→ 창/헤드리스 렌더에 바뀐 텍스트가 나오면 성공.

## 원칙 대조 (가볍고 빠른 브라우저)

- 0부터: 외부 JS 엔진 없음. 렉서/파서/인터프리터 전부 자작. 의존성 추가 0.
- 가벼움: 트리 워킹 인터프리터 (작은 코드, 바이너리 증가 미미).
- 빠름: 1차는 정확성 우선. `--bench` 에 JS 케이스를 추가해 숫자로 추적하고,
  병목이 증명되면 바이트코드 VM 으로 전환한다 (측정 → 증명 → 최적화 사이클).

## 아키텍처: 트리 워킹 인터프리터

소스 → 렉서(토큰) → 파서(AST) → 인터프리터(AST 직접 순회 실행).

대안 검토:
- 바이트코드 VM: 빠르지만 초기 복잡도 큼. 측정으로 병목 증명 후 전환.
- 기존 엔진 임베드: 0부터 원칙 위반. 탈락.

## 언어 서브셋 (1차)

- 값: undefined, null, boolean, number(f64), string, object, array, function
- 문: var/let/const, if/else, while, for(;;), return, break/continue, 블록, 식문
- 식: 산술(+,-,*,/,%), 비교(==,===,!=,!==,<,>,<=,>=), 논리(&&,||,!), 삼항,
  할당(=, +=, -=, *=, /=), ++/-- (전위/후위), 멤버(obj.a, obj["a"]),
  호출, typeof, 객체/배열 리터럴, 그룹핑
- 함수: 선언식/표현식/화살표. 클로저 완전 지원 (렉시컬 환경 체인).
- 내장: console.log, 배열 push/length, 문자열 length,
  숫자↔문자열 변환 (+ 연산자의 문자열 연결 포함)
- 제외 (후속): 프로토타입/클래스/new, try/catch, async/Promise, 정규식,
  for-in/for-of, getter/setter, 템플릿 리터럴, this 완전 의미론

## 값 모델

`Value` enum: Undefined, Null, Bool(bool), Num(f64), Str(String),
Obj(Rc<RefCell<HashMap<String, Value>>>), Arr(Rc<RefCell<Vec<Value>>>),
Fn(Rc<JsFn>), NativeFn(이름 + fn 포인터), DomHandle(경로).
참조 타입은 Rc 공유 (JS 의미론과 일치). 순환 참조 누수는 1차에서 감수 (문서화).

## 환경 (스코프)

`Env { vars: HashMap<String, Value>, parent: Option<Rc<RefCell<Env>>> }`.
함수 호출 = 클로저가 캡처한 env 를 부모로 새 Env. 블록(let/const) = 새 Env.
var 는 가장 가까운 함수 Env 에 선언 (단순화된 호이스팅: 선언만, TDZ 미구현).

## DOM 바인딩

- `document.getElementById(id)` → DomHandle(루트로부터의 자식 인덱스 경로)
- `handle.textContent` get: 하위 텍스트 연결 / set: 자식들을 텍스트 노드 하나로 교체
- 핸들 = 경로 방식의 한계: 형제 삽입/삭제가 생기면 경로가 어긋날 수 있음.
  1차 변형은 textContent 교체뿐이라 안전. 이벤트 마일스톤(M4c)에서 DOM 을
  아레나(NodeId) 구조로 리팩터링할 것을 전제한다.

## 파이프라인 통합

HTML 파싱 → 인라인 `<script>` 수집(문서 순서) → 각각 실행 (DOM &mut 변형)
→ 스타일 → 레이아웃 → 렌더. 즉 동기 스크립트처럼 첫 렌더 전에 실행.
- 외부 src 스크립트: 1차 제외 (수집 시 스킵).
- 에러 격리: 렉스/파스/런타임 에러는 해당 스크립트만 중단하고 터미널에
  `[js error] ...` 출력. 페이지는 계속 렌더 (관용 원칙 — html/css 파서와 동일).
- console.log → 터미널에 `[console] ...` 출력.

## 파일 구조

- `src/js/mod.rs` — 공개 API: `run_scripts(dom: &mut dom::Node)` (수집+실행)
- `src/js/lexer.rs` — Tok enum + tokenize
- `src/js/ast.rs` — Expr/Stmt enum
- `src/js/parser.rs` — 우선순위 등반 식 파서 + 문 파서
- `src/js/interp.rs` — Value/Env/eval/builtin + DOM 바인딩

## 커밋 단위 (각각 TDD)

1. 렉서: 토큰화 (숫자/문자열+이스케이프/식별자/키워드/연산자/주석)
2. 파서: AST (식 우선순위, 문, 함수/객체/배열 리터럴)
3. 인터프리터: 값/환경/클로저/제어문/내장 (console.log 캡처 가능한 출력 훅)
4. DOM 바인딩 + 파이프라인 통합 + 데모 렌더 검증 + bench js 케이스

## 성능 추적

`--bench` 에 `js-loop` 케이스 (예: 10만 회 루프 산술) 추가, 기준선 기록.
이후 최적화(바이트코드 등)는 이 숫자 개선으로 증명한다.
