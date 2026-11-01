//! Storage enumeration and alias resolution.
//!
//! An MTP device reports one or more *storages* (internal flash, microSD card, …). Their numeric
//! IDs are session-scoped: they are re-read on every run and never written to a configuration
//! file. Users may nevertheless want to say “the internal one” or “the SD card”, so this module
//! derives the aliases `internal` and `sd` from the metadata the device actually reports.
//!
//! # Alias rules
//!
//! A storage is classified by the strongest evidence available, and the alias is only assigned
//! when exactly one storage matches:
//!
//! 1. **Strong evidence** — the MTP storage type says fixed (`FixedRom`/`FixedRam`) or removable
//!    (`RemovableRom`/`RemovableRam`). This is what the device declares, so it wins.
//! 2. **Weak evidence** — the human-readable description contains a familiar keyword such as
//!    “internal” or “SD card”. Only consulted when no storage has strong evidence.
//!
//! If two storages match the same alias, the alias is *ambiguous* and refused with an error that
//! lists the candidates: the program never falls back to “first storage = internal”. If nothing
//! matches, the alias is *unavailable* (that is also the honest answer when no microSD card is
//! inserted). Both cases point the user at `booxmtp storages` and an explicit numeric ID.

use std::fmt;
use std::str::FromStr;

use mtp_rs::mtp::{FilesystemType, Storage, StorageId, StorageType};

use crate::error::{Error, Result};

/// The two optional, human-friendly storage names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageAlias {
    /// The device's built-in storage (`--storage internal`).
    Internal,
    /// A removable card (`--storage sd`).
    Sd,
}

impl StorageAlias {
    /// Every alias, in display order.
    pub const ALL: [StorageAlias; 2] = [StorageAlias::Internal, StorageAlias::Sd];

    /// The spelling accepted on the command line and printed in tables.
    pub fn as_str(self) -> &'static str {
        match self {
            StorageAlias::Internal => "internal",
            StorageAlias::Sd => "sd",
        }
    }
}

impl fmt::Display for StorageAlias {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for StorageAlias {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self> {
        StorageSelector::parse(raw).and_then(|selector| match selector {
            StorageSelector::Internal => Ok(StorageAlias::Internal),
            StorageSelector::Sd => Ok(StorageAlias::Sd),
            StorageSelector::Id(_) => Err(Error::InvalidStorageSelector {
                raw: raw.to_string(),
            }),
        })
    }
}

/// How strongly the metadata supports an alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AliasSignal {
    /// The description merely contains a keyword; a hint, not a fact.
    Weak,
    /// The device's declared storage type says fixed or removable.
    Strong,
}

/// Which storage a `--storage` argument refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StorageSelector {
    /// `--storage internal`
    Internal,
    /// `--storage sd`
    Sd,
    /// `--storage 65537` — always available and always wins over heuristics.
    Id(StorageId),
}

impl StorageSelector {
    /// Parse the user-provided selector: an alias or a numeric MTP storage ID.
    pub fn parse(raw: &str) -> Result<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(Error::InvalidStorageSelector {
                raw: raw.to_string(),
            });
        }
        if trimmed.eq_ignore_ascii_case("internal") {
            return Ok(StorageSelector::Internal);
        }
        // Accept a few obvious spellings; anything else must be a numeric ID.
        if ["sd", "sdcard", "sd-card", "external"]
            .iter()
            .any(|alias| trimmed.eq_ignore_ascii_case(alias))
        {
            return Ok(StorageSelector::Sd);
        }
        if let Ok(id) = trimmed.parse::<u64>() {
            return Ok(StorageSelector::Id(StorageId(id)));
        }
        Err(Error::InvalidStorageSelector {
            raw: raw.to_string(),
        })
    }
}

impl FromStr for StorageSelector {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self> {
        StorageSelector::parse(raw)
    }
}

