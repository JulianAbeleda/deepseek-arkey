use std::io::{self, Write};
use std::path::Path;

use crossterm::cursor::{position, MoveToColumn};
use crossterm::execute;
use crossterm::terminal::{size, Clear, ClearType};

use crate::provider::{PROVIDER, SUPPORTED_MODELS};
use crate::session;

pub fn print_banner(model: &str) -> Option<u16> {
    let mut stdout = io::stdout();
    let _ = execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine));
    let _ = write!(
        stdout,
        "{} {}\r\n",
        accent(PROVIDER.to_ascii_lowercase()),
        muted(format!("[{model}]"))
    );
    let _ = execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine));
    let _ = write!(
        stdout,
        "{}\r\n",
        muted("Enter send · ? help · /model · /debug · /runtime · /end · /exit")
    );
    let _ = stdout.flush();
    position()
        .ok()
        .map(|(_, row)| row.min(max_transcript_row()))
        .filter(|row| *row >= 2)
}

pub fn prompt_text(model: &str) -> String {
    format!(
        "{} {} › ",
        accent(PROVIDER.to_ascii_lowercase()),
        backend(format!("[{model}]"))
    )
}

pub fn print_help(model: &str) {
    print_line(heading(format!("{PROVIDER} Commands")));
    print_line(muted("Session"));
    print_line("  /model              Show or switch DeepSeek model");
    print_line("  /model <id>         Switch model for this active session");
    print_line("  /status             Show active session details");
    print_line("  /runtime            Show provider/debug runtime state");
    print_line("  /debug [on|off]     Toggle local debug backend");
    print_line("  /end                End the current session and clear context");
    print_blank_line();
    print_line(muted("General"));
    print_line("  ? or /help          Show this help");
    print_line("  /exit               Exit without clearing context");
    print_blank_line();
    print_line(muted("Shell"));
    print_line(format!("  model               {model}"));
}

pub fn print_model_help(model: &str) {
    print_line(heading("Model commands"));
    print_line(format!("current: {}", accent(model)));
    print_blank_line();
    print_line(muted("Usage"));
    print_line("  /model <id>");
    print_blank_line();
    print_line(muted("Current DeepSeek text models"));
    for model in SUPPORTED_MODELS {
        print_line(format!("  {model}"));
    }
    print_blank_line();
    print_line(muted(
        "Legacy aliases deepseek-chat and deepseek-reasoner retire on 2026-07-24.",
    ));
}

pub fn print_model_set(model: &str) {
    print_line(format!("model set: {}", accent(model)));
}

pub fn interactive_help(model: &str) -> String {
    format!(
        "DeepSeek Chat Commands\nWorkspace\n  /root               Show active workspace root\n  /root <path>        Set workspace root for routed agent tasks\n  /root clear         Return to cwd-based root detection\n\nSession\n  /model              Show or switch DeepSeek model\n  /model <id>         Switch model for this active session\n  /status             Show active session details\n  /runtime            Show provider/debug runtime state\n  /debug [on|off]     Toggle local debug backend\n  /agent              Switch to workspace agent mode\n  /end                End the current session and clear context\n\nGeneral\n  ? or /help          Show this help\n  /exit               Exit without clearing context\n\nShell\n  mode                chat\n  model               {model}\n"
    )
}

pub fn model_help(model: &str) -> String {
    format!(
        "Model commands\ncurrent: {model}\n\nUsage\n  /model <id>\n\nCurrent DeepSeek text models\n  {}\n\nLegacy aliases deepseek-chat and deepseek-reasoner retire on 2026-07-24.\n",
        SUPPORTED_MODELS.join("\n  ")
    )
}

pub fn agent_help(model: &str, root: &Path) -> String {
    format!(
        "DeepSeek Agent Commands\nWorkspace\n  root                {}\n  read tools          list_files, read_file, search_files, inspect_tree\n  shell               requires yes run\n  edits               require yes apply\n\nSession\n  /chat               Switch to plain chat mode\n  /model              Show or switch DeepSeek model\n  /model <id>         Switch model for this active session\n  /status             Show mode, root, model, and session details\n  /runtime            Show provider/debug runtime state\n  /debug [on|off]     Toggle local debug backend\n  /end                End the current session and clear context\n\nGeneral\n  ? or /help          Show this help\n  /exit               Exit without clearing context\n\nShell\n  mode                agent\n  model               {model}\n",
        root.display()
    )
}

pub fn print_status(model: &str) -> Result<(), String> {
    print_line(heading(format!("{PROVIDER} Status")));
    print_line(format!(
        "session-path: {}",
        session::session_path().display()
    ));
    match session::load()? {
        Some(state) => {
            print_line(format!("session: {}", state.name));
            print_line(format!("model: {}", state.model));
            print_line(format!("turns: {}", state.messages.len() / 2));
            print_line(grounding("health: ok"));
        }
        None => {
            print_line("session: none");
            print_line(format!("model: {model}"));
            print_line(muted("health: stateless"));
        }
    }
    Ok(())
}

pub fn print_session_started(name: &str) {
    print_line(format!("session started: {}", accent(name)));
}

pub fn print_login_ok() {
    print_line(grounding(format!("{PROVIDER} login ok")));
}

pub fn print_session_ended() {
    print_line(grounding("session ended"));
}

pub fn print_no_session() {
    print_line(muted("session: none"));
}

pub fn print_error(message: impl AsRef<str>) {
    let mut stderr = io::stderr();
    let _ = writeln!(stderr, "{}", error(message));
    let _ = stderr.flush();
}

fn print_line(line: impl AsRef<str>) {
    let mut stdout = io::stdout();
    let _ = writeln!(stdout, "{}", line.as_ref());
    let _ = stdout.flush();
}

fn print_blank_line() {
    print_line("");
}

fn max_transcript_row() -> u16 {
    size()
        .ok()
        .map(|(_, rows)| rows.saturating_sub(2))
        .unwrap_or(u16::MAX)
}

fn accent(text: impl AsRef<str>) -> String {
    style("36;1", text)
}

fn backend(text: impl AsRef<str>) -> String {
    style("38;2;122;162;247", text)
}

fn heading(text: impl AsRef<str>) -> String {
    style("34;1", text)
}

fn muted(text: impl AsRef<str>) -> String {
    style("90", text)
}

fn grounding(text: impl AsRef<str>) -> String {
    style("32", text)
}

fn error(text: impl AsRef<str>) -> String {
    style("31;1", text)
}

fn style(code: &str, text: impl AsRef<str>) -> String {
    let text = text.as_ref();
    if no_color() {
        text.to_string()
    } else {
        format!("\x1b[{code}m{text}\x1b[0m")
    }
}

fn no_color() -> bool {
    std::env::var_os("NO_COLOR").is_some()
}

#[cfg(test)]
mod tests {
    use super::prompt_text;

    #[test]
    fn prompt_includes_provider_and_model() {
        let prompt = prompt_text("deepseek-v4-flash");
        assert!(prompt.contains("deepseek"));
        assert!(prompt.contains("deepseek-v4-flash"));
    }
}
