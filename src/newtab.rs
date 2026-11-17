//! Das `vyre://`-Protokoll: die Neuer-Tab-Seite mit Suche und Kacheln.
//!
//! - `vyre://neuer-tab`: die Seite selbst (beim ersten Start mit Wahl der Suchmaschine)
//! - `vyre://suche?q=…`: leitet zur Suchmaschine oder zum !Bang weiter
//! - `vyre://aktion/…?t=<schlüssel>`: ändert Kacheln oder Suchmaschine und kehrt zurück
//!
//! Aktionen verlangen einen bei jedem Start neu erzeugten Schlüssel, den nur die
//! Neuer-Tab-Seite kennt. Fremde Seiten können sie daher nicht auslösen.

use std::collections::hash_map::RandomState;
use std::future::Future;
use std::hash::{BuildHasher, Hasher};
use std::pin::Pin;
use std::sync::Arc;

use http::HeaderValue;
use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, ProtocolHandler, Request, ResourceFetchTiming, Response,
    ResponseBody,
};
use url::Url;

use crate::history::{self, Store};
use crate::search::{self, ENGINES};
use crate::settings::{self, Tile};

pub const NEW_TAB_URL: &str = "vyre://neuer-tab";
pub const HISTORY_URL: &str = "vyre://verlauf";

pub struct VyreProtocol {
    settings: settings::Shared,
    history: Arc<Store>,
    token: String,
}

impl VyreProtocol {
    pub fn new(settings: settings::Shared, history: Arc<Store>) -> Self {
        let a = RandomState::new().build_hasher().finish();
        let mut h = RandomState::new().build_hasher();
        h.write_u64(a ^ std::process::id() as u64);
        Self { settings, history, token: format!("{a:016x}{:016x}", h.finish()) }
    }

