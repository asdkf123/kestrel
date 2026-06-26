# Kestrel — 이미지(PNG) 렌더링: 설계 문서

- 날짜: 2026-06-26
- 상태: 승인됨

## 1. 목표

`<img src="...">`를 가져와 디코드해 화면에 그린다. **PNG 먼저**, JPEG는 후속.

## 2. 분할

- **A. inflate (DEFLATE/zlib)** — `inflate.rs`. RFC 1951 DEFLATE + RFC 1950 zlib 래퍼 압축 해제. PNG의 IDAT가 zlib 스트림. 알려진 입력→출력으로 독립 단위 테스트.
- **B. PNG 디코더** — `png.rs`. 청크 파싱(IHDR/IDAT/IEND/PLTE/tRNS) + zlib 해제 + 스캔라인 언필터(None/Sub/Up/Average/Paeth) + 색상타입(0 grayscale, 2 RGB, 3 palette, 6 RGBA) → RGBA8 픽셀. `decode(bytes) -> Image { width, height, rgba: Vec<u8> }`.
- **C. img 통합** — `<img src>` URL을 페이지 기준으로 해석 → fetch → png::decode → 크기 가진 대체 요소 박스로 레이아웃 → 캔버스에 알파 블렌딩으로 픽셀 블릿.

## 3. A — inflate

- LSB-우선 비트 리더. 블록 루프: BFINAL/BTYPE.
  - 00 stored, 01 고정 허프만, 10 동적 허프만.
- 정규(canonical) 허프만 디코더(puff.c 방식): 코드길이 배열 → counts/symbols → 비트별 디코드.
- 리터럴/길이(257~285, extra bits) + 거리(0~29, extra bits), LZ77 슬라이딩 윈도우 복사.
- `pub fn zlib_decompress(&[u8]) -> Option<Vec<u8>>`, `pub fn inflate(&[u8]) -> Option<Vec<u8>>`.

## 4. B — PNG

- 시그니처(8바이트) 확인 → 청크 순회(length+type+data+crc; crc는 검증 생략 가능).
- IHDR: width/height/bitDepth/colorType. (비트뎁스 8만; 16/1/2/4는 비범위)
- IDAT 이어붙여 zlib 해제 → 필터된 스캔라인.
- 언필터: 행마다 filter byte + 픽셀들. Sub/Up/Average/Paeth.
- 색상타입 → RGBA8 변환. 팔레트(3)는 PLTE + (옵션)tRNS.
- 인터레이스(Adam7)는 비범위(인터레이스 PNG는 거부/스킵).

## 5. C — img 통합

- DOM에서 `<img>` 요소(void) — `src` 속성. 레이아웃에서 대체 요소로: 박스 크기 = `width`/`height` 속성 또는 이미지 고유 크기.
- `LayoutBox`에 이미지 핸들(디코드된 픽셀 참조 또는 인덱스) 보관. paint에서 블릿.
- 이미지 fetch는 페이지 렌더 시(render_url) `<img src>`를 모아 받아 디코드, 박스에 연결.
- 단순화: 인라인 흐름 안의 이미지 정밀 배치 대신, 우선 블록/단순 배치.

## 6. 범위 / 비범위

**범위**: PNG(8비트, 색상타입 0/2/3/6, 비인터레이스), DEFLATE 직접, `<img>` 블록 배치 + 블릿.

**비범위**: JPEG/GIF/WebP, 16비트/저비트뎁스 PNG, Adam7 인터레이스, 이미지 리사이즈/스케일, CSS background-image, lazy loading.

## 7. 테스트 / 검증

- A: python으로 만든 zlib 벡터로 `zlib_decompress` 검증(알려진 출력 일치). 길이/거리 백레퍼런스 포함 케이스.
- B: 작은 알려진 PNG 디코드 → width/height + 특정 픽셀 색 확인(헤드리스).
- C: `<img>` 든 페이지/로컬 예제를 렌더 → 이미지가 보이는지(헤드리스).

## 8. 완료 기준

1. inflate가 zlib 스트림을 정확히 푼다.
2. PNG가 RGBA로 디코드된다(픽셀 검증).
3. `<img>`가 페이지에 실제 이미지로 렌더된다.
4. 기존 테스트 통과.
