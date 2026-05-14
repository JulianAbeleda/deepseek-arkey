use std::collections::VecDeque;
use std::fs;
use std::io;

use serde_json::json;

use crate::cancel::CancellationToken;
use crate::provider::PROVIDER_STATE_DIR;

use super::super::workspace::Workspace;
use super::super::write_tools::{apply_prepared_patch, prepare_patch};
use super::{
    append_no_action_retry_note, append_parser_repair_notes, parse_decision,
    parse_decision_with_metadata, system_prompt, unreachable_external_approval, write_transcript,
    AgentChatRoute, AgentConfig, ApprovalDecision, ApprovalMode, ApprovalRequest, ApprovalScope,
    ToolCall, TranscriptEntry, DEFAULT_MAX_STEPS,
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
fn system_prompt_includes_web_tools() {
    let prompt = system_prompt(std::path::Path::new("/tmp/workspace"));
    assert!(prompt.contains("- web_search:"));
    assert!(prompt.contains("- fetch_url:"));
    assert!(prompt.contains("Only web_search and fetch_url may access the network"));
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
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{\"depth\":2,\"path\":\"sample_repo\"}}"}}]}"#,
        )
        .unwrap();
    let tool = decision.tool.unwrap();
    assert_eq!(tool.name, "inspect_tree");
    assert_eq!(tool.arguments["depth"], 2);
    assert_eq!(tool.arguments["path"], "sample_repo");
}

#[test]
fn records_trailing_arguments_repair_metadata() {
    let parsed = parse_decision_with_metadata(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"inspect_tree","arguments":"{\"depth\":2,\"path\":\"sample_repo\"}}"}}]}"#,
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
        parse_decision(r#"{"content":"answer with concrete findings","blocked":"wait"}"#).unwrap();
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
    assert!(
        err.contains("agent response did not include final_answer, blocked, or tool after retry")
    );
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
fn agent_chat_route_keeps_quiet_and_cancel_orthogonal() {
    let cancel = CancellationToken::new();

    assert_eq!(
        AgentChatRoute::from_options(false, None),
        AgentChatRoute::Standard
    );
    assert_eq!(
        AgentChatRoute::from_options(true, None),
        AgentChatRoute::Quiet
    );
    assert_eq!(
        AgentChatRoute::from_options(false, Some(&cancel)),
        AgentChatRoute::Cancelled
    );
    assert_eq!(
        AgentChatRoute::from_options(true, Some(&cancel)),
        AgentChatRoute::QuietCancelled
    );
}

#[test]
fn default_agent_approval_handler_panics_if_reached() {
    let result = std::panic::catch_unwind(|| {
        unreachable_external_approval(ApprovalRequest {
            step: 1,
            tool: "run_shell".to_string(),
            root: std::env::current_dir().unwrap(),
            scope: ApprovalScope::Shell,
            summary: "approval required".to_string(),
        });
    });

    assert!(result.is_err());
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
    let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
    let request = super::approval_request(&workspace, 2, &call).unwrap();
    assert_eq!(request.step, 2);
    assert_eq!(request.tool, "run_shell");
    assert_eq!(request.root, workspace.root);
    assert_eq!(request.scope, ApprovalScope::Shell);
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
            name: "unknown_tool".to_string(),
            arguments: json!({}),
        },
    );
    assert!(result.contains("unknown agent tool"));
}

#[test]
fn fetch_url_tool_dispatches_without_approval() {
    let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
    let result = execute_tool(
        &workspace,
        &ToolCall {
            name: "fetch_url".to_string(),
            arguments: json!({"url":"file:///etc/passwd"}),
        },
    );
    assert!(result.contains("only http:// and https:// URLs are supported"));
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
    let workspace = Workspace::new(std::env::current_dir().unwrap()).unwrap();
    let request = super::approval_request(
        &workspace,
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
    assert_eq!(request.root, workspace.root);
    assert_eq!(request.scope, ApprovalScope::Write);
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

#[test]
fn reads_latest_transcript_migrates_old_transcript_dir() {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "deepseek-agent-old-transcript-test-{}-{unique}",
        std::process::id()
    ));
    let old_dir = root.join(".deepseek").join("agent-transcripts");
    fs::create_dir_all(&old_dir).unwrap();
    let old_path = old_dir.join("1.json");
    fs::write(&old_path, r#"[{"role":"task","content":"old transcript"}]"#).unwrap();

    let latest = super::read_latest_transcript(root.clone())
        .unwrap()
        .unwrap();
    assert_eq!(
        fs::canonicalize(latest.0.parent().unwrap()).unwrap(),
        fs::canonicalize(root.join(PROVIDER_STATE_DIR).join("agent-transcripts")).unwrap()
    );
    assert!(latest.1.contains("old transcript"));
    assert!(old_path.exists());
    let _ = fs::remove_dir_all(root);
}
