// 간이 정규식 엔진: 파싱 → 명령어 컴파일 → 백트래킹 VM.
// JS 정규식의 흔한 부분집합 (앵커/클래스/그룹/교대/수량자/역참조/기본 룩어헤드).
// 파국적 백트래킹은 스텝 한도로 차단.

#[derive(Debug, Clone)]
enum Inst {
    Char(char),
    Any,                 // . (dotall 이면 개행 포함)
    Class(Box<ClassData>),
    Save(usize),         // 캡처 슬롯에 현재 위치 저장
    Jmp(usize),
    Split(usize, usize), // pc1 먼저, 실패 시 pc2
    Start,               // ^
    End,                 // $
    WordBoundary(bool),  // \b(true) / \B(false)
    Backref(usize),      // \1..
    Look { neg: bool, prog: Vec<Inst> }, // (?=..) / (?!..)
    // (?<=..) / (?<!..) — sp 에서 **끝나는** 매치를 찾는다 (오른쪽에서 왼쪽).
    LookBehind { neg: bool, prog: Vec<Inst> },
    Match,
}

#[derive(Debug, Clone)]
struct ClassData {
    neg: bool,
    ranges: Vec<(char, char)>,
    kinds: Vec<ClassKind>, // \d \w \s (+ 부정형)
}

#[derive(Debug, Clone, Copy)]
enum ClassKind {
    Digit(bool),
    Word(bool),
    Space(bool),
}

impl ClassData {
    fn matches(&self, c: char) -> bool {
        let mut hit = self.ranges.iter().any(|(lo, hi)| c >= *lo && c <= *hi);
        if !hit {
            hit = self.kinds.iter().any(|k| k.matches(c));
        }
        hit ^ self.neg
    }
}

impl ClassKind {
    fn matches(&self, c: char) -> bool {
        match self {
            ClassKind::Digit(neg) => c.is_ascii_digit() != *neg,
            ClassKind::Word(neg) => (c.is_ascii_alphanumeric() || c == '_') != *neg,
            ClassKind::Space(neg) => c.is_whitespace() != *neg,
        }
    }
}

pub struct Match {
    pub start: usize,
    pub end: usize,
    // 각 그룹의 (시작, 끝) 문자 인덱스. 0 은 전체 매치.
    pub groups: Vec<Option<(usize, usize)>>,
}

pub struct Regex {
    prog: Vec<Inst>,
    ngroups: usize,
    // 이름 있는 그룹 (?<name>...) → 그룹 인덱스. .groups / $<name> 치환에 사용.
    pub group_names: Vec<(String, usize)>,
    ignore_case: bool,
    multiline: bool,
    dotall: bool,
    pub global: bool,
    // 룩비하인드용: Match 는 이 위치에서 끝나야만 성공이다 (없으면 아무 데서나 끝나도 됨).
    require_end: Option<usize>,
}

// ── 파서: source → AST ──────────────────────────────────────────────
enum Ast {
    Empty,
    Char(char),
    Any,
    Class(ClassData),
    Start,
    End,
    WordBoundary(bool),
    Backref(usize),
    Group(usize, Box<Ast>),
    NonCap(Box<Ast>),
    Look(bool, Box<Ast>),
    LookBehind(bool, Box<Ast>),
    Concat(Vec<Ast>),
    Alt(Vec<Ast>),
    Repeat { node: Box<Ast>, min: usize, max: Option<usize>, greedy: bool },
}

