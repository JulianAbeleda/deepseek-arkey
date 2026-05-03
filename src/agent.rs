use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::provider::{self, Message};
use crate::safety::{atomic_write, cap_text, redact_text};

mod read_tools;
mod transcript;
mod workspace;
use transcript::{write_transcript, TranscriptEntry};
use workspace::Workspace;

const MAX_TOOL_CHARS: usize = 12_000;
const MAX_READ_BYTES: u64 = 64 * 1024;

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
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ToolCall {
    pub name: String,
    #[serde(default)]
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct AgentDecision {
    #[serde(default)]
    pub thought: Option<String>,
    #[serde(default)]
    pub tool: Option<ToolCall>,
    #[serde(default)]
    pub final_answer: Option<String>,
    #[serde(default)]
    pub blocked: Option<String>,
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
    mut on_step: impl FnMut(usize, &str),
) -> Result<AgentOutcome, String> {
    let workspace = Workspace::new(config.root)?;
    let mut messages = vec![
        Message {
            role: "system".to_string(),
            content: system_prompt(&workspace.root),
        },
        Message {
            role: "user".to_string(),
            content: format!("Task: {}", redact_text(task)),
        },
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
        let decision = parse_decision(&raw)?;
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
        let result = execute_tool(&workspace, &tool, approval_mode);
        let result_text = cap_text(&redact_text(&result), MAX_TOOL_CHARS);
        transcript.push(TranscriptEntry {
            role: format!("tool:{}", tool.name),
            content: result_text.clone(),
        });
        messages.push(Message {
            role: "assistant".to_string(),
            content: redacted_raw,
        });
        messages.push(Message {
            role: "user".to_string(),
            content: format!(
                "Tool result for step {step}:\n{result_text}\nContinue with JSON only."
            ),
        });
    }

    let transcript_path = write_transcript(&workspace.root, &transcript)?;
    Ok(AgentOutcome {
        answer: format!("blocked: reached max agent steps ({})", config.max_steps),
        steps: config.max_steps,
        transcript_path,
    })
}

pub fn read_latest_transcript(
    root: impl Into<PathBuf>,
) -> Result<Option<(PathBuf, String)>, String> {
    transcript::read_latest_transcript(root, |root| {
        Workspace::new(root).map(|workspace| workspace.root)
    })
}

fn system_prompt(root: &Path) -> String {
    format!(
        r#"You are DeepSeek local agent mode. Work only inside this read-only workspace:
{}

Return exactly one JSON object and no prose.

To request a tool:
{{"thought":"short reason","tool":{{"name":"list_files","arguments":{{"path":"."}}}}}}

To finish:
{{"final_answer":"answer with concrete findings"}}

To stop when the task cannot continue safely:
{{"blocked":"short reason"}}

Available read-only tools:
- list_files: {{"path":"relative/path"}}
- read_file: {{"path":"relative/path"}}
- search_files: {{"path":"relative/path","query":"literal text"}}
- inspect_tree: {{"path":"relative/path","depth":2}}

Approval-gated tool:
- run_shell: {{"command":"command to run","cwd":"relative/path","reason":"why this is needed"}}
- propose_patch: {{"path":"relative/file","find":"exact existing text","replace":"replacement text","reason":"why this edit is needed"}}

No raw writes, creates, deletes, network actions, or paths outside the workspace are available. Shell commands and exact text replacements require explicit user approval and may be denied."#,
        root.display()
    )
}

pub fn parse_decision(text: &str) -> Result<AgentDecision, String> {
    let json =
        extract_json_object(text).ok_or_else(|| "agent response was not JSON".to_string())?;
    serde_json::from_str(json).map_err(|err| format!("invalid agent JSON: {err}"))
}

fn extract_json_object(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed);
    }
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (start < end).then_some(&text[start..=end])
}

fn execute_tool(workspace: &Workspace, call: &ToolCall, approval_mode: ApprovalMode) -> String {
    match call.name.as_str() {
        "list_files" => read_tools::list_files(workspace, call),
        "read_file" => read_tools::read_file(workspace, call),
        "search_files" => read_tools::search_files(workspace, call),
        "inspect_tree" => read_tools::inspect_tree(workspace, call),
        "run_shell" => tool_run_shell(workspace, call, approval_mode),
        "propose_patch" => tool_propose_patch(workspace, call, approval_mode),
        other => format!("error: unknown agent tool `{other}`"),
    }
}

fn arg_path(call: &ToolCall) -> Result<String, String> {
    call.arguments
        .get("path")
        .and_then(|value| value.as_str())
        .filter(|path| !path.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "missing non-empty `path`".to_string())
}

fn arg_string(call: &ToolCall, name: &str) -> Result<String, String> {
    call.arguments
        .get(name)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("missing non-empty `{name}`"))
}

