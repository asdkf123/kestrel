// PNG 디코더 — 청크 파싱 + zlib 해제 + 스캔라인 언필터 + 색상타입 → RGBA8. 직접 구현.
use crate::inflate;

#[derive(Clone, Debug)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>, // width*height*4
}

fn be_u32(d: &[u8], o: usize) -> u32 {
    ((d[o] as u32) << 24) | ((d[o + 1] as u32) << 16) | ((d[o + 2] as u32) << 8) | (d[o + 3] as u32)
}

fn paeth(a: i32, b: i32, c: i32) -> i32 {
    let p = a + b - c;
    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

pub fn decode(data: &[u8]) -> Option<Image> {
    if !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return None;
    }
    let mut pos = 8;
    let (mut width, mut height) = (0usize, 0usize);
    let (mut bit_depth, mut color_type, mut interlace) = (0u8, 0u8, 0u8);
    let mut idat: Vec<u8> = Vec::new();
    let mut palette: Vec<[u8; 4]> = Vec::new();

    while pos + 8 <= data.len() {
        let len = be_u32(data, pos) as usize;
        let ctype = &data[pos + 4..pos + 8];
        let cstart = pos + 8;
        if cstart + len > data.len() {
            break;
        }
        let cdata = &data[cstart..cstart + len];
        match ctype {
            b"IHDR" => {
                if len < 13 {
                    return None;
                }
                width = be_u32(cdata, 0) as usize;
                height = be_u32(cdata, 4) as usize;
                bit_depth = cdata[8];
                color_type = cdata[9];
                interlace = cdata[12];
            }
            b"PLTE" => {
                for i in (0..len).step_by(3) {
                    if i + 2 < len {
                        palette.push([cdata[i], cdata[i + 1], cdata[i + 2], 255]);
                    }
                }
            }
            b"tRNS" => {
                if color_type == 3 {
                    for (i, a) in cdata.iter().enumerate() {
                        if i < palette.len() {
                            palette[i][3] = *a;
                        }
                    }
                }
            }
            b"IDAT" => idat.extend_from_slice(cdata),
            b"IEND" => break,
            _ => {}
        }
        pos = cstart + len + 4; // crc 4바이트 건너뜀
    }

    if width == 0 || height == 0 || bit_depth != 8 || interlace != 0 {
        return None; // 8비트 + 비인터레이스만
    }
    let channels = match color_type {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    let bpp = channels;
    let stride = width * bpp;
    let raw = inflate::zlib_decompress(&idat)?;
    if raw.len() < (stride + 1) * height {
        return None;
    }

    // 언필터
    let mut recon = vec![0u8; stride * height];
    let mut p = 0usize;
    for y in 0..height {
        let filter = raw[p];
        p += 1;
        for i in 0..stride {
            let filt = raw[p] as i32;
            p += 1;
            let a = if i >= bpp { recon[y * stride + i - bpp] as i32 } else { 0 };
            let b = if y > 0 { recon[(y - 1) * stride + i] as i32 } else { 0 };
            let c = if i >= bpp && y > 0 { recon[(y - 1) * stride + i - bpp] as i32 } else { 0 };
            let val = match filter {
                0 => filt,
                1 => filt + a,
                2 => filt + b,
                3 => filt + (a + b) / 2,
                4 => filt + paeth(a, b, c),
                _ => return None,
            };
            recon[y * stride + i] = (val & 0xFF) as u8;
        }
    }

    // RGBA8 변환
    let mut rgba = vec![0u8; width * height * 4];
    for i in 0..width * height {
        let s = i * bpp;
        let d = i * 4;
        match color_type {
            0 => {
                let g = recon[s];
                rgba[d] = g;
                rgba[d + 1] = g;
                rgba[d + 2] = g;
                rgba[d + 3] = 255;
            }
            2 => {
                rgba[d] = recon[s];
                rgba[d + 1] = recon[s + 1];
                rgba[d + 2] = recon[s + 2];
                rgba[d + 3] = 255;
            }
            3 => {
                let c = palette.get(recon[s] as usize).copied().unwrap_or([0, 0, 0, 255]);
                rgba[d..d + 4].copy_from_slice(&c);
            }
            4 => {
                let g = recon[s];
                rgba[d] = g;
                rgba[d + 1] = g;
                rgba[d + 2] = g;
                rgba[d + 3] = recon[s + 1];
            }
            6 => {
                rgba[d..d + 4].copy_from_slice(&recon[s..s + 4]);
            }
            _ => {}
        }
    }

    Some(Image { width, height, rgba })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn decodes_2x2_rgba() {
        // 2x2 PNG (RGBA): (255,0,0,255) (0,255,0,255) / (0,0,255,255) (255,255,255,128)
        // python3 으로 생성 (PNG_HEX 참고)
        let png = hex(PNG_HEX);
        let img = decode(&png).unwrap();
        assert_eq!((img.width, img.height), (2, 2));
        assert_eq!(&img.rgba[0..4], &[255, 0, 0, 255]); // top-left red
        assert_eq!(&img.rgba[4..8], &[0, 255, 0, 255]); // top-right green
        assert_eq!(&img.rgba[8..12], &[0, 0, 255, 255]); // bottom-left blue
        assert_eq!(&img.rgba[12..16], &[255, 255, 255, 128]); // bottom-right white/half-alpha
    }

    const PNG_HEX: &str = "89504e470d0a1a0a0000000d494844520000000200000002080600000072b60d240000001349444154789c63f8cfc0f01f0c8134083400004949097828a0db770000000049454e44ae426082";
}
