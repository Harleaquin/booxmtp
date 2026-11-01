//! End-to-end tests against `mtp-rs`' virtual MTP device.
//!
//! The virtual device is backed by a temporary directory, so these tests exercise the real code
//! path — storage enumeration, segment-wise path resolution, streaming upload/download, collision
//! handling and verification — without USB hardware. They are what makes the MVP testable on CI.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use booxmtp::device::DeviceSummary;
use booxmtp::error::Error;
use booxmtp::remote_path::{EntryKind, RemotePath};
use booxmtp::session::Session;
use booxmtp::transfer::{TransferOptions, TransferProgress};
use mtp_rs::CancelToken;
use mtp_rs::mtp::{MtpDevice, StorageId};
use mtp_rs::virtual_device::{
    VirtualDeviceConfig, VirtualStorageConfig, register_virtual_device, unregister_virtual_device,
};

/// Distinguishes virtual device registrations inside one test process.
static NEXT_DEVICE: AtomicU64 = AtomicU64::new(0);

/// One virtual reader plus its backing and host-side temporary directories.
struct Fixture {
    session: Session,
    storage: StorageId,
    backing: PathBuf,
    local: tempfile::TempDir,
    location_id: u64,
    _backing_dir: tempfile::TempDir,
}

impl Fixture {
    async fn new() -> Self {
        let backing_dir = tempfile::tempdir().expect("backing dir");
        let local = tempfile::tempdir().expect("local dir");

        // Seed the "device" with the kind of layout a reader has.
        std::fs::create_dir_all(backing_dir.path().join("Books/Sci-Fi")).expect("mkdir");
        std::fs::write(
            backing_dir.path().join("Books/Roman.epub"),
            b"virtueller inhalt",
        )
        .expect("seed epub");
        std::fs::write(
            backing_dir.path().join("Books/Sci-Fi/Dune.epub"),
            vec![0x7u8; 4096],
        )
        .expect("seed dune");

        let index = NEXT_DEVICE.fetch_add(1, Ordering::Relaxed);
        let config = VirtualDeviceConfig {
            manufacturer: "BOOX".to_string(),
            model: "Virtual Go Color 7".to_string(),
            serial: format!("virtual-boox-{index}"),
            storages: vec![VirtualStorageConfig {
                description: "Internal storage".to_string(),
                capacity: 64 * 1024 * 1024,
                backing_dir: backing_dir.path().to_path_buf(),
                read_only: false,
            }],
            watch_backing_dirs: false,
            ..Default::default()
        };

        let info = register_virtual_device(&config);
        let device = MtpDevice::open_by_location(info.location_id)
            .await
            .expect("open virtual device");
        let summary = DeviceSummary::from_info(&info);
        let session = Session::attach(device, summary).await.expect("session");
        let storage = session.catalog().entries()[0].id;

        Self {
            session,
            storage,
            backing: backing_dir.path().to_path_buf(),
            local,
            location_id: info.location_id,
            _backing_dir: backing_dir,
        }
    }

    fn local_path(&self, name: &str) -> PathBuf {
        self.local.path().join(name)
    }

    /// Options without overwrite, as the CLI would build them by default.
    fn options() -> TransferOptions {
        TransferOptions::default()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        unregister_virtual_device(self.location_id);
    }
}

fn no_progress(_: TransferProgress) {}

#[tokio::test]
async fn storages_are_discovered_and_aliased() {
    let fixture = Fixture::new().await;
    let entries = fixture.session.catalog().entries();

    assert_eq!(entries.len(), 1, "the virtual device reports one storage");
    assert_eq!(entries[0].description, "Internal storage");
    // The virtual device declares fixed RAM, so `internal` is unambiguous.
    let target = fixture
        .session
        .select_storage("internal")
        .expect("internal alias");
    assert_eq!(target.id(), fixture.storage);

    // No removable storage is reported: `sd` must be refused instead of simulated.
    let error = fixture.session.select_storage("sd").unwrap_err();
    assert!(matches!(error, Error::StorageAliasUnavailable { .. }));

    // The numeric ID always works.
    assert!(
        fixture
            .session
            .select_storage(&fixture.storage.0.to_string())
            .is_ok()
    );
}

#[tokio::test]
async fn listing_shows_folders_first_with_sizes() {
    let fixture = Fixture::new().await;

    let root = fixture
        .session
        .list_dir(fixture.storage, &RemotePath::root(), None)
        .await
        .expect("list root");
    assert_eq!(root.entries.len(), 1);
    assert_eq!(root.entries[0].name, "Books");
    assert_eq!(root.entries[0].kind, EntryKind::Directory);
    assert_eq!(root.entries[0].size, None);

    let books = fixture
        .session
        .list_dir(fixture.storage, &RemotePath::parse("/Books").unwrap(), None)
        .await
        .expect("list Books");
    let names: Vec<&str> = books
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, vec!["Sci-Fi", "Roman.epub"]);
    let roman = &books.entries[1];
    assert_eq!(roman.size, Some(b"virtueller inhalt".len() as u64));
    assert!(roman.modified.is_some());
}

