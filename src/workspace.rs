use std::path::{Path, PathBuf};

fn workspace_root() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if home.as_ref().is_some_and(|home| paths_equal(&cwd, home)) {
        return None;
    }
    Some(cwd)
}

pub(crate) fn effective_workspace_root(selected_root: Option<&Path>) -> Option<PathBuf> {
    selected_root.map(Path::to_path_buf).or_else(workspace_root)
}

pub(crate) fn infer_natural_root(prompt: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    let lowered = prompt.to_lowercase();
    if lowered.contains("desktop") {
        return Some(home.join("Desktop"));
    }
    if lowered.contains("downloads") {
        return Some(home.join("Downloads"));
    }
    if lowered.contains("documents") {
        return Some(home.join("Documents"));
    }
    if lowered.contains("env folder")
        || lowered.contains("env directory")
        || lowered.contains("my env")
    {
        return Some(home.join("env"));
    }
    None
}

#[cfg(test)]
pub(crate) fn parse_navigation_request(prompt: &str) -> Result<Option<PathBuf>, String> {
    parse_navigation_request_from(prompt, None)
}

pub(crate) fn parse_navigation_request_from(
    prompt: &str,
    base_root: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    let prompt = prompt.trim();
    let lowered = prompt.to_lowercase();
    let Some((target, explicit_path)) = navigation_target(prompt, &lowered) else {
        return Ok(None);
    };
    let target = clean_navigation_target(target);
    if target.is_empty() {
        return Ok(None);
    }
    if let Some(root) = navigation_alias_root(target) {
        return canonical_dir(root).map(Some);
    }
    let path = expand_path(target, base_root);
    if path.is_dir() {
        return canonical_dir(path).map(Some);
    }
    if explicit_path || looks_like_path_target(target) {
        return Err(format!("{} is not a directory", path.display()));
    }
    Ok(None)
}

pub(crate) fn parse_root_command(prompt: &str) -> Option<Option<&str>> {
    if prompt == "/root" {
        return Some(None);
    }
    prompt
        .strip_prefix("/root ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(Some)
}

pub(crate) fn update_selected_root(root_arg: &str) -> Result<Option<PathBuf>, String> {
    if matches!(root_arg, "clear" | "reset" | "cwd") {
        return Ok(None);
    }
    let path = PathBuf::from(root_arg);
    let path = if path.is_absolute() {
        path
    } else {
        std::env::current_dir()
            .map_err(|err| err.to_string())?
            .join(path)
    };
    let root = path
        .canonicalize()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    Ok(Some(root))
}

pub(crate) fn update_selected_root_from(
    root_arg: &str,
    base_root: Option<&Path>,
) -> Result<Option<PathBuf>, String> {
    if matches!(root_arg, "clear" | "reset" | "cwd") {
        return Ok(None);
    }
    let path = expand_path(root_arg, base_root);
    canonical_dir(path).map(Some)
}

fn navigation_target<'a>(prompt: &'a str, lowered: &str) -> Option<(&'a str, bool)> {
    for verb in [
        "cd", "go", "navigate", "switch", "move", "change", "enter", "open",
    ] {
        if lowered == verb {
            return None;
        }
        if let Some(rest) = lowered
            .strip_prefix(verb)
            .and_then(|rest| rest.strip_prefix(' '))
        {
            let offset = prompt.len() - rest.len();
            return Some((&prompt[offset..], verb == "cd"));
        }
    }
    None
}

fn clean_navigation_target(target: &str) -> &str {
    let mut target = target.trim();
    for separator in [" and ", " then "] {
        if let Some(index) = target.to_lowercase().find(separator) {
            target = &target[..index];
        }
    }
    target = trim_navigation_punctuation(target.trim());
    for prep in ["into ", "inside ", "from ", "to ", "in "] {
        if target.to_lowercase().starts_with(prep) {
            target = &target[prep.len()..];
            break;
        }
    }
    target = trim_navigation_punctuation(target.trim());
    if target.to_lowercase().starts_with("the ") {
        target = &target[4..];
    }
    for suffix in [" folder", " directory", " repo", " repository"] {
        if let Some(stripped) = target.strip_suffix(suffix) {
            return stripped.trim();
        }
    }
    target
}

