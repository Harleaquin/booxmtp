//! What a panel can show: local folders and MTP storages, unified into one list model.
//!
//! A panel points at a [`Location`] (either a host directory or a storage + remote path) and
//! displays a list of [`FileEntry`] rows. Both sources are normalised into the same shape so the
//! UI, the key handling and the copy logic do not care where an entry comes from.

use std::path::{Path, PathBuf};

use mtp_rs::mtp::DateTime;

use crate::remote_path::{EntryKind, RemoteEntry, RemotePath};
use crate::storage::{StorageAlias, StorageEntry};
use crate::transfer::datetime_from_system_time;

/// Identifies one of the two panels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    /// The left panel.
    Left,
    /// The right panel.
    Right,
}

impl PanelId {
    /// Both panels, left to right.
    pub const ALL: [PanelId; 2] = [PanelId::Left, PanelId::Right];

    /// The panel on the other side.
    pub fn other(self) -> Self {
        match self {
            PanelId::Left => PanelId::Right,
            PanelId::Right => PanelId::Left,
        }
    }

    /// Index into `[Panel; 2]`.
    pub fn index(self) -> usize {
        match self {
            PanelId::Left => 0,
            PanelId::Right => 1,
        }
    }
}

impl std::fmt::Display for PanelId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PanelId::Left => "left",
            PanelId::Right => "right",
        })
    }
}

/// What a panel is looking at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A directory on the host.
    Local(PathBuf),
    /// A folder inside one of the device's storages.
    Remote {
        /// Storage metadata (including the resolved alias and free space).
        storage: StorageEntry,
        /// Folder path relative to the storage root.
        path: RemotePath,
    },
}

impl Location {
    /// A host directory.
    pub fn local(path: impl Into<PathBuf>) -> Self {
        Location::Local(path.into())
    }

    /// A folder on the device.
    pub fn remote(storage: StorageEntry, path: RemotePath) -> Self {
        Location::Remote { storage, path }
    }

    /// Whether this location lives on the reader.
    pub fn is_remote(&self) -> bool {
        matches!(self, Location::Remote { .. })
    }

    /// The storage metadata for remote locations.
    pub fn storage(&self) -> Option<&StorageEntry> {
        match self {
            Location::Remote { storage, .. } => Some(storage),
            Location::Local(_) => None,
        }
    }

    /// The path part only, as a display string (`/Books` or `/Users/me/Books`).
    pub fn display_path(&self) -> String {
        match self {
            Location::Local(path) => path.display().to_string(),
            Location::Remote { path, .. } => path.display(),
        }
    }

    /// A compact label for the panel title: `Local`, or `internal:/Books` for the device.
    pub fn title(&self) -> String {
        match self {
            Location::Local(path) => format!("Local: {}", display_local_path(path)),
            Location::Remote { storage, path } => {
                let label = storage
                    .alias()
                    .map(StorageAlias::as_str)
                    .unwrap_or(storage.description.as_str());
                format!("{label}:{}", path.display())
            }
        }
    }

    /// Whether both locations point at the same folder.
    ///
    /// Deliberately ignores volatile metadata such as free space: a listing response carries the
    /// storage snapshot from when the request was sent, and refreshed free space must not make a
    /// perfectly valid response look stale.
    pub fn same_target(&self, other: &Location) -> bool {
        match (self, other) {
            (Location::Local(left), Location::Local(right)) => left == right,
            (
                Location::Remote {
                    storage: left_storage,
                    path: left_path,
                },
                Location::Remote {
                    storage: right_storage,
                    path: right_path,
                },
            ) => left_storage.id == right_storage.id && left_path == right_path,
            _ => false,
        }
    }

    /// Whether files may be written here.
    pub fn is_writable(&self) -> bool {
        match self {
            Location::Local(_) => true,
            Location::Remote { storage, .. } => storage.is_writable(),
        }
    }

    /// Descend into `name`, which must be a single path segment.
    pub fn child(&self, name: &str) -> Option<Location> {
        match self {
            Location::Local(path) => Some(Location::Local(path.join(name))),
            Location::Remote { storage, path } => {
                path.join(name).ok().map(|path| Location::Remote {
                    storage: storage.clone(),
                    path,
                })
            }
        }
    }

