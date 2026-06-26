# Kestrel M3c — 실제 페이지 렌더 통합 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `kestrel <url>` 로 단순한 실제 페이지(example.com)를 직접 만든 브라우저 창에 렌더링한다.

**Architecture:** `css.rs`를 관용적(패닉 금지)으로 재작성하고 UA 기본 스타일시트를 추가. `main.rs`에서 fetch→parse→`<style>` CSS 추출→(UA+페이지) style→layout→paint→창. 인라인 CSS만.

**Tech Stack:** Rust(edition 2021). 기존 모듈 재사용.

## Global Constraints

- 프로젝트 위치: `~/Documents/Projects/kestrel/`. 다른 저장소 건드리지 않는다.
- `css::parse(String) -> Stylesheet` 시그니처와 CSS 타입은 변경하지 않는다.
- CSS 파서는 어떤 입력에도 **패닉 금지**: at-rule·복합 셀렉터·미지원 값은 스킵.
- 인라인 `<style>`만(외부 `<link>` 비범위). 단순 셀렉터(tag/id/class/`*`)만.
- UA 스타일시트로 블록 요소 기본 display:block.
- 커밋 메시지 끝에: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

### Task 1: `css.rs` 관용적 재작성 + UA 스타일시트

**Files:**
- Modify: `src/css.rs` (파서 부분 재작성, 타입 유지)

**Interfaces:**
- Consumes: 없음
- Produces:
  - `pub fn parse(source: String) -> Stylesheet` (불변, 관용적)
  - `pub fn user_agent_stylesheet() -> Stylesheet`
  - 타입(Stylesheet/Rule/Selector/SimpleSelector/Declaration/Value/Unit/Color/Specificity) 불변

- [ ] **Step 1: 테스트 — 기존 3개 유지 + 관용성 4개 추가**

