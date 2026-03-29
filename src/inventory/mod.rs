//! Absorption Engine — grepvec's code inventory system
//!
//! Parses source files across the Enscribe platform using tree-sitter,
//! extracts items (functions, structs, enums, traits, impls, etc.),
//! detects edges (calls, imports, implements), and stores everything
//! in Postgres for documentation, search, and visualization.

pub mod items;
pub mod edges;
pub mod qualified_names;
pub mod external_deps;
pub mod db;
pub mod biography;
pub mod scope;
pub mod boundary;

use crate::tree_sitter_validator::Language;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Result of absorbing an entire repository.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbsorptionResult {
    pub repo_name: String,
    pub files: Vec<AbsorbedFile>,
    pub total_items: usize,
    pub total_edges: usize,
    pub errors: Vec<AbsorptionError>,
}

/// Result of absorbing a single file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbsorbedFile {
    pub path: String,
    pub language: String,
    pub line_count: usize,
    pub items: Vec<ExtractedItem>,
    pub edges: Vec<ExtractedEdge>,
}

/// An extracted code item (function, struct, enum, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedItem {
    pub name: String,
    pub item_type: ItemType,
    pub visibility: Visibility,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub line_start: usize,
    pub line_end: usize,
    pub is_test: bool,
    pub is_async: bool,
    pub attributes: Vec<String>,
    pub child_indices: Vec<usize>,
    pub qualified_name: Option<String>,
}

/// Types of code items that can be extracted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemType {
    Function,
    Struct,
    Enum,
    Trait,
    Impl,
    Module,
    Constant,
    Static,
    TypeAlias,
    MacroDefinition,
    UseDeclaration,
    // Python
    Class,
    // TypeScript
    Interface,
}

impl ItemType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ItemType::Function => "function",
            ItemType::Struct => "struct",
            ItemType::Enum => "enum",
            ItemType::Trait => "trait",
            ItemType::Impl => "impl",
            ItemType::Module => "module",
            ItemType::Constant => "constant",
            ItemType::Static => "static",
            ItemType::TypeAlias => "type_alias",
            ItemType::MacroDefinition => "macro_definition",
            ItemType::UseDeclaration => "use_declaration",
            ItemType::Class => "class",
            ItemType::Interface => "interface",
        }
    }
}

/// Visibility of a code item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    PublicCrate,
    PublicSuper,
    PublicIn(String),
    Private,
}

impl Visibility {
    pub fn as_str(&self) -> &str {
        match self {
            Visibility::Public => "pub",
            Visibility::PublicCrate => "pub(crate)",
            Visibility::PublicSuper => "pub(super)",
            Visibility::PublicIn(path) => path.as_str(),
            Visibility::Private => "private",
        }
    }
}

/// An extracted edge between code items or to external systems.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractedEdge {
    pub source_item_name: String,
    pub edge_type: EdgeType,
    pub target_name: String,
    pub line: usize,
}

/// Types of edges between code items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EdgeType {
    Calls,
    Implements,
    Imports,
    Contains,
    MacroInvocation,
    ExternalDep,
    UsesType,
}

impl EdgeType {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeType::Calls => "calls",
            EdgeType::Implements => "implements",
            EdgeType::Imports => "imports",
            EdgeType::Contains => "contains",
            EdgeType::MacroInvocation => "macro_invocation",
            EdgeType::ExternalDep => "external_dep",
            EdgeType::UsesType => "uses_type",
        }
    }
}

/// An error that occurred during absorption.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AbsorptionError {
    pub file: String,
    pub message: String,
}

/// Repository configuration for absorption.
#[derive(Debug, Clone)]
pub struct RepoConfig {
    pub name: String,
    pub path: PathBuf,
    pub primary_language: String,
}

/// Absorb a single repository: parse all source files, extract items and edges.
/// If `changed_since` is Some(sha), only absorb files changed since that git SHA.
pub fn absorb_repo(config: &RepoConfig, changed_since: Option<&str>) -> AbsorptionResult {
    let mut result = AbsorptionResult {
        repo_name: config.name.clone(),
        files: Vec::new(),
        total_items: 0,
        total_edges: 0,
        errors: Vec::new(),
    };

    let all_files = match collect_source_files(&config.path) {
        Ok(f) => f,
        Err(e) => {
            result.errors.push(AbsorptionError {
                file: config.path.display().to_string(),
                message: e,
            });
            return result;
        }
    };

    // Filter to only changed files if --changed-since was provided
    let files = if let Some(sha) = changed_since {
        match get_changed_files(&config.path, sha) {
            Ok(changed) => {
                let changed_set: std::collections::HashSet<PathBuf> = changed.into_iter().collect();
                all_files
                    .into_iter()
                    .filter(|f| changed_set.contains(f))
                    .collect()
            }
            Err(e) => {
                result.errors.push(AbsorptionError {
                    file: config.path.display().to_string(),
                    message: format!("git diff failed: {}", e),
                });
                return result;
            }
        }
    } else {
        all_files
    };

    for file_path in files {
        match absorb_file(&file_path, &config.path) {
            Ok(absorbed) => {
                result.total_items += absorbed.items.len();
                result.total_edges += absorbed.edges.len();
                result.files.push(absorbed);
            }
            Err(e) => {
                result.errors.push(AbsorptionError {
                    file: file_path.display().to_string(),
                    message: e,
                });
            }
        }
    }

    result
}

