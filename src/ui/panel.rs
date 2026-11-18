//! Die Inspektor-Leiste rechts im Fenster (F12): Kopfzeile mit Reitern und Knöpfen, darunter
//! Element (Box-Modell und Stile), Konsole (mit Eingabe) oder Netzwerk.
//!
//! Gezeichnet wird in zwei Schritten wie der Rest der Oberfläche: `panel_prepare` setzt die
//! Texte und merkt sich, wo sie hingehören; `panel_text` macht daraus die Textblöcke.

use std::collections::VecDeque;

use super::*;
use crate::inspector::{Element, Inspector, Level, Tab, css_color, format_duration, format_size};

const ICON_PICK: &str = "\u{E7C9}";
const ICON_CLEAR: &str = "\u{E74D}";
const ROW_H: f32 = 40.0;

/// Welche Beschriftung an einer Stelle steht.
#[derive(Clone, Copy, Debug)]
enum L {
    Tab(usize),
    Pick,
    Clear,
    Close,
    Hint,
    Title,
    Path,
    BoxName(usize),
    BoxNum(usize),
    Content,
    Key(usize),
    Val(usize),
    Line(usize),
    Prompt,
    Input,
    Placeholder,
    Summary,
    NetName(usize),
    NetDetail(usize),
    NetRight(usize),
}

struct Place {
    label: L,
    x: f32,
    y: f32,
    color: Rgba,
    clip: [i32; 4],
}

pub(super) struct PanelLabels {
    tabs: [Label; 3],
    pick: Label,
    clear: Label,
    close: Label,
    hint: Label,
    title: Label,
    path: Label,
    box_names: [Label; 3],
    box_nums: [Label; 12],
    content: Label,
    keys: Vec<Label>,
    vals: Vec<Label>,
    /// Eine Beschriftung je Konsolenzeile; `lines_base` ist die Nummer der ersten.
    lines: VecDeque<Label>,
    lines_base: u64,
    prompt: Label,
    input: Label,
    placeholder: Label,
    summary: Label,
    net: Vec<[Label; 3]>,
    places: Vec<Place>,
    /// Tatsächlich benutzte Scrollposition und größte mögliche, je Reiter.
    pub scroll: [f32; 3],
    pub scroll_max: [f32; 3],
    input_scroll: f32,
}

impl PanelLabels {
    pub(super) fn new(fs: &mut FontSystem) -> Self {
        Self {
            tabs: std::array::from_fn(|_| Label::new(fs)),
            pick: Label::new(fs),
            clear: Label::new(fs),
            close: Label::new(fs),
            hint: Label::new(fs),
            title: Label::new(fs),
            path: Label::new(fs),
            box_names: std::array::from_fn(|_| Label::new(fs)),
            box_nums: std::array::from_fn(|_| Label::new(fs)),
            content: Label::new(fs),
            keys: Vec::new(),
            vals: Vec::new(),
            lines: VecDeque::new(),
            lines_base: 0,
            prompt: Label::new(fs),
            input: Label::new(fs),
            placeholder: Label::new(fs),
            summary: Label::new(fs),
            net: Vec::new(),
            places: Vec::new(),
            scroll: [0.0; 3],
            scroll_max: [0.0; 3],
            input_scroll: 0.0,
        }
    }

    fn label(&self, l: L) -> &Label {
        match l {
            L::Tab(i) => &self.tabs[i],
            L::Pick => &self.pick,
            L::Clear => &self.clear,
            L::Close => &self.close,
            L::Hint => &self.hint,
            L::Title => &self.title,
            L::Path => &self.path,
            L::BoxName(i) => &self.box_names[i],
            L::BoxNum(i) => &self.box_nums[i],
            L::Content => &self.content,
            L::Key(i) => &self.keys[i],
            L::Val(i) => &self.vals[i],
            L::Line(i) => &self.lines[i],
            L::Prompt => &self.prompt,
            L::Input => &self.input,
            L::Placeholder => &self.placeholder,
            L::Summary => &self.summary,
            L::NetName(i) => &self.net[i][0],
            L::NetDetail(i) => &self.net[i][1],
            L::NetRight(i) => &self.net[i][2],
        }
    }
}

