use crate::runtime;
use crate::workspace::parse_root_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RuntimeCommand {
    Status,
    LegacyRouting(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FeaturesCommand {
    Show,
    Toggle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandParse<T> {
    NotACommand,
    Valid(T),
    Invalid(CommandError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CommandError {
    UnknownRuntimeCommand,
    UnknownFeaturesCommand,
}

impl CommandError {
    pub(super) fn message(self) -> &'static str {
        match self {
            Self::UnknownRuntimeCommand => {
                "unknown runtime command; use /runtime or /runtime legacy-routing <on|off>"
            }
            Self::UnknownFeaturesCommand => {
                "unknown features command; use /features or /features toggle"
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChatCommand<'a> {
    Help,
    ChatMode,
    SwitchToAgent,
    DirectAgentTask(&'a str),
    Status,
    Features(FeaturesCommand),
    Root(Option<&'a str>),
    Runtime(RuntimeCommand),
    Debug(Option<&'a str>),
    Model(Option<&'a str>),
    End,
    Exit,
}

pub(super) fn parse_chat_command(prompt: &str) -> CommandParse<ChatCommand<'_>> {
    if is_exit_command(prompt) {
        return CommandParse::Valid(ChatCommand::Exit);
    }
    if is_end_command(prompt) {
        return CommandParse::Valid(ChatCommand::End);
    }
    if matches!(prompt, "?" | "/help") {
        return CommandParse::Valid(ChatCommand::Help);
    }
    if prompt == "/chat" {
        return CommandParse::Valid(ChatCommand::ChatMode);
    }
    if prompt == "/agent" {
        return CommandParse::Valid(ChatCommand::SwitchToAgent);
    }
    if let Some(task) = parse_agent_task_command(prompt) {
        return CommandParse::Valid(ChatCommand::DirectAgentTask(task));
    }
    if prompt == "/status" {
        return CommandParse::Valid(ChatCommand::Status);
    }
    match parse_features_command(prompt) {
        Ok(Some(command)) => return CommandParse::Valid(ChatCommand::Features(command)),
        Ok(None) => {}
        Err(error) => return CommandParse::Invalid(error),
    }
    if let Some(root) = parse_root_command(prompt) {
        return CommandParse::Valid(ChatCommand::Root(root));
    }
    match parse_runtime_command(prompt) {
        Ok(Some(command)) => return CommandParse::Valid(ChatCommand::Runtime(command)),
        Ok(None) => {}
        Err(error) => return CommandParse::Invalid(error),
    }
    if let Some(mode) = parse_debug_command(prompt) {
        return CommandParse::Valid(ChatCommand::Debug(mode));
    }
    if let Some(model) = parse_model_command(prompt) {
        return CommandParse::Valid(ChatCommand::Model(model));
    }
    CommandParse::NotACommand
}

pub(super) fn parse_features_command(
    prompt: &str,
) -> Result<Option<FeaturesCommand>, CommandError> {
    if prompt == "/features" {
        return Ok(Some(FeaturesCommand::Show));
    }
    let Some(args) = prompt.strip_prefix("/features ") else {
        return Ok(None);
    };
    match args.trim() {
        "toggle" => Ok(Some(FeaturesCommand::Toggle)),
        _ => Err(CommandError::UnknownFeaturesCommand),
    }
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

pub(super) fn parse_runtime_command(prompt: &str) -> Result<Option<RuntimeCommand>, CommandError> {
    if prompt == "/runtime" {
        return Ok(Some(RuntimeCommand::Status));
    }
    let Some(args) = prompt.strip_prefix("/runtime ") else {
        return Ok(None);
    };
    let args = args.trim();
    match args {
        "legacy-routing on" => Ok(Some(RuntimeCommand::LegacyRouting(true))),
        "legacy-routing off" => Ok(Some(RuntimeCommand::LegacyRouting(false))),
        _ => Err(CommandError::UnknownRuntimeCommand),
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
        is_end_command, is_exit_command, parse_chat_command, parse_debug_command,
        parse_features_command, parse_model_command, parse_runtime_command, ChatCommand,
        CommandError, CommandParse, FeaturesCommand, RuntimeCommand,
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
            Ok(Some(RuntimeCommand::Status))
        );
        assert_eq!(
            parse_runtime_command("/runtime legacy-routing on"),
            Ok(Some(RuntimeCommand::LegacyRouting(true)))
        );
        assert_eq!(
            parse_runtime_command("/runtime legacy-routing off"),
            Ok(Some(RuntimeCommand::LegacyRouting(false)))
        );
        assert!(parse_runtime_command("/runtime unknown").is_err());
        assert!(parse_runtime_command("/runtime ").is_err());
        assert!(parse_runtime_command("/runtime legacy-routing").is_err());
        assert_eq!(parse_runtime_command("runtime"), Ok(None));
    }

    #[test]
    fn parses_invalid_runtime_as_chat_command_error() {
        assert_eq!(
            parse_chat_command("/runtime unknown"),
            CommandParse::Invalid(CommandError::UnknownRuntimeCommand)
        );
    }

    #[test]
    fn parses_direct_agent_task_as_valid_chat_command() {
        assert_eq!(
            parse_chat_command("/agent fix the bug"),
            CommandParse::Valid(ChatCommand::DirectAgentTask("fix the bug"))
        );
    }

    #[test]
    fn parses_features_slash_command() {
        assert_eq!(
            parse_chat_command("/features"),
            CommandParse::Valid(ChatCommand::Features(FeaturesCommand::Show))
        );
        assert_eq!(
            parse_chat_command("/features toggle"),
            CommandParse::Valid(ChatCommand::Features(FeaturesCommand::Toggle))
        );
        assert_eq!(
            parse_features_command("/features unknown"),
            Err(CommandError::UnknownFeaturesCommand)
        );
    }
}
