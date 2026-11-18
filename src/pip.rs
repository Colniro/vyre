//! Bild-in-Bild: Ein Video läuft in einem kleinen Fenster über allen anderen weiter.
//!
//! Videos spielen in Vyre nur in WebView2-Tabs (Servo ist ohne Medien gebaut). Deshalb zieht
//! der ganze Tab ins kleine Fenster um (WebView2 kann sein Elternfenster wechseln), und ein
//! Skript in der Seite streckt das Video auf die ganze Fläche. So läuft es ohne Unterbrechung
//! weiter, auch bei YouTube und anderen Seiten mit Streaming (MSE).
//!
//! Das Seitenskript zeigt beim Hovern über einem Video einen Knopf "Bild-in-Bild" und im
//! kleinen Fenster schlichte Bedienelemente. Es meldet sich mit `__vyre_pip:<befehl>`.

pub const PREFIX: &str = "__vyre_pip:";
/// Standardgröße und kleinste Größe in logischen Pixeln.
pub const DEFAULT_W: f64 = 480.0;
pub const DEFAULT_H: f64 = 270.0;
pub const MIN_W: f64 = 200.0;
pub const MIN_H: f64 = 112.0;

#[derive(Debug, PartialEq)]
pub enum Message {
    /// Knopf über dem Video geklickt.
    Start,
    /// "Zum Tab": Video zurück in seinen Tab, Tab anzeigen.
    Back,
    /// Fenster schließen; das Video läuft im Tab weiter.
    Close,
    /// Verschieben bzw. Größe ändern: Beginn und Abstand zum Beginn (physische Pixel).
    DragStart,
    Drag(i32, i32),
    ResizeStart,
    Resize(i32, i32),
}

pub fn parse(message: &str) -> Option<Message> {
    let rest = message.strip_prefix(PREFIX)?;
    let (cmd, arg) = rest.split_once(':').unwrap_or((rest, ""));
    let pair = || -> Option<(i32, i32)> {
        let (a, b) = arg.split_once(',')?;
        Some((a.trim().parse::<f64>().ok()?.round() as i32, b.trim().parse::<f64>().ok()?.round() as i32))
    };
    Some(match cmd {
        "start" => Message::Start,
        "back" => Message::Back,
        "close" => Message::Close,
        "dragstart" => Message::DragStart,
        "drag" => {
            let (x, y) = pair()?;
            Message::Drag(x, y)
        }
        "resizestart" => Message::ResizeStart,
        "resize" => {
            let (x, y) = pair()?;
            Message::Resize(x, y)
        }
        _ => return None,
    })
}

/// Platz für ein neues Fenster: unten rechts auf der Fläche `[x, y, b, h]` (physische Pixel).
pub fn default_rect(area: [i32; 4], scale: f64) -> [i32; 4] {
    let w = (DEFAULT_W * scale).round() as i32;
    let h = (DEFAULT_H * scale).round() as i32;
    let margin = (24.0 * scale).round() as i32;
    // Unten etwas mehr Abstand für die Taskleiste.
    let bottom = (72.0 * scale).round() as i32;
    [area[0] + area[2] - w - margin, area[1] + area[3] - h - bottom, w, h]
}

/// Liegt das Fenster zu einem guten Teil auf einem der Bildschirme?
pub fn on_screen(rect: [i32; 4], screens: &[[i32; 4]]) -> bool {
    screens.iter().any(|s| {
        let x0 = rect[0].max(s[0]);
        let y0 = rect[1].max(s[1]);
        let x1 = (rect[0] + rect[2]).min(s[0] + s[2]);
        let y1 = (rect[1] + rect[3]).min(s[1] + s[3]);
        x1 - x0 >= 80 && y1 - y0 >= 60
    })
}

/// Ins kleine Fenster: das Video füllt die Fläche. `true`, wenn es ein Video gab.
pub const ENTER_SCRIPT: &str = "window.__vyrePip?window.__vyrePip.enter():false";

