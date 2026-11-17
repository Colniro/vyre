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

/// Nach so langer Zeit ohne Benutzung schläft ein Tab ein.
const SLEEP_AFTER: Duration = Duration::from_secs(10 * 60);
/// So oft wird nach Tabs gesucht, die einschlafen können.
const SLEEP_CHECK_EVERY: Duration = Duration::from_secs(30);

/// Vor dem Einschlafen: Gibt es ungesendete Formulareingaben, läuft Ton oder Video?
pub(crate) const PROBE_SLEEP: &str = "(function(){var d=false,p=false;\
    document.querySelectorAll('input,textarea,select').forEach(function(e){var t=(e.type||'').toLowerCase();\
    if(t==='checkbox'||t==='radio'){if(e.checked!==e.defaultChecked)d=true;}\
    else if(e.tagName==='SELECT'){for(var i=0;i<e.options.length;i++){if(e.options[i].selected!==e.options[i].defaultSelected)d=true;}}\
    else if(t!=='hidden'&&t!=='submit'&&t!=='button'&&e.value!==''&&e.value!==e.defaultValue)d=true;});\
    var a=document.activeElement;if(a&&a.isContentEditable&&(a.innerText||'').trim().length>0)d=true;\
    document.querySelectorAll('audio,video').forEach(function(m){if(!m.paused&&!m.muted&&m.volume>0)p=true;});\
    return [d,p];})()";

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
    /// Der Tab schläft: seine Seite ist aus dem Speicher und lädt beim Anklicken neu.
    pub asleep: bool,
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
    /// Die Tab-Liste in Zeilen: einzelne Tabs und Paare (geteilte Ansicht).
    fn rows(&self) -> Vec<Row>;
    fn active(&self) -> usize;
    /// Der zuvor aktive Tab, falls er noch offen ist.
    fn previous(&self) -> Option<usize>;
    fn select(&mut self, index: usize);
    /// Öffnet einen neuen Tab hinter dem aktiven und wählt ihn aus.
    fn new_tab(&mut self, url: Url);
    /// Schließt einen Tab. `false`, wenn danach keiner mehr übrig ist.
    fn close_tab(&mut self, index: usize) -> bool;
    /// Wechselt den aktiven Tab von Hand zur anderen Engine und merkt sich das für die Seite.
    fn toggle_engine(&mut self);
    /// Legt den aktiven Tab sofort schlafen und wechselt zum vorher benutzten.
    fn sleep_active(&mut self);
    /// Zeigt `other` neben `target` (rechts oder links davon).
    fn split(&mut self, target: usize, other: usize, other_right: bool);
    /// Löst den Tab aus seiner geteilten Ansicht.
    fn unsplit(&mut self, index: usize);
    /// Anteil der linken Hälfte, wenn der aktive Tab geteilt ist.
    fn ratio(&self) -> Option<f32>;
    fn set_ratio(&mut self, ratio: f32);

    fn load(&mut self, url: Url);
    fn reload(&self);
    fn go_back(&self);
    fn go_forward(&self);
    fn state(&self) -> PageState;
    fn set_focused(&self, focused: bool);

    /// Position und Größe des Seitenbereichs im Hauptfenster (physische Pixel).
    fn set_bounds(&self, x: i32, y: i32, width: u32, height: u32);
    fn set_scale(&self, scale: f64);
    /// Gehört das Fenster zu Servo (Seitenfenster)?
    fn owns_window(&self, id: WindowId) -> bool;
    /// Linke Kante eines Servo-Fensters im Hauptfenster.
    fn window_x(&self, id: WindowId) -> Option<i32>;
    /// Ereignisse eines Servo-Fensters (Maus, Neuzeichnen, Größe).
    fn handle_window_event(&mut self, id: WindowId, event: &WindowEvent);
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
/// Ein Kind-Fenster mit OpenGL-Kontext, in das Servo zeichnet.
struct Surface {
    window: Arc<Window>,
    context: Rc<WindowRenderingContext>,
}

