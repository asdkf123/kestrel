// 네이티브(엔진 구현) 함수 식별자와 그 하위 연산 코드들.
// Value::Native(Native::X) 로 값이 되어 JS 에 노출된다.
use super::*;

// canvas 2D 컨텍스트 메서드
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CanvasMethod {
    GetImageData,
    PutImageData,
    CreateImageData,
    CreateLinearGradient,
    CreateRadialGradient,
    AddColorStop,
    CreatePattern,
    Clip,
    BezierCurveTo,
    QuadraticCurveTo,
    Translate,
    Rotate,
    Scale,
    Transform,
    SetTransform,
    ResetTransform,
    Save,
    Restore,
    MeasureText,
    DrawImage,
    Ellipse,
    RoundRect,
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
    // print(x) — 셸 print. test262 async 하네스가 $DONE 결과를 이걸로 낸다.
    Print,
    ArrayPush,
    GetElementById,
    AddEventListener,
    AddGlobalListener,
    FnCall,
    FnApply,
    FnBind,
    FunctionCtor,
    // eval (§19.2.1). 직접 호출은 현재 스코프에서, 간접 호출은 전역 스코프에서 평가한다.
    Eval,
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
    ObjectSetPrototypeOf,
    ObjectDefineProperties,
    ObjectGetOwnPropertySymbols,
    ObjectIsPrototypeOf,
    HasOwnProperty,
    ObjToString,
    // Error.prototype.toString (§20.5.3.4): name + ": " + message (빈 쪽은 생략)
    ErrorToString,
    // Error.isError(v) (ES2025): v 가 [[ErrorData]] 를 가진 객체인가.
    ErrorIsError,
    // Error.prototype.stack 접근자 (Error Stacks 제안, 이제 스펙). 인스턴스 데이터가
    // 아니라 프로토타입 accessor 다. get 은 캡처된 스택 문자열, set 은 own 데이터 설치.
    ErrorStackGet,
    ErrorStackSet,
    ReturnTrue,
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
    MatchMedia,
    ComputedGetProperty,
    // a.compareDocumentPosition(b) — 문서 순서 비트마스크 (jQuery sortOrder)
    CompareDocPosition,
    // document.implementation.createHTMLDocument(title) — 분리된 문서
    CreateHTMLDocument,
    DocQuery(&'static str),
    CreateTextNode,
    // document.createComment (§4.5.1). 코멘트 노드는 DOM 의 일부다.
    CreateComment,
    // CharacterData 메서드 (§4.9): 텍스트/코멘트의 문자 데이터 조작.
    CharData(CharDataOp),
    // Text.splitText(offset) (§4.10): 텍스트 노드를 둘로 쪼갠다.
    SplitText,
    // Attr 노드 접근 (§4.9.2)
    GetAttributeNode,
    // 네임스페이스 조회 (DOM §4.4)
    // 업그레이드된 커스텀 엘리먼트에 그 클래스를 연결한다 (프로토타입 체인용)
    BindElementClass,
    LookupNamespaceURI,
    LookupPrefix,
    IsDefaultNamespace,
    // CSSOM (§CSSOM 6)
    StyleSheets,
    // 플랫폼 객체의 브랜드 문자열 (§WebIDL 의 인터페이스 판별). instanceof 에 쓴다.
    Brand,
    // NodeList/CSSRuleList 의 item(i) — 표준 컬렉션 인터페이스
    ListItem,
    SheetInsertRule,
    SheetDeleteRule,
    RuleStyleGet,
    RuleStyleSet,
    RuleStyleRemove,
    RuleStyleItem,
    SetAttributeNode,
    RemoveAttributeNode,
    // document.defaultView → window
    WindowSelf,
    // Element.setAttributeNS/getAttributeNS/… (§4.9.2). SVG/XML 이 쓴다.
    SetAttributeNS,
    GetAttributeNS,
    RemoveAttributeNS,
    HasAttributeNS,
    InsertBefore,
    StyleSetProperty,
    StyleGetProperty,
    StyleRemoveProperty,
    ClassAdd,
    ClassRemove,
    ClassToggle,
    ClassContains,
    ClassReplace,
    ClassSupports,
    ClassItem,
    ClassValue,
    RegExpCtor,
    RegExpEscape,
    RegexTest,
    RegexExec,
    RegexGet(RegexAccessor),
    // RegExp.prototype[Symbol.match/replace/split/search/matchAll] (§22.2.6.x).
    // this=정규식, args=[문자열, ...]. StrOp 로 위임한다.
    RegexSym(StrOp),
    StringCtor,
    NumberCtor,
    BooleanCtor,
    StrFromCharCode,
    StrRaw,
    NumIsInteger,
    NumIsFinite,
    NumIsNaN,
    NumToFixed,
    ValueToStr, // recv.toString([radix]) → 문자열
    ValueOfSelf, // recv.valueOf() → recv
    // 원시 래퍼 프로토타입의 brand-checked valueOf/toString (§20.3.3/§21.1.3/§22.1.3).
    // thisBooleanValue/thisNumberValue/thisStringValue 를 강제한다 — 잘못된 종류의
    // 수신자에 전달하면 TypeError. generic ValueToStr/ValueOfSelf 는 이 검사가 없어
    // X.prototype.toString() 이 [object Object] 였고 다른 종류 수신자도 조용히 통과했다.
    PrimValueOf(PrimBrand),
    PrimToString(PrimBrand),
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
    RemoveEventListener,
    RemoveGlobalListener,
    DispatchGlobalEvent,
    TakeMutations,
    DynamicImport,
    QueueMicrotask,
    CssSupports,
    ElementAnimate,
    GetAttributeNames,
    HasAttributes,
    ToggleAttribute,
    ReplaceChildren,
    GetAnimations,
    AttachShadow,
    InsertAdjacentHTML,
    InsertAdjacentElement,
    ScrollTo,
    ScrollBy,
    ScrollIntoView,
    Focus,
    Blur,
    ActiveElement,
    LocationHref,
    BigIntCtor,
    BigIntToString,
    DocWrite,
    CookieGet,
    CookieSet,
    GetNamedItem,
    LsKey,
    LsLength,
    HeadersGet,
    HeadersHas,
    ObjectGetOwnPropertyDescriptor,
    LocationAssign,
    LocationReload,
    LocationHrefSet,
    Escape,
    Unescape,
    HistoryPushState,
    HistoryReplaceState,
    DomParserCtor,
    DomParserParse,
    // 이벤트 인터페이스 생성자. 이름을 들고 있어야 프로토타입 체인을 구분한다 —
    // 예전엔 MouseEvent/KeyboardEvent 가 전부 같은 EventCtor 로 "근사" 되어서,
    // new Event('x') instanceof Event 조차 false 였다.
    EventCtor(&'static str),
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
    ObjectGetOwnPropertyNames,
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
    // Response.arrayBuffer() — 원본 바이트 그대로 (텍스트로 거치면 바이너리가 망가진다)
    ResponseArrayBuffer,
    CurrentScript,
    // document.createElementNS(namespace, qualifiedName)
    CreateElementNS,
    // element.click() — 합성 클릭 이벤트를 실제로 디스패치한다 (HTML §8.3.5).
    // 없으면 프로그램적 클릭 한 줄에 스크립트가 죽는다 (탭 전환/다운로드 트리거 등 흔하다).
    ElementClick,
    // WebSocket (RFC 6455) — 진짜 소켓이다. 스텁이면 "연결됐다" 는 거짓말이 된다.
    WebSocketCtor,
    WsSend,
    WsClose,
    // ArrayBuffer 의 0 채우기. JS 루프로 채우면 1MB 버퍼가 100만 번 반복이라
    // 사실상 못 쓴다 (wasm 메모리는 최소 1MB 로 시작하는 게 보통).
    ZeroBytes,
    // WebAssembly. 프렐류드의 얇은 JS 껍데기가 이 네이티브들을 부른다.
    WasmValidate,
    WasmCompile,
    WasmMemPages,
    WasmRegisterMemory,
    WasmGrow,
    WasmInstantiate,
    // 내보내진 wasm 함수 (인스턴스 인덱스, 함수 인덱스)
    WasmCall(u32, u32),
    // 내보내진 전역 (인스턴스, 전역 인덱스) — get/set 접근자
    WasmGlobalGet(u32, u32),
    WasmGlobalSet(u32, u32),
    // 내보내진 테이블 (인스턴스) — table.get(i) / table.length
    WasmTableGet(u32),
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

// 원시 래퍼의 종류 (thisBooleanValue/thisNumberValue/thisStringValue 의 brand).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PrimBrand {
    Boolean,
    Number,
    String,
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
    // setter 계열 (표준 Date 는 가변 객체다 — setter 가 없으면 날짜 계산 코드가 통째로 죽는다)
    SetTime,
    SetFullYear,
    SetMonth,
    SetDate,
    SetHours,
    SetMinutes,
    SetSeconds,
    SetMs,
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
    Entries,
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
    // change-array-by-copy (ES2023): 원본을 건드리지 않고 새 배열을 돌려준다.
    With,
    ToSpliced,
}

// Array.prototype 메서드의 단일 소스 (이름 → 연산). member_get 의 인스턴스 조회와
// array_proto(=Array.prototype 객체) 구성이 **둘 다** 이걸 쓴다. 예전엔 두 곳에
// 목록을 따로 관리해서 어긋났다 — flat/flatMap/at/findLast/fill 등이 인스턴스에서는
// 되는데 Array.prototype.flat.call(...) 로는 undefined 였다.
pub static ARRAY_PROTO_OPS: &[(&str, ArrOp)] = &[
    ("join", ArrOp::Join),
    ("pop", ArrOp::Pop),
    ("indexOf", ArrOp::IndexOf),
    ("slice", ArrOp::Slice),
    ("forEach", ArrOp::ForEach),
    ("map", ArrOp::Map),
    ("filter", ArrOp::Filter),
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
    ("sort", ArrOp::Sort),
    ("flat", ArrOp::Flat),
    ("flatMap", ArrOp::FlatMap),
    ("at", ArrOp::At),
    ("findLast", ArrOp::FindLast),
    ("findLastIndex", ArrOp::FindLastIndex),
    ("fill", ArrOp::Fill),
    ("reduceRight", ArrOp::ReduceRight),
    ("with", ArrOp::With),
    ("toSpliced", ArrOp::ToSpliced),
    ("toString", ArrOp::Join),
];

pub fn array_op_for(name: &str) -> Option<ArrOp> {
    ARRAY_PROTO_OPS.iter().find(|(n, _)| *n == name).map(|(_, op)| *op)
}

// 내장 함수의 (name, length). 표준(§17 ECMAScript Standard Built-in Objects):
// 모든 내장 함수는 name 프로퍼티(메서드 이름)와 length 프로퍼티(형식 매개변수 수)를
// 가진다 — { writable:false, enumerable:false, configurable:true }.
// 예전엔 네이티브 함수의 name 이 항상 ""이고 length 가 0이라 verifyProperty 가
// 전 서브셋에서 깨졌다. DOM/내부 전용 네이티브는 None (test262 대상 아님).
pub fn native_meta(n: &Native) -> Option<(&'static str, u32)> {
    use Native::*;
    Some(match n {
        // ── 배열 메서드 (Array.prototype) ──
        Arr(op) => {
            let name = ARRAY_PROTO_OPS.iter().find(|(_, o)| o == op).map(|(nm, _)| *nm)?;
            let len = match op {
                ArrOp::Slice | ArrOp::Splice | ArrOp::With | ArrOp::ToSpliced => 2,
                ArrOp::Pop | ArrOp::Shift | ArrOp::Reverse | ArrOp::Keys
                | ArrOp::Values | ArrOp::Entries | ArrOp::Flat => 0,
                _ => 1,
            };
            return Some((name, len));
        }
        ArrayPush => ("push", 1),
        // ── 문자열 메서드 (String.prototype) ──
        Str(op) => {
            let (name, len): (&'static str, u32) = match op {
                StrOp::IndexOf => ("indexOf", 1),
                StrOp::LastIndexOf => ("lastIndexOf", 1),
                StrOp::Slice => ("slice", 2),
                StrOp::Split => ("split", 2),
                StrOp::Upper => ("toUpperCase", 0),
                StrOp::Lower => ("toLowerCase", 0),
                StrOp::Trim => ("trim", 0),
                StrOp::Replace => ("replace", 2),
                StrOp::ReplaceAll => ("replaceAll", 2),
                StrOp::CharAt => ("charAt", 1),
                StrOp::Includes => ("includes", 1),
                StrOp::StartsWith => ("startsWith", 1),
                StrOp::EndsWith => ("endsWith", 1),
                StrOp::Match => ("match", 1),
                StrOp::MatchAll => ("matchAll", 1),
                StrOp::Search => ("search", 1),
                StrOp::PadStart => ("padStart", 1),
                StrOp::PadEnd => ("padEnd", 1),
                StrOp::Repeat => ("repeat", 1),
                StrOp::TrimStart => ("trimStart", 0),
                StrOp::TrimEnd => ("trimEnd", 0),
                StrOp::CharCodeAt => ("charCodeAt", 1),
                StrOp::CodePointAt => ("codePointAt", 1),
                StrOp::Concat => ("concat", 1),
                StrOp::At => ("at", 1),
                StrOp::LocaleCompare => ("localeCompare", 1),
            };
            return Some((name, len));
        }
        // ── 생성자 ──
        ArrayCtor => ("Array", 1),
        StringCtor => ("String", 1),
        NumberCtor => ("Number", 1),
        BooleanCtor => ("Boolean", 1),
        ObjectCtor => ("Object", 1),
        SymbolCtor => ("Symbol", 0),
        RegExpCtor => ("RegExp", 2),
        RegExpEscape => ("escape", 1),
        MapCtor => ("Map", 0),
        SetCtor => ("Set", 0),
        DateCtor => ("Date", 7),
        PromiseCtor => ("Promise", 1),
        BigIntCtor => ("BigInt", 1),
        ProxyCtor => ("Proxy", 2),
        FunctionCtor => ("Function", 1),
        // ── 전역 함수 ──
        ParseInt => ("parseInt", 2),
        ParseFloat => ("parseFloat", 1),
        IsNaN => ("isNaN", 1),
        EncodeUri => ("encodeURI", 1),
        EncodeUriComponent => ("encodeURIComponent", 1),
        DecodeUri => ("decodeURI", 1),
        DecodeUriComponent => ("decodeURIComponent", 1),
        Escape => ("escape", 1),
        Unescape => ("unescape", 1),
        Eval => ("eval", 1),
        // ── Function.prototype ──
        FnCall => ("call", 1),
        FnApply => ("apply", 2),
        FnBind => ("bind", 1),
        FnToString => ("toString", 0),
        // ── Object.* 정적 ──
        ObjectKeys => ("keys", 1),
        ObjectValues => ("values", 1),
        ObjectEntries => ("entries", 1),
        ObjectAssign => ("assign", 2),
        ObjectFreeze => ("freeze", 1),
        ObjectCreate => ("create", 2),
        ObjectDefineProperty => ("defineProperty", 3),
        ObjectDefineProperties => ("defineProperties", 2),
        ObjectGetOwnPropertyDescriptor => ("getOwnPropertyDescriptor", 2),
        ObjectGetOwnPropertyNames => ("getOwnPropertyNames", 1),
        ObjectGetPrototypeOf => ("getPrototypeOf", 1),
        ObjectSetPrototypeOf => ("setPrototypeOf", 2),
        ObjectFromEntries => ("fromEntries", 1),
        ObjectIsFrozen => ("isFrozen", 1),
        ObjectIsSealed => ("isSealed", 1),
        ObjectSeal => ("seal", 1),
        ObjectPreventExt => ("preventExtensions", 1),
        ObjectIsExtensible => ("isExtensible", 1),
        ObjectGetOwnPropertySymbols => ("getOwnPropertySymbols", 1),
        ObjectIsPrototypeOf => ("isPrototypeOf", 1),
        // ── Object.prototype ──
        HasOwnProperty => ("hasOwnProperty", 1),
        ObjToString => ("toString", 0),
        ErrorToString => ("toString", 0),
        ErrorIsError => ("isError", 1),
        // 접근자 함수의 표준 이름/길이 (§ get/set 접두). prop-desc 검사가 확인한다.
        ErrorStackGet => ("get stack", 0),
        ErrorStackSet => ("set stack", 1),
        // ── Array.* 정적 ──
        ArrayIsArray => ("isArray", 1),
        ArrayFrom => ("from", 1),
        ArrayOf => ("of", 0),
        // ── String.* 정적 ──
        StrFromCharCode => ("fromCharCode", 1),
        StrRaw => ("raw", 1),
        // ── Number.* 정적 ──
        NumIsInteger => ("isInteger", 1),
        NumIsFinite => ("isFinite", 1),
        NumIsNaN => ("isNaN", 1),
        NumToFixed => ("toFixed", 1),
        // ── Reflect.* ──
        ReflectGet => ("get", 2),
        ReflectSet => ("set", 3),
        ReflectHas => ("has", 2),
        ReflectDeleteProperty => ("deleteProperty", 2),
        ReflectApply => ("apply", 3),
        ReflectConstruct => ("construct", 2),
        // ── JSON.* ──
        JsonParse => ("parse", 2),
        JsonStringify => ("stringify", 3),
        // ── Symbol.* ──
        SymbolFor => ("for", 1),
        SymbolKeyFor => ("keyFor", 1),
        // ── 타이머/기타 전역 ──
        SetTimeout => ("setTimeout", 2),
        SetInterval => ("setInterval", 2),
        ClearTimer => ("clearTimeout", 1),
        QueueMicrotask => ("queueMicrotask", 1),
        StructuredClone => ("structuredClone", 1),
        // ── Promise 정적 ──
        PromiseResolve => ("resolve", 1),
        PromiseReject => ("reject", 1),
        PromiseAll => ("all", 1),
        PromiseRace => ("race", 1),
        PromiseAllSettled => ("allSettled", 1),
        PromiseThen => ("then", 2),
        PromiseCatch => ("catch", 1),
        PromiseFinally => ("finally", 1),
        // ── 원시 래퍼 프로토타입 valueOf/toString ──
        // Number.prototype.toString 만 radix 인자로 length 1, 나머지는 0 (§21.1.3.6).
        PrimValueOf(_) => ("valueOf", 0),
        PrimToString(b) => ("toString", if matches!(b, PrimBrand::Number) { 1 } else { 0 }),
        _ => return None,
    })
}

// 이 내장 함수가 [[Construct]] 를 가진 생성자인가 (§17). Symbol/BigInt 는 호출은
// 되지만 생성자가 아니다(new Symbol() → TypeError). 나머지 정적/프로토타입 메서드,
// 전역 함수(parseInt 등)도 전부 생성자가 아니다.
pub fn native_is_constructor(n: &Native) -> bool {
    use Native::*;
    matches!(
        n,
        ObjectCtor
            | ArrayCtor
            | StringCtor
            | NumberCtor
            | BooleanCtor
            | RegExpCtor
            | MapCtor
            | SetCtor
            | DateCtor
            | PromiseCtor
            | FunctionCtor
            | ProxyCtor
            | ErrorCtor(_)
            | EventCtor(_)
            | UrlCtor
            | XhrCtor
            | DomParserCtor
            | WebSocketCtor
    )
}

// RegExp.prototype 의 접근자 프로퍼티 (§22.2.6). 표준상 flags/source/각 플래그는
// RegExp.prototype 의 getter 다 — 인스턴스의 own 데이터 프로퍼티가 아니다.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RegexAccessor {
    Source,
    Flags,
    Global,
    IgnoreCase,
    Multiline,
    DotAll,
    Unicode,
    Sticky,
    HasIndices,
    UnicodeSets,
}

impl RegexAccessor {
    // 접근자 이름 → 종류, 그리고 flags 문자열에서의 플래그 문자.
    pub fn table() -> &'static [(&'static str, RegexAccessor, Option<char>)] {
        &[
            ("source", RegexAccessor::Source, None),
            ("flags", RegexAccessor::Flags, None),
            ("global", RegexAccessor::Global, Some('g')),
            ("ignoreCase", RegexAccessor::IgnoreCase, Some('i')),
            ("multiline", RegexAccessor::Multiline, Some('m')),
            ("dotAll", RegexAccessor::DotAll, Some('s')),
            ("unicode", RegexAccessor::Unicode, Some('u')),
            ("sticky", RegexAccessor::Sticky, Some('y')),
            ("hasIndices", RegexAccessor::HasIndices, Some('d')),
            ("unicodeSets", RegexAccessor::UnicodeSets, Some('v')),
        ]
    }
}

