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
use crate::terminal_markdown::render_terminal_markdown;
use crate::ui;
use crate::workspace::{
    effective_workspace_root, infer_natural_root, parse_navigation_request_from,
    parse_root_command, path_boundary_clarify_text, root_status, update_selected_root,
};

enum TurnEvent {
    Delta(String),
    ToolStep(usize, String),
    ApprovalRequest(agent::ApprovalRequest, Sender<agent::ApprovalDecision>),
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

struct PendingDockApproval {
    request: agent::ApprovalRequest,
    reply: Sender<agent::ApprovalDecision>,
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
        if let Some(task) = parse_agent_task_command(prompt) {
            let root = effective_workspace_root(None)
                .ok_or_else(|| "agent task needs a workspace root; run from a project directory or use interactive /root <path>".to_string())?;
            run_confirmed_agent_task(
                &PendingAgentTask {
                    prompt: task.to_string(),
                    root,
                },
                &current_model,
                temperature,
            )?;
            continue;
        }
        if prompt == "/status" {
            ui::print_status(&current_model)?;
            continue;
        }
        if let Some(command) = parse_runtime_command(prompt) {
            println!("{}", execute_runtime_command(&current_model, command)?);
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
    let persisted_selected_root = active_session
        .as_ref()
        .and_then(SessionState::selected_root_path)
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
    let mut pending_approval: Option<PendingDockApproval> = None;
    let mut selected_root: Option<PathBuf> =
        persisted_selected_root.or_else(|| approved_agent_root.clone());
    let mut switch_to_agent = false;
    loop {
        let context_scan_ready = context_scan_started
            .map(|started| started.elapsed() >= Duration::from_secs(1))
            .unwrap_or(true);
        if context_scan_ready && pending_approval.is_none() {
            if let Some(receiver) = &in_flight {
                let (result, streamed, approval) =
                    drain_turn_events(receiver, &mut composer, "response worker disconnected")?;
                if let Some(approval) = approval {
                    pending_approval = Some(approval);
                    context_scan_started = None;
                }
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
                        in_flight = Some(spawn_docked_turn(
                            &memory,
                            next,
                            selected_root.as_deref(),
                            current_model.clone(),
                            temperature,
                            runtime_state.legacy_routing,
                        ));
                    }
                }
            }
        }
        if in_flight.is_some() && pending_approval.is_none() {
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
        if let Some(approval) = pending_approval.take() {
            if is_approval_accept(prompt, &approval.request.approve_phrase) {
                let _ = approval.reply.send(agent::ApprovalDecision::Approve);
                composer.print_above(&format!("approval: approved {}\n", approval.request.tool))?;
                context_scan_started = Some(start_context_scan(&mut composer)?);
            } else if is_approval_denial(prompt) {
                let _ = approval.reply.send(agent::ApprovalDecision::Deny);
                composer.print_above(&format!("approval: denied {}\n", approval.request.tool))?;
                context_scan_started = Some(start_context_scan(&mut composer)?);
            } else if is_exit_command(prompt) {
                let _ = approval.reply.send(agent::ApprovalDecision::Deny);
                composer.print_above(&format!(
                    "approval: denied {}\nexiting\n",
                    approval.request.tool
                ))?;
                break;
            } else {
                composer.print_above(&approval_pending_text(
                    &approval.request.tool,
                    &approval.request.approve_phrase,
                ))?;
                pending_approval = Some(approval);
            }
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
        match parse_navigation_request_from(prompt, selected_root.as_deref()) {
            Ok(Some(root)) => {
                selected_root = Some(root);
                approved_agent_root = None;
                clear_session_agent_root()?;
                save_selected_root(selected_root.as_deref())?;
                pending_agent_task = None;
                composer.print_above(&root_status(selected_root.as_deref(), true))?;
                continue;
            }
            Ok(None) => {}
            Err(err) => {
                composer.print_above(&format!("root error: {err}\n"))?;
                continue;
            }
        }
        if let Some(task) = parse_agent_task_command(prompt) {
            if in_flight.is_some() {
                queued.push_back(prompt.to_string());
                composer.print_above(&format!("queued: {} prompt(s)\n", queued.len()))?;
                continue;
            }
            let Some(root) = task_root_for_prompt(task, selected_root.as_deref()) else {
                composer.print_above(&clarify_route_text())?;
                pending_agent_task = None;
                continue;
            };
            if let Some(path) = path_boundary_violation(task, &root) {
                composer.print_above(&path_boundary_clarify_text(&root, &path))?;
                pending_agent_task = None;
                continue;
            }
            context_scan_started = Some(start_context_scan(&mut composer)?);
            in_flight = Some(spawn_agent_turn(
                task.to_string(),
                root,
                current_model.clone(),
                temperature,
            ));
            continue;
        }
        if is_agent_task_choice(prompt) {
            if let Some(task) = pending_agent_task.take() {
                approve_session_agent_root(&task.root)?;
                composer.print_above(&format!(
                    "agent task accepted\nroot: {}\npending: {}\n",
                    task.root.display(),
                    task.prompt
                ))?;
                context_scan_started = Some(start_context_scan(&mut composer)?);
                in_flight = Some(spawn_agent_turn(
                    task.prompt,
                    task.root,
                    current_model.clone(),
                    temperature,
                ));
                continue;
            }
            composer.print_above(&no_pending_agent_task_text())?;
            continue;
        }
        if is_agent_task_cancel_choice(prompt) {
            if pending_agent_task.take().is_some() {
                composer.print_above("agent task cancelled\nmode: chat\n")?;
            } else {
                composer.print_above(&no_pending_agent_task_text())?;
            }
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
                        save_selected_root(selected_root.as_deref())?;
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
        if let Some(command) = parse_runtime_command(prompt) {
            let output = execute_runtime_command(&current_model, command)?;
            runtime_state = runtime::load(&current_model)?;
            composer.print_above(&output)?;
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
        if let Some(command) = parse_shell_read_command(prompt) {
            match command {
                ShellReadCommand::Pwd => {
                    composer.print_above(&shell_pwd_text(
                        effective_workspace_root(selected_root.as_deref()).as_deref(),
                    ))?;
                }
                ShellReadCommand::Ls(task) => {
                    if in_flight.is_some() {
                        queued.push_back(prompt.to_string());
                        composer.print_above(&format!("queued: {} prompt(s)\n", queued.len()))?;
                        continue;
                    }
                    let Some(root) = effective_workspace_root(selected_root.as_deref()) else {
                        composer.print_above(&clarify_route_text())?;
                        pending_agent_task = None;
                        continue;
                    };
                    if let Some(path) = path_boundary_violation(&task, &root) {
                        composer.print_above(&path_boundary_clarify_text(&root, &path))?;
                        pending_agent_task = None;
                        continue;
                    }
                    pending_agent_task = None;
                    context_scan_started = Some(start_context_scan(&mut composer)?);
                    in_flight = Some(spawn_agent_turn(
                        task,
                        root,
                        current_model.clone(),
                        temperature,
                    ));
                }
            }
            continue;
        }
        if in_flight.is_some() {
            queued.push_back(prompt.to_string());
            composer.print_above(&format!("queued: {} prompt(s)\n", queued.len()))?;
            continue;
        }
        if !runtime_state.legacy_routing {
            if let Some(root) = workspace_agent_root_for_prompt(prompt, selected_root.as_deref()) {
                if let Some(path) = path_boundary_violation(prompt, &root) {
                    composer.print_above(&path_boundary_clarify_text(&root, &path))?;
                    pending_agent_task = None;
                    continue;
                }
            }
            context_scan_started = Some(start_context_scan(&mut composer)?);
            in_flight = Some(spawn_docked_turn(
                &memory,
                prompt.to_string(),
                selected_root.as_deref(),
                current_model.clone(),
                temperature,
                false,
            ));
            continue;
        }

        if let Some(root) = workspace_agent_root_for_prompt(prompt, selected_root.as_deref()) {
            if let Some(path) = path_boundary_violation(prompt, &root) {
                composer.print_above(&path_boundary_clarify_text(&root, &path))?;
                pending_agent_task = None;
                continue;
            }
            pending_agent_task = None;
            context_scan_started = Some(start_context_scan(&mut composer)?);
            in_flight = Some(spawn_agent_turn(
                prompt.to_string(),
                root,
                current_model.clone(),
                temperature,
            ));
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
                    pending_agent_task = None;
                    context_scan_started = Some(start_context_scan(&mut composer)?);
                    in_flight = Some(spawn_agent_turn(
                        prompt.to_string(),
                        root.clone(),
                        current_model.clone(),
                        temperature,
                    ));
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
            &memory,
            prompt.to_string(),
            current_model.clone(),
            temperature,
        ));
    }
    drop(raw_mode);
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
        agent::AgentConfig::new(task.root.clone(), agent::DEFAULT_MAX_STEPS),
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
    println!("{}", terminal_agent_answer(&outcome.answer));
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
        if let Some(command) = parse_runtime_command(prompt) {
            println!("{}", execute_runtime_command(&current_model, command)?);
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
            agent::AgentConfig::new(root.clone(), agent::DEFAULT_MAX_STEPS),
        )?;
        eprintln!(
            "agent: steps={} transcript={}",
            outcome.steps,
            outcome.transcript_path.display()
        );
        println!("{}", terminal_agent_answer(&outcome.answer));
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

fn is_approval_denial(prompt: &str) -> bool {
    matches!(prompt, "n" | "no" | "deny")
}

fn is_approval_accept(prompt: &str, approve_phrase: &str) -> bool {
    prompt == "y" || prompt == approve_phrase
}

fn approval_pending_text(tool: &str, approve_phrase: &str) -> String {
    format!("approval pending: {tool}\nType `y` or `{approve_phrase}` to approve, `n` to deny. `/exit` cancels and exits.\n")
}

fn spawn_docked_turn(
    prior_messages: &[Message],
    prompt: String,
    selected_root: Option<&Path>,
    model: String,
    temperature: Option<f32>,
    legacy_routing: bool,
) -> Receiver<TurnEvent> {
    if let Some(task) = parse_agent_task_command(&prompt) {
        if let Some(root) = task_root_for_prompt(task, selected_root) {
            if path_boundary_violation(task, &root).is_none() {
                return spawn_agent_turn(task.to_string(), root, model, temperature);
            }
        }
    }
    if !legacy_routing {
        if let Some(root) = workspace_agent_root_for_prompt(&prompt, selected_root) {
            if path_boundary_violation(&prompt, &root).is_none() {
                return spawn_agent_turn(prompt, root, model, temperature);
            }
        }
    }
    spawn_prompt_turn(prior_messages, prompt, model, temperature)
}

fn spawn_agent_turn(
    prompt: String,
    root: PathBuf,
    model: String,
    temperature: Option<f32>,
) -> Receiver<TurnEvent> {
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let result = run_agent_streaming(&prompt, root, &model, temperature, sender.clone());
        let _ = sender.send(TurnEvent::Complete(result));
    });
    receiver
}

fn run_agent_streaming(
    prompt: &str,
    root: PathBuf,
    model: &str,
    temperature: Option<f32>,
    sender: Sender<TurnEvent>,
) -> Result<(String, String), String> {
    if runtime::load(model)?.backend == RuntimeBackend::Debug {
        let response = format!(
            "debug/manual agent backend root: {}\nmodel: {model}\nprompt: {prompt}\n",
            root.display()
        );
        let _ = sender.send(TurnEvent::Delta(response.clone()));
        return Ok((prompt.to_string(), response));
    }
    let outcome = agent::run_agent_with_approval_handler(
        prompt,
        model,
        temperature,
        agent::AgentConfig::new(root, agent::DEFAULT_MAX_STEPS),
        agent::ApprovalMode::External,
        |step, tool| {
            let _ = sender.send(TurnEvent::ToolStep(step, tool.to_string()));
        },
        |request| {
            let (reply_sender, reply_receiver) = mpsc::channel();
            let _ = sender.send(TurnEvent::ApprovalRequest(request, reply_sender));
            reply_receiver
                .recv()
                .unwrap_or(agent::ApprovalDecision::Deny)
        },
    )?;
    let response = format_agent_answer(&outcome.answer);
    let _ = sender.send(TurnEvent::Delta(render_terminal_markdown(&response)));
    Ok((prompt.to_string(), response))
}

fn drain_turn_events(
    receiver: &Receiver<TurnEvent>,
    composer: &mut DockedComposer,
    disconnected_message: &str,
) -> Result<
    (
        Option<Result<(String, String), String>>,
        bool,
        Option<PendingDockApproval>,
    ),
    String,
> {
    let mut chunk = String::new();
    let mut complete = None;
    let mut activity = false;
    let mut approval = None;
    loop {
        match receiver.try_recv() {
            Ok(TurnEvent::Delta(delta)) => {
                activity = true;
                chunk.push_str(&delta);
            }
            Ok(TurnEvent::ToolStep(step, tool)) => {
                activity = true;
                if !chunk.is_empty() {
                    composer.stream_above(&chunk)?;
                    chunk.clear();
                }
                composer.print_above(&format!("agent step {step}: {tool}\n"))?;
            }
            Ok(TurnEvent::ApprovalRequest(request, reply)) => {
                activity = true;
                if !chunk.is_empty() {
                    composer.stream_above(&chunk)?;
                    chunk.clear();
                }
                composer.print_above(&request.summary)?;
                approval = Some(PendingDockApproval { request, reply });
                break;
            }
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
    Ok((complete, activity, approval))
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
        "route: agent task\nroot: {}\nRun this as an agent task?\nType y to continue, n to cancel, or /chat to keep chatting.\n",
        root.display()
    )
}

fn clarify_route_text() -> String {
    "route: unclear\nDo you want chat analysis or an agent task?\nType /chat to discuss, /root <path> to choose a workspace, or /agent <task> to execute.\n".to_string()
}

enum ShellReadCommand {
    Pwd,
    Ls(String),
}

fn parse_shell_read_command(prompt: &str) -> Option<ShellReadCommand> {
    let prompt = prompt.trim();
    if prompt == "pwd" {
        return Some(ShellReadCommand::Pwd);
    }
    if prompt == "ls" {
        return Some(ShellReadCommand::Ls("list files".to_string()));
    }
    prompt
        .strip_prefix("ls ")
        .map(str::trim)
        .filter(|path| !path.is_empty())
        .map(|path| ShellReadCommand::Ls(format!("list files in {path}")))
}

fn shell_pwd_text(root: Option<&Path>) -> String {
    match root {
        Some(root) => format!("{}\n", root.display()),
        None => "root: unset\n".to_string(),
    }
}

fn no_pending_agent_task_text() -> String {
    "route: unclear\nNo pending agent task to confirm.\nType /root <path> to choose a workspace, then repeat the task; or type /agent <task> with the leading slash to run one directly.\n".to_string()
}

pub(crate) fn format_agent_answer(answer: &str) -> String {
    let trimmed = answer.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut text = trimmed.replace("\r\n", "\n").replace('\r', "\n");
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) {
        text = json_answer_to_markdown(&value);
    }
    text = insert_markdown_boundaries(&text);
    text = split_horizontal_rule_lines(&text);
    text = split_known_heading_bodies(&text);
    text = collapse_excess_blank_lines(&text);
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

pub(crate) fn terminal_agent_answer(answer: &str) -> String {
    render_terminal_markdown(&format_agent_answer(answer))
}

fn json_answer_to_markdown(value: &serde_json::Value) -> String {
    let mut output = String::new();
    match value {
        serde_json::Value::Object(map) => render_json_object(map, 2, &mut output),
        serde_json::Value::Array(items) => render_json_array(items, 0, &mut output),
        _ => {
            output.push_str(&json_scalar_text(value));
            output.push('\n');
        }
    }
    output
}

fn render_json_object(
    map: &serde_json::Map<String, serde_json::Value>,
    level: usize,
    output: &mut String,
) {
    for (key, value) in map {
        if value.is_null() {
            continue;
        }
        if value_is_scalar(value) {
            output.push_str("- ");
            output.push_str(&humanize_json_key(key));
            output.push_str(": ");
            output.push_str(&json_scalar_text(value));
            output.push('\n');
            continue;
        }
        push_json_heading(output, level, key);
        match value {
            serde_json::Value::Object(child) => {
                render_json_object(child, (level + 1).min(6), output)
            }
            serde_json::Value::Array(items) => render_json_array(items, level + 1, output),
            _ => {}
        }
        output.push('\n');
    }
}

fn render_json_array(items: &[serde_json::Value], level: usize, output: &mut String) {
    for (index, item) in items.iter().filter(|item| !item.is_null()).enumerate() {
        match item {
            serde_json::Value::Object(map) => {
                output.push_str(&format!("{}. Item {}\n", index + 1, index + 1));
                render_json_object(map, (level + 1).clamp(3, 6), output);
            }
            serde_json::Value::Array(items) => render_json_array(items, level + 1, output),
            _ => {
                output.push_str("- ");
                output.push_str(&json_scalar_text(item));
                output.push('\n');
            }
        }
    }
}

fn push_json_heading(output: &mut String, level: usize, key: &str) {
    if !output.is_empty() && !output.ends_with("\n\n") {
        if output.ends_with('\n') {
            output.push('\n');
        } else {
            output.push_str("\n\n");
        }
    }
    output.push_str(&"#".repeat(level.clamp(2, 6)));
    output.push(' ');
    output.push_str(&humanize_json_key(key));
    output.push_str("\n\n");
}

fn value_is_scalar(value: &serde_json::Value) -> bool {
    matches!(
        value,
        serde_json::Value::String(_) | serde_json::Value::Number(_) | serde_json::Value::Bool(_)
    )
}

fn json_scalar_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.replace('\n', " "),
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::Bool(value) => value.to_string(),
        serde_json::Value::Null => String::new(),
        _ => value.to_string(),
    }
}

fn humanize_json_key(key: &str) -> String {
    let mut output = String::new();
    let mut capitalize_next = true;
    for ch in key.chars() {
        if ch == '_' || ch == '-' {
            output.push(' ');
            capitalize_next = true;
        } else if capitalize_next {
            output.push(ch.to_ascii_uppercase());
            capitalize_next = false;
        } else {
            output.push(ch);
        }
    }
    output
}

fn insert_markdown_boundaries(text: &str) -> String {
    let mut output = String::new();
    let mut index = 0usize;
    while index < text.len() {
        let rest = &text[index..];
        if should_break_before(text, index, rest) {
            push_boundary(&mut output);
        }
        let ch = rest.chars().next().unwrap();
        output.push(ch);
        index += ch.len_utf8();
    }
    output
}

fn should_break_before(text: &str, index: usize, rest: &str) -> bool {
    if index == 0 || text[..index].ends_with('\n') {
        return false;
    }
    let heading_marker = !text[..index].ends_with('#')
        && (rest.starts_with("### ") || rest.starts_with("## ") || rest.starts_with("# "));
    heading_marker
        || horizontal_rule_marker(rest)
        || (!text[..index].ends_with('-') && rest.starts_with("- "))
        || (previous_char_is_list_boundary(text, index) && numbered_list_marker(rest))
}

fn horizontal_rule_marker(text: &str) -> bool {
    text.starts_with("--- ### ") || text.starts_with("--- ## ") || text.starts_with("--- # ")
}

fn previous_char_is_digit(text: &str, index: usize) -> bool {
    matches!(text[..index].chars().last(), Some(ch) if ch.is_ascii_digit())
}

fn previous_char_is_list_boundary(text: &str, index: usize) -> bool {
    matches!(text[..index].chars().last(), Some(ch) if ch.is_whitespace())
        && !previous_char_is_digit(text, index)
}

fn numbered_list_marker(text: &str) -> bool {
    let mut chars = text.chars();
    let mut saw_digit = false;
    for ch in chars.by_ref() {
        if ch.is_ascii_digit() {
            saw_digit = true;
            continue;
        }
        if ch != '.' {
            return false;
        }
        return saw_digit && chars.next() == Some(' ');
    }
    false
}

fn split_horizontal_rule_lines(text: &str) -> String {
    let mut output = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("--- ") {
            output.push_str("---\n\n");
            output.push_str(rest);
        } else {
            output.push_str(line);
        }
        output.push('\n');
    }
    output.trim_end().to_string()
}

