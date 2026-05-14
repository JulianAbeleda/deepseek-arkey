use crate::agent::ApprovalScope;
use crate::terminal_width::pad_display_width;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{muted_dock_help, truncate_display_text, visible_len, DOCK_RESERVED_ROWS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    ApproveOnce,
    ApproveForSession,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ApprovalModal {
    tool: String,
    scope: ApprovalScope,
    summary: String,
    selected_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ApprovalKeyAction {
    Choose(ApprovalChoice),
    Ignore,
    MoveSelection(isize),
    PassThrough,
}

impl ApprovalModal {
    pub(crate) fn new(tool: String, scope: ApprovalScope, summary: String) -> Self {
        Self {
            tool,
            scope,
            summary,
            selected_index: 0,
        }
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
        let option_count = APPROVAL_OPTIONS.len();
        self.selected_index = if delta.is_negative() {
            self.selected_index
                .checked_sub(delta.unsigned_abs())
                .unwrap_or(option_count - 1)
        } else {
            (self.selected_index + delta as usize) % option_count
        };
    }

    pub(crate) fn selected_choice(&self) -> ApprovalChoice {
        APPROVAL_OPTIONS[self.selected_index].choice
    }

    pub(crate) fn key_action(&self, key: KeyEvent) -> ApprovalKeyAction {
        match key.code {
            KeyCode::Up => ApprovalKeyAction::MoveSelection(-1),
            KeyCode::Down => ApprovalKeyAction::MoveSelection(1),
            KeyCode::Char('1') => ApprovalKeyAction::Choose(ApprovalChoice::ApproveOnce),
            KeyCode::Char('2') => ApprovalKeyAction::Choose(ApprovalChoice::ApproveForSession),
            KeyCode::Char('3') => ApprovalKeyAction::Choose(ApprovalChoice::Reject),
            KeyCode::Char('n') | KeyCode::Char('N') => {
                ApprovalKeyAction::Choose(ApprovalChoice::Reject)
            }
            KeyCode::Esc => ApprovalKeyAction::Choose(ApprovalChoice::Reject),
            KeyCode::Char('c') | KeyCode::Char('d')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                ApprovalKeyAction::Choose(ApprovalChoice::Reject)
            }
            KeyCode::Enter => ApprovalKeyAction::Choose(self.selected_choice()),
            KeyCode::PageUp | KeyCode::PageDown => ApprovalKeyAction::PassThrough,
            _ => ApprovalKeyAction::Ignore,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ApprovalOption {
    choice: ApprovalChoice,
}

const APPROVAL_OPTIONS: &[ApprovalOption] = &[
    ApprovalOption {
        choice: ApprovalChoice::ApproveOnce,
    },
    ApprovalOption {
        choice: ApprovalChoice::ApproveForSession,
    },
    ApprovalOption {
        choice: ApprovalChoice::Reject,
    },
];

pub(crate) fn approval_panel_rows(modal: &ApprovalModal, width: usize) -> Vec<String> {
    let width = width.max(1);
    let inner_width = width.saturating_sub(2);
    let mut rows = Vec::new();
    rows.push(muted_dock_help(&approval_top_border(width)));
    rows.push(approval_panel_content_row(
        &format!("{} requires approval", modal.tool),
        width,
    ));

    let preview_rows = approval_preview_rows(&modal.summary, inner_width);
    if let Some(preview) = preview_rows.first() {
        rows.push(approval_panel_content_row(preview, width));
    } else {
        rows.push(approval_panel_content_row("", width));
    }

    for (index, option) in APPROVAL_OPTIONS.iter().enumerate() {
        let marker = if modal.selected_index == index {
            "→"
        } else {
            " "
        };
        rows.push(approval_panel_content_row(
            &format!(
                "{marker} [{}] {}",
                index + 1,
                modal.option_label(option.choice)
            ),
            width,
        ));
    }

    rows.push(muted_dock_help(&approval_bottom_border(width)));
    rows.truncate(DOCK_RESERVED_ROWS);
    rows
}

impl ApprovalModal {
    fn option_label(&self, choice: ApprovalChoice) -> &'static str {
        match choice {
            ApprovalChoice::ApproveOnce => "Approve once",
            ApprovalChoice::ApproveForSession => match self.scope {
                ApprovalScope::Shell => "Approve shell for this root",
                ApprovalScope::Write => "Approve writes for this root",
            },
            ApprovalChoice::Reject => "Reject",
        }
    }
}

fn approval_preview_rows(summary: &str, width: usize) -> Vec<String> {
    let candidates = summary
        .lines()
        .map(str::trim)
        .filter(|line| {
            !line.is_empty()
                && !line.starts_with("approval required:")
                && !line.starts_with("Type ")
        })
        .collect::<Vec<_>>();
    let preview = candidates
        .iter()
        .find(|line| line.starts_with("command:") || line.starts_with("path:"))
        .copied()
        .or_else(|| candidates.first().copied());
    preview
        .into_iter()
        .map(|line| truncate_display_text(line, width.saturating_sub(2)))
        .collect()
}

fn approval_top_border(width: usize) -> String {
    if width <= 2 {
        return "─".repeat(width);
    }
    let title = "─ approval ";
    let right = width.saturating_sub(2 + visible_len(title));
    format!("╭{}{}╮", title, "─".repeat(right))
}

fn approval_bottom_border(width: usize) -> String {
    if width <= 2 {
        return "─".repeat(width);
    }
    let hint = " ↑/↓ Enter Esc ";
    let hint_width = visible_len(hint);
    if width <= hint_width + 2 {
        return format!("╰{}╯", "─".repeat(width - 2));
    }
    let left = width.saturating_sub(hint_width + 2);
    format!("╰{}{}╯", "─".repeat(left), hint)
}

fn approval_panel_content_row(text: &str, width: usize) -> String {
    if width <= 2 {
        return muted_dock_help(&truncate_display_text(text, width));
    }
    let inner_width = width - 2;
    let content = truncate_display_text(text, inner_width);
    muted_dock_help(&format!("│{}│", pad_display_width(&content, inner_width)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL)
    }

    #[test]
    fn approval_modal_key_actions_map_shortcuts() {
        let modal =
            ApprovalModal::new("run_shell".to_string(), ApprovalScope::Shell, String::new());

        assert_eq!(
            modal.key_action(key(KeyCode::Char('1'))),
            ApprovalKeyAction::Choose(ApprovalChoice::ApproveOnce)
        );
        assert_eq!(
            modal.key_action(key(KeyCode::Char('2'))),
            ApprovalKeyAction::Choose(ApprovalChoice::ApproveForSession)
        );
        assert_eq!(
            modal.key_action(key(KeyCode::Char('3'))),
            ApprovalKeyAction::Choose(ApprovalChoice::Reject)
        );
        assert_eq!(
            modal.key_action(key(KeyCode::Char('n'))),
            ApprovalKeyAction::Choose(ApprovalChoice::Reject)
        );
        assert_eq!(
            modal.key_action(key(KeyCode::Char('N'))),
            ApprovalKeyAction::Choose(ApprovalChoice::Reject)
        );
        assert_eq!(
            modal.key_action(key(KeyCode::Esc)),
            ApprovalKeyAction::Choose(ApprovalChoice::Reject)
        );
        assert_eq!(
            modal.key_action(ctrl('c')),
            ApprovalKeyAction::Choose(ApprovalChoice::Reject)
        );
        assert_eq!(
            modal.key_action(ctrl('d')),
            ApprovalKeyAction::Choose(ApprovalChoice::Reject)
        );
    }

    #[test]
    fn approval_modal_key_actions_move_and_choose_selection() {
        let mut modal = ApprovalModal::new(
            "propose_patch".to_string(),
            ApprovalScope::Write,
            String::new(),
        );

        assert_eq!(
            modal.key_action(key(KeyCode::Down)),
            ApprovalKeyAction::MoveSelection(1)
        );
        assert_eq!(
            modal.key_action(key(KeyCode::Up)),
            ApprovalKeyAction::MoveSelection(-1)
        );

        modal.move_selection(1);
        assert_eq!(
            modal.key_action(key(KeyCode::Enter)),
            ApprovalKeyAction::Choose(ApprovalChoice::ApproveForSession)
        );
    }

    #[test]
    fn approval_modal_key_actions_ignore_or_pass_through() {
        let modal =
            ApprovalModal::new("run_shell".to_string(), ApprovalScope::Shell, String::new());

        assert_eq!(
            modal.key_action(key(KeyCode::PageUp)),
            ApprovalKeyAction::PassThrough
        );
        assert_eq!(
            modal.key_action(key(KeyCode::PageDown)),
            ApprovalKeyAction::PassThrough
        );
        assert_eq!(
            modal.key_action(key(KeyCode::Char('x'))),
            ApprovalKeyAction::Ignore
        );
    }
}
