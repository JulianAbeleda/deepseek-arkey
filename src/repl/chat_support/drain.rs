use super::super::*;

pub(in crate::repl::chat) fn drain_turn_events(
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
