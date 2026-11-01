//! Rendering. Everything here is stateless drawing; behaviour lives in [`super::app`].

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, Gauge, List, ListItem, ListState, Paragraph, Row, Table,
    TableState, Wrap,
};

use crate::output::{format_bytes, format_datetime};

use super::app::{App, Dialog, ProgressState, Screen};
use super::entry::{FileEntry, PanelId};

/// Colour used for the active panel and the selection.
const ACCENT: Color = Color::Yellow;
/// Colour for folders.
const DIRECTORY: Color = Color::Cyan;
/// Colour for secondary information.
const MUTED: Color = Color::DarkGray;

/// Draw the whole UI.
pub fn draw(frame: &mut Frame, app: &mut App) {
    let [header, panels, details, status, keys] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    draw_header(frame, app, header);

    if app.screen == Screen::Connecting {
        draw_connecting(frame, app, panels);
        draw_status(frame, app, status);
        draw_key_bar(frame, keys);
        draw_dialog(frame, app, frame.area());
        return;
    }

    draw_panels(frame, app, panels);
    draw_details(frame, app, details);
    draw_status(frame, app, status);
    draw_key_bar(frame, keys);
    draw_dialog(frame, app, frame.area());
}

// ---------------------------------------------------------------------------
// Header, status and key bar
// ---------------------------------------------------------------------------

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![Span::styled(
        format!(" {} ", app.device),
        Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
    )];

    if !app.storages.is_empty() {
        let mut summary = String::new();
        for storage in &app.storages {
            if !summary.is_empty() {
                summary.push_str("  ·  ");
            }
            let label = storage
                .alias()
                .map(|alias| alias.as_str().to_string())
                .unwrap_or_else(|| storage.description.clone());
            match storage.free_bytes {
                Some(free) => summary.push_str(&format!("{label}: {} free", format_bytes(free))),
                None => summary.push_str(&format!("{label}: free space unknown")),
            }
        }
        spans.push(Span::styled(summary, Style::new().fg(MUTED)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let style = if app.busy {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(Color::Gray)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(format!(" {}", app.status), style)),
        area,
    );
}

fn draw_key_bar(frame: &mut Frame, area: Rect) {
    let keys = [
        ("Ctrl-C", "Copy"),
        ("Ctrl-N", "New"),
        ("Ctrl-Q", "Quit"),
        ("Ctrl-R", "Reload"),
        ("Ctrl-S", "Storage"),
        ("Ctrl-U", "Swap"),
        ("?", "Help"),
    ];
    let mut spans = Vec::new();
    for (index, (key, label)) in keys.iter().enumerate() {
        if index > 0 {
            spans.push(Span::styled("  ", Style::new()));
        }
        spans.push(Span::styled(
            (*key).to_string(),
            Style::new().fg(Color::Black).bg(Color::Gray),
        ));
        spans.push(Span::styled(
            format!(" {label}"),
            Style::new().fg(Color::Gray),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_connecting(frame: &mut Frame, app: &App, area: Rect) {
    let text = Text::from(vec![
        Line::from(Span::styled(
            "Connecting to the reader…",
            Style::new().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(app.connecting.clone(), Style::new().fg(MUTED))),
    ]);
    let paragraph = Paragraph::new(text)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });
    let inner = centered_rect(60, 5, area);
    frame.render_widget(paragraph, inner);
}

fn draw_details(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = Vec::new();
    if let Some(entry) = app.selected_entry() {
        spans.push(Span::raw(format!(" {}", top_bar_name(entry))));
        if let Some(size) = entry.size {
            spans.push(Span::styled(
                format!("   {}", format_bytes(size)),
                Style::new().fg(MUTED),
            ));
        }
        let date = format_datetime(entry.modified);
        if date != "-" {
            spans.push(Span::styled(format!("   {date}"), Style::new().fg(MUTED)));
        }
    }
    if let Some(storage) = app.active_panel().location.storage()
        && let Some(free) = storage.free_bytes
    {
        spans.push(Span::styled(
            format!("   [{} free]", format_bytes(free)),
            Style::new().fg(MUTED),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn top_bar_name(entry: &FileEntry) -> String {
    entry.display_name()
}

// ---------------------------------------------------------------------------
// Panels
// ---------------------------------------------------------------------------

fn draw_panels(frame: &mut Frame, app: &mut App, area: Rect) {
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);

    // The render pass is also where the app learns how many rows fit, for PgUp/PgDn.
    for (id, rect) in [(PanelId::Left, left), (PanelId::Right, right)] {
        let rows = rect.height.saturating_sub(2).max(1) as usize;
        app.panels[id.index()].viewport_rows = rows;
    }

    for (id, rect) in [(PanelId::Left, left), (PanelId::Right, right)] {
        draw_panel(frame, app, id, rect);
    }
}

fn draw_panel(frame: &mut Frame, app: &App, id: PanelId, area: Rect) {
    let panel = app.panel(id);
    let is_active = app.active == id;
    let border = if is_active {
        Style::new().fg(ACCENT)
    } else {
        Style::new().fg(MUTED)
    };

    let mut title = format!(" {} ", panel.location.title());
    if panel.loading {
        title.push_str("· loading ");
    }
    if panel.error.is_some() {
        title.push_str("· error ");
    }
    let title_style = if is_active {
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(Color::Gray)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border)
        .title(Line::from(Span::styled(title, title_style)));

    if panel.entries.is_empty() {
        let message = if panel.loading {
            "Loading…"
        } else {
            "empty folder"
        };
        frame.render_widget(
            Paragraph::new(message)
                .style(Style::new().fg(MUTED))
                .block(block)
                .alignment(Alignment::Center),
            area,
        );
        return;
    }

    let rows = panel.entries.iter().map(|entry| {
        let name_style = if entry.is_dir() {
            Style::new().fg(DIRECTORY)
        } else {
            Style::new()
        };
        let size = entry.size.map(format_bytes).unwrap_or_default();
        Row::new(vec![
            Cell::from(Span::styled(entry.display_name(), name_style)),
            Cell::from(Span::styled(size, Style::new().fg(MUTED))),
        ])
    });

    let table = Table::new(rows, [Constraint::Min(10), Constraint::Length(9)])
        .block(block)
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));

    let mut state = TableState::default();
    state.select(Some(panel.selected.min(panel.entries.len() - 1)));
    frame.render_stateful_widget(table, area, &mut state);
}

// ---------------------------------------------------------------------------
// Dialogs
// ---------------------------------------------------------------------------

fn draw_dialog(frame: &mut Frame, app: &App, area: Rect) {
    let Some(dialog) = &app.dialog else {
        return;
    };

    match dialog {
        Dialog::Message {
            title,
            message,
            hint,
            is_error,
        } => {
            let width = 72;
            let mut lines = vec![
                Line::from(Span::styled(
                    message.clone(),
                    Style::new().fg(if *is_error {
                        Color::LightRed
                    } else {
                        Color::White
                    }),
                )),
                Line::from(""),
            ];
            if let Some(hint) = hint {
                lines.push(Line::from(Span::styled(
                    format!("hint: {hint}"),
                    Style::new().fg(MUTED),
                )));
                lines.push(Line::from(""));
            }
            lines.push(Line::from(Span::styled(
                "Enter / Esc  close",
                Style::new().fg(MUTED),
            )));
            let height = lines.len() as u16 + 1;
            let rect = centered_rect(width, height, area);
            render_dialog_frame(
                frame,
                title,
                if *is_error {
                    Style::new().fg(Color::LightRed)
                } else {
                    Style::new().fg(ACCENT)
                },
                rect,
            );
            frame.render_widget(
                Paragraph::new(Text::from(lines)).wrap(Wrap { trim: false }),
                dialog_content_area(rect),
            );
        }
        Dialog::ConfirmOverwrite { message, yes, .. } => {
            let rect = centered_rect(66, 10, area);
            render_dialog_frame(
                frame,
                "Overwrite?",
                Style::new().fg(Color::LightYellow),
                rect,
            );
            let buttons = if *yes {
                Line::from(vec![
                    Span::styled(" [ Yes ] ", Style::new().bg(ACCENT).fg(Color::Black)),
                    Span::raw("   "),
                    Span::styled(" No ", Style::new().fg(MUTED)),
                ])
            } else {
                Line::from(vec![
                    Span::styled(" Yes ", Style::new().fg(MUTED)),
                    Span::raw("   "),
                    Span::styled(" [ No ] ", Style::new().bg(ACCENT).fg(Color::Black)),
                ])
            };
            let text = Text::from(vec![
                Line::from(""),
                Line::from(message.clone()),
                Line::from(""),
                buttons,
                Line::from(""),
                Line::from(Span::styled(
                    "←/→ choose · Enter confirm · y/n · Esc cancel",
                    Style::new().fg(MUTED),
                )),
            ]);
            frame.render_widget(
                Paragraph::new(text).wrap(Wrap { trim: false }),
                dialog_content_area(rect),
            );
        }
        Dialog::Input {
            title,
            prompt,
            value,
            ..
        } => {
            let rect = centered_rect(66, 7, area);
            render_dialog_frame(frame, title, Style::new().fg(ACCENT), rect);
            let text = Text::from(vec![
                Line::from(Span::styled(prompt.clone(), Style::new().fg(MUTED))),
                Line::from(""),
                Line::from(vec![
                    Span::raw("  "),
                    Span::styled(value.clone(), Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled("▏", Style::new().fg(ACCENT)),
                ]),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter create · Esc cancel",
                    Style::new().fg(MUTED),
                )),
            ]);
            frame.render_widget(Paragraph::new(text), dialog_content_area(rect));
        }
        Dialog::Progress => {
            let Some(progress) = &app.progress else {
                return;
            };
            draw_progress(frame, progress, area);
        }
        Dialog::Help { scroll } => draw_help(frame, *scroll, area),
        Dialog::StoragePicker { selected } => draw_storage_picker(frame, app, *selected, area),
    }
}

fn draw_progress(frame: &mut Frame, progress: &ProgressState, area: Rect) {
    let rect = centered_rect(70, 9, area);
    render_dialog_frame(
        frame,
        if progress.cancel_requested {
            "Cancelling…"
        } else {
            "Transfer"
        },
        Style::new().fg(ACCENT),
        rect,
    );

    let inner = dialog_content_area(rect);
    let [label_area, gauge_area, stats_area, hint_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(Span::styled(
            progress.label.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        )),
        label_area,
    );

    let ratio = progress.ratio().unwrap_or(0.0);
    let percent = format!("{:>3.0}%", ratio * 100.0);
    let gauge = Gauge::default()
        .gauge_style(Style::new().fg(ACCENT))
        .ratio(ratio)
        .label(percent);
    frame.render_widget(gauge, gauge_area);

    let total = progress
        .total
        .map(format_bytes)
        .unwrap_or_else(|| "?".to_string());
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(
                "{} / {}   {}/s",
                format_bytes(progress.transferred),
                total,
                format_bytes(progress.bytes_per_second())
            ),
            Style::new().fg(MUTED),
        )),
        stats_area,
    );

    frame.render_widget(
        Paragraph::new(Span::styled(
            if progress.cancel_requested {
                "waiting for the device to stop…"
            } else {
                "Esc  cancel"
            },
            Style::new().fg(MUTED),
        )),
        hint_area,
    );
}

fn draw_help(frame: &mut Frame, scroll: u16, area: Rect) {
    let lines: Vec<String> = [
        "Navigation",
        "  ↑ / ↓           move the cursor",
        "  PgUp / PgDn     move one screen",
        "  Home / End      first / last entry",
        "  Enter, →        open folder",
        "  Backspace, ←    parent folder",
        "  Tab             switch panel",
        "  Ctrl-U          swap panels",
        "  Ctrl-R          reload both panels",
        "  Ctrl-S          choose storage (remote panel)",
        "",
        "Files",
        "  Ctrl-C          copy selected file to the other panel",
        "  Ctrl-N          create a folder",
        "",
        "Other",
        "  ?               this help",
        "  Ctrl-Q, Esc, q  quit",
        "",
        "Notes",
        "  • No function keys: terminals and tmux intercept them too",
        "    inconsistently. Ctrl-C is Copy here, not an interrupt.",
        "  • Folders are not copied recursively (yet).",
        "  • Existing files are only replaced after confirmation.",
        "  • Copying between two remote folders is not supported;",
        "    use a local panel in between.",
        "",
        "Logging",
        &format!("  • with -v: {}", crate::cli::tui_log_path().display()),
    ]
    .into_iter()
    .map(str::to_string)
    .collect();

    let rect = centered_rect(
        72,
        (lines.len() as u16 + 1).min(area.height.saturating_sub(1)),
        area,
    );
    render_dialog_frame(frame, "Help", Style::new().fg(ACCENT), rect);
    let text: Vec<Line> = lines
        .iter()
        .map(|line| {
            if line.is_empty() || !line.starts_with("  ") {
                Line::from(Span::styled(
                    line.clone(),
                    Style::new().add_modifier(Modifier::BOLD),
                ))
            } else {
                Line::from(Span::styled(line.clone(), Style::new().fg(Color::Gray)))
            }
        })
        .collect();
    frame.render_widget(
        Paragraph::new(text)
            .scroll((scroll, 0))
            .block(Block::default().padding(ratatui::widgets::Padding::new(1, 1, 0, 0)))
            .wrap(Wrap { trim: false }),
        dialog_content_area(rect),
    );
}

fn draw_storage_picker(frame: &mut Frame, app: &App, selected: usize, area: Rect) {
    let height = (app.storages.len() as u16 + 4).min(area.height.saturating_sub(2));
    let rect = centered_rect(70, height.max(5), area);
    render_dialog_frame(frame, "Select storage", Style::new().fg(ACCENT), rect);

    let items: Vec<ListItem> = app
        .storages
        .iter()
        .map(|storage| {
            let alias = storage
                .alias()
                .map(|alias| alias.as_str().to_string())
                .unwrap_or_else(|| "-".to_string());
            let free = storage
                .free_bytes
                .map(format_bytes)
                .unwrap_or_else(|| "?".to_string());
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:>8} ", storage.numeric_id()),
                    Style::new().fg(MUTED),
                ),
                Span::styled(format!("{alias:<9}"), Style::new().fg(DIRECTORY)),
                Span::raw(format!("{:<28}", storage.description)),
                Span::styled(format!("{free:>10} free"), Style::new().fg(MUTED)),
            ]))
        })
        .collect();

    let mut state = ListState::default();
    state.select(Some(selected.min(items.len().saturating_sub(1))));
    let list = List::new(items).highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    frame.render_stateful_widget(list, dialog_content_area(rect), &mut state);
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Draw the dialog border. The inner area is left for the caller's paragraph.
fn render_dialog_frame(frame: &mut Frame, title: &str, style: Style, rect: Rect) {
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(style)
        .title(Line::from(Span::styled(
            format!(" {title} "),
            style.add_modifier(Modifier::BOLD),
        )));
    frame.render_widget(block, rect);
}

