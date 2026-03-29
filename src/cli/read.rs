//! grepvec Read subcommand
//!
//! Fetches the precise source code for a code item by name.
//! Uses the inventory (file path + line range) to extract exactly
//! the relevant lines — no filesystem browsing, no grep, no noise.
//!
//! Usage:
//!   grepvec read "api::ingest::ingest_documents"
//!   grepvec read "QdrantStorage::new" --repo enscribe-embed

use clap::Args;
use colored::Colorize;
use uuid::Uuid;

#[derive(Args)]
pub struct ReadArgs {
    /// Item name or qualified name to read
    pub query: String,

    /// Filter to a specific repository
    #[arg(short, long)]
    repo: Option<String>,

    /// Show N lines of surrounding context (before and after)
    #[arg(short = 'C', long, default_value = "0")]
    context: i32,
}

#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
struct ItemRow {
    id: Uuid,
    name: String,
    qualified_name: Option<String>,
    item_type: String,
    line_start: i32,
    line_end: i32,
    file_path: String,
    repo_name: String,
}

pub async fn run(args: ReadArgs) {
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

    // Find the item
    let item: Option<ItemRow> = if let Some(ref repo) = args.repo {
        sqlx::query_as(
            "SELECT i.id, i.name, i.qualified_name, i.item_type,
                    i.line_start, i.line_end, sf.file_path, r.name as repo_name
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
            "SELECT i.id, i.name, i.qualified_name, i.item_type,
                    i.line_start, i.line_end, sf.file_path, r.name as repo_name
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
            eprintln!("{} Item not found: {}", "Error:".red().bold(), args.query);
            std::process::exit(1);
        }
    };

    let qname = item.qualified_name.as_deref().unwrap_or(&item.name);

    // Resolve full file path from scope.toml repo paths
    let full_path = resolve_file_path(&item.repo_name, &item.file_path);

    let full_path = match full_path {
        Some(p) => p,
        None => {
            eprintln!(
                "{} Cannot resolve file path: {}/{}",
                "Error:".red().bold(),
                item.repo_name,
                item.file_path
            );
            eprintln!("  Ensure .grepvec/scope.toml has the correct repo path.");
            std::process::exit(1);
        }
    };

    // Read the file
    let content = match std::fs::read_to_string(&full_path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} Cannot read {}: {}", "Error:".red().bold(), full_path, e);
            std::process::exit(1);
        }
    };

    let lines: Vec<&str> = content.lines().collect();
    let start = ((item.line_start - 1 - args.context).max(0)) as usize;
    let end = ((item.line_end + args.context) as usize).min(lines.len());

    // Header
    println!(
        "{} {} ({}) {}:{}–{}",
        item.item_type.dimmed(),
        qname.bold(),
        item.repo_name.cyan(),
        item.file_path,
        item.line_start,
        item.line_end
    );
    println!();

    // Source code with line numbers
    for (i, line) in lines[start..end].iter().enumerate() {
        let line_num = start + i + 1;
        let in_range = line_num >= item.line_start as usize && line_num <= item.line_end as usize;
        if in_range {
            println!("{:>4} {}", line_num.to_string().dimmed(), line);
        } else {
            println!("{:>4} {}", line_num.to_string().dimmed(), line.dimmed());
        }
    }
}

/// Resolve a repo-relative file path to an absolute path using scope.toml.
fn resolve_file_path(repo_name: &str, file_path: &str) -> Option<String> {
    let cwd = std::env::current_dir().ok()?;
    let scope_path = crate::inventory::scope::find_scope_file(&cwd)?;
    let scope = crate::inventory::scope::read_scope(&scope_path).ok()?;

    for repo in &scope.repos {
        if repo.name == repo_name {
            let full = format!("{}/{}", repo.path, file_path);
            if std::path::Path::new(&full).exists() {
                return Some(full);
            }
        }
    }

    // Fallback: try common parent directories
    let bases = [
        format!("/home/christopher/enscribe-io/{}", repo_name),
        "/home/christopher/enscribe-io".to_string(),
    ];
    for base in &bases {
        let full = format!("{}/{}", base, file_path);
        if std::path::Path::new(&full).exists() {
            return Some(full);
        }
    }

    None
}
