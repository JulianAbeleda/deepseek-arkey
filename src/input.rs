use std::io::{self, IsTerminal, Write};
use std::iter::Peekable;
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, MoveToColumn, Show};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, size, Clear, ClearType};

use crate::terminal_width::{
    char_display_width, display_width, pad_display_width, wrap_plain_text,
};

pub enum InputAction {
    Submit(String),
    Exit,
}

const DOCK_RESERVED_ROWS: usize = 7;
const DOCK_VERTICAL_PADDING_ROWS: usize = 2;
const DOCK_HELP_TEXT: &str = "Enter send · ? help · /model · /debug · /runtime · /end · /exit";
const SLASH_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec::new("/chat", "Switch to plain chat mode"),
    SlashCommandSpec::new("/agent", "Switch to workspace agent mode"),
    SlashCommandSpec::new("/root", "Show or set active workspace root"),
    SlashCommandSpec::new("/model", "Show or switch DeepSeek model"),
    SlashCommandSpec::new("/debug", "Toggle local debug backend"),
    SlashCommandSpec::new("/runtime", "Show provider/debug runtime state"),
    SlashCommandSpec::new("/status", "Show active session details"),
    SlashCommandSpec::new("/end", "End the current session and clear context"),
    SlashCommandSpec::new("/exit", "Exit without clearing context"),
    SlashCommandSpec::new("/quit", "Exit without clearing context"),
    SlashCommandSpec::new("/help", "Show this help"),
    SlashCommandSpec::new("?", "Show this help"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SlashCommandSpec {
    command: &'static str,
    description: &'static str,
}

impl SlashCommandSpec {
    const fn new(command: &'static str, description: &'static str) -> Self {
        Self {
            command,
            description,
        }
    }
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
    slash_completion_index: Option<usize>,
    slash_completion_prefix: Option<String>,
    stream_buffer: String,
    status_active: bool,
    transcript_lines: Vec<String>,
    transcript_view_offset: usize,
    transcript_start_row: Option<u16>,
    transcript_cursor_row: Option<u16>,
    transcript_cursor_column: usize,
    rendered_dock_rows: usize,
    cursor_hidden: bool,
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
            match event::read().map_err(|err| err.to_string())? {
                Event::Paste(text) => {
                    insert_str_at(&mut buffer, &mut cursor, &text);
                    render_line(prompt, &buffer, cursor)?;
                    continue;
                }
                Event::Key(key) => match key.code {
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
                        cursor = if key.modifiers.contains(KeyModifiers::CONTROL)
                            || key.modifiers.contains(KeyModifiers::ALT)
                        {
                            previous_word_cursor(&buffer, cursor)
                        } else {
                            cursor.saturating_sub(1)
                        };
                        render_line(prompt, &buffer, cursor)?;
                    }
                    KeyCode::Right => {
                        cursor = if key.modifiers.contains(KeyModifiers::CONTROL)
                            || key.modifiers.contains(KeyModifiers::ALT)
                        {
                            next_word_cursor(&buffer, cursor)
                        } else {
                            (cursor + 1).min(char_len(&buffer))
                        };
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
                },
                _ => continue,
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
            slash_completion_index: None,
            slash_completion_prefix: None,
            stream_buffer: String::new(),
            status_active: false,
            transcript_lines: Vec::new(),
            transcript_view_offset: 0,
            transcript_start_row: None,
            transcript_cursor_row: None,
            transcript_cursor_column: 0,
            rendered_dock_rows: 0,
            cursor_hidden: false,
        }
    }

    pub fn set_transcript_start_row(&mut self, row: Option<u16>) {
        self.transcript_start_row = row;
        self.transcript_cursor_row = row;
        self.transcript_cursor_column = 0;
    }

    pub fn set_prompt(&mut self, prompt: String) -> Result<(), String> {
        self.prompt = prompt;
        self.render()
    }

    pub fn render(&mut self) -> Result<(), String> {
        self.rendered_dock_rows = render_dock_lines(
            &self.prompt,
            &self.buffer,
            self.cursor,
            self.slash_completion_footer().as_deref(),
            self.rendered_dock_rows,
        )?;
        Ok(())
    }

    pub fn hide_cursor(&mut self) -> Result<(), String> {
        if !self.cursor_hidden {
            execute!(io::stdout(), Hide).map_err(|err| err.to_string())?;
            self.cursor_hidden = true;
        }
        Ok(())
    }

    pub fn show_cursor(&mut self) -> Result<(), String> {
        if self.cursor_hidden {
            execute!(io::stdout(), Show).map_err(|err| err.to_string())?;
            self.cursor_hidden = false;
        }
        Ok(())
    }

    fn render_preserving_cursor(&mut self) -> Result<(), String> {
        let row = self.transcript_row();
        let column = self
            .transcript_cursor_column
            .min(terminal_width().saturating_sub(1)) as u16;
        self.rendered_dock_rows = render_dock_lines(
            &self.prompt,
            &self.buffer,
            self.cursor,
            self.slash_completion_footer().as_deref(),
            self.rendered_dock_rows,
        )?;
        execute!(io::stdout(), MoveTo(column, row)).map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn poll_action(&mut self, timeout: Duration) -> Result<Option<InputAction>, String> {
        if !event::poll(timeout).map_err(|err| err.to_string())? {
            return Ok(None);
        }
        match event::read().map_err(|err| err.to_string())? {
            Event::Paste(text) => {
                self.insert_text(&text)?;
                Ok(None)
            }
            Event::Key(key) => match key.code {
                KeyCode::Enter => {
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        || key.modifiers.contains(KeyModifiers::ALT)
                    {
                        self.insert_text("\n")?;
                        return Ok(None);
                    }
                    let submitted = std::mem::take(&mut self.buffer);
                    self.cursor = 0;
                    self.history_index = None;
                    self.reset_slash_completion();
                    if !submitted.trim().is_empty() {
                        self.history.push(submitted.clone());
                    }
                    self.print_above(&submitted_prompt_echo(&submitted))?;
                    Ok(Some(InputAction::Submit(submitted)))
                }
                KeyCode::Tab => {
                    self.complete_slash_command()?;
                    Ok(None)
                }
                KeyCode::PageUp => {
                    self.scroll_transcript(1)?;
                    Ok(None)
                }
                KeyCode::PageDown => {
                    self.scroll_transcript_down(1)?;
                    Ok(None)
                }
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.print_above("")?;
                    Ok(Some(InputAction::Exit))
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if remove_previous_word(&mut self.buffer, &mut self.cursor) {
                        self.history_index = None;
                        self.reset_slash_completion();
                        self.render()?;
                    }
                    Ok(None)
                }
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.buffer.clear();
                    self.cursor = 0;
                    self.history_index = None;
                    self.reset_slash_completion();
                    self.render()?;
                    Ok(None)
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    insert_at(&mut self.buffer, &mut self.cursor, ch);
                    self.history_index = None;
                    self.reset_slash_completion();
                    self.render()?;
                    Ok(None)
                }
                KeyCode::Backspace => {
                    if key.modifiers.contains(KeyModifiers::ALT)
                        || key.modifiers.contains(KeyModifiers::CONTROL)
                    {
                        if remove_previous_word(&mut self.buffer, &mut self.cursor) {
                            self.history_index = None;
                            self.reset_slash_completion();
                            self.render()?;
                        }
                    } else if remove_before(&mut self.buffer, &mut self.cursor).is_some() {
                        self.history_index = None;
                        self.reset_slash_completion();
                        self.render()?;
                    }
                    Ok(None)
                }
                KeyCode::Delete => {
                    if remove_at(&mut self.buffer, self.cursor).is_some() {
                        self.history_index = None;
                        self.reset_slash_completion();
                        self.render()?;
                    }
                    Ok(None)
                }
                KeyCode::Left => {
                    self.cursor = if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::ALT)
                    {
                        previous_word_cursor(&self.buffer, self.cursor)
                    } else {
                        self.cursor.saturating_sub(1)
                    };
                    self.reset_slash_completion();
                    self.render()?;
                    Ok(None)
                }
                KeyCode::Right => {
                    self.cursor = if key.modifiers.contains(KeyModifiers::CONTROL)
                        || key.modifiers.contains(KeyModifiers::ALT)
                    {
                        next_word_cursor(&self.buffer, self.cursor)
                    } else {
                        (self.cursor + 1).min(char_len(&self.buffer))
                    };
                    self.reset_slash_completion();
                    self.render()?;
                    Ok(None)
                }
                KeyCode::Home => {
                    self.cursor = 0;
                    self.reset_slash_completion();
                    self.render()?;
                    Ok(None)
                }
                KeyCode::End => {
                    self.cursor = char_len(&self.buffer);
                    self.reset_slash_completion();
                    self.render()?;
                    Ok(None)
                }
                KeyCode::Up => {
                    if let Some(line) = self.previous_history() {
                        self.buffer = line;
                        self.cursor = char_len(&self.buffer);
                        self.reset_slash_completion();
                        self.render()?;
                    }
                    Ok(None)
                }
                KeyCode::Down => {
                    if let Some(line) = self.next_history() {
                        self.buffer = line;
                        self.cursor = char_len(&self.buffer);
                        self.reset_slash_completion();
                        self.render()?;
                    }
                    Ok(None)
                }
                _ => Ok(None),
            },
            _ => Ok(None),
        }
    }

    pub fn print_above(&mut self, text: &str) -> Result<(), String> {
        self.snap_transcript_to_bottom();
        let had_status = self.take_status_active();
        let mut stdout = io::stdout();
        if had_status {
            clear_rows_above_dock(&mut stdout, self.active_dock_rows(), 1)?;
        }
        self.move_to_transcript_cursor(&mut stdout)?;
        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
        write_raw_lines(&mut stdout, text)?;
        if !text.is_empty() && !text.ends_with('\n') {
            write!(stdout, "\r\n").map_err(|err| err.to_string())?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.advance_transcript_text(text);
        if !text.is_empty() && !text.ends_with('\n') {
            self.advance_transcript_text("\n");
        }
        self.record_transcript_text(text);
        if !text.is_empty() && !text.ends_with('\n') {
            self.record_transcript_text("\n");
        }
        self.render()
    }

    pub fn status_above(&mut self, text: &str) -> Result<(), String> {
        let had_status = self.take_status_active();
        let mut stdout = io::stdout();
        if had_status {
            clear_rows_above_dock(&mut stdout, self.active_dock_rows(), 1)?;
        }
        self.move_to_transcript_cursor(&mut stdout)?;
        execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))
            .map_err(|err| err.to_string())?;
        if let Some(line) = text.lines().next() {
            write!(stdout, "{line}").map_err(|err| err.to_string())?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.status_active = true;
        self.render_preserving_cursor()
    }

    pub fn stream_above(&mut self, text: &str) -> Result<(), String> {
        self.snap_transcript_to_bottom();
        let mut stdout = io::stdout();
        if self.stream_buffer.is_empty() {
            self.move_to_transcript_cursor(&mut stdout)?;
        }
        if self.status_active {
            clear_transient_rows(
                &mut stdout,
                self.active_dock_rows(),
                self.transcript_row(),
                1,
            )?;
            self.move_to_transcript_cursor(&mut stdout)?;
            self.status_active = false;
        }
        write_raw_lines(&mut stdout, text)?;
        stdout.flush().map_err(|err| err.to_string())?;
        self.stream_buffer.push_str(text);
        self.advance_transcript_text(text);
        self.record_transcript_text(text);
        self.render_preserving_cursor()
    }

    pub fn finish_stream(&mut self) -> Result<(), String> {
        let stream_had_content = !self.stream_buffer.is_empty();
        let mut stdout = io::stdout();
        if !stream_had_content {
            self.reset_stream_state();
            self.show_cursor()?;
            return self.render();
        }
        self.move_to_transcript_cursor(&mut stdout)?;
        if !self.stream_buffer.ends_with('\n') {
            write!(stdout, "\r\n").map_err(|err| err.to_string())?;
            self.advance_transcript_text("\n");
            self.record_transcript_text("\n");
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.reset_stream_state();
        self.show_cursor()?;
        self.render()
    }

    fn reset_stream_state(&mut self) {
        self.stream_buffer.clear();
        self.status_active = false;
    }

    fn take_status_active(&mut self) -> bool {
        let had_status = self.status_active;
        self.reset_stream_state();
        had_status
    }

    fn insert_text(&mut self, text: &str) -> Result<(), String> {
        insert_str_at(&mut self.buffer, &mut self.cursor, text);
        self.history_index = None;
        self.reset_slash_completion();
        self.render()
    }

    fn complete_slash_command(&mut self) -> Result<(), String> {
        if self.apply_slash_completion() {
            self.render()?;
        }
        Ok(())
    }

    fn apply_slash_completion(&mut self) -> bool {
        let Some(next) = next_slash_completion(
            &self.buffer,
            self.cursor,
            self.slash_completion_index,
            self.slash_completion_prefix.as_deref(),
        ) else {
            return false;
        };
        let rest = &self.buffer[byte_index(&self.buffer, next.token_end)..];
        self.buffer = format!("{}{}", next.command, rest);
        self.cursor = char_len(&next.command);
        self.history_index = None;
        self.slash_completion_index = Some(next.index);
        self.slash_completion_prefix = Some(next.prefix);
        true
    }

    fn reset_slash_completion(&mut self) {
        self.slash_completion_index = None;
        self.slash_completion_prefix = None;
    }

    fn slash_completion_footer(&self) -> Option<String> {
        slash_completion_footer(&self.buffer)
    }

    fn scroll_transcript(&mut self, pages: usize) -> Result<(), String> {
        let page = transcript_view_height(self.active_dock_rows()).max(1) * pages;
        let max_offset = self
            .transcript_lines
            .len()
            .saturating_sub(transcript_view_height(self.active_dock_rows()));
        self.transcript_view_offset = self
            .transcript_view_offset
            .saturating_add(page)
            .min(max_offset);
        self.render_transcript_view()
    }

    fn scroll_transcript_down(&mut self, pages: usize) -> Result<(), String> {
        let page = transcript_view_height(self.active_dock_rows()).max(1) * pages;
        self.transcript_view_offset = self.transcript_view_offset.saturating_sub(page);
        self.render_transcript_view()
    }

    fn snap_transcript_to_bottom(&mut self) {
        self.transcript_view_offset = 0;
    }

    fn render_transcript_view(&mut self) -> Result<(), String> {
        let mut stdout = io::stdout();
        let height = transcript_view_height(self.active_dock_rows());
        for row in 0..height as u16 {
            execute!(stdout, MoveTo(0, row), Clear(ClearType::CurrentLine))
                .map_err(|err| err.to_string())?;
        }
        let end = self
            .transcript_lines
            .len()
            .saturating_sub(self.transcript_view_offset);
        let start = end.saturating_sub(height);
        execute!(stdout, MoveTo(0, 0)).map_err(|err| err.to_string())?;
        for (index, line) in self.transcript_lines[start..end].iter().enumerate() {
            if index > 0 {
                write!(stdout, "\r\n").map_err(|err| err.to_string())?;
            }
            write_raw_lines(&mut stdout, &visible_suffix(line, terminal_width()))?;
        }
        stdout.flush().map_err(|err| err.to_string())?;
        self.render()
    }

    fn record_transcript_text(&mut self, text: &str) {
        if self.transcript_lines.is_empty() {
            self.transcript_lines.push(String::new());
        }
        for ch in text.chars() {
            match ch {
                '\r' => {}
                '\n' => self.transcript_lines.push(String::new()),
                ch => {
                    if let Some(line) = self.transcript_lines.last_mut() {
                        line.push(ch);
                    }
                }
            }
        }
        if self.transcript_lines.len() > 2000 {
            let extra = self.transcript_lines.len() - 2000;
            self.transcript_lines.drain(0..extra);
        }
    }

    fn transcript_row(&self) -> u16 {
        self.transcript_cursor_row
            .or(self.transcript_start_row)
            .unwrap_or_else(|| output_row(self.active_dock_rows()))
            .min(output_row(self.active_dock_rows()))
    }

    fn move_to_transcript_cursor(&mut self, stdout: &mut io::Stdout) -> Result<(), String> {
        set_output_scroll_region(self.active_dock_rows())?;
        let row = self.transcript_row();
        self.transcript_cursor_row = Some(row);
        execute!(stdout, MoveTo(0, row)).map_err(|err| err.to_string())
    }

    fn advance_transcript_text(&mut self, text: &str) {
        let mut row = self.transcript_row();
        let mut column = self.transcript_cursor_column;
        let width = terminal_width().max(1);
        let bottom = output_row(self.active_dock_rows());
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' && take_ansi_sequence(ch, &mut chars).is_some() {
                continue;
            }
            match ch {
                '\r' => column = 0,
                '\n' => {
                    row = row.saturating_add(1).min(bottom);
                    column = 0;
                }
                _ => {
                    let char_width = char_display_width(ch);
                    if char_width == 0 {
                        continue;
                    }
                    if column.saturating_add(char_width) > width {
                        row = row.saturating_add(1).min(bottom);
                        column = 0;
                    }
                    column = column.saturating_add(char_width).min(width);
                }
            }
        }
        self.transcript_cursor_row = Some(row);
        self.transcript_cursor_column = column;
    }

    fn active_dock_rows(&self) -> usize {
        self.rendered_dock_rows.max(1).min(DOCK_RESERVED_ROWS)
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
        execute!(io::stdout(), EnableBracketedPaste).map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for RawModeSession {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show);
        let _ = execute!(io::stdout(), DisableBracketedPaste);
        let _ = reset_output_scroll_region();
        let _ = disable_raw_mode();
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self, String> {
        enable_raw_mode().map_err(|err| err.to_string())?;
        execute!(io::stdout(), EnableBracketedPaste).map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show);
        let _ = execute!(io::stdout(), DisableBracketedPaste);
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

fn render_dock_lines(
    prompt: &str,
    buffer: &str,
    cursor: usize,
    footer: Option<&str>,
    previous_rows: usize,
) -> Result<usize, String> {
    let rows = compose_rendered_dock_rows(prompt, buffer, cursor, terminal_width(), footer);
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

fn compose_rendered_dock_rows(
    prompt: &str,
    buffer: &str,
    cursor: usize,
    width: usize,
    footer: Option<&str>,
) -> ComposedDockRows {
    let mut rows = compose_dock_rows(prompt, buffer, cursor, width);
    rows.lines.insert(0, String::new());
    if let Some(footer) = footer {
        rows.lines.push(muted_dock_help(footer));
    }
    rows.lines.push(muted_dock_help(DOCK_HELP_TEXT));
    rows.cursor_row += 1;
    rows
}

fn muted_dock_help(text: &str) -> String {
    style_prompt_echo("90", text)
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

fn submitted_prompt_echo(submitted: &str) -> String {
    submitted_prompt_echo_with_options(
        submitted,
        terminal_width(),
        std::env::var_os("NO_COLOR").is_none(),
    )
}

fn submitted_prompt_echo_with_options(
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

fn prompt_echo_block_lines(text: &str, width: usize, color_enabled: bool) -> Vec<String> {
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

fn prompt_echo_plain_blank(width: usize) -> String {
    " ".repeat(width)
}

fn prompt_echo_marker(text: &str, color_enabled: bool) -> String {
    style_prompt_echo_with_color("1;38;2;187;154;247;48;2;40;42;54", text, color_enabled)
}

fn prompt_echo_block(text: &str, color_enabled: bool) -> String {
    style_prompt_echo_with_color("38;2;220;223;230;48;2;40;42;54", text, color_enabled)
}

fn style_prompt_echo(code: &str, text: impl AsRef<str>) -> String {
    style_prompt_echo_with_color(code, text, std::env::var_os("NO_COLOR").is_none())
}

fn style_prompt_echo_with_color(code: &str, text: impl AsRef<str>, color_enabled: bool) -> String {
    let text = text.as_ref();
    if color_enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

fn insert_at(buffer: &mut String, cursor: &mut usize, ch: char) {
    let byte_index = byte_index(buffer, *cursor);
    buffer.insert(byte_index, ch);
    *cursor += 1;
}

fn insert_str_at(buffer: &mut String, cursor: &mut usize, text: &str) {
    let byte_index = byte_index(buffer, *cursor);
    buffer.insert_str(byte_index, text);
    *cursor += char_len(text);
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

#[cfg(test)]
fn buffer_prefix(buffer: &str, cursor: usize) -> String {
    buffer.chars().take(cursor).collect()
}

fn previous_word_cursor(buffer: &str, cursor: usize) -> usize {
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

fn next_word_cursor(buffer: &str, cursor: usize) -> usize {
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

fn remove_previous_word(buffer: &mut String, cursor: &mut usize) -> bool {
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

struct SlashCompletion {
    command: String,
    index: usize,
    prefix: String,
    token_end: usize,
}

fn next_slash_completion(
    buffer: &str,
    cursor: usize,
    previous_index: Option<usize>,
    previous_prefix: Option<&str>,
) -> Option<SlashCompletion> {
    let (token, token_end) = slash_completion_token(buffer, cursor)?;
    let prefix = previous_prefix.unwrap_or(token);
    let matches = slash_command_match_entries(prefix);
    if matches.is_empty() {
        return None;
    }
    let selected = previous_index
        .and_then(|index| {
            matches
                .iter()
                .position(|(command_index, _)| *command_index == index)
        })
        .map(|position| (position + 1) % matches.len())
        .unwrap_or(0);
    let (index, command) = matches[selected];
    Some(SlashCompletion {
        command: command.command.to_string(),
        index,
        prefix: prefix.to_string(),
        token_end,
    })
}

fn slash_completion_token(buffer: &str, cursor: usize) -> Option<(&str, usize)> {
    if cursor > char_len(buffer) {
        return None;
    }
    let (token, token_end) = first_slash_token_with_end(buffer)?;
    if cursor != token_end || token_end == 0 {
        return None;
    }
    Some((token, token_end))
}

fn slash_command_match_entries(prefix: &str) -> Vec<(usize, SlashCommandSpec)> {
    SLASH_COMMANDS
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, command)| command.command.starts_with(prefix))
        .collect()
}

fn slash_command_matches(prefix: &str) -> Vec<&'static str> {
    slash_command_match_entries(prefix)
        .into_iter()
        .map(|(_, command)| command.command)
        .collect()
}

fn slash_completion_footer(buffer: &str) -> Option<String> {
    let token = first_slash_token(buffer)?;
    let matches = slash_command_matches(token);
    if matches.is_empty() {
        return Some("No slash command match".to_string());
    }
    Some(format!("Tab complete  {}", matches.join("  ")))
}

fn first_slash_token(buffer: &str) -> Option<&str> {
    first_slash_token_with_end(buffer).map(|(token, _)| token)
}

fn first_slash_token_with_end(buffer: &str) -> Option<(&str, usize)> {
    if buffer.is_empty() {
        return None;
    }
    let mut token_end = char_len(buffer);
    for (index, ch) in buffer.chars().enumerate() {
        if ch.is_whitespace() {
            token_end = index;
            break;
        }
    }
    if token_end == 0 {
        return None;
    }
    let token = &buffer[..byte_index(buffer, token_end)];
    if token.starts_with('/') || token == "?" {
        Some((token, token_end))
    } else {
        None
    }
}

struct ComposedDockRows {
    lines: Vec<String>,
    cursor_row: usize,
    cursor_col: usize,
}

fn compose_dock_rows(prompt: &str, buffer: &str, cursor: usize, width: usize) -> ComposedDockRows {
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

fn visible_len(text: &str) -> usize {
    display_width(text)
}

fn terminal_width() -> usize {
    if let Some((cols, _)) = forced_terminal_size() {
        return cols as usize;
    }
    size().map(|(cols, _)| cols as usize).unwrap_or(80).max(1)
}

fn terminal_rows() -> u16 {
    if let Some((_, rows)) = forced_terminal_size() {
        return rows;
    }
    size().map(|(_, rows)| rows).unwrap_or(24).max(1)
}

fn forced_terminal_size() -> Option<(u16, u16)> {
    std::env::var("DEEPSEEK_FORCE_TTY_SIZE")
        .ok()
        .and_then(|value| parse_forced_terminal_size(&value))
}

fn parse_forced_terminal_size(value: &str) -> Option<(u16, u16)> {
    let (cols, rows) = value.split_once('x')?;
    let cols = cols.parse::<u16>().ok()?.max(1);
    let rows = rows.parse::<u16>().ok()?.max(1);
    Some((cols, rows))
}

fn dock_row() -> u16 {
    terminal_rows().saturating_sub(1)
}

fn output_row(reserved_bottom_lines: usize) -> u16 {
    let reserved = reserved_bottom_lines.max(1).min(DOCK_RESERVED_ROWS) as u16;
    terminal_rows().saturating_sub(reserved + 1)
}

fn transcript_view_height(reserved_bottom_lines: usize) -> usize {
    output_row(reserved_bottom_lines) as usize + 1
}

fn clear_dock_rows(stdout: &mut io::Stdout, rows: usize) -> Result<(), String> {
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

fn clear_rows_above_dock(
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

fn clear_transient_rows(
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
    let rows = terminal_rows();
    let mut stdout = io::stdout();
    // DECSTBM (including the reset form \x1b[r) homes the cursor to (0,0) per
    // ANSI spec. Move immediately to the last row so subsequent output falls at
    // the bottom and scrolls into scrollback rather than overwriting from row 0.
    write!(stdout, "\x1b[r\x1b[{rows};1H").map_err(|err| err.to_string())?;
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

#[cfg(test)]
mod tests {
    use super::{
        buffer_prefix, compose_dock_rows, compose_rendered_dock_rows, insert_at,
        next_slash_completion, next_word_cursor, output_row, parse_forced_terminal_size,
        previous_word_cursor, prompt_echo_block_lines, remove_at, remove_before,
        remove_previous_word, slash_command_matches, slash_completion_footer,
        submitted_prompt_echo_with_options, take_ansi_sequence, visible_len, visible_suffix,
        DockedComposer, DOCK_RESERVED_ROWS,
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
    fn word_motion_skips_whitespace_and_words() {
        let text = "one  two\nthree";

        assert_eq!(previous_word_cursor(text, 8), 5);
        assert_eq!(previous_word_cursor(text, 5), 0);
        assert_eq!(next_word_cursor(text, 0), 5);
        assert_eq!(next_word_cursor(text, 5), 9);
    }

    #[test]
    fn deletes_previous_word() {
        let mut text = "one  two\nthree".to_string();
        let mut cursor = 8;

        assert!(remove_previous_word(&mut text, &mut cursor));
        assert_eq!(text, "one  \nthree");
        assert_eq!(cursor, 5);
    }

    #[test]
    fn dock_composer_wraps_multiline_buffer_and_tracks_cursor() {
        let rows = compose_dock_rows("p> ", "alpha\nbeta", 8, 20);

        assert_eq!(rows.lines, vec!["p> alpha".to_string(), "beta".to_string()]);
        assert_eq!(rows.cursor_row, 1);
        assert_eq!(rows.cursor_col, 2);
        assert!(DOCK_RESERVED_ROWS >= rows.lines.len());
    }

    #[test]
    fn dock_reserves_vertical_padding_rows() {
        let mut rows = compose_dock_rows("p> ", "draft", 5, 20);
        rows.lines.insert(0, String::new());
        rows.lines.push(String::new());
        rows.cursor_row += 1;

        assert_eq!(rows.lines.first().map(String::as_str), Some(""));
        assert_eq!(rows.lines.last().map(String::as_str), Some(""));
        assert_eq!(rows.cursor_row, 1);
        assert!(DOCK_RESERVED_ROWS >= rows.lines.len());
    }

    #[test]
    fn tab_completes_unique_slash_command_prefix() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.buffer = "/sta".to_string();
        composer.cursor = 4;

        assert!(composer.apply_slash_completion());
        assert_eq!(composer.buffer, "/status");
        assert_eq!(composer.cursor, 7);
    }

    #[test]
    fn repeated_tab_cycles_multiple_slash_matches() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.buffer = "/r".to_string();
        composer.cursor = 2;

        assert!(composer.apply_slash_completion());
        assert_eq!(composer.buffer, "/root");
        assert_eq!(composer.cursor, 5);
        assert!(composer.apply_slash_completion());
        assert_eq!(composer.buffer, "/runtime");
        assert_eq!(composer.cursor, 8);
        assert!(composer.apply_slash_completion());
        assert_eq!(composer.buffer, "/root");
    }

    #[test]
    fn slash_completion_preserves_trailing_text_after_command_token() {
        let completion = next_slash_completion("/r path", 2, None, None).unwrap();
        let rest = &"/r path"[super::byte_index("/r path", completion.token_end)..];

        assert_eq!(completion.command, "/root");
        assert_eq!(rest, " path");
    }

    #[test]
    fn editing_after_completion_resets_slash_cycle() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.buffer = "/r".to_string();
        composer.cursor = 2;

        assert!(composer.apply_slash_completion());
        assert!(composer.slash_completion_index.is_some());
        insert_at(&mut composer.buffer, &mut composer.cursor, 'x');
        composer.reset_slash_completion();

        assert_eq!(composer.buffer, "/rootx");
        assert_eq!(composer.slash_completion_index, None);
        assert_eq!(composer.slash_completion_prefix, None);
    }

    #[test]
    fn slash_draft_renders_footer_suggestions() {
        assert_eq!(
            slash_completion_footer("/r"),
            Some("Tab complete  /root  /runtime".to_string())
        );
        assert_eq!(slash_command_matches("/sta"), vec!["/status"]);
    }

    #[test]
    fn help_slash_command_completes_from_h_prefix() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.buffer = "/h".to_string();
        composer.cursor = 2;

        assert_eq!(
            slash_completion_footer("/h"),
            Some("Tab complete  /help".to_string())
        );
        assert!(composer.apply_slash_completion());
        assert_eq!(composer.buffer, "/help");
        assert_eq!(composer.cursor, 5);
    }

    #[test]
    fn quit_alias_is_available_to_slash_completion() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.buffer = "/q".to_string();
        composer.cursor = 2;

        assert_eq!(
            slash_completion_footer("/q"),
            Some("Tab complete  /quit".to_string())
        );
        assert!(composer.apply_slash_completion());
        assert_eq!(composer.buffer, "/quit");
        assert_eq!(composer.cursor, 5);
    }

    #[test]
    fn no_match_slash_draft_renders_footer() {
        assert_eq!(
            slash_completion_footer("/zzz"),
            Some("No slash command match".to_string())
        );
    }

    #[test]
    fn footer_does_not_alter_input_cursor_position() {
        let rows = compose_rendered_dock_rows(
            "p> ",
            "/r",
            2,
            20,
            slash_completion_footer("/r").as_deref(),
        );

        assert_eq!(
            strip_ansi_for_test(&rows.lines[2]),
            "Tab complete  /root  /runtime"
        );
        assert_eq!(rows.cursor_row, 1);
        assert_eq!(rows.cursor_col, 5);
    }

    #[test]
    fn dock_active_rows_follow_rendered_rows() {
        let mut composer = DockedComposer::new("p> ".to_string());

        assert_eq!(composer.active_dock_rows(), 1);

        composer.rendered_dock_rows = 3;
        assert_eq!(composer.active_dock_rows(), 3);

        composer.rendered_dock_rows = DOCK_RESERVED_ROWS + 4;
        assert_eq!(composer.active_dock_rows(), DOCK_RESERVED_ROWS);
    }

    #[test]
    fn output_region_expands_when_dock_reservation_shrinks() {
        assert!(output_row(3) > output_row(DOCK_RESERVED_ROWS));
    }

    #[test]
    fn submitted_prompt_echo_uses_highlighted_prompt_block() {
        let echo = submitted_prompt_echo_with_options("inspect README", 40, true);
        let plain = strip_ansi_for_test(&echo);
        let lines = plain.lines().collect::<Vec<_>>();

        assert_eq!(lines.len(), 5);
        assert_eq!(lines[0], "");
        assert_eq!(lines[1], " ".repeat(40));
        assert!(lines[2].starts_with(">  inspect README"));
        assert_eq!(visible_len(lines[2]), 40);
        assert_eq!(lines[3], " ".repeat(40));
        assert_eq!(lines[4], "");
        assert!(echo.contains("48;2;40;42;54"));
        assert!(echo.contains("\x1b[1;38;2;187;154;247;48;2;40;42;54m"));

        let raw_lines = echo.lines().collect::<Vec<_>>();
        assert!(!raw_lines[1].contains("48;2;40;42;54"));
        assert!(raw_lines[2].contains("48;2;40;42;54"));
        assert!(!raw_lines[3].contains("48;2;40;42;54"));
    }

    #[test]
    fn submitted_prompt_echo_prefixes_multiline_prompts() {
        let echo = submitted_prompt_echo_with_options("first line\nsecond line", 32, false);
        let plain = strip_ansi_for_test(&echo);

        assert!(plain.contains(">  first line"));
        assert!(plain.contains(">  second line"));
        assert!(!echo.contains("\x1b["));
        assert!(plain
            .lines()
            .all(|line| line.is_empty() || visible_len(line) == 32));
    }

    #[test]
    fn prompt_echo_block_wraps_and_pads_content_rows() {
        let lines =
            prompt_echo_block_lines("this is a longer prompt that should wrap cleanly", 30, true);
        let plain = strip_ansi_for_test(&lines.join("\n"));

        assert!(lines.len() > 3);
        assert!(plain.contains(">  this is a longer prompt"));
        assert!(lines
            .iter()
            .all(|line| visible_len(&strip_ansi_for_test(line)) == 30));
        assert!(!lines[0].contains("48;2;40;42;54"));
        assert!(lines[1].contains("48;2;40;42;54"));
        assert!(!lines.last().unwrap().contains("48;2;40;42;54"));
    }

    #[test]
    fn composer_stream_state_can_reset() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.buffer = "draft".to_string();
        composer.stream_buffer = "hello".to_string();
        assert_eq!(composer.stream_buffer, "hello");
        assert!(!composer.status_active);
        composer.status_active = true;
        composer.reset_stream_state();
        assert!(composer.stream_buffer.is_empty());
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
    fn sequential_stream_cursor_advances_with_text() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.set_transcript_start_row(Some(0));
        composer.advance_transcript_text("abc");
        assert_eq!(composer.transcript_cursor_row, Some(0));
        assert_eq!(composer.transcript_cursor_column, 3);
        composer.advance_transcript_text("\ndef");
        assert_eq!(composer.transcript_cursor_row, Some(1));
        assert_eq!(composer.transcript_cursor_column, 3);
    }

    #[test]
    fn transcript_cursor_ignores_ansi_sequences() {
        let mut composer = DockedComposer::new("prompt › ".to_string());
        composer.set_transcript_start_row(Some(0));
        composer.advance_transcript_text("\x1b[36;1ma\x1b[0m\x1b[38;2;125;207;255mb\x1b[0m");

        assert_eq!(composer.transcript_cursor_row, Some(0));
        assert_eq!(composer.transcript_cursor_column, 2);
    }

    #[test]
    fn parses_forced_terminal_size() {
        assert_eq!(parse_forced_terminal_size("80x24"), Some((80, 24)));
        assert_eq!(parse_forced_terminal_size("0x0"), Some((1, 1)));
        assert_eq!(parse_forced_terminal_size("80:24"), None);
    }

    fn strip_ansi_for_test(text: &str) -> String {
        let mut out = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if take_ansi_sequence(ch, &mut chars).is_some() {
                continue;
            }
            out.push(ch);
        }
        out
    }
}
