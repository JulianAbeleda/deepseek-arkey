use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::agent;
use crate::input::{DockedComposer, InlineInput, InputAction, RawModeSession};
use crate::intent::{classify_intent, path_boundary_violation, recent_task_context, Intent};
use crate::provider::{self, Message, DEFAULT_SESSION_NAME, PROVIDER};
use crate::runtime::{self, RuntimeBackend};
use crate::session::{self, SessionState};
use crate::ui;
use crate::workspace::{
    effective_workspace_root, infer_natural_root, parse_root_command, path_boundary_clarify_text,
    root_status, update_selected_root,
};

enum TurnEvent {
    Delta(String),
    Complete(Result<(String, String), String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractiveMode {
    Agent,
    Chat,
}

struct PendingAgentTask {
    prompt: String,
    root: PathBuf,
}

pub(crate) fn run_interactive(
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
    let active_session = reset_persisted_chat_messages()?;
    let mut current_model = active_session
        .map(|state| state.model)
        .unwrap_or_else(|| model.to_string());
    if session::load()?.is_none() {
        session::save(&SessionState::new(
            PROVIDER,
            DEFAULT_SESSION_NAME,
            current_model.clone(),
        ))?;
    }
    ui::print_banner(&current_model);
    let mut input = InlineInput::new();
    let mut memory = Vec::<Message>::new();
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
            println!("{}", runtime::runtime_result(&current_model, false)?);
            continue;
        }
        if let Some(mode) = parse_debug_command(prompt) {
            let output = match mode {
                Some(mode) => runtime::debug_result(&current_model, Some(mode), false)?,
                None => runtime::toggle_debug_result(&current_model)?,
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
        match run_prompt_with_memory(
            &mut memory,
            prompt,
            &current_model,
            temperature,
            stream,
            None,
        ) {
            Ok((_, response)) => {
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
    let active_session = reset_persisted_chat_messages()?;
    let mut current_model = active_session
        .as_ref()
        .map(|state| state.model.clone())
        .unwrap_or_else(|| model.to_string());
    if active_session.is_none() {
        session::save(&SessionState::new(
            PROVIDER,
            DEFAULT_SESSION_NAME,
            current_model.clone(),
        ))?;
    }
    let mut approved_agent_root = active_session
        .as_ref()
        .and_then(SessionState::agent_root_path)
        .filter(|root| root.is_dir());
    let raw_mode = RawModeSession::enable()?;
    let transcript_start_row = ui::print_banner(&current_model);
    raw_mode.set_output_scroll_region()?;
    let mut runtime_state = runtime::load(&current_model)?;
    let mut composer = DockedComposer::new(ui::prompt_text(&runtime_state.label(&current_model)));
    composer.set_transcript_start_row(transcript_start_row);
    composer.render()?;
    let mut in_flight: Option<Receiver<TurnEvent>> = None;
    let mut context_scan_started: Option<Instant> = None;
    let mut queued = VecDeque::<String>::new();
    let mut memory = Vec::<Message>::new();
    let mut pending_agent_task: Option<PendingAgentTask> = None;
    let mut confirmed_agent_task: Option<PendingAgentTask> = None;
    let mut selected_root: Option<PathBuf> = approved_agent_root.clone();
    let mut switch_to_agent = false;
    loop {
        let context_scan_ready = context_scan_started
            .map(|started| started.elapsed() >= Duration::from_secs(1))
            .unwrap_or(true);
        if context_scan_ready {
            if let Some(receiver) = &in_flight {
                let (result, streamed) =
                    drain_turn_events(receiver, &mut composer, "response worker disconnected")?;
                if streamed {
                    context_scan_started = None;
                }
                if let Some(result) = result {
                    in_flight = None;
                    context_scan_started = None;
                    match result {
                        Ok((prompt, response)) => {
                            push_interactive_turn(&mut memory, prompt, response);
                            composer.finish_stream()?;
                        }
                        Err(err) => {
                            composer.print_above(&format!("error: {err}\n"))?;
                        }
                    }
                    if let Some(next) = queued.pop_front() {
                        context_scan_started = Some(start_context_scan(&mut composer)?);
                        in_flight = Some(spawn_prompt_turn(
                            &memory,
                            next,
                            current_model.clone(),
                            temperature,
                        ));
                    }
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
            composer.print_above(&ui::interactive_help(&current_model))?;
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
        if is_agent_task_choice(prompt) {
            if let Some(task) = pending_agent_task.take() {
                approve_session_agent_root(&task.root)?;
                composer.print_above(&format!(
                    "agent task accepted\nroot: {}\npending: {}\n",
                    task.root.display(),
                    task.prompt
                ))?;
                confirmed_agent_task = Some(task);
                break;
            }
            composer.print_above(&no_pending_agent_task_text())?;
            continue;
        }
        if prompt == "/status" {
            composer.print_above(&interactive_chat_status(
                &current_model,
                effective_workspace_root(selected_root.as_deref()).as_deref(),
                selected_root.is_some(),
                approved_agent_root.as_deref(),
                memory.len() / 2,
            )?)?;
            continue;
        }
        if let Some(root_arg) = parse_root_command(prompt) {
            let output = match root_arg {
                Some(root_arg) => match update_selected_root(root_arg) {
                    Ok(next_root) => {
                        selected_root = next_root;
                        approved_agent_root = None;
                        clear_session_agent_root()?;
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
            composer.print_above(&runtime::runtime_result(&current_model, false)?)?;
            continue;
        }
        if let Some(mode) = parse_debug_command(prompt) {
            let output = match mode {
                Some(mode) => runtime::debug_result(&current_model, Some(mode), false)?,
                None => runtime::toggle_debug_result(&current_model)?,
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
                None => composer.print_above(&ui::model_help(&current_model))?,
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
                let root = task_root_for_prompt(prompt, selected_root.as_deref());
                let Some(root) = root else {
                    composer.print_above(&clarify_route_text())?;
                    pending_agent_task = None;
                    continue;
                };
                if let Some(path) = path_boundary_violation(prompt, &root) {
                    composer.print_above(&path_boundary_clarify_text(&root, &path))?;
                    pending_agent_task = None;
                    continue;
                }
                if agent_root_matches(approved_agent_root.as_deref(), &root) {
                    confirmed_agent_task = Some(PendingAgentTask {
                        prompt: prompt.to_string(),
                        root: root.clone(),
                    });
                    break;
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
            &memory,
            prompt.to_string(),
            current_model.clone(),
            temperature,
        ));
    }
    drop(raw_mode);
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
    let mut current_model = reset_persisted_chat_messages()?
        .map(|state| state.model)
        .unwrap_or_else(|| model.to_string());
    if session::load()?.is_none() {
        session::save(&SessionState::new(
            PROVIDER,
            DEFAULT_SESSION_NAME,
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
            print!("{}", ui::agent_help(&current_model, &root));
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
            println!("{}", runtime::runtime_result(&current_model, false)?);
            continue;
        }
        if let Some(mode) = parse_debug_command(prompt) {
            let output = match mode {
                Some(mode) => runtime::debug_result(&current_model, Some(mode), false)?,
                None => runtime::toggle_debug_result(&current_model)?,
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
    prior_messages: &[Message],
    prompt: String,
    model: String,
    temperature: Option<f32>,
) -> Receiver<TurnEvent> {
    let (sender, receiver) = mpsc::channel();
    let prior_messages = prior_messages.to_vec();
    thread::spawn(move || {
        let result = run_prompt_streaming(
            &prior_messages,
            &prompt,
            &model,
            temperature,
            sender.clone(),
        );
        let _ = sender.send(TurnEvent::Complete(result));
    });
    receiver
}

fn drain_turn_events(
    receiver: &Receiver<TurnEvent>,
    composer: &mut DockedComposer,
    disconnected_message: &str,
) -> Result<(Option<Result<(String, String), String>>, bool), String> {
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
    let elapsed = started.elapsed().min(Duration::from_secs(1));
    let elapsed_tenths = (elapsed.as_millis() / 100).min(10);
    let width = 12usize;
    let filled = ((elapsed_tenths as usize * width) / 10).clamp(1, width);
    format!(
        "context: scanning [{}{}] {:.1}s",
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

fn no_pending_agent_task_text() -> String {
    "route: unclear\nNo pending agent task.\nType /root <path> to choose a workspace, then repeat the task; or type /agent <task> to run one directly.\n".to_string()
}

fn is_agent_task_choice(prompt: &str) -> bool {
    matches!(prompt, "yes agent" | "agent task" | "agent")
}

fn task_root_for_prompt(prompt: &str, selected_root: Option<&Path>) -> Option<PathBuf> {
    infer_natural_root(prompt)
        .or_else(|| selected_root.map(Path::to_path_buf))
        .or_else(|| effective_workspace_root(None))
}

fn run_prompt_streaming(
    prior_messages: &[Message],
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    sender: Sender<TurnEvent>,
) -> Result<(String, String), String> {
    let runtime_state = runtime::load(model)?;
    let mut messages = prior_messages.to_vec();
    messages.push(provider::user_message(prompt));
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        let response = runtime::debug_response(prompt, model);
        let delay = runtime::debug_stream_delay();
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
    Ok((prompt.to_string(), response))
}

fn run_prompt_with_memory(
    memory: &mut Vec<Message>,
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    stream: bool,
    sender: Option<Sender<TurnEvent>>,
) -> Result<(String, String), String> {
    let runtime_state = runtime::load(model)?;
    let mut messages = memory.clone();
    messages.push(provider::user_message(prompt));
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        let response = runtime::debug_response(prompt, model);
        if stream {
            if let Some(sender) = sender {
                for delta in response.chars() {
                    let _ = sender.send(TurnEvent::Delta(delta.to_string()));
                }
            } else {
                print!("{response}");
            }
        }
        response
    } else if let Some(sender) = sender {
        provider::chat_with_delta(&messages, model, temperature, None, true, |delta| {
            let _ = sender.send(TurnEvent::Delta(delta.to_string()));
        })?
    } else {
        provider::chat(&messages, model, temperature, None, stream)?
    };
    push_interactive_turn(memory, prompt.to_string(), response.clone());
    Ok((prompt.to_string(), response))
}

fn push_interactive_turn(memory: &mut Vec<Message>, prompt: String, response: String) {
    memory.push(provider::user_message(prompt));
    memory.push(provider::assistant_message(response));
    cap_interactive_memory(memory);
}

fn cap_interactive_memory(memory: &mut Vec<Message>) {
    const MAX_TURNS: usize = 20;
    const MAX_CHARS: usize = 40_000;
    let max_messages = MAX_TURNS * 2;
    if memory.len() > max_messages {
        let drop_count = memory.len() - max_messages;
        memory.drain(0..drop_count);
    }
    while total_message_chars(memory) > MAX_CHARS && memory.len() > 2 {
        memory.drain(0..2);
    }
}

fn total_message_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum()
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
    approved_agent_root: Option<&Path>,
    memory_turns: usize,
) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str("mode: chat\n");
    output.push_str(&format!(
        "chat-memory: process\nchat-turns: {memory_turns}\n"
    ));
    output.push_str(&root_status(root, explicit_root));
    match approved_agent_root {
        Some(root) => output.push_str(&format!(
            "agent-session: allowed\nagent-root: {}\n",
            root.display()
        )),
        None => output.push_str("agent-session: confirm-required\n"),
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

fn interactive_agent_status(model: &str, root: &Path) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str(&format!("mode: agent\nroot: {}\n", root.display()));
    Ok(output)
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

fn reset_persisted_chat_messages() -> Result<Option<SessionState>, String> {
    let Some(mut state) = session::load()? else {
        return Ok(None);
    };
    if !state.messages.is_empty() {
        state.clear_messages();
        session::save(&state)?;
    }
    Ok(Some(state))
}

fn update_active_session_model(model: &str) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.model = model.to_string();
    session::save(&state)
}

fn approve_session_agent_root(root: &Path) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.approve_agent_root(root);
    session::save(&state)
}

fn clear_session_agent_root() -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.clear_agent_root();
    session::save(&state)
}

fn agent_root_matches(approved: Option<&Path>, root: &Path) -> bool {
    let Some(approved) = approved else {
        return false;
    };
    paths_equal(approved, root)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        cap_interactive_memory, context_scan_status, is_agent_task_choice, is_end_command,
        is_exit_command, no_pending_agent_task_text, parse_debug_command, parse_model_command,
        task_root_for_prompt,
    };
    use crate::provider;
    use crate::runtime;
    use std::path::{Path, PathBuf};
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
    fn parses_debug_slash_command() {
        assert_eq!(parse_debug_command("/debug"), Some(None));
        assert_eq!(parse_debug_command("/debug off"), Some(Some("off")));
        assert_eq!(parse_debug_command("/debug manual"), Some(Some("manual")));
        assert_eq!(parse_debug_command("debug"), None);
    }

    #[test]
    fn debug_response_points_file_work_to_agent_mode() {
        let response = runtime::debug_response("can you write files?", "deepseek-v4-flash");
        assert!(response.contains("local diagnostic response"));
        assert!(response.contains("agent --root"));
    }

    #[test]
    fn natural_location_prompt_wins_over_selected_root() {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap();
        let selected = Path::new("/tmp/selected-workspace");
        assert_eq!(
            task_root_for_prompt("go through downloads", Some(selected)),
            Some(home.join("Downloads"))
        );
        assert_eq!(
            task_root_for_prompt("fix this repo", Some(selected)),
            Some(selected.to_path_buf())
        );
    }

    #[test]
    fn agent_task_choice_accepts_natural_confirmation_words() {
        assert!(is_agent_task_choice("yes agent"));
        assert!(is_agent_task_choice("agent task"));
        assert!(is_agent_task_choice("agent"));
        assert!(!is_agent_task_choice("/agent"));
        assert!(!is_agent_task_choice("agent task please"));
    }

    #[test]
    fn no_pending_agent_task_text_points_to_root_or_direct_agent() {
        let response = no_pending_agent_task_text();
        assert!(response.contains("No pending agent task"));
        assert!(response.contains("/root <path>"));
        assert!(response.contains("/agent <task>"));
    }

    #[test]
    fn interactive_memory_is_capped_in_process() {
        let mut memory = Vec::new();
        for index in 0..25 {
            memory.push(provider::user_message(format!("u{index}")));
            memory.push(provider::assistant_message(format!("a{index}")));
        }
        cap_interactive_memory(&mut memory);
        assert_eq!(memory.len(), 40);
        assert_eq!(memory[0].content, "u5");
    }

    #[test]
    fn context_scan_status_has_loading_bar_and_timer() {
        let status = context_scan_status(Instant::now());
        assert!(status.starts_with("context: scanning ["));
        assert!(status.ends_with("s"));
        assert!(!status.ends_with('\n'));
    }
}
