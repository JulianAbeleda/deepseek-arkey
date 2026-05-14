use crate::cancel::CancellationToken;
use crate::provider::{self, assistant_message, system_message, user_message, Message};
use crate::safety::{cap_text, redact_text};

use super::commit_audit;
use super::decision::{parse_decision_with_metadata, system_prompt};
use super::dispatch::{approval_request, execute_tool};
use super::notes::{
    append_assistant_transcript_entry, append_no_action_retry_note,
    append_parse_failure_retry_note, append_parser_repair_notes, check_cancelled,
    decision_has_action, decision_retry_prompt, fail_no_action_with_transcript,
    fail_with_transcript, no_action_fallback_outcome, sanitize_tool_observation,
    transcript_has_tool_results,
};
use super::transcript::{write_transcript, TranscriptEntry};
use super::types::{
    AgentChatRoute, AgentConfig, AgentOutcome, AgentRunOptions, AgentStep, ApprovalDecision,
    ApprovalMode, ApprovalRequest, MAX_TOOL_CHARS,
};
use super::workspace::Workspace;

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
    pub(super) fn from_options(quiet_cache: bool, cancel: Option<&CancellationToken>) -> Self {
        match (quiet_cache, cancel.is_some()) {
            (false, false) => Self::Standard,
            (true, false) => Self::Quiet,
            (false, true) => Self::Cancelled,
            (true, true) => Self::QuietCancelled,
        }
    }
}

pub(super) fn unreachable_external_approval(_: ApprovalRequest) -> ApprovalDecision {
    unreachable!("default agent wrappers must not request external approval")
}

pub(super) fn run_agent_with_chat_handler(
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
                match approval_request(&workspace, step, tool) {
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
