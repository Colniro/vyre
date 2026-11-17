//! Downloads: Übersicht, eigener Downloader (für Servo-Tabs) und Anbindung der eingebauten
//! WebView2-Downloads.
//!
//! Dateien landen im Windows-Downloads-Ordner; gleiche Namen bekommen " (1)", " (2)" …
//! Während des Ladens heißt die Datei `<name>.vyre-teil` und wird am Ende umbenannt.
//! Fertige Downloads werden in `%LOCALAPPDATA%\Vyre\downloads.json` gemerkt.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use url::Url;

/// Endungen, die Servo nicht anzeigen kann und die deshalb als Download gelten.
const DOWNLOAD_EXTENSIONS: &[&str] = &[
    "zip", "rar", "7z", "gz", "tgz", "tar", "bz2", "xz", "zst", "exe", "msi", "msix", "appx", "appxbundle", "dmg",
    "pkg", "deb", "rpm", "apk", "iso", "img", "bin", "jar", "pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx",
    "odt", "ods", "odp", "epub", "mp3", "wav", "flac", "ogg", "m4a", "aac", "mp4", "mkv", "avi", "mov", "webm",
    "wmv", "torrent", "psd", "ttf", "otf", "woff2",
];

/// Endungen, bei denen "Öffnen" ein Programm starten würde (mit Nachfrage).
const PROGRAM_EXTENSIONS: &[&str] = &[
    "exe", "msi", "msix", "appx", "appxbundle", "bat", "cmd", "com", "ps1", "vbs", "js", "jar", "scr", "lnk", "reg",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Running,
    Paused,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Vyres eigener Downloader (Servo-Tabs, Strg+S).
    Own,
    /// Eingebauter Download von WebView2.
    Edge,
}

#[derive(Clone, Debug)]
pub struct Entry {
    pub id: u64,
    pub url: String,
    pub name: String,
    pub path: PathBuf,
    pub received: u64,
    pub total: Option<u64>,
    pub state: State,
    pub kind: Kind,
    /// Bytes pro Sekunde (geglättet).
    pub speed: f64,
    /// Aus der Seitenleiste ausgeblendet (× oder auf vyre://downloads angesehen).
    pub dismissed: bool,
}

impl Entry {
    pub fn is_active(&self) -> bool {
        matches!(self.state, State::Running | State::Paused)
    }

    pub fn is_program(&self) -> bool {
        extension(&self.name).is_some_and(|e| PROGRAM_EXTENSIONS.contains(&e.as_str()))
    }
}

struct Job {
    id: u64,
    /// 0 = läuft, 1 = pausieren, 2 = abbrechen
    flag: Arc<AtomicU8>,
    headers: Vec<(String, String)>,
}

#[derive(Default)]
struct Inner {
    entries: Vec<Entry>,
    jobs: Vec<Job>,
    next_id: u64,
}

/// Alle Downloads; teilbar zwischen Hauptthread, Download-Threads und vyre://downloads.
#[derive(Clone)]
pub struct Downloads {
    inner: Arc<Mutex<Inner>>,
    dir: PathBuf,
    store: Option<PathBuf>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

pub fn extension(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 12).then(|| ext.to_ascii_lowercase())
}

/// Sieht die Adresse nach einer Datei aus, die Servo nicht anzeigen kann?
pub fn looks_like_download(url: &Url) -> bool {
    if !matches!(url.scheme(), "http" | "https") {
        return false;
    }
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    extension(last).is_some_and(|e| DOWNLOAD_EXTENSIONS.contains(&e.as_str()))
}

/// Der Windows-Downloads-Ordner.
pub fn downloads_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Downloads")).unwrap_or_else(std::env::temp_dir)
}

