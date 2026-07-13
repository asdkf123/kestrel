// brotli 압축 해제 (RFC 7932).
//
// woff2 웹폰트가 brotli 로 압축돼 있다. 이게 없으면 모던 사이트의 웹폰트를 하나도
// 못 읽는다 — 서버는 브라우저 UA 에게 woff2 만 내려주기 때문에, 우리가 브라우저라고
// 말하는 순간 폰트가 전멸한다(실제로 그렇게 됐다). 그래서 표준을 구현한다.
//
// 정적 테이블(사전/변환/문맥 조회표)은 손으로 옮기지 않고 google/brotli 원전에서
// 기계적으로 추출했다 (brotli_tables.rs). 하나만 어긋나도 출력이 통째로 쓰레기가 된다.

use crate::brotli_tables as t;

const NUM_LITERAL_SYMBOLS: usize = 256;
const NUM_COMMAND_SYMBOLS: usize = 704;
const NUM_BLOCK_LENGTH_SYMBOLS: usize = 26;
const CODE_LENGTH_CODES: usize = 18;
const NUM_DISTANCE_SHORT_CODES: u32 = 16;
const MAX_HUFFMAN_BITS: usize = 15;

// 코드 길이 부호의 읽는 순서 (RFC 7932 §3.5)
const CODE_LENGTH_ORDER: [usize; CODE_LENGTH_CODES] =
    [1, 2, 3, 4, 0, 5, 17, 6, 16, 7, 8, 9, 10, 11, 12, 13, 14, 15];

// 코드 길이 부호 자체의 고정 프리픽스 코드 (4비트 엿보기 → (길이, 값))
const CL_PREFIX_LEN: [u8; 16] = [2, 2, 2, 3, 2, 2, 2, 4, 2, 2, 2, 3, 2, 2, 2, 4];
const CL_PREFIX_VAL: [u8; 16] = [0, 4, 3, 2, 0, 4, 3, 1, 0, 4, 3, 2, 0, 4, 3, 5];

const INSERT_OFFSET: [u32; 24] = [
    0, 1, 2, 3, 4, 5, 6, 8, 10, 14, 18, 26, 34, 50, 66, 98, 130, 194, 322, 578, 1090, 2114, 6210,
    22594,
];
const INSERT_EXTRA: [u32; 24] = [
    0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 12, 14, 24,
];
const COPY_OFFSET: [u32; 24] = [
    2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 14, 18, 22, 30, 38, 54, 70, 102, 134, 198, 326, 582, 1094, 2118,
];
const COPY_EXTRA: [u32; 24] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 24,
];
const BLOCK_LENGTH_OFFSET: [u32; NUM_BLOCK_LENGTH_SYMBOLS] = [
    1, 5, 9, 13, 17, 25, 33, 41, 49, 65, 81, 97, 113, 145, 177, 209, 241, 305, 369, 497, 753, 1265,
    2289, 4337, 8433, 16625,
];
const BLOCK_LENGTH_EXTRA: [u32; NUM_BLOCK_LENGTH_SYMBOLS] = [
    2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 6, 6, 7, 8, 9, 10, 11, 12, 13, 24,
];
// 명령 코드 → (insert 범위, copy 범위) 시작값 (RFC 7932 §5)
const INSERT_RANGE_LUT: [u32; 9] = [0, 0, 8, 8, 0, 16, 8, 16, 16];
const COPY_RANGE_LUT: [u32; 9] = [0, 8, 0, 8, 16, 0, 16, 8, 16];

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize, // 비트 위치
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    // LSB 우선으로 n 비트 (n <= 24)
    fn bits(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for i in 0..n {
            let byte = *self.data.get(self.pos >> 3)?;
            let bit = (byte >> (self.pos & 7)) & 1;
            v |= (bit as u32) << i;
            self.pos += 1;
        }
        Some(v)
    }

    fn bit(&mut self) -> Option<u32> {
        self.bits(1)
    }

    // 소비하지 않고 엿보기 (부족하면 0 으로 채움 — 끝단에서만 발생)
    fn peek(&self, n: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..n {
            let p = self.pos + i as usize;
            let byte = self.data.get(p >> 3).copied().unwrap_or(0);
            v |= (((byte >> (p & 7)) & 1) as u32) << i;
        }
        v
    }

    fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    fn align_byte(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
}

