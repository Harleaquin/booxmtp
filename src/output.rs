//! Rendering helpers: tables, byte sizes, dates, escaped names and the progress line.
//!
//! Everything here is *pure formatting*: functions take plain data and return strings (or, for
//! progress, write a single transient line to a writer). The CLI is the only place that decides
//! which stream is used. A future TUI or GUI can reuse the byte/date/name formatting and draw its
//! own widgets instead of the [`Table`].

use std::fmt::Write as _;
use std::io::{IsTerminal, Write as _};
use std::time::{Duration, Instant};

use mtp_rs::UsbSpeed;
use mtp_rs::mtp::DateTime;

use crate::transfer::{TransferDirection, TransferProgress};

/// Binary units, as used by macOS and by `booxmtp storages`.
const BYTE_UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

/// Format a byte count with binary units and one decimal, e.g. `24.1 GiB`.
pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < BYTE_UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", BYTE_UNITS[unit])
}

/// Format an optional byte count, using `-` for “the device did not say”.
pub fn format_optional_bytes(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "-".to_string(), format_bytes)
}

/// Format an optional MTP date as `YYYY-MM-DD HH:MM:SS`, appending the offset when the device
/// provided one.
///
/// Most devices (Android included) send zoneless local time, which is what this prints.
pub fn format_datetime(datetime: Option<DateTime>) -> String {
    let Some(datetime) = datetime else {
        return "-".to_string();
    };
    let mut rendered = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        datetime.year,
        datetime.month,
        datetime.day,
        datetime.hour,
        datetime.minute,
        datetime.second
    );
    if let Some(offset) = datetime.offset {
        let minutes = offset.minutes();
        let sign = if minutes < 0 { '-' } else { '+' };
        let minutes = minutes.abs();
        let _ = write!(rendered, " {sign}{:02}:{:02}", minutes / 60, minutes % 60);
    }
    rendered
}

/// Make a device-provided name safe to print on one line.
///
/// UTF-8 is preserved; control characters (newlines, terminals escape characters, NUL) are
/// rendered as Rust escapes so a hostile or broken file name cannot mangle the output.
pub fn escape_name(name: &str) -> String {
    let mut escaped = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_control() {
            escaped.extend(character.escape_default());
        } else {
            escaped.push(character);
        }
    }
    escaped
}

/// Human-readable USB link speed.
pub fn format_usb_speed(speed: Option<UsbSpeed>) -> String {
    match speed {
        None => "-".to_string(),
        Some(UsbSpeed::Low) => "1.5 Mbit/s (USB 1.0)".to_string(),
        Some(UsbSpeed::Full) => "12 Mbit/s (USB 1.1)".to_string(),
        Some(UsbSpeed::High) => "480 Mbit/s (USB 2.0)".to_string(),
        Some(UsbSpeed::Super) => "5 Gbit/s (USB 3.2 Gen 1)".to_string(),
        Some(UsbSpeed::SuperPlus) => "10 Gbit/s (USB 3.2 Gen 2)".to_string(),
        // `UsbSpeed` is `#[non_exhaustive]`.
        Some(other) => format!("{other:?}"),
    }
}

/// Column alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// Text starts at the column's left edge.
    Left,
    /// Text ends at the column's right edge (numbers).
    Right,
}

/// A simple, dependency-free text table.
///
/// Column widths are computed from the widest cell; cells are separated by two spaces. Widths are
/// counted in `char`s, so wide CJK glyphs and emoji still line up approximately rather than
/// perfectly (a full terminal-width calculation would need Unicode tables).
#[derive(Debug, Clone, Default)]
pub struct Table {
    headers: Vec<String>,
    aligns: Vec<Align>,
    rows: Vec<Vec<String>>,
}

impl Table {
    /// Start a table with a header row. Every column defaults to left alignment.
    pub fn new(headers: &[&str]) -> Self {
        Self {
            headers: headers.iter().map(|header| header.to_string()).collect(),
            aligns: vec![Align::Left; headers.len()],
            rows: Vec::new(),
        }
    }

    /// Set the alignment of one column (ignored when the index is out of range).
    pub fn align_column(mut self, index: usize, align: Align) -> Self {
        if let Some(slot) = self.aligns.get_mut(index) {
            *slot = align;
        }
        self
    }

    /// Append one row.
    pub fn push<I, S>(&mut self, cells: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut row: Vec<String> = cells.into_iter().map(Into::into).collect();
        // Keep ragged input harmless: pad short rows, ignore surplus cells.
        row.resize(self.headers.len(), String::new());
        row.truncate(self.headers.len());
        self.rows.push(row);
    }

    /// Whether the table has no body rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Render the table, one line per row, without a trailing newline.
    pub fn render(&self) -> String {
        if self.headers.is_empty() {
            return String::new();
        }

        let mut widths: Vec<usize> = self.headers.iter().map(|header| width(header)).collect();
        for row in &self.rows {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(width(cell));
            }
        }

        let mut rendered = String::new();
        render_row(&mut rendered, &self.headers, &widths, &self.aligns);
        for row in &self.rows {
            rendered.push('\n');
            render_row(&mut rendered, row, &widths, &self.aligns);
        }
        rendered
    }
}

/// Visible width of a cell (characters, not bytes).
fn width(text: &str) -> usize {
    text.chars().count()
}

