//! Item extraction via tree-sitter AST walking.
//!
//! Extracts functions, structs, enums, traits, impls, modules, constants,
//! use declarations, and other code items from parsed ASTs.

use crate::inventory::{ExtractedItem, ItemType, Visibility};
use crate::tree_sitter_validator::Language;
use tree_sitter::{Node, Tree};

/// Extract all code items from a parsed source file.
pub fn extract_items(source: &str, language: Language, tree: &Tree) -> Vec<ExtractedItem> {
    let root = tree.root_node();
    let source_bytes = source.as_bytes();
    let source_lines: Vec<&str> = source.lines().collect();
    let mut items = Vec::new();

    match language {
        Language::Rust => walk_rust(root, source_bytes, &source_lines, &mut items, None),
        Language::Python => walk_python(root, source_bytes, &source_lines, &mut items, None),
        Language::TypeScript | Language::JavaScript => {
            walk_typescript(root, source_bytes, &source_lines, &mut items, None)
        }
        Language::Go => walk_go(root, source_bytes, &source_lines, &mut items, None),
        Language::C => walk_c(root, source_bytes, &source_lines, &mut items, None),
    }

    items
}

// ---------------------------------------------------------------------------
// Rust item extraction
// ---------------------------------------------------------------------------

fn walk_rust(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    match node.kind() {
        "function_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);
            let attrs = extract_rust_attributes(node, source);
            let is_test = attrs.iter().any(|a| a.contains("test"));
            let is_async = node_text(node, source)
                .map(|t| t.starts_with("async ") || t.contains(" async "))
                .unwrap_or(false);
            let sig = extract_rust_fn_signature(node, source);
            let doc = extract_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Function,
                visibility: vis,
                signature: Some(sig),
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test,
                is_async,
                attributes: attrs,
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
            // Don't recurse into function body for top-level items
        }

        "struct_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);
            let doc = extract_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Struct,
                visibility: vis,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "enum_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);
            let doc = extract_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Enum,
                visibility: vis,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "trait_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);
            let doc = extract_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Trait,
                visibility: vis,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
            // Walk body for trait methods
            if let Some(body) = node.child_by_field_name("body") {
                walk_rust_children(body, source, source_lines, items, Some(idx));
            }
        }

        "impl_item" => {
            let type_name = field_text(node, "type", source).unwrap_or_default();
            let trait_name = field_text(node, "trait", source);
            let name = match trait_name {
                Some(ref t) => format!("impl {} for {}", t, type_name),
                None => format!("impl {}", type_name),
            };

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Impl,
                visibility: Visibility::Private, // impls don't have visibility
                signature: None,
                doc_comment: extract_doc_comment(source_lines, node.start_position().row),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
            // Walk body for methods
            if let Some(body) = node.child_by_field_name("body") {
                walk_rust_children(body, source, source_lines, items, Some(idx));
            }
        }

        "mod_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);
            let doc = extract_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Module,
                visibility: vis,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
            // Walk body for inline modules
            if let Some(body) = node.child_by_field_name("body") {
                walk_rust_children(body, source, source_lines, items, Some(idx));
            }
        }

        "const_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Constant,
                visibility: vis,
                signature: None,
                doc_comment: extract_doc_comment(source_lines, node.start_position().row),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "static_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Static,
                visibility: vis,
                signature: None,
                doc_comment: extract_doc_comment(source_lines, node.start_position().row),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "type_item" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::TypeAlias,
                visibility: vis,
                signature: None,
                doc_comment: extract_doc_comment(source_lines, node.start_position().row),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: extract_rust_attributes(node, source),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "macro_definition" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = extract_rust_visibility(node, source);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::MacroDefinition,
                visibility: vis,
                signature: None,
                doc_comment: extract_doc_comment(source_lines, node.start_position().row),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "use_declaration" => {
            let text = node_text(node, source).unwrap_or_default();

            let idx = items.len();
            items.push(ExtractedItem {
                name: text,
                item_type: ItemType::UseDeclaration,
                visibility: extract_rust_visibility(node, source),
                signature: None,
                doc_comment: None,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        // Recurse into containers that aren't items themselves
        _ => {
            walk_rust_children(node, source, source_lines, items, parent);
        }
    }
}

/// Walk all named children of a node for Rust items.
fn walk_rust_children(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_rust(child, source, source_lines, items, parent);
        }
    }
}

