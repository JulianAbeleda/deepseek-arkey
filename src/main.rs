mod agent;
mod cli;
mod input;
mod provider;
mod runtime;
mod safety;
mod session;
mod ui;

use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::path::Path;
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use clap::Parser;

use cli::{Args, Command, SessionCommand};
use input::{DockedComposer, InlineInput, InputAction, RawModeSession};
use provider::{Message, DEFAULT_MODEL, PROVIDER};
use runtime::{RuntimeBackend, RuntimeState};
use session::SessionState;

enum TurnEvent {
    Delta(String),
    Complete(Result<(), String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InteractiveMode {
    Agent,
    Chat,
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
                run_interactive(&chat_model, temperature, stream, InteractiveMode::Chat)?;
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
            let output = debug_result(&model, mode.as_deref(), json)?;
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
                let mode = if args.chat {
                    InteractiveMode::Chat
                } else if args.agent_mode || io::stdin().is_terminal() {
                    InteractiveMode::Agent
                } else {
                    InteractiveMode::Chat
                };
                run_interactive(&model, args.temperature, args.stream, mode)?;
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

fn run_interactive(
    model: &str,
    temperature: Option<f32>,
    stream: bool,
    mode: InteractiveMode,
) -> Result<(), String> {
    if mode == InteractiveMode::Agent {
        return run_interactive_agent(model, temperature);
    }
    run_interactive_chat(model, temperature, stream)
}

fn run_interactive_chat(model: &str, temperature: Option<f32>, stream: bool) -> Result<(), String> {
    if io::stdin().is_terminal() {
        return run_interactive_chat_docked(model, temperature);
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
        let runtime_state = runtime::load(&current_model)?;
        let prompt_text = ui::prompt_text(&runtime_state.label(&current_model));
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
        if prompt == "/agent" {
            run_interactive_agent(&current_model, temperature)?;
            break;
        }
        if prompt == "/status" {
            ui::print_status(&current_model)?;
            continue;
        }
        if prompt == "/runtime" {
            println!("{}", runtime_result(&current_model, false)?);
            continue;
        }
        if let Some(mode) = parse_debug_command(prompt) {
            let output = match mode {
                Some(mode) => debug_result(&current_model, Some(mode), false)?,
                None => toggle_debug_result(&current_model)?,
            };
            println!("{output}");
            continue;
        }
        if let Some(next_model) = parse_model_command(prompt) {
            match next_model {
                Some(next_model) => {
                    current_model = next_model.to_string();
                    update_active_session_model(&current_model)?;
                    let runtime_state = runtime::load(&current_model)?.with_model(&current_model);
                    runtime::save(&runtime_state)?;
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

fn run_interactive_chat_docked(model: &str, temperature: Option<f32>) -> Result<(), String> {
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
    let mut runtime_state = runtime::load(&current_model)?;
    let mut composer = DockedComposer::new(ui::prompt_text(&runtime_state.label(&current_model)));
    composer.render()?;
    let mut in_flight: Option<Receiver<TurnEvent>> = None;
    let mut queued = VecDeque::<String>::new();
    let mut switch_to_agent = false;
    loop {
        if let Some(receiver) = &in_flight {
            if let Some(result) =
                drain_turn_events(receiver, &mut composer, "response worker disconnected")?
            {
                in_flight = None;
                if let Err(err) = result {
                    composer.print_above(&format!("error: {err}\n"))?;
                } else {
                    composer.finish_stream()?;
                }
                if let Some(next) = queued.pop_front() {
                    composer.status_above("context: scanning\n")?;
                    in_flight = Some(spawn_prompt_turn(next, current_model.clone(), temperature));
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
        if prompt == "/agent" {
            composer.print_above("switching to agent mode\n")?;
            switch_to_agent = true;
            break;
        }
        if prompt == "/status" {
            composer.print_above(&interactive_status(&current_model)?)?;
            continue;
        }
        if prompt == "/runtime" {
            composer.print_above(&runtime_result(&current_model, false)?)?;
            continue;
        }
        if let Some(mode) = parse_debug_command(prompt) {
            let output = match mode {
                Some(mode) => debug_result(&current_model, Some(mode), false)?,
                None => toggle_debug_result(&current_model)?,
            };
            runtime_state = runtime::load(&current_model)?;
            composer.set_prompt(ui::prompt_text(&runtime_state.label(&current_model)))?;
            composer.print_above(&output)?;
            continue;
        }
        if let Some(next_model) = parse_model_command(prompt) {
            match next_model {
                Some(next_model) => {
                    current_model = next_model.to_string();
                    update_active_session_model(&current_model)?;
                    runtime_state = runtime_state.with_model(current_model.clone());
                    runtime::save(&runtime_state)?;
                    composer.set_prompt(ui::prompt_text(&runtime_state.label(&current_model)))?;
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
        composer.status_above("context: scanning\n")?;
        in_flight = Some(spawn_prompt_turn(
            prompt.to_string(),
            current_model.clone(),
            temperature,
        ));
    }
    drop(_raw_mode);
    if switch_to_agent {
        run_interactive_agent(&current_model, temperature)?;
    }
    Ok(())
}

fn run_interactive_agent(model: &str, temperature: Option<f32>) -> Result<(), String> {
    let root = std::env::current_dir().map_err(|err| err.to_string())?;
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
    print_agent_banner(&current_model, &root);
    let mut input = InlineInput::new();
    loop {
        let runtime_state = runtime::load(&current_model)?;
        let prompt_text = agent_prompt_text(&runtime_state.label(&current_model));
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
            print!("{}", agent_help(&current_model, &root));
            continue;
        }
        if prompt == "/chat" {
            run_interactive_chat(&current_model, temperature, false)?;
            break;
        }
        if prompt == "/status" {
            print!("{}", interactive_agent_status(&current_model, &root)?);
            continue;
        }
        if prompt == "/runtime" {
            println!("{}", runtime_result(&current_model, false)?);
            continue;
        }
        if let Some(mode) = parse_debug_command(prompt) {
            let output = match mode {
                Some(mode) => debug_result(&current_model, Some(mode), false)?,
                None => toggle_debug_result(&current_model)?,
            };
            println!("{output}");
            continue;
        }
        if let Some(next_model) = parse_model_command(prompt) {
            match next_model {
                Some(next_model) => {
                    current_model = next_model.to_string();
                    update_active_session_model(&current_model)?;
                    let runtime_state = runtime::load(&current_model)?.with_model(&current_model);
                    runtime::save(&runtime_state)?;
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
        let outcome = agent::run_agent(
            prompt,
            &current_model,
            temperature,
            agent::AgentConfig::new(root.clone(), 8),
        )?;
        eprintln!(
            "agent: steps={} transcript={}",
            outcome.steps,
            outcome.transcript_path.display()
        );
        println!("{}", outcome.answer);
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

fn drain_turn_events(
    receiver: &Receiver<TurnEvent>,
    composer: &mut DockedComposer,
    disconnected_message: &str,
) -> Result<Option<Result<(), String>>, String> {
    let mut chunk = String::new();
    let mut complete = None;
    loop {
        match receiver.try_recv() {
            Ok(TurnEvent::Delta(delta)) => chunk.push_str(&delta),
            Ok(TurnEvent::Complete(result)) => {
                complete = Some(result);
                break;
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                complete = Some(Err(disconnected_message.to_string()));
                break;
            }
        }
    }
    if !chunk.is_empty() {
        composer.stream_above(&chunk)?;
    }
    Ok(complete)
}

fn run_prompt_streaming(
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    sender: Sender<TurnEvent>,
) -> Result<(), String> {
    let runtime_state = runtime::load(model)?;
    let active_state = session::load()?;
    let mut messages = active_state
        .as_ref()
        .map(|state| state.messages.clone())
        .unwrap_or_default();
    messages.push(Message {
        role: "user".to_string(),
        content: prompt.to_string(),
    });
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        let response = debug_response(prompt, model);
        let delay = debug_stream_delay();
        if let Some(delay) = delay {
            thread::sleep(delay);
        }
        for delta in response.chars() {
            let _ = sender.send(TurnEvent::Delta(delta.to_string()));
            if let Some(delay) = delay {
                thread::sleep(delay);
            }
        }
        response
    } else {
        provider::chat_with_delta(&messages, model, temperature, None, true, |delta| {
            let _ = sender.send(TurnEvent::Delta(delta.to_string()));
        })?
    };
    if let Some(mut state) = active_state {
        state.push_turn(prompt.to_string(), response.clone());
        session::save(&state)?;
    }
    Ok(())
}

fn interactive_help(model: &str) -> String {
    format!(
        "DeepSeek Chat Commands\nSession\n  /model              Show or switch DeepSeek model\n  /model <id>         Switch model for this active session\n  /status             Show active session details\n  /runtime            Show provider/debug runtime state\n  /debug [on|off]     Toggle local debug backend\n  /agent              Switch to workspace agent mode\n  /end                End the current session and clear context\n\nGeneral\n  ? or /help          Show this help\n  /exit               Exit without clearing context\n\nShell\n  mode                chat\n  model               {model}\n"
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

fn print_agent_banner(model: &str, root: &Path) {
    println!("deepseek [{model}] agent");
    println!("workspace: {}", root.display());
    println!("read tools on - edits require yes apply - shell requires yes run");
    println!("Enter send - ? help - /chat - /model - /status - /end - /exit");
}

fn agent_prompt_text(model: &str) -> String {
    format!("deepseek [{model}] agent › ")
}

fn agent_help(model: &str, root: &Path) -> String {
    format!(
        "DeepSeek Agent Commands\nWorkspace\n  root                {}\n  read tools          list_files, read_file, search_files, inspect_tree\n  shell               requires yes run\n  edits               require yes apply\n\nSession\n  /chat               Switch to plain chat mode\n  /model              Show or switch DeepSeek model\n  /model <id>         Switch model for this active session\n  /status             Show mode, root, model, and session details\n  /runtime            Show provider/debug runtime state\n  /debug [on|off]     Toggle local debug backend\n  /end                End the current session and clear context\n\nGeneral\n  ? or /help          Show this help\n  /exit               Exit without clearing context\n\nShell\n  mode                agent\n  model               {model}\n",
        root.display()
    )
}

fn interactive_agent_status(model: &str, root: &Path) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str(&format!("mode: agent\nroot: {}\n", root.display()));
    Ok(output)
}

fn run_prompt(
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
        let response = debug_response(prompt, model);
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

fn debug_result(model: &str, mode: Option<&str>, json: bool) -> Result<String, String> {
    let state = match mode {
        Some(mode) => {
            let backend = RuntimeBackend::parse(mode).ok_or_else(|| {
                "debug mode must be one of: on, off, debug, manual, provider".to_string()
            })?;
            runtime::set_backend(model, backend)?
        }
        None => runtime::load(model)?,
    };
    if json {
        return serde_json::to_string_pretty(&state).map_err(|err| err.to_string());
    }
    Ok(format_runtime_state(&state, model))
}

fn runtime_result(model: &str, json: bool) -> Result<String, String> {
    debug_result(model, None, json)
}

fn toggle_debug_result(model: &str) -> Result<String, String> {
    let current = runtime::load(model)?;
    let next = match current.backend {
        RuntimeBackend::Provider => RuntimeBackend::Debug,
        RuntimeBackend::Debug => RuntimeBackend::Provider,
    };
    let state = runtime::set_backend(model, next)?;
    Ok(format_runtime_state(&state, model))
}

fn format_runtime_state(state: &RuntimeState, fallback_model: &str) -> String {
    let backend = match state.backend {
        RuntimeBackend::Provider => "provider",
        RuntimeBackend::Debug => "debug",
    };
    format!(
        "LLM: {backend}\nRuntime: {}\nModel: {}\nUpdated: {}\n",
        state.runtime,
        state.model.as_deref().unwrap_or(fallback_model),
        state.updated_at
    )
}

fn debug_response(prompt: &str, model: &str) -> String {
    format!(
        "debug/manual backend\nprovider: {PROVIDER}\nmodel: {model}\nprompt: {prompt}\n\nThis is a local diagnostic response. Normal chat does not get filesystem tools; use `agent --root <path> ...` for file read/write work."
    )
}

fn debug_stream_delay() -> Option<Duration> {
    std::env::var("DEEPSEEK_DEBUG_STREAM_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|delay| *delay > 0)
        .map(Duration::from_millis)
}

fn parse_debug_command(prompt: &str) -> Option<Option<&str>> {
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
    use super::{
        debug_response, is_agent_transcript_latest, is_end_command, is_exit_command,
        parse_debug_command, parse_model_command,
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
    fn recognizes_agent_transcript_command() {
        assert!(is_agent_transcript_latest(&[
            "transcript".to_string(),
            "latest".to_string()
        ]));
        assert!(!is_agent_transcript_latest(&["inspect".to_string()]));
    }

    #[test]
    fn parses_debug_slash_command() {
        assert_eq!(parse_debug_command("/debug"), Some(None));
        assert_eq!(parse_debug_command("/debug off"), Some(Some("off")));
        assert_eq!(parse_debug_command("/debug manual"), Some(Some("manual")));
        assert_eq!(parse_debug_command("debug"), None);
    }

    #[test]
    fn debug_response_points_file_work_to_agent_mode() {
        let response = debug_response("can you write files?", "deepseek-v4-flash");
        assert!(response.contains("local diagnostic response"));
        assert!(response.contains("agent --root"));
    }
}