fn tool_run_shell(workspace: &Workspace, call: &ToolCall, approval_mode: ApprovalMode) -> String {
    let command = match arg_string(call, "command") {
        Ok(command) => command,
        Err(err) => return format!("error: {err}"),
    };
    let cwd = call
        .arguments
        .get("cwd")
        .and_then(|value| value.as_str())
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(".");
    let cwd = match workspace.resolve_existing(cwd) {
        Ok(cwd) => cwd,
        Err(err) => return format!("error: {err}"),
    };
    if !cwd.is_dir() {
        return "error: cwd is not a directory".to_string();
    }
    let reason = call
        .arguments
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("no reason provided");
    if !approve_shell_command(workspace, &command, &cwd, reason, approval_mode) {
        return "denied: run_shell requires explicit interactive approval".to_string();
    }
    let output = match Command::new("sh")
        .arg("-lc")
        .arg(&command)
        .current_dir(&cwd)
        .output()
    {
        Ok(output) => output,
        Err(err) => return format!("error: failed to run shell command: {err}"),
    };
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[derive(Debug, Clone)]
struct PreparedPatch {
    path: PathBuf,
    display_path: String,
    reason: String,
    find: String,
    replace: String,
    original: String,
    updated: String,
}

fn tool_propose_patch(
    workspace: &Workspace,
    call: &ToolCall,
    approval_mode: ApprovalMode,
) -> String {
    let patch = match prepare_patch(workspace, call) {
        Ok(patch) => patch,
        Err(err) => return format!("error: {err}"),
    };
    if !approve_patch(&patch, approval_mode) {
        return "denied: propose_patch requires explicit interactive approval".to_string();
    }
    match apply_prepared_patch(&patch) {
        Ok(()) => format!("ok: patched {}", patch.display_path),
        Err(err) => format!("error: failed to apply patch: {err}"),
    }
}

fn prepare_patch(workspace: &Workspace, call: &ToolCall) -> Result<PreparedPatch, String> {
    let requested = arg_path(call)?;
    let find = arg_string(call, "find")?;
    let replace = match call
        .arguments
        .get("replace")
        .and_then(|value| value.as_str())
    {
        Some(replace) => replace.to_string(),
        None => return Err("missing string `replace`".to_string()),
    };
    let reason = arg_string(call, "reason")?;
    let path = workspace.resolve_existing(&requested)?;
    if !path.is_file() {
        return Err("path is not a file".to_string());
    }
    let metadata = fs::metadata(&path).map_err(|err| err.to_string())?;
    if metadata.len() > MAX_READ_BYTES {
        return Err(format!("file exceeds patch cap of {MAX_READ_BYTES} bytes"));
    }
    let original = fs::read_to_string(&path).map_err(|err| format!("file is not UTF-8: {err}"))?;
    let matches = original.matches(&find).count();
    if matches == 0 {
        return Err("find text was not present".to_string());
    }
    if matches > 1 {
        return Err("find text matched more than once".to_string());
    }
    let updated = original.replacen(&find, &replace, 1);
    Ok(PreparedPatch {
        display_path: workspace.display_path(&path),
        path,
        reason,
        find,
        replace,
        original,
        updated,
    })
}

fn apply_prepared_patch(patch: &PreparedPatch) -> io::Result<()> {
    let current = fs::read_to_string(&patch.path)?;
    if current != patch.original {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file changed after patch was prepared",
        ));
    }
    atomic_write(&patch.path, patch.updated.as_bytes())
}

fn approve_patch(patch: &PreparedPatch, approval_mode: ApprovalMode) -> bool {
    if approval_mode == ApprovalMode::Deny {
        return false;
    }
    if !io::stdin().is_terminal() {
        return false;
    }
    eprintln!("agent requests file edit");
    eprintln!("path: {}", patch.display_path);
    eprintln!("reason: {}", redact_text(&cap_text(&patch.reason, 1200)));
    eprintln!("--- find ---");
    eprintln!("find:\n{}", redact_text(&cap_text(&patch.find, 1200)));
    eprintln!("--- replace ---");
    eprintln!("replace:\n{}", redact_text(&cap_text(&patch.replace, 1200)));
    eprint!("Type yes apply to apply this edit: ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map(|_| answer.trim() == "yes apply")
        .unwrap_or(false)
}

fn approve_shell_command(
    workspace: &Workspace,
    command: &str,
    cwd: &Path,
    reason: &str,
    approval_mode: ApprovalMode,
) -> bool {
    if approval_mode == ApprovalMode::Deny {
        return false;
    }
    if !io::stdin().is_terminal() {
        return false;
    }
    eprintln!("agent requests shell execution");
    eprintln!("cwd: {}", workspace.display_path(cwd));
    eprintln!("reason: {}", redact_text(reason));
    eprintln!("--- command ---");
    eprintln!("command: {}", redact_text(command));
    eprint!("Type yes run to run this command: ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map(|_| answer.trim() == "yes run")
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io;

    use serde_json::json;

    use super::workspace::Workspace;
    use super::{
        apply_prepared_patch, parse_decision, prepare_patch, write_transcript, ApprovalMode,
        ToolCall, TranscriptEntry,
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
        assert!(path.starts_with(root.join(".deepseek/agent-transcripts")));
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
