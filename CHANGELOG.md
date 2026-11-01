# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/).

## [0.1.0] - 2026-11-01

First public release.

### Added

- **MTP over USB** via [`mtp-rs`](https://crates.io/crates/mtp-rs): device discovery, storage
  listing, and `internal`/`sd` aliases derived strictly from the metadata the device reports.
- **CLI**: `devices`, `storages`, `ls`, `pull`, `push`, `mkdir` — streaming in both directions,
  `.part` downloads with atomic rename, upload verification by re-listing, no implicit overwrites.
- **TUI** in Midnight Commander style (`booxmtp tui`): two panels (local ↔ reader), cursor
  navigation, multi-selection (`Space`, `Ctrl-A`), copy (`Ctrl-C`), move (`Ctrl-X`), delete with
  confirmation (`Ctrl-D`), recursive folders, batch progress with cancellation, storage switching,
  help screen, and a background I/O worker so the UI never blocks on USB.
- **Mounting** (macOS/Linux, `booxmtp mount`): a thin wrapper around the proven
  [`mtp-mount`](https://crates.io/crates/mtp-mount) helper, read-only by default, with storage
  validation and clean unmounting on Ctrl-C.
- Stable exit codes, actionable hints (including macOS `ptpcamerad`), and `-v` diagnostics.
- Automated tests: unit tests, virtual-MTP-device integration tests, recursive copy/move/delete
  tests, rendering tests for the TUI, and a PTY smoke test for real hardware.
- GitHub Actions CI (format, clippy, tests on Linux and macOS) and tag-driven release builds.

### Known limitations

- No synchronisation, no `--json` output and no GUI yet.
- Delete, move and recursive folders are currently available in the TUI only.
- Mounting on Windows is not supported (Windows Explorer exposes MTP natively via WPD).
- The mount runtime needs a FUSE driver and the `mtp-mount` helper; it is verified up to the helper
  handover (see the README for details).

[0.1.0]: https://github.com/Harleaquin/booxmtp/releases/tag/v0.1.0
