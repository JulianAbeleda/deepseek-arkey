use std::io::{self};

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyEvent, KeyEventKind, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::input::composer::RawModeSession;

use super::layout::reset_output_scroll_region;

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
