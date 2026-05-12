use std::collections::{HashSet, VecDeque};
use std::io::{self, IsTerminal};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::agent;
use crate::agent::commit_audit::is_commit_audit_prompt;
use crate::answer_format::{format_agent_answer, terminal_agent_answer};
use crate::cancel::CancellationToken;
use crate::input::{ApprovalChoice, DockedComposer, InlineInput, InputAction, RawModeSession};
use crate::intent::{classify_intent, path_boundary_violation, recent_task_context, Intent};
use crate::provider::{self, Message, DEFAULT_SESSION_NAME, PROVIDER};
use crate::runtime::{self, RuntimeBackend};
use crate::session::{self, SessionState};
use crate::terminal_markdown::render_terminal_markdown;
use crate::ui;
use crate::workspace::{
    effective_workspace_root, infer_natural_root, parse_navigation_request_from,
    parse_root_command, path_boundary_clarify_text, root_status, update_selected_root,
    update_selected_root_from,
};

mod commands;

use commands::{
    execute_runtime_command, is_end_command, is_exit_command, parse_agent_task_command,
    parse_debug_command, parse_model_command, parse_runtime_command,
};

enum TurnEvent {
    Delta(String),
    RenderedMarkdown(String),
    ToolStep(agent::AgentStep),
    ApprovalRequest(agent::ApprovalRequest, Sender<agent::ApprovalDecision>),
    Complete(Result<(String, String), String>),
}

