//! Einstellungen, die einen Neustart überleben: `%LOCALAPPDATA%\Vyre\einstellungen.txt`.
//!
//! Einfaches `schlüssel = wert`-Format, damit man die Datei auch von Hand bearbeiten kann.

use std::path::PathBuf;

use crate::ui::SidebarMode;

pub struct Settings {
    pub sidebar: SidebarMode,
    /// Seiten, auf denen der Werbeblocker ausgeschaltet ist.
    pub adblock_off: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self { sidebar: SidebarMode::Pinned, adblock_off: Vec::new() }
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
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else { continue };
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
                other => log::warn!("unbekannte Einstellung: {other}"),
            }
        }
        s
    }

    fn to_text(&self) -> String {
        format!(
            "# Vyre-Einstellungen\nseitenleiste = {}\n# Seiten ohne Werbeblocker, mit Komma getrennt\nwerbeblocker_aus = {}\n",
            self.sidebar.name(),
            self.adblock_off.join(", ")
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hin_und_zurueck() {
        let s = Settings { sidebar: SidebarMode::Hover, adblock_off: vec!["a.de".into(), "b.com".into()] };
        let back = Settings::parse(&s.to_text());
        assert_eq!(back.sidebar, SidebarMode::Hover);
        assert_eq!(back.adblock_off, ["a.de", "b.com"]);
        assert_eq!(Settings::parse("").sidebar, SidebarMode::Pinned);
        assert_eq!(Settings::parse("seitenleiste = quatsch").sidebar, SidebarMode::Pinned);
    }
}
