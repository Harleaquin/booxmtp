//! Application state and key handling — deliberately free of rendering and I/O.
//!
//! [`App`] is a pure state machine: it consumes [`Event`]s from the worker and key presses, and
//! returns [`Action`]s that the event loop forwards to the worker. That separation is what makes
//! the panel logic and every dialog testable without a terminal, a device or threads.

use std::path::PathBuf;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::remote_path::RemotePath;
use crate::storage::{StorageAlias, StorageEntry, StorageSelector};

use super::entry::{FileEntry, Location, Origin, PanelId};
use super::worker::{CopyRequest, Event};

/// Which screen the app is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Waiting for the worker to open the device.
    Connecting,
    /// The two-panel file manager.
    Panels,
}

/// One of the two file panels.
#[derive(Debug, Clone)]
pub struct Panel {
    /// Where the panel points.
    pub location: Location,
    /// Visible rows (including the synthetic `..` row).
    pub entries: Vec<FileEntry>,
    /// Selected row index.
    pub selected: usize,
    /// Whether a listing is in flight.
    pub loading: bool,
    /// Content height of the last render, used for page-wise scrolling.
    pub viewport_rows: usize,
    /// Last listing error, kept while the previous contents stay visible.
    pub error: Option<String>,
}

impl Panel {
    /// An empty panel pointing at `location`.
    pub fn new(location: Location) -> Self {
        Self {
            location,
            entries: Vec::new(),
            selected: 0,
            loading: false,
            viewport_rows: 10,
            error: None,
        }
    }

    /// The selected row, if any.
    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.entries.get(self.selected)
    }

    /// The selected name, used to keep the cursor on the same file across reloads.
    fn selected_name(&self) -> Option<String> {
        self.selected_entry().map(|entry| entry.name.clone())
    }

    /// Move up one row.
    pub fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move down one row.
    pub fn select_next(&mut self) {
        if self.selected + 1 < self.entries.len() {
            self.selected += 1;
        }
    }

    /// Move up by one screen.
    pub fn select_page_up(&mut self) {
        self.selected = self.selected.saturating_sub(self.page_size());
    }

    /// Move down by one screen.
    pub fn select_page_down(&mut self) {
        self.selected = (self.selected + self.page_size()).min(self.last_index());
    }

    /// Jump to the first row.
    pub fn select_first(&mut self) {
        self.selected = 0;
    }

    /// Jump to the last row.
    pub fn select_last(&mut self) {
        self.selected = self.last_index();
    }

    fn last_index(&self) -> usize {
        self.entries.len().saturating_sub(1)
    }

    fn page_size(&self) -> usize {
        self.viewport_rows.max(1)
    }
}

/// A modal dialog.
#[derive(Debug, Clone)]
pub enum Dialog {
    /// Information or error, dismissed with Enter/Esc.
    Message {
        /// Dialog title.
        title: String,
        /// Main text.
        message: String,
        /// Optional hint, shown below the message.
        hint: Option<String>,
        /// Style it as an error rather than information.
        is_error: bool,
    },
    /// “Overwrite?” confirmation.
    ConfirmOverwrite {
        /// The copy job, retried with `overwrite = true` when confirmed.
        request: CopyRequest,
        /// Path that already exists.
        message: String,
        /// Currently highlighted button.
        yes: bool,
    },
    /// Text input (used for “create folder”).
    Input {
        /// Dialog title.
        title: String,
        /// Where the folder will be created.
        prompt: String,
        /// Current input value.
        value: String,
        /// What to do on Enter.
        purpose: InputPurpose,
    },
    /// Transfer in progress; the numbers live in [`App::progress`].
    Progress,
    /// Key and behaviour reference.
    Help {
        /// Scroll offset in lines.
        scroll: u16,
    },
    /// Choose which storage a remote panel shows.
    StoragePicker {
        /// Highlighted entry in [`App::storages`].
        selected: usize,
    },
}

/// What an [`Dialog::Input`] produces.
#[derive(Debug, Clone)]
pub enum InputPurpose {
    /// Create a folder inside this folder.
    MakeDirectory {
        /// Panel to reload afterwards.
        panel: PanelId,
        /// Parent folder.
        parent: Location,
    },
}

/// Progress of the running transfer.
#[derive(Debug, Clone)]
pub struct ProgressState {
    /// File being transferred.
    pub label: String,
    /// Bytes transferred so far.
    pub transferred: u64,
    /// Total bytes, when known.
    pub total: Option<u64>,
    /// When the transfer started (for the rate estimate).
    pub started: Instant,
    /// Whether the user already asked to stop.
    pub cancel_requested: bool,
}

impl ProgressState {
    /// Completion between 0.0 and 1.0, when the total is known.
    pub fn ratio(&self) -> Option<f64> {
        match self.total {
            Some(0) => Some(1.0),
            Some(total) => Some((self.transferred as f64 / total as f64).clamp(0.0, 1.0)),
            None => None,
        }
    }

    /// Average transfer rate in bytes per second.
    pub fn bytes_per_second(&self) -> u64 {
        let elapsed = self.started.elapsed().as_secs_f64();
        if elapsed <= 0.0 {
            0
        } else {
            (self.transferred as f64 / elapsed) as u64
        }
    }
}

/// Work the event loop must hand to the worker.
#[derive(Debug)]
pub enum Action {
    /// Read a folder.
    List {
        /// Which panel.
        panel: PanelId,
        /// Folder to read.
        location: Location,
    },
    /// Copy the selected file.
    Copy(CopyRequest),
    /// Create a folder.
    CreateDir {
        /// Panel to reload.
        panel: PanelId,
        /// Parent folder.
        parent: Location,
        /// New folder name.
        name: String,
    },
    /// Re-read storage metadata.
    RefreshStorages,
    /// Cancel the running transfer.
    CancelTransfer,
    /// The worker reported a fatal problem; the event loop returns this error.
    Abort(crate::error::Error),
}