// ---------------------------------------------------------------------------
// Python item extraction
// ---------------------------------------------------------------------------

fn walk_python(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    match node.kind() {
        "function_definition" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let is_async = node_text(node, source)
                .map(|t| t.starts_with("async "))
                .unwrap_or(false);
            let sig = extract_python_fn_signature(node, source);
            let doc = extract_python_docstring(node, source);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Function,
                visibility: Visibility::Public, // Python has no enforced visibility
                signature: Some(sig),
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "class_definition" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let doc = extract_python_docstring(node, source);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Class,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
            // Walk body for methods
            if let Some(body) = node.child_by_field_name("body") {
                walk_python_children(body, source, source_lines, items, Some(idx));
            }
        }

        "decorated_definition" => {
            // Collect decorators, then process the inner definition
            let mut decorators = Vec::new();
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "decorator" {
                        if let Some(text) = node_text(child, source) {
                            decorators.push(text);
                        }
                    } else {
                        // Process the inner definition (function or class)
                        walk_python(child, source, source_lines, items, parent);
                        // Attach decorators to the last added item
                        if let Some(last) = items.last_mut() {
                            last.attributes = decorators.clone();
                            last.is_test = decorators.iter().any(|d| d.contains("test"));
                        }
                    }
                }
            }
        }

        "import_statement" | "import_from_statement" => {
            let text = node_text(node, source).unwrap_or_default();
            items.push(ExtractedItem {
                name: text,
                item_type: ItemType::UseDeclaration,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: None,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
        }

        _ => {
            walk_python_children(node, source, source_lines, items, parent);
        }
    }
}

fn walk_python_children(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_python(child, source, source_lines, items, parent);
        }
    }
}

// ---------------------------------------------------------------------------
// TypeScript item extraction
// ---------------------------------------------------------------------------

