mod media;
mod shorthand;
mod supports;
mod values;

pub(crate) use media::{container_matches, media_matches, media_matches_vp};
pub(crate) use supports::SUPPORTED;

// 단축(shorthand) → 롱핸드 이름들. 확장기에게 직접 물어본다 — 프로퍼티마다 목록을 손으로
// 적지 않는다. **한 번만** 만든다: 요소마다 다시 물어보면 (프로퍼티 150개 × 요소 수)만큼
// 값 파싱이 돈다 — react.dev 에서 그것만 6초를 먹었다.
static SHORTHANDS: std::sync::OnceLock<std::collections::HashMap<&'static str, Vec<String>>> =
    std::sync::OnceLock::new();

pub(crate) fn shorthand_table() -> &'static std::collections::HashMap<&'static str, Vec<String>> {
    SHORTHANDS.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        for p in SUPPORTED {
            let probe = shorthand::expand_declaration(p, "0px");
            // 자기 자신으로만 펼쳐지면 롱핸드다
            if probe.len() <= 1 && probe.first().map(|d| d.name.as_str()) == Some(*p) {
                continue;
            }
            let longs: Vec<String> =
                probe.into_iter().map(|d| d.name).filter(|n| n != p).collect();
            if !longs.is_empty() {
                m.insert(*p, longs);
            }
        }
        m
    })
}
use shorthand::expand_declaration;
pub(crate) use supports::supports_condition;
use values::valid_identifier_char;

#[derive(Debug, PartialEq)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
    // @layer 선언 순서. 뒤 레이어가 이긴다 (일반 선언), !important 는 **역순** (표준 §6.4.4).
    // 레이어 없는 선언은 모든 레이어보다 세다 (important 면 반대로 가장 약하다).
    pub layers: Vec<String>,
    pub font_faces: Vec<FontFace>,
    // @keyframes 이름 → 최종(100%/to) 프레임 선언. 정적 렌더는 애니메이션 종료 상태를 적용.
    pub keyframes: std::collections::HashMap<String, Vec<(String, Value)>>,
}

impl Stylesheet {
    // @container 규칙이 하나라도 있는가 (있을 때만 두 번째 스타일 패스를 돈다)
    pub fn has_containers(&self) -> bool {
        self.rules.iter().any(|r| r.container.is_some())
    }

    // 다른 시트를 뒤에 합친다. 규칙뿐 아니라 **레이어 순서와 @font-face/@keyframes 도**
    // 합쳐야 한다 — 규칙만 옮기면 그 시트의 @layer 가 순위 0(레이어 밖)으로 떨어져
    // 캐스케이드가 뒤집힌다.
    pub fn merge(&mut self, other: Stylesheet) {
        for l in other.layers {
            if !self.layers.contains(&l) {
                self.layers.push(l);
            }
        }
        self.rules.extend(other.rules);
        self.font_faces.extend(other.font_faces);
        self.keyframes.extend(other.keyframes);
    }
}

// @font-face 규칙: 패밀리 이름 + src URL 목록(우선순위 순).
#[derive(Debug, PartialEq, Clone)]
pub struct FontFace {
    pub family: String,
    pub srcs: Vec<String>,
    // unicode-range: 이 face 가 덮는 코드포인트 구간들 (비면 전체).
    // **이걸 무시하면 서브셋 폰트를 전부 받는다** — Google 폰트 CSS 는 스크립트마다
    // 수백 개 서브셋을 선언한다 (developer.chrome.com 에서 1240개를 받고 있었다).
    // 브라우저는 문서에 실제로 나오는 문자를 덮는 face 만 받는다.
    pub unicode_range: Vec<(u32, u32)>,
}

impl FontFace {
    // 이 face 가 주어진 문자 집합 중 하나라도 덮는가 (범위가 없으면 항상 참).
    pub fn covers_any(&self, chars: &std::collections::HashSet<u32>) -> bool {
        if self.unicode_range.is_empty() {
            return true;
        }
        chars
            .iter()
            .any(|&c| self.unicode_range.iter().any(|&(a, b)| c >= a && c <= b))
    }
}

// "U+0-7F, U+30??, U+4E00-9FFF" → [(0,0x7f), (0x3000,0x30ff), (0x4e00,0x9fff)]
pub fn parse_unicode_range(s: &str) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let p = part.trim();
        let Some(rest) = p.strip_prefix("U+").or_else(|| p.strip_prefix("u+")) else { continue };
        if let Some((a, b)) = rest.split_once('-') {
            // U+4E00-9FFF
            if let (Ok(a), Ok(b)) =
                (u32::from_str_radix(a.trim(), 16), u32::from_str_radix(b.trim(), 16))
            {
                out.push((a, b));
            }
        } else if rest.contains('?') {
            // U+30?? → 3000..30FF (와일드카드)
            let lo = rest.replace('?', "0");
            let hi = rest.replace('?', "F");
            if let (Ok(a), Ok(b)) =
                (u32::from_str_radix(&lo, 16), u32::from_str_radix(&hi, 16))
            {
                out.push((a, b));
            }
        } else if let Ok(a) = u32::from_str_radix(rest.trim(), 16) {
            out.push((a, a));
        }
    }
    out
}

#[derive(Debug, PartialEq)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
    // 이 규칙이 속한 @layer 이름 (없으면 레이어 밖).
    pub layer: Option<String>,
    // @container 조건 (컨테이너 이름, 조건문). 조건은 **레이아웃 후** 컨테이너의 실제
    // 크기로 평가한다 — 스타일 시점엔 아직 모른다.
    pub container: Option<(String, String)>,
    // UA(브라우저 기본) 스타일에서 온 규칙인가. `revert` 는 저자 선언을 되돌려
    // UA 원점 값으로 계산해야 하므로 원점을 구분해야 한다 (CSS Cascade §6.2).
    pub ua: bool,
}

