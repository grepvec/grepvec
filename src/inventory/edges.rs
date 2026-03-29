//! Edge detection via tree-sitter AST walking.
//!
//! Detects call expressions, use/import declarations, macro invocations,
//! and trait implementations to build the call graph and dependency edges.

use crate::inventory::{EdgeType, ExtractedEdge, ExtractedItem, ItemType};
use crate::tree_sitter_validator::Language;
use tree_sitter::{Node, Tree};

/// Extract edges (calls, imports, implements) from a parsed source file.
/// Filters out noise edges (stdlib/utility calls) to keep only meaningful relationships.
pub fn extract_edges(
    source: &str,
    language: Language,
    tree: &Tree,
    items: &[ExtractedItem],
) -> Vec<ExtractedEdge> {
    let root = tree.root_node();
    let source_bytes = source.as_bytes();
    let mut edges = Vec::new();

    // Build contains edges from parent-child relationships
    for item in items.iter() {
        for &child_idx in &item.child_indices {
            if let Some(child) = items.get(child_idx) {
                edges.push(ExtractedEdge {
                    source_item_name: item.name.clone(),
                    edge_type: EdgeType::Contains,
                    target_name: child.name.clone(),
                    line: child.line_start,
                });
            }
        }

        // Detect implements edges from impl items
        if item.item_type == ItemType::Impl && item.name.contains(" for ") {
            // "impl Trait for Type" → implements edge
            let parts: Vec<&str> = item.name.splitn(4, ' ').collect();
            if parts.len() >= 4 {
                let trait_name = parts[1];
                let type_name = parts[3];
                edges.push(ExtractedEdge {
                    source_item_name: type_name.to_string(),
                    edge_type: EdgeType::Implements,
                    target_name: trait_name.to_string(),
                    line: item.line_start,
                });
            }
        }
    }

    // Walk AST for call expressions and imports
    match language {
        Language::Rust => walk_rust_edges(root, source_bytes, items, &mut edges),
        Language::Python => walk_python_edges(root, source_bytes, items, &mut edges),
        Language::TypeScript | Language::JavaScript => {
            walk_ts_edges(root, source_bytes, items, &mut edges)
        }
        Language::Go => walk_go_edges(root, source_bytes, items, &mut edges),
        Language::C => walk_c_edges(root, source_bytes, items, &mut edges),
    }

    // Filter out noise edges
    edges.retain(|e| !is_noise_edge(e));

    // Detect type-reference edges: functions that use structs/enums/traits
    // in their signatures or field types
    detect_type_edges(source, items, &mut edges);

    edges
}

/// Detect edges from functions to the types they reference in signatures,
/// and from structs to the types they use in field declarations.
fn detect_type_edges(
    source: &str,
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    // Collect all type names (structs, enums, traits) as targets
    let type_names: std::collections::HashSet<&str> = items.iter()
        .filter(|i| matches!(i.item_type, ItemType::Struct | ItemType::Enum | ItemType::Trait))
        .map(|i| i.name.as_str())
        .collect();

    if type_names.is_empty() { return; }

    let source_lines: Vec<&str> = source.lines().collect();

    for item in items {
        match item.item_type {
            ItemType::Function => {
                // Check function signature for type references
                if let Some(ref sig) = item.signature {
                    for type_name in &type_names {
                        // Match whole word: the type name must be preceded and followed
                        // by non-alphanumeric characters (or be at string boundaries)
                        if contains_type_ref(sig, type_name) {
                            edges.push(ExtractedEdge {
                                source_item_name: item.name.clone(),
                                edge_type: EdgeType::UsesType,
                                target_name: type_name.to_string(),
                                line: item.line_start,
                            });
                        }
                    }
                }
            }
            ItemType::Struct => {
                // Check struct fields for type references to other structs/enums
                let start = item.line_start.saturating_sub(1);
                let end = item.line_end.min(source_lines.len());
                for line_idx in start..end {
                    if let Some(line) = source_lines.get(line_idx) {
                        for type_name in &type_names {
                            if *type_name != item.name && contains_type_ref(line, type_name) {
                                edges.push(ExtractedEdge {
                                    source_item_name: item.name.clone(),
                                    edge_type: EdgeType::UsesType,
                                    target_name: type_name.to_string(),
                                    line: line_idx + 1,
                                });
                                break; // one edge per type per struct
                            }
                        }
                    }
                }
            }
            ItemType::Impl => {
                // Check which type this impl is for
                // "impl Foo" or "impl Trait for Foo" — the type name is in the item name
                for type_name in &type_names {
                    if item.name.contains(type_name) && item.name != *type_name {
                        edges.push(ExtractedEdge {
                            source_item_name: item.name.clone(),
                            edge_type: EdgeType::UsesType,
                            target_name: type_name.to_string(),
                            line: item.line_start,
                        });
                    }
                }
            }
            _ => {}
        }
    }
}

