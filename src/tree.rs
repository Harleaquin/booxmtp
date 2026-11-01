//! Recursive copy, move and delete across host folders and MTP storages.
//!
//! The TUI (and any future frontend) hands in two [`Location`]s and gets a summary back. The
//! module walks folder trees itself, because MTP has no server-side recursive copy:
//!
//! * **Copy** streams every file through the existing single-file transfer code, so memory stays
//!   flat and progress, `.part` handling and collision rules are shared with the CLI.
//! * **Move** copies first and only deletes the source after the copy fully succeeded. For
//!   host-to-host moves on one filesystem it uses `rename`, which is atomic and instant.
//! * **Delete** walks depth-first, deletes the children and then the folder itself, and asks no
//!   questions — confirmation is the caller's job.
//!
//! Merging is allowed: copying a folder onto an existing folder adds/replaces the contained files
//! instead of failing. Copying a *file* onto a folder (or the reverse) needs
//! [`TransferOptions::overwrite`] and then replaces the entry.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use async_recursion::async_recursion;
use mtp_rs::CancelToken;
use mtp_rs::mtp::StorageId;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::error::{Error, IoResultExt, MtpResultExt, Result};
use crate::location::Location;
use crate::remote_path::{RemotePath, resolve_entry};
use crate::session::Session;
use crate::transfer::{self, TransferOptions, TransferProgress};

/// Chunk size for host-to-host copies.
const LOCAL_COPY_CHUNK: usize = 256 * 1024;
/// How often progress is reported for host-to-host copies.
const LOCAL_PROGRESS_INTERVAL: Duration = Duration::from_millis(80);

/// Callback for per-file progress inside a recursive operation.
///
/// The first argument is the file's path relative to the top-level item (e.g. `Books/Dune.epub`),
/// so a UI can show which file of a tree is currently moving.
pub type FileProgressCallback<'a> = &'a mut (dyn FnMut(&str, TransferProgress) + Send);

/// What a copy or move produced.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CopySummary {
    /// Number of files copied.
    pub files: u64,
    /// Number of folders created.
    pub dirs: u64,
    /// Number of bytes copied.
    pub bytes: u64,
}

impl CopySummary {
    /// A short human-readable description, e.g. `3 files, 1 folder, 4.2 MiB`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.files > 0 {
            parts.push(count(self.files, "file", "files"));
        }
        if self.dirs > 0 {
            parts.push(count(self.dirs, "folder", "folders"));
        }
        if self.bytes > 0 {
            parts.push(crate::output::format_bytes(self.bytes));
        }
        if parts.is_empty() {
            "nothing".to_string()
        } else {
            parts.join(", ")
        }
    }
}

impl std::ops::AddAssign for CopySummary {
    fn add_assign(&mut self, other: Self) {
        self.files += other.files;
        self.dirs += other.dirs;
        self.bytes += other.bytes;
    }
}

/// What a delete removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeleteSummary {
    /// Number of files removed.
    pub files: u64,
    /// Number of folders removed (including the top-level one).
    pub dirs: u64,
}

impl DeleteSummary {
    /// A short human-readable description, e.g. `3 files, 1 folder`.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if self.files > 0 {
            parts.push(count(self.files, "file", "files"));
        }
        if self.dirs > 0 {
            parts.push(count(self.dirs, "folder", "folders"));
        }
        if parts.is_empty() {
            "nothing".to_string()
        } else {
            parts.join(", ")
        }
    }
}

impl std::ops::AddAssign for DeleteSummary {
    fn add_assign(&mut self, other: Self) {
        self.files += other.files;
        self.dirs += other.dirs;
    }
}

