//! Remote (MTP object) paths and path resolution.
//!
//! Remote paths are **MTP object paths relative to one storage root**, not host filesystem paths.
//! `/` is the root of the selected storage, `/Books/Roman.epub` is a file inside it. Paths are
//! walked *segment by segment* through the folder objects the device reports, because MTP has no
//! “open path” call: every step is a folder listing.
//!
//! Resolution is generic over [`RemoteFs`], a tiny trait with one operation (list the children of a
//! folder). That keeps the interesting logic — normalisation, segment walking, case handling —
//! testable without USB hardware, and lets a future TUI/GUI reuse it for previews and navigation.
//!
//! # Rules
//!
//! * A leading `/` is optional; it always means “this storage's root”.
//! * Empty segments and `.` are ignored, so `//Books//./x.epub` equals `/Books/x.epub`.
//! * `..` is rejected outright. A remote path can never leave its storage.
//! * Names are matched exactly first, then case-insensitively **if that is unambiguous**. MTP
//!   storages are often case-preserving but case-insensitive, so `ls /books` finds `/Books`.
//! * Symlinks are not modelled; MTP has no such concept.

use std::fmt;

use mtp_rs::CancelToken;
use mtp_rs::mtp::{DateTime, ObjectHandle, ObjectInfo, Storage};

use crate::error::{Error, Result};

/// Whether a remote entry is a folder or a file.
/// Variant order matters: sorting by this enum puts folders before files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EntryKind {
    /// A folder (MTP “association”).
    Directory,
    /// A regular file.
    File,
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EntryKind::Directory => "dir",
            EntryKind::File => "file",
        })
    }
}

/// One object inside a folder, in backend-neutral form.
///
/// `handle` is only meaningful within the current device session — never persist or reuse it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEntry {
    /// Opaque, session-scoped MTP object handle.
    pub handle: ObjectHandle,
    /// File or folder name as the device reports it.
    pub name: String,
    /// Folder or file.
    pub kind: EntryKind,
    /// Size in bytes for files; `None` for folders.
    pub size: Option<u64>,
    /// Modification time, if the device reported a valid one.
    pub modified: Option<DateTime>,
}

impl RemoteEntry {
    /// Convert from the `mtp-rs` object description.
    pub fn from_object_info(info: &ObjectInfo) -> Self {
        let kind = if info.is_folder() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        Self {
            handle: info.handle,
            name: info.filename.clone(),
            kind,
            size: (kind == EntryKind::File).then_some(info.size),
            modified: info.modified,
        }
    }

    /// Whether this entry is a folder.
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }
}

/// Result of listing one folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Children {
    /// Objects whose metadata the device returned.
    pub entries: Vec<RemoteEntry>,
    /// Handles the device refused to describe. Not fatal, but worth surfacing: the folder may
    /// contain more objects than [`entries`](Self::entries) shows.
    pub unreadable: Vec<ObjectHandle>,
}

/// The minimal view of a device the path resolver needs.
///
/// `mtp-rs`'s [`Storage`] implements it against real hardware; tests implement it against an
/// in-memory tree.
pub trait RemoteFs {
    /// List the direct children of `parent` (`None` means the storage root).
    fn list_children(
        &self,
        parent: Option<ObjectHandle>,
        cancel: Option<&CancelToken>,
    ) -> impl Future<Output = Result<Children>> + Send;
}

impl RemoteFs for Storage {
    async fn list_children(
        &self,
        parent: Option<ObjectHandle>,
        cancel: Option<&CancelToken>,
    ) -> Result<Children> {
        // `collect_objects` keeps the handles the device refuses to describe instead of silently
        // dropping them, so a listing can say "and one entry we could not read".
        let collection = self
            .collect_objects_with_cancel(parent, cancel)
            .await
            .map_err(Error::from)?;
        Ok(Children {
            entries: collection
                .objects
                .iter()
                .map(RemoteEntry::from_object_info)
                .collect(),
            unreadable: collection.skipped.iter().map(|item| item.handle).collect(),
        })
    }
}

/// A normalised remote path.
///
/// Construct with [`RemotePath::parse`]; it is guaranteed to contain only real name segments
/// (never empty, `.` or `..`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RemotePath {
    segments: Vec<String>,
}

