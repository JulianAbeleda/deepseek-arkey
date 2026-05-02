use std::io::{self, IsTerminal, Write};
use std::iter::Peekable;
use std::time::Duration;

use crossterm::cursor::{MoveTo, MoveToColumn};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType};

pub enum InputAction {
    Submit(String),
    Exit,
}

pub struct InlineInput {
    history: Vec<String>,
    history_index: Option<usize>,
}

pub struct DockedComposer {
    prompt: String,
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    stream_buffer: String,
    stream_rendered_lines: Vec<String>,
    status_active: bool,
}

pub struct RawModeSession;

impl InlineInput {
    pub fn new() -> Self {
        Self {
            history: Vec::new(),
            history_index: None,
        }
    }

    pub fn read_action(&mut self, prompt: &str) -> Result<InputAction, String> {
        if !io::stdin().is_terminal() {
            print!("{prompt}");
            io::stdout().flush().map_err(|err| err.to_string())?;
            let mut line = String::new();
            let bytes = io::stdin()
                .read_line(&mut line)
                .map_err(|err| err.to_string())?;
            if bytes == 0 {
                return Ok(InputAction::Exit);
            }
            return Ok(InputAction::Submit(line));
        }
        let _raw_mode = RawModeGuard::enable()?;
        let mut buffer = String::new();
        let mut cursor = 0usize;
        self.history_index = None;
        render_line(prompt, &buffer, cursor)?;
        loop {
            let Event::Key(key) = event::read().map_err(|err| err.to_string())? else {
                continue;
            };
            match key.code {
                KeyCode::Enter => {
                    newline()?;
                    if !buffer.trim().is_empty() {
                        self.history.push(buffer.clone());
                    }
                    return Ok(InputAction::Submit(buffer));
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    newline()?;
                    return Ok(InputAction::Exit);
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    buffer.clear();
                    cursor = 0;
                    self.history_index = None;
                    newline()?;
                    render_line(prompt, &buffer, cursor)?;
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    insert_at(&mut buffer, &mut cursor, ch);
                    render_line(prompt, &buffer, cursor)?;
                }
                KeyCode::Backspace => {
                    if remove_before(&mut buffer, &mut cursor).is_some() {
                        render_line(prompt, &buffer, cursor)?;
                    }
                }
                KeyCode::Delete => {
                    if remove_at(&mut buffer, cursor).is_some() {
                        render_line(prompt, &buffer, cursor)?;
                    }
                }
                KeyCode::Left => {
                    cursor = cursor.saturating_sub(1);
                    render_line(prompt, &buffer, cursor)?;
                }
                KeyCode::Right => {
                    cursor = (cursor + 1).min(char_len(&buffer));
                    render_line(prompt, &buffer, cursor)?;
                }
                KeyCode::Home => {
                    cursor = 0;
                    render_line(prompt, &buffer, cursor)?;
                }
                KeyCode::End => {
                    cursor = char_len(&buffer);
                    render_line(prompt, &buffer, cursor)?;
                }
                KeyCode::Up => {
                    if let Some(line) = self.previous_history() {
                        buffer = line;
                        cursor = char_len(&buffer);
                        render_line(prompt, &buffer, cursor)?;
                    }
                }
                KeyCode::Down => {
                    if let Some(line) = self.next_history() {
                        buffer = line;
                        cursor = char_len(&buffer);
                        render_line(prompt, &buffer, cursor)?;
                    }
                }
                _ => {}
            }
        }
    }

    fn previous_history(&mut self) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }
        let next = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => self.history.len() - 1,
        };
        self.history_index = Some(next);
        self.history.get(next).cloned()
    }

    fn next_history(&mut self) -> Option<String> {
        let index = self.history_index?;
        if index + 1 >= self.history.len() {
            self.history_index = None;
            return Some(String::new());
        }
        let next = index + 1;
        self.history_index = Some(next);
        self.history.get(next).cloned()
    }
}

impl DockedComposer {
    pub fn new(prompt: String) -> Self {
        Self {
            prompt,
            buffer: String::new(),
            cursor: 0,
            history: Vec::new(),
            history_index: None,
            stream_buffer: String::new(),
            stream_rendered_lines: Vec::new(),
            status_active: false,
        }
    }

    pub fn set_prompt(&mut self, prompt: String) -> Result<(), String> {
        self.prompt = prompt;
        self.render()
    }