/// Copy a file or a whole folder tree.
///
/// `destination` is the final path, not a folder that should receive the item. The caller derives
/// it from the destination panel plus the source name.
pub async fn copy(
    session: &Session,
    source: &Location,
    destination: &Location,
    options: &TransferOptions,
    on_progress: FileProgressCallback<'_>,
) -> Result<CopySummary> {
    match (source, destination) {
        (Location::Local(from), Location::Local(to)) => {
            let label = file_label(from);
            copy_local(from, to, &label, options, on_progress).await
        }
        (Location::Local(from), Location::Remote { storage, path }) => {
            let label = file_label(from);
            copy_to_remote(
                session,
                storage.id,
                from,
                path,
                &label,
                options,
                on_progress,
            )
            .await
        }
        (Location::Remote { storage, path }, Location::Local(to)) => {
            let label = file_label(Path::new(&path.display()));
            copy_to_local(session, storage.id, path, to, &label, options, on_progress).await
        }
        (Location::Remote { .. }, Location::Remote { .. }) => Err(Error::UnsupportedByApp {
            what: "copying between two remote folders is not implemented yet".to_string(),
        }),
    }
}

/// Move a file or a whole folder tree.
///
/// Copies first and deletes the source only when the copy succeeded, so an interrupted move never
/// loses data. Host-to-host moves use `rename` when the destination does not exist yet.
pub async fn move_entry(
    session: &Session,
    source: &Location,
    destination: &Location,
    options: &TransferOptions,
    on_progress: FileProgressCallback<'_>,
) -> Result<CopySummary> {
    if let (Location::Local(from), Location::Local(to)) = (source, destination)
        && !to.exists()
    {
        let summary = measure_local(from)?;
        if tokio::fs::rename(from, to).await.is_ok() {
            tracing::debug!(from = %from.display(), to = %to.display(), "moved by rename");
            return Ok(summary);
        }
        // Different filesystem: fall through to copy + delete.
    }

    let summary = copy(session, source, destination, options, on_progress).await?;
    remove(session, source, options.cancel.as_ref()).await?;
    Ok(summary)
}

/// Delete a file or a folder tree. Confirmation belongs to the caller.
pub async fn remove(
    session: &Session,
    target: &Location,
    cancel: Option<&CancelToken>,
) -> Result<DeleteSummary> {
    match target {
        Location::Local(path) => remove_local(path).await,
        Location::Remote { storage, path } => {
            remove_remote(session, storage.id, path, cancel).await
        }
    }
}

// ---------------------------------------------------------------------------
// Host -> host
// ---------------------------------------------------------------------------

#[async_recursion]
async fn copy_local(
    from: &Path,
    to: &Path,
    relative: &str,
    options: &TransferOptions,
    on_progress: &mut (dyn FnMut(&str, TransferProgress) + Send),
) -> Result<CopySummary> {
    check_cancel(options.cancel.as_ref())?;
    let metadata = tokio::fs::symlink_metadata(from)
        .await
        .map_err(|error| local_metadata_error(from, error))?;

    if metadata.is_dir() {
        ensure_local_dir(to, options.overwrite).await?;
        let mut summary = CopySummary {
            dirs: 1,
            ..CopySummary::default()
        };
        for child in sorted_children(from)? {
            let name = child_name(&child);
            let child_relative = format!("{relative}/{name}");
            summary += copy_local(
                &child,
                &to.join(&name),
                &child_relative,
                options,
                on_progress,
            )
            .await?;
        }
        Ok(summary)
    } else if metadata.is_file() {
        let bytes = copy_local_file(from, to, relative, options, on_progress).await?;
        Ok(CopySummary {
            files: 1,
            dirs: 0,
            bytes,
        })
    } else {
        Err(Error::LocalNotAFile {
            path: from.to_path_buf(),
        })
    }
}

/// Create a host directory, merging into an existing one.
async fn ensure_local_dir(path: &Path, overwrite: bool) -> Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) if overwrite => {
            tokio::fs::remove_file(path)
                .await
                .io_context(format!("replacing `{}`", path.display()))?;
            tokio::fs::create_dir_all(path)
                .await
                .io_context(format!("creating `{}`", path.display()))
        }
        Ok(_) => Err(Error::LocalAlreadyExists {
            path: path.to_path_buf(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tokio::fs::create_dir_all(path)
                .await
                .io_context(format!("creating `{}`", path.display()))
        }
        Err(error) => Err(Error::io(format!("inspecting `{}`", path.display()), error)),
    }
}

