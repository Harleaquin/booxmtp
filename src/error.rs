//! Domain error type for `booxmtp`.
//!
//! Every failure the program can report is represented by one [`Error`] variant, which carries
//! three things the frontends need:
//!
//! * a human-readable message ([`std::fmt::Display`]),
//! * an actionable hint where one exists ([`Error::hint`]),
//! * a stable process exit code ([`Error::exit_code`]).
//!
//! The CLI turns these into a message on `stderr` and a process status; a future TUI or GUI can
//! render the same data in a dialog. Keeping the error type free of `clap`/`stdout` knowledge is
//! what makes that reuse possible.
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |------|---------|
//! | 0 | success |
//! | 1 | unclassified failure |
//! | 2 | command-line usage error (emitted by `clap` itself) |
//! | 3 | no MTP device found |
//! | 4 | device selection problem (several devices, unknown serial) |
//! | 5 | storage selection problem (unknown/ambiguous ID or alias) |
//! | 6 | remote path problem (invalid, missing, wrong type) |
//! | 7 | local filesystem problem (missing file, collision, I/O) |
//! | 8 | conflict (remote object already exists, no space left) |
//! | 9 | transfer or device failure (USB, protocol, disconnect) |
//! | 130 | cancelled by the user (Ctrl-C) |

use std::path::PathBuf;

use crate::storage::StorageAlias;

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Stable process exit codes, see the [module docs](self).
pub mod exit_code {
    /// Everything worked.
    pub const SUCCESS: u8 = 0;
    /// Failure without a more specific category.
    pub const FAILURE: u8 = 1;
    /// Command-line usage error (`clap`).
    pub const USAGE: u8 = 2;
    /// No MTP device was found.
    pub const NO_DEVICE: u8 = 3;
    /// The requested device is unknown or the choice is ambiguous.
    pub const DEVICE_SELECTION: u8 = 4;
    /// The requested storage is unknown or the alias is ambiguous.
    pub const STORAGE_SELECTION: u8 = 5;
    /// A remote (MTP object) path is invalid, missing or of the wrong type.
    pub const PATH: u8 = 6;
    /// A local file operation failed.
    pub const LOCAL_IO: u8 = 7;
    /// The operation would overwrite or exceed something (collision, full storage).
    pub const CONFLICT: u8 = 8;
    /// The transfer or the device failed mid-flight.
    pub const TRANSFER: u8 = 9;
    /// Interrupted by the user.
    pub const CANCELLED: u8 = 130;
}

/// Outcome of trying to remove a partially uploaded remote object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PartialCleanup {
    /// The device never created an object, or the backend commits atomically.
    NothingToDo,
    /// The half-written object was removed again.
    Removed,
    /// Cleanup was attempted but failed; a partial object may remain on the device.
    Failed(String),
}

