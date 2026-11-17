//! Web-Engines hinter einer eigenen Schnittstelle: Servo für (fast) alles, WebView2 als
//! unsichtbarer Ersatz für Seiten, die Servo noch nicht schafft.
//!
//! Alle Servo-Tabs teilen sich ein Kind-Fenster mit einem OpenGL-Kontext; Servo zeigt darin nur
//! die WebView des aktiven Tabs. Jeder WebView2-Tab ist ein eigenes Kind-Fenster. Windows setzt
//! sie mit der wgpu-Oberfläche des Hauptfensters zusammen, ohne Bilder zu kopieren.
//!
//! Welche Engine ein Tab bekommt, entscheidet [`Sites`]: Startliste, gelernte Seiten und
//! Festlegungen von Hand. Scheitert eine Seite in Servo, wechselt der Tab still zu WebView2.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use euclid::Scale;
use servo::protocol_handler::ProtocolRegistry;
use servo::{
    ConsoleLogLevel, CreateNewWebViewRequest, Cursor, DevicePoint, EventLoopWaker, InputEvent, JSValue,
    LoadStatus, MouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent,
    NavigationRequest, Opts, RenderingContext, Servo, ServoBuilder, WebResourceLoad, WebResourceResponse,
    WebView, WebViewBuilder, WebViewDelegate, WebViewId, WebViewPoint, WheelDelta, WheelEvent, WheelMode,
    WindowRenderingContext,
};
use url::Url;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, Window, WindowId};

use crate::UserEvent;
use crate::blocker::{self, Blocker};
use crate::edge::{self, EdgeEvent, EdgeRuntime, EdgeShared, EdgeView};
use crate::history::Store;
use crate::sites::{self, Choice, Sites};
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
pub(crate) const COLLECT_CLASSES_IDS: &str = "(function(){var c=new Set(),i=new Set();\
    document.querySelectorAll('[class],[id]').forEach(function(e){if(e.id)i.add(e.id);\
    if(e.classList)e.classList.forEach(function(x){c.add(x)});});\
    return [Array.from(c),Array.from(i)];})()";

/// Prüft nach dem Laden, ob die Seite in Servo gescheitert ist: Textlänge, Anzahl Elemente und
/// ob sie "Browser nicht unterstützt" o. ä. meldet.
const PROBE_PAGE: &str = "(function(){var b=document.body;if(!b)return [0,0,false];\
    var t=(b.innerText||'').trim();var l=t.slice(0,6000).toLowerCase();\
    var bad=/(browser|browsers)[^.]{0,40}(not supported|isn't supported|is not supported|unsupported|nicht unterst\u{fc}tzt)|unsupported browser|update your browser|upgrade your browser|switch to a supported browser|use a supported browser|aktualisiere deinen browser|browser wird nicht unterst\u{fc}tzt/.test(l);\
    return [t.length,b.getElementsByTagName('*').length,bad];})()";

/// So lange nach dem Laden wird geprüft, damit Skripte Zeit zum Aufbauen haben.
const PROBE_DELAY: Duration = Duration::from_millis(2500);

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

