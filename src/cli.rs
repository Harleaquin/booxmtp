//! Command-line interface: argument parsing, command dispatch and terminal output.
//!
//! This module is the only place that knows about `stdout`/`stderr`, exit codes and `clap`. All
//! device work goes through [`Session`] and [`crate::transfer`], which is what lets a future TUI or
//! GUI reuse the same operations.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};

use clap::{ArgAction, Parser, Subcommand};
use mtp_rs::CancelToken;

use crate::device;
use crate::error::{Error, Result};
use crate::output::{
    Align, ProgressPrinter, Table, escape_name, format_datetime, format_optional_bytes,
    format_usb_speed,
};
use crate::remote_path::RemotePath;
use crate::session::Session;
use crate::storage::StorageAlias;
use crate::transfer::{TransferOptions, TransferProgress};

/// Root command.
#[derive(Debug, Parser)]
#[command(
    name = "booxmtp",
    version,
    about = "Transfer files between macOS and Android-based MTP e-readers (e.g. BOOX)",
    long_about = "booxmtp lists, downloads and uploads files on an MTP reader such as the BOOX Go \
                  Color 7. It talks MTP directly over USB — no Finder mount, no Android File \
                  Transfer, no external helper.",
    after_help = "\
STORAGE SELECTORS
  internal      alias for the built-in storage
  sd            alias for a removable card
  <ID>          numeric MTP storage ID, e.g. 65537 (always works)

  Aliases are only accepted when the device's metadata identifies exactly one matching
  storage. Otherwise `booxmtp storages` prints the IDs to use instead.

EXAMPLES
  booxmtp devices
  booxmtp storages
  booxmtp ls --storage internal /Books
  booxmtp pull --storage sd /Books/Roman.epub ./Roman.epub
  booxmtp push --storage internal ./Notes.pdf /Documents/
  booxmtp mkdir --storage sd /Books/ToRead"
)]
pub struct Cli {
    /// Serial number of the device to use (required when several are connected).
    #[arg(long, global = true, value_name = "SERIAL")]
    pub device: Option<String>,

    /// Print diagnostics to stderr; repeat for more detail.
    #[arg(short, long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Never draw a progress line (it is only drawn on an interactive terminal anyway).
    #[arg(long, global = true)]
    pub no_progress: bool,

    /// Subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// All subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List connected MTP devices.
    Devices,

    /// Show the device's storages (internal memory, SD card, …) with IDs and free space.
    Storages,

    /// List the contents of one remote folder.
    #[command(
        after_help = "Remote paths are relative to the storage root; a leading `/` is optional."
    )]
    Ls {
        /// Storage selector: `internal`, `sd`, or a numeric ID.
        #[arg(short, long, value_name = "STORAGE")]
        storage: String,

        /// Remote folder to list.
        #[arg(default_value = "/")]
        path: String,
    },

    /// Download one file from the reader.
    Pull {
        /// Storage selector: `internal`, `sd`, or a numeric ID.
        #[arg(short, long, value_name = "STORAGE")]
        storage: String,

        /// Remote file to download, e.g. `/Books/Roman.epub`.
        remote: String,

        /// Local destination file path.
        local: PathBuf,

        /// Replace the local file if it already exists.
        #[arg(long)]
        overwrite: bool,
    },

    /// Upload one file to the reader.
    #[command(after_help = "\
A remote target ending in `/` (or `/` alone) is treated as a folder: the local file name is \
appended. A target without a trailing slash is used verbatim.")]
    Push {
        /// Storage selector: `internal`, `sd`, or a numeric ID.
        #[arg(short, long, value_name = "STORAGE")]
        storage: String,

        /// Local file to upload.
        local: PathBuf,

        /// Remote target: a file path or a folder path ending in `/`.
        remote: String,

        /// Replace the remote object if it already exists.
        #[arg(long)]
        overwrite: bool,
    },

    /// Open the two-panel terminal file manager (Midnight-Commander style).
    #[command(after_help = "\
