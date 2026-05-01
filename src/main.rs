mod agent;
mod cli;
mod input;
mod provider;
mod safety;
mod session;
mod ui;

use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use clap::Parser;

use cli::{Args, Command, SessionCommand};
use input::{DockedComposer, InlineInput, InputAction, RawModeSession};
use provider::{Message, DEFAULT_MODEL, PROVIDER};
use session::SessionState;

enum TurnEvent {
    Delta(String),
    Complete(Result<(), String>),
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            ui::print_error(err);
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), String> {
    let model = args.model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
    match args.command {
        Some(Command::Agent {
            task,
            root,
            max_steps,
        }) => {
            if is_agent_transcript_latest(&task) {
                print_latest_agent_transcript(root)?;
                return Ok(());
            }
            let task = task.join(" ");
            let outcome = agent::run_agent(
                &task,
                &model,
                args.temperature,
                agent::AgentConfig::new(root, max_steps),
            )?;
            eprintln!(
                "agent: steps={} transcript={}",
                outcome.steps,
                outcome.transcript_path.display()
            );
            println!("{}", outcome.answer);
        }
        Some(Command::Login) => {
            provider::login_check(&model)?;
            ui::print_login_ok();
        }
        Some(Command::Session { command }) => handle_session_command(command, &model)?,
        None => {
            if let Some(prompt) = args.prompt {
                let response = run_prompt(
                    &prompt,
                    &model,
                    args.temperature,
                    args.no_session,
                    args.stream,
                )?;
                if !args.stream {
                    println!("{response}");
                }
            } else {
                run_interactive(&model, args.temperature, args.stream)?;
            }
        }
    }
    Ok(())
}

fn is_agent_transcript_latest(task: &[String]) -> bool {
    matches!(task, [first, second] if first == "transcript" && second == "latest")
}

fn print_latest_agent_transcript(root: String) -> Result<(), String> {
    match agent::read_latest_transcript(root)? {
        Some((path, content)) => {
            eprintln!("agent transcript: {}", path.display());
            println!("{content}");
        }
        None => {
            eprintln!("agent transcript: none");
        }
    }
    Ok(())
}

fn handle_session_command(command: SessionCommand, model: &str) -> Result<(), String> {
    match command {
        SessionCommand::Start { name } => {
            let name = name.unwrap_or_else(|| "default".to_string());
            let state = SessionState::new(PROVIDER, name, model.to_string());
            session::save(&state)?;
            ui::print_session_started(&state.name);
        }
        SessionCommand::Status => ui::print_status(model)?,
        SessionCommand::End => {
            if session::delete()? {
                ui::print_session_ended();
            } else {
                ui::print_no_session();
            }
        }
    }
    Ok(())
}

fn run_interactive(model: &str, temperature: Option<f32>, stream: bool) -> Result<(), String> {
    if io::stdin().is_terminal() {
        return run_interactive_docked(model, temperature);
    }
    let mut current_model = session::load()?
        .map(|state| state.model)
        .unwrap_or_else(|| model.to_string());
    if session::load()?.is_none() {
        session::save(&SessionState::new(
            PROVIDER,
            "default",
            current_model.clone(),
        ))?;
    }
    ui::print_banner(&current_model);
    let mut input = InlineInput::new();
    loop {
        let prompt_text = ui::prompt_text(&current_model);
        let line = match input.read_action(&prompt_text)? {
            InputAction::Submit(line) => line,
            InputAction::Exit => break,
        };
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        if is_exit_command(prompt) {
            break;
        }
        if matches!(prompt, "?" | "/help") {
            ui::print_help(&current_model);
            continue;
        }
        if prompt == "/status" {
            ui::print_status(&current_model)?;
            continue;
        }
        if let Some(next_model) = parse_model_command(prompt) {
            match next_model {
                Some(next_model) => {
                    current_model = next_model.to_string();
                    update_active_session_model(&current_model)?;
                    ui::print_model_set(&current_model);
                }
                None => ui::print_model_help(&current_model),
            }
            continue;
        }
        if is_end_command(prompt) {
            let _ = session::delete()?;
            ui::print_session_ended();
            break;
        }
        match run_prompt(prompt, &current_model, temperature, false, stream) {
            Ok(response) => {
                if !stream {
                    println!("{response}");
                }
            }
            Err(err) => ui::print_error(err),
        }
    }
    Ok(())
}