/// Dateiname aus `Content-Disposition` (`filename*=UTF-8''…` oder `filename="…"`).
pub fn name_from_disposition(header: &str) -> Option<String> {
    let mut plain = None;
    for part in header.split(';').map(str::trim) {
        let Some((key, value)) = part.split_once('=') else { continue };
        let value = value.trim().trim_matches('"');
        match key.trim().to_ascii_lowercase().as_str() {
            "filename*" => {
                let encoded = value.rsplit_once("''").map(|(_, v)| v).unwrap_or(value);
                let decoded = percent_decode(encoded);
                if !decoded.is_empty() {
                    return Some(decoded);
                }
            }
            "filename" if !value.is_empty() => plain = Some(value.to_string()),
            _ => {}
        }
    }
    plain
}

fn percent_decode(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

/// Dateiname aus der Adresse.
pub fn name_from_url(url: &Url) -> String {
    let last = url.path_segments().and_then(|mut s| s.next_back()).unwrap_or("");
    let name = percent_decode(last);
    if name.is_empty() { url.host_str().unwrap_or("download").to_string() } else { name }
}

/// Entfernt Zeichen, die Windows in Dateinamen nicht erlaubt.
pub fn sanitize(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() { "download".into() } else { cleaned.chars().take(180).collect() }
}

/// Ein freier Pfad im Ordner: "name.ext", sonst "name (1).ext" usw.
pub fn unique_path(dir: &Path, name: &str, taken: &[PathBuf]) -> PathBuf {
    let name = sanitize(name);
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.clone(), String::new()),
    };
    for i in 0.. {
        let candidate = if i == 0 { dir.join(&name) } else { dir.join(format!("{stem} ({i}){ext}")) };
        let part = part_path(&candidate);
        if !candidate.exists() && !part.exists() && !taken.contains(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

fn part_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".vyre-teil");
    PathBuf::from(s)
}

impl Downloads {
    pub fn new(dir: PathBuf, store: Option<PathBuf>, wake: impl Fn() + Send + Sync + 'static) -> Self {
        let d = Self { inner: Arc::default(), dir, store, wake: Arc::new(wake) };
        d.load();
        d
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn list(&self) -> Vec<Entry> {
        self.lock().entries.clone()
    }

    /// Einträge für die Seitenleiste: laufende und noch nicht weggeklickte, die neuesten zuerst.
    pub fn sidebar(&self) -> Vec<Entry> {
        self.lock().entries.iter().rev().filter(|e| !e.dismissed).take(3).cloned().collect()
    }

    pub fn get(&self, id: u64) -> Option<Entry> {
        self.lock().entries.iter().find(|e| e.id == id).cloned()
    }

    fn update(&self, id: u64, f: impl FnOnce(&mut Entry)) {
        if let Some(e) = self.lock().entries.iter_mut().find(|e| e.id == id) {
            f(e);
        }
    }

    pub fn dismiss(&self, id: u64) {
        self.update(id, |e| e.dismissed = true);
        (self.wake)();
    }

    /// Alle fertigen aus der Seitenleiste nehmen (beim Öffnen von vyre://downloads).
    pub fn dismiss_finished(&self) {
        for e in self.lock().entries.iter_mut().filter(|e| !e.is_active()) {
            e.dismissed = true;
        }
    }

    pub fn remove(&self, id: u64) {
        let mut inner = self.lock();
        inner.entries.retain(|e| e.id != id || e.is_active());
        drop(inner);
        self.save();
        (self.wake)();
    }

    fn new_entry(&self, url: &str, name: &str, kind: Kind) -> Entry {
        let mut inner = self.lock();
        inner.next_id += 1;
        let taken: Vec<PathBuf> = inner.entries.iter().filter(|e| e.is_active()).map(|e| e.path.clone()).collect();
        let path = unique_path(&self.dir, name, &taken);
        let entry = Entry {
            id: inner.next_id,
            url: url.to_string(),
            name: path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| name.to_string()),
            path,
            received: 0,
            total: None,
            state: State::Running,
            kind,
            speed: 0.0,
            dismissed: false,
        };
        inner.entries.push(entry.clone());
        entry
    }

