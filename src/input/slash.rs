use crate::terminal_width::pad_display_width;

use super::{
    byte_index, char_len, muted_dock_help, truncate_display_text, visible_len, SlashCommandSpec,
    SLASH_COMMANDS,
};

pub(super) struct SlashCompletion {
    pub(super) command: String,
    pub(super) index: usize,
    pub(super) prefix: String,
    pub(super) token_end: usize,
}

pub(super) fn next_slash_completion(
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

#[cfg(test)]
pub(super) fn slash_command_matches(prefix: &str) -> Vec<&'static str> {
    slash_command_match_entries(prefix)
        .into_iter()
        .map(|(_, command)| command.command)
        .collect()
}

pub(super) fn slash_completion_panel_rows(
    buffer: &str,
    selected_command_index: Option<usize>,
    width: usize,
) -> Vec<String> {
    let width = width.max(1);
    let Some(token) = first_slash_token(buffer) else {
        return Vec::new();
    };
    let matches = slash_command_match_entries(token);
    let mut rows = Vec::new();
    rows.push(muted_dock_help(&"─".repeat(width)));
    if matches.is_empty() {
        rows.push(muted_dock_help(&pad_display_width(
            "  No slash command match",
            width,
        )));
        return rows;
    }

    let marker_width = 2usize;
    let command_width = slash_panel_command_width(&matches, width, marker_width);
    let gap_width = if width > marker_width + command_width + 6 {
        3
    } else {
        1
    };
    let description_width = width.saturating_sub(marker_width + command_width + gap_width);

    rows.extend(matches.into_iter().map(|(index, command)| {
        slash_completion_panel_row(
            command,
            selected_command_index == Some(index),
            marker_width,
            command_width,
            gap_width,
            description_width,
            width,
        )
    }));
    rows
}

fn slash_panel_command_width(
    matches: &[(usize, SlashCommandSpec)],
    width: usize,
    marker_width: usize,
) -> usize {
    let longest = matches
        .iter()
        .map(|(_, command)| visible_len(command.command))
        .max()
        .unwrap_or(0);
    let usable_width = width.saturating_sub(marker_width);
    longest.saturating_add(2).min(usable_width)
}

fn slash_completion_panel_row(
    command: SlashCommandSpec,
    selected: bool,
    marker_width: usize,
    command_width: usize,
    gap_width: usize,
    description_width: usize,
    width: usize,
) -> String {
    let marker = if selected { "› " } else { "  " };
    let command_text = truncate_display_text(command.command, command_width);
    let description = truncate_display_text(command.description, description_width);
    let text = format!(
        "{}{}{}{}",
        pad_display_width(marker, marker_width),
        pad_display_width(&command_text, command_width),
        " ".repeat(gap_width),
        pad_display_width(&description, description_width)
    );
    muted_dock_help(&pad_display_width(&text, width))
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
