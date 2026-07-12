// call_native: 모든 네이티브(내장) 메서드/함수 디스패치. interp/mod.rs 에서 분리.
use super::*;
use super::value::*;
use std::cell::RefCell;
use std::rc::Rc;

// URI 인코딩: 비예약문자(A-Za-z0-9 -_.!~*'()) 와 extra_safe 는 보존, 나머지는
// UTF-8 바이트별 %XX. encodeURI 는 예약문자를 extra_safe 로 넘겨 보존한다.
fn uri_encode(s: &str, extra_safe: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() || "-_.!~*'()".contains(ch) || extra_safe.contains(ch) {
            out.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for &b in ch.encode_utf8(&mut buf).as_bytes() {
                out.push('%');
                out.push(char::from_digit((b >> 4) as u32, 16).unwrap().to_ascii_uppercase());
                out.push(char::from_digit((b & 0xf) as u32, 16).unwrap().to_ascii_uppercase());
            }
        }
    }
    out
}

// URI 디코딩: %XX 를 바이트로 모아 UTF-8 해석. 유효하지 않은 %는 그대로 통과.
fn uri_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// 수신자 객체의 문자열 프로퍼티 (URL/URLSearchParams 네이티브 메서드용).
fn recv_prop_str(recv: &Option<Value>, key: &str) -> String {
    if let Some(Value::Obj(o)) = recv {
        if let Some(v) = o.borrow().get(key) {
            return to_display(v);
        }
    }
    String::new()
}

// (키,값) 쌍을 x-www-form-urlencoded 쿼리 문자열로 (각 성분 encodeURIComponent).
fn build_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k, ""), uri_encode(v, "")))
        .collect::<Vec<_>>()
        .join("&")
}

// application/x-www-form-urlencoded 쿼리를 (키,값) 쌍으로. '+' → 공백, %XX 디코드.
fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (uri_decode(&k.replace('+', " ")), uri_decode(&v.replace('+', " ")))
        })
        .collect()
}

// 컨텍스트의 __path 배열 조작 ([x0,y0,x1,y1,...] 평탄 저장).
fn set_path(ctx: &Rc<RefCell<ObjMap>>, pts: Vec<Value>) {
    ctx.borrow_mut().insert("__path".to_string(), Value::Arr(ArrayObj::new(pts)));
}
fn push_path(ctx: &Rc<RefCell<ObjMap>>, x: f32, y: f32) {
    if let Some(Value::Arr(a)) = ctx.borrow().get("__path") {
        a.borrow_mut().push(Value::Num(x as f64));
        a.borrow_mut().push(Value::Num(y as f64));
    }
}
fn get_path(ctx: &Rc<RefCell<ObjMap>>) -> Vec<(f32, f32)> {
    if let Some(Value::Arr(a)) = ctx.borrow().get("__path") {
        let flat = a.borrow();
        return flat
            .chunks(2)
            .filter(|c| c.len() == 2)
            .map(|c| (to_num(&c[0]) as f32, to_num(&c[1]) as f32))
            .collect();
    }
    Vec::new()
}

// font 문자열에서 px 크기 추출 ("bold 16px sans-serif" → 16). 없으면 10.
fn font_px(font: &str) -> f32 {
    for tok in font.split_whitespace() {
        if let Some(n) = tok.strip_suffix("px") {
            if let Ok(v) = n.parse::<f32>() {
                return v;
            }
        }
    }
    10.0
}

// DocumentFragment 판별 (센티널 태그)
fn is_fragment(dom: &crate::dom::Dom, id: crate::dom::NodeId) -> bool {
    matches!(&dom.get(id).node_type,
        crate::dom::NodeType::Element(e) if e.tag_name == "#document-fragment")
}

impl Interp {
    // canvas 2D 컨텍스트 메서드 처리. recv=컨텍스트 객체. ops 를 canvas_cmds 에 쌓는다.
    fn canvas_method(
        &mut self,
        method: CanvasMethod,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        use CanvasMethod::*;
        let Some(Value::Obj(ctx)) = recv else { return Ok(Value::Undefined) };
        let canvas_id = match ctx.borrow().get("__canvas") {
            Some(Value::Num(n)) => *n as crate::dom::NodeId,
            _ => return Ok(Value::Undefined),
        };
        let num = |i: usize| args.get(i).map(to_num).unwrap_or(0.0) as f32;
        let style = |key: &str| -> crate::css::Color {
            match ctx.borrow().get(key) {
                Some(Value::Str(s)) => {
                    crate::css::parse_color(s).unwrap_or(crate::css::Color { r: 0, g: 0, b: 0, a: 255 })
                }
                _ => crate::css::Color { r: 0, g: 0, b: 0, a: 255 },
            }
        };
        let ops = self.canvas_cmds.entry(canvas_id).or_default();
        match method {
            FillRect => ops.push(CanvasOp::FillRect {
                x: num(0), y: num(1), w: num(2), h: num(3), color: style("fillStyle"),
            }),
            ClearRect => ops.push(CanvasOp::ClearRect { x: num(0), y: num(1), w: num(2), h: num(3) }),
            StrokeRect => {
                let lw = match ctx.borrow().get("lineWidth") { Some(Value::Num(n)) => *n as f32, _ => 1.0 };
                ops.push(CanvasOp::StrokeRect {
                    x: num(0), y: num(1), w: num(2), h: num(3), color: style("strokeStyle"), lw,
                });
            }
            FillText => {
                let text = args.first().map(to_display).unwrap_or_default();
                let px = match ctx.borrow().get("font") { Some(Value::Str(f)) => font_px(f), _ => 10.0 };
                ops.push(CanvasOp::FillText { text, x: num(0), y: num(1), color: style("fillStyle"), px });
            }
            // 경로: __path 에 점을 쌓았다가 fill 시 폴리곤으로.
            BeginPath => set_path(&ctx, Vec::new()),
            MoveTo | LineTo => push_path(&ctx, num(0), num(1)),
            Rect => {
                // 사각형 경로(4모서리) 추가
                let (x, y, w, h) = (num(0), num(1), num(2), num(3));
                for (px, py) in [(x, y), (x + w, y), (x + w, y + h), (x, y + h)] {
                    push_path(&ctx, px, py);
                }
            }
            Arc => {
                // (cx, cy, r, start, end) 를 선분으로 근사해 경로에 추가
                let (cx, cy, r) = (num(0), num(1), num(2));
                let (s, e) = (num(3), num(4));
                let seg = 24;
                for k in 0..=seg {
                    let t = s + (e - s) * k as f32 / seg as f32;
                    push_path(&ctx, cx + r * t.cos(), cy + r * t.sin());
                }
            }
            ClosePath => {}
            Fill => {
                let pts = get_path(&ctx);
                if pts.len() >= 3 {
                    self.canvas_cmds.entry(canvas_id).or_default().push(CanvasOp::FillPath {
                        pts,
                        color: style("fillStyle"),
                    });
                }
            }
            Stroke => {} // 경로 스트로크는 미지원(근사로 생략)
            Noop => {}
        }
        Ok(Value::Undefined)
    }

    // 정규식 매치 → [full, g1, ...] 배열 (+ index/input/groups own-property)
    pub(super) fn regex_match_array(
        &self,
        chars: &[char],
        mt: &crate::js::regex::Match,
        group_names: &[(String, usize)],
    ) -> Value {
        let mut items = Vec::new();
        for g in &mt.groups {
            match g {
                Some((a, b)) => items.push(Value::Str(chars[*a..*b].iter().collect())),
                None => items.push(Value::Undefined),
            }
        }
        let arr = ArrayObj::new(items);
        // index 는 UTF-16 코드 유닛 오프셋(정규식 엔진은 코드포인트 인덱스).
        let u16_index: usize = chars[..mt.start].iter().map(|c| c.len_utf16()).sum();
        arr.set_prop("index".to_string(), Value::Num(u16_index as f64));
        arr.set_prop("input".to_string(), Value::Str(chars.iter().collect()));
        // groups: 이름 있는 그룹의 {name: value} (없으면 undefined)
        let groups = if group_names.is_empty() {
            Value::Undefined
        } else {
            let mut g = ObjMap::new();
            for (name, idx) in group_names {
                let v = mt
                    .groups
                    .get(*idx)
                    .and_then(|o| o.as_ref())
                    .map(|(a, b)| Value::Str(chars[*a..*b].iter().collect()))
                    .unwrap_or(Value::Undefined);
                g.insert(name.clone(), v);
            }
            Value::Obj(Rc::new(RefCell::new(g)))
        };
        arr.set_prop("groups".to_string(), groups);
        Value::Arr(arr)
    }

    // str.replace/replaceAll: 패턴(문자열/정규식) + 치환(문자열/함수)
    fn str_replace(
        &mut self,
        s: &str,
        pat: &Value,
        repl: &Value,
        all: bool,
    ) -> Result<String, String> {
        let chars: Vec<char> = s.chars().collect();
        if let Some((src, flags)) = regex_src_flags(pat) {
            let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                .map_err(|e| format!("정규식: {}", e))?;
            let global = all || re.global;
            let mut out = String::new();
            let mut pos = 0usize;
            loop {
                match re.find(&chars, pos) {
                    Some(mt) => {
                        out.extend(&chars[pos..mt.start]);
                        let rep = if is_callable(repl) {
                            let mut cargs =
                                vec![Value::Str(chars[mt.start..mt.end].iter().collect())];
                            for g in mt.groups.iter().skip(1) {
                                cargs.push(match g {
                                    Some((a, b)) => Value::Str(chars[*a..*b].iter().collect()),
                                    None => Value::Undefined,
                                });
                            }
                            cargs.push(Value::Num(mt.start as f64));
                            cargs.push(Value::Str(s.to_string()));
                            to_display(&self.call_value(repl.clone(), None, cargs)?)
                        } else {
                            expand_replacement(&to_display(repl), &chars, &mt)
                        };
                        out.push_str(&rep);
                        // 빈 매치는 한 글자 진행(무한 루프 방지)
                        if mt.end > mt.start {
                            pos = mt.end;
                        } else {
                            if mt.end < chars.len() {
                                out.push(chars[mt.end]);
                            }
                            pos = mt.end + 1;
                        }
                        if !global {
                            out.extend(chars.get(pos.min(chars.len())..).unwrap_or(&[]));
                            break;
                        }
                        if pos > chars.len() {
                            break;
                        }
                    }
                    None => {
                        out.extend(chars.get(pos.min(chars.len())..).unwrap_or(&[]));
                        break;
                    }
                }
            }
            Ok(out)
        } else {
            let needle = to_display(pat);
            if needle.is_empty() {
                return Ok(s.to_string());
            }
            let rep_fn = is_callable(repl);
            let mut out = String::new();
            let mut rest = s;
            let mut consumed = 0usize;
            loop {
                match rest.find(&needle) {
                    Some(idx) => {
                        out.push_str(&rest[..idx]);
                        let at = consumed + idx;
                        if rep_fn {
                            let r = self.call_value(
                                repl.clone(),
                                None,
                                vec![
                                    Value::Str(needle.clone()),
                                    Value::Num(s[..at].encode_utf16().count() as f64), // UTF-16 인덱스
                                    Value::Str(s.to_string()),
                                ],
                            )?;
                            out.push_str(&to_display(&r));
                        } else {
                            out.push_str(&to_display(repl));
                        }
                        let adv = idx + needle.len();
                        consumed = at + needle.len();
                        rest = &rest[adv..];
                        if !all {
                            out.push_str(rest);
                            break;
                        }
                    }
                    None => {
                        out.push_str(rest);
                        break;
                    }
                }
            }
            Ok(out)
        }
    }

    // 상대 URL 을 페이지 기준으로 해석
    fn resolve_url(&self, url: &str) -> String {
        if let Some(base) = &self.base_url {
            if let Ok(b) = crate::url::Url::parse(base) {
                if let Some(u) = b.join(url) {
                    return u.as_string();
                }
            }
        }
        url.to_string()
    }

