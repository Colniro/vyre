//! Vyre – ein schlanker Browser in Rust auf Basis von Servo.
//!
//! Aufbau: Das rahmenlose Hauptfenster zeichnet die Oberfläche mit wgpu (Seitenleiste wie bei
//! Zen). Die Webseiten liegen in einem Kind-Fenster, in das Servo mit OpenGL zeichnet.
//! Die Tastatur bleibt beim Hauptfenster und wird je nach Fokus an die Seite weitergereicht.

mod engine;
mod keys;
mod render;
mod settings;
mod ui;
#[cfg(windows)]
mod win;

use std::sync::Arc;
use std::time::{Duration, Instant};

use engine::{Engine, ServoEngine};
use render::{RoundRect, Scene};
use settings::Settings;
use ui::{FpsCounter, Hit, Layout, SidebarMode, Stats, TabsView, THEME, Ui};
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
}

const START_PAGE: &str = "https://servo.org/";
const NEW_TAB_PAGE: &str = "about:blank";
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
    let start = std::env::args()
        .nth(1)
        .and_then(|a| ui::resolve_input(&a))
        .unwrap_or_else(|| Url::parse(START_PAGE).expect("gültige Startseite"));
    let mut app = App { proxy: event_loop.create_proxy(), start, browser: None };
    event_loop.run_app(&mut app).expect("Ereignisschleife");
}

struct App {
    proxy: EventLoopProxy<UserEvent>,
    start: Url,
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
    settings: Settings,
    modifiers: ModifiersState,
    cursor: (f32, f32),
    ui_fps: FpsCounter,
    /// Dauerlauf: zeichnet ununterbrochen, um die erreichbare Bildrate zu messen (F2).
    bench: bool,
    refresh_hz: Option<f32>,
    last_click: Option<(Instant, Hit)>,
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

    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        if let (Some(b), UserEvent::ServoWake) = (&mut self.browser, event) {
            b.spin();
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        let Some(b) = &mut self.browser else { return };
        if id == b.engine.window_id() {
            b.page_window_event(&event);
        } else {
            b.main_window_event(event_loop, event);
        }
        b.spin();
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
    fn new(event_loop: &ActiveEventLoop, proxy: EventLoopProxy<UserEvent>, start: Url) -> Result<Self, String> {
        let settings = Settings::load();
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
        let sidebar = Sidebar::new(settings.sidebar);
        let size = window.inner_size();
        let layout = Layout::new(size.width, size.height, window.scale_factor() as f32, sidebar.mode, sidebar.reveal, 1);

        let t = Instant::now();
        let engine = ServoEngine::new(event_loop, window.clone(), proxy, to_bounds(layout.content), start)?;
        log::info!("Servo bereit nach {:?}", t.elapsed());

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
            modifiers: ModifiersState::empty(),
            cursor: (0.0, 0.0),
            ui_fps: FpsCounter::default(),
            bench: false,
            refresh_hz,
            last_click: None,
            last_draw: Instant::now(),
            window,
        })
    }

    fn spin(&mut self) {
        if self.engine.spin() {
            self.relayout();
        }
    }

    fn page_window_event(&mut self, event: &WindowEvent) {
        match event {
            WindowEvent::MouseInput { state: ElementState::Pressed, .. } => self.focus_page(),
            WindowEvent::CursorMoved { position, .. } => {
                self.set_hover(Hit::Content);
                // Ganz links in der Seite zählt wie der Rand: Seitenleiste ausfahren.
                if self.sidebar.mode == SidebarMode::Hover {
                    if position.x < 3.0 * self.layout.scale as f64 {
                        self.sidebar.show();
                    } else {
                        self.sidebar.mouse_left();
                    }
                    self.window.request_redraw();
                }
            }
            _ => {}
        }
        self.engine.handle_window_event(event);
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
                self.window.set_cursor(match hit {
                    Hit::Resize(d) => resize_cursor(d),
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
                    if let Hit::Tab(i) | Hit::TabClose(i) = self.hit() {
                        self.close_tab(event_loop, i);
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
            self.engine.tabs().len(),
        );
        let (x, y, w, h) = to_bounds(self.layout.content);
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
        self.window.request_redraw();
    }

    fn focus_page(&mut self) {
        if self.ui.url_focused {
            self.ui.url_focused = false;
            self.window.request_redraw();
        }
        self.engine.set_focused(true);
    }

    fn new_tab(&mut self) {
        self.engine.new_tab(Url::parse(NEW_TAB_PAGE).expect("gültige URL"));
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

    fn toggle_sidebar(&mut self) {
        self.sidebar.mode = self.sidebar.mode.toggled();
        self.settings.sidebar = self.sidebar.mode;
        self.settings.save();
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
                // Tabs wechseln schon beim Drücken, das fühlt sich schneller an.
                self.select_tab(i);
                self.ui.pressed = Some(hit);
            }
            Hit::Nothing | Hit::Content | Hit::Edge => self.focus_page(),
            _ => self.ui.pressed = Some(hit),
        }
        self.window.request_redraw();
    }

    fn release(&mut self, event_loop: &ActiveEventLoop) {
        let pressed = self.ui.pressed.take();
        self.window.request_redraw();
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
                if let Some(url) = ui::resolve_input(&field.text) {
                    self.engine.load(url);
                }
                self.focus_page();
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
        };
        let view = TabsView { tabs: &tabs, active };
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
            overlay_cards: &[],
            overlay: &[],
            overlay_text: &[],
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