fn push_boundary(output: &mut String) {
    while output.ends_with(' ') || output.ends_with('\t') {
        output.pop();
    }
    if output.ends_with("\n\n") {
        return;
    }
    if output.ends_with('\n') {
        output.push('\n');
    } else {
        output.push_str("\n\n");
    }
}

fn split_known_heading_bodies(text: &str) -> String {
    text.lines()
        .map(split_known_heading_body)
        .collect::<Vec<_>>()
        .join("\n")
}

fn split_known_heading_body(line: &str) -> String {
    let Some((prefix, content)) = heading_parts(line) else {
        return line.to_string();
    };
    for heading in [
        "Key Design Decisions",
        "Scripts & Tools",
        "Rust Core Packages",
        "Knowledge/Runtime Corpus",
        "Architecture Highlights",
        "Key Technical Points",
        "Notable Design Patterns",
        "Current Entrypoints",
        "Knowledge & Corpus Layer (`mind/`)",
        "Development & Docs",
        "Overall Purpose",
        "Purpose",
        "Key Components",
        "Documentation",
        "Dependencies",
        "Architecture",
        "Structure",
        "Overview",
        "Summary",
        "Skills",
        "Status",
    ] {
        if let Some(rest) = content
            .strip_prefix(heading)
            .and_then(|rest| rest.strip_prefix(' '))
        {
            return format!("{prefix}{heading}\n{rest}");
        }
    }
    if let Some((heading, rest)) = content.split_once(" **") {
        return format!("{prefix}{heading}\n**{rest}");
    }
    line.to_string()
}

