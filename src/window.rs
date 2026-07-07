use std::num::NonZeroU32;
use std::rc::Rc;

use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{Key, KeyCode, NamedKey, PhysicalKey};
use winit::window::{CursorIcon, WindowBuilder};

use crate::css::Color;
use crate::layout::{hit_link, Rect};
use crate::paint::DisplayItem;

/// 창에 띄울 페이지: 소유된 디스플레이 리스트 + 리소스. 스크롤 시 재래스터화한다.
pub struct Page {
    pub items: Vec<DisplayItem>,
    pub images: Vec<crate::png::Image>,
    pub fonts: crate::font::FontStack,
    pub doc_height: f32,
    pub links: Vec<(Rect, String)>,
    pub url: crate::url::Url,
}

const LINE_SCROLL: f32 = 48.0;
// 상단 크롬(주소창) 높이. 페이지는 이 아래에 렌더된다.
const CHROME_H: f32 = 36.0;

/// 스크롤 + 링크 클릭 + 주소창이 있는 브라우저 창.
pub fn run_page(
    page: Page,
    width: u32,
    height: u32,
    mut load: impl FnMut(&str) -> Option<Page> + 'static,
) {
    let event_loop = EventLoop::new().unwrap();
    let window = Rc::new(
        WindowBuilder::new()
            .with_title(format!("Kestrel — {}", page.url.as_string()))
            .with_inner_size(LogicalSize::new(width, height))
            .build(&event_loop)
            .unwrap(),
    );

    let context = softbuffer::Context::new(window.clone()).unwrap();
    let mut surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

    let mut page = page;
    let mut cache = crate::raster::GlyphCache::new();
    let mut scroll_y: f32 = 0.0;
    let mut cursor: (f32, f32) = (0.0, 0.0);
    // 뒤로 가기 스택: (URL, 떠날 때 스크롤 위치)
    let mut history: Vec<(String, f32)> = Vec::new();
    // 주소창 상태
    let mut url_input: String = page.url.as_string();
    let mut focused = false;

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);
            // 물리(픽셀)/논리 배율. 레이아웃·스크롤·히트 테스트는 전부 논리 좌표로.
            let scale = window.scale_factor() as f32;
            let viewport_h =
                (window.inner_size().height.max(1) as f32 / scale - CHROME_H).max(1.0);
            let max_scroll = (page.doc_height - viewport_h).max(0.0);
            match event {
                Event::Resumed => {
                    window.request_redraw();
                }
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => elwt.exit(),
                    WindowEvent::ScaleFactorChanged { .. } => {
                        window.request_redraw();
                    }
                    WindowEvent::CursorMoved { position, .. } => {
                        cursor = (position.x as f32 / scale, position.y as f32 / scale);
                        let icon = if cursor.1 < CHROME_H {
                            CursorIcon::Text
                        } else if hit_link(&page.links, cursor.0, cursor.1 - CHROME_H + scroll_y)
                            .is_some()
                        {
                            CursorIcon::Pointer
                        } else {
                            CursorIcon::Default
                        };
                        window.set_cursor_icon(icon);
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    } => {
                        // 주소창 클릭 → 포커스
                        if cursor.1 < CHROME_H {
                            if !focused {
                                focused = true;
                                window.request_redraw();
                            }
                            return;
                        }
                        if focused {
                            focused = false;
                            url_input = page.url.as_string();
                            window.request_redraw();
                        }
                        if let Some(href) =
                            hit_link(&page.links, cursor.0, cursor.1 - CHROME_H + scroll_y)
                        {
                            if href.starts_with('#') {
                                return; // 페이지 내 앵커는 아직 미지원
                            }
                            if let Some(target) = page.url.join(href) {
                                let url_str = target.as_string();
                                println!("→ {}", url_str);
                                if let Some(new_page) = load(&url_str) {
                                    history.push((page.url.as_string(), scroll_y));
                                    page = new_page;
                                    scroll_y = 0.0;
                                    cache = crate::raster::GlyphCache::new(); // 폰트 인덱스가 바뀔 수 있음
                                    url_input = page.url.as_string();
                                    window.set_title(&format!(
                                        "Kestrel — {}",
                                        page.url.as_string()
                                    ));
                                    window.request_redraw();
                                }
                            }
                        }
                    }
                    WindowEvent::MouseWheel { delta, .. } => {
                        let dy = match delta {
                            MouseScrollDelta::LineDelta(_, y) => -y * LINE_SCROLL,
                            MouseScrollDelta::PixelDelta(p) => -p.y as f32 / scale,
                        };
                        let next = (scroll_y + dy).clamp(0.0, max_scroll);
                        if next != scroll_y {
                            scroll_y = next;
                            window.request_redraw();
                        }
                    }
                    WindowEvent::KeyboardInput { event: key, .. }
                        if key.state == ElementState::Pressed =>
                    {
                        // ── 주소창 편집 모드 ──
                        if focused {
                            match &key.logical_key {
                                Key::Named(NamedKey::Enter) => {
                                    let t = url_input.trim().to_string();
                                    let target = if t.starts_with("http://")
                                        || t.starts_with("https://")
                                    {
                                        t
                                    } else {
                                        format!("https://{}", t)
                                    };
                                    println!("→ {}", target);
                                    focused = false;
                                    if let Some(new_page) = load(&target) {
                                        history.push((page.url.as_string(), scroll_y));
                                        page = new_page;
                                        scroll_y = 0.0;
                                        cache = crate::raster::GlyphCache::new();
                                        url_input = page.url.as_string();
                                        window.set_title(&format!(
                                            "Kestrel — {}",
                                            page.url.as_string()
                                        ));
                                    } else {
                                        url_input = page.url.as_string();
                                    }
                                    window.request_redraw();
                                }
                                Key::Named(NamedKey::Escape) => {
                                    focused = false;
                                    url_input = page.url.as_string();
                                    window.request_redraw();
                                }
                                Key::Named(NamedKey::Backspace) => {
                                    url_input.pop();
                                    window.request_redraw();
                                }
                                Key::Character(s) => {
                                    url_input.push_str(s);
                                    window.request_redraw();
                                }
                                _ => {}
                            }
                            return;
                        }
                        // ── 뒤로 가기: Backspace (스크롤 위치까지 복원) ──
                        if key.physical_key == PhysicalKey::Code(KeyCode::Backspace) {
                            if let Some((prev_url, prev_scroll)) = history.pop() {
                                println!("← {}", prev_url);
                                if let Some(new_page) = load(&prev_url) {
                                    page = new_page;
                                    scroll_y = prev_scroll
                                        .clamp(0.0, (page.doc_height - viewport_h).max(0.0));
                                    cache = crate::raster::GlyphCache::new();
                                    url_input = page.url.as_string();
                                    window.set_title(&format!(
                                        "Kestrel — {}",
                                        page.url.as_string()
                                    ));
                                    window.request_redraw();
                                } else {
                                    history.push((prev_url, prev_scroll)); // 실패 시 스택 보존
                                }
                            }
                            return;
                        }
                        // ── 스크롤 키 ──
                        let dy = match key.physical_key {
                            PhysicalKey::Code(KeyCode::ArrowDown) => Some(LINE_SCROLL),
                            PhysicalKey::Code(KeyCode::ArrowUp) => Some(-LINE_SCROLL),
                            PhysicalKey::Code(KeyCode::PageDown)
                            | PhysicalKey::Code(KeyCode::Space) => Some(viewport_h * 0.9),
                            PhysicalKey::Code(KeyCode::PageUp) => Some(-viewport_h * 0.9),
                            PhysicalKey::Code(KeyCode::Home) => Some(-scroll_y),
                            PhysicalKey::Code(KeyCode::End) => Some(max_scroll - scroll_y),
                            _ => None,
                        };
                        if let Some(dy) = dy {
                            let next = (scroll_y + dy).clamp(0.0, max_scroll);
                            if next != scroll_y {
                                scroll_y = next;
                                window.request_redraw();
                            }
                        }
                    }
                    WindowEvent::Resized(_) => {
                        scroll_y = scroll_y.clamp(0.0, max_scroll);
                        window.request_redraw();
                    }
                    WindowEvent::RedrawRequested => {
                        let size = window.inner_size();
                        let (w, h) = (size.width.max(1), size.height.max(1));
                        surface
                            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                            .unwrap();

                        // 페이지: 크롬 아래부터 그린다 (scroll 을 CHROME_H 만큼 당겨서)
                        let mut canvas = crate::paint::rasterize(
                            &page.items,
                            w as usize,
                            h as usize,
                            scroll_y - CHROME_H,
                            scale,
                            &page.fonts,
                            &mut cache,
                            &page.images,
                        );
                        // 크롬 (주소창) — 물리 좌표로 직접 그림
                        let s = scale;
                        let wf = w as f32;
                        canvas.fill_rect(
                            Color { r: 32, g: 32, b: 38, a: 255 },
                            Rect { x: 0.0, y: 0.0, width: wf, height: CHROME_H * s },
                        );
                        let field_bg = if focused {
                            Color { r: 14, g: 14, b: 20, a: 255 }
                        } else {
                            Color { r: 22, g: 22, b: 28, a: 255 }
                        };
                        canvas.fill_rect(
                            field_bg,
                            Rect {
                                x: 8.0 * s,
                                y: 6.0 * s,
                                width: wf - 16.0 * s,
                                height: (CHROME_H - 12.0) * s,
                            },
                        );
                        let end_x = crate::paint::draw_text(
                            &mut canvas,
                            &page.fonts,
                            &mut cache,
                            &url_input,
                            16.0 * s,
                            24.0 * s,
                            14.0 * s,
                            Color { r: 214, g: 218, b: 228, a: 255 },
                        );
                        if focused {
                            canvas.fill_rect(
                                Color { r: 244, g: 132, b: 44, a: 255 },
                                Rect {
                                    x: end_x + 2.0 * s,
                                    y: 10.0 * s,
                                    width: 2.0 * s,
                                    height: (CHROME_H - 20.0) * s,
                                },
                            );
                        }

                        let buffer = canvas.to_u32_buffer();
                        let mut frame = surface.buffer_mut().unwrap();
                        frame.copy_from_slice(&buffer);
                        frame.present().unwrap();
                    }
                    _ => {}
                },
                _ => {}
            }
        })
        .unwrap();
}