/// Copy one host file through `<destination>.part` and report progress in chunks.
async fn copy_local_file(
    from: &Path,
    to: &Path,
    relative: &str,
    options: &TransferOptions,
    on_progress: &mut (dyn FnMut(&str, TransferProgress) + Send),
) -> Result<u64> {
    if let Ok(metadata) = tokio::fs::symlink_metadata(to).await {
        if metadata.is_dir() {
            return Err(Error::LocalIsDirectory {
                path: to.to_path_buf(),
            });
        }
        if !options.overwrite {
            return Err(Error::LocalAlreadyExists {
                path: to.to_path_buf(),
            });
        }
    }

    let partial = transfer::partial_path(to);
    let outcome = stream_local_copy(from, &partial, relative, options, on_progress).await;

    match outcome {
        Ok(bytes) => {
            if !options.overwrite && to.exists() {
                remove_file_quietly(&partial).await;
                return Err(Error::LocalAlreadyExists {
                    path: to.to_path_buf(),
                });
            }
            std::fs::rename(&partial, to).io_context(format!(
                "moving `{}` to `{}`",
                partial.display(),
                to.display()
            ))?;
            Ok(bytes)
        }
        Err(error) => {
            remove_file_quietly(&partial).await;
            Err(error)
        }
    }
}

async fn stream_local_copy(
    from: &Path,
    partial: &Path,
    relative: &str,
    options: &TransferOptions,
    on_progress: &mut (dyn FnMut(&str, TransferProgress) + Send),
) -> Result<u64> {
    let mut source = tokio::fs::File::open(from)
        .await
        .io_context(format!("opening `{}`", from.display()))?;
    let total = source.metadata().await.ok().map(|metadata| metadata.len());
    let mut sink = tokio::fs::File::create(partial)
        .await
        .io_context(format!("creating `{}`", partial.display()))?;

    let mut buffer = vec![0u8; LOCAL_COPY_CHUNK];
    let mut transferred = 0u64;
    let mut last_emit = Instant::now() - LOCAL_PROGRESS_INTERVAL;

    loop {
        check_cancel(options.cancel.as_ref())?;
        let read = source
            .read(&mut buffer)
            .await
            .io_context(format!("reading `{}`", from.display()))?;
        if read == 0 {
            break;
        }
        sink.write_all(&buffer[..read])
            .await
            .io_context(format!("writing `{}`", partial.display()))?;
        transferred += read as u64;
        if last_emit.elapsed() >= LOCAL_PROGRESS_INTERVAL {
            last_emit = Instant::now();
            on_progress(
                relative,
                TransferProgress {
                    direction: transfer::TransferDirection::Download,
                    transferred,
                    total,
                },
            );
        }
    }

    sink.flush()
        .await
        .io_context(format!("flushing `{}`", partial.display()))?;
    drop(sink);
    on_progress(
        relative,
        TransferProgress {
            direction: transfer::TransferDirection::Download,
            transferred,
            total,
        },
    );
    Ok(transferred)
}

// ---------------------------------------------------------------------------
// Host -> reader
// ---------------------------------------------------------------------------

#[async_recursion]
async fn copy_to_remote(
    session: &Session,
    storage_id: StorageId,
    from: &Path,
    to: &RemotePath,
    relative: &str,
    options: &TransferOptions,
    on_progress: &mut (dyn FnMut(&str, TransferProgress) + Send),
) -> Result<CopySummary> {
    check_cancel(options.cancel.as_ref())?;
    let metadata = tokio::fs::metadata(from)
        .await
        .map_err(|error| local_metadata_error(from, error))?;

    if metadata.is_dir() {
        ensure_remote_dir(session, storage_id, to, options).await?;
        let mut summary = CopySummary {
            dirs: 1,
            ..CopySummary::default()
        };
        for child in sorted_children(from)? {
            let name = child_name(&child);
            let child_to = to.join(&name)?;
            let child_relative = format!("{relative}/{name}");
            summary += copy_to_remote(
                session,
                storage_id,
                &child,
                &child_to,
                &child_relative,
                options,
                on_progress,
            )
            .await?;
        }
        Ok(summary)
    } else if metadata.is_file() {
        let storage = session.storage(storage_id)?;
        let mut report_progress = |progress: TransferProgress| on_progress(relative, progress);
        let report =
            transfer::upload_file(storage, from, to, options, &mut report_progress).await?;
        Ok(CopySummary {
            files: 1,
            dirs: 0,
            bytes: report.bytes,
        })
    } else {
        Err(Error::LocalNotAFile {
            path: from.to_path_buf(),
        })
    }
}

