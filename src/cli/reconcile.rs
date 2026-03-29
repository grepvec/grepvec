//! grepvec Reconcile subcommand
//!
//! Resolves cross-repo edges by matching unresolved target_name values
//! against items in other repositories.

use clap::Args;
use colored::Colorize;
use sqlx::PgPool;

#[derive(Args)]
pub struct ReconcileArgs {
    /// Reconcile cross-repo edges
    #[arg(long)]
    edges: bool,

    /// Show what would be resolved without committing changes
    #[arg(long)]
    dry_run: bool,

    /// Show detailed gap report: unresolved targets grouped by frequency
    #[arg(long)]
    report: bool,
}

/// Counts from each reconciliation pass.
#[derive(Debug, Default)]
struct PassStats {
    pass1_qualified: u64,
    pass2_suffix: u64,
    pass3_simple: u64,
}

impl PassStats {
    fn total(&self) -> u64 {
        self.pass1_qualified + self.pass2_suffix + self.pass3_simple
    }
}

pub async fn run(args: ReconcileArgs) {
    if !args.edges {
        eprintln!(
            "{} Specify --edges to reconcile cross-repo edges",
            "Error:".red().bold()
        );
        std::process::exit(1);
    }

    let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        eprintln!("{} TOWER_DB_URL not set", "Error:".red().bold());
        std::process::exit(1);
    }

    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!("{}", "  GREPVEC RECONCILE".cyan().bold());
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );

    if args.dry_run {
        println!("{}", "  [DRY RUN] No changes will be committed.\n".yellow().bold());
    }

    println!("{}", "Connecting to database...".green().bold());

    let pool = match crate::inventory::db::connect(&db_url).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    };

    // Snapshot: count of unresolved edges before reconciliation
    let before_unresolved = count_unresolved(&pool).await;
    let before_resolved = count_resolved(&pool).await;
    let total_edges = count_total(&pool).await;

    println!(
        "  Edges before: {} total, {} resolved, {} unresolved\n",
        total_edges.to_string().bold(),
        before_resolved.to_string().bold(),
        before_unresolved.to_string().bold(),
    );

    // --- Run the three-pass reconciliation ---
    println!("{}", "Reconciling cross-repo edges (3-pass)...".green().bold());

    let stats = if args.dry_run {
        reconcile_dry_run(&pool).await
    } else {
        reconcile_commit(&pool).await
    };

    // --- Results ---
    println!(
        "\n{}",
        "───────────────────────────────────────────────────────────────"
    );
    println!("{}", "  Reconciliation Results".cyan().bold());
    println!(
        "{}",
        "───────────────────────────────────────────────────────────────"
    );
    println!(
        "  Pass 1 (qualified_name match, conf=0.9): {}",
        stats.pass1_qualified.to_string().bold()
    );
    println!(
        "  Pass 2 (suffix match, unique,  conf=0.7): {}",
        stats.pass2_suffix.to_string().bold()
    );
    println!(
        "  Pass 3 (simple name match,     conf=0.8): {}",
        stats.pass3_simple.to_string().bold()
    );
    println!(
        "  {}                              {}",
        "Total resolved:".green().bold(),
        stats.total().to_string().bold()
    );

    // Updated stats (after commit, or projected for dry-run)
    let after_resolved = if args.dry_run {
        before_resolved + stats.total()
    } else {
        count_resolved(&pool).await
    };
    let after_unresolved = if args.dry_run {
        before_unresolved.saturating_sub(stats.total())
    } else {
        count_unresolved(&pool).await
    };

    let rate = if total_edges > 0 {
        (after_resolved as f64 / total_edges as f64) * 100.0
    } else {
        0.0
    };

    println!(
        "\n  Resolution rate: {:.1}%  ({} / {})",
        rate,
        after_resolved.to_string().bold(),
        total_edges.to_string().bold(),
    );
    println!(
        "  Still unresolved: {}",
        after_unresolved.to_string().bold()
    );

    // --- Gap report ---
    if args.report {
        print_gap_report(&pool).await;
    }

    // --- Platform stats ---
    match crate::inventory::db::get_stats(&pool).await {
        Ok(pstats) => {
            println!(
                "\n{}",
                "───────────────────────────────────────────────────────────────"
            );
            println!("{}", "  Platform Stats".cyan().bold());
            println!(
                "{}",
                "───────────────────────────────────────────────────────────────"
            );
            print!("{}", pstats);
        }
        Err(e) => {
            eprintln!("Warning: could not fetch updated stats: {}", e);
        }
    }

    println!(
        "\n{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
}

