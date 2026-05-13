use super::super::*;

pub(in crate::repl::chat) fn spawn_docked_turn(
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

pub(in crate::repl::chat) fn spawn_agent_turn(
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
