//! WebView2 (Chromium von Microsoft Edge) als Ersatz-Engine für Seiten, die Servo nicht schafft.
//!
//! Jeder Edge-Tab ist ein eigenes Kind-Fenster von WebView2 im Hauptfenster, genau wie Servos
//! Seitenfenster. Die Laufzeit (`msedgewebview2.exe`) startet erst, wenn der erste Tab sie
//! braucht, und benutzt ein eigenes Profil unter `%LOCALAPPDATA%\Vyre\WebView2`.
//!
//! Abschottung: Diagnose, Crash-Berichte, SmartScreen, Hintergrund-Verbindungen, Autofill und
//! Passwort-Speicher sind abgeschaltet. Was WebView2 darüber hinaus selbst sendet, lässt sich
//! von außen nicht garantiert abstellen.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;

use url::Url;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::*;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VK_CONTROL, VK_DELETE, VK_F2, VK_F3, VK_F5, VK_F6, VK_LEFT, VK_MENU, VK_RIGHT, VK_SHIFT, VK_TAB,
};
use windows::core::{BOOL, HSTRING, Interface, PCWSTR, PWSTR};
use winit::keyboard::{Key, ModifiersState, NamedKey, SmolStr};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

use crate::blocker::Blocker;
use crate::engine::PageState;
use crate::history::Store;

/// Startoptionen, die WebView2 so weit wie möglich vom Nach-Hause-Telefonieren abhalten.
const BROWSER_ARGS: &str = "--disable-features=msSmartScreenProtection,msEdgeShoppingAssistant,\
msEdgeCollections,msWebOOUI,msPdfOOUI,msEdgeFeedback,msEdgeTranslate,AutofillServerCommunication \
--disable-background-networking --disable-component-update --disable-domain-reliability \
--disable-client-side-phishing-detection --disable-sync --disable-breakpad --no-pings \
--metrics-recording-only --no-first-run --no-default-browser-check";

/// Was ein Edge-Tab der Oberfläche melden will.
pub enum EdgeEvent {
    /// Ein Tastenkürzel des Browsers wurde in der Seite gedrückt.
    Shortcut(Key, ModifiersState),
    /// Die Seite hat den Tastaturfokus bekommen (Klick hinein).
    Focused,
    /// Die Seite will einen neuen Tab öffnen.
    OpenTab(String),
    /// HTTPS ist nach Vyres Umleitung gescheitert (Adresse mit https).
    NoHttps(String),
    /// Nachricht des Passwort-Skripts: Adresse der Seite, Nachricht.
    Message(String, String),
    /// Meldung der Seitenkonsole: Stufe, Text.
    Console(String, String),
}

/// Gemeinsamer Zustand aller Edge-Tabs.
pub struct EdgeShared {
    pub main: Arc<Window>,
    pub history: Arc<Store>,
    pub blocker: Arc<Blocker>,
    pub settings: crate::settings::Shared,
    /// Hosts, die Vyre auf HTTPS umgeleitet hat (gemeinsam mit den Servo-Tabs).
    pub upgraded: Rc<RefCell<std::collections::HashSet<String>>>,
    pub downloads: crate::downloads::Downloads,
    /// Laufende WebView2-Downloads (zum Pausieren, Fortsetzen, Abbrechen).
    pub download_ops: RefCell<std::collections::HashMap<u64, ICoreWebView2DownloadOperation>>,
    /// Meldungen pro Tab-Nummer, abgeholt vom Hauptprogramm.
    pub events: RefCell<Vec<(u64, EdgeEvent)>>,
}

enum EnvState {
    None,
    Creating(Vec<Box<dyn FnOnce(&ICoreWebView2Environment)>>),
    Ready(ICoreWebView2Environment),
    Failed,
}

/// Die WebView2-Laufzeit; wird beim ersten Edge-Tab gestartet.
pub struct EdgeRuntime {
    pub shared: Rc<EdgeShared>,
    env: Rc<RefCell<EnvState>>,
    parent: HWND,
    dir: PathBuf,
    /// Privates Fenster: Tabs im InPrivate-Modus (nichts wird gespeichert).
    private: bool,
}

fn hwnd(window: &Window) -> HWND {
    match window.window_handle().map(|h| h.as_raw()) {
        Ok(RawWindowHandle::Win32(h)) => HWND(h.hwnd.get() as *mut _),
        _ => HWND::default(),
    }
}

fn read(f: impl FnOnce(*mut PWSTR) -> windows::core::Result<()>) -> String {
    let mut p = PWSTR::null();
    if f(&mut p).is_ok() { take_pwstr(p) } else { String::new() }
}

