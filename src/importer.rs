//! Import von Passwörtern, Lesezeichen und Verlauf aus anderen Browsern und aus CSV-Dateien.
//!
//! - Chrome, Edge, Brave: Profile unter `%LOCALAPPDATA%`. Passwörter sind mit einem Schlüssel
//!   verschlüsselt, der selbst per DPAPI geschützt in `Local State` liegt (AES-256-GCM, "v10").
//!   Neuere Chrome-Versionen schützen manche Daten zusätzlich ("v20", an Chrome gebunden); die
//!   lassen sich von außen nicht lesen und werden übersprungen – dafür gibt es den CSV-Import.
//! - Firefox, Zen: Profile unter `%APPDATA%`. Passwörter in `logins.json`, der Schlüssel in
//!   `key4.db` (NSS: PBKDF2 + AES-256-CBC bzw. 3DES). Mit Hauptpasswort geht es nicht.
//! - CSV: Export aus jedem Browser oder Passwort-Manager (Spalten werden am Kopf erkannt).
//!
//! Gelesen wird immer eine Kopie der Dateien, damit der andere Browser offen bleiben kann.

use std::path::{Path, PathBuf};

use base64::Engine as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Chrome,
    Edge,
    Brave,
    Firefox,
    Zen,
    Csv,
}

impl Source {
    pub const ALL: [Source; 6] = [Source::Chrome, Source::Edge, Source::Brave, Source::Firefox, Source::Zen, Source::Csv];

    pub fn id(self) -> &'static str {
        match self {
            Source::Chrome => "chrome",
            Source::Edge => "edge",
            Source::Brave => "brave",
            Source::Firefox => "firefox",
            Source::Zen => "zen",
            Source::Csv => "csv",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Source::Chrome => "Chrome",
            Source::Edge => "Edge",
            Source::Brave => "Brave",
            Source::Firefox => "Firefox",
            Source::Zen => "Zen",
            Source::Csv => "CSV-Datei",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.id() == id)
    }

    fn dir(self) -> Option<PathBuf> {
        let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
        let roaming = std::env::var_os("APPDATA").map(PathBuf::from);
        match self {
            Source::Chrome => local.map(|p| p.join(r"Google\Chrome\User Data")),
            Source::Edge => local.map(|p| p.join(r"Microsoft\Edge\User Data")),
            Source::Brave => local.map(|p| p.join(r"BraveSoftware\Brave-Browser\User Data")),
            Source::Firefox => roaming.map(|p| p.join(r"Mozilla\Firefox\Profiles")),
            Source::Zen => roaming.map(|p| p.join(r"zen\Profiles")),
            Source::Csv => None,
        }
    }

    /// Ist der Browser auf diesem PC installiert (bzw. CSV immer)?
    pub fn available(self) -> bool {
        self == Source::Csv || self.dir().is_some_and(|d| d.is_dir())
    }
}

#[derive(Debug, Default)]
pub struct Imported {
    /// Adresse, Benutzername, Passwort
    pub logins: Vec<(String, String, String)>,
    /// Titel, Adresse
    pub bookmarks: Vec<(String, String)>,
    /// Adresse, Titel, Besuche, letzter Besuch (Sekunden seit 1970)
    pub history: Vec<(String, String, i64, i64)>,
    /// Passwörter, die sich nicht entschlüsseln ließen.
    pub skipped_passwords: usize,
    /// Hinweise für den Bericht.
    pub notes: Vec<String>,
}

pub fn import(source: Source) -> Result<Imported, String> {
    match source {
        Source::Chrome | Source::Edge | Source::Brave => chromium(&source.dir().ok_or("kein Profil")?),
        Source::Firefox | Source::Zen => firefox(&source.dir().ok_or("kein Profil")?),
        Source::Csv => {
            let Some(path) = rfd::FileDialog::new()
                .set_title("Passwörter aus CSV importieren")
                .add_filter("CSV", &["csv"])
                .pick_file()
            else {
                return Err("keine Datei gewählt".into());
            };
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            Ok(Imported { logins: parse_csv(&text)?, ..Default::default() })
        }
    }
}

