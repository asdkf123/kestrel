// 측정 하네스: "가볍고 빠른" 목표를 숫자로 추적한다.
// - CountingAllocator: 힙 사용량을 항상 추적하는 자작 global allocator (relaxed 원자연산, 오버헤드 미미)
// - 단계별 타이밍(html/css/style/layout/paint)
// - 합성 페이지(결정론적, 네트워크 불필요)로 재현 가능한 벤치
//
// 실행: cargo run --release -- --bench

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use crate::layout::{Dimensions, ImageMap, Rect};

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

pub struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_ptr = System.realloc(ptr, layout, new_size);
        if !new_ptr.is_null() {
            CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
            let now = CURRENT.fetch_add(new_size, Ordering::Relaxed) + new_size;
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        new_ptr
    }
}

pub fn current_bytes() -> usize {
    CURRENT.load(Ordering::Relaxed)
}

pub fn peak_bytes() -> usize {
    PEAK.load(Ordering::Relaxed)
}

// 이후 측정 구간의 peak 을 현재값으로 리셋한다.
pub fn reset_peak() {
    PEAK.store(CURRENT.load(Ordering::Relaxed), Ordering::Relaxed);
}

pub struct Stats {
    pub min: f64,
    pub median: f64,
    pub mean: f64,
}

// 표본(밀리초 등)의 최소/중앙값/평균. 표본은 비어있지 않다고 가정.
pub fn stats(samples: &[f64]) -> Stats {
    let mut s = samples.to_vec();
    s.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let min = s[0];
    let median = s[s.len() / 2];
    let mean = s.iter().sum::<f64>() / s.len() as f64;
    Stats { min, median, mean }
}

fn fmt_bytes(n: usize) -> String {
    if n >= 1 << 20 {
        format!("{:.2} MB", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1} KB", n as f64 / (1 << 10) as f64)
    } else {
        format!("{} B", n)
    }
}

struct Phases {
    html: Duration,
    css: Duration,
    style: Duration,
    layout: Duration,
    paint: Duration,
}

impl Phases {
    fn total(&self) -> Duration {
        self.html + self.css + self.style + self.layout + self.paint
    }
}

const VW: f32 = 800.0;
const VH: f32 = 600.0;

// 파이프라인 1회 실행 (폰트는 미리 로드해 재사용 — 파일 I/O 는 측정 대상이 아님).
fn render_once(html: &str, fonts: &crate::font::FontStack) -> Phases {
    let t = Instant::now();
    let dom = crate::html::parse(html.to_string());
    let html_t = t.elapsed();

    let t = Instant::now();
    let mut sheet = crate::css::user_agent_stylesheet();
    let mut inline = String::new();
    crate::extract_css(&dom, &mut inline);
    sheet.rules.extend(crate::css::parse(inline).rules);
    let css_t = t.elapsed();

    let t = Instant::now();
    let styled = crate::style::style_tree(&dom, &sheet);
    let style_t = t.elapsed();

    let mut viewport: Dimensions = Default::default();
    viewport.content.width = VW;
    viewport.content.height = VH;
    let imgs = ImageMap::new();
    let t = Instant::now();
    let layout_root = crate::layout::layout_tree(&styled, viewport, fonts, &imgs);
    let layout_t = t.elapsed();

    let mut cache = crate::raster::GlyphCache::new();
    let t = Instant::now();
    let _canvas = crate::paint::paint(
        &layout_root,
        Rect { x: 0.0, y: 0.0, width: VW, height: VH },
        fonts,
        &mut cache,
        &[],
    );
    let paint_t = t.elapsed();

    Phases { html: html_t, css: css_t, style: style_t, layout: layout_t, paint: paint_t }
}

// 합성 텍스트 페이지: n 개 문단 (인라인 레이아웃/래스터/폰트 부하).
fn synth_text(n: usize) -> String {
    let words = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "kestrel", "renders",
        "text", "fast", "light", "browser", "engine", "rust", "layout", "paint", "glyph", "cascade",
    ];
    let mut s = String::from(
        "<html><head><style>body{font-size:16px;color:#222222;background-color:#ffffff;padding:16px}\
         p{margin:0 0 8px;max-width:700px}</style></head><body>",
    );
    for i in 0..n {
        s.push_str("<p>");
        let count = 8 + (i % 12);
        for w in 0..count {
            if w > 0 {
                s.push(' ');
            }
            s.push_str(words[(i * 7 + w * 3) % words.len()]);
        }
        s.push_str("</p>");
    }
    s.push_str("</body></html>");
    s
}

