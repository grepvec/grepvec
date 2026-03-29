//! grepvec Absorb subcommand
//!
//! Absorbs codebases using tree-sitter: parses every source file, extracts
//! items and edges, computes qualified names, detects external dependencies,
//! and stores everything in Postgres.

use clap::Args;
use colored::Colorize;
use crate::inventory::{self, AbsorptionResult, RepoConfig};
use std::path::PathBuf;

#[derive(Args)]
pub struct AbsorbArgs {
    /// Repository name (e.g., enscribe-embed)
    #[arg(short, long)]
    repo: Option<String>,

    /// Path to the repository root
    #[arg(short, long)]
    path: Option<PathBuf>,

    /// Absorb all configured repositories
    #[arg(long)]
    all: bool,

    /// Parse and report without writing to the database
    #[arg(long)]
    dry_run: bool,

    /// Show platform-wide statistics from the database
    #[arg(long)]
    stats: bool,

    /// Only absorb files changed since this git SHA
    #[arg(long)]
    changed_since: Option<String>,

    /// Output format: text or json
    #[arg(short, long, default_value = "text")]
    format: String,

    /// Run database migrations (ensure constraints, add missing columns)
    #[arg(long)]
    migrate: bool,

    /// Show database schema introspection
    #[arg(long)]
    schema: bool,
}

/// Default repository configurations for the Enscribe platform.
fn default_repos() -> Vec<RepoConfig> {
    let base = PathBuf::from("/home/christopher/enscribe-io");
    vec![
        RepoConfig {
            name: "enscribe-embed".into(),
            path: base.join("enscribe-embed"),
            primary_language: "rust".into(),
        },
        RepoConfig {
            name: "enscribe-developer".into(),
            path: base.join("enscribe-developer"),
            primary_language: "rust".into(),
        },
        RepoConfig {
            name: "enscribe-observe".into(),
            path: base.join("enscribe-observe"),
            primary_language: "rust".into(),
        },
        RepoConfig {
            name: "enscribe-CLI".into(),
            path: base.join("enscribe-CLI"),
            primary_language: "rust".into(),
        },
        // enscribe-io root is governance/docs only — no source to absorb.
        // Marketing site uses .astro files (not yet supported by tree-sitter).
        // enscribe-lab is EXCLUDED (legacy, superseded by enscribe-developer).
    ]
}

