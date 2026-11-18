//! Vyre – ein schlanker Browser in Rust auf Basis von Servo.
//!
//! Aufbau: Das rahmenlose Hauptfenster zeichnet die Oberfläche mit wgpu (Seitenleiste wie bei
//! Zen). Die Webseiten liegen in einem Kind-Fenster, in das Servo mit OpenGL zeichnet.
//! Die Tastatur bleibt beim Hauptfenster und wird je nach Fokus an die Seite weitergereicht.

mod blocker;
#[cfg(windows)]
mod edge;
mod downloads;
mod engine;
mod history;
mod importer;
mod ipc;
mod keys;
mod newtab;
mod privacy;
mod pwfill;
mod render;
mod search;
mod session;
mod sites;
mod settings;
mod ui;
mod update;
mod vault;
#[cfg(windows)]
mod win;

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::{Engine, EngineEvent, ServoEngine};
use render::{RoundRect, Scene};
use ui::{Counts, FpsCounter, Hit, Layout, SidebarMode, Stats, Suggestion, SuggestionKind, TabsView, Ui};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, KeyEvent, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key, ModifiersState, NamedKey};
use winit::window::{CursorIcon, ResizeDirection, Window, WindowId};

#[derive(Debug)]
pub enum UserEvent {
    /// Servo hat Arbeit für den Hauptthread.
    ServoWake,
    /// Die Filterlisten des Werbeblockers sind geladen.
    BlockerReady,
    /// Import aus einem anderen Browser wurde auf vyre://passwoerter gestartet.
    Import(importer::Source),
    /// Ein Download hat Fortschritt gemacht oder ist fertig.
    DownloadsChanged,
    /// Befehl über den Steuerkanal (`vyre open` …).
    Ipc(ipc::Incoming),
    /// Pausieren/Fortsetzen/Abbrechen auf vyre://downloads (für WebView2-Downloads im Hauptthread).
    DownloadOp(u64, String),
    /// "Nach Updates suchen" auf der Neuer-Tab-Seite.
    CheckUpdate,
    /// Ergebnis der Update-Prüfung oder des Downloads.
    Update(update::Event),
}

/// Was die Update-Karte in der Seitenleiste zeigt.
#[derive(Debug)]
enum UpdateCard {
    Checking,
    Offer(update::Manifest),
    Downloading(String),
    Latest,
    Failed(String),
}

impl UpdateCard {
    /// Titel, Zeile darunter, Knöpfe (leer: kein Knopf).
    fn texts(&self) -> (String, String, &'static str, &'static str) {
        let current = update::CURRENT;
        match self {
            UpdateCard::Checking => ("Suche nach Updates …".into(), format!("Vyre {current}"), "", ""),
            UpdateCard::Offer(m) => {
                let note = m.notes.lines().map(str::trim).find(|l| !l.is_empty()).map(str::to_string);
                (format!("Vyre {} ist da", m.version), note.unwrap_or_else(|| format!("Du hast {current}")), "Aktualisieren", "Später")
            }
            UpdateCard::Downloading(v) => (format!("Vyre {v} wird geladen …"), "Danach startet Vyre neu".into(), "", ""),
            UpdateCard::Latest => ("Vyre ist aktuell".into(), format!("Version {current}"), "", ""),
            UpdateCard::Failed(e) => ("Update fehlgeschlagen".into(), e.clone(), "", ""),
        }
    }
}

/// So oft wird die FPS-Anzeige auch ohne andere Änderungen aufgefrischt.
const STATS_REFRESH: Duration = Duration::from_millis(500);
/// So lange bleibt die ausgefahrene Seitenleiste stehen, nachdem die Maus sie verlassen hat.
const SIDEBAR_LINGER: Duration = Duration::from_millis(350);
/// Zeitkonstante des Ein-/Ausfahrens (etwa 3x so lang bis ganz da).
const SIDEBAR_TAU: f32 = 0.045;

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn,vyre=info"))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Steuerbefehle (`vyre open …`, `vyre tabs` …) gehen an das laufende Vyre.
    if let Some(request) = ipc::parse_args(&args) {
        std::process::exit(run_command(request));
    }
    let private = args.iter().any(|a| a == "--privat");
    let beside = ipc::parse_args(&[vec!["open".to_string()], args.clone()].concat()).and_then(|r| match r {
        ipc::Request::Open { beside, .. } => beside,
        _ => None,
    });
    // Läuft schon ein Vyre: dort einen Tab öffnen statt ein zweites Fenster.
    if !private && ipc::running() {
        let url = args.iter().find(|a| !a.starts_with("--")).cloned();
        if ipc::send(&ipc::Request::Open { url, beside }, |_| {}).is_ok() {
            return;
        }
    }
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("Ereignisschleife konnte nicht erstellt werden");
    // "--privat" öffnet ein privates Fenster; sonst Startadresse oder Suchbegriffe.
    let start = args.iter().enumerate().find(|(i, a)| !a.starts_with("--") && (*i == 0 || args[i - 1] != "--neben")).map(|(_, a)| a.clone());
    let mut app = App { proxy: event_loop.create_proxy(), start, private, beside, browser: None };
    event_loop.run_app(&mut app).expect("Ereignisschleife");
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    start: Option<String>,
    private: bool,
    /// Beim Start die rechte Hälfte dieser Fläche einnehmen (aus Wyven).
    beside: Option<[i32; 4]>,
    browser: Option<Browser>,
}

/// Ein- und Ausfahren der Seitenleiste im Hover-Modus.
struct Sidebar {
    mode: SidebarMode,
    /// Aktueller Wert 0..1 (animiert).
    reveal: f32,
    /// Soll sie ausgefahren sein?
    wanted: bool,
    /// Seit wann die Maus die ausgefahrene Leiste verlassen hat.
    left_at: Option<Instant>,
    last_step: Instant,
}

impl Sidebar {
    fn new(mode: SidebarMode) -> Self {
        let reveal = if mode == SidebarMode::Pinned { 1.0 } else { 0.0 };
        Self { mode, reveal, wanted: false, left_at: None, last_step: Instant::now() }
    }

    fn show(&mut self) {
        self.wanted = true;
        self.left_at = None;
    }

    fn mouse_left(&mut self) {
        if self.wanted {
            self.left_at.get_or_insert_with(Instant::now);
        }
    }

    /// Schreitet die Animation voran; `true`, solange sie läuft oder auf das Einfahren wartet.
    fn step(&mut self, keep_open: bool) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_step).as_secs_f32().min(0.05);
        self.last_step = now;
        if self.mode == SidebarMode::Pinned {
            self.reveal = 1.0;
            return false;
        }
        if keep_open {
            self.show();
        } else if self.left_at.is_some_and(|t| now.duration_since(t) >= SIDEBAR_LINGER) {
            self.wanted = false;
            self.left_at = None;
        }
        let target = if self.wanted { 1.0 } else { 0.0 };
        let k = 1.0 - (-dt / SIDEBAR_TAU).exp();
        self.reveal += (target - self.reveal) * k;
        if (target - self.reveal).abs() < 0.002 {
            self.reveal = target;
        }
        self.reveal != target || self.left_at.is_some()
    }
}