`src/css.rs`의 `mod tests`를 아래로 한다:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rule_with_length_and_color() {
        let ss = parse("div { width: 100px; background-color: #ff0000; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        let rule = &ss.rules[0];
        assert_eq!(rule.declarations.len(), 2);
        assert_eq!(rule.declarations[0].name, "width");
        assert_eq!(rule.declarations[0].value, Value::Length(100.0, Unit::Px));
        assert_eq!(rule.declarations[1].value, Value::Color(Color { r: 255, g: 0, b: 0, a: 255 }));
    }

    #[test]
    fn parses_compound_selector() {
        let ss = parse("p.note { color: #112233; }".to_string());
        match &ss.rules[0].selectors[0] {
            Selector::Simple(s) => {
                assert_eq!(s.tag_name.as_deref(), Some("p"));
                assert_eq!(s.class, vec!["note".to_string()]);
                assert_eq!(s.id, None);
            }
        }
    }

    #[test]
    fn specificity_counts_id_class_tag() {
        let ss = parse("#x { color: #000000; }".to_string());
        assert_eq!(ss.rules[0].selectors[0].specificity(), (1, 0, 0));
    }

    #[test]
    fn skips_at_rules() {
        let ss = parse(
            "@media screen { p { color: #ff0000; } } div { width: 5px; }".to_string(),
        );
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
    }

    #[test]
    fn skips_unsupported_selectors() {
        let ss = parse(".a .b { color: #ff0000; } div { width: 5px; }".to_string());
        // 자손 결합자 규칙은 스킵, div 규칙만 남음
        assert_eq!(ss.rules.len(), 1);
        match &ss.rules[0].selectors[0] {
            Selector::Simple(s) => assert_eq!(s.tag_name.as_deref(), Some("div")),
        }
    }

    #[test]
    fn skips_unsupported_values() {
        let ss = parse("p { color: rgb(1,2,3); width: 5px; }".to_string());
        // rgb() 선언은 스킵, width 만 남음
        assert_eq!(ss.rules[0].declarations.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
    }

    #[test]
    fn ua_stylesheet_has_display_block_for_div() {
        let ss = user_agent_stylesheet();
        let matches_div = ss.rules.iter().any(|r| {
            r.selectors.iter().any(|s| match s {
                Selector::Simple(sel) => sel.tag_name.as_deref() == Some("div"),
            }) && r
                .declarations
                .iter()
                .any(|d| d.name == "display" && d.value == Value::Keyword("block".to_string()))
        });
        assert!(matches_div);
    }
}
```

- [ ] **Step 2: 테스트 실패 확인**

Run: `source ~/.cargo/env && cargo test css`
Expected: 새 테스트(at-rule 등)에서 기존 파서가 패닉/실패.

- [ ] **Step 3: 구현 — 파서 부분 교체**

`src/css.rs`에서 **타입 정의(Stylesheet ~ Color, Specificity, `impl Selector`, `impl Value`)는 그대로 두고**, `pub fn parse`부터 `fn valid_identifier_char`까지(파서 구현 전체)를 아래로 교체:

```rust
pub fn parse(source: String) -> Stylesheet {
    let mut parser = Parser { pos: 0, input: source };
    Stylesheet { rules: parser.parse_rules() }
}

const UA_CSS: &str = "html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, section, article, header, footer, nav, main, aside, blockquote, pre, table, tr, form, figure, figcaption, address, dl, dt, dd { display: block; }";

pub fn user_agent_stylesheet() -> Stylesheet {
    parse(UA_CSS.to_string())
}

struct Parser {
    pos: usize,
    input: String,
}

impl Parser {
    fn parse_rules(&mut self) -> Vec<Rule> {
        let mut rules = Vec::new();
        loop {
            self.consume_whitespace();
            if self.eof() {
                break;
            }
            if self.peek() == Some('@') {
                self.skip_at_rule();
                continue;
            }
            if let Some(rule) = self.parse_rule() {
                rules.push(rule);
            }
        }
        rules
    }

    fn parse_rule(&mut self) -> Option<Rule> {
        match self.parse_selectors() {
            Some(selectors) => {
                let declarations = self.parse_declarations();
                Some(Rule { selectors, declarations })
            }
            None => {
                self.skip_to_block_end();
                None
            }
        }
    }

    fn parse_selectors(&mut self) -> Option<Vec<Selector>> {
        let mut selectors = Vec::new();
        loop {
            selectors.push(Selector::Simple(self.parse_simple_selector()));
            self.consume_whitespace();
            match self.peek() {
                Some(',') => {
                    self.consume_char();
                    self.consume_whitespace();
                }
                Some('{') => {
                    self.consume_char();
                    break;
                }
                // 자손 결합자/의사클래스/속성/eof 등은 미지원 → 규칙 스킵
                _ => return None,
            }
        }
        selectors.sort_by(|a, b| b.specificity().cmp(&a.specificity()));
        Some(selectors)
    }

    fn parse_simple_selector(&mut self) -> SimpleSelector {
        let mut selector = SimpleSelector { tag_name: None, id: None, class: Vec::new() };
        while !self.eof() {
            match self.input[self.pos..].chars().next().unwrap() {
                '#' => {
                    self.consume_char();
                    selector.id = Some(self.parse_identifier());
                }
                '.' => {
                    self.consume_char();
                    selector.class.push(self.parse_identifier());
                }
                '*' => {
                    self.consume_char();
                }
                c if valid_identifier_char(c) => {
                    selector.tag_name = Some(self.parse_identifier());
                }
                _ => break,
            }
        }
        selector
    }

    fn parse_declarations(&mut self) -> Vec<Declaration> {
        // '{' 는 parse_selectors 에서 이미 소비됨
        let mut declarations = Vec::new();
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                _ => {
                    if let Some(decl) = self.parse_declaration() {
                        declarations.push(decl);
                    }
                }
            }
        }
        declarations
    }

    fn parse_declaration(&mut self) -> Option<Declaration> {
        let name = self.parse_identifier().trim().to_ascii_lowercase();
        self.consume_whitespace();
        if self.peek() != Some(':') {
            self.skip_to_decl_end();
            return None;
        }
        self.consume_char(); // ':'
        self.consume_whitespace();
        let value_text = self.consume_while(|c| c != ';' && c != '}');
        if self.peek() == Some(';') {
            self.consume_char();
        }
        if name.is_empty() {
            return None;
        }
        let value = interpret_value(value_text.trim())?;
        Some(Declaration { name, value })
    }

    fn skip_to_decl_end(&mut self) {
        self.consume_while(|c| c != ';' && c != '}');
        if self.peek() == Some(';') {
            self.consume_char();
        }
    }

    fn skip_at_rule(&mut self) {
        while !self.eof() {
            let c = self.consume_char();
            if c == ';' {
                return;
            }
            if c == '{' {
                self.skip_block();
                return;
            }
        }
    }

    fn skip_to_block_end(&mut self) {
        while !self.eof() {
            let c = self.consume_char();
            if c == '{' {
                self.skip_block();
                return;
            }
            if c == '}' {
                return;
            }
        }
    }

    fn skip_block(&mut self) {
        // 여는 '{' 는 이미 소비됨
        let mut depth = 1;
        while !self.eof() && depth > 0 {
            match self.consume_char() {
                '{' => depth += 1,
                '}' => depth -= 1,
                _ => {}
            }
        }
    }

    fn parse_identifier(&mut self) -> String {
        self.consume_while(valid_identifier_char)
    }

    fn peek(&self) -> Option<char> {
        self.input[self.pos..].chars().next()
    }

    fn eof(&self) -> bool {
        self.pos >= self.input.len()
    }

    fn consume_char(&mut self) -> char {
        let mut iter = self.input[self.pos..].char_indices();
        let (_, cur_char) = iter.next().unwrap();
        let (next_pos, _) = iter.next().unwrap_or((1, ' '));
        self.pos += next_pos;
        cur_char
    }

    fn consume_while<F>(&mut self, test: F) -> String
    where
        F: Fn(char) -> bool,
    {
        let mut result = String::new();
        while !self.eof() && test(self.peek().unwrap()) {
            result.push(self.consume_char());
        }
        result
    }

    fn consume_whitespace(&mut self) {
        self.consume_while(char::is_whitespace);
    }
}

