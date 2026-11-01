//! Streaming downloads, uploads and folder creation.
//!
//! Everything here works on **one file at a time** (the MVP is deliberately non-recursive) and
//! never buffers a whole file in memory:
//!
//! * Downloads use `mtp-rs`'s *windowed* reader. Each window is a bounded `GetPartialObject` call
//!   that releases the USB session in between, so a UI stays responsive and stopping early is safe
//!   (no half-drained bulk transfer can wedge the device).
//! * Uploads stream the local file through a `ReaderStream` in chunks; the device gets the total
//!   size up front, as the protocol requires.
//!
//! # Safety rules baked into this module
//!
//! * Local downloads go to `<destination>.part` first and are renamed into place only after the
//!   byte count matches what the device announced. The temporary file is removed on failure.
//! * Existing local files or remote objects are never replaced unless
//!   [`TransferOptions::overwrite`] is set.
//! * A failed upload is reported with the state of the possibly half-written remote object; it is
//!   removed again when the device told us its handle, and reported as “may remain” otherwise.
//! * Progress is delivered through a callback, never printed here. The CLI, a TUI, or a GUI can
//!   each render it their own way.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use mtp_rs::CancelToken;
use mtp_rs::mtp::{
    ByteRange, DateTime, MAX_NAME_UNITS, NewObjectInfo, ObjectHandle, Storage, UtcOffset,
};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::error::{Error, IoResultExt, MtpResultExt, PartialCleanup, Result};
use crate::remote_path::{
    RemoteEntry, RemotePath, as_parent, find_child, is_valid_entry_name, resolve_dir, resolve_entry,
};
use crate::storage::StorageEntry;

/// Bytes requested per download window.
///
/// The session is released between windows, so this is a trade-off between USB round-trip overhead
/// and how often another operation (or a cancel check) can slip in. 4 MiB is a few milliseconds of
/// USB 2.0 traffic and keeps memory flat.
pub const DOWNLOAD_WINDOW: u32 = 4 * 1024 * 1024;

/// Which way the bytes flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Device → host.
    Download,
    /// Host → device.
    Upload,
}

/// A progress sample, delivered to the caller's callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferProgress {
    /// Download or upload.
    pub direction: TransferDirection,
    /// Bytes transferred so far.
    pub transferred: u64,
    /// Total bytes, when known (it always is for these operations).
    pub total: Option<u64>,
}

impl TransferProgress {
    /// Completion between 0.0 and 1.0, or `None` when the total is unknown.
    pub fn fraction(&self) -> Option<f64> {
        match self.total {
            None => None,
            Some(0) => Some(1.0),
            Some(total) => Some((self.transferred as f64 / total as f64).clamp(0.0, 1.0)),
        }
    }

    /// Completion in percent, or `None` when the total is unknown.
    pub fn percent(&self) -> Option<f64> {
        self.fraction().map(|fraction| fraction * 100.0)
    }
}

/// Callback signature for progress updates.
///
/// A `&mut dyn FnMut` (instead of a generic) keeps the call sites simple and lets a TUI/GUI capture
/// a channel sender or a shared state handle.
pub type ProgressCallback<'a> = &'a mut (dyn FnMut(TransferProgress) + Send);

/// Options shared by downloads and uploads.
#[derive(Debug, Clone, Default)]
pub struct TransferOptions {
    /// Replace an existing local file / remote object.
    pub overwrite: bool,
    /// Cooperative cancellation, checked between download windows and upload chunks.
    pub cancel: Option<CancelToken>,
}

/// Result of a successful download.
#[derive(Debug, Clone)]
pub struct DownloadReport {
    /// Remote file that was read.
    pub remote: RemotePath,
    /// Local path that now holds the data.
    pub local: PathBuf,
    /// Number of bytes written.
    pub bytes: u64,
    /// Whether an existing local file was replaced.
    pub overwritten: bool,
    /// Modification time the device reported for the file.
    pub modified: Option<DateTime>,
}

/// Result of a successful upload.
#[derive(Debug, Clone)]
pub struct UploadReport {
    /// Local file that was sent.
    pub local: PathBuf,
    /// Remote object that was created.
    pub remote: RemotePath,
    /// Number of bytes sent.
    pub bytes: u64,
    /// Handle of the new remote object (session-scoped).
    pub handle: ObjectHandle,
    /// Whether an existing remote object was replaced.
    pub overwritten: bool,
    /// Size the device reports after a verification listing, if the re-listing worked.
    pub remote_size: Option<u64>,
}