// ---------------------------------------------------------------------------
// Three-pass reconciliation (commit mode)
// ---------------------------------------------------------------------------

async fn reconcile_commit(pool: &PgPool) -> PassStats {
    let mut stats = PassStats::default();

    // Pass 1: Match target_name against qualified_name in other repos (most specific)
    println!("  Pass 1: qualified_name exact match...");
    let r = sqlx::query(
        "UPDATE edges e
         SET target_item_id = i.id,
             confidence = 0.9
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND e.target_name = i.qualified_name
           AND i.qualified_name IS NOT NULL
           AND sf.repo_id != (
             SELECT sf2.repo_id FROM source_files sf2
             JOIN items i2 ON i2.file_id = sf2.id
             WHERE i2.id = e.source_item_id
             LIMIT 1
           )",
    )
    .execute(pool)
    .await;
    stats.pass1_qualified = r.map(|r| r.rows_affected()).unwrap_or(0);
    println!("    -> {} edges resolved", stats.pass1_qualified);

    // Pass 2: Suffix match — target_name contains "::", match last segment
    //         against items whose qualified_name ends with that segment,
    //         but only where the name is unique across repos.
    println!("  Pass 2: suffix match on unique names...");
    let r = sqlx::query(
        "UPDATE edges e
         SET target_item_id = match.id,
             confidence = 0.7
         FROM (
           SELECT i.id, i.name
           FROM items i
           JOIN source_files sf ON i.file_id = sf.id
           WHERE i.item_type IN ('function', 'struct', 'enum', 'trait', 'constant', 'static', 'type_alias', 'class')
           GROUP BY i.id, i.name
           HAVING COUNT(*) OVER (PARTITION BY i.name) = 1
         ) match
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND e.target_name LIKE '%::%'
           AND match.name = split_part(e.target_name, '::', -1)
           AND match.id NOT IN (
             SELECT i2.id FROM items i2
             JOIN source_files sf2 ON i2.file_id = sf2.id
             WHERE sf2.repo_id = (
               SELECT sf3.repo_id FROM source_files sf3
               JOIN items i3 ON i3.file_id = sf3.id
               WHERE i3.id = e.source_item_id
               LIMIT 1
             )
           )",
    )
    .execute(pool)
    .await;
    stats.pass2_suffix = r.map(|r| r.rows_affected()).unwrap_or(0);
    println!("    -> {} edges resolved", stats.pass2_suffix);

    // Pass 3: Simple name match (the original logic)
    println!("  Pass 3: simple name match...");
    let r = sqlx::query(
        "UPDATE edges e
         SET target_item_id = i.id,
             confidence = 0.8
         FROM items i
         JOIN source_files sf ON i.file_id = sf.id
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND e.target_name = i.name
           AND i.item_type IN ('function', 'struct', 'enum', 'trait', 'constant', 'static', 'type_alias', 'class')
           AND sf.repo_id != (
             SELECT sf2.repo_id FROM source_files sf2
             JOIN items i2 ON i2.file_id = sf2.id
             WHERE i2.id = e.source_item_id
             LIMIT 1
           )",
    )
    .execute(pool)
    .await;
    stats.pass3_simple = r.map(|r| r.rows_affected()).unwrap_or(0);
    println!("    -> {} edges resolved", stats.pass3_simple);

    stats
}

// ---------------------------------------------------------------------------
// Three-pass reconciliation (dry-run mode: uses CTEs to count without writing)
// ---------------------------------------------------------------------------