    /// Erzeugt das HTML für eine `vyre://`-Adresse.
    pub fn page(&self, url: &Url) -> String {
        let param = |name: &str| url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned());
        match url.host_str().unwrap_or("") {
            "neuer-tab" => self.new_tab(),
            "verlauf" => self.history_page(&param("q").unwrap_or_default()),
            "suche" => {
                let q = param("q").unwrap_or_default();
                let engine = self.settings.get().search_engine.clone();
                match search::search_url(&q, engine.as_deref()) {
                    Some(target) => redirect(target.as_str()),
                    None => redirect(NEW_TAB_URL),
                }
            }
            "aktion" => {
                if param("t").as_deref() != Some(self.token.as_str()) {
                    log::warn!("vyre://aktion ohne gültigen Schlüssel abgelehnt: {url}");
                    return message("Diese Aktion ist nur von der Neuer-Tab-Seite aus erlaubt.");
                }
                redirect(self.action(url.path(), &param))
            }
            _ => message("Diese Vyre-Seite gibt es nicht."),
        }
    }

    /// Führt eine Aktion aus und liefert die Seite, zu der es danach zurückgeht.
    fn action(&self, path: &str, param: &dyn Fn(&str) -> Option<String>) -> &'static str {
        match path {
            "/verlauf-loeschen" => {
                let seconds = match param("zeit").as_deref() {
                    Some("stunde") => Some(3600),
                    Some("tag") => Some(86400),
                    Some("woche") => Some(7 * 86400),
                    Some("alles") => None,
                    _ => return HISTORY_URL,
                };
                self.history.delete_recent(seconds);
                return HISTORY_URL;
            }
            "/verlauf-weg" => {
                if let Some(url) = param("url") {
                    self.history.delete_url(&url);
                }
                return HISTORY_URL;
            }
            "/suchmaschine" => {
                if let Some(id) = param("id").filter(|id| ENGINES.iter().any(|e| e.id == id)) {
                    self.settings.update(|s| s.search_engine = Some(id));
                }
            }
            "/kachel-neu" => {
                let raw = param("url").unwrap_or_default();
                // Nur echte Adressen, keine Suchbegriffe.
                let Some(url) = search::resolve_input(&raw, None).filter(|u| !is_search(u, &raw)) else { return NEW_TAB_URL };
                let title = param("titel")
                    .map(|t| t.trim().to_string())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| url.host_str().unwrap_or("Seite").trim_start_matches("www.").to_string());
                self.settings.update(|s| {
                    if !s.tiles.iter().any(|t| t.url == url.as_str()) {
                        s.tiles.push(Tile { title, url: url.to_string() });
                    }
                });
            }
            "/kachel-weg" => {
                if let Some(url) = param("url") {
                    self.settings.update(|s| s.tiles.retain(|t| t.url != url));
                }
            }
            other => log::warn!("unbekannte vyre-Aktion: {other}"),
        }
        NEW_TAB_URL
    }

    fn history_page(&self, query: &str) -> String {
        let entries = self.history.recent(query, 500);
        let mut rows = String::new();
        let mut group = "";
        for e in &entries {
            let age = history::now() - e.last;
            let g = match age {
                ..3600 => "Letzte Stunde",
                ..86400 => "Heute",
                ..604800 => "Diese Woche",
                _ => "Älter",
            };
            if g != group {
                group = g;
                rows += &format!("<h3>{g}</h3>");
            }
            let title = if e.title.trim().is_empty() { e.url.as_str() } else { e.title.as_str() };
            rows += &format!(
                r#"<div class="entry"><a href="{url}"><span class="t">{title}</span><span class="u">{url_short}</span></a><span class="when">{when}</span><button class="del" title="Eintrag löschen" data-url="{url}"></button></div>"#,
                url = esc(&e.url),
                title = esc(title),
                url_short = esc(e.url.split("://").nth(1).unwrap_or(&e.url).trim_end_matches('/')),
                when = esc(&history::ago(e.last)),
            );
        }
        if entries.is_empty() {
            rows = format!(
                "<p class=\"empty\">{}</p>",
                if query.is_empty() { "Der Verlauf ist leer." } else { "Nichts gefunden." }
            );
        }
        format!(
            r##"<!doctype html>
<html lang="de"><head><meta charset="utf-8"><title>Verlauf</title>
<style>{CSS}{HISTORY_CSS}</style></head>
<body><div class="page">
  <h1>Verlauf</h1>
  <form method="get" action="vyre://verlauf" autocomplete="off"><input name="q" value="{q}" placeholder="Verlauf durchsuchen"></form>
  <div class="actions">
    <span>Löschen:</span>
    <button data-zeit="stunde">Letzte Stunde</button>
    <button data-zeit="tag">Heute</button>
    <button data-zeit="woche">Diese Woche</button>
    <button data-zeit="alles" class="danger">Alles</button>
  </div>
  {rows}
</div>
<script>
var T = {token_js};
function go(path, params) {{
  var q = Object.keys(params).map(function (k) {{ return k + "=" + encodeURIComponent(params[k]); }}).join("&");
  location.replace("vyre://aktion/" + path + "?t=" + T + (q ? "&" + q : ""));
}}
document.querySelectorAll(".del").forEach(function (b) {{
  b.addEventListener("click", function () {{ go("verlauf-weg", {{url: b.dataset.url}}); }});
}});
document.querySelectorAll(".actions button").forEach(function (b) {{
  b.addEventListener("click", function () {{
    // Zweimal klicken statt Rückfrage-Dialog.
    if (b.dataset.armed) {{ go("verlauf-loeschen", {{zeit: b.dataset.zeit}}); return; }}
    b.dataset.armed = "1"; b.textContent = "Wirklich löschen?"; b.classList.add("armed");
  }});
}});
</script>
</body></html>"##,
            q = esc(query),
            token_js = crate::engine::js_string(&self.token),
        )
    }

    fn new_tab(&self) -> String {
        let s = self.settings.get();
        let engine = search::engine(s.search_engine.as_deref());
        let token = &self.token;

        let mut tiles = String::new();
        for t in &s.tiles {
            let host = Url::parse(&t.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default();
            let letter = t.title.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
            tiles += &format!(
                r#"<div class="tile"><a href="{url}" title="{url}"><span class="icon" style="background:hsl({hue},42%,36%)">{letter}</span><span class="name">{title}</span></a><button class="x" title="Kachel entfernen" data-url="{url}"></button></div>"#,
                url = esc(&t.url),
                hue = hue(&host),
                letter = esc(&letter),
                title = esc(&t.title),
            );
        }

        let mut engines = String::new();
        for e in ENGINES {
            let chosen = if Some(e.id) == s.search_engine.as_deref() { " chosen" } else { "" };
            engines += &format!(
                r#"<button class="engine{chosen}" data-id="{id}"><b>{name}</b><span>{note}</span></button>"#,
                id = e.id,
                name = esc(e.name),
                note = esc(e.note),
            );
        }
        let first_start = s.search_engine.is_none();

        format!(
            r##"<!doctype html>
<html lang="de"><head><meta charset="utf-8"><title>Neuer Tab</title>
<style>{CSS}</style></head>
<body>
<main>
  <div class="logo">Vyre</div>
  <form id="search" autocomplete="off">
    <input id="q" name="q" placeholder="Mit {engine} suchen oder !Bang eingeben (!yt, !w, !gh …)">
  </form>
  <div class="engine-line">Suchmaschine: {engine} · <a href="#" id="change">ändern</a> · <a href="vyre://verlauf">Verlauf</a></div>
  <div class="tiles">{tiles}
    <div class="tile add"><a href="#" id="add"><span class="icon plus">+</span><span class="name">Hinzufügen</span></a></div>
  </div>
  <form id="addform" class="card hidden" autocomplete="off">
    <input id="addurl" placeholder="Adresse, z. B. servo.org">
    <input id="addtitle" placeholder="Name (optional)">
    <div class="row"><button type="button" id="addcancel">Abbrechen</button><button type="submit" class="primary">Kachel anlegen</button></div>
  </form>
</main>
<div id="chooser" class="overlay{chooser_hidden}">
  <div class="card wide">
    <h2>{chooser_title}</h2>
    <p>Gilt für die Adressleiste und dieses Suchfeld. !Bangs funktionieren mit jeder Suchmaschine.</p>
    <div class="engines">{engines}</div>
  </div>
</div>
<script>
var T = {token_js};
function go(path, params) {{
  var q = Object.keys(params).map(function (k) {{ return k + "=" + encodeURIComponent(params[k]); }}).join("&");
  location.replace("vyre://aktion/" + path + "?t=" + T + (q ? "&" + q : ""));
}}
document.getElementById("search").addEventListener("submit", function (e) {{
  e.preventDefault();
  var q = document.getElementById("q").value.trim();
  if (q) location.href = "vyre://suche?q=" + encodeURIComponent(q);
}});
document.querySelectorAll(".x").forEach(function (b) {{
  b.addEventListener("click", function (e) {{ e.preventDefault(); go("kachel-weg", {{url: b.dataset.url}}); }});
}});
document.querySelectorAll(".engine").forEach(function (b) {{
  b.addEventListener("click", function () {{ go("suchmaschine", {{id: b.dataset.id}}); }});
}});
document.getElementById("change").addEventListener("click", function (e) {{
  e.preventDefault(); document.getElementById("chooser").classList.remove("hidden");
}});
document.getElementById("chooser").addEventListener("click", function (e) {{
  if (e.target.id === "chooser" && !{first_start}) e.target.classList.add("hidden");
}});
var form = document.getElementById("addform");
document.getElementById("add").addEventListener("click", function (e) {{
  e.preventDefault(); form.classList.remove("hidden"); document.getElementById("addurl").focus();
}});
document.getElementById("addcancel").addEventListener("click", function () {{ form.classList.add("hidden"); }});
form.addEventListener("submit", function (e) {{
  e.preventDefault();
  var url = document.getElementById("addurl").value.trim();
  if (url) go("kachel-neu", {{url: url, titel: document.getElementById("addtitle").value}});
}});
</script>
</body></html>"##,
            engine = esc(engine.name),
            chooser_hidden = if first_start { "" } else { " hidden" },
            chooser_title = if first_start { "Willkommen bei Vyre! Welche Suchmaschine möchtest du nutzen?" } else { "Suchmaschine wählen" },
            token_js = crate::engine::js_string(token),
        )
    }
}

