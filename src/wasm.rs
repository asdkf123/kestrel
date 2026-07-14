// WebAssembly (MVP + 흔히 쓰는 확장) — 바이너리 파서 + 스택 머신.
//
// 왜 필요한가: 실제 사이트가 wasm 을 쓴다. fmkorea 의 봇 차단은 wasm 모듈로 챌린지를
// 풀고, 이미지·암호·압축 라이브러리가 점점 wasm 으로 간다. 없으면 그 페이지는 영영
// 인터스티셜만 보인다.
//
// 메모리는 JS 의 ArrayBuffer 바이트 배열을 **그대로** 쓴다 — 복사본을 두면
// new Uint8Array(memory.buffer) 가 죽은 사본을 보게 되어 조용히 틀린다.

use std::rc::Rc;

// ── 값 ────────────────────────────────────────────────────────────────────
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Val {
    I32(i32),
    I64(i64),
    F32(f32),
    F64(f64),
}

impl Val {
    pub fn as_i32(&self) -> i32 {
        match self {
            Val::I32(v) => *v,
            Val::I64(v) => *v as i32,
            Val::F32(v) => *v as i32,
            Val::F64(v) => *v as i32,
        }
    }
    pub fn as_i64(&self) -> i64 {
        match self {
            Val::I32(v) => *v as i64,
            Val::I64(v) => *v,
            Val::F32(v) => *v as i64,
            Val::F64(v) => *v as i64,
        }
    }
    pub fn as_f64(&self) -> f64 {
        match self {
            Val::I32(v) => *v as f64,
            Val::I64(v) => *v as f64,
            Val::F32(v) => *v as f64,
            Val::F64(v) => *v,
        }
    }
    fn zero(t: u8) -> Val {
        match t {
            0x7f => Val::I32(0),
            0x7e => Val::I64(0),
            0x7d => Val::F32(0.0),
            _ => Val::F64(0.0),
        }
    }
}

