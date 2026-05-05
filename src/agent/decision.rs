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
    pub tools: Vec<ToolCall>,
    #[serde(default)]
    pub final_answer: Option<String>,
    #[serde(default)]
    pub blocked: Option<String>,
}

const PLACEHOLDER_FINAL_CONTENT: &str = "answer with concrete findings";

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
    let value: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(first_err) => match parse_repaired_decision_value(json, first_err.column()) {
            Some(value) => value,
            None => {
                return Err(format!("invalid agent JSON: {first_err}"));
            }
        },
    };
    normalize_decision(value)
}

fn parse_repaired_decision_value(json: &str, column: usize) -> Option<serde_json::Value> {
    if let Some(repaired) = repair_malformed_arguments_string(json) {
        if let Ok(value) = serde_json::from_str(&repaired) {
            return Some(value);
        }
    }
    if let Some(repaired) = insert_missing_comma(json, column) {
        if let Ok(value) = serde_json::from_str(&repaired) {
            return Some(value);
        }
    }
    if let Some(repaired) = remove_extra_brace_at(json, column) {
        if let Ok(value) = serde_json::from_str(&repaired) {
            return Some(value);
        }
    }
    if let Some(repaired) = repair_unescaped_final_content_string(json) {
        if let Ok(value) = serde_json::from_str(&repaired) {
            return Some(value);
        }
    }
    None
}

fn repair_unescaped_final_content_string(json: &str) -> Option<String> {
    let prefix = r#"{"content":""#;
    let suffix = r#"","tool_calls":null}"#;
    if !json.starts_with(prefix) || !json.ends_with(suffix) {
        return None;
    }
    let content_end = json.len().checked_sub(suffix.len())?;
    let content = &json[prefix.len()..content_end];
    let mut escaped_content = String::with_capacity(content.len());
    let mut escaped = false;
    for ch in content.chars() {
        if escaped {
            escaped_content.push(ch);
            escaped = false;
            continue;
        }
        match ch {
            '\\' => {
                escaped_content.push(ch);
                escaped = true;
            }
            '"' => escaped_content.push_str("\\\""),
            '\n' => escaped_content.push_str("\\n"),
            '\r' => escaped_content.push_str("\\r"),
            '\t' => escaped_content.push_str("\\t"),
            _ => escaped_content.push(ch),
        }
    }
    Some(format!("{prefix}{escaped_content}{suffix}"))
}