fn heading_parts(line: &str) -> Option<(&str, &str)> {
    for prefix in ["### ", "## ", "# "] {
        if let Some(content) = line.strip_prefix(prefix) {
            return Some((prefix, content));
        }
    }
    None
}

fn collapse_excess_blank_lines(text: &str) -> String {
    let mut output = String::new();
    let mut blank_count = 0usize;
    for line in text.lines() {
        if line.trim().is_empty() {
            blank_count += 1;
            if blank_count <= 1 {
                output.push('\n');
            }
            continue;
        }
        blank_count = 0;
        output.push_str(line.trim_end());
        output.push('\n');
    }
    output.trim_end().to_string()
}

fn is_agent_task_choice(prompt: &str) -> bool {
    matches!(prompt, "y" | "yes" | "yes agent" | "agent task" | "agent")
}

fn is_agent_task_cancel_choice(prompt: &str) -> bool {
    matches!(prompt, "n" | "no")
}

fn parse_agent_task_command(prompt: &str) -> Option<&str> {
    prompt
        .strip_prefix("/agent ")
        .map(str::trim)
        .filter(|task| !task.is_empty())
}

fn task_root_for_prompt(prompt: &str, selected_root: Option<&Path>) -> Option<PathBuf> {
    infer_natural_root(prompt)
        .or_else(|| selected_root.map(Path::to_path_buf))
        .or_else(|| effective_workspace_root(None))
}

