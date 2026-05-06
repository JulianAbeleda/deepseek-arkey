use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Output, Stdio};

use serde::{Deserialize, Serialize};

use crate::safety::{cap_text, redact_text, DEFAULT_TEXT_CAP};

pub const PROVIDER: &str = "DeepSeek";
pub const PROVIDER_DIR: &str = "deepseek";
pub const PROVIDER_STATE_DIR: &str = ".deepseek";
pub const ENV_KEY: &str = "DEEPSEEK_API_KEY";
pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";
pub const DEFAULT_SESSION_NAME: &str = "default";
pub const SUPPORTED_MODELS: &[&str] = &["deepseek-v4-flash", "deepseek-v4-pro"];
const API_URL: &str = "https://api.deepseek.com/chat/completions";
const LOGIN_MAX_TOKENS: u32 = 128;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

pub fn message(role: impl Into<String>, content: impl Into<String>) -> Message {
    Message {
        role: role.into(),
        content: content.into(),
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

#[derive(Debug, Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: &'a [Message],
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    stream: bool,
}

pub fn api_key() -> Result<String, String> {
    parse_api_key(std::env::var(ENV_KEY).ok())
}

fn parse_api_key(value: Option<String>) -> Result<String, String> {
    value
        .map(|key| key.trim().to_string())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| format!("{ENV_KEY} is not set"))
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
        max_tokens,
        stream,
        true,
        true,
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
        max_tokens,
        stream,
        true,
        false,
        on_delta,
    )
}

pub fn chat_with_delta_quiet_cache<F>(
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
        max_tokens,
        stream,
        false,
        false,
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
        max_tokens,
        false,
        false,
        false,
        |_| {},
    )
}

fn chat_impl<F>(
    messages: &[Message],
    model: &str,
    temperature: Option<f32>,
    max_tokens: Option<u32>,
    stream: bool,
    print_cache: bool,
    print_stream_trailing_newline: bool,
    on_delta: F,
) -> Result<String, String>
where
    F: FnMut(&str),
{
    let key = api_key()?;
    let body = serde_json::to_string(&ChatRequest {
        model,
        messages,
        temperature,
        max_tokens,
        stream,
    })
    .map_err(|err| err.to_string())?;
    let client = CurlHttpClient;
    if stream {
        return chat_streaming(&client, &key, body, print_stream_trailing_newline, on_delta);
    }
    let output = client.post(&key, &body, false)?;
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
    extract_assistant_text(&raw)
}

fn chat_streaming<F>(
    client: &impl HttpClient,
    key: &str,
    body: String,
    print_trailing_newline: bool,
    mut on_delta: F,
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
    if let Some(decision) = openai_tool_decision_text(message) {
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

fn openai_tool_decision_text(message: &serde_json::Value) -> Option<String> {
    let calls = message.get("tool_calls").filter(|calls| calls.is_array())?;
    let content = message
        .get("content")
        .and_then(|content| content.as_str())
        .map(str::trim)
        .filter(|content| !content.is_empty());
    let decision = serde_json::json!({
        "content": content,
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
    use super::{curl_command, curl_config, curl_quote, extract_assistant_text, parse_api_key};

    #[test]
    fn extracts_assistant_text() {
        let raw = r#"{"choices":[{"message":{"content":"hello"}}]}"#;
        assert_eq!(extract_assistant_text(raw).unwrap(), "hello");
    }

    #[test]
    fn extracts_native_tool_calls_as_agent_decision_json() {
        let raw = r#"{"choices":[{"message":{"content":"","tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"src/main.rs\"}"}}]}}]}"#;
        let text = extract_assistant_text(raw).unwrap();
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert!(value.get("content").unwrap().is_null());
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
        assert!(parse_api_key(None)
            .unwrap_err()
            .contains("DEEPSEEK_API_KEY"));
        assert!(parse_api_key(Some(" \t\n ".to_string()))
            .unwrap_err()
            .contains("DEEPSEEK_API_KEY"));
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
}