impl EdgeRuntime {
    pub fn new(shared: Rc<EdgeShared>, dir: PathBuf, private: bool) -> Self {
        let parent = hwnd(&shared.main);
        Self { shared, env: Rc::new(RefCell::new(EnvState::None)), parent, dir, private }
    }

    pub fn failed(&self) -> bool {
        matches!(*self.env.borrow(), EnvState::Failed)
    }

    /// Führt `f` aus, sobald die Laufzeit bereit ist (startet sie beim ersten Mal).
    fn with_env(&self, f: impl FnOnce(&ICoreWebView2Environment) + 'static) {
        let mut state = self.env.borrow_mut();
        match &mut *state {
            EnvState::Ready(env) => {
                let env = env.clone();
                drop(state);
                f(&env);
            }
            EnvState::Creating(queue) => queue.push(Box::new(f)),
            EnvState::Failed => log::warn!("WebView2 nicht verfügbar"),
            EnvState::None => {
                *state = EnvState::Creating(vec![Box::new(f)]);
                drop(state);
                self.start();
            }
        }
    }

    fn start(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        let options = CoreWebView2EnvironmentOptions::default();
        // SAFETY: Die Optionen werden nur hier und nur auf diesem Thread gesetzt.
        unsafe {
            options.set_additional_browser_arguments(BROWSER_ARGS.to_string());
            // Abstürze nicht an Microsoft melden.
            options.set_is_custom_crash_reporting_enabled(true);
            options.set_enable_tracking_prevention(true);
            options.set_are_browser_extensions_enabled(false);
        }
        let options: ICoreWebView2EnvironmentOptions = options.into();
        let env_state = self.env.clone();
        let handler = CreateCoreWebView2EnvironmentCompletedHandler::create(Box::new(move |result, env| {
            let queued = match std::mem::replace(&mut *env_state.borrow_mut(), EnvState::Failed) {
                EnvState::Creating(q) => q,
                _ => Vec::new(),
            };
            match (result, env) {
                (Ok(()), Some(env)) => {
                    log::info!("WebView2 bereit");
                    *env_state.borrow_mut() = EnvState::Ready(env.clone());
                    for f in queued {
                        f(&env);
                    }
                }
                (r, _) => log::error!("WebView2 konnte nicht starten: {r:?}"),
            }
            Ok(())
        }));
        let dir = HSTRING::from(self.dir.as_os_str());
        // SAFETY: gültige Zeichenketten und Handler; der Rückruf kommt über die Nachrichtenschleife.
        let r = unsafe { CreateCoreWebView2EnvironmentWithOptions(PCWSTR::null(), &dir, &options, &handler) };
        if let Err(e) = r {
            log::error!("WebView2-Laufzeit fehlt oder startet nicht: {e}");
            *self.env.borrow_mut() = EnvState::Failed;
        }
    }

    /// Pausiert, setzt fort oder bricht einen WebView2-Download ab.
    pub fn download_control(&self, id: u64, op: &str) {
        let ops = self.shared.download_ops.borrow();
        let Some(d) = ops.get(&id) else { return };
        // SAFETY: gültiges COM-Objekt auf dem UI-Thread.
        let _ = unsafe {
            match op {
                "pause" => d.Pause(),
                "resume" => d.Resume(),
                "cancel" => d.Cancel(),
                _ => Ok(()),
            }
        };
    }

