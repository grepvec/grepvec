//! Database layer for the absorption engine.
//!
//! Uses sqlx to connect to the Neon.tech Postgres database and upsert
//! repositories, source files, items, edges, and parse run records.

use crate::inventory::{AbsorptionResult, ExtractedEdge, ExtractedItem};
use sqlx::PgPool;
use uuid::Uuid;

/// Connect to the Postgres database.
pub async fn connect(db_url: &str) -> Result<PgPool, sqlx::Error> {
    PgPool::connect(db_url).await
}

/// Introspect the database schema and return table/column info.
pub async fn introspect_schema(pool: &PgPool) -> Result<Vec<TableInfo>, sqlx::Error> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT table_name, column_name, data_type, is_nullable
         FROM information_schema.columns
         WHERE table_schema = 'public'
         ORDER BY table_name, ordinal_position",
    )
    .fetch_all(pool)
    .await?;

    let mut tables: Vec<TableInfo> = Vec::new();
    let mut current_table = String::new();

    for (table, col, dtype, nullable) in rows {
        if table != current_table {
            tables.push(TableInfo {
                name: table.clone(),
                columns: Vec::new(),
            });
            current_table = table;
        }
        if let Some(t) = tables.last_mut() {
            t.columns.push(ColumnInfo {
                name: col,
                data_type: dtype,
                nullable: nullable == "YES",
            });
        }
    }

    Ok(tables)
}

#[derive(Debug)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<ColumnInfo>,
}

#[derive(Debug)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
    pub nullable: bool,
}

/// A single versioned migration.
struct Migration {
    version: i32,
    description: &'static str,
    sql: &'static str,
}

