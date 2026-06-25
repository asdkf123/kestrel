use std::num::NonZeroU32;
use std::rc::Rc;

use winit::dpi::LogicalSize;
use winit::event::{Event, WindowEvent};
use winit::event_loop::{ControlFlow, EventLoop};
use winit::window::WindowBuilder;

/// 미리 렌더된 픽셀 버퍼(0x00RRGGBB, width*height)를 창에 표시한다.
pub fn run(buffer: Vec<u32>, width: u32, height: u32) {
    let event_loop = EventLoop::new().unwrap();
    let window = Rc::new(
        WindowBuilder::new()
            .with_title("Kestrel")
            .with_inner_size(LogicalSize::new(width, height))
            .build(&event_loop)
            .unwrap(),
    );

    let context = softbuffer::Context::new(window.clone()).unwrap();
    let mut surface = softbuffer::Surface::new(&context, window.clone()).unwrap();

    event_loop
        .run(move |event, elwt| {
            elwt.set_control_flow(ControlFlow::Wait);
            match event {
                Event::Resumed => {
                    window.request_redraw();
                }
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => elwt.exit(),
                    WindowEvent::RedrawRequested => {
                        let size = window.inner_size();
                        let (w, h) = (size.width.max(1), size.height.max(1));
                        surface
                            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
                            .unwrap();

                        let mut frame = surface.buffer_mut().unwrap();
                        for y in 0..h as usize {
                            for x in 0..w as usize {
                                let dst = y * w as usize + x;
                                let val = if x < width as usize && y < height as usize {
                                    buffer[y * width as usize + x]
                                } else {
                                    0x00_1e_1e_1e // 캔버스 밖은 어두운 회색
                                };
                                frame[dst] = val;
                            }
                        }
                        frame.present().unwrap();
                    }
                    _ => {}
                },
                _ => {}
            }
        })
        .unwrap();
}