/// All failures `booxmtp` can report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// No MTP-capable device is attached (or none is visible to this user).
    #[error("no MTP device found")]
    NoDevice,

    /// More than one device is attached and `--device` was not given.
    #[error("more than one MTP device is connected")]
    MultipleDevices {
        /// One human-readable label per detected device.
        devices: Vec<String>,
    },

    /// The device reports no storage at all.
    #[error("the device reports no storage")]
    NoStorage,

    /// The operation is understood but not implemented by `booxmtp` yet.
    #[error("{what}")]
    UnsupportedByApp {
        /// What was attempted.
        what: String,
    },

    /// The background I/O worker of the TUI stopped unexpectedly.
    #[error("background I/O worker failed: {message}")]
    Worker {
        /// What went wrong.
        message: String,
    },

    /// The device does not advertise an MTP operation the program needs.
    #[error("this device does not support {operation}")]
    Unsupported {
        /// The unsupported operation, in lower case (e.g. "uploading files").
        operation: &'static str,
    },

    /// `--device <SERIAL>` did not match any detected device.
    #[error("no connected MTP device has the serial number `{requested}`")]
    DeviceNotFound {
        /// The serial number the user asked for.
        requested: String,
        /// One human-readable label per detected device.
        available: Vec<String>,
    },

    /// The `--storage` value is neither an alias nor a numeric storage ID.
    #[error("`{raw}` is not a storage selector")]
    InvalidStorageSelector {
        /// The raw value the user typed.
        raw: String,
    },

    /// An alias (`internal`, `sd`) matches more than one reported storage.
    #[error("the storage alias `{alias}` is ambiguous")]
    StorageAliasAmbiguous {
        /// The alias that could not be resolved.
        alias: StorageAlias,
        /// Storage labels the alias matches.
        candidates: Vec<String>,
    },

    /// No reported storage could be identified as the requested alias.
    #[error("no storage on this device matches the alias `{alias}`")]
    StorageAliasUnavailable {
        /// The alias that could not be resolved.
        alias: StorageAlias,
        /// All storages the device reported, for context.
        available: Vec<String>,
    },

    /// A numeric storage ID is not reported by the device.
    #[error("storage ID {id} is not reported by this device")]
    StorageNotFound {
        /// The numeric ID that was requested.
        id: u64,
        /// All storages the device reported, for context.
        available: Vec<String>,
    },

    /// A write operation was aimed at a read-only storage.
    #[error("storage {storage} is read-only")]
    StorageReadOnly {
        /// Label of the storage, including its ID.
        storage: String,
    },

    /// A remote path is syntactically unusable (e.g. contains `..`).
    #[error("invalid remote path `{path}`: {reason}")]
    InvalidRemotePath {
        /// The raw path as the user wrote it.
        path: String,
        /// Why it was rejected.
        reason: String,
    },

    /// A path segment does not exist on the device.
    #[error("`{path}` does not exist on the device")]
    RemotePathNotFound {
        /// The path that could not be resolved.
        path: String,
    },

    /// A path segment exists but is a file, so it cannot be descended into.
    #[error("`{path}` is not a directory")]
    RemoteNotADirectory {
        /// The offending path prefix.
        path: String,
    },

    /// A file operation was aimed at a directory.
    #[error("`{path}` is a directory")]
    RemoteIsADirectory {
        /// The offending path.
        path: String,
    },

    /// The remote target already exists and overwriting was not requested.
    #[error("`{path}` already exists on the device")]
    RemoteAlreadyExists {
        /// The offending path.
        path: String,
    },

    /// The local source file does not exist.
    #[error("local file `{path}` does not exist", path = .path.display())]
    LocalNotFound {
        /// The path that was checked.
        path: PathBuf,
    },

    /// The local source exists but is not a regular file (directory, socket, …).
    #[error("local path `{path}` is not a regular file", path = .path.display())]
    LocalNotAFile {
        /// The offending path.
        path: PathBuf,
    },

    /// The folder that should receive the downloaded file does not exist.
    #[error("local directory `{path}` does not exist", path = .path.display())]
    LocalDirectoryMissing {
        /// The missing directory.
        path: PathBuf,
    },

    /// The local download destination already exists and overwriting was not requested.
    #[error("local file `{path}` already exists", path = .path.display())]
    LocalAlreadyExists {
        /// The destination that was refused.
        path: PathBuf,
    },

    /// The local download destination is a directory.
    #[error("local path `{path}` is a directory", path = .path.display())]
    LocalIsDirectory {
        /// The destination that was refused.
        path: PathBuf,
    },

    /// The remote storage does not have enough free space for the upload.
    #[error("not enough space on {storage}: {required} bytes needed, {available} available")]
    InsufficientSpace {
        /// Size of the file being uploaded.
        required: u64,
        /// Free space the device reported.
        available: u64,
        /// Label of the target storage.
        storage: String,
    },

    /// A transfer ended early: fewer bytes arrived than the device announced.
    #[error("incomplete transfer of `{path}`: got {transferred} of {expected} bytes")]
    IncompleteTransfer {
        /// Remote path of the object.
        path: String,
        /// Bytes that were actually transferred.
        transferred: u64,
        /// Bytes the device announced.
        expected: u64,
    },

    /// The user (or the embedding application) cancelled the operation.
    #[error("cancelled by user")]
    Cancelled,

    /// An upload failed; the device may or may not still hold a partial object.
    #[error("upload of `{path}` failed: {source}")]
    Upload {
        /// Remote target path.
        path: String,
        /// What happened to a possibly half-written remote object.
        cleanup: PartialCleanup,
        /// The underlying device error.
        #[source]
        source: mtp_rs::Error,
    },

    /// Any other MTP/device failure, annotated with what we were doing.
    #[error("{context}: {source}")]
    Mtp {
        /// What the program was doing (e.g. "listing `/Books`").
        context: String,
        /// The underlying device error.
        #[source]
        source: mtp_rs::Error,
    },

    /// A local filesystem operation failed, annotated with what we were doing.
    #[error("{context}: {source}")]
    Io {
        /// What the program was doing (e.g. "writing `/tmp/x.part`").
        context: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    /// Build an [`Error::Mtp`] with context.
    pub fn mtp(context: impl Into<String>, source: mtp_rs::Error) -> Self {
        Error::Mtp {
            context: context.into(),
            source,
        }
    }

    /// Build an [`Error::Io`] with context.
    pub fn io(context: impl Into<String>, source: std::io::Error) -> Self {
        Error::Io {
            context: context.into(),
            source,
        }
    }

    /// Build an [`Error::InvalidRemotePath`].
    pub fn invalid_remote_path(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Error::InvalidRemotePath {
            path: path.into(),
            reason: reason.into(),
        }
    }

    /// Prefix the *context* of a device or I/O error with what the program was doing.
    ///
    /// Other variants (path problems, collisions, …) are already self-explanatory and pass through
    /// unchanged, so error messages never grow redundant wrappers.
    pub fn contextualize(self, context: impl Into<String>) -> Self {
        let context = context.into();
        match self {
            Error::Mtp { source, .. } => Error::Mtp { context, source },
            Error::Io { source, .. } => Error::Io { context, source },
            other => other,
        }
    }

    /// A short, actionable suggestion for the user, when one exists.
    ///
    /// `None` means the message alone is enough (for example a plain "file already exists").
    pub fn hint(&self) -> Option<String> {
        match self {
            Error::NoDevice => Some(no_device_hint()),
            Error::NoStorage => Some(
                "unlock the reader, make sure a storage volume is mounted, then try again."
                    .to_string(),
            ),
            Error::UnsupportedByApp { .. } => {
                Some("use a local panel in between (download first, then upload).".to_string())
            }
            Error::Unsupported { .. } => Some(
                "the reader's MTP implementation does not advertise this operation; check the \
                 device's USB mode or update its firmware."
                    .to_string(),
            ),
            Error::MultipleDevices { .. } => Some(
                "pick one with `--device <SERIAL>`; `booxmtp devices` lists the serial numbers."
                    .to_string(),
            ),
            Error::DeviceNotFound { .. } => Some(
                "run `booxmtp devices` to see which serial numbers are currently visible."
                    .to_string(),
            ),
            Error::InvalidStorageSelector { .. } => Some(
                "use `internal`, `sd`, or the numeric ID printed by `booxmtp storages`."
                    .to_string(),
            ),
            Error::StorageAliasAmbiguous { .. } => Some(
                "run `booxmtp storages` and pass the explicit ID, e.g. `--storage 65537`."
                    .to_string(),
            ),
            Error::StorageAliasUnavailable { alias, .. } => {
                if *alias == StorageAlias::Sd {
                    Some(
                        "the device does not expose a second, removable storage right now. \
                         Check that the microSD card is inserted and mounted, then run \
                         `booxmtp storages` again — or pass a numeric ID from that list."
                            .to_string(),
                    )
                } else {
                    Some(
                        "run `booxmtp storages` and pass the numeric ID explicitly, e.g. \
                         `--storage 65537`."
                            .to_string(),
                    )
                }
            }
            Error::StorageNotFound { .. } => Some(
                "run `booxmtp storages` to list the IDs this device actually reports."
                    .to_string(),
            ),
            Error::StorageReadOnly { .. } => {
                Some("pick a writable storage; `booxmtp storages` shows the ones that accept writes.".to_string())
            }
            Error::InvalidRemotePath { .. } => Some(
                "remote paths are relative to the storage root: `/` is the root, `/Books/Roman.epub` \
                 a file inside it; `..` is not allowed."
                    .to_string(),
            ),
            Error::RemotePathNotFound { .. } => {
                Some("check the path with `booxmtp ls --storage <S> <PARENT_DIR>`; names are matched case-insensitively if that is unambiguous.".to_string())
            }
            Error::RemoteNotADirectory { .. } | Error::RemoteIsADirectory { .. } => {
                Some("`booxmtp ls` shows which entries are directories.".to_string())
            }
            Error::RemoteAlreadyExists { .. } => Some(
                "choose a different target name; `push --overwrite` replaces an existing object \
                 (MTP replacement is not atomic)."
                    .to_string(),
            ),
            Error::LocalNotFound { .. } => Some("check the path and file permissions.".to_string()),
            Error::LocalDirectoryMissing { .. } => {
                Some("create the target directory first.".to_string())
            }
            Error::LocalNotAFile { .. } => {
                Some("only regular files can be uploaded, and only to a file path.".to_string())
            }
            Error::LocalAlreadyExists { .. } => Some(
                "pass `--overwrite` to replace the local file, or choose a different destination."
                    .to_string(),
            ),
            Error::LocalIsDirectory { .. } => Some(
                "the destination must be a file path including the file name, not a directory."
                    .to_string(),
            ),
            Error::InsufficientSpace { .. } => Some(
                "free space on the device or choose the other storage (`--storage internal|sd`)."
                    .to_string(),
            ),
            Error::IncompleteTransfer { .. } => Some(
                "the device stopped sending data. Reconnect it and try again; the partial local \
                 file was removed."
                    .to_string(),
            ),
            Error::Cancelled => Some("no changes were made beyond what had already completed.".to_string()),
            Error::Upload { cleanup, .. } => Some(match cleanup {
                PartialCleanup::Removed => {
                    "the half-written object was removed from the device; nothing to clean up."
                        .to_string()
                }
                PartialCleanup::Failed(reason) => format!(
                    "a partial object may remain on the device (cleanup failed: {reason}). \
                     Inspect it with `booxmtp ls` and remove it manually if needed."
                ),
                PartialCleanup::NothingToDo => {
                    "the device reported no partial object; check `booxmtp ls` if in doubt.".to_string()
                }
            }),
            Error::Mtp { source, .. } => mtp_hint(source),
            Error::Worker { .. } => Some(
                "this is a bug; run with `-v` and include the last log lines in a report."
                    .to_string(),
            ),
            Error::Io { .. } => None,
        }
    }

    /// Process exit code for this error, see the [module docs](self).
    pub fn exit_code(&self) -> u8 {
        match self {
            Error::NoDevice => exit_code::NO_DEVICE,
            Error::MultipleDevices { .. } | Error::DeviceNotFound { .. } => {
                exit_code::DEVICE_SELECTION
            }
            Error::InvalidStorageSelector { .. }
            | Error::StorageAliasAmbiguous { .. }
            | Error::StorageAliasUnavailable { .. }
            | Error::StorageNotFound { .. }
            | Error::StorageReadOnly { .. }
            | Error::NoStorage => exit_code::STORAGE_SELECTION,
            Error::Unsupported { .. } | Error::UnsupportedByApp { .. } => exit_code::FAILURE,
            Error::InvalidRemotePath { .. }
            | Error::RemotePathNotFound { .. }
            | Error::RemoteNotADirectory { .. }
            | Error::RemoteIsADirectory { .. } => exit_code::PATH,
            Error::LocalNotFound { .. }
            | Error::LocalNotAFile { .. }
            | Error::LocalDirectoryMissing { .. }
            | Error::LocalAlreadyExists { .. }
            | Error::LocalIsDirectory { .. } => exit_code::LOCAL_IO,
            Error::RemoteAlreadyExists { .. } | Error::InsufficientSpace { .. } => {
                exit_code::CONFLICT
            }
            Error::Cancelled => exit_code::CANCELLED,
            Error::IncompleteTransfer { .. } | Error::Upload { .. } | Error::Mtp { .. } => {
                exit_code::TRANSFER
            }
            Error::Io { .. } => exit_code::LOCAL_IO,
            Error::Worker { .. } => exit_code::FAILURE,
        }
    }
}

