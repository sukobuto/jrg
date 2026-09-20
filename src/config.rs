//! Resolve credentials without modifying the process environment or executing shell code.

use anyhow::{Context, Result, bail};
use std::{
    env,
    fs::File,
    path::{Path, PathBuf},
};

/// Find the nearest project credential file without inheriting another project's key.
pub fn discover_env_file(cwd: &Path) -> Option<PathBuf> {
    let root = cwd.ancestors().find(|dir| dir.join(".git").exists());
    for dir in cwd.ancestors() {
        let file = dir.join(".jrgenv");
        if file.is_file() {
            return Some(file);
        }
        // A directory outside Git has no trustworthy project boundary; only its
        // own .jrgenv participates. A worktree's .git file counts as a boundary.
        if root.is_none() || root == Some(dir) {
            break;
        }
    }
    None
}

/// Resolve one key while leaving every unrelated dotenv setting unused.
pub fn api_key(env_file: Option<&Path>) -> Result<String> {
    if let Ok(key) = env::var("TYPESAFE_API_KEY")
        && !key.trim().is_empty()
    {
        return Ok(key);
    }
    if let Some(path) = env_file {
        let file = File::open(path).context("Could not open the credential file")?;
        let mut key = None;
        for item in dotenvy::from_read_iter(file) {
            // Dotenv parser errors contain the source line, which can be a secret.
            // Use a fixed diagnostic and never load values into global process state.
            let (name, value) =
                item.map_err(|_| anyhow::anyhow!("Invalid dotenv syntax in credential file"))?;
            if name == "TYPESAFE_API_KEY" && key.is_none() {
                key = Some(value);
            }
        }
        if let Some(key) = key.filter(|key| !key.trim().is_empty()) {
            return Ok(key);
        }
    }
    bail!(
        "Set TYPESAFE_API_KEY or add it to .jrgenv (or use --env-file). Use --dry-run for local search."
    )
}

/// Keep credential paths out of candidate output even when explicit rg paths bypass globs.
pub struct Exclusions {
    env_file: Option<PathBuf>,
}

impl Exclusions {
    /// Remember the resolved credential path so alternate spellings cannot expose it.
    pub fn new(env_file: Option<&Path>) -> Self {
        Self {
            env_file: env_file.and_then(|path| path.canonicalize().ok()),
        }
    }

    /// Exclude credential names and aliases of the explicitly loaded credential file.
    pub fn contains(&self, path: &Path) -> bool {
        if let Some(name) = path.file_name().and_then(|name| name.to_str())
            && is_env_name(name)
        {
            return true;
        }
        // Canonicalization also catches symlinked .env files. Filtering happens
        // before decoding source lines so rejected credentials never become snippets.
        if let Ok(resolved) = path.canonicalize() {
            if self.env_file.as_ref() == Some(&resolved) {
                return true;
            }
            if let Some(name) = resolved.file_name().and_then(|name| name.to_str()) {
                return is_env_name(name);
            }
        }
        false
    }
}

fn is_env_name(name: &str) -> bool {
    name == ".env" || name.starts_with(".env.") || name == ".jrgenv" || name.starts_with(".jrgenv.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_file_stops_at_git_and_worktree_boundaries() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("project");
        let nested = root.join("src/nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join(".git"), "gitdir: elsewhere").unwrap();
        std::fs::write(temp.path().join(".jrgenv"), "outside").unwrap();
        assert_eq!(discover_env_file(&nested), None);
        std::fs::write(root.join(".jrgenv"), "root").unwrap();
        assert_eq!(discover_env_file(&nested), Some(root.join(".jrgenv")));
        std::fs::write(root.join("src/.jrgenv"), "nearest").unwrap();
        assert_eq!(discover_env_file(&nested), Some(root.join("src/.jrgenv")));
    }

    #[test]
    fn outside_git_only_current_directory_is_checked() {
        let temp = tempfile::tempdir().unwrap();
        let child = temp.path().join("child");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(temp.path().join(".jrgenv"), "parent").unwrap();
        assert_eq!(discover_env_file(&child), None);
        std::fs::write(child.join(".jrgenv"), "local").unwrap();
        assert_eq!(discover_env_file(&child), Some(child.join(".jrgenv")));
    }
}