/// Check if a string contains a type reference (whole word match).
fn contains_type_ref(text: &str, type_name: &str) -> bool {
    if type_name.len() < 2 { return false; }

    let mut search_from = 0;
    while let Some(pos) = text[search_from..].find(type_name) {
        let abs_pos = search_from + pos;
        let before_ok = abs_pos == 0 ||
            !text.as_bytes()[abs_pos - 1].is_ascii_alphanumeric();
        let after_pos = abs_pos + type_name.len();
        let after_ok = after_pos >= text.len() ||
            !text.as_bytes()[after_pos].is_ascii_alphanumeric();

        if before_ok && after_ok {
            return true;
        }
        search_from = abs_pos + 1;
        if search_from >= text.len() { break; }
    }
    false
}

/// Returns true if this edge is noise (stdlib/utility call) that would
/// pollute biographies with useless information.
fn is_noise_edge(edge: &ExtractedEdge) -> bool {
    if edge.edge_type != EdgeType::Calls && edge.edge_type != EdgeType::MacroInvocation {
        return false; // never filter Contains, Implements, ExternalDep, Imports
    }

    let name = edge.target_name.as_str();

    // For qualified calls like "Type::method", extract the method name
    let short_name = name.rsplit("::").next().unwrap_or(name);

    // Macro noise
    if edge.edge_type == EdgeType::MacroInvocation {
        return NOISE_MACROS.contains(&short_name);
    }

    // Single-word stdlib/utility methods
    if NOISE_CALLS.contains(&short_name) {
        return true;
    }

    // Method calls on external types (short lowercase names that aren't in our items)
    // These are calls like timer.elapsed(), client.send(), builder.build()
    // that will never resolve to our inventory
    EXTERNAL_METHOD_NOISE.contains(&short_name)
}

/// Stdlib/utility function calls that add no documentation value.
const NOISE_CALLS: &[&str] = &[
    // Result/Option chaining
    "unwrap", "unwrap_or", "unwrap_or_else", "unwrap_or_default",
    "expect", "ok", "err", "is_ok", "is_err", "is_some", "is_none",
    "map", "map_err", "map_or", "map_or_else",
    "and_then", "or_else", "or", "and",
    "ok_or", "ok_or_else", "transpose",
    // Constructors/conversions that are universal
    "Ok", "Err", "Some", "None",
    "into", "from", "try_into", "try_from",
    "clone", "to_string", "to_owned", "as_ref", "as_mut",
    "default", "new", // too common to be meaningful alone
    // Iterator adapters
    "iter", "into_iter", "iter_mut",
    "collect", "filter", "flat_map", "fold",
    "any", "all", "find", "position",
    "enumerate", "zip", "chain", "take", "skip",
    "peekable", "fuse", "rev",
    "for_each", "count", "sum", "product",
    "min", "max", "min_by", "max_by",
    "next", "last", "nth",
    // Container ops
    "len", "is_empty", "push", "pop",
    "insert", "remove", "get", "get_mut",
    "contains", "contains_key",
    "entry", "or_insert", "or_insert_with", "or_default",
    "keys", "values", "drain", "clear", "extend", "retain",
    // String ops
    "trim", "trim_start", "trim_end",
    "starts_with", "ends_with",
    "to_lowercase", "to_uppercase",
    "replace", "split", "splitn", "join",
    "as_str", "as_bytes", "chars", "lines",
    // Formatting/display
    "fmt", "display", "to_string_lossy",
    "write", "write_all", "flush",
    // Pointer/ref
    "deref", "borrow", "borrow_mut",
    "lock", "read", "write",
    // Async
    "await", "poll", "spawn",
    // Type assertions
    "downcast", "downcast_ref", "type_id",
];