    // new XMLHttpRequest() → 메서드/속성을 가진 객체
    pub(super) fn make_xhr(&self) -> Value {
        let mut m = ObjMap::new();
        m.insert("__isXhr".to_string(), Value::Bool(true));
        m.insert("readyState".to_string(), Value::Num(0.0));
        m.insert("status".to_string(), Value::Num(0.0));
        m.insert("statusText".to_string(), Value::Str(String::new()));
        m.insert("responseText".to_string(), Value::Str(String::new()));
        m.insert("response".to_string(), Value::Str(String::new()));
        m.insert("responseType".to_string(), Value::Str(String::new()));
        m.insert("open".to_string(), Value::Native(Native::XhrOpen));
        m.insert("send".to_string(), Value::Native(Native::XhrSend));
        m.insert("setRequestHeader".to_string(), Value::Native(Native::XhrSetHeader));
        m.insert("getResponseHeader".to_string(), Value::Native(Native::XhrGetHeader));
        m.insert("getAllResponseHeaders".to_string(), Value::Native(Native::XhrGetHeader));
        m.insert("abort".to_string(), Value::Native(Native::Noop));
        m.insert("addEventListener".to_string(), Value::Native(Native::AddEventListener));
        m.insert("removeEventListener".to_string(), Value::Native(Native::Noop));
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // XHR 발화: on<event> 프로퍼티 + addEventListener 핸들러(요소 핸들러 재사용은 안 함)
    fn xhr_fire(&mut self, obj: &Rc<RefCell<ObjMap>>, event: &str) {
        let on = format!("on{}", event);
        let handler = obj.borrow().get(&on).cloned();
        if let Some(h) = handler {
            if is_callable(&h) {
                let _ = self.call_value(h, Some(Value::Obj(obj.clone())), Vec::new());
            }
        }
    }

    // new Date(...) / Date(...) 인자 처리
    fn make_date_from_args(&self, args: &[Value]) -> Value {
        let millis = match args.len() {
            0 => now_millis(),
            1 => match &args[0] {
                Value::Num(n) => *n,
                Value::Str(s) => parse_date_string(s).unwrap_or(f64::NAN),
                Value::Obj(m) if is_date_obj(m) => match m.borrow().get("__time") {
                    Some(Value::Num(n)) => *n,
                    _ => f64::NAN,
                },
                v => to_num(v),
            },
            _ => {
                let g = |i: usize, dflt: f64| args.get(i).map(to_num).unwrap_or(dflt);
                // (year, month[0기준], day, h, m, s, ms)
                date_to_millis(
                    g(0, 1970.0) as i64,
                    g(1, 0.0) as i64 + 1,
                    g(2, 1.0) as i64,
                    g(3, 0.0) as i64,
                    g(4, 0.0) as i64,
                    g(5, 0.0) as i64,
                    g(6, 0.0) as i64,
                )
            }
        };
        make_date(millis)
    }

    pub(super) fn call_native(
        &mut self,
        n: Native,
        recv: Option<Value>,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        match n {
            // Proxy 는 new 로만 생성 (construct 에서 처리). 함수 호출은 무의미.
            Native::ProxyCtor => Err("Proxy 는 new 로 생성해야 함".to_string()),
            Native::ConsoleLog => {
                let line = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.console.push(line);
                Ok(Value::Undefined)
            }
            Native::ArrayPush => match recv {
                Some(Value::Arr(a)) => {
                    for v in args {
                        a.borrow_mut().push(v);
                    }
                    Ok(Value::Num(a.borrow().len() as f64))
                }
                _ => Err("push 는 배열 메서드".to_string()),
            },
            Native::GetElementById => self.dom_get_element_by_id(args),
            Native::AddEventListener => match recv {
                Some(Value::Dom(id)) => {
                    let event = args.first().map(to_display).unwrap_or_default();
                    if let Some(f @ Value::Fn(_)) = args.get(1) {
                        self.handlers.push((id, event, f.clone()));
                    }
                    Ok(Value::Undefined)
                }
                _ => Err("addEventListener 는 요소 메서드".to_string()),
            },
            // document/window.addEventListener — 전역 핸들러로 등록 (recv 무시)
            Native::AddGlobalListener => {
                let event = args.first().map(to_display).unwrap_or_default();
                if let Some(f) = args.get(1) {
                    if is_callable(f) {
                        self.global_handlers.push((event, f.clone()));
                    }
                }
                Ok(Value::Undefined)
            }
            // fn.call(thisArg, a, b, ...) — recv 가 대상 함수
            Native::FnCall => {
                let target = recv.ok_or("call 대상 함수 없음")?;
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                self.call_value(target, Some(this_arg), it.collect())
            }
            // fn.apply(thisArg, [args]) — 두 번째 인자는 배열 또는 유사배열(arguments)
            Native::FnApply => {
                let target = recv.ok_or("apply 대상 함수 없음")?;
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                let call_args = match it.next() {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                self.call_value(target, Some(this_arg), call_args)
            }
            // fn.bind(thisArg, ...partial) → 바운드 함수
            Native::FnBind => {
                let target = recv.ok_or("bind 대상 함수 없음")?;
                let mut it = args.into_iter();
                let this_arg = it.next().unwrap_or(Value::Undefined);
                let partial: Vec<Value> = it.collect();
                Ok(Value::Bound(Rc::new((target, this_arg, partial))))
            }
            Native::FunctionCtor => self.make_function(args),
            // Object.defineProperty(target, key, {get|value}) — 접근자/값 정의
            Native::ObjectDefineProperty => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).map(to_display).unwrap_or_default();
                let entry = if let Some(Value::Obj(d)) = args.get(2) {
                    let d = d.borrow();
                    if let Some(g) = d.get("get") {
                        Some(Value::Getter(Rc::new(g.clone())))
                    } else {
                        d.get("value").cloned()
                    }
                } else {
                    None
                };
                if let Some(val) = entry {
                    match &target {
                        Value::Obj(map) => {
                            map.borrow_mut().insert(key, val);
                        }
                        // require.n 은 함수에 접근자를 정의 (getter.a = ...)
                        Value::Fn(func) => {
                            func.props.borrow_mut().insert(key, val);
                        }
                        _ => {}
                    }
                }
                Ok(target)
            }
            // Object.create(proto) — proto 의 얕은 복사 기반 새 객체 (관용)
            Native::ObjectCreate => {
                // proto 를 __proto__ 로 링크(스냅샷 복사 아님). Object.create(null) 은 링크 없음.
                let mut map = ObjMap::new();
                if let Some(p @ Value::Obj(_)) = args.first() {
                    map.insert("__proto__".to_string(), p.clone());
                }
                let obj = Value::Obj(Rc::new(RefCell::new(map)));
                // 2번째 인자(프로퍼티 서술자): {k: {value: v}} 를 얕게 반영(get/set 은 근사).
                if let Some(Value::Obj(descs)) = args.get(1) {
                    if let Value::Obj(m) = &obj {
                        for (k, d) in descs.borrow().iter() {
                            if let Value::Obj(dm) = d {
                                if let Some(v) = dm.borrow().get("value") {
                                    m.borrow_mut().insert(k.clone(), v.clone());
                                }
                            }
                        }
                    }
                }
                Ok(obj)
            }
            // freeze 는 그대로 반환(불변성 미구현)
            Native::ObjectFreeze => Ok(args.into_iter().next().unwrap_or(Value::Undefined)),
            // getPrototypeOf: 객체의 __proto__ 링크(없으면 null)
            Native::ObjectGetPrototypeOf => Ok(match args.first() {
                Some(Value::Obj(m)) => m.borrow().get("__proto__").cloned().unwrap_or(Value::Null),
                _ => Value::Null,
            }),
            // Object.prototype.hasOwnProperty.call(obj, key) / obj.hasOwnProperty(key)
            Native::HasOwnProperty => {
                let key = args.first().map(to_display).unwrap_or_default();
                let has = match &recv {
                    // __proto__ 는 own 프로퍼티 아님(상속 accessor)
                    Some(Value::Obj(m)) => !is_internal_key(&key) && m.borrow().contains_key(&key),
                    // 인스턴스는 own 필드만 own 프로퍼티(메서드는 프로토타입 격)
                    Some(Value::Instance(i)) => i.fields.borrow().contains_key(&key),
                    Some(Value::Arr(a)) => {
                        key.parse::<usize>().map(|i| i < a.borrow().len()).unwrap_or(false)
                    }
                    _ => false,
                };
                Ok(Value::Bool(has))
            }
            // Object.prototype.toString.call(x) → "[object Array]" 등 (타입 판별 관용)
            Native::ObjToString => {
                let tag = match &recv {
                    Some(Value::Arr(_)) => "Array",
                    None | Some(Value::Undefined) => "Undefined",
                    Some(Value::Null) => "Null",
                    Some(Value::Str(_)) => "String",
                    Some(Value::Num(_)) => "Number",
                    Some(Value::Bool(_)) => "Boolean",
                    Some(Value::Fn(_))
                    | Some(Value::Native(_))
                    | Some(Value::Bound(_))
                    | Some(Value::Class(_)) => "Function",
                    Some(Value::MapVal(_)) => "Map",
                    Some(Value::SetVal(_)) => "Set",
                    _ => "Object",
                };
                Ok(Value::Str(format!("[object {}]", tag)))
            }
            Native::ReturnFalse => Ok(Value::Bool(false)),
            Native::ReturnThis => Ok(recv.unwrap_or(Value::Undefined)),
            Native::FnToString => Ok(Value::Str("function () { [native code] }".to_string())),
            // obj[Symbol.iterator]() → 반복자 객체 { next(), value/done }
            Native::MakeIter => {
                let items: Vec<Value> = match &recv {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    Some(Value::Str(s)) => s.chars().map(|c| Value::Str(c.to_string())).collect(),
                    Some(Value::SetVal(s)) => s.borrow().clone(),
                    Some(Value::MapVal(m)) => m
                        .borrow()
                        .iter()
                        .map(|(k, v)| Value::Arr(ArrayObj::new(vec![k.clone(), v.clone()])))
                        .collect(),
                    _ => Vec::new(),
                };
                let mut it = ObjMap::new();
                it.insert("__items".to_string(), Value::Arr(ArrayObj::new(items)));
                it.insert("__i".to_string(), Value::Num(0.0));
                it.insert("next".to_string(), Value::Native(Native::IterNext));
                Ok(Value::Obj(Rc::new(RefCell::new(it))))
            }
            // 반복자.next() → { value, done }
            Native::IterNext => {
                let mut res = ObjMap::new();
                if let Some(Value::Obj(o)) = &recv {
                    let (items, i) = {
                        let b = o.borrow();
                        (b.get("__items").cloned(), b.get("__i").cloned())
                    };
                    if let (Some(Value::Arr(items)), Some(Value::Num(i))) = (items, i) {
                        let idx = i as usize;
                        let len = items.borrow().len();
                        if idx < len {
                            res.insert("value".to_string(), items.borrow()[idx].clone());
                            res.insert("done".to_string(), Value::Bool(false));
                            o.borrow_mut().insert("__i".to_string(), Value::Num((idx + 1) as f64));
                        } else {
                            res.insert("value".to_string(), Value::Undefined);
                            res.insert("done".to_string(), Value::Bool(true));
                        }
                    }
                }
                Ok(Value::Obj(Rc::new(RefCell::new(res))))
            }
            // 지연 제너레이터: next(v)/return(v)/throw(e) — 다음 yield 까지 재개 실행.
            Native::GenNext | Native::GenReturn | Native::GenThrow => {
                if let Some(Value::Gen(gs)) = &recv {
                    let arg = args.into_iter().next().unwrap_or(Value::Undefined);
                    let mode = match n {
                        Native::GenReturn => super::generator::ResumeMode::Return(arg.clone()),
                        Native::GenThrow => super::generator::ResumeMode::Throw(arg.clone()),
                        _ => super::generator::ResumeMode::Next,
                    };
                    let gs = gs.clone();
                    self.gen_resume(&gs, arg, mode)
                } else {
                    Ok(Value::Undefined)
                }
            }
            // Symbol(desc) — 고유 심볼 원시값 생성.
            Native::SymbolCtor => {
                self.sym_counter += 1;
                let desc = match args.first() {
                    Some(Value::Undefined) | None => None,
                    Some(v) => Some(to_display(v)),
                };
                Ok(Value::Symbol(Rc::new(super::SymbolData {
                    key: format!("@@sym:{}", self.sym_counter),
                    desc,
                })))
            }
            // Symbol.for(k) — 전역 레지스트리에서 공유 심볼.
            Native::SymbolFor => {
                let k = args.first().map(to_display).unwrap_or_else(|| "undefined".to_string());
                if let Some(sym) = self.sym_registry.get(&k) {
                    return Ok(sym.clone());
                }
                let sym = Value::Symbol(Rc::new(super::SymbolData {
                    key: format!("@@for:{}", k),
                    desc: Some(k.clone()),
                }));
                self.sym_registry.insert(k, sym.clone());
                Ok(sym)
            }
            // Symbol.keyFor(sym) — 레지스트리 심볼이면 키, 아니면 undefined.
            Native::SymbolKeyFor => Ok(match args.first() {
                Some(Value::Symbol(s)) => s
                    .key
                    .strip_prefix("@@for:")
                    .map(|k| Value::Str(k.to_string()))
                    .unwrap_or(Value::Undefined),
                _ => Value::Undefined,
            }),
            Native::MapCtor => self.make_map(args),
            Native::SetCtor => self.make_set(args),
            Native::ErrorCtor(name) => {
                let mut map = ObjMap::new();
                map.insert("name".to_string(), Value::Str(name.to_string()));
                map.insert(
                    "message".to_string(),
                    Value::Str(args.first().map(to_display).unwrap_or_default()),
                );
                map.insert("stack".to_string(), Value::Str(String::new()));
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
            }
            Native::Map(op) => {
                let Some(Value::MapVal(m)) = recv else {
                    return Err("Map 메서드".to_string());
                };
                self.map_method(m, op, args)
            }
            Native::Set(op) => {
                let Some(Value::SetVal(s)) = recv else {
                    return Err("Set 메서드".to_string());
                };
                Ok(self.set_method(s, op, args))
            }
            Native::CreateElement => {
                let tag = args.first().map(to_display).unwrap_or_default();
                if tag.is_empty() {
                    return Err("createElement 에 태그 이름이 필요".to_string());
                }
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element(&tag)))
            }
            Native::CreateTextNode => {
                let text = args.first().map(to_display).unwrap_or_default();
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_text(text)))
            }
            Native::CreateDocumentFragment => {
                // 프래그먼트: 센티널 태그 컨테이너. appendChild 시 자식만 옮겨진다.
                let dom = self.dom_arena()?;
                Ok(Value::Dom(dom.create_element("#document-fragment")))
            }
            // style.setProperty(name, value) / getPropertyValue(name) / removeProperty(name)
            Native::StyleSetProperty => {
                if let Some(Value::Style(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let value = args.get(1).map(to_display).unwrap_or_default();
                    self.style_set(id, &name, &value);
                }
                Ok(Value::Undefined)
            }
            Native::StyleGetProperty => {
                if let Some(Value::Style(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    return Ok(Value::Str(self.style_get(id, &name)));
                }
                Ok(Value::Str(String::new()))
            }
            Native::StyleRemoveProperty => {
                if let Some(Value::Style(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    self.style_set(id, &name, "");
                }
                Ok(Value::Undefined)
            }
            // classList.add(...names) / remove(...names) / toggle(name[,force]) / contains(name)
            Native::ClassAdd | Native::ClassRemove => {
                if let Some(Value::ClassList(id)) = recv {
                    let mut tokens = self.class_tokens(id);
                    for a in &args {
                        let name = to_display(a);
                        if name.is_empty() {
                            continue;
                        }
                        tokens.retain(|t| t != &name);
                        if matches!(n, Native::ClassAdd) {
                            tokens.push(name);
                        }
                    }
                    self.set_class_tokens(id, tokens);
                }
                Ok(Value::Undefined)
            }
            Native::ClassToggle => {
                if let Some(Value::ClassList(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let mut tokens = self.class_tokens(id);
                    let present = tokens.iter().any(|t| t == &name);
                    // 두 번째 인자(force)가 있으면 강제 설정
                    let want = match args.get(1) {
                        Some(v) => to_bool(v),
                        None => !present,
                    };
                    tokens.retain(|t| t != &name);
                    if want && !name.is_empty() {
                        tokens.push(name);
                    }
                    self.set_class_tokens(id, tokens);
                    return Ok(Value::Bool(want));
                }
                Ok(Value::Bool(false))
            }
            Native::ClassContains => {
                if let Some(Value::ClassList(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    return Ok(Value::Bool(self.class_tokens(id).iter().any(|t| t == &name)));
                }
                Ok(Value::Bool(false))
            }
            // event.preventDefault() / stopPropagation() — recv 가 이벤트 객체
            // getElementsByClassName / getElementsByTagName — 서브트리 수집
            Native::GetElementsByClass | Native::GetElementsByTag => {
                let scope = match &recv {
                    Some(Value::Dom(id)) => Some(*id),
                    _ => None, // document 등 → 루트
                };
                let query = args.first().map(to_display).unwrap_or_default();
                let by_class = matches!(n, Native::GetElementsByClass);
                let dom = self.dom_arena()?;
                let root = scope.unwrap_or(dom.root);
                let mut out = Vec::new();
                collect_elements(dom, root, scope.is_some(), &query, by_class, &mut out);
                Ok(Value::Arr(ArrayObj::new(out)))
            }
            Native::EventPreventDefault => {
                if let Some(Value::Obj(o)) = &recv {
                    o.borrow_mut().insert("defaultPrevented".to_string(), Value::Bool(true));
                }
                Ok(Value::Undefined)
            }
            Native::EventStopProp => {
                if let Some(Value::Obj(o)) = &recv {
                    o.borrow_mut().insert("__stopProp".to_string(), Value::Bool(true));
                }
                Ok(Value::Undefined)
            }
            Native::XhrCtor => Ok(self.make_xhr()),
            // XHR: open(method, url) → __method/__url 저장, readyState=1
            Native::XhrOpen => {
                if let Some(Value::Obj(o)) = &recv {
                    let method = args.first().map(to_display).unwrap_or_else(|| "GET".to_string());
                    let url = args.get(1).map(to_display).unwrap_or_default();
                    let mut b = o.borrow_mut();
                    b.insert("__method".to_string(), Value::Str(method));
                    b.insert("__url".to_string(), Value::Str(url));
                    b.insert("readyState".to_string(), Value::Num(1.0));
                }
                Ok(Value::Undefined)
            }
            Native::XhrSetHeader => Ok(Value::Undefined), // 헤더 저장 생략(요청은 GET 위주)
            Native::XhrGetHeader => Ok(Value::Null),
            // XHR: send() → 동기 HTTP, 필드 설정 후 readystatechange/load 발화
            Native::XhrSend => {
                let obj = match &recv {
                    Some(Value::Obj(o)) => o.clone(),
                    _ => return Ok(Value::Undefined),
                };
                let url = match obj.borrow().get("__url") {
                    Some(Value::Str(u)) => u.clone(),
                    _ => String::new(),
                };
                let full = self.resolve_url(&url);
                match crate::http::fetch(&full) {
                    Ok(r) => {
                        let body = String::from_utf8_lossy(&r.body).to_string();
                        let mut b = obj.borrow_mut();
                        b.insert("status".to_string(), Value::Num(r.status as f64));
                        b.insert(
                            "statusText".to_string(),
                            Value::Str(if r.status == 200 { "OK".into() } else { String::new() }),
                        );
                        b.insert("responseText".to_string(), Value::Str(body.clone()));
                        b.insert("response".to_string(), Value::Str(body));
                        b.insert("readyState".to_string(), Value::Num(4.0));
                    }
                    Err(e) => {
                        self.console.push(format!("XHR 실패: {:?}", e));
                        let mut b = obj.borrow_mut();
                        b.insert("status".to_string(), Value::Num(0.0));
                        b.insert("readyState".to_string(), Value::Num(4.0));
                    }
                }
                // 발화 순서: readystatechange → load
                self.xhr_fire(&obj, "readystatechange");
                self.xhr_fire(&obj, "load");
                self.xhr_fire(&obj, "loadend");
                Ok(Value::Undefined)
            }
            Native::DateNow => Ok(Value::Num(now_millis())),
            // Date.parse(str) → 밀리초(파싱 불가 시 NaN)
            Native::DateParse => {
                let millis = match args.first() {
                    Some(Value::Str(s)) => parse_date_string(s).unwrap_or(f64::NAN),
                    Some(v) => parse_date_string(&to_display(v)).unwrap_or(f64::NAN),
                    None => f64::NAN,
                };
                Ok(Value::Num(millis))
            }
            // Date.UTC(year, month[0기준], day, h, m, s, ms) → 밀리초(UTC)
            Native::DateUTC => {
                let g = |i: usize, dflt: f64| args.get(i).map(to_num).unwrap_or(dflt);
                if args.is_empty() {
                    return Ok(Value::Num(f64::NAN));
                }
                let millis = date_to_millis(
                    g(0, 1970.0) as i64,
                    g(1, 0.0) as i64 + 1,
                    g(2, 1.0) as i64,
                    g(3, 0.0) as i64,
                    g(4, 0.0) as i64,
                    g(5, 0.0) as i64,
                    g(6, 0.0) as i64,
                );
                Ok(Value::Num(millis))
            }
            Native::DateCtor => Ok(self.make_date_from_args(&args)),
            // date.getFullYear() 등 — recv 가 Date 객체
            Native::DateMethod(field) => {
                let millis = match &recv {
                    Some(Value::Obj(m)) => match m.borrow().get("__time") {
                        Some(Value::Num(n)) => *n,
                        _ => f64::NAN,
                    },
                    _ => f64::NAN,
                };
                let (y, mo, d, h, mi, s, ms, wd) = date_parts(millis);
                Ok(match field {
                    DateField::Time => Value::Num(millis),
                    DateField::FullYear => Value::Num(y as f64),
                    DateField::Month => Value::Num((mo - 1) as f64), // JS 는 0 기준
                    DateField::Date => Value::Num(d as f64),
                    DateField::Day => Value::Num(wd as f64),
                    DateField::Hours => Value::Num(h as f64),
                    DateField::Minutes => Value::Num(mi as f64),
                    DateField::Seconds => Value::Num(s as f64),
                    DateField::Ms => Value::Num(ms as f64),
                    DateField::TimezoneOffset => Value::Num(0.0),
                    DateField::ToIso => Value::Str(date_iso(millis)),
                    DateField::ToStr => Value::Str(date_string(millis)),
                    DateField::ToDateStr => Value::Str(date_string(millis)),
                })
            }
            // String(x)/Number(x)/Boolean(x) 변환 생성자
            Native::StringCtor => {
                // ToString: 객체는 ToPrimitive(hint string) → toString/valueOf 호출.
                let s = match args.into_iter().next() {
                    Some(v) => {
                        let p = self.to_primitive(v, true);
                        to_display(&p)
                    }
                    None => String::new(),
                };
                Ok(Value::Str(s))
            }
            Native::NumberCtor => Ok(Value::Num(match args.first() {
                Some(v) => to_num(v),
                None => 0.0,
            })),
            Native::BooleanCtor => Ok(Value::Bool(args.first().map(to_bool).unwrap_or(false))),
            Native::StrFromCharCode => {
                let s: String = args
                    .iter()
                    .filter_map(|a| char::from_u32(to_num(a) as u32))
                    .collect();
                Ok(Value::Str(s))
            }
            Native::NumIsInteger => {
                let ok = matches!(args.first(), Some(Value::Num(n)) if n.fract() == 0.0 && n.is_finite());
                Ok(Value::Bool(ok))
            }
            Native::NumIsFinite => {
                let ok = matches!(args.first(), Some(Value::Num(n)) if n.is_finite());
                Ok(Value::Bool(ok))
            }
            Native::NumIsNaN => {
                let ok = matches!(args.first(), Some(Value::Num(n)) if n.is_nan());
                Ok(Value::Bool(ok))
            }
            // recv.toString([radix]) / valueOf()
            Native::ValueToStr => {
                let v = recv.unwrap_or(Value::Undefined);
                // 숫자 + radix(2..36)면 진법 변환
                if let (Value::Num(n), Some(r)) = (&v, args.first().map(to_num)) {
                    let radix = r as u32;
                    if (2..=36).contains(&radix) && n.fract() == 0.0 && n.is_finite() {
                        let mut x = n.abs() as u64;
                        if x == 0 {
                            return Ok(Value::Str("0".to_string()));
                        }
                        let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
                        let mut buf = Vec::new();
                        while x > 0 {
                            buf.push(digits[(x % radix as u64) as usize]);
                            x /= radix as u64;
                        }
                        if *n < 0.0 {
                            buf.push(b'-');
                        }
                        buf.reverse();
                        return Ok(Value::Str(String::from_utf8_lossy(&buf).to_string()));
                    }
                }
                Ok(Value::Str(to_display(&v)))
            }
            Native::ValueOfSelf => Ok(recv.unwrap_or(Value::Undefined)),
            // n.toFixed(digits) — recv 가 숫자
            Native::NumToFixed => {
                let n = match recv {
                    Some(v) => to_num(&v),
                    None => 0.0,
                };
                let digits = args.first().map(to_num).unwrap_or(0.0).clamp(0.0, 100.0) as usize;
                Ok(Value::Str(format!("{:.*}", digits, n)))
            }
            // RegExp(pattern, flags) — 문자열/정규식 → 정규식 객체
            Native::RegExpCtor => {
                let (src, flags) = match args.first() {
                    Some(v) if regex_src_flags(v).is_some() => {
                        let (s, f) = regex_src_flags(v).unwrap();
                        let f = args.get(1).map(to_display).unwrap_or(f);
                        (s, f)
                    }
                    Some(v) => (to_display(v), args.get(1).map(to_display).unwrap_or_default()),
                    None => (String::new(), String::new()),
                };
                Ok(make_regex_obj(&src, &flags))
            }
            // regex.test(str) → bool
            Native::RegexTest => {
                let (src, flags) = recv.as_ref().and_then(regex_src_flags).ok_or("test 대상이 정규식 아님")?;
                let text = args.first().map(to_display).unwrap_or_default();
                let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                    .map_err(|e| format!("정규식 컴파일 실패: {}", e))?;
                let chars: Vec<char> = text.chars().collect();
                Ok(Value::Bool(re.find(&chars, 0).is_some()))
            }
            // regex.exec(str) → [full, g1, ...] with .index, or null. global 이면 lastIndex 갱신.
            Native::RegexExec => {
                let recv_obj = recv.clone();
                let (src, flags) = recv.as_ref().and_then(regex_src_flags).ok_or("exec 대상이 정규식 아님")?;
                let text = args.first().map(to_display).unwrap_or_default();
                let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                    .map_err(|e| format!("정규식 컴파일 실패: {}", e))?;
                let chars: Vec<char> = text.chars().collect();
                let from = if re.global {
                    match &recv_obj {
                        Some(Value::Obj(m)) => match m.borrow().get("lastIndex") {
                            Some(Value::Num(n)) => *n as usize,
                            _ => 0,
                        },
                        _ => 0,
                    }
                } else {
                    0
                };
                match re.find(&chars, from.min(chars.len())) {
                    Some(mt) => {
                        if re.global {
                            if let Some(Value::Obj(m)) = &recv_obj {
                                m.borrow_mut()
                                    .insert("lastIndex".to_string(), Value::Num(mt.end as f64));
                            }
                        }
                        Ok(self.regex_match_array(&chars, &mt, &re.group_names))
                    }
                    None => {
                        if re.global {
                            if let Some(Value::Obj(m)) = &recv_obj {
                                m.borrow_mut().insert("lastIndex".to_string(), Value::Num(0.0));
                            }
                        }
                        Ok(Value::Null)
                    }
                }
            }
            // document.body/head/documentElement (라이브 접근자)
            Native::DocQuery(tag) => {
                let dom = self.dom_arena()?;
                let root = dom.root;
                Ok(find_tag(dom, root, tag).map(Value::Dom).unwrap_or(Value::Null))
            }
            // parent.insertBefore(newNode, referenceNode)
            Native::InsertBefore => match (recv, args.first()) {
                (Some(Value::Dom(parent)), Some(Value::Dom(child))) => {
                    let child = *child;
                    let reference = match args.get(1) {
                        Some(Value::Dom(r)) => Some(*r),
                        _ => None,
                    };
                    let dom = self.dom_arena()?;
                    dom.insert_before(parent, child, reference);
                    Ok(Value::Dom(child))
                }
                _ => Err("insertBefore 는 요소 인자가 필요".to_string()),
            },
            Native::Matches => {
                let sel = args.first().map(to_display).unwrap_or_default();
                match recv {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        let ok = crate::css::parse_selector_list(&sel)
                            .map(|sels| crate::style::element_matches(dom, id, &sels))
                            .unwrap_or(false);
                        Ok(Value::Bool(ok))
                    }
                    _ => Ok(Value::Bool(false)),
                }
            }
            Native::Closest => {
                let sel = args.first().map(to_display).unwrap_or_default();
                let start = match recv {
                    Some(Value::Dom(id)) => id,
                    _ => return Ok(Value::Null),
                };
                let dom = self.dom_arena()?;
                let Some(sels) = crate::css::parse_selector_list(&sel) else {
                    return Ok(Value::Null);
                };
                // 자신부터 조상까지 올라가며 첫 매칭 반환
                let mut chain = vec![start];
                chain.extend(dom.ancestors(start));
                for id in chain {
                    if matches!(dom.get(id).node_type, crate::dom::NodeType::Element(_))
                        && crate::style::element_matches(dom, id, &sels)
                    {
                        return Ok(Value::Dom(id));
                    }
                }
                Ok(Value::Null)
            }
            Native::DomContains => {
                let other = match args.first() {
                    Some(Value::Dom(o)) => *o,
                    _ => return Ok(Value::Bool(false)),
                };
                match recv {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        // other == id 이거나 id 의 자손이면 true
                        let contained = other == id || dom.ancestors(other).contains(&id);
                        Ok(Value::Bool(contained))
                    }
                    _ => Ok(Value::Bool(false)),
                }
            }
            Native::CanvasGetContext => {
                // canvas.getContext('2d') → 상태 + 메서드를 담은 컨텍스트 객체
                let canvas_id = match recv {
                    Some(Value::Dom(id)) => id,
                    _ => return Ok(Value::Null),
                };
                self.canvas_cmds.entry(canvas_id).or_default();
                let mut m = ObjMap::new();
                m.insert("__canvas".to_string(), Value::Num(canvas_id as f64));
                m.insert("fillStyle".to_string(), Value::Str("#000000".to_string()));
                m.insert("strokeStyle".to_string(), Value::Str("#000000".to_string()));
                m.insert("lineWidth".to_string(), Value::Num(1.0));
                m.insert("font".to_string(), Value::Str("10px sans-serif".to_string()));
                m.insert("__path".to_string(), Value::Arr(ArrayObj::new(Vec::new())));
                use CanvasMethod::*;
                for (name, meth) in [
                    ("fillRect", FillRect),
                    ("clearRect", ClearRect),
                    ("strokeRect", StrokeRect),
                    ("beginPath", BeginPath),
                    ("moveTo", MoveTo),
                    ("lineTo", LineTo),
                    ("arc", Arc),
                    ("rect", Rect),
                    ("closePath", ClosePath),
                    ("fill", Fill),
                    ("stroke", Stroke),
                    ("fillText", FillText),
                    ("strokeText", FillText),
                    ("save", Noop),
                    ("restore", Noop),
                    ("scale", Noop),
                    ("translate", Noop),
                    ("rotate", Noop),
                    ("transform", Noop),
                    ("setTransform", Noop),
                    ("setLineDash", Noop),
                    ("clip", Noop),
                    ("measureText", Noop),
                    ("createLinearGradient", Noop),
                    ("bezierCurveTo", Noop),
                    ("quadraticCurveTo", Noop),
                    ("drawImage", Noop),
                    ("putImageData", Noop),
                ] {
                    m.insert(name.to_string(), Value::Native(Native::Canvas(meth)));
                }
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            Native::Canvas(method) => self.canvas_method(method, recv, args),
            Native::CloneNode => {
                let deep = args.first().map(to_bool).unwrap_or(false);
                match recv {
                    Some(Value::Dom(id)) => {
                        let dom = self.dom_arena()?;
                        Ok(Value::Dom(dom.clone_node(id, deep)))
                    }
                    _ => Err("cloneNode 는 요소 메서드".to_string()),
                }
            }
            Native::DispatchEvent => {
                let node = match recv {
                    Some(Value::Dom(id)) => id,
                    _ => return Ok(Value::Bool(false)),
                };
                let evt = args.first().cloned().unwrap_or(Value::Undefined);
                let etype = match &evt {
                    Value::Obj(o) => o.borrow().get("type").map(to_display).unwrap_or_default(),
                    other => to_display(other),
                };
                // Obj 이벤트면 그대로 사용, 아니면 표준 이벤트 객체 생성
                let evt = if matches!(evt, Value::Obj(_)) {
                    evt
                } else {
                    self.make_event(&etype, node)
                };
                self.dispatch_event_value(node, &etype, evt.clone());
                // !defaultPrevented 근사
                let prevented = matches!(&evt,
                    Value::Obj(o) if matches!(o.borrow().get("defaultPrevented"), Some(Value::Bool(true))));
                Ok(Value::Bool(!prevented))
            }
            Native::EventCtor => {
                // new Event(type, opts) / new CustomEvent(type, {detail}) → 이벤트 객체
                let etype = args.first().map(to_display).unwrap_or_default();
                let mut m = ObjMap::new();
                m.insert("type".to_string(), Value::Str(etype));
                let mut bubbles = false;
                if let Some(Value::Obj(o)) = args.get(1) {
                    let o = o.borrow();
                    if let Some(d) = o.get("detail") {
                        m.insert("detail".to_string(), d.clone());
                    }
                    bubbles = matches!(o.get("bubbles"), Some(Value::Bool(true)));
                }
                m.insert("bubbles".to_string(), Value::Bool(bubbles));
                m.insert("defaultPrevented".to_string(), Value::Bool(false));
                m.insert("preventDefault".to_string(), Value::Native(Native::EventPreventDefault));
                m.insert("stopPropagation".to_string(), Value::Native(Native::EventStopProp));
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            Native::GetBoundingClientRect => {
                let (x, y, w, h) = match recv {
                    Some(Value::Dom(id)) => {
                        self.layout_rects.get(&id).copied().unwrap_or((0.0, 0.0, 0.0, 0.0))
                    }
                    _ => (0.0, 0.0, 0.0, 0.0),
                };
                let mut m = ObjMap::new();
                m.insert("x".to_string(), Value::Num(x as f64));
                m.insert("y".to_string(), Value::Num(y as f64));
                m.insert("left".to_string(), Value::Num(x as f64));
                m.insert("top".to_string(), Value::Num(y as f64));
                m.insert("right".to_string(), Value::Num((x + w) as f64));
                m.insert("bottom".to_string(), Value::Num((y + h) as f64));
                m.insert("width".to_string(), Value::Num(w as f64));
                m.insert("height".to_string(), Value::Num(h as f64));
                Ok(Value::Obj(Rc::new(RefCell::new(m))))
            }
            Native::AppendChild => match (recv, args.first()) {
                (Some(Value::Dom(parent)), Some(Value::Dom(child))) => {
                    let child = *child;
                    let dom = self.dom_arena()?;
                    // DocumentFragment 는 자신이 아니라 자식들을 옮긴다
                    if is_fragment(dom, child) {
                        for c in dom.get(child).children.clone() {
                            dom.append_child(parent, c);
                        }
                    } else {
                        dom.append_child(parent, child);
                    }
                    Ok(Value::Dom(child))
                }
                _ => Err("appendChild 는 요소 인자가 필요".to_string()),
            },
            // ParentNode/ChildNode 삽입 (append/prepend/before/after/replaceWith).
            // 가변 인자, 문자열은 텍스트 노드로. append/prepend 는 recv 가 부모,
            // before/after/replaceWith 는 recv 의 부모에 삽입.
            Native::NodeAppend => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    for id in ids {
                        dom.insert_before(target, id, None);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodePrepend => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    let first = dom.get(target).children.first().copied();
                    for id in ids {
                        dom.insert_before(target, id, first);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodeBefore => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    if let Some(parent) = dom.get(target).parent {
                        for id in ids {
                            dom.insert_before(parent, id, Some(target));
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodeAfter => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    if let Some(parent) = dom.get(target).parent {
                        let kids = dom.get(parent).children.clone();
                        let next = kids
                            .iter()
                            .position(|&c| c == target)
                            .and_then(|i| kids.get(i + 1).copied());
                        for id in ids {
                            dom.insert_before(parent, id, next);
                        }
                    }
                }
                Ok(Value::Undefined)
            }
            Native::NodeReplaceWith => {
                if let Some(Value::Dom(target)) = recv {
                    let ids = self.nodes_from_args(&args)?;
                    let dom = self.dom_arena()?;
                    if let Some(parent) = dom.get(target).parent {
                        for id in ids {
                            dom.insert_before(parent, id, Some(target));
                        }
                        dom.detach(target);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::RemoveElement => match recv {
                Some(Value::Dom(id)) => {
                    let dom = self.dom_arena()?;
                    dom.detach(id);
                    Ok(Value::Undefined)
                }
                _ => Err("remove 는 요소 메서드".to_string()),
            },
            Native::RemoveAttribute => {
                if let Some(Value::Dom(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    if let crate::dom::NodeType::Element(e) = &mut dom.get_mut(id).node_type {
                        e.attributes.remove(&name);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::HasAttribute => {
                let has = if let Some(Value::Dom(id)) = recv {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    matches!(&dom.get(id).node_type,
                        crate::dom::NodeType::Element(e) if e.attributes.contains_key(&name))
                } else {
                    false
                };
                Ok(Value::Bool(has))
            }
            Native::RemoveChild => {
                // parent.removeChild(child) → child 를 트리에서 분리, child 반환
                let child = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Value::Dom(cid) = child {
                    let dom = self.dom_arena()?;
                    dom.detach(cid);
                }
                Ok(child)
            }
            Native::SetAttribute => match recv {
                Some(Value::Dom(id)) => {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let value = args.get(1).map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    if let crate::dom::NodeType::Element(e) = &mut dom.get_mut(id).node_type {
                        e.attributes.insert(name, value);
                    }
                    Ok(Value::Undefined)
                }
                _ => Err("setAttribute 는 요소 메서드".to_string()),
            },
            Native::QuerySelector | Native::QuerySelectorAll => {
                let all = n == Native::QuerySelectorAll;
                let sel = args.first().map(to_display).unwrap_or_default();
                // 요소 수신자면 그 서브트리(자신 제외), document 면 문서 전체
                let scope = match recv {
                    Some(Value::Dom(id)) => Some(id),
                    _ => None,
                };
                self.dom_query(scope, &sel, all)
            }
            Native::Math(op) => {
                let a = args.first().map(to_num).unwrap_or(f64::NAN);
                Ok(Value::Num(match op {
                    MathOp::Floor => a.floor(),
                    MathOp::Ceil => a.ceil(),
                    // JS Math.round: floor(x+0.5) — 반올림이 +∞ 방향 (round(-2.5)=-2, not -3)
                    MathOp::Round => {
                        if a.is_nan() || a.is_infinite() {
                            a
                        } else {
                            (a + 0.5).floor()
                        }
                    }
                    MathOp::Abs => a.abs(),
                    MathOp::Sqrt => a.sqrt(),
                    MathOp::Pow => a.powf(args.get(1).map(to_num).unwrap_or(f64::NAN)),
                    // JS min/max: 인자에 NaN 있으면 NaN (Rust min/max 는 NaN 무시하므로 직접)
                    MathOp::Min => {
                        let vs = args.iter().map(to_num);
                        vs.fold(f64::INFINITY, |acc, x| {
                            if acc.is_nan() || x.is_nan() { f64::NAN } else { acc.min(x) }
                        })
                    }
                    MathOp::Max => {
                        let vs = args.iter().map(to_num);
                        vs.fold(f64::NEG_INFINITY, |acc, x| {
                            if acc.is_nan() || x.is_nan() { f64::NAN } else { acc.max(x) }
                        })
                    }
                    MathOp::Trunc => a.trunc(),
                    MathOp::Sign => {
                        if a.is_nan() {
                            f64::NAN
                        } else if a > 0.0 {
                            1.0
                        } else if a < 0.0 {
                            -1.0
                        } else {
                            a // ±0 유지
                        }
                    }
                    MathOp::Cbrt => a.cbrt(),
                    MathOp::Log => a.ln(),
                    MathOp::Log2 => a.log2(),
                    MathOp::Log10 => a.log10(),
                    MathOp::Exp => a.exp(),
                    MathOp::Sin => a.sin(),
                    MathOp::Cos => a.cos(),
                    MathOp::Tan => a.tan(),
                    MathOp::Asin => a.asin(),
                    MathOp::Acos => a.acos(),
                    MathOp::Atan => a.atan(),
                    MathOp::Atan2 => a.atan2(args.get(1).map(to_num).unwrap_or(f64::NAN)),
                    MathOp::Hypot => args.iter().map(to_num).fold(0.0f64, |acc, x| acc.hypot(x)),
                    MathOp::Random => {
                        // xorshift64*
                        self.rng ^= self.rng << 13;
                        self.rng ^= self.rng >> 7;
                        self.rng ^= self.rng << 17;
                        (self.rng >> 11) as f64 / (1u64 << 53) as f64
                    }
                }))
            }
            Native::Str(op) => {
                let Some(Value::Str(s)) = recv else {
                    return Err("문자열 메서드".to_string());
                };
                let chars: Vec<char> = s.chars().collect();
                // JS 문자열은 UTF-16 코드 유닛 열 — 길이/인덱스는 코드 유닛 기준.
                let units: Vec<u16> = s.encode_utf16().collect();
                let arg_str = |i: usize| args.get(i).map(to_display).unwrap_or_default();
                Ok(match op {
                    StrOp::Upper => Value::Str(s.to_uppercase()),
                    StrOp::Lower => Value::Str(s.to_lowercase()),
                    StrOp::Trim => Value::Str(s.trim().to_string()),
                    StrOp::CharAt => {
                        // UTF-16 코드 유닛 하나(범위 밖은 ""). 짝 없는 서로게이트는 U+FFFD.
                        let i = args.first().map(to_num).unwrap_or(0.0);
                        let out = if i >= 0.0 && (i as usize) < units.len() {
                            String::from_utf16_lossy(&units[i as usize..i as usize + 1])
                        } else {
                            String::new()
                        };
                        Value::Str(out)
                    }
                    StrOp::IndexOf => {
                        // UTF-16 코드 유닛 인덱스. fromIndex 2번째 인자.
                        let ndl: Vec<u16> = arg_str(0).encode_utf16().collect();
                        let from = args
                            .get(1)
                            .map(to_num)
                            .filter(|n| !n.is_nan())
                            .map(|n| n.max(0.0) as usize)
                            .unwrap_or(0);
                        Value::Num(utf16_index_of(&units, &ndl, from).map(|i| i as f64).unwrap_or(-1.0))
                    }
                    StrOp::LastIndexOf => {
                        let ndl: Vec<u16> = arg_str(0).encode_utf16().collect();
                        Value::Num(utf16_last_index_of(&units, &ndl).map(|i| i as f64).unwrap_or(-1.0))
                    }
                    StrOp::Includes => Value::Bool(s.contains(&arg_str(0))),
                    StrOp::StartsWith => Value::Bool(s.starts_with(&arg_str(0))),
                    StrOp::EndsWith => Value::Bool(s.ends_with(&arg_str(0))),
                    StrOp::Replace => {
                        let pat = args.first().cloned().unwrap_or(Value::Undefined);
                        let repl = args.get(1).cloned().unwrap_or(Value::Undefined);
                        Value::Str(self.str_replace(&s, &pat, &repl, false)?)
                    }
                    StrOp::ReplaceAll => {
                        let pat = args.first().cloned().unwrap_or(Value::Undefined);
                        let repl = args.get(1).cloned().unwrap_or(Value::Undefined);
                        Value::Str(self.str_replace(&s, &pat, &repl, true)?)
                    }
                    StrOp::Search => {
                        let (src, flags) = args
                            .first()
                            .and_then(regex_src_flags)
                            .unwrap_or_else(|| (regex_escape(&arg_str(0)), String::new()));
                        let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                            .map_err(|e| format!("정규식: {}", e))?;
                        match re.find(&chars, 0) {
                            // 정규식 엔진은 코드포인트 인덱스 → UTF-16 유닛 오프셋으로 변환
                            Some(mt) => {
                                let u16_start: usize =
                                    chars[..mt.start].iter().map(|c| c.len_utf16()).sum();
                                Value::Num(u16_start as f64)
                            }
                            None => Value::Num(-1.0),
                        }
                    }
                    StrOp::Match => {
                        let (src, flags) = args
                            .first()
                            .and_then(regex_src_flags)
                            .unwrap_or_else(|| (regex_escape(&arg_str(0)), String::new()));
                        let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                            .map_err(|e| format!("정규식: {}", e))?;
                        if re.global {
                            // 전역: 매치 문자열들의 배열
                            let mut out = Vec::new();
                            let mut pos = 0;
                            while let Some(mt) = re.find(&chars, pos) {
                                out.push(Value::Str(chars[mt.start..mt.end].iter().collect()));
                                pos = if mt.end > mt.start { mt.end } else { mt.end + 1 };
                                if pos > chars.len() {
                                    break;
                                }
                            }
                            if out.is_empty() {
                                Value::Null
                            } else {
                                Value::Arr(ArrayObj::new(out))
                            }
                        } else {
                            match re.find(&chars, 0) {
                                Some(mt) => self.regex_match_array(&chars, &mt, &re.group_names),
                                None => Value::Null,
                            }
                        }
                    }
                    StrOp::MatchAll => {
                        let (src, flags) = args
                            .first()
                            .and_then(regex_src_flags)
                            .unwrap_or_else(|| (regex_escape(&arg_str(0)), String::new()));
                        let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                            .map_err(|e| format!("정규식: {}", e))?;
                        let mut out = Vec::new();
                        let mut pos = 0;
                        while let Some(mt) = re.find(&chars, pos) {
                            out.push(self.regex_match_array(&chars, &mt, &re.group_names));
                            pos = if mt.end > mt.start { mt.end } else { mt.end + 1 };
                            if pos > chars.len() {
                                break;
                            }
                        }
                        Value::Arr(ArrayObj::new(out))
                    }
                    StrOp::Slice => {
                        // UTF-16 코드 유닛 기준 슬라이스(음수 인덱스는 끝에서).
                        let len = units.len() as isize;
                        let clampi = |v: f64| -> usize {
                            let i = v as isize;
                            (if i < 0 { len + i } else { i }).clamp(0, len) as usize
                        };
                        let start = clampi(args.first().map(to_num).unwrap_or(0.0));
                        let end = clampi(args.get(1).map(to_num).unwrap_or(len as f64));
                        Value::Str(String::from_utf16_lossy(&units[start..end.max(start)]))
                    }
                    StrOp::Split => {
                        // 정규식 구분자 지원
                        if let Some((src, flags)) = args.first().and_then(regex_src_flags) {
                            let re = crate::js::regex::Regex::compile_pattern(&src, &flags)
                                .map_err(|e| format!("정규식: {}", e))?;
                            let mut parts = Vec::new();
                            let mut last = 0;
                            let mut pos = 0;
                            while pos <= chars.len() {
                                match re.find(&chars, pos) {
                                    Some(mt) if mt.end > mt.start => {
                                        parts.push(Value::Str(chars[last..mt.start].iter().collect()));
                                        last = mt.end;
                                        pos = mt.end;
                                    }
                                    _ => break,
                                }
                            }
                            parts.push(Value::Str(chars[last..].iter().collect()));
                            Value::Arr(ArrayObj::new(parts))
                        } else {
                            let sep = arg_str(0);
                            let mut parts: Vec<Value> = if args.is_empty() {
                                vec![Value::Str(s.clone())]
                            } else if sep.is_empty() {
                                chars.iter().map(|c| Value::Str(c.to_string())).collect()
                            } else {
                                s.split(&sep).map(|p| Value::Str(p.to_string())).collect()
                            };
                            // limit (2번째 인자): 결과를 그 개수로 자름
                            if let Some(lim) = args.get(1).map(to_num).filter(|n| !n.is_nan() && *n >= 0.0) {
                                parts.truncate(lim as usize);
                            }
                            Value::Arr(ArrayObj::new(parts))
                        }
                    }
                    StrOp::TrimStart => Value::Str(s.trim_start().to_string()),
                    StrOp::TrimEnd => Value::Str(s.trim_end().to_string()),
                    StrOp::Repeat => {
                        let n = args.first().map(to_num).unwrap_or(0.0).max(0.0) as usize;
                        Value::Str(s.repeat(n))
                    }
                    StrOp::PadStart | StrOp::PadEnd => {
                        let target = args.first().map(to_num).unwrap_or(0.0) as usize;
                        let pad = if args.len() > 1 { arg_str(1) } else { " ".to_string() };
                        let cur = chars.len();
                        if cur >= target || pad.is_empty() {
                            Value::Str(s.clone())
                        } else {
                            let need = target - cur;
                            let padchars: Vec<char> = pad.chars().collect();
                            let fill: String =
                                (0..need).map(|i| padchars[i % padchars.len()]).collect();
                            Value::Str(if matches!(op, StrOp::PadStart) {
                                format!("{}{}", fill, s)
                            } else {
                                format!("{}{}", s, fill)
                            })
                        }
                    }
                    StrOp::CharCodeAt => {
                        // i번째 UTF-16 코드 유닛(u16). 범위 밖 NaN.
                        let i = args.first().map(to_num).unwrap_or(0.0);
                        if i < 0.0 {
                            Value::Num(f64::NAN)
                        } else {
                            match units.get(i as usize) {
                                Some(u) => Value::Num(*u as f64),
                                None => Value::Num(f64::NAN),
                            }
                        }
                    }
                    StrOp::CodePointAt => {
                        // i번째 UTF-16 위치에서 시작하는 코드 포인트(서로게이트쌍 결합). 범위 밖 undefined.
                        let i = args.first().map(to_num).unwrap_or(0.0);
                        if i < 0.0 {
                            Value::Undefined
                        } else {
                            let idx = i as usize;
                            match units.get(idx).copied() {
                                None => Value::Undefined,
                                Some(hi) if (0xD800..=0xDBFF).contains(&hi) => {
                                    let cp = units
                                        .get(idx + 1)
                                        .copied()
                                        .filter(|lo| (0xDC00..=0xDFFF).contains(lo))
                                        .map(|lo| {
                                            0x10000 + ((hi as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00)
                                        })
                                        .unwrap_or(hi as u32);
                                    Value::Num(cp as f64)
                                }
                                Some(u) => Value::Num(u as f64),
                            }
                        }
                    }
                    StrOp::Concat => {
                        let mut out = s.clone();
                        for a in &args {
                            out.push_str(&to_display(a));
                        }
                        Value::Str(out)
                    }
                    StrOp::LocaleCompare => {
                        // 로케일 콜레이션 근사(코드포인트 순서). -1/0/1.
                        let other = arg_str(0);
                        Value::Num(match s.as_str().cmp(other.as_str()) {
                            std::cmp::Ordering::Less => -1.0,
                            std::cmp::Ordering::Equal => 0.0,
                            std::cmp::Ordering::Greater => 1.0,
                        })
                    }
                    StrOp::At => {
                        // str.at(i): UTF-16 유닛, 음수는 끝에서. 범위 밖 undefined.
                        let len = units.len() as isize;
                        let i = args.first().map(to_num).unwrap_or(0.0) as isize;
                        let idx = if i < 0 { len + i } else { i };
                        if idx >= 0 && idx < len {
                            Value::Str(String::from_utf16_lossy(&units[idx as usize..idx as usize + 1]))
                        } else {
                            Value::Undefined
                        }
                    }
                })
            }
            Native::Arr(op) => {
                let Some(Value::Arr(a)) = recv else {
                    return Err("배열 메서드".to_string());
                };
                Ok(match op {
                    ArrOp::Join => {
                        let sep = args.first().map(to_display).unwrap_or(",".to_string());
                        Value::Str(
                            a.borrow().iter().map(to_display).collect::<Vec<_>>().join(&sep),
                        )
                    }
                    ArrOp::Pop => a.borrow_mut().pop().unwrap_or(Value::Undefined),
                    ArrOp::IndexOf => {
                        let needle = args.first().cloned().unwrap_or(Value::Undefined);
                        match a.borrow().iter().position(|v| strict_eq(v, &needle)) {
                            Some(i) => Value::Num(i as f64),
                            None => Value::Num(-1.0),
                        }
                    }
                    ArrOp::Slice => {
                        let items = a.borrow();
                        let len = items.len() as isize;
                        let clampi = |v: f64| -> usize {
                            let i = v as isize;
                            (if i < 0 { len + i } else { i }).clamp(0, len) as usize
                        };
                        let start = clampi(args.first().map(to_num).unwrap_or(0.0));
                        let end = clampi(args.get(1).map(to_num).unwrap_or(len as f64));
                        Value::Arr(ArrayObj::new(items[start..end.max(start)].to_vec()))
                    }
                    ArrOp::ForEach | ArrOp::Map | ArrOp::Filter | ArrOp::FlatMap => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut out = Vec::new();
                        for (i, item) in snapshot.into_iter().enumerate() {
                            let r = self.call_value(
                                f.clone(),
                                None,
                                vec![item.clone(), Value::Num(i as f64)],
                            )?;
                            match op {
                                ArrOp::Map => out.push(r),
                                // flatMap: 콜백 결과가 배열이면 한 단계 펼침
                                ArrOp::FlatMap => match r {
                                    Value::Arr(inner) => out.extend(inner.borrow().iter().cloned()),
                                    other => out.push(other),
                                },
                                ArrOp::Filter => {
                                    if to_bool(&r) {
                                        out.push(item);
                                    }
                                }
                                _ => {}
                            }
                        }
                        match op {
                            ArrOp::ForEach => Value::Undefined,
                            _ => Value::Arr(ArrayObj::new(out)),
                        }
                    }
                    ArrOp::Some | ArrOp::Every | ArrOp::Find | ArrOp::FindIndex => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut result = Value::Undefined;
                        let mut found = false;
                        for (i, item) in snapshot.into_iter().enumerate() {
                            let r = self.call_value(
                                f.clone(),
                                None,
                                vec![item.clone(), Value::Num(i as f64)],
                            )?;
                            let truthy = to_bool(&r);
                            match op {
                                ArrOp::Some if truthy => {
                                    result = Value::Bool(true);
                                    found = true;
                                    break;
                                }
                                ArrOp::Every if !truthy => {
                                    result = Value::Bool(false);
                                    found = true;
                                    break;
                                }
                                ArrOp::Find if truthy => {
                                    result = item;
                                    found = true;
                                    break;
                                }
                                ArrOp::FindIndex if truthy => {
                                    result = Value::Num(i as f64);
                                    found = true;
                                    break;
                                }
                                _ => {}
                            }
                        }
                        if found {
                            result
                        } else {
                            match op {
                                ArrOp::Some => Value::Bool(false),
                                ArrOp::Every => Value::Bool(true),
                                ArrOp::FindIndex => Value::Num(-1.0),
                                _ => Value::Undefined,
                            }
                        }
                    }
                    ArrOp::Reduce => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut iter = snapshot.into_iter().enumerate();
                        let mut acc = match args.get(1) {
                            Some(init) => init.clone(),
                            None => match iter.next() {
                                Some((_, v)) => v,
                                None => return Err("빈 배열 reduce (초기값 없음)".to_string()),
                            },
                        };
                        for (i, item) in iter {
                            acc = self.call_value(
                                f.clone(),
                                None,
                                vec![acc, item, Value::Num(i as f64)],
                            )?;
                        }
                        acc
                    }
                    ArrOp::ReduceRight => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut idx = snapshot.len();
                        let mut acc = match args.get(1) {
                            Some(init) => init.clone(),
                            None => {
                                if idx == 0 {
                                    return Err("빈 배열 reduceRight (초기값 없음)".to_string());
                                }
                                idx -= 1;
                                snapshot[idx].clone()
                            }
                        };
                        while idx > 0 {
                            idx -= 1;
                            acc = self.call_value(
                                f.clone(),
                                None,
                                vec![acc, snapshot[idx].clone(), Value::Num(idx as f64)],
                            )?;
                        }
                        acc
                    }
                    ArrOp::FindLast | ArrOp::FindLastIndex => {
                        let f = args.first().cloned().ok_or("콜백이 필요")?;
                        let snapshot: Vec<Value> = a.borrow().clone();
                        let mut result = if matches!(op, ArrOp::FindLastIndex) {
                            Value::Num(-1.0)
                        } else {
                            Value::Undefined
                        };
                        for i in (0..snapshot.len()).rev() {
                            let r = self.call_value(
                                f.clone(),
                                None,
                                vec![snapshot[i].clone(), Value::Num(i as f64)],
                            )?;
                            if to_bool(&r) {
                                result = if matches!(op, ArrOp::FindLastIndex) {
                                    Value::Num(i as f64)
                                } else {
                                    snapshot[i].clone()
                                };
                                break;
                            }
                        }
                        result
                    }
                    ArrOp::Concat => {
                        let mut out: Vec<Value> = a.borrow().clone();
                        for arg in &args {
                            match arg {
                                Value::Arr(b) => out.extend(b.borrow().iter().cloned()),
                                other => out.push(other.clone()),
                            }
                        }
                        Value::Arr(ArrayObj::new(out))
                    }
                    ArrOp::Includes => {
                        let needle = args.first().cloned().unwrap_or(Value::Undefined);
                        Value::Bool(a.borrow().iter().any(|v| strict_eq(v, &needle)))
                    }
                    ArrOp::Splice => {
                        // splice(start, deleteCount, ...items) → 제거분 배열 반환, 원본 변형
                        let mut arr = a.borrow_mut();
                        let len = arr.len() as isize;
                        let start = {
                            let s = args.first().map(to_num).unwrap_or(0.0) as isize;
                            (if s < 0 { len + s } else { s }).clamp(0, len) as usize
                        };
                        let del = match args.get(1) {
                            Some(v) => (to_num(v) as isize).clamp(0, len - start as isize) as usize,
                            None => arr.len() - start,
                        };
                        let removed: Vec<Value> = arr.splice(
                            start..start + del,
                            args.iter().skip(2).cloned(),
                        ).collect();
                        Value::Arr(ArrayObj::new(removed))
                    }
                    ArrOp::Shift => {
                        let mut arr = a.borrow_mut();
                        if arr.is_empty() {
                            Value::Undefined
                        } else {
                            arr.remove(0)
                        }
                    }
                    ArrOp::Unshift => {
                        let mut arr = a.borrow_mut();
                        for (i, v) in args.iter().cloned().enumerate() {
                            arr.insert(i, v);
                        }
                        Value::Num(arr.len() as f64)
                    }
                    ArrOp::Reverse => {
                        a.borrow_mut().reverse();
                        Value::Arr(a.clone())
                    }
                    ArrOp::Keys => {
                        let n = a.borrow().len();
                        Value::Arr(ArrayObj::new(
                            (0..n).map(|i| Value::Num(i as f64)).collect(),
                        ))
                    }
                    ArrOp::Values => Value::Arr(ArrayObj::new(a.borrow().clone())),
                    ArrOp::Sort => {
                        // 제자리 정렬 후 같은 배열 반환. 비교자 있으면 부호, 없으면 문자열 비교.
                        // 비교자가 Result 를 반환하므로 삽입정렬로 에러를 전파한다.
                        let cmp = args.first().cloned().filter(|v| !matches!(v, Value::Undefined));
                        let mut items: Vec<Value> = a.borrow().clone();
                        let n = items.len();
                        for i in 1..n {
                            let mut j = i;
                            while j > 0 {
                                let ord = match &cmp {
                                    Some(f) => {
                                        let r = self.call_value(
                                            f.clone(),
                                            None,
                                            vec![items[j - 1].clone(), items[j].clone()],
                                        )?;
                                        to_num(&r)
                                    }
                                    None => {
                                        let x = to_display(&items[j - 1]);
                                        let y = to_display(&items[j]);
                                        if x < y {
                                            -1.0
                                        } else if x > y {
                                            1.0
                                        } else {
                                            0.0
                                        }
                                    }
                                };
                                if ord > 0.0 {
                                    items.swap(j - 1, j);
                                    j -= 1;
                                } else {
                                    break;
                                }
                            }
                        }
                        *a.borrow_mut() = items;
                        Value::Arr(a.clone())
                    }
                    ArrOp::Flat => {
                        // 기본 깊이 1: 한 단계 배열만 펼친다.
                        let mut out = Vec::new();
                        for v in a.borrow().iter() {
                            match v {
                                Value::Arr(inner) => out.extend(inner.borrow().iter().cloned()),
                                other => out.push(other.clone()),
                            }
                        }
                        Value::Arr(ArrayObj::new(out))
                    }
                    ArrOp::At => {
                        // arr.at(i): 음수는 끝에서. 범위 밖 undefined.
                        let len = a.borrow().len() as isize;
                        let i = args.first().map(to_num).unwrap_or(0.0) as isize;
                        let idx = if i < 0 { len + i } else { i };
                        if idx >= 0 && idx < len {
                            a.borrow().get(idx as usize).cloned().unwrap_or(Value::Undefined)
                        } else {
                            Value::Undefined
                        }
                    }
                    // FlatMap 은 콜백이 필요 — 위 콜백 처리부에서 처리됨(여기 도달 안 함).
                    ArrOp::FlatMap => Value::Undefined,
                    ArrOp::Fill => {
                        // arr.fill(value, start?, end?): 제자리 채우고 배열 반환.
                        let val = args.first().cloned().unwrap_or(Value::Undefined);
                        let len = a.borrow().len() as isize;
                        let clampi = |v: f64| -> usize {
                            let i = v as isize;
                            (if i < 0 { len + i } else { i }).clamp(0, len) as usize
                        };
                        let start = clampi(args.get(1).map(to_num).unwrap_or(0.0));
                        let end = clampi(args.get(2).map(to_num).unwrap_or(len as f64));
                        {
                            let mut b = a.borrow_mut();
                            for i in start..end {
                                if i < b.len() {
                                    b[i] = val.clone();
                                }
                            }
                        }
                        Value::Arr(a.clone())
                    }
                })
            }
            Native::JsonParse => {
                let src = args.first().map(to_display).unwrap_or_default();
                json_parse(&src)
            }
            Native::JsonStringify => {
                Ok(json_stringify(args.first().unwrap_or(&Value::Undefined))
                    .map(Value::Str)
                    .unwrap_or(Value::Undefined))
            }
            Native::ParseInt => {
                let s = args.first().map(to_display).unwrap_or_default();
                let t = s.trim();
                let (neg, mut body) = match t.strip_prefix('-') {
                    Some(rest) => (true, rest),
                    None => (false, t.strip_prefix('+').unwrap_or(t)),
                };
                // radix: 인자 있으면 사용, 0/미지정이면 자동(0x→16, 아니면 10)
                let mut radix = args.get(1).map(|v| to_num(v) as i64).unwrap_or(0);
                if (radix == 16 || radix == 0)
                    && (body.starts_with("0x") || body.starts_with("0X"))
                {
                    body = &body[2..];
                    radix = 16;
                }
                if radix == 0 {
                    radix = 10;
                }
                if !(2..=36).contains(&radix) {
                    return Ok(Value::Num(f64::NAN));
                }
                // 자리별 f64 누적 (오버플로 없이 임의 길이, JS 동일 근사)
                let mut val = 0.0f64;
                let mut any = false;
                for c in body.chars() {
                    match c.to_digit(radix as u32) {
                        Some(d) => {
                            val = val * radix as f64 + d as f64;
                            any = true;
                        }
                        None => break,
                    }
                }
                if !any {
                    return Ok(Value::Num(f64::NAN));
                }
                Ok(Value::Num(if neg { -val } else { val }))
            }
            Native::ParseFloat => {
                let s = args.first().map(to_display).unwrap_or_default();
                let t = s.trim();
                // 앞부분의 유효한 수 프리픽스만
                let mut end = 0;
                let bytes = t.as_bytes();
                let mut seen_dot = false;
                if end < bytes.len() && (bytes[end] == b'-' || bytes[end] == b'+') {
                    end += 1;
                }
                while end < bytes.len()
                    && (bytes[end].is_ascii_digit() || (bytes[end] == b'.' && !seen_dot))
                {
                    if bytes[end] == b'.' {
                        seen_dot = true;
                    }
                    end += 1;
                }
                Ok(t[..end].parse::<f64>().map(Value::Num).unwrap_or(Value::Num(f64::NAN)))
            }
            Native::EncodeUri => {
                // encodeURI: 예약문자(;,/?:@&=+$#) 와 비예약문자는 보존
                Ok(Value::Str(uri_encode(&args.first().map(to_display).unwrap_or_default(), ";,/?:@&=+$#")))
            }
            Native::EncodeUriComponent => {
                // encodeURIComponent: 비예약문자만 보존 (예약문자도 인코딩)
                Ok(Value::Str(uri_encode(&args.first().map(to_display).unwrap_or_default(), "")))
            }
            Native::DecodeUri | Native::DecodeUriComponent => {
                Ok(Value::Str(uri_decode(&args.first().map(to_display).unwrap_or_default())))
            }
            Native::UrlCtor => self.make_url(args), // URL(x) (new 없이) — 관대하게 생성
            Native::UrlToString => Ok(Value::Str(recv_prop_str(&recv, "href"))),
            Native::UrlSearchToString => Ok(Value::Str(recv_prop_str(&recv, "__query"))),
            Native::UrlSearchGet => {
                let key = args.first().map(to_display).unwrap_or_default();
                Ok(parse_query(&recv_prop_str(&recv, "__query"))
                    .into_iter()
                    .find(|(k, _)| *k == key)
                    .map(|(_, v)| Value::Str(v))
                    .unwrap_or(Value::Null))
            }
            Native::UrlSearchGetAll => {
                let key = args.first().map(to_display).unwrap_or_default();
                let vals: Vec<Value> = parse_query(&recv_prop_str(&recv, "__query"))
                    .into_iter()
                    .filter(|(k, _)| *k == key)
                    .map(|(_, v)| Value::Str(v))
                    .collect();
                Ok(Value::Arr(ArrayObj::new(vals)))
            }
            Native::UrlSearchHas => {
                let key = args.first().map(to_display).unwrap_or_default();
                Ok(Value::Bool(
                    parse_query(&recv_prop_str(&recv, "__query")).iter().any(|(k, _)| *k == key),
                ))
            }
            Native::UrlSearchSet => {
                if let Some(Value::Obj(o)) = &recv {
                    let mut pairs = parse_query(&recv_prop_str(&recv, "__query"));
                    let key = args.first().map(to_display).unwrap_or_default();
                    let val = args.get(1).map(to_display).unwrap_or_default();
                    pairs.retain(|(k, _)| *k != key);
                    pairs.push((key, val));
                    o.borrow_mut().insert("__query".to_string(), Value::Str(build_query(&pairs)));
                }
                Ok(Value::Undefined)
            }
            Native::UrlSearchAppend => {
                if let Some(Value::Obj(o)) = &recv {
                    let mut pairs = parse_query(&recv_prop_str(&recv, "__query"));
                    let key = args.first().map(to_display).unwrap_or_default();
                    let val = args.get(1).map(to_display).unwrap_or_default();
                    pairs.push((key, val));
                    o.borrow_mut().insert("__query".to_string(), Value::Str(build_query(&pairs)));
                }
                Ok(Value::Undefined)
            }
            Native::UrlSearchDelete => {
                if let Some(Value::Obj(o)) = &recv {
                    let mut pairs = parse_query(&recv_prop_str(&recv, "__query"));
                    let key = args.first().map(to_display).unwrap_or_default();
                    pairs.retain(|(k, _)| *k != key);
                    o.borrow_mut().insert("__query".to_string(), Value::Str(build_query(&pairs)));
                }
                Ok(Value::Undefined)
            }
            Native::IsNaN => {
                Ok(Value::Bool(args.first().map(to_num).unwrap_or(f64::NAN).is_nan()))
            }
            Native::StructuredClone => {
                Ok(deep_clone(args.first().unwrap_or(&Value::Undefined), 0))
            }
            Native::ReflectGet => {
                let target = args.first().cloned().unwrap_or(Value::Undefined);
                let key = args.get(1).map(to_display).unwrap_or_default();
                self.member_get(&target, &key)
            }
            Native::ReflectSet => {
                let key = args.get(1).map(to_display).unwrap_or_default();
                let val = args.get(2).cloned().unwrap_or(Value::Undefined);
                let mut ok = false;
                match args.first() {
                    Some(Value::Obj(o)) => {
                        o.borrow_mut().insert(key, val);
                        ok = true;
                    }
                    Some(Value::Instance(i)) => {
                        i.fields.borrow_mut().insert(key, val);
                        ok = true;
                    }
                    _ => {}
                }
                Ok(Value::Bool(ok))
            }
            Native::ReflectHas => {
                let key = args.get(1).map(to_display).unwrap_or_default();
                let has = match args.first() {
                    Some(Value::Obj(m)) => {
                        !is_internal_key(&key)
                            && (m.borrow().contains_key(&key)
                                || self
                                    .proto_chain_lookup(m, &key, args.first().unwrap())
                                    .map(|v| v.is_some())
                                    .unwrap_or(false))
                    }
                    Some(Value::Instance(i)) => i.fields.borrow().contains_key(&key),
                    _ => false,
                };
                Ok(Value::Bool(has))
            }
            Native::ReflectDeleteProperty => {
                let key = args.get(1).map(to_display).unwrap_or_default();
                if let Some(Value::Obj(o)) = args.first() {
                    o.borrow_mut().remove(&key);
                }
                Ok(Value::Bool(true))
            }
            Native::ReflectApply => {
                // Reflect.apply(fn, thisArg, argsList)
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                let this = args.get(1).cloned();
                let arg_list = match args.get(2) {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                self.call_value(f, this, arg_list)
            }
            Native::ReflectConstruct => {
                // Reflect.construct(fn, argsList)
                let f = args.first().cloned().unwrap_or(Value::Undefined);
                let arg_list = match args.get(1) {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                self.construct(f, arg_list)
            }
            Native::LsGetItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                Ok(self.storage.get(&k).map(|v| Value::Str(v.clone())).unwrap_or(Value::Null))
            }
            Native::LsSetItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                let v = args.get(1).map(to_display).unwrap_or_default();
                self.storage.insert(k, v);
                Ok(Value::Undefined)
            }
            Native::LsRemoveItem => {
                let k = args.first().map(to_display).unwrap_or_default();
                self.storage.remove(&k);
                Ok(Value::Undefined)
            }
            Native::LsClear => {
                self.storage.clear();
                Ok(Value::Undefined)
            }
            Native::Alert => {
                let msg = args.iter().map(to_display).collect::<Vec<_>>().join(" ");
                self.console.push(format!("[alert] {}", msg));
                Ok(Value::Undefined)
            }
            Native::Noop => Ok(Value::Undefined),
            Native::ObjectKeys => match args.first() {
                Some(Value::Obj(m)) => {
                    let keys: Vec<Value> = m
                        .borrow()
                        .keys()
                        .filter(|k| !is_internal_key(k.as_str()))
                        .map(|k| Value::Str(k.clone()))
                        .collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                Some(Value::Arr(a)) => {
                    let keys: Vec<Value> =
                        (0..a.borrow().len()).map(|i| Value::Str(i.to_string())).collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                Some(Value::Instance(i)) => {
                    let keys: Vec<Value> =
                        i.fields.borrow().keys().map(|k| Value::Str(k.clone())).collect();
                    Ok(Value::Arr(ArrayObj::new(keys)))
                }
                _ => Ok(Value::Arr(ArrayObj::new(Vec::new()))),
            },
            Native::ObjectValues => {
                let vals: Vec<Value> = match args.first() {
                    Some(Value::Obj(m)) => m
                        .borrow()
                        .iter()
                        .filter(|(k, _)| !is_internal_key(k.as_str()))
                        .map(|(_, v)| v.clone())
                        .collect(),
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    Some(Value::Instance(i)) => i.fields.borrow().values().cloned().collect(),
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(vals)))
            }
            Native::ObjectEntries => {
                let pair = |k: &str, v: &Value| {
                    Value::Arr(ArrayObj::new(vec![Value::Str(k.to_string()), v.clone()]))
                };
                let entries: Vec<Value> = match args.first() {
                    Some(Value::Obj(m)) => m
                        .borrow()
                        .iter()
                        .filter(|(k, _)| !is_internal_key(k.as_str()))
                        .map(|(k, v)| pair(k, v))
                        .collect(),
                    Some(Value::Arr(a)) => a
                        .borrow()
                        .iter()
                        .enumerate()
                        .map(|(i, v)| pair(&i.to_string(), v))
                        .collect(),
                    Some(Value::Instance(inst)) => {
                        inst.fields.borrow().iter().map(|(k, v)| pair(k, v)).collect()
                    }
                    _ => Vec::new(),
                };
                Ok(Value::Arr(ArrayObj::new(entries)))
            }
            Native::ObjectFromEntries => {
                // [[k,v], ...] 또는 Map → 객체. 이터러블 순회.
                let mut map = ObjMap::new();
                if let Some(src) = args.first() {
                    for entry in self.iterate_to_vec(src) {
                        let (k, v) = match &entry {
                            Value::Arr(a) => {
                                let b = a.borrow();
                                (b.first().cloned(), b.get(1).cloned())
                            }
                            _ => (None, None),
                        };
                        if let Some(k) = k {
                            map.insert(to_display(&k), v.unwrap_or(Value::Undefined));
                        }
                    }
                }
                Ok(Value::Obj(Rc::new(RefCell::new(map))))
            }
            Native::ObjectAssign => {
                let Some(Value::Obj(target)) = args.first() else {
                    return Err("Object.assign 대상은 객체".to_string());
                };
                for src in &args[1..] {
                    if let Value::Obj(m) = src {
                        for (k, v) in m.borrow().iter() {
                            if !is_internal_key(k.as_str()) {
                                target.borrow_mut().insert(k.clone(), v.clone());
                            }
                        }
                    }
                }
                Ok(args.into_iter().next().unwrap())
            }
            Native::ArrayIsArray => {
                Ok(Value::Bool(matches!(args.first(), Some(Value::Arr(_)))))
            }
            Native::ArrayOf => Ok(Value::Arr(ArrayObj::new(args))),
            Native::ArrayFrom => {
                let src = args.first().cloned().unwrap_or(Value::Undefined);
                let map_fn = args.get(1).cloned().filter(is_callable);
                // 이터러블(배열/문자열/Set/Map/반복자)이면 그대로, 아니면 array-like(length).
                let items: Vec<Value> = match &src {
                    Value::Arr(_)
                    | Value::Str(_)
                    | Value::SetVal(_)
                    | Value::MapVal(_) => self.iterate_to_vec(&src),
                    Value::Obj(o)
                        if o.borrow().contains_key("__items") || o.borrow().contains_key("next") =>
                    {
                        self.iterate_to_vec(&src)
                    }
                    // array-like: length + 인덱스 프로퍼티
                    Value::Obj(o) => {
                        let len = o.borrow().get("length").map(to_num).unwrap_or(0.0);
                        let len = if len.is_finite() && len > 0.0 { len as usize } else { 0 };
                        (0..len)
                            .map(|i| o.borrow().get(&i.to_string()).cloned().unwrap_or(Value::Undefined))
                            .collect()
                    }
                    _ => Vec::new(),
                };
                // mapFn(value, index) 적용
                let out = match map_fn {
                    Some(f) => {
                        let mut r = Vec::with_capacity(items.len());
                        for (i, v) in items.into_iter().enumerate() {
                            r.push(self.call_value(f.clone(), None, vec![v, Value::Num(i as f64)])?);
                        }
                        r
                    }
                    None => items,
                };
                Ok(Value::Arr(ArrayObj::new(out)))
            }
            Native::SetTimeout | Native::SetInterval => {
                let callback = args.first().cloned().unwrap_or(Value::Undefined);
                if !matches!(callback, Value::Fn(_) | Value::Native(_)) {
                    return Ok(Value::Num(0.0)); // 콜백 아니면 무시
                }
                let delay_ms = args.get(1).map(to_num).unwrap_or(0.0).max(0.0);
                let id = self.next_timer_id;
                self.next_timer_id += 1;
                self.timers.push(Timer {
                    id,
                    callback,
                    delay_ms,
                    repeat: n == Native::SetInterval,
                });
                Ok(Value::Num(id as f64))
            }
            Native::ClearTimer => {
                if let Some(id) = args.first().map(to_num) {
                    let id = id as u64;
                    self.cleared.insert(id);
                    self.timers.retain(|t| t.id != id);
                }
                Ok(Value::Undefined)
            }
            // Promise(executor) (new 없이 호출) — 관대하게 생성자처럼
            Native::PromiseCtor => self.construct(Value::Native(Native::PromiseCtor), args),
            // executor 의 resolve(v): this(=promise)를 v 로 이행
            Native::PromiseSettleResolve => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Some(p) = recv {
                    // 이미 정착된 promise 는 무시 (한 번만)
                    let pending = matches!(&p, Value::Obj(o)
                        if matches!(o.borrow().get("__state"), Some(Value::Str(s)) if s == "pending"));
                    if pending {
                        self.resolve_promise(&p, v);
                    }
                }
                Ok(Value::Undefined)
            }
            // executor 의 reject(e): this(=promise)를 거부 상태로
            Native::PromiseSettleReject => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                if let Some(Value::Obj(o)) = recv {
                    let mut m = o.borrow_mut();
                    if matches!(m.get("__state"), Some(Value::Str(s)) if s == "pending") {
                        m.insert("__state".to_string(), Value::Str("rejected".to_string()));
                        m.insert("__value".to_string(), v);
                    }
                }
                Ok(Value::Undefined)
            }
            Native::PromiseResolve => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                let p = self.new_promise();
                self.resolve_promise(&p, v);
                Ok(p)
            }
            Native::PromiseReject => {
                let v = args.into_iter().next().unwrap_or(Value::Undefined);
                let p = self.new_promise();
                if let Value::Obj(o) = &p {
                    let mut m = o.borrow_mut();
                    m.insert("__state".to_string(), Value::Str("rejected".to_string()));
                    m.insert("__value".to_string(), v);
                }
                Ok(p)
            }
            Native::PromiseAll | Native::PromiseAllSettled => {
                let all_settled = matches!(n, Native::PromiseAllSettled);
                let items = match args.into_iter().next() {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                let p = self.new_promise();
                let mut out = Vec::new();
                for item in items {
                    let (fulfilled, v) = self.promise_settle_state(&item);
                    if all_settled {
                        // allSettled: 항목마다 {status, value|reason}
                        let mut m = ObjMap::new();
                        if fulfilled {
                            m.insert("status".to_string(), Value::Str("fulfilled".to_string()));
                            m.insert("value".to_string(), v);
                        } else {
                            m.insert("status".to_string(), Value::Str("rejected".to_string()));
                            m.insert("reason".to_string(), v);
                        }
                        out.push(Value::Obj(Rc::new(RefCell::new(m))));
                    } else if !fulfilled {
                        // all: 하나라도 거부되면 그 이유로 즉시 거부
                        self.reject_promise(&p, v);
                        return Ok(p);
                    } else {
                        out.push(v);
                    }
                }
                self.resolve_promise(&p, Value::Arr(ArrayObj::new(out)));
                Ok(p)
            }
            Native::PromiseRace => {
                let items = match args.into_iter().next() {
                    Some(Value::Arr(a)) => a.borrow().clone(),
                    _ => Vec::new(),
                };
                let p = self.new_promise();
                // 첫 항목의 정착 상태를 채택(이행/거부)
                if let Some(first) = items.first() {
                    let (fulfilled, v) = self.promise_settle_state(first);
                    if fulfilled {
                        self.resolve_promise(&p, v);
                    } else {
                        self.reject_promise(&p, v);
                    }
                }
                Ok(p)
            }
            Native::PromiseThen => {
                let p = recv.unwrap_or(Value::Undefined);
                let mut it = args.into_iter();
                let on_f = it.next().unwrap_or(Value::Undefined);
                let on_r = it.next().unwrap_or(Value::Undefined);
                let dep = self.new_promise();
                Ok(self.promise_then(&p, on_f, on_r, dep))
            }
            // p.catch(onR) = then(undefined, onR)
            Native::PromiseCatch => {
                let p = recv.unwrap_or(Value::Undefined);
                let on_r = args.into_iter().next().unwrap_or(Value::Undefined);
                let dep = self.new_promise();
                Ok(self.promise_then(&p, Value::Undefined, on_r, dep))
            }
            // p.finally(cb): 이행/거부 모두 cb 실행 후 원 결과(값/거부)를 전파.
            // 동기 모델: 대기 마이크로태스크를 먼저 흘려 p 를 정착시킨 뒤 cb 실행하고
            // p 를 그대로 반환(정착 상태 — 이행값 또는 거부이유 — 유지).
            Native::PromiseFinally => {
                let p = recv.unwrap_or(Value::Undefined);
                if let Some(cb) = args.into_iter().next() {
                    if is_callable(&cb) {
                        self.drain_microtasks();
                        let _ = self.call_value(cb, None, vec![]);
                    }
                }
                Ok(p)
            }
            Native::Identity => Ok(args.into_iter().next().unwrap_or(Value::Undefined)),
            Native::Fetch => {
                let url = args.first().map(to_display).unwrap_or_default();
                let resp = self.new_promise();
                match crate::http::fetch(&url) {
                    Ok(r) => {
                        let body = String::from_utf8_lossy(&r.body).to_string();
                        let mut m = ObjMap::new();
                        m.insert("status".to_string(), Value::Num(r.status as f64));
                        m.insert(
                            "ok".to_string(),
                            Value::Bool(r.status >= 200 && r.status < 300),
                        );
                        m.insert("__body".to_string(), Value::Str(body));
                        m.insert("text".to_string(), Value::Native(Native::ResponseText));
                        m.insert("json".to_string(), Value::Native(Native::ResponseJson));
                        let response = Value::Obj(Rc::new(RefCell::new(m)));
                        self.resolve_promise(&resp, response);
                    }
                    Err(e) => {
                        self.console.push(format!("fetch 실패: {:?}", e));
                        self.resolve_promise(&resp, Value::Undefined);
                    }
                }
                Ok(resp)
            }
            Native::ResponseText | Native::ResponseJson => {
                let body = match &recv {
                    Some(Value::Obj(o)) => match o.borrow().get("__body") {
                        Some(Value::Str(s)) => s.clone(),
                        _ => String::new(),
                    },
                    _ => String::new(),
                };
                let val = if matches!(n, Native::ResponseJson) {
                    json_parse(&body).unwrap_or(Value::Null)
                } else {
                    Value::Str(body)
                };
                let p = self.new_promise();
                self.resolve_promise(&p, val);
                Ok(p)
            }
            Native::GetAttribute => match recv {
                Some(Value::Dom(id)) => {
                    let name = args.first().map(to_display).unwrap_or_default();
                    let dom = self.dom_arena()?;
                    match &dom.get(id).node_type {
                        crate::dom::NodeType::Element(e) => Ok(e
                            .attributes
                            .get(&name)
                            .map(|v| Value::Str(v.clone()))
                            .unwrap_or(Value::Null)),
                        _ => Ok(Value::Null),
                    }
                }
                _ => Err("getAttribute 는 요소 메서드".to_string()),
            },
        }
    }
}
