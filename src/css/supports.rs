use super::shorthand::expand_declaration;

// @supports 조건 평가. not / and / or (괄호 밖) 와 (prop: value) 원자를 처리.
// 원자는 해당 선언이 파싱되면 지원으로 본다(관용적 — 미지원보단 포함 쪽으로).
// selector(...) 등 함수형 조건은 미지원(false)으로 간주.
pub(crate) fn supports_condition(cond: &str) -> bool {
    let c = cond.trim();
    if c.is_empty() {
        return false;
    }
    // not <cond>
    if let Some(rest) = strip_not(c) {
        return !supports_condition(rest);
    }
    // 최상위(괄호 밖) and / or
    if let Some(parts) = split_on_kw(c, "and") {
        return parts.iter().all(|p| supports_condition(p));
    }
    if let Some(parts) = split_on_kw(c, "or") {
        return parts.iter().any(|p| supports_condition(p));
    }
    // 괄호 묶음: 내부가 선언이면 검사, 아니면 하위 조건으로 재귀
    if c.starts_with('(') && c.ends_with(')') {
        let inner = c[1..c.len() - 1].trim();
        let nested = inner.starts_with('(')
            || strip_not(inner).is_some()
            || split_on_kw(inner, "and").is_some()
            || split_on_kw(inner, "or").is_some();
        if !nested && inner.contains(':') {
            return declaration_supported(inner);
        }
        return supports_condition(inner);
    }
    // selector() / font-tech() / font-format() — 함수형 조건 (CSS Conditional §3)
    if let Some(r) = functional_supports(c) {
        return r;
    }
    false
}

// @supports 의 함수형 조건. 처리 대상이면 Some(결과), 아니면 None(비함수형).
fn functional_supports(c: &str) -> Option<bool> {
    let lower = c.to_ascii_lowercase();
    // selector(<complex-selector>): 셀렉터가 파싱되면(엔진이 매칭하는 표준 문법) 지원.
    if let Some(rest) = lower.strip_prefix("selector(") {
        if !rest.ends_with(')') {
            return Some(false);
        }
        let inner = c["selector(".len()..c.len() - 1].trim();
        return Some(
            crate::css::parse_selector_list(inner)
                .map(|l| !l.is_empty())
                .unwrap_or(false),
        );
    }
    // font-tech(<font-tech>): 알려진 폰트 기술 식별자만 참.
    if let Some(rest) = lower.strip_prefix("font-tech(") {
        if !rest.ends_with(')') {
            return Some(false);
        }
        return Some(FONT_TECHS.contains(&rest[..rest.len() - 1].trim()));
    }
    // font-format(<font-format>): 알려진 폰트 포맷 식별자만 참.
    if let Some(rest) = lower.strip_prefix("font-format(") {
        if !rest.ends_with(')') {
            return Some(false);
        }
        return Some(FONT_FORMATS.contains(&rest[..rest.len() - 1].trim()));
    }
    None
}

// CSS Fonts §11.1 <font-tech> 값. 대소문자 무시(호출측이 소문자로 넘긴다).
const FONT_TECHS: &[&str] = &[
    "features-opentype", "features-aat", "features-graphite",
    "color-colrv0", "color-colrv1", "color-svg", "color-sbix", "color-cbdt",
    "variations", "palettes", "incremental",
];
// CSS Fonts §11.1 <font-format> 값.
const FONT_FORMATS: &[&str] = &[
    "collection", "embedded-opentype", "opentype", "svg", "truetype", "woff", "woff2",
];

fn strip_not(c: &str) -> Option<&str> {
    let lower = c.to_ascii_lowercase();
    if lower.starts_with("not ") || lower.starts_with("not(") {
        Some(c[3..].trim())
    } else {
        None
    }
}

