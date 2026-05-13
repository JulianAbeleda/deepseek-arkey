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
    path_boundary_clarify_text, root_status, update_selected_root, update_selected_root_from,
};

use super::commands;
use super::commands::execute_runtime_command;

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
        match commands::parse_chat_command(prompt) {
            commands::CommandParse::NotACommand => {}
            commands::CommandParse::Invalid(error) => {
                ui::print_error(error.message());
                continue;
            }
            commands::CommandParse::Valid(command) => match command {
                commands::ChatCommand::Exit => break,
                commands::ChatCommand::End => {
                    let _ = session::delete()?;
                    ui::print_session_ended();
                    break;
                }
                commands::ChatCommand::Help => {
                    ui::print_help(&current_model);
                    continue;
                }
                commands::ChatCommand::ChatMode => {
                    println!("mode: chat");
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
                commands::ChatCommand::Status => {
                    ui::print_status(&current_model)?;
                    continue;
                }
                commands::ChatCommand::Root(_) => {
                    ui::print_error("/root is only available in docked interactive chat");
                    continue;
                }
                commands::ChatCommand::Runtime(command) => {
                    println!("{}", execute_runtime_command(&current_model, command)?);
                    continue;
                }
                commands::ChatCommand::Debug(mode) => {
                    let output = match mode {
                        Some(mode) => runtime::debug_result(&current_model, Some(mode), false)?,
                        None => runtime::toggle_debug_result(&current_model)?,
                    };
                    println!("{output}");
                    continue;
                }
                commands::ChatCommand::Model(next_model) => {
                    if let Some(next_model) = next_model {
                        current_model = next_model.to_string();
                        update_active_session_model(&current_model)?;
                        let runtime_state =
                            runtime::load(&current_model)?.with_model(&current_model);
                        runtime::save(&runtime_state)?;
                        ui::print_model_set(&current_model);
                    } else {
                        ui::print_model_help(&current_model);
                    }
                    continue;
                }
            },
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
        match commands::parse_chat_command(prompt) {
            commands::CommandParse::NotACommand => {}
            commands::CommandParse::Invalid(error) => {
                composer.print_above(&format!("error: {}\n", error.message()))?;
                continue;
            }
            commands::CommandParse::Valid(command) => match command {
                commands::ChatCommand::Exit => break,
                commands::ChatCommand::End => {
                    let _ = session::delete()?;
                    composer.print_above("session ended\n")?;
                    break;
                }
                commands::ChatCommand::Help => {
                    composer.print_above(&ui::interactive_help(&current_model))?;
                    continue;
                }
                commands::ChatCommand::ChatMode => {
                    pending_agent_task = None;
                    composer.print_above("mode: chat\n")?;
                    continue;
                }
                commands::ChatCommand::SwitchToAgent => {
                    composer.print_above("switching to agent mode\n")?;
                    switch_to_agent = true;
                    break;
                }
                commands::ChatCommand::DirectAgentTask(task) => {
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
                    context_scan_started =
                        Some(start_context_scan(&mut composer, &active_tool_steps)?);
                    in_flight = Some(spawn_agent_turn(
                        task.to_string(),
                        root,
                        current_model.clone(),
                        temperature,
                    ));
                    continue;
                }
                commands::ChatCommand::Status => {
                    composer.print_above(&interactive_chat_status(
                        &current_model,
                        effective_workspace_root(selected_root.as_deref()).as_deref(),
                        selected_root.is_some(),
                        approved_agent_root.as_deref(),
                        memory.len() / 2,
                    )?)?;
                    continue;
                }
                commands::ChatCommand::Root(root_arg) => {
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
                                        effective_workspace_root(selected_root.as_deref())
                                            .as_deref(),
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
                commands::ChatCommand::Runtime(command) => {
                    let output = execute_runtime_command(&current_model, command)?;
                    runtime_state = runtime::load(&current_model)?;
                    composer.print_above(&output)?;
                    continue;
                }
                commands::ChatCommand::Debug(mode) => {
                    let output = match mode {
                        Some(mode) => runtime::debug_result(&current_model, Some(mode), false)?,
                        None => runtime::toggle_debug_result(&current_model)?,
                    };
                    runtime_state = runtime::load(&current_model)?;
                    composer.set_prompt(ui::prompt_text(&runtime_state.label(&current_model)))?;
                    composer.print_above(&output)?;
                    continue;
                }
                commands::ChatCommand::Model(next_model) => {
                    match next_model {
                        Some(next_model) => {
                            current_model = next_model.to_string();
                            update_active_session_model(&current_model)?;
                            runtime_state = runtime_state.with_model(current_model.clone());
                            runtime::save(&runtime_state)?;
                            composer.set_prompt(ui::prompt_text(
                                &runtime_state.label(&current_model),
                            ))?;
                            composer.print_above(&format!("model set: {current_model}\n"))?;
                        }
                        None => composer.print_above(&ui::model_help(&current_model))?,
                    }
                    continue;
                }
            },
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

#[path = "chat_agent_mode.rs"]
mod agent_mode;
use agent_mode::{run_confirmed_agent_task, run_interactive_agent};

#[path = "chat_support.rs"]
mod support;
use support::*;

#[cfg(test)]
#[path = "chat_tests.rs"]
mod tests;
