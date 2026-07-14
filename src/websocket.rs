// WebSocket 클라이언트 (RFC 6455). 진짜로 연결하고, 진짜로 프레임을 주고받는다.
//
// 왜 진짜여야 하나: 스텁을 두면 "연결됐다" 는 거짓말이 된다 (onopen 을 부르고 아무것도
// 안 오거나, 반대로 onerror 를 불러 서버가 죽은 것처럼 군다). 지금은 아예 없어서
// `new WebSocket(...)` 한 줄에 스크립트가 통째로 죽는다 (fmkorea 가 그렇다).
//
// 정적 렌더러의 한계는 정직하게: 우리는 이벤트 루프를 계속 돌리지 않는다. 연결과
// 핸드셰이크는 동기로 하고, 스크립트/타이머 드레인 구간에서 도착한 프레임만 읽어
// message 이벤트로 배달한다 (poll). 그 뒤엔 페이지 스냅샷을 찍는다.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

use crate::url::Url;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
// 폴링 시 프레임을 기다리는 시간. 렌더가 끝없이 미뤄지면 안 된다.
const POLL_TIMEOUT: Duration = Duration::from_millis(50);

pub struct WebSocket {
    stream: Box<dyn Stream>,
    // 같은 소켓을 가리키는 복제 핸들. 핸드셰이크 뒤 읽기 타임아웃을 짧게 바꾸는 데 쓴다
    // (TLS 를 쓰면 스트림이 TcpStream 을 삼켜 버려 나중엔 손댈 수 없다). 살아 있어야
    // 소켓이 닫히지 않는다.
    _tcp: TcpStream,
    pub protocol: String,
    // 0=CONNECTING, 1=OPEN, 2=CLOSING, 3=CLOSED (표준의 readyState)
    pub ready_state: u16,
    buf: Vec<u8>,
    // 조각난(fragmented) 메시지 조립 중인 payload 와 그 타입
    frag: Vec<u8>,
    frag_text: bool,
    // 클라이언트→서버 마스킹 키를 만드는 카운터 (RFC 6455 §5.3: 마스킹은 필수)
    mask_seed: u32,
}

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Message(String),
    Binary(Vec<u8>),
    Close(u16, String),
}

impl WebSocket {
    // ws:// 또는 wss:// 로 연결하고 HTTP Upgrade 핸드셰이크를 마친다.
    pub fn connect(raw_url: &str) -> Result<WebSocket, String> {
        // ws → http, wss → https 로 바꿔서 파싱 (같은 권한/포트 규칙)
        let http_url = if let Some(rest) = raw_url.strip_prefix("wss://") {
            format!("https://{}", rest)
        } else if let Some(rest) = raw_url.strip_prefix("ws://") {
            format!("http://{}", rest)
        } else {
            raw_url.to_string()
        };
        let url = Url::parse(&http_url).map_err(|e| format!("WebSocket URL: {:?}", e))?;
        let secure = url.scheme == "https";

        let addr = format!("{}:{}", url.host, url.port);
        let tcp = TcpStream::connect(&addr).map_err(|e| format!("연결 실패: {}", e))?;
        tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).ok();
        tcp.set_write_timeout(Some(HANDSHAKE_TIMEOUT)).ok();
        let tcp2 = tcp.try_clone().map_err(|e| format!("소켓 복제 실패: {}", e))?;
        let mut stream: Box<dyn Stream> = if secure {
            Box::new(crate::http::tls_stream(tcp, &url.host).map_err(|e| format!("{:?}", e))?)
        } else {
            Box::new(tcp)
        };

        // Sec-WebSocket-Key: 16바이트 난수의 base64
        let key_bytes = random16();
        let key = base64_encode(&key_bytes);
        let req = format!(
            "GET {} HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Key: {}\r\nSec-WebSocket-Version: 13\r\nOrigin: {}://{}\r\n\r\n",
            if url.path.is_empty() { "/" } else { &url.path },
            url.host,
            key,
            if secure { "https" } else { "http" },
            url.host
        );
        stream.write_all(req.as_bytes()).map_err(|e| format!("전송 실패: {}", e))?;

