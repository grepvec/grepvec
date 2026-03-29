//! grepvec Search subcommand
//!
//! Semantic search across absorbed and documented code items.
//! Pass 1: keyword ILIKE search against Postgres biographies.
//! Pass 2 (--neural): vector-similarity search via VectorBackend trait
//!   (Enscribe HTTP or local Qdrant+BGE, selected by config).

use clap::Args;
use colored::Colorize;

#[derive(Args)]
pub struct SearchArgs {
    /// The search query
    pub query: String,

    /// Filter to a specific repository
    #[arg(short, long)]
    repo: Option<String>,

    /// Maximum results to return
    #[arg(short, long, default_value = "10")]
    limit: i64,

    /// Use exact substring matching (ILIKE) instead of full-text ranking
    #[arg(long, default_value = "false")]
    exact: bool,

    /// Enable neural (semantic) search as a second pass.
    /// Auto-enabled when a vector backend is configured.
    #[arg(long)]
    neural: bool,

    /// Disable neural search even when credentials are available
    #[arg(long)]
    no_neural: bool,

    /// Enscribe collection ID for neural search (auto-detected from scope.toml)
    #[arg(long)]
    collection_id: Option<String>,
}

pub async fn run(args: SearchArgs) {
    // Build a vector backend from config (env vars populated by load_config)
    let backend_config = crate::vector_backend::BackendConfig {
        backend_type: match std::env::var("GREPVEC_VECTOR_BACKEND").as_deref() {
            Ok("local") => crate::vector_backend::BackendType::Local,
            _ => crate::vector_backend::BackendType::Enscribe,
        },
        enscribe_url: std::env::var("ENSCRIBE_BASE_URL").ok(),
        enscribe_key: std::env::var("ENSCRIBE_API_KEY").ok(),
        qdrant_url: std::env::var("GREPVEC_QDRANT_URL").ok(),
        bge_url: std::env::var("GREPVEC_BGE_URL").ok(),
    };

    let backend = crate::vector_backend::create_backend(&backend_config);
    let use_neural = (args.neural || backend.is_some()) && !args.no_neural;

    if use_neural {
        // Pass 1: keyword search
        println!(
            "\n{}",
            "━━━ Pass 1: Keyword Search ━━━".cyan().bold()
        );
        db_search(&args, true).await;

        // Pass 2: neural search
        println!(
            "\n{}",
            "━━━ Pass 2: Neural Search ━━━".magenta().bold()
        );
        if let Some(ref backend) = backend {
            neural_search(&args, backend.as_ref()).await;
        } else {
            eprintln!(
                "{} No vector backend configured — skipping neural search",
                "Warning:".yellow().bold()
            );
        }
    } else {
        db_search(&args, false).await;
    }
}

/// Search annotations via Postgres full-text search (tsvector) or ILIKE (--exact).
async fn db_search(args: &SearchArgs, labeled: bool) {
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

    if args.exact {
        db_search_ilike(args, &pool, labeled).await;
    } else {
        db_search_tsvector(args, &pool, labeled).await;
    }
}

