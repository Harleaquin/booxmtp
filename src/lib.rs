//! `booxmtp` — file transfer between a host and an Android-based MTP e-reader.
//!
//! The crate is split so that the same core can serve a CLI today and a TUI or GUI later:
//!
//! | Module | Responsibility |
//! |--------|----------------|
//! | [`device`] | USB discovery and strict device selection |
//! | [`storage`] | storage metadata and `internal`/`sd` alias resolution |
//! | [`location`] | a panel target: a host folder or a storage + remote path |
//! | [`remote_path`] | normalisation and segment-wise resolution of remote paths |
//! | [`transfer`] | streaming download/upload, atomic local writes, verification |
//! | [`tree`] | recursive copy, move and delete across host and reader |
//! | [`session`] | UI-agnostic façade tying the above together |
//! | [`output`] | byte/date/name formatting, tables, progress line |
//! | [`error`] | one error type with hints and stable exit codes |
//! | [`cli`] | `clap` definitions and command dispatch |
//!
//! No module except [`cli`] writes to stdout/stderr or exits the process, and no module except
//! [`cli`] depends on `clap`.
//!
//! # Example
//!
//! ```no_run
//! use booxmtp::{RemotePath, Session};
//!
//! # async fn example() -> booxmtp::Result<()> {
//! let session = Session::connect(None).await?;
//! for storage in session.catalog().entries() {
//!     println!("{}: {}", storage.numeric_id(), storage.description);
//! }
//!
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

#![deny(missing_docs)]

pub mod cli;
pub mod device;
pub mod error;
pub mod location;
pub mod output;
pub mod remote_path;
pub mod session;
pub mod storage;
pub mod transfer;
pub mod tree;
pub mod tui;

pub use error::{Error, Result};
pub use location::Location;
pub use remote_path::{DirListing, EntryKind, RemoteEntry, RemotePath};
pub use session::{Session, StorageHandle};
pub use storage::{StorageAlias, StorageCatalog, StorageEntry, StorageSelector};
pub use transfer::{DownloadReport, TransferOptions, TransferProgress, UploadReport};