struct Parser {
    c: Vec<char>,
    i: usize,
    ngroups: usize,
    group_names: Vec<(String, usize)>,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn next(&mut self) -> Option<char> {
        let c = self.c.get(self.i).copied();
        if c.is_some() {
            self.i += 1;
        }
        c
    }
    fn eat(&mut self, ch: char) -> bool {
        if self.peek() == Some(ch) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    // alt := concat ('|' concat)*
    fn parse_alt(&mut self) -> Result<Ast, String> {
        let mut branches = vec![self.parse_concat()?];
        while self.eat('|') {
            branches.push(self.parse_concat()?);
        }
        if branches.len() == 1 {
            Ok(branches.pop().unwrap())
        } else {
            Ok(Ast::Alt(branches))
        }
    }

    fn parse_concat(&mut self) -> Result<Ast, String> {
        let mut items = Vec::new();
        while let Some(ch) = self.peek() {
            if ch == '|' || ch == ')' {
                break;
            }
            items.push(self.parse_quant()?);
        }
        if items.is_empty() {
            Ok(Ast::Empty)
        } else if items.len() == 1 {
            Ok(items.pop().unwrap())
        } else {
            Ok(Ast::Concat(items))
        }
    }

    // quant := atom ('*'|'+'|'?'|'{n,m}') '?'?
    fn parse_quant(&mut self) -> Result<Ast, String> {
        let atom = self.parse_atom()?;
        let (min, max) = match self.peek() {
            Some('*') => {
                self.i += 1;
                (0, None)
            }
            Some('+') => {
                self.i += 1;
                (1, None)
            }
            Some('?') => {
                self.i += 1;
                (0, Some(1))
            }
            Some('{') => {
                if let Some((lo, hi, len)) = self.try_brace() {
                    self.i += len;
                    (lo, hi)
                } else {
                    return Ok(atom); // 리터럴 '{'
                }
            }
            _ => return Ok(atom),
        };
        let greedy = !self.eat('?');
        Ok(Ast::Repeat { node: Box::new(atom), min, max, greedy })
    }

    // {n} {n,} {n,m} → (min, max, 소비 길이). 아니면 None.
    fn try_brace(&self) -> Option<(usize, Option<usize>, usize)> {
        let mut j = self.i + 1;
        let mut lo = String::new();
        while j < self.c.len() && self.c[j].is_ascii_digit() {
            lo.push(self.c[j]);
            j += 1;
        }
        if lo.is_empty() {
            return None;
        }
        let min: usize = lo.parse().ok()?;
        if j < self.c.len() && self.c[j] == '}' {
            return Some((min, Some(min), j + 1 - self.i));
        }
        if j < self.c.len() && self.c[j] == ',' {
            j += 1;
            let mut hi = String::new();
            while j < self.c.len() && self.c[j].is_ascii_digit() {
                hi.push(self.c[j]);
                j += 1;
            }
            if j < self.c.len() && self.c[j] == '}' {
                let max = if hi.is_empty() { None } else { hi.parse().ok() };
                return Some((min, max, j + 1 - self.i));
            }
        }
        None
    }

    fn parse_atom(&mut self) -> Result<Ast, String> {
        match self.next() {
            Some('(') => {
                // (?: ...) 비캡처, (?= ...) 룩어헤드+, (?! ...) 룩어헤드-
                if self.eat('?') {
                    match self.next() {
                        Some(':') => {
                            let inner = self.parse_alt()?;
                            self.expect_close()?;
                            Ok(Ast::NonCap(Box::new(inner)))
                        }
                        Some('=') => {
                            let inner = self.parse_alt()?;
                            self.expect_close()?;
                            Ok(Ast::Look(false, Box::new(inner)))
                        }
                        Some('!') => {
                            let inner = self.parse_alt()?;
                            self.expect_close()?;
                            Ok(Ast::Look(true, Box::new(inner)))
                        }
                        // (?<name>...) 이름 있는 캡처 그룹 / (?<=..)(?<!..) 룩비하인드
                        Some('<') => match self.peek() {
                            Some('=') | Some('!') => {
                                let neg = self.peek() == Some('!');
                                self.i += 1;
                                let inner = self.parse_alt()?;
                                self.expect_close()?;
                                Ok(Ast::LookBehind(neg, Box::new(inner)))
                            }
                            _ => {
                                let mut name = String::new();
                                while let Some(c) = self.peek() {
                                    if c == '>' {
                                        break;
                                    }
                                    name.push(c);
                                    self.i += 1;
                                }
                                self.eat('>');
                                self.ngroups += 1;
                                let idx = self.ngroups;
                                self.group_names.push((name, idx));
                                let inner = self.parse_alt()?;
                                self.expect_close()?;
                                Ok(Ast::Group(idx, Box::new(inner)))
                            }
                        },
                        _ => Err("지원 안 하는 그룹".to_string()),
                    }
                } else {
                    self.ngroups += 1;
                    let idx = self.ngroups;
                    let inner = self.parse_alt()?;
                    self.expect_close()?;
                    Ok(Ast::Group(idx, Box::new(inner)))
                }
            }
            Some('[') => self.parse_class(),
            Some('.') => Ok(Ast::Any),
            Some('^') => Ok(Ast::Start),
            Some('$') => Ok(Ast::End),
            Some('\\') => self.parse_escape(),
            Some(ch) => Ok(Ast::Char(ch)),
            None => Ok(Ast::Empty),
        }
    }

    fn expect_close(&mut self) -> Result<(), String> {
        if self.eat(')') {
            Ok(())
        } else {
            Err("정규식 그룹 닫힘 ')' 필요".to_string())
        }
    }

    fn parse_escape(&mut self) -> Result<Ast, String> {
        let ch = self.next().ok_or("정규식 \\ 뒤 문자 필요")?;
        Ok(match ch {
            'd' => Ast::Class(kind_class(ClassKind::Digit(false))),
            'D' => Ast::Class(kind_class(ClassKind::Digit(true))),
            'w' => Ast::Class(kind_class(ClassKind::Word(false))),
            'W' => Ast::Class(kind_class(ClassKind::Word(true))),
            's' => Ast::Class(kind_class(ClassKind::Space(false))),
            'S' => Ast::Class(kind_class(ClassKind::Space(true))),
            'b' => Ast::WordBoundary(true),
            'B' => Ast::WordBoundary(false),
            'n' => Ast::Char('\n'),
            't' => Ast::Char('\t'),
            'r' => Ast::Char('\r'),
            'f' => Ast::Char('\u{0c}'),
            'v' => Ast::Char('\u{0b}'),
            '0' => Ast::Char('\0'),
            c @ '1'..='9' => {
                let mut num = String::from(c);
                while let Some(d) = self.peek() {
                    if d.is_ascii_digit() {
                        num.push(d);
                        self.i += 1;
                    } else {
                        break;
                    }
                }
                Ast::Backref(num.parse().unwrap_or(0))
            }
            'u' => Ast::Char(self.parse_unicode().unwrap_or('u')),
            'x' => Ast::Char(self.parse_hex2().unwrap_or('x')),
            other => Ast::Char(other), // \. \\ \/ 등 리터럴
        })
    }

    fn parse_unicode(&mut self) -> Option<char> {
        // \uXXXX 또는 \u{XXXX}
        if self.eat('{') {
            let mut hex = String::new();
            while let Some(c) = self.peek() {
                if c == '}' {
                    break;
                }
                hex.push(c);
                self.i += 1;
            }
            self.eat('}');
            char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
        } else {
            let mut hex = String::new();
            for _ in 0..4 {
                hex.push(self.next()?);
            }
            char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
        }
    }

    fn parse_hex2(&mut self) -> Option<char> {
        let mut hex = String::new();
        for _ in 0..2 {
            hex.push(self.next()?);
        }
        char::from_u32(u32::from_str_radix(&hex, 16).ok()?)
    }

    fn parse_class(&mut self) -> Result<Ast, String> {
        let neg = self.eat('^');
        let mut ranges = Vec::new();
        let mut kinds = Vec::new();
        // 첫 위치의 ']' 는 리터럴
        loop {
            match self.peek() {
                None => return Err("정규식 문자클래스 ']' 필요".to_string()),
                Some(']') => {
                    self.i += 1;
                    break;
                }
                _ => {}
            }
            let lo = self.class_char(&mut kinds)?;
            let Some(lo) = lo else { continue };
            // 범위 a-z
            if self.peek() == Some('-') && self.c.get(self.i + 1) != Some(&']') {
                self.i += 1; // '-'
                if let Some(hi) = self.class_char(&mut kinds)? {
                    ranges.push((lo, hi));
                } else {
                    ranges.push((lo, lo));
                    ranges.push(('-', '-'));
                }
            } else {
                ranges.push((lo, lo));
            }
        }
        Ok(Ast::Class(ClassData { neg, ranges, kinds }))
    }

    // 클래스 내부의 한 문자(또는 \d 류 → kinds 에 추가하고 None)
    fn class_char(&mut self, kinds: &mut Vec<ClassKind>) -> Result<Option<char>, String> {
        let ch = self.next().ok_or("정규식 클래스 조기 종료")?;
        if ch == '\\' {
            let e = self.next().ok_or("정규식 클래스 \\ 뒤 문자")?;
            return Ok(match e {
                'd' => {
                    kinds.push(ClassKind::Digit(false));
                    None
                }
                'D' => {
                    kinds.push(ClassKind::Digit(true));
                    None
                }
                'w' => {
                    kinds.push(ClassKind::Word(false));
                    None
                }
                'W' => {
                    kinds.push(ClassKind::Word(true));
                    None
                }
                's' => {
                    kinds.push(ClassKind::Space(false));
                    None
                }
                'S' => {
                    kinds.push(ClassKind::Space(true));
                    None
                }
                'n' => Some('\n'),
                't' => Some('\t'),
                'r' => Some('\r'),
                'f' => Some('\u{0c}'),
                'v' => Some('\u{0b}'),
                'b' => Some('\u{08}'),
                '0' => Some('\0'),
                'u' => self.parse_unicode(),
                'x' => self.parse_hex2(),
                other => Some(other),
            });
        }
        Ok(Some(ch))
    }
}

fn kind_class(k: ClassKind) -> ClassData {
    ClassData { neg: false, ranges: Vec::new(), kinds: vec![k] }
}

// ── 컴파일: AST → 명령어 ────────────────────────────────────────────
fn compile(ast: &Ast, prog: &mut Vec<Inst>) {
    match ast {
        Ast::Empty => {}
        Ast::Char(c) => prog.push(Inst::Char(*c)),
        Ast::Any => prog.push(Inst::Any),
        Ast::Class(cd) => prog.push(Inst::Class(Box::new(cd.clone()))),
        Ast::Start => prog.push(Inst::Start),
        Ast::End => prog.push(Inst::End),
        Ast::WordBoundary(b) => prog.push(Inst::WordBoundary(*b)),
        Ast::Backref(n) => prog.push(Inst::Backref(*n)),
        Ast::Concat(items) => {
            for it in items {
                compile(it, prog);
            }
        }
        Ast::Group(idx, inner) => {
            prog.push(Inst::Save(idx * 2));
            compile(inner, prog);
            prog.push(Inst::Save(idx * 2 + 1));
        }
        Ast::NonCap(inner) => compile(inner, prog),
        Ast::Look(neg, inner) => {
            let mut sub = Vec::new();
            compile(inner, &mut sub);
            sub.push(Inst::Match);
            prog.push(Inst::Look { neg: *neg, prog: sub });
        }
        Ast::LookBehind(neg, inner) => {
            let mut sub = Vec::new();
            compile(inner, &mut sub);
            sub.push(Inst::Match);
            prog.push(Inst::LookBehind { neg: *neg, prog: sub });
        }
        Ast::Alt(branches) => {
            // Split b0, (Split b1, ...); 각 브랜치 끝에 Jmp end
            let mut jmp_ends = Vec::new();
            for (k, b) in branches.iter().enumerate() {
                let last = k + 1 == branches.len();
                if !last {
                    let split_pc = prog.len();
                    prog.push(Inst::Split(0, 0));
                    let b_start = prog.len();
                    compile(b, prog);
                    let jmp_pc = prog.len();
                    prog.push(Inst::Jmp(0));
                    jmp_ends.push(jmp_pc);
                    let next = prog.len();
                    prog[split_pc] = Inst::Split(b_start, next);
                } else {
                    compile(b, prog);
                }
            }
            let end = prog.len();
            for j in jmp_ends {
                prog[j] = Inst::Jmp(end);
            }
        }
        Ast::Repeat { node, min, max, greedy } => {
            // min 회 필수 복제
            for _ in 0..*min {
                compile(node, prog);
            }
            match max {
                None => {
                    // (min 이후) 0회 이상: L: Split body, end; body; Jmp L
                    let l = prog.len();
                    let split_pc = prog.len();
                    prog.push(Inst::Split(0, 0));
                    let body = prog.len();
                    compile(node, prog);
                    prog.push(Inst::Jmp(l));
                    let end = prog.len();
                    prog[split_pc] = if *greedy {
                        Inst::Split(body, end)
                    } else {
                        Inst::Split(end, body)
                    };
                }
                Some(mx) => {
                    // (max-min) 회 선택적
                    let optional = mx.saturating_sub(*min);
                    let mut split_pcs = Vec::new();
                    for _ in 0..optional {
                        let split_pc = prog.len();
                        prog.push(Inst::Split(0, 0));
                        split_pcs.push(split_pc);
                        compile(node, prog);
                    }
                    let end = prog.len();
                    for sp in split_pcs {
                        let body = sp + 1;
                        prog[sp] = if *greedy {
                            Inst::Split(body, end)
                        } else {
                            Inst::Split(end, body)
                        };
                    }
                }
            }
        }
    }
}

const REGEX_STEP_LIMIT: usize = 2_000_000;

impl Regex {
    pub fn compile_pattern(source: &str, flags: &str) -> Result<Regex, String> {
        let mut p = Parser { c: source.chars().collect(), i: 0, ngroups: 0, group_names: Vec::new() };
        let ast = p.parse_alt()?;
        if p.i != p.c.len() {
            return Err(format!("정규식 파싱 실패 (위치 {})", p.i));
        }
        let ngroups = p.ngroups;
        let group_names = std::mem::take(&mut p.group_names);
        let mut prog = Vec::new();
        prog.push(Inst::Save(0));
        compile(&ast, &mut prog);
        prog.push(Inst::Save(1));
        prog.push(Inst::Match);
        Ok(Regex {
            prog,
            ngroups,
            group_names,
            ignore_case: flags.contains('i'),
            multiline: flags.contains('m'),
            dotall: flags.contains('s'),
            global: flags.contains('g'),
            require_end: None,
        })
    }