    /// Legt einen Edge-Tab an. Er lädt `url`, sobald WebView2 bereit ist.
    pub fn create_view(
        &self,
        id: u64,
        url: Url,
        bounds: (i32, i32, u32, u32),
        visible: bool,
        cookies: Vec<EdgeCookie>,
    ) -> EdgeView {
        let state = Rc::new(RefCell::new(PageState { url: Some(url.clone()), loading: true, ..Default::default() }));
        let inner = Rc::new(Inner {
            controller: RefCell::new(None),
            webview: RefCell::new(None),
            pending_url: RefCell::new(Some(url.to_string())),
            pending_focus: Cell::new(false),
            bounds: Cell::new(bounds),
            visible: Cell::new(visible),
            closed: Cell::new(false),
        });
        let view = EdgeView { state: state.clone(), inner: inner.clone() };
        let shared = self.shared.clone();
        let parent = self.parent;
        let private = self.private;
        self.with_env(move |env| {
            let env2 = env.clone();
            let handler = CreateCoreWebView2ControllerCompletedHandler::create(Box::new(move |result, controller| {
                let Some(controller) = controller.filter(|_| result.is_ok()) else {
                    log::error!("WebView2-Tab nicht erstellt: {result:?}");
                    return Ok(());
                };
                if inner.closed.get() {
                    let _ = unsafe { controller.Close() };
                    return Ok(());
                }
                if let Err(e) = setup(&env2, &controller, &inner, &state, &shared, id, &cookies) {
                    log::error!("WebView2-Tab einrichten: {e}");
                }
                Ok(())
            }));
            // SAFETY: Hauptfenster-Handle ist gültig, solange Vyre läuft.
            let created = unsafe {
                match env.cast::<ICoreWebView2Environment10>().ok().filter(|_| private) {
                    // Privat: InPrivate-Profil, das beim Schließen verworfen wird.
                    Some(env10) => env10.CreateCoreWebView2ControllerOptions().and_then(|options| {
                        options.SetIsInPrivateModeEnabled(true)?;
                        env10.CreateCoreWebView2ControllerWithOptions(parent, &options, &handler)
                    }),
                    None => env.CreateCoreWebView2Controller(parent, &handler),
                }
            };
            if let Err(e) = created {
                log::error!("WebView2-Tab: {e}");
            }
        });
        view
    }
}

/// Ein Cookie, das beim Wechsel von Servo nach WebView2 mitgenommen wird.
#[derive(Clone, Debug)]
pub struct EdgeCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    pub secure: bool,
    pub http_only: bool,
    /// Ablauf in Sekunden seit 1970; `None` = Sitzungs-Cookie.
    pub expires: Option<f64>,
    /// 0 = None, 1 = Lax, 2 = Strict
    pub same_site: Option<u8>,
}

struct Inner {
    controller: RefCell<Option<ICoreWebView2Controller>>,
    webview: RefCell<Option<ICoreWebView2>>,
    pending_url: RefCell<Option<String>>,
    pending_focus: Cell<bool>,
    bounds: Cell<(i32, i32, u32, u32)>,
    visible: Cell<bool>,
    closed: Cell<bool>,
}

fn rect((x, y, w, h): (i32, i32, u32, u32)) -> RECT {
    RECT { left: x, top: y, right: x + w as i32, bottom: y + h as i32 }
}