/// Kopiert eine (evtl. gesperrte) Datenbank samt WAL in einen Temp-Ordner und öffnet die Kopie.
fn open_copy(path: &Path) -> Option<(rusqlite::Connection, tempdir::Dir)> {
    if !path.is_file() {
        return None;
    }
    let dir = tempdir::Dir::new()?;
    let name = path.file_name()?;
    let target = dir.path.join(name);
    std::fs::copy(path, &target).ok()?;
    for ext in ["-wal", "-shm"] {
        let side = PathBuf::from(format!("{}{ext}", path.display()));
        if side.is_file() {
            let _ = std::fs::copy(&side, PathBuf::from(format!("{}{ext}", target.display())));
        }
    }
    let conn = rusqlite::Connection::open(&target).ok()?;
    Some((conn, dir))
}

/// Ein Temp-Ordner, der beim Freigeben gelöscht wird (Kopien von Passwort-Datenbanken!).
mod tempdir {
    use std::path::PathBuf;

    pub struct Dir {
        pub path: PathBuf,
    }

    impl Dir {
        pub fn new() -> Option<Self> {
            let n = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
            let path = std::env::temp_dir().join(format!("vyre-import-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&path).ok()?;
            Some(Self { path })
        }
    }

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

// ------------------------------------------------------------------ Chrome / Edge / Brave

fn chromium(user_data: &Path) -> Result<Imported, String> {
    let mut out = Imported::default();
    let key = chromium_key(user_data);
    if key.is_none() {
        out.notes.push("Schlüssel aus „Local State“ nicht lesbar, Passwörter übersprungen".into());
    }
    let mut profiles: Vec<PathBuf> = std::fs::read_dir(user_data)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            (n == "Default" || n.starts_with("Profile ")) && p.join("Preferences").is_file()
        })
        .collect();
    profiles.sort();
    for profile in profiles {
        if let Some(key) = &key {
            chromium_logins(&profile, key, &mut out);
        }
        chromium_bookmarks(&profile, &mut out);
        chromium_history(&profile, &mut out);
    }
    Ok(out)
}

fn chromium_key(user_data: &Path) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(user_data.join("Local State")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let b64 = v.get("os_crypt")?.get("encrypted_key")?.as_str()?;
    let raw = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    let protected = raw.strip_prefix(b"DPAPI")?;
    crate::vault::unprotect(protected)
}

fn chromium_logins(profile: &Path, key: &[u8], out: &mut Imported) {
    let Some((db, _tmp)) = open_copy(&profile.join("Login Data")) else { return };
    let Ok(mut stmt) = db.prepare("SELECT origin_url, username_value, password_value FROM logins WHERE blacklisted_by_user = 0") else {
        return;
    };
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, Vec<u8>>(2)?)));
    let Ok(rows) = rows else { return };
    for (url, user, enc) in rows.flatten() {
        match chromium_decrypt(key, &enc) {
            Some(pass) if !pass.is_empty() => out.logins.push((url, user, pass)),
            Some(_) => {}
            None => out.skipped_passwords += 1,
        }
    }
}

/// "v10"/"v11": AES-256-GCM mit 12 Byte Nonce. Ältere Einträge: direkt DPAPI. "v20": nicht lesbar.
fn chromium_decrypt(key: &[u8], enc: &[u8]) -> Option<String> {
    use aes_gcm::aead::{Aead, KeyInit};
    if enc.starts_with(b"v10") || enc.starts_with(b"v11") {
        if enc.len() < 3 + 12 + 16 {
            return None;
        }
        let cipher = aes_gcm::Aes256Gcm::new_from_slice(key).ok()?;
        let nonce = aes_gcm::Nonce::from_slice(&enc[3..15]);
        let plain = cipher.decrypt(nonce, &enc[15..]).ok()?;
        return String::from_utf8(plain).ok();
    }
    if enc.starts_with(b"v20") {
        return None;
    }
    String::from_utf8(crate::vault::unprotect(enc)?).ok()
}

