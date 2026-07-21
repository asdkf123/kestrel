// DOM 바인딩 메서드(dom_get/dom_set/query 등). interp/mod.rs 에서 분리.
use super::value::*;
use super::*;

// Node 인터페이스의 상수(§Node): nodeType 값 + compareDocumentPosition 비트마스크.
// 전역 Node 와 모든 노드 인스턴스가 공유한다(Node.prototype 상속).
pub(super) fn node_constant(key: &str) -> Option<f64> {
    Some(match key {
        "ELEMENT_NODE" => 1.0,
        "ATTRIBUTE_NODE" => 2.0,
        "TEXT_NODE" => 3.0,
        "CDATA_SECTION_NODE" => 4.0,
        "ENTITY_REFERENCE_NODE" => 5.0,
        "ENTITY_NODE" => 6.0,
        "PROCESSING_INSTRUCTION_NODE" => 7.0,
        "COMMENT_NODE" => 8.0,
        "DOCUMENT_NODE" => 9.0,
        "DOCUMENT_TYPE_NODE" => 10.0,
        "DOCUMENT_FRAGMENT_NODE" => 11.0,
        "NOTATION_NODE" => 12.0,
        "DOCUMENT_POSITION_DISCONNECTED" => 1.0,
        "DOCUMENT_POSITION_PRECEDING" => 2.0,
        "DOCUMENT_POSITION_FOLLOWING" => 4.0,
        "DOCUMENT_POSITION_CONTAINS" => 8.0,
        "DOCUMENT_POSITION_CONTAINED_BY" => 16.0,
        "DOCUMENT_POSITION_IMPLEMENTATION_SPECIFIC" => 32.0,
        _ => return None,
    })
}

impl Interp {
    pub(super) fn dom_arena(&mut self) -> Result<&mut crate::dom::Dom, String> {
        match self.dom {
            // 안전성: run_scripts/dispatch 가 실행 동안에만 유효한 포인터를 설정/해제한다.
            Some(p) => unsafe { Ok(&mut *p) },
            None => Err("document 를 사용할 수 없음".to_string()),
        }
    }

    // §4.2.3 pre-insertion validity 핵심 검사. 위반이면 DOMException 을 던진다.
    //  - node 가 parent 의 inclusive ancestor(자기 자신 포함)면 순환 → HierarchyRequestError.
    //  - reference(있으면)의 부모가 parent 가 아니면 → NotFoundError.
    // insertBefore/appendChild 가 공유. (문서 자식 제약 등 나머지는 후속.)
    pub(super) fn ensure_pre_insert_valid(
        &mut self,
        parent: crate::dom::NodeId,
        node: crate::dom::NodeId,
        reference: Option<crate::dom::NodeId>,
    ) -> Result<(), String> {
        let bad: Option<(&'static str, &'static str)> = {
            let dom = self.dom_arena()?;
            // §pre-insert 유효성 1단계: parent 는 Document/DocumentFragment/Element 여야
            // 한다. Text/Comment/PI/DocumentType(잎 노드)엔 자식을 넣을 수 없다.
            if !matches!(dom.get(parent).node_type, crate::dom::NodeType::Element(_)) {
                Some(("HierarchyRequestError", "The parent node cannot have children"))
            } else if node == parent || dom.ancestors(parent).contains(&node) {
                Some(("HierarchyRequestError", "The new child is an ancestor of the parent"))
            } else if let Some(r) = reference {
                if dom.get(r).parent != Some(parent) {
                    Some((
                        "NotFoundError",
                        "The node before which the new node is to be inserted is not a child of this node",
                    ))
                } else {
                    None
                }
            } else {
                None
            }
        };
        if let Some((name, msg)) = bad {
            return Err(self.throw_dom(name, msg));
        }
        Ok(())
    }

    pub(super) fn dom_get_element_by_id(&mut self, args: Vec<Value>) -> Result<Value, String> {
        let id = args.first().map(to_display).unwrap_or_default();
        let dom = self.dom_arena()?;
        match dom.find_by_attr_id(&id) {
            Some(node_id) => Ok(Value::Dom(node_id)),
            None => Ok(Value::Null),
        }
    }

    // CSS 선택자로 문서/서브트리 검색 (문서 순서 DFS). 미지원 선택자는 관용:
    // querySelector → null, querySelectorAll → 빈 배열.
    pub(super) fn dom_query(
        &mut self,
        scope: Option<crate::dom::NodeId>,
        sel_src: &str,
        all: bool,
    ) -> Result<Value, String> {
        let selectors = crate::css::parse_selector_list(sel_src);
        let dom = self.dom_arena()?;
        let mut out: Vec<Value> = Vec::new();
        if let Some(selectors) = selectors {
            fn rec(
                dom: &crate::dom::Dom,
                id: crate::dom::NodeId,
                selectors: &[crate::css::Selector],
                out: &mut Vec<Value>,
                all: bool,
            ) -> bool {
                if crate::style::element_matches(dom, id, selectors) {
                    out.push(Value::Dom(id));
                    if !all {
                        return true; // 첫 매칭에서 중단
                    }
                }
                dom.get(id).children.iter().any(|&c| rec(dom, c, selectors, out, all))
            }
            match scope {
                // 요소 스코프: 자손만 (자신 제외)
                Some(el) => {
                    let children = dom.get(el).children.clone();
                    children.iter().any(|&c| rec(dom, c, &selectors, &mut out, all));
                }
                None => {
                    rec(dom, dom.root, &selectors, &mut out, all);
                }
            }
        }
        if all {
            Ok(Value::Arr(ArrayObj::new(out)))
        } else {
            Ok(out.into_iter().next().unwrap_or(Value::Null))
        }
    }

