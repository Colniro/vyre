//! Suchmaschinen und !Bangs. Bangs werden lokal aufgelöst, ohne Umweg über einen Server:
//! "!yt katzen" oder "katzen !yt" springt direkt zur YouTube-Suche.

use url::Url;

pub struct SearchEngine {
    pub id: &'static str,
    pub name: &'static str,
    /// Adresse mit `{q}` als Platzhalter für die Suchbegriffe.
    pub url: &'static str,
    /// Ein Satz für die Auswahl beim ersten Start.
    pub note: &'static str,
}

pub const ENGINES: &[SearchEngine] = &[
    SearchEngine {
        id: "duckduckgo",
        name: "DuckDuckGo",
        url: "https://duckduckgo.com/?q={q}",
        note: "Kein Tracking, keine Suchprofile.",
    },
    SearchEngine {
        id: "brave",
        name: "Brave Search",
        url: "https://search.brave.com/search?q={q}",
        note: "Eigener, unabhängiger Suchindex, kein Tracking.",
    },
    SearchEngine {
        id: "startpage",
        name: "Startpage",
        url: "https://www.startpage.com/do/search?q={q}",
        note: "Google-Ergebnisse, aber anonym abgefragt.",
    },
    SearchEngine {
        id: "ecosia",
        name: "Ecosia",
        url: "https://www.ecosia.org/search?q={q}",
        note: "Pflanzt mit den Werbeeinnahmen Bäume.",
    },
    SearchEngine {
        id: "google",
        name: "Google",
        url: "https://www.google.com/search?q={q}",
        note: "Die bekannteste Suche, speichert aber viel über dich.",
    },
];

/// Eingebaute !Bangs: Kürzel und Ziel-Adresse mit `{q}`.
const BANGS: &[(&[&str], &str)] = &[
    (&["g", "google"], "https://www.google.com/search?q={q}"),
    (&["gi", "img"], "https://www.google.com/search?tbm=isch&q={q}"),
    (&["ddg"], "https://duckduckgo.com/?q={q}"),
    (&["br", "brave"], "https://search.brave.com/search?q={q}"),
    (&["sp", "startpage"], "https://www.startpage.com/do/search?q={q}"),
    (&["e", "ecosia"], "https://www.ecosia.org/search?q={q}"),
    (&["b", "bing"], "https://www.bing.com/search?q={q}"),
    (&["yt", "youtube"], "https://www.youtube.com/results?search_query={q}"),
    (&["w", "wen", "wiki"], "https://en.wikipedia.org/wiki/Special:Search?search={q}"),
    (&["wde", "wd"], "https://de.wikipedia.org/wiki/Spezial:Suche?search={q}"),
    (&["gh", "github"], "https://github.com/search?q={q}"),
    (&["a", "amazon", "amz"], "https://www.amazon.de/s?k={q}"),
    (&["m", "maps", "osm"], "https://www.openstreetmap.org/search?query={q}"),
    (&["gm", "gmaps"], "https://www.google.com/maps/search/{q}"),
    (&["r", "reddit"], "https://www.reddit.com/search/?q={q}"),
    (&["so", "stackoverflow"], "https://stackoverflow.com/search?q={q}"),
    (&["mdn"], "https://developer.mozilla.org/de/search?q={q}"),
    (&["rs", "docs", "docsrs"], "https://docs.rs/releases/search?query={q}"),
    (&["crates", "crate"], "https://crates.io/search?q={q}"),
    (&["rust", "std"], "https://doc.rust-lang.org/std/?search={q}"),
    (&["npm"], "https://www.npmjs.com/search?q={q}"),
    (&["py", "pypi"], "https://pypi.org/search/?q={q}"),
    (&["imdb"], "https://www.imdb.com/find/?q={q}"),
    (&["leo"], "https://dict.leo.org/englisch-deutsch/{q}"),
    (&["dl", "deepl"], "https://www.deepl.com/translator#auto/de/{q}"),
    (&["tr", "translate"], "https://translate.google.com/?sl=auto&tl=de&text={q}"),
    (&["x", "tw", "twitter"], "https://x.com/search?q={q}"),
    (&["ebay"], "https://www.ebay.de/sch/i.html?_nkw={q}"),
    (&["idealo"], "https://www.idealo.de/preisvergleich/MainSearchProductCategory.html?q={q}"),
    (&["db", "bahn"], "https://www.bahn.de/buchung/fahrplan/suche#sts=true&so={q}"),
];

/// Die gewählte Suchmaschine; ohne Wahl DuckDuckGo.
pub fn engine(id: Option<&str>) -> &'static SearchEngine {
    id.and_then(|id| ENGINES.iter().find(|e| e.id == id)).unwrap_or(&ENGINES[0])
}

