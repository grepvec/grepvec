//! Biography generation from absorbed inventory data.
//!
//! Assembles structured biographies for code items by querying the
//! Postgres inventory for relationships (callers, callees, external deps,
//! parent containment) and formatting them as human-readable + embeddable text.

use sqlx::PgPool;
use uuid::Uuid;

/// A fully assembled biography for a code item.
#[derive(Debug, Clone)]
pub struct Biography {
    pub item_id: Uuid,
    pub qualified_name: String,
    pub item_type: String,
    pub file_path: String,
    pub repo_name: String,
    pub text: String,
}

/// A named segment of a biography for element-level embedding.
#[derive(Debug, Clone)]
pub struct BiographySegment {
    pub label: String,
    pub content: String,
}

/// A biography broken into element-level segments for structured embedding.
#[derive(Debug, Clone)]
pub struct SegmentedBiography {
    pub item_id: Uuid,
    pub qualified_name: String,
    pub item_type: String,
    pub file_path: String,
    pub repo_name: String,
    pub segments: Vec<BiographySegment>,
}

/// Raw data for biography assembly, fetched from the DB.
#[derive(Debug, sqlx::FromRow)]
struct BiographyRow {
    id: Uuid,
    name: String,
    qualified_name: Option<String>,
    item_type: String,
    visibility: Option<String>,
    signature: Option<String>,
    doc_comment: Option<String>,
    line_start: i32,
    line_end: i32,
    is_async: Option<bool>,
    file_path: String,
    repo_name: String,
    parent_name: Option<String>,
    parent_type: Option<String>,
}

/// Generate biographies for all public, non-test items in a repository.
pub async fn generate_biographies(
    pool: &PgPool,
    repo_name: &str,
) -> Result<Vec<Biography>, sqlx::Error> {
    // Fetch all biography-eligible items
    let items: Vec<BiographyRow> = sqlx::query_as(
        "SELECT i.id, i.name, i.qualified_name, i.item_type, i.visibility,
                i.signature, i.doc_comment, i.line_start, i.line_end, i.is_async,
                sf.file_path, r.name as repo_name,
                parent.name as parent_name, parent.item_type as parent_type
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         JOIN repositories r ON sf.repo_id = r.id
         LEFT JOIN items parent ON i.parent_item_id = parent.id
         WHERE r.name = $1
           AND i.item_type IN ('function', 'struct', 'enum', 'trait', 'impl')
           AND (i.is_test = false OR i.is_test IS NULL)
         ORDER BY sf.file_path, i.line_start",
    )
    .bind(repo_name)
    .fetch_all(pool)
    .await?;

    if items.is_empty() {
        return Ok(Vec::new());
    }

    // Collect all item IDs for batch edge queries
    let item_ids: Vec<Uuid> = items.iter().map(|i| i.id).collect();

    // Batch fetch all callers (who calls these items)
    let caller_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.target_item_id as item_id,
                COALESCE(src.qualified_name, src.name) as caller_name
         FROM edges e
         JOIN items src ON e.source_item_id = src.id
         WHERE e.target_item_id = ANY($1)
           AND e.edge_type = 'calls'",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Batch fetch all callees (what these items call)
    let callee_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.source_item_id as item_id,
                COALESCE(tgt.qualified_name, tgt.name, e.target_name) as callee_name
         FROM edges e
         LEFT JOIN items tgt ON e.target_item_id = tgt.id
         WHERE e.source_item_id = ANY($1)
           AND e.edge_type IN ('calls', 'macro_invocation')",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Batch fetch external deps
    let dep_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.source_item_id as item_id, e.target_name
         FROM edges e
         WHERE e.source_item_id = ANY($1)
           AND e.edge_type = 'external_dep'",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Batch fetch children
    let child_rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT parent_item_id as item_id, name, item_type
         FROM items
         WHERE parent_item_id = ANY($1)
         ORDER BY line_start",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Build lookup maps
    let mut callers: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (item_id, name) in &caller_rows {
        callers.entry(*item_id).or_default().push(name.clone());
    }

    let mut callees: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (item_id, name) in &callee_rows {
        callees.entry(*item_id).or_default().push(name.clone());
    }

    let mut deps: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (item_id, name) in &dep_rows {
        deps.entry(*item_id).or_default().push(name.clone());
    }

    let mut children: std::collections::HashMap<Uuid, Vec<(String, String)>> =
        std::collections::HashMap::new();
    for (item_id, name, item_type) in &child_rows {
        children
            .entry(*item_id)
            .or_default()
            .push((name.clone(), item_type.clone()));
    }

    // Assemble biographies
    let mut biographies = Vec::with_capacity(items.len());

    for item in &items {
        let qname = item
            .qualified_name
            .as_deref()
            .unwrap_or(&item.name);

        let item_callers = callers.get(&item.id);
        let item_callees = callees.get(&item.id);
        let item_deps = deps.get(&item.id);
        let item_children = children.get(&item.id);

        let loc = (item.line_end - item.line_start + 1).max(1);
        let caller_count = item_callers.map(|c| c.len()).unwrap_or(0);
        let callee_count = item_callees.map(|c| c.len()).unwrap_or(0);

        let text = format_biography(
            &item.item_type,
            qname,
            &item.file_path,
            item.visibility.as_deref(),
            item.signature.as_deref(),
            item.doc_comment.as_deref(),
            item.is_async.unwrap_or(false),
            loc,
            item.parent_name.as_deref(),
            item.parent_type.as_deref(),
            item_callers,
            item_callees,
            item_deps,
            item_children,
            caller_count,
            callee_count,
        );

        biographies.push(Biography {
            item_id: item.id,
            qualified_name: qname.to_string(),
            item_type: item.item_type.clone(),
            file_path: item.file_path.clone(),
            repo_name: item.repo_name.clone(),
            text,
        });
    }

    Ok(biographies)
}