    /// The parent folder, or `None` when already at the top.
    pub fn parent(&self) -> Option<Location> {
        match self {
            Location::Local(path) => path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .map(|parent| Location::Local(parent.to_path_buf())),
            Location::Remote { storage, path } => path.parent().map(|path| Location::Remote {
                storage: storage.clone(),
                path,
            }),
        }
    }
}

/// Render a host path with `~` for the home directory.
fn display_local_path(path: &Path) -> String {
    let home = std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from);
    match home.and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rest) if !rest.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => path.display().to_string(),
    }
}

/// Where an entry's data lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// The synthetic `..` row.
    Parent,
    /// A file or folder on the host; the path is already fully resolved.
    Local(PathBuf),
    /// An object on the reader, addressed by its panel location plus name.
    Remote,
}

/// One visible row of a panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Display name (the parent row is named `..`).
    pub name: String,
    /// Folder or file.
    pub kind: EntryKind,
    /// Size in bytes for files.
    pub size: Option<u64>,
    /// Modification time, device-local or host-local.
    pub modified: Option<DateTime>,
    /// Where the data is.
    pub origin: Origin,
}

impl FileEntry {
    /// The synthetic parent-directory row.
    pub fn parent() -> Self {
        Self {
            name: "..".to_string(),
            kind: EntryKind::Directory,
            size: None,
            modified: None,
            origin: Origin::Parent,
        }
    }

    /// Convert a remote listing entry.
    pub fn from_remote(entry: &RemoteEntry) -> Self {
        Self {
            name: entry.name.clone(),
            kind: entry.kind,
            size: entry.size,
            modified: entry.modified,
            origin: Origin::Remote,
        }
    }

    /// Whether this is the `..` row.
    pub fn is_parent(&self) -> bool {
        self.origin == Origin::Parent
    }

    /// Whether this row is a folder (the `..` row counts as one).
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Directory
    }

    /// Name with a trailing slash for folders, as file managers display it.
    pub fn display_name(&self) -> String {
        if self.is_dir() && !self.is_parent() {
            format!("{}/", self.name)
        } else {
            self.name.clone()
        }
    }

    /// Sort key: `..` first, then folders, then files, then case-insensitive name.
    pub fn sort_key(&self) -> (u8, u8, String, String) {
        let group = u8::from(!self.is_parent());
        let kind = u8::from(!self.is_dir());
        (group, kind, self.name.to_lowercase(), self.name.clone())
    }
}

/// Read a host directory into panel rows.
///
/// The `..` row is prepended when the directory has a parent. Entries that cannot be inspected
/// (broken symlinks, races) are skipped with a debug log instead of failing the whole listing.
pub fn read_local_dir(path: &Path) -> std::io::Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    if path.parent().is_some() {
        entries.push(FileEntry::parent());
    }

    for item in std::fs::read_dir(path)? {
        let item = match item {
            Ok(item) => item,
            Err(error) => {
                tracing::debug!(path = %path.display(), %error, "skipping unreadable directory entry");
                continue;
            }
        };
        let metadata = match item.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(path = %item.path().display(), %error, "skipping entry without metadata");
                continue;
            }
        };
        let kind = if metadata.is_dir() {
            EntryKind::Directory
        } else {
            EntryKind::File
        };
        entries.push(FileEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            kind,
            size: (kind == EntryKind::File).then_some(metadata.len()),
            modified: metadata.modified().ok().and_then(datetime_from_system_time),
            origin: Origin::Local(item.path()),
        });
    }

    sort_entries(&mut entries);
    Ok(entries)
}

