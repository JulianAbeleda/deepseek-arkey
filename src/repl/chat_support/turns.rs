use super::super::*;

pub(in crate::repl::chat) fn spawn_prompt_turn(
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

pub(in crate::repl::chat) fn handle_dock_approval_choice(
    composer: &mut DockedComposer,
    approval: PendingDockApproval,
    choice: ApprovalChoice,
    session_approved_scopes: &mut HashSet<ApprovalGrant>,
) -> Result<(), String> {
    composer.clear_approval_modal()?;
    let decision = approval_decision_for_choice(&approval.request, choice, session_approved_scopes);
    match choice {
        ApprovalChoice::ApproveOnce => {
            let _ = approval.reply.send(decision);
            composer.print_above(&format!("approval: approved {}\n", approval.request.tool))?;
        }
        ApprovalChoice::ApproveForSession => {
            let _ = approval.reply.send(decision);
            composer.print_above(&format!(
                "approval: approved {} for root\n",
                approval.request.scope.label()
            ))?;
        }
        ApprovalChoice::Reject => {
            let _ = approval.reply.send(decision);
            composer.print_above(&format!("approval: denied {}\n", approval.request.tool))?;
        }
    }
    Ok(())
}

pub(in crate::repl::chat) fn approval_decision_for_choice(
    request: &agent::ApprovalRequest,
    choice: ApprovalChoice,
    session_approved_scopes: &mut HashSet<ApprovalGrant>,
) -> agent::ApprovalDecision {
    match choice {
        ApprovalChoice::ApproveOnce => agent::ApprovalDecision::Approve,
        ApprovalChoice::ApproveForSession => {
            session_approved_scopes.insert(approval_grant(request));
            agent::ApprovalDecision::ApproveForSession
        }
        ApprovalChoice::Reject => agent::ApprovalDecision::Deny,
    }
}

pub(in crate::repl::chat) fn approval_grant(request: &agent::ApprovalRequest) -> ApprovalGrant {
    ApprovalGrant {
        root: request.root.clone(),
        scope: request.scope,
    }
}

pub(in crate::repl::chat) fn is_cd_previous_request(prompt: &str) -> bool {
    matches!(
        prompt.trim().to_ascii_lowercase().as_str(),
        "cd -" | "cd previous" | "cd back"
    )
}
