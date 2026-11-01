# booxmtp

[English](README.md) · **Deutsch**

[![CI](https://github.com/Harleaquin/booxmtp/actions/workflows/ci.yml/badge.svg)](https://github.com/Harleaquin/booxmtp/actions/workflows/ci.yml)

Dateitransfer zwischen macOS und Android-basierten MTP-E-Readern — entwickelt und getestet mit
einem **BOOX Go Color 7**. Reines Rust, ohne FUSE-Mount, ohne Android File Transfer, ohne
`libmtp`/FFI.

```console
$ booxmtp storages
    ID  ALIAS     DESCRIPTION                        FREE  CAPACITY  WRITABLE
 65537  internal  Interner gemeinsamer Speicher  40.8 GiB  52.0 GiB  yes
131073  sd        android                        39.8 GiB  59.5 GiB  yes

$ booxmtp ls --storage internal /Books
TYPE       SIZE  MODIFIED             NAME
dir           -  2026-11-01 14:09:36  ...
file  982.5 KiB  2026-11-01 14:09:40  metadata.calibre

$ booxmtp push --storage sd ./Roman.epub /Books/
uploaded ./Roman.epub -> /Books/Roman.epub (1.2 MiB, storage “android”)

$ booxmtp pull --storage sd /Books/Roman.epub ./Roman.epub
downloaded /Books/Roman.epub -> ./Roman.epub (1.2 MiB, storage “android”)
```

## Status

Der MVP aus `PLAN.md` ist umgesetzt und auf echter Hardware verifiziert:

* Geräteerkennung, Storage-Liste mit IDs und Aliasen (`internal`, `sd`)
* `ls` (auch verschachtelt), `pull`, `push`, `mkdir`
* Streaming in beide Richtungen (kein vollständiges Einlesen in den RAM)
* Push/Pull-Roundtrip bytegleich (SHA-256 auf BOOX Go Color 7, macOS, Apple Silicon)
* Fehlerfälle: fehlender Pfad, Kollision, fehlende SD-Karte, ungültige Storage-ID, Abbruch
* **TUI im Midnight-Commander-Stil** (`booxmtp tui`): zwei Panels (lokal ↔ Reader), Navigation,
  Mehrfachauswahl, Kopieren, **Verschieben**, **Löschen mit Rückfrage**, rekursive Ordner,
  Fortschritt/Abbrechen, Storage-Wechsel, Hilfebildschirm
* **Mount (macOS/Linux)** über `booxmtp mount` – ein Wrapper um den erprobten `mtp-mount`-Helper
  (Details und Voraussetzungen unten)

Noch nicht enthalten (bewusst, siehe „Nicht-Ziele“): Synchronisation, `--json`, GUI, Löschen/
Verschieben im CLI (bisher nur in der TUI), Mount unter Windows.

## Voraussetzungen

* Rust **1.91+** (Edition 2024)
* macOS (getestet) oder Linux; Windows ist nicht Teil des MVP
* Reader per USB verbunden, **entsperrt**, USB-Modus **Dateiübertragung / MTP**
* Keine andere MTP-Anwendung, die das USB-Interface belegt

### macOS-Fallstricke

Apple beansprucht MTP/PTP-Geräte gern über `ptpcamerad`, und „Android File Transfer“ hängt sich
ebenfalls dazwischen. Beides blockiert den Zugriff:

```bash
# Android File Transfer beenden (falls installiert)
osascript -e 'quit app "Android File Transfer"' 2>/dev/null

# Einmalig: ptpcamerad stoppen (greift sofort)
sudo killall ptpcamerad 2>/dev/null

# Dauerhaft (Photos.app-Import funktioniert danach ggf. nicht mehr):
# sudo launchctl disable system/com.apple.ptpcamerad
```

`booxmtp` erkennt diesen Fall (`ExclusiveAccess`) und gibt einen entsprechenden Hinweis aus.

## Installation

```bash
git clone https://github.com/Harleaquin/booxmtp.git && cd booxmtp
cargo build --release
# Binary: target/release/booxmtp
# oder systemweit:
cargo install --path .
```

### Fertige Binaries

Jeder getaggte Release liegt auf der
[Releases-Seite](https://github.com/Harleaquin/booxmtp/releases):

| Plattform | Archiv |
|---|---|
| Linux (x86_64) | `booxmtp-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Apple Silicon + Intel) | `booxmtp-<version>-universal-apple-darwin.tar.gz` |

```bash
# nach dem Download von Archiv und SHA256SUMS
sha256sum -c SHA256SUMS          # macOS: shasum -a 256 -c SHA256SUMS
tar -xzf booxmtp-*.tar.gz
./booxmtp --help
```

Unter macOS ggf. das Quarantäne-Flag entfernen: `xattr -d com.apple.quarantine booxmtp`.
Die Binaries sind eigenständig; nur das optionale `booxmtp mount` braucht zusätzlich
`cargo install mtp-mount` und einen FUSE-Treiber.

## Befehle

```text
booxmtp [--device <SERIAL>] [-v] [--no-progress] <COMMAND>

  devices                          Verfügbare MTP-Geräte anzeigen
  storages                         Speicherbereiche mit ID, Alias, Größe, freiem Platz
  ls       -s <STORAGE> [PATH]     Ordnerinhalt auflisten (Standard: /)
  pull     -s <STORAGE> <REMOTE> <LOKAL>
                                   Datei herunterladen
  push     -s <STORAGE> <LOKAL> <REMOTE>
                                   Datei hochladen
  mkdir    -s <STORAGE> <REMOTE>   Ordner anlegen
  tui      [-s <STORAGE>]          Interaktiver Zwei-Panel-Dateimanager
  mount    [-s <STORAGE>] [OPTIONEN] <MOUNTPOINT>
                                   Reader als Dateisystem einhängen (macOS/Linux)
```

Globale Optionen:

| Option | Bedeutung |
|---|---|
| `--device <SERIAL>` | Gerät explizit wählen (nötig, wenn mehrere verbunden sind) |
| `-v`, `-vv` | Diagnose nach `stderr` (`debug`/`trace`); `RUST_LOG` überschreibt |
| `--no-progress` | Keine Fortschrittszeile (wird ohnehin nur am Terminal gezeichnet) |

Beispiele:

```bash
booxmtp devices
booxmtp storages
booxmtp ls --storage internal /Books
booxmtp ls --storage sd "/Books/Sci-Fi"
booxmtp pull --storage internal "/Books/Beispiel.epub" ./Beispiel.epub
booxmtp push --storage sd ./Roman.epub /Books/            # Zielordner: Name wird angehängt
booxmtp push --storage 65537 ./Notizen.pdf /Documents/Notizen.pdf
booxmtp mkdir --storage sd /Books/ToRead
booxmtp tui                       # Dateimanager: links lokal, rechts internal
booxmtp tui --storage sd          # rechte Seite startet auf der SD-Karte
booxmtp mount --storage internal ~/mnt/reader   # als Dateisystem einhängen (read-only)
```

### Storage-Auswahl

| Wert | Bedeutung |
|---|---|
| `internal` | Alias für den internen Speicher |
| `sd` | Alias für eine wechselbare Karte |
| `65537` | numerische MTP-Storage-ID — funktioniert **immer** |

Die IDs sind sitzungsbezogen und werden bei jedem Lauf neu gelesen. Aliase werden **nur** vergeben,
wenn die vom Gerät gemeldeten Metadaten genau einen Kandidaten ergeben:

1. **Starkes Signal:** der MTP-Storage-Typ (`FixedRam`/`FixedRom` vs. `RemovableRam`/`RemovableRom`).
2. **Schwaches Signal:** Stichworte in der Beschreibung („internal“, „SD card“, „external“, …) —
   nur wenn kein Gerät einen Storage-Typ meldet.

Bei Mehrdeutigkeit bricht `booxmtp` mit einer Liste der Kandidaten ab und empfiehlt eine explizite
ID. Es wird nie „erster Storage = intern“ angenommen. Das war auf dem Testgerät wichtig: die
SD-Karte heißt dort schlicht `android` und wird allein über den `removable`-Typ erkannt.

### Pfadregeln

Remote-Pfade sind **MTP-Objektpfade relativ zum Storage-Root**, keine POSIX-Pfade:

* `/` ist der Root des gewählten Speichers; führende `/` sind optional.
* `//`, `./` werden normalisiert.
* `..` wird abgelehnt — ein Remote-Pfad kann den Speicher nicht verlassen.
* Namen werden exakt verglichen; ist der Treffer nur über Groß-/Kleinschreibung eindeutig, wird
  dieser akzeptiert (MTP-Speicher sind häufig case-insensitiv, aber case-preserving).
* Bei `push` bedeutet ein Ziel mit abschließendem `/` (oder `/` allein): „in diesen Ordner legen,
  Dateiname von der Quelle übernehmen“.
* Fehlende Zielordner werden nicht automatisch angelegt (`mkdir` ist ein eigener Befehl).

### Sicherheitsverhalten

* **Kein Überschreiben ohne `--overwrite`** — weder lokal noch remote.
* Downloads schreiben zuerst nach `<ziel>.part`, prüfen die Bytezahl gegen die Geräteangabe und
  benennen erst dann atomar um; bei Fehlern wird die `.part`-Datei entfernt.
* Nach einem Upload wird der Zielordner erneut gelistet und die Größe verglichen; weicht sie ab,
  erscheint eine Warnung (der Upload gilt trotzdem als erfolgreich, wenn das Gerät ihn quittiert).
* Ein abgebrochener Upload wird als solcher gemeldet. Hat das Gerät das halb geschriebene Objekt
  mit einem Handle gemeldet, wird genau dieses Objekt wieder entfernt — keine Raterei über Namen.
* `--overwrite` bei `push` bedeutet: vorhandenes Objekt löschen, dann neu hochladen. MTP kann nicht
  atomar ersetzen; schlägt der Upload dazwischen fehl, ist das Ziel weg (dokumentiert).
* `stdout` bleibt maschinenlesbar (Tabellen für `devices`, `storages`, `ls`), Statusmeldungen,
  Fortschritt und Warnungen gehen nach `stderr`.

## Interaktive TUI (Midnight-Commander-Stil)

```text
booxmtp tui [--device <SERIAL>] [-s <STORAGE>]
```

Eine echte Aufnahme (100 % Terminal, keine Mockups):

```text
 ONYX KHAJE-IDP _SN:72E08D03 · serial 6DE76AE3 · 480 Mbit/s (USB 2.0) internal: 40.8 GiB free  ·  sd: 39.8 GiB free
┌ Local: ~/Code/BooxMTP ───────────────────────────────────┐┌ sd:/Books ─────────────────────────────────── [2 marked] ┐
│  ..                                                      ││  ..                                                      │
│  .git/                                                   ││  One Piece (Digital) (1r0n)/                             │
│  .github/                                                ││✓ Accursed Race, An - Elizabeth Gaskell.epub     149.6 KiB│
│  scripts/                                                ││✓ Algebraist, Der - Iain Banks.mobi              962.8 KiB│
│  src/                                                    ││  American Gods_ Roman (German Edition) - Neil G 665.6 KiB│
│  target/                                                 ││  Atta Troll_ Erweiterte Ausgabe (German Edition 159.9 KiB│
│  tests/                                                  ││  Bedenke Phlebas - Iain Banks.mobi              810.2 KiB│
│  .gitignore                                     28 B     ││  besten Erzahlungen des H.P. Lovecraft_ Berge d 988.6 KiB│
│  Cargo.lock                                     63.5 KiB ││  Blicke windwaerts - Iain Banks.mobi            636.8 KiB│
│  Cargo.toml                                     714 B    ││  Caesar's Messiah_ The Roman Conspiracy to Inve 1.5 MiB  │
│  PLAN.md                                        14.9 KiB ││  Call of Cthulhu_ 67 Tales of Horror by H.P. Lo 1.8 MiB  │
│  README.md                                      22.8 KiB ││  Can Such Things Be_ - Ambrose Bierce.epub      255.8 KiB│
└──────────────────────────────────────────────────────────┘└──────────────────────────────────────────────────────────┘
 American Gods_ Roman (German Edition) - Neil Gaiman.epub   665.6 KiB   2024-12-13 19:56:10   [39.8 GiB free]
 2 marked — Ctrl-C copies them
Space Mark  Ctrl-A All  Ctrl-C Copy  Ctrl-X Move  Ctrl-D Del  Ctrl-N New  Ctrl-Q Quit  ? Help
```

### Tasten

| Taste | Wirkung |
|---|---|
| `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End` | Cursor bewegen |
| `Enter`, `→` | Ordner öffnen |
| `Backspace`, `←` | eine Ebene nach oben |
| `Tab` | aktives Panel wechseln |
| `Ctrl-U` | Panels tauschen (Positionen inklusive) |
| `Ctrl-R` | beide Panels und die Speicher-Metadaten neu laden |
| `Ctrl-S` | Speicher des aktiven Remote-Panels wählen |
| `Space` | Eintrag markieren/entmarkieren, Cursor rückt weiter |
| `Ctrl-A` | alles im Ordner markieren (nochmal: alle Markierungen aufheben) |
| `Ctrl-C` | markierte Einträge kopieren, sonst den Eintrag unter dem Cursor |
| `Ctrl-X` | markierte Einträge verschieben (Move), sonst den Eintrag unter dem Cursor |
| `Ctrl-D` | markierte Einträge löschen (mit Rückfrage) |
| `Ctrl-N` | Ordner anlegen (Name eintippen, `Enter`, `Esc` bricht ab) |
| `?` | Hilfe |
| `Ctrl-Q`, `Esc`, `q` | beenden |

Es werden **keine Funktionstasten** verwendet: Terminals, tmux und Remote-Sessions fangen `F1`–`F12`
uneinheitlich ab. Alle Aktionen liegen auf `Ctrl`-Kombinationen; `Ctrl-C` ist hier **Copy**, nicht
Abbruch (im Transfer-Dialog bricht `Esc` ab). Einzige Ausnahme ohne `Ctrl` ist `?` für die Hilfe —
`Ctrl-H` wäre nicht von Backspace zu unterscheiden.

### Verhalten

* **Richtungen:** lokal → Reader (`push`), Reader → lokal (`pull`), lokal → lokal. Das linke Panel
  ist lokal, das rechte zeigt den Reader (Speicher per `Ctrl-S` wechselbar). Reader → Reader ist
  noch nicht unterstützt (MTP kann nicht streamend zwischen zwei Objekten kopieren) und wird mit
  Hinweis abgelehnt.
* **Mehrfachauswahl:** `Space` markiert einen Eintrag (Haken `✓` links, Zähler rechts im
  Panel-Titel) und rückt den Cursor weiter, `Ctrl-A` markiert alles im Ordner. Markierungen gelten
  pro Ordner: beim Wechsel in einen anderen Ordner werden sie geleert, bei `Ctrl-R` bleiben sie für
  noch vorhandene Einträge erhalten. Nach einem erfolgreichen Transfer verlieren die übertragenen
  Einträge ihre Markierung; übersprungene behalten sie (ein Tastendruck zum Wiederholen).
* **Kopieren (`Ctrl-C`) und Verschieben (`Ctrl-X`)** nutzen denselben Streaming-Kern wie
  `pull`/`push`. **Ordner werden rekursiv** kopiert bzw. verschoben (MTP kennt kein serverseitiges
  rekursives Kopieren, also läuft es Datei für Datei). Verschieben kopiert zuerst vollständig und
  löscht die Quelle erst danach; bei lokal → lokal nutzt es `rename`, wenn das Ziel noch nicht
  existiert. Markierte Einträge werden nacheinander abgearbeitet (Fortschritt „2/5“, Statuszeile
  zeigt den aktuellen Pfad). Die Kollisionsprüfung passiert einmal vorab: Existieren Zieleinträge,
  fragt die TUI „Overwrite all“ bzw. bei einem einzelnen Lauf „Overwrite“. Taucht während des Laufs
  doch ein Konflikt auf (z. B. durch ein anderes Programm), fragt sie pro Eintrag mit
  „Overwrite“/„Skip“. Am Ende steht eine Zusammenfassung („Copied 3 of 5 entries (2 skipped)“).
* **Löschen (`Ctrl-D`)** verlangt immer eine Bestätigung („Delete?“/„Cancel“) und zeigt an, wenn
  Ordner samt Inhalt betroffen sind. Gelöscht wird rekursiv; danach werden die Markierungen der
  gelöschten Einträge entfernt und das Panel neu geladen. Ein Datei-über-Ordner-Konflikt wird nie
  stillschweigend „überschrieben“: einen Ordner durch eine Datei zu ersetzen ist bewusst nur über
  Löschen + Kopieren möglich.
* **Grenzen:** Reader → Reader bleibt unsupported (Hinweis: lokales Panel dazwischen). `Esc` bricht
  einen laufenden Transfer bzw. die restlichen Einträge eines Stapels ab.
* **Nichts blockiert die Oberfläche:** Geräte-I/O läuft in einem eigenen Thread mit eigener
  Tokio-Runtime; die UI verarbeitet währenddessen Tastendrücke und zeichnet Fortschritt.
* **Freier Platz** wird nach jedem Transfer aktualisiert (Header und Statuszeile).
* **Diagnose:** damit Log-Ausgaben nicht in die Vollbild-Oberfläche schreiben, gehen sie bei `-v`
  in eine Datei (`$TMPDIR/booxmtp-tui.log`); der Pfad steht auch im Hilfebildschirm (`?`).
* **Fehler** erscheinen als Dialog mit Hinweis; fatale Fehler (kein Gerät, falsche Seriennummer)
  beenden die TUI und werden wie beim CLI mit Exit-Code gemeldet.

## Mounten (macOS/Linux)

```bash
cargo install mtp-mount                    # erledigt das eigentliche Einhängen (FUSE)
cargo install --path .                     # booxmtp selbst braucht keine FUSE-Build-Abhängigkeit
mkdir -p ~/mnt/reader
booxmtp mount --storage internal ~/mnt/reader

ls  ~/mnt/reader/"Interner gemeinsamer Speicher"/Books
cp  ./Roman.epub ~/mnt/reader/"Interner gemeinsamer Speicher"/Books/   # nur mit --read-write
```

`booxmtp mount` ist bewusst ein **Wrapper um [`mtp-mount`](https://crates.io/crates/mtp-mount)** und
keine eigene FUSE-Implementierung: MTP braucht für ein Dateisystem lokales Spooling (die Größe ist
erst bei `close()` bekannt), eine eigene Inode-Tabelle gegen instabile Objekt-Handles, Caches und
Reconnect-Logik. `mtp-mount` (MIT/Apache, gleicher `mtp-rs`-Stack) löst das bereits in ~5.400 Zeilen
mit 128 Tests. Wir setzen nur unsere Geräte- und Storage-Auswahl davor – und bleiben damit frei von
FUSE-Build-Abhängigkeiten und Treiber-Code.

### Voraussetzungen

| Plattform | Was gebraucht wird |
|---|---|
| Linux | `fuse3` (z. B. `sudo apt install fuse3`), ggf. Mitgliedschaft in der Gruppe `fuse` |
| macOS | [macFUSE](https://macfuse.github.io/) inkl. Entwicklungsdateien (`fuse.pc`, `libfuse`) und `pkg-config` – der getestete Pfad; FUSE-T ist eine Alternative, siehe Teststatus |
| Windows | **nicht unterstützt** – der Explorer hängt MTP über WPD ohnehin nativ ein |

`mtp-mount` wird separat installiert (`cargo install mtp-mount`); `booxmtp` prüft das beim Start und
nennt den Installationsbefehl, wenn es fehlt. Ein anders gebauter Helper lässt sich über
`BOOXMTP_MOUNT_HELPER=/pfad/zu/mtp-mount` verwenden.

### Verhalten

* Der Mount zeigt **alle** Storages als Unterordner: `<MOUNTPOINT>/<Gerätebeschreibung>`.
  `--storage internal` prüft den Alias mit derselben Logik wie alle anderen Befehle und nennt den
  exakten Pfad (inkl. Hinweis, wenn keine SD-Karte erkannt wird).
* **Read-only ist der Default.** `--read-write` erlaubt Schreibzugriff; `mtp-mount` puffert
  Schreibvorgänge lokal und lädt sie beim Schließen hoch. Der Spool liegt standardmäßig im
  Cache-Verzeichnis und lässt sich mit `--spool-dir <PATH>` umlenken (Platz für die größte Datei
  einplanen).
* `--reconnect-timeout <SEKUNDEN>` hält den Mount bei einem Kabelwackler am Leben.
* `Ctrl-C` unmountet sauber; alternativ `umount <MOUNTPOINT>` (Linux) bzw.
  `diskutil unmount <MOUNTPOINT>` (macOS).
* Dateisystem-Semantik bleibt MTP: Rechte/Besitzer sind erfunden, es gibt keine Symlinks, xattrs
  oder harten Links; Finder/Spotlight erzeugen viele Zugriffe (der Helper cached); In-Place-
  Änderungen laufen über Download + Upload; das Gerät hat eine MTP-Session, Zugriffe werden
  serialisiert. Read-only ist deshalb der sichere Default.

### Ehrlicher Teststatus

Der Wrapper ist auf dem BOOX verifiziert bis zur Übergabe an den Helper: Gerätewahl, Storage-Alias,
Mountpoint-Prüfung, Fehlerpfade und Prozess-/Signal-Handling funktionieren. Das eigentliche
Einhängen konnte auf dem Entwicklungs-Mac nicht abgeschlossen werden – fuser 0.18 + FUSE-T 1.2.9
bricht mit `Short read of FUSE request header` ab, und macFUSE ist dort ohne Entwicklungsdateien
installiert (`pkg-config`/`libfuse` fehlen), sodass der Mount nicht gebaut werden konnte. Auf Linux
ist der von `mtp-mount` getestete Pfad. Wenn der Mount bei dir scheitert, sagt der Wrapper das mit
Hinweis auf FUSE-Treiber und Mountpoint.

## Exit-Codes

| Code | Bedeutung |
|---|---|
| 0 | Erfolg |
| 1 | Sonstiger Fehler |
| 2 | Aufruffehler (`clap`) |
| 3 | Kein MTP-Gerät gefunden |
| 4 | Geräteauswahl (mehrere Geräte / unbekannte Seriennummer) |
| 5 | Storage-Auswahl (unbekannt, mehrdeutig, schreibgeschützt) |
| 6 | Remote-Pfad (ungültig, fehlt, falscher Typ) |
| 7 | Lokale Datei (fehlt, Kollision, I/O) |
| 8 | Konflikt (existiert bereits, kein Platz) |
| 9 | Transfer-/Gerätefehler (USB, Protokoll, Disconnect) |
| 130 | Vom Benutzer abgebrochen (Ctrl-C) |

## Fehlersuche

| Symptom | Ursache / Lösung |
|---|---|
| `no MTP device found` | Kabel/USB-Modus/Entsperrung prüfen; auf macOS `ptpcamerad` und Android File Transfer beenden |
| `more than one MTP device is connected` | `--device <SERIAL>` verwenden (`booxmtp devices`) |
| `no storage on this device matches the alias sd` | Karte nicht eingelegt/gemountet, oder Gerät meldet sie nicht als `removable` — `booxmtp storages` zeigt die echten IDs |
| `the storage alias internal is ambiguous` | Gerät meldet mehrere gleichartige Speicher — explizite ID verwenden |
| `... does not exist on the device` | Pfad mit `booxmtp ls -s <ID> <ELTERNORDNER>` prüfen |
| Transfer bricht mit `Disconnected` ab | Gerät neu verbinden; `.part`-Reste werden entfernt |
| Alles hängt | `-v` zeigt die letzte Operation; USB-Hub/Port wechseln |

## Architektur

```text
src/
  main.rs          CLI-Einstieg: Parsing, Tracing, Exit-Code
  cli.rs           clap-Definitionen + Kommando-Dispatch + Ausgabe
  session.rs       UI-unabhängige Fassade: Gerät + Storages + Operationen
  device.rs        USB-Erkennung und strikte Geräteauswahl
  storage.rs       Storage-Metadaten, Alias-Auflösung, Selektoren
  location.rs      Panel-Orte (lokal oder Storage + Remote-Pfad), im Core für Copy/Move/Delete
  remote_path.rs   Pfadnormalisierung + segmentweises Auflösen (RemoteFs-Trait)
  tree.rs          rekursives Kopieren, Verschieben und Löschen (Host ↔ Reader)
  mount.rs         Wrapper: Geräte-/Storage-Auswahl + Start des `mtp-mount`-Helpers
  transfer.rs      Streaming-Download/-Upload, .part-Handling, Verifikation
  output.rs        Tabellen, Größen/Datum/Namen, Fortschrittszeile
  error.rs         ein Fehlertyp mit Hinweisen und stabilen Exit-Codes
  tui/
    mod.rs         Terminal-Lebenszyklus (Alt-Screen, Panic-Hook, Restore) + Event-Loop
    app.rs         reiner Zustandsautomat: Tasten/Events rein, Actions raus
    entry.rs       Panel-Orte (lokal/remote) und einheitliche Datei-Einträge
    worker.rs      Hintergrund-Thread mit Session, Request/Event-Kanälen, Cancel-Token
    ui.rs          Rendering mit ratatui (Panels, Dialoge, Fortschritt, Hilfe)
tests/
  remote_paths.rs        Pfadregeln gegen einen In-Memory-Baum
  storage_selection.rs   Alias-/ID-Auflösung inkl. Mehrdeutigkeit
  device_integration.rs  End-to-End gegen das virtuelle MTP-Gerät von mtp-rs
```

Leitlinien:

* Außer `cli`/`main` schreibt kein Modul nach stdout/stderr und beendet keinen Prozess.
* Operationen sind `async`-Methoden auf [`Session`](src/session.rs) und liefern reine Daten
  (`DirListing`, `DownloadReport`, `UploadReport`, …).
* Fortschritt kommt als Callback (`TransferProgress`), Abbruch über `mtp_rs::CancelToken`
  (Ctrl-C setzt ihn; Downloads prüfen zwischen Fenstern, Uploads je Chunk).
* Remote-Pfade werden segmentweise über den `RemoteFs`-Trait aufgelöst. Das hält die Logik ohne
  Hardware testbar und erlaubt später Preview-/Navigationsfunktionen in einer UI.

### TUI-Architektur

Die TUI ist bewusst in vier Schichten getrennt:

```text
 ratatui/crossterm            app.rs (pure state machine)          worker.rs (thread + runtime)
 ┌──────────────┐   Tasten   ┌──────────────────────────┐ Request ┌──────────────────────────┐
 │  ui.rs       │──────────▶ │ Panel/ Dialog/ Progress  │────────▶│ Session + tokio          │
 │  (Rendering) │◀────────── │  → Vec<Action>           │◀────────│  MTP-Operationen         │
 └──────────────┘   Screen   └──────────────────────────┘  Event  └──────────────────────────┘
                                      ▲                                    │
                                      └────────── CancelToken ◀────────────┘
```

* `app.rs` kennt weder Terminal noch Gerät: Tests füttern Tasten und Events und prüfen Zustand,
  Dialoge und die erzeugten Actions.
* `worker.rs` verarbeitet Requests strikt sequenziell (MTP hat eine Session). Fortschritt und
  Ergebnisse kommen als Events zurück; Abbruch läuft über ein geteiltes `CancelToken`, damit ein
  laufender Transfer nicht auf die nächste Nachricht warten muss.
* `ui.rs` zeichnet nur; es schreibt außerdem die sichtbare Zeilenzahl zurück, damit `PgUp`/`PgDn`
  bildschirmweise springen.
* `mod.rs` besitzt den Terminal-Lebenszyklus: Alt-Screen + Raw-Mode, Panic-Hook von `ratatui`, und
  ein Drop-Guard, der das Terminal auf jedem Rückweg wiederherstellt.

Tests: Unit-Tests in der Library (inkl. Zustandsautomat und Rendering über `TestBackend`),
Integrationstests für Pfadregeln und Storage-Auswahl, End-to-End-Tests gegen das virtuelle
MTP-Gerät (`device_integration.rs`) und für rekursive Copy/Move/Delete-Operationen
(`tree_ops.rs`), Unit-Tests für den Mount-Wrapper (Storage-Unterordner, Argumentabbildung,
Mountpoint-Prüfung) sowie ein PTY-Smoke-Test (`scripts/tui-smoke.py`, benötigt echte Hardware und
`pyte`). Letzterer markiert drei Einträge (zwei Dateien und einen Ordner), kopiert sie als Stapel,
löscht den Ordner rekursiv auf dem Gerät, lädt eine Datei mit Überschreib-Bestätigung zurück und
prüft die SHA-256-Summe. Der Mount selbst braucht einen FUSE-Treiber und `mtp-mount` und läuft
deshalb nicht in CI.

### Weg zur GUI

Eine GUI kann dieselbe Basis nutzen:

```rust
use booxmtp::{RemotePath, Session, TransferOptions};

let session = Session::connect(None).await?;
let storage = session.select_storage("internal")?;
let listing = session.list_dir(storage.id(), &RemotePath::parse("/Books")?, None).await?;

let mut on_progress = |p: booxmtp::TransferProgress| {
    // z. B. an einen Kanal für den Event-Loop schicken
    let _ = p.percent();
};
session
    .download_file(storage.id(), &RemotePath::parse("/Books/x.epub")?, "./x.epub".as_ref(),
                   &TransferOptions::default(), &mut on_progress)
    .await?;
```

* `Session` ist `Send` und hält keine UI-Zustände; Operationen können in einem Background-Task
  laufen, während die UI Events verarbeitet.
* `ctl`-Abbruch: `CancelToken::cancel()` aus dem Event-Loop; die Operation endet mit
  `Error::Cancelled` (Exit-Code 130).
* Für eine Geräte-/Storage-Auswahl im UI: `booxmtp::device::list_devices()`, `Session::attach()`,
  `session.catalog()` und `session.reload_storages()`.
* `booxmtp::output` liefert Formatierung für Bytes, Datum, Namen und Tabellen; eine GUI kann
  stattdessen eigene Widgets nutzen.

## Tests

```bash
cargo test                 # Unit-Tests + virtuelle MTP-Integrationstests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Die Integrationstests (`tests/device_integration.rs`) nutzen das `virtual-device`-Feature von
`mtp-rs`: ein temporäres Verzeichnis wird als MTP-Speicher exponiert, inklusive Upload, Download,
Kollisionen, `mkdir`, Unicode-Namen und Abbruch. Dadurch läuft die komplette Logik ohne Hardware.

Die TUI wird auf drei Ebenen geprüft:

1. **Zustandsautomat** (`src/tui/app.rs`): Navigation, Dialoge, Kopier-/Mkdir-Entscheidungen,
   Konflikte, Abbruch, veraltete Antworten.
2. **Rendering** (`src/tui/ui.rs`): `ratatui::TestBackend` rendert die Oberfläche in einen
   Zeichenpuffer; die Tests prüfen Panels, Kopfzeile, Hilfe und Tastenleiste.
3. **PTY-End-to-End** (`scripts/tui-smoke.py`, manuell): startet das echte Binary in einem
   Pseudo-Terminal, bedient es mit Tasten und prüft die Bildschirminhalte. Es lädt eine Datei hoch,
   lädt sie mit Überschreib-Bestätigung wieder herunter (SHA-256-Vergleich), legt einen Ordner an
   und beendet die TUI. Benötigt einen angeschlossenen Reader:

   ```bash
   python3 -m venv .venv && .venv/bin/pip install pyte
   .venv/bin/python scripts/tui-smoke.py --work /tmp/booxmtp-tui-smoke
   ```

Hardware-Checkliste (manuell, mit angeschlossenem Reader):

1. `booxmtp devices` und `booxmtp storages` — Gerät und alle Speicher sichtbar.
2. `ls -s internal /` und `ls -s sd /` — beide Speicher getrennt.
3. `push` einer ~2 MiB großen Zufallsdatei auf beide Speicher, danach `pull` und SHA-256 vergleichen.
4. Kollision ohne `--overwrite` (muss scheitern), mit `--overwrite` (muss gelingen).
5. Fehlender Pfad, fehlende SD-Karte (`--storage sd` bei eingelegter/entfernter Karte).
6. USB-Kabel während eines Transfers ziehen — verständliche Fehlermeldung, kein Panic.
7. TUI: Ordner mit Unterordnern per `Ctrl-C` kopieren, mit `Ctrl-X` verschieben, mit `Ctrl-D`
   (nach Rückfrage) löschen; Markierungen verschwinden nach dem Transfer.

## Bewusste Abweichungen vom Plan

* **Fehlertyp statt `anyhow`:** `error.rs` definiert einen domänenspezifischen `thiserror`-Enum.
  Nur so lassen sich Hinweise und stabile Exit-Codes zentral und UI-neutral ableiten.
* **`mkdir` ist bereits umgesetzt** (im Plan als optionales Phase-2-Element markiert), weil es für
  Upload-Tests und die Bedienung offensichtlich nützlich ist.
* **`--overwrite` für `pull` und `push`** ist implementiert, mit dokumentierter Semantik: lokal
  atomar per `rename`, remote per Löschen-und-neu-Hochladen (MTP kann nicht atomar ersetzen).
* **Kein `--json`:** die Datenmodelle (`DirListing`, `StorageEntry`, …) sind dafür vorbereitet, die
  Ausgabe ist aber noch rein menschenlesbar.
* **Löschen, Verschieben und rekursive Ordner gibt es bisher nur in der TUI.** Das CLI bleibt beim
  Read-only-plus-Push-MVP; die Operationen liegen im Core (`tree.rs`) und können später auch als
  CLI-Befehle (`rm`, `mv`, `--recursive`) angebunden werden.
* **Datei ersetzt keinen Ordner:** auch mit `--overwrite` wird ein Ordner nicht durch eine Datei
  ersetzt; das wäre zu destruktiv für eine einzelne Bestätigung. Ordner ersetzen Dateien dagegen
  schon (mit Rückfrage).
* **Mount als Wrapper statt eigener FUSE-Stack:** `booxmtp` enthält keinen Dateisystem-Treiber,
  sondern startet `mtp-mount` (MIT/Apache) mit derselben Geräte- und Storage-Auswahl. Das spart
  einen zweiten, schlechter getesteten Schreibpfad und hält unseren Build frei von macFUSE-/
  libfuse-Abhängigkeiten. Windows bleibt außen vor (WPD im Explorer).
* **Streaming-Download über Fenster:** statt einer durchgehenden Bulk-Übertragung nutzt `pull`
  `download_windowed` (4 MiB-Fenster), damit die USB-Session zwischen Fenstern frei bleibt und ein
  Abbruch das Gerät nicht blockiert.

## Nicht-Ziele des MVP

Ein eigener FUSE-Stack, Mount unter Windows, Hintergrund-Sync, WLAN/BOOXDrop, automatisches Löschen
und ein eigener MTP-Protokollstack. Ein `libmtp`-Backend ist als Fallback denkbar,
falls ein Gerät mit `mtp-rs` nicht zusammenarbeitet; die Modulgrenzen (`RemoteFs`, `StorageEntry`)
sind darauf vorbereitet.

## Getestete Umgebung

* BOOX Go Color 7 (interner Speicher + microSD), macOS, Apple Silicon
* CLI und TUI (PTY-End-to-End: Stapel-Kopie inkl. Ordner, rekursives Löschen mit Rückfrage,
  Download mit Überschreib-Dialog, Ordner anlegen, SHA-256-Roundtrip)
* Mount-Wrapper bis zur Helper-Übergabe; der eigentliche FUSE-Mount scheiterte lokal an
  FUSE-T 1.2.9 + fuser 0.18 (macFUSE ohne Dev-Dateien vorhanden), siehe Abschnitt „Mounten“
* `mtp-rs` 0.37, `tokio` 1.x, `ratatui` 0.30, Rust 1.99 (Edition 2024)

Andere BOOX-Modelle oder Firmware-Stände können Storage-Metadaten anders melden. `booxmtp
storages -v` zeigt den Rohbefund (`storage_type`, `filesystem_type`, `removable`), was bei der
Einordnung hilft.

## Lizenz

MIT — siehe [LICENSE](LICENSE). Copyright (c) 2026 Stefan Köppel.