#[tokio::test]
async fn push_then_pull_is_byte_identical() {
    let fixture = Fixture::new().await;

    // 512 KiB of deterministic, non-textual bytes.
    let payload: Vec<u8> = (0..512 * 1024u32)
        .map(|index| (index % 251) as u8)
        .collect();
    let source = fixture.local_path("payload.bin");
    std::fs::write(&source, &payload).expect("write source");

    let remote = RemotePath::parse("/Books/Sci-Fi/payload.bin").unwrap();
    let mut progress = no_progress;
    let upload = fixture
        .session
        .upload_file(
            fixture.storage,
            &source,
            &remote,
            &Fixture::options(),
            &mut progress,
        )
        .await
        .expect("upload");
    assert_eq!(upload.bytes, payload.len() as u64);
    assert!(upload.verified(), "re-listing should confirm the size");

    // The virtual device writes into its backing directory, like a real reader would.
    assert_eq!(
        std::fs::read(fixture.backing.join("Books/Sci-Fi/payload.bin")).unwrap(),
        payload
    );

    let destination = fixture.local_path("downloaded.bin");
    let mut progress = no_progress;
    let download = fixture
        .session
        .download_file(
            fixture.storage,
            &remote,
            &destination,
            &Fixture::options(),
            &mut progress,
        )
        .await
        .expect("download");
    assert_eq!(download.bytes, payload.len() as u64);
    assert_eq!(std::fs::read(&destination).unwrap(), payload);

    // No `.part` leftovers.
    assert!(!destination.with_extension("bin.part").exists());
    assert!(
        std::fs::read_dir(fixture.local.path())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".part"))
    );
}

#[tokio::test]
async fn existing_targets_are_only_replaced_with_overwrite() {
    let fixture = Fixture::new().await;
    let source = fixture.local_path("note.txt");
    std::fs::write(&source, b"first").unwrap();
    let remote = RemotePath::parse("/Books/note.txt").unwrap();

    let mut progress = no_progress;
    fixture
        .session
        .upload_file(
            fixture.storage,
            &source,
            &remote,
            &Fixture::options(),
            &mut progress,
        )
        .await
        .expect("initial upload");

    // Same target again without overwrite.
    let mut progress = no_progress;
    let error = fixture
        .session
        .upload_file(
            fixture.storage,
            &source,
            &remote,
            &Fixture::options(),
            &mut progress,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::RemoteAlreadyExists { .. }));

    // With overwrite the object is replaced and the new content lands on the device.
    std::fs::write(&source, b"second revision").unwrap();
    let mut progress = no_progress;
    fixture
        .session
        .upload_file(
            fixture.storage,
            &source,
            &remote,
            &TransferOptions {
                overwrite: true,
                cancel: None,
            },
            &mut progress,
        )
        .await
        .expect("overwrite upload");
    assert_eq!(
        std::fs::read(fixture.backing.join("Books/note.txt")).unwrap(),
        b"second revision"
    );

    // Local download collision.
    let destination = fixture.local_path("note-copy.txt");
    std::fs::write(&destination, b"do not clobber").unwrap();
    let mut progress = no_progress;
    let error = fixture
        .session
        .download_file(
            fixture.storage,
            &remote,
            &destination,
            &Fixture::options(),
            &mut progress,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::LocalAlreadyExists { .. }));
    assert_eq!(std::fs::read(&destination).unwrap(), b"do not clobber");

    // ... and with overwrite it is replaced.
    let mut progress = no_progress;
    fixture
        .session
        .download_file(
            fixture.storage,
            &remote,
            &destination,
            &TransferOptions {
                overwrite: true,
                cancel: None,
            },
            &mut progress,
        )
        .await
        .expect("overwrite download");
    assert_eq!(std::fs::read(&destination).unwrap(), b"second revision");
}