struct Browser {
    // Freigabe-Reihenfolge: Engine (Servo + Kind-Fenster), dann GPU, dann Hauptfenster.
    engine: ServoEngine,
    renderer: render::Renderer,
    ui: Ui,
    layout: Layout,
    sidebar: Sidebar,
    settings: settings::Shared,
    blocker: Arc<blocker::Blocker>,
    history: Arc<history::Store>,
    vault: Arc<vault::Vault>,
    downloads: downloads::Downloads,
    /// Terminals, die `vyre konsole` laufen lassen.
    console_subscribers: Vec<std::sync::mpsc::Sender<String>>,
    /// Abgeschicktes Login, das noch auf "Speichern?" wartet: Seite, Benutzername, Passwort.
    pending_save: Option<(String, String, String)>,
    /// Karte zum Update (Angebot, Laden, Ergebnis einer Prüfung von Hand).
    update_card: Option<UpdateCard>,
    /// `vyre update` wartet auf das Ergebnis.
    update_reply: Option<std::sync::mpsc::Sender<String>>,
    update_proxy: EventLoopProxy<UserEvent>,
    /// Angepinnte Favoriten (Kopie aus der Datenbank, bei Änderungen neu gelesen).
    favorites: Vec<history::Favorite>,
    /// Zuletzt gespeicherte Sitzung, damit nur bei Änderungen geschrieben wird.
    saved_session: session::Session,
    /// Mit einer Adresse gestartet: die gespeicherte Sitzung nicht überschreiben.
    keep_session: bool,
    modifiers: ModifiersState,
    cursor: (f32, f32),
    ui_fps: FpsCounter,
    /// Dauerlauf: zeichnet ununterbrochen, um die erreichbare Bildrate zu messen (F2).
    bench: bool,
    refresh_hz: Option<f32>,
    last_click: Option<(Instant, Hit)>,
    /// Ein Tab wird (vielleicht) gezogen: Tab, Startpunkt, schon bewegt?
    drag: Option<(usize, (f32, f32), bool)>,
    /// Beim Ziehen auf die Seite: landet der Tab rechts (`true`) oder links?
    drop_side: Option<bool>,
    last_draw: Instant,
    window: Arc<Window>,
}

impl ApplicationHandler<UserEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.browser.is_some() {
            return;
        }
        match Browser::new(event_loop, self.proxy.clone(), self.start.clone(), self.private) {
            Ok(b) => {
                if let Some(area) = self.beside {
                    b.place_beside(area);
                }
                b.auto_update_check();
                self.browser = Some(b);
            }
            Err(e) => {
                log::error!("Vyre konnte nicht starten: {e}");
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: UserEvent) {
        let Some(b) = &mut self.browser else { return };
        match event {
            UserEvent::ServoWake => b.spin(event_loop),
            UserEvent::BlockerReady => b.window.request_redraw(),
            UserEvent::Import(source) => b.run_import(source),
            UserEvent::DownloadsChanged => b.window.request_redraw(),
            UserEvent::Ipc(incoming) => b.handle_ipc(incoming),
            UserEvent::DownloadOp(id, op) => b.engine.download_control(id, &op),
            UserEvent::CheckUpdate => b.manual_update_check(),
            UserEvent::Update(e) => b.on_update(event_loop, e),
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(b) = &mut self.browser else { return };
        if b.engine.owns_window(id) {
            b.page_window_event(id, &event);
        } else {
            b.main_window_event(event_loop, event);
        }
        b.spin(event_loop);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let Some(b) = &self.browser else { return };
        if b.ui.show_fps {
            let due = b.last_draw + STATS_REFRESH;
            if Instant::now() >= due {
                b.window.request_redraw();
            }
            event_loop.set_control_flow(ControlFlow::WaitUntil(due.max(Instant::now() + Duration::from_millis(16))));
        } else {
            // Ohne FPS-Anzeige trotzdem ab und zu aufwachen, damit Tabs einschlafen können.
            event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + Duration::from_secs(30)));
        }
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        if let Some(b) = &self.browser {
            b.settings.save();
        }
    }
}

fn to_bounds(r: ui::Rect) -> (i32, i32, u32, u32) {
    (r.x.round() as i32, r.y.round() as i32, r.w.round() as u32, r.h.round() as u32)
}

impl Browser {
    fn new(event_loop: &ActiveEventLoop, proxy: EventLoopProxy<UserEvent>, start: Option<String>, private: bool) -> Result<Self, String> {
        let settings = settings::Shared::load();
        let vyre_dir = engine::profile_dir().parent().map(|p| p.to_path_buf()).unwrap_or_default();
        // Privat: Verlauf nur lesen (für Vorschläge), nichts speichern.
        let history = history::Store::open(&vyre_dir.join("verlauf.sqlite"));
        let history = Arc::new(if private { history.read_only() } else { history });
        let vault = Arc::new(vault::Vault::open(&vyre_dir.join("passwoerter.sqlite")));
        let downloads = downloads::Downloads::new(downloads::downloads_dir(), (!private).then(|| vyre_dir.join("downloads.json")), {
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send_event(UserEvent::DownloadsChanged);
            }
        });
        // Adresse von der Kommandozeile, sonst die Tabs vom letzten Mal, sonst ein neuer Tab.
        // Privat: keine Sitzung wiederherstellen und keine speichern.
        let keep_session = start.is_some() || private;
        let restore = match start {
            Some(arg) => session::Session {
                urls: search::resolve_input(&arg, settings.get().search_engine.as_deref())
                    .map(|u| vec![u.to_string()])
                    .unwrap_or_default(),
                active: 0,
            },
            None if private => session::Session::default(),
            None => session::Session::load(&session::path()).unwrap_or_default(),
        };
        let mut urls: Vec<Url> = restore.urls.iter().filter_map(|u| Url::parse(u).ok()).collect();
        if urls.is_empty() {
            urls.push(Url::parse(newtab::NEW_TAB_URL).expect("gültige URL"));
        }
        let start = urls.remove(0);
        // Filterlisten so früh wie möglich im Hintergrund laden.
        let ready = proxy.clone();
        let blocker = blocker::Blocker::start(
            engine::profile_dir().parent().map(|p| p.join("Filter")).unwrap_or_default(),
            settings.get().adblock_off.iter().cloned().collect(),
            move || {
                let _ = ready.send_event(UserEvent::BlockerReady);
            },
        );
        let attrs = Window::default_attributes()
            .with_title(if private { "Vyre – Privat" } else { "Vyre" })
            .with_decorations(false)
            .with_inner_size(LogicalSize::new(1280.0, 820.0))
            .with_min_inner_size(LogicalSize::new(640.0, 420.0))
            .with_visible(false);
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        #[cfg(windows)]
        win::style_main_window(&window);

        let t = Instant::now();
        let mut renderer = pollster::block_on(render::Renderer::new(window.clone(), event_loop.owned_display_handle()));
        log::info!("GPU bereit nach {:?}: {:?}", t.elapsed(), renderer.gpu_info());
        if !private {
            // Steuerkanal für `vyre …` und Wyven; Vyre in den PATH eintragen (einmalig).
            let p = proxy.clone();
            ipc::serve(move |incoming| {
                let _ = p.send_event(UserEvent::Ipc(incoming));
            });
            std::thread::spawn(ipc::ensure_in_path);
        }
        let mut ui = Ui::new(renderer.font_system());
        ui.set_private(private);
        let sidebar = Sidebar::new(settings.get().sidebar);
        let size = window.inner_size();
        let layout = Layout::new(size.width, size.height, window.scale_factor() as f32, sidebar.mode, sidebar.reveal, Counts { tabs: 1, ..Default::default() });

