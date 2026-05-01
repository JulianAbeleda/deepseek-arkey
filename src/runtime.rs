use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::safety::atomic_write;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeBackend {
    Provider,
    Debug,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RuntimeState {
    pub backend: RuntimeBackend,
    pub runtime: String,
    pub model: Option<String>,
    pub updated_at: u64,
}

impl RuntimeState {
    pub fn provider(model: Option<String>) -> Self {
        Self {
            backend: RuntimeBackend::Provider,
            runtime: "terminal".to_string(),
            model,
            updated_at: unix_timestamp(),
        }
    }

    pub fn with_backend(mut self, backend: RuntimeBackend) -> Self {
        self.backend = backend;
        self.updated_at = unix_timestamp();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self.updated_at = unix_timestamp();
        self
    }

    pub fn label(&self, fallback_model: &str) -> String {
        let model = self.model.as_deref().unwrap_or(fallback_model);
        match self.backend {
            RuntimeBackend::Provider => model.to_string(),
            RuntimeBackend::Debug => format!("debug:{model}"),
        }
    }
}

impl RuntimeBackend {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "provider" | "deepseek" | "off" => Some(Self::Provider),
            "debug" | "debug-manual" | "manual" | "on" => Some(Self::Debug),
            _ => None,
        }
    }
}

pub fn runtime_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".local/state/provider-cli/deepseek/runtime-state.json");
    }
    PathBuf::from(".deepseek/runtime-state.json")
}

pub fn load(default_model: &str) -> Result<RuntimeState, String> {
    let path = runtime_path();
    if !path.exists() {
        return Ok(RuntimeState::provider(Some(default_model.to_string())));
    }
    let raw = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    serde_json::from_str(&raw).map_err(|err| err.to_string())
}

pub fn save(state: &RuntimeState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state).map_err(|err| err.to_string())?;
    atomic_write(&runtime_path(), &bytes).map_err(|err| err.to_string())
}

pub fn set_backend(default_model: &str, backend: RuntimeBackend) -> Result<RuntimeState, String> {
    let state = load(default_model)?.with_backend(backend);
    save(&state)?;
    Ok(state)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::{RuntimeBackend, RuntimeState};

    #[test]
    fn parses_debug_aliases() {
        assert_eq!(RuntimeBackend::parse("debug"), Some(RuntimeBackend::Debug));
        assert_eq!(RuntimeBackend::parse("manual"), Some(RuntimeBackend::Debug));
        assert_eq!(RuntimeBackend::parse("on"), Some(RuntimeBackend::Debug));
        assert_eq!(
            RuntimeBackend::parse("provider"),
            Some(RuntimeBackend::Provider)
        );
        assert_eq!(RuntimeBackend::parse("off"), Some(RuntimeBackend::Provider));
        assert_eq!(RuntimeBackend::parse("unknown"), None);
    }

    #[test]
    fn labels_debug_backend() {
        let state = RuntimeState::provider(Some("deepseek-v4-flash".to_string()))
            .with_backend(RuntimeBackend::Debug);
        assert_eq!(state.label("fallback"), "debug:deepseek-v4-flash");
    }
}
