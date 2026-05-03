mod agent;
mod cli;
mod input;
mod intent;
mod provider;
mod repl;
mod runtime;
mod safety;
mod session;
mod ui;
mod workspace;

use std::process::ExitCode;

use clap::Parser;

use cli::{Args, Command, SessionCommand};
use provider::{Message, DEFAULT_MODEL, PROVIDER};
use runtime::RuntimeBackend;
use session::SessionState;

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
    if args.chat && args.agent_mode {
        return Err("choose only one of --chat or --agent".to_string());
    }
    let model = args.model.unwrap_or_else(|| DEFAULT_MODEL.to_string());
    match args.command {
        Some(Command::Chat {
            prompt,
            no_session,
            model: chat_model,
            temperature,
            stream,
        }) => {
            let chat_model = chat_model.unwrap_or_else(|| model.clone());
            if let Some(prompt) = prompt {
                let response = run_prompt(&prompt, &chat_model, temperature, no_session, stream)?;
                if !stream {
                    println!("{response}");
                }
            } else {
                repl::run_interactive(
                    &chat_model,
                    temperature,
                    stream,
                    repl::InteractiveMode::Chat,
                )?;
            }
        }
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
        Some(Command::Debug { mode, json }) => {
            let output = runtime::debug_result(&model, mode.as_deref(), json)?;
            println!("{output}");
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
                let mode = if args.agent_mode {
                    repl::InteractiveMode::Agent
                } else {
                    repl::InteractiveMode::Chat
                };
                repl::run_interactive(&model, args.temperature, args.stream, mode)?;
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

pub(crate) fn run_prompt(
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    no_session: bool,
    stream: bool,
) -> Result<String, String> {
    let runtime_state = runtime::load(model)?;
    let active_state = if no_session { None } else { session::load()? };
    let mut messages = active_state
        .as_ref()
        .map(|state| state.messages.clone())
        .unwrap_or_default();
    messages.push(Message {
        role: "user".to_string(),
        content: prompt.to_string(),
    });
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        let response = runtime::debug_response(prompt, model);
        if stream {
            print!("{response}");
        }
        response
    } else {
        provider::chat(&messages, model, temperature, None, stream)?
    };
    if let Some(mut state) = active_state {
        state.push_turn(prompt.to_string(), response.clone());
        session::save(&state)?;
    }
    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::is_agent_transcript_latest;

    #[test]
    fn recognizes_agent_transcript_command() {
        assert!(is_agent_transcript_latest(&[
            "transcript".to_string(),
            "latest".to_string()
        ]));
        assert!(!is_agent_transcript_latest(&["inspect".to_string()]));
    }
}
