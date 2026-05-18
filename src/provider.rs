use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Output, Stdio};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize, Serializer};

use crate::cancel::{CancellationToken, CANCELLED};
use crate::safety::{cap_text, redact_text, DEFAULT_TEXT_CAP};

pub const PROVIDER: &str = "DeepSeek";
pub const APP_COMMAND: &str = "deepseek-arkey";
pub const PROVIDER_DIR: &str = "deepseek-arkey";
pub const OLD_PROVIDER_DIR: &str = "deepseek";
pub const PROVIDER_STATE_DIR: &str = ".deepseek-arkey";
pub const OLD_PROVIDER_STATE_DIR: &str = ".deepseek";
pub const ENV_KEY: &str = "DEEPSEEK_API_KEY";
pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";
pub const DEFAULT_SESSION_NAME: &str = "default";
pub const SUPPORTED_MODELS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];
const API_URL: &str = "https://api.deepseek.com/chat/completions";
const LOGIN_MAX_TOKENS: u32 = 128;
const API_KEY_SETUP_HELP: &str = r#"DeepSeek API key is not set.

For troubleshooting, you can share this message with an AI provider or support
chat, but do not include any real API keys, tokens, or secrets.

Set it for the current shell:
  export DEEPSEEK_API_KEY="your_deepseek_api_key"

For zsh persistence on this machine:
  echo 'export DEEPSEEK_API_KEY="your_deepseek_api_key"' >> ~/.zsh_secrets
  source ~/.zshrc

The ~/.zshrc file sources ~/.zsh_secrets, so keep provider keys there instead
of writing secrets directly into ~/.zshrc.

Then verify:
  deepseek-arkey login"#;

#[derive(Debug, Clone, Deserialize)]
pub struct Message {
    pub role: String,
    #[serde(default, deserialize_with = "deserialize_nullable_string")]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<NativeToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

fn deserialize_nullable_string<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value: Option<String> = Option::deserialize(deserializer)?;
    Ok(value.unwrap_or_default())
}

impl Serialize for Message {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let content =
            if self.role == "assistant" && self.tool_calls.is_some() && self.content.is_empty() {
                None
            } else {
                Some(self.content.as_str())
            };
        MessageWire {
            role: &self.role,
            content,
            reasoning_content: self.reasoning_content.as_deref(),
            tool_calls: self.tool_calls.as_deref(),
            tool_call_id: self.tool_call_id.as_deref(),
        }
        .serialize(serializer)
    }
}

#[derive(Serialize)]
struct MessageWire<'a> {
    role: &'a str,
    content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_content: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<&'a [NativeToolCall]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: NativeFunctionCall,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct NativeFunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatTool {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub function: ChatToolFunction,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChatToolFunction {
    pub name: &'static str,
    pub description: &'static str,
    pub parameters: serde_json::Value,
}

