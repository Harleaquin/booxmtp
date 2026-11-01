//! Recursive copy, move and delete (host ↔ reader), against `mtp-rs`' virtual device.
//!
//! These tests exercise `booxmtp::tree` with a filesystem-backed MTP storage, so the full
//! traversal logic runs without USB hardware.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use booxmtp::device::DeviceSummary;
use booxmtp::error::Error;
use booxmtp::location::Location;
use booxmtp::remote_path::RemotePath;
use booxmtp::session::Session;
use booxmtp::storage::StorageEntry;
use booxmtp::transfer::{TransferOptions, TransferProgress};
use booxmtp::tree;
use mtp_rs::mtp::{MtpDevice, StorageId};
use mtp_rs::virtual_device::{
    VirtualDeviceConfig, VirtualStorageConfig, register_virtual_device, unregister_virtual_device,
};

static NEXT_DEVICE: AtomicU64 = AtomicU64::new(0);

/// A virtual reader plus the temporary directories backing it and the host side.
struct Fixture {
    session: Session,
    storage: StorageEntry,
    device_root: PathBuf,
    host: tempfile::TempDir,
    device_dir: tempfile::TempDir,
    location_id: u64,
}

impl Fixture {
    async fn new() -> Self {
        let device_dir = tempfile::tempdir().expect("device dir");
        let host = tempfile::tempdir().expect("host dir");

        // Seed the device storage with a small tree.
        std::fs::create_dir_all(device_dir.path().join("Books/Sci-Fi")).unwrap();
        std::fs::write(device_dir.path().join("Books/Roman.epub"), b"roman").unwrap();
        std::fs::write(device_dir.path().join("Books/Sci-Fi/Dune.epub"), b"dune").unwrap();

        // And the host with a tree of its own.
        std::fs::create_dir_all(host.path().join("src/sub")).unwrap();
        std::fs::write(host.path().join("src/a.txt"), b"aaaa").unwrap();
        std::fs::write(host.path().join("src/sub/b.txt"), b"bb").unwrap();

        let index = NEXT_DEVICE.fetch_add(1, Ordering::Relaxed);
        let config = VirtualDeviceConfig {
            manufacturer: "BOOX".to_string(),
            model: "Virtual Go Color 7".to_string(),
            serial: format!("virtual-tree-{index}"),
            storages: vec![VirtualStorageConfig {
                description: "Internal storage".to_string(),
                capacity: 64 * 1024 * 1024,
                backing_dir: device_dir.path().to_path_buf(),
                read_only: false,
            }],
            watch_backing_dirs: false,
            ..Default::default()
        };

        let info = register_virtual_device(&config);
        let device = MtpDevice::open_by_location(info.location_id).await.unwrap();
        let summary = DeviceSummary::from_info(&info);
        let session = Session::attach(device, summary).await.unwrap();
        let storage = session.catalog().entries()[0].clone();

        Self {
            session,
            storage,
            device_root: device_dir.path().to_path_buf(),
            host,
            device_dir,
            location_id: info.location_id,
        }
    }

    fn host(&self, relative: &str) -> PathBuf {
        self.host.path().join(relative)
    }

    fn remote(&self, path: &str) -> Location {
        Location::remote(
            self.storage.clone(),
            RemotePath::parse(path).expect("valid remote path"),
        )
    }

    fn storage_id(&self) -> StorageId {
        self.storage.id
    }

    async fn remote_names(&self, path: &str) -> Vec<String> {
        let listing = self
            .session
            .list_dir(self.storage_id(), &RemotePath::parse(path).unwrap(), None)
            .await
            .expect("list");
        listing
            .entries
            .iter()
            .map(|entry| entry.name.clone())
            .collect()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unregister_virtual_device(self.location_id);
        let _ = &self.device_dir; // keep alive until the end of the test
    }
}

fn options(overwrite: bool) -> TransferOptions {
    TransferOptions {
        overwrite,
        cancel: None,
    }
}

fn collecting<'a>(labels: &'a mut Vec<String>) -> impl FnMut(&str, TransferProgress) + Send + 'a {
    move |label, progress| {
        assert!(!label.is_empty());
        let _ = progress;
        if labels.last().map(String::as_str) != Some(label) {
            labels.push(label.to_string());
        }
    }
}