fn trim_navigation_punctuation(target: &str) -> &str {
    let target = target.trim_matches(|ch: char| matches!(ch, ',' | ';' | ':' | '"' | '\''));
    if target == "." || target == ".." || looks_like_path_target(target) {
        return target;
    }
    target.trim_matches('.')
}

fn expand_path(path: &str, base_root: Option<&Path>) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            return home.join(rest);
        }
    }
    let path = PathBuf::from(path);
    if path.is_absolute() {
        path
    } else if let Some(base_root) = base_root {
        base_root.join(path)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    }
}

fn navigation_alias_root(target: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    match target.to_lowercase().as_str() {
        "desktop" => Some(home.join("Desktop")),
        "downloads" => Some(home.join("Downloads")),
        "documents" => Some(home.join("Documents")),
        "env" | "my env" => Some(home.join("env")),
        _ => None,
    }
}

fn looks_like_path_target(target: &str) -> bool {
    target == "~"
        || target.starts_with("~/")
        || target == "."
        || target == ".."
        || target.starts_with('/')
        || target.starts_with("./")
        || target.starts_with("../")
        || target.contains('/')
}

fn canonical_dir(path: PathBuf) -> Result<PathBuf, String> {
    let root = path
        .canonicalize()
        .map_err(|err| format!("{}: {err}", path.display()))?;
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }
    Ok(root)
}

