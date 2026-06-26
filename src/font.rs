use std::collections::HashMap;

#[derive(Debug)]
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
}
