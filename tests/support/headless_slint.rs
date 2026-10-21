use i_slint_renderer_femtovg::{
    opengl::{OpenGLBackend, OpenGLInterface},
    FemtoVGRenderer,
};
use memento_app::MementoApp;
use slint::{
    platform::{Platform, PlatformError, Renderer, WindowAdapter, WindowEvent},
    ComponentHandle,
};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    time::Instant,
};

pub struct HeadlessWindow {
    window: slint::Window,
    size: Cell<slint::PhysicalSize>,
    pub renderer: FemtoVGRenderer<OpenGLBackend>,
    pub redraw_requested: Cell<bool>,
}

impl WindowAdapter for HeadlessWindow {
    fn window(&self) -> &slint::Window {
        &self.window
    }
    fn size(&self) -> slint::PhysicalSize {
        self.size.get()
    }
    fn set_size(&self, size: slint::WindowSize) {
        self.size.set(size.to_physical(1.0));
        self.window.dispatch_event(WindowEvent::Resized {
            size: size.to_logical(1.0),
        });
    }
    fn renderer(&self) -> &dyn Renderer {
        &self.renderer
    }
    fn request_redraw(&self) {
        self.redraw_requested.set(true);
    }
}

struct HeadlessPlatform {
    renderer: RefCell<Option<FemtoVGRenderer<OpenGLBackend>>>,
    window: Rc<RefCell<Weak<HeadlessWindow>>>,
    started: Instant,
}

impl Platform for HeadlessPlatform {
    fn create_window_adapter(&self) -> Result<Rc<dyn WindowAdapter>, PlatformError> {
        let renderer = self.renderer.borrow_mut().take().expect("one test window");
        let window = Rc::new_cyclic(|weak: &Weak<HeadlessWindow>| HeadlessWindow {
            window: slint::Window::new(weak.clone()),
            renderer,
            size: Cell::new(slint::PhysicalSize::new(1280, 720)),
            redraw_requested: Cell::new(true),
        });
        *self.window.borrow_mut() = Rc::downgrade(&window);
        Ok(window)
    }
    fn duration_since_start(&self) -> std::time::Duration {
        self.started.elapsed()
    }
}

pub fn create_window(gl: impl OpenGLInterface + 'static) -> (MementoApp, Rc<HeadlessWindow>) {
    let window = Rc::new(RefCell::new(Weak::new()));
    slint::platform::set_platform(Box::new(HeadlessPlatform {
        renderer: RefCell::new(Some(FemtoVGRenderer::new(gl).unwrap())),
        window: window.clone(),
        started: Instant::now(),
    }))
    .unwrap();
    let app = MementoApp::new().unwrap();
    app.window().set_size(slint::PhysicalSize::new(1280, 720));
    app.show().unwrap();
    let adapter = window.borrow().upgrade().unwrap();
    (app, adapter)
}
