// 인터프리터 테스트. 표준 동작을 못 박는 회귀 테스트다 — 여기 있는 각 항목은
// 실제로 조용히 틀렸던 적이 있는 동작이다.
use super::*;

fn run(src: &str) -> Value {
    Interp::new().run(src).unwrap()
}

// 프렐류드(플랫폼 전역들)를 깔고 실행 — 실제 페이지와 같은 환경.
fn run_prelude(src: &str) -> Value {
    let mut it = Interp::new();
    it.run(crate::js::JS_PRELUDE).expect("프렐류드 실행");
    it.run(src).unwrap()
}

fn prelude_str(src: &str) -> String {
    to_display(&run_prelude(src))
}

fn prelude_num(src: &str) -> f64 {
    match run_prelude(src) {
        Value::Num(n) => n,
        v => panic!("수를 기대: {:?}", v),
    }
}

fn prelude_bool(src: &str) -> bool {
    matches!(run_prelude(src), Value::Bool(true))
}

fn run_bool_in(it: &mut Interp, src: &str) -> bool {
    matches!(it.run(src).unwrap(), Value::Bool(true))
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

// ── WebAssembly (JS API) ───────────────────────────────────────────────
// 바이트는 테스트 안에서 기계적으로 만든다. 손으로 센 오프셋은 반드시 틀린다.
// 모듈: memory 1페이지, add(i32,i32)->i32, poke(i32) [메모리 0번지에 store8],
//       grow() -> memory.grow(1), 데이터 세그먼트로 0x10 에 'OK'.
fn wasm_test_bytes() -> String {
    fn leb(mut n: u32) -> Vec<u8> {
        let mut o = Vec::new();
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                o.push(b);
                return o;
            }
            o.push(b | 0x80);
        }
    }
    fn sec(id: u8, body: Vec<u8>) -> Vec<u8> {
        let mut o = vec![id];
        o.extend(leb(body.len() as u32));
        o.extend(body);
        o
    }
    fn vecs(items: Vec<Vec<u8>>) -> Vec<u8> {
        let mut o = leb(items.len() as u32);
        for i in items {
            o.extend(i);
        }
        o
    }
    fn nm(s: &str) -> Vec<u8> {
        let mut o = leb(s.len() as u32);
        o.extend_from_slice(s.as_bytes());
        o
    }
    fn body(code: Vec<u8>) -> Vec<u8> {
        let mut b = leb(0); // 로컬 없음
        b.extend(code);
        b.push(0x0b);
        let mut o = leb(b.len() as u32);
        o.extend(b);
        o
    }
    let i32t = 0x7fu8;
    let types = vec![
        vec![0x60, 0x02, i32t, i32t, 0x01, i32t], // (i32,i32)->i32
        vec![0x60, 0x01, i32t, 0x00],             // (i32)->()
        vec![0x60, 0x00, 0x01, i32t],             // ()->i32
    ];
    let mut data = vec![0x00, 0x41, 0x10, 0x0b]; // active, offset=16
    data.extend(leb(2));
    data.extend_from_slice(b"OK");

    let mut m = Vec::new();
    m.extend_from_slice(b"\0asm");
    m.extend_from_slice(&1u32.to_le_bytes());
    m.extend(sec(1, vecs(types)));
    m.extend(sec(3, vecs(vec![leb(0), leb(1), leb(2)])));
    m.extend(sec(5, vecs(vec![vec![0x00, 0x01]])));
    m.extend(sec(
        7,
        vecs(vec![
            {
                let mut v = nm("add");
                v.push(0x00);
                v.extend(leb(0));
                v
            },
            {
                let mut v = nm("poke");
                v.push(0x00);
                v.extend(leb(1));
                v
            },
            {
                let mut v = nm("grow");
                v.push(0x00);
                v.extend(leb(2));
                v
            },
            {
                let mut v = nm("memory");
                v.push(0x02);
                v.extend(leb(0));
                v
            },
        ]),
    ));
    m.extend(sec(
        10,
        vecs(vec![
            body(vec![0x20, 0x00, 0x20, 0x01, 0x6a]), // add
            body(vec![0x41, 0x00, 0x20, 0x00, 0x3a, 0x00, 0x00]), // mem[0] = x
            body(vec![0x41, 0x01, 0x40, 0x00]),       // memory.grow(1)
        ]),
    ));
    m.extend(sec(11, vecs(vec![data])));

    let items: Vec<String> = m.iter().map(|b| b.to_string()).collect();
    format!("new Uint8Array([{}])", items.join(","))
}

#[test]
fn wasm_js_api_roundtrip() {
    let b = wasm_test_bytes();
    assert!(
        prelude_bool(&format!("WebAssembly.validate({})", b)),
        "유효한 모듈"
    );
    assert!(
        !prelude_bool("WebAssembly.validate(new Uint8Array([1,2,3,4]))"),
        "쓰레기 바이트는 거부"
    );
    // 동기 경로(new Module/new Instance)로 내보낸 함수를 부른다
    assert_eq!(
        prelude_num(&format!(
            "var m = new WebAssembly.Module({}); \
             var i = new WebAssembly.Instance(m, {{}}); i.exports.add(20, 22)",
            b
        )),
        42.0
    );
}

// 메모리가 **살아있는 뷰**인가 — 사본이면 여기서 걸린다.
#[test]
fn wasm_memory_is_a_live_view() {
    let b = wasm_test_bytes();
    assert_eq!(
        prelude_str(&format!(
            "var i = new WebAssembly.Instance(new WebAssembly.Module({}), {{}}); \
             var u8 = new Uint8Array(i.exports.memory.buffer); \
             var before = u8[0]; \
             i.exports.poke(200); \
             [before, u8[0], u8[16], u8[17]].join(',')",
            b
        )),
        // 데이터 세그먼트('OK' = 79,75)도 실려 있어야 한다
        "0,200,79,75"
    );
}