/// Sort panel rows: `..` first, folders before files, names case-insensitively.
pub fn sort_entries(entries: &mut [FileEntry]) {
    entries.sort_by_key(FileEntry::sort_key);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageEntry;
    use mtp_rs::mtp::StorageId;

    fn storage() -> StorageEntry {
        StorageEntry::new(StorageId(65_537), "Internal storage")
            .with_capacity(1024, 512)
            .with_removable(false)
            .with_writable(true)
    }

    #[test]
    fn remote_locations_walk_up_and_down() {
        let root = Location::remote(storage(), RemotePath::root());
        assert_eq!(root.display_path(), "/");
        assert_eq!(root.title(), "Internal storage:/");
        assert!(root.parent().is_none());

        let books = root.child("Books").expect("child");
        assert_eq!(books.display_path(), "/Books");
        let chapter = books.child("chapter.epub").expect("child");
        assert_eq!(chapter.display_path(), "/Books/chapter.epub");
        assert_eq!(chapter.parent().unwrap(), books);
        assert_eq!(books.parent().unwrap(), root);
        assert!(chapter.child("nested/x").is_none(), "a name is one segment");
        assert!(books.is_writable());
    }

    #[test]
    fn remote_title_uses_the_alias_when_there_is_one() {
        let catalog = crate::storage::StorageCatalog::new([storage()]);
        let entry = catalog.entries()[0].clone();
        assert_eq!(entry.alias(), Some(StorageAlias::Internal));
        let location = Location::remote(entry, RemotePath::parse("/Books").unwrap());
        assert_eq!(location.title(), "internal:/Books");
    }

    #[test]
    fn same_target_ignores_refreshed_storage_metadata() {
        let location = Location::remote(storage(), RemotePath::root());

        let mut refreshed = storage();
        refreshed.free_bytes = Some(1);
        let refreshed = Location::remote(refreshed, RemotePath::root());
        assert!(location.same_target(&refreshed));

        // Same storage, different folder.
        let books = Location::remote(storage(), RemotePath::parse("/Books").unwrap());
        assert!(!location.same_target(&books));

        // Different storage, same path.
        let other_storage = StorageEntry::new(mtp_rs::mtp::StorageId(2), "SD").with_removable(true);
        let other = Location::remote(other_storage, RemotePath::root());
        assert!(!location.same_target(&other));

        // Local vs remote is never the same target.
        assert!(!location.same_target(&Location::local("/")));
        assert!(Location::local("/tmp").same_target(&Location::local("/tmp")));
    }

    #[test]
    fn local_locations_walk_up_and_down() {
        let root = Location::local("/");
        assert!(root.parent().is_none());
        let tmp = Location::local("/tmp");
        assert_eq!(tmp.parent().unwrap(), root);
        assert_eq!(tmp.child("x.txt").unwrap(), Location::local("/tmp/x.txt"));
    }

    #[test]
    fn parent_row_sorts_first_and_displays_as_dotdot() {
        let mut entries = vec![
            FileEntry {
                name: "z.epub".into(),
                kind: EntryKind::File,
                size: Some(1),
                modified: None,
                origin: Origin::Remote,
            },
            FileEntry {
                name: "Alpha".into(),
                kind: EntryKind::Directory,
                size: None,
                modified: None,
                origin: Origin::Remote,
            },
            FileEntry::parent(),
        ];
        sort_entries(&mut entries);
        let names: Vec<String> = entries.iter().map(FileEntry::display_name).collect();
        assert_eq!(names, vec!["..", "Alpha/", "z.epub"]);
    }

    #[test]
    fn local_listing_reads_real_entries_and_marks_directories() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Sub")).unwrap();
        std::fs::write(dir.path().join("book.epub"), b"abc").unwrap();

        let entries = read_local_dir(dir.path()).unwrap();
        let names: Vec<String> = entries.iter().map(FileEntry::display_name).collect();
        assert_eq!(names, vec!["..", "Sub/", "book.epub"]);

        let book = entries
            .iter()
            .find(|entry| entry.name == "book.epub")
            .unwrap();
        assert_eq!(book.size, Some(3));
        assert!(!book.is_dir());
        assert!(matches!(book.origin, Origin::Local(_)));
    }

    #[test]
    fn panel_ids_map_to_array_indices() {
        assert_eq!(PanelId::Left.index(), 0);
        assert_eq!(PanelId::Right.index(), 1);
        assert_eq!(PanelId::Left.other(), PanelId::Right);
    }
}
