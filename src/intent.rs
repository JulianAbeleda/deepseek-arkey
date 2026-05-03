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
    let is_task = is_task_prompt(&normalized, has_recent_task_context)
        || references_workspace_file(prompt, workspace_root);
    let has_natural_root = references_natural_location(&normalized);
    if is_task {
        if workspace_root.is_none() && !has_natural_root {
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
        "read",
        "scan",
        "inspect",
    ];
    let first = prompt.split_whitespace().next().unwrap_or("");
    if task_verbs.contains(&first) {
        return true;
    }
    if has_task_phrase(prompt) || has_scoped_problem_statement(prompt) {
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

fn has_task_phrase(prompt: &str) -> bool {
    [
        "go through",
        "look at",
        "look through",
        "take a look at",
        "review",
        "read this",
        "read my files",
        "read files",
        "scan my",
        "scan the",
        "inspect my",
        "inspect the",
    ]
    .iter()
    .any(|phrase| starts_with_phrase(prompt, phrase))
}

fn has_scoped_problem_statement(prompt: &str) -> bool {
    let has_problem = [
        "is broken",
        "is failing",
        "are failing",
        "has errors",
        "have errors",
        "needs cleanup",
        "needs refactor",
        "needs fixing",
        "needs to be fixed",
        "is not working",
        "are not working",
        "does not work",
        "doesnt work",
        "don t work",
        "won t build",
        "will not build",
        "is crashing",
        "keeps crashing",
        "is a mess",
        "are a mess",
    ]
    .iter()
    .any(|pattern| prompt.contains(pattern));
    has_problem && references_task_scope(prompt)
}

fn references_task_scope(prompt: &str) -> bool {
    [
        "this repo",
        "this project",
        "this codebase",
        "the repo",
        "the project",
        "the codebase",
        "my repo",
        "my project",
        "my codebase",
        "my files",
        "these files",
        "the config",
        "config",
        "the code",
        "my code",
        "the tests",
        "tests",
        "test suite",
        "the build",
        "build",
        "the app",
        "app",
        "desktop",
        "downloads",
        "documents",
    ]
    .iter()
    .any(|scope| prompt.contains(scope))
}

fn starts_with_phrase(prompt: &str, phrase: &str) -> bool {
    prompt == phrase
        || prompt
            .strip_prefix(phrase)
            .is_some_and(|rest| rest.starts_with(' '))
}

fn references_natural_location(prompt: &str) -> bool {
    ["desktop", "downloads", "documents"]
        .iter()
        .any(|loc| prompt.contains(loc))
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
        assert_eq!(
            classify_intent("what is a desktop?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("how do I read files in Rust?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("can you explain config files?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("can you explain this repo structure?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("what is a config file?", false, Some(root)),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("why is my code broken?", false, Some(root)),
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
        assert_eq!(
            classify_intent("the config is broken", false, None),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("this repo needs cleanup", false, None),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("review this project", false, None),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("my files are a mess", false, None),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("the tests are failing", false, None),
            Intent::Clarify
        );
    }

    #[test]
    fn classifies_declarative_tasks_inside_workspace() {
        let root = Path::new("/tmp/workspace");
        assert_eq!(
            classify_intent("the config is broken", false, Some(root)),
            Intent::Task
        );
        assert_eq!(
            classify_intent("this repo needs cleanup", false, Some(root)),
            Intent::Task
        );
        assert_eq!(
            classify_intent("review this project", false, Some(root)),
            Intent::Task
        );
        assert_eq!(
            classify_intent("go through the codebase", false, Some(root)),
            Intent::Task
        );
        assert_eq!(
            classify_intent("my files are a mess", false, Some(root)),
            Intent::Task
        );
        assert_eq!(
            classify_intent("the tests are failing", false, Some(root)),
            Intent::Task
        );
        assert_eq!(
            classify_intent("the app is not working", false, Some(root)),
            Intent::Task
        );
        assert_eq!(
            classify_intent("the build won't build", false, Some(root)),
            Intent::Task
        );
    }

    #[test]
    fn natural_location_phrases_route_to_task() {
        assert_eq!(
            classify_intent("read my files on my desktop", false, None),
            Intent::Task
        );
        assert_eq!(
            classify_intent("scan my desktop", false, None),
            Intent::Task
        );
        assert_eq!(classify_intent("scan desktop", false, None), Intent::Task);
        assert_eq!(
            classify_intent("look through downloads", false, None),
            Intent::Task
        );
        assert_eq!(
            classify_intent("go through downloads", false, None),
            Intent::Task
        );
        assert_eq!(
            classify_intent("inspect my documents folder", false, None),
            Intent::Task
        );
    }

    #[test]
    fn what_questions_stay_chat_even_with_natural_locations() {
        assert_eq!(
            classify_intent("what files are on my desktop?", false, None),
            Intent::Chat
        );
        assert_eq!(
            classify_intent("how do I find files in downloads?", false, None),
            Intent::Chat
        );
    }

    #[test]
    fn read_scan_inspect_without_natural_root_clarify_when_no_workspace() {
        assert_eq!(
            classify_intent("read this function", false, None),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("inspect the config", false, None),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("look at the config", false, None),
            Intent::Clarify
        );
    }

    #[test]
    fn scoped_mess_statements_route_to_task_context() {
        assert_eq!(
            classify_intent("my files are a mess", false, None),
            Intent::Clarify
        );
        assert_eq!(
            classify_intent("my desktop files are a mess", false, None),
            Intent::Task
        );
        assert_eq!(
            classify_intent(
                "this repo is a mess",
                false,
                Some(Path::new("/tmp/workspace"))
            ),
            Intent::Task
        );
    }
}
