use std::path::PathBuf;

use crate::provider::{self, assistant_message, system_message, user_message};
use crate::safety::{cap_text, redact_text};

mod decision;
mod read_tools;
mod transcript;
mod workspace;
mod write_tools;
use decision::system_prompt;
pub use decision::{parse_decision, ToolCall};
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
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub step: usize,
    pub tool: String,
    pub summary: String,
    pub approve_phrase: String,
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
        |step, tool| {
            eprintln!("agent step {step}: {tool}");
        },
    )
}

pub fn run_agent_with_options(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
    approval_mode: ApprovalMode,
    on_step: impl FnMut(usize, &str),
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

pub fn run_agent_with_approval_handler(
    task: &str,
    model: &str,
    temperature: Option<f32>,
    config: AgentConfig,
    approval_mode: ApprovalMode,
    mut on_step: impl FnMut(usize, &str),
    mut on_approval: impl FnMut(ApprovalRequest) -> ApprovalDecision,
) -> Result<AgentOutcome, String> {
    let workspace = Workspace::new(config.root)?;
    let mut messages = vec![
        system_message(system_prompt(&workspace.root)),
        user_message(format!("Task: {}", redact_text(task))),
    ];
    let mut transcript = vec![TranscriptEntry {
        role: "task".to_string(),
        content: redact_text(task),
    }];

    for step in 1..=config.max_steps {
        let raw = provider::chat(&messages, model, temperature, None, false)?;
        let redacted_raw = cap_text(&redact_text(&raw), MAX_TOOL_CHARS);
        transcript.push(TranscriptEntry {
            role: "assistant".to_string(),
            content: redacted_raw.clone(),
        });
        let decision = parse_decision(&raw).map_err(|err| {
            let snippet = cap_text(&redact_text(&raw), 400);
            format!("{err}\nraw snippet: {snippet}")
        })?;
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
        let Some(tool) = decision.tool else {
            return Err(
                "agent response did not include final_answer, blocked, or tool".to_string(),
            );
        };
        on_step(step, &tool.name);
        let tool_approval_mode = if approval_mode == ApprovalMode::External {
            match approval_request(step, &tool) {
                Some(request) => match on_approval(request) {
                    ApprovalDecision::Approve => ApprovalMode::Approved,
                    ApprovalDecision::Deny => ApprovalMode::Deny,
                },
                None => ApprovalMode::Deny,
            }
        } else {
            approval_mode
        };
        let result = execute_tool(&workspace, &tool, tool_approval_mode);
        let result_text = cap_text(&redact_text(&result), MAX_TOOL_CHARS);
        transcript.push(TranscriptEntry {
            role: format!("tool:{}", tool.name),
            content: result_text.clone(),
        });
        messages.push(assistant_message(redacted_raw));
        messages.push(user_message(format!(
            "Tool result for step {step}:\n{result_text}\nContinue with JSON only."
        )));
    }

    let transcript_path = write_transcript(&workspace.root, &transcript)?;
    Ok(AgentOutcome {
        answer: format!("blocked: reached max agent steps ({})", config.max_steps),
        steps: config.max_steps,
        transcript_path,
    })
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
                approve_phrase: "yes run".to_string(),
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
                approve_phrase: "yes apply".to_string(),
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
    use std::fs;
    use std::io;

    use serde_json::json;

    use crate::provider::PROVIDER_STATE_DIR;

    use super::workspace::Workspace;
    use super::write_tools::{apply_prepared_patch, prepare_patch};
    use super::{
        parse_decision, write_transcript, ApprovalMode, ToolCall, TranscriptEntry,
        DEFAULT_MAX_STEPS,
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
    fn parses_openai_style_tool_call() {
        let decision = parse_decision(
            r#"{"content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}}]}"#,
        )
        .unwrap();
        let tool = decision.tool.unwrap();
        assert_eq!(tool.name, "read_file");
        assert_eq!(tool.arguments["path"], "src/main.rs");
    }

    #[test]
    fn parses_openai_style_final_content() {
        let decision = parse_decision(r#"{"content":"done","tool_calls":null}"#).unwrap();
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
        assert_eq!(request.approve_phrase, "yes run");
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
        assert_eq!(request.approve_phrase, "yes apply");
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
