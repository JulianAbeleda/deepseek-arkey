use std::io::{self, Write};

use crate::provider::PROVIDER;
use crate::session;

pub fn print_banner(model: &str) {
    print_line(format!(
        "{} {}",
        accent(PROVIDER.to_ascii_lowercase()),
        muted(format!("[{model}]"))
    ));
    print_line(muted(
        "Enter send · ? help · /model · /debug · /runtime · /end · /exit",
    ));
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
    for model in ["deepseek-v4-flash", "deepseek-v4-pro"] {
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
