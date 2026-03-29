//! grepvec Context subcommand
//!
//! Returns the complete context package for a code item: biography,
//! graph neighborhood (callers, callees, external deps), and file metadata.

use clap::Args;
use colored::Colorize;
use uuid::Uuid;

#[derive(Args)]
pub struct ContextArgs {
    /// Item name or qualified name to look up
    pub query: String,

    /// Filter to a specific repository
    #[arg(short, long)]
    repo: Option<String>,

    /// Number of hops in the graph neighborhood (default: 1)
    #[arg(long, default_value = "1")]
    hops: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct ItemRow {
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
    file_git_sha: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
struct NeighborRow {
    name: String,
    qualified_name: Option<String>,
    item_type: String,
    file_path: String,
    line_start: i32,
}

#[derive(Debug, sqlx::FromRow)]
struct EdgeTargetRow {
    target_name: String,
    resolved_name: Option<String>,
    file_path: Option<String>,
    line_start: Option<i32>,
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
struct AnnotationRow {
    content: String,
    is_stale: Option<bool>,
}

pub async fn run(args: ContextArgs) {
    let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        eprintln!("{} TOWER_DB_URL not set", "Error:".red().bold());
        std::process::exit(1);
    }

    let pool = match crate::inventory::db::connect(&db_url).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    };

    // Find the item — try qualified_name first, then name
    let item: Option<ItemRow> = if let Some(ref repo) = args.repo {
        sqlx::query_as(
            "SELECT i.id, i.name, i.qualified_name, i.item_type, i.visibility,
                    i.signature, i.doc_comment, i.line_start, i.line_end, i.is_async,
                    sf.file_path, r.name as repo_name, sf.git_sha as file_git_sha
             FROM items i
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE r.name = $2
               AND (i.qualified_name = $1 OR i.name = $1
                    OR i.qualified_name LIKE '%' || $1)
             ORDER BY CASE
               WHEN i.qualified_name = $1 THEN 0
               WHEN i.name = $1 THEN 1
               ELSE 2
             END
             LIMIT 1",
        )
        .bind(&args.query)
        .bind(repo)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
    } else {
        sqlx::query_as(
            "SELECT i.id, i.name, i.qualified_name, i.item_type, i.visibility,
                    i.signature, i.doc_comment, i.line_start, i.line_end, i.is_async,
                    sf.file_path, r.name as repo_name, sf.git_sha as file_git_sha
             FROM items i
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE i.qualified_name = $1 OR i.name = $1
                   OR i.qualified_name LIKE '%' || $1
             ORDER BY CASE
               WHEN i.qualified_name = $1 THEN 0
               WHEN i.name = $1 THEN 1
               ELSE 2
             END
             LIMIT 1",
        )
        .bind(&args.query)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten()
    };

    let item = match item {
        Some(i) => i,
        None => {
            eprintln!("{} No item found matching \"{}\"", "Error:".red().bold(), args.query);
            std::process::exit(1);
        }
    };

    let qname = item.qualified_name.as_deref().unwrap_or(&item.name);
    let loc = (item.line_end - item.line_start + 1).max(1);

    // Print header
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
    println!(
        "  {} {} ({}:{}–{})",
        item.item_type.cyan().bold(),
        qname.bold(),
        item.file_path,
        item.line_start,
        item.line_end
    );
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );

    // Signature
    if let Some(ref sig) = item.signature {
        let sig_line = sig.lines().next().unwrap_or(sig);
        println!("  {}", sig_line);
    }

    // Doc comment
    if let Some(ref doc) = item.doc_comment {
        println!();
        for line in doc.lines().take(5) {
            println!("  {}", line);
        }
    }

