# Kestrel M3b — 관용적 HTML 파서 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `html.rs`를 관용적 파서로 재작성해 실제 웹페이지 HTML에 절대 패닉하지 않고 합리적인 DOM을 만든다.

**Architecture:** 바이트 기반 토크나이저 + 스택 기반 트리 빌더(`Vec<(ElementData, Vec<Node>)>`). 끝태그는 스택에서 매칭까지 pop(자동 닫기), EOF에서 전부 닫음. `parse(String)->Node`와 DOM 타입은 불변.

**Tech Stack:** Rust(edition 2021). 표준 라이브러리만.

## Global Constraints

- 프로젝트 위치: `~/Documents/Projects/kestrel/`. 다른 저장소 건드리지 않는다.
- `parse(source: String) -> Node` 시그니처와 `dom` 타입은 변경하지 않는다.
- 어떤 입력에도 **패닉 금지**(assert/unwrap-on-input 금지).
- void/raw-text/주석/doctype/엔티티/대문자/안 닫힌 태그를 관용적으로 처리.
- 완전한 HTML5 트리 구성(삽입 모드)은 비범위.
- 커밋 메시지 끝에: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: `html.rs` 관용적 파서 재작성

**Files:**
- Modify: `src/html.rs` (전체 재작성)

**Interfaces:**
- Consumes: `crate::dom::{self, AttrMap, ElementData, Node, NodeType}`
- Produces: `pub fn parse(source: String) -> Node` (시그니처 불변)

- [ ] **Step 1: 테스트 먼저 — 기존 3개 유지 + 관용성 5개 추가**

`src/html.rs`의 `mod tests`를 아래로 한다(구현은 Step 3에서 위에 작성):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::NodeType;

    fn tag_names(node: &Node, out: &mut Vec<String>) {
        if let NodeType::Element(e) = &node.node_type {
            out.push(e.tag_name.clone());
        }
        for c in &node.children {
            tag_names(c, out);
        }
    }

    fn all_text(node: &Node, out: &mut String) {
        if let NodeType::Text(t) = &node.node_type {
            out.push_str(t);
        }
        for c in &node.children {
            all_text(c, out);
        }
    }

    // --- 기존 M1 동작 보존 ---
    #[test]
    fn parses_single_element_with_text() {
        let node = parse("<p>hello</p>".to_string());
        match node.node_type {
            NodeType::Element(ref e) => assert_eq!(e.tag_name, "p"),
            _ => panic!("expected element"),
        }
        let mut s = String::new();
        all_text(&node, &mut s);
        assert_eq!(s, "hello");
    }

    #[test]
    fn parses_attributes() {
        let node = parse("<div id=\"main\" class=\"a b\"></div>".to_string());
        if let NodeType::Element(ref e) = node.node_type {
            assert_eq!(e.attributes.get("id").map(|s| s.as_str()), Some("main"));
            assert_eq!(e.attributes.get("class").map(|s| s.as_str()), Some("a b"));
        } else {
            panic!("expected element");
        }
    }

    #[test]
    fn wraps_multiple_roots_in_html() {
        let node = parse("<p></p><p></p>".to_string());
        if let NodeType::Element(ref e) = node.node_type {
            assert_eq!(e.tag_name, "html");
        } else {
            panic!("expected synthetic html root");
        }
    }

    // --- M3b 관용성 ---
    #[test]
    fn skips_doctype_and_void_meta() {
        let n = parse(
            "<!doctype html><html><head><meta charset=\"utf-8\"></head><body><p>hi</p></body></html>"
                .to_string(),
        );
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.contains(&"html".to_string()));
        assert!(names.contains(&"meta".to_string()));
        assert!(names.contains(&"p".to_string()));
    }

    #[test]
    fn auto_closes_unclosed_tags() {
        let n = parse("<div><p>a<p>b</div>".to_string());
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.iter().filter(|s| *s == "p").count() >= 1);
        assert!(names.contains(&"div".to_string()));
    }

    #[test]
    fn raw_text_style_not_parsed_as_html() {
        let n = parse("<style>.a > .b { color: red; }</style>".to_string());
        let mut css = String::new();
        all_text(&n, &mut css);
        assert!(css.contains(".a > .b"), "style content preserved: {:?}", css);
    }

    #[test]
    fn decodes_entities() {
        let n = parse("<p>a &amp; b &#65;</p>".to_string());
        let mut s = String::new();
        all_text(&n, &mut s);
        assert!(s.contains("a & b A"), "got {:?}", s);
    }

    #[test]
    fn lowercases_tags_no_panic_on_messy() {
        let n = parse("<DIV><BR><img src=x><span>ok".to_string());
        let mut names = vec![];
        tag_names(&n, &mut names);
        assert!(names.contains(&"div".to_string()));
        assert!(names.contains(&"br".to_string()));
        assert!(names.contains(&"img".to_string()));
    }
}
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test html`
Expected: 새 테스트 컴파일/실행은 되지만 기존 토이 파서가 doctype 등에 패닉(또는 실패).

- [ ] **Step 3: 구현 — 토크나이저 + 빌더 + 엔티티**

`src/html.rs`의 테스트 모듈 위쪽을 아래로 **전부 교체**:

```rust
use std::collections::HashMap;

