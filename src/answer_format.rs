use crate::terminal_markdown::render_terminal_markdown;

pub(crate) fn format_agent_answer(answer: &str) -> String {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut text = trimmed.replace("\r\n", "\n").replace('\r', "\n");
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        text = json_answer_to_markdown(&value);
    }
    text = insert_markdown_boundaries(&text);
    text = split_markdown_table_lines(&text);
    text = split_horizontal_rule_lines(&text);
    text = split_known_heading_bodies(&text);
    text = collapse_excess_blank_lines(&text);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

pub(crate) fn terminal_agent_answer(answer: &str) -> String {
    render_terminal_markdown(&format_agent_answer(answer))
}

fn json_answer_to_markdown(value: &serde_json::Value) -> String {
    let mut output = String::new();
    match value {
        serde_json::Value::Object(map) => render_json_object(map, 2, &mut output),
        serde_json::Value::Array(items) => render_json_array(items, 0, &mut output),
        _ => {
            output.push_str(&json_scalar_text(value));
            output.push('\n');
        }
    }
    output
}

fn render_json_object(
    map: &serde_json::Map<String, serde_json::Value>,
    level: usize,
    output: &mut String,
) {
    for (key, value) in map {
        if value.is_null() {
            continue;
        }
        if value_is_scalar(value) {
            output.push_str("- ");
            output.push_str(&humanize_json_key(key));
            output.push_str(": ");
            output.push_str(&json_scalar_text(value));
            output.push('\n');
            continue;
        }
        push_json_heading(output, level, key);
        match value {
            serde_json::Value::Object(child) => {
                render_json_object(child, (level + 1).min(6), output)
            }
            serde_json::Value::Array(items) => render_json_array(items, level + 1, output),
            _ => {}
        }
        output.push('\n');
    }
}

fn render_json_array(items: &[serde_json::Value], level: usize, output: &mut String) {
    for (index, item) in items.iter().filter(|item| !item.is_null()).enumerate() {
        match item {
            serde_json::Value::Object(map) => {
                output.push_str(&format!("{}. Item {}\n", index + 1, index + 1));
                render_json_object(map, (level + 1).clamp(3, 6), output);
            }
            serde_json::Value::Array(items) => render_json_array(items, level + 1, output),
            _ => {
                output.push_str("- ");
                output.push_str(&json_scalar_text(item));
                output.push('\n');
            }
        }
    }
}

fn push_json_heading(output: &mut String, level: usize, key: &str) {
    if !output.is_empty() && !output.ends_with("\n\n") {
        if output.ends_with('\n') {
            output.push('\n');
        } else {
            output.push_str("\n\n");
        }
    }
    output.push_str(&"#".repeat(level.clamp(2, 6)));
    output.push(' ');
    output.push_str(&humanize_json_key(key));
    output.push_str("\n\n");
}

fn value_is_scalar(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::String(_) | serde_json::Value::Number(_) | serde_json::Value::Bool(_)
    )
}

fn json_scalar_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.replace('\n', " "),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn humanize_json_key(key: &str) -> String {
    let mut output = String::new();
    let mut capitalize_next = true;
    for ch in key.chars() {
        if ch == '_' || ch == '-' {
            output.push(' ');
            capitalize_next = true;
        } else if capitalize_next {
            output.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            output.push(ch);
        }
    }
    output
}