const DEFAULT_RENDERED_MARKDOWN_STREAM_DELAY: Duration = Duration::from_millis(12);
const DEFAULT_RENDERED_MARKDOWN_STREAM_MAX_DELAY: Duration = Duration::from_millis(1200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InteractiveMode {
    Agent,
    Chat,
}

struct PendingAgentTask {
    prompt: String,
    root: PathBuf,
}

struct InFlightTurn {
    receiver: Receiver<TurnEvent>,
    cancel: CancellationToken,
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
            InputAction::Approval(_) => continue,
            InputAction::Cancel => continue,
            InputAction::Exit => break,
        };
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        if let Some(command) = commands::parse_chat_command(prompt) {
            match command {
                commands::ChatCommand::Exit => break,
                commands::ChatCommand::Help => {
                    ui::print_help(&current_model);
                    continue;
                }
                commands::ChatCommand::SwitchToAgent => {
                    run_interactive_agent(&current_model, temperature)?;
                    break;
                }
                commands::ChatCommand::DirectAgentTask(task) => {
                    let root = effective_workspace_root(None).ok_or_else(|| {
                        "agent task needs a workspace root; run from a project directory or use interactive /root <path>".to_string()
                    })?;
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
                _ => {}
            }
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
                    print!("{}", render_terminal_markdown(&response));
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
    let mut runtime_state = runtime::load(&current_model)?;
    let mut composer = DockedComposer::new(ui::prompt_text(&runtime_state.label(&current_model)));
    composer.set_transcript_start_row(transcript_start_row);
    composer.render()?;
    let mut in_flight: Option<InFlightTurn> = None;
    let mut context_scan_started: Option<Instant> = None;
    let mut queued = VecDeque::<String>::new();
    let mut memory = Vec::<Message>::new();
    let mut pending_agent_task: Option<PendingAgentTask> = None;
    let mut pending_approval: Option<PendingDockApproval> = None;
    let mut session_approved_tools = HashSet::<String>::new();
    let mut active_tool_steps = Vec::<agent::AgentStep>::new();
    let mut selected_root: Option<PathBuf> =
        persisted_selected_root.or_else(|| approved_agent_root.clone());
    let mut previous_selected_root: Option<PathBuf> = None;
    let mut switch_to_agent = false;
    loop {
        if pending_approval.is_none() {
            if let Some(turn) = &in_flight {
                let (result, streamed, approval) = drain_turn_events(
                    &turn.receiver,
                    &mut composer,
                    "response worker disconnected",
                    context_scan_started,
                    &mut active_tool_steps,
                )?;
                if let Some(approval) = approval {
                    context_scan_started = None;
                    if session_approved_tools.contains(&approval.request.tool) {
                        composer.status_above(&format!(
                            "approval: auto-approved {} for session",
                            approval.request.tool
                        ))?;
                        let _ = approval.reply.send(agent::ApprovalDecision::Approve);
                        context_scan_started =
                            Some(start_context_scan(&mut composer, &active_tool_steps)?);
                    } else {
                        composer.show_approval_modal(
                            approval.request.tool.clone(),
                            approval.request.summary.clone(),
                        )?;
                        pending_approval = Some(approval);
                    }
                }
                if streamed {
                    context_scan_started = None;
                }
                if let Some(result) = result {
                    in_flight = None;
                    context_scan_started = None;
                    active_tool_steps.clear();
                    match result {
                        Ok((prompt, response)) => {
                            push_interactive_turn(&mut memory, prompt, response);
                            composer.finish_stream()?;
                        }
                        Err(err) => {
                            composer.show_cursor()?;
                            composer.clear_progress_dock()?;
                            composer.print_above(&format!("error: {err}\n"))?;
                        }
                    }
                    if let Some(next) = queued.pop_front() {
                        active_tool_steps.clear();
                        context_scan_started =
                            Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
                composer.progress_dock(&context_scan_status(started, &active_tool_steps))?;
            }
        }
        let Some(action) = composer.poll_action(Duration::from_millis(50))? else {
            continue;
        };
        let line = match action {
            InputAction::Submit(line) => line,
            InputAction::Approval(choice) => {
                let Some(approval) = pending_approval.take() else {
                    composer.clear_approval_modal()?;
                    continue;
                };
                handle_dock_approval_choice(
                    &mut composer,
                    approval,
                    choice,
                    &mut session_approved_tools,
                )?;
                context_scan_started = Some(start_context_scan(&mut composer, &active_tool_steps)?);
                continue;
            }
            InputAction::Cancel => {
                if let Some(turn) = in_flight.take() {
                    turn.cancel.cancel();
                    context_scan_started = None;
                    active_tool_steps.clear();
                    queued.clear();
                    composer.clear_progress_dock()?;
                    composer.finish_stream()?;
                    composer.print_above("cancelled current response\n")?;
                }
                continue;
            }
            InputAction::Exit => break,
        };
        let prompt = line.trim();
        if prompt.is_empty() {
            continue;
        }
        if pending_approval.is_some() {
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
        if is_cd_previous_request(prompt) {
            let Some(previous_root) = previous_selected_root.take() else {
                composer.print_above("root error: no previous root\n")?;
                continue;
            };
            previous_selected_root = selected_root.clone();
            selected_root = Some(previous_root);
            approved_agent_root = None;
            clear_session_agent_root()?;
            save_selected_root(selected_root.as_deref())?;
            pending_agent_task = None;
            composer.print_above(&root_status(selected_root.as_deref(), true))?;
            continue;
        }
        match parse_navigation_request_from(prompt, selected_root.as_deref()) {
            Ok(Some(root)) => {
                if selected_root.as_deref() != Some(root.as_path()) {
                    previous_selected_root = selected_root.clone();
                }
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
            active_tool_steps.clear();
            context_scan_started = Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
                active_tool_steps.clear();
                context_scan_started = Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
                Some(root_arg) => {
                    match update_selected_root_from(root_arg, selected_root.as_deref())
                        .or_else(|_| update_selected_root(root_arg))
                    {
                        Ok(next_root) => {
                            if selected_root != next_root {
                                previous_selected_root = selected_root.clone();
                            }
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
                    }
                }
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
                    active_tool_steps.clear();
                    context_scan_started =
                        Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
            active_tool_steps.clear();
            context_scan_started = Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
            active_tool_steps.clear();
            context_scan_started = Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
                    active_tool_steps.clear();
                    context_scan_started =
                        Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
        active_tool_steps.clear();
        context_scan_started = Some(start_context_scan(&mut composer, &active_tool_steps)?);
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
            InputAction::Approval(_) => continue,
            InputAction::Cancel => continue,
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
) -> InFlightTurn {
    let (sender, receiver) = mpsc::channel();
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    let prior_messages = prior_messages.to_vec();
    thread::spawn(move || {
        let result = run_prompt_buffered_rendered(
            &prior_messages,
            &prompt,
            &model,
            temperature,
            sender.clone(),
            worker_cancel,
        );
        let _ = sender.send(TurnEvent::Complete(result));
    });
    InFlightTurn { receiver, cancel }
}

fn handle_dock_approval_choice(
    composer: &mut DockedComposer,
    approval: PendingDockApproval,
    choice: ApprovalChoice,
    session_approved_tools: &mut HashSet<String>,
) -> Result<(), String> {
    composer.clear_approval_modal()?;
    let decision =
        approval_decision_for_choice(&approval.request.tool, choice, session_approved_tools);
    match choice {
        ApprovalChoice::ApproveOnce => {
            let _ = approval.reply.send(decision);
            composer.print_above(&format!("approval: approved {}\n", approval.request.tool))?;
        }
        ApprovalChoice::ApproveForSession => {
            let _ = approval.reply.send(decision);
            composer.print_above(&format!(
                "approval: approved {} for session\n",
                approval.request.tool
            ))?;
        }
        ApprovalChoice::Reject => {
            let _ = approval.reply.send(decision);
            composer.print_above(&format!("approval: denied {}\n", approval.request.tool))?;
        }
    }
    Ok(())
}

fn approval_decision_for_choice(
    tool: &str,
    choice: ApprovalChoice,
    session_approved_tools: &mut HashSet<String>,
) -> agent::ApprovalDecision {
    match choice {
        ApprovalChoice::ApproveOnce => agent::ApprovalDecision::Approve,
        ApprovalChoice::ApproveForSession => {
            session_approved_tools.insert(tool.to_string());
            agent::ApprovalDecision::ApproveForSession
        }
        ApprovalChoice::Reject => agent::ApprovalDecision::Deny,
    }
}

fn is_cd_previous_request(prompt: &str) -> bool {
    matches!(
        prompt.trim().to_ascii_lowercase().as_str(),
        "cd -" | "cd previous" | "cd back"
    )
}

fn spawn_docked_turn(
    prior_messages: &[Message],
    prompt: String,
    selected_root: Option<&Path>,
    model: String,
    temperature: Option<f32>,
    legacy_routing: bool,
) -> InFlightTurn {
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
) -> InFlightTurn {
    let (sender, receiver) = mpsc::channel();
    let cancel = CancellationToken::new();
    let worker_cancel = cancel.clone();
    thread::spawn(move || {
        let result = run_agent_streaming(
            &prompt,
            root,
            &model,
            temperature,
            sender.clone(),
            worker_cancel,
        );
        let _ = sender.send(TurnEvent::Complete(result));
    });
    InFlightTurn { receiver, cancel }
}

fn run_agent_streaming(
    prompt: &str,
    root: PathBuf,
    model: &str,
    temperature: Option<f32>,
    sender: Sender<TurnEvent>,
    cancel: CancellationToken,
) -> Result<(String, String), String> {
    cancel.check()?;
    if runtime::load(model)?.backend == RuntimeBackend::Debug {
        let response = format!(
            "debug/manual agent backend root: {}\nmodel: {model}\nprompt: {prompt}\n",
            root.display()
        );
        let _ = sender.send(TurnEvent::Delta(response.clone()));
        return Ok((prompt.to_string(), response));
    }
    let outcome = agent::run_agent_quiet_cache_with_approval_handler_cancelled(
        prompt,
        model,
        temperature,
        agent::AgentConfig::new(root, agent::DEFAULT_MAX_STEPS),
        agent::ApprovalMode::External,
        |step| {
            let _ = sender.send(TurnEvent::ToolStep(step));
        },
        |request| {
            let (reply_sender, reply_receiver) = mpsc::channel();
            let _ = sender.send(TurnEvent::ApprovalRequest(request, reply_sender));
            reply_receiver
                .recv()
                .unwrap_or(agent::ApprovalDecision::Deny)
        },
        cancel,
    )?;
    let response = format_agent_answer(&outcome.answer);
    send_rendered_markdown_stream(&sender, &response);
    Ok((prompt.to_string(), response))
}

fn send_rendered_markdown_stream(sender: &Sender<TurnEvent>, markdown: &str) {
    let rendered = render_terminal_markdown(markdown);
    let _ = sender.send(TurnEvent::RenderedMarkdown(rendered));
}

fn rendered_markdown_stream_chunks(rendered: &str) -> Vec<String> {
    if rendered.is_empty() {
        return Vec::new();
    }
    rendered.split_inclusive('\n').map(str::to_string).collect()
}

fn terminal_stream_chunk_delay(chunk: &str, base_delay: Duration) -> Duration {
    if chunk.trim().is_empty() {
        Duration::ZERO
    } else {
        base_delay
    }
}

fn rendered_markdown_stream_delay() -> Duration {
    std::env::var("DEEPSEEK_RENDERED_STREAM_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_RENDERED_MARKDOWN_STREAM_DELAY)
}

fn rendered_markdown_effective_stream_delay(
    chunks: &[String],
    requested_delay: Duration,
) -> Duration {
    if requested_delay.is_zero() {
        return Duration::ZERO;
    }
    let sleepable_chunks = chunks
        .iter()
        .take(chunks.len().saturating_sub(1))
        .filter(|chunk| !chunk.trim().is_empty())
        .count();
    if sleepable_chunks == 0 {
        return Duration::ZERO;
    }
    let capped_delay_millis =
        DEFAULT_RENDERED_MARKDOWN_STREAM_MAX_DELAY.as_millis() / sleepable_chunks as u128;
    if capped_delay_millis == 0 {
        return Duration::ZERO;
    }
    let capped_delay = Duration::from_millis(capped_delay_millis as u64);
    requested_delay.min(capped_delay)
}

fn stream_rendered_markdown(
    composer: &mut DockedComposer,
    rendered: &str,
    delay: Duration,
) -> Result<(), String> {
    let chunks = rendered_markdown_stream_chunks(rendered);
    let delay = rendered_markdown_effective_stream_delay(&chunks, delay);
    let chunk_count = chunks.len();
    for (index, chunk) in chunks.iter().enumerate() {
        composer.stream_above(chunk)?;
        if index + 1 < chunk_count {
            let chunk_delay = terminal_stream_chunk_delay(chunk, delay);
            if !chunk_delay.is_zero() {
                thread::sleep(chunk_delay);
            }
        }
    }
    Ok(())
}

fn drain_turn_events(
    receiver: &Receiver<TurnEvent>,
    composer: &mut DockedComposer,
    disconnected_message: &str,
    progress_started: Option<Instant>,
    active_tool_steps: &mut Vec<agent::AgentStep>,
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
    let mut answer_streamed = false;
    let mut approval = None;
    loop {
        match receiver.try_recv() {
            Ok(TurnEvent::Delta(delta)) => {
                answer_streamed = true;
                chunk.push_str(&delta);
            }
            Ok(TurnEvent::RenderedMarkdown(rendered)) => {
                if !chunk.is_empty() {
                    composer.stream_above(&chunk)?;
                    chunk.clear();
                }
                stream_rendered_markdown(composer, &rendered, rendered_markdown_stream_delay())?;
                answer_streamed = true;
            }
            Ok(TurnEvent::ToolStep(step)) => {
                if !chunk.is_empty() {
                    composer.stream_above(&chunk)?;
                    chunk.clear();
                }
                active_tool_steps.push(step);
                if let Some(started) = progress_started {
                    composer.progress_dock(&context_scan_status(started, active_tool_steps))?;
                }
            }
            Ok(TurnEvent::ApprovalRequest(request, reply)) => {
                if !chunk.is_empty() {
                    composer.stream_above(&chunk)?;
                    chunk.clear();
                }
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
        answer_streamed = true;
    }
    Ok((complete, answer_streamed, approval))
}

fn start_context_scan(
    composer: &mut DockedComposer,
    tool_steps: &[agent::AgentStep],
) -> Result<Instant, String> {
    let started = Instant::now();
    composer.hide_cursor()?;
    composer.progress_dock(&context_scan_status(started, tool_steps))?;
    Ok(started)
}

fn context_scan_status(started: Instant, tool_steps: &[agent::AgentStep]) -> String {
    let mut lines = vec![format!("Loading {}s", started.elapsed().as_secs())];
    if !tool_steps.is_empty() {
        lines.push(String::new());
        lines.extend(
            tool_steps
                .iter()
                .map(|step| format!("agent step {}: {}", step.label(), step.tool)),
        );
    }
    lines.join("\n")
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

fn is_agent_task_choice(prompt: &str) -> bool {
    matches!(prompt, "y" | "yes" | "yes agent" | "agent task" | "agent")
}

fn is_agent_task_cancel_choice(prompt: &str) -> bool {
    matches!(prompt, "n" | "no")
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
    if is_commit_audit_prompt(&normalized) {
        return true;
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

fn run_prompt_buffered_rendered(
    prior_messages: &[Message],
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    sender: Sender<TurnEvent>,
    cancel: CancellationToken,
) -> Result<(String, String), String> {
    cancel.check()?;
    let runtime_state = runtime::load(model)?;
    let mut messages = prior_messages.to_vec();
    messages.push(provider::user_message(prompt));
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        let response = runtime::debug_response(prompt, model);
        let delay = runtime::debug_stream_delay();
        if let Some(delay) = delay {
            thread::sleep(delay);
            cancel.check()?;
        }
        for delta in response.chars() {
            cancel.check()?;
            let _ = sender.send(TurnEvent::Delta(delta.to_string()));
            if let Some(delay) = delay {
                thread::sleep(delay);
            }
        }
        response
    } else {
        let response =
            provider::chat_quiet_cancelled(&messages, model, temperature, None, &cancel)?;
        send_rendered_markdown_stream(&sender, &response);
        response
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
        let response = provider::chat_quiet(&messages, model, temperature, None)?;
        send_rendered_markdown_stream(&sender, &response);
        response
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
        agent_route_confirmation, approval_decision_for_choice, cap_interactive_memory,
        context_scan_status, is_agent_task_cancel_choice, is_agent_task_choice, is_end_command,
        is_exit_command, is_workspace_agent_prompt, no_pending_agent_task_text,
        parse_agent_task_command, parse_shell_read_command,
        rendered_markdown_effective_stream_delay, rendered_markdown_stream_chunks, shell_pwd_text,
        task_root_for_prompt, terminal_stream_chunk_delay, workspace_agent_root_for_prompt,
        ShellReadCommand,
    };
    use crate::agent;
    use crate::input::ApprovalChoice;
    use crate::provider;
    use crate::runtime;
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::time::Instant;

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
    fn approval_for_session_records_tool_type() {
        let mut approved = HashSet::new();
        let decision = approval_decision_for_choice(
            "run_shell",
            ApprovalChoice::ApproveForSession,
            &mut approved,
        );

        assert_eq!(decision, agent::ApprovalDecision::ApproveForSession);
        assert!(approved.contains("run_shell"));
        assert!(!approved.contains("propose_patch"));
    }

    #[test]
    fn approval_once_and_reject_do_not_record_session_tool() {
        let mut approved = HashSet::new();

        assert_eq!(
            approval_decision_for_choice("run_shell", ApprovalChoice::ApproveOnce, &mut approved),
            agent::ApprovalDecision::Approve
        );
        assert!(approved.is_empty());

        assert_eq!(
            approval_decision_for_choice("run_shell", ApprovalChoice::Reject, &mut approved),
            agent::ApprovalDecision::Deny
        );
        assert!(approved.is_empty());
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
            "inspect shell denial gate",
            "inspect patch approval gate",
            "audit commit 3ca875a",
            "3ca875a — [repo] Close analysis followups < can you audit this commit",
        ] {
            assert_eq!(
                workspace_agent_root_for_prompt(prompt, Some(selected)),
                Some(selected.to_path_buf()),
                "{prompt}"
            );
        }
    }

    #[test]
    fn smoke_fixture_phrases_do_not_route_as_production_tasks() {
        let selected = Path::new("/tmp/selected-workspace");
        for prompt in [
            concat!("try a shell", " command"),
            concat!("deny shell", " command"),
            concat!("approve shell", " command"),
            concat!("deny patch", " edit"),
            concat!("approve patch", " edit"),
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
    fn rendered_markdown_stream_chunks_preserve_line_boundaries() {
        assert_eq!(
            rendered_markdown_stream_chunks("one\n\nthree\n"),
            vec!["one\n", "\n", "three\n"]
        );
        assert_eq!(rendered_markdown_stream_chunks("tail"), vec!["tail"]);
        assert!(rendered_markdown_stream_chunks("").is_empty());
    }

    #[test]
    fn terminal_stream_chunk_delay_skips_blank_lines() {
        let delay = std::time::Duration::from_millis(12);
        assert_eq!(terminal_stream_chunk_delay("answer\n", delay), delay);
        assert_eq!(
            terminal_stream_chunk_delay("\n", delay),
            std::time::Duration::ZERO
        );
    }

    #[test]
    fn rendered_markdown_effective_stream_delay_keeps_small_outputs_at_default() {
        let chunks = rendered_markdown_stream_chunks("one\n\ntwo\nthree\n");
        assert_eq!(
            rendered_markdown_effective_stream_delay(&chunks, std::time::Duration::from_millis(12)),
            std::time::Duration::from_millis(12)
        );
    }

    #[test]
    fn rendered_markdown_effective_stream_delay_caps_large_outputs() {
        let rendered = (0..301)
            .map(|index| format!("line {index}\n"))
            .collect::<String>();
        let chunks = rendered_markdown_stream_chunks(&rendered);
        assert_eq!(
            rendered_markdown_effective_stream_delay(&chunks, std::time::Duration::from_millis(12)),
            std::time::Duration::from_millis(4)
        );
    }

    #[test]
    fn rendered_markdown_effective_stream_delay_can_disable_or_skip_sleep() {
        assert_eq!(
            rendered_markdown_effective_stream_delay(
                &rendered_markdown_stream_chunks("one\n"),
                std::time::Duration::ZERO
            ),
            std::time::Duration::ZERO
        );
        assert_eq!(
            rendered_markdown_effective_stream_delay(
                &rendered_markdown_stream_chunks("\n\n"),
                std::time::Duration::from_millis(12)
            ),
            std::time::Duration::ZERO
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
    fn context_scan_status_shows_elapsed_loading_seconds() {
        let status = context_scan_status(Instant::now(), &[]);
        assert_eq!(status, "Loading 0s");
        assert!(!status.ends_with('\n'));
    }

    #[test]
    fn context_scan_status_lists_tool_steps_transiently() {
        let status = context_scan_status(
            Instant::now(),
            &[
                agent::AgentStep {
                    step: 1,
                    item: None,
                    total: 1,
                    tool: "list_files".to_string(),
                },
                agent::AgentStep {
                    step: 2,
                    item: Some(1),
                    total: 2,
                    tool: "read_file".to_string(),
                },
                agent::AgentStep {
                    step: 2,
                    item: Some(2),
                    total: 2,
                    tool: "read_file".to_string(),
                },
            ],
        );
        assert!(status.contains("Loading 0s"));
        assert!(status.contains("agent step 1: list_files"));
        assert!(status.contains("agent step 2.1: read_file"));
        assert!(status.contains("agent step 2.2: read_file"));
    }
}