/// Ist die Seite laut Prüfung in Servo gescheitert?
fn looks_broken(text_len: f64, elements: f64, unsupported: bool, js_errors: u32) -> bool {
    unsupported || (text_len < 40.0 && elements < 25.0 && js_errors >= 1)
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

/// Meldungen der Engines an die Oberfläche.
pub enum EngineEvent {
    /// Ein Tastenkürzel des Browsers wurde in einer WebView2-Seite gedrückt.
    Shortcut(Key, ModifiersState),
    /// Die Seite hat den Tastaturfokus bekommen.
    PageFocused,
    /// Eine Seite will einen neuen Tab öffnen.
    OpenTab(Url),
}

/// Was Vyre von den Web-Engines braucht. Navigation und Eingaben betreffen den aktiven Tab.
pub trait Engine {
    /// Lässt die Engines anstehende Arbeit erledigen. `true`, wenn sich die Tab-Liste geändert
    /// hat (von der Seite geöffnete oder geschlossene Tabs).
    fn spin(&mut self) -> bool;
    /// Meldungen seit dem letzten Abholen.
    fn take_events(&mut self) -> Vec<EngineEvent>;

    fn tabs(&self) -> Vec<PageState>;
    fn active(&self) -> usize;
    fn select(&mut self, index: usize);
    /// Öffnet einen neuen Tab hinter dem aktiven und wählt ihn aus.
    fn new_tab(&mut self, url: Url);
    /// Schließt einen Tab. `false`, wenn danach keiner mehr übrig ist.
    fn close_tab(&mut self, index: usize) -> bool;
    /// Wechselt den aktiven Tab von Hand zur anderen Engine und merkt sich das für die Seite.
    fn toggle_engine(&mut self);

    fn load(&mut self, url: Url);
    fn reload(&self);
    fn go_back(&self);
    fn go_forward(&self);
    fn state(&self) -> PageState;
    fn set_focused(&self, focused: bool);

    /// Position und Größe des Seitenbereichs im Hauptfenster (physische Pixel).
    fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32);
    fn set_scale(&self, scale: f64);
    /// Fenster, in das Servo zeichnet.
    fn window_id(&self) -> WindowId;
    /// Ereignisse des Servo-Fensters (Maus, Neuzeichnen, Größe).
    fn handle_window_event(&self, event: &WindowEvent);
    /// Tastatur (kommt bei Servo-Tabs über das Hauptfenster, das den Fokus behält).
    fn handle_key(&self, event: &KeyEvent, modifiers: ModifiersState);
    /// Mausrad mit Position im Seitenbereich.
    fn wheel(&self, delta: MouseScrollDelta, x: f64, y: f64);
    /// Von Servo gezeichnete Seitenbilder in der letzten Sekunde.
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

/// Ein Tab soll von Servo zu WebView2 wechseln.
struct SwitchRequest {
    webview: WebViewId,
    url: Url,
    /// Automatisch erkannt (merken bis zum nächsten Update) statt Startliste.
    learn: bool,
}

/// Gemeinsamer Zustand aller Servo-Tabs, auf den Servos Meldungen zugreifen.
struct Shared {
    child: Arc<Window>,
    main: Arc<Window>,
    context: Rc<WindowRenderingContext>,
    blocker: Arc<Blocker>,
    history: Arc<Store>,
    sites: Rc<RefCell<Sites>>,
    /// Von Seiten geöffnete Tabs (target="_blank", window.open), noch nicht in der Liste.
    opened: RefCell<Vec<(WebView, Rc<Delegate>)>>,
    /// Von Seiten geschlossene Tabs (window.close).
    closed: RefCell<Vec<WebViewId>>,
    /// CSS zum Ausblenden, das nach dem nächsten Servo-Durchlauf eingefügt wird.
    pending_css: RefCell<Vec<(WebView, String)>>,
    /// Tabs, die zu WebView2 wechseln sollen.
    switch: RefCell<Vec<SwitchRequest>>,
}

impl Shared {
    fn queue_css(&self, webview: &WebView, css: String) {
        if !css.is_empty() {
            self.pending_css.borrow_mut().push((webview.clone(), css));
            self.main.request_redraw(); // sorgt für einen weiteren Durchlauf
        }
    }

    fn request_switch(&self, webview: &WebView, url: Url, learn: bool) {
        self.switch.borrow_mut().push(SwitchRequest { webview: webview.id(), url, learn });
        self.main.request_redraw();
    }
}

/// Nimmt Servos Meldungen für einen Tab entgegen.
struct Delegate {
    state: RefCell<PageState>,
    shared: Rc<Shared>,
    /// JavaScript-Fehler seit dem Laden der Seite (für die Erkennung).
    js_errors: Cell<u32>,
    /// Wann die Seite auf Scheitern geprüft wird.
    probe_at: Cell<Option<Instant>>,
}

impl Delegate {
    fn new(shared: &Rc<Shared>, url: Option<Url>) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(PageState { url, loading: true, ..Default::default() }),
            shared: shared.clone(),
            js_errors: Cell::new(0),
            probe_at: Cell::new(None),
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

    fn request_navigation(&self, webview: WebView, request: NavigationRequest) {
        // Seiten, die WebView2 brauchen, gar nicht erst in Servo laden.
        if self.shared.sites.borrow().choice(&request.url) == Choice::Edge {
            let url = request.url.clone();
            request.deny();
            self.shared.request_switch(&webview, url, false);
        } else {
            request.allow();
        }
    }

    fn notify_url_changed(&self, webview: WebView, url: Url) {
        // Der Titel kommt erst später (notify_page_title_changed), daher hier ohne Titel.
        self.shared.history.record_visit(url.as_str(), None);
        self.state.borrow_mut().url = Some(url);
        self.changed(&webview);
    }

    fn notify_page_title_changed(&self, webview: WebView, title: Option<String>) {
        if let (Some(url), Some(t)) = (webview.url(), title.as_deref()) {
            self.shared.history.set_title(url.as_str(), t);
        }
        self.state.borrow_mut().title = title;
        self.changed(&webview);
    }

    fn notify_load_status_changed(&self, webview: WebView, status: LoadStatus) {
        // Abgebrochen (Stopped) oder fertig (Complete): lädt nicht mehr.
        self.state.borrow_mut().loading = matches!(status, LoadStatus::Started | LoadStatus::HeadParsed);
        self.changed(&webview);
        let Some(page) = webview.url().map(|u| u.to_string()) else { return };
        let blocker = self.shared.blocker.clone();
        match status {
            LoadStatus::Started => {
                self.js_errors.set(0);
                self.probe_at.set(None);
            }
            // Seitenspezifische Regeln so früh wie möglich, damit nichts aufblitzt.
            LoadStatus::HeadParsed => {
                if let Some(c) = blocker.cosmetic(&page) {
                    self.shared.queue_css(&webview, c.css);
                }
            }
            LoadStatus::Complete => {
                if page.starts_with("http") {
                    self.probe_at.set(Some(Instant::now() + PROBE_DELAY));
                }
                // Allgemeine Regeln brauchen die Klassen und IDs der fertigen Seite.
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
            LoadStatus::Stopped => {}
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
        shared.opened.borrow_mut().push((webview, delegate));
        shared.main.request_redraw();
    }

    fn notify_closed(&self, webview: WebView) {
        self.shared.closed.borrow_mut().push(webview.id());
        self.shared.main.request_redraw();
    }

    fn show_console_message(&self, _webview: WebView, level: ConsoleLogLevel, message: String) {
        if matches!(level, ConsoleLogLevel::Error) {
            self.js_errors.set(self.js_errors.get() + 1);
        }
        // Später wird das an Wyven weitergereicht.
        log::info!(target: "vyre::console", "[{level:?}] {message}");
    }

    fn notify_crashed(&self, webview: WebView, reason: String, _backtrace: Option<String>) {
        log::warn!("Seite in Servo abgestürzt ({reason}), wechsle zu WebView2");
        if let Some(url) = webview.url() {
            self.shared.request_switch(&webview, url, true);
        }
    }
}

enum Backend {
    Servo { webview: WebView, delegate: Rc<Delegate> },
    Edge(EdgeView),
}

struct Tab {
    id: u64,
    backend: Backend,
}

impl Tab {
    fn state(&self) -> PageState {
        match &self.backend {
            Backend::Servo { delegate, .. } => delegate.state.borrow().clone(),
            Backend::Edge(e) => e.state.borrow().clone(),
        }
    }

    fn servo(&self) -> Option<&WebView> {
        match &self.backend {
            Backend::Servo { webview, .. } => Some(webview),
            Backend::Edge(_) => None,
        }
    }
}

/// Servo und WebView2 als Engines, eine Engine pro Tab.
pub struct ServoEngine {
    // Reihenfolge = Drop-Reihenfolge: zuerst die Tabs, dann Servo, dann Fenster und Kontext.
    tabs: Vec<Tab>,
    active: usize,
    next_id: u64,
    edge: EdgeRuntime,
    servo: Servo,
    shared: Rc<Shared>,
    bounds: Cell<(i32, i32, u32, u32)>,
    pointer: Cell<DevicePoint>,
    fps: RefCell<FpsCounter>,
    events: Vec<EngineEvent>,
}

impl ServoEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_loop: &ActiveEventLoop,
        main: Arc<Window>,
        proxy: EventLoopProxy<UserEvent>,
        bounds: (i32, i32, u32, u32),
        url: Url,
        blocker: Arc<Blocker>,
        protocols: ProtocolRegistry,
        history: Arc<Store>,
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
        let vyre_dir = profile.parent().map(|p| p.to_path_buf()).unwrap_or_default();
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

        let edge = EdgeRuntime::new(
            Rc::new(EdgeShared {
                main: main.clone(),
                history: history.clone(),
                blocker: blocker.clone(),
                events: RefCell::default(),
            }),
            vyre_dir.join("WebView2"),
        );
        let shared = Rc::new(Shared {
            child,
            main,
            context,
            blocker,
            history,
            sites: Rc::new(RefCell::new(Sites::load(&sites::path()))),
            opened: RefCell::default(),
            closed: RefCell::default(),
            pending_css: RefCell::default(),
            switch: RefCell::default(),
        });
        let mut engine = Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            edge,
            servo,
            shared,
            bounds: Cell::new(bounds),
            pointer: Cell::new(DevicePoint::new(0.0, 0.0)),
            fps: RefCell::new(FpsCounter::default()),
            events: Vec::new(),
        };
        engine.new_tab(url);
        Ok(engine)
    }

    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    fn send(&self, event: InputEvent) {
        if let Some(webview) = self.tab().and_then(Tab::servo) {
            webview.notify_input_event(event);
        }
    }

    fn point(&self) -> WebViewPoint {
        WebViewPoint::Device(self.pointer.get())
    }

    fn servo_backend(&self, url: Url) -> Backend {
        let delegate = Delegate::new(&self.shared, Some(url.clone()));
        let webview = WebViewBuilder::new(&self.servo, self.shared.context.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(self.shared.child.scale_factor() as f32))
            .delegate(delegate.clone())
            .build();
        Backend::Servo { webview, delegate }
    }

    fn edge_backend(&self, id: u64, url: Url, visible: bool) -> Backend {
        // Cookies aus Servo mitnehmen, damit man angemeldet bleibt.
        let cookies = edge::cookies_from_servo(&self.servo, &url);
        Backend::Edge(self.edge.create_view(id, url, self.bounds.get(), visible, cookies))
    }

    /// Die passende Engine für eine Adresse; Servo, wenn WebView2 nicht verfügbar ist.
    fn backend_for(&self, id: u64, url: Url, visible: bool) -> Backend {
        let choice = self.shared.sites.borrow().choice(&url);
        if choice == Choice::Edge && !self.edge.failed() {
            log::info!("{url} läuft mit WebView2");
            self.edge_backend(id, url, visible)
        } else {
            self.servo_backend(url)
        }
    }

    /// Ersetzt die Engine eines Tabs und lädt dort `url`.
    fn replace_backend(&mut self, index: usize, backend: Backend) {
        if let Some(tab) = self.tabs.get_mut(index) {
            // Die alte Engine wird hier freigegeben (WebView2-Fenster geschlossen bzw. Servo-WebView).
            tab.backend = backend;
        }
        self.show_active();
    }

    /// Zeigt nur den aktiven Tab; das Servo-Fenster nur, wenn ein Servo-Tab aktiv ist.
    fn show_active(&self) {
        let mut servo_active = false;
        for (i, t) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            match &t.backend {
                Backend::Servo { webview, .. } => {
                    if active {
                        webview.show();
                        servo_active = true;
                    } else {
                        webview.hide();
                        webview.set_focused(false);
                    }
                }
                Backend::Edge(e) => e.set_visible(active),
            }
        }
        self.shared.child.set_visible(servo_active);
        self.shared.child.request_redraw();
        self.shared.main.request_redraw();
    }

    fn insert(&mut self, tab: Tab) {
        let at = (self.active + 1).min(self.tabs.len());
        self.tabs.insert(at, tab);
        self.active = at;
        self.show_active();
    }

    fn new_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Prüft fertig geladene Servo-Seiten, ob sie gescheitert sind.
    fn run_probes(&self) {
        let now = Instant::now();
        for tab in &self.tabs {
            let Backend::Servo { webview, delegate } = &tab.backend else { continue };
            if !delegate.probe_at.get().is_some_and(|t| t <= now) {
                continue;
            }
            delegate.probe_at.set(None);
            let (wv, d) = (webview.clone(), delegate.clone());
            webview.evaluate_javascript(PROBE_PAGE, move |result| {
                let Ok(JSValue::Array(v)) = result else { return };
                let num = |i: usize| match v.get(i) {
                    Some(JSValue::Number(n)) => *n,
                    _ => 0.0,
                };
                let unsupported = matches!(v.get(2), Some(JSValue::Boolean(true)));
                let errors = d.js_errors.get();
                if looks_broken(num(0), num(1), unsupported, errors) {
                    if let Some(url) = wv.url() {
                        log::info!(
                            "{url} scheint in Servo zu scheitern (Text {}, Elemente {}, Fehler {errors}, \
                             nicht unterstützt: {unsupported}) – wechsle zu WebView2",
                            num(0),
                            num(1)
                        );
                        // Nur vormerken: wir sind hier mitten in Servos JavaScript-Auswertung.
                        d.shared.request_switch(&wv, url, true);
                    }
                }
            });
        }
    }

    fn handle_switches(&mut self) -> bool {
        let requests: Vec<SwitchRequest> = self.shared.switch.borrow_mut().drain(..).collect();
        let mut changed = false;
        for r in requests {
            if self.edge.failed() {
                log::warn!("WebView2 nicht verfügbar, {} bleibt in Servo", r.url);
                continue;
            }
            let Some(i) = self.tabs.iter().position(|t| t.servo().is_some_and(|w| w.id() == r.webview)) else {
                continue;
            };
            if r.learn {
                self.shared.sites.borrow_mut().learn_edge(&r.url);
            }
            let id = self.tabs[i].id;
            let backend = self.edge_backend(id, r.url, i == self.active);
            self.replace_backend(i, backend);
            changed = true;
        }
        changed
    }
}

