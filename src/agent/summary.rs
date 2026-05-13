use std::path::PathBuf;

use crate::safety::cap_text;

use super::decision::parse_decision;
use super::transcript::{self, TranscriptEntry};
use super::workspace::Workspace;

pub fn read_latest_transcript(
    root: impl Into<PathBuf>,
) -> Result<Option<(PathBuf, String)>, String> {
    transcript::read_latest_transcript(root, |root| {
        Workspace::new(root).map(|workspace| workspace.root)
    })
}

pub fn read_latest_transcript_summary(
    root: impl Into<PathBuf>,
) -> Result<Option<(PathBuf, String)>, String> {
    let Some((path, content)) = read_latest_transcript(root)? else {
        return Ok(None);
    };
    Ok(Some((path, summarize_transcript(&content)?)))
}

pub(super) fn summarize_transcript(content: &str) -> Result<String, String> {
    let (entries, partial) = parse_transcript_entries(content)?;
    let task = entries
        .iter()
        .find(|entry| entry.role == "task")
        .map(|entry| one_line(&entry.content, 240))
        .unwrap_or_else(|| "unavailable".to_string());
    let parser_notes = entries
        .iter()
        .filter(|entry| entry.role == "parser")
        .map(|entry| one_line(&entry.content, 240))
        .collect::<Vec<_>>();
    let tools = entries
        .iter()
        .filter_map(|entry| entry.role.strip_prefix("tool:"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    let assistant_turns = entries
        .iter()
        .filter(|entry| matches!(entry.role.as_str(), "assistant" | "assistant_retry"))
        .count();
    let final_outcome = entries
        .iter()
        .rev()
        .filter(|entry| matches!(entry.role.as_str(), "assistant" | "assistant_retry"))
        .find_map(|entry| parse_decision(&entry.content).ok())
        .and_then(|decision| {
            decision
                .final_answer
                .map(|answer| format!("final_answer: {}", one_line(&answer, 500)))
                .or_else(|| {
                    decision
                        .blocked
                        .map(|blocked| format!("blocked: {}", one_line(&blocked, 500)))
                })
        })
        .unwrap_or_else(|| "unavailable".to_string());

    let mut lines = vec![
        format!("task: {task}"),
        format!("entries: {}", entries.len()),
        format!("assistant turns: {assistant_turns}"),
    ];
    if partial {
        lines.push(
            "warning: transcript JSON is incomplete; summarized complete entries only".to_string(),
        );
    }
    lines.push(String::new());
    lines.push("parser:".to_string());
    if parser_notes.is_empty() {
        lines.push("- none".to_string());
    } else {
        lines.extend(parser_notes.into_iter().map(|note| format!("- {note}")));
    }
    lines.push(String::new());
    lines.push("tools:".to_string());
    if tools.is_empty() {
        lines.push("- none".to_string());
    } else {
        lines.extend(
            tools
                .into_iter()
                .enumerate()
                .map(|(index, tool)| format!("{}. {tool}", index + 1)),
        );
    }
    lines.push(String::new());
    lines.push(format!("final: {final_outcome}"));
    Ok(lines.join("\n"))
}

fn parse_transcript_entries(content: &str) -> Result<(Vec<TranscriptEntry>, bool), String> {
    match serde_json::from_str(content) {
        Ok(entries) => Ok((entries, false)),
        Err(err) => {
            let entries = salvage_complete_transcript_entries(content);
            if entries.is_empty() {
                Err(format!("invalid agent transcript JSON: {err}"))
            } else {
                Ok((entries, true))
            }
        }
    }
}

fn salvage_complete_transcript_entries(content: &str) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    let mut object_start = None;
    let mut brace_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (index, ch) in content.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if brace_depth == 0 {
                    object_start = Some(index);
                }
                brace_depth += 1;
            }
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if brace_depth == 0 {
                    if let Some(start) = object_start.take() {
                        if let Ok(entry) =
                            serde_json::from_str::<TranscriptEntry>(&content[start..=index])
                        {
                            entries.push(entry);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    entries
}

fn one_line(text: &str, max_chars: usize) -> String {
    cap_text(
        &text.split_whitespace().collect::<Vec<_>>().join(" "),
        max_chars,
    )
}