impl RemotePath {
    /// The storage root (`/`).
    pub fn root() -> Self {
        Self::default()
    }

    /// Parse and normalise a user-provided remote path.
    ///
    /// Accepts an optional leading `/`; rejects `..` and embedded NUL bytes.
    pub fn parse(raw: &str) -> Result<Self> {
        let mut segments = Vec::new();
        for segment in raw.split('/') {
            match segment {
                "" | "." => continue,
                ".." => {
                    return Err(Error::invalid_remote_path(
                        raw,
                        "`..` is not allowed: a remote path cannot leave its storage",
                    ));
                }
                _ => {
                    if segment.contains('\0') {
                        return Err(Error::invalid_remote_path(
                            raw,
                            "names must not contain NUL bytes",
                        ));
                    }
                    segments.push(segment.to_string());
                }
            }
        }
        Ok(Self { segments })
    }

    /// Whether this is the storage root.
    pub fn is_root(&self) -> bool {
        self.segments.is_empty()
    }

    /// The individual name segments, root first.
    pub fn segments(&self) -> &[String] {
        &self.segments
    }

    /// Append a single, already validated name segment.
    pub fn join(&self, name: &str) -> Result<Self> {
        if !is_valid_entry_name(name) {
            return Err(Error::invalid_remote_path(
                self.display(),
                format!("`{name}` is not a valid file or folder name"),
            ));
        }
        let mut segments = self.segments.clone();
        segments.push(name.to_string());
        Ok(Self { segments })
    }

    /// The parent path, or `None` for the root.
    pub fn parent(&self) -> Option<Self> {
        (!self.is_root()).then(|| Self {
            segments: self.segments[..self.segments.len() - 1].to_vec(),
        })
    }

    /// The last name segment, or `None` for the root.
    pub fn file_name(&self) -> Option<&str> {
        self.segments.last().map(String::as_str)
    }

    /// Split into “containing folder” and “entry name”.
    ///
    /// Returns `None` for the storage root: it has no parent and no name.
    pub fn split_parent(&self) -> Option<(Self, String)> {
        let (name, parent_segments) = self.segments.split_last()?;
        Some((
            Self {
                segments: parent_segments.to_vec(),
            },
            name.clone(),
        ))
    }

    /// Render as an absolute-looking path: `/` for the root, `/a/b` otherwise.
    pub fn display(&self) -> String {
        if self.is_root() {
            return "/".to_string();
        }
        let mut rendered =
            String::with_capacity(self.segments.iter().map(String::len).sum::<usize>() + 1);
        for segment in &self.segments {
            rendered.push('/');
            rendered.push_str(segment);
        }
        rendered
    }
}

impl fmt::Display for RemotePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.display())
    }
}

impl std::str::FromStr for RemotePath {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self> {
        Self::parse(raw)
    }
}

/// A resolved file or folder plus its location.
#[derive(Debug, Clone)]
pub struct ResolvedEntry {
    /// The full path that was resolved.
    pub path: RemotePath,
    /// The containing folder path.
    pub parent: RemotePath,
    /// Handle of the containing folder (`ObjectHandle::ROOT` at the top level).
    pub parent_handle: ObjectHandle,
    /// The object itself.
    pub entry: RemoteEntry,
}

/// Contents of one remote folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirListing {
    /// The folder that was read.
    pub path: RemotePath,
    /// Its entries, folders first, then files, each sorted by name.
    pub entries: Vec<RemoteEntry>,
    /// Handles the device could not describe.
    pub unreadable: Vec<ObjectHandle>,
}

impl DirListing {
    /// Number of entries that are folders.
    pub fn directory_count(&self) -> usize {
        self.entries.iter().filter(|entry| entry.is_dir()).count()
    }

    /// Number of entries that are files.
    pub fn file_count(&self) -> usize {
        self.entries.len() - self.directory_count()
    }
}

/// Translate a resolved object handle into the `Option` form the `mtp-rs` calls expect.
pub fn as_parent(handle: ObjectHandle) -> Option<ObjectHandle> {
    (handle != ObjectHandle::ROOT).then_some(handle)
}

/// Whether `name` can be used as a single remote path segment or upload file name.
pub fn is_valid_entry_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

