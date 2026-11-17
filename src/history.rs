//! Verlauf und angepinnte Favoriten in `%LOCALAPPDATA%\Vyre\verlauf.sqlite`.
//!
//! Pro Adresse gibt es eine Zeile mit Titel, Anzahl der Besuche und letztem Besuch. Daraus
//! entstehen die Vorschläge in der Adressleiste: oft und kürzlich Besuchtes zuerst.
//! Der Verlauf lässt sich ganz, nach Zeitraum oder pro Eintrag löschen.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, params};

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub url: String,
    pub title: String,
    pub visits: i64,
    /// Letzter Besuch, Sekunden seit 1970.
    pub last: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Favorite {
    pub url: String,
    pub title: String,
}

pub struct Store {
    conn: Mutex<Connection>,
}

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Nur echte Webseiten kommen in den Verlauf.
pub fn is_recordable(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

impl Store {
    pub fn open(path: &Path) -> Self {
        match Connection::open(path) {
            Ok(conn) => Self::init(conn),
            Err(e) => {
                log::warn!("Verlauf {} nicht zu öffnen ({e}), nur im Speicher", path.display());
                Self::in_memory()
            }
        }
    }

    pub fn in_memory() -> Self {
        Self::init(Connection::open_in_memory().expect("SQLite im Speicher"))
    }

    fn init(conn: Connection) -> Self {
        let r = conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS visits (
                 url TEXT PRIMARY KEY,
                 title TEXT NOT NULL DEFAULT '',
                 count INTEGER NOT NULL DEFAULT 0,
                 last INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS visits_last ON visits(last);
             CREATE TABLE IF NOT EXISTS favorites (
                 url TEXT PRIMARY KEY,
                 title TEXT NOT NULL DEFAULT '',
                 pos INTEGER NOT NULL DEFAULT 0
             );",
        );
        if let Err(e) = r {
            log::error!("Verlauf-Datenbank: {e}");
        }
        Self { conn: Mutex::new(conn) }
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn record_visit(&self, url: &str, title: Option<&str>) {
        self.record_visit_at(url, title, now());
    }

    fn record_visit_at(&self, url: &str, title: Option<&str>, at: i64) {
        if !is_recordable(url) {
            return;
        }
        let r = self.db().execute(
            "INSERT INTO visits (url, title, count, last) VALUES (?1, COALESCE(?2, ''), 1, ?3)
             ON CONFLICT(url) DO UPDATE SET count = count + 1, last = ?3,
                 title = CASE WHEN ?2 IS NULL OR ?2 = '' THEN title ELSE ?2 END",
            params![url, title, at],
        );
        if let Err(e) = r {
            log::warn!("Besuch nicht gespeichert: {e}");
        }
    }

    pub fn set_title(&self, url: &str, title: &str) {
        if title.trim().is_empty() {
            return;
        }
        let _ = self.db().execute("UPDATE visits SET title = ?2 WHERE url = ?1", params![url, title]);
        let _ = self.db().execute("UPDATE favorites SET title = ?2 WHERE url = ?1 AND title = ''", params![url, title]);
    }

    /// Einträge, deren Adresse oder Titel alle Wörter von `query` enthält.
    fn matching(&self, query: &str, order: &str, limit: usize) -> Vec<Entry> {
        let words: Vec<String> = query
            .split_whitespace()
            .map(|w| format!("%{}%", w.to_lowercase().replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_")))
            .collect();
        let mut sql = String::from("SELECT url, title, count, last FROM visits WHERE 1");
        for i in 0..words.len() {
            sql += &format!(" AND (lower(url) LIKE ?{0} ESCAPE '\\' OR lower(title) LIKE ?{0} ESCAPE '\\')", i + 1);
        }
        sql += &format!(" ORDER BY {order} LIMIT {limit}");
        let db = self.db();
        let Ok(mut stmt) = db.prepare(&sql) else { return Vec::new() };
        let rows = stmt.query_map(rusqlite::params_from_iter(words.iter()), |r| {
            Ok(Entry { url: r.get(0)?, title: r.get(1)?, visits: r.get(2)?, last: r.get(3)? })
        });
        match rows {
            Ok(rows) => rows.filter_map(Result::ok).collect(),
            Err(e) => {
                log::warn!("Verlauf-Abfrage: {e}");
                Vec::new()
            }
        }
    }

    /// Vorschläge für die Adressleiste, die besten zuerst.
    pub fn suggest(&self, query: &str, limit: usize) -> Vec<Entry> {
        let query = query.trim();
        if query.is_empty() {
            return Vec::new();
        }
        let mut entries = self.matching(query, "last DESC", 300);
        let q = query.to_lowercase();
        let t = now();
        let score = |e: &Entry| {
            let bare = e.url.split("://").nth(1).unwrap_or(&e.url).trim_start_matches("www.").to_lowercase();
            let prefix = if bare.starts_with(&q) { 4.0 } else { 0.0 };
            let days = ((t - e.last).max(0) as f64) / 86400.0;
            let recency = 3.0 / (1.0 + days);
            // Kurze Adressen (Startseiten) vor tiefen Unterseiten.
            let short = 1.0 / (1.0 + bare.len() as f64 / 30.0);
            prefix + (e.visits as f64 + 1.0).ln() + recency + short
        };
        entries.sort_by(|a, b| score(b).total_cmp(&score(a)));
        entries.truncate(limit);
        entries
    }

    /// Für die Verlauf-Seite: neueste zuerst.
    pub fn recent(&self, query: &str, limit: usize) -> Vec<Entry> {
        self.matching(query, "last DESC", limit)
    }

    /// Löscht alles seit `seconds` Sekunden; `None` löscht den ganzen Verlauf.
    pub fn delete_recent(&self, seconds: Option<i64>) {
        let r = match seconds {
            Some(s) => self.db().execute("DELETE FROM visits WHERE last >= ?1", params![now() - s]),
            None => self.db().execute("DELETE FROM visits", []),
        };
        if let Err(e) = r {
            log::warn!("Verlauf nicht gelöscht: {e}");
        }
        // Platz in der Datei wirklich freigeben.
        let _ = self.db().execute_batch("VACUUM;");
    }

    /// Alle Hosts, die man selbst besucht hat (für das Wegräumen von Drittanbieter-Cookies).
    pub fn visited_hosts(&self) -> std::collections::HashSet<String> {
        let db = self.db();
        let Ok(mut stmt) = db.prepare("SELECT url FROM visits") else { return Default::default() };
        stmt.query_map([], |r| r.get::<_, String>(0))
            .map(|rows| {
                rows.filter_map(Result::ok)
                    .filter_map(|u| url::Url::parse(&u).ok()?.host_str().map(|h| h.to_ascii_lowercase()))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn delete_url(&self, url: &str) {
        let _ = self.db().execute("DELETE FROM visits WHERE url = ?1", params![url]);
    }

    pub fn favorites(&self) -> Vec<Favorite> {
        let db = self.db();
        let Ok(mut stmt) = db.prepare("SELECT url, title FROM favorites ORDER BY pos, rowid") else { return Vec::new() };
        stmt.query_map([], |r| Ok(Favorite { url: r.get(0)?, title: r.get(1)? }))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn is_favorite(&self, url: &str) -> bool {
        self.db()
            .query_row("SELECT 1 FROM favorites WHERE url = ?1", params![url], |_| Ok(()))
            .is_ok()
    }

    /// Pinnt die Seite an oder löst sie. `true`, wenn sie danach angepinnt ist.
    pub fn toggle_favorite(&self, url: &str, title: &str) -> bool {
        if self.is_favorite(url) {
            self.remove_favorite(url);
            false
        } else {
            let _ = self.db().execute(
                "INSERT INTO favorites (url, title, pos) VALUES (?1, ?2, (SELECT COALESCE(MAX(pos), 0) + 1 FROM favorites))",
                params![url, title],
            );
            true
        }
    }

    /// Pinnt eine Seite an, falls sie es noch nicht ist (Import von Lesezeichen).
    pub fn add_favorite(&self, url: &str, title: &str) -> bool {
        self.db()
            .execute(
                "INSERT OR IGNORE INTO favorites (url, title, pos) VALUES (?1, ?2, (SELECT COALESCE(MAX(pos), 0) + 1 FROM favorites))",
                params![url, title],
            )
            .is_ok_and(|n| n > 0)
    }

    /// Übernimmt Verlauf aus einem anderen Browser; vorhandene Einträge behalten den höheren Wert.
    pub fn import_visits(&self, visits: &[(String, String, i64, i64)]) -> usize {
        let mut db = self.db();
        let Ok(tx) = db.transaction() else { return 0 };
        let mut n = 0;
        for (url, title, count, last) in visits {
            if !is_recordable(url) {
                continue;
            }
            let r = tx.execute(
                "INSERT INTO visits (url, title, count, last) VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(url) DO UPDATE SET count = MAX(count, ?3), last = MAX(last, ?4),
                     title = CASE WHEN title = '' THEN ?2 ELSE title END",
                params![url, title, (*count).max(1), last],
            );
            if r.is_ok() {
                n += 1;
            }
        }
        let _ = tx.commit();
        n
    }

    pub fn remove_favorite(&self, url: &str) {
        let _ = self.db().execute("DELETE FROM favorites WHERE url = ?1", params![url]);
    }
}

/// "vor 5 Min.", "vor 3 Std.", "vor 2 Tagen" – ohne Zeitzonen-Bibliothek.
pub fn ago(last: i64) -> String {
    let d = (now() - last).max(0);
    match d {
        0..60 => "gerade eben".into(),
        60..3600 => format!("vor {} Min.", d / 60),
        3600..86400 => format!("vor {} Std.", d / 3600),
        86400..172800 => "gestern".into(),
        _ => format!("vor {} Tagen", d / 86400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn besuche_und_vorschlaege() {
        let s = Store::in_memory();
        s.record_visit("https://de.wikipedia.org/wiki/Rust", Some("Rust – Wikipedia"));
        s.record_visit("https://www.rust-lang.org/", Some("Rust Programming Language"));
        s.record_visit("https://www.rust-lang.org/", None);
        s.record_visit("vyre://neuer-tab", Some("Neuer Tab"));
        let r = s.suggest("rust", 5);
        assert_eq!(r.len(), 2);
        // Adress-Anfang und mehr Besuche schlagen Treffer im Titel.
        assert_eq!(r[0].url, "https://www.rust-lang.org/");
        assert_eq!(r[0].visits, 2);
        assert_eq!(r[0].title, "Rust Programming Language");
        assert_eq!(s.suggest("wikipedia rust", 5).len(), 1);
        assert!(s.suggest("python", 5).is_empty());
        // Sonderzeichen sind keine Platzhalter.
        assert!(s.suggest("%", 5).is_empty());
    }

    #[test]
    fn loeschen() {
        let s = Store::in_memory();
        s.record_visit_at("https://alt.de/", None, now() - 10 * 86400);
        s.record_visit("https://neu.de/", None);
        s.delete_recent(Some(3600));
        assert_eq!(s.recent("", 10).len(), 1);
        assert_eq!(s.recent("", 10)[0].url, "https://alt.de/");
        s.delete_url("https://alt.de/");
        assert!(s.recent("", 10).is_empty());
        s.record_visit("https://x.de/", None);
        s.delete_recent(None);
        assert!(s.recent("", 10).is_empty());
    }

    #[test]
    fn favoriten() {
        let s = Store::in_memory();
        assert!(s.toggle_favorite("https://a.de/", "A"));
        assert!(s.toggle_favorite("https://b.de/", ""));
        s.set_title("https://b.de/", "B");
        assert_eq!(s.favorites(), [
            Favorite { url: "https://a.de/".into(), title: "A".into() },
            Favorite { url: "https://b.de/".into(), title: "B".into() },
        ]);
        assert!(!s.toggle_favorite("https://a.de/", "A"));
        assert_eq!(s.favorites().len(), 1);
    }
}
