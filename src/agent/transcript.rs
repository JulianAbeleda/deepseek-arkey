use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::provider::{OLD_PROVIDER_STATE_DIR, PROVIDER_STATE_DIR};
use crate::safety::{atomic_write, cap_text};

const MAX_TRANSCRIPT_ENTRY_CHARS: usize = 12_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
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
    let entries = capped_entries(entries);
    let bytes = serde_json::to_vec_pretty(&entries).map_err(|err| err.to_string())?;
    atomic_write(&path, &bytes).map_err(|err| err.to_string())?;
    Ok(path)
}

fn capped_entries(entries: &[TranscriptEntry]) -> Vec<TranscriptEntry> {
    entries
        .iter()
        .map(|entry| TranscriptEntry {
            role: entry.role.clone(),
            content: cap_text(&entry.content, MAX_TRANSCRIPT_ENTRY_CHARS),
        })
        .collect()
}

pub(super) fn read_latest_transcript(
    root: impl Into<PathBuf>,
    canonicalize_root: impl FnOnce(PathBuf) -> Result<PathBuf, String>,
) -> Result<Option<(PathBuf, String)>, String> {
    let root = canonicalize_root(root.into())?;
    let dir = transcript_dir(&root);
    migrate_transcripts_if_needed(&dir, &old_transcript_dir(&root))?;
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

fn old_transcript_dir(root: &Path) -> PathBuf {
    root.join(OLD_PROVIDER_STATE_DIR).join("agent-transcripts")
}

fn migrate_transcripts_if_needed(new_dir: &Path, old_dir: &Path) -> Result<(), String> {
    if new_dir.exists() || !old_dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(new_dir).map_err(|err| err.to_string())?;
    for entry in fs::read_dir(old_dir).map_err(|err| err.to_string())? {
        let entry = entry.map_err(|err| err.to_string())?;
        let old_path = entry.path();
        if !old_path.is_file() {
            continue;
        }
        let Some(file_name) = old_path.file_name() else {
            continue;
        };
        let new_path = new_dir.join(file_name);
        fs::copy(&old_path, &new_path).map_err(|err| err.to_string())?;
    }
    Ok(())
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}
