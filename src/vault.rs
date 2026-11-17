//! Passwort-Tresor: `%LOCALAPPDATA%\Vyre\passwoerter.sqlite`.
//!
//! Jedes Passwort ist einzeln mit Windows DPAPI verschlüsselt (an dein Windows-Konto gebunden).
//! Ohne deine Windows-Anmeldung lässt sich die Datei nicht entschlüsseln, auch nicht auf einem
//! anderen PC. Benutzernamen und Seiten stehen unverschlüsselt darin (für die Suche).

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{Connection, params};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Login {
    pub id: i64,
    /// Ursprung der Seite, z. B. "https://example.org".
    pub origin: String,
    pub username: String,
}

pub struct Vault {
    conn: Mutex<Connection>,
}

/// Ursprung (Schema + Host + Port) einer Adresse; Logins gelten pro Ursprung.
pub fn origin_of(url: &str) -> Option<String> {
    let u = url::Url::parse(url).ok()?;
    if !matches!(u.scheme(), "http" | "https") {
        return None;
    }
    Some(u.origin().ascii_serialization())
}

#[cfg(windows)]
mod dpapi {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };

    fn blob(data: &[u8]) -> CRYPT_INTEGER_BLOB {
        CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 }
    }

    fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
        // SAFETY: DPAPI hat `cbData` Bytes bei `pbData` angelegt; danach mit LocalFree freigeben.
        unsafe {
            let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            LocalFree(out.pbData as _);
            v
        }
    }

    pub fn protect(data: &[u8]) -> Option<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: gültige Eingabe- und Ausgabe-Puffer, keine Benutzeroberfläche.
        let ok = unsafe {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }

    pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
        let input = blob(data);
        let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
        // SAFETY: wie oben.
        let ok = unsafe {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        };
        (ok != 0).then(|| take(out))
    }
}

#[cfg(windows)]
pub use dpapi::{protect, unprotect};