fn chromium_bookmarks(profile: &Path, out: &mut Imported) {
    let Ok(text) = std::fs::read_to_string(profile.join("Bookmarks")) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    fn walk(node: &serde_json::Value, out: &mut Vec<(String, String)>) {
        if node.get("type").and_then(|t| t.as_str()) == Some("url") {
            if let (Some(name), Some(url)) = (node.get("name").and_then(|n| n.as_str()), node.get("url").and_then(|u| u.as_str())) {
                out.push((name.to_string(), url.to_string()));
            }
        }
        if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
            for c in children {
                walk(c, out);
            }
        }
    }
    if let Some(roots) = v.get("roots").and_then(|r| r.as_object()) {
        for root in roots.values() {
            walk(root, &mut out.bookmarks);
        }
    }
}

fn chromium_history(profile: &Path, out: &mut Imported) {
    let Some((db, _tmp)) = open_copy(&profile.join("History")) else { return };
    let Ok(mut stmt) = db.prepare(
        "SELECT url, title, visit_count, last_visit_time FROM urls WHERE hidden = 0 ORDER BY last_visit_time DESC LIMIT 5000",
    ) else {
        return;
    };
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)));
    if let Ok(rows) = rows {
        // Chrome zählt Mikrosekunden seit 1601.
        out.history.extend(rows.flatten().map(|(u, t, c, last)| (u, t, c, last / 1_000_000 - 11_644_473_600)));
    }
}

// ------------------------------------------------------------------ Firefox / Zen

/// Das zuletzt benutzte Profil (jüngste places.sqlite).
fn firefox_profile(profiles: &Path) -> Option<PathBuf> {
    std::fs::read_dir(profiles)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("places.sqlite").is_file())
        .max_by_key(|p| p.join("places.sqlite").metadata().and_then(|m| m.modified()).ok())
}

fn firefox(profiles: &Path) -> Result<Imported, String> {
    let profile = firefox_profile(profiles).ok_or("kein Profil mit Daten gefunden")?;
    let mut out = Imported::default();
    if let Some((db, _tmp)) = open_copy(&profile.join("places.sqlite")) {
        if let Ok(mut stmt) = db.prepare(
            "SELECT COALESCE(b.title, p.title, ''), p.url FROM moz_bookmarks b JOIN moz_places p ON b.fk = p.id
             WHERE b.type = 1 AND p.url LIKE 'http%' ORDER BY b.position",
        ) {
            if let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))) {
                out.bookmarks.extend(rows.flatten());
            }
        }
        if let Ok(mut stmt) = db.prepare(
            "SELECT url, COALESCE(title, ''), visit_count, COALESCE(last_visit_date, 0) FROM moz_places
             WHERE visit_count > 0 AND url LIKE 'http%' ORDER BY last_visit_date DESC LIMIT 5000",
        ) {
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?, r.get::<_, i64>(3)?)));
            if let Ok(rows) = rows {
                // Firefox zählt Mikrosekunden seit 1970.
                out.history.extend(rows.flatten().map(|(u, t, c, last)| (u, t, c, last / 1_000_000)));
            }
        }
    }
    match firefox_key(&profile) {
        Ok(key) => firefox_logins(&profile, &key, &mut out),
        Err(e) => out.notes.push(e),
    }
    Ok(out)
}