/// Method calls on external types that will never resolve.
const EXTERNAL_METHOD_NOISE: &[&str] = &[
    // Time/Duration
    "elapsed", "as_secs", "as_millis", "as_nanos", "as_secs_f64",
    // HTTP/Network
    "send", "header", "headers", "json", "body", "status",
    "bind", "listen", "accept", "connect", "close", "shutdown",
    "post", "get", "put", "delete", "patch",
    // Builder pattern
    "build", "builder", "finish", "finalize", "done",
    "set", "with", "add", "configure",
    // Serialization
    "serialize", "deserialize",
    // Database/query
    "execute", "fetch", "fetch_one", "fetch_all", "fetch_optional",
    "query", "prepare",
    // Tonic/gRPC
    "into_inner", "into_request", "metadata", "metadata_mut",
    "max_encoding_message_size", "max_decoding_message_size",
    // Channels
    "send", "recv", "try_recv", "try_send",
    // Tracing
    "instrument", "in_scope",
    // Misc external type methods
    "copied", "cloned", "as_deref", "as_ref",
    "first", "last", "is_empty",
    "to_str", "to_string_lossy", "display",
    "exists", "is_dir", "is_file",
    "inner", "inner_mut",
];

/// Macros that are noise in call graphs.
const NOISE_MACROS: &[&str] = &[
    "println", "eprintln", "print", "eprint",
    "format", "write", "writeln",
    "vec", "dbg",
    "todo", "unimplemented", "unreachable",
    "panic", "assert", "assert_eq", "assert_ne",
    "debug_assert", "debug_assert_eq", "debug_assert_ne",
    "cfg", "cfg_attr",
    "include", "include_str", "include_bytes",
    "env", "option_env",
    "concat", "stringify",
    "log", "trace", "debug", "info", "warn", "error",
];

// ---------------------------------------------------------------------------
// Rust edge detection
// ---------------------------------------------------------------------------

fn walk_rust_edges(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(callee) = extract_rust_callee(node, source) {
                let line = node.start_position().row + 1;
                let caller = find_enclosing_item(items, line);
                edges.push(ExtractedEdge {
                    source_item_name: caller.unwrap_or_default(),
                    edge_type: EdgeType::Calls,
                    target_name: callee,
                    line,
                });
            }
            // Still recurse into children (arguments may contain calls)
            walk_rust_edge_children(node, source, items, edges);
        }

        "macro_invocation" => {
            if let Some(macro_node) = node.child_by_field_name("macro") {
                if let Ok(name) = macro_node.utf8_text(source) {
                    let line = node.start_position().row + 1;
                    let caller = find_enclosing_item(items, line);
                    edges.push(ExtractedEdge {
                        source_item_name: caller.unwrap_or_default(),
                        edge_type: EdgeType::MacroInvocation,
                        target_name: name.to_string(),
                        line,
                    });
                }
            }
            walk_rust_edge_children(node, source, items, edges);
        }

        _ => {
            walk_rust_edge_children(node, source, items, edges);
        }
    }
}

fn walk_rust_edge_children(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_rust_edges(child, source, items, edges);
        }
    }
}

