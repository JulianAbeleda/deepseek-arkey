mod agent;
mod answer_format;
mod cancel;
mod cli;
mod features;
mod input;
mod intent;
mod internet;
mod provider;
mod repl;
mod runtime;
mod safety;
mod session;
mod terminal_markdown;
mod terminal_width;
mod ui;
mod workspace;

use std::process::ExitCode;

use clap::Parser;

use cli::{Args, Command, SessionCommand};
use provider::{DEFAULT_MODEL, DEFAULT_SESSION_NAME, PROVIDER};
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
            final_only,
        }) => {
            if let Some(command) = parse_agent_transcript_command(&task) {
                print_latest_agent_transcript(root, command)?;
                return Ok(());
            }
            let task = task.join(" ");
            let config = agent::AgentConfig::new(root, max_steps);
            let outcome = if final_only {
                agent::run_agent_final_only(&task, &model, args.temperature, config)?
            } else {
                agent::run_agent(&task, &model, args.temperature, config)?
            };
            if !final_only {
                eprintln!(
                    "agent: steps={} transcript={}",
                    outcome.steps,
                    outcome.transcript_path.display()
                );
            }
            println!("{}", answer_format::terminal_agent_answer(&outcome.answer));
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AgentTranscriptCommand {
    LatestRaw,
    LatestSummary,
}

fn parse_agent_transcript_command(task: &[String]) -> Option<AgentTranscriptCommand> {
    match task {
        [first, second] if first == "transcript" && second == "latest" => {
            Some(AgentTranscriptCommand::LatestRaw)
        }
        [first, second, third]
            if first == "transcript" && second == "latest" && third == "--summary" =>
        {
            Some(AgentTranscriptCommand::LatestSummary)
        }
        _ => None,
    }
}

fn print_latest_agent_transcript(
    root: String,
    command: AgentTranscriptCommand,
) -> Result<(), String> {
    let transcript = match command {
        AgentTranscriptCommand::LatestRaw => agent::read_latest_transcript(root)?,
        AgentTranscriptCommand::LatestSummary => agent::read_latest_transcript_summary(root)?,
    };
    match transcript {
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
            let name = name.unwrap_or_else(|| DEFAULT_SESSION_NAME.to_string());
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
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        let response = runtime::debug_response(prompt, model);
        if stream {
            print!("{response}");
        }
        response
    } else {
        if let Some(context) = internet::web_context_message_for_prompt_lossy(prompt, |warning| {
            eprintln!("warning: {warning}");
        }) {
            messages.push(context);
        }
        messages.push(provider::user_message(prompt));
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
    use super::{parse_agent_transcript_command, AgentTranscriptCommand};

    #[test]
    fn recognizes_agent_transcript_command() {
        assert_eq!(
            parse_agent_transcript_command(&["transcript".to_string(), "latest".to_string()]),
            Some(AgentTranscriptCommand::LatestRaw)
        );
        assert_eq!(
            parse_agent_transcript_command(&[
                "transcript".to_string(),
                "latest".to_string(),
                "--summary".to_string()
            ]),
            Some(AgentTranscriptCommand::LatestSummary)
        );
        assert_eq!(
            parse_agent_transcript_command(&["inspect".to_string()]),
            None
        );
    }
}