    // 요소의 inline style 속성 문자열을 읽는다
    pub(super) fn style_attr(&mut self, id: crate::dom::NodeId) -> String {
        if let Ok(dom) = self.dom_arena() {
            if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
                return e.attributes.get("style").cloned().unwrap_or_default();
            }
        }
        String::new()
    }

    // element.style.cssText 게터 — 선언 블록을 CSSOM 규칙대로 직렬화한다
    // (§CSSOM "serialize a CSS declaration block"): 각 선언을 "property: value;" 로
    // 만들어 공백으로 잇는다(각 선언 끝에 세미콜론 — 마지막도 포함). 프로퍼티 이름은
    // 소문자, 값은 serialize_decl 로 정규화. 예전엔 style 속성 원문을 그대로 줘서
    // 끝 세미콜론이 없고("color: red") 값도 정규화되지 않았다.
    pub(super) fn css_text(&mut self, id: crate::dom::NodeId) -> String {
        let attr = self.style_attr(id);
        let mut out: Vec<String> = Vec::new();
        for (prop, raw) in style_pairs(&attr) {
            let name = prop.to_ascii_lowercase();
            let val = Self::serialize_decl(&name, &raw);
            if val.is_empty() {
                continue;
            }
            out.push(format!("{}: {};", name, val));
        }
        out.join(" ")
    }

    pub(super) fn set_style_attr(&mut self, id: crate::dom::NodeId, value: String) {
        if let Ok(dom) = self.dom_arena() {
            if value.is_empty() {
                dom.remove_attr(id, "style");
            } else {
                dom.set_attr(id, "style", value);
            }
        }
    }

    // style.prop 읽기 (prop 은 CSS 케밥 이름)
    // el.style.prop 읽기. CSSOM 은 **정규 형태로 직렬화한 값**을 준다 (§6.7):
    //   style="background-color: black"  →  el.style.backgroundColor === "rgb(0, 0, 0)"
    // 예전엔 style 속성의 원문을 그대로 돌려줬다. 값을 되읽어 비교하는 코드가
    // 조용히 틀렸다 (setProperty 로 쓴 값은 정규화하면서 읽기만 원문이라 앞뒤가
    // 맞지 않기까지 했다).
    pub(super) fn style_get(&mut self, id: crate::dom::NodeId, prop: &str) -> String {
        let prop = canonical_css_name(prop);
        let raw = self.style_get_raw(id, prop);
        if raw.is_empty() {
            return raw;
        }
        Self::serialize_decl(prop, &raw)
    }

    // 인라인 지정값을 정규화(serialize_decl) 없이 원문 그대로. cubic-bezier 인자
    // 정밀도 등 직렬화가 접는 정보가 필요할 때(전이 easing 캡처).
    pub(super) fn style_get_raw(&mut self, id: crate::dom::NodeId, prop: &str) -> String {
        let prop = canonical_css_name(prop);
        let attr = self.style_attr(id);
        // 뒤 선언 우선. 롱핸드가 직접 없으면 style 속성에 든 단축을 펼쳐 그 롱핸드를
        // 읽는다(§CSSOM: style="place-items: normal left" → el.style.alignItems 은 normal).
        let pairs = style_pairs(&attr);
        for (k, v) in pairs.iter().rev() {
            if k == prop {
                return v.clone();
            }
            let expanded = crate::css::expand_decl_pub(k, v);
            if expanded.iter().any(|d| d.name != *k) {
                if let Some(d) = expanded.iter().find(|d| d.name == prop) {
                    return crate::style::computed_value_string(&d.value);
                }
            }
        }
        // gap 단축은 롱핸드(row-gap/column-gap)에서 재구성(§CSSOM). 직렬화는 style_get 이.
        if matches!(prop, "gap" | "grid-gap") {
            let get = |n: &str| pairs.iter().rev().find(|(k, _)| k == n).map(|(_, v)| v.clone());
            if let (Some(rg), Some(cg)) = (get("row-gap"), get("column-gap")) {
                return format!("{} {}", rg, cg);
            }
        }
        String::new()
    }

    // 선언 하나를 CSSOM 정규 형태로 직렬화 (§6.7).
    //
    // 인라인 스타일은 **지정값**을 직렬화한다 (계산값이 아니다):
    //   style="background-color: black"  →  el.style.backgroundColor === "black"
    //   getComputedStyle(el).backgroundColor === "rgb(0, 0, 0)"   ← 여기서만 rgb()
    // 색 키워드는 키워드로 남고, 숫자 표기(#f00 / rgb(1,2,3))만 rgb()/rgba() 로 접힌다.
    // font-weight: bold 도 700 이 아니라 bold 그대로, content: "x" 도 따옴표를 유지한다.
    // (파싱된 값으로 전부 직렬화하면 이 모두가 계산값으로 접혀 버린다 — 실제로 그랬다.)
    pub(super) fn serialize_decl(prop: &str, raw: &str) -> String {
        let raw = raw.trim();
        // image-set() 은 어느 이미지 프로퍼티에서든(background-image/content/
        // border-image-source/mask-image 등) 캐논 직렬화한다(§CSS Images 4).
        let rl = raw.to_ascii_lowercase();
        if rl.starts_with("image-set(") || rl.starts_with("-webkit-image-set(") {
            return crate::css::normalize_image_set(raw);
        }
        // color-mix 지정값 캐논 직렬화(퍼센트 위치, inner 색, 기본 50% 생략).
        if rl.starts_with("color-mix(") {
            if let Some(s) = crate::css::normalize_color_mix(raw) {
                return s;
            }
        }
        // relative-color(rgb(from ...)) 지정값 캐논: rgba→rgb, origin 키워드 소문자 등.
        if rl.contains("(from ") {
            if let Some(s) = crate::css::normalize_relative_color(raw) {
                return s;
            }
        }
        // color(<space> ...) 지정값 캐논: 채널 %→0-1, alpha 1 생략(§CSS Color 4).
        if rl.starts_with("color(") {
            if let Some(s) = crate::css::normalize_color_function(raw) {
                return s;
            }
        }
        // lab/lch/oklab/oklch 지정값 캐논(§CSS Color 4).
        if rl.starts_with("lab(") || rl.starts_with("lch(") || rl.starts_with("oklab(")
            || rl.starts_with("oklch(")
        {
            if let Some(s) = crate::css::normalize_lab_like(raw) {
                return s;
            }
        }
        // hsl/hwb 지정값: none 채널이 있으면 modern 형태로 캐논(rgb 변환 불가).
        if rl.starts_with("hsl(") || rl.starts_with("hsla(") || rl.starts_with("hwb(") {
            if let Some(s) = crate::css::normalize_hsl_hwb(raw) {
                return s;
            }
        }
        // content 의 단일 문자열 토큰은 CSSOM 문자열 직렬화 — 항상 큰따옴표로(§CSSOM
        // "serialize a string"). content: 'x' 도 "x" 가 된다.
        if prop == "content" {
            if let Some(inner) = single_css_string(raw) {
                return serialize_css_string(&inner);
            }
            // url(...) 은 url("...") 로 — URL 을 문자열로 직렬화(§CSSOM).
            if let Some(u) = single_url(raw) {
                return format!("url({})", serialize_css_string(&u));
            }
        }
        // font-family: 각 패밀리가 따옴표 문자열이고 내용이 유효한 식별자 시퀀스면
        // 따옴표를 제거한다(§CSSOM serialize a font-family). 'Lucida Grande' → Lucida Grande.
        if prop == "font-family" {
            return serialize_font_family(raw);
        }
        // font 단축: 기본값(normal, /normal) 생략하고 캐논 직렬화(§CSS Fonts §font).
        if prop == "font" {
            return crate::style::normalize_font_shorthand(raw);
        }
        // transition 단축: property 먼저, 기본값 생략 캐논 직렬화(§CSSOM).
        if prop == "transition" {
            return crate::style::normalize_transition(raw);
        }
        // text-decoration-line 캐논 순서 재정렬.
        if prop == "text-decoration-line" {
            if let Some(s) = crate::css::normalize_text_decoration_line(raw) {
                return s;
            }
        }
        // white-space 단축(§CSS Text 4): collapse+wrap→normal 등 표준 키워드로.
        if prop == "white-space" {
            if let Some(s) = crate::css::normalize_white_space(raw) {
                return s;
            }
        }
        // hyphenate-limit-chars(§CSS Text 4): 후행 중복 성분 생략(5 2 2→5 2).
        if prop == "hyphenate-limit-chars" {
            if let Some(s) = crate::css::normalize_hyphenate_limit_chars(raw) {
                return s;
            }
        }
        // text-wrap 단축(§CSS Text 4): mode || style 캐논(wrap auto→wrap).
        if prop == "text-wrap" {
            if let Some(s) = crate::css::normalize_text_wrap(raw) {
                return s;
            }
        }
        // font-size-adjust(§CSS Fonts 5): 기본 basis 생략, calc 평가.
        if prop == "font-size-adjust" {
            if let Some(s) = crate::css::normalize_font_size_adjust(raw) {
                return s;
            }
        }
        // box-shadow(§CSS Backgrounds): 그림자마다 <color> <lengths> <inset> 순 캐논.
        if prop == "box-shadow" {
            if let Some(s) = crate::css::box_shadow_canonical(raw) {
                return s;
            }
        }
        // transition/animation-timing-function(§CSS Easing): step-start→steps(1, start),
        // steps 기본 위치 생략 등 캐논 직렬화.
        if matches!(prop, "transition-timing-function" | "animation-timing-function") {
            if crate::css::timing_function_valid(raw) {
                return crate::css::timing_function_canonical(raw);
            }
        }
        // transition-property(§CSS Transitions): all 키워드만 소문자화, custom-ident 보존.
        if prop == "transition-property" && crate::css::transition_property_valid(raw) {
            return crate::css::transition_property_canonical(raw);
        }
        // text-transform(§CSS Text): [case] full-width full-size-kana 캐논 순서.
        if prop == "text-transform" && crate::css::text_transform_valid(raw) {
            return crate::css::text_transform_canonical(raw);
        }
        // text-autospace(§CSS Text 4): ideograph-alpha/numeric/punctuation/삽입 캐논.
        if prop == "text-autospace" && crate::css::text_autospace_valid(raw) {
            return crate::css::text_autospace_canonical(raw);
        }
        // display(§CSS Display 3): flow→block, 두값→레거시 단일 캐논(지정값도 동일).
        if prop == "display" && crate::css::display_valid(raw) {
            return crate::css::display_canonical(raw);
        }
        // contain(§CSS Contain): size/layout/style/paint 캐논 순서.
        if prop == "contain" && crate::css::contain_valid(raw) {
            return crate::css::contain_canonical(raw);
        }
        // inset-block/inset-inline·scroll-*-block/inline·gap 단축(§CSSOM): 0→0px, 두 값
        // 같으면 축약. gap 은 normal 도 그대로(길이 아님) 통과.
        if matches!(
            prop,
            "inset-block" | "inset-inline" | "scroll-margin-block" | "scroll-margin-inline"
                | "scroll-padding-block" | "scroll-padding-inline" | "gap" | "grid-gap"
        ) {
            return crate::css::inset_pair_canonical(raw);
        }
        // row-gap/column-gap(§CSS Box Alignment): normal | <length-percentage>. 0→0px.
        if matches!(prop, "row-gap" | "column-gap" | "grid-row-gap" | "grid-column-gap") {
            if let Some(v) = crate::css::interpret_value(raw.trim()) {
                match v {
                    crate::css::Value::Length(n, _) if n == 0.0 => return "0px".to_string(),
                    v @ (crate::css::Value::Length(..)
                    | crate::css::Value::Calc(..)
                    | crate::css::Value::MinMax(..)) => {
                        return crate::style::computed_value_string(&v);
                    }
                    _ => {}
                }
            }
        }
        // scroll-margin/scroll-padding 단축(§CSSOM): TRBL 박스 축약, 0→0px.
        if matches!(prop, "scroll-margin" | "scroll-padding") {
            return crate::css::box_canonical(raw);
        }
        // scroll-snap-type(§CSS Scroll Snap): 기본 strictness(proximity) 생략.
        if prop == "scroll-snap-type" && crate::css::scroll_snap_type_valid(raw) {
            return crate::css::scroll_snap_type_canonical(raw);
        }
        // 정렬 롱핸드(§CSS Box Alignment): first baseline → baseline 캐논.
        if matches!(
            prop,
            "align-content" | "justify-content" | "align-items" | "justify-items" | "align-self"
                | "justify-self"
        ) {
            return crate::css::alignment_canonical(raw);
        }
        // place-items/place-content/place-self(§CSS Box Alignment): align + justify 캐논,
        // 두 절반이 같으면 하나로 축약.
        if matches!(prop, "place-items" | "place-content" | "place-self") {
            let d = crate::css::expand_decl_pub(prop, raw);
            if d.len() == 2 {
                let a = crate::style::computed_value_string(&d[0].value);
                let j = crate::style::computed_value_string(&d[1].value);
                return if a == j { a } else { format!("{} {}", a, j) };
            }
        }
        // object-position/perspective-origin(§CSSOM): [수평] [수직] 캐논(1값→center 보충).
        if matches!(prop, "object-position" | "perspective-origin")
            && crate::css::position_valid(raw)
        {
            return normalize_numbers(&crate::css::position_canonical(raw));
        }
        // transform-origin(§CSSOM): [수평] [수직] [z] 캐논.
        if prop == "transform-origin" && crate::css::transform_origin_valid(raw) {
            return normalize_numbers(&crate::css::transform_origin_canonical(raw));
        }
        // background-position(§CSSOM): <bg-position># — 레이어마다 캐논(3값 포함).
        if prop == "background-position" {
            return normalize_numbers(&crate::css::bg_position_list_canonical(raw));
        }
        // mask-position(§CSS Masking): <position># — 레이어마다 캐논.
        if prop == "mask-position" {
            return normalize_numbers(&crate::css::mask_position_canonical(raw));
        }
        // contain-intrinsic-size(§CSS Sizing 4): width + height, 두 그룹 같으면 축약.
        if prop == "contain-intrinsic-size" {
            let d = crate::css::expand_decl_pub(prop, raw);
            if d.len() == 2 {
                let w = crate::style::computed_value_string(&d[0].value);
                let h = crate::style::computed_value_string(&d[1].value);
                return if w == h { w } else { format!("{} {}", w, h) };
            }
        }
        // corner-shape 계열(§CSS Borders 4): superellipse 인자 공백 정리.
        if (prop.starts_with("corner-") && prop.ends_with("-shape")) || prop == "corner-shape" {
            if crate::css::corner_shape_list_valid(raw, 4) {
                return normalize_numbers(&crate::css::corner_shape_canonical(raw));
            }
        }
        // aspect-ratio(§CSS Sizing): auto 앞, <ratio> "a / b" 캐논.
        if prop == "aspect-ratio" && crate::css::aspect_ratio_valid(raw) {
            return crate::css::aspect_ratio_canonical(raw);
        }
        // grid-template-columns/rows(§CSS Grid): 빈 [] line-names 제거.
        if (prop == "grid-template-columns" || prop == "grid-template-rows")
            && crate::css::grid_template_track_valid(raw)
        {
            return crate::css::grid_template_track_canonical(raw);
        }
        // font-variation-settings(§CSS Fonts 4): 따옴표·수치 캐논.
        if prop == "font-variation-settings" && crate::css::font_variation_settings_valid(raw) {
            return crate::css::font_variation_settings_canonical(raw);
        }
        // font-synthesis(§CSS Fonts 4): 슬롯 순서 캐논.
        if prop == "font-synthesis" && crate::css::font_synthesis_valid(raw) {
            return crate::css::font_synthesis_canonical(raw);
        }
        // columns(§CSS Multicol): [width if not auto] [count if not auto] 캐논.
        if prop == "columns" {
            if let Some((w, c)) = crate::css::columns_expand(raw) {
                return crate::css::columns_canonical(&w, &c);
            }
        }
        // column-rule(§CSS Multicol): 초기값 성분 생략 캐논.
        if prop == "column-rule" && crate::css::column_rule_valid(raw) {
            return crate::css::column_rule_canonical(raw);
        }
        // text-underline-position(§CSS Text Decor): [from-font|under] 먼저 캐논.
        if prop == "text-underline-position" && crate::css::text_underline_position_valid(raw) {
            return crate::css::text_underline_position_canonical(raw);
        }
        // max-lines(§CSS Overflow 4): 정수 먼저 캐논.
        if prop == "max-lines" && crate::css::max_lines_valid(raw) {
            return crate::css::max_lines_canonical(raw);
        }
        // list-style-type(§CSS Lists): symbols() 기본 type 생략 캐논.
        if prop == "list-style-type" && crate::css::list_style_type_valid(raw) {
            return crate::css::list_style_type_canonical(raw);
        }
        // content(§CSS Content 3): counter 기본 스타일 생략 캐논.
        if prop == "content" && crate::css::content_valid(raw) {
            return crate::css::content_canonical(raw);
        }
        // background-clip(§CSS Backgrounds 4): visual-box 를 text 앞에 캐논.
        if prop == "background-clip" && crate::css::background_clip_valid(raw) {
            return crate::css::background_clip_canonical(raw);
        }
        // text-emphasis-position(§CSS Text Decor): 기본값 right 생략 캐논.
        if prop == "text-emphasis-position" && crate::css::text_emphasis_position_valid(raw) {
            return crate::css::text_emphasis_position_canonical(raw);
        }
        // text-decoration-inset(§CSS Text Decor 4): 0→0px, 같은 값 축약.
        if prop == "text-decoration-inset" && crate::css::text_decoration_inset_valid(raw) {
            return crate::css::text_decoration_inset_canonical(raw);
        }
        // counter-increment/reset/set(§CSS Lists 3): 기본 정수 추가 캐논.
        if matches!(prop, "counter-increment" | "counter-reset" | "counter-set") {
            let allow_reversed = prop == "counter-reset";
            if crate::css::counter_list_valid(raw, allow_reversed) {
                let default_int = if prop == "counter-increment" { 1 } else { 0 };
                return crate::css::counter_list_canonical(raw, default_int);
            }
        }
        // font-variant-numeric/east-asian(§CSS Fonts 4): 그룹 순서 캐논.
        if prop == "font-variant-numeric" && crate::css::font_variant_numeric_valid(raw) {
            return crate::css::font_variant_numeric_canonical(raw);
        }
        if prop == "font-variant-east-asian" && crate::css::font_variant_east_asian_valid(raw) {
            return crate::css::font_variant_east_asian_canonical(raw);
        }
        // flex-flow(§CSS Flexbox): 기본값(row/nowrap) 생략, 방향 먼저.
        if prop == "flex-flow" {
            let low = raw.trim().to_ascii_lowercase();
            if !matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                return crate::css::flex_flow_canonical(raw);
            }
        }
        // flex 단축(§CSS Flexbox): grow shrink basis 로 재구성(1→1 1 0%, none→0 0 auto).
        if prop == "flex" {
            let low = raw.trim().to_ascii_lowercase();
            if !matches!(low.as_str(), "inherit" | "initial" | "unset" | "revert" | "revert-layer") {
                let d = crate::css::expand_decl_pub("flex", raw);
                if d.len() == 3 {
                    let get = |n: &str| {
                        d.iter()
                            .find(|x| x.name == n)
                            .map(|x| crate::style::computed_value_string(&x.value))
                            .unwrap_or_default()
                    };
                    return format!(
                        "{} {} {}",
                        get("flex-grow"),
                        get("flex-shrink"),
                        get("flex-basis")
                    );
                }
            }
        }
        // scrollbar-gutter(§CSS Overflow): stable both-edges 순서로 캐논.
        if prop == "scrollbar-gutter" {
            let low = raw.trim().to_ascii_lowercase();
            let parts: Vec<&str> = low.split_whitespace().collect();
            if parts.len() == 2 && parts.contains(&"stable") && parts.contains(&"both-edges") {
                return "stable both-edges".to_string();
            }
            return low;
        }
        // overflow 단축(§CSSOM): 두 값이 같으면 하나로 축약(visible visible→visible).
        if prop == "overflow" {
            let parts: Vec<String> =
                raw.split_whitespace().map(|s| s.to_ascii_lowercase()).collect();
            if parts.len() == 2 && parts[0] == parts[1] {
                return parts[0].clone();
            }
            return parts.join(" ");
        }
        // outline-offset·top/right/bottom/left·inset 논리 롱핸드: <length-percentage>
        // 캐논(0 → 0px). auto/CSS-wide 는 아래 일반 경로로.
        if matches!(
            prop,
            "outline-offset" | "top" | "right" | "bottom" | "left" | "inset-block-start"
                | "inset-block-end" | "inset-inline-start" | "inset-inline-end"
        ) || matches!(
            prop,
            "width" | "height" | "min-width" | "min-height" | "max-width" | "max-height"
        ) || (prop.starts_with("scroll-margin-") || prop.starts_with("scroll-padding-"))
        {
            if let Some(v) = crate::css::interpret_value(raw.trim()) {
                match v {
                    crate::css::Value::Length(n, _) if n == 0.0 => return "0px".to_string(),
                    v @ (crate::css::Value::Length(..)
                    | crate::css::Value::Calc(..)
                    | crate::css::Value::MinMax(..)) => {
                        return crate::style::computed_value_string(&v);
                    }
                    _ => {}
                }
            }
        }
        // 개별 변환 프로퍼티 지정값 정규화(§CSS Transforms 2): scale % → 수/축약,
        // rotate 각도 → 도, translate 후행 0. computed 와 같은 규칙(함수형 scale() 과
        // 달리 프로퍼티는 축약한다). scale:100 100→"100", rotate:400grad→"360deg".
        // shape-outside/clip-path 의 basic shape(circle/ellipse/inset/rect/xywh) 캐논.
        if matches!(prop, "shape-outside" | "clip-path" | "offset-path")
            && (rl.starts_with("circle(")
                || rl.starts_with("ellipse(")
                || rl.starts_with("inset(")
                || rl.starts_with("rect(")
                || rl.starts_with("xywh("))
        {
            return crate::css::normalize_shape(raw);
        }
        match prop {
            "scale" => return crate::style::normalize_scale(raw),
            "rotate" => return crate::style::normalize_rotate(raw),
            "translate" => return crate::style::normalize_translate(raw),
            "transform" | "-webkit-transform" => {
                return crate::style::normalize_transform(raw)
            }
            // origin 프로퍼티 무효값 거부(auto/과다토큰/동축중복). 유효면 원문(계산값에서
            // 박스 해석은 getComputedStyle 이 담당).
            "perspective-origin" if !crate::style::origin_valid(raw, 2) => {
                return String::new()
            }
            "transform-origin" if !crate::style::origin_valid(raw, 3) => {
                return String::new()
            }
            // background-image gradient 지정값: 보간 재정렬 + 색 정규화(키워드 유지).
            "background-image" | "-webkit-background-image"
                if raw.contains("gradient(") =>
            {
                if !crate::css::gradient_valid(raw) {
                    return String::new(); // 무효 gradient → 거부
                }
                return crate::css::normalize_gradient_serial(raw, false);
            }
            _ => {}
        }
        let parsed = crate::css::parse_inline_style(&format!("{}: {}", prop, raw))
            .into_iter()
            .find(|d| d.name == prop)
            .map(|d| d.value);
        match parsed {
            // 색: 키워드(black/transparent/currentcolor)는 그대로(소문자),
            // 그 외 표기(#f00, rgb(1,2,3))는 rgb()/rgba() 로 정규화
            Some(v @ crate::css::Value::Color(_)) => {
                if raw.chars().all(|c| c.is_ascii_alphabetic() || c == '-') {
                    raw.to_ascii_lowercase()
                } else {
                    crate::style::computed_value_string(&v)
                }
            }
            Some(v @ crate::css::Value::Url(_)) => crate::style::computed_value_string(&v),
            _ => normalize_numbers(raw),
        }
    }


    // style.prop = value 쓰기 (빈 값이면 제거)
    pub(super) fn style_set(&mut self, id: crate::dom::NodeId, prop: &str, value: &str) {
        let prop = canonical_css_name(prop);
        let text_trimmed = value.trim().to_string();
        // 검증형 단축(white-space/text-wrap/font-family/font)이 확장이 비면 무효값 —
        // CSSOM 규약상 지정 자체를 무시한다(기존 값·롱핸드 그대로 유지). 반드시 아래
        // retain/capture 전에 조기 반환해야 기존 값이 지워지지 않는다.
        if !text_trimmed.is_empty()
            && matches!(
                prop,
                "white-space"
                    | "text-wrap"
                    | "font-family"
                    | "font"
                    | "font-size-adjust"
                    | "caret-color"
                    | "box-sizing"
                    | "cursor"
                    | "field-sizing"
                    | "box-shadow"
                    | "transition-timing-function"
                    | "animation-timing-function"
                    | "transition-property"
                    | "transition-duration"
                    | "transition-delay"
                    | "transition"
                    | "font-variant"
                    | "font-style"
                    | "text-transform"
                    | "text-wrap-mode"
                    | "text-wrap-style"
                    | "word-break"
                    | "text-group-align"
                    | "hanging-punctuation"
                    | "text-autospace"
                    | "text-spacing-trim"
                    | "outline-offset"
                    | "display"
                    | "contain"
                    | "top"
                    | "right"
                    | "bottom"
                    | "left"
                    | "inset"
                    | "inset-block"
                    | "inset-inline"
                    | "inset-block-start"
                    | "inset-block-end"
                    | "inset-inline-start"
                    | "inset-inline-end"
                    | "overflow"
                    | "overflow-x"
                    | "overflow-y"
                    | "overflow-block"
                    | "overflow-inline"
                    | "scrollbar-gutter"
                    | "scroll-margin"
                    | "scroll-margin-top"
                    | "scroll-margin-right"
                    | "scroll-margin-bottom"
                    | "scroll-margin-left"
                    | "scroll-margin-block"
                    | "scroll-margin-inline"
                    | "scroll-margin-block-start"
                    | "scroll-margin-block-end"
                    | "scroll-margin-inline-start"
                    | "scroll-margin-inline-end"
                    | "scroll-padding"
                    | "scroll-padding-top"
                    | "scroll-padding-right"
                    | "scroll-padding-bottom"
                    | "scroll-padding-left"
                    | "scroll-padding-block"
                    | "scroll-padding-inline"
                    | "scroll-padding-block-start"
                    | "scroll-padding-block-end"
                    | "scroll-padding-inline-start"
                    | "scroll-padding-inline-end"
                    | "scroll-snap-type"
                    | "flex"
                    | "flex-grow"
                    | "flex-shrink"
                    | "flex-basis"
                    | "flex-flow"
                    | "flex-direction"
                    | "flex-wrap"
                    | "order"
                    | "align-content"
                    | "justify-content"
                    | "align-items"
                    | "justify-items"
                    | "align-self"
                    | "justify-self"
                    | "place-items"
                    | "place-content"
                    | "place-self"
                    | "gap"
                    | "row-gap"
                    | "column-gap"
                    | "grid-gap"
                    | "grid-row-gap"
                    | "grid-column-gap"
                    | "width"
                    | "height"
                    | "min-width"
                    | "min-height"
                    | "max-width"
                    | "max-height"
                    | "aspect-ratio"
                    | "contain-intrinsic-size"
                    | "contain-intrinsic-width"
                    | "contain-intrinsic-height"
                    | "contain-intrinsic-inline-size"
                    | "contain-intrinsic-block-size"
                    | "image-orientation"
                    | "object-position"
                    | "background-position"
                    | "mask-position"
                    | "perspective-origin"
                    | "rotate"
                    | "scale"
                    | "translate"
                    | "transform-origin"
                    | "corner-shape"
                    | "corner-top-shape"
                    | "corner-bottom-shape"
                    | "corner-left-shape"
                    | "corner-right-shape"
                    | "corner-block-shape"
                    | "corner-inline-shape"
                    | "corner-top-left-shape"
                    | "corner-top-right-shape"
                    | "corner-bottom-left-shape"
                    | "corner-bottom-right-shape"
                    | "corner-block-start-shape"
                    | "corner-block-end-shape"
                    | "corner-inline-start-shape"
                    | "corner-inline-end-shape"
                    | "grid-row"
                    | "grid-column"
                    | "grid-area"
                    | "grid-row-start"
                    | "grid-row-end"
                    | "grid-column-start"
                    | "grid-column-end"
                    | "grid-template-columns"
                    | "grid-template-rows"
                    | "grid-auto-columns"
                    | "grid-auto-rows"
                    | "font-stretch"
                    | "font-width"
                    | "font-variant-emoji"
                    | "font-variation-settings"
                    | "font-synthesis"
                    | "font-synthesis-weight"
                    | "font-synthesis-style"
                    | "font-synthesis-small-caps"
                    | "font-synthesis-position"
                    | "font-variant-numeric"
                    | "font-variant-east-asian"
                    | "font-variant-alternates"
                    | "counter-increment"
                    | "counter-reset"
                    | "counter-set"
                    | "column-count"
                    | "column-width"
                    | "column-rule-width"
                    | "columns"
                    | "column-span"
                    | "column-fill"
                    | "column-rule-style"
                    | "column-rule-color"
                    | "column-rule"
                    | "will-change"
                    | "text-decoration-line"
                    | "text-decoration-skip-ink"
                    | "text-decoration-skip-spaces"
                    | "widows"
                    | "orphans"
                    | "float"
                    | "clear"
                    | "visibility"
                    | "break-before"
                    | "break-after"
                    | "break-inside"
                    | "box-decoration-break"
                    | "text-underline-position"
                    | "text-decoration-style"
                    | "text-decoration-color"
                    | "text-emphasis-position"
                    | "text-decoration-inset"
                    | "text-overflow"
                    | "continue"
                    | "max-lines"
                    | "block-ellipsis"
                    | "-webkit-line-clamp"
                    | "position"
                    | "z-index"
                    | "list-style-position"
                    | "list-style-image"
                    | "list-style-type"
                    | "list-style"
                    | "shape-margin"
                    | "shape-image-threshold"
                    | "content"
                    | "border-radius"
                    | "border-top-left-radius"
                    | "border-top-right-radius"
                    | "border-bottom-left-radius"
                    | "border-bottom-right-radius"
                    | "background-clip"
                    | "background-origin"
                    | "background-position-x"
                    | "background-position-y"
                    | "color"
                    | "opacity"
                    | "animation-name"
                    | "animation-duration"
                    | "animation-delay"
                    | "animation-iteration-count"
                    | "animation-direction"
                    | "animation-fill-mode"
                    | "animation-play-state"
                    | "animation-range-start"
                    | "animation-range-end"
                    | "object-fit"
                    | "image-rendering"
                    | "image-resolution"
            )
            && !text_trimmed.to_ascii_lowercase().contains("var(")
            && crate::css::expand_decl_pub(prop, &text_trimmed).is_empty()
        {
            return;
        }
        // CSS Transitions: transition 걸린 프로퍼티가 바뀌면 이전 계산값→새 값 전이를
        // element_animations 에 등록(getComputedStyle 이 진행률에서 보간). additive.
        if !prop.starts_with("transition") && !text_trimmed.is_empty() {
            self.maybe_capture_transition(id, prop, &text_trimmed);
        }
        let attr = self.style_attr(id);
        let mut pairs = style_pairs(&attr);
        pairs.retain(|(k, _)| k != prop);
        if !text_trimmed.is_empty() {
            // 인라인 스타일은 **지정값**을 보관한다 (계산값으로 접지 않는다).
            // 예전엔 여기서 computed_value_string 으로 접어서 `el.style.color = "black"`
            // 이 rgb(0, 0, 0) 으로 저장됐다 — 지정값이 통째로 사라졌다.
            // 직렬화는 **읽을 때** serialize_decl 이 한 번만 한다.
            let text = text_trimmed.clone();
            // transition/animation 등 단축은 인라인 롱핸드로도 펼친다(§CSSOM: el.style
            // 로 단축을 설정하면 롱핸드가 읽힌다). 확장이 자기 자신과 다른 이름을 내면
            // 단축이다. 기존 동명 롱핸드는 교체.
            let longhands = crate::css::expand_decl_pub(prop, &text);
            {
                if longhands.iter().any(|d| d.name != prop) {
                    for d in &longhands {
                        if d.name == prop {
                            continue;
                        }
                        let lv = crate::style::computed_value_string(&d.value);
                        pairs.retain(|(k, _)| k != &d.name);
                        pairs.push((d.name.clone(), lv));
                    }
                }
                pairs.push((prop.to_string(), text));
            }
        }
        let s = style_serialize(&pairs);
        self.set_style_attr(id, s);
    }

    // transition 걸린 프로퍼티 변경 시 이전 계산값→새 값 전이를 element_animations 에
    // 등록. currentTime=-delay(경과), 진행률 = 경과/지속. 테스트가 먼저 getComputedStyle
    // 로 from 을 확정하므로 computed_styles 에서 from 을 읽는다.
    fn maybe_capture_transition(&mut self, id: crate::dom::NodeId, prop: &str, new_value: &str) {
        let time_ms = |s: &str| -> f32 {
            let s = s.trim();
            if let Some(n) = s.strip_suffix("ms") {
                n.trim().parse::<f32>().unwrap_or(0.0)
            } else if let Some(n) = s.strip_suffix('s') {
                n.trim().parse::<f32>().unwrap_or(0.0) * 1000.0
            } else {
                0.0
            }
        };
        // 다중값(최상위 쉼표)은 첫 값만. 단 cubic-bezier()/steps() 내부 쉼표는
        // 무시해야 하므로 괄호 깊이를 추적한다.
        let first = |s: String| -> String {
            let mut depth = 0i32;
            for (i, c) in s.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => return s[..i].trim().to_string(),
                    _ => {}
                }
            }
            s.trim().to_string()
        };
        let dur = time_ms(&first(self.style_get(id, "transition-duration")));
        if dur <= 0.0 {
            return;
        }
        let tprop = self.style_get(id, "transition-property");
        let tprop = tprop.trim();
        if !(tprop.is_empty()
            || tprop == "all"
            || tprop.split(',').any(|p| p.trim() == prop))
        {
            return;
        }
        let delay = time_ms(&first(self.style_get(id, "transition-delay")));
        let elapsed = -delay; // 음수 delay = 과거 시작(경과). 양수면 아직 미시작.
        if elapsed < 0.0 {
            return;
        }
        // easing 은 원문(반올림 안 된 cubic-bezier 인자)으로 읽어야 진행률이 정확하다.
        let easing = first(self.style_get_raw(id, "transition-timing-function"));
        let easing = if easing.is_empty() { "ease".to_string() } else { easing };
        // from = 덮어쓰기 전 현재 인라인 지정값(테스트가 setup 에서 el.style[p]=from 설정).
        // computed_styles 는 JS 실행 중 stale 할 수 있어 인라인을 우선. transform 은
        // 원문(raw)으로 읽어야 to(new_value, 원문)와 각도 단위(rad/deg)가 어긋나지
        // 않는다 — style_get 은 rad→deg 로 정규화한다.
        let mut from = if prop == "transform" {
            self.style_get_raw(id, prop)
        } else {
            self.style_get(id, prop)
        };
        if from.is_empty() {
            // neutral from: 기저 계산값. 동적 생성 요소는 computed_styles 가 비어 있을 수
            // 있으므로 레이아웃을 강제해 채운다(덮어쓰기 전이라 아직 neutral 상태).
            self.ensure_layout();
            from = self
                .computed_styles
                .get(&id)
                .and_then(|m| m.get(prop))
                .cloned()
                .unwrap_or_default();
        }
        if from.is_empty() || from == new_value {
            return;
        }
        // CSS 전역 키워드(initial/inherit/unset) 해석.
        let from = self.resolve_wide_keyword(id, prop, &from);
        let to = self.resolve_wide_keyword(id, prop, new_value);
        // em/rem 을 px 로 해석 — 안 그러면 불연속 판정(interp_css_value 가 em 보간 못함)이
        // 부드러운 길이 전이를 불연속으로 오판해 캡처를 스킵한다(text-decoration-thickness
        // 1em→0em 등). transform 은 raw 각도 유지가 필요하므로 제외.
        let (from, to) = if prop == "transform" {
            (from, to)
        } else {
            let (fs, rfs) = self.elem_font_sizes(id);
            (Self::resolve_font_units(&from, fs, rfs), Self::resolve_font_units(&to, fs, rfs))
        };
        if from.is_empty() || from == to {
            return;
        }
        // 불연속(보간 불가) 값은 transition-behavior:allow-discrete 일 때만 전이한다.
        // 기본(normal)은 전이 없이 to 로 즉시 점프하므로 캡처하지 않는다. transform/
        // scale 등도 interp_prop 이 판정(interp_css_value 는 함수 리스트를 모른다).
        // 판정: interp 가 None 이거나, 0.25 에서 from 을 그대로 돌려주면(스텝 함수 =
        // 불연속 플립) 부드러운 보간이 아니다. interp_prop(0.5).is_none() 만으론
        // 불연속 플립이 Some(to) 를 내 놓쳤다(background-size auto→길이 등).
        let (bw, bh) = self.layout_rects.get(&id).map(|r| (r.2, r.3)).unwrap_or((0.0, 0.0));
        let cc = self.elem_color(id);
        let smooth = Self::interp_prop(prop, &from, &to, 0.25, bw, bh, &cc)
            .map(|v| v.trim() != from.trim())
            .unwrap_or(false);
        if !smooth {
            let behavior = self.style_get(id, "transition-behavior");
            if !behavior.contains("allow-discrete") {
                return;
            }
        }
        let mut m = ObjMap::new();
        m.insert("currentTime".to_string(), Value::Num(elapsed as f64));
        let rc = std::rc::Rc::new(std::cell::RefCell::new(m));
        let mut props = std::collections::HashMap::new();
        props.insert(prop.to_string(), (from, to, easing));
        self.element_animations.entry(id).or_default().push((rc, dur as f64, props));
    }

    // element.classList: class 속성을 공백 구분 토큰 목록으로
    // 토큰 검증 (§7.1): 빈 문자열은 SyntaxError, ASCII 공백이 들어 있으면
    // InvalidCharacterError. 예전엔 검증이 없어서 조용히 통과했고, 공백이 든 토큰이
    // class 속성에 들어가 **두 개의 클래스**가 돼 버렸다.
    // XML Name 문법 (Namespaces in XML §2 / DOM §Validate).
    fn is_name_start(c: char) -> bool {
        c == ':'
            || c == '_'
            || c.is_ascii_alphabetic()
            || matches!(c as u32,
                0xC0..=0xD6 | 0xD8..=0xF6 | 0xF8..=0x2FF | 0x370..=0x37D | 0x37F..=0x1FFF
                | 0x200C..=0x200D | 0x2070..=0x218F | 0x2C00..=0x2FEF | 0x3001..=0xD7FF
                | 0xF900..=0xFDCF | 0xFDF0..=0xFFFD | 0x10000..=0xEFFFF)
    }

    fn is_name_char(c: char) -> bool {
        Self::is_name_start(c)
            || c == '-'
            || c == '.'
            || c.is_ascii_digit()
            || c as u32 == 0xB7
            || matches!(c as u32, 0x0300..=0x036F | 0x203F..=0x2040)
    }

    pub(super) fn is_valid_name(name: &str) -> bool {
        let mut it = name.chars();
        match it.next() {
            Some(c) if Self::is_name_start(c) => {}
            _ => return false,
        }
        it.all(Self::is_name_char)
    }

    // HTMLCollection 을 만든다: 요소 배열에 "\0coll" 표시를 달아 member_get 이
    // item()/namedItem()/이름 접근을 처리하게 한다.
    pub(super) fn make_collection(&self, items: Vec<Value>) -> Value {
        let a = ArrayObj::new(items);
        a.set_prop("\u{0}coll".to_string(), Value::Bool(true));
        Value::Arr(a)
    }

    // HTMLCollection 의 이름 조회(§4.2.10.2 "named item"): id 가 일치하는 첫 요소,
    // 없으면 name 속성이 일치하는 첫 요소. 빈 이름은 매칭하지 않는다.
    pub(super) fn collection_named(
        &mut self,
        a: &std::rc::Rc<ArrayObj>,
        name: &str,
    ) -> Option<Value> {
        if name.is_empty() {
            return None;
        }
        let items = a.borrow().clone();
        let dom = self.dom_arena().ok()?;
        for attr in ["id", "name"] {
            for v in &items {
                if let Value::Dom(id) = v {
                    if let crate::dom::NodeType::Element(e) = &dom.get(*id).node_type {
                        if e.attributes.get(attr).map(String::as_str) == Some(name) {
                            return Some(v.clone());
                        }
                    }
                }
            }
        }
        None
    }

    // §4.4 "locate a namespace": 요소에서 조상으로 올라가며 네임스페이스를 찾는다.
    // prefix 가 비면 기본 네임스페이스(xmlns), 아니면 xmlns:prefix 선언을 본다.
    // 선언이 없으면 요소 자신의 네임스페이스(접두사가 일치할 때)를 쓴다.
    pub(super) fn locate_namespace(
        &mut self,
        id: crate::dom::NodeId,
        prefix: &str,
    ) -> Result<Option<String>, String> {
        let dom = self.dom_arena()?;
        let mut cur = Some(id);
        while let Some(nid) = cur {
            if let crate::dom::NodeType::Element(e) = &dom.get(nid).node_type {
                // 요소 자신의 네임스페이스: 접두사가 일치하면 그것이다
                let own_prefix = e.prefix().unwrap_or("");
                if own_prefix == prefix && e.namespace.is_some() {
                    return Ok(e.namespace.clone());
                }
                // xmlns / xmlns:prefix 선언
                let attr = if prefix.is_empty() {
                    "xmlns".to_string()
                } else {
                    format!("xmlns:{}", prefix)
                };
                if let Some(v) = e.attributes.get(&attr) {
                    return Ok(if v.is_empty() { None } else { Some(v.clone()) });
                }
                // HTML 네임스페이스 요소이고 기본 네임스페이스를 찾는 중이면 HTML ns.
                // #document/#document-fragment 는 요소가 아니라 컨테이너 — 네임스페이스
                // 조회에서 null 이다(§DOM locate-a-namespace). HTML ns 를 주면 안 된다.
                if prefix.is_empty()
                    && own_prefix.is_empty()
                    && e.namespace.is_none()
                    && !matches!(e.tag_name.as_str(), "#document" | "#document-fragment")
                {
                    return Ok(Some(crate::dom::NS_HTML.to_string()));
                }
            }
            cur = dom.get(nid).parent;
        }
        Ok(None)
    }

    // §4.4 "locate a namespace prefix": 이 네임스페이스를 선언한 접두사를 찾는다.
    pub(super) fn locate_prefix(
        &mut self,
        id: crate::dom::NodeId,
        ns: &str,
    ) -> Result<Option<String>, String> {
        let dom = self.dom_arena()?;
        let mut cur = Some(id);
        while let Some(nid) = cur {
            if let crate::dom::NodeType::Element(e) = &dom.get(nid).node_type {
                if e.ns() == ns {
                    if let Some(p) = e.prefix() {
                        return Ok(Some(p.to_string()));
                    }
                }
                for (k, v) in e.attributes.iter() {
                    if let Some(p) = k.strip_prefix("xmlns:") {
                        if v == ns {
                            return Ok(Some(p.to_string()));
                        }
                    }
                }
            }
            cur = dom.get(nid).parent;
        }
        Ok(None)
    }

    // 속성 이름 정규화 (§4.9): HTML 네임스페이스 요소의 속성 이름은 **소문자**다.
    // 검증도 함께 — 유효한 이름이 아니면 InvalidCharacterError.
    // 예전엔 둘 다 없어서 setAttribute('FOO', v) 가 "FOO" 라는 속성을 만들었고,
    // getAttribute('foo') 는 그걸 못 찾았다 (조회는 소문자로 하니까).
    pub(super) fn attr_name(
        &mut self,
        id: crate::dom::NodeId,
        raw: &str,
    ) -> Result<String, String> {
        if !Self::is_valid_name(raw) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 속성 이름"));
        }
        let dom = self.dom_arena()?;
        let html_ns = matches!(&dom.get(id).node_type,
            crate::dom::NodeType::Element(e) if e.namespace.is_none());
        Ok(if html_ns { raw.to_ascii_lowercase() } else { raw.to_string() })
    }

    // createElement 의 이름 검증 (§4.5): 유효한 Name 이 아니면 InvalidCharacterError.
    // 예전엔 빈 문자열만 걸렀다 — createElement("<div>") 가 조용히 통과했다.
    pub(super) fn validate_element_name(&mut self, name: &str) -> Result<(), String> {
        if !Self::is_valid_name(name) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 요소 이름"));
        }
        Ok(())
    }

    // createElementNS 의 (네임스페이스, 정규화 이름) 검증 (§Validate and extract).
    pub(super) fn validate_qualified_name(
        &mut self,
        qname: &str,
        ns: Option<&str>,
    ) -> Result<(), String> {
        if !Self::is_valid_name(qname) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 이름"));
        }
        let parts: Vec<&str> = qname.split(':').collect();
        if parts.len() > 2 || parts.iter().any(|p| p.is_empty()) {
            return Err(self.throw_dom("InvalidCharacterError", "유효하지 않은 정규화 이름"));
        }
        let prefix = if parts.len() == 2 { Some(parts[0]) } else { None };
        if prefix.is_some() && ns.is_none() {
            return Err(self.throw_dom("NamespaceError", "접두사에 네임스페이스가 없다"));
        }
        if prefix == Some("xml") && ns != Some("http://www.w3.org/XML/1998/namespace") {
            return Err(self.throw_dom("NamespaceError", "xml 접두사의 네임스페이스가 다르다"));
        }
        let xmlns_ns = "http://www.w3.org/2000/xmlns/";
        let is_xmlns = qname == "xmlns" || prefix == Some("xmlns");
        if is_xmlns != (ns == Some(xmlns_ns)) {
            return Err(self.throw_dom("NamespaceError", "xmlns 네임스페이스가 맞지 않다"));
        }
        Ok(())
    }

    // 검증 순서가 중요하다 (표준): **모든** 토큰의 빈 문자열을 먼저 보고,
    // 그다음 **모든** 토큰의 공백을 본다. replace(" ", "") 는 InvalidCharacterError 가
    // 아니라 SyntaxError 다 (두 번째 인자가 빈 문자열이므로).
    pub(super) fn validate_tokens(&mut self, tokens: &[String]) -> Result<(), String> {
        if tokens.iter().any(|t| t.is_empty()) {
            return Err(self.throw_dom("SyntaxError", "빈 토큰"));
        }
        if tokens.iter().any(|t| t.contains([' ', '\t', '\n', '\x0C', '\r'])) {
            return Err(self.throw_dom("InvalidCharacterError", "토큰에 공백이 있다"));
        }
        Ok(())
    }

    // DOMTokenList 의 토큰 집합 (§7.1): ASCII 공백으로 자르고 **중복을 없앤 순서 집합**.
    // 예전엔 유니코드 공백으로 자르고 중복도 남겨서, class="a a b" 의 length 가 3 이었다.
    pub(super) fn class_tokens(&mut self, id: crate::dom::NodeId) -> Vec<String> {
        let raw = self.class_attr(id);
        let mut out: Vec<String> = Vec::new();
        for t in crate::dom::split_ascii_ws(&raw) {
            if !out.iter().any(|x| x == t) {
                out.push(t.to_string());
            }
        }
        out
    }

    // class 속성의 **원문** (§7.1 value 는 반영 속성이라 정규화하지 않는다)
    pub(super) fn class_attr(&mut self, id: crate::dom::NodeId) -> String {
        if let Ok(dom) = self.dom_arena() {
            if let crate::dom::NodeType::Element(e) = &dom.get(id).node_type {
                return e.attributes.get("class").cloned().unwrap_or_default();
            }
        }
        String::new()
    }

    // "update steps" (§7.1): 토큰 집합을 공백 하나로 이어 class 속성에 쓴다.
    // 단 속성이 원래 없고 집합도 비면 **속성을 만들지 않는다** (표준).
    pub(super) fn set_class_tokens(&mut self, id: crate::dom::NodeId, tokens: Vec<String>) {
        let had = {
            match self.dom_arena() {
                Ok(dom) => matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.attributes.get("class").is_some()),
                Err(_) => false,
            }
        };
        if !had && tokens.is_empty() {
            return;
        }
        let joined = tokens.join(" ");
        if let Ok(dom) = self.dom_arena() {
            dom.set_attr(id, "class", joined);
        }
    }

    // 렌더된 텍스트 (innerText). display:none 은 건너뛰고, 블록 경계마다 줄을 나누고,
    // 공백은 접는다 (white-space: pre* 면 보존).
    fn inner_text(&mut self, id: crate::dom::NodeId) -> String {
        // 자기 자신이 렌더되지 않으면 textContent 를 돌려준다 (표준 §3.6.1 1단계).
        let hidden = self
            .computed_styles
            .get(&id)
            .and_then(|m| m.get("display"))
            .map(|d| d == "none")
            .unwrap_or(false);
        if hidden {
            if let Ok(dom) = self.dom_arena() {
                return dom.text_content(id);
            }
        }
        let mut lines: Vec<String> = vec![String::new()];
        self.render_text_into(id, &mut lines, true);
        let out: Vec<String> = lines.iter().map(|l| l.trim_end().to_string()).collect();
        // 앞뒤 빈 줄은 버린다 (표준: 시작/끝의 줄바꿈 제거)
        let start = out.iter().position(|l| !l.is_empty()).unwrap_or(out.len());
        let end = out.iter().rposition(|l| !l.is_empty()).map(|i| i + 1).unwrap_or(start);
        out[start..end].join("\n")
    }

    fn render_text_into(&mut self, id: crate::dom::NodeId, lines: &mut Vec<String>, root: bool) {
        let disp = self
            .computed_styles
            .get(&id)
            .and_then(|m| m.get("display"))
            .cloned()
            .unwrap_or_default();
        // 루트 자신이 렌더되지 않으면 textContent 를 돌려준다 (표준).
        if disp == "none" && !root {
            return;
        }
        let (kids, is_text, text, tag) = {
            let Ok(dom) = self.dom_arena() else { return };
            let node = dom.get(id);
            match &node.node_type {
                crate::dom::NodeType::Text(t) => (Vec::new(), true, t.clone(), String::new()),
                // 코멘트는 텍스트 콘텐츠에 기여하지 않는다 (§4.5 textContent)
                crate::dom::NodeType::Comment(_) => {
                    (Vec::new(), true, String::new(), String::new())
                }
                crate::dom::NodeType::Element(e) => {
                    (node.children.clone(), false, String::new(), e.tag_name.to_ascii_lowercase())
                }
                // PI/DocumentType 는 텍스트 콘텐츠에 기여하지 않는다.
                _ => (Vec::new(), true, String::new(), String::new()),
            }
        };
        if is_text {
            // 텍스트 노드는 계산 스타일이 없다 — 부모 요소의 white-space 를 본다(상속).
            let parent_id = match self.dom_arena() {
                Ok(dom) => dom.get(id).parent,
                Err(_) => None,
            };
            let ws = parent_id
                .and_then(|p| self.computed_styles.get(&p))
                .and_then(|m| m.get("white-space"))
                .cloned()
                .unwrap_or_default();
            let keep = ws.starts_with("pre") || ws == "break-spaces";
            if keep {
                // 줄바꿈 정규화(\r\n, \r → \n), 탭/공백은 보존(§HTML get-the-text).
                let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
                let mut parts = normalized.split('\n');
                if let Some(first) = parts.next() {
                    lines.last_mut().unwrap().push_str(first);
                }
                for p in parts {
                    lines.push(p.to_string());
                }
            } else {
                // 공백 접기: 연속 공백/줄바꿈 → 공백 하나
                let mut collapsed = String::new();
                let mut sp = false;
                for c in text.chars() {
                    if c.is_whitespace() {
                        if !sp {
                            collapsed.push(' ');
                            sp = true;
                        }
                    } else {
                        collapsed.push(c);
                        sp = false;
                    }
                }
                let cur = lines.last_mut().unwrap();
                // 줄 맨 앞의 공백은 버린다 (블록 시작의 공백은 렌더되지 않는다)
                if cur.is_empty() {
                    cur.push_str(collapsed.trim_start());
                } else {
                    cur.push_str(&collapsed);
                }
            }
            return;
        }
        if tag == "br" {
            lines.push(String::new());
            return;
        }
        // 스크립트류·대체/임베디드 요소의 내용은 innerText 에 기여하지 않는다(§HTML
        // "rendered text collection"). textarea 는 값이 별도, iframe/video/canvas 등은
        // 대체 요소. select 는 옵션 텍스트가 포함되므로 제외하지 않는다.
        if matches!(
            tag.as_str(),
            "script" | "style" | "template" | "noscript" | "iframe" | "video" | "audio"
                | "canvas" | "embed" | "object" | "textarea"
        ) {
            return;
        }
        // 블록 레벨이면 앞뒤로 줄을 나눈다
        let block = matches!(
            disp.as_str(),
            "block" | "flex" | "grid" | "list-item" | "table" | "table-row" | "table-cell"
                | "table-row-group" | "table-header-group" | "table-footer-group" | "flow-root"
        );
        if block && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
        for c in kids {
            self.render_text_into(c, lines, false);
        }
        if block && !lines.last().map(|l| l.is_empty()).unwrap_or(true) {
            lines.push(String::new());
        }
    }

    // offsetParent: 가장 가까운 위치 지정(static 아님) 조상. 없으면 body (표준 §CSSOM View).
    // position: fixed 인 요소와 body/html 자신은 null.
    fn offset_parent(&mut self, id: crate::dom::NodeId) -> Option<crate::dom::NodeId> {
        let pos = |me: &Self, n: crate::dom::NodeId| -> String {
            me.computed_styles
                .get(&n)
                .and_then(|m| m.get("position"))
                .cloned()
                .unwrap_or_else(|| "static".to_string())
        };
        if pos(self, id) == "fixed" {
            return None;
        }
        let dom = self.dom_arena().ok()?;
        // 조상 사슬 (아레나 borrow 를 먼저 끝낸다)
        let mut chain = Vec::new();
        let mut cur = dom.get(id).parent;
        while let Some(p) = cur {
            chain.push(p);
            cur = dom.get(p).parent;
        }
        let mut body = None;
        for p in &chain {
            if let crate::dom::NodeType::Element(e) = &self.dom_arena().ok()?.get(*p).node_type {
                if e.tag_name.eq_ignore_ascii_case("body") {
                    body = Some(*p);
                }
            }
        }
        for p in chain {
            if matches!(pos(self, p).as_str(), "relative" | "absolute" | "fixed" | "sticky") {
                return Some(p);
            }
        }
        body
    }

    pub(super) fn dom_get(&mut self, id: crate::dom::NodeId, key: &str) -> Result<Value, String> {
        // Node 인터페이스 상수(§Node): 모든 노드가 Node.prototype 에서 상속한다 —
        // el.ELEMENT_NODE === 1 등. 예전엔 전역 Node 에만 있고 인스턴스엔 없어
        // el.nodeType === el.ELEMENT_NODE 를 검사하는 WPT 다수가 undefined 로 깨졌다.
        if let Some(c) = node_constant(key) {
            return Ok(Value::Num(c));
        }
        // href/src 절대 URL 해석용 base (dom borrow 전에 복제).
        let base = self.base_url.clone();
        let self_shadow = self.shadow_hosts.contains(&id);
        // 레이아웃 측정 프로퍼티 (dom 아레나 borrow 전에 처리 — 이중 borrow 방지).
        // offset* 는 border box, client* 는 근사로 같은 박스 크기를 돌려준다.
        match key {
            "offsetWidth" | "clientWidth" | "scrollWidth" | "offsetHeight" | "clientHeight"
            | "scrollHeight" | "offsetLeft" | "clientLeft" | "offsetTop" | "clientTop"
            | "offsetParent" | "innerText" | "outerText" => {
                // 측정 전에 보류된 레이아웃을 흘린다 (CSSOM View: forced layout)
                // innerText/outerText 도 "렌더된 텍스트" 라 렌더 정보가 있어야 한다.
                self.ensure_layout();
            }
            _ => {}
        }
        // innerText: **렌더된** 텍스트 (HTML §3.6.1). textContent 와 다르다 —
        // display:none 인 가지, <script>/<style>/<template> 의 내용은 빠지고,
        // 블록 경계에서 줄바꿈이 들어가고, 공백은 접힌다.
        // (예전엔 textContent 별칭이라 스크립트 소스까지 그대로 돌려줬다.)
        // outerText getter 는 innerText 와 같은 "렌더된 텍스트"를 반환한다(§HTML).
        if key == "innerText" || key == "outerText" {
            return Ok(Value::Str(self.inner_text(id)));
        }
        // CSSOM View §4 — 셋은 서로 다른 상자다:
        //   offset* = 테두리 박스, client* = 패딩 박스(테두리 제외), scroll* = 스크롤 오버플로.
        //   clientLeft/clientTop 은 **좌표가 아니라 테두리 두께**다.
        //   offsetLeft/offsetTop 은 offsetParent 의 패딩 모서리 기준 상대 좌표다.
        match key {
            "offsetWidth" => {
                let w = self.layout_rects.get(&id).map(|r| r.2).unwrap_or(0.0);
                return Ok(Value::Num(w as f64));
            }
            "offsetHeight" => {
                let h = self.layout_rects.get(&id).map(|r| r.3).unwrap_or(0.0);
                return Ok(Value::Num(h as f64));
            }
            "clientWidth" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.padding_w as f64));
            }
            "clientHeight" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.padding_h as f64));
            }
            "scrollWidth" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.scroll_w.round() as f64));
            }
            "scrollHeight" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.scroll_h.round() as f64));
            }
            "clientLeft" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.border.3 as f64));
            }
            "clientTop" => {
                let m = self.layout_metrics.get(&id).copied().unwrap_or_default();
                return Ok(Value::Num(m.border.0 as f64));
            }
            "offsetLeft" | "offsetTop" => {
                let (x, y, ..) = self.layout_rects.get(&id).copied().unwrap_or_default();
                // offsetParent 의 패딩 모서리를 원점으로
                let (ox, oy) = match self.offset_parent(id) {
                    Some(p) => {
                        let (px, py, ..) = self.layout_rects.get(&p).copied().unwrap_or_default();
                        let m = self.layout_metrics.get(&p).copied().unwrap_or_default();
                        (px + m.border.3, py + m.border.0)
                    }
                    None => (0.0, 0.0),
                };
                return Ok(Value::Num(if key == "offsetLeft" {
                    (x - ox) as f64
                } else {
                    (y - oy) as f64
                }));
            }
            // 가장 가까운 위치 지정 조상 (없으면 body). 툴팁/드롭다운 배치가 이걸로 좌표계를 잡는다.
            "offsetParent" => {
                return Ok(match self.offset_parent(id) {
                    Some(p) => Value::Dom(p),
                    None => Value::Null,
                });
            }
            // element.dataset — data-* 속성을 camelCase 키 객체로 (읽기 스냅샷)
            // dataset 은 **살아있는 뷰**다 (DOMStringMap): 읽기도 쓰기도 data-* 속성에 직결.
            // 예전엔 스냅샷 객체를 돌려줘서 el.dataset.x = '1' 이 조용히 사라졌다.
            "dataset" => return Ok(Value::Dataset(id)),
            _ => {}
        }
        let dom = self.dom_arena()?;
        let is_el = |d: &crate::dom::Dom, c: crate::dom::NodeId| {
            matches!(d.get(c).node_type, crate::dom::NodeType::Element(_))
        };
        match key {
            // <template>.content — 우리 파서는 템플릿 자식을 그대로 그 아래 둔다.
            // UA 스타일시트가 template 을 display:none 으로 감추므로 렌더되지 않는다.
            // 템플릿 자신을 돌려주면 content.querySelector/cloneNode/children 이 다 동작한다.
            "content"
                if matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "template") =>
            {
                Ok(Value::Dom(id))
            }
            // DOMParser 가 돌려준 <html> 문서 노드에서의 body/head/documentElement
            "body" | "head" | "documentElement"
                if matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "html") =>
            {
                if key == "documentElement" {
                    return Ok(Value::Dom(id));
                }
                let want = key;
                Ok(dom
                    .get(id)
                    .children
                    .iter()
                    .copied()
                    .find(|&c| matches!(&dom.get(c).node_type,
                        crate::dom::NodeType::Element(e) if e.tag_name == want))
                    .map(Value::Dom)
                    .unwrap_or(Value::Null))
            }
            // el.attributes — NamedNodeMap (§4.9.1). 진짜 Attr 노드들이다.
            // 예전엔 평범한 {name, value} 객체 배열이라, attr.value 를 바꿔도 요소에
            // 아무 반영이 없었고 attr.ownerElement 도 없었다.
            "attributes" => {
                let names: Vec<String> = match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => {
                        e.attributes.iter().map(|(k, _)| k.clone()).collect()
                    }
                    _ => Vec::new(),
                };
                let list: Vec<Value> =
                    names.iter().map(|n| Value::Attr(id, n.clone())).collect();
                // NamedNodeMap 은 인덱스와 이름 양쪽으로 접근한다 (attrs['class'])
                let arr = ArrayObj::new(list.clone());
                for (n, v) in names.iter().zip(list.iter()) {
                    arr.set_prop(n.clone(), v.clone());
                }
                arr.set_prop("getNamedItem".to_string(), Value::Native(Native::GetNamedItem));
                // NamedNodeMap 브랜드 — Object.prototype.toString 이 @@toStringTag 를 읽어
                // "[object NamedNodeMap]" 를 낸다(내부 키라 Object.keys 등엔 안 샌다).
                arr.set_prop(
                    "\u{0}@@toStringTag".to_string(),
                    Value::Str("NamedNodeMap".to_string()),
                );
                Ok(Value::Arr(arr))
            }
            // <style>/<link> 의 .sheet — 그 요소가 만든 CSSStyleSheet (§CSSOM 6.3)
            "sheet" => {
                self.sync_sheets();
                let owner = id;
                let idx = self
                    .sheets()
                    .and_then(|ss| ss.iter().position(|e| e.owner == Some(owner)));
                return Ok(idx.map(Value::Sheet).unwrap_or(Value::Null));
            }
            // 문서 트리에 붙어 있는가 (분리된 노드인지 판별 — 프레임워크가 흔히 본다)
            "isConnected" => {
                let root = dom.root;
                let connected = id == root || dom.ancestors(id).contains(&root);
                Ok(Value::Bool(connected))
            }
            // attachShadow 를 부른 요소면 자기 자신이 섀도 루트다 (문서화된 근사)
            "shadowRoot" => Ok(if self_shadow {
                Value::Dom(id)
            } else {
                Value::Null
            }),
            // <form>.elements — 폼 컨트롤 목록
            "elements"
                if matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "form") =>
            {
                let mut out = Vec::new();
                collect_form_controls(dom, id, &mut out);
                Ok(Value::Arr(ArrayObj::new(out.into_iter().map(Value::Dom).collect())))
            }
            // element.style/classList → 속성에 대한 라이브 프록시
            "style" => Ok(Value::Style(id)),
            "classList" => Ok(Value::ClassList(id)),
            // textContent(§DOM): CharacterData(Text/Comment/PI)는 자신의 데이터,
            // Document·DocumentType 은 null, Element·DocumentFragment 는 하위 텍스트 연결.
            "textContent" => match &dom.get(id).node_type {
                crate::dom::NodeType::Text(t) => Ok(Value::Str(t.clone())),
                crate::dom::NodeType::Comment(c) => Ok(Value::Str(c.clone())),
                crate::dom::NodeType::ProcessingInstruction { data, .. } => {
                    Ok(Value::Str(data.clone()))
                }
                crate::dom::NodeType::DocumentType { .. } => Ok(Value::Null),
                crate::dom::NodeType::Element(e) if e.tag_name == "#document" => Ok(Value::Null),
                _ => Ok(Value::Str(dom.text_content(id))),
            },
            "innerHTML" => Ok(Value::Str(dom.inner_html(id))),
            "outerHTML" => Ok(Value::Str(dom.outer_html(id))),
            // value: <select> 는 선택된 option 의 값, <option> 은 value 속성 없으면 텍스트,
            // 그 외(input/textarea)는 value 속성. 예전엔 셋 다 value 속성만 봐서
            // select.value 가 늘 빈 문자열이었다(폼 로직이 통째로 어긋난다).
            "value" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) if e.tag_name == "select" => {
                    Ok(Value::Str(selected_option(dom, id).map(|o| option_value(dom, o)).unwrap_or_default()))
                }
                crate::dom::NodeType::Element(e) if e.tag_name == "option" => {
                    Ok(Value::Str(option_value(dom, id)))
                }
                crate::dom::NodeType::Element(e) if e.tag_name == "textarea" => Ok(Value::Str(
                    e.attributes.get("value").cloned().unwrap_or_else(|| dom.text_content(id)),
                )),
                // <li>.value 는 long 반영(§HTML) — 문자열이 아니라 수. HTML 정수 파싱
                // (선행 공백/부호 허용), 범위 밖·무효는 기본값 0.
                crate::dom::NodeType::Element(e) if e.tag_name == "li" => {
                    let n = e
                        .attributes
                        .get("value")
                        .and_then(|s| {
                            let t = s.trim_start_matches([' ', '\t', '\n', '\x0C', '\r']);
                            let (neg, t) = t
                                .strip_prefix('-')
                                .map(|r| (true, r))
                                .unwrap_or_else(|| (false, t.strip_prefix('+').unwrap_or(t)));
                            let d: String =
                                t.chars().take_while(|c| c.is_ascii_digit()).collect();
                            d.parse::<i64>().ok().map(|v| if neg { -v } else { v })
                        })
                        .filter(|v| (-2147483648..=2147483647).contains(v))
                        .unwrap_or(0);
                    Ok(Value::Num(n as f64))
                }
                crate::dom::NodeType::Element(e) => Ok(Value::Str(
                    e.attributes.get("value").cloned().unwrap_or_default(),
                )),
                _ => Ok(Value::Undefined),
            },
            // checked/selected/disabled 등 불리언 속성 반사. 예전엔 undefined 였다 —
            // `if (cb.checked)` 가 항상 거짓이라 체크박스 로직이 죽는다.
            "checked" | "disabled" | "readOnly" | "required" | "multiple" | "hidden" => {
                let attr = match key {
                    "readOnly" => "readonly",
                    k => k,
                };
                Ok(match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => {
                        Value::Bool(e.attributes.contains_key(attr))
                    }
                    _ => Value::Undefined,
                })
            }
            "selected" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    Value::Bool(e.attributes.contains_key("selected"))
                }
                _ => Value::Undefined,
            }),
            "selectedIndex" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) if e.tag_name == "select" => {
                    let opts = option_list(dom, id);
                    let sel = selected_option(dom, id);
                    Value::Num(
                        sel.and_then(|s| opts.iter().position(|&o| o == s))
                            .map(|i| i as f64)
                            .unwrap_or(-1.0),
                    )
                }
                _ => Value::Undefined,
            }),
            "options" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) if e.tag_name == "select" => {
                    Value::Arr(ArrayObj::new(
                        option_list(dom, id).into_iter().map(Value::Dom).collect(),
                    ))
                }
                _ => Value::Undefined,
            }),
            // 트리 순회 프로퍼티 (프레임워크/앱 코드가 광범위하게 사용)
            "children" => {
                let arr: Vec<Value> = dom
                    .get(id)
                    .children
                    .clone()
                    .into_iter()
                    .filter(|&c| is_el(dom, c))
                    .map(Value::Dom)
                    .collect();
                // children 은 HTMLCollection — item()/namedItem()/이름 접근을 위해 표시.
                let a = ArrayObj::new(arr);
                a.set_prop("\u{0}coll".to_string(), Value::Bool(true));
                Ok(Value::Arr(a))
            }
            "childNodes" => {
                let arr: Vec<Value> =
                    dom.get(id).children.iter().copied().map(Value::Dom).collect();
                Ok(Value::Arr(ArrayObj::new(arr)))
            }
            "childElementCount" => {
                let n = dom.get(id).children.iter().filter(|&&c| is_el(dom, c)).count();
                Ok(Value::Num(n as f64))
            }
            "firstElementChild" => Ok(dom
                .get(id)
                .children
                .iter()
                .copied()
                .find(|&c| is_el(dom, c))
                .map(Value::Dom)
                .unwrap_or(Value::Null)),
            "lastElementChild" => Ok(dom
                .get(id)
                .children
                .iter()
                .copied()
                .rev()
                .find(|&c| is_el(dom, c))
                .map(Value::Dom)
                .unwrap_or(Value::Null)),
            "firstChild" => {
                Ok(dom.get(id).children.first().copied().map(Value::Dom).unwrap_or(Value::Null))
            }
            "lastChild" => {
                Ok(dom.get(id).children.last().copied().map(Value::Dom).unwrap_or(Value::Null))
            }
            // parentNode 는 모든 부모, parentElement 는 부모가 **요소일 때만** (§4.4).
            // 예전엔 둘이 같았다.
            "parentNode" => Ok(dom.get(id).parent.map(Value::Dom).unwrap_or(Value::Null)),
            "parentElement" => Ok(dom
                .get(id)
                .parent
                .filter(|&p| is_el(dom, p))
                .map(Value::Dom)
                .unwrap_or(Value::Null)),
            // nextSibling/previousSibling — **모든** 노드 종류를 센다 (텍스트/코멘트 포함).
            // 예전엔 nextElementSibling 만 있어서 이 둘이 undefined 였다. DOM 순회의
            // 기본 연산이라, 이게 없으면 TreeWalker 도 하이라이터도 한 노드에서 멈춘다.
            "nextSibling" | "previousSibling" => {
                let next = key.starts_with("next");
                let result = dom.get(id).parent.and_then(|p| {
                    let sibs = &dom.get(p).children;
                    let idx = sibs.iter().position(|&c| c == id)?;
                    if next {
                        sibs.get(idx + 1).copied()
                    } else {
                        idx.checked_sub(1).and_then(|i| sibs.get(i).copied())
                    }
                });
                Ok(result.map(Value::Dom).unwrap_or(Value::Null))
            }
            // 요소가 속한 문서. jQuery 의 setDocument 가 `node.ownerDocument || node` 로
            // 문서를 정하는데, 없으면 요소 자신을 document 로 삼아 document.createElement
            // 가 undefined 가 되며 jQuery 전체가 죽었다.
            "ownerDocument" => {
                Ok(env_get(&self.global, "document").unwrap_or(Value::Null))
            }
            // 문서 순서 비교 (jQuery 의 sortOrder). 4=뒤따름, 2=앞섬, 0=동일.
            "compareDocumentPosition" => Ok(Value::Native(Native::CompareDocPosition)),
            // getRootNode() — 노드가 속한 트리의 루트 (§4.4). 섀도우 DOM 없으므로 연결된
            // 노드는 document, 분리된 서브트리는 최상위 조상.
            "getRootNode" => Ok(Value::Native(Native::DomGetRootNode)),
            "nextElementSibling" | "previousElementSibling" => {
                let next = key.starts_with("next");
                let result = dom.get(id).parent.and_then(|p| {
                    let sibs = dom.get(p).children.clone();
                    let idx = sibs.iter().position(|&c| c == id)?;
                    let order: Vec<usize> = if next {
                        (idx + 1..sibs.len()).collect()
                    } else {
                        (0..idx).rev().collect()
                    };
                    order.into_iter().map(|i| sibs[i]).find(|&c| is_el(dom, c))
                });
                Ok(result.map(Value::Dom).unwrap_or(Value::Null))
            }
            // tagName 은 요소만. nodeName 은 모든 노드에 있다 (§4.4).
            // tagName: HTML 네임스페이스에서만 대문자로 (§4.9). SVG 의 clipPath 를
            // 대문자로 만들면 다른 이름이 된다.
            "tagName" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => Ok(Value::Str(
                    if e.namespace.is_none() {
                        e.tag_name.to_ascii_uppercase()
                    } else {
                        e.tag_name.clone()
                    },
                )),
                _ => Ok(Value::Undefined),
            },
            // 네임스페이스 관련 (DOM §4.9). 예전엔 아예 없어서 undefined 였다.
            "localName" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => Value::Str(e.local_name().to_string()),
                _ => Value::Undefined,
            }),
            "namespaceURI" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => Value::Str(e.ns().to_string()),
                _ => Value::Null,
            }),
            "prefix" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    e.prefix().map(|p| Value::Str(p.to_string())).unwrap_or(Value::Null)
                }
                _ => Value::Null,
            }),
            // ProcessingInstruction.target / DocumentType.name/publicId/systemId (§4.13/4.7).
            "target"
                if matches!(&dom.get(id).node_type, crate::dom::NodeType::ProcessingInstruction { .. }) =>
            {
                match &dom.get(id).node_type {
                    crate::dom::NodeType::ProcessingInstruction { target, .. } => {
                        Ok(Value::Str(target.clone()))
                    }
                    _ => Ok(Value::Undefined),
                }
            }
            "name" | "publicId" | "systemId"
                if matches!(&dom.get(id).node_type, crate::dom::NodeType::DocumentType { .. }) =>
            {
                match &dom.get(id).node_type {
                    crate::dom::NodeType::DocumentType { name, public_id, system_id } => {
                        Ok(Value::Str(match key {
                            "name" => name.clone(),
                            "publicId" => public_id.clone(),
                            _ => system_id.clone(),
                        }))
                    }
                    _ => Ok(Value::Undefined),
                }
            }
            "nodeName" => Ok(Value::Str(match &dom.get(id).node_type {
                // DocumentFragment 센티널: nodeName 은 "#document-fragment"(대문자화 안 함).
                crate::dom::NodeType::Element(e) if e.tag_name == "#document-fragment" => {
                    "#document-fragment".to_string()
                }
                crate::dom::NodeType::Element(e) => {
                    if e.namespace.is_none() {
                        e.tag_name.to_ascii_uppercase()
                    } else {
                        e.tag_name.clone()
                    }
                }
                crate::dom::NodeType::Text(_) => "#text".to_string(),
                crate::dom::NodeType::Comment(_) => "#comment".to_string(),
                // PI 의 nodeName 은 target, DocumentType 의 nodeName 은 name(§4.4).
                crate::dom::NodeType::ProcessingInstruction { target, .. } => target.clone(),
                crate::dom::NodeType::DocumentType { name, .. } => name.clone(),
            })),
            // nodeValue/data: 텍스트·코멘트의 문자 데이터 (§4.9 CharacterData).
            // 예전엔 아예 없어서 textNode.data 가 undefined 였다.
            "nodeValue" => Ok(match &dom.get(id).node_type {
                crate::dom::NodeType::Text(t) => Value::Str(t.clone()),
                crate::dom::NodeType::Comment(c) => Value::Str(c.clone()),
                crate::dom::NodeType::ProcessingInstruction { data, .. } => Value::Str(data.clone()),
                // 요소/DocumentType 는 nodeValue 가 null (표준)
                crate::dom::NodeType::Element(_) | crate::dom::NodeType::DocumentType { .. } => {
                    Value::Null
                }
            }),
            "data" => {
                let cd = match &dom.get(id).node_type {
                    crate::dom::NodeType::Text(t) => Some(t.clone()),
                    crate::dom::NodeType::Comment(c) => Some(c.clone()),
                    crate::dom::NodeType::ProcessingInstruction { data, .. } => Some(data.clone()),
                    _ => None,
                };
                match cd {
                    Some(s) => Ok(Value::Str(s)),
                    // 요소(object.data 등)는 URL 반영으로, 없으면 Undefined.
                    None => Ok(self.reflect_get(id, "data")?.unwrap_or(Value::Undefined)),
                }
            }
            "length" => match &dom.get(id).node_type {
                crate::dom::NodeType::Text(t) => {
                    Ok(Value::Num(t.encode_utf16().count() as f64))
                }
                crate::dom::NodeType::Comment(c) => {
                    Ok(Value::Num(c.encode_utf16().count() as f64))
                }
                crate::dom::NodeType::ProcessingInstruction { data, .. } => {
                    Ok(Value::Num(data.encode_utf16().count() as f64))
                }
                crate::dom::NodeType::Element(_) | crate::dom::NodeType::DocumentType { .. } => {
                    Ok(Value::Undefined)
                }
            },
            // nodeType: ELEMENT_NODE(1) / TEXT_NODE(3).
            // jQuery·프레임워크가 노드 종류 판별에 광범위하게 쓴다.
            "nodeType" => Ok(Value::Num(match &dom.get(id).node_type {
                // DocumentFragment 센티널(#document-fragment)은 DOCUMENT_FRAGMENT_NODE(11).
                crate::dom::NodeType::Element(e) if e.tag_name == "#document-fragment" => 11.0,
                crate::dom::NodeType::Element(_) => 1.0,
                crate::dom::NodeType::Text(_) => 3.0,
                crate::dom::NodeType::ProcessingInstruction { .. } => 7.0,
                crate::dom::NodeType::Comment(_) => 8.0,
                crate::dom::NodeType::DocumentType { .. } => 10.0,
            })),
            "id" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    Ok(Value::Str(e.attributes.get("id").cloned().unwrap_or_default()))
                }
                _ => Ok(Value::Undefined),
            },
            "className" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    Ok(Value::Str(e.attributes.get("class").cloned().unwrap_or_default()))
                }
                _ => Ok(Value::Undefined),
            },
            // URL 반사 프로퍼티: 절대 URL 로 해석 (getAttribute 는 원문 반환).
            // <a>/<area>/<link> 의 URL 분해 속성 (HTML 표준 HTMLHyperlinkElementUtils).
            // 없으면 a.pathname 같은 흔한 코드가 undefined 를 읽고 죽는다 (naver).
            "protocol" | "hostname" | "host" | "port" | "pathname" | "search" | "hash"
            | "origin" => {
                let raw = match &dom.get(id).node_type {
                    crate::dom::NodeType::Element(e) => {
                        e.attributes.get("href").cloned().unwrap_or_default()
                    }
                    _ => String::new(),
                };
                if raw.is_empty() {
                    return Ok(Value::Str(String::new()));
                }
                let abs = match &base {
                    Some(b) => crate::url::Url::parse(b)
                        .ok()
                        .and_then(|u| u.join(&raw))
                        .map(|u| u.as_string())
                        .unwrap_or(raw.clone()),
                    None => raw.clone(),
                };
                let Ok(u) = crate::url::Url::parse(&abs) else {
                    return Ok(Value::Str(String::new()));
                };
                let path_no_hash = u.path.split('#').next().unwrap_or("").to_string();
                let (pathname, search) = match path_no_hash.split_once('?') {
                    Some((p, q)) => (p.to_string(), format!("?{}", q)),
                    None => (path_no_hash.clone(), String::new()),
                };
                // 프래그먼트는 Url 파서가 떼어내므로 **속성 원문**에서 뽑는다
                // (join 이 이미 버린 뒤라 절대 URL 에는 남아 있지 않다).
                let hash = match raw.split_once('#') {
                    Some((_, h)) if !h.is_empty() => format!("#{}", h),
                    _ => String::new(),
                };
                // host 는 포트를 포함한다 (기본 포트면 생략) — hostname 은 포트 없이.
                let default_port = matches!(
                    (u.scheme.as_str(), u.port),
                    ("http", 80) | ("https", 443)
                );
                let host = if default_port {
                    u.host.clone()
                } else {
                    format!("{}:{}", u.host, u.port)
                };
                let port = if default_port { String::new() } else { u.port.to_string() };
                Ok(Value::Str(match key {
                    "protocol" => format!("{}:", u.scheme),
                    "hostname" => u.host.clone(),
                    "host" => host.clone(),
                    "port" => port,
                    "pathname" => {
                        if pathname.is_empty() {
                            "/".to_string()
                        } else {
                            pathname
                        }
                    }
                    "search" => search,
                    "hash" => hash,
                    _ => format!("{}://{}", u.scheme, host),
                }))
            }
            "href" | "src" | "action" => match &dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => {
                    let raw = e.attributes.get(key).cloned().unwrap_or_default();
                    let abs = match &base {
                        Some(b) if !raw.is_empty() => crate::url::Url::parse(b)
                            .ok()
                            .and_then(|u| u.join(&raw))
                            .map(|u| u.as_string())
                            .unwrap_or(raw),
                        _ => raw,
                    };
                    Ok(Value::Str(abs))
                }
                _ => Ok(Value::Undefined),
            },
            // 여기까지 안 잡혔으면 IDL 반영 표를 본다 (HTML §2.6).
            // 표에도 없으면 undefined (표준의 "그런 IDL 속성 없음").
            _ => Ok(self.reflect_get(id, key)?.unwrap_or(Value::Undefined)),
        }
    }

    pub(super) fn dom_set(&mut self, id: crate::dom::NodeId, key: &str, value: Value) -> Result<(), String> {
        // el.onclick = fn → 핸들러 등록
        if let Some(event) = key.strip_prefix("on") {
            if matches!(value, Value::Fn(_)) {
                self.handlers.push((id, event.to_string(), value, false, false, false)); // on* 속성은 버블 단계, non-passive
            }
            return Ok(());
        }
        // DOMString 반영 속성 대입은 ToString 강제변환(§Web IDL) — 객체의 toString/
        // valueOf 를 호출한다. to_display 는 "[object Object]" 로 눌러 버렸다.
        let text = self.to_string_value(&value)?;
        let dom = self.dom_arena()?;
        match key {
            "textContent" => {
                // textContent 는 nullable(DOMString?) — null/undefined 는 "" 로 취급해
                // 자식을 모두 제거(텍스트 노드도 안 만듦). 그 외는 ToString.
                let t = if matches!(value, Value::Null | Value::Undefined) {
                    String::new()
                } else {
                    text
                };
                dom.set_text_content(id, t);
                Ok(())
            }
            // 문자 데이터 대입 (§4.9). 요소에 대한 nodeValue 대입은 무시 (표준).
            "nodeValue" | "data" => {
                dom.set_char_data(id, text);
                Ok(())
            }
            // innerText 대입: 줄바꿈은 <br> 가 된다 (표준). textContent 로 넣으면
            // 줄이 통째로 붙어 버린다.
            "innerText" => {
                if text.contains('\n') {
                    let html = text
                        .split('\n')
                        .map(|l| {
                            l.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
                        })
                        .collect::<Vec<_>>()
                        .join("<br>");
                    dom.clear_children(id);
                    for tree in crate::html::parse_fragment(html) {
                        let sub = dom.insert_tree(tree, Some(id));
                        dom.get_mut(id).children.push(sub);
                    }
                } else {
                    dom.set_text_content(id, text);
                }
                Ok(())
            }
            // outerText 대입: 요소 **자신**을 텍스트로 교체(§HTML). 줄바꿈(\r\n,\r,\n)→
            // <br>, 빈 문자열은 빈 텍스트 노드 하나. 앞/뒤 텍스트 노드와만 경계 병합
            // (전체 정규화 아님). 부모가 없으면 NoModificationAllowedError.
            "outerText" => {
                let Some(parent) = dom.get(id).parent else {
                    return Err(self.throw_dom(
                        "NoModificationAllowedError",
                        "outerText: 부모 없는 요소는 교체할 수 없다",
                    ));
                };
                let idx = dom.get(parent).children.iter().position(|&c| c == id).unwrap_or(0);
                let norm = text.replace("\r\n", "\n").replace('\r', "\n");
                let mut new_nodes: Vec<crate::dom::NodeId> = Vec::new();
                if norm.is_empty() {
                    new_nodes.push(dom.create_text(String::new()));
                } else {
                    for (i, seg) in norm.split('\n').enumerate() {
                        if i > 0 {
                            new_nodes.push(dom.create_element("br"));
                        }
                        if !seg.is_empty() {
                            new_nodes.push(dom.create_text(seg.to_string()));
                        }
                    }
                }
                let is_text = |d: &crate::dom::Dom, n: crate::dom::NodeId| {
                    matches!(d.get(n).node_type, crate::dom::NodeType::Text(_))
                };
                let text_of = |d: &crate::dom::Dom, n: crate::dom::NodeId| match &d.get(n).node_type {
                    crate::dom::NodeType::Text(t) => t.clone(),
                    _ => String::new(),
                };
                // 경계 병합: 첫 new 텍스트를 이전 형제(텍스트)와, 마지막 new 텍스트를 다음
                // 형제(텍스트)와 이어붙이고, 그 형제들을 splice 범위에 포함시켜 제거.
                let prev = if idx > 0 { dom.get(parent).children.get(idx - 1).copied() } else { None };
                let next = dom.get(parent).children.get(idx + 1).copied();
                let mut start = idx;
                let mut end = idx + 1;
                if let (Some(p), Some(&f)) = (prev, new_nodes.first()) {
                    if is_text(dom, p) && is_text(dom, f) {
                        let pd = text_of(dom, p);
                        if let crate::dom::NodeType::Text(t) = &mut dom.get_mut(f).node_type {
                            *t = pd + t;
                        }
                        dom.get_mut(p).parent = None;
                        start = idx - 1;
                    }
                }
                if let (Some(nx), Some(&l)) = (next, new_nodes.last()) {
                    if is_text(dom, nx) && is_text(dom, l) {
                        let nd = text_of(dom, nx);
                        if let crate::dom::NodeType::Text(t) = &mut dom.get_mut(l).node_type {
                            t.push_str(&nd);
                        }
                        dom.get_mut(nx).parent = None;
                        end = idx + 2;
                    }
                }
                dom.get_mut(id).parent = None;
                for &nn in &new_nodes {
                    dom.get_mut(nn).parent = Some(parent);
                }
                dom.get_mut(parent).children.splice(start..end, new_nodes.iter().copied());
                Ok(())
            }
            "innerHTML" => {
                // 조각 파싱 (관용 파서) → 자식 교체
                dom.clear_children(id);
                for tree in crate::html::parse_fragment(text) {
                    let sub = dom.insert_tree(tree, Some(id));
                    dom.get_mut(id).children.push(sub);
                }
                Ok(())
            }
            "value" => {
                // select.value = x → 그 값을 가진 option 을 선택 상태로 (표준)
                let is_select = matches!(&dom.get(id).node_type,
                    crate::dom::NodeType::Element(e) if e.tag_name == "select");
                if is_select {
                    for o in option_list(dom, id) {
                        if option_value(dom, o) == text {
                            dom.set_attr(o, "selected", String::new());
                        } else {
                            dom.remove_attr(o, "selected");
                        }
                    }
                    return Ok(());
                }
                dom.set_attr(id, "value", text);
                Ok(())
            }
            // 불리언 속성: true 면 속성 추가, false 면 제거 (표준 반사)
            "checked" | "disabled" | "readOnly" | "required" | "multiple" | "hidden"
            | "selected" => {
                let attr = match key {
                    "readOnly" => "readonly",
                    k => k,
                };
                if to_bool(&value) {
                    dom.set_attr(id, attr, String::new());
                } else {
                    dom.remove_attr(id, attr);
                }
                Ok(())
            }
            // className/id 는 대응 속성으로 (스타일 매칭이 읽음)
            "className" | "id" => {
                let attr = if key == "className" { "class" } else { "id" };
                dom.set_attr(id, attr, text);
                Ok(())
            }
            // IDL 반영 표 (HTML §2.6). 예전엔 표에 있는 속성도 조용히 무시했다 —
            // img.width = 100 이 아무 일도 안 했다.
            // classList / style 대입은 [PutForwards] 다 (표준):
            // el.classList = "a b" 는 class 속성을, el.style = "..." 는 style 속성을 쓴다.
            "classList" => {
                let dom = self.dom_arena()?;
                dom.set_attr(id, "class", text);
                Ok(())
            }
            // el.style = "..." 는 [PutForwards=cssText] — 문자열을 style 속성(인라인
            // 선언)으로 파싱해 쓴다. 예전엔 이 대입이 조용히 무시됐다(el.style 은 읽기
            // 전용 IDL 속성이라 _ 갈래에서 no-op). 빈 문자열은 인라인 스타일을 지운다.
            "style" => {
                if text.is_empty() {
                    dom.remove_attr(id, "style");
                } else {
                    dom.set_attr(id, "style", text);
                }
                Ok(())
            }
            _ => {
                if self.reflect_set(id, key, &value)? {
                    return Ok(());
                }
                // 이미 존재하는 IDL 속성(읽기 전용)에 대입하면 **아무 일도 없다** (표준의
                // sloppy 모드). expando 로 저장하면 진짜 프로퍼티를 가려 버린다 —
                // 실제로 el.classList = "x" 가 DOMTokenList 를 문자열로 덮어썼다.
                if !matches!(self.dom_get(id, key)?, Value::Undefined) {
                    return Ok(());
                }
                // 그 외에는 스크립트가 붙인 임의 프로퍼티(expando)로 보관한다.
                // 플랫폼 객체도 평범한 객체다 — el.foo = 1 이 실제로 저장돼야 한다.
                // 예전엔 조용히 버려서, 커스텀 엘리먼트의 this._v = ... 가 사라졌다.
                self.dom_props.insert((id, key.to_string()), value);
                Ok(())
            }
        }
    }
}