// ── 바이너리 리더 ─────────────────────────────────────────────────────────
struct Reader<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn new(d: &'a [u8]) -> Self {
        Reader { d, i: 0 }
    }
    fn eof(&self) -> bool {
        self.i >= self.d.len()
    }
    fn byte(&mut self) -> Result<u8, String> {
        let b = *self.d.get(self.i).ok_or("wasm: 예기치 않은 끝")?;
        self.i += 1;
        Ok(b)
    }
    fn bytes(&mut self, n: usize) -> Result<&'a [u8], String> {
        let s = self.d.get(self.i..self.i + n).ok_or("wasm: 예기치 않은 끝")?;
        self.i += n;
        Ok(s)
    }
    // LEB128 (부호 없음)
    fn u32(&mut self) -> Result<u32, String> {
        let mut r = 0u32;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            r |= ((b & 0x7f) as u32) << shift;
            if b & 0x80 == 0 {
                return Ok(r);
            }
            shift += 7;
            if shift > 31 {
                return Err("wasm: u32 LEB128 오버플로".to_string());
            }
        }
    }
    // LEB128 (부호 있음)
    fn i32v(&mut self) -> Result<i32, String> {
        let mut r = 0i64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            r |= ((b & 0x7f) as i64) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && (b & 0x40) != 0 {
                    r |= -1i64 << shift;
                }
                return Ok(r as i32);
            }
            if shift > 63 {
                return Err("wasm: i32 LEB128 오버플로".to_string());
            }
        }
    }
    fn i64v(&mut self) -> Result<i64, String> {
        let mut r = 0i64;
        let mut shift = 0;
        loop {
            let b = self.byte()?;
            r |= ((b & 0x7f) as i64) << shift;
            shift += 7;
            if b & 0x80 == 0 {
                if shift < 64 && (b & 0x40) != 0 {
                    r |= -1i64 << shift;
                }
                return Ok(r);
            }
            if shift > 70 {
                return Err("wasm: i64 LEB128 오버플로".to_string());
            }
        }
    }
    fn f32v(&mut self) -> Result<f32, String> {
        let b = self.bytes(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn f64v(&mut self) -> Result<f64, String> {
        let b = self.bytes(8)?;
        Ok(f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }
    fn name(&mut self) -> Result<String, String> {
        let n = self.u32()? as usize;
        let b = self.bytes(n)?;
        Ok(String::from_utf8_lossy(b).into_owned())
    }
}

// ── 모듈 ──────────────────────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub struct FuncType {
    pub params: Vec<u8>,
    pub results: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Import {
    pub module: String,
    pub name: String,
    pub kind: ImportKind,
}

#[derive(Clone, Debug)]
pub enum ImportKind {
    Func(u32),   // 타입 인덱스
    Memory(u32), // 최소 페이지
    Global(u8),  // 타입
    Table,
}

#[derive(Clone, Debug)]
pub enum Export {
    Func(u32),
    Memory,
    Global(u32),
    Table,
}

#[derive(Clone, Debug)]
pub struct Body {
    pub locals: Vec<u8>,
    pub code: Vec<Instr>,
}

#[derive(Clone, Debug, Default)]
pub struct Module {
    pub types: Vec<FuncType>,
    pub imports: Vec<Import>,
    pub func_types: Vec<u32>, // 로컬 함수의 타입 인덱스
    pub bodies: Vec<Body>,
    pub exports: Vec<(String, Export)>,
    pub mem_pages: Option<u32>,
    pub globals: Vec<(u8, Vec<Instr>)>, // (타입, 초기식)
    pub data: Vec<(Vec<Instr>, Vec<u8>)>,
    pub elems: Vec<(Vec<Instr>, Vec<u32>)>,
    // 패시브 세그먼트 (bulk memory). 인덱스는 **모든** 세그먼트를 순서대로 센 것 —
    // memory.init / table.init 이 그 인덱스로 참조한다.
    pub data_segments: Vec<Vec<u8>>,
    pub passive_elems: Vec<Vec<u32>>,
    pub table_size: u32,
    pub start: Option<u32>,
    pub imported_funcs: usize,
}

impl Module {
    pub fn imported_globals(&self) -> usize {
        self.imports
            .iter()
            .filter(|i| matches!(i.kind, ImportKind::Global(_)))
            .count()
    }

    // 전역 idx 의 값 타입 (임포트 전역이 먼저, 그다음 모듈 자신의 전역)
    pub fn global_type(&self, idx: usize) -> u8 {
        let ni = self.imported_globals();
        if idx < ni {
            let mut n = 0;
            for imp in &self.imports {
                if let ImportKind::Global(t) = imp.kind {
                    if n == idx {
                        return t;
                    }
                    n += 1;
                }
            }
        }
        self.globals.get(idx - ni).map(|(t, _)| *t).unwrap_or(0x7f)
    }

    // 임포트된 함수 idx(임포트 함수들 사이의 순번)의 시그니처
    pub fn import_func_type(&self, idx: usize) -> Option<&FuncType> {
        let mut n = 0;
        for imp in &self.imports {
            if let ImportKind::Func(t) = imp.kind {
                if n == idx {
                    return self.types.get(t as usize);
                }
                n += 1;
            }
        }
        None
    }
}

// ── 명령 ──────────────────────────────────────────────────────────────────
#[derive(Clone, Debug)]
pub enum Instr {
    Unreachable,
    Nop,
    Block(u8, Vec<Instr>),          // 결과 개수(0/1), 본문
    Loop(u8, Vec<Instr>),
    If(u8, Vec<Instr>, Vec<Instr>), // then, else
    Br(u32),
    BrIf(u32),
    BrTable(Vec<u32>, u32),
    Return,
    Call(u32),
    CallIndirect(u32),
    Drop,
    Select,
    LocalGet(u32),
    LocalSet(u32),
    LocalTee(u32),
    GlobalGet(u32),
    GlobalSet(u32),
    Load { op: u8, offset: u32 },
    Store { op: u8, offset: u32 },
    MemorySize,
    MemoryGrow,
    MemoryCopy,
    MemoryFill,
    // bulk memory / reference types
    MemoryInit(u32),
    DataDrop(u32),
    TableInit(u32),
    ElemDrop(u32),
    TableCopy,
    TableGet,
    TableSet,
    TableSize,
    TableGrow,
    TableFill,
    RefNull,
    RefIsNull,
    RefFunc(u32),
    I32Const(i32),
    I64Const(i64),
    F32Const(f32),
    F64Const(f64),
    Num(u8),      // 단순 수치 연산 (0x45..0xC4)
    NumFC(u32),   // 0xFC 접두 (trunc_sat 등)
}

pub fn parse(data: &[u8]) -> Result<Module, String> {
    if data.len() < 8 || &data[0..4] != b"\0asm" {
        return Err("wasm: 매직이 아니다".to_string());
    }
    let ver = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if ver != 1 {
        return Err(format!("wasm: 버전 {} 는 지원하지 않는다", ver));
    }
    let mut m = Module::default();
    let mut r = Reader::new(&data[8..]);
    while !r.eof() {
        let id = r.byte()?;
        let size = r.u32()? as usize;
        let body = r.bytes(size)?;
        let mut s = Reader::new(body);
        match id {
            1 => {
                // 타입
                let n = s.u32()?;
                for _ in 0..n {
                    if s.byte()? != 0x60 {
                        return Err("wasm: 함수 타입이 아니다".to_string());
                    }
                    let np = s.u32()?;
                    let params = (0..np).map(|_| s.byte()).collect::<Result<Vec<_>, _>>()?;
                    let nr = s.u32()?;
                    let results = (0..nr).map(|_| s.byte()).collect::<Result<Vec<_>, _>>()?;
                    m.types.push(FuncType { params, results });
                }
            }
            2 => {
                // 임포트
                let n = s.u32()?;
                for _ in 0..n {
                    let module = s.name()?;
                    let name = s.name()?;
                    let kind = match s.byte()? {
                        0x00 => {
                            let t = s.u32()?;
                            m.imported_funcs += 1;
                            ImportKind::Func(t)
                        }
                        0x01 => {
                            let _et = s.byte()?;
                            let limits = s.byte()?;
                            let _min = s.u32()?;
                            if limits == 1 {
                                let _max = s.u32()?;
                            }
                            ImportKind::Table
                        }
                        0x02 => {
                            let limits = s.byte()?;
                            let min = s.u32()?;
                            if limits == 1 {
                                let _max = s.u32()?;
                            }
                            ImportKind::Memory(min)
                        }
                        0x03 => {
                            let t = s.byte()?;
                            let _mutable = s.byte()?;
                            ImportKind::Global(t)
                        }
                        other => return Err(format!("wasm: 모르는 임포트 종류 {}", other)),
                    };
                    m.imports.push(Import { module, name, kind });
                }
            }
            3 => {
                let n = s.u32()?;
                for _ in 0..n {
                    m.func_types.push(s.u32()?);
                }
            }
            4 => {
                // 테이블
                let n = s.u32()?;
                for _ in 0..n {
                    let _et = s.byte()?;
                    let limits = s.byte()?;
                    let min = s.u32()?;
                    if limits == 1 {
                        let _max = s.u32()?;
                    }
                    m.table_size = m.table_size.max(min);
                }
            }
            5 => {
                // 메모리
                let n = s.u32()?;
                for _ in 0..n {
                    let limits = s.byte()?;
                    let min = s.u32()?;
                    if limits == 1 {
                        let _max = s.u32()?;
                    }
                    m.mem_pages = Some(min);
                }
            }
            6 => {
                let n = s.u32()?;
                for _ in 0..n {
                    let t = s.byte()?;
                    let _mutable = s.byte()?;
                    let init = parse_expr(&mut s)?;
                    m.globals.push((t, init));
                }
            }
            7 => {
                let n = s.u32()?;
                for _ in 0..n {
                    let name = s.name()?;
                    let kind = s.byte()?;
                    let idx = s.u32()?;
                    let e = match kind {
                        0x00 => Export::Func(idx),
                        0x01 => Export::Table,
                        0x02 => Export::Memory,
                        _ => Export::Global(idx),
                    };
                    m.exports.push((name, e));
                }
            }
            8 => m.start = Some(s.u32()?),
            9 => {
                // 요소 세그먼트. 형태는 flags 로 갈린다 (bulk-memory 확장):
                // 0=액티브(테이블0), 1=패시브, 2=액티브(테이블 지정), 3=선언,
                // 4/5/6/7 = 위와 같되 항목이 함수 인덱스가 아니라 식(expr)이다.
                let n = s.u32()?;
                for _ in 0..n {
                    let flags = s.u32()?;
                    let active = flags == 0 || flags == 2 || flags == 4 || flags == 6;
                    if flags == 2 || flags == 6 {
                        let _table = s.u32()?;
                    }
                    let off = if active { parse_expr(&mut s)? } else { Vec::new() };
                    // 패시브/선언 세그먼트는 element 종류 바이트가 붙는다
                    if matches!(flags, 1 | 2 | 3) {
                        let _kind = s.byte()?;
                    } else if matches!(flags, 5 | 7) {
                        let _reftype = s.byte()?;
                    }
                    let cnt = s.u32()?;
                    let mut fns = Vec::new();
                    for _ in 0..cnt {
                        if flags >= 4 {
                            // 식 형태: ref.func <idx> end | ref.null t end
                            let code = parse_expr(&mut s)?;
                            match code.first() {
                                Some(Instr::RefFunc(f)) => fns.push(*f),
                                _ => fns.push(u32::MAX), // ref.null → 빈 칸
                            }
                        } else {
                            fns.push(s.u32()?);
                        }
                    }
                    if active {
                        m.elems.push((off, fns.clone()));
                    }
                    // 패시브도 table.init 이 참조하므로 인덱스대로 보관한다
                    m.passive_elems.push(fns);
                }
            }
            10 => {
                let n = s.u32()?;
                for _ in 0..n {
                    let sz = s.u32()? as usize;
                    let b = s.bytes(sz)?;
                    let mut br = Reader::new(b);
                    let nl = br.u32()?;
                    let mut locals = Vec::new();
                    for _ in 0..nl {
                        let cnt = br.u32()?;
                        let t = br.byte()?;
                        for _ in 0..cnt {
                            locals.push(t);
                        }
                    }
                    let code = parse_block(&mut br)?;
                    m.bodies.push(Body { locals, code });
                }
            }
            11 => {
                // flags: 0=액티브(메모리0), 1=패시브, 2=액티브(메모리 지정)
                let n = s.u32()?;
                for _ in 0..n {
                    let flags = s.u32()?;
                    if flags == 2 {
                        let _mem = s.u32()?;
                    }
                    let off = if flags == 1 { Vec::new() } else { parse_expr(&mut s)? };
                    let sz = s.u32()? as usize;
                    let b = s.bytes(sz)?.to_vec();
                    if flags != 1 {
                        m.data.push((off, b.clone()));
                    }
                    m.data_segments.push(b);
                }
            }
            _ => {} // 커스텀 섹션 등은 건너뛴다
        }
    }
    Ok(m)
}

// 초기식 (end 로 끝나는 짧은 명령열)
fn parse_expr(r: &mut Reader) -> Result<Vec<Instr>, String> {
    parse_block(r)
}

// end(0x0B) 또는 else(0x05) 를 만날 때까지 명령을 읽는다. 종결자는 소비한다.
fn parse_block(r: &mut Reader) -> Result<Vec<Instr>, String> {
    let (code, _) = parse_until(r)?;
    Ok(code)
}

// 반환: (명령들, 종결자) — 종결자는 0x0B(end) 또는 0x05(else)
fn parse_until(r: &mut Reader) -> Result<(Vec<Instr>, u8), String> {
    let mut out = Vec::new();
    loop {
        if r.eof() {
            return Ok((out, 0x0B));
        }
        let op = r.byte()?;
        match op {
            0x0B => return Ok((out, 0x0B)),
            0x05 => return Ok((out, 0x05)),
            0x00 => out.push(Instr::Unreachable),
            0x01 => out.push(Instr::Nop),
            0x02 | 0x03 => {
                let bt = block_arity(r)?;
                let body = parse_block(r)?;
                out.push(if op == 0x02 {
                    Instr::Block(bt, body)
                } else {
                    Instr::Loop(bt, body)
                });
            }
            0x04 => {
                let bt = block_arity(r)?;
                let (then, term) = parse_until(r)?;
                let els = if term == 0x05 { parse_block(r)? } else { Vec::new() };
                out.push(Instr::If(bt, then, els));
            }
            0x0C => out.push(Instr::Br(r.u32()?)),
            0x0D => out.push(Instr::BrIf(r.u32()?)),
            0x0E => {
                let n = r.u32()?;
                let mut ts = Vec::new();
                for _ in 0..n {
                    ts.push(r.u32()?);
                }
                let d = r.u32()?;
                out.push(Instr::BrTable(ts, d));
            }
            0x0F => out.push(Instr::Return),
            0x10 => out.push(Instr::Call(r.u32()?)),
            0x11 => {
                let t = r.u32()?;
                let _table = r.byte()?;
                out.push(Instr::CallIndirect(t));
            }
            0x25 => {
                let _t = r.u32()?;
                out.push(Instr::TableGet);
            }
            0x26 => {
                let _t = r.u32()?;
                out.push(Instr::TableSet);
            }
            0xD0 => {
                let _t = r.byte()?;
                out.push(Instr::RefNull);
            }
            0xD1 => out.push(Instr::RefIsNull),
            0xD2 => out.push(Instr::RefFunc(r.u32()?)),
            0x1A => out.push(Instr::Drop),
            0x1B => out.push(Instr::Select),
            0x1C => {
                // select t* (타입 있는 select) — 타입은 무시하고 동작은 같다
                let n = r.u32()?;
                for _ in 0..n {
                    let _ = r.byte()?;
                }
                out.push(Instr::Select);
            }
            0x20 => out.push(Instr::LocalGet(r.u32()?)),
            0x21 => out.push(Instr::LocalSet(r.u32()?)),
            0x22 => out.push(Instr::LocalTee(r.u32()?)),
            0x23 => out.push(Instr::GlobalGet(r.u32()?)),
            0x24 => out.push(Instr::GlobalSet(r.u32()?)),
            0x28..=0x35 => {
                let _align = r.u32()?;
                let offset = r.u32()?;
                out.push(Instr::Load { op, offset });
            }
            0x36..=0x3E => {
                let _align = r.u32()?;
                let offset = r.u32()?;
                out.push(Instr::Store { op, offset });
            }
            0x3F => {
                let _ = r.byte()?;
                out.push(Instr::MemorySize);
            }
            0x40 => {
                let _ = r.byte()?;
                out.push(Instr::MemoryGrow);
            }
            0x41 => out.push(Instr::I32Const(r.i32v()?)),
            0x42 => out.push(Instr::I64Const(r.i64v()?)),
            0x43 => out.push(Instr::F32Const(r.f32v()?)),
            0x44 => out.push(Instr::F64Const(r.f64v()?)),
            0x45..=0xC4 => out.push(Instr::Num(op)),
            0xFC => {
                let sub = r.u32()?;
                match sub {
                    0..=7 => out.push(Instr::NumFC(sub)), // trunc_sat
                    8 => {
                        let seg = r.u32()?;
                        let _mem = r.byte()?;
                        out.push(Instr::MemoryInit(seg));
                    }
                    9 => out.push(Instr::DataDrop(r.u32()?)),
                    // memory.copy / memory.fill 은 피연산자에 메모리 인덱스가 붙는다
                    10 => {
                        let _ = r.byte()?;
                        let _ = r.byte()?;
                        out.push(Instr::MemoryCopy);
                    }
                    11 => {
                        let _ = r.byte()?;
                        out.push(Instr::MemoryFill);
                    }
                    12 => {
                        let seg = r.u32()?;
                        let _table = r.u32()?;
                        out.push(Instr::TableInit(seg));
                    }
                    13 => out.push(Instr::ElemDrop(r.u32()?)),
                    14 => {
                        let _dst = r.u32()?;
                        let _src = r.u32()?;
                        out.push(Instr::TableCopy);
                    }
                    15 => {
                        let _t = r.u32()?;
                        out.push(Instr::TableGrow);
                    }
                    16 => {
                        let _t = r.u32()?;
                        out.push(Instr::TableSize);
                    }
                    17 => {
                        let _t = r.u32()?;
                        out.push(Instr::TableFill);
                    }
                    other => {
                        return Err(format!("wasm: 0xFC {} 는 아직 미지원", other));
                    }
                }
            }
            other => return Err(format!("wasm: 모르는 명령 0x{:02x}", other)),
        }
    }
}

// 블록 타입 → 결과 개수 (0 또는 1). 다중 결과(타입 인덱스)는 그 타입의 결과 수.
fn block_arity(r: &mut Reader) -> Result<u8, String> {
    let b = *r.d.get(r.i).ok_or("wasm: 블록 타입 없음")?;
    if b == 0x40 {
        r.i += 1;
        return Ok(0);
    }
    if (0x7c..=0x7f).contains(&b) || b == 0x70 || b == 0x6f {
        r.i += 1;
        return Ok(1);
    }
    // 타입 인덱스 (다중 값). 부호 있는 LEB — 여기서는 결과 1개로 근사하면 위험하므로
    // 정직하게 거부한다 (조용히 스택을 어긋나게 하는 것보다 낫다).
    Err("wasm: 다중 값 블록 타입은 아직 미지원".to_string())
}

// ── 인스턴스 ──────────────────────────────────────────────────────────────
// 호스트(JS)로 나가는 호출.
pub trait Host {
    fn call_import(&mut self, idx: usize, args: &[Val]) -> Result<Vec<Val>, String>;
}

// 선형 메모리. JS 의 ArrayBuffer._b (바이트 값이 든 JS 배열) 를 **그대로** 가리킨다.
// 그래서 new Uint8Array(memory.buffer) 는 살아있는 뷰가 된다 — 사본이 아니다.
// grow 는 새 배열로 갈아끼우므로 RefCell 로 감싼다 (표준: 옛 버퍼는 분리된다).
pub type MemRef = Rc<std::cell::RefCell<Rc<crate::js::interp::ArrayObj>>>;

pub struct Instance {
    pub module: Rc<Module>,
    pub globals: std::cell::RefCell<Vec<Val>>,
    pub table: std::cell::RefCell<Vec<Option<u32>>>,
    pub mem: Option<MemRef>,
    // data.drop / elem.drop 으로 버려진 세그먼트. 버린 뒤 init 하면 트랩해야 한다
    // (조용히 옛 내용을 다시 쓰면 틀린 데이터가 들어간다).
    dropped_data: std::cell::RefCell<Vec<bool>>,
    dropped_elems: std::cell::RefCell<Vec<bool>>,
}

pub const PAGE: usize = 65536;
const MAX_STEPS: u64 = 200_000_000;
const MAX_PAGES: usize = 4096; // 256MB — 우리 메모리 표현(JS 배열)의 현실적 상한

fn byte_of(v: &crate::js::interp::Value) -> u64 {
    match v {
        crate::js::interp::Value::Num(x) => *x as i64 as u64 & 0xff,
        _ => 0,
    }
}

impl Instance {
    pub fn mem_len(&self) -> usize {
        self.mem.as_ref().map(|m| m.borrow().borrow().len()).unwrap_or(0)
    }

    fn read(&self, addr: usize, n: usize) -> Result<u64, String> {
        let Some(m) = &self.mem else { return Err("wasm: 메모리가 없다".to_string()) };
        let cell = m.borrow();
        let b = cell.borrow();
        if addr.checked_add(n).map(|e| e > b.len()).unwrap_or(true) {
            return Err("wasm: 메모리 범위 밖 접근".to_string());
        }
        let mut v = 0u64;
        for k in 0..n {
            v |= byte_of(&b[addr + k]) << (8 * k);
        }
        Ok(v)
    }

    fn write(&self, addr: usize, n: usize, v: u64) -> Result<(), String> {
        let Some(m) = &self.mem else { return Err("wasm: 메모리가 없다".to_string()) };
        let cell = m.borrow();
        let mut b = cell.borrow_mut();
        if addr.checked_add(n).map(|e| e > b.len()).unwrap_or(true) {
            return Err("wasm: 메모리 범위 밖 쓰기".to_string());
        }
        for k in 0..n {
            b[addr + k] = crate::js::interp::Value::Num(((v >> (8 * k)) & 0xff) as f64);
        }
        Ok(())
    }

    // memory.grow — 실패하면 -1 (표준). 성공하면 이전 페이지 수.
    // 새 배열로 갈아끼운다: 옛 ArrayBuffer 는 호출자(JS 쪽)가 분리한다.
    pub fn grow(&self, pages: u32) -> i32 {
        let Some(m) = &self.mem else { return -1 };
        grow_mem(m, pages)
    }

    pub fn func_type(&self, idx: u32) -> Option<&FuncType> {
        let m = &self.module;
        let i = idx as usize;
        if i < m.imported_funcs {
            // 임포트 함수
            let mut n = 0;
            for imp in &m.imports {
                if let ImportKind::Func(t) = imp.kind {
                    if n == i {
                        return m.types.get(t as usize);
                    }
                    n += 1;
                }
            }
            None
        } else {
            let t = *m.func_types.get(i - m.imported_funcs)?;
            m.types.get(t as usize)
        }
    }

    // 함수 호출 (인덱스는 임포트 포함 전체 공간)
    pub fn call(&self, host: &mut dyn Host, idx: u32, args: &[Val]) -> Result<Vec<Val>, String> {
        let mut steps = 0u64;
        self.call_inner(host, idx, args, &mut steps, 0)
    }

    fn call_inner(
        &self,
        host: &mut dyn Host,
        idx: u32,
        args: &[Val],
        steps: &mut u64,
        depth: u32,
    ) -> Result<Vec<Val>, String> {
        if depth > 512 {
            return Err("wasm: 호출 스택 초과".to_string());
        }
        let m = self.module.clone();
        let i = idx as usize;
        if i < m.imported_funcs {
            return host.call_import(i, args);
        }
        let body = m
            .bodies
            .get(i - m.imported_funcs)
            .ok_or_else(|| format!("wasm: 함수 {} 없음", idx))?;
        let ft = self
            .func_type(idx)
            .ok_or_else(|| format!("wasm: 함수 {} 의 타입 없음", idx))?
            .clone();

        let mut locals: Vec<Val> = Vec::with_capacity(ft.params.len() + body.locals.len());
        for (k, t) in ft.params.iter().enumerate() {
            locals.push(args.get(k).copied().unwrap_or(Val::zero(*t)));
        }
        for t in &body.locals {
            locals.push(Val::zero(*t));
        }
        let mut stack: Vec<Val> = Vec::new();
        match self.exec(host, &body.code, &mut locals, &mut stack, steps, depth)? {
            Flow::Normal | Flow::Return => {}
            Flow::Br(_) => return Err("wasm: 함수 밖으로 분기".to_string()),
        }
        let n = ft.results.len();
        let start = stack.len().saturating_sub(n);
        Ok(stack.split_off(start))
    }

    #[allow(clippy::too_many_arguments)]
    fn exec(
        &self,
        host: &mut dyn Host,
        code: &[Instr],
        locals: &mut Vec<Val>,
        stack: &mut Vec<Val>,
        steps: &mut u64,
        depth: u32,
    ) -> Result<Flow, String> {
        for ins in code {
            *steps += 1;
            if *steps > MAX_STEPS {
                return Err("wasm: 실행 한도 초과 (무한 루프?)".to_string());
            }
            match ins {
                Instr::Unreachable => return Err("wasm: unreachable".to_string()),
                Instr::Nop => {}
                Instr::Block(arity, body) => {
                    let base = stack.len();
                    match self.exec(host, body, locals, stack, steps, depth)? {
                        Flow::Normal => {}
                        Flow::Return => return Ok(Flow::Return),
                        Flow::Br(0) => {
                            // 블록 탈출: 결과만 남긴다
                            keep_results(stack, base, *arity as usize);
                        }
                        Flow::Br(n) => return Ok(Flow::Br(n - 1)),
                    }
                }
                Instr::Loop(arity, body) => {
                    let base = stack.len();
                    loop {
                        match self.exec(host, body, locals, stack, steps, depth)? {
                            Flow::Normal => break,
                            Flow::Return => return Ok(Flow::Return),
                            // loop 의 br 0 은 **처음으로 되돌아간다**
                            Flow::Br(0) => {
                                stack.truncate(base);
                                *steps += 1;
                                if *steps > MAX_STEPS {
                                    return Err("wasm: 실행 한도 초과 (무한 루프?)".to_string());
                                }
                                continue;
                            }
                            Flow::Br(n) => return Ok(Flow::Br(n - 1)),
                        }
                    }
                    let _ = arity;
                }
                Instr::If(arity, then, els) => {
                    let c = pop(stack)?.as_i32();
                    let base = stack.len();
                    let body = if c != 0 { then } else { els };
                    match self.exec(host, body, locals, stack, steps, depth)? {
                        Flow::Normal => {}
                        Flow::Return => return Ok(Flow::Return),
                        Flow::Br(0) => keep_results(stack, base, *arity as usize),
                        Flow::Br(n) => return Ok(Flow::Br(n - 1)),
                    }
                }
                Instr::Br(n) => return Ok(Flow::Br(*n)),
                Instr::BrIf(n) => {
                    if pop(stack)?.as_i32() != 0 {
                        return Ok(Flow::Br(*n));
                    }
                }
                Instr::BrTable(ts, d) => {
                    let i = pop(stack)?.as_i32();
                    let n = if i >= 0 && (i as usize) < ts.len() {
                        ts[i as usize]
                    } else {
                        *d
                    };
                    return Ok(Flow::Br(n));
                }
                Instr::Return => return Ok(Flow::Return),
                Instr::Call(f) => {
                    let ft = self
                        .func_type(*f)
                        .ok_or_else(|| format!("wasm: 함수 {} 의 타입 없음", f))?
                        .clone();
                    let n = ft.params.len();
                    if stack.len() < n {
                        return Err("wasm: 인자가 모자라다".to_string());
                    }
                    let args: Vec<Val> = stack.split_off(stack.len() - n);
                    let rs = self.call_inner(host, *f, &args, steps, depth + 1)?;
                    stack.extend(rs);
                }
                Instr::CallIndirect(t) => {
                    let ti = pop(stack)?.as_i32();
                    let f = self
                        .table
                        .borrow()
                        .get(ti as usize)
                        .and_then(|x| *x)
                        .ok_or("wasm: 테이블 항목이 비었다")?;
                    let ft = self
                        .module
                        .types
                        .get(*t as usize)
                        .ok_or("wasm: 타입 없음")?
                        .clone();
                    let n = ft.params.len();
                    if stack.len() < n {
                        return Err("wasm: 인자가 모자라다".to_string());
                    }
                    let args: Vec<Val> = stack.split_off(stack.len() - n);
                    let rs = self.call_inner(host, f, &args, steps, depth + 1)?;
                    stack.extend(rs);
                }
                Instr::Drop => {
                    pop(stack)?;
                }
                Instr::Select => {
                    let c = pop(stack)?.as_i32();
                    let b = pop(stack)?;
                    let a = pop(stack)?;
                    stack.push(if c != 0 { a } else { b });
                }
                Instr::LocalGet(i) => {
                    let v = *locals.get(*i as usize).ok_or("wasm: 로컬 없음")?;
                    stack.push(v);
                }
                Instr::LocalSet(i) => {
                    let v = pop(stack)?;
                    *locals.get_mut(*i as usize).ok_or("wasm: 로컬 없음")? = v;
                }
                Instr::LocalTee(i) => {
                    let v = *stack.last().ok_or("wasm: 스택 비었음")?;
                    *locals.get_mut(*i as usize).ok_or("wasm: 로컬 없음")? = v;
                }
                Instr::GlobalGet(i) => {
                    let v = *self
                        .globals
                        .borrow()
                        .get(*i as usize)
                        .ok_or("wasm: 전역 없음")?;
                    stack.push(v);
                }
                Instr::GlobalSet(i) => {
                    let v = pop(stack)?;
                    let mut g = self.globals.borrow_mut();
                    *g.get_mut(*i as usize).ok_or("wasm: 전역 없음")? = v;
                }
                Instr::Load { op, offset } => {
                    let addr = pop(stack)?.as_i32() as u32 as usize + *offset as usize;
                    let v = self.do_load(*op, addr)?;
                    stack.push(v);
                }
                Instr::Store { op, offset } => {
                    let v = pop(stack)?;
                    let addr = pop(stack)?.as_i32() as u32 as usize + *offset as usize;
                    self.do_store(*op, addr, v)?;
                }
                Instr::MemorySize => stack.push(Val::I32((self.mem_len() / PAGE) as i32)),
                Instr::MemoryGrow => {
                    let n = pop(stack)?.as_i32();
                    stack.push(Val::I32(self.grow(n.max(0) as u32)));
                }
                Instr::MemoryCopy => {
                    let n = pop(stack)?.as_i32() as usize;
                    let src = pop(stack)?.as_i32() as u32 as usize;
                    let dst = pop(stack)?.as_i32() as u32 as usize;
                    for k in 0..n {
                        let b = self.read(src + k, 1)?;
                        self.write(dst + k, 1, b)?;
                    }
                }
                Instr::MemoryFill => {
                    let n = pop(stack)?.as_i32() as usize;
                    let val = pop(stack)?.as_i32() as u64 & 0xff;
                    let dst = pop(stack)?.as_i32() as u32 as usize;
                    for k in 0..n {
                        self.write(dst + k, 1, val)?;
                    }
                }
                // bulk memory: 패시브 세그먼트 → 메모리/테이블
                Instr::MemoryInit(seg) => {
                    let n = pop(stack)?.as_i32() as usize;
                    let src = pop(stack)?.as_i32() as u32 as usize;
                    let dst = pop(stack)?.as_i32() as u32 as usize;
                    if *self
                        .dropped_data
                        .borrow()
                        .get(*seg as usize)
                        .unwrap_or(&true)
                    {
                        if n > 0 {
                            return Err("wasm: 버려진 data 세그먼트를 init 했다".to_string());
                        }
                    } else {
                        let bytes = self
                            .module
                            .data_segments
                            .get(*seg as usize)
                            .ok_or("wasm: data 세그먼트 없음")?
                            .clone();
                        if src + n > bytes.len() {
                            return Err("wasm: data 세그먼트 범위 밖".to_string());
                        }
                        for k in 0..n {
                            self.write(dst + k, 1, bytes[src + k] as u64)?;
                        }
                    }
                }
                Instr::DataDrop(seg) => {
                    if let Some(f) = self.dropped_data.borrow_mut().get_mut(*seg as usize) {
                        *f = true;
                    }
                }
                Instr::TableInit(seg) => {
                    let n = pop(stack)?.as_i32() as usize;
                    let src = pop(stack)?.as_i32() as usize;
                    let dst = pop(stack)?.as_i32() as usize;
                    if *self
                        .dropped_elems
                        .borrow()
                        .get(*seg as usize)
                        .unwrap_or(&true)
                    {
                        if n > 0 {
                            return Err("wasm: 버려진 elem 세그먼트를 init 했다".to_string());
                        }
                    } else {
                        let fns = self
                            .module
                            .passive_elems
                            .get(*seg as usize)
                            .ok_or("wasm: elem 세그먼트 없음")?
                            .clone();
                        let mut t = self.table.borrow_mut();
                        if dst + n > t.len() {
                            t.resize(dst + n, None);
                        }
                        for k in 0..n {
                            let f = *fns.get(src + k).ok_or("wasm: elem 범위 밖")?;
                            t[dst + k] = if f == u32::MAX { None } else { Some(f) };
                        }
                    }
                }
                Instr::ElemDrop(seg) => {
                    if let Some(f) = self.dropped_elems.borrow_mut().get_mut(*seg as usize) {
                        *f = true;
                    }
                }
                Instr::TableCopy => {
                    let n = pop(stack)?.as_i32() as usize;
                    let src = pop(stack)?.as_i32() as usize;
                    let dst = pop(stack)?.as_i32() as usize;
                    let mut t = self.table.borrow_mut();
                    if src + n > t.len() || dst + n > t.len() {
                        return Err("wasm: 테이블 범위 밖 복사".to_string());
                    }
                    // 겹칠 수 있다 — 먼저 읽어 두고 쓴다
                    let src_vals: Vec<Option<u32>> = t[src..src + n].to_vec();
                    t[dst..dst + n].clone_from_slice(&src_vals);
                }
                Instr::TableGet => {
                    let i = pop(stack)?.as_i32() as usize;
                    let f = self.table.borrow().get(i).copied().flatten();
                    stack.push(Val::I32(match f {
                        Some(x) => x as i32,
                        None => -1, // null 참조
                    }));
                }
                Instr::TableSet => {
                    let v = pop(stack)?.as_i32();
                    let i = pop(stack)?.as_i32() as usize;
                    let mut t = self.table.borrow_mut();
                    if i >= t.len() {
                        return Err("wasm: 테이블 범위 밖 쓰기".to_string());
                    }
                    t[i] = if v < 0 { None } else { Some(v as u32) };
                }
                Instr::TableSize => stack.push(Val::I32(self.table.borrow().len() as i32)),
                Instr::TableGrow => {
                    let n = pop(stack)?.as_i32() as usize;
                    let init = pop(stack)?.as_i32();
                    let mut t = self.table.borrow_mut();
                    let old = t.len() as i32;
                    let v = if init < 0 { None } else { Some(init as u32) };
                    t.resize(old as usize + n, v);
                    stack.push(Val::I32(old));
                }
                Instr::TableFill => {
                    let n = pop(stack)?.as_i32() as usize;
                    let v = pop(stack)?.as_i32();
                    let i = pop(stack)?.as_i32() as usize;
                    let mut t = self.table.borrow_mut();
                    if i + n > t.len() {
                        return Err("wasm: 테이블 범위 밖 채우기".to_string());
                    }
                    let v = if v < 0 { None } else { Some(v as u32) };
                    for k in 0..n {
                        t[i + k] = v;
                    }
                }
                // 참조: funcref 는 함수 인덱스로, null 은 -1 로 표현한다
                Instr::RefNull => stack.push(Val::I32(-1)),
                Instr::RefIsNull => {
                    let v = pop(stack)?.as_i32();
                    stack.push(Val::I32(if v < 0 { 1 } else { 0 }));
                }
                Instr::RefFunc(f) => stack.push(Val::I32(*f as i32)),
                Instr::I32Const(v) => stack.push(Val::I32(*v)),
                Instr::I64Const(v) => stack.push(Val::I64(*v)),
                Instr::F32Const(v) => stack.push(Val::F32(*v)),
                Instr::F64Const(v) => stack.push(Val::F64(*v)),
                Instr::Num(op) => num_op(*op, stack)?,
                Instr::NumFC(sub) => num_fc(*sub, stack)?,
            }
        }
        Ok(Flow::Normal)
    }

    fn do_load(&self, op: u8, addr: usize) -> Result<Val, String> {
        Ok(match op {
            0x28 => Val::I32(self.read(addr, 4)? as u32 as i32),
            0x29 => Val::I64(self.read(addr, 8)? as i64),
            0x2A => Val::F32(f32::from_bits(self.read(addr, 4)? as u32)),
            0x2B => Val::F64(f64::from_bits(self.read(addr, 8)?)),
            0x2C => Val::I32(self.read(addr, 1)? as u8 as i8 as i32),
            0x2D => Val::I32(self.read(addr, 1)? as i32),
            0x2E => Val::I32(self.read(addr, 2)? as u16 as i16 as i32),
            0x2F => Val::I32(self.read(addr, 2)? as i32),
            0x30 => Val::I64(self.read(addr, 1)? as u8 as i8 as i64),
            0x31 => Val::I64(self.read(addr, 1)? as i64),
            0x32 => Val::I64(self.read(addr, 2)? as u16 as i16 as i64),
            0x33 => Val::I64(self.read(addr, 2)? as i64),
            0x34 => Val::I64(self.read(addr, 4)? as u32 as i32 as i64),
            0x35 => Val::I64(self.read(addr, 4)? as i64),
            _ => return Err(format!("wasm: 모르는 load 0x{:02x}", op)),
        })
    }

    fn do_store(&self, op: u8, addr: usize, v: Val) -> Result<(), String> {
        match op {
            0x36 => self.write(addr, 4, v.as_i32() as u32 as u64)?,
            0x37 => self.write(addr, 8, v.as_i64() as u64)?,
            0x38 => self.write(addr, 4, (v.as_f64() as f32).to_bits() as u64)?,
            0x39 => self.write(addr, 8, v.as_f64().to_bits())?,
            0x3A => self.write(addr, 1, v.as_i32() as u64 & 0xff)?,
            0x3B => self.write(addr, 2, v.as_i32() as u64 & 0xffff)?,
            0x3C => self.write(addr, 1, v.as_i64() as u64 & 0xff)?,
            0x3D => self.write(addr, 2, v.as_i64() as u64 & 0xffff)?,
            0x3E => self.write(addr, 4, v.as_i64() as u64 & 0xffff_ffff)?,
            _ => return Err(format!("wasm: 모르는 store 0x{:02x}", op)),
        }
        Ok(())
    }
}

enum Flow {
    Normal,
    Return,
    Br(u32),
}

// 메모리를 pages 만큼 키운다. 이전 페이지 수를 돌려주고, 실패하면 -1.
// 새 JS 배열을 만들어 내용을 복사하고 그것으로 갈아끼운다 — 표준의 "옛 버퍼 분리".
pub fn grow_mem(m: &MemRef, pages: u32) -> i32 {
    use crate::js::interp::{ArrayObj, Value};
    let old_arr = m.borrow().clone();
    let old_len = old_arr.borrow().len();
    let old_pages = (old_len / PAGE) as i32;
    let add = pages as usize * PAGE;
    if (old_len + add) / PAGE > MAX_PAGES {
        return -1;
    }
    let mut items: Vec<Value> = Vec::with_capacity(old_len + add);
    items.extend(old_arr.borrow().iter().cloned());
    items.extend(std::iter::repeat_n(Value::Num(0.0), add));
    *m.borrow_mut() = ArrayObj::new(items);
    old_pages
}

// 임포트로 들어오는 것들 (JS 쪽이 채워 준다)
pub enum Extern {
    Func, // 실제 호출은 Host 가 처리 (임포트 함수 인덱스 순서로)
    Memory(MemRef),
    Global(Val),
}

// 모듈 + 임포트 → 인스턴스. 데이터/요소 세그먼트를 적용하고 start 를 부른다.
pub fn instantiate(
    module: Rc<Module>,
    imports: Vec<Extern>,
    own_mem: Option<MemRef>,
    host: &mut dyn Host,
) -> Result<Instance, String> {
    use crate::js::interp::{ArrayObj, Value};

    // 메모리: 임포트가 있으면 그것을, 없고 모듈이 정의했으면 own_mem 을 쓴다.
    let mut mem: Option<MemRef> = None;
    let mut globals: Vec<Val> = Vec::new();
    for imp in &imports {
        match imp {
            Extern::Memory(m) => mem = Some(m.clone()),
            Extern::Global(v) => globals.push(*v),
            _ => {}
        }
    }
    if mem.is_none() {
        if let Some(pages) = module.mem_pages {
            let m = own_mem.unwrap_or_else(|| {
                Rc::new(std::cell::RefCell::new(ArrayObj::new(vec![
                    Value::Num(0.0);
                    pages as usize * PAGE
                ])))
            });
            mem = Some(m);
        }
    }

    // 모듈 자신의 전역 (임포트 전역 뒤에 이어진다)
    for (_, init) in &module.globals {
        let v = eval_const(init, &globals)?;
        globals.push(v);
    }

    let inst = Instance {
        globals: std::cell::RefCell::new(globals),
        table: std::cell::RefCell::new(vec![None; module.table_size as usize]),
        mem,
        dropped_data: std::cell::RefCell::new(vec![false; module.data_segments.len()]),
        dropped_elems: std::cell::RefCell::new(vec![false; module.passive_elems.len()]),
        module: module.clone(),
    };

    // 요소 세그먼트 → 테이블
    for (off, fns) in &module.elems {
        let base = eval_const(off, &inst.globals.borrow())?.as_i32() as usize;
        let mut t = inst.table.borrow_mut();
        if base + fns.len() > t.len() {
            t.resize(base + fns.len(), None);
        }
        for (k, f) in fns.iter().enumerate() {
            t[base + k] = Some(*f);
        }
    }

    // 데이터 세그먼트 → 메모리
    for (off, bytes) in &module.data {
        let base = eval_const(off, &inst.globals.borrow())?.as_i32() as u32 as usize;
        for (k, b) in bytes.iter().enumerate() {
            inst.write(base + k, 1, *b as u64)?;
        }
    }

    if let Some(s) = module.start {
        inst.call(host, s, &[])?;
    }
    Ok(inst)
}

fn keep_results(stack: &mut Vec<Val>, base: usize, arity: usize) {
    if stack.len() >= base + arity {
        let start = stack.len() - arity;
        let res: Vec<Val> = stack.split_off(start);
        stack.truncate(base);
        stack.extend(res);
    } else {
        stack.truncate(base);
    }
}

fn pop(stack: &mut Vec<Val>) -> Result<Val, String> {
    stack.pop().ok_or_else(|| "wasm: 스택이 비었다".to_string())
}

// ── 수치 연산 ─────────────────────────────────────────────────────────────
fn num_op(op: u8, s: &mut Vec<Val>) -> Result<(), String> {
    macro_rules! i32_bin {
        ($f:expr) => {{
            let b = pop(s)?.as_i32();
            let a = pop(s)?.as_i32();
            let r: Result<i32, String> = $f(a, b);
            s.push(Val::I32(r?));
        }};
    }
    macro_rules! i64_bin {
        ($f:expr) => {{
            let b = pop(s)?.as_i64();
            let a = pop(s)?.as_i64();
            let r: Result<i64, String> = $f(a, b);
            s.push(Val::I64(r?));
        }};
    }
    macro_rules! i32_cmp {
        ($f:expr) => {{
            let b = pop(s)?.as_i32();
            let a = pop(s)?.as_i32();
            s.push(Val::I32(if $f(a, b) { 1 } else { 0 }));
        }};
    }
    macro_rules! i64_cmp {
        ($f:expr) => {{
            let b = pop(s)?.as_i64();
            let a = pop(s)?.as_i64();
            s.push(Val::I32(if $f(a, b) { 1 } else { 0 }));
        }};
    }
    macro_rules! f64_bin {
        ($f:expr) => {{
            let b = pop(s)?.as_f64();
            let a = pop(s)?.as_f64();
            s.push(Val::F64($f(a, b)));
        }};
    }
    macro_rules! f32_bin {
        ($f:expr) => {{
            let b = pop(s)?.as_f64() as f32;
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32($f(a, b)));
        }};
    }
    macro_rules! f_cmp {
        ($f:expr) => {{
            let b = pop(s)?.as_f64();
            let a = pop(s)?.as_f64();
            s.push(Val::I32(if $f(a, b) { 1 } else { 0 }));
        }};
    }

    let div0 = || "wasm: 0 으로 나눔".to_string();
    match op {
        0x45 => {
            let a = pop(s)?.as_i32();
            s.push(Val::I32(if a == 0 { 1 } else { 0 }));
        }
        0x46 => i32_cmp!(|a: i32, b: i32| a == b),
        0x47 => i32_cmp!(|a: i32, b: i32| a != b),
        0x48 => i32_cmp!(|a: i32, b: i32| a < b),
        0x49 => i32_cmp!(|a: i32, b: i32| (a as u32) < (b as u32)),
        0x4A => i32_cmp!(|a: i32, b: i32| a > b),
        0x4B => i32_cmp!(|a: i32, b: i32| (a as u32) > (b as u32)),
        0x4C => i32_cmp!(|a: i32, b: i32| a <= b),
        0x4D => i32_cmp!(|a: i32, b: i32| (a as u32) <= (b as u32)),
        0x4E => i32_cmp!(|a: i32, b: i32| a >= b),
        0x4F => i32_cmp!(|a: i32, b: i32| (a as u32) >= (b as u32)),
        0x50 => {
            let a = pop(s)?.as_i64();
            s.push(Val::I32(if a == 0 { 1 } else { 0 }));
        }
        0x51 => i64_cmp!(|a: i64, b: i64| a == b),
        0x52 => i64_cmp!(|a: i64, b: i64| a != b),
        0x53 => i64_cmp!(|a: i64, b: i64| a < b),
        0x54 => i64_cmp!(|a: i64, b: i64| (a as u64) < (b as u64)),
        0x55 => i64_cmp!(|a: i64, b: i64| a > b),
        0x56 => i64_cmp!(|a: i64, b: i64| (a as u64) > (b as u64)),
        0x57 => i64_cmp!(|a: i64, b: i64| a <= b),
        0x58 => i64_cmp!(|a: i64, b: i64| (a as u64) <= (b as u64)),
        0x59 => i64_cmp!(|a: i64, b: i64| a >= b),
        0x5A => i64_cmp!(|a: i64, b: i64| (a as u64) >= (b as u64)),
        0x5B..=0x60 => {
            // f32 비교
            let b = pop(s)?.as_f64() as f32;
            let a = pop(s)?.as_f64() as f32;
            let r = match op {
                0x5B => a == b,
                0x5C => a != b,
                0x5D => a < b,
                0x5E => a > b,
                0x5F => a <= b,
                _ => a >= b,
            };
            s.push(Val::I32(if r { 1 } else { 0 }));
        }
        0x61 => f_cmp!(|a: f64, b: f64| a == b),
        0x62 => f_cmp!(|a: f64, b: f64| a != b),
        0x63 => f_cmp!(|a: f64, b: f64| a < b),
        0x64 => f_cmp!(|a: f64, b: f64| a > b),
        0x65 => f_cmp!(|a: f64, b: f64| a <= b),
        0x66 => f_cmp!(|a: f64, b: f64| a >= b),
        0x67 => {
            let a = pop(s)?.as_i32();
            s.push(Val::I32(a.leading_zeros() as i32));
        }
        0x68 => {
            let a = pop(s)?.as_i32();
            s.push(Val::I32(a.trailing_zeros() as i32));
        }
        0x69 => {
            let a = pop(s)?.as_i32();
            s.push(Val::I32(a.count_ones() as i32));
        }
        0x6A => i32_bin!(|a: i32, b: i32| Ok(a.wrapping_add(b))),
        0x6B => i32_bin!(|a: i32, b: i32| Ok(a.wrapping_sub(b))),
        0x6C => i32_bin!(|a: i32, b: i32| Ok(a.wrapping_mul(b))),
        0x6D => i32_bin!(|a: i32, b: i32| if b == 0 {
            Err(div0())
        } else {
            Ok(a.wrapping_div(b))
        }),
        0x6E => i32_bin!(|a: i32, b: i32| if b == 0 {
            Err(div0())
        } else {
            Ok(((a as u32) / (b as u32)) as i32)
        }),
        0x6F => i32_bin!(|a: i32, b: i32| if b == 0 {
            Err(div0())
        } else {
            Ok(a.wrapping_rem(b))
        }),
        0x70 => i32_bin!(|a: i32, b: i32| if b == 0 {
            Err(div0())
        } else {
            Ok(((a as u32) % (b as u32)) as i32)
        }),
        0x71 => i32_bin!(|a: i32, b: i32| Ok(a & b)),
        0x72 => i32_bin!(|a: i32, b: i32| Ok(a | b)),
        0x73 => i32_bin!(|a: i32, b: i32| Ok(a ^ b)),
        0x74 => i32_bin!(|a: i32, b: i32| Ok(a.wrapping_shl(b as u32 & 31))),
        0x75 => i32_bin!(|a: i32, b: i32| Ok(a.wrapping_shr(b as u32 & 31))),
        0x76 => i32_bin!(|a: i32, b: i32| Ok(((a as u32) >> (b as u32 & 31)) as i32)),
        0x77 => i32_bin!(|a: i32, b: i32| Ok(a.rotate_left(b as u32 & 31))),
        0x78 => i32_bin!(|a: i32, b: i32| Ok(a.rotate_right(b as u32 & 31))),
        0x79 => {
            let a = pop(s)?.as_i64();
            s.push(Val::I64(a.leading_zeros() as i64));
        }
        0x7A => {
            let a = pop(s)?.as_i64();
            s.push(Val::I64(a.trailing_zeros() as i64));
        }
        0x7B => {
            let a = pop(s)?.as_i64();
            s.push(Val::I64(a.count_ones() as i64));
        }
        0x7C => i64_bin!(|a: i64, b: i64| Ok(a.wrapping_add(b))),
        0x7D => i64_bin!(|a: i64, b: i64| Ok(a.wrapping_sub(b))),
        0x7E => i64_bin!(|a: i64, b: i64| Ok(a.wrapping_mul(b))),
        0x7F => i64_bin!(|a: i64, b: i64| if b == 0 {
            Err(div0())
        } else {
            Ok(a.wrapping_div(b))
        }),
        0x80 => i64_bin!(|a: i64, b: i64| if b == 0 {
            Err(div0())
        } else {
            Ok(((a as u64) / (b as u64)) as i64)
        }),
        0x81 => i64_bin!(|a: i64, b: i64| if b == 0 {
            Err(div0())
        } else {
            Ok(a.wrapping_rem(b))
        }),
        0x82 => i64_bin!(|a: i64, b: i64| if b == 0 {
            Err(div0())
        } else {
            Ok(((a as u64) % (b as u64)) as i64)
        }),
        0x83 => i64_bin!(|a: i64, b: i64| Ok(a & b)),
        0x84 => i64_bin!(|a: i64, b: i64| Ok(a | b)),
        0x85 => i64_bin!(|a: i64, b: i64| Ok(a ^ b)),
        0x86 => i64_bin!(|a: i64, b: i64| Ok(a.wrapping_shl(b as u32 & 63))),
        0x87 => i64_bin!(|a: i64, b: i64| Ok(a.wrapping_shr(b as u32 & 63))),
        0x88 => i64_bin!(|a: i64, b: i64| Ok(((a as u64) >> (b as u32 & 63)) as i64)),
        0x89 => i64_bin!(|a: i64, b: i64| Ok(a.rotate_left(b as u32 & 63))),
        0x8A => i64_bin!(|a: i64, b: i64| Ok(a.rotate_right(b as u32 & 63))),
        // f32 단항
        0x8B => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32(a.abs()));
        }
        0x8C => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32(-a));
        }
        0x8D => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32(a.ceil()));
        }
        0x8E => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32(a.floor()));
        }
        0x8F => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32(a.trunc()));
        }
        0x90 => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32(round_even_f32(a)));
        }
        0x91 => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F32(a.sqrt()));
        }
        0x92 => f32_bin!(|a: f32, b: f32| a + b),
        0x93 => f32_bin!(|a: f32, b: f32| a - b),
        0x94 => f32_bin!(|a: f32, b: f32| a * b),
        0x95 => f32_bin!(|a: f32, b: f32| a / b),
        0x96 => f32_bin!(|a: f32, b: f32| if a < b { a } else { b }),
        0x97 => f32_bin!(|a: f32, b: f32| if a > b { a } else { b }),
        0x98 => f32_bin!(|a: f32, b: f32| a.copysign(b)),
        // f64 단항
        0x99 => {
            let a = pop(s)?.as_f64();
            s.push(Val::F64(a.abs()));
        }
        0x9A => {
            let a = pop(s)?.as_f64();
            s.push(Val::F64(-a));
        }
        0x9B => {
            let a = pop(s)?.as_f64();
            s.push(Val::F64(a.ceil()));
        }
        0x9C => {
            let a = pop(s)?.as_f64();
            s.push(Val::F64(a.floor()));
        }
        0x9D => {
            let a = pop(s)?.as_f64();
            s.push(Val::F64(a.trunc()));
        }
        0x9E => {
            let a = pop(s)?.as_f64();
            s.push(Val::F64(round_even_f64(a)));
        }
        0x9F => {
            let a = pop(s)?.as_f64();
            s.push(Val::F64(a.sqrt()));
        }
        0xA0 => f64_bin!(|a: f64, b: f64| a + b),
        0xA1 => f64_bin!(|a: f64, b: f64| a - b),
        0xA2 => f64_bin!(|a: f64, b: f64| a * b),
        0xA3 => f64_bin!(|a: f64, b: f64| a / b),
        0xA4 => f64_bin!(|a: f64, b: f64| if a < b { a } else { b }),
        0xA5 => f64_bin!(|a: f64, b: f64| if a > b { a } else { b }),
        0xA6 => f64_bin!(|a: f64, b: f64| a.copysign(b)),
        // 변환
        0xA7 => {
            let a = pop(s)?.as_i64();
            s.push(Val::I32(a as i32));
        }
        0xA8 | 0xAA => {
            let a = pop(s)?.as_f64() as f32;
            if !a.is_finite() {
                return Err("wasm: 정수 변환 불가 (NaN/Inf)".to_string());
            }
            s.push(Val::I32(a as i32));
        }
        0xA9 | 0xAB => {
            let a = pop(s)?.as_f64() as f32;
            if !a.is_finite() {
                return Err("wasm: 정수 변환 불가 (NaN/Inf)".to_string());
            }
            s.push(Val::I32(a as u32 as i32));
        }
        0xAC => {
            let a = pop(s)?.as_i32();
            s.push(Val::I64(a as i64));
        }
        0xAD => {
            let a = pop(s)?.as_i32();
            s.push(Val::I64(a as u32 as i64));
        }
        0xAE..=0xB1 => {
            let a = pop(s)?.as_f64();
            if !a.is_finite() {
                return Err("wasm: 정수 변환 불가 (NaN/Inf)".to_string());
            }
            s.push(Val::I64(if op == 0xAF || op == 0xB1 {
                a as u64 as i64
            } else {
                a as i64
            }));
        }
        0xB2 => {
            let a = pop(s)?.as_i32();
            s.push(Val::F32(a as f32));
        }
        0xB3 => {
            let a = pop(s)?.as_i32();
            s.push(Val::F32(a as u32 as f32));
        }
        0xB4 => {
            let a = pop(s)?.as_i64();
            s.push(Val::F32(a as f32));
        }
        0xB5 => {
            let a = pop(s)?.as_i64();
            s.push(Val::F32(a as u64 as f32));
        }
        0xB6 => {
            let a = pop(s)?.as_f64();
            s.push(Val::F32(a as f32));
        }
        0xB7 => {
            let a = pop(s)?.as_i32();
            s.push(Val::F64(a as f64));
        }
        0xB8 => {
            let a = pop(s)?.as_i32();
            s.push(Val::F64(a as u32 as f64));
        }
        0xB9 => {
            let a = pop(s)?.as_i64();
            s.push(Val::F64(a as f64));
        }
        0xBA => {
            let a = pop(s)?.as_i64();
            s.push(Val::F64(a as u64 as f64));
        }
        0xBB => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::F64(a as f64));
        }
        0xBC => {
            let a = pop(s)?.as_f64() as f32;
            s.push(Val::I32(a.to_bits() as i32));
        }
        0xBD => {
            let a = pop(s)?.as_f64();
            s.push(Val::I64(a.to_bits() as i64));
        }
        0xBE => {
            let a = pop(s)?.as_i32();
            s.push(Val::F32(f32::from_bits(a as u32)));
        }
        0xBF => {
            let a = pop(s)?.as_i64();
            s.push(Val::F64(f64::from_bits(a as u64)));
        }
        // 부호 확장 (sign_extend 제안)
        0xC0 => {
            let a = pop(s)?.as_i32();
            s.push(Val::I32(a as i8 as i32));
        }
        0xC1 => {
            let a = pop(s)?.as_i32();
            s.push(Val::I32(a as i16 as i32));
        }
        0xC2 => {
            let a = pop(s)?.as_i64();
            s.push(Val::I64(a as i8 as i64));
        }
        0xC3 => {
            let a = pop(s)?.as_i64();
            s.push(Val::I64(a as i16 as i64));
        }
        0xC4 => {
            let a = pop(s)?.as_i64();
            s.push(Val::I64(a as i32 as i64));
        }
        other => return Err(format!("wasm: 모르는 수치 명령 0x{:02x}", other)),
    }
    Ok(())
}

