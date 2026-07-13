// GIF 디코더 (GIF89a). 첫 프레임만 그린다 — 정적 렌더라 애니메이션은 첫 프레임이 정답이다.
//
// 아직도 흔하다: HN 의 s.gif(스페이서), 옛 사이트의 아이콘·배너. 못 읽으면 그 자리가 빈다.
//
// 구성: 헤더 → 논리 화면 기술자 → 전역 색표 → (확장 블록)* → 이미지 기술자 →
// 지역 색표 → LZW 압축 데이터. 투명색은 그래픽 제어 확장에서 온다.

struct Reader<'a> {
    d: &'a [u8],
    i: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.d.get(self.i)?;
        self.i += 1;
        Some(v)
    }
    fn u16(&mut self) -> Option<u16> {
        let a = self.u8()? as u16;
        let b = self.u8()? as u16;
        Some(a | (b << 8))
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.d.get(self.i..self.i + n)?;
        self.i += n;
        Some(s)
    }
    // 서브블록 열: [길이][데이터…] 반복, 길이 0 이면 끝
    fn sub_blocks(&mut self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let n = self.u8()? as usize;
            if n == 0 {
                return Some(out);
            }
            out.extend_from_slice(self.take(n)?);
        }
    }
}

pub fn decode(data: &[u8]) -> Option<crate::png::Image> {
    if data.len() < 13 || (&data[0..6] != b"GIF87a" && &data[0..6] != b"GIF89a") {
        return None;
    }
    let mut r = Reader { d: data, i: 6 };
    let screen_w = r.u16()? as usize;
    let screen_h = r.u16()? as usize;
    let flags = r.u8()?;
    let _bg = r.u8()?;
    let _aspect = r.u8()?;
    let has_gct = flags & 0x80 != 0;
    let gct_size = 2usize << (flags & 7);
    let gct = if has_gct { r.take(gct_size * 3)?.to_vec() } else { Vec::new() };

    let mut transparent: Option<u8> = None;

    loop {
        match r.u8()? {
            // 확장 블록
            0x21 => {
                let label = r.u8()?;
                if label == 0xF9 {
                    // 그래픽 제어 확장: 투명색 인덱스
                    let n = r.u8()? as usize;
                    let blk = r.take(n)?;
                    if n >= 4 && blk[0] & 1 != 0 {
                        transparent = Some(blk[3]);
                    }
                    let _ = r.u8()?; // 종료 0
                } else {
                    r.sub_blocks()?; // 주석/응용 확장 등은 건너뛴다
                }
            }
            // 이미지 기술자
            0x2C => {
                let left = r.u16()? as usize;
                let top = r.u16()? as usize;
                let w = r.u16()? as usize;
                let h = r.u16()? as usize;
                let f = r.u8()?;
                let has_lct = f & 0x80 != 0;
                let interlaced = f & 0x40 != 0;
                let lct_size = 2usize << (f & 7);
                let lct = if has_lct { r.take(lct_size * 3)?.to_vec() } else { Vec::new() };
                let palette = if has_lct { &lct } else { &gct };
                if palette.is_empty() || w == 0 || h == 0 {
                    return None;
                }
                let min_code = r.u8()? as u32;
                let compressed = r.sub_blocks()?;
                let indices = lzw_decode(&compressed, min_code, w * h)?;

                // 논리 화면 크기로 캔버스를 만들고(투명), 프레임을 그 위치에 올린다.
                let (cw, ch) = (screen_w.max(left + w), screen_h.max(top + h));
                let mut rgba = vec![0u8; cw * ch * 4];
                for y in 0..h {
                    for x in 0..w {
                        // 인터레이스: 4패스로 행 순서가 섞여 있다
                        let src_row = if interlaced { interlace_row(y, h) } else { y };
                        let idx = indices.get(src_row * w + x).copied().unwrap_or(0) as usize;
                        if Some(idx as u8) == transparent {
                            continue; // 투명 → 알파 0 유지
                        }
                        let p = idx * 3;
                        if p + 2 >= palette.len() {
                            continue;
                        }
                        let o = ((top + y) * cw + left + x) * 4;
                        if o + 3 < rgba.len() {
                            rgba[o] = palette[p];
                            rgba[o + 1] = palette[p + 1];
                            rgba[o + 2] = palette[p + 2];
                            rgba[o + 3] = 255;
                        }
                    }
                }
                return Some(crate::png::Image { width: cw, height: ch, rgba });
            }
            0x3B => return None, // 트레일러 — 이미지가 없었다
            _ => return None,
        }
    }
}

// 인터레이스 행 순서: 1) 0,8,16… 2) 4,12,… 3) 2,6,10… 4) 1,3,5…
fn interlace_row(y: usize, h: usize) -> usize {
    let rows1: Vec<usize> = (0..h).step_by(8).collect();
    let rows2: Vec<usize> = (4..h).step_by(8).collect();
    let rows3: Vec<usize> = (2..h).step_by(4).collect();
    let rows4: Vec<usize> = (1..h).step_by(2).collect();
    let mut all = rows1;
    all.extend(rows2);
    all.extend(rows3);
    all.extend(rows4);
    all.get(y).copied().unwrap_or(y)
}

