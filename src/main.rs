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
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Intent {
    Chat,
    Task,
    Clarify,
}

struct PendingAgentTask {
    prompt: String,
    root: PathBuf,
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
                let mode = if args.agent_mode {
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
    let mut context_scan_started: Option<Instant> = None;
    let mut queued = VecDeque::<String>::new();
    let mut pending_agent_task: Option<PendingAgentTask> = None;
    let mut confirmed_agent_task: Option<PendingAgentTask> = None;
    let mut selected_root: Option<PathBuf> = None;
    let mut switch_to_agent = false;
    loop {
        if let Some(receiver) = &in_flight {
            let (result, streamed) =
                drain_turn_events(receiver, &mut composer, "response worker disconnected")?;
            if streamed {
                context_scan_started = None;
            }
            if let Some(result) = result {
                in_flight = None;
                context_scan_started = None;
                if let Err(err) = result {
                    composer.print_above(&format!("error: {err}\n"))?;
                } else {
                    composer.finish_stream()?;
                }
                if let Some(next) = queued.pop_front() {
                    context_scan_started = Some(start_context_scan(&mut composer)?);
                    in_flight = Some(spawn_prompt_turn(next, current_model.clone(), temperature));
                }
            }
        }
        if in_flight.is_some() {
            if let Some(started) = context_scan_started {
                composer.status_above(&context_scan_status(started))?;
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
        if prompt == "/chat" {
            pending_agent_task = None;
            composer.print_above("mode: chat\n")?;
            continue;
        }
        if prompt == "/agent" {
            composer.print_above("switching to agent mode\n")?;
            switch_to_agent = true;
            break;
        }
        if prompt == "yes agent" {
            if let Some(task) = pending_agent_task.take() {
                composer.print_above(&format!(
                    "agent task accepted\nroot: {}\npending: {}\n",
                    task.root.display(),
                    task.prompt
                ))?;
                confirmed_agent_task = Some(task);
                break;
            }
            composer.print_above("no pending agent task\n")?;
            continue;
        }
        if prompt == "/status" {
            composer.print_above(&interactive_chat_status(
                &current_model,
                effective_workspace_root(selected_root.as_deref()).as_deref(),
                selected_root.is_some(),
            )?)?;
            continue;
        }
        if let Some(root_arg) = parse_root_command(prompt) {
            let output = match root_arg {
                Some(root_arg) => match update_selected_root(root_arg) {
                    Ok(next_root) => {
                        selected_root = next_root;
                        pending_agent_task = None;
                        root_status(
                            effective_workspace_root(selected_root.as_deref()).as_deref(),
                            selected_root.is_some(),
                        )
                    }
                    Err(err) => format!("root error: {err}\n"),
                },
                None => root_status(
                    effective_workspace_root(selected_root.as_deref()).as_deref(),
                    selected_root.is_some(),
                ),
            };
            composer.print_above(&output)?;
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
        match classify_intent(
            prompt,
            recent_task_context(&queued),
            effective_workspace_root(selected_root.as_deref()).as_deref(),
        ) {
            Intent::Chat => {}
            Intent::Task => {
                let Some(root) = effective_workspace_root(selected_root.as_deref()) else {
                    composer.print_above(&clarify_route_text())?;
                    pending_agent_task = None;
                    continue;
                };
                if let Some(path) = path_boundary_violation(prompt, &root) {
                    composer.print_above(&path_boundary_clarify_text(&root, &path))?;
                    pending_agent_task = None;
                    continue;
                }
                pending_agent_task = Some(PendingAgentTask {
                    prompt: prompt.to_string(),
                    root: root.clone(),
                });
                composer.print_above(&agent_route_confirmation(&root))?;
                continue;
            }
            Intent::Clarify => {
                pending_agent_task = None;
                composer.print_above(&clarify_route_text())?;
                continue;
            }
        }
        context_scan_started = Some(start_context_scan(&mut composer)?);
        in_flight = Some(spawn_prompt_turn(
            prompt.to_string(),
            current_model.clone(),
            temperature,
        ));
    }
    drop(_raw_mode);
    if let Some(task) = confirmed_agent_task {
        run_confirmed_agent_task(&task, &current_model, temperature)?;
        return run_interactive_chat(&current_model, temperature, false);
    }
    if switch_to_agent {
        run_interactive_agent(&current_model, temperature)?;
    }
    Ok(())
}

fn run_confirmed_agent_task(
    task: &PendingAgentTask,
    model: &str,
    temperature: Option<f32>,
) -> Result<(), String> {
    println!("agent task: {}", task.prompt);
    println!("root: {}", task.root.display());
    let outcome = match agent::run_agent(
        &task.prompt,
        model,
        temperature,
        agent::AgentConfig::new(task.root.clone(), 8),
    ) {
        Ok(outcome) => outcome,
        Err(err) => {
            println!("agent task failed: {err}");
            println!("returning to chat");
            return Ok(());
        }
    };
    eprintln!(
        "agent: steps={} transcript={}",
        outcome.steps,
        outcome.transcript_path.display()
    );
    println!("{}", outcome.answer);
    println!("returning to chat");
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
) -> Result<(Option<Result<(), String>>, bool), String> {
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
    Ok((complete, !chunk.is_empty()))
}

fn start_context_scan(composer: &mut DockedComposer) -> Result<Instant, String> {
    let started = Instant::now();
    composer.status_above(&context_scan_status(started))?;
    Ok(started)
}

fn context_scan_status(started: Instant) -> String {
    let elapsed = started.elapsed();
    let elapsed_tenths = elapsed.as_millis() / 100;
    let width = 12usize;
    let filled = (elapsed_tenths as usize % (width + 1)).max(1);
    format!(
        "context: scanning [{}{}] {:.1}s\n",
        "=".repeat(filled),
        " ".repeat(width - filled),
        elapsed.as_secs_f32()
    )
}

fn agent_route_confirmation(root: &Path) -> String {
    format!(
        "route: agent task\nroot: {}\nRun this as an agent task?\nType yes agent to continue, or /chat to keep chatting.\n",
        root.display()
    )
}

fn clarify_route_text() -> String {
    "route: unclear\nDo you want chat analysis or an agent task?\nType /chat to discuss, /root <path> to choose a workspace, or /agent <task> to execute.\n".to_string()
}

fn path_boundary_clarify_text(root: &Path, path: &Path) -> String {
    let suggested_root = path.parent().unwrap_or(root);
    format!(
        "route: unclear\nReferenced path is outside the selected workspace root.\nroot: {}\npath: {}\nSuggested root: {}\nType /root {} to choose that workspace, or /chat to discuss.\n",
        root.display(),
        path.display(),
        suggested_root.display(),
        suggested_root.display()
    )
}

fn workspace_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if home.as_ref().is_some_and(|home| paths_equal(&cwd, home)) {
        return None;
    }
    Some(cwd)
}

fn effective_workspace_root(selected_root: Option<&Path>) -> Option<PathBuf> {
    selected_root.map(Path::to_path_buf).or_else(workspace_root)
}

fn parse_root_command(prompt: &str) -> Option<Option<&str>> {
    if prompt == "/root" {
        return Some(None);
    }
    prompt
        .strip_prefix("/root ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Some)
}

fn update_selected_root(root_arg: &str) -> Result<Option<PathBuf>, String> {
    if matches!(root_arg, "clear" | "reset" | "cwd") {
        return Ok(None);
    }
    let path = PathBuf::from(root_arg);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|err| err.to_string())?
            .join(path)
    };
    let root = path
        .canonicalize()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    Ok(Some(root))
}

fn root_status(root: Option<&Path>, explicit: bool) -> String {
    match root {
        Some(root) => format!(
            "root: {}\nroot-source: {}\n",
            root.display(),
            if explicit { "explicit" } else { "cwd" }
        ),
        None => "root: unset\nroot-source: none\nUse /root <path> before running workspace tasks from $HOME.\n".to_string(),
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn path_boundary_violation(prompt: &str, root: &Path) -> Option<PathBuf> {
    let root = normalize_path(root);
    prompt
        .split_whitespace()
        .filter_map(clean_prompt_token)
        .filter(|token| is_path_like_token(token))
        .filter_map(|token| {
            let path = PathBuf::from(token);
            let resolved = if path.is_absolute() {
                normalize_path(&path)
            } else {
                normalize_path(&root.join(path))
            };
            if resolved.starts_with(&root) {
                None
            } else {
                Some(resolved)
            }
        })
        .next()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn classify_intent(
    prompt: &str,
    has_recent_task_context: bool,
    workspace_root: Option<&Path>,
) -> Intent {
    let normalized = normalize_prompt(prompt);
    if normalized.is_empty() {
        return Intent::Chat;
    }
    if is_clarify_prompt(&normalized) {
        return Intent::Clarify;
    }
    if is_chat_prompt(&normalized) {
        return Intent::Chat;
    }
    if is_task_prompt(&normalized, has_recent_task_context)
        || references_workspace_file(prompt, workspace_root)
    {
        if workspace_root.is_none() {
            return Intent::Clarify;
        }
        return Intent::Task;
    }
    Intent::Chat
}

fn normalize_prompt(prompt: &str) -> String {
    prompt
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_punctuation() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_chat_prompt(prompt: &str) -> bool {
    let chat_prefixes = [
        "what ",
        "why ",
        "how ",
        "should we ",
        "does this make sense",
        "can you explain",
        "help me understand",
        "explain ",
    ];
    chat_prefixes
        .iter()
        .any(|prefix| prompt.starts_with(prefix))
}

fn is_clarify_prompt(prompt: &str) -> bool {
    matches!(
        prompt,
        "can you look at this" | "can you look at this please"
    )
}

fn is_task_prompt(prompt: &str, has_recent_task_context: bool) -> bool {
    let task_verbs = [
        "fix",
        "add",
        "remove",
        "update",
        "implement",
        "run",
        "commit",
        "push",
        "audit",
        "refactor",
        "create",
        "delete",
        "rename",
        "test",
        "build",
    ];
    let first = prompt.split_whitespace().next().unwrap_or("");
    if task_verbs.contains(&first) {
        return true;
    }
    has_recent_task_context
        && [
            "lets do it",
            "let s do it",
            "go ahead",
            "make that change",
            "apply the patch",
            "ship it",
        ]
        .iter()
        .any(|phrase| prompt.starts_with(phrase))
}

fn references_workspace_file(prompt: &str, workspace_root: Option<&Path>) -> bool {
    prompt.split_whitespace().any(|token| {
        let Some(token) = clean_prompt_token(token) else {
            return false;
        };
        is_path_like_token(token) || workspace_root.is_some_and(|root| root.join(token).is_file())
    })
}

fn clean_prompt_token(token: &str) -> Option<&str> {
    let mut token = token.trim_matches(|ch: char| {
        ch == '"'
            || ch == '\''
            || ch == '`'
            || ch == ','
            || ch == ':'
            || ch == ';'
            || ch == '?'
            || ch == '!'
            || ch == '('
            || ch == ')'
            || ch == '['
            || ch == ']'
            || ch == '{'
            || ch == '}'
    });
    if token.ends_with('.') && !token.ends_with("..") {
        token = &token[..token.len() - 1];
    }
    (!token.is_empty()).then_some(token)
}

fn is_path_like_token(token: &str) -> bool {
    token.contains('/')
        || matches!(
            token,
            "README.md" | "Cargo.toml" | "Cargo.lock" | "package.json" | "tsconfig.json"
        )
        || token.ends_with(".rs")
        || token.ends_with(".py")
        || token.ends_with(".md")
        || token.ends_with(".toml")
        || token.ends_with(".json")
}

fn recent_task_context(queued: &VecDeque<String>) -> bool {
    queued.iter().any(|prompt| {
        let normalized = normalize_prompt(prompt);
        is_task_prompt(&normalized, false)
    })
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
        "DeepSeek Chat Commands\nWorkspace\n  /root               Show active workspace root\n  /root <path>        Set workspace root for routed agent tasks\n  /root clear         Return to cwd-based root detection\n\nSession\n  /model              Show or switch DeepSeek model\n  /model <id>         Switch model for this active session\n  /status             Show active session details\n  /runtime            Show provider/debug runtime state\n  /debug [on|off]     Toggle local debug backend\n  /agent              Switch to workspace agent mode\n  /end                End the current session and clear context\n\nGeneral\n  ? or /help          Show this help\n  /exit               Exit without clearing context\n\nShell\n  mode                chat\n  model               {model}\n"
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

fn interactive_chat_status(
    model: &str,
    root: Option<&Path>,
    explicit_root: bool,
) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str("mode: chat\n");
    output.push_str(&root_status(root, explicit_root));
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
        classify_intent, context_scan_status, debug_response, is_agent_transcript_latest,
        is_end_command, is_exit_command, parse_debug_command, parse_model_command,
        parse_root_command, path_boundary_clarify_text, path_boundary_violation, root_status,
        Intent,
    };
    use std::path::Path;
    use std::time::Instant;

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
    fn parses_root_slash_command() {
        assert_eq!(parse_root_command("/root"), Some(None));
        assert_eq!(parse_root_command("/root   .  "), Some(Some(".")));
        assert_eq!(parse_root_command("/root clear"), Some(Some("clear")));
        assert_eq!(parse_root_command("root ."), None);
    }

    #[test]
    fn root_status_reports_source() {
        let root = Path::new("/tmp/workspace");
        assert!(root_status(Some(root), true).contains("root-source: explicit"));
        assert!(root_status(Some(root), false).contains("root-source: cwd"));
        assert!(root_status(None, false).contains("root: unset"));
    }

    #[test]
    fn detects_path_references_outside_root() {
        let root = Path::new("/tmp/workspace");
        assert_eq!(path_boundary_violation("fix README.md", root), None);
        assert_eq!(path_boundary_violation("fix src/main.rs.", root), None);
        assert!(path_boundary_violation("fix ../outside.md", root).is_some());
        assert!(path_boundary_violation("audit /Users/example/.ssh/config", root).is_some());
    }

    #[test]
    fn outside_root_clarify_suggests_parent_root() {
        let text = path_boundary_clarify_text(
            Path::new("/tmp/workspace"),
            Path::new("/Users/example/.ssh/config"),
        );
        assert!(text.contains("Suggested root: /Users/example/.ssh"));
        assert!(text.contains("Type /root /Users/example/.ssh"));
    }

    #[test]
    fn debug_response_points_file_work_to_agent_mode() {
        let response = debug_response("can you write files?", "deepseek-v4-flash");
        assert!(response.contains("local diagnostic response"));
        assert!(response.contains("agent --root"));
    }

    #[test]
    fn context_scan_status_has_loading_bar_and_timer() {
        let status = context_scan_status(Instant::now());
        assert!(status.starts_with("context: scanning ["));
        assert!(status.ends_with("s\n"));
    }

    #[test]
    fn classifies_open_questions_as_chat() {
        let root = Path::new("/tmp/workspace");
        assert_eq!(
            classify_intent("what do you think about this design?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("how do I fix this?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("explain this codebase", false, Some(root)),
            Intent::Chat
        );
    }

    #[test]
    fn classifies_imperatives_as_tasks_inside_workspace() {
        let root = Path::new("/tmp/workspace");
        assert_eq!(
            classify_intent(
                "fix the duplicate helper in both repos and run tests",
                false,
                Some(root)
            ),
            Intent::Task
        );
        assert_eq!(classify_intent("fix it", false, Some(root)), Intent::Task);
        assert_eq!(
            classify_intent("implement a logout button", false, Some(root)),
            Intent::Task
        );
    }

    #[test]
    fn classifies_ambiguous_or_home_tasks_as_clarify() {
        assert_eq!(
            classify_intent(
                "can you look at this?",
                false,
                Some(Path::new("/tmp/workspace"))
            ),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("fix the README in this directory", false, None),
            Intent::Clarify
        );
    }
}