    // text 의 from 위치부터 첫 매치를 찾는다 (앵커 없는 한 위치 이동하며 시도).
    pub fn find(&self, text: &[char], from: usize) -> Option<Match> {
        let mut start = from;
        loop {
            let mut saves = vec![None; (self.ngroups + 1) * 2];
            let mut steps = 0usize;
            if self.run(0, text, start, &mut saves, &mut steps) {
                let s = saves[0].unwrap_or(start);
                let e = saves[1].unwrap_or(start);
                let mut groups = Vec::with_capacity(self.ngroups + 1);
                for g in 0..=self.ngroups {
                    match (saves.get(g * 2).copied().flatten(), saves.get(g * 2 + 1).copied().flatten()) {
                        (Some(a), Some(b)) => groups.push(Some((a, b))),
                        _ => groups.push(None),
                    }
                }
                return Some(Match { start: s, end: e, groups });
            }
            if start >= text.len() {
                return None;
            }
            start += 1;
        }
    }

    fn char_eq(&self, a: char, b: char) -> bool {
        if self.ignore_case {
            a.eq_ignore_ascii_case(&b)
                || a.to_lowercase().eq(b.to_lowercase())
        } else {
            a == b
        }
    }

    // 백트래킹 실행. pc 에서 시작해 Match 도달 시 true.
    fn run(
        &self,
        pc: usize,
        s: &[char],
        sp: usize,
        saves: &mut Vec<Option<usize>>,
        steps: &mut usize,
    ) -> bool {
        *steps += 1;
        if *steps > REGEX_STEP_LIMIT {
            return false;
        }
        match &self.prog[pc] {
            // 룩비하인드 서브매치는 정확히 그 위치에서 끝나야 한다.
            Inst::Match => self.require_end.map_or(true, |e| sp == e),
            Inst::Char(c) => {
                if sp < s.len() && self.char_eq(s[sp], *c) {
                    self.run(pc + 1, s, sp + 1, saves, steps)
                } else {
                    false
                }
            }
            Inst::Any => {
                if sp < s.len() && (self.dotall || s[sp] != '\n') {
                    self.run(pc + 1, s, sp + 1, saves, steps)
                } else {
                    false
                }
            }
            Inst::Class(cd) => {
                if sp < s.len() && self.class_match(cd, s[sp]) {
                    self.run(pc + 1, s, sp + 1, saves, steps)
                } else {
                    false
                }
            }
            Inst::Start => {
                let ok = sp == 0 || (self.multiline && sp > 0 && s[sp - 1] == '\n');
                ok && self.run(pc + 1, s, sp, saves, steps)
            }
            Inst::End => {
                let ok = sp == s.len() || (self.multiline && s[sp] == '\n');
                ok && self.run(pc + 1, s, sp, saves, steps)
            }
            Inst::WordBoundary(want) => {
                let before = sp > 0 && is_word(s[sp - 1]);
                let after = sp < s.len() && is_word(s[sp]);
                let boundary = before != after;
                (boundary == *want) && self.run(pc + 1, s, sp, saves, steps)
            }
            Inst::Save(slot) => {
                let old = saves[*slot];
                saves[*slot] = Some(sp);
                if self.run(pc + 1, s, sp, saves, steps) {
                    true
                } else {
                    saves[*slot] = old;
                    false
                }
            }
            Inst::Jmp(t) => self.run(*t, s, sp, saves, steps),
            Inst::Split(a, b) => {
                let snapshot = saves.clone();
                if self.run(*a, s, sp, saves, steps) {
                    true
                } else {
                    *saves = snapshot;
                    self.run(*b, s, sp, saves, steps)
                }
            }
            Inst::Backref(n) => {
                let (gs, ge) = match (saves.get(n * 2).copied().flatten(), saves.get(n * 2 + 1).copied().flatten()) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return self.run(pc + 1, s, sp, saves, steps), // 미캡처는 빈 매치
                };
                let len = ge - gs;
                if sp + len <= s.len() && (0..len).all(|k| self.char_eq(s[sp + k], s[gs + k])) {
                    self.run(pc + 1, s, sp + len, saves, steps)
                } else {
                    false
                }
            }
            Inst::Look { neg, prog } => {
                // 룩어라운드 **안의 캡처 그룹도 표준상 값을 남긴다** — /(?=(\d+))/ 는 그룹 1 을
                // 채운다. 예전엔 ngroups:0 에 임시 saves 를 써서 통째로 버렸다.
                let sub = Regex {
                    prog: prog.clone(),
                    ngroups: self.ngroups,
                    group_names: Vec::new(),
                    ignore_case: self.ignore_case,
                    multiline: self.multiline,
                    dotall: self.dotall,
                    global: false,
                    require_end: None,
                };
                let mut sub_saves = saves.clone();
                let mut sub_steps = 0;
                let matched = sub.run(0, s, sp, &mut sub_saves, &mut sub_steps);
                *steps += sub_steps;
                if matched != *neg {
                    if matched {
                        *saves = sub_saves; // 긍정 룩어라운드의 캡처는 유지
                    }
                    self.run(pc + 1, s, sp, saves, steps)
                } else {
                    false
                }
            }
            // (?<=X) / (?<!X): sp 에서 **끝나는** X 매치가 있는가. 시작 후보를 sp 에서
            // 0 쪽으로 훑으며 "정확히 sp 에서 끝나는" 매치를 찾는다 (require_end).
            Inst::LookBehind { neg, prog } => {
                let sub = Regex {
                    prog: prog.clone(),
                    ngroups: self.ngroups,
                    group_names: Vec::new(),
                    ignore_case: self.ignore_case,
                    multiline: self.multiline,
                    dotall: self.dotall,
                    global: false,
                    require_end: Some(sp),
                };
                let mut found = None;
                for j in (0..=sp).rev() {
                    let mut sub_saves = saves.clone();
                    let mut sub_steps = 0;
                    let ok = sub.run(0, s, j, &mut sub_saves, &mut sub_steps);
                    *steps += sub_steps;
                    if *steps > REGEX_STEP_LIMIT {
                        return false;
                    }
                    if ok {
                        found = Some(sub_saves);
                        break;
                    }
                }
                let matched = found.is_some();
                if matched != *neg {
                    if let Some(sv) = found {
                        *saves = sv;
                    }
                    self.run(pc + 1, s, sp, saves, steps)
                } else {
                    false
                }
            }
        }
    }