impl From<StorageAlias> for StorageSelector {
    fn from(alias: StorageAlias) -> Self {
        match alias {
            StorageAlias::Internal => StorageSelector::Internal,
            StorageAlias::Sd => StorageSelector::Sd,
        }
    }
}

/// Everything `booxmtp` knows about one storage, in backend-neutral form.
///
/// Optional fields are `None` when the device did not report the value. In particular a capacity
/// or free-space value of `0` is treated as *unknown*, never as “0 bytes available”: some MTP
/// implementations leave the field empty and a `0` there must not block an upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageEntry {
    /// Session-scoped MTP storage ID. Do not persist it across runs.
    pub id: StorageId,
    /// Device-provided description, e.g. “Internal storage” or “SD Card”.
    pub description: String,
    /// Volume identifier, when the device provides one.
    pub volume_identifier: String,
    /// Total size in bytes, when known.
    pub capacity_bytes: Option<u64>,
    /// Free space in bytes, when known.
    pub free_bytes: Option<u64>,
    /// Whether the medium is removable, when known.
    pub removable: Option<bool>,
    /// Whether the storage accepts writes, when known.
    pub writable: Option<bool>,
    /// Raw MTP storage type, kept for diagnostics.
    pub storage_type: Option<StorageType>,
    /// Raw MTP filesystem type, kept for diagnostics.
    pub filesystem_type: Option<FilesystemType>,
    /// Assigned by [`StorageCatalog`], never by the device.
    alias: Option<StorageAlias>,
}

impl StorageEntry {
    /// Start a minimal entry. Mainly useful for tests and for future MTP backends.
    pub fn new(id: StorageId, description: impl Into<String>) -> Self {
        Self {
            id,
            description: description.into(),
            volume_identifier: String::new(),
            capacity_bytes: None,
            free_bytes: None,
            removable: None,
            writable: None,
            storage_type: None,
            filesystem_type: None,
            alias: None,
        }
    }

    /// Build an entry from a live `mtp-rs` storage handle.
    pub fn from_storage(storage: &Storage) -> Self {
        Self::from_info(storage.id(), storage.info())
    }

    /// Build an entry from an MTP storage-info record.
    pub fn from_info(id: StorageId, info: &mtp_rs::mtp::StorageInfo) -> Self {
        let capacity = (info.total_capacity > 0).then_some(info.total_capacity);
        Self {
            id,
            description: info.description.clone(),
            volume_identifier: info.volume_identifier.clone(),
            capacity_bytes: capacity,
            // Free space is only meaningful once the device reported a capacity.
            free_bytes: capacity.map(|_| info.free_space),
            removable: removable_from_type(info.storage_type),
            writable: Some(info.is_writable),
            storage_type: Some(info.storage_type),
            filesystem_type: Some(info.filesystem_type),
            alias: None,
        }
    }

    /// Set capacity and free space (builder style, for tests).
    pub fn with_capacity(mut self, capacity_bytes: u64, free_bytes: u64) -> Self {
        self.capacity_bytes = Some(capacity_bytes);
        self.free_bytes = Some(free_bytes);
        self
    }

    /// Set the removability flag (builder style, for tests).
    pub fn with_removable(mut self, removable: bool) -> Self {
        self.removable = Some(removable);
        self
    }

    /// Set the writable flag (builder style, for tests).
    pub fn with_writable(mut self, writable: bool) -> Self {
        self.writable = Some(writable);
        self
    }

    /// Set the raw MTP storage type (builder style, for tests).
    pub fn with_storage_type(mut self, storage_type: StorageType) -> Self {
        self.removable = removable_from_type(storage_type);
        self.storage_type = Some(storage_type);
        self
    }

    /// The numeric ID as printed on the command line.
    pub fn numeric_id(&self) -> u64 {
        self.id.0
    }

    /// The alias this entry was assigned by its [`StorageCatalog`], if any.
    pub fn alias(&self) -> Option<StorageAlias> {
        self.alias
    }