#[tokio::test]
async fn missing_paths_and_wrong_target_types_fail_clearly() {
    let fixture = Fixture::new().await;

    let error = fixture
        .session
        .list_dir(fixture.storage, &RemotePath::parse("/Nope").unwrap(), None)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::RemotePathNotFound { .. }));

    // Listing a file is not allowed.
    let error = fixture
        .session
        .list_dir(
            fixture.storage,
            &RemotePath::parse("/Books/Roman.epub").unwrap(),
            None,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::RemoteNotADirectory { .. }));

    // Pulling a folder is not allowed.
    let mut progress = no_progress;
    let error = fixture
        .session
        .download_file(
            fixture.storage,
            &RemotePath::parse("/Books").unwrap(),
            &fixture.local_path("books"),
            &Fixture::options(),
            &mut progress,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::RemoteIsADirectory { .. }));

    // Uploading into a folder that does not exist is refused (no implicit mkdir -p).
    let source = fixture.local_path("x.txt");
    std::fs::write(&source, b"x").unwrap();
    let mut progress = no_progress;
    let error = fixture
        .session
        .upload_file(
            fixture.storage,
            &source,
            &RemotePath::parse("/Nowhere/x.txt").unwrap(),
            &Fixture::options(),
            &mut progress,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::RemotePathNotFound { .. }));

    // Uploading a local directory is refused before touching the device.
    let mut progress = no_progress;
    let error = fixture
        .session
        .upload_file(
            fixture.storage,
            fixture.local.path(),
            &RemotePath::parse("/Books/x").unwrap(),
            &Fixture::options(),
            &mut progress,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::LocalNotAFile { .. }));
}

#[tokio::test]
async fn mkdir_creates_one_folder_and_refuses_duplicates() {
    let fixture = Fixture::new().await;
    let folder = RemotePath::parse("/Books/ToRead").unwrap();

    let entry = fixture
        .session
        .create_dir(fixture.storage, &folder, None)
        .await
        .expect("create folder");
    assert_eq!(entry.kind, EntryKind::Directory);
    assert!(fixture.backing.join("Books/ToRead").is_dir());

    let error = fixture
        .session
        .create_dir(fixture.storage, &folder, None)
        .await
        .unwrap_err();
    assert!(matches!(error, Error::RemoteAlreadyExists { .. }));

    // A file can be uploaded into the new folder.
    let source = fixture.local_path("chapter.txt");
    std::fs::write(&source, b"chapter one").unwrap();
    let mut progress = no_progress;
    fixture
        .session
        .upload_file(
            fixture.storage,
            &source,
            &RemotePath::parse("/Books/ToRead/chapter.txt").unwrap(),
            &Fixture::options(),
            &mut progress,
        )
        .await
        .expect("upload into new folder");
}

#[tokio::test]
async fn names_with_spaces_and_umlauts_round_trip() {
    let fixture = Fixture::new().await;
    let name = "Über Bücher & Notizen.epub";
    let remote = RemotePath::parse("/Books").unwrap().join(name).unwrap();
    let source = fixture.local_path("source.epub");
    std::fs::write(&source, b"inhalt").unwrap();

    let mut progress = no_progress;
    fixture
        .session
        .upload_file(
            fixture.storage,
            &source,
            &remote,
            &Fixture::options(),
            &mut progress,
        )
        .await
        .expect("upload unicode name");

    let listing = fixture
        .session
        .list_dir(fixture.storage, &RemotePath::parse("/Books").unwrap(), None)
        .await
        .expect("list");
    assert!(listing.entries.iter().any(|entry| entry.name == name));

    let destination = fixture.local_path("roundtrip.epub");
    let mut progress = no_progress;
    fixture
        .session
        .download_file(
            fixture.storage,
            &remote,
            &destination,
            &Fixture::options(),
            &mut progress,
        )
        .await
        .expect("download unicode name");
    assert_eq!(std::fs::read(destination).unwrap(), b"inhalt");
}

#[tokio::test]
async fn a_cancelled_token_stops_the_operation() {
    let fixture = Fixture::new().await;
    let cancel = CancelToken::new();
    cancel.cancel();

    let mut progress = no_progress;
    let error = fixture
        .session
        .download_file(
            fixture.storage,
            &RemotePath::parse("/Books/Roman.epub").unwrap(),
            &fixture.local_path("cancelled.epub"),
            &TransferOptions {
                overwrite: false,
                cancel: Some(cancel.clone()),
            },
            &mut progress,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Cancelled));
    assert!(!fixture.local_path("cancelled.epub").exists());
    assert!(!Path::new(&fixture.local_path("cancelled.epub")).exists());
}

#[tokio::test]
async fn download_into_a_missing_directory_is_refused() {
    let fixture = Fixture::new().await;
    let mut progress = no_progress;
    let error = fixture
        .session
        .download_file(
            fixture.storage,
            &RemotePath::parse("/Books/Roman.epub").unwrap(),
            &fixture.local.path().join("does/not/exist.epub"),
            &Fixture::options(),
            &mut progress,
        )
        .await
        .unwrap_err();
    assert!(matches!(error, Error::LocalDirectoryMissing { .. }));
}