fn interpret_value(text: &str) -> Option<Value> {
    if text.is_empty() {
        return None;
    }
    let bytes = text.as_bytes();
    if bytes[0] == b'#' {
        if text.len() == 7 {
            let r = u8::from_str_radix(&text[1..3], 16).ok()?;
            let g = u8::from_str_radix(&text[3..5], 16).ok()?;
            let b = u8::from_str_radix(&text[5..7], 16).ok()?;
            return Some(Value::Color(Color { r, g, b, a: 255 }));
        }
        return None;
    }
    let numeric_start = bytes[0].is_ascii_digit()
        || bytes[0] == b'.'
        || (bytes[0] == b'-' && bytes.len() > 1 && (bytes[1].is_ascii_digit() || bytes[1] == b'.'));
    if numeric_start {
        if let Some(num) = text.strip_suffix("px") {
            if let Ok(f) = num.trim().parse::<f32>() {
                return Some(Value::Length(f, Unit::Px));
            }
        }
        return None; // em/%/rem/단위없는수 등은 미지원
    }
    if text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Some(Value::Keyword(text.to_string()));
    }
    None // rgb()/calc()/다중값 등
}

fn valid_identifier_char(c: char) -> bool {
    matches!(c, 'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_')
}
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `source ~/.cargo/env && cargo test css`
Expected: 7개 PASS(기존 3 + 신규 4).

- [ ] **Step 5: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/css.rs
git commit -m "$(printf 'feat(css): 관용적 파서 — at-rule/복합셀렉터/미지원값 스킵 + UA 스타일시트\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

### Task 2: `kestrel <url>` 통합 + 실제 렌더 검증

**Files:**
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: `http::fetch`, `html::parse`, `css::{parse, user_agent_stylesheet}`, `style`, `layout`, `paint`, `font`, `raster`, `window`
- Produces: `kestrel <url>` 렌더 모드

- [ ] **Step 1: 통합 코드 추가**

`src/main.rs`의 `--parse` 분기 다음에 URL 위치 인자 분기 추가:

```rust
    // URL 렌더 모드: kestrel <url>
    if args.len() >= 2 && args[1].contains("://") {
        render_url(&args[1]);
        return;
    }
```

그리고 `main.rs` 하단(`count_elements` 근처)에 함수 추가:

