//! Mount the reader as a local filesystem by delegating to `mtp-mount`.
//!
//! `booxmtp` itself does **not** contain a filesystem implementation. Mounting needs a platform
//! driver (FUSE on macOS/Linux) and a lot of protocol-specific buffering, caching and error
//! recovery; [`mtp-mount`](https://crates.io/crates/mtp-mount) already does that on top of the same
//! MTP stack (`mtp-rs`), so this module is deliberately a thin wrapper:
//!
//! 1. open the reader with our own session, so the device selection and the `--storage` aliases
//!    behave exactly like in every other command,
//! 2. close it again (the mount helper needs the USB interface),
//! 3. spawn `mtp-mount` with the same device and the chosen options,
//! 4. translate the storage description into the sub-folder the mount will expose,
//! 5. wait for the helper, translating Ctrl-C into a clean unmount.
//!
//! Windows is rejected on purpose: there is no FUSE there, and Windows Explorer already exposes MTP
//! devices natively through WPD.
//!
//! # Limitations (inherited from MTP, not from this wrapper)
//!
//! * All storages appear as sub-folders of the mount point; MTP has no single root across storages.
//! * POSIX semantics are approximated: permissions are faked, there are no symlinks or xattrs, and
//!   writes are buffered locally by `mtp-mount` and uploaded when the file is closed.
//! * The mount is read-only unless [`MountRequest::read_only`] is disabled explicitly.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{Error, Result};
use crate::session::Session;
use crate::storage::StorageEntry;

/// Program used to do the actual mounting. Override with `BOOXMTP_MOUNT_HELPER` for a custom build.
pub const DEFAULT_HELPER: &str = "mtp-mount";
/// Environment variable that overrides [`DEFAULT_HELPER`].
pub const HELPER_ENV: &str = "BOOXMTP_MOUNT_HELPER";

/// How long to wait for the helper to finish unmounting after Ctrl-C.
const UNMOUNT_TIMEOUT: Duration = Duration::from_secs(20);

/// How long to pause between closing our session and starting the helper.
///
/// macOS can take a moment to release the USB interface; without the pause the helper's open can
/// race the close of our own session.
const HANDOVER_DELAY: Duration = Duration::from_millis(150);

/// What the caller asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountRequest {
    /// Existing, empty directory that becomes the mount point.
    pub mountpoint: PathBuf,
    /// Mount without write support (the default of the CLI).
    pub read_only: bool,
    /// Keep the mount alive for this many seconds when the device drops off.
    pub reconnect_timeout: Option<u64>,
    /// Where the helper buffers writes; `None` uses its platform default.
    pub spool_dir: Option<PathBuf>,
}

