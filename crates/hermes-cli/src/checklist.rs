//! Curses-style interactive multi-select checklist.
//!
//! Ported from Python `hermes_cli/curses_ui.py`.
//! Provides an interactive multi-select list with keyboard navigation
//! (↑↓ navigate, Space toggle, Enter confirm, Esc cancel) and a
//! numbered text fallback for non-TTY terminals.

use std::collections::HashSet;
use std::io::{self, Write};

use crossterm::event::{self, Event, KeyCode, KeyEvent};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{cursor, execute, terminal};

/// Result of a checklist interaction.
#[derive(Debug, Clone)]
pub struct ChecklistResult {
    /// Indices of selected items.
    pub selected: HashSet<usize>,
    /// Whether the user confirmed (Enter) or cancelled (Esc).
    pub confirmed: bool,
}

/// Result of a single-select interaction.
#[derive(Debug, Clone, Copy)]
pub struct SelectResult {
    /// Selected index.
    pub index: usize,
    /// Whether the user confirmed (Enter) or cancelled (Esc).
    pub confirmed: bool,
}

/// Run an interactive multi-select checklist in the terminal.
///
/// # Arguments
/// * `title` - Header line displayed above the checklist
/// * `items` - Display labels for each row
/// * `selected` - Indices that start checked (pre-selected)
/// * `status_fn` - Optional callback that returns a status string for the bottom row
///
/// # Returns
/// A `ChecklistResult` with the final selection and whether the user confirmed.
pub fn curses_checklist(
    title: &str,
    items: &[String],
    selected: &HashSet<usize>,
    status_fn: Option<&dyn Fn(&HashSet<usize>) -> String>,
) -> ChecklistResult {
    // If not a TTY, fall back to numbered text input
    if !atty_is_tty() {
        return numbered_fallback(title, items, selected, status_fn);
    }

    match curses_checklist_inner(title, items, selected, status_fn) {
        Ok(result) => result,
        Err(_) => {
            // On error, fall back to numbered input
            numbered_fallback(title, items, selected, status_fn)
        }
    }
}

/// Run an interactive single-select list in the terminal.
///
/// Uses arrow keys (`↑↓` / `j``k`) and Enter to confirm. Esc cancels and
/// returns `confirmed = false` while keeping the initial index.
pub fn curses_select(title: &str, items: &[String], initial_index: usize) -> SelectResult {
    if items.is_empty() {
        return SelectResult {
            index: 0,
            confirmed: false,
        };
    }

    let clamped_initial = initial_index.min(items.len().saturating_sub(1));
    if !atty_is_tty() {
        return numbered_select_fallback(title, items, clamped_initial);
    }

    match curses_select_inner(title, items, clamped_initial) {
        Ok(result) => result,
        Err(_) => numbered_select_fallback(title, items, clamped_initial),
    }
}

/// Check if stdin is a TTY (best-effort).
fn atty_is_tty() -> bool {
    // crossterm's is_raw_mode_enabled is not what we want;
    // use a simple heuristic: try to get terminal size
    terminal::size().is_ok()
}