/// All migrations in order. Each migration's SQL must be idempotent.
fn all_migrations() -> Vec<Migration> {
    vec![
        Migration {
            version: 1,
            description: "Create base tables: repositories, source_files, items, edges, annotations, boundary_nodes, parse_runs",
            sql: r#"
                CREATE TABLE IF NOT EXISTS repositories (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    name TEXT NOT NULL UNIQUE,
                    path TEXT NOT NULL,
                    primary_language TEXT NOT NULL DEFAULT 'rust',
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    updated_at TIMESTAMPTZ DEFAULT NOW()
                );

                CREATE TABLE IF NOT EXISTS parse_runs (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    repo_id UUID NOT NULL REFERENCES repositories(id),
                    git_sha TEXT NOT NULL,
                    status TEXT NOT NULL DEFAULT 'running',
                    started_at TIMESTAMPTZ DEFAULT NOW(),
                    completed_at TIMESTAMPTZ,
                    files_parsed INTEGER DEFAULT 0,
                    items_found INTEGER DEFAULT 0,
                    edges_found INTEGER DEFAULT 0
                );

                CREATE TABLE IF NOT EXISTS source_files (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    repo_id UUID NOT NULL REFERENCES repositories(id),
                    file_path TEXT NOT NULL,
                    language TEXT NOT NULL DEFAULT 'rust',
                    git_sha TEXT,
                    line_count INTEGER DEFAULT 0,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    updated_at TIMESTAMPTZ DEFAULT NOW()
                );

                CREATE TABLE IF NOT EXISTS items (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    file_id UUID NOT NULL REFERENCES source_files(id),
                    parent_item_id UUID REFERENCES items(id),
                    item_type TEXT NOT NULL,
                    name TEXT NOT NULL,
                    qualified_name TEXT,
                    visibility TEXT NOT NULL DEFAULT 'public',
                    signature TEXT,
                    doc_comment TEXT,
                    line_start INTEGER NOT NULL,
                    line_end INTEGER NOT NULL,
                    git_sha TEXT,
                    is_test BOOLEAN DEFAULT false,
                    is_async BOOLEAN DEFAULT false,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    updated_at TIMESTAMPTZ DEFAULT NOW()
                );

                CREATE TABLE IF NOT EXISTS edges (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    source_item_id UUID REFERENCES items(id),
                    target_item_id UUID REFERENCES items(id),
                    edge_type TEXT NOT NULL,
                    target_name TEXT,
                    confidence FLOAT DEFAULT 1.0,
                    boundary_node_id UUID,
                    created_at TIMESTAMPTZ DEFAULT NOW()
                );

                CREATE TABLE IF NOT EXISTS annotations (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    item_id UUID NOT NULL REFERENCES items(id),
                    annotation_type TEXT NOT NULL DEFAULT 'biography',
                    content TEXT NOT NULL,
                    author TEXT DEFAULT 'grepvec-absorb',
                    git_sha TEXT,
                    is_stale BOOLEAN DEFAULT false,
                    created_at TIMESTAMPTZ DEFAULT NOW(),
                    updated_at TIMESTAMPTZ DEFAULT NOW()
                );

                CREATE TABLE IF NOT EXISTS boundary_nodes (
                    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
                    name TEXT NOT NULL,
                    crate_name TEXT,
                    category TEXT,
                    apis_used TEXT,
                    configuration TEXT,
                    failure_impact TEXT,
                    confidence FLOAT DEFAULT 0.5,
                    agent_id TEXT,
                    created_at TIMESTAMPTZ DEFAULT NOW()
                );
            "#,
        },
        Migration {
            version: 2,
            description: "Indexes and constraints on base tables",
            sql: r#"
                -- items: unique on (file_id, name, item_type, line_start) for upsert
                CREATE UNIQUE INDEX IF NOT EXISTS uq_items_file_name_type_line
                    ON items (file_id, name, item_type, line_start);

                -- edges: deduplicate before creating constraint
                DELETE FROM edges a USING edges b
                    WHERE a.id > b.id
                      AND a.source_item_id = b.source_item_id
                      AND a.edge_type = b.edge_type
                      AND a.target_name IS NOT DISTINCT FROM b.target_name;

                -- edges: unique on (source_item_id, edge_type, target_name) for idempotent inserts
                CREATE UNIQUE INDEX IF NOT EXISTS uq_edges_source_type_target
                    ON edges (source_item_id, edge_type, COALESCE(target_name, ''));

                -- source_files: unique on (repo_id, file_path) for upsert
                CREATE UNIQUE INDEX IF NOT EXISTS uq_source_files_repo_path
                    ON source_files (repo_id, file_path);

                -- parse_runs: add completed_at column if missing
                ALTER TABLE parse_runs ADD COLUMN IF NOT EXISTS completed_at TIMESTAMPTZ;

                -- source_files: add line_count column if missing
                ALTER TABLE source_files ADD COLUMN IF NOT EXISTS line_count INTEGER DEFAULT 0;
            "#,
        },
        Migration {
            version: 3,
            description: "Add tsvector search column to annotations",
            sql: r#"
                -- annotations: add tsvector column for full-text search ranking
                ALTER TABLE annotations ADD COLUMN IF NOT EXISTS
                    search_vector tsvector
                    GENERATED ALWAYS AS (to_tsvector('english', content)) STORED;

                -- annotations: GIN index on tsvector for fast full-text search
                CREATE INDEX IF NOT EXISTS idx_annotations_search_vector
                    ON annotations USING gin(search_vector);
            "#,
        },
    ]
}

/// Get the current schema version from the database.
/// Returns 0 if the schema_versions table doesn't exist or is empty.
pub async fn get_schema_version(pool: &PgPool) -> Result<i32, sqlx::Error> {
    // Check if the schema_versions table exists
    let exists: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM information_schema.tables
         WHERE table_schema = 'public' AND table_name = 'schema_versions'",
    )
    .fetch_optional(pool)
    .await?;

    match exists {
        Some((count,)) if count > 0 => {}
        _ => return Ok(0),
    }

    // Get the max version
    let row: Option<(Option<i32>,)> =
        sqlx::query_as("SELECT MAX(version) FROM schema_versions")
            .fetch_optional(pool)
            .await?;

    Ok(row.and_then(|(v,)| v).unwrap_or(0))
}

