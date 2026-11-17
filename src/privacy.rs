//! Datenschutz: nur HTTPS, Drittanbieter-Cookies wegräumen, Fingerprinting erschweren,
//! unauffällige Browser-Kennung.

use std::collections::HashSet;

use url::{Host, Url};

/// Vyre gibt sich als Firefox ESR aus: verbreitet und unauffällig. "Servo" wäre selten und damit
/// gut wiedererkennbar.
pub const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";

/// Titel von Servos eigener Fehlerseite, wenn eine Seite nicht geladen werden konnte.
pub const SERVO_ERROR_TITLE: &str = "Error loading page";

/// Lokale Adressen bekommen kein erzwungenes HTTPS (Entwicklungsserver, Router).
pub fn is_local(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => {
            d == "localhost" || d.ends_with(".localhost") || d.ends_with(".local") || d.ends_with(".lan") || !d.contains('.')
        }
        Some(Host::Ipv4(ip)) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => true,
    }
}

/// Soll diese Adresse auf HTTPS umgeleitet werden? Liefert die HTTPS-Adresse.
/// `allowed`: Seiten, für die man „Trotzdem öffnen“ gewählt hat.
pub fn upgrade(url: &Url, allowed: &[String]) -> Option<Url> {
    if url.scheme() != "http" || is_local(url) {
        return None;
    }
    let host = url.host_str()?.trim_start_matches("www.").to_ascii_lowercase();
    if allowed.iter().any(|a| a == &host) {
        return None;
    }
    let mut https = url.clone();
    https.set_scheme("https").ok()?;
    if https.port() == Some(80) {
        let _ = https.set_port(None);
    }
    Some(https)
}

/// Welche Seiten mit Cookies sind Drittanbieter? Alles, was weder im Verlauf noch in einem offenen
/// Tab vorkommt. `sites` sind Servos Seitennamen (meist die registrierbare Domain, z. B. "google.com").
pub fn third_party_sites(sites: &[String], visited_hosts: &HashSet<String>) -> Vec<String> {
    sites
        .iter()
        .filter(|site| {
            let site = site.trim_start_matches('.').to_ascii_lowercase();
            !visited_hosts.iter().any(|h| h == &site || h.ends_with(&format!(".{site}")))
        })
        .cloned()
        .collect()
}

/// Läuft vor den Skripten jeder Seite: einheitliche Gerätewerte, leicht verrauschtes Auslesen von
/// Canvas und Audio. So sehen viele Vyre-Nutzer für Fingerprinting-Skripte gleich aus.
pub const FINGERPRINT_SCRIPT: &str = r#"(function () {
  if (window.__vyreFp) return; window.__vyreFp = true;
  function def(obj, prop, value) {
    try { Object.defineProperty(obj, prop, { get: function () { return value; }, configurable: true }); } catch (e) {}
  }
  try {
    def(Navigator.prototype, 'hardwareConcurrency', 4);
    def(Navigator.prototype, 'deviceMemory', 8);
    // Bildschirm: die nächstgrößere übliche Auflösung statt der echten.
    var sizes = [[1366, 768], [1536, 864], [1920, 1080], [2560, 1440], [3840, 2160]];
    var w = screen.width, h = screen.height, s = sizes[sizes.length - 1];
    for (var i = 0; i < sizes.length; i++) { if (sizes[i][0] >= w && sizes[i][1] >= h) { s = sizes[i]; break; } }
    def(Screen.prototype, 'width', s[0]); def(Screen.prototype, 'height', s[1]);
    def(Screen.prototype, 'availWidth', s[0]); def(Screen.prototype, 'availHeight', s[1] - 40);
    def(Screen.prototype, 'colorDepth', 24); def(Screen.prototype, 'pixelDepth', 24);
  } catch (e) {}
  // Canvas: beim Auslesen ein paar Pixel minimal verändern (für das Auge unsichtbar).
  try {
    var seed = (Math.random() * 251) | 0;
    var noise = function (d) { for (var i = seed * 4; i < d.length; i += 4093) { d[i] = d[i] ^ 1; } };
    var gid = CanvasRenderingContext2D.prototype.getImageData;
    CanvasRenderingContext2D.prototype.getImageData = function () {
      var r = gid.apply(this, arguments); noise(r.data); return r;
    };
    var exportNoisy = function (orig) {
      return function () {
        try {
          var w = this.width, h = this.height;
          if (w && h && w * h < 4000000) {
            var c = document.createElement('canvas'); c.width = w; c.height = h;
            var x = c.getContext('2d'); x.drawImage(this, 0, 0);
            var d = gid.call(x, 0, 0, w, h); noise(d.data); x.putImageData(d, 0, 0);
            return orig.apply(c, arguments);
          }
        } catch (e) {}
        return orig.apply(this, arguments);
      };
    };
    HTMLCanvasElement.prototype.toDataURL = exportNoisy(HTMLCanvasElement.prototype.toDataURL);
    if (HTMLCanvasElement.prototype.toBlob) HTMLCanvasElement.prototype.toBlob = exportNoisy(HTMLCanvasElement.prototype.toBlob);
  } catch (e) {}
  // Audio: winziges Rauschen in ausgelesenen Audiodaten.
  try {
    if (window.AudioBuffer) {
      var gcd = AudioBuffer.prototype.getChannelData;
      AudioBuffer.prototype.getChannelData = function () {
        var d = gcd.apply(this, arguments);
        for (var i = 0; i < d.length; i += 997) { d[i] = d[i] + 1e-7; }
        return d;
      };
    }
    if (window.AnalyserNode) {
      var gff = AnalyserNode.prototype.getFloatFrequencyData;
      AnalyserNode.prototype.getFloatFrequencyData = function (arr) {
        gff.apply(this, arguments);
        for (var i = 0; i < arr.length; i += 97) { arr[i] = arr[i] + 0.0001; }
      };
    }
  } catch (e) {}
})();"#;

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn https_umleitung() {
        assert_eq!(upgrade(&u("http://example.org/a?b"), &[]).unwrap().as_str(), "https://example.org/a?b");
        assert_eq!(upgrade(&u("http://example.org:80/"), &[]).unwrap().as_str(), "https://example.org/");
        assert!(upgrade(&u("https://example.org/"), &[]).is_none());
        assert!(upgrade(&u("http://localhost:8080/"), &[]).is_none());
        assert!(upgrade(&u("http://192.168.1.1/"), &[]).is_none());
        assert!(upgrade(&u("http://fritz.box.local/"), &[]).is_none());
        assert!(upgrade(&u("http://router/"), &[]).is_none());
        assert!(upgrade(&u("http://www.alt.de/"), &["alt.de".into()]).is_none());
    }

    #[test]
    fn drittanbieter_erkennen() {
        let visited: HashSet<String> = ["de.wikipedia.org", "www.spiegel.de"].iter().map(|s| s.to_string()).collect();
        let sites: Vec<String> = ["wikipedia.org", "spiegel.de", "doubleclick.net", ".adnxs.com"].iter().map(|s| s.to_string()).collect();
        assert_eq!(third_party_sites(&sites, &visited), ["doubleclick.net", ".adnxs.com"]);
    }
}