fn fill(template: &str, query: &str) -> Option<Url> {
    if query.trim().is_empty() {
        // Nur das Kürzel: zur Startseite der Seite.
        let u = Url::parse(&template.replace("{q}", "")).ok()?;
        return Url::parse(&format!("{}://{}/", u.scheme(), u.host_str()?)).ok();
    }
    let encoded: String = url::form_urlencoded::byte_serialize(query.trim().as_bytes()).collect();
    Url::parse(&template.replace("{q}", &encoded)).ok()
}

/// Findet ein !Bang am Anfang oder Ende: liefert Kürzel und die restlichen Suchbegriffe.
fn split_bang(query: &str) -> Option<(String, String)> {
    let words: Vec<&str> = query.split_whitespace().collect();
    let pos = words.iter().position(|w| w.len() > 1 && w.starts_with('!'))?;
    if pos != 0 && pos != words.len() - 1 {
        return None;
    }
    let bang = words[pos][1..].to_lowercase();
    let rest: Vec<&str> = words.iter().enumerate().filter(|(i, _)| *i != pos).map(|(_, w)| *w).collect();
    Some((bang, rest.join(" ")))
}

/// Adresse für eine Suche mit der gewählten Suchmaschine, inklusive !Bangs.
pub fn search_url(query: &str, engine_id: Option<&str>) -> Option<Url> {
    if let Some((bang, rest)) = split_bang(query) {
        if let Some((_, template)) = BANGS.iter().find(|(names, _)| names.contains(&bang.as_str())) {
            return fill(template, &rest);
        }
        // Unbekanntes Kürzel: DuckDuckGo kennt tausende weitere.
        return fill("https://duckduckgo.com/?q={q}", query);
    }
    fill(engine(engine_id).url, query)
}

/// Macht aus einer Eingabe in der Adressleiste eine URL: Adresse oder Suche.
pub fn resolve_input(input: &str, engine_id: Option<&str>) -> Option<Url> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let looks_like_url = !input.contains(' ')
        && (input.contains("://")
            || input.starts_with("localhost")
            || input.starts_with("about:")
            || input.starts_with("vyre:")
            || (input.contains('.') && !input.starts_with('!') && !input.ends_with('.')));
    if looks_like_url {
        if let Ok(u) = Url::parse(input) {
            let special = matches!(u.scheme(), "about" | "file" | "vyre");
            if u.scheme().len() > 1 && (u.has_host() || special) {
                return Some(u);
            }
        }
        // Lokale Entwicklungsserver laufen meist ohne HTTPS.
        let scheme = if input.starts_with("localhost") || input.starts_with("127.0.0.1") { "http" } else { "https" };
        if let Ok(u) = Url::parse(&format!("{scheme}://{input}")) {
            return Some(u);
        }
    }
    search_url(input, engine_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eingaben_werden_zu_urls() {
        let ddg = None;
        assert_eq!(resolve_input("servo.org", ddg).unwrap().as_str(), "https://servo.org/");
        assert_eq!(resolve_input("https://a.b/c", ddg).unwrap().as_str(), "https://a.b/c");
        assert_eq!(resolve_input("localhost:8080", ddg).unwrap().as_str(), "http://localhost:8080/");
        assert_eq!(resolve_input("vyre://neuer-tab", ddg).unwrap().as_str(), "vyre://neuer-tab");
        assert_eq!(resolve_input("rust lernen", ddg).unwrap().as_str(), "https://duckduckgo.com/?q=rust+lernen");
        assert_eq!(
            resolve_input("rust lernen", Some("brave")).unwrap().as_str(),
            "https://search.brave.com/search?q=rust+lernen"
        );
        assert!(resolve_input("   ", ddg).is_none());
    }

    #[test]
    fn bangs_lokal() {
        let yt = "https://www.youtube.com/results?search_query=katzen+video";
        assert_eq!(search_url("!yt katzen video", None).unwrap().as_str(), yt);
        assert_eq!(search_url("katzen video !yt", None).unwrap().as_str(), yt);
        assert_eq!(search_url("!YT katzen video", None).unwrap().as_str(), yt);
        assert_eq!(search_url("!gh", None).unwrap().as_str(), "https://github.com/");
        // Unbekannt: an DuckDuckGo weiterreichen.
        assert_eq!(search_url("!xyz test", None).unwrap().as_str(), "https://duckduckgo.com/?q=%21xyz+test");
        // Ausrufezeichen mitten im Text ist kein Bang.
        assert_eq!(
            search_url("hallo !yt welt", Some("google")).unwrap().as_str(),
            "https://www.google.com/search?q=hallo+%21yt+welt"
        );
    }
}