/// Richtet einen frisch erstellten WebView2-Tab ein: Einstellungen, Ereignisse, Cookies, Laden.
fn setup(
    env: &ICoreWebView2Environment,
    controller: &ICoreWebView2Controller,
    inner: &Rc<Inner>,
    state: &Rc<RefCell<PageState>>,
    shared: &Rc<EdgeShared>,
    id: u64,
    cookies: &[EdgeCookie],
) -> windows::core::Result<()> {
    // SAFETY: Alle Aufrufe gehen an gültige COM-Objekte auf dem UI-Thread.
    unsafe {
        controller.SetBounds(rect(inner.bounds.get()))?;
        controller.SetIsVisible(inner.visible.get())?;
        let webview = controller.CoreWebView2()?;

        let settings = webview.Settings()?;
        settings.SetIsStatusBarEnabled(false)?;
        if let Ok(s) = settings.cast::<ICoreWebView2Settings4>() {
            s.SetIsGeneralAutofillEnabled(false)?;
            s.SetIsPasswordAutosaveEnabled(false)?;
        }
        if let Ok(s) = settings.cast::<ICoreWebView2Settings8>() {
            s.SetIsReputationCheckingRequired(false)?;
        }
        // Strenger Tracking-Schutz für das WebView2-Profil.
        if let Ok(profile) = webview.cast::<ICoreWebView2_13>().and_then(|w| w.Profile()) {
            if let Ok(p3) = profile.cast::<ICoreWebView2Profile3>() {
                let _ = p3.SetPreferredTrackingPreventionLevel(COREWEBVIEW2_TRACKING_PREVENTION_LEVEL_STRICT);
            }
        }
        // Passwort-Skript: Vorschläge beim Klick in Login-Felder, Logins erkennen.
        webview.AddScriptToExecuteOnDocumentCreated(
            &HSTRING::from(crate::pwfill::PAGE_SCRIPT),
            &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(()))),
        )?;
        let mut message_token = 0i64;
        let sh = shared.clone();
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |sender, args| {
                let (Some(wv), Some(a)) = (sender, args) else { return Ok(()) };
                let message = read(|p| a.TryGetWebMessageAsString(p));
                if message.starts_with(crate::pwfill::PREFIX) || message.starts_with(crate::inspector::PREFIX) {
                    let page = read(|p| wv.Source(p));
                    sh.events.borrow_mut().push((id, EdgeEvent::Message(page, message)));
                    sh.main.request_redraw();
                }
                Ok(())
            })),
            &mut message_token,
        )?;
        // Downloads: in den Downloads-Ordner, Edges eigene Download-Leiste ausblenden.
        if let Ok(wv4) = webview.cast::<ICoreWebView2_4>() {
            let sh = shared.clone();
            let mut dl_token = 0i64;
            wv4.add_DownloadStarting(
                &DownloadStartingEventHandler::create(Box::new(move |_, args| {
                    let Some(a) = args else { return Ok(()) };
                    let op = a.DownloadOperation()?;
                    let uri = read(|p| op.Uri(p));
                    let suggested = read(|p| a.ResultFilePath(p));
                    let name = std::path::Path::new(&suggested)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "download".into());
                    let entry = sh.downloads.add_edge(&uri, &name);
                    a.SetResultFilePath(&HSTRING::from(entry.path.as_os_str()))?;
                    a.SetHandled(true)?;
                    log::info!("Download (WebView2): {uri}");
                    let id = entry.id;
                    let report = {
                        let sh = sh.clone();
                        move |op: &ICoreWebView2DownloadOperation| {
                            let (mut got, mut total) = (0i64, 0i64);
                            let _ = op.BytesReceived(&mut got);
                            let _ = op.TotalBytesToReceive(&mut total);
                            let mut st = COREWEBVIEW2_DOWNLOAD_STATE_IN_PROGRESS;
                            let _ = op.State(&mut st);
                            let state = if st == COREWEBVIEW2_DOWNLOAD_STATE_COMPLETED {
                                crate::downloads::State::Done
                            } else if st == COREWEBVIEW2_DOWNLOAD_STATE_INTERRUPTED {
                                let mut reason = COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED;
                                let _ = op.InterruptReason(&mut reason);
                                if reason == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_PAUSED {
                                    crate::downloads::State::Paused
                                } else if reason == COREWEBVIEW2_DOWNLOAD_INTERRUPT_REASON_USER_CANCELED {
                                    crate::downloads::State::Cancelled
                                } else {
                                    crate::downloads::State::Failed(format!("Abbruch ({})", reason.0))
                                }
                            } else {
                                crate::downloads::State::Running
                            };
                            let total = (total > 0).then_some(total as u64);
                            sh.downloads.update_edge(id, got.max(0) as u64, total, state);
                        }
                    };
                    let report2 = report.clone();
                    let mut t = 0i64;
                    op.add_BytesReceivedChanged(
                        &BytesReceivedChangedEventHandler::create(Box::new(move |op, _| {
                            if let Some(op) = op {
                                report(&op);
                            }
                            Ok(())
                        })),
                        &mut t,
                    )?;
                    op.add_StateChanged(
                        &StateChangedEventHandler::create(Box::new(move |op, _| {
                            if let Some(op) = op {
                                report2(&op);
                            }
                            Ok(())
                        })),
                        &mut t,
                    )?;
                    sh.download_ops.borrow_mut().insert(id, op);
                    Ok(())
                })),
                &mut dl_token,
            )?;
        }
        // Konsole der Seite über das DevTools-Protokoll mitlesen (für `vyre konsole`).
        for event in ["Runtime.consoleAPICalled", "Runtime.exceptionThrown"] {
            let receiver = webview.GetDevToolsProtocolEventReceiver(&HSTRING::from(event))?;
            let sh = shared.clone();
            let mut t = 0i64;
            receiver.add_DevToolsProtocolEventReceived(
                &DevToolsProtocolEventReceivedEventHandler::create(Box::new(move |_, args| {
                    let Some(a) = args else { return Ok(()) };
                    let json = read(|p| a.ParameterObjectAsJson(p));
                    if let Some((level, text)) = console_from_devtools(&json) {
                        sh.events.borrow_mut().push((id, EdgeEvent::Console(level, text)));
                        sh.main.request_redraw();
                    }
                    Ok(())
                })),
                &mut t,
            )?;
        }
        webview.CallDevToolsProtocolMethod(
            &HSTRING::from("Runtime.enable"),
            &HSTRING::from("{}"),
            &CallDevToolsProtocolMethodCompletedHandler::create(Box::new(|_, _| Ok(()))),
        )?;
        // Fingerprinting-Schutz vor den Skripten jeder Seite.
        webview.AddScriptToExecuteOnDocumentCreated(
            &HSTRING::from(crate::privacy::FINGERPRINT_SCRIPT),
            &AddScriptToExecuteOnDocumentCreatedCompletedHandler::create(Box::new(|_, _| Ok(()))),
        )?;

        let mut token = 0i64;

        // Adresse und Verlauf.
        let (st, sh) = (state.clone(), shared.clone());
        webview.add_SourceChanged(
            &SourceChangedEventHandler::create(Box::new(move |sender, args| {
                let Some(wv) = sender else { return Ok(()) };
                let url = read(|p| wv.Source(p));
                let new_doc = args.map(|a| {
                    let mut b = BOOL(0);
                    let _ = a.IsNewDocument(&mut b);
                    b.as_bool()
                });
                sh.history.record_visit(&url, None);
                let mut s = st.borrow_mut();
                s.url = Url::parse(&url).ok();
                if new_doc == Some(true) {
                    s.blocked = 0;
                }
                drop(s);
                sh.main.request_redraw();
                Ok(())
            })),
            &mut token,
        )?;
        let (st, sh) = (state.clone(), shared.clone());
        webview.add_DocumentTitleChanged(
            &DocumentTitleChangedEventHandler::create(Box::new(move |sender, _| {
                let Some(wv) = sender else { return Ok(()) };
                let title = read(|p| wv.DocumentTitle(p));
                let url = read(|p| wv.Source(p));
                sh.history.set_title(&url, &title);
                st.borrow_mut().title = Some(title);
                sh.main.request_redraw();
                Ok(())
            })),
            &mut token,
        )?;
        let (st, sh) = (state.clone(), shared.clone());
        webview.add_HistoryChanged(
            &HistoryChangedEventHandler::create(Box::new(move |sender, _| {
                let Some(wv) = sender else { return Ok(()) };
                let (mut back, mut fwd) = (BOOL(0), BOOL(0));
                let _ = wv.CanGoBack(&mut back);
                let _ = wv.CanGoForward(&mut fwd);
                let mut s = st.borrow_mut();
                s.can_back = back.as_bool();
                s.can_forward = fwd.as_bool();
                drop(s);
                sh.main.request_redraw();
                Ok(())
            })),
            &mut token,
        )?;
        let (st, sh) = (state.clone(), shared.clone());
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |sender, args| {
                // Nur HTTPS: unverschlüsselte Adressen umleiten.
                if let (Some(wv), Some(a)) = (sender, args) {
                    let uri = read(|p| a.Uri(p));
                    if let Ok(url) = Url::parse(&uri) {
                        let allowed = sh.settings.get().http_allowed.clone();
                        if let Some(https) = crate::privacy::upgrade(&url, &allowed) {
                            if let Some(h) = https.host_str() {
                                sh.upgraded.borrow_mut().insert(h.to_string());
                            }
                            a.SetCancel(true)?;
                            wv.Navigate(&HSTRING::from(https.as_str()))?;
                            return Ok(());
                        }
                    }
                }
                st.borrow_mut().loading = true;
                sh.main.request_redraw();
                Ok(())
            })),
            &mut token,
        )?;
        let (st, sh) = (state.clone(), shared.clone());
        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |sender, args| {
                st.borrow_mut().loading = false;
                sh.main.request_redraw();
                // HTTPS nach Umleitung gescheitert: Warnseite.
                if let (Some(wv), Some(a)) = (&sender, &args) {
                    let mut ok = BOOL(1);
                    let _ = a.IsSuccess(&mut ok);
                    if !ok.as_bool() {
                        let src = read(|p| wv.Source(p));
                        if let Ok(url) = Url::parse(&src) {
                            let upgraded = url.host_str().is_some_and(|h| sh.upgraded.borrow().contains(h));
                            if url.scheme() == "https" && upgraded {
                                sh.events.borrow_mut().push((id, EdgeEvent::NoHttps(src)));
                                return Ok(());
                            }
                        }
                    }
                }
                if let Some(wv) = sender {
                    hide_ads(&wv, &sh.blocker);
                }
                Ok(())
            })),
            &mut token,
        )?;

        // Links mit target="_blank" und window.open als Vyre-Tab öffnen.
        let sh = shared.clone();
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(move |_, args| {
                if let Some(a) = args {
                    let uri = read(|p| a.Uri(p));
                    a.SetHandled(true)?;
                    sh.events.borrow_mut().push((id, EdgeEvent::OpenTab(uri)));
                    sh.main.request_redraw();
                }
                Ok(())
            })),
            &mut token,
        )?;

        // Werbeblocker: jede Anfrage prüfen (außer der Seite selbst).
        webview.AddWebResourceRequestedFilter(&HSTRING::from("*"), COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL)?;
        let (st, sh, env) = (state.clone(), shared.clone(), env.clone());
        webview.add_WebResourceRequested(
            &WebResourceRequestedEventHandler::create(Box::new(move |sender, args| {
                let (Some(wv), Some(a)) = (sender, args) else { return Ok(()) };
                let mut ctx = COREWEBVIEW2_WEB_RESOURCE_CONTEXT_ALL;
                a.ResourceContext(&mut ctx)?;
                if ctx == COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT {
                    return Ok(());
                }
                let uri = read(|p| a.Request()?.Uri(p));
                let page = read(|p| wv.Source(p));
                if sh.blocker.should_block(&uri, &page, request_kind(ctx)) {
                    log::debug!("blockiert (WebView2): {uri}");
                    let response = env.CreateWebResourceResponse(
                        None::<&IStream>,
                        403,
                        &HSTRING::from("Blocked"),
                        &HSTRING::new(),
                    )?;
                    a.SetResponse(&response)?;
                    st.borrow_mut().blocked += 1;
                    sh.events.borrow_mut().push((id, EdgeEvent::Message(page.clone(), crate::inspector::blocked_message(&uri))));
                    sh.main.request_redraw();
                }
                Ok(())
            })),
            &mut token,
        )?;

        // Tastenkürzel des Browsers abfangen, bevor die Seite sie bekommt.
        let sh = shared.clone();
        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(a) = args else { return Ok(()) };
                let mut kind = COREWEBVIEW2_KEY_EVENT_KIND_KEY_UP;
                a.KeyEventKind(&mut kind)?;
                if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN {
                    return Ok(());
                }
                let mut vk = 0u32;
                a.VirtualKey(&mut vk)?;
                let mods = current_modifiers();
                if let Some(key) = shortcut_key(vk, mods) {
                    a.SetHandled(true)?;
                    sh.events.borrow_mut().push((id, EdgeEvent::Shortcut(key, mods)));
                    sh.main.request_redraw();
                }
                Ok(())
            })),
            &mut token,
        )?;
        let sh = shared.clone();
        controller.add_GotFocus(
            &FocusChangedEventHandler::create(Box::new(move |_, _| {
                sh.events.borrow_mut().push((id, EdgeEvent::Focused));
                sh.main.request_redraw();
                Ok(())
            })),
            &mut token,
        )?;

        // Cookies aus Servo mitnehmen, damit man angemeldet bleibt.
        if !cookies.is_empty() {
            if let Ok(cm) = webview.cast::<ICoreWebView2_2>().and_then(|w| w.CookieManager()) {
                for c in cookies {
                    let Ok(cookie) = cm.CreateCookie(
                        &HSTRING::from(c.name.as_str()),
                        &HSTRING::from(c.value.as_str()),
                        &HSTRING::from(c.domain.as_str()),
                        &HSTRING::from(c.path.as_str()),
                    ) else {
                        continue;
                    };
                    let _ = cookie.SetIsSecure(c.secure);
                    let _ = cookie.SetIsHttpOnly(c.http_only);
                    if let Some(e) = c.expires {
                        let _ = cookie.SetExpires(e);
                    }
                    let kind = match c.same_site {
                        Some(0) => Some(COREWEBVIEW2_COOKIE_SAME_SITE_KIND_NONE),
                        Some(1) => Some(COREWEBVIEW2_COOKIE_SAME_SITE_KIND_LAX),
                        Some(2) => Some(COREWEBVIEW2_COOKIE_SAME_SITE_KIND_STRICT),
                        _ => None,
                    };
                    if let Some(k) = kind {
                        let _ = cookie.SetSameSite(k);
                    }
                    let _ = cm.AddOrUpdateCookie(&cookie);
                }
                log::info!("{} Cookies an WebView2 übergeben", cookies.len());
            }
        }

        *inner.controller.borrow_mut() = Some(controller.clone());
        *inner.webview.borrow_mut() = Some(webview.clone());
        if let Some(url) = inner.pending_url.borrow_mut().take() {
            webview.Navigate(&HSTRING::from(url))?;
        }
        if inner.pending_focus.get() {
            let _ = controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC);
        }
    }
    Ok(())
}

