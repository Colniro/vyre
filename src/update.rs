//! Updates wie bei Wyven: Vyre liest ein kleines Manifest (`update.json`, gebaut von
//! `scripts/release.ps1`) aus den GitHub-Releases. Ist dort eine neuere Version, fragt eine Karte
//! in der Seitenleiste nach. Erst nach "Aktualisieren" wird der Installer geladen, seine SHA-256
//! geprüft und still gestartet; er schließt Vyre und startet die neue Version.
//!
//! Automatisch wird höchstens einmal am Tag beim Start geprüft; `vyre update` und der Link auf
//! der Neuer-Tab-Seite prüfen sofort. Die Adresse lässt sich in einstellungen.txt ändern
//! (`updates = <adresse oder ordner>`, `updates = aus` schaltet die Prüfung ab).

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::Value;
use sha2::{Digest, Sha256};

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
pub const OFFICIAL: &str = "https://github.com/Colniro/vyre/releases/latest/download/update.json";
const DAY: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    pub version: String,
    /// Der Installer: absolut oder relativ zum Manifest.
    pub url: String,
    pub sha256: String,
    pub notes: String,
}

#[derive(Debug)]
pub enum Event {
    /// Ergebnis einer Prüfung: neuere Version oder keine (`manual`: Ergebnis auch so melden).
    Checked { found: Option<Manifest>, manual: bool },
    /// Installer geladen und geprüft.
    Ready(PathBuf),
    Failed(String),
}

/// `"0.10.2"` → `[0, 10, 2]` (alles nach `-` oder `+` zählt nicht).
fn parts(v: &str) -> Vec<u64> {
    v.trim().trim_start_matches('v').split(['-', '+']).next().unwrap_or("").split('.').map(|p| p.parse().unwrap_or(0)).collect()
}

pub fn is_newer(candidate: &str, current: &str) -> bool {
    let (a, b) = (parts(candidate), parts(current));
    let n = a.len().max(b.len());
    let get = |v: &Vec<u64>, i: usize| v.get(i).copied().unwrap_or(0);
    (0..n).map(|i| get(&a, i).cmp(&get(&b, i))).find(|o| o.is_ne()) == Some(std::cmp::Ordering::Greater)
}

