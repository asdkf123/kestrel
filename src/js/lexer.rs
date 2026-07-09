// JS 렉서: 소스 → 토큰열. 최대 munch (=== 이 == 보다 우선).
// 미지원: 템플릿 리터럴, 정규식 리터럴, \u 이스케이프 (에러로 보고).

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

// '/' 위치에서 정규식이 시작될 수 있는가: 직전 토큰이 값으로 끝나면 나눗셈.
fn regex_can_start(prev: Option<&Tok>) -> bool {
    !matches!(
        prev,
        Some(
            Tok::Num(_)
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

pub fn tokenize(src: &str) -> Result<Vec<Tok>, String> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        // 공백
        if c.is_whitespace() {
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
                    i += 1;
                }
                if i + 1 >= b.len() {
                    return Err("닫히지 않은 블록 주석".to_string());
                }
                i += 2;
                continue;
            }
        }
        // 정규식 리터럴 vs 나눗셈: 직전 토큰이 식을 끝낼 수 있으면 나눗셈
        if c == '/' && regex_can_start(out.last()) {
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
                    while j < b.len() && b[j].is_digit(radix) {
                        j += 1;
                    }
                    if j == start {
                        return Err(err.to_string());
                    }
                    let s: String = b[start..j].iter().collect();
                    let v = u64::from_str_radix(&s, radix).map_err(|e| e.to_string())?;
                    i = j;
                    if i < b.len() && b[i] == 'n' {
                        i += 1; // BigInt 접미 — f64 로 근사
                    }
                    out.push(Tok::Num(v as f64));
                    continue;
                }
            }
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i < b.len() && b[i] == '.' {
                i += 1;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
            }
            lex_exponent(&b, &mut i);
            let s: String = b[start..i].iter().collect();
            let v = s.parse::<f64>().map_err(|e| e.to_string())?;
            if i < b.len() && b[i] == 'n' {
                i += 1; // BigInt 접미
            }
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
                    s.push(match b[i] {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        '0' => '\0',
                        other => other, // \" \' \\ / 등: 그대로
                    });
                    i += 1;
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
                    lit.push(match b[i] {
                        'n' => '\n',
                        't' => '\t',
                        'r' => '\r',
                        other => other, // \` \$ \\ 등: 그대로
                    });
                    i += 1;
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
        // 식별자/키워드 ('#' 은 private 필드 수용용 — 클래스 미지원이라 관용 처리)
        if c.is_ascii_alphabetic() || c == '_' || c == '$' || c == '#' {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == '_' || b[i] == '$') {
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
        out.push(one);
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_operators() {
        let t = tokenize("1 + 2.5 * 0x10").unwrap();
        assert_eq!(
            t,
            vec![Tok::Num(1.0), Tok::Plus, Tok::Num(2.5), Tok::Star, Tok::Num(16.0)]
        );
    }

    #[test]
    fn strings_with_escapes() {
        let t = tokenize(r#"'a\'b' + "c\nd""#).unwrap();
        assert_eq!(
            t,
            vec![Tok::Str("a'b".to_string()), Tok::Plus, Tok::Str("c\nd".to_string())]
        );
    }

    #[test]
    fn keywords_vs_identifiers() {
        let t = tokenize("var letx = functionx").unwrap();
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
        let t = tokenize("a === b == c => d ++ += !").unwrap();
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
        let t = tokenize("1 // line\n /* block\n */ 2").unwrap();
        assert_eq!(t, vec![Tok::Num(1.0), Tok::Num(2.0)]);
    }

    #[test]
    fn unterminated_string_errors() {
        assert!(tokenize("'abc").is_err());
        assert!(tokenize("/* abc").is_err());
    }
}