#[tokio::test]
async fn copy_host_folder_to_reader() {
    let fixture = Fixture::new().await;
    let mut labels = Vec::new();

    let summary = tree::copy(
        &fixture.session,
        &Location::local(fixture.host("src")),
        &fixture.remote("/uploaded"),
        &options(false),
        &mut collecting(&mut labels),
    )
    .await
    .expect("copy folder to device");

    assert_eq!(summary.files, 2);
    assert_eq!(summary.dirs, 2, "the folder itself and `sub`");
    assert_eq!(summary.bytes, 6);
    assert_eq!(
        std::fs::read(fixture.device_root.join("uploaded/a.txt")).unwrap(),
        b"aaaa"
    );
    assert_eq!(
        std::fs::read(fixture.device_root.join("uploaded/sub/b.txt")).unwrap(),
        b"bb"
    );
    assert!(labels.iter().any(|label| label.ends_with("b.txt")));
}

#[tokio::test]
async fn copy_reader_folder_to_host() {
    let fixture = Fixture::new().await;
    let mut labels = Vec::new();

    let summary = tree::copy(
        &fixture.session,
        &fixture.remote("/Books"),
        &Location::local(fixture.host("books-copy")),
        &options(false),
        &mut collecting(&mut labels),
    )
    .await
    .expect("copy folder to host");

    assert_eq!(summary.files, 2);
    assert_eq!(summary.dirs, 2);
    assert_eq!(
        std::fs::read(fixture.host("books-copy/Roman.epub")).unwrap(),
        b"roman"
    );
    assert_eq!(
        std::fs::read(fixture.host("books-copy/Sci-Fi/Dune.epub")).unwrap(),
        b"dune"
    );
}

#[tokio::test]
async fn copy_host_folder_to_host() {
    let fixture = Fixture::new().await;
    let mut labels = Vec::new();

    let summary = tree::copy(
        &fixture.session,
        &Location::local(fixture.host("src")),
        &Location::local(fixture.host("src-copy")),
        &options(false),
        &mut collecting(&mut labels),
    )
    .await
    .expect("copy folder to host");

    assert_eq!(summary.files, 2);
    assert_eq!(summary.dirs, 2);
    assert_eq!(
        std::fs::read(fixture.host("src-copy/sub/b.txt")).unwrap(),
        b"bb"
    );
}

#[tokio::test]
async fn copying_a_folder_merges_into_an_existing_one() {
    let fixture = Fixture::new().await;

    std::fs::create_dir_all(fixture.host("merge")).unwrap();
    std::fs::write(fixture.host("merge/keep.txt"), b"keep").unwrap();

    tree::copy(
        &fixture.session,
        &Location::local(fixture.host("src")),
        &Location::local(fixture.host("merge")),
        &options(false),
        &mut |_, _| {},
    )
    .await
    .expect("merge");

    assert!(fixture.host("merge/keep.txt").exists());
    assert!(fixture.host("merge/a.txt").exists());
    assert!(fixture.host("merge/sub/b.txt").exists());
}

#[tokio::test]
async fn a_file_never_replaces_a_folder() {
    let fixture = Fixture::new().await;
    std::fs::create_dir_all(fixture.host("target")).unwrap();
    std::fs::write(fixture.host("file.txt"), b"x").unwrap();

    // Not even with overwrite: silently deleting a folder is too destructive, so this is a plain
    // error rather than an "Overwrite?" question.
    for overwrite in [false, true] {
        let error = tree::copy(
            &fixture.session,
            &Location::local(fixture.host("file.txt")),
            &Location::local(fixture.host("target")),
            &options(overwrite),
            &mut |_, _| {},
        )
        .await
        .expect_err("a file cannot replace a folder");
        assert!(matches!(error, Error::LocalIsDirectory { .. }));
    }
}

#[tokio::test]
async fn a_folder_can_replace_a_file_when_overwriting() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.host("destination"), b"old file").unwrap();

    tree::copy(
        &fixture.session,
        &Location::local(fixture.host("src")),
        &Location::local(fixture.host("destination")),
        &options(true),
        &mut |_, _| {},
    )
    .await
    .expect("folder replaces file");

    assert!(fixture.host("destination/a.txt").exists());
}

#[tokio::test]
async fn move_host_folder_to_reader_removes_the_source() {
    let fixture = Fixture::new().await;

    let summary = tree::move_entry(
        &fixture.session,
        &Location::local(fixture.host("src")),
        &fixture.remote("/moved"),
        &options(false),
        &mut |_, _| {},
    )
    .await
    .expect("move to device");

    assert_eq!(summary.files, 2);
    assert!(!fixture.host("src").exists(), "source folder is gone");
    assert_eq!(
        std::fs::read(fixture.device_root.join("moved/sub/b.txt")).unwrap(),
        b"bb"
    );
}

