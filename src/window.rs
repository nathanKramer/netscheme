use std::{num::NonZeroU32, rc::Rc};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

use crate::{dom::Element, lisp::Value, paint::Renderer};

pub fn run(page: Rc<Element>, renderer: Renderer) {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App { page, renderer, state: None };
    event_loop.run_app(&mut app).expect("run event loop");
}

struct App {
    page: Rc<Element>,
    renderer: Renderer,
    state: Option<WindowState>,
}

struct WindowState {
    window: Rc<Window>,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let title = match self.page.attr("title") {
            Some(Value::Str(t)) => format!("{t} — Toy Browser"),
            _ => "Toy Browser".into(),
        };
        let attrs = Window::default_attributes().with_title(title).with_inner_size(LogicalSize::new(900.0, 700.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("softbuffer surface");
        self.state = Some(WindowState { window, _context: context, surface });
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(state) = &mut self.state else { return };
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => state.window.request_redraw(),
            WindowEvent::RedrawRequested => {
                let size = state.window.inner_size();
                let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
                    return;
                };
                state.surface.resize(w, h).expect("resize surface");
                let mut buffer = state.surface.buffer_mut().expect("surface buffer");
                let scale = state.window.scale_factor() as f32;
                self.renderer.render(&self.page, &mut buffer, size.width as usize, size.height as usize, scale);
                buffer.present().expect("present buffer");
            }
            _ => {}
        }
    }
}
