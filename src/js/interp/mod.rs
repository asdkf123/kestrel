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
mod generator;
use generator::GenState;
use value::*;

const STEP_LIMIT: u64 = 5_000_000;
// 이 접두사의 에러는 try/catch 로 잡을 수 없다 (무한 루프 가드가 무력화되지 않게)
const STEP_LIMIT_MSG: &str = "실행 한도 초과";

// 정규 배열 인덱스인가 (0 ~ 2^32-2, 선행 0 없음). 열거 순서 결정에 쓰인다.
fn array_index(k: &str) -> Option<u32> {
    if k.is_empty() || (k.len() > 1 && k.starts_with('0')) {
        return None;
    }
    match k.parse::<u32>() {
        Ok(n) if n != u32::MAX => Some(n),
        _ => None,
    }
}

// 삽입 순서를 유지하는 객체 프로퍼티 맵 (ECMAScript OrdinaryOwnPropertyKeys):
// 정수 인덱스 키는 오름차순으로 먼저, 그다음 문자열 키는 삽입 순서.
// HashMap 과 같은 메서드 이름을 노출해 호출부를 그대로 둔다.
#[derive(Clone, Debug, Default)]
pub struct ObjMap {
    entries: Vec<(String, Value)>,
    index: HashMap<String, usize>,
}

impl ObjMap {
    pub fn new() -> ObjMap {
        ObjMap::default()
    }
    pub fn get(&self, k: &str) -> Option<&Value> {
        self.index.get(k).map(|&i| &self.entries[i].1)
    }
    pub fn contains_key(&self, k: &str) -> bool {
        self.index.contains_key(k)
    }
    // 정수 인덱스 키는 정렬 위치에, 문자열 키는 끝에 삽입할 위치를 구한다.
    fn insert_position(&self, k: &str) -> usize {
        match array_index(k) {
            Some(kn) => {
                let mut pos = 0;
                for (ek, _) in &self.entries {
                    match array_index(ek) {
                        Some(en) if en < kn => pos += 1,
                        _ => break, // 더 큰 정수키 또는 문자열키 → 여기 삽입
                    }
                }
                pos
            }
            None => self.entries.len(),
        }
    }
    pub fn insert(&mut self, k: String, v: Value) -> Option<Value> {
        if let Some(&i) = self.index.get(&k) {
            return Some(std::mem::replace(&mut self.entries[i].1, v));
        }
        let pos = self.insert_position(&k);
        self.entries.insert(pos, (k, v));
        for i in pos..self.entries.len() {
            self.index.insert(self.entries[i].0.clone(), i);
        }
        None
    }
    pub fn remove(&mut self, k: &str) -> Option<Value> {
        let &i = self.index.get(k)?;
        let (_, v) = self.entries.remove(i);
        self.index.remove(k);
        for j in i..self.entries.len() {
            self.index.insert(self.entries[j].0.clone(), j);
        }
        Some(v)
    }
    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.entries.iter().map(|(k, _)| k)
    }
    pub fn iter(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.entries.iter().map(|(k, v)| (k, v))
    }
}

#[derive(Clone)]
pub enum Value {
    Undefined,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Obj(Rc<RefCell<ObjMap>>),
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
    // function* 로 만든 지연 제너레이터. 호출 시 즉시 평가하지 않고, next()마다 다음
    // yield 까지 본문을 재개 실행한다(무한 제너레이터/양방향 next(v) 지원). generator.rs.
    Gen(Rc<RefCell<GenState>>),
    // Symbol 원시값. key 는 프로퍼티 키로 쓰일 때의 문자열(잘 알려진 심볼은 "\u{0}@@iterator"
    // 등 고정, 일반 심볼은 "\u{0}@@sym:<n>" 고유). 동일성(===)은 key 비교. desc 는 설명.
    Symbol(Rc<SymbolData>),
    // getComputedStyle(el) 이 돌려주는 읽기전용 계산 스타일 뷰. 요소 NodeId 로
    // computed_styles 맵을 조회한다(카멜케이스/대시 프로퍼티 + getPropertyValue).
    ComputedStyle(crate::dom::NodeId),
}

// Symbol 원시값의 데이터. key 로 프로퍼티 저장 키와 동일성을 동시에 표현한다.
pub struct SymbolData {
    pub key: String,
    pub desc: Option<String>,
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
    // 인덱스 외의 own 프로퍼티 (엔진 내부 마커 제외) — Object.assign 등의 열거용
    pub fn own_props(&self) -> Vec<(String, Value)> {
        self.props
            .borrow()
            .iter()
            .filter(|(k, _)| !is_internal_key(k.as_str()))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
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
    // 이 함수가 클래스 메서드면 그 클래스의 부모 생성자 (super.x 해석용).
    // 클래스일 수도, 일반 생성자(Error/함수)일 수도 있어 Value 로 둔다.
    pub super_class: Option<Value>,
    // 함수도 객체: F.prototype / F.staticProp 등 (Rc 공유 → 변경 반영)
    pub props: RefCell<HashMap<String, Value>>,
}

pub struct JsClass {
    pub name: String,
    pub parent: Option<Rc<JsClass>>,
    // 클래스가 아닌 생성자를 확장한 경우(class E extends Error / extends function).
    // 표준은 아무 생성자나 확장 가능하다. super() 는 이 생성자를 실행해 this 를 채운다.
    pub parent_ctor: Option<Value>,
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

    // 클래스 체인을 올라가며 첫 non-class 부모 생성자를 찾는다 (extends Error 등).
    fn find_parent_ctor(&self) -> Option<Value> {
        if let Some(pc) = &self.parent_ctor {
            return Some(pc.clone());
        }
        self.parent.as_ref().and_then(|p| p.find_parent_ctor())
    }
}

pub struct Instance {
    pub class: Rc<JsClass>,
    pub fields: RefCell<HashMap<String, Value>>,
}

// canvas 2D 컨텍스트 메서드
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    // Object/Array 전역 자체 (typeof 가 "function" 이어야 하고 호출/new 가 가능해야 함)
    ObjectCtor,
    ArrayCtor,
    ObjectDefineProperty,
    ObjectCreate,
    ObjectFreeze,
    ObjectSeal,
    ObjectPreventExt,
    ObjectIsFrozen,
    ObjectIsSealed,
    ObjectIsExtensible,
    ObjectGetPrototypeOf,
    HasOwnProperty,
    ObjToString,
    ReturnFalse,
    ReturnThis, // valueOf 등 — 수신자(this) 반환
    FnToString, // Function.prototype.toString
    MakeIter,
    IterNext,
    // 지연 제너레이터 반복자 프로토콜 (generator.rs)
    GenNext,
    GenReturn,
    GenThrow,
    // Symbol 원시값 (Symbol()/Symbol.for/Symbol.keyFor)
    SymbolCtor,
    SymbolFor,
    SymbolKeyFor,
    // getComputedStyle(el) 과 그 반환 뷰의 getPropertyValue(name)
    GetComputedStyle,
    ComputedGetProperty,
    // a.compareDocumentPosition(b) — 문서 순서 비트마스크 (jQuery sortOrder)
    CompareDocPosition,
    // document.implementation.createHTMLDocument(title) — 분리된 문서
    CreateHTMLDocument,
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
    DateParse, // Date.parse(str) → millis
    DateUTC,   // Date.UTC(y,m,d,...) → millis
    DateCtor,
    DateMethod(DateField),
    XhrCtor,
    UrlCtor,
    UrlToString,
    UrlSearchGet,
    UrlSearchGetAll,
    UrlSearchHas,
    UrlSearchSet,
    UrlSearchAppend,
    UrlSearchDelete,
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
    StructuredClone,
    ReflectGet,
    ReflectSet,
    ReflectHas,
    ReflectDeleteProperty,
    ReflectApply,
    ReflectConstruct,
    ObjectKeys,
    ObjectValues,
    ObjectEntries,
    ObjectFromEntries,
    ObjectAssign,
    ArrayIsArray,
    ArrayFrom, // Array.from(iterable|array-like, mapFn?)
    ArrayOf,   // Array.of(...args)
    SetTimeout,
    SetInterval,
    ClearTimer,
    // Promise/fetch
    PromiseCtor,          // new Promise(executor)
    PromiseSettleResolve, // executor 의 resolve (this=promise)
    PromiseSettleReject,  // executor 의 reject (this=promise)
    PromiseResolve,
    PromiseReject,
    PromiseAll,
    PromiseRace,
    PromiseAllSettled,
    PromiseThen,
    PromiseCatch,
    PromiseFinally,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum StrOp {
    IndexOf,
    LastIndexOf,
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
    At,
    LocaleCompare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SetOp {
    Add,
    Has,
    Delete,
    Clear,
    ForEach,
    Values,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
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
    FlatMap,
    At,
    FindLast,
    FindLastIndex,
    Fill,
    ReduceRight,
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
            Value::Gen(_) => write!(f, "[object Generator]"),
            Value::Symbol(s) => write!(f, "Symbol({})", s.desc.as_deref().unwrap_or("")),
            Value::ComputedStyle(id) => write!(f, "[computedStyle {:?}]", id),
        }
    }
}

// ── 환경 (스코프 체인) ──────────────────────────────────────────────

pub type EnvRef = Rc<RefCell<Env>>;

pub struct Env {
    vars: HashMap<String, Value>,
    // const 로 선언된 이름 (재대입 시 TypeError). 바인딩과 같은 스코프에 표시.
    consts: std::collections::HashSet<String>,
    parent: Option<EnvRef>,
}

impl Env {
    fn new(parent: Option<EnvRef>) -> EnvRef {
        Rc::new(RefCell::new(Env {
            vars: HashMap::new(),
            consts: std::collections::HashSet::new(),
            parent,
        }))
    }
}

// name 바인딩이 있는 스코프에서 그것이 const 인가 (체인 탐색, env_get 과 동일 해석).
fn env_is_const(env: &EnvRef, name: &str) -> bool {
    let (has, is_const, parent) = {
        let e = env.borrow();
        (e.vars.contains_key(name), e.consts.contains(name), e.parent.clone())
    };
    if has {
        return is_const;
    }
    parent.map_or(false, |p| env_is_const(&p, name))
}