Keys: arrows/Home/End/PgUp/PgDn move, Enter opens a folder, Backspace goes up, Tab switches \
panels, Ctrl-C copies the selected file to the other panel, Ctrl-N creates a folder, Ctrl-Q quits, \
? shows help, Ctrl-S changes the storage of the active remote panel. No function keys are used. \
With -v, diagnostics are written to the file shown in the help screen (not to the terminal).")]
    Tui {
        /// Storage to show in the right panel on startup (default: internal).
        #[arg(short, long, value_name = "STORAGE")]
        storage: Option<String>,
    },

    /// Create one remote folder.
    #[command(
        after_help = "The parent folder must already exist; intermediate folders are not created."
    )]
    Mkdir {
        /// Storage selector: `internal`, `sd`, or a numeric ID.
        #[arg(short, long, value_name = "STORAGE")]
        storage: String,

        /// Remote folder to create, e.g. `/Books/ToRead`.
        remote: String,
    },
}

/// Runtime settings shared by all commands.
struct RunContext {
    /// Cancelled when the user presses Ctrl-C.
    cancel: CancelToken,
    /// Whether a progress line may be drawn.
    show_progress: bool,
}

/// Execute the parsed command.
pub async fn run(cli: Cli) -> Result<()> {
    let context = RunContext {
        cancel: CancelToken::new(),
        show_progress: !cli.no_progress && std::io::stderr().is_terminal(),
    };
    install_ctrl_c_handler(&context.cancel);

    match cli.command {
        Command::Devices => run_devices(cli.device.as_deref()).await,
        Command::Storages => run_storages(cli.device.as_deref()).await,
        Command::Ls { storage, path } => {
            run_ls(cli.device.as_deref(), &storage, &path, &context).await
        }
        Command::Pull {
            storage,
            remote,
            local,
            overwrite,
        } => {
            run_pull(
                cli.device.as_deref(),
                &storage,
                &remote,
                &local,
                overwrite,
                &context,
            )
            .await
        }
        Command::Push {
            storage,
            local,
            remote,
            overwrite,
        } => {
            run_push(
                cli.device.as_deref(),
                &storage,
                &local,
                &remote,
                overwrite,
                &context,
            )
            .await
        }
        Command::Mkdir { storage, remote } => {
            run_mkdir(cli.device.as_deref(), &storage, &remote, &context).await
        }
        Command::Tui { storage } => run_tui(cli.device.clone(), storage).await,
    }
}