    // Biography from annotations
    let bio: Option<AnnotationRow> = sqlx::query_as(
        "SELECT content, is_stale FROM annotations
         WHERE item_id = $1 AND annotation_type = 'biography'",
    )
    .bind(item.id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    if let Some(ref bio) = bio {
        if bio.is_stale == Some(true) {
            println!("\n{}", "  [BIOGRAPHY — STALE]".yellow().bold());
        }
    }

    // Status line
    let vis = item.visibility.as_deref().unwrap_or("private");
    let mut flags = vec![vis.to_string()];
    if item.is_async == Some(true) {
        flags.push("async".to_string());
    }
    flags.push(format!("{} LOC", loc));
    flags.push(format!("repo: {}", item.repo_name));
    println!("\n  Status: {}", flags.join(" | "));

    // === GRAPH NEIGHBORHOOD ===
    println!(
        "\n{}",
        "───────────────────────────────────────────────────────────────"
    );
    println!("{}", "  GRAPH NEIGHBORHOOD".cyan().bold());
    println!(
        "{}",
        "───────────────────────────────────────────────────────────────"
    );

    // Callers (who calls this item)
    let callers: Vec<NeighborRow> = sqlx::query_as(
        "SELECT DISTINCT caller.name, caller.qualified_name, caller.item_type,
                sf.file_path, caller.line_start
         FROM edges e
         JOIN items caller ON e.source_item_id = caller.id
         JOIN source_files sf ON caller.file_id = sf.id
         WHERE e.target_item_id = $1 AND e.edge_type = 'calls'
         ORDER BY sf.file_path, caller.line_start
         LIMIT 20",
    )
    .bind(item.id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    if !callers.is_empty() {
        println!("\n  {} ({}):", "Called by".green().bold(), callers.len());
        for c in &callers {
            let cname = c.qualified_name.as_deref().unwrap_or(&c.name);
            println!("    {} {} ({}:{})", "←".green(), cname, c.file_path, c.line_start);
        }
    }

    // Callees (what this item calls)
    let callees: Vec<EdgeTargetRow> = sqlx::query_as(
        "SELECT e.target_name,
                COALESCE(tgt.qualified_name, tgt.name) as resolved_name,
                sf.file_path, tgt.line_start
         FROM edges e
         LEFT JOIN items tgt ON e.target_item_id = tgt.id
         LEFT JOIN source_files sf ON tgt.file_id = sf.id
         WHERE e.source_item_id = $1
           AND e.edge_type IN ('calls', 'macro_invocation')
         ORDER BY e.target_name
         LIMIT 20",
    )
    .bind(item.id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    if !callees.is_empty() {
        println!("\n  {} ({}):", "Calls".green().bold(), callees.len());
        for c in &callees {
            let name = c.resolved_name.as_deref().unwrap_or(&c.target_name);
            if let (Some(ref fp), Some(ls)) = (&c.file_path, c.line_start) {
                println!("    {} {} ({}:{})", "→".green(), name, fp, ls);
            } else {
                println!("    {} {} (unresolved)", "→".yellow(), name);
            }
        }
    }

    // External dependencies
    let deps: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT e.target_name
         FROM edges e
         WHERE e.source_item_id = $1 AND e.edge_type = 'external_dep'",
    )
    .bind(item.id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    if !deps.is_empty() {
        println!(
            "\n  {} {}",
            "Touches:".cyan().bold(),
            deps.iter().map(|(n,)| n.as_str()).collect::<Vec<_>>().join(", ")
        );
    }

    // Boundary nodes (inferred layer)
    let boundary: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT bn.name, bn.category, bn.failure_impact
         FROM edges e
         JOIN boundary_nodes bn ON e.boundary_node_id = bn.id
         WHERE e.source_item_id = $1",
    )
    .bind(item.id)
    .fetch_all(&pool)
    .await
    .unwrap_or_default();

    if !boundary.is_empty() {
        println!("\n  {} (inferred layer):", "Boundary Deps".cyan().bold());
        for (name, category, impact) in &boundary {
            print!("    ◆ {} ({})", name.bold(), category);
            if let Some(impact) = impact {
                print!(" — {}", impact.yellow());
            }
            println!();
        }
    }

    // File context
    println!(
        "\n{}",
        "───────────────────────────────────────────────────────────────"
    );
    println!("{}", "  FILE CONTEXT".cyan().bold());
    println!(
        "───────────────────────────────────────────────────────────────"
    );

    // Count items in the same file
    let file_stats: Option<(i64, i64)> = sqlx::query_as(
        "SELECT COUNT(*) as item_count,
                SUM(CASE WHEN item_type = 'function' THEN 1 ELSE 0 END) as fn_count
         FROM items WHERE file_id = (
           SELECT file_id FROM items WHERE id = $1
         )",
    )
    .bind(item.id)
    .fetch_optional(&pool)
    .await
    .ok()
    .flatten();

    if let Some((items, fns)) = file_stats {
        println!(
            "  {}: {} items ({} functions)",
            item.file_path, items, fns
        );
    }
    if let Some(ref sha) = item.file_git_sha {
        println!("  Last SHA: {}", &sha[..sha.len().min(8)]);
    }

    println!(
        "\n{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
}
