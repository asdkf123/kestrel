// 네트워크 바인딩: XMLHttpRequest / WebSocket / URL 질의 문자열.
use super::*;
use super::builtins::{percent_decode_lossy, uri_encode};

// (키,값) 쌍을 x-www-form-urlencoded 쿼리 문자열로 (각 성분 encodeURIComponent).
pub(super) fn build_query(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", uri_encode(k, ""), uri_encode(v, "")))
        .collect::<Vec<_>>()
        .join("&")
}

// application/x-www-form-urlencoded 쿼리를 (키,값) 쌍으로. '+' → 공백, %XX 디코드.
pub(super) fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|s| !s.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            // 폼 파싱은 관대 디코드(URIError 없음).
            (
                percent_decode_lossy(&k.replace('+', " ")),
                percent_decode_lossy(&v.replace('+', " ")),
            )
        })
        .collect()
}

impl Interp {
    // 현재 문서의 (호스트, 경로) — 쿠키 범위 판정용
    pub(super) fn page_host_path(&self) -> (String, String) {
        let raw = self
            .base_url
            .clone()
            .unwrap_or_else(|| "http://localhost/".to_string());
        match crate::url::Url::parse(&raw) {
            Ok(u) => {
                let path = u.path.split(['?', '#']).next().unwrap_or("/").to_string();
                (u.host, if path.is_empty() { "/".to_string() } else { path })
            }
            Err(_) => ("localhost".to_string(), "/".to_string()),
        }
    }

    // 상대 URL 을 페이지 기준으로 해석
    pub(super) fn resolve_url(&self, url: &str) -> String {
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
        m.insert("\u{0}isXhr".to_string(), Value::Bool(true));
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
        m.insert("removeEventListener".to_string(), Value::Native(Native::RemoveEventListener));
        m.insert("dispatchEvent".to_string(), Value::Native(Native::DispatchEvent));
        Value::Obj(Rc::new(RefCell::new(m)))
    }

    // new WebSocket(url [, protocols]) — 진짜로 연결한다 (RFC 6455 핸드셰이크).
    // 실패하면 error/close 이벤트를 쏜다 (표준: 생성자는 throw 하지 않는다 —
    // 연결 실패는 비동기 이벤트다. 여기서 throw 하면 스크립트가 통째로 죽는다).
    pub(super) fn make_websocket(&mut self, args: Vec<Value>) -> Value {
        let raw = args.first().map(to_display).unwrap_or_default();
        let url = self.absolute_url(&raw);
        // http(s) 기준 URL 로 절대화됐으면 ws(s) 로 되돌린다
        let url = if raw.starts_with("ws://") || raw.starts_with("wss://") {
            raw.clone()
        } else if let Some(rest) = url.strip_prefix("https://") {
            format!("wss://{}", rest)
        } else if let Some(rest) = url.strip_prefix("http://") {
            format!("ws://{}", rest)
        } else {
            url
        };

        let mut m = ObjMap::new();
        m.insert("url".to_string(), Value::Str(url.clone()));
        m.insert("readyState".to_string(), Value::Num(0.0)); // CONNECTING
        m.insert("bufferedAmount".to_string(), Value::Num(0.0));
        m.insert("protocol".to_string(), Value::Str(String::new()));
        m.insert("binaryType".to_string(), Value::Str("blob".to_string()));
        m.insert("send".to_string(), Value::Native(Native::WsSend));
        m.insert("close".to_string(), Value::Native(Native::WsClose));
        m.insert("addEventListener".to_string(), Value::Native(Native::AddEventListener));
        m.insert("removeEventListener".to_string(), Value::Native(Native::RemoveEventListener));
        m.insert("dispatchEvent".to_string(), Value::Native(Native::DispatchEvent));
        // 표준 상수
        m.insert("CONNECTING".to_string(), Value::Num(0.0));
        m.insert("OPEN".to_string(), Value::Num(1.0));
        m.insert("CLOSING".to_string(), Value::Num(2.0));
        m.insert("CLOSED".to_string(), Value::Num(3.0));
        let obj = Rc::new(RefCell::new(m));
        let val = Value::Obj(obj.clone());

        match crate::websocket::WebSocket::connect(&url) {
            Ok(ws) => {
                obj.borrow_mut().insert("readyState".to_string(), Value::Num(1.0));
                obj.borrow_mut()
                    .insert("protocol".to_string(), Value::Str(ws.protocol.clone()));
                obj.borrow_mut()
                    .insert("\u{0}sock".to_string(), Value::Num(self.sockets.len() as f64));
                self.sockets.push((ws, val.clone()));
                // open 이벤트는 마이크로태스크 뒤(핸들러 등록 후)에 와야 한다 —
                // 지금 부르면 onopen 을 붙이기도 전이라 아무도 못 듣는다.
                self.pending_ws_open.push(val.clone());
            }
            Err(e) => {
                self.console.push(format!("WebSocket 연결 실패: {}", e));
                obj.borrow_mut().insert("readyState".to_string(), Value::Num(3.0));
                self.pending_ws_error.push(val.clone());
            }
        }
        val
    }

    pub(super) fn ws_fire(&mut self, obj: &Rc<RefCell<ObjMap>>, event: &str, args: Vec<Value>) {
        let on = format!("on{}", event);
        let handler = obj.borrow().get(&on).cloned();
        if let Some(h) = handler {
            if is_callable(&h) {
                let _ = self.call_value(h, Some(Value::Obj(obj.clone())), args.clone());
            }
        }
        let listeners: Vec<Value> = match obj.borrow().get(&obj_listener_key(event)) {
            Some(Value::Arr(a)) => a.borrow().clone(),
            _ => Vec::new(),
        };
        for l in listeners {
            let _ = self.call_value(l, Some(Value::Obj(obj.clone())), args.clone());
        }
    }

    // XHR 발화: on<event> 프로퍼티 + addEventListener 로 등록된 리스너.
    // 예전엔 on<event> 만 불렀다 — addEventListener 로 붙인 load 핸들러가 영영 안 왔다.
    pub(super) fn xhr_fire(&mut self, obj: &Rc<RefCell<ObjMap>>, event: &str) {
        let on = format!("on{}", event);
        let handler = obj.borrow().get(&on).cloned();
        if let Some(h) = handler {
            if is_callable(&h) {
                let _ = self.call_value(h, Some(Value::Obj(obj.clone())), Vec::new());
            }
        }
        let listeners: Vec<Value> = match obj.borrow().get(&obj_listener_key(event)) {
            Some(Value::Arr(a)) => a.borrow().clone(),
            _ => Vec::new(),
        };
        for l in listeners {
            let _ = self.call_value(l, Some(Value::Obj(obj.clone())), Vec::new());
        }
    }
}