/// Run all pending migrations above the current schema version.
/// Creates the schema_versions table if it doesn't exist.
/// Returns (previous_version, new_version, migrations_applied).
pub async fn run_migrations(pool: &PgPool) -> Result<(i32, i32, usize), sqlx::Error> {
    // Create the schema_versions table if needed
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_versions (
            version INTEGER PRIMARY KEY,
            description TEXT NOT NULL,
            applied_at TIMESTAMPTZ DEFAULT NOW()
        )",
    )
    .execute(pool)
    .await?;

    let current_version = get_schema_version(pool).await?;
    let migrations = all_migrations();
    let mut applied = 0usize;

    for migration in &migrations {
        if migration.version <= current_version {
            continue;
        }

        // Apply the migration SQL
        // Split on semicolons and execute each statement individually,
        // since sqlx doesn't support multi-statement queries reliably.
        for statement in migration.sql.split(';') {
            let trimmed = statement.trim();
            if trimmed.is_empty() {
                continue;
            }
            sqlx::query(trimmed).execute(pool).await.ok();
        }

        // Record the migration
        sqlx::query(
            "INSERT INTO schema_versions (version, description)
             VALUES ($1, $2)
             ON CONFLICT (version) DO NOTHING",
        )
        .bind(migration.version)
        .bind(migration.description)
        .execute(pool)
        .await?;

        applied += 1;
    }

    let new_version = if applied > 0 {
        get_schema_version(pool).await?
    } else {
        current_version
    };

    Ok((current_version, new_version, applied))
}

