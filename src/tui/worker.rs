//! Background I/O worker.
//!
//! The UI must never block on USB. All device work therefore happens on a dedicated thread that
//! owns the [`Session`] and a small current-thread Tokio runtime. The UI thread communicates with
//! it through two channels:
//!
//! ```text
//!   UI thread ──Request──▶ worker thread (Session + runtime) ──Event──▶ UI thread
//!                        ▲
//!                        └── CancelSlot (shared, so the UI can abort a running transfer)
//! ```
//!
//! Requests are processed strictly one at a time, which matches MTP: a device has a single session
//! and concurrent operations would interleave on the wire. Long operations report progress through
//! [`Event::Progress`] and can be cancelled without waiting for the worker to become idle, because
//! cancellation goes through a shared [`CancelToken`] rather than a channel message.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use mtp_rs::CancelToken;
use tokio::sync::mpsc as async_mpsc;

use crate::error::{Error, Result};
use crate::session::Session;
use crate::storage::StorageEntry;
use crate::transfer::{TransferOptions, TransferProgress};
use crate::tree::{self, DeleteSummary};

use super::entry::{FileEntry, Location, PanelId, read_local_dir};

/// Whether a transfer copies or moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferMode {
    /// Leave the source in place.
    Copy,
    /// Remove the source after a successful copy.
    Move,
}

impl TransferMode {
    /// Verb for status messages (`Copying…`, `Moving…`).
    pub fn present_participle(self) -> &'static str {
        match self {
            TransferMode::Copy => "Copying",
            TransferMode::Move => "Moving",
        }
    }

    /// Verb for finished messages (`Copied…`, `Moved…`).
    pub fn past_tense(self) -> &'static str {
        match self {
            TransferMode::Copy => "Copied",
            TransferMode::Move => "Moved",
        }
    }
}

/// A transfer job (copy or move), as prepared by the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyRequest {
    /// Panel the file comes from (for reloading and messages).
    pub source_panel: PanelId,
    /// Fully resolved source path (a file).
    pub source: Location,
    /// File name, used to derive the destination path.
    pub source_name: String,
    /// Panel the file goes to (a folder location).
    pub destination_panel: PanelId,
    /// Destination folder.
    pub destination: Location,
    /// Whether an existing target may be replaced.
    pub overwrite: bool,
    /// Copy or move.
    pub mode: TransferMode,
}

impl CopyRequest {
    /// The full destination path (folder + source file name).
    pub fn destination_file(&self) -> Option<Location> {
        self.destination.child(&self.source_name)
    }
}

/// Work the UI asks for.
#[derive(Debug)]
pub enum Request {
    /// Read a folder and report its entries.
    List {
        /// Which panel asked.
        panel: PanelId,
        /// Folder to read.
        location: Location,
    },
    /// Copy or move one file or folder tree between the panel locations.
    Transfer(CopyRequest),
    /// Delete the named entries of a folder.
    Delete {
        /// Panel whose listing should be reloaded.
        panel: PanelId,
        /// Folder that contains the entries.
        location: Location,
        /// Names to delete (files or folders, recursively).
        names: Vec<String>,
    },
    /// Create a folder inside `location`.
    CreateDir {
        /// Panel the folder belongs to.
        panel: PanelId,
        /// Parent folder.
        location: Location,
        /// New folder name.
        name: String,
    },
    /// Re-read storage metadata (free space changes after transfers).
    RefreshStorages,
    /// Stop the worker loop.
    Shutdown,
}

