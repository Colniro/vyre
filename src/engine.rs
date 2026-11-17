//! Web-Engine hinter einer eigenen Schnittstelle. Jetzt Servo; später kommt WebView2 als
//! automatischer Ersatz für Seiten dazu, die Servo noch nicht kann.
//!
//! Alle Tabs teilen sich ein Kind-Fenster mit einem OpenGL-Kontext. Servo zeigt darin nur die
//! WebView des aktiven Tabs. Windows setzt das Kind-Fenster mit der wgpu-Oberfläche des
//! Hauptfensters zusammen, ohne dass Bilder kopiert werden müssen.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use euclid::Scale;
use servo::{
    ConsoleLogLevel, CreateNewWebViewRequest, Cursor, DevicePoint, EventLoopWaker, InputEvent,
    JSValue, LoadStatus, WebResourceLoad, WebResourceResponse, MouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent,
    MouseMoveEvent, Opts, RenderingContext, Servo, ServoBuilder, WebView, WebViewBuilder,
    WebViewDelegate, WebViewId, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
    WindowRenderingContext,
};
use servo::protocol_handler::ProtocolRegistry;
use url::Url;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::ModifiersState;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, Window, WindowId};

use crate::UserEvent;
use crate::blocker::{self, Blocker};
use crate::ui::FpsCounter;

/// Fügt CSS als `<style>` in die Seite ein.
fn inject_css(webview: &WebView, css: &str) {
    if css.is_empty() {
        return;
    }
    let script = format!(
        "(function(){{var s=document.createElement('style');s.className='vyre-adblock';\
         s.textContent={};(document.head||document.documentElement).appendChild(s);}})()",
        js_string(css)
    );
    webview.evaluate_javascript(script, |_| {});
}

/// Sammelt alle Klassen und IDs der Seite für die allgemeinen Ausblend-Regeln.
const COLLECT_CLASSES_IDS: &str = "(function(){var c=new Set(),i=new Set();\
    document.querySelectorAll('[class],[id]').forEach(function(e){if(e.id)i.add(e.id);\
    if(e.classList)e.classList.forEach(function(x){c.add(x)});});\
    return [Array.from(c),Array.from(i)];})()";

fn strings(v: &JSValue) -> Vec<String> {
    match v {
        JSValue::Array(items) => items
            .iter()
            .filter_map(|x| if let JSValue::String(s) = x { Some(s.clone()) } else { None })
            .collect(),
        _ => Vec::new(),
    }
}

/// Text als JavaScript-String-Literal.
pub(crate) fn js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '<' => out.push_str("\\u003c"),
            c if (c as u32) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Was die Oberfläche über eine geladene Seite wissen muss.
#[derive(Clone, Debug, Default)]
pub struct PageState {
    pub url: Option<Url>,
    pub title: Option<String>,
    pub loading: bool,
    pub can_back: bool,
    pub can_forward: bool,
    /// Blockierte Anfragen (Werbung, Tracker) seit dem Laden der Seite.
    pub blocked: usize,
}

/// Was Vyre von einer Web-Engine braucht, unabhängig davon, welche es ist.
/// Navigation und Eingaben betreffen immer den aktiven Tab.
pub trait Engine {
    /// Lässt die Engine anstehende Arbeit erledigen. `true`, wenn sich die Tab-Liste geändert
    /// hat (von der Seite geöffnete oder geschlossene Tabs).
    fn spin(&mut self) -> bool;

    fn tabs(&self) -> Vec<PageState>;
    fn active(&self) -> usize;
    fn select(&mut self, index: usize);
    /// Öffnet einen neuen Tab hinter dem aktiven und wählt ihn aus.
    fn new_tab(&mut self, url: Url);
    /// Schließt einen Tab. `false`, wenn danach keiner mehr übrig ist.
    fn close_tab(&mut self, index: usize) -> bool;

    fn load(&self, url: Url);
    fn reload(&self);
    fn go_back(&self);
    fn go_forward(&self);
    fn state(&self) -> PageState;
    fn set_focused(&self, focused: bool);

