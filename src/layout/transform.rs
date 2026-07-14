// CSS 변환 행렬: 2D(Mat/Mat3) 와 3D(Mat4) 조립, transform 문법 파싱.
// SVG 의 transform 은 문법이 CSS 와 달라(단위 없음, 콤마 선택) 따로 파싱한다.
use super::*;

// SVG viewBox "minx miny width height" → (minx, miny, width, height)
// SVG 의 transform 속성. CSS 와 **문법이 다르다**: 단위 없는 수(사용자 단위)를 쓰고,
// rotate(각도 [cx cy]) 처럼 회전 중심을 인자로 받으며, 구분자가 공백일 수 있다.
// CSS 파서에 넘기면 조용히 항등이 된다 (그룹이 엉뚱한 자리에 그려진다).
pub fn parse_svg_transform(text: &str) -> Mat3 {
    let mut m = Mat3::IDENTITY;
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open]
            .trim()
            .rsplit(|c: char| c.is_whitespace() || c == ')' || c == ',')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(close_rel) = rest[open..].find(')') else { break };
        let close = close_rel + open;
        let a: Vec<f32> = rest[open + 1..close]
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .filter_map(|t| t.parse::<f32>().ok())
            .collect();
        let g = |i: usize| a.get(i).copied().unwrap_or(0.0);
        let step = match name.as_str() {
            "translate" => Mat3 {
                m: [[1.0, 0.0, g(0)], [0.0, 1.0, a.get(1).copied().unwrap_or(0.0)], [0.0, 0.0, 1.0]],
            },
            "scale" => {
                let sx = a.first().copied().unwrap_or(1.0);
                let sy = a.get(1).copied().unwrap_or(sx);
                Mat3 { m: [[sx, 0.0, 0.0], [0.0, sy, 0.0], [0.0, 0.0, 1.0]] }
            }
            "rotate" => {
                let (s, c) = g(0).to_radians().sin_cos();
                let r = Mat3 { m: [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]] };
                if a.len() >= 3 {
                    // rotate(deg cx cy) = T(c) · R · T(-c)
                    let (cx, cy) = (g(1), g(2));
                    let t1 = Mat3 { m: [[1.0, 0.0, -cx], [0.0, 1.0, -cy], [0.0, 0.0, 1.0]] };
                    let t2 = Mat3 { m: [[1.0, 0.0, cx], [0.0, 1.0, cy], [0.0, 0.0, 1.0]] };
                    t1.then(&r).then(&t2)
                } else {
                    r
                }
            }
            "skewx" => Mat3 {
                m: [[1.0, g(0).to_radians().tan(), 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            },
            "skewy" => Mat3 {
                m: [[1.0, 0.0, 0.0], [g(0).to_radians().tan(), 1.0, 0.0], [0.0, 0.0, 1.0]],
            },
            "matrix" => Mat3 {
                m: [[g(0), g(2), g(4)], [g(1), g(3), g(5)], [0.0, 0.0, 1.0]],
            },
            _ => Mat3::IDENTITY,
        };
        // SVG 도 왼쪽 함수가 바깥쪽
        m = step.then(&m);
        rest = &rest[close + 1..];
    }
    m
}

// ── CSS 2D 변환 행렬 ──
// x' = a·x + c·y + e ;  y' = b·x + d·y + f   (CSS matrix(a,b,c,d,e,f) 와 같은 순서)
//
// 예전엔 translate/scale 만 박스 좌표를 직접 밀고 늘리는 식으로 처리하고
// rotate/skew/matrix 는 `_ => {}` 로 **조용히 무시**했다. 회전을 무시하면 화면은
// 멀쩡해 보이는데 실제와 다르다 — 가장 알아채기 어려운 종류의 거짓말이다.
// 이제 모든 함수를 행렬로 합성하고, 페인트가 서브트리 전체(글자·이미지·그림자 포함)를
// 그 행렬로 변환한다.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mat {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

// 3x3 투영행렬 (평면 요소의 3D 변환 결과). [x', y', w'] = M · [x, y, 1] 이고 실제 좌표는
// (x'/w', y'/w') 다 — perspective 가 있으면 w' 가 1 이 아니다.
//
// 왜 필요한가: rotateY/rotateX/perspective 는 2x3 아핀행렬로 표현할 수 없다.
// 예전엔 3D 함수를 만나면 **항등행렬로 두고 조용히 무시**했다 (요소가 안 돌아간 채 나왔다).
// 요소는 평평하므로(z=0) 4x4 를 만든 뒤 z 행/열을 접으면 3x3 투영행렬로 정확히 떨어진다.
// (preserve-3d 로 자손이 진짜 3D 공간에 서는 경우는 아직 다루지 않는다 — 정직하게.)
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Mat3 {
    pub m: [[f32; 3]; 3],
}