fn format_biography(
    item_type: &str,
    qualified_name: &str,
    file_path: &str,
    visibility: Option<&str>,
    signature: Option<&str>,
    doc_comment: Option<&str>,
    is_async: bool,
    loc: i32,
    parent_name: Option<&str>,
    parent_type: Option<&str>,
    callers: Option<&Vec<String>>,
    callees: Option<&Vec<String>>,
    external_deps: Option<&Vec<String>>,
    children: Option<&Vec<(String, String)>>,
    caller_count: usize,
    callee_count: usize,
) -> String {
    let mut bio = String::with_capacity(512);

    // Header: type + qualified name + file
    bio.push_str(&format!("{} {} ({})\n", item_type, qualified_name, file_path));

    // Signature (for functions)
    if let Some(sig) = signature {
        let sig_trimmed = sig.lines().next().unwrap_or(sig);
        if sig_trimmed.len() <= 120 {
            bio.push_str(&format!("  {}\n", sig_trimmed));
        }
    }

    // Doc comment (existing documentation)
    if let Some(doc) = doc_comment {
        bio.push('\n');
        for line in doc.lines().take(5) {
            bio.push_str(&format!("  {}\n", line));
        }
    }

    bio.push('\n');

    // Parent containment
    if let Some(parent) = parent_name {
        let ptype = parent_type.unwrap_or("container");
        bio.push_str(&format!("Parent: {} ({})\n", parent, ptype));
    }

    // Callers (who calls this)
    if let Some(callers) = callers {
        if !callers.is_empty() {
            let deduped = dedup_names(callers, 8);
            bio.push_str(&format!("Called by: {}\n", deduped.join(", ")));
        }
    }

    // Callees (what this calls)
    if let Some(callees) = callees {
        if !callees.is_empty() {
            let deduped = dedup_names(callees, 8);
            bio.push_str(&format!("Calls: {}\n", deduped.join(", ")));
        }
    }

    // External dependencies
    if let Some(deps) = external_deps {
        if !deps.is_empty() {
            let deduped = dedup_names(deps, 6);
            bio.push_str(&format!("Touches: {}\n", deduped.join(", ")));
        }
    }

    // Children (for impl blocks, traits, modules)
    if let Some(children) = children {
        if !children.is_empty() && (item_type == "impl" || item_type == "trait") {
            let method_names: Vec<&str> = children
                .iter()
                .filter(|(_, t)| t == "function")
                .map(|(n, _)| n.as_str())
                .take(12)
                .collect();
            if !method_names.is_empty() {
                bio.push_str(&format!("Methods: {}\n", method_names.join(", ")));
            }
        }
    }

    // Status line
    let vis = visibility.unwrap_or("private");
    let mut flags = vec![vis.to_string()];
    if is_async {
        flags.push("async".to_string());
    }
    flags.push(format!("{} LOC", loc));
    if caller_count > 0 {
        flags.push(format!("{} callers", caller_count));
    }
    if callee_count > 0 {
        flags.push(format!("{} callees", callee_count));
    }
    bio.push_str(&format!("\nStatus: {}\n", flags.join(" | ")));

    bio
}