fn insert_markdown_boundaries(text: &str) -> String {
    let mut output = String::new();
    let mut index = 0usize;
    while index < text.len() {
        let rest = &text[index..];
        if should_break_table_row_before(text, index, rest) {
            push_line_boundary(&mut output);
        } else if should_break_before(text, index, rest) {
            push_boundary(&mut output);
        }
        let ch = rest.chars().next().unwrap();
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

fn should_break_before(text: &str, index: usize, rest: &str) -> bool {
    if index == 0 || text[..index].ends_with('\n') {
        return false;
    }
    let heading_marker = !text[..index].ends_with('#')
        && (rest.starts_with("### ") || rest.starts_with("## ") || rest.starts_with("# "));
    heading_marker
        || horizontal_rule_marker(rest)
        || (!text[..index].ends_with('-') && rest.starts_with("- "))
        || (previous_char_is_list_boundary(text, index) && numbered_list_marker(rest))
}

fn should_break_table_row_before(text: &str, index: usize, rest: &str) -> bool {
    if index == 0 || text[..index].ends_with('\n') || !markdown_table_row_marker(rest) {
        return false;
    }
    !current_line_contains_pipe(text, index)
        || previous_non_whitespace_char(text, index) == Some('|')
}

fn markdown_table_row_marker(text: &str) -> bool {
    let Some(after_pipe) = text.strip_prefix('|') else {
        return false;
    };
    if !matches!(after_pipe.chars().next(), Some(' ' | '-' | ':')) {
        return false;
    }
    after_pipe
        .split('\n')
        .next()
        .is_some_and(|line| line.contains('|'))
}

fn horizontal_rule_marker(text: &str) -> bool {
    text.starts_with("--- ### ") || text.starts_with("--- ## ") || text.starts_with("--- # ")
}

fn previous_char_is_digit(text: &str, index: usize) -> bool {
    matches!(text[..index].chars().last(), Some(ch) if ch.is_ascii_digit())
}

fn previous_char_is_list_boundary(text: &str, index: usize) -> bool {
    matches!(text[..index].chars().last(), Some(ch) if ch.is_whitespace())
        && !previous_char_is_digit(text, index)
}

fn numbered_list_marker(text: &str) -> bool {
    let mut chars = text.chars();
    let mut saw_digit = false;
    for ch in chars.by_ref() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            continue;
        }
        if ch != '.' {
            return false;
        }
        return saw_digit && chars.next() == Some(' ');
    }
    false
}

fn current_line_contains_pipe(text: &str, index: usize) -> bool {
    text[..index]
        .rsplit_once('\n')
        .map_or(&text[..index], |(_, line)| line)
        .contains('|')
}

fn previous_non_whitespace_char(text: &str, index: usize) -> Option<char> {
    text[..index].chars().rev().find(|ch| !ch.is_whitespace())
}

fn split_markdown_table_lines(text: &str) -> String {
    let mut output = String::new();
    let mut expected_pipes = None;
    for line in text.lines() {
        if line.starts_with('|') {
            let expected = *expected_pipes
                .get_or_insert_with(|| infer_table_row_pipe_count(line).unwrap_or_default());
            let mut rest = line;
            while let Some((row, next)) = split_table_line_after_pipe_count(rest, expected) {
                output.push_str(row);
                output.push('\n');
                rest = next.trim_start();
                if !markdown_table_row_marker(rest) {
                    break;
                }
            }
            if !rest.is_empty() {
                output.push_str(rest);
                output.push('\n');
            }
            continue;
        } else if !line.trim().is_empty() {
            expected_pipes = None;
        }
        output.push_str(line);
        output.push('\n');
    }
    output.trim_end().to_string()
}

fn infer_table_row_pipe_count(line: &str) -> Option<usize> {
    let mut count = 0usize;
    for (index, ch) in line.char_indices() {
        if ch != '|' {
            continue;
        }
        count += 1;
        let rest = line[index + ch.len_utf8()..].trim_start();
        if count >= 2 && markdown_table_row_marker(rest) {
            return Some(count);
        }
    }
    Some(count).filter(|count| *count >= 2)
}

fn split_table_line_after_pipe_count(line: &str, expected_pipes: usize) -> Option<(&str, &str)> {
    if expected_pipes < 2 {
        return None;
    }
    let mut count = 0usize;
    for (index, ch) in line.char_indices() {
        if ch != '|' {
            continue;
        }
        count += 1;
        if count == expected_pipes {
            let end = index + ch.len_utf8();
            let rest = &line[end..];
            if rest.trim().is_empty() {
                return None;
            }
            return Some((&line[..end], rest));
        }
    }
    None
}

fn split_horizontal_rule_lines(text: &str) -> String {
    let mut output = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            output.push_str("---\n\n");
            output.push_str(rest);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output.trim_end().to_string()
}

fn push_boundary(output: &mut String) {
    while output.ends_with(' ') || output.ends_with('\t') {
        output.pop();
    }
    if output.ends_with("\n\n") {
        return;
    }
    if output.ends_with('\n') {
        output.push('\n');
    } else {
        output.push_str("\n\n");
    }
}