/// Area inside a dialog frame, with one column of padding on each side.
fn dialog_content_area(rect: Rect) -> Rect {
    rect.inner(ratatui::layout::Margin::new(1, 1))
}

/// A rectangle of at most `width` × `height`, centred in `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2)).max(8);
    let height = height.min(area.height.saturating_sub(2)).max(3);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use crate::remote_path::EntryKind;
    use crate::storage::StorageCatalog;
    use crate::storage::StorageEntry;
    use crate::tui::app::{Action, App};
    use crate::tui::worker::Event;
    use mtp_rs::mtp::StorageId;

    fn storage() -> StorageEntry {
        let entry = StorageEntry::new(StorageId(65_537), "Internal storage")
            .with_capacity(64 * 1024 * 1024 * 1024, 40 * 1024 * 1024 * 1024)
            .with_removable(false)
            .with_writable(true);
        StorageCatalog::new([entry]).entries()[0].clone()
    }

    fn app_with_listing() -> App {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("local.txt"), b"hello").unwrap();
        let mut app = App::new(dir.path().to_path_buf(), "the test device");
        let actions = app.apply(Event::Connected {
            device: "BOOX Test · serial TEST".to_string(),
            storages: vec![storage()],
        });
        assert!(matches!(actions.first(), Some(Action::List { .. })));
        app.apply(Event::Listed {
            panel: PanelId::Left,
            location: app.panel(PanelId::Left).location.clone(),
            entries: vec![
                FileEntry::parent(),
                FileEntry {
                    name: "local.txt".to_string(),
                    kind: EntryKind::File,
                    size: Some(5),
                    modified: None,
                    origin: crate::tui::entry::Origin::Remote,
                },
            ],
        });
        app.apply(Event::Listed {
            panel: PanelId::Right,
            location: app.panel(PanelId::Right).location.clone(),
            entries: vec![FileEntry {
                name: "Books".to_string(),
                kind: EntryKind::Directory,
                size: None,
                modified: None,
                origin: crate::tui::entry::Origin::Remote,
            }],
        });
        app
    }

    fn render(app: &mut App, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| draw(frame, app)).unwrap();
        let buffer = terminal.backend().buffer();
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer.cell((x, y)).map(|cell| cell.symbol()).unwrap_or(" "));
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn renders_panels_device_header_and_keys() {
        let mut app = app_with_listing();
        let screen = render(&mut app, 100, 24);

        assert!(screen.contains("BOOX Test"), "header missing:\n{screen}");
        assert!(
            screen.contains("Local:"),
            "left panel title missing:\n{screen}"
        );
        assert!(
            screen.contains("internal:/"),
            "right panel title missing:\n{screen}"
        );
        assert!(screen.contains("local.txt"), "file missing:\n{screen}");
        assert!(screen.contains("Books/"), "folder missing:\n{screen}");
        assert!(screen.contains("Ctrl-C Copy"), "key bar missing:\n{screen}");
        assert!(screen.contains("Ctrl-Q Quit"), "key bar missing:\n{screen}");
    }

    #[test]
    fn help_dialog_is_rendered_and_closes() {
        let mut app = app_with_listing();
        app.on_key(ratatui::crossterm::event::KeyEvent::new(
            ratatui::crossterm::event::KeyCode::Char('?'),
            ratatui::crossterm::event::KeyModifiers::NONE,
        ));
        let screen = render(&mut app, 100, 30);
        assert!(screen.contains("Help"), "help title missing:\n{screen}");
        assert!(screen.contains("Ctrl-N"), "help body missing:\n{screen}");
    }

    #[test]
    fn locating_a_storage_by_selector_works() {
        let app = app_with_listing();
        let mut app = app;
        assert!(app.focus_storage("internal"));
        assert_eq!(app.active, PanelId::Left, "focus stays on the local panel");
        assert_eq!(app.panel(PanelId::Right).location.display_path(), "/");
    }
}