fn walk_typescript(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    match node.kind() {
        "function_declaration" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let doc = extract_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Function,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "class_declaration" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let doc = extract_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Class,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
            if let Some(body) = node.child_by_field_name("body") {
                walk_ts_children(body, source, source_lines, items, Some(idx));
            }
        }

        "interface_declaration" => {
            let name = field_text(node, "name", source).unwrap_or_default();

            items.push(ExtractedItem {
                name,
                item_type: ItemType::Interface,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: extract_doc_comment(source_lines, node.start_position().row),
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
        }

        "type_alias_declaration" => {
            let name = field_text(node, "name", source).unwrap_or_default();

            items.push(ExtractedItem {
                name,
                item_type: ItemType::TypeAlias,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: None,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
        }

        "lexical_declaration" => {
            // Check for arrow function assignments: const foo = () => {}
            for i in 0..node.named_child_count() {
                if let Some(decl) = node.named_child(i) {
                    if decl.kind() == "variable_declarator" {
                        let has_arrow = decl
                            .child_by_field_name("value")
                            .map(|v| v.kind() == "arrow_function")
                            .unwrap_or(false);
                        if has_arrow {
                            let name = field_text(decl, "name", source).unwrap_or_default();
                            items.push(ExtractedItem {
                                name,
                                item_type: ItemType::Function,
                                visibility: Visibility::Public,
                                signature: None,
                                doc_comment: extract_doc_comment(
                                    source_lines,
                                    node.start_position().row,
                                ),
                                line_start: node.start_position().row + 1,
                                line_end: node.end_position().row + 1,
                                is_test: false,
                                is_async: false,
                                attributes: vec![],
                                child_indices: vec![],
                                qualified_name: None,
                            });
                        }
                    }
                }
            }
        }

        "import_statement" => {
            let text = node_text(node, source).unwrap_or_default();
            items.push(ExtractedItem {
                name: text,
                item_type: ItemType::UseDeclaration,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: None,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
        }

        _ => {
            walk_ts_children(node, source, source_lines, items, parent);
        }
    }
}

fn walk_ts_children(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_typescript(child, source, source_lines, items, parent);
        }
    }
}

// ---------------------------------------------------------------------------
// Go item extraction
// ---------------------------------------------------------------------------

fn walk_go(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    match node.kind() {
        "function_declaration" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = go_visibility(&name);
            let sig = extract_go_fn_signature(node, source);
            let doc = extract_go_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Function,
                visibility: vis,
                signature: Some(sig),
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "method_declaration" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = go_visibility(&name);
            let sig = extract_go_fn_signature(node, source);
            let doc = extract_go_doc_comment(source_lines, node.start_position().row);

            // Extract receiver type for context
            let receiver = node
                .child_by_field_name("receiver")
                .and_then(|r| r.named_child(0)) // parameter_declaration inside parameter_list
                .and_then(|pd| pd.child_by_field_name("type"))
                .and_then(|t| t.utf8_text(source).ok())
                .map(|s| s.trim_start_matches('*').to_string());

            let is_test = name.starts_with("Test")
                || name.starts_with("Benchmark")
                || name.starts_with("Example");

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Function,
                visibility: vis,
                signature: Some(sig),
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test,
                is_async: false,
                attributes: receiver.into_iter().collect(),
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "type_declaration" => {
            // type_declaration contains type_spec children
            walk_go_children(node, source, source_lines, items, parent);
        }

        "type_spec" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            let vis = go_visibility(&name);
            let doc = extract_go_doc_comment(source_lines, node.start_position().row);

            // Determine item type from the type field
            let type_node = node.child_by_field_name("type");
            let item_type = match type_node.map(|n| n.kind()) {
                Some("struct_type") => ItemType::Struct,
                Some("interface_type") => ItemType::Trait, // Go interfaces map to Trait
                _ => ItemType::TypeAlias,
            };

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type,
                visibility: vis,
                signature: None,
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "const_declaration" => {
            // Walk const_spec children
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "const_spec" {
                        let name = field_text(child, "name", source).unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }
                        let vis = go_visibility(&name);
                        let doc = extract_go_doc_comment(source_lines, node.start_position().row);

                        let idx = items.len();
                        items.push(ExtractedItem {
                            name,
                            item_type: ItemType::Constant,
                            visibility: vis,
                            signature: None,
                            doc_comment: doc,
                            line_start: child.start_position().row + 1,
                            line_end: child.end_position().row + 1,
                            is_test: false,
                            is_async: false,
                            attributes: vec![],
                            child_indices: vec![],
                            qualified_name: None,
                        });
                        if let Some(p) = parent {
                            items[p].child_indices.push(idx);
                        }
                    }
                }
            }
        }

        "var_declaration" => {
            // Walk var_spec children
            for i in 0..node.named_child_count() {
                if let Some(child) = node.named_child(i) {
                    if child.kind() == "var_spec" {
                        let name = field_text(child, "name", source).unwrap_or_default();
                        if name.is_empty() {
                            continue;
                        }
                        let vis = go_visibility(&name);

                        let idx = items.len();
                        items.push(ExtractedItem {
                            name,
                            item_type: ItemType::Static, // package-level vars are like statics
                            visibility: vis,
                            signature: None,
                            doc_comment: None,
                            line_start: child.start_position().row + 1,
                            line_end: child.end_position().row + 1,
                            is_test: false,
                            is_async: false,
                            attributes: vec![],
                            child_indices: vec![],
                            qualified_name: None,
                        });
                        if let Some(p) = parent {
                            items[p].child_indices.push(idx);
                        }
                    }
                }
            }
        }

        "import_declaration" => {
            let text = node_text(node, source).unwrap_or_default();
            items.push(ExtractedItem {
                name: text,
                item_type: ItemType::UseDeclaration,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: None,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
        }

        _ => {
            walk_go_children(node, source, source_lines, items, parent);
        }
    }
}

fn walk_go_children(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_go(child, source, source_lines, items, parent);
        }
    }
}

