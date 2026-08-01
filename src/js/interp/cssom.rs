// CSSOM (§CSSOM): document.styleSheets / CSSStyleSheet / CSSStyleRule /
// CSSStyleDeclaration. 모두 파서가 만든 시트 목록에 대한 **살아 있는 뷰**다 —
// 스냅샷을 복사하면 insertRule 이 화면에 반영되지 않는다.
use super::*;

// CSS Nesting 중첩 규칙(.nested)을 읽기용 CSSOM 객체로 materialize(재귀). selectorText·
// cssText·type·cssRules 를 노출한다(라이브 mutation·instanceof 은 별도 addressing 필요).
fn materialize_rule(rule: &crate::css::Rule) -> Value {
    let sel = crate::css::serialize_selector(&rule.selector_text);
    let decls: Vec<String> = rule
        .declarations
        .iter()
        .map(|d| {
            let imp = if d.important { " !important" } else { "" };
            format!("{}: {}{};", d.name, crate::style::computed_value_string(&d.value), imp)
        })
        .collect();
    let inner = decls.join(" ");
    let css_text = if inner.is_empty() {
        format!("{} {{ }}", sel)
    } else {
        format!("{} {{ {} }}", sel, inner)
    };
    let children: Vec<Value> = rule.nested.iter().map(materialize_rule).collect();
    let child_arr = ArrayObj::new(children);
    child_arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
    let mut m = ObjMap::new();
    m.insert("selectorText".to_string(), Value::Str(sel));
    m.insert("cssText".to_string(), Value::Str(css_text));
    m.insert("type".to_string(), Value::Num(1.0));
    m.insert("cssRules".to_string(), Value::Arr(child_arr));
    m.insert("length".to_string(), Value::Num(rule.nested.len() as f64));
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
        let decls: Vec<String> = rule
            .declarations
            .iter()
            .map(|d| {
                let imp = if d.important { " !important" } else { "" };
                format!(
                    "{}: {}{};",
                    d.name,
                    crate::style::computed_value_string(&d.value),
                    imp
                )
            })
            .collect();
        if decls.is_empty() {
            format!("{} {{ }}", rule.selector_text)
        } else {
            format!("{} {{ {} }}", rule.selector_text, decls.join(" "))
        }
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
                        "cssText" => Ok(Value::Str(format!("@media {} {{ }}", cond))),
                        "cssRules" => {
                            let arr = ArrayObj::new(Vec::new());
                            arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                            Ok(Value::Arr(arr))
                        }
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
            let query = trimmed[6..].split('{').next().unwrap_or("").trim().to_string();
            let media = crate::css::serialize_media_query_list(&query);
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
                nested: Vec::new(),
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
