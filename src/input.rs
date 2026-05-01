use std::io::{self, IsTerminal, Write};
use std::time::Duration;

use crossterm::cursor::MoveToColumn;
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, Clear, ClearType};

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
    stream_active: bool,
    stream_col: usize,
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
            stream_active: false,
            stream_col: 0,
        }
    }

    pub fn set_prompt(&mut self, prompt: String) -> Result<(), String> {
        self.prompt = prompt;
        self.render()
    }

    pub fn render(&self) -> Result<(), String> {
        render_line(&self.prompt, &self.buffer, self.cursor)
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
        self.reset_stream_state();
        let mut stdout = io::stdout();
        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
        write_raw_lines(&mut stdout, text)?;
        if !text.is_empty() && !text.ends_with('\n') {
            write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.render()
    }

    pub fn stream_above(&mut self, text: &str) -> Result<(), String> {
        let mut stdout = io::stdout();
        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
        if self.stream_active {
            execute!(
                stdout,
                crossterm::cursor::MoveUp(1),
                MoveToColumn(self.stream_col as u16)
            )
            .map_err(|err| err.to_string())?;
        }
        for ch in text.chars() {
            if ch == '\n' {
                write!(stdout, "\r\n").map_err(|err| err.to_string())?;
            } else {
                write!(stdout, "{ch}").map_err(|err| err.to_string())?;
            }
        }
        self.note_stream_text(text);
        write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        stdout.flush().map_err(|err| err.to_string())?;
        self.render()
    }

    pub fn finish_stream(&mut self) -> Result<(), String> {
        if !self.stream_active {
            return self.render();
        }
        let mut stdout = io::stdout();
        execute!(
            stdout,
            MoveToColumn(0),
            Clear(ClearType::CurrentLine),
            crossterm::cursor::MoveUp(1),
            MoveToColumn(self.stream_col as u16)
        )
        .map_err(|err| err.to_string())?;
        write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        stdout.flush().map_err(|err| err.to_string())?;
        self.reset_stream_state();
        self.render()
    }

    fn note_stream_text(&mut self, text: &str) {
        self.stream_col = stream_column_after(self.stream_col, text);
        self.stream_active = true;
    }

    fn reset_stream_state(&mut self) {
        self.stream_active = false;
        self.stream_col = 0;
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
        Ok(Self)
    }
}

impl Drop for RawModeSession {
    fn drop(&mut self) {
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

fn visible_len(text: &str) -> usize {
    let mut len = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for code in chars.by_ref() {
                if code == 'm' {
                    break;
                }
            }
        } else {
            len += 1;
        }
    }
    len
}

fn stream_column_after(start_col: usize, text: &str) -> usize {
    let mut col = start_col;
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' && chars.peek() == Some(&'[') {
            let _ = chars.next();
            for code in chars.by_ref() {
                if ('@'..='~').contains(&code) {
                    break;
                }
            }
        } else if ch == '\n' || ch == '\r' {
            col = 0;
        } else {
            col += char_display_width(ch);
        }
    }
    col
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
        insert_at, remove_at, remove_before, stream_column_after, visible_len, DockedComposer,
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
    fn stream_column_tracks_newlines_ansi_and_wide_chars() {
        assert_eq!(stream_column_after(0, "ab"), 2);
        assert_eq!(stream_column_after(2, "界"), 4);
        assert_eq!(stream_column_after(4, "\n界"), 2);
        assert_eq!(stream_column_after(2, "\x1b[31mred\x1b[0m"), 5);
    }

    #[test]
    fn composer_stream_state_can_reset() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.note_stream_text("hello");
        assert!(composer.stream_active);
        assert_eq!(composer.stream_col, 5);
        composer.note_stream_text(" 界");
        assert_eq!(composer.stream_col, 8);
        composer.reset_stream_state();
        assert!(!composer.stream_active);
        assert_eq!(composer.stream_col, 0);
    }
}