/// Go visibility: uppercase first letter = public, lowercase = private.
fn go_visibility(name: &str) -> Visibility {
    if name.starts_with(|c: char| c.is_uppercase()) {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

/// Extract Go function signature (everything before the body block).
fn extract_go_fn_signature(node: Node, source: &[u8]) -> String {
    if let Some(body) = node.child_by_field_name("body") {
        let sig_start = node.start_byte();
        let sig_end = body.start_byte();
        std::str::from_utf8(&source[sig_start..sig_end])
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        node_text(node, source).unwrap_or_default()
    }
}

/// Extract Go doc comment (// lines immediately preceding an item).
fn extract_go_doc_comment(source_lines: &[&str], item_row_0idx: usize) -> Option<String> {
    let mut comments = Vec::new();
    let mut line = item_row_0idx;

    while line > 0 {
        line -= 1;
        let trimmed = source_lines.get(line).map(|l| l.trim()).unwrap_or("");
        if trimmed.starts_with("//") {
            comments.push(trimmed[2..].trim().to_string());
        } else if trimmed.is_empty() {
            break; // Go doc comments must be contiguous
        } else {
            break;
        }
    }

    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// C item extraction
// ---------------------------------------------------------------------------

fn walk_c(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    match node.kind() {
        "function_definition" => {
            let name = extract_c_function_name(node, source).unwrap_or_default();
            if name.is_empty() {
                walk_c_children(node, source, source_lines, items, parent);
                return;
            }
            let vis = extract_c_visibility(node, source);
            let sig = extract_c_fn_signature(node, source);
            let doc = extract_c_doc_comment(source_lines, node.start_position().row);

            let idx = items.len();
            items.push(ExtractedItem {
                name,
                item_type: ItemType::Function,
                visibility: vis,
                signature: Some(sig),
                doc_comment: doc,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
            if let Some(p) = parent {
                items[p].child_indices.push(idx);
            }
        }

        "declaration" => {
            // Check if this is a forward declaration of a function
            let has_func_declarator = find_child_of_kind(node, "function_declarator").is_some();
            if has_func_declarator {
                // Forward declaration — extract as function declaration
                let name = extract_c_decl_function_name(node, source).unwrap_or_default();
                if !name.is_empty() {
                    let vis = extract_c_visibility(node, source);
                    let sig = node_text(node, source).unwrap_or_default();

                    let idx = items.len();
                    items.push(ExtractedItem {
                        name,
                        item_type: ItemType::Function,
                        visibility: vis,
                        signature: Some(sig.trim_end_matches(';').trim().to_string()),
                        doc_comment: extract_c_doc_comment(
                            source_lines,
                            node.start_position().row,
                        ),
                        line_start: node.start_position().row + 1,
                        line_end: node.end_position().row + 1,
                        is_test: false,
                        is_async: false,
                        attributes: vec![],
                        child_indices: vec![],
                        qualified_name: None,
                    });
                    if let Some(p) = parent {
                        items[p].child_indices.push(idx);
                    }
                }
            }
            // Otherwise skip regular variable declarations at file scope
        }

        "type_definition" => {
            // typedef struct { ... } Name;
            // Extract the typedef name from the declarator
            let name = field_text(node, "declarator", source).unwrap_or_default();
            let type_node = node.child_by_field_name("type");
            let item_type = match type_node.map(|n| n.kind()) {
                Some("struct_specifier") => ItemType::Struct,
                Some("enum_specifier") => ItemType::Enum,
                _ => ItemType::TypeAlias,
            };

            if !name.is_empty() {
                let doc = extract_c_doc_comment(source_lines, node.start_position().row);
                let idx = items.len();
                items.push(ExtractedItem {
                    name,
                    item_type,
                    visibility: Visibility::Public,
                    signature: None,
                    doc_comment: doc,
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    is_test: false,
                    is_async: false,
                    attributes: vec![],
                    child_indices: vec![],
                    qualified_name: None,
                });
                if let Some(p) = parent {
                    items[p].child_indices.push(idx);
                }
            }
        }

        "struct_specifier" => {
            // Named struct: struct Foo { ... }; (not inside a typedef)
            let name = field_text(node, "name", source).unwrap_or_default();
            if !name.is_empty() && node.child_by_field_name("body").is_some() {
                let doc = extract_c_doc_comment(source_lines, node.start_position().row);
                let idx = items.len();
                items.push(ExtractedItem {
                    name,
                    item_type: ItemType::Struct,
                    visibility: Visibility::Public,
                    signature: None,
                    doc_comment: doc,
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    is_test: false,
                    is_async: false,
                    attributes: vec![],
                    child_indices: vec![],
                    qualified_name: None,
                });
                if let Some(p) = parent {
                    items[p].child_indices.push(idx);
                }
            }
        }

        "enum_specifier" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            if !name.is_empty() {
                let doc = extract_c_doc_comment(source_lines, node.start_position().row);
                let idx = items.len();
                items.push(ExtractedItem {
                    name,
                    item_type: ItemType::Enum,
                    visibility: Visibility::Public,
                    signature: None,
                    doc_comment: doc,
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    is_test: false,
                    is_async: false,
                    attributes: vec![],
                    child_indices: vec![],
                    qualified_name: None,
                });
                if let Some(p) = parent {
                    items[p].child_indices.push(idx);
                }
            }
        }

        "preproc_include" => {
            let text = node_text(node, source).unwrap_or_default();
            items.push(ExtractedItem {
                name: text,
                item_type: ItemType::UseDeclaration,
                visibility: Visibility::Public,
                signature: None,
                doc_comment: None,
                line_start: node.start_position().row + 1,
                line_end: node.end_position().row + 1,
                is_test: false,
                is_async: false,
                attributes: vec![],
                child_indices: vec![],
                qualified_name: None,
            });
        }

        "preproc_def" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            if !name.is_empty() {
                items.push(ExtractedItem {
                    name,
                    item_type: ItemType::Constant,
                    visibility: Visibility::Public,
                    signature: None,
                    doc_comment: None,
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    is_test: false,
                    is_async: false,
                    attributes: vec![],
                    child_indices: vec![],
                    qualified_name: None,
                });
            }
        }

        "preproc_function_def" => {
            let name = field_text(node, "name", source).unwrap_or_default();
            if !name.is_empty() {
                let sig = node_text(node, source).unwrap_or_default();
                items.push(ExtractedItem {
                    name,
                    item_type: ItemType::MacroDefinition,
                    visibility: Visibility::Public,
                    signature: Some(sig),
                    doc_comment: None,
                    line_start: node.start_position().row + 1,
                    line_end: node.end_position().row + 1,
                    is_test: false,
                    is_async: false,
                    attributes: vec![],
                    child_indices: vec![],
                    qualified_name: None,
                });
            }
        }

        _ => {
            walk_c_children(node, source, source_lines, items, parent);
        }
    }
}

fn walk_c_children(
    node: Node,
    source: &[u8],
    source_lines: &[&str],
    items: &mut Vec<ExtractedItem>,
    parent: Option<usize>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_c(child, source, source_lines, items, parent);
        }
    }
}

/// Extract function name from a C function_definition node.
/// The name is inside: function_definition > declarator (function_declarator) > declarator (identifier).
fn extract_c_function_name(node: Node, source: &[u8]) -> Option<String> {
    let declarator = node.child_by_field_name("declarator")?;
    extract_c_name_from_declarator(declarator, source)
}

/// Extract function name from a C declaration node (forward declaration).
fn extract_c_decl_function_name(node: Node, source: &[u8]) -> Option<String> {
    let declarator = node.child_by_field_name("declarator")?;
    // For init_declarator, get the inner declarator
    let actual = if declarator.kind() == "init_declarator" {
        declarator.child_by_field_name("declarator")?
    } else {
        declarator
    };
    extract_c_name_from_declarator(actual, source)
}

/// Recursively descend through declarator nodes to find the identifier name.
fn extract_c_name_from_declarator(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "function_declarator" | "pointer_declarator" | "parenthesized_declarator" => {
            let inner = node.child_by_field_name("declarator")?;
            extract_c_name_from_declarator(inner, source)
        }
        _ => node.child_by_field_name("declarator")
            .and_then(|d| extract_c_name_from_declarator(d, source)),
    }
}

