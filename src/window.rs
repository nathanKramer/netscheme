use std::{num::NonZeroU32, rc::Rc};

use winit::{
    application::ApplicationHandler,
    dpi::LogicalSize,
    event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent},
    event_loop::{ActiveEventLoop, EventLoop},
    keyboard::{Key, ModifiersState, NamedKey},
    window::{CursorIcon, Window, WindowId},
};

use crate::{dom::Element, layout::Link, lisp::Value, loader, paint::Renderer};

/// Logical pixels scrolled per arrow key press or mouse wheel line.
const LINE_HEIGHT: f32 = 40.0;

pub fn run(location: String, page: Rc<Element>, renderer: Renderer) {
    let event_loop = EventLoop::new().expect("create event loop");
    let mut app = App {
        location,
        page,
        history: Vec::new(),
        scroll: 0.0,
        renderer,
        links: Vec::new(),
        cursor: (0.0, 0.0),
        modifiers: ModifiersState::empty(),
        state: None,
    };
    event_loop.run_app(&mut app).expect("run event loop");
}

struct App {
    location: String,
    page: Rc<Element>,
    /// Pages to return to with "back" and their scroll offsets, most recent last.
    history: Vec<(String, f32)>,
    /// How far down the page is scrolled, in physical pixels.
    scroll: f32,
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

    /// Navigates the whole window, even when the link is inside a frame.
    fn follow(&mut self, href: &str, base: Option<&str>) {
        let page = match loader::resolve(base.unwrap_or(&self.location), href) {
            Ok(location) => {
                let page = loader::open(&location);
                self.history.push((std::mem::replace(&mut self.location, location), self.scroll));
                page
            }
            Err(e) => loader::error_page(&e),
        };
        self.show(page, 0.0);
    }

    fn back(&mut self) {
        if let Some((location, scroll)) = self.history.pop() {
            let page = loader::open(&location);
            self.location = location;
            self.show(page, scroll);
        }
    }

    /// Scrolls by `dy` physical pixels; positive is down. The renderer clamps it.
    fn scroll_by(&mut self, dy: f32) {
        self.scroll += dy;
        if let Some(state) = &self.state {
            state.window.request_redraw();
        }
    }

    fn update_cursor(&self) {
        if let Some(state) = &self.state {
            let icon = if self.link_under_cursor().is_some() { CursorIcon::Pointer } else { CursorIcon::Default };
            state.window.set_cursor(icon);
        }
    }

    /// Scrolling for arrow keys, Page Up/Down, Space, and Home/End.
    fn key_scroll(&self, key: &Key) -> Option<f32> {
        let window = &self.state.as_ref()?.window;
        let (scale, height) = (window.scale_factor() as f32, window.inner_size().height as f32);
        let page = (height - 40.0 * scale).max(height / 2.0);
        Some(match key {
            Key::Named(NamedKey::ArrowDown) => LINE_HEIGHT * scale,
            Key::Named(NamedKey::ArrowUp) => -LINE_HEIGHT * scale,
            Key::Named(NamedKey::PageDown | NamedKey::Space) if self.modifiers.shift_key() => -page,
            Key::Named(NamedKey::PageDown | NamedKey::Space) => page,
            Key::Named(NamedKey::PageUp) => -page,
            Key::Named(NamedKey::Home) => -f32::INFINITY,
            Key::Named(NamedKey::End) => f32::INFINITY,
            _ => return None,
        })
    }

    fn show(&mut self, page: Rc<Element>, scroll: f32) {
        self.page = page;
        self.scroll = scroll;
        self.links.clear();
        if let Some(state) = &self.state {
            state.window.set_title(&self.title());
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
                let (w, h) = (size.width as usize, size.height as usize);
                let rendered = self.renderer.render(&self.page, &mut buffer, w, h, scale, self.scroll);
                buffer.present().expect("present buffer");
                (self.links, self.scroll) = (rendered.links, rendered.scroll);
                // Scrolling moves links under a still cursor.
                self.update_cursor();
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                self.update_cursor();
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let dy = match delta {
                    // Mouse wheels move in notches.
                    MouseScrollDelta::LineDelta(_, lines) => -lines * 3.0 * LINE_HEIGHT * state.window.scale_factor() as f32,
                    // Trackpads report exact physical pixels.
                    MouseScrollDelta::PixelDelta(position) => -position.y as f32,
                };
                self.scroll_by(dy);
            }
            WindowEvent::MouseInput { state: ElementState::Released, button, .. } => match button {
                MouseButton::Left => {
                    if let Some((href, base)) = self.link_under_cursor().map(|link| (link.href.clone(), link.base.clone())) {
                        self.follow(&href, base.as_deref());
                    }
                }
                MouseButton::Back => self.back(),
                _ => {}
            },
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if self.modifiers.alt_key() && event.logical_key == Key::Named(NamedKey::ArrowLeft) {
                    self.back();
                } else if let Some(dy) = self.key_scroll(&event.logical_key) {
                    self.scroll_by(dy);
                }
            }
            _ => {}
        }
    }
}
