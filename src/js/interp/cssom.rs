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
// 빈 CSSNestedDeclarations(선언 없는 맨선언 규칙)는 직렬화에서 생략한다(§CSSOM
// serialize a CSS rule). cssRules 목록엔 여전히 존재하지만 cssText 엔 안 나온다.
fn is_empty_nested_decls(r: &crate::css::Rule) -> bool {
    r.selector_text.is_empty()
        && r.at_media.is_none()
        && r.at_supports.is_none()
        && r.at_property.is_none()
        && r.at_custom_media.is_none()
        && r.declarations.is_empty()
}

fn serialize_rule_full(rule: &crate::css::Rule) -> String {
    let dl = decls_line(rule);
    // @media / @supports / @container 그룹 규칙: `@<kw> <cond> {\n  <내부>\n}`.
    if let Some((kw, cond)) = rule
        .at_media
        .as_ref()
        .map(|c| ("@media", c))
        .or_else(|| rule.at_supports.as_ref().map(|c| ("@supports", c)))
        .or_else(|| rule.at_container.as_ref().map(|c| ("@container", c)))
    {
        let children: Vec<String> = rule
            .nested
            .iter()
            .filter(|r| !is_empty_nested_decls(r))
            .map(serialize_rule_full)
            .collect();
        if children.is_empty() {
            return format!("{} {} {{\n}}", kw, cond);
        }
        let body = children.iter().map(|c| format!("  {}", c)).collect::<Vec<_>>().join("\n");
        return format!("{} {} {{\n{}\n}}", kw, cond, body);
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
    items.extend(
        rule.nested
            .iter()
            .filter(|r| !is_empty_nested_decls(r))
            .map(serialize_rule_full),
    );
    let body = items.iter().map(|c| format!("  {}", c)).collect::<Vec<_>>().join("\n");
    format!("{} {{\n{}\n}}", sel, body)
}

// 중첩경로(np)를 따라 최상위 규칙에서 실제 규칙으로 내려간다. np 가 비면 top 자신 →
// 기존 최상위 주소지정과 동일(안전 가법). 각 인덱스는 .nested 의 위치.
pub(super) fn resolve_nested<'a>(top: Option<&'a crate::css::Rule>, np: &[usize]) -> Option<&'a crate::css::Rule> {
    let mut cur = top?;
    for &i in np {
        cur = cur.nested.get(i)?;
    }
    Some(cur)
}