fn run_interactive_docked(model: &str, temperature: Option<f32>) -> Result<(), String> {
    let mut current_model = session::load()?
        .map(|state| state.model)
        .unwrap_or_else(|| model.to_string());
    if session::load()?.is_none() {
        session::save(&SessionState::new(
            PROVIDER,
            "default",
            current_model.clone(),
        ))?;
    }
    ui::print_banner(&current_model);
    let _raw_mode = RawModeSession::enable()?;
    let mut composer = DockedComposer::new(ui::prompt_text(&current_model));
    composer.render()?;
    let mut in_flight: Option<Receiver<TurnEvent>> = None;
    let mut queued = VecDeque::<String>::new();
    loop {
        if let Some(receiver) = &in_flight {
            match receiver.try_recv() {
                Ok(TurnEvent::Delta(delta)) => composer.stream_above(&delta)?,
                Ok(TurnEvent::Complete(result)) => {
                    in_flight = None;
                    if let Err(err) = result {
                        composer.print_above(&format!("error: {err}\n"))?;
                    } else {
                        composer.finish_stream()?;
                    }
                    if let Some(next) = queued.pop_front() {
                        composer.print_above("context: scanning\n")?;
                        in_flight =
                            Some(spawn_prompt_turn(next, current_model.clone(), temperature));
                    }
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    in_flight = None;
                    composer.print_above("error: response worker disconnected\n")?;
                }
            }
        }
        let Some(action) = composer.poll_action(Duration::from_millis(50))? else {
            continue;
        };
        let line = match action {
            InputAction::Submit(line) => line,
            InputAction::Exit => break,
        };
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        if is_exit_command(prompt) {
            break;
        }
        if matches!(prompt, "?" | "/help") {
            composer.print_above(&interactive_help(&current_model))?;
            continue;
        }
        if prompt == "/status" {
            composer.print_above(&interactive_status(&current_model)?)?;
            continue;
        }
        if let Some(next_model) = parse_model_command(prompt) {
            match next_model {
                Some(next_model) => {
                    current_model = next_model.to_string();
                    update_active_session_model(&current_model)?;
                    composer.set_prompt(ui::prompt_text(&current_model))?;
                    composer.print_above(&format!("model set: {current_model}\n"))?;
                }
                None => composer.print_above(&model_help(&current_model))?,
            }
            continue;
        }
        if is_end_command(prompt) {
            let _ = session::delete()?;
            composer.print_above("session ended\n")?;
            break;
        }
        if in_flight.is_some() {
            queued.push_back(prompt.to_string());
            composer.print_above(&format!("queued: {} prompt(s)\n", queued.len()))?;
            continue;
        }
        composer.print_above("context: scanning\n")?;
        in_flight = Some(spawn_prompt_turn(
            prompt.to_string(),
            current_model.clone(),
            temperature,
        ));
    }
    Ok(())
}

fn spawn_prompt_turn(
    prompt: String,
    model: String,
    temperature: Option<f32>,
) -> Receiver<TurnEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_prompt_streaming(&prompt, &model, temperature, sender.clone());
        let _ = sender.send(TurnEvent::Complete(result));
    });
    receiver
}

fn run_prompt_streaming(
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    sender: Sender<TurnEvent>,
) -> Result<(), String> {
    let active_state = session::load()?;
    let mut messages = active_state
        .as_ref()
        .map(|state| state.messages.clone())
        .unwrap_or_default();
    messages.push(Message {
        role: "user".to_string(),
        content: prompt.to_string(),
    });
    let response = provider::chat_with_delta(&messages, model, temperature, None, true, |delta| {
        let _ = sender.send(TurnEvent::Delta(delta.to_string()));
    })?;
    if let Some(mut state) = active_state {
        state.push_turn(prompt.to_string(), response.clone());
        session::save(&state)?;
    }
    Ok(())
}

fn interactive_help(model: &str) -> String {
    format!(
        "DeepSeek Commands\nSession\n  /model              Show or switch DeepSeek model\n  /model <id>         Switch model for this active session\n  /status             Show active session details\n  /end                End the current session and clear context\n\nGeneral\n  ? or /help          Show this help\n  /exit               Exit without clearing context\n\nShell\n  model               {model}\n"
    )
}

fn model_help(model: &str) -> String {
    format!(
        "Model commands\ncurrent: {model}\n\nUsage\n  /model <id>\n\nCurrent DeepSeek text models\n  deepseek-v4-flash\n  deepseek-v4-pro\n\nLegacy aliases deepseek-chat and deepseek-reasoner retire on 2026-07-24.\n"
    )
}

fn interactive_status(model: &str) -> Result<String, String> {
    let mut output = format!(
        "DeepSeek Status\nsession-path: {}\n",
        session::session_path().display()
    );
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

fn run_prompt(
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    no_session: bool,
    stream: bool,
) -> Result<String, String> {
    let active_state = if no_session { None } else { session::load()? };
    let mut messages = active_state
        .as_ref()
        .map(|state| state.messages.clone())
        .unwrap_or_default();
    messages.push(Message {
        role: "user".to_string(),
        content: prompt.to_string(),
    });
    let response = provider::chat(&messages, model, temperature, None, stream)?;
    if let Some(mut state) = active_state {
        state.push_turn(prompt.to_string(), response.clone());
        session::save(&state)?;
    }
    Ok(response)
}

fn parse_model_command(prompt: &str) -> Option<Option<&str>> {
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

fn is_exit_command(prompt: &str) -> bool {
    matches!(
        prompt,
        "exit" | "quit" | "/exit" | "/quit" | "/exit quit" | "/quit exit"
    )
}

fn is_end_command(prompt: &str) -> bool {
    matches!(prompt, "session end" | "/end" | "/end session")
}

fn update_active_session_model(model: &str) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.model = model.to_string();
    session::save(&state)
}

#[cfg(test)]
mod tests {
    use super::{is_agent_transcript_latest, is_end_command, is_exit_command, parse_model_command};

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
    fn recognizes_agent_transcript_command() {
        assert!(is_agent_transcript_latest(&[
            "transcript".to_string(),
            "latest".to_string()
        ]));
        assert!(!is_agent_transcript_latest(&["inspect".to_string()]));
    }
}