    fn class_match(&self, cd: &ClassData, c: char) -> bool {
        if self.ignore_case {
            cd.matches(c)
                || cd.matches(c.to_ascii_uppercase())
                || cd.matches(c.to_ascii_lowercase())
        } else {
            cd.matches(c)
        }
    }
}

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookbehind_and_lookaround_captures() {
        // (?<=..) / (?<!..) 는 ES2018 표준인데 "미지원" 오류를 던지고 있었다.
        let re = Regex::compile_pattern(r"(?<=\$)\d+", "").unwrap();
        let s: Vec<char> = "price: $42".chars().collect();
        let m = re.find(&s, 0).expect("룩비하인드 매치");
        assert_eq!(s[m.start..m.end].iter().collect::<String>(), "42");

        // 부정 룩비하인드
        let re = Regex::compile_pattern(r"(?<!a)\d", "").unwrap();
        let s: Vec<char> = "a1 b2".chars().collect();
        let m = re.find(&s, 0).expect("매치");
        assert_eq!(s[m.start..m.end].iter().collect::<String>(), "2");

        // 가변 길이 룩비하인드 (JS 는 허용한다 — Perl 과 다르다)
        let re = Regex::compile_pattern(r"(?<=fo+)bar", "").unwrap();
        let s: Vec<char> = "foobar".chars().collect();
        assert!(re.find(&s, 0).is_some());

        // 룩어라운드 안의 캡처 그룹은 값을 남긴다 (표준). 예전엔 통째로 버렸다.
        let re = Regex::compile_pattern(r"(?=(\d+))", "").unwrap();
        let s: Vec<char> = "123".chars().collect();
        let m = re.find(&s, 0).expect("매치");
        let g1 = m.groups.get(1).and_then(|g| *g).expect("그룹 1"); // 0 은 전체 매치
        assert_eq!(s[g1.0..g1.1].iter().collect::<String>(), "123");
    }

    fn m(pat: &str, flags: &str, text: &str) -> Option<(usize, usize)> {
        let re = Regex::compile_pattern(pat, flags).unwrap();
        let chars: Vec<char> = text.chars().collect();
        re.find(&chars, 0).map(|m| (m.start, m.end))
    }

    #[test]
    fn basics() {
        assert_eq!(m("abc", "", "xabcy"), Some((1, 4)));
        assert_eq!(m("a.c", "", "axc"), Some((0, 3)));
        assert_eq!(m("^abc$", "", "abc"), Some((0, 3)));
        assert_eq!(m("a+", "", "baaa"), Some((1, 4)));
        assert_eq!(m("a*", "", "bbb"), Some((0, 0)));
        assert_eq!(m("colou?r", "", "color"), Some((0, 5)));
        assert_eq!(m("colou?r", "", "colour"), Some((0, 6)));
    }

    #[test]
    fn classes_and_flags() {
        assert_eq!(m("[0-9]+", "", "ab123"), Some((2, 5)));
        assert_eq!(m("\\d+", "", "ab123"), Some((2, 5)));
        assert_eq!(m("[^a-z]+", "", "ab12"), Some((2, 4)));
        assert_eq!(m("ABC", "i", "xabc"), Some((1, 4)));
        assert_eq!(m("\\w+", "", "  hi_5!"), Some((2, 6)));
    }

    #[test]
    fn groups_alt_quant() {
        assert_eq!(m("(ab)+", "", "ababab"), Some((0, 6)));
        assert_eq!(m("cat|dog", "", "a dog"), Some((2, 5)));
        assert_eq!(m("a{2,3}", "", "aaaa"), Some((0, 3)));
        assert_eq!(m("a{2}", "", "aaaa"), Some((0, 2)));
        // 비탐욕
        assert_eq!(m("a+?", "", "aaa"), Some((0, 1)));
    }

    #[test]
    fn capture_groups() {
        let re = Regex::compile_pattern("(\\d+)-(\\d+)", "").unwrap();
        let chars: Vec<char> = "x 12-34".chars().collect();
        let mt = re.find(&chars, 0).unwrap();
        assert_eq!((mt.start, mt.end), (2, 7));
        assert_eq!(mt.groups[1], Some((2, 4)));
        assert_eq!(mt.groups[2], Some((5, 7)));
    }

    #[test]
    fn word_boundary_and_lookahead() {
        assert_eq!(m("\\bcat\\b", "", "the cat sat"), Some((4, 7)));
        assert_eq!(m("foo(?=bar)", "", "foobar"), Some((0, 3)));
        assert_eq!(m("foo(?!bar)", "", "foobaz"), Some((0, 3)));
        assert_eq!(m("foo(?!bar)", "", "foobar"), None);
    }
}
