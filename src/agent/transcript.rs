use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::provider::PROVIDER_STATE_DIR;
use crate::safety::{atomic_write, cap_text};

const MAX_TRANSCRIPT_CHARS: usize = 80_000;

#[derive(Debug, Clone, Serialize)]
pub(super) struct TranscriptEntry {
    pub(super) role: String,
    pub(super) content: String,
}

pub(super) fn write_transcript(
    root: &Path,
    entries: &[TranscriptEntry],
) -> Result<PathBuf, String> {
    let dir = transcript_dir(root);
    let path = dir.join(format!("{}.json", unix_timestamp()));
    let bytes = serde_json::to_vec_pretty(entries).map_err(|err| err.to_string())?;
    let text = cap_text(&String::from_utf8_lossy(&bytes), MAX_TRANSCRIPT_CHARS);
    atomic_write(&path, text.as_bytes()).map_err(|err| err.to_string())?;
    Ok(path)
}

pub(super) fn read_latest_transcript(
    root: impl Into<PathBuf>,
    canonicalize_root: impl FnOnce(PathBuf) -> Result<PathBuf, String>,
) -> Result<Option<(PathBuf, String)>, String> {
    let root = canonicalize_root(root.into())?;
    let dir = transcript_dir(&root);
    if !dir.exists() {
        return Ok(None);
    }
    let mut entries = fs::read_dir(&dir)
        .map_err(|err| err.to_string())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    entries.sort();
    let Some(path) = entries.pop() else {
        return Ok(None);
    };
    let content = fs::read_to_string(&path).map_err(|err| err.to_string())?;
    Ok(Some((path, content)))
}

fn transcript_dir(root: &Path) -> PathBuf {
    root.join(PROVIDER_STATE_DIR).join("agent-transcripts")
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