#[tokio::test]
async fn move_reader_file_to_host_removes_it_on_the_device() {
    let fixture = Fixture::new().await;

    let summary = tree::move_entry(
        &fixture.session,
        &fixture.remote("/Books/Roman.epub"),
        &Location::local(fixture.host("Roman.epub")),
        &options(false),
        &mut |_, _| {},
    )
    .await
    .expect("move to host");

    assert_eq!(summary.files, 1);
    assert_eq!(std::fs::read(fixture.host("Roman.epub")).unwrap(), b"roman");
    assert!(!fixture.device_root.join("Books/Roman.epub").exists());
    assert_eq!(fixture.remote_names("/Books").await, vec!["Sci-Fi"]);
}

#[tokio::test]
async fn move_host_file_to_host_uses_rename() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.host("note.txt"), b"note").unwrap();

    tree::move_entry(
        &fixture.session,
        &Location::local(fixture.host("note.txt")),
        &Location::local(fixture.host("note-moved.txt")),
        &options(false),
        &mut |_, _| {},
    )
    .await
    .expect("move on host");

    assert!(!fixture.host("note.txt").exists());
    assert_eq!(
        std::fs::read(fixture.host("note-moved.txt")).unwrap(),
        b"note"
    );
}

#[tokio::test]
async fn delete_reader_folder_recursively() {
    let fixture = Fixture::new().await;

    let summary = tree::remove(&fixture.session, &fixture.remote("/Books"), None)
        .await
        .expect("delete folder");

    assert_eq!(summary.files, 2);
    assert_eq!(summary.dirs, 2);
    assert!(!fixture.device_root.join("Books").exists());
    assert!(fixture.remote_names("/").await.is_empty());
}

#[tokio::test]
async fn delete_host_folder_and_file() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.host("single.txt"), b"x").unwrap();

    let summary = tree::remove(
        &fixture.session,
        &Location::local(fixture.host("src")),
        None,
    )
    .await
    .expect("delete folder");
    assert_eq!(summary.files, 2);
    assert_eq!(summary.dirs, 2);
    assert!(!fixture.host("src").exists());

    let summary = tree::remove(
        &fixture.session,
        &Location::local(fixture.host("single.txt")),
        None,
    )
    .await
    .expect("delete file");
    assert_eq!(summary.files, 1);
    assert_eq!(summary.dirs, 0);
    assert!(!fixture.host("single.txt").exists());
}

#[tokio::test]
async fn deleting_a_missing_entry_is_reported() {
    let fixture = Fixture::new().await;
    let error = tree::remove(
        &fixture.session,
        &Location::local(fixture.host("nope")),
        None,
    )
    .await
    .expect_err("missing entries cannot be deleted");
    assert!(matches!(error, Error::LocalNotFound { .. }));
}

#[tokio::test]
async fn remote_to_remote_is_refused_with_a_hint() {
    let fixture = Fixture::new().await;
    let error = tree::copy(
        &fixture.session,
        &fixture.remote("/Books"),
        &fixture.remote("/Elsewhere"),
        &options(false),
        &mut |_, _| {},
    )
    .await
    .expect_err("remote to remote is not implemented");
    assert!(matches!(error, Error::UnsupportedByApp { .. }));
    assert!(error.hint().unwrap().contains("local panel"));
}

#[tokio::test]
async fn overwriting_a_remote_file_replaces_it() {
    let fixture = Fixture::new().await;
    std::fs::write(fixture.host("Roman.epub"), b"new contents").unwrap();

    tree::copy(
        &fixture.session,
        &Location::local(fixture.host("Roman.epub")),
        &fixture.remote("/Books/Roman.epub"),
        &options(true),
        &mut |_, _| {},
    )
    .await
    .expect("overwrite remote file");

    assert_eq!(
        std::fs::read(fixture.device_root.join("Books/Roman.epub")).unwrap(),
        b"new contents"
    );
}

#[tokio::test]
async fn a_cancelled_token_stops_a_tree_copy() {
    let fixture = Fixture::new().await;
    let cancel = mtp_rs::CancelToken::new();
    cancel.cancel();

    let error = tree::copy(
        &fixture.session,
        &Location::local(fixture.host("src")),
        &fixture.remote("/cancelled"),
        &TransferOptions {
            overwrite: false,
            cancel: Some(cancel),
        },
        &mut |_, _| {},
    )
    .await
    .expect_err("cancelled copy must fail");
    assert!(matches!(error, Error::Cancelled));
    assert!(!fixture.device_root.join("cancelled").exists());
}

#[allow(dead_code)]
fn _path(_: &Path) {}
