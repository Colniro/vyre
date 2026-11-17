//! Die Browser-Oberfläche: Seitenleiste (Navigation, Adressleiste, Tabs, Fensterknöpfe),
//! Hover-Animationen und die FPS-Anzeige. Alles in physischen Pixeln, skaliert mit `scale`.
//!
//! Die Seitenleiste hat zwei Modi wie bei Zen: fest angeheftet, oder eingeklappt und beim
//! Hovern am linken Rand ausfahrend. Beim Ausfahren wird die Seite nur verschoben, nicht in der
//! Größe geändert. So muss Servo nichts neu umbrechen, und die Animation bleibt flüssig.

use std::time::{Duration, Instant};

use winit::window::ResizeDirection;

use crate::engine::PageState;
use crate::render::{Attrs, Buffer, Family, FontSystem, Metrics, Quad, RoundRect, Rgba, Shaping, TextBlock, Weight, Wrap};

/// Farben der Oberfläche (dunkel, violetter Akzent).
pub struct Theme {
    pub bg: Rgba,
    pub surface: Rgba,
    pub hover: Rgba,
    pub active: Rgba,
    pub text: Rgba,
    pub dim: Rgba,
    pub accent: Rgba,
    pub selection: Rgba,
    pub close: Rgba,
}

pub const THEME: Theme = Theme {
    bg: Rgba::rgb(0x15, 0x15, 0x1c),
    surface: Rgba::rgb(0x21, 0x21, 0x2b),
    hover: Rgba::rgb(0x2d, 0x2d, 0x3a),
    active: Rgba::rgb(0x34, 0x32, 0x48),
    text: Rgba::rgb(0xe8, 0xe8, 0xf0),
    dim: Rgba::rgb(0x8a, 0x8a, 0x9c),
    accent: Rgba::rgb(0x9d, 0x8c, 0xff),
    selection: Rgba(0x9d, 0x8c, 0xff, 0x60),
    close: Rgba::rgb(0xc4, 0x2b, 0x1c),
};

/// Breite der Seitenleiste in logischen Pixeln.
const SIDEBAR_W: f32 = 264.0;
/// Abstand des Seiteninhalts zum Fensterrand.
const GAP: f32 = 8.0;
/// Breite des unsichtbaren Rands, an dem man die Fenstergröße ändert.
const RESIZE_BAND: f32 = 6.0;
const TAB_H: f32 = 36.0;
const TAB_SPACING: f32 = 2.0;

/// Wie sich die Seitenleiste verhält.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SidebarMode {
    /// Immer sichtbar, die Seite steht daneben.
    Pinned,
    /// Eingeklappt; fährt aus, wenn die Maus an den linken Rand kommt.
    Hover,
}

