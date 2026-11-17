//! Die Browser-Oberfläche: Seitenleiste (Navigation, Adressleiste, Tabs, Fensterknöpfe),
//! Hover-Animationen und die FPS-Anzeige. Alles in physischen Pixeln, skaliert mit `scale`.

use std::time::{Duration, Instant};

use winit::window::ResizeDirection;

use crate::engine::PageState;
use crate::render::{Attrs, Buffer, Family, FontSystem, Metrics, Quad, RoundRect, Rgba, Shaping, TextBlock, Weight, Wrap};

/// Farben der Oberfläche (dunkel, violetter Akzent).
pub struct Theme {
    pub bg: Rgba,
    pub surface: Rgba,
    pub hover: Rgba,
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
    Minimize,
    Maximize,
    Close,
    UrlBar,
    Tab,
    Drag,
    Resize(ResizeDirection),
    Content,
    Nothing,
}

/// Knöpfe mit Hover-Animation, in dieser Reihenfolge in `Ui::hover_anim`.
const BUTTONS: [Hit; 8] = [
    Hit::Back,
    Hit::Forward,
    Hit::Reload,
    Hit::Minimize,
    Hit::Maximize,
    Hit::Close,
    Hit::UrlBar,
    Hit::Tab,
];

/// Positionen aller Bereiche für die aktuelle Fenstergröße.
pub struct Layout {
    pub scale: f32,
    pub width: f32,
    pub height: f32,
    pub buttons: [Rect; 8],
    pub drag: Rect,
    pub content: Rect,
    pub fps: Rect,
    pub tabs_label: Rect,
}

impl Layout {
    pub fn new(width: u32, height: u32, scale: f32) -> Self {
        let s = scale;
        let (w, h) = (width as f32, height as f32);
        let sw = (SIDEBAR_W * s).round();
        let btn = 30.0 * s;
        let top = 9.0 * s;
        let pad = 10.0 * s;
        let nav = |i: f32| Rect { x: pad + i * (btn + 2.0 * s), y: top, w: btn, h: btn };
        let ctl_w = 38.0 * s;
        let ctl = |i: f32| Rect { x: sw - pad - (3.0 - i) * ctl_w + 4.0 * s, y: top, w: ctl_w, h: btn };
        let url = Rect { x: pad, y: top + btn + 8.0 * s, w: sw - 2.0 * pad, h: 36.0 * s };
        let tabs_label = Rect { x: pad + 6.0 * s, y: url.y + url.h + 16.0 * s, w: sw - 2.0 * pad, h: 18.0 * s };
        let tab = Rect { x: pad, y: tabs_label.y + tabs_label.h + 4.0 * s, w: sw - 2.0 * pad, h: 36.0 * s };
        let nav_end = nav(3.0).x;
        let drag = Rect { x: nav_end, y: 0.0, w: (ctl(0.0).x - nav_end).max(0.0), h: top + btn };
        let gap = (GAP * s).round();
        let content = Rect { x: sw, y: gap, w: (w - sw - gap).max(1.0), h: (h - 2.0 * gap).max(1.0) };
        let fps = Rect { x: pad + 4.0 * s, y: h - 46.0 * s, w: sw - 2.0 * pad, h: 40.0 * s };
        Self {
            scale,
            width: w,
            height: h,
            buttons: [nav(0.0), nav(1.0), nav(2.0), ctl(0.0), ctl(1.0), ctl(2.0), url, tab],
            drag,
            content,
            fps,
            tabs_label,
        }
    }

    pub fn rect(&self, hit: Hit) -> Rect {
        BUTTONS.iter().position(|b| *b == hit).map(|i| self.buttons[i]).unwrap_or_default()
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
        for (i, r) in self.buttons.iter().enumerate() {
            if r.contains(x, y) {
                return BUTTONS[i];
            }
        }
        if self.content.contains(x, y) {
            return Hit::Content;
        }
        if self.drag.contains(x, y) || x < self.content.x || y < self.content.y {
            // Freie Flächen der Seitenleiste und der Rand verschieben das Fenster.
            return Hit::Drag;
        }
        Hit::Nothing
    }
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
    fn new(fs: &mut FontSystem, size: f32) -> Self {
        let mut buffer = Buffer::new(fs, Metrics::new(size, size * 1.3));
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
const ICON_MIN: &str = "\u{E921}";
const ICON_MAX: &str = "\u{E922}";
const ICON_RESTORE: &str = "\u{E923}";
const ICON_CLOSE: &str = "\u{E8BB}";
const ICON_LOCK: &str = "\u{E72E}";
const ICON_GLOBE: &str = "\u{E774}";

pub struct Ui {
    pub url: TextField,
    pub url_focused: bool,
    pub hover: Hit,
    pub pressed: Option<Hit>,
    hover_anim: [f32; 8],
    last_anim: Instant,
    /// Lade-Fortschritt 0..1 für den Balken (nur Anzeige).
    load_anim: f32,
    ui_family: String,
    icon_family: String,
    icons: Vec<Label>,
    url_label: Label,
    tabs_label: Label,
    tab_label: Label,
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
            hover_anim: [0.0; 8],
            last_anim: Instant::now(),
            load_anim: 0.0,
            icons: (0..8).map(|_| Label::new(fs, 12.0)).collect(),
            url_label: Label::new(fs, 14.0),
            tabs_label: Label::new(fs, 12.0),
            tab_label: Label::new(fs, 14.0),
            fps_label: Label::new(fs, 12.0),
            ui_family,
            icon_family,
            url_scroll: 0.0,
            show_fps: true,
        }
    }