/// Ensure required constraints and schema are up to date.
/// Delegates to the versioned migration system.
/// Idempotent -- safe to call multiple times.
pub async fn ensure_constraints(pool: &PgPool) -> Result<(), sqlx::Error> {
    let (prev, new, applied) = run_migrations(pool).await?;
    if applied > 0 {
        eprintln!(
            "Schema migrated: v{} -> v{} ({} migration{} applied)",
            prev,
            new,
            applied,
            if applied == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// Upsert a repository record. Returns the repository UUID.
pub async fn upsert_repository(
    pool: &PgPool,
    name: &str,
    path: &str,
    primary_language: &str,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO repositories (name, path, primary_language)
         VALUES ($1, $2, $3)
         ON CONFLICT (name) DO UPDATE
           SET path = EXCLUDED.path,
               primary_language = EXCLUDED.primary_language,
               updated_at = NOW()
         RETURNING id",
    )
    .bind(name)
    .bind(path)
    .bind(primary_language)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Create a parse run record. Returns the parse run UUID.
pub async fn create_parse_run(
    pool: &PgPool,
    repo_id: Uuid,
    git_sha: &str,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO parse_runs (repo_id, git_sha, status, started_at)
         VALUES ($1, $2, 'running', NOW())
         RETURNING id",
    )
    .bind(repo_id)
    .bind(git_sha)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Complete a parse run with final counts.
pub async fn complete_parse_run(
    pool: &PgPool,
    run_id: Uuid,
    files_parsed: i32,
    items_found: i32,
    status: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE parse_runs
         SET status = $1, files_parsed = $2, items_found = $3,
             completed_at = NOW()
         WHERE id = $4",
    )
    .bind(status)
    .bind(files_parsed)
    .bind(items_found)
    .bind(run_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Upsert a source file record. Returns the file UUID.
/// If the file's git_sha changed, marks all associated annotations as stale.
pub async fn upsert_source_file(
    pool: &PgPool,
    repo_id: Uuid,
    file_path: &str,
    language: &str,
    line_count: i32,
    git_sha: &str,
) -> Result<Uuid, sqlx::Error> {
    // Check if the file exists and its SHA changed → mark annotations stale
    let existing: Option<(Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, git_sha FROM source_files WHERE repo_id = $1 AND file_path = $2",
    )
    .bind(repo_id)
    .bind(file_path)
    .fetch_optional(pool)
    .await?;

    if let Some((file_id, old_sha)) = &existing {
        if old_sha.as_deref() != Some(git_sha) {
            // SHA changed — mark annotations stale
            sqlx::query(
                "UPDATE annotations SET is_stale = true, updated_at = NOW()
                 WHERE is_stale = false
                   AND item_id IN (SELECT id FROM items WHERE file_id = $1)",
            )
            .bind(file_id)
            .execute(pool)
            .await
            .ok();
        }
    }

    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO source_files (repo_id, file_path, language, line_count, git_sha)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (repo_id, file_path) DO UPDATE
           SET language = EXCLUDED.language,
               line_count = EXCLUDED.line_count,
               git_sha = EXCLUDED.git_sha,
               updated_at = NOW()
         RETURNING id",
    )
    .bind(repo_id)
    .bind(file_path)
    .bind(language)
    .bind(line_count)
    .bind(git_sha)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Upsert an item record. Returns the item UUID.
pub async fn upsert_item(
    pool: &PgPool,
    file_id: Uuid,
    parent_item_id: Option<Uuid>,
    item: &ExtractedItem,
    git_sha: &str,
) -> Result<Uuid, sqlx::Error> {
    let row: (Uuid,) = sqlx::query_as(
        "INSERT INTO items (
            file_id, parent_item_id, item_type, name, qualified_name,
            visibility, signature, doc_comment, line_start, line_end,
            git_sha, is_test, is_async, attributes
         )
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         ON CONFLICT (file_id, name, item_type, line_start) DO UPDATE
           SET parent_item_id = EXCLUDED.parent_item_id,
               qualified_name = EXCLUDED.qualified_name,
               visibility = EXCLUDED.visibility,
               signature = EXCLUDED.signature,
               doc_comment = EXCLUDED.doc_comment,
               line_end = EXCLUDED.line_end,
               git_sha = EXCLUDED.git_sha,
               is_test = EXCLUDED.is_test,
               is_async = EXCLUDED.is_async,
               attributes = EXCLUDED.attributes,
               updated_at = NOW()
         RETURNING id",
    )
    .bind(file_id)
    .bind(parent_item_id)
    .bind(item.item_type.as_str())
    .bind(&item.name)
    .bind(&item.qualified_name)
    .bind(item.visibility.as_str())
    .bind(&item.signature)
    .bind(&item.doc_comment)
    .bind(item.line_start as i32)
    .bind(item.line_end as i32)
    .bind(git_sha)
    .bind(item.is_test)
    .bind(item.is_async)
    .bind(&item.attributes)
    .fetch_one(pool)
    .await?;

    Ok(row.0)
}

/// Insert an edge record.
pub async fn insert_edge(
    pool: &PgPool,
    source_item_id: Uuid,
    target_item_id: Option<Uuid>,
    edge: &ExtractedEdge,
    confidence: f64,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO edges (source_item_id, target_item_id, edge_type, target_name, confidence)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT DO NOTHING",
    )
    .bind(source_item_id)
    .bind(target_item_id)
    .bind(edge.edge_type.as_str())
    .bind(&edge.target_name)
    .bind(confidence)
    .execute(pool)
    .await?;

    Ok(())
}

/// Store an entire absorption result in the database using batch inserts.
pub async fn store_absorption(
    pool: &PgPool,
    result: &AbsorptionResult,
    repo_path: &str,
    git_sha: &str,
) -> Result<AbsorptionDbStats, sqlx::Error> {
    let mut stats = AbsorptionDbStats::default();

    // Upsert repository
    let primary_lang = result
        .files
        .first()
        .map(|f| f.language.as_str())
        .unwrap_or("rust");
    let repo_id = upsert_repository(pool, &result.repo_name, repo_path, primary_lang).await?;

    // Clear old edges for this repo (so noise filter takes effect on re-absorption)
    sqlx::query(
        "DELETE FROM edges WHERE source_item_id IN (
            SELECT i.id FROM items i
            JOIN source_files sf ON i.file_id = sf.id
            WHERE sf.repo_id = $1
         )",
    )
    .bind(repo_id)
    .execute(pool)
    .await
    .ok();

    // Create parse run
    let run_id = create_parse_run(pool, repo_id, git_sha).await?;

    // Process each file with batch inserts
    for file in &result.files {
        let file_id = upsert_source_file(
            pool,
            repo_id,
            &file.path,
            &file.language,
            file.line_count as i32,
            git_sha,
        )
        .await?;
        stats.files += 1;

        if file.items.is_empty() {
            continue;
        }

        // Batch insert all items for this file using UNNEST
        let n = file.items.len();
        let file_ids: Vec<Uuid> = vec![file_id; n];
        let item_types: Vec<String> = file.items.iter().map(|i| i.item_type.as_str().to_string()).collect();
        let names: Vec<String> = file.items.iter().map(|i| i.name.clone()).collect();
        let qualified_names: Vec<Option<String>> = file.items.iter().map(|i| i.qualified_name.clone()).collect();
        let visibilities: Vec<String> = file.items.iter().map(|i| i.visibility.as_str().to_string()).collect();
        let signatures: Vec<Option<String>> = file.items.iter().map(|i| i.signature.clone()).collect();
        let doc_comments: Vec<Option<String>> = file.items.iter().map(|i| i.doc_comment.clone()).collect();
        let line_starts: Vec<i32> = file.items.iter().map(|i| i.line_start as i32).collect();
        let line_ends: Vec<i32> = file.items.iter().map(|i| i.line_end as i32).collect();
        let git_shas: Vec<String> = vec![git_sha.to_string(); n];
        let is_tests: Vec<bool> = file.items.iter().map(|i| i.is_test).collect();
        let is_asyncs: Vec<bool> = file.items.iter().map(|i| i.is_async).collect();

        let item_rows: Vec<(Uuid, i32)> = match sqlx::query_as(
            "INSERT INTO items (file_id, item_type, name, qualified_name, visibility,
                               signature, doc_comment, line_start, line_end, git_sha,
                               is_test, is_async)
             SELECT * FROM UNNEST(
                $1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[],
                $6::text[], $7::text[], $8::int4[], $9::int4[], $10::text[],
                $11::bool[], $12::bool[]
             )
             ON CONFLICT (file_id, name, item_type, line_start) DO UPDATE
             SET qualified_name = EXCLUDED.qualified_name,
                 visibility = EXCLUDED.visibility,
                 signature = EXCLUDED.signature,
                 doc_comment = EXCLUDED.doc_comment,
                 line_end = EXCLUDED.line_end,
                 git_sha = EXCLUDED.git_sha,
                 is_test = EXCLUDED.is_test,
                 is_async = EXCLUDED.is_async,
                 updated_at = NOW()
             RETURNING id, line_start",
        )
        .bind(&file_ids)
        .bind(&item_types)
        .bind(&names)
        .bind(&qualified_names)
        .bind(&visibilities)
        .bind(&signatures)
        .bind(&doc_comments)
        .bind(&line_starts)
        .bind(&line_ends)
        .bind(&git_shas)
        .bind(&is_tests)
        .bind(&is_asyncs)
        .fetch_all(pool)
        .await
        {
            Ok(rows) => {
                stats.items += rows.len();
                rows
            }
            Err(e) => {
                eprintln!(
                    "Warning: batch item insert failed for {}: {}",
                    file.path, e
                );
                continue;
            }
        };

        // --- Stale item cleanup ---
        // Delete any items in this file that were NOT just upserted.
        // This handles code movement (line_start changed), renames, and deletions.
        // Without this, old items become ghosts: edges point at them but
        // biographies and grepvec_context find the new item (with no edges).
        let current_ids: Vec<Uuid> = item_rows.iter().map(|&(u, _)| u).collect();
        if !current_ids.is_empty() {
            // Delete stale edges first (both directions), then stale items and annotations
            let deleted = sqlx::query(
                "WITH stale AS (
                    SELECT id FROM items
                    WHERE file_id = $1 AND id != ALL($2::uuid[])
                )
                , del_edges AS (
                    DELETE FROM edges
                    WHERE source_item_id IN (SELECT id FROM stale)
                       OR target_item_id IN (SELECT id FROM stale)
                )
                , del_annotations AS (
                    DELETE FROM annotations
                    WHERE item_id IN (SELECT id FROM stale)
                )
                DELETE FROM items WHERE id IN (SELECT id FROM stale)",
            )
            .bind(file_id)
            .bind(&current_ids)
            .execute(pool)
            .await;

            if let Ok(result) = deleted {
                let n = result.rows_affected();
                if n > 0 {
                    stats.stale_items_cleaned += n as usize;
                }
            }
        }

        // Also clean stale edges from this file's current items — edges from
        // a previous parse run that no longer exist in the current parse.
        // This ensures edge set matches the current code exactly.
        if !current_ids.is_empty() {
            sqlx::query(
                "DELETE FROM edges
                 WHERE source_item_id = ANY($1::uuid[])
                   AND edge_type IN ('calls', 'implements', 'contains', 'uses_type', 'macro_invocation')
                   AND source_item_id IS NOT NULL",
            )
            .bind(&current_ids)
            .execute(pool)
            .await
            .ok();
        }

        // Build line_start → UUID map for edge resolution
        let mut item_uuid_map: std::collections::HashMap<i32, Uuid> = std::collections::HashMap::new();
        for (uuid, line_start) in &item_rows {
            item_uuid_map.insert(*line_start, *uuid);
        }

        // Build name → UUID map (first match) and index → UUID map
        let mut name_uuid_map: std::collections::HashMap<&str, Uuid> = std::collections::HashMap::new();
        let mut idx_uuid_map: std::collections::HashMap<usize, Uuid> = std::collections::HashMap::new();
        for (idx, item) in file.items.iter().enumerate() {
            let uuid = item_rows.get(idx)
                .map(|&(u, _)| u)
                .or_else(|| item_uuid_map.get(&(item.line_start as i32)).copied());
            if let Some(uuid) = uuid {
                name_uuid_map.entry(item.name.as_str()).or_insert(uuid);
                idx_uuid_map.insert(idx, uuid);
            }
        }

        // --- Fix #1: Update parent_item_id and attributes ---
        let mut update_ids: Vec<Uuid> = Vec::new();
        let mut update_parent_ids: Vec<Option<Uuid>> = Vec::new();
        let mut update_attrs: Vec<String> = Vec::new(); // JSON-serialized attributes

        for (idx, item) in file.items.iter().enumerate() {
            let uuid = match idx_uuid_map.get(&idx) {
                Some(&u) => u,
                None => continue,
            };
            // Find parent UUID
            let parent_uuid = file.items.iter().enumerate().find_map(|(pidx, p)| {
                if p.child_indices.contains(&idx) {
                    idx_uuid_map.get(&pidx).copied()
                } else {
                    None
                }
            });
            // Only update if there's a parent or attributes to set
            if parent_uuid.is_some() || !item.attributes.is_empty() {
                update_ids.push(uuid);
                update_parent_ids.push(parent_uuid);
                update_attrs.push(
                    serde_json::to_string(&item.attributes).unwrap_or_else(|_| "[]".to_string())
                );
            }
        }

        if !update_ids.is_empty() {
            // Batch update parent_item_id and attributes
            // Use attributes as text (JSON) since TEXT[][] UNNEST is complex
            sqlx::query(
                "UPDATE items SET
                    parent_item_id = u.parent_id,
                    attributes = CASE WHEN u.attrs = '[]' THEN items.attributes ELSE string_to_array(trim(both '[]' from replace(replace(u.attrs, '\"', ''), ',', ',')), ',') END
                 FROM (SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::text[])) AS u(item_id, parent_id, attrs)
                 WHERE items.id = u.item_id",
            )
            .bind(&update_ids)
            .bind(&update_parent_ids)
            .bind(&update_attrs)
            .execute(pool)
            .await
            .ok(); // non-fatal if attributes parse fails
        }

        // --- Batch insert edges ---
        if !file.edges.is_empty() {
            let mut edge_source_ids: Vec<Uuid> = Vec::new();
            let mut edge_target_ids: Vec<Option<Uuid>> = Vec::new();
            let mut edge_types: Vec<String> = Vec::new();
            let mut edge_target_names: Vec<String> = Vec::new();
            let mut edge_confidences: Vec<f32> = Vec::new();

            for edge in &file.edges {
                let source_uuid = name_uuid_map.get(edge.source_item_name.as_str()).copied();
                let target_uuid = name_uuid_map.get(edge.target_name.as_str()).copied();

                if let Some(source_id) = source_uuid {
                    edge_source_ids.push(source_id);
                    edge_target_ids.push(target_uuid);
                    edge_types.push(edge.edge_type.as_str().to_string());
                    edge_target_names.push(edge.target_name.clone());
                    edge_confidences.push(if target_uuid.is_some() { 1.0 } else { 0.5 });
                }
            }

            if !edge_source_ids.is_empty() {
                match sqlx::query(
                    "INSERT INTO edges (source_item_id, target_item_id, edge_type, target_name, confidence)
                     SELECT * FROM UNNEST(
                        $1::uuid[], $2::uuid[], $3::text[], $4::text[], $5::real[]
                     )
                     ON CONFLICT (source_item_id, edge_type, COALESCE(target_name, ''))
                     DO NOTHING",
                )
                .bind(&edge_source_ids)
                .bind(&edge_target_ids)
                .bind(&edge_types)
                .bind(&edge_target_names)
                .bind(&edge_confidences)
                .execute(pool)
                .await
                {
                    Ok(result) => {
                        stats.edges += result.rows_affected() as usize;
                    }
                    Err(e) => {
                        eprintln!(
                            "Warning: batch edge insert failed for {}: {}",
                            file.path, e
                        );
                    }
                }
            }
        }
    }

    // --- Intra-repo edge resolution ---
    // Pass 1: exact name match (e.g., target_name "log_event" = item name "log_event")
    let intra_exact = sqlx::query(
        "UPDATE edges e
         SET target_item_id = i.id,
             confidence = 0.7
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         WHERE sf.repo_id = $1
           AND e.target_item_id IS NULL
           AND e.target_name = i.name
           AND i.item_type IN ('function', 'struct', 'enum', 'trait', 'constant', 'static', 'type_alias', 'class')
           AND e.source_item_id IN (
             SELECT i2.id FROM items i2
             JOIN source_files sf2 ON i2.file_id = sf2.id
             WHERE sf2.repo_id = $1
           )",
    )
    .bind(repo_id)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);

    // Pass 2: qualified target → last segment match
    // e.g., target_name "audit::log_event" → match item name "log_event"
    let intra_qualified = sqlx::query(
        "UPDATE edges e
         SET target_item_id = i.id,
             confidence = 0.6
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         WHERE sf.repo_id = $1
           AND e.target_item_id IS NULL
           AND e.target_name LIKE '%::%'
           AND i.name = split_part(e.target_name, '::', -1)
           AND i.item_type IN ('function', 'struct', 'enum', 'trait', 'constant', 'static', 'type_alias', 'class')
           AND e.source_item_id IN (
             SELECT i2.id FROM items i2
             JOIN source_files sf2 ON i2.file_id = sf2.id
             WHERE sf2.repo_id = $1
           )",
    )
    .bind(repo_id)
    .execute(pool)
    .await
    .map(|r| r.rows_affected())
    .unwrap_or(0);

    let intra_resolved = intra_exact + intra_qualified;

    if intra_resolved > 0 {
        stats.intra_resolved = intra_resolved as usize;
    }

    // Complete parse run
    complete_parse_run(
        pool,
        run_id,
        stats.files as i32,
        stats.items as i32,
        "completed",
    )
    .await?;

    Ok(stats)
}