/// Find a named child of a specific kind (recursive, shallow).
fn find_child_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            if child.kind() == kind {
                return Some(child);
            }
            // Check one level deeper for nested declarators
            if let Some(found) = find_child_of_kind(child, kind) {
                return Some(found);
            }
        }
    }
    None
}

/// C visibility: static = private, everything else = public.
fn extract_c_visibility(node: Node, source: &[u8]) -> Visibility {
    let text = node_text(node, source).unwrap_or_default();
    if text.starts_with("static ") || text.contains(" static ") {
        Visibility::Private
    } else {
        Visibility::Public
    }
}

/// Extract C function signature (everything before the body).
fn extract_c_fn_signature(node: Node, source: &[u8]) -> String {
    if let Some(body) = node.child_by_field_name("body") {
        let sig_start = node.start_byte();
        let sig_end = body.start_byte();
        std::str::from_utf8(&source[sig_start..sig_end])
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        node_text(node, source).unwrap_or_default()
    }
}

/// Extract C doc comment (// or /* */ lines preceding an item).
fn extract_c_doc_comment(source_lines: &[&str], item_row_0idx: usize) -> Option<String> {
    let mut comments = Vec::new();
    let mut line = item_row_0idx;

    while line > 0 {
        line -= 1;
        let trimmed = source_lines.get(line).map(|l| l.trim()).unwrap_or("");
        if trimmed.starts_with("//") {
            comments.push(trimmed[2..].trim().to_string());
        } else if trimmed.starts_with("/*") || trimmed.starts_with("*") || trimmed.starts_with("*/")
        {
            // Block comment line
            let cleaned = trimmed
                .trim_start_matches("/*")
                .trim_start_matches("*/")
                .trim_start_matches('*')
                .trim();
            if !cleaned.is_empty() {
                comments.push(cleaned.to_string());
            }
        } else if trimmed.is_empty() {
            break;
        } else {
            break;
        }
    }

    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join("\n"))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Get text of a named field child.
