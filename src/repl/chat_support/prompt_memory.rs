use super::super::*;
use crate::internet;

pub(in crate::repl::chat) fn run_prompt_buffered_rendered(
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
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        messages.push(provider::user_message(prompt));
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
        if let Some(context) = internet::web_context_message_for_prompt_lossy(prompt, |warning| {
            eprintln!("warning: {warning}");
        }) {
            messages.push(context);
        }
        messages.push(provider::user_message(prompt));
        let response =
            provider::chat_quiet_cancelled(&messages, model, temperature, None, &cancel)?;
        send_rendered_markdown_stream(&sender, &response);
        response
    };
    Ok((prompt.to_string(), response))
}

pub(in crate::repl::chat) fn run_prompt_with_memory(
    memory: &mut Vec<Message>,
    prompt: &str,
    model: &str,
    temperature: Option<f32>,
    stream: bool,
    sender: Option<Sender<TurnEvent>>,
) -> Result<(String, String), String> {
    let runtime_state = runtime::load(model)?;
    let mut messages = memory.clone();
    let response = if runtime_state.backend == RuntimeBackend::Debug {
        messages.push(provider::user_message(prompt));
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
        if let Some(context) = internet::web_context_message_for_prompt_lossy(prompt, |warning| {
            eprintln!("warning: {warning}");
        }) {
            messages.push(context);
        }
        messages.push(provider::user_message(prompt));
        let response = provider::chat_quiet(&messages, model, temperature, None)?;
        send_rendered_markdown_stream(&sender, &response);
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
    push_interactive_turn(memory, prompt.to_string(), response.clone());
    Ok((prompt.to_string(), response))
}

pub(in crate::repl::chat) fn push_interactive_turn(
    memory: &mut Vec<Message>,
    prompt: String,
    response: String,
) {
    memory.push(provider::user_message(prompt));
    memory.push(provider::assistant_message(response));
    cap_interactive_memory(memory);
}

pub(in crate::repl::chat) fn cap_interactive_memory(memory: &mut Vec<Message>) {
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

pub(in crate::repl::chat) fn total_message_chars(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|message| message.content.chars().count())
        .sum()
}
