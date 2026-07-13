// 임의 정밀도 정수 (ECMAScript BigInt). 부호 + 크기(리틀엔디언 u32 림브).
//
// 왜 직접 쓰는가: 1n 을 f64 로 두면 2n**64n 같은 값이 조용히 틀린다. 조용히 틀린 답은
// 미구현보다 나쁘다. 사이트는 typeof BigInt 로 기능을 탐지하고 정수 경로를 타므로,
// 있는 척하려면 정확해야 한다.
//
// 비트 연산은 무한폭 2의 보수 의미론이다 (표준 §6.1.6.2): 음수는 …111 로 확장된다.

use std::cmp::Ordering;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BigInt {
    pub neg: bool,     // 0 은 항상 neg=false
    pub mag: Vec<u32>, // 리틀엔디언, 선행(상위) 0 없음. 0 이면 빈 벡터.
}

impl BigInt {
    pub fn zero() -> Self {
        BigInt { neg: false, mag: Vec::new() }
    }

    pub fn is_zero(&self) -> bool {
        self.mag.is_empty()
    }

    pub fn from_i64(mut v: i64) -> Self {
        let neg = v < 0;
        let mut mag = Vec::new();
        // i64::MIN 은 부호 반전이 오버플로 → u64 로 먼저 옮긴다
        let mut u = if neg { (v as i128).unsigned_abs() as u128 } else { v as u128 };
        v = 0;
        let _ = v;
        while u > 0 {
            mag.push((u & 0xffff_ffff) as u32);
            u >>= 32;
        }
        let mut r = BigInt { neg, mag };
        r.trim();
        r
    }

    // 정수 f64 → BigInt. 정수가 아니거나 유한하지 않으면 None (표준: RangeError).
    pub fn from_f64(v: f64) -> Option<Self> {
        if !v.is_finite() || v.fract() != 0.0 {
            return None;
        }
        let neg = v < 0.0;
        let mut a = v.abs();
        let mut mag = Vec::new();
        while a >= 1.0 {
            let rem = a % 4294967296.0;
            mag.push(rem as u32);
            a = (a - rem) / 4294967296.0;
        }
        let mut r = BigInt { neg, mag };
        r.trim();
        Some(r)
    }

    pub fn to_f64(&self) -> f64 {
        let mut out = 0.0f64;
        for limb in self.mag.iter().rev() {
            out = out * 4294967296.0 + *limb as f64;
        }
        if self.neg {
            -out
        } else {
            out
        }
    }

