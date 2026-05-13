mod approval_text;
pub(crate) mod commit_audit;
mod decision;
mod r#loop;
mod read_tools;
mod transcript;
mod workspace;
mod write_tools;

#[allow(unused_imports)]
pub use r#loop::{parse_decision, AgentDecision, ToolCall};
#[allow(unused_imports)]
pub use r#loop::{
    read_latest_transcript, read_latest_transcript_summary, run_agent, run_agent_final_only,
    run_agent_with_handlers, AgentConfig, AgentOutcome, AgentRunOptions, AgentStep,
    ApprovalDecision, ApprovalMode, ApprovalRequest, DEFAULT_MAX_STEPS,
};
