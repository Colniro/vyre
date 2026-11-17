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

## Seitenleiste

Zwei Modi, umschaltbar mit dem Knopf oben in der Leiste oder Strg+B:

- **fest**: immer sichtbar, die Seite steht daneben.
- **hover**: eingeklappt; fährt aus, wenn die Maus an den linken Fensterrand kommt, und nach
  kurzer Zeit wieder ein. Die Seite wird dabei nur verschoben, nicht neu umbrochen.

Die Wahl steht in `%LOCALAPPDATA%\Vyre\einstellungen.txt`. Cookies und Seitenspeicher liegen in
`%LOCALAPPDATA%\Vyre\Profil`.

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
- `src/blocker.rs`: Werbeblocker (Filterlisten laden, Anfragen prüfen, Ausblend-Regeln)
- `src/settings.rs`: gespeicherte Einstellungen
- `src/render/`: GPU-Zeichnen (aus Wyven übernommen)
- `src/keys.rs`, `src/win.rs`: Tastenübersetzung, Windows-11-Fenstereffekte