    pub fn render(&self) -> Result<(), String> {
        render_dock_line(&self.prompt, &self.buffer, self.cursor)
    }

    pub fn poll_action(&mut self, timeout: Duration) -> Result<Option<InputAction>, String> {
        if !event::poll(timeout).map_err(|err| err.to_string())? {
            return Ok(None);
        }
        let Event::Key(key) = event::read().map_err(|err| err.to_string())? else {
            return Ok(None);
        };
        match key.code {
            KeyCode::Enter => {
                let submitted = std::mem::take(&mut self.buffer);
                self.cursor = 0;
                self.history_index = None;
                if !submitted.trim().is_empty() {
                    self.history.push(submitted.clone());
                }
                self.print_above(&format!("{}{}\n", self.prompt, submitted))?;
                Ok(Some(InputAction::Submit(submitted)))
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.print_above("")?;
                Ok(Some(InputAction::Exit))
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.buffer.clear();
                self.cursor = 0;
                self.history_index = None;
                self.render()?;
                Ok(None)
            }
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                insert_at(&mut self.buffer, &mut self.cursor, ch);
                self.render()?;
                Ok(None)
            }
            KeyCode::Backspace => {
                if remove_before(&mut self.buffer, &mut self.cursor).is_some() {
                    self.render()?;
                }
                Ok(None)
            }
            KeyCode::Delete => {
                if remove_at(&mut self.buffer, self.cursor).is_some() {
                    self.render()?;
                }
                Ok(None)
            }
            KeyCode::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                self.render()?;
                Ok(None)
            }
            KeyCode::Right => {
                self.cursor = (self.cursor + 1).min(char_len(&self.buffer));
                self.render()?;
                Ok(None)
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.render()?;
                Ok(None)
            }
            KeyCode::End => {
                self.cursor = char_len(&self.buffer);
                self.render()?;
                Ok(None)
            }
            KeyCode::Up => {
                if let Some(line) = self.previous_history() {
                    self.buffer = line;
                    self.cursor = char_len(&self.buffer);
                    self.render()?;
                }
                Ok(None)
            }
            KeyCode::Down => {
                if let Some(line) = self.next_history() {
                    self.buffer = line;
                    self.cursor = char_len(&self.buffer);
                    self.render()?;
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    pub fn print_above(&mut self, text: &str) -> Result<(), String> {
        let had_status = self.take_status_active();
        let mut stdout = io::stdout();
        if had_status {
            clear_rows_above_dock(&mut stdout, 1)?;
        }
        move_to_output_row(&mut stdout)?;
        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
        write_raw_lines(&mut stdout, text)?;
        if !text.is_empty() && !text.ends_with('\n') {
            write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.render()
    }

    pub fn status_above(&mut self, text: &str) -> Result<(), String> {
        let had_status = self.take_status_active();
        let mut stdout = io::stdout();
        if had_status {
            clear_rows_above_dock(&mut stdout, 1)?;
        }
        move_to_status_row(&mut stdout)?;
        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
        write_raw_lines(&mut stdout, text)?;
        if !text.ends_with('\n') {
            write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.status_active = true;
        self.render()
    }

    pub fn stream_above(&mut self, text: &str) -> Result<(), String> {
        self.stream_buffer.push_str(text);
        let lines = wrap_visible_lines(&self.stream_buffer, terminal_width());
        let prior_rows = self.transient_rows();
        let mut stdout = io::stdout();
        if self.status_active && self.stream_rendered_lines.is_empty() {
            clear_rows_above_dock(&mut stdout, prior_rows)?;
            repaint_lines_above_dock(&mut stdout, &lines)?;
        } else {
            repaint_changed_lines_above_dock(&mut stdout, &self.stream_rendered_lines, &lines)?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.status_active = false;
        self.stream_rendered_lines = lines;
        self.render()
    }

    pub fn finish_stream(&mut self) -> Result<(), String> {
        if !self.stream_buffer.is_empty() {
            let text = self.stream_buffer.clone();
            let mut stdout = io::stdout();
            clear_rows_above_dock(&mut stdout, self.transient_rows())?;
            move_to_output_row(&mut stdout)?;
            execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
                .map_err(|err| err.to_string())?;
            write_raw_lines(&mut stdout, &text)?;
            if !text.ends_with('\n') {
                write!(stdout, "\r\n").map_err(|err| err.to_string())?;
            }
            stdout.flush().map_err(|err| err.to_string())?;
        }
        self.reset_stream_state();
        self.render()
    }

    fn reset_stream_state(&mut self) {
        self.stream_buffer.clear();
        self.stream_rendered_lines.clear();
        self.status_active = false;
    }

    fn take_status_active(&mut self) -> bool {
        let had_status = self.status_active;
        self.reset_stream_state();
        had_status
    }

    fn transient_rows(&self) -> usize {
        if !self.stream_rendered_lines.is_empty() {
            self.stream_rendered_lines.len()
        } else if self.status_active {
            1
        } else {
            0
        }
    }

    fn previous_history(&mut self) -> Option<String> {
        if self.history.is_empty() {
            return None;
        }
        let next = match self.history_index {
            Some(index) => index.saturating_sub(1),
            None => self.history.len() - 1,
        };
        self.history_index = Some(next);
        self.history.get(next).cloned()
    }

    fn next_history(&mut self) -> Option<String> {
        let index = self.history_index?;
        if index + 1 >= self.history.len() {
            self.history_index = None;
            return Some(String::new());
        }
        let next = index + 1;
        self.history_index = Some(next);
        self.history.get(next).cloned()
    }
}

impl RawModeSession {
    pub fn enable() -> Result<Self, String> {
        enable_raw_mode().map_err(|err| err.to_string())?;
        set_output_scroll_region(1)?;
        Ok(Self)
    }
}

impl Drop for RawModeSession {
    fn drop(&mut self) {
        let _ = reset_output_scroll_region();
        let _ = disable_raw_mode();
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self, String> {
        enable_raw_mode().map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn render_line(prompt: &str, buffer: &str, cursor: usize) -> Result<(), String> {
    let mut stdout = io::stdout();
    execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
        .map_err(|err| err.to_string())?;
    write!(stdout, "{prompt}{buffer}").map_err(|err| err.to_string())?;
    let cursor_col = visible_len(prompt) + cursor;
    execute!(stdout, MoveToColumn(cursor_col as u16)).map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

fn render_dock_line(prompt: &str, buffer: &str, cursor: usize) -> Result<(), String> {
    let width = terminal_width();
    let combined = format!("{prompt}{buffer}");
    let total_width = visible_len(&combined);
    let offset = total_width.saturating_sub(width);
    let visible_before_cursor = visible_len(prompt) + visible_len(&buffer_prefix(buffer, cursor));
    let cursor_col = visible_before_cursor
        .saturating_sub(offset)
        .min(width.saturating_sub(1));
    let display = visible_suffix(&combined, width);
    let mut stdout = io::stdout();
    execute!(stdout, MoveTo(0, dock_row()), Clear(ClearType::CurrentLine))
        .map_err(|err| err.to_string())?;
    write!(stdout, "{display}").map_err(|err| err.to_string())?;
    execute!(stdout, MoveTo(cursor_col as u16, dock_row())).map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

fn newline() -> Result<(), String> {
    let mut stdout = io::stdout();
    write!(stdout, "\r\n").map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

fn write_raw_lines(stdout: &mut io::Stdout, text: &str) -> Result<(), String> {
    for ch in text.chars() {
        if ch == '\n' {
            write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        } else {
            write!(stdout, "{ch}").map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn insert_at(buffer: &mut String, cursor: &mut usize, ch: char) {
    let byte_index = byte_index(buffer, *cursor);
    buffer.insert(byte_index, ch);
    *cursor += 1;
}

fn remove_before(buffer: &mut String, cursor: &mut usize) -> Option<char> {
    if *cursor == 0 {
        return None;
    }
    *cursor -= 1;
    remove_at(buffer, *cursor)
}

fn remove_at(buffer: &mut String, cursor: usize) -> Option<char> {
    if cursor >= char_len(buffer) {
        return None;
    }
    let start = byte_index(buffer, cursor);
    let ch = buffer[start..].chars().next()?;
    let end = start + ch.len_utf8();
    buffer.replace_range(start..end, "");
    Some(ch)
}

fn byte_index(buffer: &str, cursor: usize) -> usize {
    buffer
        .char_indices()
        .nth(cursor)
        .map(|(index, _)| index)
        .unwrap_or(buffer.len())
}

fn char_len(buffer: &str) -> usize {
    buffer.chars().count()
}

fn buffer_prefix(buffer: &str, cursor: usize) -> String {
    buffer.chars().take(cursor).collect()
}

fn visible_len(text: &str) -> usize {
    let mut len = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if take_ansi_sequence(ch, &mut chars).is_some() {
            continue;
        }
        len += char_display_width(ch);
    }
    len
}

fn terminal_width() -> usize {
    size().map(|(cols, _)| cols as usize).unwrap_or(80).max(1)
}

fn terminal_rows() -> u16 {
    size().map(|(_, rows)| rows).unwrap_or(24).max(1)
}

fn dock_row() -> u16 {
    terminal_rows().saturating_sub(1)
}

fn output_row() -> u16 {
    terminal_rows().saturating_sub(2)
}

fn move_to_output_row(stdout: &mut io::Stdout) -> Result<(), String> {
    execute!(stdout, MoveTo(0, output_row())).map_err(|err| err.to_string())
}

fn move_to_status_row(stdout: &mut io::Stdout) -> Result<(), String> {
    execute!(stdout, MoveTo(0, output_row())).map_err(|err| err.to_string())
}

fn clear_rows_above_dock(stdout: &mut io::Stdout, rows: usize) -> Result<(), String> {
    if rows == 0 {
        return Ok(());
    }
    let dock = dock_row();
    let rows = rows.min(dock as usize);
    let start = dock.saturating_sub(rows as u16);
    for row in start..dock {
        execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn repaint_lines_above_dock(stdout: &mut io::Stdout, lines: &[String]) -> Result<(), String> {
    let dock = dock_row();
    let max_rows = dock as usize;
    let visible_lines = if lines.len() > max_rows {
        &lines[lines.len() - max_rows..]
    } else {
        lines
    };
    let start = dock.saturating_sub(visible_lines.len() as u16);
    for (index, line) in visible_lines.iter().enumerate() {
        execute!(
            stdout,
            MoveTo(0, start + index as u16),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|err| err.to_string())?;
        write!(stdout, "{line}").map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn repaint_changed_lines_above_dock(
    stdout: &mut io::Stdout,
    previous: &[String],
    next: &[String],
) -> Result<(), String> {
    let dock = dock_row();
    let max_rows = dock as usize;
    let previous_lines = visible_tail(previous, max_rows);
    let next_lines = visible_tail(next, max_rows);
    let rows = previous_lines.len().max(next_lines.len());
    let start = dock.saturating_sub(rows as u16);
    for index in 0..rows {
        let old = previous_lines.get(index).map(String::as_str);
        let new = next_lines.get(index).map(String::as_str);
        if old == new {
            continue;
        }
        execute!(
            stdout,
            MoveTo(0, start + index as u16),
            Clear(ClearType::CurrentLine)
        )
        .map_err(|err| err.to_string())?;
        if let Some(line) = new {
            write!(stdout, "{line}").map_err(|err| err.to_string())?;
        }
    }
    Ok(())
}

fn visible_tail(lines: &[String], max_rows: usize) -> &[String] {
    if lines.len() > max_rows {
        &lines[lines.len() - max_rows..]
    } else {
        lines
    }
}

fn set_output_scroll_region(reserved_bottom_lines: usize) -> Result<(), String> {
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

fn reset_output_scroll_region() -> Result<(), String> {
    let mut stdout = io::stdout();
    write!(stdout, "\x1b[r").map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())
}

fn visible_suffix(text: &str, width: usize) -> String {
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

fn wrap_visible_lines(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    let mut col = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if let Some(sequence) = take_ansi_sequence(ch, &mut chars) {
            line.push_str(&sequence);
            continue;
        }
        if ch == '\n' {
            lines.push(std::mem::take(&mut line));
            col = 0;
            continue;
        }
        let width_of_char = char_display_width(ch);
        if col > 0 && col + width_of_char > width {
            lines.push(std::mem::take(&mut line));
            col = 0;
        }
        line.push(ch);
        col += width_of_char;
    }
    if !line.is_empty() || lines.is_empty() {
        lines.push(line);
    }
    lines
}

enum VisibleToken {
    Ansi(String),
    Char(char, usize),
}

fn take_ansi_sequence<I>(ch: char, chars: &mut Peekable<I>) -> Option<String>
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

fn char_display_width(ch: char) -> usize {
    if ch.is_control() {
        0
    } else if is_wide_char(ch) {
        2
    } else {
        1
    }
}

fn is_wide_char(ch: char) -> bool {
    matches!(
        ch as u32,
        0x1100..=0x115f
            | 0x2329..=0x232a
            | 0x2e80..=0xa4cf
            | 0xac00..=0xd7a3
            | 0xf900..=0xfaff
            | 0xfe10..=0xfe19
            | 0xfe30..=0xfe6f
            | 0xff00..=0xff60
            | 0xffe0..=0xffe6
            | 0x1f300..=0x1faff
    )
}

#[cfg(test)]
mod tests {
    use super::{
        buffer_prefix, insert_at, remove_at, remove_before, visible_len, visible_suffix,
        wrap_visible_lines, DockedComposer,
    };

    #[test]
    fn edits_at_cursor() {
        let mut text = "hi".to_string();
        let mut cursor = 1;
        insert_at(&mut text, &mut cursor, 'e');
        assert_eq!(text, "hei");
        assert_eq!(cursor, 2);
        assert_eq!(remove_before(&mut text, &mut cursor), Some('e'));
        assert_eq!(text, "hi");
        assert_eq!(cursor, 1);
        assert_eq!(remove_at(&mut text, cursor), Some('i'));
        assert_eq!(text, "h");
    }

    #[test]
    fn visible_len_ignores_ansi() {
        assert_eq!(visible_len("\x1b[36mdeepseek\x1b[0m › "), 11);
    }

    #[test]
    fn dock_display_uses_visible_suffix_for_long_lines() {
        assert_eq!(visible_suffix("abcdef", 4), "cdef");
        assert_eq!(visible_suffix("ab界", 3), "b界");
        assert_eq!(buffer_prefix("hello", 3), "hel");
    }

    #[test]
    fn dock_display_keeps_ansi_sequences_intact_at_narrow_widths() {
        let text =
            "\x1b[36;1mdeepseek\x1b[0m \x1b[38;2;122;162;247m[deepseek-v4-flash]\x1b[0m › draft";
        let suffix = visible_suffix(text, 10);

        assert_eq!(suffix, "\x1b[38;2;122;162;247mh]\x1b[0m › draft");
        assert_eq!(visible_len(&suffix), 10);
    }

    #[test]
    fn dock_display_preserves_active_ansi_style_inside_suffix() {
        let suffix = visible_suffix("\x1b[31mabcdef\x1b[0m", 4);

        assert_eq!(suffix, "\x1b[31mcdef\x1b[0m");
        assert_eq!(visible_len(&suffix), 4);
    }

    #[test]
    fn dock_display_does_not_treat_rgb_zero_as_ansi_reset() {
        let suffix = visible_suffix("\x1b[38;2;0;162;0mabcdef\x1b[0m", 4);

        assert_eq!(suffix, "\x1b[38;2;0;162;0mcdef\x1b[0m");
        assert_eq!(visible_len(&suffix), 4);
    }

    #[test]
    fn composer_stream_state_can_reset() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.buffer = "draft".to_string();
        composer.stream_buffer = "hello".to_string();
        composer.stream_rendered_lines = vec!["hello".to_string()];
        assert_eq!(composer.stream_buffer, "hello");
        assert_eq!(composer.stream_rendered_lines, vec!["hello"]);
        assert!(!composer.status_active);
        composer.status_active = true;
        composer.reset_stream_state();
        assert!(composer.stream_buffer.is_empty());
        assert!(composer.stream_rendered_lines.is_empty());
        assert!(!composer.status_active);
        assert_eq!(composer.buffer, "draft");
    }

    #[test]
    fn composer_status_state_is_consumed_before_rewrite() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.status_active = true;
        assert!(composer.take_status_active());
        assert!(!composer.status_active);
        assert!(!composer.take_status_active());
    }

    #[test]
    fn wraps_stream_lines_to_terminal_width() {
        assert_eq!(wrap_visible_lines("abcdef", 3), vec!["abc", "def"]);
        assert_eq!(wrap_visible_lines("ab\ncd", 10), vec!["ab", "cd"]);
        assert_eq!(wrap_visible_lines("ab界", 3), vec!["ab", "界"]);
        assert_eq!(
            wrap_visible_lines("\x1b[31mred\x1b[0m!", 4),
            vec!["\x1b[31mred\x1b[0m!"]
        );
    }
}