/// Text mit Umbruch auf `width`.
fn set_wrapped(l: &mut Label, fs: &mut FontSystem, text: &str, size: f32, family: &str, bold: bool, width: f32) {
    let key = (size.to_bits() ^ width.round().to_bits().rotate_left(11), bold);
    if l.text == text && l.key == key {
        return;
    }
    l.text = text.to_string();
    l.key = key;
    l.buffer.set_metrics(Metrics::new(size, (size * 1.35).round()));
    l.buffer.set_wrap(Wrap::WordOrGlyph);
    l.buffer.set_size(Some(width.max(10.0)), None);
    let weight = if bold { Weight::SEMIBOLD } else { Weight::NORMAL };
    let attrs = Attrs::new().family(Family::Name(family)).weight(weight);
    l.buffer.set_text(text, &attrs, Shaping::Advanced, None);
    l.buffer.shape_until_scroll(fs, false);
}

/// Höhe eines (umbrochenen) Textes.
fn text_h(l: &Label) -> f32 {
    l.buffer.layout_runs().count().max(1) as f32 * l.line_height()
}

fn clip_of(r: Rect) -> [i32; 4] {
    [r.x.max(0.0) as i32, r.y.max(0.0) as i32, (r.x + r.w).max(0.0) as i32, (r.y + r.h).max(0.0) as i32]
}

/// Schneidet eine Fläche auf `area` zu (für Flächen in gescrollten Listen).
fn crop(r: RoundRect, area: Rect) -> Option<RoundRect> {
    let y0 = r.y.max(area.y);
    let y1 = (r.y + r.h).min(area.y + area.h);
    (y1 > y0).then(|| RoundRect { y: y0, h: y1 - y0, ..r })
}

fn num(v: f64) -> String {
    if v == 0.0 {
        "–".into()
    } else if (v - v.round()).abs() < 0.05 {
        format!("{}", v.round() as i64)
    } else {
        format!("{v:.1}").replace('.', ",")
    }
}

/// Farben des Box-Modells (wie in anderen Browsern, etwas gedämpft für die dunkle Leiste).
const BOX_COLORS: [Rgba; 4] = [
    Rgba(0xf6, 0xb2, 0x6b, 0x55),
    Rgba(0xff, 0xe5, 0x99, 0x55),
    Rgba(0x93, 0xc4, 0x7d, 0x55),
    Rgba(0x6f, 0xa8, 0xdc, 0x70),
];
const RED: Rgba = Rgba::rgb(0xff, 0x8a, 0x7a);
const YELLOW: Rgba = Rgba::rgb(0xff, 0xd0, 0x6b);

