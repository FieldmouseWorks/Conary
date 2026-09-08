// apps/conary/src/ui/progress.rs
//! Transient terminal rows. Command adapters own phases; command summaries own results.

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::IsTerminal;
use std::sync::OnceLock;
use std::time::Duration;

fn terminal() -> &'static MultiProgress {
    static TERMINAL: OnceLock<MultiProgress> = OnceLock::new();
    TERMINAL.get_or_init(|| {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        let target =
            if std::io::stdout().is_terminal() && std::io::stderr().is_terminal() && !no_color {
                ProgressDrawTarget::stderr()
            } else {
                ProgressDrawTarget::hidden()
            };
        MultiProgress::with_draw_target(target)
    })
}

/// Keep durable UI output above every active progress row, including nested operations.
pub(crate) fn suspend<T>(write: impl FnOnce() -> T) -> T {
    terminal().suspend(write)
}

/// One spinner for unknown/single totals, or an aggregate bar and one active row.
///
/// Every row belongs to the same terminal coordinator. Future download workers can
/// add bounded rows there without introducing another redraw owner.
pub(crate) struct ProgressDisplay {
    multi: MultiProgress,
    overall: ProgressBar,
    status: Option<ProgressBar>,
}

impl ProgressDisplay {
    pub(crate) fn new(total: u64, operation: &str) -> Self {
        Self::with_terminal(terminal().clone(), total, operation)
    }

    fn with_terminal(multi: MultiProgress, total: u64, operation: &str) -> Self {
        let overall = if total > 1 {
            let bar = ProgressBar::new(total);
            bar.set_style(
                ProgressStyle::default_bar()
                    .template("{msg} ({pos}/{len}) [{bar:40.green/dim}] {percent}%")
                    .expect("valid progress template")
                    .progress_chars("##-"),
            );
            bar
        } else {
            spinner()
        };
        overall.set_message(operation.to_owned());
        let overall = multi.add(overall);
        // Adding a hidden ProgressBar to MultiProgress replaces its draw target
        // and exposes its default 0/0 bar. An absent row must remain absent.
        let status = (total > 1).then(|| multi.add(spinner()));
        let ticker = status.as_ref().unwrap_or(&overall);
        if !multi.is_hidden() {
            ticker.enable_steady_tick(Duration::from_millis(100));
        }
        Self {
            multi,
            overall,
            status,
        }
    }

    pub(crate) fn set_status(&self, message: impl Into<String>) {
        self.status
            .as_ref()
            .unwrap_or(&self.overall)
            .set_message(message.into());
    }

    pub(crate) fn set_position(&self, completed: u64) {
        self.overall.set_position(completed);
    }

    /// Temporarily erase progress while reading input, then resume the same rows.
    pub(crate) fn suspend<T>(&self, operation: impl FnOnce() -> T) -> T {
        self.multi.suspend(operation)
    }

    pub(crate) fn clear(&self) {
        if let Some(status) = &self.status {
            status.finish_and_clear();
        }
        self.overall.finish_and_clear();
    }
}

impl Drop for ProgressDisplay {
    fn drop(&mut self) {
        // Includes early `?` returns: stop tickers and erase rows before errors print.
        self.clear();
        if let Some(status) = &self.status {
            self.multi.remove(status);
        }
        self.multi.remove(&self.overall);
    }
}

fn spinner() -> ProgressBar {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(
        ProgressStyle::default_spinner()
            .template("{spinner:.cyan} {msg}")
            .expect("valid spinner template"),
    );
    spinner
}

#[cfg(test)]
mod tests;