// 엔진이 실제로 해석하는 longhand 프로퍼티 집합.
// @supports 는 이 집합으로만 참을 낸다 — 과다보고하면 사이트가 우리가 못 그리는
// 모던 레이아웃(container query/subgrid 등)을 내보내고 렌더가 깨진다.
// 과소보고는 안전하다(사이트가 폴백 CSS 를 준다). 새 프로퍼티를 구현하면 여기 추가.
pub(crate) const SUPPORTED: &[&str] = &[
    "align-content", "align-items", "align-self", "aspect-ratio", "backdrop-filter",
    "background-color", "background-image", "background-position", "background-repeat",
    "background-size", "border-bottom-width", "border-collapse",
    "border-color", "border-left-width", "border-radius",
    "border-right-width", "border-spacing", "border-style",
    "border-top-color", "border-right-color", "border-bottom-color", "border-left-color",
    "border-top-width", "border-width", "bottom", "box-shadow",
    // 컨테이너 쿼리 (레이아웃 후 두 번째 스타일 패스로 실제 평가한다)
    "container", "container-name", "container-type",
    // 마스크 (그라디언트/이미지의 알파로 서브트리를 곱한다)
    "mask-image", "-webkit-mask-image",
    // 3D 변환 (4x4 로 조립해 평면 요소를 투영행렬로 접는다)
    "perspective", "perspective-origin",
    "box-sizing", "clear", "clip-path", "color", "column-count", "column-gap", "content",
    "direction", "display", "filter", "flex", "flex-basis", "flex-direction",
    "flex-flow", "flex-grow", "flex-shrink", "flex-wrap", "float", "font-family", "font-size",
    "font-style", "font-weight", "gap", "grid-area", "grid-auto-rows", "grid-column",
    "grid-column-end", "grid-column-start", "grid-row", "grid-row-end", "grid-row-start",
    "grid-template-areas", "grid-template-columns", "grid-template-rows", "height",
    "justify-content", "justify-items", "justify-self", "left", "letter-spacing",
    "line-height", "list-style", "list-style-type", "margin-bottom", "margin-left",
    "margin-right", "margin-top", "max-height", "max-width", "min-height", "min-width",
    "mix-blend-mode", "object-fit", "object-position", "opacity", "order", "outline-color",
    "outline-offset", "outline-style", "outline-width", "overflow", "overflow-wrap",
    "overflow-x", "overflow-y", "overflow-block", "overflow-inline",
    "padding-bottom", "padding-left", "padding-right",
    "padding-top", "position", "right", "row-gap", "text-align", "text-decoration-color",
    "text-decoration-line", "text-indent", "text-overflow", "text-shadow", "text-transform", "top",
    "transition",
    "transform", "vertical-align", "visibility", "white-space", "width", "word-break",
    "word-spacing", "word-wrap", "z-index",
    // transition/animation 롱핸드: 애니메이션은 미구현이나 계산값(속성 목록/시간/이징)은
    // 파싱·직렬화한다 — 실제 CSS 프로퍼티이므로 getComputedStyle·CSS.supports 에 노출.
    "transition-property", "transition-duration", "transition-delay",
    "transition-timing-function", "transition-behavior",
    "animation-name", "animation-duration", "animation-delay",
    "animation-timing-function", "animation-iteration-count", "animation-direction",
    "animation-fill-mode", "animation-play-state", "animation-composition",
    "animation-range", "animation-range-start", "animation-range-end",
    // 이번 스윕에서 검증 추가한 프로퍼티(CSS.supports·계산값 노출).
    "input-security", "overlay", "caret-animation", "object-view-box", "margin-trim",
    "flex-line-count", "interpolate-size", "scroll-initial-target", "offset",
    // UI/인터랙션/표/스크롤 키워드 프로퍼티 — 실제 CSS 프로퍼티라 계산값 노출.
    "cursor", "appearance", "user-select", "resize", "pointer-events", "touch-action",
    "interactivity",
    "hyphens", "hyphenate-limit-chars", "hyphenate-character", "writing-mode", "text-orientation", "image-rendering", "isolation",
    "box-decoration-break", "caption-side", "empty-cells", "table-layout",
    "background-attachment", "background-clip", "background-origin", "overflow-anchor",
    "overflow-clip-margin",
    "scroll-behavior", "text-decoration-style", "text-underline-position", "will-change",
    "contain", "content-visibility", "backface-visibility", "transform-style",
    "transform-box", "text-align-last", "overscroll-behavior-x", "overscroll-behavior-y",
    "background-blend-mode", "font-kerning", "font-variant-caps", "text-rendering",
    "color-scheme", "forced-color-adjust", "print-color-adjust",
    "caret-color", "accent-color", "tab-size",
    // 2차 배치: text/font-variant/ruby/scrollbar/list 키워드 프로퍼티
    "text-emphasis-style", "text-emphasis-position", "text-combine-upright",
    "text-decoration-skip-ink", "line-break", "text-wrap", "text-wrap-mode",
    "text-wrap-style", "text-spacing-trim", "ruby-position", "ruby-align",
    "white-space-collapse", "font-optical-sizing", "font-synthesis",
    "font-synthesis-weight", "font-synthesis-style", "font-synthesis-small-caps",
    "font-synthesis-position",
    "font-variant-ligatures", "font-variant-numeric", "font-variant-east-asian",
    "font-variant-position", "font-variant-alternates", "font-language-override",
    "list-style-position", "quotes", "scrollbar-width", "scrollbar-color",
    "scrollbar-gutter", "mask-type", "text-justify",
    // 3차 배치: grid/break/column/bidi 키워드 + 수치 프로퍼티
    "grid-auto-flow", "grid-auto-columns", "break-before", "break-after", "break-inside",
    "page-break-before", "page-break-after", "page-break-inside", "column-span",
    "column-fill", "column-rule-style", "caret-shape", "unicode-bidi",
    "border-image-repeat", "text-underline-offset", "column-width", "column-rule-width",
    "counter-reset", "counter-increment", "counter-set", "orphans", "widows",
    "shape-margin", "shape-image-threshold",
    // SVG 페인트/색 프로퍼티
    "fill", "stroke", "stop-color", "flood-color", "lighting-color", "column-rule-color",
    "text-emphasis-color", "-webkit-text-fill-color", "-webkit-text-stroke-color",
    // 4차: logical 프로퍼티(길이/색/스타일) — 계산값 노출(가로쓰기 기준 물리와 동치).
    "inset-block-start", "inset-block-end", "inset-inline-start", "inset-inline-end",
    "margin-block-start", "margin-block-end", "margin-inline-start", "margin-inline-end",
    "padding-block-start", "padding-block-end", "padding-inline-start", "padding-inline-end",
    "border-block-start-width", "border-block-end-width", "border-inline-start-width",
    "border-inline-end-width", "border-block-start-style", "border-block-end-style",
    "border-inline-start-style", "border-inline-end-style", "border-block-start-color",
    "border-block-end-color", "border-inline-start-color", "border-inline-end-color",
    "block-size", "inline-size", "min-block-size", "min-inline-size", "max-block-size",
    "max-inline-size",
    // mask / offset / scroll / contain-intrinsic
    "mask-image", "mask-repeat", "mask-position", "mask-size", "mask-origin", "mask-clip",
    "mask-composite", "mask-mode", "offset-path", "offset-distance", "offset-rotate",
    "offset-anchor", "offset-position", "scroll-margin-top", "scroll-margin-right", "scroll-margin-bottom",
    "scroll-margin-left", "scroll-padding-top", "scroll-padding-right",
    "scroll-padding-bottom", "scroll-padding-left", "scroll-snap-stop", "place-self",
    "place-items", "place-content",
    "grid-gap", "grid-row-gap", "grid-column-gap",
    // scroll-margin/scroll-padding 단축·논리(§CSS Scroll Snap).
    "scroll-margin", "scroll-padding",
    "scroll-margin-block", "scroll-margin-inline", "scroll-padding-block", "scroll-padding-inline",
    "scroll-margin-block-start", "scroll-margin-block-end", "scroll-margin-inline-start",
    "scroll-margin-inline-end", "scroll-padding-block-start", "scroll-padding-block-end",
    "scroll-padding-inline-start", "scroll-padding-inline-end",
    "contain-intrinsic-width", "contain-intrinsic-height", "contain-intrinsic-size",
    "contain-intrinsic-inline-size", "contain-intrinsic-block-size",
    // 5차: SVG presentation 프로퍼티(비페인트) — svg/ 및 css 전반에서 대량 테스트.
    "fill-opacity", "stroke-opacity", "stroke-width", "stroke-linecap", "stroke-linejoin",
    "stroke-dasharray", "stroke-dashoffset", "stroke-miterlimit", "clip-rule", "fill-rule",
    "paint-order", "vector-effect", "dominant-baseline", "text-anchor", "shape-rendering",
    "color-interpolation", "color-interpolation-filters", "marker-start", "marker-mid",
    "marker-end", "baseline-shift",
    // 6차: font/text/webkit-box/math/misc 키워드 프로퍼티.
    "font-feature-settings", "font-variation-settings", "font-stretch", "font-width", "font-size-adjust",
    "font-palette", "text-decoration-thickness", "hanging-punctuation",
    "text-autospace", "text-fit", "text-size-adjust", "-webkit-text-size-adjust", "-webkit-box-orient",
    "-webkit-line-clamp", "line-clamp", "-webkit-box-align", "-webkit-box-pack", "zoom",
    "image-orientation", "math-style", "math-depth", "math-shift",
    // 8차: 순수 키워드 롱핸드(paint 핸들러 없음 — 게이트 안전).
    "scroll-snap-type", "scroll-snap-align", "view-transition-name", "anchor-name",
    "field-sizing",
    // position-area(§css-anchor-2): 계산값을 캐논으로 방출하므로 계산 스타일 노출 정직.
    "position-area",
    // 9차: transform-origin + 개별 변환 프로퍼티(scale/rotate/translate).
    "transform-origin", "scale", "rotate", "translate",
    // 10차: corner-shape(신규, 코너 렌더 미세조정 — 계산값 노출, 렌더는 기본 모양).
    "corner-shape", "corner-top-left-shape", "corner-top-right-shape",
    "corner-bottom-left-shape", "corner-bottom-right-shape", "corner-block-start-shape",
    "corner-block-end-shape", "corner-inline-start-shape", "corner-inline-end-shape",
    "corner-top-shape", "corner-bottom-shape", "corner-left-shape", "corner-right-shape",
    "corner-block-shape", "corner-inline-shape",
    // 11차: border-image 롱핸드(계산값 노출 — border-image 렌더는 별개).
    "border-image-source", "border-image-slice", "border-image-width", "border-image-outset",
    // border-radius 코너 롱핸드(border-radius 가 이들로 펼쳐짐 — CSS.supports/계산값).
    "border-top-left-radius", "border-top-right-radius", "border-bottom-left-radius",
    "border-bottom-right-radius",
    // 개별 border-*-style(border-style 롱핸드) — 계산값/supports.
    "border-top-style", "border-right-style", "border-bottom-style", "border-left-style",
    // 12차: 흩어진 미지원 프로퍼티(계산값 노출).
    "background-position-x", "background-position-y", "shape-outside", "shape-image-threshold",
    "word-space-transform", "view-transition-class", "text-box-trim", "text-box-edge",
    "text-box", "white-space-trim",
];