impl Ui {
    /// Setzt die Texte der Leiste und legt Flächen an. Muss vor allen Textblöcken laufen.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn panel_prepare(
        &mut self,
        fs: &mut FontSystem,
        p: &InspLayout,
        insp: &Inspector,
        s: f32,
        cards: &mut Vec<RoundRect>,
        under: &mut Vec<Quad>,
        over: &mut Vec<Quad>,
    ) {
        let t = self.theme;
        let (uf, mf, icf) = (self.ui_family.clone(), self.mono_family.clone(), self.icon_family.clone());
        let tab_anim: Vec<f32> = (0..3).map(|i| self.anim(Hit::InspTab(i))).collect();
        let (pick_a, clear_a, close_a) = (self.anim(Hit::InspPick), self.anim(Hit::InspClear), self.anim(Hit::InspClose));
        let resize_a = self.anim(Hit::InspResize).max(if self.pressed == Some(Hit::InspResize) { 1.0 } else { 0.0 });
        let pl = &mut self.panel;
        pl.places.clear();

        let card = p.panel;
        cards.push(RoundRect::shadow(card.x, card.y, card.w, card.h, Rgba(0, 0, 0, 90), 10.0 * s, 6.0 * s));
        cards.push(RoundRect::new(card.x, card.y, card.w, card.h, t.surface, 10.0 * s));
        if resize_a > 0.0 {
            let r = p.resize;
            cards.push(RoundRect::new(r.x + r.w / 2.0 - 1.5 * s, card.y + card.h / 2.0 - 24.0 * s, 3.0 * s, 48.0 * s, t.accent.with_alpha((resize_a * 200.0) as u8), 1.5 * s));
        }

        // Reiter.
        let errors = insp.errors();
        for (i, r) in p.tabs.iter().enumerate() {
            let active = insp.tab.index() == i;
            if active {
                cards.push(RoundRect::new(r.x, r.y, r.w, r.h, t.active, 7.0 * s));
            } else if tab_anim[i] > 0.0 {
                cards.push(RoundRect::new(r.x, r.y, r.w, r.h, t.hover.with_alpha((tab_anim[i] * 255.0) as u8), 7.0 * s));
            }
            let text = match i {
                0 => "Element".to_string(),
                1 if errors > 0 => format!("Konsole · {errors}"),
                1 => "Konsole".to_string(),
                _ => "Netzwerk".to_string(),
            };
            pl.tabs[i].set(fs, &text, 12.5 * s, &uf, active);
            let l = &pl.tabs[i];
            let color = if i == 1 && errors > 0 && !active { RED } else if active { t.text } else { t.dim };
            pl.places.push(Place {
                label: L::Tab(i),
                x: (r.x + ((r.w - l.width()) / 2.0).max(4.0 * s)).round(),
                y: (r.y + (r.h - l.line_height()) / 2.0).round(),
                color,
                clip: clip_of(*r),
            });
        }
        // Knöpfe: Zielkreuz, Leeren (nicht bei "Element"), Schließen.
        let buttons = [
            (p.pick, pick_a, L::Pick, ICON_PICK, insp.picking, true),
            (p.clear, clear_a, L::Clear, ICON_CLEAR, false, insp.tab != Tab::Element),
            (p.close, close_a, L::Close, ICON_CLOSE, false, true),
        ];
        for (r, a, which, icon, on, visible) in buttons {
            if !visible {
                continue;
            }
            if on {
                cards.push(RoundRect::new(r.x, r.y, r.w, r.h, t.accent, 7.0 * s));
            } else if a > 0.0 {
                cards.push(RoundRect::new(r.x, r.y, r.w, r.h, t.hover.with_alpha((a * 255.0) as u8), 7.0 * s));
            }
            let size = if matches!(which, L::Close) { 9.0 } else { 14.0 };
            let lab = match which {
                L::Pick => &mut pl.pick,
                L::Clear => &mut pl.clear,
                _ => &mut pl.close,
            };
            lab.set(fs, icon, size * s, &icf, false);
            let (w, lh) = (lab.width(), lab.line_height());
            pl.places.push(Place {
                label: which,
                x: (r.x + (r.w - w) / 2.0).round(),
                y: (r.y + (r.h - lh) / 2.0).round(),
                color: if on { t.bg } else { t.text },
                clip: clip_of(r),
            });
        }
        let b = p.body;
        over.push(Quad { x: card.x + 10.0 * s, y: b.y - 7.0 * s, w: card.w - 20.0 * s, h: s.max(1.0), color: t.hover });
        let clip = clip_of(b);

        // Inhalte werden relativ zu ihrer Oberkante gesammelt und nach dem Scrollen verschoben.
        let mut rel: Vec<Place> = Vec::new();
        let mut rel_cards: Vec<RoundRect> = Vec::new();
        let ti = insp.tab.index();
        let (area, height) = match insp.tab {
            Tab::Element => {
                let h = element_tab(pl, fs, insp, b.w, s, t, &uf, &mf, &mut rel, &mut rel_cards);
                (b, h)
            }
            Tab::Console => {
                let h = console_tab(pl, fs, insp, b.w, s, t, &uf, &mf, &mut rel, &mut rel_cards);
                (b, h)
            }
            Tab::Network => {
                // Zusammenfassung steht fest oben, die Liste darunter scrollt.
                let list = insp.network();
                let total: u64 = list.iter().map(|r| r.size).sum();
                let blocked = list.iter().filter(|r| r.blocked).count();
                let text = if list.is_empty() {
                    "Noch keine Anfragen erfasst.".to_string()
                } else {
                    format!("{} Anfragen · {} übertragen · {blocked} blockiert", list.len(), format_size(total))
                };
                pl.summary.set(fs, &text, 11.5 * s, &uf, false);
                pl.places.push(Place { label: L::Summary, x: b.x.round(), y: b.y.round(), color: t.dim, clip });
                let area = Rect { y: b.y + 24.0 * s, h: (b.h - 24.0 * s).max(0.0), ..b };
                let sc_guess = insp.scroll[ti].clamp(0.0, pl.scroll_max[ti].max(0.0));
                let h = network_tab(pl, fs, &list, area, sc_guess, s, t, &uf, &mut rel, &mut rel_cards);
                (area, h)
            }
        };
        let max = (height - area.h).max(0.0);
        let sc = if insp.tab == Tab::Console && insp.stick_bottom { max } else { insp.scroll[ti].clamp(0.0, max) };
        pl.scroll[ti] = sc;
        pl.scroll_max[ti] = max;
        let dy = area.y - sc;
        let area_clip = clip_of(area);
        for mut place in rel {
            place.x += area.x;
            place.y = (place.y + dy).round();
            if place.y > area.y + area.h || place.y < area.y - 2000.0 * s {
                continue;
            }
            // Ein rechter Rand in `clip[2]` gilt relativ zur Fläche (z. B. Name vor der Größe).
            let right = if place.clip[2] > 0 { ((area.x + place.clip[2] as f32) as i32).min(area_clip[2]) } else { area_clip[2] };
            place.clip = [area_clip[0], area_clip[1], right, area_clip[3]];
            pl.places.push(place);
        }
        for mut c in rel_cards {
            c.x += area.x;
            c.y += dy;
            if let Some(c) = crop(c, area) {
                cards.push(c);
            }
        }

        // Eingabezeile der Konsole.
        if insp.tab == Tab::Console {
            let r = p.input;
            cards.push(RoundRect::new(r.x, r.y, r.w, r.h, t.bg, 8.0 * s));
            if insp.input_focused {
                cards.push(RoundRect::outline(r.x, r.y, r.w, r.h, t.accent.with_alpha(170), 8.0 * s, 1.0 * s));
            }
            pl.prompt.set(fs, "›", 15.0 * s, &uf, true);
            pl.places.push(Place {
                label: L::Prompt,
                x: (r.x + 10.0 * s).round(),
                y: (r.y + (r.h - pl.prompt.line_height()) / 2.0).round(),
                color: t.accent,
                clip: clip_of(r),
            });
            let text_left = r.x + 26.0 * s;
            let text_w = r.w - 36.0 * s;
            let text_clip = [text_left as i32, r.y as i32, (text_left + text_w) as i32, (r.y + r.h) as i32];
            pl.input.set(fs, &insp.input.text, 12.5 * s, &mf, false);
            let cursor_x = pl.input.x_at(insp.input.cursor);
            if cursor_x - pl.input_scroll > text_w - 2.0 * s {
                pl.input_scroll = cursor_x - text_w + 2.0 * s;
            } else if cursor_x < pl.input_scroll {
                pl.input_scroll = cursor_x;
            }
            if insp.input.text.is_empty() {
                pl.input_scroll = 0.0;
                pl.placeholder.set(fs, "JavaScript ausführen …", 12.5 * s, &uf, false);
                pl.places.push(Place {
                    label: L::Placeholder,
                    x: text_left.round(),
                    y: (r.y + (r.h - pl.placeholder.line_height()) / 2.0).round(),
                    color: t.dim.with_alpha(170),
                    clip: text_clip,
                });
            }
            let ty = (r.y + (r.h - pl.input.line_height()) / 2.0).round();
            pl.places.push(Place { label: L::Input, x: (text_left - pl.input_scroll).round(), y: ty, color: t.text, clip: text_clip });
            if insp.input_focused {
                if let Some((a, bb)) = insp.input.selection() {
                    let x0 = (text_left + pl.input.x_at(a) - pl.input_scroll).max(text_left);
                    let x1 = (text_left + pl.input.x_at(bb) - pl.input_scroll).min(text_left + text_w);
                    under.push(Quad { x: x0, y: ty, w: (x1 - x0).max(0.0), h: pl.input.line_height(), color: t.selection });
                } else {
                    let cx = text_left + cursor_x - pl.input_scroll;
                    over.push(Quad { x: cx.round(), y: ty + 1.0 * s, w: (1.5 * s).max(1.0), h: pl.input.line_height() - 2.0 * s, color: t.accent });
                }
            }
        }
    }

    /// Die Textblöcke der Leiste (nach `panel_prepare`).
    pub(super) fn panel_text<'b>(&'b self, out: &mut Vec<TextBlock<'b>>) {
        for p in &self.panel.places {
            out.push(TextBlock { buffer: &self.panel.label(p.label).buffer, x: p.x, y: p.y, color: p.color, clip: Some(p.clip) });
        }
    }

    /// Byte-Index in der Konsolen-Eingabe an der Klickposition.
    pub fn insp_input_index(&self, x: f32, layout: &Layout, text: &str) -> usize {
        let Some(p) = &layout.insp else { return 0 };
        if text.is_empty() {
            return 0;
        }
        let text_x = p.input.x + 26.0 * layout.scale - self.panel.input_scroll;
        self.panel.input.index_at(x - text_x)
    }

    /// Scrollposition und größte mögliche des aktuellen Reiters (wie zuletzt gezeichnet).
    pub fn insp_scroll(&self, tab: Tab) -> (f32, f32) {
        (self.panel.scroll[tab.index()], self.panel.scroll_max[tab.index()])
    }
}

