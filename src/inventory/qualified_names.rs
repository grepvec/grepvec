//! Qualified name resolution.
//!
//! Computes fully qualified names for extracted items based on:
//! - File path → module path (e.g., `src/api/grpc.rs` → `api::grpc`)
//! - Parent nesting (e.g., method inside impl → `Type::method`)
//! - Language-specific conventions

use crate::inventory::{ExtractedItem, ItemType};
use crate::tree_sitter_validator::Language;

/// Compute qualified names for all items in a file.
pub fn compute_qualified_names(
    file_path: &str,
    language: Language,
    items: &mut [ExtractedItem],
) {
    let module_path = match language {
        Language::Rust => rust_module_path(file_path),
        Language::Python => python_module_path(file_path),
        Language::TypeScript | Language::JavaScript => ts_module_path(file_path),
    };

    // First pass: compute qualified names for top-level items
    // (items with no parent in the child_indices of any other item)
    let child_set: std::collections::HashSet<usize> = items
        .iter()
        .flat_map(|i| i.child_indices.iter().copied())
        .collect();

    // We need to build names iteratively because parents reference children by index
    // Process in index order; parent always has lower index than children
    let item_count = items.len();
    let mut names: Vec<Option<String>> = vec![None; item_count];

    for idx in 0..item_count {
        let item = &items[idx];
        let is_top_level = !child_set.contains(&idx);

        let parent_prefix = if is_top_level {
            module_path.clone()
        } else {
            // Find parent
            let parent_name = items.iter().enumerate().find_map(|(pidx, p)| {
                if p.child_indices.contains(&idx) {
                    names[pidx].clone()
                } else {
                    None
                }
            });
            parent_name.unwrap_or_else(|| module_path.clone())
        };

        let qualified = match item.item_type {
            ItemType::UseDeclaration => {
                // Use declarations don't get qualified names
                None
            }
            ItemType::Impl => {
                // Impl blocks use their type name as the qualified prefix
                // "impl Foo" → module::Foo, "impl Trait for Foo" → module::Foo
                let type_name = extract_impl_type_name(&item.name);
                if parent_prefix.is_empty() {
                    Some(type_name)
                } else {
                    Some(format!("{}::{}", parent_prefix, type_name))
                }
            }
            _ => {
                if item.name.is_empty() {
                    None
                } else if parent_prefix.is_empty() {
                    Some(item.name.clone())
                } else {
                    Some(format!("{}::{}", parent_prefix, item.name))
                }
            }
        };

        names[idx] = qualified;
    }

    // Apply computed names
    for (idx, name) in names.into_iter().enumerate() {
        items[idx].qualified_name = name;
    }
}

/// Convert a Rust file path to a module path.
///
/// Examples:
/// - `src/api/grpc.rs` → `api::grpc`
/// - `src/lib.rs` → `` (crate root)
/// - `src/main.rs` → `` (crate root)
/// - `src/api/mod.rs` → `api`
/// - `crates/core/src/types.rs` → `types`
fn rust_module_path(file_path: &str) -> String {
    let path = file_path
        .replace('\\', "/")
        .trim_end_matches(".rs")
        .to_string();

    // Strip everything up to and including `src/`
    let after_src = if let Some(pos) = path.rfind("/src/") {
        &path[pos + 5..]
    } else if path.starts_with("src/") {
        &path[4..]
    } else {
        &path
    };

    // Handle special files
    match after_src {
        "lib" | "main" => String::new(),
        s if s.ends_with("/mod") => s.trim_end_matches("/mod").replace('/', "::"),
        s => s.replace('/', "::"),
    }
}

/// Convert a Python file path to a module path.
///
/// Examples:
/// - `scripts/run_manifests.py` → `scripts.run_manifests`
/// - `__init__.py` → `` (package root)
fn python_module_path(file_path: &str) -> String {
    let path = file_path
        .replace('\\', "/")
        .trim_end_matches(".py")
        .trim_end_matches(".pyi")
        .to_string();

    if path == "__init__" {
        String::new()
    } else if path.ends_with("/__init__") {
        path.trim_end_matches("/__init__").replace('/', ".")
    } else {
        path.replace('/', ".")
    }
}