impl UploadReport {
    /// Whether a re-listing confirmed the expected size.
    pub fn verified(&self) -> bool {
        self.remote_size == Some(self.bytes)
    }
}

/// Download one remote file to a local path.
///
/// The file is written to `<destination>.part` and renamed into place afterwards; on any error the
/// temporary file is removed. An existing destination is refused unless
/// [`TransferOptions::overwrite`] is set.
pub async fn download_file(
    storage: &Storage,
    remote: &RemotePath,
    destination: &Path,
    options: &TransferOptions,
    on_progress: ProgressCallback<'_>,
) -> Result<DownloadReport> {
    // Local checks first: a refused destination should not cost a USB round trip.
    check_local_destination(destination, options.overwrite)?;

    let resolved = resolve_entry(storage, remote, options.cancel.as_ref()).await?;
    if resolved.entry.is_dir() {
        return Err(Error::RemoteIsADirectory {
            path: remote.display(),
        });
    }

    let partial = partial_path(destination);
    tracing::debug!(
        remote = %remote.display(),
        local = %destination.display(),
        size = resolved.entry.size.unwrap_or_default(),
        storage = storage.id().0,
        "downloading"
    );

    let outcome = write_partial(
        storage,
        &resolved.entry,
        remote,
        &partial,
        options,
        on_progress,
    )
    .await;

    match outcome {
        Ok(bytes) => {
            // A second check narrows the window in which another process could create the
            // destination between our first check and the rename. It is not a full defence (a
            // race-free "create without replacing" needs platform-specific flags); for a
            // single-user CLI this is the documented behaviour.
            if !options.overwrite && destination.exists() {
                remove_file_quietly(&partial).await;
                return Err(Error::LocalAlreadyExists {
                    path: destination.to_path_buf(),
                });
            }
            move_into_place(&partial, destination)?;

            Ok(DownloadReport {
                remote: remote.clone(),
                local: destination.to_path_buf(),
                bytes,
                overwritten: options.overwrite,
                modified: resolved.entry.modified,
            })
        }
        Err(error) => {
            // MVP policy: a failed download leaves nothing behind. (Keeping the `.part` file for
            // resumption would need a `--resume` flag and explicit user consent.)
            remove_file_quietly(&partial).await;
            Err(error)
        }
    }
}

