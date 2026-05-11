use std::path::PathBuf;

use crate::cancel::CancellationToken;
use crate::provider::{self, assistant_message, system_message, user_message, Message};
use crate::safety::{cap_text, redact_text};

pub(crate) mod commit_audit;
mod decision;
mod read_tools;
mod transcript;
mod workspace;
mod write_tools;
#[allow(unused_imports)]
pub use decision::{parse_decision, AgentDecision, ToolCall};
use decision::{parse_decision_with_metadata, system_prompt};
use transcript::{write_transcript, TranscriptEntry};
use workspace::Workspace;

const MAX_TOOL_CHARS: usize = 12_000;
pub const DEFAULT_MAX_STEPS: usize = 1000;

pub struct AgentConfig {
    pub root: PathBuf,
    pub max_steps: usize,
}

impl AgentConfig {
    pub fn new(root: impl Into<PathBuf>, max_steps: usize) -> Self {
        Self {
            root: root.into(),
            max_steps: max_steps.max(1),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub answer: String,
    pub steps: usize,
    pub transcript_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    Interactive,
    Deny,
    Approved,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    ApproveForSession,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub step: usize,
    pub tool: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStep {
    pub step: usize,
    pub item: Option<usize>,
    pub total: usize,
    pub tool: String,
}

impl AgentStep {
    pub fn label(&self) -> String {
        match self.item {
            Some(item) => format!("{}.{}", self.step, item),
            None => self.step.to_string(),
        }
    }
}

pub fn run_agent(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
) -> Result<AgentOutcome, String> {
    run_agent_with_options(
        task,
        model,
        temperature,
        config,
        ApprovalMode::Interactive,
        |step| {
            eprintln!("agent step {}: {}", step.label(), step.tool);
        },
    )
}

pub fn run_agent_with_options(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
    approval_mode: ApprovalMode,
    on_step: impl FnMut(AgentStep),
) -> Result<AgentOutcome, String> {
    run_agent_with_approval_handler(
        task,
        model,
        temperature,
        config,
        approval_mode,
        on_step,
        |_| ApprovalDecision::Deny,
    )
}

pub fn run_agent_final_only(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
) -> Result<AgentOutcome, String> {
    run_agent_with_chat_handler(
        task,
        model,
        temperature,
        config,
        ApprovalMode::Interactive,
        |_| {},
        |_| ApprovalDecision::Deny,
        None,
        |messages, model, temperature| provider::chat_quiet(messages, model, temperature, None),
    )
}

pub fn run_agent_with_approval_handler(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
    approval_mode: ApprovalMode,
    mut on_step: impl FnMut(AgentStep),
    mut on_approval: impl FnMut(ApprovalRequest) -> ApprovalDecision,
) -> Result<AgentOutcome, String> {
    run_agent_with_chat_handler(
        task,
        model,
        temperature,
        config,
        approval_mode,
        &mut on_step,
        &mut on_approval,
        None,
        |messages, model, temperature| provider::chat(messages, model, temperature, None, false),
    )
}

#[allow(dead_code)]
pub fn run_agent_quiet_cache_with_approval_handler(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
    approval_mode: ApprovalMode,
    mut on_step: impl FnMut(AgentStep),
    mut on_approval: impl FnMut(ApprovalRequest) -> ApprovalDecision,
) -> Result<AgentOutcome, String> {
    run_agent_with_chat_handler(
        task,
        model,
        temperature,
        config,
        approval_mode,
        &mut on_step,
        &mut on_approval,
        None,
        |messages, model, temperature| provider::chat_quiet(messages, model, temperature, None),
    )
}

pub fn run_agent_quiet_cache_with_approval_handler_cancelled(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
    approval_mode: ApprovalMode,
    mut on_step: impl FnMut(AgentStep),
    mut on_approval: impl FnMut(ApprovalRequest) -> ApprovalDecision,
    cancel: CancellationToken,
) -> Result<AgentOutcome, String> {
    let chat_cancel = cancel.clone();
    run_agent_with_chat_handler(
        task,
        model,
        temperature,
        config,
        approval_mode,
        &mut on_step,
        &mut on_approval,
        Some(cancel),
        move |messages, model, temperature| {
            provider::chat_quiet_cancelled(messages, model, temperature, None, &chat_cancel)
        },
    )
}

fn run_agent_with_chat_handler(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
    approval_mode: ApprovalMode,
    mut on_step: impl FnMut(AgentStep),
    mut on_approval: impl FnMut(ApprovalRequest) -> ApprovalDecision,
    cancel: Option<CancellationToken>,
    mut chat: impl FnMut(&[Message], &str, Option<f32>) -> Result<String, String>,
) -> Result<AgentOutcome, String> {
    let workspace = Workspace::new(config.root)?;
    let prepared_task = commit_audit::prepare_task(task, &workspace.root);
    let mut messages = vec![
        system_message(system_prompt(&workspace.root)),
        user_message(format!("Task: {}", redact_text(&prepared_task))),
    ];
    let mut transcript = vec![TranscriptEntry {
        role: "task".to_string(),
        content: redact_text(&prepared_task),
    }];

    for step in 1..=config.max_steps {
        check_cancelled(&cancel)?;
        let mut raw = chat(&messages, model, temperature)?;
        check_cancelled(&cancel)?;
        let mut redacted_raw =
            append_assistant_transcript_entry(&mut transcript, "assistant", &raw);
        let mut parsed = match parse_decision_with_metadata(&raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                append_parse_failure_retry_note(&mut transcript, &err);
                messages.push(assistant_message(redacted_raw.clone()));
                messages.push(user_message(decision_retry_prompt()));
                check_cancelled(&cancel)?;
                raw = chat(&messages, model, temperature)?;
                check_cancelled(&cancel)?;
                redacted_raw =
                    append_assistant_transcript_entry(&mut transcript, "assistant_retry", &raw);
                match parse_decision_with_metadata(&raw) {
                    Ok(parsed) => parsed,
                    Err(retry_err) => {
                        return Err(fail_with_transcript(
                            &workspace.root,
                            &transcript,
                            &retry_err,
                            &raw,
                        ));
                    }
                }
            }
        };
        append_parser_repair_notes(&mut transcript, &parsed.repairs);
        let mut decision = parsed.decision;
        if !decision_has_action(&decision) {
            append_no_action_retry_note(&mut transcript);
            messages.push(assistant_message(redacted_raw.clone()));
            messages.push(user_message(decision_retry_prompt()));
            check_cancelled(&cancel)?;
            raw = chat(&messages, model, temperature)?;
            check_cancelled(&cancel)?;
            redacted_raw =
                append_assistant_transcript_entry(&mut transcript, "assistant_retry", &raw);
            parsed = match parse_decision_with_metadata(&raw) {
                Ok(parsed) => parsed,
                Err(err) => {
                    return Err(fail_with_transcript(
                        &workspace.root,
                        &transcript,
                        &err,
                        &raw,
                    ));
                }
            };
            append_parser_repair_notes(&mut transcript, &parsed.repairs);
            decision = parsed.decision;
        }
        if let Some(answer) = decision.final_answer {
            let transcript_path = write_transcript(&workspace.root, &transcript)?;
            return Ok(AgentOutcome {
                answer: cap_text(&redact_text(&answer), MAX_TOOL_CHARS),
                steps: step,
                transcript_path,
            });
        }
        if let Some(blocked) = decision.blocked {
            let transcript_path = write_transcript(&workspace.root, &transcript)?;
            return Ok(AgentOutcome {
                answer: format!(
                    "blocked: {}",
                    cap_text(&redact_text(&blocked), MAX_TOOL_CHARS)
                ),
                steps: step,
                transcript_path,
            });
        }
        let mut tools = decision.tools;
        if tools.is_empty() {
            if let Some(tool) = decision.tool {
                tools.push(tool);
            }
        }
        if tools.is_empty() {
            if transcript_has_tool_results(&transcript) {
                return no_action_fallback_outcome(&workspace.root, &mut transcript, step);
            }
            return Err(fail_no_action_with_transcript(&workspace.root, &transcript));
        }
        let mut result_sections = Vec::new();
        let total_tools = tools.len();
        for (index, tool) in tools.iter().enumerate() {
            on_step(AgentStep {
                step,
                item: (total_tools > 1).then_some(index + 1),
                total: total_tools,
                tool: tool.name.clone(),
            });
            let tool_approval_mode = if approval_mode == ApprovalMode::External {
                match approval_request(step, tool) {
                    Some(request) => match on_approval(request) {
                        ApprovalDecision::Approve | ApprovalDecision::ApproveForSession => {
                            ApprovalMode::Approved
                        }
                        ApprovalDecision::Deny => ApprovalMode::Deny,
                    },
                    None => ApprovalMode::Deny,
                }
            } else {
                approval_mode
            };
            check_cancelled(&cancel)?;
            let result = execute_tool(&workspace, tool, tool_approval_mode);
            check_cancelled(&cancel)?;
            let result_text = cap_text(
                &redact_text(&sanitize_tool_observation(&result)),
                MAX_TOOL_CHARS,
            );
            transcript.push(TranscriptEntry {
                role: format!("tool:{}", tool.name),
                content: result_text.clone(),
            });
            result_sections.push(format!(
                "Tool result for step {step}, item {} ({}):\n{result_text}",
                index + 1,
                tool.name
            ));
        }
        let combined_results = cap_text(&result_sections.join("\n\n"), MAX_TOOL_CHARS);
        messages.push(assistant_message(redacted_raw));
        messages.push(user_message(format!(
            "{combined_results}\nContinue with JSON only."
        )));
    }

    let transcript_path = write_transcript(&workspace.root, &transcript)?;
    Ok(AgentOutcome {
        answer: format!("blocked: reached max agent steps ({})", config.max_steps),
        steps: config.max_steps,
        transcript_path,
    })
}

fn append_assistant_transcript_entry(
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

fn sanitize_tool_observation(text: &str) -> String {
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

fn decision_has_action(decision: &AgentDecision) -> bool {
    decision.final_answer.is_some()
        || decision.blocked.is_some()
        || decision.tool.is_some()
        || !decision.tools.is_empty()
}

fn append_parser_repair_notes(transcript: &mut Vec<TranscriptEntry>, repairs: &[&'static str]) {
    for repair in repairs {
        transcript.push(TranscriptEntry {
            role: "parser".to_string(),
            content: format!("repaired decision JSON via {repair}"),
        });
    }
}

fn append_no_action_retry_note(transcript: &mut Vec<TranscriptEntry>) {
    transcript.push(TranscriptEntry {
        role: "parser".to_string(),
        content: "retried no-action decision".to_string(),
    });
}

fn append_no_action_fallback_note(transcript: &mut Vec<TranscriptEntry>, reason: &str) {
    transcript.push(TranscriptEntry {
        role: "parser".to_string(),
        content: "used no-action fallback after tool observations".to_string(),
    });
    transcript.push(TranscriptEntry {
        role: "assistant_retry".to_string(),
        content: format!(r#"{{"blocked":"{reason}"}}"#),
    });
}

fn append_parse_failure_retry_note(transcript: &mut Vec<TranscriptEntry>, err: &str) {
    transcript.push(TranscriptEntry {
        role: "parser".to_string(),
        content: format!(
            "retried invalid decision JSON: {}",
            cap_text(&redact_text(err), 240)
        ),
    });
}

fn decision_retry_prompt() -> String {
    "Your previous response was either invalid JSON or valid JSON without an actionable decision. Return exactly one JSON object with one of these shapes: {\"content\":\"final answer\",\"tool_calls\":null}, {\"content\":null,\"tool_calls\":[{\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"inspect_tree\",\"arguments\":\"{\\\"path\\\":\\\".\\\",\\\"depth\\\":2}\"}}]}, or {\"blocked\":\"short reason\"}. No prose outside JSON.".to_string()
}

fn fail_with_transcript(
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

fn fail_no_action_with_transcript(
    root: &std::path::Path,
    transcript: &[TranscriptEntry],
) -> String {
    let err = "agent response did not include final_answer, blocked, or tool after retry";
    match write_transcript(root, transcript) {
        Ok(path) => format!("{err}\ntranscript: {}", path.display()),
        Err(write_err) => format!("{err}\ntranscript write failed: {write_err}"),
    }
}

fn transcript_has_tool_results(transcript: &[TranscriptEntry]) -> bool {
    transcript
        .iter()
        .any(|entry| entry.role.starts_with("tool:"))
}

fn no_action_fallback_outcome(
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

fn check_cancelled(cancel: &Option<CancellationToken>) -> Result<(), String> {
    if let Some(cancel) = cancel {
        cancel.check()
    } else {
        Ok(())
    }
}

fn approval_request(step: usize, call: &ToolCall) -> Option<ApprovalRequest> {
    match call.name.as_str() {
        "run_shell" => {
            let command = call
                .arguments
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing command>");
            let cwd = call
                .arguments
                .get("cwd")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            let reason = call
                .arguments
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("no reason provided");
            Some(ApprovalRequest {
                step,
                tool: call.name.clone(),
                summary: format!(
                    "approval required: run_shell\ncwd: {cwd}\nreason: {reason}\ncommand: {command}\nType yes run to approve, n to deny.\n"
                ),
            })
        }
        "propose_patch" => {
            let path = call
                .arguments
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing path>");
            let reason = call
                .arguments
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("no reason provided");
            let find = call
                .arguments
                .get("find")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing find>");
            let replace = call
                .arguments
                .get("replace")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing replace>");
            Some(ApprovalRequest {
                step,
                tool: call.name.clone(),
                summary: format!(
                    "approval required: propose_patch\npath: {path}\nreason: {reason}\n--- find ---\n{find}\n--- replace ---\n{replace}\nType yes apply to approve, n to deny.\n"
                ),
            })
        }
        _ => None,
    }
}

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

fn summarize_transcript(content: &str) -> Result<String, String> {
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

fn execute_tool(workspace: &Workspace, call: &ToolCall, approval_mode: ApprovalMode) -> String {
    match call.name.as_str() {
        "list_files" => read_tools::list_files(workspace, call),
        "read_file" => read_tools::read_file(workspace, call),
        "search_files" => read_tools::search_files(workspace, call),
        "inspect_tree" => read_tools::inspect_tree(workspace, call),
        "run_shell" => write_tools::run_shell(workspace, call, approval_mode),
        "propose_patch" => write_tools::propose_patch(workspace, call, approval_mode),
        other => format!("error: unknown agent tool `{other}`"),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::io;

    use serde_json::json;

    use crate::cancel::CancellationToken;
    use crate::provider::PROVIDER_STATE_DIR;

    use super::workspace::Workspace;
    use super::write_tools::{apply_prepared_patch, prepare_patch};
    use super::{
        append_no_action_retry_note, append_parser_repair_notes, parse_decision,
        parse_decision_with_metadata, system_prompt, write_transcript, AgentConfig,
        ApprovalDecision, ApprovalMode, ToolCall, TranscriptEntry, DEFAULT_MAX_STEPS,
    };

    fn execute_tool(workspace: &Workspace, call: &ToolCall) -> String {
        super::execute_tool(workspace, call, ApprovalMode::Interactive)
    }

    #[test]
    fn parses_tool_schema() {
        let decision = parse_decision(
            r#"{"thought":"need context","tool":{"name":"read_file","arguments":{"path":"src/main.rs"}}}"#,
        )
        .unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.arguments["path"], "src/main.rs");
    }

    #[test]
    fn agent_step_label_uses_substeps_only_for_batches() {
        assert_eq!(
            super::AgentStep {
                step: 1,
                item: None,
                total: 1,
                tool: "list_files".to_string(),
            }
            .label(),
            "1"
        );
        assert_eq!(
            super::AgentStep {
                step: 2,
                item: Some(1),
                total: 2,
                tool: "read_file".to_string(),
            }
            .label(),
            "2.1"
        );
    }

    #[test]
    fn system_prompt_includes_markdown_final_answer_style() {
        let prompt = system_prompt(std::path::Path::new("/tmp/workspace"));
        assert!(prompt.contains("Final answer style:"));
        assert!(prompt.contains("Put polished Markdown inside the `content` string."));
        assert!(prompt.contains("Start substantial answers with a `##` heading"));
        assert!(prompt.contains("compact Markdown tables"));
        assert!(prompt.contains("For reviews, lead with findings before summary."));
    }

    #[test]
    fn parses_openai_style_tool_call() {
        let decision = parse_decision(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}}]}"#,
        )
        .unwrap();
        assert_eq!(decision.tools.len(), 1);
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.arguments["path"], "src/main.rs");
    }

    #[test]
    fn parses_openai_style_multiple_tool_calls_in_order() {
        let decision = parse_decision(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"list_files","arguments":"{\"path\":\".\"}"}},{"id":"call_2","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]}"#,
        )
        .unwrap();
        assert_eq!(decision.tools.len(), 2);
        assert_eq!(decision.tool.as_ref().unwrap().name, "list_files");
        assert_eq!(decision.tools[0].arguments["path"], ".");
        assert_eq!(decision.tools[1].name, "read_file");
        assert_eq!(decision.tools[1].arguments["path"], "README.md");
    }

    #[test]
    fn clean_openai_parse_has_no_repair_metadata() {
        let parsed = parse_decision_with_metadata(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"list_files","arguments":"{\"path\":\".\"}"}}]}"#,
        )
        .unwrap();
        assert!(parsed.repairs.is_empty());
        assert_eq!(parsed.decision.tool.unwrap().name, "list_files");
    }

    #[test]
    fn skips_malformed_openai_tool_call_after_repairing_extra_brace() {
        let decision = parse_decision(
            r#"{"content":null,"tool_calls":[{"id":"call_19","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"docs/framework-port.md\"}"}},{"id":"call_20","type":"function","function":"{\"path\":\"docs/mind-qa-scope.md\"}"}}]}"#,
        )
        .unwrap();
        assert_eq!(decision.tools.len(), 1);
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.arguments["path"], "docs/framework-port.md");
    }

    #[test]
    fn records_extra_brace_repair_metadata() {
        let parsed = parse_decision_with_metadata(
            r#"{"content":null,"tool_calls":[{"id":"call_19","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"docs/framework-port.md\"}"}},{"id":"call_20","type":"function","function":"{\"path\":\"docs/mind-qa-scope.md\"}"}}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.repairs, vec!["extra_brace"]);
    }

    #[test]
    fn repairs_trailing_characters_in_openai_tool_arguments_string() {
        let decision = parse_decision(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{\"depth\":2,\"path\":\"pkos_v0.2\"}}"}}]}"#,
        )
        .unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "inspect_tree");
        assert_eq!(tool.arguments["depth"], 2);
        assert_eq!(tool.arguments["path"], "pkos_v0.2");
    }

    #[test]
    fn records_trailing_arguments_repair_metadata() {
        let parsed = parse_decision_with_metadata(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{\"depth\":2,\"path\":\"pkos_v0.2\"}}"}}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.repairs, vec!["arguments_trailing_json"]);
    }

    #[test]
    fn repairs_unclosed_terminal_string_in_openai_tool_arguments_string() {
        let decision = parse_decision(
            r#"{"content":null,"tool_calls":[{"id":"call_read_runtime","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"arkey-core/src/runtime.rs}"}}]}"#,
        )
        .unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.arguments["path"], "arkey-core/src/runtime.rs");
    }

    #[test]
    fn records_unclosed_arguments_repair_metadata() {
        let parsed = parse_decision_with_metadata(
            r#"{"content":null,"tool_calls":[{"id":"call_read_runtime","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"arkey-core/src/runtime.rs}"}}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.repairs, vec!["arguments_unclosed_terminal_string"]);
    }

    #[test]
    fn parses_openai_style_final_content() {
        let decision = parse_decision(r#"{"content":"done","tool_calls":null}"#).unwrap();
        assert_eq!(decision.final_answer.as_deref(), Some("done"));
    }

    #[test]
    fn parses_common_final_answer_aliases() {
        let decision = parse_decision(r#"{"answer":"done"}"#).unwrap();
        assert_eq!(decision.final_answer.as_deref(), Some("done"));

        let decision = parse_decision(r#"{"response":"also done"}"#).unwrap();
        assert_eq!(decision.final_answer.as_deref(), Some("also done"));

        let decision = parse_decision(r#"{"result":"finished"}"#).unwrap();
        assert_eq!(decision.final_answer.as_deref(), Some("finished"));
    }

    #[test]
    fn repairs_unescaped_quotes_in_openai_style_final_content() {
        let decision = parse_decision(
            r#"{"content":"Repo says "knowledge as code" works\nDone","tool_calls":null}"#,
        )
        .unwrap();
        assert_eq!(
            decision.final_answer.as_deref(),
            Some("Repo says \"knowledge as code\" works\nDone")
        );
    }

    #[test]
    fn records_unescaped_final_content_repair_metadata() {
        let parsed = parse_decision_with_metadata(
            r#"{"content":"Repo says "knowledge as code" works\nDone","tool_calls":null}"#,
        )
        .unwrap();
        assert_eq!(parsed.repairs, vec!["unescaped_final_content"]);
    }

    #[test]
    fn repairs_missing_key_quote_before_tool_calls() {
        // Provider returns ,tool_calls":null} with the opening key quote missing.
        let raw = r#"{"content":"Repo summary: tables and files.",tool_calls":null}"#;
        let parsed = parse_decision_with_metadata(raw).unwrap();
        assert_eq!(parsed.repairs, vec!["missing_key_quote_tool_calls"]);
        assert_eq!(
            parsed.decision.final_answer.as_deref(),
            Some("Repo summary: tables and files.")
        );
    }

    #[test]
    fn placeholder_content_does_not_mask_real_decision_fields() {
        let decision =
            parse_decision(r#"{"content":"answer with concrete findings","blocked":"wait"}"#)
                .unwrap();
        assert_eq!(decision.final_answer, None);
        assert_eq!(decision.blocked.as_deref(), Some("wait"));

        let decision =
            parse_decision(r#"{"content":"answer with concrete findings","final_answer":"done"}"#)
                .unwrap();
        assert_eq!(decision.final_answer.as_deref(), Some("done"));
    }

    #[test]
    fn parses_first_json_object_before_trailing_prose() {
        let decision = parse_decision(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"list_files","arguments":"{\"path\":\".\"}"}}]}
I will list the files now."#,
        )
        .unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "list_files");
        assert_eq!(tool.arguments["path"], ".");
    }

    #[test]
    fn repairs_missing_comma_in_function_object() {
        let missing_comma = r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree" "arguments":"{\"path\":\".\",\"depth\":2}"}}]}"#;
        let decision = parse_decision(missing_comma).unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "inspect_tree");
        assert_eq!(tool.arguments["path"], ".");
        assert_eq!(tool.arguments["depth"], 2);
    }

    #[test]
    fn records_missing_comma_repair_metadata() {
        let missing_comma = r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree" "arguments":"{\"path\":\".\",\"depth\":2}"}}]}"#;
        let parsed = parse_decision_with_metadata(missing_comma).unwrap();
        assert_eq!(parsed.repairs, vec!["missing_comma"]);
    }

    #[test]
    fn repairs_malformed_arguments_string() {
        let malformed = r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{\"path\":".","depth":3}"}}]}"#;
        assert!(serde_json::from_str::<serde_json::Value>(malformed).is_err());
        let decision = parse_decision(malformed).unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "inspect_tree");
        assert_eq!(tool.arguments["path"], ".");
        assert_eq!(tool.arguments["depth"], 3);
    }

    #[test]
    fn records_malformed_arguments_repair_metadata() {
        let malformed = r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{\"path\":".","depth":3}"}}]}"#;
        let parsed = parse_decision_with_metadata(malformed).unwrap();
        assert_eq!(parsed.repairs, vec!["malformed_arguments_string"]);
    }

    #[test]
    fn repairs_multiple_malformed_arguments_strings() {
        let malformed = r#"{"content":null,"tool_calls":[{"id":"call_2","type":"function","function":{"name":"inspect_tree","arguments":"{\"path":"arkey-core/src","depth":1}"}},{"id":"call_3","type":"function","function":{"name":"inspect_tree","arguments":"{\"path":"arkey-rs/src","depth":1}"}}]}"#;
        let parsed = parse_decision_with_metadata(malformed).unwrap();
        assert_eq!(parsed.repairs, vec!["malformed_arguments_string"]);
        assert_eq!(parsed.decision.tools.len(), 2);
        assert_eq!(parsed.decision.tools[0].arguments["path"], "arkey-core/src");
        assert_eq!(parsed.decision.tools[1].arguments["path"], "arkey-rs/src");
    }

    #[test]
    fn parser_repair_notes_are_sanitized_transcript_entries() {
        let mut transcript = Vec::new();
        append_parser_repair_notes(&mut transcript, &["extra_brace", "arguments_trailing_json"]);
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].role, "parser");
        assert_eq!(
            transcript[0].content,
            "repaired decision JSON via extra_brace"
        );
        assert_eq!(
            transcript[1].content,
            "repaired decision JSON via arguments_trailing_json"
        );
        assert!(!transcript[0].content.contains('{'));
    }

    #[test]
    fn no_action_retry_note_is_sanitized_transcript_entry() {
        let mut transcript = Vec::new();
        append_no_action_retry_note(&mut transcript);
        assert_eq!(transcript.len(), 1);
        assert_eq!(transcript[0].role, "parser");
        assert_eq!(transcript[0].content, "retried no-action decision");
        assert!(!transcript[0].content.contains('{'));
    }

    #[test]
    fn parse_failure_retries_once_and_succeeds() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-parse-retry-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut responses = VecDeque::from([
            "not json".to_string(),
            r#"{"content":"recovered","tool_calls":null}"#.to_string(),
        ]);
        let outcome = super::run_agent_with_chat_handler(
            "test",
            "model",
            None,
            AgentConfig::new(root.clone(), 3),
            ApprovalMode::Deny,
            |_| {},
            |_| ApprovalDecision::Deny,
            None,
            |_, _, _| Ok(responses.pop_front().unwrap()),
        )
        .unwrap();
        assert_eq!(outcome.answer, "recovered");
        let transcript = fs::read_to_string(outcome.transcript_path).unwrap();
        assert!(transcript.contains("retried invalid decision JSON"));
        assert!(transcript.contains("assistant_retry"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_action_decision_retries_once_and_succeeds() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-no-action-retry-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut responses = VecDeque::from([
            r#"{"thought":"still thinking"}"#.to_string(),
            r#"{"content":"done","tool_calls":null}"#.to_string(),
        ]);
        let outcome = super::run_agent_with_chat_handler(
            "test",
            "model",
            None,
            AgentConfig::new(root.clone(), 3),
            ApprovalMode::Deny,
            |_| {},
            |_| ApprovalDecision::Deny,
            None,
            |_, _, _| Ok(responses.pop_front().unwrap()),
        )
        .unwrap();
        assert_eq!(outcome.answer, "done");
        let transcript = fs::read_to_string(outcome.transcript_path).unwrap();
        assert!(transcript.contains("retried no-action decision"));
        assert!(transcript.contains("assistant_retry"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn retry_parse_failure_writes_transcript() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-parse-retry-fail-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut responses = VecDeque::from(["not json".to_string(), "still not json".to_string()]);
        let err = super::run_agent_with_chat_handler(
            "test",
            "model",
            None,
            AgentConfig::new(root.clone(), 3),
            ApprovalMode::Deny,
            |_| {},
            |_| ApprovalDecision::Deny,
            None,
            |_, _, _| Ok(responses.pop_front().unwrap()),
        )
        .unwrap_err();
        assert!(err.contains("agent response was not JSON"));
        assert!(err.contains("raw snippet: still not json"));
        assert!(err.contains("transcript:"));
        let latest = super::read_latest_transcript(root.clone())
            .unwrap()
            .unwrap();
        assert!(latest.1.contains("retried invalid decision JSON"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn retry_no_action_failure_writes_clear_error() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-no-action-retry-fail-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let mut responses = VecDeque::from([
            r#"{"thought":"one"}"#.to_string(),
            r#"{"thought":"two"}"#.to_string(),
        ]);
        let err = super::run_agent_with_chat_handler(
            "test",
            "model",
            None,
            AgentConfig::new(root.clone(), 3),
            ApprovalMode::Deny,
            |_| {},
            |_| ApprovalDecision::Deny,
            None,
            |_, _, _| Ok(responses.pop_front().unwrap()),
        )
        .unwrap_err();
        assert!(err
            .contains("agent response did not include final_answer, blocked, or tool after retry"));
        assert!(err.contains("transcript:"));
        let latest = super::read_latest_transcript(root.clone())
            .unwrap()
            .unwrap();
        assert!(latest.1.contains("retried no-action decision"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn no_action_after_tool_results_returns_blocked_fallback() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-no-action-after-tools-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("README.md"), "fixture").unwrap();
        let mut responses = VecDeque::from([
            r#"{"tool":{"name":"list_files","arguments":{"path":"."}}}"#.to_string(),
            r#"{"thought":"I have enough context"}"#.to_string(),
            r#"{"thought":"still no final"}"#.to_string(),
        ]);
        let outcome = super::run_agent_with_chat_handler(
            "test",
            "model",
            None,
            AgentConfig::new(root.clone(), 3),
            ApprovalMode::Deny,
            |_| {},
            |_| ApprovalDecision::Deny,
            None,
            |_, _, _| Ok(responses.pop_front().unwrap()),
        )
        .unwrap();
        assert!(outcome
            .answer
            .contains("blocked: model returned no actionable decision"));
        let transcript = fs::read_to_string(&outcome.transcript_path).unwrap();
        assert!(transcript.contains("tool:list_files"));
        assert!(transcript.contains("used no-action fallback after tool observations"));
        let summary = super::read_latest_transcript_summary(root.clone())
            .unwrap()
            .unwrap()
            .1;
        assert!(summary.contains("final: blocked: model returned no actionable decision"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn multi_tool_response_reports_substeps() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-substep-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("README.md"), "fixture").unwrap();
        let mut responses = VecDeque::from([
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"list_files","arguments":"{\"path\":\".\"}"}},{"id":"call_2","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]}"#.to_string(),
            r#"{"content":"done","tool_calls":null}"#.to_string(),
        ]);
        let mut steps = Vec::new();
        let outcome = super::run_agent_with_chat_handler(
            "test",
            "model",
            None,
            AgentConfig::new(root.clone(), 3),
            ApprovalMode::Deny,
            |step| steps.push(step),
            |_| ApprovalDecision::Deny,
            None,
            |_, _, _| Ok(responses.pop_front().unwrap()),
        )
        .unwrap();
        assert_eq!(outcome.answer, "done");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].label(), "1.1");
        assert_eq!(steps[0].tool, "list_files");
        assert_eq!(steps[1].label(), "1.2");
        assert_eq!(steps[1].tool, "read_file");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cancelled_agent_stops_before_chat_call() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-cancel-before-chat-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();

        let err = super::run_agent_with_chat_handler(
            "test",
            "model",
            None,
            AgentConfig::new(root.clone(), 3),
            ApprovalMode::Deny,
            |_| {},
            |_| ApprovalDecision::Deny,
            Some(cancel),
            |_, _, _| panic!("chat should not run after cancellation"),
        )
        .unwrap_err();

        assert_eq!(err, crate::cancel::CANCELLED);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sanitizes_tool_observation_control_characters() {
        assert_eq!(
            super::sanitize_tool_observation("ok\0bad\u{001F}\nnext\tcol"),
            "ok\\u{0000}bad\\u{001F}\nnext\tcol"
        );
    }

    #[test]
    fn repairs_malformed_arguments_string_with_braces_in_value() {
        let malformed = r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md","pattern":"{heading}","depth":1}"}}]}"#;
        assert!(serde_json::from_str::<serde_json::Value>(malformed).is_err());
        let decision = parse_decision(malformed).unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.arguments["path"], "README.md");
        assert_eq!(tool.arguments["pattern"], "{heading}");
        assert_eq!(tool.arguments["depth"], 1);
    }

    #[test]
    fn repairs_unescaped_arguments_object_string() {
        let malformed = r###"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{"path":".","depth":3}"}}]}"###;
        assert!(serde_json::from_str::<serde_json::Value>(malformed).is_err());
        let decision = parse_decision(malformed).unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "inspect_tree");
        assert_eq!(tool.arguments["path"], ".");
        assert_eq!(tool.arguments["depth"], 3);
    }

    #[test]
    fn default_max_steps_matches_long_running_agent_budget() {
        assert_eq!(DEFAULT_MAX_STEPS, 1000);
    }

    #[test]
    fn builds_shell_approval_request() {
        let call = ToolCall {
            name: "run_shell".to_string(),
            arguments: json!({
                "command": "pwd",
                "cwd": ".",
                "reason": "check location"
            }),
        };
        let request = super::approval_request(2, &call).unwrap();
        assert_eq!(request.step, 2);
        assert_eq!(request.tool, "run_shell");
        assert!(request.summary.contains("command: pwd"));
    }

    #[test]
    fn external_approval_can_approve_shell_once() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-external-approval-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let call = ToolCall {
            name: "run_shell".to_string(),
            arguments: json!({
                "command": "printf APPROVED",
                "cwd": ".",
                "reason": "test"
            }),
        };
        let workspace = Workspace::new(root.clone()).unwrap();
        let result = super::execute_tool(&workspace, &call, ApprovalMode::Approved);
        assert!(result.contains("APPROVED"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parses_embedded_json() {
        let decision = parse_decision(
            r#"```json
{"final_answer":"done"}
```"#,
        )
        .unwrap();
        assert_eq!(decision.final_answer.as_deref(), Some("done"));
    }

    #[test]
    fn rejects_parent_paths() {
        let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
        assert!(workspace.resolve_existing("../Cargo.toml").is_err());
    }

    #[test]
    fn unknown_tool_returns_observation_error() {
        let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
        let result = execute_tool(
            &workspace,
            &ToolCall {
                name: "fetch_url".to_string(),
                arguments: json!({"url":"https://example.com"}),
            },
        );
        assert!(result.contains("unknown agent tool"));
    }

    #[test]
    fn run_shell_denies_without_interactive_approval() {
        let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
        let result = execute_tool(
            &workspace,
            &ToolCall {
                name: "run_shell".to_string(),
                arguments: json!({"command":"pwd","cwd":".","reason":"test"}),
            },
        );
        assert!(result.contains("requires explicit interactive approval"));
    }

    #[test]
    fn deny_approval_mode_blocks_shell_without_prompting() {
        let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
        let result = super::execute_tool(
            &workspace,
            &ToolCall {
                name: "run_shell".to_string(),
                arguments: json!({"command":"pwd","cwd":".","reason":"test"}),
            },
            ApprovalMode::Deny,
        );
        assert!(result.contains("requires explicit interactive approval"));
    }

    #[test]
    fn builds_patch_approval_request() {
        let request = super::approval_request(
            2,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"note.txt",
                    "find":"old",
                    "replace":"new",
                    "reason":"test patch approval"
                }),
            },
        )
        .unwrap();
        assert_eq!(request.step, 2);
        assert_eq!(request.tool, "propose_patch");
        assert!(request.summary.contains("approval required: propose_patch"));
        assert!(request.summary.contains("path: note.txt"));
        assert!(request.summary.contains("reason: test patch approval"));
        assert!(request.summary.contains("--- find ---\nold"));
        assert!(request.summary.contains("--- replace ---\nnew"));
    }

    #[test]
    fn approved_approval_mode_applies_patch_once() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-patch-approved-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), "old").unwrap();
        let workspace = Workspace::new(root.clone()).unwrap();
        let result = super::execute_tool(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"note.txt",
                    "find":"old",
                    "replace":"new",
                    "reason":"test approved patch"
                }),
            },
            ApprovalMode::Approved,
        );
        assert!(result.contains("ok: patched note.txt"));
        assert_eq!(fs::read_to_string(root.join("note.txt")).unwrap(), "new");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn missing_path_returns_observation_error() {
        let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
        let result = execute_tool(
            &workspace,
            &ToolCall {
                name: "read_file".to_string(),
                arguments: json!({}),
            },
        );
        assert!(result.contains("missing non-empty `path`"));
    }

    #[test]
    fn propose_patch_denies_without_interactive_approval() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-patch-deny-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), "old").unwrap();
        let workspace = Workspace::new(root.clone()).unwrap();
        let result = execute_tool(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"note.txt",
                    "find":"old",
                    "replace":"new",
                    "reason":"test"
                }),
            },
        );
        assert!(result.contains("requires explicit interactive approval"));
        assert_eq!(fs::read_to_string(root.join("note.txt")).unwrap(), "old");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn propose_patch_requires_args() {
        let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
        let result = execute_tool(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({"path":"Cargo.toml","find":"[package]","replace":"[package]"}),
            },
        );
        assert!(result.contains("missing non-empty `reason`"));
    }

    #[test]
    fn propose_patch_rejects_path_escape_and_missing_file() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-patch-path-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let workspace = Workspace::new(root.clone()).unwrap();
        let escaped = execute_tool(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"../outside.txt",
                    "find":"a",
                    "replace":"b",
                    "reason":"test"
                }),
            },
        );
        assert!(escaped.contains("path must stay inside workspace root"));
        let missing = execute_tool(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"missing.txt",
                    "find":"a",
                    "replace":"b",
                    "reason":"test"
                }),
            },
        );
        assert!(missing.contains("No such file") || missing.contains("not found"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn prepare_and_apply_patch_replaces_exact_unique_text() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-patch-apply-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), "alpha\nbeta\ngamma\n").unwrap();
        let workspace = Workspace::new(root.clone()).unwrap();
        let patch = prepare_patch(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"note.txt",
                    "find":"beta\n",
                    "replace":"delta\n",
                    "reason":"test exact replacement"
                }),
            },
        )
        .unwrap();
        assert_eq!(patch.display_path, "note.txt");
        apply_prepared_patch(&patch).unwrap();
        assert_eq!(
            fs::read_to_string(root.join("note.txt")).unwrap(),
            "alpha\ndelta\ngamma\n"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_ambiguous_replacement() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-patch-ambiguous-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), "same same").unwrap();
        let workspace = Workspace::new(root.clone()).unwrap();
        let err = prepare_patch(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"note.txt",
                    "find":"same",
                    "replace":"other",
                    "reason":"test"
                }),
            },
        )
        .unwrap_err();
        assert!(err.contains("more than once"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn apply_patch_rejects_changed_file() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-patch-race-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("note.txt"), "old").unwrap();
        let workspace = Workspace::new(root.clone()).unwrap();
        let patch = prepare_patch(
            &workspace,
            &ToolCall {
                name: "propose_patch".to_string(),
                arguments: json!({
                    "path":"note.txt",
                    "find":"old",
                    "replace":"new",
                    "reason":"test"
                }),
            },
        )
        .unwrap();
        fs::write(root.join("note.txt"), "changed").unwrap();
        let err = apply_prepared_patch(&patch).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn inspect_tree_skips_node_modules() {
        let root =
            std::env::temp_dir().join(format!("deepseek-agent-tree-test-{}", std::process::id()));
        fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        fs::write(root.join("README.md"), "hello").unwrap();
        let workspace = Workspace::new(root.clone()).unwrap();
        let result = execute_tool(
            &workspace,
            &ToolCall {
                name: "inspect_tree".to_string(),
                arguments: json!({"path":".","depth":2}),
            },
        );
        assert!(result.contains("README.md"));
        assert!(!result.contains("node_modules"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn summarizes_transcript_without_raw_payloads() {
        let content = serde_json::to_string(&vec![
            TranscriptEntry {
                role: "task".to_string(),
                content: "analyze this repo".to_string(),
            },
            TranscriptEntry {
                role: "assistant".to_string(),
                content: r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{\"path\":\".\",\"depth\":2}"}}]}"#.to_string(),
            },
            TranscriptEntry {
                role: "parser".to_string(),
                content: "repaired decision JSON via extra_brace".to_string(),
            },
            TranscriptEntry {
                role: "tool:inspect_tree".to_string(),
                content: "README.md".to_string(),
            },
            TranscriptEntry {
                role: "assistant_retry".to_string(),
                content: r#"{"content":"done with findings","tool_calls":null}"#.to_string(),
            },
        ])
        .unwrap();
        let summary = super::summarize_transcript(&content).unwrap();
        assert!(summary.contains("task: analyze this repo"));
        assert!(summary.contains("entries: 5"));
        assert!(summary.contains("assistant turns: 2"));
        assert!(summary.contains("- repaired decision JSON via extra_brace"));
        assert!(summary.contains("1. inspect_tree"));
        assert!(summary.contains("final: final_answer: done with findings"));
        assert!(!summary.contains(r#""tool_calls""#));
    }

    #[test]
    fn summarizes_blocked_transcript() {
        let content = serde_json::to_string(&vec![
            TranscriptEntry {
                role: "task".to_string(),
                content: "dangerous task".to_string(),
            },
            TranscriptEntry {
                role: "assistant".to_string(),
                content: r#"{"blocked":"needs approval"}"#.to_string(),
            },
        ])
        .unwrap();
        let summary = super::summarize_transcript(&content).unwrap();
        assert!(summary.contains("final: blocked: needs approval"));
    }

    #[test]
    fn summarizes_malformed_assistant_content() {
        let content = serde_json::to_string(&vec![
            TranscriptEntry {
                role: "task".to_string(),
                content: "weird task".to_string(),
            },
            TranscriptEntry {
                role: "assistant".to_string(),
                content: "plain text".to_string(),
            },
        ])
        .unwrap();
        let summary = super::summarize_transcript(&content).unwrap();
        assert!(summary.contains("assistant turns: 1"));
        assert!(summary.contains("final: unavailable"));
    }

    #[test]
    fn summarizes_complete_entries_from_truncated_transcript() {
        let content = r#"[
  {"role":"task","content":"analyze this repo"},
  {"role":"tool:read_file","content":"ok"},
  {"role":"assistant","content":"unfinished"#;
        let summary = super::summarize_transcript(content).unwrap();
        assert!(summary.contains("task: analyze this repo"));
        assert!(summary.contains("entries: 2"));
        assert!(summary
            .contains("warning: transcript JSON is incomplete; summarized complete entries only"));
        assert!(summary.contains("1. read_file"));
    }

    #[test]
    fn writes_transcript_under_workspace() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-transcript-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = write_transcript(
            &root,
            &[TranscriptEntry {
                role: "task".to_string(),
                content: "hello".to_string(),
            }],
        )
        .unwrap();
        assert!(path.starts_with(root.join(PROVIDER_STATE_DIR).join("agent-transcripts")));
        assert!(path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn writes_valid_transcript_json_with_large_entries() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-large-transcript-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let path = write_transcript(
            &root,
            &[
                TranscriptEntry {
                    role: "task".to_string(),
                    content: "large transcript".to_string(),
                },
                TranscriptEntry {
                    role: "tool:read_file".to_string(),
                    content: "x".repeat(100_000),
                },
                TranscriptEntry {
                    role: "assistant".to_string(),
                    content: r#"{"content":"done","tool_calls":null}"#.to_string(),
                },
            ],
        )
        .unwrap();
        let content = fs::read_to_string(path).unwrap();
        serde_json::from_str::<Vec<TranscriptEntry>>(&content).unwrap();
        assert!(content.contains("[truncated]"));
        let summary = super::summarize_transcript(&content).unwrap();
        assert!(!summary.contains("warning: transcript JSON is incomplete"));
        assert!(summary.contains("final: final_answer: done"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reads_latest_transcript() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-agent-latest-transcript-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let _older = write_transcript(
            &root,
            &[TranscriptEntry {
                role: "task".to_string(),
                content: "older".to_string(),
            }],
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let newer = write_transcript(
            &root,
            &[TranscriptEntry {
                role: "task".to_string(),
                content: "newer".to_string(),
            }],
        )
        .unwrap();
        let latest = super::read_latest_transcript(root.clone())
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::canonicalize(&latest.0).unwrap(),
            fs::canonicalize(&newer).unwrap()
        );
        assert!(latest.1.contains("newer"));
        let _ = fs::remove_dir_all(root);
    }
}
