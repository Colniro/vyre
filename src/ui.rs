//! Die Browser-Oberfläche: Seitenleiste (Navigation, Adressleiste, Tabs, Fensterknöpfe),
//! Hover-Animationen und die FPS-Anzeige. Alles in physischen Pixeln, skaliert mit `scale`.
//!
//! Die Seitenleiste hat zwei Modi wie bei Zen: fest angeheftet, oder eingeklappt und beim
//! Hovern am linken Rand ausfahrend. Beim Ausfahren wird die Seite nur verschoben, nicht in der
//! Größe geändert. So muss Servo nichts neu umbrechen, und die Animation bleibt flüssig.

use std::time::{Duration, Instant};

use winit::window::ResizeDirection;

use crate::engine::{PageState, Row};
use crate::history::Favorite;
use crate::inspector::Inspector;

mod panel;
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

/// Farben des privaten Fensters: dunkles Lila, damit man es sofort erkennt.
pub const PRIVATE_THEME: Theme = Theme {
    bg: Rgba::rgb(0x1b, 0x13, 0x26),
    surface: Rgba::rgb(0x29, 0x1e, 0x38),
    hover: Rgba::rgb(0x36, 0x28, 0x49),
    active: Rgba::rgb(0x47, 0x33, 0x63),
    text: Rgba::rgb(0xee, 0xe8, 0xf6),
    dim: Rgba::rgb(0x9a, 0x8c, 0xae),
    accent: Rgba::rgb(0xc4, 0x8c, 0xff),
    selection: Rgba(0xc4, 0x8c, 0xff, 0x60),
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
    /// Werbeblocker für diese Seite ein/aus.
    Shield,
    /// Seite oben in der Seitenleiste anpinnen.
    Star,
    /// Angepinnter Favorit oben in der Seitenleiste.
    Favorite(usize),
    /// Vorschlag unter der Adressleiste.
    Suggestion(usize),
    /// Karte "Passwort speichern?": speichern, nie für diese Seite, schließen.
    PwSave,
    PwNever,
    PwDismiss,
    /// "Privates Fenster" unter "Neuer Tab".
    NewPrivate,
    /// Download unten in der Seitenleiste (öffnet vyre://downloads) und sein Schließen-Kreuz.
    Download(usize),
    DownloadClose(usize),
    Tab(usize),
    TabClose(usize),
    NewTab,
    /// Linker Rand im Hover-Modus: lässt die Seitenleiste ausfahren.
    Edge,
    /// Trennlinie zwischen den Hälften einer geteilten Ansicht (zum Ziehen).
    Divider,
    Drag,
    Resize(ResizeDirection),
    Content,
    /// Inspektor rechts: Reiter, Zielkreuz, Leeren, Schließen, Rand zum Ziehen, Eingabe, Rest.
    InspTab(usize),
    InspPick,
    InspClear,
    InspClose,
    InspResize,
    InspInput,
    InspBody,
    /// Karte im Seitenbereich, wenn der Tab im Bild-in-Bild-Fenster läuft.
    PipBack,
    Nothing,
}

/// Flächen der Inspektor-Leiste.
#[derive(Clone, Copy, Debug, Default)]
pub struct InspLayout {
    pub panel: Rect,
    /// Spalt links neben der Leiste (zum Ziehen der Breite).
    pub resize: Rect,
    pub tabs: [Rect; 3],
    pub pick: Rect,
    pub clear: Rect,
    pub close: Rect,
    /// Inhalt des Reiters.
    pub body: Rect,
    /// Eingabezeile der Konsole.
    pub input: Rect,
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
    /// Werbeblocker-Schild rechts in der Adressleiste.
    pub shield: Rect,
    /// Stern zum Anpinnen, links neben dem Schild.
    pub star: Rect,
    /// Karte "Passwort speichern?" mit ihren Knöpfen.
    pub pw_card: Option<Rect>,
    pub pw_save: Rect,
    pub pw_never: Rect,
    pub pw_close: Rect,
    /// Downloads unten in der Seitenleiste, über der FPS-Anzeige.
    pub downloads: Vec<Rect>,
    /// Angepinnte Favoriten als kleine Quadrate unter der Adressleiste.
    pub favorites: Vec<Rect>,
    /// Vorschläge, schwebend unter der Adressleiste.
    pub suggestions: Vec<Rect>,
    pub tabs_label: Rect,
    /// Fläche jedes Tabs (bei Paaren die jeweilige Hälfte der Zeile).
    pub tabs: Vec<Rect>,
    /// Zeilen der Tab-Liste mit ihrer ganzen Fläche.
    pub rows: Vec<(Row, Rect)>,
    /// Die beiden Hälften einer geteilten Ansicht.
    pub panes: Option<(Rect, Rect)>,
    /// Spalt zwischen den Hälften (zum Ziehen der Trennlinie).
    pub divider: Option<Rect>,
    /// Beim Ziehen eines Tabs auf die Seite: wo er landen würde.
    pub drop_zone: Option<Rect>,
    /// Fläche, die die Engine für die Seite(n) bekommt.
    pub page: Rect,
    row_x: f32,
    row_w: f32,
    first_row_y: f32,
    pub new_tab: Rect,
    pub new_private: Option<Rect>,
    pub content: Rect,
    pub fps: Rect,
    /// Inspektor rechts, wenn offen.
    pub insp: Option<InspLayout>,
    /// Karte "Läuft im Bild-in-Bild-Fenster" (zum Zurückholen).
    pub pip_back: Option<Rect>,
}

/// Wie viele Einträge die Seitenleiste zeigt.
#[derive(Clone, Copy, Debug, Default)]
pub struct Counts {
    pub tabs: usize,
    pub favorites: usize,
    pub suggestions: usize,
    /// Karte "Passwort speichern?" unter der Adressleiste.
    pub prompt: bool,
    /// Downloads unten in der Seitenleiste.
    pub downloads: usize,
    /// Zeile "Privates Fenster" anzeigen (nicht im privaten Fenster selbst).
    pub private_button: bool,
    /// Inspektor offen: Breite (logische Pixel) und ob der Konsolen-Reiter (mit Eingabe) aktiv ist.
    pub inspector: Option<(f32, bool)>,
    /// Der aktive Tab läuft im Bild-in-Bild-Fenster.
    pub pip_here: bool,
}

const FAVS_PER_ROW: usize = 6;
const FAV_GAP: f32 = 6.0;
const SUGGESTION_H: f32 = 46.0;