/// Resolve a folder path to its object handle.
///
/// The root resolves to [`ObjectHandle::ROOT`] without talking to the device.
pub async fn resolve_dir<F: RemoteFs + ?Sized>(
    fs: &F,
    path: &RemotePath,
    cancel: Option<&CancelToken>,
) -> Result<ObjectHandle> {
    let mut handle = ObjectHandle::ROOT;
    let mut walked = RemotePath::root();

    for segment in path.segments() {
        walked = walked.join(segment)?;
        let children = children_of(fs, handle, &walked, cancel).await?;
        match find_in(&children.entries, segment) {
            Some(entry) if entry.is_dir() => handle = entry.handle,
            Some(_) => {
                return Err(Error::RemoteNotADirectory {
                    path: walked.display(),
                });
            }
            None => {
                return Err(Error::RemotePathNotFound {
                    path: path.display(),
                });
            }
        }
    }

    Ok(handle)
}

/// Resolve a path to a single entry, returning its handle together with its parent folder.
pub async fn resolve_entry<F: RemoteFs + ?Sized>(
    fs: &F,
    path: &RemotePath,
    cancel: Option<&CancelToken>,
) -> Result<ResolvedEntry> {
    let (parent_path, name) = path.split_parent().ok_or_else(|| {
        Error::invalid_remote_path(path.display(), "the storage root is a folder, not a file")
    })?;

    let parent_handle = resolve_dir(fs, &parent_path, cancel).await?;
    let entry = find_child(fs, parent_handle, &parent_path, &name, cancel)
        .await?
        .ok_or_else(|| Error::RemotePathNotFound {
            path: path.display(),
        })?;

    Ok(ResolvedEntry {
        path: path.clone(),
        parent: parent_path,
        parent_handle,
        entry,
    })
}

/// Read a remote folder's direct children.
///
/// Directories come first, then files, each group sorted case-insensitively by name.
pub async fn read_dir<F: RemoteFs + ?Sized>(
    fs: &F,
    path: &RemotePath,
    cancel: Option<&CancelToken>,
) -> Result<DirListing> {
    let handle = resolve_dir(fs, path, cancel).await?;
    let mut children = children_of(fs, handle, path, cancel).await?;
    sort_entries(&mut children.entries);

    Ok(DirListing {
        path: path.clone(),
        entries: children.entries,
        unreadable: children.unreadable,
    })
}

/// Look a name up in one folder.
///
/// Exact matches win. A unique case-insensitive match is accepted (MTP storages are commonly
/// case-insensitive); if several names differ only in case, the lookup reports “not found” rather
/// than guessing.
pub async fn find_child<F: RemoteFs + ?Sized>(
    fs: &F,
    parent_handle: ObjectHandle,
    parent_path: &RemotePath,
    name: &str,
    cancel: Option<&CancelToken>,
) -> Result<Option<RemoteEntry>> {
    let children = children_of(fs, parent_handle, parent_path, cancel).await?;
    Ok(find_in(&children.entries, name).cloned())
}

/// Find an entry in an already-loaded folder listing.
pub fn find_in<'a>(entries: &'a [RemoteEntry], name: &str) -> Option<&'a RemoteEntry> {
    if let Some(exact) = entries.iter().find(|entry| entry.name == name) {
        return Some(exact);
    }

    let mut case_insensitive = entries
        .iter()
        .filter(|entry| entry.name.eq_ignore_ascii_case(name));
    match (case_insensitive.next(), case_insensitive.next()) {
        (Some(unique), None) => {
            tracing::debug!(
                requested = name,
                matched = %unique.name,
                "matched a name case-insensitively"
            );
            Some(unique)
        }
        _ => None,
    }
}

/// List one folder, adding the path to transport errors.
async fn children_of<F: RemoteFs + ?Sized>(
    fs: &F,
    handle: ObjectHandle,
    path: &RemotePath,
    cancel: Option<&CancelToken>,
) -> Result<Children> {
    tracing::trace!(path = %path.display(), handle = handle.0, "listing folder");
    fs.list_children(as_parent(handle), cancel)
        .await
        .map_err(|error| error.contextualize(format!("listing `{}`", path.display())))
}

/// Folders first, then files, each sorted by name (case-insensitive, ties broken by the raw name).
fn sort_entries(entries: &mut [RemoteEntry]) {
    entries.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            .then_with(|| left.name.cmp(&right.name))
    });
}
