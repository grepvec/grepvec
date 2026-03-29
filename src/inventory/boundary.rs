//! Boundary node infrastructure.
//!
//! Boundary nodes represent external dependencies at the crate/module level —
//! things our code depends on that we don't own. They are synthesized by agent
//! reasoning over unresolved edges, not by tree-sitter parsing.
//!
//! This module provides:
//! - Gap report: unresolved edges grouped by crate
//! - Boundary node CRUD in Postgres
//! - Edge resolution from items to boundary nodes

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// A boundary node — an inferred representation of an external dependency.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoundaryNode {
    pub id: Option<Uuid>,
    pub name: String,
    pub crate_name: String,
    pub version: Option<String>,
    pub category: String,
    pub description: Option<String>,
    pub apis_used: Vec<String>,
    pub config_env_vars: Vec<String>,
    pub failure_impact: Option<String>,
    pub confidence: f32,
    pub agent_id: Option<String>,
    pub dependent_repos: Vec<String>,
}

/// A gap report entry: unresolved edges grouped by crate prefix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GapEntry {
    pub crate_prefix: String,
    pub unresolved_targets: Vec<String>,
    pub dependent_items: usize,
    pub repos: Vec<String>,
    pub has_boundary_node: bool,
}

/// Ensure the boundary_nodes table exists.
pub async fn ensure_table(pool: &PgPool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS boundary_nodes (
            id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
            name TEXT NOT NULL UNIQUE,
            crate_name TEXT NOT NULL,
            version TEXT,
            category TEXT NOT NULL DEFAULT 'library',
            description TEXT,
            apis_used TEXT[] DEFAULT '{}',
            config_env_vars TEXT[] DEFAULT '{}',
            failure_impact TEXT,
            confidence REAL NOT NULL DEFAULT 0.5,
            agent_id TEXT,
            dependent_repos TEXT[] DEFAULT '{}',
            created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
            updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
        )",
    )
    .execute(pool)
    .await?;

    // Index for edge resolution
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_boundary_nodes_crate
         ON boundary_nodes (crate_name)",
    )
    .execute(pool)
    .await?;

    // Add boundary_node_id column to edges if missing
    sqlx::query(
        "ALTER TABLE edges ADD COLUMN IF NOT EXISTS boundary_node_id UUID REFERENCES boundary_nodes(id)",
    )
    .execute(pool)
    .await
    .ok();

    Ok(())
}

/// Generate a gap report: unresolved edges grouped by crate/module prefix.
pub async fn gap_report(pool: &PgPool, repo_name: Option<&str>) -> Result<Vec<GapEntry>, sqlx::Error> {
    // Get all unresolved call/macro edges with their source repo
    let rows: Vec<(String, String)> = if let Some(repo) = repo_name {
        sqlx::query_as(
            "SELECT DISTINCT e.target_name, r.name as repo_name
             FROM edges e
             JOIN items i ON e.source_item_id = i.id
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE e.target_item_id IS NULL
               AND e.target_dep_id IS NULL
               AND e.edge_type IN ('calls', 'macro_invocation')
               AND r.name = $1
             ORDER BY e.target_name",
        )
        .bind(repo)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as(
            "SELECT DISTINCT e.target_name, r.name as repo_name
             FROM edges e
             JOIN items i ON e.source_item_id = i.id
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE e.target_item_id IS NULL
               AND e.target_dep_id IS NULL
               AND e.edge_type IN ('calls', 'macro_invocation')
             ORDER BY e.target_name",
        )
        .fetch_all(pool)
        .await?
    };

    // Get existing boundary nodes for has_boundary_node check
    let existing: Vec<(String,)> = sqlx::query_as(
        "SELECT crate_name FROM boundary_nodes",
    )
    .fetch_all(pool)
    .await?;
    let existing_crates: std::collections::HashSet<String> =
        existing.into_iter().map(|(c,)| c).collect();

    // Group by crate prefix
    let mut groups: std::collections::BTreeMap<String, GapEntry> =
        std::collections::BTreeMap::new();

    for (target_name, repo_name) in &rows {
        // Extract crate prefix: "qdrant_client::Qdrant::search" → "qdrant_client"
        // "Config::from_env" → "Config" (local type, skip)
        // "serde_json::json" → "serde_json"
        let prefix = extract_crate_prefix(target_name);
        if prefix.is_empty() || is_likely_local_type(&prefix) {
            continue;
        }

        let entry = groups.entry(prefix.clone()).or_insert_with(|| GapEntry {
            crate_prefix: prefix.clone(),
            unresolved_targets: Vec::new(),
            dependent_items: 0,
            repos: Vec::new(),
            has_boundary_node: existing_crates.contains(&prefix),
        });

        if !entry.unresolved_targets.contains(target_name) {
            entry.unresolved_targets.push(target_name.clone());
        }
        entry.dependent_items += 1;
        if !entry.repos.contains(repo_name) {
            entry.repos.push(repo_name.clone());
        }
    }

    let mut result: Vec<GapEntry> = groups.into_values().collect();
    result.sort_by(|a, b| b.dependent_items.cmp(&a.dependent_items));
    Ok(result)
}

