use super::approval::ApprovalChoice;

pub enum InputAction {
    Submit(String),
    Approval(ApprovalChoice),
    Cancel,
    Exit,
}

pub(super) enum ApprovalModalEvent {
    Consumed(Option<InputAction>),
    PassThrough,
}

pub(crate) const DOCK_RESERVED_ROWS: usize = 7;
pub(crate) const DOCK_VERTICAL_PADDING_ROWS: usize = 2;
pub(crate) const DOCK_HELP_TEXT: &str =
    "Enter send · Alt/Shift+Enter newline · Esc stop · ? help · /exit";
pub(crate) const SLASH_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec::new("/chat", "Switch to plain chat mode"),
    SlashCommandSpec::new("/agent", "Switch to workspace agent mode"),
    SlashCommandSpec::new("/root", "Show or set active workspace root"),
    SlashCommandSpec::new("/model", "Show or switch DeepSeek model"),
    SlashCommandSpec::new("/debug", "Toggle local debug backend"),
    SlashCommandSpec::new("/runtime", "Show provider/debug runtime state"),
    SlashCommandSpec::new("/status", "Show active session details"),
    SlashCommandSpec::new("/end", "End the current session and clear context"),
    SlashCommandSpec::new("/exit", "Exit without clearing context"),
    SlashCommandSpec::new("/quit", "Exit without clearing context"),
    SlashCommandSpec::new("/help", "Show this help"),
    SlashCommandSpec::new("?", "Show this help"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct SlashCommandSpec {
    pub(crate) command: &'static str,
    pub(crate) description: &'static str,
}

impl SlashCommandSpec {
    const fn new(command: &'static str, description: &'static str) -> Self {
        Self {
            command,
            description,
        }
    }
}
