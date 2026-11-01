//! Storage selection rules, exercised through the public API.
//!
//! These tests encode the guarantees from the project plan: aliases are derived from metadata,
//! never from enumeration order; ambiguous aliases fail with the candidate IDs; and a numeric ID
//! always wins.

use booxmtp::error::Error;
use booxmtp::storage::{AliasSignal, StorageAlias, StorageCatalog, StorageEntry, StorageSelector};
use mtp_rs::mtp::{StorageId, StorageType};

fn internal(id: u64) -> StorageEntry {
    StorageEntry::new(StorageId(id), "Internal storage")
        .with_capacity(64 * 1024 * 1024 * 1024, 24 * 1024 * 1024 * 1024)
        .with_removable(false)
        .with_writable(true)
}

fn sd(id: u64) -> StorageEntry {
    StorageEntry::new(StorageId(id), "SD Card")
        .with_capacity(128 * 1024 * 1024 * 1024, 90 * 1024 * 1024 * 1024)
        .with_removable(true)
        .with_writable(true)
}

#[test]
fn typical_two_storage_device_resolves_both_aliases() {
    // Deliberately unsorted input: the catalog must not rely on enumeration order.
    let catalog = StorageCatalog::new([sd(131_073), internal(65_537)]);

    let (internal_entry, sd_entry) = (
        catalog.find(&StorageSelector::Internal).unwrap(),
        catalog.find(&StorageSelector::Sd).unwrap(),
    );
    assert_eq!(internal_entry.numeric_id(), 65_537);
    assert_eq!(sd_entry.numeric_id(), 131_073);
    assert_eq!(internal_entry.alias(), Some(StorageAlias::Internal));
    assert_eq!(sd_entry.alias(), Some(StorageAlias::Sd));
}

#[test]
fn a_numeric_id_always_works_and_beats_aliases() {
    let catalog = StorageCatalog::new([internal(65_537), sd(131_073)]);
    let selector = StorageSelector::parse("131073").unwrap();
    assert_eq!(catalog.find(&selector).unwrap().numeric_id(), 131_073);

    // Even for a storage the alias heuristic would never pick, the ID resolves.
    let opaque = StorageEntry::new(StorageId(7), "Volume");
    let catalog = StorageCatalog::new([opaque]);
    assert_eq!(
        catalog
            .find(&StorageSelector::Id(StorageId(7)))
            .unwrap()
            .numeric_id(),
        7
    );
}

#[test]
fn unknown_numeric_id_lists_the_available_storages() {
    let catalog = StorageCatalog::new([internal(65_537)]);
    let error = catalog
        .find(&StorageSelector::Id(StorageId(99)))
        .unwrap_err();
    assert!(matches!(error, Error::StorageNotFound { id: 99, .. }));
    assert!(error.to_string().contains("99"));
    assert!(error.hint().unwrap().contains("booxmtp storages"));
}

#[test]
fn ambiguous_alias_names_the_candidates_and_never_guesses() {
    let catalog = StorageCatalog::new([
        StorageEntry::new(StorageId(1), "First fixed storage").with_removable(false),
        StorageEntry::new(StorageId(2), "Second fixed storage").with_removable(false),
    ]);

    let error = catalog.find(&StorageSelector::Internal).unwrap_err();
    assert!(matches!(
        error,
        Error::StorageAliasAmbiguous {
            alias: StorageAlias::Internal,
            ..
        }
    ));
    let message = error.to_string();
    assert!(message.contains("ambiguous"));
    // The hint must tell the user how to disambiguate.
    assert!(
        error.hint().unwrap().contains("--storage 65537") || error.hint().unwrap().contains("ID")
    );
}

#[test]
fn missing_sd_card_is_an_error_not_a_simulation() {
    let catalog = StorageCatalog::new([internal(65_537)]);
    let error = catalog.find(&StorageSelector::Sd).unwrap_err();
    assert!(matches!(
        error,
        Error::StorageAliasUnavailable {
            alias: StorageAlias::Sd,
            ..
        }
    ));
    let hint = error.hint().unwrap();
    assert!(hint.contains("microSD") && hint.contains("booxmtp storages"));
}

#[test]
fn description_keywords_are_only_a_weak_signal() {
    let entry = StorageEntry::new(StorageId(1), "Internal shared storage");
    assert_eq!(
        entry.alias_signal(),
        Some((StorageAlias::Internal, AliasSignal::Weak))
    );

    let entry =
        StorageEntry::new(StorageId(1), "SD card").with_storage_type(StorageType::RemovableRam);
    assert_eq!(
        entry.alias_signal(),
        Some((StorageAlias::Sd, AliasSignal::Strong))
    );

    // Declared metadata overrides a contradictory description.
    let entry = StorageEntry::new(StorageId(1), "SD card").with_storage_type(StorageType::FixedRam);
    assert_eq!(
        entry.alias_signal(),
        Some((StorageAlias::Internal, AliasSignal::Strong))
    );
}

#[test]
fn selector_parsing_is_explicit() {
    assert_eq!(
        StorageSelector::parse("internal").unwrap(),
        StorageSelector::Internal
    );
    assert_eq!(
        StorageSelector::parse("  SD  ").unwrap(),
        StorageSelector::Sd
    );
    assert_eq!(
        StorageSelector::parse("65537").unwrap(),
        StorageSelector::Id(StorageId(65_537))
    );
    assert!(matches!(
        StorageSelector::parse("sdcard0"),
        Err(Error::InvalidStorageSelector { .. })
    ));
}

#[test]
fn read_only_storage_is_reported_as_not_writable() {
    let catalog = StorageCatalog::new([StorageEntry::new(StorageId(1), "Internal storage")
        .with_removable(false)
        .with_writable(false)]);
    let entry = catalog.find(&StorageSelector::Internal).unwrap();
    assert!(!entry.is_writable());
    assert_eq!(entry.writable, Some(false));
}

#[test]
fn storages_are_listed_in_id_order_and_unknown_sizes_stay_unknown() {
    let catalog = StorageCatalog::new([
        StorageEntry::new(StorageId(131_073), "SD Card").with_removable(true),
        internal(65_537),
    ]);
    let ids: Vec<u64> = catalog
        .entries()
        .iter()
        .map(|entry| entry.numeric_id())
        .collect();
    assert_eq!(ids, vec![65_537, 131_073]);

    let first = &catalog.entries()[1];
    assert_eq!(first.capacity_bytes, None);
    assert_eq!(first.free_bytes, None);
    assert_eq!(first.label(), "ID 131073 (SD Card)");
}