/// Extract the crate prefix from a qualified call target.
fn extract_crate_prefix(target: &str) -> String {
    if !target.contains("::") {
        return String::new();
    }
    let prefix = target.split("::").next().unwrap_or("");

    // Filter out expression chains: "resp.json", "s.parse", "body.field.method"
    if prefix.contains('.') || prefix.contains('(') || prefix.contains(')') {
        return String::new();
    }
    // Filter out multi-line expressions
    if prefix.contains('\n') || prefix.len() > 40 {
        return String::new();
    }
    // Filter out variable names that happen to have :: (turbofish on local vars)
    // Real crate names are snake_case or known identifiers
    if !is_valid_crate_name(prefix) {
        return String::new();
    }

    prefix.to_string()
}

/// Is this a plausible Rust crate name?
fn is_valid_crate_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 30 {
        return false;
    }
    // Known crates pass immediately
    if KNOWN_CRATES.contains(&name) {
        return true;
    }
    // Crate names: snake_case, lowercase, letters/digits/underscores
    // Also allow: std, PascalCase known types (but filter those as local)
    let first = name.chars().next().unwrap();
    if first.is_uppercase() {
        return false; // PascalCase = local type, not a crate
    }
    // Must be valid identifier chars
    name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Heuristic: is this likely a local type name rather than a crate?
fn is_likely_local_type(name: &str) -> bool {
    if name.is_empty() {
        return true;
    }
    let first = name.chars().next().unwrap();
    first.is_uppercase() && !KNOWN_CRATES.contains(&name)
}

const KNOWN_CRATES: &[&str] = &[
    "qdrant_client", "tonic", "reqwest", "sqlx", "axum", "tokio",
    "serde", "serde_json", "tracing", "tracing_subscriber",
    "chrono", "uuid", "anyhow", "thiserror", "clap", "colored",
    "hmac", "sha2", "hex", "base64", "aws_sdk_s3", "aws_sdk_dynamodb",
    "aws_config", "tower", "tower_http", "hyper", "http",
    "prost", "bytes", "futures", "async_trait", "once_cell",
    "regex", "walkdir", "tree_sitter", "tree_sitter_rust",
];

/// Create or update a boundary node.
pub async fn upsert_boundary_node(
    pool: &PgPool,
    node: &BoundaryNode,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO boundary_nodes
            (name, crate_name, version, category, description,
             apis_used, config_env_vars, failure_impact,
             confidence, agent_id, dependent_repos)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
         ON CONFLICT (name) DO UPDATE
         SET version = EXCLUDED.version,
             category = EXCLUDED.category,
             description = EXCLUDED.description,
             apis_used = EXCLUDED.apis_used,
             config_env_vars = EXCLUDED.config_env_vars,
             failure_impact = EXCLUDED.failure_impact,
             confidence = EXCLUDED.confidence,
             agent_id = EXCLUDED.agent_id,
             dependent_repos = EXCLUDED.dependent_repos,
             updated_at = NOW()
         RETURNING id",
    )
    .bind(&node.name)
    .bind(&node.crate_name)
    .bind(&node.version)
    .bind(&node.category)
    .bind(&node.description)
    .bind(&node.apis_used)
    .bind(&node.config_env_vars)
    .bind(&node.failure_impact)
    .bind(node.confidence)
    .bind(&node.agent_id)
    .bind(&node.dependent_repos)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Resolve unresolved edges to boundary nodes by matching crate prefix.
