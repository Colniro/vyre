//! Der einfache Inspektor (F12): Element-Picker, Konsole mit Eingabe und Netzwerk-Liste.
//!
//! Vyre zeichnet die Leiste rechts selbst; die Daten liefert ein Skript, das beim Öffnen in die
//! Seite gelegt wird. Es meldet sich wie das Passwort-Skript mit Nachrichten, die mit
//! `__vyre_insp:` beginnen (Servo über `console.log`, WebView2 über `postMessage`), danach JSON:
//! `{"t": "hover" | "pick" | "pickend" | "net" | "eval" | "blocked", "d": …}`.
//! Blockierte Anfragen meldet der Werbeblocker auf demselben Weg.
//!
//! Das Netzwerk kommt aus den Resource Timings der Seite (Adresse, Art, Größe, Dauer, Status),
//! damit es bei Servo und WebView2 gleich funktioniert.

use serde_json::Value;

use crate::ui::TextField;

pub const PREFIX: &str = "__vyre_insp:";
/// So viele Konsolenzeilen bleiben erhalten.
const MAX_LINES: usize = 500;
const MAX_REQUESTS: usize = 2000;
/// Breite der Leiste in logischen Pixeln (Standard, kleinste, größte).
pub const DEFAULT_W: f32 = 380.0;
pub const MIN_W: f32 = 300.0;
pub const MAX_W: f32 = 900.0;

/// Ein Element der Seite, wie der Picker es meldet.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Element {
    pub tag: String,
    pub id: String,
    pub classes: Vec<String>,
    /// Weg von oben: `html > body > main > div.x`.
    pub path: String,
    pub width: f64,
    pub height: f64,
    /// Oben, rechts, unten, links (px).
    pub margin: [f64; 4],
    pub border: [f64; 4],
    pub padding: [f64; 4],
    /// Berechnete Stile und Attribute: Name, Wert.
    pub props: Vec<(String, String)>,
}

impl Element {
    /// `div#main.inhalt.breit`
    pub fn selector(&self) -> String {
        let mut s = self.tag.clone();
        if !self.id.is_empty() {
            s += &format!("#{}", self.id);
        }
        for c in &self.classes {
            s += &format!(".{c}");
        }
        s
    }

    /// Größe des Inhalts (ohne Innenabstand und Rahmen).
    pub fn content_size(&self) -> (f64, f64) {
        let w = self.width - self.padding[1] - self.padding[3] - self.border[1] - self.border[3];
        let h = self.height - self.padding[0] - self.padding[2] - self.border[0] - self.border[2];
        (w.max(0.0), h.max(0.0))
    }
}

/// Eine Anfrage der Seite.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Request {
    pub url: String,
    /// "document", "script", "img", "css", "fetch" … oder "blockiert".
    pub kind: String,
    /// Dauer in ms.
    pub duration: f64,
    /// Übertragene Bytes (0: unbekannt, z. B. aus dem Cache oder fremde Seite ohne Freigabe).
    pub size: u64,
    /// HTTP-Status (0: unbekannt).
    pub status: u16,
    pub blocked: bool,
}

impl Request {
    /// Kurzer Name: letzter Teil des Pfads, sonst der Host.
    pub fn name(&self) -> String {
        let Ok(u) = url::Url::parse(&self.url) else { return self.url.clone() };
        let last = u.path_segments().and_then(|mut s| s.next_back().map(str::to_string)).unwrap_or_default();
        if last.is_empty() { u.host_str().unwrap_or(&self.url).to_string() } else { last }
    }