    // "123", "0x1f", "0b101", "0o17", "  42  ", "" (=0). 실패하면 None (SyntaxError).
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim();
        if t.is_empty() {
            return Some(BigInt::zero());
        }
        let (neg, body) = match t.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, t.strip_prefix('+').unwrap_or(t)),
        };
        let (radix, digits) = if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
            (16u32, h)
        } else if let Some(b) = body.strip_prefix("0b").or_else(|| body.strip_prefix("0B")) {
            (2, b)
        } else if let Some(o) = body.strip_prefix("0o").or_else(|| body.strip_prefix("0O")) {
            (8, o)
        } else {
            (10, body)
        };
        if digits.is_empty() {
            return None;
        }
        let mut out = BigInt::zero();
        let base = BigInt::from_i64(radix as i64);
        for c in digits.chars() {
            if c == '_' {
                continue; // 숫자 구분자
            }
            let d = c.to_digit(radix)?;
            out = out.mul(&base).add(&BigInt::from_i64(d as i64));
        }
        out.neg = neg && !out.is_zero();
        Some(out)
    }

    pub fn to_string_radix(&self, radix: u32) -> String {
        if self.is_zero() {
            return "0".to_string();
        }
        let mut digits = Vec::new();
        let mut cur = self.abs();
        let base = BigInt::from_i64(radix as i64);
        while !cur.is_zero() {
            let (q, r) = cur.divrem(&base);
            let d = r.mag.first().copied().unwrap_or(0);
            digits.push(std::char::from_digit(d, radix).unwrap());
            cur = q;
        }
        if self.neg {
            digits.push('-');
        }
        digits.iter().rev().collect()
    }

    fn trim(&mut self) {
        while self.mag.last() == Some(&0) {
            self.mag.pop();
        }
        if self.mag.is_empty() {
            self.neg = false;
        }
    }

    pub fn abs(&self) -> Self {
        BigInt { neg: false, mag: self.mag.clone() }
    }

    pub fn negate(&self) -> Self {
        if self.is_zero() {
            return BigInt::zero();
        }
        BigInt { neg: !self.neg, mag: self.mag.clone() }
    }

    // 크기 비교 (부호 무시)
    fn cmp_mag(a: &[u32], b: &[u32]) -> Ordering {
        if a.len() != b.len() {
            return a.len().cmp(&b.len());
        }
        for i in (0..a.len()).rev() {
            if a[i] != b[i] {
                return a[i].cmp(&b[i]);
            }
        }
        Ordering::Equal
    }

    fn add_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len().max(b.len()) + 1);
        let mut carry = 0u64;
        for i in 0..a.len().max(b.len()) {
            let x = *a.get(i).unwrap_or(&0) as u64;
            let y = *b.get(i).unwrap_or(&0) as u64;
            let s = x + y + carry;
            out.push((s & 0xffff_ffff) as u32);
            carry = s >> 32;
        }
        if carry > 0 {
            out.push(carry as u32);
        }
        out
    }

    // a - b (a >= b 가정)
    fn sub_mag(a: &[u32], b: &[u32]) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len());
        let mut borrow = 0i64;
        for i in 0..a.len() {
            let x = a[i] as i64;
            let y = *b.get(i).unwrap_or(&0) as i64;
            let mut d = x - y - borrow;
            if d < 0 {
                d += 1 << 32;
                borrow = 1;
            } else {
                borrow = 0;
            }
            out.push(d as u32);
        }
        while out.last() == Some(&0) {
            out.pop();
        }
        out
    }

    pub fn add(&self, other: &Self) -> Self {
        if self.neg == other.neg {
            let mut r = BigInt { neg: self.neg, mag: Self::add_mag(&self.mag, &other.mag) };
            r.trim();
            return r;
        }
        match Self::cmp_mag(&self.mag, &other.mag) {
            Ordering::Equal => BigInt::zero(),
            Ordering::Greater => {
                let mut r = BigInt { neg: self.neg, mag: Self::sub_mag(&self.mag, &other.mag) };
                r.trim();
                r
            }
            Ordering::Less => {
                let mut r = BigInt { neg: other.neg, mag: Self::sub_mag(&other.mag, &self.mag) };
                r.trim();
                r
            }
        }
    }

    pub fn sub(&self, other: &Self) -> Self {
        self.add(&other.negate())
    }

    pub fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return BigInt::zero();
        }
        let mut out = vec![0u32; self.mag.len() + other.mag.len()];
        for (i, &x) in self.mag.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &y) in other.mag.iter().enumerate() {
                let idx = i + j;
                let cur = out[idx] as u64 + x as u64 * y as u64 + carry;
                out[idx] = (cur & 0xffff_ffff) as u32;
                carry = cur >> 32;
            }
            let mut idx = i + other.mag.len();
            while carry > 0 {
                let cur = out[idx] as u64 + carry;
                out[idx] = (cur & 0xffff_ffff) as u32;
                carry = cur >> 32;
                idx += 1;
            }
        }
        let mut r = BigInt { neg: self.neg != other.neg, mag: out };
        r.trim();
        r
    }

    // 절단 나눗셈 (몫은 0 쪽으로, 나머지는 피제수 부호) — 표준 BigInt 의미론.
    // 0 으로 나누면 None (RangeError).
    pub fn divrem(&self, other: &Self) -> (Self, Self) {
        self.checked_divrem(other).expect("0 으로 나눔")
    }

    pub fn checked_divrem(&self, other: &Self) -> Option<(Self, Self)> {
        if other.is_zero() {
            return None;
        }
        if Self::cmp_mag(&self.mag, &other.mag) == Ordering::Less {
            return Some((BigInt::zero(), self.clone()));
        }
        // 이진 장제법 (비트 단위). 림브 단위 Knuth D 보다 느리지만 명백히 옳다.
        let n = self.abs();
        let d = other.abs();
        let bits = n.bit_len();
        let mut q = vec![0u32; n.mag.len()];
        let mut rem = BigInt::zero();
        for i in (0..bits).rev() {
            rem = rem.shl_bits(1);
            if n.bit(i) {
                if rem.mag.is_empty() {
                    rem.mag.push(1);
                } else {
                    rem.mag[0] |= 1;
                }
            }
            if Self::cmp_mag(&rem.mag, &d.mag) != Ordering::Less {
                rem = BigInt { neg: false, mag: Self::sub_mag(&rem.mag, &d.mag) };
                q[i / 32] |= 1 << (i % 32);
            }
        }
        let mut quo = BigInt { neg: self.neg != other.neg, mag: q };
        quo.trim();
        rem.neg = self.neg && !rem.is_zero(); // 나머지는 피제수 부호
        rem.trim_keep_sign();
        Some((quo, rem))
    }

    fn trim_keep_sign(&mut self) {
        while self.mag.last() == Some(&0) {
            self.mag.pop();
        }
        if self.mag.is_empty() {
            self.neg = false;
        }
    }

    pub fn bit_len(&self) -> usize {
        match self.mag.last() {
            None => 0,
            Some(&top) => (self.mag.len() - 1) * 32 + (32 - top.leading_zeros() as usize),
        }
    }

    fn bit(&self, i: usize) -> bool {
        let limb = i / 32;
        limb < self.mag.len() && (self.mag[limb] >> (i % 32)) & 1 == 1
    }

    // 크기만 왼쪽으로 시프트 (부호 유지)
    fn shl_bits(&self, n: usize) -> Self {
        if self.is_zero() {
            return BigInt::zero();
        }
        let limb_shift = n / 32;
        let bit_shift = n % 32;
        let mut out = vec![0u32; limb_shift];
        let mut carry = 0u32;
        for &x in &self.mag {
            if bit_shift == 0 {
                out.push(x);
            } else {
                out.push((x << bit_shift) | carry);
                carry = x >> (32 - bit_shift);
            }
        }
        if bit_shift != 0 && carry != 0 {
            out.push(carry);
        }
        let mut r = BigInt { neg: self.neg, mag: out };
        r.trim();
        r
    }

    // 산술 오른쪽 시프트 (2의 보수: 음수는 -무한대 쪽으로 내림)
    fn shr_bits(&self, n: usize) -> Self {
        if self.is_zero() {
            return BigInt::zero();
        }
        if n >= self.bit_len() && !self.neg {
            return BigInt::zero();
        }
        let limb_shift = n / 32;
        let bit_shift = n % 32;
        if limb_shift >= self.mag.len() {
            return if self.neg { BigInt::from_i64(-1) } else { BigInt::zero() };
        }
        let mut out = Vec::with_capacity(self.mag.len() - limb_shift);
        for i in limb_shift..self.mag.len() {
            let lo = self.mag[i] >> bit_shift;
            let hi = if bit_shift == 0 {
                0
            } else {
                self.mag.get(i + 1).map(|&x| x << (32 - bit_shift)).unwrap_or(0)
            };
            out.push(lo | hi);
        }
        let mut r = BigInt { neg: self.neg, mag: out };
        r.trim();
        // 음수는 버려진 비트가 있으면 한 칸 더 내린다 (floor 의미)
        if self.neg {
            let mut dropped = false;
            for i in 0..n.min(self.bit_len()) {
                if self.bit(i) {
                    dropped = true;
                    break;
                }
            }
            if dropped {
                r = r.sub(&BigInt::from_i64(1));
            }
        }
        r
    }

    pub fn shl(&self, n: &Self) -> Self {
        let k = n.to_f64();
        if k < 0.0 {
            return self.shr_bits((-k) as usize);
        }
        self.shl_bits(k as usize)
    }

    pub fn shr(&self, n: &Self) -> Self {
        let k = n.to_f64();
        if k < 0.0 {
            return self.shl_bits((-k) as usize);
        }
        self.shr_bits(k as usize)
    }

    pub fn pow(&self, exp: &Self) -> Option<Self> {
        if exp.neg {
            return None; // BigInt 음수 지수 → RangeError
        }
        let mut result = BigInt::from_i64(1);
        let mut base = self.clone();
        let mut e = exp.clone();
        let two = BigInt::from_i64(2);
        while !e.is_zero() {
            let (q, r) = e.divrem(&two);
            if !r.is_zero() {
                result = result.mul(&base);
            }
            base = base.mul(&base);
            e = q;
        }
        Some(result)
    }

    // 2의 보수 무한폭 비트 연산 (표준 §6.1.6.2). 음수는 …111 로 확장된다.
    fn to_twos(&self, len: usize) -> Vec<u32> {
        let mut v = vec![0u32; len];
        for (i, &x) in self.mag.iter().enumerate() {
            if i < len {
                v[i] = x;
            }
        }
        if self.neg {
            // ~v + 1
            for x in v.iter_mut() {
                *x = !*x;
            }
            let mut carry = 1u64;
            for x in v.iter_mut() {
                let s = *x as u64 + carry;
                *x = (s & 0xffff_ffff) as u32;
                carry = s >> 32;
                if carry == 0 {
                    break;
                }
            }
        }
        v
    }

    fn from_twos(v: Vec<u32>, neg: bool) -> Self {
        let mut v = v;
        if neg {
            // -( ~v + 1 )
            for x in v.iter_mut() {
                *x = !*x;
            }
            let mut carry = 1u64;
            for x in v.iter_mut() {
                let s = *x as u64 + carry;
                *x = (s & 0xffff_ffff) as u32;
                carry = s >> 32;
                if carry == 0 {
                    break;
                }
            }
        }
        let mut r = BigInt { neg, mag: v };
        r.trim();
        r
    }

    pub fn bitand(&self, other: &Self) -> Self {
        let len = self.mag.len().max(other.mag.len()) + 1;
        let a = self.to_twos(len);
        let b = other.to_twos(len);
        let v: Vec<u32> = a.iter().zip(b.iter()).map(|(x, y)| x & y).collect();
        Self::from_twos(v, self.neg && other.neg)
    }

    pub fn bitor(&self, other: &Self) -> Self {
        let len = self.mag.len().max(other.mag.len()) + 1;
        let a = self.to_twos(len);
        let b = other.to_twos(len);
        let v: Vec<u32> = a.iter().zip(b.iter()).map(|(x, y)| x | y).collect();
        Self::from_twos(v, self.neg || other.neg)
    }

    pub fn bitxor(&self, other: &Self) -> Self {
        let len = self.mag.len().max(other.mag.len()) + 1;
        let a = self.to_twos(len);
        let b = other.to_twos(len);
        let v: Vec<u32> = a.iter().zip(b.iter()).map(|(x, y)| x ^ y).collect();
        Self::from_twos(v, self.neg != other.neg)
    }

    // ~x = -x - 1
    pub fn bitnot(&self) -> Self {
        self.negate().sub(&BigInt::from_i64(1))
    }
}

