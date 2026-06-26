use std::collections::HashMap;

#[derive(Debug)]
#[allow(dead_code)]
pub enum FontError {
    TooShort,
    MissingTable(&'static str),
    UnsupportedCmap,
}

fn be_u16(d: &[u8], off: usize) -> u16 {
    ((d[off] as u16) << 8) | d[off + 1] as u16
}
fn be_i16(d: &[u8], off: usize) -> i16 {
    be_u16(d, off) as i16
}
fn be_u32(d: &[u8], off: usize) -> u32 {
    ((be_u16(d, off) as u32) << 16) | be_u16(d, off + 2) as u32
}

#[derive(Clone, Copy, Debug)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    pub on_curve: bool,
}

#[derive(Clone, Debug)]
pub struct Contour {
    pub points: Vec<Point>,
}

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Glyph {
    pub contours: Vec<Contour>,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

struct Cmap4 {
    seg_count: usize,
    end_code: Vec<u16>,
    start_code: Vec<u16>,
    id_delta: Vec<i16>,
    id_range_offset: Vec<u16>,
    id_range_offset_pos: usize,
}

pub struct Font {
    data: Vec<u8>,
    tables: HashMap<[u8; 4], (usize, usize)>,
    units_per_em: u16,
    index_to_loc_format: i16,
    #[allow(dead_code)]
    num_glyphs: u16,
    ascent: i16,
    descent: i16,
    line_gap: i16,
    num_h_metrics: u16,
    cmap: Cmap4,
}

impl Font {
    pub fn from_bytes(data: Vec<u8>) -> Result<Font, FontError> {
        if data.len() < 12 {
            return Err(FontError::TooShort);
        }
        let num_tables = be_u16(&data, 4) as usize;
        let mut tables = HashMap::new();
        for i in 0..num_tables {
            let rec = 12 + i * 16;
            if rec + 16 > data.len() {
                return Err(FontError::TooShort);
            }
            let mut tag = [0u8; 4];
            tag.copy_from_slice(&data[rec..rec + 4]);
            let off = be_u32(&data, rec + 8) as usize;
            let len = be_u32(&data, rec + 12) as usize;
            tables.insert(tag, (off, len));
        }

        let head = tables.get(b"head").ok_or(FontError::MissingTable("head"))?.0;
        let units_per_em = be_u16(&data, head + 18);
        let index_to_loc_format = be_i16(&data, head + 50);

        let maxp = tables.get(b"maxp").ok_or(FontError::MissingTable("maxp"))?.0;
        let num_glyphs = be_u16(&data, maxp + 4);

        let hhea = tables.get(b"hhea").ok_or(FontError::MissingTable("hhea"))?.0;
        let ascent = be_i16(&data, hhea + 4);
        let descent = be_i16(&data, hhea + 6);
        let line_gap = be_i16(&data, hhea + 8);
        let num_h_metrics = be_u16(&data, hhea + 34);

        tables.get(b"hmtx").ok_or(FontError::MissingTable("hmtx"))?;
        tables.get(b"loca").ok_or(FontError::MissingTable("loca"))?;
        tables.get(b"glyf").ok_or(FontError::MissingTable("glyf"))?;

        let cmap = parse_cmap(&data, &tables)?;

        Ok(Font {
            data,
            tables,
            units_per_em,
            index_to_loc_format,
            num_glyphs,
            ascent,
            descent,
            line_gap,
            num_h_metrics,
            cmap,
        })
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }
    pub fn ascent(&self) -> i16 {
        self.ascent
    }
    pub fn descent(&self) -> i16 {
        self.descent
    }
    pub fn line_gap(&self) -> i16 {
        self.line_gap
    }
    pub fn glyph_index(&self, c: char) -> u16 {
        let cp = c as u32;
        if cp > 0xFFFF {
            return 0;
        }
        let c = cp as u16;
        let cm = &self.cmap;
        for i in 0..cm.seg_count {
            if c <= cm.end_code[i] && c >= cm.start_code[i] {
                if cm.id_range_offset[i] == 0 {
                    return (c as i32 + cm.id_delta[i] as i32) as u16;
                }
                let addr = cm.id_range_offset_pos
                    + 2 * i
                    + cm.id_range_offset[i] as usize
                    + 2 * (c - cm.start_code[i]) as usize;
                let g = be_u16(&self.data, addr);
                if g == 0 {
                    return 0;
                }
                return (g as i32 + cm.id_delta[i] as i32) as u16;
            }
        }
        0
    }

    pub fn advance_width(&self, glyph_id: u16) -> u16 {
        let (hmtx, _) = self.tables[b"hmtx"];
        let n = self.num_h_metrics as usize;
        let i = (glyph_id as usize).min(n.saturating_sub(1));
        be_u16(&self.data, hmtx + i * 4)
    }

    fn glyph_range(&self, gid: u16) -> (usize, usize) {
        let (loca, _) = self.tables[b"loca"];
        if self.index_to_loc_format == 0 {
            let a = be_u16(&self.data, loca + gid as usize * 2) as usize * 2;
            let b = be_u16(&self.data, loca + (gid as usize + 1) * 2) as usize * 2;
            (a, b)
        } else {
            let a = be_u32(&self.data, loca + gid as usize * 4) as usize;
            let b = be_u32(&self.data, loca + (gid as usize + 1) * 4) as usize;
            (a, b)
        }
    }

    pub fn outline(&self, glyph_id: u16) -> Glyph {
        let (glyf, _) = self.tables[b"glyf"];
        let (start, end) = self.glyph_range(glyph_id);
        if end <= start {
            return Glyph { contours: vec![], x_min: 0, y_min: 0, x_max: 0, y_max: 0 };
        }
        let g = glyf + start;
        let num_contours = be_i16(&self.data, g);
        let x_min = be_i16(&self.data, g + 2);
        let y_min = be_i16(&self.data, g + 4);
        let x_max = be_i16(&self.data, g + 6);
        let y_max = be_i16(&self.data, g + 8);
        if num_contours <= 0 {
            // composite (<0) is out of scope; 0 contours = empty
            return Glyph { contours: vec![], x_min, y_min, x_max, y_max };
        }
        let num_contours = num_contours as usize;
        let mut p = g + 10;
        let mut end_pts = Vec::with_capacity(num_contours);
        for _ in 0..num_contours {
            end_pts.push(be_u16(&self.data, p));
            p += 2;
        }
        let num_points = *end_pts.last().unwrap() as usize + 1;
        let instr_len = be_u16(&self.data, p) as usize;
        p += 2 + instr_len;

        // flags (with repeat)
        let mut flags = Vec::with_capacity(num_points);
        while flags.len() < num_points {
            let f = self.data[p];
            p += 1;
            flags.push(f);
            if f & 0x08 != 0 {
                let repeat = self.data[p];
                p += 1;
                for _ in 0..repeat {
                    if flags.len() < num_points {
                        flags.push(f);
                    }
                }
            }
        }

        // x coords
        let mut xs = Vec::with_capacity(num_points);
        let mut x: i32 = 0;
        for &f in &flags {
            if f & 0x02 != 0 {
                let dx = self.data[p] as i32;
                p += 1;
                x += if f & 0x10 != 0 { dx } else { -dx };
            } else if f & 0x10 == 0 {
                x += be_i16(&self.data, p) as i32;
                p += 2;
            }
            xs.push(x);
        }
        // y coords
        let mut ys = Vec::with_capacity(num_points);
        let mut y: i32 = 0;
        for &f in &flags {
            if f & 0x04 != 0 {
                let dy = self.data[p] as i32;
                p += 1;
                y += if f & 0x20 != 0 { dy } else { -dy };
            } else if f & 0x20 == 0 {
                y += be_i16(&self.data, p) as i32;
                p += 2;
            }
            ys.push(y);
        }

        // split into contours
        let mut contours = Vec::with_capacity(num_contours);
        let mut s = 0usize;
        for &e in &end_pts {
            let e = e as usize;
            let mut points = Vec::with_capacity(e - s + 1);
            for i in s..=e {
                points.push(Point {
                    x: xs[i] as f32,
                    y: ys[i] as f32,
                    on_curve: flags[i] & 0x01 != 0,
                });
            }
            contours.push(Contour { points });
            s = e + 1;
        }
        Glyph { contours, x_min, y_min, x_max, y_max }
    }
}

pub struct FontStack {
    pub fonts: Vec<Font>,
}

impl FontStack {
    pub fn new(fonts: Vec<Font>) -> FontStack {
        FontStack { fonts }
    }
    pub fn primary(&self) -> &Font {
        &self.fonts[0]
    }
    pub fn font(&self, index: usize) -> &Font {
        &self.fonts[index]
    }
    /// 글자를 가진 첫 폰트의 (인덱스, 글리프 id). 없으면 (0, 0)=.notdef.
    pub fn glyph_for(&self, c: char) -> (usize, u16) {
        for (i, f) in self.fonts.iter().enumerate() {
            let g = f.glyph_index(c);
            if g != 0 {
                return (i, g);
            }
        }
        (0, 0)
    }
}

fn parse_cmap(data: &[u8], tables: &HashMap<[u8; 4], (usize, usize)>) -> Result<Cmap4, FontError> {
    let cmap_off = tables.get(b"cmap").ok_or(FontError::MissingTable("cmap"))?.0;
    let num_sub = be_u16(data, cmap_off + 2) as usize;
    let mut best: Option<(i32, usize)> = None;
    for i in 0..num_sub {
        let rec = cmap_off + 4 + i * 8;
        let plat = be_u16(data, rec);
        let enc = be_u16(data, rec + 2);
        let sub_off = be_u32(data, rec + 4) as usize;
        let sub = cmap_off + sub_off;
        let format = be_u16(data, sub);
        if format == 4 {
            let score = match (plat, enc) {
                (3, 1) => 3,
                (0, _) => 2,
                (3, 0) => 1,
                _ => 0,
            };
            if best.map_or(true, |(s, _)| score > s) {
                best = Some((score, sub));
            }
        }
    }
    let sub = best.ok_or(FontError::UnsupportedCmap)?.1;
    let seg_x2 = be_u16(data, sub + 6) as usize;
    let seg_count = seg_x2 / 2;
    let end_base = sub + 14;
    let start_base = end_base + seg_x2 + 2;
    let delta_base = start_base + seg_x2;
    let range_base = delta_base + seg_x2;
    let end_code = (0..seg_count).map(|i| be_u16(data, end_base + i * 2)).collect();
    let start_code = (0..seg_count).map(|i| be_u16(data, start_base + i * 2)).collect();
    let id_delta = (0..seg_count).map(|i| be_i16(data, delta_base + i * 2)).collect();
    let id_range_offset = (0..seg_count).map(|i| be_u16(data, range_base + i * 2)).collect();
    Ok(Cmap4 {
        seg_count,
        end_code,
        start_code,
        id_delta,
        id_range_offset,
        id_range_offset_pos: range_base,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load() -> Font {
        let bytes = std::fs::read("assets/fonts/Kestrel.ttf").expect("read font");
        Font::from_bytes(bytes).expect("parse font")
    }

    #[test]
    fn parses_global_metrics() {
        let f = load();
        assert!((16..=16384).contains(&f.units_per_em()), "upm={}", f.units_per_em());
        assert!(f.ascent() > 0);
        assert!(f.descent() < 0);
    }

    #[test]
    fn maps_ascii_to_glyphs() {
        let f = load();
        let a = f.glyph_index('A');
        let b = f.glyph_index('B');
        assert_ne!(a, 0, "'A' should map to a real glyph");
        assert_ne!(b, 0);
        assert_ne!(a, b, "'A' and 'B' should differ");
        assert_eq!(f.glyph_index('\u{1F600}'), 0, "non-BMP -> .notdef");
    }

    #[test]
    fn advance_widths_are_positive() {
        let f = load();
        let a = f.advance_width(f.glyph_index('A'));
        let space = f.advance_width(f.glyph_index(' '));
        assert!(a > 0);
        assert!(space > 0, "space should still advance the pen");
    }

    #[test]
    fn fontstack_falls_back_for_korean() {
        let latin = Font::from_bytes(std::fs::read("assets/fonts/Latin.ttf").unwrap()).unwrap();
        let noto = Font::from_bytes(std::fs::read("assets/fonts/Kestrel.ttf").unwrap()).unwrap();
        let stack = FontStack::new(vec![latin, noto]);
        let (ai, ag) = stack.glyph_for('A');
        assert_eq!(ai, 0, "Latin 'A' from primary");
        assert_ne!(ag, 0);
        let (ki, kg) = stack.glyph_for('한');
        assert_eq!(ki, 1, "Korean should fall back to Noto");
        assert_ne!(kg, 0);
    }

    #[test]
    fn outlines_have_contours() {
        let f = load();
        let o = f.outline(f.glyph_index('o'));
        assert!(o.contours.len() >= 1, "'o' should have at least one contour");
        let total_pts: usize = o.contours.iter().map(|c| c.points.len()).sum();
        assert!(total_pts > 0);
        // space is an empty glyph (advance only, no contours)
        let sp = f.outline(f.glyph_index(' '));
        assert_eq!(sp.contours.len(), 0);
    }
}
