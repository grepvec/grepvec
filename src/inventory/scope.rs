//! Scope discovery and configuration.
//!
//! Reads and writes `.grepvec/scope.toml` — the control document that tells grepvec
//! which repositories are in scope, their last absorbed SHA, and voice configuration.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const SCOPE_DIR: &str = ".grepvec";
const SCOPE_FILE: &str = "scope.toml";

/// grepvec scope configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShiftScope {
    pub repos: Vec<RepoScope>,
    #[serde(default)]
    pub enscribe: Option<EnscribeScope>,
}

/// A repository in scope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoScope {
    pub name: String,
    pub path: String,
    #[serde(default = "default_language")]
    pub language: String,
    #[serde(default)]
    pub last_sha: Option<String>,
}

fn default_language() -> String {
    "rust".to_string()
}

/// Enscribe integration configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnscribeScope {
    pub collection: String,
    #[serde(default)]
    pub voices: Vec<VoiceConfig>,
}

/// An agent voice configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceConfig {
    pub name: String,
    pub granularity: String,
}

/// Find the scope file by walking up from the current directory.
pub fn find_scope_file(start: &Path) -> Option<PathBuf> {
    let mut dir = start.to_path_buf();
    loop {
        let candidate = dir.join(SCOPE_DIR).join(SCOPE_FILE);
        if candidate.exists() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// Read scope from a `.grepvec/scope.toml` file.
pub fn read_scope(path: &Path) -> Result<ShiftScope, String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {}", path.display(), e))?;
    toml::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {}", path.display(), e))
}

/// Write scope to a `.grepvec/scope.toml` file.
pub fn write_scope(base_dir: &Path, scope: &ShiftScope) -> Result<PathBuf, String> {
    let dir = base_dir.join(SCOPE_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Failed to create {}: {}", dir.display(), e))?;

    let path = dir.join(SCOPE_FILE);
    let content = toml::to_string_pretty(scope)
        .map_err(|e| format!("Failed to serialize scope: {}", e))?;

    std::fs::write(&path, &content)
        .map_err(|e| format!("Failed to write {}: {}", path.display(), e))?;

    Ok(path)
}

/// Update the last_sha for a specific repo in the scope file.
pub fn update_last_sha(scope_path: &Path, repo_name: &str, sha: &str) -> Result<(), String> {
    let mut scope = read_scope(scope_path)?;

    for repo in &mut scope.repos {
        if repo.name == repo_name {
            repo.last_sha = Some(sha.to_string());
        }
    }

    let content = toml::to_string_pretty(&scope)
        .map_err(|e| format!("Failed to serialize scope: {}", e))?;
    std::fs::write(scope_path, &content)
        .map_err(|e| format!("Failed to write {}: {}", scope_path.display(), e))?;

    Ok(())
}

/// Convert scope repos to RepoConfig for absorption.
pub fn to_repo_configs(scope: &ShiftScope) -> Vec<crate::inventory::RepoConfig> {
    scope
        .repos
        .iter()
        .map(|r| crate::inventory::RepoConfig {
            name: r.name.clone(),
            path: PathBuf::from(&r.path),
            primary_language: r.language.clone(),
        })
        .collect()
}

/// Get the current git SHA for a repository path.
pub fn get_git_sha(repo_path: &Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scope_roundtrip() {
        let scope = ShiftScope {
            repos: vec![
                RepoScope {
                    name: "test-repo".to_string(),
                    path: "/tmp/test".to_string(),
                    language: "rust".to_string(),
                    last_sha: Some("abc123".to_string()),
                },
            ],
            enscribe: Some(EnscribeScope {
                collection: "grepvec-test".to_string(),
                voices: vec![
                    VoiceConfig {
                        name: "architectural".to_string(),
                        granularity: "broad".to_string(),
                    },
                ],
            }),
        };

        let toml_str = toml::to_string_pretty(&scope).unwrap();
        let parsed: ShiftScope = toml::from_str(&toml_str).unwrap();

        assert_eq!(parsed.repos.len(), 1);
        assert_eq!(parsed.repos[0].name, "test-repo");
        assert_eq!(parsed.repos[0].last_sha.as_deref(), Some("abc123"));
    }
}