/// Mount the reader and block until the helper exits (Ctrl-C unmounts).
///
/// The device is opened here (not by the caller) so the mount can fail before touching the USB
/// interface when the helper or the mount point is unusable. `storage` is an optional selector
/// (`internal`, `sd`, or a numeric ID) that is validated up front and translated into the
/// sub-folder the mount will show.
pub async fn run(serial: Option<&str>, storage: Option<&str>, request: MountRequest) -> Result<()> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(Error::UnsupportedByApp {
            what: "mounting is only supported on macOS and Linux; Windows Explorer exposes MTP \
                   devices natively"
                .to_string(),
        });
    }
    validate_mountpoint(&request.mountpoint)?;

    let helper = helper_program();
    if !helper_is_available(&helper) {
        return Err(Error::MountHelperMissing);
    }

    let session = Session::connect(serial).await?;

    // Resolve the storage (and its mount sub-folder) while we still own the device.
    let device = session.summary().display_name();
    let device_serial = session.summary().serial_number.clone();
    let storages: Vec<(String, String)> = session
        .catalog()
        .entries()
        .iter()
        .map(|entry| (storage_label(entry), storage_dir_name(entry)))
        .collect();
    let selected = match storage {
        Some(selector) => {
            let handle = session.select_storage(selector)?;
            let name = storage_dir_name(handle.entry);
            Some((handle.entry.description.clone(), name))
        }
        None => None,
    };

    session
        .close()
        .await
        .map_err(|error| error.contextualize("closing the device before mounting"))?;
    tokio::time::sleep(HANDOVER_DELAY).await;

    report_plan(&device, &request, &storages, &selected);

    let arguments = helper_arguments(&request, device_serial.as_deref());
    tracing::debug!(helper = %helper, ?arguments, "starting the mount helper");

    let mut child = tokio::process::Command::new(&helper)
        .args(&arguments)
        .spawn()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Error::MountHelperMissing
            } else {
                Error::io(format!("starting `{helper}`"), error)
            }
        })?;

    // Ctrl-C reaches the helper too (same foreground process group); it unmounts on SIGINT. We wait
    // for that instead of dying immediately and possibly leaving a half-unmounted volume behind.
    let status = tokio::select! {
        status = child.wait() => status,
        _ = tokio::signal::ctrl_c() => {
            eprintln!("Unmounting…");
            match tokio::time::timeout(UNMOUNT_TIMEOUT, child.wait()).await {
                Ok(status) => status,
                Err(_) => {
                    let _ = child.start_kill();
                    return Err(Error::MountFailed {
                        message: "the mount helper did not finish unmounting in time".to_string(),
                    });
                }
            }
        }
    }
    .map_err(|error| Error::io("waiting for the mount helper", error))?;

    if status.success() {
        eprintln!("Unmounted {}", request.mountpoint.display());
        Ok(())
    } else {
        Err(Error::MountFailed {
            message: format!(
                "the mount helper exited with {}",
                status
                    .code()
                    .map_or_else(|| "a signal".to_string(), |code| format!("status {code}"))
            ),
        })
    }
}

/// Check that the mount point exists and is a directory.
fn validate_mountpoint(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(Error::InvalidMountpoint {
            path: path.to_path_buf(),
            reason: "is not a directory".to_string(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(Error::InvalidMountpoint {
                path: path.to_path_buf(),
                reason: "does not exist".to_string(),
            })
        }
        Err(error) => Err(Error::io(format!("inspecting `{}`", path.display()), error)),
    }
}

/// The sub-folder `mtp-mount` creates for a storage.
///
/// It uses the device-provided description and falls back to `Storage_<id>` when the device reports
/// none, exactly like the helper does.
pub fn storage_dir_name(entry: &StorageEntry) -> String {
    if entry.description.is_empty() {
        format!("Storage_{}", entry.numeric_id())
    } else {
        entry.description.clone()
    }
}

/// Full path of a storage inside the mount.
pub fn storage_mount_path(mountpoint: &Path, entry: &StorageEntry) -> PathBuf {
    mountpoint.join(storage_dir_name(entry))
}

/// The helper binary, honouring [`HELPER_ENV`].
fn helper_program() -> String {
    std::env::var(HELPER_ENV)
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_HELPER.to_string())
}

/// Whether `program` can be spawned: either a path to a file, or a name found on `PATH`.
fn helper_is_available(program: &str) -> bool {
    let path = Path::new(program);
    if path.components().count() > 1 {
        return path.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(program))
            .any(|candidate| candidate.is_file())
    })
}

/// Arguments for the helper (everything except the program name).
///
/// Kept separate from spawning so the argument mapping is unit-testable.
fn helper_arguments(request: &MountRequest, serial: Option<&str>) -> Vec<OsString> {
    let mut arguments = Vec::new();
    if let Some(serial) = serial {
        arguments.push("--device".into());
        arguments.push(serial.into());
    }
    if request.read_only {
        arguments.push("--read-only".into());
    }
    if let Some(seconds) = request.reconnect_timeout {
        arguments.push("--reconnect-timeout".into());
        arguments.push(seconds.to_string().into());
    }
    if let Some(spool) = &request.spool_dir {
        arguments.push("--spool-dir".into());
        arguments.push(spool.as_os_str().to_os_string());
    }
    arguments.push(request.mountpoint.as_os_str().to_os_string());
    arguments
}

