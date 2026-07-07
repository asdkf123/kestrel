use std::num::NonZeroU32;
use std::rc::Rc;

use winit::dpi::LogicalSize;
use winit::event::{ElementState, Event, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{CursorIcon, WindowBuilder};

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

/// 스크롤 + 링크 클릭이 되는 페이지 창.
/// 클릭한 링크는 현재 페이지 URL 기준으로 해석해 `load` 로 새 페이지를 받아 교체한다.
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

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);
            let viewport_h = window.inner_size().height.max(1) as f32;
            let max_scroll = (page.doc_height - viewport_h).max(0.0);
            match event {
                Event::Resumed => {
                    window.request_redraw();
                }
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => elwt.exit(),
                    WindowEvent::CursorMoved { position, .. } => {
                        cursor = (position.x as f32, position.y as f32);
                        let over =
                            hit_link(&page.links, cursor.0, cursor.1 + scroll_y).is_some();
                        window.set_cursor_icon(if over {
                            CursorIcon::Pointer
                        } else {
                            CursorIcon::Default
                        });
                    }
                    WindowEvent::MouseInput {
                        state: ElementState::Pressed,
                        button: MouseButton::Left,
                        ..
                    } => {
                        if let Some(href) =
                            hit_link(&page.links, cursor.0, cursor.1 + scroll_y)
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
                            MouseScrollDelta::PixelDelta(p) => -p.y as f32,
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
                        // 뒤로 가기: Backspace (스크롤 위치까지 복원)
                        if key.physical_key == PhysicalKey::Code(KeyCode::Backspace) {
                            if let Some((prev_url, prev_scroll)) = history.pop() {
                                println!("← {}", prev_url);
                                if let Some(new_page) = load(&prev_url) {
                                    page = new_page;
                                    let vh = window.inner_size().height.max(1) as f32;
                                    scroll_y =
                                        prev_scroll.clamp(0.0, (page.doc_height - vh).max(0.0));
                                    cache = crate::raster::GlyphCache::new();
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

                        let canvas = crate::paint::rasterize(
                            &page.items,
                            w as usize,
                            h as usize,
                            scroll_y,
                            &page.fonts,
                            &mut cache,
                            &page.images,
                        );
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