        // Eigene Seiten unter vyre:// (Neuer Tab, Suche).
        let mut protocols = servo::protocol_handler::ProtocolRegistry::with_internal_protocols();
        if let Err(e) = protocols.register("vyre", newtab::VyreProtocol::with_private(private, newtab::VyreProtocol::new(
            settings.clone(),
            history.clone(),
            vault.clone(),
            {
                let proxy = proxy.clone();
                move |source| {
                    let _ = proxy.send_event(UserEvent::Import(source));
                }
            },
            downloads.clone(),
            {
                let proxy = proxy.clone();
                move |id, op| {
                    let _ = proxy.send_event(UserEvent::DownloadOp(id, op));
                }
            },
        )).with_update_check({
            let proxy = proxy.clone();
            move || {
                let _ = proxy.send_event(UserEvent::CheckUpdate);
            }
        })) {
            log::error!("vyre:// nicht registriert: {e:?}");
        }
        let update_proxy = proxy.clone();
        let t = Instant::now();
        let mut engine = ServoEngine::new(
            event_loop,
            window.clone(),
            proxy,
            to_bounds(layout.content),
            start,
            blocker.clone(),
            protocols,
            history.clone(),
            settings.clone(),
            downloads.clone(),
            private,
        )?;
        for url in urls {
            engine.new_tab(url);
        }
        engine.select(restore.active);
        log::info!("Servo bereit nach {:?} ({} Tabs)", t.elapsed(), engine.tabs().len());