fn longhand_supported(prop: &str) -> bool {
    let p = prop.trim().to_ascii_lowercase();
    // 커스텀 프로퍼티(--x)는 var() 로 지원한다.
    if p.starts_with("--") {
        return true;
    }
    SUPPORTED.contains(&p.as_str())
}

// getComputedStyle 뷰의 HasProperty('X' in cs) 판정용 — 엔진이 아는 CSS 속성명인가.
// (대시 형태로 정규화된 이름을 받는다. §CSSOM: 지원 속성은 선언 뷰에 존재한다.)
pub(crate) fn is_known_property(name: &str) -> bool {
    // text-decoration/font 은 계산값에 노출되는 단축(getComputedStyle 의 'font' /
    // 'text-decoration' in cs 게이트) — longhand 집합엔 없지만 알려진 프로퍼티다.
    longhand_supported(name) || matches!(name, "text-decoration" | "font" | "text-spacing")
}

// 엔진이 실제로 계산하는 값 함수 전부. 여기 없는 함수(color-mix/oklch/lab/env/attr/
// image-set …)는 파싱만 되고 무시되므로 지원한다고 하면 거짓말이다.
// 프로퍼티별로 나누지 않고 합집합으로 본다 — 과소보고는 안전, 과다보고만 위험하다.
const FUNCS: &[&str] = &[
    // 값 계산
    "var", "calc", "min", "max", "clamp",
    // 색 — 레거시 + 모던 색 함수(계산값 색공간 보존을 구현했으므로 정직하게 지원).
    "rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "color", "color-mix",
    // 이미지
    "url", "linear-gradient", "radial-gradient", "conic-gradient",
    // content
    "counter", "counters",
    // transform — 2D 함수 전부 (행렬로 합성해 서브트리를 실제로 변환한다)
    "translate", "translatex", "translatey", "scale", "scalex", "scaley",
    "rotate", "rotatez", "skew", "skewx", "skewy", "matrix",
    // transform — 3D 함수. 4x4 행렬로 계산·직렬화(matrix3d)하고 보간도 한다.
    "translate3d", "translatez", "scale3d", "scalez", "rotate3d", "rotatex",
    "rotatey", "matrix3d", "perspective",
    // filter / backdrop-filter
    "blur", "grayscale", "brightness", "invert", "contrast", "sepia", "saturate",
    "hue-rotate", "opacity",
    // clip-path (inset 만 그린다)
    "inset",
    // grid 트랙
    "repeat", "minmax", "fit-content",
    // 이징 함수(transition/animation-timing-function) — 계산값 직렬화 지원
    "cubic-bezier", "steps",
    // 수학 함수(파스 타임 계산값 확정 — abs/sign/round/삼각/sqrt 등)
    "abs", "sign", "mod", "rem", "round", "sin", "cos", "tan", "sqrt", "pow", "log", "exp",
    "hypot",
    // corner-shape 의 superellipse() (계산값 노출)
    "superellipse",
];