/// Reiter "Element": Hinweis oder Name, Weg, Box-Modell, Stile. Liefert die Höhe.
#[allow(clippy::too_many_arguments)]
fn element_tab(
    pl: &mut PanelLabels,
    fs: &mut FontSystem,
    insp: &Inspector,
    w: f32,
    s: f32,
    t: &Theme,
    uf: &str,
    mf: &str,
    rel: &mut Vec<Place>,
    rel_cards: &mut Vec<RoundRect>,
) -> f32 {
    let mut y = 0.0;
    let none = [0; 4];
    let hint = match (&insp.element, insp.picking) {
        (None, true) => Some("Fahre über die Seite und klicke ein Element an. Esc bricht ab."),
        (None, false) => Some("Klicke oben auf das Zielkreuz (oder Strg+Umschalt+C) und dann auf ein Element der Seite."),
        (Some(_), true) => Some("Element anklicken · Esc bricht ab"),
        (Some(_), false) => None,
    };
    if let Some(h) = hint {
        set_wrapped(&mut pl.hint, fs, h, 12.0 * s, uf, false, w);
        rel.push(Place { label: L::Hint, x: 0.0, y, color: if insp.picking { t.accent } else { t.dim }, clip: none });
        y += text_h(&pl.hint) + 10.0 * s;
    }
    let Some(e) = &insp.element else { return y };
    set_wrapped(&mut pl.title, fs, &e.selector(), 13.5 * s, mf, true, w);
    rel.push(Place { label: L::Title, x: 0.0, y, color: t.accent, clip: none });
    y += text_h(&pl.title) + 4.0 * s;
    set_wrapped(&mut pl.path, fs, &e.path, 11.0 * s, uf, false, w);
    rel.push(Place { label: L::Path, x: 0.0, y, color: t.dim, clip: none });
    y += text_h(&pl.path) + 14.0 * s;
    y = box_model(pl, fs, e, y, w, s, t, uf, rel, rel_cards) + 14.0 * s;

    // Stile und Attribute: Name links, Wert rechts (umbrochen), Farben mit Farbfeld.
    let key_w = 104.0 * s;
    let mut props = vec![("Größe".to_string(), format!("{} × {} px", num(e.width), num(e.height)))];
    props.extend(e.props.iter().cloned());
    while pl.keys.len() < props.len() {
        pl.keys.push(Label::new(fs));
        pl.vals.push(Label::new(fs));
    }
    for (i, (k, v)) in props.iter().enumerate() {
        let swatch = css_color(v);
        let vx = key_w + if swatch.is_some() { 18.0 * s } else { 0.0 };
        pl.keys[i].set(fs, k, 11.5 * s, mf, false);
        set_wrapped(&mut pl.vals[i], fs, v, 11.5 * s, mf, false, (w - vx).max(40.0 * s));
        rel.push(Place { label: L::Key(i), x: 0.0, y, color: t.dim, clip: none });
        rel.push(Place { label: L::Val(i), x: vx, y, color: t.text, clip: none });
        if let Some([r, g, b, a]) = swatch {
            let lh = pl.vals[i].line_height();
            rel_cards.push(RoundRect::new(key_w, y + (lh - 12.0 * s) / 2.0, 12.0 * s, 12.0 * s, t.dim.with_alpha(120), 3.0 * s));
            rel_cards.push(RoundRect::new(key_w + 1.0 * s, y + (lh - 10.0 * s) / 2.0, 10.0 * s, 10.0 * s, Rgba(r, g, b, a), 2.5 * s));
        }
        y += text_h(&pl.vals[i]).max(pl.keys[i].line_height()) + 5.0 * s;
    }
    y
}

