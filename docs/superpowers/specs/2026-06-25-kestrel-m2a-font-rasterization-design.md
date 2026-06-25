# Kestrel M2a — 폰트 파싱 + 글리프 래스터화: 설계 문서

- 날짜: 2026-06-25
- 상태: 승인됨 (브레인스토밍 완료)
- 범위: M2a 만. M2b(인라인 레이아웃 + 페인트 통합)는 별도 스펙.

## 1. 맥락

M1에서 HTML/CSS → DOM → 스타일 → 블록 레이아웃 → 페인트 → 창 파이프라인을 완성했다. M2는 여기에 **텍스트**를 더한다. 전부 0부터 직접 구현한다는 프로젝트 원칙에 따라 폰트 래스터화도 라이브러리 없이 직접 짠다.

M2는 둘로 분할한다:
- **M2a (이 문서)**: TrueType 폰트 파일을 직접 파싱해 글리프 외곽선과 메트릭을 얻고, 외곽선을 안티앨리어싱된 커버리지 비트맵으로 래스터화한다. "글자 한 개를 이미지로 뽑는다"로 독립 검증 가능.
- **M2b (후속)**: 글리프들을 인라인 레이아웃(워드랩)으로 배치하고 캔버스에 알파 블렌딩으로 칠한다. "문단이 창에 뜬다"로 검증.

결정사항(브레인스토밍):
- 문자 범위: **라틴/ASCII 먼저** (cmap format 4, 단순 글리프만).
- 래스터화: **커버리지 스캔라인** 방식 (슈퍼샘플링 아님).

## 2. 목표 (M2a, 한 줄)

번들된 TrueType 폰트를 직접 파싱해, 임의의 ASCII 글자를 주어진 픽셀 크기로 **안티앨리어싱된 그레이스케일 커버리지 비트맵**으로 래스터화한다.

## 3. 아키텍처

새 모듈 두 개. 기존 M1 모듈(layout/paint)은 M2a에서 **건드리지 않는다** (통합은 M2b).

```
폰트 바이트 → [font]   → Font (테이블 룩업) + Glyph(외곽선) + 메트릭
Glyph        → [raster] → CoverageBitmap (AA 그레이스케일) + GlyphCache
```

| 모듈 | 입력 → 출력 | 책임 |
|------|------------|------|
| `font.rs` | `Vec<u8>` → `Font` | TrueType 테이블 파싱, cmap 룩업, 글리프 외곽선 추출, 메트릭 |
| `raster.rs` | `Glyph` + 픽셀 크기 → `CoverageBitmap` | 외곽선 → 커버리지 스캔라인 래스터화, 글리프 캐시 |

### 3.1 모듈 간 계약 타입 (M2b가 소비)

```
// font.rs
pub struct Font { /* 원본 바이트 + 파싱된 테이블 오프셋 */ }
impl Font {
    pub fn from_bytes(data: Vec<u8>) -> Result<Font, FontError>;
    pub fn units_per_em(&self) -> u16;
    pub fn ascent(&self) -> i16;        // hhea, font units
    pub fn descent(&self) -> i16;       // hhea, font units (음수)
    pub fn line_gap(&self) -> i16;      // hhea
    pub fn glyph_index(&self, c: char) -> u16;          // cmap format 4, 없으면 0(.notdef)
    pub fn advance_width(&self, glyph_id: u16) -> u16;  // hmtx, font units
    pub fn outline(&self, glyph_id: u16) -> Glyph;      // glyf, 빈 글리프면 contours 비어있음
}

pub struct Glyph {
    pub contours: Vec<Contour>,   // 닫힌 윤곽선들
    pub x_min: i16, pub y_min: i16, pub x_max: i16, pub y_max: i16, // glyf 바운딩 박스
}
pub struct Contour { pub points: Vec<Point> }   // on/off-curve 포함
pub struct Point { pub x: f32, pub y: f32, pub on_curve: bool }  // font units

// raster.rs
pub struct CoverageBitmap {
    pub width: usize,
    pub height: usize,
    pub data: Vec<u8>,   // 0..=255 커버리지(알파), width*height, 행 우선
    pub left: i32,       // 펜 원점(베이스라인 위 origin) 기준 비트맵 좌상단 x 오프셋
    pub top: i32,        // 베이스라인 기준 비트맵 상단 y 오프셋 (위가 +)
    pub advance: f32,    // 스케일된 advance(px). 다음 글자로 펜 이동량
}
pub fn rasterize_glyph(font: &Font, glyph_id: u16, px_per_em: f32) -> CoverageBitmap;

pub struct GlyphCache { /* (glyph_id, px bits) → CoverageBitmap */ }
impl GlyphCache {
    pub fn new() -> GlyphCache;
    pub fn get(&mut self, font: &Font, glyph_id: u16, px_per_em: f32) -> &CoverageBitmap;
}
```

## 4. `font.rs` 상세

TrueType(스냅샷)은 빅엔디언 바이너리. 필요한 테이블만 파싱한다.

- **테이블 디렉터리**: 파일 헤더(`sfnt` 버전 + numTables)에서 각 테이블의 태그/오프셋/길이를 읽어 맵으로 보관.
- **`head`**: `unitsPerEm`, `indexToLocFormat`(loca 16/32비트 여부).
- **`maxp`**: `numGlyphs`.
- **`hhea`**: `ascent`, `descent`, `lineGap`, `numberOfHMetrics`.
- **`hmtx`**: 글리프별 advance width (마지막 advance가 이후 글리프에 반복 적용되는 규칙 처리).
- **`cmap`**: 유니코드 BMP용 **format 4** 서브테이블 선택(platform 3 encoding 1, 또는 platform 0). char → glyph id.
- **`loca`**: glyph id → `glyf` 내 오프셋 (indexToLocFormat에 따라 16/32비트).
- **`glyf`**: 글리프 윤곽선. **단순 글리프만** 지원 (numberOfContours ≥ 0). 합성 글리프(< 0)는 M2a 비범위 — ASCII엔 없음. 빈 글리프(공백 등, 길이 0)는 외곽선 없는 Glyph로 처리하고 advance만 사용.