impl SidebarMode {
    pub fn toggled(self) -> Self {
        match self {
            Self::Pinned => Self::Hover,
            Self::Hover => Self::Pinned,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Pinned => "fest",
            Self::Hover => "hover",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        match s.trim() {
            "fest" => Some(Self::Pinned),
            "hover" => Some(Self::Hover),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Was unter dem Mauszeiger liegt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Back,
    Forward,
    Reload,
    SidebarToggle,
    Minimize,
    Maximize,
    Close,
    UrlBar,
    Tab(usize),
    TabClose(usize),
    NewTab,
    /// Linker Rand im Hover-Modus: lässt die Seitenleiste ausfahren.
    Edge,
    Drag,
    Resize(ResizeDirection),
    Content,
    Nothing,
}

/// Feste Knöpfe der oberen Zeile, in dieser Reihenfolge in `Layout::top`.
const TOP: [Hit; 7] = [
    Hit::Back,
    Hit::Forward,
    Hit::Reload,
    Hit::SidebarToggle,
    Hit::Minimize,
    Hit::Maximize,
    Hit::Close,
];

/// Positionen aller Bereiche für die aktuelle Fenstergröße.
pub struct Layout {
    pub scale: f32,
    pub width: f32,
    pub height: f32,
    pub mode: SidebarMode,
    /// Sichtbarer Teil der Seitenleiste (0 = eingeklappt, 1 = ganz da).
    pub reveal: f32,
    /// Linke Kante der Seitenleiste (negativ, solange sie eingeklappt ist).
    pub sidebar_x: f32,
    pub sidebar_w: f32,
    pub top: [Rect; 7],
    pub url: Rect,
    pub tabs_label: Rect,
    pub tabs: Vec<Rect>,
    pub new_tab: Rect,
    pub content: Rect,
    pub fps: Rect,
}

impl Layout {
    pub fn new(width: u32, height: u32, scale: f32, mode: SidebarMode, reveal: f32, tab_count: usize) -> Self {
        let s = scale;
        let (w, h) = (width as f32, height as f32);
        let sw = (SIDEBAR_W * s).round();
        let gap = (GAP * s).round();
        let reveal = if mode == SidebarMode::Pinned { 1.0 } else { reveal.clamp(0.0, 1.0) };
        let sx = ((reveal - 1.0) * sw).round();

        let btn = 30.0 * s;
        let top = 9.0 * s;
        let pad = 10.0 * s;
        let nav = |i: f32| Rect { x: sx + pad + i * (btn + 2.0 * s), y: top, w: btn, h: btn };
        let ctl_w = 36.0 * s;
        let ctl = |i: f32| Rect { x: sx + sw - pad - (3.0 - i) * ctl_w + 4.0 * s, y: top, w: ctl_w, h: btn };
        let url = Rect { x: sx + pad, y: top + btn + 8.0 * s, w: sw - 2.0 * pad, h: 36.0 * s };
        let tabs_label = Rect { x: sx + pad + 6.0 * s, y: url.y + url.h + 16.0 * s, w: sw - 2.0 * pad, h: 18.0 * s };
        let first = tabs_label.y + tabs_label.h + 4.0 * s;
        let row = |i: usize| Rect { x: sx + pad, y: first + i as f32 * (TAB_H + TAB_SPACING) * s, w: sw - 2.0 * pad, h: TAB_H * s };
        let tabs = (0..tab_count).map(row).collect();
        let new_tab = row(tab_count);

        // Fest: Seite neben der Leiste. Hover: Seite fast fensterbreit, beim Ausfahren verschoben.
        let content = match mode {
            SidebarMode::Pinned => Rect { x: sw, y: gap, w: (w - sw - gap).max(1.0), h: (h - 2.0 * gap).max(1.0) },
            SidebarMode::Hover => Rect {
                x: (gap + reveal * (sw - gap)).round(),
                y: gap,
                w: (w - 2.0 * gap).max(1.0),
                h: (h - 2.0 * gap).max(1.0),
            },
        };
        let fps = Rect { x: sx + pad + 4.0 * s, y: h - 46.0 * s, w: sw - 2.0 * pad, h: 40.0 * s };
        Self {
            scale,
            width: w,
            height: h,
            mode,
            reveal,
            sidebar_x: sx,
            sidebar_w: sw,
            top: [nav(0.0), nav(1.0), nav(2.0), nav(3.0), ctl(0.0), ctl(1.0), ctl(2.0)],
            url,
            tabs_label,
            tabs,
            new_tab,
            content,
            fps,
        }
    }

    /// Liegt der Punkt über der (sichtbaren) Seitenleiste?
    pub fn over_sidebar(&self, x: f32) -> bool {
        self.reveal > 0.0 && x < self.sidebar_x + self.sidebar_w
    }

    pub fn hit(&self, x: f32, y: f32, maximized: bool) -> Hit {
        if !maximized {
            let band = RESIZE_BAND * self.scale;
            let (l, r) = (x < band, x >= self.width - band);
            let (t, b) = (y < band, y >= self.height - band);
            let dir = match (l, r, t, b) {
                (true, _, true, _) => Some(ResizeDirection::NorthWest),
                (_, true, true, _) => Some(ResizeDirection::NorthEast),
                (true, _, _, true) => Some(ResizeDirection::SouthWest),
                (_, true, _, true) => Some(ResizeDirection::SouthEast),
                (true, ..) => Some(ResizeDirection::West),
                (_, true, ..) => Some(ResizeDirection::East),
                (_, _, true, _) => Some(ResizeDirection::North),
                (_, _, _, true) => Some(ResizeDirection::South),
                _ => None,
            };
            if let Some(dir) = dir {
                return Hit::Resize(dir);
            }
        }
        if self.over_sidebar(x) {
            for (i, r) in self.top.iter().enumerate() {
                if r.contains(x, y) {
                    return TOP[i];
                }
            }
            if self.url.contains(x, y) {
                return Hit::UrlBar;
            }
            for (i, r) in self.tabs.iter().enumerate() {
                if r.contains(x, y) {
                    if close_rect(*r, self.scale).contains(x, y) {
                        return Hit::TabClose(i);
                    }
                    return Hit::Tab(i);
                }
            }
            if self.new_tab.contains(x, y) {
                return Hit::NewTab;
            }
        }
        if self.content.contains(x, y) {
            return Hit::Content;
        }
        if self.mode == SidebarMode::Hover && x < self.content.x && !self.over_sidebar(x) {
            return Hit::Edge;
        }
        // Freie Flächen der Seitenleiste und der Rand verschieben das Fenster.
        Hit::Drag
    }
}

/// Schließen-Knopf rechts in einer Tab-Zeile.
fn close_rect(tab: Rect, s: f32) -> Rect {
    let size = 24.0 * s;
    Rect { x: tab.x + tab.w - size - 6.0 * s, y: tab.y + (tab.h - size) / 2.0, w: size, h: size }
}

/// Einzeiliges Eingabefeld (Adressleiste).
#[derive(Default)]
pub struct TextField {
    pub text: String,
    /// Cursor als Byte-Index in `text`.
    pub cursor: usize,
    /// Anfang der Auswahl; die Auswahl geht von hier bis `cursor`.
    pub anchor: Option<usize>,
}

impl TextField {
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
        self.anchor = None;
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = self.text.len();
    }

    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?;
        (a != self.cursor).then(|| (a.min(self.cursor), a.max(self.cursor)))
    }

    pub fn selected_text(&self) -> Option<&str> {
        self.selection().map(|(a, b)| &self.text[a..b])
    }

    fn delete_selection(&mut self) -> bool {
        if let Some((a, b)) = self.selection() {
            self.text.replace_range(a..b, "");
            self.cursor = a;
            self.anchor = None;
            true
        } else {
            self.anchor = None;
            false
        }
    }

    pub fn insert(&mut self, s: &str) {
        self.delete_selection();
        let s: String = s.chars().filter(|c| !c.is_control()).collect();
        self.text.insert_str(self.cursor, &s);
        self.cursor += s.len();
    }

    fn prev(&self, i: usize) -> usize {
        self.text[..i].char_indices().next_back().map(|(j, _)| j).unwrap_or(0)
    }

    fn next(&self, i: usize) -> usize {
        self.text[i..].chars().next().map(|c| i + c.len_utf8()).unwrap_or(i)
    }

    pub fn backspace(&mut self) {
        if !self.delete_selection() && self.cursor > 0 {
            let p = self.prev(self.cursor);
            self.text.replace_range(p..self.cursor, "");
            self.cursor = p;
        }
    }

    pub fn delete(&mut self) {
        if !self.delete_selection() && self.cursor < self.text.len() {
            let n = self.next(self.cursor);
            self.text.replace_range(self.cursor..n, "");
        }
    }

    /// Bewegt den Cursor; mit `select` wird die Auswahl erweitert.
    pub fn move_to(&mut self, to: usize, select: bool) {
        if select {
            self.anchor.get_or_insert(self.cursor);
        } else {
            self.anchor = None;
        }
        self.cursor = to.min(self.text.len());
    }

    pub fn left(&mut self, select: bool) {
        let to = match (select, self.selection()) {
            (false, Some((a, _))) => a,
            _ => self.prev(self.cursor),
        };
        self.move_to(to, select);
    }

    pub fn right(&mut self, select: bool) {
        let to = match (select, self.selection()) {
            (false, Some((_, b))) => b,
            _ => self.next(self.cursor),
        };
        self.move_to(to, select);
    }
}

/// Zählt gezeichnete Bilder der letzten Sekunde.
#[derive(Default)]
pub struct FpsCounter {
    frames: std::collections::VecDeque<Instant>,
}

impl FpsCounter {
    pub fn tick(&mut self) {
        let now = Instant::now();
        self.frames.push_back(now);
        self.trim(now);
    }

