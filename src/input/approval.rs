use crate::terminal_width::pad_display_width;

use super::{muted_dock_help, truncate_display_text, visible_len, DOCK_RESERVED_ROWS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    ApproveOnce,
    ApproveForSession,
    Reject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ApprovalModal {
    tool: String,
    summary: String,
    selected_index: usize,
}

impl ApprovalModal {
    pub(super) fn new(tool: String, summary: String) -> Self {
        Self {
            tool,
            summary,
            selected_index: 0,
        }
    }

    pub(super) fn move_selection(&mut self, delta: isize) {
        let option_count = APPROVAL_OPTIONS.len();
        self.selected_index = if delta.is_negative() {
            self.selected_index
                .checked_sub(delta.unsigned_abs())
                .unwrap_or(option_count - 1)
        } else {
            (self.selected_index + delta as usize) % option_count
        };
    }

    pub(super) fn selected_choice(&self) -> ApprovalChoice {
        APPROVAL_OPTIONS[self.selected_index].choice
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ApprovalOption {
    label: &'static str,
    choice: ApprovalChoice,
}

const APPROVAL_OPTIONS: &[ApprovalOption] = &[
    ApprovalOption {
        label: "Approve once",
        choice: ApprovalChoice::ApproveOnce,
    },
    ApprovalOption {
        label: "Approve for this session",
        choice: ApprovalChoice::ApproveForSession,
    },
    ApprovalOption {
        label: "Reject",
        choice: ApprovalChoice::Reject,
    },
];

pub(super) fn approval_panel_rows(modal: &ApprovalModal, width: usize) -> Vec<String> {
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
            &format!("{marker} [{}] {}", index + 1, option.label),
            width,
        ));
    }

    rows.push(muted_dock_help(&approval_bottom_border(width)));
    rows.truncate(DOCK_RESERVED_ROWS);
    rows
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