// trunc_sat: NaN → 0, 범위 밖 → 포화 (트랩하지 않는다)
fn num_fc(sub: u32, s: &mut Vec<Val>) -> Result<(), String> {
    let a = pop(s)?.as_f64();
    let v = match sub {
        0 | 2 => {
            let x = if a.is_nan() { 0.0 } else { a };
            Val::I32(x.clamp(i32::MIN as f64, i32::MAX as f64) as i32)
        }
        1 | 3 => {
            let x = if a.is_nan() { 0.0 } else { a };
            Val::I32(x.clamp(0.0, u32::MAX as f64) as u32 as i32)
        }
        4 | 6 => {
            let x = if a.is_nan() { 0.0 } else { a };
            Val::I64(x.clamp(i64::MIN as f64, i64::MAX as f64) as i64)
        }
        _ => {
            let x = if a.is_nan() { 0.0 } else { a };
            Val::I64(x.clamp(0.0, u64::MAX as f64) as u64 as i64)
        }
    };
    s.push(v);
    Ok(())
}

// 짝수로 반올림 (nearest, ties-to-even) — 표준이 요구하는 규칙이다.
fn round_even_f32(a: f32) -> f32 {
    let r = a.round();
    if (a - a.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        r - a.signum()
    } else {
        r
    }
}