/// Break a biography into element-level segments for structured embedding.
/// Each segment is a distinct semantic unit: identity, callers, callees, etc.
/// The qualified name is prepended to every segment as context.
fn segment_biography(
    item_type: &str,
    qualified_name: &str,
    file_path: &str,
    visibility: Option<&str>,
    signature: Option<&str>,
    doc_comment: Option<&str>,
    is_async: bool,
    loc: i32,
    parent_name: Option<&str>,
    parent_type: Option<&str>,
    callers: Option<&Vec<String>>,
    callees: Option<&Vec<String>>,
    external_deps: Option<&Vec<String>>,
    children: Option<&Vec<(String, String)>>,
    caller_count: usize,
    callee_count: usize,
) -> Vec<BiographySegment> {
    let mut segments = Vec::new();
    let context = format!("{} {}", item_type, qualified_name);

    // Identity + signature + doc (what is this)
    let mut identity = format!("{} ({}).", context, file_path);
    if let Some(sig) = signature {
        let sig_line = sig.lines().next().unwrap_or(sig);
        if sig_line.len() <= 120 {
            identity.push_str(&format!(" {}", sig_line));
        }
    }
    if let Some(doc) = doc_comment {
        let first_lines: Vec<&str> = doc.lines().take(3).collect();
        if !first_lines.is_empty() {
            identity.push_str(&format!(" {}", first_lines.join(" ")));
        }
    }
    segments.push(BiographySegment {
        label: "identity".to_string(),
        content: identity,
    });

    // Parent (containment)
    if let Some(parent) = parent_name {
        let ptype = parent_type.unwrap_or("container");
        segments.push(BiographySegment {
            label: "parent".to_string(),
            content: format!("{} — Parent: {} ({})", context, parent, ptype),
        });
    }

    // Callers (who calls this)
    if let Some(callers) = callers {
        if !callers.is_empty() {
            let deduped = dedup_names(callers, 15);
            segments.push(BiographySegment {
                label: "callers".to_string(),
                content: format!("{} — Called by: {}", context, deduped.join(", ")),
            });
        }
    }

    // Callees (what this calls)
    if let Some(callees) = callees {
        if !callees.is_empty() {
            let deduped = dedup_names(callees, 15);
            segments.push(BiographySegment {
                label: "callees".to_string(),
                content: format!("{} — Calls: {}", context, deduped.join(", ")),
            });
        }
    }

    // External dependencies
    if let Some(deps) = external_deps {
        if !deps.is_empty() {
            let deduped = dedup_names(deps, 10);
            segments.push(BiographySegment {
                label: "touches".to_string(),
                content: format!("{} — Touches: {}", context, deduped.join(", ")),
            });
        }
    }

    // Methods (for impl/trait blocks)
    if let Some(children) = children {
        if !children.is_empty() && (item_type == "impl" || item_type == "trait") {
            let method_names: Vec<&str> = children
                .iter()
                .filter(|(_, t)| t == "function")
                .map(|(n, _)| n.as_str())
                .take(20)
                .collect();
            if !method_names.is_empty() {
                segments.push(BiographySegment {
                    label: "methods".to_string(),
                    content: format!("{} — Methods: {}", context, method_names.join(", ")),
                });
            }
        }
    }

    // Status
    let vis = visibility.unwrap_or("private");
    let mut flags = vec![vis.to_string()];
    if is_async { flags.push("async".to_string()); }
    flags.push(format!("{} LOC", loc));
    if caller_count > 0 { flags.push(format!("{} callers", caller_count)); }
    if callee_count > 0 { flags.push(format!("{} callees", callee_count)); }
    segments.push(BiographySegment {
        label: "status".to_string(),
        content: format!("{} — Status: {}", context, flags.join(" | ")),
    });

    segments
}