impl Mat3 {
    pub const IDENTITY: Mat3 =
        Mat3 { m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]] };

    pub fn from_affine(a: &Mat) -> Mat3 {
        Mat3 { m: [[a.a, a.c, a.e], [a.b, a.d, a.f], [0.0, 0.0, 1.0]] }
    }

    // self 다음에 other 를 적용 (other ∘ self)
    pub fn then(&self, other: &Mat3) -> Mat3 {
        let mut r = [[0.0f32; 3]; 3];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..3).map(|k| other.m[i][k] * self.m[k][j]).sum();
            }
        }
        Mat3 { m: r }
    }

    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let w = self.m[2][0] * x + self.m[2][1] * y + self.m[2][2];
        let ix = self.m[0][0] * x + self.m[0][1] * y + self.m[0][2];
        let iy = self.m[1][0] * x + self.m[1][1] * y + self.m[1][2];
        if w.abs() < 1e-9 {
            return (ix, iy);
        }
        (ix / w, iy / w)
    }

    pub fn invert(&self) -> Option<Mat3> {
        let m = &self.m;
        let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
            - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
            + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
        if det.abs() < 1e-9 {
            return None;
        }
        let id = 1.0 / det;
        let mut r = [[0.0f32; 3]; 3];
        r[0][0] = (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * id;
        r[0][1] = (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * id;
        r[0][2] = (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * id;
        r[1][0] = (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * id;
        r[1][1] = (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * id;
        r[1][2] = (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * id;
        r[2][0] = (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * id;
        r[2][1] = (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * id;
        r[2][2] = (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * id;
        Some(Mat3 { m: r })
    }

    pub fn is_identity(&self) -> bool {
        *self == Mat3::IDENTITY
    }

    // 아핀인가 (원근 항 없음)
    #[cfg(test)]
    pub fn is_affine(&self) -> bool {
        self.m[2][0].abs() < 1e-6 && self.m[2][1].abs() < 1e-6
    }

    // 변환된 사각형의 경계 상자 (getBoundingClientRect/히트 테스트용)
    pub fn bounds(&self, r: Rect) -> Rect {
        let pts = [
            self.apply(r.x, r.y),
            self.apply(r.x + r.width, r.y),
            self.apply(r.x, r.y + r.height),
            self.apply(r.x + r.width, r.y + r.height),
        ];
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        for (x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        Rect { x: x0, y: y0, width: x1 - x0, height: y1 - y0 }
    }

}

// 4x4 (3D 변환 조립용). 행렬 곱만 쓰고, 마지막에 z 를 접어 Mat3 로 만든다.
#[derive(Clone, Copy)]
pub(crate) struct Mat4 {
    pub(crate) m: [[f32; 4]; 4],
}

impl Mat4 {
    pub(crate) const IDENTITY: Mat4 = Mat4 {
        m: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };
    pub(crate) fn mul(&self, o: &Mat4) -> Mat4 {
        let mut r = [[0.0f32; 4]; 4];
        for (i, row) in r.iter_mut().enumerate() {
            for (j, cell) in row.iter_mut().enumerate() {
                *cell = (0..4).map(|k| self.m[i][k] * o.m[k][j]).sum();
            }
        }
        Mat4 { m: r }
    }
    // 평행이동 (4x4)
    pub(crate) fn translate(tx: f32, ty: f32) -> Mat4 {
        let mut t = Mat4::IDENTITY;
        t.m[0][3] = tx;
        t.m[1][3] = ty;
        t
    }

    // 원근: 보는 거리 d (양수). z 가 커질수록 화면에서 작아진다.
    pub(crate) fn perspective(d: f32) -> Mat4 {
        let mut p = Mat4::IDENTITY;
        if d > 0.0 {
            p.m[3][2] = -1.0 / d;
        }
        p
    }

    // 평면 요소(z=0) → 3x3 투영행렬 (0,1,3 행/열만 남긴다)
    pub(crate) fn flatten(&self) -> Mat3 {
        let m = &self.m;
        Mat3 {
            m: [
                [m[0][0], m[0][1], m[0][3]],
                [m[1][0], m[1][1], m[1][3]],
                [m[3][0], m[3][1], m[3][3]],
            ],
        }
    }
}

impl Mat {
    pub const IDENTITY: Mat = Mat { a: 1.0, b: 0.0, c: 0.0, d: 1.0, e: 0.0, f: 0.0 };

    pub fn is_identity(&self) -> bool {
        *self == Mat::IDENTITY
    }

    // 축 정렬인가 (회전/기울임 없음) — 사각형이 사각형으로 남는가

    // 2D 파서 검증용 (실행 경로는 Mat3 를 쓴다)
    #[cfg(test)]
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        (self.a * x + self.c * y + self.e, self.b * x + self.d * y + self.f)
    }

    // self 다음에 m 을 적용 (m ∘ self)
    pub fn then(&self, m: &Mat) -> Mat {
        Mat {
            a: m.a * self.a + m.c * self.b,
            b: m.b * self.a + m.d * self.b,
            c: m.a * self.c + m.c * self.d,
            d: m.b * self.c + m.d * self.d,
            e: m.a * self.e + m.c * self.f + m.e,
            f: m.b * self.e + m.d * self.f + m.f,
        }
    }

}

// 각도 문자열 → 라디안 (deg/rad/grad/turn)
pub(crate) fn parse_angle(s: &str) -> f32 {
    let t = s.trim();
    let num = |suf: &str| t.strip_suffix(suf).and_then(|n| n.trim().parse::<f32>().ok());
    if let Some(v) = num("deg") {
        return v.to_radians();
    }
    if let Some(v) = num("rad") {
        return v;
    }
    if let Some(v) = num("grad") {
        return v * std::f32::consts::PI / 200.0;
    }
    if let Some(v) = num("turn") {
        return v * std::f32::consts::TAU;
    }
    t.parse::<f32>().map(|v| v.to_radians()).unwrap_or(0.0)
}

// transform 함수 목록 → 행렬 (요소 로컬 좌표, 원점은 transform-origin).
// bw/bh 는 border box 크기 (translate 의 % 해석 기준).
pub fn parse_transform(text: &str, bw: f32, bh: f32) -> Mat {
    let mut m = Mat::IDENTITY;
    let mut rest = text;
    while let Some(open) = rest.find('(') {
        let name = rest[..open]
            .trim()
            .rsplit(|c: char| c.is_whitespace() || c == ')' || c == ',')
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let Some(close_rel) = rest[open..].find(')') else { break };
        let close = close_rel + open;
        let args: Vec<&str> = rest[open + 1..close].split(',').map(|s| s.trim()).collect();
        let len = |t: &str, base: f32| -> f32 {
            if let Some(p) = t.strip_suffix('%') {
                p.trim().parse::<f32>().map(|v| v / 100.0 * base).unwrap_or(0.0)
            } else {
                crate::css::parse_len_px(t).unwrap_or(0.0)
            }
        };
        let num = |t: &str| t.parse::<f32>().unwrap_or(1.0);
        let get = |i: usize| args.get(i).copied().unwrap_or("");
        let step = match name.as_str() {
            "translate" => Mat {
                e: len(get(0), bw),
                f: args.get(1).map(|t| len(t, bh)).unwrap_or(0.0),
                ..Mat::IDENTITY
            },
            "translatex" => Mat { e: len(get(0), bw), ..Mat::IDENTITY },
            "translatey" => Mat { f: len(get(0), bh), ..Mat::IDENTITY },
            "scale" => {
                let sx = num(get(0));
                let sy = args.get(1).map(|t| num(t)).unwrap_or(sx);
                Mat { a: sx, d: sy, ..Mat::IDENTITY }
            }
            "scalex" => Mat { a: num(get(0)), ..Mat::IDENTITY },
            "scaley" => Mat { d: num(get(0)), ..Mat::IDENTITY },
            "rotate" | "rotatez" => {
                let (s, c) = parse_angle(get(0)).sin_cos();
                Mat { a: c, b: s, c: -s, d: c, e: 0.0, f: 0.0 }
            }
            "skew" => {
                let ax = parse_angle(get(0)).tan();
                let ay = args.get(1).map(|t| parse_angle(t).tan()).unwrap_or(0.0);
                Mat { a: 1.0, b: ay, c: ax, d: 1.0, e: 0.0, f: 0.0 }
            }
            "skewx" => Mat { c: parse_angle(get(0)).tan(), ..Mat::IDENTITY },
            "skewy" => Mat { b: parse_angle(get(0)).tan(), ..Mat::IDENTITY },
            "matrix" => Mat {
                a: args.first().map(|t| num(t)).unwrap_or(1.0),
                b: args.get(1).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
                c: args.get(2).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
                d: args.get(3).map(|t| num(t)).unwrap_or(1.0),
                e: args.get(4).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
                f: args.get(5).map(|t| t.parse().unwrap_or(0.0)).unwrap_or(0.0),
            },
            _ => Mat::IDENTITY, // 3D 함수는 parse_transform3d 가 다룬다

        };
        m = step.then(&m); // CSS 는 왼쪽 함수가 바깥쪽
        rest = &rest[close + 1..];
    }
    m
}