fn workspace_agent_root_for_prompt(prompt: &str, selected_root: Option<&Path>) -> Option<PathBuf> {
    if is_workspace_chat_followup(&normalize_workspace_prompt(prompt)) {
        return None;
    }
    infer_natural_root(prompt).or_else(|| {
        is_workspace_agent_prompt(prompt)
            .then(|| effective_workspace_root(selected_root))
            .flatten()
    })
}

fn is_workspace_agent_prompt(prompt: &str) -> bool {
    let normalized = normalize_workspace_prompt(prompt);
    if normalized.is_empty() || is_workspace_chat_followup(&normalized) {
        return false;
    }
    if normalized.contains("main branch") {
        return false;
    }
    let first = normalized.split_whitespace().next().unwrap_or("");
    if matches!(
        first,
        "analyze" | "audit" | "inspect" | "list" | "read" | "review" | "scan" | "summarize"
    ) {
        return true;
    }
    [
        "repo structure",
        "repository structure",
        "project structure",
        "codebase structure",
        "main modules",
        "list files",
        "what is this repo trying to do",
        "what is this repository trying to do",
        "what is this project trying to do",
        "tell me what this repo is trying to do",
        "tell me what this project is trying to do",
        "try a shell command",
        "deny shell command",
        "approve shell command",
        "deny patch edit",
        "approve patch edit",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase))
}