        // 응답 헤더를 \r\n\r\n 까지 읽는다 (본문은 없다)
        let mut buf = Vec::new();
        let mut tmp = [0u8; 512];
        loop {
            let n = stream.read(&mut tmp).map_err(|e| format!("응답 없음: {}", e))?;
            if n == 0 {
                return Err("핸드셰이크 중 연결이 끊겼다".to_string());
            }
            buf.extend_from_slice(&tmp[..n]);
            if let Some(p) = find(&buf, b"\r\n\r\n") {
                // 헤더 뒤에 이미 프레임이 붙어 왔을 수 있다 — 버려선 안 된다.
                let rest = buf[p + 4..].to_vec();
                let head = String::from_utf8_lossy(&buf[..p]).to_string();
                return Self::finish(stream, tcp2, head, rest, &key_bytes);
            }
            if buf.len() > 64 * 1024 {
                return Err("핸드셰이크 응답이 너무 크다".to_string());
            }
        }
    }

    fn finish(
        stream: Box<dyn Stream>,
        tcp: TcpStream,
        head: String,
        rest: Vec<u8>,
        key: &[u8],
    ) -> Result<WebSocket, String> {
        let mut lines = head.split("\r\n");
        let status = lines.next().unwrap_or("");
        if !status.contains(" 101") {
            return Err(format!("업그레이드 거부: {}", status));
        }
        let mut accept = String::new();
        let mut protocol = String::new();
        for l in lines {
            let Some((k, v)) = l.split_once(':') else { continue };
            match k.trim().to_ascii_lowercase().as_str() {
                "sec-websocket-accept" => accept = v.trim().to_string(),
                "sec-websocket-protocol" => protocol = v.trim().to_string(),
                _ => {}
            }
        }
        // 서버가 정말 우리 키에 답했는지 검증한다 (RFC 6455 §4.1 — 건너뛰면 아무나
        // 101 만 뱉어도 WebSocket 인 척할 수 있다).
        let mut concat = base64_encode(key).into_bytes();
        concat.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        let expect = base64_encode(&sha1(&concat));
        if accept != expect {
            return Err("Sec-WebSocket-Accept 가 맞지 않는다".to_string());
        }
        // 핸드셰이크가 끝나면 읽기 타임아웃을 짧게. 안 그러면 데이터가 없을 때
        // poll() 이 5초를 통째로 블록해 렌더가 그만큼 멈춘다.
        let _ = tcp.set_read_timeout(Some(POLL_TIMEOUT));
        Ok(WebSocket {
            stream,
            _tcp: tcp,
            protocol,
            ready_state: 1, // OPEN
            buf: rest,
            frag: Vec::new(),
            frag_text: true,
            mask_seed: u32::from_le_bytes([key[0], key[1], key[2], key[3]]),
        })
    }

    // 텍스트 프레임 전송 (클라이언트는 반드시 마스킹한다 — RFC 6455 §5.3)
    pub fn send_text(&mut self, s: &str) -> Result<(), String> {
        self.send_frame(0x1, s.as_bytes())
    }

    pub fn send_binary(&mut self, b: &[u8]) -> Result<(), String> {
        self.send_frame(0x2, b)
    }

    pub fn close(&mut self) {
        if self.ready_state == 1 {
            let _ = self.send_frame(0x8, &1000u16.to_be_bytes());
            self.ready_state = 3;
        }
    }

    fn send_frame(&mut self, opcode: u8, payload: &[u8]) -> Result<(), String> {
        if self.ready_state != 1 {
            return Err("WebSocket 이 열려 있지 않다".to_string());
        }
        let mut f = vec![0x80 | opcode];
        let n = payload.len();
        if n < 126 {
            f.push(0x80 | n as u8);
        } else if n <= 0xffff {
            f.push(0x80 | 126);
            f.extend_from_slice(&(n as u16).to_be_bytes());
        } else {
            f.push(0x80 | 127);
            f.extend_from_slice(&(n as u64).to_be_bytes());
        }
        // 마스킹 키 (예측 가능하면 안 되지만, 우리는 난수원이 빈약하다 — 시드+카운터로 섞는다)
        self.mask_seed = self.mask_seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let mask = self.mask_seed.to_be_bytes();
        f.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            f.push(b ^ mask[i % 4]);
        }
        self.stream.write_all(&f).map_err(|e| format!("전송 실패: {}", e))?;
        Ok(())
    }

    // 지금 도착해 있는 프레임들을 읽어 이벤트로. 없으면 빈 벡터 (블록하지 않는다).
    pub fn poll(&mut self) -> Vec<Event> {
        let mut out = Vec::new();
        if self.ready_state == 3 {
            return out;
        }
        // 짧은 타임아웃으로 한 번 더 읽어 본다
        let mut tmp = [0u8; 4096];
        loop {
            // 이미 버퍼에 완전한 프레임이 있으면 먼저 처리
            while let Some(ev) = self.next_frame() {
                match ev {
                    Some(e) => {
                        let close = matches!(e, Event::Close(..));
                        out.push(e);
                        if close {
                            self.ready_state = 3;
                            return out;
                        }
                    }
                    None => {} // 제어 프레임(ping 등) — 이벤트 없음
                }
            }
            match self.stream.read(&mut tmp) {
                Ok(0) => {
                    self.ready_state = 3;
                    out.push(Event::Close(1006, "연결이 끊겼다".to_string()));
                    return out;
                }
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(_) => return out, // 타임아웃/데이터 없음
            }
        }
    }

    // 버퍼에서 프레임 하나를 꺼낸다. Some(None) = 처리했지만 이벤트 없음(ping/조각).
    fn next_frame(&mut self) -> Option<Option<Event>> {
        let b = &self.buf;
        if b.len() < 2 {
            return None;
        }
        let fin = b[0] & 0x80 != 0;
        let opcode = b[0] & 0x0f;
        let masked = b[1] & 0x80 != 0;
        let len7 = (b[1] & 0x7f) as usize;
        let mut i = 2;
        let len = match len7 {
            126 => {
                if b.len() < 4 {
                    return None;
                }
                i = 4;
                u16::from_be_bytes([b[2], b[3]]) as usize
            }
            127 => {
                if b.len() < 10 {
                    return None;
                }
                i = 10;
                u64::from_be_bytes([b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9]]) as usize
            }
            n => n,
        };
        let mask: [u8; 4] = if masked {
            if b.len() < i + 4 {
                return None;
            }
            let m = [b[i], b[i + 1], b[i + 2], b[i + 3]];
            i += 4;
            m
        } else {
            [0; 4]
        };
        if b.len() < i + len {
            return None; // 아직 다 안 왔다
        }
        let mut payload: Vec<u8> = b[i..i + len].to_vec();
        if masked {
            for (k, byte) in payload.iter_mut().enumerate() {
                *byte ^= mask[k % 4];
            }
        }
        self.buf.drain(..i + len);

        match opcode {
            0x0 | 0x1 | 0x2 => {
                if opcode == 0x1 {
                    self.frag_text = true;
                } else if opcode == 0x2 {
                    self.frag_text = false;
                }
                self.frag.extend_from_slice(&payload);
                if !fin {
                    return Some(None); // 조각 — 다음 프레임을 기다린다
                }
                let data = std::mem::take(&mut self.frag);
                Some(Some(if self.frag_text {
                    Event::Message(String::from_utf8_lossy(&data).into_owned())
                } else {
                    Event::Binary(data)
                }))
            }
            0x8 => {
                let code = if payload.len() >= 2 {
                    u16::from_be_bytes([payload[0], payload[1]])
                } else {
                    1005
                };
                let reason = if payload.len() > 2 {
                    String::from_utf8_lossy(&payload[2..]).into_owned()
                } else {
                    String::new()
                };
                Some(Some(Event::Close(code, reason)))
            }
            0x9 => {
                // ping → pong 으로 답한다 (안 하면 서버가 끊는다)
                let _ = self.send_frame(0xA, &payload);
                Some(None)
            }
            _ => Some(None), // pong 등
        }
    }
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