        let refresh_hz = window
            .current_monitor()
            .and_then(|m| m.refresh_rate_millihertz())
            .map(|m| m as f32 / 1000.0);
        window.set_visible(true);
        window.request_redraw();
        Ok(Self {
            engine,
            renderer,
            ui,
            layout,
            sidebar,
            settings,
            blocker,
            favorites: history.favorites(),
            history,
            vault,
            downloads,
            console_subscribers: Vec::new(),
            pending_save: None,
            update_card: None,
            update_reply: None,
            update_proxy,
            saved_session: restore,
            keep_session,
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            ui_fps: FpsCounter::default(),
            bench: false,
            refresh_hz,
            last_click: None,
            drag: None,
            drop_side: None,
            last_draw: Instant::now(),
            window,
        })
    }

    fn spin(&mut self, event_loop: &ActiveEventLoop) {
        if self.engine.spin() {
            self.relayout();
        }
        for event in self.engine.take_events() {
            match event {
                // Kürzel aus einem WebView2-Tab: wie im Hauptfenster behandeln.
                EngineEvent::Shortcut(key, mods) => {
                    let saved = std::mem::replace(&mut self.modifiers, mods);
                    self.shortcut(event_loop, &key);
                    self.modifiers = saved;
                }
                EngineEvent::PageFocused => {
                    if self.ui.url_focused {
                        self.ui.url_focused = false;
                        self.ui.suggestions.clear();
                        self.relayout();
                    }
                }
                EngineEvent::Password { tab, page, message } => self.password_message(tab, &page, &message),
                EngineEvent::Console { tab, level, text } => self.forward_console(tab, &level, &text),
                EngineEvent::OpenTab(url) => {
                    self.engine.new_tab(url);
                    self.relayout();
                }
            }
        }
    }

    fn page_window_event(&mut self, id: WindowId, event: &WindowEvent) {
        // Zuerst die Engine: ein Klick in eine Hälfte macht sie zum aktiven Tab.
        self.engine.handle_window_event(id, event);
        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, .. } => self.focus_page(),
            WindowEvent::CursorMoved { position, .. } => {
                self.set_hover(Hit::Content);
                // Ganz links in der Seite zählt wie der Rand: Seitenleiste ausfahren.
                let left = self.engine.window_x(id).unwrap_or(0) as f64 + position.x;
                if self.sidebar.mode == SidebarMode::Hover {
                    if left < self.layout.content.x as f64 + 3.0 * self.layout.scale as f64 {
                        self.sidebar.show();
                    } else {
                        self.sidebar.mouse_left();
                    }
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
    }

    fn main_window_event(&mut self, event_loop: &ActiveEventLoop, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                self.renderer.resize(size.width, size.height);
                self.relayout();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.engine.set_scale(scale_factor);
                self.relayout();
            }
            WindowEvent::Moved(_) => {
                self.refresh_hz = self
                    .window
                    .current_monitor()
                    .and_then(|m| m.refresh_rate_millihertz())
                    .map(|m| m as f32 / 1000.0);
            }
            WindowEvent::ModifiersChanged(m) => self.modifiers = m.state(),
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = (position.x as f32, position.y as f32);
                let hit = self.hit();
                self.set_hover(hit);
                if self.sidebar.mode == SidebarMode::Hover {
                    if hit == Hit::Edge || self.layout.over_sidebar(self.cursor.0) {
                        self.sidebar.show();
                    } else {
                        self.sidebar.mouse_left();
                    }
                    self.window.request_redraw();
                }
                self.drag_move();
                self.window.set_cursor(match hit {
                    _ if self.ui.dragging.is_some() => CursorIcon::Grabbing,
                    _ if self.ui.pressed == Some(Hit::Divider) => CursorIcon::EwResize,
                    Hit::Resize(d) => resize_cursor(d),
                    Hit::Divider => CursorIcon::EwResize,
                    Hit::UrlBar => CursorIcon::Text,
                    _ => CursorIcon::Default,
                });
                // Text in der Adressleiste mit gedrückter Maus markieren.
                if self.ui.pressed == Some(Hit::UrlBar) && self.ui.url_focused {
                    self.ui.click_url(self.cursor.0, &self.layout, true);
                    self.window.request_redraw();
                }
            }
            WindowEvent::CursorLeft { .. } => {
                self.set_hover(Hit::Nothing);
                self.sidebar.mouse_left();
                self.window.request_redraw();
            }
            WindowEvent::MouseInput { state, button, .. } => match (button, state) {
                (MouseButton::Left, ElementState::Pressed) => self.press(),
                (MouseButton::Left, ElementState::Released) => self.release(event_loop),
                (MouseButton::Middle, ElementState::Released) => {
                    // Mittelklick auf einen Tab schließt ihn.
                    match self.hit() {
                        Hit::Tab(i) | Hit::TabClose(i) => self.close_tab(event_loop, i),
                        // Mittelklick auf einen Favoriten löst ihn.
                        Hit::Favorite(i) => {
                            if let Some(f) = self.favorites.get(i).cloned() {
                                self.history.remove_favorite(&f.url);
                                self.favorites = self.history.favorites();
                                self.relayout();
                            }
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            WindowEvent::MouseWheel { delta, .. } => {
                let c = self.layout.content;
                if c.contains(self.cursor.0, self.cursor.1) {
                    self.engine.wheel(delta, (self.cursor.0 - c.x) as f64, (self.cursor.1 - c.y) as f64);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.key(event_loop, event),
            WindowEvent::RedrawRequested => self.draw(),
            _ => {}
        }
    }

    fn hit(&self) -> Hit {
        self.layout.hit(self.cursor.0, self.cursor.1, self.window.is_maximized())
    }

    /// Berechnet die Positionen neu und schiebt das Seitenfenster an seinen Platz.
    fn relayout(&mut self) {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.layout = Layout::new(
            size.width,
            size.height,
            self.window.scale_factor() as f32,
            self.sidebar.mode,
            self.sidebar.reveal,
            Counts {
                tabs: self.engine.tabs().len(),
                favorites: self.favorites.len(),
                suggestions: if self.ui.url_focused { self.ui.suggestions.len() } else { 0 },
                prompt: self.pending_save.is_some() || self.update_card.is_some(),
                downloads: self.downloads.sidebar().len(),
                private_button: !self.ui.private,
            },
        )
        .with_split(&self.engine.rows(), self.engine.ratio(), self.drop_side);
        let (x, y, w, h) = to_bounds(self.layout.page);
        self.engine.set_bounds(x, y, w, h);
        self.window.request_redraw();
    }

    fn set_hover(&mut self, hit: Hit) {
        if self.ui.hover != hit {
            self.ui.hover = hit;
            self.window.request_redraw();
        }
    }

    fn focus_url(&mut self) {
        let state = self.engine.state();
        let text = if Ui::display_url(&state).is_empty() {
            String::new()
        } else {
            state.url.as_ref().map(|u| u.as_str().to_string()).unwrap_or_default()
        };
        self.ui.url.set(&text);
        self.ui.url.select_all();
        self.ui.url_focused = true;
        self.engine.set_focused(false);
        // Aus einem WebView2-Tab den Tastaturfokus zurückholen.
        #[cfg(windows)]
        win::take_focus(&self.window);
        self.window.request_redraw();
    }

    fn focus_page(&mut self) {
        if self.ui.url_focused {
            self.ui.url_focused = false;
            self.ui.suggestions.clear();
            self.relayout();
        }
        self.engine.set_focused(true);
    }

    /// Berechnet die Vorschläge zur Eingabe neu: zuerst das, was Enter tut, dann Favoriten
    /// und Verlauf.
    fn update_suggestions(&mut self) {
        let text = self.ui.url.text.trim().to_string();
        let mut list: Vec<Suggestion> = Vec::new();
        let engine_id = self.settings.get().search_engine.clone();
        if let Some(url) = search::resolve_input(&text, engine_id.as_deref()) {
            let is_search = search::search_url(&text, engine_id.as_deref()).as_ref() == Some(&url);
            let engine = search::engine(engine_id.as_deref());
            list.push(if is_search {
                let bang = text.split_whitespace().any(|w| w.len() > 1 && w.starts_with('!'));
                Suggestion {
                    title: text.clone(),
                    detail: if bang { format!("!Bang · {}", url.host_str().unwrap_or("")) } else { format!("Suche mit {}", engine.name) },
                    url,
                    kind: SuggestionKind::Search,
                }
            } else {
                Suggestion { title: text.clone(), detail: "Seite öffnen".into(), url, kind: SuggestionKind::Open }
            });
        }
        if !text.is_empty() {
            let needle = text.to_lowercase();
            for f in &self.favorites {
                if list.len() >= 3 {
                    break;
                }
                if f.title.to_lowercase().contains(&needle) || f.url.to_lowercase().contains(&needle) {
                    if let Ok(url) = Url::parse(&f.url) {
                        list.push(Suggestion { title: f.title.clone(), detail: short_url(&f.url), url, kind: SuggestionKind::Favorite });
                    }
                }
            }
            for e in self.history.suggest(&text, 8) {
                if list.len() >= 6 {
                    break;
                }
                if list.iter().any(|s| s.url.as_str() == e.url) {
                    continue;
                }
                if let Ok(url) = Url::parse(&e.url) {
                    let title = if e.title.trim().is_empty() { short_url(&e.url) } else { e.title.clone() };
                    list.push(Suggestion { title, detail: short_url(&e.url), url, kind: SuggestionKind::History });
                }
            }
        }
        // Nur die eigene Eingabe ohne weitere Treffer: keine Liste nötig.
        if list.len() == 1 && list[0].kind != SuggestionKind::Search {
            list.clear();
        }
        if list != self.ui.suggestions {
            self.ui.suggestions = list;
            self.ui.selected = 0;
            self.relayout();
        }
    }

    /// Öffnet einen Vorschlag im aktiven Tab.
    fn open_suggestion(&mut self, index: usize) {
        if let Some(s) = self.ui.suggestions.get(index) {
            self.engine.load(s.url.clone());
        }
        self.focus_page();
    }

    /// Pinnt die Seite im aktiven Tab an oder löst sie.
    fn toggle_favorite(&mut self) {
        let state = self.engine.state();
        let Some(url) = state.url.filter(|u| history::is_recordable(u.as_str())) else { return };
        let title = ui::tab_title(&self.engine.state());
        let pinned = self.history.toggle_favorite(url.as_str(), &title);
        log::info!("Favorit {}: {url}", if pinned { "angepinnt" } else { "gelöst" });
        self.favorites = self.history.favorites();
        self.relayout();
    }

    /// Favorit öffnen: zum Tab mit dieser Seite wechseln oder einen neuen öffnen (wie bei Zen).
    fn open_favorite(&mut self, index: usize) {
        let Some(f) = self.favorites.get(index).cloned() else { return };
        let tabs = self.engine.tabs();
        let open = tabs
            .iter()
            .position(|t| t.url.as_ref().is_some_and(|u| ui::same_site(u.as_str(), &f.url)));
        match open {
            Some(i) => self.select_tab(i),
            None => {
                if let Ok(url) = Url::parse(&f.url) {
                    self.engine.new_tab(url);
                    self.relayout();
                    self.focus_page();
                }
            }
        }
    }

    /// Wechselt zu einem Tab mit dieser vyre://-Seite oder öffnet sie neu.
    fn open_page(&mut self, page: &str) {
        let tabs = self.engine.tabs();
        match tabs.iter().position(|t| t.url.as_ref().is_some_and(|u| u.as_str().starts_with(page))) {
            Some(i) => {
                self.select_tab(i);
                self.engine.reload();
            }
            None => self.open_in_new_tab(page),
        }
    }

    fn open_in_new_tab(&mut self, url: &str) {
        if let Ok(url) = Url::parse(url) {
            self.engine.new_tab(url);
            self.relayout();
            self.focus_page();
        }
    }

    /// Befehl über den Steuerkanal (`vyre open`, `tabs`, `reload`, `konsole`).
    fn handle_ipc(&mut self, incoming: ipc::Incoming) {
        let reply = incoming.reply;
        match incoming.request {
            ipc::Request::Open { url, beside } => {
                let engine = self.settings.get().search_engine.clone();
                let url = url
                    .and_then(|u| search::resolve_input(&u, engine.as_deref()))
                    .unwrap_or_else(|| Url::parse(newtab::NEW_TAB_URL).expect("gültige URL"));
                // Ist der aktive Tab leer (Neuer Tab), dort laden, sonst einen neuen öffnen.
                let blank = self.engine.state().url.as_ref().is_none_or(|u| u.as_str() == newtab::NEW_TAB_URL);
                if blank {
                    self.engine.load(url.clone());
                } else {
                    self.engine.new_tab(url.clone());
                }
                self.relayout();
                self.focus_page();
                self.window.set_minimized(false);
                self.window.focus_window();
                if let Some(area) = beside {
                    self.place_beside(area);
                }
                let _ = reply.send(format!("Geöffnet: {url}"));
            }
            ipc::Request::Tabs => {
                let active = self.engine.active();
                for (i, t) in self.engine.tabs().iter().enumerate() {
                    let mark = if i == active { "*" } else { " " };
                    // Lange Adressen (z. B. mit Login-Codes) kürzen, damit die Liste lesbar bleibt.
                    let full = t.url.as_ref().map(|u| u.as_str()).unwrap_or("");
                    let url = if full.chars().count() > 80 {
                        format!("{}…", full.chars().take(79).collect::<String>())
                    } else {
                        full.to_string()
                    };
                    let state = if t.asleep { " (schläft)" } else { "" };
                    let _ = reply.send(format!("{mark}{:>3}  {}{state}  {url}", i + 1, ui::tab_title(t)));
                }
            }
            ipc::Request::Reload => {
                self.engine.reload();
                let _ = reply.send(format!("Neu geladen: {}", ui::tab_title(&self.engine.state())));
            }
            ipc::Request::Update => {
                let _ = reply.send(format!("Vyre {} – suche nach Updates …", update::CURRENT));
                self.update_reply = Some(reply);
                self.manual_update_check();
            }
            ipc::Request::Console => {
                let _ = reply.send(format!("Konsole von „{}“ – Strg+C beendet", ui::tab_title(&self.engine.state())));
                self.console_subscribers.push(reply);
            }
        }
    }

    /// Woher Updates kommen (einstellungen.txt, sonst GitHub) und ob automatisch geprüft wird.
    fn update_source(&self) -> (String, bool) {
        let value = self.settings.get().updates.trim().to_string();
        match value.as_str() {
            "aus" | "off" => (update::OFFICIAL.into(), false),
            "" => (update::OFFICIAL.into(), true),
            _ => (value, true),
        }
    }

    fn update_sender(&self) -> impl FnOnce(update::Event) + Send + 'static {
        let proxy = self.update_proxy.clone();
        move |e| {
            let _ = proxy.send_event(UserEvent::Update(e));
        }
    }

    /// Einmal am Tag beim Start (nicht im privaten Fenster); still, solange nichts Neues da ist.
    fn auto_update_check(&self) {
        let (source, auto) = self.update_source();
        if auto && !self.ui.private && update::due() {
            update::check(source, false, self.update_sender());
        }
    }

    /// Sofort prüfen (Neuer-Tab-Seite, `vyre update`); das Ergebnis zeigt die Karte.
    fn manual_update_check(&mut self) {
        if matches!(self.update_card, Some(UpdateCard::Checking | UpdateCard::Downloading(_))) {
            return;
        }
        self.set_update_card(Some(UpdateCard::Checking));
        let (source, _) = self.update_source();
        update::check(source, true, self.update_sender());
    }

    fn set_update_card(&mut self, card: Option<UpdateCard>) {
        self.update_card = card;
        self.relayout();
    }

    /// Antwort an ein wartendes `vyre update`.
    fn reply_update(&mut self, text: String) {
        if let Some(reply) = self.update_reply.take() {
            let _ = reply.send(text);
        }
    }

    /// "Aktualisieren": Installer laden und prüfen (danach `on_update` mit `Ready`).
    fn update_accept(&mut self) {
        let Some(UpdateCard::Offer(m)) = &self.update_card else { return };
        let m = m.clone();
        let (source, _) = self.update_source();
        self.set_update_card(Some(UpdateCard::Downloading(m.version.clone())));
        update::download(source, m, self.update_sender());
    }

    fn on_update(&mut self, event_loop: &ActiveEventLoop, event: update::Event) {
        match event {
            update::Event::Checked { found: Some(m), .. } => {
                log::info!("Update verfügbar: Vyre {}", m.version);
                self.reply_update(format!("Vyre {} ist da – „Aktualisieren“ in der Seitenleiste installiert es", m.version));
                self.set_update_card(Some(UpdateCard::Offer(m)));
            }
            update::Event::Checked { found: None, manual } => {
                self.reply_update(format!("Vyre ist aktuell ({})", update::CURRENT));
                if manual {
                    self.set_update_card(Some(UpdateCard::Latest));
                }
            }
            update::Event::Ready(path) => match update::run_installer(&path) {
                // Der Installer ersetzt vyre.exe und startet Vyre danach wieder.
                Ok(()) => {
                    log::info!("Installer gestartet: {}", path.display());
                    event_loop.exit();
                }
                Err(e) => self.set_update_card(Some(UpdateCard::Failed(e))),
            },
            update::Event::Failed(e) => {
                log::warn!("Update: {e}");
                self.reply_update(format!("Update: {e}"));
                // Scheitert die automatische Prüfung (z. B. offline), bleibt es still.
                if self.update_card.is_some() {
                    self.set_update_card(Some(UpdateCard::Failed(e)));
                }
            }
        }
    }

    /// Reicht Konsolenmeldungen der aktiven Seite an `vyre konsole` weiter (farbig).
    fn forward_console(&mut self, tab: usize, level: &str, text: &str) {
        if self.console_subscribers.is_empty() || tab != self.engine.active() {
            return;
        }
        let color = match level {
            "fehler" => "\x1b[31m",
            "warnung" => "\x1b[33m",
            "debug" => "\x1b[90m",
            _ => "\x1b[0m",
        };
        let line = format!("{color}[{level}]\x1b[0m {text}");
        self.console_subscribers.retain(|s| s.send(line.clone()).is_ok());
    }

    /// Nimmt die rechte Hälfte der Fläche ein (Wyven nimmt die linke).
    fn place_beside(&self, [x, y, w, h]: [i32; 4]) {
        self.window.set_maximized(false);
        let half = w / 2;
        self.window.set_outer_position(PhysicalPosition::new(x + half, y));
        let _ = self.window.request_inner_size(PhysicalSize::new((w - half) as u32, h as u32));
    }

    /// Importiert Passwörter, Lesezeichen und Verlauf und zeigt das Ergebnis auf vyre://passwoerter.
    fn run_import(&mut self, source: importer::Source) {
        let report = match importer::import(source) {
            Ok(data) => {
                let mut saved = 0;
                for (url, user, pass) in &data.logins {
                    if let Some(origin) = vault::origin_of(url) {
                        if self.vault.save(&origin, user, pass) {
                            saved += 1;
                        }
                    }
                }
                let mut pinned = 0;
                for (title, url) in &data.bookmarks {
                    if history::is_recordable(url) && self.history.add_favorite(url, title) {
                        pinned += 1;
                    }
                }
                let visits = self.history.import_visits(&data.history);
                self.favorites = self.history.favorites();
                let mut text = format!(
                    "Aus {} importiert: {saved} Passwörter, {pinned} Lesezeichen (als Favoriten), {visits} Verlaufseinträge.",
                    source.name()
                );
                if data.skipped_passwords > 0 {
                    text += &format!(
                        " {} Passwörter ließen sich nicht lesen (der Browser schützt sie zusätzlich) – exportiere sie dort als CSV und importiere die Datei.",
                        data.skipped_passwords
                    );
                }
                for note in &data.notes {
                    text += &format!(" {note}.");
                }
                log::info!("{text}");
                text
            }
            Err(e) => format!("Import aus {} nicht möglich: {e}", source.name()),
        };
        let mut url = Url::parse(newtab::PASSWORDS_URL).expect("gültige URL");
        url.query_pairs_mut().append_pair("meldung", &report);
        self.engine.load(url);
        self.relayout();
    }

    /// Nachricht des Passwort-Skripts einer Seite: Konten anzeigen, ausfüllen oder Speichern anbieten.
    fn password_message(&mut self, tab: usize, page: &str, message: &str) {
        // Die Seite bestimmt Vyre selbst aus der Adresse des Tabs.
        let Some(origin) = vault::origin_of(page) else { return };
        match pwfill::parse(message) {
            Some(pwfill::Message::Focus) => {
                let logins = self.vault.logins_for(&origin);
                if !logins.is_empty() {
                    self.engine.eval_in(tab, &pwfill::show_script(&logins));
                }
            }
            Some(pwfill::Message::Fill(id)) => {
                // Nur Konten genau dieser Seite.
                let Some(login) = self.vault.get(id).filter(|l| l.origin == origin) else { return };
                if let Some(pass) = self.vault.password(id) {
                    self.engine.eval_in(tab, &pwfill::fill_script(&login.username, &pass));
                }
            }
            Some(pwfill::Message::Submit { username, password }) => {
                // Privat: gespeicherte Passwörter ausfüllen ja, neue speichern nein.
                if self.ui.private || self.vault.is_never(&origin) || self.vault.has_exact(&origin, &username, &password) {
                    return;
                }
                self.pending_save = Some((origin, username, password));
                self.sidebar.show();
                self.relayout();
            }
            None => {}
        }
    }

    /// Speichert die offenen Tabs, wenn sie sich geändert haben.
    fn save_session(&mut self, tabs: &[engine::PageState], active: usize) {
        if self.keep_session {
            return;
        }
        let now = session::Session {
            urls: tabs.iter().filter_map(|t| t.url.as_ref().map(|u| u.to_string())).collect(),
            active,
        };
        if now != self.saved_session && !now.urls.is_empty() {
            now.save(&session::path());
            self.saved_session = now;
        }
    }

    fn new_tab(&mut self) {
        self.engine.new_tab(Url::parse(newtab::NEW_TAB_URL).expect("gültige URL"));
        self.relayout();
        self.sidebar.show();
        self.focus_url();
    }

    fn close_tab(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        if !self.engine.close_tab(index) {
            event_loop.exit();
            return;
        }
        self.relayout();
        if self.ui.url_focused {
            self.focus_url();
        }
    }

    fn select_tab(&mut self, index: usize) {
        self.engine.select(index);
        if self.ui.url_focused {
            self.focus_url();
        } else {
            self.engine.set_focused(true);
        }
        self.window.request_redraw();
    }

    /// Schaltet den Werbeblocker für die Seite im aktiven Tab um und lädt sie neu.
    fn toggle_adblock(&mut self) {
        let Some(host) = self.engine.state().url.and_then(|u| blocker::host_of(u.as_str())) else { return };
        let on = self.blocker.toggle(&host);
        log::info!("Werbeblocker auf {host}: {}", if on { "an" } else { "aus" });
        let hosts = self.blocker.allowed_hosts();
        self.settings.update(|s| s.adblock_off = hosts);
        self.engine.reload();
    }

    fn toggle_sidebar(&mut self) {
        self.sidebar.mode = self.sidebar.mode.toggled();
        let mode = self.sidebar.mode;
        self.settings.update(|s| s.sidebar = mode);
        // Beim Wechsel zu "hover" bleibt die Leiste erst einmal stehen, bis die Maus geht.
        self.sidebar.reveal = 1.0;
        self.sidebar.show();
        self.sidebar.mouse_left();
        self.relayout();
    }

    fn press(&mut self) {
        let hit = self.hit();
        let now = Instant::now();
        let double = self
            .last_click
            .is_some_and(|(t, h)| h == hit && now.duration_since(t) < Duration::from_millis(400));
        self.last_click = Some((now, hit));
        match hit {
            Hit::Drag => {
                if double {
                    self.window.set_maximized(!self.window.is_maximized());
                } else {
                    let _ = self.window.drag_window();
                }
            }
            Hit::Resize(dir) => {
                let _ = self.window.drag_resize_window(dir);
            }
            Hit::UrlBar => {
                if !self.ui.url_focused {
                    self.focus_url();
                } else if double {
                    self.ui.url.select_all();
                } else {
                    let shift = self.modifiers.shift_key();
                    self.ui.click_url(self.cursor.0, &self.layout, shift);
                }
                self.ui.pressed = Some(hit);
            }
            Hit::Tab(i) => {
                // Erst beim Loslassen wechseln: der Tab könnte auch gezogen werden.
                self.drag = Some((i, self.cursor, false));
                self.ui.pressed = Some(hit);
            }
            Hit::Nothing | Hit::Content | Hit::Edge => self.focus_page(),
            _ => self.ui.pressed = Some(hit),
        }
        self.window.request_redraw();
    }

    /// Tab ziehen (auf die Seite = teilen, aus einem Paar in die Leiste = lösen) und Trennlinie.
    fn drag_move(&mut self) {
        if self.ui.pressed == Some(Hit::Divider) {
            let c = self.layout.content;
            if c.w > 0.0 {
                self.engine.set_ratio((self.cursor.0 - c.x) / c.w);
                self.relayout();
            }
            return;
        }
        let Some((tab, start, moved)) = self.drag else { return };
        let threshold = 6.0 * self.layout.scale;
        if !moved && (self.cursor.0 - start.0).hypot(self.cursor.1 - start.1) < threshold {
            return;
        }
        self.drag = Some((tab, start, true));
        self.ui.dragging = Some(tab);
        let c = self.layout.content;
        // Über der Seite: Vorschau, auf welcher Hälfte der Tab landet. Beim sichtbaren Tab selbst
        // kommt der zuvor benutzte Tab in die andere Hälfte – ohne den geht es nicht.
        let partner = if tab == self.engine.active() { self.engine.previous() } else { Some(self.engine.active()) };
        let over_page = self.cursor.0 >= c.x && partner.is_some_and(|p| p != tab);
        let side = over_page.then(|| self.cursor.0 > c.x + c.w / 2.0);
        if side != self.drop_side {
            self.drop_side = side;
            self.relayout();
        }
        self.window.request_redraw();
    }

    /// Beendet ein Ziehen. `true`, wenn wirklich gezogen (nicht nur geklickt) wurde.
    fn drag_end(&mut self) -> bool {
        let Some((tab, _, moved)) = self.drag.take() else { return false };
        self.ui.dragging = None;
        let side = self.drop_side.take();
        if !moved {
            return false;
        }
        let rows = self.engine.rows();
        let in_pair = rows.iter().any(|r| matches!(*r, engine::Row::Pair(a, b) if a == tab || b == tab));
        if let Some(right) = side {
            let target = if tab == self.engine.active() { self.engine.previous() } else { Some(self.engine.active()) };
            if let Some(target) = target {
                self.engine.split(target, tab, right);
            }
            self.focus_page();
        } else if in_pair && self.layout.over_sidebar(self.cursor.0) {
            // Hälfte zurück in die Leiste gezogen (nicht auf ihre eigene Zeile): Paar lösen.
            let own_row = self.layout.rows.iter().find(|(r, _)| matches!(*r, engine::Row::Pair(a, b) if a == tab || b == tab));
            if !own_row.is_some_and(|(_, rect)| rect.contains(self.cursor.0, self.cursor.1)) {
                self.engine.unsplit(tab);
            }
        }
        self.relayout();
        true
    }

    fn release(&mut self, event_loop: &ActiveEventLoop) {
        let pressed = self.ui.pressed.take();
        self.window.request_redraw();
        if self.drag_end() {
            return;
        }
        if pressed == Some(Hit::Divider) {
            return;
        }
        if pressed != Some(self.ui.hover) {
            return;
        }
        match self.ui.hover {
            Hit::Back => self.engine.go_back(),
            Hit::Forward => self.engine.go_forward(),
            Hit::Reload => self.engine.reload(),
            Hit::SidebarToggle => self.toggle_sidebar(),
            Hit::Minimize => self.window.set_minimized(true),
            Hit::Maximize => self.window.set_maximized(!self.window.is_maximized()),
            Hit::Close => event_loop.exit(),
            Hit::TabClose(i) => self.close_tab(event_loop, i),
            Hit::NewTab => self.new_tab(),
            Hit::NewPrivate => open_private_window(),
            Hit::Shield => self.toggle_adblock(),
            Hit::Tab(i) => self.select_tab(i),
            Hit::Star => self.toggle_favorite(),
            // Ohne Passwort-Karte gehören die Knöpfe zur Update-Karte.
            Hit::PwSave if self.pending_save.is_none() => self.update_accept(),
            Hit::PwNever | Hit::PwDismiss if self.pending_save.is_none() => self.set_update_card(None),
            Hit::PwSave => {
                if let Some((origin, user, pass)) = self.pending_save.take() {
                    if self.vault.save(&origin, &user, &pass) {
                        log::info!("Passwort für {origin} gespeichert");
                    }
                }
                self.relayout();
            }
            Hit::PwNever => {
                if let Some((origin, _, _)) = self.pending_save.take() {
                    self.vault.never_save(&origin);
                }
                self.relayout();
            }
            Hit::Download(_) => self.open_page(newtab::DOWNLOADS_URL),
            Hit::DownloadClose(i) => {
                if let Some(d) = self.downloads.sidebar().get(i) {
                    self.downloads.dismiss(d.id);
                }
                self.relayout();
            }
            Hit::PwDismiss => {
                self.pending_save = None;
                self.relayout();
            }
            Hit::Favorite(i) => self.open_favorite(i),
            Hit::Suggestion(i) => self.open_suggestion(i),
            _ => {}
        }
    }

    /// Tastenkürzel des Browsers; `true`, wenn die Taste verbraucht wurde.
    fn shortcut(&mut self, event_loop: &ActiveEventLoop, key: &Key) -> bool {
        let ctrl = self.modifiers.control_key();
        let alt = self.modifiers.alt_key();
        let shift = self.modifiers.shift_key();
        let char_is = |c: &str, want: &str| c.eq_ignore_ascii_case(want);
        match key {
            Key::Character(c) if ctrl && char_is(c, "l") => self.focus_url(),
            Key::Named(NamedKey::F6) => self.focus_url(),
            Key::Named(NamedKey::F5) => self.engine.reload(),
            Key::Character(c) if ctrl && char_is(c, "r") => self.engine.reload(),
            Key::Named(NamedKey::ArrowLeft) if alt => self.engine.go_back(),
            Key::Named(NamedKey::ArrowRight) if alt => self.engine.go_forward(),
            Key::Character(c) if ctrl && char_is(c, "t") => self.new_tab(),
            Key::Character(c) if ctrl && char_is(c, "w") => {
                let i = self.engine.active();
                self.close_tab(event_loop, i);
            }
            Key::Character(c) if ctrl && char_is(c, "b") => self.toggle_sidebar(),
            Key::Character(c) if ctrl && char_is(c, "d") => self.toggle_favorite(),
            Key::Character(c) if ctrl && shift && char_is(c, "n") => open_private_window(),
            Key::Character(c) if ctrl && !shift && char_is(c, "s") => self.engine.save_current(),
            // Aktiven Tab sofort schlafen legen.
            Key::Character(c) if ctrl && shift && char_is(c, "z") => {
                self.engine.sleep_active();
                self.relayout();
            }
            // Notausgang: Engine des Tabs von Hand wechseln (Servo <-> WebView2).
            Key::Character(c) if ctrl && shift && char_is(c, "e") => {
                self.engine.toggle_engine();
                self.relayout();
            }
            Key::Character(c) if ctrl && char_is(c, "h") => self.open_in_new_tab(newtab::HISTORY_URL),
            Key::Named(NamedKey::Delete) if ctrl && shift => self.open_in_new_tab(newtab::HISTORY_URL),
            Key::Named(NamedKey::Tab) if ctrl => {
                let n = self.engine.tabs().len();
                let a = self.engine.active();
                self.select_tab(if shift { (a + n - 1) % n } else { (a + 1) % n });
            }
            // Strg+1..8 springt zum Tab, Strg+9 zum letzten.
            Key::Character(c) if ctrl && c.len() == 1 && ('1'..='9').contains(&c.chars().next().unwrap()) => {
                let n = self.engine.tabs().len();
                let d = c.chars().next().unwrap().to_digit(10).unwrap() as usize;
                self.select_tab(if d == 9 { n - 1 } else { (d - 1).min(n - 1) });
            }
            Key::Named(NamedKey::F2) => {
                self.bench = !self.bench;
                self.window.request_redraw();
            }
            Key::Named(NamedKey::F3) => {
                self.ui.show_fps = !self.ui.show_fps;
                self.window.request_redraw();
            }
            _ => return false,
        }
        true
    }

    fn key(&mut self, event_loop: &ActiveEventLoop, event: KeyEvent) {
        let pressed = event.state == ElementState::Pressed;
        if pressed && self.shortcut(event_loop, &event.logical_key) {
            return;
        }
        if !self.ui.url_focused {
            self.engine.handle_key(&event, self.modifiers);
            return;
        }
        if !pressed {
            return;
        }
        let ctrl = self.modifiers.control_key();
        let alt = self.modifiers.alt_key();
        let shift = self.modifiers.shift_key();
        let field = &mut self.ui.url;
        match &event.logical_key {
            Key::Named(NamedKey::Enter) => {
                if self.ui.selected > 0 {
                    let i = self.ui.selected;
                    self.open_suggestion(i);
                    return;
                }
                let engine = self.settings.get().search_engine.clone();
                if let Some(url) = search::resolve_input(&field.text, engine.as_deref()) {
                    self.engine.load(url);
                }
                self.focus_page();
                return;
            }
            Key::Named(NamedKey::ArrowDown) => {
                if !self.ui.suggestions.is_empty() {
                    self.ui.selected = (self.ui.selected + 1) % self.ui.suggestions.len();
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                let n = self.ui.suggestions.len();
                if n > 0 {
                    self.ui.selected = (self.ui.selected + n - 1) % n;
                }
            }
            Key::Named(NamedKey::Escape) => self.focus_page(),
            Key::Named(NamedKey::Backspace) => field.backspace(),
            Key::Named(NamedKey::Delete) => field.delete(),
            Key::Named(NamedKey::ArrowLeft) => field.left(shift),
            Key::Named(NamedKey::ArrowRight) => field.right(shift),
            Key::Named(NamedKey::Home) => field.move_to(0, shift),
            Key::Named(NamedKey::End) => field.move_to(field.text.len(), shift),
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("a") => field.select_all(),
            Key::Character(c) if ctrl && (c.eq_ignore_ascii_case("c") || c.eq_ignore_ascii_case("x")) => {
                if let Some(sel) = field.selected_text() {
                    if let Ok(mut cb) = arboard::Clipboard::new() {
                        let _ = cb.set_text(sel.to_string());
                    }
                    if c.eq_ignore_ascii_case("x") {
                        field.backspace();
                    }
                }
            }
            Key::Character(c) if ctrl && c.eq_ignore_ascii_case("v") => {
                if let Some(text) = arboard::Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()) {
                    field.insert(&text.replace(['\r', '\n'], " "));
                }
            }
            _ if !ctrl && !alt => {
                if let Some(text) = &event.text {
                    field.insert(text);
                }
            }
            _ => {}
        }
        if self.ui.url_focused {
            self.update_suggestions();
        }
        self.window.request_redraw();
    }

    fn draw(&mut self) {
        // Seitenleiste: offen halten, solange in der Adressleiste getippt wird.
        let sliding = self.sidebar.step(self.ui.url_focused);
        if sliding || self.layout.reveal != self.sidebar.reveal {
            self.relayout();
        }

        let tabs = self.engine.tabs();
        let active = self.engine.active();
        let page = tabs.get(active).cloned().unwrap_or_default();
        let animating = self.ui.animate(&page) || sliding;
        let stats = Stats {
            ui_fps: self.ui_fps.fps(),
            web_fps: self.engine.fps(),
            refresh_hz: self.refresh_hz,
            bench: self.bench,
            maximized: self.window.is_maximized(),
            adblock_on: self.blocker.is_ready()
                && !page
                    .url
                    .as_ref()
                    .and_then(|u| blocker::host_of(u.as_str()))
                    .is_some_and(|h| self.blocker.is_allowed(&h)),
        };
        self.save_session(&tabs, active);
        let is_favorite = page.url.as_ref().is_some_and(|u| self.favorites.iter().any(|f| f.url == u.as_str()));
        let sidebar_downloads = self.downloads.sidebar();
        // Die Liste der Downloads in der Seitenleiste kann sich jederzeit ändern.
        if sidebar_downloads.len() != self.layout.downloads.len() {
            self.relayout();
        }
        // Die Passwort-Karte geht vor; sonst die Update-Karte.
        let card_text = match &self.pending_save {
            Some((origin, user, _)) => {
                let host = origin.split("://").nth(1).unwrap_or(origin);
                let who = if user.is_empty() { host.to_string() } else { format!("{user} · {host}") };
                Some(("Passwort speichern?".to_string(), who, "Speichern", "Nie hier"))
            }
            None => self.update_card.as_ref().map(UpdateCard::texts),
        };
        let view = TabsView {
            tabs: &tabs,
            active,
            favorites: &self.favorites,
            is_favorite,
            card: card_text.as_ref().map(|(title, detail, primary, secondary)| ui::Card { title, detail, primary, secondary }),
            downloads: &sidebar_downloads,
        };
        let background = self.ui.theme().bg;
        let mut frame = self.ui.build(self.renderer.font_system(), &self.layout, &view, &stats);
        // Weicher Schatten unter dem Seitenbereich.
        let c = self.layout.content;
        let s = self.layout.scale;
        frame.cards.insert(0, RoundRect::shadow(c.x, c.y, c.w, c.h, render::Rgba(0, 0, 0, 90), 6.0 * s, 6.0 * s));
        let scene = Scene {
            background,
            cards: &frame.cards,
            under: &frame.under,
            text: &frame.text,
            over: &frame.over,
            masks: &[],
            overlay_cards: &frame.overlay_cards,
            overlay: &frame.overlay,
            overlay_text: &frame.overlay_text,
        };
        if self.renderer.draw(&scene) {
            self.ui_fps.tick();
        }
        self.last_draw = Instant::now();
        if animating || self.bench {
            self.window.request_redraw();
        }
    }
}

fn resize_cursor(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::East | ResizeDirection::West => CursorIcon::EwResize,
        ResizeDirection::North | ResizeDirection::South => CursorIcon::NsResize,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::NeswResize,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::NwseResize,
    }
}

/// Adresse ohne "https://", "www." und abschließendes "/".
fn short_url(url: &str) -> String {
    let s = url.split("://").nth(1).unwrap_or(url);
    s.trim_start_matches("www.").trim_end_matches('/').to_string()
}

/// Startet Vyre noch einmal als privates Fenster (eigener Prozess, nichts wird gespeichert).
fn open_private_window() {
    match std::env::current_exe() {
        Ok(exe) => {
            if let Err(e) = std::process::Command::new(exe).arg("--privat").spawn() {
                log::error!("Privates Fenster nicht gestartet: {e}");
            }
        }
        Err(e) => log::error!("Privates Fenster: {e}"),
    }
}

/// Führt einen `vyre …`-Befehl im Terminal aus. Läuft kein Vyre, startet `vyre open` es.
fn run_command(request: ipc::Request) -> i32 {
    match ipc::send(&request, |line| println!("{line}")) {
        Ok(()) => 0,
        Err(_) => match request {
            ipc::Request::Open { url, beside } => {
                let Ok(exe) = std::env::current_exe() else { return 1 };
                let mut cmd = std::process::Command::new(exe);
                cmd.args(url);
                if let Some([x, y, w, h]) = beside {
                    cmd.arg("--neben").arg(format!("{x},{y},{w},{h}"));
                }
                // Vom Terminal lösen, damit Vyre weiterläuft, wenn es geschlossen wird.
                use std::os::windows::process::CommandExt;
                const DETACHED_PROCESS: u32 = 0x0000_0008;
                match cmd.creation_flags(DETACHED_PROCESS).spawn() {
                    Ok(_) => {
                        println!("Vyre wird gestartet …");
                        0
                    }
                    Err(e) => {
                        eprintln!("Vyre konnte nicht gestartet werden: {e}");
                        1
                    }
                }
            }
            // Ohne laufendes Vyre: nur nachsehen und Bescheid sagen.
            ipc::Request::Update => {
                let (tx, rx) = std::sync::mpsc::channel();
                let source = settings::Settings::load().updates.trim().to_string();
                let source = if source.is_empty() || source == "aus" || source == "off" { update::OFFICIAL.to_string() } else { source };
                update::check(source, true, move |e| {
                    let _ = tx.send(e);
                });
                match rx.recv() {
                    Ok(update::Event::Checked { found: Some(m), .. }) => {
                        println!("Vyre {} ist da (du hast {}). Starte Vyre und klicke „Aktualisieren“ in der Seitenleiste.", m.version, update::CURRENT);
                        0
                    }
                    Ok(update::Event::Checked { found: None, .. }) => {
                        println!("Vyre ist aktuell ({})", update::CURRENT);
                        0
                    }
                    Ok(update::Event::Failed(e)) => {
                        eprintln!("Update-Prüfung fehlgeschlagen: {e}");
                        1
                    }
                    _ => 1,
                }
            }
            _ => {
                eprintln!("Vyre läuft nicht. Starte es mit `vyre open <adresse>`.");
                1
            }
        },
    }
}