    /// Whether writes should be attempted.
    pub fn is_writable(&self) -> bool {
        self.writable.unwrap_or(false)
    }

    /// A label suitable for error messages, e.g. `ID 65537 (Internal storage)`.
    pub fn label(&self) -> String {
        format!("ID {} ({})", self.numeric_id(), self.description)
    }

    /// Which alias this storage *suggests*, and how strongly.
    ///
    /// Metadata wins over the description: mislabelled descriptions exist, a declared storage type
    /// is what the device actually implements.
    pub fn alias_signal(&self) -> Option<(StorageAlias, AliasSignal)> {
        if let Some(removable) = self.removable {
            let alias = if removable {
                StorageAlias::Sd
            } else {
                StorageAlias::Internal
            };
            return Some((alias, AliasSignal::Strong));
        }

        let description = self.description.to_lowercase();
        if SD_DESCRIPTION_KEYWORDS
            .iter()
            .any(|keyword| description.contains(keyword))
        {
            return Some((StorageAlias::Sd, AliasSignal::Weak));
        }
        if INTERNAL_DESCRIPTION_KEYWORDS
            .iter()
            .any(|keyword| description.contains(keyword))
        {
            return Some((StorageAlias::Internal, AliasSignal::Weak));
        }
        None
    }
}

/// MTP storage types that are removable. `Undefined`/`Other` deliberately yield `None`.
fn removable_from_type(storage_type: StorageType) -> Option<bool> {
    match storage_type {
        StorageType::FixedRom | StorageType::FixedRam => Some(false),
        StorageType::RemovableRom | StorageType::RemovableRam => Some(true),
        StorageType::Undefined | StorageType::Other => None,
        // `StorageType` is `#[non_exhaustive]`: unknown future values carry no evidence.
        _ => None,
    }
}

/// Description keywords used only as a *weak* fallback when the device declares no storage type.
const SD_DESCRIPTION_KEYWORDS: &[&str] = &[
    "sd card",
    "sdcard",
    "sd-card",
    "microsd",
    "micro sd",
    "external",
    "removable",
    "memory card",
];

/// Description keywords used only as a *weak* fallback when the device declares no storage type.
const INTERNAL_DESCRIPTION_KEYWORDS: &[&str] = &[
    "internal",
    "built-in",
    "built in",
    "onboard",
    "on-board",
    "phone storage",
    "device storage",
];

/// Why an alias could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AliasIssue {
    alias: StorageAlias,
    /// Storages that matched, as labels. Empty when nothing matched.
    candidates: Vec<String>,
}

/// All storages of one device, with aliases resolved once at construction.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StorageCatalog {
    entries: Vec<StorageEntry>,
    issues: Vec<AliasIssue>,
}

impl StorageCatalog {
    /// Build a catalog and infer the `internal`/`sd` aliases.
    pub fn new(entries: impl IntoIterator<Item = StorageEntry>) -> Self {
        let mut entries: Vec<StorageEntry> = entries.into_iter().collect();
        // Sort for stable output; the device's order carries no meaning.
        entries.sort_by(|left, right| {
            left.numeric_id()
                .cmp(&right.numeric_id())
                .then_with(|| left.description.cmp(&right.description))
        });

        let issues = assign_aliases(&mut entries);
        Self { entries, issues }
    }

    /// Build a catalog from live `mtp-rs` storages.
    pub fn from_storages(storages: &[Storage]) -> Self {
        Self::new(
            storages
                .iter()
                .map(StorageEntry::from_storage)
                .collect::<Vec<_>>(),
        )
    }

    /// All storages, sorted by numeric ID.
    pub fn entries(&self) -> &[StorageEntry] {
        &self.entries
    }