/// Create a remote directory, merging into an existing one.
async fn ensure_remote_dir(
    session: &Session,
    storage_id: StorageId,
    path: &RemotePath,
    options: &TransferOptions,
) -> Result<()> {
    let storage = session.storage(storage_id)?;
    match resolve_entry(storage, path, options.cancel.as_ref()).await {
        Ok(resolved) if resolved.entry.is_dir() => Ok(()),
        Ok(resolved) if options.overwrite => {
            storage
                .delete(resolved.entry.handle)
                .await
                .mtp_context(format!("replacing `{}`", path.display()))?;
            session
                .create_dir(storage_id, path, options.cancel.as_ref())
                .await?;
            Ok(())
        }
        Ok(_) => Err(Error::RemoteAlreadyExists {
            path: path.display(),
        }),
        Err(Error::RemotePathNotFound { .. }) => {
            session
                .create_dir(storage_id, path, options.cancel.as_ref())
                .await?;
            Ok(())
        }
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// Reader -> host
// ---------------------------------------------------------------------------

#[async_recursion]
async fn copy_to_local(
    session: &Session,
    storage_id: StorageId,
    from: &RemotePath,
    to: &Path,
    relative: &str,
    options: &TransferOptions,
    on_progress: &mut (dyn FnMut(&str, TransferProgress) + Send),
) -> Result<CopySummary> {
    check_cancel(options.cancel.as_ref())?;
    let storage = session.storage(storage_id)?;
    let resolved = resolve_entry(storage, from, options.cancel.as_ref()).await?;

    if resolved.entry.is_dir() {
        ensure_local_dir(to, options.overwrite).await?;
        let listing = session
            .list_dir(storage_id, from, options.cancel.as_ref())
            .await?;
        let mut summary = CopySummary {
            dirs: 1,
            ..CopySummary::default()
        };
        for entry in &listing.entries {
            let child_from = from.join(&entry.name)?;
            let child_to = to.join(&entry.name);
            let child_relative = format!("{relative}/{}", entry.name);
            summary += copy_to_local(
                session,
                storage_id,
                &child_from,
                &child_to,
                &child_relative,
                options,
                on_progress,
            )
            .await?;
        }
        Ok(summary)
    } else {
        let mut report_progress = |progress: TransferProgress| on_progress(relative, progress);
        let report =
            transfer::download_file(storage, from, to, options, &mut report_progress).await?;
        Ok(CopySummary {
            files: 1,
            dirs: 0,
            bytes: report.bytes,
        })
    }
}

// ---------------------------------------------------------------------------
// Deleting
// ---------------------------------------------------------------------------

async fn remove_local(path: &Path) -> Result<DeleteSummary> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| local_metadata_error(path, error))?;

    if metadata.is_dir() {
        let summary = measure_local(path)?;
        tokio::fs::remove_dir_all(path)
            .await
            .io_context(format!("deleting `{}`", path.display()))?;
        Ok(DeleteSummary {
            files: summary.files,
            dirs: summary.dirs,
        })
    } else {
        tokio::fs::remove_file(path)
            .await
            .io_context(format!("deleting `{}`", path.display()))?;
        Ok(DeleteSummary { files: 1, dirs: 0 })
    }
}

