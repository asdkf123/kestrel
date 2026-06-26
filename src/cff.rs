// CFF (Compact Font Format) — .otf 의 PostScript 아웃라인(Type 2 charstring)을 직접 파싱.
use std::collections::HashMap;

struct Index {
    offsets: Vec<usize>, // 절대 파일 오프셋, len = count+1 (없으면 빈 Vec)
}

impl Index {
    fn count(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }
    fn get<'a>(&self, data: &'a [u8], i: usize) -> &'a [u8] {
        if i + 1 < self.offsets.len() {
            let (a, b) = (self.offsets[i], self.offsets[i + 1]);
            if a <= b && b <= data.len() {
                return &data[a..b];
            }
        }
        &[]
    }
}

pub struct Cff {
    charstrings: Index,
    gsubrs: Index,
    lsubrs: Index,
}

fn be_u16(d: &[u8], o: usize) -> usize {
    ((d[o] as usize) << 8) | d[o + 1] as usize
}

fn parse_index(data: &[u8], pos: usize) -> (Index, usize) {
    if pos + 2 > data.len() {
        return (Index { offsets: vec![] }, pos);
    }
    let count = be_u16(data, pos);
    if count == 0 {
        return (Index { offsets: vec![] }, pos + 2);
    }
    let off_size = data[pos + 2] as usize;
    let off_array = pos + 3;
    let read_off = |i: usize| -> usize {
        let base = off_array + i * off_size;
        let mut v = 0usize;
        for k in 0..off_size {
            v = (v << 8) | data[base + k] as usize;
        }
        v
    };
    let data_base = off_array + (count + 1) * off_size - 1;
    let mut offsets = Vec::with_capacity(count + 1);
    for i in 0..=count {
        offsets.push(data_base + read_off(i));
    }
    let end = offsets[count];
    (Index { offsets }, end)
}

fn parse_dict(d: &[u8]) -> HashMap<u16, Vec<f64>> {
    let mut map = HashMap::new();
    let mut operands: Vec<f64> = Vec::new();
    let mut i = 0;
    while i < d.len() {
        let b0 = d[i];
        if b0 <= 21 {
            let op = if b0 == 12 {
                i += 1;
                1200 + d[i] as u16
            } else {
                b0 as u16
            };
            map.insert(op, std::mem::take(&mut operands));
            i += 1;
        } else if b0 == 28 {
            operands.push(i16::from_be_bytes([d[i + 1], d[i + 2]]) as f64);
            i += 3;
        } else if b0 == 29 {
            operands.push(i32::from_be_bytes([d[i + 1], d[i + 2], d[i + 3], d[i + 4]]) as f64);
            i += 5;
        } else if b0 == 30 {
            i += 1;
            let mut s = String::new();
            'real: loop {
                if i >= d.len() {
                    break;
                }
                let byte = d[i];
                i += 1;
                for nib in [byte >> 4, byte & 0xf] {
                    match nib {
                        0..=9 => s.push((b'0' + nib) as char),
                        0xa => s.push('.'),
                        0xb => s.push('E'),
                        0xc => s.push_str("E-"),
                        0xe => s.push('-'),
                        0xf => break 'real,
                        _ => {}
                    }
                }
            }
            operands.push(s.parse().unwrap_or(0.0));
        } else if (32..=246).contains(&b0) {
            operands.push(b0 as f64 - 139.0);
            i += 1;
        } else if (247..=250).contains(&b0) {
            operands.push((b0 as f64 - 247.0) * 256.0 + d[i + 1] as f64 + 108.0);
            i += 2;
        } else if (251..=254).contains(&b0) {
            operands.push(-(b0 as f64 - 251.0) * 256.0 - d[i + 1] as f64 - 108.0);
            i += 2;
        } else {
            i += 1;
        }
    }
    map
}

fn bias(count: usize) -> i32 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

impl Cff {
    pub fn parse(data: &[u8], off: usize) -> Option<Cff> {
        if off + 4 > data.len() {
            return None;
        }
        let hdr_size = data[off + 2] as usize;
        let mut pos = off + hdr_size;
        let (_name, p) = parse_index(data, pos);
        pos = p;
        let (top_index, p) = parse_index(data, pos);
        pos = p;
        let (_strings, p) = parse_index(data, pos);
        pos = p;
        let (gsubrs, _) = parse_index(data, pos);

        if top_index.count() == 0 {
            return None;
        }
        let top = parse_dict(top_index.get(data, 0));
        let cs_off = off + *top.get(&17)?.first()? as usize; // CharStrings
        let (charstrings, _) = parse_index(data, cs_off);

        let mut lsubrs = Index { offsets: vec![] };
        if let Some(p) = top.get(&18) {
            // Private = [size, offset]
            if p.len() >= 2 {
                let priv_size = p[0] as usize;
                let priv_off = off + p[1] as usize;
                if priv_off + priv_size <= data.len() {
                    let private = parse_dict(&data[priv_off..priv_off + priv_size]);
                    if let Some(s) = private.get(&19) {
                        // Subrs offset, relative to Private DICT start
                        let lsub_off = priv_off + s[0] as usize;
                        lsubrs = parse_index(data, lsub_off).0;
                    }
                }
            }
        }
        Some(Cff { charstrings, gsubrs, lsubrs })
    }

