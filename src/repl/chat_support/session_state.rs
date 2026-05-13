use super::super::*;

pub(in crate::repl::chat) fn print_agent_banner(model: &str, root: &Path) {
    println!("deepseek [{model}] agent");
    println!("workspace: {}", root.display());
    println!("read tools on - edits require yes apply - shell requires yes run");
    println!("Enter send - ? help - /chat - /model - /status - /end - /exit");
}

pub(in crate::repl::chat) fn agent_prompt_text(model: &str) -> String {
    format!("deepseek [{model}] agent › ")
}

pub(in crate::repl::chat) fn interactive_agent_status(
    model: &str,
    root: &Path,
) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str(&format!("mode: agent\nroot: {}\n", root.display()));
    Ok(output)
}

pub(in crate::repl::chat) fn reset_persisted_chat_messages() -> Result<Option<SessionState>, String>
{
    let Some(mut state) = session::load()? else {
        return Ok(None);
    };
    if !state.messages.is_empty() {
        state.clear_messages();
        session::save(&state)?;
    }
    Ok(Some(state))
}

pub(in crate::repl::chat) fn update_active_session_model(model: &str) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.model = model.to_string();
    session::save(&state)
}

pub(in crate::repl::chat) fn approve_session_agent_root(root: &Path) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.approve_agent_root(root)?;
    session::save(&state)
}

pub(in crate::repl::chat) fn clear_session_agent_root() -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.clear_agent_root();
    session::save(&state)
}

pub(in crate::repl::chat) fn save_selected_root(root: Option<&Path>) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    match root {
        Some(root) => state.select_root(root)?,
        None => state.clear_selected_root(),
    }
    session::save(&state)
}

pub(in crate::repl::chat) fn agent_root_matches(approved: Option<&Path>, root: &Path) -> bool {
    let Some(approved) = approved else {
        return false;
    };
    paths_equal(approved, root)
}

pub(in crate::repl::chat) fn paths_equal(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}
