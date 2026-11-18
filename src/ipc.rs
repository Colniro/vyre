//! Steuerkanal für Wyven und das Terminal: der Befehl `vyre` und die Named Pipe `\\.\pipe\vyre`.
//!
//! - `vyre open <adresse>` öffnet einen Tab im laufenden Vyre (startet es, wenn nötig).
//!   Mit `--neben x,y,b,h` (Arbeitsfläche des Monitors) nimmt Vyre die rechte Hälfte ein.
//! - `vyre tabs` listet die offenen Tabs.
//! - `vyre reload` lädt die aktive Seite neu.
//! - `vyre konsole` zeigt laufend die Konsole der aktiven Seite (Strg+C beendet).
//! - `vyre update` sucht sofort nach einer neuen Version.
//!
//! Protokoll: eine JSON-Zeile vom Client, danach Antwortzeilen (Text) vom Server.

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::windows::io::FromRawHandle;
use std::sync::mpsc::{Receiver, Sender, channel};

use serde_json::{Value, json};

pub const PIPE: &str = r"\\.\pipe\vyre";

/// Eine Anfrage an das laufende Vyre.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Open { url: Option<String>, beside: Option<[i32; 4]> },
    Tabs,
    Reload,
    Console,
    Update,
}

impl Request {
    fn to_json(&self) -> Value {
        match self {
            Request::Open { url, beside } => json!({ "cmd": "open", "url": url, "neben": beside }),
            Request::Tabs => json!({ "cmd": "tabs" }),
            Request::Reload => json!({ "cmd": "reload" }),
            Request::Console => json!({ "cmd": "konsole" }),
            Request::Update => json!({ "cmd": "update" }),
        }
    }

    fn from_json(v: &Value) -> Option<Self> {
        Some(match v.get("cmd")?.as_str()? {
            "open" => Request::Open {
                url: v.get("url").and_then(|u| u.as_str()).map(str::to_string),
                beside: v.get("neben").and_then(|n| {
                    let a = n.as_array()?;
                    let get = |i: usize| a.get(i)?.as_i64().map(|x| x as i32);
                    Some([get(0)?, get(1)?, get(2)?, get(3)?])
                }),
            },
            "tabs" => Request::Tabs,
            "reload" => Request::Reload,
            "konsole" => Request::Console,
            "update" => Request::Update,
            _ => return None,
        })
    }
}

/// Liest die Befehlszeile: `vyre open <url> [--neben x,y,b,h]`, `vyre tabs` …
/// `None`, wenn es kein Steuerbefehl ist (dann startet Vyre normal).
pub fn parse_args(args: &[String]) -> Option<Request> {
    let mut beside = None;
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        if args[i] == "--neben" {
            beside = args.get(i + 1).and_then(|v| {
                let n: Vec<i32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
                (n.len() == 4).then(|| [n[0], n[1], n[2], n[3]])
            });
            i += 2;
            continue;
        }
        rest.push(args[i].as_str());
        i += 1;
    }
    match rest.first().copied()? {
        "open" | "öffnen" => Some(Request::Open { url: rest.get(1).map(|s| s.to_string()), beside }),
        "tabs" => Some(Request::Tabs),
        "reload" => Some(Request::Reload),
        "konsole" | "console" => Some(Request::Console),
        "update" | "aktualisieren" => Some(Request::Update),
        _ => None,
    }
}

/// Schickt eine Anfrage an das laufende Vyre und gibt jede Antwortzeile weiter.
/// `Err`, wenn kein Vyre läuft.
pub fn send(request: &Request, mut on_line: impl FnMut(&str)) -> std::io::Result<()> {
    let mut pipe = std::fs::OpenOptions::new().read(true).write(true).open(PIPE)?;
    pipe.write_all(format!("{}\n", request.to_json()).as_bytes())?;
    pipe.flush()?;
    for line in BufReader::new(pipe).lines() {
        match line {
            Ok(l) => on_line(&l),
            Err(_) => break,
        }
    }
    Ok(())
}

/// Läuft gerade ein Vyre mit Steuerkanal?
pub fn running() -> bool {
    std::fs::metadata(PIPE).is_ok() || std::fs::OpenOptions::new().read(true).write(true).open(PIPE).is_ok()
}

/// Eine eingegangene Anfrage; Antwortzeilen gehen über `reply` zurück (Ende: Sender fallen lassen).
#[derive(Debug)]
pub struct Incoming {
    pub request: Request,
    pub reply: Sender<String>,
}

