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
    None
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
    use super::{parse_root_command, path_boundary_clarify_text, root_status};
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
        assert_eq!(infer_natural_root("fix main.rs"), None);
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
