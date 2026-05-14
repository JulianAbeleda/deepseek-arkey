use super::super::*;

pub(in crate::repl::chat) fn interactive_status(model: &str) -> Result<String, String> {
    let mut output = format!(
        "DeepSeek Status\nsession-path: {}\n",
        session::session_path().display()
    );
    let runtime_state = runtime::load(model)?;
    output.push_str(&format!(
        "backend: {:?}\nruntime: {}\n",
        runtime_state.backend, runtime_state.runtime
    ));
    match session::load()? {
        Some(state) => {
            output.push_str(&format!(
                "session: {}\nmodel: {}\nturns: {}\nhealth: ok\n",
                state.name,
                state.model,
                state.messages.len() / 2
            ));
        }
        None => {
            output.push_str(&format!(
                "session: none\nmodel: {model}\nhealth: stateless\n"
            ));
        }
    }
    Ok(output)
}

pub(in crate::repl::chat) fn interactive_chat_status(
    model: &str,
    root: Option<&Path>,
    explicit_root: bool,
    session_approved_scopes: &HashSet<ApprovalGrant>,
    memory_turns: usize,
) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str("mode: chat\n");
    output.push_str(&format!(
        "chat-memory: process\nchat-turns: {memory_turns}\n"
    ));
    output.push_str(&root_status(root, explicit_root));
    output.push_str("agent-routing: direct\n");
    if let Some(root) = root {
        let write = approval_status(root, ApprovalScope::Write, session_approved_scopes);
        let shell = approval_status(root, ApprovalScope::Shell, session_approved_scopes);
        output.push_str(&format!(
            "write-permission: {write}\nshell-permission: {shell}\n"
        ));
    } else {
        output.push_str("write-permission: confirm-required\nshell-permission: confirm-required\n");
    }
    Ok(output)
}

fn approval_status(
    root: &Path,
    scope: ApprovalScope,
    session_approved_scopes: &HashSet<ApprovalGrant>,
) -> &'static str {
    let Ok(root) = root.canonicalize() else {
        return "confirm-required";
    };
    if session_approved_scopes.contains(&ApprovalGrant { root, scope }) {
        "approved for root"
    } else {
        "confirm-required"
    }
}
