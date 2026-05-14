use std::io::{self, Write};
use std::time::Duration;

use crossterm::cursor::{Hide, MoveTo, MoveToColumn, Show};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{Clear, ClearType};

use crate::agent::ApprovalScope;
use crate::terminal_width::char_display_width;

use super::approval::ApprovalKeyAction;
#[allow(unused_imports)]
pub(crate) use super::approval::{approval_panel_rows, ApprovalChoice, ApprovalModal};
#[cfg(test)]
pub(crate) use super::slash::slash_command_matches;
pub(crate) use super::slash::{next_slash_completion, slash_completion_panel_rows};
pub(crate) use super::support::*;
use super::types::ApprovalModalEvent;

use super::types::InputAction;
#[allow(unused_imports)]
pub(crate) use super::types::{
    SlashCommandSpec, DOCK_HELP_TEXT, DOCK_RESERVED_ROWS, DOCK_VERTICAL_PADDING_ROWS,
    SLASH_COMMANDS,
};

pub struct DockedComposer {
    prompt: String,
    buffer: String,
    cursor: usize,
    history: Vec<String>,
    history_index: Option<usize>,
    slash_completion_index: Option<usize>,
    slash_completion_prefix: Option<String>,
    approval_modal: Option<ApprovalModal>,
    progress_rows: Vec<String>,
    stream_buffer: String,
    transcript_lines: Vec<String>,
    transcript_view_offset: usize,
    transcript_start_row: Option<u16>,
    transcript_cursor_row: Option<u16>,
    transcript_cursor_column: usize,
    rendered_dock_rows: usize,
    cursor_hidden: bool,
}