/// Entschlüsselt den Hauptschlüssel aus key4.db (ohne Firefox-Hauptpasswort).
fn firefox_key(profile: &Path) -> Result<Vec<u8>, String> {
    let (db, _tmp) = open_copy(&profile.join("key4.db")).ok_or("key4.db nicht gefunden")?;
    let (global_salt, check): (Vec<u8>, Vec<u8>) = db
        .query_row("SELECT item1, item2 FROM metaData WHERE id = 'password'", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(|e| e.to_string())?;
    let ok = nss::pbe_decrypt(&check, &global_salt).ok_or("Passwörter mit Hauptpasswort geschützt – bitte in Firefox als CSV exportieren")?;
    if !ok.starts_with(b"password-check") {
        return Err("Passwörter mit Hauptpasswort geschützt – bitte in Firefox als CSV exportieren".into());
    }
    let a11: Vec<u8> = db.query_row("SELECT a11 FROM nssPrivate", [], |r| r.get(0)).map_err(|e| e.to_string())?;
    let key = nss::pbe_decrypt(&a11, &global_salt).ok_or("Schlüssel in key4.db nicht lesbar")?;
    Ok(key)
}

fn firefox_logins(profile: &Path, key: &[u8], out: &mut Imported) {
    let Ok(text) = std::fs::read_to_string(profile.join("logins.json")) else { return };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return };
    let Some(list) = v.get("logins").and_then(|l| l.as_array()) else { return };
    let b64 = base64::engine::general_purpose::STANDARD;
    for l in list {
        let host = l.get("hostname").and_then(|h| h.as_str()).unwrap_or("").to_string();
        let field = |name: &str| -> Option<String> {
            let der = b64.decode(l.get(name)?.as_str()?).ok()?;
            String::from_utf8(nss::decrypt_login_field(&der, key)?).ok()
        };
        match (field("encryptedUsername"), field("encryptedPassword")) {
            (Some(user), Some(pass)) => out.logins.push((host, user, pass)),
            _ => out.skipped_passwords += 1,
        }
    }
}

/// Die Teile von NSS (Firefox), die zum Entschlüsseln nötig sind.
mod nss {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    use sha1::Digest;