/// Full-text search using tsvector ranking (default).
async fn db_search_tsvector(args: &SearchArgs, pool: &sqlx::PgPool, labeled: bool) {
    let (sql, has_repo) = if args.repo.is_some() {
        (
            "SELECT a.content, i.qualified_name, i.name, i.item_type, sf.file_path,
                    r.name as repo_name,
                    (i.line_end - i.line_start + 1) as loc,
                    ts_rank_cd(a.search_vector, plainto_tsquery('english', $1)) as rank
             FROM annotations a
             JOIN items i ON a.item_id = i.id
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE a.search_vector @@ plainto_tsquery('english', $1)
               AND a.annotation_type = 'biography'
               AND r.name = $3
             ORDER BY rank DESC
             LIMIT $2".to_string(),
            true,
        )
    } else {
        (
            "SELECT a.content, i.qualified_name, i.name, i.item_type, sf.file_path,
                    r.name as repo_name,
                    (i.line_end - i.line_start + 1) as loc,
                    ts_rank_cd(a.search_vector, plainto_tsquery('english', $1)) as rank
             FROM annotations a
             JOIN items i ON a.item_id = i.id
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE a.search_vector @@ plainto_tsquery('english', $1)
               AND a.annotation_type = 'biography'
             ORDER BY rank DESC
             LIMIT $2".to_string(),
            false,
        )
    };

    let mut query = sqlx::query_as::<_, SearchResult>(&sql)
        .bind(&args.query)
        .bind(args.limit);

    if has_repo {
        query = query.bind(args.repo.as_ref().unwrap());
    }

    match query.fetch_all(pool).await {
        Ok(results) => {
            display_results(args, &results, labeled, "tsvector");
        }
        Err(e) => {
            eprintln!("{} Search failed: {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

/// Exact substring search using ILIKE (--exact mode).
async fn db_search_ilike(args: &SearchArgs, pool: &sqlx::PgPool, labeled: bool) {
    let search_terms: Vec<String> = args
        .query
        .split_whitespace()
        .map(|w| format!("%{}%", w))
        .collect();

    let mut conditions = Vec::new();
    let mut binds: Vec<String> = Vec::new();

    for (i, term) in search_terms.iter().enumerate() {
        conditions.push(format!("a.content ILIKE ${}", i + 1));
        binds.push(term.clone());
    }

    let repo_condition = if args.repo.is_some() {
        conditions.push(format!("r.name = ${}", binds.len() + 1));
        true
    } else {
        false
    };

    let where_clause = if conditions.is_empty() {
        "TRUE".to_string()
    } else {
        conditions.join(" AND ")
    };

    let sql = format!(
        "SELECT i.name, i.qualified_name, i.item_type, sf.file_path,
                r.name as repo_name, a.content,
                (i.line_end - i.line_start + 1) as loc,
                0.0::real as rank
         FROM annotations a
         JOIN items i ON a.item_id = i.id
         JOIN source_files sf ON i.file_id = sf.id
         JOIN repositories r ON sf.repo_id = r.id
         WHERE a.annotation_type = 'biography' AND {}
         ORDER BY i.visibility DESC, (i.line_end - i.line_start) DESC
         LIMIT {}",
        where_clause, args.limit
    );

    let mut query = sqlx::query_as::<_, SearchResult>(&sql);
    for term in &binds {
        query = query.bind(term);
    }
    if repo_condition {
        query = query.bind(args.repo.as_ref().unwrap());
    }

    match query.fetch_all(pool).await {
        Ok(results) => {
            display_results(args, &results, labeled, "exact");
        }
        Err(e) => {
            eprintln!("{} Search failed: {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }
}

/// Display search results.
fn display_results(args: &SearchArgs, results: &[SearchResult], labeled: bool, mode: &str) {
    if results.is_empty() {
        println!(
            "{} No results found for \"{}\"",
            "Search:".yellow().bold(),
            args.query
        );
        return;
    }

    let tag = if labeled {
        format!(" [keyword/{}]", mode)
    } else {
        String::new()
    };
    println!(
        "\n{} {} results for \"{}\"{}\n",
        "Search:".green().bold(),
        results.len(),
        args.query,
        tag
    );

    for (i, result) in results.iter().enumerate() {
        let qname = result
            .qualified_name
            .as_deref()
            .unwrap_or(&result.name);

        let label = if labeled {
            format!("[{}] ", mode).yellow().to_string()
        } else {
            String::new()
        };

        let rank_display = if result.rank > 0.0 {
            format!(" rank={:.4}", result.rank)
        } else {
            String::new()
        };

        println!(
            "{}. {}{} {} ({}) — {} LOC{}",
            (i + 1).to_string().bold(),
            label,
            result.item_type.cyan(),
            qname.bold(),
            result.file_path,
            result.loc.unwrap_or(0),
            rank_display
        );

        // Show first few lines of biography
        let preview: Vec<&str> = result.content.lines().take(4).collect();
        for line in preview {
            println!("   {}", line);
        }
        println!();
    }
}

// ---------------------------------------------------------------------------
// Neural search via VectorBackend trait
// ---------------------------------------------------------------------------

/// Parse a document_id like "grepvec::bio::ObserveGrpcService::health_check:chunk:0"
/// into a display-friendly qualified name like "ObserveGrpcService::health_check".
fn parse_qualified_name(document_id: &str) -> String {
    // Expected format: grepvec::bio::<qualified_name>:chunk:<n>
    // Strip the "grepvec::bio::" prefix and ":chunk:<n>" suffix.
    let stripped = document_id
        .strip_prefix("grepvec::bio::")
        .unwrap_or(document_id);

    // Remove trailing :chunk:<n> if present
    if let Some(pos) = stripped.rfind(":chunk:") {
        stripped[..pos].to_string()
    } else {
        stripped.to_string()
    }
}

/// Pass 2: neural/semantic search via VectorBackend.
async fn neural_search(args: &SearchArgs, backend: &dyn crate::vector_backend::VectorBackend) {
    let collection = args
        .collection_id
        .clone()
        .or_else(|| std::env::var("GREPVEC_COLLECTION").ok())
        .unwrap_or_default();

    if collection.is_empty() {
        eprintln!(
            "{} No collection configured for neural search",
            "Warning:".yellow().bold()
        );
        return;
    }

    let config = crate::vector_backend::SearchConfig {
        collection,
        limit: args.limit as usize,
        score_threshold: 0.2,
    };

    let start = std::time::Instant::now();
    match backend.search(&args.query, &config).await {
        Ok(results) => {
            let elapsed = start.elapsed().as_millis();
            if results.is_empty() {
                println!(
                    "{} No neural results for \"{}\"",
                    "Search:".yellow().bold(),
                    args.query
                );
            } else {
                println!(
                    "\n{} {} results for \"{}\" [{}] ({}ms)\n",
                    "Search:".green().bold(),
                    results.len(),
                    args.query,
                    backend.name(),
                    elapsed
                );
                for (i, r) in results.iter().enumerate() {
                    let qname = parse_qualified_name(&r.document_id);
                    println!(
                        "{}. {} {} — score {:.4}",
                        (i + 1).to_string().bold(),
                        format!("[{}]", backend.name()).magenta().to_string(),
                        qname.bold(),
                        r.score
                    );
                    println!(
                        "   {} {}",
                        "doc_id:".dimmed(),
                        r.document_id.dimmed()
                    );

                    // Show first 3 lines of content as preview
                    for line in r.content.lines().take(3) {
                        println!("   {}", line);
                    }
                    println!();
                }
            }
        }
        Err(e) => {
            eprintln!("{} Neural search failed: {}", "Error:".red().bold(), e);
        }
    }
}

// ---------------------------------------------------------------------------
// Database types
// ---------------------------------------------------------------------------

#[derive(Debug, sqlx::FromRow)]
struct SearchResult {
    name: String,
    qualified_name: Option<String>,
    item_type: String,
    file_path: String,
    #[allow(dead_code)]
    repo_name: String,
    content: String,
    loc: Option<i32>,
    rank: f32,
}