/// Das Box-Modell als verschachtelte Flächen mit den Werten. Liefert die Unterkante.
#[allow(clippy::too_many_arguments)]
fn box_model(
    pl: &mut PanelLabels,
    fs: &mut FontSystem,
    e: &Element,
    y0: f32,
    w: f32,
    s: f32,
    t: &Theme,
    uf: &str,
    rel: &mut Vec<Place>,
    rel_cards: &mut Vec<RoundRect>,
) -> f32 {
    let none = [0; 4];
    let (dx, dy, ch) = (34.0 * s, 22.0 * s, 30.0 * s);
    let total_h = ch + 6.0 * dy;
    for k in 0..4 {
        let kf = k as f32;
        rel_cards.push(RoundRect::new(kf * dx, y0 + kf * dy, w - 2.0 * kf * dx, total_h - 2.0 * kf * dy, BOX_COLORS[k], 3.0 * s));
    }
    let names = ["margin", "border", "padding"];
    let values = [e.margin, e.border, e.padding];
    for k in 0..3 {
        let kf = k as f32;
        pl.box_names[k].set(fs, names[k], 10.0 * s, uf, false);
        rel.push(Place { label: L::BoxName(k), x: kf * dx + 5.0 * s, y: y0 + kf * dy + 3.0 * s, color: t.text.with_alpha(170), clip: none });
        for side in 0..4 {
            let i = k * 4 + side;
            pl.box_nums[i].set(fs, &num(values[k][side]), 11.0 * s, uf, false);
            let (lw, lh) = (pl.box_nums[i].width(), pl.box_nums[i].line_height());
            let (x, y) = match side {
                0 => ((w - lw) / 2.0, y0 + kf * dy + (dy - lh) / 2.0),
                1 => (w - (kf + 1.0) * dx + (dx - lw) / 2.0, y0 + (total_h - lh) / 2.0),
                2 => ((w - lw) / 2.0, y0 + total_h - (kf + 1.0) * dy + (dy - lh) / 2.0),
                _ => (kf * dx + (dx - lw) / 2.0, y0 + (total_h - lh) / 2.0),
            };
            rel.push(Place { label: L::BoxNum(i), x, y, color: t.text, clip: none });
        }
    }
    let (cw, chh) = e.content_size();
    pl.content.set(fs, &format!("{} × {}", num(cw), num(chh)), 11.5 * s, uf, true);
    let (lw, lh) = (pl.content.width(), pl.content.line_height());
    rel.push(Place { label: L::Content, x: (w - lw) / 2.0, y: y0 + (total_h - lh) / 2.0, color: t.text, clip: none });
    y0 + total_h
}