    /// Position und Größe des Seitenbereichs im Hauptfenster (physische Pixel).
    fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32);
    fn set_scale(&self, scale: f64);
    /// Fenster, in das die Seiten gezeichnet werden.
    fn window_id(&self) -> WindowId;
    /// Ereignisse des Seitenfensters (Maus, Neuzeichnen, Größe).
    fn handle_window_event(&self, event: &WindowEvent);
    /// Tastatur (kommt über das Hauptfenster, das den Fokus behält).
    fn handle_key(&self, event: &KeyEvent, modifiers: ModifiersState);
    /// Mausrad mit Position im Seitenbereich.
    fn wheel(&self, delta: MouseScrollDelta, x: f64, y: f64);
    /// Gezeichnete Seitenbilder in der letzten Sekunde.
    fn fps(&self) -> usize;
}

/// Ordner für Cookies, Speicher und Einstellungen: `%LOCALAPPDATA%\Vyre\Profil`.
pub fn profile_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("Vyre").join("Profil")
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

/// Gemeinsamer Zustand aller Tabs, auf den Servos Meldungen zugreifen.
struct Shared {
    child: Arc<Window>,
    main: Arc<Window>,
    context: Rc<WindowRenderingContext>,
    blocker: Arc<Blocker>,
    /// Von Seiten geöffnete Tabs (target="_blank", window.open), noch nicht in der Liste.
    opened: RefCell<Vec<Tab>>,
    /// Von Seiten geschlossene Tabs (window.close).
    closed: RefCell<Vec<WebViewId>>,
    /// CSS zum Ausblenden, das nach dem nächsten Servo-Durchlauf eingefügt wird.
    pending_css: RefCell<Vec<(WebView, String)>>,
}

impl Shared {
    fn queue_css(&self, webview: &WebView, css: String) {
        if !css.is_empty() {
            self.pending_css.borrow_mut().push((webview.clone(), css));
            self.main.request_redraw(); // sorgt für einen weiteren Durchlauf
        }
    }
}

/// Nimmt Servos Meldungen für einen Tab entgegen.
struct Delegate {
    state: RefCell<PageState>,
    shared: Rc<Shared>,
}

impl Delegate {
    fn new(shared: &Rc<Shared>, url: Option<Url>) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(PageState { url, loading: true, ..Default::default() }),
            shared: shared.clone(),
        })
    }

    fn changed(&self, webview: &WebView) {
        {
            let mut s = self.state.borrow_mut();
            s.can_back = webview.can_go_back();
            s.can_forward = webview.can_go_forward();
        }
        self.shared.main.request_redraw();
    }
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _webview: WebView) {
        self.shared.child.request_redraw();
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
        let Some(page) = webview.url().map(|u| u.to_string()) else { return };
        let blocker = self.shared.blocker.clone();
        match status {
            // Seitenspezifische Regeln so früh wie möglich, damit nichts aufblitzt.
            LoadStatus::HeadParsed => {
                if let Some(c) = blocker.cosmetic(&page) {
                    self.shared.queue_css(&webview, c.css);
                }
            }
            // Allgemeine Regeln brauchen die Klassen und IDs der fertigen Seite.
            LoadStatus::Complete => {
                if !blocker.cosmetic(&page).is_some_and(|c| c.generic) {
                    return;
                }
                let target = webview.clone();
                let shared = self.shared.clone();
                webview.evaluate_javascript(COLLECT_CLASSES_IDS, move |result| {
                    let Ok(JSValue::Array(parts)) = result else { return };
                    let (Some(classes), Some(ids)) = (parts.first(), parts.get(1)) else { return };
                    let css = blocker.generic_css(&page, &strings(classes), &strings(ids));
                    // Nicht direkt einfügen: Servo hält hier noch seinen JavaScript-Auswerter.
                    shared.queue_css(&target, css);
                });
            }
            _ => {}
        }
    }

    fn load_web_resource(&self, webview: WebView, load: WebResourceLoad) {
        let request = load.request();
        if request.is_for_main_frame {
            // Neue Seite im Tab: Zähler zurücksetzen. Die Seite selbst wird nicht blockiert.
            self.state.borrow_mut().blocked = 0;
            return;
        }
        let page = webview
            .url()
            .map(|u| u.to_string())
            .or_else(|| request.referrer_url.as_ref().map(|u| u.to_string()))
            .unwrap_or_default();
        let kind = blocker::request_kind(&format!("{:?}", request.destination));
        let url = request.url.clone().into_url();
        if self.shared.blocker.should_block(url.as_str(), &page, kind) {
            log::debug!("blockiert: {url}");
            self.state.borrow_mut().blocked += 1;
            self.shared.main.request_redraw();
            load.intercept(WebResourceResponse::new(url)).cancel();
        }
        // Sonst: `load` fällt weg und Servo lädt die Anfrage normal.
    }

    fn notify_history_changed(&self, webview: WebView, _entries: Vec<Url>, _current: usize) {
        self.changed(&webview);
    }

    fn notify_cursor_changed(&self, webview: WebView, cursor: Cursor) {
        if !webview.focused() {
            return; // Tab im Hintergrund
        }
        match cursor_icon(cursor) {
            Some(icon) => {
                self.shared.child.set_cursor_visible(true);
                self.shared.child.set_cursor(icon);
            }
            None => self.shared.child.set_cursor_visible(false),
        }
    }

    fn request_create_new(&self, _parent: WebView, request: CreateNewWebViewRequest) {
        let shared = &self.shared;
        let delegate = Delegate::new(shared, None);
        let webview = request
            .builder(shared.context.clone())
            .hidpi_scale_factor(Scale::new(shared.child.scale_factor() as f32))
            .delegate(delegate.clone())
            .build();
        shared.opened.borrow_mut().push(Tab { webview, delegate });
        shared.main.request_redraw();
    }

    fn notify_closed(&self, webview: WebView) {
        self.shared.closed.borrow_mut().push(webview.id());
        self.shared.main.request_redraw();
    }

    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        // Später wird das an Wyven weitergereicht.
        log::info!(target: "vyre::console", "[{level:?}] {message}");
    }

    fn notify_crashed(&self, _webview: WebView, reason: String, _backtrace: Option<String>) {
        log::error!("Seite abgestürzt: {reason}");
    }
}