글리프 윤곽선 디코딩: endPtsOfContours, 플래그(반복 플래그 처리), x/y 좌표(델타, short/same 비트) 디코딩 → on/off-curve 포인트 목록. 2차 베지어는 off-curve 점이 제어점이며, 연속 off-curve 사이에는 암묵적 on-curve 중점이 들어간다(래스터화 단계에서 처리).

## 5. `raster.rs` 상세 — 커버리지 스캔라인

목표: 글리프 윤곽선(font units)을 `px_per_em` 크기의 AA 커버리지 비트맵으로.

1. **스케일 + Y 뒤집기**: scale = px_per_em / unitsPerEm. 폰트는 Y가 위로 증가, 비트맵은 아래로 증가 → Y 반전. 비트맵 크기는 글리프 바운딩 박스를 스케일해 결정(여백 1px). `left`/`top`은 바운딩 박스에서 계산.
2. **윤곽선 평탄화**: 2차 베지어를 재귀 분할(평탄도 임계값)로 직선 세그먼트 열로 변환. 연속 off-curve 점 사이 암묵 중점 삽입.
3. **커버리지 스캔라인 (분석적 수평 + 수직 오버샘플링)**:
   - 각 픽셀 행을 수직으로 N개(예: N=5) 서브스캔라인으로 나눈다.
   - 각 서브스캔라인에서 모든 에지와의 교점 x를 구하고 정렬, non-zero/even-odd 규칙으로 내부 구간을 정한다.
   - 내부 구간을 픽셀 격자에 **분석적으로** 매핑해 각 픽셀의 수평 커버리지(부분 픽셀 포함)를 누적.
   - N개 서브스캔라인 평균이 그 픽셀 행의 커버리지(0..255).
   - (이 방식이 stb_truetype의 비SIMD 경로와 동형 — 수평은 정확, 수직은 N배 오버샘플. "커버리지 스캔라인"의 실용적 형태.)
4. 결과 `CoverageBitmap` 반환. **글리프 캐시**가 (glyph_id, px) 단위로 결과를 보관해 재래스터화를 막는다(빠름/가벼움 목표).

## 6. 범위 / 비범위

**범위**: TrueType `glyf` 단순 글리프, cmap format 4, 라틴/ASCII, 단일 폰트, AA 커버리지 비트맵, 글리프 캐시.

**비범위 (후속)**: 합성 글리프, CFF/OTF 아웃라인, 커닝(`kern`/GPOS), 힌팅, 서브픽셀 렌더링, 한글/유니코드 전 범위, 인라인 레이아웃과 페인트 통합(M2b).

## 7. 폰트 에셋

`assets/fonts/`에 오픈 라이선스 TrueType(`glyf`) 폰트 1종 번들. 구현 시 `glyf` 테이블 존재를 확인하고 고정(예: Roboto 또는 DejaVu Sans, OFL). 라이선스 파일도 함께 둔다.

## 8. 에러 처리

- `Font::from_bytes`는 필수 테이블 누락/버전 불일치 시 `FontError` 반환.
- 글리프 디코딩 중 합성 글리프를 만나면 빈 외곽선으로 처리(M2a 비범위)하되 패닉하지 않는다.
- 범위를 벗어난 코드포인트는 glyph id 0(.notdef).

## 9. 테스트 / 검증

- `font.rs` 단위 테스트:
  - `glyph_index('A')`가 0이 아니고, `glyph_index('\u{FFFF}')` 같은 미지원은 0.
  - `advance_width`가 공백/문자에 대해 합리적 양수.
  - `outline('o')`의 contour 수 ≥ 1, 포인트 수 > 0; 빈 글리프(스페이스)는 contour 0.
- `raster.rs` 단위 테스트:
  - `rasterize_glyph` 결과 비트맵의 합(잉크량)이 0보다 크고, 글리프 바운딩 박스 안에 들어옴.
  - 가장자리 픽셀에 중간 커버리지(0<val<255)가 존재 → AA 동작 확인.
- 눈 검증: 'A'(또는 단어)를 큰 크기로 래스터화해 그레이스케일 PPM으로 덤프 → 이미지로 확인.

## 10. 완료 기준 (M2a Definition of Done)

1. 번들 TTF를 `Font::from_bytes`로 파싱 성공.
2. `glyph_index` + `outline` + `advance_width`가 ASCII에 대해 동작.
3. `rasterize_glyph`가 임의 ASCII 글자를 AA 커버리지 비트맵으로 생성.
4. 위 단위 테스트 통과 + 글리프 PPM 덤프가 사람이 보기에 해당 글자로 읽힘.

## 11. 결정 기록

| 질문 | 결정 |
|------|------|
| M2 분할 | M2a(폰트→글리프 비트맵) / M2b(인라인 레이아웃+페인트) |
| 문자 범위 | 라틴/ASCII 먼저 |
| 아웃라인 포맷 | TrueType `glyf` 단순 글리프만 |
| cmap | format 4 (BMP) |
| 래스터화 | 커버리지 스캔라인 (분석적 수평 + 수직 오버샘플 N=5) |
| 폰트 | 번들 오픈 라이선스 TTF 1종 (OFL) |