pub async fn resolve_to_boundary_nodes(pool: &PgPool) -> Result<usize, sqlx::Error> {
    // For each boundary node, resolve edges whose target starts with the crate name
    let result = sqlx::query(
        "UPDATE edges e
         SET boundary_node_id = bn.id,
             confidence = GREATEST(e.confidence, 0.4)
         FROM boundary_nodes bn
         WHERE e.target_item_id IS NULL
           AND e.boundary_node_id IS NULL
           AND e.edge_type IN ('calls', 'macro_invocation')
           AND split_part(e.target_name, '::', 1) = bn.crate_name",
    )
    .execute(pool)
    .await?;

    Ok(result.rows_affected() as usize)
}

/// List all boundary nodes.
pub async fn list_boundary_nodes(pool: &PgPool) -> Result<Vec<BoundaryNode>, sqlx::Error> {
    let rows: Vec<BoundaryNodeRow> = sqlx::query_as(
        "SELECT id, name, crate_name, version, category, description,
                apis_used, config_env_vars, failure_impact,
                confidence, agent_id, dependent_repos
         FROM boundary_nodes
         ORDER BY crate_name",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows.into_iter().map(|r| r.into()).collect())
}

/// Get boundary nodes that a specific item connects to.
pub async fn get_item_boundary_nodes(
    pool: &PgPool,
    item_id: Uuid,
) -> Result<Vec<(String, String, Option<String>)>, sqlx::Error> {
    // Returns (boundary_node_name, crate_name, failure_impact)
    sqlx::query_as(
        "SELECT DISTINCT bn.name, bn.crate_name, bn.failure_impact
         FROM edges e
         JOIN boundary_nodes bn ON e.boundary_node_id = bn.id
         WHERE e.source_item_id = $1",
    )
    .bind(item_id)
    .fetch_all(pool)
    .await
}

#[derive(Debug, sqlx::FromRow)]
struct BoundaryNodeRow {
    id: Uuid,
    name: String,
    crate_name: String,
    version: Option<String>,
    category: String,
    description: Option<String>,
    apis_used: Option<Vec<String>>,
    config_env_vars: Option<Vec<String>>,
    failure_impact: Option<String>,
    confidence: f32,
    agent_id: Option<String>,
    dependent_repos: Option<Vec<String>>,
}

impl From<BoundaryNodeRow> for BoundaryNode {
    fn from(r: BoundaryNodeRow) -> Self {
        BoundaryNode {
            id: Some(r.id),
            name: r.name,
            crate_name: r.crate_name,
            version: r.version,
            category: r.category,
            description: r.description,
            apis_used: r.apis_used.unwrap_or_default(),
            config_env_vars: r.config_env_vars.unwrap_or_default(),
            failure_impact: r.failure_impact,
            confidence: r.confidence,
            agent_id: r.agent_id,
            dependent_repos: r.dependent_repos.unwrap_or_default(),
        }
    }
}

/// Parse a Cargo.toml to extract dependency versions.
pub fn parse_cargo_versions(cargo_toml_path: &std::path::Path) -> std::collections::HashMap<String, String> {
    let mut versions = std::collections::HashMap::new();
    let content = match std::fs::read_to_string(cargo_toml_path) {
        Ok(c) => c,
        Err(_) => return versions,
    };
    let parsed: toml::Value = match content.parse() {
        Ok(v) => v,
        Err(_) => return versions,
    };

    // Check [dependencies] and [dev-dependencies]
    for section in &["dependencies", "dev-dependencies"] {
        if let Some(deps) = parsed.get(section).and_then(|d| d.as_table()) {
            for (name, value) in deps {
                let version = match value {
                    toml::Value::String(v) => v.clone(),
                    toml::Value::Table(t) => {
                        t.get("version")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    }
                    _ => String::new(),
                };
                if !version.is_empty() {
                    // Convert crate name: tree-sitter → tree_sitter
                    let normalized = name.replace('-', "_");
                    versions.insert(normalized, version);
                }
            }
        }
    }

    versions
}
