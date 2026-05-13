use super::*;

pub(super) fn spawn_prompt_turn(
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

pub(super) fn handle_dock_approval_choice(
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

pub(super) fn approval_decision_for_choice(
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

pub(super) fn is_cd_previous_request(prompt: &str) -> bool {
    matches!(
        prompt.trim().to_ascii_lowercase().as_str(),
        "cd -" | "cd previous" | "cd back"
    )
}

pub(super) fn spawn_docked_turn(
    prior_messages: &[Message],
    prompt: String,
    selected_root: Option<&Path>,
    model: String,
    temperature: Option<f32>,
    legacy_routing: bool,
) -> InFlightTurn {
    if let Some(task) = commands::parse_agent_task_command(&prompt) {
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

pub(super) fn spawn_agent_turn(
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

pub(super) fn run_agent_streaming(
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
    let outcome = agent::run_agent_with_handlers(
        prompt,
        model,
        temperature,
        agent::AgentRunOptions::new(agent::AgentConfig::new(root, agent::DEFAULT_MAX_STEPS))
            .approval_mode(agent::ApprovalMode::External)
            .quiet_cache(true)
            .cancel(cancel),
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
    )?;
    let response = format_agent_answer(&outcome.answer);
    send_rendered_markdown_stream(&sender, &response);
    Ok((prompt.to_string(), response))
}

pub(super) fn send_rendered_markdown_stream(sender: &Sender<TurnEvent>, markdown: &str) {
    let rendered = render_terminal_markdown(markdown);
    let _ = sender.send(TurnEvent::RenderedMarkdown(rendered));
}

pub(super) fn rendered_markdown_stream_chunks(rendered: &str) -> Vec<String> {
    if rendered.is_empty() {
        return Vec::new();
    }
    rendered.split_inclusive('\n').map(str::to_string).collect()
}

pub(super) fn terminal_stream_chunk_delay(chunk: &str, base_delay: Duration) -> Duration {
    if chunk.trim().is_empty() {
        Duration::ZERO
    } else {
        base_delay
    }
}

pub(super) fn rendered_markdown_stream_delay() -> Duration {
    std::env::var("DEEPSEEK_RENDERED_STREAM_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_RENDERED_MARKDOWN_STREAM_DELAY)
}

pub(super) fn rendered_markdown_effective_stream_delay(
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

pub(super) fn stream_rendered_markdown(
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

pub(super) fn drain_turn_events(
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

pub(super) fn start_context_scan(
    composer: &mut DockedComposer,
    tool_steps: &[agent::AgentStep],
) -> Result<Instant, String> {
    let started = Instant::now();
    composer.hide_cursor()?;
    composer.progress_dock(&context_scan_status(started, tool_steps))?;
    Ok(started)
}

pub(super) fn context_scan_status(started: Instant, tool_steps: &[agent::AgentStep]) -> String {
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

pub(super) fn agent_route_confirmation(root: &Path) -> String {
    format!(
        "route: agent task\nroot: {}\nRun this as an agent task?\nType y to continue, n to cancel, or /chat to keep chatting.\n",
        root.display()
    )
}

pub(super) fn clarify_route_text() -> String {
    "route: unclear\nDo you want chat analysis or an agent task?\nType /chat to discuss, /root <path> to choose a workspace, or /agent <task> to execute.\n".to_string()
}

pub(super) enum ShellReadCommand {
    Pwd,
    Ls(String),
}

pub(super) fn parse_shell_read_command(prompt: &str) -> Option<ShellReadCommand> {
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

pub(super) fn shell_pwd_text(root: Option<&Path>) -> String {
    match root {
        Some(root) => format!("{}\n", root.display()),
        None => "root: unset\n".to_string(),
    }
}

pub(super) fn no_pending_agent_task_text() -> String {
    "route: unclear\nNo pending agent task to confirm.\nType /root <path> to choose a workspace, then repeat the task; or type /agent <task> with the leading slash to run one directly.\n".to_string()
}

pub(super) fn is_agent_task_choice(prompt: &str) -> bool {
    matches!(prompt, "y" | "yes" | "yes agent" | "agent task" | "agent")
}

pub(super) fn is_agent_task_cancel_choice(prompt: &str) -> bool {
    matches!(prompt, "n" | "no")
}

pub(super) fn task_root_for_prompt(prompt: &str, selected_root: Option<&Path>) -> Option<PathBuf> {
    infer_natural_root(prompt)
        .or_else(|| selected_root.map(Path::to_path_buf))
        .or_else(|| effective_workspace_root(None))
}

pub(super) fn workspace_agent_root_for_prompt(
    prompt: &str,
    selected_root: Option<&Path>,
) -> Option<PathBuf> {
    if is_workspace_chat_followup(&normalize_workspace_prompt(prompt)) {
        return None;
    }
    infer_natural_root(prompt).or_else(|| {
        is_workspace_agent_prompt(prompt)
            .then(|| effective_workspace_root(selected_root))
            .flatten()
    })
}

pub(super) fn is_workspace_agent_prompt(prompt: &str) -> bool {
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

pub(super) fn is_workspace_chat_followup(prompt: &str) -> bool {
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

pub(super) fn normalize_workspace_prompt(prompt: &str) -> String {
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

pub(super) fn run_prompt_buffered_rendered(
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

pub(super) fn run_prompt_with_memory(
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

pub(super) fn push_interactive_turn(memory: &mut Vec<Message>, prompt: String, response: String) {
    memory.push(provider::user_message(prompt));
    memory.push(provider::assistant_message(response));
    cap_interactive_memory(memory);
}

pub(super) fn cap_interactive_memory(memory: &mut Vec<Message>) {
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

pub(super) fn total_message_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum()
}

pub(super) fn interactive_status(model: &str) -> Result<String, String> {
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

pub(super) fn interactive_chat_status(
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

pub(super) fn print_agent_banner(model: &str, root: &Path) {
    println!("deepseek [{model}] agent");
    println!("workspace: {}", root.display());
    println!("read tools on - edits require yes apply - shell requires yes run");
    println!("Enter send - ? help - /chat - /model - /status - /end - /exit");
}

pub(super) fn agent_prompt_text(model: &str) -> String {
    format!("deepseek [{model}] agent › ")
}

pub(super) fn interactive_agent_status(model: &str, root: &Path) -> Result<String, String> {
    let mut output = interactive_status(model)?;
    output.push_str(&format!("mode: agent\nroot: {}\n", root.display()));
    Ok(output)
}

pub(super) fn reset_persisted_chat_messages() -> Result<Option<SessionState>, String> {
    let Some(mut state) = session::load()? else {
        return Ok(None);
    };
    if !state.messages.is_empty() {
        state.clear_messages();
        session::save(&state)?;
    }
    Ok(Some(state))
}

pub(super) fn update_active_session_model(model: &str) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.model = model.to_string();
    session::save(&state)
}

pub(super) fn approve_session_agent_root(root: &Path) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.approve_agent_root(root)?;
    session::save(&state)
}

pub(super) fn clear_session_agent_root() -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    state.clear_agent_root();
    session::save(&state)
}

pub(super) fn save_selected_root(root: Option<&Path>) -> Result<(), String> {
    let Some(mut state) = session::load()? else {
        return Ok(());
    };
    match root {
        Some(root) => state.select_root(root)?,
        None => state.clear_selected_root(),
    }
    session::save(&state)
}

pub(super) fn agent_root_matches(approved: Option<&Path>, root: &Path) -> bool {
    let Some(approved) = approved else {
        return false;
    };
    paths_equal(approved, root)
}

pub(super) fn paths_equal(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}