/// Startet den Server-Thread. Jede Anfrage wird an `deliver` gereicht (im Hauptthread verarbeiten).
pub fn serve(deliver: impl Fn(Incoming) + Send + Sync + Clone + 'static) {
    let _ = std::thread::Builder::new().name("vyre-ipc".into()).spawn(move || loop {
        let Some(pipe) = create_pipe() else {
            log::warn!("Steuerkanal {PIPE} nicht verfügbar");
            return;
        };
        let deliver = deliver.clone();
        let _ = std::thread::Builder::new().name("vyre-ipc-client".into()).spawn(move || handle(pipe, deliver));
    });
}

/// Legt eine Instanz der Pipe an und wartet, bis sich ein Client verbindet.
fn create_pipe() -> Option<File> {
    use windows_sys::Win32::Foundation::{ERROR_PIPE_CONNECTED, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT};
    let name: Vec<u16> = PIPE.encode_utf16().chain(Some(0)).collect();
    // SAFETY: gültiger, nullterminierter Name; Standard-Sicherheit (nur dieser Benutzer und Admins).
    let handle = unsafe {
        CreateNamedPipeW(name.as_ptr(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, PIPE_UNLIMITED_INSTANCES, 64 * 1024, 64 * 1024, 0, std::ptr::null())
    };
    if handle == INVALID_HANDLE_VALUE {
        return None;
    }
    // SAFETY: gültiges Pipe-Handle; blockiert, bis sich ein Client verbindet.
    let ok = unsafe { ConnectNamedPipe(handle, std::ptr::null_mut()) } != 0 || unsafe { GetLastError() } == ERROR_PIPE_CONNECTED;
    // SAFETY: Das Handle gehört ab hier der Datei (wird beim Freigeben geschlossen).
    let file = unsafe { File::from_raw_handle(handle as _) };
    ok.then_some(file)
}

fn handle(pipe: File, deliver: impl Fn(Incoming)) {
    let Ok(read_side) = pipe.try_clone() else { return };
    let mut line = String::new();
    if BufReader::new(read_side).read_line(&mut line).is_err() {
        return;
    }
    let Some(request) = serde_json::from_str::<Value>(&line).ok().as_ref().and_then(Request::from_json) else {
        let _ = (&pipe).write_all(b"Unbekannter Befehl\n");
        return;
    };
    let (tx, rx): (Sender<String>, Receiver<String>) = channel();
    deliver(Incoming { request, reply: tx });
    let mut pipe = pipe;
    for answer in rx {
        if pipe.write_all(answer.as_bytes()).and_then(|_| pipe.write_all(b"\n")).and_then(|_| pipe.flush()).is_err() {
            break; // Client hat aufgelegt (z. B. Strg+C bei `vyre konsole`)
        }
    }
}

/// Ein Build aus dem Projektordner (`…\target\fast` usw.)?
fn is_dev_dir(dir: &str) -> bool {
    dir.to_ascii_lowercase().replace('/', "\\").contains("\\target\\")
}

/// Der neue Benutzer-PATH mit `dir`, oder `None`, wenn nichts zu ändern ist.
/// Ein installiertes Vyre geht vor: Es nimmt Entwicklungs-Builds aus dem PATH, und ein
/// Entwicklungs-Build trägt sich nicht ein, wenn schon ein installiertes Vyre im PATH ist.
fn updated_path(current: &str, dir: &str, has_vyre: impl Fn(&str) -> bool) -> Option<String> {
    let same = |p: &str| p.trim().trim_end_matches('\\').eq_ignore_ascii_case(dir.trim_end_matches('\\'));
    let entries: Vec<&str> = current.split(';').filter(|p| !p.trim().is_empty()).collect();
    let mut kept: Vec<&str> = if is_dev_dir(dir) {
        if entries.iter().any(|p| same(p) || (!is_dev_dir(p) && has_vyre(p.trim()))) {
            return None;
        }
        entries.clone()
    } else {
        entries.iter().copied().filter(|p| same(p) || !(is_dev_dir(p) && has_vyre(p.trim()))).collect()
    };
    if !kept.iter().any(|p| same(p)) {
        kept.push(dir);
    }
    (kept != entries).then(|| kept.join(";"))
}

/// Trägt den Ordner von vyre.exe in den Benutzer-PATH ein (einmalig, ohne Adminrechte),
/// damit `vyre` in jedem neuen Terminal funktioniert. Liefert `true`, wenn neu eingetragen.
pub fn ensure_in_path() -> bool {
    use windows_sys::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_EXPAND_SZ, RRF_NOEXPAND, RRF_RT_ANY, RegCloseKey, RegGetValueW,
        RegOpenKeyExW, RegSetValueExW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE};
    let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.to_path_buf())) else { return false };
    let dir = dir.to_string_lossy().to_string();
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() };
    let key_name = wide("Environment");
    let value_name = wide("Path");
    let mut key: HKEY = std::ptr::null_mut();
    // SAFETY: Standard-Registry-Aufrufe mit gültigen Puffern; der Schlüssel wird wieder geschlossen.
    unsafe {
        if RegOpenKeyExW(HKEY_CURRENT_USER, key_name.as_ptr(), 0, KEY_READ | KEY_WRITE, &mut key) != 0 {
            return false;
        }
        let mut size = 0u32;
        RegGetValueW(key, std::ptr::null(), value_name.as_ptr(), RRF_RT_ANY | RRF_NOEXPAND, std::ptr::null_mut(), std::ptr::null_mut(), &mut size);
        let mut buf = vec![0u16; (size as usize / 2).max(1)];
        let current = if size > 0
            && RegGetValueW(key, std::ptr::null(), value_name.as_ptr(), RRF_RT_ANY | RRF_NOEXPAND, std::ptr::null_mut(), buf.as_mut_ptr() as _, &mut size) == 0
        {
            String::from_utf16_lossy(&buf).trim_end_matches('\0').to_string()
        } else {
            String::new()
        };
        let Some(new) = updated_path(&current, &dir, |d| std::path::Path::new(d).join("vyre.exe").is_file()) else {
            RegCloseKey(key);
            return false;
        };
        let data = wide(&new);
        let ok = RegSetValueExW(key, value_name.as_ptr(), 0, REG_EXPAND_SZ, data.as_ptr() as *const u8, (data.len() * 2) as u32) == 0;
        RegCloseKey(key);
        if ok {
            // Laufenden Programmen (Explorer, neue Terminals) Bescheid geben.
            let env = wide("Environment");
            let mut result = 0usize;
            SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, env.as_ptr() as isize, SMTO_ABORTIFHUNG, 2000, &mut result);
            log::info!("{dir} in den Benutzer-PATH eingetragen");
        }
        ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn installiertes_vyre_geht_im_path_vor() {
        let dev = r"C:\P\Vyre\target\fast";
        let inst = r"C:\U\AppData\Local\Programs\Vyre";
        let all = |d: &str| d != r"C:\Tools";
        // Erster Start eines Builds: eintragen.
        assert_eq!(updated_path(r"C:\Tools", dev, all).as_deref(), Some(r"C:\Tools;C:\P\Vyre\target\fast"));
        // Installiert: den Entwicklungs-Build verdrängen.
        let p = format!(r"C:\Tools;{dev}");
        assert_eq!(updated_path(&p, inst, all), Some(format!(r"C:\Tools;{inst}")));
        // Entwicklungs-Build neben installiertem: nichts ändern.
        assert_eq!(updated_path(&format!(r"C:\Tools;{inst}"), dev, all), None);
        // Schon eingetragen: nichts ändern.
        assert_eq!(updated_path(&format!(r"C:\Tools;{inst}\"), inst, all), None);
    }

    #[test]
    fn befehlszeile() {
        assert_eq!(parse_args(&a(&["tabs"])), Some(Request::Tabs));
        assert_eq!(parse_args(&a(&["konsole"])), Some(Request::Console));
        assert_eq!(
            parse_args(&a(&["open", "localhost:5173", "--neben", "0,0,1920,1040"])),
            Some(Request::Open { url: Some("localhost:5173".into()), beside: Some([0, 0, 1920, 1040]) })
        );
        assert_eq!(parse_args(&a(&["https://servo.org"])), None);
        assert_eq!(parse_args(&a(&[])), None);
    }

    #[test]
    fn json_hin_und_zurueck() {
        for r in [
            Request::Open { url: Some("x.de".into()), beside: None },
            Request::Open { url: None, beside: Some([1, 2, 3, 4]) },
            Request::Tabs,
            Request::Reload,
            Request::Console,
            Request::Update,
        ] {
            assert_eq!(Request::from_json(&r.to_json()), Some(r));
        }
    }
}
