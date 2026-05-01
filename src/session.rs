use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::provider::Message;
use crate::safety::atomic_write;

const MAX_TURNS: usize = 20;
const MAX_CHARS: usize = 40_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionState {
    pub provider: String,
    pub name: String,
    pub model: String,
    pub updated_at: u64,
    pub messages: Vec<Message>,
}

impl SessionState {
    pub fn new(provider: &str, name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.to_string(),
            name: name.into(),
            model: model.into(),
            updated_at: unix_timestamp(),
            messages: Vec::new(),
        }
    }

    pub fn push_turn(&mut self, user: String, assistant: String) {
        self.messages.push(Message {
            role: "user".to_string(),
            content: user,
        });
        self.messages.push(Message {
            role: "assistant".to_string(),
            content: assistant,
        });
        self.updated_at = unix_timestamp();
        self.cap_history();
    }

    fn cap_history(&mut self) {
        let max_messages = MAX_TURNS * 2;
        if self.messages.len() > max_messages {
            let drop_count = self.messages.len() - max_messages;
            self.messages.drain(0..drop_count);
        }
        while total_chars(&self.messages) > MAX_CHARS && self.messages.len() > 2 {
            self.messages.drain(0..2);
        }
    }
}

pub fn session_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/provider-cli/deepseek/active-session.json");
    }
    PathBuf::from(".deepseek/active-session.json")
}

pub fn load() -> Result<Option<SessionState>, String> {
    let path = session_path();
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|err| err.to_string())
}

pub fn save(state: &SessionState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
    atomic_write(&session_path(), &bytes).map_err(|err| err.to_string())
}

pub fn delete() -> Result<bool, String> {
    let path = session_path();
    if !path.exists() {
        return Ok(false);
    }
    fs::remove_file(path).map_err(|err| err.to_string())?;
    Ok(true)
}

fn total_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum()
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::SessionState;

    #[test]
    fn caps_turn_count() {
        let mut state = SessionState::new("DeepSeek", "default", "model");
        for index in 0..25 {
            state.push_turn(format!("u{index}"), format!("a{index}"));
        }
        assert_eq!(state.messages.len(), 40);
        assert_eq!(state.messages[0].content, "u5");
    }
}