impl RunContext {
    /// Options for a transfer, carrying the shared cancellation token.
    fn transfer_options(&self, overwrite: bool) -> TransferOptions {
        TransferOptions {
            overwrite,
            cancel: Some(self.cancel.clone()),
        }
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

async fn run_devices(serial: Option<&str>) -> Result<()> {
    let devices = device::list_devices()?;
    if devices.is_empty() {
        return Err(Error::NoDevice);
    }
    // `devices` always lists everything, but an explicit `--device` is still validated so a typo
    // is reported instead of silently ignored.
    if serial.is_some() {
        device::select(&devices, serial)?;
    }

    let mut table = Table::new(&["SERIAL", "MODEL", "LOCATION", "USB LINK", "VID:PID"])
        .align_column(2, Align::Right);
    for device in &devices {
        table.push([
            device
                .serial_number
                .clone()
                .unwrap_or_else(|| "(none)".to_string()),
            device.display_name(),
            format!("{:08x}", device.location_id),
            format_usb_speed(device.usb_speed),
            device.vendor_product(),
        ]);
    }
    print_stdout(&table.render())?;

    if devices.len() > 1 {
        eprintln!("hint: several devices are connected; pass `--device <SERIAL>` to choose one.");
    }
    Ok(())
}

async fn run_storages(serial: Option<&str>) -> Result<()> {
    let session = Session::connect(serial).await?;
    let mut table = Table::new(&["ID", "ALIAS", "DESCRIPTION", "FREE", "CAPACITY", "WRITABLE"])
        .align_column(0, Align::Right)
        .align_column(3, Align::Right)
        .align_column(4, Align::Right);

    for entry in session.catalog().entries() {
        table.push([
            entry.numeric_id().to_string(),
            entry
                .alias()
                .map(|alias| alias.as_str().to_string())
                .unwrap_or_else(|| "-".to_string()),
            escape_name(&entry.description),
            format_optional_bytes(entry.free_bytes),
            format_optional_bytes(entry.capacity_bytes),
            match entry.writable {
                Some(true) => "yes".to_string(),
                Some(false) => "no".to_string(),
                None => "?".to_string(),
            },
        ]);
        tracing::debug!(
            id = entry.numeric_id(),
            storage_type = ?entry.storage_type,
            filesystem = ?entry.filesystem_type,
            removable = ?entry.removable,
            volume = %entry.volume_identifier,
            "storage detail"
        );
    }
    print_stdout(&table.render())?;

    for alias in StorageAlias::ALL {
        tracing::debug!(
            %alias,
            available = session.catalog().find(&alias.into()).is_ok(),
            "alias resolution"
        );
    }

    close_quietly(session).await;
    Ok(())
}

async fn run_ls(
    serial: Option<&str>,
    storage_selector: &str,
    raw_path: &str,
    context: &RunContext,
) -> Result<()> {
    let session = Session::connect(serial).await?;
    let storage = session.select_storage(storage_selector)?;
    let path = RemotePath::parse(raw_path)?;

    let listing = session
        .list_dir(storage.id(), &path, Some(&context.cancel))
        .await?;

    if !listing.unreadable.is_empty() {
        eprintln!(
            "warning: the device did not describe {} entr{} in `{}`",
            listing.unreadable.len(),
            if listing.unreadable.len() == 1 {
                "y"
            } else {
                "ies"
            },
            path.display()
        );
    }

    if listing.entries.is_empty() {
        tracing::debug!(path = %path.display(), "folder is empty");
        return Ok(());
    }

    let mut table = Table::new(&["TYPE", "SIZE", "MODIFIED", "NAME"]).align_column(1, Align::Right);
    for entry in &listing.entries {
        table.push([
            entry.kind.to_string(),
            format_optional_bytes(entry.size),
            format_datetime(entry.modified),
            escape_name(&entry.name),
        ]);
    }
    print_stdout(&table.render())
}

async fn run_pull(
    serial: Option<&str>,
    storage_selector: &str,
    raw_remote: &str,
    local: &Path,
    overwrite: bool,
    context: &RunContext,
) -> Result<()> {
    let session = Session::connect(serial).await?;
    let storage = session.select_storage(storage_selector)?;
    let remote = RemotePath::parse(raw_remote)?;
    let options = context.transfer_options(overwrite);

    let mut printer = ProgressPrinter::new(context.show_progress);
    let report = {
        let mut on_progress = |progress: TransferProgress| printer.update(&progress);
        session
            .download_file(storage.id(), &remote, local, &options, &mut on_progress)
            .await?
    };
    printer.finish();

    eprintln!(
        "downloaded {} -> {} ({}, storage “{}”)",
        remote.display(),
        report.local.display(),
        crate::output::format_bytes(report.bytes),
        storage.description()
    );
    close_quietly(session).await;
    Ok(())
}

async fn run_push(
    serial: Option<&str>,
    storage_selector: &str,
    local: &Path,
    raw_remote: &str,
    overwrite: bool,
    context: &RunContext,
) -> Result<()> {
    let session = Session::connect(serial).await?;
    let storage = session.select_storage(storage_selector)?;
    let remote = resolve_push_target(raw_remote, local)?;
    let options = context.transfer_options(overwrite);

    let mut printer = ProgressPrinter::new(context.show_progress);
    let report = {
        let mut on_progress = |progress: TransferProgress| printer.update(&progress);
        session
            .upload_file(storage.id(), local, &remote, &options, &mut on_progress)
            .await?
    };
    printer.finish();

    eprintln!(
        "uploaded {} -> {} ({}, storage “{}”)",
        local.display(),
        remote.display(),
        crate::output::format_bytes(report.bytes),
        storage.description()
    );
    if !report.verified() {
        eprintln!(
            "warning: the device did not confirm the upload by re-listing the folder; \
             check with `booxmtp ls --storage {} {}`",
            storage.entry.numeric_id(),
            report
                .remote
                .parent()
                .map_or_else(|| "/".to_string(), |parent| parent.display())
        );
    }
    Ok(())
}

async fn run_mkdir(
    serial: Option<&str>,
    storage_selector: &str,
    raw_remote: &str,
    context: &RunContext,
) -> Result<()> {
    let session = Session::connect(serial).await?;
    let storage = session.select_storage(storage_selector)?;
    let remote = RemotePath::parse(raw_remote)?;

    let entry = session
        .create_dir(storage.id(), &remote, Some(&context.cancel))
        .await?;
    tracing::debug!(handle = entry.handle.0, "folder created");
    eprintln!(
        "created {} on storage “{}”",
        remote.display(),
        storage.description()
    );
    close_quietly(session).await;
    Ok(())
}

/// Run the blocking TUI on this thread.
///
/// `block_in_place` keeps the multi-threaded runtime usable (the TUI's own I/O worker has its own
/// thread and runtime anyway). `#[tokio::main]` uses the multi-thread flavour, which is required.
async fn run_tui(serial: Option<String>, storage: Option<String>) -> Result<()> {
    let flavor = tokio::runtime::Handle::try_current().map(|handle| handle.runtime_flavor());
    if matches!(flavor, Ok(tokio::runtime::RuntimeFlavor::MultiThread)) {
        tokio::task::block_in_place(|| crate::tui::run(serial, storage))
    } else {
        // Single-threaded runtime (not used by the binary, but let embedders call this safely):
        // run the blocking UI on its own thread.
        std::thread::spawn(move || crate::tui::run(serial, storage))
            .join()
            .unwrap_or_else(|_| {
                Err(Error::Worker {
                    message: "the TUI thread panicked".to_string(),
                })
            })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Turn the `push` target argument into a concrete remote file path.
///
/// `/Books/` (a trailing slash) or `/` means “put the file here”: the local file name is appended.
/// Anything else is used as written.
fn resolve_push_target(raw_remote: &str, local: &Path) -> Result<RemotePath> {
    let remote = RemotePath::parse(raw_remote)?;
    if raw_remote.ends_with('/') || remote.is_root() {
        let local_name = local
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::invalid_remote_path(
                    raw_remote,
                    "cannot derive a remote file name from the local path; pass an explicit target",
                )
            })?;
        return remote.join(local_name);
    }
    Ok(remote)
}

/// Close the session, ignoring failures: the work is already done and dropping the handle also
/// releases the device. An explicit close just frees the USB interface sooner.
async fn close_quietly(session: Session) {
    if let Err(error) = session.close().await {
        tracing::debug!(%error, "closing the MTP device failed");
    }
}

/// Write a block of text to stdout, tolerating a closed pipe (`booxmtp ls | head`).
fn print_stdout(text: &str) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    if let Err(error) = writeln!(stdout, "{text}") {
        if error.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(Error::io("writing to stdout", error));
    }
    Ok(())
}

/// Cancel the shared token on Ctrl-C so transfers stop between chunks instead of being killed.
fn install_ctrl_c_handler(cancel: &CancelToken) {
    let token = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            token.cancel();
        }
    });
}

