//! What a panel can show: local folders and MTP storages, unified into one list model.
//!
//! A panel points at a [`Location`] (either a host directory or a storage + remote path) and
//! displays a list of [`FileEntry`] rows. Both sources are normalised into the same shape so the
//! UI, the key handling and the copy logic do not care where an entry comes from.

use std::path::{Path, PathBuf};

use mtp_rs::mtp::DateTime;

use crate::remote_path::{EntryKind, RemoteEntry};
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

pub use crate::location::Location;

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