    pub fn outline(&self, data: &[u8], glyph_id: u16) -> Vec<Vec<(f32, f32)>> {
        let gid = glyph_id as usize;
        if gid + 1 >= self.charstrings.offsets.len() {
            return vec![];
        }
        let cs = self.charstrings.get(data, gid);
        let mut ctx = Ctx::new();
        ctx.exec(data, cs, &self.gsubrs, &self.lsubrs, 0);
        ctx.finish()
    }
}

struct Ctx {
    x: f32,
    y: f32,
    contours: Vec<Vec<(f32, f32)>>,
    current: Vec<(f32, f32)>,
    stack: Vec<f32>,
    nstems: usize,
    width_parsed: bool,
    done: bool,
}

impl Ctx {
    fn new() -> Ctx {
        Ctx {
            x: 0.0,
            y: 0.0,
            contours: vec![],
            current: vec![],
            stack: vec![],
            nstems: 0,
            width_parsed: false,
            done: false,
        }
    }

    fn finish(mut self) -> Vec<Vec<(f32, f32)>> {
        self.close_current();
        self.contours
    }

    // 현재 윤곽선을 시작점으로 닫아서 contours 로 옮긴다.
    fn close_current(&mut self) {
        if self.current.is_empty() {
            return;
        }
        let first = self.current[0];
        if self.current.last() != Some(&first) {
            self.current.push(first);
        }
        self.contours.push(std::mem::take(&mut self.current));
    }

    fn moveto(&mut self, dx: f32, dy: f32) {
        self.close_current();
        self.x += dx;
        self.y += dy;
        self.current.push((self.x, self.y));
    }
    fn lineto(&mut self, dx: f32, dy: f32) {
        self.x += dx;
        self.y += dy;
        self.current.push((self.x, self.y));
    }
    fn curveto(&mut self, dx1: f32, dy1: f32, dx2: f32, dy2: f32, dx3: f32, dy3: f32) {
        let p0 = (self.x, self.y);
        let p1 = (p0.0 + dx1, p0.1 + dy1);
        let p2 = (p1.0 + dx2, p1.1 + dy2);
        let p3 = (p2.0 + dx3, p2.1 + dy3);
        flatten_cubic(&mut self.current, p0, p1, p2, p3);
        self.x = p3.0;
        self.y = p3.1;
    }

    fn count_stems(&mut self) {
        if !self.width_parsed && self.stack.len() % 2 == 1 {
            self.stack.remove(0);
        }
        self.width_parsed = true;
        self.nstems += self.stack.len() / 2;
        self.stack.clear();
    }

    fn take_move_width(&mut self, expected: usize) {
        if !self.width_parsed && self.stack.len() > expected {
            self.stack.remove(0);
        }
        self.width_parsed = true;
    }