#[async_recursion]
async fn remove_remote(
    session: &Session,
    storage_id: StorageId,
    path: &RemotePath,
    cancel: Option<&CancelToken>,
) -> Result<DeleteSummary> {
    check_cancel(cancel)?;
    let storage = session.storage(storage_id)?;
    let resolved = resolve_entry(storage, path, cancel).await?;

    if resolved.entry.is_dir() {
        let listing = session.list_dir(storage_id, path, cancel).await?;
        let mut summary = DeleteSummary { files: 0, dirs: 1 };
        for entry in &listing.entries {
            let child = path.join(&entry.name)?;
            summary += remove_remote(session, storage_id, &child, cancel).await?;
        }
        // Deleting children can re-key handles on Android, so resolve the folder again.
        let resolved = resolve_entry(storage, path, cancel).await?;
        storage
            .delete(resolved.entry.handle)
            .await
            .mtp_context(format!("deleting `{}`", path.display()))?;
        Ok(summary)
    } else {
        storage
            .delete(resolved.entry.handle)
            .await
            .mtp_context(format!("deleting `{}`", path.display()))?;
        Ok(DeleteSummary { files: 1, dirs: 0 })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Count files, folders and bytes of a host tree without reading any content.
fn measure_local(root: &Path) -> Result<CopySummary> {
    let metadata = std::fs::symlink_metadata(root)
        .map_err(|error| Error::io(format!("inspecting `{}`", root.display()), error))?;
    let mut summary = CopySummary::default();
    if metadata.is_dir() {
        summary.dirs += 1;
        for child in sorted_children(root)? {
            summary += measure_local(&child)?;
        }
    } else if metadata.is_file() {
        summary.files += 1;
        summary.bytes += metadata.len();
    }
    Ok(summary)
}

/// Direct children of `folder`, sorted by name so output and progress are reproducible.
fn sorted_children(folder: &Path) -> Result<Vec<PathBuf>> {
    let mut children = Vec::new();
    let entries = std::fs::read_dir(folder)
        .map_err(|error| Error::io(format!("reading `{}`", folder.display()), error))?;
    for entry in entries {
        let entry =
            entry.map_err(|error| Error::io(format!("reading `{}`", folder.display()), error))?;
        children.push(entry.path());
    }
    children.sort();
    Ok(children)
}

fn child_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn file_label(path: &Path) -> String {
    child_name(path)
}

fn local_metadata_error(path: &Path, error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::NotFound {
        Error::LocalNotFound {
            path: path.to_path_buf(),
        }
    } else {
        Error::io(format!("inspecting `{}`", path.display()), error)
    }
}

async fn remove_file_quietly(path: &Path) {
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "could not remove temporary file")
        }
    }
}

fn check_cancel(cancel: Option<&CancelToken>) -> Result<()> {
    if cancel.is_some_and(CancelToken::is_cancelled) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn count(value: u64, singular: &str, plural: &str) -> String {
    if value == 1 {
        format!("1 {singular}")
    } else {
        format!("{value} {plural}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summaries_are_human_readable() {
        let file = CopySummary {
            files: 1,
            dirs: 0,
            bytes: 1024,
        };
        assert_eq!(file.describe(), "1 file, 1.0 KiB");

        let tree = CopySummary {
            files: 3,
            dirs: 2,
            bytes: 0,
        };
        assert_eq!(tree.describe(), "3 files, 2 folders");

        assert_eq!(CopySummary::default().describe(), "nothing");
        assert_eq!(
            CopySummary {
                files: 0,
                dirs: 1,
                bytes: 0
            }
            .describe(),
            "1 folder",
            "an empty folder still says what was copied"
        );
        assert_eq!(
            DeleteSummary { files: 2, dirs: 1 }.describe(),
            "2 files, 1 folder"
        );
    }

    #[test]
    fn measuring_a_local_tree_counts_everything() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("a.txt"), b"12345").unwrap();
        std::fs::write(dir.path().join("sub/b.txt"), b"123").unwrap();

        let summary = measure_local(dir.path()).unwrap();
        assert_eq!(summary.files, 2);
        assert_eq!(summary.dirs, 2);
        assert_eq!(summary.bytes, 8);
    }
}