    fn trim(&mut self, now: Instant) {
        while self.frames.front().is_some_and(|t| now.duration_since(*t) > Duration::from_secs(1)) {
            self.frames.pop_front();
        }
    }

    pub fn fps(&mut self) -> usize {
        self.trim(Instant::now());
        self.frames.len()
    }
}

/// Text, der nur neu gesetzt wird, wenn er sich ändert.
struct Label {
    buffer: Buffer,
    text: String,
    key: (u32, bool),
}

impl Label {
    fn new(fs: &mut FontSystem) -> Self {
        let mut buffer = Buffer::new(fs, Metrics::new(14.0, 18.0));
        buffer.set_wrap(Wrap::None);
        Self { buffer, text: "\u{0}".into(), key: (0, false) }
    }

    fn set(&mut self, fs: &mut FontSystem, text: &str, size: f32, family: &str, bold: bool) {
        let key = (size.to_bits(), bold);
        if self.text == text && self.key == key {
            return;
        }
        self.text = text.to_string();
        self.key = key;
        self.buffer.set_metrics(Metrics::new(size, (size * 1.3).round()));
        self.buffer.set_size(None, None);
        let weight = if bold { Weight::SEMIBOLD } else { Weight::NORMAL };
        let attrs = Attrs::new().family(Family::Name(family)).weight(weight);
        self.buffer.set_text(text, &attrs, Shaping::Advanced, None);
        self.buffer.shape_until_scroll(fs, false);
    }