/// Macht aus einem DevTools-Ereignis (consoleAPICalled / exceptionThrown) Stufe und Text.
fn console_from_devtools(json: &str) -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    if let Some(details) = v.get("exceptionDetails") {
        let text = details
            .get("exception")
            .and_then(|e| e.get("description"))
            .and_then(|d| d.as_str())
            .or_else(|| details.get("text").and_then(|t| t.as_str()))
            .unwrap_or("Fehler");
        return Some(("fehler".into(), text.to_string()));
    }
    let level = match v.get("type")?.as_str()? {
        "error" | "assert" => "fehler",
        "warning" => "warnung",
        "debug" | "trace" => "debug",
        _ => "info",
    };
    let parts: Vec<String> = v
        .get("args")?
        .as_array()?
        .iter()
        .map(|a| match a.get("value") {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(other) => other.to_string(),
            None => a.get("description").and_then(|d| d.as_str()).unwrap_or("").to_string(),
        })
        .collect();
    Some((level.into(), parts.join(" ")))
}

fn request_kind(ctx: COREWEBVIEW2_WEB_RESOURCE_CONTEXT) -> &'static str {
    match ctx {
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_DOCUMENT => "document",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_STYLESHEET => "stylesheet",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_IMAGE => "image",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_MEDIA => "media",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FONT => "font",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_SCRIPT => "script",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_XML_HTTP_REQUEST | COREWEBVIEW2_WEB_RESOURCE_CONTEXT_FETCH => "xmlhttprequest",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_WEBSOCKET => "websocket",
        COREWEBVIEW2_WEB_RESOURCE_CONTEXT_PING => "ping",
        _ => "other",
    }
}