// 값에 쓰인 함수 이름을 전부 뽑는다: 식별자 바로 뒤에 '(' 가 오는 형태.
fn value_functions(value: &str) -> Vec<String> {
    let b = value.as_bytes();
    let mut out = Vec::new();
    for (i, &c) in b.iter().enumerate() {
        if c != b'(' {
            continue;
        }
        let mut s = i;
        while s > 0 {
            let p = b[s - 1];
            if p.is_ascii_alphanumeric() || p == b'-' || p == b'_' {
                s -= 1;
            } else {
                break;
            }
        }
        if s < i {
            out.push(value[s..i].to_ascii_lowercase());
        }
    }
    out
}

// 열거형 프로퍼티: 엔진이 키워드를 하나씩 매칭하고 나머지는 조용히 기본값으로 떨어뜨린다.
// 그래서 값 검사 없이는 `@supports (position: sticky)` 가 참이 되고, 사이트는 스티키
// 헤더를 내보내지만 우리는 static 으로 그린다. 각 집합은 엔진 코드의 match 와 1:1 이다.
// 우리가 실제로 그리지 못하는 값들 (어느 프로퍼티에 오든).
const UNSUPPORTED_VALUES: &[&str] = &["subgrid", "masonry"];

fn enum_values(prop: &str) -> Option<&'static [&'static str]> {
    Some(match prop {
        // style.rs StyledNode::display()
        // 실제로 레이아웃하는 값만(§honest, style.rs display_specified 참고). table 계열은
        // layout_table 이 키워드를 직접 봐 렌더하므로 포함. flow-root/list-item/ruby/run-in/
        // math 는 아직 전용 레이아웃이 없어(block 폴백) 제외 — 못 그리는 걸 지원한다 하지 않음.
        "display" => &[
            "block", "inline", "inline-block", "flex", "inline-flex", "grid", "inline-grid",
            "none", "contents", "table", "inline-table", "table-row", "table-cell",
            "table-row-group", "table-header-group", "table-footer-group", "table-caption",
        ],
        // layout/mod.rs LayoutBox::position()
        "position" => &["static", "relative", "absolute", "fixed", "sticky"],
        "float" => &["left", "right", "none"],
        "clear" => &["left", "right", "both", "none"],
        _ => return None,
    })
}