struct Shared {
    /// [0] für normale Tabs, [1] für die zweite Hälfte einer geteilten Ansicht.
    surfaces: Vec<Surface>,
    main: Arc<Window>,
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
    /// Ergebnis der Prüfung vor dem Einschlafen: Tab-Nummer und ob er wach bleiben muss.
    sleep_checks: Rc<RefCell<Vec<(u64, bool)>>>,
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
    /// In welches Servo-Fenster der Tab zeichnet.
    surface: Cell<usize>,
}

impl Delegate {
    fn new(shared: &Rc<Shared>, url: Option<Url>, surface: usize) -> Rc<Self> {
        Rc::new(Self {
            state: RefCell::new(PageState { url, loading: true, ..Default::default() }),
            shared: shared.clone(),
            js_errors: Cell::new(0),
            probe_at: Cell::new(None),
            surface: Cell::new(surface),
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
        self.shared.surfaces[self.surface.get()].window.request_redraw();
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
        let window = &self.shared.surfaces[self.surface.get()].window;
        match cursor_icon(cursor) {
            Some(icon) => {
                window.set_cursor_visible(true);
                window.set_cursor(icon);
            }
            None => window.set_cursor_visible(false),
        }
    }

    fn request_create_new(&self, _parent: WebView, request: CreateNewWebViewRequest) {
        let shared = &self.shared;
        let delegate = Delegate::new(shared, None, 0);
        let surface = &shared.surfaces[0];
        let webview = request
            .builder(surface.context.clone())
            .hidpi_scale_factor(Scale::new(surface.window.scale_factor() as f32))
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
    /// Schläft: nur Adresse und Titel sind übrig.
    Asleep(PageState),
}

struct Tab {
    id: u64,
    backend: Backend,
    /// Wann der Tab zuletzt zu sehen war.
    last_used: Cell<Instant>,
}

impl Tab {
    fn new(id: u64, backend: Backend) -> Self {
        Self { id, backend, last_used: Cell::new(Instant::now()) }
    }
}

impl Tab {
    fn state(&self) -> PageState {
        match &self.backend {
            Backend::Servo { delegate, .. } => delegate.state.borrow().clone(),
            Backend::Edge(e) => e.state.borrow().clone(),
            Backend::Asleep(s) => s.clone(),
        }
    }

    fn servo(&self) -> Option<&WebView> {
        match &self.backend {
            Backend::Servo { webview, .. } => Some(webview),
            Backend::Edge(_) | Backend::Asleep(_) => None,
        }
    }

    /// In welches Servo-Fenster der Tab zeichnet (`None` bei WebView2).
    fn surface(&self) -> Option<usize> {
        match &self.backend {
            Backend::Servo { delegate, .. } => Some(delegate.surface.get()),
            Backend::Edge(_) | Backend::Asleep(_) => None,
        }
    }
}

/// Zwei Tabs nebeneinander.
#[derive(Clone, Copy, Debug)]
struct Pair {
    left: u64,
    right: u64,
    /// Anteil der linken Hälfte an der Breite (0..1).
    ratio: f32,
}

/// Eine Zeile der Tab-Liste: ein Tab oder ein Paar (Indizes in die Tab-Liste).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    Single(usize),
    Pair(usize, usize),
}

type Bounds = (i32, i32, u32, u32);

/// Servo und WebView2 als Engines, eine Engine pro Tab.
pub struct ServoEngine {
    // Reihenfolge = Drop-Reihenfolge: zuerst die Tabs, dann Servo, dann Fenster und Kontext.
    tabs: Vec<Tab>,
    active: usize,
    /// Der zuvor aktive Tab (für das Teilen mit dem gerade sichtbaren Tab).
    previous: Option<u64>,
    next_id: u64,
    pairs: Vec<Pair>,
    edge: EdgeRuntime,
    servo: Servo,
    shared: Rc<Shared>,
    /// Seitenbereich im Hauptfenster; bei geteilter Ansicht wird er aufgeteilt.
    bounds: Cell<Bounds>,
    pointer: Vec<Cell<DevicePoint>>,
    fps: RefCell<FpsCounter>,
    events: Vec<EngineEvent>,
    /// Nächste Suche nach Tabs, die einschlafen können.
    next_sleep_check: Instant,
    /// Tabs, deren Prüfung vor dem Einschlafen noch läuft.
    sleep_pending: Vec<u64>,
}

/// Erstellt ein Kind-Fenster mit OpenGL-Kontext, in das Servo zeichnet.
fn create_surface(event_loop: &ActiveEventLoop, main: &Window, bounds: Bounds, visible: bool) -> Result<Surface, String> {
    let (x, y, w, h) = bounds;
    let parent = main.window_handle().map_err(|e| e.to_string())?.as_raw();
    let attrs = Window::default_attributes()
        .with_title("Vyre Seite")
        .with_decorations(false)
        .with_visible(visible)
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
    Ok(Surface { window, context })
}

impl ServoEngine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        event_loop: &ActiveEventLoop,
        main: Arc<Window>,
        proxy: EventLoopProxy<UserEvent>,
        bounds: Bounds,
        url: Url,
        blocker: Arc<Blocker>,
        protocols: ProtocolRegistry,
        history: Arc<Store>,
    ) -> Result<Self, String> {
        // Zwei Servo-Fenster: eines für normale Tabs, das zweite für die zweite Hälfte einer
        // geteilten Ansicht, wenn beide Hälften Servo-Seiten sind.
        let first = create_surface(event_loop, &main, bounds, true)?;
        let second = create_surface(event_loop, &main, bounds, false)?;
        let _ = first.context.make_current();

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
            surfaces: vec![first, second],
            main,
            blocker,
            history,
            sites: Rc::new(RefCell::new(Sites::load(&sites::path()))),
            opened: RefCell::default(),
            closed: RefCell::default(),
            pending_css: RefCell::default(),
            switch: RefCell::default(),
            sleep_checks: Rc::default(),
        });
        let mut engine = Self {
            tabs: Vec::new(),
            active: 0,
            previous: None,
            next_id: 1,
            pairs: Vec::new(),
            edge,
            servo,
            shared,
            bounds: Cell::new(bounds),
            pointer: vec![Cell::new(DevicePoint::new(0.0, 0.0)), Cell::new(DevicePoint::new(0.0, 0.0))],
            fps: RefCell::new(FpsCounter::default()),
            events: Vec::new(),
            next_sleep_check: Instant::now() + SLEEP_CHECK_EVERY,
            sleep_pending: Vec::new(),
        };
        engine.new_tab(url);
        Ok(engine)
    }

    fn tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    /// Setzt den aktiven Tab und merkt sich den bisherigen.
    fn set_active(&mut self, index: usize) {
        if let Some(old) = self.tab().map(|t| t.id) {
            if self.tabs.get(index).map(|t| t.id) != Some(old) {
                self.previous = Some(old);
            }
        }
        self.active = index;
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    fn pair_of(&self, id: u64) -> Option<usize> {
        self.pairs.iter().position(|p| p.left == id || p.right == id)
    }

    fn send_to(&self, index: usize, event: InputEvent) {
        if let Some(webview) = self.tabs.get(index).and_then(Tab::servo) {
            webview.notify_input_event(event);
        }
    }

    fn servo_backend(&self, url: Url, surface: usize) -> Backend {
        let delegate = Delegate::new(&self.shared, Some(url.clone()), surface);
        let s = &self.shared.surfaces[surface];
        let webview = WebViewBuilder::new(&self.servo, s.context.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(s.window.scale_factor() as f32))
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
    fn backend_for(&self, id: u64, url: Url, visible: bool, surface: usize) -> Backend {
        let choice = self.shared.sites.borrow().choice(&url);
        if choice == Choice::Edge && !self.edge.failed() {
            log::info!("{url} läuft mit WebView2");
            self.edge_backend(id, url, visible)
        } else {
            self.servo_backend(url, surface)
        }
    }

    /// Ersetzt die Engine eines Tabs (die alte wird freigegeben).
    fn replace_backend(&mut self, index: usize, backend: Backend) {
        if let Some(tab) = self.tabs.get_mut(index) {
            tab.backend = backend;
        }
        self.show_active();
    }

    /// Welche Tabs gerade zu sehen sind und wo: ein Tab oder ein Paar.
    fn view(&self) -> Vec<(usize, Bounds)> {
        let content = self.bounds.get();
        let Some(tab) = self.tab() else { return Vec::new() };
        if let Some(p) = self.pair_of(tab.id).map(|i| self.pairs[i]) {
            if let (Some(l), Some(r)) = (self.index_of(p.left), self.index_of(p.right)) {
                let scale = self.shared.main.scale_factor() as f32;
                let (a, b) = split_bounds(content, p.ratio, scale);
                return vec![(l, a), (r, b)];
            }
        }
        vec![(self.active, content)]
    }

    /// Zwei sichtbare Servo-Tabs brauchen verschiedene Fenster; sonst lädt einer im anderen neu.
    fn separate_surfaces(&mut self) {
        let view = self.view();
        let [(a, _), (b, _)] = view[..] else { return };
        let (Some(sa), Some(sb)) = (self.tabs[a].surface(), self.tabs[b].surface()) else { return };
        if sa != sb {
            return;
        }
        // Die rechte (bzw. nicht aktive) Seite zieht um und lädt dabei einmal neu.
        let mover = if b == self.active { a } else { b };
        let Some(url) = self.tabs[mover].state().url else { return };
        log::info!("{url} lädt für die geteilte Ansicht im zweiten Servo-Fenster neu");
        self.tabs[mover].backend = self.servo_backend(url, 1 - sa);
    }

    /// Zeigt den aktiven Tab (bzw. das aktive Paar) an der richtigen Stelle, alles andere nicht.
    fn show_active(&mut self) {
        self.wake_visible();
        self.separate_surfaces();
        self.apply_view();
    }

    /// Weckt schlafende Tabs, die gleich zu sehen sein werden: sie laden ihre Seite neu.
    fn wake_visible(&mut self) {
        for (i, _) in self.view() {
            let Backend::Asleep(state) = &self.tabs[i].backend else { continue };
            let Some(url) = state.url.clone() else { continue };
            log::info!("Tab wacht auf: {url}");
            let id = self.tabs[i].id;
            self.tabs[i].backend = self.backend_for(id, url, true, 0);
        }
    }

    /// Legt einen Tab schlafen: die Seite wird freigegeben, Adresse und Titel bleiben.
    fn sleep_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get_mut(index) else { return };
        if matches!(tab.backend, Backend::Asleep(_)) {
            return;
        }
        let mut state = tab.state();
        if state.url.is_none() {
            return;
        }
        log::info!("Tab schläft ein: {}", state.url.as_ref().map(|u| u.as_str()).unwrap_or(""));
        state.asleep = true;
        state.loading = false;
        state.blocked = 0;
        // Die alte Engine (Servo-WebView bzw. WebView2-Fenster) wird hier freigegeben.
        tab.backend = Backend::Asleep(state);
    }

    /// Sucht Tabs, die lange nicht zu sehen waren, und prüft sie vor dem Einschlafen.
    fn check_sleep(&mut self) {
        let now = Instant::now();
        let visible: Vec<usize> = self.view().into_iter().map(|(i, _)| i).collect();
        for i in &visible {
            self.tabs[*i].last_used.set(now);
        }
        // Ergebnisse der Prüfungen auswerten.
        let results: Vec<(u64, bool)> = self.shared.sleep_checks.borrow_mut().drain(..).collect();
        for (id, keep_awake) in results {
            self.sleep_pending.retain(|p| *p != id);
            let Some(i) = self.index_of(id) else { continue };
            if keep_awake {
                // Formulareingaben oder Ton: später noch einmal versuchen.
                self.tabs[i].last_used.set(now);
            } else if !self.view().iter().any(|(v, _)| *v == i) {
                self.sleep_tab(i);
            }
        }
        if now < self.next_sleep_check {
            return;
        }
        self.next_sleep_check = now + SLEEP_CHECK_EVERY;
        for (i, tab) in self.tabs.iter().enumerate() {
            if visible.contains(&i)
                || self.sleep_pending.contains(&tab.id)
                || now.duration_since(tab.last_used.get()) < SLEEP_AFTER
            {
                continue;
            }
            let (id, results) = (tab.id, self.shared.sleep_checks.clone());
            let report = move |keep: bool| results.borrow_mut().push((id, keep));
            match &tab.backend {
                Backend::Asleep(_) => continue,
                Backend::Servo { webview, .. } => {
                    webview.evaluate_javascript(PROBE_SLEEP, move |result| {
                        let keep = match result {
                            Ok(JSValue::Array(v)) => v.iter().any(|x| matches!(x, JSValue::Boolean(true))),
                            // Unklar: lieber wach lassen.
                            _ => true,
                        };
                        report(keep);
                    });
                }
                Backend::Edge(e) => {
                    if e.is_playing_audio() {
                        continue;
                    }
                    e.eval(PROBE_SLEEP, move |json| report(json.contains("true") || json.is_empty()));
                }
            }
            self.sleep_pending.push(tab.id);
        }
    }

    fn apply_view(&self) {
        let view = self.view();
        let mut surface_used = [false, false];
        for (i, t) in self.tabs.iter().enumerate() {
            let place = view.iter().find(|(v, _)| *v == i).map(|(_, b)| *b);
            match (&t.backend, place) {
                (Backend::Servo { webview, delegate }, Some(bounds)) => {
                    let s = delegate.surface.get();
                    surface_used[s] = true;
                    let surface = &self.shared.surfaces[s];
                    place_window(&surface.window, bounds);
                    if surface.window.inner_size() != PhysicalSize::new(bounds.2.max(1), bounds.3.max(1)) {
                        webview.resize(PhysicalSize::new(bounds.2.max(1), bounds.3.max(1)));
                    }
                    webview.show();
                    surface.window.request_redraw();
                }
                (Backend::Servo { webview, .. }, None) => {
                    webview.hide();
                    webview.set_focused(false);
                }
                (Backend::Edge(e), Some(bounds)) => {
                    e.set_bounds(bounds);
                    e.set_visible(true);
                }
                (Backend::Edge(e), None) => e.set_visible(false),
                (Backend::Asleep(_), _) => {}
            }
        }
        for (s, used) in surface_used.iter().enumerate() {
            self.shared.surfaces[s].window.set_visible(*used);
        }
        self.shared.main.request_redraw();
    }

    fn insert(&mut self, tab: Tab) {
        let at = (self.active + 1).min(self.tabs.len());
        // Nicht mitten in ein Paar einfügen.
        let at = match (self.tabs.get(at.wrapping_sub(1)), self.tabs.get(at)) {
            (Some(prev), Some(next)) if self.pairs.iter().any(|p| p.left == prev.id && p.right == next.id) => at + 1,
            _ => at,
        };
        let old = self.tab().map(|t| t.id);
        self.tabs.insert(at, tab);
        self.active = at;
        if old.is_some() {
            self.previous = old;
        }
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
            let visible = self.view().iter().any(|(v, _)| *v == i);
            let backend = self.edge_backend(id, r.url, visible);
            self.replace_backend(i, backend);
            changed = true;
        }
        changed
    }

    /// Welches Servo-Fenster gehört zu dieser Fenster-ID?
    fn surface_by_window(&self, id: WindowId) -> Option<usize> {
        self.shared.surfaces.iter().position(|s| s.window.id() == id)
    }

    /// Der sichtbare Tab in einem Servo-Fenster.
    fn tab_on_surface(&self, surface: usize) -> Option<usize> {
        self.view().into_iter().map(|(i, _)| i).find(|i| self.tabs[*i].surface() == Some(surface))
    }

    /// Ein Tab einer sichtbaren geteilten Ansicht bekommt den Fokus (Klick hinein).
    fn activate_visible(&mut self, index: usize) {
        if index != self.active && self.view().iter().any(|(i, _)| *i == index) {
            self.set_active(index);
            self.shared.main.request_redraw();
        }
    }
}