/// Inner implementation using crossterm raw mode.
fn curses_checklist_inner(
    title: &str,
    items: &[String],
    initial_selected: &HashSet<usize>,
    status_fn: Option<&dyn Fn(&HashSet<usize>) -> String>,
) -> Result<ChecklistResult, io::Error> {
    if items.is_empty() {
        return Ok(ChecklistResult {
            selected: initial_selected.clone(),
            confirmed: true,
        });
    }

    let mut chosen = initial_selected.clone();
    let mut cursor_pos: usize = 0;
    let mut scroll_offset: usize = 0;
    let mut stdout = io::stdout();

    enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;

    let result = loop {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let max_x = cols as usize;
        let max_y = rows as usize;

        // Reserve rows: 2 for header, 1 for status
        let footer_rows = if status_fn.is_some() { 1 } else { 0 };
        let visible_rows = max_y.saturating_sub(3 + footer_rows);

        // Adjust scroll
        if cursor_pos < scroll_offset {
            scroll_offset = cursor_pos;
        } else if cursor_pos >= scroll_offset + visible_rows {
            scroll_offset = cursor_pos.saturating_sub(visible_rows) + 1;
        }

        // Clear and draw
        execute!(stdout, terminal::Clear(terminal::ClearType::All))?;

        // Header
        execute!(stdout, cursor::MoveTo(0, 0))?;
        write!(
            stdout,
            "\x1b[1;33m{}\x1b[0m",
            &title[..title.len().min(max_x)]
        )?;
        execute!(stdout, cursor::MoveTo(0, 1))?;
        write!(
            stdout,
            "\x1b[2m  ↑↓ navigate  SPACE toggle  ENTER confirm  ESC cancel\x1b[0m"
        )?;

        // Items
        for (draw_i, i) in
            (scroll_offset..items.len().min(scroll_offset + visible_rows)).enumerate()
        {
            let y = draw_i + 3;
            if y >= max_y.saturating_sub(footer_rows) {
                break;
            }
            let check = if chosen.contains(&i) { "✓" } else { " " };
            let arrow = if i == cursor_pos { "→" } else { " " };
            let line = format!(" {} [{}] {}", arrow, check, &items[i]);
            let truncated = &line[..line.len().min(max_x)];

            execute!(stdout, cursor::MoveTo(0, y as u16))?;
            if i == cursor_pos {
                write!(stdout, "\x1b[1;32m{}\x1b[0m", truncated)?;
            } else {
                write!(stdout, "{}", truncated)?;
            }
        }

        // Status bar
        if let Some(ref sfn) = status_fn {
            let status_text = sfn(&chosen);
            if !status_text.is_empty() {
                let sx = max_x.saturating_sub(status_text.len() + 1);
                execute!(stdout, cursor::MoveTo(sx as u16, (max_y - 1) as u16))?;
                write!(
                    stdout,
                    "\x1b[2m{}\x1b[0m",
                    &status_text[..status_text.len().min(max_x)]
                )?;
            }
        }

        stdout.flush()?;

        // Read key
        if let Ok(Event::Key(KeyEvent { code, .. })) = event::read() {
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor_pos = if cursor_pos == 0 {
                        items.len() - 1
                    } else {
                        cursor_pos - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor_pos = (cursor_pos + 1) % items.len();
                }
                KeyCode::Char(' ') => {
                    if chosen.contains(&cursor_pos) {
                        chosen.remove(&cursor_pos);
                    } else {
                        chosen.insert(cursor_pos);
                    }
                }
                KeyCode::Enter => {
                    break ChecklistResult {
                        selected: chosen,
                        confirmed: true,
                    };
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    break ChecklistResult {
                        selected: initial_selected.clone(),
                        confirmed: false,
                    };
                }
                _ => {}
            }
        }
    };

    // Restore terminal
    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    disable_raw_mode()?;

    Ok(result)
}

/// Text-based numbered fallback for non-TTY terminals.
fn numbered_fallback(
    title: &str,
    items: &[String],
    initial_selected: &HashSet<usize>,
    status_fn: Option<&dyn Fn(&HashSet<usize>) -> String>,
) -> ChecklistResult {
    let mut chosen = initial_selected.clone();

    println!("\n  \x1b[33m{}\x1b[0m", title);
    println!("  \x1b[2mToggle by number, Enter to confirm.\x1b[0m\n");

    loop {
        for (i, label) in items.iter().enumerate() {
            let marker = if chosen.contains(&i) {
                "\x1b[32m[✓]\x1b[0m"
            } else {
                "[ ]"
            };
            println!("  {} {:>2}. {}", marker, i + 1, label);
        }

        if let Some(ref sfn) = status_fn {
            let status_text = sfn(&chosen);
            if !status_text.is_empty() {
                println!("\n  \x1b[2m{}\x1b[0m", status_text);
            }
        }

        println!();
        print!("  \x1b[2mToggle # (or Enter to confirm): \x1b[0m");
        io::stdout().flush().ok();

        let mut input = String::new();
        match io::stdin().read_line(&mut input) {
            Ok(_) => {
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    return ChecklistResult {
                        selected: chosen,
                        confirmed: true,
                    };
                }
                if let Ok(idx) = trimmed.parse::<usize>() {
                    let idx = idx.saturating_sub(1);
                    if idx < items.len() {
                        if chosen.contains(&idx) {
                            chosen.remove(&idx);
                        } else {
                            chosen.insert(idx);
                        }
                    }
                }
            }
            Err(_) => {
                return ChecklistResult {
                    selected: initial_selected.clone(),
                    confirmed: false,
                };
            }
        }
        println!();
    }
}