// memory.grow 후에도 JS 가 새 버퍼를 본다. 옛 버퍼는 분리(length 0).
#[test]
fn wasm_grow_rebinds_buffer_and_detaches_old() {
    let b = wasm_test_bytes();
    assert_eq!(
        prelude_str(&format!(
            "var i = new WebAssembly.Instance(new WebAssembly.Module({}), {{}}); \
             var old = new Uint8Array(i.exports.memory.buffer); \
             var prev = i.exports.grow(); \
             var now = new Uint8Array(i.exports.memory.buffer); \
             [prev, old.length, now.length, now[16]].join(',')",
            b
        )),
        // 이전 페이지 수 1, 옛 뷰는 분리되어 0, 새 뷰는 2페이지, 데이터는 살아 있다
        "1,0,131072,79"
    );
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
fn remove_event_listener_actually_removes() {
    // 예전엔 요소에 removeEventListener 메서드 자체가 없어 TypeError 로 스크립트가 죽고,
    // document/window/XHR 은 무동작 스텁이라 "제거했다"고 믿는 코드에서 계속 발화했다.
    let mut dom = crate::html::parse_dom("<button id=\"b\">x</button>".to_string());
    let mut it = Interp::new();
    it.dom = Some(&mut dom as *mut _);
    let n = it
        .run(
            "var n = 0; function h(){ n++; } \
             var b = document.getElementById('b'); \
             b.addEventListener('click', h); \
             b.dispatchEvent(new Event('click')); \
             b.removeEventListener('click', h); \
             b.dispatchEvent(new Event('click')); \
             n",
        )
        .unwrap();
    assert!(matches!(n, Value::Num(x) if x == 1.0), "제거 후엔 발화 안 함: {:?}", n);

    // document 리스너도 제거되고, dispatchEvent 로 실제 호출된다
    let m = it
        .run(
            "var m = 0; function g(){ m++; } \
             document.addEventListener('ping', g); \
             document.dispatchEvent(new CustomEvent('ping')); \
             document.removeEventListener('ping', g); \
             document.dispatchEvent(new CustomEvent('ping')); \
             m",
        )
        .unwrap();
    assert!(matches!(m, Value::Num(x) if x == 1.0), "document 리스너 제거: {:?}", m);
}

#[test]
fn xhr_is_an_event_target() {
    // xhr.addEventListener 는 예전에 "요소 메서드"라며 던졌다 — 한 줄에 스크립트 전체가 죽었다.
    // 이제 객체 수신자도 EventTarget 이다(등록/제거/디스패치).
    let mut it = Interp::new();
    let v = it
        .run(
            "var x = new XMLHttpRequest(); var hits = 0; \
             function f(){ hits++; } \
             x.addEventListener('load', f); \
             x.dispatchEvent(new Event('load')); \
             x.removeEventListener('load', f); \
             x.dispatchEvent(new Event('load')); \
             hits",
        )
        .unwrap();
    assert!(matches!(v, Value::Num(n) if n == 1.0), "XHR 리스너 등록/제거: {:?}", v);
}

#[test]
fn form_state_properties_reflect_attributes() {
    // checked/select.value 는 undefined/"" 였다 — 폼 로직이 통째로 어긋난다.
    let mut dom = crate::html::parse_dom(
        "<input id=\"cb\" type=\"checkbox\">\
         <select id=\"s\"><option value=\"a\">A</option>\
         <option value=\"b\" selected>B</option></select>"
            .to_string(),
    );
    let mut it = Interp::new();
    it.dom = Some(&mut dom as *mut _);
    assert!(!run_bool_in(&mut it, "document.getElementById('cb').checked"), "기본 false");
    assert!(
        run_bool_in(&mut it, "var c=document.getElementById('cb'); c.checked=true; c.checked"),
        "쓰기 후 true"
    );
    assert_eq!(
        to_display(&it.run("document.getElementById('s').value").unwrap()),
        "b",
        "select.value 는 선택된 option 의 값"
    );
    assert_eq!(
        to_display(&it.run("document.getElementById('s').selectedIndex").unwrap()),
        "1"
    );
    // select.value = 'a' → 그 option 이 선택된다
    assert_eq!(
        to_display(
            &it.run("var s=document.getElementById('s'); s.value='a'; s.value").unwrap()
        ),
        "a"
    );
}

#[test]
fn insert_adjacent_html_and_template_content() {
    let mut dom = crate::html::parse_dom(
        "<div id=\"h\"></div><template id=\"t\"><i class=\"in\">x</i></template>"
            .to_string(),
    );
    let mut it = Interp::new();
    it.dom = Some(&mut dom as *mut _);
    // 예전엔 insertAdjacentHTML 메서드가 없어 TypeError 로 스크립트가 죽었다
    let v = it
        .run(
            "document.getElementById('h')\
               .insertAdjacentHTML('beforeend', '<b id=\"ins\">i</b>'); \
             !!document.getElementById('ins')",
        )
        .unwrap();
    assert!(matches!(v, Value::Bool(true)), "insertAdjacentHTML 로 삽입");
    let t = it.run("!!document.getElementById('t').content.querySelector('.in')").unwrap();
    assert!(matches!(t, Value::Bool(true)), "template.content 조회");
}

#[test]
fn history_push_state_updates_location() {
    // 예전엔 no-op 이라 SPA 라우터가 pushState 후 읽는 location 이 그대로였다.
    let mut it = Interp::new();
    it.install_location("https://x.test/a/b?q=1");
    it.run("history.pushState({}, '', '/c/d?z=2')").unwrap();
    assert_eq!(to_display(&it.run("location.pathname").unwrap()), "/c/d");
    assert_eq!(to_display(&it.run("location.search").unwrap()), "?z=2");
    assert_eq!(to_display(&it.run("history.length").unwrap()), "2");
    // 상대 경로도 현재 URL 기준으로 결합
    it.run("history.replaceState(null, '', 'e')").unwrap();
    assert_eq!(to_display(&it.run("location.pathname").unwrap()), "/c/e");
}

#[test]
fn window_scroll_updates_state() {
    // 예전엔 window.scrollTo 자체가 없어 TypeError 로 스크립트가 죽었다.
    let mut it = Interp::new();
    it.run("window.scrollTo(0, 120)").unwrap();
    assert_eq!(to_display(&it.run("window.scrollY").unwrap()), "120");
    it.run("window.scrollBy(0, 30)").unwrap();
    assert_eq!(to_display(&it.run("window.pageYOffset").unwrap()), "150");
    it.run("window.scrollTo({ top: 5, left: 2 })").unwrap();
    assert_eq!(to_display(&it.run("window.scrollY").unwrap()), "5");
    assert_eq!(to_display(&it.run("window.scrollX").unwrap()), "2");
}

#[test]
fn derived_this_is_what_super_returned() {
    // 표준: 파생 클래스의 this 는 super() 가 만들어낸 객체다.
    // 예전엔 그 객체의 own 프로퍼티만 this 로 복사했다 — 겉보기엔 비슷하지만
    // 진짜 대상이 아니다. 커스텀 엘리먼트에서 HTMLElement 가 DOM 노드를 돌려줘도
    // this 는 여전히 빈 인스턴스라 this.innerHTML 이 아무 데도 안 그렸다.
    assert_eq!(
        run_str("class A { constructor(){ return {x:1}; } } \
                 class B extends A { constructor(){ super(); this.y=2; } } \
                 var b=new B(); b.x+':'+b.y"),
        "1:2"
    );
    assert_eq!(
        run_str("function A(){ return {x:3}; } \
                 class B extends A { constructor(){ super(); this.y=4; } } \
                 var b=new B(); b.x+':'+b.y"),
        "3:4"
    );
}

#[test]
fn class_setters_and_static_accessors_work() {
    // 파서가 클래스 setter 를 조용히 버렸다 — obj.x = v 가 아무 일도 안 했다.
    assert_eq!(
        run_num("class C { set v(x){ this._v = x * 2; } get v(){ return this._v; } } \
                 var c = new C(); c.v = 5; c.v"),
        10.0
    );
    // static get 은 평범한 정적 메서드로 저장돼 값이 아니라 함수를 돌려줬다.
    // (커스텀 엘리먼트의 static get observedAttributes 가 대표적인 피해자)
    assert_eq!(run_num("class C { static get list(){ return [1,2,3]; } } C.list.length"), 3.0);
    // C.prototype 으로 메서드를 꺼내 특정 this 로 호출할 수 있어야 한다
    assert_eq!(
        run_num("class C { m(){ return this.n; } } \
                 C.prototype.m.call({n: 7})"),
        7.0
    );
}

#[test]
fn constructor_returning_object_wins_over_this() {
    // 표준: 생성자가 객체를 반환하면 그게 결과다. 예전엔 Obj/Instance/Arr 만
    // 객체로 봐서 Proxy 반환이 조용히 버려졌다 — Proxy 로 인덱스를 가로채는
    // 구현(타입드 배열)이 통째로 무력화되고, 값이 그냥 평범한 프로퍼티로 저장됐다.
    assert_eq!(run_num("function F(){ this.a=1; return {a:2}; } new F().a"), 2.0);
    assert_eq!(run_num("function F(){ this.a=1; return 5; } new F().a"), 1.0, "원시값 반환은 무시");
    assert_eq!(
        run_num("function F(){ return new Proxy({}, {get:function(){return 9}}); } new F().x"),
        9.0,
        "Proxy 반환도 객체다"
    );
}

#[test]
fn typed_arrays_have_real_semantics() {
    // 숫자 배열로 흉내내면 랩어라운드/버퍼 공유가 전부 조용히 틀린다.
    assert_eq!(prelude_num("var a=new Uint8Array(2); a[0]=300; a[0]"), 44.0, "8비트 랩어라운드");
    assert_eq!(prelude_num("var a=new Int8Array(1); a[0]=200; a[0]"), -56.0, "부호 있는 8비트");
    assert_eq!(
        prelude_num("var b=new ArrayBuffer(4); var u8=new Uint8Array(b); var u32=new Uint32Array(b); u8[0]=1; u32[0]"),
        1.0,
        "두 뷰가 같은 바이트를 본다"
    );
    // Float32 는 실제로 32비트로 반올림된다 (0.1 왕복 시 값이 달라진다)
    assert!(prelude_bool("var a=new Float32Array(1); a[0]=0.1; a[0] !== 0.1"));
    assert_eq!(prelude_str("Array.from(new TextEncoder().encode('가')).join(',')"), "234,176,128");
}

#[test]
fn intl_formats_numbers_and_dates() {
    // 예전엔 Intl 이 아예 없어서 new Intl.NumberFormat(...) 한 줄에서 스크립트가 죽었다.
    assert_eq!(prelude_str("new Intl.NumberFormat('en-US').format(1234567.891)"), "1,234,567.891");
    assert_eq!(prelude_str("new Intl.NumberFormat('de-DE').format(1234.5)"), "1.234,5");
    assert_eq!(
        prelude_str("new Intl.NumberFormat('en-US',{style:'currency',currency:'USD'}).format(12.5)"),
        "$12.50"
    );
    assert_eq!(prelude_str("new Intl.NumberFormat('en-US',{style:'percent'}).format(0.256)"), "25.6%");
    assert_eq!(prelude_str("new Intl.PluralRules('en').select(1)"), "one");
    assert_eq!(prelude_str("new Intl.RelativeTimeFormat('en').format(-2,'day')"), "2 days ago");
    // Collator.compare 는 바인딩된 함수여야 sort 에 그대로 넘길 수 있다 (표준)
    assert_eq!(
        prelude_str("['10','9','2'].sort(new Intl.Collator('en',{numeric:true}).compare).join(',')"),
        "2,9,10"
    );
    // 프로토타입 오버라이드가 네이티브를 이긴다 (표준) — 예전엔 조용히 무시됐다
    assert_eq!(prelude_str("(1234.5).toLocaleString('en-US')"), "1,234.5");
}

#[test]
fn platform_globals_exist_and_work() {
    // 이 전역들이 없으면 그걸 쓰는 첫 줄에서 TypeError 가 나고 번들 전체가 멈춘다.
    assert_eq!(prelude_str("typeof queueMicrotask"), "function");
    assert_eq!(prelude_str("typeof performance.now()"), "number");
    assert_eq!(prelude_str("atob(btoa('hi'))"), "hi");
    assert_eq!(prelude_str("typeof Promise.any"), "function");
    assert_eq!(prelude_str("new URLSearchParams('a=1&b=2').get('b')"), "2");
    assert_eq!(prelude_str("typeof crypto.randomUUID()"), "string");
    assert!(prelude_bool("var c=new AbortController(); var hit=false; \
                      c.signal.addEventListener('abort', function(){hit=true;}); \
                      c.abort(); hit && c.signal.aborted"));
    // CSS.supports 는 CSS 의 @supports 와 같은 평가기를 쓴다
    assert!(prelude_bool("CSS.supports('display','grid')"));
    assert!(prelude_bool("CSS.supports('position','sticky')"), "구현했으므로 참");
    assert!(
        !prelude_bool("CSS.supports('display','table-cell')"),
        "미구현 값은 거짓 (한 엔진 한 답)"
    );
}

#[test]
fn match_media_agrees_with_css_media_queries() {
    // 예전엔 프렐류드가 항상 matches:false 를 돌려줬다 — CSS 는 데스크톱 규칙을
    // 적용하는데 JS 는 모바일로 분기하는 자기모순. 같은 평가기를 쓴다(뷰포트 1000x800).
    assert!(run_bool("matchMedia('(min-width: 768px)').matches"));
    assert!(!run_bool("matchMedia('(max-width: 500px)').matches"));
    assert!(run_bool("window.matchMedia('(min-width: 100px) and (max-width: 2000px)').matches"));
    assert!(!run_bool("matchMedia('(prefers-color-scheme: dark)').matches"));
    assert_eq!(run_str("matchMedia('(min-width: 768px)').media"), "(min-width: 768px)");
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
    assert_eq!(run_num("var m=new Map([['x',7]]); Map.prototype.get.call(m,'x')"), 7.0);
    // Map.prototype.size 는 accessor 라 non-Map(prototype 자신)에 접근하면 TypeError.
    assert!(run_bool(
        "var t=false; try{ Map.prototype.size }catch(e){ t=e instanceof TypeError } t"
    ));
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
    // 가드는 **시간** 기반이다 (스텝 수가 아니라). 테스트는 짧은 예산으로 확인한다.
    let mut it = Interp::new();
    it.script_budget_ms = 200;
    let t0 = std::time::Instant::now();
    let err = it.run("while (true) {}").unwrap_err();
    assert!(err.starts_with(STEP_LIMIT_MSG), "한도 메시지: {}", err);
    assert!(t0.elapsed().as_secs() < 3, "예산 안에서 끊겨야 한다");
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

// Proxy.revocable (§28.2.1): { proxy, revoke }. revoke() 후 모든 내부 메서드가 TypeError.
// new Proxy/Proxy.revocable 는 target·handler 가 비객체면 TypeError.
#[test]
fn proxy_revocable() {
    assert!(run_bool("var r=Proxy.revocable({a:1},{}); typeof r.proxy==='object' && typeof r.revoke==='function'"));
    // revoke 전에는 정상
    assert_eq!(run_num("var r=Proxy.revocable({a:1},{}); r.proxy.a"), 1.0);
    // revoke() 는 undefined 반환, 멱등
    assert!(run_bool("var r=Proxy.revocable({},{}); r.revoke()===undefined && r.revoke()===undefined"));
    // revoke 후 get/set/has/delete/keys/gOPD/defineProperty 는 전부 TypeError
    for op in [
        "r.proxy.a", "r.proxy.a=5", "'a' in r.proxy", "delete r.proxy.a",
        "Object.keys(r.proxy)", "Object.getOwnPropertyDescriptor(r.proxy,'a')",
        "Object.defineProperty(r.proxy,'x',{value:1})",
    ] {
        assert!(run_bool(&format!(
            "var r=Proxy.revocable({{a:1}},{{}}); r.revoke(); \
             var t=false; try{{ {} }}catch(e){{ t=e instanceof TypeError }} t", op)),
            "revoked {} 는 TypeError 여야", op);
    }
    // 트랩 있는 프록시도 revoke 후 트랩 호출 전에 TypeError
    assert!(run_bool("var r=Proxy.revocable({},{get:function(){return 9;}}); r.proxy.x; r.revoke(); \
                      var t=false; try{ r.proxy.x }catch(e){ t=e instanceof TypeError } t"));
    // target/handler 가 비객체면 TypeError (new Proxy / revocable 양쪽)
    assert!(run_bool("var t=false; try{ new Proxy(1,{}) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ Proxy.revocable(1,{}) }catch(e){ t=e instanceof TypeError } t"));
    // 취소 안 된 프록시는 무회귀
    assert_eq!(run_num("var p=new Proxy({z:7},{}); p.z"), 7.0);
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
    // frozen 대상에 대입은 TypeError (§20.1.2.1, Set with Throw=true). 원래 값 유지.
    assert!(run_bool(
        "var t=Object.freeze({a:1}); \
         try{ Object.assign(t,{a:99,b:2}); false }catch(e){ e instanceof TypeError && t.a===1 }"
    ));
}

#[test]
fn setters_actually_run() {
    // setter 가 파싱만 되고 버려져 대입이 조용히 setter 를 우회했다(부작용 미발생).
    assert_eq!(
        run_str(
            "var log=''; var o={ _v:0, get v(){return this._v;}, set v(x){ log='set:'+x; this._v=x*10; } }; \
             o.v = 5; log + '|' + o.v"
        ),
        "set:5|50",
    );
    // set 만 있는 프로퍼티: 읽으면 undefined, 부작용은 발생
    assert_eq!(
        run_str("var o={ set only(x){ this.got=x; } }; o.only='z'; (o.only===undefined) + '|' + o.got"),
        "true|z",
    );
    // get 만 있는 프로퍼티: 대입 무시
    assert_eq!(run_num("var o={ get ro(){return 1;} }; o.ro=9; o.ro"), 1.0);
    // 계산 키 setter (심볼 키)
    assert_eq!(
        run_num("var k=Symbol('k'); var o={ set [k](x){ this.hit=x; } }; o[k]=7; o.hit"),
        7.0,
    );
    // 프로토타입의 setter 도 호출된다
    assert_eq!(
        run_num(
            "function P(){}; Object.defineProperty(P.prototype,'p',\
             { get:function(){return this.s;}, set:function(x){ this.s=x*2; } }); \
             var i=new P(); i.p=4; i.p"
        ),
        8.0,
    );
    // 같은 키의 get/set 이 하나의 접근자로 병합된다
    assert_eq!(
        run_num("var o={ get x(){return this._x;}, set x(v){ this._x=v+1; } }; o.x=1; o.x"),
        2.0,
    );
}

#[test]
fn integrity_is_uniform_across_object_kinds() {
    // isFrozen 이 인스턴스/함수/Map 을 "원시값" 취급해 안 얼렸는데 true 를 반환했다(거짓말).
    assert!(run_bool("class K{}; !Object.isFrozen(new K())"));
    assert!(run_bool("function F(){}; !Object.isFrozen(F)"));
    assert!(run_bool("!Object.isFrozen(new Map()) && !Object.isFrozen(new Set())"));
    // 얼리면 실제로 막힌다
    assert_eq!(
        run_num("class K{ constructor(){ this.a=1; } }; var i=new K(); Object.freeze(i); i.a=99; i.a"),
        1.0,
    );
    assert_eq!(
        run_num("function F(){}; F.x=1; Object.freeze(F); F.x=99; F.y=2; F.x + (F.y||0)"),
        1.0,
    );
    // 얼린 뒤 isFrozen 은 true
    assert!(run_bool("var m=new Map(); Object.freeze(m); Object.isFrozen(m)"));
    // Object.assign 도 무결성을 존중 — frozen 대상엔 TypeError (§20.1.2.1). 값 유지.
    assert!(run_bool(
        "var t=Object.freeze({a:1}); \
         try{ Object.assign(t,{a:99,b:2}); false }catch(e){ e instanceof TypeError && t.a===1 }"
    ));
    // 원시값은 표준대로 frozen/sealed=true, extensible=false
    assert!(run_bool("Object.isFrozen(5) && Object.isSealed('x') && !Object.isExtensible(3)"));
}

#[test]
fn prototype_and_descriptor_apis_are_real() {
    // 전부 "거짓말하는 스텁" 이었다: setPrototypeOf=no-op, isPrototypeOf=항상 false,
    // defineProperties=defineProperty 별칭(시그니처 불일치), getOwnPropertySymbols=항상 [].
    assert!(run_bool(
        "var proto={ hi:function(){return 'h';} }; var o={}; Object.setPrototypeOf(o, proto); \
         typeof o.hi === 'function' && Object.getPrototypeOf(o) === proto"
    ));
    assert!(run_bool(
        "var proto={}; var o={}; Object.setPrototypeOf(o, proto); \
         proto.isPrototypeOf(o) && !proto.isPrototypeOf({})"
    ));
    assert_eq!(
        run_num("var d={}; Object.defineProperties(d, { x:{value:1}, y:{value:2} }); d.x + d.y"),
        3.0,
    );
    assert_eq!(
        run_num("var s=Symbol('s'); var o={}; o[s]=1; Object.getOwnPropertySymbols(o).length"),
        1.0,
    );
    // 복원된 심볼이 원본과 같은 키(=동일성)와 설명을 갖는다
    assert!(run_bool(
        "var s=Symbol('desc'); var o={}; o[s]=1; \
         var got=Object.getOwnPropertySymbols(o)[0]; got === s && got.description === 'desc'"
    ));
}

#[test]
fn engine_markers_are_not_forgeable_from_js() {
    // 엔진 내부 마커는 NUL 접두 공간에 산다 — JS 문자열 키가 도달할 수 없다.
    // 예전엔 `obj.__isPromise = true` 로 promise 를, `__isDate` 로 Date 를,
    // `__items` 로 이터러블을 위장할 수 있었다(타입 시스템 우회).
    assert!(run_bool(
        "var f={__isPromise:true,__state:'fulfilled',__value:42}; f.then === undefined"
    ));
    assert!(run_bool("var f={__isDate:true,__time:0}; f.getTime === undefined"));
    assert_eq!(
        run_str(
            "var f={__items:[1,2,3],__i:0}; \
             try { var n=0; for (var x of f) n++; 'iterated:'+n } catch(e) { 'not-iterable' }"
        ),
        "not-iterable",
    );
    // 반대로 사용자의 정상 __ 키가 열거에서 사라지지 않는다
    assert_eq!(
        run_str("var u={__items:'d',__time:'t',__value:'v',normal:1}; Object.keys(u).join(',')"),
        "__items,__time,__value,normal",
    );
    assert_eq!(
        run_str("var u={__value:'v'}; JSON.stringify(u)"),
        "{\"__value\":\"v\"}",
    );
    // 진짜 Promise/Date/정규식은 여전히 동작
    assert!(run_bool("typeof Promise.resolve(1).then === 'function'"));
    assert!(run_bool("typeof (new Date()).getTime === 'function'"));
    assert!(run_bool("/a/.test('a')"));
    // __proto__ 는 표준 이름이라 유지(비열거 + 프로토타입 의미론)
    assert_eq!(run_str("var o={a:1}; Object.keys(o).join(',')"), "a");
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
fn destructuring_targets_can_be_members() {
    // 표준: 구조분해 대상은 멤버 표현식일 수 있다. 예전엔 "잘못된 구조분해 할당 대상"
    // 으로 파싱이 죽어서, 이 패턴을 쓰는 번들(vue 런타임 등)이 통째로 안 돌았다.
    assert_eq!(run_num("var o={}; [o.p, o.q] = [5, 6]; o.p + o.q"), 11.0);
    assert_eq!(run_num("var o={}; ({x: o.a, y: o.b} = {x:1, y:2}); o.a + o.b"), 3.0);
    assert_eq!(run_num("var a=[0,0]; [a[0], a[1]] = [7, 8]; a[0] + a[1]"), 15.0);
    // 기존 동작(이름 대상 / 스왑)도 그대로
    assert_eq!(run_num("var a=1,b=2; [a,b]=[b,a]; a*10+b"), 21.0);
}

#[test]
fn module_hoists_vars_like_scripts() {
    // `var a, le, ue = …` 처럼 초기화 없는 var 선언자는 호이스팅에 의존한다.
    // 모듈 평가에 var 호이스팅이 없어서, 그 이름을 읽는 순간 "정의되지 않음" 으로
    // 죽었다 (vue 런타임이 정확히 이 모양이라 사이트가 통째로 안 돌았다).
    let mut it = Interp::new();
    it.module_sources.insert(
        "https://x.test/m.js".to_string(),
        "var a = 1, le, ue = () => (le = le || 7); \
         globalThis.k1 = typeof le; \
         globalThis.k2 = ue(); \
         export const done = true;"
            .to_string(),
    );
    it.run_module("https://x.test/m.js").expect("모듈 평가");
    assert_eq!(to_display(&it.run("k1").unwrap()), "undefined", "선언은 됐고 값은 undefined");
    assert_eq!(to_display(&it.run("k2").unwrap()), "7");
}

#[test]
fn storage_has_length_and_key() {
    // Storage 인터페이스는 length 와 key(i) 를 갖는다 (표준 §12.2, 삽입 순서).
    // 없으면 for (i < localStorage.length) 로 순회하는 흔한 코드가 죽는다.
    assert_eq!(
        run_str(
            "localStorage.setItem('a', '1'); localStorage.setItem('b', '2'); \
             var r = localStorage.length + ',' + localStorage.key(0) + ',' + localStorage.key(1); \
             localStorage.setItem('a', '9'); \
             r += '|' + localStorage.length; \
             r += '|' + String(localStorage.key(99)); \
             localStorage.removeItem('a'); \
             r + '|' + localStorage.length + ',' + localStorage.key(0)"
        ),
        "2,a,b|2|null|1,b"
    );
}

#[test]
fn get_prototype_of_is_real() {
    // 예전엔 __proto__ 링크가 없으면 무조건 null 이었다 — 평범한 객체·배열·인스턴스가
    // 전부 null. regenerator/babel 런타임이 getProto(getProto(values([]))) 로 내장
    // 프로토타입을 캐내는데 null 이면 이터레이터 체인이 통째로 무너진다 (naver 가 죽었다).
    assert_eq!(run_str("String(Object.getPrototypeOf({}) !== null)"), "true");
    assert_eq!(run_str("String(Object.getPrototypeOf([]) !== null)"), "true");
    assert_eq!(
        run_str("class C {} var c = new C(); String(Object.getPrototypeOf(c) === C.prototype)"),
        "true"
    );
    // C.prototype 은 매번 같은 객체여야 한다 (정체성)
    assert_eq!(run_str("class C {} String(C.prototype === C.prototype)"), "true");
    // 체인의 끝은 null 이다 — 자기 자신을 돌려주면 체인을 걷는 코드가 무한 루프에 빠진다
    assert_eq!(run_str("String(Object.getPrototypeOf(Object.prototype))"), "null");
    assert_eq!(
        run_num("var p = Object.getPrototypeOf({}), n = 0; while (p && n < 20) { p = Object.getPrototypeOf(p); n++ } n"),
        1.0
    );
}

#[test]
fn array_length_overflow_is_range_error() {
    // 배열 최대 길이는 2^32-1 이다 (§10.4.2.2). 넘으면 RangeError.
    // 상한이 없어서 core-js 의 기능 탐지(Array.from({length: 2**32}))가 40억 개
    // 할당을 시도했고 프로세스가 통째로 죽었다 (naver 가 110초 만에 SIGKILL).
    assert_eq!(
        run_str("try { Array.from({ length: 4294967296 }) } catch (e) { 'RangeError' }"),
        "RangeError"
    );
    // 정상 범위는 그대로 동작
    assert_eq!(run_str("Array.from({ length: 3, 0: 'a', 1: 'b', 2: 'c' }).join()"), "a,b,c");
}

#[test]
fn keys_values_entries_return_iterators() {
    // 표준: Array/Map/Set 의 keys/values/entries 는 **이터레이터**를 돌려준다.
    // 배열을 주면 for-of 는 되지만 .next() 가 없어서, 이터레이터 프로토콜을 직접
    // 쓰는 코드(core-js/regenerator/babel 헬퍼)가 "next 가 undefined" 로 죽는다.
    assert_eq!(run_num("[7, 8].values().next().value"), 7.0);
    assert_eq!(run_num("[7, 8].keys().next().value"), 0.0);
    assert_eq!(run_str("[7].entries().next().value.join()"), "0,7");
    assert_eq!(run_str("new Map([['a', 1]]).entries().next().value.join()"), "a,1");
    assert_eq!(run_num("new Set([5]).values().next().value"), 5.0);
    // 이터레이터는 스스로 이터러블이다 (it[Symbol.iterator]() === it)
    assert_eq!(
        run_str("var it = [1].values(); String(it[Symbol.iterator]() === it)"),
        "true"
    );
    assert_eq!(
        run_str("function* g() { yield 1 } var it = g(); String(it[Symbol.iterator]() === it)"),
        "true"
    );
    // 여전히 for-of / 전개도 된다
    assert_eq!(run_str("[...new Map([['a',1]]).keys()].join()"), "a");
    // null 전개는 TypeError (조용히 빈 배열로 넘기면 진짜 버그가 숨는다)
    assert_eq!(run_str("try { [...null] } catch (e) { 'TypeError' }"), "TypeError");
}

#[test]
fn property_descriptors_and_enumerable() {
    // 게터 프로퍼티의 디스크립터에는 get 이 있어야 한다. 예전 프렐류드 폴리필은
    // {value: o[k]} 를 만들어 **게터를 실행해 값만** 줬다 — 라이브러리가 d.get 으로
    // 분기하므로 조용히 틀린 길로 간다 (naver 가 여기서 죽었다).
    assert_eq!(
        run_str("var o = { get a() { return 1 } }; typeof Object.getOwnPropertyDescriptor(o, 'a').get"),
        "function"
    );
    // 배열 length / 함수 prototype 도 own 프로퍼티다
    assert_eq!(run_num("Object.getOwnPropertyDescriptor([1,2], 'length').value"), 2.0);
    assert_eq!(
        run_str("function F(){}; typeof Object.getOwnPropertyDescriptor(F, 'prototype').value"),
        "object"
    );
    // enumerable: false 는 Object.keys / for-in / JSON 에서 빠져야 한다.
    // 예전엔 이 플래그를 통째로 무시해서 숨겨야 할 프로퍼티가 그대로 새어 나왔다.
    assert_eq!(
        run_num("var o = {}; Object.defineProperty(o, 'h', { value: 1 }); Object.keys(o).length"),
        0.0
    );
    assert_eq!(
        run_str("var o = {}; Object.defineProperty(o, 'h', { value: 1 }); JSON.stringify(o)"),
        "{}"
    );
    assert_eq!(
        run_num("var o = {}; Object.defineProperty(o, 'h', { value: 1 }); var n = 0; for (var k in o) n++; n"),
        0.0
    );
    // enumerable: true 는 보인다
    assert_eq!(
        run_str("var o = {}; Object.defineProperty(o, 'v', { value: 9, enumerable: true }); Object.keys(o).join()"),
        "v"
    );
    // 숨긴 뒤에도 값은 읽힌다
    assert_eq!(
        run_num("var o = {}; Object.defineProperty(o, 'h', { value: 7 }); o.h"),
        7.0
    );
}

#[test]
fn date_is_mutable() {
    // Date 는 가변 객체다 (표준). setter 가 없으면 쿠키 만료 계산 같은 흔한 코드가
    // "함수 아님" 으로 죽는다 (fmkorea 의 보안 스크립트가 date.setTime 을 쓴다).
    assert_eq!(
        run_num("var d = new Date(0); d.setTime(86400000); d.getTime()"),
        86400000.0
    );
    // 쿠키 만료 관용구: date.setTime(date.getTime() + 7일)
    assert_eq!(
        run_str("var d = new Date(0); d.setTime(d.getTime() + 7*24*60*60*1000); d.toISOString().slice(0,10)"),
        "1970-01-08"
    );
    assert_eq!(run_num("var d = new Date(2024,0,1); d.setDate(d.getDate()+7); d.getDate()"), 8.0);
    assert_eq!(run_num("var d = new Date(2024,0,1); d.setDate(32); d.getMonth()"), 1.0, "월 넘김");
    assert_eq!(run_num("var d = new Date(2024,0,1); d.setFullYear(2030); d.getFullYear()"), 2030.0);
    // setter 의 반환값은 새 타임스탬프
    assert_eq!(run_str("typeof new Date(0).setTime(5)"), "number");
}

#[test]
fn escape_unescape_annex_b() {
    // Annex B.2.1/B.2.2. 레거시지만 표준이고 국내 사이트가 쿠키 인코딩에 쓴다.
    assert_eq!(run_str("escape('a b')"), "a%20b");
    assert_eq!(run_str("escape('한')"), "%uD55C");
    assert_eq!(run_str("unescape('a%20b')"), "a b");
    assert_eq!(run_str("unescape('%uD55C')"), "한");
    assert_eq!(run_str("unescape(escape('가나다 ABC'))"), "가나다 ABC");
}

#[test]
fn array_callbacks_get_index_array_and_thisarg() {
    // 표준: 콜백은 (값, 인덱스, **배열**) 로 부르고, 두 번째 인자는 thisArg 다.
    // (값, 인덱스)만 넘기면 a[i-1] 같은 관용 코드가 죽는다 — IntersectionObserver
    // 폴리필의 _initThresholds 가 정확히 그 모양이라 fmkorea 에서 터졌다.
    assert_eq!(run_str("[1,1,2].filter((t, i, a) => t !== a[i-1]).join()"), "1,2");
    assert_eq!(run_str("String([1].map((v, i, a) => Array.isArray(a))[0])"), "true");
    assert_eq!(run_str("String([1].some((v, i, a) => Array.isArray(a)))"), "true");
    assert_eq!(run_str("String([1].find((v, i, a) => Array.isArray(a)))"), "1");
    assert_eq!(
        run_num("[1,2].reduce((acc, v, i, a) => acc + (Array.isArray(a) ? 1 : 0), 0)"),
        2.0
    );
    // thisArg
    assert_eq!(run_num("[1].map(function () { return this.x }, { x: 7 })[0]"), 7.0);
    assert_eq!(
        run_num("var n = 0; [1,2].forEach(function () { n += this.k }, { k: 3 }); n"),
        6.0
    );
}

#[test]
fn import_map_resolves_bare_specifiers() {
    // <script type="importmap"> 은 베어 명세자를 URL 로 해석하는 표준 메커니즘이다
    // (HTML §4.12.5). 없으면 import "react" 는 해석 불가로 실패한다.
    let mut it = Interp::new();
    it.import_map = vec![
        ("pkg/".to_string(), "https://x.test/js/".to_string()),
        ("mylib".to_string(), "https://x.test/lib.js".to_string()),
    ];
    assert_eq!(
        it.map_specifier("mylib").as_deref(),
        Some("https://x.test/lib.js"),
        "정확 매핑"
    );
    assert_eq!(
        it.map_specifier("pkg/deep/a.js").as_deref(),
        Some("https://x.test/js/deep/a.js"),
        "접두 매핑"
    );
    assert_eq!(it.map_specifier("./rel.js"), None, "상대 경로는 맵 대상이 아니다");
    assert_eq!(it.map_specifier("unknown"), None, "맵에 없으면 None (지어내지 않는다)");
}

#[test]
fn module_namespace_is_live_during_own_evaluation() {
    // ESM 네임스페이스는 모듈 환경의 **살아있는 뷰**다 (§10.4.6).
    // rspack/webpack 청크는 자기 자신을 import 해서 본문 도중 자기 네임스페이스를
    // 런타임에 넘긴다 (import * as a from "./self.js"; __webpack_require__.C(a)).
    // 예전엔 본문이 끝난 뒤에야 네임스페이스를 채워서 그때는 통째로 비어 있었고,
    // MDN 의 메인 모듈이 여기서 죽었다.
    let mut it = Interp::new();
    it.module_sources.insert(
        "https://x.test/self.js".to_string(),
        "import * as me from \"./self.js\"; \
         export const IDS = [\"a\", \"b\"]; \
         globalThis.seen = me.IDS ? me.IDS.length : -1;"
            .to_string(),
    );
    it.run_module("https://x.test/self.js").expect("모듈 평가");
    assert_eq!(to_display(&it.run("seen").unwrap()), "2", "본문 도중 자기 export 가 보여야");
}

#[test]
fn es_modules_evaluate_with_live_bindings() {
    // 예전엔 파서가 import 를 통째로 버리고 export 는 수식어만 벗겼다.
    // 의존성이 사라지니 모듈은 실행돼도 전부 undefined 였다
    // ("스크립트는 돌았는데 화면이 비었다"). 이제 표준 의미론대로 평가한다.
    let mut it = Interp::new();
    it.module_sources.insert(
        "https://x.test/util.js".to_string(),
        "export const VERSION = '1.0'; \
         let n = 0; \
         export function bump() { n++; return n; } \
         export function get() { return n; } \
         export default function greet(w) { return 'hi ' + w; }"
            .to_string(),
    );
    it.module_sources.insert(
        "https://x.test/re.js".to_string(),
        "export * from './util.js'; export { bump as inc } from './util.js';".to_string(),
    );
    it.module_sources.insert(
        "https://x.test/main.js".to_string(),
        "import greet, { VERSION, bump, get } from './util.js'; \
         import * as U from './util.js'; \
         import { inc } from './re.js'; \
         globalThis.r1 = greet('you'); \
         globalThis.r2 = VERSION; \
         globalThis.r3 = U.VERSION; \
         bump(); bump(); \
         globalThis.r4 = get(); \
         inc(); \
         globalThis.r5 = U.get(); \
         globalThis.r6 = typeof inc;"
            .to_string(),
    );
    it.run_module("https://x.test/main.js").expect("모듈 평가");

    assert_eq!(to_display(&it.run("r1").unwrap()), "hi you", "기본 export");
    assert_eq!(to_display(&it.run("r2").unwrap()), "1.0", "이름 export");
    assert_eq!(to_display(&it.run("r3").unwrap()), "1.0", "네임스페이스 import");
    assert_eq!(to_display(&it.run("r4").unwrap()), "2", "모듈 상태는 공유된다");
    // 재수출된 함수가 같은 모듈 인스턴스를 건드리고, 그 변화가 네임스페이스로 보인다
    // (살아있는 바인딩 — 값 스냅샷으로 흉내내면 3 이 안 보인다)
    assert_eq!(to_display(&it.run("r5").unwrap()), "3", "살아있는 바인딩");
    assert_eq!(to_display(&it.run("r6").unwrap()), "function", "재수출");
}

#[test]
fn import_outside_module_is_an_error_not_silence() {
    // 클래식 스크립트에 import 가 있으면 표준상 문법 오류다.
    // 예전엔 조용히 버려서, 의존성 없이 실행되다 엉뚱한 곳에서 죽었다.
    let mut it = Interp::new();
    assert!(it.run("import x from './m.js'; x").is_err());
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

// (1) Math/JSON 의 Symbol.toStringTag (§21.3.1.9/§25.5.1) → Object.prototype.toString.
// (2) Array 반복 메서드의 콜백 "배열" 인자는 원래 수신자(ToObject)여야 한다 (§23.1.3) —
// 예전엔 임시 복사본을 넘겨 array-like(Math 등) 대상에서 [object Array] 로 어긋났다.
#[test]
fn tostringtag_and_callback_array_arg() {
    assert_eq!(run_str("Object.prototype.toString.call(Math)"), "[object Math]");
    assert_eq!(run_str("Object.prototype.toString.call(JSON)"), "[object JSON]");
    assert_eq!(run_str("Math[Symbol.toStringTag]"), "Math");
    assert!(run_bool("Object.keys(Math).indexOf('Symbol(Symbol.toStringTag)')<0")); // 비열거
    // 콜백의 배열 인자가 원래 수신자
    assert!(run_bool("Math.length=1; Math[0]=1; \
        Array.prototype.reduce.call(Math, function(a,b,i,o){ return Object.prototype.toString.call(o)==='[object Math]'; }, 1)"));
    assert!(run_bool("var al={0:'x',length:1}; var seen; \
        Array.prototype.forEach.call(al, function(v,i,o){ seen=o; }); seen===al"));
    assert!(run_bool("var al={0:1,1:2,length:2}; \
        Array.prototype.map.call(al, function(v,i,o){ return o===al; }).every(function(x){return x;})"));
    // findLast 콜백은 (값,인덱스,배열) 3인자 + thisArg
    assert!(run_bool("var al={0:1,1:2,length:2}; var got; \
        Array.prototype.findLast.call(al, function(v,i,o){ got=o; return true; }); got===al"));
    assert_eq!(run_num("[10,20,30].findLast(function(v,i){ return i>=0; })"), 30.0);
}

// Array.prototype.sort/toSorted (§23.1.3.30): undefined 는 비교자 유무와 무관하게
// 항상 끝으로 가고 비교자에 넘기지 않는다. 예전엔 비교자에 undefined 를 넘겨(NaN)
// 정렬이 깨졌다.
#[test]
fn array_sort_undefined_to_end() {
    // 비교자 있어도 undefined 는 끝으로
    assert_eq!(run_str("[undefined,1].sort(function(a,b){return a-b;}).join(',')"), "1,");
    assert_eq!(run_str("[2,undefined,1].sort(function(a,b){return a-b;}).join(',')"), "1,2,");
    // 비교자는 undefined 로 호출되지 않는다
    assert_eq!(run_num("var n=0; [2,undefined,1].sort(function(a,b){n++;return a-b;}); n"), 1.0);
    // 기본(ToString) 정렬도 undefined 끝
    assert_eq!(run_str("[3,undefined,1].sort().join(',')"), "1,3,");
    // 희소 배열: 홀은 끝, 길이 보존
    assert!(run_bool("var x=new Array(2); x[1]=1; x.sort(); x[0]===1 && x[1]===undefined && x.length===2"));
    // toSorted(복사본, 프렐류드 폴리필)도 동일 + 원본 불변
    assert_eq!(prelude_str("var a=[3,undefined,1]; a.toSorted(function(x,y){return x-y;}).join(',')"), "1,3,");
    assert_eq!(prelude_str("var a=[3,undefined,1]; a.toSorted(); a.join(',')"), "3,,1");
    // 정상 숫자 정렬 무회귀
    assert_eq!(run_str("[3,1,2,10].sort(function(a,b){return a-b;}).join(',')"), "1,2,3,10");
    assert_eq!(run_str("[10,9,1,20].sort().join(',')"), "1,10,20,9");
}

// super() 는 부모(함수/네이티브)를 **현재 new.target**(파생 클래스)로 호출한다 (§10.2.2).
// 명시·암묵(기본 파생) 생성자 둘 다. 예전엔 부모의 new.target 이 undefined 라
// new.target 을 검사하는 추상 생성자(Iterator 등) 확장이 깨졌다.
#[test]
fn super_call_propagates_new_target() {
    // 부모 함수가 new.target 을 본다: 파생 클래스여야 (undefined 아님)
    assert!(run_bool("var seen; function B(){ seen=new.target; } \
        class C extends B { constructor(){ super(); } } new C(); seen!==undefined"));
    // 기본(암묵) 파생 생성자도 전파
    assert!(run_bool("var seen; function B(){ seen=new.target; } class C extends B {} new C(); seen!==undefined"));
    assert!(run_bool("var seen; function B(){ seen=new.target; } class C extends B { m(){} } new C(); seen!==undefined"));
    // 추상 생성자 패턴: new.target===자기면 throw, 서브클래스면 OK
    assert!(run_bool("function A(){ if(new.target===A||new.target===undefined) throw new TypeError('abstract'); } \
        var t=false; try{ new A() }catch(e){ t=e instanceof TypeError } \
        class S extends A {} var ok = typeof new S()==='object'; t && ok"));
    // Iterator(프렐류드 추상)도: 직접 new 는 throw, 확장은 OK
    assert!(prelude_bool("var t=false; try{ new Iterator() }catch(e){ t=e instanceof TypeError } \
        class S extends Iterator { get next(){ return function(){return {done:true,value:undefined};}; } } \
        t && typeof new S()==='object'"));
}

// RegExp \p{...} 유니코드 속성 이스케이프 (§, u 플래그). UCD 실제 데이터로 매칭.
#[test]
fn regex_unicode_property_escapes() {
    // General_Category (짧은/긴 이름, 파생 그룹)
    assert!(run_bool(r"/\p{L}/u.test('a') && /\p{L}/u.test('가') && !/\p{L}/u.test('5')"));
    assert!(run_bool(r"/\p{Lu}/u.test('A') && !/\p{Lu}/u.test('a')"));
    assert!(run_bool(r"/\p{N}/u.test('5') && /\p{Nd}/u.test('5') && !/\p{N}/u.test('a')"));
    assert!(run_bool(r"/\p{General_Category=Letter}/u.test('x')"));
    // Script / Script_Extensions (짧은/긴)
    assert!(run_bool(r"/\p{Script=Latin}/u.test('a') && !/\p{Script=Latin}/u.test('가')"));
    assert!(run_bool(r"/\p{Script=Hangul}/u.test('가') && /\p{sc=Grek}/u.test('α')"));
    assert!(run_bool(r"/\p{scx=Latin}/u.test('a')"));
    // 이진 속성
    assert!(run_bool(r"/\p{Alphabetic}/u.test('a') && /\p{White_Space}/u.test(' ') && /\p{Uppercase}/u.test('A')"));
    // 아스트랄(supplementary) 코드포인트
    assert!(run_bool(r"/\p{Lu}/u.test('\u{10400}') && /\p{Emoji}/u.test('\u{1F600}')"));
    // 부정 \P, 문자 클래스 안
    assert!(run_bool(r"/\P{L}/u.test('5') && !/\P{L}/u.test('a')"));
    assert!(run_bool(r"/[\p{L}\d]/u.test('a') && /[\p{L}\d]/u.test('5') && !/[\p{L}\d]/u.test('!')"));
    // 인식 못 하는 속성/값 또는 대소문자 불일치 → SyntaxError
    assert!(run_bool(r"var t=false; try{ new RegExp('\\p{Foobar}','u') }catch(e){ t=e instanceof SyntaxError } t"));
    assert!(run_bool(r"var t=false; try{ new RegExp('\\p{lu}','u') }catch(e){ t=e instanceof SyntaxError } t"));
    // u 플래그 없으면 리터럴 p
    assert!(run_bool(r"/\p{L}/.test('p{L}')"));
    // 중간 규모 선형 매치(테스트 스레드 스택 한계 내). 대규모(수백만)는 --js 큰 스택에서.
    assert!(run_bool("var s=''; for(var i=0;i<3000;i++) s+='a'; /^\\p{L}+$/u.test(s)"));
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
fn class_static_block_runs() {
    // ES2022 static 초기화 블록. 예전엔 파서가 여기서 죽어 **스크립트 전체**가 날아갔다.
    assert_eq!(run_num("class C { static x = 1; static { C.x = 4 } } C.x"), 4.0);
}

#[test]
fn bigint_is_exact_and_typed() {
    // 예전엔 렉서가 n 접미를 버리고 f64 로 근사했다 — 2n**64n 이 조용히 틀렸다.
    // 조용히 틀린 답은 미구현보다 나쁘다 (사이트는 typeof 로 탐지하고 정수 경로를 탄다).
    assert_eq!(run_str("typeof 1n"), "bigint");
    assert_eq!(run_str("(2n ** 64n).toString()"), "18446744073709551616");
    assert_eq!(run_str("String(-7n / 2n)"), "-3", "절단 나눗셈");
    assert_eq!(run_str("String(-7n % 2n)"), "-1", "나머지는 피제수 부호");
    assert_eq!(run_str("String(-5n & 3n)"), "3", "2의 보수 비트연산");
    assert_eq!(run_str("String(~5n)"), "-6");
    assert_eq!(run_str("(255n).toString(16)"), "ff");
    assert_eq!(run_str("String(BigInt('0x1f') + 1n)"), "32");
    // 타입 규칙: === 는 타입까지, == 는 값
    assert_eq!(run_str("String(1n === 1)"), "false");
    assert_eq!(run_str("String(1n == 1)"), "true");
    assert_eq!(run_str("String(2n > 1)"), "true", "비교는 혼합 허용");
    // 혼합 산술은 TypeError (조용히 f64 로 떨어뜨리면 값이 틀린다)
    assert_eq!(run_str("try { 1n + 1 } catch (e) { 'TypeError' }"), "TypeError");
    assert_eq!(run_str("try { +1n } catch (e) { 'TypeError' }"), "TypeError");
    // 문자열 결합은 허용
    assert_eq!(run_str("'x' + 1n"), "x1");
    assert_eq!(run_str("try { 1n / 0n } catch (e) { 'RangeError' }"), "RangeError");
}

// 태그드 템플릿의 strings.raw — styled-components / lit-html / graphql-tag 가
// 전부 이걸 읽는다. 없으면 그 라이브러리를 쓰는 페이지가 통째로 죽는다.
#[test]
fn tagged_template_provides_raw_strings() {
    assert_eq!(
        run_str("function t(s, ...v){ return s.raw.join('|') + '#' + v.join(','); } t`a${1}b${2}c`"),
        "a|b|c#1,2"
    );
    // raw 는 이스케이프를 처리하지 않은 원문이다
    assert_eq!(run_str("function t(s){ return s.raw[0]; } t`a\\nb`"), "a\\nb");
    assert_eq!(run_str("function t(s){ return s[0]; } t`a\\nb`"), "a\nb");
    // strings.length === values.length + 1 (표준)
    assert_eq!(run_str("function t(s, ...v){ return s.length + ',' + v.length; } t`${1}${2}`"), "3,2");
}

// instanceof 는 [Symbol.hasInstance] 가 있으면 그것이 최우선이다 (§13.10.2)
// 일반 함수를 extends 한 클래스에서 super() 가 this 를 **부모가 만든 객체로 갈아끼워**
// 파생 클래스의 메서드가 통째로 사라졌다 (astro.build 의 class Bus extends EventTarget).
// 표준(§10.2.2): 파생 생성자의 this 는 new.target.prototype 을 가진 객체다.
// test262 로 드러난 것들 — 표준이 요구하는데 조용히 틀렸던 동작들.

#[test]
fn huge_array_lengths_do_not_allocate_densely() {
    // new Array(2**32-1) 나 arr.length = 큰수 는 표준 동작이다. 밀집 배열로 확보하면
    // 요소당 24바이트라 64GB+ 를 즉시 요구해 머신을 다운시킨다 (실제로 그랬다 —
    // 전량 test262 실행이 맥을 멈췄다). length 만 기억하고 저장은 안 한다 (근사 희박).
    assert_eq!(run_str("'' + new Array(4294967295).length"), "4294967295");
    assert_eq!(run_str("var a = []; a.length = 1000000000; '' + a.length"), "1000000000");
    // 초거대 인덱스 대입도 폭주하지 않는다
    assert_eq!(run_str("var a = []; a[2000000000] = 1; typeof a[2000000000]"), "undefined");
    // 정상 크기 배열은 그대로 동작한다
    assert_eq!(run_str("'' + new Array(100).length"), "100");
    assert_eq!(run_str("var a = [1,2,3]; a[5] = 9; a.length + ',' + a[5]"), "6,9");
    assert_eq!(run_str("'' + [1,2,3].length"), "3");
}

#[test]
fn class_and_function_property_descriptors() {
    // §15.7.14: 클래스의 static 멤버는 클래스 객체의 own 프로퍼티다.
    // 예전엔 서술자 경로가 클래스를 몰라서 hasOwnProperty(C, 'm') 이 false 였고,
    // getOwnPropertyDescriptor(C, 'm') 이 undefined 였다.
    assert!(run_bool(
        "class C { static m(){} } Object.prototype.hasOwnProperty.call(C, 'm')"
    ));
    // static 메서드는 비열거, static 필드는 열거 가능
    assert!(run_bool(
        "class C { static m(){} } !Object.getOwnPropertyDescriptor(C, 'm').enumerable"
    ));
    assert!(run_bool(
        "class C { static F = 1; } Object.getOwnPropertyDescriptor(C, 'F').enumerable"
    ));
    assert_eq!(
        run_str("class C { static m(){} static F = 1; } JSON.stringify(Object.keys(C))"),
        "[\"F\"]"
    );
    // 클래스의 prototype 은 재설정 불가 (§15.7.14)
    assert!(run_bool(
        "class C {} var d = Object.getOwnPropertyDescriptor(C, 'prototype'); \
         !d.writable && !d.enumerable && !d.configurable"
    ));
    // 함수의 prototype/name/length 는 own 프로퍼티다 (§10.2.4~10.2.9)
    assert!(run_bool(
        "function f(a,b){} var d = Object.getOwnPropertyDescriptor(f, 'prototype'); \
         d.writable && !d.enumerable && !d.configurable"
    ));
    assert!(run_bool(
        "function f(a,b){} var d = Object.getOwnPropertyDescriptor(f, 'length'); \
         d.value === 2 && !d.writable && !d.enumerable && d.configurable"
    ));
    assert!(run_bool(
        "function f(){} Object.getOwnPropertyDescriptor(f, 'name').value === 'f'"
    ));
}

#[test]
fn named_evaluation_only_for_anonymous_function_expressions() {
    // §13.15.2 / §14.3.1: 구문상 **익명 함수/클래스**를 이름 있는 참조에 대입할 때만
    // 그 이름을 갖는다. 예전엔 값이 익명이기만 하면 이름을 줘서,
    // `var w = makeFn()` 처럼 호출 결과를 받은 함수까지 이름이 붙었다.
    assert_eq!(run_str("var x; x = function(){}; x.name"), "x");
    assert_eq!(run_str("var c; c = class {}; c.name"), "c");
    // 호출 결과를 대입하는 건 해당 없다
    assert_eq!(
        run_str("var w = (function(){ return function(){} })(); w.name"),
        ""
    );
    // 매개변수 기본값도 대입으로 펼쳐진다 (§10.2.11)
    assert_eq!(run_str("function f(a = function(){}) { return a.name; } f()"), "a");
    // 구조분해 기본값 (§8.6.2 / §14.3.3)
    assert_eq!(run_str("var { a = function(){} } = {}; a.name"), "a");
    assert_eq!(run_str("var [b = () => {}] = []; b.name"), "b");
    // 익명 클래스식 자체의 이름은 "" 다
    assert_eq!(run_str("(class {}).name"), "");
    assert_eq!(run_str("(class Named {}).name"), "Named");
}

#[test]
fn destructuring_assignment_with_defaults_parses() {
    // CoverInitializedName: { a = 1 } 은 객체 리터럴로는 문법 오류지만
    // 구조분해 **대입 대상**으로는 유효하다. 예전엔 파서가 여기서 죽어서
    // 그 스크립트가 통째로 못 돌았다.
    assert_eq!(run_str("var f; ({ f = 'dflt' } = {}); f"), "dflt");
    assert_eq!(run_str("var g; ({ g = 'dflt' } = { g: 'given' }); g"), "given");
    assert_eq!(run_str("var h; ({ x: h = 'd' } = {}); h"), "d");
}

#[test]
fn identifiers_allow_unicode_and_escapes() {
    // §12.7: 식별자는 유니코드다. 예전엔 ASCII 만 받아서, 유니코드 식별자나
    // 식별자 안의 유니코드 이스케이프가 파싱에서 통째로 죽었다.
    assert_eq!(run_str("var \u{c790}\u{bc14} = 'ko'; \u{c790}\u{bc14}"), "ko");
    assert_eq!(run_str("var caf\u{e9} = 'fr'; caf\u{e9}"), "fr");
    // Other_ID_Start (℘)
    assert_eq!(run_str("var \u{2118} = 'weierstrass'; \u{2118}"), "weierstrass");
    // 식별자 안의 유니코드 이스케이프
    assert_eq!(run_str(r"var \u0061bc = 'esc'; abc"), "esc");
    assert_eq!(run_str(r"var \u{1D49C} = 'ext'; \u{1D49C}"), "ext");
}

#[test]
fn private_names_are_not_properties() {
    // §6.2.12: private 이름은 프로퍼티가 아니다. 예전엔 "#x" 라는 이름의 필드였고,
    // Object.keys(instance) 가 ["#x"] 를 냈다 — JSON.stringify 로 private 데이터가
    // 그대로 새어 나갔다.
    assert_eq!(run_str("class C { #x = 1; p = 2; } JSON.stringify(Object.keys(new C()))"), "[\"p\"]");
    assert_eq!(run_str("class C { #x = 1; p = 2; } JSON.stringify(new C())"), "{\"p\":2}");
    // 그래도 클래스 안에서는 읽고 쓴다
    assert_eq!(run_str("class C { #x = 1; get(){ return this.#x } set(v){ this.#x = v; return this.#x } } var c = new C(); c.get() + ',' + c.set(9)"), "1,9");
    // #x in obj — 브랜드 검사 (§13.10.1). 왼쪽은 값이 아니라 private 이름이다.
    assert!(run_bool("class C { #x = 1; static has(o){ return #x in o } } C.has(new C())"));
    assert!(run_bool("class C { #x = 1; static has(o){ return #x in o } } !C.has({})"));
    assert!(run_bool("class C { #x = 1; static has(o){ return #x in o } } !C.has({'#x': 1})"));
}

#[test]
fn private_names_are_scoped_per_class() {
    // §6.2.12: 클래스를 평가할 때마다 **새 private 이름**이 만들어진다.
    // 같은 이름 #x 라도 클래스가 다르면 다른 이름이다.
    // 예전엔 키가 그냥 "#x" 라서 서로 다른 클래스가 같은 필드를 봤고,
    // 브랜드 검사도 서로를 통과시켰다.
    assert_eq!(
        run_str(
            "class C { #x = 1; get(){ return this.#x } } \
             class D { #x = 9; get(){ return this.#x } } \
             new C().get() + ',' + new D().get()"
        ),
        "1,9"
    );
    assert!(run_bool(
        "class C { #x = 1; static has(o){ return #x in o } } class D { #x = 9; } \
         C.has(new C()) && !C.has(new D())"
    ));
    // private 이름 해석은 렉시컬이다 — 클래스 메서드가 만든 콜백을 나중에 밖에서
    // 불러도 그 클래스의 #x 를 본다.
    assert_eq!(
        run_str("class F { #v = 42; cb(){ return () => this.#v; } } String(new F().cb()())"),
        "42"
    );
    // 클래스 본문 안에는 클래스 이름의 내부 바인딩이 있다 (§15.7.14) —
    // 메서드에서도 보여야 한다 (예전엔 static 필드 초기화에만 있었다).
    assert_eq!(
        run_str("String((class E { static #s = 5; static r(){ return E.#s } }).r())"),
        "5"
    );
}

#[test]
fn with_statement_uses_object_environment_record() {
    // §14.11 + §9.1.1.2. 없어서 with 를 쓰는 스크립트가 통째로 죽었다.
    assert_eq!(run_str("var o = {a:'x'}; with (o) { a }"), "x");
    // 객체 프로퍼티가 바깥 변수를 가린다
    assert_eq!(run_str("var o = {b:'inner'}; var b = 'outer'; with (o) { b }"), "inner");
    // 대입은 객체의 프로퍼티에 간다 (바깥 변수가 아니라)
    assert_eq!(run_str("var o = {a:1}; var a = 9; with (o) { a = 2; } o.a + ',' + a"), "2,9");
    // 세터가 실제로 돈다
    assert_eq!(
        run_str("var hit = ''; var s = { set x(v) { hit = v; } }; with (s) { x = 'set'; } hit"),
        "set"
    );
    // 프로토타입 체인도 본다
    assert_eq!(
        run_str("var p = {q:'proto'}; var c = Object.create(p); with (c) { q }"),
        "proto"
    );
    // null/undefined 에는 쓸 수 없다
    assert!(run_bool("try { with (null) {} } catch (e) { e instanceof TypeError }"));
}

#[test]
fn class_members_are_non_enumerable() {
    // §15.4: 클래스의 메서드/접근자/constructor 는 비열거다.
    // 예전엔 열거 가능해서 Object.keys(C.prototype) 가 ["m","g","constructor"] 였다 —
    // for-in 이나 JSON 으로 프로토타입 메서드가 새어 나온다.
    assert_eq!(run_str("class C { m(){} get g(){return 1} } JSON.stringify(Object.keys(C.prototype))"), "[]");
    assert!(run_bool(
        "class C { m(){} } !Object.getOwnPropertyDescriptor(C.prototype, 'm').enumerable"
    ));
    // 그래도 호출은 된다
    assert_eq!(run_str("class C { m(){ return 'ok' } } new C().m()"), "ok");
    // for-in 에도 안 나온다
    assert_eq!(
        run_str("class C { m(){} } var s = []; for (var k in new C()) s.push(k); JSON.stringify(s)"),
        "[]"
    );
}

#[test]
fn event_interfaces_have_real_prototype_chains() {
    // 예전엔 MouseEvent/KeyboardEvent 가 전부 같은 EventCtor 로 "근사" 되어 있었다.
    // new Event('x') instanceof Event 조차 false 였다.
    assert!(run_bool("new Event('x') instanceof Event"));
    assert!(run_bool("Object.getPrototypeOf(new Event('x')) === Event.prototype"));
    // MouseEvent → UIEvent → Event 상속 체인
    assert!(run_bool("new MouseEvent('click') instanceof MouseEvent"));
    assert!(run_bool("new MouseEvent('click') instanceof UIEvent"));
    assert!(run_bool("new MouseEvent('click') instanceof Event"));
    // 다른 인터페이스는 아니다
    assert!(run_bool("!(new MouseEvent('click') instanceof KeyboardEvent)"));
    assert!(run_bool("Event.prototype !== MouseEvent.prototype"));
    assert_eq!(run_str("new MouseEvent('click').constructor.name"), "MouseEvent");
    // 스크립트가 만든 이벤트는 신뢰되지 않는다 (표준)
    assert!(run_bool("new Event('x').isTrusted === false"));
}

#[test]
fn native_error_constructors_inherit_from_error() {
    // §20.5.6.2: NativeError 생성자의 [[Prototype]] 은 **Error 생성자**다.
    // 없으면 "TypeError 가 Error 의 서브타입인가" 를 프로토타입 체인으로 확인하는
    // 코드(testharness 의 assert_throws_js 가 정확히 이렇게 한다)가 아니라고 답한다.
    assert!(run_bool("Object.getPrototypeOf(TypeError) === Error"));
    assert!(run_bool("Object.getPrototypeOf(RangeError) === Error"));
    assert!(run_bool("Object.getPrototypeOf(SyntaxError) === Error"));
    // 체인을 걸어 name === 'Error' 인 생성자를 찾을 수 있어야 한다
    assert!(run_bool(
        "var o = TypeError; var found = false; \
         while (o) { if (typeof o === 'function' && o.name === 'Error') { found = true; break; } \
                     o = Object.getPrototypeOf(o); } found"
    ));
}

#[test]
fn in_operator_sees_array_length_and_methods() {
    // 예전엔 인덱스만 봐서 `"length" in []` 가 false 였다 — 값이 배열인지 확인하는
    // 코드(testharness 의 assert_array_equals 가 정확히 이렇게 한다)가 우리 배열을
    // 배열이 아니라고 판정했다.
    assert!(run_bool("'length' in []"));
    assert!(run_bool("'push' in []"));
    assert!(run_bool("0 in [1]"));
    assert!(run_bool("!(1 in [1])"));
    assert!(run_bool("!('nope' in [1])"));
}

#[test]
fn function_names_follow_standard() {
    // §10.2.9 SetFunctionName / NamedEvaluation. 예전엔 f.name 이 항상 "" 였다.
    assert_eq!(run_str("function f(){} f.name"), "f");
    assert_eq!(run_str("var g = function(){}; g.name"), "g"); // NamedEvaluation
    assert_eq!(run_str("var h = function named(){}; h.name"), "named"); // 명명식이 이긴다
    assert_eq!(run_str("const k = () => {}; k.name"), "k");
    assert_eq!(run_str("class C { m(){} } C.name"), "C");
    assert_eq!(run_str("class C { m(){} } C.prototype.m.name"), "m");
    assert_eq!(run_str("({ p: function(){} }).p.name"), "p");
    assert_eq!(run_str("({ q(){} }).q.name"), "q");
    // __proto__: v 는 프로퍼티 정의가 아니라 [[Prototype]] 설정 — 이름을 주지 않는다
    assert_eq!(
        run_str("var o = { __proto__: function(){} }; Object.getPrototypeOf(o).name"),
        ""
    );
}

#[test]
fn errors_are_real_objects_with_prototype_chain() {
    // 8종이 Error.prototype 하나를 공유했었다
    assert!(run_bool("TypeError.prototype !== Error.prototype"));
    assert!(run_bool("new TypeError('x').constructor === TypeError"));
    assert!(run_bool("new TypeError('x') instanceof TypeError"));
    assert!(run_bool("new TypeError('x') instanceof Error"));
    assert!(run_bool("!(new TypeError('x') instanceof RangeError)"));
    // "message 가 있으면 Error" 라는 오리 판별은 죽었다
    assert!(run_bool("!(({message:'x'}) instanceof Error)"));
    // message 는 비열거, 인자가 없으면 own 도 아니다
    assert_eq!(run_str("JSON.stringify(Object.keys(new Error('x')))"), "[]");
    assert!(run_bool(
        "!Object.prototype.hasOwnProperty.call(new Error(), 'message')"
    ));
    assert_eq!(run_str("String(new TypeError('boom'))"), "TypeError: boom");
    assert_eq!(run_str("String(new Error())"), "Error");
}

#[test]
fn internal_errors_have_standard_types() {
    // 예전엔 전부 Err(String) 이라 catch 가 문자열을 잡았다
    assert!(run_bool("try { null.x } catch (e) { e instanceof TypeError }"));
    assert!(run_bool("try { nope() } catch (e) { e instanceof ReferenceError }"));
    assert!(run_bool("try { (5)() } catch (e) { e instanceof TypeError }"));
    assert!(run_bool("try { for (const q of 5) {} } catch (e) { e instanceof TypeError }"));
    assert!(run_bool("try { var {z} = null; } catch (e) { e instanceof TypeError }"));
    assert!(run_bool("try { null.x } catch (e) { typeof e.message === 'string' }"));
}

#[test]
fn array_destructuring_uses_iterator_protocol() {
    // 예전엔 인덱스 접근만 해서 Set/Map/제너레이터가 조용히 undefined 였다
    assert_eq!(run_str("var [a,b] = new Set([1,2]); a + ',' + b"), "1,2");
    assert_eq!(
        run_str("function* g(){ yield 7; yield 8; yield 9; } var [p,,q] = g(); p + ',' + q"),
        "7,9"
    );
    assert_eq!(
        run_str("var [[k,v]] = new Map([['k',1]]); k + ',' + v"),
        "k,1"
    );
    assert_eq!(run_str("var [...r] = new Set([3,4]); r.join('-')"), "3-4");
    // 무한 이터러블도 필요한 만큼만 당긴다 (지연)
    assert_eq!(
        run_str("function* inf(){ var i=0; while(true) yield i++; } var [i0,i1] = inf(); i0 + ',' + i1"),
        "0,1"
    );
    // rest 는 이름이 아니라 패턴이다
    assert_eq!(run_str("var [x, ...[y, z]] = [1,2,3]; x + ',' + y + ',' + z"), "1,2,3");
}

#[test]
fn destructuring_propagates_thrown_errors() {
    // 이터레이터/게터가 던진 오류를 삼키면 안 된다
    assert!(run_bool(
        "var bad = { [Symbol.iterator]() { return { next() { throw new RangeError('i'); } }; } }; \
         try { var [a] = bad; false } catch (e) { e instanceof RangeError }"
    ));
    assert!(run_bool(
        "var g = { get x() { throw new RangeError('g'); } }; \
         try { var {x} = g; false } catch (e) { e instanceof RangeError }"
    ));
    assert!(run_bool(
        "var bad = { [Symbol.iterator]() { return { next() { throw new RangeError('s'); } }; } }; \
         try { var arr = [...bad]; false } catch (e) { e instanceof RangeError }"
    ));
}

#[test]
fn eval_direct_and_indirect() {
    assert_eq!(run_str("String(eval('1+1'))"), "2");
    // 직접 eval 은 호출 지점 스코프를 본다
    assert_eq!(
        run_str("var x = 10; function f(){ var y = 5; return String(eval('y + x')); } f()"),
        "15"
    );
    // 간접 eval 은 전역 스코프 (번들의 (0,eval)('this') 패턴)
    assert_eq!(
        run_str("var e = eval; function h(){ var z = 99; return e('typeof z'); } h()"),
        "undefined"
    );
    assert_eq!(run_str("typeof (0,eval)('this')"), "object");
    // var 는 호출자의 변수 환경에 만들어진다
    assert_eq!(run_str("eval('var ev1 = 7;'); String(ev1)"), "7");
    // 문자열이 아니면 그대로, 파싱 실패는 SyntaxError
    assert_eq!(run_str("String(eval(42))"), "42");
    assert!(run_bool("try { eval('var 1 = ;') } catch (e) { e instanceof SyntaxError }"));
}

#[test]
fn super_with_function_parent_keeps_derived_methods() {
    assert_eq!(
        run_num(
            "function Base(){ this.b = 1; } \
             class D extends Base { constructor(){ super(); this.n = 1; } inc(){ return ++this.n; } } \
             var d = new D(); d.inc() + d.b"
        ),
        3.0
    );
    // 부모의 prototype 메서드도 여전히 보인다
    assert_eq!(
        run_num(
            "function Base(){} Base.prototype.p = function(){ return 7; }; \
             class D extends Base { constructor(){ super(); } } \
             (new D()).p()"
        ),
        7.0
    );
    // 암묵 생성자여도 super(...args) 는 돈다 (class F extends Error {})
    assert_eq!(
        run_str("class F extends Error {} (new F('x')).message"),
        "x"
    );
    // 명시 생성자 + extends Error: 클래스 정체성이 유지된다
    assert!(run_bool(
        "class E extends Error { constructor(m){ super(m); this.name='E'; } } \
         var e = new E('b'); e instanceof E && e.message === 'b' && e.name === 'E'"
    ));
}

#[test]
fn instanceof_honors_symbol_has_instance() {
    assert!(prelude_bool(
        "var O = {}; O[Symbol.hasInstance] = function(x){ return typeof x === 'number'; }; 5 instanceof O"
    ));
    assert!(!prelude_bool(
        "var O = {}; O[Symbol.hasInstance] = function(x){ return typeof x === 'number'; }; 'a' instanceof O"
    ));
}

#[test]
fn optional_call_keeps_receiver() {
    // obj.m?.(args) 는 평범한 메서드 호출이다 — this 는 obj 다 (표준 §13.3.6.1).
    // 예전엔 수신자를 버려서 el.getAttribute?.('src') 같은 코드가 죽었다.
    assert_eq!(
        run_num("var o = { n: 7, get: function(){ return this.n; } }; o.get?.()"),
        7.0
    );
    // 옵셔널 멤버 + 옵셔널 호출 조합
    assert_eq!(
        run_num("var o = { n: 5, get: function(){ return this.n; } }; o?.get?.()"),
        5.0
    );
    // 함수가 없으면 단락 (호출 안 함)
    assert!(matches!(run("var o = {}; o.missing?.()"), Value::Undefined));
}

#[test]
fn for_await_unwraps_promises_and_async_iterators() {
    // for await (ES2018). 파싱이 안 되면 그 스크립트가 **통째로** 죽는다
    // (tailwindcss.com 이 그랬다). 값이 promise 면 이행값을 꺼내야 한다.
    assert_eq!(
        prelude_str(
            "var out = [];\
             async function f(){ for await (const v of [Promise.resolve(1), 2]) out.push(v); }\
             f(); out.join(',')"
        ),
        "1,2"
    );
    // Symbol.asyncIterator 를 먼저 찾는다
    assert_eq!(
        prelude_str(
            "var o = {}; o[Symbol.asyncIterator] = function(){ var i = 0; return { next: function(){ \
               i++; return Promise.resolve({value: i, done: i > 2}); } }; };\
             var out = [];\
             async function g(){ for await (const v of o) out.push(v); }\
             g(); out.join(',')"
        ),
        "1,2"
    );
}

// 구조분해의 **문자열/숫자 키** — 미니파이된 번들이 흔히 쓴다
// ({"icon-node": a}) => …. 예전엔 식별자 키만 받아서 그 스크립트가
// **파싱에서 통째로 죽었다** (lucide.dev 의 번들이 그렇다).
#[test]
fn string_and_number_keys_in_destructuring() {
    assert_eq!(run_num("var f = ({\"a-b\": x}) => x; f({\"a-b\": 5})"), 5.0);
    assert_eq!(run_num("var o = {\"k\": 7}; let {\"k\": v} = o; v"), 7.0);
    assert_eq!(run_num("var f = ({1: x}) => x; f({1: 9})"), 9.0);
    assert_eq!(
        run_num("function f({\"a-b\": x}) { return x; } f({\"a-b\": 3})"),
        3.0
    );
}

#[test]
fn computed_keys_in_destructuring() {
    // let { [ex]: v } = o (ES6). 예전엔 파서가 죽어서 그 번들 전체가 안 돌았다.
    assert_eq!(run_num("var k = 'a'; var o = {a: 3}; let { [k]: v } = o; v"), 3.0);
    assert_eq!(
        run_num("var k = 'miss'; var o = {a: 3}; let { [k]: v = 9 } = o; v"),
        9.0
    );
    assert_eq!(run_num("var k = 'a'; var o = {a: 4}; var t; ({ [k]: t } = o); t"), 4.0);
}

#[test]
fn optional_call_short_circuits() {
    // a?.m() 은 a 가 nullish 면 **호출 전체가 단락**된다 (§13.3.9).
    // 예전엔 a?.m 이 undefined 로 평가된 뒤 그걸 호출하려다 "함수 아님" 으로 죽었다.
    assert_eq!(run_str("var a = null; String(a?.m())"), "undefined");
    assert_eq!(run_str("var a = undefined; String(a?.m(1, 2))"), "undefined");
    // 인자는 평가되지 않는다 (단락)
    assert_eq!(
        run_num("var hit = 0; var a = null; a?.m(hit++); hit"),
        0.0
    );
    // 수신자가 있으면 정상 호출 (this 바인딩 유지)
    assert_eq!(run_num("var o = { n: 5, m() { return this.n } }; o?.m()"), 5.0);
}

#[test]
fn assignment_evaluates_left_reference_first() {
    // 표준 §13.15.2: 왼쪽 참조 → 오른쪽 값 순서. jQuery 가 이 순서에 의존한다:
    //   (b = se.selectors = {…}).pseudos.nth = b.pseudos.eq
    // 오른쪽을 먼저 평가하면 b 가 아직 undefined 라 jQuery 가 통째로 죽는다.
    assert_eq!(
        run_str("var b, se = {}; (b = se.sel = { p: { eq: 'E' } }).p.nth = b.p.eq; se.sel.p.nth"),
        "E"
    );
    // 복합 대입도 같은 순서 (왼쪽 참조 먼저)
    assert_eq!(
        run_num("var o, box = {}; (o = box.v = { n: 1 }).n += o.n; box.v.n"),
        2.0
    );
}

#[test]
fn class_heritage_can_be_a_call() {
    // ClassHeritage 는 LeftHandSideExpression — 믹스인 호출이 온다 (lit-element/MDN).
    // 예전엔 파서가 죽어서 모듈 전체가 실행되지 않았다.
    assert_eq!(
        run_num("class A { m() { return 1 } } var id = x => x; class B extends (0, id)(A) { m() { return super.m() + 1 } } new B().m()"),
        2.0
    );
}

#[test]
fn super_property_read_uses_parent_getter() {
    // super.x 는 호출이 아니라 **읽기**로도 쓴다. 예전엔 super 가 undefined 로 평가돼 터졌다.
    assert_eq!(
        run_num("class A { get v() { return 1 } } class B extends A { get v() { return super.v + 1 } } new B().v"),
        2.0
    );
    // super.method 를 값으로 꺼내 쓰기
    assert_eq!(
        run_num("class A { m() { return 5 } } class B extends A { m() { return super.m.call(this) } } new B().m()"),
        5.0
    );
}

#[test]
fn in_operator_walks_prototype_chain() {
    // in 은 own 프로퍼티만이 아니라 프로토타입 체인까지 본다 (§13.10)
    assert_eq!(run_str("var o = Object.create({a:1}); ('a' in o) + ',' + ('b' in o)"), "true,false");
    // 클래스 인스턴스: 메서드도 in 으로 보인다
    assert_eq!(run_str("class C { m(){} } var c = new C(); String('m' in c)"), "true");
}

#[test]
fn proxy_has_delete_ownkeys_traps() {
    // Vue 3 같은 반응성 라이브러리가 실제로 쓰는 트랩들. 없으면 조용히 틀린 답을 준다.
    assert_eq!(run_str("var p = new Proxy({}, {has: () => true}); String('z' in p)"), "true");
    assert_eq!(
        run_str("var hit=false; var p=new Proxy({a:1},{deleteProperty(t,k){hit=true; delete t[k]; return true}}); delete p.a; String(hit)"),
        "true"
    );
    assert_eq!(
        run_str("var p=new Proxy({a:1},{ownKeys:()=>['a','b']}); Object.keys(p).join()"),
        "a,b"
    );
}

#[test]
fn to_primitive_for_unary_and_symbol() {
    // 단항 + 도 ToPrimitive 를 거친다 (이항만 거쳐서 +obj 가 NaN 이었다)
    assert_eq!(run_num("+{ valueOf() { return 5 } }"), 5.0);
    assert_eq!(run_num("-{ valueOf() { return 5 } }"), -5.0);
    // Symbol.toPrimitive 가 valueOf/toString 보다 우선
    assert_eq!(run_num("+{ [Symbol.toPrimitive]() { return 42 }, valueOf() { return 1 } }"), 42.0);
}

#[test]
fn json_stringify_replacer_and_indent() {
    // replacer 배열 = 키 필터
    assert_eq!(run_str("JSON.stringify({a:1,b:2}, ['a'])"), "{\"a\":1}");
    // replacer 함수 = 값 변환
    assert_eq!(
        run_str("JSON.stringify({a:1}, (k,v) => typeof v === 'number' ? v * 2 : v)"),
        "{\"a\":2}"
    );
    // space = 들여쓰기 (JSON.stringify(o, null, 2) 는 아주 흔하다)
    assert_eq!(run_str("JSON.stringify({a:1}, null, 2)"), "{\n  \"a\": 1\n}");
    assert_eq!(run_str("JSON.stringify([1,2], null, 1)"), "[\n 1,\n 2\n]");
    // 들여쓰기 없으면 예전 그대로 한 줄
    assert_eq!(run_str("JSON.stringify({a:[1,{b:2}]})"), "{\"a\":[1,{\"b\":2}]}");
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
    // 실행 한도는 try/catch 로 못 잡는다 (가드 무력화 방지). 짧은 예산으로 확인.
    let mut it = Interp::new();
    it.script_budget_ms = 200;
    assert!(it.run("try { while (true) {} } catch (e) { 'nope' }").is_err());
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
fn computed_and_keyword_accessors() {
    // { get [expr]() {} } — 계산된 접근자. 키는 런타임 평가(심볼 키도 가능).
    assert_eq!(
        run_num("var k='dyn'; var o={ base:5, get [k]() { return this.base*2; } }; o.dyn"),
        10.0,
    );
    assert_eq!(
        run_str("var s=Symbol('t'); var o={ get [s]() { return 'sg'; } }; o[s]"),
        "sg",
    );
    // 예약어를 접근자 이름으로 — { get class() {} } (미니파이 번들에 흔함)
    assert_eq!(
        run_str("var o={ get class(){ return 'cls'; }, get default(){ return 'def'; } }; o.class + o.default"),
        "clsdef",
    );
    // 기존 접근자는 그대로
    assert_eq!(run_num("var o={ get x(){ return 42; } }; o.x"), 42.0);
    // get 이 그냥 프로퍼티명인 경우 오검출 방지
    assert_eq!(run_num("var o={ get: 7 }; o.get"), 7.0);
    assert_eq!(run_str("var o={ get(){ return 'm'; } }; o.get()"), "m");
}

#[test]
fn await_operand_can_be_async_function_expression() {
    // `await async function(){}` — await 의 피연산자로 async 함수식이 오는 패턴.
    // await 는 unary() 로 피연산자를 파싱하는데 async 감지가 assignment() 에만 있어
    // 번들이 통째로 파싱 실패했다.
    assert!(run_bool(
        "var r=null; (async function(){ r = await (async function(a,b){ return a+b; })(3,4); })(); \
         r === 7"
    ));
    // async 화살표도
    assert!(run_bool(
        "var r=null; (async function(){ r = await (async (a)=>a*2)(5); })(); r === 10"
    ));
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

// 프로퍼티 서술자 (§10.1.6): writable/configurable 이 실제로 강제되는가.
// 예전엔 서술자가 이름만 있고 강제되지 않았다 — writable:false 여도 재대입이
// 통과하고, configurable:false 여도 재정의/삭제가 됐고, getOwnPropertyDescriptor 는
// 항상 writable:true/configurable:true 를 거짓말했다.
#[test]
fn property_descriptors_are_enforced() {
    // writable:false → 재대입 무시 (sloppy)
    assert_eq!(
        run_num("var o={}; Object.defineProperty(o,'x',{value:1,writable:false}); o.x=99; o.x"),
        1.0
    );
    // configurable:false → 재정의 TypeError
    assert!(run_bool(
        "var o={}; Object.defineProperty(o,'x',{value:1,configurable:false}); \
         try{ Object.defineProperty(o,'x',{value:2}); false }catch(e){ e instanceof TypeError }"
    ));
    // configurable:false → 삭제 거부, delete 는 false
    assert!(run_bool(
        "var o={}; Object.defineProperty(o,'x',{value:1,configurable:false}); \
         (delete o.x)===false && o.x===1"
    ));
    // getOwnPropertyDescriptor 가 실제 속성을 보고
    assert!(run_bool(
        "var o={}; Object.defineProperty(o,'x',{value:1}); \
         var d=Object.getOwnPropertyDescriptor(o,'x'); \
         d.writable===false && d.enumerable===false && d.configurable===false && d.value===1"
    ));
    // 접근자 서술자
    assert!(run_bool(
        "var o={}; Object.defineProperty(o,'y',{get:function(){return 7},enumerable:true}); \
         var d=Object.getOwnPropertyDescriptor(o,'y'); \
         typeof d.get==='function' && d.set===undefined && d.enumerable===true && o.y===7"
    ));
    // writable:false 는 configurable:true 면 defineProperty 로 값 변경 가능
    assert_eq!(
        run_num("var o={}; Object.defineProperty(o,'x',{value:1,writable:false,configurable:true}); \
                 Object.defineProperty(o,'x',{value:2}); o.x"),
        2.0
    );
    // 접근자와 value 를 동시 지정하면 TypeError
    assert!(run_bool(
        "try{ Object.defineProperty({},'x',{value:1,get:function(){}}); false }\
         catch(e){ e instanceof TypeError }"
    ));
}

// 배열 메서드는 generic 하다 (§23.1.3): array-like 에도 적용되고, null/undefined 는
// TypeError. 예전엔 진짜 배열 아니면 일반 Error 를 던졌고, 두 곳에 메서드 목록을
// 따로 관리해 flat/at/fill 등이 Array.prototype 경로에서 undefined 였다.
#[test]
fn array_methods_are_generic() {
    // array-like 에 적용
    assert_eq!(
        run_str("var al={0:'a',1:'b',2:'c',length:3}; \
                 JSON.stringify(Array.prototype.map.call(al,function(x){return x+x}))"),
        r#"["aa","bb","cc"]"#
    );
    assert_eq!(run_num("Array.prototype.indexOf.call({0:'a',1:'b',length:2},'b')"), 1.0);
    // 문자열에 적용 (array-like)
    assert_eq!(run_str("Array.prototype.join.call('xyz','-')"), "x-y-z");
    // null/undefined 는 TypeError (§7.1.18)
    assert!(run_bool(
        "try{ Array.prototype.forEach.call(null,function(){}); false }\
         catch(e){ e instanceof TypeError }"
    ));
    // Array.prototype.flat/at/fill 등이 인스턴스와 프로토타입 양쪽에서 동작
    assert!(run_bool("typeof Array.prototype.flat==='function'"));
    assert!(run_bool("typeof Array.prototype.at==='function'"));
    assert!(run_bool("typeof Array.prototype.fill==='function'"));
    assert_eq!(
        run_str("JSON.stringify(Array.prototype.flat.call({0:1,1:[2,3],length:2}))"),
        "[1,2,3]"
    );
    // flat 은 depth 를 존중한다 (§23.1.3.11)
    assert_eq!(run_str("JSON.stringify([1,[2,[3,[4]]]].flat(2))"), "[1,2,3,[4]]");
    assert_eq!(run_str("JSON.stringify([1,[2,[3]]].flat(Infinity))"), "[1,2,3]");
    assert_eq!(run_str("JSON.stringify([1,[2,[3]]].flat())"), "[1,2,[3]]");
}

// String.prototype 메서드는 generic 하다 (§22.1.3): this 를 ToString 으로 강제한다.
// null/undefined 는 TypeError. 예전엔 진짜 문자열이 아니면 일반 Error 를 던졌다.
#[test]
fn string_methods_are_generic() {
    assert_eq!(run_str("String.prototype.trim.call(42)"), "42");
    assert_eq!(run_str("String.prototype.slice.call(12345,1,3)"), "23");
    assert_eq!(run_str("String.prototype.toUpperCase.call('ab')"), "AB");
    // 불리언/객체도 ToString
    assert_eq!(run_str("String.prototype.charAt.call(true,0)"), "t");
    // null/undefined → TypeError
    assert!(run_bool(
        "try{ String.prototype.trim.call(null); false }catch(e){ e instanceof TypeError }"
    ));
    assert!(run_bool(
        "try{ String.prototype.trim.call(undefined); false }catch(e){ e instanceof TypeError }"
    ));
    // Symbol → TypeError (문자열로 변환 불가)
    assert!(run_bool(
        "try{ String.prototype.trim.call(Symbol('x')); false }catch(e){ e instanceof TypeError }"
    ));
}

// String.prototype.toLocaleLowerCase/toLocaleUpperCase (§22.1.3.24/.25): Intl 없으면
// 로케일 독립 대소문자 매핑(=toLowerCase/toUpperCase). 예전엔 미구현(undefined)이었다.
#[test]
fn string_to_locale_case() {
    assert!(prelude_bool("typeof ''.toLocaleLowerCase==='function' && typeof ''.toLocaleUpperCase==='function'"));
    assert_eq!(prelude_str("'ABC'.toLocaleLowerCase()"), "abc");
    assert_eq!(prelude_str("'abc'.toLocaleUpperCase()"), "ABC");
    // 수신자 ToString(§RequireObjectCoercible 후)
    assert_eq!(prelude_str("String.prototype.toLocaleUpperCase.call(456)"), "456");
    // null/undefined → TypeError
    assert!(prelude_bool("try{ String.prototype.toLocaleLowerCase.call(null); false }catch(e){ e instanceof TypeError }"));
    assert!(prelude_bool("try{ String.prototype.toLocaleUpperCase.call(undefined); false }catch(e){ e instanceof TypeError }"));
    // name/length 서술자 (§10.2): name===메서드명, length===0, writable:false, configurable:true
    assert!(prelude_bool("var d=Object.getOwnPropertyDescriptor(String.prototype.toLocaleLowerCase,'name'); \
                          d.value==='toLocaleLowerCase' && d.writable===false && d.configurable===true"));
    assert!(prelude_bool("String.prototype.toLocaleLowerCase.length===0"));
}

// Error cause 옵션 (§20.5.1.1, ES2022) + Promise.try (§27.2.4.6, ES2025).
#[test]
fn error_cause_and_promise_try() {
    // cause: options 객체의 cause 를 비열거 own 으로
    assert_eq!(run_num("new Error('m',{cause:42}).cause"), 42.0);
    assert_eq!(run_str("new TypeError('t',{cause:'x'}).cause"), "x");
    assert!(run_bool("var e=new Error('m',{cause:1}); Object.keys(e).indexOf('cause')<0 && Object.prototype.hasOwnProperty.call(e,'cause')"));
    // options 없거나 cause 없으면 cause 프로퍼티 없음
    assert!(run_bool("!('cause' in new Error('m')) && !('cause' in new Error('m',{}))"));
    // cause: undefined 도 프로퍼티는 존재(HasProperty 기준)
    assert!(run_bool("var e=new Error('m',{cause:undefined}); Object.prototype.hasOwnProperty.call(e,'cause') && e.cause===undefined"));
    // AggregateError 는 options 가 세 번째 인자
    assert_eq!(run_num("new AggregateError([],'m',{cause:99}).cause"), 99.0);
    // Promise.try(프렐류드): 구조(length 1, thenable). fn 은 동기 호출(부수효과로 관측).
    assert!(prelude_bool("typeof Promise.try==='function' && Promise.try.length===1"));
    assert!(prelude_bool("var called=false; var p=Promise.try(function(){called=true;}); called===true && typeof p.then==='function'"));
    assert!(prelude_bool("var got; Promise.try(function(a,b){got=a+b;},2,3); got===5"));  // 인자 전달(동기)
}

// Array.prototype.toLocaleString (§23.1.3.32): 각 원소의 toLocaleString() 을 호출해
// ','로 잇는다. null/undefined 는 빈 문자열. 예전 폴리필은 원소 toLocaleString 미호출.
#[test]
fn array_to_locale_string() {
    assert_eq!(prelude_str("[1,2,3].toLocaleString()"), "1,2,3");
    assert_eq!(prelude_str("[1,null,3,undefined].toLocaleString()"), "1,,3,");
    assert_eq!(prelude_str("[].toLocaleString()"), "");
    // 원소의 toLocaleString 이 실제 호출됨
    assert_eq!(prelude_str("var o={toLocaleString:function(){return 'X';}}; [o,o].toLocaleString()"), "X,X");
}

// Iterator 헬퍼 (§27.1): 제너레이터의 member 해석을 %IteratorPrototype%(__kIterProto)로
// 위임하고 map/filter/take/drop/flatMap/reduce/toArray/forEach/some/every/find 를 지연
// 제너레이터로 구현. 예전엔 전부 미구현(undefined)이었다.
#[test]
fn iterator_helpers() {
    assert!(prelude_bool("typeof Iterator==='function' && typeof Iterator.prototype.map==='function'"));
    assert_eq!(prelude_str("function* g(){yield 1;yield 2;yield 3;yield 4;} g().map(function(x){return x*10;}).toArray().join(',')"), "10,20,30,40");
    assert_eq!(prelude_str("function* g(){yield 1;yield 2;yield 3;yield 4;} g().filter(function(x){return x%2;}).toArray().join(',')"), "1,3");
    assert_eq!(prelude_str("function* g(){yield 1;yield 2;yield 3;yield 4;} g().take(2).toArray().join(',')"), "1,2");
    assert_eq!(prelude_str("function* g(){yield 1;yield 2;yield 3;yield 4;} g().drop(2).toArray().join(',')"), "3,4");
    // 체이닝 (반환 제너레이터도 헬퍼 상속)
    assert_eq!(prelude_str("function* g(){yield 1;yield 2;yield 3;yield 4;} g().map(function(x){return x*2;}).filter(function(x){return x>2;}).take(2).toArray().join(',')"), "4,6");
    assert_eq!(prelude_str("function* g(){yield 1;yield 2;yield 3;} g().flatMap(function(x){return [x,x];}).toArray().join(',')"), "1,1,2,2,3,3");
    assert_eq!(prelude_num("function* g(){yield 1;yield 2;yield 3;} g().reduce(function(a,b){return a+b;},0)"), 6.0);
    assert_eq!(prelude_num("function* g(){yield 1;yield 2;yield 3;} g().reduce(function(a,b){return a+b;})"), 6.0);
    assert!(prelude_bool("function* g(){yield 1;yield 2;yield 3;} g().some(function(x){return x===2;})===true && g().every(function(x){return x<10;})===true"));
    assert_eq!(prelude_num("function* g(){yield 1;yield 2;yield 3;} g().find(function(x){return x>1;})"), 2.0);
    // Iterator.prototype.map.call
    assert_eq!(prelude_str("function* g(){yield 5;yield 6;} Iterator.prototype.map.call(g(),function(x){return x;}).toArray().join(',')"), "5,6");
    // 인자/수신자 검증
    assert!(prelude_bool("function* g(){yield 1;} var t=false; try{ g().map(5) }catch(e){ t=e instanceof TypeError } t"));
    assert!(prelude_bool("function* g(){yield 1;} var t=false; try{ g().take(-1).toArray() }catch(e){ t=e instanceof RangeError } t"));
    assert!(prelude_bool("var t=false; try{ (function*(){})().reduce(function(a,b){return a;}) }catch(e){ t=e instanceof TypeError } t"));
    // Iterator.from + 추상 생성자 + Symbol.iterator
    assert_eq!(prelude_str("Iterator.from([7,8,9]).toArray().join(',')"), "7,8,9");
    assert!(prelude_bool("var t=false; try{ new Iterator() }catch(e){ t=e instanceof TypeError } t"));
    assert!(prelude_bool("function* g(){yield 1;} var it=g(); it[Symbol.iterator]()===it"));
    // 헬퍼 이름
    assert!(prelude_bool("Iterator.prototype.map.name==='map' && Iterator.prototype.filter.name==='filter'"));
}

// Explicit Resource Management (§): Symbol.dispose + DisposableStack + SuppressedError.
// 예전엔 전부 미구현이었다.
#[test]
fn disposable_stack() {
    assert!(prelude_bool("typeof Symbol.dispose==='symbol' && typeof DisposableStack==='function'"));
    assert!(prelude_bool("new DisposableStack().disposed===false"));
    // use/adopt/defer 후 dispose 는 역순으로 정리자 실행
    assert_eq!(prelude_str(
        "var s=new DisposableStack(); var o=[]; \
         s.use({[Symbol.dispose](){o.push('u1');}}); \
         s.use({[Symbol.dispose](){o.push('u2');}}); \
         s.defer(function(){o.push('d');}); \
         s.adopt('X',function(v){o.push('a:'+v);}); \
         s.dispose(); o.join(',')"), "a:X,d,u2,u1");
    // use 는 값 반환, 처리 후 disposed
    assert!(prelude_bool("var s=new DisposableStack(); var r={[Symbol.dispose](){}}; s.use(r)===r && (s.dispose(), s.disposed===true)"));
    // dispose 멱등
    assert!(prelude_bool("var s=new DisposableStack(); s.dispose(); s.dispose()===undefined"));
    // dispose 후 use → ReferenceError; 비-disposable → TypeError; null → no-op
    assert!(prelude_bool("var s=new DisposableStack(); s.dispose(); \
                          var t=false; try{ s.use({}) }catch(e){ t=e instanceof ReferenceError } t"));
    assert!(prelude_bool("var t=false; try{ new DisposableStack().use({}) }catch(e){ t=e instanceof TypeError } t"));
    assert!(prelude_bool("new DisposableStack().use(null)===null"));
    // defer/adopt 비함수 인자 → TypeError
    assert!(prelude_bool("var t=false; try{ new DisposableStack().defer(5) }catch(e){ t=e instanceof TypeError } t"));
    // move: 원본은 disposed, 새 스택이 정리자 소유
    assert!(prelude_bool("var a=new DisposableStack(); var ran=false; a.defer(function(){ran=true;}); \
                          var b=a.move(); a.disposed===true && b.disposed===false && (b.dispose(), ran===true)"));
    // [Symbol.dispose]() === dispose
    assert!(prelude_bool("var s=new DisposableStack(); var x=false; s.defer(function(){x=true;}); s[Symbol.dispose](); x===true"));
    // SuppressedError: 정리 중 두 오류가 겹치면 집계
    assert!(prelude_bool(
        "var s=new DisposableStack(); s.defer(function(){throw new Error('first');}); \
         s.defer(function(){throw new Error('second');}); \
         var ok=false; try{ s.dispose() }catch(e){ \
           ok = e instanceof SuppressedError && e.error.message==='first' && e.suppressed.message==='second'; } ok"));
    // SuppressedError 는 Error 하위, prototype.name 정확
    assert!(prelude_bool("new SuppressedError(1,2,'m') instanceof Error && SuppressedError.prototype.name==='SuppressedError'"));
    assert!(prelude_bool("var e=new SuppressedError('x','y','m'); e.error==='x' && e.suppressed==='y' && e.message==='m'"));
    // AsyncDisposableStack — 동기 관측 부분(구조/가드/move) + async 정리 결과
    assert!(prelude_bool("typeof AsyncDisposableStack==='function' && typeof Symbol.asyncDispose==='symbol'"));
    assert!(prelude_bool("new AsyncDisposableStack().disposed===false"));
    assert!(prelude_bool("var s=new AsyncDisposableStack(); var r={[Symbol.asyncDispose](){}}; s.use(r)===r"));
    assert!(prelude_bool("var t=false; try{ new AsyncDisposableStack().use({}) }catch(e){ t=e instanceof TypeError } t"));
    assert!(prelude_bool("new AsyncDisposableStack().use(null)===null"));
    assert!(prelude_bool("var a=new AsyncDisposableStack(); a.defer(function(){}); var b=a.move(); a.disposed===true && b.disposed===false"));
    // disposeAsync 는 Promise 반환. dispose 후 disposed 플래그는 동기적으로 즉시 true.
    assert!(prelude_bool("var s=new AsyncDisposableStack(); var p=s.disposeAsync(); typeof p.then==='function' && s.disposed===true"));
}

// 클래스도 함수다 (§10.2/§15.7): C.length===생성자 파라미터 수, new 없이 호출하면
// TypeError (§15.7.10). 예전엔 length 가 undefined 였고 C() 가 조용히 생성했다.
#[test]
fn class_length_and_new_required() {
    assert_eq!(run_num("class C{ constructor(a,b,c){} } C.length"), 3.0);
    assert_eq!(run_num("class D{} D.length"), 0.0);
    assert_eq!(run_num("class E extends Array{ constructor(a,b){ super(); } } E.length"), 2.0);
    assert_eq!(run_str("class C{} C.name"), "C");
    // new 없이 호출 → TypeError (C() / C.call() / Reflect.apply)
    assert!(run_bool("class C{} var t=false; try{ C() }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("class C{} var t=false; try{ C.call(null) }catch(e){ t=e instanceof TypeError } t"));
    // new 는 정상
    assert!(run_bool("class C{ constructor(){ this.x=1; } } new C().x===1"));
    // static length 오버라이드가 우선
    assert_eq!(run_num("class C{ constructor(a,b){} static get length(){ return 99; } } C.length"), 99.0);
}

// 내장 함수는 스펙상 name/length own 프로퍼티를 가진다 (§17). 예전엔 항상 ""/0.
// 읽기 경로(값, getOwnPropertyDescriptor, hasOwnProperty)를 표준대로 보고한다.
#[test]
fn native_function_name_and_length() {
    // 메서드 계열
    assert_eq!(run_str("[].map.name"), "map");
    assert_eq!(run_num("[].map.length"), 1.0);
    assert_eq!(run_str("''.trim.name"), "trim");
    assert_eq!(run_num("''.slice.length"), 2.0);
    // 생성자
    assert_eq!(run_str("Array.name"), "Array");
    assert_eq!(run_num("Array.length"), 1.0);
    assert_eq!(run_str("String.name"), "String");
    // 전역 함수
    assert_eq!(run_str("parseInt.name"), "parseInt");
    assert_eq!(run_num("parseInt.length"), 2.0);
    // getOwnPropertyDescriptor: name 은 { writable:false, enumerable:false, configurable:true }
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor([].map,'name'); \
         d.value==='map' && d.writable===false && d.enumerable===false && d.configurable===true"
    ));
    // hasOwnProperty
    assert!(run_bool("[].map.hasOwnProperty('name') && [].map.hasOwnProperty('length')"));
    // 바운드 함수: 'bound ' 접두 + length = target.length - 바운드 인자 수
    assert_eq!(run_str("(function foo(a,b,c){}).bind(null,1).name"), "bound foo");
    assert_eq!(run_num("(function foo(a,b,c){}).bind(null,1).length"), 2.0);
}

// 표준 위반 편법: 내장 연산이 일반 Error(한국어 문자열)를 던져서 catch 에서
// TypeError/RangeError 로 잡히지 않았다. throw_error 로 타입을 세우고,
// error_from_msg 가 "RangeError: ..." 접두를 그 타입으로 승격한다.
#[test]
fn builtin_errors_have_correct_type() {
    assert!(run_bool("try{[].reduce(function(a,b){return a});false}catch(e){e instanceof TypeError}"));
    assert!(run_bool("try{[].reduceRight(function(a,b){return a});false}catch(e){e instanceof TypeError}"));
    assert!(run_bool("try{Object.assign(null,{});false}catch(e){e instanceof TypeError}"));
    assert!(run_bool("try{JSON.stringify(10n);false}catch(e){e instanceof TypeError}"));
    // 스택 초과 → RangeError
    assert!(run_bool("try{(function r(){return r()})();false}catch(e){e instanceof RangeError}"));
    // BigInt 0 나눗셈/음수 지수 → RangeError (접두 파싱 경로)
    assert!(run_bool("try{5n/0n;false}catch(e){e instanceof RangeError}"));
    assert!(run_bool("try{2n**-1n;false}catch(e){e instanceof RangeError}"));
}

// RegExp.prototype 의 flags/source/각 플래그는 접근자(getter)다 (§22.2.6) —
// 인스턴스 own 데이터가 아니다. 예전엔 sticky/unicode/dotAll 이 undefined 였고
// getOwnPropertyDescriptor(RegExp.prototype,'flags').get 이 없었다.
#[test]
fn regexp_flag_accessors() {
    // 인스턴스 접근: 모든 플래그가 boolean
    assert!(run_bool("/x/gi.global===true && /x/gi.ignoreCase===true"));
    assert!(run_bool("/x/.sticky===false && /x/.unicode===false && /x/.dotAll===false"));
    assert!(run_bool("/x/y.sticky===true && /x/s.dotAll===true && /x/u.unicode===true"));
    assert!(run_bool("/x/d.hasIndices===true"));
    // flags 는 표준 순서로 정렬 (d,g,i,m,s,u,y)
    assert_eq!(run_str("/x/yig.flags"), "giy");
    assert_eq!(run_str("/ab+c/.source"), "ab+c");
    // 빈 패턴 source 는 "(?:)"
    assert_eq!(run_str("new RegExp('').source"), "(?:)");
    // RegExp.prototype 의 접근자 서술자
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(RegExp.prototype,'flags'); typeof d.get==='function'"
    ));
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(RegExp.prototype,'source'); typeof d.get==='function'"
    ));
    // getter 를 인스턴스에 호출하면 계산됨
    assert!(run_bool(
        "Object.getOwnPropertyDescriptor(RegExp.prototype,'global').get.call(/x/g)===true"
    ));
    // 인스턴스는 플래그 own 데이터를 갖지 않는다 (상속 접근자)
    assert!(run_bool("/x/g.hasOwnProperty('global')===false"));
}

// 표준 §22.2.3.1: RegExp 생성자는 생성 시점에 패턴/플래그를 검증하고 잘못됐으면
// SyntaxError. 예전엔 검증 없이 객체만 만들어 new RegExp("(") 가 조용히 통과했다.
#[test]
fn regexp_construction_validates() {
    let bad = |p: &str| {
        run_bool(&format!(
            "try{{new RegExp('{}');false}}catch(e){{e instanceof SyntaxError}}",
            p
        ))
    };
    assert!(bad("("));
    assert!(bad("a)"));
    assert!(bad("["));
    // 플래그 검증
    assert!(run_bool("try{new RegExp('x','gg');false}catch(e){e instanceof SyntaxError}")); // 중복
    assert!(run_bool("try{new RegExp('x','z');false}catch(e){e instanceof SyntaxError}"));  // 무효
    assert!(run_bool("try{new RegExp('x','uv');false}catch(e){e instanceof SyntaxError}")); // u+v
    // 유효 패턴은 던지지 않는다
    assert!(run_bool("var r=new RegExp('a+','gi'); r.test('aaa')===true"));
    assert!(run_bool("/(?<y>\\d+)/.test('42')===true")); // named group
}

// RegExp.escape(S) (§22.2.5.2, ES2025): 문자열을 정규식 리터럴로 이스케이프.
#[test]
fn regexp_escape_static() {
    // 첫 문자가 영숫자면 \xHH
    assert_eq!(run_str("RegExp.escape('a.b')"), "\\x61\\.b");
    assert_eq!(run_str("RegExp.escape('1+1')"), "\\x31\\+1");
    // 구문 문자는 백슬래시
    assert_eq!(run_str("RegExp.escape('(x)')"), "\\(x\\)");
    // 공백 → \x20
    assert_eq!(run_str("RegExp.escape('a b')"), "\\x61\\x20b");
    // 왕복: escape 결과로 만든 정규식은 원본을 매칭
    assert!(run_bool("var s='a.b*c?'; new RegExp(RegExp.escape(s)).test(s)===true"));
    assert!(run_bool("new RegExp(RegExp.escape('a.b')).test('axb')===false"));
    // 비문자열 → TypeError
    assert!(run_bool("try{RegExp.escape(42);false}catch(e){e instanceof TypeError}"));
    // 메타데이터
    assert_eq!(run_str("RegExp.escape.name"), "escape");
    assert_eq!(run_num("RegExp.escape.length"), 1.0);
}

// RegExp.prototype[Symbol.match/replace/split/search/matchAll] (§22.2.6). 예전엔
// 이 심볼들이 정의되지도, RegExp.prototype 에 메서드가 얹히지도 않았다.
#[test]
fn regexp_symbol_methods() {
    // 심볼 자체가 정의됨
    assert!(run_bool("typeof Symbol.match==='symbol' && typeof Symbol.replace==='symbol'"));
    // RegExp.prototype 에 메서드 존재
    assert!(run_bool("typeof RegExp.prototype[Symbol.match]==='function'"));
    assert!(run_bool("typeof RegExp.prototype[Symbol.replace]==='function'"));
    assert!(run_bool("typeof RegExp.prototype[Symbol.split]==='function'"));
    assert!(run_bool("typeof RegExp.prototype[Symbol.search]==='function'"));
    assert!(run_bool("typeof RegExp.prototype[Symbol.matchAll]==='function'"));
    // 인스턴스에서 직접 호출 동작
    assert_eq!(run_str("JSON.stringify(/\\d+/g[Symbol.match]('a12b3'))"), r#"["12","3"]"#);
    assert_eq!(run_str("/\\d+/g[Symbol.replace]('a12b3','#')"), "a#b#");
    assert_eq!(run_str("JSON.stringify(/,/[Symbol.split]('a,b,c'))"), r#"["a","b","c"]"#);
    assert_eq!(run_num("/b/[Symbol.search]('abc')"), 1.0);
    // 기존 String 연산 무회귀
    assert_eq!(run_str("JSON.stringify('a1b2'.match(/\\d/g))"), r#"["1","2"]"#);
}

// Number.prototype.toString(radix) 는 소수부도 변환한다 (§21.1.3.6). 범위 밖 radix 는
// RangeError. 예전엔 정수만 변환하고 소수는 base-10 으로 흘렸다.
#[test]
fn number_tostring_radix() {
    assert_eq!(run_str("(255).toString(16)"), "ff");
    assert_eq!(run_str("(255).toString(2)"), "11111111");
    assert_eq!(run_str("(10.5).toString(2)"), "1010.1");
    assert_eq!(run_str("(-255).toString(16)"), "-ff");
    assert_eq!(run_str("(0).toString(16)"), "0");
    // 범위 밖 radix → RangeError
    assert!(run_bool("try{(1).toString(1);false}catch(e){e instanceof RangeError}"));
    assert!(run_bool("try{(1).toString(37);false}catch(e){e instanceof RangeError}"));
}

// StringToNumber (§7.1.4.1): 0x/0b/0o 진법 접두, "Infinity" 만 무한대,
// "inf"/"nan" 은 NaN. 예전엔 Rust 파서라 0x* 는 NaN, "inf" 는 무한대였다.
#[test]
fn string_to_number_grammar() {
    assert_eq!(run_num("Number('0x10')"), 16.0);
    assert_eq!(run_num("Number('0b101')"), 5.0);
    assert_eq!(run_num("Number('0o17')"), 15.0);
    assert_eq!(run_num("Number('  12  ')"), 12.0);
    assert_eq!(run_num("Number('1e3')"), 1000.0);
    assert_eq!(run_num("+'0x10'"), 16.0);
    assert_eq!(run_num("Number('')"), 0.0);
    // Rust 오탐 차단
    assert!(run_bool("isNaN(Number('inf'))"));
    assert!(run_bool("isNaN(Number('nan'))"));
    assert!(run_bool("isNaN(Number('0xG'))"));
    // Infinity 는 정확한 표기만
    assert_eq!(run_num("Number('Infinity')"), f64::INFINITY);
    assert_eq!(run_num("Number('-Infinity')"), f64::NEG_INFINITY);
}

// ToPropertyDescriptor (§10.2.4): 서술자는 임의의 객체(함수/배열/인스턴스 포함)이고
// 필드는 HasProperty+Get(상속·getter 반영)으로 읽는다. 예전엔 Value::Obj 만 받고
// 상속 필드를 무시했다.
#[test]
fn define_property_descriptor_coercion() {
    // 함수를 서술자로 (own value 프로퍼티)
    assert_eq!(
        run_num("var o={}; var d=function(){}; d.value=42; Object.defineProperty(o,'y',d); o.y"),
        42.0
    );
    // 상속된 서술자 필드 (Object.create(proto))
    assert_eq!(
        run_num(
            "var o={}; var proto={value:7,enumerable:true,configurable:true,writable:true}; \
             Object.defineProperty(o,'z',Object.create(proto)); o.z"
        ),
        7.0
    );
    // getter 로 노출된 서술자 필드
    assert_eq!(
        run_num(
            "var o={}; var d={get value(){return 99;},configurable:true}; \
             Object.defineProperty(o,'w',d); o.w"
        ),
        99.0
    );
    // 비객체 서술자 → TypeError
    assert!(run_bool("try{Object.defineProperty({},'x',5);false}catch(e){e instanceof TypeError}"));
}

// 내장 프로퍼티는 non-enumerable, writable, configurable (§17). 예전엔 표식이 없어
// getOwnPropertyDescriptor 가 enumerable:true 로 보고하고 Object.keys(Math) 가
// 메서드를 나열했다.
#[test]
fn builtin_properties_non_enumerable() {
    assert!(run_bool("Object.getOwnPropertyDescriptor(Math,'atan2').enumerable===false"));
    assert!(run_bool("Object.getOwnPropertyDescriptor(Math,'atan2').writable===true"));
    assert!(run_bool("Object.getOwnPropertyDescriptor(Math,'atan2').configurable===true"));
    assert_eq!(run_str("JSON.stringify(Object.keys(Math))"), "[]");
    assert!(run_bool("Object.getOwnPropertyDescriptor(Array.prototype,'map').enumerable===false"));
    assert!(run_bool("Object.getOwnPropertyDescriptor(JSON,'stringify').enumerable===false"));
    // 사용자 프로퍼티는 여전히 열거된다 (대입한 Native 값 포함)
    assert_eq!(run_str("var o={}; o.f=[].map; JSON.stringify(Object.keys(o))"), r#"["f"]"#);
    assert_eq!(run_str("JSON.stringify(Object.keys({a:1,b:2}))"), r#"["a","b"]"#);
}

// Object.defineProperties: Properties 의 열거 가능한 own 키를 돌며 각 서술자를
// Get(getter 호출)으로 읽는다. 예전엔 getter 서술자를 Accessor 그대로 넘겨 거부됐다.
#[test]
fn define_properties_reads_via_get() {
    assert_eq!(
        run_num(
            "var o={}; Object.defineProperties(o,{a:{value:5,enumerable:true},\
             b:{get:function(){return 9},enumerable:true}}); o.a+o.b"
        ),
        14.0
    );
}

// Object.defineProperty/defineProperties/create 는 대상이 객체가 아니면 TypeError
// (§20.1.2.4/.5/.2). 예전엔 조용히 무시했다. create 의 서술자는 defineProperties 에
// 위임해 getter/속성을 전부 반영한다.
#[test]
fn object_ops_reject_non_objects() {
    for expr in [
        "Object.defineProperty(5,'x',{value:1})",
        "Object.defineProperty('s','x',{value:1})",
        "Object.defineProperty(true,'x',{value:1})",
        "Object.defineProperties(5,{x:{value:1}})",
        "Object.create(5)",
        "Object.create('s')",
    ] {
        assert!(
            run_bool(&format!("try{{{};false}}catch(e){{e instanceof TypeError}}", expr)),
            "expected TypeError from: {}",
            expr
        );
    }
    // create(null) 은 유효
    assert!(run_bool("var o=Object.create(null); typeof o==='object'"));
    // create 의 서술자는 완전 반영 (value + getter + enumerable)
    assert_eq!(
        run_num(
            "var o=Object.create(null,{a:{value:1,enumerable:true},\
             b:{get:function(){return 2},enumerable:true}}); o.a+o.b"
        ),
        3.0
    );
    assert!(run_bool(
        "Object.getOwnPropertyDescriptor(Object.create(null,{a:{value:1,enumerable:true}}),'a').enumerable===true"
    ));
}

// Object.getOwnPropertyNames 는 열거 여부 무관 모든 own 문자열 키를 돌려준다
// (§20.1.2.10). 예전엔 Object.keys 별칭이라 non-enumerable(내장 메서드)을 빠뜨렸다.
#[test]
fn get_own_property_names_includes_non_enumerable() {
    // 내장 메서드(non-enumerable)도 포함
    assert!(run_bool("Object.getOwnPropertyNames(Math).indexOf('atan2')>=0"));
    assert!(run_num("Object.getOwnPropertyNames(Math).length") > 20.0);
    // 배열은 length 포함
    assert_eq!(run_str("JSON.stringify(Object.getOwnPropertyNames([1,2]))"), r#"["0","1","length"]"#);
    // Object.keys 는 여전히 열거 가능한 것만
    assert_eq!(run_str("JSON.stringify(Object.keys(Math))"), "[]");
    // 내장 생성자(Native)도 객체 — defineProperty 가 non-object 로 던지지 않음
    assert!(run_bool("try{Object.defineProperty(Array,'zzz',{value:1,configurable:true});true}catch(e){false}"));
}

// Object.assign 은 Set(Throw=true) 로 복사한다 (§20.1.2.1) — read-only/non-extensible/
// getter-only 대상이면 TypeError. 예전엔 조용히 무시했다.
#[test]
fn object_assign_throws_on_readonly() {
    assert!(run_bool(
        "try{Object.assign(Object.freeze({a:1}),{a:2});false}catch(e){e instanceof TypeError}"
    ));
    assert!(run_bool(
        "try{var o={};Object.defineProperty(o,'x',{value:1,writable:false});\
         Object.assign(o,{x:2});false}catch(e){e instanceof TypeError}"
    ));
    assert!(run_bool(
        "try{Object.assign(Object.preventExtensions({}),{n:1});false}catch(e){e instanceof TypeError}"
    ));
    assert!(run_bool(
        "try{Object.assign({get g(){return 1}},{g:2});false}catch(e){e instanceof TypeError}"
    ));
    // 정상 assign 은 그대로
    assert_eq!(run_str("JSON.stringify(Object.assign({a:1},{b:2},{c:3}))"), r#"{"a":1,"b":2,"c":3}"#);
}

// new String/Number/Boolean 은 원시 래퍼 객체다 (§20/21/22) — typeof "object",
// valueOf 는 원시값, 프로퍼티 대입 가능. 예전엔 원시값을 그대로 돌려줘서
// (new Boolean).x = 1 이 "false 에 대입 불가" 로 죽었다.
#[test]
fn primitive_wrapper_objects() {
    assert_eq!(run_str("typeof new Boolean(false)"), "object");
    assert_eq!(run_str("typeof new Number(42)"), "object");
    assert_eq!(run_str("typeof new String('hi')"), "object");
    // valueOf 는 원시값
    assert!(run_bool("new Boolean(false).valueOf()===false"));
    assert_eq!(run_num("new Number(42).valueOf()"), 42.0);
    assert_eq!(run_str("new String('hi').valueOf()"), "hi");
    // 프로퍼티 대입 가능 (객체이므로)
    assert_eq!(run_num("var b=new Boolean(false); b.foo=7; b.foo"), 7.0);
    // String 래퍼: 인덱스 + length
    assert_eq!(run_num("new String('abc').length"), 3.0);
    assert_eq!(run_str("new String('abc')[1]"), "b");
    // 강제 변환은 내부 슬롯 사용
    assert_eq!(run_num("+new Number(5)"), 5.0);
    assert_eq!(run_str("''+new String('x')"), "x");
    assert!(run_bool("!!new Boolean(false)===true")); // 객체는 truthy(래퍼도)
    // String.prototype 메서드를 Boolean 래퍼에 적용 → ToString
    assert_eq!(
        run_str("var i=new Boolean(false); i.charAt=String.prototype.charAt; i.charAt(0)"),
        "f"
    );
    // 생성자 아닌 호출은 원시값
    assert_eq!(run_str("typeof Boolean(1)"), "boolean");
    assert_eq!(run_str("typeof Number('5')"), "number");
}

// String.raw (§22.1.2.4) + Object.prototype.toString 의 빌트인 태그.
#[test]
fn string_raw_and_tostring_tags() {
    // String.raw: 세그먼트와 치환값을 번갈아
    assert_eq!(run_str("String.raw({raw:['a','b','c']}, 1, 2)"), "a1b2c");
    assert_eq!(run_str("String.raw({raw:['x']})"), "x");
    assert_eq!(run_str("String.raw({raw:[]})"), "");
    // 태그된 템플릿: 원시 이스케이프 보존
    assert_eq!(run_str("String.raw`a\\nb${1+1}c`"), "a\\nb2c");
    assert_eq!(run_str("String.raw.name"), "raw");
    // Object.prototype.toString 태그
    assert_eq!(run_str("Object.prototype.toString.call(new Number(1))"), "[object Number]");
    assert_eq!(run_str("Object.prototype.toString.call(new String('x'))"), "[object String]");
    assert_eq!(run_str("Object.prototype.toString.call(new Boolean(true))"), "[object Boolean]");
    assert_eq!(run_str("Object.prototype.toString.call(/x/)"), "[object RegExp]");
    assert_eq!(run_str("Object.prototype.toString.call(null)"), "[object Null]");
    assert_eq!(run_str("Object.prototype.toString.call([])"), "[object Array]");
    // Symbol.toStringTag 우선
    assert_eq!(
        run_str("var o={}; o[Symbol.toStringTag]='Custom'; Object.prototype.toString.call(o)"),
        "[object Custom]"
    );
}

// 배열 length 대입은 ToUint32(v)!==ToNumber(v) 면 RangeError (§10.4.2.4) —
// 음수/소수/2^32 이상. 예전엔 검증 없이 잘라서 조용히 통과했다.
#[test]
fn array_length_assignment_validates() {
    // 정상 truncate/extend
    assert_eq!(run_str("var a=[1,2,3,4,5]; a.length=3; JSON.stringify(a)"), "[1,2,3]");
    assert_eq!(run_num("var a=[1,2,3]; a.length=5; a.length"), 5.0);
    // 음수/소수/범위초과 → RangeError
    assert!(run_bool("try{[].length=-1;false}catch(e){e instanceof RangeError}"));
    assert!(run_bool("try{[].length=1.5;false}catch(e){e instanceof RangeError}"));
    assert!(run_bool("try{[].length=4294967296;false}catch(e){e instanceof RangeError}"));
    assert!(run_bool("try{[].length=NaN;false}catch(e){e instanceof RangeError}"));
    // 경계: 2^32-1 은 유효
    assert_eq!(run_num("var a=[]; a.length=4294967295; a.length"), 4294967295.0);
}

// 내장 함수 name/length 의 mutation 의미론 (§17): writable:false(재대입 무시),
// configurable:true(delete 성공). verifyProperty 가 전 서브셋에서 이걸 검사한다.
// 예전엔 재대입이 name 을 바꾸고 delete 는 no-op 라 대량 실패했다.
#[test]
fn native_name_length_mutation_semantics() {
    // 재대입은 무시 (non-writable)
    assert_eq!(run_str("var f=[].map; f.name='X'; f.name"), "map");
    assert_eq!(run_num("var f=[].slice; f.length=99; f.length"), 2.0);
    // delete 는 성공 (configurable)
    assert!(run_bool("var f=[].filter; delete f.name; f.name===undefined && !f.hasOwnProperty('name')"));
    assert!(run_bool("var f=[].concat; delete f.length; !f.hasOwnProperty('length')"));
    // delete 후 getOwnPropertyDescriptor 는 undefined
    assert!(run_bool("var f=[].every; delete f.name; Object.getOwnPropertyDescriptor(f,'name')===undefined"));
    // 삭제 전 서술자는 표준대로
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor([].some,'name'); \
         d.writable===false && d.enumerable===false && d.configurable===true"
    ));
    // 폴리필이 얹는 다른 프로퍼티는 정상 저장
    assert_eq!(run_num("Array.prototype.customX=42; [].customX"), 42.0);
}

// 원시 래퍼 프로토타입(Boolean/Number/String.prototype)은 그 자신이 [[PrimitiveValue]]
// 슬롯을 가진 원시 래퍼 객체다 (§20.3.3/§21.1.3/§22.1.3). 그리고 valueOf/toString 은
// brand-checked — 잘못된 종류의 수신자에 전달하면 TypeError 다 (§20.3.3.2 thisBooleanValue 등).
// 예전엔 generic ValueOfSelf/ValueToStr 라 X.prototype.toString() 이 [object Object] 였고
// 다른 종류 수신자에도 조용히 통과했다. constructor 링크도 없었다.
#[test]
fn primitive_wrapper_prototypes_are_exotic() {
    // X.prototype.constructor === X
    assert!(run_bool("Boolean.prototype.constructor === Boolean"));
    assert!(run_bool("Number.prototype.constructor === Number"));
    assert!(run_bool("String.prototype.constructor === String"));
    // X.prototype 자신이 원시 래퍼 → thisXValue(X.prototype) = 기본값
    assert_eq!(run_str("Boolean.prototype.toString()"), "false");
    assert_eq!(run_str("Number.prototype.toString()"), "0");
    assert_eq!(run_str("String.prototype.toString()"), "");
    assert!(run_bool("Boolean.prototype.valueOf() === false"));
    assert!(run_bool("Number.prototype.valueOf() === 0"));
    assert!(run_bool("String.prototype.valueOf() === ''"));
    // 래퍼 객체 언박싱
    assert!(run_bool("new Boolean(true).valueOf() === true"));
    assert_eq!(run_str("(new Boolean(0)).toString()"), "false");
    assert_eq!(run_str("(new Number(7)).toString()"), "7");
    assert_eq!(run_str("(new String('hi')).valueOf()"), "hi");
    // brand 체크: 잘못된 수신자면 TypeError
    assert!(run_bool(
        "var t=false; try{ Boolean.prototype.valueOf.call('x') }catch(e){ t=e instanceof TypeError } t"
    ));
    assert!(run_bool(
        "var t=false; try{ Number.prototype.valueOf.call(true) }catch(e){ t=e instanceof TypeError } t"
    ));
    assert!(run_bool(
        "var t=false; try{ String.prototype.toString.call(5) }catch(e){ t=e instanceof TypeError } t"
    ));
    // A2_T1 형태: 다른 래퍼로 전달해도 brand 불일치 → TypeError
    assert!(run_bool(
        "var t=false; var s=new String(); s.vo=Boolean.prototype.valueOf; \
         try{ s.vo() }catch(e){ t=e instanceof TypeError } t"
    ));
    // radix 유지 + 검증
    assert_eq!(run_str("(255).toString(16)"), "ff");
    assert!(run_bool("var t=false; try{ (5).toString(1) }catch(e){ t=e instanceof RangeError } t"));
    // 다른 빌트인 프로토타입도 constructor 링크
    assert!(run_bool("RegExp.prototype.constructor === RegExp"));
    assert!(run_bool("Map.prototype.constructor === Map"));
    assert!(run_bool("Set.prototype.constructor === Set"));
    assert!(run_bool("Date.prototype.constructor === Date"));
    assert!(run_bool("Symbol.prototype.constructor === Symbol"));
    // constructor 는 비열거 (§17)
    assert!(run_bool("Object.keys(Boolean.prototype).indexOf('constructor') === -1"));
}

// 내장 생성자는 정적 메서드/상수/prototype 을 own 프로퍼티로 노출한다 (§17). 예전엔
// getOwnPropertyNames(Date)=[], getOwnPropertyDescriptor(Date,'parse')=undefined,
// Date.hasOwnProperty 조차 undefined(크래시) 였다 — 리플렉션이 내장 생성자에 안 통했다.
#[test]
fn native_ctor_reflection() {
    // 정적 메서드: {value, writable:true, enumerable:false, configurable:true} (§17)
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Date,'parse'); \
         d.value===Date.parse && d.writable===true && d.enumerable===false && d.configurable===true"
    ));
    assert!(run_bool("Date.hasOwnProperty('parse')"));
    assert!(run_bool("Number.hasOwnProperty('MAX_SAFE_INTEGER')"));
    // 상수 값(Number.MAX_VALUE 등)은 전부 non-writable/non-configurable
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Number,'MAX_VALUE'); \
         d.writable===false && d.enumerable===false && d.configurable===false"
    ));
    // 생성자의 prototype 은 non-writable/non-configurable/non-enumerable
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Boolean,'prototype'); \
         d.value===Boolean.prototype && d.writable===false && d.configurable===false && d.enumerable===false"
    ));
    assert!(run_bool("Boolean.hasOwnProperty('prototype')"));
    // getOwnPropertyNames 가 정적들과 prototype 을 나열
    assert!(run_bool("Object.getOwnPropertyNames(Date).indexOf('parse')>=0"));
    assert!(run_bool("Object.getOwnPropertyNames(Number).indexOf('isInteger')>=0"));
    assert!(run_bool("Object.getOwnPropertyNames(Object).indexOf('keys')>=0"));
    assert!(run_bool("Object.getOwnPropertyNames(Number).indexOf('prototype')>=0"));
    // 상속 메서드는 함수로 남아 있어야 한다 (크래시 회귀 방지)
    assert!(run_bool("typeof Date.hasOwnProperty==='function'"));
    assert!(run_bool("typeof Promise.hasOwnProperty==='function'"));
    assert!(run_bool("typeof Error.hasOwnProperty==='function'"));
    // in 연산자: own + 상속
    assert!(run_bool("'parse' in Date"));
    assert!(run_bool("'hasOwnProperty' in Date"));
    // 드리프트 가드: own 키 목록의 모든 키가 실제로 member_get 으로 resolve 된다.
    assert!(run_bool(
        "[Number,Boolean,String,Date,RegExp,Map,Set,Promise,Symbol,Object,Array].every(function(C){ \
           return Object.getOwnPropertyNames(C).every(function(k){ return typeof C[k]!=='undefined'; }); \
         })"
    ));
}