```rust
fn extract_css(node: &dom::Node, out: &mut String) {
    if let dom::NodeType::Element(e) = &node.node_type {
        if e.tag_name == "style" {
            for c in &node.children {
                if let dom::NodeType::Text(t) = &c.node_type {
                    out.push_str(t);
                    out.push('\n');
                }
            }
        }
    }
    for c in &node.children {
        extract_css(c, out);
    }
}

fn render_url(url: &str) {
    let resp = match http::fetch(url) {
        Ok(r) => r,
        Err(e) => {
            println!("fetch error: {:?}", e);
            return;
        }
    };
    println!("fetched {} ({} bytes, http {})", url, resp.body.len(), resp.status);

    let html = String::from_utf8_lossy(&resp.body).to_string();
    let dom = html::parse(html);

    let mut page_css = String::new();
    extract_css(&dom, &mut page_css);

    let mut sheet = css::user_agent_stylesheet();
    sheet.rules.extend(css::parse(page_css).rules);

    let style_root = style::style_tree(&dom, &sheet);

    let viewport_width: u32 = 1000;
    let viewport_height: u32 = 1400;
    let mut viewport: layout::Dimensions = Default::default();
    viewport.content.width = viewport_width as f32;
    viewport.content.height = viewport_height as f32;

    let font = font::Font::from_bytes(fs::read("assets/fonts/Kestrel.ttf").expect("read font"))
        .expect("parse font");
    let mut cache = raster::GlyphCache::new();

    let layout_root = layout::layout_tree(&style_root, viewport, &font);
    let canvas = paint::paint(
        &layout_root,
        layout::Rect { x: 0.0, y: 0.0, width: viewport_width as f32, height: viewport_height as f32 },
        &font,
        &mut cache,
    );

    if let Ok(path) = std::env::var("KESTREL_RENDER_TO") {
        write_ppm(&canvas, &path);
        println!("rendered to {}", path);
        return;
    }
    window::run(canvas.to_u32_buffer(), viewport_width, viewport_height);
}
```

- [ ] **Step 2: 빌드 + 전체 테스트**

Run: `source ~/.cargo/env && cargo build`
Expected: 성공.

Run: `source ~/.cargo/env && cargo test`
Expected: 전부 PASS.

- [ ] **Step 3: 실제 페이지 렌더 검증 (네트워크 필요, 헤드리스)**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
KESTREL_RENDER_TO=target/example.ppm cargo run -- https://example.com
sips -s format png target/example.ppm --out target/example.png
```
Expected: `target/example.png`에 example.com이 **블록 레이아웃 + 텍스트("Example Domain" 제목과 본문 문단)**로 렌더. 패닉 없음. (링크 텍스트 등 인라인 요소 내부 텍스트는 미지원이라 빠질 수 있음 — 예상된 한계.)

- [ ] **Step 4: 커밋**

```bash
cd ~/Documents/Projects/kestrel && source ~/.cargo/env
git add src/main.rs
git commit -m "$(printf 'feat: kestrel <url> — fetch→parse→render 통합 (M3c 완성, 실제 페이지 렌더)\n\nCo-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>')"
```

---

## Self-Review

**1. Spec coverage:** CSS 견고화(at-rule/복합셀렉터/미지원값 스킵) → Task 1. UA 스타일시트 → Task 1 `user_agent_stylesheet`. 인라인 `<style>` 추출 → Task 2 `extract_css`. fetch→render 통합 + KESTREL_RENDER_TO → Task 2 `render_url`. 헤드리스 검증 → Task 2. 스펙 전부 커버.

**2. Placeholder scan:** 없음. 모든 코드 완전. css 타입은 유지(교체 범위는 파서 구현만).

**3. Type consistency:**
- `css::parse(String)->Stylesheet` 불변, `user_agent_stylesheet()->Stylesheet` 신규. `Stylesheet.rules` pub로 extend 가능. ✓
- `interpret_value(&str)->Option<Value>`, `Value::{Length,Color,Keyword}`/`Unit::Px`/`Color` 기존과 일치. ✓
- `extract_css(&dom::Node,&mut String)`, `render_url(&str)` — Task 2 정의/사용. ✓
- `style_tree`/`layout_tree(.., &font)`/`paint(.., &font, &mut cache)`/`write_ppm`/`window::run` — 기존 시그니처와 일치. ✓

불일치 없음.
