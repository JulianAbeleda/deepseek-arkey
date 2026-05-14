use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_TEXT_CAP: usize = 40_000;

const REDACTION_PATTERNS: [&str; 6] = [
    "api_key=",
    "apikey=",
    "token=",
    "secret=",
    "password=",
    "authorization:",
];

pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path has no parent: {}", path.display()),
        )
    })?;
    fs::create_dir_all(parent)?;
    let tmp_path = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("atomic-write"),
        unique_suffix()
    ));
    let mut file = File::create(&tmp_path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp_path, path).inspect_err(|_err| {
        let _ = fs::remove_file(&tmp_path);
    })
}

pub fn migrate_file_if_missing(new_path: &Path, old_path: &Path) -> io::Result<()> {
    if new_path.exists() || !old_path.exists() {
        return Ok(());
    }
    let raw = fs::read(old_path)?;
    atomic_write(new_path, &raw)
}

pub fn unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

pub fn unix_timestamp_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default()
}

pub fn cap_text(text: &str, max_chars: usize) -> String {
    let mut capped: String = text.chars().take(max_chars).collect();
    if text.chars().count() > max_chars {
        capped.push_str("\n[truncated]");
    }
    capped
}

pub fn redact_text(text: &str) -> String {
    text.split_whitespace()
        .map(redact_token)
        .collect::<Vec<_>>()
        .join(" ")
}

fn redact_token(token: &str) -> String {
    let lower = token.to_ascii_lowercase();
    for pattern in REDACTION_PATTERNS {
        if let Some(index) = lower.find(pattern) {
            let value_start = index + pattern.len();
            if value_start < token.len() {
                return format!("{}[REDACTED]", &token[..value_start]);
            }
        }
    }
    token.to_string()
}

fn unique_suffix() -> String {
    let nanos = unix_timestamp_nanos();
    format!("{}-{nanos}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::{cap_text, redact_text};

    #[test]
    fn redacts_secret_tokens() {
        assert_eq!(
            redact_text("hello token=abc password=hunter2"),
            "hello token=[REDACTED] password=[REDACTED]"
        );
    }

    #[test]
    fn caps_text() {
        assert_eq!(cap_text("abcdef", 3), "abc\n[truncated]");
    }
}
