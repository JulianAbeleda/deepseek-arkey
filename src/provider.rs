use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};

use crate::safety::{cap_text, redact_text, DEFAULT_TEXT_CAP};

pub const PROVIDER: &str = "DeepSeek";
pub const ENV_KEY: &str = "DEEPSEEK_API_KEY";
pub const DEFAULT_MODEL: &str = "deepseek-v4-flash";
const API_URL: &str = "https://api.deepseek.com/chat/completions";
const LOGIN_MAX_TOKENS: u32 = 128;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Message {
    pub role: String,
    pub content: String,
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
    std::env::var(ENV_KEY)
        .map(|key| key.trim().to_string())
        .ok()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| format!("{ENV_KEY} is not set"))
}

pub fn login_check(model: &str) -> Result<(), String> {
    let messages = vec![Message {
        role: "user".to_string(),
        content: "Say exactly: OK".to_string(),
    }];
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
    chat_with_delta(messages, model, temperature, max_tokens, stream, |delta| {
        print!("{delta}");
        let _ = std::io::stdout().flush();
    })
}

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
    let key = api_key()?;
    let body = serde_json::to_string(&ChatRequest {
        model,
        messages,
        temperature,
        max_tokens,
        stream,
    })
    .map_err(|err| err.to_string())?;
    if stream {
        return chat_streaming(&key, body, on_delta);
    }
    let output = Command::new("curl")
        .arg("-sS")
        .arg("-X")
        .arg("POST")
        .arg(API_URL)
        .arg("-H")
        .arg(format!("Authorization: Bearer {key}"))
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-d")
        .arg(body)
        .output()
        .map_err(|err| format!("failed to run curl: {err}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            "provider request failed".to_string()
        } else {
            redact_text(&stderr)
        });
    }
    let raw = String::from_utf8_lossy(&output.stdout).to_string();
    print_cache_stats(&raw);
    extract_assistant_text(&raw).map(|text| cap_text(&text, DEFAULT_TEXT_CAP))
}

fn chat_streaming<F>(key: &str, body: String, mut on_delta: F) -> Result<String, String>
where
    F: FnMut(&str),
{
    let mut child = Command::new("curl")
        .arg("-sS")
        .arg("-N")
        .arg("-X")
        .arg("POST")
        .arg(API_URL)
        .arg("-H")
        .arg(format!("Authorization: Bearer {key}"))
        .arg("-H")
        .arg("Content-Type: application/json")
        .arg("-d")
        .arg(body)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("failed to run curl: {err}"))?;
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
    if !response.ends_with('\n') {
        println!();
    }
    Ok(cap_text(response.trim(), DEFAULT_TEXT_CAP))
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
    value
        .get("choices")
        .and_then(|choices| choices.get(0))
        .and_then(|choice| choice.get("message"))
        .and_then(|message| message.get("content"))
        .and_then(|content| content.as_str())
        .map(|content| content.trim().to_string())
        .filter(|content| !content.is_empty())
        .ok_or_else(|| {
            format!(
                "provider response did not include assistant text: {}",
                cap_text(raw, 1000)
            )
        })
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
    use super::extract_assistant_text;

    #[test]
    fn extracts_assistant_text() {
        let raw = r#"{"choices":[{"message":{"content":"hello"}}]}"#;
        assert_eq!(extract_assistant_text(raw).unwrap(), "hello");
    }

    #[test]
    fn rejects_error_response() {
        let raw = r#"{"error":{"message":"bad key"}}"#;
        assert!(extract_assistant_text(raw)
            .unwrap_err()
            .contains("provider error"));
    }
}