fn repair_malformed_arguments_string(json: &str) -> Option<String> {
    let marker = r#""arguments":"{"#;
    let marker_start = json.find(marker)?;
    let value_start = marker_start + r#""arguments":""#.len();
    let (value_end, object) = find_repairable_arguments_object(json, value_start)?;

    Some(
        format!("{}{}{}", &json[..marker_start], r#""arguments":"#, object)
            + &json[value_end + 2..],
    )
}

fn find_repairable_arguments_object(text: &str, start: usize) -> Option<(usize, String)> {
    if text.as_bytes().get(start) != Some(&b'{') {
        return None;
    }
    for (offset, ch) in text[start..].char_indices() {
        if ch != '}' {
            continue;
        }
        let end = start + offset;
        if text.as_bytes().get(end + 1) != Some(&b'"') {
            continue;
        }
        let object = text[start..=end].replace("\\\"", "\"");
        if serde_json::from_str::<serde_json::Value>(&object).is_ok() {
            return Some((end, object));
        }
    }
    None
}

fn insert_missing_comma(json: &str, col: usize) -> Option<String> {
    let pos = col.checked_sub(1)?;
    let bytes = json.as_bytes();
    if bytes.get(pos) != Some(&b'"') {
        return None;
    }
    let previous = json[..pos].trim_end().as_bytes().last().copied()?;
    if !matches!(previous, b'"' | b'}' | b']' | b'e' | b'l' | b'0'..=b'9') {
        return None;
    }
    let key_end = json[pos + 1..].find('"')? + pos + 1;
    if json[key_end + 1..].trim_start().as_bytes().first() != Some(&b':') {
        return None;
    }
    Some(format!("{},{}", &json[..pos], &json[pos..]))
}

fn remove_extra_brace_at(json: &str, col: usize) -> Option<String> {
    let pos = col.checked_sub(1)?;
    if json.as_bytes().get(pos) != Some(&b'}') {
        return None;
    }
    let next = json[pos + 1..].trim_start().as_bytes().first().copied();
    if !matches!(next, Some(b']' | b'}')) {
        return None;
    }
    Some(format!("{}{}", &json[..pos], &json[pos + 1..]))
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
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
            '{' => brace_depth += 1,
            '[' => bracket_depth += 1,
            ']' => bracket_depth = bracket_depth.saturating_sub(1),
            '}' => {
                brace_depth = brace_depth.saturating_sub(1);
                if brace_depth == 0 && bracket_depth == 0 {
                    return Some(&text[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    let end = text.rfind('}')?;
    (start < end).then_some(&text[start..=end])
}

fn normalize_decision(value: serde_json::Value) -> Result<AgentDecision, String> {
    let tools = openai_tool_calls(&value)?;
    if !tools.is_empty() {
        return Ok(AgentDecision {
            thought: None,
            tool: tools.first().cloned(),
            tools,
            final_answer: None,
            blocked: None,
        });
    }
    if let Some(content) = value.get("content").and_then(|content| content.as_str()) {
        let content = content.trim();
        if !content.is_empty() && content != PLACEHOLDER_FINAL_CONTENT {
            return Ok(AgentDecision {
                thought: None,
                tool: None,
                tools: Vec::new(),
                final_answer: Some(content.to_string()),
                blocked: None,
            });
        }
    }
    serde_json::from_value(value).map_err(|err| format!("invalid agent JSON: {err}"))
}

fn openai_tool_calls(value: &serde_json::Value) -> Result<Vec<ToolCall>, String> {
    let Some(calls) = value.get("tool_calls").and_then(|calls| calls.as_array()) else {
        return Ok(Vec::new());
    };
    let mut parsed = Vec::new();
    for call in calls {
        let Some(function) = call.get("function").and_then(normalize_function_call) else {
            continue;
        };
        let Some(name) = function.get("name").and_then(|name| name.as_str()) else {
            continue;
        };
        let arguments = match function.get("arguments") {
            Some(value) if value.is_string() => parse_arguments_string(value.as_str().unwrap())?,
            Some(value) => value.clone(),
            None => serde_json::json!({}),
        };
        parsed.push(ToolCall {
            name: name.to_string(),
            arguments,
        });
    }
    Ok(parsed)
}

fn parse_arguments_string(text: &str) -> Result<serde_json::Value, String> {
    match serde_json::from_str(text) {
        Ok(value) => Ok(value),
        Err(first_err) => {
            if let Some(object) = extract_json_object(text) {
                if let Ok(value) = serde_json::from_str(object) {
                    return Ok(value);
                }
            }
            if let Some(repaired) = repair_unclosed_terminal_string_value(text) {
                if let Ok(value) = serde_json::from_str(&repaired) {
                    return Ok(value);
                }
            }
            Err(format!("invalid tool arguments JSON: {first_err}"))
        }
    }
}

fn repair_unclosed_terminal_string_value(text: &str) -> Option<String> {
    let trimmed = text.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return None;
    }
    if !has_odd_unescaped_quote_count(trimmed) {
        return None;
    }
    let last_brace = trimmed.rfind('}')?;
    let mut repaired = String::with_capacity(trimmed.len() + 1);
    repaired.push_str(&trimmed[..last_brace]);
    repaired.push('"');
    repaired.push_str(&trimmed[last_brace..]);
    Some(repaired)
}

fn has_odd_unescaped_quote_count(text: &str) -> bool {
    let mut escaped = false;
    let mut count = 0usize;
    for ch in text.chars() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            count += 1;
        }
    }
    count % 2 == 1
}

fn normalize_function_call(value: &serde_json::Value) -> Option<serde_json::Value> {
    if value.is_object() {
        return Some(value.clone());
    }
    value
        .as_str()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(text).ok())
        .filter(|value| value.is_object())
}