fn render_row(out: &mut String, cells: &[String], widths: &[usize], aligns: &[Align]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push_str("  ");
        }
        let padding = widths[index].saturating_sub(width(cell));
        match aligns.get(index).copied().unwrap_or(Align::Left) {
            Align::Left => {
                out.push_str(cell);
                out.extend(std::iter::repeat_n(' ', padding));
            }
            Align::Right => {
                out.extend(std::iter::repeat_n(' ', padding));
                out.push_str(cell);
            }
        }
    }
    // Trim trailing padding so table output has no trailing whitespace.
    while out.ends_with(' ') {
        out.pop();
    }
}

/// Renders transfer progress as a single transient line on `stderr`.
///
/// The line is only drawn when [`enabled`](Self::new) is true — the CLI passes
/// `stderr.is_terminal() && !no_progress`, so redirected output stays clean. Updates are throttled
/// to ten per second; [`finish`](Self::finish) clears the line so the caller can print a summary.
pub struct ProgressPrinter {
    enabled: bool,
    started: Option<Instant>,
    last_draw: Option<Instant>,
    last_width: usize,
}

impl ProgressPrinter {
    /// How often the line is redrawn.
    const REDRAW_INTERVAL: Duration = Duration::from_millis(100);

    /// Create a printer. `enabled = false` makes every method a no-op.
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            started: None,
            last_draw: None,
            last_width: 0,
        }
    }

    /// Feed one progress sample.
    pub fn update(&mut self, progress: &TransferProgress) {
        if !self.enabled {
            return;
        }

        let now = Instant::now();
        let started = *self.started.get_or_insert(now);
        let complete = progress
            .total
            .is_some_and(|total| progress.transferred >= total);
        if !complete
            && self
                .last_draw
                .is_some_and(|last| now.duration_since(last) < Self::REDRAW_INTERVAL)
        {
            return;
        }
        self.last_draw = Some(now);

        let verb = match progress.direction {
            TransferDirection::Download => "downloading",
            TransferDirection::Upload => "uploading",
        };
        let elapsed = now.duration_since(started).as_secs_f64();
        let rate = if elapsed > 0.0 {
            progress.transferred as f64 / elapsed
        } else {
            0.0
        };

        let transferred = format_bytes(progress.transferred);
        let line = match progress.total {
            Some(total) => format!(
                "{verb} {transferred} / {} {:>3.0}% {}",
                format_bytes(total),
                progress.percent().unwrap_or(0.0),
                rate_label(rate),
            ),
            None => format!("{verb} {transferred} {}", rate_label(rate)),
        };
        self.draw(&line);
    }

    /// Clear the progress line (does not print a newline of its own).
    pub fn finish(&mut self) {
        if !self.enabled || self.last_width == 0 {
            return;
        }
        let mut stderr = std::io::stderr().lock();
        let _ = write!(stderr, "\r{}\r", " ".repeat(self.last_width));
        let _ = stderr.flush();
        self.last_width = 0;
    }

    /// Draw/overwrite the transient line.
    fn draw(&mut self, line: &str) {
        let mut stderr = std::io::stderr().lock();
        let width = line.chars().count();
        let padding = self.last_width.saturating_sub(width);
        let _ = write!(stderr, "\r{line}{}", " ".repeat(padding));
        let _ = stderr.flush();
        self.last_width = width;
    }
}

fn rate_label(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_second.max(0.0) as u64))
}

/// Whether `stderr` is an interactive terminal (and progress should be drawn).
pub fn stderr_is_terminal() -> bool {
    std::io::stderr().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_use_binary_units() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1023), "1023 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(24 * 1024 * 1024 * 1024 / 10), "2.4 GiB");
        assert_eq!(format_optional_bytes(None), "-");
    }

    #[test]
    fn names_with_control_characters_are_escaped() {
        assert_eq!(escape_name("Roman.epub"), "Roman.epub");
        assert_eq!(escape_name("Bücher/Ümlaut"), "Bücher/Ümlaut");
        assert_eq!(escape_name("a\nb\tc"), "a\\nb\\tc");
        assert_eq!(escape_name("nul\0"), "nul\\u{0}");
    }

    #[test]
    fn datetimes_are_printed_verbatim() {
        let date = DateTime::new(2024, 3, 15, 14, 30, 5).unwrap();
        assert_eq!(format_datetime(Some(date)), "2024-03-15 14:30:05");
        assert_eq!(format_datetime(None), "-");
    }

    #[test]
    fn table_columns_line_up_and_have_no_trailing_spaces() {
        let mut table = Table::new(&["NAME", "SIZE"]);
        table = table.align_column(1, Align::Right);
        table.push(["a.epub", "1.0 KiB"]);
        table.push(["longer-name.pdf", "12 B"]);

        let rendered = table.render();
        let lines: Vec<&str> = rendered.lines().collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0], "NAME                SIZE");
        assert_eq!(lines[1], "a.epub           1.0 KiB");
        assert_eq!(lines[2], "longer-name.pdf     12 B");
        assert!(lines.iter().all(|line| !line.ends_with(' ')));
    }

    #[test]
    fn table_pads_ragged_rows() {
        let mut table = Table::new(&["A", "B"]);
        table.push(["only-one"]);
        assert_eq!(table.render().lines().count(), 2);
    }
}
