use crate::terminal_width::{display_width, pad_display_width, wrap_plain_text};

pub(crate) fn render_terminal_markdown(text: &str) -> String {
    // This renderer expects complete text, not incremental streaming chunks.
    let mut output = String::new();
    let mut in_code_block = false;
    let lines = text.lines().collect::<Vec<_>>();
    let mut index = 0usize;
    while index < lines.len() {
        let line = lines[index];
        let stripped = line.trim();
        if stripped.starts_with("```") {
            in_code_block = !in_code_block;
            output.push_str(line);
            output.push('\n');
            index += 1;
            continue;
        }
        if in_code_block {
            output.push_str(line);
            output.push('\n');
            index += 1;
            continue;
        }
        if is_table_row(stripped) {
            let (rendered_table, next_index) = render_table_block(&lines, index);
            if !rendered_table.is_empty() {
                output.push_str(&rendered_table);
                index = next_index;
                continue;
            }
        }
        if stripped.is_empty() {
            output.push('\n');
            index += 1;
            continue;
        }
        if let Some((level, body)) = markdown_heading(stripped) {
            output.push_str(&tokyo_heading(level, body));
            output.push('\n');
            index += 1;
            continue;
        }
        if stripped == "---" {
            output.push_str(&tokyo_muted(&"-".repeat(40)));
            output.push('\n');
            index += 1;
            continue;
        }
        if let Some(body) = stripped
            .strip_prefix("- ")
            .or_else(|| stripped.strip_prefix("* "))
            .or_else(|| stripped.strip_prefix("+ "))
        {
            output.push_str(&tokyo_list_marker("-"));
            output.push(' ');
            output.push_str(&render_inline_markdown(body));
            output.push('\n');
            index += 1;
            continue;
        }
        if let Some((marker, body)) = markdown_numbered_item(stripped) {
            output.push_str(&tokyo_list_marker(marker));
            output.push(' ');
            output.push_str(&render_inline_markdown(body));
            output.push('\n');
            index += 1;
            continue;
        }
        output.push_str(&render_inline_markdown(line));
        output.push('\n');
        index += 1;
    }
    output
}

fn markdown_heading(line: &str) -> Option<(usize, &str)> {
    for (level, prefix) in [(3, "### "), (2, "## "), (1, "# ")] {
        if let Some(body) = line.strip_prefix(prefix) {
            return Some((level, body));
        }
    }
    None
}

fn markdown_numbered_item(line: &str) -> Option<(&str, &str)> {
    let (marker, body) = line.split_once(' ')?;
    let number = marker.strip_suffix('.')?;
    (!number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit())).then_some((marker, body))
}

fn render_inline_markdown(text: &str) -> String {
    let mut output = String::new();
    let mut remaining = text;
    let mut bold = false;
    while let Some(index) = remaining.find("**") {
        output.push_str(&render_inline_code_and_urls(&remaining[..index], bold));
        if bold {
            output.push_str("\x1b[0m");
        } else {
            output.push_str("\x1b[1m");
        }
        bold = !bold;
        remaining = &remaining[index + 2..];
    }
    output.push_str(&render_inline_code_and_urls(remaining, bold));
    if bold {
        output.push_str("\x1b[0m");
    }
    output
}

fn render_table_block(lines: &[&str], start: usize) -> (String, usize) {
    let mut end = start;
    let mut rows = Vec::new();
    while let Some(line) = lines.get(end) {
        let stripped = line.trim();
        if !is_table_row(stripped) {
            break;
        }
        rows.push(parse_table_row(stripped));
        end += 1;
    }
    if rows.len() < 2 || !is_table_separator_row(&rows[1]) {
        return (String::new(), start);
    }
    let column_count = rows.iter().map(Vec::len).max().unwrap_or(0);
    if column_count < 2 {
        return (String::new(), start);
    }
    for row in &mut rows {
        row.resize(column_count, String::new());
    }
    let data_rows = rows
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != 1)
        .map(|(_, row)| row)
        .collect::<Vec<_>>();
    let mut widths = vec![3usize; column_count];
    for row in &data_rows {
        for (index, cell) in row.iter().enumerate() {
            widths[index] = widths[index]
                .max(display_width(cell).min(MAX_TABLE_CELL_WIDTH))
                .min(MAX_TABLE_CELL_WIDTH);
        }
    }
    let mut output = String::new();
    output.push_str(&format_table_row(&rows[0], &widths));
    output.push_str(&format_table_rule(&widths));
    for row in rows.iter().skip(2) {
        output.push_str(&format_table_row(row, &widths));
    }
    (output, end)
}

const MAX_TABLE_CELL_WIDTH: usize = 36;

fn is_table_row(line: &str) -> bool {
    line.starts_with('|') && line.ends_with('|') && line.matches('|').count() >= 2
}

fn parse_table_row(line: &str) -> Vec<String> {
    line.trim()
        .trim_matches('|')
        .split('|')
        .map(|cell| cell.trim().to_string())
        .collect()
}

fn is_table_separator_row(row: &[String]) -> bool {
    row.iter().all(|cell| {
        let cell = cell.trim();
        cell.len() >= 3 && cell.chars().all(|ch| matches!(ch, '-' | ':' | ' '))
    })
}

fn format_table_row(row: &[String], widths: &[usize]) -> String {
    let wrapped = row
        .iter()
        .zip(widths)
        .map(|(cell, width)| wrap_plain_text(cell, *width))
        .collect::<Vec<_>>();
    let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
    let mut output = String::new();
    for line_index in 0..height {
        output.push_str(&tokyo_muted("|"));
        for (column, width) in widths.iter().enumerate() {
            let cell = wrapped
                .get(column)
                .and_then(|lines| lines.get(line_index))
                .map(String::as_str)
                .unwrap_or("");
            let rendered = render_inline_markdown(cell);
            output.push(' ');
            output.push_str(&pad_display_width(&rendered, *width));
            output.push(' ');
            output.push_str(&tokyo_muted("|"));
        }
        output.push('\n');
    }
    output
}