/// Something the worker reports back.
#[derive(Debug)]
pub enum Event {
    /// The device is open and its storages are known.
    Connected {
        /// One-line device description for the header.
        device: String,
        /// Storages with resolved aliases.
        storages: Vec<StorageEntry>,
    },
    /// A folder was read.
    Listed {
        /// Which panel asked.
        panel: PanelId,
        /// Folder that was read.
        location: Location,
        /// Sorted entries, including the synthetic `..` row when applicable.
        entries: Vec<FileEntry>,
    },
    /// An operation failed.
    Failure {
        /// Panel to blame, if any.
        panel: Option<PanelId>,
        /// Human-readable message.
        message: String,
        /// Optional suggestion.
        hint: Option<String>,
    },
    /// A copy or move finished successfully.
    TransferFinished {
        /// Source panel (reload).
        source_panel: PanelId,
        /// Destination panel (reload).
        destination_panel: PanelId,
        /// Name of the transferred entry (used to clear its mark).
        source_name: String,
        /// Message for the status line.
        message: String,
    },
    /// A delete finished successfully.
    DeleteFinished {
        /// Panel whose listing should be reloaded.
        panel: PanelId,
        /// Message for the status line.
        message: String,
    },
    /// The destination exists and overwriting was not allowed.
    Conflict {
        /// The rejected job, so the UI can retry with `overwrite = true`.
        request: CopyRequest,
        /// Message to show in the confirmation dialog.
        message: String,
    },
    /// The user cancelled the running operation.
    Cancelled,
    /// A folder was created.
    DirCreated {
        /// Panel to reload.
        panel: PanelId,
        /// Message for the status line.
        message: String,
    },
    /// Transfer progress.
    Progress {
        /// File being transferred.
        label: String,
        /// Bytes transferred so far.
        transferred: u64,
        /// Total bytes, when known.
        total: Option<u64>,
    },
    /// Updated storage metadata.
    Storages {
        /// Fresh storage list.
        storages: Vec<StorageEntry>,
    },
    /// The device could not be opened (fatal: the TUI exits with this error).
    Fatal(Error),
}

/// Shared handle to the currently running operation's cancellation token.
#[derive(Debug, Clone)]
struct CancelSlot(Arc<Mutex<Option<CancelToken>>>);

impl CancelSlot {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(None)))
    }

    /// Install a fresh token for the next operation.
    fn arm(&self) -> CancelToken {
        let token = CancelToken::new();
        *self.0.lock().expect("cancel slot poisoned") = Some(token.clone());
        token
    }

    /// Remove the token once the operation is over.
    fn disarm(&self) {
        *self.0.lock().expect("cancel slot poisoned") = None;
    }

    /// Ask the running operation to stop (no-op when idle).
    fn cancel(&self) {
        if let Some(token) = self.0.lock().expect("cancel slot poisoned").as_ref() {
            token.cancel();
        }
    }
}

/// The UI-side handle to the background worker.
pub struct WorkerHandle {
    requests: async_mpsc::UnboundedSender<Request>,
    events: Receiver<Event>,
    cancel: CancelSlot,
    thread: Option<JoinHandle<()>>,
}

impl WorkerHandle {
    /// Start the worker thread and connect to the device.
    ///
    /// The connection is attempted immediately; the outcome arrives as [`Event::Connected`] or
    /// [`Event::Fatal`].
    pub fn spawn(serial: Option<String>) -> Result<Self> {
        let (request_tx, request_rx) = async_mpsc::unbounded_channel();
        let (event_tx, event_rx) = mpsc::channel();
        let cancel = CancelSlot::new();

        let slot = cancel.clone();
        let thread = thread::Builder::new()
            .name("booxmtp-io".to_string())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build();
                let runtime = match runtime {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = event_tx
                            .send(Event::Fatal(Error::io("starting the I/O runtime", error)));
                        return;
                    }
                };
                runtime.block_on(worker_main(serial, request_rx, event_tx, slot));
            })
            .map_err(|error| Error::io("starting the background I/O thread", error))?;

        Ok(Self {
            requests: request_tx,
            events: event_rx,
            cancel,
            thread: Some(thread),
        })
    }

    /// Ask for a folder listing.
    pub fn list(&self, panel: PanelId, location: Location) {
        self.send(Request::List { panel, location });
    }

    /// Ask for a copy or move.
    pub fn transfer(&self, request: CopyRequest) {
        self.send(Request::Transfer(request));
    }

    /// Ask for deleting the named entries of a folder.
    pub fn delete(&self, panel: PanelId, location: Location, names: Vec<String>) {
        self.send(Request::Delete {
            panel,
            location,
            names,
        });
    }

    /// Ask for a new folder.
    pub fn create_dir(&self, panel: PanelId, location: Location, name: String) {
        self.send(Request::CreateDir {
            panel,
            location,
            name,
        });
    }

    /// Ask for fresh storage metadata.
    pub fn refresh_storages(&self) {
        self.send(Request::RefreshStorages);
    }

    /// Cancel the currently running operation, if any.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// Take the next pending event without blocking.
    pub fn try_recv(&self) -> std::result::Result<Event, TryRecvError> {
        self.events.try_recv()
    }

    /// Stop the worker, cancelling a running operation and waiting briefly for a clean close.
    pub fn shutdown(&mut self) {
        self.cancel();
        let _ = self.requests.send(Request::Shutdown);
        if let Some(thread) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_millis(500);
            while !thread.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            } else {
                // The operation ignored the cancel (or the device stalled). The process is about to
                // exit, so detaching is better than hanging the terminal.
                tracing::warn!("I/O worker did not stop in time; detaching");
            }
        }
    }

    fn send(&self, request: Request) {
        if self.requests.send(request).is_err() {
            tracing::warn!("background I/O worker is gone");
        }
    }
}

