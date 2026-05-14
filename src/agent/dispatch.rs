use super::approval_text;
use super::read_tools;
use super::workspace::Workspace;
use super::write_tools;
use super::{ApprovalMode, ApprovalRequest, ApprovalScope, ToolCall};
use crate::internet;

pub(super) fn approval_request(
    workspace: &Workspace,
    step: usize,
    call: &ToolCall,
) -> Option<ApprovalRequest> {
    match call.name.as_str() {
        "run_shell" => {
            let command = call
                .arguments
                .get("command")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing command>");
            let cwd = call
                .arguments
                .get("cwd")
                .and_then(|value| value.as_str())
                .unwrap_or(".");
            let reason = call
                .arguments
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("no reason provided");
            Some(ApprovalRequest {
                step,
                tool: call.name.clone(),
                root: workspace.root.clone(),
                scope: ApprovalScope::Shell,
                summary: approval_text::shell_summary(cwd, reason, command),
            })
        }
        "propose_patch" => {
            let path = call
                .arguments
                .get("path")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing path>");
            let reason = call
                .arguments
                .get("reason")
                .and_then(|value| value.as_str())
                .unwrap_or("no reason provided");
            let find = call
                .arguments
                .get("find")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing find>");
            let replace = call
                .arguments
                .get("replace")
                .and_then(|value| value.as_str())
                .unwrap_or("<missing replace>");
            Some(ApprovalRequest {
                step,
                tool: call.name.clone(),
                root: workspace.root.clone(),
                scope: ApprovalScope::Write,
                summary: approval_text::patch_summary(path, reason, find, replace),
            })
        }
        _ => None,
    }
}

pub(super) fn execute_tool(
    workspace: &Workspace,
    call: &ToolCall,
    approval_mode: ApprovalMode,
) -> String {
    match call.name.as_str() {
        "list_files" => read_tools::list_files(workspace, call),
        "read_file" => read_tools::read_file(workspace, call),
        "search_files" => read_tools::search_files(workspace, call),
        "inspect_tree" => read_tools::inspect_tree(workspace, call),
        "web_search" => internet::web_search_tool(&call.arguments),
        "fetch_url" => internet::fetch_url_tool(&call.arguments),
        "run_shell" => write_tools::run_shell(workspace, call, approval_mode),
        "propose_patch" => write_tools::propose_patch(workspace, call, approval_mode),
        other => format!("error: unknown agent tool `{other}`"),
    }
}