    /// Schreitet die Animationen voran. `true`, solange noch etwas animiert.
    pub fn animate(&mut self, page: &PageState) -> bool {
        let now = Instant::now();
        let dt = now.duration_since(self.last_anim).as_secs_f32().min(0.1);
        self.last_anim = now;
        // Dezent: etwa 90 ms bis zum Ziel, exponentiell geglättet.
        let k = 1.0 - (-dt / 0.035).exp();
        let mut moving = false;
        for (i, b) in BUTTONS.iter().enumerate() {
            let target = if self.hover == *b { 1.0 } else { 0.0 };
            let v = &mut self.hover_anim[i];
            *v += (target - *v) * k;
            if (target - *v).abs() < 0.01 {
                *v = target;
            } else {
                moving = true;
            }
        }
        if page.loading {
            // Läuft langsam auf 90 % zu, bis die Seite fertig ist.
            self.load_anim += (0.9 - self.load_anim) * (1.0 - (-dt / 1.2).exp());
            moving = true;
        } else if self.load_anim > 0.0 {
            self.load_anim = 0.0;
        }
        moving
    }

    pub fn hover_index(&self, hit: Hit) -> f32 {
        BUTTONS.iter().position(|b| *b == hit).map(|i| self.hover_anim[i]).unwrap_or(0.0)
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
        let r = layout.rect(Hit::UrlBar);
        let text_x = r.x + 34.0 * layout.scale - self.url_scroll;
        let idx = self.url_label.index_at(x - text_x);
        self.url.move_to(idx, select);
    }

