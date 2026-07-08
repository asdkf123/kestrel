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

/// 페이지: 원본(DOM/스타일시트/JS 런타임)을 소유하고, rebuild() 로 렌더 산출물을
/// 재생성한다. 이벤트 핸들러가 DOM 을 바꾸면 rebuild 로 화면이 갱신된다.
/// 스타일/레이아웃 트리는 rebuild 안에서만 사는 일시 산물 (borrow 격리).
pub struct Page {
    pub dom: crate::dom::Dom,
    pub sheet: crate::css::Stylesheet,
    pub images: Vec<crate::png::Image>,
    pub img_map: crate::layout::ImageMap,
    pub fonts: crate::font::FontStack,
    pub js: crate::js::interp::Interp,
    pub url: crate::url::Url,
    pub viewport_width: f32,
    // ── rebuild() 산출물 ──
    pub items: Vec<DisplayItem>,
    pub links: Vec<(Rect, String)>,
    pub element_rects: Vec<(Rect, crate::dom::NodeId, usize)>,
    pub doc_height: f32,
}

impl Page {
    pub fn rebuild(&mut self) {
        let style_root = crate::style::style_tree(&self.dom, &self.sheet);
        let mut viewport: crate::layout::Dimensions = Default::default();
        viewport.content.width = self.viewport_width;
        let layout_root =
            crate::layout::layout_tree(&style_root, viewport, &self.fonts, &self.img_map);
        self.items = crate::paint::build_display_list(&layout_root);
        self.links.clear();
        crate::layout::collect_link_regions(&layout_root, &mut self.links);
        self.element_rects.clear();
        crate::layout::collect_element_rects(&layout_root, 0, &mut self.element_rects);
        self.doc_height = layout_root.dimensions.margin_box().height;
    }

