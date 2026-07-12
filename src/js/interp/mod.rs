// 트리 워킹 인터프리터. Value/Env(렉시컬 체인)/제어 흐름.
// 무한 루프로 브라우저가 멈추지 않도록 실행 스텝 한도를 둔다.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::ast::*;
use super::parser::parse;

mod builtins;
mod value;
mod dom_api;
use value::*;

const STEP_LIMIT: u64 = 5_000_000;
// 이 접두사의 에러는 try/catch 로 잡을 수 없다 (무한 루프 가드가 무력화되지 않게)
const STEP_LIMIT_MSG: &str = "실행 한도 초과";

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Obj(Rc<RefCell<HashMap<String, Value>>>),
    // 배열은 항목 + own-property 맵을 가진 객체(표준). arr.push 재정의 등 지원.
    Arr(Rc<ArrayObj>),
    Fn(Rc<JsFn>),
    Native(Native),
    // DOM 요소 핸들: 아레나 NodeId (구조 변형에도 안정)
    Dom(crate::dom::NodeId),
    Class(Rc<JsClass>),
    Instance(Rc<Instance>),
    // bind 로 만든 바운드 함수: (대상, this, 선행 인자)
    Bound(Rc<(Value, Value, Vec<Value>)>),
    // Object.defineProperty 의 접근자(get). 객체 맵에만 저장되고, 멤버 읽기 때
    // 호출돼 실제 값을 낸다. 다른 경로엔 노출되지 않음.
    Getter(Rc<Value>),
    // Map/Set — 삽입 순서 보존. 키 비교는 strict_eq (객체는 참조 동일).
    MapVal(Rc<RefCell<Vec<(Value, Value)>>>),
    SetVal(Rc<RefCell<Vec<Value>>>),
    // element.style — 요소의 inline style 속성에 대한 라이브 프록시(CSSStyleDeclaration).
    Style(crate::dom::NodeId),
    // element.classList — 요소의 class 속성에 대한 라이브 프록시(DOMTokenList).
    ClassList(crate::dom::NodeId),
    // new Proxy(target, handler) — get/set/has 트랩 지원 (프레임워크 반응성).
    Proxy(Rc<(Value, Value)>),
}

// 배열 객체: 인덱스 항목(items)과 own-property(props)를 분리 보관.
// borrow()/borrow_mut() 는 items 로 위임 — 기존 접근 코드가 그대로 동작한다.
// props 는 arr.push=fn 재정의나 arr.customProp=x 같은 표준 동작을 위한 것.
pub struct ArrayObj {
    items: RefCell<Vec<Value>>,
    props: RefCell<HashMap<String, Value>>,
}

impl ArrayObj {
    pub fn new(items: Vec<Value>) -> Rc<ArrayObj> {
        Rc::new(ArrayObj { items: RefCell::new(items), props: RefCell::new(HashMap::new()) })
    }
    pub fn borrow(&self) -> std::cell::Ref<'_, Vec<Value>> {
        self.items.borrow()
    }
    pub fn borrow_mut(&self) -> std::cell::RefMut<'_, Vec<Value>> {
        self.items.borrow_mut()
    }
    pub fn get_prop(&self, k: &str) -> Option<Value> {
        self.props.borrow().get(k).cloned()
    }
    pub fn set_prop(&self, k: String, v: Value) {
        self.props.borrow_mut().insert(k, v);
    }
}

pub struct JsFn {
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
    pub env: EnvRef, // 클로저가 캡처한 렉시컬 환경
    pub is_arrow: bool,
    pub is_generator: bool, // function* — 호출 시 yield 값을 모아 반복자 반환(eager)
    pub is_async: bool, // async — 반환값을 이행된 Promise 로 감싼다
    pub this: Option<Box<Value>>, // 화살표가 정의 시점에 캡처한 this
    // 이 함수가 클래스 메서드면 그 클래스의 부모 (super.x 해석용)
    pub super_class: Option<Rc<JsClass>>,
    // 함수도 객체: F.prototype / F.staticProp 등 (Rc 공유 → 변경 반영)
    pub props: RefCell<HashMap<String, Value>>,
}

pub struct JsClass {
    pub name: String,
    pub parent: Option<Rc<JsClass>>,
    pub ctor: Option<Rc<JsFn>>,
    pub methods: HashMap<String, Rc<JsFn>>,
    pub getters: HashMap<String, Rc<JsFn>>,
    // 인스턴스 필드 초기화 함수 (없으면 None → undefined). 생성 시 this 로 호출.
    pub fields: Vec<(String, Option<Rc<JsFn>>)>,
    pub statics: RefCell<HashMap<String, Value>>,
}

