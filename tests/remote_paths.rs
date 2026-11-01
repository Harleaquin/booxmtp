//! Remote-path parsing and resolution rules.
//!
//! The resolver is exercised against an in-memory [`RemoteFs`] so these tests are fast and need no
//! USB hardware. MTP itself is covered by `device_integration.rs`.

use std::collections::HashMap;

use booxmtp::Result;
use booxmtp::remote_path::{
    Children, EntryKind, RemoteEntry, RemoteFs, RemotePath, find_in, is_valid_entry_name, read_dir,
    resolve_dir, resolve_entry,
};
use mtp_rs::CancelToken;
use mtp_rs::mtp::ObjectHandle;

/// An in-memory folder tree keyed by the parent's handle.
#[derive(Debug, Default)]
struct FakeFs {
    children: HashMap<u64, Vec<RemoteEntry>>,
}

impl FakeFs {
    fn dir(mut self, parent: ObjectHandle, handle: u64, name: &str) -> Self {
        self.children
            .entry(parent.0)
            .or_default()
            .push(RemoteEntry {
                handle: ObjectHandle(handle),
                name: name.to_string(),
                kind: EntryKind::Directory,
                size: None,
                modified: None,
            });
        self
    }

    fn file(mut self, parent: ObjectHandle, handle: u64, name: &str, size: u64) -> Self {
        self.children
            .entry(parent.0)
            .or_default()
            .push(RemoteEntry {
                handle: ObjectHandle(handle),
                name: name.to_string(),
                kind: EntryKind::File,
                size: Some(size),
                modified: None,
            });
        self
    }
}

impl RemoteFs for FakeFs {
    fn list_children(
        &self,
        parent: Option<ObjectHandle>,
        _cancel: Option<&CancelToken>,
    ) -> impl Future<Output = Result<Children>> + Send {
        let key = parent.unwrap_or(ObjectHandle::ROOT).0;
        let entries = self.children.get(&key).cloned().unwrap_or_default();
        async move {
            Ok(Children {
                entries,
                unreadable: Vec::new(),
            })
        }
    }
}

/// `/Books/` (handle 10) with a nested folder and two files, one with spaces and umlauts.
fn sample_tree() -> FakeFs {
    FakeFs::default()
        .dir(ObjectHandle::ROOT, 10, "Books")
        .dir(ObjectHandle(10), 11, "Sci-Fi")
        .file(ObjectHandle::ROOT, 20, "notes.txt", 42)
        .file(ObjectHandle(10), 12, "Roman.epub", 1_000)
        .file(ObjectHandle(10), 13, "Über Bücher.pdf", 2_000)
        .file(ObjectHandle(11), 14, "Dune.epub", 3_000)
}

#[tokio::test]
async fn root_and_nested_paths_resolve() {
    let fs = sample_tree();

    assert_eq!(
        resolve_dir(&fs, &RemotePath::root(), None).await.unwrap(),
        ObjectHandle::ROOT
    );
    assert_eq!(
        resolve_dir(&fs, &RemotePath::parse("/Books").unwrap(), None)
            .await
            .unwrap(),
        ObjectHandle(10)
    );
    assert_eq!(
        resolve_dir(&fs, &RemotePath::parse("/Books/Sci-Fi/").unwrap(), None)
            .await
            .unwrap(),
        ObjectHandle(11)
    );
}

#[tokio::test]
async fn a_file_resolves_with_its_parent_folder() {
    let fs = sample_tree();
    let resolved = resolve_entry(&fs, &RemotePath::parse("/Books/Roman.epub").unwrap(), None)
        .await
        .unwrap();

    assert_eq!(resolved.entry.handle, ObjectHandle(12));
    assert_eq!(resolved.entry.size, Some(1_000));
    assert_eq!(resolved.parent.display(), "/Books");
    assert_eq!(resolved.parent_handle, ObjectHandle(10));
    assert_eq!(resolved.path.display(), "/Books/Roman.epub");
}

#[tokio::test]
async fn paths_are_normalised() {
    for raw in [
        "/Books/Roman.epub",
        "Books/Roman.epub",
        "//Books//./Roman.epub",
        "/Books/./Roman.epub/",
    ] {
        assert_eq!(
            RemotePath::parse(raw).unwrap().display(),
            "/Books/Roman.epub",
            "raw: {raw}"
        );
    }
    assert_eq!(RemotePath::parse("").unwrap(), RemotePath::root());
    assert!(RemotePath::parse("").unwrap().is_root());
}

