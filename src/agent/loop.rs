use std::path::PathBuf;

use crate::cancel::CancellationToken;
use crate::provider::{self, assistant_message, system_message, user_message, Message};
use crate::safety::{cap_text, redact_text};

use super::approval_text;
use super::commit_audit;
#[allow(unused_imports)]
pub use super::decision::{parse_decision, AgentDecision, ToolCall};
use super::decision::{parse_decision_with_metadata, system_prompt};
use super::read_tools;
use super::transcript::{self, write_transcript, TranscriptEntry};
use super::workspace::Workspace;
use super::write_tools;

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

pub struct AgentRunOptions {
    config: AgentConfig,
    approval_mode: ApprovalMode,
    quiet_cache: bool,
    cancel: Option<CancellationToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentChatRoute {
    Standard,
    Quiet,
    Cancelled,
    QuietCancelled,
}

impl AgentRunOptions {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            approval_mode: ApprovalMode::Interactive,
            quiet_cache: false,
            cancel: None,
        }
    }

    pub fn approval_mode(mut self, approval_mode: ApprovalMode) -> Self {
        self.approval_mode = approval_mode;
        self
    }

    pub fn quiet_cache(mut self, quiet_cache: bool) -> Self {
        self.quiet_cache = quiet_cache;
        self
    }

    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
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
    run_agent_with_handlers(
        task,
        model,
        temperature,
        AgentRunOptions::new(config),
        |step| {
            eprintln!("agent step {}: {}", step.label(), step.tool);
        },
        unreachable_external_approval,
    )
}

pub fn run_agent_final_only(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
) -> Result<AgentOutcome, String> {
    run_agent_with_handlers(
        task,
        model,
        temperature,
        AgentRunOptions::new(config).quiet_cache(true),
        |_| {},
        unreachable_external_approval,
    )
}

pub fn run_agent_with_handlers(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    options: AgentRunOptions,
    mut on_step: impl FnMut(AgentStep),
    mut on_approval: impl FnMut(ApprovalRequest) -> ApprovalDecision,
) -> Result<AgentOutcome, String> {
    let AgentRunOptions {
        config,
        approval_mode,
        quiet_cache,
        cancel,
    } = options;
    let chat_cancel = cancel.clone();
    let chat_route = AgentChatRoute::from_options(quiet_cache, chat_cancel.as_ref());
    run_agent_with_chat_handler(
        task,
        model,
        temperature,
        config,
        approval_mode,
        &mut on_step,
        &mut on_approval,
        cancel,
        move |messages, model, temperature| match chat_route {
            AgentChatRoute::Standard => provider::chat(messages, model, temperature, None, false),
            AgentChatRoute::Quiet => provider::chat_quiet(messages, model, temperature, None),
            AgentChatRoute::Cancelled => {
                let cancel = chat_cancel.as_ref().expect("cancel route has token");
                provider::chat_cancelled(messages, model, temperature, None, cancel)
            }
            AgentChatRoute::QuietCancelled => {
                let cancel = chat_cancel.as_ref().expect("quiet cancel route has token");
                provider::chat_quiet_cancelled(messages, model, temperature, None, cancel)
            }
        },
    )
}

impl AgentChatRoute {
    fn from_options(quiet_cache: bool, cancel: Option<&CancellationToken>) -> Self {
        match (quiet_cache, cancel.is_some()) {
            (false, false) => Self::Standard,
            (true, false) => Self::Quiet,
            (false, true) => Self::Cancelled,
            (true, true) => Self::QuietCancelled,
        }
    }
}

fn unreachable_external_approval(_: ApprovalRequest) -> ApprovalDecision {
    unreachable!("default agent wrappers must not request external approval")
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
                summary: approval_text::shell_summary(cwd, reason, command),
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
                summary: approval_text::patch_summary(path, reason, find, replace),
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
#[path = "loop_tests.rs"]
mod tests;