    fn width(&self) -> f32 {
        self.buffer.layout_runs().map(|r| r.line_w).fold(0.0, f32::max)
    }

    fn line_height(&self) -> f32 {
        self.buffer.metrics().line_height
    }

    /// x-Position vor dem Byte `idx`.
    fn x_at(&self, idx: usize) -> f32 {
        let Some(run) = self.buffer.layout_runs().next() else { return 0.0 };
        for g in run.glyphs {
            if idx <= g.start {
                return g.x;
            }
        }
        run.line_w
    }

    /// Byte-Index, der der x-Position am nächsten liegt.
    fn index_at(&self, x: f32) -> usize {
        let Some(run) = self.buffer.layout_runs().next() else { return 0 };
        for g in run.glyphs {
            if x < g.x + g.w / 2.0 {
                return g.start;
            }
        }
        self.text.len()
    }
}

// Symbole aus "Segoe Fluent Icons" (Windows 11).
const ICON_BACK: &str = "\u{E72B}";
const ICON_FORWARD: &str = "\u{E72A}";
const ICON_RELOAD: &str = "\u{E72C}";
const ICON_STOP: &str = "\u{E711}";
const ICON_PANE_OPEN: &str = "\u{E8A0}";
const ICON_PANE_CLOSE: &str = "\u{E89F}";
const ICON_MIN: &str = "\u{E921}";
const ICON_MAX: &str = "\u{E922}";
const ICON_RESTORE: &str = "\u{E923}";
const ICON_CLOSE: &str = "\u{E8BB}";
const ICON_LOCK: &str = "\u{E72E}";
const ICON_GLOBE: &str = "\u{E774}";
const ICON_ADD: &str = "\u{E710}";

/// Text und Symbol einer Tab-Zeile.
struct TabLabels {
    icon: Label,
    title: Label,
    close: Label,
}

pub struct Ui {
    pub url: TextField,
    pub url_focused: bool,
    pub hover: Hit,
    pub pressed: Option<Hit>,
    /// Hover-Animation je Knopf (0..1); Einträge bei 0 werden entfernt.
    anims: Vec<(Hit, f32)>,
    last_anim: Instant,
    /// Lade-Fortschritt 0..1 für den Balken (nur Anzeige).
    load_anim: f32,
    ui_family: String,
    icon_family: String,
    top_icons: Vec<Label>,
    url_icon: Label,
    url_label: Label,
    tabs_label: Label,
    tab_labels: Vec<TabLabels>,
    new_tab_icon: Label,
    new_tab_label: Label,
    fps_label: Label,
    /// Horizontales Verschieben des URL-Textes, damit der Cursor sichtbar bleibt.
    url_scroll: f32,
    pub show_fps: bool,
}

/// Was gezeichnet werden soll.
pub struct Frame<'a> {
    pub cards: Vec<RoundRect>,
    pub under: Vec<Quad>,
    pub text: Vec<TextBlock<'a>>,
    pub over: Vec<Quad>,
}

pub struct Stats {
    pub ui_fps: usize,
    pub web_fps: usize,
    pub refresh_hz: Option<f32>,
    pub bench: bool,
    pub maximized: bool,
}

fn has_family(fs: &FontSystem, name: &str) -> bool {
    fs.db()
        .faces()
        .any(|f| f.families.iter().any(|(fam, _)| fam.eq_ignore_ascii_case(name)))
}

/// Was die Oberfläche über die Tabs wissen muss.
pub struct TabsView<'a> {
    pub tabs: &'a [PageState],
    pub active: usize,
}

impl Ui {
    pub fn new(fs: &mut FontSystem) -> Self {
        let ui_family = ["Segoe UI Variable Text", "Segoe UI", "Noto Sans", "DejaVu Sans"]
            .into_iter()
            .find(|f| has_family(fs, f))
            .unwrap_or("sans-serif")
            .to_string();
        let icon_family = ["Segoe Fluent Icons", "Segoe MDL2 Assets"]
            .into_iter()
            .find(|f| has_family(fs, f))
            .unwrap_or("Segoe MDL2 Assets")
            .to_string();
        log::info!("UI-Schrift: {ui_family}, Symbole: {icon_family}");
        Self {
            url: TextField::default(),
            url_focused: false,
            hover: Hit::Nothing,
            pressed: None,
            anims: Vec::new(),
            last_anim: Instant::now(),
            load_anim: 0.0,
            top_icons: (0..TOP.len()).map(|_| Label::new(fs)).collect(),
            url_icon: Label::new(fs),
            url_label: Label::new(fs),
            tabs_label: Label::new(fs),
            tab_labels: Vec::new(),
            new_tab_icon: Label::new(fs),
            new_tab_label: Label::new(fs),
            fps_label: Label::new(fs),
            ui_family,
            icon_family,
            url_scroll: 0.0,
            show_fps: true,
        }
    }