/// Upload one local file to a remote path.
///
/// The parent folder of `remote` must already exist. An existing remote object at `remote` is
/// refused unless [`TransferOptions::overwrite`] is set, in which case it is deleted first — MTP has
/// no atomic replace, so a failure in between leaves the target missing rather than partially
/// written.
pub async fn upload_file(
    storage: &Storage,
    local: &Path,
    remote: &RemotePath,
    options: &TransferOptions,
    on_progress: ProgressCallback<'_>,
) -> Result<UploadReport> {
    let storage_entry = StorageEntry::from_storage(storage);
    if !storage_entry.is_writable() {
        return Err(Error::StorageReadOnly {
            storage: storage_entry.label(),
        });
    }

    let metadata = tokio::fs::metadata(local).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            Error::LocalNotFound {
                path: local.to_path_buf(),
            }
        } else {
            Error::io(format!("inspecting `{}`", local.display()), error)
        }
    })?;
    if !metadata.is_file() {
        return Err(Error::LocalNotAFile {
            path: local.to_path_buf(),
        });
    }
    let size = metadata.len();

    let (parent_path, name) = remote.split_parent().ok_or_else(|| {
        Error::invalid_remote_path(
            remote.display(),
            "the target must be a file path inside the storage, not the storage root",
        )
    })?;
    validate_upload_name(remote, &name)?;

    let parent_handle = resolve_dir(storage, &parent_path, options.cancel.as_ref()).await?;

    // Collision handling before any bytes move.
    let existing = find_child(
        storage,
        parent_handle,
        &parent_path,
        &name,
        options.cancel.as_ref(),
    )
    .await?;
    if existing.as_ref().is_some_and(|entry| entry.is_dir()) {
        return Err(Error::RemoteIsADirectory {
            path: remote.display(),
        });
    }
    if existing.is_some() && !options.overwrite {
        return Err(Error::RemoteAlreadyExists {
            path: remote.display(),
        });
    }

    // Refuse early rather than half-way through the transfer when the device told us its limits.
    // Replacing an existing file frees its bytes first, so they count towards the budget.
    let reclaimable = existing
        .as_ref()
        .and_then(|entry| entry.size)
        .unwrap_or_default();
    ensure_capacity(&storage_entry, size, reclaimable)?;

    check_cancel(options.cancel.as_ref())?;

    let overwritten = match existing {
        Some(entry) => {
            tracing::debug!(handle = entry.handle.0, "replacing existing remote object");
            storage
                .delete(entry.handle)
                .await
                .mtp_context(format!("removing existing `{}`", remote.display()))?;
            true
        }
        None => false,
    };

    let mut object_info = NewObjectInfo::file(name.clone(), size);
    if let Some(modified) = local_modification_time(&metadata) {
        object_info = object_info.with_modified(modified);
    }

    tracing::debug!(
        local = %local.display(),
        remote = %remote.display(),
        size,
        storage = storage.id().0,
        "uploading"
    );

    let file = tokio::fs::File::open(local)
        .await
        .io_context(format!("opening `{}`", local.display()))?;
    let stream = ReaderStream::new(file);

    // The callback both reports progress and implements cancellation: `ControlFlow::Break` makes
    // `mtp-rs` stop the data phase (the object itself has already been created by then).
    let handle = match storage
        .upload_with_progress(
            as_parent(parent_handle),
            object_info,
            stream,
            |progress: mtp_rs::mtp::Progress| {
                if is_cancelled(options.cancel.as_ref()) {
                    return ControlFlow::Break(());
                }
                on_progress(TransferProgress {
                    direction: TransferDirection::Upload,
                    transferred: progress.bytes_transferred,
                    total: progress.total_bytes.or(Some(size)),
                });
                ControlFlow::Continue(())
            },
        )
        .await
    {
        Ok(handle) => handle,
        Err(error) => {
            // The device created an object before streaming the data. Removing *that* handle is
            // safe identification, not guessing by name.
            let cleanup = match error.partial {
                Some(partial) => remove_remote_partial(storage, partial).await,
                None => PartialCleanup::NothingToDo,
            };
            // A user cancel is not an upload failure; report it as such so the CLI exits with 130.
            if matches!(error.source, mtp_rs::Error::Cancelled) {
                return Err(Error::Cancelled);
            }
            return Err(Error::Upload {
                path: remote.display(),
                cleanup,
                source: error.source,
            });
        }
    };

    on_progress(TransferProgress {
        direction: TransferDirection::Upload,
        transferred: size,
        total: Some(size),
    });

    // Verification: best effort. A device that refuses the re-listing does not turn a successful
    // upload into a failure, it only leaves `remote_size` unknown.
    let remote_size = match find_child(
        storage,
        parent_handle,
        &parent_path,
        &name,
        options.cancel.as_ref(),
    )
    .await
    {
        Ok(Some(entry)) => entry.size,
        Ok(None) => {
            tracing::warn!(
                remote = %remote.display(),
                "upload reported success but the object is missing from the folder listing"
            );
            None
        }
        Err(error) => {
            tracing::warn!(
                remote = %remote.display(),
                %error,
                "could not verify the upload by re-listing the folder"
            );
            None
        }
    };

    Ok(UploadReport {
        local: local.to_path_buf(),
        remote: remote.clone(),
        bytes: size,
        handle,
        overwritten,
        remote_size,
    })
}

/// Create a remote folder.
///
/// Fails when the parent folder is missing (the MVP deliberately does not create intermediate
/// folders) or when the target already exists.
pub async fn create_dir(
    storage: &Storage,
    remote: &RemotePath,
    cancel: Option<&CancelToken>,
) -> Result<RemoteEntry> {
    let storage_entry = StorageEntry::from_storage(storage);
    if !storage_entry.is_writable() {
        return Err(Error::StorageReadOnly {
            storage: storage_entry.label(),
        });
    }

    let (parent_path, name) = remote.split_parent().ok_or_else(|| {
        Error::invalid_remote_path(
            remote.display(),
            "cannot create the storage root; it always exists",
        )
    })?;
    validate_upload_name(remote, &name)?;

    let parent_handle = resolve_dir(storage, &parent_path, cancel).await?;
    if find_child(storage, parent_handle, &parent_path, &name, cancel)
        .await?
        .is_some()
    {
        return Err(Error::RemoteAlreadyExists {
            path: remote.display(),
        });
    }

    let handle = storage
        .create_folder(as_parent(parent_handle), &name)
        .await
        .mtp_context(format!("creating folder `{}`", remote.display()))?;

    Ok(RemoteEntry {
        handle,
        name,
        kind: crate::remote_path::EntryKind::Directory,
        size: None,
        modified: None,
    })
}

/// Path of the temporary file used while downloading `destination`.
pub fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(".part");
    PathBuf::from(name)
}