pub(crate) fn root_status(root: Option<&Path>, explicit: bool) -> String {
    match root {
        Some(root) => format!(
            "root: {}\nroot-source: {}\n",
            root.display(),
            if explicit { "explicit" } else { "cwd" }
        ),
        None => "root: unset\nroot-source: none\nUse /root <path> before running workspace tasks from $HOME.\n".to_string(),
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

pub(crate) fn path_boundary_clarify_text(root: &Path, path: &Path) -> String {
    let suggested_root = path.parent().unwrap_or(root);
    format!(
        "route: unclear\nReferenced path is outside the selected workspace root.\nroot: {}\npath: {}\nSuggested root: {}\nType /root {} to choose that workspace, or /chat to discuss.\n",
        root.display(),
        path.display(),
        suggested_root.display(),
        suggested_root.display()
    )
}

#[cfg(test)]
mod tests {
    use super::{
        parse_navigation_request, parse_navigation_request_from, parse_root_command,
        path_boundary_clarify_text, root_status, update_selected_root_from,
    };
    use std::fs;
    use std::path::Path;

    #[test]
    fn parses_root_slash_command() {
        assert_eq!(parse_root_command("/root"), Some(None));
        assert_eq!(parse_root_command("/root   .  "), Some(Some(".")));
        assert_eq!(parse_root_command("/root clear"), Some(Some("clear")));
        assert_eq!(parse_root_command("root ."), None);
    }

    #[test]
    fn root_status_reports_source() {
        let root = Path::new("/tmp/workspace");
        assert!(root_status(Some(root), true).contains("root-source: explicit"));
        assert!(root_status(Some(root), false).contains("root-source: cwd"));
        assert!(root_status(None, false).contains("root: unset"));
    }

    #[test]
    fn infers_natural_roots_from_prompt() {
        use super::infer_natural_root;
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap();
        assert_eq!(
            infer_natural_root("read my files on my desktop"),
            Some(home.join("Desktop"))
        );
        assert_eq!(
            infer_natural_root("scan my downloads"),
            Some(home.join("Downloads"))
        );
        assert_eq!(
            infer_natural_root("inspect documents"),
            Some(home.join("Documents"))
        );
        assert_eq!(
            infer_natural_root("go to my env folder"),
            Some(home.join("env"))
        );
        assert_eq!(infer_natural_root("switch to the deepseek repo"), None);
        assert_eq!(infer_natural_root("fix main.rs"), None);
    }

    #[test]
    fn parses_navigation_requests_as_persistent_roots() {
        let home = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap();
        let env_root = home.join("env");
        let isolated_root = tempfile::tempdir().unwrap();
        if env_root.is_dir() {
            assert_eq!(
                parse_navigation_request("go to my env folder and stay there")
                    .unwrap()
                    .as_deref(),
                Some(env_root.canonicalize().unwrap().as_path())
            );
            assert_eq!(
                parse_navigation_request("navigate into my env folder")
                    .unwrap()
                    .as_deref(),
                Some(env_root.canonicalize().unwrap().as_path())
            );
            assert_eq!(
                parse_navigation_request("cd into my env folder")
                    .unwrap()
                    .as_deref(),
                Some(env_root.canonicalize().unwrap().as_path())
            );
            assert_eq!(
                parse_navigation_request("go into the env folder")
                    .unwrap()
                    .as_deref(),
                Some(env_root.canonicalize().unwrap().as_path())
            );
            assert_eq!(
                parse_navigation_request_from(
                    "enter the deepseek repo",
                    Some(isolated_root.path())
                )
                .unwrap(),
                None
            );
            assert_eq!(
                parse_navigation_request_from("navigate into deepseek", Some(isolated_root.path()))
                    .unwrap(),
                None
            );
            assert_eq!(
                parse_navigation_request_from("open the minimax repo", Some(isolated_root.path()))
                    .unwrap(),
                None
            );
            assert!(
                parse_navigation_request_from("cd into minimax", Some(isolated_root.path()))
                    .is_err()
            );
        }
        assert_eq!(
            parse_navigation_request("go through downloads").unwrap(),
            None
        );
        assert_eq!(parse_navigation_request("fix this repo").unwrap(), None);
        assert_eq!(
            parse_navigation_request("switch to main branch").unwrap(),
            None
        );
        assert_eq!(parse_navigation_request("stay in touch").unwrap(), None);
        assert_eq!(parse_navigation_request("open a ticket").unwrap(), None);
        assert!(parse_navigation_request("go to /definitely/not/here").is_err());
        assert!(parse_navigation_request("cd into definitely-not-here").is_err());
    }

    #[test]
    fn relative_navigation_uses_selected_root_as_base() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("sample_project");
        fs::create_dir_all(&child).unwrap();

        assert_eq!(
            parse_navigation_request_from("cd into sample_project", Some(root.path()))
                .unwrap()
                .as_deref(),
            Some(child.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn relative_navigation_supports_parent_and_current_directory() {
        let root = tempfile::tempdir().unwrap();
        let parent = root.path().join("parent");
        let child = parent.join("child");
        let sibling = parent.join("sibling");
        fs::create_dir_all(&child).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        assert_eq!(
            parse_navigation_request_from("cd ..", Some(&child))
                .unwrap()
                .as_deref(),
            Some(parent.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            parse_navigation_request_from("cd ../sibling", Some(&child))
                .unwrap()
                .as_deref(),
            Some(sibling.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            parse_navigation_request_from("cd .", Some(&child))
                .unwrap()
                .as_deref(),
            Some(child.canonicalize().unwrap().as_path())
        );
        assert_eq!(
            update_selected_root_from("../sibling", Some(&child))
                .unwrap()
                .as_deref(),
            Some(sibling.canonicalize().unwrap().as_path())
        );
    }

    #[test]
    fn outside_root_clarify_suggests_parent_root() {
        let text = path_boundary_clarify_text(
            Path::new("/tmp/workspace"),
            Path::new("/Users/example/.ssh/config"),
        );
        assert!(text.contains("Suggested root: /Users/example/.ssh"));
        assert!(text.contains("Type /root /Users/example/.ssh"));
    }
}