    // (x, y): 문서 좌표. 클릭 지점의 가장 깊은 요소를 타깃으로 핸들러를 버블링
    // 실행하고, 하나라도 실행됐으면 rebuild 후 true.
    pub fn dispatch_click(&mut self, x: f32, y: f32) -> bool {
        let Some(target) = crate::layout::hit_element(&self.element_rects, x, y) else {
            return false;
        };
        self.js.dom = Some(&mut self.dom as *mut crate::dom::Dom);
        let mut fired = self.js.fire_handlers(target, "click");
        // onclick 속성: 타깃부터 조상 순서로 평가
        let mut chain = vec![target];
        chain.extend(self.dom.ancestors(target));
        for id in chain {
            let src = match &self.dom.get(id).node_type {
                crate::dom::NodeType::Element(e) => e.attributes.get("onclick").cloned(),
                _ => None,
            };
            if let Some(src) = src {
                fired = true;
                self.js.run_inline_handler(&src);
            }
        }
        for line in self.js.console.drain(..) {
            println!("[console] {}", line);
        }
        self.js.dom = None;
        if fired {
            self.rebuild();
        }
        fired
    }
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
                        // 이벤트 핸들러 먼저 (실행되면 rebuild 됨), 링크 기본 동작은 그 다음
                        if page.dispatch_click(cursor.0, cursor.1 - CHROME_H + scroll_y) {
                            scroll_y = scroll_y.clamp(0.0, (page.doc_height - viewport_h).max(0.0));
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dom::{Dom, NodeType};

    fn make_page(html: &str) -> Page {
        let mut dom = crate::html::parse_dom(html.to_string());
        let js = crate::js::run_scripts(&mut dom);
        let sheet = crate::css::user_agent_stylesheet();
        let f = crate::font::Font::from_bytes(std::fs::read("assets/fonts/Latin.ttf").unwrap())
            .unwrap();
        let fonts = crate::font::FontStack::new(vec![f]);
        let mut page = Page {
            dom,
            sheet,
            images: Vec::new(),
            img_map: crate::layout::ImageMap::new(),
            fonts,
            js,
            url: crate::url::Url::parse("https://localhost/").unwrap(),
            viewport_width: 400.0,
            items: Vec::new(),
            links: Vec::new(),
            element_rects: Vec::new(),
            doc_height: 0.0,
        };
        page.rebuild();
        page
    }

    fn text_of_id(dom: &Dom, id: &str) -> Option<String> {
        dom.find_by_attr_id(id).map(|n| dom.text_content(n))
    }

    // 태그 이름으로 요소 히트 영역 중심점 찾기
    fn center_of_tag(page: &Page, tag: &str) -> (f32, f32) {
        for (r, id, _) in &page.element_rects {
            if let NodeType::Element(e) = &page.dom.get(*id).node_type {
                if e.tag_name == tag {
                    return (r.x + r.width / 2.0, r.y + r.height / 2.0);
                }
            }
        }
        panic!("{} 요소를 찾지 못함", tag);
    }

    #[test]
    fn click_fires_add_event_listener_and_rerenders() {
        let mut page = make_page(
            "<p id=\"out\">count 0</p><button>inc</button>\
             <script>var n = 0; \
             document.getElementById('out').textContent = 'count 0'; \
             var b = document.getElementById('out'); \
             </script>",
        );
        // 핸들러를 스크립트로 등록하는 완전한 흐름은 아래 카운터 테스트에서;
        // 여기선 등록 없는 클릭이 false 를 반환하는지부터
        let (x, y) = center_of_tag(&page, "button");
        assert!(!page.dispatch_click(x, y), "핸들러 없으면 false");
    }

    #[test]
    fn counter_button_increments_on_clicks() {
        let mut page = make_page(
            "<p id=\"out\">count 0</p><button id=\"b\">inc</button>\
             <script>var n = 0; \
             document.getElementById('b').addEventListener('click', function() { \
               n++; document.getElementById('out').textContent = 'count ' + n; \
             });</script>",
        );
        let (x, y) = center_of_tag(&page, "button");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "count 1");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "count 2", "클로저 상태 유지");
        assert!(!page.items.is_empty(), "rebuild 후 디스플레이 리스트 존재");
    }

    #[test]
    fn onclick_property_and_attribute_fire() {
        // el.onclick = fn
        let mut page = make_page(
            "<p id=\"out\">no</p><button id=\"b\">go</button>\
             <script>document.getElementById('b').onclick = function() { \
               document.getElementById('out').textContent = 'via property'; \
             };</script>",
        );
        let (x, y) = center_of_tag(&page, "button");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "out").unwrap(), "via property");

        // onclick="..." 속성
        let mut page2 = make_page(
            "<p id=\"out\">no</p>\
             <button onclick=\"document.getElementById('out').textContent = 'via attr'\">go</button>",
        );
        let (x2, y2) = center_of_tag(&page2, "button");
        assert!(page2.dispatch_click(x2, y2));
        assert_eq!(text_of_id(&page2.dom, "out").unwrap(), "via attr");
    }

    #[test]
    fn click_appends_list_items_and_hit_regions_grow() {
        let mut page = make_page(
            "<ul id=\"list\"></ul><button id=\"add\">add</button>\
             <script>var n = 0; \
             document.getElementById('add').addEventListener('click', function() { \
               n++; \
               var li = document.createElement('li'); \
               li.textContent = 'row ' + n; \
               document.getElementById('list').appendChild(li); \
             });</script>",
        );
        let before = page.element_rects.len();
        // 리스트가 자라면 버튼이 아래로 밀리므로 매 클릭마다 좌표를 다시 잡는다
        let (x, y) = center_of_tag(&page, "button");
        assert!(page.dispatch_click(x, y));
        let (x2, y2) = center_of_tag(&page, "button");
        assert!(y2 > y, "리스트가 자라서 버튼이 아래로 이동");
        assert!(page.dispatch_click(x2, y2));
        let list = page.dom.find_by_attr_id("list").unwrap();
        assert_eq!(page.dom.get(list).children.len(), 2);
        assert_eq!(page.dom.text_content(list), "row 1row 2");
        assert!(
            page.element_rects.len() >= before + 2,
            "rebuild 후 새 li 들이 히트 영역에 반영"
        );
    }

    #[test]
    fn click_bubbles_to_ancestor_handler() {
        let mut page = make_page(
            "<div id=\"wrap\"><p id=\"inner\">child text</p></div>\
             <script>document.getElementById('wrap').addEventListener('click', function() { \
               document.getElementById('inner').textContent = 'bubbled'; \
             });</script>",
        );
        // 안쪽 p 를 클릭해도 조상 div 핸들러가 실행 (버블링)
        let (x, y) = center_of_tag(&page, "p");
        assert!(page.dispatch_click(x, y));
        assert_eq!(text_of_id(&page.dom, "inner").unwrap(), "bubbled");
    }
}
