// apps/conary/src/ui/mod.rs
//! Single source of truth for user-facing CLI output styling.

pub(crate) mod diagnostics;
pub(crate) mod progress;
pub(crate) mod update_summary;

use console::style;
use std::io::Write;

/// Write durable text without colliding with active progress rows.
pub fn message(message: &str) {
    write_line(std::io::stdout(), message).expect("failed printing to stdout");
}

/// Write a diagnostic without colliding with active progress rows.
pub fn diagnostic(message: &str) {
    write_line(std::io::stderr(), message).expect("failed printing to stderr");
}

// Report I/O failure after the coordinator unlocks. Panicking while it is held
// would poison the lock and make progress cleanup panic during unwinding.
fn write_line(mut writer: impl Write, message: &str) -> std::io::Result<()> {
    progress::suspend(|| writeln!(writer, "{message}"))
}

/// Per-item indicator used by [`row`]/[`row_line`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Fail,
    Warn,
    Skip,
    Info,
    Off,
    Missing,
    Pending,
}

impl Status {
    fn label(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Fail => "fail",
            Status::Warn => "warn",
            Status::Skip => "skip",
            Status::Info => "info",
            Status::Off => "off",
            Status::Missing => "missing",
            Status::Pending => "pending",
        }
    }
}

const TAG_COLUMN: usize = 9;

pub fn tag(status: Status) -> String {
    let inner = status.label();
    let styled = match status {
        Status::Ok => style(inner).green(),
        Status::Fail => style(inner).red(),
        Status::Warn => style(inner).yellow(),
        Status::Skip => style(inner).dim(),
        Status::Info => style(inner).cyan(),
        Status::Off => style(inner).dim(),
        Status::Missing => style(inner).red(),
        Status::Pending => style(inner).yellow(),
    };
    format!("[{styled}]")
}

pub fn row_line(status: Status, cells: &[&str]) -> String {
    let visible = status.label().len() + 2;
    let pad = TAG_COLUMN.saturating_sub(visible);
    format!("{}{}  {}", tag(status), " ".repeat(pad), cells.join("  "))
}

pub fn error_line(msg: &str) -> String {
    format!("{}: {msg}", style("error").red().bold())
}

pub fn warn_line(msg: &str) -> String {
    format!("{}: {msg}", style("warning").yellow().bold())
}

pub fn note_line(msg: &str) -> String {
    format!("{}: {msg}", style("note").cyan().bold())
}

pub fn status_line(verb: &str, msg: &str) -> String {
    format!("{} {msg}", style(verb).green().bold())
}

pub fn heading_line(text: &str) -> String {
    style(text).bold().to_string()
}

pub fn field_line(label: &str, value: &str) -> String {
    format!("  {}: {value}", style(label).bold())
}

pub fn error(msg: &str) {
    diagnostic(&error_line(msg));
}

pub fn warn(msg: &str) {
    diagnostic(&warn_line(msg));
}

pub fn note(msg: &str) {
    diagnostic(&note_line(msg));
}

pub fn status(verb: &str, msg: &str) {
    message(&status_line(verb, msg));
}

pub fn row(status: Status, cells: &[&str]) {
    message(&row_line(status, cells));
}

pub fn heading(text: &str) {
    message(&heading_line(text));
}

pub fn field(label: &str, value: &str) {
    message(&field_line(label, value));
}

// Preserve formatting at legacy call sites while coordinating their terminal writes.
macro_rules! println {
    () => { $crate::ui::message("") };
    ($($args:tt)*) => { $crate::ui::message(&format!($($args)*)) };
}
macro_rules! eprintln {
    () => { $crate::ui::diagnostic("") };
    ($($args:tt)*) => { $crate::ui::diagnostic(&format!($($args)*)) };
}
pub(crate) use {eprintln, println};

#[cfg(test)]
mod tests {
    use super::*;

    fn plain() {
        console::set_colors_enabled(false);
    }

    #[test]
    fn tags_are_lowercase_bracketed_words() {
        plain();
        assert_eq!(tag(Status::Ok), "[ok]");
        assert_eq!(tag(Status::Fail), "[fail]");
        assert_eq!(tag(Status::Warn), "[warn]");
        assert_eq!(tag(Status::Skip), "[skip]");
        assert_eq!(tag(Status::Info), "[info]");
        assert_eq!(tag(Status::Off), "[off]");
        assert_eq!(tag(Status::Missing), "[missing]");
        assert_eq!(tag(Status::Pending), "[pending]");
    }

    #[test]
    fn rows_align_regardless_of_tag_width() {
        plain();
        let short = row_line(Status::Ok, &["alpha"]);
        let long = row_line(Status::Missing, &["beta"]);
        assert_eq!(short.find("alpha"), long.find("beta"));
    }

    #[test]
    fn message_prefixes_are_lowercase() {
        plain();
        assert_eq!(error_line("boom"), "error: boom");
        assert_eq!(warn_line("stale"), "warning: stale");
        assert_eq!(note_line("hint"), "note: hint");
        assert_eq!(status_line("Installing", "nginx"), "Installing nginx");
        assert_eq!(field_line("Arch", "x86_64"), "  Arch: x86_64");
        assert_eq!(heading_line("Installed packages:"), "Installed packages:");
    }

    #[test]
    fn failed_output_releases_the_coordinator_before_unwinding() {
        struct FailedWriter;
        impl Write for FailedWriter {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let progress = crate::commands::progress::InstallProgress::single("Installing fixture");
        let failed = std::panic::catch_unwind(|| {
            write_line(FailedWriter, "fixture output").expect("output failed");
        });
        assert!(failed.is_err());
        // Cleanup and subsequent writes must still acquire the coordinator.
        drop(progress);
        let mut output = Vec::new();
        write_line(&mut output, "retained diagnostic").unwrap();
        assert_eq!(output, b"retained diagnostic\n");
    }
}