// 하나의 longhand 선언(이름+값)이 실제로 구현돼 있는가.
fn longhand_decl_supported(prop: &str, value: &str) -> bool {
    if !longhand_supported(prop) {
        return false;
    }
    let p = prop.trim().to_ascii_lowercase();
    if p.starts_with("--") {
        return true; // 커스텀 프로퍼티의 값은 임의 토큰이다
    }
    let v = value.trim().to_ascii_lowercase();
    // 미구현 함수가 하나라도 있으면 거짓
    if value_functions(&v).iter().any(|f| !FUNCS.contains(&f.as_str())) {
        return false;
    }
    // 전역 키워드는 어디서나 유효
    if matches!(v.as_str(), "inherit" | "initial" | "unset" | "revert") {
        return true;
    }
    // 값 자체가 미구현인 것들. 프로퍼티 이름만 보면 (grid-template-columns 는 지원하니)
    // subgrid 도 지원한다고 거짓말하게 된다 → 사이트가 우리가 못 그리는 레이아웃을 보낸다.
    // UA 도 값 단위 지원 표를 갖는다.
    if v.split(|c: char| !c.is_alphanumeric() && c != '-').any(|t| UNSUPPORTED_VALUES.contains(&t))
    {
        return false;
    }
    match enum_values(&p) {
        Some(allowed) => allowed.contains(&v.as_str()),
        None => true,
    }
}

