use crate::runtime;
use crate::workspace::parse_root_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeCommand {
    Status,
    LegacyRouting(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatCommand<'a> {
    Help,
    ChatMode,
    SwitchToAgent,
    DirectAgentTask(&'a str),
    Status,
    Root(Option<&'a str>),
    Runtime(RuntimeCommand),
    Debug(Option<&'a str>),
    Model(Option<&'a str>),
    End,
    Exit,
}

pub(super) fn parse_chat_command(prompt: &str) -> Option<ChatCommand<'_>> {
    if is_exit_command(prompt) {
        return Some(ChatCommand::Exit);
    }
    if is_end_command(prompt) {
        return Some(ChatCommand::End);
    }
    if matches!(prompt, "?" | "/help") {
        return Some(ChatCommand::Help);
    }
    if prompt == "/chat" {
        return Some(ChatCommand::ChatMode);
    }
    if prompt == "/agent" {
        return Some(ChatCommand::SwitchToAgent);
    }
    if let Some(task) = parse_agent_task_command(prompt) {
        return Some(ChatCommand::DirectAgentTask(task));
    }
    if prompt == "/status" {
        return Some(ChatCommand::Status);
    }
    if let Some(root) = parse_root_command(prompt) {
        return Some(ChatCommand::Root(root));
    }
    if let Some(command) = parse_runtime_command(prompt) {
        return Some(ChatCommand::Runtime(command));
    }
    if let Some(mode) = parse_debug_command(prompt) {
        return Some(ChatCommand::Debug(mode));
    }
    if let Some(model) = parse_model_command(prompt) {
        return Some(ChatCommand::Model(model));
    }
    None
}

pub(super) fn parse_agent_task_command(prompt: &str) -> Option<&str> {
    prompt
        .strip_prefix("/agent ")
        .map(str::trim)
        .filter(|task| !task.is_empty())
}

pub(super) fn parse_debug_command(prompt: &str) -> Option<Option<&str>> {
    if prompt == "/debug" {
        return Some(None);
    }
    prompt.strip_prefix("/debug ").map(|mode| {
        let mode = mode.trim();
        if mode.is_empty() {
            None
        } else {
            Some(mode)
        }
    })
}

pub(super) fn parse_runtime_command(prompt: &str) -> Option<RuntimeCommand> {
    if prompt == "/runtime" {
        return Some(RuntimeCommand::Status);
    }
    let args = prompt.strip_prefix("/runtime ")?.trim();
    match args {
        "legacy-routing on" => Some(RuntimeCommand::LegacyRouting(true)),
        "legacy-routing off" => Some(RuntimeCommand::LegacyRouting(false)),
        _ => Some(RuntimeCommand::Status),
    }
}

pub(super) fn execute_runtime_command(
    model: &str,
    command: RuntimeCommand,
) -> Result<String, String> {
    match command {
        RuntimeCommand::Status => runtime::runtime_result(model, false),
        RuntimeCommand::LegacyRouting(enabled) => {
            let state = runtime::set_legacy_routing(model, enabled)?;
            Ok(runtime::format_runtime_state(&state, model))
        }
    }
}

pub(super) fn parse_model_command(prompt: &str) -> Option<Option<&str>> {
    if prompt == "/model" {
        return Some(None);
    }
    prompt.strip_prefix("/model ").map(|model| {
        let model = model.trim();
        if model.is_empty() {
            None
        } else {
            Some(model)
        }
    })
}

pub(super) fn is_exit_command(prompt: &str) -> bool {
    matches!(
        prompt,
        "exit" | "quit" | "/exit" | "/quit" | "/exit quit" | "/quit exit"
    )
}

pub(super) fn is_end_command(prompt: &str) -> bool {
    matches!(prompt, "session end" | "/end" | "/end session")
}

#[cfg(test)]
mod tests {
    use super::{
        is_end_command, is_exit_command, parse_debug_command, parse_model_command,
        parse_runtime_command, RuntimeCommand,
    };

    #[test]
    fn parses_model_slash_command() {
        assert_eq!(parse_model_command("/model"), Some(None));
        assert_eq!(parse_model_command("/model   "), Some(None));
        assert_eq!(
            parse_model_command("/model deepseek-v4-pro"),
            Some(Some("deepseek-v4-pro"))
        );
        assert_eq!(parse_model_command("model deepseek-v4-flash"), None);
    }

    #[test]
    fn recognizes_exit_commands() {
        for prompt in ["exit", "quit", "/exit", "/quit", "/exit quit", "/quit exit"] {
            assert!(is_exit_command(prompt));
        }
        assert!(!is_exit_command("/end"));
    }

    #[test]
    fn recognizes_end_commands() {
        for prompt in ["session end", "/end", "/end session"] {
            assert!(is_end_command(prompt));
        }
        assert!(!is_end_command("/exit"));
    }

    #[test]
    fn parses_debug_slash_command() {
        assert_eq!(parse_debug_command("/debug"), Some(None));
        assert_eq!(parse_debug_command("/debug off"), Some(Some("off")));
        assert_eq!(parse_debug_command("/debug manual"), Some(Some("manual")));
        assert_eq!(parse_debug_command("debug"), None);
    }

    #[test]
    fn parses_runtime_legacy_routing_command() {
        assert_eq!(
            parse_runtime_command("/runtime"),
            Some(RuntimeCommand::Status)
        );
        assert_eq!(
            parse_runtime_command("/runtime legacy-routing on"),
            Some(RuntimeCommand::LegacyRouting(true))
        );
        assert_eq!(
            parse_runtime_command("/runtime legacy-routing off"),
            Some(RuntimeCommand::LegacyRouting(false))
        );
        assert_eq!(
            parse_runtime_command("/runtime unknown"),
            Some(RuntimeCommand::Status)
        );
        assert_eq!(parse_runtime_command("runtime"), None);
    }
}
