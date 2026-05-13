use crate::cancel::CancellationToken;
use crate::safety::{cap_text, redact_text};

use super::decision::AgentDecision;
use super::transcript::{write_transcript, TranscriptEntry};
use super::types::{AgentOutcome, MAX_TOOL_CHARS};

pub(super) fn append_assistant_transcript_entry(
    transcript: &mut Vec<TranscriptEntry>,
    role: &str,
    raw: &str,
) -> String {
    let redacted_raw = cap_text(&redact_text(raw), MAX_TOOL_CHARS);
    transcript.push(TranscriptEntry {
        role: role.to_string(),
        content: redacted_raw.clone(),
    });
    redacted_raw
}

pub(super) fn sanitize_tool_observation(text: &str) -> String {
    let mut sanitized = String::new();
    for ch in text.chars() {
        match ch {
            '\n' | '\r' | '\t' => sanitized.push(ch),
            ch if ch.is_control() => {
                sanitized.push_str(&format!("\\u{{{:04X}}}", ch as u32));
            }
            ch => sanitized.push(ch),
        }
    }
    sanitized
}

pub(super) fn decision_has_action(decision: &AgentDecision) -> bool {
    decision.final_answer.is_some()
        || decision.blocked.is_some()
        || decision.tool.is_some()
        || !decision.tools.is_empty()
}

pub(super) fn append_parser_repair_notes(
    transcript: &mut Vec<TranscriptEntry>,
    repairs: &[&'static str],
) {
    for repair in repairs {
        transcript.push(TranscriptEntry {
            role: "parser".to_string(),
            content: format!("repaired decision JSON via {repair}"),
        });
    }
}

pub(super) fn append_no_action_retry_note(transcript: &mut Vec<TranscriptEntry>) {
    transcript.push(TranscriptEntry {
        role: "parser".to_string(),
        content: "retried no-action decision".to_string(),
    });
}

pub(super) fn append_no_action_fallback_note(transcript: &mut Vec<TranscriptEntry>, reason: &str) {
    transcript.push(TranscriptEntry {
        role: "parser".to_string(),
        content: "used no-action fallback after tool observations".to_string(),
    });
    transcript.push(TranscriptEntry {
        role: "assistant_retry".to_string(),
        content: format!(r#"{{"blocked":"{reason}"}}"#),
    });
}

pub(super) fn append_parse_failure_retry_note(transcript: &mut Vec<TranscriptEntry>, err: &str) {
    transcript.push(TranscriptEntry {
        role: "parser".to_string(),
        content: format!(
            "retried invalid decision JSON: {}",
            cap_text(&redact_text(err), 240)
        ),
    });
}

pub(super) fn decision_retry_prompt() -> String {
    "Your previous response was either invalid JSON or valid JSON without an actionable decision. Return exactly one JSON object with one of these shapes: {\"content\":\"final answer\",\"tool_calls\":null}, {\"content\":null,\"tool_calls\":[{\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"inspect_tree\",\"arguments\":\"{\\\"path\\\":\\\".\\\",\\\"depth\\\":2}\"}}]}, or {\"blocked\":\"short reason\"}. No prose outside JSON.".to_string()
}

pub(super) fn fail_with_transcript(
    root: &std::path::Path,
    transcript: &[TranscriptEntry],
    err: &str,
    raw: &str,
) -> String {
    let snippet = cap_text(&redact_text(raw), 400);
    match write_transcript(root, transcript) {
        Ok(path) => format!(
            "{err}\nraw snippet: {snippet}\ntranscript: {}",
            path.display()
        ),
        Err(write_err) => {
            format!("{err}\nraw snippet: {snippet}\ntranscript write failed: {write_err}")
        }
    }
}

pub(super) fn fail_no_action_with_transcript(
    root: &std::path::Path,
    transcript: &[TranscriptEntry],
) -> String {
    let err = "agent response did not include final_answer, blocked, or tool after retry";
    match write_transcript(root, transcript) {
        Ok(path) => format!("{err}\ntranscript: {}", path.display()),
        Err(write_err) => format!("{err}\ntranscript write failed: {write_err}"),
    }
}

pub(super) fn transcript_has_tool_results(transcript: &[TranscriptEntry]) -> bool {
    transcript
        .iter()
        .any(|entry| entry.role.starts_with("tool:"))
}

pub(super) fn no_action_fallback_outcome(
    root: &std::path::Path,
    transcript: &mut Vec<TranscriptEntry>,
    steps: usize,
) -> Result<AgentOutcome, String> {
    let reason =
        "model returned no actionable decision after retry; see transcript tool observations";
    append_no_action_fallback_note(transcript, reason);
    let transcript_path = write_transcript(root, transcript)?;
    Ok(AgentOutcome {
        answer: format!("blocked: {reason}"),
        steps,
        transcript_path,
    })
}

pub(super) fn check_cancelled(cancel: &Option<CancellationToken>) -> Result<(), String> {
    if let Some(cancel) = cancel {
        cancel.check()
    } else {
        Ok(())
    }
}