/// Teilt den Seitenbereich in zwei Hälften mit einem Spalt dazwischen.
fn split_bounds((x, y, w, h): Bounds, ratio: f32, scale: f32) -> (Bounds, Bounds) {
    let r = crate::ui::split_rects(
        crate::ui::Rect { x: x as f32, y: y as f32, w: w as f32, h: h as f32 },
        ratio,
        scale,
    );
    let b = |r: crate::ui::Rect| (r.x.round() as i32, r.y.round() as i32, r.w.round() as u32, r.h.round() as u32);
    (b(r.0), b(r.1))
}

fn place_window(window: &Window, (x, y, w, h): Bounds) {
    if window.outer_position().ok() != Some(PhysicalPosition::new(x, y)) {
        window.set_outer_position(PhysicalPosition::new(x, y));
    }
    let size = PhysicalSize::new(w.max(1), h.max(1));
    if window.inner_size() != size {
        let _ = window.request_inner_size(size);
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
        self.check_sleep();

        let opened: Vec<(WebView, Rc<Delegate>)> = self.shared.opened.borrow_mut().drain(..).collect();
        let closed: Vec<WebViewId> = self.shared.closed.borrow_mut().drain(..).collect();
        changed |= !opened.is_empty() || !closed.is_empty();
        for (webview, delegate) in opened {
            let id = self.new_id();
            self.insert(Tab::new(id, Backend::Servo { webview, delegate }));
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
        for (id, e) in edge_events {
            match e {
                EdgeEvent::Shortcut(key, mods) => self.events.push(EngineEvent::Shortcut(key, mods)),
                EdgeEvent::Focused => {
                    // Klick in eine WebView2-Hälfte einer geteilten Ansicht: sie wird aktiv.
                    if let Some(i) = self.index_of(id) {
                        self.activate_visible(i);
                        if i == self.active {
                            self.events.push(EngineEvent::PageFocused);
                        }
                    }
                }
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

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut i = 0;
        while i < self.tabs.len() {
            let id = self.tabs[i].id;
            let next = self.tabs.get(i + 1).map(|t| t.id);
            if next.is_some_and(|n| self.pairs.iter().any(|p| p.left == id && p.right == n)) {
                rows.push(Row::Pair(i, i + 1));
                i += 2;
            } else {
                rows.push(Row::Single(i));
                i += 1;
            }
        }
        rows
    }

    fn active(&self) -> usize {
        self.active
    }

    fn previous(&self) -> Option<usize> {
        self.previous.and_then(|id| self.index_of(id))
    }

    fn select(&mut self, index: usize) {
        if index < self.tabs.len() && index != self.active {
            self.set_active(index);
            self.show_active();
        }
    }

    fn new_tab(&mut self, url: Url) {
        let id = self.new_id();
        let backend = self.backend_for(id, url, true, 0);
        self.insert(Tab::new(id, backend));
    }

    fn close_tab(&mut self, index: usize) -> bool {
        if index >= self.tabs.len() {
            return !self.tabs.is_empty();
        }
        let id = self.tabs[index].id;
        // Schließt man eine Hälfte, wird die andere wieder ein normaler Tab.
        self.pairs.retain(|p| p.left != id && p.right != id);
        if self.previous == Some(id) {
            self.previous = None;
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

    fn split(&mut self, target: usize, other: usize, other_right: bool) {
        if target == other || target >= self.tabs.len() || other >= self.tabs.len() {
            return;
        }
        let (target_id, other_id) = (self.tabs[target].id, self.tabs[other].id);
        // Ziehen auf eine geteilte Ansicht ersetzt die Hälfte auf dieser Seite.
        let mut ratio = 0.5;
        if let Some(p) = self.pair_of(target_id) {
            let pair = self.pairs.remove(p);
            ratio = pair.ratio;
            let keep = if other_right { pair.left } else { pair.right };
            let replaced = if other_right { pair.right } else { pair.left };
            if replaced == other_id {
                // Die Hälfte wurde nur auf ihre eigene Seite gezogen.
                self.pairs.push(pair);
                return;
            }
            return self.split_ids(keep, other_id, other_right, ratio);
        }
        self.unsplit(other);
        self.split_ids(target_id, other_id, other_right, ratio);
    }

    fn unsplit(&mut self, index: usize) {
        let Some(id) = self.tabs.get(index).map(|t| t.id) else { return };
        let Some(p) = self.pair_of(id) else { return };
        self.pairs.remove(p);
        self.show_active();
    }

    fn ratio(&self) -> Option<f32> {
        let id = self.tab()?.id;
        self.pair_of(id).map(|p| self.pairs[p].ratio)
    }

    fn set_ratio(&mut self, ratio: f32) {
        let Some(id) = self.tab().map(|t| t.id) else { return };
        if let Some(p) = self.pair_of(id) {
            self.pairs[p].ratio = ratio.clamp(0.2, 0.8);
            self.apply_view();
        }
    }

    fn sleep_active(&mut self) {
        if self.tabs.len() < 2 {
            return;
        }
        let index = self.active;
        // Zuerst weg vom Tab: zum vorher benutzten oder zum Nachbarn.
        let next = self
            .previous
            .and_then(|id| self.index_of(id))
            .filter(|i| *i != index)
            .unwrap_or(if index + 1 < self.tabs.len() { index + 1 } else { index - 1 });
        // Aus einer geteilten Ansicht lösen, sonst würde er gleich wieder geweckt.
        let id = self.tabs[index].id;
        self.pairs.retain(|p| p.left != id && p.right != id);
        self.set_active(next);
        self.sleep_tab(index);
        self.show_active();
    }

    fn toggle_engine(&mut self) {
        let Some(tab) = self.tab() else { return };
        let Some(url) = tab.state().url.filter(|u| matches!(u.scheme(), "http" | "https")) else { return };
        let to = match tab.backend {
            Backend::Servo { .. } => Choice::Edge,
            Backend::Edge(_) => Choice::Servo,
            Backend::Asleep(_) => return,
        };
        if to == Choice::Edge && self.edge.failed() {
            return;
        }
        log::info!("{url}: von Hand auf {to:?} umgeschaltet");
        self.shared.sites.borrow_mut().set_manual(&url, to);
        let id = tab.id;
        let backend = match to {
            Choice::Edge => self.edge_backend(id, url, true),
            Choice::Servo => self.servo_backend(url, 0),
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
                let surface = tab.surface().unwrap_or(0);
                let backend = self.backend_for(tab.id, url, true, surface);
                self.replace_backend(self.active, backend);
            }
        }
    }

    fn reload(&self) {
        match self.tab().map(|t| &t.backend) {
            Some(Backend::Servo { webview, .. }) => webview.reload(),
            Some(Backend::Edge(e)) => e.reload(),
            Some(Backend::Asleep(_)) | None => {}
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
        if self.bounds.replace((x, y, width, height)) != (x, y, width, height) {
            self.apply_view();
        }
    }

    fn set_scale(&self, scale: f64) {
        for w in self.tabs.iter().filter_map(Tab::servo) {
            w.set_hidpi_scale_factor(Scale::new(scale as f32));
        }
    }

    fn owns_window(&self, id: WindowId) -> bool {
        self.surface_by_window(id).is_some()
    }

    fn window_x(&self, id: WindowId) -> Option<i32> {
        let s = self.surface_by_window(id)?;
        self.shared.surfaces[s].window.outer_position().ok().map(|p| p.x)
    }

    fn handle_window_event(&mut self, id: WindowId, event: &WindowEvent) {
        let Some(s) = self.surface_by_window(id) else { return };
        let target = self.tab_on_surface(s);
        match event {
            WindowEvent::RedrawRequested => {
                if let Some(webview) = target.and_then(|i| self.tabs[i].servo()) {
                    let context = &self.shared.surfaces[s].context;
                    let _ = context.make_current();
                    webview.paint();
                    context.present();
                    self.fps.borrow_mut().tick();
                }
            }
            WindowEvent::Resized(size) => {
                if let Some(webview) = target.and_then(|i| self.tabs[i].servo()) {
                    webview.resize(*size);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer[s].set(DevicePoint::new(position.x as f32, position.y as f32));
                if let Some(i) = target {
                    let point = WebViewPoint::Device(self.pointer[s].get());
                    self.send_to(i, InputEvent::MouseMove(MouseMoveEvent::new(point)));
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(i) = target {
                    self.send_to(i, InputEvent::MouseLeftViewport(MouseLeftViewportEvent::default()));
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let Some(i) = target else { return };
                if *state == ElementState::Pressed {
                    // Klick in eine Hälfte einer geteilten Ansicht: sie wird aktiv.
                    self.activate_visible(i);
                }
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
                let point = WebViewPoint::Device(self.pointer[s].get());
                self.send_to(i, InputEvent::MouseButton(MouseButtonEvent::new(action, button, point)));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let Some(i) = target else { return };
                let p = self.pointer[s].get();
                self.send_to(i, wheel_event(*delta, p.x as f64, p.y as f64));
            }
            _ => {}
        }
    }

    fn handle_key(&self, event: &KeyEvent, modifiers: ModifiersState) {
        self.send_to(self.active, InputEvent::Keyboard(crate::keys::to_servo(event, modifiers)));
    }

    fn wheel(&self, delta: MouseScrollDelta, x: f64, y: f64) {
        self.send_to(self.active, wheel_event(delta, x, y));
    }

    fn fps(&self) -> usize {
        self.fps.borrow_mut().fps()
    }
}

fn wheel_event(delta: MouseScrollDelta, x: f64, y: f64) -> InputEvent {
    // Eine Zeile = 76 px wie in servoshell; Servo dreht das Vorzeichen selbst.
    let (dx, dy, mode) = match delta {
        MouseScrollDelta::LineDelta(dx, dy) => ((dx * 76.0) as f64, (dy * 76.0) as f64, WheelMode::DeltaLine),
        MouseScrollDelta::PixelDelta(d) => (d.x, d.y, WheelMode::DeltaPixel),
    };
    let point = WebViewPoint::Device(DevicePoint::new(x as f32, y as f32));
    InputEvent::Wheel(WheelEvent::new(WheelDelta { x: dx, y: dy, z: 0.0, mode }, point))
}

impl ServoEngine {
    /// Bildet ein Paar aus zwei Tab-IDs; die Tabs rücken in der Liste nebeneinander.
    fn split_ids(&mut self, keep: u64, other: u64, other_right: bool, ratio: f32) {
        // `other` direkt neben `keep` stellen (links oder rechts davon).
        let Some(oi) = self.index_of(other) else { return };
        let tab = self.tabs.remove(oi);
        let Some(ki) = self.index_of(keep) else {
            self.tabs.insert(oi, tab);
            return;
        };
        let at = if other_right { ki + 1 } else { ki };
        self.tabs.insert(at, tab);
        let (left, right) = if other_right { (keep, other) } else { (other, keep) };
        self.pairs.push(Pair { left, right, ratio });
        // Die gezogene Seite wird aktiv.
        self.active = self.index_of(other).unwrap_or(0);
        self.previous = Some(keep);
        self.show_active();
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
