//! Offene Tabs über einen Neustart retten: `%LOCALAPPDATA%\Vyre\sitzung.txt`.
//!
//! Wird bei jeder Änderung der Tabs geschrieben, damit auch nach einem Absturz nichts verloren
//! geht. Format: eine Adresse pro Zeile, die Zeile des aktiven Tabs beginnt mit `*`.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Session {
    pub urls: Vec<String>,
    pub active: usize,
}

pub fn path() -> PathBuf {
    crate::engine::profile_dir()
        .parent()
        .map(|p| p.join("sitzung.txt"))
        .unwrap_or_else(|| PathBuf::from("sitzung.txt"))
}

impl Session {
    pub fn load(path: &Path) -> Option<Self> {
        Self::parse(&std::fs::read_to_string(path).ok()?)
    }

    fn parse(text: &str) -> Option<Self> {
        let mut s = Session::default();
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
            let (active, url) = match line.strip_prefix('*') {
                Some(rest) => (true, rest.trim()),
                None => (false, line),
            };
            if url::Url::parse(url).is_err() {
                continue;
            }
            if active {
                s.active = s.urls.len();
            }
            s.urls.push(url.to_string());
        }
        (!s.urls.is_empty()).then_some(s)
    }

    fn to_text(&self) -> String {
        let mut t = String::from("# Vyre-Sitzung: offene Tabs, * = aktiver Tab\n");
        for (i, u) in self.urls.iter().enumerate() {
            if i == self.active {
                t.push('*');
            }
            t += u;
            t.push('\n');
        }
        t
    }

    pub fn save(&self, path: &Path) {
        // Erst in eine Hilfsdatei schreiben, dann umbenennen: so ist die Datei nie halb geschrieben.
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, self.to_text()).and_then(|_| std::fs::rename(&tmp, path)).is_err() {
            log::warn!("Sitzung nicht gespeichert: {}", path.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hin_und_zurueck() {
        let s = Session { urls: vec!["https://a.de/".into(), "vyre://neuer-tab".into()], active: 1 };
        assert_eq!(Session::parse(&s.to_text()), Some(s));
        assert_eq!(Session::parse("# leer\n"), None);
        assert_eq!(Session::parse("kaputt\nhttps://b.de/").unwrap().urls, ["https://b.de/"]);
    }
}
