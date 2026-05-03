use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct Workspace {
    pub(super) root: PathBuf,
}

impl Workspace {
    pub(super) fn new(root: PathBuf) -> Result<Self, String> {
        let root = root.canonicalize().map_err(|err| err.to_string())?;
        Ok(Self { root })
    }

    pub(super) fn resolve_existing(&self, requested: &str) -> Result<PathBuf, String> {
        let requested_path = Path::new(requested);
        if requested_path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        }) {
            return Err("path must stay inside workspace root".to_string());
        }
        let joined = self.root.join(requested_path);
        let resolved = joined.canonicalize().map_err(|err| err.to_string())?;
        if !resolved.starts_with(&self.root) {
            return Err("path escapes workspace root".to_string());
        }
        Ok(resolved)
    }

    pub(super) fn display_path(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .ok()
            .and_then(|path| path.to_str())
            .filter(|path| !path.is_empty())
            .unwrap_or(".")
            .to_string()
    }

    pub(super) fn contains_existing(&self, path: &Path) -> bool {
        path.canonicalize()
            .map(|path| path.starts_with(&self.root))
            .unwrap_or(false)
    }
}