/// Refuse a download destination that exists (without overwrite), is a folder, or lives in a
/// directory that does not exist.
fn check_local_destination(path: &Path, overwrite: bool) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.is_dir() {
                return Err(Error::LocalIsDirectory {
                    path: path.to_path_buf(),
                });
            }
            if !overwrite {
                return Err(Error::LocalAlreadyExists {
                    path: path.to_path_buf(),
                });
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                && !parent.is_dir()
            {
                return Err(Error::LocalDirectoryMissing {
                    path: parent.to_path_buf(),
                });
            }
        }
        Err(error) => {
            return Err(Error::io(format!("inspecting `{}`", path.display()), error));
        }
    }
    Ok(())
}

/// Stream the remote object into `partial` and return the number of bytes written.
async fn write_partial(
    storage: &Storage,
    entry: &RemoteEntry,
    remote: &RemotePath,
    partial: &Path,
    options: &TransferOptions,
    on_progress: ProgressCallback<'_>,
) -> Result<u64> {
    let mut download = storage
        .download_windowed(entry.handle, ByteRange::Full, DOWNLOAD_WINDOW)
        .await
        .mtp_context(format!("opening `{}` for reading", remote.display()))?;
    let total = download.size();

    let mut file = tokio::fs::File::create(partial)
        .await
        .io_context(format!("creating `{}`", partial.display()))?;
    let mut transferred = 0u64;
    on_progress(TransferProgress {
        direction: TransferDirection::Download,
        transferred,
        total: Some(total),
    });

    while let Some(window) = download.next_window().await {
        check_cancel(options.cancel.as_ref())?;
        let bytes = window.mtp_context(format!("reading `{}`", remote.display()))?;
        file.write_all(&bytes)
            .await
            .io_context(format!("writing `{}`", partial.display()))?;
        transferred += bytes.len() as u64;
        on_progress(TransferProgress {
            direction: TransferDirection::Download,
            transferred,
            total: Some(total),
        });
    }

    file.flush()
        .await
        .io_context(format!("flushing `{}`", partial.display()))?;
    file.sync_all()
        .await
        .io_context(format!("syncing `{}`", partial.display()))?;
    drop(file);

    if transferred != total {
        return Err(Error::IncompleteTransfer {
            path: remote.display(),
            transferred,
            expected: total,
        });
    }

    Ok(transferred)
}

/// Move the finished `.part` file onto the destination.
fn move_into_place(partial: &Path, destination: &Path) -> Result<()> {
    // On Unix `rename` replaces atomically; on Windows it fails if the target exists, which is why
    // overwriting removes the target first. `booxmtp` targets macOS/Linux, so the atomic path is the
    // normal one.
    if cfg!(windows) && destination.exists() {
        std::fs::remove_file(destination)
            .io_context(format!("removing `{}`", destination.display()))?;
    }
    std::fs::rename(partial, destination).io_context(format!(
        "moving `{}` to `{}`",
        partial.display(),
        destination.display()
    ))
}

/// Best-effort removal of a local temporary file; failures are only logged.
async fn remove_file_quietly(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => tracing::debug!(path = %path.display(), "removed temporary file"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "could not remove temporary file")
        }
    }
}

/// Remove the half-written object an upload left behind.
async fn remove_remote_partial(storage: &Storage, handle: ObjectHandle) -> PartialCleanup {
    match storage.delete(handle).await {
        Ok(()) => {
            tracing::debug!(handle = handle.0, "removed partial upload");
            PartialCleanup::Removed
        }
        Err(error) => {
            tracing::warn!(handle = handle.0, %error, "could not remove partial upload");
            PartialCleanup::Failed(error.to_string())
        }
    }
}

/// Local modification time as an MTP date.
///
/// `mtp-rs` documents that Android honours a UTC (`Z`) suffix on uploads, so an absolute instant is
/// sent rather than a zoneless local time.
fn local_modification_time(metadata: &std::fs::Metadata) -> Option<DateTime> {
    datetime_from_system_time(metadata.modified().ok()?)
}

/// Convert a host timestamp into an MTP datetime at UTC.
///
/// Useful for display and for uploads (`NewObjectInfo::with_modified`). Returns `None` for
/// timestamps outside the range MTP can express.
pub fn datetime_from_system_time(time: std::time::SystemTime) -> Option<DateTime> {
    let since_epoch = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(since_epoch.as_secs()).ok()?;
    DateTime::from_unix_seconds(seconds, UtcOffset::UTC)
}