fn push_line_boundary(output: &mut String) {
    while output.ends_with(' ') || output.ends_with('\t') {
        output.pop();
    }
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

fn split_known_heading_bodies(text: &str) -> String {
    text.lines()
        .map(split_known_heading_body)
        .collect::<Vec<_>>()
        .join("\n")
}

fn split_known_heading_body(line: &str) -> String {
    let Some((prefix, content)) = heading_parts(line) else {
        return line.to_string();
    };
    for heading in [
        "Key Design Decisions",
        "Scripts & Tools",
        "Rust Core Packages",
        "Knowledge/Runtime Corpus",
        "Architecture Highlights",
        "Key Technical Points",
        "Notable Design Patterns",
        "Current Entrypoints",
        "Knowledge & Corpus Layer (`mind/`)",
        "Development & Docs",
        "Overall Purpose",
        "Purpose",
        "Key Components",
        "Documentation",
        "Dependencies",
        "Architecture",
        "Structure",
        "Overview",
        "Summary",
        "Skills",
        "Status",
    ] {
        if let Some(rest) = content
            .strip_prefix(heading)
            .and_then(|rest| rest.strip_prefix(' '))
        {
            return format!("{prefix}{heading}\n{rest}");
        }
    }
    if let Some((heading, rest)) = content.split_once(" **") {
        return format!("{prefix}{heading}\n**{rest}");
    }
    line.to_string()
}

fn heading_parts(line: &str) -> Option<(&str, &str)> {
    for prefix in ["### ", "## ", "# "] {
        if let Some(content) = line.strip_prefix(prefix) {
            return Some((prefix, content));
        }
    }
    None
}

fn collapse_excess_blank_lines(text: &str) -> String {
    let mut output = String::new();
    let mut blank_count = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                output.push('\n');
            }
            continue;
        }
        blank_count = 0;
        output.push_str(line.trim_end());
        output.push('\n');
    }
    output.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::{format_agent_answer, split_markdown_table_lines, terminal_agent_answer};

    #[test]
    fn formats_flat_agent_markdown_into_scannable_blocks() {
        let raw = "## Arkey v2 / PKOS v0.2 Repository Analysis ### Overview A Rust-core migration project. --- ### Structure **Rust Core Packages**: - `arkey-core/` - `arkey-rs/` ### Key Design Decisions 1. Incremental migration 2. Reference preservation";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("## Arkey v2 / PKOS v0.2 Repository Analysis\n\n"));
        assert!(formatted.contains("### Overview\nA Rust-core migration project."));
        assert!(formatted.contains("\n---\n"));
        assert!(formatted.contains("\n- `arkey-core/`"));
        assert!(formatted.contains("\n1. Incremental migration"));
        assert!(formatted.ends_with('\n'));
    }

    #[test]
    fn formats_json_agent_answer_into_readable_markdown() {
        let raw = r#"{"repository":{"name":"arkey","version":"v2","workspace_structure":{"crates":["arkey-core","arkey-rs"],"ready":true}}}"#;
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("## Repository\n"));
        assert!(formatted.contains("- Name: arkey"));
        assert!(formatted.contains("- Version: v2"));
        assert!(formatted.contains("### Workspace Structure\n"));
        assert!(formatted.contains("- arkey-core"));
        assert!(formatted.contains("- arkey-rs"));
        assert!(formatted.contains("- Ready: true"));
        assert!(formatted.ends_with('\n'));
    }

    #[test]
    fn formats_json_array_agent_answer_into_readable_markdown() {
        let raw = r#"[{"name":"deepseek","passed":true},{"name":"minimax","passed":true}]"#;
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("1. Item 1"));
        assert!(formatted.contains("- Name: deepseek"));
        assert!(formatted.contains("2. Item 2"));
        assert!(formatted.contains("- Name: minimax"));
    }

    #[test]
    fn leaves_inline_horizontal_rule_text_alone() {
        let raw = "Use --- as a separator inside prose.";
        let formatted = format_agent_answer(raw);
        assert_eq!(formatted, "Use --- as a separator inside prose.\n");
    }

    #[test]
    fn splits_multi_digit_numbered_lists() {
        let raw = "9. item 10. next item";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("9. item\n\n10. next item"));
    }

    #[test]
    fn does_not_split_version_numbers_as_numbered_lists() {
        let raw = "Runtime is v2. It is ready.";
        let formatted = format_agent_answer(raw);
        assert_eq!(formatted, "Runtime is v2. It is ready.\n");
    }

    #[test]
    fn splits_common_agent_heading_bodies() {
        let raw = "### Overall Purpose This is a Rust migration. ### Architecture **Rust Workspace:** details";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("### Overall Purpose\nThis is a Rust migration."));
        assert!(formatted.contains("### Architecture\n**Rust Workspace:** details"));
    }

    #[test]
    fn splits_purpose_heading_body() {
        let raw = "### Purpose A standalone Rust CLI for querying models.";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("### Purpose\nA standalone Rust CLI for querying models."));
    }

    #[test]
    fn splits_flattened_markdown_table_rows() {
        let raw = "### Supported Commands | Command | Description | |---|---| | `chat` | Single-shot prompt | | `agent` | Autonomous run | Key inline code: `run_interactive`.";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("### Supported Commands\n| Command | Description |"));
        assert!(formatted.contains("\n|---|---|"));
        assert!(formatted.contains("\n| `chat` | Single-shot prompt |"));
        assert!(formatted.contains("\n| `agent` | Autonomous run |"));
        assert!(formatted.contains("\nKey inline code: `run_interactive`."));
    }

    #[test]
    fn split_markdown_table_lines_splits_overlong_table_line() {
        let raw = "| Command | Description | |---|---| | `chat` | Single-shot prompt | Key inline code: `run_interactive`.";
        let split = split_markdown_table_lines(raw);
        assert!(split.contains("| Command | Description |\n"));
        assert!(split.contains("|---|---|\n"));
        assert!(split.contains("| `chat` | Single-shot prompt |\n"));
        assert!(split.contains("\nKey inline code: `run_interactive`."));
    }

    #[test]
    fn splits_heading_body_before_bold_text() {
        let raw = "## Arkey v2 / PKOS v0.2 Repository Analysis **Purpose:** Rust migration.";
        let formatted = format_agent_answer(raw);
        assert!(formatted
            .contains("## Arkey v2 / PKOS v0.2 Repository Analysis\n**Purpose:** Rust migration."));
    }

    #[test]
    fn terminal_agent_answer_renders_tokyo_markdown_styles() {
        let raw = "## Result\n\n**text here** and `code`\n\n- item\n\n1. next\n\n---\n\n```text\n**raw**\n```";
        let rendered = terminal_agent_answer(raw);
        assert!(rendered.contains("\x1b[36;1mResult\x1b[0m"));
        assert!(rendered.contains("\x1b[1mtext here\x1b[0m"));
        assert!(rendered.contains("\x1b[38;2;125;207;255mcode\x1b[0m"));
        assert!(rendered.contains("\x1b[38;2;187;154;247m-\x1b[0m item"));
        assert!(rendered.contains("\x1b[38;2;187;154;247m1.\x1b[0m next"));
        assert!(rendered.contains("\x1b[90m----------------------------------------\x1b[0m"));
        assert!(rendered.contains("**raw**"));
        assert_eq!(
            strip_ansi_for_test(&rendered),
            "Result\n\ntext here and code\n\n- item\n\n1. next\n\n----------------------------------------\n\n```text\n**raw**\n```\n"
        );
    }

    #[test]
    fn terminal_agent_answer_handles_nested_inline_styles() {
        let rendered = terminal_agent_answer(
            "## **Important** Result\n\n**use the `run_shell` tool**\n\nSee https://example.com.",
        );

        assert!(!rendered.contains("**Important**"));
        assert!(rendered.contains("\x1b[36;1m\x1b[1mImportant\x1b[0m\x1b[36;1m Result\x1b[0m"));
        assert!(rendered.contains("\x1b[1muse the "));
        assert!(rendered.contains("\x1b[38;2;125;207;255mrun_shell\x1b[0m\x1b[1m tool\x1b[0m"));
        assert!(rendered.contains("\x1b[38;2;125;207;255mhttps://example.com\x1b[0m."));
        assert_eq!(
            strip_ansi_for_test(&rendered),
            "Important Result\n\nuse the run_shell tool\n\nSee https://example.com.\n"
        );
    }

    fn strip_ansi_for_test(text: &str) -> String {
        let mut output = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
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