async fn reconcile_dry_run(pool: &PgPool) -> PassStats {
    let mut stats = PassStats::default();

    // Pass 1: qualified_name match
    println!("  Pass 1: qualified_name exact match...");
    let r: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM edges e
         JOIN items i ON e.target_name = i.qualified_name
         JOIN source_files sf ON i.file_id = sf.id
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND i.qualified_name IS NOT NULL
           AND sf.repo_id != (
             SELECT sf2.repo_id FROM source_files sf2
             JOIN items i2 ON i2.file_id = sf2.id
             WHERE i2.id = e.source_item_id
             LIMIT 1
           )",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    stats.pass1_qualified = r.map(|r| r.0 as u64).unwrap_or(0);
    println!("    -> {} edges would be resolved", stats.pass1_qualified);

    // Pass 2: suffix match on unique names
    // We need to exclude the edges that would have been resolved in pass 1.
    println!("  Pass 2: suffix match on unique names...");
    let r: Option<(i64,)> = sqlx::query_as(
        "WITH pass1_resolved AS (
           SELECT e.id
           FROM edges e
           JOIN items i ON e.target_name = i.qualified_name
           JOIN source_files sf ON i.file_id = sf.id
           WHERE e.target_item_id IS NULL
             AND e.target_dep_id IS NULL
             AND i.qualified_name IS NOT NULL
             AND sf.repo_id != (
               SELECT sf2.repo_id FROM source_files sf2
               JOIN items i2 ON i2.file_id = sf2.id
               WHERE i2.id = e.source_item_id
               LIMIT 1
             )
         ),
         unique_items AS (
           SELECT i.id, i.name
           FROM items i
           WHERE i.item_type IN ('function', 'struct', 'enum', 'trait', 'constant', 'static', 'type_alias', 'class')
           GROUP BY i.id, i.name
           HAVING COUNT(*) OVER (PARTITION BY i.name) = 1
         )
         SELECT COUNT(*) FROM edges e
         JOIN unique_items ui ON ui.name = split_part(e.target_name, '::', -1)
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND e.target_name LIKE '%::%'
           AND e.id NOT IN (SELECT id FROM pass1_resolved)
           AND ui.id NOT IN (
             SELECT i2.id FROM items i2
             JOIN source_files sf2 ON i2.file_id = sf2.id
             WHERE sf2.repo_id = (
               SELECT sf3.repo_id FROM source_files sf3
               JOIN items i3 ON i3.file_id = sf3.id
               WHERE i3.id = e.source_item_id
               LIMIT 1
             )
           )",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    stats.pass2_suffix = r.map(|r| r.0 as u64).unwrap_or(0);
    println!("    -> {} edges would be resolved", stats.pass2_suffix);

    // Pass 3: simple name match (excluding pass 1 and pass 2 edges)
    println!("  Pass 3: simple name match...");
    let r: Option<(i64,)> = sqlx::query_as(
        "WITH pass1_resolved AS (
           SELECT e.id
           FROM edges e
           JOIN items i ON e.target_name = i.qualified_name
           JOIN source_files sf ON i.file_id = sf.id
           WHERE e.target_item_id IS NULL
             AND e.target_dep_id IS NULL
             AND i.qualified_name IS NOT NULL
             AND sf.repo_id != (
               SELECT sf2.repo_id FROM source_files sf2
               JOIN items i2 ON i2.file_id = sf2.id
               WHERE i2.id = e.source_item_id
               LIMIT 1
             )
         ),
         unique_items AS (
           SELECT i.id, i.name
           FROM items i
           WHERE i.item_type IN ('function', 'struct', 'enum', 'trait', 'constant', 'static', 'type_alias', 'class')
           GROUP BY i.id, i.name
           HAVING COUNT(*) OVER (PARTITION BY i.name) = 1
         ),
         pass2_resolved AS (
           SELECT e.id
           FROM edges e
           JOIN unique_items ui ON ui.name = split_part(e.target_name, '::', -1)
           WHERE e.target_item_id IS NULL
             AND e.target_dep_id IS NULL
             AND e.target_name LIKE '%::%'
             AND e.id NOT IN (SELECT id FROM pass1_resolved)
             AND ui.id NOT IN (
               SELECT i2.id FROM items i2
               JOIN source_files sf2 ON i2.file_id = sf2.id
               WHERE sf2.repo_id = (
                 SELECT sf3.repo_id FROM source_files sf3
                 JOIN items i3 ON i3.file_id = sf3.id
                 WHERE i3.id = e.source_item_id
                 LIMIT 1
               )
             )
         )
         SELECT COUNT(*) FROM edges e
         JOIN items i ON e.target_name = i.name
         JOIN source_files sf ON i.file_id = sf.id
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND i.item_type IN ('function', 'struct', 'enum', 'trait', 'constant', 'static', 'type_alias', 'class')
           AND e.id NOT IN (SELECT id FROM pass1_resolved)
           AND e.id NOT IN (SELECT id FROM pass2_resolved)
           AND sf.repo_id != (
             SELECT sf2.repo_id FROM source_files sf2
             JOIN items i2 ON i2.file_id = sf2.id
             WHERE i2.id = e.source_item_id
             LIMIT 1
           )",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    stats.pass3_simple = r.map(|r| r.0 as u64).unwrap_or(0);
    println!("    -> {} edges would be resolved", stats.pass3_simple);

    stats
}

