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

## Aufbau

- `src/main.rs`: Fenster, Ereignisse, Fokus, Tastenkürzel, Ein-/Ausfahren der Seitenleiste
- `src/ui.rs`: Layout, Seitenleiste mit Tab-Liste, Adressleiste, Animationen, FPS-Zähler
- `src/engine.rs`: Engine-Schnittstelle `Engine` und die Servo-Anbindung (ein Kind-Fenster für alle Tabs)
- `src/settings.rs`: gespeicherte Einstellungen
- `src/render/`: GPU-Zeichnen (aus Wyven übernommen)
- `src/keys.rs`, `src/win.rs`: Tastenübersetzung, Windows-11-Fenstereffekte
