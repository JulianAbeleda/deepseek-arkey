use std::path::Path;

use serde::{Deserialize, Serialize};

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

pub(super) fn system_prompt(root: &Path) -> String {
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
