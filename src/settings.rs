//! Einstellungen, die einen Neustart überleben: `%LOCALAPPDATA%\Vyre\einstellungen.txt`.
//!
//! Einfaches `schlüssel = wert`-Format, damit man die Datei auch von Hand bearbeiten kann.
//! Die Einstellungen werden zwischen Hauptthread und Servos Netzwerk-Threads geteilt
//! (die `vyre://`-Seiten ändern Kacheln und Suchmaschine), daher [`Shared`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::ui::SidebarMode;

/// Eine Kachel auf der Neuer-Tab-Seite.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tile {
    pub title: String,
    pub url: String,
}

impl Tile {
    fn new(title: &str, url: &str) -> Self {
        Self { title: title.into(), url: url.into() }
    }
}

pub struct Settings {
    pub sidebar: SidebarMode,
    /// Seiten, auf denen der Werbeblocker ausgeschaltet ist.
    pub adblock_off: Vec<String>,
    /// Gewählte Suchmaschine (`None`: noch nicht gewählt, dann fragt die Neuer-Tab-Seite).
    pub search_engine: Option<String>,
    pub tiles: Vec<Tile>,
    /// Seiten ohne HTTPS, die man trotzdem öffnen wollte.
    pub http_allowed: Vec<String>,
}

fn default_tiles() -> Vec<Tile> {
    vec![
        Tile::new("Wikipedia", "https://de.wikipedia.org/"),
        Tile::new("GitHub", "https://github.com/"),
        Tile::new("YouTube", "https://www.youtube.com/"),
        Tile::new("Reddit", "https://www.reddit.com/"),
        Tile::new("OpenStreetMap", "https://www.openstreetmap.org/"),
        Tile::new("Servo", "https://servo.org/"),
    ]
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            sidebar: SidebarMode::Pinned,
            adblock_off: Vec::new(),
            search_engine: None,
            tiles: default_tiles(),
            http_allowed: Vec::new(),
        }
    }
}

fn path() -> PathBuf {
    crate::engine::profile_dir()
        .parent()
        .map(|p| p.join("einstellungen.txt"))
        .unwrap_or_else(|| PathBuf::from("einstellungen.txt"))
}

impl Settings {
    pub fn load() -> Self {
        let text = std::fs::read_to_string(path()).unwrap_or_default();
        Self::parse(&text)
    }

    fn parse(text: &str) -> Self {
        let mut s = Self::default();
        let mut tiles = Vec::new();
        let mut tiles_set = false;
        for line in text.lines() {
            if line.trim_start().starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            let value = value.trim();
            match key.trim() {
                "seitenleiste" => {
                    if let Some(mode) = SidebarMode::from_name(value) {
                        s.sidebar = mode;
                    }
                }
                "werbeblocker_aus" => {
                    s.adblock_off = value
                        .split(',')
                        .map(|h| h.trim().to_string())
                        .filter(|h| !h.is_empty())
                        .collect();
                }
                "suchmaschine" if !value.is_empty() => s.search_engine = Some(value.to_string()),
                "http_erlaubt" => {
                    s.http_allowed = value
                        .split(',')
                        .map(|h| h.trim().to_string())
                        .filter(|h| !h.is_empty())
                        .collect();
                }
                "kacheln" => tiles_set = true,
                "kachel" => {
                    if let Some((title, url)) = value.split_once('|') {
                        tiles.push(Tile::new(title.trim(), url.trim()));
                    }
                }
                "suchmaschine" => {}
                other => log::warn!("unbekannte Einstellung: {other}"),
            }
        }
        if tiles_set || !tiles.is_empty() {
            s.tiles = tiles;
        }
        s
    }

    fn to_text(&self) -> String {
        let mut t = String::from("# Vyre-Einstellungen\n");
        t += &format!("seitenleiste = {}\n", self.sidebar.name());
        t += &format!("suchmaschine = {}\n", self.search_engine.as_deref().unwrap_or(""));
        t += "# Seiten ohne Werbeblocker, mit Komma getrennt\n";
        t += &format!("werbeblocker_aus = {}\n", self.adblock_off.join(", "));
        t += "# Seiten ohne HTTPS, die trotzdem geöffnet werden dürfen\n";
        t += &format!("http_erlaubt = {}\n", self.http_allowed.join(", "));
        t += "# Kacheln der Neuer-Tab-Seite: kachel = Titel | Adresse\nkacheln = eigene\n";
        for tile in &self.tiles {
            t += &format!("kachel = {} | {}\n", tile.title.replace('|', "/"), tile.url);
        }
        t
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&p, self.to_text()) {
            log::warn!("Einstellungen nicht gespeichert ({}): {e}", p.display());
        }
    }
}

/// Einstellungen, die mehrere Threads lesen und ändern.
#[derive(Clone)]
pub struct Shared {
    inner: Arc<Mutex<Settings>>,
    /// `false` in Tests: Änderungen nicht in die echte Datei schreiben.
    persist: bool,
}

impl Shared {
    pub fn load() -> Self {
        Self { inner: Arc::new(Mutex::new(Settings::load())), persist: true }
    }

    #[cfg(test)]
    pub fn for_tests() -> Self {
        Self { inner: Arc::new(Mutex::new(Settings::default())), persist: false }
    }

    pub fn get(&self) -> MutexGuard<'_, Settings> {
        // Ein Panik-Zustand in einem anderen Thread soll die Einstellungen nicht unbrauchbar machen.
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Ändert die Einstellungen und speichert sie.
    pub fn update(&self, f: impl FnOnce(&mut Settings)) {
        let mut s = self.get();
        f(&mut s);
        if self.persist {
            s.save();
        }
    }

    pub fn save(&self) {
        if self.persist {
            self.get().save();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hin_und_zurueck() {
        let s = Settings {
            sidebar: SidebarMode::Hover,
            adblock_off: vec!["a.de".into(), "b.com".into()],
            search_engine: Some("brave".into()),
            tiles: vec![Tile::new("A | B", "https://a.de/")],
            http_allowed: vec!["alt.de".into()],
        };
        let back = Settings::parse(&s.to_text());
        assert_eq!(back.sidebar, SidebarMode::Hover);
        assert_eq!(back.adblock_off, ["a.de", "b.com"]);
        assert_eq!(back.search_engine.as_deref(), Some("brave"));
        assert_eq!(back.tiles, [Tile::new("A / B", "https://a.de/")]);
        assert_eq!(back.http_allowed, ["alt.de"]);
    }

    #[test]
    fn standardwerte() {
        let s = Settings::parse("");
        assert_eq!(s.sidebar, SidebarMode::Pinned);
        assert!(s.search_engine.is_none());
        assert_eq!(s.tiles, default_tiles());
        assert_eq!(Settings::parse("seitenleiste = quatsch").sidebar, SidebarMode::Pinned);
        // Alle Kacheln entfernt bleibt leer und füllt sich nicht wieder mit Standardkacheln.
        let mut empty = Settings::default();
        empty.tiles.clear();
        assert!(Settings::parse(&empty.to_text()).tiles.is_empty());
    }
}