// getComputedStyle 프로퍼티명: 카멜케이스 → CSS 대시. backgroundColor → background-color,
// cssFloat → float, webkitTransform → -webkit-transform. 이미 대시면 그대로.
fn camel_to_dashed(s: &str) -> String {
    if s == "cssFloat" || s == "styleFloat" {
        return "float".to_string();
    }
    if s.contains('-') || !s.chars().any(|c| c.is_ascii_uppercase()) {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len() + 2);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            // 선두 대문자(webkit/moz/ms/o 벤더)는 앞에도 대시
            if i == 0 {
                out.push('-');
            } else {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
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
    // 계산된 스타일 (NodeId → 대시 프로퍼티명 → CSS 텍스트). 리빌드 후 호스트가 채움.
    // getComputedStyle 이 읽는다. 빈 맵이면 빈 문자열.
    pub computed_styles: std::collections::HashMap<crate::dom::NodeId, HashMap<String, String>>,
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
    // Object/Array 의 정적 멤버·prototype 을 담은 네임스페이스 맵.
    // 전역은 Native 생성자이고, 멤버 조회는 이 맵에 위임한다.
    object_ns: Value,
    array_ns: Value,
    date_proto: Value,
    symbol_proto: Value,
    // 페이지 기준 URL (상대 URL 해석용 — XHR/fetch)
    base_url: Option<String>,
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
}

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
        // 전역 생성자 스텁 (instanceof 판별 + 정적 메서드)
        let mut object_ns = ObjMap::new();
        object_ns.insert("keys".to_string(), Value::Native(Native::ObjectKeys));
        object_ns.insert("values".to_string(), Value::Native(Native::ObjectValues));
        object_ns.insert("entries".to_string(), Value::Native(Native::ObjectEntries));
        object_ns.insert("fromEntries".to_string(), Value::Native(Native::ObjectFromEntries));
        object_ns.insert("getOwnPropertyNames".to_string(), Value::Native(Native::ObjectKeys));
        object_ns.insert("assign".to_string(), Value::Native(Native::ObjectAssign));
        object_ns.insert("defineProperty".to_string(), Value::Native(Native::ObjectDefineProperty));
        object_ns.insert("defineProperties".to_string(), Value::Native(Native::ObjectDefineProperty));
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
        // Object.prototype: hasOwnProperty(webpack .o), toString(타입 판별 관용),
        // isPrototypeOf/propertyIsEnumerable/valueOf
        let mut object_proto = ObjMap::new();
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
        window.insert("removeEventListener".to_string(), Value::Native(Native::Noop));
        window.insert("getComputedStyle".to_string(), Value::Native(Native::GetComputedStyle));
        window.insert("requestAnimationFrame".to_string(), Value::Native(Native::SetTimeout));
        window.insert("cancelAnimationFrame".to_string(), Value::Native(Native::ClearTimer));
        window.insert("setTimeout".to_string(), Value::Native(Native::SetTimeout));
        window.insert("setInterval".to_string(), Value::Native(Native::SetInterval));
        // Event 생성자류(window.Event.prototype 참조 등) — 모두 EventCtor 로 근사.
        for ev in ["Event", "CustomEvent", "MouseEvent", "KeyboardEvent", "PointerEvent", "FocusEvent", "InputEvent"] {
            window.insert(ev.to_string(), Value::Native(Native::EventCtor));
        }
        // history 스텁 (pushState/replaceState 등은 URL 만 갱신하지 않는 no-op).
        let mut history = ObjMap::new();
        for m in ["pushState", "replaceState", "back", "forward", "go"] {
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
            ("toISOString", Native::DateMethod(DateField::ToIso)),
            ("toJSON", Native::DateMethod(DateField::ToIso)),
            ("toString", Native::DateMethod(DateField::ToStr)),
        ]);
        let symbol_proto = mk_proto(vec![
            ("toString", Native::ValueToStr),
            ("valueOf", Native::ValueOfSelf),
        ]);
        // Error.prototype — core-js/번들이 Error.prototype 을 참조(확장/기능 탐지).
        let error_proto = mk_proto(vec![("toString", Native::ObjToString)]);
        if let Value::Obj(m) = &error_proto {
            m.borrow_mut().insert("name".to_string(), Value::Str("Error".to_string()));
            m.borrow_mut().insert("message".to_string(), Value::Str(String::new()));
        }
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
            computed_styles: std::collections::HashMap::new(),
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
            map_proto,
            set_proto,
            error_proto,
            object_ns,
            array_ns,
            date_proto,
            symbol_proto,
            string_proto,
            number_proto,
            boolean_proto,
            regexp_proto,
            base_url: None,
            lenient: std::env::var("KESTREL_LENIENT").is_ok(),
            lenient_hits: std::collections::HashMap::new(),
            pending_label: None,
            sym_counter: 0,
            sym_registry: HashMap::new(),
            new_target: None,
            window_obj,
            native_props: HashMap::new(),
        }
    }

    // 새 pending Promise (Obj 표현: 상태·값·대기콜백을 맵에 저장, then/catch 는 Native)
    fn new_promise(&self) -> Value {
        let mut m = ObjMap::new();
        m.insert("__isPromise".to_string(), Value::Bool(true));
        m.insert("__state".to_string(), Value::Str("pending".to_string()));
        m.insert("__value".to_string(), Value::Undefined);
        m.insert("__cbs".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
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
            if !matches!(m.get("__state"), Some(Value::Str(s)) if s == "pending") {
                return; // 이미 정착 — 한 번만
            }
        }
        let cbs = {
            let mut m = o.borrow_mut();
            let state = if fulfilled { "fulfilled" } else { "rejected" };
            m.insert("__state".to_string(), Value::Str(state.to_string()));
            m.insert("__value".to_string(), value.clone());
            match m.get("__cbs") {
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
                match m.get("__state") {
                    Some(Value::Str(s)) => s.clone(),
                    _ => "pending".into(),
                },
                m.get("__value").cloned().unwrap_or(Value::Undefined),
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
                if let Some(Value::Arr(a)) = o.borrow().get("__cbs") {
                    a.borrow_mut().push(entry);
                }
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
            let state = match m.get("__state") {
                Some(Value::Str(s)) => s.clone(),
                _ => "pending".into(),
            };
            let value = m.get("__value").cloned().unwrap_or(Value::Undefined);
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
        let mut sp = ObjMap::new();
        sp.insert("__query".to_string(), Value::Str(search.trim_start_matches('?').to_string()));
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
                out.extend(self.iterate_to_vec(&v));
            } else {
                out.push(self.eval(a, env)?);
            }
        }
        Ok(out)
    }

    // 값들의 Vec 을 반복자 객체로 (MakeIter 와 동일 구조: __items/__i/next).
    fn make_iter_from_vec(&self, items: Vec<Value>) -> Value {
        let mut it = ObjMap::new();
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
            // 재료화된 반복자 객체(__items)는 그대로.
            Value::Obj(o) if o.borrow().contains_key("__items") => {
                match o.borrow().get("__items") {
                    Some(Value::Arr(items)) => items.borrow().clone(),
                    _ => Vec::new(),
                }
            }
            // 그 외: 반복자 프로토콜(제너레이터/사용자 [Symbol.iterator]/반복자 객체)로
            // done 까지 재료화. 무한이면 step 상한이 방어.
            _ => {
                let it = match self.try_get_iterator(v) {
                    Ok(Some(it)) => it,
                    _ => return Vec::new(),
                };
                let mut out = Vec::new();
                loop {
                    match self.gen_iter_next(&it, Value::Undefined) {
                        Ok((val, done)) => {
                            if done {
                                break;
                            }
                            out.push(val);
                        }
                        Err(_) => break,
                    }
                    if self.tick().is_err() {
                        break;
                    }
                }
                out
            }
        }
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
                    let mut map = ObjMap::new();
                    match &value {
                        Value::Obj(o) => {
                            for (k, v) in o.borrow().iter() {
                                if !consumed.contains(k.as_str()) && !is_internal_key(k.as_str()) {
                                    map.insert(k.clone(), v.clone());
                                }
                            }
                        }
                        Value::Instance(i) => {
                            for (k, v) in i.fields.borrow().iter() {
                                if !consumed.contains(k.as_str()) {
                                    map.insert(k.clone(), v.clone());
                                }
                            }
                        }
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
        // 직전 레이블 문이 남긴 레이블을 이 문(주로 루프)이 가져간다. 루프 아닌 문은 무시.
        let my_label = self.pending_label.take();
        match stmt {
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
                    // __proto__ 링크는 열거 대상 아님(JS 에서 non-enumerable accessor)
                    Value::Obj(m) => m.borrow().keys().filter(|k| !is_internal_key(k.as_str())).cloned().collect(),
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
            Stmt::ForOf { name, iter, body } => {
                let target = self.eval(iter, env)?;
                // 유한한 내장 이터러블(배열/문자열/Set/Map/재료화 반복자)은 재료화해 순회.
                let finite = matches!(&target,
                    Value::Arr(_) | Value::Str(_) | Value::SetVal(_) | Value::MapVal(_))
                    || matches!(&target, Value::Obj(o) if o.borrow().contains_key("__items"));
                if !finite {
                    // 반복자 프로토콜(지연): 제너레이터/사용자 [Symbol.iterator] 이터러블/
                    // 반복자 객체. 한 번에 하나씩 뽑아 무한+break 에도 대응.
                    if let Some(iter_obj) = self.try_get_iterator(&target)? {
                        loop {
                            self.tick()?;
                            let (v, done) = self.gen_iter_next(&iter_obj, Value::Undefined)?;
                            if done {
                                break;
                            }
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
                    return Err(format!("{} 은(는) 반복 가능하지 않음", type_of(&target)));
                }
                let values = self.iterate_to_vec(&target);
                for v in values {
                    self.tick()?;
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
                        PropKey::Computed(ke) => key_of(&self.eval(ke, env)?),
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
                if !is_promise(&v) {
                    return Ok(v); // thenable 아닌 값은 그대로
                }
                self.drain_microtasks();
                if let Value::Obj(o) = &v {
                    let (state, value) = {
                        let m = o.borrow();
                        (
                            match m.get("__state") {
                                Some(Value::Str(s)) => s.clone(),
                                _ => "pending".into(),
                            },
                            m.get("__value").cloned().unwrap_or(Value::Undefined),
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
                        if let Value::Obj(m) = &target {
                            m.borrow_mut().remove(&key);
                        }
                        return Ok(Value::Bool(true));
                    }
                    return Ok(Value::Bool(true));
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
                    let (Some(sc), Some(this)) =
                        (env_get(env, "__superclass__"), env_get(env, "this"))
                    else {
                        return Err("super() 는 파생 클래스 생성자에서만".to_string());
                    };
                    match sc {
                        Value::Class(parent) => {
                            self.run_constructor(&parent, &this, arg_vals)?;
                        }
                        // 클래스가 아닌 생성자(Error/함수 등) 확장: 부모 생성자를 실행해
                        // 나온 객체의 own 프로퍼티를 this 로 옮긴다(= this 를 채운 효과).
                        other => {
                            let produced = self.construct(other, arg_vals)?;
                            for (k, v) in builtins::own_enumerable_entries(&produced) {
                                builtins::set_own_property(&this, k, v);
                            }
                        }
                    }
                    return Ok(Value::Undefined);
                }
                // super.method(...) — 부모 메서드를 현재 this 로 실행
                if let Expr::Member { obj, prop, computed } = &**callee {
                    if matches!(&**obj, Expr::Super) {
                        let key = self.member_key(prop, *computed, env)?;
                        let (Some(sc), Some(this)) =
                            (env_get(env, "__superclass__"), env_get(env, "this"))
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
            Ok(key_of(&v))
        } else if let Expr::Str(s) = prop {
            Ok(s.clone())
        } else {
            Err("잘못된 멤버 접근".to_string())
        }
    }

    // 잘 알려진 심볼(Symbol.iterator 등) — 고정 key 로 배열/제너레이터 반복자와 연결.
    fn well_known_symbol(key: &str, desc: &str) -> Value {
        Value::Symbol(Rc::new(SymbolData { key: key.to_string(), desc: Some(desc.to_string()) }))
    }

    // getComputedStyle(el) → 계산 스타일 뷰(el 이 요소면). 요소 아니면 빈 뷰.
    pub(super) fn get_computed_style(&self, arg: Option<&Value>) -> Value {
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
                Some(Value::Getter(g)) => {
                    return Ok(Some(self.call_value((*g).clone(), Some(this.clone()), vec![])?))
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
                    Some(Value::Getter(g)) => {
                        self.call_value((*g).clone(), Some(recv.clone()), vec![])
                    }
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
                "prototype" => self.error_proto.clone(),
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
                    env_declare(&scope, "__superclass__", sc.clone());
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
            other => Err(format!("{} 은(는) 함수가 아님", to_display(&other))),
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
                        let err = self.thrown.take().unwrap_or(Value::Str(e));
                        let _ = self.call_value(reject, None, vec![err]);
                    } else {
                        return Err(e);
                    }
                }
                return Ok(p);
            }
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
                let mut map = ObjMap::new();
                map.insert("name".to_string(), Value::Str(name.to_string()));
                map.insert(
                    "message".to_string(),
                    Value::Str(args.first().map(to_display).unwrap_or_default()),
                );
                return Ok(Value::Obj(Rc::new(RefCell::new(map))));
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
                return Ok(match ret {
                    v @ (Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) => v,
                    _ => this,
                });
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
                // 클래스 생성자는 항상 new 로 실행 → new.target 은 이 클래스.
                env_declare(&scope, "\u{0}newtarget", Value::Class(cls.clone()));
                // super 참조용: 현재 클래스의 부모를 스코프에 숨겨둠.
                // 부모가 클래스가 아니면(Error/함수 등) 그 생성자 값을 그대로 둔다.
                if let Some(parent) = &cls.parent {
                    env_declare(&scope, "__superclass__", Value::Class(parent.clone()));
                } else if let Some(pc) = &cls.parent_ctor {
                    env_declare(&scope, "__superclass__", pc.clone());
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
            name: def.name.clone().unwrap_or_else(|| "(anonymous)".to_string()),
            parent,
            parent_ctor,
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


    // ToPrimitive: 객체를 원시값으로 (valueOf/toString 호출). prefer_string 이면 toString 먼저.
    // 원시값은 그대로. 사용자 정의 toString/valueOf(BigNumber/moment/커스텀 값형)를 존중.
    fn to_primitive(&mut self, v: Value, prefer_string: bool) -> Value {
        if !matches!(v, Value::Obj(_) | Value::Instance(_) | Value::Arr(_)) {
            return v;
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
                    // Error 및 서브타입: 에러 객체(name/message 보유). 정확한 이름은 subtype.
                    Value::Native(Native::ErrorCtor(name)) => {
                        obj_has("message")
                            && (*name == "Error"
                                || matches!(&l, Value::Obj(m)
                                    if matches!(m.borrow().get("name"), Some(Value::Str(s)) if s == name)))
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
                        // window.x = v 는 전역 변수 x 를 만든다(전역 객체 의미론).
                        let is_window = Rc::ptr_eq(&map, &self.window_obj);
                        {
                            let mut m = map.borrow_mut();
                            // freeze: 변경 금지. seal/preventExtensions: 새 프로퍼티 추가 금지.
                            if m.contains_key("\u{0}@@frozen") {
                                return Ok(());
                            }
                            if !m.contains_key(&key)
                                && (m.contains_key("\u{0}@@sealed") || m.contains_key("\u{0}@@nonext"))
                            {
                                return Ok(());
                            }
                            m.insert(key.clone(), value.clone());
                        }
                        if is_window {
                            env_declare(&self.global, &key, value);
                        }
                        Ok(())
                    }
                    Value::Arr(a) => {
                        // freeze: 모든 변경 금지. seal/preventExtensions: 새 인덱스·프로퍼티 금지.
                        // (isFrozen 이 참인데 실제로는 변경되던 구멍 — 표준대로 막는다)
                        if builtins::arr_is_frozen(&a) {
                            return Ok(());
                        }
                        if let Ok(i) = key.parse::<usize>() {
                            let is_new = i >= a.borrow().len();
                            if is_new && builtins::arr_is_sealed(&a) {
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
                            if a.get_prop(&key).is_none() && builtins::arr_is_sealed(&a) {
                                return Ok(());
                            }
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
                    // 내장(네이티브)에 프로퍼티 얹기 — 폴리필의
                    // `if (!Promise.allSettled) Promise.allSettled = fn` 패턴.
                    Value::Native(n) => {
                        self.native_props.entry(n).or_default().insert(key, value);
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
    fn labeled_break_exits_outer_loop() {
        // i=0: j 0,1,2 → r=3. i=1: j=0 → r=4, j=1 → break outer. 결과 4.
        let src = "let r = 0; \
            outer: for (let i = 0; i < 3; i++) { \
              for (let j = 0; j < 3; j++) { \
                if (i === 1 && j === 1) break outer; \
                r++; \
              } \
            } r";
        assert_eq!(run_num(src), 4.0);
    }

    #[test]
    fn labeled_continue_skips_to_outer() {
        // 각 i 에서 j=0 만 세고 j=1 이면 outer 로 continue → i 당 1씩, 총 3.
        let src = "let r = 0; \
            outer: for (let i = 0; i < 3; i++) { \
              for (let j = 0; j < 3; j++) { \
                if (j === 1) continue outer; \
                r++; \
              } \
            } r";
        assert_eq!(run_num(src), 3.0);
    }

    #[test]
    fn unlabeled_break_continue_still_work() {
        assert_eq!(run_num("let r=0; for(let i=0;i<5;i++){ if(i===3) break; r++; } r"), 3.0);
        assert_eq!(run_num("let r=0; for(let i=0;i<5;i++){ if(i%2===0) continue; r++; } r"), 2.0);
    }

    #[test]
    fn labeled_block_break() {
        // 레이블 붙은 블록에서 break 로 탈출 → 이후 문 건너뜀.
        assert_eq!(run_num("let r=0; block: { r=1; break block; r=99; } r"), 1.0);
    }

    #[test]
    fn class_generator_method() {
        // *gen() 메서드가 반복자를 돌려주고 for-of 로 소비 가능.
        let src = "class C { *gen() { yield 1; yield 2; yield 3; } } \
            let s = 0; for (const x of new C().gen()) s += x; s";
        assert_eq!(run_num(src), 6.0);
    }

    #[test]
    fn class_async_method_returns_thenable() {
        // async 메서드는 파싱/실행되고 then 을 가진 값(Promise)을 돌려준다.
        let src = "class C { async foo() { return 42; } } \
            typeof new C().foo().then";
        assert_eq!(run_str(src), "function");
    }

    #[test]
    fn class_regular_and_static_methods_still_work() {
        assert_eq!(run_num("class C { m() { return 7; } static s() { return 9; } } \
            new C().m() + C.s()"), 16.0);
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
        // 진짜 Symbol.iterator (엔진 제공 원시값). 배열 반복자.
        assert_eq!(
            run_num(
                "var a=[10,20,30]; var it=a[Symbol.iterator](); var s=0,r; \
                 while(!(r=it.next()).done){ s+=r.value; } s"
            ),
            60.0
        );
        // Set 반복자
        assert_eq!(
            run_num(
                "var it=new Set([1,2,3])[Symbol.iterator](); var s=0,r; \
                 while(!(r=it.next()).done){ s+=r.value; } s"
            ),
            6.0
        );
    }

    #[test]
    fn symbol_primitive_type() {
        // typeof 는 'symbol'
        assert!(run_bool("typeof Symbol() === 'symbol'"));
        assert!(run_bool("typeof Symbol.iterator === 'symbol'"));
        // 고유성: 같은 설명이어도 서로 다름
        assert!(run_bool("Symbol('x') !== Symbol('x')"));
        assert!(run_bool("var s=Symbol('a'); s === s"));
        // description
        assert_eq!(run_str("Symbol('hello').description"), "hello");
        // 잘 알려진 심볼은 안정적 동일성
        assert!(run_bool("Symbol.iterator === Symbol.iterator"));
        // Symbol.for 레지스트리: 같은 키면 동일
        assert!(run_bool("Symbol.for('k') === Symbol.for('k')"));
        assert!(run_bool("Symbol.for('k') !== Symbol('k')"));
        assert_eq!(run_str("Symbol.keyFor(Symbol.for('abc'))"), "abc");
        assert!(run_bool("Symbol.keyFor(Symbol('x')) === undefined"));
    }

    #[test]
    fn user_defined_iterable() {
        // obj[Symbol.iterator] = function(){...} — 사용자 정의 이터러블
        let iter = "var range={n:4}; \
            range[Symbol.iterator]=function(){ var i=0; var self=this; \
              return { next:function(){ return i<self.n?{value:i++,done:false}:{value:undefined,done:true}; } }; };";
        // for-of
        assert_eq!(
            run_num(&format!("{iter} var s=0; for(var x of range) s+=x; s")),
            6.0, // 0+1+2+3
        );
        // 스프레드
        assert_eq!(
            run_str(&format!("{iter} [...range].join(',')")),
            "0,1,2,3",
        );
        // Array.from
        assert_eq!(
            run_num(&format!("{iter} Array.from(range).length")),
            4.0,
        );
        // 제너레이터를 반복자로 반환하는 이터러블
        let gi = "var g={}; g[Symbol.iterator]=function*(){ yield 'a'; yield 'b'; yield 'c'; };";
        assert_eq!(
            run_str(&format!("{gi} var out=''; for(var x of g) out+=x; out")),
            "abc",
        );
    }

    #[test]
    fn class_symbol_iterator_method() {
        // class C { [Symbol.iterator]() {...} } — 계산된 메서드 키(사용자 정의 이터러블)
        let src = "class Range { \
              constructor(n){ this.n = n; } \
              [Symbol.iterator]() { var i=0; var n=this.n; \
                return { next: function(){ return i<n ? {value:i++,done:false} : {value:undefined,done:true}; } }; } \
            } \
            var s=0; for(const x of new Range(5)) s+=x; s";
        assert_eq!(run_num(src), 10.0); // 0+1+2+3+4
        // 제너레이터 메서드 *[Symbol.iterator]()
        let src2 = "class Chars { \
              constructor(s){ this.s = s; } \
              *[Symbol.iterator]() { for (var c of this.s) yield c.toUpperCase(); } \
            } \
            var out=''; for(const c of new Chars('abc')) out+=c; out";
        assert_eq!(run_str(src2), "ABC");
        // 스프레드로도 소비 가능
        assert_eq!(run_num("class R { constructor(n){this.n=n;} [Symbol.iterator](){ var i=0,n=this.n; return {next:function(){return i<n?{value:i++,done:false}:{value:0,done:true};}}; } } [...new R(3)].length"), 3.0);
        // 객체 리터럴 계산 메서드 { [Symbol.iterator]() {...} }
        let obj = "var o={ data:[1,2,3], [Symbol.iterator]() { var i=0; var d=this.data; \
            return { next: function(){ return i<d.length?{value:d[i++],done:false}:{value:0,done:true}; } }; } };";
        assert_eq!(run_num(&format!("{obj} var s=0; for(var x of o) s+=x; s")), 6.0);
    }

    #[test]
    fn symbol_as_property_key() {
        // 심볼 키로 저장/조회
        assert_eq!(
            run_num("var s=Symbol('k'); var o={}; o[s]=42; o[s]"),
            42.0
        );
        // 계산된 심볼 키 객체 리터럴
        assert_eq!(
            run_num("var s=Symbol(); var o={[s]: 7, a: 1}; o[s] + o.a"),
            8.0
        );
        // 심볼 키는 열거되지 않는다(for-in/Object.keys/JSON 제외)
        assert_eq!(
            run_str(
                "var s=Symbol('hidden'); var o={a:1, b:2}; o[s]='x'; \
                 var k=[]; for(var p in o) k.push(p); k.join(',')"
            ),
            "a,b"
        );
        assert_eq!(run_str("var s=Symbol(); var o={a:1}; o[s]=9; Object.keys(o).join(',')"), "a");
        assert_eq!(run_str("var s=Symbol(); var o={a:1}; o[s]=9; JSON.stringify(o)"), "{\"a\":1}");
    }

    #[test]
    fn dom_node_type_and_owner_document() {
        let mut dom = crate::html::parse_dom("<div id=\"box\">hi</div>".to_string());
        let box_id = dom.find_by_attr_id("box").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        // document.nodeType === 9 — jQuery 의 setDocument 가 이걸로 문서를 검증한다.
        // 없으면 조기 반환해 jQuery 의 로컬 document 가 undefined 로 남아 전체가 죽었다.
        assert_eq!(to_display(&interp.run("document.nodeType").unwrap()), "9");
        // 요소 nodeType === 1
        assert_eq!(
            to_display(&interp.run("document.getElementById('box').nodeType").unwrap()),
            "1",
        );
        // element.ownerDocument === document (jQuery setDocument 의 `node.ownerDocument || node`)
        assert_eq!(
            to_display(
                &interp
                    .run("document.getElementById('box').ownerDocument === document")
                    .unwrap()
            ),
            "true",
        );
        let _ = box_id;
        // document.implementation.createHTMLDocument — 분리 문서(body/head 보유)
        assert_eq!(
            to_display(
                &interp
                    .run("var d = document.implementation.createHTMLDocument(''); \
                          (d.nodeType) + ',' + (d.body ? 'body' : 'no') + ',' + (d.head ? 'head' : 'no')")
                    .unwrap()
            ),
            "9,body,head",
        );
    }

    #[test]
    fn constructor_found_on_prototype_chain() {
        // jQuery 는 `jQuery.fn.constructor = jQuery` 로 프로토타입에 둔다.
        // own 만 보면 this.constructor() 가 전역 Object 로 떨어져 "함수 아님" 이 됐다.
        assert!(run_bool(
            "function F(){}; F.prototype = { constructor: F, tag: 'proto' }; \
             var o = new F(); o.constructor === F"
        ));
        // 인스턴스가 자기 constructor 를 가지면 그것이 우선
        assert_eq!(
            run_str(
                "function F(){}; F.prototype = { constructor: F }; \
                 var o = new F(); o.constructor = 'own'; o.constructor"
            ),
            "own",
        );
    }

    #[test]
    fn array_methods_are_generic_over_array_likes() {
        // 표준: 배열 메서드는 "length 를 가진 객체"에도 동작한다(generic).
        // jQuery 핵심: `var push = arr.push; push.apply(jqObj, elems)` — 예전엔
        // "push 는 배열 메서드" 로 즉사해 jQuery 전체가 못 떴다.
        let pre = "var arr=[]; var push=arr.push, slice=arr.slice, indexOf=arr.indexOf;";
        // own length 를 가진 array-like
        assert_eq!(
            run_str(&format!("{pre} var al={{length:0}}; push.call(al,'a','b'); al.length + ':' + al[0] + al[1]")),
            "2:ab",
        );
        // length 가 프로토타입에 있는 경우 (jQuery.fn 패턴)
        assert_eq!(
            run_str(&format!(
                "{pre} function JQ(){{}} JQ.prototype={{length:0, push:push}}; \
                 var j=new JQ(); push.apply(j,['x','y','z']); j.length + ':' + j[0] + j[2]"
            )),
            "3:xz",
        );
        // 비변형 메서드도 generic
        assert_eq!(
            run_str(&format!("{pre} var al={{0:'x',1:'y',length:2}}; slice.call(al).join(',')")),
            "x,y",
        );
        assert_eq!(
            run_num(&format!("{pre} var al={{0:'x',1:'y',length:2}}; indexOf.call(al,'y')")),
            1.0,
        );
        // arguments 객체 (가장 흔한 관용구)
        assert_eq!(
            run_str(&format!("{pre} function f(){{ return slice.call(arguments).join('-'); }} f(1,2,3)")),
            "1-2-3",
        );
    }

    #[test]
    fn polyfill_can_assign_props_to_natives() {
        // 폴리필의 `if (!X.method) X.method = fn` 패턴 — 내장에 프로퍼티 저장소가
        // 없어 "function 에 할당할 수 없음" 으로 전부 죽었다.
        // (allSettled 는 이미 내장이라 폴리필 분기를 안 탄다 — 없는 이름으로 검증)
        assert_eq!(
            run_str("if (!Promise.any) { Promise.any = function(){ return 'p'; }; } Promise.any()"),
            "p",
        );
        assert_eq!(run_str("Symbol.observable = 'obs'; Symbol.observable"), "obs");
        assert_eq!(run_num("Date.helper = function(){ return 3; }; Date.helper()"), 3.0);
        // 기존 내장 멤버는 그대로 (덮어쓰지 않은 것)
        assert!(run_bool("Symbol.observable = 'x'; typeof Symbol.iterator === 'symbol'"));
        assert!(run_bool("Date.helper = 1; typeof Date.now === 'function'"));
        // 얹은 값이 내장보다 우선 (명시적 덮어쓰기)
        assert_eq!(run_str("Date.now = function(){ return 'stub'; }; Date.now()"), "stub");
        // 함수의 toString (번들이 fn.toString() 으로 소스 검사)
        assert!(run_bool("typeof (function f(){}).toString === 'function'"));
        assert_eq!(run_str("(function f(a){ return a; }).toString().slice(0,8)"), "function");
    }

    #[test]
    fn array_constructor_and_error_prototype() {
        // Array 는 네임스페이스 객체라 호출 자체가 안 됐다 (new Array(3) / Array(1,2,3)).
        assert_eq!(run_num("new Array(3).length"), 3.0);
        assert_eq!(run_str("Array(1,2,3).join(',')"), "1,2,3");
        assert_eq!(run_num("new Array(1,2).length"), 2.0); // 인자 2개 이상은 항목들
        assert!(run_bool("Array.isArray(new Array(2))"));
        assert!(run_bool("new Array(3)[0] === undefined")); // 길이만 잡힌 빈 슬롯
        // 정적 메서드는 그대로
        assert_eq!(run_num("Array.from([1,2]).length"), 2.0);
        assert_eq!(run_num("Array.of(1,2,3).length"), 3.0);
        // Error.prototype (core-js/번들의 확장·기능 탐지가 참조)
        assert!(run_bool("typeof Error.prototype === 'object'"));
        assert!(run_bool("typeof TypeError.prototype === 'object'"));
        assert_eq!(run_str("Error.name"), "Error");
        assert_eq!(run_str("TypeError.name"), "TypeError");
    }

    #[test]
    fn window_exposes_globals_as_properties() {
        // 전역 이름이 window 프로퍼티로도 보여야 한다 (window 는 전역 객체).
        // `if (window.Promise)` 류 기능 탐지가 실제 코드에 아주 흔한데 전부 실패했었다.
        assert!(run_bool("typeof window.Promise === 'function'"));
        assert!(run_bool("typeof window.Symbol === 'function'"));
        assert!(run_bool("typeof window.Error === 'function'"));
        assert!(run_bool("typeof window.JSON === 'object'"));
        assert!(run_bool("typeof window.Math === 'object'"));
        // 사용자 전역도 보인다
        assert_eq!(run_num("var myGlobal = 42; window.myGlobal"), 42.0);
        // own 프로퍼티(직접 심은 값)는 그대로
        assert_eq!(run_num("window.innerWidth"), 1000.0);
        // 없는 이름은 undefined (에러 아님)
        assert!(run_bool("window.definitelyNotDefined === undefined"));
    }

    #[test]
    fn class_extends_non_class_constructor() {
        // class E extends Error — 커스텀 에러 클래스(아주 흔함). 예전엔 전부 깨졌다.
        assert_eq!(
            run_str(
                "class E extends Error { constructor(m){ super(m); this.name='E'; } } \
                 var e = new E('boom'); e.name + ':' + e.message"
            ),
            "E:boom",
        );
        assert!(run_bool("class E extends Error {} (new E('x')) instanceof E"));
        assert!(run_bool("class E extends Error {} (new E('x')) instanceof Error"));
        // 일반 함수 생성자 확장 — super() 가 this 를 채우고 prototype 메서드도 상속
        assert_eq!(
            run_str(
                "function Base(x){ this.x = x; } \
                 Base.prototype.hi = function(){ return 'hi' + this.x; }; \
                 class D extends Base { constructor(){ super(5); } } \
                 var d = new D(); d.x + '|' + d.hi()"
            ),
            "5|hi5",
        );
        assert!(run_bool(
            "function B(){}; class D extends B {} (new D()) instanceof B"
        ));
        // 파생 클래스 자신의 메서드가 부모 prototype 보다 우선
        assert_eq!(
            run_str(
                "function B(){}; B.prototype.who = function(){ return 'base'; }; \
                 class D extends B { who(){ return 'derived'; } } (new D()).who()"
            ),
            "derived",
        );
        // super.method() — 부모 prototype 메서드 호출
        assert_eq!(
            run_str(
                "function B(){}; B.prototype.who = function(){ return 'base'; }; \
                 class D extends B { who(){ return 'd+' + super.who(); } } (new D()).who()"
            ),
            "d+base",
        );
    }

    #[test]
    fn map_set_date_symbol_prototypes() {
        // 번들/core-js 가 Constructor.prototype.method 를 참조(feature detection, uncurryThis).
        // 예전엔 Map/Set/Date/Symbol 에 .prototype 자체가 없어 여기서 전부 깨졌다.
        assert!(run_bool("typeof Map.prototype === 'object' && typeof Set.prototype === 'object'"));
        assert!(run_bool("typeof Date.prototype === 'object' && typeof Symbol.prototype === 'object'"));
        // WeakMap/WeakSet 도 (Map/Set 으로 근사)
        assert!(run_bool("typeof WeakMap.prototype === 'object'"));
        // 정체성 보존 (같은 객체를 돌려줘야 함)
        assert!(run_bool("Map.prototype === Map.prototype"));
        // uncurryThis 패턴: 프로토타입 메서드를 .call 로 빌려 쓰기
        assert_eq!(
            run_num("var m=new Map(); m.set('a',1); Map.prototype.get.call(m,'a')"),
            1.0,
        );
        assert!(run_bool("var s=new Set([1,2]); Set.prototype.has.call(s, 2)"));
        assert_eq!(
            run_num("var m=new Map([['x',7]]); Map.prototype.size !== undefined ? 0 : Map.prototype.get.call(m,'x')"),
            7.0,
        );
        // Date.prototype.getTime.call
        assert!(run_bool("var d=new Date(0); Date.prototype.getTime.call(d) === 0"));
        // Array.prototype.sort (유일하게 빠져 있던 것)
        assert_eq!(
            run_str("Array.prototype.sort.call([3,1,2]).join(',')"),
            "1,2,3",
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
    fn object_assign_to_object_types() {
        // 기본: 객체 → 객체
        assert_eq!(run_num("var t={a:1}; Object.assign(t,{b:2},{c:3}); t.a+t.b+t.c"), 6.0);
        // 대상이 함수 (번들의 정적 복사 패턴 Object.assign(Fn, {...}))
        assert_eq!(
            run_num("function F(){}; Object.assign(F, {version: 7, x: 1}); F.version + F.x"),
            8.0,
        );
        // 대상이 인스턴스 (Object.assign(this, props))
        assert_eq!(
            run_num("class C { constructor(p){ Object.assign(this, p); } } (new C({v:9})).v"),
            9.0,
        );
        // 소스가 배열/인스턴스/함수여도 own 프로퍼티 복사
        assert_eq!(run_str("var t={}; Object.assign(t, ['x','y']); t[0]+t[1]"), "xy");
        assert_eq!(
            run_num("function S(){}; S.k = 4; var t={}; Object.assign(t, S); t.k"),
            4.0,
        );
        // 반환값은 대상 (체이닝)
        assert_eq!(run_num("Object.assign({}, {n:5}).n"), 5.0);
        // null/undefined 대상만 에러
        assert_eq!(
            run_str("try { Object.assign(null, {}); 'no-throw' } catch(e) { 'threw' }"),
            "threw",
        );
        // frozen 대상은 변경 안 됨
        assert_eq!(
            run_num("var t=Object.freeze({a:1}); Object.assign(t,{a:99,b:2}); t.a"),
            1.0,
        );
    }

    #[test]
    fn symbol_keys_do_not_share_string_keyspace() {
        // 심볼 키는 문자열이 도달할 수 없는 내부 공간(NUL 접두)에 산다.
        // 예전엔 "@@iterator" 라는 그냥 문자열로 이터러블을 위장할 수 있었고,
        // 반대로 "@@" 로 시작하는 정상 문자열 키가 열거에서 조용히 사라졌다.
        // 문자열 "@@iterator" 로는 이터러블이 되지 않는다
        assert_eq!(
            run_num(
                "var o={}; o['@@iterator']=function(){var i=0;return{next:function(){\
                 return i<2?{value:i++,done:false}:{done:true};}};}; [...o].length"
            ),
            0.0,
        );
        // "@@" 로 시작하는 문자열 키는 정상 프로퍼티 (열거·JSON 에 보인다)
        assert_eq!(
            run_str("var o={}; o['@@myprop']=1; o.normal=2; Object.keys(o).join(',')"),
            "@@myprop,normal",
        );
        assert_eq!(
            run_str("var o={}; o['@@x']=1; JSON.stringify(o)"),
            "{\"@@x\":1}",
        );
        // 진짜 심볼 키는 여전히 비열거
        assert_eq!(
            run_str("var s=Symbol('k'); var o={a:1}; o[s]=9; Object.keys(o).join(',')"),
            "a",
        );
        // 진짜 Symbol.iterator 로는 이터러블이 된다
        assert_eq!(
            run_num(
                "var o={}; o[Symbol.iterator]=function(){var i=0;return{next:function(){\
                 return i<2?{value:i++,done:false}:{done:true};}};}; [...o].length"
            ),
            2.0,
        );
    }

    #[test]
    fn builtin_constructors_are_functions() {
        // 표준: 전역 생성자는 함수다. Array/Object 가 네임스페이스 객체라
        // typeof 가 "object" 였다 — 기능 탐지(typeof Object === 'function')가 실패했다.
        assert!(run_bool("typeof Array === 'function'"));
        assert!(run_bool("typeof Object === 'function'"));
        assert!(run_bool("typeof Promise === 'function' && typeof Date === 'function'"));
        // 호출/new 는 그대로
        assert_eq!(run_num("new Array(3).length"), 3.0);
        assert_eq!(run_str("Array(1,2).join(',')"), "1,2");
        assert_eq!(run_num("Object({a:5}).a"), 5.0);
        // 정적 멤버·prototype 도 그대로
        assert_eq!(run_num("Array.from([1,2]).length"), 2.0);
        assert!(run_bool("typeof Object.keys === 'function' && typeof Array.prototype.map === 'function'"));
        // instanceof 유지
        assert!(run_bool("[1,2] instanceof Array && ({}) instanceof Object"));
    }

    #[test]
    fn frozen_arrays_are_actually_frozen() {
        // isFrozen 이 참을 반환하면서 실제로는 변경되던 구멍 — 표준대로 막는다.
        assert_eq!(run_num("var a=[1,2,3]; Object.freeze(a); a[0]=99; a[0]"), 1.0);
        assert_eq!(run_num("var a=[1,2,3]; Object.freeze(a); a.push(4); a.length"), 3.0);
        assert_eq!(run_num("var a=[1,2,3]; Object.freeze(a); a.pop(); a.length"), 3.0);
        assert_eq!(run_str("var a=[3,1,2]; Object.freeze(a); a.sort(); a.join(',')"), "3,1,2");
        assert!(run_bool("var a=[1]; Object.freeze(a); Object.isFrozen(a)"));
        // seal: 기존 인덱스 변경은 되고 새 인덱스 추가는 안 된다
        assert_eq!(run_num("var a=[1,2]; Object.seal(a); a[0]=9; a[0]"), 9.0);
        assert_eq!(run_num("var a=[1,2]; Object.seal(a); a.push(3); a.length"), 2.0);
        // 안 얼린 배열은 그대로
        assert_eq!(run_num("var a=[1]; a[0]=7; a.push(8); a.length + a[0]"), 9.0);
    }

    #[test]
    fn readonly_array_methods_do_not_mutate_array_like() {
        // 읽기 전용 연산이 array-like 대상에 own length/인덱스를 심던 부작용 제거.
        let pre = "var arr=[]; var indexOf=arr.indexOf, slice=arr.slice, join=arr.join;";
        assert_eq!(
            run_num(&format!(
                "{pre} function P(){{}} P.prototype={{length:0}}; var al=new P(); \
                 indexOf.call(al,'x'); slice.call(al); join.call(al); Object.keys(al).length"
            )),
            0.0,
        );
        // 변형 연산은 여전히 되쓴다
        assert_eq!(
            run_num(&format!(
                "{pre} var push=arr.push; var al={{length:0}}; push.call(al,'a'); al.length"
            )),
            1.0,
        );
    }

    #[test]
    fn object_integrity_methods() {
        // freeze 후 isFrozen, 변경 무시
        assert!(run_bool("var o={a:1}; Object.freeze(o); Object.isFrozen(o)"));
        assert_eq!(run_num("var o={a:1}; Object.freeze(o); o.a=99; o.b=5; o.a"), 1.0);
        assert!(run_bool("var o={a:1}; Object.freeze(o); o.b=5; o.b === undefined"));
        // 안 얼린 객체는 isFrozen false, 변경 가능
        assert!(run_bool("!Object.isFrozen({})"));
        assert_eq!(run_num("var o={}; o.x=7; o.x"), 7.0);
        // seal: 기존 값 변경 가능, 새 프로퍼티 추가 금지
        assert_eq!(run_num("var o={a:1}; Object.seal(o); o.a=2; o.b=9; o.a"), 2.0);
        assert!(run_bool("var o={a:1}; Object.seal(o); o.b=9; o.b === undefined"));
        assert!(run_bool("var o={a:1}; Object.seal(o); Object.isSealed(o) && !Object.isFrozen(o)"));
        // isExtensible
        assert!(run_bool("Object.isExtensible({})"));
        assert!(run_bool("var o={}; Object.preventExtensions(o); !Object.isExtensible(o)"));
        // 원시값: frozen/sealed=true, extensible=false
        assert!(run_bool("Object.isFrozen(5) && Object.isSealed('x') && !Object.isExtensible(3)"));
        // freeze 는 인자를 반환 (체이닝)
        assert_eq!(run_num("Object.freeze({a:42}).a"), 42.0);
        // 배열도 정확: 안 얼린 배열은 not frozen
        assert!(run_bool("!Object.isFrozen([1,2,3])"));
        assert!(run_bool("var a=[1]; Object.freeze(a); Object.isFrozen(a)"));
    }

    #[test]
    fn get_computed_style_reads_real_values() {
        let mut dom = crate::html::parse_dom("<div id=\"box\"></div>".to_string());
        let box_id = dom.find_by_attr_id("box").unwrap();
        let mut interp = Interp::new();
        interp.dom = Some(&mut dom as *mut _);
        // 호스트(리빌드)가 채우는 계산 스타일을 흉내낸다.
        let mut m = HashMap::new();
        m.insert("display".to_string(), "flex".to_string());
        m.insert("background-color".to_string(), "rgb(204, 0, 0)".to_string());
        m.insert("font-size".to_string(), "20px".to_string());
        m.insert("width".to_string(), "240px".to_string());
        interp.computed_styles.insert(box_id, m);
        // 카멜케이스 프로퍼티 + getPropertyValue(대시) 둘 다 동작
        let r = interp
            .run(
                "var cs = getComputedStyle(document.getElementById('box')); \
                 cs.display + '|' + cs.backgroundColor + '|' + cs.getPropertyValue('font-size') + '|' + cs.width",
            )
            .unwrap();
        assert_eq!(to_display(&r), "flex|rgb(204, 0, 0)|20px|240px");
        // 없는 프로퍼티는 빈 문자열
        assert_eq!(to_display(&interp.run("getComputedStyle(document.getElementById('box')).color").unwrap()), "");
        // getComputedStyle 은 CSSStyleDeclaration 유형(존재 자체로 크래시 방지)
        assert_eq!(to_display(&interp.run("'' + getComputedStyle(document.getElementById('box'))").unwrap()), "[object CSSStyleDeclaration]");
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
    fn generator_is_lazy_infinite() {
        // 무한 제너레이터를 유한하게 소비 — eager 였다면 여기서 멈춘다.
        assert_eq!(
            run_num(
                "function* nat(){ var i=0; while(true) yield i++; } \
                 var it=nat(); it.next().value + it.next().value + it.next().value"
            ),
            3.0, // 0+1+2
        );
        // for-of + break 로 무한 제너레이터 순회
        assert_eq!(
            run_num(
                "function* nat(){ var i=0; while(true) yield i++; } \
                 var s=0; for(const x of nat()){ if(x>=5) break; s+=x; } s"
            ),
            10.0, // 0+1+2+3+4
        );
    }

    #[test]
    fn generator_lazy_side_effects_interleave() {
        // 본문 부작용이 생성 시점이 아니라 next() 마다 하나씩 일어난다.
        // 생성 직후엔 로그가 비어 있어야 한다(eager 였다면 'ab').
        assert_eq!(
            run_str(
                "var log=[]; function* g(){ log.push('a'); yield 1; log.push('b'); yield 2; } \
                 var it=g(); var before=log.join(''); it.next(); var mid=log.join(''); \
                 it.next(); before + '|' + mid + '|' + log.join('')"
            ),
            "|a|ab",
        );
    }

    #[test]
    fn generator_two_way_next() {
        // next(v) 로 넘긴 값이 yield 식의 값이 된다.
        assert_eq!(
            run_num("function* g(){ var x = yield 1; yield x + 10; } var it=g(); it.next(); it.next(5).value"),
            15.0,
        );
        // 선언 초기화 형태 let x = yield
        assert_eq!(
            run_num("function* g(){ let a = yield 1; let b = yield 2; yield a + b; } \
                     var it=g(); it.next(); it.next(10); it.next(20).value"),
            30.0,
        );
    }

    #[test]
    fn generator_return_value_and_done() {
        // return 값이 { value, done:true } 로 나온다.
        assert!(run_bool(
            "function* g(){ yield 1; return 99; yield 2; } var it=g(); it.next(); \
             var r=it.next(); r.value===99 && r.done===true"
        ));
        // 끝난 뒤 next() 는 { undefined, true }
        assert!(run_bool(
            "function* g(){ yield 1; } var it=g(); it.next(); it.next(); it.next().done"
        ));
    }

    #[test]
    fn generator_yield_star_delegation() {
        // yield* 로 내부 제너레이터/배열을 위임 전개
        assert_eq!(
            run_str("function* inner(){ yield 'a'; yield 'b'; } \
                     function* g(){ yield* inner(); yield* ['c','d']; yield 'e'; } \
                     var out=''; for(const x of g()) out+=x; out"),
            "abcde",
        );
        // yield* 의 값 = 내부 제너레이터의 return 값
        assert_eq!(
            run_num("function* inner(){ yield 1; return 42; } \
                     function* g(){ var r = yield* inner(); yield r; } \
                     var out=[]; for(const x of g()) out.push(x); out[0]*1 + out[1]"),
            43.0, // 1 + 42
        );
    }

    #[test]
    fn generator_try_finally_runs() {
        // 제너레이터 안 try/finally: finally 의 yield 도 산출된다.
        assert_eq!(
            run_str("function* g(){ try { yield 1; yield 2; } finally { yield 9; } } \
                     var out=''; for(const x of g()) out+=x; out"),
            "129",
        );
        // try 안에서 throw → catch 로 이어 실행
        assert_eq!(
            run_str("function* g(){ try { yield 1; throw 'e'; yield 2; } catch(e) { yield e; } } \
                     var out=''; for(const x of g()) out+=x; out"),
            "1e",
        );
    }

    #[test]
    fn generator_early_return_method() {
        // it.return(v) 로 조기 종료 — { v, done:true }, 이후엔 done.
        assert!(run_bool(
            "function* g(){ yield 1; yield 2; yield 3; } var it=g(); it.next(); \
             var r=it.return(77); r.value===77 && r.done===true && it.next().done===true"
        ));
    }

    #[test]
    fn generator_yield_in_expression_positions() {
        // 이항식 내부 yield — 평가 순서 보존(왼쪽 먼저)
        assert_eq!(
            run_num("function* g(){ return 10 + (yield 1); } var it=g(); it.next(); it.next(5).value"),
            15.0,
        );
        // 함수 호출 인자 위치 yield (부작용 함수)
        assert_eq!(
            run_num("function* g(){ return Math.max(yield 1, yield 2); } \
                     var it=g(); it.next(); it.next(3); it.next(8).value"),
            8.0,
        );
        // 메서드 호출 인자 위치 yield — this 보존
        assert_eq!(
            run_str("function* g(){ var a=[]; a.push(yield 1); a.push(yield 2); return a.join(','); } \
                     var it=g(); it.next(); it.next('x'); var r=it.next('y'); r.value"),
            "x,y",
        );
        // 배열 리터럴 안 yield, 순서 보존
        assert_eq!(
            run_str("function* g(){ return [yield 1, yield 2, 3].join('-'); } \
                     var it=g(); it.next(); it.next('a'); it.next('b').value"),
            "a-b-3",
        );
        // 삼항식 분기 안 yield — 선택된 분기만 산출
        assert_eq!(
            run_num("function* g(cond){ return cond ? (yield 1) : (yield 2); } \
                     var it=g(true); it.next(); it.next(42).value"),
            42.0,
        );
    }

    #[test]
    fn generator_yield_in_loop_condition() {
        // while 조건 안 yield: 매 반복 조건 재평가(양방향 next 로 종료 제어)
        // g: 소비자가 0 을 보낼 때까지 받은 값을 합산.
        assert_eq!(
            run_num("function* g(){ var sum=0, v; while((v = yield sum)) { sum += v; } return sum; } \
                     var it=g(); it.next(); it.next(3); it.next(4); it.next(0).value"),
            7.0,
        );
        // do-while 조건 안 yield: 본문 최소 1회 후 조건 검사
        // next(): 본문 n=1, cond=yield 1. next(true): cond 참 → n=2, cond=yield 2.
        // next(false): cond 거짓 → 종료, return n=2.
        assert_eq!(
            run_num("function* g(){ var n=0; do { n++; } while(yield n); return n; } \
                     var it=g(); it.next(); it.next(true); it.next(false).value"),
            2.0,
        );
    }

    #[test]
    fn generator_yield_short_circuit() {
        // && 오른쪽 yield 는 왼쪽이 truthy 일 때만 실행(부작용 로그로 확인)
        assert_eq!(
            run_str("var log=[]; function* g(){ false && (yield log.push('R')); return log.join(''); } \
                     var it=g(); var r=it.next(); r.value"),
            "", // 오른쪽 미실행 → 첫 next 가 바로 done
        );
        // || 왼쪽 falsy → 오른쪽 yield 실행
        assert_eq!(
            run_num("function* g(){ var x = 0 || (yield 1); return x; } \
                     var it=g(); it.next(); it.next(7).value"),
            7.0,
        );
    }

    #[test]
    fn generator_for_of_in_body() {
        // 제너레이터 본문 안 for-of (지연 위임과 동류) — 값을 변환해 산출
        assert_eq!(
            run_num("function* g(){ for(const x of [1,2,3]) yield x*x; } \
                     var s=0; for(const v of g()) s+=v; s"),
            14.0, // 1+4+9
        );
        // 본문 안 switch + yield
        assert_eq!(
            run_str("function* g(n){ switch(n){ case 1: yield 'a'; case 2: yield 'b'; break; default: yield 'z'; } } \
                     var out=''; for(const x of g(1)) out+=x; out"),
            "ab",
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
    fn object_property_insertion_order() {
        // Object.keys / for-in / JSON 은 삽입 순서를 따른다(정렬/무작위 아님).
        assert_eq!(run_str("Object.keys({z:1, a:2, m:3}).join(',')"), "z,a,m");
        assert_eq!(run_str("var o={}; o.b=1; o.a=2; o.c=3; Object.keys(o).join(',')"), "b,a,c");
        assert_eq!(run_str("var s=''; for(var k in {y:1,x:2,w:3}) s+=k; s"), "yxw");
        assert_eq!(run_str("JSON.stringify({one:1, two:2, three:3})"),
            "{\"one\":1,\"two\":2,\"three\":3}");
        // 정수 인덱스 키는 오름차순으로 먼저, 그다음 문자열 키 삽입 순서
        assert_eq!(run_str("var o={}; o.b=1; o[2]=1; o.a=1; o[1]=1; Object.keys(o).join(',')"),
            "1,2,b,a");
        // 재대입은 순서 유지
        assert_eq!(run_str("var o={x:1,y:2}; o.x=9; Object.keys(o).join(',')"), "x,y");
    }

    #[test]
    fn promise_rejection_and_catch() {
        // .catch 가 거부를 잡는다(예전엔 no-op).
        assert_eq!(run_num("await Promise.reject(5).catch(function(e){ return e + 1; })"), 6.0);
        // .then(null, onR) 두 번째 인자로 거부 처리
        assert_eq!(run_num("await Promise.reject(3).then(null, function(e){ return e * 2; })"), 6.0);
        // await 로 거부된 promise → throw (try/catch 로 잡힘)
        assert_eq!(run_num("var r; try { await Promise.reject(9); r=-1; } catch(e){ r=e; } r"), 9.0);
        // .then 핸들러가 throw → 체인 거부 → .catch 로 잡힘
        assert_eq!(
            run_num("await Promise.resolve(1).then(function(){ throw 8; }).catch(function(e){ return e; })"),
            8.0
        );
        // onRejected 없는 .then 뒤로 거부가 통과해 .catch 로
        assert_eq!(
            run_num("await Promise.reject(4).then(function(v){ return v; }).catch(function(e){ return e + 100; })"),
            104.0
        );
        // async 함수 본문 throw → 거부된 promise
        assert_eq!(run_num("await (async function(){ throw 11; })().catch(function(e){ return e; })"), 11.0);
    }

    #[test]
    fn promise_all_rejects_on_any() {
        // Promise.all 은 하나라도 거부되면 그 이유로 거부.
        assert_eq!(
            run_num("var r; try { await Promise.all([Promise.resolve(1), Promise.reject(2)]); r=-1; } catch(e){ r=e; } r"),
            2.0
        );
        // 모두 이행이면 값 배열로 이행
        assert_eq!(run_num("var a = await Promise.all([Promise.resolve(3), Promise.resolve(4)]); a[0]+a[1]"), 7.0);
        // allSettled 는 거부돼도 status/reason 으로 수집(거부 안 함)
        assert_eq!(
            run_str("var a = await Promise.allSettled([Promise.resolve(1), Promise.reject(2)]); a[1].status"),
            "rejected"
        );
    }

    #[test]
    fn delete_removes_property() {
        // delete 가 실제로 own 프로퍼티를 제거한다(예전엔 항상 true 만 반환).
        assert_eq!(run_str("var o={a:1,b:2,c:3}; delete o.b; Object.keys(o).join(',')"), "a,c");
        assert_eq!(run_str("var o={a:1}; delete o.a; typeof o.a"), "undefined");
        assert!(run_bool("var o={a:1}; delete o['a']; !('a' in o)"));
        assert!(run_bool("var o={a:1}; delete o.a === true"));
    }

    #[test]
    fn internal_markers_not_leaked() {
        // Date/Promise 의 엔진 내부 마커가 Object.keys/JSON 에 노출되지 않는다.
        assert_eq!(run_num("Object.keys(new Date(0)).length"), 0.0);
        assert_eq!(run_num("Object.keys(Promise.resolve(1)).length"), 0.0);
        // Date 는 JSON 에서 ISO 문자열(toJSON 규약).
        assert_eq!(run_str("JSON.stringify(new Date(0))"), "\"1970-01-01T00:00:00.000Z\"");
        assert_eq!(run_str("JSON.stringify({d: new Date(0)})"),
            "{\"d\":\"1970-01-01T00:00:00.000Z\"}");
        // 사용자 __ 키(__typename 등)는 보존 — 내부 마커만 필터.
        assert_eq!(run_str("JSON.stringify({__typename:'X', a:1})"),
            "{\"__typename\":\"X\",\"a\":1}");
        assert_eq!(run_str("Object.keys({__typename:'X'}).join(',')"), "__typename");
    }

    #[test]
    fn instance_object_prototype_fallback() {
        // 클래스 인스턴스도 Object.prototype 메서드를 상속(hasOwnProperty/toString/valueOf).
        assert!(run_bool("class P{constructor(x){this.x=x;}} new P(5).hasOwnProperty('x')"));
        assert!(run_bool("class P{constructor(x){this.x=x;}} !new P(5).hasOwnProperty('y')"));
        assert_eq!(run_str("class A{} new A().toString()"), "[object Object]");
        assert!(run_bool("class A{} var a=new A(); a.valueOf() === a"));
        // 클래스가 toString 정의하면 그것 우선
        assert_eq!(run_str("class A{ toString(){ return 'custom'; } } new A().toString()"), "custom");
    }

    #[test]
    fn object_values_entries_fromentries() {
        assert_eq!(run_num("Object.values({a:1,b:2,c:3}).length"), 3.0);
        assert_eq!(run_str("Object.values({a:1,b:2}).join(',')"), "1,2");
        assert_eq!(run_str("Object.entries({x:5})[0].join('=')"), "x=5");
        assert_eq!(run_num("Object.entries({a:1,b:2}).length"), 2.0);
        assert_eq!(run_num("Object.fromEntries([['a',1],['b',2]]).b"), 2.0);
        assert_eq!(run_str("Object.fromEntries(new Map([['k','v']])).k"), "v");
        // 삽입 순서 유지
        assert_eq!(run_str("Object.keys(Object.fromEntries([['z',1],['a',2]])).join(',')"), "z,a");
    }

    #[test]
    fn reflect_namespace() {
        assert_eq!(run_num("Reflect.get({a:5},'a')"), 5.0);
        assert_eq!(run_num("var o={}; Reflect.set(o,'x',9); o.x"), 9.0);
        assert!(run_bool("Reflect.has({a:1},'a')"));
        assert!(run_bool("!Reflect.has({a:1},'b')"));
        assert_eq!(run_num("Reflect.ownKeys({a:1,b:2}).length"), 2.0);
        assert!(run_bool("var o={a:1}; Reflect.deleteProperty(o,'a'); o.a === undefined"));
        assert_eq!(run_num("Reflect.apply(function(a,b){return a+b;},null,[2,3])"), 5.0);
        assert_eq!(run_num("function P(x){this.x=x;} Reflect.construct(P,[7]).x"), 7.0);
    }

    #[test]
    fn more_array_string_methods() {
        assert_eq!(run_num("[1,2,3,4].findLast(function(x){return x<3;})"), 2.0);
        assert_eq!(run_num("[1,2,3,4].findLastIndex(function(x){return x<3;})"), 1.0);
        assert_eq!(run_str("[1,2,3].fill(0).join(',')"), "0,0,0");
        assert_eq!(run_str("[1,2,3,4].fill(9,1,3).join(',')"), "1,9,9,4");
        assert_eq!(run_num("[1,2,3,4].reduceRight(function(a,b){return a-b;})"), -2.0); // 4-3-2-1
        assert_eq!(run_num("'a'.localeCompare('b')"), -1.0);
        assert_eq!(run_num("'b'.localeCompare('b')"), 0.0);
        assert_eq!(run_num("Object.getOwnPropertyNames({a:1,b:2}).length"), 2.0);
    }

    #[test]
    fn structured_clone_deep() {
        // 깊은 복제 — 복제본 변경이 원본에 영향 없음.
        assert_eq!(run_num("var o={a:1,b:{c:2}}; var d=structuredClone(o); d.b.c=9; o.b.c"), 2.0);
        assert_eq!(run_num("var a=[1,[2,3]]; var d=structuredClone(a); d[1][0]=9; a[1][0]"), 2.0);
        assert_eq!(run_num("structuredClone({x:5}).x"), 5.0);
        assert_eq!(run_num("structuredClone([1,2,3]).length"), 3.0);
        assert_eq!(run_num("structuredClone(new Map([['a',7]])).get('a')"), 7.0);
    }

    #[test]
    fn array_string_at_and_flatmap() {
        // .at (음수 인덱스)
        assert_eq!(run_num("[10,20,30].at(-1)"), 30.0);
        assert_eq!(run_num("[10,20,30].at(0)"), 10.0);
        assert!(run_bool("[1,2].at(5) === undefined"));
        assert_eq!(run_str("'abc'.at(-1)"), "c");
        assert_eq!(run_str("'abc'.at(0)"), "a");
        // flatMap
        assert_eq!(run_str("[1,2,3].flatMap(function(x){return [x, x*2];}).join(',')"), "1,2,2,4,3,6");
        assert_eq!(run_num("[1,2].flatMap(function(x){return x;}).length"), 2.0);
    }

    #[test]
    fn regex_named_groups() {
        // (?<name>...) 이름 있는 그룹: 번호 접근 + .groups 이름 접근 + 번호 치환.
        assert_eq!(run_str("'2020-01-15'.match(/(?<y>\\d{4})-(?<m>\\d{2})/)[1]"), "2020");
        assert_eq!(run_str("'2020-01-15'.match(/(?<y>\\d{4})-(?<m>\\d{2})/).groups.y"), "2020");
        assert_eq!(run_str("'2020-01-15'.match(/(?<y>\\d{4})-(?<m>\\d{2})/).groups.m"), "01");
        assert_eq!(
            run_str("'2020-01-15'.replace(/(?<y>\\d{4})-(?<m>\\d{2})-(?<d>\\d{2})/, '$3.$2.$1')"),
            "15.01.2020"
        );
        assert!(run_bool("/(?<year>\\d+)/.test('abc123')"));
        // 이름 그룹 없으면 groups 는 undefined
        assert!(run_bool("'ab'.match(/a/).groups === undefined"));
    }

    #[test]
    fn array_from_and_of() {
        // Array.from: 이터러블/문자열(코드포인트)/Set/array-like/mapFn.
        assert_eq!(run_str("Array.from([1,2,3]).join(',')"), "1,2,3");
        assert_eq!(run_num("Array.from('a😀b').length"), 3.0); // 코드 포인트
        assert_eq!(run_num("Array.from(new Set([1,1,2,2,3])).length"), 3.0);
        assert_eq!(run_str("Array.from({length:3}, function(v,i){return i*2;}).join(',')"), "0,2,4");
        assert_eq!(run_str("Array.from({0:'a',1:'b',length:2}).join(',')"), "a,b");
        // Array.of: 인자 그대로(Array(7)과 달리 [7])
        assert_eq!(run_num("Array.of(7).length"), 1.0);
        assert_eq!(run_str("Array.of(1,2,3).join(',')"), "1,2,3");
    }

    #[test]
    fn string_utf16_semantics() {
        // 문자열은 UTF-16 코드 유닛열: astral 문자는 길이 2.
        assert_eq!(run_num("'😀'.length"), 2.0);
        assert_eq!(run_num("'a😀b'.length"), 4.0);
        assert_eq!(run_num("'café'.length"), 4.0); // é는 BMP → 1
        // charCodeAt=코드 유닛(서로게이트), codePointAt=코드 포인트
        assert_eq!(run_num("'😀'.charCodeAt(0)"), 55357.0); // 0xD83D 하이 서로게이트
        assert_eq!(run_num("'😀'.codePointAt(0)"), 128512.0); // 0x1F600
        // 인덱싱/slice/indexOf 는 UTF-16 유닛 기준
        assert_eq!(run_num("'a😀b'.indexOf('b')"), 3.0);
        assert_eq!(run_str("'a😀b'.slice(0,1)"), "a");
        assert_eq!(run_str("'a😀b'[0]"), "a");
        assert_eq!(run_num("'a😀b'.charCodeAt(1)"), 55357.0);
        // BMP 문자열은 그대로(코드포인트==코드유닛)
        assert_eq!(run_num("'hello'.length"), 5.0);
        assert_eq!(run_num("'hello'.indexOf('llo')"), 2.0);
        assert_eq!(run_str("'hello'.slice(1,3)"), "el");
        // 반복/스프레드는 코드 포인트(astral=1)
        assert_eq!(run_num("[...'😀'].length"), 1.0);
    }

    #[test]
    fn string_conversion_calls_toprimitive() {
        // String(obj) 는 ToString → ToPrimitive(hint string) → toString 호출.
        assert_eq!(run_str("String({toString:function(){return 'Z';}})"), "Z");
        // hint string: toString(상속) 우선 → valueOf 만 있어도 "[object Object]"(스펙 정확)
        assert_eq!(run_str("String({valueOf:function(){return 42;}})"), "[object Object]");
        // 원시값은 그대로
        assert_eq!(run_str("String(5)"), "5");
        assert_eq!(run_str("String(true)"), "true");
        assert_eq!(run_str("String([1,2,3])"), "1,2,3");
    }

    #[test]
    fn regex_vs_division_after_paren() {
        // 제어문 헤더 ')' 뒤는 정규식 허용.
        assert!(run_bool("if(1) /ab/.test('xabx')"));
        assert_eq!(run_num("var r; if(true) r = /x/.test('x') ? 1 : 0; r"), 1.0);
        // 그룹/호출 ')' 뒤는 나눗셈 유지.
        assert_eq!(run_num("var a=6,b=2,c=3; (a)/b/c"), 1.0);
        assert_eq!(run_num("var r=(function(){return 10;})()/2; r"), 5.0);
        // 일반 위치의 정규식도 유지.
        assert_eq!(run_num("'a1b2'.match(/\\d/g).length"), 2.0);
    }

    #[test]
    fn date_parse_and_utc() {
        // Date.parse 는 new Date(문자열).getTime 과 일치, 미파싱은 NaN.
        assert!(run_bool("Date.parse('2020-01-15') === new Date('2020-01-15').getTime()"));
        assert!(run_bool("isNaN(Date.parse('nonsense'))"));
        assert!(run_bool("typeof Date.parse === 'function'"));
        // Date.UTC 는 UTC 컴포넌트의 밀리초.
        assert_eq!(run_num("Date.UTC(1970,0,1)"), 0.0);
        assert!(run_bool("Date.UTC(2020,0,1) === new Date('2020-01-01T00:00:00.000Z').getTime()"));
        assert!(run_bool("typeof Date.UTC === 'function'"));
    }

    #[test]
    fn unicode_identifiers() {
        // 유니코드 식별자(비ASCII 문자·숫자) 인식.
        assert_eq!(run_num("var café = 5; café"), 5.0);
        assert_eq!(run_num("let 你好 = 7; 你好"), 7.0);
        assert_eq!(run_num("const Ω = 3; Ω * 2"), 6.0);
        assert_eq!(run_num("var π=3; π"), 3.0);
    }

    #[test]
    fn native_function_strict_equality() {
        // 같은 내장 함수는 === 로 동일 (기능 탐지/함수 비교에 쓰임).
        assert!(run_bool("Math.round === Math.round"));
        assert!(run_bool("[].push === [].push"));
        assert!(run_bool("JSON.stringify === JSON.stringify"));
        // 다른 내장 함수는 다름
        assert!(run_bool("Math.round !== Math.floor"));
    }

    #[test]
    fn const_reassignment_throws() {
        // const 재대입은 TypeError(잡을 수 있음). 재선언 없는 정상 사용은 유지.
        assert!(Interp::new().run("const x=1; x=2;").is_err());
        assert_eq!(run_num("const x=1; try{ x=2; }catch(e){} x"), 1.0);
        // const 객체의 프로퍼티 변경은 허용(바인딩만 상수)
        assert_eq!(run_num("const o={a:1}; o.a=5; o.a"), 5.0);
        // for-of/for-in const 루프 변수는 반복마다 새 바인딩 → 정상
        assert_eq!(run_num("var s=0; for(const v of [1,2,3]) s+=v; s"), 6.0);
        // let 은 재대입 가능
        assert_eq!(run_num("let y=1; y=2; y"), 2.0);
    }

    #[test]
    fn map_set_same_value_zero_nan() {
        // Set/Map 은 SameValueZero — NaN 은 서로 같다(중복 제거/조회).
        assert_eq!(run_num("var s=new Set(); s.add(NaN); s.add(NaN); s.size"), 1.0);
        assert!(run_bool("var s=new Set(); s.add(NaN); s.has(NaN)"));
        assert_eq!(run_num("var m=new Map(); m.set(NaN,1); m.set(NaN,2); m.size"), 1.0);
        assert_eq!(run_num("var m=new Map(); m.set(NaN,7); m.get(NaN)"), 7.0);
        // 일반 값은 그대로 strict
        assert_eq!(run_num("var s=new Set(); s.add(1); s.add(2); s.add(1); s.size"), 2.0);
    }

    #[test]
    fn number_to_string_ecmascript() {
        let s = |src: &str| run_str(&format!("String({})", src));
        // 지수 임계: n>21 또는 n≤-6 에서 지수 표기, 형식 "de+X"/"de-X"
        assert_eq!(s("1e21"), "1e+21");
        assert_eq!(s("1e-7"), "1e-7");
        assert_eq!(s("0.0000001"), "1e-7");
        // 경계: 1e-6 은 지수 아님(소수)
        assert_eq!(s("0.000001"), "0.000001");
        // 일반 정수/소수
        assert_eq!(s("100"), "100");
        assert_eq!(s("123.45"), "123.45");
        assert_eq!(s("0.5"), "0.5");
        assert_eq!(s("1000000"), "1000000");
        assert_eq!(s("-0"), "0");
        assert_eq!(s("-12.5"), "-12.5");
        // 큰 정수(<1e21)는 전체 자리, ≥1e21 은 지수
        assert_eq!(s("1e20"), "100000000000000000000");
        assert_eq!(s("1.5e21"), "1.5e+21");
    }

    #[test]
    fn json_roundtrip() {
        assert_eq!(run_num("JSON.parse('42')"), 42.0);
        assert_eq!(run_str("JSON.parse('\"hi\\\\n\"')"), "hi\n");
        assert_eq!(run_num("JSON.parse('[1, 2, 3]')[1]"), 2.0);
        assert_eq!(run_num("JSON.parse('{\"a\": {\"b\": 7}}').a.b"), 7.0);
        assert!(run_bool("JSON.parse('true') === true && JSON.parse('null') === null"));
        // 삽입(소스) 순서 보존 — 정렬 아님(ECMAScript OrdinaryOwnPropertyKeys)
        assert_eq!(run_str("JSON.stringify({ b: 2, a: 'x' })"), "{\"b\":2,\"a\":\"x\"}");
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
    fn prototype_linked_not_snapshotted() {
        // 인스턴스 생성 '후'에 prototype 에 추가한 메서드도 보여야 함(링크, 스냅샷 아님).
        let src = "function C(){this.n=10;} var c = new C(); \
            C.prototype.later = function(){ return this.n + 5; }; c.later()";
        assert_eq!(run_num(src), 15.0);
        // 공유 프로토타입: 두 인스턴스가 같은 메서드를 본다
        let src2 = "function P(){} P.prototype.hi = function(){ return 7; }; \
            var a = new P(), b = new P(); a.hi() + b.hi()";
        assert_eq!(run_num(src2), 14.0);
    }

    #[test]
    fn object_create_links_prototype() {
        // Object.create(proto) 는 proto 를 링크 → 상속 메서드 조회, getPrototypeOf 반환.
        assert_eq!(
            run_str("var proto = { greet: function(){ return 'hi'; } }; \
                var o = Object.create(proto); o.greet()"),
            "hi"
        );
        assert!(run_bool("var p = {a:1}; var o = Object.create(p); Object.getPrototypeOf(o) === p"));
        // 생성 후 proto 에 추가한 것도 링크로 보인다
        assert_eq!(
            run_num("var p = {}; var o = Object.create(p); p.late = 9; o.late"),
            9.0
        );
        // 2번째 인자 서술자의 value 반영
        assert_eq!(run_num("var o = Object.create({}, { x: { value: 5 } }); o.x"), 5.0);
        // 링크는 열거 안 됨
        assert_eq!(run_num("var o = Object.create({a:1}); o.b = 2; Object.keys(o).length"), 1.0);
    }

    #[test]
    fn instanceof_function_constructor() {
        assert!(run_bool("function F(){} var x = new F(); x instanceof F"));
        assert!(run_bool("function F(){} function G(){} var x = new F(); !(x instanceof G)"));
    }

    #[test]
    fn proto_link_not_enumerated() {
        // __proto__ 링크는 Object.keys/for-in/JSON 에 노출되지 않는다.
        assert_eq!(run_num("function C(){this.a=1;} var c=new C(); Object.keys(c).length"), 1.0);
        assert_eq!(run_str("function C(){this.a=1;} var c=new C(); Object.keys(c)[0]"), "a");
        assert_eq!(run_str("function C(){this.a=1;} var c=new C(); JSON.stringify(c)"), "{\"a\":1}");
        assert!(run_bool("function C(){this.a=1;} var c=new C(); !c.hasOwnProperty('__proto__')"));
        // for-in 은 own 키만(__proto__ 제외)
        assert_eq!(run_num(
            "function C(){this.a=1;this.b=2;} var c=new C(); var n=0; for(var k in c) n++; n"), 2.0);
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
    fn window_globals_history_top_event() {
        // history 전역 + 메서드(no-op) 존재
        assert!(run_bool("typeof history === 'object' && typeof history.pushState === 'function'"));
        assert_eq!(run_str("history.scrollRestoration"), "auto");
        assert!(run_bool("(history.pushState({}, '', '/x'), true)")); // 크래시 없이 실행
        // top/parent/frames = window (프레임 없음 → 자기 자신)
        assert!(run_bool("top === window && parent === window && window.top === window"));
        // window.Event 접근 가능(프레임워크가 window.Event.prototype 참조)
        assert!(run_bool("typeof window.Event === 'function'"));
    }

    #[test]
    fn json_stringify_throws_on_circular() {
        // 표준: 순환 구조는 TypeError. (깊이 가드로는 분기 순환의 조합 폭발을 못 막는다)
        assert_eq!(
            run_str(
                "var o={a:1}; o.self=o; \
                 try { JSON.stringify(o); 'no-throw' } catch(e) { e.name }"
            ),
            "TypeError",
        );
        // 배열 순환도
        assert_eq!(
            run_str("var a=[1]; a.push(a); try { JSON.stringify(a); 'no-throw' } catch(e) { e.name }"),
            "TypeError",
        );
        // 상호 순환 (a→b→a)
        assert_eq!(
            run_str(
                "var a={},b={}; a.b=b; b.a=a; \
                 try { JSON.stringify(a); 'no-throw' } catch(e) { e.name }"
            ),
            "TypeError",
        );
        // 같은 객체를 두 번 참조(순환 아님)는 정상 직렬화 — 경로 기반이라 오탐 없음
        assert_eq!(
            run_str("var s={n:1}; JSON.stringify({x:s, y:s})"),
            "{\"x\":{\"n\":1},\"y\":{\"n\":1}}",
        );
        // 정상 중첩은 그대로
        assert_eq!(run_str("JSON.stringify({a:[1,{b:2}]})"), "{\"a\":[1,{\"b\":2}]}");
    }

    #[test]
    fn new_target_meta_property() {
        // 일반 호출: new.target 은 undefined
        assert!(run_bool("function f(){ return new.target === undefined; } f()"));
        // new 호출: new.target 은 그 함수 (truthy)
        assert!(run_bool("function f(){ return new.target !== undefined; } (new f()) instanceof f"));
        // 흔한 가드 패턴: new 강제
        assert_eq!(
            run_str(
                "function C(){ if(!new.target) return 'called'; this.ok='new'; } \
                 C() + '|' + (new C()).ok"
            ),
            "called|new",
        );
        // 클래스 생성자 안 new.target 은 클래스
        assert!(run_bool("class A { constructor(){ this.t = new.target === A; } } (new A()).t"));
    }

    #[test]
    fn object_async_generator_method_shorthand() {
        // 제너레이터 메서드 단축 { *gen() {} }
        assert_eq!(
            run_num("var o = { *gen() { yield 1; yield 2; yield 3; } }; var s=0; for(var x of o.gen()) s+=x; s"),
            6.0,
        );
        // async 메서드 단축 { async fetch() {} } — thenable 반환
        assert!(run_bool(
            "var o = { async load() { return 42; } }; typeof o.load().then === 'function'"
        ));
        // async 가 프로퍼티명/메서드명인 경우는 그대로 (오검출 방지)
        assert_eq!(run_num("var o = { async: 5 }; o.async"), 5.0);
        assert_eq!(run_str("var o = { async() { return 'x'; } }; o.async()"), "x");
        // async 제너레이터 메서드 { async *stream() {} } — 파싱만 (호출 안 함)
        assert_eq!(run_num("var o = { async *stream() { yield 1; }, n: 7 }; o.n"), 7.0);
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
