//! The UI-agnostic session façade.
//!
//! Every frontend — today the CLI, later a TUI and a GUI — talks to a [`Session`]. It owns the
//! open MTP device and the storage list, and exposes the operations as ordinary `async` methods
//! that return plain data structures:
//!
//! ```no_run
//! # use booxmtp::{RemotePath, Session};
//! # async fn example() -> booxmtp::Result<()> {
//! let session = Session::connect(None).await?;
//! let storage = session.select_storage("internal")?;
//! let listing = session
//!     .list_dir(storage.id(), &RemotePath::parse("/Books")?, None)
//!     .await?;
//! for entry in &listing.entries {
//!     println!("{} {}", entry.kind, entry.name);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Nothing in here prints, prompts, or knows about exit codes, so embedding the same calls in a
//! event loop is straightforward. Progress is reported through callbacks (see
//! [`crate::transfer`]), cancellation through `mtp-rs`'s [`CancelToken`].

use std::fmt;

use mtp_rs::CancelToken;
use mtp_rs::mtp::{Capabilities, DeviceInfo, MtpDevice, Storage, StorageId};

use crate::device::{self, DeviceSummary};
use crate::error::{Error, MtpResultExt, Result};
use crate::remote_path::{DirListing, RemoteEntry, RemotePath, read_dir};
use crate::storage::{StorageCatalog, StorageEntry, StorageSelector};
use crate::transfer::{self, DownloadReport, ProgressCallback, TransferOptions, UploadReport};

/// A resolved storage: its metadata and the live handle to operate on.
///
/// Implemented as a small struct (rather than a tuple) so it is printable and self-describing when
/// a frontend shows “operating on …” state.
pub struct StorageHandle<'a> {
    /// Catalog metadata, including the resolved alias and free space.
    pub entry: &'a StorageEntry,
    /// Live handle used for all device calls.
    pub storage: &'a Storage,
}

impl StorageHandle<'_> {
    /// Session-scoped storage ID.
    pub fn id(&self) -> StorageId {
        self.entry.id
    }

    /// Device-provided description, e.g. `SD Card`.
    pub fn description(&self) -> &str {
        &self.entry.description
    }
}

impl fmt::Debug for StorageHandle<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StorageHandle")
            .field("id", &self.entry.numeric_id())
            .field("description", &self.entry.description)
            .field("alias", &self.entry.alias())
            .finish()
    }
}

/// One open MTP device plus its cached storage list.
///
/// The storage list is read once when the session is created. Call
/// [`reload_storages`](Self::reload_storages) after the user inserts or removes a card.
pub struct Session {
    device: MtpDevice,
    summary: DeviceSummary,
    storages: Vec<Storage>,
    catalog: StorageCatalog,
}

impl Session {
    /// Discover and open a device.
    ///
    /// `selector` is a serial number (`--device`) or `None` to use the only attached device.
    pub async fn connect(selector: Option<&str>) -> Result<Self> {
        let (device, summary) = device::open(selector).await?;
        Self::attach(device, summary).await
    }

    /// Wrap an already-open device. Useful for tests (virtual devices) and embedding.
    pub async fn attach(device: MtpDevice, summary: DeviceSummary) -> Result<Self> {
        let storages = device
            .storages()
            .await
            .mtp_context("reading the device's storage list")?;
        if storages.is_empty() {
            return Err(Error::NoStorage);
        }
        let catalog = StorageCatalog::from_storages(&storages);
        tracing::debug!(
            device = %summary.label(),
            storages = storages.len(),
            "session ready"
        );
        Ok(Self {
            device,
            summary,
            storages,
            catalog,
        })
    }

    /// The underlying device handle, for operations this façade does not wrap.
    pub fn device(&self) -> &MtpDevice {
        &self.device
    }

    /// USB-level description of the device.
    pub fn summary(&self) -> &DeviceSummary {
        &self.summary
    }

    /// Protocol-level device information (model, firmware, serial).
    pub fn device_info(&self) -> &DeviceInfo {
        self.device.device_info()
    }

