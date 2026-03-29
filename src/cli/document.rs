//! grepvec Document subcommand
//!
//! Generates biographies for absorbed code items, stores them in the
//! annotations table.

use clap::Args;
use colored::Colorize;

#[derive(Args)]
pub struct DocumentArgs {
    /// Repository name
    #[arg(short, long)]
    repo: Option<String>,

    /// Document all configured repositories
    #[arg(long)]
    all: bool,

    /// Preview biographies without storing to DB
    #[arg(long)]
    dry_run: bool,

    /// Show a sample of N biographies
    #[arg(long)]
    sample: Option<usize>,

    /// Only regenerate stale biographies (items whose source changed)
    #[arg(long)]
    stale_only: bool,
}

fn default_repo_names() -> Vec<String> {
    vec![
        "enscribe-embed".into(),
        "enscribe-developer".into(),
        "enscribe-observe".into(),
        "enscribe-CLI".into(),
    ]
}

pub async fn run(args: DocumentArgs) {
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

    let repos = if args.all {
        default_repo_names()
    } else if let Some(ref name) = args.repo {
        vec![name.clone()]
    } else {
        eprintln!("{} Specify --repo <name> or --all", "Error:".red().bold());
        std::process::exit(1);
    };

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
    println!("{}", "  GREPVEC DOCUMENT".cyan().bold());
    if args.dry_run {
        println!("{}", "  (DRY RUN — no database writes)".yellow().bold());
    }
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );

    let mut total_bios = 0;
    let mut total_stored = 0;

    for repo_name in &repos {
        println!(
            "{} {}",
            "Generating biographies:".green().bold(),
            repo_name
        );

        match crate::inventory::biography::generate_biographies(&pool, repo_name).await {
            Ok(biographies) => {
                println!(
                    "  {} biographies generated",
                    biographies.len().to_string().bold()
                );
                total_bios += biographies.len();

                // Show sample if requested
                if let Some(n) = args.sample {
                    println!();
                    for bio in biographies.iter().take(n) {
                        println!(
                            "{}",
                            "───────────────────────────────────────────────────────────────"
                        );
                        println!("{}", bio.text);
                    }
                    println!(
                        "{}",
                        "───────────────────────────────────────────────────────────────"
                    );
                }

                // Store in DB
                if !args.dry_run {
                    let git_sha = std::process::Command::new("git")
                        .args(["rev-parse", "HEAD"])
                        .output()
                        .ok()
                        .and_then(|o| String::from_utf8(o.stdout).ok())
                        .map(|s| s.trim().to_string())
                        .unwrap_or_else(|| "unknown".to_string());

                    // Ensure unique constraint on annotations
                    sqlx::query(
                        "CREATE UNIQUE INDEX IF NOT EXISTS uq_annotations_item_type
                         ON annotations (item_id, annotation_type)",
                    )
                    .execute(&pool)
                    .await
                    .ok();

                    match crate::inventory::biography::store_biographies(
                        &pool,
                        &biographies,
                        &git_sha,
                    )
                    .await
                    {
                        Ok(n) => {
                            println!(
                                "  {} {} biographies stored in annotations table",
                                "Stored:".green().bold(),
                                n
                            );
                            total_stored += n;
                        }
                        Err(e) => {
                            eprintln!(
                                "  {} Failed to store biographies: {}",
                                "Error:".red().bold(),
                                e
                            );
                        }
                    }
                }

            }
            Err(e) => {
                eprintln!(
                    "  {} Failed to generate biographies: {}",
                    "Error:".red().bold(),
                    e
                );
            }
        }

        // Show stale count
        if let Ok(stale) = crate::inventory::biography::count_stale_biographies(&pool, repo_name).await {
            if stale > 0 {
                println!(
                    "  {} {} stale biographies (source changed since last generation)",
                    "Stale:".yellow().bold(),
                    stale
                );
            }
        }
    }

    // Summary
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
    println!("{}", "  DOCUMENT SUMMARY".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
    println!("  Biographies generated:  {}", total_bios.to_string().bold());
    if !args.dry_run {
        println!("  Biographies stored:     {}", total_stored.to_string().bold());
    }
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
}
