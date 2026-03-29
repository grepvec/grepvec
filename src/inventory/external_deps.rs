//! External dependency detection.
//!
//! Maps crate imports and environment variable references to known
//! external dependencies (Qdrant, Neon, OpenAI, Keycloak, etc.).

use crate::inventory::{EdgeType, ExtractedEdge, ExtractedItem, ItemType};

/// A known external dependency mapping.
struct ExternalDepRule {
    /// Crate or module prefix that indicates this dependency
    import_prefix: &'static str,
    /// Name of the external dependency (matches external_dependencies.name in DB)
    dep_name: &'static str,
}

/// Environment variable → external dependency mapping.
struct EnvVarRule {
    env_var: &'static str,
    dep_name: &'static str,
}

const CRATE_RULES: &[ExternalDepRule] = &[
    ExternalDepRule { import_prefix: "qdrant_client", dep_name: "qdrant" },
    ExternalDepRule { import_prefix: "sqlx", dep_name: "neon-postgres" },
    ExternalDepRule { import_prefix: "async_openai", dep_name: "openai-embeddings" },
    ExternalDepRule { import_prefix: "reqwest", dep_name: "http-client" },
    ExternalDepRule { import_prefix: "tonic", dep_name: "grpc" },
    ExternalDepRule { import_prefix: "aws_sdk_s3", dep_name: "aws-s3" },
    ExternalDepRule { import_prefix: "aws_sdk_dynamodb", dep_name: "aws-dynamodb" },
    ExternalDepRule { import_prefix: "aws_config", dep_name: "aws-config" },
    ExternalDepRule { import_prefix: "keycloak", dep_name: "keycloak" },
    ExternalDepRule { import_prefix: "loki_", dep_name: "grafana-loki" },
    ExternalDepRule { import_prefix: "vector", dep_name: "vector-agent" },
    ExternalDepRule { import_prefix: "tracing", dep_name: "tracing" },
];

const ENV_VAR_RULES: &[EnvVarRule] = &[
    EnvVarRule { env_var: "DATABASE_URL", dep_name: "neon-postgres" },
    EnvVarRule { env_var: "QDRANT_GRPC", dep_name: "qdrant" },
    EnvVarRule { env_var: "QDRANT_URL", dep_name: "qdrant" },
    EnvVarRule { env_var: "OPENAI_API_KEY", dep_name: "openai-embeddings" },
    EnvVarRule { env_var: "VOYAGE_API_KEY", dep_name: "voyage-embeddings" },
    EnvVarRule { env_var: "ANTHROPIC_API_KEY", dep_name: "anthropic-chunking" },
    EnvVarRule { env_var: "BGE_ENDPOINT", dep_name: "bge-tei" },
    EnvVarRule { env_var: "KEYCLOAK_URL", dep_name: "keycloak" },
    EnvVarRule { env_var: "ESM_MASTER_KEY", dep_name: "esm" },
    EnvVarRule { env_var: "OBSERVE_GRPC_ADDR", dep_name: "grpc-observe" },
    EnvVarRule { env_var: "EMBED_URL", dep_name: "grpc-embed" },
    EnvVarRule { env_var: "LOKI_URL", dep_name: "grafana-loki" },
    EnvVarRule { env_var: "BACKUP_BUCKET", dep_name: "aws-s3" },
    EnvVarRule { env_var: "TOWER_DB_URL", dep_name: "neon-postgres-tower" },
];

/// Detect external dependency edges from extracted items.
///
/// Scans use declarations for known crate imports and scans source text
/// for environment variable references.
pub fn detect_external_deps(
    items: &[ExtractedItem],
    source: &str,
) -> Vec<ExtractedEdge> {
    let mut edges = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Check use declarations for known crate imports
    for item in items {
        if item.item_type == ItemType::UseDeclaration {
            for rule in CRATE_RULES {
                if item.name.contains(rule.import_prefix) {
                    let key = (item.line_start, rule.dep_name);
                    if seen.insert(key) {
                        edges.push(ExtractedEdge {
                            source_item_name: item.name.clone(),
                            edge_type: EdgeType::ExternalDep,
                            target_name: rule.dep_name.to_string(),
                            line: item.line_start,
                        });
                    }
                }
            }
        }
    }

    // Scan source for env var references
    for rule in ENV_VAR_RULES {
        if source.contains(rule.env_var) {
            // Find which item contains this reference
            for (line_idx, line) in source.lines().enumerate() {
                if line.contains(rule.env_var) {
                    let line_num = line_idx + 1;
                    let caller = find_enclosing_named_item(items, line_num);
                    let key = (line_num, rule.dep_name);
                    if seen.insert(key) {
                        edges.push(ExtractedEdge {
                            source_item_name: caller.unwrap_or_else(|| "<module>".to_string()),
                            edge_type: EdgeType::ExternalDep,
                            target_name: rule.dep_name.to_string(),
                            line: line_num,
                        });
                    }
                }
            }
        }
    }

    edges
}

/// Find the enclosing named item for a line (excludes use declarations).
fn find_enclosing_named_item(items: &[ExtractedItem], line: usize) -> Option<String> {
    let mut best: Option<&ExtractedItem> = None;

    for item in items {
        if item.item_type == ItemType::UseDeclaration {
            continue;
        }
        if item.line_start <= line && item.line_end >= line {
            match best {
                None => best = Some(item),
                Some(current) => {
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
    use crate::inventory::Visibility;

    fn make_use_item(name: &str, line: usize) -> ExtractedItem {
        ExtractedItem {
            name: name.to_string(),
            item_type: ItemType::UseDeclaration,
            visibility: Visibility::Private,
            signature: None,
            doc_comment: None,
            line_start: line,
            line_end: line,
            is_test: false,
            is_async: false,
            attributes: vec![],
            child_indices: vec![],
            qualified_name: None,
        }
    }

    #[test]
    fn test_detect_qdrant_dep() {
        let items = vec![make_use_item("use qdrant_client::Qdrant", 1)];
        let edges = detect_external_deps(&items, "use qdrant_client::Qdrant;\n");

        assert!(edges.iter().any(|e| e.target_name == "qdrant"));
    }

    #[test]
    fn test_detect_env_var_dep() {
        let source = r#"
fn connect() {
    let url = std::env::var("DATABASE_URL").unwrap();
}
"#;
        let items = vec![ExtractedItem {
            name: "connect".to_string(),
            item_type: ItemType::Function,
            visibility: Visibility::Private,
            signature: None,
            doc_comment: None,
            line_start: 2,
            line_end: 4,
            is_test: false,
            is_async: false,
            attributes: vec![],
            child_indices: vec![],
            qualified_name: None,
        }];

        let edges = detect_external_deps(&items, source);
        assert!(edges.iter().any(|e| e.target_name == "neon-postgres"));
    }

    #[test]
    fn test_no_duplicate_deps() {
        let items = vec![
            make_use_item("use qdrant_client::Qdrant", 1),
            make_use_item("use qdrant_client::models::Filter", 2),
        ];
        let source = "use qdrant_client::Qdrant;\nuse qdrant_client::models::Filter;\n";
        let edges = detect_external_deps(&items, source);

        let qdrant_edges: Vec<_> = edges.iter().filter(|e| e.target_name == "qdrant").collect();
        assert_eq!(qdrant_edges.len(), 2); // one per use statement
    }
}