use crate::dom::{self, AttrMap, ElementData, Node, NodeType};

pub fn parse(source: String) -> Node {
    let mut t = Tokenizer { input: source.into_bytes(), pos: 0 };
    let mut b = Builder { stack: Vec::new(), roots: Vec::new() };

    while !t.eof() {
        if t.starts_with(b"<!--") {
            t.pos += 4;
            t.skip_past(b"-->");
        } else if t.starts_with(b"<!") {
            t.skip_past(b">");
        } else if t.starts_with(b"</") {
            let name = t.read_close_tag();
            b.close(&name);
        } else if t.peek() == b'<' && t.peek_at(1).map_or(false, |c| c.is_ascii_alphabetic()) {
            let (name, attrs, self_closing) = t.read_start_tag();
            if is_void(&name) || self_closing {
                b.add(elem_node(name, attrs, Vec::new()));
            } else if name == "script" || name == "style" {
                let raw = t.read_raw_until_close(&name);
                let children = if raw.is_empty() { Vec::new() } else { vec![dom::text(raw)] };
                b.add(elem_node(name, attrs, children));
            } else {
                b.open(ElementData { tag_name: name, attributes: attrs });
            }
        } else {
            let text = t.read_text();
            if !text.is_empty() {
                b.add(dom::text(text));
            }
        }
    }
    b.close_all();

    if b.roots.len() == 1 {
        b.roots.pop().unwrap()
    } else {
        elem_node("html".to_string(), HashMap::new(), b.roots)
    }
}

fn elem_node(name: String, attrs: AttrMap, children: Vec<Node>) -> Node {
    Node { children, node_type: NodeType::Element(ElementData { tag_name: name, attributes: attrs }) }
}

fn is_void(name: &str) -> bool {
    matches!(
        name,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input" | "link" | "meta"
            | "param" | "source" | "track" | "wbr"
    )
}

struct Builder {
    stack: Vec<(ElementData, Vec<Node>)>,
    roots: Vec<Node>,
}

impl Builder {
    fn add(&mut self, node: Node) {
        if let Some((_, children)) = self.stack.last_mut() {
            children.push(node);
        } else {
            self.roots.push(node);
        }
    }
    fn open(&mut self, elem: ElementData) {
        self.stack.push((elem, Vec::new()));
    }
    fn close(&mut self, name: &str) {
        if let Some(pos) = self.stack.iter().rposition(|(e, _)| e.tag_name == name) {
            while self.stack.len() > pos {
                let (elem, children) = self.stack.pop().unwrap();
                let node = Node { children, node_type: NodeType::Element(elem) };
                self.add(node);
            }
        }
        // 매칭 없음: 스트레이 끝태그 무시
    }
    fn close_all(&mut self) {
        while let Some((elem, children)) = self.stack.pop() {
            let node = Node { children, node_type: NodeType::Element(elem) };
            self.add(node);
        }
    }
}

struct Tokenizer {
    input: Vec<u8>,
    pos: usize,
}