    /// Hover-Wert (0..1) eines Knopfes.
    pub fn anim(&self, hit: Hit) -> f32 {
        self.anims.iter().find(|(h, _)| *h == hit).map(|(_, v)| *v).unwrap_or(0.0)
    }

    /// Der Knopf, über dem die Maus ist; über dem Schließen-Kreuz zählt auch die Tab-Zeile.
    fn hovered(&self, hit: Hit) -> bool {
        self.hover == hit || matches!((self.hover, hit), (Hit::TabClose(a), Hit::Tab(b)) if a == b)
    }

    /// Schreitet die Animationen voran. `true`, solange noch etwas animiert.
    pub fn animate(&mut self, page: &PageState) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_anim).as_secs_f32().min(0.1);
        self.last_anim = now;
        // Dezent: etwa 90 ms bis zum Ziel, exponentiell geglättet.
        let k = 1.0 - (-dt / 0.035).exp();
        let mut targets = vec![self.hover];
        if let Hit::TabClose(i) = self.hover {
            targets.push(Hit::Tab(i));
        }
        for t in &targets {
            if !matches!(t, Hit::Nothing | Hit::Drag | Hit::Content | Hit::Edge | Hit::Resize(_))
                && !self.anims.iter().any(|(h, _)| h == t)
            {
                self.anims.push((*t, 0.0));
            }
        }
        let mut moving = false;
        for (h, v) in &mut self.anims {
            let target = if targets.contains(h) { 1.0 } else { 0.0 };
            *v += (target - *v) * k;
            if (target - *v).abs() < 0.01 {
                *v = target;
            } else {
                moving = true;
            }
        }
        self.anims.retain(|(h, v)| *v > 0.0 || targets.contains(h));
        if page.loading {
            // Läuft langsam auf 90 % zu, bis die Seite fertig ist.
            self.load_anim += (0.9 - self.load_anim) * (1.0 - (-dt / 1.2).exp());
            moving = true;
        } else if self.load_anim > 0.0 {
            self.load_anim = 0.0;
        }
        moving
    }

    /// Text, den die Adressleiste ohne Fokus zeigt.
    pub fn display_url(page: &PageState) -> String {
        match &page.url {
            Some(u) if u.scheme() == "about" => String::new(),
            Some(u) => u.as_str().to_string(),
            None => String::new(),
        }
    }

    /// Setzt den Cursor in der Adressleiste an die Klickposition.
    pub fn click_url(&mut self, x: f32, layout: &Layout, select: bool) {
        let r = layout.url;
        let text_x = r.x + 34.0 * layout.scale - self.url_scroll;
        let idx = self.url_label.index_at(x - text_x);
        self.url.move_to(idx, select);
    }

    pub fn build<'a>(
        &'a mut self,
        fs: &mut FontSystem,
        layout: &Layout,
        tabs: &TabsView<'_>,
        stats: &Stats,
    ) -> Frame<'a> {
        let s = layout.scale;
        let t = &THEME;
        let page = tabs.tabs.get(tabs.active).cloned().unwrap_or_default();
        let mut cards = Vec::new();
        let mut under = Vec::new();
        let mut over = Vec::new();
        // Alles in der Seitenleiste wird auf ihren sichtbaren Teil beschnitten.
        let side_clip = [0, 0, (layout.sidebar_x + layout.sidebar_w).max(0.0) as i32, layout.height as i32];

        // Obere Zeile: Hintergrund beim Hovern weich einblenden.
        let icons = [
            ICON_BACK,
            ICON_FORWARD,
            if page.loading { ICON_STOP } else { ICON_RELOAD },
            if layout.mode == SidebarMode::Pinned { ICON_PANE_CLOSE } else { ICON_PANE_OPEN },
            ICON_MIN,
            if stats.maximized { ICON_RESTORE } else { ICON_MAX },
            ICON_CLOSE,
        ];
        for (i, icon) in icons.iter().enumerate() {
            let r = layout.top[i];
            let a = self.anim(TOP[i]);
            let pressed = self.pressed == Some(TOP[i]) && self.hover == TOP[i];
            if a > 0.0 {
                let base = if TOP[i] == Hit::Close { t.close } else { t.hover };
                let alpha = (a * if pressed { 200.0 } else { 255.0 }) as u8;
                cards.push(RoundRect::new(r.x, r.y, r.w, r.h, base.with_alpha(alpha), 7.0 * s));
            }
            self.top_icons[i].set(fs, icon, 11.0 * s, &self.icon_family, false);
        }

        // Adressleiste.
        let ur = layout.url;
        let ua = self.anim(Hit::UrlBar);
        let url_bg = if self.url_focused { t.hover } else { lerp(t.surface, t.hover, ua * 0.6) };
        cards.push(RoundRect::new(ur.x, ur.y, ur.w, ur.h, url_bg, 9.0 * s));
        if self.url_focused {
            cards.push(RoundRect::outline(ur.x, ur.y, ur.w, ur.h, t.accent.with_alpha(170), 9.0 * s, 1.5 * s));
        }
        let secure = page.url.as_ref().is_some_and(|u| u.scheme() == "https");
        self.url_icon.set(fs, if secure { ICON_LOCK } else { ICON_GLOBE }, 11.0 * s, &self.icon_family, false);

        let (url_text, url_color) = if self.url_focused {
            (self.url.text.clone(), t.text)
        } else {
            let shown = Self::display_url(&page);
            if shown.is_empty() {
                ("Suchen oder Adresse eingeben".to_string(), t.dim)
            } else {
                (pretty_url(&shown), t.text)
            }
        };
        self.url_label.set(fs, &url_text, 14.0 * s, &self.ui_family, false);
        let text_left = ur.x + 34.0 * s;
        let text_w = ur.w - 34.0 * s - 10.0 * s;
        if self.url_focused {
            let cx = self.url_label.x_at(self.url.cursor);
            if cx - self.url_scroll > text_w {
                self.url_scroll = cx - text_w;
            } else if cx < self.url_scroll {
                self.url_scroll = cx;
            }
        } else {
            self.url_scroll = 0.0;
        }
        let url_y = ur.y + (ur.h - self.url_label.line_height()) / 2.0;
        if self.url_focused {
            if let Some((a, b)) = self.url.selection() {
                let x0 = (text_left + self.url_label.x_at(a) - self.url_scroll).max(text_left);
                let x1 = (text_left + self.url_label.x_at(b) - self.url_scroll).min(text_left + text_w);
                under.push(Quad { x: x0, y: url_y, w: (x1 - x0).max(0.0), h: self.url_label.line_height(), color: t.selection });
            } else {
                let cx = text_left + self.url_label.x_at(self.url.cursor) - self.url_scroll;
                over.push(Quad { x: cx.round(), y: url_y + 1.0 * s, w: (1.5 * s).max(1.0), h: self.url_label.line_height() - 2.0 * s, color: t.accent });
            }
        }
        // Ladebalken unten in der Adressleiste.
        if page.loading {
            let w = (ur.w - 18.0 * s) * self.load_anim.max(0.08);
            cards.push(RoundRect::new(ur.x + 9.0 * s, ur.y + ur.h - 3.0 * s, w, 2.0 * s, t.accent, 1.0 * s));
        }

        // Tab-Liste.
        self.tabs_label.set(fs, "TABS", 11.0 * s, &self.ui_family, true);
        while self.tab_labels.len() < tabs.tabs.len() {
            self.tab_labels.push(TabLabels { icon: Label::new(fs), title: Label::new(fs), close: Label::new(fs) });
        }
        self.tab_labels.truncate(tabs.tabs.len().max(1));
        for (i, state) in tabs.tabs.iter().enumerate() {
            let r = layout.tabs[i];
            let a = self.anim(Hit::Tab(i));
            let bg = if i == tabs.active { t.active } else { lerp(t.bg, t.hover, a * 0.8) };
            if i == tabs.active || a > 0.0 {
                cards.push(RoundRect::new(r.x, r.y, r.w, r.h, bg, 9.0 * s));
            }
            if self.hovered(Hit::Tab(i)) || i == tabs.active {
                let c = close_rect(r, s);
                let ca = self.anim(Hit::TabClose(i));
                if ca > 0.0 {
                    cards.push(RoundRect::new(c.x, c.y, c.w, c.h, t.hover.with_alpha((ca * 255.0) as u8), 6.0 * s));
                }
            }
            let labels = &mut self.tab_labels[i];
            labels.icon.set(fs, ICON_GLOBE, 11.0 * s, &self.icon_family, false);
            labels.title.set(fs, &tab_title(state), 13.5 * s, &self.ui_family, i == tabs.active);
            labels.close.set(fs, ICON_CLOSE, 8.5 * s, &self.icon_family, false);
            if state.loading {
                // Kleiner Punkt statt Symbol, solange der Tab lädt.
                let d = 6.0 * s;
                cards.push(RoundRect::new(r.x + 15.0 * s, r.y + (r.h - d) / 2.0, d, d, t.accent, d / 2.0));
            }
        }
        let na = self.anim(Hit::NewTab);
        let nr = layout.new_tab;
        if na > 0.0 {
            cards.push(RoundRect::new(nr.x, nr.y, nr.w, nr.h, t.hover.with_alpha((na * 200.0) as u8), 9.0 * s));
        }
        self.new_tab_icon.set(fs, ICON_ADD, 11.0 * s, &self.icon_family, false);
        self.new_tab_label.set(fs, "Neuer Tab", 13.5 * s, &self.ui_family, false);

        // FPS-Anzeige.
        if self.show_fps {
            let hz = stats.refresh_hz.map(|h| format!("{h:.0} Hz")).unwrap_or_else(|| "? Hz".into());
            let mode = if stats.bench { "  ·  Dauerlauf (F2)" } else { "" };
            let line = format!("Monitor {hz}{mode}\nUI {} fps  ·  Web {} fps", stats.ui_fps, stats.web_fps);
            self.fps_label.set(fs, &line, 11.5 * s, &self.ui_family, false);
        }

        // Text zusammenstellen.
        let mut text = Vec::new();
        let centered = |l: &'a Label, r: Rect, color: Rgba| TextBlock {
            buffer: &l.buffer,
            x: (r.x + (r.w - l.width()) / 2.0).round(),
            y: (r.y + (r.h - l.line_height()) / 2.0).round(),
            color,
            clip: Some(side_clip),
        };
        for i in 0..TOP.len() {
            let disabled = (TOP[i] == Hit::Back && !page.can_back) || (TOP[i] == Hit::Forward && !page.can_forward);
            let color = if TOP[i] == Hit::Close && self.anim(Hit::Close) > 0.5 {
                Rgba::rgb(255, 255, 255)
            } else if disabled {
                t.dim.with_alpha(120)
            } else {
                t.text
            };
            text.push(centered(&self.top_icons[i], layout.top[i], color));
        }
        let left_icon = |l: &'a Label, r: Rect, color: Rgba| TextBlock {
            buffer: &l.buffer,
            x: (r.x + 12.0 * s).round(),
            y: (r.y + (r.h - l.line_height()) / 2.0).round(),
            color,
            clip: Some(side_clip),
        };
        text.push(left_icon(&self.url_icon, ur, if secure { t.dim } else { t.dim.with_alpha(160) }));
        text.push(TextBlock {
            buffer: &self.url_label.buffer,
            x: text_left - self.url_scroll,
            y: url_y.round(),
            color: url_color,
            clip: Some([text_left.max(0.0) as i32, ur.y as i32, (text_left + text_w).max(0.0) as i32, (ur.y + ur.h) as i32]),
        });
        let lr = layout.tabs_label;
        text.push(TextBlock { buffer: &self.tabs_label.buffer, x: lr.x, y: lr.y, color: t.dim, clip: Some(side_clip) });
        for (i, state) in tabs.tabs.iter().enumerate() {
            let r = layout.tabs[i];
            let labels = &self.tab_labels[i];
            if !state.loading {
                text.push(left_icon(&labels.icon, r, t.dim));
            }
            let show_close = self.hovered(Hit::Tab(i)) || i == tabs.active;
            let title_right = if show_close { close_rect(r, s).x - 4.0 * s } else { r.x + r.w - 8.0 * s };
            text.push(TextBlock {
                buffer: &labels.title.buffer,
                x: (r.x + 34.0 * s).round(),
                y: (r.y + (r.h - labels.title.line_height()) / 2.0).round(),
                color: if i == tabs.active { t.text } else { lerp(t.dim, t.text, 0.6) },
                clip: Some([r.x.max(0.0) as i32, r.y as i32, title_right.min(side_clip[2] as f32).max(0.0) as i32, (r.y + r.h) as i32]),
            });
            if show_close {
                text.push(centered(&labels.close, close_rect(r, s), t.dim));
            }
        }
        text.push(left_icon(&self.new_tab_icon, nr, t.dim));
        text.push(TextBlock {
            buffer: &self.new_tab_label.buffer,
            x: (nr.x + 34.0 * s).round(),
            y: (nr.y + (nr.h - self.new_tab_label.line_height()) / 2.0).round(),
            color: t.dim,
            clip: Some(side_clip),
        });
        if self.show_fps && layout.reveal > 0.0 {
            let fr = layout.fps;
            text.push(TextBlock { buffer: &self.fps_label.buffer, x: fr.x, y: fr.y, color: t.dim, clip: Some(side_clip) });
        }
        Frame { cards, under, text, over }
    }
}