// 16바이트 난수 (핸드셰이크 키). 시스템 시각 + 주소를 섞는다 — 암호용은 아니지만
// 핸드셰이크 키는 예측 불가만 하면 된다 (RFC 6455 §4.1: nonce).
fn random16() -> [u8; 16] {
    let mut out = [0u8; 16];
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let a = &out as *const _ as u64;
    let mut s = t ^ a.rotate_left(17);
    for chunk in out.chunks_mut(8) {
        s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let bytes = s.to_le_bytes();
        chunk.copy_from_slice(&bytes[..chunk.len()]);
    }
    out
}

pub fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

// SHA-1 (RFC 3174). 핸드셰이크 검증에만 쓴다 — 서명/암호 용도가 아니다.
pub fn sha1(msg: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut data = msg.to_vec();
    let bits = (msg.len() as u64) * 8;
    data.push(0x80);
    while data.len() % 64 != 56 {
        data.push(0);
    }
    data.extend_from_slice(&bits.to_be_bytes());

    for block in data.chunks(64) {
        let mut w = [0u32; 80];
        for (i, c) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([c[0], c[1], c[2], c[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // 프레임 파싱만 시험한다 — 소켓은 쓰지 않지만 구조체가 하나 필요하다.
    fn loopback_socket() -> TcpStream {
        let l = std::net::TcpListener::bind("127.0.0.1:0").expect("바인드");
        let addr = l.local_addr().expect("주소");
        let c = TcpStream::connect(addr).expect("연결");
        drop(l);
        c
    }

    // RFC 3174 / FIPS 180-1 의 공식 테스트 벡터 — 손으로 지어낸 값이 아니다.
    #[test]
    fn sha1_matches_rfc_vectors() {
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{:02x}", b)).collect::<String>();
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            hex(sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    }

    // RFC 4648 의 base64 테스트 벡터
    #[test]
    fn base64_matches_rfc_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    // RFC 6455 §1.3 의 예시: 키 dGhlIHNhbXBsZSBub25jZQ== → accept s3pPLMBiTxaQ9kYGzzhZRbK+xOo=
    #[test]
    fn handshake_accept_matches_rfc_example() {
        let mut concat = b"dGhlIHNhbXBsZSBub25jZQ==".to_vec();
        concat.extend_from_slice(b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11");
        assert_eq!(base64_encode(&sha1(&concat)), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    // 프레임 파싱: RFC 6455 §5.7 의 예시 (마스킹된 "Hello")
    #[test]
    fn parses_masked_text_frame() {
        let mut ws = WebSocket {
            stream: Box::new(std::io::Cursor::new(Vec::new())),
            _tcp: loopback_socket(),
            protocol: String::new(),
            ready_state: 1,
            buf: vec![
                0x81, 0x85, 0x37, 0xfa, 0x21, 0x3d, 0x7f, 0x9f, 0x4d, 0x51, 0x58,
            ],
            frag: Vec::new(),
            frag_text: true,
            mask_seed: 1,
        };
        let ev = ws.next_frame().expect("프레임").expect("이벤트");
        assert_eq!(ev, Event::Message("Hello".to_string()));
    }

    // 조각난 메시지: "Hel" + "lo" (RFC 6455 §5.4)
    #[test]
    fn reassembles_fragmented_message() {
        let mut ws = WebSocket {
            stream: Box::new(std::io::Cursor::new(Vec::new())),
            _tcp: loopback_socket(),
            protocol: String::new(),
            ready_state: 1,
            // FIN=0 text "Hel", 그 다음 FIN=1 continuation "lo" (마스킹 없음 = 서버→클라)
            buf: vec![0x01, 0x03, b'H', b'e', b'l', 0x80, 0x02, b'l', b'o'],
            frag: Vec::new(),
            frag_text: true,
            mask_seed: 1,
        };
        assert_eq!(ws.next_frame(), Some(None), "첫 조각은 이벤트 없음");
        let ev = ws.next_frame().expect("프레임").expect("이벤트");
        assert_eq!(ev, Event::Message("Hello".to_string()));
    }
}