    /// Meldet einen WebView2-Download an und liefert den Pfad, unter dem er speichern soll.
    pub fn add_edge(&self, url: &str, suggested_name: &str) -> Entry {
        let e = self.new_entry(url, suggested_name, Kind::Edge);
        (self.wake)();
        e
    }

    /// Fortschritt eines WebView2-Downloads.
    pub fn update_edge(&self, id: u64, received: u64, total: Option<u64>, state: State) {
        let done = state != State::Running && state != State::Paused;
        self.update(id, |e| {
            e.received = received;
            e.total = total.or(e.total);
            e.state = state;
        });
        if done {
            self.save();
        }
        (self.wake)();
    }

    /// Startet einen eigenen Download (Servo-Tabs). `headers`: Cookie, Referer …
    pub fn start(&self, url: Url, name_hint: Option<String>, headers: Vec<(String, String)>) -> u64 {
        if url.scheme() == "data" {
            return self.save_data_url(&url, name_hint);
        }
        let name = name_hint.filter(|n| !n.trim().is_empty()).unwrap_or_else(|| name_from_url(&url));
        let entry = self.new_entry(url.as_str(), &name, Kind::Own);
        let flag = Arc::new(AtomicU8::new(0));
        self.lock().jobs.push(Job { id: entry.id, flag: flag.clone(), headers: headers.clone() });
        self.spawn(entry.id, url, headers, flag, 0, true);
        (self.wake)();
        entry.id
    }

    /// `data:`-Adressen (z. B. von <a download>) direkt speichern.
    fn save_data_url(&self, url: &Url, name_hint: Option<String>) -> u64 {
        let raw = url.as_str().strip_prefix("data:").unwrap_or("");
        let (meta, data) = raw.split_once(',').unwrap_or(("", ""));
        let bytes = if meta.ends_with(";base64") {
            use base64::Engine as _;
            base64::engine::general_purpose::STANDARD.decode(percent_decode(data)).unwrap_or_default()
        } else {
            percent_decode(data).into_bytes()
        };
        let entry = self.new_entry("data:…", &name_hint.unwrap_or_else(|| "download".into()), Kind::Own);
        let state = match std::fs::write(&entry.path, &bytes) {
            Ok(()) => State::Done,
            Err(e) => State::Failed(e.to_string()),
        };
        self.update(entry.id, |e| {
            e.received = bytes.len() as u64;
            e.total = Some(bytes.len() as u64);
            e.state = state;
        });
        self.save();
        (self.wake)();
        entry.id
    }

    fn spawn(&self, id: u64, url: Url, headers: Vec<(String, String)>, flag: Arc<AtomicU8>, offset: u64, first: bool) {
        let me = self.clone();
        let _ = std::thread::Builder::new().name(format!("vyre-download-{id}")).spawn(move || {
            let result = me.run(id, &url, &headers, &flag, offset, first);
            let state = match (result, flag.load(Ordering::Relaxed)) {
                (_, 2) => State::Cancelled,
                (_, 1) => State::Paused,
                (Ok(()), _) => State::Done,
                (Err(e), _) => State::Failed(e),
            };
            if let Some(entry) = me.get(id) {
                let part = part_path(&entry.path);
                match state {
                    State::Done => {
                        if let Err(e) = std::fs::rename(&part, &entry.path) {
                            me.update(id, |x| x.state = State::Failed(e.to_string()));
                            me.save();
                            (me.wake)();
                            return;
                        }
                    }
                    State::Cancelled | State::Failed(_) => {
                        let _ = std::fs::remove_file(&part);
                    }
                    _ => {}
                }
            }
            log::info!("Download {id}: {state:?}");
            let finished = !matches!(state, State::Paused);
            me.update(id, |x| x.state = state);
            if finished {
                me.lock().jobs.retain(|j| j.id != id);
                me.save();
            }
            (me.wake)();
        });
    }

