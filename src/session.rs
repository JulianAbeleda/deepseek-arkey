use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::provider::{assistant_message, user_message, Message, PROVIDER_DIR, PROVIDER_STATE_DIR};
use crate::safety::atomic_write;

const MAX_TURNS: usize = 20;
const MAX_CHARS: usize = 40_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedRoot(PathBuf);

impl PersistedRoot {
    fn from_path(root: &Path) -> Result<Self, String> {
        Self::from_path_buf(root.to_path_buf())
    }

    fn from_path_buf(root: PathBuf) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err(format!("session root is not absolute: {}", root.display()));
        }
        let root = root.canonicalize().map_err(|err| {
            format!(
                "failed to canonicalize session root {}: {err}",
                root.display()
            )
        })?;
        if !root.is_dir() {
            return Err(format!(
                "session root is not a directory: {}",
                root.display()
            ));
        }
        Ok(Self(root))
    }

    pub fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Serialize for PersistedRoot {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for PersistedRoot {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = PathBuf::deserialize(deserializer)?;
        Self::from_path_buf(raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SessionState {
    pub provider: String,
    pub name: String,
    pub model: String,
    pub updated_at: u64,
    pub messages: Vec<Message>,
    #[serde(
        default,
        deserialize_with = "deserialize_persisted_root_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_root: Option<PersistedRoot>,
    #[serde(
        default,
        deserialize_with = "deserialize_persisted_root_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub agent_root: Option<PersistedRoot>,
}

impl SessionState {
    pub fn new(provider: &str, name: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            provider: provider.to_string(),
            name: name.into(),
            model: model.into(),
            updated_at: unix_timestamp(),
            messages: Vec::new(),
            selected_root: None,
            agent_root: None,
        }
    }

    pub fn push_turn(&mut self, user: String, assistant: String) {
        self.messages.push(user_message(user));
        self.messages.push(assistant_message(assistant));
        self.updated_at = unix_timestamp();
        self.cap_history();
    }

    pub fn approve_agent_root(&mut self, root: &Path) -> Result<(), String> {
        self.agent_root = Some(PersistedRoot::from_path(root)?);
        self.updated_at = unix_timestamp();
        Ok(())
    }

    pub fn clear_agent_root(&mut self) {
        self.agent_root = None;
        self.updated_at = unix_timestamp();
    }

    pub fn select_root(&mut self, root: &Path) -> Result<(), String> {
        self.selected_root = Some(PersistedRoot::from_path(root)?);
        self.updated_at = unix_timestamp();
        Ok(())
    }

    pub fn clear_selected_root(&mut self) {
        self.selected_root = None;
        self.updated_at = unix_timestamp();
    }

    pub fn clear_messages(&mut self) {
        self.messages.clear();
        self.updated_at = unix_timestamp();
    }

    pub fn selected_root_path(&self) -> Option<PathBuf> {
        self.selected_root.as_ref().map(PersistedRoot::path)
    }

    pub fn agent_root_path(&self) -> Option<PathBuf> {
        self.agent_root.as_ref().map(PersistedRoot::path)
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

fn deserialize_persisted_root_option<'de, D>(
    deserializer: D,
) -> Result<Option<PersistedRoot>, D::Error>
where
    D: Deserializer<'de>,
{
    let root = Option::<PathBuf>::deserialize(deserializer)?;
    let Some(root) = root else {
        return Ok(None);
    };
    // User-selected roots fail loudly. Stale persisted roots warn and
    // degrade softly so a moved directory does not brick session loading.
    match PersistedRoot::from_path_buf(root) {
        Ok(root) => Ok(Some(root)),
        Err(err) => {
            eprintln!("warning: dropping invalid persisted root: {err}");
            Ok(None)
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
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir();
            for attempt in 0..100 {
                let unique = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos();
                let path = base.join(format!(
                    "deepseek-{name}-{}-{unique}-{attempt}",
                    std::process::id()
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(err) => panic!("failed to create test dir {}: {err}", path.display()),
                }
            }
            panic!("failed to create unique test dir for {name}");
        }

        fn path(&self) -> &Path {
            &self.path
        }

        fn join(&self, path: impl AsRef<Path>) -> PathBuf {
            self.path.join(path)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            if let Err(err) = std::fs::remove_dir_all(&self.path) {
                if std::thread::panicking() {
                    eprintln!("failed to remove test dir {}: {err}", self.path.display());
                } else {
                    panic!("failed to remove test dir {}: {err}", self.path.display());
                }
            }
        }
    }

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
        let root = TestDir::new("session-roundtrip-test");
        let path = root.join("active-session.json");

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
    }

    #[test]
    fn stores_agent_root_permission() {
        let root = TestDir::new("agent-root");
        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");

        state.approve_agent_root(root.path()).unwrap();
        assert_eq!(
            state.agent_root_path().as_deref(),
            Some(root.path().canonicalize().unwrap().as_path())
        );

        state.clear_agent_root();
        assert_eq!(state.agent_root_path(), None);
    }

    #[test]
    fn clears_messages_without_clearing_session_metadata() {
        let root = TestDir::new("agent-root-metadata");
        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");
        state.push_turn("hello".to_string(), "world".to_string());
        state.approve_agent_root(root.path()).unwrap();

        state.clear_messages();

        assert!(state.messages.is_empty());
        assert_eq!(state.model, "model-a");
        assert_eq!(
            state.agent_root_path().as_deref(),
            Some(root.path().canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn saves_and_loads_agent_root_permission() {
        let root = TestDir::new("session-agent-root-test");
        let path = root.join("active-session.json");
        let approved_root = root.join("workspace");
        std::fs::create_dir_all(&approved_root).unwrap();

        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");
        state.approve_agent_root(&approved_root).unwrap();
        save_to_path(&path, &state).unwrap();

        let loaded = load_from_path(&path).unwrap().unwrap();
        assert_eq!(
            loaded.agent_root_path().as_deref(),
            Some(approved_root.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn loads_legacy_string_roots() {
        let root = TestDir::new("legacy-roots-test");
        let selected = root.join("selected");
        let agent = root.join("agent");
        std::fs::create_dir_all(&selected).unwrap();
        std::fs::create_dir_all(&agent).unwrap();
        let raw = format!(
            r#"{{
          "provider": "DeepSeek",
          "name": "default",
          "model": "model-a",
          "updated_at": 1,
          "messages": [],
          "selected_root": {},
          "agent_root": {}
        }}"#,
            serde_json::to_string(&selected).unwrap(),
            serde_json::to_string(&agent).unwrap()
        );

        let loaded: SessionState = serde_json::from_str(&raw).unwrap();

        assert_eq!(
            loaded.selected_root_path().as_deref(),
            Some(selected.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            loaded.agent_root_path().as_deref(),
            Some(agent.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn normalizes_persisted_roots() {
        let root = TestDir::new("normalize-roots-test");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let spelling = workspace.join("..").join("workspace");
        let mut state = SessionState::new(PROVIDER, DEFAULT_SESSION_NAME, "model-a");

        state.approve_agent_root(&spelling).unwrap();

        assert_eq!(
            state.agent_root_path().as_deref(),
            Some(workspace.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn drops_relative_persisted_roots_on_session_load() {
        let raw = r#"{
          "provider": "DeepSeek",
          "name": "default",
          "model": "model-a",
          "updated_at": 1,
          "messages": [],
          "selected_root": "relative/path"
        }"#;

        let loaded = serde_json::from_str::<SessionState>(raw).unwrap();

        assert_eq!(loaded.selected_root_path(), None);
    }

    #[test]
    fn drops_stale_persisted_roots_on_session_load() {
        let missing = std::env::temp_dir().join(format!(
            "deepseek-missing-session-root-{}",
            std::process::id()
        ));
        let raw = format!(
            r#"{{
          "provider": "DeepSeek",
          "name": "default",
          "model": "model-a",
          "updated_at": 1,
          "messages": [],
          "selected_root": {},
          "agent_root": {}
        }}"#,
            serde_json::to_string(&missing).unwrap(),
            serde_json::to_string(&missing).unwrap()
        );

        let loaded = serde_json::from_str::<SessionState>(&raw).unwrap();

        assert_eq!(loaded.selected_root_path(), None);
        assert_eq!(loaded.agent_root_path(), None);
    }
}