pub struct RawModeSession;

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
            approval_modal: None,
            progress_rows: Vec::new(),
            stream_buffer: String::new(),
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
        let approval_rows = self.approval_panel_rows();
        let panel_rows = if approval_rows.is_empty() {
            let slash_rows = self.slash_completion_panel_rows();
            if slash_rows.is_empty() {
                self.progress_rows.clone()
            } else {
                slash_rows
            }
        } else {
            approval_rows
        };
        self.rendered_dock_rows = render_dock_lines(
            &self.prompt,
            &self.buffer,
            self.cursor,
            &panel_rows,
            self.approval_modal.is_some(),
            self.rendered_dock_rows,
        )?;
        Ok(())
    }

    pub fn show_approval_modal(
        &mut self,
        tool: String,
        scope: ApprovalScope,
        summary: String,
    ) -> Result<(), String> {
        self.buffer.clear();
        self.cursor = 0;
        self.history_index = None;
        self.reset_slash_completion();
        self.approval_modal = Some(ApprovalModal::new(tool, scope, summary));
        self.hide_cursor()?;
        self.render()
    }

    pub fn clear_approval_modal(&mut self) -> Result<(), String> {
        self.approval_modal = None;
        self.show_cursor()?;
        self.render()
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
        let approval_rows = self.approval_panel_rows();
        let panel_rows = if approval_rows.is_empty() {
            let slash_rows = self.slash_completion_panel_rows();
            if slash_rows.is_empty() {
                self.progress_rows.clone()
            } else {
                slash_rows
            }
        } else {
            approval_rows
        };
        self.rendered_dock_rows = render_dock_lines(
            &self.prompt,
            &self.buffer,
            self.cursor,
            &panel_rows,
            self.approval_modal.is_some(),
            self.rendered_dock_rows,
        )?;
        execute!(io::stdout(), MoveTo(column, row)).map_err(|err| err.to_string())?;
        Ok(())
    }

    pub fn poll_action(&mut self, timeout: Duration) -> Result<Option<InputAction>, String> {
        if !event::poll(timeout).map_err(|err| err.to_string())? {
            return Ok(None);
        }
        let event = event::read().map_err(|err| err.to_string())?;
        if let ApprovalModalEvent::Consumed(action) = self.handle_approval_modal_event(&event)? {
            return Ok(action);
        }
        match event {
            Event::Paste(text) => {
                self.insert_text(&text)?;
                Ok(None)
            }
            Event::Key(key) if is_key_press_or_repeat(key) => match key.code {
                KeyCode::Esc => Ok(Some(InputAction::Cancel)),
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
                    let ch = shifted_char(ch, key.modifiers);
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
            Event::Key(_) => Ok(None),
            _ => Ok(None),
        }
    }

    pub fn print_above(&mut self, text: &str) -> Result<(), String> {
        self.snap_transcript_to_bottom();
        let mut stdout = io::stdout();
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

    pub fn progress_dock(&mut self, text: &str) -> Result<(), String> {
        self.progress_rows = progress_panel_rows(text, terminal_width());
        self.hide_cursor()?;
        self.render_preserving_cursor()
    }

    pub fn clear_progress_dock(&mut self) -> Result<(), String> {
        if self.progress_rows.is_empty() {
            return Ok(());
        }
        self.progress_rows.clear();
        self.render_preserving_cursor()
    }

    pub fn stream_above(&mut self, text: &str) -> Result<(), String> {
        self.snap_transcript_to_bottom();
        self.progress_rows.clear();
        let mut stdout = io::stdout();
        if self.stream_buffer.is_empty() {
            self.move_to_transcript_cursor(&mut stdout)?;
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
        self.progress_rows.clear();
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

    fn handle_approval_modal_event(&mut self, event: &Event) -> Result<ApprovalModalEvent, String> {
        let Some(modal) = self.approval_modal.as_ref() else {
            return Ok(ApprovalModalEvent::PassThrough);
        };

        match event {
            Event::Key(key) if is_key_press_or_repeat(*key) => match modal.key_action(*key) {
                ApprovalKeyAction::Choose(choice) => Ok(ApprovalModalEvent::Consumed(Some(
                    InputAction::Approval(choice),
                ))),
                ApprovalKeyAction::Ignore => Ok(ApprovalModalEvent::Consumed(None)),
                ApprovalKeyAction::MoveSelection(delta) => {
                    self.move_approval_selection(delta)?;
                    Ok(ApprovalModalEvent::Consumed(None))
                }
                ApprovalKeyAction::PassThrough => Ok(ApprovalModalEvent::PassThrough),
            },
            Event::Key(_) => Ok(ApprovalModalEvent::Consumed(None)),
            Event::Paste(_) => Ok(ApprovalModalEvent::Consumed(None)),
            _ => Ok(ApprovalModalEvent::Consumed(None)),
        }
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

    fn move_approval_selection(&mut self, delta: isize) -> Result<(), String> {
        let Some(modal) = self.approval_modal.as_mut() else {
            return Ok(());
        };
        modal.move_selection(delta);
        self.render()
    }

    fn approval_panel_rows(&self) -> Vec<String> {
        self.approval_modal
            .as_ref()
            .map(|modal| approval_panel_rows(modal, terminal_width()))
            .unwrap_or_default()
    }

    fn slash_completion_panel_rows(&self) -> Vec<String> {
        slash_completion_panel_rows(&self.buffer, self.slash_completion_index, terminal_width())
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

pub(crate) fn shifted_char(ch: char, modifiers: KeyModifiers) -> char {
    if !modifiers.contains(KeyModifiers::SHIFT) {
        return ch;
    }
    match ch {
        'a'..='z' => ch.to_ascii_uppercase(),
        '1' => '!',
        '2' => '@',
        '3' => '#',
        '4' => '$',
        '5' => '%',
        '6' => '^',
        '7' => '&',
        '8' => '*',
        '9' => '(',
        '0' => ')',
        '`' => '~',
        '-' => '_',
        '=' => '+',
        '[' => '{',
        ']' => '}',
        '\\' => '|',
        ';' => ':',
        '\'' => '"',
        ',' => '<',
        '.' => '>',
        '/' => '?',
        _ => ch,
    }
}

#[cfg(test)]
#[path = "composer_tests.rs"]
mod tests;
