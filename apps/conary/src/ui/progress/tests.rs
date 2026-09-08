// apps/conary/src/ui/progress/tests.rs

use super::*;
use indicatif::TermLike;
use std::io;
use std::sync::{Arc, Mutex};

#[derive(Debug, Default)]
struct Screen {
    rows: Vec<Vec<char>>,
    row: usize,
    column: usize,
}

#[derive(Clone, Debug, Default)]
struct Terminal(Arc<Mutex<Screen>>);

impl Terminal {
    fn contents(&self) -> String {
        self.0
            .lock()
            .unwrap()
            .rows
            .iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_owned())
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned()
    }
}

impl TermLike for Terminal {
    fn width(&self) -> u16 {
        80
    }
    fn move_cursor_up(&self, n: usize) -> io::Result<()> {
        let mut screen = self.0.lock().unwrap();
        screen.row = screen.row.saturating_sub(n);
        Ok(())
    }
    fn move_cursor_down(&self, n: usize) -> io::Result<()> {
        self.0.lock().unwrap().row += n;
        Ok(())
    }
    fn move_cursor_right(&self, n: usize) -> io::Result<()> {
        self.0.lock().unwrap().column += n;
        Ok(())
    }
    fn move_cursor_left(&self, n: usize) -> io::Result<()> {
        let mut screen = self.0.lock().unwrap();
        screen.column = screen.column.saturating_sub(n);
        Ok(())
    }
    fn write_line(&self, text: &str) -> io::Result<()> {
        self.write_str(text)?;
        let mut screen = self.0.lock().unwrap();
        screen.row += 1;
        screen.column = 0;
        Ok(())
    }
    fn write_str(&self, text: &str) -> io::Result<()> {
        let mut screen = self.0.lock().unwrap();
        for ch in console::strip_ansi_codes(text).chars() {
            let Screen { rows, row, column } = &mut *screen;
            rows.resize_with(rows.len().max(*row + 1), Vec::new);
            let width = rows[*row].len().max(*column + 1);
            rows[*row].resize(width, ' ');
            rows[*row][*column] = ch;
            *column += 1;
        }
        Ok(())
    }
    fn clear_line(&self) -> io::Result<()> {
        let mut screen = self.0.lock().unwrap();
        let row = screen.row;
        if let Some(line) = screen.rows.get_mut(row) {
            line.clear();
        }
        screen.column = 0;
        Ok(())
    }
    fn flush(&self) -> io::Result<()> {
        Ok(())
    }
}

fn display(total: u64) -> (Terminal, ProgressDisplay) {
    let terminal = Terminal::default();
    let multi =
        MultiProgress::with_draw_target(ProgressDrawTarget::term_like(Box::new(terminal.clone())));
    let display = ProgressDisplay::with_terminal(multi, total, "Installing");
    display.overall.disable_steady_tick();
    if let Some(status) = &display.status {
        status.disable_steady_tick();
    }
    (terminal, display)
}

#[test]
fn single_and_unknown_totals_render_one_row_and_clear() {
    for total in [0, 1] {
        let (terminal, display) = display(total);
        display.set_status("Extracting fixture");
        display.overall.tick();
        let screen = terminal.contents();
        assert!(screen.contains("Extracting fixture"), "{screen:?}");
        assert_eq!(screen.lines().count(), 1, "{screen:?}");
        assert!(!screen.contains("0/0"));
        display.clear();
        assert_eq!(terminal.contents(), "");
        drop(display);
        assert_eq!(terminal.contents(), "");
    }
}

#[test]
fn aggregate_progress_preserves_durable_output_and_cleans_up_on_drop() {
    let (terminal, display) = display(2);
    display.set_status("Extracting fixture");
    display.set_position(1);
    display.overall.tick();
    let screen = terminal.contents();
    assert_eq!(screen.lines().count(), 2, "{screen:?}");
    assert!(screen.contains("Installing (1/2)"), "{screen:?}");
    assert!(screen.contains("Extracting fixture"), "{screen:?}");
    display
        .multi
        .suspend(|| terminal.write_line("Retained diagnostic").unwrap());
    drop(display);
    assert_eq!(terminal.contents(), "Retained diagnostic");
}

#[test]
fn nested_child_cleanup_preserves_parent_and_summary() {
    let (terminal, parent) = display(2);
    let child = ProgressDisplay::with_terminal(parent.multi.clone(), 0, "Child install");
    child.overall.disable_steady_tick();
    child.overall.tick();
    assert!(terminal.contents().contains("Child install"));
    drop(child);
    parent
        .multi
        .suspend(|| terminal.write_line("Installed fixture").unwrap());
    parent.set_position(1);
    parent.overall.tick();
    assert!(terminal.contents().contains("Installing (1/2)"));
    assert!(!terminal.contents().contains("Child install"));
    drop(parent);
    assert_eq!(terminal.contents(), "Installed fixture");
}