/// Run cross-repo edge reconciliation.
///
/// Resolves edges that have a target_name but no target_item_id by
/// matching against items in other repositories.
pub async fn reconcile_edges(pool: &PgPool) -> Result<ReconcileStats, sqlx::Error> {
    // Resolve unresolved cross-repo edges
    let result = sqlx::query(
        "UPDATE edges e
         SET target_item_id = i.id,
             confidence = 0.8
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND e.target_name = i.name
           AND sf.repo_id != (
             SELECT sf2.repo_id FROM source_files sf2
             JOIN items i2 ON i2.file_id = sf2.id
             WHERE i2.id = e.source_item_id
           )",
    )
    .execute(pool)
    .await?;

    Ok(ReconcileStats {
        edges_resolved: result.rows_affected() as usize,
    })
}

/// Statistics from storing an absorption result.
#[derive(Debug, Default)]
pub struct AbsorptionDbStats {
    pub files: usize,
    pub items: usize,
    pub edges: usize,
    pub intra_resolved: usize,
    pub ext_dep_edges: usize,
    pub stale_items_cleaned: usize,
}

/// Store external dependency edges for a repo.
/// Links detected external deps to the external_dependencies table.
pub async fn store_external_dep_edges(
    pool: &PgPool,
    repo_id: Uuid,
    ext_edges: &[crate::inventory::ExtractedEdge],
) -> Result<usize, sqlx::Error> {
    if ext_edges.is_empty() {
        return Ok(0);
    }

    // For each external dep edge, find the source item UUID and the external dep UUID
    let mut stored = 0usize;
    for edge in ext_edges {
        // Find source item by name within this repo
        let source: Option<(Uuid,)> = sqlx::query_as(
            "SELECT i.id FROM items i
             JOIN source_files sf ON i.file_id = sf.id
             WHERE sf.repo_id = $1 AND i.name = $2
             LIMIT 1",
        )
        .bind(repo_id)
        .bind(&edge.source_item_name)
        .fetch_optional(pool)
        .await?;

        let source_id = match source {
            Some((id,)) => id,
            None => continue,
        };

        // Find external dependency by name
        let ext_dep: Option<(Uuid,)> = sqlx::query_as(
            "SELECT id FROM external_dependencies WHERE name = $1",
        )
        .bind(&edge.target_name)
        .fetch_optional(pool)
        .await?;

        let target_dep_id = ext_dep.map(|(id,)| id);

        // Insert edge with target_dep_id
        let result = sqlx::query(
            "INSERT INTO edges (source_item_id, target_dep_id, edge_type, target_name, confidence)
             VALUES ($1, $2, 'external_dep', $3, 0.9)
             ON CONFLICT (source_item_id, edge_type, COALESCE(target_name, ''))
             DO UPDATE SET target_dep_id = EXCLUDED.target_dep_id, confidence = EXCLUDED.confidence",
        )
        .bind(source_id)
        .bind(target_dep_id)
        .bind(&edge.target_name)
        .execute(pool)
        .await;

        if result.is_ok() {
            stored += 1;
        }
    }

    Ok(stored)
}