// <form> 안의 폼 컨트롤 (input/select/textarea/button)
pub(super) fn collect_form_controls(
    dom: &crate::dom::Dom,
    id: crate::dom::NodeId,
    out: &mut Vec<crate::dom::NodeId>,
) {
    for &c in &dom.get(id).children {
        if let crate::dom::NodeType::Element(e) = &dom.get(c).node_type {
            if matches!(e.tag_name.as_str(), "input" | "select" | "textarea" | "button") {
                out.push(c);
            }
        }
        collect_form_controls(dom, c, out);
    }
}

// <select> 의 option 목록 (optgroup 안쪽 포함)
pub(super) fn option_list(dom: &crate::dom::Dom, sel: crate::dom::NodeId) -> Vec<crate::dom::NodeId> {
    let mut out = Vec::new();
    fn walk(dom: &crate::dom::Dom, id: crate::dom::NodeId, out: &mut Vec<crate::dom::NodeId>) {
        for &c in &dom.get(id).children {
            if let crate::dom::NodeType::Element(e) = &dom.get(c).node_type {
                if e.tag_name == "option" {
                    out.push(c);
                } else {
                    walk(dom, c, out);
                }
            }
        }
    }
    walk(dom, sel, &mut out);
    out
}

// 선택된 option: selected 속성이 있는 첫 번째, 없으면 첫 option (HTML 표준의 기본 선택)
pub(super) fn selected_option(
    dom: &crate::dom::Dom,
    sel: crate::dom::NodeId,
) -> Option<crate::dom::NodeId> {
    let opts = option_list(dom, sel);
    opts.iter()
        .copied()
        .find(|&o| matches!(&dom.get(o).node_type,
            crate::dom::NodeType::Element(e) if e.attributes.contains_key("selected")))
        .or_else(|| opts.first().copied())
}

