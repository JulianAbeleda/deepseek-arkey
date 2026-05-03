use std::collections::VecDeque;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Intent {
    Chat,
    Task,
    Clarify,
}

pub(crate) fn classify_intent(
    prompt: &str,
    has_recent_task_context: bool,
    workspace_root: Option<&Path>,
) -> Intent {
    let normalized = normalize_prompt(prompt);
    if normalized.is_empty() {
        return Intent::Chat;
    }
    if is_clarify_prompt(&normalized) {
        return Intent::Clarify;
    }
    if is_chat_prompt(&normalized) {
        return Intent::Chat;
    }
    if is_task_prompt(&normalized, has_recent_task_context)
        || references_workspace_file(prompt, workspace_root)
    {
        if workspace_root.is_none() {
            return Intent::Clarify;
        }
        return Intent::Task;
    }
    Intent::Chat
}

fn normalize_prompt(prompt: &str) -> String {
    prompt
        .trim()
        .to_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_punctuation() { ' ' } else { ch })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_chat_prompt(prompt: &str) -> bool {
    let chat_prefixes = [
        "what ",
        "why ",
        "how ",
        "should we ",
        "does this make sense",
        "can you explain",
        "help me understand",
        "explain ",
    ];
    chat_prefixes
        .iter()
        .any(|prefix| prompt.starts_with(prefix))
}

fn is_clarify_prompt(prompt: &str) -> bool {
    matches!(
        prompt,
        "can you look at this" | "can you look at this please"
    )
}

fn is_task_prompt(prompt: &str, has_recent_task_context: bool) -> bool {
    let task_verbs = [
        "fix",
        "add",
        "remove",
        "update",
        "implement",
        "run",
        "commit",
        "push",
        "audit",
        "refactor",
        "create",
        "delete",
        "rename",
        "test",
        "build",
    ];
    let first = prompt.split_whitespace().next().unwrap_or("");
    if task_verbs.contains(&first) {
        return true;
    }
    has_recent_task_context
        && [
            "lets do it",
            "let s do it",
            "go ahead",
            "make that change",
            "apply the patch",
            "ship it",
        ]
        .iter()
        .any(|phrase| prompt.starts_with(phrase))
}

fn references_workspace_file(prompt: &str, workspace_root: Option<&Path>) -> bool {
    prompt.split_whitespace().any(|token| {
        let Some(token) = clean_prompt_token(token) else {
            return false;
        };
        is_path_like_token(token) || workspace_root.is_some_and(|root| root.join(token).is_file())
    })
}

fn clean_prompt_token(token: &str) -> Option<&str> {
    let mut token = token.trim_matches(|ch: char| {
        ch == '"'
            || ch == '\''
            || ch == '`'
            || ch == ','
            || ch == ':'
            || ch == ';'
            || ch == '?'
            || ch == '!'
            || ch == '('
            || ch == ')'
            || ch == '['
            || ch == ']'
            || ch == '{'
            || ch == '}'
    });
    if token.ends_with('.') && !token.ends_with("..") {
        token = &token[..token.len() - 1];
    }
    (!token.is_empty()).then_some(token)
}

fn is_path_like_token(token: &str) -> bool {
    token.contains('/')
        || matches!(
            token,
            "README.md" | "Cargo.toml" | "Cargo.lock" | "package.json" | "tsconfig.json"
        )
        || token.ends_with(".rs")
        || token.ends_with(".py")
        || token.ends_with(".md")
        || token.ends_with(".toml")
        || token.ends_with(".json")
}

pub(crate) fn recent_task_context(queued: &VecDeque<String>) -> bool {
    queued.iter().any(|prompt| {
        let normalized = normalize_prompt(prompt);
        is_task_prompt(&normalized, false)
    })
}

pub(crate) fn path_boundary_violation(prompt: &str, root: &Path) -> Option<PathBuf> {
    let root = normalize_path(root);
    prompt
        .split_whitespace()
        .filter_map(clean_prompt_token)
        .filter(|token| is_path_like_token(token))
        .filter_map(|token| {
            let path = PathBuf::from(token);
            let resolved = if path.is_absolute() {
                normalize_path(&path)
            } else {
                normalize_path(&root.join(path))
            };
            if resolved.starts_with(&root) {
                None
            } else {
                Some(resolved)
            }
        })
        .next()
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            component => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::{classify_intent, path_boundary_violation, Intent};
    use std::path::Path;

    #[test]
    fn detects_path_references_outside_root() {
        let root = Path::new("/tmp/workspace");
        assert_eq!(path_boundary_violation("fix README.md", root), None);
        assert_eq!(path_boundary_violation("fix src/main.rs.", root), None);
        assert!(path_boundary_violation("fix ../outside.md", root).is_some());
        assert!(path_boundary_violation("audit /Users/example/.ssh/config", root).is_some());
    }

    #[test]
    fn classifies_open_questions_as_chat() {
        let root = Path::new("/tmp/workspace");
        assert_eq!(
            classify_intent("what do you think about this design?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("how do I fix this?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("explain this codebase", false, Some(root)),
            Intent::Chat
        );
    }

    #[test]
    fn classifies_imperatives_as_tasks_inside_workspace() {
        let root = Path::new("/tmp/workspace");
        assert_eq!(
            classify_intent(
                "fix the duplicate helper in both repos and run tests",
                false,
                Some(root)
            ),
            Intent::Task
        );
        assert_eq!(classify_intent("fix it", false, Some(root)), Intent::Task);
        assert_eq!(
            classify_intent("implement a logout button", false, Some(root)),
            Intent::Task
        );
    }

    #[test]
    fn classifies_ambiguous_or_home_tasks_as_clarify() {
        assert_eq!(
            classify_intent(
                "can you look at this?",
                false,
                Some(Path::new("/tmp/workspace"))
            ),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("fix the README in this directory", false, None),
            Intent::Clarify
        );
    }
}