fn field_text(node: Node, field: &str, source: &[u8]) -> Option<String> {
    node.child_by_field_name(field)
        .and_then(|n| n.utf8_text(source).ok())
        .map(|s| s.to_string())
}

/// Get text of a node.
fn node_text(node: Node, source: &[u8]) -> Option<String> {
    node.utf8_text(source).ok().map(|s| s.to_string())
}

/// Extract Rust visibility modifier from item node.
fn extract_rust_visibility(node: Node, source: &[u8]) -> Visibility {
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i) {
            if child.kind() == "visibility_modifier" {
                let text = child.utf8_text(source).unwrap_or("");
                return match text {
                    "pub" => Visibility::Public,
                    "pub(crate)" => Visibility::PublicCrate,
                    "pub(super)" => Visibility::PublicSuper,
                    s if s.starts_with("pub(in ") => Visibility::PublicIn(s.to_string()),
                    _ => Visibility::Public,
                };
            }
        }
    }
    Visibility::Private
}

/// Extract Rust attributes from preceding sibling nodes.
fn extract_rust_attributes(node: Node, source: &[u8]) -> Vec<String> {
    let mut attrs = Vec::new();
    let mut current = node;
    while let Some(prev) = current.prev_named_sibling() {
        if prev.kind() == "attribute_item" {
            if let Ok(text) = prev.utf8_text(source) {
                attrs.push(text.to_string());
            }
            current = prev;
        } else {
            break;
        }
    }
    attrs.reverse();
    attrs
}

