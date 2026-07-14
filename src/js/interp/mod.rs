// 트리 워킹 인터프리터. Value/Env(렉시컬 체인)/제어 흐름.
// 무한 루프로 브라우저가 멈추지 않도록 실행 스텝 한도를 둔다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::*;
use super::parser::parse;

mod builtins;
mod canvas;
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
                .unwrap_or_else(|| i.class.name.clone());
            return format!("{}: {}", name, to_display(m));
        }
    }
    to_display(v)
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
        implementation.insert("hasFeature".to_string(), Value::Native(Native::ReturnFalse));
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
        ] {
            math.insert(name.to_string(), Value::Native(Native::Math(op)));
        }
        math.insert("PI".to_string(), Value::Num(std::f64::consts::PI));
        math.insert("E".to_string(), Value::Num(std::f64::consts::E));
        math.insert("SQRT2".to_string(), Value::Num(std::f64::consts::SQRT_2));
        math.insert("LN2".to_string(), Value::Num(std::f64::consts::LN_2));
        math.insert("LN10".to_string(), Value::Num(std::f64::consts::LN_10));
        env_declare(&global, "Math", Value::Obj(Rc::new(RefCell::new(math))));
        // JSON
        let mut json = ObjMap::new();
        json.insert("parse".to_string(), Value::Native(Native::JsonParse));
        json.insert("stringify".to_string(), Value::Native(Native::JsonStringify));
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
        reflect_ns.insert("ownKeys".to_string(), Value::Native(Native::ObjectKeys));
        reflect_ns.insert("getPrototypeOf".to_string(), Value::Native(Native::ObjectGetPrototypeOf));
        reflect_ns.insert("apply".to_string(), Value::Native(Native::ReflectApply));
        reflect_ns.insert("construct".to_string(), Value::Native(Native::ReflectConstruct));
        reflect_ns.insert("defineProperty".to_string(), Value::Native(Native::ObjectDefineProperty));
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
        object_ns.insert("getOwnPropertyNames".to_string(), Value::Native(Native::ObjectKeys));
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
        object_proto
            .insert("propertyIsEnumerable".to_string(), Value::Native(Native::HasOwnProperty));
        object_proto.insert("isPrototypeOf".to_string(), Value::Native(Native::ObjectIsPrototypeOf));
        object_proto
            .insert("propertyIsEnumerable".to_string(), Value::Native(Native::HasOwnProperty));
        object_ns.insert("prototype".to_string(), Value::Obj(Rc::new(RefCell::new(object_proto))));
        let object_ns = Value::Obj(Rc::new(RefCell::new(object_ns)));
        env_declare(&global, "Object", Value::Native(Native::ObjectCtor));
        // Array.prototype: 모든 배열 메서드를 담아 Array.prototype.slice.call(x) 지원
        let mut array_ns = ObjMap::new();
        array_ns.insert("isArray".to_string(), Value::Native(Native::ArrayIsArray));
        array_ns.insert("from".to_string(), Value::Native(Native::ArrayFrom));
        array_ns.insert("of".to_string(), Value::Native(Native::ArrayOf));
        let mut array_proto = ObjMap::new();
        for (name, op) in [
            ("forEach", ArrOp::ForEach),
            ("map", ArrOp::Map),
            ("filter", ArrOp::Filter),
            ("slice", ArrOp::Slice),
            ("join", ArrOp::Join),
            ("indexOf", ArrOp::IndexOf),
            ("pop", ArrOp::Pop),
            ("some", ArrOp::Some),
            ("every", ArrOp::Every),
            ("reduce", ArrOp::Reduce),
            ("find", ArrOp::Find),
            ("findIndex", ArrOp::FindIndex),
            ("concat", ArrOp::Concat),
            ("includes", ArrOp::Includes),
            ("splice", ArrOp::Splice),
            ("shift", ArrOp::Shift),
            ("unshift", ArrOp::Unshift),
            ("reverse", ArrOp::Reverse),
            ("keys", ArrOp::Keys),
            ("values", ArrOp::Values),
            ("entries", ArrOp::Entries),
        ] {
            array_proto.insert(name.to_string(), Value::Native(Native::Arr(op)));
        }
        array_proto.insert("push".to_string(), Value::Native(Native::ArrayPush));
        // Array.prototype[Symbol.iterator]/toString — core-js uncurryThis 참조
        array_proto.insert("\u{0}@@iterator".to_string(), Value::Native(Native::MakeIter));
        array_proto.insert("toString".to_string(), Value::Native(Native::Arr(ArrOp::Join)));
        array_proto.insert("sort".to_string(), Value::Native(Native::Arr(ArrOp::Sort)));
        array_ns.insert("prototype".to_string(), Value::Obj(Rc::new(RefCell::new(array_proto))));
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
        // Map / Set / WeakMap / WeakSet (약한 참조는 일반 Map/Set 으로 근사)
        env_declare(&global, "Map", Value::Native(Native::MapCtor));
        env_declare(&global, "WeakMap", Value::Native(Native::MapCtor));
        env_declare(&global, "Set", Value::Native(Native::SetCtor));
        env_declare(&global, "WeakSet", Value::Native(Native::SetCtor));
        env_declare(&global, "Event", Value::Native(Native::EventCtor));
        env_declare(&global, "CustomEvent", Value::Native(Native::EventCtor));
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
        // Event 생성자류(window.Event.prototype 참조 등) — 모두 EventCtor 로 근사.
        for ev in ["Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "PointerEvent", "FocusEvent", "InputEvent"] {
            window.insert(ev.to_string(), Value::Native(Native::EventCtor));
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
        let fn_proto = Value::Obj(Rc::new(RefCell::new(fn_proto)));
        // String.prototype: 문자열 메서드 (String.prototype.slice.call(x) 지원)
        let mut string_proto = ObjMap::new();
        for (name, op) in [
            ("charAt", StrOp::CharAt),
            ("charCodeAt", StrOp::CharCodeAt),
            ("indexOf", StrOp::IndexOf),
            ("lastIndexOf", StrOp::LastIndexOf),
            ("slice", StrOp::Slice),
            ("substring", StrOp::Slice),
            ("split", StrOp::Split),
            ("toUpperCase", StrOp::Upper),
            ("toLowerCase", StrOp::Lower),
            ("trim", StrOp::Trim),
            ("replace", StrOp::Replace),
            ("includes", StrOp::Includes),
            ("startsWith", StrOp::StartsWith),
            ("endsWith", StrOp::EndsWith),
            ("match", StrOp::Match),
            ("padStart", StrOp::PadStart),
            ("padEnd", StrOp::PadEnd),
            ("repeat", StrOp::Repeat),
        ] {
            string_proto.insert(name.to_string(), Value::Native(Native::Str(op)));
        }
        let string_proto = Value::Obj(Rc::new(RefCell::new(string_proto)));
        // Number/Boolean/RegExp.prototype — 원시값 메서드 네이티브를 재사용.
        let mk_proto = |pairs: Vec<(&str, Native)>| {
            let mut m = ObjMap::new();
            for (k, n) in pairs {
                m.insert(k.to_string(), Value::Native(n));
            }
            Value::Obj(Rc::new(RefCell::new(m)))
        };
        let number_proto = mk_proto(vec![
            ("toString", Native::ValueToStr),
            ("toLocaleString", Native::ValueToStr),
            ("toFixed", Native::NumToFixed),
            ("toPrecision", Native::NumToFixed),
            ("valueOf", Native::ValueOfSelf),
        ]);
        let boolean_proto = mk_proto(vec![
            ("toString", Native::ValueToStr),
            ("valueOf", Native::ValueOfSelf),
        ]);
        let regexp_proto = mk_proto(vec![
            ("exec", Native::RegexExec),
            ("test", Native::RegexTest),
            ("toString", Native::ValueToStr),
        ]);
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
            ("\u{0}@@iterator", Native::Set(SetOp::Values)),
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
            ("toJSON", Native::DateMethod(DateField::ToIso)),
            ("toString", Native::DateMethod(DateField::ToStr)),
        ]);
        let symbol_proto = mk_proto(vec![
            ("toString", Native::ValueToStr),
            ("valueOf", Native::ValueOfSelf),
        ]);
        // Error.prototype 및 서브타입 prototype (ECMA-262 §20.5.3, §20.5.6.3).
        // NativeError.prototype 의 [[Prototype]] 은 Error.prototype 이고,
        // 각자 자기 name 과 constructor 를 갖는다. 프로퍼티는 전부 비열거.
        let error_proto = mk_proto(vec![("toString", Native::ErrorToString)]);
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
            object_ns,
            array_ns,
            date_proto,
            symbol_proto,
            string_proto,
            number_proto,
            boolean_proto,
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
    fn new_promise(&self) -> Value {
        let mut m = ObjMap::new();
        m.insert("\u{0}isPromise".to_string(), Value::Bool(true));
        m.insert("\u{0}state".to_string(), Value::Str("pending".to_string()));
        m.insert("\u{0}value".to_string(), Value::Undefined);
        m.insert("\u{0}cbs".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
        // then/catch/finally 는 own 프로퍼티가 아니라 member_get 에서 해석(비열거).
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // promise 를 값으로 이행. 값이 또 promise 면 그것이 이행될 때 이어서 이행(체이닝).
    // promise 를 이행(fulfilled)으로 정착. 값이 thenable 이면 그 상태를 채택.
    fn resolve_promise(&mut self, p: &Value, v: Value) {
        if is_promise(&v) {
            // p 는 v 의 상태를 채택: v 이행 → Identity 로 p 이행, v 거부 → 전파로 p 거부
            self.promise_then(&v, Value::Native(Native::Identity), Value::Undefined, p.clone());
            return;
        }
        self.settle(p, true, v);
    }

    // promise 를 거부(rejected)로 정착.
    fn reject_promise(&mut self, p: &Value, reason: Value) {
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
        // stack: 표준은 아니지만 모든 엔진이 준다. 비열거로 둔다.
        map.insert(
            "stack".to_string(),
            Value::Str(self.err_stack.clone().unwrap_or_default().join("\n")),
        );
        map.insert(nonenum_marker("stack"), Value::Bool(true));
        let proto = self
            .error_protos
            .iter()
            .find(|(k, _)| *k == kind)
            .map(|(_, p)| p.clone())
            .unwrap_or_else(|| self.error_proto.clone());
        map.insert("__proto__".to_string(), proto);
        Value::Obj(Rc::new(RefCell::new(map)))
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

    // 종류가 지정되지 않은 내부 오류를 잡을 때 쓰는 Error 객체.
    pub(super) fn error_from_msg(&self, msg: &str) -> Value {
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
                    Value::Arr(ArrayObj::new(vec![Value::Undefined; *n as usize]))
                }
                items => Value::Arr(ArrayObj::new(items.to_vec())),
            }),
            Value::Native(Native::ObjectCtor) => {
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                Some(match arg {
                    Value::Null | Value::Undefined => {
                        Value::Obj(Rc::new(RefCell::new(ObjMap::new())))
                    }
                    other => other, // ToObject 근사 (원시값 박싱 미구현)
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
    pub(super) fn global_has(&self, m: &Rc<RefCell<ObjMap>>, key: &str) -> bool {
        self.is_global_obj(m) && !is_internal_key(key) && env_get(&self.global, key).is_some()
    }

    // window(전역 객체) 프로퍼티 조회 — 브라우저처럼 window.X 를 맨 X 로 읽게 하는 폴백.
    fn window_prop(&self, name: &str) -> Option<Value> {
        if let Some(Value::Obj(m)) = env_get(&self.global, "window") {
            let v = m.borrow().get(name).cloned();
            // window.window 등 자기참조로 인한 무의미 순환 방지: window 자신은 제외
            if name != "window" {
                return v;
            }
        }
        None
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
    fn make_function(&self, args: Vec<Value>) -> Result<Value, String> {
        let (body_src, param_args) = match args.split_last() {
            Some((last, rest)) => (to_display(last), rest.to_vec()),
            None => (String::new(), Vec::new()),
        };
        let mut params = Vec::new();
        for p in &param_args {
            for name in to_display(p).split(',') {
                let name = name.trim();
                if !name.is_empty() {
                    params.push(name.to_string());
                }
            }
        }
        let body = parse(&body_src).map_err(|e| format!("Function 본문 파싱 실패: {}", e))?;
        Ok(Value::Fn(Rc::new(JsFn {
            params,
            body,
            env: self.global.clone(),
            is_arrow: false,
            is_generator: false,
            is_async: false,
            this: None,
            super_class: None,
            props: RefCell::new(HashMap::new()),
        })))
    }

    // new Map(iterable): [[k,v],...] 로 초기화
    fn make_map(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let store: Vec<(Value, Value)> = Vec::new();
        let map = Rc::new(RefCell::new(store));
        if let Some(Value::Arr(a)) = args.first() {
            for entry in a.borrow().iter() {
                if let Value::Arr(pair) = entry {
                    let p = pair.borrow();
                    let k = p.first().cloned().unwrap_or(Value::Undefined);
                    let v = p.get(1).cloned().unwrap_or(Value::Undefined);
                    map.borrow_mut().push((k, v));
                }
            }
        }
        Ok(Value::MapVal(map))
    }

    // new Set(iterable): 배열로 초기화 (중복 제거)
    fn make_set(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let set = Rc::new(RefCell::new(Vec::<Value>::new()));
        if let Some(Value::Arr(a)) = args.first() {
            for v in a.borrow().iter() {
                if !set.borrow().iter().any(|e| same_value_zero(e, v)) {
                    set.borrow_mut().push(v.clone());
                }
            }
        }
        Ok(Value::SetVal(set))
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
                let f = args.first().cloned().ok_or("콜백 필요")?;
                let snapshot: Vec<(Value, Value)> = m.borrow().clone();
                for (k, v) in snapshot {
                    self.call_value(f.clone(), None, vec![v, k])?;
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

    fn set_method(&mut self, s: Rc<RefCell<Vec<Value>>>, op: SetOp, args: Vec<Value>) -> Value {
        let val = args.first().cloned().unwrap_or(Value::Undefined);
        match op {
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
                let f = match args.first() {
                    Some(f) => f.clone(),
                    None => return Value::Undefined,
                };
                let snapshot: Vec<Value> = s.borrow().clone();
                for v in snapshot {
                    let _ = self.call_value(f.clone(), None, vec![v.clone(), v]);
                }
                Value::Undefined
            }
            // values/keys 는 이터레이터다 (배열이 아니다 — 표준).
            SetOp::Values => {
                let items = s.borrow().clone();
                self.make_iter_from_vec(items)
            }
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
                        }
                    }
                    self.bind_pattern(sub, v, env, assign)?;
                }
                // { a, ...rest } — 분해되지 않은 나머지 own 프로퍼티를 객체로
                if let Some(rest_pat) = rest {
                    let consumed: std::collections::HashSet<&str> =
                        keys.iter().map(|k| k.as_str()).collect();
                    let mut map = ObjMap::new();
                    match &value {
                        Value::Obj(o) => {
                            for (k, v) in o.borrow().iter() {
                                if !consumed.contains(k.as_str()) && !is_internal_key(k.as_str()) {
                                    map.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        v @ Value::Instance(_) => {
                            for (k, val) in builtins::own_enumerable_entries(v) {
                                if !consumed.contains(k.as_str()) {
                                    map.insert(k, val);
                                }
                            }
                        }
                        _ => {}
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
            if let Stmt::FuncDecl { name, params, body, is_generator, is_async } = s {
                let f = Value::Fn(Rc::new(JsFn {
                    params: params.clone(),
                    body: body.clone(),
                    env: env.clone(),
                    is_arrow: false,
                    is_generator: *is_generator,
                    is_async: *is_async,
                    this: None,
                    super_class: None,
                    props: RefCell::new(HashMap::new()),
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
                                env_declare(&cscope, p, caught);
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
                    Value::Arr(a) => (0..a.borrow().len()).map(|i| i.to_string()).collect(),
                    Value::Str(s) => (0..s.encode_utf16().count()).map(|i| i.to_string()).collect(),
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
                for item in items {
                    if let Expr::Spread(inner) = item {
                        let val = self.eval(inner, env)?;
                        // null/undefined 전개는 TypeError (표준). 조용히 빈 배열로 넘기면
                        // 진짜 버그가 숨는다.
                        if matches!(val, Value::Undefined | Value::Null) {
                            let d = to_display(&val);
                            return Err(self
                                .throw_error("TypeError", format!("{} 은(는) 이터러블이 아님", d)));
                        }
                        v.extend(self.iterate_to_vec(&val)?);
                    } else {
                        v.push(self.eval(item, env)?);
                    }
                }
                Ok(Value::Arr(ArrayObj::new(v)))
            }
            // 스프레드가 배열/호출 밖에 홀로 나오면 값 그대로 (관용)
            Expr::Spread(inner) => self.eval(inner, env),
            Expr::Object(props) => {
                let mut map = ObjMap::new();
                for (k, e) in props {
                    if matches!(k, PropKey::Spread) {
                        // {...obj} — obj/배열/인스턴스의 own 프로퍼티 병합
                        match self.eval(e, env)? {
                            Value::Obj(o) => {
                                for (k, v) in o.borrow().iter() {
                                    if !is_internal_key(k.as_str()) {
                                        map.insert(k.clone(), v.clone());
                                    }
                                }
                            }
                            v @ Value::Instance(_) => {
                                for (k, val) in builtins::own_enumerable_entries(&v) {
                                    map.insert(k, val);
                                }
                            }
                            Value::Arr(a) => {
                                for (i, v) in a.borrow().iter().enumerate() {
                                    map.insert(i.to_string(), v.clone());
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
            Expr::Func { name, params, body, is_arrow, is_generator, is_async } => {
                // 화살표는 정의 시점 this 를 캡처 (렉시컬)
                let this = if *is_arrow { env_get(env, "this").map(Box::new) } else { None };
                // 명명 함수식: 자기 이름을 감싸는 스코프에 바인딩(재귀용). 외부엔 미노출.
                let fn_env = match name {
                    Some(_) => Env::new(Some(env.clone())),
                    None => env.clone(),
                };
                let f = Rc::new(JsFn {
                    params: params.clone(),
                    body: body.clone(),
                    env: fn_env.clone(),
                    is_arrow: *is_arrow,
                    is_generator: *is_generator,
                    is_async: *is_async,
                    this,
                    super_class: None,
                    props: RefCell::new(HashMap::new()),
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
                            // ${obj} 는 문자열 힌트로 ToPrimitive (toString 우선)
                            let v = self.eval(e, env)?;
                            let p = self.to_primitive(v, true);
                            s.push_str(&to_display(&p));
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
                        if env_get(env, name).is_none() {
                            return Ok(Value::Str("undefined".to_string()));
                        }
                    }
                }
                // delete obj.key / obj[key] — 실제로 own 프로퍼티 제거 후 true.
                if matches!(op, UnOp::Delete) {
                    if let Expr::Member { obj, prop, computed } = expr.as_ref() {
                        let target = self.eval(obj, env)?;
                        let key = match (computed, prop.as_ref()) {
                            (false, Expr::Str(s)) => s.clone(),
                            _ => to_display(&self.eval(prop, env)?),
                        };
                        match &target {
                            Value::Obj(m) => {
                                m.borrow_mut().remove(&key);
                            }
                            // Proxy: deleteProperty 트랩 (없으면 타깃에 위임).
                            // 반응성 라이브러리(Vue 등)가 delete 를 이 트랩으로 잡는다.
                            Value::Proxy(p) => {
                                let (t, h) = (p.0.clone(), p.1.clone());
                                let trap = self.member_get(&h, "deleteProperty")?;
                                if is_callable(&trap) {
                                    let res = self.call_value(
                                        trap,
                                        Some(h),
                                        vec![t, Value::Str(key.clone())],
                                    )?;
                                    return Ok(Value::Bool(to_bool(&res)));
                                }
                                if let Value::Obj(m) = &t {
                                    m.borrow_mut().remove(&key);
                                }
                            }
                            Value::Arr(a) => {
                                // 배열 요소 삭제는 구멍(undefined)을 남긴다 (길이 불변)
                                if let Ok(i) = key.parse::<usize>() {
                                    let mut b = a.borrow_mut();
                                    if i < b.len() {
                                        b[i] = Value::Undefined;
                                    }
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
                    // 단항 +/- 도 객체를 원시값으로 변환해야 한다 (ToNumber → ToPrimitive).
                    // 이항 연산만 변환하고 있어서 +obj 는 NaN 이었다.
                    UnOp::Neg => {
                        let p = self.to_primitive(v.clone(), false);
                        Value::Num(-to_num(&p))
                    }
                    UnOp::Pos => {
                        let p = self.to_primitive(v.clone(), false);
                        Value::Num(to_num(&p))
                    }
                    UnOp::Not => Value::Bool(!to_bool(&v)),
                    UnOp::Typeof => Value::Str(type_of(&v).to_string()),
                    UnOp::BitNot => Value::Num(!to_i32(&v) as f64),
                    // void: 피연산자 평가 후 undefined. delete: 근사(항상 true)
                    UnOp::Void => Value::Undefined,
                    UnOp::Delete => Value::Bool(true),
                })
            }
            Expr::Update { op, prefix, target } => {
                let old = to_num(&self.eval(target, env)?);
                let new = match op {
                    UpdOp::Inc => old + 1.0,
                    UpdOp::Dec => old - 1.0,
                };
                self.assign_to(target, Value::Num(new), env)?;
                Ok(Value::Num(if *prefix { new } else { old }))
            }
            Expr::Binary { op, left, right } => {
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
                    return Err("호출 스택 초과".to_string());
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
            Ok(key_of(&v))
        } else if let Expr::Str(s) = prop {
            Ok(s.clone())
        } else {
            Err("잘못된 멤버 접근".to_string())
        }
    }

    // 대상에 own 프로퍼티 설정 (Object.assign 의 대상 쓰기, super() 의 this 채우기).
    // 무결성(freeze/seal)을 존중하고, 접근자(setter)가 있으면 setter 를 호출한다.
    pub(super) fn set_own_property(&mut self, target: &Value, k: String, v: Value) {
        if self.is_frozen_val(target) {
            return;
        }
        match target {
            Value::Obj(m) => {
                // setter 가 있으면 호출 (own → 프로토타입)
                if let Some(acc) = self.find_accessor(m, &k) {
                    if let Some(st) = acc.set.clone() {
                        let _ = self.call_value(st, Some(target.clone()), vec![v]);
                    }
                    return;
                }
                if !m.borrow().contains_key(&k) && self.is_nonextensible_val(target) {
                    return;
                }
                m.borrow_mut().insert(k, v);
            }
            Value::Arr(a) => {
                if let Ok(i) = k.parse::<usize>() {
                    if i >= a.borrow().len() && self.is_nonextensible_val(target) {
                        return;
                    }
                    let mut items = a.borrow_mut();
                    if i >= items.len() {
                        items.resize(i + 1, Value::Undefined);
                    }
                    items[i] = v;
                } else {
                    if a.get_prop(&k).is_none() && self.is_nonextensible_val(target) {
                        return;
                    }
                    a.set_prop(k, v);
                }
            }
            Value::Instance(inst) => {
                // set 접근자가 있으면 호출한다 (표준). 예전엔 파서가 setter 를 버려서
                // 대입이 그냥 필드에 꽂혔고, 검증/변환 로직이 통째로 우회됐다.
                if let Some(setter) = inst.class.find_setter(&k) {
                    let _ = self.call_value(Value::Fn(setter), Some(target.clone()), vec![v]);
                    return;
                }
                if !inst.fields.borrow().contains_key(&k) && self.is_nonextensible_val(target) {
                    return;
                }
                inst.fields.borrow_mut().insert(k, v);
            }
            Value::Fn(f) => {
                if !f.props.borrow().contains_key(&k) && self.is_nonextensible_val(target) {
                    return;
                }
                f.props.borrow_mut().insert(k, v);
            }
            Value::Class(c) => {
                c.statics.borrow_mut().insert(k, v);
            }
            _ => {}
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
            if let Some(Stmt::FuncDecl { name, params, body: fb, is_generator, is_async }) = decl {
                let f = Value::Fn(Rc::new(JsFn {
                    params: params.clone(),
                    body: fb.clone(),
                    env: env.clone(),
                    is_arrow: false,
                    is_generator: *is_generator,
                    is_async: *is_async,
                    this: None,
                    super_class: None,
                    props: RefCell::new(HashMap::new()),
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
                params: vec![],
                body: vec![Stmt::Return(Some(Expr::Ident(local.clone())))],
                env: env.clone(),
                is_arrow: false,
                is_generator: false,
                is_async: false,
                this: None,
                super_class: None,
                props: RefCell::new(HashMap::new()),
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
    fn proto_chain_lookup(
        &mut self,
        map: &Rc<RefCell<ObjMap>>,
        key: &str,
        this: &Value,
    ) -> Result<Option<Value>, String> {
        let mut proto = map.borrow().get("__proto__").cloned();
        let mut depth = 0;
        while let Some(Value::Obj(p)) = proto {
            depth += 1;
            if depth > 100 {
                break; // 순환/과도한 체인 방어
            }
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
        Ok(None)
    }

    fn member_get(&mut self, recv: &Value, key: &str) -> Result<Value, String> {
        // 내장에 얹힌 프로퍼티가 최우선 (폴리필이 Promise.allSettled 등을 덮어쓴 경우).
        if let Value::Native(n) = recv {
            if let Some(v) = self.native_props.get(n).and_then(|m| m.get(key)) {
                return Ok(v.clone());
            }
        }
        // .constructor — 값 타입의 전역 생성자 (core-js/프레임워크의 타입판별·종/species 에 필수).
        // 객체/인스턴스가 자체 constructor 프로퍼티를 가지면 그것 우선.
        if key == "constructor" {
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
            Value::Gen(_) => Ok(match key {
                "next" => Value::Native(Native::GenNext),
                "return" => Value::Native(Native::GenReturn),
                "throw" => Value::Native(Native::GenThrow),
                "\u{0}@@iterator" => Value::Native(Native::ReturnThis),
                _ => Value::Undefined,
            }),
            // Proxy: get 트랩 있으면 handler.get(target, key, receiver), 없으면 target 위임
            Value::Proxy(p) => {
                let (target, handler) = (&p.0, &p.1);
                let trap = self.member_get(handler, "get")?;
                if !matches!(trap, Value::Undefined) {
                    return self.call_value(
                        trap,
                        Some(handler.clone()),
                        vec![target.clone(), Value::Str(key.to_string()), recv.clone()],
                    );
                }
                let target = target.clone();
                self.member_get(&target, key)
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
                        }
                        match key {
                        "hasOwnProperty" => Ok(Value::Native(Native::HasOwnProperty)),
                        // propertyIsEnumerable: own 프로퍼티면 열거가능(단순 모델) → hasOwnProperty 로 근사.
                        // core-js 등이 {}.propertyIsEnumerable.call(...) 로 기능탐지 → 없으면 크래시.
                        "propertyIsEnumerable" => Ok(Value::Native(Native::HasOwnProperty)),
                        "test" if is_regex_obj(map) => Ok(Value::Native(Native::RegexTest)),
                        "exec" if is_regex_obj(map) => Ok(Value::Native(Native::RegexExec)),
                        // promise 메서드는 프로토타입 격(비열거) — own 프로퍼티 아님.
                        "then" if is_promise(recv) => Ok(Value::Native(Native::PromiseThen)),
                        "catch" if is_promise(recv) => Ok(Value::Native(Native::PromiseCatch)),
                        "finally" if is_promise(recv) => Ok(Value::Native(Native::PromiseFinally)),
                        _ if is_date_obj(map) => {
                            let field = match key {
                                "getTime" | "valueOf" => Some(DateField::Time),
                                "getFullYear" | "getUTCFullYear" => Some(DateField::FullYear),
                                "getMonth" | "getUTCMonth" => Some(DateField::Month),
                                "getDate" | "getUTCDate" => Some(DateField::Date),
                                "getDay" | "getUTCDay" => Some(DateField::Day),
                                "getHours" | "getUTCHours" => Some(DateField::Hours),
                                "getMinutes" | "getUTCMinutes" => Some(DateField::Minutes),
                                "getSeconds" | "getUTCSeconds" => Some(DateField::Seconds),
                                "getMilliseconds" => Some(DateField::Ms),
                                "getTimezoneOffset" => Some(DateField::TimezoneOffset),
                                "toISOString" | "toJSON" => Some(DateField::ToIso),
                                "toString" | "toUTCString" | "toGMTString" => Some(DateField::ToStr),
                                "toDateString" | "toLocaleDateString" | "toLocaleString"
                                | "toLocaleTimeString" => Some(DateField::ToDateStr),
                                // Date 는 가변 객체다 (표준). setter 가 없으면
                                // date.setTime(...) 같은 흔한 코드가 "함수 아님" 으로 죽는다.
                                "setTime" => Some(DateField::SetTime),
                                "setFullYear" | "setUTCFullYear" => Some(DateField::SetFullYear),
                                "setMonth" | "setUTCMonth" => Some(DateField::SetMonth),
                                "setDate" | "setUTCDate" => Some(DateField::SetDate),
                                "setHours" | "setUTCHours" => Some(DateField::SetHours),
                                "setMinutes" | "setUTCMinutes" => Some(DateField::SetMinutes),
                                "setSeconds" | "setUTCSeconds" => Some(DateField::SetSeconds),
                                "setMilliseconds" | "setUTCMilliseconds" => Some(DateField::SetMs),
                                _ => None,
                            };
                            Ok(field
                                .map(|f| Value::Native(Native::DateMethod(f)))
                                .unwrap_or(Value::Undefined))
                        }
                        // Object.prototype 폴백 — 인스턴스 객체도 valueOf/toString/hasOwnProperty 등
                        _ => Ok(self.proto_method("Object", key).unwrap_or(Value::Undefined)),
                        }
                    }
                }
            }
            Value::Arr(a) => {
                // 재정의된 own-property 가 내장 메서드를 가린다 (arr.push = fn 등)
                if let Some(v) = a.get_prop(key) {
                    return Ok(v);
                }
                if key == "length" {
                    return Ok(Value::Num(a.borrow().len() as f64));
                }
                if key == "push" {
                    return Ok(Value::Native(Native::ArrayPush));
                }
                let op = match key {
                    "join" => Some(ArrOp::Join),
                    "pop" => Some(ArrOp::Pop),
                    "indexOf" => Some(ArrOp::IndexOf),
                    "slice" => Some(ArrOp::Slice),
                    "forEach" => Some(ArrOp::ForEach),
                    "map" => Some(ArrOp::Map),
                    "filter" => Some(ArrOp::Filter),
                    "some" => Some(ArrOp::Some),
                    "every" => Some(ArrOp::Every),
                    "reduce" => Some(ArrOp::Reduce),
                    "find" => Some(ArrOp::Find),
                    "findIndex" => Some(ArrOp::FindIndex),
                    "concat" => Some(ArrOp::Concat),
                    "includes" => Some(ArrOp::Includes),
                    "splice" => Some(ArrOp::Splice),
                    "shift" => Some(ArrOp::Shift),
                    "unshift" => Some(ArrOp::Unshift),
                    "reverse" => Some(ArrOp::Reverse),
                    "keys" => Some(ArrOp::Keys),
                    "values" => Some(ArrOp::Values),
                    "entries" => Some(ArrOp::Entries),
                    "sort" => Some(ArrOp::Sort),
                    "flat" => Some(ArrOp::Flat),
                    "flatMap" => Some(ArrOp::FlatMap),
                    "at" => Some(ArrOp::At),
                    "findLast" => Some(ArrOp::FindLast),
                    "findLastIndex" => Some(ArrOp::FindLastIndex),
                    "fill" => Some(ArrOp::Fill),
                    "reduceRight" => Some(ArrOp::ReduceRight),
                    _ => None,
                };
                if let Some(op) = op {
                    return Ok(Value::Native(Native::Arr(op)));
                }
                if key == "hasOwnProperty" {
                    return Ok(Value::Native(Native::HasOwnProperty));
                }
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                if let Ok(i) = key.parse::<usize>() {
                    return Ok(a.borrow().get(i).cloned().unwrap_or(Value::Undefined));
                }
                // Array.prototype 폴리필 메서드 (at/flatMap/findLast 등) 조회
                if let Some(m) = self.proto_method("Array", key) {
                    return Ok(m);
                }
                Ok(Value::Undefined)
            }
            Value::MapVal(m) => {
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                if key == "size" {
                    return Ok(Value::Num(m.borrow().len() as f64));
                }
                let op = match key {
                    "get" => Some(MapOp::Get),
                    "set" => Some(MapOp::Set),
                    "has" => Some(MapOp::Has),
                    "delete" => Some(MapOp::Delete),
                    "clear" => Some(MapOp::Clear),
                    "forEach" => Some(MapOp::ForEach),
                    "keys" => Some(MapOp::Keys),
                    "values" => Some(MapOp::Values),
                    "entries" => Some(MapOp::Entries),
                    _ => None,
                };
                Ok(op.map(|o| Value::Native(Native::Map(o))).unwrap_or(Value::Undefined))
            }
            Value::SetVal(s) => {
                if key == "size" {
                    return Ok(Value::Num(s.borrow().len() as f64));
                }
                if key == "\u{0}@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                let op = match key {
                    "add" => Some(SetOp::Add),
                    "has" => Some(SetOp::Has),
                    "delete" => Some(SetOp::Delete),
                    "clear" => Some(SetOp::Clear),
                    "forEach" => Some(SetOp::ForEach),
                    "values" | "keys" => Some(SetOp::Values),
                    _ => None,
                };
                Ok(op.map(|o| Value::Native(Native::Set(o))).unwrap_or(Value::Undefined))
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
                    "length" => Ok(Value::Num(self.class_tokens(id).len() as f64)),
                    "value" => Ok(Value::Str(self.class_tokens(id).join(" "))),
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
                    "slice" | "substring" => Some(StrOp::Slice),
                    "split" => Some(StrOp::Split),
                    "toUpperCase" => Some(StrOp::Upper),
                    "toLowerCase" => Some(StrOp::Lower),
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
                    "substr" => Some(StrOp::Slice),
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
                self.dom_get(*id, key)
            }
            Value::Instance(inst) => {
                // 필드 우선, 그다음 get 접근자(호출해 값 산출), 그다음 메서드 체인
                if let Some(v) = inst.fields.borrow().get(key) {
                    return Ok(v.clone());
                }
                if let Some(g) = inst.class.find_getter(key) {
                    return self.call_value(Value::Fn(g), Some(recv.clone()), vec![]);
                }
                if let Some(m) = inst.class.find_method(key) {
                    return Ok(Value::Fn(m));
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
                    "hasOwnProperty" | "propertyIsEnumerable" => {
                        Ok(Value::Native(Native::HasOwnProperty))
                    }
                    _ => Ok(self.proto_method("Object", key).unwrap_or(Value::Undefined)),
                }
            }
            Value::Class(c) => {
                // C.prototype — 클래스 메서드를 담은 객체 (상속 체인 포함).
                // 예전엔 undefined 라, 프로토타입에서 메서드를 꺼내 특정 this 로 호출하는
                // 코드(커스텀 엘리먼트의 connectedCallback 등)가 전부 실패했다.
                if key == "prototype" {
                    if let Some(p) = c.proto_cache.borrow().clone() {
                        return Ok(p);
                    }
                    let mut m = ObjMap::new();
                    fn collect(cls: &Rc<JsClass>, m: &mut ObjMap) {
                        if let Some(p) = &cls.parent {
                            collect(p, m);
                        }
                        for (k, f) in &cls.methods {
                            m.insert(k.clone(), Value::Fn(f.clone()));
                        }
                        for (k, g) in &cls.getters {
                            m.insert(
                                k.clone(),
                                Value::Accessor(AccessorPair::getter(Value::Fn(g.clone()))),
                            );
                        }
                    }
                    collect(c, &mut m);
                    m.insert("constructor".to_string(), recv.clone());
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
                // 상속된 정적 멤버
                let mut p = c.parent.clone();
                while let Some(cls) = p {
                    if let Some(v) = cls.statics.borrow().get(key).cloned() {
                        return Ok(v);
                    }
                    p = cls.parent.clone();
                }
                Ok(Value::Undefined)
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
                    "name" => Ok(Value::Str(String::new())),
                    "length" => Ok(Value::Num(func.params.len() as f64)),
                    // 함수도 toString 을 가진다 (번들이 fn.toString() 으로 소스 검사)
                    "toString" => Ok(Value::Native(Native::FnToString)),
                    // F.prototype 지연 생성: 접근 시 빈 객체를 만들어 저장
                    // (F.prototype.method = ... 패턴 지원)
                    "prototype" => {
                        let proto = Value::Obj(Rc::new(RefCell::new(ObjMap::new())));
                        func.props.borrow_mut().insert("prototype".to_string(), proto.clone());
                        Ok(proto)
                    }
                    _ => Ok(Value::Undefined),
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
                _ => Value::Undefined,
            }),
            // Object/Array 전역은 Native 생성자(typeof === "function"). 정적 멤버·prototype 은
            // 보관된 네임스페이스 맵에 위임한다.
            Value::Native(Native::ObjectCtor) => {
                let ns = self.object_ns.clone();
                self.member_get(&ns, key)
            }
            Value::Native(Native::ArrayCtor) => {
                let ns = self.array_ns.clone();
                self.member_get(&ns, key)
            }
            // Map/Set(=WeakMap/WeakSet).prototype — 번들의 Map.prototype.get 등.
            Value::Native(Native::MapCtor) if key == "prototype" => Ok(self.map_proto.clone()),
            Value::Native(Native::SetCtor) if key == "prototype" => Ok(self.set_proto.clone()),
            // Error/TypeError/… 의 prototype 과 name (class X extends Error, 기능 탐지).
            Value::Native(Native::ErrorCtor(n)) => Ok(match key {
                // 종류별 prototype (TypeError.prototype !== Error.prototype)
                "prototype" => self
                    .error_protos
                    .iter()
                    .find(|(k, _)| k == n)
                    .map(|(_, p)| p.clone())
                    .unwrap_or_else(|| self.error_proto.clone()),
                "name" => Value::Str(n.to_string()),
                "call" => Value::Native(Native::FnCall),
                "apply" => Value::Native(Native::FnApply),
                "bind" => Value::Native(Native::FnBind),
                _ => Value::Undefined,
            }),
            // String.fromCharCode/prototype
            Value::Native(Native::StringCtor) => Ok(match key {
                "fromCharCode" | "fromCodePoint" => Value::Native(Native::StrFromCharCode),
                "prototype" => self.string_proto.clone(),
                _ => Value::Undefined,
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
                "for" => Value::Native(Native::SymbolFor),
                "keyFor" => Value::Native(Native::SymbolKeyFor),
                "prototype" => self.symbol_proto.clone(),
                _ => Value::Undefined,
            }),
            // 심볼 원시값: .description / .toString()
            Value::Symbol(s) => Ok(match key {
                "description" => {
                    s.desc.clone().map(Value::Str).unwrap_or(Value::Undefined)
                }
                "toString" => Value::Native(Native::ValueToStr),
                "constructor" => Value::Native(Native::SymbolCtor),
                _ => Value::Undefined,
            }),
            // Number.isInteger/isNaN/isFinite/parseInt/parseFloat + 상수
            Value::Native(Native::NumberCtor) => Ok(match key {
                "isInteger" | "isSafeInteger" => Value::Native(Native::NumIsInteger),
                "isFinite" => Value::Native(Native::NumIsFinite),
                "isNaN" => Value::Native(Native::NumIsNaN),
                "parseInt" => Value::Native(Native::ParseInt),
                "parseFloat" => Value::Native(Native::ParseFloat),
                "MAX_SAFE_INTEGER" => Value::Num(9007199254740991.0),
                "MIN_SAFE_INTEGER" => Value::Num(-9007199254740991.0),
                "MAX_VALUE" => Value::Num(f64::MAX),
                "MIN_VALUE" => Value::Num(f64::MIN_POSITIVE),
                "EPSILON" => Value::Num(f64::EPSILON),
                "POSITIVE_INFINITY" => Value::Num(f64::INFINITY),
                "NEGATIVE_INFINITY" => Value::Num(f64::NEG_INFINITY),
                "NaN" => Value::Num(f64::NAN),
                "prototype" => self.number_proto.clone(),
                _ => Value::Undefined,
            }),
            Value::Native(Native::BooleanCtor) => Ok(match key {
                "prototype" => self.boolean_proto.clone(),
                _ => Value::Undefined,
            }),
            Value::Native(Native::RegExpCtor) => Ok(match key {
                "prototype" => self.regexp_proto.clone(),
                _ => Value::Undefined,
            }),
            // Promise 정적 메서드 + prototype (기능 탐지 'finally' in Promise.prototype)
            Value::Native(Native::PromiseCtor) => Ok(match key {
                "resolve" => Value::Native(Native::PromiseResolve),
                "reject" => Value::Native(Native::PromiseReject),
                "all" => Value::Native(Native::PromiseAll),
                "race" => Value::Native(Native::PromiseRace),
                "allSettled" => Value::Native(Native::PromiseAllSettled),
                "prototype" => {
                    let mut m = ObjMap::new();
                    m.insert("then".to_string(), Value::Native(Native::PromiseThen));
                    m.insert("catch".to_string(), Value::Native(Native::PromiseCatch));
                    m.insert("finally".to_string(), Value::Native(Native::PromiseFinally));
                    Value::Obj(Rc::new(RefCell::new(m)))
                }
                _ => Value::Undefined,
            }),
            // 네이티브/바운드 함수도 호출 어댑터 제공
            Value::Native(_) | Value::Bound(_) => match key {
                "call" => Ok(Value::Native(Native::FnCall)),
                "apply" => Ok(Value::Native(Native::FnApply)),
                "bind" => Ok(Value::Native(Native::FnBind)),
                "name" => Ok(Value::Str(String::new())),
                "length" => Ok(Value::Num(0.0)),
                // 내장 함수도 toString 을 가진다. jQuery 서두가
                // `fnToString = hasOwn.toString; fnToString.call(Object)` 로 이걸 쓴다 —
                // 없으면 undefined.call(...) 로 jQuery 전체가 즉사했다.
                "toString" => Ok(Value::Native(Native::FnToString)),
                _ => Ok(Value::Undefined),
            },
            // BigInt 메서드: toString(radix) / toLocaleString / valueOf
            Value::BigInt(_) => Ok(match key {
                "toString" | "toLocaleString" => Value::Native(Native::BigIntToString),
                "valueOf" => Value::Native(Native::ValueOfSelf),
                "constructor" => Value::Native(Native::BigIntCtor),
                _ => Value::Undefined,
            }),
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
                    "toFixed" | "toPrecision" => Value::Native(Native::NumToFixed),
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

    fn call_value(
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
                    // 수신자 없는 일반 호출: sloppy 모드처럼 this = window (undefined 아님)
                    let this = recv
                        .unwrap_or_else(|| env_get(&self.global, "window").unwrap_or(Value::Undefined));
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
                    return Ok(self.make_generator(func.clone(), scope));
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
            Value::Class(_) => self.construct(f, args), // 클래스를 함수처럼 호출 = new (관용)
            // 바운드 함수: 캡처한 this + 선행 인자 앞에 붙여 대상 호출
            Value::Bound(b) => {
                let (target, this_val, partial) = (*b).clone();
                let mut all = partial;
                all.extend(args);
                self.call_value(target, Some(this_val), all)
            }
            other => {
                let d = to_display(&other);
                Err(self.throw_error("TypeError", format!("{} 은(는) 함수가 아님", d)))
            }
        }
    }

    // new Class(args) / 클래스 호출: 인스턴스 생성 → 생성자 체인 실행 → 인스턴스 반환.
    fn construct(&mut self, class: Value, args: Vec<Value>) -> Result<Value, String> {
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
            Value::Native(Native::EventCtor) => {
                return self.call_native(Native::EventCtor, None, args)
            }
            Value::Native(Native::ProxyCtor) => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let handler = args.get(1).cloned().unwrap_or(Value::Undefined);
                return Ok(Value::Proxy(Rc::new((target, handler))));
            }
            Value::Native(Native::RegExpCtor) => {
                return self.call_native(Native::RegExpCtor, None, args)
            }
            // new String/Number/Boolean → 원시값 근사 (박싱 미구현)
            Value::Native(n @ (Native::StringCtor | Native::NumberCtor | Native::BooleanCtor)) => {
                return self.call_native(n, None, args)
            }
            Value::Native(Native::DateCtor) => return self.call_native(Native::DateCtor, None, args),
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
                // message 는 인자가 undefined 가 아닐 때만 own 프로퍼티 (§20.5.1.1)
                let msg = match args.first() {
                    None | Some(Value::Undefined) => None,
                    Some(v) => Some(to_display(v)),
                };
                return Ok(self.make_error(name, msg));
            }
            // 네이티브 생성자 스텁: new Error('m') / new Object() 등 → 객체
            // new f() — 일반 함수를 생성자로 (ES6 이전 패턴, 미니파이 코드 다수).
            // 새 객체의 __proto__ 를 f.prototype 에 '링크'(스냅샷 복사 아님) → 이후
            // F.prototype.m 추가도 인스턴스에 반영되고 프로토타입 체인 조회가 동작한다.
            // 함수가 객체를 반환하면 그것 우선(JS 규칙).
            Value::Fn(func) => {
                let obj = Rc::new(RefCell::new(ObjMap::new()));
                // f.prototype 지연 생성(없으면) 후 링크. borrow 를 먼저 끊고 match.
                let existing = func.props.borrow().get("prototype").cloned();
                let proto = match existing {
                    Some(p @ Value::Obj(_)) => p,
                    _ => {
                        let p = Value::Obj(Rc::new(RefCell::new(ObjMap::new())));
                        func.props.borrow_mut().insert("prototype".to_string(), p.clone());
                        p
                    }
                };
                obj.borrow_mut().insert("__proto__".to_string(), proto);
                let this = Value::Obj(obj);
                // new.target = 이 함수 (call_value 가 스코프에 심는다).
                self.new_target = Some(Value::Fn(func.clone()));
                let ret = self.call_value(Value::Fn(func), Some(this.clone()), args)?;
                // 표준: 생성자가 객체를 반환하면 그게 결과, 원시값이면 this.
                return Ok(if is_object(&ret) { ret } else { this });
            }
            Value::Obj(_) | Value::Native(_) => {
                let mut map = ObjMap::new();
                if let Some(a0) = args.first() {
                    map.insert("message".to_string(), a0.clone());
                }
                return Ok(Value::Obj(Rc::new(RefCell::new(map))));
            }
            other => return Err(format!("{} 은(는) 생성자가 아님", to_display(&other))),
        };
        let inst = Value::Instance(Rc::new(Instance {
            class: cls.clone(),
            fields: RefCell::new(HashMap::new()),
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
                i.fields.borrow_mut().insert(name.clone(), v);
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
                for (i, p) in ctor.params.iter().enumerate() {
                    env_declare(&scope, p, args.get(i).cloned().unwrap_or(Value::Undefined));
                }
                let flow = self.exec_block(&ctor.body, &scope)?;
                // 생성자 본문이 객체를 반환했거나, super() 가 this 를 갈아끼웠다면 그것이 결과다
                if let Flow::Return(v) = flow {
                    if is_object(&v) {
                        return Ok(Some(v));
                    }
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
        // 부모는 클래스일 수도, 일반 생성자(함수/네이티브/Array 같은 네임스페이스 객체)일
        // 수도 있다 — 표준은 아무 생성자나 확장 가능(class E extends Error 가 대표).
        let (parent, parent_ctor): (Option<Rc<JsClass>>, Option<Value>) = match &def.parent {
            Some(e) => match self.eval(e, env)? {
                Value::Class(c) => (Some(c), None),
                v @ (Value::Fn(_) | Value::Native(_) | Value::Obj(_) | Value::Bound(_)) => {
                    (None, Some(v))
                }
                other => return Err(format!("{} 은(는) 확장할 클래스가 아님", to_display(&other))),
            },
            None => (None, None),
        };
        let mk = |params: &Vec<String>, body: &Vec<Stmt>, is_generator: bool, is_async: bool| {
            Rc::new(JsFn {
                params: params.clone(),
                body: body.clone(),
                env: env.clone(),
                is_arrow: false,
                is_generator,
                is_async,
                this: None,
                // super.x → 이 클래스의 부모 (클래스 또는 일반 생성자)
                super_class: parent
                    .clone()
                    .map(Value::Class)
                    .or_else(|| parent_ctor.clone()),
                props: RefCell::new(HashMap::new()),
            })
        };
        let ctor = def.ctor.as_ref().map(|(p, b)| mk(p, b, false, false));
        let mut methods = HashMap::new();
        for (name, p, b, gen, asy) in &def.methods {
            methods.insert(name.clone(), mk(p, b, *gen, *asy));
        }
        let mut getters = HashMap::new();
        let mut setters = HashMap::new();
        for (name, p, b) in &def.setters {
            setters.insert(name.clone(), mk(p, b, false, false));
        }
        let mut static_getters = HashMap::new();
        for (name, p, b) in &def.static_getters {
            static_getters.insert(name.clone(), mk(p, b, false, false));
        }
        let mut static_setters = HashMap::new();
        for (name, p, b) in &def.static_setters {
            static_setters.insert(name.clone(), mk(p, b, false, false));
        }
        for (name, p, b) in &def.getters {
            getters.insert(name.clone(), mk(p, b, false, false));
        }
        // 인스턴스 필드: 초기화식을 무인자 함수로 감싸(this 바인딩+env) 생성 시 호출
        let mut fields = Vec::new();
        for (name, init) in &def.fields {
            let f = init
                .as_ref()
                .map(|e| mk(&Vec::new(), &vec![Stmt::Return(Some(e.clone()))], false, false));
            fields.push((name.clone(), f));
        }
        // 정적 멤버는 parent 가 cls 로 이동하기 전에 만든다 (mk 가 parent 참조)
        let mut statics = HashMap::new();
        for (name, p, b, gen, asy) in &def.statics {
            statics.insert(name.clone(), Value::Fn(mk(p, b, *gen, *asy)));
        }
        let cls = Rc::new(JsClass {
            proto_cache: RefCell::new(None),
            name: def.name.clone().unwrap_or_else(|| "(anonymous)".to_string()),
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
        });
        // static 필드: 클래스 완성 후 this=클래스로 평가해 statics 에 설정
        for (name, init) in &def.static_fields {
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
            cls.statics.borrow_mut().insert(name.clone(), v);
        }
        Ok(Value::Class(cls))
    }


    // ToPrimitive: 객체를 원시값으로 (valueOf/toString 호출). prefer_string 이면 toString 먼저.
    // 원시값은 그대로. 사용자 정의 toString/valueOf(BigNumber/moment/커스텀 값형)를 존중.
    fn to_primitive(&mut self, v: Value, prefer_string: bool) -> Value {
        if !matches!(v, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
            return v;
        }
        // Symbol.toPrimitive 가 있으면 그것이 우선한다 (표준 §7.1.1).
        if let Ok(f) = self.member_get(&v, "\u{0}@@toPrimitive") {
            if is_callable(&f) {
                let hint = Value::Str(if prefer_string { "string" } else { "number" }.to_string());
                if let Ok(res) = self.call_value(f, Some(v.clone()), vec![hint]) {
                    if !matches!(res, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
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
                        if !matches!(res, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
                            return res; // 원시값이면 채택
                        }
                    }
                }
            }
        }
        v
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
        let (l, r) = if matches!(
            op,
            BinOp::Add
                | BinOp::Sub
                | BinOp::Mul
                | BinOp::Div
                | BinOp::Mod
                | BinOp::Pow
                | BinOp::Lt
                | BinOp::Gt
                | BinOp::Le
                | BinOp::Ge
        ) {
            (self.to_primitive(l, false), self.to_primitive(r, false))
        } else {
            (l, r)
        };
        // BigInt 가 끼면 별도 의미론 (혼합 산술은 TypeError)
        if let Some(res) = self.bigint_binary(op, &l, &r) {
            return res;
        }
        Ok(match op {
            BinOp::Add => match (&l, &r) {
                (Value::Str(_), _) | (_, Value::Str(_)) => {
                    Value::Str(format!("{}{}", to_display(&l), to_display(&r)))
                }
                _ => Value::Num(to_num(&l) + to_num(&r)),
            },
            BinOp::Sub => Value::Num(to_num(&l) - to_num(&r)),
            BinOp::Mul => Value::Num(to_num(&l) * to_num(&r)),
            BinOp::Div => Value::Num(to_num(&l) / to_num(&r)),
            BinOp::Mod => Value::Num(to_num(&l) % to_num(&r)),
            BinOp::Pow => Value::Num(to_num(&l).powf(to_num(&r))),
            BinOp::BitAnd => Value::Num((to_i32(&l) & to_i32(&r)) as f64),
            BinOp::BitOr => Value::Num((to_i32(&l) | to_i32(&r)) as f64),
            BinOp::BitXor => Value::Num((to_i32(&l) ^ to_i32(&r)) as f64),
            BinOp::Shl => Value::Num((to_i32(&l) << (to_i32(&r) & 31)) as f64),
            BinOp::Shr => Value::Num((to_i32(&l) >> (to_i32(&r) & 31)) as f64),
            BinOp::UShr => Value::Num(((to_i32(&l) as u32) >> (to_i32(&r) & 31)) as f64),
            // in: 프로토타입 체인까지 본다 (표준 §13.10). Proxy 면 has 트랩.
            BinOp::In => {
                let key = to_display(&l);
                match &r {
                    Value::Proxy(p) => {
                        let (target, handler) = (p.0.clone(), p.1.clone());
                        let trap = self.member_get(&handler, "has")?;
                        if is_callable(&trap) {
                            let res = self.call_value(
                                trap,
                                Some(handler),
                                vec![target, Value::Str(key)],
                            )?;
                            return Ok(Value::Bool(to_bool(&res)));
                        }
                        return self.binary(BinOp::In, l, target);
                    }
                    Value::Obj(m) => {
                        let mut cur = Some(m.clone());
                        while let Some(o) = cur {
                            let b = o.borrow();
                            if b.contains_key(&key) {
                                return Ok(Value::Bool(true));
                            }
                            cur = match b.get("__proto__") {
                                Some(Value::Obj(p)) => Some(p.clone()),
                                _ => None,
                            };
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
                    Value::Arr(a) => Value::Bool(
                        key.parse::<usize>().map_or(false, |i| i < a.borrow().len()),
                    ),
                    _ => Value::Bool(false),
                }
            }
            BinOp::Instanceof => {
                // 표준 §13.10.2: 오른쪽에 [Symbol.hasInstance] 가 있으면 **그것이 최우선**이다.
                // (Symbol.hasInstance 로 instanceof 를 커스터마이즈하는 라이브러리가 있다)
                let hi = self.member_get(&r, "\u{0}@@hasInstance").unwrap_or(Value::Undefined);
                if is_callable(&hi) {
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
                    let (x, y) = (to_num(&l), to_num(&r));
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
                match recv {
                    // Proxy: set 트랩 있으면 handler.set(target, key, value, receiver), 없으면 target 에 위임
                    Value::Proxy(p) => {
                        let (target, handler) = (p.0.clone(), p.1.clone());
                        let trap = self.member_get(&handler, "set")?;
                        if !matches!(trap, Value::Undefined) {
                            let receiver = Value::Proxy(p.clone());
                            self.call_value(
                                trap,
                                Some(handler),
                                vec![target, Value::Str(key), value, receiver],
                            )?;
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
                            let is_new = i >= a.borrow().len();
                            if is_new && self.is_nonextensible_val(&av) {
                                return Ok(());
                            }
                            let mut arr = a.borrow_mut();
                            if i >= arr.len() {
                                arr.resize(i + 1, Value::Undefined);
                            }
                            arr[i] = value;
                        } else if key == "length" {
                            let n = to_num(&value).max(0.0) as usize;
                            a.borrow_mut().resize(n, Value::Undefined);
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
                        if !inst.fields.borrow().contains_key(&key)
                            && self.is_nonextensible_val(&iv)
                        {
                            return Ok(());
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
                    // 함수 프로퍼티 (F.prototype, F.staticProp = ...)
                    Value::Fn(func) => {
                        let fv = Value::Fn(func.clone());
                        if self.is_frozen_val(&fv) {
                            return Ok(());
                        }
                        if !func.props.borrow().contains_key(&key)
                            && self.is_nonextensible_val(&fv)
                        {
                            return Ok(());
                        }
                        func.props.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    // 내장(네이티브)에 프로퍼티 얹기 — 폴리필의
                    // `if (!Promise.allSettled) Promise.allSettled = fn` 패턴.
                    Value::Native(n) => {
                        self.native_props.entry(n).or_default().insert(key, value);
                        Ok(())
                    }
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
