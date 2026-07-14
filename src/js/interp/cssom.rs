// CSSOM (§CSSOM): document.styleSheets / CSSStyleSheet / CSSStyleRule /
// CSSStyleDeclaration. 모두 파서가 만든 시트 목록에 대한 **살아 있는 뷰**다 —
// 스냅샷을 복사하면 insertRule 이 화면에 반영되지 않는다.
use super::*;

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
        if crate::window::sync_style_sheets(dom, sheets, ctx.vw) {
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
                    _ => Ok(Value::Undefined),
                }
            }
            // CSSStyleRule
            Value::CssRule(si, ri) => {
                let (si, ri) = (*si, *ri);
                match key {
                    "selectorText" => Ok(Value::Str(
                        self.sheets()
                            .and_then(|s| s.get(si))
                            .and_then(|s| s.sheet.rules.get(ri))
                            .map(|r| r.selector_text.clone())
                            .unwrap_or_default(),
                    )),
                    "cssText" => Ok(Value::Str(self.rule_css_text(si, ri))),
                    "style" => Ok(Value::RuleStyle(si, ri)),
                    "type" => Ok(Value::Num(1.0)), // STYLE_RULE
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