pub fn message(role: impl Into<String>, content: impl Into<String>) -> Message {
    Message {
        role: role.into(),
        content: content.into(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

pub fn system_message(content: impl Into<String>) -> Message {
    message("system", content)
}

pub fn user_message(content: impl Into<String>) -> Message {
    message("user", content)
}

pub fn assistant_message(content: impl Into<String>) -> Message {
    message("assistant", content)
}

#[allow(dead_code)]
pub fn assistant_tool_calls_message(tool_calls: Vec<NativeToolCall>) -> Message {
    assistant_tool_calls_message_with_reasoning(tool_calls, None)
}

pub fn assistant_tool_calls_message_with_reasoning(
    tool_calls: Vec<NativeToolCall>,
    reasoning_content: Option<String>,
) -> Message {
    Message {
        role: "assistant".to_string(),
        content: String::new(),
        reasoning_content,
        tool_calls: Some(tool_calls),
        tool_call_id: None,
    }
}

pub fn tool_message(tool_call_id: impl Into<String>, content: impl Into<String>) -> Message {
    Message {
        role: "tool".to_string(),
        content: content.into(),
        reasoning_content: None,
        tool_calls: None,
        tool_call_id: Some(tool_call_id.into()),
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [ChatTool]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'a str>,
}

#[derive(Clone, Copy)]
struct ChatOptions<'a> {
    max_tokens: Option<u32>,
    stream: bool,
    print_cache: bool,
    print_stream_trailing_newline: bool,
    decision_response: bool,
    cancel: Option<&'a CancellationToken>,
    tools: Option<&'a [ChatTool]>,
    tool_choice: Option<&'a str>,
}

impl Default for ChatOptions<'_> {
    fn default() -> Self {
        Self {
            max_tokens: None,
            stream: false,
            print_cache: false,
            print_stream_trailing_newline: false,
            decision_response: false,
            cancel: None,
            tools: None,
            tool_choice: None,
        }
    }
}

pub fn api_key() -> Result<String, String> {
    parse_api_key(std::env::var(ENV_KEY).ok())
}

fn parse_api_key(value: Option<String>) -> Result<String, String> {
    value
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| API_KEY_SETUP_HELP.to_string())
}

pub fn login_check(model: &str) -> Result<(), String> {
    let messages = vec![user_message("Say exactly: OK")];
    let _ = chat(&messages, model, Some(0.0), Some(LOGIN_MAX_TOKENS), false)?;
    Ok(())
}

pub fn chat(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: bool,
) -> Result<String, String> {
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            stream,
            print_cache: true,
            print_stream_trailing_newline: true,
            ..ChatOptions::default()
        },
        |delta| {
            print!("{delta}");
            let _ = std::io::stdout().flush();
        },
    )
}

#[allow(dead_code)]
pub fn chat_with_delta<F>(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: bool,
    on_delta: F,
) -> Result<String, String>
where
    F: FnMut(&str),
{
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            stream,
            print_cache: true,
            ..ChatOptions::default()
        },
        on_delta,
    )
}

pub fn chat_quiet(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
) -> Result<String, String> {
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            ..ChatOptions::default()
        },
        |_| {},
    )
}

#[allow(dead_code)]
pub fn chat_cancelled(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    cancel: &CancellationToken,
) -> Result<String, String> {
    // stream=false, so trailing newline printing is disabled for clarity.
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            print_cache: true,
            cancel: Some(cancel),
            ..ChatOptions::default()
        },
        |_| {},
    )
}

#[allow(dead_code)]
pub fn chat_quiet_cancelled(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    cancel: &CancellationToken,
) -> Result<String, String> {
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            cancel: Some(cancel),
            ..ChatOptions::default()
        },
        |_| {},
    )
}

pub fn chat_tools(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    tools: &[ChatTool],
) -> Result<String, String> {
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            print_cache: true,
            decision_response: true,
            tools: Some(tools),
            tool_choice: Some("auto"),
            ..ChatOptions::default()
        },
        |_| {},
    )
}

pub fn chat_tools_quiet(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    tools: &[ChatTool],
) -> Result<String, String> {
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            decision_response: true,
            tools: Some(tools),
            tool_choice: Some("auto"),
            ..ChatOptions::default()
        },
        |_| {},
    )
}

pub fn chat_tools_cancelled(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    cancel: &CancellationToken,
    tools: &[ChatTool],
) -> Result<String, String> {
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            print_cache: true,
            decision_response: true,
            cancel: Some(cancel),
            tools: Some(tools),
            tool_choice: Some("auto"),
            ..ChatOptions::default()
        },
        |_| {},
    )
}

pub fn chat_tools_quiet_cancelled(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    cancel: &CancellationToken,
    tools: &[ChatTool],
) -> Result<String, String> {
    chat_impl(
        messages,
        model,
        temperature,
        ChatOptions {
            max_tokens,
            decision_response: true,
            cancel: Some(cancel),
            tools: Some(tools),
            tool_choice: Some("auto"),
            ..ChatOptions::default()
        },
        |_| {},
    )
}

