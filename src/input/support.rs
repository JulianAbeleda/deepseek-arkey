use std::io::{self, Write};
use std::iter::Peekable;

use crossterm::cursor::{MoveTo, MoveToColumn, Show};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyEvent, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType};

use crate::terminal_width::{
    char_display_width, display_width, pad_display_width, wrap_plain_text,
};

use super::composer::{
    RawModeSession, DOCK_HELP_TEXT, DOCK_RESERVED_ROWS, DOCK_VERTICAL_PADDING_ROWS,
};

impl RawModeSession {
    pub fn enable() -> Result<Self, String> {
        enable_raw_mode().map_err(|err| err.to_string())?;
        execute!(
            io::stdout(),
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )
        .map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for RawModeSession {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show);
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        let _ = reset_output_scroll_region();
        let _ = disable_raw_mode();
    }
}

pub(crate) struct RawModeGuard;

impl RawModeGuard {
    pub(crate) fn enable() -> Result<Self, String> {
        enable_raw_mode().map_err(|err| err.to_string())?;
        execute!(
            io::stdout(),
            EnableBracketedPaste,
            PushKeyboardEnhancementFlags(keyboard_enhancement_flags())
        )
        .map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show);
        let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        let _ = disable_raw_mode();
    }
}

pub(crate) fn keyboard_enhancement_flags() -> KeyboardEnhancementFlags {
    KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
        | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
        | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
}

pub(crate) fn is_key_press_or_repeat(key: KeyEvent) -> bool {
    matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
}

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

pub(crate) fn insert_at(buffer: &mut String, cursor: &mut usize, ch: char) {
    let byte_index = byte_index(buffer, *cursor);
    buffer.insert(byte_index, ch);
    *cursor += 1;
}

pub(crate) fn insert_str_at(buffer: &mut String, cursor: &mut usize, text: &str) {
    let byte_index = byte_index(buffer, *cursor);
    buffer.insert_str(byte_index, text);
    *cursor += char_len(text);
}

pub(crate) fn remove_before(buffer: &mut String, cursor: &mut usize) -> Option<char> {
    if *cursor == 0 {
        return None;
    }
    *cursor -= 1;
    remove_at(buffer, *cursor)
}

pub(crate) fn remove_at(buffer: &mut String, cursor: usize) -> Option<char> {
    if cursor >= char_len(buffer) {
        return None;
    }
    let start = byte_index(buffer, cursor);
    let ch = buffer[start..].chars().next()?;
    let end = start + ch.len_utf8();
    buffer.replace_range(start..end, "");
    Some(ch)
}

pub(crate) fn byte_index(buffer: &str, cursor: usize) -> usize {
    buffer
        .char_indices()
        .nth(cursor)
        .map(|(index, _)| index)
        .unwrap_or(buffer.len())
}

pub(crate) fn char_len(buffer: &str) -> usize {
    buffer.chars().count()
}

#[cfg(test)]
pub(crate) fn buffer_prefix(buffer: &str, cursor: usize) -> String {
    buffer.chars().take(cursor).collect()
}

pub(crate) fn previous_word_cursor(buffer: &str, cursor: usize) -> usize {
    let chars = buffer.chars().collect::<Vec<_>>();
    let mut index = cursor.min(chars.len());
    while index > 0 && chars[index - 1].is_whitespace() {
        index -= 1;
    }
    while index > 0 && !chars[index - 1].is_whitespace() {
        index -= 1;
    }
    index
}

pub(crate) fn next_word_cursor(buffer: &str, cursor: usize) -> usize {
    let chars = buffer.chars().collect::<Vec<_>>();
    let mut index = cursor.min(chars.len());
    while index < chars.len() && !chars[index].is_whitespace() {
        index += 1;
    }
    while index < chars.len() && chars[index].is_whitespace() {
        index += 1;
    }
    index
}

pub(crate) fn remove_previous_word(buffer: &mut String, cursor: &mut usize) -> bool {
    let end = *cursor;
    let start = previous_word_cursor(buffer, end);
    if start == end {
        return false;
    }
    let start_byte = byte_index(buffer, start);
    let end_byte = byte_index(buffer, end);
    buffer.replace_range(start_byte..end_byte, "");
    *cursor = start;
    true
}

pub(crate) struct ComposedDockRows {
    pub(crate) lines: Vec<String>,
    pub(crate) cursor_row: usize,
    pub(crate) cursor_col: usize,
}