/// Refuse an upload that cannot fit, using only capacity data the device actually reported.
fn ensure_capacity(storage: &StorageEntry, required: u64, reclaimable: u64) -> Result<()> {
    let Some(free) = storage.free_bytes else {
        // Unknown free space is not "zero free space": try the upload and let the device decide.
        return Ok(());
    };
    let available = free.saturating_add(reclaimable);
    if required > available {
        return Err(Error::InsufficientSpace {
            required,
            available,
            storage: storage.label(),
        });
    }
    Ok(())
}

/// Validate a name that is about to be created on the device.
fn validate_upload_name(path: &RemotePath, name: &str) -> Result<()> {
    if !is_valid_entry_name(name) {
        return Err(Error::invalid_remote_path(
            path.display(),
            "the file name is empty or contains `/`",
        ));
    }
    let units = name.encode_utf16().count();
    if units > MAX_NAME_UNITS {
        return Err(Error::invalid_remote_path(
            path.display(),
            format!(
                "name is {units} UTF-16 units long, the device accepts at most {MAX_NAME_UNITS}"
            ),
        ));
    }
    Ok(())
}

/// `Ok` when the operation may continue, [`Error::Cancelled`] when the user asked to stop.
fn check_cancel(cancel: Option<&CancelToken>) -> Result<()> {
    if is_cancelled(cancel) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn is_cancelled(cancel: Option<&CancelToken>) -> bool {
    cancel.is_some_and(CancelToken::is_cancelled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_path_keeps_the_original_extension() {
        let partial = partial_path(Path::new("/tmp/Roman.epub"));
        assert_eq!(partial, PathBuf::from("/tmp/Roman.epub.part"));
    }

    #[test]
    fn progress_fractions_cover_unknown_and_empty_totals() {
        let unknown = TransferProgress {
            direction: TransferDirection::Upload,
            transferred: 10,
            total: None,
        };
        assert_eq!(unknown.fraction(), None);

        let empty = TransferProgress {
            direction: TransferDirection::Download,
            transferred: 0,
            total: Some(0),
        };
        assert_eq!(empty.fraction(), Some(1.0));

        let half = TransferProgress {
            direction: TransferDirection::Download,
            transferred: 50,
            total: Some(100),
        };
        assert_eq!(half.percent(), Some(50.0));
    }

    #[test]
    fn capacity_checks_respect_unknown_and_reclaimable_space() {
        let unknown = StorageEntry::new(mtp_rs::mtp::StorageId(1), "volume");
        assert!(ensure_capacity(&unknown, u64::MAX, 0).is_ok());

        let roomy = StorageEntry::new(mtp_rs::mtp::StorageId(1), "volume").with_capacity(1000, 100);
        assert!(ensure_capacity(&roomy, 100, 0).is_ok());

        let error = ensure_capacity(&roomy, 101, 0).unwrap_err();
        assert!(matches!(
            error,
            Error::InsufficientSpace {
                required: 101,
                available: 100,
                ..
            }
        ));

        // Replacing a 60-byte file makes room for a 150-byte one on a 100-byte-free storage.
        assert!(ensure_capacity(&roomy, 150, 60).is_ok());
        assert!(ensure_capacity(&roomy, 161, 60).is_err());
    }

    #[test]
    fn local_destination_rules() {
        let directory = tempfile::tempdir().unwrap();
        let existing = directory.path().join("exists.epub");
        std::fs::write(&existing, b"x").unwrap();

        // Fresh path in an existing directory.
        assert!(check_local_destination(&directory.path().join("new.epub"), false).is_ok());
        // Existing file without overwrite.
        let error = check_local_destination(&existing, false).unwrap_err();
        assert!(matches!(error, Error::LocalAlreadyExists { .. }));
        // Existing file with overwrite.
        assert!(check_local_destination(&existing, true).is_ok());
        // Destination is a directory.
        let error = check_local_destination(directory.path(), true).unwrap_err();
        assert!(matches!(error, Error::LocalIsDirectory { .. }));
        // Parent directory does not exist.
        let error =
            check_local_destination(&directory.path().join("nope/x.epub"), false).unwrap_err();
        assert!(matches!(error, Error::LocalDirectoryMissing { .. }));
    }

    #[test]
    fn names_must_be_single_path_segments() {
        assert!(validate_upload_name(&RemotePath::root(), "Roman.epub").is_ok());
        assert!(validate_upload_name(&RemotePath::root(), "").is_err());
        assert!(validate_upload_name(&RemotePath::root(), "a/b").is_err());
        assert!(validate_upload_name(&RemotePath::root(), "..").is_err());
        let too_long = "x".repeat(MAX_NAME_UNITS + 1);
        assert!(validate_upload_name(&RemotePath::root(), &too_long).is_err());
    }
}