    const OID_PBES2: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0d];
    const OID_AES256_CBC: &[u8] = &[0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x01, 0x2a];
    const OID_DES3_CBC: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x03, 0x07];

    /// Ein DER-Element: Tag, Inhalt, Rest.
    pub fn der(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let tag = *input.first()?;
        let first = *input.get(1)? as usize;
        let (len, head) = if first < 0x80 {
            (first, 2)
        } else {
            let n = first & 0x7f;
            if n == 0 || n > 4 {
                return None;
            }
            let mut len = 0usize;
            for i in 0..n {
                len = (len << 8) | *input.get(2 + i)? as usize;
            }
            (len, 2 + n)
        };
        let content = input.get(head..head + len)?;
        Some((tag, content, &input[head + len..]))
    }

    /// Alle Elemente einer SEQUENCE.
    pub fn items(seq: &[u8]) -> Option<Vec<(u8, &[u8])>> {
        let mut out = Vec::new();
        let mut rest = seq;
        while !rest.is_empty() {
            let (t, c, r) = der(rest)?;
            out.push((t, c));
            rest = r;
        }
        Some(out)
    }

    fn integer(bytes: &[u8]) -> usize {
        bytes.iter().fold(0usize, |a, b| (a << 8) | *b as usize)
    }

    fn cbc_decrypt(alg: &[u8], key: &[u8], iv: &[u8], data: &[u8]) -> Option<Vec<u8>> {
        let mut buf = data.to_vec();
        let len = if alg == OID_AES256_CBC {
            cbc::Decryptor::<aes::Aes256>::new_from_slices(key.get(..32)?, iv).ok()?.decrypt_padded_mut::<Pkcs7>(&mut buf).ok()?.len()
        } else if alg == OID_DES3_CBC {
            cbc::Decryptor::<des::TdesEde3>::new_from_slices(key.get(..24)?, iv).ok()?.decrypt_padded_mut::<Pkcs7>(&mut buf).ok()?.len()
        } else {
            return None;
        };
        buf.truncate(len);
        Some(buf)
    }

    /// Entschlüsselt einen PBES2-Eintrag aus key4.db (leeres Hauptpasswort).
    pub fn pbe_decrypt(blob: &[u8], global_salt: &[u8]) -> Option<Vec<u8>> {
        let (_, outer, _) = der(blob)?;
        let parts = items(outer)?;
        let (_, alg_seq) = parts.first()?;
        let (_, ciphertext) = parts.get(1)?;
        let alg = items(alg_seq)?;
        if alg.first()?.1 != OID_PBES2 {
            return None; // altes Verfahren (Firefox vor 2019) wird nicht unterstützt
        }
        let params = items(alg.get(1)?.1)?;
        let kdf = items(params.first()?.1)?;
        let kdf_params = items(kdf.get(1)?.1)?;
        let salt = kdf_params.first()?.1;
        let iterations = integer(kdf_params.get(1)?.1);
        let key_len = kdf_params.get(2).map(|k| integer(k.1)).filter(|l| *l > 0).unwrap_or(32);
        let enc = items(params.get(1)?.1)?;
        let enc_alg = enc.first()?.1;
        let iv_raw = enc.get(1)?.1;
        // Firefox speichert nur 14 Byte IV; die ersten zwei sind das DER-Präfix 04 0E.
        let iv: Vec<u8> = if iv_raw.len() == 14 { [&[0x04, 0x0e][..], iv_raw].concat() } else { iv_raw.to_vec() };
        let password_hash = sha1::Sha1::digest(global_salt); // sha1(global_salt + leeres Passwort)
        let mut key = vec![0u8; key_len];
        pbkdf2::pbkdf2_hmac::<sha2::Sha256>(&password_hash, salt, iterations as u32, &mut key);
        cbc_decrypt(enc_alg, &key, &iv, ciphertext)
    }

    /// Entschlüsselt ein Feld aus logins.json mit dem Hauptschlüssel.
    pub fn decrypt_login_field(blob: &[u8], key: &[u8]) -> Option<Vec<u8>> {
        let (_, outer, _) = der(blob)?;
        let parts = items(outer)?;
        let alg = items(parts.get(1)?.1)?;
        let (_, data) = parts.get(2)?;
        cbc_decrypt(alg.first()?.1, key, alg.get(1)?.1, data)
    }

    #[cfg(test)]
    pub mod build {
        //! Baut verschlüsselte NSS-Einträge für Tests (Gegenstück zum Entschlüsseln).
        use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
        use sha1::Digest;

        fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
            let mut v = vec![tag];
            if content.len() < 0x80 {
                v.push(content.len() as u8);
            } else {
                v.push(0x82);
                v.push((content.len() >> 8) as u8);
                v.push(content.len() as u8);
            }
            v.extend_from_slice(content);
            v
        }

        fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
            tlv(0x30, &parts.concat())
        }

        pub fn pbe(plain: &[u8], global_salt: &[u8]) -> Vec<u8> {
            let salt = [7u8; 32];
            let iv14 = [9u8; 14];
            let iterations: u16 = 100;
            let mut key = [0u8; 32];
            pbkdf2::pbkdf2_hmac::<sha2::Sha256>(&sha1::Sha1::digest(global_salt), &salt, iterations as u32, &mut key);
            let iv: Vec<u8> = [&[0x04, 0x0e][..], &iv14].concat();
            let ct = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key, &iv).unwrap().encrypt_padded_vec_mut::<Pkcs7>(plain);
            let kdf = seq(&[
                tlv(0x06, &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x05, 0x0c]),
                seq(&[tlv(0x04, &salt), tlv(0x02, &iterations.to_be_bytes()), tlv(0x02, &[32])]),
            ]);
            let enc = seq(&[tlv(0x06, super::OID_AES256_CBC), tlv(0x04, &iv14)]);
            let alg = seq(&[tlv(0x06, super::OID_PBES2), seq(&[kdf, enc])]);
            seq(&[alg, tlv(0x04, &ct)])
        }

        pub fn field(plain: &[u8], key: &[u8]) -> Vec<u8> {
            let iv = [3u8; 16];
            let ct = cbc::Encryptor::<aes::Aes256>::new_from_slices(&key[..32], &iv).unwrap().encrypt_padded_vec_mut::<Pkcs7>(plain);
            seq(&[tlv(0x04, &[1u8; 16]), seq(&[tlv(0x06, super::OID_AES256_CBC), tlv(0x04, &iv)]), tlv(0x04, &ct)])
        }
    }
}