    pub fn build<'a>(
        &'a mut self,
        fs: &mut FontSystem,
        layout: &Layout,
        page: &PageState,
        stats: &Stats,
    ) -> Frame<'a> {
        let s = layout.scale;
        let t = &THEME;
        let mut cards = Vec::new();
        let mut under = Vec::new();
        let mut over = Vec::new();

        // Knöpfe: Hintergrund beim Hovern weich einblenden.
        let icons = [
            ICON_BACK,
            ICON_FORWARD,
            if page.loading { ICON_STOP } else { ICON_RELOAD },
            ICON_MIN,
            if stats.maximized { ICON_RESTORE } else { ICON_MAX },
            ICON_CLOSE,
        ];
        for (i, icon) in icons.iter().enumerate() {
            let r = layout.buttons[i];
            let a = self.hover_anim[i];
            let pressed = self.pressed == Some(BUTTONS[i]) && self.hover == BUTTONS[i];
            if a > 0.0 {
                let base = if BUTTONS[i] == Hit::Close { t.close } else { t.hover };
                let alpha = (a * if pressed { 200.0 } else { 255.0 }) as u8;
                cards.push(RoundRect::new(r.x, r.y, r.w, r.h, base.with_alpha(alpha), 7.0 * s));
            }
            self.icons[i].set(fs, icon, 11.0 * s, &self.icon_family, false);
        }

        // Adressleiste.
        let ur = layout.rect(Hit::UrlBar);
        let ua = self.hover_index(Hit::UrlBar);
        let url_bg = if self.url_focused { t.hover } else { lerp(t.surface, t.hover, ua * 0.6) };
        cards.push(RoundRect::new(ur.x, ur.y, ur.w, ur.h, url_bg, 9.0 * s));
        if self.url_focused {
            cards.push(RoundRect::outline(ur.x, ur.y, ur.w, ur.h, t.accent.with_alpha(170), 9.0 * s, 1.5 * s));
        }
        let secure = page.url.as_ref().is_some_and(|u| u.scheme() == "https");
        self.icons[6].set(fs, if secure { ICON_LOCK } else { ICON_GLOBE }, 11.0 * s, &self.icon_family, false);

        let (url_text, url_color) = if self.url_focused {
            (self.url.text.clone(), t.text)
        } else {
            let shown = Self::display_url(page);
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
                let x0 = text_left + self.url_label.x_at(a) - self.url_scroll;
                let x1 = text_left + self.url_label.x_at(b) - self.url_scroll;
                let x0 = x0.max(text_left);
                let x1 = x1.min(text_left + text_w);
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

        // Tabs.
        self.tabs_label.set(fs, "TABS", 11.0 * s, &self.ui_family, true);
        let tr = layout.rect(Hit::Tab);
        let ta = self.hover_index(Hit::Tab);
        cards.push(RoundRect::new(tr.x, tr.y, tr.w, tr.h, lerp(t.surface, t.hover, ta * 0.5), 9.0 * s));
        let title = page
            .title
            .clone()
            .filter(|t| !t.trim().is_empty())
            .or_else(|| page.url.as_ref().and_then(|u| u.host_str().map(str::to_string)))
            .unwrap_or_else(|| "Neuer Tab".into());
        self.tab_label.set(fs, &ellipsize(&title, 30), 13.5 * s, &self.ui_family, false);
        self.icons[7].set(fs, ICON_GLOBE, 11.0 * s, &self.icon_family, false);

        // FPS-Anzeige.
        if self.show_fps {
            let hz = stats
                .refresh_hz
                .map(|h| format!("{h:.0} Hz"))
                .unwrap_or_else(|| "? Hz".into());
            let mode = if stats.bench { "  ·  Dauerlauf (F2)" } else { "" };
            let line = format!("Monitor {hz}{mode}\nUI {} fps  ·  Web {} fps", stats.ui_fps, stats.web_fps);
            self.fps_label.set(fs, &line, 11.5 * s, &self.ui_family, false);
        }

        // Text zusammenstellen.
        let mut text = Vec::new();
        let full = [0, 0, layout.width as i32, layout.height as i32];
        for i in 0..6 {
            let r = layout.buttons[i];
            let l = &self.icons[i];
            let disabled = (i == 0 && !page.can_back) || (i == 1 && !page.can_forward);
            let color = if i == 5 && self.hover_anim[5] > 0.5 { Rgba::rgb(255, 255, 255) } else if disabled { t.dim.with_alpha(120) } else { t.text };
            text.push(TextBlock {
                buffer: &l.buffer,
                x: (r.x + (r.w - l.width()) / 2.0).round(),
                y: (r.y + (r.h - l.line_height()) / 2.0).round(),
                color,
                clip: Some(full),
            });
        }
        let icon = &self.icons[6];
        text.push(TextBlock {
            buffer: &icon.buffer,
            x: (ur.x + 12.0 * s).round(),
            y: (ur.y + (ur.h - icon.line_height()) / 2.0).round(),
            color: if secure { t.dim } else { t.dim.with_alpha(160) },
            clip: Some(full),
        });
        text.push(TextBlock {
            buffer: &self.url_label.buffer,
            x: text_left - self.url_scroll,
            y: url_y.round(),
            color: url_color,
            clip: Some([text_left as i32, ur.y as i32, (text_left + text_w) as i32, (ur.y + ur.h) as i32]),
        });
        let lr = layout.tabs_label;
        text.push(TextBlock { buffer: &self.tabs_label.buffer, x: lr.x, y: lr.y, color: t.dim, clip: Some(full) });
        let ti = &self.icons[7];
        text.push(TextBlock {
            buffer: &ti.buffer,
            x: (tr.x + 12.0 * s).round(),
            y: (tr.y + (tr.h - ti.line_height()) / 2.0).round(),
            color: t.dim,
            clip: Some(full),
        });
        text.push(TextBlock {
            buffer: &self.tab_label.buffer,
            x: (tr.x + 34.0 * s).round(),
            y: (tr.y + (tr.h - self.tab_label.line_height()) / 2.0).round(),
            color: t.text,
            clip: Some([tr.x as i32, tr.y as i32, (tr.x + tr.w - 8.0 * s) as i32, (tr.y + tr.h) as i32]),
        });
        if self.show_fps {
            let fr = layout.fps;
            text.push(TextBlock { buffer: &self.fps_label.buffer, x: fr.x, y: fr.y, color: t.dim, clip: Some(full) });
        }

        Frame { cards, under, text, over }
    }
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

fn ellipsize(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
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
}
