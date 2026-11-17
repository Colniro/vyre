//! Vyre – ein schlanker Browser in Rust auf Basis von Servo.
//!
//! Aufbau: Das rahmenlose Hauptfenster zeichnet die Oberfläche mit wgpu (Seitenleiste wie bei
//! Zen). Die Webseiten liegen in einem Kind-Fenster, in das Servo mit OpenGL zeichnet.
//! Die Tastatur bleibt beim Hauptfenster und wird je nach Fokus an die Seite weitergereicht.

mod blocker;
#[cfg(windows)]
mod edge;
mod engine;
mod history;
mod keys;
mod newtab;
mod render;
mod search;
mod session;
mod sites;
mod settings;
mod ui;
#[cfg(windows)]
mod win;

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::{Engine, EngineEvent, ServoEngine};
use render::{RoundRect, Scene};
use ui::{Counts, FpsCounter, Hit, Layout, SidebarMode, Stats, Suggestion, SuggestionKind, TabsView, THEME, Ui};
use url::Url;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
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
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();

    let event_loop = EventLoop::<UserEvent>::with_user_event()
        .build()
        .expect("Ereignisschleife konnte nicht erstellt werden");
    // Startadresse oder Suchbegriffe von der Kommandozeile; sonst die Neuer-Tab-Seite.
    let start = std::env::args().nth(1);
    let mut app = App { proxy: event_loop.create_proxy(), start, browser: None };
    event_loop.run_app(&mut app).expect("Ereignisschleife");
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    start: Option<String>,
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
    /// Angepinnte Favoriten (Kopie aus der Datenbank, bei Änderungen neu gelesen).
    favorites: Vec<history::Favorite>,
    /// Zuletzt gespeicherte Sitzung, damit nur bei Änderungen geschrieben wird.
    saved_session: session::Session,
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
        match Browser::new(event_loop, self.proxy.clone(), self.start.clone()) {
            Ok(b) => self.browser = Some(b),
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
            event_loop.set_control_flow(ControlFlow::Wait);
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
    fn new(event_loop: &ActiveEventLoop, proxy: EventLoopProxy<UserEvent>, start: Option<String>) -> Result<Self, String> {
        let settings = settings::Shared::load();
        let vyre_dir = engine::profile_dir().parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let history = Arc::new(history::Store::open(&vyre_dir.join("verlauf.sqlite")));
        // Adresse von der Kommandozeile, sonst die Tabs vom letzten Mal, sonst ein neuer Tab.
        let restore = match start {
            Some(arg) => session::Session {
                urls: search::resolve_input(&arg, settings.get().search_engine.as_deref())
                    .map(|u| vec![u.to_string()])
                    .unwrap_or_default(),
                active: 0,
            },
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
            .with_title("Vyre")
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
        let ui = Ui::new(renderer.font_system());
        let sidebar = Sidebar::new(settings.get().sidebar);
        let size = window.inner_size();
        let layout = Layout::new(size.width, size.height, window.scale_factor() as f32, sidebar.mode, sidebar.reveal, Counts { tabs: 1, ..Default::default() });

        // Eigene Seiten unter vyre:// (Neuer Tab, Suche).
        let mut protocols = servo::protocol_handler::ProtocolRegistry::with_internal_protocols();
        if let Err(e) = protocols.register("vyre", newtab::VyreProtocol::new(settings.clone(), history.clone())) {
            log::error!("vyre:// nicht registriert: {e:?}");
        }
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
            saved_session: restore,
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

    fn open_in_new_tab(&mut self, url: &str) {
        if let Ok(url) = Url::parse(url) {
            self.engine.new_tab(url);
            self.relayout();
            self.focus_page();
        }
    }

    /// Speichert die offenen Tabs, wenn sie sich geändert haben.
    fn save_session(&mut self, tabs: &[engine::PageState], active: usize) {
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
            Hit::Shield => self.toggle_adblock(),
            Hit::Tab(i) => self.select_tab(i),
            Hit::Star => self.toggle_favorite(),
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
        let view = TabsView { tabs: &tabs, active, favorites: &self.favorites, is_favorite };
        let mut frame = self.ui.build(self.renderer.font_system(), &self.layout, &view, &stats);
        // Weicher Schatten unter dem Seitenbereich.
        let c = self.layout.content;
        let s = self.layout.scale;
        frame.cards.insert(0, RoundRect::shadow(c.x, c.y, c.w, c.h, render::Rgba(0, 0, 0, 90), 6.0 * s, 6.0 * s));
        let scene = Scene {
            background: THEME.bg,
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
