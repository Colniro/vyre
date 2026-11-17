//! Ausfüllen von Login-Formularen: ein kleines Skript in jeder Seite und die Nachrichten
//! zwischen Seite und Vyre.
//!
//! Die Seite meldet sich über `console.log` (Servo) bzw. `chrome.webview.postMessage`
//! (WebView2) mit Nachrichten, die mit `__vyre_pw:` beginnen:
//! - `focus`: man hat in ein Login-Feld geklickt → Vyre schickt die Benutzernamen für die Seite
//! - `fill:<id>`: man hat ein Konto gewählt → Vyre füllt Benutzername und Passwort aus
//! - `submit:<json>`: ein Login wurde abgeschickt → Vyre bietet an, es zu speichern
//!
//! Vyre bestimmt die Seite selbst aus der Adresse des Tabs, nicht aus der Nachricht. Eine Seite
//! bekommt so nur ihre eigenen Konten.

use crate::engine::js_string;
use crate::vault::Login;

pub const PREFIX: &str = "__vyre_pw:";

/// Was eine Seite gemeldet hat.
#[derive(Debug, PartialEq, Eq)]
pub enum Message {
    Focus,
    Fill(i64),
    Submit { username: String, password: String },
}

pub fn parse(message: &str) -> Option<Message> {
    let rest = message.strip_prefix(PREFIX)?;
    if rest == "focus" {
        return Some(Message::Focus);
    }
    if let Some(id) = rest.strip_prefix("fill:") {
        return id.trim().parse().ok().map(Message::Fill);
    }
    let json = rest.strip_prefix("submit:")?;
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let password = v.get("pass")?.as_str()?.to_string();
    let username = v.get("user").and_then(|u| u.as_str()).unwrap_or("").trim().to_string();
    (!password.is_empty()).then_some(Message::Submit { username, password })
}

/// Zeigt die Vorschlagsliste mit den Benutzernamen (ohne Passwörter).
pub fn show_script(logins: &[Login]) -> String {
    let items: Vec<String> = logins
        .iter()
        .map(|l| format!("{{id:{},user:{}}}", l.id, js_string(&l.username)))
        .collect();
    format!("window.__vyreShow&&window.__vyreShow([{}])", items.join(","))
}

/// Trägt Benutzername und Passwort in das Formular ein.
pub fn fill_script(username: &str, password: &str) -> String {
    format!("window.__vyreFill&&window.__vyreFill({},{})", js_string(username), js_string(password))
}

/// Läuft in jeder Seite (Servo: Nutzer-Skript, WebView2: vor dem Dokument).
pub const PAGE_SCRIPT: &str = r#"(function () {
  if (window.__vyrePw) return; window.__vyrePw = true;
  function send(m) {
    try {
      if (window.chrome && window.chrome.webview) window.chrome.webview.postMessage(m);
      else console.log(m);
    } catch (e) {}
  }
  var box = null, target = null;
  function isPw(e) { return e && e.tagName === 'INPUT' && (e.type || '').toLowerCase() === 'password'; }
  function isText(e) {
    if (!e || e.tagName !== 'INPUT') return false;
    var t = (e.type || 'text').toLowerCase();
    return t === 'text' || t === 'email' || t === 'tel';
  }
  function userField(pw) {
    var root = pw.form || document;
    var inputs = Array.prototype.slice.call(root.querySelectorAll('input'));
    for (var j = inputs.indexOf(pw) - 1; j >= 0; j--) { if (isText(inputs[j])) return inputs[j]; }
    return null;
  }
  function loginField(e) {
    if (isPw(e)) return true;
    if (!isText(e)) return false;
    return !!(e.form || document).querySelector('input[type=password]');
  }
  function hide() { if (box) { box.remove(); box = null; } }
  document.addEventListener('focusin', function (ev) {
    if (loginField(ev.target)) { target = ev.target; send('__vyre_pw:focus'); }
  }, true);
  document.addEventListener('mousedown', function (ev) {
    if (box && !box.contains(ev.target)) hide();
  }, true);
  window.__vyreShow = function (list) {
    hide();
    if (!target || !list.length) return;
    var r = target.getBoundingClientRect();
    box = document.createElement('div');
    box.setAttribute('style', 'position:fixed;z-index:2147483647;left:' + r.left + 'px;top:' + (r.bottom + 4) +
      'px;min-width:' + Math.max(r.width, 220) + 'px;background:#21212b;color:#e8e8f0;border:1px solid #3a3a4a;' +
      'border-radius:10px;padding:4px;font:13px "Segoe UI",sans-serif;box-shadow:0 8px 24px rgba(0,0,0,.45)');
    list.forEach(function (l) {
      var it = document.createElement('div');
      it.textContent = l.user || '(ohne Benutzername)';
      it.setAttribute('style', 'padding:8px 12px;border-radius:7px;cursor:pointer');
      it.onmouseenter = function () { it.style.background = '#343248'; };
      it.onmouseleave = function () { it.style.background = ''; };
      it.onmousedown = function (ev) { ev.preventDefault(); ev.stopPropagation(); send('__vyre_pw:fill:' + l.id); hide(); };
      box.appendChild(it);
    });
    document.documentElement.appendChild(box);
  };
  function setValue(e, v) {
    if (!e) return;
    var d = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value');
    if (d && d.set) d.set.call(e, v); else e.value = v;
    e.dispatchEvent(new Event('input', { bubbles: true }));
    e.dispatchEvent(new Event('change', { bubbles: true }));
  }
  window.__vyreFill = function (user, pass) {
    var pw = isPw(target) ? target : (target && target.form || document).querySelector('input[type=password]');
    var u = pw ? userField(pw) : target;
    if (u && user) setValue(u, user);
    if (pw) setValue(pw, pass);
  };
  var last = '';
  function capture(root) {
    var pw = (root || document).querySelector('input[type=password]');
    if (!pw || !pw.value) return;
    var u = userField(pw);
    var msg = '__vyre_pw:submit:' + JSON.stringify({ user: u ? u.value : '', pass: pw.value });
    if (msg === last) return;
    last = msg; send(msg);
  }
  document.addEventListener('submit', function (ev) { capture(ev.target); }, true);
  document.addEventListener('keydown', function (ev) { if (ev.key === 'Enter' && isPw(ev.target)) capture(ev.target.form); }, true);
  document.addEventListener('click', function (ev) {
    var b = ev.target && ev.target.closest && ev.target.closest('button,input[type=submit]');
    if (b) capture(b.form || (b.closest && b.closest('form')));
  }, true);
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nachrichten_lesen() {
        assert_eq!(parse("__vyre_pw:focus"), Some(Message::Focus));
        assert_eq!(parse("__vyre_pw:fill:12"), Some(Message::Fill(12)));
        assert_eq!(
            parse(r#"__vyre_pw:submit:{"user":" anna ","pass":"x\"y"}"#),
            Some(Message::Submit { username: "anna".into(), password: "x\"y".into() })
        );
        assert_eq!(parse(r#"__vyre_pw:submit:{"user":"a","pass":""}"#), None);
        assert_eq!(parse("hallo"), None);
    }

    #[test]
    fn skripte_maskieren() {
        let s = fill_script("an\"na", "p'w</script>");
        assert!(s.contains(r#""an\"na""#));
        assert!(!s.contains("</script>"));
        let l = [Login { id: 3, origin: "https://a.de".into(), username: "x".into() }];
        assert_eq!(show_script(&l), r#"window.__vyreShow&&window.__vyreShow([{id:3,user:"x"}])"#);
    }
}
