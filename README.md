# Vyre

Ein schlanker Browser in Rust. Webseiten stellt [Servo](https://servo.org) dar, die Oberfläche
wird mit wgpu selbst gezeichnet (Seitenleiste wie bei Zen, Funktionen nach dem Vorbild von Helium).

## Starten

```
cargo run --profile fast -- [adresse oder suchbegriff]
```

Der erste Build dauert lange, weil Servo mitgebaut wird. SpiderMonkey wird fertig heruntergeladen.

## Tasten

| Taste | Wirkung |
|---|---|
| Strg+L, F6 | Adressleiste |
| F5, Strg+R | Neu laden |
| Alt+← / Alt+→ | Zurück / Vor |
| F2 | Dauerlauf: misst die höchste Bildrate der Oberfläche |
| F3 | FPS-Anzeige ein/aus |
| Strg+T | Neuer Tab |
| Strg+W, Mittelklick auf Tab | Tab schließen (der letzte schließt Vyre) |
| Strg+Tab / Strg+Umschalt+Tab | Nächster / vorheriger Tab |
| Strg+1 … 8, Strg+9 | Zu Tab 1 … 8, zum letzten Tab |
| Strg+B | Seitenleiste: fest ↔ beim Hovern ausfahren |
| Strg+D | Seite als Favorit anpinnen / lösen |
| Strg+S | Aktuelle Seite/Datei herunterladen |
| Strg+Umschalt+E | Engine des Tabs wechseln (Servo ↔ WebView2) |
| Strg+Umschalt+Z | Aktiven Tab schlafen legen |
| Strg+H, Strg+Umschalt+Entf | Verlauf |
| ↑ / ↓ in der Adressleiste | Vorschlag wählen |

## Seitenleiste

Zwei Modi, umschaltbar mit dem Knopf oben in der Leiste oder Strg+B:

- **fest**: immer sichtbar, die Seite steht daneben.
- **hover**: eingeklappt; fährt aus, wenn die Maus an den linken Fensterrand kommt, und nach
  kurzer Zeit wieder ein. Die Seite wird dabei nur verschoben, nicht neu umbrochen.

Die Wahl steht in `%LOCALAPPDATA%\Vyre\einstellungen.txt`. Cookies und Seitenspeicher liegen in
`%LOCALAPPDATA%\Vyre\Profil`.

## Neuer Tab und Suche

Neue Tabs (und der Start ohne Adresse) zeigen `vyre://neuer-tab`: ein Suchfeld und Kacheln
für den Schnellzugriff. Kacheln lassen sich auf der Seite hinzufügen (`+`) und entfernen (`×`
beim Hovern). Statt Favicons aus dem Netz zeigen sie einen farbigen Anfangsbuchstaben, damit die
Seite ohne eine einzige Verbindung auskommt.

Beim ersten Start fragt die Seite nach der Suchmaschine (DuckDuckGo, Brave Search, Startpage,
Ecosia, Google). Ändern geht jederzeit über „ändern“ unter dem Suchfeld.

**!Bangs** werden lokal aufgelöst, am Anfang oder Ende der Suche: `!yt`, `!w`/`!wde`
(Wikipedia), `!gh`, `!g`, `!gi`, `!a` (Amazon), `!m` (OpenStreetMap), `!gm`, `!r`, `!so`,
`!mdn`, `!rs` (docs.rs), `!crates`, `!rust`, `!npm`, `!py`, `!imdb`, `!leo`, `!dl` (DeepL),
`!tr`, `!x`, `!ebay`, `!idealo`, `!db` und die Suchmaschinen selbst (`!ddg`, `!br`, `!sp`,
`!e`, `!b`). Ein Bang ohne Suchbegriff öffnet die Startseite. Unbekannte Bangs gehen an
DuckDuckGo, das tausende weitere kennt.

## Geteilte Ansicht (Split View)

- **Teilen**: einen Tab aus der Seitenleiste auf die linke oder rechte Hälfte der Seite ziehen.
  Beim Ziehen rückt die Seite zur Seite und zeigt, wo der Tab landet.
- **Trennlinie** zwischen den Hälften mit der Maus verschieben (20–80 %).
- In der Seitenleiste werden geteilte Tabs **ein verbundener Eintrag** mit beiden Titeln.
  Ein Klick in eine Hälfte (Seite oder Eintrag) macht sie zum aktiven Tab; Adressleiste,
  Zurück und Tastenkürzel gelten für diese Hälfte.
- **Lösen**: eine Hälfte (ihren Titel im verbundenen Eintrag) zurück in die Seitenleiste
  ziehen. Schließt man eine Hälfte, wird die andere wieder ein normaler Tab.
- Beliebig viele geteilte Ansichten gleichzeitig; zieht man einen Tab auf eine geteilte
  Ansicht, ersetzt er die Hälfte auf dieser Seite.
- Sind beide Hälften Servo-Seiten, lädt eine davon beim Teilen einmal neu (Servo zeichnet
  sie in ein zweites Fenster). WebView2-Seiten laden nicht neu.

## Schlafende Tabs

Tabs, die 10 Minuten nicht zu sehen waren, schlafen ein: ihre Seite wird aus dem Speicher
genommen, Adresse und Titel bleiben in der Seitenleiste (blass). Ein Klick weckt den Tab, die
Seite lädt neu. Strg+Umschalt+Z legt den aktiven Tab sofort schlafen.

Wach bleiben Tabs, die Ton oder Video abspielen, Tabs mit ungesendeten Formulareingaben und
die gerade sichtbare geteilte Ansicht. Geprüft wird alle 30 Sekunden.

## Verlauf, Favoriten, Sitzung

- **Vorschläge** beim Tippen in der Adressleiste: zuerst das, was Enter tut (Suche, !Bang
  oder Adresse), dann passende Favoriten und Verlauf. Oft und kürzlich Besuchtes steht oben.
  Pfeiltasten wählen, Enter oder Klick öffnet.
- **Favoriten** (wie Zens „Essentials“): der Stern in der Adressleiste oder Strg+D pinnt die
  Seite als farbiges Quadrat oben in die Seitenleiste. Ein Klick wechselt zum Tab mit dieser
  Seite oder öffnet sie neu, ein Mittelklick löst den Favoriten.
- **Verlauf** unter `vyre://verlauf` (Strg+H, Strg+Umschalt+Entf oder Link auf der
  Neuer-Tab-Seite): durchsuchbar, Löschen pro Eintrag, für die letzte Stunde, heute, diese
  Woche oder alles. Gelöschtes wird aus der Datei wirklich entfernt (`VACUUM`).
- **Sitzung**: Die offenen Tabs werden bei jeder Änderung gespeichert und beim nächsten Start
  wiederhergestellt (nicht, wenn Vyre mit einer Adresse gestartet wird).

Gespeichert in `%LOCALAPPDATA%\Vyre\verlauf.sqlite` und `sitzung.txt`. Nur `http`- und
`https`-Seiten kommen in den Verlauf.

## WebView2 als Ersatz-Engine

Seiten, die Servo noch nicht schafft, laufen still mit WebView2 (der Chromium-Engine von
Microsoft Edge, die in Windows 11 steckt), im selben Tab und Fenster. Man merkt davon nichts.

- **Startliste**: YouTube, Discord, Google Docs/Drive/Mail/Meet/Kalender, Spotify, WhatsApp Web,
  Telegram, Teams, Outlook, Office, Figma, Notion, Twitch, Canva öffnen sofort in WebView2.
- **Erkennung**: 2,5 s nach dem Laden prüft Vyre Servo-Seiten. Meldet die Seite „Browser nicht
  unterstützt“, bleibt sie nach JavaScript-Fehlern praktisch leer, oder stürzt sie ab, wechselt
  der Tab zu WebView2. Gelernte Seiten gelten bis zum nächsten Vyre-Update, dann wird wieder
  zuerst Servo versucht.
- **Notausgang**: Strg+Umschalt+E wechselt die Engine des Tabs von Hand und merkt sich das
  dauerhaft für die Seite.
- **Logins**: Beim Wechsel nimmt Vyre die Cookies der Seite aus Servo mit.
- **Abgeschottet**: eigenes Profil (`%LOCALAPPDATA%\Vyre\WebView2`), keine Crash-Berichte an
  Microsoft, SmartScreen, Sync, Hintergrund-Verbindungen, Autofill und Passwort-Speicher aus.
  Was WebView2 darüber hinaus selbst sendet, lässt sich von außen nicht garantiert abstellen.
- **Werbeblocker** und Tastenkürzel funktionieren auch in WebView2-Tabs.
- Die Laufzeit startet erst, wenn ein Tab sie braucht.

Die gelernten und festgelegten Seiten stehen in `%LOCALAPPDATA%\Vyre\engine-seiten.txt`.

## Passwörter

- **Tresor** in `%LOCALAPPDATA%\Vyre\passwoerter.sqlite`: jedes Passwort einzeln mit Windows
  DPAPI verschlüsselt, also an dein Windows-Konto gebunden.
- **Ausfüllen**: Klick in ein Benutzer- oder Passwortfeld zeigt die Konten für genau diese Seite;
  ein Klick füllt aus. Die Seite bekommt erst dann das Passwort – und nur ihr eigenes.
- **Speichern**: Nach dem Anmelden fragt eine Karte oben in der Seitenleiste „Passwort
  speichern?“ (Speichern / Nie hier).
- **Verwaltung** unter `vyre://passwoerter` (Link auf der Neuer-Tab-Seite): suchen, anzeigen,
  löschen (zweimal klicken).
- **Import** von Passwörtern, Lesezeichen (werden Favoriten) und Verlauf aus Chrome, Edge,
  Brave, Firefox und Zen, oder Passwörter aus einer CSV-Datei (Export aus jedem Browser,
  Bitwarden, 1Password, KeePass …). Neuere Chrome-Versionen schützen manche Passwörter
  zusätzlich so, dass nur Chrome selbst sie lesen kann – die meldet der Import, dafür gibt es
  den CSV-Weg. Firefox/Zen mit Hauptpasswort: ebenfalls über CSV.

## Downloads

- Dateien landen ohne Nachfrage im Windows-Downloads-Ordner; gleiche Namen bekommen „(1)“ …
- **WebView2-Tabs** nutzen die eingebauten Downloads (Edges eigene Leiste ist ausgeblendet).
- **Servo-Tabs** kennen keine Downloads; Vyre lädt selbst herunter – mit den Cookies der
  Seite, damit Downloads nach dem Anmelden klappen. Erkannt werden Dateien an der Endung
  (`.zip`, `.exe`, `.pdf`, Videos …) und an `<a download>`-Links. **Strg+S** speichert, was im
  Tab offen ist.
- Laufende Downloads unten in der Seitenleiste mit Fortschrittsbalken; alle auf
  `vyre://downloads` (pausieren, fortsetzen, abbrechen, öffnen, im Ordner zeigen).
  Programme (`.exe`, `.msi` …) starten erst nach einer zweiten Bestätigung.

## Datenschutz

- **Nur HTTPS**: Unverschlüsselte Adressen werden auf HTTPS umgeleitet (außer lokale wie
  `localhost`, `192.168.…` oder Namen ohne Punkt). Kann eine Seite kein HTTPS, zeigt Vyre eine
  Warnseite; „Trotzdem öffnen“ merkt sich die Ausnahme (`http_erlaubt` in
  `einstellungen.txt`). Unverschlüsselte Seiten zeigen in der Adressleiste ein rotes Warnsymbol.
- **Drittanbieter-Cookies**: Alle 5 Minuten löscht Vyre Cookies von Seiten, die du nie selbst
  besucht hast (Tracker). WebView2 läuft zusätzlich mit seinem strengen Tracking-Schutz.
- **Fingerprinting**: Ein Skript vor jeder Seite liefert einheitliche Werte (4 Kerne, 8 GB,
  übliche Bildschirmgröße) und verrauscht das Auslesen von Canvas und Audio minimal.
- **Kennung**: Vyre meldet sich als Firefox ESR statt als (seltenes, gut wiedererkennbares) Servo.

## Werbeblocker

Eingebaut mit Braves [adblock-rust](https://github.com/brave/adblock-rust) und denselben
Filterlisten wie uBlock Origin: EasyList, EasyPrivacy, EasyList Germany, die uBO-Listen
(Filter, Privatsphäre, Unbreak, Cookie-Hinweise), Fanboys Cookie-Liste und Peter Lowes Liste.

- **Netzwerk:** Werbung und Tracker werden gar nicht erst geladen.
- **Ausblenden:** seitenspezifische Regeln beim Laden des `<head>`, allgemeine Regeln (nach
  Klassen und IDs, z. B. Cookie-Banner) wenn die Seite fertig ist.
- **Schild in der Adressleiste:** zeigt, wie viel auf der Seite blockiert wurde. Ein Klick
  schaltet den Blocker für diese Seite aus oder wieder ein (gespeichert in
  `einstellungen.txt` unter `werbeblocker_aus`).

Die Listen liegen in `%LOCALAPPDATA%\Vyre\Filter` und werden höchstens einmal am Tag neu
geladen. Das sind die einzigen Verbindungen, die Vyre von sich aus aufbaut.

## Aufbau

- `src/main.rs`: Fenster, Ereignisse, Fokus, Tastenkürzel, Ein-/Ausfahren der Seitenleiste
- `src/ui.rs`: Layout, Seitenleiste mit Tab-Liste, Adressleiste, Animationen, FPS-Zähler
- `src/engine.rs`: Engine-Schnittstelle `Engine` und die Servo-Anbindung (ein Kind-Fenster für alle Tabs)
- `src/newtab.rs`: das `vyre://`-Protokoll (Neuer-Tab-Seite, Suche, Aktionen)
- `src/search.rs`: Suchmaschinen und lokale !Bangs
- `src/blocker.rs`: Werbeblocker (Filterlisten laden, Anfragen prüfen, Ausblend-Regeln)
- `src/history.rs`: Verlauf und Favoriten (SQLite), Vorschläge
- `src/session.rs`: offene Tabs speichern und wiederherstellen
- `src/edge.rs`: WebView2-Tabs (Abschottung, Werbeblocker, Cookies, Kürzel)
- `src/sites.rs`: welche Seite mit welcher Engine läuft
- `src/privacy.rs`: Nur-HTTPS, Cookie-Aufräumen, Fingerprinting-Schutz, Kennung
- `src/vault.rs`, `src/pwfill.rs`: Passwort-Tresor (DPAPI) und Ausfüllen in Seiten
- `src/downloads.rs`: Downloads (eigener Downloader, WebView2-Anbindung, Übersicht)
- `src/importer.rs`: Import aus Chrome/Edge/Brave, Firefox/Zen und CSV
- `src/settings.rs`: gespeicherte Einstellungen
- `src/render/`: GPU-Zeichnen (aus Wyven übernommen)
- `src/keys.rs`, `src/win.rs`: Tastenübersetzung, Windows-11-Fenstereffekte