/// A short label for a storage in the hint output: the alias if it has one, else the ID.
fn storage_label(entry: &StorageEntry) -> String {
    match entry.alias() {
        Some(alias) => alias.as_str().to_string(),
        None => entry.numeric_id().to_string(),
    }
}

/// Tell the user where the mount will show what.
fn report_plan(
    device: &str,
    request: &MountRequest,
    storages: &[(String, String)],
    selected: &Option<(String, String)>,
) {
    let mode = if request.read_only {
        "read-only"
    } else {
        "read-write"
    };
    eprintln!(
        "Mounting {device} at {} ({mode}).",
        request.mountpoint.display()
    );

    if let Some((description, name)) = selected {
        eprintln!(
            "  storage \"{description}\" will be at {}",
            request.mountpoint.join(name).display()
        );
    } else if !storages.is_empty() {
        eprintln!("  the mount will show:");
        for (label, name) in storages {
            eprintln!(
                "    {label:<9} -> {}",
                request.mountpoint.join(name).display()
            );
        }
    }
    eprintln!("  press Ctrl-C to unmount.");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageCatalog;
    use mtp_rs::mtp::StorageId;

    fn storage(description: &str, id: u64) -> StorageEntry {
        let entry = StorageEntry::new(StorageId(id), description)
            .with_capacity(1024, 512)
            .with_removable(false);
        StorageCatalog::new([entry]).entries()[0].clone()
    }

    fn request() -> MountRequest {
        MountRequest {
            mountpoint: PathBuf::from("/mnt/reader"),
            read_only: true,
            reconnect_timeout: None,
            spool_dir: None,
        }
    }

    #[test]
    fn storage_folders_match_the_helper_naming() {
        assert_eq!(
            storage_dir_name(&storage("Internal storage", 65537)),
            "Internal storage"
        );
        assert_eq!(storage_dir_name(&storage("", 65537)), "Storage_65537");
        assert_eq!(
            storage_mount_path(Path::new("/mnt/reader"), &storage("SD Card", 131073))
                .display()
                .to_string(),
            "/mnt/reader/SD Card"
        );
    }

    #[test]
    fn arguments_start_with_the_device_and_end_with_the_mountpoint() {
        let arguments = helper_arguments(&request(), Some("6DE76AE3"));
        assert_eq!(
            arguments,
            vec![
                OsString::from("--device"),
                OsString::from("6DE76AE3"),
                OsString::from("--read-only"),
                OsString::from("/mnt/reader"),
            ]
        );
    }

    #[test]
    fn writable_mounts_omit_the_read_only_flag() {
        let mut request = request();
        request.read_only = false;
        request.reconnect_timeout = Some(30);
        request.spool_dir = Some(PathBuf::from("/var/tmp/spool"));
        let arguments = helper_arguments(&request, None);
        assert_eq!(
            arguments,
            vec![
                OsString::from("--reconnect-timeout"),
                OsString::from("30"),
                OsString::from("--spool-dir"),
                OsString::from("/var/tmp/spool"),
                OsString::from("/mnt/reader"),
            ]
        );
    }

    #[test]
    fn the_mountpoint_must_be_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_mountpoint(dir.path()).is_ok());

        let file = dir.path().join("file");
        std::fs::write(&file, b"x").unwrap();
        let error = validate_mountpoint(&file).unwrap_err();
        assert!(matches!(error, Error::InvalidMountpoint { .. }));

        let error = validate_mountpoint(&dir.path().join("missing")).unwrap_err();
        assert!(matches!(error, Error::InvalidMountpoint { .. }));
        assert!(error.hint().unwrap().contains("directory"));
    }
}
