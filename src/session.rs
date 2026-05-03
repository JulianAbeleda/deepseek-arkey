use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::provider::{assistant_message, user_message, Message, PROVIDER_DIR, PROVIDER_STATE_DIR};
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_root: Option<String>,
}

impl SessionState {
    pub fn new(provider: &str, name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.to_string(),
            name: name.into(),
            model: model.into(),
            updated_at: unix_timestamp(),
            messages: Vec::new(),
            agent_root: None,
        }
    }

    pub fn push_turn(&mut self, user: String, assistant: String) {
        self.messages.push(user_message(user));
        self.messages.push(assistant_message(assistant));
        self.updated_at = unix_timestamp();
        self.cap_history();
    }

    pub fn approve_agent_root(&mut self, root: &Path) {
        self.agent_root = Some(root.display().to_string());
        self.updated_at = unix_timestamp();
    }

    pub fn clear_agent_root(&mut self) {
        self.agent_root = None;
        self.updated_at = unix_timestamp();
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.updated_at = unix_timestamp();
    }

    pub fn agent_root_path(&self) -> Option<PathBuf> {
        self.agent_root.as_ref().map(PathBuf::from)
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
        return PathBuf::from(home)
            .join(".local/state/provider-cli")
            .join(PROVIDER_DIR)
            .join("active-session.json");
    }
    PathBuf::from(PROVIDER_STATE_DIR).join("active-session.json")
}

pub fn load() -> Result<Option<SessionState>, String> {
    let path = session_path();
    load_from_path(&path)
}

fn load_from_path(path: &Path) -> Result<Option<SessionState>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|err| err.to_string())?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|err| err.to_string())
}

pub fn save(state: &SessionState) -> Result<(), String> {
    save_to_path(&session_path(), state)
}

fn save_to_path(path: &Path, state: &SessionState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
    atomic_write(path, &bytes).map_err(|err| err.to_string())
}

pub fn delete() -> Result<bool, String> {
    let path = session_path();
    delete_path(&path)
}

fn delete_path(path: &Path) -> Result<bool, String> {
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
    use super::{delete_path, load_from_path, save_to_path, SessionState};
    use crate::provider::{DEFAULT_SESSION_NAME, PROVIDER};

    #[test]
    fn caps_turn_count() {
        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model");
        for index in 0..25 {
            state.push_turn(format!("u{index}"), format!("a{index}"));
        }
        assert_eq!(state.messages.len(), 40);
        assert_eq!(state.messages[0].content, "u5");
    }

    #[test]
    fn save_load_delete_round_trip() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-session-roundtrip-test-{}",
            std::process::id()
        ));
        let path = root.join("active-session.json");
        let _ = std::fs::remove_dir_all(&root);

        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");
        state.push_turn("hello".to_string(), "world".to_string());
        save_to_path(&path, &state).unwrap();

        let loaded = load_from_path(&path).unwrap().unwrap();
        assert_eq!(loaded.provider, PROVIDER);
        assert_eq!(loaded.name, DEFAULT_SESSION_NAME);
        assert_eq!(loaded.model, "model-a");
        assert_eq!(loaded.agent_root, None);
        assert_eq!(loaded.messages.len(), 2);
        assert_eq!(loaded.messages[0].role, "user");
        assert_eq!(loaded.messages[0].content, "hello");
        assert_eq!(loaded.messages[1].role, "assistant");
        assert_eq!(loaded.messages[1].content, "world");

        assert!(delete_path(&path).unwrap());
        assert!(load_from_path(&path).unwrap().is_none());
        assert!(!delete_path(&path).unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stores_agent_root_permission() {
        let root = std::env::temp_dir().join("deepseek-agent-root");
        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");

        state.approve_agent_root(&root);
        assert_eq!(state.agent_root_path().as_deref(), Some(root.as_path()));

        state.clear_agent_root();
        assert_eq!(state.agent_root_path(), None);
    }

    #[test]
    fn clears_messages_without_clearing_session_metadata() {
        let root = std::env::temp_dir().join("deepseek-agent-root");
        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");
        state.push_turn("hello".to_string(), "world".to_string());
        state.approve_agent_root(&root);

        state.clear_messages();

        assert!(state.messages.is_empty());
        assert_eq!(state.model, "model-a");
        assert_eq!(state.agent_root_path().as_deref(), Some(root.as_path()));
    }

    #[test]
    fn saves_and_loads_agent_root_permission() {
        let root = std::env::temp_dir().join(format!(
            "deepseek-session-agent-root-test-{}",
            std::process::id()
        ));
        let path = root.join("active-session.json");
        let approved_root = root.join("workspace");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&approved_root).unwrap();

        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");
        state.approve_agent_root(&approved_root);
        save_to_path(&path, &state).unwrap();

        let loaded = load_from_path(&path).unwrap().unwrap();
        assert_eq!(
            loaded.agent_root_path().as_deref(),
            Some(approved_root.as_path())
        );
        let _ = std::fs::remove_dir_all(root);
    }
}