fn is_web(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

fn read(source: &str) -> Result<Vec<u8>, String> {
    if is_web(source) {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent(format!("Vyre/{CURRENT}"))
            .build()
            .into();
        let resp = agent.get(source).call().map_err(|e| format!("{source}: {e}"))?;
        resp.into_body().with_config().limit(1 << 30).read_to_vec().map_err(|e| format!("{source}: {e}"))
    } else {
        std::fs::read(source).map_err(|e| format!("{source}: {e}"))
    }
}

/// Die Adresse des Installers: absolut oder neben dem Manifest.
pub fn resolve(manifest_source: &str, url: &str) -> String {
    if is_web(url) || Path::new(url).is_absolute() {
        return url.to_string();
    }
    if is_web(manifest_source) {
        let base = manifest_source.rsplit_once('/').map_or(manifest_source, |(b, _)| b);
        format!("{base}/{url}")
    } else {
        Path::new(manifest_source).parent().unwrap_or(Path::new(".")).join(url).display().to_string()
    }
}

fn stamp_file() -> Option<PathBuf> {
    crate::engine::profile_dir().parent().map(|p| p.join("letzte-update-pruefung"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Automatische Prüfung fällig? Höchstens einmal am Tag.
pub fn due() -> bool {
    let last = stamp_file().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|s| s.trim().parse::<u64>().ok());
    last.is_none_or(|t| now().saturating_sub(t) >= DAY)
}

fn remember_check() {
    if let Some(p) = stamp_file() {
        let _ = std::fs::write(p, now().to_string());
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Das Manifest (ein UTF-8-BOM, wie es Windows-Werkzeuge gern schreiben, ist erlaubt).
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let v: Value = serde_json::from_slice(bytes).map_err(|e| format!("update.json: {e}"))?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    Ok(Manifest {
        version: get("version").ok_or("update.json: version fehlt")?,
        url: get("url").ok_or("update.json: url fehlt")?,
        sha256: get("sha256").ok_or("update.json: sha256 fehlt")?.to_lowercase(),
        notes: get("notes").unwrap_or_default(),
    })
}

/// Prüft im Hintergrund; das Ergebnis kommt über `done`.
pub fn check(source: String, manual: bool, done: impl FnOnce(Event) + Send + 'static) {
    let _ = std::thread::Builder::new().name("vyre-update-check".into()).spawn(move || {
        let result = read(&source).and_then(|bytes| parse_manifest(&bytes));
        remember_check();
        done(match result {
            Ok(m) if is_newer(&m.version, CURRENT) => Event::Checked { found: Some(m), manual },
            Ok(_) => Event::Checked { found: None, manual },
            Err(e) => Event::Failed(e),
        });
    });
}

/// Lädt den Installer in den Temp-Ordner und prüft seine SHA-256.
pub fn download(source: String, manifest: Manifest, done: impl FnOnce(Event) + Send + 'static) {
    let _ = std::thread::Builder::new().name("vyre-update-download".into()).spawn(move || {
        let result = (|| {
            let url = resolve(&source, &manifest.url);
            let bytes = read(&url)?;
            let hash = sha256_hex(&bytes);
            if hash != manifest.sha256 {
                return Err(format!("Prüfsumme stimmt nicht (erwartet {}, erhalten {hash}) – Update verworfen", manifest.sha256));
            }
            let path = std::env::temp_dir().join(format!("vyre-setup-{}.exe", manifest.version));
            std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
            Ok(path)
        })();
        done(match result {
            Ok(path) => Event::Ready(path),
            Err(e) => Event::Failed(e),
        });
    });
}

/// Startet den Installer still; er schließt Vyre und öffnet die neue Version.
pub fn run_installer(path: &Path) -> Result<(), String> {
    std::process::Command::new(path)
        .args(["/SILENT", "/SP-", "/NOCANCEL", "/CLOSEAPPLICATIONS", "/RESTARTAPPLICATIONS"])
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Installer startet nicht: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versionen_vergleichen() {
        assert!(is_newer("0.14.0", "0.13.0"));
        assert!(is_newer("0.13.10", "0.13.9"));
        assert!(is_newer("v1.0", "0.99.9"));
        assert!(!is_newer("0.13.0", "0.13.0"));
        assert!(!is_newer("0.12.9", "0.13.0"));
        assert!(!is_newer("0.14.0-beta", "0.14.0"));
    }

    #[test]
    fn manifest_und_adressen() {
        let m = parse_manifest(b"\xEF\xBB\xBF{\"version\":\"0.14.0\",\"url\":\"vyre-setup-0.14.0.exe\",\"sha256\":\"ABC\"}").unwrap();
        assert_eq!(m.version, "0.14.0");
        assert_eq!(m.sha256, "abc");
        assert_eq!(resolve(OFFICIAL, &m.url), "https://github.com/Colniro/vyre/releases/latest/download/vyre-setup-0.14.0.exe");
        assert_eq!(resolve(OFFICIAL, "https://x.org/s.exe"), "https://x.org/s.exe");
        assert!(parse_manifest(b"{\"version\":\"1\"}").is_err());
    }

    #[test]
    fn falsche_pruefsumme_wird_abgelehnt() {
        let dir = std::env::temp_dir().join(format!("vyre-upd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("setup.exe"), b"echt").unwrap();
        let manifest = Manifest { version: "9.9.9".into(), url: "setup.exe".into(), sha256: sha256_hex(b"anders"), notes: String::new() };
        let source = dir.join("update.json").display().to_string();
        let (tx, rx) = std::sync::mpsc::channel();
        download(source, manifest, move |e| tx.send(e).unwrap());
        match rx.recv_timeout(Duration::from_secs(10)).unwrap() {
            Event::Failed(e) => assert!(e.contains("Prüfsumme")),
            other => panic!("{other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