fn round_even_f64(a: f64) -> f64 {
    let r = a.round();
    if (a - a.trunc()).abs() == 0.5 && r % 2.0 != 0.0 {
        r - a.signum()
    } else {
        r
    }
}

// 상수식 평가 (전역/데이터/요소 오프셋)
pub fn eval_const(code: &[Instr], globals: &[Val]) -> Result<Val, String> {
    let mut st: Vec<Val> = Vec::new();
    for i in code {
        match i {
            Instr::I32Const(v) => st.push(Val::I32(*v)),
            Instr::I64Const(v) => st.push(Val::I64(*v)),
            Instr::F32Const(v) => st.push(Val::F32(*v)),
            Instr::F64Const(v) => st.push(Val::F64(*v)),
            Instr::GlobalGet(g) => {
                st.push(*globals.get(*g as usize).ok_or("wasm: 상수식의 전역 없음")?)
            }
            Instr::RefFunc(f) => st.push(Val::I32(*f as i32)),
            Instr::RefNull => st.push(Val::I32(-1)),
            Instr::Num(op) => num_op(*op, &mut st)?,
            _ => return Err("wasm: 상수식이 아니다".to_string()),
        }
    }
    st.pop().ok_or_else(|| "wasm: 빈 상수식".to_string())
}


#[cfg(test)]
mod tests {
    use super::*;