impl Engine for ServoEngine {
    fn spin(&mut self) -> bool {
        self.servo.spin_event_loop();
        let css: Vec<(WebView, String)> = self.shared.pending_css.borrow_mut().drain(..).collect();
        for (webview, css) in css {
            inject_css(&webview, &css);
        }
        self.run_probes();
        let mut changed = self.handle_switches();

        let opened: Vec<(WebView, Rc<Delegate>)> = self.shared.opened.borrow_mut().drain(..).collect();
        let closed: Vec<WebViewId> = self.shared.closed.borrow_mut().drain(..).collect();
        changed |= !opened.is_empty() || !closed.is_empty();
        for (webview, delegate) in opened {
            let id = self.new_id();
            self.insert(Tab { id, backend: Backend::Servo { webview, delegate } });
        }
        for wid in closed {
            if let Some(i) = self.tabs.iter().position(|t| t.servo().is_some_and(|w| w.id() == wid)) {
                // Der letzte Tab bleibt offen, auch wenn die Seite sich schließt.
                if self.tabs.len() > 1 {
                    self.close_tab(i);
                }
            }
        }

        let edge_events: Vec<(u64, EdgeEvent)> = self.edge.shared.events.borrow_mut().drain(..).collect();
        let active_id = self.tab().map(|t| t.id);
        for (id, e) in edge_events {
            match e {
                EdgeEvent::Shortcut(key, mods) => self.events.push(EngineEvent::Shortcut(key, mods)),
                EdgeEvent::Focused if Some(id) == active_id => self.events.push(EngineEvent::PageFocused),
                EdgeEvent::Focused => {}
                EdgeEvent::OpenTab(u) => {
                    if let Ok(url) = Url::parse(&u) {
                        self.events.push(EngineEvent::OpenTab(url));
                    }
                }
            }
        }
        changed
    }