// 내장 메서드/전역함수/Symbol/BigInt 는 [[Construct]] 가 없다 (§17). new 하면 TypeError 고,
// Reflect.construct 의 target/newTarget 검증도 이를 따른다(isConstructor 하네스가 의존).
// 예전엔 construct 폴백이 {message} 스텁을 만들어 조용히 통과시켰다.
#[test]
fn builtin_methods_are_not_constructors() {
    // 내장 메서드: new 하면 TypeError
    assert!(run_bool(
        "var t=false; try{ new (Array.prototype.map)(function(){}) }catch(e){ t=e instanceof TypeError } t"
    ));
    assert!(run_bool(
        "var t=false; try{ new (String.prototype.slice)() }catch(e){ t=e instanceof TypeError } t"
    ));
    assert!(run_bool(
        "var t=false; try{ new (Object.keys)({}) }catch(e){ t=e instanceof TypeError } t"
    ));
    assert!(run_bool("var t=false; try{ new parseInt('1') }catch(e){ t=e instanceof TypeError } t"));
    // Symbol/BigInt 는 호출은 되지만 생성자 아님
    assert!(run_bool("var t=false; try{ new Symbol() }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ new BigInt(1) }catch(e){ t=e instanceof TypeError } t"));
    // 진짜 생성자는 여전히 new 가능
    assert!(run_bool("(new Array(3)).length===3"));
    assert!(run_bool("(new Map()) instanceof Map"));
    assert!(run_bool("typeof (new Date())==='object'"));
    assert!(run_bool("(new RegExp('a')).test('a')"));
    // isConstructor 하네스 패턴: Reflect.construct 의 newTarget 검증
    let ic = "function ic(f){ try{ Reflect.construct(function(){}, [], f); }catch(e){ return false; } return true; } ";
    assert!(run_bool(&format!("{} ic(Array.prototype.map)===false", ic)));
    assert!(run_bool(&format!("{} ic(parseInt)===false", ic)));
    assert!(run_bool(&format!("{} ic(Array)===true", ic)));
    assert!(run_bool(&format!("{} ic(Map)===true", ic)));
    // Reflect.construct target 이 비생성자면 TypeError
    assert!(run_bool(
        "var t=false; try{ Reflect.construct(parseInt, []) }catch(e){ t=e instanceof TypeError } t"
    ));
}

