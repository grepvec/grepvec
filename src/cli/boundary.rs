//! grepvec Boundary subcommand
//!
//! Manages the inferred layer: boundary nodes for external dependencies.

use clap::{Args, Subcommand};
use colored::Colorize;

#[derive(Args)]
pub struct BoundaryArgs {
    #[command(subcommand)]
    command: BoundaryCommand,
}

#[derive(Subcommand)]
enum BoundaryCommand {
    /// Show unresolved edges grouped by crate (the gap report)
    Gaps {
        /// Filter to a specific repository
        #[arg(short, long)]
        repo: Option<String>,
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Create boundary nodes from a JSON file
    Create {
        /// Path to JSON file with boundary node definitions
        #[arg(long)]
        from: std::path::PathBuf,
    },
    /// List existing boundary nodes
    List,
    /// Resolve unresolved edges to boundary nodes
    Resolve,
}

pub async fn run(args: BoundaryArgs) {
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

    // Ensure table exists
    if let Err(e) = crate::inventory::boundary::ensure_table(&pool).await {
        eprintln!("{} Failed to ensure boundary_nodes table: {}", "Error:".red().bold(), e);
        std::process::exit(1);
    }

    match args.command {
        BoundaryCommand::Gaps { repo, json } => cmd_gaps(&pool, repo.as_deref(), json).await,
        BoundaryCommand::Create { from } => cmd_create(&pool, &from).await,
        BoundaryCommand::List => cmd_list(&pool).await,
        BoundaryCommand::Resolve => cmd_resolve(&pool).await,
    }
}

async fn cmd_gaps(pool: &sqlx::PgPool, repo: Option<&str>, json_output: bool) {
    match crate::inventory::boundary::gap_report(pool, repo).await {
        Ok(gaps) => {
            if json_output {
                println!("{}", serde_json::to_string_pretty(&gaps).unwrap_or_default());
                return;
            }

            if gaps.is_empty() {
                println!("{} No unresolved crate-level edges found", "Gaps:".green().bold());
                return;
            }

            println!(
                "\n{}",
                "═══════════════════════════════════════════════════════════════"
                    .cyan().bold()
            );
            println!("{}", "  BOUNDARY NODE GAP REPORT".cyan().bold());
            if let Some(repo) = repo {
                println!("  Repo: {}", repo);
            }
            println!(
                "{}\n",
                "═══════════════════════════════════════════════════════════════"
                    .cyan().bold()
            );

            for gap in &gaps {
                let status = if gap.has_boundary_node {
                    "✓".green().to_string()
                } else {
                    "○".yellow().to_string()
                };

                println!(
                    "  {} {} — {} edges, repos: [{}]",
                    status,
                    gap.crate_prefix.bold(),
                    gap.dependent_items,
                    gap.repos.join(", ")
                );

                // Show sample API targets
                let sample: Vec<&str> = gap.unresolved_targets
                    .iter()
                    .take(5)
                    .map(|s| s.as_str())
                    .collect();
                for target in &sample {
                    println!("      → {}", target);
                }
                if gap.unresolved_targets.len() > 5 {
                    println!(
                        "      ... and {} more",
                        gap.unresolved_targets.len() - 5
                    );
                }
                println!();
            }

            let total_gaps = gaps.iter().filter(|g| !g.has_boundary_node).count();
            let total_covered = gaps.iter().filter(|g| g.has_boundary_node).count();
            println!(
                "  {} crates without boundary nodes, {} covered",
                total_gaps.to_string().yellow().bold(),
                total_covered.to_string().green().bold()
            );
            println!(
                "\n{}\n",
                "═══════════════════════════════════════════════════════════════"
                    .cyan().bold()
            );
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

async fn cmd_create(pool: &sqlx::PgPool, from: &std::path::Path) {
    let content = match std::fs::read_to_string(from) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{} Failed to read {}: {}", "Error:".red().bold(), from.display(), e);
            std::process::exit(1);
        }
    };

    let nodes: Vec<crate::inventory::boundary::BoundaryNode> = match serde_json::from_str(&content) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("{} Failed to parse JSON: {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    };

    println!("{} Creating {} boundary nodes...", "Boundary:".green().bold(), nodes.len());

    let mut created = 0;
    for node in &nodes {
        match crate::inventory::boundary::upsert_boundary_node(pool, node).await {
            Ok(_) => {
                println!("  {} {}", "✓".green(), node.name);
                created += 1;
            }
            Err(e) => {
                eprintln!("  {} {}: {}", "✗".red(), node.name, e);
            }
        }
    }

    println!(
        "\n{} {} boundary nodes created/updated",
        "Done:".green().bold(),
        created
    );
}

async fn cmd_list(pool: &sqlx::PgPool) {
    match crate::inventory::boundary::list_boundary_nodes(pool).await {
        Ok(nodes) => {
            if nodes.is_empty() {
                println!("{} No boundary nodes exist yet", "Boundary:".yellow().bold());
                println!("  Run 'grepvec boundary gaps' to see what's needed");
                return;
            }

            println!(
                "\n{}",
                "═══════════════════════════════════════════════════════════════"
                    .cyan().bold()
            );
            println!("{}", "  BOUNDARY NODES".cyan().bold());
            println!(
                "{}\n",
                "═══════════════════════════════════════════════════════════════"
                    .cyan().bold()
            );

            for node in &nodes {
                let version = node.version.as_deref().unwrap_or("?");
                println!(
                    "  {} ({}) — {} [confidence: {:.1}]",
                    node.name.bold(),
                    node.category.cyan(),
                    version,
                    node.confidence
                );
                if let Some(ref desc) = node.description {
                    println!("    {}", desc);
                }
                if !node.apis_used.is_empty() {
                    println!("    APIs: {}", node.apis_used.join(", "));
                }
                if let Some(ref impact) = node.failure_impact {
                    println!("    Failure: {}", impact.yellow());
                }
                if !node.dependent_repos.is_empty() {
                    println!("    Repos: {}", node.dependent_repos.join(", "));
                }
                println!();
            }

            println!(
                "  {} boundary nodes total",
                nodes.len().to_string().bold()
            );
            println!(
                "\n{}\n",
                "═══════════════════════════════════════════════════════════════"
                    .cyan().bold()
            );
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

async fn cmd_resolve(pool: &sqlx::PgPool) {
    println!("{}", "Resolving edges to boundary nodes...".green().bold());

    match crate::inventory::boundary::resolve_to_boundary_nodes(pool).await {
        Ok(n) => {
            println!(
                "{} {} edges resolved to boundary nodes",
                "Done:".green().bold(),
                n
            );
        }
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}
