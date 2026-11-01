//! A location a panel can show: a host folder or a folder on the reader.
//!
//! Keeping this type in the core (instead of the TUI) lets recursive copy, move and delete
//! operations work on the same abstraction the panels use, and keeps them testable without a
//! terminal.

use std::path::{Path, PathBuf};

use crate::remote_path::RemotePath;
use crate::storage::{StorageAlias, StorageEntry};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::StorageCatalog;
    use mtp_rs::mtp::{StorageId, StorageType};

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
        let catalog = StorageCatalog::new([storage()]);
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

        let books = Location::remote(storage(), RemotePath::parse("/Books").unwrap());
        assert!(!location.same_target(&books));

        let other_storage = StorageEntry::new(StorageId(2), "SD").with_removable(true);
        let other = Location::remote(other_storage, RemotePath::root());
        assert!(!location.same_target(&other));

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
    fn storage_type_is_reported_through_location() {
        let storage = storage().with_storage_type(StorageType::RemovableRam);
        let location = Location::remote(storage, RemotePath::root());
        assert_eq!(
            location.storage().and_then(|storage| storage.removable),
            Some(true)
        );
    }
}