impl Layout {
    pub fn new(width: u32, height: u32, scale: f32, mode: SidebarMode, reveal: f32, counts: Counts) -> Self {
        let tab_count = counts.tabs;
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
        // Favoriten in Reihen unter der Adressleiste.
        // Feste Anzahl pro Reihe; die Größe füllt die Breite der Leiste genau aus.
        let per_row = FAVS_PER_ROW;
        let fav_gap = FAV_GAP * s;
        let fav = ((sw - 2.0 * pad - (per_row - 1) as f32 * fav_gap) / per_row as f32).floor();
        let pw_card = counts.prompt.then(|| Rect { x: url.x, y: url.y + url.h + 10.0 * s, w: url.w, h: 96.0 * s });
        let (pw_save, pw_never, pw_close) = match pw_card {
            Some(c) => {
                let bw = (c.w - 30.0 * s) / 2.0;
                let by = c.y + c.h - 40.0 * s;
                (
                    Rect { x: c.x + 10.0 * s, y: by, w: bw, h: 30.0 * s },
                    Rect { x: c.x + 20.0 * s + bw, y: by, w: bw, h: 30.0 * s },
                    Rect { x: c.x + c.w - 30.0 * s, y: c.y + 6.0 * s, w: 24.0 * s, h: 24.0 * s },
                )
            }
            None => Default::default(),
        };
        let fav_top = pw_card.map(|c| c.y + c.h).unwrap_or(url.y + url.h) + 10.0 * s;
        let favorites: Vec<Rect> = (0..counts.favorites)
            .map(|i| Rect {
                x: sx + pad + (i % per_row) as f32 * (fav + fav_gap),
                y: fav_top + (i / per_row) as f32 * (fav + fav_gap),
                w: fav,
                h: fav,
            })
            .collect();
        let above_tabs = favorites.last().map(|r| r.y + r.h).unwrap_or(url.y + url.h);
        let tabs_label = Rect { x: sx + pad + 6.0 * s, y: above_tabs + 16.0 * s, w: sw - 2.0 * pad, h: 18.0 * s };
        let suggestions = (0..counts.suggestions)
            .map(|i| Rect {
                x: url.x,
                y: url.y + url.h + 6.0 * s + i as f32 * SUGGESTION_H * s,
                w: url.w,
                h: SUGGESTION_H * s,
            })
            .collect();
        let first = tabs_label.y + tabs_label.h + 4.0 * s;
        let row = |i: usize| Rect { x: sx + pad, y: first + i as f32 * (TAB_H + TAB_SPACING) * s, w: sw - 2.0 * pad, h: TAB_H * s };
        let tabs = (0..tab_count).map(row).collect();
        let new_tab = row(tab_count);

        // Fest: Seite neben der Leiste. Hover: Seite fast fensterbreit, beim Ausfahren verschoben.
        // Mit Inspektor rechts wird die Seite schmaler (im Hover-Modus beim Ausfahren auch, damit
        // sie nicht unter die Leiste rutscht).
        let pw = counts.inspector.map(|(iw, _)| (iw * s).round().min(w * 0.7)).unwrap_or(0.0);
        let content = match mode {
            SidebarMode::Pinned => Rect { x: sw, y: gap, w: (w - sw - gap - pw).max(1.0), h: (h - 2.0 * gap).max(1.0) },
            SidebarMode::Hover => Rect {
                x: (gap + reveal * (sw - gap)).round(),
                y: gap,
                w: (w - 2.0 * gap - pw - if pw > 0.0 { reveal * (sw - gap) } else { 0.0 }).max(1.0),
                h: (h - 2.0 * gap).max(1.0),
            },
        };
        let pip_back = counts.pip_here.then(|| {
            let (cw, ch) = ((360.0 * s).min(content.w - 20.0 * s), 92.0 * s);
            Rect { x: (content.x + (content.w - cw) / 2.0).round(), y: (content.y + (content.h - ch) / 2.0).round(), w: cw, h: ch }
        });
        let insp = counts.inspector.map(|(_, console)| {
            let p = Rect { x: w - pw, y: gap, w: (pw - gap).max(1.0), h: (h - 2.0 * gap).max(1.0) };
            let hy = p.y + 8.0 * s;
            let bh = 30.0 * s;
            let close = Rect { x: p.x + p.w - 8.0 * s - 30.0 * s, y: hy, w: 30.0 * s, h: bh };
            let clear = Rect { x: close.x - 32.0 * s, ..close };
            let pick = Rect { x: clear.x - 32.0 * s, ..close };
            let tabs_x = p.x + 8.0 * s;
            let tw = ((pick.x - 6.0 * s - tabs_x) / 3.0).min(96.0 * s).max(1.0);
            let tab = |i: usize| Rect { x: tabs_x + i as f32 * tw, y: hy, w: tw - 2.0 * s, h: bh };
            let input = Rect { x: p.x + 8.0 * s, y: p.y + p.h - 8.0 * s - 34.0 * s, w: p.w - 16.0 * s, h: 34.0 * s };
            let by = hy + bh + 14.0 * s;
            let bottom = if console { input.y - 8.0 * s } else { p.y + p.h - 10.0 * s };
            InspLayout {
                panel: p,
                resize: Rect { x: p.x - gap - 2.0 * s, y: p.y, w: gap + 4.0 * s, h: p.h },
                tabs: [tab(0), tab(1), tab(2)],
                pick,
                clear,
                close,
                body: Rect { x: p.x + 12.0 * s, y: by, w: p.w - 24.0 * s, h: (bottom - by).max(0.0) },
                input,
            }
        });
        let fps = Rect { x: sx + pad + 4.0 * s, y: h - 46.0 * s, w: sw - 2.0 * pad, h: 40.0 * s };
        let dl_h = 46.0 * s;
        let downloads: Vec<Rect> = (0..counts.downloads)
            .map(|i| Rect {
                x: sx + pad,
                y: fps.y - 8.0 * s - (counts.downloads - i) as f32 * (dl_h + 4.0 * s),
                w: sw - 2.0 * pad,
                h: dl_h,
            })
            .collect();
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
            shield: Rect { x: url.x + url.w - 50.0 * s, y: url.y + 4.0 * s, w: 46.0 * s, h: url.h - 8.0 * s },
            star: Rect { x: url.x + url.w - 80.0 * s, y: url.y + 4.0 * s, w: 28.0 * s, h: url.h - 8.0 * s },
            pw_card,
            pw_save,
            pw_never,
            pw_close,
            downloads,
            favorites,
            suggestions,
            tabs_label,
            rows: (0..tab_count).map(|i| (Row::Single(i), row(i))).collect(),
            tabs,
            new_tab,
            new_private: counts.private_button.then(|| Rect { y: new_tab.y + (TAB_H + TAB_SPACING) * s, ..new_tab }),
            content,
            panes: None,
            divider: None,
            drop_zone: None,
            page: content,
            row_x: sx + pad,
            row_w: sw - 2.0 * pad,
            first_row_y: first,
            fps,
            insp,
            pip_back,
        }
    }

    /// Ordnet die Tab-Zeilen (Paare teilen sich eine Zeile) und den Seitenbereich für eine geteilte
    /// Ansicht bzw. die Vorschau beim Ziehen eines Tabs auf die Seite (`drop`: rechts = `true`).
    pub fn with_split(mut self, rows: &[Row], split: Option<f32>, drop: Option<bool>) -> Self {
        let s = self.scale;
        if !rows.is_empty() {
            let step = (TAB_H + TAB_SPACING) * s;
            let n = rows.iter().map(|r| match r { Row::Single(i) | Row::Pair(_, i) => i + 1 }).max().unwrap_or(0);
            let mut tabs = vec![Rect::default(); n];
            self.rows.clear();
            for (k, row) in rows.iter().enumerate() {
                let r = Rect { x: self.row_x, y: self.first_row_y + k as f32 * step, w: self.row_w, h: TAB_H * s };
                match *row {
                    Row::Single(i) => tabs[i] = r,
                    Row::Pair(a, b) => {
                        let half = (r.w / 2.0).floor();
                        tabs[a] = Rect { w: half, ..r };
                        tabs[b] = Rect { x: r.x + half, w: r.w - half, ..r };
                    }
                }
                self.rows.push((*row, r));
            }
            self.tabs = tabs;
            self.new_tab = Rect { x: self.row_x, y: self.first_row_y + rows.len() as f32 * step, w: self.row_w, h: TAB_H * s };
            if let Some(p) = &mut self.new_private {
                p.y = self.new_tab.y + step;
            }
        }
        let c = self.content;
        if let Some(right) = drop {
            // Vorschau: die Seite rückt zur Seite, der gezogene Tab bekommt die andere Hälfte.
            let (l, r) = split_rects(c, 0.5, s);
            let (zone, page) = if right { (r, l) } else { (l, r) };
            self.drop_zone = Some(zone);
            self.page = page;
        } else {
            self.page = c;
        }
        if let (Some(ratio), None) = (split, drop) {
            let (l, r) = split_rects(c, ratio, s);
            self.panes = Some((l, r));
            self.divider = Some(Rect { x: l.x + l.w - 3.0 * s, y: c.y, w: r.x - (l.x + l.w) + 6.0 * s, h: c.h });
        }
        self
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
        if let Some(p) = &self.insp {
            if p.resize.contains(x, y) {
                return Hit::InspResize;
            }
            if p.panel.contains(x, y) {
                for (i, r) in p.tabs.iter().enumerate() {
                    if r.contains(x, y) {
                        return Hit::InspTab(i);
                    }
                }
                for (r, hit) in [(p.pick, Hit::InspPick), (p.clear, Hit::InspClear), (p.close, Hit::InspClose)] {
                    if r.contains(x, y) {
                        return hit;
                    }
                }
                if !p.body.contains(x, y) && p.input.contains(x, y) {
                    return Hit::InspInput;
                }
                return Hit::InspBody;
            }
        }
        if self.over_sidebar(x) {
            // Die Vorschläge schweben über allem anderen.
            for (i, r) in self.suggestions.iter().enumerate() {
                if r.contains(x, y) {
                    return Hit::Suggestion(i);
                }
            }
            for (i, r) in self.top.iter().enumerate() {
                if r.contains(x, y) {
                    return TOP[i];
                }
            }
            if let Some(card) = self.pw_card {
                for (r, hit) in [(self.pw_close, Hit::PwDismiss), (self.pw_save, Hit::PwSave), (self.pw_never, Hit::PwNever)] {
                    if r.contains(x, y) {
                        return hit;
                    }
                }
                if card.contains(x, y) {
                    return Hit::Nothing;
                }
            }
            if self.shield.contains(x, y) {
                return Hit::Shield;
            }
            if self.star.contains(x, y) {
                return Hit::Star;
            }
            for (i, r) in self.favorites.iter().enumerate() {
                if r.contains(x, y) {
                    return Hit::Favorite(i);
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
            if self.new_private.is_some_and(|r| r.contains(x, y)) {
                return Hit::NewPrivate;
            }
            for (i, r) in self.downloads.iter().enumerate() {
                if r.contains(x, y) {
                    let close = Rect { x: r.x + r.w - 28.0 * self.scale, y: r.y + 4.0 * self.scale, w: 24.0 * self.scale, h: 24.0 * self.scale };
                    return if close.contains(x, y) { Hit::DownloadClose(i) } else { Hit::Download(i) };
                }
            }
        }
        if self.divider.is_some_and(|d| d.contains(x, y)) {
            return Hit::Divider;
        }
        if self.pip_back.is_some_and(|r| r.contains(x, y)) {
            return Hit::PipBack;
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

/// Teilt eine Fläche in zwei Hälften nebeneinander, mit einem Spalt dazwischen.
/// `ratio` ist der Anteil der linken Hälfte.
pub fn split_rects(c: Rect, ratio: f32, scale: f32) -> (Rect, Rect) {
    let gap = (GAP * scale).round();
    let left_w = ((c.w - gap) * ratio.clamp(0.05, 0.95)).round();
    let left = Rect { w: left_w, ..c };
    let right = Rect { x: c.x + left_w + gap, w: (c.w - left_w - gap).max(1.0), ..c };
    (left, right)
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
const ICON_PRIVATE: &str = "\u{E727}";
const ICON_WARNING: &str = "\u{E7BA}";
const ICON_SHIELD: &str = "\u{EA18}";
const ICON_STAR: &str = "\u{E734}";
const ICON_STAR_FILLED: &str = "\u{E735}";
const ICON_SEARCH: &str = "\u{E721}";
const ICON_HISTORY: &str = "\u{E81C}";

/// Ein Vorschlag unter der Adressleiste.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub title: String,
    pub detail: String,
    /// Wohin Enter oder ein Klick führt.
    pub url: url::Url,
    pub kind: SuggestionKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SuggestionKind {
    Search,
    Open,
    History,
    Favorite,
}

/// Texte einer Vorschlagszeile.
struct SuggestionLabels {
    icon: Label,
    title: Label,
    detail: Label,
}

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
    shield_icon: Label,
    shield_count: Label,
    star_icon: Label,
    pw_labels: [Label; 5],
    pip_labels: [Label; 2],
    dl_labels: Vec<[Label; 3]>,
    fav_labels: Vec<Label>,
    sug_labels: Vec<SuggestionLabels>,
    /// Vorschläge zur aktuellen Eingabe; der erste ist das, was Enter ohne Auswahl tut.
    pub suggestions: Vec<Suggestion>,
    /// Ausgewählter Vorschlag (Pfeiltasten).
    pub selected: usize,
    /// Tab, der gerade gezogen wird.
    pub dragging: Option<usize>,
    tabs_label: Label,
    tab_labels: Vec<TabLabels>,
    new_tab_icon: Label,
    new_tab_label: Label,
    private_icon: Label,
    private_label: Label,
    /// Farben: normal oder privat.
    theme: &'static Theme,
    pub private: bool,
    fps_label: Label,
    /// Horizontales Verschieben des URL-Textes, damit der Cursor sichtbar bleibt.
    url_scroll: f32,
    pub show_fps: bool,
    mono_family: String,
    panel: panel::PanelLabels,
}

/// Was gezeichnet werden soll.
pub struct Frame<'a> {
    pub cards: Vec<RoundRect>,
    pub under: Vec<Quad>,
    pub text: Vec<TextBlock<'a>>,
    pub over: Vec<Quad>,
    /// Schwebende Vorschläge über allem anderen.
    pub overlay_cards: Vec<RoundRect>,
    pub overlay: Vec<Quad>,
    pub overlay_text: Vec<TextBlock<'a>>,
}

pub struct Stats {
    pub ui_fps: usize,
    pub web_fps: usize,
    pub refresh_hz: Option<f32>,
    pub bench: bool,
    pub maximized: bool,
    /// Blockt der Werbeblocker auf der aktuellen Seite?
    pub adblock_on: bool,
}

fn has_family(fs: &FontSystem, name: &str) -> bool {
    fs.db()
        .faces()
        .any(|f| f.families.iter().any(|(fam, _)| fam.eq_ignore_ascii_case(name)))
}

/// Inhalt der Karte unter der Adressleiste; leere Knopftexte blenden den Knopf aus.
#[derive(Clone, Copy, Debug)]
pub struct Card<'a> {
    pub title: &'a str,
    pub detail: &'a str,
    pub primary: &'a str,
    pub secondary: &'a str,
}

/// Was die Oberfläche über die Tabs wissen muss.
pub struct TabsView<'a> {
    pub tabs: &'a [PageState],
    pub active: usize,
    pub favorites: &'a [Favorite],
    /// Ist die Seite im aktiven Tab angepinnt?
    pub is_favorite: bool,
    /// Karte unter der Adressleiste ("Passwort speichern?", "Vyre X ist da").
    pub card: Option<Card<'a>>,
    /// Downloads für die Seitenleiste.
    pub downloads: &'a [crate::downloads::Entry],
    /// Inspektor, wenn offen.
    pub inspector: Option<&'a Inspector>,
}

fn paired(layout: &Layout, i: usize) -> Option<(usize, usize, Rect)> {
    layout.rows.iter().find_map(|(row, r)| match *row {
        Row::Pair(a, b) if a == i || b == i => Some((a, b, *r)),
        _ => None,
    })
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
        let mono_family = ["Cascadia Mono", "Cascadia Code", "Consolas", "Courier New"]
            .into_iter()
            .find(|f| has_family(fs, f))
            .unwrap_or("monospace")
            .to_string();
        log::info!("UI-Schrift: {ui_family}, Symbole: {icon_family}, Konsole: {mono_family}");
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
            shield_icon: Label::new(fs),
            shield_count: Label::new(fs),
            star_icon: Label::new(fs),
            pw_labels: [Label::new(fs), Label::new(fs), Label::new(fs), Label::new(fs), Label::new(fs)],
            pip_labels: [Label::new(fs), Label::new(fs)],
            dl_labels: Vec::new(),
            fav_labels: Vec::new(),
            sug_labels: Vec::new(),
            suggestions: Vec::new(),
            selected: 0,
            dragging: None,
            tabs_label: Label::new(fs),
            tab_labels: Vec::new(),
            new_tab_icon: Label::new(fs),
            new_tab_label: Label::new(fs),
            private_icon: Label::new(fs),
            private_label: Label::new(fs),
            theme: &THEME,
            private: false,
            fps_label: Label::new(fs),
            panel: panel::PanelLabels::new(fs),
            mono_family,
            ui_family,
            icon_family,
            url_scroll: 0.0,
            show_fps: true,
        }
    }

    /// Schaltet auf die Farben des privaten Fensters.
    pub fn set_private(&mut self, private: bool) {
        self.private = private;
        self.theme = if private { &PRIVATE_THEME } else { &THEME };
    }

    pub fn theme(&self) -> &'static Theme {
        self.theme
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
            if !matches!(t, Hit::Nothing | Hit::Drag | Hit::Content | Hit::Edge | Hit::Resize(_) | Hit::InspBody | Hit::InspInput)
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
            Some(u) if matches!(u.scheme(), "about" | "vyre") => String::new(),
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
        let t = self.theme;
        let page = tabs.tabs.get(tabs.active).cloned().unwrap_or_default();
        let mut cards = Vec::new();
        let mut under = Vec::new();
        let mut over = Vec::new();
        if let Some(r) = layout.pip_back {
            let a = self.anim(Hit::PipBack);
            cards.push(RoundRect::shadow(r.x, r.y, r.w, r.h, Rgba(0, 0, 0, 80), 12.0 * s, 6.0 * s));
            cards.push(RoundRect::new(r.x, r.y, r.w, r.h, lerp(t.surface, t.hover, a), 12.0 * s));
            self.pip_labels[0].set(fs, "Läuft im Bild-in-Bild-Fenster", 15.0 * s, &self.ui_family, true);
            self.pip_labels[1].set(fs, "Klicken, um das Video hierher zurückzuholen", 12.5 * s, &self.ui_family, false);
        }
        if let (Some(p), Some(insp)) = (layout.insp.as_ref(), tabs.inspector) {
            self.panel_prepare(fs, p, insp, s, &mut cards, &mut under, &mut over);
        }
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
        // Unverschlüsselte Seite: Warnsymbol statt Schloss.
        let insecure = page.url.as_ref().is_some_and(|u| u.scheme() == "http");
        let url_icon = if secure { ICON_LOCK } else if insecure { ICON_WARNING } else { ICON_GLOBE };
        self.url_icon.set(fs, url_icon, 11.0 * s, &self.icon_family, false);

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
        // Das Schild des Werbeblockers: nur auf echten Seiten und nicht beim Tippen.
        let show_shield = !self.url_focused && page.url.as_ref().is_some_and(|u| u.scheme().starts_with("http"));
        let text_w = ur.w - 34.0 * s - if show_shield { ur.x + ur.w - layout.star.x + 4.0 * s } else { 10.0 * s };
        if show_shield {
            let st = layout.star;
            let a = self.anim(Hit::Star);
            if a > 0.0 {
                cards.push(RoundRect::new(st.x, st.y, st.w, st.h, t.bg.with_alpha((a * 160.0) as u8), 6.0 * s));
            }
            let icon = if tabs.is_favorite { ICON_STAR_FILLED } else { ICON_STAR };
            self.star_icon.set(fs, icon, 11.0 * s, &self.icon_family, false);
        }
        if show_shield {
            let sr = layout.shield;
            let a = self.anim(Hit::Shield);
            if a > 0.0 {
                cards.push(RoundRect::new(sr.x, sr.y, sr.w, sr.h, t.bg.with_alpha((a * 160.0) as u8), 6.0 * s));
            }
            self.shield_icon.set(fs, ICON_SHIELD, 11.0 * s, &self.icon_family, false);
            let count = match (stats.adblock_on, page.blocked) {
                (false, _) => "aus".to_string(),
                (true, 0) => String::new(),
                (true, n) if n > 999 => "999+".to_string(),
                (true, n) => n.to_string(),
            };
            self.shield_count.set(fs, &count, 11.0 * s, &self.ui_family, true);
        }
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

        // Karte "Passwort speichern?" oder Update.
        if let (Some(c), Some(card)) = (layout.pw_card, tabs.card) {
            cards.push(RoundRect::shadow(c.x, c.y, c.w, c.h, Rgba(0, 0, 0, 90), 10.0 * s, 6.0 * s));
            cards.push(RoundRect::new(c.x, c.y, c.w, c.h, t.surface, 10.0 * s));
            cards.push(RoundRect::outline(c.x, c.y, c.w, c.h, t.accent.with_alpha(150), 10.0 * s, 1.0 * s));
            let sa = self.anim(Hit::PwSave);
            let na = self.anim(Hit::PwNever);
            let ca = self.anim(Hit::PwDismiss);
            let (b1, b2, b3) = (layout.pw_save, layout.pw_never, layout.pw_close);
            if !card.primary.is_empty() {
                cards.push(RoundRect::new(b1.x, b1.y, b1.w, b1.h, lerp(t.accent, Rgba::rgb(255, 255, 255), sa * 0.15), 8.0 * s));
            }
            if !card.secondary.is_empty() {
                cards.push(RoundRect::new(b2.x, b2.y, b2.w, b2.h, lerp(t.bg, t.hover, 0.5 + na * 0.5), 8.0 * s));
            }
            if ca > 0.0 {
                cards.push(RoundRect::new(b3.x, b3.y, b3.w, b3.h, t.hover.with_alpha((ca * 255.0) as u8), 6.0 * s));
            }
            let [title, detail, save, never, close] = &mut self.pw_labels;
            title.set(fs, card.title, 13.0 * s, &self.ui_family, true);
            detail.set(fs, card.detail, 11.5 * s, &self.ui_family, false);
            save.set(fs, card.primary, 12.5 * s, &self.ui_family, true);
            never.set(fs, card.secondary, 12.5 * s, &self.ui_family, false);
            close.set(fs, ICON_CLOSE, 8.5 * s, &self.icon_family, false);
        }

        // Downloads unten in der Seitenleiste.
        while self.dl_labels.len() < tabs.downloads.len() {
            self.dl_labels.push([Label::new(fs), Label::new(fs), Label::new(fs)]);
        }
        for (i, d) in tabs.downloads.iter().enumerate().take(layout.downloads.len()) {
            let r = layout.downloads[i];
            let a = self.anim(Hit::Download(i));
            cards.push(RoundRect::new(r.x, r.y, r.w, r.h, lerp(t.surface, t.hover, a * 0.7), 9.0 * s));
            let bar_y = r.y + r.h - 6.0 * s;
            let bar_w = r.w - 20.0 * s;
            match crate::downloads::progress(d) {
                Some(p) if d.is_active() => {
                    cards.push(RoundRect::new(r.x + 10.0 * s, bar_y, bar_w, 3.0 * s, t.bg, 1.5 * s));
                    cards.push(RoundRect::new(r.x + 10.0 * s, bar_y, (bar_w * p).max(3.0 * s), 3.0 * s, t.accent, 1.5 * s));
                }
                None if d.is_active() => {
                    // Größe unbekannt: wandernder Abschnitt.
                    let phase = (Instant::now().duration_since(self.last_anim).as_secs_f32() + i as f32 * 0.3).fract();
                    cards.push(RoundRect::new(r.x + 10.0 * s + phase * bar_w * 0.7, bar_y, bar_w * 0.3, 3.0 * s, t.accent, 1.5 * s));
                }
                _ => {}
            }
            let ca = self.anim(Hit::DownloadClose(i));
            if ca > 0.0 {
                cards.push(RoundRect::new(r.x + r.w - 28.0 * s, r.y + 4.0 * s, 24.0 * s, 24.0 * s, t.hover.with_alpha((ca * 255.0) as u8), 6.0 * s));
            }
            let [name, status, close] = &mut self.dl_labels[i];
            name.set(fs, &d.name, 12.5 * s, &self.ui_family, false);
            status.set(fs, &crate::downloads::status_text(d), 10.5 * s, &self.ui_family, false);
            close.set(fs, ICON_CLOSE, 8.0 * s, &self.icon_family, false);
        }

        // Angepinnte Favoriten: farbiges Quadrat mit Anfangsbuchstaben.
        while self.fav_labels.len() < tabs.favorites.len() {
            self.fav_labels.push(Label::new(fs));
        }
        self.fav_labels.truncate(tabs.favorites.len());
        let active_url = page.url.as_ref().map(|u| u.as_str().to_string()).unwrap_or_default();
        for (i, f) in tabs.favorites.iter().enumerate() {
            let r = layout.favorites[i];
            let a = self.anim(Hit::Favorite(i));
            let host = url::Url::parse(&f.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
            let base = hue_color(&host);
            cards.push(RoundRect::new(r.x, r.y, r.w, r.h, lerp(base, Rgba::rgb(255, 255, 255), a * 0.12), 9.0 * s));
            if same_site(&f.url, &active_url) {
                cards.push(RoundRect::outline(r.x - 2.0 * s, r.y - 2.0 * s, r.w + 4.0 * s, r.h + 4.0 * s, t.accent, 11.0 * s, 1.5 * s));
            }
            let name = if f.title.trim().is_empty() { host.trim_start_matches("www.").to_string() } else { f.title.clone() };
            let letter = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            self.fav_labels[i].set(fs, &letter, 15.0 * s, &self.ui_family, true);
        }

        // Tab-Liste.
        self.tabs_label.set(fs, if self.private { "PRIVAT · TABS" } else { "TABS" }, 11.0 * s, &self.ui_family, true);
        if let Some(pr) = layout.new_private {
            let pa = self.anim(Hit::NewPrivate);
            if pa > 0.0 {
                cards.push(RoundRect::new(pr.x, pr.y, pr.w, pr.h, t.hover.with_alpha((pa * 200.0) as u8), 9.0 * s));
            }
            self.private_icon.set(fs, ICON_PRIVATE, 11.0 * s, &self.icon_family, false);
            self.private_label.set(fs, "Privates Fenster", 13.5 * s, &self.ui_family, false);
        }
        while self.tab_labels.len() < tabs.tabs.len() {
            self.tab_labels.push(TabLabels { icon: Label::new(fs), title: Label::new(fs), close: Label::new(fs) });
        }
        self.tab_labels.truncate(tabs.tabs.len().max(1));
        // Verbundene Einträge (geteilte Ansicht): eine gemeinsame Karte mit Trennstrich.
        for (row, r) in &layout.rows {
            let Row::Pair(a, b) = *row else { continue };
            let has_active = a == tabs.active || b == tabs.active;
            cards.push(RoundRect::new(r.x, r.y, r.w, r.h, if has_active { t.surface } else { lerp(t.bg, t.surface, 0.7) }, 9.0 * s));
            cards.push(RoundRect::outline(r.x, r.y, r.w, r.h, t.accent.with_alpha(if has_active { 140 } else { 60 }), 9.0 * s, 1.0 * s));
            let mid = layout.tabs[b].x;
            under.push(Quad { x: mid - 0.5 * s, y: r.y + 8.0 * s, w: (1.0 * s).max(1.0), h: r.h - 16.0 * s, color: t.dim.with_alpha(90) });
        }
        for (i, state) in tabs.tabs.iter().enumerate() {
            let r = layout.tabs[i];
            let a = self.anim(Hit::Tab(i));
            let in_pair = paired(layout, i).is_some();
            let bg = if i == tabs.active { t.active } else { lerp(t.bg, t.hover, a * 0.8) };
            if i == tabs.active || a > 0.0 {
                // In einem Paar: nur eine etwas kleinere Fläche innerhalb der gemeinsamen Karte.
                let inset = if in_pair { 3.0 * s } else { 0.0 };
                cards.push(RoundRect::new(r.x + inset, r.y + inset, r.w - 2.0 * inset, r.h - 2.0 * inset, bg, 7.0 * s));
            }
            if self.dragging == Some(i) {
                cards.push(RoundRect::outline(r.x, r.y, r.w, r.h, t.accent, 9.0 * s, 1.5 * s));
            }
            if self.hovered(Hit::Tab(i)) || (i == tabs.active && !in_pair) {
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

        // Griff der Trennlinie zwischen den Hälften.
        if let Some(d) = layout.divider {
            let a = self.anim(Hit::Divider).max(if self.pressed == Some(Hit::Divider) { 1.0 } else { 0.0 });
            let (gw, gh) = (3.0 * s, 36.0 * s);
            cards.push(RoundRect::new(
                d.x + (d.w - gw) / 2.0,
                d.y + (d.h - gh) / 2.0,
                gw,
                gh,
                lerp(t.hover, t.accent, a),
                gw / 2.0,
            ));
        }
        if let Some(z) = layout.drop_zone {
            cards.push(RoundRect::new(z.x, z.y, z.w, z.h, t.accent.with_alpha(40), 10.0 * s));
            cards.push(RoundRect::outline(z.x, z.y, z.w, z.h, t.accent.with_alpha(200), 10.0 * s, 2.0 * s));
        }

        // Vorschläge schweben über Favoriten und Tabs.
        let mut overlay_cards = Vec::new();
        let mut overlay_text = Vec::new();
        if self.url_focused && !layout.suggestions.is_empty() {
            let first = layout.suggestions[0];
            let last = layout.suggestions[layout.suggestions.len() - 1];
            let (x, y, w, h) = (first.x, first.y, first.w, last.y + last.h - first.y);
            overlay_cards.push(RoundRect::shadow(x, y, w, h, Rgba(0, 0, 0, 120), 10.0 * s, 10.0 * s));
            overlay_cards.push(RoundRect::new(x, y, w, h, t.surface, 10.0 * s));
            while self.sug_labels.len() < self.suggestions.len() {
                self.sug_labels.push(SuggestionLabels { icon: Label::new(fs), title: Label::new(fs), detail: Label::new(fs) });
            }
            for (i, sug) in self.suggestions.iter().enumerate().take(layout.suggestions.len()) {
                let r = layout.suggestions[i];
                let a = self.anim(Hit::Suggestion(i));
                if i == self.selected {
                    overlay_cards.push(RoundRect::new(r.x + 4.0 * s, r.y + 3.0 * s, r.w - 8.0 * s, r.h - 6.0 * s, t.active, 8.0 * s));
                } else if a > 0.0 {
                    overlay_cards.push(RoundRect::new(r.x + 4.0 * s, r.y + 3.0 * s, r.w - 8.0 * s, r.h - 6.0 * s, t.hover.with_alpha((a * 255.0) as u8), 8.0 * s));
                }
                let icon = match sug.kind {
                    SuggestionKind::Search => ICON_SEARCH,
                    SuggestionKind::Open => ICON_GLOBE,
                    SuggestionKind::History => ICON_HISTORY,
                    SuggestionKind::Favorite => ICON_STAR_FILLED,
                };
                let l = &mut self.sug_labels[i];
                l.icon.set(fs, icon, 11.0 * s, &self.icon_family, false);
                l.title.set(fs, &sug.title, 13.0 * s, &self.ui_family, false);
                l.detail.set(fs, &sug.detail, 11.0 * s, &self.ui_family, false);
            }
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
        let url_icon_color = if secure {
            t.dim
        } else if page.url.as_ref().is_some_and(|u| u.scheme() == "http") {
            Rgba::rgb(0xff, 0x8a, 0x7a)
        } else {
            t.dim.with_alpha(160)
        };
        text.push(left_icon(&self.url_icon, ur, url_icon_color));
        text.push(TextBlock {
            buffer: &self.url_label.buffer,
            x: text_left - self.url_scroll,
            y: url_y.round(),
            color: url_color,
            clip: Some([text_left.max(0.0) as i32, ur.y as i32, (text_left + text_w).max(0.0) as i32, (ur.y + ur.h) as i32]),
        });
        if show_shield {
            text.push(TextBlock {
                buffer: &self.star_icon.buffer,
                x: (layout.star.x + (layout.star.w - self.star_icon.width()) / 2.0).round(),
                y: (layout.star.y + (layout.star.h - self.star_icon.line_height()) / 2.0).round(),
                color: if tabs.is_favorite { t.accent } else { t.dim },
                clip: Some(side_clip),
            });
        }
        for (i, l) in self.fav_labels.iter().enumerate() {
            text.push(centered(l, layout.favorites[i], Rgba::rgb(255, 255, 255)));
        }
        for (i, d) in tabs.downloads.iter().enumerate().take(layout.downloads.len()) {
            let r = layout.downloads[i];
            let [name, status, close] = &self.dl_labels[i];
            let clip = Some([r.x.max(0.0) as i32, r.y as i32, (r.x + r.w - 30.0 * s).max(0.0) as i32, (r.y + r.h) as i32]);
            text.push(TextBlock { buffer: &name.buffer, x: (r.x + 10.0 * s).round(), y: (r.y + 5.0 * s).round(), color: t.text, clip });
            let failed = matches!(d.state, crate::downloads::State::Failed(_));
            text.push(TextBlock {
                buffer: &status.buffer,
                x: (r.x + 10.0 * s).round(),
                y: (r.y + 6.0 * s + name.line_height()).round(),
                color: if failed { Rgba::rgb(0xff, 0x8a, 0x7a) } else { t.dim },
                clip,
            });
            let cr = Rect { x: r.x + r.w - 28.0 * s, y: r.y + 4.0 * s, w: 24.0 * s, h: 24.0 * s };
            text.push(centered(close, cr, t.dim));
        }
        if let (Some(c), Some(card)) = (layout.pw_card, tabs.card) {
            let [title, detail, save, never, close] = &self.pw_labels;
            let clip = Some([c.x.max(0.0) as i32, c.y as i32, (c.x + c.w - 34.0 * s).max(0.0) as i32, (c.y + c.h) as i32]);
            text.push(TextBlock { buffer: &title.buffer, x: (c.x + 12.0 * s).round(), y: (c.y + 10.0 * s).round(), color: t.text, clip });
            text.push(TextBlock {
                buffer: &detail.buffer,
                x: (c.x + 12.0 * s).round(),
                y: (c.y + 12.0 * s + title.line_height()).round(),
                color: t.dim,
                clip,
            });
            if !card.primary.is_empty() {
                text.push(centered(save, layout.pw_save, t.bg));
            }
            if !card.secondary.is_empty() {
                text.push(centered(never, layout.pw_never, t.text));
            }
            text.push(centered(close, layout.pw_close, t.dim));
        }
        if show_shield {
            let sr = layout.shield;
            let (icon_color, count_color) = if stats.adblock_on { (t.accent, t.text) } else { (t.dim.with_alpha(140), t.dim) };
            let gap = 4.0 * s;
            let total = self.shield_icon.width() + if self.shield_count.text.is_empty() { 0.0 } else { gap + self.shield_count.width() };
            let x0 = (sr.x + (sr.w - total) / 2.0).round();
            text.push(TextBlock {
                buffer: &self.shield_icon.buffer,
                x: x0,
                y: (sr.y + (sr.h - self.shield_icon.line_height()) / 2.0).round(),
                color: icon_color,
                clip: Some(side_clip),
            });
            text.push(TextBlock {
                buffer: &self.shield_count.buffer,
                x: x0 + self.shield_icon.width() + gap,
                y: (sr.y + (sr.h - self.shield_count.line_height()) / 2.0).round(),
                color: count_color,
                clip: Some(side_clip),
            });
        }
        let lr = layout.tabs_label;
        text.push(TextBlock {
            buffer: &self.tabs_label.buffer,
            x: lr.x,
            y: lr.y,
            color: if self.private { t.accent } else { t.dim },
            clip: Some(side_clip),
        });
        if let Some(pr) = layout.new_private {
            text.push(left_icon(&self.private_icon, pr, t.dim));
            text.push(TextBlock {
                buffer: &self.private_label.buffer,
                x: (pr.x + 34.0 * s).round(),
                y: (pr.y + (pr.h - self.private_label.line_height()) / 2.0).round(),
                color: t.dim,
                clip: Some(side_clip),
            });
        }
        for (i, state) in tabs.tabs.iter().enumerate() {
            let r = layout.tabs[i];
            let labels = &self.tab_labels[i];
            if !state.loading {
                text.push(left_icon(&labels.icon, r, if state.asleep { t.dim.with_alpha(110) } else { t.dim }));
            }
            let show_close = self.hovered(Hit::Tab(i)) || (i == tabs.active && paired(layout, i).is_none());
            let title_right = if show_close { close_rect(r, s).x - 4.0 * s } else { r.x + r.w - 8.0 * s };
            text.push(TextBlock {
                buffer: &labels.title.buffer,
                x: (r.x + 34.0 * s).round(),
                y: (r.y + (r.h - labels.title.line_height()) / 2.0).round(),
                color: if i == tabs.active {
                    t.text
                } else if state.asleep {
                    t.dim.with_alpha(150)
                } else {
                    lerp(t.dim, t.text, 0.6)
                },
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
        if self.url_focused && !layout.suggestions.is_empty() {
            for (i, sug) in self.suggestions.iter().enumerate().take(layout.suggestions.len()) {
                let r = layout.suggestions[i];
                let l = &self.sug_labels[i];
                let clip = Some([r.x.max(0.0) as i32, r.y as i32, (r.x + r.w - 10.0 * s).max(0.0) as i32, (r.y + r.h) as i32]);
                overlay_text.push(TextBlock {
                    buffer: &l.icon.buffer,
                    x: (r.x + 12.0 * s).round(),
                    y: (r.y + (r.h - l.icon.line_height()) / 2.0).round(),
                    color: if sug.kind == SuggestionKind::Favorite { t.accent } else { t.dim },
                    clip,
                });
                overlay_text.push(TextBlock {
                    buffer: &l.title.buffer,
                    x: (r.x + 34.0 * s).round(),
                    y: (r.y + 6.0 * s).round(),
                    color: t.text,
                    clip,
                });
                overlay_text.push(TextBlock {
                    buffer: &l.detail.buffer,
                    x: (r.x + 34.0 * s).round(),
                    y: (r.y + 6.0 * s + l.title.line_height()).round(),
                    color: t.dim,
                    clip,
                });
            }
        }

        if let Some(r) = layout.pip_back {
            let [title, hint] = &self.pip_labels;
            let clip = Some([r.x.max(0.0) as i32, r.y as i32, (r.x + r.w) as i32, (r.y + r.h) as i32]);
            let mid = r.y + r.h / 2.0;
            text.push(TextBlock {
                buffer: &title.buffer,
                x: (r.x + (r.w - title.width()) / 2.0).round(),
                y: (mid - title.line_height() - 1.0 * s).round(),
                color: t.text,
                clip,
            });
            text.push(TextBlock { buffer: &hint.buffer, x: (r.x + (r.w - hint.width()) / 2.0).round(), y: (mid + 3.0 * s).round(), color: t.dim, clip });
        }
        if layout.insp.is_some() && tabs.inspector.is_some() {
            self.panel_text(&mut text);
        }
        Frame { cards, under, text, over, overlay_cards, overlay: Vec::new(), overlay_text }
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

/// Farbe für einen Anfangsbuchstaben, fest pro Domain (wie auf der Neuer-Tab-Seite).
fn hue_color(host: &str) -> Rgba {
    let h = host.trim_start_matches("www.").bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32) % 360;
    // HSL(h, 42 %, 36 %) nach RGB.
    let (s, l) = (0.42f32, 0.36f32);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h as f32 / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    let to = |v: f32| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgba::rgb(to(r), to(g), to(b))
}

/// Gehören zwei Adressen zur selben Seite (Host ohne "www.")?
pub fn same_site(a: &str, b: &str) -> bool {
    let host = |u: &str| url::Url::parse(u).ok().and_then(|u| u.host_str().map(|h| h.trim_start_matches("www.").to_string()));
    matches!((host(a), host(b)), (Some(x), Some(y)) if x == y)
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

#[cfg(test)]
mod tests {
    use super::*;

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
        let pinned = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 0.0, Counts { tabs, ..Default::default() });
        assert_eq!(pinned.sidebar_x, 0.0);
        assert_eq!(pinned.content.x, SIDEBAR_W);
        assert_eq!(pinned.hit(100.0, pinned.tabs[1].y + 5.0, false), Hit::Tab(1));

        let hidden = Layout::new(1600, 900, 1.0, SidebarMode::Hover, 0.0, Counts { tabs, ..Default::default() });
        assert_eq!(hidden.sidebar_x, -SIDEBAR_W);
        assert_eq!(hidden.content.x, GAP);
        assert_eq!(hidden.hit(7.0, 400.0, false), Hit::Edge);
        assert_eq!(hidden.hit(100.0, hidden.tabs[1].y + 5.0, false), Hit::Content);

        // Die Seite behält im Hover-Modus ihre Breite, sie wird nur verschoben.
        let shown = Layout::new(1600, 900, 1.0, SidebarMode::Hover, 1.0, Counts { tabs, ..Default::default() });
        assert_eq!(shown.content.w, hidden.content.w);
        assert_eq!(shown.content.x, SIDEBAR_W);
        assert_eq!(shown.hit(100.0, shown.tabs[1].y + 5.0, false), Hit::Tab(1));
    }

    #[test]
    fn favoriten_und_vorschlaege() {
        let l = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 1.0, Counts { tabs: 2, favorites: 7, suggestions: 3, prompt: false, downloads: 0, private_button: false, inspector: None, pip_here: false });
        // 6 Favoriten pro Reihe, der siebte rutscht in die zweite Reihe.
        assert_eq!(l.favorites[6].x, l.favorites[0].x);
        assert!(l.favorites[6].y > l.favorites[0].y);
        assert!(l.tabs_label.y > l.favorites[6].y + l.favorites[6].h);
        // Ohne Vorschläge trifft man den Favoriten …
        let plain = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 1.0, Counts { tabs: 2, favorites: 7, suggestions: 0, prompt: false, downloads: 0, private_button: false, inspector: None, pip_here: false });
        let f = plain.favorites[1];
        assert_eq!(plain.hit(f.x + 3.0, f.y + 3.0, false), Hit::Favorite(1));
        // … mit Vorschlägen liegen diese darüber und gewinnen.
        assert_eq!(l.hit(f.x + 3.0, f.y + 3.0, false), Hit::Suggestion(0));
        assert_eq!(l.hit(l.star.x + 3.0, l.star.y + 3.0, false), Hit::Star);
        assert!(same_site("https://www.a.de/x", "https://a.de/"));
        assert!(!same_site("https://a.de/", "vyre://neuer-tab"));
    }

    #[test]
    fn geteilte_ansicht() {
        let base = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 1.0, Counts { tabs: 3, ..Default::default() });
        let l = base.with_split(&[Row::Single(0), Row::Pair(1, 2)], Some(0.5), None);
        // Das Paar teilt sich eine Zeile.
        assert_eq!(l.tabs[1].y, l.tabs[2].y);
        assert!(l.tabs[2].x > l.tabs[1].x);
        assert_eq!(l.hit(l.tabs[2].x + 10.0, l.tabs[2].y + 10.0, false), Hit::Tab(2));
        // Die Hälften lassen genau den Spalt frei, und der Spalt ist die Trennlinie.
        let (a, b) = l.panes.unwrap();
        assert_eq!(a.x + a.w + GAP, b.x);
        assert_eq!(b.x + b.w, l.content.x + l.content.w);
        assert_eq!(l.hit(a.x + a.w + GAP / 2.0, 400.0, false), Hit::Divider);
        // Ziehen auf die rechte Hälfte: Seite links, Ablage rechts.
        let base = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 1.0, Counts { tabs: 2, ..Default::default() });
        let d = base.with_split(&[], None, Some(true));
        assert!(d.drop_zone.unwrap().x > d.page.x);
        assert!(d.page.w < d.content.w);
    }

    #[test]
    fn tab_schliessen_knopf() {
        let l = Layout::new(1600, 900, 1.0, SidebarMode::Pinned, 1.0, Counts { tabs: 2, ..Default::default() });
        let c = close_rect(l.tabs[0], 1.0);
        assert_eq!(l.hit(c.x + 2.0, c.y + 2.0, false), Hit::TabClose(0));
        assert_eq!(l.hit(l.new_tab.x + 5.0, l.new_tab.y + 5.0, false), Hit::NewTab);
    }
}