impl Action {
    /// Convenience constructor for “reload this panel”.
    fn list(panel: PanelId, location: &Location) -> Self {
        Action::List {
            panel,
            location: location.clone(),
        }
    }
}

/// The whole UI state.
#[derive(Debug)]
pub struct App {
    /// Connecting or file manager.
    pub screen: Screen,
    /// Human-readable target shown while connecting.
    pub connecting: String,
    /// One-line device description.
    pub device: String,
    /// Storages reported by the device (updated after transfers).
    pub storages: Vec<StorageEntry>,
    /// Left and right panel.
    pub panels: [Panel; 2],
    /// Panel that receives key presses.
    pub active: PanelId,
    /// Open dialog, if any.
    pub dialog: Option<Dialog>,
    /// Message shown on the status line.
    pub status: String,
    /// Running transfer, if any.
    pub progress: Option<ProgressState>,
    /// Whether an operation is in flight (input is ignored for navigation).
    pub busy: bool,
    /// Set to leave the event loop.
    pub should_quit: bool,
}

impl App {
    /// Create the app in the connecting state, with both panels on the host cwd.
    pub fn new(local_dir: PathBuf, connecting: impl Into<String>) -> Self {
        let left = Panel::new(Location::local(local_dir.clone()));
        let right = Panel::new(Location::local(local_dir));
        Self {
            screen: Screen::Connecting,
            connecting: connecting.into(),
            device: String::new(),
            storages: Vec::new(),
            panels: [left, right],
            active: PanelId::Left,
            dialog: None,
            status: String::new(),
            progress: None,
            busy: false,
            should_quit: false,
        }
    }

    /// The active panel.
    pub fn active_panel(&self) -> &Panel {
        &self.panels[self.active.index()]
    }

    /// A panel by id.
    pub fn panel(&self, id: PanelId) -> &Panel {
        &self.panels[id.index()]
    }

    fn panel_mut(&mut self, id: PanelId) -> &mut Panel {
        &mut self.panels[id.index()]
    }

    /// Whether a transfer is running (used to keep redrawing the progress dialog).
    pub fn is_animating(&self) -> bool {
        self.busy || self.progress.is_some()
    }

    /// The selected entry of the active panel.
    pub fn selected_entry(&self) -> Option<&FileEntry> {
        self.active_panel().selected_entry()
    }

    /// Consume a worker event and return the follow-up work.
    pub fn apply(&mut self, event: Event) -> Vec<Action> {
        match event {
            Event::Connected { device, storages } => self.on_connected(device, storages),
            Event::Listed {
                panel,
                location,
                entries,
            } => {
                self.on_listed(panel, location, entries);
                Vec::new()
            }
            Event::Failure {
                panel,
                message,
                hint,
            } => {
                self.busy = false;
                self.progress = None;
                if let Some(panel) = panel {
                    self.panel_mut(panel).loading = false;
                    self.panel_mut(panel).error = Some(message.clone());
                }
                self.dialog = Some(Dialog::Message {
                    title: "Error".to_string(),
                    message,
                    hint,
                    is_error: true,
                });
                Vec::new()
            }
            Event::CopyFinished {
                source_panel,
                destination_panel,
                message,
            } => {
                self.busy = false;
                self.progress = None;
                self.close_progress_dialog();
                self.status = message;
                vec![
                    Action::list(source_panel, &self.panel(source_panel).location),
                    Action::list(destination_panel, &self.panel(destination_panel).location),
                    Action::RefreshStorages,
                ]
            }
            Event::Conflict { request, message } => {
                self.busy = false;
                self.progress = None;
                self.close_progress_dialog();
                self.dialog = Some(Dialog::ConfirmOverwrite {
                    request,
                    message,
                    yes: false,
                });
                Vec::new()
            }
            Event::Cancelled => {
                self.busy = false;
                self.progress = None;
                self.close_progress_dialog();
                self.status = "Cancelled".to_string();
                Vec::new()
            }
            Event::DirCreated { panel, message } => {
                self.busy = false;
                self.close_progress_dialog();
                self.status = message;
                vec![Action::list(panel, &self.panel(panel).location)]
            }
            Event::Progress {
                label,
                transferred,
                total,
            } => {
                let state = self.progress.get_or_insert_with(|| ProgressState {
                    label: label.clone(),
                    transferred: 0,
                    total,
                    started: Instant::now(),
                    cancel_requested: false,
                });
                state.label = label;
                state.transferred = transferred;
                state.total = total;
                if self.dialog.is_none() {
                    self.dialog = Some(Dialog::Progress);
                }
                Vec::new()
            }
            Event::Storages { storages } => {
                self.update_storages(storages);
                Vec::new()
            }
            Event::Fatal(error) => vec![Action::Abort(error)],
        }
    }

    /// Handle one key press.
    ///
    /// There are deliberately no function-key bindings: terminals, terminal multiplexers and
    /// remote sessions intercept them inconsistently. Every command is a `Ctrl` combination
    /// (`Ctrl-C` copies, `Ctrl-N` creates, `Ctrl-Q` quits); only `?` for help is a plain key.
    pub fn on_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Ctrl-Q quits from anywhere, including open dialogs (the worker cancels on shutdown).
        if ctrl && matches!(key.code, KeyCode::Char('q') | KeyCode::Char('Q')) {
            self.should_quit = true;
            return Vec::new();
        }

        if let Some(dialog) = self.dialog.clone() {
            return self.on_dialog_key(dialog, key);
        }