/// Worker thread entry point.
async fn worker_main(
    serial: Option<String>,
    mut requests: async_mpsc::UnboundedReceiver<Request>,
    events: Sender<Event>,
    cancel: CancelSlot,
) {
    let mut session = match Session::connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Fatal(error));
            return;
        }
    };

    let _ = events.send(Event::Connected {
        device: describe_device(&session),
        storages: session.catalog().entries().to_vec(),
    });

    while let Some(request) = requests.recv().await {
        match request {
            Request::Shutdown => break,
            Request::List { panel, location } => {
                let outcome = load_entries(&session, &location).await;
                let _ = events.send(match outcome {
                    Ok(entries) => Event::Listed {
                        panel,
                        location,
                        entries,
                    },
                    Err(error) => failure(Some(panel), error),
                });
            }
            Request::Transfer(request) => {
                handle_transfer(&mut session, &events, &cancel, request).await;
            }
            Request::Delete {
                panel,
                location,
                names,
            } => handle_delete(&mut session, &events, &cancel, panel, &location, &names).await,
            Request::CreateDir {
                panel,
                location,
                name,
            } => handle_create_dir(&mut session, &events, panel, &location, &name).await,
            Request::RefreshStorages => match session.reload_storages().await {
                Ok(catalog) => {
                    let _ = events.send(Event::Storages {
                        storages: catalog.entries().to_vec(),
                    });
                }
                Err(error) => {
                    let _ = events.send(failure(None, error));
                }
            },
        }
    }

    if let Err(error) = session.close().await {
        tracing::debug!(%error, "closing the device failed");
    }
}

/// Read one folder into UI rows.
async fn load_entries(session: &Session, location: &Location) -> Result<Vec<FileEntry>> {
    match location {
        Location::Local(path) => read_local_dir(path)
            .map_err(|error| Error::io(format!("reading `{}`", path.display()), error)),
        Location::Remote { storage, path } => {
            let listing = session.list_dir(storage.id, path, None).await?;
            let mut entries = Vec::with_capacity(listing.entries.len() + 1);
            if !path.is_root() {
                entries.push(FileEntry::parent());
            }
            entries.extend(listing.entries.iter().map(FileEntry::from_remote));
            Ok(entries)
        }
    }
}

/// Execute a copy/move job and report the outcome.
async fn handle_transfer(
    session: &mut Session,
    events: &Sender<Event>,
    cancel: &CancelSlot,
    request: CopyRequest,
) {
    let Some(destination) = request.destination_file() else {
        let _ = events.send(Event::Failure {
            panel: Some(request.source_panel),
            message: format!("`{}` is not a valid file name", request.source_name),
            hint: None,
        });
        return;
    };

    // Remote-to-remote would need an on-device copy; refuse instead of pretending.
    if request.source.is_remote() && destination.is_remote() {
        let _ = events.send(Event::Failure {
            panel: Some(request.source_panel),
            message: "Copying between two remote folders is not supported yet".to_string(),
            hint: Some(
                "use a local folder as the destination, then upload again from there.".to_string(),
            ),
        });
        return;
    }

    let token = cancel.arm();
    let options = TransferOptions {
        overwrite: request.overwrite,
        cancel: Some(token.clone()),
    };
    let mut progress = progress_reporter(events);
    let outcome = match request.mode {
        TransferMode::Copy => {
            tree::copy(
                session,
                &request.source,
                &destination,
                &options,
                &mut progress,
            )
            .await
        }
        TransferMode::Move => {
            tree::move_entry(
                session,
                &request.source,
                &destination,
                &options,
                &mut progress,
            )
            .await
        }
    };
    cancel.disarm();

    match outcome {
        Ok(summary) => {
            let message = format!(
                "{} {} → {} ({})",
                request.mode.past_tense(),
                request.source_name,
                destination.display_path(),
                summary.describe()
            );
            tracing::debug!(%message, "transfer finished");
            let _ = events.send(Event::TransferFinished {
                source_panel: request.source_panel,
                destination_panel: request.destination_panel,
                source_name: request.source_name,
                message,
            });
            refresh_storages_quietly(session, events).await;
        }
        Err(Error::Cancelled) => {
            let _ = events.send(Event::Cancelled);
            refresh_storages_quietly(session, events).await;
        }
        Err(error @ (Error::LocalAlreadyExists { .. } | Error::RemoteAlreadyExists { .. })) => {
            let _ = events.send(Event::Conflict {
                request,
                message: error.to_string(),
            });
        }
        Err(error) => {
            let _ = events.send(failure(Some(request.source_panel), error));
        }
    }
}