    fn run(&self, id: u64, url: &Url, headers: &[(String, String)], flag: &AtomicU8, offset: u64, first: bool) -> Result<(), String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(30)))
            .user_agent(crate::privacy::USER_AGENT)
            .build()
            .into();
        let mut req = agent.get(url.as_str());
        for (k, v) in headers {
            req = req.header(k.as_str(), v.as_str());
        }
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
        }
        let resp = req.call().map_err(|e| e.to_string())?;
        let resumed = offset > 0 && resp.status().as_u16() == 206;
        let start = if resumed { offset } else { 0 };
        let len = resp.headers().get("content-length").and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
        // Beim ersten Start: besseren Namen aus Content-Disposition bzw. ".html" für Seiten.
        if first {
            let disposition = resp.headers().get("content-disposition").and_then(|v| v.to_str().ok()).and_then(name_from_disposition);
            let is_html = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).is_some_and(|t| t.starts_with("text/html"));
            let current = self.get(id).map(|e| e.name).unwrap_or_default();
            let better = match disposition {
                Some(n) => Some(n),
                None if is_html && extension(&current).is_none() => Some(format!("{current}.html")),
                None => None,
            };
            if let Some(name) = better.filter(|n| *n != current) {
                let mut inner = self.lock();
                let taken: Vec<PathBuf> = inner.entries.iter().filter(|e| e.is_active() && e.id != id).map(|e| e.path.clone()).collect();
                let path = unique_path(&self.dir, &name, &taken);
                if let Some(e) = inner.entries.iter_mut().find(|e| e.id == id) {
                    e.name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(name);
                    e.path = path;
                }
            }
        }
        let path = self.get(id).map(|e| e.path).ok_or("Download verschwunden")?;
        let part = part_path(&path);
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(resumed)
            .truncate(!resumed)
            .open(&part)
            .map_err(|e| e.to_string())?;
        self.update(id, |e| {
            e.received = start;
            e.total = len.map(|l| l + start);
            e.state = State::Running;
        });
        let mut reader = resp.into_body().into_with_config().limit(u64::MAX).reader();
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = start;
        let (mut last_report, mut last_bytes) = (Instant::now(), start);
        loop {
            if flag.load(Ordering::Relaxed) != 0 {
                return Ok(());
            }
            let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            received += n as u64;
            let dt = last_report.elapsed();
            if dt >= Duration::from_millis(250) {
                let speed = (received - last_bytes) as f64 / dt.as_secs_f64();
                self.update(id, |e| {
                    e.received = received;
                    e.speed = if e.speed == 0.0 { speed } else { e.speed * 0.6 + speed * 0.4 };
                });
                (last_report, last_bytes) = (Instant::now(), received);
                (self.wake)();
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        self.update(id, |e| {
            e.received = received;
            e.total = Some(received);
        });
        Ok(())
    }

    /// Pausiert einen eigenen Download. `false` bei WebView2-Downloads (die steuert die Engine).
    pub fn pause(&self, id: u64) -> bool {
        self.signal(id, 1)
    }

    pub fn cancel(&self, id: u64) -> bool {
        // Ein pausierter eigener Download hat keinen Thread mehr: Teildatei direkt löschen.
        if let Some(e) = self.get(id).filter(|e| e.kind == Kind::Own && e.state == State::Paused) {
            let _ = std::fs::remove_file(part_path(&e.path));
            self.update(id, |x| x.state = State::Cancelled);
            self.lock().jobs.retain(|j| j.id != id);
            self.save();
            (self.wake)();
            return true;
        }
        self.signal(id, 2)
    }

    fn signal(&self, id: u64, value: u8) -> bool {
        let inner = self.lock();
        match inner.jobs.iter().find(|j| j.id == id) {
            Some(job) => {
                job.flag.store(value, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Setzt einen pausierten eigenen Download fort (per HTTP-Range, sonst von vorn).
    pub fn resume(&self, id: u64) -> bool {
        let Some(entry) = self.get(id).filter(|e| e.kind == Kind::Own && e.state == State::Paused) else { return false };
        let Ok(url) = Url::parse(&entry.url) else { return false };
        let (flag, headers) = {
            let inner = self.lock();
            let Some(job) = inner.jobs.iter().find(|j| j.id == id) else { return false };
            job.flag.store(0, Ordering::Relaxed);
            (job.flag.clone(), job.headers.clone())
        };
        let offset = part_path(&entry.path).metadata().map(|m| m.len()).unwrap_or(0);
        self.update(id, |e| e.state = State::Running);
        self.spawn(id, url, headers, flag, offset, false);
        (self.wake)();
        true
    }

    /// Fertige Downloads merken (eine Zeile JSON pro Eintrag).
    fn save(&self) {
        let Some(store) = &self.store else { return };
        let entries = self.list();
        let lines: Vec<String> = entries
            .iter()
            .filter(|e| e.state == State::Done)
            .rev()
            .take(200)
            .map(|e| {
                serde_json::json!({ "url": e.url, "name": e.name, "path": e.path.to_string_lossy(), "size": e.received })
                    .to_string()
            })
            .collect();
        let _ = std::fs::write(store, lines.join("\n"));
    }

    fn load(&self) {
        let Some(text) = self.store.as_ref().and_then(|p| std::fs::read_to_string(p).ok()) else { return };
        let mut inner = self.lock();
        for line in text.lines().rev() {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
            let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
            inner.next_id += 1;
            let size = v.get("size").and_then(|s| s.as_u64()).unwrap_or(0);
            let entry = Entry {
                id: inner.next_id,
                url: get("url"),
                name: get("name"),
                path: PathBuf::from(get("path")),
                received: size,
                total: Some(size),
                state: State::Done,
                kind: Kind::Own,
                speed: 0.0,
                dismissed: true,
            };
            inner.entries.push(entry);
        }
    }
}

/// Öffnet eine fertige Datei mit dem zugehörigen Programm.
pub fn open_file(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(path).spawn();
}

/// Zeigt die Datei im Explorer an.
pub fn show_in_folder(path: &Path) {
    let _ = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
}

/// "12,3 MB" usw.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[u]).replace('.', ",") }
}

/// Kurzer Zustandstext, z. B. "12,3 MB von 40,0 MB · 2,1 MB/s".
pub fn status_text(e: &Entry) -> String {
    match &e.state {
        State::Running => {
            let mut t = match e.total {
                Some(total) => format!("{} von {}", size(e.received), size(total)),
                None => size(e.received),
            };
            if e.speed > 0.0 {
                t += &format!(" · {}/s", size(e.speed as u64));
            }
            t
        }
        State::Paused => format!("Pausiert · {}", size(e.received)),
        State::Done => format!("Fertig · {}", size(e.received)),
        State::Failed(err) => format!("Fehlgeschlagen: {err}"),
        State::Cancelled => "Abgebrochen".into(),
    }
}

/// Fortschritt 0..1, wenn die Größe bekannt ist.
pub fn progress(e: &Entry) -> Option<f32> {
    match e.state {
        State::Done => Some(1.0),
        _ => e.total.filter(|t| *t > 0).map(|t| (e.received as f64 / t as f64).min(1.0) as f32),
    }
}

/// Fängt in Servo-Seiten Klicks auf `<a download>` ab und meldet sie (WebView2 kann das selbst).
pub const PAGE_SCRIPT: &str = r#"(function () {
  if (window.__vyreDl || (window.chrome && window.chrome.webview)) return; window.__vyreDl = true;
  document.addEventListener('click', function (ev) {
    var a = ev.target && ev.target.closest && ev.target.closest('a[download]');
    if (!a || !a.href) return;
    if (!/^(https?|data):/i.test(a.href)) return;
    ev.preventDefault();
    console.log('__vyre_dl:' + JSON.stringify({ url: a.href, name: a.getAttribute('download') || '' }));
  }, true);
})();"#;

pub const MESSAGE_PREFIX: &str = "__vyre_dl:";

/// Liest eine Download-Nachricht aus der Seite: Adresse und gewünschter Name.
pub fn parse_message(message: &str) -> Option<(String, Option<String>)> {
    let v: serde_json::Value = serde_json::from_str(message.strip_prefix(MESSAGE_PREFIX)?).ok()?;
    let url = v.get("url")?.as_str()?.to_string();
    let name = v.get("name").and_then(|n| n.as_str()).map(str::to_string).filter(|n| !n.trim().is_empty());
    Some((url, name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erkennt_dateien() {
        assert!(looks_like_download(&Url::parse("https://x.de/setup.EXE").unwrap()));
        assert!(looks_like_download(&Url::parse("https://x.de/a/b/doku.pdf?x=1").unwrap()));
        assert!(!looks_like_download(&Url::parse("https://x.de/index.html").unwrap()));
        assert!(!looks_like_download(&Url::parse("https://x.de/").unwrap()));
        assert!(!looks_like_download(&Url::parse("https://x.de/v1.2/notes").unwrap()));
    }

    #[test]
    fn dateinamen() {
        assert_eq!(name_from_disposition("attachment; filename=\"bericht.pdf\"").as_deref(), Some("bericht.pdf"));
        assert_eq!(name_from_disposition("attachment; filename=a.txt; filename*=UTF-8''%C3%BCbung%201.txt").as_deref(), Some("übung 1.txt"));
        assert_eq!(name_from_url(&Url::parse("https://x.de/dl/Mein%20Bild.png").unwrap()), "Mein Bild.png");
        assert_eq!(sanitize("a:b/c?.txt"), "a_b_c_.txt");
        assert_eq!(size(1536), "1,5 KB");
        assert_eq!(parse_message(r#"__vyre_dl:{"url":"https://x.de/a.zip","name":""}"#), Some(("https://x.de/a.zip".into(), None)));
    }

    #[test]
    fn freie_namen() {
        let dir = std::env::temp_dir().join(format!("vyre-dl-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.zip"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a.zip", &[]), dir.join("a (1).zip"));
        assert_eq!(unique_path(&dir, "a.zip", &[dir.join("a (1).zip")]), dir.join("a (2).zip"));
        assert_eq!(unique_path(&dir, "neu", &[]), dir.join("neu"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Echter Download aus dem Netz (nur von Hand: `cargo test -- --ignored echter_download`).
    #[test]
    #[ignore]
    fn echter_download() {
        let dir = std::env::temp_dir().join(format!("vyre-dl-net-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = Downloads::new(dir.clone(), None, || {});
        let id = d.start(Url::parse("https://www.w3.org/WAI/ER/tests/xhtml/testfiles/resources/pdf/dummy.pdf").unwrap(), None, vec![]);
        let start = Instant::now();
        while d.get(id).unwrap().is_active() && start.elapsed() < Duration::from_secs(30) {
            std::thread::sleep(Duration::from_millis(100));
        }
        let e = d.get(id).unwrap();
        assert_eq!(e.state, State::Done, "{e:?}");
        let bytes = std::fs::read(&e.path).unwrap();
        assert!(bytes.starts_with(b"%PDF"), "keine PDF");
        assert!(!part_path(&e.path).exists());
        println!("{} ({} Bytes)", e.name, bytes.len());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn data_adresse_speichern() {
        let dir = std::env::temp_dir().join(format!("vyre-dl-data-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = Downloads::new(dir.clone(), None, || {});
        let id = d.start(Url::parse("data:text/plain;base64,SGFsbG8=").unwrap(), Some("gruss.txt".into()), vec![]);
        let e = d.get(id).unwrap();
        assert_eq!(e.state, State::Done);
        assert_eq!(std::fs::read_to_string(&e.path).unwrap(), "Hallo");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