struct Tab {
    webview: WebView,
    delegate: Rc<Delegate>,
}

/// Servo als Engine: eine Servo-Instanz, ein Seitenfenster, eine WebView pro Tab.
pub struct ServoEngine {
    // Reihenfolge = Drop-Reihenfolge: zuerst die Tabs, dann Servo, dann Fenster und Kontext.
    tabs: Vec<Tab>,
    active: usize,
    servo: Servo,
    shared: Rc<Shared>,
    pointer: Cell<DevicePoint>,
    fps: RefCell<FpsCounter>,
}

impl ServoEngine {
    pub fn new(
        event_loop: &ActiveEventLoop,
        main: Arc<Window>,
        proxy: EventLoopProxy<UserEvent>,
        bounds: (i32, i32, u32, u32),
        url: Url,
        blocker: Arc<Blocker>,
        protocols: ProtocolRegistry,
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
        let child = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);

        let display = event_loop.display_handle().map_err(|e| e.to_string())?;
        let handle = child.window_handle().map_err(|e| e.to_string())?;
        let context = Rc::new(
            WindowRenderingContext::new(display, handle, child.inner_size())
                .map_err(|e| format!("OpenGL-Kontext für Servo: {e:?}"))?,
        );
        let _ = context.make_current();

        let profile = profile_dir();
        if let Err(e) = std::fs::create_dir_all(&profile) {
            log::warn!("Profilordner {} nicht anlegbar: {e}", profile.display());
        }
        let opts = Opts {
            config_dir: Some(profile),
            // Ein Absturz in einer Seite soll nicht den ganzen Browser beenden.
            hard_fail: false,
            ..Default::default()
        };
        let servo = ServoBuilder::default()
            .opts(opts)
            .protocol_registry(protocols)
            .event_loop_waker(Box::new(Waker(proxy)))
            .build();

        let shared = Rc::new(Shared {
            child,
            main,
            context,
            blocker,
            opened: RefCell::default(),
            closed: RefCell::default(),
            pending_css: RefCell::default(),
        });
        let mut engine = Self {
            tabs: Vec::new(),
            active: 0,
            servo,
            shared,
            pointer: Cell::new(DevicePoint::new(0.0, 0.0)),
            fps: RefCell::new(FpsCounter::default()),
        };
        engine.new_tab(url);
        Ok(engine)
    }

    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn send(&self, event: InputEvent) {
        if let Some(t) = self.tab() {
            t.webview.notify_input_event(event);
        }
    }

    fn point(&self) -> WebViewPoint {
        WebViewPoint::Device(self.pointer.get())
    }

    /// Zeigt nur die WebView des aktiven Tabs.
    fn show_active(&self) {
        for (i, t) in self.tabs.iter().enumerate() {
            if i == self.active {
                t.webview.show();
            } else {
                t.webview.hide();
                t.webview.set_focused(false);
            }
        }
        self.shared.child.request_redraw();
        self.shared.main.request_redraw();
    }

    fn insert(&mut self, tab: Tab) {
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.active = at;
        self.show_active();
    }
}