    /// Look up a storage by its session-scoped ID.
    pub fn get(&self, id: StorageId) -> Option<&StorageEntry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    /// Resolve a `--storage` selector.
    ///
    /// Numeric IDs always work. Aliases only work when exactly one storage matches; otherwise the
    /// error lists the alternatives so the user can pick an explicit ID.
    pub fn find(&self, selector: &StorageSelector) -> Result<&StorageEntry> {
        match selector {
            StorageSelector::Id(id) => self.get(*id).ok_or_else(|| Error::StorageNotFound {
                id: id.0,
                available: self.labels(),
            }),
            StorageSelector::Internal | StorageSelector::Sd => {
                let alias = match selector {
                    StorageSelector::Internal => StorageAlias::Internal,
                    StorageSelector::Sd => StorageAlias::Sd,
                    StorageSelector::Id(_) => unreachable!("handled above"),
                };
                if let Some(entry) = self.entries.iter().find(|entry| entry.alias == Some(alias)) {
                    return Ok(entry);
                }
                match self.issues.iter().find(|issue| issue.alias == alias) {
                    Some(issue) if !issue.candidates.is_empty() => {
                        Err(Error::StorageAliasAmbiguous {
                            alias,
                            candidates: issue.candidates.clone(),
                        })
                    }
                    _ => Err(Error::StorageAliasUnavailable {
                        alias,
                        available: self.labels(),
                    }),
                }
            }
        }
    }

    /// Labels of all storages, used in error messages.
    fn labels(&self) -> Vec<String> {
        self.entries.iter().map(StorageEntry::label).collect()
    }
}