#[tokio::test]
async fn dotdot_is_rejected_before_touching_the_device() {
    for raw in [
        "/Books/../etc/passwd",
        "..",
        "/..",
        "/Books/..",
        "Books/../../x",
    ] {
        let error = RemotePath::parse(raw).expect_err("`..` must be rejected");
        assert!(
            error.to_string().contains(".."),
            "message should explain the rule: {error}"
        );
    }
}

#[tokio::test]
async fn missing_paths_report_the_requested_path() {
    let fs = sample_tree();
    let error = resolve_entry(
        &fs,
        &RemotePath::parse("/Books/Missing.epub").unwrap(),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(
        error.to_string(),
        "`/Books/Missing.epub` does not exist on the device"
    );

    let error = resolve_dir(&fs, &RemotePath::parse("/Nope/Books").unwrap(), None)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "`/Nope/Books` does not exist on the device"
    );
}

#[tokio::test]
async fn descending_into_a_file_is_refused() {
    let fs = sample_tree();
    let error = resolve_dir(
        &fs,
        &RemotePath::parse("/Books/Roman.epub/Chapter1").unwrap(),
        None,
    )
    .await
    .unwrap_err();
    assert_eq!(error.to_string(), "`/Books/Roman.epub` is not a directory");
}

#[tokio::test]
async fn names_match_case_insensitively_when_unambiguous() {
    let fs = sample_tree();
    let resolved = resolve_entry(&fs, &RemotePath::parse("/books/roman.EPUB").unwrap(), None)
        .await
        .unwrap();
    assert_eq!(resolved.entry.name, "Roman.epub");
}

#[tokio::test]
async fn ambiguous_case_variants_are_not_guessed() {
    let fs = FakeFs::default()
        .file(ObjectHandle::ROOT, 1, "Same.txt", 1)
        .file(ObjectHandle::ROOT, 2, "same.txt", 2);
    assert!(find_in(&fs.children[&0], "SAME.TXT").is_none());
    // An exact match still wins over the ambiguity.
    assert_eq!(
        find_in(&fs.children[&0], "same.txt").unwrap().handle,
        ObjectHandle(2)
    );
}

#[tokio::test]
async fn listings_put_folders_first_and_sort_by_name() {
    let fs = sample_tree();
    let listing = read_dir(&fs, &RemotePath::parse("/Books").unwrap(), None)
        .await
        .unwrap();

    let names: Vec<&str> = listing
        .entries
        .iter()
        .map(|entry| entry.name.as_str())
        .collect();
    assert_eq!(names, vec!["Sci-Fi", "Roman.epub", "Über Bücher.pdf"]);
    assert_eq!(listing.directory_count(), 1);
    assert_eq!(listing.file_count(), 2);
}

#[tokio::test]
async fn unicode_and_spaces_survive_parsing_and_lookup() {
    let fs = sample_tree();
    let path = RemotePath::parse("/Books/Über Bücher.pdf").unwrap();
    let resolved = resolve_entry(&fs, &path, None).await.unwrap();
    assert_eq!(resolved.entry.name, "Über Bücher.pdf");
    assert_eq!(resolved.entry.size, Some(2_000));
}

#[test]
fn entry_names_are_single_segments() {
    assert!(is_valid_entry_name("Roman.epub"));
    assert!(is_valid_entry_name("Über Bücher.pdf"));
    assert!(!is_valid_entry_name(""));
    assert!(!is_valid_entry_name("."));
    assert!(!is_valid_entry_name(".."));
    assert!(!is_valid_entry_name("a/b"));
    assert!(!is_valid_entry_name("nul\0byte"));
}

#[test]
fn split_parent_and_join_round_trip() {
    let path = RemotePath::parse("/Books/Sci-Fi/Dune.epub").unwrap();
    let (parent, name) = path.split_parent().unwrap();
    assert_eq!(parent.display(), "/Books/Sci-Fi");
    assert_eq!(name, "Dune.epub");
    assert_eq!(parent.join(&name).unwrap(), path);
    assert!(RemotePath::root().split_parent().is_none());
    assert_eq!(RemotePath::root().display(), "/");
    assert!(RemotePath::parse("/a").unwrap().parent().unwrap().is_root());
}