impl From<mtp_rs::Error> for Error {
    fn from(source: mtp_rs::Error) -> Self {
        // Keep cancellation recognisable: it maps to its own exit code and message.
        match source {
            mtp_rs::Error::Cancelled => Error::Cancelled,
            other => Error::Mtp {
                context: "device operation failed".to_string(),
                source: other,
            },
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(source: std::io::Error) -> Self {
        Error::Io {
            context: "local file operation failed".to_string(),
            source,
        }
    }
}

/// Add MTP context to a `Result` without losing the error category.
pub trait MtpResultExt<T> {
    /// Attach a description of what the program was doing.
    fn mtp_context(self, context: impl Into<String>) -> Result<T>;
}

impl<T> MtpResultExt<T> for std::result::Result<T, mtp_rs::Error> {
    fn mtp_context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|source| match source {
            // Cancellation keeps its own category so the UI can report "cancelled", not "failed".
            mtp_rs::Error::Cancelled => Error::Cancelled,
            other => Error::mtp(context, other),
        })
    }
}

/// Add local-I/O context to a `Result`.
pub trait IoResultExt<T> {
    /// Attach a description of what the program was doing.
    fn io_context(self, context: impl Into<String>) -> Result<T>;
}

impl<T> IoResultExt<T> for std::io::Result<T> {
    fn io_context(self, context: impl Into<String>) -> Result<T> {
        self.map_err(|source| Error::io(context, source))
    }
}