impl std::fmt::Display for BigInt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_string_radix(10))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints() {
        assert_eq!(BigInt::parse("0").unwrap().to_string(), "0");
        assert_eq!(BigInt::parse("123456789012345678901234567890").unwrap().to_string(),
                   "123456789012345678901234567890");
        assert_eq!(BigInt::parse("-42").unwrap().to_string(), "-42");
        assert_eq!(BigInt::parse("0x1f").unwrap().to_string(), "31");
        assert_eq!(BigInt::parse("0b1010").unwrap().to_string(), "10");
        assert_eq!(BigInt::parse("1_000").unwrap().to_string(), "1000");
        assert!(BigInt::parse("12abc").is_none());
    }

    #[test]
    fn arithmetic_is_exact_beyond_f64() {
        // f64 로는 18446744073709552000 이 되는 값 — BigInt 는 정확해야 한다
        let two = BigInt::from_i64(2);
        let p64 = two.pow(&BigInt::from_i64(64)).unwrap();
        assert_eq!(p64.to_string(), "18446744073709551616");
        // 곱셈/덧셈/뺄셈
        let a = BigInt::parse("123456789012345678901234567890").unwrap();
        let b = BigInt::parse("987654321098765432109876543210").unwrap();
        assert_eq!(
            a.mul(&b).to_string(),
            "121932631137021795226185032733622923332237463801111263526900"
        );
        assert_eq!(a.add(&b).to_string(), "1111111110111111111011111111100");
        assert_eq!(a.sub(&b).to_string(), "-864197532086419753208641975320");
    }

    #[test]
    fn division_truncates_toward_zero() {
        let (q, r) = BigInt::from_i64(7).divrem(&BigInt::from_i64(2));
        assert_eq!((q.to_string(), r.to_string()), ("3".into(), "1".into()));
        // 절단(0 쪽) — 내림이 아니다
        let (q, r) = BigInt::from_i64(-7).divrem(&BigInt::from_i64(2));
        assert_eq!((q.to_string(), r.to_string()), ("-3".into(), "-1".into()));
        let (q, r) = BigInt::from_i64(7).divrem(&BigInt::from_i64(-2));
        assert_eq!((q.to_string(), r.to_string()), ("-3".into(), "1".into()));
        // 큰 수
        let a = BigInt::parse("121932631137021795226185032733622923332237463801111263526900").unwrap();
        let b = BigInt::parse("987654321098765432109876543210").unwrap();
        let (q, r) = a.divrem(&b);
        assert_eq!(q.to_string(), "123456789012345678901234567890");
        assert!(r.is_zero());
        assert!(BigInt::from_i64(1).checked_divrem(&BigInt::zero()).is_none(), "0 으로 나누기");
    }

    #[test]
    fn bitwise_uses_twos_complement() {
        // -1 은 무한폭에서 …111 → -1 & x == x
        let m1 = BigInt::from_i64(-1);
        assert_eq!(m1.bitand(&BigInt::from_i64(12345)).to_string(), "12345");
        assert_eq!(BigInt::from_i64(12).bitand(&BigInt::from_i64(10)).to_string(), "8");
        assert_eq!(BigInt::from_i64(12).bitor(&BigInt::from_i64(10)).to_string(), "14");
        assert_eq!(BigInt::from_i64(12).bitxor(&BigInt::from_i64(10)).to_string(), "6");
        assert_eq!(BigInt::from_i64(5).bitnot().to_string(), "-6"); // ~5 = -6
        assert_eq!(BigInt::from_i64(-5).bitand(&BigInt::from_i64(3)).to_string(), "3"); // -5 = …1011
        // 시프트
        assert_eq!(BigInt::from_i64(1).shl(&BigInt::from_i64(64)).to_string(), "18446744073709551616");
        assert_eq!(BigInt::from_i64(-8).shr(&BigInt::from_i64(1)).to_string(), "-4");
        assert_eq!(BigInt::from_i64(-7).shr(&BigInt::from_i64(1)).to_string(), "-4", "음수는 내림");
    }

    #[test]
    fn f64_roundtrip() {
        assert_eq!(BigInt::from_f64(9007199254740992.0).unwrap().to_string(), "9007199254740992");
        assert!(BigInt::from_f64(1.5).is_none(), "정수가 아니면 실패");
        assert!(BigInt::from_f64(f64::NAN).is_none());
        assert_eq!(BigInt::parse("255").unwrap().to_f64(), 255.0);
    }
}
