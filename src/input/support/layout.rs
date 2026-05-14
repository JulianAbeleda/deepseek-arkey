use std::io::{self, Write};

use crossterm::terminal::size;

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