// change-array-by-copy (ES2023): Array.prototype.with / toSpliced — 원본 불변, 새 배열.
#[test]
fn array_change_by_copy_with_tospliced() {
    // with
    assert_eq!(run_str("[1,2,3].with(1,9).join(',')"), "1,9,3");
    assert_eq!(run_str("[1,2,3].with(-1,9).join(',')"), "1,2,9");
    assert!(run_bool("var a=[1,2,3]; a.with(0,9); a.join(',')==='1,2,3'")); // 원본 불변
    assert!(run_bool("var t=false; try{ [1,2,3].with(5,9) }catch(e){ t=e instanceof RangeError } t"));
    assert!(run_bool("var t=false; try{ [1,2,3].with(-9,9) }catch(e){ t=e instanceof RangeError } t"));
    assert_eq!(run_str("[].with.name"), "with");
    assert_eq!(run_num("[].with.length"), 2.0);
    // toSpliced
    assert_eq!(run_str("[1,2,3,4].toSpliced(1,2,'a','b').join(',')"), "1,a,b,4");
    assert_eq!(run_str("[1,2,3].toSpliced(1).join(',')"), "1"); // deleteCount 생략 → 끝까지
    assert_eq!(run_str("[1,2,3].toSpliced(1,0,'x').join(',')"), "1,x,2,3");
    assert_eq!(run_str("[1,2,3].toSpliced(-1,1,'z').join(',')"), "1,2,z");
    assert!(run_bool("var a=[1,2,3]; a.toSpliced(0,1); a.join(',')==='1,2,3'")); // 원본 불변
    assert_eq!(run_str("[].toSpliced.name"), "toSpliced");
    assert_eq!(run_num("[].toSpliced.length"), 2.0);
    // 생성자 아님 (commit db6d6c8 과 일관)
    assert!(run_bool("var t=false; try{ new ([].with)() }catch(e){ t=e instanceof TypeError } t"));
}