// option 의 값: value 속성이 없으면 텍스트 내용 (HTML 표준)
pub(super) fn option_value(dom: &crate::dom::Dom, o: crate::dom::NodeId) -> String {
    match &dom.get(o).node_type {
        crate::dom::NodeType::Element(e) => {
            e.attributes.get("value").cloned().unwrap_or_else(|| dom.text_content(o).trim().to_string())
        }
        _ => String::new(),
    }
}

// data-foo-bar → fooBar (dataset 키 변환)

// CSS 값 원문 안의 숫자를 정규 형태로 (§6.7.2 "serialize a CSS component value"):
//   .5 → 0.5,  1.50 → 1.5,  +3 → 3
// 값 전체가 하나의 따옴표 문자열이면 그 내용(따옴표 제거)을 돌려준다. 내부에
// 이스케이프 안 된 같은 종류 따옴표가 있으면(= 여러 토큰일 수 있음) None.
fn single_css_string(raw: &str) -> Option<String> {
    let b = raw.as_bytes();
    if raw.len() < 2 {
        return None;
    }
    let q = b[0];
    if (q != b'"' && q != b'\'') || b[raw.len() - 1] != q {
        return None;
    }
    let inner = &raw[1..raw.len() - 1];
    if inner.contains(q as char) {
        return None;
    }
    Some(inner.to_string())
}