    fn exec(&mut self, data: &[u8], cs: &[u8], gsubrs: &Index, lsubrs: &Index, depth: usize) {
        if depth > 10 {
            return;
        }
        let mut i = 0;
        while i < cs.len() && !self.done {
            let b0 = cs[i];
            if b0 == 28 || b0 >= 32 {
                let (v, ni) = parse_num(cs, i);
                self.stack.push(v);
                i = ni;
                continue;
            }
            i += 1;
            match b0 {
                1 | 3 | 18 | 23 => self.count_stems(), // h/v stem(hm)
                19 | 20 => {
                    // hintmask / cntrmask: 남은 오퍼랜드는 암묵 vstem
                    self.count_stems();
                    i += (self.nstems + 7) / 8;
                }
                21 => {
                    self.take_move_width(2);
                    let (dx, dy) = (self.arg(0), self.arg(1));
                    self.moveto(dx, dy);
                    self.stack.clear();
                }
                22 => {
                    self.take_move_width(1);
                    let dx = self.arg(0);
                    self.moveto(dx, 0.0);
                    self.stack.clear();
                }
                4 => {
                    self.take_move_width(1);
                    let dy = self.arg(0);
                    self.moveto(0.0, dy);
                    self.stack.clear();
                }
                5 => {
                    let n = self.stack.len();
                    let mut k = 0;
                    while k + 1 < n {
                        let (dx, dy) = (self.stack[k], self.stack[k + 1]);
                        self.lineto(dx, dy);
                        k += 2;
                    }
                    self.stack.clear();
                }
                6 => self.alt_lineto(true),
                7 => self.alt_lineto(false),
                8 => {
                    let n = self.stack.len();
                    let mut k = 0;
                    while k + 5 < n {
                        let (a, b, c, d, e, f) = (
                            self.stack[k],
                            self.stack[k + 1],
                            self.stack[k + 2],
                            self.stack[k + 3],
                            self.stack[k + 4],
                            self.stack[k + 5],
                        );
                        self.curveto(a, b, c, d, e, f);
                        k += 6;
                    }
                    self.stack.clear();
                }
                24 => {
                    // rcurveline: 곡선들 + 마지막 직선
                    let n = self.stack.len();
                    let mut k = 0;
                    while k + 6 <= n.saturating_sub(2) {
                        let (a, b, c, d, e, f) = (
                            self.stack[k],
                            self.stack[k + 1],
                            self.stack[k + 2],
                            self.stack[k + 3],
                            self.stack[k + 4],
                            self.stack[k + 5],
                        );
                        self.curveto(a, b, c, d, e, f);
                        k += 6;
                    }
                    if k + 1 < n {
                        let (dx, dy) = (self.stack[k], self.stack[k + 1]);
                        self.lineto(dx, dy);
                    }
                    self.stack.clear();
                }
                25 => {
                    // rlinecurve: 직선들 + 마지막 곡선
                    let n = self.stack.len();
                    let mut k = 0;
                    while k + 2 <= n.saturating_sub(6) {
                        let (dx, dy) = (self.stack[k], self.stack[k + 1]);
                        self.lineto(dx, dy);
                        k += 2;
                    }
                    if k + 6 <= n {
                        let (a, b, c, d, e, f) = (
                            self.stack[k],
                            self.stack[k + 1],
                            self.stack[k + 2],
                            self.stack[k + 3],
                            self.stack[k + 4],
                            self.stack[k + 5],
                        );
                        self.curveto(a, b, c, d, e, f);
                    }
                    self.stack.clear();
                }
                26 => self.vvcurveto(),
                27 => self.hhcurveto(),
                30 => self.hv_vh(false), // vhcurveto: 수직 시작
                31 => self.hv_vh(true),  // hvcurveto: 수평 시작
                10 => {
                    if let Some(idx) = self.stack.pop() {
                        let si = (idx as i32 + bias(lsubrs.count())) as usize;
                        if si < lsubrs.count() {
                            let sub = lsubrs.get(data, si).to_vec();
                            self.exec(data, &sub, gsubrs, lsubrs, depth + 1);
                        }
                    }
                }
                29 => {
                    if let Some(idx) = self.stack.pop() {
                        let si = (idx as i32 + bias(gsubrs.count())) as usize;
                        if si < gsubrs.count() {
                            let sub = gsubrs.get(data, si).to_vec();
                            self.exec(data, &sub, gsubrs, lsubrs, depth + 1);
                        }
                    }
                }
                11 => return, // return
                14 => {
                    self.done = true;
                    return;
                }
                12 => {
                    let b1 = cs[i];
                    i += 1;
                    match b1 {
                        34 => self.hflex(),
                        35 => self.flex(),
                        36 => self.hflex1(),
                        37 => self.flex1(),
                        _ => {}
                    }
                    self.stack.clear();
                }
                _ => self.stack.clear(),
            }
        }
    }

    fn arg(&self, i: usize) -> f32 {
        self.stack.get(i).copied().unwrap_or(0.0)
    }

    fn alt_lineto(&mut self, mut horiz: bool) {
        let n = self.stack.len();
        for k in 0..n {
            let v = self.stack[k];
            if horiz {
                self.lineto(v, 0.0);
            } else {
                self.lineto(0.0, v);
            }
            horiz = !horiz;
        }
        self.stack.clear();
    }

    fn vvcurveto(&mut self) {
        let n = self.stack.len();
        let mut k = 0;
        let mut dx1 = 0.0;
        if n % 4 == 1 {
            dx1 = self.stack[0];
            k = 1;
        }
        while k + 3 < n {
            let (dya, dxb, dyb, dyc) =
                (self.stack[k], self.stack[k + 1], self.stack[k + 2], self.stack[k + 3]);
            self.curveto(dx1, dya, dxb, dyb, 0.0, dyc);
            dx1 = 0.0;
            k += 4;
        }
        self.stack.clear();
    }

