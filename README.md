# booxmtp

**English** · [Deutsch](README.de.md)

[![CI](https://github.com/Harleaquin/booxmtp/actions/workflows/ci.yml/badge.svg)](https://github.com/Harleaquin/booxmtp/actions/workflows/ci.yml)

File transfer between macOS/Linux and Android-based MTP e-readers — developed and tested with a
**BOOX Go Color 7**. Pure Rust, no Finder mount, no Android File Transfer, no `libmtp`/FFI.

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

The MVP from `PLAN.md` (German) is implemented and verified on real hardware:

* device discovery, storage listing with IDs and the `internal`/`sd` aliases
* `ls` (including nested folders), `pull`, `push`, `mkdir`
* streaming in both directions (a file is never read into memory as a whole)
* byte-identical push/pull round trips (SHA-256 on a BOOX Go Color 7, macOS, Apple Silicon)
* error cases: missing path, collision, missing SD card, invalid storage ID, cancellation
* **Midnight-Commander-style TUI** (`booxmtp tui`): two panels (local ↔ reader), navigation,
  multi-selection, copy, **move**, **delete with confirmation**, recursive folders, progress/
  cancellation, storage switching, help screen
* **Mounting (macOS/Linux)** via `booxmtp mount` — a wrapper around the proven `mtp-mount` helper
  (details and prerequisites below)

Deliberately not included yet (see “Non-goals”): synchronisation, `--json`, a GUI, delete/move in
the CLI (TUI only so far), mounting on Windows.

## Requirements

* Rust **1.91+** (edition 2024)
* macOS (tested) or Linux; Windows is not part of the MVP
* reader connected via USB, **unlocked**, USB mode **file transfer / MTP**
* no other MTP application holding the USB interface

### macOS pitfalls

Apple likes to claim MTP/PTP devices through `ptpcamerad`, and “Android File Transfer” gets in the
way as well. Both block access:

```bash
# Quit Android File Transfer (if installed)
osascript -e 'quit app "Android File Transfer"' 2>/dev/null

# One-off: stop ptpcamerad (takes effect immediately)
sudo killall ptpcamerad 2>/dev/null

# Persistent (Photos.app import may stop working afterwards):
# sudo launchctl disable system/com.apple.ptpcamerad
```

`booxmtp` detects this case (`ExclusiveAccess`) and prints a matching hint.

## Installation

```bash
git clone https://github.com/Harleaquin/booxmtp.git && cd booxmtp
cargo build --release
# Binary: target/release/booxmtp
# or system-wide:
cargo install --path .
```

### Prebuilt binaries

Every tagged release ships binaries on the
[releases page](https://github.com/Harleaquin/booxmtp/releases):

| Platform | Archive |
|---|---|
| Linux (x86_64, static) | `booxmtp-<version>-x86_64-unknown-linux-musl.tar.gz` |
| Linux (x86_64, glibc 2.39+) | `booxmtp-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| macOS (Apple Silicon + Intel) | `booxmtp-<version>-universal-apple-darwin.tar.gz` |

The **static musl build** runs on any Linux distribution; the glibc build is a regular dynamic
binary and needs glibc 2.39 or newer.

```bash
# after downloading the archive and SHA256SUMS from the release page
sha256sum -c SHA256SUMS          # macOS: shasum -a 256 -c SHA256SUMS
tar -xzf booxmtp-*.tar.gz
./booxmtp --help
```

On macOS, clear the quarantine flag if Gatekeeper complains:
`xattr -d com.apple.quarantine booxmtp`. The binaries are self-contained; only the optional
`booxmtp mount` needs `cargo install mtp-mount` plus a FUSE driver.

## Commands

```text
booxmtp [--device <SERIAL>] [-v] [--no-progress] <COMMAND>

  devices                          list available MTP devices
  storages                         show storages with ID, alias, size and free space
  ls       -s <STORAGE> [PATH]     list a folder (default: /)
  pull     -s <STORAGE> <REMOTE> <LOCAL>
                                   download a file
  push     -s <STORAGE> <LOCAL> <REMOTE>
                                   upload a file
  mkdir    -s <STORAGE> <REMOTE>   create a folder
  tui      [-s <STORAGE>]          interactive two-panel file manager
  mount    [-s <STORAGE>] [OPTIONS] <MOUNTPOINT>
                                   mount the reader as a filesystem (macOS/Linux)
```

Global options:

| Option | Meaning |
|---|---|
| `--device <SERIAL>` | pick the device explicitly (required when several are connected) |
| `-v`, `-vv` | diagnostics on `stderr` (`debug`/`trace`); `RUST_LOG` overrides |
| `--no-progress` | no progress line (it is only drawn on a terminal anyway) |

Examples:

```bash
booxmtp devices
booxmtp storages
booxmtp ls --storage internal /Books
booxmtp ls --storage sd "/Books/Sci-Fi"
booxmtp pull --storage internal "/Books/Example.epub" ./Example.epub
booxmtp push --storage sd ./Novel.epub /Books/            # folder target: name is appended
booxmtp push --storage 65537 ./Notes.pdf /Documents/Notes.pdf
booxmtp mkdir --storage sd /Books/ToRead
booxmtp tui                       # file manager: local on the left, internal on the right
booxmtp tui --storage sd          # right panel starts on the SD card
booxmtp mount --storage internal ~/mnt/reader   # mount as a filesystem (read-only)
```

### Storage selection

| Value | Meaning |
|---|---|
| `internal` | alias for the built-in storage |
| `sd` | alias for a removable card |
| `65537` | numeric MTP storage ID — works **always** |

IDs are session-scoped and read again on every run. Aliases are assigned **only** when the metadata
reported by the device yields exactly one candidate:

1. **Strong signal:** the MTP storage type (`FixedRam`/`FixedRom` vs. `RemovableRam`/`RemovableRom`).
2. **Weak signal:** keywords in the description (“internal”, “SD card”, “external”, …) — only used
   when no storage reports a type.

If the choice is ambiguous, `booxmtp` aborts with a list of the candidates and recommends an explicit
ID. It never assumes “first storage = internal”. That mattered on the test device: its SD card is
simply called `android` and is identified only by its `removable` type.

### Path rules

Remote paths are **MTP object paths relative to the storage root**, not POSIX paths:

* `/` is the root of the selected storage; leading `/` is optional.
* `//` and `./` are normalised.
* `..` is rejected — a remote path can never leave its storage.
* Names are compared exactly; a match that is unambiguous only after case folding is accepted
  (MTP storages are often case-insensitive but case-preserving).
* With `push`, a target ending in `/` (or `/` alone) means “put it in this folder, keep the source
  file name”.
* Missing target folders are not created implicitly (`mkdir` is a separate command).

### Safety behaviour

* **No overwriting without `--overwrite`** — neither local nor remote.
* Downloads write to `<target>.part` first, verify the byte count against the device's size and only
  then rename atomically; the `.part` file is removed on errors.
* After an upload the target folder is listed again and the size compared; a mismatch prints a
  warning (the upload still counts as successful if the device acknowledged it).
* An aborted upload is reported as such. If the device returned a handle for the half-written
  object, exactly that object is deleted — no guessing by name.
* `--overwrite` for `push` means: delete the existing object, then upload. MTP cannot replace
  atomically; if the upload fails in between, the target is gone (documented behaviour).
* `stdout` stays machine-readable (tables for `devices`, `storages`, `ls`); status messages,
  progress and warnings go to `stderr`.

## Interactive TUI (Midnight Commander style)

```text
booxmtp tui [--device <SERIAL>] [-s <STORAGE>]
```

A real screen capture (100 % terminal, no mockups):

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

### Keys

| Key | Action |
|---|---|
| `↑`/`↓`, `PgUp`/`PgDn`, `Home`/`End` | move the cursor |
| `Enter`, `→` | open a folder |
| `Backspace`, `←` | go up one level |
| `Tab` | switch the active panel |
| `Ctrl-U` | swap the panels (including their locations) |
| `Ctrl-R` | reload both panels and the storage metadata |
| `Ctrl-S` | choose the storage of the active remote panel |
| `Space` | mark/unmark an entry, then move the cursor down |
| `Ctrl-A` | mark everything in the folder (press again: clear all marks) |
| `Ctrl-C` | copy marked entries, or the entry under the cursor |
| `Ctrl-X` | move marked entries, or the entry under the cursor |
| `Ctrl-D` | delete marked entries (asks for confirmation) |
| `Ctrl-N` | create a folder (type the name, `Enter`; `Esc` cancels) |
| `?` | help |
| `Ctrl-Q`, `Esc`, `q` | quit |

**No function keys are used**: terminals, tmux and remote sessions intercept `F1`–`F12`
inconsistently. Every action lives on a `Ctrl` combination; `Ctrl-C` means **Copy** here, not
interrupt (the transfer dialog is cancelled with `Esc`). The only key without `Ctrl` is `?` for the
help screen — `Ctrl-H` would be indistinguishable from Backspace.

### Behaviour

* **Directions:** local → reader (`push`), reader → local (`pull`), local → local. The left panel is
  local, the right one shows the reader (storage switchable with `Ctrl-S`). Reader → reader is not
  supported yet (MTP cannot stream between two objects) and is refused with a hint.
* **Multi-selection:** `Space` marks an entry (check mark on the left, counter on the right of the
  panel title) and advances the cursor; `Ctrl-A` marks everything in the folder. Marks belong to a
  folder: navigating elsewhere clears them, `Ctrl-R` keeps them for entries that still exist. After
  a successful transfer the transferred entries lose their mark; skipped ones keep it (one keypress
  to retry).
* **Copy (`Ctrl-C`) and move (`Ctrl-X`)** use the same streaming core as `pull`/`push`. **Folders
  are copied and moved recursively** (MTP has no server-side recursive copy, so it runs file by
  file). A move copies first and deletes the source only afterwards; for local → local it uses
  `rename` when the destination does not exist yet. Marked entries are processed one after another
  (progress “2/5”, the status line shows the current path). Collisions are checked once up front:
  if targets exist, the TUI asks “Overwrite all” (or “Overwrite” for a single entry). If a conflict
  still shows up during the run (e.g. through another program), it asks per entry with
  “Overwrite”/“Skip”. The run ends with a summary (“Copied 3 of 5 entries (2 skipped)”).
* **Delete (`Ctrl-D`)** always asks for confirmation (“Delete?”/“Cancel”) and points out when
  folders and their contents are affected. Deletion is recursive; afterwards the marks of the
  deleted entries are removed and the panel is reloaded. A file-over-folder conflict is never
  silently “overwritten”: replacing a folder with a file is deliberately only possible via delete +
  copy.
* **Limits:** reader → reader stays unsupported (hint: use a local panel in between). `Esc` cancels
  a running transfer or the remaining entries of a batch.
* **Nothing blocks the interface:** device I/O runs on its own thread with its own Tokio runtime;
  the UI keeps handling keys and redrawing progress meanwhile.
* **Free space** is refreshed after every transfer (header and status line).
* **Diagnostics:** so that log output does not scribble over the full-screen UI, `-v` writes it to a
  file (`$TMPDIR/booxmtp-tui.log`); the help screen (`?`) shows the path.
* **Errors** appear as a dialog with a hint; fatal errors (no device, wrong serial) leave the TUI and
  are reported like any CLI error, with an exit code.

## Mounting (macOS/Linux)

```bash
cargo install mtp-mount                    # does the actual mounting (FUSE)
cargo install --path .                     # booxmtp itself needs no FUSE build dependency
mkdir -p ~/mnt/reader
booxmtp mount --storage internal ~/mnt/reader

ls  ~/mnt/reader/"Interner gemeinsamer Speicher"/Books
cp  ./Novel.epub ~/mnt/reader/"Interner gemeinsamer Speicher"/Books/   # only with --read-write
```

`booxmtp mount` is deliberately a **wrapper around [`mtp-mount`](https://crates.io/crates/mtp-mount)**
rather than its own FUSE implementation: a filesystem over MTP needs local spooling (the size is only
known at `close()`), its own inode table against unstable object handles, caches and reconnect logic.
`mtp-mount` (MIT/Apache, same `mtp-rs` stack) already solves that in ~5,400 lines with 128 tests. We
only put our device and storage selection in front of it — which also keeps our build free of FUSE
dependencies and driver code.

### Requirements

| Platform | What is needed |
|---|---|
| Linux | `fuse3` (e.g. `sudo apt install fuse3`), possibly membership in the `fuse` group |
| macOS | [macFUSE](https://macfuse.github.io/) including development files (`fuse.pc`, `libfuse`) and `pkg-config` — the tested path; FUSE-T is an alternative, see the test status |
| Windows | **not supported** — Explorer already mounts MTP devices natively through WPD |

`mtp-mount` is installed separately (`cargo install mtp-mount`); `booxmtp` checks for it and prints
the install command when it is missing. A custom helper build can be selected with
`BOOXMTP_MOUNT_HELPER=/path/to/mtp-mount`.

### Behaviour

* The mount shows **all** storages as sub-folders: `<MOUNTPOINT>/<device description>`.
  `--storage internal` validates the alias with the same logic as every other command and prints the
  exact path (including a hint when no SD card is detected).
* **Read-only is the default.** `--read-write` allows writes; `mtp-mount` buffers writes locally and
  uploads them on close. The spool lives in the cache directory by default and can be redirected with
  `--spool-dir <PATH>` (leave room for the largest file).
* `--reconnect-timeout <SECONDS>` keeps the mount alive across a flaky cable.
* `Ctrl-C` unmounts cleanly; alternatively `umount <MOUNTPOINT>` (Linux) or
  `diskutil unmount <MOUNTPOINT>` (macOS).
* Filesystem semantics remain MTP: permissions/owners are made up, there are no symlinks, xattrs or
  hard links; Finder/Spotlight generate a lot of requests (the helper caches); in-place edits go
  through download + upload; the device has one MTP session, so accesses are serialised. Read-only
  is therefore the safe default.

### Honest test status

The wrapper is verified on the BOOX up to the handover to the helper: device selection, storage
alias, mountpoint checks, error paths and process/signal handling work. The actual mount could not
be completed on the development Mac — fuser 0.18 + FUSE-T 1.2.9 fails with
`Short read of FUSE request header`, and macFUSE is installed there without development files
(`pkg-config`/`libfuse` missing), so the mount could not be built. On Linux the path is the one
`mtp-mount` tests. If the mount fails on your machine, the wrapper says so and points at the FUSE
driver and the mount point.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | other failure |
| 2 | usage error (`clap`) |
| 3 | no MTP device found |
| 4 | device selection (several devices / unknown serial) |
| 5 | storage selection (unknown, ambiguous, read-only) |
| 6 | remote path (invalid, missing, wrong type) |
| 7 | local file (missing, collision, I/O) |
| 8 | conflict (already exists, no space) |
| 9 | transfer/device failure (USB, protocol, disconnect) |
| 130 | cancelled by the user (Ctrl-C) |

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `no MTP device found` | check cable/USB mode/unlock; on macOS stop `ptpcamerad` and Android File Transfer |
| `more than one MTP device is connected` | use `--device <SERIAL>` (`booxmtp devices`) |
| `no storage on this device matches the alias sd` | card not inserted/mounted, or the device does not report it as `removable` — `booxmtp storages` shows the real IDs |
| `the storage alias internal is ambiguous` | the device reports several similar storages — use an explicit ID |
| `... does not exist on the device` | check the path with `booxmtp ls -s <ID> <PARENT_FOLDER>` |
| a transfer fails with `Disconnected` | reconnect the device; leftover `.part` files are removed |
| everything hangs | `-v` shows the last operation; try another USB hub/port |

## Architecture

```text
src/
  main.rs          CLI entry point: parsing, tracing, exit codes
  cli.rs           clap definitions + command dispatch + output
  session.rs       UI-agnostic façade: device + storages + operations
  device.rs        USB discovery and strict device selection
  storage.rs       storage metadata, alias resolution, selectors
  location.rs      panel locations (local or storage + remote path), used by copy/move/delete
  remote_path.rs   path normalisation + segment-wise resolution (RemoteFs trait)
  tree.rs          recursive copy, move and delete (host ↔ reader)
  mount.rs         wrapper: device/storage selection + starting the `mtp-mount` helper
  transfer.rs      streaming download/upload, .part handling, verification
  output.rs        tables, sizes/dates/names, progress line
  error.rs         one error type with hints and stable exit codes
  tui/
    mod.rs         terminal lifecycle (alternate screen, panic hook, restore) + event loop
    app.rs         pure state machine: keys/events in, actions out
    entry.rs       panel locations (local/remote) and unified file entries
    worker.rs      background thread with session, request/event channels, cancel token
    ui.rs          rendering with ratatui (panels, dialogs, progress, help)
tests/
  remote_paths.rs        path rules against an in-memory tree
  storage_selection.rs   alias/ID resolution including ambiguity
  device_integration.rs  end-to-end against mtp-rs' virtual MTP device
  tree_ops.rs            recursive copy/move/delete against the virtual device
```

Guidelines:

* Outside `cli`/`main`, no module writes to stdout/stderr or exits the process.
* Operations are `async` methods on [`Session`](src/session.rs) and return plain data
  (`DirListing`, `DownloadReport`, `UploadReport`, …).
* Progress is delivered as a callback (`TransferProgress`), cancellation through
  `mtp_rs::CancelToken` (Ctrl-C sets it; downloads check between windows, uploads per chunk).
* Remote paths are resolved segment by segment through the `RemoteFs` trait. That keeps the logic
  testable without hardware and allows preview/navigation features in a UI later.

### TUI architecture

The TUI is deliberately split into four layers:

```text
 ratatui/crossterm            app.rs (pure state machine)          worker.rs (thread + runtime)
 ┌──────────────┐    keys    ┌──────────────────────────┐ Request ┌──────────────────────────┐
 │  ui.rs       │──────────▶ │ Panel/ Dialog/ Progress  │────────▶│ Session + tokio          │
 │  (rendering) │◀────────── │  → Vec<Action>           │◀────────│  MTP operations           │
 └──────────────┘   screen   └──────────────────────────┘  Event  └──────────────────────────┘
                                      ▲                                    │
                                      └────────── CancelToken ◀────────────┘
```

* `app.rs` knows neither the terminal nor the device: tests feed keys and events and check state,
  dialogs and the resulting actions.
* `worker.rs` processes requests strictly one at a time (MTP has a single session). Progress and
  results come back as events; cancellation goes through a shared `CancelToken`, so a running
  transfer does not have to wait for the next message.
* `ui.rs` only draws; it also reports the visible row count back so `PgUp`/`PgDn` jump by one screen.
* `mod.rs` owns the terminal lifecycle: alternate screen + raw mode, `ratatui`'s panic hook, and a
  drop guard that restores the terminal on every exit path.

Tests: unit tests in the library (including the state machine and rendering through `TestBackend`),
integration tests for path rules and storage selection, end-to-end tests against the virtual MTP
device (`device_integration.rs`) and for recursive copy/move/delete operations (`tree_ops.rs`), unit
tests for the mount wrapper (storage sub-folder, argument mapping, mountpoint validation) and a PTY
smoke test (`scripts/tui-smoke.py`, needs real hardware and `pyte`). The smoke test marks three
entries (two files and a folder), copies them as a batch, deletes the folder recursively on the
device, downloads a file with overwrite confirmation and checks the SHA-256 sum. The mount itself
needs a FUSE driver and `mtp-mount` and therefore does not run in CI.

### Path to a GUI

A GUI can use the same foundation:

```rust
use booxmtp::{RemotePath, Session, TransferOptions};

let session = Session::connect(None).await?;
let storage = session.select_storage("internal")?;
let listing = session.list_dir(storage.id(), &RemotePath::parse("/Books")?, None).await?;

let mut on_progress = |p: booxmtp::TransferProgress| {
    // e.g. forward to a channel for the event loop
    let _ = p.percent();
};
session
    .download_file(storage.id(), &RemotePath::parse("/Books/x.epub")?, "./x.epub".as_ref(),
                   &TransferOptions::default(), &mut on_progress)
    .await?;
```

* `Session` is `Send` and holds no UI state; operations can run in a background task while the UI
  processes events.
* Cancellation: call `CancelToken::cancel()` from the event loop; the operation ends with
  `Error::Cancelled` (exit code 130).
* For a device/storage picker in the UI: `booxmtp::device::list_devices()`, `Session::attach()`,
  `session.catalog()` and `session.reload_storages()`.
* `booxmtp::output` provides formatting for bytes, dates, names and tables; a GUI can use its own
  widgets instead.

## Tests

```bash
cargo test                 # unit tests + virtual MTP integration tests
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

The integration tests (`tests/device_integration.rs`) use the `virtual-device` feature of `mtp-rs`:
a temporary directory is exposed as an MTP storage, including upload, download, collisions,
`mkdir`, Unicode names and cancellation. That lets the whole logic run without hardware.

The TUI is checked on three levels:

1. **State machine** (`src/tui/app.rs`): navigation, dialogs, copy/mkdir decisions, conflicts,
   cancellation, stale responses.
2. **Rendering** (`src/tui/ui.rs`): `ratatui::TestBackend` renders the interface into a character
   buffer; the tests inspect panels, header, help and key bar.
3. **PTY end-to-end** (`scripts/tui-smoke.py`, manual): starts the real binary in a pseudo terminal,
   drives it with keys and checks screen contents. It uploads a file, downloads it again with
   overwrite confirmation (SHA-256 comparison), creates a folder and quits the TUI. Needs a
   connected reader:

   ```bash
   python3 -m venv .venv && .venv/bin/pip install pyte
   .venv/bin/python scripts/tui-smoke.py --work /tmp/booxmtp-tui-smoke
   ```

Hardware checklist (manual, with the reader attached):

1. `booxmtp devices` and `booxmtp storages` — device and all storages visible.
2. `ls -s internal /` and `ls -s sd /` — both storages separately.
3. `push` a random ~2 MiB file to both storages, then `pull` it and compare SHA-256.
4. Collision without `--overwrite` (must fail), with `--overwrite` (must succeed).
5. Missing path, missing SD card (`--storage sd` with the card inserted/removed).
6. Pull the USB cable during a transfer — understandable error message, no panic.
7. TUI: copy a folder with sub-folders with `Ctrl-C`, move it with `Ctrl-X`, delete it with `Ctrl-D`
   (after confirmation); marks disappear after the transfer.

## Deliberate deviations from the plan

* **Domain error type instead of `anyhow`:** `error.rs` defines a `thiserror` enum. Only that makes
  it possible to derive hints and stable exit codes centrally and independently of the UI.
* **`mkdir` is already implemented** (marked as an optional phase-2 item in the plan) because it is
  obviously useful for upload tests and for day-to-day use.
* **`--overwrite` for `pull` and `push`** is implemented with documented semantics: locally atomic
  via `rename`, remotely via delete-and-reupload (MTP cannot replace atomically).
* **No `--json`:** the data models (`DirListing`, `StorageEntry`, …) are ready for it, but the
  output is still purely human-readable.
* **Delete, move and recursive folders exist only in the TUI so far.** The CLI stays at the
  read-only-plus-push MVP; the operations live in the core (`tree.rs`) and can later be exposed as
  CLI commands (`rm`, `mv`, `--recursive`).
* **A file never replaces a folder:** even with `--overwrite` a folder is not replaced by a file;
  that would be too destructive for a single confirmation. A folder does replace a file (with
  confirmation).
* **Mount as a wrapper instead of its own FUSE stack:** `booxmtp` contains no filesystem driver, it
  starts `mtp-mount` (MIT/Apache) with the same device and storage selection. That avoids a second,
  less-tested write path and keeps our build free of macFUSE/libfuse dependencies. Windows stays out
  (WPD in Explorer).
* **Streaming download in windows:** instead of one continuous bulk transfer, `pull` uses
  `download_windowed` (4 MiB windows) so the USB session is free between windows and a cancellation
  cannot wedge the device.

## Non-goals of the MVP

An in-house FUSE stack, mounting on Windows, background sync, Wi-Fi/BOOXDrop, automatic deletion and
a custom MTP protocol stack. A `libmtp` backend is conceivable as a fallback if a device does not
work with `mtp-rs`; the module boundaries (`RemoteFs`, `StorageEntry`) are prepared for that.

## Tested environment

* BOOX Go Color 7 (internal storage + microSD), macOS, Apple Silicon
* CLI and TUI (PTY end-to-end: batch copy including a folder, recursive delete with confirmation,
  download with overwrite dialog, folder creation, SHA-256 round trip)
* mount wrapper up to the helper handover; the actual FUSE mount failed locally due to
  FUSE-T 1.2.9 + fuser 0.18 (macFUSE present without dev files), see “Mounting”
* `mtp-rs` 0.37, `tokio` 1.x, `ratatui` 0.30, Rust 1.99 (edition 2024)

Other BOOX models or firmware versions may report storage metadata differently. `booxmtp storages -v`
shows the raw facts (`storage_type`, `filesystem_type`, `removable`), which helps with the mapping.

## License

MIT — see [LICENSE](LICENSE). Copyright (c) 2026 Stefan Köppel.