// font-family 직렬화(§CSSOM): 쉼표로 나눈 각 패밀리에서, 따옴표 문자열인데
// 내용이 유효한 식별자 시퀀스(공백으로 나뉜 각 토큰이 CSS 식별자)면 따옴표를 뺀다.
// 아니면 큰따옴표 문자열로. 이미 따옴표 없는 이름/generic 은 그대로.
fn serialize_font_family(raw: &str) -> String {
    raw.split(',')
        .map(|fam| {
            let f = fam.trim();
            match single_css_string(f) {
                Some(inner) if is_css_ident_sequence(&inner) => inner,
                Some(inner) => serialize_css_string(&inner),
                None => f.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

// 공백으로 나뉜 각 토큰이 모두 유효한 CSS 식별자인가(비어있지 않아야 함).
// -webkit- 레거시 이름 별칭(§CSS Flexbox 1 부록). 별칭으로 설정/조회해도 캐논 이름으로
// 저장·직렬화한다(단축 확장이 아니라 이름 별칭이라 var() 도 그대로 보존).
fn canonical_css_name(prop: &str) -> &str {
    match prop {
        "-webkit-align-content" => "align-content",
        "-webkit-align-items" => "align-items",
        "-webkit-align-self" => "align-self",
        "-webkit-flex" => "flex",
        "-webkit-flex-basis" => "flex-basis",
        "-webkit-flex-direction" => "flex-direction",
        "-webkit-flex-flow" => "flex-flow",
        "-webkit-flex-grow" => "flex-grow",
        "-webkit-flex-shrink" => "flex-shrink",
        "-webkit-flex-wrap" => "flex-wrap",
        "-webkit-justify-content" => "justify-content",
        "-webkit-order" => "order",
        other => other,
    }
}

fn is_css_ident_sequence(s: &str) -> bool {
    let toks: Vec<&str> = s.split(' ').collect();
    !s.is_empty() && toks.iter().all(|t| is_css_ident(t))
}

// CSS 식별자 근사(§CSS Syntax): 첫 글자는 문자/_/-(숫자·빈문자 불가), 나머지는
// 문자/숫자/_/-. (이스케이프는 근사로 미지원 — 있으면 문자열로 유지된다.)
fn is_css_ident(t: &str) -> bool {
    let mut it = t.chars();
    match it.next() {
        Some(c) if c.is_alphabetic() || c == '_' || c == '-' => {}
        _ => return false,
    }
    t.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}

// 값 전체가 하나의 url(...) 토큰이면 그 URL(따옴표 제거)을 돌려준다.
fn single_url(raw: &str) -> Option<String> {
    let low = raw.to_ascii_lowercase();
    if !low.starts_with("url(") || !raw.ends_with(')') {
        return None;
    }
    let inner = raw[4..raw.len() - 1].trim();
    // url() 안에 또 ) 가 있으면 단일 토큰이 아니다
    if inner.contains(')') {
        return None;
    }
    let unq = if inner.len() >= 2 {
        let q = inner.as_bytes()[0];
        if (q == b'"' || q == b'\'') && inner.as_bytes()[inner.len() - 1] == q {
            &inner[1..inner.len() - 1]
        } else {
            inner
        }
    } else {
        inner
    };
    Some(unq.to_string())
}

// CSSOM "serialize a string" (§common serializing): 큰따옴표로 감싸고 내부의
// 큰따옴표와 역슬래시를 이스케이프한다.
fn serialize_css_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

// 문자열 리터럴과 url(...) 안은 건드리지 않는다 (그 안의 숫자는 값이 아니다).
fn normalize_numbers(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        // 문자열 리터럴: 그대로 복사
        if c == '"' || c == '\'' {
            let quote = c;
            out.push(c);
            i += 1;
            while i < b.len() {
                out.push(b[i]);
                let esc = b[i] == '\\';
                i += 1;
                if !esc && b.get(i - 1) == Some(&quote) {
                    break;
                }
            }
            continue;
        }
        // url( ... ) 안은 그대로
        if b[i..].starts_with(&['u', 'r', 'l', '(']) || b[i..].starts_with(&['U', 'R', 'L', '(']) {
            while i < b.len() {
                out.push(b[i]);
                i += 1;
                if b.get(i - 1) == Some(&')') {
                    break;
                }
            }
            continue;
        }
        // 숫자 시작인가. 식별자 중간의 숫자(예: rgb1)는 건드리지 않는다.
        let prev_ident = i > 0 && (b[i - 1].is_alphanumeric() || b[i - 1] == '-' || b[i - 1] == '_');
        let starts_num = c.is_ascii_digit()
            || (c == '.' && b.get(i + 1).map_or(false, |d| d.is_ascii_digit()))
            || ((c == '+' || c == '-')
                && b.get(i + 1).map_or(false, |d| {
                    d.is_ascii_digit() || (*d == '.' && b.get(i + 2).map_or(false, |e| e.is_ascii_digit()))
                }));
        if !starts_num || prev_ident {
            out.push(c);
            i += 1;
            continue;
        }
        let start = i;
        if b[i] == '+' || b[i] == '-' {
            i += 1;
        }
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i < b.len() && b[i] == '.' {
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
        }
        // 지수 표기 (1e3)
        if i < b.len() && (b[i] == 'e' || b[i] == 'E') {
            let save = i;
            i += 1;
            if i < b.len() && (b[i] == '+' || b[i] == '-') {
                i += 1;
            }
            if i < b.len() && b[i].is_ascii_digit() {
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            } else {
                i = save;
            }
        }
        let text: String = b[start..i].iter().collect();
        match text.parse::<f32>() {
            Ok(n) => out.push_str(&crate::style::num_css(n)),
            Err(_) => out.push_str(&text),
        }
    }
    out
}