// 정준 프리픽스 코드. 스트림에서 읽은 첫 비트가 코드의 최상위 비트다(DEFLATE 와 동일).
struct Huffman {
    counts: [u16; MAX_HUFFMAN_BITS + 1],
    symbols: Vec<u16>,
}

impl Huffman {
    fn from_lengths(lengths: &[u8]) -> Option<Huffman> {
        let mut counts = [0u16; MAX_HUFFMAN_BITS + 1];
        for &l in lengths {
            if l as usize > MAX_HUFFMAN_BITS {
                return None;
            }
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        // 오프셋 계산 후 심볼을 길이순 → 심볼값순으로 배치
        let mut offs = [0u16; MAX_HUFFMAN_BITS + 2];
        for i in 1..=MAX_HUFFMAN_BITS {
            offs[i + 1] = offs[i] + counts[i];
        }
        let mut symbols = vec![0u16; lengths.iter().filter(|&&l| l > 0).count()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l > 0 {
                symbols[offs[l as usize] as usize] = sym as u16;
                offs[l as usize] += 1;
            }
        }
        Some(Huffman { counts, symbols })
    }

    // 단일 심볼 코드(길이 0짜리 하나) — 비트를 읽지 않는다
    fn single(sym: u16) -> Huffman {
        Huffman { counts: [0; MAX_HUFFMAN_BITS + 1], symbols: vec![sym] }
    }

    fn decode(&self, br: &mut BitReader) -> Option<u16> {
        if self.symbols.len() == 1 && self.counts.iter().all(|&c| c == 0) {
            return Some(self.symbols[0]); // 심볼 하나뿐 → 비트 소비 없음
        }
        let mut code = 0i32;
        let mut first = 0i32;
        let mut index = 0i32;
        for len in 1..=MAX_HUFFMAN_BITS {
            code |= br.bit()? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self.symbols.get((index + (code - first)) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}

// 0..255 를 1~11비트로 (RFC 7932 §9.2 의 가변길이 코드).
// 값 = 0 | 1 | (1<<n) + extra  — "+1" 을 넣으면 2,3,4 가 표현 불가능해지고
// 블록 타입 수가 어긋나 알파벳 크기가 틀어진다(그러면 그 뒤 비트가 전부 밀린다).
fn read_varlen_u8(br: &mut BitReader) -> Option<u32> {
    if br.bit()? == 0 {
        return Some(0);
    }
    let n = br.bits(3)?;
    if n == 0 {
        return Some(1);
    }
    let extra = br.bits(n)?;
    Some((1 << n) + extra)
}

// 프리픽스 코드 하나 읽기 (단순/복합)
fn read_huffman(br: &mut BitReader, alphabet_size: usize) -> Option<Huffman> {
    let alpha_bits = {
        let mut b = 0;
        while (1usize << b) < alphabet_size {
            b += 1;
        }
        b.max(1) as u32
    };
    let hskip = br.bits(2)?;
    if hskip == 1 {
        // 단순 코드: 심볼 1~4개
        let nsym = br.bits(2)? as usize + 1;
        let mut syms = Vec::with_capacity(nsym);
        for _ in 0..nsym {
            let s = br.bits(alpha_bits)? as usize;
            if s >= alphabet_size {
                return None;
            }
            syms.push(s as u16);
        }
        let mut lengths = vec![0u8; alphabet_size];
        match nsym {
            1 => return Some(Huffman::single(syms[0])),
            2 => {
                lengths[syms[0] as usize] = 1;
                lengths[syms[1] as usize] = 1;
            }
            3 => {
                lengths[syms[0] as usize] = 1;
                lengths[syms[1] as usize] = 2;
                lengths[syms[2] as usize] = 2;
            }
            _ => {
                let tree_select = br.bit()?;
                if tree_select == 1 {
                    lengths[syms[0] as usize] = 1;
                    lengths[syms[1] as usize] = 2;
                    lengths[syms[2] as usize] = 3;
                    lengths[syms[3] as usize] = 3;
                } else {
                    for s in &syms {
                        lengths[*s as usize] = 2;
                    }
                }
            }
        }
        return Huffman::from_lengths(&lengths);
    }

    // 복합 코드: 먼저 코드-길이 부호를 읽는다
    let mut cl_lengths = [0u8; CODE_LENGTH_CODES];
    let mut space = 32i32;
    let mut num_codes = 0;
    for i in (hskip as usize)..CODE_LENGTH_CODES {
        if space <= 0 {
            break;
        }
        let p = br.peek(4) as usize;
        br.skip(CL_PREFIX_LEN[p] as u32);
        let v = CL_PREFIX_VAL[p];
        cl_lengths[CODE_LENGTH_ORDER[i]] = v;
        if v != 0 {
            space -= 32 >> v;
            num_codes += 1;
        }
    }
    if num_codes != 1 && space > 0 {
        return None;
    }
    let cl_huff = Huffman::from_lengths(&cl_lengths)?;

    // 심볼 길이를 RLE 로 읽는다
    let mut lengths = vec![0u8; alphabet_size];
    let mut i = 0usize;
    let mut prev_nonzero = 8u8; // 반복(16)의 기준값 기본 8
    let mut repeat = 0u32;
    let mut repeat_len = 0u8;
    let mut space = 32768i32;
    while i < alphabet_size && space > 0 {
        let sym = cl_huff.decode(br)? as u8;
        match sym {
            0..=15 => {
                lengths[i] = sym;
                i += 1;
                repeat = 0;
                if sym != 0 {
                    prev_nonzero = sym;
                    space -= 32768 >> sym;
                }
            }
            16 => {
                // 직전 0 아닌 길이를 3..6회 반복 (누적 규칙)
                let extra = br.bits(2)?;
                let new_len = prev_nonzero;
                if repeat_len != new_len {
                    repeat = 0;
                    repeat_len = new_len;
                }
                let old = repeat;
                if repeat > 0 {
                    repeat -= 2;
                    repeat <<= 2;
                }
                repeat += extra + 3;
                let n = repeat - old;
                for _ in 0..n {
                    if i >= alphabet_size {
                        return None;
                    }
                    lengths[i] = new_len;
                    i += 1;
                    space -= 32768 >> new_len;
                }
            }
            17 => {
                // 0 을 3..10회 반복
                let extra = br.bits(3)?;
                if repeat_len != 0 {
                    repeat = 0;
                    repeat_len = 0;
                }
                let old = repeat;
                if repeat > 0 {
                    repeat -= 2;
                    repeat <<= 3;
                }
                repeat += extra + 3;
                let n = repeat - old;
                for _ in 0..n {
                    if i >= alphabet_size {
                        return None;
                    }
                    lengths[i] = 0;
                    i += 1;
                }
            }
            _ => return None,
        }
    }
    Huffman::from_lengths(&lengths)
}

// 문맥 맵 (RLE + inverse MTF)
fn read_context_map(br: &mut BitReader, size: usize) -> Option<(Vec<u8>, u32)> {
    let num_htrees = read_varlen_u8(br)? + 1;
    if num_htrees <= 1 {
        return Some((vec![0u8; size], 1));
    }
    let use_rle = br.bit()?;
    let max_run = if use_rle == 1 { br.bits(4)? + 1 } else { 0 };
    let huff = read_huffman(br, (num_htrees + max_run) as usize)?;
    let mut map = Vec::with_capacity(size);
    while map.len() < size {
        let sym = huff.decode(br)? as u32;
        if sym == 0 {
            map.push(0);
        } else if sym <= max_run {
            // 0 을 (1<<sym) + extra 번
            let extra = br.bits(sym)?;
            let n = (1u32 << sym) + extra;
            for _ in 0..n {
                if map.len() >= size {
                    return None;
                }
                map.push(0);
            }
        } else {
            map.push((sym - max_run) as u8);
        }
    }
    if br.bit()? == 1 {
        inverse_move_to_front(&mut map);
    }
    Some((map, num_htrees))
}

fn inverse_move_to_front(v: &mut [u8]) {
    let mut mtf: Vec<u8> = (0..=255u8).collect();
    for x in v.iter_mut() {
        let idx = *x as usize;
        let val = mtf[idx];
        mtf.copy_within(0..idx, 1);
        mtf[0] = val;
        *x = val;
    }
}

fn read_block_length(br: &mut BitReader, huff: &Huffman) -> Option<u32> {
    let sym = huff.decode(br)? as usize;
    if sym >= NUM_BLOCK_LENGTH_SYMBOLS {
        return None;
    }
    let extra = br.bits(BLOCK_LENGTH_EXTRA[sym])?;
    Some(BLOCK_LENGTH_OFFSET[sym] + extra)
}

// 블록 전환 상태 (리터럴/명령/거리 각각)
struct BlockSwitch {
    ntypes: u32,
    ty: u32,
    prev_ty: u32,
    len: u32,
    ty_huff: Option<Huffman>,
    len_huff: Option<Huffman>,
}

impl BlockSwitch {
    fn read(br: &mut BitReader) -> Option<BlockSwitch> {
        let ntypes = read_varlen_u8(br)? + 1;
        if ntypes < 2 {
            return Some(BlockSwitch {
                ntypes,
                ty: 0,
                prev_ty: 1,
                len: u32::MAX,
                ty_huff: None,
                len_huff: None,
            });
        }
        let ty_huff = read_huffman(br, (ntypes + 2) as usize)?;
        let len_huff = read_huffman(br, NUM_BLOCK_LENGTH_SYMBOLS)?;
        let len = read_block_length(br, &len_huff)?;
        Some(BlockSwitch {
            ntypes,
            ty: 0,
            prev_ty: 1,
            len,
            ty_huff: Some(ty_huff),
            len_huff: Some(len_huff),
        })
    }

    fn next(&mut self, br: &mut BitReader) -> Option<()> {
        let (Some(th), Some(lh)) = (&self.ty_huff, &self.len_huff) else {
            self.len = u32::MAX;
            return Some(());
        };
        let sym = th.decode(br)? as u32;
        let new_ty = match sym {
            0 => self.prev_ty,
            1 => (self.ty + 1) % self.ntypes,
            _ => sym - 2,
        };
        self.prev_ty = self.ty;
        self.ty = new_ty;
        self.len = read_block_length(br, lh)?;
        Some(())
    }
}

/// brotli 스트림 압축 해제. 실패하면 None (형식 오류).
pub fn decompress(data: &[u8]) -> Option<Vec<u8>> {
    let mut br = BitReader::new(data);
    // 윈도우 크기
    let wbits = if br.bit()? == 0 {
        16
    } else {
        let n = br.bits(3)?;
        if n != 0 {
            17 + n
        } else {
            let n = br.bits(3)?;
            match n {
                0 => 17,
                1 => return None, // large window 미지원
                _ => 8 + n,
            }
        }
    };
    let max_backward = (1u32 << wbits) - 16;

    let mut out: Vec<u8> = Vec::new();
    // 마지막 4개 거리. last[0] 이 가장 최근.
    // RFC 는 초기값을 "16, 15, 11, 4" 로 적는데 이건 오래된 것부터의 나열이다 —
    // 가장 최근이 4다. 순서를 뒤집어 넣으면 첫 명령부터 거리 16 이 나와 사전 참조로
    // 오인되고(윈도우보다 크므로) 압축 해제가 통째로 실패한다.
    let mut last: [u32; 4] = [4, 11, 15, 16];

    loop {
        // ── 메타블록 헤더 ──
        let is_last = br.bit()? == 1;
        if is_last && br.bit()? == 1 {
            break; // 마지막이며 빈 블록
        }
        let nibbles_code = br.bits(2)?;
        if nibbles_code == 3 {
            // 메타데이터 블록 — 출력 없음
            if br.bit()? != 0 {
                return None; // 예약 비트
            }
            let skip_bytes = br.bits(2)?;
            let skip_len = if skip_bytes == 0 {
                0
            } else {
                let v = br.bits(skip_bytes * 8)?;
                v + 1
            };
            br.align_byte();
            br.skip(skip_len * 8);
            if is_last {
                break;
            }
            continue;
        }
        let nibbles = 4 + nibbles_code;
        let mlen = br.bits(nibbles * 4)? + 1;
        if !is_last {
            if br.bit()? == 1 {
                // 비압축 블록
                br.align_byte();
                for _ in 0..mlen {
                    out.push(br.bits(8)? as u8);
                }
                continue;
            }
        }

        // ── 블록 전환 / 문맥 ──
        let mut bl_l = BlockSwitch::read(&mut br)?;
        let mut bl_i = BlockSwitch::read(&mut br)?;
        let mut bl_d = BlockSwitch::read(&mut br)?;

        let npostfix = br.bits(2)?;
        let ndirect = br.bits(4)? << npostfix;
        let postfix_mask = (1u32 << npostfix) - 1;

        // 리터럴 블록 타입별 문맥 모드
        let mut context_modes = Vec::with_capacity(bl_l.ntypes as usize);
        for _ in 0..bl_l.ntypes {
            context_modes.push(br.bits(2)? as usize);
        }

        let (ctx_map_l, num_l_trees) = read_context_map(&mut br, (bl_l.ntypes * 64) as usize)?;
        let (ctx_map_d, num_d_trees) = read_context_map(&mut br, (bl_d.ntypes * 4) as usize)?;

        let mut lit_huffs = Vec::with_capacity(num_l_trees as usize);
        for _ in 0..num_l_trees {
            lit_huffs.push(read_huffman(&mut br, NUM_LITERAL_SYMBOLS)?);
        }
        let mut cmd_huffs = Vec::with_capacity(bl_i.ntypes as usize);
        for _ in 0..bl_i.ntypes {
            cmd_huffs.push(read_huffman(&mut br, NUM_COMMAND_SYMBOLS)?);
        }
        let dist_alphabet = (NUM_DISTANCE_SHORT_CODES + ndirect + (48 << npostfix)) as usize;
        let mut dist_huffs = Vec::with_capacity(num_d_trees as usize);
        for _ in 0..num_d_trees {
            dist_huffs.push(read_huffman(&mut br, dist_alphabet)?);
        }

        // ── 명령 루프 ──
        let block_start = out.len();
        while (out.len() - block_start) < mlen as usize {
            if bl_i.len == 0 {
                bl_i.next(&mut br)?;
            }
            bl_i.len = bl_i.len.saturating_sub(1);

            let cmd = cmd_huffs.get(bl_i.ty as usize)?.decode(&mut br)? as u32;
            let mut range_idx = cmd >> 6;
            let dist_code_zero = range_idx < 2;
            if !dist_code_zero {
                range_idx -= 2;
            }
            let insert_code = INSERT_RANGE_LUT[range_idx as usize] + ((cmd >> 3) & 7);
            let copy_code = COPY_RANGE_LUT[range_idx as usize] + (cmd & 7);
            let insert_len = INSERT_OFFSET[insert_code as usize]
                + br.bits(INSERT_EXTRA[insert_code as usize])?;
            let copy_len =
                COPY_OFFSET[copy_code as usize] + br.bits(COPY_EXTRA[copy_code as usize])?;

            // 리터럴 삽입
            for _ in 0..insert_len {
                if bl_l.len == 0 {
                    bl_l.next(&mut br)?;
                }
                bl_l.len = bl_l.len.saturating_sub(1);
                let p1 = out.last().copied().unwrap_or(0) as usize;
                let p2 = if out.len() >= 2 { out[out.len() - 2] as usize } else { 0 };
                let mode = context_modes[bl_l.ty as usize];
                let lut = mode << 9;
                let ctx = (t::CONTEXT_LOOKUP[lut + p1] | t::CONTEXT_LOOKUP[lut + 256 + p2]) as usize;
                let tree = ctx_map_l[(bl_l.ty as usize) * 64 + ctx] as usize;
                let b = lit_huffs.get(tree)?.decode(&mut br)? as u8;
                out.push(b);
            }
            if (out.len() - block_start) >= mlen as usize {
                break;
            }

            // 거리
            let distance;
            let mut push_rb = false;
            if dist_code_zero {
                distance = last[0]; // 코드 0 = 직전 거리 (링버퍼 갱신 없음)
            } else {
                if bl_d.len == 0 {
                    bl_d.next(&mut br)?;
                }
                bl_d.len = bl_d.len.saturating_sub(1);
                let dctx = if copy_len > 4 { 3usize } else { (copy_len - 2) as usize };
                let tree = ctx_map_d[(bl_d.ty as usize) * 4 + dctx] as usize;
                let dcode = dist_huffs.get(tree)?.decode(&mut br)? as u32;
                if dcode == 0 {
                    distance = last[0];
                } else if dcode < NUM_DISTANCE_SHORT_CODES {
                    // 1..15: 직전/두번째 거리에 ±1..3 (RFC 7932 §4 표)
                    let base = match dcode {
                        1 => last[1],
                        2 => last[2],
                        3 => last[3],
                        4..=9 => last[0],
                        _ => last[1],
                    };
                    let delta: i32 = match dcode {
                        1 | 2 | 3 => 0,
                        4 | 10 => -1,
                        5 | 11 => 1,
                        6 | 12 => -2,
                        7 | 13 => 2,
                        8 | 14 => -3,
                        _ => 3,
                    };
                    let d = base as i32 + delta;
                    if d <= 0 {
                        return None;
                    }
                    distance = d as u32;
                    push_rb = true;
                } else if dcode < NUM_DISTANCE_SHORT_CODES + ndirect {
                    distance = dcode - NUM_DISTANCE_SHORT_CODES + 1;
                    push_rb = true;
                } else {
                    let v = dcode - NUM_DISTANCE_SHORT_CODES - ndirect;
                    let nbits = 1 + (v >> (npostfix + 1));
                    let hcode = v >> npostfix;
                    let lcode = v & postfix_mask;
                    let offset = ((2 + (hcode & 1)) << nbits) as i64 - 4;
                    let extra = br.bits(nbits)? as i64;
                    distance = (((offset + extra) as u32) << npostfix) + lcode + ndirect + 1;
                    push_rb = true;
                }
            }

            let max_dist = (out.len() as u32).min(max_backward);
            // 사전 참조(윈도우 밖)는 링버퍼에 넣지 않는다 — 윈도우 안의 거리가 아니다.
            if push_rb && distance <= max_dist {
                last = [distance, last[0], last[1], last[2]];
            }
            if distance <= max_dist {
                // 출력 버퍼에서 복사 (겹침 허용 — 바이트 단위)
                let start = out.len() - distance as usize;
                for k in 0..copy_len as usize {
                    let b = out[start + k];
                    out.push(b);
                }
            } else {
                // 정적 사전 참조
                let word_id = distance - max_dist - 1;
                let len = copy_len as usize;
                if !(4..=24).contains(&len) {
                    return None;
                }
                let bits = t::DICT_SIZE_BITS[len] as u32;
                let index = word_id & ((1u32 << bits) - 1);
                let transform_id = word_id >> bits;
                if transform_id >= 121 {
                    return None;
                }
                let off = t::DICT_OFFSETS[len] as usize + index as usize * len;
                let word = t::DICTIONARY.get(off..off + len)?;
                let transformed = apply_transform(word, transform_id as usize)?;
                out.extend_from_slice(&transformed);
            }
        }

        if is_last {
            break;
        }
    }
    Some(out)
}

// 사전 단어 변환 (RFC 7932 §8). (prefix, type, suffix) × 121
fn apply_transform(word: &[u8], id: usize) -> Option<Vec<u8>> {
    let prefix_id = t::TRANSFORMS[id * 3] as usize;
    let ty = t::TRANSFORMS[id * 3 + 1] as usize;
    let suffix_id = t::TRANSFORMS[id * 3 + 2] as usize;

    let pick = |i: usize| -> &[u8] {
        let start = t::PREFIX_SUFFIX_MAP[i] as usize;
        let len = t::PREFIX_SUFFIX[start] as usize;
        &t::PREFIX_SUFFIX[start + 1..start + 1 + len]
    };
    let prefix = pick(prefix_id);
    let suffix = pick(suffix_id);

    // 본체: omit-first/omit-last 적용
    let mut body: Vec<u8> = word.to_vec();
    match ty {
        1..=9 => {
            // OMIT_LAST_n
            let n = ty;
            if body.len() >= n {
                body.truncate(body.len() - n);
            } else {
                body.clear();
            }
        }
        12..=20 => {
            // OMIT_FIRST_n (12 → 1)
            let n = ty - 11;
            if body.len() >= n {
                body.drain(0..n);
            } else {
                body.clear();
            }
        }
        _ => {}
    }
    match ty {
        10 => uppercase(&mut body, false), // UPPERCASE_FIRST
        11 => uppercase(&mut body, true),  // UPPERCASE_ALL
        // SHIFT_FIRST/SHIFT_ALL(21/22)은 brotli 의 확장 사전용이라 RFC 7932 의 121개
        // 변환에는 나오지 않는다(테이블에서 확인: 쓰이는 타입은 0~20 뿐).
        // 혹시 나오면 조용히 틀린 바이트를 내놓는 대신 실패로 알린다.
        21 | 22 => return None,
        _ => {}
    }

    let mut out = Vec::with_capacity(prefix.len() + body.len() + suffix.len());
    out.extend_from_slice(prefix);
    out.extend_from_slice(&body);
    out.extend_from_slice(suffix);
    Some(out)
}

// UTF-8 인지 대문자화 (RFC 7932 §8 의 정의 그대로)
fn uppercase(s: &mut [u8], all: bool) {
    let mut i = 0usize;
    while i < s.len() {
        if s[i] < 0x80 {
            s[i] = s[i].to_ascii_uppercase();
            i += 1;
        } else if s[i] < 0xC0 {
            i += 1;
        } else if s[i] < 0xE0 {
            if i + 1 < s.len() {
                s[i + 1] ^= 32;
            }
            i += 2;
        } else {
            if i + 2 < s.len() {
                s[i + 2] ^= 5;
            }
            i += 3;
        }
        if !all {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_the_right_size() {
        assert_eq!(t::DICTIONARY.len(), 122_784, "brotli 정적 사전");
        assert_eq!(t::TRANSFORMS.len(), 121 * 3, "변환 121개");
        assert_eq!(t::PREFIX_SUFFIX.len(), 217);
        assert_eq!(t::CONTEXT_LOOKUP.len(), 2048);
        // 사전 첫 단어들 (스펙의 그 문자열)
        assert_eq!(&t::DICTIONARY[0..4], b"time");
    }

    #[test]
    fn empty_input_is_none_not_panic() {
        // 잘린 입력에 패닉하지 않고 None 을 돌려준다
        assert!(decompress(&[]).is_none());
        assert!(decompress(&[0x00]).is_none());
    }

}