/// Extract function signature (everything before the body).
fn extract_rust_fn_signature(node: Node, source: &[u8]) -> String {
    if let Some(body) = node.child_by_field_name("body") {
        let sig_start = node.start_byte();
        let sig_end = body.start_byte();
        std::str::from_utf8(&source[sig_start..sig_end])
            .unwrap_or("")
            .trim()
            .to_string()
    } else {
        // Trait method declaration (no body)
        node_text(node, source).unwrap_or_default()
    }
}

/// Extract doc comment (/// lines) from source lines before an item.
fn extract_doc_comment(source_lines: &[&str], item_row_0idx: usize) -> Option<String> {
    let mut comments = Vec::new();
    let mut line = item_row_0idx;

    while line > 0 {
        line -= 1;
        let trimmed = source_lines.get(line).map(|l| l.trim()).unwrap_or("");
        if trimmed.starts_with("///") {
            comments.push(trimmed[3..].trim().to_string());
        } else if trimmed.starts_with("#[") || trimmed.is_empty() {
            continue; // skip attributes and blank lines between doc and item
        } else {
            break;
        }
    }

    if comments.is_empty() {
        None
    } else {
        comments.reverse();
        Some(comments.join("\n"))
    }
}

/// Extract Python docstring from the first expression statement in a body.
fn extract_python_docstring(node: Node, source: &[u8]) -> Option<String> {
    let body = node.child_by_field_name("body")?;
    let first = body.named_child(0)?;
    if first.kind() == "expression_statement" {
        let expr = first.named_child(0)?;
        if expr.kind() == "string" {
            let text = expr.utf8_text(source).ok()?;
            // Strip triple quotes
            let stripped = text
                .trim_start_matches("\"\"\"")
                .trim_start_matches("'''")
                .trim_end_matches("\"\"\"")
                .trim_end_matches("'''")
                .trim();
            return Some(stripped.to_string());
        }
    }
    None
}

