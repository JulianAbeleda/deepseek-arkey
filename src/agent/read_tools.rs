use std::fs;
use std::path::Path;

use crate::safety::cap_text;

use super::workspace::Workspace;
use super::ToolCall;

const MAX_TOOL_CHARS: usize = 12_000;
const MAX_READ_BYTES: u64 = 64 * 1024;
const MAX_LIST_ENTRIES: usize = 200;
const MAX_SEARCH_MATCHES: usize = 80;
const MAX_TREE_ENTRIES: usize = 240;
const MAX_TREE_DEPTH: usize = 4;

pub(super) fn list_files(workspace: &Workspace, call: &ToolCall) -> String {
    let path = match arg_path(call) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let dir = match workspace.resolve_existing(&path) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let read_dir = match fs::read_dir(&dir) {
        Ok(read_dir) => read_dir,
        Err(err) => return format!("error: {err}"),
    };
    let mut entries = Vec::new();
    for entry in read_dir.take(MAX_LIST_ENTRIES) {
        let Ok(entry) = entry else {
            continue;
        };
        entries.push(format_entry(workspace, &entry.path()));
    }
    entries.sort();
    if entries.is_empty() {
        "ok: no entries".to_string()
    } else {
        format!("ok:\n{}", entries.join("\n"))
    }
}

pub(super) fn read_file(workspace: &Workspace, call: &ToolCall) -> String {
    let path = match arg_path(call) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let file = match workspace.resolve_existing(&path) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let metadata = match fs::metadata(&file) {
        Ok(metadata) => metadata,
        Err(err) => return format!("error: {err}"),
    };
    if !metadata.is_file() {
        return "error: path is not a file".to_string();
    }
    if metadata.len() > MAX_READ_BYTES {
        return format!("error: file exceeds read cap of {MAX_READ_BYTES} bytes");
    }
    match fs::read_to_string(&file) {
        Ok(content) => format!("ok:\n{}", cap_text(&content, MAX_TOOL_CHARS)),
        Err(err) => format!("error: {err}"),
    }
}

pub(super) fn search_files(workspace: &Workspace, call: &ToolCall) -> String {
    let path = match arg_path(call) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let query = match call.arguments.get("query").and_then(|value| value.as_str()) {
        Some(query) if !query.is_empty() => query,
        _ => return "error: missing non-empty `query`".to_string(),
    };
    let root = match workspace.resolve_existing(&path) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let mut matches = Vec::new();
    collect_search_matches(workspace, &root, query, &mut matches);
    if matches.is_empty() {
        "ok: no matches".to_string()
    } else {
        format!("ok:\n{}", matches.join("\n"))
    }
}

pub(super) fn inspect_tree(workspace: &Workspace, call: &ToolCall) -> String {
    let path = match arg_path(call) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let depth = call
        .arguments
        .get("depth")
        .and_then(|value| value.as_u64())
        .map(|depth| depth as usize)
        .unwrap_or(2)
        .min(MAX_TREE_DEPTH);
    let root = match workspace.resolve_existing(&path) {
        Ok(path) => path,
        Err(err) => return format!("error: {err}"),
    };
    let mut entries = Vec::new();
    collect_tree_entries(workspace, &root, depth, 0, &mut entries);
    if entries.is_empty() {
        "ok: no entries".to_string()
    } else {
        format!("ok:\n{}", entries.join("\n"))
    }
}

fn arg_path(call: &ToolCall) -> Result<String, String> {
    call.arguments
        .get("path")
        .and_then(|value| value.as_str())
        .filter(|path| !path.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "missing non-empty `path`".to_string())
}

fn collect_search_matches(
    workspace: &Workspace,
    path: &Path,
    query: &str,
    matches: &mut Vec<String>,
) {
    if matches.len() >= MAX_SEARCH_MATCHES || is_ignored(path) {
        return;
    }
    if !workspace.contains_existing(path) {
        return;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if metadata.is_dir() {
        let Ok(read_dir) = fs::read_dir(path) else {
            return;
        };
        let mut children: Vec<_> = read_dir
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        children.sort();
        for child in children {
            collect_search_matches(workspace, &child, query, matches);
            if matches.len() >= MAX_SEARCH_MATCHES {
                break;
            }
        }
        return;
    }
    if !metadata.is_file() || metadata.len() > MAX_READ_BYTES {
        return;
    }
    let Ok(content) = fs::read_to_string(path) else {
        return;
    };
    for (index, line) in content.lines().enumerate() {
        if line.contains(query) {
            matches.push(format!(
                "{}:{}: {}",
                workspace.display_path(path),
                index + 1,
                cap_text(line.trim(), 240)
            ));
            if matches.len() >= MAX_SEARCH_MATCHES {
                break;
            }
        }
    }
}

fn collect_tree_entries(
    workspace: &Workspace,
    path: &Path,
    max_depth: usize,
    current_depth: usize,
    entries: &mut Vec<String>,
) {
    if entries.len() >= MAX_TREE_ENTRIES || is_ignored(path) {
        return;
    }
    if !workspace.contains_existing(path) {
        return;
    }
    entries.push(format_entry(workspace, path));
    if current_depth >= max_depth {
        return;
    }
    let Ok(metadata) = fs::metadata(path) else {
        return;
    };
    if !metadata.is_dir() {
        return;
    }
    let Ok(read_dir) = fs::read_dir(path) else {
        return;
    };
    let mut children: Vec<_> = read_dir
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    children.sort();
    for child in children {
        collect_tree_entries(workspace, &child, max_depth, current_depth + 1, entries);
        if entries.len() >= MAX_TREE_ENTRIES {
            break;
        }
    }
}

fn format_entry(workspace: &Workspace, path: &Path) -> String {
    let kind = fs::metadata(path)
        .map(|metadata| {
            if metadata.is_dir() {
                "dir"
            } else if metadata.is_file() {
                "file"
            } else {
                "other"
            }
        })
        .unwrap_or("unknown");
    format!("{kind}\t{}", workspace.display_path(path))
}

fn is_ignored(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| matches!(name, ".git" | "target" | "node_modules"))
}