// CharacterData 의 문자 데이터 연산 (§4.9). 오프셋/길이는 UTF-16 코드 단위 기준.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CharDataOp {
    Substring,
    Append,
    Insert,
    Delete,
    Replace,
}

// 이벤트 인터페이스 상속 표 (DOM §2.2 및 각 명세). (인터페이스, 부모).
// createEvent 의 표(§4.5.1)가 참조하는 이름들을 모두 담는다.
pub static EVENT_IFACES: &[(&str, &str)] = &[
    ("Event", ""),
    ("CustomEvent", "Event"),
    ("UIEvent", "Event"),
    ("MouseEvent", "UIEvent"),
    ("PointerEvent", "MouseEvent"),
    ("DragEvent", "MouseEvent"),
    ("WheelEvent", "MouseEvent"),
    ("KeyboardEvent", "UIEvent"),
    ("FocusEvent", "UIEvent"),
    ("InputEvent", "UIEvent"),
    ("CompositionEvent", "UIEvent"),
    ("TouchEvent", "UIEvent"),
    ("BeforeUnloadEvent", "Event"),
    ("HashChangeEvent", "Event"),
    ("MessageEvent", "Event"),
    ("StorageEvent", "Event"),
    ("DeviceMotionEvent", "Event"),
    ("DeviceOrientationEvent", "Event"),
    ("ProgressEvent", "Event"),
    ("PopStateEvent", "Event"),
    ("AnimationEvent", "Event"),
    ("TransitionEvent", "Event"),
    ("ErrorEvent", "Event"),
    ("SubmitEvent", "Event"),
    ("ClipboardEvent", "Event"),
];
