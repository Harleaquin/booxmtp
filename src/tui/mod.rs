//! Two-panel terminal user interface, modelled on Midnight Commander.
//!
//! ```text
//! ┌ BOOX Go Color 7 · serial 6DE76AE3 · 480 Mbit/s ───────────────────────────┐
//! │ Local: ~/Books            │ internal:/Books                              │
//! │  ..                       │  ..                                           │
//! │  Roman.epub    1.2 MiB    │  Sci-Fi/                                      │
//! │  Notes.pdf     340 KiB    │  Drei Sonnen.epub          2.1 MiB            │
//! ├───────────────────────────┴───────────────────────────────────────────────┤
//! │ Roman.epub   1.2 MiB   2026-11-01 14:46      [40.8 GiB free]             │
//! │ Copied Roman.epub (1.2 MiB) → /Books                                      │
//! │ Space Mark  Ctrl-A All  Ctrl-C Copy  Ctrl-N New  Ctrl-Q Quit …           │
//! └───────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! # Design
//!
//! * `app` is a pure state machine: keys and worker events in, `app::Action`s out. No I/O, no
//!   rendering — which is what makes it unit-testable.
//! * `worker` owns the [`crate::session::Session`] on its own thread with its own Tokio runtime.
//!   The UI never blocks on USB, and a long listing or transfer cannot freeze the screen.
//! * `ui` renders the current state with `ratatui`. It also reports the panel height back to the
//!   app so PgUp/PgDn move by one screen.
//! * Cancellation goes through a shared `CancelToken`, so pressing Esc aborts a running transfer
//!   without waiting for the worker to pick up a message.
//!
//! The panel model is deliberately backend-neutral (`entry::Location` is either a local folder or
//! a storage + remote path), so local↔reader copies use the same code paths as the CLI.

mod app;
mod entry;
mod ui;
mod worker;

use std::path::PathBuf;
use std::sync::mpsc::TryRecvError;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event as TerminalEvent, KeyEventKind};

use crate::error::{Error, Result};

use app::{Action, App, Screen};
use worker::WorkerHandle;

/// How often input is checked while idle.
const IDLE_POLL: Duration = Duration::from_millis(100);
/// How often input is checked while a transfer animates the screen.
const BUSY_POLL: Duration = Duration::from_millis(33);

/// Run the file manager until the user quits.
///
/// Blocks the calling thread. The device is opened on the worker thread, so a slow USB handshake
/// shows up as a “connecting” screen, and a failure is reported as the usual CLI error.
pub fn run(serial: Option<String>, initial_storage: Option<String>) -> Result<()> {
    let mut worker = WorkerHandle::spawn(serial.clone())?;

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    let connecting = match &serial {
        Some(serial) => format!("device with serial {serial}"),
        None => "the connected reader".to_string(),
    };
    let mut app = App::new(cwd, connecting);

    let mut session = TerminalSession::start()?;
    let result = event_loop(
        &mut session.terminal,
        &mut app,
        &worker,
        initial_storage.as_deref(),
    );

    // Restore the terminal before the worker tears down the device, so any warning it logs after
    // this point lands on a normal screen.
    drop(session);
    worker.shutdown();
    result
}

/// The event loop: drain worker events, handle input, redraw when something changed.
fn event_loop(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    worker: &WorkerHandle,
    initial_storage: Option<&str>,
) -> Result<()> {
    let mut pending_storage = initial_storage.map(str::to_string);

    draw(terminal, app)?;

    while !app.should_quit {
        let mut dirty = false;

        loop {
            match worker.try_recv() {
                Ok(event) => {
                    let was_connecting = app.screen == Screen::Connecting;
                    let actions = app.apply(event);
                    dispatch_all(actions, worker)?;
                    dirty = true;

                    if was_connecting
                        && app.screen == Screen::Panels
                        && let Some(selector) = pending_storage.take()
                    {
                        if app.focus_storage(&selector) {
                            dispatch_all(app.reload_all(), worker)?;
                        } else {
                            app.status = format!(
                                "Storage `{selector}` is not available; showing the default"
                            );
                        }
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    return Err(Error::Worker {
                        message: "the background I/O worker stopped unexpectedly".to_string(),
                    });
                }
            }
        }

        let timeout = if app.is_animating() {
            BUSY_POLL
        } else {
            IDLE_POLL
        };
        if event::poll(timeout).map_err(|error| Error::io("reading terminal input", error))? {
            match event::read().map_err(|error| Error::io("reading terminal input", error))? {
                TerminalEvent::Key(key)
                    if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                {
                    let actions = app.on_key(key);
                    dispatch_all(actions, worker)?;
                    dirty = true;
                }
                TerminalEvent::Resize(_, _) => dirty = true,
                _ => {}
            }
        }

        if dirty || app.is_animating() {
            draw(terminal, app)?;
        }
    }

    Ok(())
}

/// Send a batch of actions to the worker, stopping at a fatal error.
fn dispatch_all(actions: Vec<Action>, worker: &WorkerHandle) -> Result<()> {
    for action in actions {
        match action {
            Action::List { panel, location } => worker.list(panel, location),
            Action::Copy(request) => worker.copy(request),
            Action::CreateDir {
                panel,
                parent,
                name,
            } => worker.create_dir(panel, parent, name),
            Action::RefreshStorages => worker.refresh_storages(),
            Action::CancelTransfer => worker.cancel(),
            Action::Abort(error) => return Err(error),
        }
    }
    Ok(())
}

fn draw(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    terminal
        .draw(|frame| ui::draw(frame, app))
        .map_err(|error| Error::io("drawing the interface", error))?;
    Ok(())
}

/// Owns the terminal state and restores it on every exit path.
struct TerminalSession {
    terminal: DefaultTerminal,
}

impl TerminalSession {
    /// Enter raw mode and the alternate screen.
    fn start() -> Result<Self> {
        let terminal =
            ratatui::try_init().map_err(|error| Error::io("initialising the terminal", error))?;
        Ok(Self { terminal })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // `ratatui::try_init` installs a panic hook that already restores the terminal; restoring
        // twice would fight with the panic message the hook is about to print.
        if !std::thread::panicking()
            && let Err(error) = ratatui::try_restore()
        {
            tracing::warn!(%error, "restoring the terminal failed");
        }
    }
}
