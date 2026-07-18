// 트리 워킹 인터프리터. Value/Env(렉시컬 체인)/제어 흐름.
// 무한 루프로 브라우저가 멈추지 않도록 실행 스텝 한도를 둔다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::*;
use super::parser::parse;

mod builtins;
mod canvas;
mod cssom;
mod reflect_api;
mod net;
mod wasm_bind;
mod env;
mod natives;
mod objects;
mod value;
mod dom_api;
mod generator;
use generator::GenState;
use env::*;
pub use natives::*;
pub use objects::*;
use value::*;

// 스크립트 하나(또는 이벤트 핸들러 하나)에 줄 **시간** 예산. 스텝 수로 자르면 무한 루프뿐
// 아니라 무겁지만 정상적인 번들도 잘린다 — 실제로 fmkorea 의 스크립트가 5,000,000 스텝에서
// 잘려 나갔다. 브라우저도 "느린 스크립트" 를 스텝이 아니라 시간으로 판정한다.
const SCRIPT_BUDGET_MS: u64 = 5_000;
// 페이지 전체(모든 스크립트 + 핸들러 + 타이머)에 줄 총 예산. 개별 예산만 두면 폭주하는
// 콜백이 N개일 때 N × 예산이 든다 — fmkorea 의 타이머 드레인이 실제로 25초를 먹었다.
// 브라우저의 "페이지가 응답하지 않습니다" 에 해당한다.
const TOTAL_BUDGET_MS: u64 = 10_000;
// 시각 확인은 비싸다 — 이만큼마다 한 번만 본다 (2^16).
const TIME_CHECK_MASK: u64 = 0xffff;
// 이 접두사의 에러는 try/catch 로 잡을 수 없다 (무한 루프 가드가 무력화되지 않게)
const STEP_LIMIT_MSG: &str = "실행 한도 초과";

// 배열을 한 번에 밀도 있게 확보할 수 있는 최대 요소 수 (방어선).
// new Array(2**32-1) 같은 표준적 호출은 **밀집 배열로는** 64GB+ 를 즉시 요구한다.
// 진짜 브라우저는 희박 배열이라 length 만 키우고 저장은 안 한다. 우리는 아직
// 밀집 배열이라, 이 상한을 넘는 확보는 거부해 머신을 지킨다 (희박 배열은 별도 작업).
// 이 값(약 100만 * 24바이트 ≈ 24MB)이면 실사이트도 test262 도 다 통과한다.
const MAX_DENSE_ARRAY: usize = 1 << 20;

// 표준 네이티브 오류 종류 (ECMA-262 §20.5). Error 가 첫째여야 한다 (나머지의 프로토타입 부모).
pub(super) const ERROR_KINDS: [&str; 8] = [
    "Error",
    "TypeError",
    "RangeError",
    "SyntaxError",
    "ReferenceError",
    "EvalError",
    "URIError",
    "AggregateError",
];

// ── 제어 흐름 ──────────────────────────────────────────────────────

enum Flow {
    Normal(Value),
    Return(Value),
    // 선택적 레이블. Some(l) 은 레이블 l 을 지목한 break/continue.
    Break(Option<String>),
    Continue(Option<String>),
}

// 루프 몸통 실행 결과를 이 루프(my_label) 기준으로 해석.
enum LoopAct {
    Exit,            // 이 루프 종료 (break)
    Next,            // 다음 반복 (정상 종료 또는 continue)
    Propagate(Flow), // 이 루프 소관 아님 (return, 상위 레이블 대상 break/continue)
}

// 구문상 익명 함수/클래스 식인가 (NamedEvaluation 의 조건).
// `function(){}`, `() => {}`, `class {}` 만 해당한다. `makeFn()` 이나 `other` 는 아니다.
fn is_anonymous_fn_expr(e: &Expr) -> bool {
    match e {
        Expr::Func { name: None, .. } => true,
        Expr::Class(c) => c.name.is_none(),
        _ => false,
    }
}

// 함수 본문이 'use strict' 지시어 프롤로그로 시작하는가 (§11.2.1 Directive Prologue).
// 선행 문자열 리터럴 문장(디렉티브) 중 정확히 "use strict" 가 있으면 strict.
// strict 함수는 this 를 강제변환하지 않는다(§10.2.1.2). 내장 메서드도 이 규칙을
// 따르므로 프렐류드 빌트인에 'use strict' 를 붙여 raw this(null/원시)를 받게 한다.
fn body_is_strict(body: &[Stmt]) -> bool {
    for stmt in body {
        if let Stmt::Expr(Expr::Str(s)) = stmt {
            if s == "use strict" {
                return true;
            }
            // 다른 문자열 디렉티브 — 프롤로그 계속.
        } else {
            return false; // 프롤로그 끝(문자열 아닌 첫 문장).
        }
    }
    false
}

fn loop_action(f: Flow, my_label: &Option<String>) -> LoopAct {
    match f {
        Flow::Break(None) => LoopAct::Exit,
        Flow::Break(Some(l)) if Some(&l) == my_label.as_ref() => LoopAct::Exit,
        Flow::Continue(None) => LoopAct::Next,
        Flow::Continue(Some(l)) if Some(&l) == my_label.as_ref() => LoopAct::Next,
        Flow::Normal(_) => LoopAct::Next,
        other => LoopAct::Propagate(other),
    }
}

// ── 인터프리터 ────────────────────────────────────────────────────

// <canvas> 2D 그리기 명령 (캔버스 좌표계). 호스트가 박스로 매핑해 렌더.
#[derive(Clone, Debug)]
pub enum CanvasOp {
    FillRect { x: f32, y: f32, w: f32, h: f32, color: crate::css::Color },
    ClearRect { x: f32, y: f32, w: f32, h: f32 },
    StrokeRect { x: f32, y: f32, w: f32, h: f32, color: crate::css::Color, lw: f32 },
    FillPath { pts: Vec<(f32, f32)>, color: crate::css::Color },
    FillText { text: String, x: f32, y: f32, color: crate::css::Color, px: f32 },
    // 현재 변환 행렬(CTM). 이후 op 들에 적용된다. 예전엔 translate/rotate/scale 이
    // 조용한 no-op 이라 그림이 엉뚱한 자리에 그려졌다 (아무 말도 없이).
    SetTransform { m: crate::layout::Mat },
    // drawImage(img, dx, dy [, dw, dh]) — 예전엔 no-op 이라 그림이 통째로 사라졌다.
    DrawImage { idx: usize, x: f32, y: f32, w: f32, h: f32 },
    // 그라디언트로 칠하기 (모양은 shape, 없으면 rect 전체)
    FillGradient {
        rect: crate::layout::Rect,
        shape: Option<Vec<(f32, f32)>>,
        kind: crate::paint::CanvasGrad,
        stops: Vec<(crate::css::Color, f32)>,
    },
    // 패턴(이미지 반복)으로 칠하기
    FillPattern { rect: crate::layout::Rect, shape: Option<Vec<(f32, f32)>>, idx: usize, repeat: bool },
    // clip(): 이후 그리기를 이 다각형으로 자른다 (save/restore 로 복원)
    Clip { pts: Option<Vec<(f32, f32)>> },
    // putImageData: 즉석 픽셀을 그대로 얹는다
    PutImage { x: f32, y: f32, img: std::rc::Rc<crate::png::Image> },
    // 그림자 상태 (shadowColor/Blur/OffsetX/Y). 이후 그리기 op 에 적용된다.
    // 예전엔 이 프로퍼티들이 **있기만 하고 아무도 안 읽었다** — 그림자가 아예 안 나왔다.
    SetShadow { color: crate::css::Color, blur: f32, dx: f32, dy: f32 },
}

// 복합 대입 연산자 → 대응하는 이항 연산자
fn compound_binop(op: &AssignOp) -> BinOp {
    match op {
        AssignOp::Add => BinOp::Add,
        AssignOp::Sub => BinOp::Sub,
        AssignOp::Mul => BinOp::Mul,
        AssignOp::Div => BinOp::Div,
        AssignOp::Mod => BinOp::Mod,
        AssignOp::Pow => BinOp::Pow,
        AssignOp::BitAnd => BinOp::BitAnd,
        AssignOp::BitOr => BinOp::BitOr,
        AssignOp::BitXor => BinOp::BitXor,
        AssignOp::Shl => BinOp::Shl,
        AssignOp::Shr => BinOp::Shr,
        AssignOp::UShr => BinOp::UShr,
        _ => BinOp::Add, // Set/And/Or/Nullish 는 호출측에서 처리
    }
}

// 호출식에서 사람이 읽을 프레임 이름을 뽑는다: f(), o.m(), o[k](), (expr)()
fn callee_label(e: &Expr) -> String {
    match e {
        Expr::Ident(n) => n.clone(),
        Expr::Member { obj, prop, computed } => {
            let base = callee_label(obj);
            match (computed, prop.as_ref()) {
                (false, Expr::Str(p)) => format!("{}.{}", base, p),
                _ => format!("{}[…]", base),
            }
        }
        Expr::Call { callee, .. } => format!("{}()", callee_label(callee)),
        Expr::Func { name: Some(n), .. } => n.clone(),
        _ => "<anonymous>".to_string(),
    }
}

pub struct Interp {
    pub global: EnvRef,
    pub console: Vec<String>, // console.log 캡처 (호출측이 터미널에 출력)
    steps: u64,
    // 지금 실행 중인 스크립트/핸들러가 시작한 시각과 그 예산 (시간 기반 가드)
    script_start: Option<std::time::Instant>,
    script_budget_ms: u64,
    // 페이지 전체 누적 JS 시간과 그 총예산
    js_spent_ms: u64,
    total_budget_ms: u64,
    // JS 호출 스택 (호출식에서 뽑은 이름). 오류 메시지에 "어디서" 를 붙인다.
    // 스택이 없으면 진단이 사실상 불가능하다.
    js_stack: Vec<String>,
    // 오류가 **처음 던져진 시점**의 스택. 호출 경계를 빠져나오며 프레임이 pop 되므로
    // 맨 위에서 읽으면 이미 비어 있다. 가장 안쪽 프레임에서 한 번만 스냅샷한다.
    err_stack: Option<Vec<String>>,
    // DOM 바인딩이 사용 (실행 동안만 유효한 아레나 포인터)
    pub dom: Option<*mut crate::dom::Dom>,
    // 이벤트 핸들러 레지스트리: (요소 NodeId, 이벤트 타입, 핸들러 함수)
    // (요소, 이벤트, 리스너, 캡처 여부). 캡처 플래그가 없으면 DOM 이벤트의 3단계
    // (캡처 → 타깃 → 버블)를 지킬 수 없다 — 캡처 리스너가 버블 순서로 늦게 불린다.
    // (요소, 이벤트, 리스너, 캡처, once). once 를 무시하면 리스너가 두 번 이상 불린다 —
    // "한 번만" 을 전제로 짠 코드(모달 닫기, 애니메이션 종료 처리)가 조용히 두 번 돈다.
    pub handlers: Vec<(crate::dom::NodeId, String, Value, bool, bool)>,
    // MutationObserver 배달을 이미 예약했는가 (마이크로태스크 중복 예약 방지)
    mutation_scheduled: bool,
    // attachShadow 를 부른 요소들. 우리는 섀도 트리를 따로 두지 않고 요소 자신을
    // 섀도 루트로 돌려준다 — 콘텐츠는 실제로 렌더되지만 스타일 격리는 없다(문서화된 근사).
    shadow_hosts: std::collections::HashSet<crate::dom::NodeId>,
    // 스크립트가 요청한 스크롤 위치(px). 호스트가 렌더에 반영한다.
    pub scroll_x: f32,
    pub scroll_y: f32,
    // document.activeElement (focus/blur 가 갱신). 없으면 body 로 보고한다.
    active_element: Option<crate::dom::NodeId>,
    // 강제 레이아웃(forced layout) 입력. 스크립트/콜백 실행 구간에만 설정된다.
    // 측정 API 를 읽는 순간 보류된 스타일·레이아웃을 흘리기 위한 것 (CSSOM View).
    pub layout_ctx: Option<crate::window::LayoutCtx>,
    // 지금 실행 중인 코드가 속한 클래스의 private 스코프 id (0 = 없음).
    // 함수를 만들 때 이 값을 그 함수에 새기고(렉시컬), 호출할 때 복원한다.
    priv_id: u64,
    priv_counter: u64,
    // DOM 노드에 스크립트가 붙인 임의 프로퍼티 (expando). 표준의 플랫폼 객체는
    // 평범한 객체이기도 하다 — el.foo = 1 이 실제로 저장돼야 한다.
    // 예전엔 조용히 버려서, 커스텀 엘리먼트의 this._v = ... 가 통째로 사라졌다.
    dom_props: HashMap<(crate::dom::NodeId, String), Value>,
    // 업그레이드된 커스텀 엘리먼트의 생성자 (프로토타입 체인 연결용).
    // 예전엔 연결이 없어서 this.anyMethod() 가 전부 undefined 였다.
    element_classes: HashMap<crate::dom::NodeId, Value>,
    // 인라인 이벤트 핸들러 속성의 소스 (중복 등록 방지) 와 그 함수
    inline_handlers: HashMap<(crate::dom::NodeId, String), String>,
    inline_fns: HashMap<(crate::dom::NodeId, String), Value>,
    // CSSOM 변경(insertRule/deleteRule/disabled) 세대. 반영된 세대와 다르면 재구성.
    pub css_epoch: u64,
    pub css_applied_epoch: u64,
    // 아래 측정 맵이 반영하고 있는 DOM 버전. dom.version() 과 다르면 다시 레이아웃한다.
    pub layout_version: Option<u64>,
    // 레이아웃 산출 요소 사각형 (NodeId → (x, y, w, h), CSS px). 리빌드 후 호스트가 채움.
    // getBoundingClientRect/offsetWidth 등이 읽는다. 빈 맵이면 0 을 돌려준다.
    pub layout_rects: std::collections::HashMap<crate::dom::NodeId, (f32, f32, f32, f32)>,
    // CSSOM View 용 상자 메트릭. 예전엔 client*/scroll*/offset* 를 전부 테두리 박스로
    // 근사했다 — clientLeft 가 좌표를 돌려주고(테두리 두께여야 한다), scrollHeight 가
    // clientHeight 와 같아(콘텐츠가 넘쳐도) "넘쳤나?" 검사가 항상 거짓이었다.
    pub layout_metrics: std::collections::HashMap<crate::dom::NodeId, BoxMetrics>,
    // 계산된 스타일 (NodeId → 대시 프로퍼티명 → CSS 텍스트). 리빌드 후 호스트가 채움.
    // getComputedStyle 이 읽는다. 빈 맵이면 빈 문자열.
    pub computed_styles: std::collections::HashMap<crate::dom::NodeId, HashMap<String, String>>,
    // <canvas> 2D 그리기 명령 (NodeId → ops). 호스트가 렌더 시 DisplayItem 으로 변환.
    // 캔버스 미지원 기능 경고 중복 방지
    canvas_warned: std::collections::HashSet<String>,
    pub canvas_cmds: std::collections::HashMap<crate::dom::NodeId, Vec<CanvasOp>>,
    // document/window 레벨 핸들러: (이벤트 타입, 핸들러) — DOMContentLoaded/load 등
    pub global_handlers: Vec<(String, Value)>,
    // Math.random 용 xorshift 상태
    rng: u64,
    // throw 된 값 (에러 채널은 String 이라 값은 사이드 채널로 전달)
    thrown: Option<Value>,
    // localStorage 스텁 저장소 (페이지 수명)
    // Storage 는 삽입 순서를 유지해야 한다 (key(i) 가 인덱스로 접근 — 표준 §12.2).
    storage: Vec<(String, String)>,
    // setTimeout/setInterval 로 등록된 미처리 타이머 (호출측이 드레인해 실행)
    pub timers: Vec<Timer>,
    pub cleared: std::collections::HashSet<u64>,
    next_timer_id: u64,
    // Promise 마이크로태스크 큐: (핸들러, 값, 의존 promise, is_reject 반응). 핸들러가
    // 비함수면 값을 그대로 전파(is_reject 면 dep 거부, 아니면 이행). 스크립트/타이머 후 드레인.
    microtasks: std::collections::VecDeque<(Value, Value, Value, bool)>,
    // Function.prototype (call/apply/bind). 정체성 보존 위해 보관.
    fn_proto: Value,
    // String.prototype (문자열 메서드) — String.prototype.slice.call(x) 용.
    string_proto: Value,
    // Number/Boolean/RegExp.prototype — core-js uncurryThis(Constructor.prototype.method) 용.
    number_proto: Value,
    boolean_proto: Value,
    bigint_proto: Value,
    regexp_proto: Value,
    // Map/Set/Date/Symbol.prototype — 번들이 Map.prototype.get 등으로 참조(정체성 보존).
    map_proto: Value,
    set_proto: Value,
    error_proto: Value,
    // 오류 종류별 prototype (TypeError.prototype 등). 예전엔 8종이 Error.prototype 하나를
    // 공유해서 TypeError.prototype === Error.prototype 이었고, 던져진 오류 객체에는
    // __proto__ 도 constructor 도 없었다 — instanceof 는 "message 가 있나?" 오리 판별로,
    // e.constructor 는 Object 로 나왔다. 이제 각자 진짜 프로토타입 체인을 갖는다.
    error_protos: Vec<(&'static str, Value)>,
    // 이벤트 인터페이스별 prototype (Event/UIEvent/MouseEvent…). 진짜 상속 체인이다.
    event_protos: Vec<(&'static str, Value)>,
    // Object/Array 의 정적 멤버·prototype 을 담은 네임스페이스 맵.
    // 전역은 Native 생성자이고, 멤버 조회는 이 맵에 위임한다.
    object_ns: Value,
    array_ns: Value,
    date_proto: Value,
    symbol_proto: Value,
    // 페이지 기준 URL (상대 URL 해석용 — XHR/fetch)
    // 상대 URL 해석 기준 (문서의 base URL). <base href> 가 있으면 그것이다 —
    // location.href(문서 URL)와는 다를 수 있다.
    base_url: Option<String>,
    // ES 모듈: 절대 URL → 소스 (호스트가 미리 받아 넣는다. 인터프리터는 네트워크를 모른다)
    pub module_sources: HashMap<String, String>,
    // 임포트 맵 (베어 명세자 → URL). 긴 키 우선으로 정렬돼 들어온다.
    pub import_map: Vec<(String, String)>,
    // 스크립트가 요청한 내비게이션 (location.href = … / assign / replace / reload).
    // 호출측(렌더러)이 새 URL 로 다시 그린다 — 인터프리터는 네트워크를 모른다.
    pub navigate_to: Option<String>,
    // 지금 실행 중인 클래식 스크립트 노드 (document.write 의 삽입 지점 / document.currentScript).
    pub current_script: Option<crate::dom::NodeId>,
    // document.write 로 새로 생긴 스크립트 (src, 인라인 코드). 호출측이 실행한다 —
    // 인터프리터는 네트워크·실행 순서를 모른다.
    pub written_scripts: Vec<(Option<String>, String)>,
    // 절대 URL → 네임스페이스 객체 (평가 완료/진행 중). 순환 의존은 부분 채워진 채로 공유한다.
    module_namespaces: HashMap<String, Value>,
    // 진단용 관대 모드(KESTREL_LENIENT): undefined 멤버 접근/호출을 에러 대신
    // undefined 로 (표준 아님, naver 등 롱테일 거리 측정용). 접근 키를 집계.
    lenient: bool,
    pub lenient_hits: std::collections::HashMap<String, usize>,
    // 레이블 문(outer:)이 직후 루프/문에 넘겨줄 레이블. 그 문의 exec_stmt 가 즉시 take.
    pending_label: Option<String>,
    // Symbol() 고유 키 카운터, Symbol.for(k) 전역 레지스트리(key → Symbol 값).
    sym_counter: u64,
    sym_registry: HashMap<String, Value>,
    // new 로 함수를 호출하기 직전 설정 → call_value 가 스코프에 new.target 을 심는다.
    new_target: Option<Value>,
    // window 객체 자체(정체성). window 는 전역 객체이므로 own 프로퍼티에 없는 키는
    // 전역 스코프로 폴백한다 — `window.Promise` 같은 기능 탐지가 실제로 동작해야 한다.
    window_obj: Rc<RefCell<ObjMap>>,
    // 네이티브(내장 생성자/함수)에 붙은 프로퍼티. 폴리필이 `Promise.allSettled = fn`,
    // `Symbol.observable = …` 처럼 내장에 값을 얹는다 — 저장소가 없어 전부 에러였다.
    native_props: HashMap<Native, HashMap<String, Value>>,
    // 무결성 상태(freeze/seal/preventExtensions). 모든 객체 종류(Obj/Arr/Fn/Instance/
    // Class/Map/Set)에 통일 적용한다. Value 를 함께 보관해 주소 재사용으로 인한
    // 오탐을 막는다(강한 참조 → 주소 안정).
    // 비트: 1=preventExtensions, 2=sealed, 4=frozen
    integrity: HashMap<usize, (Value, u8)>,
    // 취소된(revoked) Proxy 들 — Proxy.revocable 의 revoke() 가 여기에 포인터를 남긴다.
    // 취소되면 모든 내부 메서드(get/set/has/delete/ownKeys/…)가 TypeError (§10.5, §28.2).
    // 강한 참조가 integrity 에 있어(주소 안정) 포인터 키가 안전하다.
    revoked_proxies: std::collections::HashSet<usize>,
    // 열린 WebSocket 들. JS 객체는 인덱스로 참조하고, 드레인 구간에서 폴링해 이벤트를 배달한다.
    pub sockets: Vec<(crate::websocket::WebSocket, Value)>,
    // 아직 배달하지 않은 open/error (핸들러가 등록되기 전에 쏘면 아무도 못 듣는다)
    pending_ws_open: Vec<Value>,
    pending_ws_error: Vec<Value>,
    // WebAssembly: 컴파일된 모듈 / 선형 메모리 / 인스턴스. JS 는 인덱스로 참조한다.
    wasm_modules: Vec<Rc<crate::wasm::Module>>,
    // (바이트 배열, 그 메모리를 감싼 JS 의 WebAssembly.Memory 객체)
    wasm_memories: Vec<(crate::wasm::MemRef, Value)>,
    wasm_instances: Vec<Rc<WasmInstance>>,
    // fetch 응답의 원본 바이트 (Response.arrayBuffer 용). 텍스트로 바꾸면 바이너리가
    // 조용히 망가진다 (from_utf8_lossy 가 U+FFFD 로 덮어쓴다) — wasm 은 그러면 못 읽는다.
    fetch_bodies: Vec<Rc<Vec<u8>>>,
}

// 인스턴스 + 그 임포트 함수들(JS 값). 임포트 호출은 이 순서로 색인한다.
pub struct WasmInstance {
    pub inst: crate::wasm::Instance,
    pub imports: Vec<Value>,
}

// wasm → JS 호출을 이어 주는 다리. 인스턴스는 Rc 로 잡고 있으므로
// 인터프리터를 &mut 로 빌려도 안전하다.
struct WasmHost<'a> {
    interp: &'a mut Interp,
    imports: Vec<Value>,
    module: Rc<crate::wasm::Module>,
}

impl crate::wasm::Host for WasmHost<'_> {
    fn call_import(
        &mut self,
        idx: usize,
        args: &[crate::wasm::Val],
    ) -> Result<Vec<crate::wasm::Val>, String> {
        let f = self
            .imports
            .get(idx)
            .cloned()
            .ok_or_else(|| format!("wasm: 임포트 {} 가 연결되지 않았다", idx))?;
        if !matches!(f, Value::Fn(_) | Value::Native(_) | Value::Bound(_) | Value::Class(_)) {
            return Err(format!("wasm: 임포트 {} 가 함수가 아니다", idx));
        }
        let js_args: Vec<Value> = args.iter().map(wasm_val_to_js).collect();
        // JS 로 나가기 전에 메모리를 다시 묶는다 — 임포트 콜백은 memory.buffer 를 읽는다
        // (wasm-bindgen 의 문자열 전달이 정확히 이 경로다). 안 하면 옛 배열을 본다.
        self.interp.sync_wasm_memories();
        let r = self.interp.call_value(f, None, js_args)?;
        // 결과 타입은 모듈에 적혀 있다 — 값의 모양으로 추측하면 조용히 틀린다.
        let results = self
            .module
            .import_func_type(idx)
            .map(|t| t.results.clone())
            .unwrap_or_default();
        Ok(match results.len() {
            0 => vec![],
            1 => vec![js_to_wasm_typed(&r, results[0])],
            n => {
                // 다중 값: JS 는 배열로 돌려준다 (JS-API §ToWebAssemblyValue, iterable)
                let items: Vec<Value> = match &r {
                    Value::Arr(a) => a.borrow().clone(),
                    _ => return Err("wasm: 다중 값 임포트는 배열을 돌려줘야 한다".to_string()),
                };
                (0..n)
                    .map(|k| {
                        js_to_wasm_typed(
                            items.get(k).unwrap_or(&Value::Undefined),
                            results[k],
                        )
                    })
                    .collect()
            }
        })
    }
}

pub(super) fn wasm_val_to_js(v: &crate::wasm::Val) -> Value {
    use crate::wasm::Val;
    match v {
        Val::I32(n) => Value::Num(*n as f64),
        Val::F32(n) => Value::Num(*n as f64),
        Val::F64(n) => Value::Num(*n),
        // i64 는 반드시 BigInt 다 (JS-API §ToJSValue). Number 로 주면 2^53 위에서 조용히 틀린다.
        Val::I64(n) => Value::BigInt(Rc::new(crate::js::bigint::BigInt::from_i64(*n))),
    }
}

// ToInt32 (표준 §7.1.6): NaN/Inf → 0, 나머지는 2^32 로 감싸 부호 있는 32비트로.
// `n as i32` 는 범위 밖에서 포화(saturate)한다 — 표준은 감싸야 한다.
pub(super) fn to_int32(n: f64) -> i32 {
    if !n.is_finite() {
        return 0;
    }
    let t = n.trunc();
    let m = t.rem_euclid(4294967296.0);
    if m >= 2147483648.0 {
        (m - 4294967296.0) as i32
    } else {
        m as i32
    }
}

// JS 값 → wasm 값. 대상 타입을 알면 그 타입으로 (표준의 ToWebAssemblyValue).
pub(super) fn js_to_wasm_typed(v: &Value, t: u8) -> crate::wasm::Val {
    use crate::wasm::Val;
    let num = |v: &Value| -> f64 {
        match v {
            Value::Num(n) => *n,
            Value::Bool(b) => {
                if *b {
                    1.0
                } else {
                    0.0
                }
            }
            Value::Str(s) => s.trim().parse::<f64>().unwrap_or(f64::NAN),
            Value::BigInt(b) => b.to_f64(),
            _ => f64::NAN,
        }
    };
    match t {
        0x7f => Val::I32(to_int32(num(v))),
        0x7e => Val::I64(match v {
            Value::BigInt(b) => b.to_i64(),
            other => num(other) as i64,
        }),
        0x7d => Val::F32(num(v) as f32),
        _ => Val::F64(num(v)),
    }
}


// 요소의 상자 메트릭 (CSSOM View §4). 전부 CSS px.
#[derive(Clone, Copy, Debug, Default)]
pub struct BoxMetrics {
    pub border: (f32, f32, f32, f32), // top, right, bottom, left
    pub padding_w: f32,               // 패딩 박스 크기 = clientWidth/Height
    pub padding_h: f32,
    pub scroll_w: f32, // 스크롤 가능 오버플로 크기
    pub scroll_h: f32,
}

// 무결성 상태를 걸 수 있는 값의 신원(Rc 포인터). 원시값은 None.
// 던져진 값의 사람이 읽을 문자열. Error 객체면 "TypeError: 메시지" 로 —
// to_display 는 표준대로 "[object Object]" 라, 진단만 보면 **무엇이 틀렸는지 알 수 없다**.
pub(super) fn error_text(v: &Value) -> String {
    if let Value::Obj(o) = v {
        let b = o.borrow();
        let name = match b.get("name") {
            Some(Value::Str(s)) => s.clone(),
            _ => String::new(),
        };
        let msg = match b.get("message") {
            Some(m) => to_display(m),
            None => String::new(),
        };
        if !name.is_empty() || !msg.is_empty() {
            return match (name.is_empty(), msg.is_empty()) {
                (false, false) => format!("{}: {}", name, msg),
                (true, false) => msg,
                _ => name,
            };
        }
    }
    if let Value::Instance(i) = v {
        // class X extends Error 로 만든 인스턴스
        if let Some(m) = i.fields.borrow().get("message") {
            let name = i
                .fields
                .borrow()
                .get("name")
                .map(to_display)
                .unwrap_or_else(|| i.class.name.borrow().clone());
            return format!("{}: {}", name, to_display(m));
        }
    }
    to_display(v)
}

// Symbol.species 접근자를 가지는 내장 생성자 (§): Array/Map/Set/RegExp/Promise.
// getter 는 this 를 돌려준다(파생 종 = 자기 자신). TypedArray 는 별도 경로.
pub(super) fn native_has_species(n: &Native) -> bool {
    matches!(
        n,
        Native::ArrayCtor
            | Native::MapCtor
            | Native::SetCtor
            | Native::RegExpCtor
            | Native::PromiseCtor
    )
}

pub(super) fn integrity_ptr(v: &Value) -> Option<usize> {
    Some(match v {
        Value::Obj(m) => Rc::as_ptr(m) as usize,
        Value::Arr(a) => Rc::as_ptr(a) as usize,
        Value::Fn(f) => Rc::as_ptr(f) as usize,
        Value::Instance(i) => Rc::as_ptr(i) as usize,
        Value::Class(c) => Rc::as_ptr(c) as usize,
        Value::MapVal(m) => Rc::as_ptr(m) as usize,
        Value::SetVal(s) => Rc::as_ptr(s) as usize,
        _ => return None,
    })
}

pub(super) const INTEG_NONEXT: u8 = 1;
pub(super) const INTEG_SEALED: u8 = 2;
pub(super) const INTEG_FROZEN: u8 = 4;

impl Interp {
    pub fn new() -> Interp {
        let global = Env::new(None);
        // console.log
        let mut console = ObjMap::new();
        console.insert("log".to_string(), Value::Native(Native::ConsoleLog));
        env_declare(&global, "console", Value::Obj(Rc::new(RefCell::new(console))));
        // document (dom 포인터 미설정 시 호출하면 런타임 에러)
        let mut document = ObjMap::new();
        document.insert("getElementById".to_string(), Value::Native(Native::GetElementById));
        document.insert("createElement".to_string(), Value::Native(Native::CreateElement));
        document.insert(
            "createDocumentFragment".to_string(),
            Value::Native(Native::CreateDocumentFragment),
        );
        document.insert("querySelector".to_string(), Value::Native(Native::QuerySelector));
        document.insert("querySelectorAll".to_string(), Value::Native(Native::QuerySelectorAll));
        // 문서 레벨 이벤트(DOMContentLoaded/load): 핸들러를 등록하고 스크립트
        // 실행 후 발화한다(run_scripts). 프레임워크가 여기서 콘텐츠를 구성.
        document.insert("addEventListener".to_string(), Value::Native(Native::AddGlobalListener));
        document.insert(
            "removeEventListener".to_string(),
            Value::Native(Native::RemoveGlobalListener),
        );
        document.insert(
            "dispatchEvent".to_string(),
            Value::Native(Native::DispatchGlobalEvent),
        );
        // 스크립트 실행 중엔 "loading" — 프레임워크가 DOMContentLoaded 리스너를
        // 등록하도록. run_scripts 가 이후 interactive → complete 로 갱신.
        document.insert("readyState".to_string(), Value::Str("loading".to_string()));
        // 흔한 document 프로퍼티(미정의 크래시 방지). cookie 는 간이(문자열).
        // document.cookie: 진짜 쿠키 항아리에 연결한다 (읽기·쓰기 모두 HTTP 계층과 공유).
        // 예전엔 빈 문자열 상수라, 쿠키를 심는 스크립트가 아무 일도 안 한 채 성공했다.
        document.insert(
            "cookie".to_string(),
            Value::Accessor(Rc::new(AccessorPair {
                get: Some(Value::Native(Native::CookieGet)),
                set: Some(Value::Native(Native::CookieSet)),
            })),
        );
        document.insert("title".to_string(), Value::Str(String::new()));
        document.insert("referrer".to_string(), Value::Str(String::new()));
        document.insert("characterSet".to_string(), Value::Str("UTF-8".to_string()));
        document.insert("compatMode".to_string(), Value::Str("CSS1Compat".to_string()));
        document.insert("hidden".to_string(), Value::Bool(false));
        document.insert("visibilityState".to_string(), Value::Str("visible".to_string()));
        document.insert("createTextNode".to_string(), Value::Native(Native::CreateTextNode));
        document.insert("createComment".to_string(), Value::Native(Native::CreateComment));
        // document.styleSheets (§CSSOM 6.1). 예전엔 아예 없어서, 이걸 읽는 스크립트가
        // 그 줄에서 통째로 죽었다.
        document.insert(
            "styleSheets".to_string(),
            Value::Accessor(AccessorPair::getter(Value::Native(Native::StyleSheets))),
        );
        // document.defaultView — 이 문서의 window (§3.1). 없으면 프레임워크가
        // 문서에서 window 를 못 얻어 기능 탐지가 통째로 어긋난다.
        document.insert(
            "defaultView".to_string(),
            Value::Accessor(AccessorPair::getter(Value::Native(Native::WindowSelf))),
        );
        // createElementNS(ns, name) — JS 로 SVG 를 만드는 코드가 전부 이걸 쓴다.
        // 없으면 아이콘/차트를 동적으로 그리는 스크립트가 한 줄에서 죽는다.
        document.insert("createElementNS".to_string(), Value::Native(Native::CreateElementNS));
        // document.write / writeln (HTML §8.4.3). 레거시지만 아직도 대량으로 쓰인다
        // (국내 포털·광고 스크립트). 없으면 그 스크립트가 통째로 죽는다.
        document.insert("write".to_string(), Value::Native(Native::DocWrite));
        document.insert("writeln".to_string(), Value::Native(Native::DocWrite));
        document
            .insert("getElementsByClassName".to_string(), Value::Native(Native::GetElementsByClass));
        document.insert("getElementsByTagName".to_string(), Value::Native(Native::GetElementsByTag));
        // 라이브 접근자: document.body/head/documentElement → DOM 요소 핸들
        let live = |tag| Value::Accessor(AccessorPair::getter(Value::Native(Native::DocQuery(tag))));
        document.insert("body".to_string(), live("body"));
        document.insert("head".to_string(), live("head"));
        document.insert("documentElement".to_string(), live("html"));
        // document.currentScript — **지금 실행 중인 클래식 스크립트 요소** (HTML §4.12.1).
        // 번들러 런타임이 이걸로 자기 청크 URL 을 구한다 (Turbopack/webpack 의
        // publicPath 자동 감지). 없으면 "chunk path empty" 로 런타임이 통째로 죽는다.
        document.insert(
            "currentScript".to_string(),
            Value::Accessor(AccessorPair::getter(Value::Native(Native::CurrentScript))),
        );
        // document.activeElement — focus()/blur() 가 갱신한다. 없으면 body (표준).
        document.insert(
            "activeElement".to_string(),
            Value::Accessor(AccessorPair::getter(Value::Native(Native::ActiveElement))),
        );
        // nodeType: DOCUMENT_NODE(9). jQuery 의 setDocument 가 `doc.nodeType !== 9` 로
        // 문서를 검증하는데, 없으면 조기 반환해 로컬 document 가 undefined 로 남고
        // 이후 document.createElement 가 죽는다 → jQuery 전체가 못 뜬다.
        document.insert("nodeType".to_string(), Value::Num(9.0));
        // document.implementation.createHTMLDocument — jQuery 가 이걸로 분리 문서를
        // 만들어 feature test 를 한다(support.createHTMLDocument).
        let mut implementation = ObjMap::new();
        implementation
            .insert("createHTMLDocument".to_string(), Value::Native(Native::CreateHTMLDocument));
        // hasFeature 는 **언제나 true** 다 (DOM §4.5.1: "useless; always returns true").
        // 우리는 ReturnFalse 였다 — 표준과 정반대라, 기능 탐지를 하는 코드가 조용히
        // 다른 길로 샜다.
        implementation.insert("hasFeature".to_string(), Value::Native(Native::ReturnTrue));
        document.insert(
            "implementation".to_string(),
            Value::Obj(Rc::new(RefCell::new(implementation))),
        );
        env_declare(&global, "document", Value::Obj(Rc::new(RefCell::new(document))));
        // Math
        let mut math = ObjMap::new();
        for (name, op) in [
            ("floor", MathOp::Floor),
            ("ceil", MathOp::Ceil),
            ("round", MathOp::Round),
            ("abs", MathOp::Abs),
            ("min", MathOp::Min),
            ("max", MathOp::Max),
            ("sqrt", MathOp::Sqrt),
            ("pow", MathOp::Pow),
            ("random", MathOp::Random),
            ("trunc", MathOp::Trunc),
            ("sign", MathOp::Sign),
            ("cbrt", MathOp::Cbrt),
            ("log", MathOp::Log),
            ("log2", MathOp::Log2),
            ("log10", MathOp::Log10),
            ("exp", MathOp::Exp),
            ("sin", MathOp::Sin),
            ("cos", MathOp::Cos),
            ("tan", MathOp::Tan),
            ("asin", MathOp::Asin),
            ("acos", MathOp::Acos),
            ("atan", MathOp::Atan),
            ("atan2", MathOp::Atan2),
            ("hypot", MathOp::Hypot),
            ("clz32", MathOp::Clz32),
            ("expm1", MathOp::Expm1),
            ("log1p", MathOp::Log1p),
            ("sinh", MathOp::Sinh),
            ("cosh", MathOp::Cosh),
            ("tanh", MathOp::Tanh),
            ("asinh", MathOp::Asinh),
            ("acosh", MathOp::Acosh),
            ("atanh", MathOp::Atanh),
            ("fround", MathOp::Fround),
            ("imul", MathOp::Imul),
        ] {
            math.insert(name.to_string(), Value::Native(Native::Math(op)));
        }
        math.insert("PI".to_string(), Value::Num(std::f64::consts::PI));
        math.insert("E".to_string(), Value::Num(std::f64::consts::E));
        math.insert("SQRT2".to_string(), Value::Num(std::f64::consts::SQRT_2));
        math.insert("LN2".to_string(), Value::Num(std::f64::consts::LN_2));
        math.insert("LN10".to_string(), Value::Num(std::f64::consts::LN_10));
        // 예전엔 빠져 있던 상수 (§21.3.1).
        math.insert("LOG2E".to_string(), Value::Num(std::f64::consts::LOG2_E));
        math.insert("LOG10E".to_string(), Value::Num(std::f64::consts::LOG10_E));
        math.insert("SQRT1_2".to_string(), Value::Num(std::f64::consts::FRAC_1_SQRT_2));
        // 모든 Math 상수는 { writable:false, enumerable:false, configurable:false } (§21.3.1).
        for c in ["PI", "E", "SQRT2", "LN2", "LN10", "LOG2E", "LOG10E", "SQRT1_2"] {
            set_prop_attrs(&mut math, c, 0);
        }
        // Math[Symbol.toStringTag] === "Math" (§21.3.1.9) — Object.prototype.toString.call(Math)
        // 이 "[object Math]" 가 되게 한다. mark_nonenum_all 전에 넣어 비열거로.
        math.insert("\u{0}@@toStringTag".to_string(), Value::Str("Math".to_string()));
        // §21.3.1.9: { writable:false, enumerable:false, configurable:true }.
        set_prop_attrs(&mut math, "\u{0}@@toStringTag", ATTR_CONFIGURABLE);
        mark_nonenum_all(&mut math); // 내장 프로퍼티는 비열거 (§17)
        env_declare(&global, "Math", Value::Obj(Rc::new(RefCell::new(math))));
        // JSON
        let mut json = ObjMap::new();
        json.insert("parse".to_string(), Value::Native(Native::JsonParse));
        json.insert("stringify".to_string(), Value::Native(Native::JsonStringify));
        json.insert("rawJSON".to_string(), Value::Native(Native::JsonRawJson));
        json.insert("isRawJSON".to_string(), Value::Native(Native::JsonIsRawJson));
        // JSON[Symbol.toStringTag] === "JSON" (§25.5.1) → "[object JSON]".
        json.insert("\u{0}@@toStringTag".to_string(), Value::Str("JSON".to_string()));
        // §25.5.1: { writable:false, enumerable:false, configurable:true }.
        set_prop_attrs(&mut json, "\u{0}@@toStringTag", ATTR_CONFIGURABLE);
        mark_nonenum_all(&mut json);
        env_declare(&global, "JSON", Value::Obj(Rc::new(RefCell::new(json))));
        // 전역 함수
        env_declare(&global, "BigInt", Value::Native(Native::BigIntCtor));
        // escape/unescape (ECMAScript Annex B.2.1/B.2.2). 레거시지만 표준이고,
        // 국내 사이트가 쿠키·URL 인코딩에 아직도 쓴다 — 없으면 스크립트가 죽는다.
        env_declare(&global, "escape", Value::Native(Native::Escape));
        env_declare(&global, "unescape", Value::Native(Native::Unescape));
        env_declare(&global, "parseInt", Value::Native(Native::ParseInt));
        env_declare(&global, "parseFloat", Value::Native(Native::ParseFloat));
        env_declare(&global, "encodeURI", Value::Native(Native::EncodeUri));
        env_declare(&global, "encodeURIComponent", Value::Native(Native::EncodeUriComponent));
        env_declare(&global, "decodeURI", Value::Native(Native::DecodeUri));
        env_declare(&global, "decodeURIComponent", Value::Native(Native::DecodeUriComponent));
        env_declare(&global, "isNaN", Value::Native(Native::IsNaN));
        env_declare(&global, "structuredClone", Value::Native(Native::StructuredClone));
        // Reflect 네임스페이스 (Proxy/프레임워크 코드에서 흔함)
        let mut reflect_ns = ObjMap::new();
        reflect_ns.insert("get".to_string(), Value::Native(Native::ReflectGet));
        reflect_ns.insert("set".to_string(), Value::Native(Native::ReflectSet));
        reflect_ns.insert("has".to_string(), Value::Native(Native::ReflectHas));
        reflect_ns.insert("deleteProperty".to_string(), Value::Native(Native::ReflectDeleteProperty));
        // ownKeys 는 **모든** own 키(비열거 포함) — 예전엔 ObjectKeys(열거만)라 틀렸다.
        reflect_ns.insert("ownKeys".to_string(), Value::Native(Native::ReflectOwnKeys));
        reflect_ns.insert("getPrototypeOf".to_string(), Value::Native(Native::ReflectGetPrototypeOf));
        reflect_ns.insert("apply".to_string(), Value::Native(Native::ReflectApply));
        reflect_ns.insert("construct".to_string(), Value::Native(Native::ReflectConstruct));
        // defineProperty 는 성공 여부(불리언) — Object.defineProperty(객체 반환)와 다르다.
        reflect_ns.insert("defineProperty".to_string(), Value::Native(Native::ReflectDefineProperty));
        reflect_ns.insert("getOwnPropertyDescriptor".to_string(), Value::Native(Native::ReflectGetOwnPropertyDescriptor));
        reflect_ns.insert("setPrototypeOf".to_string(), Value::Native(Native::ReflectSetPrototypeOf));
        reflect_ns.insert("isExtensible".to_string(), Value::Native(Native::ReflectIsExtensible));
        reflect_ns.insert("preventExtensions".to_string(), Value::Native(Native::ReflectPreventExtensions));
        // Reflect[Symbol.toStringTag] === "Reflect" (§28.1.14) → "[object Reflect]".
        // { writable:false, enumerable:false, configurable:true }.
        reflect_ns.insert("\u{0}@@toStringTag".to_string(), Value::Str("Reflect".to_string()));
        set_prop_attrs(&mut reflect_ns, "\u{0}@@toStringTag", ATTR_CONFIGURABLE);
        mark_nonenum_all(&mut reflect_ns); // 내장 메서드는 비열거 (§17)
        env_declare(&global, "Reflect", Value::Obj(Rc::new(RefCell::new(reflect_ns))));
        env_declare(&global, "NaN", Value::Num(f64::NAN));
        env_declare(&global, "Infinity", Value::Num(f64::INFINITY));
        env_declare(&global, "isFinite", Value::Native(Native::NumIsFinite));
        // 타이머
        env_declare(&global, "setTimeout", Value::Native(Native::SetTimeout));
        env_declare(&global, "setInterval", Value::Native(Native::SetInterval));
        env_declare(&global, "clearTimeout", Value::Native(Native::ClearTimer));
        env_declare(&global, "clearInterval", Value::Native(Native::ClearTimer));
        env_declare(&global, "requestAnimationFrame", Value::Native(Native::SetTimeout));
        env_declare(&global, "cancelAnimationFrame", Value::Native(Native::ClearTimer));
        // getComputedStyle — 리빌드 후 채워진 computed_styles 를 읽는 실제 계산 스타일.
        env_declare(&global, "getComputedStyle", Value::Native(Native::GetComputedStyle));
        // matchMedia — CSS 의 @media 와 같은 평가기를 쓴다. 예전엔 프렐류드가 항상
        // matches:false 를 돌려줘서, CSS 는 데스크톱 규칙을 적용하는데 JS 는 모바일로
        // 분기하는 자기모순이 있었다.
        env_declare(&global, "matchMedia", Value::Native(Native::MatchMedia));
        env_declare(&global, "scrollTo", Value::Native(Native::ScrollTo));
        env_declare(&global, "scrollBy", Value::Native(Native::ScrollBy));
        // DOMParser — 문자열을 실제 DOM 으로 파싱 (분리된 서브트리라 렌더되지 않는다)
        env_declare(&global, "DOMParser", Value::Native(Native::DomParserCtor));
        // 프렐류드의 MutationObserver 가 쌓인 변형 기록을 가져가는 통로
        env_declare(&global, "__kTakeMutations", Value::Native(Native::TakeMutations));
        // 동적 import('m') — 렉서가 import 를 식별자로 내므로 호출식이 된다.
        // 미리 받아둔 모듈만 풀 수 있다(인터프리터는 네트워크를 모른다). 없으면
        // 조용히 undefined 를 주지 않고 명확한 이유로 거부한다.
        env_declare(&global, "import", Value::Native(Native::DynamicImport));
        // queueMicrotask — 진짜 마이크로태스크 큐에 넣는다 (setTimeout 으로 흉내내면
        // 실행 순서가 달라진다: 마이크로태스크는 매크로태스크보다 먼저 돈다).
        env_declare(&global, "queueMicrotask", Value::Native(Native::QueueMicrotask));
        // CSS.supports — CSS 의 @supports 와 같은 평가기를 쓴다 (한 엔진 두 답 금지)
        let mut css_ns = ObjMap::new();
        css_ns.insert("supports".to_string(), Value::Native(Native::CssSupports));
        css_ns.insert("escape".to_string(), Value::Native(Native::Noop));
        env_declare(&global, "CSS", Value::Obj(Rc::new(RefCell::new(css_ns))));
        // 전역 생성자 스텁 (instanceof 판별 + 정적 메서드)
        let mut object_ns = ObjMap::new();
        object_ns.insert("keys".to_string(), Value::Native(Native::ObjectKeys));
        object_ns.insert("values".to_string(), Value::Native(Native::ObjectValues));
        object_ns.insert("entries".to_string(), Value::Native(Native::ObjectEntries));
        object_ns.insert("fromEntries".to_string(), Value::Native(Native::ObjectFromEntries));
        object_ns.insert(
            "getOwnPropertyNames".to_string(),
            Value::Native(Native::ObjectGetOwnPropertyNames),
        );
        object_ns.insert("assign".to_string(), Value::Native(Native::ObjectAssign));
        object_ns.insert("defineProperty".to_string(), Value::Native(Native::ObjectDefineProperty));
        object_ns.insert(
            "getOwnPropertyDescriptor".to_string(),
            Value::Native(Native::ObjectGetOwnPropertyDescriptor),
        );
        object_ns.insert("defineProperties".to_string(), Value::Native(Native::ObjectDefineProperties));
        object_ns.insert("create".to_string(), Value::Native(Native::ObjectCreate));
        object_ns.insert("freeze".to_string(), Value::Native(Native::ObjectFreeze));
        object_ns.insert("seal".to_string(), Value::Native(Native::ObjectSeal));
        object_ns.insert("preventExtensions".to_string(), Value::Native(Native::ObjectPreventExt));
        object_ns.insert("isFrozen".to_string(), Value::Native(Native::ObjectIsFrozen));
        object_ns.insert("isSealed".to_string(), Value::Native(Native::ObjectIsSealed));
        object_ns.insert("isExtensible".to_string(), Value::Native(Native::ObjectIsExtensible));
        object_ns.insert(
            "getPrototypeOf".to_string(),
            Value::Native(Native::ObjectGetPrototypeOf),
        );
        object_ns.insert("groupBy".to_string(), Value::Native(Native::ObjectGroupBy));
        object_ns
            .insert("setPrototypeOf".to_string(), Value::Native(Native::ObjectSetPrototypeOf));
        object_ns.insert(
            "getOwnPropertySymbols".to_string(),
            Value::Native(Native::ObjectGetOwnPropertySymbols),
        );
        // Object.prototype: hasOwnProperty(webpack .o), toString(타입 판별 관용),
        // isPrototypeOf/propertyIsEnumerable/valueOf
        let mut object_proto = ObjMap::new();
        object_proto.insert("hasOwnProperty".to_string(), Value::Native(Native::HasOwnProperty));
        object_proto.insert("toString".to_string(), Value::Native(Native::ObjToString));
        object_proto.insert("toLocaleString".to_string(), Value::Native(Native::ObjToString));
        object_proto.insert("valueOf".to_string(), Value::Native(Native::ReturnThis));
        object_proto.insert(
            "propertyIsEnumerable".to_string(),
            Value::Native(Native::PropertyIsEnumerable),
        );
        object_proto.insert("isPrototypeOf".to_string(), Value::Native(Native::ObjectIsPrototypeOf));
        mark_nonenum_all(&mut object_proto); // 내장 메서드는 비열거 (§17)
        object_ns.insert("prototype".to_string(), Value::Obj(Rc::new(RefCell::new(object_proto))));
        mark_nonenum_all(&mut object_ns);
        let object_ns = Value::Obj(Rc::new(RefCell::new(object_ns)));
        env_declare(&global, "Object", Value::Native(Native::ObjectCtor));
        // Array.prototype: 모든 배열 메서드를 담아 Array.prototype.slice.call(x) 지원
        let mut array_ns = ObjMap::new();
        array_ns.insert("isArray".to_string(), Value::Native(Native::ArrayIsArray));
        array_ns.insert("from".to_string(), Value::Native(Native::ArrayFrom));
        array_ns.insert("of".to_string(), Value::Native(Native::ArrayOf));
        let mut array_proto = ObjMap::new();
        // 단일 소스에서 — member_get 의 인스턴스 조회와 정확히 같은 목록.
        for (name, op) in natives::ARRAY_PROTO_OPS {
            array_proto.insert(name.to_string(), Value::Native(Native::Arr(*op)));
        }
        array_proto.insert("push".to_string(), Value::Native(Native::ArrayPush));
        // Array.prototype[Symbol.iterator] — core-js uncurryThis 참조
        array_proto.insert("\u{0}@@iterator".to_string(), Value::Native(Native::MakeIter));
        // Array.prototype[Symbol.iterator]: { writable:true, enumerable:false,
        // configurable:true } (§23.1.3.x). mark_nonenum_all 은 심볼 키를 건너뛰므로 직접.
        set_prop_attrs(
            &mut array_proto,
            "\u{0}@@iterator",
            ATTR_WRITABLE | ATTR_CONFIGURABLE,
        );
        mark_nonenum_all(&mut array_proto); // 내장 메서드는 비열거 (§17)
        array_ns.insert("prototype".to_string(), Value::Obj(Rc::new(RefCell::new(array_proto))));
        mark_nonenum_all(&mut array_ns);
        let array_ns = Value::Obj(Rc::new(RefCell::new(array_ns)));
        env_declare(&global, "Array", Value::Native(Native::ArrayCtor));
        env_declare(&global, "RegExp", Value::Native(Native::RegExpCtor));
        env_declare(&global, "String", Value::Native(Native::StringCtor));
        env_declare(&global, "Number", Value::Native(Native::NumberCtor));
        env_declare(&global, "Boolean", Value::Native(Native::BooleanCtor));
        // Symbol 원시값 — Symbol()/Symbol.for/Symbol.iterator 등은 Native 로 제공.
        env_declare(&global, "Symbol", Value::Native(Native::SymbolCtor));
        // Node — 노드 타입/문서 위치 상수 (jQuery 등이 Node.ELEMENT_NODE 를 읽는다).
        let mut node_ns = ObjMap::new();
        for (k, v) in [
            ("ELEMENT_NODE", 1.0),
            ("ATTRIBUTE_NODE", 2.0),
            ("TEXT_NODE", 3.0),
            ("CDATA_SECTION_NODE", 4.0),
            ("COMMENT_NODE", 8.0),
            ("DOCUMENT_NODE", 9.0),
            ("DOCUMENT_TYPE_NODE", 10.0),
            ("DOCUMENT_FRAGMENT_NODE", 11.0),
            ("DOCUMENT_POSITION_DISCONNECTED", 1.0),
            ("DOCUMENT_POSITION_PRECEDING", 2.0),
            ("DOCUMENT_POSITION_FOLLOWING", 4.0),
            ("DOCUMENT_POSITION_CONTAINS", 8.0),
            ("DOCUMENT_POSITION_CONTAINED_BY", 16.0),
        ] {
            node_ns.insert(k.to_string(), Value::Num(v));
        }
        env_declare(&global, "Node", Value::Obj(Rc::new(RefCell::new(node_ns))));
        env_declare(&global, "Date", Value::Native(Native::DateCtor));
        env_declare(&global, "URL", Value::Native(Native::UrlCtor));
        env_declare(&global, "XMLHttpRequest", Value::Native(Native::XhrCtor));
        // Error 계열: 호출/ new 둘 다로 {name, message} 객체 생성
        for name in [
            "Error",
            "TypeError",
            "RangeError",
            "SyntaxError",
            "ReferenceError",
            "EvalError",
            "URIError",
            "AggregateError",
        ] {
            env_declare(&global, name, Value::Native(Native::ErrorCtor(name)));
        }
        // Function 생성자: Function(params.., body) 를 실제로 컴파일 (호출/ new 둘 다)
        env_declare(&global, "Function", Value::Native(Native::FunctionCtor));
        env_declare(&global, "eval", Value::Native(Native::Eval));
        env_declare(&global, "__kBrand", Value::Native(Native::Brand));
        // print: test262 의 async 하네스($DONE)가 결과를 알리는 통로다 (SpiderMonkey/V8
        // 셸의 print). console.log 와 같은 캡처 버퍼로 간다.
        env_declare(&global, "print", Value::Native(Native::Print));
        env_declare(&global, "__kBindElementClass", Value::Native(Native::BindElementClass));
        // Map / Set / WeakMap / WeakSet (약한 참조는 일반 Map/Set 으로 근사)
        env_declare(&global, "Map", Value::Native(Native::MapCtor));
        env_declare(&global, "WeakMap", Value::Native(Native::MapCtor));
        env_declare(&global, "Set", Value::Native(Native::SetCtor));
        env_declare(&global, "WeakSet", Value::Native(Native::SetCtor));
        for (iface, _) in natives::EVENT_IFACES {
            env_declare(&global, iface, Value::Native(Native::EventCtor(iface)));
        }
        env_declare(&global, "Proxy", Value::Native(Native::ProxyCtor));
        // localStorage: 페이지 수명 동안 실제로 동작하는 인메모리 스토리지
        let mut ls = ObjMap::new();
        ls.insert("getItem".to_string(), Value::Native(Native::LsGetItem));
        ls.insert("setItem".to_string(), Value::Native(Native::LsSetItem));
        ls.insert("removeItem".to_string(), Value::Native(Native::LsRemoveItem));
        ls.insert("clear".to_string(), Value::Native(Native::LsClear));
        // Storage 인터페이스: length(접근자) + key(i). 없으면 스토리지를 순회하는
        // 흔한 코드(for i < localStorage.length)가 죽는다.
        ls.insert("key".to_string(), Value::Native(Native::LsKey));
        ls.insert(
            "length".to_string(),
            Value::Accessor(Rc::new(AccessorPair {
                get: Some(Value::Native(Native::LsLength)),
                set: None,
            })),
        );
        let ls = Value::Obj(Rc::new(RefCell::new(ls)));
        env_declare(&global, "localStorage", ls.clone());
        env_declare(&global, "sessionStorage", ls.clone());
        // navigator / alert
        // navigator — 프레임워크가 흔히 읽는 속성들(없으면 .includes() 등에서 즉사).
        let mut nav = ObjMap::new();
        for (k, v) in [
            ("userAgent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Kestrel/0.1"),
            ("platform", "MacIntel"),
            ("vendor", ""),
            ("appName", "Netscape"),
            ("appVersion", "5.0 (Macintosh)"),
            ("product", "Gecko"),
            ("language", "en-US"),
            ("doNotTrack", "unspecified"),
        ] {
            nav.insert(k.to_string(), Value::Str(v.to_string()));
        }
        nav.insert(
            "languages".to_string(),
            Value::Arr(ArrayObj::new(vec![
                Value::Str("en-US".to_string()),
                Value::Str("en".to_string()),
            ])),
        );
        nav.insert("onLine".to_string(), Value::Bool(true));
        nav.insert("cookieEnabled".to_string(), Value::Bool(true));
        nav.insert("webdriver".to_string(), Value::Bool(false));
        nav.insert("hardwareConcurrency".to_string(), Value::Num(4.0));
        nav.insert("maxTouchPoints".to_string(), Value::Num(0.0));
        let nav = Value::Obj(Rc::new(RefCell::new(nav)));
        env_declare(&global, "navigator", nav.clone());
        env_declare(&global, "alert", Value::Native(Native::Alert));
        // window: 전역 객체 스텁 — 프로퍼티 읽기/쓰기는 되지만 전역 변수와
        // 연동되진 않음 (window.x = 1 후 x 로 읽기 미지원). 존재 자체가
        // "window 미정의" 즉사를 막는다. 필드 테스트 최다 런타임 에러.
        let mut window = ObjMap::new();
        window.insert("localStorage".to_string(), ls);
        window.insert("navigator".to_string(), nav);
        window.insert("addEventListener".to_string(), Value::Native(Native::AddGlobalListener));
        window.insert(
            "removeEventListener".to_string(),
            Value::Native(Native::RemoveGlobalListener),
        );
        window.insert(
            "dispatchEvent".to_string(),
            Value::Native(Native::DispatchGlobalEvent),
        );
        window.insert("getComputedStyle".to_string(), Value::Native(Native::GetComputedStyle));
        // 스크롤: 예전엔 window.scrollTo 자체가 없어 TypeError 로 스크립트가 죽었다.
        window.insert("scrollTo".to_string(), Value::Native(Native::ScrollTo));
        window.insert("scroll".to_string(), Value::Native(Native::ScrollTo));
        window.insert("scrollBy".to_string(), Value::Native(Native::ScrollBy));
        window.insert("requestAnimationFrame".to_string(), Value::Native(Native::SetTimeout));
        window.insert("cancelAnimationFrame".to_string(), Value::Native(Native::ClearTimer));
        window.insert("setTimeout".to_string(), Value::Native(Native::SetTimeout));
        window.insert("setInterval".to_string(), Value::Native(Native::SetInterval));
        // 이벤트 인터페이스 생성자 (각자 자기 prototype 을 갖는다)
        for (iface, _) in natives::EVENT_IFACES {
            window.insert(iface.to_string(), Value::Native(Native::EventCtor(iface)));
        }
        // history: pushState/replaceState 는 location 을 실제로 갱신한다(SPA 라우터가
        // 그 뒤 location.pathname 을 읽는다). 예전엔 no-op 이라 라우팅이 조용히 어긋났다.
        // back/forward/go 는 실제 세션 이동이라 정적 렌더에선 하지 않는다.
        let mut history = ObjMap::new();
        history.insert("pushState".to_string(), Value::Native(Native::HistoryPushState));
        history.insert("replaceState".to_string(), Value::Native(Native::HistoryReplaceState));
        for m in ["back", "forward", "go"] {
            history.insert(m.to_string(), Value::Native(Native::Noop));
        }
        history.insert("length".to_string(), Value::Num(1.0));
        history.insert("state".to_string(), Value::Null);
        history.insert("scrollRestoration".to_string(), Value::Str("auto".to_string()));
        let history = Value::Obj(Rc::new(RefCell::new(history)));
        window.insert("history".to_string(), history.clone());
        // 뷰포트/화면 메트릭 (반응형 스크립트가 흔히 읽음). 렌더 뷰포트에 맞춤.
        for (k, v) in [
            ("innerWidth", 1000.0),
            ("innerHeight", 800.0),
            ("outerWidth", 1000.0),
            ("outerHeight", 800.0),
            ("devicePixelRatio", 1.0),
            ("scrollX", 0.0),
            ("scrollY", 0.0),
            ("pageXOffset", 0.0),
            ("pageYOffset", 0.0),
        ] {
            window.insert(k.to_string(), Value::Num(v));
        }
        let mut screen = ObjMap::new();
        for (k, v) in [("width", 1000.0), ("height", 800.0), ("availWidth", 1000.0), ("availHeight", 800.0), ("colorDepth", 24.0), ("pixelDepth", 24.0)] {
            screen.insert(k.to_string(), Value::Num(v));
        }
        window.insert("screen".to_string(), Value::Obj(Rc::new(RefCell::new(screen.clone()))));
        let window_obj = Rc::new(RefCell::new(window));
        let window = Value::Obj(window_obj.clone());
        // self / globalThis 는 전역 객체(window) 별칭 (구글/워커 코드)
        env_declare(&global, "window", window.clone());
        env_declare(&global, "self", window.clone());
        // top/parent/frames 는 (프레임 없으니) window 자신. history 전역.
        env_declare(&global, "top", window.clone());
        env_declare(&global, "parent", window.clone());
        env_declare(&global, "frames", window.clone());
        env_declare(&global, "history", history);
        if let Value::Obj(w) = &window {
            w.borrow_mut().insert("top".to_string(), window.clone());
            w.borrow_mut().insert("parent".to_string(), window.clone());
            w.borrow_mut().insert("self".to_string(), window.clone());
        }
        env_declare(&global, "screen", Value::Obj(Rc::new(RefCell::new(screen))));
        // 최상위 this = window (sloppy 스크립트: `(function(){this.x=…}).call(this)` 등)
        env_declare(&global, "this", window.clone());
        env_declare(&global, "globalThis", window);
        // Promise 생성자 (new Promise(executor)) + 정적 메서드(member_get 에서 제공)
        env_declare(&global, "Promise", Value::Native(Native::PromiseCtor));
        // fetch(url) — 동기 HTTP 후 resolved Promise(Response) 반환
        env_declare(&global, "fetch", Value::Native(Native::Fetch));
        // WebAssembly 의 밑바닥 훅. 표면(WebAssembly.Module/Instance/Memory…)은 프렐류드가
        // JS 로 만든다 — 그래야 Memory.buffer 가 진짜 ArrayBuffer(프로토타입 포함)가 되고
        // new Uint8Array(memory.buffer) 가 **살아있는 뷰**가 된다.
        env_declare(&global, "WebSocket", Value::Native(Native::WebSocketCtor));
        env_declare(&global, "__kZeroBytes", Value::Native(Native::ZeroBytes));
        env_declare(&global, "__kWasmValidate", Value::Native(Native::WasmValidate));
        env_declare(&global, "__kWasmCompile", Value::Native(Native::WasmCompile));
        env_declare(&global, "__kWasmMemPages", Value::Native(Native::WasmMemPages));
        env_declare(
            &global,
            "__kWasmRegisterMemory",
            Value::Native(Native::WasmRegisterMemory),
        );
        env_declare(&global, "__kWasmGrow", Value::Native(Native::WasmGrow));
        env_declare(
            &global,
            "__kWasmInstantiate",
            Value::Native(Native::WasmInstantiate),
        );
        // Function.prototype (call/apply/bind) — 폴리필이 Function.prototype.call.apply
        // 등으로 광범위하게 참조. 정체성 보존 위해 필드로 보관.
        let mut fn_proto = ObjMap::new();
        fn_proto.insert("call".to_string(), Value::Native(Native::FnCall));
        fn_proto.insert("apply".to_string(), Value::Native(Native::FnApply));
        fn_proto.insert("bind".to_string(), Value::Native(Native::FnBind));
        // Function.prototype.toString — core-js 등이 uncurryThis 로 참조
        fn_proto.insert("toString".to_string(), Value::Native(Native::FnToString));
        // Function.prototype[@@hasInstance] (§20.2.3.6) — OrdinaryHasInstance. {w:f,e:f,c:f}.
        fn_proto.insert("\u{0}@@hasInstance".to_string(), Value::Native(Native::FnHasInstance));
        set_prop_attrs(&mut fn_proto, "\u{0}@@hasInstance", 0);
        let fn_proto = Value::Obj(Rc::new(RefCell::new(fn_proto)));
        // Function.prototype.[[Prototype]] === Object.prototype (§20.2.3): 함수도 ordinary
        // object 라 Object.prototype 메서드(hasOwnProperty/valueOf/isPrototypeOf/
        // propertyIsEnumerable/toLocaleString)를 상속한다. member_get 의 fn_static_lookup 이
        // fn → Function.prototype → Object.prototype 체인을 걸어 이를 해석한다.
        if let (Value::Obj(fp), Value::Obj(ons)) = (&fn_proto, &object_ns) {
            if let Some(op) = ons.borrow().get("prototype").cloned() {
                fp.borrow_mut().insert("__proto__".to_string(), op);
            }
        }
        // String.prototype: 문자열 메서드 (String.prototype.slice.call(x) 지원)
        let mut string_proto = ObjMap::new();
        // 문자열 프로토타입 메서드 전량 — member_get 의 인스턴스 조회와 같은 목록이어야
        // String.prototype.X 가 own 프로퍼티로 존재한다(hasOwnProperty/name/length/
        // not-a-constructor 검사). trimLeft/trimRight 는 trimStart/trimEnd 의 별칭이라
        // 같은 함수(=같은 name)를 가리킨다.
        for (name, op) in [
            ("charAt", StrOp::CharAt),
            ("charCodeAt", StrOp::CharCodeAt),
            ("codePointAt", StrOp::CodePointAt),
            ("indexOf", StrOp::IndexOf),
            ("lastIndexOf", StrOp::LastIndexOf),
            ("slice", StrOp::Slice),
            ("substring", StrOp::Substring),
            ("substr", StrOp::Substr),
            ("split", StrOp::Split),
            ("toUpperCase", StrOp::Upper),
            ("toLowerCase", StrOp::Lower),
            ("toLocaleUpperCase", StrOp::LocaleUpper),
            ("toLocaleLowerCase", StrOp::LocaleLower),
            ("trim", StrOp::Trim),
            ("trimStart", StrOp::TrimStart),
            ("trimEnd", StrOp::TrimEnd),
            ("trimLeft", StrOp::TrimStart),
            ("trimRight", StrOp::TrimEnd),
            ("replace", StrOp::Replace),
            ("replaceAll", StrOp::ReplaceAll),
            ("includes", StrOp::Includes),
            ("startsWith", StrOp::StartsWith),
            ("endsWith", StrOp::EndsWith),
            ("match", StrOp::Match),
            ("matchAll", StrOp::MatchAll),
            ("search", StrOp::Search),
            ("concat", StrOp::Concat),
            ("at", StrOp::At),
            ("localeCompare", StrOp::LocaleCompare),
            ("padStart", StrOp::PadStart),
            ("padEnd", StrOp::PadEnd),
            ("repeat", StrOp::Repeat),
        ] {
            string_proto.insert(name.to_string(), Value::Native(Native::Str(op)));
        }
        // String.prototype.toString/valueOf — thisStringValue (§22.1.3.28/.29). 원시
        // 문자열/String 래퍼면 그 문자열, 아니면 TypeError. String.prototype 자신은
        // [[StringData]]="" 인 원시 래퍼라 String.prototype.toString() === "".
        string_proto.insert("toString".to_string(), Value::Native(Native::PrimToString(PrimBrand::String)));
        string_proto.insert("valueOf".to_string(), Value::Native(Native::PrimValueOf(PrimBrand::String)));
        // String.prototype.constructor === String (§22.1.3.1), 비열거.
        string_proto.insert("constructor".to_string(), Value::Native(Native::StringCtor));
        // 모든 내장 메서드는 비열거 (§17) — 예전엔 constructor/toString/valueOf 만 표식해
        // trim/charAt/… 이 열거 가능이었다(Object.keys(String.prototype) 누출).
        mark_nonenum_all(&mut string_proto);
        string_proto.insert(WRAPPER_SLOT.to_string(), Value::Str(String::new()));
        let string_proto = Value::Obj(Rc::new(RefCell::new(string_proto)));
        // Number/Boolean/RegExp.prototype — 원시값 메서드 네이티브를 재사용.
        let mk_proto = |pairs: Vec<(&str, Native)>| {
            let mut m = ObjMap::new();
            for (k, n) in pairs {
                m.insert(k.to_string(), Value::Native(n));
                // 내장 메서드는 non-enumerable, writable, configurable (§17). 예전엔
                // 표식이 없어 getOwnPropertyDescriptor 가 enumerable:true 로 보고했다.
                m.insert(nonenum_marker(k), Value::Bool(true));
            }
            Value::Obj(Rc::new(RefCell::new(m)))
        };
        let number_proto = mk_proto(vec![
            ("toString", Native::PrimToString(PrimBrand::Number)),
            ("toLocaleString", Native::PrimToString(PrimBrand::Number)),
            ("toFixed", Native::NumToFixed),
            ("toPrecision", Native::NumToPrecision),
            ("toExponential", Native::NumToExponential),
            ("valueOf", Native::PrimValueOf(PrimBrand::Number)),
        ]);
        let boolean_proto = mk_proto(vec![
            ("toString", Native::PrimToString(PrimBrand::Boolean)),
            ("valueOf", Native::PrimValueOf(PrimBrand::Boolean)),
        ]);
        // BigInt.prototype (§21.2.3) — toString/toLocaleString/valueOf + @@toStringTag.
        // 예전엔 BigInt.prototype 자체가 없어 BigInt.prototype.x=… 가 "undefined 에 할당"
        // 으로 죽었다(Intl 폴리필/toLocaleString 재정의가 통째로 불가).
        let bigint_proto = mk_proto(vec![
            ("toString", Native::BigIntToString),
            ("toLocaleString", Native::BigIntToString),
            ("valueOf", Native::ValueOfSelf),
        ]);
        if let Value::Obj(m) = &bigint_proto {
            let mut b = m.borrow_mut();
            b.insert("\u{0}@@toStringTag".to_string(), Value::Str("BigInt".to_string()));
            b.insert(nonenum_marker("\u{0}@@toStringTag"), Value::Bool(true));
        }
        let regexp_proto = mk_proto(vec![
            ("exec", Native::RegexExec),
            ("test", Native::RegexTest),
            ("toString", Native::ValueToStr),
        ]);
        // flags/source/global/… 는 RegExp.prototype 의 접근자다 (§22.2.6) — 인스턴스의
        // own 데이터가 아니다. getOwnPropertyDescriptor(RegExp.prototype,'flags').get 이
        // 함수여야 하는 test262 검사가 다수. 각 getter 는 this 정규식에서 계산한다.
        if let Value::Obj(m) = &regexp_proto {
            let mut b = m.borrow_mut();
            for (name, kind, _) in natives::RegexAccessor::table() {
                b.insert(
                    name.to_string(),
                    Value::Accessor(AccessorPair::getter(Value::Native(Native::RegexGet(*kind)))),
                );
            }
            // Symbol.match/replace/split/search/matchAll 메서드 (§22.2.6). str.match(re)
            // 등이 표준상 위임하는 대상. 내부 심볼 키 \0@@match 등으로 얹는다.
            for (sym, op) in [
                ("\u{0}@@match", StrOp::Match),
                ("\u{0}@@matchAll", StrOp::MatchAll),
                ("\u{0}@@replace", StrOp::Replace),
                ("\u{0}@@search", StrOp::Search),
                ("\u{0}@@split", StrOp::Split),
            ] {
                b.insert(sym.to_string(), Value::Native(Native::RegexSym(op)));
            }
        }
        // Map/Set/Date/Symbol.prototype — 인스턴스 멤버 해석과 같은 Native 를 얹는다.
        // 번들/core-js 의 Map.prototype.get, uncurryThis(Set.prototype.has) 등이 참조.
        let map_proto = mk_proto(vec![
            ("get", Native::Map(MapOp::Get)),
            ("set", Native::Map(MapOp::Set)),
            ("has", Native::Map(MapOp::Has)),
            ("delete", Native::Map(MapOp::Delete)),
            ("clear", Native::Map(MapOp::Clear)),
            ("forEach", Native::Map(MapOp::ForEach)),
            ("keys", Native::Map(MapOp::Keys)),
            ("values", Native::Map(MapOp::Values)),
            ("entries", Native::Map(MapOp::Entries)),
            ("\u{0}@@iterator", Native::Map(MapOp::Entries)),
        ]);
        let set_proto = mk_proto(vec![
            ("add", Native::Set(SetOp::Add)),
            ("has", Native::Set(SetOp::Has)),
            ("delete", Native::Set(SetOp::Delete)),
            ("clear", Native::Set(SetOp::Clear)),
            ("forEach", Native::Set(SetOp::ForEach)),
            ("keys", Native::Set(SetOp::Values)),
            ("values", Native::Set(SetOp::Values)),
            ("entries", Native::Set(SetOp::Entries)),
            ("\u{0}@@iterator", Native::Set(SetOp::Values)),
            // ES2024 집합 연산 (§24.2.4)
            ("union", Native::Set(SetOp::Union)),
            ("intersection", Native::Set(SetOp::Intersection)),
            ("difference", Native::Set(SetOp::Difference)),
            ("symmetricDifference", Native::Set(SetOp::SymmetricDifference)),
            ("isSubsetOf", Native::Set(SetOp::IsSubsetOf)),
            ("isSupersetOf", Native::Set(SetOp::IsSupersetOf)),
            ("isDisjointFrom", Native::Set(SetOp::IsDisjointFrom)),
        ]);
        let date_proto = mk_proto(vec![
            ("getTime", Native::DateMethod(DateField::Time)),
            ("valueOf", Native::DateMethod(DateField::Time)),
            ("getFullYear", Native::DateMethod(DateField::FullYear)),
            ("getMonth", Native::DateMethod(DateField::Month)),
            ("getDate", Native::DateMethod(DateField::Date)),
            ("getDay", Native::DateMethod(DateField::Day)),
            ("getHours", Native::DateMethod(DateField::Hours)),
            ("getMinutes", Native::DateMethod(DateField::Minutes)),
            ("getSeconds", Native::DateMethod(DateField::Seconds)),
            ("getMilliseconds", Native::DateMethod(DateField::Ms)),
            ("getTimezoneOffset", Native::DateMethod(DateField::TimezoneOffset)),
            ("setTime", Native::DateMethod(DateField::SetTime)),
            ("setFullYear", Native::DateMethod(DateField::SetFullYear)),
            ("setMonth", Native::DateMethod(DateField::SetMonth)),
            ("setDate", Native::DateMethod(DateField::SetDate)),
            ("setHours", Native::DateMethod(DateField::SetHours)),
            ("setMinutes", Native::DateMethod(DateField::SetMinutes)),
            ("setSeconds", Native::DateMethod(DateField::SetSeconds)),
            ("setMilliseconds", Native::DateMethod(DateField::SetMs)),
            ("toISOString", Native::DateMethod(DateField::ToIso)),
            ("toJSON", Native::DateMethod(DateField::ToJson)),
            ("toString", Native::DateMethod(DateField::ToStr)),
            ("toDateString", Native::DateMethod(DateField::ToDateStr)),
            ("toTimeString", Native::DateMethod(DateField::ToTimeStr)),
            ("toUTCString", Native::DateMethod(DateField::ToUtcStr)),
            ("toGMTString", Native::DateMethod(DateField::ToUtcStr)),
            ("toLocaleString", Native::DateMethod(DateField::ToLocaleStr)),
            ("toLocaleDateString", Native::DateMethod(DateField::ToLocaleDateStr)),
            ("toLocaleTimeString", Native::DateMethod(DateField::ToLocaleTimeStr)),
            ("getUTCFullYear", Native::DateMethod(DateField::UtcFullYear)),
            ("getUTCMonth", Native::DateMethod(DateField::UtcMonth)),
            ("getUTCDate", Native::DateMethod(DateField::UtcDate)),
            ("getUTCDay", Native::DateMethod(DateField::UtcDay)),
            ("getUTCHours", Native::DateMethod(DateField::UtcHours)),
            ("getUTCMinutes", Native::DateMethod(DateField::UtcMinutes)),
            ("getUTCSeconds", Native::DateMethod(DateField::UtcSeconds)),
            ("getUTCMilliseconds", Native::DateMethod(DateField::UtcMs)),
            ("setUTCFullYear", Native::DateMethod(DateField::SetUtcFullYear)),
            ("setUTCMonth", Native::DateMethod(DateField::SetUtcMonth)),
            ("setUTCDate", Native::DateMethod(DateField::SetUtcDate)),
            ("setUTCHours", Native::DateMethod(DateField::SetUtcHours)),
            ("setUTCMinutes", Native::DateMethod(DateField::SetUtcMinutes)),
            ("setUTCSeconds", Native::DateMethod(DateField::SetUtcSeconds)),
            ("setUTCMilliseconds", Native::DateMethod(DateField::SetUtcMs)),
            ("getYear", Native::DateMethod(DateField::GetYear)),
            ("setYear", Native::DateMethod(DateField::SetYear)),
            ("\u{0}@@toPrimitive", Native::DateMethod(DateField::ToPrimitive)),
        ]);
        let symbol_proto = mk_proto(vec![
            // brand 체크(thisSymbolValue)하는 toString/valueOf — 심볼/심볼래퍼가 아니면 TypeError.
            ("toString", Native::PrimToString(PrimBrand::Symbol)),
            ("valueOf", Native::PrimValueOf(PrimBrand::Symbol)),
        ]);
        // X.prototype.constructor === X (§19.x/§20~22), 전부 비열거 (§17). 예전엔
        // 원시 래퍼/빌트인 프로토타입에 constructor 링크가 없어 대량으로 깨졌다.
        let link_ctor = |proto: &Value, ctor: Native| {
            if let Value::Obj(m) = proto {
                let mut b = m.borrow_mut();
                b.insert("constructor".to_string(), Value::Native(ctor));
                b.insert(nonenum_marker("constructor"), Value::Bool(true));
            }
        };
        link_ctor(&number_proto, Native::NumberCtor);
        link_ctor(&boolean_proto, Native::BooleanCtor);
        link_ctor(&bigint_proto, Native::BigIntCtor);
        link_ctor(&symbol_proto, Native::SymbolCtor);
        link_ctor(&regexp_proto, Native::RegExpCtor);
        link_ctor(&map_proto, Native::MapCtor);
        link_ctor(&set_proto, Native::SetCtor);
        link_ctor(&date_proto, Native::DateCtor);
        // Date.prototype[Symbol.toPrimitive] 는 { writable:false, enumerable:false,
        // configurable:true } (§21.4.4.45). mk_proto 기본(writable:true)과 달라 보정.
        if let Value::Obj(m) = &date_proto {
            set_prop_attrs(&mut m.borrow_mut(), "\u{0}@@toPrimitive", ATTR_CONFIGURABLE);
        }
        // Map/Set.prototype.size 는 프로토타입 accessor(getter) 다 — 인스턴스 own 아님.
        // getOwnPropertyDescriptor(Set.prototype,'size').get 검사가 이걸 본다. 비열거.
        let add_getter = |proto: &Value, name: &str, g: Native| {
            if let Value::Obj(m) = proto {
                let mut b = m.borrow_mut();
                b.insert(
                    name.to_string(),
                    Value::Accessor(AccessorPair::getter(Value::Native(g))),
                );
                b.insert(nonenum_marker(name), Value::Bool(true));
            }
        };
        add_getter(&map_proto, "size", Native::MapSize);
        add_getter(&set_proto, "size", Native::SetSize);
        // Symbol.prototype.description 도 프로토타입 accessor(getter) 다 (§20.4.3.2).
        add_getter(&symbol_proto, "description", Native::SymbolDescGet);
        // Symbol.prototype[Symbol.toPrimitive] (§20.4.3.5): { writable:false, enumerable:false,
        // configurable:true }. Symbol.prototype[Symbol.toStringTag]="Symbol" (§20.4.3.6): 동일 속성.
        if let Value::Obj(m) = &symbol_proto {
            let mut b = m.borrow_mut();
            b.insert("\u{0}@@toPrimitive".to_string(), Value::Native(Native::SymbolToPrimitive));
            b.insert(nonenum_marker("\u{0}@@toPrimitive"), Value::Bool(true));
            b.insert("\u{0}@@toStringTag".to_string(), Value::Str("Symbol".to_string()));
            b.insert(nonenum_marker("\u{0}@@toStringTag"), Value::Bool(true));
            drop(b);
            // 기본 writable(true)과 다르게 둘 다 non-writable, configurable 로 보정.
            set_prop_attrs(&mut m.borrow_mut(), "\u{0}@@toPrimitive", ATTR_CONFIGURABLE);
            set_prop_attrs(&mut m.borrow_mut(), "\u{0}@@toStringTag", ATTR_CONFIGURABLE);
        }
        // Number.prototype/Boolean.prototype 자신이 [[PrimitiveValue]] 슬롯을 가진
        // 원시 래퍼다 (§21.1.3/§20.3.3) — thisNumberValue(Number.prototype)=+0 등.
        if let Value::Obj(m) = &number_proto {
            m.borrow_mut().insert(WRAPPER_SLOT.to_string(), Value::Num(0.0));
        }
        if let Value::Obj(m) = &boolean_proto {
            m.borrow_mut().insert(WRAPPER_SLOT.to_string(), Value::Bool(false));
        }
        // Error.prototype 및 서브타입 prototype (ECMA-262 §20.5.3, §20.5.6.3).
        // NativeError.prototype 의 [[Prototype]] 은 Error.prototype 이고,
        // 각자 자기 name 과 constructor 를 갖는다. 프로퍼티는 전부 비열거.
        let error_proto = mk_proto(vec![("toString", Native::ErrorToString)]);
        // Error.prototype.stack — 프로토타입 accessor(get/set), 비열거·configurable.
        // 인스턴스는 [[ErrorData]]/캡처스택을 내부 슬롯에 들고, getter 가 그걸 읽는다.
        // 서브타입 prototype 은 error_proto 를 상속하므로 여기 한 번만 둔다.
        if let Value::Obj(m) = &error_proto {
            let mut b = m.borrow_mut();
            b.insert(
                "stack".to_string(),
                Value::Accessor(Rc::new(AccessorPair {
                    get: Some(Value::Native(Native::ErrorStackGet)),
                    set: Some(Value::Native(Native::ErrorStackSet)),
                })),
            );
            b.insert(nonenum_marker("stack"), Value::Bool(true));
        }
        let mut error_protos: Vec<(&'static str, Value)> = Vec::new();
        for kind in ERROR_KINDS {
            let proto = if kind == "Error" {
                error_proto.clone()
            } else {
                let mut m = ObjMap::new();
                m.insert("__proto__".to_string(), error_proto.clone());
                Value::Obj(Rc::new(RefCell::new(m)))
            };
            if let Value::Obj(m) = &proto {
                let mut b = m.borrow_mut();
                b.insert("name".to_string(), Value::Str(kind.to_string()));
                b.insert("message".to_string(), Value::Str(String::new()));
                b.insert("constructor".to_string(), Value::Native(Native::ErrorCtor(kind)));
                for k in ["name", "message", "constructor", "toString"] {
                    b.insert(nonenum_marker(k), Value::Bool(true));
                }
            }
            error_protos.push((kind, proto));
        }
        // 이벤트 인터페이스 prototype: MouseEvent.prototype 의 [[Prototype]] 은
        // UIEvent.prototype 이고, 그건 다시 Event.prototype 이다 (진짜 상속 체인).
        let mut event_protos: Vec<(&'static str, Value)> = Vec::new();
        for (iface, parent) in natives::EVENT_IFACES {
            let mut m = ObjMap::new();
            if !parent.is_empty() {
                if let Some((_, p)) = event_protos.iter().find(|(k, _)| k == parent) {
                    m.insert("__proto__".to_string(), p.clone());
                }
            }
            m.insert("constructor".to_string(), Value::Native(Native::EventCtor(iface)));
            m.insert(nonenum_marker("constructor"), Value::Bool(true));
            event_protos.push((iface, Value::Obj(Rc::new(RefCell::new(m)))));
        }
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 | 1)
            .unwrap_or(0x9e3779b9);
        Interp {
            global,
            console: Vec::new(),
            steps: 0,
            script_start: None,
            script_budget_ms: std::env::var("KESTREL_SCRIPT_BUDGET_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(SCRIPT_BUDGET_MS),
            js_spent_ms: 0,
            total_budget_ms: std::env::var("KESTREL_TOTAL_BUDGET_MS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(TOTAL_BUDGET_MS),
            js_stack: Vec::new(),
            err_stack: None,
            dom: None,
            handlers: Vec::new(),
            mutation_scheduled: false,
            shadow_hosts: std::collections::HashSet::new(),
            scroll_x: 0.0,
            scroll_y: 0.0,
            active_element: None,
            layout_ctx: None,
            priv_id: 0,
            priv_counter: 0,
            dom_props: HashMap::new(),
            element_classes: HashMap::new(),
            inline_handlers: HashMap::new(),
            inline_fns: HashMap::new(),
            css_epoch: 0,
            css_applied_epoch: 0,
            layout_version: None,
            layout_rects: std::collections::HashMap::new(),
            computed_styles: std::collections::HashMap::new(),
            canvas_warned: std::collections::HashSet::new(),
            canvas_cmds: std::collections::HashMap::new(),
            global_handlers: Vec::new(),
            rng: seed,
            thrown: None,
            storage: Vec::new(),
            timers: Vec::new(),
            cleared: std::collections::HashSet::new(),
            next_timer_id: 1,
            microtasks: std::collections::VecDeque::new(),
            fn_proto,
            map_proto,
            set_proto,
            error_proto,
            error_protos,
            event_protos,
            object_ns,
            array_ns,
            date_proto,
            symbol_proto,
            string_proto,
            number_proto,
            boolean_proto,
            bigint_proto,
            regexp_proto,
            base_url: None,
            module_sources: HashMap::new(),
            import_map: Vec::new(),
            navigate_to: None,
            current_script: None,
            written_scripts: Vec::new(),
            module_namespaces: HashMap::new(),
            lenient: std::env::var("KESTREL_LENIENT").is_ok(),
            lenient_hits: std::collections::HashMap::new(),
            pending_label: None,
            sym_counter: 0,
            sym_registry: HashMap::new(),
            new_target: None,
            window_obj,
            native_props: HashMap::new(),
            integrity: HashMap::new(),
            revoked_proxies: std::collections::HashSet::new(),
            layout_metrics: std::collections::HashMap::new(),
            sockets: Vec::new(),
            pending_ws_open: Vec::new(),
            pending_ws_error: Vec::new(),
            wasm_modules: Vec::new(),
            wasm_memories: Vec::new(),
            wasm_instances: Vec::new(),
            fetch_bodies: Vec::new(),
        }
    }

    // 새 pending Promise (Obj 표현: 상태·값·대기콜백을 맵에 저장, then/catch 는 Native)
    pub(super) fn new_promise(&self) -> Value {
        let mut m = ObjMap::new();
        m.insert("\u{0}isPromise".to_string(), Value::Bool(true));
        m.insert("\u{0}state".to_string(), Value::Str("pending".to_string()));
        m.insert("\u{0}value".to_string(), Value::Undefined);
        m.insert("\u{0}cbs".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
        // then/catch/finally 는 own 프로퍼티가 아니라 member_get 에서 해석(비열거).
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // async 제너레이터 결과 {value,done} 의 value 가 thenable 이면 await 로 풀어
    // 넣는다 (§27.6.3.8 AsyncGeneratorYield 는 yield 값을 Await 한다).
    pub(super) fn await_iter_result_value(&mut self, res: Value) -> Result<Value, String> {
        if let Value::Obj(o) = &res {
            let val = o.borrow().get("value").cloned().unwrap_or(Value::Undefined);
            if is_promise(&val) {
                let awaited = self.await_value(val)?;
                o.borrow_mut().insert("value".to_string(), awaited);
            }
        }
        Ok(res)
    }

    // promise 를 값으로 이행. 값이 또 promise 면 그것이 이행될 때 이어서 이행(체이닝).
    // promise 를 이행(fulfilled)으로 정착. 값이 thenable 이면 그 상태를 채택.
    pub(super) fn resolve_promise(&mut self, p: &Value, v: Value) {
        if is_promise(&v) {
            // p 는 v 의 상태를 채택: v 이행 → Identity 로 p 이행, v 거부 → 전파로 p 거부
            self.promise_then(&v, Value::Native(Native::Identity), Value::Undefined, p.clone());
            return;
        }
        self.settle(p, true, v);
    }

    // promise 를 거부(rejected)로 정착.
    pub(super) fn reject_promise(&mut self, p: &Value, reason: Value) {
        self.settle(p, false, reason);
    }

    // 공통 정착: pending 일 때만 상태/값을 확정하고 대기 반응을 마이크로태스크로 스케줄.
    fn settle(&mut self, p: &Value, fulfilled: bool, value: Value) {
        let Value::Obj(o) = p else { return };
        {
            let m = o.borrow();
            if !matches!(m.get("\u{0}state"), Some(Value::Str(s)) if s == "pending") {
                return; // 이미 정착 — 한 번만
            }
        }
        let cbs = {
            let mut m = o.borrow_mut();
            let state = if fulfilled { "fulfilled" } else { "rejected" };
            m.insert("\u{0}state".to_string(), Value::Str(state.to_string()));
            m.insert("\u{0}value".to_string(), value.clone());
            match m.get("\u{0}cbs") {
                Some(Value::Arr(a)) => {
                    let v = a.borrow().clone();
                    a.borrow_mut().clear();
                    v
                }
                _ => Vec::new(),
            }
        };
        for reaction in cbs {
            self.schedule_reaction(&reaction, fulfilled, value.clone());
        }
    }

    // 반응 레코드 {onF, onR, dep} 에서 상태에 맞는 핸들러를 골라 마이크로태스크로.
    fn schedule_reaction(&mut self, reaction: &Value, fulfilled: bool, value: Value) {
        if let Value::Obj(c) = reaction {
            let cm = c.borrow();
            let handler = if fulfilled { cm.get("onF") } else { cm.get("onR") }
                .cloned()
                .unwrap_or(Value::Undefined);
            let dep = cm.get("dep").cloned().unwrap_or(Value::Undefined);
            self.microtasks.push_back((handler, value, dep, !fulfilled));
        }
    }

    // p.then(onF, onR) → dep promise 반환. 정착돼 있으면 즉시 마이크로태스크, 아니면 대기열에.
    fn promise_then(&mut self, p: &Value, on_f: Value, on_r: Value, dep: Value) -> Value {
        let Value::Obj(o) = p else { return dep };
        let (state, value) = {
            let m = o.borrow();
            (
                match m.get("\u{0}state") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "pending".into(),
                },
                m.get("\u{0}value").cloned().unwrap_or(Value::Undefined),
            )
        };
        match state.as_str() {
            "fulfilled" => self.microtasks.push_back((on_f, value, dep.clone(), false)),
            "rejected" => self.microtasks.push_back((on_r, value, dep.clone(), true)),
            _ => {
                // 대기: {onF, onR, dep} 를 __cbs 에 추가
                let mut entry = ObjMap::new();
                entry.insert("onF".to_string(), on_f);
                entry.insert("onR".to_string(), on_r);
                entry.insert("dep".to_string(), dep.clone());
                let entry = Value::Obj(Rc::new(RefCell::new(entry)));
                if let Some(Value::Arr(a)) = o.borrow().get("\u{0}cbs") {
                    a.borrow_mut().push(entry);
                }
            }
        }
        dep
    }

    // 마이크로태스크 드레인: 콜백 실행 → 그 결과로 의존 promise 이행 (체이닝).
    // 값 타입에 대응하는 전역 생성자 (x.constructor 용).
    // 진짜 네이티브 오류 객체를 만든다 (ECMA-262 §20.5.1.1).
    // message 는 인자가 있을 때만 own 프로퍼티이고, 비열거다 — Object.keys(new Error('x'))
    // 는 [] 여야 한다. __proto__ 는 해당 종류의 prototype 이므로 instanceof 와
    // e.constructor 가 프로토타입 체인만으로 표준대로 동작한다.
    pub(super) fn make_error(&self, kind: &str, message: Option<String>) -> Value {
        let mut map = ObjMap::new();
        if let Some(msg) = message {
            map.insert("message".to_string(), Value::Str(msg));
            map.insert(nonenum_marker("message"), Value::Bool(true));
        }
        // [[ErrorData]] 내부 슬롯 — Error.isError 판별과 stack getter 의 brand 체크용.
        map.insert("\u{0}errdata".to_string(), Value::Bool(true));
        // 캡처된 스택은 내부 슬롯에 둔다. Error.prototype.stack 은 인스턴스 own 데이터가
        // 아니라 프로토타입 **accessor** 다 (Error Stacks). getter 가 이 슬롯을 읽는다.
        map.insert(
            "\u{0}errstack".to_string(),
            Value::Str(self.err_stack.clone().unwrap_or_default().join("\n")),
        );
        let proto = self
            .error_protos
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| self.error_proto.clone());
        map.insert("__proto__".to_string(), proto);
        Value::Obj(Rc::new(RefCell::new(map)))
    }

    /// InstallErrorCause (§20.5.8.1, ES2022): options 가 객체이고 "cause" 를 가지면
    /// error 에 비열거 own "cause" 를 심는다. AggregateError 는 (errors, message,
    /// options) 라 options 가 args[2], 나머지 Error 계열은 args[1].
    pub(super) fn install_error_cause(
        &mut self,
        err: &Value,
        args: &[Value],
        name: &str,
    ) -> Result<(), String> {
        let opt_idx = if name == "AggregateError" { 2 } else { 1 };
        if let Some(opts) = args.get(opt_idx) {
            if self.has_property(opts, "cause") {
                let cause = self.member_get(opts, "cause")?;
                if let Value::Obj(m) = err {
                    let mut b = m.borrow_mut();
                    b.insert("cause".to_string(), cause);
                    b.insert(nonenum_marker("cause"), Value::Bool(true));
                }
            }
        }
        Ok(())
    }

    // eval (§19.2.1 / §19.2.1.1 PerformEval).
    // - 인자가 문자열이 아니면 그대로 돌려준다 (표준).
    // - 파싱 실패는 SyntaxError 객체를 던진다 (문자열이 아니라).
    // - direct 면 호출 지점의 스코프에서, indirect 면 전역 스코프에서 평가한다.
    //   번들이 globalThis 를 찾을 때 쓰는 (0,eval)('this') 가 이 구분에 의존한다.
    // - 완료값(마지막 표현식 문의 값)을 반환한다.
    pub(super) fn do_eval(
        &mut self,
        arg: Value,
        var_env: &Rc<RefCell<Env>>,
        lex_env: &Rc<RefCell<Env>>,
    ) -> Result<Value, String> {
        let Value::Str(src) = arg else {
            return Ok(arg);
        };
        let program = match parse(&src) {
            Ok(p) => p,
            Err(e) => return Err(self.throw_error("SyntaxError", e)),
        };
        // var 와 함수 선언은 '변수 환경'(호출자의 함수/전역 스코프)에 만든다.
        // let/const 는 eval 전용 렉시컬 스코프에 갇힌다 (§19.2.1.1).
        hoist_vars(&program, var_env);
        match self.exec_block(&program, lex_env) {
            Ok(Flow::Normal(v)) | Ok(Flow::Return(v)) => Ok(v),
            Ok(_) => Ok(Value::Undefined),
            Err(e) => Err(e),
        }
    }

    // §10.2.9 SetFunctionName / NamedEvaluation:
    // 익명 함수(또는 익명 클래스)가 이름 있는 바인딩에 대입되면 그 이름을 갖는다.
    //   var f = function(){};  f.name === "f"
    //   const g = () => {};    g.name === "g"
    // 이미 이름이 있으면 (명명 함수식) 덮지 않는다.
    fn set_fn_name(v: &Value, name: &str) {
        match v {
            Value::Fn(f) if f.name.borrow().is_empty() => {
                *f.name.borrow_mut() = name.to_string();
            }
            Value::Class(c) if c.name.borrow().is_empty() => {
                *c.name.borrow_mut() = name.to_string();
            }
            _ => {}
        }
    }

    pub(super) fn event_proto(&self, iface: &str) -> Option<Value> {
        self.event_protos.iter().find(|(k, _)| *k == iface).map(|(_, p)| p.clone())
    }

    // 이 값이 그 private 이름을 갖는가 (#x in obj). 필드든 메서드든 접근자든 센다.
    fn has_private(&self, v: &Value, name: &str) -> bool {
        let key = format!("#{}", name);
        match v {
            Value::Instance(i) => {
                if i.fields.borrow().contains_key(&field_key(&key, self.priv_id)) {
                    return true;
                }
                // private 메서드/접근자는 클래스에 산다
                i.class.find_method(&key).is_some()
                    || i.class.find_getter(&key).is_some()
                    || i.class.find_setter(&key).is_some()
            }
            // static private 은 클래스 자신에 산다
            Value::Class(c) => c.statics.borrow().contains_key(&key),
            _ => false,
        }
    }

    // 종류가 지정되지 않은 내부 오류를 잡을 때 쓰는 Error 객체.
    pub(super) fn error_from_msg(&self, msg: &str) -> Value {
        // "TypeError: ..." / "RangeError: ..." 처럼 알려진 에러 타입 접두가 있으면
        // 그 타입으로 만든다. 예전엔 무조건 일반 Error 라, throw_error 를 안 거치고
        // Err("RangeError: ...") 로 반환한 곳들이 전부 catch 에서 Error 로 잡혀
        // "Expected RangeError but got Error" 로 깨졌다.
        const KINDS: &[&str] = &[
            "TypeError",
            "RangeError",
            "SyntaxError",
            "ReferenceError",
            "EvalError",
            "URIError",
        ];
        if let Some((prefix, rest)) = msg.split_once(": ") {
            if KINDS.contains(&prefix) {
                // 접두가 정확히 알려진 종류일 때만 (임의 문자열 오탐 방지)
                let kind = KINDS.iter().find(|k| **k == prefix).unwrap();
                return self.make_error(kind, Some(rest.to_string()));
            }
        }
        self.make_error("Error", Some(msg.to_string()))
    }

    // DOM 이 던지는 표준 오류 (WebIDL DOMException). 이름이 동작을 결정한다:
    // NotFoundError / InvalidCharacterError / HierarchyRequestError 등.
    pub(super) fn throw_dom(&mut self, name: &'static str, message: impl Into<String>) -> String {
        let msg = message.into();
        if let Some(ctor) = env_get(&self.global, "DOMException") {
            if let Ok(v) = self.construct(
                ctor,
                vec![Value::Str(msg.clone()), Value::Str(name.to_string())],
            ) {
                self.thrown = Some(v);
                return format!("{}: {}", name, msg);
            }
        }
        self.throw_error("Error", msg)
    }

    // 표준이 명시한 종류의 오류를 던진다. 내부 오류를 그냥 Err(String) 으로 올리면
    // catch 가 문자열을 잡게 되어 `e instanceof TypeError` 도 `e.message` 도 거짓이 된다.
    pub(super) fn throw_error(&mut self, kind: &'static str, message: impl Into<String>) -> String {
        let msg = message.into();
        self.thrown = Some(self.make_error(kind, Some(msg.clone())));
        format!("{}: {}", kind, msg)
    }

    fn constructor_of(&self, v: &Value) -> Value {
        let name = match v {
            Value::Arr(_) => "Array",
            Value::Str(_) => "String",
            Value::Num(_) => "Number",
            Value::Bool(_) => "Boolean",
            Value::BigInt(_) => "BigInt",
            Value::Symbol(_) => "Symbol",
            Value::Fn(_) | Value::Native(_) | Value::Bound(_) => "Function",
            Value::MapVal(_) => "Map",
            Value::SetVal(_) => "Set",
            Value::Class(_) => "Function",
            Value::Obj(_) => "Object",
            _ => return Value::Undefined,
        };
        env_get(&self.global, name).unwrap_or(Value::Undefined)
    }

    // Object(x) 호출(또는 new Object(x)) 강제변환. f 가 전역 Object 네임스페이스면 Some.
    // null/undefined → 새 빈 객체, 이미 객체/원시값이면 그대로(근사). 아니면 None.
    // Array(…)/new Array(…) 와 Object(x)/new Object(x) — 전역이 Native 생성자이므로
    // 호출·new 가 여기로 온다(표준: typeof 도 "function").
    fn coerce_object_call(&self, f: &Value, args: &[Value]) -> Option<Value> {
        match f {
            Value::Native(Native::ArrayCtor) => Some(match args {
                // new Array(n) → 길이 n 의 빈 배열, Array(a,b,…) → 항목 배열
                [Value::Num(n)] if n.fract() == 0.0 && *n >= 0.0 && *n < 4294967296.0 => {
                    let len = *n as usize;
                    // 상한을 넘으면 밀집 확보를 거부하고 length 만 기억한다 (근사 희박).
                    if len > MAX_DENSE_ARRAY {
                        let a = ArrayObj::new(Vec::new());
                        a.set_prop("\u{0}sparse_len".to_string(), Value::Num(*n));
                        Value::Arr(a)
                    } else {
                        // new Array(n) 은 n 개의 구멍(§23.1.1.1) — 채워진 undefined 아님.
                        let holes: std::collections::HashSet<usize> = (0..len).collect();
                        Value::Arr(ArrayObj::with_holes(vec![Value::Undefined; len], holes))
                    }
                }
                items => Value::Arr(ArrayObj::new(items.to_vec())),
            }),
            Value::Native(Native::ObjectCtor) => {
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                Some(match arg {
                    Value::Null | Value::Undefined => {
                        Value::Obj(Rc::new(RefCell::new(ObjMap::new())))
                    }
                    // ToObject(§7.1.18): 이미 객체면 그대로, 원시값(문자열/숫자/불리언)은
                    // 래퍼 객체로 박싱한다. 예전엔 원시값을 그대로 돌려줬다(typeof Object(5)="number").
                    other if is_object(&other) => other,
                    other => self.to_object_value(other),
                })
            }
            _ => None,
        }
    }

    // 이 객체가 전역 객체(window === globalThis)인가.
    // 전역 객체의 프로퍼티는 전역 환경의 바인딩과 같은 것이어야 한다 (§9.3 Global Environment
    // Record). 예전엔 window.Math 는 되는데 'Math' in window 는 false 였다 — 게터와 in 이
    // 서로 다른 진실을 말했다. 그래서 testharness.js 가 'document' in globalThis 로
    // 환경을 판별하다 실패해 우리를 셸 환경으로 오인했다.
    pub(super) fn is_global_obj(&self, m: &Rc<RefCell<ObjMap>>) -> bool {
        matches!(env_get(&self.global, "window"), Some(Value::Obj(w)) if Rc::ptr_eq(&w, m))
    }

    // 전역 객체가 이 이름을 프로퍼티로 갖는가 (own 맵 또는 전역 환경 바인딩).
    pub(super) fn global_has(&mut self, m: &Rc<RefCell<ObjMap>>, key: &str) -> bool {
        if !self.is_global_obj(m) || is_internal_key(key) {
            return false;
        }
        env_get(&self.global, key).is_some() || self.named_access(key).is_some()
    }

    // window(전역 객체) 프로퍼티 조회 — 브라우저처럼 window.X 를 맨 X 로 읽게 하는 폴백.
    fn window_prop(&mut self, name: &str) -> Option<Value> {
        if let Some(Value::Obj(m)) = env_get(&self.global, "window") {
            let v = m.borrow().get(name).cloned();
            // window.window 등 자기참조로 인한 무의미 순환 방지: window 자신은 제외
            if name != "window" && v.is_some() {
                return v;
            }
        }
        self.named_access(name)
    }

    // 전역 객체의 이름 붙은 프로퍼티 (HTML §7.3.3 "named access on the Window object").
    // id 를 가진 요소와, name 을 가진 form/img/iframe/embed/object 는 그 이름으로
    // 전역에서 바로 읽힌다. 없으면 <div id=target> 을 쓰는 코드가 ReferenceError 로
    // 죽는다 — 레거시가 아니라 지금도 살아 있는 표준이다.
    fn named_access(&mut self, name: &str) -> Option<Value> {
        if name.is_empty() {
            return None;
        }
        let dom = self.dom_arena().ok()?;
        if let Some(id) = dom.find_by_attr_id(name) {
            return Some(Value::Dom(id));
        }
        // name 속성으로 노출되는 요소들 (표준이 정한 태그 목록만)
        let hit = dom.find(|e| {
            matches!(e.tag_name.as_str(), "form" | "img" | "iframe" | "embed" | "object")
                && e.attributes.get("name").map(|v| v == name).unwrap_or(false)
        });
        hit.map(Value::Dom)
    }

    // promise 를 드레인해 정착 상태를 (true=이행/false=거부, 값/이유)로 반환.
    // thenable 아닌 값은 (true, 값). 펜딩은 (true, undefined) 근사.
    fn promise_settle_state(&mut self, v: &Value) -> (bool, Value) {
        if !is_promise(v) {
            return (true, v.clone());
        }
        self.drain_microtasks();
        if let Value::Obj(o) = v {
            let m = o.borrow();
            let state = match m.get("\u{0}state") {
                Some(Value::Str(s)) => s.clone(),
                _ => "pending".into(),
            };
            let value = m.get("\u{0}value").cloned().unwrap_or(Value::Undefined);
            if state == "rejected" {
                return (false, value);
            }
            return (true, value);
        }
        (true, Value::Undefined)
    }

    pub fn drain_microtasks(&mut self) {
        let mut guard = 0;
        while let Some((handler, value, dep, is_reject)) = self.microtasks.pop_front() {
            guard += 1;
            if guard > 100_000 {
                break; // 폭주 방지
            }
            if is_callable(&handler) {
                // 핸들러 실행: 정상 반환 → dep 이행, throw → dep 거부(체인 전파).
                match self.call_value(handler, None, vec![value]) {
                    Ok(r) => self.resolve_promise(&dep, r),
                    Err(e) if e.starts_with(STEP_LIMIT_MSG) => return, // 스텝 한도는 삼키지 않음
                    Err(_) => {
                        let reason = self.thrown.take().unwrap_or(Value::Undefined);
                        self.reject_promise(&dep, reason);
                    }
                }
            } else if is_reject {
                // onRejected 없음 → 거부를 그대로 전파(.then(f) 뒤 .catch 로 흐름)
                self.reject_promise(&dep, value);
            } else {
                // onFulfilled 없음 → 값을 그대로 전파(.catch(r) 뒤 .then 으로 흐름)
                self.resolve_promise(&dep, value);
            }
        }
    }

    // location 전역 설치 (페이지 URL 기반). window.location 에도 공유.
    // 문서의 기준 URL 설정 (<base href>). location 은 바뀌지 않는다.
    pub fn set_base_url(&mut self, base: &str) {
        self.base_url = Some(base.to_string());
    }

    pub fn install_location(&mut self, url: &str) {
        self.base_url = Some(url.to_string());
        let Ok(u) = crate::url::Url::parse(url) else { return };
        // path 는 쿼리(?...)를 포함하므로 pathname/search 로 분리. hash 는 원문에서.
        let (pathname, search) = match u.path.split_once('?') {
            Some((p, q)) => (p.to_string(), format!("?{}", q)),
            None => (u.path.clone(), String::new()),
        };
        let hash = match url.split_once('#') {
            Some((_, f)) => format!("#{}", f),
            None => String::new(),
        };
        let mut loc = ObjMap::new();
        // href 는 읽으면 현재 URL, **쓰면 내비게이션**이다 (HTML §7.10.5).
        // 예전엔 그냥 문자열이라 location.href = "..." 가 아무 일도 안 했다 —
        // 봇 차단·로그인 리다이렉트가 전부 무시됐다.
        loc.insert("\u{0}href".to_string(), Value::Str(url.to_string()));
        loc.insert(
            "href".to_string(),
            Value::Accessor(Rc::new(AccessorPair {
                get: Some(Value::Native(Native::LocationHref)),
                set: Some(Value::Native(Native::LocationHrefSet)),
            })),
        );
        loc.insert("assign".to_string(), Value::Native(Native::LocationAssign));
        loc.insert("replace".to_string(), Value::Native(Native::LocationAssign));
        loc.insert("reload".to_string(), Value::Native(Native::LocationReload));
        // Location 의 stringifier 는 href 다 (HTML 표준). String(location) / new URL(location).
        loc.insert("toString".to_string(), Value::Native(Native::LocationHref));
        loc.insert("protocol".to_string(), Value::Str(format!("{}:", u.scheme)));
        loc.insert("host".to_string(), Value::Str(u.host.clone()));
        loc.insert("hostname".to_string(), Value::Str(u.host.clone()));
        loc.insert("origin".to_string(), Value::Str(format!("{}://{}", u.scheme, u.host)));
        loc.insert("pathname".to_string(), Value::Str(pathname));
        loc.insert("search".to_string(), Value::Str(search));
        loc.insert("hash".to_string(), Value::Str(hash));
        let loc = Value::Obj(Rc::new(RefCell::new(loc)));
        env_declare(&self.global, "location", loc.clone());
        if let Some(Value::Obj(w)) = env_get(&self.global, "window") {
            w.borrow_mut().insert("location".to_string(), loc);
        }
    }

    // new URL(url, base?) — WHATWG URL. 핵심 프로퍼티 + searchParams(get/has/getAll/toString).
    fn make_url(&mut self, args: Vec<Value>) -> Result<Value, String> {
        // 인자는 ToString 을 거친다 (표준). Location/URL 은 stringifier 가 href 라
        // new URL(location) 이 정상 동작해야 한다 — 예전엔 "[object Object]" 를 파싱하려
        // 했다. to_display 는 toString 을 부르지 않으므로 ToPrimitive(string) 를 쓴다.
        let to_str = |me: &mut Self, v: &Value| -> String {
            let p = me.to_primitive(v.clone(), true);
            to_display(&p)
        };
        let first = args.first().cloned().unwrap_or(Value::Undefined);
        let input = to_str(self, &first);
        let resolved = match args.get(1).cloned() {
            Some(b) if !matches!(b, Value::Undefined | Value::Null) => {
                let base = to_str(self, &b);
                crate::url::Url::parse(&base)
                    .ok()
                    .and_then(|bu| bu.join(&input))
                    .map(|u| u.as_string())
                    .unwrap_or_else(|| input.clone())
            }
            _ => input.clone(),
        };
        let u = crate::url::Url::parse(&resolved).map_err(|_| format!("Invalid URL: {}", input))?;
        // path 에 쿼리·프래그먼트가 붙어올 수 있으니 분리 (search 는 # 앞까지).
        let path_no_hash = u.path.split('#').next().unwrap_or(&u.path);
        let (pathname, search) = match path_no_hash.split_once('?') {
            Some((p, q)) => (p.to_string(), format!("?{}", q)),
            None => (path_no_hash.to_string(), String::new()),
        };
        let hash = match resolved.split_once('#') {
            Some((_, f)) => format!("#{}", f),
            None => String::new(),
        };
        let default_port = match u.scheme.as_str() {
            "http" | "ws" => 80,
            "https" | "wss" => 443,
            _ => 0,
        };
        let port = if u.port != 0 && u.port != default_port {
            u.port.to_string()
        } else {
            String::new()
        };
        let host = if port.is_empty() { u.host.clone() } else { format!("{}:{}", u.host, port) };
        // searchParams: 쿼리 문자열을 담고 네이티브 메서드로 조회
        let mut sp = ObjMap::new();
        sp.insert("\u{0}query".to_string(), Value::Str(search.trim_start_matches('?').to_string()));
        sp.insert("get".to_string(), Value::Native(Native::UrlSearchGet));
        sp.insert("getAll".to_string(), Value::Native(Native::UrlSearchGetAll));
        sp.insert("has".to_string(), Value::Native(Native::UrlSearchHas));
        sp.insert("set".to_string(), Value::Native(Native::UrlSearchSet));
        sp.insert("append".to_string(), Value::Native(Native::UrlSearchAppend));
        sp.insert("delete".to_string(), Value::Native(Native::UrlSearchDelete));
        sp.insert("toString".to_string(), Value::Native(Native::UrlSearchToString));
        let search_params = Value::Obj(Rc::new(RefCell::new(sp)));

        let mut m = ObjMap::new();
        m.insert("href".to_string(), Value::Str(resolved.clone()));
        m.insert("protocol".to_string(), Value::Str(format!("{}:", u.scheme)));
        m.insert("host".to_string(), Value::Str(host.clone()));
        m.insert("hostname".to_string(), Value::Str(u.host.clone()));
        m.insert("port".to_string(), Value::Str(port));
        m.insert("origin".to_string(), Value::Str(format!("{}://{}", u.scheme, host)));
        m.insert("pathname".to_string(), Value::Str(pathname));
        m.insert("search".to_string(), Value::Str(search));
        m.insert("hash".to_string(), Value::Str(hash));
        m.insert("searchParams".to_string(), search_params);
        m.insert("toString".to_string(), Value::Native(Native::UrlToString));
        Ok(Value::Obj(Rc::new(RefCell::new(m))))
    }

    // append/prepend/before/after 인자를 노드 id 로. Dom 은 그대로, 그 외(문자열 등)는
    // 텍스트 노드로 생성.
    fn nodes_from_args(&mut self, args: &[Value]) -> Result<Vec<crate::dom::NodeId>, String> {
        let dom = self.dom_arena()?;
        let mut ids = Vec::with_capacity(args.len());
        for a in args {
            match a {
                Value::Dom(id) => ids.push(*id),
                other => ids.push(dom.create_text(to_display(other))),
            }
        }
        Ok(ids)
    }

    // 이벤트 객체 생성: type/target + preventDefault/stopPropagation 등.
    // 내부 플래그(__defaultPrevented/__stopProp)를 네이티브가 갱신.
    // 호출/생성 인자 평가 (스프레드 ...arr 전개).
    fn eval_args(&mut self, args: &[crate::js::ast::Expr], env: &EnvRef) -> Result<Vec<Value>, String> {
        let mut out = Vec::new();
        for a in args {
            if let crate::js::ast::Expr::Spread(inner) = a {
                let v = self.eval(inner, env)?;
                out.extend(self.iterate_to_vec(&v)?);
            } else {
                out.push(self.eval(a, env)?);
            }
        }
        Ok(out)
    }

    // 값들의 Vec 을 반복자 객체로 (MakeIter 와 동일 구조: __items/__i/next).
    pub(super) fn make_iter_from_vec(&self, items: Vec<Value>) -> Value {
        let mut it = ObjMap::new();
        it.insert("\u{0}items".to_string(), Value::Arr(ArrayObj::new(items)));
        it.insert("\u{0}i".to_string(), Value::Num(0.0));
        it.insert("next".to_string(), Value::Native(Native::IterNext));
        // 이터레이터는 스스로 이터러블이다 (표준): it[Symbol.iterator]() === it
        it.insert("\u{0}@@iterator".to_string(), Value::Native(Native::ReturnThis));
        set_prop_attrs(&mut it, "\u{0}@@iterator", ATTR_WRITABLE | ATTR_CONFIGURABLE);
        // Iterator Helpers (ES2025: map/filter/find/take/drop/toArray…).
        // 프렐류드가 정의한 프로토타입을 달아 준다 — 사이트가 실제로 쓴다
        // (astro.build 가 el.querySelectorAll().values().find(…) 를 쓴다).
        if let Some(proto) = env_get(&self.global, "__kIterProto") {
            it.insert("__proto__".to_string(), proto);
        }
        Value::Obj(Rc::new(RefCell::new(it)))
    }

    // 이터러블(배열/문자열/Set/Map/반복자 객체)을 값 Vec 으로. yield* 와 for-of 공용.
    // 반복자가 던진 오류는 반드시 전파한다. 예전엔 Err(_) => break 로 삼켜서,
    // 검증하다 throw 하는 이터러블이 조용히 빈 결과가 됐다.
    fn iterate_to_vec(&mut self, v: &Value) -> Result<Vec<Value>, String> {
        Ok(match v {
            Value::Arr(a) => a.borrow().clone(),
            Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
            Value::SetVal(s) => s.borrow().clone(),
            Value::MapVal(m) => m
                .borrow()
                .iter()
                .map(|(k, val)| Value::Arr(ArrayObj::new(vec![k.clone(), val.clone()])))
                .collect(),
            // 재료화된 반복자 객체(__items)는 그대로.
            Value::Obj(o) if o.borrow().contains_key("\u{0}items") => {
                match o.borrow().get("\u{0}items") {
                    Some(Value::Arr(items)) => items.borrow().clone(),
                    _ => Vec::new(),
                }
            }
            // 그 외: 반복자 프로토콜(제너레이터/사용자 [Symbol.iterator]/반복자 객체)로
            // done 까지 재료화. 무한이면 step 상한이 방어.
            _ => {
                let it = match self.try_get_iterator(v)? {
                    Some(it) => it,
                    None => return Ok(Vec::new()),
                };
                let mut out = Vec::new();
                loop {
                    let (val, done) = self.gen_iter_next(&it, Value::Undefined)?;
                    if done {
                        break;
                    }
                    out.push(val);
                    self.tick()?;
                }
                out
            }
        })
    }

    pub(super) fn make_event(&self, event: &str, target: crate::dom::NodeId) -> Value {
        let mut m = ObjMap::new();
        if let Some(p) = self.event_proto("Event") {
            m.insert("__proto__".to_string(), p);
        }
        m.insert("type".to_string(), Value::Str(event.to_string()));
        m.insert("target".to_string(), Value::Dom(target));
        m.insert("currentTarget".to_string(), Value::Dom(target));
        m.insert("srcElement".to_string(), Value::Dom(target));
        m.insert("bubbles".to_string(), Value::Bool(true));
        m.insert("cancelable".to_string(), Value::Bool(true));
        m.insert("defaultPrevented".to_string(), Value::Bool(false));
        m.insert("isTrusted".to_string(), Value::Bool(true));
        m.insert("\u{0}stopProp".to_string(), Value::Bool(false));
        m.insert("timeStamp".to_string(), Value::Num(0.0));
        // 0=NONE, 1=CAPTURING, 2=AT_TARGET, 3=BUBBLING (디스패치가 단계마다 갱신한다)
        m.insert("eventPhase".to_string(), Value::Num(0.0));
        m.insert("preventDefault".to_string(), Value::Native(Native::EventPreventDefault));
        m.insert("stopPropagation".to_string(), Value::Native(Native::EventStopProp));
        m.insert("stopImmediatePropagation".to_string(), Value::Native(Native::EventStopProp));
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // 이벤트 디스패치: 타깃 → 조상 순(버블링). 이벤트 객체를 인자로 전달,
    // this 는 currentTarget. stopPropagation 시 상위 전파 중단.
    // 반환: 핸들러가 하나라도 실행됐는지(호출측 리플로우 판단용).
    pub fn fire_handlers(&mut self, target: crate::dom::NodeId, event: &str) -> bool {
        self.begin_unit();
        let evt = self.make_event(event, target);
        self.dispatch_event_value(target, event, evt)
    }

    // 주어진 이벤트 객체로 target 에서 버블링하며 핸들러 실행. fire_handlers 와
    // dispatchEvent 가 공유. 하나라도 실행됐으면 true.
    // on<type> 콘텐츠 속성이 있으면 그 소스를 컴파일해 리스너로 등록한다 (HTML §8.1.5.2).
    // 이미 등록돼 있으면(속성 내용이 그대로면) 다시 만들지 않는다.
    fn ensure_inline_handler(&mut self, id: crate::dom::NodeId, event: &str) {
        let attr = format!("on{}", event);
        let src = {
            let Ok(dom) = self.dom_arena() else { return };
            match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => e.attributes.get(&attr).cloned(),
                _ => None,
            }
        };
        let Some(src) = src else { return };
        // 같은 소스로 이미 등록했으면 건너뛴다
        if self.inline_handlers.get(&(id, attr.clone())).map(|s| s == &src).unwrap_or(false) {
            return;
        }
        // 핸들러 본문은 함수 본문이다. this = 요소, event/arguments[0] = 이벤트.
        // 반환값이 false 면 preventDefault (레거시지만 표준이다).
        let body = format!(
            "(function(event){{ var __r = (function(){{ {} }}).call(this, event);              if (__r === false && event && event.preventDefault) event.preventDefault(); }})",
            src
        );
        let f = match self.run(&body) {
            Ok(v) if is_callable(&v) => v,
            _ => return,
        };
        // 예전 소스로 등록된 리스너가 있으면 뺀다 (속성이 바뀐 경우)
        if let Some(old) = self.inline_handlers.get(&(id, attr.clone())).cloned() {
            let _ = old;
            self.handlers.retain(|(hid, t, _, cap, _)| {
                !(*hid == id && t == event && !*cap && self.inline_fns.contains_key(&(id, t.clone())))
            });
        }
        self.inline_fns.insert((id, event.to_string()), f.clone());
        self.inline_handlers.insert((id, attr), src);
        self.handlers.push((id, event.to_string(), f, false, false));
    }

    pub fn dispatch_event_value(
        &mut self,
        target: crate::dom::NodeId,
        event: &str,
        evt: Value,
    ) -> bool {
        // DOM 이벤트는 3단계다 (DOM 표준 §2.9): 캡처(루트→타깃 부모) → 타깃 → 버블(부모→루트).
        // 예전엔 캡처 플래그를 버리고 타깃부터 위로 한 번만 돌아서, 캡처 리스너가
        // 타깃보다 **늦게** 불렸다 (이벤트 위임 라이브러리가 조용히 어긋난다).
        let ancestors: Vec<crate::dom::NodeId> = match self.dom {
            Some(p) => unsafe { (*p).ancestors(target) },
            None => Vec::new(),
        };
        let evt_obj = if let Value::Obj(o) = &evt { o.clone() } else { return false };
        evt_obj.borrow_mut().insert("target".to_string(), Value::Dom(target));

        // (노드, 캡처단계인가, eventPhase) 순서:
        // 조상 역순(루트→부모) 캡처(1) → 타깃(2, 둘 다) → 부모→루트 버블(3)
        let mut phases: Vec<(crate::dom::NodeId, Option<bool>, u8)> = Vec::new();
        for &a in ancestors.iter().rev() {
            phases.push((a, Some(true), 1)); // 캡처 리스너만
        }
        phases.push((target, None, 2)); // 타깃: 등록 순서대로 전부
        for &a in ancestors.iter() {
            phases.push((a, Some(false), 3)); // 버블 리스너만
        }

        let mut fired = false;
        for (id, want_capture, phase) in phases {
            // 버블 단계는 이벤트가 bubbles=true 일 때만 (표준). focus/blur 등은 안 올라간다.
            if phase == 3
                && !matches!(evt_obj.borrow().get("bubbles"), Some(Value::Bool(true)) | None)
            {
                break;
            }
            // 이벤트 핸들러 **콘텐츠 속성** (HTML §8.1.5.2): onclick="..." 등.
            // 예전엔 통째로 무시했다 — 인라인 핸들러가 영영 안 돌았다.
            // 버블 단계 리스너로 취급한다 (표준). 처음 마주칠 때 컴파일해 등록한다.
            if want_capture != Some(true) {
                self.ensure_inline_handler(id, event);
            }
            let to_run: Vec<Value> = self
                .handlers
                .iter()
                .filter(|(hid, t, _, cap, _)| {
                    *hid == id && t == event && want_capture.map_or(true, |w| *cap == w)
                })
                .map(|(_, _, f, _, _)| f.clone())
                .collect();
            if !to_run.is_empty() {
                fired = true;
                evt_obj.borrow_mut().insert("currentTarget".to_string(), Value::Dom(id));
                evt_obj.borrow_mut().insert("eventPhase".to_string(), Value::Num(phase as f64));
            }
            for f in to_run {
                // once 리스너는 **부르기 전에** 목록에서 뺀다 (핸들러가 재진입해도 두 번 안 불린다)
                let once = self.handlers.iter().any(|(hid, t, hf, _, o)| {
                    *o && *hid == id && t == event && strict_eq(hf, &f)
                });
                if once {
                    self.handlers.retain(|(hid, t, hf, _, o)| {
                        !(*o && *hid == id && t == event && strict_eq(hf, &f))
                    });
                }
                if let Err(e) = self.call_value(f, Some(Value::Dom(id)), vec![evt.clone()]) {
                    println!("[js error] {}", e);
                }
            }
            if matches!(evt_obj.borrow().get("\u{0}stopProp"), Some(Value::Bool(true))) {
                break; // stopPropagation
            }
        }
        evt_obj.borrow_mut().insert("eventPhase".to_string(), Value::Num(0.0)); // NONE (디스패치 끝)
        fired
    }

    // Function(p1, p2, ..., body) 를 실제 함수로 컴파일. 마지막 인자가 본문,
    // 앞 인자들은 파라미터 이름(각각 콤마로 여러 개 가능). new/호출 공용.
    fn make_function(&mut self, args: Vec<Value>) -> Result<Value, String> {
        // §20.2.1.1 CreateDynamicFunction: 각 인자 ToString(valueOf/toString 호출, Symbol
        // TypeError). 마지막이 본문, 나머지가 파라미터 목록(쉼표 구분).
        let (body_src, param_args) = match args.split_last() {
            Some((last, rest)) => (self.to_string_value(last)?, rest.to_vec()),
            None => (String::new(), Vec::new()),
        };
        let mut params = Vec::new();
        for p in &param_args {
            let s = self.to_string_value(p)?;
            for name in s.split(',') {
                let name = name.trim();
                if !name.is_empty() {
                    params.push(name.to_string());
                }
            }
        }
        // 본문 파싱 실패는 SyntaxError (§20.2.1.1.1) — 예전엔 일반 Error 라
        // "e instanceof SyntaxError" 검사가 깨졌다.
        let body = match parse(&body_src) {
            Ok(b) => b,
            Err(e) => {
                return Err(self.throw_error("SyntaxError", format!("Function body: {}", e)))
            }
        };
        Ok(Value::Fn(Rc::new(JsFn {
            priv_id: std::cell::Cell::new(0),
            name: RefCell::new("anonymous".to_string()),
            params,
            body,
            param_prologue_len: 0,
            env: self.global.clone(),
            is_arrow: false,
            is_generator: false,
            is_async: false,
            is_method: false,
            this: None,
            super_class: None,
            props: RefCell::new(ObjMap::new()),
            source: None,
        })))
    }

    // new Map(iterable) — §24.1.1.1. iterable 이 있으면 AddEntriesFromIterable 로
    // 이터레이터 프로토콜을 따라 채운다(사용자가 오버라이드한 set 을 관측).
    fn make_map(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let target = Value::MapVal(Rc::new(RefCell::new(Vec::new())));
        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        // step 4: iterable 이 undefined/null 이면 빈 Map (adder 검사 전에 조기 반환).
        if matches!(iterable, Value::Undefined | Value::Null) {
            return Ok(target);
        }
        // step 5-6: adder = ? Get(target, "set"); IsCallable 아니면 TypeError.
        let adder = self.member_get(&target, "set")?;
        if !is_callable(&adder) {
            return Err(self.throw_error("TypeError", "Map: 'set' is not a function"));
        }
        self.add_entries_from_iterable(&target, &iterable, &adder)?;
        Ok(target)
    }

    // new Set(iterable) — §24.2.1.1. adder = ? Get(target, "add"); 각 값을
    // Call(adder, target, «value») (Set 항목엔 객체 제약 없음). 비정상 완료 시 IteratorClose.
    fn make_set(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let target = Value::SetVal(Rc::new(RefCell::new(Vec::new())));
        let iterable = args.first().cloned().unwrap_or(Value::Undefined);
        if matches!(iterable, Value::Undefined | Value::Null) {
            return Ok(target);
        }
        let adder = self.member_get(&target, "add")?;
        if !is_callable(&adder) {
            return Err(self.throw_error("TypeError", "Set: 'add' is not a function"));
        }
        let it = match self.try_get_iterator(&iterable)? {
            Some(it) => it,
            None => return Err(self.throw_error("TypeError", "값이 이터러블이 아닙니다")),
        };
        loop {
            let (item, done) = self.gen_iter_next(&it, Value::Undefined)?;
            if done {
                break;
            }
            if let Err(e) = self.call_value(adder.clone(), Some(target.clone()), vec![item]) {
                return Err(self.iterator_close_throw(&it, e));
            }
            self.tick()?;
        }
        Ok(target)
    }

    // §7.4.11 IteratorClose (throw 완료 전용): iterator.return() 을 호출하되 그 결과와
    // 예외는 무시하고 원래 throw 완료(err)를 그대로 돌려준다. throw 완료가 return 실패보다
    // 우선한다(§7.4.11 step 5).
    fn iterator_close_throw(&mut self, it: &Value, err: String) -> String {
        if let Ok(ret) = self.member_get(it, "return") {
            if is_callable(&ret) {
                let _ = self.call_value(ret, Some(it.clone()), vec![]);
            }
        }
        err
    }

    // §24.1.1.2 AddEntriesFromIterable(target, iterable, adder). 이터레이터 프로토콜로
    // 각 항목을 순회하며 [0]·[1] 을 뽑아 adder(target, k, v) 로 채운다. 각 항목은 객체여야
    // 하고, 어느 단계든 비정상 완료면 IteratorClose 후 그 완료를 전파한다.
    fn add_entries_from_iterable(
        &mut self,
        target: &Value,
        iterable: &Value,
        adder: &Value,
    ) -> Result<(), String> {
        let it = match self.try_get_iterator(iterable)? {
            Some(it) => it,
            None => return Err(self.throw_error("TypeError", "값이 이터러블이 아닙니다")),
        };
        loop {
            // IteratorStep 의 비정상 완료는 close 없이 그대로 전파(§ step: `? IteratorStep`).
            let (item, done) = self.gen_iter_next(&it, Value::Undefined)?;
            if done {
                break;
            }
            // 각 항목은 객체여야 한다. 아니면 TypeError 로 IteratorClose.
            if !is_object(&item) {
                let e = self.throw_error("TypeError", "이터레이터 항목이 객체가 아닙니다");
                return Err(self.iterator_close_throw(&it, e));
            }
            let k = match self.member_get(&item, "0") {
                Ok(k) => k,
                Err(e) => return Err(self.iterator_close_throw(&it, e)),
            };
            let v = match self.member_get(&item, "1") {
                Ok(v) => v,
                Err(e) => return Err(self.iterator_close_throw(&it, e)),
            };
            if let Err(e) = self.call_value(adder.clone(), Some(target.clone()), vec![k, v]) {
                return Err(self.iterator_close_throw(&it, e));
            }
            self.tick()?;
        }
        Ok(())
    }

    fn map_method(
        &mut self,
        m: Rc<RefCell<Vec<(Value, Value)>>>,
        op: MapOp,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let key = args.first().cloned().unwrap_or(Value::Undefined);
        Ok(match op {
            MapOp::Get => m
                .borrow()
                .iter()
                .find(|(k, _)| same_value_zero(k, &key))
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Undefined),
            MapOp::Has => Value::Bool(m.borrow().iter().any(|(k, _)| same_value_zero(k, &key))),
            MapOp::Set => {
                let val = args.get(1).cloned().unwrap_or(Value::Undefined);
                let pos = m.borrow().iter().position(|(k, _)| same_value_zero(k, &key));
                match pos {
                    Some(i) => m.borrow_mut()[i].1 = val,
                    None => m.borrow_mut().push((key, val)),
                }
                Value::MapVal(m) // set 은 map 반환 (체이닝)
            }
            MapOp::Delete => {
                let before = m.borrow().len();
                m.borrow_mut().retain(|(k, _)| !same_value_zero(k, &key));
                Value::Bool(m.borrow().len() < before)
            }
            MapOp::Clear => {
                m.borrow_mut().clear();
                Value::Undefined
            }
            MapOp::ForEach => {
                // §24.1.3.5: callable 검사 + 콜백 (값,키,map) + thisArg.
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_callable(&f) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Map.prototype.forEach callback is not a function",
                    ));
                }
                let this_arg = args.get(1).cloned();
                let map_val = Value::MapVal(m.clone());
                let snapshot: Vec<(Value, Value)> = m.borrow().clone();
                for (k, v) in snapshot {
                    self.call_value(f.clone(), this_arg.clone(), vec![v, k, map_val.clone()])?;
                }
                Value::Undefined
            }
            // keys/values/entries 는 **이터레이터**를 돌려준다 (배열이 아니다 — 표준).
            // 배열을 주면 for-of 는 되지만 .next() 가 없어서, 이터레이터 프로토콜을
            // 직접 쓰는 코드(core-js/date-fns/regenerator)가 "next 가 undefined" 로 죽는다.
            MapOp::Keys => {
                self.make_iter_from_vec(m.borrow().iter().map(|(k, _)| k.clone()).collect())
            }
            MapOp::Values => {
                self.make_iter_from_vec(m.borrow().iter().map(|(_, v)| v.clone()).collect())
            }
            MapOp::Entries => self.make_iter_from_vec(
                m.borrow()
                    .iter()
                    .map(|(k, v)| Value::Arr(ArrayObj::new(vec![k.clone(), v.clone()])))
                    .collect(),
            ),
        })
    }

    fn set_method(&mut self, s: Rc<RefCell<Vec<Value>>>, op: SetOp, args: Vec<Value>) -> Result<Value, String> {
        // ES2024 집합 연산(union/…)은 set-like 인자 검증(GetSetRecord)과 이터레이션이
        // 필요해 별도 경로로 뺀다.
        if matches!(
            op,
            SetOp::Union
                | SetOp::Intersection
                | SetOp::Difference
                | SetOp::SymmetricDifference
                | SetOp::IsSubsetOf
                | SetOp::IsSupersetOf
                | SetOp::IsDisjointFrom
        ) {
            return self.set_binary_op(s, op, args.first().cloned().unwrap_or(Value::Undefined));
        }
        let val = args.first().cloned().unwrap_or(Value::Undefined);
        Ok(match op {
            SetOp::Add => {
                if !s.borrow().iter().any(|e| same_value_zero(e, &val)) {
                    s.borrow_mut().push(val);
                }
                Value::SetVal(s)
            }
            SetOp::Has => Value::Bool(s.borrow().iter().any(|e| same_value_zero(e, &val))),
            SetOp::Delete => {
                let before = s.borrow().len();
                s.borrow_mut().retain(|e| !same_value_zero(e, &val));
                Value::Bool(s.borrow().len() < before)
            }
            SetOp::Clear => {
                s.borrow_mut().clear();
                Value::Undefined
            }
            SetOp::ForEach => {
                // §24.2.3.6: 콜백이 callable 아니면 TypeError. 콜백은 (값,값,set)+thisArg.
                // 콜백 예외는 전파한다(예전엔 삼켰다).
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                if !is_callable(&f) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Set.prototype.forEach callback is not a function",
                    ));
                }
                let this_arg = args.get(1).cloned();
                let set_val = Value::SetVal(s.clone());
                let snapshot: Vec<Value> = s.borrow().clone();
                for v in snapshot {
                    self.call_value(
                        f.clone(),
                        this_arg.clone(),
                        vec![v.clone(), v, set_val.clone()],
                    )?;
                }
                Value::Undefined
            }
            // values/keys 는 이터레이터다 (배열이 아니다 — 표준).
            SetOp::Values => {
                let items = s.borrow().clone();
                self.make_iter_from_vec(items)
            }
            // §24.2.3.5 entries: [value, value] 쌍 이터레이터 (Set 은 키=값).
            SetOp::Entries => {
                let items: Vec<Value> = s
                    .borrow()
                    .iter()
                    .map(|v| Value::Arr(ArrayObj::new(vec![v.clone(), v.clone()])))
                    .collect();
                self.make_iter_from_vec(items)
            }
            _ => unreachable!("ES2024 집합 연산은 위에서 early-return"),
        })
    }

    // GetSetRecord (§24.2.1.2): 인자가 set-like 인지 검증하고 (size, has, keys) 를 뽑는다.
    // 객체가 아니면 TypeError, size 가 NaN 이면 TypeError, 음수면 RangeError,
    // has/keys 가 호출 불가면 TypeError.
    fn get_set_record(&mut self, obj: &Value) -> Result<(f64, Value, Value), String> {
        if !is_object(obj) {
            return Err(self.throw_error("TypeError", "Set operation argument is not an object"));
        }
        let raw_size = self.member_get(obj, "size")?;
        // step 3: numSize = ? ToNumber(rawSize) — 사용자 valueOf/@@toPrimitive 호출,
        // Symbol/BigInt 은 TypeError. 예전엔 to_num 이라 valueOf 미호출·BigInt 통과였다.
        let num = self.to_number_value(&raw_size)?;
        // step 4: NaN 이면 TypeError (ToIntegerOrInfinity 전에 검사 — NaN→0 로 뭉개지 않는다).
        if num.is_nan() {
            return Err(self.throw_error("TypeError", "Set-like 'size' is not a number"));
        }
        // step 5: intSize = ToIntegerOrInfinity(numSize).
        let int_size = if num.is_infinite() { num } else { num.trunc() };
        if int_size < 0.0 {
            return Err(self.throw_error("RangeError", "Set-like 'size' is negative"));
        }
        let has = self.member_get(obj, "has")?;
        if !is_callable(&has) {
            return Err(self.throw_error("TypeError", "Set-like 'has' is not callable"));
        }
        let keys = self.member_get(obj, "keys")?;
        if !is_callable(&keys) {
            return Err(self.throw_error("TypeError", "Set-like 'keys' is not callable"));
        }
        Ok((int_size, has, keys))
    }

    // otherRec.[[Keys]] 를 호출해 얻은 이터레이터를 원소 Vec 으로 드레인한다.
    fn set_keys_vec(&mut self, obj: &Value, keys_fn: &Value) -> Result<Vec<Value>, String> {
        let iter = self.call_value(keys_fn.clone(), Some(obj.clone()), vec![])?;
        self.iterate_to_vec(&iter)
    }

    // §10.1.10 [[Delete]]: configurable:false 인 own 프로퍼티는 삭제되지 않고 false.
    // 삭제 성공/부재는 true. delete 연산자와 Reflect.deleteProperty 가 공유하는 의미론.
    // 내부 프로퍼티 키 문자열을 트랩에 넘길 Value 로 — 심볼 키("\0@@…")는 실제
    // Symbol 값으로 되돌린다. 예전엔 트랩이 모든 키를 Value::Str 로 받아 `case sym:`
    // 같은 심볼 비교가 어긋났다(get/set/has/delete/define/gOPD 트랩 공통).
    pub(super) fn trap_key(&self, key: &str) -> Value {
        if is_symbol_key(key) {
            builtins::symbol_from_key(key)
        } else {
            Value::Str(key.to_string())
        }
    }

    // §10.5.10 [[Delete]](P) — Proxy 의 deleteProperty 트랩. 트랩 없으면 타깃에 위임,
    // 트랩이 falsy 면 false, 성공을 보고했으나 타깃의 non-configurable/non-extensible
    // 프로퍼티면 TypeError. delete 연산자와 Reflect.deleteProperty(delete_own)가 공유.
    fn proxy_delete(&mut self, p: &Rc<(Value, Value)>, key: &str) -> Result<bool, String> {
        self.proxy_revoked_guard(p)?;
        let (t, h) = (p.0.clone(), p.1.clone());
        let trap = self.member_get(&h, "deleteProperty")?;
        if matches!(trap, Value::Undefined | Value::Null) {
            return self.delete_own(&t, key);
        }
        if !is_callable(&trap) {
            return Err(self.throw_error("TypeError", "'deleteProperty' trap is not callable"));
        }
        let res = self.call_value(trap, Some(h), vec![t.clone(), self.trap_key(key)])?;
        if !to_bool(&res) {
            return Ok(false);
        }
        let td = self.call_native(
            Native::ObjectGetOwnPropertyDescriptor,
            None,
            vec![t.clone(), Value::Str(key.to_string())],
        )?;
        if let Value::Obj(d) = &td {
            let configurable = matches!(d.borrow().get("configurable"), Some(v) if to_bool(v));
            if !configurable {
                return Err(self.throw_error("TypeError", "'deleteProperty' on proxy: a non-configurable property cannot be reported as deleted"));
            }
            if !self.value_is_extensible(&t)? {
                return Err(self.throw_error("TypeError", "'deleteProperty' on proxy: a property of a non-extensible target cannot be reported as deleted"));
            }
        }
        Ok(true)
    }

    pub(super) fn delete_own(&mut self, target: &Value, key: &str) -> Result<bool, String> {
        match target {
            Value::Proxy(p) => {
                let p = p.clone();
                self.proxy_delete(&p, key)
            }
            Value::Obj(m) => {
                if m.borrow().contains_key(key) {
                    if prop_attrs(&m.borrow(), key) & ATTR_CONFIGURABLE == 0 {
                        return Ok(false);
                    }
                    let mut mm = m.borrow_mut();
                    mm.remove(key);
                    mm.remove(&attr_marker(key));
                    mm.remove(&nonenum_marker(key));
                }
                Ok(true)
            }
            Value::Instance(inst) => {
                if inst.fields.borrow().contains_key(key) {
                    if prop_attrs(&inst.fields.borrow(), key) & ATTR_CONFIGURABLE == 0 {
                        return Ok(false);
                    }
                    let mut mm = inst.fields.borrow_mut();
                    mm.remove(key);
                    mm.remove(&attr_marker(key));
                    mm.remove(&nonenum_marker(key));
                }
                Ok(true)
            }
            _ => Ok(true),
        }
    }

    // 디스크립터 Obj 에서 (is_accessor, writable, setter) 를 뽑는다. own 없으면 None.
    fn own_desc_fields(
        &mut self,
        obj: &Value,
        key: &str,
    ) -> Result<Option<(bool, bool, Value)>, String> {
        let d = self.call_native(
            Native::ObjectGetOwnPropertyDescriptor,
            None,
            vec![obj.clone(), Value::Str(key.to_string())],
        )?;
        let Value::Obj(m) = &d else {
            return Ok(None);
        };
        let b = m.borrow();
        let is_accessor = b.contains_key("get") || b.contains_key("set");
        let writable = matches!(b.get("writable"), Some(Value::Bool(true)));
        let setter = b.get("set").cloned().unwrap_or(Value::Undefined);
        Ok(Some((is_accessor, writable, setter)))
    }

    // §10.1.9 OrdinarySet(O, P, V, Receiver) → 성공 여부. Reflect.set / [[Set]] 의 핵심.
    // ownDesc 를 O 의 프로토타입 체인에서 찾고, 데이터면 Receiver 에 설정(있으면 갱신,
    // 없으면 생성), 접근자면 setter 를 Receiver=this 로 호출. non-writable/비객체 Receiver/
    // accessor-vs-data 불일치는 false.
    pub(super) fn ordinary_set(
        &mut self,
        o: &Value,
        key: &str,
        v: Value,
        receiver: &Value,
    ) -> Result<bool, String> {
        // exotic: Proxy 는 [[Set]] 트랩을 탄다(§10.5.9). Reflect.set 이 이 경로로 온다.
        if let Value::Proxy(p) = o {
            let p = p.clone();
            return self.proxy_set(&p, key, v, receiver);
        }
        // ownDesc = O.[[GetOwnProperty]](P).
        match self.own_desc_fields(o, key)? {
            None => {
                // 프로토타입 체인 위임 (§10.1.9.1 step 2).
                let parent = self.call_native(Native::ObjectGetPrototypeOf, None, vec![o.clone()])?;
                if is_object(&parent) {
                    return self.ordinary_set(&parent, key, v, receiver);
                }
                // parent null → ownDesc = 기본 data(writable). Receiver 에 생성/설정.
                self.set_data_on_receiver(receiver, key, v)
            }
            Some((is_accessor, writable, setter)) => {
                if is_accessor {
                    // §10.1.9.2 step 3: setter 없으면 false, 있으면 Receiver=this 로 호출.
                    if is_callable(&setter) {
                        self.call_value(setter, Some(receiver.clone()), vec![v])?;
                        Ok(true)
                    } else {
                        Ok(false)
                    }
                } else {
                    // 데이터: non-writable 이면 false, 아니면 Receiver 에 설정.
                    if !writable {
                        return Ok(false);
                    }
                    self.set_data_on_receiver(receiver, key, v)
                }
            }
        }
    }

    // §10.5.9 [[Set]](P, V, Receiver) — Proxy 의 set 트랩. 트랩 없으면 타깃에 위임,
    // 트랩 결과가 falsy 면 false, non-configurable non-writable 데이터/setter 없는
    // 접근자에 대한 거짓 성공은 TypeError. Reflect.set 이 이 불리언을 그대로 돌려준다.
    fn proxy_set(
        &mut self,
        p: &Rc<(Value, Value)>,
        key: &str,
        v: Value,
        receiver: &Value,
    ) -> Result<bool, String> {
        self.proxy_revoked_guard(p)?;
        let (t, h) = (p.0.clone(), p.1.clone());
        let trap = self.member_get(&h, "set")?;
        if matches!(trap, Value::Undefined | Value::Null) {
            return self.ordinary_set(&t, key, v, receiver);
        }
        if !is_callable(&trap) {
            return Err(self.throw_error("TypeError", "'set' trap is not callable"));
        }
        let btr = self.call_value(
            trap,
            Some(h),
            vec![t.clone(), self.trap_key(key), v.clone(), receiver.clone()],
        )?;
        if !to_bool(&btr) {
            return Ok(false);
        }
        let td = self.call_native(
            Native::ObjectGetOwnPropertyDescriptor,
            None,
            vec![t.clone(), Value::Str(key.to_string())],
        )?;
        if let Value::Obj(d) = &td {
            let b = d.borrow();
            let configurable = matches!(b.get("configurable"), Some(x) if to_bool(x));
            if !configurable {
                if b.contains_key("value") {
                    let writable = matches!(b.get("writable"), Some(x) if to_bool(x));
                    let val = b.get("value").cloned().unwrap_or(Value::Undefined);
                    if !writable && !same_value(&v, &val) {
                        return Err(self.throw_error("TypeError", "'set' on proxy: cannot change the value of a non-configurable non-writable data property"));
                    }
                } else if matches!(
                    b.get("set").cloned().unwrap_or(Value::Undefined),
                    Value::Undefined
                ) {
                    return Err(self.throw_error(
                        "TypeError",
                        "'set' on proxy: non-configurable accessor property without a setter",
                    ));
                }
            }
        }
        Ok(true)
    }

    // §10.1.9.2 step 2.b-e: 데이터 값을 Receiver 에 설정. Receiver 가 비객체면 false.
    // Receiver 에 own 이 있으면 accessor/non-writable 은 false, 아니면 값 갱신.
    // 없으면 CreateDataProperty. defineProperty 거부(비확장/비설정가능)는 false 로 흡수.
    fn set_data_on_receiver(
        &mut self,
        receiver: &Value,
        key: &str,
        v: Value,
    ) -> Result<bool, String> {
        if !is_object(receiver) {
            return Ok(false);
        }
        let exists = match self.own_desc_fields(receiver, key)? {
            Some((is_accessor, writable, _)) => {
                if is_accessor || !writable {
                    return Ok(false);
                }
                true
            }
            None => false,
        };
        let mut desc = ObjMap::new();
        desc.insert("value".to_string(), v);
        if !exists {
            desc.insert("writable".to_string(), Value::Bool(true));
            desc.insert("enumerable".to_string(), Value::Bool(true));
            desc.insert("configurable".to_string(), Value::Bool(true));
        }
        let desc = Value::Obj(Rc::new(RefCell::new(desc)));
        match self.call_native(
            Native::ObjectDefineProperty,
            None,
            vec![receiver.clone(), Value::Str(key.to_string()), desc],
        ) {
            Ok(_) => Ok(true),
            Err(_) => {
                self.thrown = None; // 정의 거부(비확장 등)는 예외가 아니라 false.
                Ok(false)
            }
        }
    }

    // §10.1.2 OrdinarySetPrototypeOf(O, V). target 은 Obj/Fn(설정 가능), proto 는 Object|Null
    // (호출부가 검증). SameValue(V,current)→true(무변경), non-extensible & 다름→false, 순환→false,
    // 그 밖엔 [[Prototype]] 설정 후 true. Object.setPrototypeOf 는 false 면 TypeError,
    // Reflect.setPrototypeOf 는 그 불리언을 그대로 돌려준다.
    pub(super) fn ordinary_set_prototype_of(
        &mut self,
        target: &Value,
        proto: Value,
    ) -> Result<bool, String> {
        // exotic: Proxy 는 ordinary 가 아니라 [[SetPrototypeOf]] 트랩을 탄다(§10.5.2).
        // 예전엔 여기 step 6 의 match 가 Proxy 를 `_ => {}` 로 무시해 설정이 조용히
        // 버려졌다(트랩도 안 불리고 타깃에도 안 씀).
        if let Value::Proxy(p) = target {
            let p = p.clone();
            return self.proxy_set_prototype_of(&p, proto);
        }
        // step 1-2: 현재 [[Prototype]] 과 SameValue 면 무변경 성공.
        let current = self.call_native(Native::ObjectGetPrototypeOf, None, vec![target.clone()])?;
        if same_value(&current, &proto) {
            return Ok(true);
        }
        // step 3-4: 확장 불가면 실패.
        if self.is_nonextensible_val(target) {
            return Ok(false);
        }
        // step 5: 순환 검사 — proto 의 [[Prototype]] 체인이 target 에 닿으면 실패.
        // exotic(Proxy 등)의 GetPrototypeOf 는 ordinary 가 아니므로 체인 걷기를 멈춘다.
        let mut p = proto.clone();
        let mut depth = 0;
        loop {
            depth += 1;
            if depth > 100_000 {
                break;
            }
            match &p {
                Value::Null => break,
                _ if same_value(&p, target) => return Ok(false),
                Value::Obj(_) | Value::Fn(_) => {
                    p = self.call_native(Native::ObjectGetPrototypeOf, None, vec![p.clone()])?;
                }
                _ => break,
            }
        }
        // step 6: 설정. null 은 명시적으로 Null 저장(부재=기본프로토와 구분).
        match target {
            Value::Obj(m) => {
                m.borrow_mut().insert("__proto__".to_string(), proto);
            }
            Value::Fn(f) => {
                f.props.borrow_mut().insert("__proto__".to_string(), proto);
            }
            _ => {}
        }
        Ok(true)
    }

    // §10.5.2 [[SetPrototypeOf]](V) — Proxy 의 setPrototypeOf 트랩.
    fn proxy_set_prototype_of(
        &mut self,
        p: &Rc<(Value, Value)>,
        proto: Value,
    ) -> Result<bool, String> {
        self.proxy_revoked_guard(p)?;
        let (t, h) = (p.0.clone(), p.1.clone());
        let trap = self.member_get(&h, "setPrototypeOf")?;
        // GetMethod: undefined/null 이면 타깃에 위임, 존재하나 호출 불가면 TypeError.
        if matches!(trap, Value::Undefined | Value::Null) {
            return self.ordinary_set_prototype_of(&t, proto);
        }
        if !is_callable(&trap) {
            return Err(self.throw_error("TypeError", "'setPrototypeOf' trap is not callable"));
        }
        let res = self.call_value(trap, Some(h), vec![t.clone(), proto.clone()])?;
        // 트랩이 falsy 면 실패(false), truthy 면 계속.
        if !to_bool(&res) {
            return Ok(false);
        }
        // non-extensible 타깃이면 V 가 실제 프로토타입과 SameValue 여야 한다
        // (트랩이 거짓 성공을 보고 못 함).
        if self.is_nonextensible_val(&t) {
            let target_proto = self.proto_of(&t)?;
            if !same_value(&proto, &target_proto) {
                return Err(self.throw_error(
                    "TypeError",
                    "'setPrototypeOf' on proxy: cannot change prototype of non-extensible target",
                ));
            }
        }
        Ok(true)
    }

    // [[IsExtensible]] of any value — Proxy 는 트랩을 타므로 call_native 로 위임한다
    // (중첩 프록시도 한 겹씩 벗겨진다). ordinary 는 무결성 NONEXT 비트로 판정.
    fn value_is_extensible(&mut self, v: &Value) -> Result<bool, String> {
        Ok(to_bool(&self.call_native(
            Native::ObjectIsExtensible,
            None,
            vec![v.clone()],
        )?))
    }

    // §10.5.3 [[IsExtensible]] — Proxy 의 isExtensible 트랩.
    fn proxy_is_extensible(&mut self, p: &Rc<(Value, Value)>) -> Result<bool, String> {
        self.proxy_revoked_guard(p)?;
        let (t, h) = (p.0.clone(), p.1.clone());
        let trap = self.member_get(&h, "isExtensible")?;
        if matches!(trap, Value::Undefined | Value::Null) {
            return self.value_is_extensible(&t);
        }
        if !is_callable(&trap) {
            return Err(self.throw_error("TypeError", "'isExtensible' trap is not callable"));
        }
        let res = to_bool(&self.call_value(trap, Some(h), vec![t.clone()])?);
        // invariant: 트랩 결과가 타깃의 실제 extensibility 와 같아야 한다
        // (프록시는 확장성에 대해 거짓말 못 함).
        if res != self.value_is_extensible(&t)? {
            return Err(self.throw_error(
                "TypeError",
                "'isExtensible' on proxy: trap result does not match target",
            ));
        }
        Ok(res)
    }

    // [[PreventExtensions]] of any value — Proxy 는 트랩, ordinary 는 NONEXT 비트 설정.
    // Object.preventExtensions/Reflect.preventExtensions 가 공유한다(전자는 false 면
    // throw, 후자는 그 불리언을 그대로 반환).
    pub(super) fn value_prevent_extensions(&mut self, v: &Value) -> Result<bool, String> {
        if let Value::Proxy(p) = v {
            let p = p.clone();
            return self.proxy_prevent_extensions(&p);
        }
        self.set_integrity(v, INTEG_NONEXT);
        Ok(true)
    }

    // §10.5.4 [[PreventExtensions]] — Proxy 의 preventExtensions 트랩.
    fn proxy_prevent_extensions(&mut self, p: &Rc<(Value, Value)>) -> Result<bool, String> {
        self.proxy_revoked_guard(p)?;
        let (t, h) = (p.0.clone(), p.1.clone());
        let trap = self.member_get(&h, "preventExtensions")?;
        if matches!(trap, Value::Undefined | Value::Null) {
            return self.value_prevent_extensions(&t);
        }
        if !is_callable(&trap) {
            return Err(self.throw_error("TypeError", "'preventExtensions' trap is not callable"));
        }
        let res = to_bool(&self.call_value(trap, Some(h), vec![t.clone()])?);
        // invariant: true 를 보고하려면 타깃이 실제로 non-extensible 이어야 한다.
        if res && self.value_is_extensible(&t)? {
            return Err(self.throw_error(
                "TypeError",
                "'preventExtensions' on proxy: cannot report true while target is extensible",
            ));
        }
        Ok(res)
    }

    // §10.5.11 [[OwnPropertyKeys]] — Proxy 의 ownKeys 트랩. 검증된 키 목록(Value::Str /
    // Value::Symbol)을 돌려준다. 트랩 없으면 타깃의 own 키에 위임. Reflect.ownKeys/
    // Object.getOwnPropertyNames/getOwnPropertySymbols/Object.keys 가 공유한다.
    pub(super) fn proxy_own_keys(&mut self, p: &Rc<(Value, Value)>) -> Result<Vec<Value>, String> {
        self.proxy_revoked_guard(p)?;
        let (t, h) = (p.0.clone(), p.1.clone());
        let trap = self.member_get(&h, "ownKeys")?;
        // GetMethod: undefined/null → 타깃 위임, non-callable → TypeError.
        if matches!(trap, Value::Undefined | Value::Null) {
            let keys = self.call_native(Native::ReflectOwnKeys, None, vec![t])?;
            return Ok(match keys {
                Value::Arr(a) => a.borrow().clone(),
                _ => Vec::new(),
            });
        }
        if !is_callable(&trap) {
            return Err(self.throw_error("TypeError", "'ownKeys' trap is not callable"));
        }
        let trap_res = self.call_value(trap, Some(h), vec![t.clone()])?;
        // CreateListFromArrayLike(«String, Symbol»): 배열형이어야 하고 원소는 문자열/심볼.
        if !is_object(&trap_res) {
            return Err(self.throw_error(
                "TypeError",
                "proxy 'ownKeys' trap must return an array-like object",
            ));
        }
        let list = self.generic_array_read(&trap_res)?;
        for k in &list {
            if !matches!(k, Value::Str(_) | Value::Symbol(_)) {
                return Err(self.throw_error(
                    "TypeError",
                    "proxy 'ownKeys' trap result must contain only strings and symbols",
                ));
            }
        }
        // 중복 키 금지.
        for i in 0..list.len() {
            for j in (i + 1)..list.len() {
                if same_value(&list[i], &list[j]) {
                    return Err(self.throw_error(
                        "TypeError",
                        "proxy 'ownKeys' trap result must not contain duplicate entries",
                    ));
                }
            }
        }
        // §10.5.11 invariant: 타깃 키와 대조.
        let extensible = self.value_is_extensible(&t)?;
        let target_keys = match self.call_native(Native::ReflectOwnKeys, None, vec![t.clone()])? {
            Value::Arr(a) => a.borrow().clone(),
            _ => Vec::new(),
        };
        let mut target_nonconf: Vec<Value> = Vec::new();
        let mut target_conf: Vec<Value> = Vec::new();
        for key in &target_keys {
            let desc = self.call_native(
                Native::ObjectGetOwnPropertyDescriptor,
                None,
                vec![t.clone(), key.clone()],
            )?;
            let is_conf = matches!(&desc, Value::Obj(m)
                if matches!(m.borrow().get("configurable"), Some(v) if to_bool(v)));
            if !matches!(desc, Value::Undefined) && !is_conf {
                target_nonconf.push(key.clone());
            } else {
                target_conf.push(key.clone());
            }
        }
        // 확장 가능하고 non-configurable 키가 없으면 추가 검증 불필요.
        if extensible && target_nonconf.is_empty() {
            return Ok(list);
        }
        let mut unchecked: Vec<Value> = list.clone();
        // non-configurable 키는 반드시 트랩 결과에 있어야 한다.
        for key in &target_nonconf {
            match unchecked.iter().position(|u| same_value(u, key)) {
                Some(pos) => {
                    unchecked.remove(pos);
                }
                None => {
                    return Err(self.throw_error(
                        "TypeError",
                        "proxy 'ownKeys': non-configurable key of target missing from result",
                    ))
                }
            }
        }
        if extensible {
            return Ok(list);
        }
        // non-extensible: configurable 키도 전부 있어야 하고, 여분 키가 없어야 한다.
        for key in &target_conf {
            match unchecked.iter().position(|u| same_value(u, key)) {
                Some(pos) => {
                    unchecked.remove(pos);
                }
                None => {
                    return Err(self.throw_error(
                        "TypeError",
                        "proxy 'ownKeys': key of non-extensible target missing from result",
                    ))
                }
            }
        }
        if !unchecked.is_empty() {
            return Err(self.throw_error(
                "TypeError",
                "proxy 'ownKeys': result contains keys absent on non-extensible target",
            ));
        }
        Ok(list)
    }

    // §7.4.11 IteratorClose (정상 완료용): iterator.return() 을 호출하고 그 예외는 전파,
    // 결과가 객체가 아니면 TypeError. is*Of 의 조기탈출(IteratorClose)에 쓴다.
    fn iterator_close_ok(&mut self, it: &Value) -> Result<(), String> {
        let ret = self.member_get(it, "return")?;
        if is_callable(&ret) {
            let r = self.call_value(ret, Some(it.clone()), vec![])?;
            if !is_object(&r) {
                return Err(
                    self.throw_error("TypeError", "iterator return() did not return an object")
                );
            }
        }
        Ok(())
    }

    // other.keys() 이터레이터를 **지연** 순회하며 pred(k) 가 true 인 첫 키에서 멈춘다.
    // 조기 종료 시 IteratorClose(return 호출). 소진하면 false. §24.2.4 is*Of 의 조기탈출용
    // (예전엔 set_keys_vec 로 전량 드레인해 return 이 절대 안 불렸다).
    fn set_keys_any(
        &mut self,
        other: &Value,
        keys_fn: &Value,
        mut pred: impl FnMut(&Value) -> bool,
    ) -> Result<bool, String> {
        let iter = self.call_value(keys_fn.clone(), Some(other.clone()), vec![])?;
        loop {
            let (k, done) = self.gen_iter_next(&iter, Value::Undefined)?;
            if done {
                return Ok(false);
            }
            if pred(&k) {
                self.iterator_close_ok(&iter)?;
                return Ok(true);
            }
            self.tick()?;
        }
    }

    // ES2024 집합 연산 (§24.2.4). this 는 실제 Set, other 는 set-like. union/intersection/
    // difference/symmetricDifference 는 새 Set 을, is*Of 는 불리언을 돌려준다. size 비교로
    // this 순회(has 호출) vs other 순회(keys)를 고르는 것도 표준대로.
    fn set_binary_op(
        &mut self,
        s: Rc<RefCell<Vec<Value>>>,
        op: SetOp,
        other: Value,
    ) -> Result<Value, String> {
        let (other_size, has, keys) = self.get_set_record(&other)?;
        let this_data: Vec<Value> = s.borrow().clone();
        let this_size = this_data.len() as f64;
        fn contains(data: &[Value], v: &Value) -> bool {
            data.iter().any(|e| same_value_zero(e, v))
        }
        let new_set = |v: Vec<Value>| Value::SetVal(Rc::new(RefCell::new(v)));
        match op {
            SetOp::Union => {
                let mut result = this_data.clone();
                for k in self.set_keys_vec(&other, &keys)? {
                    if !contains(&result, &k) {
                        result.push(k);
                    }
                }
                Ok(new_set(result))
            }
            SetOp::Intersection => {
                let mut result: Vec<Value> = Vec::new();
                if this_size <= other_size {
                    for e in &this_data {
                        let r = self.call_value(has.clone(), Some(other.clone()), vec![e.clone()])?;
                        if to_bool(&r) && !contains(&result, e) {
                            result.push(e.clone());
                        }
                    }
                } else {
                    for k in self.set_keys_vec(&other, &keys)? {
                        if contains(&this_data, &k) && !contains(&result, &k) {
                            result.push(k);
                        }
                    }
                }
                Ok(new_set(result))
            }
            SetOp::Difference => {
                let mut result = this_data.clone();
                if this_size <= other_size {
                    let mut kept = Vec::new();
                    for e in &this_data {
                        let r = self.call_value(has.clone(), Some(other.clone()), vec![e.clone()])?;
                        if !to_bool(&r) {
                            kept.push(e.clone());
                        }
                    }
                    result = kept;
                } else {
                    for k in self.set_keys_vec(&other, &keys)? {
                        result.retain(|e| !same_value_zero(e, &k));
                    }
                }
                Ok(new_set(result))
            }
            SetOp::SymmetricDifference => {
                let mut result = this_data.clone();
                let mut seen: Vec<Value> = Vec::new();
                for k in self.set_keys_vec(&other, &keys)? {
                    if contains(&seen, &k) {
                        continue;
                    }
                    seen.push(k.clone());
                    if contains(&this_data, &k) {
                        result.retain(|e| !same_value_zero(e, &k));
                    } else if !contains(&result, &k) {
                        result.push(k);
                    }
                }
                Ok(new_set(result))
            }
            SetOp::IsSubsetOf => {
                if this_size > other_size {
                    return Ok(Value::Bool(false));
                }
                for e in &this_data {
                    let r = self.call_value(has.clone(), Some(other.clone()), vec![e.clone()])?;
                    if !to_bool(&r) {
                        return Ok(Value::Bool(false));
                    }
                }
                Ok(Value::Bool(true))
            }
            SetOp::IsSupersetOf => {
                if this_size < other_size {
                    return Ok(Value::Bool(false));
                }
                // other.keys() 지연 순회: this 에 없는 키가 나오면 조기탈출 + IteratorClose.
                let found_missing =
                    self.set_keys_any(&other, &keys, |k| !contains(&this_data, k))?;
                Ok(Value::Bool(!found_missing))
            }
            SetOp::IsDisjointFrom => {
                if this_size <= other_size {
                    for e in &this_data {
                        let r = self.call_value(has.clone(), Some(other.clone()), vec![e.clone()])?;
                        if to_bool(&r) {
                            return Ok(Value::Bool(false));
                        }
                    }
                    Ok(Value::Bool(true))
                } else {
                    // other.keys() 지연 순회: this 에 있는 키가 나오면 조기탈출 + IteratorClose.
                    let found_common =
                        self.set_keys_any(&other, &keys, |k| contains(&this_data, k))?;
                    Ok(Value::Bool(!found_common))
                }
            }
            _ => unreachable!(),
        }
    }

    // document.readyState 갱신 (loading → interactive → complete)
    pub fn set_ready_state(&mut self, state: &str) {
        if let Some(Value::Obj(m)) = env_get(&self.global, "document") {
            m.borrow_mut().insert("readyState".to_string(), Value::Str(state.to_string()));
        }
    }

    // document/window 레벨 이벤트 발화 (DOMContentLoaded/load). 프레임워크가
    // 여기 등록한 콜백에서 콘텐츠를 구성한다. 호출측이 dom 포인터를 잡고 있어야 함.
    pub fn fire_global(&mut self, event: &str) -> bool {
        self.begin_unit();
        let to_run: Vec<Value> = self
            .global_handlers
            .iter()
            .filter(|(t, _)| t == event)
            .map(|(_, f)| f.clone())
            .collect();
        let fired = !to_run.is_empty();
        // 문서 레벨 이벤트 객체 (target = 문서 루트)
        let evt = self.dom.map(|p| self.make_event(event, unsafe { (*p).root }));
        for f in to_run {
            let args = evt.clone().map(|e| vec![e]).unwrap_or_default();
            if let Err(e) = self.call_value(f, None, args) {
                println!("[js error] {}", e);
            }
            self.drain_microtasks();
        }
        fired
    }

    // 타이머 콜백 실행 (호출측이 dom 포인터 설정/해제). 에러는 격리.
    // 태스크(타이머/이벤트 콜백) 하나를 실행한다.
    // 이벤트 루프 규칙: 태스크가 끝나면 마이크로태스크 큐를 비운다.
    // 예전엔 안 비워서, 타이머 안에서 일어난 DOM 변경의 MutationObserver 배달과
    // 그 안에서 만든 Promise 의 .then 이 영영 안 돌았다 (조용히).
    pub fn run_callback(&mut self, cb: Value) {
        self.begin_unit();
        if let Err(e) = self.call_value(cb, None, Vec::new()) {
            println!("[js error] {}", e);
        }
        self.drain_microtasks();
        for line in std::mem::take(&mut self.console) {
            println!("[console] {}", line);
        }
    }

    // 표준 이벤트 루프 (HTML §8.1.6): 마이크로태스크 큐를 비우고 → 만기 타이머 하나를
    // 실행 → 다시 마이크로태스크, 둘 다 빌 때까지. --js 모드(test262)에서 스크립트가
    // 끝난 뒤 이걸 돌린다 — 안 그러면 async 테스트의 $DONE 이 마이크로태스크/타이머에서
    // 불려도 관측되지 않고 프로세스가 그냥 끝난다.
    // DOM 이 없는 순수 JS 실행용 (window 의 타이머 루프와 별개).
    pub fn run_event_loop(&mut self) {
        self.drain_microtasks();
        // 타이머: 지연 오름차순으로 하나씩. 각 타이머 사이에 마이크로태스크를 흘린다.
        // interval 은 무한이라 한 번만 (근사), 총 라운드는 상한을 둔다.
        for _round in 0..100_000 {
            if self.timers.is_empty() {
                break;
            }
            if self.budget_exhausted() {
                break;
            }
            // 지연이 가장 짧은 타이머를 꺼낸다 (동률이면 등록 순서)
            let mut best = 0usize;
            for i in 1..self.timers.len() {
                if self.timers[i].delay_ms < self.timers[best].delay_ms {
                    best = i;
                }
            }
            let timer = self.timers.remove(best);
            if self.cleared.contains(&timer.id) {
                continue;
            }
            self.begin_unit();
            if let Err(e) = self.call_value(timer.callback, None, Vec::new()) {
                if !e.starts_with(STEP_LIMIT_MSG) {
                    println!("[js error] {}", e);
                }
            }
            self.drain_microtasks();
        }
    }

    // onclick 속성 등 인라인 핸들러 소스 실행 (전역 환경에서)
    pub fn run_inline_handler(&mut self, src: &str) {
        self.begin_unit();
        if let Err(e) = self.run(src) {
            println!("[js error] {}", e);
        }
        self.drain_microtasks();
    }

    pub fn run(&mut self, src: &str) -> Result<Value, String> {
        self.begin_unit(); // 실행 단위(스크립트/핸들러)마다 한도 리셋
        self.js_stack.clear();
        self.err_stack = None;
        let program = parse(src)?;
        let env = self.global.clone();
        hoist_vars(&program, &env); // var 하이스팅 (전역)
        let r = match self.exec_block(&program, &env) {
            Ok(Flow::Normal(v)) | Ok(Flow::Return(v)) => Ok(v),
            Ok(_) => Ok(Value::Undefined),
            // 오류엔 호출 스택을 붙인다 — "어디서" 없이는 진단이 불가능하다.
            Err(e) => Err(self.with_stack(e)),
        };
        self.js_stack.clear();
        r
    }

    // 오류 메시지 뒤에 호출 스택(안쪽부터)을 붙인다. 이미 붙어 있으면 그대로.
    pub(crate) fn with_stack(&mut self, e: String) -> String {
        let stack = self.err_stack.take().unwrap_or_else(|| self.js_stack.clone());
        if stack.is_empty() || e.contains(" @ ") {
            return e;
        }
        let frames: Vec<String> = stack.iter().rev().take(6).cloned().collect();
        format!("{} @ {}", e, frames.join(" ← "))
    }

    // 구조분해 바인딩: 패턴을 재귀적으로 풀어 값을 선언. 값이 undefined 면 기본값 사용.
    fn bind_pattern(
        &mut self,
        pat: &crate::js::ast::Pattern,
        value: Value,
        env: &EnvRef,
        // assign=true(var): 하이스트된 기존 바인딩에 대입(env_set). false(let/const/param): 새로 선언.
        assign: bool,
    ) -> Result<(), String> {
        use crate::js::ast::Pattern;
        let bind = |env: &EnvRef, n: &str, v: Value| {
            if assign {
                env_set(env, n, v);
            } else {
                env_declare(env, n, v);
            }
        };
        match pat {
            Pattern::Name(n) => bind(env, n, value),
            // 멤버 대상: 선언이 아니라 대입이다 (o.p = v)
            Pattern::Member(e) => {
                self.assign_to(e, value, env)?;
            }
            Pattern::Object(props, rest) => {
                // null/undefined 구조분해는 TypeError (§14.3.3.3 RequireObjectCoercible)
                if matches!(value, Value::Undefined | Value::Null) {
                    let d = to_display(&value);
                    return Err(self
                        .throw_error("TypeError", format!("{} 은(는) 구조분해할 수 없음", d)));
                }
                // 계산된 키는 지금 평가한다 (평가 순서: 선언 순서 — 표준)
                let mut keys: Vec<String> = Vec::with_capacity(props.len());
                for (key, _, _) in props {
                    keys.push(match key {
                        crate::js::ast::PatKey::Static(k) => k.clone(),
                        crate::js::ast::PatKey::Computed(e) => {
                            let kv = self.eval(e, env)?;
                            key_of(&kv)
                        }
                    });
                }
                for ((_, sub, default), key) in props.iter().zip(keys.iter()) {
                    // 게터가 던지면 전파한다 (예전엔 unwrap_or 로 삼켜서 undefined 가 됐다)
                    let mut v = self.member_get(&value, key)?;
                    if matches!(v, Value::Undefined) {
                        if let Some(d) = default {
                            v = self.eval(d, env)?;
                            // 기본값이 익명 함수면 대상 이름을 갖는다 (NamedEvaluation).
                            //   var { a = function(){} } = {};  a.name === "a"
                            if let Pattern::Name(n) = sub {
                                // 콤마식 등 직접 익명 함수가 아니면 NamedEvaluation 안 한다
                                if is_anonymous_fn_expr(d) { Self::set_fn_name(&v, n); }
                            }
                        }
                    }
                    self.bind_pattern(sub, v, env, assign)?;
                }
                // { a, ...rest } — 분해되지 않은 나머지 own 프로퍼티를 객체로
                if let Some(rest_pat) = rest {
                    let consumed: std::collections::HashSet<&str> =
                        keys.iter().map(|k| k.as_str()).collect();
                    let mut map = ObjMap::new();
                    // §14.3.3.1 RestBindingInitialization = CopyDataProperties: own
                    // enumerable 만(non-enumerable·구멍 제외), 이미 분해된 키(consumed)
                    // 제외, 접근자는 Get(호출)한 값을 데이터로. Obj/Instance/Arr 통일.
                    if matches!(value, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
                        for (k, val) in builtins::own_enumerable_entries(&value) {
                            if consumed.contains(k.as_str()) {
                                continue;
                            }
                            let dv = if matches!(val, Value::Accessor(_)) {
                                self.member_get(&value, &k)?
                            } else {
                                val
                            };
                            map.insert(k, dv);
                        }
                    }
                    self.bind_pattern(
                        rest_pat,
                        Value::Obj(Rc::new(RefCell::new(map))),
                        env,
                        assign,
                    )?;
                }
            }
            // 배열 패턴은 반복자 프로토콜을 쓴다 (§8.6.2 IteratorBindingInitialization).
            // 예전엔 무조건 인덱스 접근이라 Set/Map/제너레이터/사용자 이터러블을 분해하면
            // 조용히 undefined 가 나왔다. 배열·문자열·Set·Map 은 유한하고 사용자 코드가
            // 끼지 않으므로 재료화 경로로 빠르게, 그 외는 지연 순회한다(무한 이터러블 방어).
            Pattern::Array(elems, rest) => {
                if matches!(value, Value::Undefined | Value::Null) {
                    let d = to_display(&value);
                    return Err(self
                        .throw_error("TypeError", format!("{} 은(는) 이터러블이 아님", d)));
                }
                let eager = matches!(
                    value,
                    Value::Arr(_) | Value::Str(_) | Value::SetVal(_) | Value::MapVal(_)
                );
                if eager {
                    let items = self.iterate_to_vec(&value)?;
                    for (i, slot) in elems.iter().enumerate() {
                        if let Some((sub, default)) = slot {
                            let mut v = items.get(i).cloned().unwrap_or(Value::Undefined);
                            if matches!(v, Value::Undefined) {
                                if let Some(d) = default {
                                    v = self.eval(d, env)?;
                                    if let Pattern::Name(n) = sub {
                                        // 콤마식 등 직접 익명 함수가 아니면 NamedEvaluation 안 한다
                                        if is_anonymous_fn_expr(d) { Self::set_fn_name(&v, n); }
                                    }
                                }
                            }
                            self.bind_pattern(sub, v, env, assign)?;
                        }
                    }
                    if let Some(rest_pat) = rest {
                        let tail: Vec<Value> =
                            items.iter().skip(elems.len()).cloned().collect();
                        self.bind_pattern(
                            rest_pat,
                            Value::Arr(ArrayObj::new(tail)),
                            env,
                            assign,
                        )?;
                    }
                    return Ok(());
                }
                let it = match self.try_get_iterator(&value)? {
                    Some(it) => it,
                    None => {
                        let t = type_of(&value).to_string();
                        return Err(self
                            .throw_error("TypeError", format!("{} 은(는) 이터러블이 아님", t)));
                    }
                };
                let mut done = false;
                // 구멍(elision)도 반복자를 한 칸 전진시킨다 — 표준
                for slot in elems.iter() {
                    let mut v = Value::Undefined;
                    if !done {
                        let (val, d) = self.gen_iter_next(&it, Value::Undefined)?;
                        if d {
                            done = true;
                        } else {
                            v = val;
                        }
                    }
                    if let Some((sub, default)) = slot {
                        if matches!(v, Value::Undefined) {
                            if let Some(d) = default {
                                v = self.eval(d, env)?;
                                if let Pattern::Name(n) = sub {
                                    // 콤마식 등 직접 익명 함수가 아니면 NamedEvaluation 안 한다
                                    if is_anonymous_fn_expr(d) { Self::set_fn_name(&v, n); }
                                }
                            }
                        }
                        self.bind_pattern(sub, v, env, assign)?;
                    }
                }
                if let Some(rest_pat) = rest {
                    let mut items = Vec::new();
                    while !done {
                        let (val, d) = self.gen_iter_next(&it, Value::Undefined)?;
                        if d {
                            break;
                        }
                        items.push(val);
                        self.tick()?;
                    }
                    self.bind_pattern(rest_pat, Value::Arr(ArrayObj::new(items)), env, assign)?;
                }
            }
        }
        Ok(())
    }

    // 새 실행 단위(스크립트/핸들러/타이머) 시작: 직전 단위가 쓴 시간을 총합에 누적한다.
    fn begin_unit(&mut self) {
        if let Some(s) = self.script_start.take() {
            self.js_spent_ms += s.elapsed().as_millis() as u64;
        }
        self.steps = 0;
        self.script_start = Some(std::time::Instant::now());
    }

    // await 한 값의 이행값 (promise 가 아니면 그대로, 거부면 throw).
    // for await 도 각 값에 이 규칙을 적용한다 (ES2018 §14.7.5).
    pub(super) fn await_value(&mut self, v: Value) -> Result<Value, String> {
        if !is_promise(&v) {
            return Ok(v); // thenable 아닌 값은 그대로
        }
        self.drain_microtasks();
        if let Value::Obj(o) = &v {
            let (state, value) = {
                let m = o.borrow();
                (
                    match m.get("\u{0}state") {
                        Some(Value::Str(s)) => s.clone(),
                        _ => "pending".into(),
                    },
                    m.get("\u{0}value").cloned().unwrap_or(Value::Undefined),
                )
            };
            match state.as_str() {
                "fulfilled" => return Ok(value),
                // 거부된 promise 를 await → 그 이유를 throw (표준)
                "rejected" => {
                    let msg = to_display(&value);
                    self.thrown = Some(value);
                    return Err(msg);
                }
                _ => {}
            }
        }
        Ok(Value::Undefined) // 펜딩(동기 모델에서 해소 불가) — 근사
    }

    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps & TIME_CHECK_MASK == 0 {
            if self.js_spent_ms() > self.total_budget_ms {
                return Err(format!("{} (페이지 전체 JS 예산 소진)", STEP_LIMIT_MSG));
            }
            if let Some(start) = self.script_start {
                if start.elapsed().as_millis() as u64 > self.script_budget_ms {
                    return Err(format!(
                        "{} ({}초 넘게 돌았다 — 무한 루프?)",
                        STEP_LIMIT_MSG,
                        self.script_budget_ms / 1000
                    ));
                }
            }
        }
        Ok(())
    }

    // 이 페이지에서 지금까지 JS 에 쓴 총 시간 (실행 단위마다 누적)
    fn js_spent_ms(&self) -> u64 {
        self.js_spent_ms + self.script_start.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0)
    }

    // 예산이 이미 바닥났는가 — 새 실행 단위(타이머/핸들러)를 시작하기 전에 본다.
    pub fn budget_exhausted(&self) -> bool {
        self.js_spent_ms() > self.total_budget_ms
    }

    // 함수 선언 호이스팅: 블록 실행 전에 FuncDecl 을 먼저 바인딩
    fn exec_block(&mut self, stmts: &[Stmt], env: &EnvRef) -> Result<Flow, String> {
        for s in stmts {
            if let Stmt::FuncDecl { name, params, body, is_generator, is_async, source, prologue_len } = s {
                let f = Value::Fn(Rc::new(JsFn {
                    priv_id: std::cell::Cell::new(0),
                    name: RefCell::new(name.clone()),
                    params: params.clone(),
                    body: body.clone(),
                    param_prologue_len: *prologue_len,
                    env: env.clone(),
                    is_arrow: false,
                    is_generator: *is_generator,
                    is_async: *is_async,
                    is_method: false,
                    this: None,
                    super_class: None,
                    props: RefCell::new(ObjMap::new()),
                    source: source.clone(),
                }));
                env_declare(env, name, f);
            }
        }
        let mut last = Value::Undefined;
        for s in stmts {
            match self.exec_stmt(s, env)? {
                Flow::Normal(v) => last = v,
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Normal(last))
    }

    fn exec_stmt(&mut self, stmt: &Stmt, env: &EnvRef) -> Result<Flow, String> {
        self.tick()?;
        // 직전 레이블 문이 남긴 레이블을 이 문(주로 루프)이 가져간다. 루프 아닌 문은 무시.
        let my_label = self.pending_label.take();
        match stmt {
            // with (obj) stmt — 객체 환경 레코드를 스코프 체인에 얹는다 (§14.11).
            // 이 스코프에서 이름을 찾을 때 obj 의 프로퍼티(프로토타입 체인 포함)를 본다.
            // (Symbol.unscopables 는 아직 반영하지 않는다 — 그건 별도 항목이다.)
            Stmt::With { obj, body } => {
                let o = self.eval(obj, env)?;
                if matches!(o, Value::Undefined | Value::Null) {
                    let d = to_display(&o);
                    return Err(self
                        .throw_error("TypeError", format!("{} 에는 with 를 쓸 수 없음", d)));
                }
                let scope = Env::new(Some(env.clone()));
                scope.borrow_mut().with_obj = Some(o);
                self.exec_stmt(body, &scope)
            }
            // ── ES 모듈 선언 ──
            // 모듈 평가(run_module)가 미리 처리한다. 여기 도달하면 클래식 스크립트에
            // 모듈 문법이 섞인 것 — 조용히 무시하지 않고 알린다.
            Stmt::Import { .. } => Err(
                "import 는 모듈(type=module)에서만 쓸 수 있음".to_string(),
            ),
            Stmt::ExportNamed { .. } | Stmt::ExportAll { .. } => Err(
                "export 는 모듈(type=module)에서만 쓸 수 있음".to_string(),
            ),
            // export default/선언은 모듈 밖에서도 선언 자체는 실행해 준다(관용).
            Stmt::ExportDefault(inner) | Stmt::ExportDecl(inner) => self.exec_stmt(inner, env),
            Stmt::VarDecl { kind, decls } => {
                let is_var = matches!(kind, crate::js::ast::DeclKind::Var);
                let is_const = matches!(kind, crate::js::ast::DeclKind::Const);
                for (pat, init) in decls {
                    match init {
                        // var 는 하이스트된 바인딩에 대입(env_set), let/const 는 새로 선언
                        Some(e) => {
                            let v = self.eval(e, env)?;
                            // const f = () => {} 에서 f.name 은 "f" 다 (NamedEvaluation).
                            // **구문상 익명 함수/클래스**일 때만이다 —
                            // `var w = makeFn()` 은 해당 없다 (표준).
                            if let crate::js::ast::Pattern::Name(n) = pat {
                                if is_anonymous_fn_expr(e) {
                                    Self::set_fn_name(&v, n);
                                }
                            }
                            self.bind_pattern(pat, v, env, is_var)?;
                        }
                        // var x; (초기화 없음)은 하이스트된 값 보존(덮지 않음). let x; 는 undefined.
                        None if !is_var => self.bind_pattern(pat, Value::Undefined, env, false)?,
                        None => {}
                    }
                    // const 로 선언한 이름들을 이 스코프에 const 로 표시(재대입 금지).
                    if is_const {
                        let mut names = Vec::new();
                        pattern_names(pat, &mut names);
                        let mut e = env.borrow_mut();
                        for n in names {
                            e.consts.insert(n);
                        }
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::FuncDecl { .. } => Ok(Flow::Normal(Value::Undefined)), // 호이스팅됨
            Stmt::ClassDecl(def) => {
                let cls = self.make_class(def, env)?;
                if let Some(name) = &def.name {
                    env_declare(env, name, cls);
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::If { cond, then, other } => {
                let c = self.eval(cond, env)?;
                let scope = Env::new(Some(env.clone()));
                if to_bool(&c) {
                    self.exec_block(then, &scope)
                } else if let Some(other) = other {
                    self.exec_block(other, &scope)
                } else {
                    Ok(Flow::Normal(Value::Undefined))
                }
            }
            Stmt::While { cond, body } => {
                loop {
                    self.tick()?;
                    if !to_bool(&self.eval(cond, env)?) {
                        break;
                    }
                    let scope = Env::new(Some(env.clone()));
                    match loop_action(self.exec_block(body, &scope)?, &my_label) {
                        LoopAct::Exit => break,
                        LoopAct::Next => {}
                        LoopAct::Propagate(f) => return Ok(f),
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::DoWhile { body, cond } => {
                loop {
                    self.tick()?;
                    let scope = Env::new(Some(env.clone()));
                    match loop_action(self.exec_block(body, &scope)?, &my_label) {
                        LoopAct::Exit => break,
                        LoopAct::Next => {}
                        LoopAct::Propagate(f) => return Ok(f),
                    }
                    if !to_bool(&self.eval(cond, env)?) {
                        break;
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::For { init, cond, step, body } => {
                use crate::js::ast::DeclKind;
                let head = Env::new(Some(env.clone())); // for(let i...) 스코프
                if let Some(init) = init {
                    self.exec_stmt(init, &head)?;
                }
                // let/const 루프 변수는 반복마다 새 바인딩 (ES6 per-iteration environment).
                // 클로저가 각 반복의 값을 포착 → for(let i…) cbs.push(()=>i) 가 [0,1,2].
                let mut loop_vars: Vec<String> = Vec::new();
                if let Some(s) = init {
                    if let Stmt::VarDecl { kind: DeclKind::Let | DeclKind::Const, decls } = &**s {
                        for (pat, _) in decls {
                            pattern_names(pat, &mut loop_vars);
                        }
                    }
                }
                // src 의 루프 변수 값을 복사한 새 환경 (var/식 init 은 그대로 재사용).
                let make_iter = |src: &EnvRef| -> EnvRef {
                    if loop_vars.is_empty() {
                        return src.clone();
                    }
                    let e = Env::new(Some(env.clone()));
                    for name in &loop_vars {
                        env_declare(&e, name, env_get(src, name).unwrap_or(Value::Undefined));
                    }
                    e
                };
                let mut cur = make_iter(&head);
                loop {
                    self.tick()?;
                    if let Some(cond) = cond {
                        if !to_bool(&self.eval(cond, &cur)?) {
                            break;
                        }
                    }
                    let scope = Env::new(Some(cur.clone()));
                    match loop_action(self.exec_block(body, &scope)?, &my_label) {
                        LoopAct::Exit => break,
                        LoopAct::Next => {}
                        LoopAct::Propagate(f) => return Ok(f),
                    }
                    // 다음 반복: 현재 값 복사 후 step 실행 (step 은 새 바인딩에 반영)
                    let next = make_iter(&cur);
                    if let Some(step) = step {
                        self.eval(step, &next)?;
                    }
                    cur = next;
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::Return(e) => {
                let v = match e {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                Ok(Flow::Return(v))
            }
            Stmt::Break(label) => Ok(Flow::Break(label.clone())),
            Stmt::Continue(label) => Ok(Flow::Continue(label.clone())),
            Stmt::Labeled(label, inner) => {
                // 레이블을 직후 문(주로 루프)이 가져가도록 남긴다.
                self.pending_label = Some(label.clone());
                let r = self.exec_stmt(inner, env)?;
                self.pending_label = None; // 루프면 이미 take, 아니면 여기서 정리
                // 루프 아닌 레이블 문(블록/if 등)을 지목한 break/continue 는 여기서 소비.
                match r {
                    Flow::Break(Some(l)) if l == *label => Ok(Flow::Normal(Value::Undefined)),
                    Flow::Continue(Some(l)) if l == *label => Ok(Flow::Normal(Value::Undefined)),
                    other => Ok(other),
                }
            }
            Stmt::Block(stmts) => {
                let scope = Env::new(Some(env.clone()));
                self.exec_block(stmts, &scope)
            }
            Stmt::Expr(e) => Ok(Flow::Normal(self.eval(e, env)?)),
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                let msg = error_text(&v);
                self.thrown = Some(v);
                Err(msg)
            }
            Stmt::Try { body, catch, finally } => {
                let scope = Env::new(Some(env.clone()));
                let mut result = self.exec_block(body, &scope);
                if let Err(e) = &result {
                    // 스텝 한도 초과는 잡을 수 없음 (가드 무력화 방지)
                    if !e.starts_with(STEP_LIMIT_MSG) {
                        if let Some((param, cbody)) = catch {
                            // 잡힌 오류의 스택 스냅샷은 버린다 (다음 오류가 자기 스택을 갖도록)
                            self.err_stack = None;
                            // throw 된 값이 있으면 그 값, 네이티브 에러면 메시지 문자열
                            // 내부 오류(엔진이 Err(String) 으로 올린 것)도 진짜 Error
                            // 객체로 잡힌다. 예전엔 문자열이 잡혀서 e.message 가 undefined,
                            // e instanceof Error 가 false 였다.
                            let caught = match self.thrown.take() {
                                Some(v) => v,
                                None => self.error_from_msg(e),
                            };
                            let cscope = Env::new(Some(env.clone()));
                            if let Some(p) = param {
                                // 식별자/구조분해 패턴 모두 bind_pattern 으로 (assign=false: 선언).
                                self.bind_pattern(p, caught, &cscope, false)?;
                            }
                            result = self.exec_block(cbody, &cscope);
                        }
                    }
                }
                if let Some(fbody) = finally {
                    let fscope = Env::new(Some(env.clone()));
                    // finally 의 에러/제어 흐름이 우선
                    match self.exec_block(fbody, &fscope)? {
                        Flow::Normal(_) => {}
                        flow => return Ok(flow),
                    }
                }
                result
            }
            Stmt::ForIn { name, obj, body } => {
                let target = self.eval(obj, env)?;
                let keys: Vec<String> = match &target {
                    // __proto__ 링크는 열거 대상 아님(JS 에서 non-enumerable accessor)
                    Value::Obj(m) => enumerable_keys(m),
                    // 클래스 인스턴스: own 열거 가능 필드(내부/private/비열거 제외).
                    // 프로토타입 메서드는 비열거라 안 나온다(표준).
                    Value::Instance(i) => {
                        let f = i.fields.borrow();
                        f.keys()
                            .filter(|k| {
                                !is_internal_key(k) && !f.contains_key(&nonenum_marker(k))
                            })
                            .cloned()
                            .collect()
                    }
                    // 희소 배열의 구멍은 for-in 이 건너뛴다 (§enumerate: 존재 인덱스만).
                    // defineProperty 로 non-enumerable 이 된 인덱스도 제외 (§10.4.2).
                    Value::Arr(a) => a
                        .present_indices()
                        .iter()
                        .filter(|&&i| !matches!(a.index_attr(i), Some(at) if at & ATTR_ENUMERABLE == 0))
                        .map(|i| i.to_string())
                        .collect(),
                    Value::Str(s) => (0..s.encode_utf16().count()).map(|i| i.to_string()).collect(),
                    // 함수도 ordinary object — 열거 가능한 own 프로퍼티를 순회
                    // (name/length/prototype 및 상속된 Function/Object.prototype 멤버는 비열거).
                    Value::Fn(f) => {
                        let b = f.props.borrow();
                        b.keys()
                            .filter(|k| {
                                !is_internal_key(k)
                                    && !matches!(k.as_str(), "prototype" | "name" | "length")
                                    && !b.contains_key(&nonenum_marker(k))
                            })
                            .cloned()
                            .collect()
                    }
                    _ => Vec::new(), // null/undefined 등: 순회 없음 (JS 동일)
                };
                for k in keys {
                    self.tick()?;
                    let scope = Env::new(Some(env.clone()));
                    env_declare(&scope, name, Value::Str(k));
                    match loop_action(self.exec_block(body, &scope)?, &my_label) {
                        LoopAct::Exit => break,
                        LoopAct::Next => {}
                        LoopAct::Propagate(f) => return Ok(f),
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::ForOf { name, iter, body, is_await } => {
                let target = self.eval(iter, env)?;
                // for await: 각 값이 promise 면 이행값을 꺼낸다 (ES2018 §14.7.5).
                // 우리 promise 는 동기 정착 모델이라 마이크로태스크를 흘리고 값을 읽으면 된다.
                let unwrap = *is_await;
                // 유한한 내장 이터러블(배열/문자열/Set/Map/재료화 반복자)은 재료화해 순회.
                let finite = matches!(&target,
                    Value::Arr(_) | Value::Str(_) | Value::SetVal(_) | Value::MapVal(_))
                    || matches!(&target, Value::Obj(o) if o.borrow().contains_key("\u{0}items"));
                if !finite {
                    // 반복자 프로토콜(지연): 제너레이터/사용자 [Symbol.iterator] 이터러블/
                    // 반복자 객체. 한 번에 하나씩 뽑아 무한+break 에도 대응.
                    // for await 는 @@asyncIterator 를 먼저 찾는다 (표준)
                    let found = if unwrap {
                        self.try_get_async_iterator(&target)?
                    } else {
                        self.try_get_iterator(&target)?
                    };
                    if let Some(iter_obj) = found {
                        loop {
                            self.tick()?;
                            // 비동기 이터레이터의 next() 는 promise 를 돌려준다 → 풀어야 한다
                            let (v, done) =
                                self.gen_iter_next_maybe_async(&iter_obj, Value::Undefined, unwrap)?;
                            if done {
                                break;
                            }
                            let v = if unwrap { self.await_value(v)? } else { v };
                            let scope = Env::new(Some(env.clone()));
                            env_declare(&scope, name, v);
                            match loop_action(self.exec_block(body, &scope)?, &my_label) {
                                LoopAct::Exit => break,
                                LoopAct::Next => {}
                                LoopAct::Propagate(f) => return Ok(f),
                            }
                        }
                        return Ok(Flow::Normal(Value::Undefined));
                    }
                    let t = type_of(&target).to_string();
                    return Err(self.throw_error("TypeError", format!("{} 은(는) 반복 가능하지 않음", t)));
                }
                let values = self.iterate_to_vec(&target)?;
                for v in values {
                    self.tick()?;
                    let v = if unwrap { self.await_value(v)? } else { v };
                    let scope = Env::new(Some(env.clone()));
                    env_declare(&scope, name, v);
                    match loop_action(self.exec_block(body, &scope)?, &my_label) {
                        LoopAct::Exit => break,
                        LoopAct::Next => {}
                        LoopAct::Propagate(f) => return Ok(f),
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::Switch { disc, cases } => {
                let d = self.eval(disc, env)?;
                let scope = Env::new(Some(env.clone()));
                let mut start = None;
                for (i, (test, _)) in cases.iter().enumerate() {
                    if let Some(t) = test {
                        let tv = self.eval(t, &scope)?;
                        if strict_eq(&d, &tv) {
                            start = Some(i);
                            break;
                        }
                    }
                }
                if start.is_none() {
                    start = cases.iter().position(|(t, _)| t.is_none()); // default
                }
                if let Some(s) = start {
                    for (_, stmts) in &cases[s..] {
                        // 폴스루: break 가 나올 때까지 다음 케이스도 실행. 레이블 없는 break
                        // 또는 이 switch 레이블을 지목한 break 면 종료, 그 외(continue/외부
                        // 레이블/return)는 상위로 전파.
                        match self.exec_block(stmts, &scope)? {
                            Flow::Break(None) => return Ok(Flow::Normal(Value::Undefined)),
                            Flow::Break(Some(l)) if Some(&l) == my_label.as_ref() => {
                                return Ok(Flow::Normal(Value::Undefined))
                            }
                            Flow::Normal(_) => {}
                            other => return Ok(other),
                        }
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
        }
    }

    fn eval(&mut self, expr: &Expr, env: &EnvRef) -> Result<Value, String> {
        self.tick()?;
        match expr {
            Expr::Num(n) => Ok(Value::Num(*n)),
            // 홀은 배열 리터럴 안에서만 의미가 있다 (Expr::Array 가 처리). 단독 평가는 undefined.
            Expr::Hole => Ok(Value::Undefined),
            Expr::BigInt(d) => crate::js::bigint::BigInt::parse(d)
                .map(|b| Value::BigInt(Rc::new(b)))
                .ok_or_else(|| format!("잘못된 BigInt 리터럴: {}", d)),
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Undefined => Ok(Value::Undefined),
            Expr::Ident(name) => match env_get(env, name) {
                // import 바인딩은 살아있는 바인딩이다 — 스코프에 접근자가 들어 있으면
                // 읽는 시점에 모듈의 현재 값을 가져온다. 값 스냅샷으로 흉내내면
                // 순환 의존에서 "아직 초기화 안 된 이름"을 영영 undefined 로 굳혀버린다.
                Some(Value::Accessor(acc)) => match &acc.get {
                    Some(g) => self.call_value(g.clone(), None, vec![]),
                    None => Ok(Value::Undefined),
                },
                Some(v) => Ok(v),
                None => {
                    // window(전역 객체) 프로퍼티 폴백 — window.X = v 를 맨 X 로 읽게 함.
                    // 브라우저에선 window 가 곧 전역 객체 (naver 의 ndpsdk 등).
                    if let Some(v) = self.window_prop(name) {
                        return Ok(v);
                    }
                    if self.lenient {
                        *self.lenient_hits.entry(name.clone()).or_default() += 1;
                        Ok(Value::Undefined)
                    } else {
                        Err(self.throw_error("ReferenceError", format!("{} 은(는) 정의되지 않음", name)))
                    }
                }
            },
            Expr::Array(items) => {
                let mut v = Vec::new();
                let mut holes = std::collections::HashSet::new();
                for item in items {
                    match item {
                        // 엘리전 [1,,3] → 구멍 (명시 undefined 와 구별)
                        Expr::Hole => {
                            holes.insert(v.len());
                            v.push(Value::Undefined);
                        }
                        Expr::Spread(inner) => {
                            let val = self.eval(inner, env)?;
                            // null/undefined 전개는 TypeError (표준). 조용히 빈 배열로 넘기면
                            // 진짜 버그가 숨는다.
                            if matches!(val, Value::Undefined | Value::Null) {
                                let d = to_display(&val);
                                return Err(self.throw_error(
                                    "TypeError",
                                    format!("{} 은(는) 이터러블이 아님", d),
                                ));
                            }
                            v.extend(self.iterate_to_vec(&val)?);
                        }
                        _ => v.push(self.eval(item, env)?),
                    }
                }
                Ok(Value::Arr(if holes.is_empty() {
                    ArrayObj::new(v)
                } else {
                    ArrayObj::with_holes(v, holes)
                }))
            }
            // 스프레드가 배열/호출 밖에 홀로 나오면 값 그대로 (관용)
            Expr::Spread(inner) => self.eval(inner, env),
            Expr::Object(props) => {
                let mut map = ObjMap::new();
                for (k, e) in props {
                    if matches!(k, PropKey::Spread) {
                        // {...obj} — obj/배열/인스턴스의 own 프로퍼티 병합
                        match self.eval(e, env)? {
                            v @ (Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) => {
                                // §7.3.25 CopyDataProperties: own enumerable 만 복사 —
                                // non-enumerable 프로퍼티·배열 구멍은 제외. 예전엔 Obj 는
                                // raw iter(non-enumerable 포함), 배열은 구멍/non-enum 포함.
                                for (k, val) in builtins::own_enumerable_entries(&v) {
                                    // 접근자는 Get(getter 호출)한 값을 데이터 프로퍼티로 복사.
                                    let dv = if matches!(val, Value::Accessor(_)) {
                                        self.member_get(&v, &k)?
                                    } else {
                                        val
                                    };
                                    map.insert(k, dv);
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }
                    let key = match k {
                        PropKey::Static(s) => s.clone(),
                        PropKey::Getter(s) | PropKey::Setter(s) => s.clone(),
                        // { get/set [expr]() {..} } — 키를 런타임 평가 (심볼 키도 가능)
                        PropKey::Computed(ke)
                        | PropKey::ComputedGetter(ke)
                        | PropKey::ComputedSetter(ke) => key_of(&self.eval(ke, env)?),
                        PropKey::Spread => unreachable!(),
                    };
                    let val = self.eval(e, env)?;
                    // 객체 리터럴의 메서드/익명 함수 프로퍼티도 이름을 갖는다
                    // ({ m(){} }).m.name === "m" (§13.2.5.5 PropertyDefinitionEvaluation).
                    // 단 `__proto__: v` 는 프로퍼티 정의가 아니라 [[Prototype]] 설정이므로
                    // 이름을 주지 않는다 (§B.3.1) — 표준이 명시적으로 제외한다.
                    if key != "__proto__" {
                        Self::set_fn_name(&val, &key);
                    }
                    // 접근자: get/set 함수를 Accessor 로 감싼다. 같은 키에 get 과 set 이
                    // 따로 오면({get x(){}, set x(v){}}) 하나의 접근자로 병합해야 한다.
                    let is_get = matches!(k, PropKey::Getter(_) | PropKey::ComputedGetter(_));
                    let is_set = matches!(k, PropKey::Setter(_) | PropKey::ComputedSetter(_));
                    if is_get || is_set {
                        let (mut g, mut st) = match map.get(&key) {
                            Some(Value::Accessor(a)) => (a.get.clone(), a.set.clone()),
                            _ => (None, None),
                        };
                        if is_get {
                            g = Some(val);
                        } else {
                            st = Some(val);
                        }
                        map.insert(key, Value::Accessor(Rc::new(AccessorPair { get: g, set: st })));
                    } else {
                        map.insert(key, val);
                    }
                }
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
            }
            Expr::Func { name, params, body, is_arrow, is_generator, is_async, source, prologue_len } => {
                // 화살표는 정의 시점 this 를 캡처 (렉시컬)
                let this = if *is_arrow { env_get(env, "this").map(Box::new) } else { None };
                // 명명 함수식: 자기 이름을 감싸는 스코프에 바인딩(재귀용). 외부엔 미노출.
                let fn_env = match name {
                    Some(_) => Env::new(Some(env.clone())),
                    None => env.clone(),
                };
                let f = Rc::new(JsFn {
                    // 클래스 본문 안에서 만든 함수/화살표는 그 클래스의 private 이름을
                    // 본다 (렉시컬). 나중에 콜백으로 호출돼도 마찬가지다.
                    priv_id: std::cell::Cell::new(self.priv_id),
                    // 명명 함수식은 그 이름, 익명이면 NamedEvaluation 이 나중에 채운다
                    name: RefCell::new(name.clone().unwrap_or_default()),
                    params: params.clone(),
                    body: body.clone(),
                    param_prologue_len: *prologue_len,
                    env: fn_env.clone(),
                    is_arrow: *is_arrow,
                    is_generator: *is_generator,
                    is_async: *is_async,
                    is_method: false,
                    this,
                    super_class: None,
                    props: RefCell::new(ObjMap::new()),
                    source: source.clone(),
                });
                if let Some(n) = name {
                    env_declare(&fn_env, n, Value::Fn(f.clone()));
                }
                Ok(Value::Fn(f))
            }
            Expr::Yield { arg, .. } => {
                // 지연 제너레이터는 generator.rs 가 모든 yield 를 처리한다. 여기 도달하는
                // 것은 제너레이터 밖 yield(오용)뿐 — 인자만 평가하고 undefined.
                if let Some(e) = arg {
                    self.eval(e, env)?;
                }
                Ok(Value::Undefined)
            }
            Expr::This => Ok(env_get(env, "this").unwrap_or(Value::Undefined)),
            Expr::Super => {
                // super 단독은 거의 안 쓰임 — super.method()/super() 는 특수 처리됨
                Ok(Value::Undefined)
            }
            Expr::New { callee, args } => {
                let class = self.eval(callee, env)?;
                let mut arg_vals = Vec::new();
                arg_vals.extend(self.eval_args(args, env)?);
                self.construct(class, arg_vals)
            }
            // new.target: new 로 호출됐으면 그 생성자, 아니면 undefined. construct 가 스코프에
            // 숨김 바인딩을 심는다. 일반 함수 호출 스코프엔 없어 undefined.
            Expr::NewTarget => Ok(env_get(env, "\u{0}newtarget").unwrap_or(Value::Undefined)),
            // await expr: 대상이 promise 면 마이크로태스크를 드레인해 이행시킨 뒤 값.
            // (우리 promise 는 동기 resolve 모델이라 드레인만으로 이행됨)
            Expr::Await(inner) => {
                let v = self.eval(inner, env)?;
                self.await_value(v)
            }
            Expr::Class(def) => self.make_class(def, env),
            Expr::Sequence(items) => {
                let mut last = Value::Undefined;
                for item in items {
                    last = self.eval(item, env)?;
                }
                Ok(last)
            }
            Expr::Regex { source, flags } => Ok(make_regex_obj(source, flags)),
            // 태그드 템플릿: tag(strings, ...values). strings.raw 는 이스케이프 처리 전 원문.
            Expr::Tagged { tag, cooked, raw, values } => {
                let f = self.eval(tag, env)?;
                let strings = ArrayObj::new(
                    cooked.iter().map(|c| Value::Str(c.clone())).collect::<Vec<_>>(),
                );
                let raws = ArrayObj::new(
                    raw.iter().map(|r| Value::Str(r.clone())).collect::<Vec<_>>(),
                );
                strings.set_prop("raw".to_string(), Value::Arr(raws));
                let mut args = vec![Value::Arr(strings)];
                for v in values {
                    args.push(self.eval(v, env)?);
                }
                self.call_value(f, None, args)
            }
            Expr::Template(parts) => {
                let mut s = String::new();
                for part in parts {
                    match part {
                        TemplatePart::Lit(t) => s.push_str(t),
                        TemplatePart::Expr(e) => {
                            // ${expr} 는 ToString (§13.2.8.6): Symbol→TypeError, 객체는
                            // toString/valueOf 호출, abrupt 전파. 예전엔 관대한 to_primitive
                            // +to_display 라 `${Symbol()}` 이 안 던지고 abrupt 도 삼켰다.
                            let v = self.eval(e, env)?;
                            s.push_str(&self.to_string_value(&v)?);
                        }
                    }
                }
                Ok(Value::Str(s))
            }
            Expr::Unary { op, expr } => {
                // typeof 는 미선언 식별자에 던지지 않고 "undefined" 반환 (기능 탐지 관용:
                // typeof window !== 'undefined', typeof require !== 'undefined' 등)
                if matches!(op, UnOp::Typeof) {
                    if let Expr::Ident(name) = expr.as_ref() {
                        // 전역 객체의 이름 붙은 프로퍼티(id 있는 요소 등)도 '있는' 것이다 —
                        // 그래야 typeof 와 실제 조회가 같은 답을 한다.
                        if env_get(env, name).is_none() {
                            return Ok(match self.window_prop(name) {
                                Some(v) => Value::Str(type_of(&v).to_string()),
                                None => Value::Str("undefined".to_string()),
                            });
                        }
                    }
                }
                // delete obj.key / obj[key] — 실제로 own 프로퍼티 제거 후 true.
                if matches!(op, UnOp::Delete) {
                    if let Expr::Member { obj, prop, computed } = expr.as_ref() {
                        let target = self.eval(obj, env)?;
                        let key = match (computed, prop.as_ref()) {
                            (false, Expr::Str(s)) => s.clone(),
                            _ => {
                                let kv = self.eval(prop, env)?;
                                self.to_property_key(kv)? // 객체 키 toString / Symbol 내부키
                            }
                        };
                        match &target {
                            Value::Obj(m) => {
                                // configurable:false 프로퍼티는 삭제되지 않고 false 를 낸다
                                // (§10.1.10). 예전엔 서술자를 무시하고 무조건 지웠다.
                                let exists = m.borrow().contains_key(&key);
                                if exists {
                                    let attrs = prop_attrs(&m.borrow(), &key);
                                    if attrs & ATTR_CONFIGURABLE == 0 {
                                        return Ok(Value::Bool(false));
                                    }
                                    let mut mm = m.borrow_mut();
                                    mm.remove(&key);
                                    mm.remove(&attr_marker(&key));
                                    mm.remove(&nonenum_marker(&key));
                                }
                            }
                            // 클래스 정적 멤버는 클래스 객체의 own 프로퍼티 — 정적 메서드/
                            // 필드는 configurable:true 라 삭제 가능 (§ ClassDefinitionEvaluation).
                            // prototype/name/length 는 non-configurable → false.
                            Value::Class(cls) => {
                                if matches!(key.as_str(), "prototype" | "name" | "length") {
                                    return Ok(Value::Bool(false));
                                }
                                cls.statics.borrow_mut().remove(&key);
                            }
                            // 클래스 인스턴스 필드도 own 프로퍼티다 — configurable 존중 삭제.
                            Value::Instance(inst) => {
                                let exists = inst.fields.borrow().contains_key(&key);
                                if exists {
                                    let attrs = prop_attrs(&inst.fields.borrow(), &key);
                                    if attrs & ATTR_CONFIGURABLE == 0 {
                                        return Ok(Value::Bool(false));
                                    }
                                    let mut mm = inst.fields.borrow_mut();
                                    mm.remove(&key);
                                    mm.remove(&attr_marker(&key));
                                    mm.remove(&nonenum_marker(&key));
                                }
                            }
                            // 함수 프로퍼티도 configurable 을 존중해 삭제한다 (§10.2 —
                            // 함수는 ordinary object). name/length 는 configurable:true 라
                            // 삭제 가능 — 계산 프로퍼티라 삭제 툼스톤을 남겨 이후 member_get/
                            // gOPD 가 없는 것으로 본다. prototype 은 대상 밖(관대하게 true).
                            Value::Fn(func) => {
                                if matches!(key.as_str(), "name" | "length") {
                                    // props 오버라이드가 있으면 그 configurable 을 따르고,
                                    // 없으면 계산 프로퍼티(configurable:true)라 삭제 가능.
                                    if func.props.borrow().contains_key(&key)
                                        && prop_attrs(&func.props.borrow(), &key)
                                            & ATTR_CONFIGURABLE
                                            == 0
                                    {
                                        return Ok(Value::Bool(false));
                                    }
                                    let mut mm = func.props.borrow_mut();
                                    mm.remove(&key);
                                    mm.remove(&attr_marker(&key));
                                    mm.remove(&nonenum_marker(&key));
                                    mm.insert(format!("\u{0}fndel:{}", key), Value::Bool(true));
                                } else if key != "prototype"
                                    && func.props.borrow().contains_key(&key)
                                {
                                    if prop_attrs(&func.props.borrow(), &key) & ATTR_CONFIGURABLE
                                        == 0
                                    {
                                        return Ok(Value::Bool(false));
                                    }
                                    let mut mm = func.props.borrow_mut();
                                    mm.remove(&key);
                                    mm.remove(&attr_marker(&key));
                                    mm.remove(&nonenum_marker(&key));
                                }
                            }
                            // Proxy: deleteProperty 트랩 (§10.5.10, 없으면 타깃 위임).
                            // 반응성 라이브러리(Vue 등)가 delete 를 이 트랩으로 잡는다.
                            // proxy_delete 로 통일 — GetMethod·non-extensible invariant·
                            // 위임(중첩 프록시/non-configurable 존중)까지 한 경로.
                            Value::Proxy(p) => {
                                let p = p.clone();
                                return Ok(Value::Bool(self.proxy_delete(&p, &key)?));
                            }
                            Value::Arr(a) => {
                                // 배열 요소 삭제는 진짜 구멍(hole)을 남긴다 (길이 불변) —
                                // delete arr[i] 후 i 는 hasOwnProperty/in/for-in 에서 사라진다.
                                if let Ok(i) = key.parse::<usize>() {
                                    // non-configurable 로 정의된 인덱스는 삭제 불가 (§10.4.2).
                                    if matches!(a.index_attr(i), Some(at) if at & ATTR_CONFIGURABLE == 0)
                                    {
                                        return Ok(Value::Bool(false));
                                    }
                                    let in_range = {
                                        let mut b = a.borrow_mut();
                                        if i < b.len() {
                                            b[i] = Value::Undefined;
                                            true
                                        } else {
                                            false
                                        }
                                    };
                                    if in_range {
                                        a.mark_hole(i);
                                        a.clear_index_attr(i);
                                    }
                                }
                            }
                            // 내장 함수의 name/length 는 configurable → delete 성공.
                            // native_props 에서 지우고 삭제 마커를 남긴다(native_meta 기본값
                            // 도 안 보이게). 예전엔 delete 가 no-op 라 verifyProperty 의
                            // configurable 검사가 깨졌다.
                            Value::Native(n) => {
                                let ov = self.native_props.entry(*n).or_default();
                                ov.remove(&key);
                                if matches!(key.as_str(), "name" | "length") {
                                    ov.insert(format!("\u{0}del:{}", key), Value::Bool(true));
                                }
                            }
                            _ => {}
                        }
                        return Ok(Value::Bool(true));
                    }
                    return Ok(Value::Bool(true));
                }
                let v = self.eval(expr, env)?;
                // BigInt 단항: -x 는 부호 반전, ~x 는 2의 보수, +x 는 TypeError (표준).
                if let Value::BigInt(b) = &v {
                    return match op {
                        UnOp::Neg => Ok(Value::BigInt(Rc::new(b.negate()))),
                        UnOp::BitNot => Ok(Value::BigInt(Rc::new(b.bitnot()))),
                        UnOp::Pos => Err(self
                            .throw_error("TypeError", "Cannot convert a BigInt value to a number")),
                        UnOp::Not => Ok(Value::Bool(b.is_zero())),
                        UnOp::Typeof => Ok(Value::Str("bigint".to_string())),
                        UnOp::Void => Ok(Value::Undefined),
                        UnOp::Delete => Ok(Value::Bool(true)),
                    };
                }
                Ok(match op {
                    // 단항 +/- 는 ToNumber (§13.5.4/13.5.5): 객체는 ToPrimitive(number)
                    // 후 ToNumber, Symbol 은 TypeError(예전엔 to_num 이 관대해 NaN 이었다),
                    // valueOf/toString 의 abrupt 도 전파. (BigInt 는 위에서 처리.)
                    UnOp::Neg => Value::Num(-self.to_number_value(&v)?),
                    UnOp::Pos => Value::Num(self.to_number_value(&v)?),
                    UnOp::Not => Value::Bool(!to_bool(&v)),
                    UnOp::Typeof => Value::Str(type_of(&v).to_string()),
                    UnOp::BitNot => Value::Num(!self.to_int32(&v)? as f64),
                    // void: 피연산자 평가 후 undefined. delete: 근사(항상 true)
                    UnOp::Void => Value::Undefined,
                    UnOp::Delete => Value::Bool(true),
                })
            }
            Expr::Update { op, prefix, target } => {
                let cur = self.eval(target, env)?;
                // ToNumeric (§7.1.4): BigInt 는 BigInt 로 증감(타입 유지), 그 외는 ToNumber
                // (Symbol→TypeError, 객체는 valueOf/toString). 예전 to_num 은 BigInt 를
                // Number 로 바꾸고 Symbol 을 NaN 으로 삼키고 valueOf abrupt 도 삼켰다.
                if let Value::BigInt(b) = &cur {
                    let one = crate::js::bigint::BigInt::from_i64(1);
                    let new_bi = match op {
                        UpdOp::Inc => b.add(&one),
                        UpdOp::Dec => b.sub(&one),
                    };
                    let new_val = Value::BigInt(Rc::new(new_bi));
                    self.assign_to(target, new_val.clone(), env)?;
                    return Ok(if *prefix { new_val } else { cur });
                }
                let old = self.to_number_value(&cur)?;
                let new = match op {
                    UpdOp::Inc => old + 1.0,
                    UpdOp::Dec => old - 1.0,
                };
                self.assign_to(target, Value::Num(new), env)?;
                Ok(Value::Num(if *prefix { new } else { old }))
            }
            Expr::Binary { op, left, right } => {
                // `#x in obj` — 브랜드 검사 (§13.10.1). 왼쪽은 **값이 아니라 private 이름**
                // 이라 평가하면 안 된다. 예전엔 평가해서 ReferenceError 가 났다.
                if matches!(op, BinOp::In) {
                    if let Expr::Ident(name) = left.as_ref() {
                        if let Some(priv_name) = name.strip_prefix('#') {
                            let r = self.eval(right, env)?;
                            return Ok(Value::Bool(self.has_private(&r, priv_name)));
                        }
                    }
                }
                let l = self.eval(left, env)?;
                let r = self.eval(right, env)?;
                self.binary(*op, l, r)
            }
            Expr::Logical { op, left, right } => {
                let l = self.eval(left, env)?;
                match op {
                    LogOp::And => {
                        if to_bool(&l) {
                            self.eval(right, env)
                        } else {
                            Ok(l)
                        }
                    }
                    LogOp::Or => {
                        if to_bool(&l) {
                            Ok(l)
                        } else {
                            self.eval(right, env)
                        }
                    }
                }
            }
            Expr::Ternary { cond, then, other } => {
                if to_bool(&self.eval(cond, env)?) {
                    self.eval(then, env)
                } else {
                    self.eval(other, env)
                }
            }
            // 구조분해 할당 [a,b]=arr / ({x,y}=o): 기존 바인딩에 대입(assign=true)
            Expr::AssignPattern { pattern, value } => {
                let v = self.eval(value, env)?;
                self.bind_pattern(pattern, v.clone(), env, true)?;
                Ok(v) // 할당식 값 = 우변
            }
            Expr::Assign { op, target, value } => {
                // &&= / ||= / ??= 는 단락: 조건 만족 안 하면 대입도 안 함
                if matches!(op, AssignOp::And | AssignOp::Or | AssignOp::Nullish) {
                    let old = self.eval(target, env)?;
                    let do_assign = match op {
                        AssignOp::And => to_bool(&old),
                        AssignOp::Nullish => matches!(old, Value::Null | Value::Undefined),
                        _ => !to_bool(&old),
                    };
                    if !do_assign {
                        return Ok(old);
                    }
                    let rhs = self.eval(value, env)?;
                    self.assign_to(target, rhs.clone(), env)?;
                    return Ok(rhs);
                }
                // 표준 §13.15.2: **왼쪽 참조를 먼저** 평가하고 그 다음 오른쪽 값을 평가한다.
                // 반대로 하면 왼쪽 안의 부수효과가 오른쪽보다 늦게 일어난다. jQuery 가
                // 정확히 그걸 쓴다: `(b = se.selectors = {…}).pseudos.nth = b.pseudos.eq`
                // — 오른쪽을 먼저 보면 b 는 아직 undefined 라 통째로 죽었다.
                if let Expr::Member { obj, prop, computed } = &**target {
                    if !matches!(&**obj, Expr::Super) {
                        let recv = self.eval(obj, env)?;
                        let key = self.member_key(prop, *computed, env)?;
                        let rhs = self.eval(value, env)?;
                        let new = match op {
                            AssignOp::Set => rhs,
                            compound => {
                                let old = self.member_get(&recv, &key)?;
                                let bin = compound_binop(compound);
                                self.binary(bin, old, rhs)?
                            }
                        };
                        self.member_assign(recv, key, new.clone())?;
                        return Ok(new);
                    }
                }
                let rhs = self.eval(value, env)?;
                let new = match op {
                    AssignOp::Set => rhs,
                    compound => {
                        let old = self.eval(target, env)?;
                        let bin = match compound {
                            AssignOp::Add => BinOp::Add,
                            AssignOp::Sub => BinOp::Sub,
                            AssignOp::Mul => BinOp::Mul,
                            AssignOp::Div => BinOp::Div,
                            AssignOp::Mod => BinOp::Mod,
                            AssignOp::Pow => BinOp::Pow,
                            AssignOp::BitAnd => BinOp::BitAnd,
                            AssignOp::BitOr => BinOp::BitOr,
                            AssignOp::BitXor => BinOp::BitXor,
                            AssignOp::Shl => BinOp::Shl,
                            AssignOp::Shr => BinOp::Shr,
                            AssignOp::UShr => BinOp::UShr,
                            _ => BinOp::Add, // Set/And/Or 는 위에서 처리됨
                        };
                        self.binary(bin, old, rhs)?
                    }
                };
                // §13.15.2 NamedEvaluation: `a = function(){}` 처럼 **구문상 익명 함수**를
                // 이름 있는 참조에 대입하면 그 함수가 그 이름을 갖는다.
                // (`a = b` 처럼 식별자를 대입하는 건 해당 없음 — 그래서 표현식 모양을 본다.)
                if matches!(op, AssignOp::Set) {
                    if let Expr::Ident(n) = &**target {
                        if is_anonymous_fn_expr(value) {
                            Self::set_fn_name(&new, n);
                        }
                    }
                }
                self.assign_to(target, new.clone(), env)?;
                Ok(new)
            }
            Expr::Member { obj, prop, computed } if matches!(&**obj, Expr::Super) => {
                // super.x (호출이 아닌 **속성 읽기**). 부모의 게터가 있으면 현재 this 로
                // 실행하고, 없으면 부모 메서드/프로토타입 프로퍼티를 준다.
                // 예전엔 Expr::Super 가 undefined 로 평가돼 super.x 읽기가 통째로 터졌다.
                let key = self.member_key(prop, *computed, env)?;
                let this = env_get(env, "this").unwrap_or(Value::Undefined);
                let Some(sc) = env_get(env, "\u{0}superclass__") else {
                    return Err("super.x 는 파생 클래스에서만".to_string());
                };
                match sc {
                    Value::Class(p) => {
                        if let Some(g) = p.find_getter(&key) {
                            return self.call_value(Value::Fn(g), Some(this), vec![]);
                        }
                        if let Some(m) = p.find_method(&key) {
                            return Ok(Value::Fn(m));
                        }
                        Ok(Value::Undefined)
                    }
                    other => {
                        let proto = self.member_get(&other, "prototype")?;
                        self.member_get(&proto, &key)
                    }
                }
            }
            Expr::Member { obj, prop, computed } => {
                let recv = self.eval(obj, env)?;
                let key = self.member_key(prop, *computed, env)?;
                if matches!(recv, Value::Undefined | Value::Null) {
                    if self.lenient {
                        *self.lenient_hits.entry(format!(".{}", key)).or_default() += 1;
                        return Ok(Value::Undefined);
                    }
                    let m = format!(
                        "{}.{} — {} 이(가) {} (읽을 수 없음)",
                        obj_hint(obj),
                        key,
                        obj_hint(obj),
                        to_display(&recv)
                    );
                    return Err(self.throw_error("TypeError", m));
                }
                self.member_get(&recv, &key)
            }
            Expr::Nullish { left, right } => {
                let l = self.eval(left, env)?;
                if matches!(l, Value::Undefined | Value::Null) {
                    self.eval(right, env)
                } else {
                    Ok(l)
                }
            }
            Expr::OptMember { obj, prop, computed } => {
                let recv = self.eval(obj, env)?;
                if matches!(recv, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                let key = self.member_key(prop, *computed, env)?;
                self.member_get(&recv, &key)
            }
            // obj.m?.(args) — 함수가 없으면 단락, 있으면 **평범한 메서드 호출**이다.
            // 즉 this 는 obj 다 (표준 §13.3.6.1: OptionalCall 도 참조의 base 를 this 로 쓴다).
            // 예전엔 수신자를 버려서 el.getAttribute?.('src') 같은 코드가
            // "요소 메서드가 아니다" 로 죽었다 (tailwindcss.com 이 그렇다).
            Expr::OptCall { callee, args } => {
                // 수신자를 살리려면 callee 가 멤버식일 때 base 를 따로 평가해야 한다
                let (f, recv) = match &**callee {
                    Expr::Member { obj, prop, computed } => {
                        let base = self.eval(obj, env)?;
                        let key = self.member_key(prop, *computed, env)?;
                        let f = self.member_get(&base, &key)?;
                        (f, Some(base))
                    }
                    Expr::OptMember { obj, prop, computed } => {
                        let base = self.eval(obj, env)?;
                        if matches!(base, Value::Undefined | Value::Null) {
                            return Ok(Value::Undefined);
                        }
                        let key = self.member_key(prop, *computed, env)?;
                        let f = self.member_get(&base, &key)?;
                        (f, Some(base))
                    }
                    other => (self.eval(other, env)?, None),
                };
                if matches!(f, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                let mut arg_vals = Vec::new();
                arg_vals.extend(self.eval_args(args, env)?);
                self.call_value(f, recv, arg_vals)
            }
            Expr::Call { callee, args } => {
                // 호출 스택 프레임 (오류 위치 보고용). 호출식에서 이름을 뽑아 쌓는다.
                self.js_stack.push(callee_label(callee));
                if self.js_stack.len() > 400 {
                    self.js_stack.pop();
                    // 표준: 스택 초과는 RangeError "Maximum call stack size exceeded".
                    return Err(self.throw_error("RangeError", "Maximum call stack size exceeded"));
                }
                let r = self.eval_call(callee, args, env);
                if r.is_err() && self.err_stack.is_none() {
                    self.err_stack = Some(self.js_stack.clone()); // 던진 시점 스냅샷
                }
                self.js_stack.pop();
                r
            }
        }
    }

    fn member_key(&mut self, prop: &Expr, computed: bool, env: &EnvRef) -> Result<String, String> {
        if computed {
            let v = self.eval(prop, env)?;
            // §13.3.3 ToPropertyKey: 객체 키는 toString/@@toPrimitive 호출, Symbol 은 내부키.
            // 예전엔 key_of 라 o[{toString(){return 'x'}}] 가 toString 을 안 불렀다.
            self.to_property_key(v)
        } else if let Expr::Str(s) = prop {
            Ok(s.clone())
        } else {
            Err("잘못된 멤버 접근".to_string())
        }
    }

    // 대상에 own 프로퍼티 설정 (Object.assign 의 대상 쓰기, super() 의 this 채우기).
    // 무결성(freeze/seal)을 존중하고, 접근자(setter)가 있으면 setter 를 호출한다.
    // §10.4.2.4 ArraySetLength (근사): value 를 ToNumber(valueOf/toString 관측,
    // Symbol/BigInt TypeError) 후 ToUint32 와 다르면(음수/소수/2^32↑) RangeError,
    // 같으면 items 를 resize(축소는 truncate + 구멍 정리, 확장은 구멍). arr.length=
    // 대입과 defineProperty(arr,"length",{value}) 양쪽에서 재사용한다.
    pub(super) fn array_set_length(
        &mut self,
        a: &Rc<ArrayObj>,
        value: Value,
    ) -> Result<(), String> {
        let num = self.to_number_value(&value)?;
        let u = if num.is_finite() {
            num.trunc().rem_euclid(4294967296.0)
        } else {
            0.0
        };
        if u != num {
            return Err(self.throw_error("RangeError", "Invalid array length"));
        }
        let mut n = u as usize;
        if n > MAX_DENSE_ARRAY {
            a.set_prop("\u{0}sparse_len".to_string(), Value::Num(num));
        } else {
            let old_len = a.borrow().len();
            // §10.4.2.4: 축소 시 삭제될 인덱스 중 non-configurable 이 있으면 그 위로만
            // 줄인다(그 요소는 삭제 불가라 유지). index_attrs 빈 배열은 영향 없음.
            if n < old_len && a.has_index_attrs() {
                for i in (n..old_len).rev() {
                    if matches!(a.index_attr(i), Some(at) if at & ATTR_CONFIGURABLE == 0) {
                        n = i + 1;
                        break;
                    }
                }
            }
            a.borrow_mut().resize(n, Value::Undefined);
            if n > old_len {
                for h in old_len..n {
                    a.mark_hole(h);
                }
            } else if n < old_len && a.has_holes() {
                for h in n..old_len {
                    a.fill_hole(h);
                }
            }
        }
        Ok(())
    }

    // 프로퍼티를 설정한다. 성공 여부를 돌려준다(§10.1.9 [[Set]]) — Object.assign 등이
    // Throw=true 로 쓰므로 실패 시 호출부가 TypeError 를 던진다. 실패 조건: frozen,
    // non-writable 데이터 프로퍼티, getter 만 있는 접근자, non-extensible 에 새 키.
    pub(super) fn set_own_property(&mut self, target: &Value, k: String, v: Value) -> bool {
        if self.is_frozen_val(target) {
            return false;
        }
        match target {
            Value::Obj(m) => {
                // setter 가 있으면 호출 (own → 프로토타입). getter 만 있으면 실패.
                if let Some(acc) = self.find_accessor(m, &k) {
                    if let Some(st) = acc.set.clone() {
                        let _ = self.call_value(st, Some(target.clone()), vec![v]);
                        return true;
                    }
                    return false; // 접근자에 setter 없음 → 설정 불가
                }
                if m.borrow().contains_key(&k) {
                    // 기존 데이터 프로퍼티가 non-writable 이면 실패
                    if prop_attrs(&m.borrow(), &k) & ATTR_WRITABLE == 0 {
                        return false;
                    }
                } else if self.is_nonextensible_val(target) {
                    return false;
                }
                m.borrow_mut().insert(k, v);
                true
            }
            Value::Arr(a) => {
                if let Ok(i) = k.parse::<usize>() {
                    if i >= a.borrow().len() && self.is_nonextensible_val(target) {
                        return false;
                    }
                    // length 가 non-writable 이면 길이를 넘기는 인덱스 추가 불가 (§10.4.2.1).
                    if i >= a.borrow().len() && !a.length_writable() {
                        return false;
                    }
                    // non-writable 로 정의된 인덱스는 덮어쓰기 불가 (§10.4.2).
                    if matches!(a.index_attr(i), Some(at) if at & ATTR_WRITABLE == 0) {
                        return false;
                    }
                    if i >= MAX_DENSE_ARRAY {
                        return false; // 방어: 초거대 인덱스는 무시 (희박 배열 미구현)
                    }
                    let old_len = a.borrow().len();
                    {
                        let mut items = a.borrow_mut();
                        if i >= items.len() {
                            items.resize(i + 1, Value::Undefined);
                        }
                        items[i] = v;
                    }
                    if i > old_len {
                        for h in old_len..i {
                            a.mark_hole(h);
                        }
                    }
                    a.fill_hole(i);
                    true
                } else {
                    if a.get_prop(&k).is_none() && self.is_nonextensible_val(target) {
                        return false;
                    }
                    a.set_prop(k, v);
                    true
                }
            }
            Value::Instance(inst) => {
                // set 접근자가 있으면 호출한다 (표준). 예전엔 파서가 setter 를 버려서
                // 대입이 그냥 필드에 꽂혔고, 검증/변환 로직이 통째로 우회됐다.
                if let Some(setter) = inst.class.find_setter(&k) {
                    let _ = self.call_value(Value::Fn(setter), Some(target.clone()), vec![v]);
                    return true;
                }
                let k = field_key(&k, self.priv_id);
                if !inst.fields.borrow().contains_key(&k) && self.is_nonextensible_val(target) {
                    return false;
                }
                inst.fields.borrow_mut().insert(k, v);
                true
            }
            Value::Fn(f) => {
                if !f.props.borrow().contains_key(&k) && self.is_nonextensible_val(target) {
                    return false;
                }
                f.props.borrow_mut().insert(k, v);
                true
            }
            Value::Class(c) => {
                c.statics.borrow_mut().insert(k, v);
                true
            }
            _ => false,
        }
    }

    // 무결성 비트 조회/설정 (freeze/seal/preventExtensions). 모든 객체 종류 공통.
    pub(super) fn integrity_bits(&self, v: &Value) -> u8 {
        integrity_ptr(v)
            .and_then(|p| self.integrity.get(&p))
            .map(|(_, b)| *b)
            .unwrap_or(0)
    }

    pub(super) fn set_integrity(&mut self, v: &Value, bit: u8) {
        if let Some(p) = integrity_ptr(v) {
            let e = self.integrity.entry(p).or_insert((v.clone(), 0));
            e.1 |= bit;
        }
    }

    /// Proxy 가 취소(revoke)됐으면 TypeError. 모든 프록시 내부 메서드 시작에서 부른다
    /// (§10.5.* 의 "If handler is null, throw a TypeError"). Ok(()) 면 계속 진행.
    pub(super) fn proxy_revoked_guard(&mut self, p: &Rc<(Value, Value)>) -> Result<(), String> {
        if self.revoked_proxies.contains(&(Rc::as_ptr(p) as *const () as usize)) {
            return Err(self.throw_error(
                "TypeError",
                "Cannot perform 'get/set/...' on a proxy that has been revoked",
            ));
        }
        Ok(())
    }

    pub(super) fn is_frozen_val(&self, v: &Value) -> bool {
        self.integrity_bits(v) & INTEG_FROZEN != 0
    }

    // 새 프로퍼티 추가가 막혔는가 (frozen/sealed/preventExtensions 중 하나)
    pub(super) fn is_nonextensible_val(&self, v: &Value) -> bool {
        self.integrity_bits(v) & (INTEG_FROZEN | INTEG_SEALED | INTEG_NONEXT) != 0
    }

    // 접근자 프로퍼티 탐색: own → __proto__ 체인. 대입 시 setter 를 찾는 데 쓴다.
    fn find_accessor(&self, map: &Rc<RefCell<ObjMap>>, key: &str) -> Option<Rc<AccessorPair>> {
        let mut cur = map.clone();
        for _ in 0..100 {
            let (found, proto) = {
                let b = cur.borrow();
                (b.get(key).cloned(), b.get("__proto__").cloned())
            };
            match found {
                Some(Value::Accessor(a)) => return Some(a),
                // 값 프로퍼티가 먼저 발견되면 접근자가 아니다(가리워짐)
                Some(_) => return None,
                None => {}
            }
            match proto {
                Some(Value::Obj(p)) => cur = p,
                _ => return None,
            }
        }
        None
    }

    // 잘 알려진 심볼(Symbol.iterator 등) — 고정 key 로 배열/제너레이터 반복자와 연결.
    fn well_known_symbol(key: &str, desc: &str) -> Value {
        Value::Symbol(Rc::new(SymbolData { key: key.to_string(), desc: Some(desc.to_string()) }))
    }

    // getComputedStyle(el) → 계산 스타일 뷰(el 이 요소면). 요소 아니면 빈 뷰.
    // 측정 API 진입점에서 호출한다. DOM 이 지난 레이아웃 이후 바뀌었으면 스타일·레이아웃을
    // 다시 돌려 측정 맵을 최신화한다 (forced synchronous layout).
    // 예전엔 스크립트가 첫 레이아웃보다 먼저 전부 실행돼서, 파싱 중이나 load 에서 잰 값이
    // 항상 0/빈 문자열이었다. 측정 후 배치하는 코드가 전부 조용히 어긋났다.
    pub(super) fn ensure_layout(&mut self) {
        crate::window::flush_layout(self);
    }

    // DOM 변형 기록이 쌓여 있으면 배달 마이크로태스크를 한 번 예약한다.
    // 표준도 동기 콜백이 아니라 마이크로태스크로 모아서 전달한다.
    pub(super) fn schedule_mutation_delivery(&mut self) {
        if self.mutation_scheduled {
            return;
        }
        let has = match self.dom {
            Some(p) => !unsafe { &*p }.records.is_empty(),
            None => false,
        };
        if !has {
            return;
        }
        // 프렐류드가 설치한 배달 함수. 옵저버를 안 쓰는 페이지면 없을 수도 있다.
        let Some(notify) = env_get(&self.global, "__kMutationNotify") else { return };
        if is_callable(&notify) {
            self.mutation_scheduled = true;
            self.microtasks.push_back((notify, Value::Undefined, Value::Undefined, false));
        }
    }

    // ── ES 모듈 평가 ──
    //
    // 표준의 모듈 의미론을 따른다:
    //  - 각 모듈은 자기 스코프에서 한 번만 평가된다 (URL 로 식별).
    //  - 의존 모듈이 먼저 평가된다.
    //  - export 는 **살아있는 바인딩**이다 — 네임스페이스의 프로퍼티는 모듈 스코프의
    //    현재 값을 읽는 게터다. 값 스냅샷으로 흉내내면 나중에 바뀌는 값이 안 보인다.
    //  - 순환 의존은 부분 채워진 네임스페이스를 공유해 무한 재귀를 피한다.
    pub fn run_module(&mut self, url: &str) -> Result<Value, String> {
        let depth = self.js_stack.len();
        let r = self.run_module_inner(url);
        // 모듈 평가 중 난 오류에도 호출 스택을 붙인다.
        let r = r.map_err(|e| self.with_stack(e));
        self.js_stack.truncate(depth);
        r
    }

    fn run_module_inner(&mut self, url: &str) -> Result<Value, String> {
        if let Some(ns) = self.module_namespaces.get(url) {
            return Ok(ns.clone());
        }
        let Some(src) = self.module_sources.get(url).cloned() else {
            return Err(format!("모듈을 못 찾음: {}", url));
        };
        let body = parse(&src).map_err(|e| format!("모듈 파싱 실패 {}: {}", url, e))?;

        // 네임스페이스를 먼저 등록 (순환 의존 대비)
        let ns_map: Rc<RefCell<ObjMap>> = Rc::new(RefCell::new(ObjMap::new()));
        let ns = Value::Obj(ns_map.clone());
        self.module_namespaces.insert(url.to_string(), ns.clone());

        let env = Env::new(Some(self.global.clone()));

        // 1) import 먼저: 의존 모듈을 평가하고 이름을 이 스코프에 바인딩
        for st in &body {
            let Stmt::Import { specs, source } = st else { continue };
            let dep = self.resolve_module(url, source);
            let dep_ns = self.run_module_inner(&dep)?;
            // 네임스페이스의 프로퍼티를 **호출하지 않고** 그대로 가져온다.
            // 접근자면 접근자째로 스코프에 넣어 살아있는 바인딩이 된다(순환 의존 대비).
            let raw = |ns: &Value, key: &str| -> Option<Value> {
                match ns {
                    Value::Obj(m) => m.borrow().get(key).cloned(),
                    _ => None,
                }
            };
            for sp in specs {
                match sp {
                    crate::js::ast::ImportSpec::Default(local) => {
                        let v = raw(&dep_ns, "default").unwrap_or(Value::Undefined);
                        env_declare(&env, local, v);
                    }
                    crate::js::ast::ImportSpec::Named(imported, local) => {
                        let v = raw(&dep_ns, imported).unwrap_or(Value::Undefined);
                        env_declare(&env, local, v);
                    }
                    crate::js::ast::ImportSpec::Namespace(local) => {
                        env_declare(&env, local, dep_ns.clone());
                    }
                }
            }
        }

        // var 호이스팅 — 모듈도 스크립트와 같다. 이게 없으면 `var a, le, ue = …` 처럼
        // 초기화 없는 선언자가 스코프에 안 들어가고(var 는 호이스팅에 의존한다),
        // 그 이름을 읽는 순간 "정의되지 않음" 으로 죽는다. (vue 런타임이 정확히 이 모양이다)
        hoist_vars(&body, &env);

        // 함수 선언 호이스팅 (블록 실행이 하던 일 — 모듈은 문장을 직접 돌리므로 여기서).
        // 이게 없으면 `export function f(){}` 가 스코프에 안 들어가서, 그 이름을 읽는
        // 게터가 "f 은(는) 정의되지 않음" 으로 죽는다.
        for st in &body {
            let decl = match st {
                Stmt::FuncDecl { .. } => Some(st),
                Stmt::ExportDecl(inner) | Stmt::ExportDefault(inner) => {
                    matches!(&**inner, Stmt::FuncDecl { .. }).then(|| &**inner)
                }
                _ => None,
            };
            if let Some(Stmt::FuncDecl { name, params, body: fb, is_generator, is_async, source, prologue_len }) = decl {
                let f = Value::Fn(Rc::new(JsFn {
                    priv_id: std::cell::Cell::new(0),
                    name: RefCell::new(name.clone()),
                    params: params.clone(),
                    body: fb.clone(),
                    param_prologue_len: *prologue_len,
                    env: env.clone(),
                    is_arrow: false,
                    is_generator: *is_generator,
                    is_async: *is_async,
                    is_method: false,
                    this: None,
                    super_class: None,
                    props: RefCell::new(ObjMap::new()),
                    source: source.clone(),
                }));
                env_declare(&env, name, f);
            }
        }

        // 2) 내보낼 이름을 **본문 실행 전에** 살아있는 바인딩(게터)으로 등록한다.
        // ESM 네임스페이스는 모듈 환경의 살아있는 뷰다 (표준 §10.4.6). 예전엔 본문이
        // 다 끝난 뒤에 채워서, **자기 자신을 import 하는 모듈**(rspack/webpack 청크가
        // 실제로 그렇게 한다: import * as a from "./self.js"; e.C(a))이 본문 도중
        // 자기 네임스페이스를 읽으면 통째로 비어 있었다.
        let mut exported: Vec<(String, String)> = Vec::new(); // (로컬명, 내보낸 이름)
        for st in &body {
            match st {
                Stmt::ExportDecl(inner) => {
                    for n in declared_names(inner) {
                        exported.push((n.clone(), n));
                    }
                }
                Stmt::ExportNamed { specs, source: None } => {
                    for (local, name) in specs {
                        exported.push((local.clone(), name.clone()));
                    }
                }
                _ => {}
            }
        }
        for (local, exported_name) in &exported {
            let getter = Value::Fn(Rc::new(JsFn {
                priv_id: std::cell::Cell::new(0),
                name: RefCell::new(format!("get {}", exported_name)),
                params: vec![],
                body: vec![Stmt::Return(Some(Expr::Ident(local.clone())))],
                param_prologue_len: 0,
                env: env.clone(),
                is_arrow: false,
                is_generator: false,
                is_async: false,
                is_method: true,
                this: None,
                super_class: None,
                props: RefCell::new(ObjMap::new()),
                source: None,
            }));
            ns_map
                .borrow_mut()
                .insert(exported_name.clone(), Value::Accessor(AccessorPair::getter(getter)));
        }

        // 3) 본문 실행 (import 는 이미 처리)
        for (idx, st) in body.iter().enumerate() {
            let _ = idx;
            match st {
                Stmt::Import { .. } => {}
                Stmt::ExportDecl(inner) => {
                    self.exec_stmt(inner, &env)?; // 이름은 위에서 이미 게터로 등록됨
                }
                Stmt::ExportDefault(inner) => {
                    match &**inner {
                        Stmt::Expr(e) => {
                            let v = self.eval(e, &env)?;
                            ns_map.borrow_mut().insert("default".to_string(), v);
                        }
                        other => {
                            self.exec_stmt(other, &env)?;
                            // 이름 있는 함수/클래스면 그 이름의 값을 default 로
                            if let Some(n) = declared_names(other).first() {
                                if let Some(v) = env_get(&env, n) {
                                    ns_map.borrow_mut().insert("default".to_string(), v);
                                }
                            }
                        }
                    }
                }
                Stmt::ExportNamed { specs, source } => match source {
                    // export { a as b } from 'm' / export * as ns from 'm'
                    Some(src_mod) => {
                        let dep = self.resolve_module(url, src_mod);
                        let dep_ns = self.run_module(&dep)?;
                        for (local, exported_name) in specs {
                            let v = if local == "*" {
                                dep_ns.clone()
                            } else {
                                self.member_get(&dep_ns, local)?
                            };
                            ns_map.borrow_mut().insert(exported_name.clone(), v);
                        }
                    }
                    None => {} // export { a as b } — 위에서 이미 게터로 등록됨
                },
                Stmt::ExportAll { source } => {
                    let dep = self.resolve_module(url, source);
                    let dep_ns = self.run_module(&dep)?;
                    if let Value::Obj(m) = &dep_ns {
                        let entries: Vec<(String, Value)> = m
                            .borrow()
                            .iter()
                            .filter(|(k, _)| k.as_str() != "default")
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect();
                        for (k, v) in entries {
                            ns_map.borrow_mut().insert(k, v);
                        }
                    }
                }
                other => {
                    if let Err(e) = self.exec_stmt(other, &env) {
                        if std::env::var("KESTREL_MODULE_DEBUG").is_ok() {
                            let dump = format!("{:?}", other);
                            eprintln!(
                                "[module] {} 문장 #{} 오류: {}\n  AST: {}",
                                url,
                                idx,
                                e,
                                &dump[..dump.len().min(400)]
                            );
                        }
                        return Err(e);
                    }
                }
            }
        }

        self.drain_microtasks();
        Ok(ns)
    }

    // 모듈 명세자 → 절대 URL (importer 기준). 베어 명세자('react')는 지원하지 않는다 —
    // 임포트 맵/노드 해석이 필요하고, 조용히 틀린 URL 을 만들면 더 나쁘다.
    // 임포트 맵으로 베어 명세자를 해석한다. 맵에 없거나 상대 경로면 None.
    pub fn map_specifier(&self, spec: &str) -> Option<String> {
        if spec.starts_with("./") || spec.starts_with("../") || spec.starts_with('/') {
            return None;
        }
        for (key, target) in &self.import_map {
            if key.ends_with('/') {
                if let Some(rest) = spec.strip_prefix(key.as_str()) {
                    return Some(format!("{}{}", target, rest));
                }
            } else if spec == key {
                return Some(target.clone());
            }
        }
        None
    }

    fn resolve_module(&self, importer: &str, spec: &str) -> String {
        if spec.starts_with("http://") || spec.starts_with("https://") {
            return spec.to_string();
        }
        // 임포트 맵: 베어 명세자("react", "lib/x.js")를 URL 로 해석 (HTML §4.12.5)
        if let Some(mapped) = self.map_specifier(spec) {
            if mapped.starts_with("http") {
                return mapped;
            }
            return match crate::url::Url::parse(importer).ok().and_then(|u| u.join(&mapped)) {
                Some(u) => u.as_string(),
                None => mapped,
            };
        }
        match crate::url::Url::parse(importer).ok().and_then(|u| u.join(spec)) {
            Some(u) => u.as_string(),
            None => spec.to_string(),
        }
    }

    // 상대 URL → 문서 URL 기준 절대 URL. 이미 절대면 그대로.
    pub(super) fn absolute_url(&self, raw: &str) -> String {
        let Some(base) = &self.base_url else { return raw.to_string() };
        match crate::url::Url::parse(base).ok().and_then(|u| u.join(raw)) {
            Some(u) => u.as_string(),
            None => raw.to_string(),
        }
    }

    // pushState/replaceState 가 부른다. 상대 URL 을 현재 문서 URL 에 결합해
    // location 의 href/pathname/search/hash 를 갱신한다 (네비게이션은 하지 않는다).
    pub(super) fn update_location(&mut self, url: &str) {
        let base = self.base_url.clone().unwrap_or_default();
        let Some(next) = crate::url::Url::parse(&base).ok().and_then(|u| u.join(url)) else {
            return;
        };
        let full = next.as_string();
        let (path_q, hash) = match full.split_once('#') {
            Some((p, h)) => (p.to_string(), format!("#{}", h)),
            None => (full.clone(), String::new()),
        };
        let (pathname, search) = match next.path.split_once('?') {
            Some((p, q)) => (p.to_string(), format!("?{}", q)),
            None => (next.path.clone(), String::new()),
        };
        let _ = path_q;
        self.base_url = Some(full.clone());
        let Some(Value::Obj(loc)) = env_get(&self.global, "location") else { return };
        let mut m = loc.borrow_mut();
        m.insert("href".to_string(), Value::Str(full));
        m.insert("pathname".to_string(), Value::Str(pathname));
        m.insert("search".to_string(), Value::Str(search));
        m.insert("hash".to_string(), Value::Str(hash));
    }

    // 스크롤 위치 갱신 + window 프로퍼티(scrollY/pageYOffset 등) 동기화.
    // 읽는 쪽이 늘 같은 값을 보게 한다 (예전엔 scrollY 가 0 으로 고정돼 있었다).
    pub(super) fn set_scroll(&mut self, x: f32, y: f32) {
        self.scroll_x = x.max(0.0);
        self.scroll_y = y.max(0.0);
        let mut w = self.window_obj.borrow_mut();
        for k in ["scrollX", "pageXOffset"] {
            w.insert(k.to_string(), Value::Num(self.scroll_x as f64));
        }
        for k in ["scrollY", "pageYOffset"] {
            w.insert(k.to_string(), Value::Num(self.scroll_y as f64));
        }
    }

    // 현재 뷰포트(px). 강제 레이아웃 컨텍스트가 있으면 실제 값, 없으면 기본 창 크기.
    pub(super) fn viewport(&self) -> (f32, f32) {
        match self.layout_ctx {
            Some(ctx) => (ctx.vw, ctx.vh),
            None => (1000.0, 800.0),
        }
    }

    pub(super) fn get_computed_style(&mut self, arg: Option<&Value>) -> Value {
        self.ensure_layout();
        match arg {
            Some(Value::Dom(id)) => Value::ComputedStyle(*id),
            // 요소가 아니면 어떤 노드와도 겹치지 않는 센티널 → 빈 뷰.
            _ => Value::ComputedStyle(usize::MAX),
        }
    }

    // 전역 생성자(ctor)의 prototype 에서 메서드를 찾는다 (폴리필 조회용).
    // 예: proto_method("Array", "flatMap") → Array.prototype.flatMap.
    // 내장/바운드 함수의 함수 공통 멤버. name/length 는 own(§17), call/apply/bind 와
    // Object.prototype/Function.prototype 상속 메서드(hasOwnProperty/toString 등)를
    // 한곳에서 제공한다. 각 생성자별 member_get 분기의 fallback 도 이걸 쓴다 —
    // 예전엔 분기마다 _ => Undefined 라 Array.name/String.length 등이 사라졌다.
    // 내장 함수의 name/length 가 delete 로 지워졌는지 (verifyProperty 의 configurable 검사).
    pub(super) fn native_prop_deleted(&self, recv: &Value, key: &str) -> bool {
        if let Value::Native(n) = recv {
            return self
                .native_props
                .get(n)
                .map(|m| m.contains_key(&format!("\u{0}del:{}", key)))
                .unwrap_or(false);
        }
        false
    }

    // 내장 생성자의 own 문자열 프로퍼티 키(정적 메서드/상수 + prototype). name/length 는
    // 모든 함수 공통이라 호출부에서 따로 더한다. 값의 단일 소스는 member_get 이다 —
    // 이 목록이 member_get 과 어긋나면 self-test(native_ctor_reflection)가 잡는다.
    // Object/Array 는 실제 네임스페이스 Obj(object_ns/array_ns)에 위임해 드리프트가 없다.
    fn native_ctor_own_keys(&self, n: &Native) -> Option<Vec<String>> {
        use Native::*;
        let statics: &[&str] = match n {
            ObjectCtor => return Some(self.ns_own_keys(&self.object_ns)),
            ArrayCtor => return Some(self.ns_own_keys(&self.array_ns)),
            NumberCtor => &[
                "isInteger", "isSafeInteger", "isFinite", "isNaN", "parseInt", "parseFloat",
                "MAX_SAFE_INTEGER", "MIN_SAFE_INTEGER", "MAX_VALUE", "MIN_VALUE", "EPSILON",
                "POSITIVE_INFINITY", "NEGATIVE_INFINITY", "NaN", "prototype",
            ],
            BooleanCtor => &["prototype"],
            StringCtor => &["fromCharCode", "fromCodePoint", "raw", "prototype"],
            DateCtor => &["now", "parse", "UTC", "prototype"],
            RegExpCtor => &["escape", "prototype"],
            MapCtor => &["groupBy", "prototype"],
            SetCtor => &["prototype"],
            PromiseCtor => {
                &["resolve", "reject", "all", "race", "allSettled", "withResolvers", "prototype"]
            }
            SymbolCtor => &[
                "iterator", "asyncIterator", "toStringTag", "hasInstance", "toPrimitive", "match",
                "matchAll", "replace", "search", "split", "species", "isConcatSpreadable", "for",
                "keyFor", "prototype",
            ],
            ErrorCtor("Error") => return Some(vec!["prototype".to_string(), "isError".to_string()]),
            ErrorCtor(_) => &["prototype"],
            FunctionCtor => &["prototype"],
            _ => return None,
        };
        Some(statics.iter().map(|s| s.to_string()).collect())
    }

    // 네임스페이스 Obj 의 non-internal own 문자열 키.
    fn ns_own_keys(&self, ns: &Value) -> Vec<String> {
        if let Value::Obj(m) = ns {
            m.borrow().keys().filter(|k| !is_internal_key(k)).cloned().collect()
        } else {
            Vec::new()
        }
    }

    // 내장 생성자의 own 프로퍼티 서술자 (§17). 정적 메서드는 {w:true,e:false,c:true},
    // 상수(Number.MAX_VALUE 등)는 전부 false, prototype 은 {w:false,e:false,c:false}.
    // own 이 아니면 None. name/length 는 호출부가 따로 처리한다.
    pub(super) fn native_own_descriptor(
        &mut self,
        recv: &Value,
        key: &str,
    ) -> Result<Option<Value>, String> {
        let n = match recv {
            Value::Native(n) => *n,
            _ => return Ok(None),
        };
        // @@species 는 접근자 서술자 {get, undefined, e:false, c:true} (§).
        if key == "\u{0}@@species" && native_has_species(&n) {
            let mut d = ObjMap::new();
            d.insert("get".to_string(), Value::Native(Native::SpeciesGet));
            d.insert("set".to_string(), Value::Undefined);
            d.insert("enumerable".to_string(), Value::Bool(false));
            d.insert("configurable".to_string(), Value::Bool(true));
            return Ok(Some(Value::Obj(Rc::new(RefCell::new(d)))));
        }
        let Some(keys) = self.native_ctor_own_keys(&n) else {
            return Ok(None);
        };
        if !keys.iter().any(|k| k == key) {
            return Ok(None);
        }
        let val = self.member_get(recv, key)?;
        if matches!(val, Value::Undefined) {
            return Ok(None);
        }
        let (writable, configurable) = if key == "prototype" {
            (false, false)
        } else if matches!(val, Value::Native(_) | Value::Fn(_) | Value::Bound(_) | Value::Class(_))
        {
            (true, true) // 정적 메서드
        } else {
            (false, false) // 상수 값
        };
        let mut d = ObjMap::new();
        d.insert("value".to_string(), val);
        d.insert("writable".to_string(), Value::Bool(writable));
        d.insert("enumerable".to_string(), Value::Bool(false));
        d.insert("configurable".to_string(), Value::Bool(configurable));
        Ok(Some(Value::Obj(Rc::new(RefCell::new(d)))))
    }

    // 내장 생성자의 own 정적 프로퍼티가 writable:false 인가 (상수/prototype). 재대입 거부에 쓴다.
    // 정적 메서드는 writable:true 라 폴리필 오버라이드가 가능하다.
    fn native_static_readonly(&mut self, recv: &Value, key: &str) -> bool {
        let n = match recv {
            Value::Native(n) => *n,
            _ => return false,
        };
        let Some(keys) = self.native_ctor_own_keys(&n) else {
            return false;
        };
        if !keys.iter().any(|k| k == key) {
            return false;
        }
        if key == "prototype" {
            return true;
        }
        // 값이 함수/생성자면 정적 메서드(writable) — 아니면 상수(read-only).
        match self.member_get(recv, key) {
            Ok(v) => !matches!(
                v,
                Value::Native(_) | Value::Fn(_) | Value::Bound(_) | Value::Class(_)
            ),
            Err(_) => false,
        }
    }

    fn native_fn_member(&self, recv: &Value, key: &str) -> Option<Value> {
        // delete 된 name/length 는 없는 것으로 (configurable:true).
        if matches!(key, "name" | "length") && self.native_prop_deleted(recv, key) {
            return None;
        }
        Some(match key {
            "name" => Value::Str(self.native_fn_name(recv)),
            "length" => Value::Num(self.native_fn_length(recv)),
            "call" => Value::Native(Native::FnCall),
            "apply" => Value::Native(Native::FnApply),
            "bind" => Value::Native(Native::FnBind),
            // 내장 함수도 toString 을 가진다 — jQuery 서두가 fnToString.call(Object) 사용.
            "toString" => Value::Native(Native::FnToString),
            // Object.prototype 상속 메서드 — 함수도 객체이므로 상속한다.
            "hasOwnProperty" => Value::Native(Native::HasOwnProperty),
            "isPrototypeOf" => Value::Native(Native::ObjectIsPrototypeOf),
            "propertyIsEnumerable" => Value::Native(Native::PropertyIsEnumerable),
            "valueOf" => Value::Native(Native::ValueOfSelf),
            _ => return None,
        })
    }

    // thisBooleanValue/thisNumberValue/thisStringValue (§20.3.3.3/§21.1.3.7/§22.1.3.32).
    // 수신자가 해당 종류의 원시값이거나 그 종류의 원시 래퍼 객체면 원시값을 돌려주고,
    // 아니면 TypeError. valueOf/toString 이 "not generic" 인 이유가 이 brand 검사다.
    pub(super) fn this_prim_value(
        &mut self,
        this: &Value,
        brand: PrimBrand,
    ) -> Result<Value, String> {
        fn brand_of(brand: PrimBrand, v: &Value) -> bool {
            matches!(
                (brand, v),
                (PrimBrand::Boolean, Value::Bool(_))
                    | (PrimBrand::Number, Value::Num(_))
                    | (PrimBrand::String, Value::Str(_))
                    | (PrimBrand::Symbol, Value::Symbol(_))
            )
        }
        if brand_of(brand, this) {
            return Ok(this.clone());
        }
        if let Some(prim) = wrapper_primitive(this) {
            if brand_of(brand, &prim) {
                return Ok(prim);
            }
        }
        let tn = match brand {
            PrimBrand::Boolean => "Boolean",
            PrimBrand::Number => "Number",
            PrimBrand::String => "String",
            PrimBrand::Symbol => "Symbol",
        };
        Err(self.throw_error(
            "TypeError",
            format!("{}.prototype method is not generic (incompatible receiver)", tn),
        ))
    }

    // 내장/바운드 함수의 name (§10.2.9 SetFunctionName, §17). Bound 는 "bound " 접두.
    fn native_fn_name(&self, v: &Value) -> String {
        match v {
            Value::Native(n) => natives::native_meta(n).map(|(nm, _)| nm.to_string()).unwrap_or_default(),
            Value::Bound(b) => format!("bound {}", self.fn_name_of(&b.0)),
            _ => String::new(),
        }
    }
    // 내장/바운드 함수의 length. Bound 는 max(0, target.length - 바운드된 인자 수) (§10.4.1.3).
    // extends 대상이 명백히 [[Construct]] 를 갖지 않는가 — arrow/제너레이터/async/메서드
    // 함수와 그런 것을 감싼 bound/proxy. Class/일반함수/Native/Obj 는 보수적으로 생성자로 본다
    // (Error/Array/Promise 등 확장을 깨지 않기 위해).
    fn is_non_constructor(v: &Value) -> bool {
        match v {
            Value::Fn(f) => f.is_arrow || f.is_generator || f.is_async || f.is_method,
            Value::Bound(b) => Self::is_non_constructor(&b.0),
            Value::Proxy(p) => Self::is_non_constructor(&p.0),
            _ => false,
        }
    }

    // ExpectedArgumentCount(§ FunctionDeclarationInstantiation): 첫 기본값/rest 파라미터
    // 앞까지의 형식 매개변수 수. 기본값은 프롤로그의 `if (p === undefined)` 로, rest 는
    // "...p" 로, 구조분해는 "__patN__" 로 desugar 되므로 params 와 프롤로그를 함께 본다.
    fn expected_arg_count(params: &[String], prologue: &[Stmt]) -> f64 {
        let mut count = 0usize;
        for p in params {
            if p.starts_with("...") {
                break; // rest 및 그 뒤는 세지 않는다
            }
            let has_default = prologue.iter().any(|s| {
                matches!(s, Stmt::If { cond: Expr::Binary { op: BinOp::EqEqEq, left, right }, .. }
                    if matches!(&**left, Expr::Ident(n) if n == p)
                        && matches!(&**right, Expr::Undefined))
            });
            if has_default {
                break; // 기본값 있는 파라미터 및 그 뒤는 세지 않는다
            }
            count += 1;
        }
        count as f64
    }

    // JsFn 의 ExpectedArgumentCount (프롤로그는 body 앞 param_prologue_len 개 문장)
    fn fn_expected_args(f: &JsFn) -> f64 {
        let plen = f.param_prologue_len.min(f.body.len());
        Self::expected_arg_count(&f.params, &f.body[..plen])
    }

    fn native_fn_length(&self, v: &Value) -> f64 {
        match v {
            Value::Native(n) => natives::native_meta(n).map(|(_, l)| l as f64).unwrap_or(0.0),
            Value::Bound(b) => {
                let target_len = self.fn_length_of(&b.0);
                (target_len - b.2.len() as f64).max(0.0)
            }
            _ => 0.0,
        }
    }
    // 임의 함수값의 name (Fn/Class/Native/Bound 공통).
    fn fn_name_of(&self, v: &Value) -> String {
        match v {
            Value::Fn(f) => f.name.borrow().clone(),
            Value::Class(c) => c.name.borrow().clone(),
            Value::Native(_) | Value::Bound(_) => self.native_fn_name(v),
            _ => String::new(),
        }
    }
    // 임의 함수값의 length (형식 매개변수 수).
    fn fn_length_of(&self, v: &Value) -> f64 {
        match v {
            // getOwnPropertyDescriptor(Fn,"length") 와 같은 셈법 유지.
            Value::Fn(f) => Self::fn_expected_args(f),
            Value::Native(_) | Value::Bound(_) => self.native_fn_length(v),
            _ => 0.0,
        }
    }

    fn proto_method(&self, ctor: &str, key: &str) -> Option<Value> {
        let ns = env_get(&self.global, ctor)?;
        let proto = match &ns {
            Value::Obj(m) => m.borrow().get("prototype").cloned(),
            // String 은 Native 생성자 — prototype 은 보관된 string_proto (폴리필이 여기 얹힘)
            Value::Native(Native::StringCtor) => Some(self.string_proto.clone()),
            // Object/Array 도 Native 생성자 — prototype 은 네임스페이스 맵에 있다.
            Value::Native(Native::ObjectCtor) => match &self.object_ns {
                Value::Obj(m) => m.borrow().get("prototype").cloned(),
                _ => None,
            },
            Value::Native(Native::ArrayCtor) => match &self.array_ns {
                Value::Obj(m) => m.borrow().get("prototype").cloned(),
                _ => None,
            },
            _ => None,
        }?;
        match proto {
            Value::Obj(m) => m.borrow().get(key).cloned(),
            _ => None,
        }
    }

    // __proto__ 링크를 따라 프로퍼티를 찾는다. getter 면 this=원 객체로 호출. 순환 방지.
    // HasProperty (§7.3.11): own + 프로토타입 체인. ToPropertyDescriptor 등이
    // 필드 존재 판정에 쓴다({value: undefined} 처럼 명시 undefined 와 부재를 구분).
    /// [[Prototype]] 체인(Obj/Fn 노드)에 key 가 있는지. HasProperty 의 상속 부분에 쓴다.
    /// 값이 아니라 존재만 보므로 getter 를 부르지 않아 &self 로 충분하다.
    fn value_chain_has(&self, start: &Value, key: &str) -> bool {
        let mut proto = start.clone();
        let mut depth = 0;
        loop {
            depth += 1;
            if depth > 100 {
                return false;
            }
            match proto {
                Value::Obj(p) => {
                    // 심볼 키("\0@@…")는 내부 마커와 달리 실제 own 프로퍼티다(예외 허용).
                    if (!is_internal_key(key) || is_symbol_key(key)) && p.borrow().contains_key(key) {
                        return true;
                    }
                    match p.borrow().get("__proto__").cloned() {
                        Some(n) => proto = n,
                        None => return false,
                    }
                }
                Value::Fn(f) => {
                    if f.props.borrow().contains_key(key) {
                        return true;
                    }
                    proto = f
                        .props
                        .borrow()
                        .get("__proto__")
                        .cloned()
                        .unwrap_or_else(|| self.fn_proto.clone());
                }
                // 체인 속 배열: 인덱스/length/Array.prototype 상속을 has_property(Arr)가 해석.
                Value::Arr(_) => return self.has_property(&proto, key),
                _ => return false,
            }
        }
    }

    // §23.1.3.1.1 IsConcatSpreadable(O): @@isConcatSpreadable 가 있으면 ToBoolean, 없으면
    // IsArray. Array.prototype.concat 이 인자를 펼칠지 결정한다.
    pub(super) fn is_concat_spreadable(&mut self, v: &Value) -> Result<bool, String> {
        if !is_object(v) {
            return Ok(false);
        }
        let flag = self.member_get(v, "\u{0}@@isConcatSpreadable")?;
        if !matches!(flag, Value::Undefined) {
            return Ok(to_bool(&flag));
        }
        Ok(matches!(v, Value::Arr(_)))
    }

    pub(super) fn has_property(&self, obj: &Value, key: &str) -> bool {
        match obj {
            Value::Obj(m) => {
                // 심볼 키("\0@@…")는 내부 마커와 달리 실제 own 프로퍼티다(예외 허용).
                if (!is_internal_key(key) || is_symbol_key(key)) && m.borrow().contains_key(key) {
                    return true;
                }
                // 프로토타입 체인(배열/함수 프로토 포함)을 value_chain_has 로 걷는다 —
                // 예전엔 Value::Obj 만 걸어 배열 프로토타입의 상속 인덱스를 놓쳤다.
                match m.borrow().get("__proto__").cloned() {
                    Some(p) => self.value_chain_has(&p, key),
                    None => false,
                }
            }
            Value::Instance(i) => i.fields.borrow().contains_key(key),
            Value::Fn(f) => {
                // prototype 은 생성자성 함수만 가진다 — 화살표·async(비제너레이터)는 없다
                // (§ 화살표/메서드/async 엔 [[Construct]] 없음, 제너레이터는 있음).
                // 예전엔 모든 함수에 대해 true 라 `'prototype' in (()=>{})` 가 참이었다.
                let has_proto = f.is_generator || (!f.is_arrow && !f.is_method && !f.is_async);
                if f.props.borrow().contains_key(key) || (key == "prototype" && has_proto) {
                    return true;
                }
                // name/length 는 계산 own 프로퍼티지만 delete 됐으면(툼스톤) 없는 것.
                if matches!(key, "name" | "length") {
                    return !f.props.borrow().contains_key(&format!("\u{0}fndel:{}", key));
                }
                // 함수도 ordinary object — [[Prototype]] 체인(정적 상속)도 본다.
                // member_get(fn_static_lookup)과 일관돼야 ToPropertyDescriptor 가 상속
                // 서술자 필드(예: Function.prototype 에 얹은 value)를 읽는다.
                let start = f
                    .props
                    .borrow()
                    .get("__proto__")
                    .cloned()
                    .unwrap_or_else(|| self.fn_proto.clone());
                self.value_chain_has(&start, key)
            }
            Value::Arr(a) => {
                if key.parse::<usize>().map(|n| n < a.borrow().len()).unwrap_or(false)
                    || a.get_prop(key).is_some()
                    || key == "length"
                    || key == "push"
                    || key == "\u{0}@@iterator"
                    || natives::array_op_for(key).is_some()
                {
                    return true;
                }
                // Array.prototype 체인의 상속 프로퍼티(사용자가 얹은 것 포함).
                if let Value::Obj(ns) = &self.array_ns {
                    if let Some(proto) = ns.borrow().get("prototype").cloned() {
                        return self.value_chain_has(&proto, key);
                    }
                }
                false
            }
            // HasProperty (§7.3.11): own(name/length/정적/prototype) + 상속 함수 메서드.
            Value::Native(n) => {
                matches!(key, "name" | "length")
                    || self
                        .native_ctor_own_keys(n)
                        .map(|ks| ks.iter().any(|k| k.as_str() == key))
                        .unwrap_or(false)
                    || self.native_fn_member(obj, key).is_some()
            }
            Value::Bound(_) => matches!(key, "name" | "length"),
            // 원시값 수신자: 래퍼 프로토타입 체인의 상속 프로퍼티도 HasProperty 다
            // (Array.prototype.X.call(원시값) 의 상속 인덱스/length 등, §23.1.3 generic).
            Value::Str(s) => {
                key.parse::<usize>().map(|n| n < s.chars().count()).unwrap_or(false)
                    || key == "length"
                    || self.value_chain_has(&self.string_proto.clone(), key)
            }
            Value::Num(_) => self.value_chain_has(&self.number_proto.clone(), key),
            Value::Bool(_) => self.value_chain_has(&self.boolean_proto.clone(), key),
            _ => false,
        }
    }

    fn proto_chain_lookup(
        &mut self,
        map: &Rc<RefCell<ObjMap>>,
        key: &str,
        this: &Value,
    ) -> Result<Option<Value>, String> {
        let mut proto = map.borrow().get("__proto__").cloned();
        let mut depth = 0;
        loop {
            depth += 1;
            if depth > 100 {
                break; // 순환/과도한 체인 방어
            }
            match proto {
                Some(Value::Obj(p)) => {
                    let found = p.borrow().get(key).cloned();
                    match found {
                        Some(Value::Accessor(acc)) => {
                            return Ok(Some(match &acc.get {
                                Some(g) => self.call_value(g.clone(), Some(this.clone()), vec![])?,
                                None => Value::Undefined,
                            }))
                        }
                        Some(v) => return Ok(Some(v)),
                        None => proto = p.borrow().get("__proto__").cloned(),
                    }
                }
                // 프로토타입이 배열이면 그 배열의 인덱스/length/Array.prototype 메서드를 상속한다
                // (`function foo(){}; foo.prototype = [1,2,3]; new foo().filter` 등). member_get 이
                // Array own + Array.prototype 을 전부 해석하므로 그 결과를 그대로 쓴다.
                Some(Value::Arr(a)) => {
                    let v = self.member_get(&Value::Arr(a), key)?;
                    return Ok(if matches!(v, Value::Undefined) { None } else { Some(v) });
                }
                // 프로토타입이 프록시면 그 [[Get]](get 트랩)을 거친다 —
                // Object.create(proxy).foo 처럼 프록시가 프로토타입 체인에 있을 때.
                // 프록시에서 조회는 멈춘다(결과가 최종답).
                Some(v @ Value::Proxy(_)) => {
                    let r = self.member_get(&v, key)?;
                    return Ok(if matches!(r, Value::Undefined) { None } else { Some(r) });
                }
                _ => break,
            }
        }
        Ok(None)
    }

    /// 함수 객체의 [[Prototype]] 체인에서 정적 프로퍼티를 조회한다 (§10.1.8.1 OrdinaryGet).
    /// 함수도 ordinary object 이므로, own 프로퍼티와 내장 멤버(call/apply/bind/name/
    /// length/prototype — 호출부에서 이미 처리)에 없으면 [[Prototype]] 체인을 걷는다.
    /// `Int8Array.from === %TypedArray%.from` 같은 정적 상속과 `class B extends A` 의
    /// 정적 메서드 상속의 토대. 접근자는 원 수신자(this=recv)로 호출해야
    /// `Int8Array[Symbol.species] === Int8Array` 가 성립한다(수신자가 %TypedArray% 가 아님).
    fn fn_static_lookup(
        &mut self,
        start: Value,
        key: &str,
        recv: &Value,
    ) -> Result<Option<Value>, String> {
        let mut proto = start;
        let mut depth = 0;
        loop {
            depth += 1;
            if depth > 100 {
                break; // 순환/과도한 체인 방어
            }
            let (found, next) = match &proto {
                Value::Obj(p) => {
                    let b = p.borrow();
                    (b.get(key).cloned(), b.get("__proto__").cloned())
                }
                Value::Fn(f) => {
                    let b = f.props.borrow();
                    // 함수 프로토의 기본 [[Prototype]] 은 Function.prototype
                    let next = b
                        .get("__proto__")
                        .cloned()
                        .or_else(|| Some(self.fn_proto.clone()));
                    (b.get(key).cloned(), next)
                }
                // Native/그 밖의 프로토 노드: 정적 상속 대상 아님(내장 생성자는 별도 arm).
                _ => break,
            };
            match found {
                Some(Value::Accessor(acc)) => {
                    return Ok(Some(match &acc.get {
                        Some(g) => self.call_value(g.clone(), Some(recv.clone()), vec![])?,
                        None => Value::Undefined,
                    }));
                }
                Some(v) => return Ok(Some(v)),
                None => match next {
                    Some(p) => proto = p,
                    None => break,
                },
            }
        }
        Ok(None)
    }

    // 내장 exotic 값(Map/Set 등)의 인스턴스 프로퍼티 Get. [[Prototype]] 체인이 명시적으로
    // 연결돼 있지 않으므로(Date 등 다른 내장과 동일 관례), 먼저 해당 prototype 을 걷고,
    // 없으면 Object.prototype 상속분(hasOwnProperty/toString/valueOf/…)을 본다. 접근자는
    // this=recv 로 호출한다. 이로써 사용자 오버라이드(Map.prototype.set = …)도 관측된다.
    fn exotic_proto_get(
        &mut self,
        proto: Value,
        key: &str,
        recv: &Value,
    ) -> Result<Value, String> {
        if let Some(v) = self.fn_static_lookup(proto, key, recv)? {
            return Ok(v);
        }
        let objp = match &self.object_ns {
            Value::Obj(m) => m.borrow().get("prototype").cloned(),
            _ => None,
        };
        if let Some(objp) = objp {
            if let Some(v) = self.fn_static_lookup(objp, key, recv)? {
                return Ok(v);
            }
        }
        Ok(Value::Undefined)
    }

    fn member_get(&mut self, recv: &Value, key: &str) -> Result<Value, String> {
        // private 멤버(#x) 접근은 그 private 을 선언한 클래스의 인스턴스(또는 static 은
        // 클래스 자신)에서만 유효하다 — 아니면 TypeError(§ PrivateElementFind).
        // 예전엔 undefined 를 돌려줘 o.#x(o 가 미보유) 가 조용히 통과했다.
        if is_private_name(key) {
            let ok = match recv {
                Value::Instance(i) => {
                    i.fields.borrow().contains_key(&field_key(key, self.priv_id))
                        || i.class.find_method(key).is_some()
                        || i.class.find_getter(key).is_some()
                        || i.class.find_setter(key).is_some()
                }
                Value::Class(c) => {
                    c.statics.borrow().contains_key(key)
                        || c.find_static_getter(key).is_some()
                        || c.find_static_setter(key).is_some()
                }
                _ => false,
            };
            if !ok {
                return Err(self.throw_error(
                    "TypeError",
                    format!(
                        "Cannot read private member {} from an object whose class did not declare it",
                        key
                    ),
                ));
            }
        }
        // 내장에 얹힌 프로퍼티가 최우선 (폴리필이 Promise.allSettled 등을 덮어쓴 경우).
        if let Value::Native(n) = recv {
            if let Some(v) = self.native_props.get(n).and_then(|m| m.get(key)) {
                return Ok(v.clone());
            }
            // Symbol.species (§): get [Symbol.species] 접근자는 this 를 돌려준다 →
            // C[Symbol.species] === C. 사용자 오버라이드(native_props)가 우선.
            if key == "\u{0}@@species" && native_has_species(n) {
                return Ok(recv.clone());
            }
        }
        // .constructor — 값 타입의 전역 생성자 (core-js/프레임워크의 타입판별·종/species 에 필수).
        // 객체/인스턴스가 자체 constructor 프로퍼티를 가지면 그것 우선.
        // Proxy 는 제외 — [[Get]] 은 항상 get 트랩(없으면 타깃)을 거쳐야 한다. typed array
        // 는 Proxy 라, 여기서 가로채면 ta.constructor 재대입이 무시돼 SpeciesConstructor 가
        // 깨진다(sample.constructor[Symbol.species] 가 undefined 로 읽힘).
        if key == "constructor" && !matches!(recv, Value::Proxy(_)) {
            // 플랫폼 객체의 constructor 는 그 인터페이스 객체다 (WebIDL).
            // el.constructor.name === "HTMLDivElement". 예전엔 undefined 라,
            // 노드 종류를 constructor 로 판별하는 코드가 그 자리에서 죽었다.
            if matches!(
                recv,
                Value::Dom(_)
                    | Value::Attr(_, _)
                    | Value::Sheet(_)
                    | Value::CssRule(_, _)
                    | Value::RuleStyle(_, _)
                    | Value::Style(_)
                    | Value::ComputedStyle(_)
            ) {
                let chain = self.call_native(Native::Brand, None, vec![recv.clone()])?;
                if let Value::Arr(a) = &chain {
                    if let Some(Value::Str(iface)) = a.borrow().first() {
                        if let Some(ctor) = env_get(&self.global, iface) {
                            return Ok(ctor);
                        }
                    }
                }
            }
            match recv {
                Value::Obj(m) => {
                    if let Some(v) = m.borrow().get("constructor") {
                        return Ok(v.clone());
                    }
                    // 프로토타입 체인도 본다 — jQuery 는 `jQuery.fn.constructor = jQuery` 로
                    // 프로토타입에 둔다. own 만 보면 pushStack 의 this.constructor() 가
                    // 전역 Object 로 떨어져 "함수 아님" 이 된다.
                    if let Some(pv) = self.proto_chain_lookup(m, "constructor", recv)? {
                        return Ok(pv);
                    }
                }
                Value::Instance(i) => {
                    if let Some(v) = i.fields.borrow().get("constructor") {
                        return Ok(v.clone());
                    }
                    return Ok(Value::Class(i.class.clone()));
                }
                Value::Fn(f) => {
                    if let Some(v) = f.props.borrow().get("constructor") {
                        return Ok(v.clone());
                    }
                }
                _ => {}
            }
            return Ok(self.constructor_of(recv));
        }
        match recv {
            // getComputedStyle 뷰: getPropertyValue + 카멜케이스/대시 프로퍼티 읽기.
            Value::ComputedStyle(id) => {
                if key == "getPropertyValue" {
                    return Ok(Value::Native(Native::ComputedGetProperty));
                }
                self.ensure_layout();
                let dashed = camel_to_dashed(key);
                let v = self
                    .computed_styles
                    .get(id)
                    .and_then(|m| m.get(&dashed))
                    .cloned()
                    .unwrap_or_default();
                Ok(Value::Str(v))
            }
            // 지연 제너레이터: 반복자 프로토콜 메서드. @@iterator 는 자기 자신 반환.
            // el.dataset.x → data-x 속성 (살아있는 뷰)
            Value::Dataset(id) => {
                let dom = self.dom_arena()?;
                let attr = format!("data-{}", camel_to_kebab(key));
                Ok(match &dom.get(*id).node_type {
                    crate::dom::NodeType::Element(e) => e
                        .attributes
                        .get(&attr)
                        .map(|v| Value::Str(v.clone()))
                        .unwrap_or(Value::Undefined),
                    _ => Value::Undefined,
                })
            }
            Value::Gen(g) => match key {
                "next" => Ok(Value::Native(Native::GenNext)),
                "return" => Ok(Value::Native(Native::GenReturn)),
                "throw" => Ok(Value::Native(Native::GenThrow)),
                // 동기 제너레이터는 @@iterator, async generator 는 @@asyncIterator 로 자기
                // 자신을 반환한다(§27.5/§27.6). 예전엔 async gen 에 @@asyncIterator 가 없어
                // for-await 밖에서 async 반복 프로토콜이 통째로 깨졌다.
                "\u{0}@@iterator" => Ok(if Self::gen_is_async(g) {
                    Value::Undefined
                } else {
                    Value::Native(Native::ReturnThis)
                }),
                "\u{0}@@asyncIterator" => Ok(if Self::gen_is_async(g) {
                    Value::Native(Native::ReturnThis)
                } else {
                    Value::Undefined
                }),
                // Iterator 헬퍼(map/filter/take/drop/flatMap/reduce/toArray/…)는
                // %IteratorPrototype%(프렐류드 __kIterProto)에서 상속한다 (§27.1.4).
                // 제너레이터의 member 해석이 하드코딩이라 프로토 체인을 안 걸으므로 여기서 위임.
                _ => {
                    if let Some(ip) = env_get(&self.global, "__kIterProto") {
                        self.member_get(&ip, key)
                    } else {
                        Ok(Value::Undefined)
                    }
                }
            },
            // Proxy: get 트랩 있으면 handler.get(target, key, receiver), 없으면 target 위임
            Value::Proxy(p) => {
                self.proxy_revoked_guard(p)?;
                let (target, handler) = (&p.0, &p.1);
                let trap = self.member_get(handler, "get")?;
                // GetMethod: undefined/null → 타깃 위임, non-callable → TypeError.
                // 예전엔 !undefined 만 봐서 null 트랩을 호출하려다 죽었다.
                if matches!(trap, Value::Undefined | Value::Null) {
                    let target = target.clone();
                    return self.member_get(&target, key);
                }
                if !is_callable(&trap) {
                    return Err(self.throw_error("TypeError", "'get' trap is not callable"));
                }
                {
                    let target = target.clone();
                    let handler = handler.clone();
                    let tr = self.call_value(
                        trap,
                        Some(handler),
                        vec![target.clone(), self.trap_key(key), recv.clone()],
                    )?;
                    // §10.5.8 [[Get]] invariant: target 의 non-configurable 프로퍼티와
                    // 어긋나면 TypeError — non-writable 데이터는 값 SameValue, getter 없는
                    // accessor 는 undefined 여야.
                    let td = self.call_native(
                        Native::ObjectGetOwnPropertyDescriptor,
                        None,
                        vec![target.clone(), Value::Str(key.to_string())],
                    )?;
                    if let Value::Obj(d) = &td {
                        let b = d.borrow();
                        let configurable = matches!(b.get("configurable"), Some(v) if to_bool(v));
                        if !configurable {
                            if b.contains_key("value") {
                                let writable = matches!(b.get("writable"), Some(v) if to_bool(v));
                                let val = b.get("value").cloned().unwrap_or(Value::Undefined);
                                if !writable && !same_value(&tr, &val) {
                                    return Err(self.throw_error("TypeError", "'get' on proxy: non-configurable, non-writable data property but trap returned a different value"));
                                }
                            } else {
                                let get = b.get("get").cloned().unwrap_or(Value::Undefined);
                                if matches!(get, Value::Undefined)
                                    && !matches!(tr, Value::Undefined)
                                {
                                    return Err(self.throw_error("TypeError", "'get' on proxy: non-configurable accessor without getter but trap returned non-undefined"));
                                }
                            }
                        }
                    }
                    Ok(tr)
                }
            }
            Value::Obj(map) => {
                let v = map.borrow().get(key).cloned();
                match v {
                    // 접근자면 this=객체로 호출해 실제 값 산출 (defineProperty get)
                    Some(Value::Accessor(acc)) => match &acc.get {
                        Some(g) => self.call_value(g.clone(), Some(recv.clone()), vec![]),
                        None => Ok(Value::Undefined), // set 만 있는 프로퍼티는 읽으면 undefined
                    },
                    Some(v) => Ok(v),
                    // own 에 없으면 프로토타입 체인(__proto__)을 따라 조회.
                    None => {
                        if let Some(pv) = self.proto_chain_lookup(map, key, recv)? {
                            return Ok(pv);
                        }
                        // window 는 전역 객체 — own 에 없으면 전역 스코프를 본다.
                        // `window.Promise`/`window.Symbol` 같은 기능 탐지가 동작해야 한다.
                        if Rc::ptr_eq(map, &self.window_obj) {
                            if let Some(g) = env_get(&self.global, key) {
                                return Ok(g);
                            }
                            // 이름 붙은 프로퍼티 (HTML §7.3.3): window.target === <div id=target>
                            if let Some(v) = self.named_access(key) {
                                return Ok(v);
                            }
                        }
                        // __proto__ 가 명시적 Null(Object.create(null))이면 Object.prototype
                        // 상속분(hasOwnProperty/toString 등)을 주지 않는다 — 부재(기본 프로토)와 구분.
                        if matches!(map.borrow().get("__proto__"), Some(Value::Null)) {
                            return Ok(Value::Undefined);
                        }
                        match key {
                        "hasOwnProperty" => Ok(Value::Native(Native::HasOwnProperty)),
                        // propertyIsEnumerable: own 프로퍼티면 열거가능(단순 모델) → hasOwnProperty 로 근사.
                        // core-js 등이 {}.propertyIsEnumerable.call(...) 로 기능탐지 → 없으면 크래시.
                        "propertyIsEnumerable" => Ok(Value::Native(Native::PropertyIsEnumerable)),
                        "test" if is_regex_obj(map) => Ok(Value::Native(Native::RegexTest)),
                        "exec" if is_regex_obj(map) => Ok(Value::Native(Native::RegexExec)),
                        // Symbol.match/replace/split/search/matchAll — 정규식 인스턴스엔
                        // __proto__ 링크가 없으므로 프로토타입 메서드를 여기서 직접 준다.
                        _ if is_regex_obj(map) && key.starts_with("\u{0}@@") => {
                            let op = match key {
                                "\u{0}@@match" => Some(natives::StrOp::Match),
                                "\u{0}@@matchAll" => Some(natives::StrOp::MatchAll),
                                "\u{0}@@replace" => Some(natives::StrOp::Replace),
                                "\u{0}@@search" => Some(natives::StrOp::Search),
                                "\u{0}@@split" => Some(natives::StrOp::Split),
                                _ => None,
                            };
                            match op {
                                Some(op) => Ok(Value::Native(Native::RegexSym(op))),
                                None => Ok(Value::Undefined),
                            }
                        }
                        // flags/source/global/ignoreCase/… 는 인스턴스 own 데이터가 아니라
                        // RegExp.prototype 의 접근자로 계산된다 (§22.2.6). 정규식 객체엔
                        // __proto__ 링크가 없으므로 여기서 직접 계산한다. flags 는 표준
                        // 순서(d,g,i,m,s,u,v,y)로 정렬한다.
                        _ if is_regex_obj(map)
                            && natives::RegexAccessor::table().iter().any(|(n, _, _)| *n == key) =>
                        {
                            let (src, flags) = {
                                let b = map.borrow();
                                let g = |k: &str| match b.get(k) {
                                    Some(Value::Str(s)) => s.clone(),
                                    _ => String::new(),
                                };
                                (g("\u{0}source"), g("\u{0}flags"))
                            };
                            let entry = natives::RegexAccessor::table()
                                .iter()
                                .find(|(n, _, _)| *n == key)
                                .unwrap();
                            Ok(match entry.2 {
                                // 개별 플래그: 포함 여부
                                Some(ch) => Value::Bool(flags.contains(ch)),
                                // source/flags: 계산값
                                None if key == "source" => {
                                    Value::Str(if src.is_empty() { "(?:)".to_string() } else { src })
                                }
                                None => Value::Str(
                                    "dgimsuvy".chars().filter(|c| flags.contains(*c)).collect(),
                                ),
                            })
                        }
                        // promise 메서드는 프로토타입 격(비열거) — own 프로퍼티 아님.
                        "then" if is_promise(recv) => Ok(Value::Native(Native::PromiseThen)),
                        "catch" if is_promise(recv) => Ok(Value::Native(Native::PromiseCatch)),
                        "finally" if is_promise(recv) => Ok(Value::Native(Native::PromiseFinally)),
                        _ if is_date_obj(map) => {
                            // Date 인스턴스 메서드는 Date.prototype 상속분이다. 하드코딩
                            // 맵 대신 실제 프로토타입(date_proto)에서 찾아 단일 진실 공급원을
                            // 유지한다 — 이래야 getUTC*/setUTC*/toUTCString/Symbol.toPrimitive
                            // 등이 정확한 name/동작으로 나오고 프로토타입과 어긋나지 않는다.
                            // (Symbol.toPrimitive 는 "\0@@toPrimitive" 키로 온다.)
                            if let Value::Obj(pm) = &self.date_proto {
                                let v = pm.borrow().get(key).cloned();
                                if let Some(v) = v {
                                    return match v {
                                        Value::Accessor(acc) => match &acc.get {
                                            Some(g) => {
                                                self.call_value(g.clone(), Some(recv.clone()), vec![])
                                            }
                                            None => Ok(Value::Undefined),
                                        },
                                        other => Ok(other),
                                    };
                                }
                            }
                            // Date.prototype 에 없으면 Object.prototype 폴백 (hasOwnProperty 등).
                            Ok(self.proto_method("Object", key).unwrap_or(Value::Undefined))
                        }
                        // Object.prototype 폴백 — 인스턴스 객체도 valueOf/toString/hasOwnProperty 등
                        _ => Ok(self.proto_method("Object", key).unwrap_or(Value::Undefined)),
                        }
                    }
                }
            }
            Value::Arr(a) => {
                // 재정의된 own-property 가 내장 메서드를 가린다 (arr.push = fn 등).
                // 접근자면 호출한다 (defineProperty 로 심긴 getter).
                if let Some(v) = a.get_prop(key) {
                    if let Value::Accessor(acc) = &v {
                        return match &acc.get {
                            Some(g) => self.call_value(g.clone(), Some(recv.clone()), vec![]),
                            None => Ok(Value::Undefined),
                        };
                    }
                    return Ok(v);
                }
                if key == "length" {
                    // 근사 희박 배열: length 만 크게 잡은 경우 그 값을 돌려준다
                    if let Some(Value::Num(n)) = a.get_prop("\u{0}sparse_len") {
                        if n as usize > a.borrow().len() {
                            return Ok(Value::Num(n));
                        }
                    }
                    return Ok(Value::Num(a.borrow().len() as f64));
                }
                if key == "push" {
                    return Ok(Value::Native(Native::ArrayPush));
                }
                // 단일 소스(natives::ARRAY_PROTO_OPS)에서 조회 — Array.prototype 구성과 공유.
                if let Some(op) = natives::array_op_for(key) {
                    return Ok(Value::Native(Native::Arr(op)));
                }
                if key == "hasOwnProperty" {
                    return Ok(Value::Native(Native::HasOwnProperty));
                }
                if key == "propertyIsEnumerable" {
                    return Ok(Value::Native(Native::PropertyIsEnumerable));
                }
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                if let Ok(i) = key.parse::<usize>() {
                    // 구멍/범위 밖은 own 이 아니다 — 프로토타입 체인으로(§10.4.2 [[Get]]).
                    let hit = {
                        let b = a.borrow();
                        if i < b.len() && !a.is_hole(i) {
                            Some(b[i].clone())
                        } else {
                            None
                        }
                    };
                    if let Some(v) = hit {
                        // 인덱스에 심긴 접근자면 호출 (defineProperty getter).
                        if let Value::Accessor(acc) = &v {
                            return match &acc.get {
                                Some(g) => self.call_value(g.clone(), Some(recv.clone()), vec![]),
                                None => Ok(Value::Undefined),
                            };
                        }
                        return Ok(v);
                    }
                    // 구멍이면 상속 인덱스(Array.prototype[i])를 본다 — 보통 undefined.
                    // 상속된 접근자면 this=배열로 호출한다.
                    if let Some(m) = self.proto_method("Array", key) {
                        if let Value::Accessor(acc) = &m {
                            return match &acc.get {
                                Some(g) => self.call_value(g.clone(), Some(recv.clone()), vec![]),
                                None => Ok(Value::Undefined),
                            };
                        }
                        return Ok(m);
                    }
                    return Ok(Value::Undefined);
                }
                // Array.prototype 폴리필 메서드 (at/flatMap/findLast 등) 조회
                if let Some(m) = self.proto_method("Array", key) {
                    return Ok(m);
                }
                Ok(Value::Undefined)
            }
            Value::MapVal(m) => {
                // size 는 exotic 접근자(수신자별). @@iterator 는 entries 반복자.
                if key == "size" {
                    return Ok(Value::Num(m.borrow().len() as f64));
                }
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                // 그 외 모든 키는 Map.prototype 체인에서 Get (set/get 등 내장 메서드,
                // Object.prototype 상속분(hasOwnProperty/toString), 사용자 오버라이드 포함).
                let proto = self.map_proto.clone();
                self.exotic_proto_get(proto, key, recv)
            }
            Value::SetVal(s) => {
                if key == "size" {
                    return Ok(Value::Num(s.borrow().len() as f64));
                }
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                let proto = self.set_proto.clone();
                self.exotic_proto_get(proto, key, recv)
            }
            // CSSOM: 시트/규칙/규칙스타일
            Value::Sheet(_) | Value::CssRule(_, _) | Value::RuleStyle(_, _) => {
                self.cssom_get(recv, key)
            }
            // Attr 노드 읽기 (§4.9.2). 소유 요소의 속성을 실시간으로 본다.
            Value::Attr(id, name) => {
                let (id, name) = (*id, name.clone());
                match key {
                    "name" | "nodeName" => Ok(Value::Str(name)),
                    // 정규화된 이름에서 접두사를 뗀 것이 로컬 이름
                    "localName" => Ok(Value::Str(
                        name.rsplit(':').next().unwrap_or(&name).to_string(),
                    )),
                    "prefix" => Ok(match name.split_once(':') {
                        Some((p, _)) => Value::Str(p.to_string()),
                        None => Value::Null,
                    }),
                    "value" | "nodeValue" | "textContent" => {
                        let dom = self.dom_arena()?;
                        let v = match &dom.get(id).node_type {
                            crate::dom::NodeType::Element(e) => {
                                e.attributes.get(&name).cloned().unwrap_or_default()
                            }
                            _ => String::new(),
                        };
                        Ok(Value::Str(v))
                    }
                    "ownerElement" => Ok(Value::Dom(id)),
                    "nodeType" => Ok(Value::Num(2.0)), // ATTRIBUTE_NODE
                    "specified" => Ok(Value::Bool(true)), // 표준: 항상 true
                    "namespaceURI" => Ok(Value::Null),
                    _ => Ok(Value::Undefined),
                }
            }
            // element.style.prop 읽기 (라이브 프록시)
            Value::Style(id) => {
                let id = *id;
                match key {
                    "cssText" => Ok(Value::Str(self.style_attr(id))),
                    "setProperty" => Ok(Value::Native(Native::StyleSetProperty)),
                    "getPropertyValue" => Ok(Value::Native(Native::StyleGetProperty)),
                    "removeProperty" => Ok(Value::Native(Native::StyleRemoveProperty)),
                    _ => {
                        let prop = camel_to_kebab(key);
                        Ok(Value::Str(self.style_get(id, &prop)))
                    }
                }
            }
            // element.classList.add/remove/toggle/contains + length/value
            Value::ClassList(id) => {
                let id = *id;
                match key {
                    "add" => Ok(Value::Native(Native::ClassAdd)),
                    "remove" => Ok(Value::Native(Native::ClassRemove)),
                    "toggle" => Ok(Value::Native(Native::ClassToggle)),
                    "contains" => Ok(Value::Native(Native::ClassContains)),
                    "replace" => Ok(Value::Native(Native::ClassReplace)),
                    "supports" => Ok(Value::Native(Native::ClassSupports)),
                    "item" => Ok(Value::Native(Native::ClassItem)),
                    "length" => Ok(Value::Num(self.class_tokens(id).len() as f64)),
                    // value 는 class 속성을 반영한다 — **원문 그대로** (정규화하지 않는다).
                    // 예전엔 토큰을 다시 이어 붙여서 "  a  a b" 가 "a b" 로 보였다.
                    "value" => Ok(Value::Str(self.class_attr(id))),
                    "toString" => Ok(Value::Native(Native::ClassValue)),
                    _ => {
                        if let Ok(i) = key.parse::<usize>() {
                            Ok(self
                                .class_tokens(id)
                                .get(i)
                                .cloned()
                                .map(Value::Str)
                                .unwrap_or(Value::Undefined))
                        } else {
                            Ok(Value::Undefined)
                        }
                    }
                }
            }
            Value::Str(s) => {
                if key == "length" {
                    return Ok(Value::Num(s.encode_utf16().count() as f64)); // UTF-16 코드 유닛 수
                }
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                let op = match key {
                    "indexOf" => Some(StrOp::IndexOf),
                    "lastIndexOf" => Some(StrOp::LastIndexOf),
                    "slice" => Some(StrOp::Slice),
                    "substring" => Some(StrOp::Substring),
                    "split" => Some(StrOp::Split),
                    "toUpperCase" => Some(StrOp::Upper),
                    "toLowerCase" => Some(StrOp::Lower),
                    "toLocaleUpperCase" => Some(StrOp::LocaleUpper),
                    "toLocaleLowerCase" => Some(StrOp::LocaleLower),
                    "trim" => Some(StrOp::Trim),
                    "replace" => Some(StrOp::Replace),
                    "charAt" => Some(StrOp::CharAt),
                    "includes" => Some(StrOp::Includes),
                    "startsWith" => Some(StrOp::StartsWith),
                    "endsWith" => Some(StrOp::EndsWith),
                    "replaceAll" => Some(StrOp::ReplaceAll),
                    "match" => Some(StrOp::Match),
                    "matchAll" => Some(StrOp::MatchAll),
                    "search" => Some(StrOp::Search),
                    "padStart" => Some(StrOp::PadStart),
                    "padEnd" => Some(StrOp::PadEnd),
                    "repeat" => Some(StrOp::Repeat),
                    "trimStart" | "trimLeft" => Some(StrOp::TrimStart),
                    "trimEnd" | "trimRight" => Some(StrOp::TrimEnd),
                    "charCodeAt" => Some(StrOp::CharCodeAt),
                    "codePointAt" => Some(StrOp::CodePointAt),
                    "concat" => Some(StrOp::Concat),
                    "at" => Some(StrOp::At),
                    "localeCompare" => Some(StrOp::LocaleCompare),
                    "toString" | "valueOf" | "toLocaleString" => {
                        return Ok(Value::Native(Native::ValueToStr))
                    }
                    "substr" => Some(StrOp::Substr),
                    _ => None,
                };
                if let Some(op) = op {
                    return Ok(Value::Native(Native::Str(op)));
                }
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                if let Ok(i) = key.parse::<usize>() {
                    // UTF-16 코드 유닛 인덱싱(짝 없는 서로게이트는 U+FFFD).
                    let units: Vec<u16> = s.encode_utf16().collect();
                    return Ok(units
                        .get(i)
                        .map(|&u| Value::Str(String::from_utf16_lossy(&[u])))
                        .unwrap_or(Value::Undefined));
                }
                // String.prototype 폴리필(at 등) — 원시 문자열에도 적용 (배열과 동일)
                if let Some(m) = self.proto_method("String", key) {
                    return Ok(m);
                }
                Ok(Value::Undefined)
            }
            Value::Dom(id) => {
                let native = match key {
                    "addEventListener" => Some(Native::AddEventListener),
                    "removeEventListener" => Some(Native::RemoveEventListener),
                    "appendChild" => Some(Native::AppendChild),
                    "append" => Some(Native::NodeAppend),
                    "prepend" => Some(Native::NodePrepend),
                    "before" => Some(Native::NodeBefore),
                    "after" => Some(Native::NodeAfter),
                    "replaceWith" => Some(Native::NodeReplaceWith),
                    "insertBefore" => Some(Native::InsertBefore),
                    "insertAdjacentHTML" => Some(Native::InsertAdjacentHTML),
                    "insertAdjacentElement" => Some(Native::InsertAdjacentElement),
                    "createTextNode" => Some(Native::CreateTextNode),
                    "remove" => Some(Native::RemoveElement),
                    "setAttribute" => Some(Native::SetAttribute),
                    "setAttributeNS" => Some(Native::SetAttributeNS),
                    "getAttributeNS" => Some(Native::GetAttributeNS),
                    "removeAttributeNS" => Some(Native::RemoveAttributeNS),
                    "hasAttributeNS" => Some(Native::HasAttributeNS),
                    "getAttribute" => Some(Native::GetAttribute),
                    "removeAttribute" => Some(Native::RemoveAttribute),
                    "hasAttribute" => Some(Native::HasAttribute),
                    "removeChild" => Some(Native::RemoveChild),
                    "querySelector" => Some(Native::QuerySelector),
                    "querySelectorAll" => Some(Native::QuerySelectorAll),
                    "getElementsByClassName" => Some(Native::GetElementsByClass),
                    "getElementsByTagName" => Some(Native::GetElementsByTag),
                    "getBoundingClientRect" => Some(Native::GetBoundingClientRect),
                    "scrollIntoView" => Some(Native::ScrollIntoView),
                    "click" => Some(Native::ElementClick),
                    "focus" => Some(Native::Focus),
                    "blur" => Some(Native::Blur),
                    "animate" => Some(Native::ElementAnimate),
                    "getAttributeNames" => Some(Native::GetAttributeNames),
                    "hasAttributes" => Some(Native::HasAttributes),
                    "toggleAttribute" => Some(Native::ToggleAttribute),
                    "replaceChildren" => Some(Native::ReplaceChildren),
                    "getAnimations" => Some(Native::GetAnimations),
                    "attachShadow" => Some(Native::AttachShadow),
                    "dispatchEvent" => Some(Native::DispatchEvent),
                    "cloneNode" => Some(Native::CloneNode),
                    "substringData" => Some(Native::CharData(CharDataOp::Substring)),
                    "appendData" => Some(Native::CharData(CharDataOp::Append)),
                    "insertData" => Some(Native::CharData(CharDataOp::Insert)),
                    "deleteData" => Some(Native::CharData(CharDataOp::Delete)),
                    "replaceData" => Some(Native::CharData(CharDataOp::Replace)),
                    "splitText" => Some(Native::SplitText),
                    "getAttributeNode" => Some(Native::GetAttributeNode),
                    "lookupNamespaceURI" => Some(Native::LookupNamespaceURI),
                    "lookupPrefix" => Some(Native::LookupPrefix),
                    "isDefaultNamespace" => Some(Native::IsDefaultNamespace),
                    "setAttributeNode" => Some(Native::SetAttributeNode),
                    "removeAttributeNode" => Some(Native::RemoveAttributeNode),
                    "matches" => Some(Native::Matches),
                    "closest" => Some(Native::Closest),
                    "contains" => Some(Native::DomContains),
                    "getContext" => Some(Native::CanvasGetContext),
                    _ => None,
                };
                if let Some(n) = native {
                    return Ok(Value::Native(n));
                }
                // 스크립트가 붙인 프로퍼티가 우선 (표준 프로퍼티를 가리지는 않는다)
                if let Some(v) = self.dom_props.get(&(*id, key.to_string())) {
                    return Ok(v.clone());
                }
                let v = self.dom_get(*id, key)?;
                if !matches!(v, Value::Undefined) {
                    return Ok(v);
                }
                // 업그레이드된 커스텀 엘리먼트: 그 클래스의 프로토타입 체인을 본다.
                if let Some(ctor) = self.element_classes.get(id).cloned() {
                    let proto = self.member_get(&ctor, "prototype")?;
                    if !matches!(proto, Value::Undefined) {
                        let m = self.member_get(&proto, key)?;
                        if !matches!(m, Value::Undefined) {
                            return Ok(m);
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            Value::Instance(inst) => {
                // 필드 우선, 그다음 get 접근자(호출해 값 산출), 그다음 메서드 체인.
                // private 이름은 내부 키로 저장돼 있다 (프로퍼티가 아니다).
                let fkey = field_key(key, self.priv_id);
                if let Some(v) = inst.fields.borrow().get(&fkey) {
                    return Ok(v.clone());
                }
                if let Some(g) = inst.class.find_getter(key) {
                    return self.call_value(Value::Fn(g), Some(recv.clone()), vec![]);
                }
                if let Some(m) = inst.class.find_method(key) {
                    return Ok(Value::Fn(m));
                }
                // 클래스 prototype 에 **동적으로 얹은** 데이터 프로퍼티/접근자
                // (C.prototype.x = v). 클래스 체인의 각 prototype(proto_cache) own 을
                // 본다 — 예전엔 find_method(메서드만) 후 곧장 부모 네이티브 prototype 으로
                // 점프해, Err.prototype.message='custom' 같은 상속 데이터가 무시됐다.
                {
                    let mut cur = Some(inst.class.clone());
                    while let Some(cls) = cur {
                        let cached = cls.proto_cache.borrow().clone();
                        if let Some(Value::Obj(pm)) = cached {
                            let v = pm.borrow().get(key).cloned();
                            if let Some(v) = v {
                                return match v {
                                    Value::Accessor(acc) => match &acc.get {
                                        Some(g) => {
                                            self.call_value(g.clone(), Some(recv.clone()), vec![])
                                        }
                                        None => Ok(Value::Undefined),
                                    },
                                    other => Ok(other),
                                };
                            }
                        }
                        cur = cls.parent.clone();
                    }
                }
                // 클래스가 아닌 부모를 확장했으면(extends Error/함수) 그 prototype 에서 찾는다.
                if let Some(pc) = inst.class.find_parent_ctor() {
                    let proto = self.member_get(&pc, "prototype")?;
                    if !matches!(proto, Value::Undefined) {
                        let m = self.member_get(&proto, key)?;
                        if !matches!(m, Value::Undefined) {
                            return Ok(m);
                        }
                    }
                }
                // Object.prototype 폴백 — 인스턴스도 hasOwnProperty/toString/valueOf 등.
                match key {
                    "hasOwnProperty" => Ok(Value::Native(Native::HasOwnProperty)),
                    "propertyIsEnumerable" => Ok(Value::Native(Native::PropertyIsEnumerable)),
                    _ => Ok(self.proto_method("Object", key).unwrap_or(Value::Undefined)),
                }
            }
            Value::Class(c) => {
                // 클래스도 함수다: C.name / C.length (§10.2.9, §15.7). length 는 생성자
                // 파라미터 수(기본 0). 정적 오버라이드(데이터 또는 getter)가 있으면 그게 우선.
                if key == "name"
                    && c.statics.borrow().get("name").is_none()
                    && c.find_static_getter("name").is_none()
                {
                    return Ok(Value::Str(c.name.borrow().clone()));
                }
                if key == "length"
                    && c.statics.borrow().get("length").is_none()
                    && c.find_static_getter("length").is_none()
                {
                    let n = c.ctor.as_ref().map(|f| Self::fn_expected_args(f)).unwrap_or(0.0);
                    return Ok(Value::Num(n));
                }
                // C.prototype — 클래스 메서드를 담은 객체 (상속 체인 포함).
                // 예전엔 undefined 라, 프로토타입에서 메서드를 꺼내 특정 this 로 호출하는
                // 코드(커스텀 엘리먼트의 connectedCallback 등)가 전부 실패했다.
                if key == "prototype" {
                    if let Some(p) = c.proto_cache.borrow().clone() {
                        return Ok(p);
                    }
                    let mut m = ObjMap::new();
                    // 클래스의 메서드/접근자/constructor 는 **비열거**다 (§15.4).
                    // 예전엔 열거 가능해서 Object.keys(C.prototype) 가 ["m","g","constructor"]
                    // 였다 — for-in 이나 JSON 으로 프로토타입 메서드가 새어 나온다.
                    fn collect(cls: &Rc<JsClass>, m: &mut ObjMap) {
                        if let Some(p) = &cls.parent {
                            collect(p, m);
                        }
                        for (k, f) in &cls.methods {
                            // private 메서드(#x)는 prototype 의 public 프로퍼티가 아니다
                            // (§15.7.14). this.#x() 호출은 find_method 로 별도 해석된다.
                            if is_private_name(k) {
                                continue;
                            }
                            m.insert(k.clone(), Value::Fn(f.clone()));
                            m.insert(nonenum_marker(k), Value::Bool(true));
                        }
                        // getter/setter 를 같은 이름이면 하나의 Accessor(get+set)로 병합한다
                        // (§15.4). 예전엔 setter 를 통째로 빠뜨려 gOPD 가 set=undefined 였고
                        // setter-only 프로퍼티는 서술자 자체가 없었다(대입은 별도 경로라 됐음).
                        // private 접근자(#x)도 public 프로퍼티 아님.
                        let mut names: Vec<String> = Vec::new();
                        for k in cls.getters.keys().chain(cls.setters.keys()) {
                            if !names.contains(k) && !is_private_name(k) {
                                names.push(k.clone());
                            }
                        }
                        for name in names {
                            let get = cls.getters.get(&name).map(|g| Value::Fn(g.clone()));
                            let set = cls.setters.get(&name).map(|s| Value::Fn(s.clone()));
                            m.insert(
                                name.clone(),
                                Value::Accessor(Rc::new(AccessorPair { get, set })),
                            );
                            m.insert(nonenum_marker(&name), Value::Bool(true));
                        }
                    }
                    collect(c, &mut m);
                    m.insert("constructor".to_string(), recv.clone());
                    m.insert(nonenum_marker("constructor"), Value::Bool(true));
                    // class X extends null: prototype 의 [[Prototype]] 은 null (§15.7.14).
                    if matches!(&c.parent_ctor, Some(Value::Null)) {
                        m.insert("__proto__".to_string(), Value::Null);
                    }
                    let proto = Value::Obj(Rc::new(RefCell::new(m)));
                    *c.proto_cache.borrow_mut() = Some(proto.clone());
                    return Ok(proto);
                }
                // static get 접근자 (this=클래스). 예전엔 평범한 정적 메서드로 저장돼
                // C.observedAttributes 가 배열이 아니라 함수를 돌려줬다.
                if let Some(g) = c.find_static_getter(key) {
                    return self.call_value(Value::Fn(g), Some(recv.clone()), vec![]);
                }
                if let Some(v) = c.statics.borrow().get(key).cloned() {
                    return Ok(v);
                }
                // 상속된 정적 멤버 (JsClass 부모 체인). 네이티브를 extends 한 지점의
                // parent_ctor 를 기억해 둔다.
                let mut native_parent = c.parent_ctor.clone();
                let mut p = c.parent.clone();
                while let Some(cls) = p {
                    if let Some(v) = cls.statics.borrow().get(key).cloned() {
                        return Ok(v);
                    }
                    if cls.parent_ctor.is_some() {
                        native_parent = cls.parent_ctor.clone();
                    }
                    p = cls.parent.clone();
                }
                // 클래스도 함수다 — 클래스 자신의 함수 공통 멤버가 상속보다 우선.
                // (특히 toString 은 클래스 소스를 돌려줘야 하므로 부모 toString 에 가려지면
                //  안 된다.) 예전엔 C.toString/C.call/C.bind 가 전부 undefined 였다.
                match key {
                    "call" => return Ok(Value::Native(Native::FnCall)),
                    "apply" => return Ok(Value::Native(Native::FnApply)),
                    "bind" => return Ok(Value::Native(Native::FnBind)),
                    "toString" => return Ok(Value::Native(Native::FnToString)),
                    _ => {}
                }
                // 네이티브 생성자를 extends 하면 그 정적 메서드도 상속한다 (§10.2: 파생
                // 클래스의 [[Prototype]] 은 부모 생성자). class MyP extends Promise →
                // MyP.resolve === Promise.resolve, class A extends Array → A.from 등.
                // 예전엔 parent_ctor(네이티브)를 안 봐서 전부 undefined 였다. 함수 공통
                // 멤버 뒤에 둔다(클래스 자신의 name/length/toString 우선).
                if let Some(np) = &native_parent {
                    if matches!(np, Value::Native(_) | Value::Fn(_) | Value::Bound(_)) {
                        let v = self.member_get(np, key)?;
                        if !matches!(v, Value::Undefined) {
                            return Ok(v);
                        }
                    }
                }
                // 나머지는 Function.prototype→Object.prototype 체인 (hasOwnProperty 등).
                Ok(self
                    .fn_static_lookup(self.fn_proto.clone(), key, recv)?
                    .unwrap_or(Value::Undefined))
            }
            Value::Fn(func) => {
                // 함수도 객체: 속성 백 우선, 그다음 call/apply/bind, prototype/name/length
                let stored = func.props.borrow().get(key).cloned();
                if let Some(v) = stored {
                    return match v {
                        Value::Accessor(acc) => match &acc.get {
                            Some(g) => self.call_value(g.clone(), Some(recv.clone()), vec![]),
                            None => Ok(Value::Undefined),
                        },
                        other => Ok(other),
                    };
                }
                match key {
                    "call" => Ok(Value::Native(Native::FnCall)),
                    "apply" => Ok(Value::Native(Native::FnApply)),
                    "bind" => Ok(Value::Native(Native::FnBind)),
                    // name/length 는 계산 프로퍼티지만 delete 로 삭제됐으면(툼스톤) 없는 것.
                    "name" if !func.props.borrow().contains_key("\u{0}fndel:name") => {
                        Ok(Value::Str(func.name.borrow().clone()))
                    }
                    "length" if !func.props.borrow().contains_key("\u{0}fndel:length") => {
                        Ok(Value::Num(Self::fn_expected_args(func)))
                    }
                    // 함수도 toString 을 가진다 (번들이 fn.toString() 으로 소스 검사)
                    "toString" => Ok(Value::Native(Native::FnToString)),
                    // 화살표·async(비제너레이터) 함수는 prototype 이 없다 (§ [[Construct]]
                    // 없음). 제너레이터/async-generator 는 prototype 을 가진다.
                    "prototype" if !func.is_generator && (func.is_arrow || func.is_method || func.is_async) => {
                        Ok(Value::Undefined)
                    }
                    // F.prototype 지연 생성: 접근 시 { constructor: F } 객체를 만들어 저장.
                    // §20.1.1: F.prototype.constructor === F (writable, non-enum,
                    // configurable). 예전엔 빈 객체라 new F().constructor 가 Object 로
                    // 떨어졌다 — 사용자 정의 에러 클래스(함수형)의 assert.throws 가 통째로
                    // 깨졌다(thrown.constructor !== 기대 생성자).
                    "prototype" => {
                        let mut pm = ObjMap::new();
                        pm.insert("constructor".to_string(), recv.clone());
                        set_prop_attrs(
                            &mut pm,
                            "constructor",
                            ATTR_WRITABLE | ATTR_CONFIGURABLE,
                        );
                        let proto = Value::Obj(Rc::new(RefCell::new(pm)));
                        func.props.borrow_mut().insert("prototype".to_string(), proto.clone());
                        Ok(proto)
                    }
                    // own·내장 멤버에 없으면 함수의 [[Prototype]] 체인(정적 상속)을 따른다.
                    // 기본 [[Prototype]] 은 Function.prototype; setPrototypeOf 로 바뀌었으면 그것.
                    _ => {
                        let start = func
                            .props
                            .borrow()
                            .get("__proto__")
                            .cloned()
                            .unwrap_or_else(|| self.fn_proto.clone());
                        Ok(self.fn_static_lookup(start, key, recv)?.unwrap_or(Value::Undefined))
                    }
                }
            }
            // Function.prototype (정체성 보존된 객체)
            Value::Native(Native::FunctionCtor) if key == "prototype" => Ok(self.fn_proto.clone()),
            // Date.now / Date.parse / Date.UTC / Date.prototype
            Value::Native(Native::DateCtor) => Ok(match key {
                "now" => Value::Native(Native::DateNow),
                "parse" => Value::Native(Native::DateParse),
                "UTC" => Value::Native(Native::DateUTC),
                "prototype" => self.date_proto.clone(),
                // 함수 공통 멤버(call/apply/bind/hasOwnProperty/…) 상속 — Number 등과
                // 일관되게. 예전엔 Date.hasOwnProperty 가 undefined 라 "함수 아님" 크래시.
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            // Object/Array 전역은 Native 생성자(typeof === "function"). 정적 멤버·prototype 은
            // 보관된 네임스페이스 맵에 위임한다.
            Value::Native(Native::ObjectCtor) => {
                let ns = self.object_ns.clone();
                match self.member_get(&ns, key)? {
                    // ns 에 없으면 함수 공통 멤버(name/length/call/…)로 폴백
                    Value::Undefined => Ok(self.native_fn_member(recv, key).unwrap_or(Value::Undefined)),
                    v => Ok(v),
                }
            }
            Value::Native(Native::ArrayCtor) => {
                let ns = self.array_ns.clone();
                match self.member_get(&ns, key)? {
                    Value::Undefined => Ok(self.native_fn_member(recv, key).unwrap_or(Value::Undefined)),
                    v => Ok(v),
                }
            }
            // Map/Set(=WeakMap/WeakSet).prototype — 번들의 Map.prototype.get 등.
            Value::Native(Native::MapCtor) if key == "prototype" => Ok(self.map_proto.clone()),
            Value::Native(Native::MapCtor) if key == "groupBy" => Ok(Value::Native(Native::MapGroupBy)),
            Value::Native(Native::SetCtor) if key == "prototype" => Ok(self.set_proto.clone()),
            // Error/TypeError/… 의 prototype 과 name (class X extends Error, 기능 탐지).
            Value::Native(Native::EventCtor(n)) => Ok(match key {
                "prototype" => self.event_proto(n).unwrap_or(Value::Undefined),
                "name" => Value::Str(n.to_string()),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            Value::Native(Native::ErrorCtor(n)) => Ok(match key {
                // 종류별 prototype (TypeError.prototype !== Error.prototype)
                "prototype" => self
                    .error_protos
                    .iter()
                    .find(|(k, _)| k == n)
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(|| self.error_proto.clone()),
                "name" => Value::Str(n.to_string()),
                // Error.isError (ES2025) 는 %Error% 의 정적 메서드다. 서브타입도
                // 생성자 프로토타입 체인으로 상속하지만 own 은 %Error% 뿐.
                "isError" if *n == "Error" => Value::Native(Native::ErrorIsError),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            // String.fromCharCode/prototype
            Value::Native(Native::StringCtor) => Ok(match key {
                "fromCharCode" => Value::Native(Native::StrFromCharCode),
                "fromCodePoint" => Value::Native(Native::StrFromCodePoint),
                "raw" => Value::Native(Native::StrRaw),
                "prototype" => self.string_proto.clone(),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            // Proxy.revocable (§28.2.1) — 정적 메서드.
            Value::Native(Native::ProxyCtor) => Ok(match key {
                "revocable" => Value::Native(Native::ProxyRevocable),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            // Symbol.iterator 등 잘 알려진 심볼 + Symbol.for/keyFor
            Value::Native(Native::SymbolCtor) => Ok(match key {
                "iterator" => Self::well_known_symbol("\u{0}@@iterator", "Symbol.iterator"),
                "asyncIterator" => {
                    Self::well_known_symbol("\u{0}@@asyncIterator", "Symbol.asyncIterator")
                }
                "toStringTag" => Self::well_known_symbol("\u{0}@@toStringTag", "Symbol.toStringTag"),
                "hasInstance" => Self::well_known_symbol("\u{0}@@hasInstance", "Symbol.hasInstance"),
                "toPrimitive" => Self::well_known_symbol("\u{0}@@toPrimitive", "Symbol.toPrimitive"),
                // Symbol.species (§20.4.2.10): 종파생 생성자 선택. Array/TypedArray/
                // ArrayBuffer/Promise 의 map/filter/slice 등이 반환 종을 이걸로 고른다.
                "species" => Self::well_known_symbol("\u{0}@@species", "Symbol.species"),
                // Symbol.isConcatSpreadable (§23.1.3.1.1): concat 이 인자를 펼칠지 결정.
                "isConcatSpreadable" => Self::well_known_symbol(
                    "\u{0}@@isConcatSpreadable",
                    "Symbol.isConcatSpreadable",
                ),
                // Explicit Resource Management (§ using / DisposableStack).
                "dispose" => Self::well_known_symbol("\u{0}@@dispose", "Symbol.dispose"),
                "asyncDispose" => {
                    Self::well_known_symbol("\u{0}@@asyncDispose", "Symbol.asyncDispose")
                }
                // 정규식 위임 심볼 (§22.2.6): str.match/replace/split/search/matchAll 이 사용.
                "match" => Self::well_known_symbol("\u{0}@@match", "Symbol.match"),
                "matchAll" => Self::well_known_symbol("\u{0}@@matchAll", "Symbol.matchAll"),
                "replace" => Self::well_known_symbol("\u{0}@@replace", "Symbol.replace"),
                "search" => Self::well_known_symbol("\u{0}@@search", "Symbol.search"),
                "split" => Self::well_known_symbol("\u{0}@@split", "Symbol.split"),
                "for" => Value::Native(Native::SymbolFor),
                "keyFor" => Value::Native(Native::SymbolKeyFor),
                "prototype" => self.symbol_proto.clone(),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            // 심볼 원시값: .description / .toString()
            // 심볼 원시값의 멤버 접근은 Symbol.prototype 체인(→ Object.prototype)으로 위임한다.
            // valueOf/toString/description(getter)/[Symbol.toPrimitive]/constructor 및
            // hasOwnProperty 등 상속분이 전부 여기서 해석된다. 예전엔 description/toString/
            // constructor 만 하드코딩하고 나머지는 undefined 라 s.valueOf() 가 깨졌다.
            Value::Symbol(_) => self.exotic_proto_get(self.symbol_proto.clone(), key, recv),
            // Number.isInteger/isNaN/isFinite/parseInt/parseFloat + 상수
            Value::Native(Native::NumberCtor) => Ok(match key {
                "isInteger" => Value::Native(Native::NumIsInteger),
                "isSafeInteger" => Value::Native(Native::NumIsSafeInteger),
                "isFinite" => Value::Native(Native::NumIsFinite),
                "isNaN" => Value::Native(Native::NumIsNaN),
                "parseInt" => Value::Native(Native::ParseInt),
                "parseFloat" => Value::Native(Native::ParseFloat),
                "MAX_SAFE_INTEGER" => Value::Num(9007199254740991.0),
                "MIN_SAFE_INTEGER" => Value::Num(-9007199254740991.0),
                "MAX_VALUE" => Value::Num(f64::MAX),
                // §21.1.2.9: MIN_VALUE 는 최소 양의 "비정규(denormal)"값 5e-324 (from_bits(1)).
                // f64::MIN_POSITIVE 는 최소 정규값(2.2e-308)이라 MIN_VALUE/2 가 0 이 안 됐다.
                "MIN_VALUE" => Value::Num(f64::from_bits(1)),
                "EPSILON" => Value::Num(f64::EPSILON),
                "POSITIVE_INFINITY" => Value::Num(f64::INFINITY),
                "NEGATIVE_INFINITY" => Value::Num(f64::NEG_INFINITY),
                "NaN" => Value::Num(f64::NAN),
                "prototype" => self.number_proto.clone(),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            Value::Native(Native::BooleanCtor) => Ok(match key {
                "prototype" => self.boolean_proto.clone(),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            Value::Native(Native::BigIntCtor) => Ok(match key {
                "prototype" => self.bigint_proto.clone(),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            Value::Native(Native::RegExpCtor) => Ok(match key {
                "prototype" => self.regexp_proto.clone(),
                "escape" => Value::Native(Native::RegExpEscape),
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            // Promise 정적 메서드 + prototype (기능 탐지 'finally' in Promise.prototype)
            Value::Native(Native::PromiseCtor) => Ok(match key {
                "resolve" => Value::Native(Native::PromiseResolve),
                "reject" => Value::Native(Native::PromiseReject),
                "all" => Value::Native(Native::PromiseAll),
                "race" => Value::Native(Native::PromiseRace),
                "allSettled" => Value::Native(Native::PromiseAllSettled),
                "withResolvers" => Value::Native(Native::PromiseWithResolvers),
                "prototype" => {
                    let mut m = ObjMap::new();
                    m.insert("then".to_string(), Value::Native(Native::PromiseThen));
                    m.insert("catch".to_string(), Value::Native(Native::PromiseCatch));
                    m.insert("finally".to_string(), Value::Native(Native::PromiseFinally));
                    Value::Obj(Rc::new(RefCell::new(m)))
                }
                _ => self.native_fn_member(recv, key).unwrap_or(Value::Undefined),
            }),
            // 네이티브 함수: 함수 공통 멤버(name/length/call/apply/bind + 상속 메서드)
            Value::Native(_) => {
                Ok(self.native_fn_member(recv, key).unwrap_or(Value::Undefined))
            }
            // 바운드 함수: name/length/call/apply/bind 우선, 없으면 Function.prototype
            // 체인 상속(§10.4.1: [[Prototype]]=%Function.prototype%). 예전엔 상속을 안 걸어
            // boundFn 이 Function.prototype 에 추가된 프로퍼티를 못 봤다.
            Value::Bound(_) => {
                if let Some(v) = self.native_fn_member(recv, key) {
                    return Ok(v);
                }
                if key == "__proto__" {
                    return Ok(self.fn_proto.clone());
                }
                let proto = self.fn_proto.clone();
                self.member_get(&proto, key)
            }
            // BigInt 메서드: toString(radix) / toLocaleString / valueOf
            Value::BigInt(_) => {
                // BigInt.prototype 에 얹힌 것(사용자 재정의 toLocaleString 등)이 먼저다.
                // 예전엔 네이티브 하드코딩만 봐서 BigInt.prototype.toLocaleString 재정의가
                // 무시됐다(Intl 폴리필 무력화).
                let over = proto_prop(&self.bigint_proto, key);
                if !matches!(over, Value::Undefined) {
                    return Ok(over);
                }
                Ok(match key {
                    "toString" | "toLocaleString" => Value::Native(Native::BigIntToString),
                    "valueOf" => Value::Native(Native::ValueOfSelf),
                    "constructor" => Value::Native(Native::BigIntCtor),
                    _ => Value::Undefined,
                })
            }
            // 숫자 메서드: (5).toFixed(2), n.toString(radix). 나머지는 Number.prototype 폴백.
            // 프로토타입에 얹힌 것이 먼저다 (표준: 메서드는 프로토타입에서 찾는다).
            // 예전엔 네이티브를 먼저 봐서, 페이지가 Number.prototype.toLocaleString 을
            // 갈아끼워도 조용히 무시됐다 (Intl 폴리필이 통째로 무력화된다).
            Value::Num(_) => {
                let over = proto_prop(&self.number_proto, key);
                if !matches!(over, Value::Undefined) {
                    return Ok(over);
                }
                Ok(match key {
                    "toFixed" => Value::Native(Native::NumToFixed),
                    "toExponential" => Value::Native(Native::NumToExponential),
                    "toPrecision" => Value::Native(Native::NumToPrecision),
                    "toString" | "toLocaleString" => Value::Native(Native::ValueToStr),
                    "valueOf" => Value::Native(Native::ValueOfSelf),
                    _ => Value::Undefined,
                })
            }
            Value::Bool(_) => {
                let over = proto_prop(&self.boolean_proto, key);
                if !matches!(over, Value::Undefined) {
                    return Ok(over);
                }
                Ok(match key {
                    "toString" => Value::Native(Native::ValueToStr),
                    "valueOf" => Value::Native(Native::ValueOfSelf),
                    _ => Value::Undefined,
                })
            }
            Value::Undefined | Value::Null => {
                let m = format!("{} 의 '{}' 를 읽을 수 없음", to_display(recv), key);
                Err(self.throw_error("TypeError", m))
            }
            _ => Ok(Value::Undefined),
        }
    }

    // Expr::Call 의 본문 (프레임 push/pop 을 위해 분리). 동작은 그대로.
    fn eval_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        env: &EnvRef,
    ) -> Result<Value, String> {
                    // 옵셔널 체인 호출: a?.m(...) 은 a 가 null/undefined 면 **호출 전체가 단락**된다
                    // (표준 §13.3.9). 예전엔 a?.m 이 undefined 로 평가된 뒤 그걸 호출하려다
                    // "함수 아님" 으로 죽었다 — go.dev 가 menuButtonEl?.addEventListener() 를 쓴다.
                    if let Expr::OptMember { obj, prop, computed } = callee {
                        let recv = self.eval(obj, env)?;
                        if matches!(recv, Value::Undefined | Value::Null) {
                            return Ok(Value::Undefined);
                        }
                        let key = self.member_key(prop, *computed, env)?;
                        let f = self.member_get(&recv, &key)?;
                        if !is_callable(&f) {
                            // 단락은 a 가 nullish 일 때만. m 이 없으면 표준은 TypeError.
                            let name = obj_hint(callee);
                            if self.lenient {
                                *self.lenient_hits.entry(format!("{}() 비함수", name)).or_default() += 1;
                                return Ok(Value::Undefined);
                            }
                            let m = format!(
                                "{}(…) — {} 이(가) {} (함수 아님)",
                                name,
                                name,
                                to_display(&f)
                            );
                            return Err(self.throw_error("TypeError", m));
                        }
                        let a = self.eval_args(args, env)?;
                        return self.call_value(f, Some(recv), a);
                    }
                    let mut arg_vals = Vec::new();
                    // super(...) — 부모 생성자를 현재 this 로 실행
                    if matches!(callee, Expr::Super) {
                        arg_vals.extend(self.eval_args(args, env)?);
                        let (Some(sc), Some(this)) =
                            (env_get(env, "\u{0}superclass__"), env_get(env, "this"))
                        else {
                            return Err("super() 는 파생 클래스 생성자에서만".to_string());
                        };
                        // super() 호출을 기록(파생 생성자 this 초기화 검사용).
                        env_set(env, "\u{0}super_called", Value::Bool(true));
                        match sc {
                            Value::Class(parent) => {
                                if let Some(obj) = self.run_constructor(&parent, &this, arg_vals)? {
                                    env_set(env, "this", obj);
                                }
                            }
                            // 클래스가 아닌 생성자(함수/Error/EventTarget 등) 확장.
                            //
                            // 표준(§10.2.2): 파생 생성자의 this 는 **new.target.prototype**
                            // 을 가진 객체다 — 즉 파생 클래스의 인스턴스다. 부모 생성자는
                            // 그 this 위에서 돈다.
                            // 예전엔 부모를 new 로 따로 만들어 그 객체로 this 를 **갈아끼웠다**.
                            // 그러면 파생 클래스의 메서드가 통째로 사라진다
                            // (class Bus extends EventTarget { on(){} } → bus.on 이 undefined).
                            //
                            // 예외는 커스텀 엘리먼트다: HTMLElement 가 진짜 DOM 노드를 돌려주고,
                            // 표준도 그 노드가 this 가 되도록 정의한다. 그때만 갈아끼운다.
                            other => {
                                // super() 는 부모를 **현재 new.target**(파생 클래스)로 호출한다
                                // (§10.2.2). 부모가 new.target 을 검사하는 함수/네이티브
                                // (예: Iterator 추상 생성자)면 undefined 로 보여 잘못 throw 했다.
                                self.new_target = env_get(env, "\u{0}newtarget");
                                let produced =
                                    self.call_value(other.clone(), Some(this.clone()), arg_vals)?;
                                match produced {
                                    // 커스텀 엘리먼트 업그레이드: 진짜 DOM 노드가 this 다
                                    Value::Dom(_) => env_set(env, "this", produced),
                                    // 부모가 별도 객체를 만들어 돌려준 경우(Error 등):
                                    // 그 own 프로퍼티를 this 에 얹는다 (클래스 정체성은 유지)
                                    v if is_object(&v) => {
                                        for (k, val) in builtins::own_entries_all(&v) {
                                            self.set_own_property(&this, k, val);
                                        }
                                    }
                                    // 부모가 this 위에서 직접 작업한 경우 — 할 일 없음
                                    _ => {}
                                }
                            }
                        }
                        return Ok(Value::Undefined);
                    }
                    // super.method(...) — 부모 메서드를 현재 this 로 실행
                    if let Expr::Member { obj, prop, computed } = callee {
                        if matches!(&**obj, Expr::Super) {
                            let key = self.member_key(prop, *computed, env)?;
                            let (Some(sc), Some(this)) =
                                (env_get(env, "\u{0}superclass__"), env_get(env, "this"))
                            else {
                                return Err("super.x 는 파생 클래스에서만".to_string());
                            };
                            // 부모가 클래스가 아니면 그 prototype 에서 메서드를 찾는다.
                            let parent = match sc {
                                Value::Class(p) => p,
                                other => {
                                    let proto = self.member_get(&other, "prototype")?;
                                    let m = self.member_get(&proto, &key)?;
                                    arg_vals.extend(self.eval_args(args, env)?);
                                    return self.call_value(m, Some(this), arg_vals);
                                }
                            };
                            let m = parent
                                .find_method(&key)
                                .ok_or_else(|| format!("부모에 메서드 {} 없음", key))?;
                            arg_vals.extend(self.eval_args(args, env)?);
                            return self.call_value(Value::Fn(m), Some(this), arg_vals);
                        }
                        let recv = self.eval(obj, env)?;
                        let key = self.member_key(prop, *computed, env)?;
                        if matches!(recv, Value::Undefined | Value::Null) {
                            if self.lenient {
                                *self.lenient_hits.entry(format!(".{}()", key)).or_default() += 1;
                                for a in args {
                                    self.eval(a, env)?; // 부수효과 보존
                                }
                                return Ok(Value::Undefined);
                            }
                            let m = format!(
                                "{}.{}(…) — {} 이(가) {}",
                                obj_hint(obj),
                                key,
                                obj_hint(obj),
                                to_display(&recv)
                            );
                            return Err(self.throw_error("TypeError", m));
                        }
                        let f = self.member_get(&recv, &key)?;
                        arg_vals.extend(self.eval_args(args, env)?);
                        if !is_callable(&f) {
                            if self.lenient {
                                *self.lenient_hits.entry(format!("{}() 비함수", key)).or_default() += 1;
                                return Ok(Value::Undefined);
                            }
                            let m = format!(
                                "{}(…) — {}.{} 이(가) {} (함수 아님, 수신자={})",
                                key,
                                obj_hint(obj),
                                key,
                                to_display(&f),
                                type_of(&recv)
                            );
                            return Err(self.throw_error("TypeError", m));
                        }
                        self.call_value(f, Some(recv), arg_vals)
                    } else {
                        let f = self.eval(callee, env)?;
                        arg_vals.extend(self.eval_args(args, env)?);
                        // 직접 eval: 식별자 `eval` 로 부른 경우에만 호출 지점 스코프에서
                        // 평가한다 (§19.2.1.1). 그 외(간접)는 call_value 가 전역에서 돌린다.
                        if matches!(f, Value::Native(Native::Eval))
                            && matches!(callee, Expr::Ident(n) if n == "eval")
                        {
                            let a = arg_vals.into_iter().next().unwrap_or(Value::Undefined);
                            let scope = Env::new(Some(env.clone()));
                            return self.do_eval(a, env, &scope);
                        }
                        // Array(len): 단일 Number 인자가 유효 uint32(0..2^32-1) 아니면
                        // RangeError (§23.1.1.1). coerce_object_call 은 throw 못하므로 여기서.
                        if let (Value::Native(Native::ArrayCtor), [Value::Num(len)]) =
                            (&f, arg_vals.as_slice())
                        {
                            if !(len.fract() == 0.0 && *len >= 0.0 && *len < 4294967296.0) {
                                return Err(self.throw_error("RangeError", "Invalid array length"));
                            }
                        }
                        // Object(x) — 전역 Object 네임스페이스를 함수로 호출 = 객체 강제변환.
                        // core-js/프레임워크가 Object(this) 로 this 를 객체화하는 흔한 패턴.
                        if let Some(v) = self.coerce_object_call(&f, &arg_vals) {
                            return Ok(v);
                        }
                        if !is_callable(&f) {
                            let name = obj_hint(callee); // 이름이 없으면 진단이 불가능하다
                            if self.lenient {
                                *self.lenient_hits.entry(format!("{}() 비함수", name)).or_default() += 1;
                                return Ok(Value::Undefined);
                            }
                            let m = format!(
                                "{}(…) — {} 이(가) {} (함수 아님)",
                                name,
                                name,
                                to_display(&f)
                            );
                            return Err(self.throw_error("TypeError", m));
                        }
                        self.call_value(f, None, arg_vals)
                    }
    }

    // private 이름 해석은 그 함수가 **만들어진** 클래스 스코프를 따른다 (렉시컬).
    // 호출하는 쪽이 어디든 상관없다 — 클래스 메서드가 만든 콜백을 나중에 밖에서
    // 불러도 그 클래스의 #x 를 본다.
    fn call_value(
        &mut self,
        f: Value,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        let saved = self.priv_id;
        if let Value::Fn(func) = &f {
            self.priv_id = func.priv_id.get();
        }
        let r = self.call_value_inner(f, recv, args);
        self.priv_id = saved;
        r
    }

    fn call_value_inner(
        &mut self,
        f: Value,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match f {
            Value::Fn(func) => {
                let scope = Env::new(Some(func.env.clone()));
                // this 바인딩: 화살표는 캡처한 this (없으면 체인 상속), 일반 함수는 수신자
                if func.is_arrow {
                    if let Some(t) = &func.this {
                        env_declare(&scope, "this", (**t).clone());
                    }
                } else {
                    // 일반 함수 this 바인딩 (§10.2.1.2 OrdinaryCallBindThis).
                    // strict: 강제변환 없음 — 수신자 없으면 undefined, null/원시값 그대로.
                    // sloppy: undefined/null(수신자 없음 또는 apply(undefined)/call(null))은
                    // globalThis, 원시값은 ToObject 로 박싱(f.call(5) → this=Number(5)).
                    // 예전엔 strict 여도 sloppy 처럼 강제변환해 내장 메서드의 수신자 검증
                    // (map.call(null)→TypeError)이 window 로 가려졌다.
                    let this = if body_is_strict(&func.body) {
                        match recv {
                            Some(v) => v,
                            None => Value::Undefined,
                        }
                    } else {
                        match recv {
                            None | Some(Value::Undefined) | Some(Value::Null) => {
                                env_get(&self.global, "window").unwrap_or(Value::Undefined)
                            }
                            Some(v @ (Value::Str(_) | Value::Num(_) | Value::Bool(_))) => {
                                self.to_object_value(v)
                            }
                            Some(v) => v,
                        }
                    };
                    env_declare(&scope, "this", this);
                }
                // 메서드 안 super.x 해석용
                if let Some(sc) = &func.super_class {
                    env_declare(&scope, "\u{0}superclass__", sc.clone());
                }
                for (i, p) in func.params.iter().enumerate() {
                    if let Some(rest) = p.strip_prefix("...") {
                        // rest 파라미터: i 번째부터 남은 인자를 배열로 모은다
                        let items = args.get(i..).map(|s| s.to_vec()).unwrap_or_default();
                        env_declare(&scope, rest, Value::Arr(ArrayObj::new(items)));
                    } else {
                        env_declare(&scope, p, args.get(i).cloned().unwrap_or(Value::Undefined));
                    }
                }
                // arguments 객체 (화살표 제외). 배열로 근사 — .length/인덱스/slice.call 동작.
                if !func.is_arrow {
                    env_declare(&scope, "arguments", Value::Arr(ArrayObj::new(args.clone())));
                }
                // new.target: new 로 호출된 경우만 (construct 가 직전 설정). 일반 호출은 undefined.
                // 화살표는 렉시컬(자기 스코프에 안 심음 → 바깥 것 상속).
                if !func.is_arrow {
                    let nt = self.new_target.take().unwrap_or(Value::Undefined);
                    env_declare(&scope, "\u{0}newtarget", nt);
                } else {
                    self.new_target = None;
                }
                hoist_vars(&func.body, &scope); // var 하이스팅 (함수 스코프)
                // 지연 제너레이터: 본문을 즉시 실행하지 않고 재개가능 제너레이터 객체를 반환.
                // next() 마다 다음 yield 까지 실행(무한 제너레이터/양방향 next(v) 지원).
                if func.is_generator {
                    // 파라미터 프롤로그(구조분해/기본값)는 **호출 시** 실행한다 — 제너레이터도
                    // 파라미터는 즉시 바인딩·검증된다(§FunctionDeclarationInstantiation). 그래서
                    // *gen([{x}]) 를 [null] 로 부르면 여기서 즉시 TypeError. 지연 본문은
                    // make_generator 가 프롤로그를 건너뛴다(중복 실행 방지).
                    for stmt in &func.body[..func.param_prologue_len] {
                        self.exec_stmt(stmt, &scope)?;
                    }
                    return Ok(self.make_generator(func.clone(), scope, func.param_prologue_len));
                }
                // async: 반환값/에러를 Promise 로 감싼다(본문 throw → 거부된 promise).
                if func.is_async {
                    match self.exec_block(&func.body, &scope) {
                        Ok(flow) => {
                            let result = match flow {
                                Flow::Return(v) => v,
                                _ => Value::Undefined,
                            };
                            if is_promise(&result) {
                                return Ok(result); // 이미 promise 면 위임
                            }
                            let p = self.new_promise();
                            self.resolve_promise(&p, result);
                            Ok(p)
                        }
                        Err(e) if e.starts_with(STEP_LIMIT_MSG) => Err(e),
                        Err(_) => {
                            let reason = self.thrown.take().unwrap_or(Value::Undefined);
                            let p = self.new_promise();
                            self.reject_promise(&p, reason);
                            Ok(p)
                        }
                    }
                } else {
                    let result = match self.exec_block(&func.body, &scope)? {
                        Flow::Return(v) => v,
                        _ => Value::Undefined,
                    };
                    Ok(result)
                }
            }
            Value::Native(n) => self.call_native(n, recv, args),
            // 클래스 생성자는 new 없이 호출하면 TypeError (§15.7.10). new C()·Reflect.construct
            // 는 construct 로 직접 가므로, 이 경로는 new 없는 호출(C()/C.call()/apply)뿐이다.
            Value::Class(c) => {
                let nm = c.name.borrow().clone();
                Err(self.throw_error(
                    "TypeError",
                    &if nm.is_empty() {
                        "Class constructor cannot be invoked without 'new'".to_string()
                    } else {
                        format!("Class constructor {} cannot be invoked without 'new'", nm)
                    },
                ))
            }
            // 바운드 함수: 캡처한 this + 선행 인자 앞에 붙여 대상 호출
            Value::Bound(b) => {
                let (target, this_val, partial) = (*b).clone();
                let mut all = partial;
                all.extend(args);
                self.call_value(target, Some(this_val), all)
            }
            // §10.5.12 [[Call]] — 함수 프록시의 apply 트랩(없으면 타깃 직접 호출).
            Value::Proxy(p) => {
                self.proxy_revoked_guard(&p)?;
                let (t, h) = (p.0.clone(), p.1.clone());
                if !is_callable(&t) {
                    return Err(self.throw_error("TypeError", "proxy target is not callable"));
                }
                let trap = self.member_get(&h, "apply")?;
                if matches!(trap, Value::Undefined | Value::Null) {
                    return self.call_value(t, recv, args);
                }
                if !is_callable(&trap) {
                    return Err(self.throw_error("TypeError", "'apply' trap is not callable"));
                }
                let this_arg = recv.unwrap_or(Value::Undefined);
                let arg_array = Value::Arr(ArrayObj::new(args));
                self.call_value(trap, Some(h), vec![t, this_arg, arg_array])
            }
            other => {
                let d = to_display(&other);
                Err(self.throw_error("TypeError", format!("{} 은(는) 함수가 아님", d)))
            }
        }
    }

    // new Class(args) / 클래스 호출: 인스턴스 생성 → 생성자 체인 실행 → 인스턴스 반환.
    // 이 값이 [[Construct]] 를 가진 생성자인가 (§7.2.4 IsConstructor). 내장 메서드/일반
    // 네이티브 함수, 화살표/async/generator 함수, Symbol/BigInt 는 생성자가 아니다.
    pub(super) fn is_constructor(&self, v: &Value) -> bool {
        match v {
            Value::Class(_) => true,
            Value::Fn(f) => !f.is_arrow && !f.is_async && !f.is_generator,
            Value::Native(n) => natives::native_is_constructor(n),
            Value::Bound(b) => self.is_constructor(&b.0),
            Value::Proxy(p) => self.is_constructor(&p.0),
            _ => false,
        }
    }

    fn construct(&mut self, class: Value, args: Vec<Value>) -> Result<Value, String> {
        // Reflect.construct(target, args, newTarget) 나 Proxy construct 위임이
        // self.new_target 로 넘긴 명시적 newTarget(없으면 대상 자신). Fn/Proxy arm 이
        // 공유하므로 초입에서 캡처한다(내부 재진입 construct 는 영향 안 받게 take).
        let pending_new_target = self.new_target.take();
        // new Array(len): 단일 Number 인자가 유효 uint32(0..2^32-1) 아니면 RangeError
        // (§23.1.1.1). coerce_object_call 은 throw 못하므로 여기서.
        if let (Value::Native(Native::ArrayCtor), [Value::Num(len)]) = (&class, args.as_slice()) {
            if !(len.fract() == 0.0 && *len >= 0.0 && *len < 4294967296.0) {
                return Err(self.throw_error("RangeError", "Invalid array length"));
            }
        }
        // new Array(n) / new Object(x) — 네임스페이스 객체를 생성자로 호출(표준).
        if let Some(v) = self.coerce_object_call(&class, &args) {
            return Ok(v);
        }
        let cls = match class {
            Value::Class(c) => c,
            // new Function(params.., body) → 실제 함수로 컴파일
            Value::Native(Native::FunctionCtor) => return self.make_function(args),
            Value::Native(Native::MapCtor) => return self.make_map(args),
            Value::Native(Native::SetCtor) => return self.make_set(args),
            Value::Native(Native::EventCtor(n)) => {
                return self.call_native(Native::EventCtor(n), None, args)
            }
            Value::Native(Native::ProxyCtor) => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
                // §28.2.1: target·handler 는 반드시 객체여야 한다.
                if !is_object(&target) || !is_object(&handler) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Cannot create proxy with a non-object as target or handler",
                    ));
                }
                return Ok(Value::Proxy(Rc::new((target, handler))));
            }
            Value::Native(Native::RegExpCtor) => {
                return self.call_native(Native::RegExpCtor, None, args)
            }
            // new String/Number/Boolean → 원시값 근사 (박싱 미구현)
            // new String/Number/Boolean → 원시 래퍼 객체 (§20/21/22). 원시값을
            // 내부 슬롯에 박고 프로토타입 연결. 예전엔 원시값을 그대로 돌려줘서
            // (new Boolean).foo = 1 이 "false 에 대입 불가" 로 죽었다.
            Value::Native(n @ (Native::StringCtor | Native::NumberCtor | Native::BooleanCtor)) => {
                let prim = self.call_native(n, None, args)?;
                let (proto, tag) = match n {
                    Native::StringCtor => (self.string_proto.clone(), "String"),
                    Native::NumberCtor => (self.number_proto.clone(), "Number"),
                    _ => (self.boolean_proto.clone(), "Boolean"),
                };
                let mut m = ObjMap::new();
                m.insert(WRAPPER_SLOT.to_string(), prim.clone());
                m.insert("__proto__".to_string(), proto);
                m.insert("\u{0}class".to_string(), Value::Str(tag.to_string()));
                // String 래퍼: 인덱스 char + length 를 own 프로퍼티로 (§22.1.4).
                if let (Native::StringCtor, Value::Str(s)) = (n, &prim) {
                    for (i, c) in s.chars().enumerate() {
                        m.insert(i.to_string(), Value::Str(c.to_string()));
                        // 인덱스는 non-writable/non-configurable, 열거 가능
                        m.insert(attr_marker(&i.to_string()), Value::Num(ATTR_ENUMERABLE as f64));
                    }
                    let len = s.chars().count();
                    m.insert("length".to_string(), Value::Num(len as f64));
                    m.insert(nonenum_marker("length"), Value::Bool(true));
                }
                return Ok(Value::Obj(Rc::new(RefCell::new(m))));
            }
            Value::Native(Native::DateCtor) => {
                // new Date(...) 는 NewTarget 이 정의됨 → 인스턴스 생성. Date(...)(new 없이)는
                // call_native 경로에서 new_target 이 None 이라 문자열을 낸다 (§21.4.2.1/.2).
                self.new_target = Some(Value::Native(Native::DateCtor));
                let r = self.call_native(Native::DateCtor, None, args);
                self.new_target = None;
                return r;
            }
            Value::Native(Native::DomParserCtor) => {
                return self.call_native(Native::DomParserCtor, None, args)
            }
            // new Promise(executor): pending promise 생성 후 executor(resolve, reject) 동기 실행.
            // executor 가 throw 하면 reject. (동기 모델 — resolve/reject 즉시 정착)
            Value::Native(Native::PromiseCtor) => {
                let p = self.new_promise();
                let resolve = Value::Bound(Rc::new((
                    Value::Native(Native::PromiseSettleResolve),
                    p.clone(),
                    vec![],
                )));
                let reject = Value::Bound(Rc::new((
                    Value::Native(Native::PromiseSettleReject),
                    p.clone(),
                    vec![],
                )));
                let executor = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Err(e) = self.call_value(executor, None, vec![resolve, reject.clone()]) {
                    // executor throw → reject (스텝 한도는 제외)
                    if !e.starts_with(STEP_LIMIT_MSG) {
                        let err = match self.thrown.take() {
                            Some(v) => v,
                            None => self.error_from_msg(&e),
                        };
                        let _ = self.call_value(reject, None, vec![err]);
                    } else {
                        return Err(e);
                    }
                }
                return Ok(p);
            }
            Value::Native(Native::UrlCtor) => return self.make_url(args),
            Value::Native(Native::XhrCtor) => return Ok(self.make_xhr()),
            // new WebSocket(url) — 진짜로 연결한다. 등록을 빼먹으면 폴백이 엉뚱한 빈
            // 객체를 만들어 readyState/send 가 통째로 undefined 가 된다.
            Value::Native(Native::WebSocketCtor) => {
                return self.call_native(Native::WebSocketCtor, None, args)
            }
            // new (boundFn)() — Reflect.construct 의 bind 트릭 지원
            Value::Bound(b) => {
                let (target, _this, partial) = (*b).clone();
                let mut all = partial;
                all.extend(args);
                return self.construct(target, all);
            }
            Value::Native(Native::ErrorCtor(name)) => {
                // AggregateError(errors, message, options) (§20.5.7.1): 1번째 인자가
                // message 가 아니라 errors 이터러블이다 → 배열로 모아 .errors(비열거)에.
                // message 는 2번째. 일반 Error 는 1번째가 message.
                let msg_idx = if name == "AggregateError" { 1 } else { 0 };
                let msg = match args.get(msg_idx) {
                    None | Some(Value::Undefined) => None,
                    Some(v) => Some(to_display(v)),
                };
                let err = self.make_error(name, msg);
                if name == "AggregateError" {
                    let errors_arg = args.first().cloned().unwrap_or(Value::Undefined);
                    let list = self.iterate_to_vec(&errors_arg)?;
                    if let Value::Obj(m) = &err {
                        let mut b = m.borrow_mut();
                        b.insert("errors".to_string(), Value::Arr(ArrayObj::new(list)));
                        b.insert(nonenum_marker("errors"), Value::Bool(true));
                    }
                }
                self.install_error_cause(&err, &args, name)?;
                return Ok(err);
            }
            // 네이티브 생성자 스텁: new Error('m') / new Object() 등 → 객체
            // new f() — 일반 함수를 생성자로 (ES6 이전 패턴, 미니파이 코드 다수).
            // 새 객체의 __proto__ 를 f.prototype 에 '링크'(스냅샷 복사 아님) → 이후
            // F.prototype.m 추가도 인스턴스에 반영되고 프로토타입 체인 조회가 동작한다.
            // 함수가 객체를 반환하면 그것 우선(JS 규칙).
            Value::Fn(func) => {
                let obj = Rc::new(RefCell::new(ObjMap::new()));
                // new.target: 명시적(Reflect.construct 3번째 인자·Proxy 위임)이면 그것,
                // 아니면 이 함수. §10.1.13 OrdinaryCreateFromConstructor 는 인스턴스
                // [[Prototype]] 을 Get(newTarget,"prototype")에서 얻는다 — Reflect.construct
                // 로 다른 newTarget 을 주면 그 프로토타입이 링크돼야 한다.
                let new_target = pending_new_target.unwrap_or_else(|| Value::Fn(func.clone()));
                // member_get 이 함수 prototype 을 지연 생성({constructor:F})하고 그 Rc 를
                // 돌려준다(스냅샷 아님) → 이후 F.prototype.m 추가도 인스턴스에 반영된다.
                // newTarget.prototype 이 원시값이면 %Object.prototype%.
                let proto = {
                    let p = self.member_get(&new_target, "prototype")?;
                    if is_object(&p) {
                        p
                    } else {
                        self.member_get(&self.object_ns.clone(), "prototype")?
                    }
                };
                obj.borrow_mut().insert("__proto__".to_string(), proto);
                let this = Value::Obj(obj);
                // new.target 을 심는다 (call_value 가 스코프에 넣고 take 한다).
                self.new_target = Some(new_target);
                let ret = self.call_value(Value::Fn(func), Some(this.clone()), args)?;
                // 표준: 생성자가 객체를 반환하면 그게 결과, 원시값이면 this.
                return Ok(if is_object(&ret) { ret } else { this });
            }
            // 생성자가 아닌 내장(메서드/전역함수/Symbol/BigInt) 을 new 하면 TypeError
            // (§ IsConstructor). 예전엔 아래 폴백이 {message} 스텁 객체를 만들어 조용히
            // 통과시켰다 — not-a-constructor 검사가 전 서브셋에서 대량 실패했다.
            Value::Native(n) if !natives::native_is_constructor(&n) => {
                let name = self.native_fn_name(&Value::Native(n));
                let label = if name.is_empty() { "value".to_string() } else { name };
                return Err(self.throw_error("TypeError", format!("{} is not a constructor", label)));
            }
            // §10.5.13 [[Construct]] — 생성 가능한 프록시의 construct 트랩.
            Value::Proxy(p) => {
                self.proxy_revoked_guard(&p)?;
                let (t, h) = (p.0.clone(), p.1.clone());
                if !self.is_constructor(&t) {
                    return Err(
                        self.throw_error("TypeError", "proxy target is not a constructor")
                    );
                }
                // newTarget: 명시적(Reflect.construct 3번째 인자)이면 그것, 아니면
                // new proxy() 처럼 프록시 자신.
                let new_target =
                    pending_new_target.unwrap_or_else(|| Value::Proxy(p.clone()));
                let trap = self.member_get(&h, "construct")?;
                if matches!(trap, Value::Undefined | Value::Null) {
                    // 위임: target.[[Construct]](args, newTarget) — newTarget 을 넘긴다.
                    self.new_target = Some(new_target);
                    return self.construct(t, args);
                }
                if !is_callable(&trap) {
                    return Err(self.throw_error("TypeError", "'construct' trap is not callable"));
                }
                let arg_array = Value::Arr(ArrayObj::new(args));
                let new_obj =
                    self.call_value(trap, Some(h), vec![t, arg_array, new_target])?;
                // 결과가 객체가 아니면 TypeError(§10.5.13 step 9).
                if !is_object(&new_obj) {
                    return Err(self.throw_error(
                        "TypeError",
                        "proxy 'construct' trap must return an object",
                    ));
                }
                return Ok(new_obj);
            }
            Value::Obj(_) | Value::Native(_) => {
                let mut map = ObjMap::new();
                if let Some(a0) = args.first() {
                    map.insert("message".to_string(), a0.clone());
                }
                return Ok(Value::Obj(Rc::new(RefCell::new(map))));
            }
            other => {
                return Err(
                    self.throw_error("TypeError", format!("{} is not a constructor", to_display(&other)))
                );
            }
        };
        let inst = Value::Instance(Rc::new(Instance {
            class: cls.clone(),
            fields: RefCell::new(ObjMap::new()),
        }));
        // 클래스 필드 초기화(조상 → 자신 순) 후 생성자 실행
        self.init_fields(&cls, &inst)?;
        match self.run_constructor(&cls, &inst, args)? {
            Some(obj) => Ok(obj), // 생성자/super() 가 만들어낸 객체가 결과다
            None => Ok(inst),
        }
    }

    // 클래스 필드를 인스턴스에 초기화. 조상 먼저, this=인스턴스로 초기화식 평가.
    fn init_fields(&mut self, cls: &Rc<JsClass>, inst: &Value) -> Result<(), String> {
        if let Some(parent) = &cls.parent {
            self.init_fields(parent, inst)?;
        }
        for (name, init_fn) in &cls.fields {
            let v = match init_fn {
                Some(f) => self.call_value(Value::Fn(f.clone()), Some(inst.clone()), vec![])?,
                None => Value::Undefined,
            };
            if let Value::Instance(i) = inst {
                // 저장 키는 **그 필드를 선언한 클래스**의 private 스코프를 쓴다.
                // self.priv_id 는 초기화 함수가 끝나며 복원돼 있어 쓰면 안 된다.
                i.fields.borrow_mut().insert(field_key(name, cls.priv_id), v);
            }
        }
        Ok(())
    }

    // 생성자 실행 (super() 는 명시 호출로 부모 생성자 실행 — 자동 체인 아님, ES 동일)
    // 생성자를 실행하고, 생성자가 객체를 반환했으면 그 객체를 돌려준다.
    // 표준: 파생 클래스의 this 는 super() 가 만들어낸 객체다 — 반환값을 버리면
    // this 가 진짜 대상(예: 커스텀 엘리먼트의 DOM 노드)이 되지 못한다.
    fn run_constructor(
        &mut self,
        cls: &Rc<JsClass>,
        inst: &Value,
        args: Vec<Value>,
    ) -> Result<Option<Value>, String> {
        match &cls.ctor {
            Some(ctor) => {
                let scope = Env::new(Some(ctor.env.clone()));
                env_declare(&scope, "this", inst.clone());
                // 클래스 생성자는 항상 new 로 실행 → new.target 은 이 클래스.
                env_declare(&scope, "\u{0}newtarget", Value::Class(cls.clone()));
                // super 참조용: 현재 클래스의 부모를 스코프에 숨겨둠.
                // 부모가 클래스가 아니면(Error/함수 등) 그 생성자 값을 그대로 둔다.
                if let Some(parent) = &cls.parent {
                    env_declare(&scope, "\u{0}superclass__", Value::Class(parent.clone()));
                } else if let Some(pc) = &cls.parent_ctor {
                    env_declare(&scope, "\u{0}superclass__", pc.clone());
                }
                // 파생 클래스면 super() 호출 여부를 추적한다(§15.7.14: 파생 생성자는
                // super() 로 this 를 초기화해야 하고, 안 하고 반환하면 this 미초기화다).
                let is_derived = cls.parent.is_some() || cls.parent_ctor.is_some();
                if is_derived {
                    env_declare(&scope, "\u{0}super_called", Value::Bool(false));
                }
                for (i, p) in ctor.params.iter().enumerate() {
                    env_declare(&scope, p, args.get(i).cloned().unwrap_or(Value::Undefined));
                }
                let flow = self.exec_block(&ctor.body, &scope)?;
                // 생성자 본문이 객체를 반환했거나, super() 가 this 를 갈아끼웠다면 그것이 결과다
                if let Flow::Return(v) = &flow {
                    if is_object(v) {
                        return Ok(Some(v.clone()));
                    }
                    // 파생 생성자가 객체도 undefined 도 아닌 값(원시값)을 반환하면 TypeError
                    // (§10.2.2 step 12.c) — this 미초기화(ReferenceError)보다 먼저 검사한다.
                    if is_derived && !matches!(v, Value::Undefined) {
                        return Err(self.throw_error(
                            "TypeError",
                            "Derived constructors may only return an object or undefined",
                        ));
                    }
                }
                // 파생 생성자인데 super() 를 안 불렀고 객체도 안 돌려줬으면 this 가
                // 미초기화 상태 → ReferenceError (§10.2.2 [[Construct]] step 13).
                if is_derived
                    && !matches!(env_get(&scope, "\u{0}super_called"), Some(Value::Bool(true)))
                {
                    return Err(self.throw_error(
                        "ReferenceError",
                        "Must call super constructor in derived class before accessing 'this' or returning from derived constructor",
                    ));
                }
                if let Some(t) = env_get(&scope, "this") {
                    if is_object(&t) && !matches!((&t, inst), (Value::Instance(a), Value::Instance(b)) if Rc::ptr_eq(a, b))
                    {
                        return Ok(Some(t));
                    }
                }
            }
            None => {
                // 암묵 생성자 constructor(...a){ super(...a) } (표준 §15.7.10).
                if let Some(parent) = &cls.parent {
                    return self.run_constructor(parent, inst, args);
                }
                // 부모가 클래스가 아닌 생성자(Error/함수/EventTarget)여도 super(...args) 는
                // 돈다. 예전엔 이 경로가 없어서 class F extends Error {} 의 message 가
                // 통째로 사라졌다 (new F('x').message === undefined).
                if let Some(pc) = &cls.parent_ctor {
                    // 암묵 super(...args) 도 부모를 현재 클래스를 new.target 으로 호출한다
                    // (§10.2.2). 부모가 new.target 검사 함수(Iterator 등)면 필수.
                    self.new_target = Some(Value::Class(cls.clone()));
                    let produced =
                        self.call_value(pc.clone(), Some(inst.clone()), args)?;
                    match produced {
                        Value::Dom(_) => return Ok(Some(produced)),
                        v if is_object(&v) => {
                            for (k, val) in builtins::own_entries_all(&v) {
                                self.set_own_property(inst, k, val);
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Ok(None)
    }

    fn make_class(&mut self, def: &crate::js::ast::ClassDef, env: &EnvRef) -> Result<Value, String> {
        // 이 클래스 평가마다 **새 private 스코프**를 만든다 (§6.2.12).
        // 같은 이름 #x 라도 클래스가 다르면 다른 private 이름이다.
        self.priv_counter += 1;
        let priv_id = self.priv_counter;
        let outer_priv = self.priv_id;
        self.priv_id = priv_id; // 클래스 본문(메서드/필드 초기화)은 이 스코프 안이다
        let result = self.make_class_inner(def, env, priv_id);
        self.priv_id = outer_priv;
        result
    }

    fn make_class_inner(
        &mut self,
        def: &crate::js::ast::ClassDef,
        env: &EnvRef,
        priv_id: u64,
    ) -> Result<Value, String> {
        // 부모는 클래스일 수도, 일반 생성자(함수/네이티브/Array 같은 네임스페이스 객체)일
        // 수도 있다 — 표준은 아무 생성자나 확장 가능(class E extends Error 가 대표).
        let (parent, parent_ctor): (Option<Rc<JsClass>>, Option<Value>) = match &def.parent {
            Some(e) => {
                let v = self.eval(e, env)?;
                // extends 값은 생성자 또는 null 이어야 한다(§15.7.14). arrow/제너레이터/async/
                // 메서드 함수 및 그런 것을 감싼 bound/proxy 는 [[Construct]] 가 없어 TypeError.
                if Self::is_non_constructor(&v) {
                    return Err(self.throw_error(
                        "TypeError",
                        "Class extends value is not a constructor or null".to_string(),
                    ));
                }
                match v {
                    Value::Class(c) => (Some(c), None),
                    v @ (Value::Fn(_) | Value::Native(_) | Value::Obj(_) | Value::Bound(_)) => {
                        (None, Some(v))
                    }
                    // class X extends null: 유효(§15.7.14) — protoParent=null,
                    // constructorParent=%FunctionPrototype%. parent_ctor=Null 로 표현한다.
                    Value::Null => (None, Some(Value::Null)),
                    other => return Err(format!("{} 은(는) 확장할 클래스가 아님", to_display(&other))),
                }
            }
            None => (None, None),
        };
        // 클래스 본문 전용 스코프: 클래스 이름의 내부 바인딩이 여기 산다 (§15.7.14).
        // 메서드가 자기 클래스를 이름으로 참조할 수 있어야 한다 (클래스 표현식 포함).
        // 예전엔 static 필드 초기화에만 있어서, 메서드 안의 E.#s 가 ReferenceError 였다.
        let class_env = Env::new(Some(env.clone()));
        let mk = |params: &Vec<String>,
                  body: &Vec<Stmt>,
                  is_generator: bool,
                  is_async: bool,
                  source: Option<std::rc::Rc<str>>,
                  prologue_len: usize| {
            Rc::new(JsFn {
                // 클래스 본문의 함수들은 이 클래스의 private 스코프 안에 있다
                priv_id: std::cell::Cell::new(priv_id),
                name: RefCell::new(String::new()),
                params: params.clone(),
                body: body.clone(),
                param_prologue_len: prologue_len,
                env: class_env.clone(),
                is_arrow: false,
                is_generator,
                is_async,
                is_method: true,
                this: None,
                // super.x → 이 클래스의 부모 (클래스 또는 일반 생성자)
                super_class: parent
                    .clone()
                    .map(Value::Class)
                    .or_else(|| parent_ctor.clone()),
                props: RefCell::new(ObjMap::new()),
                source, // 메서드별 소스 텍스트 (§20.2.3.5)
            })
        };
        // 메서드/접근자도 이름을 갖는다 (§15.4): 접근자는 "get x" / "set x" (§10.2.9)
        let named = |f: Rc<JsFn>, n: &str| -> Rc<JsFn> {
            *f.name.borrow_mut() = n.to_string();
            f
        };
        let ctor = def.ctor.as_ref().map(|(p, b, plen)| named(mk(p, b, false, false, None, *plen), def.name.as_deref().unwrap_or("")));
        // 계산된 멤버 키([x](){}, get [k](){})는 클래스 정의 시 1회 평가해 실제 키로 쓴다.
        // computed 가 Some 이면 그 식을 평가한 값이 키, None 이면 정적 이름을 그대로 쓴다.
        let mut methods = HashMap::new();
        for (name, p, b, gen, asy, src, plen, computed) in &def.methods {
            let key = match computed { Some(e) => key_of(&self.eval(e, &class_env)?), None => name.clone() };
            methods.insert(key.clone(), named(mk(p, b, *gen, *asy, src.clone(), *plen), &key));
        }
        let mut getters = HashMap::new();
        let mut setters = HashMap::new();
        for (name, p, b, src, computed) in &def.setters {
            let key = match computed { Some(e) => key_of(&self.eval(e, &class_env)?), None => name.clone() };
            setters.insert(key.clone(), named(mk(p, b, false, false, src.clone(), 0), &format!("set {}", key)));
        }
        let mut static_getters = HashMap::new();
        for (name, p, b, src, computed) in &def.static_getters {
            let key = match computed { Some(e) => key_of(&self.eval(e, &class_env)?), None => name.clone() };
            static_getters
                .insert(key.clone(), named(mk(p, b, false, false, src.clone(), 0), &format!("get {}", key)));
        }
        let mut static_setters = HashMap::new();
        for (name, p, b, src, computed) in &def.static_setters {
            let key = match computed { Some(e) => key_of(&self.eval(e, &class_env)?), None => name.clone() };
            static_setters
                .insert(key.clone(), named(mk(p, b, false, false, src.clone(), 0), &format!("set {}", key)));
        }
        for (name, p, b, src, computed) in &def.getters {
            let key = match computed { Some(e) => key_of(&self.eval(e, &class_env)?), None => name.clone() };
            getters.insert(key.clone(), named(mk(p, b, false, false, src.clone(), 0), &format!("get {}", key)));
        }
        // 인스턴스 필드: 초기화식을 무인자 함수로 감싸(this 바인딩+env) 생성 시 호출.
        // computed 키([x]=v)는 클래스 정의 시 1회 평가해 실제 키로 쓴다(§15.7.14).
        let mut fields = Vec::new();
        for (name, init, computed) in &def.fields {
            let key = match computed {
                Some(e) => key_of(&self.eval(e, &class_env)?),
                None => name.clone(),
            };
            let f = init
                .as_ref()
                .map(|e| mk(&Vec::new(), &vec![Stmt::Return(Some(e.clone()))], false, false, None, 0));
            fields.push((key, f));
        }
        // 정적 멤버는 parent 가 cls 로 이동하기 전에 만든다 (mk 가 parent 참조)
        let mut statics = HashMap::new();
        for (name, p, b, gen, asy, src, plen, computed) in &def.statics {
            let key = match computed { Some(e) => key_of(&self.eval(e, &class_env)?), None => name.clone() };
            let f = named(mk(p, b, *gen, *asy, src.clone(), *plen), &key);
            statics.insert(key.clone(), Value::Fn(f));
            // static **메서드**는 비열거다 (§15.7.14). static **필드**는 열거 가능하다 —
            // 그래서 구분해서 표시한다. 예전엔 둘 다 같은 맵에 섞여 구분이 없었다.
            statics.insert(nonenum_marker(&key), Value::Bool(true));
        }
        let cls = Rc::new(JsClass {
            priv_id,
            proto_cache: RefCell::new(None),
            // 익명 클래스의 이름은 "" 다 (표준). NamedEvaluation 이 나중에 채운다.
            name: RefCell::new(def.name.clone().unwrap_or_default()),
            parent,
            parent_ctor,
            ctor,
            methods,
            getters,
            fields,
            statics: RefCell::new(statics),
            setters,
            static_getters,
            static_setters,
            source: def.source.clone(),
        });
        // 클래스 이름 바인딩을 본문 스코프에 심는다 (메서드가 이제 이걸 본다)
        if let Some(n) = &def.name {
            env_declare(&class_env, n, Value::Class(cls.clone()));
        }
        // static 필드: 클래스 완성 후 this=클래스로 평가해 statics 에 설정.
        // computed 키는 클래스 정의 시 평가(§15.7.14).
        for (name, init, computed) in &def.static_fields {
            let key = match computed {
                Some(e) => key_of(&self.eval(e, &class_env)?),
                None => name.clone(),
            };
            let v = match init {
                Some(e) => {
                    let scope = Env::new(Some(env.clone()));
                    env_declare(&scope, "this", Value::Class(cls.clone()));
                    // 클래스 본문 안에는 클래스 이름의 내부 바인딩이 있다 (표준 §15.7.14).
                    // static 블록/필드가 자기 클래스를 이름으로 참조할 수 있어야 한다.
                    if let Some(n) = &def.name {
                        env_declare(&scope, n, Value::Class(cls.clone()));
                    }
                    self.eval(e, &scope)?
                }
                None => Value::Undefined,
            };
            cls.statics.borrow_mut().insert(key, v);
        }
        Ok(Value::Class(cls))
    }


    // ToPrimitive: 객체를 원시값으로 (valueOf/toString 호출). prefer_string 이면 toString 먼저.
    // 원시값은 그대로. 사용자 정의 toString/valueOf(BigNumber/moment/커스텀 값형)를 존중.
    fn to_primitive(&mut self, v: Value, prefer_string: bool) -> Value {
        // 함수도 객체다 — ToPrimitive(fn) 은 toString 을 타 소스 텍스트를 낸다
        // (`"" + fn`/`String(fn)`/`${fn}`). 예전엔 Fn 을 원시로 봐 to_display 가
        // "function" 을 냈다.
        if !matches!(
            v,
            Value::Obj(_)
                | Value::Instance(_)
                | Value::Arr(_)
                | Value::Fn(_)
                | Value::Native(_)
                | Value::Class(_)
                | Value::Bound(_)
        ) {
            return v;
        }
        // 원시 래퍼(new String/Number/Boolean)도 OrdinaryToPrimitive 를 탄다 — 예전엔
        // 내부 슬롯으로 즉시 단락했지만, valueOf/toString 이 오버라이드되면 슬롯과 달라진다
        // (new Number(42) 의 valueOf 를 2 로 바꾸면 + 연산/JSON 이 2 를 봐야 한다).
        // Symbol.toPrimitive 가 있으면 그것이 우선한다 (표준 §7.1.1).
        if let Ok(f) = self.member_get(&v, "\u{0}@@toPrimitive") {
            if is_callable(&f) {
                let hint = Value::Str(if prefer_string { "string" } else { "number" }.to_string());
                if let Ok(res) = self.call_value(f, Some(v.clone()), vec![hint]) {
                    // 원시값(비객체)이면 채택. 함수(Fn/Native/…)도 객체라 원시 아님.
                    if !is_object(&res) {
                        return res;
                    }
                }
            }
        }
        let order: [&str; 2] =
            if prefer_string { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for m in order {
            if let Ok(f) = self.member_get(&v, m) {
                if is_callable(&f) {
                    if let Ok(res) = self.call_value(f, Some(v.clone()), vec![]) {
                        if !is_object(&res) {
                            return res; // 원시값이면 채택
                        }
                    }
                }
            }
        }
        v
    }

    // ToPrimitive (§7.1.1) 의 예외 전파판. to_primitive 는 관대 모드라 toString/valueOf 가
    // 던진 예외를 삼키지만, 표준 강제변환(String()/String.prototype method 의 this 등)은
    // poisoned toString/valueOf 를 그대로 전파해야 한다. 원시값을 못 얻으면 TypeError.
    pub(super) fn to_primitive_or_throw(
        &mut self,
        v: Value,
        prefer_string: bool,
    ) -> Result<Value, String> {
        self.to_primitive_hint(v, if prefer_string { "string" } else { "number" })
    }

    // ToPrimitive (§7.1.1) — 힌트 문자열("default"/"number"/"string")을 그대로 @@toPrimitive
    // 에 전달한다. Date 생성자의 1인자는 "default" 힌트를 요구하므로 bool 로는 부족했다.
    pub(super) fn to_primitive_hint(&mut self, v: Value, hint: &str) -> Result<Value, String> {
        if !matches!(
            v,
            Value::Obj(_)
                | Value::Instance(_)
                | Value::Arr(_)
                | Value::Fn(_)
                | Value::Native(_)
                | Value::Class(_)
                | Value::Bound(_)
        ) {
            return Ok(v);
        }
        // 원시 래퍼도 OrdinaryToPrimitive 를 탄다(오버라이드된 valueOf/toString 관측).
        // 예전엔 슬롯으로 단락해 오버라이드를 무시했다.
        let prim = |res: &Value| !is_object(res);
        // GetMethod(@@toPrimitive) (§7.3.11): 접근자 abrupt 전파(? 연산) + undefined/null 이
        // 아닌데 callable 이 아니면 TypeError. 예전엔 if-let Ok 로 삼키거나 non-callable 을
        // 조용히 valueOf 로 넘겼다.
        let exotic = self.member_get(&v, "\u{0}@@toPrimitive")?;
        if !matches!(exotic, Value::Undefined | Value::Null) {
            if !is_callable(&exotic) {
                return Err(self.throw_error("TypeError", "@@toPrimitive value is not callable"));
            }
            let res = self.call_value(exotic, Some(v.clone()), vec![Value::Str(hint.to_string())])?;
            if prim(&res) {
                return Ok(res);
            }
            return Err(self.throw_error("TypeError", "Cannot convert object to primitive value"));
        }
        // OrdinaryToPrimitive: "string" 힌트만 toString 우선, 나머지("default"/"number")는 valueOf 우선.
        let order: [&str; 2] =
            if hint == "string" { ["toString", "valueOf"] } else { ["valueOf", "toString"] };
        for m in order {
            let f = self.member_get(&v, m)?;
            if is_callable(&f) {
                let res = self.call_value(f, Some(v.clone()), vec![])?; // 예외 전파
                if prim(&res) {
                    return Ok(res);
                }
            }
        }
        Err(self.throw_error("TypeError", "Cannot convert object to primitive value"))
    }

    // §7.1.4 ToNumber(argument). Symbol/BigInt 은 TypeError, 객체는 ToPrimitive(number) 후
    // 재귀(사용자 valueOf/toString/@@toPrimitive 호출 및 예외 전파). 원시는 to_num.
    pub(super) fn to_number_value(&mut self, v: &Value) -> Result<f64, String> {
        match v {
            Value::Symbol(_) => {
                Err(self.throw_error("TypeError", "Cannot convert a Symbol value to a number"))
            }
            Value::BigInt(_) => {
                Err(self.throw_error("TypeError", "Cannot convert a BigInt value to a number"))
            }
            Value::Obj(_)
            | Value::Instance(_)
            | Value::Arr(_)
            | Value::Fn(_)
            | Value::Native(_)
            | Value::Class(_)
            | Value::Bound(_) => {
                let p = self.to_primitive_or_throw(v.clone(), false)?;
                self.to_number_value(&p)
            }
            _ => Ok(to_num(v)),
        }
    }

    // §7.1.17 ToString(argument). Symbol 은 TypeError, 객체는 ToPrimitive(string) 후 재귀
    // (사용자 toString/valueOf/@@toPrimitive 호출·예외 전파). 원시는 to_display.
    pub(super) fn to_string_value(&mut self, v: &Value) -> Result<String, String> {
        match v {
            Value::Symbol(_) => {
                Err(self.throw_error("TypeError", "Cannot convert a Symbol value to a string"))
            }
            Value::Obj(_)
            | Value::Instance(_)
            | Value::Arr(_)
            | Value::Fn(_)
            | Value::Native(_)
            | Value::Class(_)
            | Value::Bound(_) => {
                let p = self.to_primitive_or_throw(v.clone(), true)?;
                self.to_string_value(&p)
            }
            _ => Ok(to_display(v)),
        }
    }

    // §7.1.6 ToInt32(argument) = to_i32(ToNumber). ToNumber 가 객체 valueOf 를 호출하고
    // Symbol/BigInt 은 TypeError. 비트 연산(|/&/^/<</>>/~)이 이걸 써야 valueOf 관측·표준 오류.
    pub(super) fn to_int32(&mut self, v: &Value) -> Result<i32, String> {
        Ok(to_i32_from_num(self.to_number_value(v)?))
    }

    // §7.1.5 ToIntegerOrInfinity(argument). NaN→0, ±∞ 유지, 그 밖은 0 방향 절단.
    pub(super) fn to_integer_or_infinity(&mut self, v: &Value) -> Result<f64, String> {
        let n = self.to_number_value(v)?;
        Ok(if n.is_nan() {
            0.0
        } else if n.is_infinite() {
            n
        } else {
            n.trunc()
        })
    }

    // ToPropertyKey (§7.1.19): 값을 프로퍼티 키 문자열로. Symbol 은 내부 키 표현으로,
    // 그 외는 ToPrimitive(hint string) 후 ToString. 객체 키의 toString 을 실제로 부르고
    // 예외도 전파한다 — 예전엔 to_display 라 {toString(){return 'k'}} 가 "[object Object]"
    // 로 뭉개졌다.
    pub(super) fn to_property_key(&mut self, v: Value) -> Result<String, String> {
        if let Value::Symbol(s) = &v {
            return Ok(s.key.clone());
        }
        let prim = self.to_primitive_or_throw(v, true)?;
        if let Value::Symbol(s) = &prim {
            return Ok(s.key.clone());
        }
        Ok(key_of(&prim))
    }

    // IsRegExp (§7.2.8): Symbol.match 가 있으면 그 truthiness, 없으면 [[RegExpMatcher]]
    // 슬롯(우리 표현: __isRegex) 유무. startsWith/includes/endsWith 가 정규식 인자를
    // 거부(§22.1.3.7 등)하는 데 쓴다.
    // IsRegExp (§22.1.4.1) 의 예외 전파판: @@match 접근자가 던지면 그대로 전파한다.
    pub(super) fn is_regexp_p(&mut self, v: &Value) -> Result<bool, String> {
        if !matches!(v, Value::Obj(_)) {
            return Ok(false);
        }
        let m = self.member_get(v, "\u{0}@@match")?;
        if !matches!(m, Value::Undefined) {
            return Ok(to_bool(&m));
        }
        Ok(regex_src_flags(v).is_some())
    }

    // BigInt 연산 (표준 §6.1.6.2). 두 피연산자가 모두 BigInt 여야 산술이 된다 —
    // Number 와 섞으면 TypeError (조용히 f64 로 떨어뜨리면 값이 틀린다).
    // 비교(<,>,<=,>=,==)와 문자열 결합은 섞어도 된다.
    fn bigint_binary(&mut self, op: BinOp, l: &Value, r: &Value) -> Option<Result<Value, String>> {
        use crate::js::bigint::BigInt as BI;
        let (lb, rb) = (matches!(l, Value::BigInt(_)), matches!(r, Value::BigInt(_)));
        if !lb && !rb {
            return None;
        }
        let big = |b: BI| Ok(Value::BigInt(Rc::new(b)));
        let type_err = |me: &mut Self| -> Result<Value, String> {
            Err(me.throw_error(
                "TypeError",
                "Cannot mix BigInt and other types, use explicit conversions",
            ))
        };
        // 문자열이 끼면 + 는 결합 (표준)
        if matches!(op, BinOp::Add) && (matches!(l, Value::Str(_)) || matches!(r, Value::Str(_))) {
            return Some(Ok(Value::Str(format!("{}{}", to_display(l), to_display(r)))));
        }
        // 비교는 섞어도 된다 (수치 비교). == 는 값 비교(1n == 1 은 true),
        // === 는 타입이 달라 false (strict_eq 가 처리).
        if matches!(op, BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge | BinOp::EqEq | BinOp::NotEq) {
            // 둘 다 BigInt 면 정확히 비교한다. f64 변환은 2^53 초과에서 정밀도를 잃어
            // (2^63-1) >= 2^63 이 참이 되는 등 큰 값에서 틀렸다 — 편법이었다.
            if let (Value::BigInt(a), Value::BigInt(b)) = (l, r) {
                use std::cmp::Ordering;
                let ord = a.cmp_to(b);
                let res = match op {
                    BinOp::Lt => ord == Ordering::Less,
                    BinOp::Gt => ord == Ordering::Greater,
                    BinOp::Le => ord != Ordering::Greater,
                    BinOp::Ge => ord != Ordering::Less,
                    BinOp::EqEq => ord == Ordering::Equal,
                    _ => ord != Ordering::Equal,
                };
                return Some(Ok(Value::Bool(res)));
            }
            // 혼합(BigInt/Number)은 수치 비교 (Number 가 작으면 정밀 손실 없음).
            let (x, y) = (to_num(l), to_num(r));
            let res = match op {
                BinOp::Lt => x < y,
                BinOp::Gt => x > y,
                BinOp::Le => x <= y,
                BinOp::Ge => x >= y,
                BinOp::EqEq => x == y,
                _ => x != y,
            };
            return Some(Ok(Value::Bool(res)));
        }
        // === / !== 는 타입까지 본다
        if matches!(op, BinOp::EqEqEq | BinOp::NotEqEq) {
            let eq = strict_eq(l, r);
            return Some(Ok(Value::Bool(if matches!(op, BinOp::EqEqEq) { eq } else { !eq })));
        }
        // 산술/비트: 둘 다 BigInt 여야 한다
        let (Value::BigInt(a), Value::BigInt(b)) = (l, r) else {
            return Some(type_err(self));
        };
        let (a, b) = (a.clone(), b.clone());
        Some(match op {
            BinOp::Add => big(a.add(&b)),
            BinOp::Sub => big(a.sub(&b)),
            BinOp::Mul => big(a.mul(&b)),
            BinOp::Div => match a.checked_divrem(&b) {
                Some((q, _)) => big(q),
                None => Err("RangeError: Division by zero".to_string()),
            },
            BinOp::Mod => match a.checked_divrem(&b) {
                Some((_, r)) => big(r),
                None => Err("RangeError: Division by zero".to_string()),
            },
            BinOp::Pow => match a.pow(&b) {
                Some(p) => big(p),
                None => Err("RangeError: Exponent must be non-negative".to_string()),
            },
            BinOp::BitAnd => big(a.bitand(&b)),
            BinOp::BitOr => big(a.bitor(&b)),
            BinOp::BitXor => big(a.bitxor(&b)),
            BinOp::Shl => big(a.shl(&b)),
            BinOp::Shr => big(a.shr(&b)),
            BinOp::UShr => Err("TypeError: BigInts have no unsigned right shift".to_string()),
            _ => Ok(Value::Undefined),
        })
    }

    fn binary(&mut self, op: BinOp, l: Value, r: Value) -> Result<Value, String> {
        // 산술/비교 연산: 객체 피연산자를 원시값으로 강제변환 (ToPrimitive). in/instanceof 제외.
        // abrupt(valueOf/toString 예외)을 전파해야 한다(예전 to_primitive 는 삼켰다).
        // + 는 "default" 힌트, 나머지 산술/관계는 "number" 힌트 (§13.15.3/§13.10.1).
        let (l, r) = match op {
            BinOp::Add => (
                self.to_primitive_hint(l, "default")?,
                self.to_primitive_hint(r, "default")?,
            ),
            BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Mod
            | BinOp::Pow
            | BinOp::Lt
            | BinOp::Gt
            | BinOp::Le
            | BinOp::Ge => (
                self.to_primitive_hint(l, "number")?,
                self.to_primitive_hint(r, "number")?,
            ),
            _ => (l, r),
        };
        // BigInt 가 끼면 별도 의미론 (혼합 산술은 TypeError)
        if let Some(res) = self.bigint_binary(op, &l, &r) {
            return res;
        }
        Ok(match op {
            // + : 한쪽이라도 String 이면 ToString 둘 다(Symbol→TypeError), 아니면 ToNumber
            //     둘 다 (§13.15.3). 예전엔 to_display/to_num 이라 "x"+Symbol()·Symbol()+1 이
            //     안 던졌다.
            BinOp::Add => match (&l, &r) {
                (Value::Str(_), _) | (_, Value::Str(_)) => {
                    Value::Str(format!("{}{}", self.to_string_value(&l)?, self.to_string_value(&r)?))
                }
                _ => Value::Num(self.to_number_value(&l)? + self.to_number_value(&r)?),
            },
            BinOp::Sub => Value::Num(self.to_number_value(&l)? - self.to_number_value(&r)?),
            BinOp::Mul => Value::Num(self.to_number_value(&l)? * self.to_number_value(&r)?),
            BinOp::Div => Value::Num(self.to_number_value(&l)? / self.to_number_value(&r)?),
            BinOp::Mod => Value::Num(self.to_number_value(&l)? % self.to_number_value(&r)?),
            BinOp::Pow => {
                Value::Num(self.to_number_value(&l)?.powf(self.to_number_value(&r)?))
            }
            // 비트 연산은 ToInt32(ToNumber) — 객체 valueOf 관측, Symbol→TypeError.
            // BigInt 는 위 bigint_binary 에서 이미 처리됨.
            BinOp::BitAnd => Value::Num((self.to_int32(&l)? & self.to_int32(&r)?) as f64),
            BinOp::BitOr => Value::Num((self.to_int32(&l)? | self.to_int32(&r)?) as f64),
            BinOp::BitXor => Value::Num((self.to_int32(&l)? ^ self.to_int32(&r)?) as f64),
            BinOp::Shl => {
                Value::Num((self.to_int32(&l)? << (self.to_int32(&r)? & 31)) as f64)
            }
            BinOp::Shr => {
                Value::Num((self.to_int32(&l)? >> (self.to_int32(&r)? & 31)) as f64)
            }
            BinOp::UShr => Value::Num(
                ((self.to_int32(&l)? as u32) >> (self.to_int32(&r)? & 31)) as f64,
            ),
            // in: 프로토타입 체인까지 본다 (표준 §13.10). Proxy 면 has 트랩.
            BinOp::In => {
                // §13.10.2: 키는 ToPropertyKey(객체 toString / Symbol 내부키).
                let key = self.to_property_key(l.clone())?;
                match &r {
                    Value::Proxy(p) => {
                        self.proxy_revoked_guard(p)?;
                        let (target, handler) = (p.0.clone(), p.1.clone());
                        let trap = self.member_get(&handler, "has")?;
                        // GetMethod: undefined/null → 타깃 위임, non-callable → TypeError.
                        if matches!(trap, Value::Undefined | Value::Null) {
                            return self.binary(BinOp::In, l, target);
                        }
                        if !is_callable(&trap) {
                            return Err(self
                                .throw_error("TypeError", "'has' trap is not callable"));
                        }
                        let res = self.call_value(
                            trap,
                            Some(handler),
                            vec![target.clone(), self.trap_key(&key)],
                        )?;
                        let has = to_bool(&res);
                        // §10.5.7 [[HasProperty]] invariant: 트랩이 false 인데 target 에
                        // 그 프로퍼티가 있으면 — non-configurable 이거나 target 이
                        // non-extensible 이면 숨길 수 없다 → TypeError.
                        if !has {
                            let td = self.call_native(
                                Native::ObjectGetOwnPropertyDescriptor,
                                None,
                                vec![target.clone(), Value::Str(key.clone())],
                            )?;
                            if let Value::Obj(d) = &td {
                                let configurable = matches!(d.borrow().get("configurable"), Some(v) if to_bool(v));
                                if !configurable {
                                    return Err(self.throw_error("TypeError", "'has' on proxy: non-configurable property on target cannot be reported as non-existent"));
                                }
                                if !self.value_is_extensible(&target)? {
                                    return Err(self.throw_error("TypeError", "'has' on proxy: existing property of non-extensible target cannot be reported as non-existent"));
                                }
                            }
                        }
                        return Ok(Value::Bool(has));
                    }
                    Value::Obj(m) => {
                        // HasProperty(§7.3.11): own + 프로토타입 체인(배열/함수 프로토 포함).
                        // has_property 로 통일 — 예전엔 Value::Obj 체인만 걸어 배열 프로토타입의
                        // 상속 인덱스('0' in (proto 가 배열인 객체))를 놓쳤다.
                        if self.has_property(&r, &key) {
                            return Ok(Value::Bool(true));
                        }
                        // 전역 객체면 전역 환경의 바인딩도 프로퍼티다
                        Value::Bool(self.global_has(m, &key))
                    }
                    // 인스턴스: 필드 + 클래스 체인의 메서드/게터
                    Value::Instance(inst) => {
                        if inst.fields.borrow().contains_key(&key) {
                            return Ok(Value::Bool(true));
                        }
                        Value::Bool(
                            inst.class.find_method(&key).is_some()
                                || inst.class.find_getter(&key).is_some(),
                        )
                    }
                    // 배열의 in: 인덱스 + length + own 프로퍼티 + Array.prototype 메서드.
                    // 예전엔 인덱스만 봐서 `"length" in []` 가 false 였다 — 그래서
                    // 값이 배열인지 확인하는 코드(testharness 의 assert_array_equals 가
                    // 정확히 이렇게 한다)가 우리 컬렉션을 배열이 아니라고 판정했다.
                    Value::Arr(a) => Value::Bool(
                        // 구멍 인덱스는 own 이 아니다 (상속은 proto_method 로 별도 확인).
                        key.parse::<usize>()
                            .map_or(false, |i| i < a.borrow().len() && !a.is_hole(i))
                            || key == "length"
                            || a.get_prop(&key).is_some()
                            || self.proto_method("Array", &key).is_some(),
                    ),
                    // 함수/내장 생성자 등: HasProperty 로 위임 (name/length/정적/prototype + 상속).
                    other => Value::Bool(self.has_property(other, &key)),
                }
            }
            BinOp::Instanceof => {
                // 표준 §13.10.2: 오른쪽에 [Symbol.hasInstance] 가 있으면 **그것이 최우선**이다.
                // (Symbol.hasInstance 로 instanceof 를 커스터마이즈하는 라이브러리가 있다)
                // 기본 Function.prototype[@@hasInstance](FnHasInstance)는 아래 수동 로직으로
                // OrdinaryHasInstance 를 수행하므로 우회한다(무한 재귀 방지). 사용자 커스텀
                // @@hasInstance 만 여기서 호출한다.
                let hi = self.member_get(&r, "\u{0}@@hasInstance").unwrap_or(Value::Undefined);
                if is_callable(&hi) && !matches!(hi, Value::Native(Native::FnHasInstance)) {
                    let res = self.call_value(hi, Some(r.clone()), vec![l.clone()])?;
                    return Ok(Value::Bool(to_bool(&res)));
                }
                // 사용자 클래스: 인스턴스의 클래스 체인에 r 이 있는가
                if let (Value::Instance(inst), Value::Class(rc)) = (&l, &r) {
                    let mut cur = Some(inst.class.clone());
                    let mut found = false;
                    while let Some(c) = cur {
                        if Rc::ptr_eq(&c, rc) {
                            found = true;
                            break;
                        }
                        cur = c.parent.clone();
                    }
                    return Ok(Value::Bool(found));
                }
                // 클래스가 아닌 부모를 확장한 인스턴스: e instanceof Error 등.
                // 클래스 체인의 parent_ctor 가 r 과 같은 생성자면 참.
                if let Value::Instance(inst) = &l {
                    let mut cur = Some(inst.class.clone());
                    while let Some(c) = cur {
                        if let Some(pc) = &c.parent_ctor {
                            if strict_eq(pc, &r) {
                                return Ok(Value::Bool(true));
                            }
                        }
                        cur = c.parent.clone();
                    }
                }
                // Proxy 인스턴스: [[GetPrototypeOf]] (§10.5.1) 를 따른다. getPrototypeOf 트랩이
                // 있으면 그걸로 첫 프로토타입을 얻고, 없으면 타깃의 프로토타입. 이후 체인은
                // __proto__ 로 올라간다. typed array 가 Proxy 라 instanceof 가 여기 걸린다.
                if let (Value::Proxy(_), Value::Fn(_)) = (&l, &r) {
                    // member_get 으로 .prototype 을 materialize 한다 — 예전엔 props 를
                    // 직접 읽어 아직 안 만들어진 함수 프로토타입이 None → 무조건 false 였다
                    // (getPrototypeOf 트랩이 그 프로토타입을 돌려줘도 instanceof 가 실패).
                    let fp = match self.member_get(&r, "prototype")? {
                        Value::Obj(fp) => fp,
                        _ => return Ok(Value::Bool(false)),
                    };
                    // 첫 프로토타입은 proto_of([[GetPrototypeOf]]) 하나로 얻는다 — 트랩
                    // 호출·GetMethod·non-extensible invariant 가 전부 거기 모여 있어
                    // instanceof 도 같은 규칙을 탄다(트랩 로직 중복 제거).
                    let mut proto = self.proto_of(&l)?;
                    let mut depth = 0;
                    while let Value::Obj(p) = &proto {
                        if Rc::ptr_eq(p, &fp) {
                            return Ok(Value::Bool(true));
                        }
                        depth += 1;
                        if depth > 100 {
                            break;
                        }
                        let next = p.borrow().get("__proto__").cloned().unwrap_or(Value::Null);
                        proto = next;
                    }
                    return Ok(Value::Bool(false));
                }
                // function 생성자: l 의 __proto__ 체인에 F.prototype 이 있으면 인스턴스.
                if let (Value::Obj(lm), Value::Fn(func)) = (&l, &r) {
                    let fp = func.props.borrow().get("prototype").cloned();
                    if let Some(Value::Obj(fp)) = fp {
                        let mut proto = lm.borrow().get("__proto__").cloned();
                        let mut depth = 0;
                        while let Some(Value::Obj(p)) = proto {
                            depth += 1;
                            if depth > 100 {
                                break;
                            }
                            if Rc::ptr_eq(&p, &fp) {
                                return Ok(Value::Bool(true));
                            }
                            proto = p.borrow().get("__proto__").cloned();
                        }
                    }
                    return Ok(Value::Bool(false));
                }
                // 제너레이터: OrdinaryHasInstance — [[Prototype]] 체인(proto_of 로 시작,
                // 이제 __kIterProto)에 생성자의 .prototype 이 있으면 인스턴스.
                // (`gen instanceof Iterator`, map 등 헬퍼 결과의 Iterator 판정.)
                if matches!(l, Value::Gen(_)) && is_callable(&r) {
                    if let Value::Obj(fpm) = self.member_get(&r, "prototype")? {
                        let mut proto = self.proto_of(&l)?;
                        let mut depth = 0;
                        while let Value::Obj(p) = &proto {
                            if Rc::ptr_eq(p, &fpm) {
                                return Ok(Value::Bool(true));
                            }
                            depth += 1;
                            if depth > 100 {
                                break;
                            }
                            let next = p.borrow().get("__proto__").cloned().unwrap_or(Value::Null);
                            proto = next;
                        }
                    }
                    return Ok(Value::Bool(false));
                }
                // 내장 생성자별 값 타입 판정 (feature-detection/에러 처리에 흔함)
                let obj_has = |key: &str| -> bool {
                    matches!(&l, Value::Obj(m) if m.borrow().contains_key(key))
                };
                let is_regex = matches!(&l, Value::Obj(m) if is_regex_obj(m));
                let is_date = matches!(&l, Value::Obj(m) if is_date_obj(m));
                let hit = match &r {
                    Value::Native(Native::MapCtor) => matches!(l, Value::MapVal(_)),
                    Value::Native(Native::SetCtor) => matches!(l, Value::SetVal(_)),
                    Value::Native(Native::RegExpCtor) => is_regex,
                    Value::Native(Native::DateCtor) => is_date,
                    Value::Native(Native::PromiseCtor) => is_promise(&l),
                    Value::Native(Native::UrlCtor) => obj_has("searchParams"),
                    Value::Native(Native::FunctionCtor) => {
                        matches!(l, Value::Fn(_) | Value::Native(_) | Value::Bound(_) | Value::Class(_))
                    }
                    // 이벤트 인터페이스: 프로토타입 체인에 그 인터페이스의 prototype 이
                    // 있는가. 예전엔 전부 같은 EventCtor 라 구분 자체가 불가능했고,
                    // new Event('x') instanceof Event 조차 false 였다.
                    Value::Native(Native::EventCtor(name)) => {
                        match (&l, self.event_proto(name)) {
                            (Value::Obj(lm), Some(Value::Obj(tp))) => {
                                let mut cur = Some(lm.clone());
                                let mut hit = false;
                                let mut depth = 0;
                                while let Some(m) = cur {
                                    if Rc::ptr_eq(&m, &tp) {
                                        hit = true;
                                        break;
                                    }
                                    depth += 1;
                                    if depth > 100 {
                                        break;
                                    }
                                    cur = match m.borrow().get("__proto__") {
                                        Some(Value::Obj(p)) => Some(p.clone()),
                                        _ => None,
                                    };
                                }
                                hit
                            }
                            _ => false,
                        }
                    }
                    // Error 및 서브타입: 프로토타입 체인에 해당 종류의 prototype 이 있는가.
                    // (예전엔 "message 프로퍼티가 있나?" 라는 오리 판별이었다 — 그래서
                    //  {message:'x'} 같은 평범한 객체도 Error 로 통과했다.)
                    Value::Native(Native::ErrorCtor(name)) => {
                        let target = self
                            .error_protos
                            .iter()
                            .find(|(k, _)| k == name)
                            .map(|(_, p)| p.clone());
                        match (&l, target) {
                            (Value::Obj(lm), Some(Value::Obj(tp))) => {
                                let mut cur = Some(lm.clone());
                                let mut hit = false;
                                let mut depth = 0;
                                while let Some(m) = cur {
                                    if Rc::ptr_eq(&m, &tp) {
                                        hit = true;
                                        break;
                                    }
                                    depth += 1;
                                    if depth > 100 {
                                        break;
                                    }
                                    cur = match m.borrow().get("__proto__") {
                                        Some(Value::Obj(p)) => Some(p.clone()),
                                        _ => None,
                                    };
                                }
                                hit
                            }
                            _ => false,
                        }
                    }
                    // Array/Object 는 Native 생성자
                    Value::Native(Native::ArrayCtor) => matches!(l, Value::Arr(_)),
                    Value::Native(Native::ObjectCtor) => matches!(
                        l,
                        Value::Obj(_)
                            | Value::Arr(_)
                            | Value::MapVal(_)
                            | Value::SetVal(_)
                            | Value::Instance(_)
                    ),
                    // 원시 래퍼 생성자(Boolean/Number/String): 래퍼의 __proto__ 체인에
                    // 해당 prototype 이 있는가. 예전엔 new Boolean() instanceof Boolean 조차
                    // false 였다(§20/21/22 프로토타입이 체크 대상이 아니었다).
                    Value::Native(
                        Native::BooleanCtor | Native::NumberCtor | Native::StringCtor,
                    ) => {
                        let target = match &r {
                            Value::Native(Native::BooleanCtor) => self.boolean_proto.clone(),
                            Value::Native(Native::NumberCtor) => self.number_proto.clone(),
                            _ => self.string_proto.clone(),
                        };
                        match (&l, &target) {
                            (Value::Obj(lm), Value::Obj(tp)) => {
                                let mut cur = Some(lm.clone());
                                let mut hit = false;
                                let mut depth = 0;
                                while let Some(m) = cur {
                                    if Rc::ptr_eq(&m, tp) {
                                        hit = true;
                                        break;
                                    }
                                    depth += 1;
                                    if depth > 100 {
                                        break;
                                    }
                                    cur = match m.borrow().get("__proto__") {
                                        Some(Value::Obj(p)) => Some(p.clone()),
                                        _ => None,
                                    };
                                }
                                hit
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                };
                Value::Bool(hit)
            }
            BinOp::EqEq => Value::Bool(loose_eq(&l, &r)),
            BinOp::NotEq => Value::Bool(!loose_eq(&l, &r)),
            BinOp::EqEqEq => Value::Bool(strict_eq(&l, &r)),
            BinOp::NotEqEq => Value::Bool(!strict_eq(&l, &r)),
            BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                // 둘 다 문자열이면 사전순, 아니면 숫자 비교 (JS 유사)
                let b = if let (Value::Str(a), Value::Str(c)) = (&l, &r) {
                    match op {
                        BinOp::Lt => a < c,
                        BinOp::Gt => a > c,
                        BinOp::Le => a <= c,
                        _ => a >= c,
                    }
                } else {
                    // 관계 비교의 수치 변환도 ToNumber (Symbol→TypeError, BigInt 는 위
                    // bigint_binary 에서 이미 처리). 예전 to_num 은 Symbol 을 NaN 으로 삼켰다.
                    let (x, y) = (self.to_number_value(&l)?, self.to_number_value(&r)?);
                    match op {
                        BinOp::Lt => x < y,
                        BinOp::Gt => x > y,
                        BinOp::Le => x <= y,
                        _ => x >= y,
                    }
                };
                Value::Bool(b)
            }
        })
    }

    // 멤버 대입의 실제 수행 (수신자·키가 이미 평가된 상태).
    // 표준 §13.15.2 는 왼쪽 참조를 **먼저** 평가하고 그 다음 오른쪽을 평가하라고 한다.
    // 그래서 참조 평가와 값 대입을 분리한다.
    fn member_assign(&mut self, recv: Value, key: String, value: Value) -> Result<(), String> {
                // private 필드 대입도 그 private 을 선언한 인스턴스에서만 유효(brand check).
                // 없으면 TypeError. 예전엔 o.#x=v(o 미보유)가 조용히 통과했다.
                if is_private_name(&key) {
                    let ok = match &recv {
                        Value::Instance(i) => {
                            i.fields.borrow().contains_key(&field_key(&key, self.priv_id))
                                || i.class.find_method(&key).is_some()
                                || i.class.find_setter(&key).is_some()
                                || i.class.find_getter(&key).is_some()
                        }
                        Value::Class(c) => {
                            c.statics.borrow().contains_key(&key)
                                || c.find_static_setter(&key).is_some()
                                || c.find_static_getter(&key).is_some()
                        }
                        _ => false,
                    };
                    if !ok {
                        return Err(self.throw_error(
                            "TypeError",
                            format!("Cannot write private member {} to an object whose class did not declare it", key),
                        ));
                    }
                }
                match recv {
                    // Proxy: set 트랩 있으면 handler.set(target, key, value, receiver), 없으면 target 에 위임
                    Value::Proxy(p) => {
                        self.proxy_revoked_guard(&p)?;
                        let (target, handler) = (p.0.clone(), p.1.clone());
                        let trap = self.member_get(&handler, "set")?;
                        if !matches!(trap, Value::Undefined) {
                            let receiver = Value::Proxy(p.clone());
                            let btr = self.call_value(
                                trap,
                                Some(handler),
                                vec![
                                    target.clone(),
                                    self.trap_key(&key),
                                    value.clone(),
                                    receiver,
                                ],
                            )?;
                            // 트랩이 falsy 면 설정 실패(sloppy 무시).
                            if !to_bool(&btr) {
                                return Ok(());
                            }
                            // §10.5.9 [[Set]] invariant: target 의 non-configurable
                            // non-writable 데이터는 value 가 SameValue 여야, setter 없는
                            // non-configurable accessor 는 TypeError.
                            let td = self.call_native(
                                Native::ObjectGetOwnPropertyDescriptor,
                                None,
                                vec![target.clone(), Value::Str(key.clone())],
                            )?;
                            if let Value::Obj(d) = &td {
                                let b = d.borrow();
                                let configurable =
                                    matches!(b.get("configurable"), Some(v) if to_bool(v));
                                if !configurable {
                                    if b.contains_key("value") {
                                        let writable =
                                            matches!(b.get("writable"), Some(v) if to_bool(v));
                                        let val =
                                            b.get("value").cloned().unwrap_or(Value::Undefined);
                                        if !writable && !same_value(&value, &val) {
                                            return Err(self.throw_error("TypeError", "'set' on proxy: non-configurable, non-writable data property but trap set a different value"));
                                        }
                                    } else if matches!(
                                        b.get("set").cloned().unwrap_or(Value::Undefined),
                                        Value::Undefined
                                    ) {
                                        return Err(self.throw_error("TypeError", "'set' on proxy: non-configurable accessor without setter"));
                                    }
                                }
                            }
                            return Ok(());
                        }
                        // 트랩 없음 → target(Obj/Arr)에 직접 저장
                        match &target {
                            Value::Obj(map) => {
                                map.borrow_mut().insert(key, value);
                            }
                            Value::Arr(a) => {
                                if let Ok(i) = key.parse::<usize>() {
                                    let mut arr = a.borrow_mut();
                                    if i >= arr.len() {
                                        arr.resize(i + 1, Value::Undefined);
                                    }
                                    arr[i] = value;
                                } else {
                                    a.set_prop(key, value);
                                }
                            }
                            _ => {}
                        }
                        Ok(())
                    }
                    Value::Obj(map) => {
                        // 접근자 프로퍼티면 setter 를 호출한다(own → 프로토타입 체인 순).
                        // 예전엔 setter 를 무시하고 raw 값을 덮어써 조용히 틀렸다.
                        let this_obj = Value::Obj(map.clone());
                        if let Some(acc) = self.find_accessor(&map, &key) {
                            if let Some(setter) = acc.set.clone() {
                                self.call_value(setter, Some(this_obj), vec![value])?;
                                return Ok(());
                            }
                            // get 만 있는 접근자에 대입 → 무시 (sloppy 모드 표준)
                            if acc.get.is_some() {
                                return Ok(());
                            }
                        }
                        // window.x = v 는 전역 변수 x 를 만든다(전역 객체 의미론).
                        let is_window = Rc::ptr_eq(&map, &self.window_obj);
                        // freeze: 변경 금지. seal/preventExtensions: 새 프로퍼티 추가 금지.
                        if self.is_frozen_val(&this_obj) {
                            return Ok(());
                        }
                        let is_new = !map.borrow().contains_key(&key);
                        if is_new && self.is_nonextensible_val(&this_obj) {
                            return Ok(());
                        }
                        // writable:false 데이터 프로퍼티에는 대입이 무시된다 (sloppy 모드 §10.1.9).
                        // 예전엔 서술자를 무시하고 그냥 덮어써서 writable:false 가 허수아비였다.
                        if !is_new {
                            let attrs = prop_attrs(&map.borrow(), &key);
                            if attrs & ATTR_WRITABLE == 0 {
                                return Ok(());
                            }
                        }
                        map.borrow_mut().insert(key.clone(), value.clone());
                        if is_window {
                            env_declare(&self.global, &key, value);
                        }
                        Ok(())
                    }
                    Value::Arr(a) => {
                        // freeze: 모든 변경 금지. seal/preventExtensions: 새 인덱스·프로퍼티 금지.
                        let av = Value::Arr(a.clone());
                        if self.is_frozen_val(&av) {
                            return Ok(());
                        }
                        if let Ok(i) = key.parse::<usize>() {
                            let old_len = a.borrow().len();
                            let is_new = i >= old_len;
                            if is_new && self.is_nonextensible_val(&av) {
                                return Ok(());
                            }
                            // length 가 non-writable 이면 길이를 넘기는 새 인덱스 추가 불가
                            // (§10.4.2.1) — sloppy 대입은 조용히 무시.
                            if is_new && !a.length_writable() {
                                return Ok(());
                            }
                            // non-writable 로 정의된 인덱스 덮어쓰기 불가(§10.4.2, sloppy 무시).
                            if matches!(a.index_attr(i), Some(at) if at & ATTR_WRITABLE == 0) {
                                return Ok(());
                            }
                            if i >= MAX_DENSE_ARRAY {
                                return Ok(()); // 방어: 초거대 인덱스 (희박 배열 미구현)
                            }
                            {
                                let mut arr = a.borrow_mut();
                                if i >= arr.len() {
                                    arr.resize(i + 1, Value::Undefined);
                                }
                                arr[i] = value;
                            }
                            // arr[i]=x 로 len 을 건너뛰면 그 사이(old_len..i)는 구멍이 된다.
                            if i > old_len {
                                for h in old_len..i {
                                    a.mark_hole(h);
                                }
                            }
                            a.fill_hole(i); // i 는 이제 값이 있음
                        } else if key == "length" {
                            // §10.4.2.4 ArraySetLength (ToNumber/ToUint32 검증 + resize).
                            // length 가 non-writable 이면 대입 무시(sloppy 근사).
                            if a.length_writable() {
                                self.array_set_length(&a, value.clone())?;
                            }
                        } else {
                            // 비인덱스 프로퍼티/메서드 재정의는 own-property 로 저장
                            if a.get_prop(&key).is_none() && self.is_nonextensible_val(&av) {
                                return Ok(());
                            }
                            a.set_prop(key, value);
                        }
                        Ok(())
                    }
                    Value::Dom(id) => self.dom_set(id, &key, value),
                    // rule.style.color = 'red' → 규칙의 선언을 실제로 바꾼다
                    Value::RuleStyle(si, ri) => {
                        let text = to_display(&value);
                        let prop = camel_to_dashed(&key);
                        self.rule_set_prop(si, ri, &prop, &text);
                        Ok(())
                    }
                    // sheet.disabled = true → 그 시트를 캐스케이드에서 뺀다
                    Value::Sheet(si) => {
                        if key == "disabled" {
                            let on = to_bool(&value);
                            if let Some(sheets) = self.sheets() {
                                if let Some(e) = sheets.get_mut(si) {
                                    e.disabled = on;
                                }
                            }
                            self.css_epoch += 1;
                        }
                        Ok(())
                    }
                    // attr.value = x → 소유 요소의 속성을 실제로 바꾼다
                    Value::Attr(id, name) => {
                        if matches!(key.as_str(), "value" | "nodeValue" | "textContent") {
                            let text = to_display(&value);
                            let dom = self.dom_arena()?;
                            dom.set_attr(id, &name, text);
                        }
                        Ok(())
                    }
                    // element.style.prop = value (라이브 프록시 → inline style 갱신)
                    Value::Style(id) => {
                        let text = to_display(&value);
                        if key == "cssText" {
                            self.set_style_attr(id, text);
                        } else {
                            let prop = camel_to_kebab(&key);
                            self.style_set(id, &prop, &text);
                        }
                        Ok(())
                    }
                    Value::Instance(inst) => {
                        let iv = Value::Instance(inst.clone());
                        // set 접근자가 있으면 호출한다 (표준). 예전엔 파서가 클래스 setter 를
                        // 버려서 대입이 그냥 필드에 꽂혔고 검증/변환 로직이 통째로 우회됐다.
                        if let Some(setter) = inst.class.find_setter(&key) {
                            self.call_value(Value::Fn(setter), Some(iv), vec![value])?;
                            return Ok(());
                        }
                        if self.is_frozen_val(&iv) {
                            return Ok(());
                        }
                        let plain = key.clone();
                        let key = field_key(&key, self.priv_id);
                        if !inst.fields.borrow().contains_key(&key)
                            && self.is_nonextensible_val(&iv)
                        {
                            return Ok(());
                        }
                        // 존재하는 필드가 non-writable(defineProperty 로 지정)이면 대입 무시
                        // (§10.1.9.1 OrdinarySetWithOwnDescriptor). 마커 없는 일반 필드는
                        // writable 기본이라 영향 없다.
                        {
                            let b = inst.fields.borrow();
                            if b.contains_key(&key)
                                && !is_private_name(&plain)
                                && prop_attrs(&b, &plain) & ATTR_WRITABLE == 0
                            {
                                return Ok(());
                            }
                        }
                        inst.fields.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    // el.dataset.x = v → data-x 속성을 실제로 바꾼다
                    Value::Dataset(id) => {
                        let attr = format!("data-{}", camel_to_kebab(&key));
                        let dom = self.dom_arena()?;
                        dom.set_attr(id, &attr, to_display(&value));
                        Ok(())
                    }
                    Value::Class(c) => {
                        // static set 접근자가 있으면 호출한다. 예전엔 파서·클래스 생성이
                        // static setter 를 저장까지 해놓고 **아무도 부르지 않아**,
                        // Class.prop = v 가 검증/변환 로직을 통째로 우회했다.
                        if let Some(setter) = c.find_static_setter(&key) {
                            let cv = Value::Class(c.clone());
                            self.call_value(Value::Fn(setter), Some(cv), vec![value])?;
                            return Ok(());
                        }
                        c.statics.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    // 함수 프로퍼티 (F.prototype, F.staticProp = ...) — 함수도 ordinary
                    // object 라 접근자면 setter 호출, 데이터면 writable 을 존중한다.
                    Value::Fn(func) => {
                        let fv = Value::Fn(func.clone());
                        if self.is_frozen_val(&fv) {
                            return Ok(());
                        }
                        let existing = func.props.borrow().get(&key).cloned();
                        match existing {
                            Some(Value::Accessor(acc)) => {
                                if let Some(s) = &acc.set {
                                    self.call_value(s.clone(), Some(fv), vec![value])?;
                                }
                                // setter 없는 접근자에 대입은 조용히 무시
                                Ok(())
                            }
                            Some(_) => {
                                // 기존 데이터 프로퍼티: writable:false 면 무시(속성 비트 보존)
                                if prop_attrs(&func.props.borrow(), &key) & ATTR_WRITABLE == 0 {
                                    return Ok(());
                                }
                                func.props.borrow_mut().insert(key, value);
                                Ok(())
                            }
                            None => {
                                // name/length 는 계산된 non-writable own 프로퍼티다
                                // (§10.2.8/.9): 대입은 조용히 무시(props 오버라이드가
                                // 없을 때). prototype 은 writable 이라 계속 저장.
                                if matches!(key.as_str(), "name" | "length") {
                                    return Ok(());
                                }
                                if self.is_nonextensible_val(&fv) {
                                    return Ok(());
                                }
                                func.props.borrow_mut().insert(key, value);
                                Ok(())
                            }
                        }
                    }
                    // 내장(네이티브)에 프로퍼티 얹기 — 폴리필의
                    // `if (!Promise.allSettled) Promise.allSettled = fn` 패턴.
                    Value::Native(n) => {
                        // name/length 는 내장 함수의 non-writable own 프로퍼티 (§17).
                        // @@species 는 setter 없는 접근자라 = 재대입은 무시(writable:false).
                        // Number.MAX_VALUE 등 상수와 prototype 도 writable:false → 재대입 무시.
                        // 나머지(폴리필이 얹는 메서드 등)는 native_props 에 저장.
                        // defineProperty 는 별도 경로라 오버라이드 여전히 가능.
                        let readonly = matches!(key.as_str(), "name" | "length")
                            || (key == "\u{0}@@species" && native_has_species(&n))
                            || self.native_static_readonly(&Value::Native(n), &key);
                        if !readonly {
                            self.native_props.entry(n).or_default().insert(key, value);
                        }
                        Ok(())
                    }
                    // 원시값(심볼/숫자/불리언/문자열)에 프로퍼티 대입은 sloppy 모드에서
                    // 무음 no-op 이다(§6.2.5.4 PutValue: base 가 원시면 auto-boxing 후 버려짐).
                    // 예전엔 throw 라 s.foo=1 이 죽었다. strict 면 TypeError 지만 기본은 sloppy.
                    Value::Symbol(_) | Value::Num(_) | Value::Bool(_) | Value::Str(_) => Ok(()),
                    other => Err(format!("{} 에 할당할 수 없음", to_display(&other))),
                }
    }

    fn assign_to(&mut self, target: &Expr, value: Value, env: &EnvRef) -> Result<(), String> {
        match target {
            Expr::Ident(name) => {
                // const 재대입은 TypeError (표준).
                if env_is_const(env, name) {
                    self.thrown = Some(Value::Str(format!(
                        "TypeError: Assignment to constant variable."
                    )));
                    return Err(format!("상수 '{}' 에 재대입", name));
                }
                // with 스코프의 객체가 그 이름을 가지면 **그 객체의 프로퍼티**에 쓴다
                // (§9.1.1.2 SetMutableBinding). 세터가 있으면 세터가 돈다.
                if let Some(o) = env_with_owner(env, name) {
                    return self.member_assign(o, name.clone(), value);
                }
                env_set(env, name, value);
                Ok(())
            }
            Expr::Member { obj, prop, computed } => {
                let recv = self.eval(obj, env)?;
                let key = self.member_key(prop, *computed, env)?;
                self.member_assign(recv, key, value)
            }
            _ => Err("할당 대상이 아님".to_string()),
        }
    }

    // ── DOM 바인딩 (아레나; dom 포인터는 실행 동안만 유효, 미설정 시 에러) ──

}

#[cfg(test)]
mod tests;