// ------------------------------------------------------------------ CSV

/// Liest Passwörter aus einer CSV-Datei; die Spalten werden am Kopf erkannt.
pub fn parse_csv(text: &str) -> Result<Vec<(String, String, String)>, String> {
    let mut reader = csv::ReaderBuilder::new().flexible(true).from_reader(text.trim_start_matches('\u{feff}').as_bytes());
    let headers: Vec<String> = reader.headers().map_err(|e| e.to_string())?.iter().map(|h| h.trim().to_lowercase()).collect();
    let find = |names: &[&str]| headers.iter().position(|h| names.contains(&h.as_str()));
    let url = find(&["url", "login_uri", "website", "web site", "origin", "hostname", "uri"]).ok_or("Spalte für die Adresse fehlt")?;
    let user = find(&["username", "login_username", "login name", "user", "email", "benutzername"]);
    let pass = find(&["password", "login_password", "passwort"]).ok_or("Spalte für das Passwort fehlt")?;
    let mut out = Vec::new();
    for record in reader.records().flatten() {
        let get = |i: usize| record.get(i).unwrap_or("").trim().to_string();
        let (u, p) = (get(url), get(pass));
        if u.is_empty() || p.is_empty() {
            continue;
        }
        out.push((u, user.map(get).unwrap_or_default(), p));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_verschiedener_programme() {
        let chrome = "name,url,username,password,note\nGitHub,https://github.com/login,anna,geheim,\nleer,https://x.de,,,\n";
        assert_eq!(parse_csv(chrome).unwrap(), [("https://github.com/login".into(), "anna".into(), "geheim".into())]);
        let bitwarden = "\u{feff}folder,favorite,type,name,notes,fields,reprompt,login_uri,login_username,login_password,login_totp\n,,login,Mail,,,0,https://mail.de,ben,\"pw,mit,komma\",\n";
        assert_eq!(parse_csv(bitwarden).unwrap(), [("https://mail.de".into(), "ben".into(), "pw,mit,komma".into())]);
        assert!(parse_csv("a,b\n1,2\n").is_err());
    }

    #[test]
    fn chrome_entschluesseln() {
        use aes_gcm::aead::{Aead, KeyInit};
        let key = [5u8; 32];
        let cipher = aes_gcm::Aes256Gcm::new_from_slice(&key).unwrap();
        let nonce = [1u8; 12];
        let ct = cipher.encrypt(aes_gcm::Nonce::from_slice(&nonce), b"hunter2".as_ref()).unwrap();
        let blob = [b"v10".as_ref(), &nonce, &ct].concat();
        assert_eq!(chromium_decrypt(&key, &blob).as_deref(), Some("hunter2"));
        assert_eq!(chromium_decrypt(&key, b"v20xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"), None);
        // Älteres Format: direkt DPAPI.
        let old = crate::vault::protect(b"alt").unwrap();
        assert_eq!(chromium_decrypt(&key, &old).as_deref(), Some("alt"));
    }

    #[test]
    fn firefox_entschluesseln() {
        let salt = b"globaler-salt";
        let check = nss::build::pbe(b"password-check\x02\x02", salt);
        assert!(nss::pbe_decrypt(&check, salt).unwrap().starts_with(b"password-check"));
        let master = [42u8; 32];
        let wrapped = nss::build::pbe(&master, salt);
        let key = nss::pbe_decrypt(&wrapped, salt).unwrap();
        assert_eq!(key, master);
        let field = nss::build::field(b"anna@example.org", &key);
        assert_eq!(nss::decrypt_login_field(&field, &key).unwrap(), b"anna@example.org");
        // Falsches Salt (= Hauptpasswort gesetzt): scheitert.
        assert!(nss::pbe_decrypt(&check, b"anders").is_none_or(|p| !p.starts_with(b"password-check")));
    }
}