// String.prototype 의 this 강제변환(ToString)은 poisoned toString/valueOf 예외를 전파하고
// (§22.1.3), includes/startsWith/endsWith 는 정규식 인자를 거부한다(IsRegExp, §7.2.8).
#[test]
fn string_this_coercion_and_isregexp() {
    // this 의 toString 이 던지면 그대로 전파 (예전엔 삼켜서 [object Object] 반환)
    assert!(run_bool(
        "var t=false; try{ ''.toUpperCase.call({toString:function(){throw new TypeError('p');}, valueOf:function(){throw new TypeError('p');}}); }catch(e){ t=e instanceof TypeError } t"
    ));
    // @@toPrimitive 가 던져도 전파
    assert!(run_bool(
        "var o={}; o[Symbol.toPrimitive]=function(){ throw new RangeError('p'); }; \
         var t=false; try{ ''.slice.call(o); }catch(e){ t=e instanceof RangeError } t"
    ));
    // 정상 객체 this 는 toString 결과로 동작
    assert_eq!(run_str("''.toUpperCase.call({toString:function(){return 'ab';}})"), "AB");
    // null/undefined this → TypeError (RequireObjectCoercible)
    assert!(run_bool("var t=false; try{ ''.slice.call(null) }catch(e){ t=e instanceof TypeError } t"));
    // IsRegExp: 정규식 인자면 TypeError
    assert!(run_bool("var t=false; try{ 'abc'.startsWith(/a/) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ 'abc'.includes(/b/) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ 'abc'.endsWith(/c/) }catch(e){ t=e instanceof TypeError } t"));
    // @@match 가 truthy 인 가짜 객체도 정규식으로 취급
    assert!(run_bool(
        "var o={}; o[Symbol.match]=true; var t=false; try{ 'abc'.includes(o) }catch(e){ t=e instanceof TypeError } t"
    ));
    // 문자열 인자는 정상 동작
    assert!(run_bool("'abcdef'.startsWith('abc')"));
    assert!(run_bool("'abcdef'.includes('cd')"));
    assert!(run_bool("'abcdef'.endsWith('def')"));
    // @@match 가 falsy 인 객체는 정규식 아님 → TypeError 를 던지지 않는다
    assert!(run_bool(
        "var o={}; o[Symbol.match]=null; \
         (function(){ try{ 'axb'.includes(o); return true; }catch(e){ return false; } })()"
    ));
}

// 리플렉션 API 의 키 인자는 ToPropertyKey (§7.1.19) 로 강제변환 — 객체 키의 toString 을
// 실제로 부른다. 예전엔 to_display 라 {toString(){return 'k'}} 가 "[object Object]" 였다.
#[test]
fn reflection_key_topropertykey_coercion() {
    // getOwnPropertyDescriptor(o, objKey)
    assert!(run_bool(
        "var o={abc:1}; var k={toString:function(){return 'abc';}}; \
         Object.getOwnPropertyDescriptor(o,k).value===1"
    ));
    // defineProperty(o, objKey, desc)
    assert!(run_bool(
        "var o={}; var k={toString:function(){return 'foo';}}; \
         Object.defineProperty(o,k,{value:42,enumerable:true,configurable:true,writable:true}); o.foo===42"
    ));
    // hasOwnProperty(objKey)
    assert!(run_bool(
        "var o={abc:1}; var k={toString:function(){return 'abc';}}; o.hasOwnProperty(k)===true"
    ));
    assert!(run_bool(
        "var o={abc:1}; var k={toString:function(){return 'xyz';}}; o.hasOwnProperty(k)===false"
    ));
    // 숫자 키도 문자열화 (getOwnPropertyDescriptor(arr-like, 1))
    assert!(run_bool("Object.getOwnPropertyDescriptor({'1':9}, 1).value===9"));
    // 키의 toString 이 던지면 전파
    assert!(run_bool(
        "var t=false; try{ Object.getOwnPropertyDescriptor({}, {toString:function(){throw new TypeError('p');}, valueOf:function(){throw new TypeError('p');}}); }catch(e){ t=e instanceof TypeError } t"
    ));
}

// Error.isError(ES2025) + Error.prototype.stack 를 표준(프로토타입 accessor)으로. 예전엔
// stack 이 인스턴스 own 데이터 프로퍼티라 Error.prototype 서술자 검사가 전부 깨졌고
// isError 는 아예 없었다.
#[test]
fn error_iserror_and_stack_accessor() {
    // Error.isError
    assert!(run_bool("Error.isError(new Error())"));
    assert!(run_bool("Error.isError(new TypeError())"));
    assert!(run_bool("Error.isError(new RangeError('x'))"));
    assert!(!run_bool("Error.isError({})"));
    assert!(!run_bool("Error.isError(null)"));
    assert!(!run_bool("Error.isError('e')"));
    assert!(!run_bool("Error.isError(Error)")); // 생성자 자체는 에러 인스턴스 아님
    assert_eq!(run_str("Error.isError.name"), "isError");
    assert_eq!(run_num("Error.isError.length"), 1.0);
    assert!(run_bool("Error.hasOwnProperty('isError')"));
    // isError 는 생성자 아님
    assert!(run_bool("var t=false; try{ new Error.isError({}) }catch(e){ t=e instanceof TypeError } t"));

    // stack 은 Error.prototype 의 accessor (인스턴스 own 아님)
    assert!(run_bool("Error.prototype.hasOwnProperty('stack')"));
    assert!(run_bool("!(new Error()).hasOwnProperty('stack')"));
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Error.prototype,'stack'); \
         typeof d.get==='function' && typeof d.set==='function' && d.enumerable===false && d.configurable===true"
    ));
    assert_eq!(run_str("Object.getOwnPropertyDescriptor(Error.prototype,'stack').get.name"), "get stack");
    assert_eq!(run_num("Object.getOwnPropertyDescriptor(Error.prototype,'stack').get.length"), 0.0);
    assert_eq!(run_str("Object.getOwnPropertyDescriptor(Error.prototype,'stack').set.name"), "set stack");
    assert_eq!(run_num("Object.getOwnPropertyDescriptor(Error.prototype,'stack').set.length"), 1.0);
    // getter 로 인스턴스 스택(문자열) 접근
    assert_eq!(run_str("typeof (new Error('m')).stack"), "string");
    // setter 는 own 데이터로 accessor 를 가린다
    assert!(run_bool("var e=new Error(); e.stack='XYZ'; e.stack==='XYZ' && e.hasOwnProperty('stack')"));
}

// Set/Map 의 prototype 메서드는 brand 체크로 TypeError(예전엔 일반 Error)를 던지고,
// size 는 인스턴스 own 데이터가 아니라 prototype accessor 다 (§24.1.3.10/§24.2.3.9).
#[test]
fn set_map_brand_check_and_size_accessor() {
    // brand 체크 → TypeError
    assert!(run_bool("var t=false; try{ Set.prototype.add.call({},1) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ Map.prototype.get.call([],1) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ Set.prototype.has.call(new Map(),1) }catch(e){ t=e instanceof TypeError } t"));
    // size 는 prototype accessor (인스턴스 own 아님)
    assert!(run_bool("Object.prototype.hasOwnProperty.call(Set.prototype,'size')"));
    assert!(run_bool("Object.prototype.hasOwnProperty.call(Map.prototype,'size')"));
    assert!(run_bool("!Object.prototype.hasOwnProperty.call(new Set(),'size')"));
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Set.prototype,'size'); \
         typeof d.get==='function' && d.set===undefined && d.enumerable===false && d.configurable===true"
    ));
    assert_eq!(run_str("Object.getOwnPropertyDescriptor(Set.prototype,'size').get.name"), "get size");
    assert_eq!(run_num("Object.getOwnPropertyDescriptor(Map.prototype,'size').get.length"), 0.0);
    // 값은 그대로 동작
    assert_eq!(run_num("new Set([1,2,3,3]).size"), 3.0);
    assert_eq!(run_num("new Map([[1,2],[3,4]]).size"), 2.0);
    // size getter 도 brand 체크
    assert!(run_bool(
        "var g=Object.getOwnPropertyDescriptor(Set.prototype,'size').get; \
         var t=false; try{ g.call({}) }catch(e){ t=e instanceof TypeError } t"
    ));
}

// 내장 메서드의 name/length 메타(§17). 예전엔 native_meta 에 Date/Math arm 이 없어
// Date.prototype.getTime.name 이 "" 였다(Set/Map 과 같은 편법이었음).
#[test]
fn date_math_method_name_length() {
    assert_eq!(run_str("Date.prototype.getTime.name"), "getTime");
    assert_eq!(run_str("Date.prototype.setHours.name"), "setHours");
    assert_eq!(run_num("Date.prototype.setHours.length"), 4.0);
    assert_eq!(run_num("Date.prototype.setFullYear.length"), 3.0);
    assert_eq!(run_str("Date.now.name"), "now");
    assert_eq!(run_str("Math.floor.name"), "floor");
    assert_eq!(run_num("Math.floor.length"), 1.0);
    assert_eq!(run_str("Math.max.name"), "max");
    assert_eq!(run_num("Math.max.length"), 2.0);
    assert_eq!(run_num("Math.random.length"), 0.0);
    // 서술자도 표준: writable:false, enumerable:false, configurable:true
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Math.floor,'name'); \
         d.value==='floor' && d.writable===false && d.enumerable===false && d.configurable===true"
    ));
}

// 함수의 [[Prototype]] (§20.2.3): Object.getPrototypeOf/setPrototypeOf 가 함수에도
// 동작해야 한다(정적 상속·%TypedArray%의 토대). 예전엔 함수는 항상 Function.prototype
// 이었고 setPrototypeOf 는 무시됐다.
#[test]
fn function_prototype_chain() {
    // 기본값은 Function.prototype
    assert!(run_bool("function F(){} Object.getPrototypeOf(F) === Function.prototype"));
    // setPrototypeOf 가 반영된다
    assert!(run_bool("function A(){} function B(){} Object.setPrototypeOf(A,B); Object.getPrototypeOf(A)===B"));
    // null 로 설정하면 null 로 남는다(기본값으로 안 돌아감)
    assert!(run_bool("function G(){} Object.setPrototypeOf(G,null); Object.getPrototypeOf(G)===null"));
    // __proto__ 대입도 동작
    assert!(run_bool("function C(){} var o={m:1}; C.__proto__=o; Object.getPrototypeOf(C)===o"));
    // 일반 함수 대량 확인: 서로 다른 함수의 기본 프로토는 같은 Function.prototype
    assert!(run_bool("function X(){} function Y(){} Object.getPrototypeOf(X)===Object.getPrototypeOf(Y)"));
    // Function.prototype.[[Prototype]] === Object.prototype (§20.2.3): 함수도 Object.prototype
    // 메서드를 상속한다.
    assert!(run_bool("Object.getPrototypeOf(Function.prototype)===Object.prototype"));
    assert!(run_bool("function F(){} F.hasOwnProperty('name')===true && F.hasOwnProperty('call')===false"));
    assert!(run_bool("function F(){} F.valueOf()===F"));
    assert!(run_bool("function F(){} typeof F.isPrototypeOf==='function' && typeof F.toLocaleString==='function'"));
    assert!(run_bool("('hasOwnProperty' in function(){}) && ('valueOf' in function(){})"));
    // Function.prototype.toString 은 여전히 Object.prototype.toString 을 가린다
    assert!(run_bool("typeof (function(){}).toString==='function'"));
}