/// Ausblend-Regeln wie in Servo: seitenspezifisch und nach Klassen/IDs der Seite.
fn hide_ads(webview: &ICoreWebView2, blocker: &Arc<Blocker>) {
    let page = read(|p| unsafe { webview.Source(p) });
    let Some(c) = blocker.cosmetic(&page) else { return };
    run_script(webview, &css_script(&c.css), |_| {});
    if !c.generic {
        return;
    }
    let (wv, blocker) = (webview.clone(), blocker.clone());
    run_script(webview, crate::engine::COLLECT_CLASSES_IDS, move |json| {
        let Ok(serde_json::Value::Array(parts)) = serde_json::from_str::<serde_json::Value>(&json) else { return };
        let list = |v: Option<&serde_json::Value>| -> Vec<String> {
            v.and_then(|v| v.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                .unwrap_or_default()
        };
        let css = blocker.generic_css(&page, &list(parts.first()), &list(parts.get(1)));
        run_script(&wv, &css_script(&css), |_| {});
    });
}

fn css_script(css: &str) -> String {
    if css.is_empty() {
        return String::new();
    }
    format!(
        "(function(){{var s=document.createElement('style');s.className='vyre-adblock';\
         s.textContent={};(document.head||document.documentElement).appendChild(s);}})()",
        crate::engine::js_string(css)
    )
}

fn run_script(webview: &ICoreWebView2, script: &str, done: impl FnOnce(String) + 'static) {
    if script.is_empty() {
        return;
    }
    let mut done = Some(done);
    let handler = ExecuteScriptCompletedHandler::create(Box::new(move |result, json| {
        if result.is_ok() {
            if let Some(d) = done.take() {
                d(json);
            }
        }
        Ok(())
    }));
    // SAFETY: gültiges WebView2-Objekt auf dem UI-Thread.
    let _ = unsafe { webview.ExecuteScript(&HSTRING::from(script), &handler) };
}

fn current_modifiers() -> ModifiersState {
    // SAFETY: GetKeyState liest nur den Tastaturzustand des Threads.
    let down = |vk: u16| unsafe { GetKeyState(vk as i32) } < 0;
    let mut m = ModifiersState::empty();
    if down(VK_CONTROL.0) {
        m |= ModifiersState::CONTROL;
    }
    if down(VK_SHIFT.0) {
        m |= ModifiersState::SHIFT;
    }
    if down(VK_MENU.0) {
        m |= ModifiersState::ALT;
    }
    m
}

/// Übersetzt eine Taste in ein Browser-Kürzel, wenn sie eines ist.
fn shortcut_key(vk: u32, mods: ModifiersState) -> Option<Key> {
    let vk16 = vk as u16;
    let key = match vk16 {
        0x30..=0x39 | 0x41..=0x5A => {
            let c = char::from_u32(vk)?.to_ascii_lowercase();
            Key::Character(SmolStr::new(c.to_string()))
        }
        v if v == VK_TAB.0 => Key::Named(NamedKey::Tab),
        v if v == VK_F2.0 => Key::Named(NamedKey::F2),
        v if v == VK_F3.0 => Key::Named(NamedKey::F3),
        v if v == VK_F5.0 => Key::Named(NamedKey::F5),
        v if v == VK_F6.0 => Key::Named(NamedKey::F6),
        v if v == VK_LEFT.0 => Key::Named(NamedKey::ArrowLeft),
        v if v == VK_RIGHT.0 => Key::Named(NamedKey::ArrowRight),
        v if v == VK_DELETE.0 => Key::Named(NamedKey::Delete),
        _ => return None,
    };
    crate::keys::is_browser_shortcut(&key, mods).then_some(key)
}

/// Ein Tab, der mit WebView2 läuft.
pub struct EdgeView {
    pub state: Rc<RefCell<PageState>>,
    inner: Rc<Inner>,
}

impl EdgeView {
    fn webview(&self) -> Option<ICoreWebView2> {
        self.inner.webview.borrow().clone()
    }

    pub fn load(&self, url: &Url) {
        match self.webview() {
            // SAFETY: gültiges WebView2-Objekt auf dem UI-Thread.
            Some(wv) => unsafe {
                let _ = wv.Navigate(&HSTRING::from(url.as_str()));
            },
            None => *self.inner.pending_url.borrow_mut() = Some(url.to_string()),
        }
    }

    pub fn reload(&self) {
        if let Some(wv) = self.webview() {
            let _ = unsafe { wv.Reload() };
        }
    }

    pub fn go_back(&self) {
        if let Some(wv) = self.webview() {
            let _ = unsafe { wv.GoBack() };
        }
    }

    pub fn go_forward(&self) {
        if let Some(wv) = self.webview() {
            let _ = unsafe { wv.GoForward() };
        }
    }

    pub fn set_bounds(&self, bounds: (i32, i32, u32, u32)) {
        if self.inner.bounds.replace(bounds) == bounds {
            return;
        }
        if let Some(c) = self.inner.controller.borrow().as_ref() {
            let _ = unsafe { c.SetBounds(rect(bounds)) };
        }
    }

    pub fn set_visible(&self, visible: bool) {
        self.inner.visible.set(visible);
        if let Some(c) = self.inner.controller.borrow().as_ref() {
            let _ = unsafe { c.SetIsVisible(visible) };
        }
    }

    /// Spielt die Seite gerade Ton ab?
    pub fn is_playing_audio(&self) -> bool {
        let Some(wv) = self.webview() else { return false };
        let Ok(wv8) = wv.cast::<ICoreWebView2_8>() else { return false };
        let mut playing = BOOL(0);
        // SAFETY: gültiges WebView2-Objekt auf dem UI-Thread.
        let _ = unsafe { wv8.IsDocumentPlayingAudio(&mut playing) };
        playing.as_bool()
    }

    /// Führt ein Skript aus und liefert das Ergebnis als JSON (leer, wenn nicht möglich).
    pub fn eval(&self, script: &str, done: impl FnOnce(String) + 'static) {
        match self.webview() {
            Some(wv) => run_script(&wv, script, done),
            None => done(String::new()),
        }
    }

    pub fn focus(&self) {
        match self.inner.controller.borrow().as_ref() {
            Some(c) => {
                let _ = unsafe { c.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC) };
            }
            None => self.inner.pending_focus.set(true),
        }
    }
}