/// Generate segmented biographies (element-level) for all public, non-test items.
pub async fn generate_segmented_biographies(
    pool: &PgPool,
    repo_name: &str,
) -> Result<Vec<SegmentedBiography>, sqlx::Error> {
    // Reuse the same DB queries as generate_biographies
    let items: Vec<BiographyRow> = sqlx::query_as(
        "SELECT i.id, i.name, i.qualified_name, i.item_type, i.visibility,
                i.signature, i.doc_comment, i.line_start, i.line_end, i.is_async,
                sf.file_path, r.name as repo_name,
                parent.name as parent_name, parent.item_type as parent_type
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         JOIN repositories r ON sf.repo_id = r.id
         LEFT JOIN items parent ON i.parent_item_id = parent.id
         WHERE r.name = $1
           AND i.item_type IN ('function', 'struct', 'enum', 'trait', 'impl')
           AND (i.is_test = false OR i.is_test IS NULL)
         ORDER BY sf.file_path, i.line_start",
    )
    .bind(repo_name)
    .fetch_all(pool)
    .await?;

    if items.is_empty() {
        return Ok(Vec::new());
    }

    let item_ids: Vec<Uuid> = items.iter().map(|i| i.id).collect();

    let caller_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.target_item_id as item_id,
                COALESCE(src.qualified_name, src.name) as caller_name
         FROM edges e
         JOIN items src ON e.source_item_id = src.id
         WHERE e.target_item_id = ANY($1) AND e.edge_type = 'calls'",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    let callee_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.source_item_id as item_id,
                COALESCE(tgt.qualified_name, tgt.name, e.target_name) as callee_name
         FROM edges e
         LEFT JOIN items tgt ON e.target_item_id = tgt.id
         WHERE e.source_item_id = ANY($1)
           AND e.edge_type IN ('calls', 'macro_invocation')",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    let dep_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.source_item_id as item_id, e.target_name
         FROM edges e
         WHERE e.source_item_id = ANY($1) AND e.edge_type = 'external_dep'",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    let child_rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT parent_item_id as item_id, name, item_type
         FROM items
         WHERE parent_item_id = ANY($1)
         ORDER BY line_start",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    let mut callers: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (id, name) in &caller_rows { callers.entry(*id).or_default().push(name.clone()); }

    let mut callees: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (id, name) in &callee_rows { callees.entry(*id).or_default().push(name.clone()); }

    let mut deps: std::collections::HashMap<Uuid, Vec<String>> = std::collections::HashMap::new();
    for (id, name) in &dep_rows { deps.entry(*id).or_default().push(name.clone()); }

    let mut children: std::collections::HashMap<Uuid, Vec<(String, String)>> = std::collections::HashMap::new();
    for (id, name, itype) in &child_rows { children.entry(*id).or_default().push((name.clone(), itype.clone())); }

    let mut result = Vec::with_capacity(items.len());

    for item in &items {
        let qname = item.qualified_name.as_deref().unwrap_or(&item.name);
        let loc = (item.line_end - item.line_start + 1).max(1);
        let caller_count = callers.get(&item.id).map(|c| c.len()).unwrap_or(0);
        let callee_count = callees.get(&item.id).map(|c| c.len()).unwrap_or(0);

        let segments = segment_biography(
            &item.item_type, qname, &item.file_path,
            item.visibility.as_deref(), item.signature.as_deref(),
            item.doc_comment.as_deref(), item.is_async.unwrap_or(false),
            loc, item.parent_name.as_deref(), item.parent_type.as_deref(),
            callers.get(&item.id), callees.get(&item.id),
            deps.get(&item.id), children.get(&item.id),
            caller_count, callee_count,
        );

        result.push(SegmentedBiography {
            item_id: item.id,
            qualified_name: qname.to_string(),
            item_type: item.item_type.clone(),
            file_path: item.file_path.clone(),
            repo_name: item.repo_name.clone(),
            segments,
        });
    }

    Ok(result)
}

/// Deduplicate and limit a list of names.
fn dedup_names(names: &[String], limit: usize) -> Vec<&str> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    for name in names {
        if seen.insert(name.as_str()) {
            result.push(name.as_str());
            if result.len() >= limit {
                break;
            }
        }
    }
    result
}