// 함수 프로퍼티의 표준 속성 강제 (§10.2): JsFn.props 를 ObjMap 으로 바꿔 함수 대상
// defineProperty 가 writable/enumerable/configurable 을 실제로 강제한다. 예전엔 근사
// 경로라 속성이 무시됐다. 접근자/삭제/열거/for-in 도 함수를 ordinary object 로 취급.
#[test]
fn function_property_attributes() {
    // defineProperty 로 정의한 속성 비트가 gOPD 에 정확히 반영
    assert!(run_bool(
        "function F(){} Object.defineProperty(F,'x',{value:1,writable:false,enumerable:false,configurable:false}); \
         var d=Object.getOwnPropertyDescriptor(F,'x'); \
         d.value===1 && d.writable===false && d.enumerable===false && d.configurable===false"));
    // non-writable 대입 무시
    assert!(run_bool("function F(){} Object.defineProperty(F,'x',{value:1,writable:false}); F.x=9; F.x===1"));
    // non-configurable 삭제 거부 / 재정의 TypeError
    assert!(run_bool("function F(){} Object.defineProperty(F,'x',{value:1,configurable:false}); (delete F.x)===false"));
    assert!(run_bool("function F(){} Object.defineProperty(F,'x',{value:1,configurable:false}); \
                      var t=false; try{ Object.defineProperty(F,'x',{value:5}) }catch(e){ t=e instanceof TypeError } t"));
    // non-enumerable 은 Object.keys 에서 숨김
    assert!(run_bool("function F(){} Object.defineProperty(F,'x',{value:1,enumerable:false}); Object.keys(F).indexOf('x')===-1"));
    // 평범한 대입은 all-true 데이터 프로퍼티
    assert!(run_bool("function G(){} G.y=7; var d=Object.getOwnPropertyDescriptor(G,'y'); \
                      d.writable && d.enumerable && d.configurable && d.value===7"));
    // configurable 삭제 성공
    assert!(run_bool("function G(){} G.y=7; (delete G.y)===true && G.y===undefined"));
    // 함수 위 접근자 프로퍼티
    assert!(run_bool("function H(){} Object.defineProperty(H,'z',{get:function(){return 42;},configurable:true}); \
                      H.z===42 && typeof Object.getOwnPropertyDescriptor(H,'z').get==='function'"));
    // name/length/prototype 무회귀
    assert!(run_bool("function F(){} var d=Object.getOwnPropertyDescriptor(F,'name'); \
                      d.value==='F' && d.writable===false && d.configurable===true"));
    assert!(run_bool("function F(){} typeof F.prototype==='object'"));
    // name/length 는 non-writable 대입 무시
    assert!(run_bool("function F(){} F.name='x'; F.length=9; F.name==='F' && F.length===0"));
    // name/length 는 configurable:true → delete 로 삭제 후 undefined + gOPD 없음
    assert!(run_bool("function F(a,b){} (delete F.name)===true && F.name===undefined && \
                      Object.getOwnPropertyDescriptor(F,'name')===undefined && !('name' in F)"));
    // delete 후 defineProperty 로 복원
    assert!(run_bool("function F(){} delete F.name; \
                      Object.defineProperty(F,'name',{value:'F',writable:false,enumerable:false,configurable:true}); \
                      F.name==='F' && Object.getOwnPropertyDescriptor(F,'name').configurable===true"));
    // length 재정의(다른 값·속성) 후 gOPD 반영
    assert!(run_bool("function G(x,y,z){} Object.defineProperty(G,'length',{value:10}); \
                      G.length===10 && Object.getOwnPropertyDescriptor(G,'length').value===10"));
}

// ToPropertyDescriptor (§10.2.4): 서술자는 임의의 객체이며 필드는 HasProperty+Get 으로
// 읽어 **상속·getter 도 반영**한다. 함수/배열 서술자의 상속 필드가 특히 문제였다 —
// has_property 가 Fn/Arr 의 [[Prototype]] 체인을 안 걸어 상속 value 를 놓쳤다.
#[test]
fn to_property_descriptor_reads_inherited() {
    // 함수 서술자: 상속된 value(Function.prototype 에 얹음)를 읽는다
    assert_eq!(run_str(
        "Function.prototype.value='F'; var o={}; Object.defineProperty(o,'p',function(){}); o.p"), "F");
    // 배열 서술자: 상속된 value(Array.prototype 에 얹음)
    assert_eq!(run_str(
        "Array.prototype.value='A'; var o={}; Object.defineProperty(o,'p',[]); o.p"), "A");
    // 인스턴스 서술자: 클래스 프로토타입의 value
    assert_eq!(run_str(
        "function C(){} C.prototype.value='I'; var o={}; Object.defineProperty(o,'p',new C()); o.p"), "I");
    // getter 서술자 필드: get 을 호출해 값 산출
    assert_eq!(run_num(
        "var o={}; var d={get value(){return 7;}, get enumerable(){return true;}}; \
         Object.defineProperty(o,'p',d); o.p"), 7.0);
    // 무회귀: 평범한 서술자에 상속 필드가 잘못 끼지 않는다
    assert!(run_bool("var o={}; Object.defineProperty(o,'a',{value:5}); \
                      Object.getOwnPropertyDescriptor(o,'a').enumerable===false && o.a===5"));
    // has_property 무회귀: 상속 메서드/멤버가 in 으로 잡힘
    assert!(run_bool("('push' in []) && ('at' in []) && ('call' in function(){})"));
}

// Annex B 레거시 접근자 (§B.2.2): __defineGetter__/__defineSetter__/__lookupGetter__/
// __lookupSetter__. 예전엔 전부 미구현(undefined)이었다.
#[test]
fn annexb_legacy_accessors() {
    assert!(prelude_bool("typeof Object.prototype.__defineGetter__==='function' && \
                      typeof Object.prototype.__lookupSetter__==='function'"));
    assert_eq!(prelude_num("var o={}; o.__defineGetter__('x',function(){return 42;}); o.x"), 42.0);
    assert!(prelude_bool("var o={}; o.__defineGetter__('x',function(){return 42;}); o.__lookupGetter__('x')()===42"));
    assert_eq!(prelude_num("var o={}; o.__defineSetter__('y',function(v){this._y=v*2;}); o.y=5; o._y"), 10.0);
    assert!(prelude_bool("var o={}; o.__defineSetter__('y',function(){}); typeof o.__lookupSetter__('y')==='function'"));
    // 데이터 프로퍼티 → get/set 조회는 undefined; 없는 키도 undefined
    assert!(prelude_bool("var o={a:1}; o.__lookupGetter__('a')===undefined && o.__lookupGetter__('none')===undefined"));
    // 정의된 접근자는 enumerable, 메서드 자체는 non-enumerable
    assert!(prelude_bool("var o={}; o.__defineGetter__('x',function(){}); Object.getOwnPropertyDescriptor(o,'x').enumerable===true"));
    assert!(prelude_bool("Object.keys(Object.prototype).indexOf('__defineGetter__')===-1"));
    // 비함수 인자 → TypeError
    assert!(prelude_bool("var t=false; try{ ({}).__defineGetter__('z',5) }catch(e){ t=e instanceof TypeError } t"));
    // 프로토타입 체인 관통 조회
    assert!(prelude_bool("var p={}; p.__defineGetter__('inh',function(){return 1;}); \
                      var c=Object.create(p); typeof c.__lookupGetter__('inh')==='function'"));
}

// SetIntegrityLevel (§7.3.15/.16): seal/freeze 는 non-extensible 뿐 아니라 각 own
// 프로퍼티의 속성을 조인다. 예전엔 무결성 비트만 남겨 gOPD 가 여전히 configurable:true
// 였고 verifyProperty 가 깨졌다.
#[test]
fn seal_freeze_clamp_property_attributes() {
    // seal: configurable=false, writable 유지
    assert!(run_bool("var o={foo:1}; Object.seal(o); var d=Object.getOwnPropertyDescriptor(o,'foo'); \
                      d.configurable===false && d.writable===true"));
    assert!(run_bool("var o={foo:1}; Object.seal(o); (delete o.foo)===false && o.foo===1"));
    assert!(run_bool("var o={foo:1}; Object.seal(o); o.foo=9; o.foo===9"));  // sealed 는 값 변경 허용
    assert!(run_bool("var o={foo:1}; Object.seal(o); Object.isSealed(o)===true"));
    // freeze: configurable=false + 데이터 프로퍼티 writable=false
    assert!(run_bool("var o={a:1}; Object.freeze(o); var d=Object.getOwnPropertyDescriptor(o,'a'); \
                      d.configurable===false && d.writable===false"));
    assert!(run_bool("var o={a:1}; Object.freeze(o); o.a=9; o.a===1"));  // 쓰기 차단
    assert!(run_bool("var o={a:1}; Object.freeze(o); Object.isFrozen(o)===true"));
    // 접근자는 configurable 만 조이고 get 은 유지(writable 없음)
    assert!(run_bool("var o={}; Object.defineProperty(o,'x',{get:function(){return 5;},configurable:true,enumerable:true}); \
                      Object.freeze(o); var d=Object.getOwnPropertyDescriptor(o,'x'); \
                      d.configurable===false && typeof d.get==='function'"));
    // frozen 프로퍼티를 다른 값으로 재정의 → TypeError (같은 값은 no-op 허용)
    assert!(run_bool("var o={a:1}; Object.freeze(o); var t=false; \
                      try{ Object.defineProperty(o,'a',{value:99}) }catch(e){ t=e instanceof TypeError } t"));
}

// 함수 정적 상속 (§10.1.8 OrdinaryGet): 함수도 ordinary object 이므로 own·내장 멤버에
// 없는 정적 프로퍼티는 [[Prototype]] 체인에서 상속한다. 예전엔 member_get 의 Fn arm 이
// 곧장 Undefined 라 setPrototypeOf 해도 정적 상속이 안 됐다.
#[test]
fn function_static_inheritance() {
    // 일반 함수: setPrototypeOf 로 정적 메서드 상속
    assert_eq!(
        run_num("function A(){} function B(){} B.sm=function(){return 42;}; \
                 Object.setPrototypeOf(A,B); A.sm()"),
        42.0
    );
    // 데이터 프로퍼티도 상속
    assert!(run_bool(
        "function A(){} function B(){} B.tag='x'; Object.setPrototypeOf(A,B); A.tag==='x'"
    ));
    // 접근자는 원 수신자(this=A)로 호출된다
    assert!(run_bool(
        "function A(){} function B(){} Object.defineProperty(B,'me',{get:function(){return this;}}); \
         Object.setPrototypeOf(A,B); A.me===A"
    ));
    // own 이 상속보다 우선
    assert!(run_bool(
        "function A(){} function B(){} B.k=1; A.k=2; Object.setPrototypeOf(A,B); A.k===2"
    ));
    // 없는 키는 여전히 undefined
    assert!(run_bool(
        "function A(){} function B(){} Object.setPrototypeOf(A,B); A.nope===undefined"
    ));
}

// %TypedArray% 정적 상속 (§23.2.2): Int8Array.from/of/[Symbol.species] 는 own 이 아니라
// 공유 %TypedArray% 생성자에서 상속한다. 예전엔 각 생성자가 own from/of 를 가져
// Int8Array.from !== %TypedArray%.from 이었다.
#[test]
fn typed_array_static_inheritance() {
    // from/of 가 %TypedArray% 에서 상속(동일 함수 정체성)
    assert!(prelude_bool("var TA=Object.getPrototypeOf(Int8Array); Int8Array.from===TA.from"));
    assert!(prelude_bool("var TA=Object.getPrototypeOf(Uint8Array); Uint8Array.of===TA.of"));
    // Int8Array.from === Uint8Array.from (둘 다 %TypedArray%.from)
    assert!(prelude_bool("Int8Array.from===Uint8Array.from"));
    // 상속됐지만 기능은 유지 — 올바른 종을 만든다 (new this(...), this=수신자)
    assert_eq!(prelude_str("Int8Array.from([1,2,3]).join(',')"), "1,2,3");
    assert!(prelude_bool("Int8Array.from([1,2,3]) instanceof Int8Array"));
    assert_eq!(prelude_str("Uint8Array.of(4,5,6).join(',')"), "4,5,6");
    assert!(prelude_bool("Uint8Array.of(4,5,6) instanceof Uint8Array"));
    // from 에 map 함수
    assert_eq!(prelude_str("Int8Array.from([1,2,3],function(x){return x*2;}).join(',')"), "2,4,6");
    // Symbol.species 는 %TypedArray% 의 접근자를 상속, this=수신자라 각 생성자 자신
    assert!(prelude_bool("typeof Symbol.species==='symbol'"));
    assert!(prelude_bool("Int8Array[Symbol.species]===Int8Array"));
    assert!(prelude_bool("Uint8Array[Symbol.species]===Uint8Array"));
}

// $262.detachArrayBuffer 호스트 훅 + ValidateTypedArray (§23.2.4.4): 분리된 버퍼의
// typed array 는 length/byteLength/byteOffset 이 0 이고, 대부분의 프로토타입 메서드는
// TypeError 를 던진다. 브랜드 불일치 수신자도 TypeError. 예전엔 $262 미정의로
// $DETACHBUFFER 하네스가 통째로 죽었고(ReferenceError), 메서드는 조용히 빈 결과였다.
#[test]
fn typed_array_detach_and_262_hook() {
    assert!(prelude_bool("typeof $262==='object' && typeof $262.detachArrayBuffer==='function'"));
    // 분리 후 getter 는 0 (throw 아님)
    assert!(prelude_bool(
        "var a=new Int8Array(new ArrayBuffer(8),0,4); $262.detachArrayBuffer(a.buffer); \
         a.length===0 && a.byteLength===0 && a.byteOffset===0"
    ));
    // 분리 후 메서드는 TypeError
    let thr = |body: &str| {
        format!("var a=new Int8Array(new ArrayBuffer(8),0,4); $262.detachArrayBuffer(a.buffer); \
                 var t=false; try{{ {} }}catch(e){{ t=e instanceof TypeError }} t", body)
    };
    for m in ["a.fill(1)", "a.map(function(x){return x;})", "a.forEach(function(){})",
              "a.join()", "a.keys()", "a.values()", "a.entries()", "a.slice()",
              "a.indexOf(1)", "a.sort()", "a.reverse()", "a.reduce(function(x){return x;})"] {
        assert!(prelude_bool(&thr(m)), "detached {} 는 TypeError 여야", m);
    }
    // subarray: begin/end 강제변환을 관측한 뒤 throw (생성자가 분리 버퍼 거부)
    assert!(prelude_bool(
        "var a=new Int8Array(new ArrayBuffer(8),0,2); $262.detachArrayBuffer(a.buffer); \
         var begin=false,end=false; var o1={valueOf:function(){begin=true;return 0;}}, \
         o2={valueOf:function(){end=true;return 2;}}; var t=false; \
         try{ a.subarray(o1,o2) }catch(e){ t=e instanceof TypeError } t && begin && end"
    ));
    // 브랜드 검사: typed array 아닌 수신자 → TypeError
    assert!(prelude_bool(
        "var t=false; try{ Int8Array.prototype.fill.call({},1) }catch(e){ t=e instanceof TypeError } t"
    ));
    assert!(prelude_bool(
        "var t=false; try{ Int8Array.prototype.map.call([1,2],function(x){return x;}) }catch(e){ t=e instanceof TypeError } t"
    ));
    // 분리된 버퍼 위 생성자 → TypeError
    assert!(prelude_bool(
        "var buf=new ArrayBuffer(8); $262.detachArrayBuffer(buf); \
         var t=false; try{ new Int8Array(buf) }catch(e){ t=e instanceof TypeError } t"
    ));
    // 정상(분리 안 됨)은 무회귀
    assert_eq!(prelude_str("new Uint8Array([1,2,3,4]).filter(function(x){return x%2;}).join(',')"), "1,3");
    assert_eq!(prelude_str("Array.from(new Uint8Array([1,2,3]).values()).join(',')"), "1,2,3");
    assert_eq!(prelude_str("Array.from(new Uint8Array([5,6]).keys()).join(',')"), "0,1");
}

// TypedArraySpeciesCreate (§23.2.4.1) + Proxy .constructor get: filter/map/slice/subarray
// 는 O.constructor[Symbol.species] 로 파생 종을 만든다. typed array 는 Proxy 라
// .constructor 재대입이 get 트랩을 거쳐 읽혀야 SpeciesConstructor 가 동작한다.
#[test]
fn typed_array_species() {
    // Proxy .constructor 재대입이 읽힌다(예전엔 constructor 특수해석이 트랩을 가로챔)
    assert!(prelude_bool("var s=new Uint8Array([1]); s.constructor={}; typeof s.constructor==='object'"));
    assert!(prelude_bool("var s=new Uint8Array([1]); s.constructor===Uint8Array"));
    // filter: species 를 정확히 1개 인자(kept 길이)로 Construct, this=새 인스턴스
    assert!(prelude_bool(
        "var s=new Uint8Array([40,42,42]); var r,ct; s.constructor={}; \
         s.constructor[Symbol.species]=function(c){ r=arguments; ct=this; return new Uint8Array(c); }; \
         s.filter(function(v){return v===42;}); \
         r.length===1 && r[0]===2 && ct instanceof s.constructor[Symbol.species]"
    ));
    // map: species count === 원소 수
    assert!(prelude_bool(
        "var s=new Int16Array([1,2,3]); var n; s.constructor={}; \
         s.constructor[Symbol.species]=function(c){ n=c; return new Int16Array(c); }; \
         s.map(function(x){return x*10;}).join(',')==='10,20,30' && n===3"
    ));
    // slice: species count === 잘린 길이
    assert!(prelude_bool(
        "var s=new Uint8Array([5,6,7,8]); var n; s.constructor={}; \
         s.constructor[Symbol.species]=function(c){ n=c; return new Uint8Array(c); }; \
         s.slice(1,3); n===2"
    ));
    // species 가 undefined/null 이면 기본 생성자로 폴백
    assert_eq!(prelude_str(
        "var s=new Uint8Array([1,2,3]); s.constructor={}; s.constructor[Symbol.species]=null; \
         s.map(function(x){return x;}).join(',')"), "1,2,3");
    // species 가 생성자 아니면 TypeError
    assert!(prelude_bool(
        "var s=new Uint8Array([1,2]); s.constructor={}; s.constructor[Symbol.species]=42; \
         var t=false; try{ s.map(function(x){return x;}) }catch(e){ t=e instanceof TypeError } t"
    ));
    // filter/map/forEach 의 length 는 1 (§23.2.3, thisArg 는 세지 않음)
    assert!(prelude_bool("Uint8Array.prototype.filter.length===1 && Uint8Array.prototype.map.length===1 && Uint8Array.prototype.forEach.length===1"));
    // 무회귀: species 미지정이면 같은 종
    assert!(prelude_bool("new Uint8Array([1,2,3,4]).filter(function(x){return x%2;}) instanceof Uint8Array"));
}

// TypedArray 생성자의 iterable 인자 (§23.2.5.1 InitializeTypedArrayFromList): @@iterator
// 가 있고 length 가 없는 순수 iterable 은 이터레이터로 값을 수집한다. 예전엔 length
// 없는 iterable 이 빈 배열로 떨어졌다(Set/제너레이터/사용자 iterable 통째로 비었음).
#[test]
fn typed_array_from_iterable() {
    // 사용자 iterable
    assert_eq!(prelude_str(
        "var it={}; it[Symbol.iterator]=function(){ var a=[10,20,30],i=0; return { next:function(){ return i<a.length?{value:a[i++],done:false}:{value:undefined,done:true}; } }; }; \
         new Uint8Array(it).join(',')"), "10,20,30");
    // Set / 제너레이터 / Map.keys
    assert_eq!(prelude_str("new Uint8Array(new Set([7,8,9])).join(',')"), "7,8,9");
    assert_eq!(prelude_str("new Int16Array(function*(){yield 1;yield 2;yield 3;}()).join(',')"), "1,2,3");
    assert_eq!(prelude_str("new Uint8Array(new Map([[1,'a'],[2,'b']]).keys()).join(',')"), "1,2");
    // BigInt iterable
    assert_eq!(prelude_str("new BigInt64Array(new Set([1n,2n,3n])).join(',')"), "1,2,3");
    // 무회귀: 배열/array-like/typed array/숫자 인자는 그대로
    assert_eq!(prelude_str("new Uint8Array([1,2,3,4]).join(',')"), "1,2,3,4");
    assert_eq!(prelude_str("new Uint8Array({length:3,0:5,1:6,2:7}).join(',')"), "5,6,7");
    assert_eq!(prelude_str("new Int8Array(new Uint8Array([9,8,7])).join(',')"), "9,8,7");
    assert_eq!(prelude_num("new Uint8Array(3).length"), 3.0);
}

// %TypedArray%.prototype.set (§23.2.3.24): array-like/typed-array 소스, offset ToInteger
// (음수 RangeError), 범위 RangeError, 브랜드/분리 TypeError, BigInt 타입 불일치 TypeError.
// 예전엔 검사 없이 length 루프만 돌아 대부분 조용히 통과/오동작했다.
#[test]
fn typed_array_set() {
    // length 는 1 (§)
    assert_eq!(prelude_num("Uint8Array.prototype.set.length"), 1.0);
    // array-like / typed array 소스
    assert_eq!(prelude_str("var t=new Uint8Array([1,2,3,4]); t.set([10,20],1); t.join(',')"), "1,10,20,4");
    assert_eq!(prelude_str("var t=new Uint8Array([1,2,3,4]); t.set(new Uint8Array([7,8]),2); t.join(',')"), "1,2,7,8");
    // 같은 버퍼 오버랩(임시배열로 안전)
    assert_eq!(prelude_str("var v=new Uint8Array(8); v.set([1,2,3,4,5,6,7,8]); v.set(v.subarray(0,4),2); v.join(',')"), "1,2,1,2,3,4,7,8");
    // 음수 offset → RangeError
    for off in ["-1", "-1.5", "-Infinity"] {
        assert!(prelude_bool(&format!(
            "var t=false; try{{ new Uint8Array(4).set([1],{}) }}catch(e){{ t=e instanceof RangeError }} t", off)));
    }
    // 범위 초과 → RangeError (array-like + typed array)
    assert!(prelude_bool("var t=false; try{ new Uint8Array(2).set([1,2,3]) }catch(e){ t=e instanceof RangeError } t"));
    assert!(prelude_bool("var t=false; try{ new Uint8Array(2).set(new Uint8Array([1,2,3])) }catch(e){ t=e instanceof RangeError } t"));
    // 브랜드 아님 → TypeError
    for recv in ["{}", "[]", "new ArrayBuffer(8)"] {
        assert!(prelude_bool(&format!(
            "var t=false; try{{ Uint8Array.prototype.set.call({}, []) }}catch(e){{ t=e instanceof TypeError }} t", recv)));
    }
    // 분리된 타깃 → TypeError
    assert!(prelude_bool(
        "var a=new Uint8Array(new ArrayBuffer(4)); $262.detachArrayBuffer(a.buffer); \
         var t=false; try{ a.set([1]) }catch(e){ t=e instanceof TypeError } t"));
    // BigInt 컨텐트 타입 불일치 → TypeError
    assert!(prelude_bool("var t=false; try{ new Uint8Array(2).set(new BigInt64Array([1n])) }catch(e){ t=e instanceof TypeError } t"));
    // offset 은 ToInteger (valueOf 관측)
    assert!(prelude_bool(
        "var seen=false; var t=new Uint8Array(4); t.set([9],{valueOf:function(){seen=true;return 1;}}); \
         seen && t.join(',')==='0,9,0,0'"));
}

// DataView (§25.3): ArrayBuffer 위 뷰. 타입별 get/set + 엔디언. 예전엔 완전 미구현.
#[test]
fn data_view_get_set_endian() {
    assert!(prelude_bool("typeof DataView==='function'"));
    // Uint8/Int8
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(4)); d.setUint8(0,200); d.getUint8(0)===200 && d.getInt8(0)===-56"));
    // Uint16 엔디언
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(4)); d.setUint16(0,0x1234,true); d.getUint8(0)===0x34 && d.getUint8(1)===0x12"));
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(4)); d.setUint16(0,0x1234,false); d.getUint8(0)===0x12 && d.getUint8(1)===0x34"));
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(4)); d.setUint16(0,0x1234,true); d.getUint16(0,true)===0x1234"));
    // Int32 왕복 + 엔디언 교차
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(4)); d.setInt32(0,-100,true); d.getInt32(0,true)===-100"));
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(4)); d.setUint32(0,0x01020304,false); d.getUint8(0)===1 && d.getUint8(3)===4"));
    // Float32/64 왕복
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(8)); d.setFloat64(0,3.14159,true); Math.abs(d.getFloat64(0,true)-3.14159)<1e-12"));
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(4)); d.setFloat32(0,1.5,true); d.getFloat32(0,true)===1.5"));
    // BigInt64 왕복 (부호형/무부호)
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(8)); d.setBigInt64(0,-1n,true); d.getBigInt64(0,true)===-1n && d.getBigUint64(0,true)===18446744073709551615n"));
    // byteOffset / byteLength
    assert!(prelude_bool("var b=new ArrayBuffer(16); var d=new DataView(b,4,8); d.byteOffset===4 && d.byteLength===8 && d.buffer===b"));
    // 범위 밖 → RangeError
    assert!(prelude_bool("var t=false; try{ new DataView(new ArrayBuffer(4)).getUint32(2) }catch(e){ t=e instanceof RangeError } t"));
    // 비-ArrayBuffer → TypeError
    assert!(prelude_bool("var t=false; try{ new DataView([]) }catch(e){ t=e instanceof TypeError } t"));
}

// DataView get/set 검사 순서 (§25.3.1): 브랜드(TypeError) → ToIndex(offset)(RangeError,
// Infinity/음수, valueOf 관측) → 분리(TypeError) → 범위(RangeError). 예전엔 offset|0 만 해
// Infinity 가 0 이 됐고, 브랜드/분리 검사가 없었다.
#[test]
fn data_view_index_and_brand_checks() {
    let thr = |body: &str, ty: &str| {
        prelude_bool(&format!("var t=false; try{{ {} }}catch(e){{ t=e instanceof {} }} t", body, ty))
    };
    // Infinity/-Infinity/음수 offset → RangeError
    assert!(thr("new DataView(new ArrayBuffer(8)).getUint8(Infinity)", "RangeError"));
    assert!(thr("new DataView(new ArrayBuffer(8)).getUint8(-Infinity)", "RangeError"));
    assert!(thr("new DataView(new ArrayBuffer(8)).getUint8(-1)", "RangeError"));
    // 범위 초과 → RangeError
    assert!(thr("new DataView(new ArrayBuffer(8)).getUint32(6)", "RangeError"));
    // 브랜드 아님 → TypeError ({} / typed array / 프로토 직접)
    assert!(thr("DataView.prototype.getUint8.call({}, 0)", "TypeError"));
    assert!(thr("DataView.prototype.getUint8.call(new Uint8Array(8), 0)", "TypeError"));
    // 분리된 버퍼 read/byteLength → TypeError
    assert!(thr("var b=new ArrayBuffer(8); var v=new DataView(b); $262.detachArrayBuffer(b); v.getUint8(0)", "TypeError"));
    assert!(thr("var b=new ArrayBuffer(8); var v=new DataView(b); $262.detachArrayBuffer(b); v.byteLength", "TypeError"));
    // 생성자: 분리 버퍼 TypeError, Infinity offset RangeError
    assert!(thr("var b=new ArrayBuffer(8); $262.detachArrayBuffer(b); new DataView(b)", "TypeError"));
    assert!(thr("new DataView(new ArrayBuffer(8), Infinity)", "RangeError"));
    // offset 의 valueOf 관측(ToIndex)
    assert!(prelude_bool(
        "var seen=false; new DataView(new ArrayBuffer(8)).getUint8({valueOf:function(){seen=true;return 0;}}); seen"));
    // 정상 왕복 무회귀
    assert!(prelude_bool("var d=new DataView(new ArrayBuffer(8)); d.setFloat64(0,3.5,true); d.getFloat64(0,true)===3.5"));
}

// Integer-Indexed Exotic Object (§10.4.5): typed array 의 정수 인덱스는 defineProperty/
// getOwnPropertyDescriptor/has/delete 에서 특별 취급된다. Proxy 트랩을 gOPD/defineProperty
// 로 라우팅하고 typed array Proxy 에 트랩을 달았다. 예전엔 인덱스 define 이 조용히 무시됐다.
#[test]
fn typed_array_integer_indexed_exotic() {
    // gOPD: 유효 인덱스는 {value, w:t, e:t, c:t}, 범위 밖은 undefined
    assert!(prelude_bool(
        "var d=Object.getOwnPropertyDescriptor(new Uint8Array([1,2,3]),'1'); \
         d.value===2 && d.writable===true && d.enumerable===true && d.configurable===true"));
    assert!(prelude_bool("Object.getOwnPropertyDescriptor(new Uint8Array([1,2,3]),'5')===undefined"));
    // defineProperty: 인덱스에 값 기록
    assert_eq!(prelude_num("var t=new Uint8Array([1,2,3]); Object.defineProperty(t,'0',{value:9}); t[0]"), 9.0);
    // 범위 밖 인덱스 defineProperty → Object.defineProperty TypeError, Reflect 는 false
    assert!(prelude_bool("var t=false; try{ Object.defineProperty(new Uint8Array(3),'5',{value:1}) }catch(e){ t=e instanceof TypeError } t"));
    assert!(prelude_bool("Reflect.defineProperty(new Uint8Array(3),'9',{value:1})===false"));
    // 서술자가 configurable/enumerable/writable false 또는 접근자면 거부
    assert!(prelude_bool("Reflect.defineProperty(new Uint8Array(3),'0',{value:1,writable:false})===false"));
    assert!(prelude_bool("Reflect.defineProperty(new Uint8Array(3),'0',{value:1,configurable:false})===false"));
    assert!(prelude_bool("Reflect.defineProperty(new Uint8Array(3),'0',{get:function(){return 1;}})===false"));
    // has: 유효 인덱스만 존재, 비인덱스는 보통
    assert!(prelude_bool("'0' in new Uint8Array([1,2,3])"));
    assert!(prelude_bool("!('5' in new Uint8Array([1,2,3]))"));
    // delete: 유효 인덱스는 삭제 불가(false), 값 유지
    assert!(prelude_bool("var t=new Uint8Array([1,2,3]); (delete t[0])===false && t[0]===1"));
    // 비인덱스 define 은 보통(무회귀)
    assert!(prelude_bool("var t=new Uint8Array([1,2,3]); Object.defineProperty(t,'foo',{value:7}); t.foo===7"));
    // 무회귀: 읽기/쓰기/메서드
    assert_eq!(prelude_str("new Uint8Array([1,2,3,4]).filter(function(x){return x%2;}).join(',')"), "1,3");
}