fn format_table_rule(widths: &[usize]) -> String {
    let mut output = String::new();
    output.push_str(&tokyo_muted("|"));
    for width in widths {
        output.push_str(&tokyo_muted(&format!("{}|", "-".repeat(width + 2))));
    }
    output.push('\n');
    output
}

fn render_inline_code_and_urls(text: &str, bold_active: bool) -> String {
    let mut output = String::new();
    let mut remaining = text;
    let mut code = false;
    while let Some(index) = remaining.find('`') {
        output.push_str(&color_urls(&remaining[..index]));
        if code {
            output.push_str("\x1b[0m");
            if bold_active {
                output.push_str("\x1b[1m");
            }
        } else {
            output.push_str("\x1b[38;2;125;207;255m");
        }
        code = !code;
        remaining = &remaining[index + 1..];
    }
    output.push_str(&color_urls(remaining));
    if code {
        output.push_str("\x1b[0m");
        if bold_active {
            output.push_str("\x1b[1m");
        }
    }
    output
}

fn color_urls(text: &str) -> String {
    let mut output = String::new();
    let mut token = String::new();
    for ch in text.chars() {
        if ch.is_whitespace() {
            output.push_str(&color_url_token(&token));
            token.clear();
            output.push(ch);
        } else {
            token.push(ch);
        }
    }
    output.push_str(&color_url_token(&token));
    output
}

fn color_url_token(token: &str) -> String {
    let trimmed = token.trim_end_matches(|ch| matches!(ch, '.' | ',' | ';' | ':' | '!' | '?'));
    let suffix = &token[trimmed.len()..];
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        format!("{}{}", tokyo_url(trimmed), suffix)
    } else {
        token.to_string()
    }
}

fn tokyo_heading(level: usize, text: &str) -> String {
    let text = render_inline_markdown(text);
    let code = if level <= 2 { "36;1" } else { "94;1" };
    ansi(code, &reapply_ansi_after_resets(&text, code))
}

fn reapply_ansi_after_resets(text: &str, code: &str) -> String {
    // Inline styling closes with SGR reset. Re-apply heading style after each reset.
    let reset = "\x1b[0m";
    let resume = format!("{reset}\x1b[{code}m");
    text.replace(reset, &resume)
}

fn tokyo_list_marker(text: &str) -> String {
    ansi("38;2;187;154;247", text)
}

fn tokyo_url(text: &str) -> String {
    ansi("38;2;125;207;255", text)
}

fn tokyo_muted(text: &str) -> String {
    ansi("90", text)
}

fn ansi(code: &str, text: &str) -> String {
    format!("\x1b[{code}m{text}\x1b[0m")
}

#[cfg(test)]
mod tests {
    use super::render_terminal_markdown;

    #[test]
    fn renders_headings_at_all_levels() {
        let rendered = render_terminal_markdown("# One\n## Two\n### Three\n");
        assert!(rendered.contains("\x1b[36;1mOne\x1b[0m"));
        assert!(rendered.contains("\x1b[36;1mTwo\x1b[0m"));
        assert!(rendered.contains("\x1b[94;1mThree\x1b[0m"));
    }

    #[test]
    fn renders_nested_inline_styles_inside_headings() {
        let rendered = render_terminal_markdown("## **Important** Result\n");
        assert!(rendered.contains("\x1b[36;1m\x1b[1mImportant\x1b[0m\x1b[36;1m Result\x1b[0m"));
        assert_eq!(strip_ansi_for_test(&rendered), "Important Result\n");
    }

    #[test]
    fn renders_bullets_and_numbered_lists() {
        let rendered = render_terminal_markdown("- item\n1. next\n");
        assert!(rendered.contains("\x1b[38;2;187;154;247m-\x1b[0m item"));
        assert!(rendered.contains("\x1b[38;2;187;154;247m1.\x1b[0m next"));
    }

    #[test]
    fn preserves_fenced_code_blocks_raw() {
        let rendered = render_terminal_markdown("```text\n**raw**\n```\n");
        assert!(rendered.contains("**raw**"));
        assert_eq!(strip_ansi_for_test(&rendered), "```text\n**raw**\n```\n");
    }

    #[test]
    fn renders_aligned_markdown_tables() {
        let raw = "| A | Longer |\n|---|---|\n| `x` | **y** |\n";
        let rendered = render_terminal_markdown(raw);
        let stripped = strip_ansi_for_test(&rendered);

        assert!(stripped.contains("| A   | Longer |"));
        assert!(stripped.contains("|-----|--------|"));
        assert!(stripped.contains("| x   | y      |"));
    }

    #[test]
    fn renders_inline_code_and_urls() {
        let rendered = render_terminal_markdown("Use `run_shell`: https://example.com.\n");
        assert!(rendered.contains("\x1b[38;2;125;207;255mrun_shell\x1b[0m"));
        assert!(rendered.contains("\x1b[38;2;125;207;255mhttps://example.com\x1b[0m."));
    }

    #[test]
    fn renders_horizontal_rules_as_muted_lines() {
        let rendered = render_terminal_markdown("---\n");
        assert!(rendered.contains("\x1b[90m----------------------------------------\x1b[0m"));
    }

    fn strip_ansi_for_test(text: &str) -> String {
        let mut output = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
                continue;
            }
            output.push(ch);
        }
        output
    }
}