/// Get files changed since a git SHA in a repository.
fn get_changed_files(repo_path: &Path, since_sha: &str) -> Result<Vec<PathBuf>, String> {
    let output = std::process::Command::new("git")
        .args(["diff", "--name-only", since_sha, "HEAD"])
        .current_dir(repo_path)
        .output()
        .map_err(|e| format!("Failed to run git diff: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git diff failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<PathBuf> = stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| repo_path.join(l))
        .collect();

    Ok(files)
}

/// Absorb a single file: parse, extract items, extract edges.
pub fn absorb_file(file_path: &Path, repo_root: &Path) -> Result<AbsorbedFile, String> {
    let relative_path = file_path
        .strip_prefix(repo_root)
        .unwrap_or(file_path)
        .to_string_lossy()
        .to_string();

    let extension = file_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let language = Language::from_extension(extension)
        .ok_or_else(|| format!("Unsupported language for extension: {}", extension))?;

    let source = std::fs::read_to_string(file_path)
        .map_err(|e| format!("Failed to read {}: {}", relative_path, e))?;

    let line_count = source.lines().count();

    // Parse with tree-sitter
    let mut parser = create_parser(language)?;
    let tree = parser
        .parse(&source, None)
        .ok_or_else(|| format!("Tree-sitter failed to parse {}", relative_path))?;

    // Extract items
    let mut extracted_items = items::extract_items(&source, language, &tree);

    // Compute qualified names
    qualified_names::compute_qualified_names(&relative_path, language, &mut extracted_items);

    // Extract edges
    let extracted_edges = edges::extract_edges(&source, language, &tree, &extracted_items);

    Ok(AbsorbedFile {
        path: relative_path,
        language: language.as_str().to_string(),
        line_count,
        items: extracted_items,
        edges: extracted_edges,
    })
}

/// Collect all parseable source files from a repository.
fn collect_source_files(repo_path: &Path) -> Result<Vec<PathBuf>, String> {
    let ignore_dirs = [
        "target", "node_modules", "dist", "build", ".git", "vendor",
        "__pycache__", ".mypy_cache", ".pytest_cache",
        "venv", ".venv", "env", ".env",
        "generated", ".artifacts", ".enscribe",
        "site-packages", ".tox",
    ];

    let mut files = Vec::new();

    for entry in walkdir::WalkDir::new(repo_path)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let name = e.file_name().to_string_lossy();
            !ignore_dirs.contains(&name.as_ref())
        })
    {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().is_file() {
            if let Some(ext) = entry.path().extension().and_then(|e| e.to_str()) {
                if Language::from_extension(ext).is_some() {
                    files.push(entry.path().to_path_buf());
                }
            }
        }
    }

    files.sort();
    Ok(files)
}

/// Create a tree-sitter parser for the given language.
fn create_parser(language: Language) -> Result<tree_sitter::Parser, String> {
    let mut parser = tree_sitter::Parser::new();
    let ts_lang = match language {
        Language::Rust => tree_sitter_rust::language(),
        Language::TypeScript | Language::JavaScript => tree_sitter_typescript::language_tsx(),
        Language::Python => tree_sitter_python::language(),
    };
    parser
        .set_language(&ts_lang)
        .map_err(|e| format!("Failed to set language: {}", e))?;
    Ok(parser)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_item_type_as_str() {
        assert_eq!(ItemType::Function.as_str(), "function");
        assert_eq!(ItemType::Struct.as_str(), "struct");
        assert_eq!(ItemType::Impl.as_str(), "impl");
    }

    #[test]
    fn test_edge_type_as_str() {
        assert_eq!(EdgeType::Calls.as_str(), "calls");
        assert_eq!(EdgeType::Imports.as_str(), "imports");
    }

    #[test]
    fn test_visibility_as_str() {
        assert_eq!(Visibility::Public.as_str(), "pub");
        assert_eq!(Visibility::Private.as_str(), "private");
        assert_eq!(Visibility::PublicCrate.as_str(), "pub(crate)");
    }

    #[test]
    fn test_absorb_rust_source() {
        let source = r#"
pub fn hello() {
    println!("hello");
}

struct Foo {
    x: i32,
}

impl Foo {
    pub fn new(x: i32) -> Self {
        Self { x }
    }
}
"#;
        // Write to temp file and test
        let dir = std::env::temp_dir().join("grepvec_test_absorb");
        let _ = std::fs::create_dir_all(&dir);
        let file_path = dir.join("test.rs");
        std::fs::write(&file_path, source).unwrap();

        let result = absorb_file(&file_path, &dir).unwrap();
        assert_eq!(result.language, "rust");
        assert!(result.items.len() >= 3, "Expected at least 3 items (fn, struct, impl+method), got {}", result.items.len());

        // Clean up
        let _ = std::fs::remove_dir_all(&dir);
    }
}