#[derive(Debug, PartialEq, Clone)]
pub enum Selector {
    Simple(SimpleSelector),
    // 결합자 체인: [(결합자, 단순), ...]. 첫 항목의 결합자는 무시(대상 기준).
    // 예: ".a > .b" → [(Descendant, .a), (Child, .b)]. 마지막이 대상.
    Complex(Vec<(Combinator, SimpleSelector)>),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Combinator {
    Descendant,   // 공백
    Child,        // >
    NextSibling,  // +
    LaterSibling, // ~
}

// 의사 클래스. 구조적(위치)과 동적(상호작용) 구분.
#[derive(Debug, PartialEq, Clone)]
pub enum Pseudo {
    FirstChild,
    LastChild,
    OnlyChild,
    NthChild(i32, i32),      // an+b (앞에서)
    NthLastChild(i32, i32),  // an+b (뒤에서)
    NthOfType(i32, i32),     // 같은 타입 중 an+b (앞에서)
    NthLastOfType(i32, i32), // 같은 타입 중 an+b (뒤에서)
    OnlyOfType,              // 같은 타입 형제가 자기 하나뿐
    Not(Vec<SimpleSelector>),
    Is(Vec<SimpleSelector>),    // :is()/:matches() — 인자 중 하나라도 매칭 (특이도=인자 최대)
    Where(Vec<SimpleSelector>), // :where() — 매칭은 :is 와 같되 특이도 0
    Root,
    Empty,
    // 폼 상태 (요소 속성으로 정적 판별)
    Checked,  // checkbox/radio[checked], option[selected]
    Disabled, // 폼 요소[disabled]
    Enabled,  // 폼 요소 && !disabled
    Required, // 폼 필드[required]
    Optional, // 폼 필드 && !required
    Link,     // :link — href 있는 a/area/link (정적 렌더에선 방문 이력 없어 모든 링크가 매칭)
    // :has(상대선택자 목록) — 자손/뒤형제를 본다. 요소 하나로는 판정할 수 없는 유일한
    // 의사 클래스라, 매칭 시 앵커의 DOM 위치가 필요하다.
    // 각 항목은 (선행 결합자, 선택자). ":has(.a)" 는 Descendant, ":has(> .a)" 는 Child.
    Has(Vec<(Combinator, Selector)>),
    Dynamic,  // hover/focus/active/visited 등 — 정적 렌더에선 비매칭
}

// 속성 선택자 연산자.
#[derive(Debug, PartialEq, Clone)]
pub enum AttrOp {
    Exists,           // [attr]
    Equals(String),   // [attr=v]
    Prefix(String),   // [attr^=v]
    Suffix(String),   // [attr$=v]
    Contains(String), // [attr*=v]
    Word(String),     // [attr~=v] (공백 구분 목록에 v)
    Dash(String),     // [attr|=v] (v 또는 v-...)
}

// 의사 요소 (생성 콘텐츠). ::before / ::after 만 지원.
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum PseudoElement {
    Before,
    After,
}

#[derive(Debug, PartialEq, Clone)]
pub struct SimpleSelector {
    pub tag_name: Option<String>,
    pub id: Option<String>,
    pub class: Vec<String>,
    // 속성 선택자: (이름, 연산자, 대소문자무시). ci=true 는 [attr=v i] 플래그.
    pub attrs: Vec<(String, AttrOp, bool)>,
    pub pseudos: Vec<Pseudo>,
    // ::before / ::after — 대상 요소 자체가 아니라 생성 박스를 지정.
    pub pseudo_element: Option<PseudoElement>,
}

#[derive(Debug, PartialEq)]
pub struct Declaration {
    pub name: String,
    pub value: Value,
    pub important: bool, // !important — 캐스케이드에서 일반 선언을 이긴다
}

// 값에서 후행 `!important`(대소문자 무시, `! important` 공백 허용)를 분리.
// → (실제 값, important 여부).
fn split_important(value: &str) -> (&str, bool) {
    let t = value.trim_end();
    let lower = t.to_ascii_lowercase();
    if let Some(rest) = lower.strip_suffix("important") {
        let idx = rest.len(); // "important" 시작 바이트 오프셋 (ASCII 소문자화라 길이 동일)
        let before = t[..idx].trim_end();
        if let Some(v) = before.strip_suffix('!') {
            return (v.trim_end(), true);
        }
    }
    (value, false)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Keyword(String),
    Length(f32, Unit),
    Color(Color),
    Url(String),
    // var() 를 포함한 미해석 원문. 스타일 계산 시 커스텀 프로퍼티로 치환 후 재파싱.
    Var(String),
    // calc() 를 단위별 계수 선형식으로 축약. em/rem/vw 등 문맥 단위는 style(resolve_units)
    // 에서 px 로 접고, %는 레이아웃(len_px)까지 보존해 컨테이닝 블록 기준으로 해석.
    Calc(CalcSum),
    // linear-gradient. 페인트가 축을 따라 색 보간.
    Gradient(Gradient),
    // min()/max()/clamp() — 인자는 Length/Calc. 스타일에서 em/rem/vw 를 px 로 확정하고
    // 레이아웃(len_px)이 % 해석 후 최종 계산한다.
    MinMax(MinMaxKind, Vec<Value>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MinMaxKind {
    Min,
    Max,
    Clamp,
}

// calc() 의 선형 합. 단위별 계수를 따로 들고, resolve_units 가 문맥 단위(em/rem/vw…)
// 를 px 로 접은 뒤 px 하나로 합쳐 둔다. %(pct)만 레이아웃까지 남아 컨테이닝 블록
// 폭 기준으로 해석된다. 각 계수는 해당 단위 값의 합(예: 1rem+2rem → rem=3).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CalcSum {
    pub pct: f32,
    pub px: f32,
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
    pub vmin: f32,
    pub vmax: f32,
}

impl CalcSum {
    // 문맥 단위가 하나라도 남아 있으면 아직 px 로 확정 못 함(style 필요).
    pub fn has_ctx_units(&self) -> bool {
        self.em != 0.0
            || self.rem != 0.0
            || self.vw != 0.0
            || self.vh != 0.0
            || self.vmin != 0.0
            || self.vmax != 0.0
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gradient {
    pub angle_deg: f32, // CSS 각도 (0=위, 90=오른쪽, 180=아래). radial/conic 이면 무시.
    pub radial: bool,   // true=radial(중심에서 방사)
    pub circle: bool,   // radial 일 때 true=circle(원), false=ellipse(기본)
    pub conic: bool,    // true=conic(중심 기준 각도 스윕)
    // repeating-* 그라디언트: 스톱 구간을 반복한다. 예전엔 이 함수 이름을 몰라서
    // 값 파싱이 실패했고 → **배경이 통째로 안 그려졌다** (조용히 사라졌다).
    pub repeating: bool,
    pub stops: Vec<(Color, StopPos)>,
}

// 색 스톱의 위치. px 는 그라디언트 선 길이를 알아야 풀리므로 **페인트 시점까지** 남긴다.
// 예전엔 % 만 읽고 px 는 통째로 무시했다 — linear-gradient(#fff 0, #fff 10px, transparent 10px)
// 처럼 흔한 패턴이 균등 분배로 뭉개졌다 (조용히 틀린 그림).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StopPos {
    Auto,     // 위치 미지정 → 이웃 사이로 균등 보간 (표준)
    Pct(f32), // 0..1
    Px(f32),
    Deg(f32), // conic 전용 (0..360 → 0..1 로 정규화해서 저장)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Unit {
    Px,
    Em,      // 부모 font-size 배수
    Rem,     // 루트 font-size 배수
    Percent, // 문맥 의존 (현재 font-size 에서만 해석)
    Vw,      // 뷰포트 폭 1%
    Vh,      // 뷰포트 높이 1%
    Vmin,    // min(vw, vh)
    Vmax,    // max(vw, vh)
    // line-height 전용: 단위 없는 배수(factor). px 로 확정하지 않고 상속되어,
    // 각 요소가 자기 font-size 를 곱해 쓴다(CSS2 §10.8). 길이/%와 상속 방식이 다름.
    Lh,
    // 단위 없는 <number> — opacity/z-index/order/flex-grow/flex-shrink/font-weight.
    // 예전엔 이것들을 Length(n, Px) 로 실었다. 동작은 했지만(레이아웃이 to_px 로 읽음)
    // getComputedStyle 이 "0.5px"/"5px"/"1px" 같은 거짓 값을 돌려줬다 — 이 값들은
    // CSS 상 길이가 아니라 수다.
    Number,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

pub type Specificity = (usize, usize, usize);

impl Selector {
    // 대상(가장 오른쪽) 단순 선택자
    pub fn subject(&self) -> &SimpleSelector {
        match self {
            Selector::Simple(s) => s,
            Selector::Complex(v) => &v.last().unwrap().1,
        }
    }

    fn each_simple(&self) -> Vec<&SimpleSelector> {
        match self {
            Selector::Simple(s) => vec![s],
            Selector::Complex(v) => v.iter().map(|(_, s)| s).collect(),
        }
    }

    pub fn specificity(&self) -> Specificity {
        let (mut a, mut b, mut c) = (0usize, 0usize, 0usize);
        for s in self.each_simple() {
            let (sa, sb, sc) = simple_specificity(s);
            a += sa;
            b += sb;
            c += sc + s.pseudo_element.iter().count(); // 의사 요소 = tag 레벨
        }
        (a, b, c)
    }
}

// 단순 선택자 하나의 특이도. 의사 클래스는 종류별로: :where=0, :is/:not=인자 최대,
// 그 외(구조적/폼/동적)=클래스 1. id=a, class/attr/pseudo=b, tag=c.
fn simple_specificity(s: &SimpleSelector) -> Specificity {
    let mut a = s.id.iter().count();
    let mut b = s.class.len() + s.attrs.len();
    let mut c = s.tag_name.iter().count();
    for p in &s.pseudos {
        let (pa, pb, pc) = pseudo_specificity(p);
        a += pa;
        b += pb;
        c += pc;
    }
    (a, b, c)
}

fn pseudo_specificity(p: &Pseudo) -> Specificity {
    match p {
        Pseudo::Where(_) => (0, 0, 0), // :where() 는 특이도에 기여 안 함
        // :is()/:not() 는 인자 중 가장 특이한 것의 특이도
        Pseudo::Is(args) | Pseudo::Not(args) => {
            args.iter().map(simple_specificity).max().unwrap_or((0, 0, 0))
        }
        // :has() 도 인자 중 가장 특이한 것 (표준 §17)
        Pseudo::Has(rels) => rels
            .iter()
            .map(|(_, s)| s.specificity())
            .max()
            .unwrap_or((0, 0, 0)),
        _ => (0, 1, 0), // 그 외 의사 클래스 = 클래스 하나
    }
}

impl Value {
    pub fn to_px(&self) -> f32 {
        match self {
            // Number 는 단위 없는 수 — 레이아웃이 스칼라로 읽는다(flex-grow/order 등)
            Value::Length(f, Unit::Px) | Value::Length(f, Unit::Number) => *f,
            // 문맥 없는 to_px: %는 0 기준 (대개 인자가 이미 px 로 확정된 min/max/clamp)
            Value::MinMax(kind, args) => eval_minmax(*kind, args, 0.0),
            _ => 0.0,
        }
    }
}

// min/max/clamp 를 px 로 계산. pct_base 는 % 인자 해석 기준(레이아웃이 제공, 없으면 0).
pub fn eval_minmax(kind: MinMaxKind, args: &[Value], pct_base: f32) -> f32 {
    let arg_px = |v: &Value| -> f32 {
        match v {
            Value::Length(f, Unit::Px) => *f,
            Value::Length(f, Unit::Percent) => f / 100.0 * pct_base,
            // 문맥 단위는 style 에서 px 로 접혔어야 함. 남은 건 pct + px.
            Value::Calc(c) => c.pct / 100.0 * pct_base + c.px,
            Value::MinMax(k, a) => eval_minmax(*k, a, pct_base),
            _ => 0.0, // 미해석 단위(em/rem/vw 는 style 에서 px 로 확정됐어야)
        }
    };
    let vals: Vec<f32> = args.iter().map(arg_px).collect();
    if vals.is_empty() {
        return 0.0;
    }
    match kind {
        MinMaxKind::Min => vals.iter().cloned().fold(f32::INFINITY, f32::min),
        MinMaxKind::Max => vals.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
        // clamp(min, val, max) = max(min, min(val, max))
        MinMaxKind::Clamp => {
            let lo = vals[0];
            let val = vals.get(1).copied().unwrap_or(lo);
            let hi = vals.get(2).copied().unwrap_or(val);
            val.min(hi).max(lo)
        }
    }
}

// 단일 길이 토큰을 px 로 (px/절대단위만; em/rem/%/뷰포트 단위는 문맥 필요 → None).
// transform: translate() 등 문맥 없는 파싱에 쓴다.
pub fn parse_len_px(tok: &str) -> Option<f32> {
    match values::interpret_value(tok.trim()) {
        Some(Value::Length(v, Unit::Px)) => Some(v),
        _ => None,
    }
}

// @keyframes 프레임 셀렉터의 최대 offset (0..1). "100%"/"to" → 1.0, "from"/"0%" → 0.
fn frame_offset(sel: &str) -> f32 {
    let mut max = 0.0f32;
    for part in sel.split(',') {
        let p = part.trim().to_ascii_lowercase();
        let o = if p == "to" {
            1.0
        } else if p == "from" {
            0.0
        } else if let Some(n) = p.strip_suffix('%') {
            n.trim().parse::<f32>().map(|v| v / 100.0).unwrap_or(0.0)
        } else {
            0.0
        };
        max = max.max(o);
    }
    max
}

// 텍스트에서 url(...) 안의 URL 들을 순서대로 추출 (따옴표 제거). @font-face src 용.
fn extract_urls(text: &str, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(p) = rest.find("url(") {
        let after = &rest[p + 4..];
        if let Some(end) = after.find(')') {
            let u = after[..end].trim().trim_matches(|c| c == '"' || c == '\'');
            if !u.is_empty() {
                out.push(u.to_string());
            }
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
}

// 색 문자열(hex/named/rgb 등) → Color. SVG fill/stroke 파싱 등에 쓴다.
pub fn parse_color(s: &str) -> Option<Color> {
    match values::interpret_value(s.trim()) {
        Some(Value::Color(c)) => Some(c),
        _ => None,
    }
}

// @media 없는 시트/테스트/UA 용 기본 파스. 데스크톱 폭(1024)으로 미디어 평가.
pub fn parse(source: String) -> Stylesheet {
    parse_viewport(source, 1024.0)
}

// CSS 주석 /* ... */ 제거. 문자열(따옴표) 안은 보존. 토큰 붙음 방지로 공백 치환.
// 미압축 스타일시트(문서/개발 사이트)엔 주석이 흔해, 없으면 선언이 통째로 유실된다.
pub fn strip_comments(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match quote {
            Some(q) => {
                out.push(c);
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '/' && chars.peek() == Some(&'*') {
                    chars.next(); // '*'
                    let mut prev = ' ';
                    for cc in chars.by_ref() {
                        if prev == '*' && cc == '/' {
                            break;
                        }
                        prev = cc;
                    }
                    out.push(' ');
                } else {
                    if c == '"' || c == '\'' {
                        quote = Some(c);
                    }
                    out.push(c);
                }
            }
        }
    }
    out
}

// 뷰포트 폭을 알고 파스 — @media (min/max-width) 를 이 폭에 대해 평가해
// 매칭되는 규칙만 포함한다. 페이지 스타일시트는 실제 뷰포트 폭으로 호출.
pub fn parse_viewport(source: String, viewport_width: f32) -> Stylesheet {
    let mut parser = Parser {
        pos: 0,
        input: strip_comments(&source),
        viewport_width,
        font_faces: Vec::new(),
        keyframes: std::collections::HashMap::new(),
        layers: Vec::new(),
        cur_container: None,
        cur_layer: None,
        anon_count: 0,
    };
    let rules = parser.parse_rules();
    Stylesheet {
        rules,
        layers: parser.layers,
        font_faces: parser.font_faces,
        keyframes: parser.keyframes,
    }
}

// 인라인 style="..." 속성값(선언 블록, 중괄호 없음)을 선언 목록으로 파싱.
// 캐스케이드에서 어떤 선택자보다 높은 우선순위 (스타일 적용 시 마지막에 얹음).
// nth 인자 파싱: "2n+1"/"odd"/"even"/"3"/"n"/"-n+3" → (a, b) 의 an+b.
fn parse_nth(s: &str) -> Option<(i32, i32)> {
    let s: String = s.trim().to_ascii_lowercase().split_whitespace().collect();
    match s.as_str() {
        "odd" => return Some((2, 1)),
        "even" => return Some((2, 0)),
        _ => {}
    }
    if let Some(np) = s.find('n') {
        let a = match &s[..np] {
            "" | "+" => 1,
            "-" => -1,
            a => a.parse().ok()?,
        };
        let b_str = s[np + 1..].trim_start_matches('+');
        let b = if b_str.is_empty() { 0 } else { b_str.parse().ok()? };
        Some((a, b))
    } else {
        Some((0, s.parse().ok()?))
    }
}

pub fn parse_inline_style(text: &str) -> Vec<Declaration> {
    let mut parser = Parser {
        pos: 0,
        input: strip_comments(text),
        viewport_width: 0.0,
        font_faces: Vec::new(),
        keyframes: std::collections::HashMap::new(),
        layers: Vec::new(),
        cur_container: None,
        cur_layer: None,
        anon_count: 0,
    };
    parser.parse_declarations()
}

// var() 참조를 커스텀 프로퍼티로 치환 → 재파싱해 확정 선언들을 낸다.
// custom: 요소의 계산된 커스텀 프로퍼티 맵(--name → 원문 값). 미해석이면 빈 Vec.
pub(crate) fn resolve_var(
    name: &str,
    raw: &str,
    custom: &std::collections::HashMap<String, String>,
) -> Vec<Declaration> {
    let substituted = substitute_var(raw, custom, 0);
    if substituted.contains("var(") {
        return Vec::new(); // 여전히 미해석(정의 안 됨 + fallback 없음) → 드롭
    }
    expand_declaration(name, substituted.trim())
}

// 문자열 안의 var(--name[, fallback]) 을 커스텀 프로퍼티 값으로 치환 (중첩 8단계까지).
fn substitute_var(raw: &str, custom: &std::collections::HashMap<String, String>, depth: u32) -> String {
    if depth > 8 || !raw.contains("var(") {
        return raw.to_string();
    }
    let mut out = String::new();
    let mut rest = raw;
    while let Some(pos) = rest.find("var(") {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 4..];
        // 괄호 짝 찾기
        let mut depth_p = 1i32;
        let mut end = after.len();
        for (i, c) in after.char_indices() {
            match c {
                '(' => depth_p += 1,
                ')' => {
                    depth_p -= 1;
                    if depth_p == 0 {
                        end = i;
                        break;
                    }
                }
                _ => {}
            }
        }
        let inner = &after[..end];
        // "--name" 또는 "--name, fallback"
        let (var_name, fallback) = match inner.find(',') {
            Some(ci) => (inner[..ci].trim(), Some(inner[ci + 1..].trim())),
            None => (inner.trim(), None),
        };
        let resolved = match custom.get(var_name) {
            Some(v) => substitute_var(v, custom, depth + 1),
            None => match fallback {
                Some(f) => substitute_var(f, custom, depth + 1),
                None => {
                    // 미해석 표시(재파싱에서 드롭되게 var( 유지)
                    out.push_str("var(");
                    out.push_str(inner);
                    out.push(')');
                    rest = &after[end + 1..];
                    continue;
                }
            },
        };
        out.push_str(&resolved);
        rest = &after[(end + 1).min(after.len())..];
    }
    out.push_str(rest);
    out
}

// UA 기본 스타일시트. HTML 표준 §15 Rendering 을 근거로 함
// (https://html.spec.whatwg.org/multipage/rendering.html). 표준은 폼 컨트롤을
// appearance:auto(네이티브 위젯)로 두지만, 우리는 appearance 미구현이라 기본
// 테두리/배경을 여기 CSS 로 넣는다 — 캐스케이드상 저작자 CSS 가 덮을 수 있어
// 하드코딩(무조건 그림)과 달리 구글 등의 커스텀 스타일이 이긴다.
// 테이블 계열은 진짜 테이블 레이아웃 전까지 block 으로 근사(레이아웃은 tr 태그로 분기).
const UA_CSS: &str = "html, body, div, p, h1, h2, h3, h4, h5, h6, ul, ol, li, section, article, header, footer, nav, main, aside, blockquote, pre, center, form, fieldset, hr, figure, figcaption, address, dl, dt, dd, select, textarea { display: block; } table { display: table; } thead { display: table-header-group; } tbody { display: table-row-group; } tfoot { display: table-footer-group; } tr { display: table-row; } td, th { display: table-cell; } caption { display: table-caption; } head, script, style, title, meta, link, noscript, template { display: none; } img { display: inline-block; } a { color: #0645ad; text-decoration: underline; } details, summary, figure, figcaption { display: block; } summary { font-weight: bold; } details:not([open]) > :not(summary) { display: none; } [hidden] { display: none; } dialog { display: block; } dialog:not([open]) { display: none; } mark { background-color: #ffff00; } del, s, strike { text-decoration: line-through; } ins, u { text-decoration: underline; } small { font-size: 13px; } sub, sup { font-size: 12px; } code, kbd, samp, tt { font-family: monospace; } pre, xmp, listing, plaintext { white-space: pre; font-family: monospace; } textarea { white-space: pre-wrap; } nobr { white-space: nowrap; } abbr { text-decoration: underline; } ul, ol { padding-left: 24px; } li { padding-left: 18px; } td, th { padding: 4px 6px; } th { color: #202020; } center { text-align: center; } table { text-align: left; } input, button, progress, meter { display: inline-block; } input, textarea, select { border: 1px solid #767676; background-color: #ffffff; padding: 2px 5px; } button, input[type=submit], input[type=reset], input[type=button] { border: 1px solid #767676; background-color: #e9e9ed; padding: 2px 8px; text-align: center; } b, strong, h1, h2, h3, h4, h5, h6, th { font-weight: bold; } i, em, cite, var, address { font-style: italic; } hr { border-top: 1px solid #a0a0a0; margin: 8px 0; height: 0; box-sizing: content-box; } blockquote { margin: 8px 40px; } figure { margin: 16px 40px; } dl { margin: 8px 0; } dd { margin-left: 40px; } menu, dir { display: block; padding-left: 24px; } p { margin: 1em 0; } h1 { font-size: 2em; margin: 0.67em 0; } h2 { font-size: 1.5em; margin: 0.83em 0; } h3 { font-size: 1.17em; margin: 1em 0; } h4 { margin: 1.33em 0; } h5 { font-size: 0.83em; margin: 1.67em 0; } h6 { font-size: 0.67em; margin: 2.33em 0; } ul, ol { margin: 1em 0; } ul ul, ul ol, ol ul, ol ol { margin-top: 0; margin-bottom: 0; } pre { margin: 1em 0; } q::before { content: open-quote; } q::after { content: close-quote; }";

pub fn user_agent_stylesheet() -> Stylesheet {
    let mut ss = parse(UA_CSS.to_string());
    for r in &mut ss.rules {
        r.ua = true; // 원점 표시 (revert 가 여기로 되돌린다)
    }
    ss
}

// querySelector 용: 선택자 목록만 파싱. 빈 규칙 몸통을 붙여 기존 파서를 재사용.
// 미지원 선택자(:hover, > 등)면 None (관용).
pub fn parse_selector_list(text: &str) -> Option<Vec<Selector>> {
    let ss = parse(format!("{} {{}}", text));
    ss.rules.into_iter().next().map(|r| r.selectors)
}

struct Parser {
    pos: usize,
    input: String,
    viewport_width: f32,
    font_faces: Vec<FontFace>,
    keyframes: std::collections::HashMap<String, Vec<(String, Value)>>,
    // @layer 선언 순서 (이름). 익명 레이어는 "\u{0}anon<N>" 로 유일한 이름을 준다.
    layers: Vec<String>,
    // 지금 파싱 중인 @container 블록 (이름, 조건)
    cur_container: Option<(String, String)>,
    // 지금 파싱 중인 @layer 블록 이름 (중첩 시 "a.b")
    cur_layer: Option<String>,
    anon_count: usize,
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
                self.consume_char(); // '@'
                let ident = self.parse_identifier().to_ascii_lowercase();
                if ident == "media" {
                    let media_rules = self.parse_media_block();
                    rules.extend(media_rules);
                } else if ident == "supports" {
                    let supported = self.parse_supports_block();
                    rules.extend(supported);
                } else if ident == "font-face" {
                    if let Some(ff) = self.parse_font_face() {
                        self.font_faces.push(ff);
                    }
                } else if ident == "keyframes" || ident == "-webkit-keyframes" {
                    self.parse_keyframes();
                } else if ident == "layer" {
                    rules.extend(self.parse_layer());
                } else if ident == "container" {
                    rules.extend(self.parse_container());
                } else {
                    self.skip_at_rule(); // 그 외 @rule 은 스킵 (';' or {block})
                }
                continue;
            }
            if let Some(rule) = self.parse_rule() {
                rules.push(rule);
            }
        }
        rules
    }

    // '@media' 뒤: 조건 텍스트 → '{' → 내부 규칙들 → '}'. 조건이 뷰포트에 맞으면
    // 내부 규칙을 반환, 아니면 빈 목록. (내부 규칙은 항상 파싱해 위치를 넘겨야 함)
    fn parse_media_block(&mut self) -> Vec<Rule> {
        let query = self.consume_while(|c| c != '{' && c != ';' && c != '}');
        if self.peek() != Some('{') {
            if self.peek() == Some(';') {
                self.consume_char();
            }
            return Vec::new();
        }
        self.consume_char(); // '{'
        let mut inner = Vec::new();
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                Some('@') => self.skip_at_rule(), // 중첩 @rule 은 스킵
                _ => {
                    if let Some(r) = self.parse_rule() {
                        inner.push(r);
                    }
                }
            }
        }
        if media_matches(&query, self.viewport_width) {
            inner
        } else {
            Vec::new()
        }
    }

    // '@layer a, b;'  (순서만 선언)  또는  '@layer name { rules }'  또는  '@layer { rules }'.
    // 레이어를 모르면 그 안의 규칙이 통째로 사라진다 — Tailwind v4 는 **모든 것**을
    // @layer 로 감싼다. 즉 스타일이 전부 날아간다.
    fn parse_layer(&mut self) -> Vec<Rule> {
        let head = self.consume_while(|c| c != '{' && c != ';' && c != '}');
        let names: Vec<String> = head
            .split(',')
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .collect();
        // '@layer a, b;' — 순서만 선언한다
        if self.peek() == Some(';') {
            self.consume_char();
            for n in names {
                self.register_layer(&n);
            }
            return Vec::new();
        }
        if self.peek() != Some('{') {
            return Vec::new();
        }
        self.consume_char(); // '{'
        // 이름 없는 블록은 익명 레이어 (고유 이름)
        let name = match names.first() {
            Some(n) => n.clone(),
            None => {
                self.anon_count += 1;
                format!("\u{0}anon{}", self.anon_count)
            }
        };
        // 중첩: 부모 레이어가 있으면 "부모.자식"
        let full = match &self.cur_layer {
            Some(p) => format!("{}.{}", p, name),
            None => name,
        };
        self.register_layer(&full);
        let prev = self.cur_layer.replace(full);

        let mut inner = Vec::new();
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                Some('@') => {
                    self.consume_char();
                    let id = self.parse_identifier().to_ascii_lowercase();
                    match id.as_str() {
                        "layer" => inner.extend(self.parse_layer()),
                        "media" => inner.extend(self.parse_media_block()),
                        "supports" => inner.extend(self.parse_supports_block()),
                        _ => self.skip_at_rule(),
                    }
                }
                _ => {
                    if let Some(r) = self.parse_rule() {
                        inner.push(r);
                    }
                }
            }
        }
        self.cur_layer = prev;
        inner
    }

    // '@container [이름] (조건) { rules }'. 조건은 스타일 시점에 평가할 수 없다
    // (컨테이너의 폭은 레이아웃이 정한다) → 규칙에 조건을 달아 두고 레이아웃 뒤에 판정한다.
    // 예전엔 통째로 스킵해서 그 안의 규칙이 **소리 없이 사라졌다**.
    fn parse_container(&mut self) -> Vec<Rule> {
        let head = self.consume_while(|c| c != '{' && c != ';' && c != '}').trim().to_string();
        if self.peek() != Some('{') {
            if self.peek() == Some(';') {
                self.consume_char();
            }
            return Vec::new();
        }
        self.consume_char(); // '{'
        // "이름 (조건)" 또는 "(조건)" — 첫 '(' 앞이 이름
        let (name, cond) = match head.find('(') {
            Some(i) => (head[..i].trim().to_string(), head[i..].to_string()),
            None => (head.clone(), String::new()),
        };
        let prev = self.cur_container.replace((name, cond));
        let mut inner = Vec::new();
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                Some('@') => {
                    self.consume_char();
                    let id = self.parse_identifier().to_ascii_lowercase();
                    match id.as_str() {
                        "container" => inner.extend(self.parse_container()),
                        "media" => inner.extend(self.parse_media_block()),
                        "supports" => inner.extend(self.parse_supports_block()),
                        "layer" => inner.extend(self.parse_layer()),
                        _ => self.skip_at_rule(),
                    }
                }
                _ => {
                    if let Some(r) = self.parse_rule() {
                        inner.push(r);
                    }
                }
            }
        }
        self.cur_container = prev;
        inner
    }

    fn register_layer(&mut self, name: &str) {
        if !self.layers.iter().any(|l| l == name) {
            self.layers.push(name.to_string());
        }
    }

    // '@supports <condition> { rules }'. 조건이 참이면 내부 규칙 포함, 아니면 버림.
    // (내부 규칙은 항상 파싱해 위치를 넘긴다.)
    fn parse_supports_block(&mut self) -> Vec<Rule> {
        let cond = self.consume_while(|c| c != '{' && c != ';' && c != '}');
        if self.peek() != Some('{') {
            if self.peek() == Some(';') {
                self.consume_char();
            }
            return Vec::new();
        }
        self.consume_char(); // '{'
        let mut inner = Vec::new();
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                Some('@') => self.skip_at_rule(),
                _ => {
                    if let Some(r) = self.parse_rule() {
                        inner.push(r);
                    }
                }
            }
        }
        if supports_condition(cond.trim()) {
            inner
        } else {
            Vec::new()
        }
    }

    // '@font-face { font-family: ...; src: url(...) ...; }' → FontFace.
    // 블록 선언에서 font-family(따옴표 제거)와 src 의 url() 들을 추출.
    fn parse_font_face(&mut self) -> Option<FontFace> {
        self.consume_while(|c| c != '{' && c != ';' && c != '}');
        if self.peek() != Some('{') {
            if self.peek() == Some(';') {
                self.consume_char();
            }
            return None;
        }
        self.consume_char(); // '{' (parse_declarations 는 소비된 상태를 기대)
        let decls = self.parse_declarations();
        let mut family = String::new();
        let mut srcs = Vec::new();
        let mut unicode_range = Vec::new();
        for d in &decls {
            if d.name == "unicode-range" {
                // 값이 어떤 Value 로 파싱됐든 원문을 되살려 읽는다 (U+0-7F 는 색/길이가 아니다)
                let raw = crate::style::computed_value_string(&d.value);
                unicode_range = parse_unicode_range(&raw);
                if unicode_range.is_empty() {
                    if let Value::Keyword(k) = &d.value {
                        unicode_range = parse_unicode_range(k);
                    }
                }
            } else if d.name == "font-family" {
                if let Value::Keyword(f) = &d.value {
                    family = f.trim().trim_matches(|c| c == '"' || c == '\'').to_string();
                } else if let Value::Url(u) = &d.value {
                    family = u.clone();
                }
            } else if d.name == "src" {
                // src 원문에서 url() 추출 (Keyword/Url 로 저장됐을 수 있음)
                match &d.value {
                    Value::Url(u) => srcs.push(u.clone()),
                    Value::Keyword(raw) => extract_urls(raw, &mut srcs),
                    _ => {}
                }
            }
        }
        if family.is_empty() {
            return None;
        }
        Some(FontFace { family, srcs, unicode_range })
    }

    // '@keyframes name { 0%{...} 100%{...} }' — 최종(100%/to) 프레임 선언만 보관.
    // 정적 렌더는 애니메이션 완료 상태를 근사(진입 애니메이션의 숨김 초기상태 회피).
    fn parse_keyframes(&mut self) {
        self.consume_whitespace();
        let name = self.parse_identifier();
        self.consume_whitespace();
        if self.peek() != Some('{') {
            self.skip_at_rule();
            return;
        }
        self.consume_char(); // '{'
        let mut final_decls: Vec<(String, Value)> = Vec::new();
        let mut best_offset = -1.0f32;
        loop {
            self.consume_whitespace();
            match self.peek() {
                None => break,
                Some('}') => {
                    self.consume_char();
                    break;
                }
                _ => {
                    // 프레임 셀렉터 (0%, 100%, from, to, 콤마 목록) → 최대 offset 추출
                    let sel = self.consume_while(|c| c != '{' && c != '}');
                    let offset = frame_offset(&sel);
                    if self.peek() == Some('{') {
                        self.consume_char();
                        let decls = self.parse_declarations();
                        if offset >= best_offset {
                            best_offset = offset;
                            final_decls = decls.into_iter().map(|d| (d.name, d.value)).collect();
                        }
                    }
                }
            }
        }
        if !name.is_empty() && !final_decls.is_empty() {
            self.keyframes.insert(name, final_decls);
        }
    }

    fn parse_rule(&mut self) -> Option<Rule> {
        match self.parse_selectors() {
            Some(selectors) => {
                let declarations = self.parse_declarations();
                Some(Rule {
                    selectors,
                    declarations,
                    ua: false,
                    layer: self.cur_layer.clone(),
                    container: self.cur_container.clone(),
                })
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
            selectors.push(self.parse_complex_selector()?);
            match self.peek() {
                Some(',') => {
                    self.consume_char();
                    self.consume_whitespace();
                }
                Some('{') => {
                    self.consume_char();
                    break;
                }
                // 파싱 실패(미지원 구문 잔여) → 규칙 스킵
                _ => return None,
            }
        }
        selectors.sort_by(|a, b| b.specificity().cmp(&a.specificity()));
        Some(selectors)
    }

    // 결합자 체인: 단순 선택자를 공백/`>`/`+`/`~` 로 이음. 종료 후 peek 은 ','/'{'.
    fn parse_complex_selector(&mut self) -> Option<Selector> {
        let mut parts = vec![(Combinator::Descendant, self.parse_simple_selector()?)];
        loop {
            let had_ws = self.peek().map(|c| c.is_whitespace()).unwrap_or(false);
            self.consume_whitespace();
            let combinator = match self.peek() {
                Some('>') => {
                    self.consume_char();
                    self.consume_whitespace();
                    Combinator::Child
                }
                Some('+') => {
                    self.consume_char();
                    self.consume_whitespace();
                    Combinator::NextSibling
                }
                Some('~') => {
                    self.consume_char();
                    self.consume_whitespace();
                    Combinator::LaterSibling
                }
                Some(c)
                    if had_ws
                        && (c == '.'
                            || c == '#'
                            || c == '*'
                            || c == '['
                            || c == ':'
                            || valid_identifier_char(c)) =>
                {
                    Combinator::Descendant
                }
                _ => break,
            };
            parts.push((combinator, self.parse_simple_selector()?));
        }
        if parts.len() == 1 {
            Some(Selector::Simple(parts.pop().unwrap().1))
        } else {
            Some(Selector::Complex(parts))
        }
    }

    // 하나의 compound 선택자(태그/id/class/속성/의사클래스 조합). 없으면 None.
    fn parse_simple_selector(&mut self) -> Option<SimpleSelector> {
        let mut selector = SimpleSelector {
            tag_name: None,
            id: None,
            class: Vec::new(),
            attrs: Vec::new(),
            pseudos: Vec::new(),
            pseudo_element: None,
        };
        let mut any = false;
        while !self.eof() {
            match self.input[self.pos..].chars().next().unwrap() {
                '#' => {
                    self.consume_char();
                    selector.id = Some(self.parse_identifier());
                    any = true;
                }
                '.' => {
                    self.consume_char();
                    selector.class.push(self.parse_identifier());
                    any = true;
                }
                '*' => {
                    self.consume_char();
                    any = true;
                }
                '[' => {
                    if let Some(attr) = self.parse_attr_selector() {
                        selector.attrs.push(attr);
                        any = true;
                    } else {
                        return None;
                    }
                }
                ':' => {
                    self.consume_char();
                    let double = self.peek() == Some(':');
                    if double {
                        self.consume_char();
                    }
                    // before/after 는 의사요소로 표시 (단일 콜론 :before 레거시도 허용).
                    // 그 외는 의사클래스로 파싱 (pos 되돌려 재파싱).
                    let save = self.pos;
                    let name = self.parse_identifier().to_ascii_lowercase();
                    match name.as_str() {
                        "before" => selector.pseudo_element = Some(PseudoElement::Before),
                        "after" => selector.pseudo_element = Some(PseudoElement::After),
                        _ => {
                            self.pos = save;
                            match self.parse_pseudo() {
                                Some(p) => selector.pseudos.push(p),
                                None => return None,
                            }
                        }
                    }
                    any = true;
                }
                c if valid_identifier_char(c) => {
                    // HTML 의 타입 선택자는 ASCII 대소문자 구분이 없다(선택자 표준 §6.1).
                    // DOM 태그명은 소문자로 정규화돼 있으므로 여기서 소문자로 맞춘다.
                    // 예전엔 `DIV SPAN { … }` 같은 규칙이 조용히 아무것도 매칭하지 않았다.
                    selector.tag_name = Some(self.parse_identifier().to_ascii_lowercase());
                    any = true;
                }
                _ => break,
            }
        }
        if any {
            Some(selector)
        } else {
            None
        }
    }

    // 의사 클래스 파싱. 함수형(nth-child(..)/not(..))과 키워드형.
    // :is()/:not() 인자: 콤마로 구분된 compound 선택자 목록을 파싱 (복합 결합자는 근사로 첫 compound).
    // :has() 의 상대 선택자 목록: ".a, > .b, + .c, ~ .d"
    // 선행 결합자가 없으면 자손(Descendant)이다.
    fn parse_relative_list(arg: &str) -> Vec<(Combinator, Selector)> {
        arg.split(',')
            .filter_map(|part| {
                let p = part.trim();
                if p.is_empty() {
                    return None;
                }
                let (comb, rest) = match p.as_bytes().first() {
                    Some(b'>') => (Combinator::Child, &p[1..]),
                    Some(b'+') => (Combinator::NextSibling, &p[1..]),
                    Some(b'~') => (Combinator::LaterSibling, &p[1..]),
                    _ => (Combinator::Descendant, p),
                };
                let sels = parse_selector_list(rest.trim())?;
                sels.into_iter().next().map(|s| (comb, s))
            })
            .collect()
    }

    fn parse_selector_arg_list(arg: &str) -> Vec<SimpleSelector> {
        arg.split(',')
            .filter_map(|part| {
                let p = part.trim();
                if p.is_empty() {
                    return None;
                }
                let mut inner = Parser {
                    pos: 0,
                    input: p.to_string(),
                    viewport_width: 0.0,
                    font_faces: Vec::new(),
                    keyframes: std::collections::HashMap::new(),
                    layers: Vec::new(),
                    cur_container: None,
                    cur_layer: None,
                    anon_count: 0,
                };
                inner.parse_simple_selector()
            })
            .collect()
    }

    fn parse_pseudo(&mut self) -> Option<Pseudo> {
        let name = self.parse_identifier().to_ascii_lowercase();
        // 함수형: 괄호 안 인자
        if self.peek() == Some('(') {
            self.consume_char();
            // 괄호 균형을 맞춰 읽는다. 첫 ')' 에서 끊으면 :not(:has(.x)) 처럼 중첩된
            // 인자가 ":has(.x" 로 잘려 규칙이 통째로 죽는다.
            let mut depth = 1usize;
            let mut arg = String::new();
            while let Some(c) = self.peek() {
                self.consume_char();
                match c {
                    '(' => {
                        depth += 1;
                        arg.push(c);
                    }
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                        arg.push(c);
                    }
                    _ => arg.push(c),
                }
            }
            return match name.as_str() {
                "nth-child" => {
                    let (a, b) = parse_nth(arg.trim())?;
                    Some(Pseudo::NthChild(a, b))
                }
                "nth-last-child" => {
                    let (a, b) = parse_nth(arg.trim())?;
                    Some(Pseudo::NthLastChild(a, b))
                }
                "nth-of-type" => {
                    let (a, b) = parse_nth(arg.trim())?;
                    Some(Pseudo::NthOfType(a, b))
                }
                "nth-last-of-type" => {
                    let (a, b) = parse_nth(arg.trim())?;
                    Some(Pseudo::NthLastOfType(a, b))
                }
                "not" => {
                    // :not(a, b, ...) — 콤마 목록 중 하나라도 매칭되면 제외
                    let sels = Self::parse_selector_arg_list(&arg);
                    if sels.is_empty() {
                        Some(Pseudo::Dynamic)
                    } else {
                        Some(Pseudo::Not(sels))
                    }
                }
                "is" | "matches" | "where" => {
                    // :is/:where(a, b, ...) — 인자 중 하나라도 매칭. :where 는 특이도 0.
                    let sels = Self::parse_selector_arg_list(&arg);
                    if sels.is_empty() {
                        Some(Pseudo::Dynamic)
                    } else if name == "where" {
                        Some(Pseudo::Where(sels))
                    } else {
                        Some(Pseudo::Is(sels))
                    }
                }
                "has" => {
                    let rels = Self::parse_relative_list(&arg);
                    if rels.is_empty() {
                        Some(Pseudo::Dynamic)
                    } else {
                        Some(Pseudo::Has(rels))
                    }
                }
                _ => Some(Pseudo::Dynamic), // lang 등 미지원 → 근사
            };
        }
        Some(match name.as_str() {
            "first-child" => Pseudo::FirstChild,
            "last-child" => Pseudo::LastChild,
            "only-child" => Pseudo::OnlyChild,
            "first-of-type" => Pseudo::NthOfType(0, 1),
            "last-of-type" => Pseudo::NthLastOfType(0, 1),
            "only-of-type" => Pseudo::OnlyOfType,
            "root" => Pseudo::Root,
            "empty" => Pseudo::Empty,
            "checked" => Pseudo::Checked,
            "disabled" => Pseudo::Disabled,
            "enabled" => Pseudo::Enabled,
            "required" => Pseudo::Required,
            "optional" => Pseudo::Optional,
            "link" => Pseudo::Link,
            // 상호작용/방문 상태(hover/focus/active/visited) → 정적 렌더에선 비매칭
            _ => Pseudo::Dynamic,
        })
    }

    // [name] 또는 [name=value] / [name="value"]. =/따옴표 파싱, 그 외 연산자(~= 등)는
    // value 없는 존재 검사로 관용 처리. name 은 소문자화.
    fn parse_attr_selector(&mut self) -> Option<(String, AttrOp, bool)> {
        self.consume_char(); // '['
        self.consume_whitespace();
        let name = self.parse_identifier().to_ascii_lowercase();
        self.consume_whitespace();
        // 연산자: = ^= $= *= ~= |=
        let op_char = self.peek();
        let op = if op_char == Some('=') {
            self.consume_char();
            Some(' ')
        } else if matches!(op_char, Some('^' | '$' | '*' | '~' | '|'))
            && self.input[self.pos..].chars().nth(1) == Some('=')
        {
            let c = op_char.unwrap();
            self.consume_char();
            self.consume_char(); // '='
            Some(c)
        } else {
            None
        };
        let attr_op = match op {
            None => AttrOp::Exists,
            Some(c) => {
                self.consume_whitespace();
                let v = self.parse_attr_value();
                match c {
                    '^' => AttrOp::Prefix(v),
                    '$' => AttrOp::Suffix(v),
                    '*' => AttrOp::Contains(v),
                    '~' => AttrOp::Word(v),
                    '|' => AttrOp::Dash(v),
                    _ => AttrOp::Equals(v),
                }
            }
        };
        // 값 뒤 대소문자 플래그: i(무시)/s(구분). 기본은 대소문자 구분.
        self.consume_whitespace();
        let flag = self.parse_identifier().to_ascii_lowercase();
        let ci = flag == "i";
        // 남은 것(예상외 토큰) ']' 까지 소비
        self.consume_while(|c| c != ']');
        if self.peek() == Some(']') {
            self.consume_char();
        }
        if name.is_empty() {
            None
        } else {
            Some((name, attr_op, ci))
        }
    }

    // 속성값: 따옴표 있으면 그 안, 없으면 다음 공백/']' 까지.
    fn parse_attr_value(&mut self) -> String {
        match self.peek() {
            Some(q @ ('"' | '\'')) => {
                self.consume_char();
                let v = self.consume_while(|c| c != q);
                if self.peek() == Some(q) {
                    self.consume_char();
                }
                v
            }
            _ => self.consume_while(|c| c != ']' && !c.is_whitespace()),
        }
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
                    declarations.extend(self.parse_declaration());
                }
            }
        }
        declarations
    }

    fn parse_declaration(&mut self) -> Vec<Declaration> {
        let name = self.parse_identifier().trim().to_ascii_lowercase();
        self.consume_whitespace();
        if self.peek() != Some(':') {
            self.skip_to_decl_end();
            return Vec::new();
        }
        self.consume_char(); // ':'
        self.consume_whitespace();
        let value_text = self.consume_while(|c| c != ';' && c != '}');
        if self.peek() == Some(';') {
            self.consume_char();
        }
        if name.is_empty() {
            return Vec::new();
        }
        // 후행 !important (대소문자 무시, 공백 허용) 분리. 나머지가 실제 값.
        let (val, important) = split_important(value_text.trim());
        let mut decls = expand_declaration(&name, val);
        if important {
            for d in &mut decls {
                d.important = true;
            }
        }
        decls
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_comments_stripped_declarations_survive() {
        // 주석이 섞인 규칙(미압축 CSS)에서 선언이 유실되지 않아야 (mdBook #help 사례)
        let ss = parse(
            "#x { /* pos */ position: fixed; /* hide me */ display: none; width: 10px; }".to_string(),
        );
        let d = &ss.rules[0].declarations;
        assert!(
            d.iter().any(|x| x.name == "display" && matches!(&x.value, Value::Keyword(k) if k == "none")),
            "display:none 이 주석 뒤에서 살아남아야: {:?}",
            d.iter().map(|x| &x.name).collect::<Vec<_>>()
        );
        assert!(d.iter().any(|x| x.name == "width"));
        // 문자열 안 /* 는 주석 아님
        let s = strip_comments("a { content: \"/* not comment */\"; }");
        assert!(s.contains("not comment"), "문자열 안 보존: {}", s);
    }

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
            other => panic!("expected Simple, got {:?}", other),
        }
    }

    #[test]
    fn specificity_counts_id_class_tag() {
        let ss = parse("#x { color: #000000; }".to_string());
        assert_eq!(ss.rules[0].selectors[0].specificity(), (1, 0, 0));
    }

    #[test]
    fn repeating_gradient_parses() {
        let ss = crate::css::parse(
            "#d { background: repeating-linear-gradient(90deg, #ff0000 0 10px, #0000ff 10px 20px); }"
                .to_string(),
        );
        let d = &ss.rules[0].declarations;
        let g = d
            .iter()
            .find(|x| x.name == "background-image")
            .map(|x| &x.value)
            .expect("background-image 선언이 나와야 한다 (안 나오면 배경이 통째로 사라진다)");
        match g {
            crate::css::Value::Gradient(g) => {
                assert!(g.repeating, "repeating 플래그");
                assert_eq!(g.stops.len(), 4, "이중 위치는 스톱 두 개로 펼쳐진다");
                assert_eq!(g.stops[1].1, crate::css::StopPos::Px(10.0));
            }
            other => panic!("그라디언트가 아니다: {:?}", other),
        }
    }

    #[test]
    fn unicode_range_is_parsed() {
        let ss = crate::css::parse(
            "@font-face { font-family: 'X'; src: url(a.woff2) format('woff2'); \
             unicode-range: U+0000-00FF, U+4E00-9FFF, U+30??; }"
                .to_string(),
        );
        let ff = &ss.font_faces[0];
        assert_eq!(ff.family, "X");
        assert_eq!(
            ff.unicode_range,
            vec![(0x0, 0xff), (0x4e00, 0x9fff), (0x3000, 0x30ff)],
            "unicode-range 를 못 읽으면 서브셋 폰트를 전부 받는다"
        );
        let mut ko = std::collections::HashSet::new();
        ko.insert('가' as u32);
        assert!(!ff.covers_any(&ko), "한글은 이 face 가 안 덮는다");
        let mut en = std::collections::HashSet::new();
        en.insert('A' as u32);
        assert!(ff.covers_any(&en));
    }

    #[test]
    fn skips_non_media_at_rules() {
        // @keyframes 등은 스킵, 뒤 규칙은 파싱
        let ss = parse("@keyframes spin { from {} to {} } div { width: 5px; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
    }

    #[test]
    fn ua_styles_hidden_and_marks() {
        let ss = user_agent_stylesheet();
        // [hidden] → display: none
        let root = crate::html::parse_dom("<div hidden></div>".to_string());
        let styled = crate::style::style_tree(&root, &ss);
        fn find_tag<'a>(n: &'a crate::style::StyledNode<'a>, tag: &str) -> Option<&'a crate::style::StyledNode<'a>> {
            if matches!(&n.node.node_type, crate::dom::NodeType::Element(e) if e.tag_name == tag) {
                return Some(n);
            }
            n.children.iter().find_map(|c| find_tag(c, tag))
        }
        assert_eq!(find_tag(&styled, "div").unwrap().value("display"), Some(Value::Keyword("none".to_string())));
        // <del> → line-through
        let r2 = crate::html::parse_dom("<del>x</del>".to_string());
        let s2 = crate::style::style_tree(&r2, &ss);
        assert_eq!(
            find_tag(&s2, "del").unwrap().value("text-decoration-line"),
            Some(Value::Keyword("line-through".to_string()))
        );
    }

    #[test]
    fn logical_properties_map_to_physical() {
        let d = parse_inline_style("margin-inline: 10px 20px; padding-block: 5px; inline-size: 100px; inset-inline-start: 3px");
        let get = |n: &str| d.iter().find(|x| x.name == n).map(|x| &x.value);
        assert_eq!(get("margin-left"), Some(&Value::Length(10.0, Unit::Px)));
        assert_eq!(get("margin-right"), Some(&Value::Length(20.0, Unit::Px)));
        assert_eq!(get("padding-top"), Some(&Value::Length(5.0, Unit::Px)));
        assert_eq!(get("padding-bottom"), Some(&Value::Length(5.0, Unit::Px)));
        assert_eq!(get("width"), Some(&Value::Length(100.0, Unit::Px)));
        assert_eq!(get("left"), Some(&Value::Length(3.0, Unit::Px)));
        // inset 단축 → 네 변
        let d2 = parse_inline_style("inset: 1px 2px 3px 4px");
        let g2 = |n: &str| d2.iter().find(|x| x.name == n).map(|x| &x.value);
        assert_eq!(g2("top"), Some(&Value::Length(1.0, Unit::Px)));
        assert_eq!(g2("left"), Some(&Value::Length(4.0, Unit::Px)));
    }

    #[test]
    fn place_shorthands_expand() {
        let decls = parse_inline_style("place-items: center start");
        let get = |n: &str| decls.iter().find(|d| d.name == n).map(|d| &d.value);
        assert_eq!(get("align-items"), Some(&Value::Keyword("center".to_string())));
        assert_eq!(get("justify-items"), Some(&Value::Keyword("start".to_string())));
        // 단일 값 → 양 축 동일
        let d2 = parse_inline_style("place-content: center");
        let get2 = |n: &str| d2.iter().find(|d| d.name == n).map(|d| &d.value);
        assert_eq!(get2("align-content"), Some(&Value::Keyword("center".to_string())));
        assert_eq!(get2("justify-content"), Some(&Value::Keyword("center".to_string())));
    }

    #[test]
    fn font_face_captured() {
        // @font-face 는 rules 가 아니라 font_faces 로. family + src url 추출.
        let ss = parse(
            "@font-face { font-family: \"Icons\"; src: url(icons.ttf) format(\"truetype\"); } \
             div { width: 5px; }"
                .to_string(),
        );
        assert_eq!(ss.rules.len(), 1, "일반 규칙은 그대로");
        assert_eq!(ss.font_faces.len(), 1);
        assert_eq!(ss.font_faces[0].family, "Icons");
        assert_eq!(ss.font_faces[0].srcs, vec!["icons.ttf".to_string()]);
    }

    #[test]
    fn media_min_width_included_when_viewport_wide_enough() {
        // 뷰포트 1000 → min-width:768 매칭 → 내부 규칙 포함
        let ss = parse_viewport(
            "@media (min-width: 768px) { p { color: #ff0000; } } div { width: 5px; }".to_string(),
            1000.0,
        );
        assert_eq!(ss.rules.len(), 2);
        assert!(ss.rules.iter().any(|r| r.declarations.iter().any(|d| d.name == "color")));
    }

    #[test]
    fn media_min_width_excluded_when_viewport_too_narrow() {
        // 뷰포트 600 → min-width:768 불일치 → 내부 규칙 드롭
        let ss = parse_viewport(
            "@media (min-width: 768px) { p { color: #ff0000; } } div { width: 5px; }".to_string(),
            600.0,
        );
        assert_eq!(ss.rules.len(), 1);
        assert_eq!(ss.rules[0].declarations[0].name, "width");
    }

    #[test]
    fn media_em_units_orientation_and_unknown() {
        // 48em = 768px(@16). vw=1000 → max-width:48em 불일치(데스크톱). 이전엔 em 무시→항상 참.
        assert!(!media_matches("(max-width: 48em)", 1000.0), "48em max 데스크톱 불일치");
        assert!(media_matches("(min-width: 40em)", 1000.0), "40em min 매칭");
        // orientation (vw>=vh=800 → landscape)
        assert!(media_matches("(orientation: landscape)", 1000.0));
        assert!(!media_matches("(orientation: portrait)", 1000.0));
        // 데스크톱 인터랙션 미디어
        assert!(media_matches("(hover: hover)", 1000.0));
        assert!(media_matches("(pointer: fine)", 1000.0));
        assert!(!media_matches("(pointer: coarse)", 1000.0));
        // 미인식 특성 → 불일치 (관용적 참 아님)
        assert!(!media_matches("(grid: 1)", 1000.0));
        assert!(!media_matches("(min-resolution: 2dppx)", 1000.0));
        // Level 4 범위 문법
        assert!(media_matches("(width >= 768px)", 1000.0));
        assert!(!media_matches("(width >= 1200px)", 1000.0));
        // 결합
        assert!(media_matches("screen and (min-width: 30em)", 1000.0));
        assert!(!media_matches("screen and (max-width: 20em)", 1000.0));
    }

    #[test]
    fn prefers_color_scheme_defaults_light() {
        // 헤드리스 = light: dark 스킴 블록은 드롭, light/not-dark 는 포함.
        assert!(!media_matches("(prefers-color-scheme: dark)", 1000.0), "dark 불일치");
        assert!(media_matches("(prefers-color-scheme: light)", 1000.0), "light 일치");
        assert!(media_matches("(not (prefers-color-scheme: dark))", 1000.0), "not-dark 일치");
        assert!(!media_matches("(prefers-contrast: more)", 1000.0), "more 대비 불일치");
        // dark 스킴 @media 블록의 내부 규칙은 캐스케이드에서 빠진다
        let ss = parse_viewport(
            "@media (prefers-color-scheme: dark) { body { color: #ffffff; } } p { color: #000000; }"
                .to_string(),
            1000.0,
        );
        assert_eq!(ss.rules.len(), 1, "dark 블록 드롭 → p 규칙만");
    }

    #[test]
    fn media_max_width_and_print() {
        // max-width:600 은 뷰포트 500 에서 매칭
        let ss = parse_viewport("@media (max-width: 600px) { p { width: 1px; } }".to_string(), 500.0);
        assert_eq!(ss.rules.len(), 1);
        // print 전용은 화면(어떤 폭이든)에서 제외
        let ss2 = parse_viewport("@media print { p { width: 1px; } }".to_string(), 1000.0);
        assert_eq!(ss2.rules.len(), 0);
    }

    #[test]
    fn parses_child_combinator() {
        // '>' 자식 결합자 지원 → [(Descendant, .a), (Child, .b)]
        let ss = parse(".a > .b { color: #ff0000; }".to_string());
        assert_eq!(ss.rules.len(), 1);
        match &ss.rules[0].selectors[0] {
            Selector::Complex(parts) => {
                assert_eq!(parts.len(), 2);
                assert_eq!(parts[1].0, Combinator::Child);
            }
            other => panic!("expected Complex, got {:?}", other),
        }
    }

    #[test]
    fn parses_pseudo_elements_before_after() {
        let ss = parse(
            ".a::before { content: \"\\2022\"; } .b:after { content: \"x\"; }".to_string(),
        );
        // ::before → pseudo_element Before, 클래스 유지, content 디코드
        let s0 = ss.rules[0].selectors[0].subject();
        assert_eq!(s0.pseudo_element, Some(PseudoElement::Before));
        assert_eq!(s0.class, vec!["a".to_string()]);
        let content = ss.rules[0].declarations.iter().find(|d| d.name == "content").unwrap();
        assert_eq!(content.value, Value::Keyword("\u{2022}".to_string()), "\\2022 → •");
        // 레거시 단일 콜론 :after 도 의사요소로
        assert_eq!(ss.rules[1].selectors[0].subject().pseudo_element, Some(PseudoElement::After));
        // 일반 의사클래스는 pseudo_element 아님
        let ss2 = parse("a:hover { color: #ff0000; }".to_string());
        assert_eq!(ss2.rules[0].selectors[0].subject().pseudo_element, None);
    }

    #[test]
    fn parses_descendant_selector_chain() {
        let ss = parse("div .note p { width: 5px; }".to_string());
        match &ss.rules[0].selectors[0] {
            Selector::Complex(parts) => {
                assert_eq!(parts.len(), 3);
                assert_eq!(parts[0].1.tag_name.as_deref(), Some("div"));
                assert_eq!(parts[1].0, Combinator::Descendant);
                assert_eq!(parts[1].1.class, vec!["note".to_string()]);
                assert_eq!(parts[2].1.tag_name.as_deref(), Some("p"));
            }
            other => panic!("expected Complex, got {:?}", other),
        }
    }

    #[test]
    fn descendant_specificity_sums_parts() {
        let ss = parse("#a .b p { width: 1px; }".to_string());
        assert_eq!(ss.rules[0].selectors[0].specificity(), (1, 1, 1));
    }

    #[test]
    fn parses_named_color() {
        let ss = parse("p { color: red; }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 255, g: 0, b: 0, a: 255 })
        );
    }

    #[test]
    fn parses_short_hex_color() {
        let ss = parse("p { color: #f80; }".to_string());
        // #f80 → #ff8800
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 255, g: 136, b: 0, a: 255 })
        );
    }

    #[test]
    fn parses_rgb_function() {
        let ss = parse("p { color: rgb(1, 2, 3); }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 1, g: 2, b: 3, a: 255 })
        );
    }

    #[test]
    fn parses_rgba_function_alpha() {
        let ss = parse("p { color: rgba(10, 20, 30, 0.5); }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Color(Color { r: 10, g: 20, b: 30, a: 128 })
        );
    }

    #[test]
    fn unknown_keyword_stays_keyword() {
        let ss = parse("p { display: flex; }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Keyword("flex".to_string())
        );
    }

    #[test]
    fn parses_relative_units() {
        let ss = parse("p { font-size: 1.5em; width: 50%; margin-top: 2rem; }".to_string());
        let d = &ss.rules[0].declarations;
        assert_eq!(d[0].value, Value::Length(1.5, Unit::Em));
        assert_eq!(d[1].value, Value::Length(50.0, Unit::Percent));
        assert_eq!(d[2].value, Value::Length(2.0, Unit::Rem));
    }

    #[test]
    fn parses_url_value() {
        let ss = parse("div { background-image: url(https://a.com/B.jpg); }".to_string());
        assert_eq!(
            ss.rules[0].declarations[0].value,
            Value::Url("https://a.com/B.jpg".to_string())
        );
        let ss = parse("div { background-image: url(\"img/x.png\"); }".to_string());
        assert_eq!(ss.rules[0].declarations[0].value, Value::Url("img/x.png".to_string()));
    }

    // 캐스케이드: 같은 이름이 여러 번이면 마지막 선언이 이긴다
    fn decl<'a>(ss: &'a Stylesheet, name: &str) -> Option<&'a Value> {
        ss.rules[0].declarations.iter().rev().find(|d| d.name == name).map(|d| &d.value)
    }

    #[test]
    fn margin_shorthand_one_value_expands_to_four() {
        let ss = parse("div { margin: 10px; }".to_string());
        for side in ["margin-top", "margin-right", "margin-bottom", "margin-left"] {
            assert_eq!(decl(&ss, side), Some(&Value::Length(10.0, Unit::Px)), "{}", side);
        }
    }

    #[test]
    fn margin_shorthand_two_values() {
        let ss = parse("div { margin: 10px 20px; }".to_string());
        assert_eq!(decl(&ss, "margin-top"), Some(&Value::Length(10.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-bottom"), Some(&Value::Length(10.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-left"), Some(&Value::Length(20.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-right"), Some(&Value::Length(20.0, Unit::Px)));
    }

    #[test]
    fn margin_zero_auto_keeps_auto_sides() {
        let ss = parse("div { margin: 0 auto; }".to_string());
        assert_eq!(decl(&ss, "margin-top"), Some(&Value::Length(0.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-left"), Some(&Value::Keyword("auto".to_string())));
        assert_eq!(decl(&ss, "margin-right"), Some(&Value::Keyword("auto".to_string())));
    }

    #[test]
    fn padding_shorthand_four_values_clockwise() {
        let ss = parse("div { padding: 1px 2px 3px 4px; }".to_string());
        assert_eq!(decl(&ss, "padding-top"), Some(&Value::Length(1.0, Unit::Px)));
        assert_eq!(decl(&ss, "padding-right"), Some(&Value::Length(2.0, Unit::Px)));
        assert_eq!(decl(&ss, "padding-bottom"), Some(&Value::Length(3.0, Unit::Px)));
        assert_eq!(decl(&ss, "padding-left"), Some(&Value::Length(4.0, Unit::Px)));
    }

    #[test]
    fn border_shorthand_expands_to_width_style_color() {
        let ss = parse("div { border: 1px solid #cccccc; }".to_string());
        for side in ["top", "right", "bottom", "left"] {
            assert_eq!(decl(&ss, &format!("border-{}-width", side)), Some(&Value::Length(1.0, Unit::Px)));
            assert_eq!(
                decl(&ss, &format!("border-{}-style", side)),
                Some(&Value::Keyword("solid".to_string()))
            );
            assert_eq!(
                decl(&ss, &format!("border-{}-color", side)),
                Some(&Value::Color(Color { r: 204, g: 204, b: 204, a: 255 }))
            );
        }
    }

    #[test]
    fn box_shadow_expands_to_longhands() {
        let ss = parse("div { box-shadow: 0 2px 8px rgba(0,0,0,0.15); }".to_string());
        assert_eq!(decl(&ss, "box-shadow-x"), Some(&Value::Length(0.0, Unit::Px)));
        assert_eq!(decl(&ss, "box-shadow-y"), Some(&Value::Length(2.0, Unit::Px)));
        assert_eq!(decl(&ss, "box-shadow-blur"), Some(&Value::Length(8.0, Unit::Px)));
        assert_eq!(
            decl(&ss, "box-shadow-color"),
            Some(&Value::Color(Color { r: 0, g: 0, b: 0, a: 38 }))
        );
    }

    #[test]
    fn box_shadow_inset_parsed() {
        let ss = parse("div { box-shadow: inset 0 2px 4px #000000; }".to_string());
        assert_eq!(decl(&ss, "box-shadow-x"), Some(&Value::Length(0.0, Unit::Px)));
        assert_eq!(decl(&ss, "box-shadow-y"), Some(&Value::Length(2.0, Unit::Px)));
        assert_eq!(decl(&ss, "box-shadow-inset"), Some(&Value::Keyword("inset".to_string())));
    }

    #[test]
    fn border_radius_single_value_kept() {
        let ss = parse("div { border-radius: 12px; }".to_string());
        assert_eq!(decl(&ss, "border-radius"), Some(&Value::Length(12.0, Unit::Px)));
        // 다중값은 첫 토큰만 (균일 근사)
        let ss2 = parse("div { border-radius: 8px 4px 8px 4px; }".to_string());
        assert_eq!(decl(&ss2, "border-radius"), Some(&Value::Length(8.0, Unit::Px)));
    }

    #[test]
    fn border_side_and_color_shorthands() {
        // 변별 단축값 + border-color 4값
        let ss = parse(
            "div { border-left: 4px solid #f4b400; border-color: #111111 #222222 #333333 #444444; }"
                .to_string(),
        );
        assert_eq!(decl(&ss, "border-left-width"), Some(&Value::Length(4.0, Unit::Px)));
        assert_eq!(decl(&ss, "border-left-style"), Some(&Value::Keyword("solid".to_string())));
        // border-color 4값이 border-left-color 를 덮어씀 (문서 순서상 뒤)
        assert_eq!(decl(&ss, "border-top-color"), Some(&Value::Color(Color { r: 17, g: 17, b: 17, a: 255 })));
        assert_eq!(decl(&ss, "border-left-color"), Some(&Value::Color(Color { r: 68, g: 68, b: 68, a: 255 })));
    }

    #[test]
    fn longhand_after_shorthand_overrides() {
        let ss = parse("div { margin: 10px; margin-left: 5px; }".to_string());
        assert_eq!(decl(&ss, "margin-left"), Some(&Value::Length(5.0, Unit::Px)));
        assert_eq!(decl(&ss, "margin-top"), Some(&Value::Length(10.0, Unit::Px)));
    }

    #[test]
    fn ua_stylesheet_hides_script_and_style() {
        let ss = user_agent_stylesheet();
        for tag in ["script", "style", "head"] {
            let hidden = ss.rules.iter().any(|r| {
                r.selectors.iter().any(|s| s.subject().tag_name.as_deref() == Some(tag)) && r
                    .declarations
                    .iter()
                    .any(|d| d.name == "display" && d.value == Value::Keyword("none".to_string()))
            });
            assert!(hidden, "{} should be display:none in UA", tag);
        }
    }

    #[test]
    fn ua_stylesheet_has_display_block_for_div() {
        let ss = user_agent_stylesheet();
        let matches_div = ss.rules.iter().any(|r| {
            r.selectors.iter().any(|s| s.subject().tag_name.as_deref() == Some("div")) && r
                .declarations
                .iter()
                .any(|d| d.name == "display" && d.value == Value::Keyword("block".to_string()))
        });
        assert!(matches_div);
    }
}