// ---------------------------------------------------------------------------
// Gap report: top 20 unresolved targets by frequency
// ---------------------------------------------------------------------------

async fn print_gap_report(pool: &PgPool) {
    println!(
        "\n{}",
        "───────────────────────────────────────────────────────────────"
    );
    println!("{}", "  Gap Report: Top 20 Unresolved Targets".cyan().bold());
    println!(
        "{}",
        "───────────────────────────────────────────────────────────────"
    );

    let rows: Vec<(String, i64)> = match sqlx::query_as(
        "SELECT e.target_name, COUNT(*) as cnt
         FROM edges e
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
           AND e.target_name IS NOT NULL
         GROUP BY e.target_name
         ORDER BY cnt DESC
         LIMIT 20",
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  Warning: gap report query failed: {}", e);
            return;
        }
    };

    if rows.is_empty() {
        println!("  No unresolved edges found.");
        return;
    }

    // Find max name length for alignment (capped at 50)
    let max_name = rows
        .iter()
        .map(|(name, _)| name.len())
        .max()
        .unwrap_or(20)
        .min(50);

    println!(
        "  {:<width$}  {}",
        "Target Name",
        "Count",
        width = max_name
    );
    println!(
        "  {:<width$}  {}",
        "───────────",
        "─────",
        width = max_name
    );

    for (name, count) in &rows {
        let display_name = if name.len() > 50 {
            format!("{}...", &name[..47])
        } else {
            name.clone()
        };
        println!(
            "  {:<width$}  {}",
            display_name,
            count.to_string().bold(),
            width = max_name
        );
    }

    // Also show breakdown by edge_type
    println!(
        "\n{}",
        "  Unresolved by Edge Type:".cyan().bold()
    );

    let type_rows: Vec<(String, i64)> = match sqlx::query_as(
        "SELECT e.edge_type, COUNT(*) as cnt
         FROM edges e
         WHERE e.target_item_id IS NULL
           AND e.target_dep_id IS NULL
         GROUP BY e.edge_type
         ORDER BY cnt DESC",
    )
    .fetch_all(pool)
    .await
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("  Warning: edge type query failed: {}", e);
            return;
        }
    };

    for (edge_type, count) in &type_rows {
        println!(
            "    {:<20}  {}",
            edge_type,
            count.to_string().bold()
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn count_unresolved(pool: &PgPool) -> u64 {
    let r: Option<(i64,)> = sqlx::query_as(
        "SELECT COUNT(*) FROM edges WHERE target_item_id IS NULL AND target_dep_id IS NULL",
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten();
    r.map(|r| r.0 as u64).unwrap_or(0)
}

async fn count_resolved(pool: &PgPool) -> u64 {
    let r: Option<(i64,)> =
        sqlx::query_as("SELECT COUNT(*) FROM edges WHERE target_item_id IS NOT NULL")
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    r.map(|r| r.0 as u64).unwrap_or(0)
}

async fn count_total(pool: &PgPool) -> u64 {
    let r: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM edges")
        .fetch_optional(pool)
        .await
        .ok()
        .flatten();
    r.map(|r| r.0 as u64).unwrap_or(0)
}
