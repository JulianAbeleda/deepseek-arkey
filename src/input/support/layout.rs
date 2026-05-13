use std::io::{self, Write};

use crossterm::cursor::MoveTo;
use crossterm::execute;
use crossterm::terminal::{size, Clear, ClearType};

use crate::input::composer::DOCK_RESERVED_ROWS;

pub(crate) fn terminal_width() -> usize {
    if let Some((cols, _)) = forced_terminal_size() {
        return cols as usize;
    }
    size().map(|(cols, _)| cols as usize).unwrap_or(80).max(1)
}

pub(crate) fn terminal_rows() -> u16 {
    if let Some((_, rows)) = forced_terminal_size() {
        return rows;
    }
    size().map(|(_, rows)| rows).unwrap_or(24).max(1)
}

pub(crate) fn forced_terminal_size() -> Option<(u16, u16)> {
    std::env::var("DEEPSEEK_FORCE_TTY_SIZE")
        .ok()
        .and_then(|value| parse_forced_terminal_size(&value))
}

pub(crate) fn parse_forced_terminal_size(value: &str) -> Option<(u16, u16)> {
    let (cols, rows) = value.split_once('x')?;
    let cols = cols.parse::<u16>().ok()?.max(1);
    let rows = rows.parse::<u16>().ok()?.max(1);
    Some((cols, rows))
}

pub(crate) fn dock_row() -> u16 {
    terminal_rows().saturating_sub(1)
}

pub(crate) fn output_row(reserved_bottom_lines: usize) -> u16 {
    let reserved = reserved_bottom_lines.max(1).min(DOCK_RESERVED_ROWS) as u16;
    terminal_rows().saturating_sub(reserved + 1)
}

pub(crate) fn transcript_view_height(reserved_bottom_lines: usize) -> usize {
    output_row(reserved_bottom_lines) as usize + 1
}

pub(crate) fn clear_dock_rows(stdout: &mut io::Stdout, rows: usize) -> Result<(), String> {
    if rows == 0 {
        return Ok(());
    }
    let rows = rows.min(DOCK_RESERVED_ROWS);
    let first_row = dock_row().saturating_sub(rows.saturating_sub(1) as u16);
    for row in first_row..=dock_row() {
        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub(crate) fn clear_rows_above_dock(
    stdout: &mut io::Stdout,
    reserved_bottom_lines: usize,
    rows: usize,
) -> Result<(), String> {
    if rows == 0 {
        return Ok(());
    }
    let bottom = output_row(reserved_bottom_lines);
    let rows = rows.min(bottom as usize + 1);
    let start = bottom.saturating_add(1).saturating_sub(rows as u16);
    for row in start..=bottom {
        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub(crate) fn clear_transient_rows(
    stdout: &mut io::Stdout,
    reserved_bottom_lines: usize,
    start: u16,
    rows: usize,
) -> Result<(), String> {
    if rows == 0 {
        return Ok(());
    }
    let bottom = output_row(reserved_bottom_lines);
    let end = start
        .saturating_add(rows as u16)
        .min(bottom.saturating_add(1));
    for row in start..end {
        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

pub(crate) fn set_output_scroll_region(reserved_bottom_lines: usize) -> Result<(), String> {
    let rows = terminal_rows();
    let reserved = reserved_bottom_lines.max(1) as u16;
    if rows <= reserved + 1 {
        return Ok(());
    }
    let output_bottom = rows.saturating_sub(reserved);
    let mut stdout = io::stdout();
    write!(stdout, "\x1b[1;{output_bottom}r").map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

pub(crate) fn reset_output_scroll_region() -> Result<(), String> {
    let rows = terminal_rows();
    let mut stdout = io::stdout();
    // DECSTBM (including the reset form \x1b[r) homes the cursor to (0,0) per
    // ANSI spec. Move immediately to the last row so subsequent output falls at
    // the bottom and scrolls into scrollback rather than overwriting from row 0.
    write!(stdout, "\x1b[r\x1b[{rows};1H").map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}