        if self.screen == Screen::Connecting {
            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
                self.should_quit = true;
            }
            return Vec::new();
        }

        match key.code {
            KeyCode::Char('q') if !ctrl => {
                self.should_quit = true;
                Vec::new()
            }
            KeyCode::Char('c') if ctrl => self.copy_selected(),
            KeyCode::Char('n') if ctrl => {
                self.open_mkdir_dialog();
                Vec::new()
            }
            KeyCode::Char('u') if ctrl => {
                self.swap_panels();
                Vec::new()
            }
            KeyCode::Char('r') if ctrl => {
                self.status = "Reloading…".to_string();
                self.reload_all()
            }
            KeyCode::Char('s') if ctrl => {
                self.open_storage_picker();
                Vec::new()
            }
            KeyCode::Char('?') => {
                self.dialog = Some(Dialog::Help { scroll: 0 });
                Vec::new()
            }
            KeyCode::Up => {
                self.panel_mut(self.active).select_previous();
                Vec::new()
            }
            KeyCode::Down => {
                self.panel_mut(self.active).select_next();
                Vec::new()
            }
            KeyCode::PageUp => {
                self.panel_mut(self.active).select_page_up();
                Vec::new()
            }
            KeyCode::PageDown => {
                self.panel_mut(self.active).select_page_down();
                Vec::new()
            }
            KeyCode::Home => {
                self.panel_mut(self.active).select_first();
                Vec::new()
            }
            KeyCode::End => {
                self.panel_mut(self.active).select_last();
                Vec::new()
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.active = self.active.other();
                Vec::new()
            }
            KeyCode::Enter | KeyCode::Right => self.open_selected(),
            KeyCode::Backspace | KeyCode::Left => self.go_parent(),
            KeyCode::Esc => {
                self.should_quit = true;
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    // -----------------------------------------------------------------------
    // Event handlers
    // -----------------------------------------------------------------------

    fn on_connected(&mut self, device: String, storages: Vec<StorageEntry>) -> Vec<Action> {
        self.device = device;
        self.storages = storages;
        let Some(storage) = self.default_storage() else {
            self.dialog = Some(Dialog::Message {
                title: "No storage".to_string(),
                message: "The device reports no storage.".to_string(),
                hint: Some("Unlock the reader and reconnect it.".to_string()),
                is_error: true,
            });
            self.screen = Screen::Panels;
            return Vec::new();
        };

        self.panels[PanelId::Right.index()] =
            Panel::new(Location::remote(storage, RemotePath::root()));
        self.screen = Screen::Panels;
        self.status = "Connected".to_string();
        self.reload_all()
    }

    fn on_listed(&mut self, panel: PanelId, location: Location, entries: Vec<FileEntry>) {
        let target = self.panel_mut(panel);
        // A late response for a folder we already left must not replace the current listing.
        // Only the folder identity matters: the storage snapshot in the response may be older
        // than a free-space refresh that arrived in the meantime.
        if !target.location.same_target(&location) {
            tracing::debug!(%panel, "ignoring a stale folder listing");
            return;
        }
        let previous = target.selected_name();
        target.entries = entries;
        target.loading = false;
        target.error = None;
        target.selected = previous
            .and_then(|name| target.entries.iter().position(|entry| entry.name == name))
            .unwrap_or(0);
    }

    fn update_storages(&mut self, storages: Vec<StorageEntry>) {
        self.storages = storages;
        for panel in &mut self.panels {
            let Some(current) = panel.location.storage().map(|storage| storage.id) else {
                continue;
            };
            let Some(updated) = self.storages.iter().find(|storage| storage.id == current) else {
                continue;
            };
            if let Location::Remote { storage, .. } = &mut panel.location {
                *storage = updated.clone();
            }
        }
    }

    // -----------------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------------

    fn open_selected(&mut self) -> Vec<Action> {
        let Some(entry) = self.selected_entry().cloned() else {
            return Vec::new();
        };
        let panel_id = self.active;

        if entry.is_parent() {
            return self.go_parent();
        }
        if !entry.is_dir() {
            self.status = format!("{} — Ctrl-C copies it to the other panel", entry.name);
            return Vec::new();
        }
        let Some(child) = self.panel(panel_id).location.child(&entry.name) else {
            self.status = format!("Cannot open `{}`", entry.name);
            return Vec::new();
        };
        self.navigate_to(panel_id, child)
    }

    fn go_parent(&mut self) -> Vec<Action> {
        let panel_id = self.active;
        let Some(parent) = self.panel(panel_id).location.parent() else {
            self.status = "Already at the top".to_string();
            return Vec::new();
        };
        self.navigate_to(panel_id, parent)
    }

    fn navigate_to(&mut self, panel: PanelId, location: Location) -> Vec<Action> {
        {
            let target = self.panel_mut(panel);
            target.location = location.clone();
            target.entries.clear();
            target.selected = 0;
            target.loading = true;
            target.error = None;
        }
        vec![Action::list(panel, &location)]
    }

    /// Reload both panels and the storage metadata.
    pub fn reload_all(&mut self) -> Vec<Action> {
        let mut actions = Vec::with_capacity(3);
        for panel in PanelId::ALL {
            let location = self.panel(panel).location.clone();
            self.panel_mut(panel).loading = true;
            actions.push(Action::list(panel, &location));
        }
        actions.push(Action::RefreshStorages);
        actions
    }

    fn swap_panels(&mut self) {
        let left = self.panels[0].clone();
        self.panels[0] = self.panels[1].clone();
        self.panels[1] = left;
        self.status = "Swapped panels".to_string();
    }

    // -----------------------------------------------------------------------
    // Copy
    // -----------------------------------------------------------------------

    fn copy_selected(&mut self) -> Vec<Action> {
        if self.busy {
            self.status = "Wait for the running operation to finish".to_string();
            return Vec::new();
        }
        let Some(entry) = self.selected_entry().cloned() else {
            self.status = "Nothing to copy".to_string();
            return Vec::new();
        };
        if entry.is_parent() || entry.is_dir() {
            self.status = "Only files can be copied (recursion is not supported yet)".to_string();
            return Vec::new();
        }
        let source_panel = self.active;
        let destination_panel = source_panel.other();
        let source_location = self.panel(source_panel).location.clone();

        let source = match &entry.origin {
            Origin::Local(path) => Location::Local(path.clone()),
            Origin::Remote => match source_location.child(&entry.name) {
                Some(location) => location,
                None => {
                    self.status = format!("Cannot resolve `{}`", entry.name);
                    return Vec::new();
                }
            },
            Origin::Parent => return Vec::new(),
        };

        let destination = self.panel(destination_panel).location.clone();
        if !destination.is_writable() {
            self.show_error(
                "Destination is read-only",
                &format!("`{}` does not accept writes.", destination.display_path()),
                None,
            );
            return Vec::new();
        }
        if source.parent().as_ref() == Some(&destination) {
            self.status = "Source and destination are the same folder".to_string();
            return Vec::new();
        }

        let request = CopyRequest {
            source_panel,
            source,
            source_name: entry.name.clone(),
            destination_panel,
            destination,
            overwrite: false,
        };

        let conflict = self
            .panel(destination_panel)
            .entries
            .iter()
            .find(|candidate| {
                !candidate.is_parent() && candidate.name.eq_ignore_ascii_case(&entry.name)
            })
            .is_some();

        if conflict {
            self.dialog = Some(Dialog::ConfirmOverwrite {
                message: format!(
                    "`{}` already exists in the target folder.\n\nOverwrite it?",
                    entry.name
                ),
                request,
                yes: false,
            });
            Vec::new()
        } else {
            self.busy = true;
            self.status = format!("Copying {}…", entry.name);
            vec![Action::Copy(request)]
        }
    }

    fn open_mkdir_dialog(&mut self) {
        if self.busy {
            self.status = "Wait for the running operation to finish".to_string();
            return;
        }
        let location = self.panel(self.active).location.clone();
        if !location.is_writable() {
            self.show_error(
                "Read-only storage",
                "This storage does not accept writes.",
                None,
            );
            return;
        }
        self.dialog = Some(Dialog::Input {
            title: "Create folder".to_string(),
            prompt: location.display_path(),
            value: String::new(),
            purpose: InputPurpose::MakeDirectory {
                panel: self.active,
                parent: location,
            },
        });
    }

    fn open_storage_picker(&mut self) {
        let panel = self.panel(self.active);
        let Some(current) = panel.location.storage() else {
            self.show_info(
                "Storage",
                "Only a panel that shows the reader can switch storages.",
                None,
            );
            return;
        };
        let selected = self
            .storages
            .iter()
            .position(|storage| storage.id == current.id)
            .unwrap_or(0);
        self.dialog = Some(Dialog::StoragePicker { selected });
    }

    // -----------------------------------------------------------------------
    // Dialogs
    // -----------------------------------------------------------------------

    fn on_dialog_key(&mut self, dialog: Dialog, key: KeyEvent) -> Vec<Action> {
        match dialog {
            Dialog::Message { .. } => {
                if matches!(
                    key.code,
                    KeyCode::Enter | KeyCode::Esc | KeyCode::Char(' ') | KeyCode::Char('q')
                ) {
                    self.dialog = None;
                }
                Vec::new()
            }
            Dialog::Help { scroll } => match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') | KeyCode::Char('?') => {
                    self.dialog = None;
                    Vec::new()
                }
                KeyCode::Up => {
                    self.dialog = Some(Dialog::Help {
                        scroll: scroll.saturating_sub(1),
                    });
                    Vec::new()
                }
                KeyCode::Down => {
                    self.dialog = Some(Dialog::Help {
                        scroll: scroll.saturating_add(1),
                    });
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Dialog::ConfirmOverwrite {
                mut request,
                message,
                mut yes,
            } => match key.code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                    yes = !yes;
                    self.dialog = Some(Dialog::ConfirmOverwrite {
                        request,
                        message,
                        yes,
                    });
                    Vec::new()
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    self.dialog = None;
                    request.overwrite = true;
                    self.busy = true;
                    self.status = format!("Copying {}…", request.source_name);
                    vec![Action::Copy(request)]
                }
                KeyCode::Enter if yes => {
                    self.dialog = None;
                    request.overwrite = true;
                    self.busy = true;
                    self.status = format!("Copying {}…", request.source_name);
                    vec![Action::Copy(request)]
                }
                KeyCode::Enter | KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.dialog = None;
                    self.status = "Copy cancelled".to_string();
                    Vec::new()
                }
                _ => Vec::new(),
            },
            Dialog::Input {
                title,
                prompt,
                mut value,
                purpose,
            } => match key.code {
                KeyCode::Esc => {
                    self.dialog = None;
                    Vec::new()
                }
                KeyCode::Backspace => {
                    value.pop();
                    self.dialog = Some(Dialog::Input {
                        title,
                        prompt,
                        value,
                        purpose,
                    });
                    Vec::new()
                }
                KeyCode::Char(character)
                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                        && !character.is_control() =>
                {
                    if value.chars().count() < 128 {
                        value.push(character);
                    }
                    self.dialog = Some(Dialog::Input {
                        title,
                        prompt,
                        value,
                        purpose,
                    });
                    Vec::new()
                }
                KeyCode::Enter => {
                    let name = value.trim().to_string();
                    if !is_valid_folder_name(&name) {
                        self.show_error(
                            "Invalid name",
                            "A folder name must not be empty or contain `/`.",
                            Some("Use a single name such as `ToRead`.".to_string()),
                        );
                        return Vec::new();
                    }
                    self.dialog = None;
                    self.busy = true;
                    match purpose {
                        InputPurpose::MakeDirectory { panel, parent } => {
                            self.status = format!("Creating {name}…");
                            vec![Action::CreateDir {
                                panel,
                                parent,
                                name,
                            }]
                        }
                    }
                }
                _ => Vec::new(),
            },
            Dialog::Progress => {
                if key.code == KeyCode::Esc
                    && let Some(progress) = &mut self.progress
                    && !progress.cancel_requested
                {
                    progress.cancel_requested = true;
                    self.status = "Cancelling…".to_string();
                    return vec![Action::CancelTransfer];
                }
                Vec::new()
            }
            Dialog::StoragePicker { mut selected } => match key.code {
                KeyCode::Esc => {
                    self.dialog = None;
                    Vec::new()
                }
                KeyCode::Up => {
                    selected = selected.saturating_sub(1);
                    self.dialog = Some(Dialog::StoragePicker { selected });
                    Vec::new()
                }
                KeyCode::Down => {
                    if selected + 1 < self.storages.len() {
                        selected += 1;
                    }
                    self.dialog = Some(Dialog::StoragePicker { selected });
                    Vec::new()
                }
                KeyCode::Enter => {
                    let Some(storage) = self.storages.get(selected).cloned() else {
                        self.dialog = None;
                        return Vec::new();
                    };
                    self.dialog = None;
                    let panel = self.active;
                    let location = Location::remote(storage, RemotePath::root());
                    self.navigate_to(panel, location)
                }
                _ => Vec::new(),
            },
        }
    }

    fn show_error(&mut self, title: &str, message: &str, hint: Option<String>) {
        self.dialog = Some(Dialog::Message {
            title: title.to_string(),
            message: message.to_string(),
            hint,
            is_error: true,
        });
    }

    fn show_info(&mut self, title: &str, message: &str, hint: Option<String>) {
        self.dialog = Some(Dialog::Message {
            title: title.to_string(),
            message: message.to_string(),
            hint,
            is_error: false,
        });
    }

    fn close_progress_dialog(&mut self) {
        if matches!(self.dialog, Some(Dialog::Progress)) {
            self.dialog = None;
        }
    }

    /// Storage used for the right panel on startup: prefer the internal storage.
    fn default_storage(&self) -> Option<StorageEntry> {
        self.storages
            .iter()
            .find(|storage| storage.alias() == Some(StorageAlias::Internal))
            .or_else(|| {
                self.storages
                    .iter()
                    .find(|storage| storage.alias() == Some(StorageAlias::Sd))
            })
            .or_else(|| self.storages.first())
            .cloned()
    }

    /// Select the storage shown in the right panel (used by `booxmtp tui -s sd`).
    ///
    /// Leaves the keyboard focus where it is: the file manager always starts on the local panel.
    pub fn focus_storage(&mut self, selector: &str) -> bool {
        let Ok(selector) = StorageSelector::parse(selector) else {
            return false;
        };
        let catalog = crate::storage::StorageCatalog::new(self.storages.clone());
        let Ok(entry) = catalog.find(&selector) else {
            return false;
        };
        let storage = entry.clone();
        self.panels[PanelId::Right.index()] =
            Panel::new(Location::remote(storage, RemotePath::root()));
        true
    }
}