pub(crate) fn compose_dock_rows(
    prompt: &str,
    buffer: &str,
    cursor: usize,
    width: usize,
) -> ComposedDockRows {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut row = 0usize;
    let mut col = 0usize;
    let mut cursor_row = 0usize;
    let mut cursor_col = 0usize;
    let cursor_byte = byte_index(buffer, cursor);

    append_visible_text(prompt, width, &mut lines, &mut row, &mut col);
    if cursor == 0 {
        cursor_row = row;
        cursor_col = col;
    }

    for (byte_index, ch) in buffer.char_indices() {
        if byte_index == cursor_byte {
            cursor_row = row;
            cursor_col = col;
        }
        append_visible_char(ch, width, &mut lines, &mut row, &mut col);
    }
    if cursor_byte == buffer.len() {
        cursor_row = row;
        cursor_col = col;
    }
    ComposedDockRows {
        lines,
        cursor_row,
        cursor_col,
    }
}

fn append_visible_text(
    text: &str,
    width: usize,
    lines: &mut Vec<String>,
    row: &mut usize,
    col: &mut usize,
) {
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(sequence) = take_ansi_sequence(ch, &mut chars) {
            lines[*row].push_str(&sequence);
            continue;
        }
        append_visible_char(ch, width, lines, row, col);
    }
}

fn append_visible_char(
    ch: char,
    width: usize,
    lines: &mut Vec<String>,
    row: &mut usize,
    col: &mut usize,
) {
    if ch == '\n' {
        lines.push(String::new());
        *row += 1;
        *col = 0;
        return;
    }
    let char_width = char_display_width(ch);
    if char_width == 0 {
        return;
    }
    if col.saturating_add(char_width) > width {
        lines.push(String::new());
        *row += 1;
        *col = 0;
    }
    lines[*row].push(ch);
    *col = col.saturating_add(char_width);
}

pub(crate) fn visible_len(text: &str) -> usize {
    display_width(text)
}

pub(crate) fn truncate_display_text(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let ellipsis = "...";
    if visible_len(text) <= width {
        return text.to_string();
    }
    if width <= visible_len(ellipsis) {
        return ".".repeat(width);
    }
    let available = width - visible_len(ellipsis);
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let ch_width = char_display_width(ch);
        if used + ch_width > available {
            break;
        }
        out.push(ch);
        used += ch_width;
    }
    out.push_str(ellipsis);
    out
}

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

pub(crate) fn visible_suffix(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let mut tokens = Vec::new();
    let mut visible = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(sequence) = take_ansi_sequence(ch, &mut chars) {
            tokens.push(VisibleToken::Ansi(sequence));
            continue;
        }
        let width = char_display_width(ch);
        visible += width;
        tokens.push(VisibleToken::Char(ch, width));
    }

    if visible <= width {
        return text.to_string();
    }

    let mut suffix_width = 0usize;
    let mut start = None;
    for (index, token) in tokens.iter().enumerate().rev() {
        if let VisibleToken::Char(_, char_width) = token {
            if suffix_width + char_width > width {
                break;
            }
            suffix_width += char_width;
            start = Some(index);
        }
    }

    let Some(start) = start else {
        return String::new();
    };

    let mut out = String::new();
    if let Some(sequence) = active_sgr_before(&tokens, start) {
        out.push_str(&sequence);
    }
    for token in &tokens[start..] {
        match token {
            VisibleToken::Ansi(sequence) => out.push_str(sequence),
            VisibleToken::Char(ch, _) => out.push(*ch),
        }
    }
    out
}

enum VisibleToken {
    Ansi(String),
    Char(char, usize),
}

pub(crate) fn take_ansi_sequence<I>(ch: char, chars: &mut Peekable<I>) -> Option<String>
where
    I: Iterator<Item = char>,
{
    if ch != '\x1b' || chars.peek() != Some(&'[') {
        return None;
    }

    let mut sequence = String::from(ch);
    sequence.push(chars.next()?);
    for code in chars.by_ref() {
        sequence.push(code);
        if ('@'..='~').contains(&code) {
            break;
        }
    }
    Some(sequence)
}

fn active_sgr_before(tokens: &[VisibleToken], end: usize) -> Option<String> {
    let mut active = None;
    for token in &tokens[..end] {
        let VisibleToken::Ansi(sequence) = token else {
            continue;
        };
        if !sequence.ends_with('m') {
            continue;
        }
        if is_sgr_reset(sequence) {
            active = None;
        } else {
            active = Some(sequence.clone());
        }
    }
    active
}

fn is_sgr_reset(sequence: &str) -> bool {
    let Some(codes) = sequence
        .strip_prefix("\x1b[")
        .and_then(|rest| rest.strip_suffix('m'))
    else {
        return false;
    };
    if codes.is_empty() {
        return true;
    }

    let codes = codes.split(';').collect::<Vec<_>>();
    let mut index = 0;
    while index < codes.len() {
        match codes[index] {
            "" | "0" => return true,
            "38" | "48" | "58" if codes.get(index + 1) == Some(&"2") => index += 5,
            "38" | "48" | "58" if codes.get(index + 1) == Some(&"5") => index += 3,
            _ => index += 1,
        }
    }
    false
}
