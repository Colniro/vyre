//! Web-Engine hinter einer eigenen Schnittstelle. Jetzt Servo; später kommt WebView2 als
//! automatischer Ersatz für Seiten dazu, die Servo noch nicht kann.
//!
//! Die Seite wird in ein eigenes Kind-Fenster gezeichnet. Windows setzt es mit der
//! wgpu-Oberfläche des Hauptfensters zusammen, ohne dass Bilder kopiert werden müssen.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use euclid::Scale;
use servo::{
    Cursor, DevicePoint, EventLoopWaker, InputEvent, LoadStatus, MouseButton, MouseButtonAction,
    MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent, RenderingContext, Servo, ServoBuilder,
    WebView, WebViewBuilder, WebViewDelegate, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
    WindowRenderingContext, ConsoleLogLevel,
};
use url::Url;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, Window, WindowId};

use crate::ui::FpsCounter;
use crate::UserEvent;

/// Was die Oberfläche über die geladene Seite wissen muss.
#[derive(Clone, Debug, Default)]
pub struct PageState {
    pub url: Option<Url>,
    pub title: Option<String>,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
}

/// Eine Seite in einem Tab, unabhängig von der Engine dahinter.
pub trait PageView {
    fn load(&self, url: Url);
    fn reload(&self);
    fn go_back(&self);
    fn go_forward(&self);
    fn state(&self) -> PageState;
    /// Position und Größe des Seitenbereichs im Hauptfenster (physische Pixel).
    fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32);
    fn set_scale(&self, scale: f64);
    fn set_focused(&self, focused: bool);
    /// Fenster, in das die Seite gezeichnet wird.
    fn window_id(&self) -> WindowId;
    /// Ereignisse des eigenen Fensters (Maus, Neuzeichnen, Größe).
    fn handle_window_event(&self, event: &WindowEvent);
    /// Tastatur (kommt über das Hauptfenster, das den Fokus behält).
    fn handle_key(&self, event: &KeyEvent, modifiers: ModifiersState);
    /// Mausrad, wenn Windows es dem Hauptfenster statt dem Seitenfenster schickt.
    fn wheel(&self, delta: MouseScrollDelta, x: f64, y: f64);
    /// Gezeichnete Seitenbilder in der letzten Sekunde.
    fn fps(&self) -> usize;
}

/// Weckt die winit-Ereignisschleife, wenn Servo Arbeit hat (wird aus Servo-Threads gerufen).
#[derive(Clone)]
struct Waker(EventLoopProxy<UserEvent>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let _ = self.0.send_event(UserEvent::ServoWake);
    }
}

/// Nimmt Servos Meldungen entgegen und hält den Seitenzustand für die Oberfläche bereit.
struct Delegate {
    state: RefCell<PageState>,
    child: Arc<Window>,
    main: Arc<Window>,
}

impl Delegate {
    fn changed(&self, webview: &WebView) {
        {
            let mut s = self.state.borrow_mut();
            s.can_back = webview.can_go_back();
            s.can_forward = webview.can_go_forward();
        }
        self.main.request_redraw();
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        self.child.request_redraw();
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        self.state.borrow_mut().url = Some(url);
        self.changed(&webview);
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        self.state.borrow_mut().title = title;
        self.changed(&webview);
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        self.state.borrow_mut().loading = status != LoadStatus::Complete;
        self.changed(&webview);
    }

    fn notify_history_changed(&self, webview: WebView, _entries: Vec<Url>, _current: usize) {
        self.changed(&webview);
    }

    fn notify_cursor_changed(&self, _webview: WebView, cursor: Cursor) {
        match cursor_icon(cursor) {
            Some(icon) => {
                self.child.set_cursor_visible(true);
                self.child.set_cursor(icon);
            }
            None => self.child.set_cursor_visible(false),
        }
    }

    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        // Später wird das an Wyven weitergereicht.
        log::info!(target: "vyre::console", "[{level:?}] {message}");
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, _backtrace: Option<String>) {
        log::error!("Seite abgestürzt: {reason}");
    }
}

