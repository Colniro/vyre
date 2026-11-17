//! Eingebauter Werbe- und Tracker-Blocker auf Basis von Braves adblock-rust.
//!
//! Nutzt dieselben Filterlisten wie uBlock Origin. Die Listen werden im Hintergrund geladen,
//! in `%LOCALAPPDATA%\Vyre\Filter` zwischengespeichert und einmal am Tag aktualisiert. Der
//! fertig gebaute Filter wird als `engine.dat` gespeichert, damit der Start schnell bleibt.
//!
//! Außer diesen Listen-Downloads schickt der Blocker nichts ins Netz.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use adblock::lists::{FilterSet, ParseOptions};
use adblock::request::Request;
use adblock::Engine;

/// Filterlisten wie in uBlock Origin, plus deutsche Liste und Cookie-Banner.
const LISTS: &[(&str, &str)] = &[
    ("easylist", "https://easylist.to/easylist/easylist.txt"),
    ("easyprivacy", "https://easylist.to/easylist/easyprivacy.txt"),
    ("easylist-germany", "https://easylist.to/easylistgermany/easylistgermany.txt"),
    ("ubo-filters", "https://ublockorigin.github.io/uAssets/filters/filters.txt"),
    ("ubo-privacy", "https://ublockorigin.github.io/uAssets/filters/privacy.txt"),
    ("ubo-unbreak", "https://ublockorigin.github.io/uAssets/filters/unbreak.txt"),
    ("ubo-cookies", "https://ublockorigin.github.io/uAssets/filters/annoyances-cookies.txt"),
    ("easylist-cookies", "https://secure.fanboy.co.nz/fanboy-cookiemonster.txt"),
    (
        "peter-lowe",
        "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=adblockplus&showintro=0&mimetype=plaintext",
    ),
];

/// So alt dürfen die Listen werden, bevor sie neu geladen werden.
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub struct Blocker {
    engine: Arc<Mutex<Option<Arc<Engine>>>>,
    /// Seiten (Hostnamen), auf denen der Blocker ausgeschaltet ist.
    allowed: Mutex<HashSet<String>>,
}

impl Blocker {
    /// Startet das Laden der Filter im Hintergrund. Bis sie bereit sind, wird nichts blockiert.
    pub fn start(dir: PathBuf, allowed: HashSet<String>, on_ready: impl Fn() + Send + 'static) -> Arc<Self> {
        let blocker = Arc::new(Self { engine: Arc::default(), allowed: Mutex::new(allowed) });
        let slot = blocker.engine.clone();
        let _ = std::thread::Builder::new().name("vyre-filter".into()).spawn(move || {
            load(&dir, &slot);
            on_ready();
        });
        blocker
    }

    fn engine(&self) -> Option<Arc<Engine>> {
        self.engine.lock().ok()?.clone()
    }

    pub fn is_ready(&self) -> bool {
        self.engine().is_some()
    }

    /// Ist der Blocker für diesen Hostnamen ausgeschaltet?
    pub fn is_allowed(&self, host: &str) -> bool {
        self.allowed.lock().is_ok_and(|a| a.contains(host))
    }

    /// Schaltet den Blocker für einen Hostnamen um. `true`, wenn er danach blockiert.
    pub fn toggle(&self, host: &str) -> bool {
        let Ok(mut a) = self.allowed.lock() else { return true };
        if a.remove(host) {
            true
        } else {
            a.insert(host.to_string());
            false
        }
    }

    pub fn allowed_hosts(&self) -> Vec<String> {
        let mut v: Vec<String> = self.allowed.lock().map(|a| a.iter().cloned().collect()).unwrap_or_default();
        v.sort();
        v
    }

    /// Soll eine Anfrage blockiert werden? `page` ist die Adresse der Seite im Tab.
    pub fn should_block(&self, url: &str, page: &str, kind: &str) -> bool {
        if host_of(page).is_some_and(|h| self.is_allowed(&h)) {
            return false;
        }
        let Some(engine) = self.engine() else { return false };
        match Request::new(url, page, kind, "get") {
            Ok(req) => engine.check_network_request(&req).should_block(),
            Err(_) => false,
        }
    }

    /// Seitenspezifische CSS-Regeln zum Ausblenden. `None`, wenn nichts zu tun ist.
    pub fn cosmetic(&self, page: &str) -> Option<Cosmetic> {
        if host_of(page).is_some_and(|h| self.is_allowed(&h)) {
            return None;
        }
        let engine = self.engine()?;
        let res = engine.url_cosmetic_resources(page);
        Some(Cosmetic {
            css: hide_css(res.hide_selectors.iter()),
            generic: !res.generichide,
        })
    }

    /// Allgemeine Regeln für die Klassen und IDs, die auf der Seite vorkommen.
    pub fn generic_css(&self, page: &str, classes: &[String], ids: &[String]) -> String {
        let Some(engine) = self.engine() else { return String::new() };
        let exceptions = engine.url_cosmetic_resources(page).exceptions;
        let selectors = engine.hidden_class_id_selectors(classes, ids, &exceptions);
        hide_css(selectors.iter())
    }
}

/// Was auf einer Seite ausgeblendet werden soll.
pub struct Cosmetic {
    pub css: String,
    /// Allgemeine Regeln (nach Klassen/IDs) sind auf dieser Seite erlaubt.
    pub generic: bool,
}

