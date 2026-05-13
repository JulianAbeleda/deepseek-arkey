use crate::terminal_width::{char_display_width, display_width};

use super::ansi::take_ansi_sequence;
use super::edit::byte_index;

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