// "prop: value" 원자 지원 여부.
// 선언을 longhand 로 확장한 뒤, 확장 결과가 전부 우리가 실제로 해석하는 프로퍼티여야
// 참이다. 예전엔 "파싱만 되면 지원" 이라 subgrid 같은 미구현 기능도
// 지원한다고 거짓말했다(과다보고 → 사이트가 못 그리는 레이아웃을 보냄).
fn declaration_supported(atom: &str) -> bool {
    let Some(colon) = atom.find(':') else { return false };
    let prop = atom[..colon].trim();
    let value = atom[colon + 1..].trim();
    // 선언의 !important 플래그는 문법상 허용되며 지원 판정과 무관하다
    // (@supports (display: block !important) 은 참). 뒤에 붙은 플래그를 떼고 값만 본다.
    let value = {
        let low = value.to_ascii_lowercase();
        match low.rfind("!important") {
            Some(idx) if value[idx + "!important".len()..].trim().is_empty() => {
                value[..idx].trim()
            }
            _ => value,
        }
    };
    if prop.is_empty() || value.is_empty() {
        return false;
    }
    // CSS 전역 키워드(initial/inherit/unset/revert/revert-layer)는 알려진 프로퍼티
    // 모두에 유효하다(§CSS Cascade). expand_declaration 이 이들을 롱핸드로 펼치지
    // 못해 CSS.supports 가 false 를 내던 문제 — interpolation 하네스의 "from initial
    // value should be supported" 선행조건이 전 서브셋에서 깨졌다.
    if matches!(
        value.to_ascii_lowercase().as_str(),
        "initial" | "inherit" | "unset" | "revert" | "revert-layer"
    ) {
        return is_known_property(prop);
    }
    // gradient 는 문법 검증(빈 세그먼트/무효 스톱/hint 배치)이 필요 — 파서가 관대해서
    // 무효 형태도 파싱하므로 gradient_valid 로 걸러 CSS.supports 가 정확히 false.
    if value.to_ascii_lowercase().contains("gradient(") && !super::gradient_valid(value) {
        return false;
    }
    // 원문에 우리가 계산하지 않는 함수가 있으면 거짓 (color-mix/oklch/env/attr…).
    // 파싱 뒤에는 원문이 남지 않는 값도 있어서 확장 전에 본다.
    if value_functions(&value.to_ascii_lowercase()).iter().any(|f| !FUNCS.contains(&f.as_str())) {
        return false;
    }
    let expanded = expand_declaration(prop, value);
    if expanded.is_empty() {
        return false; // 값이 파싱 안 됨
    }
    // 확장된 longhand 가 전부 구현돼 있고, 값도 엔진이 실제로 해석하는 값이어야 한다.
    // box-shadow/text-shadow 의 내부 longhand(-x/-y/-blur/-color 등, 비표준 구현세부)는
    // 제외 — box-shadow 자신은 SUPPORTED 이고 paint 가 원문 Keyword 를 읽어 그린다.
    let is_internal = |name: &str| {
        (name.starts_with("box-shadow-") || name.starts_with("text-shadow-"))
            && name != "box-shadow"
            && name != "text-shadow"
    };
    expanded
        .iter()
        .filter(|d| !is_internal(&d.name))
        .all(|d| longhand_decl_supported(&d.name, &crate::style::computed_value_string(&d.value)))
}