/// Store biographies in the annotations table.
pub async fn store_biographies(
    pool: &PgPool,
    biographies: &[Biography],
    git_sha: &str,
) -> Result<usize, sqlx::Error> {
    if biographies.is_empty() {
        return Ok(0);
    }

    let item_ids: Vec<Uuid> = biographies.iter().map(|b| b.item_id).collect();
    let annotation_types: Vec<String> = vec!["biography".to_string(); biographies.len()];
    let contents: Vec<String> = biographies.iter().map(|b| b.text.clone()).collect();
    let authors: Vec<String> = vec!["grepvec-absorb".to_string(); biographies.len()];
    let git_shas: Vec<String> = vec![git_sha.to_string(); biographies.len()];

    let result = sqlx::query(
        "INSERT INTO annotations (item_id, annotation_type, content, author, git_sha, is_stale)
         SELECT * FROM UNNEST(
            $1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[],
            (SELECT array_agg(false) FROM generate_series(1, $6))::bool[]
         )
         ON CONFLICT (item_id, annotation_type) DO UPDATE
         SET content = EXCLUDED.content,
             author = EXCLUDED.author,
             git_sha = EXCLUDED.git_sha,
             is_stale = false,
             updated_at = NOW()",
    )
    .bind(&item_ids)
    .bind(&annotation_types)
    .bind(&contents)
    .bind(&authors)
    .bind(&git_shas)
    .bind(biographies.len() as i32)
    .execute(pool)
    .await?;

    Ok(result.rows_affected() as usize)
}

/// Get count of stale annotations for a repo.
pub async fn count_stale_biographies(
    pool: &PgPool,
    repo_name: &str,
) -> Result<i64, sqlx::Error> {
    let row: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM annotations a
         JOIN items i ON a.item_id = i.id
         JOIN source_files sf ON i.file_id = sf.id
         JOIN repositories r ON sf.repo_id = r.id
         WHERE r.name = $1 AND a.annotation_type = 'biography' AND a.is_stale = true",
    )
    .bind(repo_name)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

// ---------------------------------------------------------------------------
// Structured biography types — typed sections for future type_defined chunking
// ---------------------------------------------------------------------------