/// Titel eines Tabs: Seitentitel, sonst Domain, sonst "Neuer Tab".
pub fn tab_title(state: &PageState) -> String {
    state
        .title
        .clone()
        .filter(|t| !t.trim().is_empty())
        .or_else(|| state.url.as_ref().and_then(|u| u.host_str().map(str::to_string)))
        .unwrap_or_else(|| "Neuer Tab".into())
}

fn lerp(a: Rgba, b: Rgba, t: f32) -> Rgba {
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t.clamp(0.0, 1.0)).round() as u8;
    Rgba(m(a.0, b.0), m(a.1, b.1), m(a.2, b.2), m(a.3, b.3))
}

/// Zeigt die Adresse ohne "https://" und abschließendes "/", wie in modernen Browsern.
fn pretty_url(url: &str) -> String {
    let s = url.strip_prefix("https://").unwrap_or(url);
    let s = s.strip_prefix("www.").unwrap_or(s);
    s.strip_suffix('/').unwrap_or(s).to_string()
}

/// Macht aus einer Eingabe in der Adressleiste eine URL: Adresse oder Suche (mit !Bangs).
pub fn resolve_input(input: &str) -> Option<url::Url> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let looks_like_url = !input.contains(' ')
        && (input.contains("://")
            || input.starts_with("localhost")
            || input.starts_with("about:")
            || (input.contains('.') && !input.starts_with('!') && !input.ends_with('.')));
    if looks_like_url {
        if let Ok(u) = url::Url::parse(input) {
            if u.scheme().len() > 1 && (u.has_host() || u.scheme() == "about" || u.scheme() == "file") {
                return Some(u);
            }
        }
        // Lokale Entwicklungsserver laufen meist ohne HTTPS.
        let scheme = if input.starts_with("localhost") || input.starts_with("127.0.0.1") { "http" } else { "https" };
        if let Ok(u) = url::Url::parse(&format!("{scheme}://{input}")) {
            return Some(u);
        }
    }
    // Suche. DuckDuckGo löst !Bangs selbst auf; die Auswahl der Suchmaschine beim ersten Start
    // und lokale !Bangs kommen in einem späteren Meilenstein.
    let mut u = url::Url::parse("https://duckduckgo.com/").ok()?;
    u.query_pairs_mut().append_pair("q", input);
    Some(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eingaben_werden_zu_urls() {
        assert_eq!(resolve_input("servo.org").unwrap().as_str(), "https://servo.org/");
        assert_eq!(resolve_input("https://a.b/c").unwrap().as_str(), "https://a.b/c");
        assert_eq!(resolve_input("localhost:8080").unwrap().as_str(), "http://localhost:8080/");
        assert!(resolve_input("rust lernen").unwrap().as_str().starts_with("https://duckduckgo.com/?q=rust+lernen"));
        assert!(resolve_input("!yt katzen").unwrap().as_str().contains("q=%21yt+katzen"));
        assert!(resolve_input("   ").is_none());
    }

    #[test]
    fn textfeld_bearbeiten() {
        let mut f = TextField::default();
        f.insert("hällo");
        f.left(false);
        f.backspace();
        assert_eq!(f.text, "hälo");
        f.select_all();
        f.insert("x");
        assert_eq!(f.text, "x");
    }

    #[test]
    fn seitenleiste_faehrt_aus() {
        let tabs = 3;
        let pinned = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 0.0, tabs);
        assert_eq!(pinned.sidebar_x, 0.0);
        assert_eq!(pinned.content.x, SIDEBAR_W);
        assert_eq!(pinned.hit(100.0, pinned.tabs[1].y + 5.0, false), Hit::Tab(1));

        let hidden = Layout::new(1600, 900, 1.0, SidebarMode::Hover, 0.0, tabs);
        assert_eq!(hidden.sidebar_x, -SIDEBAR_W);
        assert_eq!(hidden.content.x, GAP);
        assert_eq!(hidden.hit(7.0, 400.0, false), Hit::Edge);
        assert_eq!(hidden.hit(100.0, hidden.tabs[1].y + 5.0, false), Hit::Content);

        // Die Seite behält im Hover-Modus ihre Breite, sie wird nur verschoben.
        let shown = Layout::new(1600, 900, 1.0, SidebarMode::Hover, 1.0, tabs);
        assert_eq!(shown.content.w, hidden.content.w);
        assert_eq!(shown.content.x, SIDEBAR_W);
        assert_eq!(shown.hit(100.0, shown.tabs[1].y + 5.0, false), Hit::Tab(1));
    }

    #[test]
    fn tab_schliessen_knopf() {
        let l = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 1.0, 2);
        let c = close_rect(l.tabs[0], 1.0);
        assert_eq!(l.hit(c.x + 2.0, c.y + 2.0, false), Hit::TabClose(0));
        assert_eq!(l.hit(l.new_tab.x + 5.0, l.new_tab.y + 5.0, false), Hit::NewTab);
    }
}
