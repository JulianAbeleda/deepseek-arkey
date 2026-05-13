use super::super::*;

pub(in crate::repl::chat) fn run_agent_streaming(
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

pub(in crate::repl::chat) fn send_rendered_markdown_stream(
    sender: &Sender<TurnEvent>,
    markdown: &str,
) {
    let rendered = render_terminal_markdown(markdown);
    let _ = sender.send(TurnEvent::RenderedMarkdown(rendered));
}

pub(in crate::repl::chat) fn rendered_markdown_stream_chunks(rendered: &str) -> Vec<String> {
    if rendered.is_empty() {
        return Vec::new();
    }
    rendered.split_inclusive('\n').map(str::to_string).collect()
}

pub(in crate::repl::chat) fn terminal_stream_chunk_delay(
    chunk: &str,
    base_delay: Duration,
) -> Duration {
    if chunk.trim().is_empty() {
        Duration::ZERO
    } else {
        base_delay
    }
}

pub(in crate::repl::chat) fn rendered_markdown_stream_delay() -> Duration {
    std::env::var("DEEPSEEK_RENDERED_STREAM_DELAY_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_RENDERED_MARKDOWN_STREAM_DELAY)
}

pub(in crate::repl::chat) fn rendered_markdown_effective_stream_delay(
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

pub(in crate::repl::chat) fn stream_rendered_markdown(
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