pub fn host_of(url: &str) -> Option<String> {
    url::Url::parse(url).ok()?.host_str().map(|h| h.trim_start_matches("www.").to_string())
}

/// Eine Regel pro Selektor: ein ungültiger Selektor macht so nur seine eigene Regel unwirksam.
fn hide_css<'a>(selectors: impl Iterator<Item = &'a String>) -> String {
    let mut css = String::new();
    for s in selectors {
        if s.contains('{') || s.contains('}') {
            continue;
        }
        css.push_str(s);
        css.push_str("{display:none!important}\n");
    }
    css
}

/// Ordnet Servos Anfrageziele (`Destination`) den Typen der Filterlisten zu.
pub fn request_kind(destination: &str) -> &'static str {
    match destination {
        "Document" => "document",
        "Script" | "Worker" | "SharedWorker" | "ServiceWorker" | "AudioWorklet" | "PaintWorklet" => "script",
        "Image" => "image",
        "Style" | "Xslt" => "stylesheet",
        "Font" => "font",
        "Frame" | "IFrame" | "Iframe" => "sub_frame",
        "Audio" | "Video" | "Track" => "media",
        "Object" | "Embed" => "object",
        "Report" => "ping",
        "None" | "Json" => "xmlhttprequest",
        _ => "other",
    }
}

/// Lädt den gespeicherten Filter und lädt die Listen neu, wenn sie zu alt sind.
fn load(dir: &Path, slot: &Mutex<Option<Arc<Engine>>>) {
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("Filterordner {} nicht anlegbar: {e}", dir.display());
    }
    let cache = dir.join("engine.dat");
    let age = std::fs::metadata(&cache)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| SystemTime::now().duration_since(t).ok());

    if let Ok(bytes) = std::fs::read(&cache) {
        let t = Instant::now();
        let mut engine = Engine::default();
        match engine.deserialize(&bytes) {
            Ok(()) => {
                log::info!("Werbeblocker aus dem Zwischenspeicher geladen in {:?}", t.elapsed());
                set(slot, engine);
            }
            Err(e) => log::warn!("engine.dat unbrauchbar ({e:?}), baue neu"),
        }
    }
    let fresh = age.is_some_and(|a| a < MAX_AGE) && slot.lock().is_ok_and(|s| s.is_some());
    if fresh {
        return;
    }

    let t = Instant::now();
    let mut set_ = FilterSet::new(false);
    let mut loaded = 0;
    for (name, url) in LISTS {
        let file = dir.join(format!("{name}.txt"));
        let text = match download(url) {
            Ok(text) => {
                let _ = std::fs::write(&file, &text);
                Some(text)
            }
            Err(e) => {
                log::warn!("Filterliste {name} nicht geladen ({e}), nehme die gespeicherte");
                std::fs::read_to_string(&file).ok()
            }
        };
        if let Some(text) = text {
            set_.add_filter_list(text, ParseOptions::default());
            loaded += 1;
        }
    }
    if loaded == 0 {
        log::warn!("keine Filterliste verfügbar, Werbeblocker bleibt aus");
        return;
    }
    let engine = Engine::new_with_filter_set(set_);
    log::info!("Werbeblocker gebaut aus {loaded} Listen in {:?}", t.elapsed());
    if let Err(e) = std::fs::write(&cache, engine.serialize()) {
        log::warn!("engine.dat nicht gespeichert: {e}");
    }
    set(slot, engine);
}

fn set(slot: &Mutex<Option<Arc<Engine>>>, engine: Engine) {
    if let Ok(mut s) = slot.lock() {
        *s = Some(Arc::new(engine));
    }
}

fn download(url: &str) -> Result<String, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .user_agent("Vyre")
        .build()
        .into();
    let mut resp = agent.get(url).call().map_err(|e| e.to_string())?;
    resp.body_mut()
        .with_config()
        .limit(32 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn engine_with(rules: &str) -> Blocker {
        let mut set = FilterSet::new(false);
        set.add_filter_list(rules.to_string(), ParseOptions::default());
        let b = Blocker { engine: Arc::default(), allowed: Mutex::default() };
        *b.engine.lock().unwrap() = Some(Arc::new(Engine::new_with_filter_set(set)));
        b
    }

    #[test]
    fn blockiert_werbung_aber_nicht_die_seite() {
        let b = engine_with("||ads.example.com^\n##.cookie-banner\nnews.de##.werbung");
        assert!(b.should_block("https://ads.example.com/banner.js", "https://news.de/", "script"));
        assert!(!b.should_block("https://news.de/app.js", "https://news.de/", "script"));
        let c = b.cosmetic("https://news.de/artikel").unwrap();
        assert!(c.css.contains(".werbung{display:none!important}"));
        assert!(c.generic);
        let generic = b.generic_css("https://news.de/", &["cookie-banner".into()], &[]);
        assert!(generic.contains(".cookie-banner"));
    }

    #[test]
    fn ausnahme_pro_seite() {
        let b = engine_with("||ads.example.com^");
        assert!(!b.toggle("news.de"));
        assert!(!b.should_block("https://ads.example.com/x.js", "https://www.news.de/", "script"));
        assert!(b.should_block("https://ads.example.com/x.js", "https://other.de/", "script"));
        assert!(b.toggle("news.de"));
        assert!(b.should_block("https://ads.example.com/x.js", "https://news.de/", "script"));
    }
}