/// Platform-aware help for "I can't see the device".
fn no_device_hint() -> String {
    let mut hint = String::from(
        "unlock the reader, choose “File transfer” (MTP) in the USB notification and check the \
         cable; `booxmtp devices` must list it first.",
    );
    if cfg!(target_os = "macos") {
        hint.push_str(
            " On macOS, Apple’s `ptpcamerad` or “Android File Transfer” may hold the device: quit \
             Android File Transfer and run `sudo killall ptpcamerad` (or \
             `sudo launchctl disable system/com.apple.ptpcamerad`).",
        );
    } else if cfg!(target_os = "linux") {
        hint.push_str(" On Linux you may need udev rules to access the USB device without root.");
    }
    hint
}

/// Translate device errors into user guidance.
fn mtp_hint(source: &mtp_rs::Error) -> Option<String> {
    use mtp_rs::Error as Mtp;
    match source {
        Mtp::ExclusiveAccess => Some(no_device_hint()),
        Mtp::PermissionDenied => Some(
            "the operating system denied access to the device (Linux: udev rules).".to_string(),
        ),
        Mtp::Disconnected => Some(
            "the reader was disconnected or stopped responding; reconnect it, unlock it and try \
             again."
                .to_string(),
        ),
        Mtp::StorageFull => Some("free space on the device and try again.".to_string()),
        Mtp::AccessDenied => Some(
            "the device refused the operation; the storage may be read-only or the file \
             write-protected."
                .to_string(),
        ),
        Mtp::Busy => Some("the device is busy; wait a moment and retry.".to_string()),
        Mtp::Timeout => Some(
            "the device did not answer in time; reconnect it if this keeps happening.".to_string(),
        ),
        Mtp::Unsupported => Some(
            "this device does not implement the required MTP operation; `booxmtp devices -v` may \
             show more detail."
                .to_string(),
        ),
        Mtp::StaleHandle => Some(
            "the device re-indexed its files mid-operation; run the command again.".to_string(),
        ),
        Mtp::DeviceReset => Some(
            "the USB session was reset to recover the device; run the command again.".to_string(),
        ),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_codes_are_grouped_by_cause() {
        assert_eq!(Error::NoDevice.exit_code(), exit_code::NO_DEVICE);
        assert_eq!(
            Error::LocalAlreadyExists {
                path: PathBuf::from("/tmp/x")
            }
            .exit_code(),
            exit_code::LOCAL_IO
        );
        assert_eq!(Error::Cancelled.exit_code(), exit_code::CANCELLED);
    }

    #[test]
    fn cancellation_stays_cancellation_through_context() {
        let error = std::result::Result::<(), _>::Err(mtp_rs::Error::Cancelled)
            .mtp_context("reading a file")
            .unwrap_err();
        assert!(matches!(error, Error::Cancelled));
        assert_eq!(error.exit_code(), exit_code::CANCELLED);
    }

    #[test]
    fn every_error_has_a_message_and_a_category() {
        let errors = [
            Error::NoDevice,
            Error::Cancelled,
            Error::RemoteAlreadyExists {
                path: "/Books/x.epub".into(),
            },
            Error::StorageAliasAmbiguous {
                alias: StorageAlias::Internal,
                candidates: vec!["65537 (Internal storage)".into()],
            },
        ];
        for error in errors {
            assert!(!error.to_string().is_empty());
            assert!(error.exit_code() > exit_code::SUCCESS);
        }
    }
}