/// Structured biography with typed sections for future type_defined chunking.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructuredBiography {
    pub item_id: uuid::Uuid,
    pub qualified_name: String,
    pub item_type: String,
    pub file_path: String,
    pub repo_name: String,
    pub identity: IdentitySection,
    pub relationships: RelationshipsSection,
    pub characteristics: CharacteristicsSection,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IdentitySection {
    pub name: String,
    pub qualified_name: String,
    pub item_type: String,
    pub file_path: String,
    pub signature: Option<String>,
    pub doc_comment: Option<String>,
    pub parent: Option<ParentRef>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParentRef {
    pub name: String,
    pub parent_type: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RelationshipsSection {
    pub callers: Vec<String>,
    pub callees: Vec<String>,
    pub external_deps: Vec<String>,
    pub methods: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CharacteristicsSection {
    pub visibility: String,
    pub is_async: bool,
    pub loc: i32,
    pub caller_count: usize,
    pub callee_count: usize,
}

impl StructuredBiography {
    /// Serialize this biography to pretty-printed JSON.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("StructuredBiography is always serializable")
    }
}

/// Generate structured biographies for all public, non-test items in a repository.
///
/// Uses the same DB queries as `generate_biographies` but produces typed
/// `StructuredBiography` values with discrete sections instead of prose text.
pub async fn generate_structured_biographies(
    pool: &PgPool,
    repo_name: &str,
) -> Result<Vec<StructuredBiography>, sqlx::Error> {
    // Fetch all biography-eligible items (same query as generate_biographies)
    let items: Vec<BiographyRow> = sqlx::query_as(
        "SELECT i.id, i.name, i.qualified_name, i.item_type, i.visibility,
                i.signature, i.doc_comment, i.line_start, i.line_end, i.is_async,
                sf.file_path, r.name as repo_name,
                parent.name as parent_name, parent.item_type as parent_type
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         JOIN repositories r ON sf.repo_id = r.id
         LEFT JOIN items parent ON i.parent_item_id = parent.id
         WHERE r.name = $1
           AND i.item_type IN ('function', 'struct', 'enum', 'trait', 'impl')
           AND (i.is_test = false OR i.is_test IS NULL)
         ORDER BY sf.file_path, i.line_start",
    )
    .bind(repo_name)
    .fetch_all(pool)
    .await?;

    if items.is_empty() {
        return Ok(Vec::new());
    }

    let item_ids: Vec<Uuid> = items.iter().map(|i| i.id).collect();

    // Batch fetch callers
    let caller_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.target_item_id as item_id,
                COALESCE(src.qualified_name, src.name) as caller_name
         FROM edges e
         JOIN items src ON e.source_item_id = src.id
         WHERE e.target_item_id = ANY($1)
           AND e.edge_type = 'calls'",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Batch fetch callees
    let callee_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.source_item_id as item_id,
                COALESCE(tgt.qualified_name, tgt.name, e.target_name) as callee_name
         FROM edges e
         LEFT JOIN items tgt ON e.target_item_id = tgt.id
         WHERE e.source_item_id = ANY($1)
           AND e.edge_type IN ('calls', 'macro_invocation')",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Batch fetch external deps
    let dep_rows: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT e.source_item_id as item_id, e.target_name
         FROM edges e
         WHERE e.source_item_id = ANY($1)
           AND e.edge_type = 'external_dep'",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Batch fetch children
    let child_rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT parent_item_id as item_id, name, item_type
         FROM items
         WHERE parent_item_id = ANY($1)
         ORDER BY line_start",
    )
    .bind(&item_ids)
    .fetch_all(pool)
    .await?;

    // Build lookup maps
    let mut callers_map: std::collections::HashMap<Uuid, Vec<String>> =
        std::collections::HashMap::new();
    for (id, name) in &caller_rows {
        callers_map.entry(*id).or_default().push(name.clone());
    }

    let mut callees_map: std::collections::HashMap<Uuid, Vec<String>> =
        std::collections::HashMap::new();
    for (id, name) in &callee_rows {
        callees_map.entry(*id).or_default().push(name.clone());
    }

    let mut deps_map: std::collections::HashMap<Uuid, Vec<String>> =
        std::collections::HashMap::new();
    for (id, name) in &dep_rows {
        deps_map.entry(*id).or_default().push(name.clone());
    }

    let mut children_map: std::collections::HashMap<Uuid, Vec<(String, String)>> =
        std::collections::HashMap::new();
    for (id, name, itype) in &child_rows {
        children_map
            .entry(*id)
            .or_default()
            .push((name.clone(), itype.clone()));
    }

    // Assemble structured biographies
    let mut result = Vec::with_capacity(items.len());

    for item in &items {
        let qname = item
            .qualified_name
            .as_deref()
            .unwrap_or(&item.name);

        let loc = (item.line_end - item.line_start + 1).max(1);

        let item_callers = callers_map.get(&item.id);
        let item_callees = callees_map.get(&item.id);
        let item_deps = deps_map.get(&item.id);
        let item_children = children_map.get(&item.id);

        let caller_count = item_callers.map(|c| c.len()).unwrap_or(0);
        let callee_count = item_callees.map(|c| c.len()).unwrap_or(0);

        // Build identity section
        let parent = match (&item.parent_name, &item.parent_type) {
            (Some(pname), Some(ptype)) => Some(ParentRef {
                name: pname.clone(),
                parent_type: ptype.clone(),
            }),
            (Some(pname), None) => Some(ParentRef {
                name: pname.clone(),
                parent_type: "container".to_string(),
            }),
            _ => None,
        };

        let identity = IdentitySection {
            name: item.name.clone(),
            qualified_name: qname.to_string(),
            item_type: item.item_type.clone(),
            file_path: item.file_path.clone(),
            signature: item.signature.clone(),
            doc_comment: item.doc_comment.clone(),
            parent,
        };

        // Build relationships section
        let callers_deduped = item_callers
            .map(|c| dedup_names(c, 30).into_iter().map(String::from).collect())
            .unwrap_or_default();

        let callees_deduped = item_callees
            .map(|c| dedup_names(c, 30).into_iter().map(String::from).collect())
            .unwrap_or_default();

        let deps_deduped = item_deps
            .map(|d| dedup_names(d, 30).into_iter().map(String::from).collect())
            .unwrap_or_default();

        let methods: Vec<String> = if item.item_type == "impl" || item.item_type == "trait" {
            item_children
                .map(|ch| {
                    ch.iter()
                        .filter(|(_, t)| t == "function")
                        .map(|(n, _)| n.clone())
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };

        let relationships = RelationshipsSection {
            callers: callers_deduped,
            callees: callees_deduped,
            external_deps: deps_deduped,
            methods,
        };

        // Build characteristics section
        let characteristics = CharacteristicsSection {
            visibility: item.visibility.as_deref().unwrap_or("private").to_string(),
            is_async: item.is_async.unwrap_or(false),
            loc,
            caller_count,
            callee_count,
        };

        result.push(StructuredBiography {
            item_id: item.id,
            qualified_name: qname.to_string(),
            item_type: item.item_type.clone(),
            file_path: item.file_path.clone(),
            repo_name: item.repo_name.clone(),
            identity,
            relationships,
            characteristics,
        });
    }

    Ok(result)
}