fn chat_impl<F>(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    options: ChatOptions<'_>,
    on_delta: F,
) -> Result<String, String>
where
    F: FnMut(&str),
{
    let ChatOptions {
        max_tokens,
        stream,
        print_cache,
        print_stream_trailing_newline,
        decision_response,
        cancel,
        tools,
        tool_choice,
    } = options;
    if let Some(cancel) = cancel {
        cancel.check()?;
    }
    let key = api_key()?;
    let body = serde_json::to_string(&ChatRequest {
        model,
        messages,
        temperature,
        max_tokens,
        stream,
        tools,
        tool_choice,
    })
    .map_err(|err| err.to_string())?;
    let client = CurlHttpClient;
    if stream {
        return chat_streaming(
            &client,
            &key,
            body,
            print_stream_trailing_newline,
            on_delta,
            cancel,
        );
    }
    let output = if let Some(cancel) = cancel {
        client.post_cancelled(&key, &body, false, cancel)?
    } else {
        client.post(&key, &body, false)?
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "provider request failed".to_string()
        } else {
            redact_text(&stderr)
        });
    }
    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    if print_cache {
        print_cache_stats(&raw);
    }
    if decision_response {
        extract_assistant_decision_text(&raw)
    } else {
        extract_assistant_text(&raw)
    }
}

fn chat_streaming<F>(
    client: &impl HttpClient,
    key: &str,
    body: String,
    print_trailing_newline: bool,
    mut on_delta: F,
    cancel: Option<&CancellationToken>,
) -> Result<String, String>
where
    F: FnMut(&str),
{
    let mut child = client.post_stream(key, &body)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "failed to capture provider stdout".to_string())?;
    let mut response = String::new();
    let mut malformed_events = 0usize;
    for line in BufReader::new(stdout).lines() {
        if let Some(cancel) = cancel {
            if cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return Err(CANCELLED.to_string());
            }
        }
        let line = line.map_err(|err| err.to_string())?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            break;
        }
        let value: serde_json::Value = match serde_json::from_str(data) {
            Ok(value) => value,
            Err(_) => {
                malformed_events += 1;
                continue;
            }
        };
        if let Some(error) = value.get("error") {
            return Err(format!(
                "provider error: {}",
                cap_text(&error.to_string(), 1000)
            ));
        }
        if let Some(delta) = stream_delta(&value) {
            on_delta(delta);
            response.push_str(delta);
            if let Some(cancel) = cancel {
                if cancel.is_cancelled() {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CANCELLED.to_string());
                }
            }
        }
    }
    if malformed_events > 0 {
        eprintln!("stream warning: skipped {malformed_events} malformed event(s)");
    }
    let output = child.wait_with_output().map_err(|err| err.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "provider request failed".to_string()
        } else {
            redact_text(&stderr)
        });
    }
    if print_trailing_newline && !response.ends_with('\n') {
        println!();
    }
    Ok(cap_text(response.trim(), DEFAULT_TEXT_CAP))
}

trait HttpClient {
    fn post(&self, key: &str, body: &str, stream: bool) -> Result<Output, String>;
    fn post_stream(&self, key: &str, body: &str) -> Result<Child, String>;
}

struct CurlHttpClient;

impl HttpClient for CurlHttpClient {
    fn post(&self, key: &str, body: &str, stream: bool) -> Result<Output, String> {
        self.spawn(&self.config(key, body, stream))?
            .wait_with_output()
            .map_err(|err| err.to_string())
    }

    fn post_stream(&self, key: &str, body: &str) -> Result<Child, String> {
        self.spawn(&self.config(key, body, true))
    }
}

impl CurlHttpClient {
    fn post_cancelled(
        &self,
        key: &str,
        body: &str,
        stream: bool,
        cancel: &CancellationToken,
    ) -> Result<Output, String> {
        wait_with_cancel(self.spawn(&self.config(key, body, stream))?, cancel)
    }

    fn spawn(&self, config: &str) -> Result<Child, String> {
        let mut child = self
            .command()
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|err| format!("failed to run curl: {err}"))?;
        child
            .stdin
            .take()
            .ok_or_else(|| "failed to open curl stdin".to_string())?
            .write_all(config.as_bytes())
            .map_err(|err| format!("failed to write curl config: {err}"))?;
        Ok(child)
    }

    fn command(&self) -> Command {
        let mut command = Command::new("curl");
        command.arg("-q").arg("-K").arg("-");
        command
    }

    fn config(&self, key: &str, body: &str, stream: bool) -> String {
        let mut config = String::new();
        config.push_str("silent\n");
        config.push_str("show-error\n");
        if stream {
            config.push_str("no-buffer\n");
        }
        config.push_str("request = \"POST\"\n");
        config.push_str(&format!("url = \"{}\"\n", curl_quote(API_URL)));
        config.push_str(&format!(
            "header = \"{}\"\n",
            curl_quote(&format!("Authorization: Bearer {key}"))
        ));
        config.push_str(&format!(
            "header = \"{}\"\n",
            curl_quote("Content-Type: application/json")
        ));
        config.push_str(&format!("data = \"{}\"\n", curl_quote(body)));
        config
    }
}

