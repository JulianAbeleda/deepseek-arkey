use std::io::{self, Write};

use crossterm::cursor::{MoveTo, MoveToColumn};
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

use crate::terminal_width::{pad_display_width, wrap_plain_text};

use crate::input::composer::{DOCK_HELP_TEXT, DOCK_RESERVED_ROWS, DOCK_VERTICAL_PADDING_ROWS};

use super::layout::{clear_dock_rows, dock_row, set_output_scroll_region, terminal_width};
use super::visible::{compose_dock_rows, truncate_display_text, visible_len, ComposedDockRows};

pub(crate) fn render_line(prompt: &str, buffer: &str, cursor: usize) -> Result<(), String> {
    let mut stdout = io::stdout();
    execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
        .map_err(|err| err.to_string())?;
    write!(stdout, "{prompt}{buffer}").map_err(|err| err.to_string())?;
    let cursor_col = visible_len(prompt) + cursor;
    execute!(stdout, MoveToColumn(cursor_col as u16)).map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

pub(crate) fn render_dock_lines(
    prompt: &str,
    buffer: &str,
    cursor: usize,
    panel_rows: &[String],
    hide_input: bool,
    previous_rows: usize,
) -> Result<usize, String> {
    let rows = compose_rendered_dock_rows(
        prompt,
        buffer,
        cursor,
        terminal_width(),
        panel_rows,
        hide_input,
    );
    let input_capacity = DOCK_RESERVED_ROWS.saturating_sub(DOCK_VERTICAL_PADDING_ROWS);
    let visible_rows = rows
        .lines
        .len()
        .saturating_sub(input_capacity)
        .min(rows.lines.len());
    let display_lines = if rows.lines.len() > DOCK_RESERVED_ROWS {
        &rows.lines[rows.lines.len() - DOCK_RESERVED_ROWS..]
    } else {
        &rows.lines[..]
    };
    let first_row = dock_row().saturating_sub(display_lines.len().saturating_sub(1) as u16);
    let cursor_row = rows
        .cursor_row
        .saturating_sub(visible_rows)
        .min(display_lines.len().saturating_sub(1));
    let cursor_col = rows.cursor_col.min(terminal_width().saturating_sub(1));
    let mut stdout = io::stdout();
    set_output_scroll_region(display_lines.len())?;
    clear_dock_rows(&mut stdout, display_lines.len().max(previous_rows))?;
    for (index, line) in display_lines.iter().enumerate() {
        execute!(
            stdout,
            MoveTo(0, first_row + index as u16),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|err| err.to_string())?;
        write!(stdout, "{line}").map_err(|err| err.to_string())?;
    }
    execute!(
        stdout,
        MoveTo(cursor_col as u16, first_row + cursor_row as u16)
    )
    .map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())?;
    Ok(display_lines.len().min(DOCK_RESERVED_ROWS))
}

pub(crate) fn compose_rendered_dock_rows(
    prompt: &str,
    buffer: &str,
    cursor: usize,
    width: usize,
    panel_rows: &[String],
    hide_input: bool,
) -> ComposedDockRows {
    if hide_input {
        let mut lines = panel_rows
            .iter()
            .take(DOCK_RESERVED_ROWS)
            .cloned()
            .collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(String::new());
        }
        return ComposedDockRows {
            lines,
            cursor_row: 0,
            cursor_col: 0,
        };
    }
    let input_rows = compose_dock_rows(prompt, buffer, cursor, width);
    let mut lines = vec![String::new()];
    let available_panel_rows =
        DOCK_RESERVED_ROWS.saturating_sub(lines.len() + input_rows.lines.len() + 1);
    for row in panel_rows.iter().take(available_panel_rows) {
        lines.push(row.clone());
    }
    let input_start_row = lines.len();
    lines.extend(input_rows.lines);
    lines.push(muted_dock_help(DOCK_HELP_TEXT));
    ComposedDockRows {
        lines,
        cursor_row: input_start_row + input_rows.cursor_row,
        cursor_col: input_rows.cursor_col,
    }
}

pub(crate) fn muted_dock_help(text: &str) -> String {
    style_prompt_echo("90", text)
}

pub(crate) fn progress_panel_rows(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    text.lines()
        .take(DOCK_RESERVED_ROWS.saturating_sub(DOCK_VERTICAL_PADDING_ROWS))
        .map(|line| {
            muted_dock_help(&pad_display_width(
                &truncate_display_text(line, width),
                width,
            ))
        })
        .collect()
}

pub(crate) fn newline() -> Result<(), String> {
    let mut stdout = io::stdout();
    write!(stdout, "\r\n").map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

pub(crate) fn write_raw_lines(stdout: &mut io::Stdout, text: &str) -> Result<(), String> {
    for ch in text.chars() {
        if ch == '\n' {
            write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        } else {
            write!(stdout, "{ch}").map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

pub(crate) fn submitted_prompt_echo(submitted: &str) -> String {
    submitted_prompt_echo_with_options(
        submitted,
        terminal_width(),
        std::env::var_os("NO_COLOR").is_none(),
    )
}

pub(crate) fn submitted_prompt_echo_with_options(
    submitted: &str,
    terminal_width: usize,
    color_enabled: bool,
) -> String {
    let mut output = String::from("\n");
    let mut block_lines = Vec::new();
    let width = terminal_width.max(24);

    for line in submitted.split('\n') {
        if !block_lines.is_empty() {
            block_lines.push(prompt_echo_plain_blank(width));
        }
        block_lines.extend(prompt_echo_block_lines(line, width, color_enabled));
    }

    output.push_str(&block_lines.join("\n"));
    output.push_str("\n\n");
    output
}

pub(crate) fn prompt_echo_block_lines(
    text: &str,
    width: usize,
    color_enabled: bool,
) -> Vec<String> {
    let marker_width = 2usize;
    let content_width = width.saturating_sub(marker_width + 2).max(10);
    let wrapped = wrap_plain_text(text, content_width);
    let mut lines = Vec::with_capacity(wrapped.len().max(1) + 2);

    lines.push(prompt_echo_plain_blank(width));
    for (index, line) in wrapped.into_iter().enumerate() {
        let marker = if index == 0 { "> " } else { "  " };
        let content = pad_display_width(&format!(" {line} "), width.saturating_sub(marker_width));
        lines.push(format!(
            "{}{}",
            prompt_echo_marker(marker, color_enabled),
            prompt_echo_block(&content, color_enabled)
        ));
    }
    lines.push(prompt_echo_plain_blank(width));
    lines
}

pub(crate) fn prompt_echo_plain_blank(width: usize) -> String {
    " ".repeat(width)
}

pub(crate) fn prompt_echo_marker(text: &str, color_enabled: bool) -> String {
    style_prompt_echo_with_color("1;38;2;187;154;247;48;2;40;42;54", text, color_enabled)
}

pub(crate) fn prompt_echo_block(text: &str, color_enabled: bool) -> String {
    style_prompt_echo_with_color("38;2;220;223;230;48;2;40;42;54", text, color_enabled)
}

pub(crate) fn style_prompt_echo(code: &str, text: impl AsRef<str>) -> String {
    style_prompt_echo_with_color(code, text, std::env::var_os("NO_COLOR").is_none())
}

pub(crate) fn style_prompt_echo_with_color(
    code: &str,
    text: impl AsRef<str>,
    color_enabled: bool,
) -> String {
    let text = text.as_ref();
    if color_enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}