// 괄호 깊이 0 에서 공백으로 둘러싸인 키워드(and/or)로 분리. 없으면 None.
fn split_on_kw(c: &str, kw: &str) -> Option<Vec<String>> {
    let lower = c.to_ascii_lowercase();
    let bytes = c.as_bytes();
    let klen = kw.len();
    let mut depth = 0i32;
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < c.len() {
        match bytes[i] {
            b'(' => depth += 1,
            b')' => depth -= 1,
            _ => {}
        }
        if depth == 0
            && i > 0
            && bytes[i - 1].is_ascii_whitespace()
            && i + klen < c.len()
            && bytes[i + klen].is_ascii_whitespace()
            && lower.get(i..i + klen) == Some(kw)
        {
            parts.push(c[start..i].trim().to_string());
            i += klen;
            start = i;
            continue;
        }
        i += 1;
    }
    if parts.is_empty() {
        return None;
    }
    parts.push(c[start..].trim().to_string());
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supports_at_rule_gates_rules_in_stylesheet() {
        // 지원되는 조건 → 내부 규칙 포함
        let ss = crate::css::parse(
            "@supports (display: grid) { .a { color: #ff0000; } }".to_string(),
        );
        assert_eq!(ss.rules.len(), 1, "지원 조건이면 규칙 포함");
        // 지원 안 되는 조건 → 내부 규칙 제외
        let ss2 = crate::css::parse(
            "@supports not (display: grid) { .a { color: #ff0000; } }".to_string(),
        );
        assert_eq!(ss2.rules.len(), 0, "미지원 조건이면 규칙 제외");
    }

    #[test]
    fn supports_basic_feature_queries() {
        assert!(supports_condition("(display: grid)"));
        assert!(supports_condition("(display: flex)"));
        // 지원하는 두 조건의 and
        assert!(supports_condition("(display: grid) and (gap: 1rem)"));
        // not 지원되는 것 → false
        assert!(!supports_condition("not (display: grid)"));
        // or: 하나라도 지원되면 true
        assert!(supports_condition("(display: -webkit-box) or (display: flex)"));
    }

    #[test]
    fn supports_does_not_overreport_unimplemented_features() {
        // 예전엔 "선언이 파싱되면 지원" 이라 미구현 기능도 참이었다(거짓말).
        // 사이트가 우리가 못 그리는 모던 레이아웃을 내보내 렌더가 깨진다.
        assert!(supports_condition("(container-type: inline-size)"), "이제 진짜로 지원한다");
        // border-*-color 는 4면 모두 페인트가 읽어 그리므로 지원한다(§CSS Backgrounds).
        assert!(supports_condition("(border-left-color: red)"));
        // 엔진이 해석하지 않는 프로퍼티 → 거짓
        assert!(!supports_condition("(unknown-prop: 1px)"));
        // and 중 하나라도 미지원이면 거짓 (subgrid 는 아직 없다)
        assert!(!supports_condition("(display: grid) and (grid-template-columns: subgrid)"));
        assert!(supports_condition("(display: grid) and (container-type: inline-size)"));
        // or 는 지원되는 쪽이 있으면 참
        assert!(supports_condition("(container-type: inline-size) or (display: grid)"));
        // 커스텀 프로퍼티는 지원(var())
        assert!(supports_condition("(--x: 1px)"));
        // not 은 뒤집는다 (container-type 은 이제 지원하므로 not 은 거짓)
        assert!(!supports_condition("not (container-type: inline-size)"));
        assert!(supports_condition("not (unknown-prop: 1px)"));
    }

    #[test]
    fn type_selectors_are_case_insensitive() {
        // HTML 의 타입 선택자는 ASCII 대소문자 구분이 없다(선택자 표준 §6.1).
        // 예전엔 `DIV { … }` 이 조용히 아무것도 매칭하지 않았다.
        let ss = crate::css::parse("DIV SPAN { color: #ff0000; }".to_string());
        match &ss.rules[0].selectors[0] {
            crate::css::Selector::Complex(parts) => {
                assert_eq!(parts[0].1.tag_name.as_deref(), Some("div"), "소문자로 정규화");
                assert_eq!(parts[1].1.tag_name.as_deref(), Some("span"));
            }
            other => panic!("복합 선택자를 기대: {:?}", other),
        }
    }

    #[test]
    fn supports_checks_values_not_just_property_names() {
        // 프로퍼티 이름만 보면 열거형의 미구현 값이 전부 지원으로 보고된다.
        // sticky 는 이제 실제로 구현했으므로 참이다 (구현하기 전엔 거짓이었다 —
        // 못 그리는 걸 지원한다고 하면 사이트가 폴백을 줄 기회를 스스로 없앤다).
        assert!(supports_condition("(position: sticky)"));
        assert!(supports_condition("(position: absolute)"));
        assert!(supports_condition("(position: fixed)"));
        assert!(!supports_condition("(position: running)"), "미구현 값은 거짓");

        // display: 우리가 실제로 레이아웃하는 값만 참. table 계열은 layout_table 이 렌더하므로
        // 참, flow-root/list-item 은 아직 전용 레이아웃이 없어(block 폴백) 거짓.
        assert!(supports_condition("(display: contents)"));
        assert!(supports_condition("(display: grid)"));
        assert!(supports_condition("(display: table-cell)"));
        assert!(!supports_condition("(display: flow-root)"));
        assert!(!supports_condition("(display: list-item)"));

        // color-mix 도 이제 색공간에서 보간해 지원 → 참
        assert!(supports_condition("(color: color-mix(in srgb, red, blue))"));
        assert!(supports_condition("(color: color-mix(in oklch, red, blue))"));
        // lab/lch/oklab/oklch/color() 는 이제 계산값 색공간을 보존하므로 지원 → 참
        assert!(supports_condition("(color: oklch(0.7 0.1 200))"));
        assert!(supports_condition("(color: lab(50 40 30))"));
        assert!(supports_condition("(color: color(display-p3 1 0 0))"));
        // env() 는 여전히 미지원
        assert!(!supports_condition("(width: env(safe-area-inset-left))"));
        // transform: 2D 함수는 전부 행렬로 합성해 실제로 변환한다
        assert!(supports_condition("(transform: rotate(45deg))"));
        assert!(supports_condition("(transform: translateX(10px))"));
        assert!(supports_condition("(transform: matrix(1, 0, 0, 1, 5, 5))"));
        // 3D 도 이제 4x4 행렬로 계산·직렬화(matrix3d)하고 보간까지 하므로 지원 → 참.
        // (translateZ/scaleZ/rotateX·Y·Z/perspective/matrix3d 모두 정확한 계산값을 낸다.)
        assert!(supports_condition("(transform: rotate3d(0, 1, 0, 45deg))"));
        assert!(supports_condition("(transform: perspective(500px))"));
        assert!(supports_condition("(transform: scaleZ(2))"));

        // 구현된 함수는 참
        assert!(supports_condition("(width: calc(100% - 10px))"));
        assert!(supports_condition("(width: min(100%, 40rem))"));
        assert!(supports_condition("(color: rgb(1 2 3))"));

        // 전역 키워드는 어디서나 유효
        assert!(supports_condition("(display: inherit)"));
    }

    #[test]
    fn supports_important_flag_is_ignored() {
        // @supports (prop: value !important) 은 플래그를 무시하고 값만 본다 (CSS Cond §2.1).
        assert!(supports_condition("(display: block !important)"));
        assert!(supports_condition("(color: red !important)"));
        // 값 자체가 미구현이면 !important 여도 거짓(flow-root 는 전용 레이아웃 없음)
        assert!(!supports_condition("(display: flow-root !important)"));
    }

    #[test]
    fn supports_functional_selector_font() {
        // selector(): 엔진이 매칭하는 표준 셀렉터는 참
        assert!(supports_condition("selector(a)"));
        assert!(supports_condition("selector(p a)"));
        assert!(supports_condition("selector(p > a)"));
        assert!(supports_condition("selector(p + a)"));
        assert!(supports_condition("(selector(div.x))"));
        // 파싱 실패하는 셀렉터는 거짓
        assert!(!supports_condition("selector(!!!)"));
        // font-tech(): 알려진 값만 참 (대소문자 무시)
        assert!(supports_condition("font-tech(color-COLRv1)"));
        assert!(supports_condition("font-tech(variations)"));
        assert!(!supports_condition("font-tech(invalid)"));
        // font-format(): 알려진 값만 참
        assert!(supports_condition("font-format(opentype)"));
        assert!(supports_condition("font-format(woff)"));
        assert!(!supports_condition("font-format(invalid)"));
    }
}