pub async fn run(args: AbsorbArgs) {
    // Schema introspection mode
    if args.schema {
        show_schema().await;
        return;
    }

    // Migration mode
    if args.migrate {
        run_migrate().await;
        return;
    }

    // Stats mode: just show DB stats and exit
    if args.stats {
        show_stats().await;
        return;
    }

    // Determine which repos to absorb
    let repos = if args.all {
        default_repos()
    } else if let Some(ref name) = args.repo {
        let path = args.path.clone().unwrap_or_else(|| {
            // Try to find the repo in the default locations
            let base = PathBuf::from("/home/christopher/enscribe-io");
            base.join(name)
        });

        if !path.exists() || !path.is_dir() {
            eprintln!(
                "{} Repository path does not exist: {}",
                "Error:".red().bold(),
                path.display()
            );
            std::process::exit(1);
        }

        vec![RepoConfig {
            name: name.clone(),
            path,
            primary_language: "rust".into(),
        }]
    } else {
        eprintln!(
            "{} Specify --repo <name> [--path <path>] or --all",
            "Error:".red().bold()
        );
        std::process::exit(1);
    };

    // Print header
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!("{}", "  GREPVEC ABSORB".cyan().bold());
    if args.dry_run {
        println!("{}", "  (DRY RUN — no database writes)".yellow().bold());
    }
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );

    // Absorb each repository
    let mut total_files = 0;
    let mut total_items = 0;
    let mut total_edges = 0;
    let mut total_errors = 0;
    let mut all_results: Vec<AbsorptionResult> = Vec::new();

    for repo in &repos {
        println!(
            "{} {} ({})",
            "Absorbing:".green().bold(),
            repo.name,
            repo.path.display()
        );

        let result = inventory::absorb_repo(repo, args.changed_since.as_deref());

        println!(
            "  {} files, {} items, {} edges, {} errors",
            result.files.len().to_string().bold(),
            result.total_items.to_string().bold(),
            result.total_edges.to_string().bold(),
            if result.errors.is_empty() {
                "0".green().bold()
            } else {
                result.errors.len().to_string().red().bold()
            }
        );

        // Also detect external dependencies for each file
        let mut ext_dep_count = 0;
        for file in &result.files {
            let ext_edges = inventory::external_deps::detect_external_deps(&file.items, "");
            ext_dep_count += ext_edges.len();
        }
        if ext_dep_count > 0 {
            println!(
                "  {} external dependency edges detected",
                ext_dep_count.to_string().cyan().bold()
            );
        }

        for err in &result.errors {
            eprintln!(
                "  {} {}: {}",
                "Error:".red(),
                err.file,
                err.message
            );
        }

        total_files += result.files.len();
        total_items += result.total_items;
        total_edges += result.total_edges;
        total_errors += result.errors.len();
        all_results.push(result);
    }

    // Store in database (unless dry run)
    if !args.dry_run {
        let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
        if db_url.is_empty() {
            println!(
                "\n{} TOWER_DB_URL not set — skipping database storage",
                "Warning:".yellow().bold()
            );
        } else {
            println!("\n{}", "Writing to database...".green().bold());
            match inventory::db::connect(&db_url).await {
                Ok(pool) => {
                    // Ensure required constraints exist
                    if let Err(e) = inventory::db::ensure_constraints(&pool).await {
                        eprintln!(
                            "{} Failed to ensure constraints: {}",
                            "Warning:".yellow().bold(),
                            e
                        );
                    }
                    for (result, repo) in all_results.iter().zip(repos.iter()) {
                        let git_sha = get_git_sha(&repo.path);
                        let repo_path = repo.path.display().to_string();
                        match inventory::db::store_absorption(
                            &pool,
                            result,
                            &repo_path,
                            &git_sha,
                        )
                        .await
                        {
                            Ok(stats) => {
                                let mut detail = format!(
                                    "{} files, {} items, {} edges",
                                    stats.files, stats.items, stats.edges
                                );
                                if stats.intra_resolved > 0 {
                                    detail.push_str(&format!(
                                        ", {} intra-repo resolved", stats.intra_resolved
                                    ));
                                }
                                if stats.stale_items_cleaned > 0 {
                                    detail.push_str(&format!(
                                        ", {} stale items cleaned", stats.stale_items_cleaned
                                    ));
                                }
                                println!(
                                    "  {} {}: {}",
                                    "Stored:".green().bold(),
                                    result.repo_name,
                                    detail
                                );

                                // Store external dependency edges
                                let mut all_ext_edges = Vec::new();
                                for file in &result.files {
                                    let source = std::fs::read_to_string(
                                        repo.path.join(&file.path)
                                    ).unwrap_or_default();
                                    let ext = inventory::external_deps::detect_external_deps(
                                        &file.items, &source
                                    );
                                    all_ext_edges.extend(ext);
                                }
                                if !all_ext_edges.is_empty() {
                                    // Need repo_id — fetch it
                                    let repo_id_row: Option<(uuid::Uuid,)> = sqlx::query_as(
                                        "SELECT id FROM repositories WHERE name = $1"
                                    )
                                    .bind(&result.repo_name)
                                    .fetch_optional(&pool)
                                    .await
                                    .ok()
                                    .flatten();

                                    if let Some((repo_id,)) = repo_id_row {
                                        match inventory::db::store_external_dep_edges(
                                            &pool, repo_id, &all_ext_edges
                                        ).await {
                                            Ok(n) if n > 0 => {
                                                println!(
                                                    "  {} {} external dep edges stored",
                                                    "  ExtDeps:".cyan().bold(), n
                                                );
                                            }
                                            _ => {}
                                        }

                                        // Store git history
                                        match inventory::db::store_git_history(
                                            &pool, repo_id, &repo.path
                                        ).await {
                                            Ok(n) if n > 0 => {
                                                println!(
                                                    "  {} git history for {} files",
                                                    "  GitLog:".cyan().bold(), n
                                                );
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                eprintln!(
                                    "  {} Failed to store {}: {}",
                                    "Error:".red().bold(),
                                    result.repo_name,
                                    e
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!(
                        "{} Failed to connect to database: {}",
                        "Error:".red().bold(),
                        e
                    );
                }
            }
        }
    }

    // Output JSON if requested
    if args.format == "json" {
        match serde_json::to_string_pretty(&all_results) {
            Ok(json) => println!("{}", json),
            Err(e) => eprintln!("Failed to serialize: {}", e),
        }
    }

    // Print summary
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!("{}", "  ABSORPTION SUMMARY".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!(
        "  Repositories:  {}",
        repos.len().to_string().bold()
    );
    println!(
        "  Files:         {}",
        total_files.to_string().bold()
    );
    println!(
        "  Items:         {}",
        total_items.to_string().bold()
    );
    println!(
        "  Edges:         {}",
        total_edges.to_string().bold()
    );
    if total_errors > 0 {
        println!(
            "  Errors:        {}",
            total_errors.to_string().red().bold()
        );
    } else {
        println!(
            "  Errors:        {}",
            "0".green().bold()
        );
    }
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
}

async fn show_stats() {
    let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        eprintln!(
            "{} TOWER_DB_URL not set",
            "Error:".red().bold()
        );
        std::process::exit(1);
    }

    match inventory::db::connect(&db_url).await {
        Ok(pool) => match inventory::db::get_stats(&pool).await {
            Ok(stats) => {
                println!(
                    "\n{}",
                    "═══════════════════════════════════════════════════════════════"
                        .cyan()
                        .bold()
                );
                println!("{}", "  PLATFORM INVENTORY STATS".cyan().bold());
                println!(
                    "{}",
                    "═══════════════════════════════════════════════════════════════"
                        .cyan()
                        .bold()
                );
                print!("{}", stats);
                println!(
                    "{}\n",
                    "═══════════════════════════════════════════════════════════════"
                        .cyan()
                        .bold()
                );
            }
            Err(e) => {
                eprintln!("{} {}", "Error:".red().bold(), e);
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

async fn show_schema() {
    let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        eprintln!("{} TOWER_DB_URL not set", "Error:".red().bold());
        std::process::exit(1);
    }

    match inventory::db::connect(&db_url).await {
        Ok(pool) => match inventory::db::introspect_schema(&pool).await {
            Ok(tables) => {
                println!(
                    "\n{}",
                    "═══════════════════════════════════════════════════════════════"
                        .cyan()
                        .bold()
                );
                println!("{}", "  DATABASE SCHEMA".cyan().bold());
                println!(
                    "{}",
                    "═══════════════════════════════════════════════════════════════"
                        .cyan()
                        .bold()
                );
                for table in &tables {
                    println!("\n  {} {}", "TABLE:".green().bold(), table.name.bold());
                    for col in &table.columns {
                        println!(
                            "    {:<30} {:<20} {}",
                            col.name,
                            col.data_type,
                            if col.nullable { "NULL" } else { "NOT NULL" }
                        );
                    }
                }
                println!(
                    "\n{}\n",
                    "═══════════════════════════════════════════════════════════════"
                        .cyan()
                        .bold()
                );
            }
            Err(e) => {
                eprintln!("{} {}", "Error:".red().bold(), e);
                std::process::exit(1);
            }
        },
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

async fn run_migrate() {
    let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        eprintln!("{} TOWER_DB_URL not set", "Error:".red().bold());
        std::process::exit(1);
    }

    match inventory::db::connect(&db_url).await {
        Ok(pool) => {
            println!("{}", "Running migrations...".green().bold());
            match inventory::db::run_migrations(&pool).await {
                Ok((prev, new, applied)) => {
                    if applied > 0 {
                        println!(
                            "{} Schema migrated: v{} -> v{} ({} migration{} applied)",
                            "Success:".green().bold(),
                            prev,
                            new,
                            applied,
                            if applied == 1 { "" } else { "s" }
                        );
                    } else {
                        println!(
                            "{} Schema already at v{} — nothing to do",
                            "Success:".green().bold(),
                            new
                        );
                    }
                }
                Err(e) => {
                    eprintln!("{} {}", "Error:".red().bold(), e);
                    std::process::exit(1);
                }
            }
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

/// Get the current git SHA for a repository path.
fn get_git_sha(repo_path: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}