fn wait_with_cancel(mut child: Child, cancel: &CancellationToken) -> Result<Output, String> {
    loop {
        if cancel.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            return Err(CANCELLED.to_string());
        }
        match child.try_wait().map_err(|err| err.to_string())? {
            Some(_) => {
                return child.wait_with_output().map_err(|err| err.to_string());
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}

#[cfg(test)]
fn curl_command() -> Command {
    CurlHttpClient.command()
}

#[cfg(test)]
fn curl_config(key: &str, body: &str, stream: bool) -> String {
    CurlHttpClient.config(key, body, stream)
}

fn curl_quote(value: &str) -> String {
    let mut quoted = String::new();
    for ch in value.chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\\\""),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            ch => quoted.push(ch),
        }
    }
    quoted
}

pub fn extract_assistant_text(raw: &str) -> Result<String, String> {
    let message = extract_assistant_message(raw)?;
    if let Some(decision) = openai_tool_decision_text(&message) {
        return Ok(decision);
    }
    message
        .get("content")
        .and_then(|content| content.as_str())
        .map(|content| content.trim().to_string())
        .map(|content| cap_text(&content, DEFAULT_TEXT_CAP))
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            format!(
                "provider response did not include assistant text: {}",
                cap_text(raw, 1000)
            )
        })
}

pub fn extract_assistant_decision_text(raw: &str) -> Result<String, String> {
    let message = extract_assistant_message(raw)?;
    if let Some(decision) = openai_tool_decision_text(&message) {
        return Ok(decision);
    }
    let content = message
        .get("content")
        .and_then(|content| content.as_str())
        .map(str::trim)
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            format!(
                "provider response did not include assistant text: {}",
                cap_text(raw, 1000)
            )
        })?;
    if content.starts_with('{') {
        return Ok(cap_text(content, DEFAULT_TEXT_CAP));
    }
    Ok(serde_json::json!({
        "content": cap_text(content, DEFAULT_TEXT_CAP),
        "tool_calls": null,
    })
    .to_string())
}

fn extract_assistant_message(raw: &str) -> Result<serde_json::Value, String> {
    let value: serde_json::Value = serde_json::from_str(raw).map_err(|err| {
        format!(
            "provider returned invalid JSON: {err}; body={}",
            cap_text(&redact_text(raw), 500)
        )
    })?;
    if let Some(error) = value.get("error") {
        return Err(format!(
            "provider error: {}",
            cap_text(&error.to_string(), 1000)
        ));
    }
    let message = value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| {
            format!(
                "provider response did not include assistant text: {}",
                cap_text(raw, 1000)
            )
        })?;
    Ok(message.clone())
}

fn openai_tool_decision_text(message: &serde_json::Value) -> Option<String> {
    let calls = message.get("tool_calls").filter(|calls| calls.is_array())?;
    let content = message
        .get("content")
        .and_then(|content| content.as_str())
        .map(str::trim)
        .filter(|content| !content.is_empty());
    let decision = serde_json::json!({
        "content": content,
        "reasoning_content": message
            .get("reasoning_content")
            .and_then(|reasoning| reasoning.as_str()),
        "tool_calls": calls,
    });
    Some(decision.to_string())
}

fn stream_delta(value: &serde_json::Value) -> Option<&str> {
    value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("delta"))
        .and_then(|delta| delta.get("content"))
        .and_then(|content| content.as_str())
}