/// Whether `name` can be used as a folder name.
fn is_valid_folder_name(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains('/') && !name.contains('\0')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remote_path::EntryKind;
    use crate::tui::entry::{read_local_dir, sort_entries};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
    }

    fn storage(id: u64, description: &str, removable: bool) -> StorageEntry {
        let entry = StorageEntry::new(mtp_rs::mtp::StorageId(id), description)
            .with_capacity(1024 * 1024, 512 * 1024)
            .with_removable(removable)
            .with_writable(true);
        crate::storage::StorageCatalog::new([entry]).entries()[0].clone()
    }

    fn remote_entry(name: &str, kind: EntryKind) -> FileEntry {
        FileEntry {
            name: name.to_string(),
            kind,
            size: (kind == EntryKind::File).then_some(10),
            modified: None,
            origin: Origin::Remote,
        }
    }

    /// A ready app: left panel = temp dir, right panel = the `internal` storage.
    struct Fixture {
        app: App,
        local: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        fixture_with_storages(vec![storage(65_537, "Internal storage", false)])
    }

    fn fixture_with_storages(storages: Vec<StorageEntry>) -> Fixture {
        let local = tempfile::tempdir().unwrap();
        std::fs::create_dir(local.path().join("Sub")).unwrap();
        std::fs::write(local.path().join("book.epub"), b"book").unwrap();
        std::fs::write(local.path().join("notes.txt"), b"notes").unwrap();

        let mut app = App::new(local.path().to_path_buf(), "test device");
        app.apply(Event::Connected {
            device: "BOOX Test".to_string(),
            storages,
        });

        let left = app.panel(PanelId::Left).location.clone();
        let entries = read_local_dir(local.path()).unwrap();
        app.apply(Event::Listed {
            panel: PanelId::Left,
            location: left,
            entries,
        });

        let right = app.panel(PanelId::Right).location.clone();
        app.apply(Event::Listed {
            panel: PanelId::Right,
            location: right,
            entries: vec![remote_entry("Books", EntryKind::Directory)],
        });

        Fixture { app, local }
    }

    fn select_by_name(app: &mut App, panel: PanelId, name: &str) {
        let index = app
            .panel(panel)
            .entries
            .iter()
            .position(|entry| entry.name == name)
            .unwrap_or_else(|| panic!("`{name}` not in {panel} panel"));
        app.panels[panel.index()].selected = index;
    }

    #[test]
    fn device_connection_fills_the_right_panel_and_starts_listings() {
        let local = tempfile::tempdir().unwrap();
        let mut app = App::new(local.path().to_path_buf(), "test");
        assert_eq!(app.screen, Screen::Connecting);

        let actions = app.apply(Event::Connected {
            device: "BOOX Test".to_string(),
            storages: vec![storage(65_537, "Internal storage", false)],
        });

        assert_eq!(app.screen, Screen::Panels);
        assert_eq!(app.device, "BOOX Test");
        assert_eq!(app.panel(PanelId::Right).location.display_path(), "/");
        assert_eq!(app.panel(PanelId::Right).location.title(), "internal:/");
        assert_eq!(actions.len(), 3, "both panels plus a storage refresh");
        assert!(matches!(
            actions[0],
            Action::List {
                panel: PanelId::Left,
                ..
            }
        ));
        assert!(matches!(
            actions[1],
            Action::List {
                panel: PanelId::Right,
                ..
            }
        ));
        assert!(matches!(actions[2], Action::RefreshStorages));
    }

    #[test]
    fn the_internal_storage_is_preferred_over_the_card() {
        let local = tempfile::tempdir().unwrap();
        let mut app = App::new(local.path().to_path_buf(), "test");
        app.apply(Event::Connected {
            device: "BOOX".to_string(),
            storages: vec![
                storage(131_073, "android", true),
                storage(65_537, "Internal storage", false),
            ],
        });
        // The catalog sorts by ID, so `internal` is the first entry here regardless.
        assert_eq!(app.panel(PanelId::Right).location.title(), "internal:/");
    }

    #[test]
    fn cursor_movement_is_clamped() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        // Entries: "..", "Sub", "book.epub", "notes.txt"
        assert_eq!(app.selected_entry().unwrap().name, "..");

        app.on_key(key(KeyCode::Up));
        assert_eq!(
            app.active_panel().selected,
            0,
            "cannot go above the first row"
        );

        app.on_key(key(KeyCode::End));
        assert_eq!(app.selected_entry().unwrap().name, "notes.txt");

        app.on_key(key(KeyCode::Down));
        assert_eq!(
            app.active_panel().selected,
            3,
            "cannot go past the last row"
        );

        app.on_key(key(KeyCode::Home));
        assert_eq!(app.active_panel().selected, 0);
    }

    #[test]
    fn page_keys_use_the_rendered_height() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        app.panels[0].viewport_rows = 2;
        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.active_panel().selected, 2);
        app.on_key(key(KeyCode::PageDown));
        assert_eq!(app.active_panel().selected, 3);
        app.on_key(key(KeyCode::PageUp));
        assert_eq!(app.active_panel().selected, 1);
    }

    #[test]
    fn enter_opens_a_folder_and_backspace_returns() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        select_by_name(app, PanelId::Left, "Sub");

        let actions = app.on_key(key(KeyCode::Enter));
        assert_eq!(
            app.panel(PanelId::Left).location.display_path(),
            fixture.local.path().join("Sub").display().to_string()
        );
        assert!(matches!(
            actions.as_slice(),
            [Action::List {
                panel: PanelId::Left,
                ..
            }]
        ));
        assert!(app.panel(PanelId::Left).loading);

        let actions = app.on_key(key(KeyCode::Backspace));
        assert_eq!(
            app.panel(PanelId::Left).location.display_path(),
            fixture.local.path().display().to_string()
        );
        assert!(matches!(actions.as_slice(), [Action::List { .. }]));
    }

    #[test]
    fn parent_navigation_stops_at_the_top() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        app.panels[0].location = Location::local("/");
        app.on_key(key(KeyCode::Backspace));
        assert_eq!(app.status, "Already at the top");
        assert_eq!(app.panel(PanelId::Left).location.display_path(), "/");
    }

    #[test]
    fn entering_a_file_only_explains_how_to_copy_it() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        select_by_name(app, PanelId::Left, "book.epub");
        let actions = app.on_key(key(KeyCode::Enter));
        assert!(actions.is_empty());
        assert!(app.status.contains("Ctrl-C"));
        assert!(app.dialog.is_none());
    }

    #[test]
    fn f5_copies_a_file_to_the_other_panel() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        select_by_name(app, PanelId::Left, "book.epub");

        let actions = app.on_key(ctrl('c'));
        let [Action::Copy(request)] = actions.as_slice() else {
            panic!("expected a copy action, got {actions:?}");
        };
        assert_eq!(request.source_name, "book.epub");
        assert_eq!(request.destination_panel, PanelId::Right);
        assert!(!request.overwrite);
        assert!(matches!(request.source, Location::Local(_)));
        assert!(app.busy);
    }

    #[test]
    fn copying_a_remote_file_targets_the_local_panel() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Right;
        app.panels[1].entries = vec![remote_entry("Other.epub", EntryKind::File)];
        app.panels[1].selected = 0;

        let actions = app.on_key(ctrl('c'));
        let [Action::Copy(request)] = actions.as_slice() else {
            panic!("expected a copy action, got {actions:?}");
        };
        assert_eq!(request.source.display_path(), "/Other.epub");
        assert!(request.source.is_remote());
        assert!(!request.destination.is_remote());
    }

    #[test]
    fn f5_asks_before_overwriting_and_retries_with_overwrite() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        select_by_name(app, PanelId::Left, "book.epub");
        // Pretend the target folder already contains that name.
        app.panels[1].entries = vec![remote_entry("book.epub", EntryKind::File)];

        let actions = app.on_key(ctrl('c'));
        assert!(actions.is_empty(), "nothing may happen before confirmation");
        assert!(matches!(
            app.dialog,
            Some(Dialog::ConfirmOverwrite { yes: false, .. })
        ));
        assert!(!app.busy);

        // "n" cancels.
        app.on_key(key(KeyCode::Char('n')));
        assert!(app.dialog.is_none());
        assert!(!app.busy);

        // Now confirm with "y".
        app.on_key(ctrl('c'));
        let actions = app.on_key(key(KeyCode::Char('y')));
        let [Action::Copy(request)] = actions.as_slice() else {
            panic!("expected a copy action after confirmation");
        };
        assert!(request.overwrite);
        assert!(app.busy);
    }

    #[test]
    fn f5_refuses_folders_and_parent_rows() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        app.panels[0].selected = 0; // ".."
        assert!(app.on_key(ctrl('c')).is_empty());
        assert!(app.status.contains("Only files"));

        select_by_name(app, PanelId::Left, "Sub");
        app.status.clear();
        assert!(app.on_key(ctrl('c')).is_empty());
        assert!(app.status.contains("Only files"));
    }

    #[test]
    fn f7_creates_a_folder_with_a_valid_name() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        let actions = app.on_key(ctrl('n'));
        assert!(actions.is_empty());
        assert!(matches!(app.dialog, Some(Dialog::Input { .. })));

        for character in "Neu".chars() {
            app.on_key(key(KeyCode::Char(character)));
        }
        let actions = app.on_key(key(KeyCode::Enter));
        let [Action::CreateDir { name, .. }] = actions.as_slice() else {
            panic!("expected a mkdir action");
        };
        assert_eq!(name, "Neu");
        assert!(app.busy);
        assert!(app.dialog.is_none());
    }

    #[test]
    fn f7_rejects_empty_or_path_like_names() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.on_key(ctrl('n'));
        let actions = app.on_key(key(KeyCode::Enter));
        assert!(actions.is_empty());
        assert!(matches!(
            app.dialog,
            Some(Dialog::Message { is_error: true, .. })
        ));

        // Start again and type a slash.
        app.on_key(key(KeyCode::Enter)); // close the message
        app.on_key(ctrl('n'));
        app.on_key(key(KeyCode::Char('a')));
        app.on_key(key(KeyCode::Char('/')));
        app.on_key(key(KeyCode::Char('b')));
        let actions = app.on_key(key(KeyCode::Enter));
        assert!(actions.is_empty());
        assert!(matches!(
            app.dialog,
            Some(Dialog::Message { is_error: true, .. })
        ));
    }

    #[test]
    fn progress_dialog_offers_cancellation_once() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.busy = true;
        app.apply(Event::Progress {
            label: "book.epub".to_string(),
            transferred: 50,
            total: Some(100),
        });
        assert!(matches!(app.dialog, Some(Dialog::Progress)));
        assert_eq!(app.progress.as_ref().unwrap().ratio(), Some(0.5));

        let actions = app.on_key(key(KeyCode::Esc));
        assert!(matches!(actions.as_slice(), [Action::CancelTransfer]));
        assert!(app.progress.as_ref().unwrap().cancel_requested);

        // A second Esc does not spam the worker.
        assert!(app.on_key(key(KeyCode::Esc)).is_empty());
    }

    #[test]
    fn finishing_a_copy_clears_busy_state_and_reloads() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.busy = true;
        app.dialog = Some(Dialog::Progress);
        app.progress = Some(ProgressState {
            label: "x".to_string(),
            transferred: 1,
            total: Some(1),
            started: Instant::now(),
            cancel_requested: false,
        });

        let actions = app.apply(Event::CopyFinished {
            source_panel: PanelId::Left,
            destination_panel: PanelId::Right,
            message: "Uploaded book.epub".to_string(),
        });

        assert!(!app.busy);
        assert!(app.progress.is_none());
        assert!(app.dialog.is_none());
        assert_eq!(app.status, "Uploaded book.epub");
        assert_eq!(actions.len(), 3);
    }

    #[test]
    fn cancellation_is_reported_without_a_dialog() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.busy = true;
        app.dialog = Some(Dialog::Progress);
        app.apply(Event::Cancelled);
        assert!(!app.busy);
        assert!(app.dialog.is_none());
        assert_eq!(app.status, "Cancelled");
    }

    #[test]
    fn failures_are_shown_as_errors_and_stop_loading() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.panels[0].loading = true;
        app.apply(Event::Failure {
            panel: Some(PanelId::Left),
            message: "permission denied".to_string(),
            hint: Some("check permissions".to_string()),
        });
        assert!(!app.panel(PanelId::Left).loading);
        assert_eq!(
            app.panel(PanelId::Left).error.as_deref(),
            Some("permission denied")
        );
        match &app.dialog {
            Some(Dialog::Message { is_error, hint, .. }) => {
                assert!(is_error);
                assert_eq!(hint.as_deref(), Some("check permissions"));
            }
            other => panic!("expected an error dialog, got {other:?}"),
        }
        // Enter closes it.
        app.on_key(key(KeyCode::Enter));
        assert!(app.dialog.is_none());
    }

    #[test]
    fn a_listing_still_applies_after_a_storage_refresh() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        let location = app.panel(PanelId::Right).location.clone();

        // Free space changed while the listing was in flight.
        let mut refreshed = storage(65_537, "Internal storage", false);
        refreshed.free_bytes = Some(1);
        app.apply(Event::Storages {
            storages: vec![refreshed],
        });

        app.apply(Event::Listed {
            panel: PanelId::Right,
            location,
            entries: vec![remote_entry("fresh.bin", EntryKind::File)],
        });
        assert_eq!(app.panel(PanelId::Right).entries.len(), 1);
        assert_eq!(app.panel(PanelId::Right).entries[0].name, "fresh.bin");
    }

    #[test]
    fn stale_listings_are_ignored() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        select_by_name(app, PanelId::Left, "Sub");
        let old_location = app.panel(PanelId::Left).location.clone();
        app.on_key(key(KeyCode::Enter));
        assert!(app.panel(PanelId::Left).entries.is_empty());

        app.apply(Event::Listed {
            panel: PanelId::Left,
            location: old_location,
            entries: vec![remote_entry("ghost", EntryKind::File)],
        });
        assert!(
            app.panel(PanelId::Left).entries.is_empty(),
            "a late response for the previous folder must not overwrite the current one"
        );
    }

    #[test]
    fn reloads_keep_the_cursor_on_the_same_file() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        select_by_name(app, PanelId::Left, "notes.txt");

        let location = app.panel(PanelId::Left).location.clone();
        let mut entries = read_local_dir(fixture.local.path()).unwrap();
        sort_entries(&mut entries);
        app.apply(Event::Listed {
            panel: PanelId::Left,
            location,
            entries,
        });
        assert_eq!(app.selected_entry().unwrap().name, "notes.txt");
    }

    #[test]
    fn tab_and_swap_change_the_layout() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        app.on_key(key(KeyCode::Tab));
        assert_eq!(app.active, PanelId::Right);

        let right_title = app.panel(PanelId::Right).location.title();
        app.on_key(ctrl('u'));
        assert_eq!(app.panel(PanelId::Left).location.title(), right_title);
        assert_eq!(app.status, "Swapped panels");
    }

    #[test]
    fn ctrl_r_reloads_and_ctrl_s_opens_the_storage_picker() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        let actions = app.on_key(ctrl('r'));
        assert_eq!(actions.len(), 3);
        assert!(matches!(actions[2], Action::RefreshStorages));

        app.apply(Event::Storages {
            storages: vec![storage(65_537, "Internal storage", false)],
        });
        app.active = PanelId::Right;
        app.on_key(ctrl('s'));
        assert!(matches!(app.dialog, Some(Dialog::StoragePicker { .. })));
        app.on_key(key(KeyCode::Esc));
        assert!(app.dialog.is_none());
    }

    #[test]
    fn the_storage_picker_switches_the_remote_panel() {
        let mut fixture = fixture_with_storages(vec![
            storage(65_537, "Internal storage", false),
            storage(131_073, "android", true),
        ]);
        let app = &mut fixture.app;
        assert_eq!(app.panel(PanelId::Right).location.title(), "internal:/");

        app.active = PanelId::Right;
        app.on_key(ctrl('s'));
        app.on_key(key(KeyCode::Down));
        let actions = app.on_key(key(KeyCode::Enter));

        assert_eq!(app.panel(PanelId::Right).location.title(), "sd:/");
        assert!(matches!(actions.as_slice(), [Action::List { .. }]));
    }

    #[test]
    fn storage_updates_refresh_free_space_in_the_panel() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        let new_storage = storage(65_537, "Internal storage", false);
        app.apply(Event::Storages {
            storages: vec![new_storage.clone()],
        });
        assert_eq!(
            app.panel(PanelId::Right)
                .location
                .storage()
                .unwrap()
                .free_bytes,
            new_storage.free_bytes
        );
    }

    #[test]
    fn quit_keys_are_consistent() {
        for quit in [ctrl('q'), key(KeyCode::Esc), key(KeyCode::Char('q'))] {
            let mut fixture = fixture();
            fixture.app.on_key(quit);
            assert!(fixture.app.should_quit, "{quit:?} should quit");
        }
    }

    #[test]
    fn ctrl_c_copies_instead_of_quitting() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.active = PanelId::Left;
        select_by_name(app, PanelId::Left, "book.epub");

        let actions = app.on_key(ctrl('c'));
        assert!(!app.should_quit, "Ctrl-C must not quit anymore");
        assert!(matches!(actions.as_slice(), [Action::Copy(_)]));
    }

    #[test]
    fn function_keys_are_not_bound() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        for code in [KeyCode::F(1), KeyCode::F(5), KeyCode::F(7), KeyCode::F(10)] {
            assert!(
                app.on_key(key(code)).is_empty(),
                "{code:?} must not trigger anything"
            );
            assert!(!app.should_quit);
        }
        assert!(app.dialog.is_none());
    }

    #[test]
    fn ctrl_q_quits_even_with_a_dialog_open() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.on_key(ctrl('n'));
        assert!(matches!(app.dialog, Some(Dialog::Input { .. })));
        app.on_key(ctrl('q'));
        assert!(app.should_quit);
    }

    #[test]
    fn question_mark_opens_help() {
        let mut fixture = fixture();
        let app = &mut fixture.app;
        app.on_key(key(KeyCode::Char('?')));
        assert!(matches!(app.dialog, Some(Dialog::Help { .. })));
        app.on_key(key(KeyCode::Esc));
        assert!(app.dialog.is_none());
        assert!(!app.should_quit, "Esc closes the help instead of quitting");
    }

    #[test]
    fn folder_name_validation() {
        assert!(is_valid_folder_name("ToRead"));
        assert!(is_valid_folder_name("Bücher 2026"));
        assert!(!is_valid_folder_name(""));
        assert!(!is_valid_folder_name("."));
        assert!(!is_valid_folder_name(".."));
        assert!(!is_valid_folder_name("a/b"));
        assert!(!is_valid_folder_name("nul\0"));
    }
}