impl Engine for ServoEngine {
    fn spin(&mut self) -> bool {
        self.servo.spin_event_loop();
        let css: Vec<(WebView, String)> = self.shared.pending_css.borrow_mut().drain(..).collect();
        for (webview, css) in css {
            inject_css(&webview, &css);
        }
        let opened: Vec<Tab> = self.shared.opened.borrow_mut().drain(..).collect();
        let closed: Vec<WebViewId> = self.shared.closed.borrow_mut().drain(..).collect();
        let changed = !opened.is_empty() || !closed.is_empty();
        for tab in opened {
            self.insert(tab);
        }
        for id in closed {
            if let Some(i) = self.tabs.iter().position(|t| t.webview.id() == id) {
                // Der letzte Tab bleibt offen, auch wenn die Seite sich schließt.
                if self.tabs.len() > 1 {
                    self.close_tab(i);
                }
            }
        }
        changed
    }

    fn tabs(&self) -> Vec<PageState> {
        self.tabs.iter().map(|t| t.delegate.state.borrow().clone()).collect()
    }

    fn active(&self) -> usize {
        self.active
    }

    fn select(&mut self, index: usize) {
        if index < self.tabs.len() && index != self.active {
            self.active = index;
            self.show_active();
        }
    }

    fn new_tab(&mut self, url: Url) {
        let delegate = Delegate::new(&self.shared, Some(url.clone()));
        let webview = WebViewBuilder::new(&self.servo, self.shared.context.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(self.shared.child.scale_factor() as f32))
            .delegate(delegate.clone())
            .build();
        self.insert(Tab { webview, delegate });
    }

    fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return !self.tabs.is_empty();
        }
        self.tabs.remove(index);
        if self.tabs.is_empty() {
            return false;
        }
        // Wie in Firefox/Zen: nach dem Schließen den rechten (hier: unteren) Nachbarn zeigen.
        if index < self.active || self.active >= self.tabs.len() {
            self.active = self.active.saturating_sub(1).min(self.tabs.len() - 1);
        }
        self.show_active();
        true
    }

    fn load(&self, url: Url) {
        if let Some(t) = self.tab() {
            t.webview.load(url);
        }
    }

    fn reload(&self) {
        if let Some(t) = self.tab() {
            t.webview.reload();
        }
    }

    fn go_back(&self) {
        if let Some(t) = self.tab().filter(|t| t.webview.can_go_back()) {
            t.webview.go_back(1);
        }
    }

    fn go_forward(&self) {
        if let Some(t) = self.tab().filter(|t| t.webview.can_go_forward()) {
            t.webview.go_forward(1);
        }
    }

    fn state(&self) -> PageState {
        self.tab().map(|t| t.delegate.state.borrow().clone()).unwrap_or_default()
    }

    fn set_focused(&self, focused: bool) {
        if let Some(t) = self.tab() {
            t.webview.set_focused(focused);
        }
    }

    fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32) {
        let child = &self.shared.child;
        let size = PhysicalSize::new(width.max(1), height.max(1));
        if child.outer_position().ok() != Some(PhysicalPosition::new(x, y)) {
            child.set_outer_position(PhysicalPosition::new(x, y));
        }
        if child.inner_size() != size {
            let _ = child.request_inner_size(size);
            // Alle WebViews teilen sich den Kontext; eine Größenänderung gilt für alle.
            if let Some(t) = self.tabs.first() {
                t.webview.resize(size);
            }
        }
    }

    fn set_scale(&self, scale: f64) {
        for t in &self.tabs {
            t.webview.set_hidpi_scale_factor(Scale::new(scale as f32));
        }
    }

    fn window_id(&self) -> WindowId {
        self.shared.child.id()
    }

    fn handle_window_event(&self, event: &WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                if let Some(t) = self.tab() {
                    let _ = self.shared.context.make_current();
                    t.webview.paint();
                    self.shared.context.present();
                    self.fps.borrow_mut().tick();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(t) = self.tabs.first() {
                    t.webview.resize(*size);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer.set(DevicePoint::new(position.x as f32, position.y as f32));
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
                let p = self.pointer.get();
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