impl Tokenizer {
    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }
    fn peek(&self) -> u8 {
        self.input[self.pos]
    }
    fn peek_at(&self, o: usize) -> Option<u8> {
        self.input.get(self.pos + o).copied()
    }
    fn starts_with(&self, s: &[u8]) -> bool {
        self.input[self.pos..].starts_with(s)
    }
    fn skip_whitespace(&mut self) {
        while !self.eof() && self.peek().is_ascii_whitespace() {
            self.pos += 1;
        }
    }
    fn skip_past(&mut self, needle: &[u8]) {
        while !self.eof() && !self.input[self.pos..].starts_with(needle) {
            self.pos += 1;
        }
        if self.input[self.pos..].starts_with(needle) {
            self.pos += needle.len();
        }
    }

    fn read_text(&mut self) -> String {
        let start = self.pos;
        // 진행 보장: 최소 1바이트(스트레이 '<' 포함) 소비
        if !self.eof() {
            self.pos += 1;
        }
        while !self.eof() && self.peek() != b'<' {
            self.pos += 1;
        }
        decode_entities(&String::from_utf8_lossy(&self.input[start..self.pos]))
    }

    fn read_close_tag(&mut self) -> String {
        self.pos += 2; // '</'
        let start = self.pos;
        while !self.eof() && self.peek() != b'>' && !self.peek().is_ascii_whitespace() {
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.input[start..self.pos]).to_ascii_lowercase();
        while !self.eof() && self.peek() != b'>' {
            self.pos += 1;
        }
        if !self.eof() {
            self.pos += 1; // '>'
        }
        name
    }

    fn read_tag_name(&mut self) -> String {
        let start = self.pos;
        while !self.eof() {
            let c = self.peek();
            if c.is_ascii_alphanumeric() || c == b'-' || c == b':' {
                self.pos += 1;
            } else {
                break;
            }
        }
        String::from_utf8_lossy(&self.input[start..self.pos]).to_ascii_lowercase()
    }

    fn read_start_tag(&mut self) -> (String, AttrMap, bool) {
        self.pos += 1; // '<'
        let name = self.read_tag_name();
        let mut attrs = HashMap::new();
        let mut self_closing = false;
        loop {
            self.skip_whitespace();
            if self.eof() {
                break;
            }
            match self.peek() {
                b'>' => {
                    self.pos += 1;
                    break;
                }
                b'/' => {
                    self_closing = true;
                    self.pos += 1;
                    if !self.eof() && self.peek() == b'>' {
                        self.pos += 1;
                    }
                    break;
                }
                _ => {
                    let (k, v) = self.read_attr();
                    if !k.is_empty() {
                        attrs.insert(k, v);
                    }
                }
            }
        }
        (name, attrs, self_closing)
    }

    fn read_attr(&mut self) -> (String, String) {
        let start = self.pos;
        while !self.eof() {
            let c = self.peek();
            if c == b'=' || c == b'>' || c == b'/' || c.is_ascii_whitespace() {
                break;
            }
            self.pos += 1;
        }
        let name = String::from_utf8_lossy(&self.input[start..self.pos]).to_ascii_lowercase();
        self.skip_whitespace();
        let mut value = String::new();
        if !self.eof() && self.peek() == b'=' {
            self.pos += 1;
            self.skip_whitespace();
            if !self.eof() && (self.peek() == b'"' || self.peek() == b'\'') {
                let quote = self.peek();
                self.pos += 1;
                let vs = self.pos;
                while !self.eof() && self.peek() != quote {
                    self.pos += 1;
                }
                value = String::from_utf8_lossy(&self.input[vs..self.pos]).to_string();
                if !self.eof() {
                    self.pos += 1;
                }
            } else {
                let vs = self.pos;
                while !self.eof() {
                    let c = self.peek();
                    if c == b'>' || c.is_ascii_whitespace() {
                        break;
                    }
                    self.pos += 1;
                }
                value = String::from_utf8_lossy(&self.input[vs..self.pos]).to_string();
            }
        }
        (name, decode_entities(&value))
    }

    fn read_raw_until_close(&mut self, name: &str) -> String {
        let close = format!("</{}", name).into_bytes();
        let start = self.pos;
        while !self.eof() {
            let end = self.pos + close.len();
            if end <= self.input.len() && self.input[self.pos..end].eq_ignore_ascii_case(&close) {
                break;
            }
            self.pos += 1;
        }
        let raw = String::from_utf8_lossy(&self.input[start..self.pos]).to_string();
        // '</name ...>' 소비
        while !self.eof() && self.peek() != b'>' {
            self.pos += 1;
        }
        if !self.eof() {
            self.pos += 1;
        }
        raw
    }
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        if let Some(semi) = after.find(';') {
            if semi <= 12 {
                if let Some(ch) = decode_one(&after[1..semi]) {
                    out.push(ch);
                    rest = &after[semi + 1..];
                    continue;
                }
            }
        }
        out.push('&');
        rest = &after[1..];
    }
    out.push_str(rest);
    out
}