/// Reiter "Konsole": eine Zeile je Meldung, Fehler und Warnungen farbig. Liefert die Höhe.
#[allow(clippy::too_many_arguments)]
fn console_tab(
    pl: &mut PanelLabels,
    fs: &mut FontSystem,
    insp: &Inspector,
    w: f32,
    s: f32,
    t: &Theme,
    uf: &str,
    mf: &str,
    rel: &mut Vec<Place>,
    rel_cards: &mut Vec<RoundRect>,
) -> f32 {
    let none = [0; 4];
    // Beschriftungen an die Zeilen angleichen: vorne weggefallene entfernen, neue anhängen.
    while pl.lines_base < insp.dropped && !pl.lines.is_empty() {
        pl.lines.pop_front();
        pl.lines_base += 1;
    }
    if pl.lines.is_empty() {
        pl.lines_base = insp.dropped;
    }
    pl.lines.truncate(insp.console.len());
    while pl.lines.len() < insp.console.len() {
        pl.lines.push_back(Label::new(fs));
    }
    if insp.console.is_empty() {
        set_wrapped(&mut pl.hint, fs, "Noch keine Meldungen. Unten lässt sich JavaScript in der Seite ausführen, z. B. document.title", 12.0 * s, uf, false, w);
        rel.push(Place { label: L::Hint, x: 0.0, y: 0.0, color: t.dim, clip: none });
        return text_h(&pl.hint);
    }
    let mut y = 0.0;
    for (i, line) in insp.console.iter().enumerate() {
        let (prefix, color, tint) = match line.level {
            Level::Error => ("", RED, Some(Rgba(0xff, 0x50, 0x50, 0x1c))),
            Level::Warn => ("", YELLOW, Some(Rgba(0xff, 0xc0, 0x40, 0x16))),
            Level::Info => ("", t.text, None),
            Level::Debug => ("", t.dim, None),
            Level::Input => ("› ", t.accent, None),
            Level::Result => ("← ", lerp(t.dim, t.text, 0.75), None),
            Level::ResultError => ("← ", RED, Some(Rgba(0xff, 0x50, 0x50, 0x1c))),
        };
        let text = format!("{prefix}{}", line.text);
        set_wrapped(&mut pl.lines[i], fs, &text, 12.0 * s, mf, false, w - 12.0 * s);
        let h = text_h(&pl.lines[i]) + 8.0 * s;
        if let Some(c) = tint {
            rel_cards.push(RoundRect::new(-4.0 * s, y, w + 8.0 * s, h - 1.0 * s, c, 4.0 * s));
        } else {
            rel_cards.push(RoundRect::new(0.0, y + h - 1.0 * s, w, s.max(1.0), t.hover.with_alpha(160), 0.0));
        }
        rel.push(Place { label: L::Line(i), x: 4.0 * s, y: y + 4.0 * s, color, clip: none });
        y += h;
    }
    y
}