    fn hhcurveto(&mut self) {
        let n = self.stack.len();
        let mut k = 0;
        let mut dy1 = 0.0;
        if n % 4 == 1 {
            dy1 = self.stack[0];
            k = 1;
        }
        while k + 3 < n {
            let (dxa, dxb, dyb, dxc) =
                (self.stack[k], self.stack[k + 1], self.stack[k + 2], self.stack[k + 3]);
            self.curveto(dxa, dy1, dxb, dyb, dxc, 0.0);
            dy1 = 0.0;
            k += 4;
        }
        self.stack.clear();
    }

    fn flex(&mut self) {
        if self.stack.len() < 12 {
            return;
        }
        let v: Vec<f32> = self.stack[..12].to_vec();
        self.curveto(v[0], v[1], v[2], v[3], v[4], v[5]);
        self.curveto(v[6], v[7], v[8], v[9], v[10], v[11]);
    }
    fn hflex(&mut self) {
        if self.stack.len() < 7 {
            return;
        }
        let v: Vec<f32> = self.stack[..7].to_vec();
        self.curveto(v[0], 0.0, v[1], v[2], v[3], 0.0);
        self.curveto(v[4], 0.0, v[5], -v[2], v[6], 0.0);
    }
    fn hflex1(&mut self) {
        if self.stack.len() < 9 {
            return;
        }
        let v: Vec<f32> = self.stack[..9].to_vec();
        self.curveto(v[0], v[1], v[2], v[3], v[4], 0.0);
        self.curveto(v[5], 0.0, v[6], v[7], v[8], -(v[1] + v[3] + v[7]));
    }
    fn flex1(&mut self) {
        if self.stack.len() < 11 {
            return;
        }
        let v: Vec<f32> = self.stack[..11].to_vec();
        let dx = v[0] + v[2] + v[4] + v[6] + v[8];
        let dy = v[1] + v[3] + v[5] + v[7] + v[9];
        self.curveto(v[0], v[1], v[2], v[3], v[4], v[5]);
        if dx.abs() > dy.abs() {
            self.curveto(v[6], v[7], v[8], v[9], v[10], -dy);
        } else {
            self.curveto(v[6], v[7], v[8], v[9], -dx, v[10]);
        }
    }

    fn hv_vh(&mut self, mut horiz: bool) {
        let n = self.stack.len();
        let mut k = 0;
        while k + 4 <= n {
            let remaining = n - k;
            let df = if remaining == 5 { self.stack[k + 4] } else { 0.0 };
            let (a, b, c, d) =
                (self.stack[k], self.stack[k + 1], self.stack[k + 2], self.stack[k + 3]);
            if horiz {
                self.curveto(a, 0.0, b, c, df, d);
            } else {
                self.curveto(0.0, a, b, c, d, df);
            }
            horiz = !horiz;
            k += 4;
        }
        self.stack.clear();
    }
}

fn parse_num(cs: &[u8], i: usize) -> (f32, usize) {
    let b0 = cs[i];
    if b0 == 28 {
        (i16::from_be_bytes([cs[i + 1], cs[i + 2]]) as f32, i + 3)
    } else if (32..=246).contains(&b0) {
        (b0 as f32 - 139.0, i + 1)
    } else if (247..=250).contains(&b0) {
        ((b0 as f32 - 247.0) * 256.0 + cs[i + 1] as f32 + 108.0, i + 2)
    } else if (251..=254).contains(&b0) {
        (-(b0 as f32 - 251.0) * 256.0 - cs[i + 1] as f32 - 108.0, i + 2)
    } else if b0 == 255 {
        (
            i32::from_be_bytes([cs[i + 1], cs[i + 2], cs[i + 3], cs[i + 4]]) as f32 / 65536.0,
            i + 5,
        )
    } else {
        (0.0, i + 1)
    }
}

fn flatten_cubic(poly: &mut Vec<(f32, f32)>, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), p3: (f32, f32)) {
    const STEPS: usize = 8;
    for s in 1..=STEPS {
        let t = s as f32 / STEPS as f32;
        let mt = 1.0 - t;
        let x = mt * mt * mt * p0.0 + 3.0 * mt * mt * t * p1.0 + 3.0 * mt * t * t * p2.0 + t * t * t * p3.0;
        let y = mt * mt * mt * p0.1 + 3.0 * mt * mt * t * p1.1 + 3.0 * mt * t * t * p2.1 + t * t * t * p3.1;
        poly.push((x, y));
    }
}