fn curses_select_inner(
    title: &str,
    items: &[String],
    initial_index: usize,
) -> Result<SelectResult, io::Error> {
    let mut cursor_pos = initial_index;
    let mut scroll_offset: usize = 0;
    let mut stdout = io::stdout();

    enable_raw_mode()?;
    execute!(stdout, terminal::EnterAlternateScreen, cursor::Hide)?;

    let result = loop {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let max_x = cols as usize;
        let max_y = rows as usize;
        let visible_rows = max_y.saturating_sub(4);

        if cursor_pos < scroll_offset {
            scroll_offset = cursor_pos;
        } else if cursor_pos >= scroll_offset + visible_rows {
            scroll_offset = cursor_pos.saturating_sub(visible_rows) + 1;
        }

        execute!(stdout, terminal::Clear(terminal::ClearType::All))?;

        execute!(stdout, cursor::MoveTo(0, 0))?;
        write!(
            stdout,
            "\x1b[1;33m{}\x1b[0m",
            &title[..title.len().min(max_x)]
        )?;
        execute!(stdout, cursor::MoveTo(0, 1))?;
        write!(
            stdout,
            "\x1b[2m  ↑↓ navigate  ENTER confirm  ESC cancel\x1b[0m"
        )?;

        for (draw_i, i) in
            (scroll_offset..items.len().min(scroll_offset + visible_rows)).enumerate()
        {
            let y = draw_i + 3;
            if y >= max_y {
                break;
            }
            let bullet = if i == cursor_pos { "▶" } else { " " };
            let line = format!(" {} {}", bullet, &items[i]);
            let truncated = &line[..line.len().min(max_x)];

            execute!(stdout, cursor::MoveTo(0, y as u16))?;
            if i == cursor_pos {
                write!(stdout, "\x1b[1;32m{}\x1b[0m", truncated)?;
            } else {
                write!(stdout, "{}", truncated)?;
            }
        }

        stdout.flush()?;

        if let Ok(Event::Key(KeyEvent { code, .. })) = event::read() {
            match code {
                KeyCode::Up | KeyCode::Char('k') => {
                    cursor_pos = if cursor_pos == 0 {
                        items.len() - 1
                    } else {
                        cursor_pos - 1
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    cursor_pos = (cursor_pos + 1) % items.len();
                }
                KeyCode::Enter => {
                    break SelectResult {
                        index: cursor_pos,
                        confirmed: true,
                    };
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    break SelectResult {
                        index: initial_index,
                        confirmed: false,
                    };
                }
                _ => {}
            }
        }
    };

    execute!(stdout, cursor::Show, terminal::LeaveAlternateScreen)?;
    disable_raw_mode()?;

    Ok(result)
}

fn numbered_select_fallback(title: &str, items: &[String], initial_index: usize) -> SelectResult {
    println!("\n  \x1b[33m{}\x1b[0m", title);
    for (i, label) in items.iter().enumerate() {
        let marker = if i == initial_index { "*" } else { " " };
        println!("  {} {:>2}. {}", marker, i + 1, label);
    }
    println!();
    print!(
        "  \x1b[2mChoose # [{}] (Enter keeps default): \x1b[0m",
        initial_index + 1
    );
    io::stdout().flush().ok();

    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_ok() {
        let trimmed = input.trim();
        if trimmed.is_empty() {
            return SelectResult {
                index: initial_index,
                confirmed: true,
            };
        }
        if let Ok(choice) = trimmed.parse::<usize>() {
            let idx = choice.saturating_sub(1);
            if idx < items.len() {
                return SelectResult {
                    index: idx,
                    confirmed: true,
                };
            }
        }
    }

    SelectResult {
        index: initial_index,
        confirmed: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_checklist_result_default() {
        let result = ChecklistResult {
            selected: HashSet::new(),
            confirmed: true,
        };
        assert!(result.confirmed);
        assert!(result.selected.is_empty());
    }

    #[test]
    fn test_numbered_fallback_empty_items() {
        // With empty items, curses_checklist returns immediately
        let result = curses_checklist("Test", &[], &HashSet::new(), None);
        assert!(result.confirmed);
    }

    #[test]
    fn test_curses_select_empty_items() {
        let result = curses_select("Select", &[], 0);
        assert!(!result.confirmed);
        assert_eq!(result.index, 0);
    }
}
