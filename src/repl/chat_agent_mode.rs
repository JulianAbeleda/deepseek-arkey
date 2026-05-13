use super::*;

pub(super) fn run_confirmed_agent_task(
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

pub(super) fn run_interactive_agent(model: &str, temperature: Option<f32>) -> Result<(), String> {
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
                    print!("{}", ui::agent_help(&current_model, &root));
                    continue;
                }
                commands::ChatCommand::ChatMode => {
                    run_interactive_chat(&current_model, temperature, false)?;
                    break;
                }
                commands::ChatCommand::SwitchToAgent => {
                    println!("mode: agent");
                    continue;
                }
                commands::ChatCommand::DirectAgentTask(_) => {
                    // Already in agent mode; require the user to enter the task directly.
                    ui::print_error("already in agent mode; enter the task without /agent");
                    continue;
                }
                commands::ChatCommand::Root(_) => {
                    ui::print_error("/root is only available in chat mode");
                    continue;
                }
                commands::ChatCommand::Status => {
                    print!("{}", interactive_agent_status(&current_model, &root)?);
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
