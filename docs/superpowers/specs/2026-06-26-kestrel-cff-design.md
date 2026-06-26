# Kestrel — CFF/OTF 폰트 지원: 설계 문서

- 날짜: 2026-06-26
- 상태: 승인됨 (실용적 from-scratch 폰트 경로 2단계)

## 1. 맥락 / 목표

지금은 TrueType `glyf`(2차 베지어)만 읽는다. `.otf`에 흔한 **CFF**(PostScript 아웃라인, 3차 베지어)는 못 읽어 시스템 폰트/어도비 폰트를 못 쓴다. CFF 파서 + **Type 2 charstring 인터프리터**를 직접 구현해 `.otf`를 렌더한다.

## 2. 분할

- **A. 아웃라인 추상화 리팩터**: `Font::outline(gid)`가 포맷 무관 **평탄화된 폴리라인** `Vec<Vec<(f32,f32)>>`(폰트 단위, Y up)을 반환. glyf의 2차 베지어 평탄화를 raster에서 font로 이동. raster는 폴리라인만 받아 채운다. (glyf 동작 유지)
- **B. CFF 파서 + Type2 인터프리터**: `CFF ` 테이블 파싱 → charstring 실행 → 3차 베지어 평탄화 → 폴리라인.
- **C. 통합 + 검증**: `Font`가 `glyf` vs `CFF ` 테이블 유무로 분기. `.otf` 폰트 번들 후 렌더 확인.

## 3. A — 아웃라인 추상화

- `pub fn outline(&self, glyph_id: u16) -> Vec<Vec<(f32, f32)>>` (폴리라인, 닫힌 윤곽선들).
- glyf 경로: 단순 글리프 파싱(현행) → on/off 포인트 → **2차 베지어 평탄화**(raster에서 옮겨온 `flatten_contour`/`flatten_quad`) → 폴리라인.
- `raster::rasterize_glyph`: `font.outline(gid)`로 폴리라인 받아 bounds 계산 → 에지 → 커버리지 스캔라인(현행). on/off 처리 제거.

## 4. B — CFF

### 4.1 CFF 구조 파싱
- Header(major/minor/hdrSize/offSize)
- **INDEX** 자료구조: count(u16) + offSize(u8) + offsets[count+1] + data. 헬퍼로 구현.
- Name INDEX → Top DICT INDEX → String INDEX → Global Subr INDEX.
- **Top DICT** 파싱: operand/operator 스트림. 필요한 키: `CharStrings`(17, 오프셋), `Private`(18, size+offset), `charset`(15), `FontMatrix`(12 7, 기본 0.001).
- CharStrings INDEX(글리프별 charstring), Private DICT(`Subrs`(19) 로컬 서브루틴 오프셋, `defaultWidthX`/`nominalWidthX`), Local Subr INDEX.

### 4.2 Type 2 charstring 인터프리터
스택 머신으로 글리프 경로 생성. 지원 연산자:
- 이동/선: `rmoveto`(21) `hmoveto`(22) `vmoveto`(4) `rlineto`(5) `hlineto`(6) `vlineto`(7)
- 곡선(3차): `rrcurveto`(8) `hhcurveto`(27) `hvcurveto`(31) `vhcurveto`(30) `vvcurveto`(26) `rcurveline`(24) `rlinecurve`(25)
- 서브루틴: `callsubr`(10) `callgsubr`(29) `return`(11), 바이어스(서브루틴 수에 따라 107/1131/32768)
- 종료: `endchar`(14)
- 힌트: `hstem`(1) `vstem`(3) `hstemhm`(18) `vstemhm`(23) `hintmask`(19) `cntrmask`(20) — 힌트 자체는 무시하되 **hintmask 데이터 바이트는 정확히 소비**(스템 수 기반)
- 숫자 인코딩: 28(int16), 32..246(small), 247..254(2바이트), 255(16.16 고정소수)
- **너비(width)**: 첫 스택비움 연산자의 선행 홀수 오퍼랜드 = width → 무시(advance는 hmtx 사용)
- 3차 베지어는 평탄화해 폴리라인으로.

## 5. C — 통합

- `Font::from_bytes`: `glyf` 또는 `CFF ` 테이블 식별. CFF면 CFF 구조 파싱해 보관.
- `outline(gid)`: glyf면 glyf 경로, CFF면 charstring 인터프리터.
- `cmap`/`hmtx`/`hhea`/메트릭은 동일(OpenType 공통). loca/glyf는 CFF엔 없음 — 없을 때 에러 안 나게.
- `.otf`(CFF) 폰트 번들 → 글리프 덤프/페이지 렌더로 확인.

## 6. 범위 / 비범위

**범위**: CFF1 + Type2 charstring(단순 폰트), 3차 베지어, 서브루틴, hintmask 소비.

**비범위**: CFF2(가변), CID 폰트의 FDArray/FDSelect, seac/deprecated 연산자, 힌팅 실행(스냅), 인코딩 테이블. (필요해지면 후속)

## 7. 테스트 / 검증

- A: 기존 glyf 테스트(폴리라인 형태로 갱신) 통과 — 'o' ≥1 폴리라인, 공백 빈 폴리라인.
- B: CFF `.otf` 폰트 로드 → `outline(glyph_index('o'))`가 폴리라인 생성(점>0). charstring 인터프리터 단위 동작.
- C: `.otf` 폰트로 글리프 덤프("Hello") 렌더 → 사람이 읽힘(헤드리스). FontStack에 CFF 폰트 폴백으로 넣어도 동작.

## 8. 완료 기준

1. `Font::outline`이 glyf/CFF 모두에서 폴리라인 반환.
2. CFF `.otf` 폰트의 글자가 렌더된다(눈 검증).
3. 기존 모든 테스트 통과.