/// Servo als Engine: eine Servo-Instanz, ein Seitenfenster mit einer WebView.
pub struct ServoPage {
    // Reihenfolge = Drop-Reihenfolge: zuerst die WebView, dann Servo, dann das Fenster.
    webview: WebView,
    servo: Servo,
    context: Rc<WindowRenderingContext>,
    delegate: Rc<Delegate>,
    window: Arc<Window>,
    pointer: RefCell<DevicePoint>,
    fps: RefCell<FpsCounter>,
}

impl ServoPage {
    pub fn new(
        event_loop: &ActiveEventLoop,
        main: Arc<Window>,
        proxy: EventLoopProxy<UserEvent>,
        bounds: (i32, i32, u32, u32),
        url: Url,
    ) -> Result<Self, String> {
        let (x, y, w, h) = bounds;
        let parent = main.window_handle().map_err(|e| e.to_string())?.as_raw();
        let attrs = Window::default_attributes()
            .with_title("Vyre Seite")
            .with_decorations(false)
            .with_position(PhysicalPosition::new(x, y))
            .with_inner_size(PhysicalSize::new(w.max(1), h.max(1)));
        // SAFETY: Das Hauptfenster lebt länger als das Kind-Fenster (es wird zuletzt freigegeben).
        let attrs = unsafe { attrs.with_parent_window(Some(parent)) };
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);

        let display = event_loop.display_handle().map_err(|e| e.to_string())?;
        let handle = window.window_handle().map_err(|e| e.to_string())?;
        let context = Rc::new(
            WindowRenderingContext::new(display, handle, window.inner_size())
                .map_err(|e| format!("OpenGL-Kontext für Servo: {e:?}"))?,
        );
        let _ = context.make_current();

        let servo = ServoBuilder::default()
            .event_loop_waker(Box::new(Waker(proxy)))
            .build();

        let delegate = Rc::new(Delegate {
            state: RefCell::new(PageState { url: Some(url.clone()), loading: true, ..Default::default() }),
            child: window.clone(),
            main,
        });
        let webview = WebViewBuilder::new(&servo, context.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(delegate.clone())
            .build();

        Ok(Self {
            webview,
            servo,
            context,
            delegate,
            window,
            pointer: RefCell::new(DevicePoint::new(0.0, 0.0)),
            fps: RefCell::new(FpsCounter::default()),
        })
    }

    /// Lässt Servo anstehende Arbeit erledigen (nach jedem Ereignis und jedem Wecken).
    pub fn spin(&self) {
        self.servo.spin_event_loop();
    }

    fn send(&self, event: InputEvent) {
        self.webview.notify_input_event(event);
    }

    fn point(&self) -> WebViewPoint {
        WebViewPoint::Device(*self.pointer.borrow())
    }
}

impl PageView for ServoPage {
    fn load(&self, url: Url) {
        self.webview.load(url);
    }

    fn reload(&self) {
        self.webview.reload();
    }

    fn go_back(&self) {
        if self.webview.can_go_back() {
            self.webview.go_back(1);
        }
    }

    fn go_forward(&self) {
        if self.webview.can_go_forward() {
            self.webview.go_forward(1);
        }
    }

    fn state(&self) -> PageState {
        self.delegate.state.borrow().clone()
    }

    fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32) {
        let size = PhysicalSize::new(width.max(1), height.max(1));
        self.window.set_outer_position(PhysicalPosition::new(x, y));
        if self.window.inner_size() != size {
            let _ = self.window.request_inner_size(size);
            self.webview.resize(size);
        }
    }

    fn set_scale(&self, scale: f64) {
        self.webview.set_hidpi_scale_factor(Scale::new(scale as f32));
    }

    fn set_focused(&self, focused: bool) {
        self.webview.set_focused(focused);
    }

    fn window_id(&self) -> WindowId {
        self.window.id()
    }

    fn handle_window_event(&self, event: &WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                let _ = self.context.make_current();
                self.webview.paint();
                self.context.present();
                self.fps.borrow_mut().tick();
            }
            WindowEvent::Resized(size) => {
                self.webview.resize(*size);
            }
            WindowEvent::CursorMoved { position, .. } => {
                *self.pointer.borrow_mut() = DevicePoint::new(position.x as f32, position.y as f32);
                self.send(InputEvent::MouseMove(MouseMoveEvent::new(self.point())));
            }
            WindowEvent::CursorLeft { .. } => {
                self.send(InputEvent::MouseLeftViewport(MouseLeftViewportEvent::default()));
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Primary,
                    winit::event::MouseButton::Right => MouseButton::Secondary,
                    winit::event::MouseButton::Middle => MouseButton::Auxiliary,
                    winit::event::MouseButton::Back => MouseButton::Back,
                    winit::event::MouseButton::Forward => MouseButton::Forward,
                    winit::event::MouseButton::Other(n) => MouseButton::Other(*n),
                };
                let action = match state {
                    ElementState::Pressed => MouseButtonAction::Down,
                    ElementState::Released => MouseButtonAction::Up,
                };
                self.send(InputEvent::MouseButton(MouseButtonEvent::new(action, button, self.point())));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let p = *self.pointer.borrow();
                self.wheel(*delta, p.x as f64, p.y as f64);
            }
            _ => {}
        }
    }

    fn handle_key(&self, event: &KeyEvent, modifiers: ModifiersState) {
        self.send(InputEvent::Keyboard(crate::keys::to_servo(event, modifiers)));
    }

    fn wheel(&self, delta: MouseScrollDelta, x: f64, y: f64) {
        // Eine Zeile = 76 px wie in servoshell; Servo dreht das Vorzeichen selbst.
        let (dx, dy, mode) = match delta {
            MouseScrollDelta::LineDelta(dx, dy) => ((dx * 76.0) as f64, (dy * 76.0) as f64, WheelMode::DeltaLine),
            MouseScrollDelta::PixelDelta(d) => (d.x, d.y, WheelMode::DeltaPixel),
        };
        let point = WebViewPoint::Device(DevicePoint::new(x as f32, y as f32));
        self.send(InputEvent::Wheel(WheelEvent::new(WheelDelta { x: dx, y: dy, z: 0.0, mode }, point)));
    }

    fn fps(&self) -> usize {
        self.fps.borrow_mut().fps()
    }
}

fn cursor_icon(cursor: Cursor) -> Option<CursorIcon> {
    Some(match cursor {
        Cursor::None => return None,
        Cursor::Default => CursorIcon::Default,
        Cursor::Pointer => CursorIcon::Pointer,
        Cursor::ContextMenu => CursorIcon::ContextMenu,
        Cursor::Help => CursorIcon::Help,
        Cursor::Progress => CursorIcon::Progress,
        Cursor::Wait => CursorIcon::Wait,
        Cursor::Cell => CursorIcon::Cell,
        Cursor::Crosshair => CursorIcon::Crosshair,
        Cursor::Text => CursorIcon::Text,
        Cursor::VerticalText => CursorIcon::VerticalText,
        Cursor::Alias => CursorIcon::Alias,
        Cursor::Copy => CursorIcon::Copy,
        Cursor::Move => CursorIcon::Move,
        Cursor::NoDrop => CursorIcon::NoDrop,
        Cursor::NotAllowed => CursorIcon::NotAllowed,
        Cursor::Grab => CursorIcon::Grab,
        Cursor::Grabbing => CursorIcon::Grabbing,
        Cursor::EResize => CursorIcon::EResize,
        Cursor::NResize => CursorIcon::NResize,
        Cursor::NeResize => CursorIcon::NeResize,
        Cursor::NwResize => CursorIcon::NwResize,
        Cursor::SResize => CursorIcon::SResize,
        Cursor::SeResize => CursorIcon::SeResize,
        Cursor::SwResize => CursorIcon::SwResize,
        Cursor::WResize => CursorIcon::WResize,
        Cursor::EwResize => CursorIcon::EwResize,
        Cursor::NsResize => CursorIcon::NsResize,
        Cursor::NeswResize => CursorIcon::NeswResize,
        Cursor::NwseResize => CursorIcon::NwseResize,
        Cursor::ColResize => CursorIcon::ColResize,
        Cursor::RowResize => CursorIcon::RowResize,
        Cursor::AllScroll => CursorIcon::AllScroll,
        Cursor::ZoomIn => CursorIcon::ZoomIn,
        Cursor::ZoomOut => CursorIcon::ZoomOut,
    })
}