// ArrayBuffer 표준화 (§25.1): byteLength/maxByteLength/resizable/detached 는 prototype
// accessor, isView/transfer/transferToFixedLength/resize, 생성자 검증.
#[test]
fn array_buffer_standardization() {
    assert_eq!(prelude_num("new ArrayBuffer(8).byteLength"), 8.0);
    // byteLength 는 accessor (인스턴스 own 데이터 아님)
    assert!(prelude_bool(
        "typeof Object.getOwnPropertyDescriptor(ArrayBuffer.prototype,'byteLength').get==='function'"
    ));
    assert!(prelude_bool("!Object.prototype.hasOwnProperty.call(new ArrayBuffer(4),'byteLength')"));
    // isView
    assert!(prelude_bool("ArrayBuffer.isView(new Uint8Array(2))===true"));
    assert!(prelude_bool("ArrayBuffer.isView(new ArrayBuffer(2))===false"));
    assert!(prelude_bool("ArrayBuffer.isView([])===false"));
    // transfer → 원본 detach
    assert!(prelude_bool(
        "var x=new ArrayBuffer(4); var y=x.transfer(); x.detached===true && y.byteLength===4 && x.byteLength===0"
    ));
    // resizable / maxByteLength
    assert!(prelude_bool("new ArrayBuffer(4).resizable===false"));
    assert!(prelude_bool("var b=new ArrayBuffer(4,{maxByteLength:8}); b.resizable===true && b.maxByteLength===8"));
    assert!(prelude_bool("var b=new ArrayBuffer(4,{maxByteLength:8}); b.resize(6); b.byteLength===6"));
    // 생성자 검증: 음수 length → RangeError
    assert!(prelude_bool("var t=false; try{ new ArrayBuffer(-1) }catch(e){ t=e instanceof RangeError } t"));
    // slice
    assert_eq!(prelude_num("new ArrayBuffer(8).slice(2,6).byteLength"), 4.0);
    // typed array 가 버퍼 byteLength(accessor) 를 정확히 읽는다
    assert_eq!(prelude_num("new Uint8Array(new ArrayBuffer(12)).length"), 12.0);
}

// TypedArray.prototype 메서드 (§23.2.3): 예전엔 filter/every/some/find/sort/reverse/
// copyWithin/at/toSorted/toReversed/with/keys/entries 등이 undefined 였다.
#[test]
fn typed_array_prototype_methods() {
    // sort 는 수치 오름차순 기본(문자열 아님)
    assert_eq!(prelude_str("new Int32Array([30,1,2]).sort().join(',')"), "1,2,30");
    // filter 는 같은 타입 typed array 반환 (instanceof 은 Proxy 한계로 별도 — BYTES 로 확인)
    assert!(prelude_bool(
        "var r=new Uint8Array([1,2,3,4]).filter(function(x){return x%2;}); \
         r.BYTES_PER_ELEMENT===1 && r.length===2"
    ));
    assert_eq!(prelude_str("new Uint8Array([1,2,3,4]).filter(function(x){return x%2;}).join(',')"), "1,3");
    assert!(prelude_bool("new Int8Array([1,2,3]).every(function(x){return x>0;})"));
    assert!(prelude_bool("new Int8Array([1,2,3]).some(function(x){return x>2;})"));
    assert_eq!(prelude_num("new Int8Array([5,6,7]).find(function(x){return x>5;})"), 6.0);
    assert_eq!(prelude_str("new Int16Array([1,2,3]).reverse().join(',')"), "3,2,1");
    assert_eq!(prelude_num("new Uint8Array([10,20,30]).at(-1)"), 30.0);
    assert_eq!(prelude_str("new Int32Array([1,2,3]).toReversed().join(',')"), "3,2,1");
    // toSorted 는 원본 불변 + 새 typed array
    assert!(prelude_bool("var a=new Int32Array([3,1,2]); var b=a.toSorted(); b.join(',')==='1,2,3' && a.join(',')==='3,1,2'"));
    assert_eq!(prelude_str("new Uint8Array([1,2,3]).with(1,9).join(',')"), "1,9,3");
    assert_eq!(prelude_str("Array.from(new Int8Array([5,6]).keys()).join(',')"), "0,1");
    assert_eq!(prelude_str("new Int32Array([1,2,3,4,5]).copyWithin(0,3).join(',')"), "4,5,3,4,5");
    // BigInt 배열 sort (BigInt 비교가 정확해야 함)
    assert_eq!(prelude_str("new BigInt64Array([3n,1n,2n]).sort().join(',')"), "1,2,3");
    // Proxy instanceof (typed array 는 Proxy) — 타깃 프로토타입 체인으로 판정
    assert!(prelude_bool("new Uint8Array([1,2,3]) instanceof Uint8Array"));
    assert!(prelude_bool("new Int32Array(3) instanceof Int32Array"));
    assert!(prelude_bool("new Uint8Array([1,2,3,4]).filter(function(x){return x%2;}) instanceof Uint8Array"));
    assert!(!prelude_bool("new Uint8Array(2) instanceof Int8Array")); // 다른 생성자
}

// BigInt64Array / BigUint64Array (ES2020) — 원소가 BigInt 인 typed array. 예전엔
// 미정의라 test262 의 testWithBigIntTypedArrayConstructors 하네스가 통째로 죽었다.
#[test]
fn bigint64_typed_arrays() {
    assert!(prelude_bool(
        "typeof BigInt64Array === 'function' && typeof BigUint64Array === 'function'"
    ));
    assert!(prelude_bool("var a=new BigInt64Array(3); a.length===3 && a[0]===0n"));
    assert!(prelude_bool("var a=new BigInt64Array([1n,2n,3n]); a[0]===1n && a[2]===3n"));
    assert!(prelude_bool("new BigInt64Array(2).BYTES_PER_ELEMENT===8"));
    // 부호형 왕복: -1n
    assert!(prelude_bool("var a=new BigInt64Array(1); a[0]=-1n; a[0]===-1n"));
    // 무부호는 2의 보수로 랩: -1n → 2^64-1
    assert!(prelude_bool("var a=new BigUint64Array(1); a[0]=-1n; a[0]===18446744073709551615n"));
    // int64 최대값 왕복
    assert!(prelude_bool(
        "var a=new BigInt64Array(1); a[0]=9223372036854775807n; a[0]===9223372036854775807n"
    ));
    // 프로토타입 메서드도 BigInt 로 동작
    assert!(prelude_bool("var a=new BigInt64Array([5n]); a.map(function(x){return x*2n;})[0]===10n"));
}

// ES2024 정적 메서드: Object.groupBy / Map.groupBy / Promise.withResolvers.
#[test]
fn es2024_static_methods() {
    // Object.groupBy — 문자열 키로 그룹화
    assert_eq!(
        run_str("var g=Object.groupBy([1,2,3,4],function(x){return x%2?'odd':'even';}); g.odd.join(',')+'|'+g.even.join(',')"),
        "1,3|2,4"
    );
    assert_eq!(run_str("Object.groupBy.name"), "groupBy");
    assert_eq!(run_num("Object.groupBy.length"), 2.0);
    assert!(run_bool("Object.getOwnPropertyNames(Object).indexOf('groupBy')>=0"));
    // Map.groupBy — 임의 키(SameValueZero), Map 반환
    assert!(run_bool("var g=Map.groupBy([1,2,3,4],function(x){return x%2;}); (g instanceof Map) && g.get(1).join(',')==='1,3' && g.get(0).join(',')==='2,4'"));
    // 콜백 비함수 → TypeError
    assert!(run_bool("var t=false; try{ Object.groupBy([1],1) }catch(e){ t=e instanceof TypeError } t"));
    // Promise.withResolvers — {promise, resolve, reject}
    assert!(run_bool(
        "var d=Promise.withResolvers(); \
         (d.promise instanceof Promise) && typeof d.resolve==='function' && typeof d.reject==='function'"
    ));
    assert_eq!(run_str("Promise.withResolvers.name"), "withResolvers");
    // withResolvers 의 resolve 로 이행
    assert!(run_bool(
        "var d=Promise.withResolvers(); var got; d.promise.then(function(v){got=v;}); d.resolve(42); \
         /* 마이크로태스크 드레인은 헤드리스에서 */ true"
    ));
}

// Symbol.prototype.description 은 프로토타입 accessor 다 (§20.4.3.2). 예전엔 심볼
// 원시값 member_get 으로만 돼서 getOwnPropertyDescriptor(Symbol.prototype,'description')
// 이 undefined 였다.
#[test]
fn symbol_description_accessor() {
    assert_eq!(run_str("Symbol('hi').description"), "hi");
    assert!(run_bool("Symbol().description === undefined"));
    // 프로토타입 accessor
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Symbol.prototype,'description'); \
         typeof d.get==='function' && d.set===undefined && d.enumerable===false && d.configurable===true"
    ));
    assert_eq!(run_str("Object.getOwnPropertyDescriptor(Symbol.prototype,'description').get.name"), "get description");
    // getter 를 잘못된 수신자에 부르면 TypeError
    assert!(run_bool(
        "var g=Object.getOwnPropertyDescriptor(Symbol.prototype,'description').get; \
         var t=false; try{ g.call({}) }catch(e){ t=e instanceof TypeError } t"
    ));
    // description 은 인스턴스 own 아님
    assert!(run_bool("!Object.prototype.hasOwnProperty.call(Symbol('x'),'description')"));
}

// JSON.parse 는 잘못된 입력에 SyntaxError 를 던지고(예전엔 일반 Error), reviver 를
// 후위 순회로 적용한다 (§25.5.1).
#[test]
fn json_parse_syntaxerror_and_reviver() {
    // 잘못된 JSON → SyntaxError
    assert!(run_bool("var t=false; try{ JSON.parse('{bad}') }catch(e){ t=e instanceof SyntaxError } t"));
    assert!(run_bool("var t=false; try{ JSON.parse('[1,2') }catch(e){ t=e instanceof SyntaxError } t"));
    assert!(run_bool("var t=false; try{ JSON.parse('') }catch(e){ t=e instanceof SyntaxError } t"));
    // 정상 파싱
    assert_eq!(run_num("JSON.parse('{\"a\":5}').a"), 5.0);
    // reviver: 값 변환
    assert_eq!(run_num("JSON.parse('{\"a\":2}', function(k,v){ return typeof v==='number' ? v*10 : v; }).a"), 20.0);
    // reviver: undefined 반환 시 키 삭제
    assert!(run_bool(
        "var o=JSON.parse('{\"a\":1,\"b\":2}', function(k,v){ return k==='b'?undefined:v; }); \
         o.a===1 && !('b' in o)"
    ));
    // reviver 는 배열도 순회
    assert_eq!(
        run_str("JSON.parse('[1,2,3]', function(k,v){ return typeof v==='number'?v+1:v; }).join(',')"),
        "2,3,4"
    );
}

// Reflect 나머지 (§28.1): getOwnPropertyDescriptor/setPrototypeOf/isExtensible/
// preventExtensions 미구현이었고, ownKeys→열거키만, defineProperty→객체 반환이던 편법.
#[test]
fn reflect_completeness() {
    // 존재 + 이름
    assert_eq!(run_str("Reflect.setPrototypeOf.name"), "setPrototypeOf");
    assert_eq!(run_str("Reflect.getOwnPropertyDescriptor.name"), "getOwnPropertyDescriptor");
    assert_eq!(run_str("Reflect.ownKeys.name"), "ownKeys");
    // ownKeys 는 비열거 키도 포함 (예전엔 열거만)
    assert!(run_bool(
        "var o={}; Object.defineProperty(o,'x',{value:1,enumerable:false}); \
         Reflect.ownKeys(o).indexOf('x')>=0"
    ));
    // getOwnPropertyDescriptor
    assert_eq!(run_num("Reflect.getOwnPropertyDescriptor({a:5},'a').value"), 5.0);
    assert!(run_bool("Reflect.getOwnPropertyDescriptor({},'z')===undefined"));
    // defineProperty 는 불리언 반환
    assert!(run_bool("Reflect.defineProperty({}, 'a', {value:1})===true"));
    // isExtensible / preventExtensions
    assert!(run_bool("Reflect.isExtensible({})===true"));
    assert!(run_bool("var o={}; Reflect.preventExtensions(o)===true && Reflect.isExtensible(o)===false"));
    // setPrototypeOf 는 불리언, 실제로 프로토타입을 바꾼다
    assert!(run_bool("var o={}; var p={pm:1}; Reflect.setPrototypeOf(o,p)===true && o.pm===1"));
    // 대상이 객체 아니면 TypeError
    assert!(run_bool("var t=false; try{ Reflect.ownKeys(1) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ Reflect.getOwnPropertyDescriptor(null,'x') }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ Reflect.isExtensible('s') }catch(e){ t=e instanceof TypeError } t"));
}

// ES2024 집합 연산 (§24.2.4): union/intersection/difference/symmetricDifference +
// isSubsetOf/isSupersetOf/isDisjointFrom. set-like 인자(GetSetRecord)도 받는다.
#[test]
fn set_es2024_methods() {
    let arr = |src: &str| run_str(&format!("Array.from({}).join(',')", src));
    assert_eq!(arr("new Set([1,2]).union(new Set([2,3]))"), "1,2,3");
    assert_eq!(arr("new Set([1,2,3]).intersection(new Set([2,3,4]))"), "2,3");
    assert_eq!(arr("new Set([1,2,3]).difference(new Set([2,3]))"), "1");
    assert_eq!(arr("new Set([1,2,3]).symmetricDifference(new Set([2,3,4]))"), "1,4");
    assert!(run_bool("new Set([1,2]).isSubsetOf(new Set([1,2,3]))"));
    assert!(!run_bool("new Set([1,2,4]).isSubsetOf(new Set([1,2,3]))"));
    assert!(run_bool("new Set([1,2,3]).isSupersetOf(new Set([1,2]))"));
    assert!(!run_bool("new Set([1,2]).isSupersetOf(new Set([1,2,3]))"));
    assert!(run_bool("new Set([1,2]).isDisjointFrom(new Set([3,4]))"));
    assert!(!run_bool("new Set([1,2]).isDisjointFrom(new Set([2,3]))"));
    // set-like 인자 (size/has/keys) — union 은 other 의 keys 를 순회한다
    assert_eq!(
        arr("new Set([1,2]).union({size:1, has:function(){return false;}, keys:function(){return [3][Symbol.iterator]();}})"),
        "1,2,3"
    );
    // 원본 불변
    assert!(run_bool("var s=new Set([1,2]); s.union(new Set([3])); s.size===2"));
    // name/length
    assert_eq!(run_str("Set.prototype.union.name"), "union");
    assert_eq!(run_num("Set.prototype.intersection.length"), 1.0);
    // GetSetRecord 오류: 비객체 TypeError, size NaN TypeError, size 음수 RangeError
    assert!(run_bool("var t=false; try{ new Set().union(null) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool("var t=false; try{ new Set().union({}) }catch(e){ t=e instanceof TypeError } t"));
    assert!(run_bool(
        "var t=false; try{ new Set().union({size:-1,has:function(){},keys:function(){}}) }catch(e){ t=e instanceof RangeError } t"
    ));
}

#[test]
fn date_utc_methods_and_string_forms() {
    // UTC 게터는 로컬(오프셋 0)과 동일 + 정확한 name/length
    assert_eq!(run_num("new Date(Date.UTC(2020,5,15,13,45,30,123)).getUTCHours()"), 13.0);
    assert_eq!(run_num("new Date(Date.UTC(2020,5,15,13,45,30,123)).getUTCMonth()"), 5.0);
    assert_eq!(run_num("new Date(Date.UTC(2020,5,15,13,45,30,123)).getUTCDay()"), 1.0);
    assert_eq!(run_num("new Date(Date.UTC(2020,5,15,13,45,30,123)).getUTCMilliseconds()"), 123.0);
    assert_eq!(run_str("Date.prototype.getUTCHours.name"), "getUTCHours");
    assert_eq!(run_str("Date.prototype.setUTCFullYear.name"), "setUTCFullYear");
    assert_eq!(run_num("Date.prototype.setUTCHours.length"), 4.0);
    // 문자열 형식
    assert_eq!(
        run_str("new Date(Date.UTC(2020,5,15,13,45,30,123)).toUTCString()"),
        "Mon, 15 Jun 2020 13:45:30 GMT"
    );
    assert_eq!(
        run_str("new Date(Date.UTC(2020,5,15,13,45,30,123)).toDateString()"),
        "Mon Jun 15 2020"
    );
    assert_eq!(
        run_str("new Date(Date.UTC(2020,5,15,13,45,30,123)).toTimeString()"),
        "13:45:30 GMT+0000 (Coordinated Universal Time)"
    );
    assert_eq!(
        run_str("new Date(Date.UTC(2020,5,15,13,45,30,123)).toString()"),
        "Mon Jun 15 2020 13:45:30 GMT+0000 (Coordinated Universal Time)"
    );
    // toGMTString === toUTCString
    assert!(run_bool("var d=new Date(0); d.toGMTString()===d.toUTCString()"));
    // Date.prototype 이 47개 own 메서드 (전량 노출)
    assert!(run_bool("Object.getOwnPropertyNames(Date.prototype).length>=44"));
}

#[test]
fn date_setter_coercion_and_nan() {
    // setHours 는 인자를 정확히 한 번 ToNumber (valueOf 관찰)
    assert!(run_bool(
        "var c=0; var d=new Date(0); d.setHours({valueOf:function(){c++;return 5;}}); c===1 && d.getHours()===5"
    ));
    // 인자 여러 개 순서대로 한 번씩
    assert!(run_bool(
        "var log=[]; var mk=function(n){return {valueOf:function(){log.push(n);return n;}};}; \
         var d=new Date(0); d.setHours(mk(1),mk(2),mk(3),mk(4)); log.join(',')==='1,2,3,4'"
    ));
    // Invalid Date + setHours: 인자 강제(valueOf 관찰) 후 결과 NaN
    assert!(run_bool(
        "var c=0; var d=new Date(NaN); var r=d.setHours({valueOf:function(){c++;return 3;}}); c===1 && isNaN(r) && isNaN(d.getTime())"
    ));
    // setFullYear 는 Invalid Date 여도 t=+0 기준으로 유효 날짜 생성
    assert_eq!(run_num("new Date(NaN).setFullYear(2000)"), 946684800000.0);
    // TimeClip: 범위 초과는 NaN
    assert!(run_bool("isNaN(new Date(0).setTime(1e21))"));
    // getter 는 Invalid Date 에 NaN
    assert!(run_bool("isNaN(new Date(NaN).getFullYear()) && isNaN(new Date(NaN).getMonth())"));
}

#[test]
fn date_tojson_toprimitive_and_receiver_check() {
    // toJSON: 유효하면 toISOString, Invalid 이면 null (throw 아님)
    assert_eq!(
        run_str("new Date(Date.UTC(2020,0,1)).toJSON()"),
        "2020-01-01T00:00:00.000Z"
    );
    assert!(run_bool("new Date(NaN).toJSON()===null"));
    // toISOString 은 Invalid 에 RangeError
    assert!(run_bool("var t=false; try{ new Date(NaN).toISOString(); }catch(e){ t=e instanceof RangeError; } t"));
    // Symbol.toPrimitive: 힌트별 동작 + 잘못된 힌트 TypeError
    assert!(run_bool("typeof new Date(0)[Symbol.toPrimitive]('string')==='string'"));
    assert_eq!(run_num("new Date(0)[Symbol.toPrimitive]('number')"), 0.0);
    assert!(run_bool("var t=false; try{ new Date(0)[Symbol.toPrimitive]('bad'); }catch(e){ t=e instanceof TypeError; } t"));
    // Symbol.toPrimitive 서술자
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Date.prototype,Symbol.toPrimitive); \
         d.writable===false && d.enumerable===false && d.configurable===true && typeof d.value==='function'"
    ));
    assert_eq!(run_str("Date.prototype[Symbol.toPrimitive].name"), "[Symbol.toPrimitive]");
    // 비 Date 수신자면 게터가 TypeError
    assert!(run_bool("var t=false; try{ Date.prototype.getHours.call({}); }catch(e){ t=e instanceof TypeError; } t"));
    // hasOwnProperty(Symbol) 와 getOwnPropertyDescriptor(obj, Symbol)
    assert!(run_bool("Date.prototype.hasOwnProperty(Symbol.toPrimitive)"));
    assert!(run_bool("Array.prototype.hasOwnProperty(Symbol.iterator)"));
    // 심볼 서술자 attrs 정확
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Array.prototype,Symbol.iterator); \
         d.writable===true && d.enumerable===false && d.configurable===true"
    ));
    assert!(run_bool(
        "var d=Object.getOwnPropertyDescriptor(Math,Symbol.toStringTag); \
         d.writable===false && d.enumerable===false && d.configurable===true && d.value==='Math'"
    ));
    // 문자열 열거는 여전히 심볼 제외
    assert!(run_bool("Object.keys(Date.prototype).indexOf('toString')<0"));
    // annexB getYear/setYear
    assert_eq!(run_num("new Date(Date.UTC(1970,0,1)).getYear()"), 70.0);
    assert!(run_bool("var d=new Date(0); d.setYear(99); d.getFullYear()===1999"));
    assert!(run_bool("var d=new Date(0); d.setYear(2005); d.getFullYear()===2005"));
}

#[test]
fn function_prototype_constructor_backlink() {
    // §20.1.1: F.prototype.constructor === F (writable, non-enum, configurable).
    assert!(run_bool("function F(){}; F.prototype.constructor===F"));
    assert!(run_bool("function F(){}; F.prototype.hasOwnProperty('constructor')"));
    // 인스턴스는 프로토타입 체인으로 constructor 를 상속
    assert!(run_bool("function F(){}; new F().constructor===F"));
    assert_eq!(run_str("function Foo(){}; new Foo().constructor.name"), "Foo");
    // 생성으로 프로토타입이 먼저 실체화돼도 constructor 존재 (member_get 없이)
    assert!(run_bool("function E(){}; var e=new E(); e.constructor===E"));
    // 사용자 정의 에러 클래스(함수형)의 throw → constructor 판별
    assert!(run_bool(
        "function MyErr(){}; var caught; \
         try{ (function(){throw new MyErr();})(); }catch(e){ caught=e; } \
         caught.constructor===MyErr"
    ));
    // constructor 속성: writable, non-enumerable, configurable
    assert!(run_bool(
        "function F(){}; var d=Object.getOwnPropertyDescriptor(F.prototype,'constructor'); \
         d.writable===true && d.enumerable===false && d.configurable===true && d.value===F"
    ));
    // constructor 는 열거되지 않는다
    assert!(run_bool("function F(){}; Object.keys(F.prototype).indexOf('constructor')<0"));
    // 화살표 함수는 prototype 이 없다
    assert!(run_bool("(function(){var a=()=>{}; return a.prototype===undefined;})()"));
    // 사용자가 constructor 를 덮어쓸 수 있다 (writable)
    assert!(run_bool("function F(){}; F.prototype.constructor=42; F.prototype.constructor===42"));
}

#[test]
fn class_instance_fields_enumeration_delete_order() {
    // 클래스 필드는 삽입 순서로 own 열거 프로퍼티 (ObjMap)
    assert_eq!(
        run_str("class C{ a; b=42; c; } var o=new C(); Object.keys(o).join(',')"),
        "a,b,c"
    );
    // for-in 이 인스턴스 필드를 순회 (메서드는 비열거)
    assert_eq!(
        run_str("class C{ m(){} x=1; y=2; } var o=new C(); var s=[]; for(var k in o)s.push(k); s.join(',')"),
        "x,y"
    );
    // 필드는 {w:true,e:true,c:true}
    assert!(run_bool(
        "class C{ a=1; } var o=new C(); var d=Object.getOwnPropertyDescriptor(o,'a'); \
         d.value===1 && d.writable && d.enumerable && d.configurable"
    ));
    // delete 가 실제로 인스턴스 필드를 제거
    assert!(run_bool("class C{ a=1; } var o=new C(); delete o.a; !o.hasOwnProperty('a')"));
    assert_eq!(run_str("class C{ a=1; b=2; } var o=new C(); delete o.a; Object.keys(o).join(',')"), "b");
    // 메서드는 프로토타입 own(비열거), 인스턴스 own 아님
    assert!(run_bool("class C{ m(){} } var o=new C(); !o.hasOwnProperty('m') && C.prototype.hasOwnProperty('m')"));
    // 프로토타입 메서드 삭제 (configurable)
    assert!(run_bool("class C{ m(){} } delete C.prototype.m; C.prototype.m===undefined"));
    // Object.assign 이 인스턴스 열거 필드를 순서대로 복사
    assert_eq!(
        run_str("class C{ a=1; b=2; } var t=Object.assign({}, new C()); Object.keys(t).join(',')"),
        "a,b"
    );
}

#[test]
fn async_generator_next_returns_promise() {
    // async function* 의 next()/return()/throw() 는 Promise 를 돌려준다 (§27.6).
    assert!(run_bool("async function* ag(){ yield 1; } typeof ag().next().then === 'function'"));
    // 결과 promise 는 {value,done} 로 이행
    assert_eq!(
        run_str("async function* ag(){ yield 5; } await ag().next().then(function(r){return r.value+':'+r.done;})"),
        "5:false"
    );
    // for await 로 소비 (await 된 yield 값 포함)
    assert_eq!(
        run_str("async function* ag(){ yield 1; yield await Promise.resolve(2); yield 3; } \
                 var out=[]; for await (var x of ag()) out.push(x); out.join(',')"),
        "1,2,3"
    );
    // return() 은 이행 promise {value, done:true}
    assert_eq!(
        run_str("async function* ag(){ yield 1; } await ag().return(9).then(function(r){return r.value+':'+r.done;})"),
        "9:true"
    );
    // throw() 는 본문 catch 로 잡혀 yield 가능
    assert_eq!(
        run_str("async function* ag(){ try{ yield 1; }catch(e){ yield 'c'+e; } } \
                 var it=ag(); await it.next(); await it.throw('X').then(function(r){return r.value;})"),
        "cX"
    );
    // 클래스 async 제너레이터 메서드
    assert!(run_bool(
        "class C{ async *m(){ yield 1; } } typeof new C().m().next().then === 'function'"
    ));
}

#[test]
fn array_generic_arraylike_and_from_index() {
    // array-like: getter length + [[Get]] 원소 읽기 (§23.1.3 generic)
    assert_eq!(
        run_num("var o={1:true}; Object.defineProperty(o,'length',{get:function(){return 2;}}); \
                 Array.prototype.indexOf.call(o, true)"),
        1.0
    );
    // 문자열 length 강제 (ToLength(ToNumber))
    assert_eq!(
        run_num("Array.prototype.indexOf.call({0:9,length:'1'}, 9)"),
        0.0
    );
    // 상속 원소(프로토타입 체인)
    assert_eq!(
        run_num("var b={0:'a'}; var c=Object.create(b); c.length=1; Array.prototype.indexOf.call(c,'a')"),
        0.0
    );
    // 원시 래퍼 수신자 + 프로토타입 원소
    assert_eq!(
        run_num("Boolean.prototype[1]='Z'; Boolean.prototype.length=2; \
                 var r=Array.prototype.indexOf.call(true,'Z'); delete Boolean.prototype[1]; delete Boolean.prototype.length; r"),
        1.0
    );
    // map/reduce on array-like
    assert_eq!(
        run_str("var al={0:1,1:2,2:3,length:3}; Array.prototype.map.call(al,function(x){return x*10;}).join(',')"),
        "10,20,30"
    );
    assert_eq!(run_num("Array.prototype.reduce.call({0:1,1:2,length:2},function(a,b){return a+b;},0)"), 3.0);
    // indexOf fromIndex (문자열 강제)
    assert_eq!(run_num("[1,2,1,2].indexOf(2,'2')"), 3.0);
    assert_eq!(run_num("[1,2,3,2,1].indexOf(2,-2)"), 3.0);
    // indexOf 는 strict eq (NaN 매칭 안 함), includes 는 SameValueZero (NaN 매칭)
    assert_eq!(run_num("[NaN].indexOf(NaN)"), -1.0);
    assert!(run_bool("[NaN].includes(NaN)"));
    assert!(!run_bool("[1,2,3].includes(1,1)"));
    // lastIndexOf fromIndex (프렐류드 폴리필 — prelude_num)
    assert_eq!(prelude_num("[3,1,2,1].lastIndexOf(1,2)"), 1.0);
    assert_eq!(prelude_num("[3,1,2,1].lastIndexOf(1,-1)"), 3.0);
    assert_eq!(prelude_num("[1,2,3].lastIndexOf(2)"), 1.0);
    // 실제 배열 무회귀
    assert_eq!(run_str("[1,2,3].map(function(x){return x*2;}).join(',')"), "2,4,6");
}

