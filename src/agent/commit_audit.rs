use std::path::Path;
use std::process::Command;

use crate::safety::{cap_text, redact_text};

const MAX_GIT_OUTPUT_CHARS: usize = 16_000;

pub(super) fn prepare_task(task: &str, root: &Path) -> String {
    let Some(target) = commit_audit_target(task) else {
        return task.to_string();
    };
    let evidence = collect_git_evidence(root, &target);
    format!(
        "Original user request:\n{task}\n\nResolved commit target: {target}\n\nLocal git evidence collected before model review:\n{evidence}\n\nAudit instructions:\n- Audit only from the provided local evidence.\n- Do not ask the user to paste the diff unless evidence collection failed.\n- Lead with findings ordered by severity.\n- Cite concrete files, commit metadata, commands, or evidence snippets when available.\n- State a clear recommendation: ACCEPT, REVISE, or BLOCK."
    )
}

fn commit_audit_target(task: &str) -> Option<String> {
    let normalized = normalize_task(task);
    let words = normalized.split_whitespace().collect::<Vec<_>>();
    if !words.contains(&"audit") || !words.contains(&"commit") {
        return None;
    }
    words
        .iter()
        .copied()
        .find(|word| is_commit_ref(word))
        .map(|word| {
            if word == "head" {
                "HEAD".to_string()
            } else {
                word.to_string()
            }
        })
        .or_else(|| Some("HEAD".to_string()))
}

fn normalize_task(task: &str) -> String {
    task.chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                ' '
            }
        })
        .collect::<String>()
}

fn is_commit_ref(word: &str) -> bool {
    word == "head"
        || (word.len() >= 7 && word.len() <= 40 && word.chars().all(|ch| ch.is_ascii_hexdigit()))
}

fn collect_git_evidence(root: &Path, target: &str) -> String {
    let target = if target == "head" { "HEAD" } else { target };
    let sections = [
        (
            "git status --short --branch",
            run_git(root, &["status", "--short", "--branch"]),
        ),
        (
            "git log --oneline -5",
            run_git(root, &["log", "--oneline", "-5"]),
        ),
        (
            "git show --stat --patch --find-renames <target> --",
            run_git(
                root,
                &["show", "--stat", "--patch", "--find-renames", target, "--"],
            ),
        ),
    ];
    sections
        .into_iter()
        .map(|(label, output)| {
            format!(
                "## {label}\n\n{}",
                redact_text(&cap_text(&output, MAX_GIT_OUTPUT_CHARS))
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn run_git(root: &Path, args: &[&str]) -> String {
    match Command::new("git").args(args).current_dir(root).output() {
        Ok(output) => format!(
            "status: {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
        Err(err) => format!("error: failed to run git: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{commit_audit_target, prepare_task};

    #[test]
    fn detects_commit_audit_targets() {
        assert_eq!(
            commit_audit_target("audit commit 3ca875a"),
            Some("3ca875a".to_string())
        );
        assert_eq!(
            commit_audit_target("3ca875a — [repo] < audit this commit"),
            Some("3ca875a".to_string())
        );
        assert_eq!(
            commit_audit_target("can you audit 3ca875a commit"),
            Some("3ca875a".to_string())
        );
        assert_eq!(
            commit_audit_target("audit this commit"),
            Some("HEAD".to_string())
        );
    }

    #[test]
    fn ignores_non_audit_hash_mentions() {
        assert_eq!(commit_audit_target("show commit 3ca875a"), None);
        assert_eq!(commit_audit_target("audit this repo"), None);
        assert_eq!(commit_audit_target("3ca875a is latest"), None);
    }

    #[test]
    fn prepared_task_embeds_local_evidence_contract() {
        let task = prepare_task("audit commit HEAD", std::path::Path::new("/no/such/root"));
        assert!(task.contains("Original user request:"));
        assert!(task.contains("Resolved commit target: HEAD"));
        assert!(task.contains("Local git evidence collected before model review:"));
        assert!(task.contains("Do not ask the user to paste the diff"));
    }
}
