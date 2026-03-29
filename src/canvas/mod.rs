//! grepvec Canvas — Circuit board visualization of the code graph.
//!
//! Renders the two-layer graph (deterministic + inferred) as a layered
//! circuit board diagram. Backend at the bottom, frontend at the top.
//! Same code → same layout → same canvas.

pub mod layout;
pub mod classify;
pub mod sphere_view;

use layout::{CircuitLayout, LayoutNode, LayoutEdge};
use sqlx::PgPool;
use std::collections::HashMap;

/// Load the full graph from Postgres and compute the circuit board layout.
pub async fn load_layout(pool: &PgPool) -> Result<CircuitLayout, sqlx::Error> {
    // Load all items
    let item_rows: Vec<(String, String, Option<String>, Option<String>, String, String, i32, i32, Option<String>, Option<bool>)> = sqlx::query_as(
        "SELECT i.name, i.item_type, i.qualified_name, i.visibility,
                sf.file_path, r.name as repo_name,
                i.line_start, i.line_end, i.signature, i.is_async
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         JOIN repositories r ON sf.repo_id = r.id
         WHERE i.item_type IN ('function', 'struct', 'enum', 'trait', 'impl')
           AND (i.is_test = false OR i.is_test IS NULL)",
    )
    .fetch_all(pool)
    .await?;

    let mut nodes: HashMap<String, LayoutNode> = HashMap::new();

    for (name, item_type, qname, vis, file_path, repo, line_start, line_end, _sig, is_async) in &item_rows {
        let qname_str = qname.as_deref().unwrap_or(name.as_str());
        let module_path = extract_module_path(qname_str);

        nodes.insert(qname_str.to_string(), LayoutNode {
            id: qname_str.to_string(),
            item_type: item_type.clone(),
            name: name.clone(),
            qualified_name: qname_str.to_string(),
            module_path,
            repo: repo.clone(),
            file_path: file_path.clone(),
            line_start: *line_start,
            visibility: vis.as_deref().unwrap_or("private").to_string(),
            loc: (line_end - line_start + 1).max(1),
            is_async: is_async.unwrap_or(false),
            is_boundary: false,
            layer: 0,
            block_id: String::new(),
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
    }

    // Load boundary nodes
    let boundary_rows: Vec<(String, String, String, Option<String>)> = sqlx::query_as(
        "SELECT name, crate_name, category, failure_impact FROM boundary_nodes",
    )
    .fetch_all(pool)
    .await?;

    for (name, _crate_name, category, _impact) in &boundary_rows {
        nodes.insert(format!("boundary::{}", name), LayoutNode {
            id: format!("boundary::{}", name),
            item_type: "boundary".to_string(),
            name: name.clone(),
            qualified_name: format!("boundary::{}", name),
            module_path: category.clone(),
            repo: "external".to_string(),
            file_path: String::new(),
            line_start: 0,
            visibility: "external".to_string(),
            loc: 0,
            is_async: false,
            is_boundary: true,
            block_id: String::new(),
            layer: 0,
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
    }

    // Load edges
    let edge_rows: Vec<(String, Option<String>, String, Option<String>)> = sqlx::query_as(
        "SELECT COALESCE(src.qualified_name, src.name) as source_name,
                COALESCE(tgt.qualified_name, tgt.name) as target_name,
                e.edge_type,
                bn.name as boundary_name
         FROM edges e
         JOIN items src ON e.source_item_id = src.id
         LEFT JOIN items tgt ON e.target_item_id = tgt.id
         LEFT JOIN boundary_nodes bn ON e.boundary_node_id = bn.id
         WHERE e.edge_type IN ('calls', 'implements', 'contains', 'uses_type')",
    )
    .fetch_all(pool)
    .await?;

    let mut edges = Vec::new();
    for (source, target, edge_type, boundary_name) in &edge_rows {
        let target_id = if let Some(bn) = boundary_name {
            format!("boundary::{}", bn)
        } else if let Some(t) = target {
            t.clone()
        } else {
            continue;
        };

        // Only include edges where both nodes exist in our layout
        if nodes.contains_key(source) && nodes.contains_key(&target_id) {
            edges.push(LayoutEdge {
                source_id: source.clone(),
                target_id,
                edge_type: edge_type.clone(),
                cross_layer: false,
                length: 0.0, // computed during layout
            });
        }
    }

    // Compute layout
    Ok(layout::compute_layout(nodes, edges))
}

/// Extract module path from qualified name.
/// "api::grpc::ObserveGrpcService::health_check" → "api::grpc"
fn extract_module_path(qualified_name: &str) -> String {
    let parts: Vec<&str> = qualified_name.split("::").collect();
    if parts.len() <= 2 {
        parts.first().unwrap_or(&"").to_string()
    } else {
        // Take all parts except the last two (type + method)
        parts[..parts.len() - 2].join("::")
    }
}