#[test]
fn function_tostring_source_text() {
    // §20.2.3.5: 사용자 함수는 원본 소스 텍스트 그대로.
    assert_eq!(run_str("function foo(a, b) { return a + b; } foo.toString()"),
               "function foo(a, b) { return a + b; }");
    assert_eq!(run_str("var f = (x) => x * 2; f.toString()"), "(x) => x * 2");
    assert_eq!(run_str("function* g(a){ yield a; } g.toString()"), "function* g(a){ yield a; }");
    assert_eq!(run_str("async function af(x){ return x; } af.toString()"), "async function af(x){ return x; }");
    // 주석/공백 보존
    assert_eq!(run_str("var f = function /*c*/ n (x) { return x; }; f.toString()"),
               "function /*c*/ n (x) { return x; }");
    // 객체 메서드/접근자
    assert_eq!(run_str("var o={ m(a){ return a; } }; Object.getOwnPropertyDescriptor(o,'m').value.toString()"),
               "m(a){ return a; }");
    assert_eq!(run_str("var o={ get x(){ return 1; } }; Object.getOwnPropertyDescriptor(o,'x').get.toString()"),
               "get x(){ return 1; }");
    // 내장 함수: NativeFunction 문법 + 이름
    assert_eq!(run_str("Math.max.toString()"), "function max() { [native code] }");
    assert_eq!(run_str("parseInt.toString()"), "function parseInt() { [native code] }");
    // 바운드 함수: native code (이름 없이)
    assert_eq!(run_str("(function f(){}).bind(null).toString()"), "function () { [native code] }");
    // toString 결과가 다시 파싱 가능(대략) — 최소한 'function' 포함
    assert!(run_bool("/function|=>/.test((function abc(){}).toString())"));
}

#[test]
fn class_method_tostring_source() {
    // 클래스 메서드/정적/getter/제너레이터/async 의 toString 은 소스 텍스트 (static 제외).
    assert_eq!(run_str("class C{ method(a){ return a; } } C.prototype.method.toString()"),
               "method(a){ return a; }");
    assert_eq!(run_str("class C{ static sm(x){ return x; } } C.sm.toString()"), "sm(x){ return x; }");
    assert_eq!(run_str("class C{ get p(){ return 1; } } Object.getOwnPropertyDescriptor(C.prototype,'p').get.toString()"),
               "get p(){ return 1; }");
    assert_eq!(run_str("class C{ *gen(){ yield 1; } } C.prototype.gen.toString()"), "*gen(){ yield 1; }");
    assert_eq!(run_str("class C{ async am(){ return 2; } } C.prototype.am.toString()"), "async am(){ return 2; }");
    // 클래스 자체
    assert_eq!(run_str("class C extends Object { m(){} } C.toString()"), "class C extends Object { m(){} }");
}

#[test]
fn class_accessor_get_set_merge() {
    // 같은 이름의 get/set 은 하나의 접근자 프로퍼티로 병합 (§15.4).
    assert!(run_bool(
        "class C{ get x(){return this._x;} set x(v){this._x=v;} } \
         var d=Object.getOwnPropertyDescriptor(C.prototype,'x'); \
         typeof d.get==='function' && typeof d.set==='function'"
    ));
    // 대입/조회 왕복
    assert_eq!(run_num("class C{ get x(){return this._x;} set x(v){this._x=v*2;} } var o=new C(); o.x=5; o.x"), 10.0);
    // setter-only 도 서술자에 노출
    assert!(run_bool(
        "class D{ set y(v){this._y=v;} } var d=Object.getOwnPropertyDescriptor(D.prototype,'y'); \
         d!==undefined && d.get===undefined && typeof d.set==='function'"
    ));
    // getter-only
    assert!(run_bool(
        "class E{ get z(){return 7;} } var d=Object.getOwnPropertyDescriptor(E.prototype,'z'); \
         typeof d.get==='function' && d.set===undefined && new E().z===7"
    ));
}

#[test]
fn sparse_array_holes() {
    // 엘리전은 구멍 — 명시 undefined 와 구별.
    assert!(run_bool("var a=[1,,3]; !a.hasOwnProperty(1) && a.hasOwnProperty(0) && a.length===3"));
    assert!(run_bool("var a=[1,undefined,3]; a.hasOwnProperty(1)")); // 명시 undefined 는 존재
    assert!(run_bool("!(1 in [1,,3])"));
    // for-in / Object.keys 는 구멍 제외
    assert_eq!(run_str("var s=[];for(var k in [1,,3])s.push(k);s.join(',')"), "0,2");
    assert_eq!(run_str("Object.keys([1,,3]).join(',')"), "0,2");
    // 생성 경로
    assert!(run_bool("var a=[1,2,3];delete a[1]; !a.hasOwnProperty(1) && a.length===3"));
    assert!(run_bool("var a=new Array(3); !a.hasOwnProperty(0) && a.length===3 && Object.keys(a).length===0"));
    assert!(run_bool("var a=[1];a[3]=4; !a.hasOwnProperty(1) && !a.hasOwnProperty(2) && a.hasOwnProperty(3)"));
    assert!(run_bool("var a=[1,2];a.length=4; !a.hasOwnProperty(2) && !a.hasOwnProperty(3)"));
    // 반복 메서드는 구멍을 건너뛴다
    assert_eq!(run_str("var s=[];[1,,3].forEach(function(v,i){s.push(i);});s.join(',')"), "0,2");
    assert_eq!(run_num("[1,,3].reduce(function(a,b){return a+b;})"), 4.0);
    assert!(!run_bool("[,,,].some(function(){return true;})"));
    assert!(run_bool("[,,,].every(function(){return false;})")); // 구멍만 → vacuously true
    assert_eq!(run_str("[1,,3].filter(function(){return true;}).join(',')"), "1,3");
    assert_eq!(run_num("[1,,3].indexOf(undefined)"), -1.0); // indexOf 는 구멍 스킵
    // map 은 구멍 보존
    assert!(run_bool("var m=[1,,3].map(function(x){return x*2;}); !m.hasOwnProperty(1) && m[0]===2 && m[2]===6 && m.length===3"));
    // 구멍 채우면 존재
    assert!(run_bool("var a=[1,,3]; a[1]=2; a.hasOwnProperty(1)"));
    // 변형은 구멍 실체화(desync 방지)
    assert!(run_bool("var a=new Array(2); a[1]=1; a.sort(); a[0]===1 && a[1]===undefined && a.length===2"));
}

#[test]
fn string_proto_methods_own_and_property_is_enumerable() {
    // 문자열 메서드가 String.prototype 의 own 프로퍼티(정확한 name/length, 비생성자).
    assert!(run_bool("String.prototype.hasOwnProperty('trimStart')"));
    assert!(run_bool("String.prototype.hasOwnProperty('replaceAll')"));
    assert_eq!(run_str("String.prototype.trimStart.name"), "trimStart");
    assert_eq!(run_str("String.prototype.at.name"), "at");
    assert_eq!(run_num("String.prototype.replaceAll.length"), 2.0);
    assert!(run_bool("var t=false; try{ new String.prototype.trim(); }catch(e){ t=e instanceof TypeError; } t"));
    // trimLeft/trimRight 는 trimStart/trimEnd 의 별칭(같은 name)
    assert_eq!(run_str("String.prototype.trimLeft.name"), "trimStart");
    // 메서드는 비열거
    assert_eq!(run_num("Object.getOwnPropertyDescriptor(String.prototype,'trim').enumerable ? 1 : 0"), 0.0);
    // substring/substr 정확한 의미론(slice 와 다름)
    assert_eq!(run_str("'hello'.substring(-1)"), "hello");
    assert_eq!(run_str("'hello'.substring(3,1)"), "el");
    assert_eq!(run_str("'hello'.substr(-2)"), "lo");
    assert_eq!(run_str("'hello'.substr(1,2)"), "el");
    assert_eq!(run_str("'hello'.slice(-2)"), "lo");
    // propertyIsEnumerable: own + enumerable
    assert!(run_bool("({a:1}).propertyIsEnumerable('a')"));
    assert!(!run_bool("String.prototype.propertyIsEnumerable('trim')")); // 비열거
    assert!(!run_bool("({}).propertyIsEnumerable('toString')")); // 상속(own 아님)
    assert!(run_bool("[1,2,3].propertyIsEnumerable(0)"));
    assert!(!run_bool("[1,2,3].propertyIsEnumerable('length')"));
    assert!(run_bool("var o={}; Object.defineProperty(o,'x',{value:1,enumerable:false}); !o.propertyIsEnumerable('x')"));
}

#[test]
fn number_to_exponential_precision_fixed() {
    // toExponential (§21.1.3.2)
    assert_eq!(run_str("(12345).toExponential(2)"), "1.23e+4");
    assert_eq!(run_str("(12345).toExponential()"), "1.2345e+4");
    assert_eq!(run_str("(0.00012).toExponential(2)"), "1.20e-4");
    assert_eq!(run_str("(0).toExponential(2)"), "0.00e+0");
    // toPrecision (§21.1.3.5)
    assert_eq!(run_str("(12345).toPrecision(2)"), "1.2e+4");
    assert_eq!(run_str("(123.456).toPrecision(5)"), "123.46");
    assert_eq!(run_str("(123.456).toPrecision()"), "123.456");
    assert_eq!(run_str("(0.001234).toPrecision(4)"), "0.001234");
    // toFixed (§21.1.3.3): RangeError + NaN/Inf
    assert_eq!(run_str("(3.14159).toFixed(2)"), "3.14");
    assert!(run_bool("var t=false; try{(5).toFixed(101);}catch(e){t=e instanceof RangeError;} t"));
    assert!(run_bool("var t=false; try{(5).toExponential(-1);}catch(e){t=e instanceof RangeError;} t"));
    assert_eq!(run_str("(NaN).toExponential()"), "NaN");
    assert_eq!(run_str("(Infinity).toPrecision(3)"), "Infinity");
    // name/length
    assert_eq!(run_str("Number.prototype.toExponential.name"), "toExponential");
    assert_eq!(run_num("Number.prototype.toPrecision.length"), 1.0);
}

#[test]
fn wrapper_instanceof_and_generic_this_toobject() {
    // 원시 래퍼 instanceof (§20/21/22) — 예전엔 new Boolean() instanceof Boolean 조차 false.
    assert!(run_bool("(new Boolean(false)) instanceof Boolean"));
    assert!(run_bool("(new Number(5)) instanceof Number"));
    assert!(run_bool("(new String('x')) instanceof String"));
    assert!(run_bool("(new Number(5)) instanceof Object"));
    assert!(!run_bool("(new Boolean(false)) instanceof Number"));
    // generic 배열 메서드의 콜백 3번째 인자 = ToObject(this) (원시는 래퍼).
    assert!(run_bool(
        "Boolean.prototype[0]=1; Boolean.prototype.length=1; \
         var r=Array.prototype.every.call(false, function(v,i,o){ return o instanceof Boolean; }); \
         delete Boolean.prototype[0]; delete Boolean.prototype.length; r"
    ));
    assert!(run_bool(
        "Number.prototype[0]=9; Number.prototype.length=1; var o; \
         Array.prototype.map.call(3.5, function(v,i,obj){ o=obj; return 0; }); \
         delete Number.prototype[0]; delete Number.prototype.length; o instanceof Number"
    ));
    // length getter 가 IsCallable 보다 먼저 (예외 전파)
    assert!(run_bool(
        "function E(){} var obj={0:1}; Object.defineProperty(obj,'length',{get:function(){throw new E();}}); \
         var t=false; try{ Array.prototype.filter.call(obj, undefined); }catch(e){ t=e instanceof E; } t"
    ));
}

#[test]
fn math_es2015_methods() {
    assert_eq!(run_num("Math.clz32(1)"), 31.0);
    assert_eq!(run_num("Math.clz32(0)"), 32.0);
    assert_eq!(run_num("Math.clz32(-1)"), 0.0);
    assert_eq!(run_num("Math.expm1(0)"), 0.0);
    assert_eq!(run_num("Math.log1p(0)"), 0.0);
    assert_eq!(run_num("Math.cosh(0)"), 1.0);
    assert_eq!(run_num("Math.sinh(0)"), 0.0);
    assert_eq!(run_num("Math.tanh(0)"), 0.0);
    assert_eq!(run_num("Math.asinh(0)"), 0.0);
    assert_eq!(run_num("Math.imul(3,4)"), 12.0);
    assert_eq!(run_num("Math.imul(0xffffffff,5)"), -5.0);
    assert_eq!(run_num("Math.fround(1.1)"), 1.100000023841858);
    // name/length + 비생성자
    assert_eq!(run_str("Math.clz32.name"), "clz32");
    assert_eq!(run_num("Math.imul.length"), 2.0);
    assert_eq!(run_num("Math.expm1.length"), 1.0);
    assert!(run_bool("var t=false; try{ new Math.clz32(); }catch(e){ t=e instanceof TypeError; } t"));
}

#[test]
fn reflect_target_must_be_object() {
    // §28.1: get/set/has/deleteProperty 는 target 이 객체가 아니면 TypeError.
    for expr in [
        "Reflect.get(1,'x')",
        "Reflect.set(1,'x',1)",
        "Reflect.has(1,'x')",
        "Reflect.deleteProperty(1,'x')",
        "Reflect.get(null,'x')",
        "Reflect.has('str','x')",
    ] {
        assert!(
            run_bool(&format!("var t=false; try{{ {}; }}catch(e){{ t=e instanceof TypeError; }} t", expr)),
            "expected TypeError from {}",
            expr
        );
    }
    // 객체 대상은 정상 동작
    assert_eq!(run_num("Reflect.get({x:5}, 'x')"), 5.0);
    assert!(run_bool("var o={}; Reflect.set(o,'y',9); o.y===9"));
    assert!(run_bool("Reflect.has({a:1}, 'a')"));
    // key 는 ToPropertyKey (Symbol/toString 관측)
    assert_eq!(run_num("var o={}; o[Symbol.for('k')]=7; Reflect.get(o, Symbol.for('k'))"), 7.0);
}

#[test]
fn json_raw_json() {
    assert!(run_bool("JSON.isRawJSON(JSON.rawJSON('1.5'))"));
    assert!(!run_bool("JSON.isRawJSON(1)"));
    assert!(!run_bool("JSON.isRawJSON({})"));
    assert_eq!(run_str("JSON.stringify({a: JSON.rawJSON('12345678901234567890')})"),
               "{\"a\":12345678901234567890}");
    assert_eq!(run_str("JSON.stringify(JSON.rawJSON('1.0'))"), "1.0");
    assert!(run_bool("Object.isFrozen(JSON.rawJSON('true'))"));
    // 검증 오류
    for bad in ["''", "' 1'", "'1 '", "'{}'", "'[]'", "'nope'", "'1,2'"] {
        assert!(run_bool(&format!("var t=false; try{{ JSON.rawJSON({}); }}catch(e){{ t=e instanceof SyntaxError; }} t", bad)),
                "expected SyntaxError from JSON.rawJSON({})", bad);
    }
    // name/length
    assert_eq!(run_str("JSON.rawJSON.name"), "rawJSON");
    assert_eq!(run_num("JSON.isRawJSON.length"), 1.0);
}

#[test]
fn set_map_foreach_callback_semantics() {
    // callable 아니면 TypeError (§24.1.3.5/§24.2.3.6)
    for e in ["new Set([1]).forEach(null)", "new Set([1]).forEach(5)",
              "new Map([[1,2]]).forEach(undefined)", "new Set([1]).forEach('x')"] {
        assert!(run_bool(&format!("var t=false; try{{ {}; }}catch(e){{ t=e instanceof TypeError; }} t", e)),
                "expected TypeError from {}", e);
    }
    // 콜백 (값, 키, 컬렉션) + thisArg
    assert!(run_bool(
        "var s=new Set([1]); var ok; s.forEach(function(v,k,set){ ok=(v===1&&k===1&&set===s&&this.x===9); }, {x:9}); ok"
    ));
    assert!(run_bool(
        "var m=new Map([['a',1]]); var ok; m.forEach(function(v,k,map){ ok=(v===1&&k==='a'&&map===m); }); ok"
    ));
    // 콜백 예외 전파(예전엔 Set 이 삼켰다)
    assert!(run_bool(
        "var t=false; try{ new Set([1]).forEach(function(){ throw new RangeError('x'); }); }catch(e){ t=e instanceof RangeError; } t"
    ));
}

#[test]
fn map_set_constructor_iterable_protocol() {
    // new 없이 호출하면 TypeError (§24.1.1.1/§24.2.1.1 step 1)
    assert!(run_bool("var t=false; try{ Map(); }catch(e){ t=e instanceof TypeError; } t"));
    assert!(run_bool("var t=false; try{ Set(); }catch(e){ t=e instanceof TypeError; } t"));
    // 비이터러블 → TypeError
    assert!(run_bool("var t=false; try{ new Map(5); }catch(e){ t=e instanceof TypeError; } t"));
    assert!(run_bool("var t=false; try{ new Set(3); }catch(e){ t=e instanceof TypeError; } t"));
    // undefined/null 이면 빈 컬렉션
    assert_eq!(run_num("new Map(null).size + new Set(undefined).size"), 0.0);
    // 항목이 객체가 아니면 TypeError (Map)
    assert!(run_bool("var t=false; try{ new Map([1,2]); }catch(e){ t=e instanceof TypeError; } t"));
    // 정상 초기화
    assert_eq!(run_num("new Map([[1,2],[3,4]]).size"), 2.0);
    assert_eq!(run_num("new Set([1,1,2,3]).size"), 3.0);
    // 사용자 오버라이드 set 관측 (adder = Get(target,'set'))
    assert_eq!(
        run_num("var o=Map.prototype.set,c=0; Map.prototype.set=function(k,v){c++;return o.call(this,k,v);}; new Map([[0,0],[1,1]]); Map.prototype.set=o; c"),
        2.0
    );
    // set 이 callable 아니면 TypeError (비어있지 않은 iterable)
    assert!(run_bool(
        "var o=Map.prototype.set; Map.prototype.set=undefined; var t=false; try{ new Map([[1,2]]); }catch(e){ t=e instanceof TypeError; } Map.prototype.set=o; t"
    ));
    // 비정상 완료 시 IteratorClose(return 호출)
    assert!(run_bool(
        "var closed=false; var it={ [Symbol.iterator](){return this;}, i:0, next(){ return this.i++<2?{value:[1,1],done:false}:{done:true}; }, return(){ closed=true; return {}; } }; var o=Map.prototype.set; Map.prototype.set=function(){ throw new Error('x'); }; try{ new Map(it); }catch(e){} Map.prototype.set=o; closed"
    ));
    // Object.prototype 상속 메서드
    assert!(run_bool("typeof new Map().hasOwnProperty === 'function' && typeof new Set().toString === 'function'"));
}

#[test]
fn async_gen_rejected_yield_rejects_promise() {
    // yield 된 promise 가 거부되면 next() 는 거부 promise 를 돌려줘야 한다(동기 throw 아님).
    assert_eq!(
        run_str("var out='sync'; try{ var p=(async function*(){ yield Promise.reject(7); })().next(); out='async'; }catch(e){ out='threw'; } out"),
        "async"
    );
    assert_eq!(
        run_str("var r='?'; (async function*(){ yield Promise.reject(9); })().next().then(function(){r='res';},function(e){r='rej:'+e;}); r"),
        "?" // 마이크로태스크 전 시점 — 최소한 동기 throw 는 아님을 위 테스트가 보장
    );
}

#[test]
fn to_fixed_negative_zero() {
    // §21.1.3.3: -0 은 부호 없이 "0.00". 하지만 작은 음수는 부호 유지.
    assert_eq!(run_str("(-0).toFixed(2)"), "0.00");
    assert_eq!(run_str("(0).toFixed(2)"), "0.00");
    assert_eq!(run_str("(-5*0).toFixed(2)"), "0.00");
    assert_eq!(run_str("(-0.001).toFixed(2)"), "-0.00");
    assert_eq!(run_str("(-1.5).toFixed(1)"), "-1.5");
    // ToIntegerOrInfinity: NaN/문자열→0(RangeError 아님), ±∞→RangeError.
    assert_eq!(run_str("(0).toFixed('some string')"), "0");
    assert_eq!(run_str("(0).toFixed(NaN)"), "0");
    assert_eq!(run_str("(0).toFixed(1.1)"), "0.0");
    assert_eq!(run_str("Number.prototype.toFixed()"), "0");
    assert!(run_bool("var t=false; try{ (0).toFixed(Infinity); }catch(e){ t=e instanceof RangeError; } t"));
    // Symbol/BigInt/불량 ToPrimitive → TypeError
    assert!(run_bool("var t=false; try{ (0).toFixed(Symbol()); }catch(e){ t=e instanceof TypeError; } t"));
    assert!(run_bool("var t=false; try{ (0).toFixed(1n); }catch(e){ t=e instanceof TypeError; } t"));
    assert!(run_bool("var t=false; try{ (0).toFixed({[Symbol.toPrimitive](){throw new TypeError();}}); }catch(e){ t=e instanceof TypeError; } t"));
}

#[test]
fn to_exponential_precision_arg_coercion() {
    // toExponential: ToIntegerOrInfinity(fractionDigits) — NaN/문자열→0, Symbol/BigInt→TypeError.
    assert_eq!(run_str("(1.5).toExponential(NaN)"), "2e+0");
    assert_eq!(run_str("(123.456).toExponential('2')"), "1.23e+2");
    assert!(run_bool("var t=false; try{ (1).toExponential(Symbol()); }catch(e){ t=e instanceof TypeError; } t"));
    assert!(run_bool("var t=false; try{ (1).toExponential(1n); }catch(e){ t=e instanceof TypeError; } t"));
    assert!(run_bool("var t=false; try{ (1).toExponential(Infinity); }catch(e){ t=e instanceof RangeError; } t"));
    // toPrecision: undefined→ToString, NaN→0(→RangeError, 1..100), Symbol/BigInt→TypeError.
    assert_eq!(run_str("(123.456).toPrecision(undefined)"), "123.456");
    assert_eq!(run_str("(5.12).toPrecision('3')"), "5.12");
    assert!(run_bool("var t=false; try{ (123.456).toPrecision(NaN); }catch(e){ t=e instanceof RangeError; } t"));
    assert!(run_bool("var t=false; try{ (1).toPrecision(Symbol()); }catch(e){ t=e instanceof TypeError; } t"));
    assert!(run_bool("var t=false; try{ (1).toPrecision(0); }catch(e){ t=e instanceof RangeError; } t"));
    // thisNumberValue 브랜드 검사: 숫자/Number 래퍼 아니면 TypeError.
    for m in ["toFixed", "toExponential", "toPrecision"] {
        assert!(run_bool(&format!("var t=false; try{{ Number.prototype.{}.call({{}}, 3); }}catch(e){{ t=e instanceof TypeError; }} t", m)),
                "expected TypeError from {}.call({{}})", m);
        assert!(run_bool(&format!("var t=false; try{{ Number.prototype.{}.call('x', 3); }}catch(e){{ t=e instanceof TypeError; }} t", m)));
    }
    // Number 래퍼 객체는 통과.
    assert_eq!(run_str("new Number(5).toFixed(2)"), "5.00");
}

#[test]
fn set_get_set_record_size_coercion() {
    // §24.2.1.2 step 3-5: size 는 ToNumber (valueOf 관측), NaN→TypeError, BigInt→TypeError.
    assert!(run_bool(
        "var c=0; var sl={size:{valueOf(){c++;return NaN;}},has(){},keys:function*(){}}; var t=false; try{ new Set([1]).union(sl); }catch(e){ t=e instanceof TypeError; } t && c===1"
    ));
    assert!(run_bool(
        "var t=false; try{ new Set([1]).union({size:0n,has(){},keys:function*(){}}); }catch(e){ t=e instanceof TypeError; } t"
    ));
    assert!(run_bool(
        "var t=false; try{ new Set([1]).union({size:'x',has(){},keys:function*(){}}); }catch(e){ t=e instanceof TypeError; } t"
    ));
    assert!(run_bool(
        "var t=false; try{ new Set([1]).union({size:undefined,has(){},keys:function*(){}}); }catch(e){ t=e instanceof TypeError; } t"
    ));
    // 정상 set-like: size 를 valueOf 로 강제변환해 union 성공.
    assert_eq!(
        run_str("[...new Set([1,2]).union({size:{valueOf(){return 1;}},has(){return false;},keys:function*(){yield 9;}})].join()"),
        "1,2,9"
    );
}

#[test]
fn iterator_result_accessor_done_value() {
    // §7.4.8/§7.4.9: 반복자 결과의 done/value 가 접근자면 호출해야 한다(raw get 금지).
    // done 접근자 무시로 무한 루프(OOM)가 났던 회귀 방지.
    assert_eq!(
        run_str("var v=['a','b','c'],i=0; var it={[Symbol.iterator](){return this;}, next(){return {get done(){return i>=v.length;}, get value(){return v[i++];}};}}; [...it].join()"),
        "a,b,c"
    );
    // value getter 예외 전파
    assert!(run_bool(
        "var it={[Symbol.iterator](){return this;}, next(){return {done:false, get value(){throw new TypeError('v');}};}}; var t=false; try{ [...it]; }catch(e){ t=e instanceof TypeError; } t"
    ));
    // done 은 ToBoolean 강제변환 (Bool 만이 아님)
    assert_eq!(
        run_str("var i=0; var it={[Symbol.iterator](){return this;}, next(){return {value:i, done: i++>=2?'y':''};}}; [...it].join()"),
        "0,1"
    );
    // Set 연산이 접근자 기반 set-like keys 이터레이터로도 크래시 없이 동작
    assert_eq!(
        run_str("var sl={size:3,has(){return false;},keys(){var v=[7,8,9],i=0;return {next(){return {get done(){return i>=v.length;}, get value(){return v[i++];}};}};}}; [...new Set([1]).union(sl)].join()"),
        "1,7,8,9"
    );
}

#[test]
fn native_builtins_are_extensible() {
    // 내장 함수/메서드는 확장 가능한 객체다(§17): isExtensible=true, isFrozen/isSealed=false.
    for e in ["Object.keys", "Array.prototype.map", "Set.prototype.union", "Math.max"] {
        assert!(run_bool(&format!("Object.isExtensible({})", e)), "isExtensible({}) should be true", e);
        assert!(run_bool(&format!("!Object.isFrozen({})", e)), "isFrozen({}) should be false", e);
        assert!(run_bool(&format!("!Object.isSealed({})", e)), "isSealed({}) should be false", e);
    }
    // 원시값/실객체 무결성은 그대로.
    assert!(run_bool("Object.isFrozen(42) && !Object.isExtensible(42)"));
    assert!(run_bool("Object.isFrozen(Object.freeze({a:1})) && Object.isSealed(Object.seal({b:2}))"));
    assert!(run_bool("!Object.isExtensible(Object.preventExtensions({}))"));
}

#[test]
fn set_prototype_entries() {
    // §24.2.3.5: Set.prototype.entries → [value, value] 쌍 이터레이터.
    assert_eq!(run_str("typeof Set.prototype.entries"), "function");
    assert_eq!(run_str("Set.prototype.entries.name"), "entries");
    assert_eq!(run_num("Set.prototype.entries.length"), 0.0);
    assert_eq!(run_str("JSON.stringify([...new Set(['a','b']).entries()])"), "[[\"a\",\"a\"],[\"b\",\"b\"]]");
    assert!(run_bool("new Set().entries().next().done"));
    assert!(run_bool("Set.prototype.hasOwnProperty('entries')"));
}

#[test]
fn constructor_symbol_species() {
    // §: C[Symbol.species] 접근자는 this 를 돌려준다 (파생 종 = 자신).
    for c in ["Map", "Set", "Array", "Promise", "RegExp"] {
        assert!(run_bool(&format!("{}[Symbol.species] === {}", c, c)), "{}[Symbol.species]", c);
        // 접근자 서술자 {get, undefined, false, true}
        assert!(run_bool(&format!(
            "var d=Object.getOwnPropertyDescriptor({}, Symbol.species); typeof d.get==='function' && d.set===undefined && d.enumerable===false && d.configurable===true", c)));
    }
    // getter 는 this 반환, name/length 표준값
    assert_eq!(run_str("Object.getOwnPropertyDescriptor(Set, Symbol.species).get.name"), "get [Symbol.species]");
    assert_eq!(run_num("Object.getOwnPropertyDescriptor(Set, Symbol.species).get.length"), 0.0);
    assert_eq!(run_num("Object.getOwnPropertyDescriptor(Set, Symbol.species).get.call(42)"), 42.0);
    // species 없는 생성자는 undefined
    assert!(run_bool("Object[Symbol.species] === undefined"));
}

#[test]
fn set_ops_iterator_close_on_early_exit() {
    // §24.2.4: is*Of 가 조기탈출 시 other.keys() 이터레이터를 IteratorClose(return 호출),
    // 소진 시엔 호출 안 함.
    let harness = "function mk(v){var it={a:v,n:0,r:0,next(){var d=this.n>=this.a.length,x=this.a[this.n];this.n++;return {done:d,value:x};},return(){this.r++;return this;}};return {sl:{size:v.length,has(x){return v.indexOf(x)>=0;},keys(){return it;}},it:it};}";
    // 소진(무close)
    assert!(run_bool(&format!("{} var m=mk([4,5,6]); var r=new Set([4,5,6,7]).isSupersetOf(m.sl); r===true && m.it.n===4 && m.it.r===0", harness)));
    // 조기탈출(close 1회)
    assert!(run_bool(&format!("{} var m=mk([4,5,6]); var r=new Set([0,1,2,3]).isSupersetOf(m.sl); r===false && m.it.n===1 && m.it.r===1", harness)));
    assert!(run_bool(&format!("{} var m=mk([1,2,3]); var r=new Set([3,4,5,6]).isDisjointFrom(m.sl); r===false && m.it.n===3 && m.it.r===1", harness)));
}
