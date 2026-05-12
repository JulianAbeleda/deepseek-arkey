pub(super) fn shell_summary(cwd: &str, reason: &str, command: &str) -> String {
    format!(
        "approval required: run_shell\ncwd: {cwd}\nreason: {reason}\ncommand: {command}\nType yes run to approve, n to deny.\n"
    )
}

pub(super) fn patch_summary(path: &str, reason: &str, find: &str, replace: &str) -> String {
    format!(
        "approval required: propose_patch\npath: {path}\nreason: {reason}\n--- find ---\n{find}\n--- replace ---\n{replace}\nType yes apply to approve, n to deny.\n"
    )
}

#[cfg(test)]
mod tests {
    use super::{patch_summary, shell_summary};

    #[test]
    fn shell_summary_keeps_modal_preview_fields() {
        let summary = shell_summary(".", "run tests", "cargo test --offline");

        assert!(summary.contains("approval required: run_shell"));
        assert!(summary.contains("cwd: ."));
        assert!(summary.contains("reason: run tests"));
        assert!(summary.contains("command: cargo test --offline"));
        assert!(summary.contains("Type yes run"));
    }

    #[test]
    fn patch_summary_keeps_find_replace_sections() {
        let summary = patch_summary("README.md", "docs", "old", "new");

        assert!(summary.contains("approval required: propose_patch"));
        assert!(summary.contains("path: README.md"));
        assert!(summary.contains("--- find ---\nold"));
        assert!(summary.contains("--- replace ---\nnew"));
        assert!(summary.contains("Type yes apply"));
    }
}