/// Extract Python function signature.
fn extract_python_fn_signature(node: Node, source: &[u8]) -> String {
    if let Some(body) = node.child_by_field_name("body") {
        let sig_start = node.start_byte();
        let sig_end = body.start_byte();
        let sig = std::str::from_utf8(&source[sig_start..sig_end]).unwrap_or("");
        sig.trim().trim_end_matches(':').trim().to_string()
    } else {
        node_text(node, source).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_rust(source: &str) -> (Tree, tree_sitter::Parser) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        (tree, parser)
    }

    fn parse_python(source: &str) -> (Tree, tree_sitter::Parser) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        (tree, parser)
    }

    #[test]
    fn test_extract_rust_function() {
        let source = r#"pub fn hello(name: &str) -> String {
    format!("Hello, {}!", name)
}"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "hello");
        assert_eq!(items[0].item_type, ItemType::Function);
        assert_eq!(items[0].visibility, Visibility::Public);
        assert!(!items[0].is_async);
        assert!(!items[0].is_test);
    }

    #[test]
    fn test_extract_rust_struct_and_impl() {
        let source = r#"
pub struct Foo {
    x: i32,
}

impl Foo {
    pub fn new(x: i32) -> Self {
        Self { x }
    }

    fn private_method(&self) -> i32 {
        self.x
    }
}
"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        // Should have: struct Foo, impl Foo, new, private_method
        let structs: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Struct).collect();
        let impls: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Impl).collect();
        let fns: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Function).collect();

        assert_eq!(structs.len(), 1);
        assert_eq!(structs[0].name, "Foo");

        assert_eq!(impls.len(), 1);
        assert!(impls[0].name.contains("Foo"));

        assert_eq!(fns.len(), 2);
        assert!(fns.iter().any(|f| f.name == "new" && f.visibility == Visibility::Public));
        assert!(fns.iter().any(|f| f.name == "private_method" && f.visibility == Visibility::Private));
    }

    #[test]
    fn test_extract_rust_enum_and_trait() {
        let source = r#"
pub enum Color {
    Red,
    Green,
    Blue,
}

pub trait Paintable {
    fn paint(&self, color: Color);
}
"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        let enums: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Enum).collect();
        let traits: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Trait).collect();

        assert_eq!(enums.len(), 1);
        assert_eq!(enums[0].name, "Color");

        assert_eq!(traits.len(), 1);
        assert_eq!(traits[0].name, "Paintable");
    }

    #[test]
    fn test_extract_rust_use_declarations() {
        let source = r#"
use std::path::Path;
use crate::compliance::ComplianceReport;
"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        let uses: Vec<_> = items
            .iter()
            .filter(|i| i.item_type == ItemType::UseDeclaration)
            .collect();

        assert_eq!(uses.len(), 2);
        assert!(uses[0].name.contains("std::path::Path"));
    }

    #[test]
    fn test_extract_rust_test_function() {
        let source = r#"
#[test]
fn test_something() {
    assert!(true);
}
"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        let fns: Vec<_> = items
            .iter()
            .filter(|i| i.item_type == ItemType::Function)
            .collect();

        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "test_something");
        assert!(fns[0].is_test);
    }

    #[test]
    fn test_extract_python_function() {
        let source = r#"def greet(name):
    """Say hello."""
    print(f"Hello, {name}!")
"#;
        let (tree, _) = parse_python(source);
        let items = extract_items(source, Language::Python, &tree);

        let fns: Vec<_> = items
            .iter()
            .filter(|i| i.item_type == ItemType::Function)
            .collect();

        assert_eq!(fns.len(), 1);
        assert_eq!(fns[0].name, "greet");
        assert!(fns[0].doc_comment.as_ref().unwrap().contains("Say hello"));
    }

    #[test]
    fn test_extract_python_class() {
        let source = r#"
class Calculator:
    """A simple calculator."""

    def add(self, a, b):
        return a + b

    def subtract(self, a, b):
        return a - b
"#;
        let (tree, _) = parse_python(source);
        let items = extract_items(source, Language::Python, &tree);

        let classes: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Class).collect();
        let fns: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Function).collect();

        assert_eq!(classes.len(), 1);
        assert_eq!(classes[0].name, "Calculator");
        assert_eq!(fns.len(), 2);
    }

    #[test]
    fn test_extract_rust_doc_comment() {
        let source = r#"/// This is a documented function.
/// It does something important.
pub fn documented() {}"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        assert_eq!(items.len(), 1);
        let doc = items[0].doc_comment.as_ref().unwrap();
        assert!(doc.contains("documented function"));
        assert!(doc.contains("something important"));
    }

    #[test]
    fn test_extract_async_function() {
        let source = r#"pub async fn fetch_data() -> Result<Data, Error> {
    Ok(Data::new())
}"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        assert_eq!(items.len(), 1);
        assert!(items[0].is_async);
    }

    #[test]
    fn test_impl_children_tracked() {
        let source = r#"
impl Foo {
    fn bar(&self) {}
    fn baz(&self) {}
}
"#;
        let (tree, _) = parse_rust(source);
        let items = extract_items(source, Language::Rust, &tree);

        let impls: Vec<_> = items.iter().filter(|i| i.item_type == ItemType::Impl).collect();
        assert_eq!(impls.len(), 1);
        assert_eq!(impls[0].child_indices.len(), 2);
    }
}