fn is_workspace_chat_followup(prompt: &str) -> bool {
    prompt == "hi"
        || prompt == "hello"
        || prompt.starts_with("does that ")
        || prompt.starts_with("does this ")
        || prompt.starts_with("what is a ")
        || prompt.starts_with("what are ")
        || prompt.starts_with("why ")
        || prompt.starts_with("how ")
        || prompt.starts_with("should ")
        || prompt.starts_with("stay in touch")
}

fn normalize_workspace_prompt(prompt: &str) -> String {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeCommand {
    Status,
    LegacyRouting(bool),
}

fn parse_runtime_command(prompt: &str) -> Option<RuntimeCommand> {
    if prompt == "/runtime" {
        return Some(RuntimeCommand::Status);
    }
    let args = prompt.strip_prefix("/runtime ")?.trim();
    match args {
        "legacy-routing on" => Some(RuntimeCommand::LegacyRouting(true)),
        "legacy-routing off" => Some(RuntimeCommand::LegacyRouting(false)),
        _ => Some(RuntimeCommand::Status),
    }
}

fn execute_runtime_command(model: &str, command: RuntimeCommand) -> Result<String, String> {
    match command {
        RuntimeCommand::Status => runtime::runtime_result(model, false),
        RuntimeCommand::LegacyRouting(enabled) => {
            let state = runtime::set_legacy_routing(model, enabled)?;
            Ok(runtime::format_runtime_state(&state, model))
        }
    }
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

fn save_selected_root(root: Option<&Path>) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    match root {
        Some(root) => state.select_root(root),
        None => state.clear_selected_root(),
    }
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
        agent_route_confirmation, approval_pending_text, cap_interactive_memory,
        context_scan_status, format_agent_answer, is_agent_task_cancel_choice,
        is_agent_task_choice, is_approval_accept, is_end_command, is_exit_command,
        is_workspace_agent_prompt, no_pending_agent_task_text, parse_agent_task_command,
        parse_debug_command, parse_model_command, parse_runtime_command, parse_shell_read_command,
        shell_pwd_text, task_root_for_prompt, terminal_agent_answer,
        workspace_agent_root_for_prompt, RuntimeCommand, ShellReadCommand,
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
    fn approval_pending_text_names_exact_phrase_and_exit() {
        let text = approval_pending_text("run_shell", "yes run");
        assert!(text.contains("Type `y` or `yes run`"));
        assert!(text.contains("`n` to deny"));
        assert!(text.contains("`/exit` cancels"));
    }

    #[test]
    fn approval_accepts_short_y_or_exact_phrase() {
        assert!(is_approval_accept("y", "yes run"));
        assert!(is_approval_accept("yes run", "yes run"));
        assert!(!is_approval_accept("yes", "yes run"));
        assert!(!is_approval_accept("yes apply", "yes run"));
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
            Some(RuntimeCommand::Status)
        );
        assert_eq!(
            parse_runtime_command("/runtime legacy-routing on"),
            Some(RuntimeCommand::LegacyRouting(true))
        );
        assert_eq!(
            parse_runtime_command("/runtime legacy-routing off"),
            Some(RuntimeCommand::LegacyRouting(false))
        );
        assert_eq!(
            parse_runtime_command("/runtime unknown"),
            Some(RuntimeCommand::Status)
        );
        assert_eq!(parse_runtime_command("runtime"), None);
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
    fn selected_root_routes_workspace_prompts_to_agent() {
        let selected = Path::new("/tmp/selected-workspace");
        for prompt in [
            "analyze this repo structure",
            "tell me the main modules",
            "list files",
            "scan src",
            "read Cargo.toml",
            "what is this repo trying to do",
            "deny shell command",
            "approve patch edit",
        ] {
            assert_eq!(
                workspace_agent_root_for_prompt(prompt, Some(selected)),
                Some(selected.to_path_buf()),
                "{prompt}"
            );
        }
    }

    #[test]
    fn selected_root_keeps_casual_followups_in_chat() {
        let selected = Path::new("/tmp/selected-workspace");
        for prompt in [
            "hi",
            "what is a repo",
            "does that make sense",
            "does that align with Kimi",
            "switch to main branch",
            "stay in touch",
        ] {
            assert_eq!(
                workspace_agent_root_for_prompt(prompt, Some(selected)),
                None,
                "{prompt}"
            );
            assert!(!is_workspace_agent_prompt(prompt), "{prompt}");
        }
    }

    #[test]
    fn formats_flat_agent_markdown_into_scannable_blocks() {
        let raw = "## Arkey v2 / PKOS v0.2 Repository Analysis ### Overview A Rust-core migration project. --- ### Structure **Rust Core Packages**: - `arkey-core/` - `arkey-rs/` ### Key Design Decisions 1. Incremental migration 2. Reference preservation";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("## Arkey v2 / PKOS v0.2 Repository Analysis\n\n"));
        assert!(formatted.contains("### Overview\nA Rust-core migration project."));
        assert!(formatted.contains("\n---\n"));
        assert!(formatted.contains("\n- `arkey-core/`"));
        assert!(formatted.contains("\n1. Incremental migration"));
        assert!(formatted.ends_with('\n'));
    }

    #[test]
    fn formats_json_agent_answer_into_readable_markdown() {
        let raw = r#"{"repository":{"name":"arkey","version":"v2","workspace_structure":{"crates":["arkey-core","arkey-rs"],"ready":true}}}"#;
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("## Repository\n"));
        assert!(formatted.contains("- Name: arkey"));
        assert!(formatted.contains("- Version: v2"));
        assert!(formatted.contains("### Workspace Structure\n"));
        assert!(formatted.contains("- arkey-core"));
        assert!(formatted.contains("- arkey-rs"));
        assert!(formatted.contains("- Ready: true"));
        assert!(formatted.ends_with('\n'));
    }

    #[test]
    fn formats_json_array_agent_answer_into_readable_markdown() {
        let raw = r#"[{"name":"deepseek","passed":true},{"name":"minimax","passed":true}]"#;
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("1. Item 1"));
        assert!(formatted.contains("- Name: deepseek"));
        assert!(formatted.contains("2. Item 2"));
        assert!(formatted.contains("- Name: minimax"));
    }

    #[test]
    fn leaves_inline_horizontal_rule_text_alone() {
        let raw = "Use --- as a separator inside prose.";
        let formatted = format_agent_answer(raw);
        assert_eq!(formatted, "Use --- as a separator inside prose.\n");
    }

    #[test]
    fn splits_multi_digit_numbered_lists() {
        let raw = "9. item 10. next item";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("9. item\n\n10. next item"));
    }

    #[test]
    fn does_not_split_version_numbers_as_numbered_lists() {
        let raw = "Runtime is v2. It is ready.";
        let formatted = format_agent_answer(raw);
        assert_eq!(formatted, "Runtime is v2. It is ready.\n");
    }

    #[test]
    fn splits_common_agent_heading_bodies() {
        let raw = "### Overall Purpose This is a Rust migration. ### Architecture **Rust Workspace:** details";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("### Overall Purpose\nThis is a Rust migration."));
        assert!(formatted.contains("### Architecture\n**Rust Workspace:** details"));
    }

    #[test]
    fn splits_purpose_heading_body() {
        let raw = "### Purpose A standalone Rust CLI for querying models.";
        let formatted = format_agent_answer(raw);
        assert!(formatted.contains("### Purpose\nA standalone Rust CLI for querying models."));
    }

    #[test]
    fn splits_heading_body_before_bold_text() {
        let raw = "## Arkey v2 / PKOS v0.2 Repository Analysis **Purpose:** Rust migration.";
        let formatted = format_agent_answer(raw);
        assert!(formatted
            .contains("## Arkey v2 / PKOS v0.2 Repository Analysis\n**Purpose:** Rust migration."));
    }

    #[test]
    fn terminal_agent_answer_renders_tokyo_markdown_styles() {
        let raw = "## Result\n\n**text here** and `code`\n\n- item\n\n1. next\n\n---\n\n```text\n**raw**\n```";
        let rendered = terminal_agent_answer(raw);
        assert!(rendered.contains("\x1b[36;1mResult\x1b[0m"));
        assert!(rendered.contains("\x1b[1mtext here\x1b[0m"));
        assert!(rendered.contains("\x1b[38;2;125;207;255mcode\x1b[0m"));
        assert!(rendered.contains("\x1b[38;2;187;154;247m-\x1b[0m item"));
        assert!(rendered.contains("\x1b[38;2;187;154;247m1.\x1b[0m next"));
        assert!(rendered.contains("\x1b[90m----------------------------------------\x1b[0m"));
        assert!(rendered.contains("**raw**"));
        assert_eq!(
            strip_ansi_for_test(&rendered),
            "Result\n\ntext here and code\n\n- item\n\n1. next\n\n----------------------------------------\n\n```text\n**raw**\n```\n"
        );
    }

    #[test]
    fn terminal_agent_answer_handles_nested_inline_styles() {
        let rendered = terminal_agent_answer(
            "## **Important** Result\n\n**use the `run_shell` tool**\n\nSee https://example.com.",
        );

        assert!(!rendered.contains("**Important**"));
        assert!(rendered.contains("\x1b[36;1m\x1b[1mImportant\x1b[0m\x1b[36;1m Result\x1b[0m"));
        assert!(rendered.contains("\x1b[1muse the "));
        assert!(rendered.contains("\x1b[38;2;125;207;255mrun_shell\x1b[0m\x1b[1m tool\x1b[0m"));
        assert!(rendered.contains("\x1b[38;2;125;207;255mhttps://example.com\x1b[0m."));
        assert_eq!(
            strip_ansi_for_test(&rendered),
            "Important Result\n\nuse the run_shell tool\n\nSee https://example.com.\n"
        );
    }

    #[test]
    fn agent_task_choice_accepts_natural_confirmation_words() {
        assert!(is_agent_task_choice("y"));
        assert!(is_agent_task_choice("yes"));
        assert!(is_agent_task_choice("yes agent"));
        assert!(is_agent_task_choice("agent task"));
        assert!(is_agent_task_choice("agent"));
        assert!(!is_agent_task_choice("n"));
        assert!(!is_agent_task_choice("/agent"));
        assert!(!is_agent_task_choice("agent task please"));
    }

    fn strip_ansi_for_test(text: &str) -> String {
        let mut output = String::new();
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
                        break;
                    }
                }
                continue;
            }
            output.push(ch);
        }
        output
    }

    #[test]
    fn agent_task_cancel_choice_accepts_short_negative_words() {
        assert!(is_agent_task_cancel_choice("n"));
        assert!(is_agent_task_cancel_choice("no"));
        assert!(!is_agent_task_cancel_choice("y"));
        assert!(!is_agent_task_cancel_choice("no thanks"));
    }

    #[test]
    fn agent_route_confirmation_points_to_short_choices() {
        let response = agent_route_confirmation(Path::new("/tmp/workspace"));
        assert!(response.contains("Type y to continue"));
        assert!(response.contains("n to cancel"));
        assert!(!response.contains("yes agent"));
    }

    #[test]
    fn parses_direct_agent_task_slash_command() {
        assert_eq!(
            parse_agent_task_command("/agent scan src"),
            Some("scan src")
        );
        assert_eq!(
            parse_agent_task_command("/agent   inspect README.md"),
            Some("inspect README.md")
        );
        assert_eq!(parse_agent_task_command("/agent"), None);
        assert_eq!(parse_agent_task_command("agent task"), None);
    }

    #[test]
    fn parses_shell_like_read_commands() {
        assert!(matches!(
            parse_shell_read_command("pwd"),
            Some(ShellReadCommand::Pwd)
        ));
        assert!(matches!(
            parse_shell_read_command("ls"),
            Some(ShellReadCommand::Ls(task)) if task == "list files"
        ));
        assert!(matches!(
            parse_shell_read_command("ls src"),
            Some(ShellReadCommand::Ls(task)) if task == "list files in src"
        ));
        assert!(parse_shell_read_command("lsdir").is_none());
        assert!(parse_shell_read_command("pwd src").is_none());
    }

    #[test]
    fn shell_pwd_prints_current_root() {
        assert_eq!(
            shell_pwd_text(Some(Path::new("/tmp/workspace"))),
            "/tmp/workspace\n"
        );
        assert_eq!(shell_pwd_text(None), "root: unset\n");
    }

    #[test]
    fn no_pending_agent_task_text_points_to_root_or_direct_agent() {
        let response = no_pending_agent_task_text();
        assert!(response.contains("No pending agent task to confirm"));
        assert!(response.contains("/root <path>"));
        assert!(response.contains("/agent <task>"));
        assert!(response.contains("leading slash"));
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
