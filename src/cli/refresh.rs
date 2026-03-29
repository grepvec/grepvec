//! grepvec Refresh subcommand — Session-Start Hook
//!
//! Reads `.grepvec/scope.toml`, runs incremental absorption for each repo
//! in scope, regenerates stale biographies, and updates last_sha.

use clap::Args;
use colored::Colorize;

#[derive(Args)]
pub struct RefreshArgs {
    /// Path to scope.toml (default: search from cwd upward)
    #[arg(long)]
    scope: Option<std::path::PathBuf>,
}

pub async fn run(args: RefreshArgs) {
    // Find scope file
    let scope_path = if let Some(ref path) = args.scope {
        path.clone()
    } else {
        let cwd = std::env::current_dir().unwrap_or_default();
        match crate::inventory::scope::find_scope_file(&cwd) {
            Some(p) => p,
            None => {
                eprintln!(
                    "{} No .grepvec/scope.toml found. Create one with the scope for your project.",
                    "Error:".red().bold()
                );
                eprintln!("  Expected location: .grepvec/scope.toml");
                eprintln!("  Example content:");
                eprintln!("    [[repos]]");
                eprintln!("    name = \"enscribe-embed\"");
                eprintln!("    path = \"/home/christopher/enscribe-io/enscribe-embed\"");
                std::process::exit(1);
            }
        }
    };

    let scope = match crate::inventory::scope::read_scope(&scope_path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    };

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

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
    println!("{}", "  GREPVEC REFRESH".cyan().bold());
    println!(
        "  Scope: {} ({} repos)",
        scope_path.display(),
        scope.repos.len()
    );
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );

    let configs = crate::inventory::scope::to_repo_configs(&scope);
    let mut total_files = 0;
    let mut total_items = 0;

    for (repo_scope, config) in scope.repos.iter().zip(configs.iter()) {
        let changed_since = repo_scope.last_sha.as_deref();

        // Check if repo path exists
        if !config.path.exists() {
            eprintln!(
                "  {} {} — path not found: {}",
                "Skip:".yellow().bold(),
                config.name,
                config.path.display()
            );
            continue;
        }

        let current_sha = crate::inventory::scope::get_git_sha(&config.path);

        // Skip if SHA hasn't changed
        if let (Some(last), Some(ref current)) = (changed_since, &current_sha) {
            if last == current.as_str() {
                println!(
                    "  {} {} — no changes since {}",
                    "Skip:".green(),
                    config.name,
                    &last[..last.len().min(8)]
                );
                continue;
            }
        }

        // Run incremental absorption
        let label = if changed_since.is_some() {
            "incremental"
        } else {
            "full"
        };
        println!(
            "  {} {} ({})...",
            "Absorb:".green().bold(),
            config.name,
            label
        );

        let result = crate::inventory::absorb_repo(config, changed_since);

        if !result.errors.is_empty() {
            for err in &result.errors {
                eprintln!("    {} {}: {}", "Error:".red(), err.file, err.message);
            }
        }

        if result.files.is_empty() {
            println!("    0 changed files");
        } else {
            // Store in DB
            let git_sha = current_sha.as_deref().unwrap_or("unknown");

            // Ensure constraints
            crate::inventory::db::ensure_constraints(&pool).await.ok();

            match crate::inventory::db::store_absorption(
                &pool,
                &result,
                &config.path.display().to_string(),
                git_sha,
            )
            .await
            {
                Ok(stats) => {
                    println!(
                        "    {} files, {} items, {} edges",
                        stats.files, stats.items, stats.edges
                    );
                    total_files += stats.files;
                    total_items += stats.items;
                }
                Err(e) => {
                    eprintln!("    {} DB store failed: {}", "Error:".red().bold(), e);
                }
            }
        }

        // Update last_sha in scope file
        if let Some(ref sha) = current_sha {
            crate::inventory::scope::update_last_sha(&scope_path, &config.name, sha).ok();
        }
    }

    // Only regenerate biographies if something changed
    let mut total_bios_refreshed = 0;

    if total_files > 0 {
        println!("\n{}", "Refreshing biographies for changed repos...".green().bold());

        for repo_scope in &scope.repos {
            // Check if this repo has stale biographies
            let stale = crate::inventory::biography::count_stale_biographies(&pool, &repo_scope.name)
                .await
                .unwrap_or(0);

            if stale == 0 {
                continue;
            }

            println!("  {} {} ({} stale)", "Regen:".green().bold(), repo_scope.name, stale);

            match crate::inventory::biography::generate_biographies(&pool, &repo_scope.name).await {
                Ok(bios) if !bios.is_empty() => {
                    let git_sha = crate::inventory::scope::get_git_sha(
                        std::path::Path::new(&repo_scope.path),
                    )
                    .unwrap_or_else(|| "unknown".to_string());

                    match crate::inventory::biography::store_biographies(&pool, &bios, &git_sha).await
                    {
                        Ok(n) => total_bios_refreshed += n,
                        Err(e) => {
                            eprintln!(
                                "  {} biographies for {}: {}",
                                "Error:".red().bold(),
                                repo_scope.name,
                                e
                            );
                        }
                    }
                }
                _ => {}
            }
        }
    }

    // Summary
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
    println!("{}", "  REFRESH COMPLETE".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
    if total_files > 0 || total_items > 0 {
        println!("  Files absorbed:       {}", total_files.to_string().bold());
        println!("  Items upserted:       {}", total_items.to_string().bold());
    }
    println!(
        "  Biographies refreshed: {}",
        total_bios_refreshed.to_string().bold()
    );

    // Memory recall: check Enscribe for last session context
    let enscribe_url = std::env::var("ENSCRIBE_BASE_URL").unwrap_or_default();
    let enscribe_key = std::env::var("ENSCRIBE_API_KEY").unwrap_or_default();
    if !enscribe_url.is_empty() && !enscribe_key.is_empty() {
        if let Some(ref ens) = scope.enscribe {
            let collection = &ens.collection;
            println!("\n  {} Recalling last session...", "Memory:".yellow().bold());
            match recall_last_session(&enscribe_url, &enscribe_key, collection).await {
                Ok(memories) if !memories.is_empty() => {
                    for mem in memories.iter().take(3) {
                        println!("    {} {}", "•".dimmed(), mem.dimmed());
                    }
                }
                Ok(_) => {
                    println!("    {}", "(no prior session memories found)".dimmed());
                }
                Err(e) => {
                    println!("    {} {}", "Skip:".yellow(), e.to_string().dimmed());
                }
            }
        }
    }

    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan().bold()
    );
}

/// Recall recent session memories from Enscribe.
async fn recall_last_session(
    base_url: &str,
    api_key: &str,
    _collection: &str,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/v1/search", base_url))
        .header("Authorization", format!("Bearer {}", api_key))
        .json(&serde_json::json!({
            "query": "session decisions summary recent work",
            "collection_id": _collection,
            "limit": 5,
            "score_threshold": 0.15
        }))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await?;

    if !resp.status().is_success() {
        return Err(format!("Enscribe search failed: {}", resp.status()).into());
    }

    let body: serde_json::Value = resp.json().await?;
    let results = body["results"].as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|r| r["content"].as_str().map(|s| {
                    // Truncate long memories to one line
                    let first_line = s.lines().next().unwrap_or(s);
                    if first_line.len() > 120 {
                        format!("{}...", &first_line[..117])
                    } else {
                        first_line.to_string()
                    }
                }))
                .collect()
        })
        .unwrap_or_default();

    Ok(results)
}