fn decode_one(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{00A0}'),
        _ => {
            let num = entity.strip_prefix('#')?;
            let (radix, digits) = match num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
                Some(hex) => (16, hex),
                None => (10, num),
            };
            char::from_u32(u32::from_str_radix(digits, radix).ok()?)
        }
    }
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test html`
Expected: 8개 PASS(기존 3 + 신규 5).

- [ ] **Step 5: 전체 테스트 + 커밋**

Run: `source ~/.cargo/env && cargo test`
Expected: 전부 PASS(파서 인터페이스 불변이라 M1/M2 영향 없음).

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/html.rs
git commit -m "$(printf 'feat(html): 관용적 파서 재작성 — 패닉 금지, void/raw-text/엔티티 처리\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 2: `--parse` 모드 + 실제 페이지 파싱 검증

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `crate::http::fetch`, `crate::html::parse`, `crate::dom`
- Produces: CLI `--parse <url>` 모드

- [ ] **Step 1: `--parse` 모드 추가**

`src/main.rs`의 `--fetch` 분기 바로 다음에 추가:

```rust
    if args.len() >= 3 && args[1] == "--parse" {
        match http::fetch(&args[2]) {
            Ok(resp) => {
                let html = String::from_utf8_lossy(&resp.body).to_string();
                let dom = html::parse(html);
                let mut count = 0usize;
                count_elements(&dom, &mut count);
                println!("parsed OK: {} elements (http {})", count, resp.status);
            }
            Err(e) => println!("fetch error: {:?}", e),
        }
        return;
    }
```

그리고 `main.rs` 하단에 헬퍼 추가:

```rust
fn count_elements(node: &dom::Node, count: &mut usize) {
    if let dom::NodeType::Element(_) = &node.node_type {
        *count += 1;
    }
    for c in &node.children {
        count_elements(c, count);
    }
}
```

- [ ] **Step 2: 빌드 + 전체 테스트**

Run: `source ~/.cargo/env && cargo build`
Expected: 성공.

Run: `source ~/.cargo/env && cargo test`
Expected: 전부 PASS.

- [ ] **Step 3: 실제 페이지 파싱 검증 (네트워크 필요)**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
cargo run -- --parse https://example.com
cargo run -- --parse https://naver.com
```
Expected: 둘 다 `parsed OK: N elements (http 200)` 출력, **패닉 없음**. naver는 수백~수천 요소.

- [ ] **Step 4: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/main.rs
git commit -m "$(printf 'feat: --parse 모드 + 실제 페이지 파싱 검증 (M3b 완성)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Self-Review

**1. Spec coverage:** doctype/주석 스킵, void 태그, raw-text(script/style), 엔티티, 대문자, 안 닫힌 태그 자동 복구, 스트레이 끝태그 무시, EOF 복구 → Task 1 코드 전부 구현. 헤르메틱 테스트(doctype/void, 자동닫기, raw-text, 엔티티, 대문자) → Task 1. 실제 페이지 파싱 검증 → Task 2. parse/DOM 불변 → 기존 테스트 유지로 확인.

**2. Placeholder scan:** 없음. 모든 코드 완전. assert/unwrap-on-input 없음(stack.pop().unwrap()은 rposition으로 존재 보장된 경우만).

**3. Type consistency:**
- `parse(String)->Node` 불변, `dom::{Node,NodeType,ElementData,AttrMap,text}` 기존과 일치. ✓
- `Builder`/`Tokenizer`는 내부 타입. `elem_node`/`is_void`/`decode_entities` 내부 함수. ✓
- `count_elements(&dom::Node, &mut usize)` — Task 2 정의/사용. ✓

불일치 없음.