/// Assign the two aliases following the rules in the [module docs](self).
fn assign_aliases(entries: &mut [StorageEntry]) -> Vec<AliasIssue> {
    let mut issues = Vec::new();

    for alias in StorageAlias::ALL {
        let matching = |minimum: AliasSignal| -> Vec<usize> {
            entries
                .iter()
                .enumerate()
                .filter(|(_, entry)| {
                    entry
                        .alias_signal()
                        .is_some_and(|(signal_alias, strength)| {
                            signal_alias == alias && strength >= minimum
                        })
                })
                .map(|(index, _)| index)
                .collect()
        };

        // Strong evidence first; weak evidence only if the device declared nothing usable.
        let mut candidates = matching(AliasSignal::Strong);
        if candidates.is_empty() {
            candidates = matching(AliasSignal::Weak);
        }

        match candidates.as_slice() {
            [only] => entries[*only].alias = Some(alias),
            [] => issues.push(AliasIssue {
                alias,
                candidates: Vec::new(),
            }),
            many => issues.push(AliasIssue {
                alias,
                candidates: many.iter().map(|index| entries[*index].label()).collect(),
            }),
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;

    fn internal(id: u64) -> StorageEntry {
        StorageEntry::new(StorageId(id), "Internal storage")
            .with_removable(false)
            .with_writable(true)
    }

    fn sd(id: u64) -> StorageEntry {
        StorageEntry::new(StorageId(id), "SD Card")
            .with_removable(true)
            .with_writable(true)
    }

    #[test]
    fn alias_parsing_accepts_the_documented_spellings() {
        assert_eq!(
            StorageSelector::parse("internal").unwrap(),
            StorageSelector::Internal
        );
        assert_eq!(StorageSelector::parse("SD").unwrap(), StorageSelector::Sd);
        assert_eq!(
            StorageSelector::parse("sdcard").unwrap(),
            StorageSelector::Sd
        );
        assert_eq!(
            StorageSelector::parse("65537").unwrap(),
            StorageSelector::Id(StorageId(65537))
        );
        assert!(matches!(
            StorageSelector::parse("music"),
            Err(Error::InvalidStorageSelector { .. })
        ));
        assert!(matches!(
            StorageSelector::parse(""),
            Err(Error::InvalidStorageSelector { .. })
        ));
    }

    #[test]
    fn two_storages_get_one_alias_each() {
        let catalog = StorageCatalog::new([sd(131_073), internal(65_537)]);
        assert_eq!(
            catalog
                .find(&StorageSelector::Internal)
                .unwrap()
                .numeric_id(),
            65_537
        );
        assert_eq!(
            catalog.find(&StorageSelector::Sd).unwrap().numeric_id(),
            131_073
        );
    }

    #[test]
    fn ids_are_sorted_and_always_selectable() {
        let catalog = StorageCatalog::new([sd(131_073), internal(65_537)]);
        let ids: Vec<u64> = catalog.entries().iter().map(|e| e.numeric_id()).collect();
        assert_eq!(ids, vec![65_537, 131_073]);
        assert!(
            catalog
                .find(&StorageSelector::parse("131073").unwrap())
                .is_ok()
        );
        assert!(matches!(
            catalog.find(&StorageSelector::parse("42").unwrap()),
            Err(Error::StorageNotFound { id: 42, .. })
        ));
    }

    #[test]
    fn first_storage_is_never_assumed_to_be_internal() {
        // Both storages claim to be fixed: the alias must stay unresolved instead of picking one.
        let catalog = StorageCatalog::new([
            StorageEntry::new(StorageId(1), "Storage A").with_removable(false),
            StorageEntry::new(StorageId(2), "Storage B").with_removable(false),
        ]);
        assert!(matches!(
            catalog.find(&StorageSelector::Internal),
            Err(Error::StorageAliasAmbiguous { .. })
        ));
        assert!(matches!(
            catalog.find(&StorageSelector::Sd),
            Err(Error::StorageAliasUnavailable { .. })
        ));
    }

    #[test]
    fn description_is_only_a_weak_fallback() {
        // No storage type reported: the description decides, unambiguously.
        let catalog = StorageCatalog::new([
            StorageEntry::new(StorageId(1), "Internal shared storage"),
            StorageEntry::new(StorageId(2), "SD card"),
        ]);
        assert_eq!(
            catalog
                .find(&StorageSelector::Internal)
                .unwrap()
                .numeric_id(),
            1
        );
        assert_eq!(catalog.find(&StorageSelector::Sd).unwrap().numeric_id(), 2);
    }

    #[test]
    fn storage_type_beats_a_misleading_description() {
        // "SD Card" in the description, but the device says it is fixed memory: metadata wins.
        let catalog = StorageCatalog::new([
            StorageEntry::new(StorageId(1), "SD Card").with_storage_type(StorageType::FixedRam)
        ]);
        assert_eq!(
            catalog
                .find(&StorageSelector::Internal)
                .unwrap()
                .numeric_id(),
            1
        );
        assert!(matches!(
            catalog.find(&StorageSelector::Sd),
            Err(Error::StorageAliasUnavailable { .. })
        ));
    }

    #[test]
    fn missing_sd_card_produces_an_unavailable_alias_with_a_hint() {
        let catalog = StorageCatalog::new([internal(65_537)]);
        let error = catalog.find(&StorageSelector::Sd).unwrap_err();
        assert!(matches!(
            error,
            Error::StorageAliasUnavailable {
                alias: StorageAlias::Sd,
                ..
            }
        ));
        assert!(error.hint().unwrap().contains("microSD"));
    }

    #[test]
    fn no_metadata_means_no_alias_at_all() {
        let catalog = StorageCatalog::new([StorageEntry::new(StorageId(1), "MTP volume")]);
        assert!(matches!(
            catalog.find(&StorageSelector::Internal),
            Err(Error::StorageAliasUnavailable { .. })
        ));
        // The numeric ID still works, which is the documented escape hatch.
        assert!(catalog.find(&StorageSelector::Id(StorageId(1))).is_ok());
    }

    #[test]
    fn zero_capacity_is_reported_as_unknown() {
        // `StorageInfo` is `#[non_exhaustive]`, so build it via `Default` and set the fields.
        let mut info = mtp_rs::mtp::StorageInfo::default();
        info.total_capacity = 0;
        info.free_space = 0;
        let entry = StorageEntry::from_info(StorageId(7), &info);
        assert_eq!(entry.capacity_bytes, None);
        assert_eq!(entry.free_bytes, None);
    }
}