    struct NoHost;
    impl Host for NoHost {
        fn call_import(&mut self, _i: usize, _a: &[Val]) -> Result<Vec<Val>, String> {
            Err("임포트 없음".to_string())
        }
    }

    // 바이너리를 기계적으로 만든다 — 섹션 길이를 손으로 세면 반드시 틀린다.
    fn leb(mut n: u32) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                return out;
            }
            out.push(b | 0x80);
        }
    }
    fn sec(id: u8, body: Vec<u8>) -> Vec<u8> {
        let mut out = vec![id];
        out.extend(leb(body.len() as u32));
        out.extend(body);
        out
    }
    // 벡터: [개수][항목…]
    fn vecs(items: Vec<Vec<u8>>) -> Vec<u8> {
        let mut out = leb(items.len() as u32);
        for i in items {
            out.extend(i);
        }
        out
    }
    fn name(s: &str) -> Vec<u8> {
        let mut out = leb(s.len() as u32);
        out.extend_from_slice(s.as_bytes());
        out
    }
    // 함수 본문: [크기][로컬 벡터][코드]
    fn body(locals: Vec<(u32, u8)>, code: Vec<u8>) -> Vec<u8> {
        let mut b = vecs(locals
            .into_iter()
            .map(|(n, t)| {
                let mut v = leb(n);
                v.push(t);
                v
            })
            .collect());
        b.extend(code);
        b.push(0x0b); // end
        let mut out = leb(b.len() as u32);
        out.extend(b);
        out
    }
    fn ftype(params: &[u8], results: &[u8]) -> Vec<u8> {
        let mut v = vec![0x60];
        v.extend(leb(params.len() as u32));
        v.extend_from_slice(params);
        v.extend(leb(results.len() as u32));
        v.extend_from_slice(results);
        v
    }
    fn export(n: &str, kind: u8, idx: u32) -> Vec<u8> {
        let mut v = name(n);
        v.push(kind);
        v.extend(leb(idx));
        v
    }
    fn module(sections: Vec<Vec<u8>>) -> Vec<u8> {
        let mut m = Vec::new();
        m.extend_from_slice(b"\0asm");
        m.extend_from_slice(&1u32.to_le_bytes());
        for s in sections {
            m.extend(s);
        }
        m
    }

    const I32: u8 = 0x7f;

    fn inst_of(m: Module) -> Instance {
        instantiate(Rc::new(m), vec![], None, &mut NoHost).expect("인스턴스화")
    }

    #[test]
    fn parses_and_runs_add() {
        let m = module(vec![
            sec(1, vecs(vec![ftype(&[I32, I32], &[I32])])),
            sec(3, vecs(vec![leb(0)])),
            sec(7, vecs(vec![export("add", 0x00, 0)])),
            sec(
                10,
                vecs(vec![body(vec![], vec![0x20, 0x00, 0x20, 0x01, 0x6a])]),
            ),
        ]);
        let parsed = parse(&m).expect("파싱");
        assert_eq!(parsed.types.len(), 1);
        assert_eq!(parsed.exports.len(), 1);
        let inst = inst_of(parsed);
        let r = inst
            .call(&mut NoHost, 0, &[Val::I32(20), Val::I32(22)])
            .expect("실행");
        assert_eq!(r, vec![Val::I32(42)]);
    }

    #[test]
    fn rejects_bad_magic() {
        assert!(parse(b"not wasm at all").is_err());
    }

    // block/loop/br_if — 0..n-1 합
    #[test]
    fn loop_and_branch_work() {
        let code = vec![
            0x02, 0x40, // block
            0x03, 0x40, // loop
            0x20, 0x01, 0x20, 0x00, 0x4e, // local.get i; local.get n; i32.ge_s
            0x0d, 0x01, // br_if 1 → 블록 밖
            0x20, 0x02, 0x20, 0x01, 0x6a, 0x21, 0x02, // acc += i
            0x20, 0x01, 0x41, 0x01, 0x6a, 0x21, 0x01, // i += 1
            0x0c, 0x00, // br 0 → loop 처음
            0x0b, // end loop
            0x0b, // end block
            0x20, 0x02, // local.get acc
        ];
        let m = module(vec![
            sec(1, vecs(vec![ftype(&[I32], &[I32])])),
            sec(3, vecs(vec![leb(0)])),
            sec(7, vecs(vec![export("sum", 0x00, 0)])),
            sec(10, vecs(vec![body(vec![(2, I32)], code)])),
        ]);
        let inst = inst_of(parse(&m).expect("파싱"));
        let r = inst.call(&mut NoHost, 0, &[Val::I32(5)]).expect("실행");
        assert_eq!(r, vec![Val::I32(10)], "0+1+2+3+4 = 10");
    }

    // 메모리 + 데이터 세그먼트: 데이터가 실제로 실리고 load 로 읽힌다.
    #[test]
    fn memory_data_segment_and_load() {
        let mut data_seg = vec![0x00]; // flags: active, 메모리 0
        data_seg.extend(vec![0x41, 0x04, 0x0b]); // offset = i32.const 4
        data_seg.extend(leb(4));
        data_seg.extend_from_slice(&[0x78, 0x56, 0x34, 0x12]);

        let m = module(vec![
            sec(1, vecs(vec![ftype(&[], &[I32])])),
            sec(3, vecs(vec![leb(0)])),
            sec(5, vecs(vec![vec![0x00, 0x01]])), // 메모리 1페이지 (최대 없음)
            sec(7, vecs(vec![export("peek", 0x00, 0)])),
            sec(
                10,
                // i32.const 4; i32.load align=2 offset=0
                vecs(vec![body(vec![], vec![0x41, 0x04, 0x28, 0x02, 0x00])]),
            ),
            sec(11, vecs(vec![data_seg])),
        ]);
        let parsed = parse(&m).expect("파싱");
        assert_eq!(parsed.mem_pages, Some(1));
        let inst = inst_of(parsed);
        assert_eq!(inst.mem_len(), PAGE, "1 페이지 = 64KB");
        let r = inst.call(&mut NoHost, 0, &[]).expect("실행");
        assert_eq!(r, vec![Val::I32(0x12345678)], "데이터가 리틀엔디언으로 실렸다");
    }

    // 메모리 쓰기 → 같은 배열(JS ArrayBuffer 의 _b)에 반영된다
    #[test]
    fn store_writes_into_shared_array() {
        let m = module(vec![
            sec(1, vecs(vec![ftype(&[I32], &[])])),
            sec(3, vecs(vec![leb(0)])),
            sec(5, vecs(vec![vec![0x00, 0x01]])),
            sec(7, vecs(vec![export("poke", 0x00, 0)])),
            sec(
                10,
                // i32.const 0; local.get 0; i32.store8
                vecs(vec![body(vec![], vec![0x41, 0x00, 0x20, 0x00, 0x3a, 0x00, 0x00])]),
            ),
        ]);
        let inst = inst_of(parse(&m).expect("파싱"));
        inst.call(&mut NoHost, 0, &[Val::I32(0xAB)]).expect("실행");
        let mem = inst.mem.as_ref().unwrap().borrow().clone();
        let first = match &mem.borrow()[0] {
            crate::js::interp::Value::Num(n) => *n,
            _ => panic!("바이트가 숫자가 아니다"),
        };
        assert_eq!(first, 0xAB as f64, "JS 가 보는 배열에 그대로 써졌다");
    }

    // 임포트 함수: wasm 이 호스트를 부르고 반환값을 쓴다
    #[test]
    fn calls_host_import() {
        struct Doubler;
        impl Host for Doubler {
            fn call_import(&mut self, i: usize, a: &[Val]) -> Result<Vec<Val>, String> {
                assert_eq!(i, 0);
                Ok(vec![Val::I32(a[0].as_i32() * 2)])
            }
        }
        let mut imp = name("e");
        imp.extend(name("d"));
        imp.push(0x00); // func
        imp.extend(leb(0)); // type 0

        let m = module(vec![
            sec(1, vecs(vec![ftype(&[I32], &[I32])])),
            sec(2, vecs(vec![imp])),
            sec(3, vecs(vec![leb(0)])),
            sec(7, vecs(vec![export("f", 0x00, 1)])),
            sec(
                10,
                // local.get 0; call 0; i32.const 1; i32.add
                vecs(vec![body(vec![], vec![0x20, 0x00, 0x10, 0x00, 0x41, 0x01, 0x6a])]),
            ),
        ]);
        let parsed = parse(&m).expect("파싱");
        assert_eq!(parsed.imported_funcs, 1);
        let inst =
            instantiate(Rc::new(parsed), vec![Extern::Func], None, &mut Doubler).expect("인스턴스화");
        let r = inst.call(&mut Doubler, 1, &[Val::I32(20)]).expect("실행");
        assert_eq!(r, vec![Val::I32(41)], "호스트가 2배 → +1");
    }

    // global.set — LLVM 이 만든 모든 모듈이 스택 포인터에 쓴다. 조용히 무시하면 다 틀린다.
    #[test]
    fn global_set_persists() {
        let mut g = vec![I32, 0x01]; // i32, mutable
        g.extend(vec![0x41, 0x07, 0x0b]); // = 7
        let m = module(vec![
            sec(1, vecs(vec![ftype(&[], &[I32])])),
            sec(3, vecs(vec![leb(0)])),
            sec(6, vecs(vec![g])),
            sec(7, vecs(vec![export("g", 0x00, 0)])),
            sec(
                10,
                // i32.const 99; global.set 0; global.get 0
                vecs(vec![body(vec![], vec![0x41, 0xe3, 0x00, 0x24, 0x00, 0x23, 0x00])]),
            ),
        ]);
        let inst = inst_of(parse(&m).expect("파싱"));
        let r = inst.call(&mut NoHost, 0, &[]).expect("실행");
        assert_eq!(r, vec![Val::I32(99)], "global.set 이 실제로 반영된다");
        assert_eq!(inst.globals.borrow()[0], Val::I32(99), "호출 뒤에도 남는다");
    }

    // call_indirect + 요소 세그먼트 (가상 함수 호출 — 모든 C++/Rust 모듈이 쓴다)
    #[test]
    fn call_indirect_through_table() {
        let mut elem = vec![0x00]; // active, 테이블 0
        elem.extend(vec![0x41, 0x00, 0x0b]); // offset 0
        elem.extend(vecs(vec![leb(1), leb(0)])); // [func 1, func 0]

        let m = module(vec![
            sec(1, vecs(vec![ftype(&[I32], &[I32])])),
            sec(3, vecs(vec![leb(0), leb(0), leb(0)])),
            sec(4, vecs(vec![vec![0x70, 0x00, 0x02]])), // funcref 테이블, 최소 2
            sec(7, vecs(vec![export("pick", 0x00, 2)])),
            sec(
                10,
                vecs(vec![
                    // func 0: x → x + 10
                    body(vec![], vec![0x20, 0x00, 0x41, 0x0a, 0x6a]),
                    // func 1: x → x * 3
                    body(vec![], vec![0x20, 0x00, 0x41, 0x03, 0x6c]),
                    // func 2: pick(i) → table[i](7)
                    body(vec![], vec![0x41, 0x07, 0x20, 0x00, 0x11, 0x00, 0x00]),
                ]),
            ),
            sec(9, vecs(vec![elem])),
        ]);
        let inst = inst_of(parse(&m).expect("파싱"));
        let a = inst.call(&mut NoHost, 2, &[Val::I32(0)]).expect("실행");
        let b = inst.call(&mut NoHost, 2, &[Val::I32(1)]).expect("실행");
        assert_eq!(a, vec![Val::I32(21)], "table[0] = func1 → 7*3");
        assert_eq!(b, vec![Val::I32(17)], "table[1] = func0 → 7+10");
    }

    // 무한 루프는 한도에서 멈춘다 (사이트 스크립트가 기계를 잡아먹으면 안 된다)
    #[test]
    fn infinite_loop_is_bounded() {
        let m = module(vec![
            sec(1, vecs(vec![ftype(&[], &[])])),
            sec(3, vecs(vec![leb(0)])),
            sec(7, vecs(vec![export("spin", 0x00, 0)])),
            // loop; br 0; end
            sec(10, vecs(vec![body(vec![], vec![0x03, 0x40, 0x0c, 0x00, 0x0b])])),
        ]);
        let inst = inst_of(parse(&m).expect("파싱"));
        let e = inst.call(&mut NoHost, 0, &[]).unwrap_err();
        assert!(e.contains("한도"), "무한 루프가 한도에서 끊긴다: {}", e);
    }
}