impl Vault {
    pub fn open(path: &Path) -> Self {
        match Connection::open(path) {
            Ok(conn) => Self::init(conn),
            Err(e) => {
                log::warn!("Passwort-Tresor {} nicht zu öffnen ({e}), nur im Speicher", path.display());
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
             CREATE TABLE IF NOT EXISTS logins (
                 id INTEGER PRIMARY KEY,
                 origin TEXT NOT NULL,
                 username TEXT NOT NULL DEFAULT '',
                 password BLOB NOT NULL,
                 created INTEGER NOT NULL DEFAULT 0,
                 last_used INTEGER NOT NULL DEFAULT 0,
                 UNIQUE(origin, username)
             );
             CREATE TABLE IF NOT EXISTS never (origin TEXT PRIMARY KEY);",
        );
        if let Err(e) = r {
            log::error!("Passwort-Tresor: {e}");
        }
        Self { conn: Mutex::new(conn) }
    }

    fn db(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Logins für einen Ursprung, zuletzt benutzte zuerst.
    pub fn logins_for(&self, origin: &str) -> Vec<Login> {
        self.query("SELECT id, origin, username FROM logins WHERE origin = ?1 ORDER BY last_used DESC, username", [origin])
    }

    /// Alle Logins (für die Verwaltungsseite), optional gefiltert.
    pub fn all(&self, filter: &str) -> Vec<Login> {
        let like = format!("%{}%", filter.to_lowercase());
        self.query(
            "SELECT id, origin, username FROM logins
             WHERE lower(origin) LIKE ?1 OR lower(username) LIKE ?1 ORDER BY origin, username",
            [like],
        )
    }

    fn query<P: rusqlite::Params>(&self, sql: &str, p: P) -> Vec<Login> {
        let db = self.db();
        let Ok(mut stmt) = db.prepare(sql) else { return Vec::new() };
        stmt.query_map(p, |r| Ok(Login { id: r.get(0)?, origin: r.get(1)?, username: r.get(2)? }))
            .map(|rows| rows.filter_map(Result::ok).collect())
            .unwrap_or_default()
    }

    pub fn get(&self, id: i64) -> Option<Login> {
        self.query("SELECT id, origin, username FROM logins WHERE id = ?1", [id]).into_iter().next()
    }

    /// Entschlüsseltes Passwort eines Logins.
    pub fn password(&self, id: i64) -> Option<String> {
        let blob: Vec<u8> = self
            .db()
            .query_row("SELECT password FROM logins WHERE id = ?1", [id], |r| r.get(0))
            .ok()?;
        let plain = decrypt(&blob)?;
        let _ = self.db().execute("UPDATE logins SET last_used = ?2 WHERE id = ?1", params![id, crate::history::now()]);
        String::from_utf8(plain).ok()
    }

    /// Speichert oder aktualisiert ein Login. `false`, wenn die Verschlüsselung scheitert.
    pub fn save(&self, origin: &str, username: &str, password: &str) -> bool {
        let Some(enc) = encrypt(password.as_bytes()) else {
            log::error!("Passwort konnte nicht verschlüsselt werden");
            return false;
        };
        let now = crate::history::now();
        self.db()
            .execute(
                "INSERT INTO logins (origin, username, password, created, last_used) VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(origin, username) DO UPDATE SET password = ?3, last_used = ?4",
                params![origin, username, enc, now],
            )
            .is_ok()
    }

    /// Ist genau dieses Login (gleiches Passwort) schon gespeichert?
    pub fn has_exact(&self, origin: &str, username: &str, password: &str) -> bool {
        self.query("SELECT id, origin, username FROM logins WHERE origin = ?1 AND username = ?2", params![origin, username])
            .first()
            .and_then(|l| self.password_quiet(l.id))
            .is_some_and(|p| p == password)
    }

    fn password_quiet(&self, id: i64) -> Option<String> {
        let blob: Vec<u8> = self.db().query_row("SELECT password FROM logins WHERE id = ?1", [id], |r| r.get(0)).ok()?;
        String::from_utf8(decrypt(&blob)?).ok()
    }

    pub fn delete(&self, id: i64) {
        let _ = self.db().execute("DELETE FROM logins WHERE id = ?1", [id]);
    }

    pub fn never_save(&self, origin: &str) {
        let _ = self.db().execute("INSERT OR IGNORE INTO never (origin) VALUES (?1)", [origin]);
    }

    pub fn is_never(&self, origin: &str) -> bool {
        self.db().query_row("SELECT 1 FROM never WHERE origin = ?1", [origin], |_| Ok(())).is_ok()
    }

    pub fn count(&self) -> usize {
        self.db().query_row("SELECT COUNT(*) FROM logins", [], |r| r.get::<_, i64>(0)).unwrap_or(0) as usize
    }
}

#[cfg(windows)]
fn encrypt(data: &[u8]) -> Option<Vec<u8>> {
    protect(data)
}

#[cfg(windows)]
fn decrypt(data: &[u8]) -> Option<Vec<u8>> {
    unprotect(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dpapi_hin_und_zurueck() {
        let enc = protect(b"geheim123").unwrap();
        assert_ne!(enc, b"geheim123");
        assert_eq!(unprotect(&enc).unwrap(), b"geheim123");
    }

    #[test]
    fn speichern_finden_loeschen() {
        let v = Vault::in_memory();
        let o = origin_of("https://login.example.org/anmelden?x=1").unwrap();
        assert_eq!(o, "https://login.example.org");
        assert!(v.save(&o, "anna", "pw1"));
        assert!(v.save(&o, "ben", "pw2"));
        assert!(v.save(&o, "anna", "pw1-neu"));
        let list = v.logins_for(&o);
        assert_eq!(list.len(), 2);
        let anna = list.iter().find(|l| l.username == "anna").unwrap();
        assert_eq!(v.password(anna.id).as_deref(), Some("pw1-neu"));
        assert!(v.has_exact(&o, "anna", "pw1-neu"));
        assert!(!v.has_exact(&o, "anna", "pw1"));
        assert!(v.logins_for("https://other.org").is_empty());
        v.delete(anna.id);
        assert_eq!(v.count(), 1);
        v.never_save("https://nie.de");
        assert!(v.is_never("https://nie.de"));
        assert!(origin_of("vyre://neuer-tab").is_none());
    }
}