impl JsClass {
    // 자신부터 조상까지 메서드 탐색
    fn find_method(&self, name: &str) -> Option<Rc<JsFn>> {
        if let Some(m) = self.methods.get(name) {
            return Some(m.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_method(name))
    }

    // get 접근자 탐색 (자신 → 조상)
    fn find_getter(&self, name: &str) -> Option<Rc<JsFn>> {
        if let Some(g) = self.getters.get(name) {
            return Some(g.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_getter(name))
    }
}

pub struct Instance {
    pub class: Rc<JsClass>,
    pub fields: RefCell<HashMap<String, Value>>,
}

// canvas 2D 컨텍스트 메서드
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CanvasMethod {
    FillRect,
    ClearRect,
    StrokeRect,
    BeginPath,
    MoveTo,
    LineTo,
    Arc,
    Rect,
    ClosePath,
    Fill,
    Stroke,
    FillText,
    Noop, // save/restore/scale/translate/setTransform 등 (근사로 무시)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Native {
    ConsoleLog,
    ArrayPush,
    GetElementById,
    AddEventListener,
    AddGlobalListener,
    FnCall,
    FnApply,
    FnBind,
    FunctionCtor,
    ObjectDefineProperty,
    ObjectCreate,
    ObjectFreeze,
    ObjectGetPrototypeOf,
    HasOwnProperty,
    ObjToString,
    ReturnFalse,
    ReturnThis, // valueOf 등 — 수신자(this) 반환
    FnToString, // Function.prototype.toString
    MakeIter,
    IterNext,
    DocQuery(&'static str),
    CreateTextNode,
    InsertBefore,
    StyleSetProperty,
    StyleGetProperty,
    StyleRemoveProperty,
    ClassAdd,
    ClassRemove,
    ClassToggle,
    ClassContains,
    RegExpCtor,
    RegexTest,
    RegexExec,
    StringCtor,
    NumberCtor,
    BooleanCtor,
    StrFromCharCode,
    NumIsInteger,
    NumIsFinite,
    NumIsNaN,
    NumToFixed,
    ValueToStr, // recv.toString([radix]) → 문자열
    ValueOfSelf, // recv.valueOf() → recv
    DateNow,
    DateCtor,
    DateMethod(DateField),
    XhrCtor,
    UrlCtor,
    UrlToString,
    UrlSearchGet,
    UrlSearchGetAll,
    UrlSearchHas,
    UrlSearchToString,
    XhrOpen,
    XhrSend,
    XhrSetHeader,
    XhrGetHeader,
    EventPreventDefault,
    EventStopProp,
    GetElementsByClass,
    GetElementsByTag,
    MapCtor,
    SetCtor,
    Map(MapOp),
    Set(SetOp),
    ErrorCtor(&'static str),
    CreateElement,
    AppendChild,
    NodeAppend,
    NodePrepend,
    NodeBefore,
    NodeAfter,
    NodeReplaceWith,
    GetBoundingClientRect,
    DispatchEvent,
    EventCtor,
    CloneNode,
    Matches,
    Closest,
    DomContains,
    CreateDocumentFragment,
    ProxyCtor,
    CanvasGetContext,
    Canvas(CanvasMethod),
    RemoveElement,
    SetAttribute,
    GetAttribute,
    QuerySelector,
    QuerySelectorAll,
    Math(MathOp),
    Str(StrOp),
    Arr(ArrOp),
    JsonParse,
    JsonStringify,
    ParseInt,
    ParseFloat,
    EncodeUri,
    EncodeUriComponent,
    DecodeUri,
    DecodeUriComponent,
    IsNaN,
    LsGetItem,
    LsSetItem,
    LsRemoveItem,
    LsClear,
    Alert,
    // 받고 아무것도 안 함 (window.addEventListener 등 — 창 이벤트는 아직 없음)
    Noop,
    ObjectKeys,
    ObjectAssign,
    ArrayIsArray,
    SetTimeout,
    SetInterval,
    ClearTimer,
    // Promise/fetch
    PromiseResolve,
    PromiseReject,
    PromiseAll,
    PromiseRace,
    PromiseAllSettled,
    PromiseThen,
    PromiseCatch,
    Identity, // 값 통과 (promise 체이닝용)
    Fetch,
    ResponseText,
    ResponseJson,
    RemoveAttribute,
    HasAttribute,
    RemoveChild,
}

// 예약된 타이머 (창 이벤트 루프 / 헤드리스 flush 가 실행)
pub struct Timer {
    pub id: u64,
    pub callback: Value,
    pub delay_ms: f64,
    pub repeat: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MathOp {
    Floor,
    Ceil,
    Round,
    Abs,
    Min,
    Max,
    Sqrt,
    Pow,
    Random,
    Trunc,
    Sign,
    Cbrt,
    Log,
    Log2,
    Log10,
    Exp,
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Hypot,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StrOp {
    IndexOf,
    Slice,
    Split,
    Upper,
    Lower,
    Trim,
    Replace,
    ReplaceAll,
    CharAt,
    Includes,
    StartsWith,
    EndsWith,
    Match,
    MatchAll,
    Search,
    PadStart,
    PadEnd,
    Repeat,
    TrimStart,
    TrimEnd,
    CharCodeAt,
    CodePointAt,
    Concat,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum DateField {
    Time,
    FullYear,
    Month,
    Date,
    Day,
    Hours,
    Minutes,
    Seconds,
    Ms,
    TimezoneOffset,
    ToIso,
    ToStr,
    ToDateStr,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MapOp {
    Get,
    Set,
    Has,
    Delete,
    Clear,
    ForEach,
    Keys,
    Values,
    Entries,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SetOp {
    Add,
    Has,
    Delete,
    Clear,
    ForEach,
    Values,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ArrOp {
    Join,
    Pop,
    IndexOf,
    Slice,
    ForEach,
    Map,
    Filter,
    Some,
    Every,
    Reduce,
    Find,
    FindIndex,
    Concat,
    Includes,
    Splice,
    Shift,
    Unshift,
    Reverse,
    Keys,
    Values,
    Sort,
    Flat,
}

impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Value::Undefined => write!(f, "undefined"),
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::Num(n) => write!(f, "{}", n),
            Value::Str(s) => write!(f, "{:?}", s),
            Value::Obj(_) => write!(f, "[object]"),
            Value::Arr(_) => write!(f, "[array]"),
            Value::Fn(_) => write!(f, "[function]"),
            Value::Native(n) => write!(f, "[native {:?}]", n),
            Value::Dom(p) => write!(f, "[dom {:?}]", p),
            Value::Class(c) => write!(f, "[class {}]", c.name),
            Value::Instance(i) => write!(f, "[instance {}]", i.class.name),
            Value::Bound(_) => write!(f, "[bound function]"),
            Value::Getter(_) => write!(f, "[getter]"),
            Value::MapVal(_) => write!(f, "[object Map]"),
            Value::SetVal(_) => write!(f, "[object Set]"),
            Value::Style(id) => write!(f, "[style {:?}]", id),
            Value::ClassList(id) => write!(f, "[classList {:?}]", id),
            Value::Proxy(_) => write!(f, "[object Proxy]"),
        }
    }
}

// ── 환경 (스코프 체인) ──────────────────────────────────────────────

pub type EnvRef = Rc<RefCell<Env>>;

pub struct Env {
    vars: HashMap<String, Value>,
    parent: Option<EnvRef>,
}

impl Env {
    fn new(parent: Option<EnvRef>) -> EnvRef {
        Rc::new(RefCell::new(Env { vars: HashMap::new(), parent }))
    }
}

fn env_get(env: &EnvRef, name: &str) -> Option<Value> {
    if let Some(v) = env.borrow().vars.get(name) {
        return Some(v.clone());
    }
    let parent = env.borrow().parent.clone();
    parent.and_then(|p| env_get(&p, name))
}

// 체인에서 기존 바인딩을 갱신. 없으면 전역(최상위)에 새로 만든다 (sloppy 모드 유사).
fn env_set(env: &EnvRef, name: &str, value: Value) {
    {
        let mut e = env.borrow_mut();
        if e.vars.contains_key(name) {
            e.vars.insert(name.to_string(), value);
            return;
        }
    }
    let parent = env.borrow().parent.clone();
    match parent {
        Some(p) => env_set(&p, name, value),
        None => {
            env.borrow_mut().vars.insert(name.to_string(), value);
        }
    }
}

fn env_declare(env: &EnvRef, name: &str, value: Value) {
    env.borrow_mut().vars.insert(name.to_string(), value);
}

// 프로토타입 객체(Value::Obj)에서 키를 꺼낸다 (원시값이 자기 프로토타입 참조용).
fn proto_prop(proto: &Value, key: &str) -> Value {
    if let Value::Obj(m) = proto {
        return m.borrow().get(key).cloned().unwrap_or(Value::Undefined);
    }
    Value::Undefined
}

// var 하이스팅: 함수/전역 진입 시 몸통의 모든 var 이름을 undefined 로 미리 선언.
// 제어흐름 몸통(if/for/while/try/switch/block)은 파고들되, 중첩 함수 몸통은 제외
// (var 은 함수 스코프). 이미 있는 이름(파라미터 등)은 덮지 않는다.
fn hoist_vars(stmts: &[Stmt], scope: &EnvRef) {
    for s in stmts {
        hoist_stmt(s, scope);
    }
}

fn pattern_names(pat: &crate::js::ast::Pattern, out: &mut Vec<String>) {
    use crate::js::ast::Pattern;
    match pat {
        Pattern::Name(n) => out.push(n.clone()),
        Pattern::Object(props, rest) => {
            for (_, sub, _) in props {
                pattern_names(sub, out);
            }
            if let Some(r) = rest {
                out.push(r.clone());
            }
        }
        Pattern::Array(elems, rest) => {
            for slot in elems.iter().flatten() {
                pattern_names(&slot.0, out);
            }
            if let Some(r) = rest {
                out.push(r.clone());
            }
        }
    }
}

fn hoist_stmt(s: &Stmt, scope: &EnvRef) {
    match s {
        Stmt::VarDecl { kind: crate::js::ast::DeclKind::Var, decls } => {
            for (pat, _) in decls {
                let mut names = Vec::new();
                pattern_names(pat, &mut names);
                for n in names {
                    if !scope.borrow().vars.contains_key(&n) {
                        env_declare(scope, &n, Value::Undefined);
                    }
                }
            }
        }
        Stmt::If { then, other, .. } => {
            hoist_vars(then, scope);
            if let Some(o) = other {
                hoist_vars(o, scope);
            }
        }
        Stmt::While { body, .. }
        | Stmt::DoWhile { body, .. }
        | Stmt::Block(body)
        | Stmt::ForIn { body, .. }
        | Stmt::ForOf { body, .. } => hoist_vars(body, scope),
        Stmt::For { init, body, .. } => {
            if let Some(init) = init {
                hoist_stmt(init, scope);
            }
            hoist_vars(body, scope);
        }
        Stmt::Try { body, catch, finally } => {
            hoist_vars(body, scope);
            if let Some((_, cb)) = catch {
                hoist_vars(cb, scope);
            }
            if let Some(fb) = finally {
                hoist_vars(fb, scope);
            }
        }
        Stmt::Switch { cases, .. } => {
            for (_, body) in cases {
                hoist_vars(body, scope);
            }
        }
        _ => {} // FuncDecl/ClassDecl 몸통은 별도 스코프 → 하이스트 안 함
    }
}

// ── 값 변환 ────────────────────────────────────────────────────────


// ── 제어 흐름 ──────────────────────────────────────────────────────

enum Flow {
    Normal(Value),
    Return(Value),
    Break,
    Continue,
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
}

pub struct Interp {
    pub global: EnvRef,
    pub console: Vec<String>, // console.log 캡처 (호출측이 터미널에 출력)
    steps: u64,
    // DOM 바인딩이 사용 (실행 동안만 유효한 아레나 포인터)
    pub dom: Option<*mut crate::dom::Dom>,
    // 이벤트 핸들러 레지스트리: (요소 NodeId, 이벤트 타입, 핸들러 함수)
    pub handlers: Vec<(crate::dom::NodeId, String, Value)>,
    // 레이아웃 산출 요소 사각형 (NodeId → (x, y, w, h), CSS px). 리빌드 후 호스트가 채움.
    // getBoundingClientRect/offsetWidth 등이 읽는다. 빈 맵이면 0 을 돌려준다.
    pub layout_rects: std::collections::HashMap<crate::dom::NodeId, (f32, f32, f32, f32)>,
    // 제너레이터 호출 스택별 yield 값 수집기 (eager). Expr::Yield 가 top 에 쌓는다.
    yield_sink: Vec<Vec<Value>>,
    // <canvas> 2D 그리기 명령 (NodeId → ops). 호스트가 렌더 시 DisplayItem 으로 변환.
    pub canvas_cmds: std::collections::HashMap<crate::dom::NodeId, Vec<CanvasOp>>,
    // document/window 레벨 핸들러: (이벤트 타입, 핸들러) — DOMContentLoaded/load 등
    pub global_handlers: Vec<(String, Value)>,
    // Math.random 용 xorshift 상태
    rng: u64,
    // throw 된 값 (에러 채널은 String 이라 값은 사이드 채널로 전달)
    thrown: Option<Value>,
    // localStorage 스텁 저장소 (페이지 수명)
    storage: HashMap<String, String>,
    // setTimeout/setInterval 로 등록된 미처리 타이머 (호출측이 드레인해 실행)
    pub timers: Vec<Timer>,
    pub cleared: std::collections::HashSet<u64>,
    next_timer_id: u64,
    // Promise 마이크로태스크 큐: (콜백, 인자, 의존 promise). 스크립트/타이머 후 드레인.
    microtasks: std::collections::VecDeque<(Value, Value, Value)>,
    // Function.prototype (call/apply/bind). 정체성 보존 위해 보관.
    fn_proto: Value,
    // String.prototype (문자열 메서드) — String.prototype.slice.call(x) 용.
    string_proto: Value,
    // Number/Boolean/RegExp.prototype — core-js uncurryThis(Constructor.prototype.method) 용.
    number_proto: Value,
    boolean_proto: Value,
    regexp_proto: Value,
    // 페이지 기준 URL (상대 URL 해석용 — XHR/fetch)
    base_url: Option<String>,
    // 진단용 관대 모드(KESTREL_LENIENT): undefined 멤버 접근/호출을 에러 대신
    // undefined 로 (표준 아님, naver 등 롱테일 거리 측정용). 접근 키를 집계.
    lenient: bool,
    pub lenient_hits: std::collections::HashMap<String, usize>,
}

impl Interp {
    pub fn new() -> Interp {
        let global = Env::new(None);
        // console.log
        let mut console = HashMap::new();
        console.insert("log".to_string(), Value::Native(Native::ConsoleLog));
        env_declare(&global, "console", Value::Obj(Rc::new(RefCell::new(console))));
        // document (dom 포인터 미설정 시 호출하면 런타임 에러)
        let mut document = HashMap::new();
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
        document.insert("removeEventListener".to_string(), Value::Native(Native::Noop));
        // 스크립트 실행 중엔 "loading" — 프레임워크가 DOMContentLoaded 리스너를
        // 등록하도록. run_scripts 가 이후 interactive → complete 로 갱신.
        document.insert("readyState".to_string(), Value::Str("loading".to_string()));
        // 흔한 document 프로퍼티(미정의 크래시 방지). cookie 는 간이(문자열).
        document.insert("cookie".to_string(), Value::Str(String::new()));
        document.insert("title".to_string(), Value::Str(String::new()));
        document.insert("referrer".to_string(), Value::Str(String::new()));
        document.insert("characterSet".to_string(), Value::Str("UTF-8".to_string()));
        document.insert("compatMode".to_string(), Value::Str("CSS1Compat".to_string()));
        document.insert("hidden".to_string(), Value::Bool(false));
        document.insert("visibilityState".to_string(), Value::Str("visible".to_string()));
        document.insert("createTextNode".to_string(), Value::Native(Native::CreateTextNode));
        document
            .insert("getElementsByClassName".to_string(), Value::Native(Native::GetElementsByClass));
        document.insert("getElementsByTagName".to_string(), Value::Native(Native::GetElementsByTag));
        // 라이브 접근자: document.body/head/documentElement → DOM 요소 핸들
        let live = |tag| Value::Getter(Rc::new(Value::Native(Native::DocQuery(tag))));
        document.insert("body".to_string(), live("body"));
        document.insert("head".to_string(), live("head"));
        document.insert("documentElement".to_string(), live("html"));
        env_declare(&global, "document", Value::Obj(Rc::new(RefCell::new(document))));
        // Math
        let mut math = HashMap::new();
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
        let mut json = HashMap::new();
        json.insert("parse".to_string(), Value::Native(Native::JsonParse));
        json.insert("stringify".to_string(), Value::Native(Native::JsonStringify));
        env_declare(&global, "JSON", Value::Obj(Rc::new(RefCell::new(json))));
        // 전역 함수
        env_declare(&global, "parseInt", Value::Native(Native::ParseInt));
        env_declare(&global, "parseFloat", Value::Native(Native::ParseFloat));
        env_declare(&global, "encodeURI", Value::Native(Native::EncodeUri));
        env_declare(&global, "encodeURIComponent", Value::Native(Native::EncodeUriComponent));
        env_declare(&global, "decodeURI", Value::Native(Native::DecodeUri));
        env_declare(&global, "decodeURIComponent", Value::Native(Native::DecodeUriComponent));
        env_declare(&global, "isNaN", Value::Native(Native::IsNaN));
        env_declare(&global, "isFinite", Value::Native(Native::NumIsFinite));
        // 타이머
        env_declare(&global, "setTimeout", Value::Native(Native::SetTimeout));
        env_declare(&global, "setInterval", Value::Native(Native::SetInterval));
        env_declare(&global, "clearTimeout", Value::Native(Native::ClearTimer));
        env_declare(&global, "clearInterval", Value::Native(Native::ClearTimer));
        env_declare(&global, "requestAnimationFrame", Value::Native(Native::SetTimeout));
        // 전역 생성자 스텁 (instanceof 판별 + 정적 메서드)
        let mut object_ns = HashMap::new();
        object_ns.insert("keys".to_string(), Value::Native(Native::ObjectKeys));
        object_ns.insert("assign".to_string(), Value::Native(Native::ObjectAssign));
        object_ns.insert("defineProperty".to_string(), Value::Native(Native::ObjectDefineProperty));
        object_ns.insert("defineProperties".to_string(), Value::Native(Native::ObjectDefineProperty));
        object_ns.insert("create".to_string(), Value::Native(Native::ObjectCreate));
        object_ns.insert("freeze".to_string(), Value::Native(Native::ObjectFreeze));
        object_ns.insert(
            "getPrototypeOf".to_string(),
            Value::Native(Native::ObjectGetPrototypeOf),
        );
        // Object.prototype: hasOwnProperty(webpack .o), toString(타입 판별 관용),
        // isPrototypeOf/propertyIsEnumerable/valueOf
        let mut object_proto = HashMap::new();
        object_proto.insert("hasOwnProperty".to_string(), Value::Native(Native::HasOwnProperty));
        object_proto.insert("toString".to_string(), Value::Native(Native::ObjToString));
        object_proto.insert("toLocaleString".to_string(), Value::Native(Native::ObjToString));
        object_proto.insert("valueOf".to_string(), Value::Native(Native::ReturnThis));
        object_proto
            .insert("propertyIsEnumerable".to_string(), Value::Native(Native::HasOwnProperty));
        object_proto.insert("isPrototypeOf".to_string(), Value::Native(Native::ReturnFalse));
        object_proto
            .insert("propertyIsEnumerable".to_string(), Value::Native(Native::HasOwnProperty));
        object_ns.insert("prototype".to_string(), Value::Obj(Rc::new(RefCell::new(object_proto))));
        env_declare(&global, "Object", Value::Obj(Rc::new(RefCell::new(object_ns))));
        // Array.prototype: 모든 배열 메서드를 담아 Array.prototype.slice.call(x) 지원
        let mut array_ns = HashMap::new();
        array_ns.insert("isArray".to_string(), Value::Native(Native::ArrayIsArray));
        let mut array_proto = HashMap::new();
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
        ] {
            array_proto.insert(name.to_string(), Value::Native(Native::Arr(op)));
        }
        array_proto.insert("push".to_string(), Value::Native(Native::ArrayPush));
        // Array.prototype[Symbol.iterator]/toString — core-js uncurryThis 참조
        array_proto.insert("@@iterator".to_string(), Value::Native(Native::MakeIter));
        array_proto.insert("toString".to_string(), Value::Native(Native::Arr(ArrOp::Join)));
        array_ns.insert("prototype".to_string(), Value::Obj(Rc::new(RefCell::new(array_proto))));
        env_declare(&global, "Array", Value::Obj(Rc::new(RefCell::new(array_ns))));
        env_declare(&global, "RegExp", Value::Native(Native::RegExpCtor));
        env_declare(&global, "String", Value::Native(Native::StringCtor));
        env_declare(&global, "Number", Value::Native(Native::NumberCtor));
        env_declare(&global, "Boolean", Value::Native(Native::BooleanCtor));
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
        // Map / Set / WeakMap / WeakSet (약한 참조는 일반 Map/Set 으로 근사)
        env_declare(&global, "Map", Value::Native(Native::MapCtor));
        env_declare(&global, "WeakMap", Value::Native(Native::MapCtor));
        env_declare(&global, "Set", Value::Native(Native::SetCtor));
        env_declare(&global, "WeakSet", Value::Native(Native::SetCtor));
        env_declare(&global, "Event", Value::Native(Native::EventCtor));
        env_declare(&global, "CustomEvent", Value::Native(Native::EventCtor));
        env_declare(&global, "Proxy", Value::Native(Native::ProxyCtor));
        // localStorage: 페이지 수명 동안 실제로 동작하는 인메모리 스토리지
        let mut ls = HashMap::new();
        ls.insert("getItem".to_string(), Value::Native(Native::LsGetItem));
        ls.insert("setItem".to_string(), Value::Native(Native::LsSetItem));
        ls.insert("removeItem".to_string(), Value::Native(Native::LsRemoveItem));
        ls.insert("clear".to_string(), Value::Native(Native::LsClear));
        let ls = Value::Obj(Rc::new(RefCell::new(ls)));
        env_declare(&global, "localStorage", ls.clone());
        env_declare(&global, "sessionStorage", ls.clone());
        // navigator / alert
        let mut nav = HashMap::new();
        nav.insert("userAgent".to_string(), Value::Str("Kestrel/0.1".to_string()));
        let nav = Value::Obj(Rc::new(RefCell::new(nav)));
        env_declare(&global, "navigator", nav.clone());
        env_declare(&global, "alert", Value::Native(Native::Alert));
        // window: 전역 객체 스텁 — 프로퍼티 읽기/쓰기는 되지만 전역 변수와
        // 연동되진 않음 (window.x = 1 후 x 로 읽기 미지원). 존재 자체가
        // "window 미정의" 즉사를 막는다. 필드 테스트 최다 런타임 에러.
        let mut window = HashMap::new();
        window.insert("localStorage".to_string(), ls);
        window.insert("navigator".to_string(), nav);
        window.insert("addEventListener".to_string(), Value::Native(Native::AddGlobalListener));
        window.insert("removeEventListener".to_string(), Value::Native(Native::Noop));
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
        let mut screen = HashMap::new();
        for (k, v) in [("width", 1000.0), ("height", 800.0), ("availWidth", 1000.0), ("availHeight", 800.0), ("colorDepth", 24.0), ("pixelDepth", 24.0)] {
            screen.insert(k.to_string(), Value::Num(v));
        }
        window.insert("screen".to_string(), Value::Obj(Rc::new(RefCell::new(screen.clone()))));
        let window = Value::Obj(Rc::new(RefCell::new(window)));
        // self / globalThis 는 전역 객체(window) 별칭 (구글/워커 코드)
        env_declare(&global, "window", window.clone());
        env_declare(&global, "self", window.clone());
        env_declare(&global, "screen", Value::Obj(Rc::new(RefCell::new(screen))));
        // 최상위 this = window (sloppy 스크립트: `(function(){this.x=…}).call(this)` 등)
        env_declare(&global, "this", window.clone());
        env_declare(&global, "globalThis", window);
        // Promise.resolve / Promise.reject (생성자 호출은 미지원, 정적 메서드만)
        let mut promise = HashMap::new();
        promise.insert("resolve".to_string(), Value::Native(Native::PromiseResolve));
        promise.insert("reject".to_string(), Value::Native(Native::PromiseReject));
        promise.insert("all".to_string(), Value::Native(Native::PromiseAll));
        promise.insert("race".to_string(), Value::Native(Native::PromiseRace));
        promise.insert("allSettled".to_string(), Value::Native(Native::PromiseAllSettled));
        env_declare(&global, "Promise", Value::Obj(Rc::new(RefCell::new(promise))));
        // fetch(url) — 동기 HTTP 후 resolved Promise(Response) 반환
        env_declare(&global, "fetch", Value::Native(Native::Fetch));
        // Function.prototype (call/apply/bind) — 폴리필이 Function.prototype.call.apply
        // 등으로 광범위하게 참조. 정체성 보존 위해 필드로 보관.
        let mut fn_proto = HashMap::new();
        fn_proto.insert("call".to_string(), Value::Native(Native::FnCall));
        fn_proto.insert("apply".to_string(), Value::Native(Native::FnApply));
        fn_proto.insert("bind".to_string(), Value::Native(Native::FnBind));
        // Function.prototype.toString — core-js 등이 uncurryThis 로 참조
        fn_proto.insert("toString".to_string(), Value::Native(Native::FnToString));
        let fn_proto = Value::Obj(Rc::new(RefCell::new(fn_proto)));
        // String.prototype: 문자열 메서드 (String.prototype.slice.call(x) 지원)
        let mut string_proto = HashMap::new();
        for (name, op) in [
            ("charAt", StrOp::CharAt),
            ("charCodeAt", StrOp::CharCodeAt),
            ("indexOf", StrOp::IndexOf),
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
            let mut m = HashMap::new();
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
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64 | 1)
            .unwrap_or(0x9e3779b9);
        Interp {
            global,
            console: Vec::new(),
            steps: 0,
            dom: None,
            handlers: Vec::new(),
            layout_rects: std::collections::HashMap::new(),
            yield_sink: Vec::new(),
            canvas_cmds: std::collections::HashMap::new(),
            global_handlers: Vec::new(),
            rng: seed,
            thrown: None,
            storage: HashMap::new(),
            timers: Vec::new(),
            cleared: std::collections::HashSet::new(),
            next_timer_id: 1,
            microtasks: std::collections::VecDeque::new(),
            fn_proto,
            string_proto,
            number_proto,
            boolean_proto,
            regexp_proto,
            base_url: None,
            lenient: std::env::var("KESTREL_LENIENT").is_ok(),
            lenient_hits: std::collections::HashMap::new(),
        }
    }

    // 새 pending Promise (Obj 표현: 상태·값·대기콜백을 맵에 저장, then/catch 는 Native)
    fn new_promise(&self) -> Value {
        let mut m = HashMap::new();
        m.insert("__isPromise".to_string(), Value::Bool(true));
        m.insert("__state".to_string(), Value::Str("pending".to_string()));
        m.insert("__value".to_string(), Value::Undefined);
        m.insert("__cbs".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
        m.insert("then".to_string(), Value::Native(Native::PromiseThen));
        m.insert("catch".to_string(), Value::Native(Native::PromiseCatch));
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // promise 를 값으로 이행. 값이 또 promise 면 그것이 이행될 때 이어서 이행(체이닝).
    fn resolve_promise(&mut self, p: &Value, v: Value) {
        if is_promise(&v) {
            // v 가 이행되면 p 도 같은 값으로 (Identity 콜백으로 위임)
            self.promise_then(&v, Value::Native(Native::Identity), p.clone());
            return;
        }
        let Value::Obj(o) = p else { return };
        {
            let mut m = o.borrow_mut();
            m.insert("__state".to_string(), Value::Str("fulfilled".to_string()));
            m.insert("__value".to_string(), v.clone());
        }
        // 대기 콜백을 마이크로태스크로
        let cbs = {
            let m = o.borrow();
            match m.get("__cbs") {
                Some(Value::Arr(a)) => a.borrow().clone(),
                _ => Vec::new(),
            }
        };
        for cb in cbs {
            if let Value::Obj(c) = cb {
                let (f, dep) = {
                    let cm = c.borrow();
                    (cm.get("cb").cloned().unwrap_or(Value::Undefined),
                     cm.get("dep").cloned().unwrap_or(Value::Undefined))
                };
                self.microtasks.push_back((f, v.clone(), dep));
            }
        }
        if let Some(Value::Arr(a)) = o.borrow().get("__cbs") {
            a.borrow_mut().clear();
        }
    }

    // p.then(cb) → dep promise 반환. p 가 이미 이행이면 마이크로태스크로, 아니면 대기열에.
    fn promise_then(&mut self, p: &Value, cb: Value, dep: Value) -> Value {
        let Value::Obj(o) = p else { return Value::Undefined };
        let (state, value) = {
            let m = o.borrow();
            (
                match m.get("__state") { Some(Value::Str(s)) => s.clone(), _ => "pending".into() },
                m.get("__value").cloned().unwrap_or(Value::Undefined),
            )
        };
        if state == "fulfilled" {
            self.microtasks.push_back((cb, value, dep.clone()));
        } else {
            // 대기: {cb, dep} 를 __cbs 에 추가
            let mut entry = HashMap::new();
            entry.insert("cb".to_string(), cb);
            entry.insert("dep".to_string(), dep.clone());
            let entry = Value::Obj(Rc::new(RefCell::new(entry)));
            if let Some(Value::Arr(a)) = o.borrow().get("__cbs") {
                a.borrow_mut().push(entry);
            }
        }
        dep
    }

    // 마이크로태스크 드레인: 콜백 실행 → 그 결과로 의존 promise 이행 (체이닝).
    // 값 타입에 대응하는 전역 생성자 (x.constructor 용).
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
    fn coerce_object_call(&self, f: &Value, args: &[Value]) -> Option<Value> {
        let Value::Obj(m) = f else { return None };
        match env_get(&self.global, "Object") {
            Some(Value::Obj(obj_ns)) if Rc::ptr_eq(m, &obj_ns) => {
                let arg = args.first().cloned().unwrap_or(Value::Undefined);
                Some(match arg {
                    Value::Null | Value::Undefined => {
                        Value::Obj(Rc::new(RefCell::new(HashMap::new())))
                    }
                    other => other,
                })
            }
            _ => None,
        }
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

    // promise 면 마이크로태스크를 비운 뒤 이행값을, 아니면 값 그대로 (Promise.all 등).
    fn promise_value(&mut self, v: &Value) -> Value {
        if !is_promise(v) {
            return v.clone();
        }
        self.drain_microtasks();
        if let Value::Obj(o) = v {
            let m = o.borrow();
            if matches!(m.get("__state"), Some(Value::Str(s)) if s == "fulfilled") {
                return m.get("__value").cloned().unwrap_or(Value::Undefined);
            }
        }
        Value::Undefined
    }

    pub fn drain_microtasks(&mut self) {
        let mut guard = 0;
        while let Some((cb, arg, dep)) = self.microtasks.pop_front() {
            guard += 1;
            if guard > 100_000 {
                break; // 폭주 방지
            }
            let r = self.call_value(cb, None, vec![arg]).unwrap_or(Value::Undefined);
            self.resolve_promise(&dep, r);
        }
    }

    // location 전역 설치 (페이지 URL 기반). window.location 에도 공유.
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
        let mut loc = HashMap::new();
        loc.insert("href".to_string(), Value::Str(url.to_string()));
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
    fn make_url(&self, args: Vec<Value>) -> Result<Value, String> {
        let input = args.first().map(to_display).unwrap_or_default();
        let resolved = match args.get(1) {
            Some(b) if !matches!(b, Value::Undefined | Value::Null) => {
                let base = to_display(b);
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
        let mut sp = HashMap::new();
        sp.insert("__query".to_string(), Value::Str(search.trim_start_matches('?').to_string()));
        sp.insert("get".to_string(), Value::Native(Native::UrlSearchGet));
        sp.insert("getAll".to_string(), Value::Native(Native::UrlSearchGetAll));
        sp.insert("has".to_string(), Value::Native(Native::UrlSearchHas));
        sp.insert("toString".to_string(), Value::Native(Native::UrlSearchToString));
        let search_params = Value::Obj(Rc::new(RefCell::new(sp)));

        let mut m = HashMap::new();
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
                out.extend(self.iterate_to_vec(&v));
            } else {
                out.push(self.eval(a, env)?);
            }
        }
        Ok(out)
    }

    // 값들의 Vec 을 반복자 객체로 (MakeIter 와 동일 구조: __items/__i/next).
    fn make_iter_from_vec(&self, items: Vec<Value>) -> Value {
        let mut it = HashMap::new();
        it.insert("__items".to_string(), Value::Arr(ArrayObj::new(items)));
        it.insert("__i".to_string(), Value::Num(0.0));
        it.insert("next".to_string(), Value::Native(Native::IterNext));
        Value::Obj(Rc::new(RefCell::new(it)))
    }

    // 이터러블(배열/문자열/Set/Map/반복자 객체)을 값 Vec 으로. yield* 와 for-of 공용.
    fn iterate_to_vec(&mut self, v: &Value) -> Vec<Value> {
        match v {
            Value::Arr(a) => a.borrow().clone(),
            Value::Str(s) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
            Value::SetVal(s) => s.borrow().clone(),
            Value::MapVal(m) => m
                .borrow()
                .iter()
                .map(|(k, val)| Value::Arr(ArrayObj::new(vec![k.clone(), val.clone()])))
                .collect(),
            // 반복자 객체: __items 있으면 그대로, 아니면 next() 반복 호출
            Value::Obj(o) => {
                if let Some(Value::Arr(items)) = o.borrow().get("__items") {
                    return items.borrow().clone();
                }
                let mut out = Vec::new();
                let next = o.borrow().get("next").cloned();
                if let Some(next) = next {
                    // next() 를 done 까지 반복 (무한 방지: step 카운터가 상한)
                    loop {
                        let r = match self.call_value(next.clone(), Some(v.clone()), vec![]) {
                            Ok(r) => r,
                            Err(_) => break,
                        };
                        if let Value::Obj(res) = &r {
                            let b = res.borrow();
                            if matches!(b.get("done"), Some(Value::Bool(true))) {
                                break;
                            }
                            out.push(b.get("value").cloned().unwrap_or(Value::Undefined));
                        } else {
                            break;
                        }
                        if self.tick().is_err() {
                            break;
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }

    pub(super) fn make_event(&self, event: &str, target: crate::dom::NodeId) -> Value {
        let mut m = HashMap::new();
        m.insert("type".to_string(), Value::Str(event.to_string()));
        m.insert("target".to_string(), Value::Dom(target));
        m.insert("currentTarget".to_string(), Value::Dom(target));
        m.insert("srcElement".to_string(), Value::Dom(target));
        m.insert("bubbles".to_string(), Value::Bool(true));
        m.insert("cancelable".to_string(), Value::Bool(true));
        m.insert("defaultPrevented".to_string(), Value::Bool(false));
        m.insert("isTrusted".to_string(), Value::Bool(true));
        m.insert("__stopProp".to_string(), Value::Bool(false));
        m.insert("timeStamp".to_string(), Value::Num(0.0));
        m.insert("preventDefault".to_string(), Value::Native(Native::EventPreventDefault));
        m.insert("stopPropagation".to_string(), Value::Native(Native::EventStopProp));
        m.insert("stopImmediatePropagation".to_string(), Value::Native(Native::EventStopProp));
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // 이벤트 디스패치: 타깃 → 조상 순(버블링). 이벤트 객체를 인자로 전달,
    // this 는 currentTarget. stopPropagation 시 상위 전파 중단.
    // 반환: 핸들러가 하나라도 실행됐는지(호출측 리플로우 판단용).
    pub fn fire_handlers(&mut self, target: crate::dom::NodeId, event: &str) -> bool {
        self.steps = 0;
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
        let mut chain = vec![target];
        if let Some(p) = self.dom {
            chain.extend(unsafe { (*p).ancestors(target) });
        }
        let evt_obj = if let Value::Obj(o) = &evt { o.clone() } else { return false };
        evt_obj.borrow_mut().insert("target".to_string(), Value::Dom(target));
        let mut fired = false;
        for id in chain {
            let to_run: Vec<Value> = self
                .handlers
                .iter()
                .filter(|(hid, t, _)| *hid == id && t == event)
                .map(|(_, _, f)| f.clone())
                .collect();
            if !to_run.is_empty() {
                fired = true;
                evt_obj.borrow_mut().insert("currentTarget".to_string(), Value::Dom(id));
            }
            for f in to_run {
                if let Err(e) = self.call_value(f, Some(Value::Dom(id)), vec![evt.clone()]) {
                    println!("[js error] {}", e);
                }
            }
            // stopPropagation 되면 상위로 전파 안 함
            if matches!(evt_obj.borrow().get("__stopProp"), Some(Value::Bool(true))) {
                break;
            }
        }
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
                if !set.borrow().iter().any(|e| strict_eq(e, v)) {
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
                .find(|(k, _)| strict_eq(k, &key))
                .map(|(_, v)| v.clone())
                .unwrap_or(Value::Undefined),
            MapOp::Has => Value::Bool(m.borrow().iter().any(|(k, _)| strict_eq(k, &key))),
            MapOp::Set => {
                let val = args.get(1).cloned().unwrap_or(Value::Undefined);
                let pos = m.borrow().iter().position(|(k, _)| strict_eq(k, &key));
                match pos {
                    Some(i) => m.borrow_mut()[i].1 = val,
                    None => m.borrow_mut().push((key, val)),
                }
                Value::MapVal(m) // set 은 map 반환 (체이닝)
            }
            MapOp::Delete => {
                let before = m.borrow().len();
                m.borrow_mut().retain(|(k, _)| !strict_eq(k, &key));
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
            MapOp::Keys => Value::Arr(ArrayObj::new(
                m.borrow().iter().map(|(k, _)| k.clone()).collect(),
            )),
            MapOp::Values => Value::Arr(ArrayObj::new(
                m.borrow().iter().map(|(_, v)| v.clone()).collect(),
            )),
            MapOp::Entries => Value::Arr(ArrayObj::new(
                m.borrow()
                    .iter()
                    .map(|(k, v)| Value::Arr(ArrayObj::new(vec![k.clone(), v.clone()])))
                    .collect(),
            )),
        })
    }

    fn set_method(&mut self, s: Rc<RefCell<Vec<Value>>>, op: SetOp, args: Vec<Value>) -> Value {
        let val = args.first().cloned().unwrap_or(Value::Undefined);
        match op {
            SetOp::Add => {
                if !s.borrow().iter().any(|e| strict_eq(e, &val)) {
                    s.borrow_mut().push(val);
                }
                Value::SetVal(s)
            }
            SetOp::Has => Value::Bool(s.borrow().iter().any(|e| strict_eq(e, &val))),
            SetOp::Delete => {
                let before = s.borrow().len();
                s.borrow_mut().retain(|e| !strict_eq(e, &val));
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
            SetOp::Values => Value::Arr(ArrayObj::new(s.borrow().clone())),
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
        self.steps = 0;
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
    pub fn run_callback(&mut self, cb: Value) {
        self.steps = 0;
        if let Err(e) = self.call_value(cb, None, Vec::new()) {
            println!("[js error] {}", e);
        }
    }

    // onclick 속성 등 인라인 핸들러 소스 실행 (전역 환경에서)
    pub fn run_inline_handler(&mut self, src: &str) {
        self.steps = 0;
        if let Err(e) = self.run(src) {
            println!("[js error] {}", e);
        }
    }

    pub fn run(&mut self, src: &str) -> Result<Value, String> {
        self.steps = 0; // 실행 단위(스크립트/핸들러)마다 한도 리셋
        let program = parse(src)?;
        let env = self.global.clone();
        hoist_vars(&program, &env); // var 하이스팅 (전역)
        match self.exec_block(&program, &env)? {
            Flow::Normal(v) | Flow::Return(v) => Ok(v),
            _ => Ok(Value::Undefined),
        }
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
            Pattern::Object(props, rest) => {
                for (key, sub, default) in props {
                    let mut v = self.member_get(&value, key).unwrap_or(Value::Undefined);
                    if matches!(v, Value::Undefined) {
                        if let Some(d) = default {
                            v = self.eval(d, env)?;
                        }
                    }
                    self.bind_pattern(sub, v, env, assign)?;
                }
                // { a, ...rest } — 분해되지 않은 나머지 own 프로퍼티를 객체로
                if let Some(rest_name) = rest {
                    let consumed: std::collections::HashSet<&str> =
                        props.iter().map(|(k, _, _)| k.as_str()).collect();
                    let mut map = HashMap::new();
                    let collect = |src: &HashMap<String, Value>, map: &mut HashMap<String, Value>| {
                        for (k, v) in src.iter() {
                            if !consumed.contains(k.as_str()) {
                                map.insert(k.clone(), v.clone());
                            }
                        }
                    };
                    match &value {
                        Value::Obj(o) => collect(&o.borrow(), &mut map),
                        Value::Instance(i) => collect(&i.fields.borrow(), &mut map),
                        _ => {}
                    }
                    bind(env, rest_name, Value::Obj(Rc::new(RefCell::new(map))));
                }
            }
            Pattern::Array(elems, rest) => {
                for (i, slot) in elems.iter().enumerate() {
                    if let Some((sub, default)) = slot {
                        let mut v =
                            self.member_get(&value, &i.to_string()).unwrap_or(Value::Undefined);
                        if matches!(v, Value::Undefined) {
                            if let Some(d) = default {
                                v = self.eval(d, env)?;
                            }
                        }
                        self.bind_pattern(sub, v, env, assign)?;
                    }
                }
                // [a, ...rest] — elems.len() 부터 남은 요소를 배열로
                if let Some(rest_name) = rest {
                    let items: Vec<Value> = match &value {
                        Value::Arr(a) => a.borrow().iter().skip(elems.len()).cloned().collect(),
                        _ => Vec::new(),
                    };
                    bind(env, rest_name, Value::Arr(ArrayObj::new(items)));
                }
            }
        }
        Ok(())
    }

    fn tick(&mut self) -> Result<(), String> {
        self.steps += 1;
        if self.steps > STEP_LIMIT {
            return Err(format!("{} (무한 루프?)", STEP_LIMIT_MSG));
        }
        Ok(())
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
        match stmt {
            Stmt::VarDecl { kind, decls } => {
                let is_var = matches!(kind, crate::js::ast::DeclKind::Var);
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
                    match self.exec_block(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        ret => return Ok(ret),
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::DoWhile { body, cond } => {
                loop {
                    self.tick()?;
                    let scope = Env::new(Some(env.clone()));
                    match self.exec_block(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        ret => return Ok(ret),
                    }
                    if !to_bool(&self.eval(cond, env)?) {
                        break;
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::For { init, cond, step, body } => {
                let outer = Env::new(Some(env.clone())); // for(let i...) 스코프
                if let Some(init) = init {
                    self.exec_stmt(init, &outer)?;
                }
                loop {
                    self.tick()?;
                    if let Some(cond) = cond {
                        if !to_bool(&self.eval(cond, &outer)?) {
                            break;
                        }
                    }
                    let scope = Env::new(Some(outer.clone()));
                    match self.exec_block(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        ret => return Ok(ret),
                    }
                    if let Some(step) = step {
                        self.eval(step, &outer)?;
                    }
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
            Stmt::Break => Ok(Flow::Break),
            Stmt::Continue => Ok(Flow::Continue),
            Stmt::Block(stmts) => {
                let scope = Env::new(Some(env.clone()));
                self.exec_block(stmts, &scope)
            }
            Stmt::Expr(e) => Ok(Flow::Normal(self.eval(e, env)?)),
            Stmt::Throw(e) => {
                let v = self.eval(e, env)?;
                let msg = to_display(&v);
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
                            // throw 된 값이 있으면 그 값, 네이티브 에러면 메시지 문자열
                            let caught =
                                self.thrown.take().unwrap_or(Value::Str(e.clone()));
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
                    Value::Obj(m) => m.borrow().keys().cloned().collect(),
                    Value::Arr(a) => (0..a.borrow().len()).map(|i| i.to_string()).collect(),
                    Value::Str(s) => (0..s.chars().count()).map(|i| i.to_string()).collect(),
                    _ => Vec::new(), // null/undefined 등: 순회 없음 (JS 동일)
                };
                for k in keys {
                    self.tick()?;
                    let scope = Env::new(Some(env.clone()));
                    env_declare(&scope, name, Value::Str(k));
                    match self.exec_block(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        ret => return Ok(ret),
                    }
                }
                Ok(Flow::Normal(Value::Undefined))
            }
            Stmt::ForOf { name, iter, body } => {
                let target = self.eval(iter, env)?;
                // 배열/문자열/Set/Map + 반복자 객체(제너레이터 포함) 지원
                let iterable = matches!(&target,
                    Value::Arr(_) | Value::Str(_) | Value::SetVal(_) | Value::MapVal(_))
                    || matches!(&target, Value::Obj(o)
                        if o.borrow().contains_key("__items") || o.borrow().contains_key("next"));
                if !iterable {
                    return Err(format!("{} 은(는) 반복 가능하지 않음", type_of(&target)));
                }
                let values = self.iterate_to_vec(&target);
                for v in values {
                    self.tick()?;
                    let scope = Env::new(Some(env.clone()));
                    env_declare(&scope, name, v);
                    match self.exec_block(body, &scope)? {
                        Flow::Break => break,
                        Flow::Continue | Flow::Normal(_) => {}
                        ret => return Ok(ret),
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
                        // 폴스루: break 가 나올 때까지 다음 케이스도 실행
                        match self.exec_block(stmts, &scope)? {
                            Flow::Break => return Ok(Flow::Normal(Value::Undefined)),
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
            Expr::Str(s) => Ok(Value::Str(s.clone())),
            Expr::Bool(b) => Ok(Value::Bool(*b)),
            Expr::Null => Ok(Value::Null),
            Expr::Undefined => Ok(Value::Undefined),
            Expr::Ident(name) => match env_get(env, name) {
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
                        Err(format!("{} 은(는) 정의되지 않음", name))
                    }
                }
            },
            Expr::Array(items) => {
                let mut v = Vec::new();
                for item in items {
                    if let Expr::Spread(inner) = item {
                        let val = self.eval(inner, env)?;
                        v.extend(self.iterate_to_vec(&val));
                    } else {
                        v.push(self.eval(item, env)?);
                    }
                }
                Ok(Value::Arr(ArrayObj::new(v)))
            }
            // 스프레드가 배열/호출 밖에 홀로 나오면 값 그대로 (관용)
            Expr::Spread(inner) => self.eval(inner, env),
            Expr::Object(props) => {
                let mut map = HashMap::new();
                for (k, e) in props {
                    if matches!(k, PropKey::Spread) {
                        // {...obj} — obj/배열/인스턴스의 own 프로퍼티 병합
                        match self.eval(e, env)? {
                            Value::Obj(o) => {
                                for (k, v) in o.borrow().iter() {
                                    map.insert(k.clone(), v.clone());
                                }
                            }
                            Value::Instance(inst) => {
                                for (k, v) in inst.fields.borrow().iter() {
                                    map.insert(k.clone(), v.clone());
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
                        PropKey::Getter(s) => s.clone(),
                        PropKey::Computed(ke) => to_display(&self.eval(ke, env)?),
                        PropKey::Spread => unreachable!(),
                    };
                    let val = self.eval(e, env)?;
                    // 접근자: 함수를 Getter 로 감싸 멤버 접근 시 호출되게
                    let val = if matches!(k, PropKey::Getter(_)) {
                        Value::Getter(Rc::new(val))
                    } else {
                        val
                    };
                    map.insert(key, val);
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
            Expr::Yield { star, arg } => {
                // eager 제너레이터: 값을 현재 yield sink 에 쌓는다. yield 식 자체는 undefined.
                let val = match arg {
                    Some(e) => self.eval(e, env)?,
                    None => Value::Undefined,
                };
                if *star {
                    // yield* iterable — 값들을 전개
                    let items = self.iterate_to_vec(&val);
                    if let Some(sink) = self.yield_sink.last_mut() {
                        sink.extend(items);
                    }
                } else if let Some(sink) = self.yield_sink.last_mut() {
                    sink.push(val);
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
            // await expr: 대상이 promise 면 마이크로태스크를 드레인해 이행시킨 뒤 값.
            // (우리 promise 는 동기 resolve 모델이라 드레인만으로 이행됨)
            Expr::Await(inner) => {
                let v = self.eval(inner, env)?;
                if !is_promise(&v) {
                    return Ok(v);
                }
                self.drain_microtasks();
                if let Value::Obj(o) = &v {
                    let m = o.borrow();
                    if matches!(m.get("__state"), Some(Value::Str(s)) if s == "fulfilled") {
                        return Ok(m.get("__value").cloned().unwrap_or(Value::Undefined));
                    }
                }
                Ok(Value::Undefined) // 펜딩(미이행) — 관용
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
            Expr::Template(parts) => {
                let mut s = String::new();
                for part in parts {
                    match part {
                        TemplatePart::Lit(t) => s.push_str(t),
                        TemplatePart::Expr(e) => s.push_str(&to_display(&self.eval(e, env)?)),
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
                let v = self.eval(expr, env)?;
                Ok(match op {
                    UnOp::Neg => Value::Num(-to_num(&v)),
                    UnOp::Pos => Value::Num(to_num(&v)),
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
            Expr::Member { obj, prop, computed } => {
                let recv = self.eval(obj, env)?;
                let key = self.member_key(prop, *computed, env)?;
                if matches!(recv, Value::Undefined | Value::Null) {
                    if self.lenient {
                        *self.lenient_hits.entry(format!(".{}", key)).or_default() += 1;
                        return Ok(Value::Undefined);
                    }
                    return Err(format!(
                        "{}.{} — {} 이(가) {} (읽을 수 없음)",
                        obj_hint(obj),
                        key,
                        obj_hint(obj),
                        to_display(&recv)
                    ));
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
            Expr::OptCall { callee, args } => {
                let f = self.eval(callee, env)?;
                if matches!(f, Value::Undefined | Value::Null) {
                    return Ok(Value::Undefined);
                }
                let mut arg_vals = Vec::new();
                arg_vals.extend(self.eval_args(args, env)?);
                self.call_value(f, None, arg_vals)
            }
            Expr::Call { callee, args } => {
                let mut arg_vals = Vec::new();
                // super(...) — 부모 생성자를 현재 this 로 실행
                if matches!(&**callee, Expr::Super) {
                    arg_vals.extend(self.eval_args(args, env)?);
                    let (Some(Value::Class(parent)), Some(this)) =
                        (env_get(env, "__superclass__"), env_get(env, "this"))
                    else {
                        return Err("super() 는 파생 클래스 생성자에서만".to_string());
                    };
                    self.run_constructor(&parent, &this, arg_vals)?;
                    return Ok(Value::Undefined);
                }
                // super.method(...) — 부모 메서드를 현재 this 로 실행
                if let Expr::Member { obj, prop, computed } = &**callee {
                    if matches!(&**obj, Expr::Super) {
                        let key = self.member_key(prop, *computed, env)?;
                        let (Some(Value::Class(parent)), Some(this)) =
                            (env_get(env, "__superclass__"), env_get(env, "this"))
                        else {
                            return Err("super.x 는 파생 클래스에서만".to_string());
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
                        return Err(format!(
                            "{}.{}(…) — {} 이(가) {}",
                            obj_hint(obj),
                            key,
                            obj_hint(obj),
                            to_display(&recv)
                        ));
                    }
                    let f = self.member_get(&recv, &key)?;
                    arg_vals.extend(self.eval_args(args, env)?);
                    if !is_callable(&f) {
                        if self.lenient {
                            *self.lenient_hits.entry(format!("{}() 비함수", key)).or_default() += 1;
                            return Ok(Value::Undefined);
                        }
                        return Err(format!(
                            "{}(…) — {}.{} 이(가) {} (함수 아님, 수신자={})",
                            key,
                            obj_hint(obj),
                            key,
                            to_display(&f),
                            type_of(&recv)
                        ));
                    }
                    self.call_value(f, Some(recv), arg_vals)
                } else {
                    let f = self.eval(callee, env)?;
                    arg_vals.extend(self.eval_args(args, env)?);
                    // Object(x) — 전역 Object 네임스페이스를 함수로 호출 = 객체 강제변환.
                    // core-js/프레임워크가 Object(this) 로 this 를 객체화하는 흔한 패턴.
                    if let Some(v) = self.coerce_object_call(&f, &arg_vals) {
                        return Ok(v);
                    }
                    if !is_callable(&f) {
                        if self.lenient {
                            let name =
                                if let Expr::Ident(n) = &**callee { n.as_str() } else { "?" };
                            *self.lenient_hits.entry(format!("{}() 비함수", name)).or_default() += 1;
                            return Ok(Value::Undefined);
                        }
                        let name = if let Expr::Ident(n) = &**callee { n.as_str() } else { "?" };
                        return Err(format!("{}(…) — {} 이(가) {} (함수 아님)", name, name, to_display(&f)));
                    }
                    self.call_value(f, None, arg_vals)
                }
            }
        }
    }

    fn member_key(&mut self, prop: &Expr, computed: bool, env: &EnvRef) -> Result<String, String> {
        if computed {
            let v = self.eval(prop, env)?;
            // 심볼 키(Symbol.iterator 등)는 고유 __key 문자열로 매핑
            if let Value::Obj(o) = &v {
                if let Some(Value::Str(k)) = o.borrow().get("__key") {
                    return Ok(k.clone());
                }
            }
            Ok(to_display(&v))
        } else if let Expr::Str(s) = prop {
            Ok(s.clone())
        } else {
            Err("잘못된 멤버 접근".to_string())
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
            _ => None,
        }?;
        match proto {
            Value::Obj(m) => m.borrow().get(key).cloned(),
            _ => None,
        }
    }

    fn member_get(&mut self, recv: &Value, key: &str) -> Result<Value, String> {
        // .constructor — 값 타입의 전역 생성자 (core-js/프레임워크의 타입판별·종/species 에 필수).
        // 객체/인스턴스가 자체 constructor 프로퍼티를 가지면 그것 우선.
        if key == "constructor" {
            match recv {
                Value::Obj(m) => {
                    if let Some(v) = m.borrow().get("constructor") {
                        return Ok(v.clone());
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
                    Some(Value::Getter(g)) => {
                        self.call_value((*g).clone(), Some(recv.clone()), vec![])
                    }
                    Some(v) => Ok(v),
                    // 내장 메서드 폴백
                    None => match key {
                        "hasOwnProperty" => Ok(Value::Native(Native::HasOwnProperty)),
                        // propertyIsEnumerable: own 프로퍼티면 열거가능(단순 모델) → hasOwnProperty 로 근사.
                        // core-js 등이 {}.propertyIsEnumerable.call(...) 로 기능탐지 → 없으면 크래시.
                        "propertyIsEnumerable" => Ok(Value::Native(Native::HasOwnProperty)),
                        "test" if is_regex_obj(map) => Ok(Value::Native(Native::RegexTest)),
                        "exec" if is_regex_obj(map) => Ok(Value::Native(Native::RegexExec)),
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
                                _ => None,
                            };
                            Ok(field
                                .map(|f| Value::Native(Native::DateMethod(f)))
                                .unwrap_or(Value::Undefined))
                        }
                        // Object.prototype 폴백 — 인스턴스 객체도 valueOf/toString/hasOwnProperty 등
                        _ => Ok(self.proto_method("Object", key).unwrap_or(Value::Undefined)),
                    },
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
                    "sort" => Some(ArrOp::Sort),
                    "flat" => Some(ArrOp::Flat),
                    _ => None,
                };
                if let Some(op) = op {
                    return Ok(Value::Native(Native::Arr(op)));
                }
                if key == "hasOwnProperty" {
                    return Ok(Value::Native(Native::HasOwnProperty));
                }
                if key == "@@iterator" {
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
                if key == "@@iterator" {
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
                if key == "@@iterator" {
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
                    return Ok(Value::Num(s.chars().count() as f64));
                }
                if key == "@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                let op = match key {
                    "indexOf" => Some(StrOp::IndexOf),
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
                    "toString" | "valueOf" | "toLocaleString" => {
                        return Ok(Value::Native(Native::ValueToStr))
                    }
                    "substr" => Some(StrOp::Slice),
                    _ => None,
                };
                if let Some(op) = op {
                    return Ok(Value::Native(Native::Str(op)));
                }
                if key == "@@iterator" {
                    return Ok(Value::Native(Native::MakeIter));
                }
                if let Ok(i) = key.parse::<usize>() {
                    return Ok(s
                        .chars()
                        .nth(i)
                        .map(|c| Value::Str(c.to_string()))
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
                    "appendChild" => Some(Native::AppendChild),
                    "append" => Some(Native::NodeAppend),
                    "prepend" => Some(Native::NodePrepend),
                    "before" => Some(Native::NodeBefore),
                    "after" => Some(Native::NodeAfter),
                    "replaceWith" => Some(Native::NodeReplaceWith),
                    "insertBefore" => Some(Native::InsertBefore),
                    "createTextNode" => Some(Native::CreateTextNode),
                    "remove" => Some(Native::RemoveElement),
                    "setAttribute" => Some(Native::SetAttribute),
                    "getAttribute" => Some(Native::GetAttribute),
                    "removeAttribute" => Some(Native::RemoveAttribute),
                    "hasAttribute" => Some(Native::HasAttribute),
                    "removeChild" => Some(Native::RemoveChild),
                    "querySelector" => Some(Native::QuerySelector),
                    "querySelectorAll" => Some(Native::QuerySelectorAll),
                    "getElementsByClassName" => Some(Native::GetElementsByClass),
                    "getElementsByTagName" => Some(Native::GetElementsByTag),
                    "getBoundingClientRect" => Some(Native::GetBoundingClientRect),
                    "dispatchEvent" => Some(Native::DispatchEvent),
                    "cloneNode" => Some(Native::CloneNode),
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
                Ok(Value::Undefined)
            }
            Value::Class(c) => {
                // 정적 멤버
                Ok(c.statics.borrow().get(key).cloned().unwrap_or(Value::Undefined))
            }
            Value::Fn(func) => {
                // 함수도 객체: 속성 백 우선, 그다음 call/apply/bind, prototype/name/length
                let stored = func.props.borrow().get(key).cloned();
                if let Some(v) = stored {
                    return match v {
                        Value::Getter(g) => {
                            self.call_value((*g).clone(), Some(recv.clone()), vec![])
                        }
                        other => Ok(other),
                    };
                }
                match key {
                    "call" => Ok(Value::Native(Native::FnCall)),
                    "apply" => Ok(Value::Native(Native::FnApply)),
                    "bind" => Ok(Value::Native(Native::FnBind)),
                    "name" => Ok(Value::Str(String::new())),
                    "length" => Ok(Value::Num(func.params.len() as f64)),
                    // F.prototype 지연 생성: 접근 시 빈 객체를 만들어 저장
                    // (F.prototype.method = ... 패턴 지원)
                    "prototype" => {
                        let proto = Value::Obj(Rc::new(RefCell::new(HashMap::new())));
                        func.props.borrow_mut().insert("prototype".to_string(), proto.clone());
                        Ok(proto)
                    }
                    _ => Ok(Value::Undefined),
                }
            }
            // Function.prototype (정체성 보존된 객체)
            Value::Native(Native::FunctionCtor) if key == "prototype" => Ok(self.fn_proto.clone()),
            // Date.now / Date.parse / Date.UTC
            Value::Native(Native::DateCtor) => Ok(match key {
                "now" => Value::Native(Native::DateNow),
                _ => Value::Undefined,
            }),
            // String.fromCharCode/prototype
            Value::Native(Native::StringCtor) => Ok(match key {
                "fromCharCode" | "fromCodePoint" => Value::Native(Native::StrFromCharCode),
                "prototype" => self.string_proto.clone(),
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
            // 네이티브/바운드 함수도 호출 어댑터 제공
            Value::Native(_) | Value::Bound(_) => match key {
                "call" => Ok(Value::Native(Native::FnCall)),
                "apply" => Ok(Value::Native(Native::FnApply)),
                "bind" => Ok(Value::Native(Native::FnBind)),
                "name" => Ok(Value::Str(String::new())),
                "length" => Ok(Value::Num(0.0)),
                _ => Ok(Value::Undefined),
            },
            // 숫자 메서드: (5).toFixed(2), n.toString(radix). 나머지는 Number.prototype 폴백.
            Value::Num(_) => Ok(match key {
                "toFixed" | "toPrecision" => Value::Native(Native::NumToFixed),
                "toString" | "toLocaleString" => Value::Native(Native::ValueToStr),
                "valueOf" => Value::Native(Native::ValueOfSelf),
                _ => proto_prop(&self.number_proto, key),
            }),
            Value::Bool(_) => Ok(match key {
                "toString" => Value::Native(Native::ValueToStr),
                "valueOf" => Value::Native(Native::ValueOfSelf),
                _ => proto_prop(&self.boolean_proto, key),
            }),
            Value::Undefined | Value::Null => {
                Err(format!("{} 의 '{}' 를 읽을 수 없음", to_display(recv), key))
            }
            _ => Ok(Value::Undefined),
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
                    env_declare(&scope, "__superclass__", Value::Class(sc.clone()));
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
                hoist_vars(&func.body, &scope); // var 하이스팅 (함수 스코프)
                // 제너레이터(eager): 본문을 즉시 실행해 yield 값을 모으고 반복자 반환.
                if func.is_generator {
                    self.yield_sink.push(Vec::new());
                    let result = self.exec_block(&func.body, &scope);
                    let items = self.yield_sink.pop().unwrap_or_default();
                    result?; // 본문 에러 전파(수집 후)
                    return Ok(self.make_iter_from_vec(items));
                }
                let result = match self.exec_block(&func.body, &scope)? {
                    Flow::Return(v) => v,
                    _ => Value::Undefined,
                };
                // async: 반환값을 이행된 Promise 로 감싼다 (await/then 대상이 되도록).
                // 이미 Promise 면 그대로 (thenable 위임).
                if func.is_async {
                    if is_promise(&result) {
                        return Ok(result);
                    }
                    let p = self.new_promise();
                    self.resolve_promise(&p, result);
                    return Ok(p);
                }
                Ok(result)
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
            other => Err(format!("{} 은(는) 함수가 아님", to_display(&other))),
        }
    }

    // new Class(args) / 클래스 호출: 인스턴스 생성 → 생성자 체인 실행 → 인스턴스 반환.
    fn construct(&mut self, class: Value, args: Vec<Value>) -> Result<Value, String> {
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
            Value::Native(Native::UrlCtor) => return self.make_url(args),
            Value::Native(Native::XhrCtor) => return Ok(self.make_xhr()),
            // new (boundFn)() — Reflect.construct 의 bind 트릭 지원
            Value::Bound(b) => {
                let (target, _this, partial) = (*b).clone();
                let mut all = partial;
                all.extend(args);
                return self.construct(target, all);
            }
            Value::Native(Native::ErrorCtor(name)) => {
                let mut map = HashMap::new();
                map.insert("name".to_string(), Value::Str(name.to_string()));
                map.insert(
                    "message".to_string(),
                    Value::Str(args.first().map(to_display).unwrap_or_default()),
                );
                return Ok(Value::Obj(Rc::new(RefCell::new(map))));
            }
            // 네이티브 생성자 스텁: new Error('m') / new Object() 등 → 객체
            // new f() — 일반 함수를 생성자로 (ES6 이전 패턴, 미니파이 코드 다수).
            // 새 객체를 this 로 함수 실행. f.prototype 메서드를 인스턴스에 복사(간이 체인).
            // 함수가 객체를 반환하면 그것 우선(JS 규칙).
            Value::Fn(func) => {
                let obj = Rc::new(RefCell::new(HashMap::new()));
                let proto = func.props.borrow().get("prototype").cloned();
                if let Some(Value::Obj(p)) = proto {
                    for (k, v) in p.borrow().iter() {
                        obj.borrow_mut().insert(k.clone(), v.clone());
                    }
                }
                let this = Value::Obj(obj);
                let ret = self.call_value(Value::Fn(func), Some(this.clone()), args)?;
                return Ok(match ret {
                    v @ (Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) => v,
                    _ => this,
                });
            }
            Value::Obj(_) | Value::Native(_) => {
                let mut map = HashMap::new();
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
        self.run_constructor(&cls, &inst, args)?;
        Ok(inst)
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
    fn run_constructor(
        &mut self,
        cls: &Rc<JsClass>,
        inst: &Value,
        args: Vec<Value>,
    ) -> Result<(), String> {
        match &cls.ctor {
            Some(ctor) => {
                let scope = Env::new(Some(ctor.env.clone()));
                env_declare(&scope, "this", inst.clone());
                // super 참조용: 현재 클래스의 부모를 스코프에 숨겨둠
                if let Some(parent) = &cls.parent {
                    env_declare(&scope, "__superclass__", Value::Class(parent.clone()));
                }
                for (i, p) in ctor.params.iter().enumerate() {
                    env_declare(&scope, p, args.get(i).cloned().unwrap_or(Value::Undefined));
                }
                self.exec_block(&ctor.body, &scope)?;
            }
            None => {
                // 암묵 생성자: 부모가 있으면 부모 생성자를 같은 인자로 실행
                if let Some(parent) = &cls.parent {
                    self.run_constructor(parent, inst, args)?;
                }
            }
        }
        Ok(())
    }

    fn make_class(&mut self, def: &crate::js::ast::ClassDef, env: &EnvRef) -> Result<Value, String> {
        let parent = match &def.parent {
            Some(e) => match self.eval(e, env)? {
                Value::Class(c) => Some(c),
                other => return Err(format!("{} 은(는) 확장할 클래스가 아님", to_display(&other))),
            },
            None => None,
        };
        let mk = |params: &Vec<String>, body: &Vec<Stmt>| {
            Rc::new(JsFn {
                params: params.clone(),
                body: body.clone(),
                env: env.clone(),
                is_arrow: false,
                is_generator: false,
                is_async: false,
                this: None,
                super_class: parent.clone(), // super.x → 이 클래스의 부모
                props: RefCell::new(HashMap::new()),
            })
        };
        let ctor = def.ctor.as_ref().map(|(p, b)| mk(p, b));
        let mut methods = HashMap::new();
        for (name, p, b) in &def.methods {
            methods.insert(name.clone(), mk(p, b));
        }
        let mut getters = HashMap::new();
        for (name, p, b) in &def.getters {
            getters.insert(name.clone(), mk(p, b));
        }
        // 인스턴스 필드: 초기화식을 무인자 함수로 감싸(this 바인딩+env) 생성 시 호출
        let mut fields = Vec::new();
        for (name, init) in &def.fields {
            let f = init
                .as_ref()
                .map(|e| mk(&Vec::new(), &vec![Stmt::Return(Some(e.clone()))]));
            fields.push((name.clone(), f));
        }
        // 정적 멤버는 parent 가 cls 로 이동하기 전에 만든다 (mk 가 parent 참조)
        let mut statics = HashMap::new();
        for (name, p, b) in &def.statics {
            statics.insert(name.clone(), Value::Fn(mk(p, b)));
        }
        let cls = Rc::new(JsClass {
            name: def.name.clone().unwrap_or_else(|| "(anonymous)".to_string()),
            parent,
            ctor,
            methods,
            getters,
            fields,
            statics: RefCell::new(statics),
        });
        // static 필드: 클래스 완성 후 this=클래스로 평가해 statics 에 설정
        for (name, init) in &def.static_fields {
            let v = match init {
                Some(e) => {
                    let scope = Env::new(Some(env.clone()));
                    env_declare(&scope, "this", Value::Class(cls.clone()));
                    self.eval(e, &scope)?
                }
                None => Value::Undefined,
            };
            cls.statics.borrow_mut().insert(name.clone(), v);
        }
        Ok(Value::Class(cls))
    }


    fn binary(&mut self, op: BinOp, l: Value, r: Value) -> Result<Value, String> {
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
            BinOp::In => match &r {
                Value::Obj(m) => Value::Bool(m.borrow().contains_key(&to_display(&l))),
                Value::Arr(a) => Value::Bool(
                    to_display(&l).parse::<usize>().map_or(false, |i| i < a.borrow().len()),
                ),
                _ => Value::Bool(false),
            },
            BinOp::Instanceof => {
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
                // 전역 생성자 스텁과의 대응 판단 (관용)
                let global_is = |name: &str| -> bool {
                    matches!(
                        (env_get(&self.global, name), &r),
                        (Some(Value::Obj(a)), Value::Obj(b)) if Rc::ptr_eq(&a, b)
                    )
                };
                let hit = if global_is("Array") {
                    matches!(l, Value::Arr(_))
                } else if global_is("Object") {
                    matches!(l, Value::Obj(_) | Value::Arr(_))
                } else if matches!(&r, Value::Native(Native::FunctionCtor)) {
                    matches!(l, Value::Fn(_) | Value::Native(_) | Value::Bound(_))
                } else {
                    false
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

    fn assign_to(&mut self, target: &Expr, value: Value, env: &EnvRef) -> Result<(), String> {
        match target {
            Expr::Ident(name) => {
                env_set(env, name, value);
                Ok(())
            }
            Expr::Member { obj, prop, computed } => {
                let recv = self.eval(obj, env)?;
                let key = self.member_key(prop, *computed, env)?;
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
                        map.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    Value::Arr(a) => {
                        if let Ok(i) = key.parse::<usize>() {
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
                            a.set_prop(key, value);
                        }
                        Ok(())
                    }
                    Value::Dom(id) => self.dom_set(id, &key, value),
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
                        inst.fields.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    Value::Class(c) => {
                        c.statics.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    // 함수 프로퍼티 (F.prototype, F.staticProp = ...)
                    Value::Fn(func) => {
                        func.props.borrow_mut().insert(key, value);
                        Ok(())
                    }
                    other => Err(format!("{} 에 할당할 수 없음", to_display(&other))),
                }
            }
            _ => Err("할당 대상이 아님".to_string()),
        }
    }

    // ── DOM 바인딩 (아레나; dom 포인터는 실행 동안만 유효, 미설정 시 에러) ──

}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(src: &str) -> Value {
        Interp::new().run(src).unwrap()
    }

    fn run_num(src: &str) -> f64 {
        match run(src) {
            Value::Num(n) => n,
            other => panic!("expected number, got {:?}", other),
        }
    }

    fn run_str(src: &str) -> String {
        match run(src) {
            Value::Str(s) => s,
            other => panic!("expected string, got {:?}", other),
        }
    }

    fn run_bool(src: &str) -> bool {
        match run(src) {
            Value::Bool(b) => b,
            other => panic!("expected bool, got {:?}", other),
        }
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(run_num("1 + 2 * 3"), 7.0);
        assert_eq!(run_num("(1 + 2) * 3"), 9.0);
        assert_eq!(run_num("7 % 3"), 1.0);
        assert_eq!(run_num("-3 + 1"), -2.0);
    }

    #[test]
    fn exponent_literals_and_operator() {
        // 지수 표기 숫자 리터럴 (미니파이 코드에 필수)
        assert_eq!(run_num("1e3"), 1000.0);
        assert_eq!(run_num("1.5e-1"), 0.15);
        assert_eq!(run_num(".5e2"), 50.0);
        assert_eq!(run_num("0b101"), 5.0);
        assert_eq!(run_num("0o17"), 15.0);
        // ** 연산자: 곱셈보다 강하고 우결합
        assert_eq!(run_num("2 ** 10"), 1024.0);
        assert_eq!(run_num("2 ** 3 ** 2"), 512.0); // 2**(3**2)=2**9
        assert_eq!(run_num("3 * 2 ** 2"), 12.0); // 3*(2**2)
        assert_eq!(run_num("let x=3; x**=2; x"), 9.0);
    }

    #[test]
    fn ushr_assign_and_do_while() {
        // >>>= (부호 없는 우시프트 대입)
        assert_eq!(run_num("let x=-1; x>>>=28; x"), 15.0);
        // do-while: 조건 거짓이어도 최소 1회 실행
        assert_eq!(run_num("let n=0; do { n++; } while(false); n"), 1.0);
        assert_eq!(run_num("let i=0,s=0; do { s+=i; i++; } while(i<3); s"), 3.0);
        // do-while 안 break/continue
        assert_eq!(run_num("let i=0,s=0; do { i++; if(i==2) continue; s+=i; } while(i<4); s"), 8.0);
    }

    #[test]
    fn iterator_protocol() {
        // Symbol 은 실제 페이지에선 프렐류드가 주입 — 테스트는 인라인 정의.
        // 계산된 심볼 키가 __key("@@iterator")로 매핑돼 반복자에 연결된다.
        let sym = "var Symbol={iterator:{__isSymbol:true,__key:'@@iterator'}};";
        assert_eq!(
            run_num(&format!(
                "{sym} var a=[10,20,30]; var it=a[Symbol.iterator](); var s=0,r; \
                 while(!(r=it.next()).done){{ s+=r.value; }} s"
            )),
            60.0
        );
        // Set 반복자
        assert_eq!(
            run_num(&format!(
                "{sym} var it=new Set([1,2,3])[Symbol.iterator](); var s=0,r; \
                 while(!(r=it.next()).done){{ s+=r.value; }} s"
            )),
            6.0
        );
    }

    #[test]
    fn builtin_prototypes() {
        // Function.prototype.call/apply/bind
        assert_eq!(run_num("Function.prototype.call.call(function(){return 5})"), 5.0);
        // Array.prototype.slice.call (배열형 → 배열)
        assert_eq!(run_num("var a=[1,2,3]; Array.prototype.slice.call(a,1).length"), 2.0);
        assert_eq!(run_num("Array.prototype.indexOf.call([7,8,9], 8)"), 1.0);
        // Object.prototype.toString.call (타입 판별 관용)
        assert_eq!(run_str("Object.prototype.toString.call([])"), "[object Array]");
        assert_eq!(run_str("Object.prototype.toString.call({})"), "[object Object]");
        assert_eq!(run_str("Object.prototype.toString.call('x')"), "[object String]");
        assert_eq!(run_str("Object.prototype.toString.call(5)"), "[object Number]");
    }

    #[test]
    fn arrays_are_objects() {
        // push 재정의 (webpack 청크 배열이 하는 핵심 동작)
        assert_eq!(
            run_num("var a=[]; var n=0; a.push=function(){n++;}; a.push(1); a.push(2); n"),
            2.0
        );
        // 커스텀 프로퍼티
        assert_eq!(run_num("var a=[1,2]; a.foo=42; a.foo"), 42.0);
        // 커스텀 프로퍼티가 항목/length 를 안 건드림
        assert_eq!(run_num("var a=[1,2]; a.foo=42; a.length"), 2.0);
        // length 대입으로 절단
        assert_eq!(run_num("var a=[1,2,3,4]; a.length=2; a.length"), 2.0);
        // 재정의 안 하면 내장 메서드 그대로
        assert_eq!(run_num("var a=[3,1,2]; a.push(9); a.length"), 4.0);
    }

    #[test]
    fn date_object() {
        assert_eq!(run_num("new Date(2026, 6, 11).getFullYear()"), 2026.0);
        assert_eq!(run_num("new Date(2026, 6, 11).getMonth()"), 6.0); // 0 기준(7월)
        assert_eq!(run_num("new Date(2026, 6, 11).getDate()"), 11.0);
        assert_eq!(run_str("new Date('2020-01-15T00:00:00Z').toISOString()"), "2020-01-15T00:00:00.000Z");
        assert_eq!(run_num("new Date('2020-01-15T00:00:00Z').getTime()"), 1579046400000.0);
        assert_eq!(run_num("new Date(0).getUTCFullYear()"), 1970.0);
        assert_eq!(run_str("typeof Date.now()"), "number");
        // 왕복
        assert_eq!(run_num("new Date(new Date(1234567890000).getTime()).getTime()"), 1234567890000.0);
    }

    #[test]
    fn string_number_boolean_globals() {
        assert_eq!(run_str("String(42)"), "42");
        assert_eq!(run_num("Number('3.5')"), 3.5);
        assert!(!run_bool("Boolean(0)"));
        assert!(run_bool("Boolean(1)"));
        assert_eq!(run_str("String.fromCharCode(72,73)"), "HI");
        assert!(run_bool("Number.isInteger(5)"));
        assert!(!run_bool("Number.isInteger(5.5)"));
        assert_eq!(run_str("(3.14159).toFixed(2)"), "3.14");
        assert_eq!(run_str("(255).toString(16)"), "ff");
        assert_eq!(run_num("Number.MAX_SAFE_INTEGER"), 9007199254740991.0);
        // String.prototype.slice.call
        assert_eq!(run_str("String.prototype.slice.call('hello', 1, 3)"), "el");
    }

    #[test]
    fn regex_and_string_methods() {
        // test/exec
        assert!(run_bool("/\\d+/.test('abc123')"));
        assert!(!run_bool("/^\\d+$/.test('ab12')"));
        assert_eq!(run_str("/(\\d+)-(\\d+)/.exec('x 12-34')[2]"), "34");
        // new RegExp + i 플래그
        assert!(run_bool("new RegExp('abc','i').test('XABC')"));
        // replace: 전역, 그룹 $1, 함수
        assert_eq!(run_str("'a1b2c3'.replace(/\\d/g,'#')"), "a#b#c#");
        assert_eq!(
            run_str("'2026-07-11'.replace(/(\\d+)-(\\d+)-(\\d+)/,'$3/$2/$1')"),
            "11/07/2026"
        );
        assert_eq!(run_str("'abc'.replace(/[a-z]/g,function(m){return m.toUpperCase()})"), "ABC");
        // match/search/split
        assert_eq!(run_num("'a1b2'.match(/\\d/g).length"), 2.0);
        assert_eq!(run_num("'hello world'.search(/wor/)"), 6.0);
        assert_eq!(run_num("'a,b;c'.split(/[,;]/).length"), 3.0);
        // 문자열 유틸
        assert_eq!(run_str("'5'.padStart(3,'0')"), "005");
        assert_eq!(run_str("'ab'.repeat(3)"), "ababab");
        assert_eq!(run_num("'A'.charCodeAt(0)"), 65.0);
    }

    #[test]
    fn map_and_set() {
        assert_eq!(run_num("var m=new Map(); m.set('a',1); m.set('b',2); m.get('b')"), 2.0);
        assert_eq!(run_num("var m=new Map(); m.set('a',1); m.set('a',9); m.size"), 1.0);
        assert!(run_bool("var m=new Map([['x',1]]); m.has('x')"));
        assert_eq!(run_num("var m=new Map(); m.set(1,'a'); m.delete(1); m.size"), 0.0);
        assert_eq!(run_num("var s=new Set([1,2,2,3]); s.size"), 3.0);
        assert!(run_bool("var s=new Set(); s.add(5); s.has(5)"));
        assert_eq!(
            run_num("var s=new Set([1,2,3]); var t=0; s.forEach(function(v){t+=v}); t"),
            6.0
        );
        // Map.forEach (value, key)
        assert_eq!(
            run_num("var m=new Map([['a',10],['b',20]]); var t=0; m.forEach(function(v){t+=v}); t"),
            30.0
        );
    }

    #[test]
    fn define_property_getter_and_value() {
        // Object.defineProperty 값
        assert_eq!(run_num("var o={}; Object.defineProperty(o,'x',{value:7}); o.x"), 7.0);
        // 접근자(get) — 읽을 때 호출
        assert_eq!(
            run_num("var o={}; var n=0; Object.defineProperty(o,'g',{get:function(){return ++n}}); o.g; o.g"),
            2.0
        );
        // hasOwnProperty
        assert!(run_bool("var o={a:1}; Object.prototype.hasOwnProperty.call(o,'a')"));
        assert!(!run_bool("var o={a:1}; o.hasOwnProperty('b')"));
    }

    #[test]
    fn array_methods_batch() {
        assert!(run_bool("[1,2,3].some(function(x){return x>2})"));
        assert!(run_bool("[1,2,3].every(function(x){return x>0})"));
        assert_eq!(run_num("[1,2,3,4].reduce(function(a,b){return a+b},0)"), 10.0);
        assert_eq!(run_num("[1,2,3].find(function(x){return x>1})"), 2.0);
        assert_eq!(run_num("[5,6,7].findIndex(function(x){return x===7})"), 2.0);
        assert!(run_bool("[1,2,3].includes(2)"));
        assert_eq!(run_num("[1,2].concat([3,4]).length"), 4.0);
        // splice: 원본 변형 + 제거분 반환
        assert_eq!(run_num("var a=[1,2,3,4]; a.splice(1,2); a.length"), 2.0);
        assert_eq!(run_num("var a=[1,2,3]; a.unshift(0); a[0]"), 0.0);
        assert_eq!(run_num("var a=[1,2,3]; a.shift(); a[0]"), 2.0);
    }

    #[test]
    fn function_constructor_compiles() {
        // Function 생성자가 문자열 본문을 실제 함수로 컴파일
        assert_eq!(run_num("var f = Function('return 42'); f()"), 42.0);
        assert_eq!(run_num("var f = new Function('a','b','return a+b'); f(2,3)"), 5.0);
        // 한 인자에 콤마로 여러 파라미터
        assert_eq!(run_num("var f = new Function('a,b','return a*b'); f(4,5)"), 20.0);
    }

    #[test]
    fn functions_are_objects() {
        // 함수 프로퍼티 (정적 + prototype)
        assert_eq!(run_num("function F(){}; F.x = 5; F.x"), 5.0);
        assert_eq!(run_num("function F(){}; F.prototype.v = 9; F.prototype.v"), 9.0);
        // call / apply / bind
        assert_eq!(run_num("function add(a,b){return a+b} add.call(null, 2, 3)"), 5.0);
        assert_eq!(run_num("function add(a,b){return a+b} add.apply(null, [4,5])"), 9.0);
        assert_eq!(run_num("function add(a,b){return a+b} add.bind(null,10)(5)"), 15.0);
        // this 바인딩 (call)
        assert_eq!(run_num("function f(){return this.x} f.call({x:7})"), 7.0);
        // bind 로 this 고정
        assert_eq!(run_num("function f(){return this.x} let g=f.bind({x:3}); g()"), 3.0);
    }

    #[test]
    fn default_parameters() {
        // 기본값 파라미터: 인자 없으면 기본값, 있으면 그 값
        assert_eq!(run_num("function f(a, b=10){ return a+b; } f(5)"), 15.0);
        assert_eq!(run_num("function f(a, b=10){ return a+b; } f(5, 2)"), 7.0);
        // 화살표 기본값
        assert_eq!(run_num("let f=(x=3)=>x*2; f()"), 6.0);
        assert_eq!(run_num("let f=(x=3)=>x*2; f(5)"), 10.0);
        // undefined 명시 전달도 기본값
        assert_eq!(run_num("function f(a=7){ return a; } f(undefined)"), 7.0);
    }

    #[test]
    fn reserved_and_computed_object_keys() {
        // 예약어를 객체 키로 (미니파이 코드에 흔함)
        assert_eq!(run_str("let o={return:'r', class:'c'}; o.return"), "r");
        assert_eq!(run_str("let o={in:'x', for:'y'}; o.for"), "y");
        // 정적 계산 키
        assert_eq!(run_str("let o={['a'+'b']:'v'}; o.ab"), "v");
    }

    #[test]
    fn string_concat_and_coercion() {
        assert_eq!(run_str("'a' + 'b'"), "ab");
        assert_eq!(run_str("'x=' + (1 + 2)"), "x=3");
        assert_eq!(run_str("1 + '2'"), "12"); // JS 의 그 동작
        assert_eq!(run_num("'3' * '4'"), 12.0);
    }

    #[test]
    fn variables_and_compound_assign() {
        assert_eq!(run_num("var x = 1; x += 3; x *= 2; x"), 8.0);
        assert_eq!(run_num("let a = 5; a - 2"), 3.0);
    }

    #[test]
    fn control_flow() {
        assert_eq!(run_num("var s = 0; for (var i = 1; i <= 10; i++) s += i; s"), 55.0);
        assert_eq!(run_num("var n = 0; while (n < 5) { n++; } n"), 5.0);
        assert_eq!(
            run_num("var s = 0; for (var i = 0; i < 10; i++) { if (i % 2) continue; if (i > 6) break; s += i; } s"),
            12.0 // 0+2+4+6
        );
        assert_eq!(run_str("if (false) 'a'; else 'b'"), "b");
    }

    #[test]
    fn functions_closures_recursion() {
        assert_eq!(run_num("function add(a, b) { return a + b; } add(2, 3)"), 5.0);
        // 클로저 카운터
        assert_eq!(
            run_num(
                "function counter() { var n = 0; return function() { n++; return n; }; } \
                 var c = counter(); c(); c(); c()"
            ),
            3.0
        );
        // 재귀 (선언 전 호출 = 호이스팅)
        assert_eq!(run_num("fib(10); function fib(n) { return n < 2 ? n : fib(n-1) + fib(n-2); } fib(10)"), 55.0);
        // 화살표 + 고차 함수
        assert_eq!(run_num("var twice = f => x => f(f(x)); twice(n => n + 3)(1)"), 7.0);
    }

    #[test]
    fn arrays_and_objects() {
        assert_eq!(run_num("var a = [1, 2, 3]; a[0] + a[2]"), 4.0);
        assert_eq!(run_num("var a = []; a.push(7); a.push(8, 9); a.length"), 3.0);
        assert_eq!(run_num("var a = [1]; a[3] = 9; a.length"), 4.0);
        assert_eq!(run_num("var o = { x: 1, y: { z: 2 } }; o.x + o.y.z"), 3.0);
        assert_eq!(run_num("var o = {}; o.k = 5; o['k'] + 1"), 6.0);
        assert_eq!(run_str("var k = 'name'; var o = {}; o[k] = 'kestrel'; o.name"), "kestrel");
    }

    #[test]
    fn equality_semantics() {
        assert!(run_bool("1 == '1'"));
        assert!(!run_bool("1 === '1'"));
        assert!(run_bool("null == undefined"));
        assert!(!run_bool("null === undefined"));
        assert!(run_bool("'b' > 'a'"));
        assert!(run_bool("typeof null === 'object'"));
        assert!(run_bool("typeof (x => x) === 'function'"));
    }

    #[test]
    fn logical_short_circuit() {
        // 우변이 평가되면 에러가 났을 것 (미정의 함수 호출)
        assert_eq!(run_num("false && boom() ? 1 : 2"), 2.0);
        assert_eq!(run_num("true || boom() ? 1 : 2"), 1.0);
        assert_eq!(run_str("'' || 'fallback'"), "fallback");
    }

    #[test]
    fn update_operators() {
        assert_eq!(run_num("var i = 5; i++"), 5.0);
        assert_eq!(run_num("var i = 5; ++i"), 6.0);
        assert_eq!(run_num("var i = 5; i--; i"), 4.0);
    }

    #[test]
    fn console_log_captures() {
        let mut it = Interp::new();
        it.run("console.log('hello', 1 + 1, [1,2], { a: 1 })").unwrap();
        assert_eq!(it.console, vec!["hello 2 1,2 [object Object]"]);
    }

    #[test]
    fn block_scoping_let() {
        assert_eq!(run_num("let x = 1; { let x = 2; } x"), 1.0);
    }

    #[test]
    fn runtime_errors() {
        assert!(Interp::new().run("undefinedVar + 1").is_err());
        assert!(Interp::new().run("null.foo").is_err());
        assert!(Interp::new().run("var x = 3; x()").is_err());
    }

    #[test]
    fn infinite_loop_is_bounded() {
        assert!(Interp::new().run("while (true) {}").is_err());
    }

    #[test]
    fn math_builtins() {
        assert_eq!(run_num("Math.floor(3.7)"), 3.0);
        assert_eq!(run_num("Math.ceil(3.1)"), 4.0);
        assert_eq!(run_num("Math.round(2.5)"), 3.0);
        assert_eq!(run_num("Math.abs(-5)"), 5.0);
        assert_eq!(run_num("Math.min(3, 1, 2)"), 1.0);
        assert_eq!(run_num("Math.max(3, 1, 2)"), 3.0);
        assert_eq!(run_num("Math.sqrt(16)"), 4.0);
        assert_eq!(run_num("Math.pow(2, 10)"), 1024.0);
        assert!(run_bool("Math.PI > 3.14 && Math.PI < 3.15"));
        assert!(run_bool("var r = Math.random(); r >= 0 && r < 1"));
        assert!(run_bool("Math.random() !== Math.random()"));
    }

    #[test]
    fn string_methods() {
        assert_eq!(run_num("'hello world'.indexOf('world')"), 6.0);
        assert_eq!(run_num("'abc'.indexOf('z')"), -1.0);
        assert_eq!(run_str("'hello'.slice(1, 3)"), "el");
        assert_eq!(run_str("'hello'.slice(-3)"), "llo");
        assert_eq!(run_str("'a,b,c'.split(',').join('|')"), "a|b|c");
        assert_eq!(run_num("'abc'.split('').length"), 3.0);
        assert_eq!(run_str("'  x  '.trim()"), "x");
        assert_eq!(run_str("'AbC'.toUpperCase()"), "ABC");
        assert_eq!(run_str("'AbC'.toLowerCase()"), "abc");
        assert_eq!(run_str("'aaa'.replace('a', 'b')"), "baa");
        assert_eq!(run_str("'hey'.charAt(1)"), "e");
        assert!(run_bool("'hello'.includes('ell')"));
        assert!(run_bool("'hello'.startsWith('he') && 'hello'.endsWith('lo')"));
        // 한글도 문자 단위로
        assert_eq!(run_str("'황조롱이'.slice(0, 2)"), "황조");
    }

    #[test]
    fn array_methods() {
        assert_eq!(run_str("[1,2,3].join('-')"), "1-2-3");
        assert_eq!(run_num("var a = [1,2,3]; a.pop(); a.length"), 2.0);
        assert_eq!(run_num("[5,6,7].indexOf(6)"), 1.0);
        assert_eq!(run_num("[1,2,3,4].slice(1, 3).length"), 2.0);
        assert_eq!(run_num("var s = 0; [1,2,3].forEach(function(x) { s += x; }); s"), 6.0);
        assert_eq!(run_str("[1,2,3].map(x => x * 10).join(',')"), "10,20,30");
        assert_eq!(run_str("[1,2,3,4,5].filter(x => x % 2).join(',')"), "1,3,5");
        assert_eq!(
            run_num("[1,2,3].map((x, i) => x + i).indexOf(5)"),
            2.0,
            "콜백 두 번째 인자 = 인덱스"
        );
    }

    #[test]
    fn proxy_get_set_traps() {
        // get 트랩: 없는 키에 기본값
        assert_eq!(
            run_num(
                "var p = new Proxy({a: 1}, { get: function(t, k) { return k in t ? t[k] : 99; } }); p.a + p.zzz"
            ),
            100.0
        );
        // set 트랩: 값 가로채 변형 후 저장
        assert_eq!(
            run_num(
                "var log = 0; \
                 var p = new Proxy({}, { set: function(t, k, v) { log = v * 2; t[k] = v; return true; } }); \
                 p.x = 5; log"
            ),
            10.0
        );
        // 트랩 없으면 target 위임
        assert_eq!(
            run_num("var p = new Proxy({n: 7}, {}); p.n"),
            7.0
        );
        assert_eq!(
            run_num("var p = new Proxy({}, {}); p.k = 3; p.k"),
            3.0
        );
    }

    #[test]
    fn document_fragment_moves_children() {
        let mut dom = crate::html::parse_dom("<ul id=\"list\"></ul>".to_string());
        let _ = dom.find_by_attr_id("list").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        // 프래그먼트에 li 2개 추가 후 ul 에 appendChild → 자식만 옮겨진다
        let n = interp
            .run(
                "var f = document.createDocumentFragment(); \
                 var a = document.createElement('li'); \
                 var b = document.createElement('li'); \
                 f.appendChild(a); f.appendChild(b); \
                 var ul = document.getElementById('list'); \
                 ul.appendChild(f); \
                 ul.children.length",
            )
            .unwrap();
        assert_eq!(to_display(&n), "2", "프래그먼트 자식 2개가 ul 로 이동");
    }

    #[test]
    fn matches_closest_contains() {
        let mut dom = crate::html::parse_dom(
            "<div class=\"outer\"><ul><li id=\"a\" class=\"item\">x</li></ul></div>".to_string(),
        );
        let a = dom.find_by_attr_id("a").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        // matches
        assert_eq!(
            to_display(&interp.run("document.getElementById('a').matches('.item')").unwrap()),
            "true"
        );
        assert_eq!(
            to_display(&interp.run("document.getElementById('a').matches('.nope')").unwrap()),
            "false"
        );
        // closest 는 조상 중 첫 매칭 (.outer)
        assert_eq!(
            to_display(
                &interp
                    .run("document.getElementById('a').closest('.outer').className")
                    .unwrap()
            ),
            "outer"
        );
        // contains: outer 가 a 를 포함
        let _ = a;
        assert_eq!(
            to_display(
                &interp
                    .run("document.getElementById('a').closest('.outer').contains(document.getElementById('a'))")
                    .unwrap()
            ),
            "true"
        );
    }

    #[test]
    fn clone_node_deep_and_shallow() {
        let mut dom = crate::html::parse_dom(
            "<div id=\"t\"><span>hi</span></div>".to_string(),
        );
        let _ = dom.find_by_attr_id("t").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        // deep clone → 자식 텍스트 포함
        let r = interp
            .run("var c = document.getElementById('t').cloneNode(true); c.textContent")
            .unwrap();
        assert_eq!(to_display(&r), "hi");
        // shallow clone → 자식 없음
        let r2 = interp
            .run("var c = document.getElementById('t').cloneNode(false); c.children.length")
            .unwrap();
        assert_eq!(to_display(&r2), "0");
    }

    #[test]
    fn dispatch_event_and_custom_event() {
        let mut dom = crate::html::parse_dom("<div id=\"box\"></div>".to_string());
        let _ = dom.find_by_attr_id("box").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        // addEventListener + dispatchEvent(CustomEvent) → 핸들러가 detail 을 읽는다
        let r = interp
            .run(
                "var got = null; \
                 var e = document.getElementById('box'); \
                 e.addEventListener('ping', function(ev) { got = ev.detail.n; }); \
                 e.dispatchEvent(new CustomEvent('ping', { detail: { n: 42 } })); \
                 got",
            )
            .unwrap();
        assert_eq!(to_display(&r), "42");
    }

    #[test]
    fn get_bounding_client_rect_and_offsets() {
        let mut dom = crate::html::parse_dom("<div id=\"box\"></div>".to_string());
        let box_id = dom.find_by_attr_id("box").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        interp.layout_rects.insert(box_id, (10.0, 20.0, 100.0, 50.0));
        // getBoundingClientRect: width/top/right/bottom
        let r = interp
            .run("var r = document.getElementById('box').getBoundingClientRect(); r.width + ',' + r.top + ',' + r.right + ',' + r.bottom")
            .unwrap();
        assert_eq!(to_display(&r), "100,20,110,70");
        // offsetWidth/offsetHeight/offsetLeft/offsetTop
        let o = interp
            .run("var e = document.getElementById('box'); e.offsetWidth + ',' + e.offsetHeight + ',' + e.offsetLeft + ',' + e.offsetTop")
            .unwrap();
        assert_eq!(to_display(&o), "100,50,10,20");
    }

    #[test]
    fn canvas_2d_records_ops() {
        let mut dom = crate::html::parse_dom("<canvas id=\"c\" width=\"100\" height=\"50\"></canvas>".to_string());
        let cid = dom.find_by_attr_id("c").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        interp
            .run(
                "var ctx = document.getElementById('c').getContext('2d'); \
                 ctx.fillStyle = '#ff0000'; ctx.fillRect(10, 20, 30, 40); \
                 ctx.beginPath(); ctx.moveTo(0,0); ctx.lineTo(50,0); ctx.lineTo(0,50); ctx.fill();",
            )
            .unwrap();
        let ops = interp.canvas_cmds.get(&cid).expect("canvas ops");
        assert_eq!(ops.len(), 2, "fillRect + fillPath");
        match &ops[0] {
            CanvasOp::FillRect { x, y, w, h, color } => {
                assert_eq!((*x, *y, *w, *h), (10.0, 20.0, 30.0, 40.0));
                assert_eq!(*color, crate::css::Color { r: 255, g: 0, b: 0, a: 255 });
            }
            other => panic!("expected FillRect, got {:?}", other),
        }
        assert!(matches!(&ops[1], CanvasOp::FillPath { pts, .. } if pts.len() == 3));
    }

    #[test]
    fn module_import_export_syntax() {
        // import 는 스킵, export 는 벗겨져 선언이 정상 동작 → 파싱 실패로 스크립트가 죽지 않음
        assert_eq!(
            run_num(
                "import foo from './foo.js'; \
                 import { a, b } from './x.js'; \
                 export const N = 42; \
                 export function add(x, y) { return x + y; } \
                 add(N, 8)"
            ),
            50.0
        );
        // export default
        assert_eq!(run_num("export default 5; var z = 7; z"), 7.0);
        // export { ... } 재익스포트 스킵
        assert_eq!(run_num("var q = 3; export { q }; q"), 3.0);
    }

    #[test]
    fn spread_array_call_object() {
        // 배열 스프레드
        assert_eq!(run_str("var a=[1,2]; var b=[0,...a,3]; b.join(',')"), "0,1,2,3");
        // 호출 인자 스프레드
        assert_eq!(run_num("function add(x,y,z){return x+y+z;} var a=[1,2,3]; add(...a)"), 6.0);
        // Math.max(...arr)
        assert_eq!(run_num("var a=[3,7,2]; Math.max(...a)"), 7.0);
        // 객체 스프레드 (병합, 뒤가 이김)
        assert_eq!(run_num("var o={a:1,b:2}; var p={...o, b:9, c:3}; p.a + p.b + p.c"), 13.0);
        // 문자열/Set 스프레드
        assert_eq!(run_str("[...'ab', 'c'].join('-')"), "a-b-c");
    }

    #[test]
    fn generators_eager() {
        // 기본 제너레이터: for-of 로 소비
        assert_eq!(
            run_num("function* g(){ yield 1; yield 2; yield 3; } var s=0; for(const x of g()) s+=x; s"),
            6.0
        );
        // .next() 직접 호출
        assert_eq!(
            run_num("function* g(){ yield 10; yield 20; } var it=g(); it.next().value + it.next().value"),
            30.0
        );
        // done 플래그
        assert!(run_bool("function* g(){ yield 1; } var it=g(); it.next(); it.next().done"));
        // yield* 위임
        assert_eq!(
            run_str("function* inner(){ yield 'a'; yield 'b'; } function* g(){ yield* inner(); yield 'c'; } var out=''; for(const x of g()) out+=x; out"),
            "abc"
        );
        // 루프 안 yield
        assert_eq!(
            run_num("function* range(n){ for(var i=0;i<n;i++) yield i; } var s=0; for(const x of range(4)) s+=x; s"),
            6.0
        );
        // 함수 식 제너레이터
        assert_eq!(
            run_num("var g = function*(){ yield 5; yield 7; }; var s=0; for(const x of g()) s+=x; s"),
            12.0
        );
    }

    #[test]
    fn for_of_iterates_values() {
        // 배열 값 순회
        assert_eq!(run_num("var s = 0; for (const x of [1,2,3,4]) s += x; s"), 10.0);
        // 문자열 문자 순회
        assert_eq!(run_str("var out = ''; for (var c of 'abc') out = c + out; out"), "cba");
        // Set 값 순회
        assert_eq!(run_num("var s = 0; for (const x of new Set([2,2,3])) s += x; s"), 5.0);
        // break 동작
        assert_eq!(run_num("var n = 0; for (const x of [1,2,3,4]) { if (x === 3) break; n++; } n"), 2.0);
    }

    #[test]
    fn array_prototype_method_dispatch() {
        // 배열 인스턴스가 Array.prototype 폴리필 메서드를 호출 (this 바인딩)
        assert_eq!(
            run_num("Array.prototype.at = function(i){ return this[i < 0 ? this.length + i : i]; }; [1,2,3].at(-1)"),
            3.0
        );
        assert_eq!(
            run_str("Array.prototype.flatMap = function(f){ return this.map(f).flat(); }; [1,2].flatMap(x => [x, x*10]).join(',')"),
            "1,10,2,20"
        );
    }

    #[test]
    fn array_sort_and_flat() {
        // 기본 정렬(문자열): 10 이 2 앞에 온다
        assert_eq!(run_str("[10, 2, 1].sort().join(',')"), "1,10,2");
        // 숫자 비교자
        assert_eq!(run_str("[10, 2, 1].sort((a, b) => a - b).join(',')"), "1,2,10");
        assert_eq!(run_str("[3, 1, 2].sort((a, b) => b - a).join(',')"), "3,2,1");
        // 제자리 정렬 + 같은 배열 반환
        assert_eq!(run_num("var a = [3,1,2]; a.sort(); a[0]"), 1.0);
        // flat 깊이 1
        assert_eq!(run_str("[1, [2, 3], 4].flat().join(',')"), "1,2,3,4");
    }

    #[test]
    fn json_roundtrip() {
        assert_eq!(run_num("JSON.parse('42')"), 42.0);
        assert_eq!(run_str("JSON.parse('\"hi\\\\n\"')"), "hi\n");
        assert_eq!(run_num("JSON.parse('[1, 2, 3]')[1]"), 2.0);
        assert_eq!(run_num("JSON.parse('{\"a\": {\"b\": 7}}').a.b"), 7.0);
        assert!(run_bool("JSON.parse('true') === true && JSON.parse('null') === null"));
        assert_eq!(run_str("JSON.stringify({ b: 2, a: 'x' })"), "{\"a\":\"x\",\"b\":2}");
        assert_eq!(run_str("JSON.stringify([1, 'two', null, true])"), "[1,\"two\",null,true]");
        // 라운드트립
        assert_eq!(
            run_str("JSON.stringify(JSON.parse('{\"k\":[1,2,{\"n\":null}]}'))"),
            "{\"k\":[1,2,{\"n\":null}]}"
        );
        // 파싱 실패는 스크립트 에러
        assert!(Interp::new().run("JSON.parse('{oops')").is_err());
    }

    #[test]
    fn for_of_destructuring() {
        // for-of 루프 변수 구조분해 (배열/entries 순회의 핵심 패턴)
        assert_eq!(run_num("var s=0; for(var [a,b] of [[1,2],[3,4]]){s+=a+b;} s"), 10.0);
        assert_eq!(run_str("var r=''; for(const [k,v] of [['x',1],['y',2]]){r+=k+v;} r"), "x1y2");
    }

    #[test]
    fn destructuring_rest() {
        // { a, ...rest } / [ f, ...tail ]
        assert_eq!(run_num("var {a,...r}={a:1,b:2,c:3}; a + r.b + r.c"), 6.0);
        assert_eq!(run_num("var [f,...t]=[1,2,3,4]; f + t.length"), 4.0);
        // 기본값 + rest 조합 (소비된 키는 rest 에서 제외)
        assert_eq!(run_num("var {x,y=9,...o}={x:1,z:5}; x + y + o.z"), 15.0);
    }

    #[test]
    fn destructuring_defaults_and_nesting() {
        // 기본값: 없는 프로퍼티/슬롯은 default 사용
        assert_eq!(run_num("var {a=3,b=4}={a:1}; a+b"), 5.0);
        assert_eq!(run_num("var [p=1,q=2]=[7]; p+q"), 9.0);
        // 중첩 구조분해
        assert_eq!(run_num("var {x:{y}}={x:{y:9}}; y"), 9.0);
        assert_eq!(run_num("var [[m],[n]]=[[3],[4]]; m+n"), 7.0);
        // 중첩 + 기본값 (없는 서브객체에 기본값 후 내부 분해)
        assert_eq!(run_num("var {d:{k=5}={}}={}; k"), 5.0);
    }

    #[test]
    fn destructuring_parameters_bind() {
        // 객체/배열 구조분해 파라미터가 실제로 바인딩돼야 (기존엔 자리표시로 버려짐)
        assert_eq!(run_num("(function({a,b}){return a+b;})({a:3,b:4})"), 7.0);
        assert_eq!(run_num("(({x,y})=>x*y)({x:5,y:6})"), 30.0);
        assert_eq!(run_num("(function([p,,q]){return p+q;})([1,2,3])"), 4.0);
    }

    #[test]
    fn rest_parameters_collect_remaining_args() {
        // ...rest 는 남은 인자를 배열로 모은다 (기존엔 단일 인자만 받았음)
        assert_eq!(run_num("(function(a, ...r){return a + r.length;})(1,2,3,4)"), 4.0);
        assert_eq!(run_num("((...n) => n.reduce((a,b)=>a+b,0))(1,2,3,4,5)"), 15.0);
        assert_eq!(run_str("(function(a, ...r){return a + r.join('');})('X','Y','Z')"), "XYZ");
    }

    #[test]
    fn tagged_template_literals() {
        // tag`a${1}b${2}c` → tag(["a","b","c"], 1, 2)
        assert_eq!(
            run_str("function t(s){return s.join('|');} t`a${1}b${2}c`"),
            "a|b|c"
        );
        assert_eq!(
            run_str("function t(s,x,y){return s[0]+x+s[1]+y+s[2];} t`(${5})[${6}]`"),
            "(5)[6]"
        );
    }

    #[test]
    fn object_literal_getters_are_invoked() {
        // { get x(){..} } 접근자는 접근 시 호출 (this=객체)
        assert_eq!(run_num("var o={n:10, get d(){return this.n*2;}}; o.d"), 20.0);
        assert_eq!(run_str("({get g(){return 'ok';}}).g"), "ok");
        // getter + setter 공존 (setter 는 무시)
        assert_eq!(run_num("({base:5, set v(x){}, get v(){return this.base+1;}}).v"), 6.0);
    }

    #[test]
    fn class_fields_and_numeric_separators() {
        // 인스턴스 필드 (this 참조 가능) + 상속 + static
        assert_eq!(run_num("class C{x=5; y=this.x+1;} var c=new C(); c.x+c.y"), 11.0);
        assert_eq!(run_num("class B{a=1;} class D extends B{b=2;} var d=new D(); d.a+d.b"), 3.0);
        assert_eq!(run_num("class E{static v=7;} E.v"), 7.0);
        // 숫자 구분자
        assert_eq!(run_num("1_000_000 + 2_500"), 1002500.0);
        assert_eq!(run_num("0xff_ff"), 65535.0);
    }

    #[test]
    fn named_function_expression_self_reference() {
        // 명명 함수식은 자기 이름으로 재귀 가능, 이름은 외부로 누출 안 됨
        assert_eq!(run_num("var f=function fac(n){return n<=1?1:n*fac(n-1)}; f(5)"), 120.0);
        assert_eq!(run_num("(function fib(n){return n<2?n:fib(n-1)+fib(n-2)})(10)"), 55.0);
        assert_eq!(run_str("var f=function g(){return typeof g}; typeof g"), "undefined");
    }

    #[test]
    fn class_getters_are_invoked() {
        // get 접근자는 프로퍼티 접근 시 호출돼 값을 낸다 (함수가 아니라)
        assert_eq!(
            run_num("class C{constructor(){this.n=20;} get double(){return this.n*2;}} new C().double"),
            40.0
        );
        // 상속된 getter
        assert_eq!(
            run_str("class B{get k(){return 'b';}} class S extends B{} new S().k"),
            "b"
        );
    }

    #[test]
    fn arguments_object() {
        // 비화살표 함수의 arguments (가변 인자 — 미니파이/구코드 흔함)
        assert_eq!(run_num("(function(){var t=0;for(var i=0;i<arguments.length;i++)t+=arguments[i];return t;})(1,2,3,4)"), 10.0);
        assert_eq!(run_str("(function(){return Array.prototype.slice.call(arguments).join('-');})('a','b')"), "a-b");
    }

    #[test]
    fn var_hoisting() {
        // var x = x || default (미니파이/UMD 흔한 패턴 — 하이스팅으로 자기참조 동작)
        assert_eq!(run_num("var a = a || 3; a"), 3.0);
        assert_eq!(run_num("(function(){ var n=n||{v:7}; return n.v; })()"), 7.0);
        // 블록 안 var 는 함수 스코프
        assert_eq!(run_num("(function(){ if(true){var z=42;} return z; })()"), 42.0);
        // for 루프 var 는 루프 밖에서도 보임
        assert_eq!(run_num("var s=0; for(var i=0;i<3;i++)s+=i; i"), 3.0);
        // 선언 전 사용 시 하이스트된 undefined
        assert_eq!(run_num("var r=(typeof q==='undefined'?1:2); var q=5; r"), 1.0);
    }

    #[test]
    fn new_regular_function_as_constructor() {
        // ES6 이전 생성자 패턴: new F() + F.prototype.method (미니파이/구코드 다수)
        assert_eq!(run_num("function P(x,y){this.x=x;this.y=y;} var p=new P(3,4); p.x+p.y"), 7.0);
        assert_eq!(
            run_num("function C(){this.n=1;} C.prototype.inc=function(){return ++this.n;}; var c=new C(); c.inc()"),
            2.0
        );
        // 함수가 객체를 반환하면 그것 우선 (JS 규칙)
        assert_eq!(run_num("function F(){return {v:99};} new F().v"), 99.0);
        assert_eq!(run_str("typeof isFinite"), "function");
    }

    #[test]
    fn instance_consults_prototype() {
        // 인스턴스 객체가 Object.prototype 메서드를 봄 (uncurryThis 및 인스턴스 호출)
        assert!(run_bool("({a:1}).hasOwnProperty('a')"));
        assert!(run_bool("!({a:1}).hasOwnProperty('b')"));
        assert_eq!(run_num("({n:5}).valueOf().n"), 5.0);
        assert_eq!(run_str("({}).toString()"), "[object Object]");
        assert!(run_bool("({a:1}).propertyIsEnumerable('a')"));
    }

    #[test]
    fn constructor_property() {
        // x.constructor → 전역 생성자 (core-js/프레임워크 타입판별에 필수)
        assert!(run_bool("[].constructor === Array"));
        assert!(run_bool("({}).constructor === Object"));
        assert_eq!(run_str("typeof (5).constructor"), "function");
        // 자체 constructor 프로퍼티가 있으면 우선
        assert_eq!(run_num("({constructor: 42}).constructor"), 42.0);
    }

    #[test]
    fn object_callable_coercion() {
        // Object(x) — 함수로 호출 시 객체 강제변환 (core-js 의 Object(this) 등)
        assert_eq!(run_num("var o={n:9}; Object(o).n"), 9.0); // 객체는 그대로
        assert_eq!(run_str("typeof Object(null)"), "object"); // null → 새 {}
        assert_eq!(run_num("var A=Object; A(7)"), 7.0); // 별칭 + 원시값 근사
    }

    #[test]
    fn window_is_global_object() {
        // window.X = v 를 맨 X 로 읽음 (브라우저에서 window 는 전역 객체)
        assert_eq!(run_num("window.foo = 42; foo"), 42.0);
        assert_eq!(run_num("window.obj = {n:7}; obj.n"), 7.0);
        // naver 패턴: window.X = window.X || {} 후 맨 X 사용
        assert_eq!(run_num("window.sdk = window.sdk || {cmd:[]}; sdk.cmd.push(1); sdk.cmd.length"), 1.0);
    }

    #[test]
    fn typeof_undeclared_returns_undefined() {
        // 미선언 식별자에 typeof 는 던지지 않고 "undefined" (기능 탐지 관용)
        assert_eq!(run_str("typeof someUndeclaredGlobal"), "undefined");
        assert_eq!(run_str("typeof require"), "undefined");
        assert_eq!(run_str("var x=5; typeof x"), "number");
        assert!(run_bool("typeof module !== 'undefined' ? false : true"));
    }

    #[test]
    fn logical_assignment_operators() {
        // ??= 는 null/undefined 일 때만, ||= 는 falsy 일 때만, &&= 는 truthy 일 때만 대입
        assert_eq!(run_num("var a=null; a??=10; a"), 10.0);
        assert_eq!(run_num("var b=5; b??=10; b"), 5.0);
        assert_eq!(run_num("var c=0; c||=99; c"), 99.0);
        assert_eq!(run_num("var d=1; d&&=7; d"), 7.0);
        // 멤버 타깃 + 단락 (두 번째 ??= 는 이미 값이 있어 무시)
        assert_eq!(run_num("var o={}; o.x??=3; o.x??=4; o.x"), 3.0);
    }

    #[test]
    fn parse_int_with_radix() {
        assert_eq!(run_num("parseInt('0xFF', 16)"), 255.0);
        assert_eq!(run_num("parseInt('FF', 16)"), 255.0);
        assert_eq!(run_num("parseInt('101', 2)"), 5.0);
        assert_eq!(run_num("parseInt('0xff')"), 255.0); // 자동 감지
        assert_eq!(run_num("parseInt('42px')"), 42.0); // 접미 무시
        assert_eq!(run_num("parseInt('z', 36)"), 35.0);
    }

    #[test]
    fn math_extended_methods() {
        assert_eq!(run_num("Math.trunc(4.7)"), 4.0);
        assert_eq!(run_num("Math.sign(-3)"), -1.0);
        assert_eq!(run_num("Math.hypot(3,4)"), 5.0);
        assert_eq!(run_num("Math.log2(8)"), 3.0);
        assert_eq!(run_num("Math.cbrt(27)"), 3.0);
        assert_eq!(run_num("Math.log10(1000)"), 3.0);
    }

    #[test]
    fn bitwise_operators() {
        assert_eq!(run_num("5 ^ 3"), 6.0);
        assert_eq!(run_num("5 & 3"), 1.0);
        assert_eq!(run_num("5 | 2"), 7.0);
        assert_eq!(run_num("~5"), -6.0);
        assert_eq!(run_num("1 << 8"), 256.0);
        assert_eq!(run_num("-8 >> 1"), -4.0);
        assert_eq!(run_num("-1 >>> 28"), 15.0);
        assert_eq!(run_num("4294967296 | 0"), 0.0, "ToInt32 랩어라운드");
        assert_eq!(run_num("3.9 | 0"), 3.0, "| 0 절삭 관용구");
        // 우선순위: & > ^ > | , 시프트 > 비교
        assert_eq!(run_num("1 | 2 & 3"), 3.0);
        assert!(run_bool("1 << 2 > 3"));
        assert!(run_bool("(5 & 3) === 1 && true"));
    }

    #[test]
    fn template_literals() {
        assert_eq!(run_str("var x = 3; `a ${x + 1} b`"), "a 4 b");
        assert_eq!(run_str("`no interp`"), "no interp");
        assert_eq!(run_str("``"), "");
        assert_eq!(run_str("`line1\nline2`"), "line1\nline2", "리터럴 줄바꿈 허용");
        assert_eq!(run_str("`\\`tick\\` ${'and'} \\${notinterp}`"), "`tick` and ${notinterp}");
        // 보간 안에 중괄호 포함 문자열
        assert_eq!(run_str("`v=${ '{'.length }`"), "v=1");
        // 중첩 식
        assert_eq!(run_str("var f = n => n * 2; `r=${f(3) + 1}`"), "r=7");
    }

    #[test]
    fn try_catch_finally_throw() {
        assert_eq!(run_str("try { throw 'boom'; } catch (e) { 'caught ' + e }"), "caught boom");
        // throw 된 값 그대로 바인딩 (객체)
        assert_eq!(
            run_num("try { throw { code: 42 }; } catch (e) { e.code }"),
            42.0
        );
        // 네이티브 런타임 에러도 잡힘
        assert_eq!(run_str("try { undefinedVar + 1; } catch (e) { 'survived' }"), "survived");
        // finally 는 항상 실행
        assert_eq!(
            run_str("var log = ''; try { log += 'a'; throw 1; } catch (e) { log += 'b'; } finally { log += 'c'; } log"),
            "abc"
        );
        // catch 없는 try/finally: 에러 전파 + finally 실행
        assert!(Interp::new()
            .run("var x = 0; try { throw 'up'; } finally { x = 1; }")
            .is_err());
        // 바인딩 생략 catch (ES2019)
        assert_eq!(run_num("try { throw 9; } catch { 7 }"), 7.0);
        // 함수 경계 넘는 전파
        assert_eq!(
            run_str("function f() { throw 'deep'; } try { f(); } catch (e) { e }"),
            "deep"
        );
        // 스텝 한도는 잡을 수 없음
        assert!(Interp::new().run("try { while (true) {} } catch (e) { 'nope' }").is_err());
    }

    #[test]
    fn switch_statement() {
        let src = "function grade(n) { \
             switch (n) { \
               case 1: return 'one'; \
               case 2: \
               case 3: return 'few'; \
               default: return 'many'; \
             } \
           }";
        assert_eq!(run_str(&format!("{} grade(1)", src)), "one");
        assert_eq!(run_str(&format!("{} grade(2)", src)), "few", "폴스루");
        assert_eq!(run_str(&format!("{} grade(3)", src)), "few");
        assert_eq!(run_str(&format!("{} grade(99)", src)), "many");
        // break 로 탈출, 문자열 판별, 스위치 뒤 계속 실행
        assert_eq!(
            run_num("var r = 0; switch ('b') { case 'a': r = 1; break; case 'b': r = 2; break; case 'c': r = 3; } r"),
            2.0
        );
        // 엄격 비교 (1 !== '1')
        assert_eq!(
            run_num("var r = 0; switch ('1') { case 1: r = 10; break; default: r = 20; } r"),
            20.0
        );
    }

    #[test]
    fn object_method_shorthand() {
        assert_eq!(run_num("var o = { double(n) { return n * 2; } }; o.double(4)"), 8.0);
        assert_eq!(
            run_str("var api = { name: 'k', hello() { return 'hi'; }, }; api.hello() + api.name"),
            "hik"
        );
    }

    #[test]
    fn regex_literal_tolerated_and_division_intact() {
        // 정규식 리터럴이 렉서를 죽이지 않고 {source, flags} 객체가 됨
        assert_eq!(run_str("var re = /a[/]b+/gi; re.source"), "a[/]b+");
        assert_eq!(run_str("var re = /x/; re.flags !== undefined ? 'obj' : 'no'"), "obj");
        // 나눗셈은 그대로
        assert_eq!(run_num("10 / 2"), 5.0);
        assert_eq!(run_num("var a = 8; a / 2 / 2"), 2.0);
        assert_eq!(run_num("(4 + 4) / 2"), 4.0);
        assert_eq!(run_num("var x = 9; x /= 3; x"), 3.0);
        // return 뒤는 정규식 문맥
        assert_eq!(run_str("function f() { return /ok/.source; } f()"), "ok");
    }

    #[test]
    fn labeled_statements_and_labeled_break() {
        // 레이블은 파싱만 하고 무시 (break label = 일반 break)
        assert_eq!(
            run_num("var n = 0; outer: for (var i = 0; i < 3; i++) { n++; break outer; } n"),
            1.0
        );
        assert_eq!(
            run_num("var s = 0; loop: while (s < 5) { s++; continue loop; } s"),
            5.0
        );
    }

    #[test]
    fn array_holes() {
        assert_eq!(run_num("[1,,2].length"), 3.0);
        assert!(run_bool("[1,,2][1] === undefined"));
        assert_eq!(run_num("[,,].length"), 2.0);
    }

    #[test]
    fn hash_identifiers_tolerated() {
        // 클래스 미지원이지만 #priv 가 렉서를 죽이진 않음
        assert!(super::super::lexer::tokenize("obj.#priv").is_ok());
    }

    #[test]
    fn storage_and_misc_stubs() {
        // localStorage 는 실제로 동작 (페이지 수명)
        assert_eq!(
            run_str("localStorage.setItem('k', 'v1'); localStorage.getItem('k')"),
            "v1"
        );
        assert!(run_bool("localStorage.getItem('none') === null"));
        assert!(run_bool(
            "localStorage.setItem('x', 1); localStorage.removeItem('x'); localStorage.getItem('x') === null"
        ));
        // window 를 통해서도 같은 스토리지
        assert_eq!(
            run_str("window.localStorage.setItem('w', 'ok'); localStorage.getItem('w')"),
            "ok"
        );
        assert!(run_bool("typeof navigator.userAgent === 'string'"));
        // alert 는 콘솔로
        let mut it = Interp::new();
        it.run("alert('hi', 2)").unwrap();
        assert_eq!(it.console, vec!["[alert] hi 2"]);
        // window.addEventListener 는 no-op (죽지 않음)
        assert!(Interp::new().run("window.addEventListener('load', x => x)").is_ok());
    }

    #[test]
    fn class_basics_this_and_methods() {
        let src = "class Counter { \
             constructor(start) { this.n = start; } \
             inc() { this.n = this.n + 1; return this.n; } \
             get() { return this.n; } \
           }";
        assert_eq!(run_num(&format!("{} var c = new Counter(10); c.inc(); c.inc()", src)), 12.0);
        assert_eq!(run_num(&format!("{} var c = new Counter(5); c.get()", src)), 5.0);
        // 각 인스턴스는 독립 상태
        assert_eq!(
            run_num(&format!(
                "{} var a = new Counter(0), b = new Counter(100); a.inc(); b.get()",
                src
            )),
            100.0
        );
    }

    #[test]
    fn class_this_in_arrow_is_lexical() {
        // 메서드 안 화살표가 바깥 this 를 캡처
        let src = "class Box { \
             constructor(v) { this.v = v; } \
             mapped(arr) { return arr.map(x => x + this.v); } \
           }";
        assert_eq!(
            run_str(&format!("{} new Box(10).mapped([1, 2, 3]).join(',')", src)),
            "11,12,13"
        );
    }

    #[test]
    fn class_inheritance_and_super() {
        let src = "class Animal { \
             constructor(name) { this.name = name; } \
             speak() { return this.name + ' makes a sound'; } \
           } \
           class Dog extends Animal { \
             constructor(name) { super(name); this.legs = 4; } \
             speak() { return super.speak() + ' (woof)'; } \
           }";
        assert_eq!(
            run_str(&format!("{} new Dog('Rex').speak()", src)),
            "Rex makes a sound (woof)"
        );
        assert_eq!(run_num(&format!("{} new Dog('Rex').legs", src)), 4.0);
        // 상속받은 필드 접근
        assert_eq!(run_str(&format!("{} new Dog('Fido').name", src)), "Fido");
        // instanceof 는 체인 전체
        assert!(run_bool(&format!("{} var d = new Dog('x'); d instanceof Dog", src)));
        assert!(run_bool(&format!("{} var d = new Dog('x'); d instanceof Animal", src)));
    }

    #[test]
    fn unary_plus_and_self_global() {
        assert_eq!(run_num("+'42'"), 42.0);
        assert_eq!(run_num("var a = '3'; a = +a; a + 1"), 4.0);
        assert!(run_bool("+true === 1"));
        // self / globalThis 는 window 별칭
        assert!(run_bool("self.localStorage !== undefined"));
        assert!(run_bool("typeof globalThis === 'object'"));
        // void 0 === undefined 관용구
        assert!(run_bool("void 0 === undefined"));
        assert!(run_bool("var x = 5; (x === void 0) === false"));
        // 선행 소수점 숫자
        assert_eq!(run_num(".5 + .25"), 0.75);
        assert!(run_bool("0.3 >= .1"));
        // 예약어를 프로퍼티 이름으로
        assert_eq!(run_num("var o = {}; o.for = 3; o['default'] = 4; o.for + o.default"), 7.0);
    }

    #[test]
    fn class_static_members() {
        let src = "class MathUtil { \
             static double(n) { return n * 2; } \
           }";
        assert_eq!(run_num(&format!("{} MathUtil.double(21)", src)), 42.0);
    }

    #[test]
    fn class_expression_and_new_error() {
        // 클래스 식
        assert_eq!(
            run_num("var C = class { constructor() { this.x = 7; } }; new C().x"),
            7.0
        );
        // 네이티브 생성자 스텁: new Error('msg') → message
        assert_eq!(run_str("var e = new Error('boom'); e.message"), "boom");
        // throw new + try/catch 조합
        assert_eq!(
            run_str("try { throw new Error('bad'); } catch (e) { e.message }"),
            "bad"
        );
    }

    #[test]
    fn set_timeout_registers_and_clear_cancels() {
        let mut it = Interp::new();
        it.run("setTimeout(function() {}, 100); setInterval(function() {}, 50)").unwrap();
        assert_eq!(it.timers.len(), 2);
        assert_eq!(it.timers[0].delay_ms, 100.0);
        assert!(!it.timers[0].repeat);
        assert!(it.timers[1].repeat);
        // clearTimeout 은 id 로 취소
        let mut it2 = Interp::new();
        it2.run("var id = setTimeout(function() {}, 10); clearTimeout(id);").unwrap();
        assert!(it2.timers.is_empty(), "취소된 타이머 제거");
    }

    #[test]
    fn set_timeout_returns_incrementing_ids() {
        let mut it = Interp::new();
        let a = it.run("setTimeout(function() {}, 0)").unwrap();
        let b = it.run("setTimeout(function() {}, 0)").unwrap();
        assert!(matches!((a, b), (Value::Num(x), Value::Num(y)) if y > x));
    }

    #[test]
    fn compound_assignments() {
        assert_eq!(run_num("var x = 10; x %= 3; x"), 1.0);
        assert_eq!(run_num("var x = 6; x &= 3; x"), 2.0);
        assert_eq!(run_num("var x = 5; x |= 2; x"), 7.0);
        assert_eq!(run_num("var x = 5; x ^= 1; x"), 4.0);
        assert_eq!(run_num("var x = 1; x <<= 4; x"), 16.0);
        assert_eq!(run_num("var x = 64; x >>= 2; x"), 16.0);
        // 멤버 복합 대입
        assert_eq!(run_num("var o = { n: 10 }; o.n += 5; o.n"), 15.0);
        // 논리 대입 (단락)
        assert_eq!(run_str("var a = ''; a ||= 'fallback'; a"), "fallback");
        assert_eq!(run_num("var a = 5; a &&= 9; a"), 9.0);
        assert_eq!(run_str("var a = 'keep'; a ||= 'no'; a"), "keep");
    }

    #[test]
    fn optional_chaining_and_nullish() {
        assert!(run_bool("var o = null; o?.x === undefined"));
        assert!(run_bool("var o = { a: { b: 5 } }; o?.a?.b === 5"));
        assert!(run_bool("var o = {}; o?.a?.b === undefined"));
        // 옵셔널 인덱스/호출
        assert!(run_bool("var o = null; o?.['x'] === undefined"));
        assert!(run_bool("var f = null; f?.(1, 2) === undefined"));
        assert_eq!(run_num("var o = { f: function() { return 7; } }; o.f?.()"), 7.0);
        // nullish 병합: null/undefined 만 폴백 (0/'' 는 그대로)
        assert_eq!(run_num("var x = 0; x ?? 9"), 0.0);
        assert_eq!(run_str("null ?? 'd'"), "d");
        assert_eq!(run_str("undefined ?? 'd'"), "d");
        assert_eq!(run_num("var o = {}; o.missing ?? 42"), 42.0);
    }

    #[test]
    fn destructuring_declarations() {
        assert_eq!(run_num("var { a, b } = { a: 1, b: 2 }; a + b"), 3.0);
        assert_eq!(run_str("var { x: first } = { x: 'hi' }; first"), "hi");
        assert_eq!(run_num("var [p, q] = [10, 20]; p + q"), 30.0);
        assert_eq!(run_num("var [, second] = [1, 2]; second"), 2.0);
        // 중첩 없는 혼합/누락
        assert!(run_bool("var { z } = {}; z === undefined"));
        assert_eq!(run_num("var [a, b, c] = [1, 2]; a + b + (c === undefined ? 100 : 0)"), 103.0);
        // 함수 반환값 구조분해
        assert_eq!(
            run_num("function pair() { return { lo: 3, hi: 7 }; } var { lo, hi } = pair(); hi - lo"),
            4.0
        );
    }

    #[test]
    fn multi_declarator_and_comma_operator() {
        // 미니파이 코드의 두 필수 패턴
        assert_eq!(run_num("var a = 1, b = 2, c; c = a + b; c"), 3.0);
        assert_eq!(run_num("let x = 1, y = x + 1; y"), 2.0);
        assert_eq!(run_num("var a; a = (1, 2, 3)"), 3.0, "콤마 연산자: 마지막 값");
        assert_eq!(
            run_num("var s = 0; for (var i = 0, j = 10; i < j; i++, j--) s++; s"),
            5.0
        );
        // 함수 인자의 콤마는 구분자 그대로
        assert_eq!(run_num("Math.max(1, 2, 3)"), 3.0);
    }

    #[test]
    fn for_in_iterates_keys_and_indices() {
        assert_eq!(
            run_num("var o = { a: 1, b: 2, c: 3 }; var n = 0; for (var k in o) n += o[k]; n"),
            6.0
        );
        assert_eq!(
            run_str("var out = ''; for (var i in ['x', 'y']) out += i; out"),
            "01"
        );
        assert_eq!(run_num("var n = 0; for (k in null) n++; n"), 0.0);
    }

    #[test]
    fn instanceof_and_in_operators() {
        assert!(run_bool("[1] instanceof Array"));
        assert!(run_bool("({}) instanceof Object"));
        assert!(!run_bool("'str' instanceof Array"));
        assert!(!run_bool("[] instanceof RegExp"));
        assert!(run_bool("'a' in { a: 1 }"));
        assert!(!run_bool("'z' in { a: 1 }"));
        assert!(run_bool("0 in [7]"));
        assert!(!run_bool("3 in [7]"));
    }

    #[test]
    fn object_array_statics() {
        assert_eq!(run_num("Object.keys({ a: 1, b: 2 }).length"), 2.0);
        assert_eq!(
            run_num("var t = { a: 1 }; Object.assign(t, { b: 2 }, { c: 3 }); Object.keys(t).length"),
            3.0
        );
        assert!(run_bool("Array.isArray([1]) && !Array.isArray('no')"));
    }

    #[test]
    fn parse_errors_include_token_context() {
        let err = Interp::new().run("var x = ;").unwrap_err();
        assert!(err.contains("근처"), "에러에 토큰 문맥 포함: {}", err);
    }

    #[test]
    fn window_and_screen_metrics() {
        let mut it = Interp::new();
        assert!(matches!(it.run("window.innerWidth").unwrap(), Value::Num(n) if n == 1000.0));
        assert!(matches!(it.run("window.devicePixelRatio").unwrap(), Value::Num(n) if n == 1.0));
        assert!(matches!(it.run("screen.width").unwrap(), Value::Num(n) if n == 1000.0));
        assert!(matches!(it.run("window.screen.height").unwrap(), Value::Num(n) if n == 800.0));
    }

    #[test]
    fn this_defaults_to_window() {
        // 최상위 this === window, 일반 함수 호출의 this === window (sloppy)
        let mut it = Interp::new();
        assert!(matches!(it.run("this === window").unwrap(), Value::Bool(true)));
        it.run("function f(){ return this === window; }").unwrap();
        assert!(matches!(it.run("f()").unwrap(), Value::Bool(true)));
        // .call(this) 로 window 에 프로퍼티 설정 (구글 gbar 패턴)
        it.run("(function(){ this.gv = 42; }).call(this);").unwrap();
        assert!(matches!(it.run("window.gv").unwrap(), Value::Num(n) if n == 42.0));
    }

    #[test]
    fn location_reflects_page_url() {
        let mut it = Interp::new();
        it.install_location("https://example.com/a/b?q=1#top");
        // pathname 은 쿼리 제외, search/hash 분리 (DOM 표준)
        let v = it.run("location.pathname + '|' + location.search + '|' + location.hash").unwrap();
        match v {
            Value::Str(s) => assert_eq!(s, "/a/b|?q=1|#top"),
            other => panic!("{:?}", other),
        }
        assert!(matches!(it.run("location.hostname").unwrap(), Value::Str(s) if s == "example.com"));
        assert!(matches!(it.run("location.origin").unwrap(), Value::Str(s) if s == "https://example.com"));
        let w = it.run("window.location.href").unwrap();
        assert!(matches!(w, Value::Str(s) if s.starts_with("https://example.com")));
        // location.search.indexOf 가 동작해야 (구글 등에서 흔한 패턴)
        assert!(matches!(it.run("location.search.indexOf('q')").unwrap(), Value::Num(n) if n == 1.0));
    }

    #[test]
    fn global_number_functions() {
        assert_eq!(run_num("parseInt('42px')"), 42.0);
        assert_eq!(run_num("parseInt('-7')"), -7.0);
        assert!(run_bool("isNaN(parseInt('abc'))"));
        assert_eq!(run_num("parseFloat('3.14 rad')"), 3.14);
        assert!(run_bool("isNaN('x' * 2)"));
        assert!(run_bool("!isNaN(5)"));
    }
}
