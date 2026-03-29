//! Universal IO surface classification.
//!
//! Classifies code items into behavioral IO classes based on patterns
//! that are language-agnostic: parameter names, return types, module paths,
//! call patterns, and function characteristics.
//!
//! These classes describe WHAT the surface does, not HOW the framework
//! implements it. They work for Rust, Python, TypeScript, Go, or any
//! well-structured web application.

use crate::canvas::layout::LayoutNode;
use std::collections::HashMap;

/// Universal IO surface classifications.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IoClass {
    /// Where the system verifies who you are
    Identity,
    /// What the human sees rendered (pages, components, templates)
    View,
    /// What the human triggers that mutates state (form submit, POST)
    Action,
    /// What the human or system reads without mutating (GET, search)
    Query,
    /// Where data enters from outside (upload, webhook, import, ingest)
    Ingest,
    /// Persistent push/bidirectional connections (SSE, WebSocket)
    Stream,
    /// Time-triggered operations (cron, background jobs, schedulers)
    Schedule,
    /// System administration and observability (health, backup, metrics)
    Operate,
    /// Internal: not a human-facing surface
    Internal,
}

impl IoClass {
    pub fn label(&self) -> &'static str {
        match self {
            IoClass::Identity => "Identity",
            IoClass::View => "View",
            IoClass::Action => "Action",
            IoClass::Query => "Query",
            IoClass::Ingest => "Ingest",
            IoClass::Stream => "Stream",
            IoClass::Schedule => "Schedule",
            IoClass::Operate => "Operate",
            IoClass::Internal => "Internal",
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            IoClass::Identity => "🔐",
            IoClass::View => "👁",
            IoClass::Action => "⚡",
            IoClass::Query => "🔍",
            IoClass::Ingest => "📥",
            IoClass::Stream => "📡",
            IoClass::Schedule => "⏰",
            IoClass::Operate => "🔧",
            IoClass::Internal => "⚙",
        }
    }

    pub fn color(&self) -> (u8, u8, u8) {
        match self {
            IoClass::Identity => (220, 180, 50),   // gold
            IoClass::View     => (80, 160, 220),    // sky blue
            IoClass::Action   => (220, 100, 60),    // orange-red
            IoClass::Query    => (60, 180, 140),    // teal
            IoClass::Ingest   => (160, 100, 200),   // purple
            IoClass::Stream   => (100, 200, 100),   // green
            IoClass::Schedule => (180, 140, 80),    // amber
            IoClass::Operate  => (140, 140, 160),   // gray-blue
            IoClass::Internal => (80, 80, 90),      // dark gray
        }
    }

    /// All human-facing IO classes (excludes Internal).
    pub fn human_facing() -> &'static [IoClass] {
        &[
            IoClass::Identity,
            IoClass::View,
            IoClass::Action,
            IoClass::Query,
            IoClass::Ingest,
            IoClass::Stream,
            IoClass::Operate,
        ]
    }
}

/// Classify a node's IO surface type based on behavioral signals.
pub fn classify_io(node: &LayoutNode, call_targets: &[String]) -> IoClass {
    let sig = node.qualified_name.to_lowercase();
    let path = node.module_path.to_lowercase();
    let file = node.file_path.to_lowercase();
    let name = node.name.to_lowercase();

    // --- Identity: authentication, authorization, key validation ---
    if matches_any(&[&sig, &path, &file, &name], &[
        "auth", "hmac", "keycloak", "api_key", "apikey", "validate_key",
        "verify", "credential", "login", "logout", "session", "jwt",
        "token", "oauth", "permission", "rbac", "identity",
    ]) && !name.contains("test") {
        return IoClass::Identity;
    }

    // --- View: rendering, pages, components, templates ---
    if matches_any(&[&sig, &path, &file], &[
        "page", "pages", "component", "components", "frontend",
        "template", "render", "view", "layout", "dashboard",
        "canvas", "widget", "panel", "modal", "form",
    ]) && node.item_type == "function" {
        return IoClass::View;
    }

    // --- Stream: SSE, WebSocket, streaming responses ---
    if matches_any(&[&sig, &path, &name], &[
        "stream", "sse", "websocket", "ws_", "channel",
        "subscribe", "event_stream", "push", "tail",
    ]) {
        return IoClass::Stream;
    }

    // --- Operate: health, admin, backup, metrics, logs ---
    if matches_any(&[&sig, &path, &name], &[
        "health", "healthz", "ready", "readyz",
        "backup", "restore", "migrate", "migration",
        "metrics", "prometheus", "telemetry",
        "admin", "manage", "status", "debug",
        "log_search", "log_stream", "log_metric",
    ]) {
        return IoClass::Operate;
    }

    // --- Ingest: data entering the system ---
    if matches_any(&[&sig, &path, &name], &[
        "ingest", "import", "upload", "webhook",
        "bulk", "batch", "segment_document",
        "prepare", "process_paragraph",
    ]) {
        return IoClass::Ingest;
    }

    // --- Schedule: time-triggered, background jobs ---
    if matches_any(&[&sig, &path, &name], &[
        "cron", "schedule", "periodic", "background",
        "job", "worker", "task_run", "sweep", "cleanup",
    ]) {
        return IoClass::Schedule;
    }

    // --- Action vs Query: mutation vs read-only ---
    // Actions: create, update, delete, write, set, add, remove, promote
    if matches_any(&[&name], &[
        "create", "update", "delete", "remove", "set_",
        "add_", "put_", "write", "insert", "promote",
        "generate_api_key", "revoke",
    ]) && node.visibility == "pub" {
        return IoClass::Action;
    }

    // Queries: get, list, search, find, fetch, query, stats, export
    if matches_any(&[&name], &[
        "search", "query", "find", "get_", "list_",
        "fetch", "stats", "count", "export", "reconstruct",
        "compare", "analyze",
    ]) && node.visibility == "pub" {
        return IoClass::Query;
    }

    // --- Fallback: public functions with outgoing calls are potential surfaces ---
    if node.visibility == "pub" && node.item_type == "function" && !call_targets.is_empty() {
        // Check call targets for mutation signals
        let has_writes = call_targets.iter().any(|t| {
            let tl = t.to_lowercase();
            tl.contains("insert") || tl.contains("upsert") || tl.contains("delete")
                || tl.contains("update") || tl.contains("create") || tl.contains("write")
        });
        if has_writes {
            return IoClass::Action;
        }
        return IoClass::Query;
    }

    IoClass::Internal
}

/// Classify all nodes in the layout and return a map of node_id → IoClass.
pub fn classify_all(
    nodes: &HashMap<String, LayoutNode>,
    call_graph: &HashMap<String, Vec<String>>,
) -> HashMap<String, IoClass> {
    let mut result = HashMap::new();
    for (id, node) in nodes {
        let targets = call_graph.get(id).cloned().unwrap_or_default();
        result.insert(id.clone(), classify_io(node, &targets));
    }
    result
}

fn matches_any(haystacks: &[&str], needles: &[&str]) -> bool {
    for hay in haystacks {
        for needle in needles {
            if hay.contains(needle) {
                return true;
            }
        }
    }
    false
}