    fn take_events(&mut self) -> Vec<EngineEvent> {
        std::mem::take(&mut self.events)
    }

    fn tabs(&self) -> Vec<PageState> {
        self.tabs.iter().map(Tab::state).collect()
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
        let id = self.new_id();
        let backend = self.backend_for(id, url, true);
        self.insert(Tab { id, backend });
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

    fn toggle_engine(&mut self) {
        let Some(tab) = self.tab() else { return };
        let Some(url) = tab.state().url.filter(|u| matches!(u.scheme(), "http" | "https")) else { return };
        let to = match tab.backend {
            Backend::Servo { .. } => Choice::Edge,
            Backend::Edge(_) => Choice::Servo,
        };
        if to == Choice::Edge && self.edge.failed() {
            return;
        }
        log::info!("{url}: von Hand auf {to:?} umgeschaltet");
        self.shared.sites.borrow_mut().set_manual(&url, to);
        let id = tab.id;
        let backend = match to {
            Choice::Edge => self.edge_backend(id, url, true),
            Choice::Servo => self.servo_backend(url),
        };
        self.replace_backend(self.active, backend);
    }

    fn load(&mut self, url: Url) {
        let Some(tab) = self.tab() else { return };
        let want = self.shared.sites.borrow().choice(&url);
        match (&tab.backend, want) {
            (Backend::Servo { webview, .. }, Choice::Servo) => webview.load(url),
            (Backend::Edge(e), Choice::Edge) if !self.edge.failed() => e.load(&url),
            (Backend::Servo { webview, .. }, Choice::Edge) if self.edge.failed() => webview.load(url),
            _ => {
                // Andere Engine nötig: Tab umbauen.
                let backend = self.backend_for(tab.id, url, true);
                self.replace_backend(self.active, backend);
            }
        }
    }

    fn reload(&self) {
        match self.tab().map(|t| &t.backend) {
            Some(Backend::Servo { webview, .. }) => webview.reload(),
            Some(Backend::Edge(e)) => e.reload(),
            None => {}
        }
    }

    fn go_back(&self) {
        match self.tab().map(|t| &t.backend) {
            Some(Backend::Servo { webview, .. }) if webview.can_go_back() => {
                webview.go_back(1);
            }
            Some(Backend::Edge(e)) => e.go_back(),
            _ => {}
        }
    }

    fn go_forward(&self) {
        match self.tab().map(|t| &t.backend) {
            Some(Backend::Servo { webview, .. }) if webview.can_go_forward() => {
                webview.go_forward(1);
            }
            Some(Backend::Edge(e)) => e.go_forward(),
            _ => {}
        }
    }

    fn state(&self) -> PageState {
        self.tab().map(Tab::state).unwrap_or_default()
    }

    fn set_focused(&self, focused: bool) {
        match self.tab().map(|t| &t.backend) {
            Some(Backend::Servo { webview, .. }) => webview.set_focused(focused),
            Some(Backend::Edge(e)) if focused => e.focus(),
            _ => {}
        }
    }

    fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32) {
        self.bounds.set((x, y, width, height));
        for t in &self.tabs {
            if let Backend::Edge(e) = &t.backend {
                e.set_bounds((x, y, width, height));
            }
        }
        let child = &self.shared.child;
        let size = PhysicalSize::new(width.max(1), height.max(1));
        if child.outer_position().ok() != Some(PhysicalPosition::new(x, y)) {
            child.set_outer_position(PhysicalPosition::new(x, y));
        }
        if child.inner_size() != size {
            let _ = child.request_inner_size(size);
            // Alle Servo-WebViews teilen sich den Kontext; eine Größenänderung gilt für alle.
            if let Some(w) = self.tabs.iter().find_map(Tab::servo) {
                w.resize(size);
            }
        }
    }