fn print_cache_stats(raw: &str) {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return;
    };
    let Some(usage) = value.get("usage") else {
        return;
    };
    let cached = usage
        .pointer("/prompt_tokens_details/cached_tokens")
        .or_else(|| usage.pointer("/input_tokens_details/cached_tokens"))
        .or_else(|| usage.get("cached_tokens"))
        .and_then(|value| value.as_u64());
    if let Some(cached) = cached {
        eprintln!("cache: cached_tokens={cached}");
    }
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command, Output, Stdio};

    use crate::cancel::{CancellationToken, CANCELLED};

    use super::{
        assistant_tool_calls_message, chat_streaming, curl_command, curl_config, curl_quote,
        extract_assistant_decision_text, extract_assistant_text, message, parse_api_key,
        tool_message, ChatRequest, ChatTool, ChatToolFunction, HttpClient, Message,
        NativeFunctionCall, NativeToolCall,
    };

    #[test]
    fn extracts_assistant_text() {
        let raw = r#"{"choices":[{"message":{"content":"hello"}}]}"#;
        assert_eq!(extract_assistant_text(raw).unwrap(), "hello");
    }

    #[test]
    fn extracts_final_content_as_agent_decision_text() {
        let raw = r#"{"choices":[{"message":{"content":"done"}}]}"#;
        let text = extract_assistant_decision_text(raw).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["content"], "done");
        assert!(value["tool_calls"].is_null());
    }

    #[test]
    fn preserves_legacy_json_content_as_agent_decision_text() {
        let raw = r#"{"choices":[{"message":{"content":"{\"tool\":{\"name\":\"list_files\",\"arguments\":{\"path\":\".\"}}}"}}]}"#;
        let text = extract_assistant_decision_text(raw).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value.pointer("/tool/name").unwrap(), "list_files");
    }

    #[test]
    fn normal_chat_request_omits_native_tools() {
        let messages = vec![message("user", "hello")];
        let body = serde_json::to_value(ChatRequest {
            model: "deepseek-v4-pro",
            messages: &messages,
            temperature: None,
            max_tokens: None,
            stream: false,
            tools: None,
            tool_choice: None,
        })
        .unwrap();
        assert!(body.get("tools").is_none());
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn agent_chat_request_serializes_native_tools() {
        let messages = vec![message("user", "list files")];
        let tools = vec![ChatTool {
            kind: "function",
            function: ChatToolFunction {
                name: "list_files",
                description: "List files.",
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"],
                }),
            },
        }];
        let body = serde_json::to_value(ChatRequest {
            model: "deepseek-v4-pro",
            messages: &messages,
            temperature: None,
            max_tokens: None,
            stream: false,
            tools: Some(&tools),
            tool_choice: Some("auto"),
        })
        .unwrap();
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["name"], "list_files");
    }

    #[test]
    fn native_tool_messages_serialize_openai_shape() {
        let assistant = assistant_tool_calls_message(vec![NativeToolCall {
            id: "call_1".to_string(),
            kind: "function".to_string(),
            function: NativeFunctionCall {
                name: "read_file".to_string(),
                arguments: "{\"path\":\"README.md\"}".to_string(),
            },
        }]);
        let tool = tool_message("call_1", "contents");
        let assistant_json = serde_json::to_value(assistant).unwrap();
        let tool_json = serde_json::to_value(tool).unwrap();
        assert_eq!(assistant_json["role"], "assistant");
        assert!(assistant_json["content"].is_null());
        assert_eq!(assistant_json["tool_calls"][0]["id"], "call_1");
        assert_eq!(tool_json["role"], "tool");
        assert_eq!(tool_json["tool_call_id"], "call_1");
    }

    #[test]
    fn native_tool_message_can_include_reasoning_content() {
        let assistant = super::assistant_tool_calls_message_with_reasoning(
            vec![NativeToolCall {
                id: "call_1".to_string(),
                kind: "function".to_string(),
                function: NativeFunctionCall {
                    name: "read_file".to_string(),
                    arguments: "{\"path\":\"README.md\"}".to_string(),
                },
            }],
            Some("need file".to_string()),
        );
        let value = serde_json::to_value(assistant).unwrap();
        assert_eq!(value["reasoning_content"], "need file");
    }

    #[test]
    fn message_round_trips_null_content() {
        let json = r#"{"role":"assistant","content":null,"tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]}"#;
        let message: Message = serde_json::from_str(json).unwrap();
        assert_eq!(message.role, "assistant");
        assert_eq!(message.content, "");
        assert_eq!(message.tool_calls.as_ref().unwrap()[0].id, "call_1");
        let roundtrip = serde_json::to_value(&message).unwrap();
        assert!(roundtrip["content"].is_null());
    }

    #[test]
    fn extracts_native_tool_calls_as_agent_decision_json() {
        let raw = r#"{"choices":[{"message":{"content":"","reasoning_content":"need file","tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}}]}}]}"#;
        let text = extract_assistant_text(raw).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(value.get("content").unwrap().is_null());
        assert_eq!(value.get("reasoning_content").unwrap(), "need file");
        assert_eq!(
            value.pointer("/tool_calls/0/function/name").unwrap(),
            "read_file"
        );
    }

    #[test]
    fn does_not_truncate_native_tool_call_decision_json() {
        let long_path = format!("{}README.md", "a".repeat(super::DEFAULT_TEXT_CAP));
        let raw = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": serde_json::to_string(&serde_json::json!({
                                "path": long_path,
                            })).unwrap(),
                        },
                    }],
                },
            }],
        })
        .to_string();
        let text = extract_assistant_text(&raw).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            value.pointer("/tool_calls/0/function/name").unwrap(),
            "read_file"
        );
    }

    #[test]
    fn rejects_error_response() {
        let raw = r#"{"error":{"message":"bad key"}}"#;
        assert!(extract_assistant_text(raw)
            .unwrap_err()
            .contains("provider error"));
    }

    #[test]
    fn api_key_rejects_missing_and_blank_values() {
        let missing = parse_api_key(None).unwrap_err();
        assert!(missing.contains("DeepSeek API key is not set"));
        assert!(missing.contains("AI provider or support"));
        assert!(missing.contains("do not include any real API keys"));
        assert!(missing.contains("export DEEPSEEK_API_KEY"));
        assert!(missing.contains("~/.zsh_secrets"));
        assert!(missing.contains("keep provider keys there"));
        assert!(missing.contains("deepseek-arkey login"));

        let blank = parse_api_key(Some(" \t\n ".to_string())).unwrap_err();
        assert_eq!(blank, missing);
    }

    #[test]
    fn api_key_trims_surrounding_whitespace() {
        assert_eq!(
            parse_api_key(Some("  secret-key\n".to_string())).unwrap(),
            "secret-key"
        );
    }

    #[test]
    fn curl_process_args_do_not_include_api_key() {
        let command = curl_command();
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(!args.contains("secret-key"));
        assert_eq!(args, "-q -K -");
    }

    #[test]
    fn curl_config_carries_auth_header_off_argv() {
        let config = curl_config("secret-key", r#"{"messages":[]}"#, false);
        assert!(config.contains("Authorization: Bearer secret-key"));
        assert!(config.contains("Content-Type: application/json"));
        assert!(config.contains("data = "));
    }

    #[test]
    fn curl_quote_escapes_config_string_controls() {
        assert_eq!(curl_quote("a\"b\\c\n\r\t"), "a\\\"b\\\\c\\n\\r\\t");
    }

    struct SlowStreamClient;

    impl HttpClient for SlowStreamClient {
        fn post(&self, _key: &str, _body: &str, _stream: bool) -> Result<Output, String> {
            Err("unused test path".to_string())
        }

        fn post_stream(&self, _key: &str, _body: &str) -> Result<Child, String> {
            Command::new("sh")
                .arg("-c")
                .arg(
                    "printf 'data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\\n\\n'; sleep 5",
                )
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|err| err.to_string())
        }
    }

    #[test]
    fn streaming_chat_returns_cancelled_when_token_is_cancelled() {
        let cancel = CancellationToken::new();
        let err = chat_streaming(
            &SlowStreamClient,
            "key",
            "{}".to_string(),
            false,
            |_| cancel.cancel(),
            Some(&cancel),
        )
        .unwrap_err();
        assert_eq!(err, CANCELLED);
    }
}