pub(super) fn resolve_nested_mut<'a>(
    top: Option<&'a mut crate::css::Rule>,
    np: &[usize],
) -> Option<&'a mut crate::css::Rule> {
    let mut cur = top?;
    for &i in np {
        cur = cur.nested.get_mut(i)?;
    }
    Some(cur)
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
    } else if let Some(cond) = &rule.at_supports {
        // CSSSupportsRule(type 12): conditionText 노출, selectorText 없음.
        m.insert("type".to_string(), Value::Num(12.0));
        m.insert("conditionText".to_string(), Value::Str(cond.clone()));
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

    // 규칙 하나를 CSS 문법으로 직렬화 (cssText). np 로 중첩 규칙 주소지정(비면 최상위).
    fn rule_css_text(&mut self, si: usize, ri: usize, np: &[usize]) -> String {
        let Some(sheets) = self.sheets() else { return String::new() };
        let Some(rule) = resolve_nested(sheets.get(si).and_then(|s| s.sheet.rules.get(ri)), np)
        else {
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
                        let list: Vec<Value> = (0..n)
                            .map(|ri| Value::CssRule(si, ri, std::rc::Rc::new(Vec::new())))
                            .collect();
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
            // CSSStyleRule / CSSPropertyRule (np 비면 최상위, 아니면 중첩 규칙 주소지정)
            Value::CssRule(si, ri, np) => {
                // Rc 내부 Vec 를 로컬로 한 번 복제 → 이하 코드는 &[usize] 로 다룬다.
                let (si, ri, np) = (*si, *ri, (**np).clone());
                // @property 규칙(CSSPropertyRule §Properties & Values API) 전용 속성.
                let atp = self
                    .sheets()
                    .and_then(|s| s.get(si))
                    .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
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
                    .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
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
                        "cssText" => Ok(Value::Str(self.rule_css_text(si, ri, &np))),
                        // 내부 규칙(.nested)을 CSSMediaRule.cssRules 로 노출(읽기용 materialize).
                        "cssRules" | "rules" => {
                            let cnt = self
                                .sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
                                .map(|r| r.nested.len())
                                .unwrap_or(0);
                            // 라이브 CssRule(np+[i]) — 내부 규칙 mutation(.style=·insertRule)이
                            // 실제 규칙에 반영되도록(materialize 읽기용 객체는 반영 안 됨).
                            let children: Vec<Value> = (0..cnt)
                                .map(|i| {
                                    let mut p = np.clone();
                                    p.push(i);
                                    Value::CssRule(si, ri, std::rc::Rc::new(p))
                                })
                                .collect();
                            let arr = ArrayObj::new(children);
                            arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                            Ok(Value::Arr(arr))
                        }
                        "length" => Ok(Value::Num(
                            self.sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
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
                // @supports 규칙(CSSSupportsRule §CSSOM) 전용 속성. at_media 와 동일한
                // 컨테이너 모델(조건은 conditionText, 내부 규칙은 .nested → cssRules).
                let ats = self
                    .sheets()
                    .and_then(|s| s.get(si))
                    .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
                    .and_then(|r| r.at_supports.clone());
                if let Some(cond) = ats {
                    return match key {
                        "conditionText" => Ok(Value::Str(cond)),
                        "type" => Ok(Value::Num(12.0)), // SUPPORTS_RULE
                        "cssText" => Ok(Value::Str(self.rule_css_text(si, ri, &np))),
                        "cssRules" | "rules" => {
                            let cnt = self
                                .sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
                                .map(|r| r.nested.len())
                                .unwrap_or(0);
                            // 라이브 CssRule(np+[i]) — 내부 규칙 mutation(.style=·insertRule)이
                            // 실제 규칙에 반영되도록(materialize 읽기용 객체는 반영 안 됨).
                            let children: Vec<Value> = (0..cnt)
                                .map(|i| {
                                    let mut p = np.clone();
                                    p.push(i);
                                    Value::CssRule(si, ri, std::rc::Rc::new(p))
                                })
                                .collect();
                            let arr = ArrayObj::new(children);
                            arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                            Ok(Value::Arr(arr))
                        }
                        "length" => Ok(Value::Num(
                            self.sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
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
                // @container 규칙(CSSContainerRule §CSS Containment). head = `[name] (query)`.
                let atcont = self
                    .sheets()
                    .and_then(|s| s.get(si))
                    .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
                    .and_then(|r| r.at_container.clone());
                if let Some(head) = atcont {
                    let (cname, cquery) = match head.find('(') {
                        Some(i) => (head[..i].trim().to_string(), head[i..].trim().to_string()),
                        None => (head.trim().to_string(), String::new()),
                    };
                    return match key {
                        "conditionText" | "containerQuery" => Ok(Value::Str(cquery)),
                        "containerName" => Ok(Value::Str(cname)),
                        "type" => Ok(Value::Num(0.0)), // 신규 규칙은 0
                        "cssText" => Ok(Value::Str(self.rule_css_text(si, ri, &np))),
                        "cssRules" | "rules" => {
                            let cnt = self
                                .sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
                                .map(|r| r.nested.len())
                                .unwrap_or(0);
                            let children: Vec<Value> = (0..cnt)
                                .map(|i| {
                                    let mut p = np.clone();
                                    p.push(i);
                                    Value::CssRule(si, ri, std::rc::Rc::new(p))
                                })
                                .collect();
                            let arr = ArrayObj::new(children);
                            arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                            Ok(Value::Arr(arr))
                        }
                        "length" => Ok(Value::Num(
                            self.sheets()
                                .and_then(|s| s.get(si))
                                .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
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
                    .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
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
                                resolve_nested(sh.rules.get(ri), &np).map(|r| {
                                    crate::css::serialize_selector_ns(
                                        &r.selector_text,
                                        sh.default_namespace.as_deref(),
                                        &sh.namespaces,
                                    )
                                })
                            })
                            .unwrap_or_default(),
                    )),
                    "cssText" => Ok(Value::Str(self.rule_css_text(si, ri, &np))),
                    // 규칙의 .style(CSSStyleDeclaration). np 로 중첩 규칙까지 주소지정.
                    "style" => Ok(Value::RuleStyle(si, ri, std::rc::Rc::new(np.clone()))),
                    "type" => Ok(Value::Num(1.0)), // STYLE_RULE
                    // CSS Nesting: 중첩 규칙(.nested)을 cssRules 로 라이브 노출(np+[i] 주소).
                    "cssRules" | "rules" => {
                        let cnt = self
                            .sheets()
                            .and_then(|s| s.get(si))
                            .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
                            .map(|r| r.nested.len())
                            .unwrap_or(0);
                        let list: Vec<Value> = (0..cnt)
                            .map(|i| {
                                let mut p = np.clone();
                                p.push(i);
                                Value::CssRule(si, ri, std::rc::Rc::new(p))
                            })
                            .collect();
                        let arr = ArrayObj::new(list);
                        arr.set_prop("item".to_string(), Value::Native(Native::ListItem));
                        Ok(Value::Arr(arr))
                    }
                    "length" => Ok(Value::Num(
                        self.sheets()
                            .and_then(|s| s.get(si))
                            .and_then(|s| resolve_nested(s.sheet.rules.get(ri), &np))
                            .map(|r| r.nested.len() as f64)
                            .unwrap_or(0.0),
                    )),
                    "parentStyleSheet" => Ok(Value::Sheet(si)),
                    // 중첩 규칙의 parentRule 은 상위 규칙(np 마지막 인덱스 제거). 최상위는 null.
                    "parentRule" => Ok(if np.is_empty() {
                        Value::Null
                    } else {
                        Value::CssRule(si, ri, std::rc::Rc::new(np[..np.len() - 1].to_vec()))
                    }),
                    // CSSStyleRule 은 CSSGroupingRule(§CSS Nesting) → insert/deleteRule 로
                    // 중첩 규칙(.nested)을 조작한다(수신자가 CssRule 이면 네이티브가 np 처리).
                    "insertRule" => Ok(Value::Native(Native::SheetInsertRule)),
                    "deleteRule" | "removeRule" => Ok(Value::Native(Native::SheetDeleteRule)),
                    _ => Ok(Value::Undefined),
                }
            }
            // 규칙의 CSSStyleDeclaration
            Value::RuleStyle(si, ri, np) => {
                let (si, ri, np) = (*si, *ri, (**np).clone());
                match key {
                    "cssText" => {
                        let t = self.rule_css_text(si, ri, &np);
                        // "sel { decls }" 에서 선언부만
                        let inner = t
                            .split_once('{')
                            .map(|(_, r)| r.trim_end_matches('}').trim().to_string())
                            .unwrap_or_default();
                        Ok(Value::Str(inner))
                    }
                    "length" => Ok(Value::Num(
                        resolve_nested(
                            self.sheets().and_then(|s| s.get(si)).and_then(|s| s.sheet.rules.get(ri)),
                            &np,
                        )
                        .map(|r| r.declarations.len() as f64)
                        .unwrap_or(0.0),
                    )),
                    "getPropertyValue" => Ok(Value::Native(Native::RuleStyleGet)),
                    "setProperty" => Ok(Value::Native(Native::RuleStyleSet)),
                    "removeProperty" => Ok(Value::Native(Native::RuleStyleRemove)),
                    "item" => Ok(Value::Native(Native::RuleStyleItem)),
                    "parentRule" => Ok(Value::CssRule(si, ri, std::rc::Rc::new(np.clone()))),
                    _ => {
                        // 인덱스 접근: style[0] → 프로퍼티 이름 (표준)
                        if let Ok(i) = key.parse::<usize>() {
                            return Ok(resolve_nested(
                                self.sheets().and_then(|s| s.get(si)).and_then(|s| s.sheet.rules.get(ri)),
                                &np,
                            )
                            .and_then(|r| r.declarations.get(i))
                            .map(|d| Value::Str(d.name.clone()))
                            .unwrap_or(Value::Undefined));
                        }
                        // camelCase 프로퍼티 접근: style.backgroundColor
                        let prop = camel_to_dashed(key);
                        Ok(Value::Str(self.rule_prop(si, ri, &np, &prop)))
                    }
                }
            }
            _ => Ok(Value::Undefined),
        }
    }

    pub(super) fn rule_prop(&mut self, si: usize, ri: usize, np: &[usize], prop: &str) -> String {
        resolve_nested(
            self.sheets().and_then(|s| s.get(si)).and_then(|s| s.sheet.rules.get(ri)),
            np,
        )
        .and_then(|r| r.declarations.iter().find(|d| d.name == prop))
        .map(|d| crate::style::computed_value_string(&d.value))
        .unwrap_or_default()
    }

    // style.setProperty / style.prop = v — 규칙(np 로 중첩 주소지정)의 선언을 실제로 바꾼다.
    pub(super) fn rule_set_prop(&mut self, si: usize, ri: usize, np: &[usize], prop: &str, val: &str) {
        // 값 파싱은 인라인 스타일과 같은 경로를 쓴다 (규칙이 두 벌이 되면 반드시 어긋난다)
        let parsed = crate::css::parse_inline_style(&format!("{}: {}", prop, val.trim()))
            .into_iter()
            .find(|d| d.name == prop);
        if let Some(sheets) = self.sheets() {
            if let Some(rule) = resolve_nested_mut(
                sheets.get_mut(si).and_then(|s| s.sheet.rules.get_mut(ri)),
                np,
            ) {
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
        // @media/@supports 는 파스시점 flatten 되어 규칙이 안 남는다 → CSSOM CSSMediaRule/
        // CSSSupportsRule 컨테이너를 만들어 삽입한다(조건만 보관; 빈 selectors 라 cascade 는
        // 자연히 건너뛴다). 내부 규칙은 첫 '{' 와 짝 '}' 사이를 파싱해 .nested 에 보관.
        let trimmed = text.trim_start();
        let low = trimmed.to_ascii_lowercase();
        let grouping = if low.starts_with("@media") {
            Some(("@media", 6))
        } else if low.starts_with("@supports") {
            Some(("@supports", 9))
        } else {
            None
        };
        if let Some((kw, kwlen)) = grouping {
            let after = &trimmed[kwlen..];
            let (cond_raw, inner_text) = match after.find('{') {
                Some(b) => {
                    let c = after[..b].trim().to_string();
                    let rest = &after[b + 1..];
                    let inner = rest.rfind('}').map(|e| &rest[..e]).unwrap_or(rest);
                    (c, inner.to_string())
                }
                None => (after.trim().to_string(), String::new()),
            };
            let cond = if kw == "@media" {
                crate::css::serialize_media_query_list(&cond_raw)
            } else {
                cond_raw
            };
            let nested = crate::css::parse_viewport(inner_text, vw).rules;
            let (at_media, at_supports) = if kw == "@media" {
                (Some(cond), None)
            } else {
                (None, Some(cond))
            };
            let rule = crate::css::Rule {
                selectors: Vec::new(),
                declarations: Vec::new(),
                layer: None,
                container: None,
                selector_text: String::new(),
                ua: false,
                at_property: None,
                at_media,
                at_custom_media: None,
                at_supports,
                at_container: None,
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

    // CSSGroupingRule.insertRule — 규칙(np 로 주소지정)의 .nested 에 파싱한 규칙을 삽입.
    // 부모 selector_text 기준 desugar. 인덱스 초과 → IndexSizeError, 무효 규칙 → SyntaxError.
    pub(super) fn rule_insert_nested(
        &mut self,
        si: usize,
        ri: usize,
        np: &[usize],
        text: &str,
        index: usize,
    ) -> Result<Value, String> {
        let vw = self.layout_ctx.map(|c| c.vw).unwrap_or(1000.0);
        let (parent_sel, len, is_group) = {
            let Some(sheets) = self.sheets() else { return Ok(Value::Num(0.0)) };
            match resolve_nested(sheets.get(si).and_then(|s| s.sheet.rules.get(ri)), np) {
                Some(r) => (
                    r.selector_text.clone(),
                    r.nested.len(),
                    r.at_media.is_some() || r.at_supports.is_some(),
                ),
                None => return Ok(Value::Num(0.0)),
            }
        };
        if index > len {
            return Err(self.throw_dom("IndexSizeError", "규칙 인덱스가 범위를 벗어남"));
        }
        // 시트 전용 at-rule(@import/@charset/@namespace)은 그룹 규칙 안에 넣을 수 없다 →
        // HierarchyRequestError(§CSSOM insertRule).
        let low = text.trim_start().to_ascii_lowercase();
        if low.starts_with("@import") || low.starts_with("@charset") || low.starts_with("@namespace") {
            return Err(self.throw_dom("HierarchyRequestError", "이 규칙은 그룹 규칙 안에 넣을 수 없다"));
        }
        // 대상이 중첩 선언(CSSNestedDeclarations)을 담을 수 있는 문맥인가(§CSS Nesting):
        // 스타일 규칙(selector 있음)이거나, 스타일 규칙 안에 중첩된 그룹 규칙(np 비지 않음).
        // 최상위 @media/@supports 나 시트에는 맨선언을 넣을 수 없다.
        let nesting_context = if is_group { !np.is_empty() } else { !parent_sel.is_empty() };
        let Some(newrule) = crate::css::parse_one_nested_rule(text, &parent_sel, vw) else {
            return Err(self.throw_dom("SyntaxError", "규칙을 파싱할 수 없다"));
        };
        // 파싱 결과가 맨선언(CSSNestedDeclarations)이면: 문맥이 아니거나 선언이 비면(빈 블록·
        // 전부 무효) SyntaxError.
        let is_bare_decls =
            newrule.selector_text.is_empty() && newrule.at_media.is_none() && newrule.at_supports.is_none();
        if is_bare_decls && (!nesting_context || newrule.declarations.is_empty()) {
            return Err(self.throw_dom("SyntaxError", "이 문맥에 맨선언을 넣을 수 없다"));
        }
        let Some(sheets) = self.sheets() else { return Ok(Value::Num(0.0)) };
        if let Some(r) = resolve_nested_mut(
            sheets.get_mut(si).and_then(|s| s.sheet.rules.get_mut(ri)),
            np,
        ) {
            let idx = index.min(r.nested.len());
            r.nested.insert(idx, newrule);
        }
        self.css_epoch += 1;
        Ok(Value::Num(index as f64))
    }

    // CSSGroupingRule.deleteRule — 규칙(np)의 .nested 에서 index 규칙 제거. 초과 → IndexSizeError.
    pub(super) fn rule_delete_nested(
        &mut self,
        si: usize,
        ri: usize,
        np: &[usize],
        index: usize,
    ) -> Result<Value, String> {
        let ok = {
            let Some(sheets) = self.sheets() else { return Ok(Value::Undefined) };
            match resolve_nested_mut(
                sheets.get_mut(si).and_then(|s| s.sheet.rules.get_mut(ri)),
                np,
            ) {
                Some(r) if index < r.nested.len() => {
                    r.nested.remove(index);
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