/// Convert a TypeScript/JavaScript file path to a module path.
///
/// Examples:
/// - `src/components/Header.tsx` → `components/Header`
fn ts_module_path(file_path: &str) -> String {
    let path = file_path.replace('\\', "/");
    let stripped = path
        .trim_end_matches(".ts")
        .trim_end_matches(".tsx")
        .trim_end_matches(".js")
        .trim_end_matches(".jsx")
        .trim_end_matches(".mjs")
        .trim_end_matches(".cjs");

    // Strip src/ prefix
    if let Some(pos) = stripped.find("/src/") {
        stripped[pos + 5..].to_string()
    } else if stripped.starts_with("src/") {
        stripped[4..].to_string()
    } else {
        stripped.to_string()
    }
}

/// Extract the type name from an impl item name.
/// "impl Foo" → "Foo", "impl Trait for Foo" → "Foo"
fn extract_impl_type_name(impl_name: &str) -> String {
    if impl_name.contains(" for ") {
        impl_name
            .split(" for ")
            .last()
            .unwrap_or(impl_name)
            .to_string()
    } else {
        impl_name
            .strip_prefix("impl ")
            .unwrap_or(impl_name)
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_module_path() {
        assert_eq!(rust_module_path("src/api/grpc.rs"), "api::grpc");
        assert_eq!(rust_module_path("src/lib.rs"), "");
        assert_eq!(rust_module_path("src/main.rs"), "");
        assert_eq!(rust_module_path("src/api/mod.rs"), "api");
        assert_eq!(
            rust_module_path("src/models/user.rs"),
            "models::user"
        );
    }

    #[test]
    fn test_rust_module_path_nested_crate() {
        assert_eq!(
            rust_module_path("crates/core/src/types.rs"),
            "types"
        );
        assert_eq!(
            rust_module_path("crates/core/src/lib.rs"),
            ""
        );
    }

    #[test]
    fn test_python_module_path() {
        assert_eq!(
            python_module_path("scripts/run_manifests.py"),
            "scripts.run_manifests"
        );
        assert_eq!(python_module_path("__init__.py"), "");
        assert_eq!(
            python_module_path("src/utils/__init__.py"),
            "src.utils"
        );
    }

    #[test]
    fn test_ts_module_path() {
        assert_eq!(
            ts_module_path("src/components/Header.tsx"),
            "components/Header"
        );
    }

    #[test]
    fn test_extract_impl_type_name() {
        assert_eq!(extract_impl_type_name("impl Foo"), "Foo");
        assert_eq!(extract_impl_type_name("impl Display for Foo"), "Foo");
        assert_eq!(
            extract_impl_type_name("impl Iterator for MyIter"),
            "MyIter"
        );
    }

    #[test]
    fn test_qualified_name_computation() {
        use crate::inventory::Visibility;

        let mut items = vec![
            ExtractedItem {
                name: "Foo".to_string(),
                item_type: ItemType::Struct,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: None,
                line_start: 1,
                line_end: 3,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            },
            ExtractedItem {
                name: "impl Foo".to_string(),
                item_type: ItemType::Impl,
                visibility: Visibility::Private,
                signature: None,
                doc_comment: None,
                line_start: 5,
                line_end: 10,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![2],
                qualified_name: None,
            },
            ExtractedItem {
                name: "new".to_string(),
                item_type: ItemType::Function,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: None,
                line_start: 6,
                line_end: 8,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            },
        ];

        compute_qualified_names("src/models/user.rs", Language::Rust, &mut items);

        assert_eq!(items[0].qualified_name.as_deref(), Some("models::user::Foo"));
        assert_eq!(items[1].qualified_name.as_deref(), Some("models::user::Foo"));
        assert_eq!(items[2].qualified_name.as_deref(), Some("models::user::Foo::new"));
    }
}