/// Store git history for source files in a repo.
pub async fn store_git_history(
    pool: &PgPool,
    repo_id: Uuid,
    repo_path: &std::path::Path,
) -> Result<usize, sqlx::Error> {
    // Get all source files for this repo
    let files: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT id, file_path FROM source_files WHERE repo_id = $1",
    )
    .bind(repo_id)
    .fetch_all(pool)
    .await?;

    let mut updated = 0usize;

    for (file_id, file_path) in &files {
        // Run git log for this file
        let output = std::process::Command::new("git")
            .args([
                "log", "--format=%H|%aI|%s",
                "--follow", "-20", // last 20 commits
                "--", file_path,
            ])
            .current_dir(repo_path)
            .output();

        let history = match output {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => continue,
        };

        if history.is_empty() {
            continue;
        }

        // Parse into JSON array
        let entries: Vec<serde_json::Value> = history
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.splitn(3, '|').collect();
                if parts.len() == 3 {
                    Some(serde_json::json!({
                        "sha": parts[0],
                        "date": parts[1],
                        "message": parts[2]
                    }))
                } else {
                    None
                }
            })
            .collect();

        if entries.is_empty() {
            continue;
        }

        // Store latest git SHA for this file
        sqlx::query(
            "UPDATE source_files SET git_sha = $1 WHERE id = $2",
        )
        .bind(entries.first()
            .and_then(|e| e["sha"].as_str())
            .unwrap_or(""))
        .bind(file_id)
        .execute(pool)
        .await
        .ok();

        updated += 1;
    }

    Ok(updated)
}