// GIF 의 가변 폭 LZW (코드 폭이 사전 크기에 따라 커진다, 리틀엔디언 비트 순서)
fn lzw_decode(data: &[u8], min_code_size: u32, expect: usize) -> Option<Vec<u8>> {
    if !(2..=11).contains(&min_code_size) {
        return None;
    }
    let clear: u16 = 1 << min_code_size;
    let end: u16 = clear + 1;
    let mut code_size = min_code_size + 1;
    // 사전: 각 항목은 (접두 코드, 마지막 바이트). 리터럴은 자기 자신.
    let mut prefix: Vec<u16> = vec![0; 4096];
    let mut suffix: Vec<u8> = vec![0; 4096];
    for i in 0..clear as usize {
        suffix[i] = i as u8;
    }
    let mut next = end + 1;
    let mut out: Vec<u8> = Vec::with_capacity(expect);
    let mut prev: Option<u16> = None;

    let mut bitpos = 0usize;
    let read_code = |bitpos: &mut usize, size: u32| -> Option<u16> {
        let mut v = 0u32;
        for i in 0..size {
            let byte = data.get(*bitpos / 8)?;
            let bit = (byte >> (*bitpos % 8)) & 1;
            v |= (bit as u32) << i;
            *bitpos += 1;
        }
        Some(v as u16)
    };

    // 코드 → 바이트열 (사전을 거슬러 올라가며)
    let expand = |code: u16, prefix: &[u16], suffix: &[u8]| -> Vec<u8> {
        let mut s = Vec::new();
        let mut c = code;
        let mut guard = 0;
        loop {
            s.push(suffix[c as usize]);
            if c < clear {
                break;
            }
            c = prefix[c as usize];
            guard += 1;
            if guard > 4096 {
                break; // 손상된 사전 방어
            }
        }
        s.reverse();
        s
    };

    loop {
        let Some(code) = read_code(&mut bitpos, code_size) else { break };
        if code == clear {
            code_size = min_code_size + 1;
            next = end + 1;
            prev = None;
            continue;
        }
        if code == end {
            break;
        }
        let seq = if code < next {
            expand(code, &prefix, &suffix)
        } else if let Some(p) = prev {
            // KwKwK 케이스: 아직 사전에 없는 코드
            let mut s = expand(p, &prefix, &suffix);
            let first = s[0];
            s.push(first);
            s
        } else {
            return None; // 첫 코드가 사전 밖 → 손상
        };
        out.extend_from_slice(&seq);
        if let Some(p) = prev {
            if next < 4096 {
                prefix[next as usize] = p;
                suffix[next as usize] = seq[0];
                next += 1;
                if next == (1 << code_size) && code_size < 12 {
                    code_size += 1;
                }
            }
        }
        prev = Some(code);
        if out.len() >= expect {
            break;
        }
    }
    out.resize(expect, 0);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // 2x2 GIF: 팔레트 [빨강, 파랑], 픽셀 [0,1,1,0]
    fn tiny_gif() -> Vec<u8> {
        let mut g = Vec::new();
        g.extend_from_slice(b"GIF89a");
        g.extend_from_slice(&[2, 0, 2, 0]); // 2x2
        g.push(0x80); // 전역 색표 있음, 크기 2
        g.push(0);
        g.push(0);
        g.extend_from_slice(&[255, 0, 0]); // 색 0 = 빨강
        g.extend_from_slice(&[0, 0, 255]); // 색 1 = 파랑
        g.push(0x2C); // 이미지 기술자
        g.extend_from_slice(&[0, 0, 0, 0, 2, 0, 2, 0, 0]);
        g.push(2); // LZW 최소 코드 크기
        // 코드: clear(4), 0, 1, 1, 0, end(5). 코드 폭은 사전이 찰 때마다 늘어난다
        // (LZW 규약) — 인코더도 디코더와 같은 규칙을 따라야 한다: [3,3,3,3,4,4] 비트.
        let codes: [(u16, u32); 6] = [(4, 3), (0, 3), (1, 3), (1, 3), (0, 4), (5, 4)];
        let mut bits: Vec<u8> = Vec::new();
        let mut acc = 0u32;
        let mut nbits = 0u32;
        for (c, width) in codes {
            acc |= (c as u32) << nbits;
            nbits += width;
            while nbits >= 8 {
                bits.push((acc & 0xff) as u8);
                acc >>= 8;
                nbits -= 8;
            }
        }
        if nbits > 0 {
            bits.push(acc as u8);
        }
        g.push(bits.len() as u8);
        g.extend_from_slice(&bits);
        g.push(0); // 서브블록 끝
        g.push(0x3B); // 트레일러
        g
    }

    #[test]
    fn decodes_palette_gif() {
        let img = decode(&tiny_gif()).expect("GIF 디코드");
        assert_eq!((img.width, img.height), (2, 2));
        let px = |x: usize, y: usize| {
            let o = (y * 2 + x) * 4;
            (img.rgba[o], img.rgba[o + 1], img.rgba[o + 2], img.rgba[o + 3])
        };
        assert_eq!(px(0, 0), (255, 0, 0, 255), "빨강");
        assert_eq!(px(1, 0), (0, 0, 255, 255), "파랑");
        assert_eq!(px(0, 1), (0, 0, 255, 255), "파랑");
        assert_eq!(px(1, 1), (255, 0, 0, 255), "빨강");
    }

    #[test]
    fn decodes_transparent_spacer() {
        // HN 의 s.gif (1x1 투명 스페이서, 43바이트). 실제 파일 그대로.
        let d: [u8; 43] = [
            0x47, 0x49, 0x46, 0x38, 0x39, 0x61, 0x01, 0x00, 0x01, 0x00, 0x80, 0xff, 0x00, 0xc0,
            0xc0, 0xc0, 0x00, 0x00, 0x00, 0x21, 0xf9, 0x04, 0x01, 0x00, 0x00, 0x00, 0x00, 0x2c,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x02, 0x02, 0x44, 0x01, 0x00,
            0x3b,
        ];
        let img = decode(&d).expect("투명 스페이서 GIF");
        assert_eq!((img.width, img.height), (1, 1));
        assert_eq!(img.rgba[3], 0, "투명색 인덱스는 알파 0");
    }

    #[test]
    fn rejects_non_gif() {
        assert!(decode(b"not a gif").is_none());
    }
}