impl Drop for EdgeView {
    fn drop(&mut self) {
        self.inner.closed.set(true);
        if let Some(c) = self.inner.controller.borrow_mut().take() {
            let _ = unsafe { c.Close() };
        }
        self.inner.webview.borrow_mut().take();
    }
}

/// Holt die Cookies einer Adresse aus Servo, um sie WebView2 mitzugeben.
pub fn cookies_from_servo(servo: &servo::Servo, url: &Url) -> Vec<EdgeCookie> {
    let host = url.host_str().unwrap_or_default().to_string();
    servo
        .site_data_manager()
        .cookies_for_url(url.clone(), servo::CookieSource::HTTP)
        .into_iter()
        .map(|c| EdgeCookie {
            name: c.name().to_string(),
            value: c.value().to_string(),
            domain: c.domain().map(str::to_string).unwrap_or_else(|| host.clone()),
            path: c.path().unwrap_or("/").to_string(),
            secure: c.secure().unwrap_or(false),
            http_only: c.http_only().unwrap_or(false),
            expires: c.expires_datetime().map(|t| t.unix_timestamp() as f64),
            same_site: c.same_site().map(|s| {
                if s.is_strict() {
                    2
                } else if s.is_lax() {
                    1
                } else {
                    0
                }
            }),
        })
        .collect()
}