/// Zurück in den Tab (läuft weiter).
pub const EXIT_SCRIPT: &str = "window.__vyrePip&&window.__vyrePip.exit(false)";

/// Läuft in jedem WebView2-Tab (nur im obersten Dokument).
pub const PAGE_SCRIPT: &str = r##"(function () {
  if (window.__vyrePip || window.top !== window) return;
  var send = function (m) { if (window.chrome && window.chrome.webview) window.chrome.webview.postMessage("__vyre_pip:" + m); };
  var target = null, over = null, active = false, anc = [], style = null, hadControls = false;
  var host = null, root = null, btn = null, bar = null, mover = null, grip = null, play = null, hideTimer = 0, barTimer = 0;
  var PAGE_CSS =
    "html[data-vyre-pip-on],html[data-vyre-pip-on] body{overflow:hidden!important;background:#000!important}" +
    "html[data-vyre-pip-on] body *{visibility:hidden!important}" +
    "html[data-vyre-pip-on] [data-vyre-pip-anc]{transform:none!important;filter:none!important;contain:none!important;perspective:none!important;will-change:auto!important;opacity:1!important}" +
    "html[data-vyre-pip-on] video[data-vyre-pip]{visibility:visible!important;position:fixed!important;left:0!important;top:0!important;right:auto!important;bottom:auto!important;" +
    "width:100vw!important;height:100vh!important;max-width:none!important;max-height:none!important;min-width:0!important;min-height:0!important;margin:0!important;padding:0!important;" +
    "border:0!important;z-index:2147483640!important;object-fit:contain!important;background:#000!important;transform:none!important;opacity:1!important;clip-path:none!important}" +
    "video[data-vyre-pip]::-webkit-media-controls,video[data-vyre-pip]::-webkit-media-controls-enclosure,video[data-vyre-pip]::-webkit-media-controls-panel{display:none!important}";
  var UI_CSS =
    ".b{position:fixed;z-index:2147483647;display:none;align-items:center;gap:6px;padding:7px 11px;border:0;border-radius:8px;" +
    "background:rgba(21,21,28,.85);color:#e8e8f0;font:600 12px/1 system-ui,'Segoe UI',sans-serif;cursor:pointer;box-shadow:0 2px 10px rgba(0,0,0,.35)}" +
    ".b:hover{background:#9d8cff;color:#15151c}" +
    ".mv{position:fixed;left:0;top:0;width:100vw;height:100vh;z-index:2147483645}" +
    ".bar{position:fixed;left:0;right:0;bottom:0;z-index:2147483646;display:flex;align-items:center;gap:6px;padding:30px 10px 10px;" +
    "background:linear-gradient(transparent,rgba(0,0,0,.7));opacity:0;transition:opacity .15s;font:13px system-ui,'Segoe UI',sans-serif}" +
    ".bar.on{opacity:1}" +
    ".c{min-width:32px;height:30px;padding:0 10px;border:0;border-radius:15px;background:rgba(255,255,255,.16);color:#fff;font:inherit;cursor:pointer;white-space:nowrap}" +
    ".c:hover{background:rgba(255,255,255,.32)}.sp{flex:1}" +
    ".grip{position:fixed;right:0;bottom:0;width:20px;height:20px;cursor:nwse-resize;z-index:2147483647}" +
    "[hidden]{display:none!important}";
  var ensure = function () {
    if (host && host.isConnected) return;
    if (!document.documentElement) return;
    host = document.createElement("vyre-pip");
    host.style.cssText = "all:initial";
    root = host.attachShadow({ mode: "closed" });
    root.innerHTML = "<style>" + UI_CSS + "</style>" +
      "<button class=b title='Video im kleinen Fenster über allem anderen abspielen'>&#x29C9;&nbsp; Bild-in-Bild</button>" +
      "<div class=mv hidden></div>" +
      "<div class=bar hidden><button class=c data-a=back10>&minus;10 s</button><button class=c data-a=play>&#10074;&#10074;</button>" +
      "<button class=c data-a=fwd10>+10 s</button><span class=sp></span>" +
      "<button class=c data-a=tab title='Video zurück in seinen Tab'>&#x21A9; Zum Tab</button><button class=c data-a=close title='Schließen (läuft im Tab weiter)'>&#x2715;</button></div>" +
      "<div class=grip hidden></div>";
    btn = root.querySelector(".b"); bar = root.querySelector(".bar"); mover = root.querySelector(".mv");
    grip = root.querySelector(".grip"); play = root.querySelector("[data-a=play]");
    document.documentElement.appendChild(host);
    var stop = function (ev) { ev.stopPropagation(); };
    ["mousedown", "mouseup", "pointerdown", "pointerup", "click", "dblclick"].forEach(function (t) { host.addEventListener(t, stop); });
    btn.addEventListener("click", function (ev) { ev.preventDefault(); target = over; hideBtn(); send("start"); });
    bar.addEventListener("click", function (ev) {
      var a = ev.target && ev.target.getAttribute && ev.target.getAttribute("data-a");
      if (!a || !target) return;
      if (a === "play") { if (target.paused) target.play(); else target.pause(); }
      else if (a === "back10") target.currentTime = Math.max(0, target.currentTime - 10);
      else if (a === "fwd10") target.currentTime = target.currentTime + 10;
      else if (a === "tab") send("back");
      else if (a === "close") send("close");
      showBar();
    });
    // Ziehen am Video verschiebt das Fenster, an der Ecke ändert es die Größe; ein Klick ohne
    // Bewegung startet oder hält das Video an.
    var drag = function (el, kind, click) {
      el.addEventListener("mousedown", function (ev) {
        if (ev.button !== 0) return;
        ev.preventDefault();
        var sx = ev.screenX, sy = ev.screenY, moved = false, frame = 0, last = null;
        var dpr = window.devicePixelRatio || 1;
        send(kind + "start");
        var move = function (e) {
          var dx = (e.screenX - sx) * dpr, dy = (e.screenY - sy) * dpr;
          if (Math.abs(dx) + Math.abs(dy) > 3) moved = true;
          last = dx + "," + dy;
          if (!frame) frame = requestAnimationFrame(function () { frame = 0; if (last) send(kind + ":" + last); });
        };
        var up = function () {
          removeEventListener("mousemove", move, true); removeEventListener("mouseup", up, true);
          if (last) send(kind + ":" + last);
          if (!moved && click) click();
        };
        addEventListener("mousemove", move, true); addEventListener("mouseup", up, true);
      });
    };
    drag(mover, "drag", function () { if (target) { if (target.paused) target.play(); else target.pause(); } });
    drag(grip, "resize", null);
  };
  var hideBtn = function () { if (btn) btn.style.display = "none"; };
  var showBar = function () {
    if (!bar) return;
    bar.classList.add("on");
    clearTimeout(barTimer);
    barTimer = setTimeout(function () { bar.classList.remove("on"); }, 2200);
  };
  var sync = function () { if (play && target) play.innerHTML = target.paused ? "&#9654;" : "&#10074;&#10074;"; };
  var area = function (v) { var r = v.getBoundingClientRect(); return r.width * r.height; };
  var largest = function () {
    var best = null, score = 0;
    document.querySelectorAll("video").forEach(function (v) {
      var s = area(v) * (v.paused ? 1 : 4);
      if (s > score) { score = s; best = v; }
    });
    return best;
  };
  var videoAt = function (x, y) {
    var list = document.elementsFromPoint ? document.elementsFromPoint(x, y) : [];
    for (var i = 0; i < list.length; i++) if (list[i].tagName === "VIDEO") return list[i];
    return null;
  };
  addEventListener("mousemove", function (ev) {
    if (active) { showBar(); return; }
    if (ev.target === host) return;
    var v = videoAt(ev.clientX, ev.clientY);
    if (!v) { hideBtn(); return; }
    var r = v.getBoundingClientRect();
    if (r.width < 200 || r.height < 110) { hideBtn(); return; }
    ensure();
    if (!btn) return;
    over = v;
    btn.style.display = "flex";
    btn.style.left = Math.max(4, Math.min(r.right, innerWidth) - btn.offsetWidth - 10) + "px";
    btn.style.top = Math.max(r.top, 0) + 10 + "px";
    clearTimeout(hideTimer);
    hideTimer = setTimeout(hideBtn, 2500);
  }, true);
  addEventListener("keydown", function (ev) {
    if (!active || !target) return;
    if (ev.key === " " || ev.key === "k") { if (target.paused) target.play(); else target.pause(); }
    else if (ev.key === "ArrowLeft") target.currentTime = Math.max(0, target.currentTime - 5);
    else if (ev.key === "ArrowRight") target.currentTime = target.currentTime + 5;
    else return;
    ev.preventDefault(); ev.stopPropagation(); showBar();
  }, true);
  window.__vyrePip = {
    enter: function () {
      var v = target && target.isConnected ? target : largest();
      if (!v) return false;
      ensure();
      target = v; active = true;
      v.setAttribute("data-vyre-pip", "");
      // Die eigenen Bedienelemente des Videos würden unsere überdecken.
      hadControls = v.hasAttribute("controls");
      v.removeAttribute("controls");
      for (var e = v.parentElement; e; e = e.parentElement) { e.setAttribute("data-vyre-pip-anc", ""); anc.push(e); }
      document.documentElement.setAttribute("data-vyre-pip-on", "");
      if (!style) { style = document.createElement("style"); style.textContent = PAGE_CSS; }
      (document.head || document.documentElement).appendChild(style);
      hideBtn();
      mover.hidden = false; bar.hidden = false; grip.hidden = false;
      v.addEventListener("play", sync); v.addEventListener("pause", sync);
      sync(); showBar();
      return true;
    },
    exit: function (pause) {
      if (!active) return;
      active = false;
      document.documentElement.removeAttribute("data-vyre-pip-on");
      anc.forEach(function (e) { e.removeAttribute("data-vyre-pip-anc"); });
      anc = [];
      if (style && style.parentNode) style.parentNode.removeChild(style);
      if (mover) { mover.hidden = true; bar.hidden = true; grip.hidden = true; }
      if (target) {
        target.removeAttribute("data-vyre-pip");
        if (hadControls) target.setAttribute("controls", "");
        target.removeEventListener("play", sync); target.removeEventListener("pause", sync);
        if (pause) target.pause();
        try { target.scrollIntoView({ block: "center" }); } catch (e) {}
      }
    }
  };
})();"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nachrichten() {
        assert_eq!(parse("__vyre_pip:start"), Some(Message::Start));
        assert_eq!(parse("__vyre_pip:drag:12.4,-30"), Some(Message::Drag(12, -30)));
        assert_eq!(parse("__vyre_pip:resize:5,6"), Some(Message::Resize(5, 6)));
        assert_eq!(parse("__vyre_pip:drag:x"), None);
        assert_eq!(parse("__vyre_pip:quatsch"), None);
        assert_eq!(parse("__vyre_pw:focus"), None);
    }

    #[test]
    fn platz_auf_dem_bildschirm() {
        let r = default_rect([0, 0, 1920, 1080], 1.0);
        assert_eq!(r, [1416, 738, 480, 270]);
        assert!(on_screen(r, &[[0, 0, 1920, 1080]]));
        // Zweiter Bildschirm abgesteckt: Fenster läge daneben.
        assert!(!on_screen([2000, 100, 480, 270], &[[0, 0, 1920, 1080]]));
        assert!(on_screen([2000, 100, 480, 270], &[[0, 0, 1920, 1080], [1920, 0, 1920, 1080]]));
    }
}
