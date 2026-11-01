# PLAN.md — booxmtp

## 1. Ziel

Entwicklung eines kleinen, zuverlässigen CLI-Programms in **Rust** für den Dateitransfer zwischen **macOS** und einem Android-basierten MTP-Reader, zunächst dem **BOOX Go Color 7**.

Das Programm soll ohne Finder-Mount und ohne externe GUI Dateien **auflisten**, **vom Reader herunterladen** (`pull`) und **auf den Reader hochladen** (`push`). Es muss mehrere MTP-Speicherbereiche wie **internen Speicher** und **microSD-Karte** separat unterstützen.

**Projektname / Binary:** `booxmtp`

**Nicht-Ziele für den MVP:** FUSE-Mount, Hintergrund-Synchronisierung, WLAN/BOOXDrop, automatisches Löschen, Rekursion bei Transfers, Windows-Unterstützung oder ein eigener MTP-Protokollstack.

## 2. Technische Entscheidung

- **Sprache:** Rust (stable, Edition 2024).
- **MTP-Backend:** [`mtp-rs`](https://crates.io/crates/mtp-rs), eine reine Rust-Implementierung mit USB-Zugriff über `nusb` und asynchroner API. Version beim Projektstart prüfen und im `Cargo.lock` fixieren; als Ausgangspunkt `mtp-rs` 0.37.x.
- **Runtime:** `tokio`.
- **CLI:** `clap` mit `derive`.
- **Fehler:** `anyhow` für Kontext an CLI-Grenzen, optional `thiserror` für interne Fehler.
- **Ausgabe:** menschenlesbare Tabellen zunächst ohne große UI-Abhängigkeit; optional `serde`/`serde_json` für `--json` in Phase 2.
- **Logging:** `tracing` und `tracing-subscriber` für `-v`/`--verbose`.
- **Tests:** Rust-Unit-Tests für Storage-Selektion und Pfadauflösung; Integrationstests mit virtuellem MTP-Gerät von `mtp-rs`, soweit API-seitig möglich; reale Hardwaretests auf macOS.

**Warum nicht `libmtp`?** Die MTP-Funktionen sind über `mtp-rs` ohne C-Library oder FFI verfügbar. Falls das BOOX mit dieser Bibliothek konkrete Kompatibilitätsprobleme zeigt, kann ein `libmtp`-Backend später hinter einer gemeinsamen Schnittstelle ergänzt werden. Das ist ein Fallback, keine MVP-Voraussetzung.

> **Technische Annahme:** Das BOOX muss über USB im Modus **Dateiübertragung / MTP** verbunden sein und entsperrt bleiben. macOS zeigt es nicht notwendigerweise als Laufwerk im Finder. Prüfen, dass keine andere MTP-Anwendung die USB-Schnittstelle blockiert.

## 3. Benutzeroberfläche: CLI-Vertrag

```text
booxmtp [--device <SERIAL>] <COMMAND>

COMMANDS
  devices                        Verfügbare MTP-Geräte anzeigen
  storages                       Speicherbereiche mit ID, Name, Größe, freiem Platz anzeigen
  ls       --storage <S> [PATH]   Dateien/Ordner im Speicherbereich auflisten
  pull     --storage <S> <REMOTE_FILE> <LOCAL_DEST>
                                 Datei vom Reader herunterladen
  push     --storage <S> <LOCAL_FILE> <REMOTE_DEST>
                                 Datei auf den Reader hochladen
  mkdir    --storage <S> <REMOTE_DIR>
                                 Verzeichnis anlegen (Phase 2)
  help                           Hilfe

GLOBAL OPTIONS
  --device <SERIAL>              Explizite Gerätewahl, wenn mehrere Geräte verbunden sind
  -v, --verbose                  Diagnoseausgaben nach stderr

STORAGE SELECTOR <S>
  internal                       Benutzerfreundlicher Alias für internen Speicher
  sd                             Benutzerfreundlicher Alias für SD-Karte
  <STORAGE_ID>                   Numerische MTP-Storage-ID, z. B. 65537
```

Beispielbefehle:

```bash
booxmtp devices
booxmtp storages
booxmtp ls --storage internal /
booxmtp ls --storage sd /Books
booxmtp pull --storage internal /Books/Beispiel.epub ./Beispiel.epub
booxmtp push --storage sd ./Roman.epub /Books/Roman.epub
booxmtp push --storage 65537 ./Notizen.pdf /Documents/
```

**Pfadregeln:** Remote-Pfade sind *MTP-Objektpfade relativ zum gewählten Storage-Root*, keine POSIX-Dateisystempfade. Führender `/` bezeichnet dieses Root. `/Books/` ist ein Zielverzeichnis; `/Books/Roman.epub` ein expliziter Dateiname. Pfade mit Leerzeichen müssen in der Shell gequotet werden. Ein Remote-Pfad darf den gewählten Speicherbereich nie über `..` verlassen. Symlink-Semantik wird nicht versprochen.

## 4. Mehrere Speicherorte: verbindliche Anforderungen

1. Beim Verbindungsaufbau die vom Gerät tatsächlich gemeldeten **Storages** abfragen. Keinen festen Storage-Index oder BOOX-spezifischen Pfad hart codieren.
2. Pro Storage mindestens `id`, `description`, `capacity`, `free_space`, `writable` (falls bereitgestellt) erfassen. Fehlende Felder als `unknown` behandeln, nicht erfinden.
3. IDs gelten nur als Geräte-/Sitzungsinformation, nicht als dauerhaft stabile Konfiguration. Für jede CLI-Ausführung erneut ermitteln.
4. `--storage <id>` muss immer möglich sein und hat Vorrang vor heuristischer Einordnung.
5. `internal` und `sd` nur anhand verlässlich verfügbarer Storage-Metadaten (`storage_type`, `removable`, `description` etc.) ableiten. **Niemals blind annehmen: erster Storage = intern, zweiter = SD.**
6. Wenn ein Alias nicht eindeutig ist, mit einer konkreten Fehlermeldung abbrechen: gefundene Storage-IDs und Beschreibungen anzeigen und `--storage <id>` empfehlen.
7. Wenn keine SD-Karte gemeldet wird, `sd` mit hilfreicher Fehlermeldung ablehnen. Keine SD-Unterstützung simulieren.
8. Jede Objektauflistung, Pfadauflösung und Upload-Zielwahl an den ausgewählten Storage binden. Objekt-Handles können gerätespezifisch sein und dürfen nicht als globale Dateipfade verwendet werden.
9. Bei Mehrgerätebetrieb nicht stillschweigend das erste Gerät wählen: `--device` verlangen. Bei genau einem Gerät dieses standardmäßig verwenden.

Beispielausgabe (nur illustrativ, keine zugesicherten BOOX-Werte):

```text
$ booxmtp storages
ID       ALIAS      DESCRIPTION         FREE       CAPACITY
65537    internal   Internal storage    24.1 GiB   64.0 GiB
131073   sd         SD Card             90.2 GiB  128.0 GiB
```

## 5. Architektur

```text
src/
  main.rs                CLI-Einstieg, Exit-Codes
  cli.rs                 clap-Kommandos und Argumente
  device.rs              Geräteerkennung/-auswahl, Verbindung
  storage.rs             Storage-Metadaten und Aliasauflösung
  remote_path.rs         Remote-Pfadnormalisierung und MTP-Objektauflösung
  transfer.rs            Streaming-Download und -Upload
  output.rs              Tabellen, Fortschritt, Fehlermeldungen
  error.rs               Domänenspezifische Fehler (optional)
tests/
  storage_selection.rs
  remote_paths.rs
  device_integration.rs  # virtuelles MTP-Gerät / opt-in
```

### Datenmodell (konzeptionell)

```rust
struct StorageInfo {
    id: u32,
    description: String,
    capacity_bytes: Option<u64>,
    free_bytes: Option<u64>,
    removable: Option<bool>,
    writable: Option<bool>,
}

enum StorageSelector {
    Internal,
    Sd,
    Id(u32),
}
```

`device.rs` hält die konkrete `mtp-rs`-Session. `storage.rs` liefert einen ausgewählten Storage oder einen präzisen Fehler. `remote_path.rs` läuft **segmentweise** vom Storage-Root durch die Ordnerobjekte, statt Pfade an MTP-Calls weiterzureichen. `transfer.rs` verwendet die Streaming-Funktionen der Bibliothek und schreibt/liest lokal in begrenzten Chunks. Methoden- und Typnamen vor Implementierung gegen die installierte `mtp-rs`-Version prüfen.

## 6. Semantik von `ls`, `pull`, `push`

### `ls`

- Standardpfad: `/`.
- Nur direkte Kinder des gewählten Verzeichnisses im MVP anzeigen.
- Pro Objekt mindestens Typ (`file`/`dir`), Dateiname, optional Größe anzeigen.
- Fehler, wenn Pfad fehlt oder kein Verzeichnis ist.
- Niemals Daten aus einem anderen Storage beimischen.

### `pull`

- Remote-Datei über Elternordner + Dateiname auflösen und den MTP-Objekt-Handle nutzen.
- Ein einzelnes File per Streaming in eine lokale temporäre Datei (`.part`) laden.
- Nach erfolgreichem Abschluss flush/sync soweit sinnvoll und atomar bzw. mit sicherer Ersatzstrategie auf den Zielpfad verschieben.
- Bereits vorhandene Zieldateien standardmäßig **nicht überschreiben**; `--overwrite` erst bewusst hinzufügen.
- Bei Fehler temporäre Datei entfernen oder mit dokumentiertem Verhalten zur Wiederaufnahme erhalten; MVP: entfernen.
- Verfügbarkeit/Ansprechbarkeit des Gerätes während der Übertragung prüfen und Fehlermeldungen mit Pfad/Storage-Kontext versehen.

### `push`

- Prüfen, ob Quelldatei lokal existiert und reguläre Datei ist.
- Den Ziel-Storage und den Elternordner des Remote-Ziels auflösen. Fehlende Zielordner im MVP **nicht** automatisch erstellen.
- Dateigröße und freien Platz, soweit verfügbar, vorab prüfen; unbekannten freien Platz nicht als `0` interpretieren.
- Die von `mtp-rs` angebotene Streaming-Upload-API nutzen; kein vollständiges Einlesen großer Dateien in den RAM.
- Keine vorhandenen Remote-Dateien implizit überschreiben. `--overwrite` erst mit klar definierter Lösch-/Ersetzungssemantik ergänzen; bei MTP ist Ersetzen häufig **nicht** atomar.
- Erfolgreichen Upload anhand der API-Rückmeldung und nach Möglichkeit anschließendem Re-Listing verifizieren.
- Einen fehlgeschlagenen Upload als möglicherweise teilweise angelegtes Remote-Objekt behandeln und klar melden; automatische destruktive Bereinigung erst nach gesicherter Identifikation.

## 7. Fehlerfälle und Bedienbarkeit

- Kein Gerät gefunden: Verbindung, USB-Datenkabel, Entsperrung und MTP-Modus prüfen.
- Mehrere Geräte: `--device` verlangen und verfügbare Geräte anzeigen.
- USB-Interface belegt: Hinweise auf andere MTP-Apps geben.
- Speicherort fehlt/mehrdeutig: `booxmtp storages` und numerische ID empfehlen.
- Pfad nicht gefunden, Ziel ist Ordner statt Datei, Schreibschutz, kein Speicherplatz, Verbindungsabbruch, Zugriffsfehler: jeweils spezifische Fehler mit Exit-Code != 0.
- `stdout` für Nutzdaten/Listen, `stderr` für Diagnose und Fortschritt.
- Keine destruktiven Aktionen im MVP; vorliegende lokale/Remote-Ziele nicht stillschweigend überschreiben.
- Dateinamen UTF-8-freundlich ausgeben; problematische Namen verlustarm oder eindeutig escaped darstellen.

## 8. Umsetzung in Phasen

### Phase 0 — Projekt und Hardware-Baseline

- [ ] `cargo new booxmtp --bin` und Git-Repository anlegen.
- [ ] `mtp-rs`, `tokio`, `clap`, `anyhow`, `tracing` hinzufügen; APIs und Versionen verifizieren.
- [ ] Auf macOS Reader entsperren, USB-Modus **Dateiübertragung** aktivieren; andere MTP-Apps schließen.
- [ ] Mit einem Minimalprogramm `MtpDevice::open_first().await?` und `storages().await?` testen.
- [ ] Tatsächliche Storage-Metadaten mit/ohne eingelegte SD-Karte dokumentieren; Alias-Regeln auf Hardwarebefund stützen.

**Abnahme:** Gerät und alle gemeldeten Speicher sind über die Rust-Bibliothek sichtbar.

### Phase 1 — Read-only CLI

- [ ] `devices` implementieren.
- [ ] `storages` mit IDs, verfügbaren Kapazitätsdaten und sicher ermittelten Aliassen implementieren.
- [ ] `--storage` mit numerischer ID und eindeutigen Aliasen implementieren.
- [ ] `ls` mit Root und verschachtelten Remote-Verzeichnissen implementieren.
- [ ] `pull` als Streaming-Download implementieren.
- [ ] Fehlerausgaben, Exit-Codes und `--help` vervollständigen.

**Abnahme:** Eine EPUB/PDF-Datei lässt sich aus jedem tatsächlich vorhandenen Storage separat auf dem Mac lesen; SHA-256 der heruntergeladenen Testdatei stimmt mit der zuvor hochgeladenen Originaldatei überein.

### Phase 2 — Upload

- [ ] `push` mit explizitem Storage, Zielordner und Zieldateinamen implementieren.
- [ ] Kollisionsvermeidung und Kapazitätsprüfung ergänzen.
- [ ] Upload-Erfolg durch erneutes Listing überprüfen.
- [ ] Optional `mkdir` und Fortschrittsanzeige ergänzen.

**Abnahme:** Verschiedene Testdateien lassen sich jeweils auf internen Speicher und SD-Karte (sofern erkannt und beschreibbar) hochladen und anschließend bytegleich wieder herunterladen.

### Phase 3 — Härtung

- [ ] Tests für Storage-Alias-Ambiguität, entfernte SD-Karte, leere Verzeichnisse, Sonderzeichen, doppelte Dateinamen und `..`.
- [ ] Übertragung einer großen Testdatei prüfen (Streaming, begrenzter RAM-Verbrauch).
- [ ] USB-Abbruch während `pull`/`push` und sichere Fehlermeldung testen.
- [ ] `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo test` in CI.
- [ ] macOS-Build für Apple Silicon prüfen; Intel-Mac soweit verfügbar.
- [ ] README mit Installation, USB-Voraussetzungen und Beispielbefehlen erstellen.

**Abnahme:** Automatische Tests erfolgreich; manuelle Checkliste mit realem BOOX abgeschlossen.

## 9. Testmatrix

| Test | Interner Speicher | SD-Karte |
|---|---|---|
| Storage sichtbar und eindeutig auswählbar | Pflicht | Pflicht, sofern eingelegt |
| `ls /` | Pflicht | Pflicht |
| `ls /Books` | Pflicht, wenn Ordner vorhanden | Pflicht, wenn Ordner vorhanden |
| `pull` kleine EPUB | Pflicht | Pflicht |
| `push` PDF und anschließend `pull` + SHA-256 | Pflicht | Pflicht |
| Fehlender Remote-Pfad | Pflicht | Pflicht |
| Datei-Kollision ohne `--overwrite` | Pflicht | Pflicht |
| Speicher entfernt / nicht verfügbar | n/a | Pflicht |
| Große Datei (>1 GiB, falls Platz vorhanden) | Optional | Optional |
| USB-Verbindung während Transfer getrennt | Pflicht | Optional |

Wichtig: Bei Geräten, die keine separate SD-Storage-ID melden, ist das keine aus dem Programm heraus behebbare Eigenschaft. In diesem Fall die tatsächliche MTP-Speicherpräsentation und USB-Modi untersuchen und die Einschränkung dokumentieren.

## 10. Definition of Done (MVP)

- [ ] `cargo build --release` funktioniert auf macOS.
- [ ] `booxmtp devices` und `booxmtp storages` funktionieren bei angeschlossenem Reader.
- [ ] Jede gemeldete Storage-ID lässt sich explizit auswählen.
- [ ] `internal`/`sd` funktionieren **nur**, wenn ihre Zuordnung zweifelsfrei ist; sonst aussagekräftiger Fehler.
- [ ] `ls`, `pull`, `push` funktionieren in verschachtelten Ordnern auf beiden erkannten Speichern.
- [ ] Push/Pull-Roundtrip ist bytegleich (SHA-256).
- [ ] Bestehende Dateien werden ohne explizite Zustimmung nicht überschrieben.
- [ ] Fehlende SD-Karte, USB-Disconnect und fehlerhafte Pfade führen zu verständlichen Fehlern statt Panic.
- [ ] README dokumentiert Installation, Voraussetzungen und getestete BOOX-/macOS-Versionen.

## 11. Mögliche Erweiterungen nach dem MVP

- `mkdir`, `rm` (mit Schutzabfrage), `mv`, `rename`.
- `push --recursive` / `pull --recursive` und Filter für EPUB/PDF.
- `--json` für Skripting; maschinenlesbare Fehlercodes.
- `--progress`, `--dry-run` und `--overwrite` mit klarer Semantik.
- `sync` in eine Richtung mit Manifest und expliziten Konfliktregeln (kein automatisches Löschen als Standard).
- Optional ein Fallback-Backend über `libmtp`, wenn BOOX-spezifische Kompatibilitätsprobleme nachgewiesen werden.

## 12. Quellen und API-Referenzen

- [`mtp-rs` auf docs.rs](https://docs.rs/crate/mtp-rs/latest) — MTP-Geräte, Storages, Streaming und virtuelle Testgeräte.
- [`mtp-rs` auf GitHub](https://github.com/vdavid/mtp-rs) — Implementierung, Beispiele und CLI-Referenz.
- [`libmtp` API](https://github.com/libmtp/libmtp/blob/master/src/libmtp.h.in) — Alternativ-Backend / MTP-Storage-Grundlagen.

**Hinweis:** Dies ist ein Implementierungsplan, kein bereits validierter Hardwaretest. Ob das konkrete BOOX Go Color 7 die SD-Karte als zweiten MTP-Storage exponiert, muss in Phase 0 am angeschlossenen Gerät verifiziert werden.