/// Extract the callee name from a Rust call expression.
fn extract_rust_callee(node: Node, source: &[u8]) -> Option<String> {
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        // Direct call: foo()
        "identifier" => func.utf8_text(source).ok().map(|s| s.to_string()),

        // Method call: self.method() or obj.method()
        "field_expression" => {
            let field = func.child_by_field_name("field")?;
            field.utf8_text(source).ok().map(|s| s.to_string())
        }

        // Qualified call: Module::function()
        "scoped_identifier" => func.utf8_text(source).ok().map(|s| s.to_string()),

        // Other (e.g., closure calls)
        _ => func.utf8_text(source).ok().map(|s| s.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Python edge detection
// ---------------------------------------------------------------------------

fn walk_python_edges(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    match node.kind() {
        "call" => {
            if let Some(func) = node.child_by_field_name("function") {
                let callee = extract_python_callee(func, source);
                if let Some(callee) = callee {
                    let line = node.start_position().row + 1;
                    let caller = find_enclosing_item(items, line);
                    edges.push(ExtractedEdge {
                        source_item_name: caller.unwrap_or_default(),
                        edge_type: EdgeType::Calls,
                        target_name: callee,
                        line,
                    });
                }
            }
            walk_python_edge_children(node, source, items, edges);
        }

        _ => {
            walk_python_edge_children(node, source, items, edges);
        }
    }
}

fn walk_python_edge_children(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_python_edges(child, source, items, edges);
        }
    }
}

fn extract_python_callee(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        "identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),
        "attribute" => node.utf8_text(source).ok().map(|s| s.to_string()),
        _ => node.utf8_text(source).ok().map(|s| s.to_string()),
    }
}

// ---------------------------------------------------------------------------
// TypeScript edge detection
// ---------------------------------------------------------------------------

fn walk_ts_edges(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let callee = func.utf8_text(source).ok().map(|s| s.to_string());
                if let Some(callee) = callee {
                    let line = node.start_position().row + 1;
                    let caller = find_enclosing_item(items, line);
                    edges.push(ExtractedEdge {
                        source_item_name: caller.unwrap_or_default(),
                        edge_type: EdgeType::Calls,
                        target_name: callee,
                        line,
                    });
                }
            }
            walk_ts_edge_children(node, source, items, edges);
        }

        _ => {
            walk_ts_edge_children(node, source, items, edges);
        }
    }
}

fn walk_ts_edge_children(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_ts_edges(child, source, items, edges);
        }
    }
}

// ---------------------------------------------------------------------------
// Go edge detection
// ---------------------------------------------------------------------------

fn walk_go_edges(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let callee = extract_go_callee(func, source);
                if let Some(callee) = callee {
                    let line = node.start_position().row + 1;
                    let caller = find_enclosing_item(items, line);
                    edges.push(ExtractedEdge {
                        source_item_name: caller.unwrap_or_default(),
                        edge_type: EdgeType::Calls,
                        target_name: callee,
                        line,
                    });
                }
            }
            walk_go_edge_children(node, source, items, edges);
        }

        _ => {
            walk_go_edge_children(node, source, items, edges);
        }
    }
}

fn walk_go_edge_children(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_go_edges(child, source, items, edges);
        }
    }
}

/// Extract callee name from a Go call expression's function node.
fn extract_go_callee(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        // Direct call: foo()
        "identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),

        // Method call or package.Function: obj.Method() or pkg.Func()
        "selector_expression" => {
            let field = node.child_by_field_name("field")?;
            field.utf8_text(source).ok().map(|s| s.to_string())
        }

        // Other (type conversions, etc.)
        _ => node.utf8_text(source).ok().map(|s| s.to_string()),
    }
}

// ---------------------------------------------------------------------------
// C edge detection
// ---------------------------------------------------------------------------

fn walk_c_edges(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    match node.kind() {
        "call_expression" => {
            if let Some(func) = node.child_by_field_name("function") {
                let callee = extract_c_callee(func, source);
                if let Some(callee) = callee {
                    let line = node.start_position().row + 1;
                    let caller = find_enclosing_item(items, line);
                    edges.push(ExtractedEdge {
                        source_item_name: caller.unwrap_or_default(),
                        edge_type: EdgeType::Calls,
                        target_name: callee,
                        line,
                    });
                }
            }
            walk_c_edge_children(node, source, items, edges);
        }

        _ => {
            walk_c_edge_children(node, source, items, edges);
        }
    }
}

