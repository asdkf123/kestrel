// JS 렉서: 소스 → 토큰열. 최대 munch (=== 이 == 보다 우선).
// 지원: 템플릿 리터럴, 정규식 리터럴, \u/\x/\u{} 이스케이프, ASI용 개행 추적.

// 지수부(e/E [+/-] 숫자)를 있으면 소비. 뒤에 숫자가 있을 때만 지수로 취급
// (그래야 `1 .e` 같은 경우에 e 를 식별자로 안 삼킴). i 를 전진시킨다.
fn lex_exponent(b: &[char], i: &mut usize) {
    if *i < b.len() && (b[*i] == 'e' || b[*i] == 'E') {
        let mut j = *i + 1;
        if j < b.len() && (b[j] == '+' || b[j] == '-') {
            j += 1;
        }
        if j < b.len() && b[j].is_ascii_digit() {
            j += 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            *i = j;
        }
    }
}

// 템플릿 리터럴 조각: 리터럴 텍스트 / ${...} 안의 식 소스 (파서가 재귀 파싱)
#[derive(Debug, Clone, PartialEq)]
pub enum TplPart {
    Lit(String),
    Expr(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Num(f64),
    // BigInt 리터럴 (123n). 문자열로 보존해 정확히 파싱한다 — f64 로 근사하면
    // 2n**64n 같은 값이 조용히 틀린다.
    BigInt(String),
    Str(String),
    Ident(String),
    Template(Vec<TplPart>),
    // 정규식 리터럴 (소스, 플래그) — 매칭 엔진은 없고 파싱만 수용 (관용)
    Regex(String, String),
    // 키워드
    Var,
    Let,
    Const,
    Function,
    Return,
    If,
    Else,
    While,
    Do,
    For,
    Break,
    Continue,
    True,
    False,
    Null,
    Undefined,
    Typeof,
    Void,
    Delete,
    Try,
    Catch,
    Finally,
    Throw,
    Switch,
    Case,
    Default,
    Instanceof,
    In,
    Class,
    New,
    This,
    Extends,
    Super,
    Static,
    // 구두점
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Semi,
    Comma,
    Dot,
    Colon,
    Question,
    Arrow, // =>
    // 연산자
    Assign,
    PlusAssign,
    MinusAssign,
    StarAssign,
    SlashAssign,
    PercentAssign,
    AmpAssign,
    PipeAssign,
    CaretAssign,
    ShlAssign,
    ShrAssign,
    UShrAssign,
    StarStar,       // **
    StarStarAssign, // **=
    AndAndAssign,
    OrOrAssign,
    QQAssign, // ??=
    QuestionQuestion,
    OptChain, // ?.
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    EqEq,
    EqEqEq,
    NotEq,
    NotEqEq,
    Lt,
    Gt,
    Le,
    Ge,
    AndAnd,
    OrOr,
    Not,
    PlusPlus,
    MinusMinus,
    // 비트 연산자
    Amp,
    Pipe,
    Caret,
    Tilde,
    Shl,
    Shr,
    UShr,
}

fn keyword(word: &str) -> Option<Tok> {
    Some(match word {
        "var" => Tok::Var,
        "let" => Tok::Let,
        "const" => Tok::Const,
        "function" => Tok::Function,
        "return" => Tok::Return,
        "if" => Tok::If,
        "else" => Tok::Else,
        "while" => Tok::While,
        "do" => Tok::Do,
        "for" => Tok::For,
        "break" => Tok::Break,
        "continue" => Tok::Continue,
        "true" => Tok::True,
        "false" => Tok::False,
        "null" => Tok::Null,
        "undefined" => Tok::Undefined,
        "typeof" => Tok::Typeof,
        "void" => Tok::Void,
        "delete" => Tok::Delete,
        "try" => Tok::Try,
        "catch" => Tok::Catch,
        "finally" => Tok::Finally,
        "throw" => Tok::Throw,
        "switch" => Tok::Switch,
        "case" => Tok::Case,
        "default" => Tok::Default,
        "instanceof" => Tok::Instanceof,
        "in" => Tok::In,
        "class" => Tok::Class,
        "new" => Tok::New,
        "this" => Tok::This,
        "extends" => Tok::Extends,
        "super" => Tok::Super,
        "static" => Tok::Static,
        _ => return None,
    })
}

// 정확히 n 자리 16진을 읽어 code point 반환 (성공 시 i 를 n 전진).
fn read_hex(b: &[char], i: &mut usize, n: usize) -> Option<u32> {
    if *i + n > b.len() {
        return None;
    }
    let hex: String = b[*i..*i + n].iter().collect();
    let cp = u32::from_str_radix(&hex, 16).ok()?;
    *i += n;
    Some(cp)
}

// 문자열/템플릿 이스케이프 해석 (b[*i] = 역슬래시 다음 문자). out 에 push, i 진행.
// \n\t\r\b\f\v\0, \xHH, \uHHHH(+서로게이트쌍), \u{...}, 줄 이음(\+개행), 그 외 문자 그대로.
fn read_escape(b: &[char], i: &mut usize, out: &mut String) {
    let c = b[*i];
    *i += 1;
    match c {
        'n' => out.push('\n'),
        't' => out.push('\t'),
        'r' => out.push('\r'),
        'b' => out.push('\u{08}'),
        'f' => out.push('\u{0C}'),
        'v' => out.push('\u{0B}'),
        '0' => out.push('\0'),
        'x' => match read_hex(b, i, 2).and_then(char::from_u32) {
            Some(ch) => out.push(ch),
            None => out.push('x'),
        },
        'u' => {
            if b.get(*i) == Some(&'{') {
                let start = *i + 1;
                let mut j = start;
                while j < b.len() && b[j] != '}' {
                    j += 1;
                }
                let hex: String = b[start..j].iter().collect();
                match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                    Some(ch) => {
                        out.push(ch);
                        *i = j + 1;
                    }
                    None => out.push('u'),
                }
            } else if let Some(hi) = read_hex(b, i, 4) {
                // 서로게이트 상위: 뒤따르는 \uXXXX 하위와 결합해 보조 평면 문자로
                if (0xD800..=0xDBFF).contains(&hi)
                    && b.get(*i) == Some(&'\\')
                    && b.get(*i + 1) == Some(&'u')
                {
                    let save = *i;
                    *i += 2;
                    if let Some(lo) = read_hex(b, i, 4) {
                        if (0xDC00..=0xDFFF).contains(&lo) {
                            let cp = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                            if let Some(ch) = char::from_u32(cp) {
                                out.push(ch);
                                return;
                            }
                        }
                    }
                    *i = save; // 결합 실패 → 롤백
                }
                out.push(char::from_u32(hi).unwrap_or('\u{FFFD}'));
            } else {
                out.push('u');
            }
        }
        '\n' => {} // 줄 이음 (LineContinuation)
        '\r' => {
            if b.get(*i) == Some(&'\n') {
                *i += 1;
            }
        }
        other => out.push(other), // \" \' \\ / ` $ 등
    }
}

