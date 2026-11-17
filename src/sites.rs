//! Welche Seite mit welcher Engine läuft: `%LOCALAPPDATA%\Vyre\engine-seiten.txt`.
//!
//! - **Startliste**: Seiten, die in Servo sicher (noch) nicht laufen, öffnen sofort in WebView2.
//! - **Gelernt**: Seiten, bei denen Vyre ein Scheitern in Servo erkannt hat. Gilt bis zum
//!   nächsten Vyre-Update, dann wird wieder zuerst Servo versucht.
//! - **Von Hand** (Strg+Umschalt+E): bleibt, bis man es wieder umschaltet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Seiten, die Servo zurzeit sicher nicht kann (große Web-Apps, Video-Plattformen).
const START_LIST: &[&str] = &[
    "youtube.com",
    "youtu.be",
    "discord.com",
    "docs.google.com",
    "drive.google.com",
    "mail.google.com",
    "meet.google.com",
    "calendar.google.com",
    "open.spotify.com",
    "web.whatsapp.com",
    "web.telegram.org",
    "teams.microsoft.com",
    "teams.live.com",
    "outlook.live.com",
    "outlook.office.com",
    "office.com",
    "figma.com",
    "notion.so",
    "twitch.tv",
    "canva.com",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Servo,
    Edge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Learned,
    Manual,
}

#[derive(Default)]
pub struct Sites {
    entries: BTreeMap<String, (Choice, Source)>,
    path: Option<PathBuf>,
}

pub fn path() -> PathBuf {
    crate::engine::profile_dir()
        .parent()
        .map(|p| p.join("engine-seiten.txt"))
        .unwrap_or_else(|| PathBuf::from("engine-seiten.txt"))
}

/// Hostname ohne "www.".
pub fn host(url: &url::Url) -> Option<String> {
    url.host_str().map(|h| h.trim_start_matches("www.").to_ascii_lowercase())
}

fn matches_domain(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

impl Sites {
    pub fn load(path: &Path) -> Self {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        let mut s = Self::parse(&text, env!("CARGO_PKG_VERSION"));
        s.path = Some(path.to_path_buf());
        s
    }

    fn parse(text: &str, version: &str) -> Self {
        let mut s = Self::default();
        let mut file_version = "";
        for line in text.lines().map(str::trim) {
            let Some((key, value)) = line.split_once('=') else { continue };
            let (key, value) = (key.trim(), value.trim().to_ascii_lowercase());
            match key {
                "version" => file_version = line.split_once('=').map(|(_, v)| v.trim()).unwrap_or(""),
                // Gelernte Einträge verfallen mit jedem Update: Servo wird ständig besser.
                "gelernt" if file_version == version => {
                    s.entries.insert(value, (Choice::Edge, Source::Learned));
                }
                "edge" => {
                    s.entries.insert(value, (Choice::Edge, Source::Manual));
                }
                "servo" => {
                    s.entries.insert(value, (Choice::Servo, Source::Manual));
                }
                _ => {}
            }
        }
        s
    }

    fn to_text(&self) -> String {
        let mut t = String::from(
            "# Vyre: welche Seite mit welcher Engine läuft\n\
             # edge/servo = von Hand festgelegt (Strg+Umschalt+E), gelernt = automatisch erkannt\n",
        );
        t += &format!("version = {}\n", env!("CARGO_PKG_VERSION"));
        for (host, (choice, source)) in &self.entries {
            let key = match (choice, source) {
                (Choice::Edge, Source::Learned) => "gelernt",
                (Choice::Edge, Source::Manual) => "edge",
                (Choice::Servo, _) => "servo",
            };
            t += &format!("{key} = {host}\n");
        }
        t
    }

    /// Privates Fenster: nichts über besuchte Seiten in die Datei schreiben.
    pub fn read_only(&mut self) {
        self.path = None;
    }

    fn save(&self) {
        if let Some(p) = &self.path {
            if let Err(e) = std::fs::write(p, self.to_text()) {
                log::warn!("engine-seiten.txt nicht gespeichert: {e}");
            }
        }
    }

    /// Welche Engine soll diese Adresse öffnen?
    pub fn choice(&self, url: &url::Url) -> Choice {
        if !matches!(url.scheme(), "http" | "https") {
            return Choice::Servo; // vyre://, about:, file: kann nur Servo
        }
        let Some(h) = host(url) else { return Choice::Servo };
        // Der genaueste Eintrag gewinnt (z. B. "servo" für eine Subdomain einer Edge-Seite).
        let best = self
            .entries
            .iter()
            .filter(|(d, _)| matches_domain(&h, d))
            .max_by_key(|(d, _)| d.len());
        if let Some((_, (choice, _))) = best {
            return *choice;
        }
        if START_LIST.iter().any(|d| matches_domain(&h, d)) {
            return Choice::Edge;
        }
        Choice::Servo
    }

    /// Merkt sich, dass die Seite in Servo gescheitert ist (bis zum nächsten Update).
    pub fn learn_edge(&mut self, url: &url::Url) {
        let Some(h) = host(url) else { return };
        // Eine Festlegung von Hand hat Vorrang.
        if self.entries.get(&h).is_some_and(|(_, s)| *s == Source::Manual) {
            return;
        }
        self.entries.insert(h, (Choice::Edge, Source::Learned));
        self.save();
    }

    /// Legt die Engine für die Seite von Hand fest.
    pub fn set_manual(&mut self, url: &url::Url, choice: Choice) {
        let Some(h) = host(url) else { return };
        self.entries.insert(h, (choice, Source::Manual));
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> url::Url {
        url::Url::parse(s).unwrap()
    }

    #[test]
    fn startliste_und_eigene_seiten() {
        let mut s = Sites::default();
        assert_eq!(s.choice(&u("https://www.youtube.com/watch?v=1")), Choice::Edge);
        assert_eq!(s.choice(&u("https://m.youtube.com/")), Choice::Edge);
        assert_eq!(s.choice(&u("https://notyoutube.com/")), Choice::Servo);
        assert_eq!(s.choice(&u("https://servo.org/")), Choice::Servo);
        assert_eq!(s.choice(&u("vyre://neuer-tab")), Choice::Servo);

        s.learn_edge(&u("https://kaputt.de/x"));
        assert_eq!(s.choice(&u("https://www.kaputt.de/")), Choice::Edge);
        // Von Hand zurück auf Servo, auch für eine Seite der Startliste.
        s.set_manual(&u("https://www.youtube.com/"), Choice::Servo);
        assert_eq!(s.choice(&u("https://youtube.com/")), Choice::Servo);
        // Gelerntes überschreibt keine Festlegung von Hand.
        s.learn_edge(&u("https://youtube.com/"));
        assert_eq!(s.choice(&u("https://youtube.com/")), Choice::Servo);
    }

    #[test]
    fn gelerntes_verfaellt_mit_update() {
        let mut s = Sites::default();
        s.learn_edge(&u("https://kaputt.de/"));
        s.set_manual(&u("https://hand.de/"), Choice::Edge);
        let text = s.to_text();
        let same = Sites::parse(&text, env!("CARGO_PKG_VERSION"));
        assert_eq!(same.choice(&u("https://kaputt.de/")), Choice::Edge);
        let updated = Sites::parse(&text, "99.0.0");
        assert_eq!(updated.choice(&u("https://kaputt.de/")), Choice::Servo);
        assert_eq!(updated.choice(&u("https://hand.de/")), Choice::Edge);
    }
}
