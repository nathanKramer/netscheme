use std::{num::NonZeroU32, path::PathBuf, rc::Rc};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use crate::{dom::Element, layout::Link, lisp::Value, paint::Renderer};

pub fn run(path: PathBuf, page: Rc<Element>, renderer: Renderer) {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App {
        path,
        page,
        history: Vec::new(),
        renderer,
        links: Vec::new(),
        cursor: (0.0, 0.0),
        modifiers: ModifiersState::empty(),
        state: None,
    };
    event_loop.run_app(&mut app).expect("run event loop");
}

struct App {
    path: PathBuf,
    page: Rc<Element>,
    /// Pages to return to with "back", most recent last.
    history: Vec<PathBuf>,
    renderer: Renderer,
    /// Link hit boxes from the last render.
    links: Vec<Link>,
    cursor: (f32, f32),
    modifiers: ModifiersState,
    state: Option<WindowState>,
}

struct WindowState {
    window: Rc<Window>,
    _context: softbuffer::Context<Rc<Window>>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

impl App {
    fn title(&self) -> String {
        match self.page.attr("title") {
            Some(Value::Str(t)) => format!("{t} — Toy Browser"),
            _ => "Toy Browser".into(),
        }
    }

    fn link_under_cursor(&self) -> Option<&Link> {
        self.links.iter().find(|link| link.contains(self.cursor.0, self.cursor.1))
    }

    fn follow(&mut self, href: &str) {
        let page = match crate::resolve(&self.path, href) {
            Ok(path) => {
                let page = crate::open(&path);
                self.history.push(std::mem::replace(&mut self.path, path));
                page
            }
            Err(e) => crate::error_page(&e),
        };
        self.show(page);
    }

    fn back(&mut self) {
        if let Some(path) = self.history.pop() {
            let page = crate::open(&path);
            self.path = path;
            self.show(page);
        }
    }

    fn show(&mut self, page: Rc<Element>) {
        self.page = page;
        self.links.clear();
        if let Some(state) = &self.state {
            state.window.set_title(&self.title());
            state.window.set_cursor(CursorIcon::Default);
            state.window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        let attrs = Window::default_attributes().with_title(self.title()).with_inner_size(LogicalSize::new(900.0, 700.0));
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
                self.links = self.renderer.render(&self.page, &mut buffer, size.width as usize, size.height as usize, scale);
                buffer.present().expect("present buffer");
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                let over_link = self.links.iter().any(|link| link.contains(self.cursor.0, self.cursor.1));
                state.window.set_cursor(if over_link { CursorIcon::Pointer } else { CursorIcon::Default });
            }
            WindowEvent::MouseInput { state: ElementState::Released, button, .. } => match button {
                MouseButton::Left => {
                    if let Some(href) = self.link_under_cursor().map(|link| link.href.clone()) {
                        self.follow(&href);
                    }
                }
                MouseButton::Back => self.back(),
                _ => {}
            },
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. }
                if event.state == ElementState::Pressed
                    && self.modifiers.alt_key()
                    && event.logical_key == Key::Named(NamedKey::ArrowLeft) =>
            {
                self.back()
            }
            _ => {}
        }
    }
}