    /// What the device claims to support.
    pub fn capabilities(&self) -> &Capabilities {
        self.device.capabilities()
    }

    /// The storage catalog with resolved `internal`/`sd` aliases.
    pub fn catalog(&self) -> &StorageCatalog {
        &self.catalog
    }

    /// Re-read the storage list (e.g. after a card was inserted) and re-resolve the aliases.
    pub async fn reload_storages(&mut self) -> Result<&StorageCatalog> {
        let storages = self
            .device
            .storages()
            .await
            .mtp_context("refreshing the device's storage list")?;
        if storages.is_empty() {
            return Err(Error::NoStorage);
        }
        self.storages = storages;
        self.catalog = StorageCatalog::from_storages(&self.storages);
        Ok(&self.catalog)
    }

    /// Metadata of one storage.
    pub fn storage_entry(&self, id: StorageId) -> Option<&StorageEntry> {
        self.catalog.get(id)
    }

    /// The live storage handle for an ID.
    pub fn storage(&self, id: StorageId) -> Result<&Storage> {
        self.storages
            .iter()
            .find(|storage| storage.id() == id)
            .ok_or_else(|| Error::StorageNotFound {
                id: id.0,
                available: self
                    .catalog
                    .entries()
                    .iter()
                    .map(StorageEntry::label)
                    .collect(),
            })
    }

    /// Resolve a `--storage` value (alias or numeric ID) to the catalog entry and live handle.
    pub fn select_storage(&self, selector: &str) -> Result<StorageHandle<'_>> {
        let parsed = StorageSelector::parse(selector)?;
        let entry = self.catalog.find(&parsed)?;
        let storage = self.storage(entry.id)?;
        Ok(StorageHandle { entry, storage })
    }

    /// List one remote folder of a storage.
    pub async fn list_dir(
        &self,
        storage_id: StorageId,
        path: &RemotePath,
        cancel: Option<&CancelToken>,
    ) -> Result<DirListing> {
        let storage = self.storage(storage_id)?;
        read_dir(storage, path, cancel).await
    }

    /// Download one remote file from a storage.
    pub async fn download_file(
        &self,
        storage_id: StorageId,
        remote: &RemotePath,
        local: &std::path::Path,
        options: &TransferOptions,
        on_progress: ProgressCallback<'_>,
    ) -> Result<DownloadReport> {
        let storage = self.storage(storage_id)?;
        transfer::download_file(storage, remote, local, options, on_progress).await
    }

    /// Upload one local file to a storage.
    pub async fn upload_file(
        &self,
        storage_id: StorageId,
        local: &std::path::Path,
        remote: &RemotePath,
        options: &TransferOptions,
        on_progress: ProgressCallback<'_>,
    ) -> Result<UploadReport> {
        if !self.capabilities().can_upload {
            return Err(Error::Unsupported {
                operation: "uploading files",
            });
        }
        let storage = self.storage(storage_id)?;
        transfer::upload_file(storage, local, remote, options, on_progress).await
    }

    /// Create one remote folder on a storage.
    pub async fn create_dir(
        &self,
        storage_id: StorageId,
        remote: &RemotePath,
        cancel: Option<&CancelToken>,
    ) -> Result<RemoteEntry> {
        if !self.capabilities().can_create_folder {
            return Err(Error::Unsupported {
                operation: "creating folders",
            });
        }
        let storage = self.storage(storage_id)?;
        transfer::create_dir(storage, remote, cancel).await
    }

    /// Close the session gracefully.
    ///
    /// Dropping a [`Session`] without closing is safe, but an explicit close releases the USB
    /// interface immediately, which makes a subsequent run faster.
    pub async fn close(self) -> Result<()> {
        self.device
            .close()
            .await
            .mtp_context("closing the MTP device")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A TUI/GUI keeps the session in a background task, so it must be movable between threads.
    /// This assertion fails to compile if a dependency change breaks that assumption.
    #[test]
    fn session_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Session>();
        assert_send_sync::<MtpDevice>();
    }
}