/// Reiter "Netzwerk": eine Zeile je Anfrage (nur die sichtbaren bekommen Texte). Liefert die Höhe.
#[allow(clippy::too_many_arguments)]
fn network_tab(
    pl: &mut PanelLabels,
    fs: &mut FontSystem,
    list: &[crate::inspector::Request],
    area: Rect,
    scroll: f32,
    s: f32,
    t: &Theme,
    uf: &str,
    rel: &mut Vec<Place>,
    rel_cards: &mut Vec<RoundRect>,
) -> f32 {
    let row_h = ROW_H * s;
    let first = (scroll / row_h).floor().max(0.0) as usize;
    let count = (area.h / row_h).ceil() as usize + 2;
    let visible: Vec<usize> = (first..(first + count).min(list.len())).collect();
    while pl.net.len() < visible.len() {
        pl.net.push([Label::new(fs), Label::new(fs), Label::new(fs)]);
    }
    for (slot, &i) in visible.iter().enumerate() {
        let r = &list[i];
        let y = i as f32 * row_h;
        let right = if r.blocked {
            "blockiert".to_string()
        } else {
            let mut parts = Vec::new();
            if r.status > 0 {
                parts.push(r.status.to_string());
            }
            parts.push(format_size(r.size));
            parts.push(format_duration(r.duration));
            parts.join(" · ")
        };
        let kind = if r.kind.is_empty() { "andere" } else { r.kind.as_str() };
        let [name, detail, right_l] = &mut pl.net[slot];
        name.set(fs, &r.name(), 12.5 * s, uf, false);
        detail.set(fs, &format!("{kind} · {}", r.host()), 11.0 * s, uf, false);
        right_l.set(fs, &right, 11.0 * s, uf, false);
        let rw = right_l.width();
        let status_color = if r.blocked || r.status >= 400 { RED } else { t.dim };
        let limit = [0, 0, (area.w - rw - 10.0 * s).max(1.0) as i32, 0];
        rel.push(Place { label: L::NetName(slot), x: 0.0, y: y + 4.0 * s, color: if r.blocked { RED } else { t.text }, clip: limit });
        rel.push(Place { label: L::NetDetail(slot), x: 0.0, y: y + 4.0 * s + name.line_height(), color: t.dim, clip: limit });
        rel.push(Place { label: L::NetRight(slot), x: area.w - rw, y: y + 4.0 * s, color: status_color, clip: [0; 4] });
        rel_cards.push(RoundRect::new(0.0, y + row_h - 1.0 * s, area.w, s.max(1.0), t.hover.with_alpha(160), 0.0));
    }
    list.len() as f32 * row_h
}