/// Delete the named entries of a folder, one after another.
async fn handle_delete(
    session: &mut Session,
    events: &Sender<Event>,
    cancel: &CancelSlot,
    panel: PanelId,
    location: &Location,
    names: &[String],
) {
    let token = cancel.arm();
    let mut summary = DeleteSummary::default();
    let mut outcome: Result<()> = Ok(());

    for name in names {
        let Some(target) = location.child(name) else {
            outcome = Err(Error::invalid_remote_path(
                location.display_path(),
                format!("`{name}` is not a valid name"),
            ));
            break;
        };
        match tree::remove(session, &target, Some(&token)).await {
            Ok(removed) => summary += removed,
            Err(error) => {
                outcome = Err(error);
                break;
            }
        }
    }
    cancel.disarm();

    match outcome {
        Ok(()) => {
            let label = if names.len() == 1 {
                format!("`{}`", names[0])
            } else {
                format!("{} items", names.len())
            };
            let message = format!("Deleted {label} ({})", summary.describe());
            tracing::debug!(%message, "delete finished");
            let _ = events.send(Event::DeleteFinished { panel, message });
            refresh_storages_quietly(session, events).await;
        }
        Err(Error::Cancelled) => {
            let _ = events.send(Event::Cancelled);
            refresh_storages_quietly(session, events).await;
        }
        Err(error) => {
            let _ = events.send(failure(Some(panel), error));
        }
    }
}

/// Create a folder on the host or the device.
async fn handle_create_dir(
    session: &mut Session,
    events: &Sender<Event>,
    panel: PanelId,
    location: &Location,
    name: &str,
) {
    let outcome = match location {
        Location::Local(parent) => {
            let target = parent.join(name);
            match std::fs::create_dir(&target) {
                Ok(()) => Ok(format!("Created {}", target.display())),
                Err(error) => Err(Error::io(format!("creating `{}`", target.display()), error)),
            }
        }
        Location::Remote { storage, path } => match path.join(name) {
            Ok(target) => session
                .create_dir(storage.id, &target, None)
                .await
                .map(|_| format!("Created {}", target.display())),
            Err(error) => Err(error),
        },
    };

    let _ = events.send(match outcome {
        Ok(message) => {
            refresh_storages_quietly(session, events).await;
            Event::DirCreated { panel, message }
        }
        Err(error) => failure(Some(panel), error),
    });
}

/// Refresh storage metadata without turning a failure into a dialog.
async fn refresh_storages_quietly(session: &mut Session, events: &Sender<Event>) {
    match session.reload_storages().await {
        Ok(catalog) => {
            let _ = events.send(Event::Storages {
                storages: catalog.entries().to_vec(),
            });
        }
        Err(error) => tracing::debug!(%error, "refreshing storages failed"),
    }
}

/// Build a failure event from an error, keeping its hint.
fn failure(panel: Option<PanelId>, error: Error) -> Event {
    Event::Failure {
        panel,
        message: error.to_string(),
        hint: error.hint(),
    }
}

/// Progress callback that forwards per-file samples to the UI thread.
fn progress_reporter(events: &Sender<Event>) -> impl FnMut(&str, TransferProgress) + Send + '_ {
    let events = events.clone();
    move |label: &str, progress: TransferProgress| {
        let _ = events.send(Event::Progress {
            label: label.to_string(),
            transferred: progress.transferred,
            total: progress.total,
        });
    }
}

/// One-line device description for the header.
fn describe_device(session: &Session) -> String {
    let summary = session.summary();
    let mut parts = vec![summary.display_name()];
    if let Some(serial) = &summary.serial_number {
        parts.push(format!("serial {serial}"));
    }
    let speed = crate::output::format_usb_speed(summary.usb_speed);
    if speed != "-" {
        parts.push(speed);
    }
    parts.join(" · ")
}
