use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::safety::{atomic_write, cap_text, redact_text};

use super::workspace::Workspace;
use super::{ApprovalMode, ToolCall};

const MAX_PATCH_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone)]
pub(super) struct PreparedPatch {
    path: PathBuf,
    pub(super) display_path: String,
    reason: String,
    find: String,
    replace: String,
    original: String,
    updated: String,
}

pub(super) fn run_shell(
    workspace: &Workspace,
    call: &ToolCall,
    approval_mode: ApprovalMode,
) -> String {
    let command = match arg_string(call, "command") {
        Ok(command) => command,
        Err(err) => return format!("error: {err}"),
    };
    let cwd = call
        .arguments
        .get("cwd")
        .and_then(|value| value.as_str())
        .filter(|cwd| !cwd.is_empty())
        .unwrap_or(".");
    let cwd = match workspace.resolve_existing(cwd) {
        Ok(cwd) => cwd,
        Err(err) => return format!("error: {err}"),
    };
    if !cwd.is_dir() {
        return "error: cwd is not a directory".to_string();
    }
    let reason = call
        .arguments
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("no reason provided");
    if !approve_shell_command(workspace, &command, &cwd, reason, approval_mode) {
        return "denied: run_shell requires explicit interactive approval".to_string();
    }
    let output = match Command::new("sh")
        .arg("-lc")
        .arg(&command)
        .current_dir(&cwd)
        .output()
    {
        Ok(output) => output,
        Err(err) => return format!("error: failed to run shell command: {err}"),
    };
    format!(
        "status: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

pub(super) fn propose_patch(
    workspace: &Workspace,
    call: &ToolCall,
    approval_mode: ApprovalMode,
) -> String {
    let patch = match prepare_patch(workspace, call) {
        Ok(patch) => patch,
        Err(err) => return format!("error: {err}"),
    };
    if !approve_patch(&patch, approval_mode) {
        return "denied: propose_patch requires explicit interactive approval".to_string();
    }
    match apply_prepared_patch(&patch) {
        Ok(()) => format!("ok: patched {}", patch.display_path),
        Err(err) => format!("error: failed to apply patch: {err}"),
    }
}

pub(super) fn prepare_patch(
    workspace: &Workspace,
    call: &ToolCall,
) -> Result<PreparedPatch, String> {
    let requested = arg_path(call)?;
    let find = arg_string(call, "find")?;
    let replace = match call
        .arguments
        .get("replace")
        .and_then(|value| value.as_str())
    {
        Some(replace) => replace.to_string(),
        None => return Err("missing string `replace`".to_string()),
    };
    let reason = arg_string(call, "reason")?;
    let path = workspace.resolve_existing(&requested)?;
    if !path.is_file() {
        return Err("path is not a file".to_string());
    }
    let metadata = fs::metadata(&path).map_err(|err| err.to_string())?;
    if metadata.len() > MAX_PATCH_BYTES {
        return Err(format!("file exceeds patch cap of {MAX_PATCH_BYTES} bytes"));
    }
    let original = fs::read_to_string(&path).map_err(|err| format!("file is not UTF-8: {err}"))?;
    let matches = original.matches(&find).count();
    if matches == 0 {
        return Err("find text was not present".to_string());
    }
    if matches > 1 {
        return Err("find text matched more than once".to_string());
    }
    let updated = original.replacen(&find, &replace, 1);
    Ok(PreparedPatch {
        display_path: workspace.display_path(&path),
        path,
        reason,
        find,
        replace,
        original,
        updated,
    })
}

pub(super) fn apply_prepared_patch(patch: &PreparedPatch) -> io::Result<()> {
    let current = fs::read_to_string(&patch.path)?;
    if current != patch.original {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file changed after patch was prepared",
        ));
    }
    atomic_write(&patch.path, patch.updated.as_bytes())
}

fn approve_patch(patch: &PreparedPatch, approval_mode: ApprovalMode) -> bool {
    if approval_mode == ApprovalMode::Approved {
        return true;
    }
    if approval_mode == ApprovalMode::Deny {
        return false;
    }
    if !io::stdin().is_terminal() {
        return false;
    }
    eprintln!("agent requests file edit");
    eprintln!("path: {}", patch.display_path);
    eprintln!("reason: {}", redact_text(&cap_text(&patch.reason, 1200)));
    eprintln!("--- find ---");
    eprintln!("find:\n{}", redact_text(&cap_text(&patch.find, 1200)));
    eprintln!("--- replace ---");
    eprintln!("replace:\n{}", redact_text(&cap_text(&patch.replace, 1200)));
    eprint!("Type yes apply to apply this edit: ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map(|_| answer.trim() == "yes apply")
        .unwrap_or(false)
}

fn approve_shell_command(
    workspace: &Workspace,
    command: &str,
    cwd: &Path,
    reason: &str,
    approval_mode: ApprovalMode,
) -> bool {
    if approval_mode == ApprovalMode::Approved {
        return true;
    }
    if approval_mode == ApprovalMode::Deny {
        return false;
    }
    if !io::stdin().is_terminal() {
        return false;
    }
    eprintln!("agent requests shell execution");
    eprintln!("cwd: {}", workspace.display_path(cwd));
    eprintln!("reason: {}", redact_text(reason));
    eprintln!("--- command ---");
    eprintln!("command: {}", redact_text(command));
    eprint!("Type yes run to run this command: ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    io::stdin()
        .read_line(&mut answer)
        .map(|_| answer.trim() == "yes run")
        .unwrap_or(false)
}

fn arg_path(call: &ToolCall) -> Result<String, String> {
    call.arguments
        .get("path")
        .and_then(|value| value.as_str())
        .filter(|path| !path.is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "missing non-empty `path`".to_string())
}

fn arg_string(call: &ToolCall, name: &str) -> Result<String, String> {
    call.arguments
        .get(name)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| format!("missing non-empty `{name}`"))
}
