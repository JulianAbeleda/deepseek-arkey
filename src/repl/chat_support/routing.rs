use super::super::*;

pub(in crate::repl::chat) fn start_context_scan(
    composer: &mut DockedComposer,
    tool_steps: &[agent::AgentStep],
) -> Result<Instant, String> {
    let started = Instant::now();
    composer.hide_cursor()?;
    composer.progress_dock(&context_scan_status(started, tool_steps))?;
    Ok(started)
}

pub(in crate::repl::chat) fn context_scan_status(
    started: Instant,
    tool_steps: &[agent::AgentStep],
) -> String {
    let mut lines = vec![format!("Loading {}s", started.elapsed().as_secs())];
    if !tool_steps.is_empty() {
        lines.push(String::new());
        lines.extend(
            tool_steps
                .iter()
                .map(|step| format!("agent step {}: {}", step.label(), step.tool)),
        );
    }
    lines.join("\n")
}

pub(in crate::repl::chat) fn agent_route_confirmation(root: &Path) -> String {
    format!(
        "route: agent task\nroot: {}\nRun this as an agent task?\nType y to continue, n to cancel, or /chat to keep chatting.\n",
        root.display()
    )
}

pub(in crate::repl::chat) fn clarify_route_text() -> String {
    "route: unclear\nDo you want chat analysis or an agent task?\nType /chat to discuss, /root <path> to choose a workspace, or /agent <task> to execute.\n".to_string()
}

pub(in crate::repl::chat) enum ShellReadCommand {
    Pwd,
    Ls(String),
}

pub(in crate::repl::chat) fn parse_shell_read_command(prompt: &str) -> Option<ShellReadCommand> {
    let prompt = prompt.trim();
    if prompt == "pwd" {
        return Some(ShellReadCommand::Pwd);
    }
    if prompt == "ls" {
        return Some(ShellReadCommand::Ls("list files".to_string()));
    }
    prompt
        .strip_prefix("ls ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| ShellReadCommand::Ls(format!("list files in {path}")))
}

pub(in crate::repl::chat) fn shell_pwd_text(root: Option<&Path>) -> String {
    match root {
        Some(root) => format!("{}\n", root.display()),
        None => "root: unset\n".to_string(),
    }
}

pub(in crate::repl::chat) fn no_pending_agent_task_text() -> String {
    "route: unclear\nNo pending agent task to confirm.\nType /root <path> to choose a workspace, then repeat the task; or type /agent <task> with the leading slash to run one directly.\n".to_string()
}

pub(in crate::repl::chat) fn is_agent_task_choice(prompt: &str) -> bool {
    matches!(prompt, "y" | "yes" | "yes agent" | "agent task" | "agent")
}

pub(in crate::repl::chat) fn is_agent_task_cancel_choice(prompt: &str) -> bool {
    matches!(prompt, "n" | "no")
}

pub(in crate::repl::chat) fn task_root_for_prompt(
    prompt: &str,
    selected_root: Option<&Path>,
) -> Option<PathBuf> {
    infer_natural_root(prompt)
        .or_else(|| selected_root.map(Path::to_path_buf))
        .or_else(|| effective_workspace_root(None))
}

pub(in crate::repl::chat) fn workspace_agent_root_for_prompt(
    prompt: &str,
    selected_root: Option<&Path>,
) -> Option<PathBuf> {
    if is_workspace_chat_followup(&normalize_workspace_prompt(prompt)) {
        return None;
    }
    infer_natural_root(prompt).or_else(|| {
        is_workspace_agent_prompt(prompt)
            .then(|| effective_workspace_root(selected_root))
            .flatten()
    })
}

pub(in crate::repl::chat) fn is_workspace_agent_prompt(prompt: &str) -> bool {
    let normalized = normalize_workspace_prompt(prompt);
    if normalized.is_empty() || is_workspace_chat_followup(&normalized) {
        return false;
    }
    if normalized.contains("main branch") {
        return false;
    }
    if is_commit_audit_prompt(&normalized) {
        return true;
    }
    let first = normalized.split_whitespace().next().unwrap_or("");
    if matches!(
        first,
        "analyze" | "audit" | "inspect" | "list" | "read" | "review" | "scan" | "summarize"
    ) {
        return true;
    }
    [
        "repo structure",
        "repository structure",
        "project structure",
        "codebase structure",
        "main modules",
        "list files",
        "what is this repo trying to do",
        "what is this repository trying to do",
        "what is this project trying to do",
        "tell me what this repo is trying to do",
        "tell me what this project is trying to do",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

pub(in crate::repl::chat) fn is_workspace_chat_followup(prompt: &str) -> bool {
    prompt == "hi"
        || prompt == "hello"
        || prompt.starts_with("does that ")
        || prompt.starts_with("does this ")
        || prompt.starts_with("what is a ")
        || prompt.starts_with("what are ")
        || prompt.starts_with("why ")
        || prompt.starts_with("how ")
        || prompt.starts_with("should ")
        || prompt.starts_with("stay in touch")
}

pub(in crate::repl::chat) fn normalize_workspace_prompt(prompt: &str) -> String {
    prompt
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_punctuation() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