fn walk_c_edge_children(
    node: Node,
    source: &[u8],
    items: &[ExtractedItem],
    edges: &mut Vec<ExtractedEdge>,
) {
    for i in 0..node.named_child_count() {
        if let Some(child) = node.named_child(i) {
            walk_c_edges(child, source, items, edges);
        }
    }
}

/// Extract callee name from a C call expression's function node.
fn extract_c_callee(node: Node, source: &[u8]) -> Option<String> {
    match node.kind() {
        // Direct call: foo()
        "identifier" => node.utf8_text(source).ok().map(|s| s.to_string()),

        // Field access: obj->method() or obj.method()
        "field_expression" => {
            let field = node.child_by_field_name("field")?;
            field.utf8_text(source).ok().map(|s| s.to_string())
        }

        // Other (function pointer calls, etc.)
        _ => node.utf8_text(source).ok().map(|s| s.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Find the enclosing item name for a given line number.
/// Returns the most specific (deepest) item that contains the line.
fn find_enclosing_item(items: &[ExtractedItem], line: usize) -> Option<String> {
    let mut best: Option<&ExtractedItem> = None;

    for item in items {
        // Skip use declarations (they don't "contain" calls)
        if item.item_type == ItemType::UseDeclaration {
            continue;
        }
        if item.line_start <= line && item.line_end >= line {
            match best {
                None => best = Some(item),
                Some(current) => {
                    // Prefer the more specific (smaller range) item
                    let current_range = current.line_end - current.line_start;
                    let candidate_range = item.line_end - item.line_start;
                    if candidate_range < current_range {
                        best = Some(item);
                    }
                }
            }
        }
    }

    best.map(|i| i.name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::items;

    fn parse_and_extract_rust(source: &str) -> (Vec<ExtractedItem>, Vec<ExtractedEdge>) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::language())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let items = items::extract_items(source, Language::Rust, &tree);
        let edges = extract_edges(source, Language::Rust, &tree, &items);
        (items, edges)
    }

    #[test]
    fn test_detect_function_calls() {
        let source = r#"
fn caller() {
    callee();
    other_fn();
}

fn callee() {}
fn other_fn() {}
"#;
        let (_items, edges) = parse_and_extract_rust(source);

        let calls: Vec<_> = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Calls)
            .collect();

        assert!(calls.len() >= 2, "Expected at least 2 call edges, got {}", calls.len());
        assert!(calls.iter().any(|e| e.target_name == "callee"));
        assert!(calls.iter().any(|e| e.target_name == "other_fn"));
    }

    #[test]
    fn test_detect_method_calls() {
        let source = r#"
impl Foo {
    fn bar(&self) {
        self.baz();
    }
    fn baz(&self) {}
}
"#;
        let (_items, edges) = parse_and_extract_rust(source);

        let calls: Vec<_> = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Calls)
            .collect();

        assert!(calls.iter().any(|e| e.target_name == "baz"));
    }

    #[test]
    fn test_detect_contains_edges() {
        let source = r#"
impl Foo {
    fn bar(&self) {}
    fn baz(&self) {}
}
"#;
        let (_items, edges) = parse_and_extract_rust(source);

        let contains: Vec<_> = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Contains)
            .collect();

        assert_eq!(contains.len(), 2);
    }

    #[test]
    fn test_detect_implements_edge() {
        let source = r#"
trait Greetable {
    fn greet(&self);
}

impl Greetable for Person {
    fn greet(&self) {
        println!("Hello!");
    }
}
"#;
        let (_items, edges) = parse_and_extract_rust(source);

        let implements: Vec<_> = edges
            .iter()
            .filter(|e| e.edge_type == EdgeType::Implements)
            .collect();

        assert_eq!(implements.len(), 1);
        assert_eq!(implements[0].source_item_name, "Person");
        assert_eq!(implements[0].target_name, "Greetable");
    }
}
