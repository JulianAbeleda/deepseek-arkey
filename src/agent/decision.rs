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

Return exactly one JSON object and no prose. Use this OpenAI-compatible shape:

To request a tool:
{{"content":null,"tool_calls":[{{"id":"call_1","type":"function","function":{{"name":"list_files","arguments":"{{\"path\":\".\"}}"}}}}]}}

To finish:
{{"content":"answer with concrete findings","tool_calls":null}}

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
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|err| format!("invalid agent JSON: {err}"))?;
    normalize_decision(value)
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in text[start..].char_indices() {
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
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(&text[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn normalize_decision(value: serde_json::Value) -> Result<AgentDecision, String> {
    if let Some(tool) = openai_tool_call(&value)? {
        return Ok(AgentDecision {
            thought: None,
            tool: Some(tool),
            final_answer: None,
            blocked: None,
        });
    }
    if let Some(content) = value.get("content").and_then(|content| content.as_str()) {
        let content = content.trim();
        if !content.is_empty() {
            return Ok(AgentDecision {
                thought: None,
                tool: None,
                final_answer: Some(content.to_string()),
                blocked: None,
            });
        }
    }
    serde_json::from_value(value).map_err(|err| format!("invalid agent JSON: {err}"))
}

fn openai_tool_call(value: &serde_json::Value) -> Result<Option<ToolCall>, String> {
    let Some(calls) = value.get("tool_calls").and_then(|calls| calls.as_array()) else {
        return Ok(None);
    };
    let Some(call) = calls.first() else {
        return Ok(None);
    };
    let Some(function) = call.get("function") else {
        return Ok(None);
    };
    let name = function
        .get("name")
        .and_then(|name| name.as_str())
        .ok_or_else(|| "invalid agent JSON: missing tool function name".to_string())?
        .to_string();
    let arguments = match function.get("arguments") {
        Some(value) if value.is_string() => serde_json::from_str(value.as_str().unwrap())
            .map_err(|err| format!("invalid tool arguments JSON: {err}"))?,
        Some(value) => value.clone(),
        None => serde_json::json!({}),
    };
    Ok(Some(ToolCall { name, arguments }))
}
