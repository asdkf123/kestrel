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
    // Object.prototype.propertyIsEnumerable (§20.1.3.4): own + enumerable.
    PropertyIsEnumerable,
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
    SpeciesGet, // get [Symbol.species] — 수신자(this) 반환 (§ 종파생 생성자 접근자)
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
    NumToExponential, // Number.prototype.toExponential (§21.1.3.2)
    NumToPrecision,   // Number.prototype.toPrecision (§21.1.3.5)
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
    // Map.prototype.size / Set.prototype.size 접근자(getter). 인스턴스 own 이 아니라
    // 프로토타입 accessor 다 (§24.1.3.10 / §24.2.3.9). brand 체크 후 원소 수를 돌려준다.
    MapSize,
    SetSize,
    // get Symbol.prototype.description (§20.4.3.2) — 프로토타입 accessor. brand 체크 후
    // 심볼의 [[Description]] 을 돌려준다(없으면 undefined).
    SymbolDescGet,
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
    DomGetRootNode,
    CreateDocumentFragment,
    ProxyCtor,
    // Proxy.revocable(§28.2.1) 과 그것이 돌려주는 revoke 함수.
    ProxyRevocable,
    ProxyRevoke,
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
    JsonRawJson,   // JSON.rawJSON (ES2024)
    JsonIsRawJson, // JSON.isRawJSON (ES2024)
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
    // Reflect 의 나머지 (§28.1). Object.* 와 달리 대상이 객체가 아니면 TypeError 고,
    // 변형 계열은 성공 여부(불리언)를 돌려준다.
    ReflectSetPrototypeOf,
    ReflectGetPrototypeOf,
    ReflectPreventExtensions,
    ReflectIsExtensible,
    ReflectOwnKeys,
    ReflectDefineProperty,
    ReflectGetOwnPropertyDescriptor,
    ObjectKeys,
    // Object.groupBy / Map.groupBy (ES2024) — iterable 을 콜백 키로 그룹화.
    ObjectGroupBy,
    MapGroupBy,
    // Promise.withResolvers (ES2024) — {promise, resolve, reject} 반환.
    PromiseWithResolvers,
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
    // ES2015 추가 (§21.3.2)
    Clz32,
    Expm1,
    Log1p,
    Sinh,
    Cosh,
    Tanh,
    Asinh,
    Acosh,
    Atanh,
    Fround,
    Imul,
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
    // substring(§22.1.3.24): slice 와 달리 음수는 0 으로 클램프, start>end 면 교환.
    Substring,
    // substr(Annex B §B.2.3.1): substr(start, length) — 음수 start 는 len+start.
    Substr,
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
    // UTC 계열 (§21.4.4). 이 엔진은 로컬 == UTC(오프셋 0)라 계산은 로컬과
    // 동일하지만, .name 프로퍼티가 "getUTCHours" 등으로 정확히 나와야 해서
    // 별도 변형으로 둔다(핸들러에서 로컬 변형으로 정규화).
    UtcFullYear,
    UtcMonth,
    UtcDate,
    UtcDay,
    UtcHours,
    UtcMinutes,
    UtcSeconds,
    UtcMs,
    SetUtcFullYear,
    SetUtcMonth,
    SetUtcDate,
    SetUtcHours,
    SetUtcMinutes,
    SetUtcSeconds,
    SetUtcMs,
    ToTimeStr,
    ToUtcStr,
    ToLocaleStr,
    ToLocaleDateStr,
    ToLocaleTimeStr,
    // Annex B (§B.2.4)
    GetYear,
    SetYear,
    // toJSON (§21.4.4.37) — toISOString 과 달리 유효하지 않으면 null, 제네릭.
    ToJson,
    // Date.prototype[Symbol.toPrimitive] (§21.4.4.45)
    ToPrimitive,
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
    Entries,
    // ES2024 집합 연산 (§24.2.4). set-like 인자를 받아 새 Set 또는 불리언을 돌려준다.
    Union,
    Intersection,
    Difference,
    SymmetricDifference,
    IsSubsetOf,
    IsSupersetOf,
    IsDisjointFrom,
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
                StrOp::Substring => ("substring", 2),
                StrOp::Substr => ("substr", 2),
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
        ProxyRevocable => ("revocable", 2),
        ProxyRevoke => ("", 0),
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
        SpeciesGet => ("get [Symbol.species]", 0),
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
        PropertyIsEnumerable => ("propertyIsEnumerable", 1),
        ObjToString => ("toString", 0),
        ErrorToString => ("toString", 0),
        ErrorIsError => ("isError", 1),
        // 접근자 함수의 표준 이름/길이 (§ get/set 접두). prop-desc 검사가 확인한다.
        ErrorStackGet => ("get stack", 0),
        ErrorStackSet => ("set stack", 1),
        MapSize => ("get size", 0),
        SetSize => ("get size", 0),
        SymbolDescGet => ("get description", 0),
        ObjectGroupBy => ("groupBy", 2),
        MapGroupBy => ("groupBy", 2),
        PromiseWithResolvers => ("withResolvers", 0),
        // Set/Map 프로토타입 메서드 이름·길이 (§24). 예전엔 Set(op)/Map(op) arm 이 없어
        // Set.prototype.add.name 이 "" 였다.
        Set(op) => {
            let name = match op {
                SetOp::Add => "add",
                SetOp::Has => "has",
                SetOp::Delete => "delete",
                SetOp::Clear => "clear",
                SetOp::ForEach => "forEach",
                SetOp::Values => "values",
                SetOp::Entries => "entries",
                SetOp::Union => "union",
                SetOp::Intersection => "intersection",
                SetOp::Difference => "difference",
                SetOp::SymmetricDifference => "symmetricDifference",
                SetOp::IsSubsetOf => "isSubsetOf",
                SetOp::IsSupersetOf => "isSupersetOf",
                SetOp::IsDisjointFrom => "isDisjointFrom",
            };
            let len = if matches!(op, SetOp::Clear | SetOp::Values | SetOp::Entries) { 0 } else { 1 };
            return Some((name, len));
        }
        Map(op) => {
            let (name, len): (&str, u32) = match op {
                MapOp::Get => ("get", 1),
                MapOp::Set => ("set", 2),
                MapOp::Has => ("has", 1),
                MapOp::Delete => ("delete", 1),
                MapOp::Clear => ("clear", 0),
                MapOp::ForEach => ("forEach", 1),
                MapOp::Keys => ("keys", 0),
                MapOp::Values => ("values", 0),
                MapOp::Entries => ("entries", 0),
            };
            return Some((name, len));
        }
        // ── Date.prototype / Date.* 정적 (§21.4) ──
        // getTime/valueOf, toISOString/toJSON 은 같은 네이티브로 접혀 있어 대표 이름을 준다.
        DateNow => ("now", 0),
        DateParse => ("parse", 1),
        DateUTC => ("UTC", 7),
        DateMethod(f) => {
            let (name, len): (&str, u32) = match f {
                DateField::Time => ("getTime", 0),
                DateField::FullYear => ("getFullYear", 0),
                DateField::Month => ("getMonth", 0),
                DateField::Date => ("getDate", 0),
                DateField::Day => ("getDay", 0),
                DateField::Hours => ("getHours", 0),
                DateField::Minutes => ("getMinutes", 0),
                DateField::Seconds => ("getSeconds", 0),
                DateField::Ms => ("getMilliseconds", 0),
                DateField::TimezoneOffset => ("getTimezoneOffset", 0),
                DateField::SetTime => ("setTime", 1),
                DateField::SetFullYear => ("setFullYear", 3),
                DateField::SetMonth => ("setMonth", 2),
                DateField::SetDate => ("setDate", 1),
                DateField::SetHours => ("setHours", 4),
                DateField::SetMinutes => ("setMinutes", 3),
                DateField::SetSeconds => ("setSeconds", 2),
                DateField::SetMs => ("setMilliseconds", 1),
                DateField::ToIso => ("toISOString", 0),
                DateField::ToStr => ("toString", 0),
                DateField::ToDateStr => ("toDateString", 0),
                DateField::UtcFullYear => ("getUTCFullYear", 0),
                DateField::UtcMonth => ("getUTCMonth", 0),
                DateField::UtcDate => ("getUTCDate", 0),
                DateField::UtcDay => ("getUTCDay", 0),
                DateField::UtcHours => ("getUTCHours", 0),
                DateField::UtcMinutes => ("getUTCMinutes", 0),
                DateField::UtcSeconds => ("getUTCSeconds", 0),
                DateField::UtcMs => ("getUTCMilliseconds", 0),
                DateField::SetUtcFullYear => ("setUTCFullYear", 3),
                DateField::SetUtcMonth => ("setUTCMonth", 2),
                DateField::SetUtcDate => ("setUTCDate", 1),
                DateField::SetUtcHours => ("setUTCHours", 4),
                DateField::SetUtcMinutes => ("setUTCMinutes", 3),
                DateField::SetUtcSeconds => ("setUTCSeconds", 2),
                DateField::SetUtcMs => ("setUTCMilliseconds", 1),
                DateField::ToTimeStr => ("toTimeString", 0),
                DateField::ToUtcStr => ("toUTCString", 0),
                DateField::ToLocaleStr => ("toLocaleString", 0),
                DateField::ToLocaleDateStr => ("toLocaleDateString", 0),
                DateField::ToLocaleTimeStr => ("toLocaleTimeString", 0),
                DateField::GetYear => ("getYear", 0),
                DateField::SetYear => ("setYear", 1),
                DateField::ToJson => ("toJSON", 1),
                DateField::ToPrimitive => ("[Symbol.toPrimitive]", 1),
            };
            return Some((name, len));
        }
        // ── Math.* (§21.3). max/min/hypot/pow/atan2 는 length 2, random 0, 나머지 1. ──
        Math(op) => {
            let name = match op {
                MathOp::Floor => "floor",
                MathOp::Ceil => "ceil",
                MathOp::Round => "round",
                MathOp::Abs => "abs",
                MathOp::Min => "min",
                MathOp::Max => "max",
                MathOp::Sqrt => "sqrt",
                MathOp::Pow => "pow",
                MathOp::Random => "random",
                MathOp::Trunc => "trunc",
                MathOp::Sign => "sign",
                MathOp::Cbrt => "cbrt",
                MathOp::Log => "log",
                MathOp::Log2 => "log2",
                MathOp::Log10 => "log10",
                MathOp::Exp => "exp",
                MathOp::Sin => "sin",
                MathOp::Cos => "cos",
                MathOp::Tan => "tan",
                MathOp::Asin => "asin",
                MathOp::Acos => "acos",
                MathOp::Atan => "atan",
                MathOp::Atan2 => "atan2",
                MathOp::Hypot => "hypot",
                MathOp::Clz32 => "clz32",
                MathOp::Expm1 => "expm1",
                MathOp::Log1p => "log1p",
                MathOp::Sinh => "sinh",
                MathOp::Cosh => "cosh",
                MathOp::Tanh => "tanh",
                MathOp::Asinh => "asinh",
                MathOp::Acosh => "acosh",
                MathOp::Atanh => "atanh",
                MathOp::Fround => "fround",
                MathOp::Imul => "imul",
            };
            let len = match op {
                MathOp::Random => 0,
                MathOp::Max | MathOp::Min | MathOp::Hypot | MathOp::Pow | MathOp::Atan2
                | MathOp::Imul => 2,
                _ => 1,
            };
            return Some((name, len));
        }
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
        NumToExponential => ("toExponential", 1),
        NumToPrecision => ("toPrecision", 1),
        // ── Reflect.* ──
        ReflectGet => ("get", 2),
        ReflectSet => ("set", 3),
        ReflectHas => ("has", 2),
        ReflectDeleteProperty => ("deleteProperty", 2),
        ReflectApply => ("apply", 3),
        ReflectConstruct => ("construct", 2),
        ReflectSetPrototypeOf => ("setPrototypeOf", 2),
        ReflectGetPrototypeOf => ("getPrototypeOf", 1),
        ReflectPreventExtensions => ("preventExtensions", 1),
        ReflectIsExtensible => ("isExtensible", 1),
        ReflectOwnKeys => ("ownKeys", 1),
        ReflectDefineProperty => ("defineProperty", 3),
        ReflectGetOwnPropertyDescriptor => ("getOwnPropertyDescriptor", 2),
        // ── JSON.* ──
        JsonParse => ("parse", 2),
        JsonStringify => ("stringify", 3),
        JsonRawJson => ("rawJSON", 1),
        JsonIsRawJson => ("isRawJSON", 1),
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