// 합성 박스 페이지: n 개 색 div (선택자 매칭/블록 레이아웃/사각형 채우기 부하).
fn synth_boxes(n: usize) -> String {
    let colors = ["#ff0000", "navy", "rgb(46,160,90)", "#1e2a44", "teal", "olive", "#f4842c", "gray"];
    let mut style = String::from("div{display:block;height:10px;margin:2px 0;max-width:760px}");
    for (i, c) in colors.iter().enumerate() {
        style.push_str(&format!(".b{}{{background-color:{}}}", i, c));
    }
    let mut s = format!("<html><head><style>{}</style></head><body>", style);
    for i in 0..n {
        s.push_str(&format!("<div class=\"b{}\"></div>", i % colors.len()));
    }
    s.push_str("</body></html>");
    s
}

pub fn run_bench() {
    let iters = 50;
    let fonts = crate::load_fonts(false);

    let profile = if cfg!(debug_assertions) { "debug" } else { "release" };
    let bin_size = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::metadata(p).ok())
        .map(|m| m.len() as usize);

    println!("Kestrel 측정 하네스 ({} 빌드, {} iters, 뷰포트 {}x{})", profile, iters, VW as u32, VH as u32);
    if let Some(sz) = bin_size {
        println!("바이너리 크기: {}", fmt_bytes(sz));
    }
    if profile == "debug" {
        println!("주의: debug 빌드 타이밍은 참고용. 실측은 --release 로.");
    }
    println!();

    let cases = [("text-200", synth_text(200)), ("boxes-600", synth_boxes(600))];

    println!(
        "{:<12} {:>9} {:>9} {:>9}   {:>10}",
        "case", "median", "mean", "min", "peak-heap"
    );
    let mut breakdowns: Vec<(String, Phases)> = Vec::new();
    for (name, html) in &cases {
        let mut totals = Vec::with_capacity(iters);
        let mut peak_delta = 0usize;
        let mut last: Option<Phases> = None;
        for i in 0..iters {
            let base = current_bytes();
            reset_peak();
            let ph = render_once(html, &fonts);
            peak_delta = peak_delta.max(peak_bytes().saturating_sub(base));
            totals.push(ph.total().as_secs_f64() * 1000.0);
            if i + 1 == iters {
                last = Some(ph);
            }
        }
        let st = stats(&totals);
        println!(
            "{:<12} {:>7.3}ms {:>7.3}ms {:>7.3}ms   {:>10}",
            name, st.median, st.mean, st.min, fmt_bytes(peak_delta)
        );
        if let Some(ph) = last {
            breakdowns.push((name.to_string(), ph));
        }
    }

    println!("\n단계별 (마지막 실행, 단위 ms):");
    println!("{:<12} {:>8} {:>8} {:>8} {:>8} {:>8}", "case", "html", "css", "style", "layout", "paint");
    for (name, ph) in &breakdowns {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        println!(
            "{:<12} {:>8.3} {:>8.3} {:>8.3} {:>8.3} {:>8.3}",
            name,
            ms(ph.html),
            ms(ph.css),
            ms(ph.style),
            ms(ph.layout),
            ms(ph.paint)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_min_median_mean() {
        let s = stats(&[3.0, 1.0, 2.0, 5.0, 4.0]);
        assert_eq!(s.min, 1.0);
        assert_eq!(s.median, 3.0); // 정렬 [1,2,3,4,5], 인덱스 2
        assert!((s.mean - 3.0).abs() < 1e-9);
    }

    #[test]
    fn allocator_tracks_allocation() {
        let before = current_bytes();
        reset_peak();
        let v: Vec<u8> = vec![7u8; 4 << 20]; // 4 MiB
        let during = current_bytes();
        assert!(
            during as i64 - before as i64 >= 3 << 20,
            "current 이 할당만큼 증가해야 함 (delta={})",
            during as i64 - before as i64
        );
        assert!(peak_bytes() >= during, "peak 은 스파이크 이상이어야 함");
        assert_eq!(v[0], 7); // 최적화로 제거되지 않도록
        drop(v);
    }
}