/// War das Ergebnis von `resolve_input` eine Suche statt einer Adresse?
fn is_search(url: &Url, input: &str) -> bool {
    search::search_url(input, None).is_some_and(|s| &s == url)
}

impl ProtocolHandler for VyreProtocol {
    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _done_chan: &mut DoneChannel,
        _context: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        let url = request.current_url();
        let html = self.page(url.as_url());
        let mut response = Response::new(url, ResourceFetchTiming::new(request.timing_type()));
        *response.body.lock() = ResponseBody::Done(html.into_bytes());
        response
            .headers
            .insert(http::header::CONTENT_TYPE, HeaderValue::from_static("text/html; charset=utf-8"));
        response.status = HttpStatus::default();
        Box::pin(std::future::ready(response))
    }

    fn is_secure(&self) -> bool {
        true
    }
}

/// Seite, die sofort weiterleitet, ohne einen Eintrag im Verlauf zu hinterlassen.
fn redirect(target: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>…</title><style>html{{background:#15151c}}</style>\
         <script>location.replace({});</script>",
        crate::engine::js_string(target)
    )
}

fn message(text: &str) -> String {
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>Vyre</title><style>{CSS}</style>\
         <main><div class=\"logo\">Vyre</div><p>{}</p><p><a href=\"{NEW_TAB_URL}\">Zur Neuer-Tab-Seite</a></p></main>",
        esc(text)
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

/// Farbton für das Kachel-Symbol, fest pro Domain.
fn hue(host: &str) -> u32 {
    let h = host.trim_start_matches("www.").bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    h % 360
}

const CSS: &str = r#"
:root { color-scheme: dark; }
* { box-sizing: border-box; }
html, body { margin: 0; height: 100%; background: #15151c; color: #e8e8f0;
  font-family: "Segoe UI Variable Text", "Segoe UI", sans-serif; }
main { max-width: 760px; margin: 0 auto; padding: 14vh 24px 40px; display: flex; flex-direction: column; align-items: center; }
.logo { font-size: 52px; font-weight: 600; letter-spacing: 1px; color: #9d8cff; margin-bottom: 28px; }
#search { width: 100%; }
input { width: 100%; padding: 15px 20px; font-size: 17px; color: #e8e8f0; background: #21212b;
  border: 1.5px solid #2d2d3a; border-radius: 14px; outline: none; font-family: inherit; }
input:focus { border-color: #9d8cff; background: #2d2d3a; }
.engine-line { margin-top: 10px; font-size: 13px; color: #8a8a9c; }
a { color: #9d8cff; text-decoration: none; }
.tiles { margin-top: 44px; display: flex; flex-wrap: wrap; justify-content: center; gap: 14px; }
.tile { position: relative; width: 104px; }
.tile a { display: flex; flex-direction: column; align-items: center; padding: 14px 6px 10px;
  border-radius: 14px; color: #e8e8f0; transition: background 0.12s; }
.tile a:hover { background: #21212b; }
.icon { width: 48px; height: 48px; border-radius: 14px; display: flex; align-items: center; justify-content: center;
  font-size: 22px; font-weight: 600; color: #fff; }
.icon.plus { background: #21212b; color: #8a8a9c; border: 1.5px dashed #3a3a4a; }
.name { margin-top: 9px; font-size: 13px; max-width: 96px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.x { position: absolute; top: 4px; right: 4px; width: 22px; height: 22px; border: none; border-radius: 11px;
  background: #2d2d3a; color: #8a8a9c; font-size: 14px; line-height: 20px; cursor: pointer; opacity: 0; transition: opacity 0.12s; }
.tile:hover .x { opacity: 1; }
.x:hover { background: #c42b1c; color: #fff; }
.card { margin-top: 24px; width: 100%; max-width: 420px; padding: 18px; background: #21212b; border-radius: 16px;
  display: flex; flex-direction: column; gap: 10px; }
.card input { background: #15151c; padding: 11px 14px; font-size: 15px; }
.row { display: flex; justify-content: flex-end; gap: 8px; }
button { font-family: inherit; }
.row button { padding: 9px 16px; border-radius: 10px; border: none; background: #2d2d3a; color: #e8e8f0; font-size: 14px; cursor: pointer; }
.row button.primary { background: #9d8cff; color: #15151c; font-weight: 600; }
.hidden { display: none !important; }
.overlay { position: fixed; left: 0; top: 0; right: 0; bottom: 0; background: rgba(10, 10, 14, 0.82);
  display: flex; align-items: center; justify-content: center; padding: 24px; }
.card.wide { max-width: 560px; margin: 0; padding: 26px; }
h2 { margin: 0; font-size: 21px; font-weight: 600; }
.card p { margin: 0 0 8px; color: #8a8a9c; font-size: 14px; }
.engines { display: flex; flex-direction: column; gap: 8px; }
.engine { display: flex; flex-direction: column; align-items: flex-start; gap: 3px; padding: 12px 16px; text-align: left;
  background: #15151c; color: #e8e8f0; border: 1.5px solid #2d2d3a; border-radius: 12px; cursor: pointer; font-size: 15px; }
.engine span { color: #8a8a9c; font-size: 13px; }
.engine:hover { border-color: #9d8cff; }
.engine.chosen { border-color: #9d8cff; background: #343248; }
/* Kreuz aus zwei gedrehten Strichen: sitzt unabhängig von der Schrift genau in der Mitte. */
.x, .del { padding: 0; font-size: 0; }
.x::before, .x::after, .del::before, .del::after { content: ""; position: absolute; left: 50%; top: 50%;
  width: 10px; height: 1.5px; border-radius: 1px; background: currentColor; }
.x::before, .del::before { transform: translate(-50%, -50%) rotate(45deg); }
.x::after, .del::after { transform: translate(-50%, -50%) rotate(-45deg); }
"#;

const HISTORY_CSS: &str = r#"
.page { max-width: 820px; margin: 0 auto; padding: 48px 24px; }
h1 { font-size: 30px; font-weight: 600; margin: 0 0 20px; }
h3 { margin: 28px 0 8px; font-size: 13px; font-weight: 600; color: #8a8a9c; text-transform: uppercase; letter-spacing: 0.5px; }
.actions { display: flex; align-items: center; gap: 8px; margin: 14px 0 8px; color: #8a8a9c; font-size: 14px; flex-wrap: wrap; }
.actions button { padding: 7px 13px; border-radius: 9px; border: 1.5px solid #2d2d3a; background: #21212b; color: #e8e8f0; font-size: 13px; cursor: pointer; }
.actions button:hover { border-color: #9d8cff; }
.actions button.danger:hover, .actions button.armed { border-color: #c42b1c; background: #3a1f1f; }
.entry { display: flex; align-items: center; gap: 12px; padding: 6px 10px; border-radius: 10px; }
.entry:hover { background: #21212b; }
.entry a { flex: 1; min-width: 0; display: flex; flex-direction: column; color: #e8e8f0; }
.entry .t { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 14px; }
.entry .u { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: 12px; color: #8a8a9c; }
.entry .when { font-size: 12px; color: #8a8a9c; white-space: nowrap; }
.del { position: relative; flex-shrink: 0; padding: 0; width: 26px; height: 26px; border: none; border-radius: 13px; background: transparent; color: #8a8a9c; font-size: 16px; cursor: pointer; opacity: 0; }
.entry:hover .del { opacity: 1; }
.del:hover { background: #c42b1c; color: #fff; }
.empty { color: #8a8a9c; margin-top: 30px; }

"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn proto() -> VyreProtocol {
        VyreProtocol { settings: settings::Shared::for_tests(), history: Arc::new(Store::in_memory()), token: "geheim".into() }
    }

    #[test]
    fn aktionen_brauchen_schluessel() {
        let p = proto();
        let before = p.settings.get().tiles.len();
        let html = p.page(&Url::parse("vyre://aktion/kachel-weg?url=https://de.wikipedia.org/").unwrap());
        assert!(html.contains("nur von der Neuer-Tab-Seite"));
        assert_eq!(p.settings.get().tiles.len(), before);
        p.page(&Url::parse("vyre://aktion/kachel-weg?t=geheim&url=https%3A%2F%2Fde.wikipedia.org%2F").unwrap());
        assert_eq!(p.settings.get().tiles.len(), before - 1);
    }

    #[test]
    fn kachel_und_suchmaschine() {
        let p = proto();
        p.page(&Url::parse("vyre://aktion/kachel-neu?t=geheim&url=example.org&titel=").unwrap());
        assert!(p.settings.get().tiles.iter().any(|t| t.url == "https://example.org/" && t.title == "example.org"));
        // Suchbegriffe werden keine Kachel.
        let n = p.settings.get().tiles.len();
        p.page(&Url::parse("vyre://aktion/kachel-neu?t=geheim&url=nur%20text").unwrap());
        assert_eq!(p.settings.get().tiles.len(), n);

        assert!(p.page(&Url::parse("vyre://neuer-tab").unwrap()).contains("Willkommen bei Vyre"));
        p.page(&Url::parse("vyre://aktion/suchmaschine?t=geheim&id=brave").unwrap());
        assert_eq!(p.settings.get().search_engine.as_deref(), Some("brave"));
        let html = p.page(&Url::parse("vyre://neuer-tab").unwrap());
        assert!(html.contains("Mit Brave Search suchen"));
        assert!(!html.contains("Willkommen bei Vyre"));
    }

    #[test]
    fn suche_leitet_weiter() {
        let html = proto().page(&Url::parse("vyre://suche?q=%21yt%20katzen").unwrap());
        assert!(html.contains("youtube.com/results?search_query=katzen"));
    }

    #[test]
    fn verlauf_seite_und_loeschen() {
        let p = proto();
        p.history.record_visit("https://de.wikipedia.org/", Some("Wikipedia <Start>"));
        let html = p.page(&Url::parse("vyre://verlauf").unwrap());
        assert!(html.contains("Wikipedia &lt;Start&gt;"));
        assert!(html.contains("Letzte Stunde"));
        let out = p.page(&Url::parse("vyre://aktion/verlauf-loeschen?t=geheim&zeit=alles").unwrap());
        assert!(out.contains("vyre://verlauf"));
        assert!(p.page(&Url::parse("vyre://verlauf").unwrap()).contains("Der Verlauf ist leer."));
    }

    #[test]
    fn titel_werden_maskiert() {
        assert_eq!(esc("<b>\"a\"</b>"), "&lt;b&gt;&quot;a&quot;&lt;/b&gt;");
    }
}
