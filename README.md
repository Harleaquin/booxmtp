# booxmtp

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

Noch nicht enthalten (bewusst, siehe „Nicht-Ziele“): Rekursion, `rm`/`mv`, Synchronisation,
`--json`, Fortschrittsbalken im Vollbild.

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
git clone <repo> && cd BooxMTP
cargo build --release
# Binary: target/release/booxmtp
# oder systemweit:
cargo install --path .
```

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
  remote_path.rs   Pfadnormalisierung + segmentweises Auflösen (RemoteFs-Trait)
  transfer.rs      Streaming-Download/-Upload, .part-Handling, Verifikation
  output.rs        Tabellen, Größen/Datum/Namen, Fortschrittszeile
  error.rs         ein Fehlertyp mit Hinweisen und stabilen Exit-Codes
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

### TUI/GUI vorbereitet

Die geplante zweite Ausbaustufe (TUI, danach GUI) kann direkt auf der Library aufsetzen:

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

Hardware-Checkliste (manuell, mit angeschlossenem Reader):

1. `booxmtp devices` und `booxmtp storages` — Gerät und alle Speicher sichtbar.
2. `ls -s internal /` und `ls -s sd /` — beide Speicher getrennt.
3. `push` einer ~2 MiB großen Zufallsdatei auf beide Speicher, danach `pull` und SHA-256 vergleichen.
4. Kollision ohne `--overwrite` (muss scheitern), mit `--overwrite` (muss gelingen).
5. Fehlender Pfad, fehlende SD-Karte (`--storage sd` bei eingelegter/entfernter Karte).
6. USB-Kabel während eines Transfers ziehen — verständliche Fehlermeldung, kein Panic.

## Bewusste Abweichungen vom Plan

* **Fehlertyp statt `anyhow`:** `error.rs` definiert einen domänenspezifischen `thiserror`-Enum.
  Nur so lassen sich Hinweise und stabile Exit-Codes zentral und UI-neutral ableiten.
* **`mkdir` ist bereits umgesetzt** (im Plan als optionales Phase-2-Element markiert), weil es für
  Upload-Tests und die Bedienung offensichtlich nützlich ist.
* **`--overwrite` für `pull` und `push`** ist implementiert, mit dokumentierter Semantik: lokal
  atomar per `rename`, remote per Löschen-und-neu-Hochladen (MTP kann nicht atomar ersetzen).
* **Kein `--json`:** die Datenmodelle (`DirListing`, `StorageEntry`, …) sind dafür vorbereitet, die
  Ausgabe ist aber noch rein menschenlesbar.
* **Streaming-Download über Fenster:** statt einer durchgehenden Bulk-Übertragung nutzt `pull`
  `download_windowed` (4 MiB-Fenster), damit die USB-Session zwischen Fenstern frei bleibt und ein
  Abbruch das Gerät nicht blockiert.

## Nicht-Ziele des MVP

FUSE-Mount, Hintergrund-Sync, WLAN/BOOXDrop, rekursive Transfers, automatisches Löschen,
Windows-Support und ein eigener MTP-Protokollstack. Ein `libmtp`-Backend ist als Fallback denkbar,
falls ein Gerät mit `mtp-rs` nicht zusammenarbeitet; die Modulgrenzen (`RemoteFs`, `StorageEntry`)
sind darauf vorbereitet.

## Getestete Umgebung

* BOOX Go Color 7 (interner Speicher + microSD), macOS, Apple Silicon
* `mtp-rs` 0.37, `tokio` 1.x, Rust 1.99 (Edition 2024)

Andere BOOX-Modelle oder Firmware-Stände können Storage-Metadaten anders melden. `booxmtp
storages -v` zeigt den Rohbefund (`storage_type`, `filesystem_type`, `removable`), was bei der
Einordnung hilft.