    fn set_scale(&self, scale: f64) {
        for w in self.tabs.iter().filter_map(Tab::servo) {
            w.set_hidpi_scale_factor(Scale::new(scale as f32));
        }
    }

    fn window_id(&self) -> WindowId {
        self.shared.child.id()
    }

    fn handle_window_event(&self, event: &WindowEvent) {
        match event {
            WindowEvent::RedrawRequested => {
                if let Some(webview) = self.tab().and_then(Tab::servo) {
                    let _ = self.shared.context.make_current();
                    webview.paint();
                    self.shared.context.present();
                    self.fps.borrow_mut().tick();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(w) = self.tabs.iter().find_map(Tab::servo) {
                    w.resize(*size);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erkennung_gescheiterter_seiten() {
        // Leere Seite nach JavaScript-Fehlern: gescheitert.
        assert!(looks_broken(0.0, 8.0, false, 2));
        // Leere Seite ohne Fehler (z. B. absichtlich schlicht): nicht wechseln.
        assert!(!looks_broken(0.0, 8.0, false, 0));
        // Normale Seite mit ein paar Fehlern: bleibt in Servo.
        assert!(!looks_broken(5000.0, 900.0, false, 3));
        // "Browser nicht unterstützt": immer wechseln.
        assert!(looks_broken(5000.0, 900.0, true, 0));
    }
}