// '/' 위치에서 정규식이 시작될 수 있는가: 직전 토큰이 값으로 끝나면 나눗셈.
fn regex_can_start(prev: Option<&Tok>) -> bool {
    !matches!(
        prev,
        Some(
            Tok::Num(_)
            | Tok::BigInt(_)
                | Tok::Str(_)
                | Tok::Ident(_)
                | Tok::Template(_)
                | Tok::Regex(_, _)
                | Tok::RParen
                | Tok::RBracket
                | Tok::PlusPlus
                | Tok::MinusMinus
                | Tok::True
                | Tok::False
                | Tok::Null
                | Tok::Undefined
                | Tok::This
                | Tok::Super
        )
    )
}

// (토큰, 각 토큰 직전에 개행(LineTerminator)이 있었는지) — 자동 세미콜론 삽입(ASI)용.
pub fn tokenize(src: &str) -> Result<(Vec<Tok>, Vec<bool>), String> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    // nl_before[k] = 토큰 k 직전 공백/주석에 개행이 있었나. pending_nl 은 마지막 토큰
    // 이후 개행 누적. 토큰이 생기는 반복 시작 시 소비.
    let mut nl_before: Vec<bool> = Vec::new();
    let mut pending_nl = false;
    // 괄호 문맥: 각 '(' 가 제어문 헤더(if/while/for/switch/catch/with)인지 스택.
    // 헤더를 닫는 ')' 뒤에는 정규식이 올 수 있다 (if(x) /re/.test(y)). 그 외 ')'는 나눗셈.
    let mut paren_headers: Vec<bool> = Vec::new();
    let mut last_rparen_header = false;
    while i < b.len() {
        // 직전 반복에서 추가된 토큰(들)의 개행 플래그 확정 (첫 토큰만 개행, 나머지 false).
        if out.len() > nl_before.len() {
            nl_before.push(pending_nl);
            while nl_before.len() < out.len() {
                nl_before.push(false);
            }
            pending_nl = false;
        }
        let c = b[i];
        // 공백 (개행이면 pending_nl)
        if c.is_whitespace() {
            if matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                pending_nl = true;
            }
            i += 1;
            continue;
        }
        // 주석
        if c == '/' && i + 1 < b.len() {
            if b[i + 1] == '/' {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
                continue;
            }
            if b[i + 1] == '*' {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    if matches!(b[i], '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                        pending_nl = true; // 여러 줄 블록 주석도 개행으로 (ASI)
                    }
                    i += 1;
                }
                if i + 1 >= b.len() {
                    return Err("닫히지 않은 블록 주석".to_string());
                }
                i += 2;
                continue;
            }
        }
        // 정규식 리터럴 vs 나눗셈: 직전 토큰이 식을 끝낼 수 있으면 나눗셈.
        // 단 ')' 는 제어문 헤더를 닫는 경우엔 정규식 허용(if(x) /re/).
        let regex_allowed = match out.last() {
            Some(Tok::RParen) => last_rparen_header,
            other => regex_can_start(other),
        };
        if c == '/' && regex_allowed {
            let start = i;
            i += 1;
            let mut in_class = false; // [...] 안의 / 는 종료가 아님
            loop {
                match b.get(i) {
                    None => return Err("닫히지 않은 정규식 리터럴".to_string()),
                    Some('\n') => return Err("정규식 리터럴 안의 줄바꿈".to_string()),
                    Some('\\') => i += 2,
                    Some('[') => {
                        in_class = true;
                        i += 1;
                    }
                    Some(']') => {
                        in_class = false;
                        i += 1;
                    }
                    Some('/') if !in_class => {
                        i += 1;
                        break;
                    }
                    Some(_) => i += 1,
                }
            }
            let source: String = b[start + 1..i - 1].iter().collect();
            let fstart = i;
            while i < b.len() && b[i].is_ascii_alphabetic() {
                i += 1;
            }
            let flags: String = b[fstart..i].iter().collect();
            out.push(Tok::Regex(source, flags));
            continue;
        }
        // 선행 소수점 숫자 (.5, .5e3) — 뒤에 숫자가 오면 수, 아니면 Dot
        if c == '.' && b.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
            let start = i;
            i += 1;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            lex_exponent(&b, &mut i);
            let s: String = b[start..i].iter().collect();
            out.push(Tok::Num(s.parse::<f64>().map_err(|e| e.to_string())?));
            continue;
        }
        // 숫자 (10진 + 0x/0b/0o 진법 접두 + 지수부 + BigInt n 접미)
        if c.is_ascii_digit() {
            // 진법 접두: 0x(16) 0b(2) 0o(8). 최소 한 자리 필요.
            if c == '0' && i + 1 < b.len() {
                let radix = match b[i + 1] {
                    'x' | 'X' => Some((16u32, "잘못된 16진 리터럴")),
                    'b' | 'B' => Some((2u32, "잘못된 2진 리터럴")),
                    'o' | 'O' => Some((8u32, "잘못된 8진 리터럴")),
                    _ => None,
                };
                if let Some((radix, err)) = radix {
                    let start = i + 2;
                    let mut j = start;
                    while j < b.len() && (b[j].is_digit(radix) || b[j] == '_') {
                        j += 1;
                    }
                    if j == start {
                        return Err(err.to_string());
                    }
                    // 숫자 구분자 _ 제거 (0xff_ff 등)
                    let s: String = b[start..j].iter().filter(|&&ch| ch != '_').collect();
                    i = j;
                    if i < b.len() && b[i] == 'n' {
                        i += 1;
                        let prefix = match radix {
                            16 => "0x",
                            2 => "0b",
                            _ => "0o",
                        };
                        out.push(Tok::BigInt(format!("{}{}", prefix, s)));
                        continue;
                    }
                    let v = u64::from_str_radix(&s, radix).map_err(|e| e.to_string())?;
                    out.push(Tok::Num(v as f64));
                    continue;
                }
            }
            let start = i;
            while i < b.len() && (b[i].is_ascii_digit() || b[i] == '_') {
                i += 1;
            }
            if i < b.len() && b[i] == '.' {
                i += 1;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '_') {
                    i += 1;
                }
            }
            lex_exponent(&b, &mut i);
            // 숫자 구분자 _ 제거 (1_000_000 등)
            let s: String = b[start..i].iter().filter(|&&ch| ch != '_').collect();
            if i < b.len() && b[i] == 'n' {
                i += 1; // BigInt 리터럴: 자릿수 그대로 보존
                out.push(Tok::BigInt(s));
                continue;
            }
            let v = s.parse::<f64>().map_err(|e| e.to_string())?;
            out.push(Tok::Num(v));
            continue;
        }
        // 문자열
        if c == '"' || c == '\'' {
            let quote = c;
            i += 1;
            let mut s = String::new();
            loop {
                if i >= b.len() {
                    return Err("닫히지 않은 문자열".to_string());
                }
                let ch = b[i];
                if ch == quote {
                    i += 1;
                    break;
                }
                if ch == '\\' {
                    i += 1;
                    if i >= b.len() {
                        return Err("문자열 끝의 역슬래시".to_string());
                    }
                    read_escape(&b, &mut i, &mut s);
                    continue;
                }
                s.push(ch);
                i += 1;
            }
            out.push(Tok::Str(s));
            continue;
        }
        // 템플릿 리터럴: `text ${expr} text`
        if c == '`' {
            i += 1;
            let mut parts: Vec<TplPart> = Vec::new();
            let mut lit = String::new();
            loop {
                if i >= b.len() {
                    return Err("닫히지 않은 템플릿 리터럴".to_string());
                }
                let ch = b[i];
                if ch == '`' {
                    i += 1;
                    break;
                }
                if ch == '\\' {
                    i += 1;
                    if i >= b.len() {
                        return Err("템플릿 끝의 역슬래시".to_string());
                    }
                    read_escape(&b, &mut i, &mut lit);
                    continue;
                }
                if ch == '$' && b.get(i + 1) == Some(&'{') {
                    if !lit.is_empty() {
                        parts.push(TplPart::Lit(std::mem::take(&mut lit)));
                    }
                    i += 2;
                    // ${...} 식 소스 추출: 중괄호 깊이 추적 + 내부 문자열 스킵
                    let start = i;
                    let mut depth = 1usize;
                    while i < b.len() {
                        match b[i] {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    break;
                                }
                            }
                            '\'' | '"' => {
                                let q = b[i];
                                i += 1;
                                while i < b.len() && b[i] != q {
                                    if b[i] == '\\' {
                                        i += 1;
                                    }
                                    i += 1;
                                }
                            }
                            _ => {}
                        }
                        i += 1;
                    }
                    if depth != 0 {
                        return Err("닫히지 않은 ${ } 보간".to_string());
                    }
                    parts.push(TplPart::Expr(b[start..i].iter().collect()));
                    i += 1; // '}'
                    continue;
                }
                lit.push(ch);
                i += 1;
            }
            if !lit.is_empty() || parts.is_empty() {
                parts.push(TplPart::Lit(lit));
            }
            out.push(Tok::Template(parts));
            continue;
        }
        // 식별자/키워드. 유니코드 식별자 지원(café/你好 등): ID_Start≈is_alphabetic,
        // ID_Continue≈is_alphanumeric. '#'은 private 필드, '_'/'$'는 허용.
        if c.is_alphabetic() || c == '_' || c == '$' || c == '#' {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_alphanumeric() || b[i] == '_' || b[i] == '$') {
                i += 1;
            }
            let word: String = b[start..i].iter().collect();
            out.push(keyword(&word).unwrap_or(Tok::Ident(word)));
            continue;
        }
        // 연산자/구두점 (최대 munch: 4글자 → 3글자 → 2글자 → 1글자)
        let four: String = b[i..(i + 4).min(b.len())].iter().collect();
        let four_tok = match four.as_str() {
            ">>>=" => Some(Tok::UShrAssign),
            _ => None,
        };
        if let Some(t) = four_tok {
            out.push(t);
            i += 4;
            continue;
        }
        let three: String = b[i..(i + 3).min(b.len())].iter().collect();
        if three == "===" {
            out.push(Tok::EqEqEq);
            i += 3;
            continue;
        }
        if three == "!==" {
            out.push(Tok::NotEqEq);
            i += 3;
            continue;
        }
        let three_tok = match three.as_str() {
            ">>>" => Some(Tok::UShr),
            "<<=" => Some(Tok::ShlAssign),
            ">>=" => Some(Tok::ShrAssign),
            "**=" => Some(Tok::StarStarAssign),
            "&&=" => Some(Tok::AndAndAssign),
            "||=" => Some(Tok::OrOrAssign),
            "??=" => Some(Tok::QQAssign),
            _ => None,
        };
        if let Some(t) = three_tok {
            out.push(t);
            i += 3;
            continue;
        }
        // ?. 은 뒤가 숫자면 옵셔널 체이닝 아님 (삼항 + .5 소수)
        if c == '?' && b.get(i + 1) == Some(&'.') && !b.get(i + 2).is_some_and(|d| d.is_ascii_digit())
        {
            out.push(Tok::OptChain);
            i += 2;
            continue;
        }
        let two: String = b[i..(i + 2).min(b.len())].iter().collect();
        let two_tok = match two.as_str() {
            "=>" => Some(Tok::Arrow),
            "==" => Some(Tok::EqEq),
            "!=" => Some(Tok::NotEq),
            "<=" => Some(Tok::Le),
            ">=" => Some(Tok::Ge),
            "&&" => Some(Tok::AndAnd),
            "||" => Some(Tok::OrOr),
            "??" => Some(Tok::QuestionQuestion),
            "++" => Some(Tok::PlusPlus),
            "--" => Some(Tok::MinusMinus),
            "+=" => Some(Tok::PlusAssign),
            "-=" => Some(Tok::MinusAssign),
            "*=" => Some(Tok::StarAssign),
            "/=" => Some(Tok::SlashAssign),
            "%=" => Some(Tok::PercentAssign),
            "&=" => Some(Tok::AmpAssign),
            "|=" => Some(Tok::PipeAssign),
            "^=" => Some(Tok::CaretAssign),
            "<<" => Some(Tok::Shl),
            ">>" => Some(Tok::Shr),
            "**" => Some(Tok::StarStar),
            _ => None,
        };
        if let Some(t) = two_tok {
            out.push(t);
            i += 2;
            continue;
        }
        let one = match c {
            '(' => Tok::LParen,
            ')' => Tok::RParen,
            '{' => Tok::LBrace,
            '}' => Tok::RBrace,
            '[' => Tok::LBracket,
            ']' => Tok::RBracket,
            ';' => Tok::Semi,
            ',' => Tok::Comma,
            '.' => Tok::Dot,
            ':' => Tok::Colon,
            '?' => Tok::Question,
            '=' => Tok::Assign,
            '+' => Tok::Plus,
            '-' => Tok::Minus,
            '*' => Tok::Star,
            '/' => Tok::Slash,
            '%' => Tok::Percent,
            '<' => Tok::Lt,
            '>' => Tok::Gt,
            '!' => Tok::Not,
            '&' => Tok::Amp,
            '|' => Tok::Pipe,
            '^' => Tok::Caret,
            '~' => Tok::Tilde,
            other => return Err(format!("알 수 없는 문자: {:?} (위치 {})", other, i)),
        };
        // 괄호 문맥 갱신: '(' 가 제어문 키워드 직후면 헤더로 표시, ')' 는 팝.
        match one {
            Tok::LParen => {
                let header = matches!(
                    out.last(),
                    Some(Tok::If | Tok::While | Tok::For | Tok::Switch | Tok::Catch)
                );
                paren_headers.push(header);
            }
            Tok::RParen => last_rparen_header = paren_headers.pop().unwrap_or(false),
            _ => {}
        }
        out.push(one);
        i += 1;
    }
    // 마지막 토큰의 개행 플래그 확정
    if out.len() > nl_before.len() {
        nl_before.push(pending_nl);
        while nl_before.len() < out.len() {
            nl_before.push(false);
        }
    }
    Ok((out, nl_before))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_operators() {
        let t = tokenize("1 + 2.5 * 0x10").unwrap().0;
        assert_eq!(
            t,
            vec![Tok::Num(1.0), Tok::Plus, Tok::Num(2.5), Tok::Star, Tok::Num(16.0)]
        );
    }

    #[test]
    fn strings_with_escapes() {
        let t = tokenize(r#"'a\'b' + "c\nd""#).unwrap().0;
        assert_eq!(
            t,
            vec![Tok::Str("a'b".to_string()), Tok::Plus, Tok::Str("c\nd".to_string())]
        );
    }

    #[test]
    fn keywords_vs_identifiers() {
        let t = tokenize("var letx = functionx").unwrap().0;
        assert_eq!(
            t,
            vec![
                Tok::Var,
                Tok::Ident("letx".to_string()),
                Tok::Assign,
                Tok::Ident("functionx".to_string())
            ]
        );
    }

    #[test]
    fn maximal_munch() {
        let t = tokenize("a === b == c => d ++ += !").unwrap().0;
        assert_eq!(
            t,
            vec![
                Tok::Ident("a".to_string()),
                Tok::EqEqEq,
                Tok::Ident("b".to_string()),
                Tok::EqEq,
                Tok::Ident("c".to_string()),
                Tok::Arrow,
                Tok::Ident("d".to_string()),
                Tok::PlusPlus,
                Tok::PlusAssign,
                Tok::Not
            ]
        );
    }

    #[test]
    fn comments_are_skipped() {
        let t = tokenize("1 // line\n /* block\n */ 2").unwrap().0;
        assert_eq!(t, vec![Tok::Num(1.0), Tok::Num(2.0)]);
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(tokenize("'abc").is_err());
        assert!(tokenize("/* abc").is_err());
    }
}
