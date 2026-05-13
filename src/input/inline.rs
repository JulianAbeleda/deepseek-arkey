use std::io::{self, IsTerminal, Write};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};

use super::support::{
    char_len, insert_at, insert_str_at, is_key_press_or_repeat, newline, next_word_cursor,
    previous_word_cursor, remove_at, remove_before, render_line, RawModeGuard,
};
use super::types::InputAction;

pub struct InlineInput {
    history: Vec<String>,
    history_index: Option<usize>,
}

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
                Event::Key(key) if is_key_press_or_repeat(key) => match key.code {
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
                Event::Key(_) => continue,
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