/// Statistics from edge reconciliation.
#[derive(Debug)]
pub struct ReconcileStats {
    pub edges_resolved: usize,
}

/// Get platform-wide statistics from the database.
pub async fn get_stats(pool: &PgPool) -> Result<PlatformStats, sqlx::Error> {
    let repos: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM repositories")
        .fetch_one(pool)
        .await?;

    let files: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM source_files")
        .fetch_one(pool)
        .await?;

    let items: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM items")
        .fetch_one(pool)
        .await?;

    let edges: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM edges")
        .fetch_one(pool)
        .await?;

    let resolved_edges: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM edges WHERE target_item_id IS NOT NULL",
    )
    .fetch_one(pool)
    .await?;

    let unresolved_edges: (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM edges WHERE target_item_id IS NULL AND target_dep_id IS NULL",
    )
    .fetch_one(pool)
    .await?;

    Ok(PlatformStats {
        repositories: repos.0 as usize,
        source_files: files.0 as usize,
        items: items.0 as usize,
        edges: edges.0 as usize,
        resolved_edges: resolved_edges.0 as usize,
        unresolved_edges: unresolved_edges.0 as usize,
    })
}

/// Platform-wide statistics.
#[derive(Debug)]
pub struct PlatformStats {
    pub repositories: usize,
    pub source_files: usize,
    pub items: usize,
    pub edges: usize,
    pub resolved_edges: usize,
    pub unresolved_edges: usize,
}

impl std::fmt::Display for PlatformStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  Repositories:      {}", self.repositories)?;
        writeln!(f, "  Source files:      {}", self.source_files)?;
        writeln!(f, "  Items:             {}", self.items)?;
        writeln!(f, "  Edges:             {}", self.edges)?;
        writeln!(f, "  Resolved edges:    {}", self.resolved_edges)?;
        writeln!(f, "  Unresolved edges:  {}", self.unresolved_edges)
    }
}