/// Configure `tracing`. `RUST_LOG` overrides the flag-based default.
///
/// The file manager draws on the alternate screen, so its diagnostics go to
/// [`tui_log_path`] instead of the terminal — otherwise the first `warn!` would corrupt the UI.
pub fn init_tracing(verbose: u8, into_file: bool) {
    use tracing_subscriber::EnvFilter;

    let level = match verbose {
        0 => "warn",
        1 => "debug",
        _ => "trace",
    };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(format!("booxmtp={level},mtp_rs={level}")));

    if into_file {
        // `Arc<File>` implements `MakeWriter` through `&File: Write`, so all threads can share it.
        // If the file cannot be created, logging stays off: writing to the terminal would corrupt
        // the alternate screen.
        if let Ok(file) = std::fs::File::create(tui_log_path()) {
            let writer = std::sync::Arc::new(file);
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(writer)
                .with_ansi(false)
                .try_init();
        }
        return;
    }

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

/// Where the file manager writes its diagnostics.
pub fn tui_log_path() -> std::path::PathBuf {
    std::env::temp_dir().join("booxmtp-tui.log")
}

/// Print an error and its hint the way the CLI wants them.
pub fn report_error(error: &Error) {
    eprintln!("error: {error}");
    if let Some(hint) = error.hint() {
        eprintln!("hint: {hint}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_target_with_trailing_slash_uses_the_local_file_name() {
        let target = resolve_push_target("/Books/", Path::new("./Roman.epub")).unwrap();
        assert_eq!(target.display(), "/Books/Roman.epub");
    }

    #[test]
    fn push_target_root_uses_the_local_file_name() {
        let target = resolve_push_target("/", Path::new("/tmp/Notes.pdf")).unwrap();
        assert_eq!(target.display(), "/Notes.pdf");
    }

    #[test]
    fn push_target_without_slash_is_used_verbatim() {
        let target =
            resolve_push_target("/Books/Other-Name.epub", Path::new("./Roman.epub")).unwrap();
        assert_eq!(target.display(), "/Books/Other-Name.epub");
    }

    #[test]
    fn push_target_rejects_dotdot() {
        assert!(resolve_push_target("/Books/../../etc/passwd", Path::new("./x")).is_err());
    }
}
