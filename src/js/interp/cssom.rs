// CSSOM (§CSSOM): document.styleSheets / CSSStyleSheet / CSSStyleRule /
// CSSStyleDeclaration. 모두 파서가 만든 시트 목록에 대한 **살아 있는 뷰**다 —
// 스냅샷을 복사하면 insertRule 이 화면에 반영되지 않는다.
use super::*;

// CSS Nesting 중첩 규칙(.nested)을 읽기용 CSSOM 객체로 materialize(재귀). selectorText·
// cssText·type·cssRules 를 노출한다(라이브 mutation·instanceof 은 별도 addressing 필요).
// 규칙의 선언들을 `name: value;` 한 줄 목록으로. CSSOM cssText 는 지정값(specified)이므로
// raw 가 있으면 그것을(예: `red`), 없으면(단축 확장 등) computed 직렬화로 폴백.
fn decls_line(rule: &crate::css::Rule) -> String {
    rule.declarations
        .iter()
        .map(|d| {
            let imp = if d.important { " !important" } else { "" };
            let v = if d.raw.is_empty() {
                crate::style::computed_value_string(&d.value)
            } else {
                d.raw.clone()
            };
            format!("{}: {}{};", d.name, v, imp)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// §CSSOM «serialize a CSS rule» + CSS Nesting. 재귀적으로 cssText 를 만든다.
//  - CSSStyleRule (중첩 없음): `sel { decls }` 한 줄. 비면 `sel { }`.
//  - CSSStyleRule (중첩 있음): `sel {\n  <자체 선언 한 줄>\n  <중첩 규칙들>\n}` 여러 줄.
//  - CSSMediaRule: `@media <cond> {\n  <내부 규칙들>\n}`. 내부 없으면 `@media <cond> {\n}`.
//  - CSSNestedDeclarations (selector_text 가 빈 문자열): 선언만 `decls` (셀렉터·중괄호 없음).
// 들여쓰기는 자식 cssText 앞에 "  " 를 붙이는 방식(첫 줄만 밀림 = 비누적) — 스펙 출력과 일치.
fn serialize_rule_full(rule: &crate::css::Rule) -> String {
    let dl = decls_line(rule);
    if let Some(cond) = &rule.at_media {
        let children: Vec<String> = rule.nested.iter().map(serialize_rule_full).collect();
        if children.is_empty() {
            return format!("@media {} {{\n}}", cond);
        }
        let body = children.iter().map(|c| format!("  {}", c)).collect::<Vec<_>>().join("\n");
        return format!("@media {} {{\n{}\n}}", cond, body);
    }
    if rule.selector_text.is_empty() {
        // CSSNestedDeclarations: 선언만.
        return dl;
    }
    let sel = crate::css::serialize_selector(&rule.selector_text);
    if rule.nested.is_empty() {
        return if dl.is_empty() {
            format!("{} {{ }}", sel)
        } else {
            format!("{} {{ {} }}", sel, dl)
        };
    }
    // 중첩 규칙이 있는 스타일 규칙: 여러 줄. 자체 선언(있으면) 한 줄 + 중첩 규칙들.
    let mut items: Vec<String> = Vec::new();
    if !dl.is_empty() {
        items.push(dl);
    }
    items.extend(rule.nested.iter().map(serialize_rule_full));
    let body = items.iter().map(|c| format!("  {}", c)).collect::<Vec<_>>().join("\n");
    format!("{} {{\n{}\n}}", sel, body)
}

fn materialize_rule(rule: &crate::css::Rule) -> Value {
    let css_text = serialize_rule_full(rule);
    let children: Vec<Value> = rule.nested.iter().map(materialize_rule).collect();
    let child_arr = ArrayObj::new(children);
    child_arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
    let mut m = ObjMap::new();
    m.insert("cssText".to_string(), Value::Str(css_text));
    m.insert("cssRules".to_string(), Value::Arr(child_arr));
    m.insert("length".to_string(), Value::Num(rule.nested.len() as f64));
    if let Some(cond) = &rule.at_media {
        // CSSMediaRule(type 4): conditionText/media 노출, selectorText 없음.
        m.insert("type".to_string(), Value::Num(4.0));
        m.insert("conditionText".to_string(), Value::Str(cond.clone()));
        m.insert("media".to_string(), Value::Str(cond.clone()));
    } else {
        let sel = crate::css::serialize_selector(&rule.selector_text);
        m.insert("selectorText".to_string(), Value::Str(sel));
        m.insert("type".to_string(), Value::Num(1.0));
    }
    Value::Obj(std::rc::Rc::new(std::cell::RefCell::new(m)))
}

impl Interp {
    // 시트 목록 (렌더 파이프라인이 소유). 스크립트 실행 동안에만 유효한 포인터다.
    pub(super) fn sheets(&mut self) -> Option<&mut Vec<crate::css::SheetEntry>> {
        let ctx = self.layout_ctx?;
        Some(unsafe { &mut *ctx.sheets })
    }

    // CSSOM 을 읽기 전에 시트 목록을 DOM 과 맞춘다. 스크립트가 방금 넣은 <style> 은
    // document.styleSheets 에 **즉시** 보여야 한다 (레이아웃을 기다리지 않는다).
    pub(super) fn sync_sheets(&mut self) {
        let (Some(ctx), Some(dom_ptr)) = (self.layout_ctx, self.dom) else { return };
        let dom = unsafe { &*dom_ptr };
        let sheets = unsafe { &mut *ctx.sheets };
        let base = self.base_url().map(|s| s.to_string());
        if crate::window::sync_style_sheets(dom, sheets, ctx.vw, base.as_deref()) {
            self.css_epoch += 1;
        }
    }

    // 규칙 하나를 CSS 문법으로 직렬화 (cssText)
    fn rule_css_text(&mut self, si: usize, ri: usize) -> String {
        let Some(sheets) = self.sheets() else { return String::new() };
        let Some(rule) = sheets.get(si).and_then(|s| s.sheet.rules.get(ri)) else {
            return String::new();
        };
        serialize_rule_full(rule)
    }

    // 계산 스타일의 열거 가능한 프로퍼티 이름 목록(대시 표기, 정렬).
    // CSSStyleDeclaration 의 인덱스 접근·length·item·ownKeys 가 공유한다. 단축
    // 프로퍼티(margin/background/inset 등)는 제외 — getComputedStyle 은 롱핸드만 나열한다.
    pub(super) fn computed_prop_names(&self, id: crate::dom::NodeId) -> Vec<String> {
        let Some(m) = self.computed_styles.get(&id) else {
            return Vec::new();
        };
        let sh = crate::css::shorthand_table();
        let mut names: Vec<String> = m
            .keys()
            // 단축은 계산값 열거에서 제외 — 단 text-decoration 은 Chrome 이 계산값에
            // 노출하는 단축이라 포함(재조립됨). \0-접두 내부 키도 제외.
            .filter(|k| {
                (!sh.contains_key(k.as_str())
                    || matches!(k.as_str(), "text-decoration" | "font" | "text-spacing"))
                    && !k.starts_with('\u{0}')
            })
            .cloned()
            .collect();
        names.sort();
        names
    }

    pub(super) fn cssom_get(&mut self, recv: &Value, key: &str) -> Result<Value, String> {
        match recv {
            // CSSStyleSheet
            Value::Sheet(si) => {
                let si = *si;
                match key {
                    "cssRules" | "rules" => {
                        let n = self
                            .sheets()
                            .and_then(|s| s.get(si))
                            .map(|s| s.sheet.rules.len())
                            .unwrap_or(0);
                        let list: Vec<Value> =
                            (0..n).map(|ri| Value::CssRule(si, ri)).collect();
                        let arr = ArrayObj::new(list);
                        arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                        Ok(Value::Arr(arr))
                    }
                    "href" => Ok(self
                        .sheets()
                        .and_then(|s| s.get(si))
                        .and_then(|s| s.href.clone())
                        .map(Value::Str)
                        .unwrap_or(Value::Null)),
                    "ownerNode" => Ok(self
                        .sheets()
                        .and_then(|s| s.get(si))
                        .and_then(|s| s.owner)
                        .map(Value::Dom)
                        .unwrap_or(Value::Null)),
                    "disabled" => Ok(Value::Bool(
                        self.sheets()
                            .and_then(|s| s.get(si))
                            .map(|s| s.disabled)
                            .unwrap_or(false),
                    )),
                    "type" => Ok(Value::Str("text/css".to_string())),
                    "title" | "parentStyleSheet" | "ownerRule" => Ok(Value::Null),
                    "media" => Ok(Value::Arr(ArrayObj::new(Vec::new()))),
                    "insertRule" => Ok(Value::Native(Native::SheetInsertRule)),
                    "deleteRule" | "removeRule" => Ok(Value::Native(Native::SheetDeleteRule)),
                    "replaceSync" => Ok(Value::Native(Native::SheetReplaceSync)),
                    "replace" => Ok(Value::Native(Native::SheetReplace)),
                    _ => Ok(Value::Undefined),
                }
            }
            // CSSStyleRule / CSSPropertyRule
            Value::CssRule(si, ri) => {
                let (si, ri) = (*si, *ri);
                // @property 규칙(CSSPropertyRule §Properties & Values API) 전용 속성.
                let atp = self
                    .sheets()
                    .and_then(|s| s.get(si))
                    .and_then(|s| s.sheet.rules.get(ri))
                    .and_then(|r| r.at_property.clone());
                if let Some((name, reg)) = atp {
                    return match key {
                        "name" => Ok(Value::Str(name)),
                        "syntax" => Ok(Value::Str(reg.syntax)), // 따옴표 없는 문자열
                        "inherits" => Ok(Value::Bool(reg.inherits)),
                        // initialValue 는 없으면 null(빈 문자열 아님).
                        "initialValue" => {
                            Ok(reg.initial.clone().map(Value::Str).unwrap_or(Value::Null))
                        }
                        "type" => Ok(Value::Num(0.0)), // CSSRule.type (CSSPropertyRule 은 0)
                        "cssText" => {
                            // initial-value 는 없으면 생략(§CSSOM serialize).
                            let init = match &reg.initial {
                                Some(v) => format!(" initial-value: {};", v),
                                None => String::new(),
                            };
                            Ok(Value::Str(format!(
                                "@property {} {{ syntax: \"{}\"; inherits: {};{} }}",
                                name, reg.syntax, reg.inherits, init
                            )))
                        }
                        "parentStyleSheet" => Ok(Value::Sheet(si)),
                        "parentRule" => Ok(Value::Null),
                        _ => Ok(Value::Undefined),
                    };
                }
                // @media 규칙(CSSMediaRule §CSSOM) 전용 속성.
                let atm = self
                    .sheets()
                    .and_then(|s| s.get(si))
                    .and_then(|s| s.sheet.rules.get(ri))
                    .and_then(|r| r.at_media.clone());
                if let Some(cond) = atm {
                    return match key {
                        // .media 는 MediaList — .mediaText 를 가진 객체.
                        "media" => {
                            let mut m = ObjMap::new();
                            m.insert("mediaText".to_string(), Value::Str(cond.clone()));
                            m.insert("length".to_string(), Value::Num(
                                if cond.is_empty() { 0.0 } else { cond.split(", ").count() as f64 },
                            ));
                            Ok(Value::Obj(std::rc::Rc::new(std::cell::RefCell::new(m))))
                        }
                        "conditionText" => Ok(Value::Str(cond)),
                        "type" => Ok(Value::Num(4.0)), // MEDIA_RULE
                        "cssText" => Ok(Value::Str(self.rule_css_text(si, ri))),
                        // 내부 규칙(.nested)을 CSSMediaRule.cssRules 로 노출(읽기용 materialize).
                        "cssRules" | "rules" => {
                            let children: Vec<Value> = self
                                .sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| s.sheet.rules.get(ri))
                                .map(|r| r.nested.iter().map(materialize_rule).collect())
                                .unwrap_or_default();
                            let arr = ArrayObj::new(children);
                            arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                            Ok(Value::Arr(arr))
                        }
                        "length" => Ok(Value::Num(
                            self.sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| s.sheet.rules.get(ri))
                                .map(|r| r.nested.len() as f64)
                                .unwrap_or(0.0),
                        )),
                        "insertRule" => Ok(Value::Native(Native::SheetInsertRule)),
                        "deleteRule" | "removeRule" => Ok(Value::Native(Native::SheetDeleteRule)),
                        "parentStyleSheet" => Ok(Value::Sheet(si)),
                        "parentRule" => Ok(Value::Null),
                        _ => Ok(Value::Undefined),
                    };
                }
                // @custom-media 규칙(CSSCustomMediaRule §Media Queries 5) 전용 속성.
                let atc = self
                    .sheets()
                    .and_then(|s| s.get(si))
                    .and_then(|s| s.sheet.rules.get(ri))
                    .and_then(|r| r.at_custom_media.clone());
                if let Some((name, cond)) = atc {
                    return match key {
                        "name" => Ok(Value::Str(name)),
                        // .query: true/false 는 불리언, 그 외는 MediaList(mediaText/length/item).
                        "query" => {
                            if cond.eq_ignore_ascii_case("true") {
                                return Ok(Value::Bool(true));
                            }
                            if cond.eq_ignore_ascii_case("false") {
                                return Ok(Value::Bool(false));
                            }
                            let mut m = ObjMap::new();
                            m.insert("mediaText".to_string(), Value::Str(cond.clone()));
                            m.insert("length".to_string(), Value::Num(
                                if cond.is_empty() { 0.0 } else { cond.split(", ").count() as f64 },
                            ));
                            m.insert("item".to_string(), Value::Native(Native::Noop));
                            m.insert("appendMedium".to_string(), Value::Native(Native::Noop));
                            m.insert("deleteMedium".to_string(), Value::Native(Native::Noop));
                            Ok(Value::Obj(std::rc::Rc::new(std::cell::RefCell::new(m))))
                        }
                        "type" => Ok(Value::Num(0.0)),
                        "cssText" => Ok(Value::Str(format!("@custom-media {} {};", name, cond))),
                        "parentStyleSheet" => Ok(Value::Sheet(si)),
                        "parentRule" => Ok(Value::Null),
                        _ => Ok(Value::Undefined),
                    };
                }
                match key {
                    "selectorText" => Ok(Value::Str(
                        self.sheets()
                            .and_then(|s| s.get(si))
                            .and_then(|s| {
                                let sh = &s.sheet;
                                sh.rules.get(ri).map(|r| {
                                    crate::css::serialize_selector_ns(
                                        &r.selector_text,
                                        sh.default_namespace.as_deref(),
                                        &sh.namespaces,
                                    )
                                })
                            })
                            .unwrap_or_default(),
                    )),
                    "cssText" => Ok(Value::Str(self.rule_css_text(si, ri))),
                    "style" => Ok(Value::RuleStyle(si, ri)),
                    "type" => Ok(Value::Num(1.0)), // STYLE_RULE
                    // CSS Nesting: 중첩 규칙(.nested)을 cssRules 로 노출(읽기용 materialize).
                    "cssRules" | "rules" => {
                        let children: Vec<Value> = self
                            .sheets()
                            .and_then(|s| s.get(si))
                            .and_then(|s| s.sheet.rules.get(ri))
                            .map(|r| r.nested.iter().map(materialize_rule).collect())
                            .unwrap_or_default();
                        let arr = ArrayObj::new(children);
                        arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                        Ok(Value::Arr(arr))
                    }
                    "length" => Ok(Value::Num(
                        self.sheets()
                            .and_then(|s| s.get(si))
                            .and_then(|s| s.sheet.rules.get(ri))
                            .map(|r| r.nested.len() as f64)
                            .unwrap_or(0.0),
                    )),
                    "parentStyleSheet" => Ok(Value::Sheet(si)),
                    "parentRule" => Ok(Value::Null),
                    _ => Ok(Value::Undefined),
                }
            }
            // 규칙의 CSSStyleDeclaration
            Value::RuleStyle(si, ri) => {
                let (si, ri) = (*si, *ri);
                match key {
                    "cssText" => {
                        let t = self.rule_css_text(si, ri);
                        // "sel { decls }" 에서 선언부만
                        let inner = t
                            .split_once('{')
                            .map(|(_, r)| r.trim_end_matches('}').trim().to_string())
                            .unwrap_or_default();
                        Ok(Value::Str(inner))
                    }
                    "length" => Ok(Value::Num(
                        self.sheets()
                            .and_then(|s| s.get(si))
                            .and_then(|s| s.sheet.rules.get(ri))
                            .map(|r| r.declarations.len() as f64)
                            .unwrap_or(0.0),
                    )),
                    "getPropertyValue" => Ok(Value::Native(Native::RuleStyleGet)),
                    "setProperty" => Ok(Value::Native(Native::RuleStyleSet)),
                    "removeProperty" => Ok(Value::Native(Native::RuleStyleRemove)),
                    "item" => Ok(Value::Native(Native::RuleStyleItem)),
                    "parentRule" => Ok(Value::CssRule(si, ri)),
                    _ => {
                        // 인덱스 접근: style[0] → 프로퍼티 이름 (표준)
                        if let Ok(i) = key.parse::<usize>() {
                            return Ok(self
                                .sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| s.sheet.rules.get(ri))
                                .and_then(|r| r.declarations.get(i))
                                .map(|d| Value::Str(d.name.clone()))
                                .unwrap_or(Value::Undefined));
                        }
                        // camelCase 프로퍼티 접근: style.backgroundColor
                        let prop = camel_to_dashed(key);
                        Ok(Value::Str(self.rule_prop(si, ri, &prop)))
                    }
                }
            }
            _ => Ok(Value::Undefined),
        }
    }

    pub(super) fn rule_prop(&mut self, si: usize, ri: usize, prop: &str) -> String {
        self.sheets()
            .and_then(|s| s.get(si))
            .and_then(|s| s.sheet.rules.get(ri))
            .and_then(|r| r.declarations.iter().find(|d| d.name == prop))
            .map(|d| crate::style::computed_value_string(&d.value))
            .unwrap_or_default()
    }

    // style.setProperty / style.prop = v — 규칙의 선언을 실제로 바꾼다.
    pub(super) fn rule_set_prop(&mut self, si: usize, ri: usize, prop: &str, val: &str) {
        // 값 파싱은 인라인 스타일과 같은 경로를 쓴다 (규칙이 두 벌이 되면 반드시 어긋난다)
        let parsed = crate::css::parse_inline_style(&format!("{}: {}", prop, val.trim()))
            .into_iter()
            .find(|d| d.name == prop);
        if let Some(sheets) = self.sheets() {
            if let Some(rule) = sheets.get_mut(si).and_then(|s| s.sheet.rules.get_mut(ri)) {
                rule.declarations.retain(|d| d.name != prop);
                if let Some(d) = parsed {
                    rule.declarations.push(d);
                }
            }
        }
        self.css_epoch += 1;
    }

    pub(super) fn sheet_insert_rule(&mut self, si: usize, text: &str, index: usize) -> Result<Value, String> {
        let vw = self.layout_ctx.map(|c| c.vw).unwrap_or(1000.0);
        // @media 는 파스시점 flatten 되어 규칙이 안 남는다 → CSSOM CSSMediaRule 로 컨테이너
        // 를 만들어 삽입한다(조건만 보관; 빈 selectors 라 cascade 는 자연히 건너뛴다).
        let trimmed = text.trim_start();
        if trimmed.len() >= 6 && trimmed[..6].eq_ignore_ascii_case("@media") {
            // `@media <query> { <내부 규칙들> }` → CSSMediaRule 컨테이너. 내부 규칙은
            // 첫 '{' 와 짝 '}' 사이를 파싱해 .nested 에 계층으로 보관(매칭은 flatten 이
            // at_media 컨테이너를 건너뛰므로 무영향; CSSOM 직렬화·cssRules 만 노출).
            let after = &trimmed[6..];
            let (query, inner_text) = match after.find('{') {
                Some(b) => {
                    let q = after[..b].trim().to_string();
                    let rest = &after[b + 1..];
                    let inner = rest.rfind('}').map(|e| &rest[..e]).unwrap_or(rest);
                    (q, inner.to_string())
                }
                None => (after.trim().to_string(), String::new()),
            };
            let media = crate::css::serialize_media_query_list(&query);
            let nested = crate::css::parse_viewport(inner_text, vw).rules;
            let rule = crate::css::Rule {
                selectors: Vec::new(),
                declarations: Vec::new(),
                layer: None,
                container: None,
                selector_text: String::new(),
                ua: false,
                at_property: None,
                at_media: Some(media),
                at_custom_media: None,
                nested,
            };
            let Some(sheets) = self.sheets() else { return Ok(Value::Num(0.0)) };
            let Some(entry) = sheets.get_mut(si) else { return Ok(Value::Num(0.0)) };
            let idx = index.min(entry.sheet.rules.len());
            entry.sheet.rules.insert(idx, rule);
            self.css_epoch += 1;
            return Ok(Value::Num(idx as f64));
        }
        let parsed = crate::css::parse_viewport(text.to_string(), vw);
        let Some(rule) = parsed.rules.into_iter().next() else {
            return Err(self.throw_dom("SyntaxError", "규칙을 파싱할 수 없다"));
        };
        let Some(sheets) = self.sheets() else { return Ok(Value::Num(0.0)) };
        let Some(entry) = sheets.get_mut(si) else { return Ok(Value::Num(0.0)) };
        let idx = index.min(entry.sheet.rules.len());
        entry.sheet.rules.insert(idx, rule);
        self.css_epoch += 1;
        Ok(Value::Num(idx as f64))
    }

    pub(super) fn sheet_delete_rule(&mut self, si: usize, index: usize) -> Result<Value, String> {
        let ok = {
            let Some(sheets) = self.sheets() else { return Ok(Value::Undefined) };
            match sheets.get_mut(si) {
                Some(e) if index < e.sheet.rules.len() => {
                    e.sheet.rules.remove(index);
                    true
                }
                _ => false,
            }
        };
        if !ok {
            return Err(self.throw_dom("IndexSizeError", "규칙 인덱스가 범위를 벗어남"));
        }
        self.css_epoch += 1;
        Ok(Value::Undefined)
    }
}
