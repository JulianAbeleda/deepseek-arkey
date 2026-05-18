use super::commands::{is_end_command, is_exit_command, parse_agent_task_command};
use super::{
    approval_decision_for_choice, approval_grant, cap_interactive_memory, context_scan_status,
    interactive_chat_status, is_workspace_agent_prompt, parse_shell_read_command,
    rendered_markdown_effective_stream_delay, rendered_markdown_stream_chunks, shell_pwd_text,
    task_root_for_prompt, terminal_stream_chunk_delay, workspace_agent_root_for_prompt,
    ApprovalGrant, ShellReadCommand,
};
use crate::agent;
use crate::input::ApprovalChoice;
use crate::provider;
use crate::runtime;
use crate::test_support::env_lock;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn approval_request(root: &Path, scope: agent::ApprovalScope) -> agent::ApprovalRequest {
    let tool = match scope {
        agent::ApprovalScope::Shell => "run_shell",
        agent::ApprovalScope::Write => "propose_patch",
    };
    agent::ApprovalRequest {
        step: 1,
        tool: tool.to_string(),
        root: root.to_path_buf(),
        scope,
        summary: "approval required".to_string(),
    }
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
fn approval_for_session_records_tool_type() {
    let mut approved = HashSet::new();
    let root = Path::new("/tmp/deepseek-root-a");
    let request = approval_request(root, agent::ApprovalScope::Shell);
    let decision =
        approval_decision_for_choice(&request, ApprovalChoice::ApproveForSession, &mut approved);

    assert_eq!(decision, agent::ApprovalDecision::ApproveForSession);
    assert!(approved.contains(&approval_grant(&request)));
    assert!(!approved.contains(&approval_grant(&approval_request(
        root,
        agent::ApprovalScope::Write
    ))));
    assert!(!approved.contains(&approval_grant(&approval_request(
        Path::new("/tmp/deepseek-root-b"),
        agent::ApprovalScope::Shell
    ))));
}

#[test]
fn approval_once_and_reject_do_not_record_session_tool() {
    let mut approved = HashSet::new();
    let request = approval_request(
        Path::new("/tmp/deepseek-root-a"),
        agent::ApprovalScope::Shell,
    );

    assert_eq!(
        approval_decision_for_choice(&request, ApprovalChoice::ApproveOnce, &mut approved),
        agent::ApprovalDecision::Approve
    );
    assert!(approved.is_empty());

    assert_eq!(
        approval_decision_for_choice(&request, ApprovalChoice::Reject, &mut approved),
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
fn arrow_chain_trailing_task_routes_to_agent_root() {
    let _guard = env_lock();
    let root = tempfile::tempdir().unwrap();
    let structure = root.path().join("env").join("tinygrad").join("structure");
    fs::create_dir_all(&structure).unwrap();
    let previous_home = std::env::var_os("HOME");
    std::env::set_var("HOME", root.path());

    assert_eq!(
        workspace_agent_root_for_prompt(
            "Go to my env -> tinygrad -> structure -> find your purpose.",
            None
        )
        .as_deref(),
        Some(structure.canonicalize().unwrap().as_path())
    );
    assert_eq!(
        workspace_agent_root_for_prompt(
            "Go to my env -> tinygrad -> structure -> what is your role?",
            None
        )
        .as_deref(),
        Some(structure.canonicalize().unwrap().as_path())
    );

    if let Some(previous_home) = previous_home {
        std::env::set_var("HOME", previous_home);
    } else {
        std::env::remove_var("HOME");
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
fn chat_status_reports_direct_agent_routing_and_tool_permissions() {
    let output = interactive_chat_status(
        "deepseek-v4-flash",
        Some(Path::new("/tmp/workspace")),
        true,
        &HashSet::<ApprovalGrant>::new(),
        0,
    )
    .unwrap();
    assert!(output.contains("agent-routing: direct"));
    assert!(output.contains("write-permission: confirm-required"));
    assert!(output.contains("shell-permission: confirm-required"));
    assert!(!output.contains("agent-session: confirm-required"));
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
