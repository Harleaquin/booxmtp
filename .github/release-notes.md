Prebuilt binaries for **Linux (x86_64)** and **macOS (universal: Apple Silicon + Intel)**.

## Install

1. Download the archive for your platform together with `SHA256SUMS` and verify it:
   `sha256sum -c SHA256SUMS` (Linux) or `shasum -a 256 -c SHA256SUMS` (macOS).
2. Extract it: `tar -xzf booxmtp-*.tar.gz`.
3. Run `./booxmtp --help`. On macOS, if Gatekeeper complains, clear the quarantine flag first:
   `xattr -d com.apple.quarantine booxmtp`.
4. Optional: `sudo install -m 755 booxmtp /usr/local/bin/` (or `cp booxmtp ~/.local/bin/`).

The archives contain the `booxmtp` binary plus `LICENSE` and both READMEs. They are self-contained —
the only optional extra is `booxmtp mount`, which needs `cargo install mtp-mount` and a FUSE driver
(macFUSE on macOS, fuse3 on Linux).

See [CHANGELOG.md](https://github.com/Harleaquin/booxmtp/blob/main/CHANGELOG.md) for what is in this
release and [README.md](https://github.com/Harleaquin/booxmtp#readme) for usage.
