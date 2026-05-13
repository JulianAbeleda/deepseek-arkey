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
    approved_agent_root: Option<&Path>,
    memory_turns: usize,
) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str("mode: chat\n");
    output.push_str(&format!(
        "chat-memory: process\nchat-turns: {memory_turns}\n"
    ));
    output.push_str(&root_status(root, explicit_root));
    match approved_agent_root {
        Some(root) => output.push_str(&format!(
            "agent-session: allowed\nagent-root: {}\n",
            root.display()
        )),
        None => output.push_str("agent-session: confirm-required\n"),
    }
    Ok(output)
}
