use std::path::PathBuf;

use crate::cancel::CancellationToken;

pub(super) const MAX_TOOL_CHARS: usize = 12_000;
pub const DEFAULT_MAX_STEPS: usize = 1000;

pub struct AgentConfig {
    pub root: PathBuf,
    pub max_steps: usize,
}

impl AgentConfig {
    pub fn new(root: impl Into<PathBuf>, max_steps: usize) -> Self {
        Self {
            root: root.into(),
            max_steps: max_steps.max(1),
        }
    }
}

pub struct AgentRunOptions {
    pub(super) config: AgentConfig,
    pub(super) approval_mode: ApprovalMode,
    pub(super) quiet_cache: bool,
    pub(super) cancel: Option<CancellationToken>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentChatRoute {
    Standard,
    Quiet,
    Cancelled,
    QuietCancelled,
}

impl AgentRunOptions {
    pub fn new(config: AgentConfig) -> Self {
        Self {
            config,
            approval_mode: ApprovalMode::Interactive,
            quiet_cache: false,
            cancel: None,
        }
    }

    pub fn approval_mode(mut self, approval_mode: ApprovalMode) -> Self {
        self.approval_mode = approval_mode;
        self
    }

    pub fn quiet_cache(mut self, quiet_cache: bool) -> Self {
        self.quiet_cache = quiet_cache;
        self
    }

    pub fn cancel(mut self, cancel: CancellationToken) -> Self {
        self.cancel = Some(cancel);
        self
    }
}

#[derive(Debug, Clone)]
pub struct AgentOutcome {
    pub answer: String,
    pub steps: usize,
    pub transcript_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalMode {
    Interactive,
    Deny,
    Approved,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approve,
    ApproveForSession,
    Deny,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ApprovalScope {
    Shell,
    Write,
}

impl ApprovalScope {
    pub fn label(self) -> &'static str {
        match self {
            Self::Shell => "shell",
            Self::Write => "writes",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub step: usize,
    pub tool: String,
    pub root: PathBuf,
    pub scope: ApprovalScope,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStep {
    pub step: usize,
    pub item: Option<usize>,
    pub total: usize,
    pub tool: String,
}

impl AgentStep {
    pub fn label(&self) -> String {
        match self.item {
            Some(item) => format!("{}.{}", self.step, item),
            None => self.step.to_string(),
        }
    }
}