    pub fn host(&self) -> String {
        url::Url::parse(&self.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
    /// Eingabe in der Konsole.
    Input,
    /// Ergebnis einer Eingabe.
    Result,
    /// Fehler einer Eingabe.
    ResultError,
}

impl Level {
    pub fn from_name(name: &str) -> Self {
        match name {
            "fehler" => Level::Error,
            "warnung" => Level::Warn,
            "debug" => Level::Debug,
            _ => Level::Info,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub level: Level,
    pub text: String,
}

#[derive(Debug, PartialEq)]
pub enum Message {
    Hover(Element),
    Pick(Element),
    /// Picker beendet (Esc in der Seite).
    PickEnd,
    Network(Vec<Request>),
    Eval { text: String, error: bool },
    Blocked(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Element,
    Console,
    Network,
}

pub const TABS: [Tab; 3] = [Tab::Element, Tab::Console, Tab::Network];

impl Tab {
    pub fn index(self) -> usize {
        TABS.iter().position(|t| *t == self).unwrap_or(0)
    }
}

/// Zustand der Leiste.
pub struct Inspector {
    pub open: bool,
    pub tab: Tab,
    /// Picker läuft (Maus über der Seite hebt Elemente hervor).
    pub picking: bool,
    /// Ausgewähltes Element; beim Picken das unter der Maus.
    pub element: Option<Element>,
    pub console: Vec<Line>,
    /// Wie viele Konsolenzeilen vorne schon weggefallen sind (für die Beschriftungen).
    pub dropped: u64,
    pub requests: Vec<Request>,
    /// Vom Werbeblocker gestoppte Adressen der aktuellen Seite (auch bei geschlossener Leiste).
    pub blocked: Vec<String>,
    pub input: TextField,
    pub input_focused: bool,
    history: Vec<String>,
    /// Position beim Blättern in früheren Eingaben (Pfeiltasten).
    history_pos: Option<usize>,
    /// Scrollposition je Reiter (px, 0 = oben).
    pub scroll: [f32; 3],
    /// Konsole: am Ende kleben, solange man nicht hochgescrollt hat.
    pub stick_bottom: bool,
    pub width: f32,
    /// Für welche Seite die Daten gelten: Tab-Nummer und Adresse (ohne #…).
    pub page: Option<(usize, String)>,
}

impl Default for Inspector {
    fn default() -> Self {
        Self {
            open: false,
            tab: Tab::Element,
            picking: false,
            element: None,
            console: Vec::new(),
            dropped: 0,
            requests: Vec::new(),
            blocked: Vec::new(),
            input: TextField::default(),
            input_focused: false,
            history: Vec::new(),
            history_pos: None,
            scroll: [0.0; 3],
            stick_bottom: true,
            width: DEFAULT_W,
            page: None,
        }
    }
}

impl Inspector {
    pub fn push_line(&mut self, level: Level, text: &str) {
        let text: String = text.chars().take(4000).collect();
        self.console.push(Line { level, text });
        if self.console.len() > MAX_LINES {
            let extra = self.console.len() - MAX_LINES;
            self.console.drain(..extra);
            self.dropped += extra as u64;
        }
    }

    /// Eine neue Seite: alles zur alten verwerfen.
    pub fn reset_page(&mut self, page: Option<(usize, String)>) {
        self.page = page;
        self.clear_console();
        self.requests.clear();
        self.blocked.clear();
        self.element = None;
        self.picking = false;
        self.scroll = [0.0; 3];
        self.stick_bottom = true;
    }

    pub fn clear_console(&mut self) {
        self.dropped += self.console.len() as u64;
        self.console.clear();
    }

    /// Verarbeitet eine Nachricht der Seite. `true`, wenn sich etwas Sichtbares geändert hat.
    pub fn handle(&mut self, message: Message) -> bool {
        match message {
            Message::Hover(e) => {
                if !self.picking {
                    return false;
                }
                self.element = Some(e);
            }
            Message::Pick(e) => {
                self.element = Some(e);
                self.picking = false;
                self.tab = Tab::Element;
            }
            Message::PickEnd => self.picking = false,
            Message::Network(list) => {
                self.requests.extend(list);
                if self.requests.len() > MAX_REQUESTS {
                    let extra = self.requests.len() - MAX_REQUESTS;
                    self.requests.drain(..extra);
                }
            }
            Message::Eval { text, error } => self.push_line(if error { Level::ResultError } else { Level::Result }, &text),
            Message::Blocked(url) => {
                if self.blocked.len() < MAX_REQUESTS {
                    self.blocked.push(url);
                }
            }
        }
        true
    }

    /// Alle Anfragen für die Liste: geladene, dann blockierte.
    pub fn network(&self) -> Vec<Request> {
        let mut all = self.requests.clone();
        all.extend(self.blocked.iter().map(|u| Request { url: u.clone(), kind: "blockiert".into(), blocked: true, ..Default::default() }));
        all
    }

    /// Enter in der Konsole: Eingabe merken und das Skript dafür liefern.
    pub fn submit(&mut self) -> Option<String> {
        let code = self.input.text.trim().to_string();
        if code.is_empty() {
            return None;
        }
        self.push_line(Level::Input, &code);
        if self.history.last() != Some(&code) {
            self.history.push(code.clone());
        }
        self.history_pos = None;
        self.input.set("");
        self.stick_bottom = true;
        Some(eval_script(&code))
    }

    /// Pfeil hoch/runter: frühere Eingaben.
    pub fn history_step(&mut self, up: bool) {
        if self.history.is_empty() {
            return;
        }
        let n = self.history.len();
        let pos = match (self.history_pos, up) {
            (None, true) => Some(n - 1),
            (None, false) => None,
            (Some(p), true) => Some(p.saturating_sub(1)),
            (Some(p), false) if p + 1 < n => Some(p + 1),
            (Some(_), false) => None,
        };
        self.history_pos = pos;
        let text = pos.map(|p| self.history[p].clone()).unwrap_or_default();
        self.input.set(&text);
    }

    /// Zahl der Fehler in der Konsole (für den Reiter).
    pub fn errors(&self) -> usize {
        self.console.iter().filter(|l| l.level == Level::Error).count()
    }
}

/// Liest eine Nachricht des Seiten-Skripts.
pub fn parse(message: &str) -> Option<Message> {
    let json = message.strip_prefix(PREFIX)?;
    let v: Value = serde_json::from_str(json).ok()?;
    let d = v.get("d").cloned().unwrap_or(Value::Null);
    Some(match v.get("t")?.as_str()? {
        "hover" => Message::Hover(element(&d)?),
        "pick" => Message::Pick(element(&d)?),
        "pickend" => Message::PickEnd,
        "net" => Message::Network(d.as_array()?.iter().filter_map(request).collect()),
        "eval" => Message::Eval {
            text: d.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string(),
            error: d.get("err").and_then(|e| e.as_bool()).unwrap_or(false),
        },
        "blocked" => Message::Blocked(d.as_str()?.to_string()),
        _ => return None,
    })
}

/// Nachricht für eine vom Werbeblocker gestoppte Anfrage.
pub fn blocked_message(url: &str) -> String {
    format!("{PREFIX}{}", serde_json::json!({ "t": "blocked", "d": url }))
}

fn element(d: &Value) -> Option<Element> {
    let s = |k: &str| d.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let n = |k: &str| d.get(k).and_then(|v| v.as_f64()).unwrap_or(0.0);
    let four = |k: &str| -> [f64; 4] {
        let a = d.get(k).and_then(|v| v.as_array());
        let g = |i: usize| a.and_then(|a| a.get(i)).and_then(|v| v.as_f64()).unwrap_or(0.0);
        [g(0), g(1), g(2), g(3)]
    };
    let tag = s("tag");
    if tag.is_empty() {
        return None;
    }
    Some(Element {
        tag,
        id: s("id"),
        classes: d
            .get("cls")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
        path: s("path"),
        width: n("w"),
        height: n("h"),
        margin: four("m"),
        border: four("b"),
        padding: four("p"),
        props: d
            .get("props")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|p| {
                        let p = p.as_array()?;
                        Some((p.first()?.as_str()?.to_string(), p.get(1)?.as_str()?.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

fn request(v: &Value) -> Option<Request> {
    Some(Request {
        url: v.get("n")?.as_str()?.to_string(),
        kind: v.get("k").and_then(|k| k.as_str()).unwrap_or("").to_string(),
        duration: v.get("d").and_then(|d| d.as_f64()).unwrap_or(0.0),
        size: v.get("s").and_then(|s| s.as_f64()).unwrap_or(0.0).max(0.0) as u64,
        status: v.get("st").and_then(|s| s.as_u64()).unwrap_or(0) as u16,
        blocked: false,
    })
}

/// "1,2 KB", "3,4 MB"
pub fn format_size(bytes: u64) -> String {
    match bytes {
        0 => "–".into(),
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{:.1} KB", b as f64 / 1024.0).replace('.', ","),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)).replace('.', ","),
    }
}

/// "12 ms", "1,4 s"
pub fn format_duration(ms: f64) -> String {
    if ms <= 0.0 {
        "–".into()
    } else if ms < 1000.0 {
        format!("{} ms", ms.round())
    } else {
        format!("{:.1} s", ms / 1000.0).replace('.', ",")
    }
}

/// Farbe aus einem CSS-Wert wie `rgb(1, 2, 3)` oder `rgba(1, 2, 3, 0.5)`.
pub fn css_color(value: &str) -> Option<[u8; 4]> {
    let v = value.trim();
    let inner = v.strip_prefix("rgba(").or_else(|| v.strip_prefix("rgb("))?.strip_suffix(')')?;
    let parts: Vec<f32> = inner.split([',', ' ', '/']).filter(|p| !p.is_empty()).filter_map(|p| p.trim().parse().ok()).collect();
    if parts.len() < 3 {
        return None;
    }
    let a = parts.get(3).copied().unwrap_or(1.0);
    let c = |x: f32| x.round().clamp(0.0, 255.0) as u8;
    Some([c(parts[0]), c(parts[1]), c(parts[2]), c(a * 255.0)])
}

/// Legt das Skript in die Seite (beim Öffnen und nach jedem Laden). Mehrfach aufrufen ist
/// harmlos: Es startet nur die Netzwerk-Meldungen neu (alles seit dem Laden noch einmal).
pub const PAGE_SCRIPT: &str = r##"(function () {
  if (!window.__vyreInsp) {
    var log = console.log.bind(console);
    var send = function (t, d) {
      var m = "__vyre_insp:" + JSON.stringify({ t: t, d: d });
      if (window.chrome && window.chrome.webview) window.chrome.webview.postMessage(m); else log(m);
    };
    var px = function (v) { return Math.round((parseFloat(v) || 0) * 10) / 10; };
    var name = function (el) {
      var s = el.tagName.toLowerCase();
      if (el.id) s += "#" + el.id;
      if (typeof el.className === "string" && el.className.trim()) s += "." + el.className.trim().split(/\s+/).slice(0, 3).join(".");
      return s;
    };
    var path = function (el) {
      var parts = [];
      for (var e = el; e && e.nodeType === 1 && parts.length < 8; e = e.parentElement) parts.unshift(name(e));
      return parts.join(" > ");
    };
    var info = function (el) {
      var cs = getComputedStyle(el), r = el.getBoundingClientRect();
      var four = function (p, s) { return [px(cs[p + "Top" + s]), px(cs[p + "Right" + s]), px(cs[p + "Bottom" + s]), px(cs[p + "Left" + s])]; };
      var props = [
        ["display", cs.display], ["position", cs.position], ["color", cs.color],
        ["background", cs.backgroundColor], ["font-family", cs.fontFamily], ["font-size", cs.fontSize],
        ["font-weight", cs.fontWeight], ["line-height", cs.lineHeight], ["box-sizing", cs.boxSizing],
        ["overflow", cs.overflow], ["z-index", cs.zIndex], ["opacity", cs.opacity]
      ];
      ["href", "src", "alt", "type", "name", "role"].forEach(function (a) {
        var v = el.getAttribute && el.getAttribute(a);
        if (v) props.push(["@" + a, String(v).slice(0, 300)]);
      });
      var text = (el.innerText || el.textContent || "").trim().replace(/\s+/g, " ");
      if (text) props.push(["text", text.slice(0, 200)]);
      return {
        tag: el.tagName.toLowerCase(), id: el.id || "",
        cls: typeof el.className === "string" ? el.className.trim().split(/\s+/).filter(Boolean).slice(0, 8) : [],
        path: path(el), w: px(r.width), h: px(r.height),
        m: four("margin", ""), b: four("border", "Width"), p: four("padding", ""),
        props: props.filter(function (p) { return p[1] !== undefined && p[1] !== ""; })
      };
    };
    // Hervorhebung wie in anderen Browsern: Außenabstand, Rahmen, Innenabstand, Inhalt.
    var box = null, tip = null, shown = null;
    var layer = function (color) {
      var d = document.createElement("div");
      d.style.cssText = "position:fixed;pointer-events:none;box-sizing:border-box;border-style:solid;border-color:" + color + ";background:transparent";
      return d;
    };
    var ensure = function () {
      if (box && box.isConnected) return;
      box = document.createElement("div");
      box.style.cssText = "position:fixed;left:0;top:0;width:0;height:0;pointer-events:none;z-index:2147483647";
      box.m = layer("rgba(246,178,107,.55)"); box.b = layer("rgba(255,229,153,.65)");
      box.p = layer("rgba(147,196,125,.55)"); box.c = layer("rgba(0,0,0,0)");
      box.c.style.background = "rgba(111,168,220,.55)"; box.c.style.borderWidth = "0";
      tip = document.createElement("div");
      tip.style.cssText = "position:fixed;pointer-events:none;font:12px/1.4 system-ui,sans-serif;background:#21212b;color:#e8e8f0;padding:3px 8px;border-radius:5px;white-space:nowrap;box-shadow:0 2px 8px rgba(0,0,0,.4)";
      [box.m, box.b, box.p, box.c, tip].forEach(function (d) { box.appendChild(d); });
      (document.body || document.documentElement).appendChild(box);
    };
    var draw = function (el) {
      shown = el;
      if (!el || !el.getBoundingClientRect) { if (box) box.style.display = "none"; return; }
      ensure();
      box.style.display = "block";
      var i = info(el), r = el.getBoundingClientRect();
      var set = function (d, x, y, w, h, bw) {
        d.style.left = x + "px"; d.style.top = y + "px"; d.style.width = Math.max(0, w) + "px"; d.style.height = Math.max(0, h) + "px";
        if (bw) d.style.borderWidth = bw.map(function (v) { return Math.max(0, v) + "px"; }).join(" ");
      };
      var m = i.m, b = i.b, p = i.p;
      set(box.m, r.left - m[3], r.top - m[0], r.width + m[1] + m[3], r.height + m[0] + m[2], m);
      set(box.b, r.left, r.top, r.width, r.height, b);
      set(box.p, r.left + b[3], r.top + b[0], r.width - b[1] - b[3], r.height - b[0] - b[2], p);
      set(box.c, r.left + b[3] + p[3], r.top + b[0] + p[0], r.width - b[1] - b[3] - p[1] - p[3], r.height - b[0] - b[2] - p[0] - p[2]);
      tip.textContent = name(el) + "  " + Math.round(r.width) + " × " + Math.round(r.height);
      var ty = r.top - 28 >= 0 ? r.top - 28 : Math.min(r.bottom + 6, innerHeight - 26);
      tip.style.left = Math.max(4, Math.min(r.left, innerWidth - 260)) + "px"; tip.style.top = ty + "px";
    };
    var picking = false, last = 0, hovered = null;
    var stop = function (ev) { if (!picking) return; ev.preventDefault(); ev.stopPropagation(); ev.stopImmediatePropagation(); };
    var move = function (ev) {
      if (!picking) return;
      var el = ev.target;
      if (!el || el === hovered || el.nodeType !== 1) return;
      hovered = el; draw(el);
      var now = Date.now();
      if (now - last > 50) { last = now; send("hover", info(el)); }
    };
    var click = function (ev) {
      if (!picking) return;
      stop(ev);
      picking = false;
      var el = ev.target && ev.target.nodeType === 1 ? ev.target : hovered;
      if (el) { draw(el); send("pick", info(el)); }
    };
    var key = function (ev) {
      if (picking && ev.key === "Escape") { stop(ev); picking = false; draw(null); send("pickend", null); }
    };
    addEventListener("mousemove", move, true);
    addEventListener("mouseover", move, true);
    addEventListener("click", click, true);
    ["mousedown", "mouseup", "pointerdown", "pointerup", "auxclick", "dblclick", "contextmenu"].forEach(function (t) { addEventListener(t, stop, true); });
    addEventListener("keydown", key, true);
    addEventListener("scroll", function () { if (shown) draw(shown); }, true);
    addEventListener("resize", function () { if (shown) draw(shown); });

    // Netzwerk: neue Einträge der Resource Timings alle halbe Sekunde melden.
    var seen = 0, navSent = false, timer = 0;
    var flush = function () {
      var out = [];
      if (!navSent) {
        navSent = true;
        var nav = performance.getEntriesByType && performance.getEntriesByType("navigation")[0];
        out.push({ n: location.href, k: "document", d: nav ? Math.round(nav.duration || nav.responseEnd || 0) : 0,
                   s: nav ? (nav.transferSize || nav.encodedBodySize || 0) : 0, st: nav ? (nav.responseStatus || 0) : 0 });
      }
      var list = performance.getEntriesByType ? performance.getEntriesByType("resource") : [];
      for (var i = seen; i < list.length; i++) {
        var e = list[i];
        out.push({ n: e.name, k: e.initiatorType || "", d: Math.round(e.duration || 0), s: e.transferSize || e.encodedBodySize || 0, st: e.responseStatus || 0 });
      }
      seen = list.length;
      if (out.length) send("net", out);
    };
    window.__vyreInsp = {
      start: function () { seen = 0; navSent = false; clearInterval(timer); flush(); timer = setInterval(flush, 500); },
      stop: function () { clearInterval(timer); picking = false; draw(null); },
      pick: function (on) { picking = on; hovered = null; if (!on) draw(null); },
      show: function () { draw(shown); }
    };
  }
  window.__vyreInsp.start();
})();"##;

/// Picker an oder aus.
pub fn pick_script(on: bool) -> String {
    format!("window.__vyreInsp&&window.__vyreInsp.pick({on})")
}

/// Leiste geschlossen: Meldungen und Hervorhebung beenden.
pub const STOP_SCRIPT: &str = "window.__vyreInsp&&window.__vyreInsp.stop()";

/// Führt eine Eingabe der Konsole aus und meldet das Ergebnis als Text.
pub fn eval_script(code: &str) -> String {
    format!(
        r##"(function () {{
  var log = console.log.bind(console);
  var send = function (d) {{
    var m = "__vyre_insp:" + JSON.stringify({{ t: "eval", d: d }});
    if (window.chrome && window.chrome.webview) window.chrome.webview.postMessage(m); else log(m);
  }};
  var fmt = function (v, depth) {{
    var t = typeof v;
    if (v === null) return "null";
    if (t === "undefined") return "undefined";
    if (t === "string") return depth ? JSON.stringify(v) : v;
    if (t === "number" || t === "boolean" || t === "bigint") return String(v);
    if (t === "function") return "ƒ " + (v.name || "anonym") + "()";
    if (t === "symbol") return v.toString();
    try {{
      if (v instanceof Error) return v.name + ": " + v.message;
      if (typeof Element !== "undefined" && v instanceof Element) {{
        var s = "<" + v.tagName.toLowerCase();
        if (v.id) s += "#" + v.id;
        if (typeof v.className === "string" && v.className.trim()) s += "." + v.className.trim().split(/\s+/).join(".");
        return s + ">";
      }}
      if (depth > 2) return Array.isArray(v) ? "[…]" : "{{…}}";
      var list = Array.isArray(v) || (typeof NodeList !== "undefined" && v instanceof NodeList) || (typeof HTMLCollection !== "undefined" && v instanceof HTMLCollection);
      if (list) {{
        var items = [];
        for (var i = 0; i < Math.min(v.length, 30); i++) items.push(fmt(v[i], depth + 1));
        return "(" + v.length + ") [" + items.join(", ") + (v.length > 30 ? ", …" : "") + "]";
      }}
      var keys = Object.keys(v), parts = [];
      for (var k = 0; k < Math.min(keys.length, 30); k++) parts.push(keys[k] + ": " + fmt(v[keys[k]], depth + 1));
      return "{{" + parts.join(", ") + (keys.length > 30 ? ", …" : "") + "}}";
    }} catch (e) {{
      return String(v);
    }}
  }};
  try {{
    var r = (0, eval)({code});
    if (r && typeof r.then === "function") {{
      send({{ text: "Promise …", err: false }});
      r.then(function (x) {{ send({{ text: "→ " + fmt(x, 0), err: false }}); }}, function (e) {{ send({{ text: "Promise abgelehnt: " + fmt(e, 0), err: true }}); }});
    }} else send({{ text: fmt(r, 0), err: false }});
  }} catch (e) {{
    send({{ text: (e && e.name ? e.name + ": " + e.message : String(e)), err: true }});
  }}
}})()"##,
        code = crate::engine::js_string(code)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nachrichten_lesen() {
        let m = parse(r#"__vyre_insp:{"t":"pick","d":{"tag":"div","id":"a","cls":["x","y"],"path":"html > body > div#a.x.y","w":100,"h":40,"m":[1,2,3,4],"b":[1,1,1,1],"p":[5,5,5,5],"props":[["display","block"]]}}"#);
        let Some(Message::Pick(e)) = m else { panic!("{m:?}") };
        assert_eq!(e.selector(), "div#a.x.y");
        assert_eq!(e.margin, [1.0, 2.0, 3.0, 4.0]);
        assert_eq!(e.content_size(), (88.0, 28.0));
        assert_eq!(e.props, [("display".to_string(), "block".to_string())]);

        let m = parse(r#"__vyre_insp:{"t":"net","d":[{"n":"https://a.de/x/app.js","k":"script","d":12,"s":2048,"st":200}]}"#);
        let Some(Message::Network(list)) = m else { panic!() };
        assert_eq!(list[0].name(), "app.js");
        assert_eq!(list[0].status, 200);
        assert_eq!(parse(&blocked_message("https://ads.x/p.gif")), Some(Message::Blocked("https://ads.x/p.gif".into())));
        assert_eq!(parse(r#"__vyre_insp:{"t":"eval","d":{"text":"42","err":false}}"#), Some(Message::Eval { text: "42".into(), error: false }));
        assert_eq!(parse("__vyre_pw:focus"), None);
        assert_eq!(parse(r#"__vyre_insp:{"t":"hover","d":{}}"#), None);
    }

    #[test]
    fn konsole_und_verlauf() {
        let mut i = Inspector::default();
        i.input.set("1 + 1");
        let script = i.submit().unwrap();
        assert!(script.contains(r#"(0, eval)("1 + 1")"#));
        assert_eq!(i.console.last().unwrap().level, Level::Input);
        assert!(i.input.text.is_empty());
        i.input.set("document.title");
        i.submit();
        i.history_step(true);
        assert_eq!(i.input.text, "document.title");
        i.history_step(true);
        assert_eq!(i.input.text, "1 + 1");
        i.history_step(false);
        i.history_step(false);
        assert_eq!(i.input.text, "");
        for n in 0..600 {
            i.push_line(Level::Info, &n.to_string());
        }
        assert_eq!(i.console.len(), MAX_LINES);
        assert_eq!(i.console.last().unwrap().text, "599");
    }

    #[test]
    fn hover_nur_beim_picken() {
        let mut i = Inspector::default();
        let e = Element { tag: "p".into(), ..Default::default() };
        assert!(!i.handle(Message::Hover(e.clone())));
        i.picking = true;
        assert!(i.handle(Message::Hover(e.clone())));
        i.handle(Message::Pick(e));
        assert!(!i.picking);
        i.handle(Message::Blocked("https://t.x/a.js".into()));
        assert!(i.network().last().unwrap().blocked);
    }

    #[test]
    fn formatieren() {
        assert_eq!(format_size(0), "–");
        assert_eq!(format_size(1536), "1,5 KB");
        assert_eq!(format_duration(12.4), "12 ms");
        assert_eq!(format_duration(1450.0), "1,4 s");
        assert_eq!(css_color("rgb(255, 0, 10)"), Some([255, 0, 10, 255]));
        assert_eq!(css_color("rgba(0, 0, 0, 0)"), Some([0, 0, 0, 0]));
        assert_eq!(css_color("red"), None);
    }
}
