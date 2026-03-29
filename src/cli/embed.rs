//! grepvec Embed subcommand
//!
//! Bulk-ingests biographies from Postgres into an Enscribe collection
//! for neural search via the ingest-prepared API.

use clap::Args;
use colored::Colorize;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::Semaphore;

/// Voice template JSON (subset of fields we need).
#[derive(Debug, Deserialize)]
struct VoiceTemplate {
    #[allow(dead_code)]
    name: String,
    collection_id: String,
}

#[derive(Args)]
pub struct EmbedArgs {
    /// Initialize: embed all biographies from scope using voice template
    #[arg(long)]
    init: bool,

    /// Enscribe collection ID (required unless --init)
    #[arg(long)]
    collection_id: Option<String>,

    /// Filter to a specific repository
    #[arg(short, long)]
    repo: Option<String>,

    /// Also embed boundary nodes
    #[arg(long)]
    boundary_nodes: bool,

    /// Number of concurrent ingest requests
    #[arg(long, default_value = "5")]
    concurrency: usize,

    /// Preview what would be ingested without sending requests
    #[arg(long)]
    dry_run: bool,

    /// Use element-level segments instead of whole-biography chunks
    #[arg(long)]
    element_level: bool,
}

/// A biography row fetched from the annotations table.
#[derive(Debug, sqlx::FromRow)]
#[allow(dead_code)]
struct BiographyRow {
    qualified_name: Option<String>,
    item_name: String,
    item_type: String,
    file_path: String,
    repo_name: String,
    content: String,
}

/// A boundary node row fetched from the boundary_nodes table.
#[derive(Debug, sqlx::FromRow)]
struct BoundaryNodeRow {
    name: String,
    crate_name: String,
    category: String,
    description: Option<String>,
    apis_used: Option<Vec<String>>,
    failure_impact: Option<String>,
    dependent_repos: Option<Vec<String>>,
}

/// The ingest-prepared request body.
#[derive(Debug, Serialize)]
struct IngestPreparedRequest {
    collection_id: String,
    document_id: String,
    segments: Vec<Segment>,
}

/// A single segment within an ingest-prepared request.
#[derive(Debug, Serialize)]
struct Segment {
    content: String,
    label: String,
    confidence: f64,
    reasoning: String,
    start_paragraph: u32,
    end_paragraph: u32,
    metadata: serde_json::Value,
}

/// A single progress event from ingest-prepared response.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct IngestProgressEvent {
    document_id: Option<String>,
    status: Option<String>,
    chunks_created: Option<u32>,
    embeddings_stored: Option<u32>,
    tokens_used: Option<u32>,
    error_message: Option<String>,
}

pub async fn run(args: EmbedArgs) {
    // ── Resolve collection_id and repo list ──────────────────────────
    // When --init is used, read from scope.toml and voice template.
    // Otherwise, require --collection-id.
    let (collection_id, init_repos): (String, Option<Vec<String>>) = if args.init {
        // Find scope file
        let cwd = std::env::current_dir().unwrap_or_else(|e| {
            eprintln!("{} Cannot determine current directory: {}", "Error:".red().bold(), e);
            std::process::exit(1);
        });
        let scope_path = match crate::inventory::scope::find_scope_file(&cwd) {
            Some(p) => p,
            None => {
                eprintln!(
                    "{} No .grepvec/scope.toml found (searched upward from {})",
                    "Error:".red().bold(),
                    cwd.display()
                );
                std::process::exit(1);
            }
        };
        let scope = match crate::inventory::scope::read_scope(&scope_path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{} {}", "Error:".red().bold(), e);
                std::process::exit(1);
            }
        };

        // Read voice template for collection_id
        let voice_path = scope_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("voices")
            .join("biography-search.json");
        let voice_template: VoiceTemplate = match std::fs::read_to_string(&voice_path) {
            Ok(content) => match serde_json::from_str(&content) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!(
                        "{} Failed to parse {}: {}",
                        "Error:".red().bold(),
                        voice_path.display(),
                        e
                    );
                    std::process::exit(1);
                }
            },
            Err(e) => {
                eprintln!(
                    "{} Failed to read {}: {}",
                    "Error:".red().bold(),
                    voice_path.display(),
                    e
                );
                std::process::exit(1);
            }
        };

        let repo_names: Vec<String> = scope.repos.iter().map(|r| r.name.clone()).collect();

        println!(
            "\n{} scope.toml  -> {} repos: {}",
            "Init:".green().bold(),
            repo_names.len(),
            repo_names.join(", ")
        );
        println!(
            "{} voice template -> collection_id: {}",
            "Init:".green().bold(),
            voice_template.collection_id
        );
        println!(
            "{} Creating collection, setting up voice, embedding biographies...",
            "Init:".green().bold(),
        );

        // If --collection-id was also provided, it is ignored in --init mode
        (voice_template.collection_id, Some(repo_names))
    } else {
        match args.collection_id.clone() {
            Some(id) => (id, None),
            None => {
                eprintln!(
                    "{} --collection-id is required (or use --init to read from scope.toml)",
                    "Error:".red().bold()
                );
                std::process::exit(1);
            }
        }
    };

    // Read environment
    let api_key = match std::env::var("ENSCRIBE_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("{} ENSCRIBE_API_KEY not set", "Error:".red().bold());
            std::process::exit(1);
        }
    };

    let base_url = std::env::var("ENSCRIBE_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());

    let db_url = std::env::var("TOWER_DB_URL").unwrap_or_default();
    if db_url.is_empty() {
        eprintln!("{} TOWER_DB_URL not set", "Error:".red().bold());
        std::process::exit(1);
    }

    // Connect to Postgres
    let pool = match crate::inventory::db::connect(&db_url).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    };

    // Print header
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!("{}", "  GREPVEC EMBED".cyan().bold());
    if args.init {
        println!("{}", "  (INIT — full pipeline from scope)".green().bold());
    }
    if args.dry_run {
        println!("{}", "  (DRY RUN — no API calls)".yellow().bold());
    }
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!("  Collection:   {}", collection_id.bold());
    println!("  Enscribe URL: {}", base_url);
    println!("  Concurrency:  {}", args.concurrency);
    if let Some(ref repos) = init_repos {
        println!("  Repos (init): {}", repos.join(", "));
    } else if let Some(ref repo) = args.repo {
        println!("  Repo filter:  {}", repo);
    }
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );

    // Fetch biographies from annotations table
    println!("{}", "Fetching biographies from Postgres...".green().bold());

    // Determine the effective repo filter: --init embeds all repos from scope,
    // --repo filters to a single repo, otherwise fetch all.
    let biographies: Vec<BiographyRow> = if let Some(ref repos) = init_repos {
        // --init mode: fetch biographies for all repos in scope
        match sqlx::query_as(
            "SELECT i.qualified_name, i.name as item_name, i.item_type,
                    sf.file_path, r.name as repo_name, a.content
             FROM annotations a
             JOIN items i ON a.item_id = i.id
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE a.annotation_type = 'biography'
               AND r.name = ANY($1)
             ORDER BY r.name, sf.file_path, i.line_start",
        )
        .bind(repos)
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("{} Failed to fetch biographies: {}", "Error:".red().bold(), e);
                std::process::exit(1);
            }
        }
    } else if let Some(ref repo) = args.repo {
        match sqlx::query_as(
            "SELECT i.qualified_name, i.name as item_name, i.item_type,
                    sf.file_path, r.name as repo_name, a.content
             FROM annotations a
             JOIN items i ON a.item_id = i.id
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE a.annotation_type = 'biography'
               AND r.name = $1
             ORDER BY r.name, sf.file_path, i.line_start",
        )
        .bind(repo)
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("{} Failed to fetch biographies: {}", "Error:".red().bold(), e);
                std::process::exit(1);
            }
        }
    } else {
        match sqlx::query_as(
            "SELECT i.qualified_name, i.name as item_name, i.item_type,
                    sf.file_path, r.name as repo_name, a.content
             FROM annotations a
             JOIN items i ON a.item_id = i.id
             JOIN source_files sf ON i.file_id = sf.id
             JOIN repositories r ON sf.repo_id = r.id
             WHERE a.annotation_type = 'biography'
             ORDER BY r.name, sf.file_path, i.line_start",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => rows,
            Err(e) => {
                eprintln!("{} Failed to fetch biographies: {}", "Error:".red().bold(), e);
                std::process::exit(1);
            }
        }
    };

    println!(
        "  {} biographies found",
        biographies.len().to_string().bold()
    );

    // Fetch boundary nodes if requested
    let boundary_nodes: Vec<BoundaryNodeRow> = if args.boundary_nodes {
        println!("{}", "Fetching boundary nodes...".green().bold());
        match sqlx::query_as(
            "SELECT name, crate_name, category, description,
                    apis_used, failure_impact, dependent_repos
             FROM boundary_nodes
             ORDER BY crate_name",
        )
        .fetch_all(&pool)
        .await
        {
            Ok(rows) => {
                println!(
                    "  {} boundary nodes found",
                    rows.len().to_string().bold()
                );
                rows
            }
            Err(e) => {
                eprintln!(
                    "{} Failed to fetch boundary nodes: {}",
                    "Warning:".yellow().bold(),
                    e
                );
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    let total_work = biographies.len() + boundary_nodes.len();
    if total_work == 0 {
        println!("\n{} Nothing to embed", "Done:".yellow().bold());
        return;
    }

    // Dry run: just show what would be sent
    if args.dry_run {
        println!(
            "\n{}",
            "───────────────────────────────────────────────────────────────"
        );
        println!("{}", "  DRY RUN PREVIEW".cyan().bold());
        println!(
            "{}",
            "───────────────────────────────────────────────────────────────"
        );

        for (i, bio) in biographies.iter().enumerate().take(10) {
            let qname = bio.qualified_name.as_deref().unwrap_or(&bio.item_name);
            let doc_id = format!("grepvec::bio::{}", qname);
            println!(
                "  {}. {} {} → {}",
                (i + 1).to_string().bold(),
                bio.item_type.cyan(),
                qname,
                doc_id
            );
        }
        if biographies.len() > 10 {
            println!("  ... and {} more biographies", biographies.len() - 10);
        }

        for (i, node) in boundary_nodes.iter().enumerate().take(5) {
            let doc_id = format!("grepvec::boundary::{}", node.name);
            println!(
                "  {}. {} {} → {}",
                (biographies.len() + i + 1).to_string().bold(),
                "boundary".cyan(),
                node.name,
                doc_id
            );
        }
        if boundary_nodes.len() > 5 {
            println!("  ... and {} more boundary nodes", boundary_nodes.len() - 5);
        }

        println!(
            "\n  {} biographies + {} boundary nodes = {} total documents",
            biographies.len().to_string().bold(),
            boundary_nodes.len().to_string().bold(),
            total_work.to_string().bold()
        );
        println!(
            "\n{}\n",
            "═══════════════════════════════════════════════════════════════"
                .cyan()
                .bold()
        );
        return;
    }

    // Build HTTP client
    let http = reqwest::Client::new();
    let semaphore = Arc::new(Semaphore::new(args.concurrency));
    let success_count = Arc::new(AtomicUsize::new(0));
    let error_count = Arc::new(AtomicUsize::new(0));

    // Ingest biographies
    if args.element_level {
        println!(
            "\n{} Ingesting {} biographies (element-level segments)...",
            "Embed:".green().bold(),
            biographies.len()
        );
    } else {
        println!(
            "\n{} Ingesting {} biographies (whole-biography chunks)...",
            "Embed:".green().bold(),
            biographies.len()
        );
    }

    // If element-level, generate segmented biographies from DB
    let segmented: Vec<crate::inventory::biography::SegmentedBiography> = if args.element_level {
        let mut all_segmented = Vec::new();
        // Get unique repo names from biographies
        let repo_names: Vec<String> = {
            let mut names: Vec<String> = biographies.iter().map(|b| b.repo_name.clone()).collect();
            names.sort();
            names.dedup();
            names
        };
        for repo in &repo_names {
            match crate::inventory::biography::generate_segmented_biographies(&pool, repo).await {
                Ok(segs) => all_segmented.extend(segs),
                Err(e) => eprintln!("  Warning: segmented biographies for {}: {}", repo, e),
            }
        }
        all_segmented
    } else {
        Vec::new()
    };

    let mut handles = Vec::new();

    if args.element_level {
        // Element-level: multiple segments per document
        for seg_bio in &segmented {
            let document_id = format!("grepvec::bio::{}", seg_bio.qualified_name);
            let segments: Vec<Segment> = seg_bio.segments.iter().enumerate().map(|(i, s)| {
                Segment {
                    content: s.content.clone(),
                    label: s.label.clone(),
                    confidence: 1.0,
                    reasoning: format!("deterministic biography segment: {}", s.label),
                    start_paragraph: i as u32,
                    end_paragraph: i as u32,
                    metadata: serde_json::json!({
                        "repo": seg_bio.repo_name,
                        "item_type": seg_bio.item_type,
                        "qualified_name": seg_bio.qualified_name,
                        "segment_label": s.label,
                    }),
                }
            }).collect();

            let request = IngestPreparedRequest {
                collection_id: collection_id.clone(),
                document_id,
                segments,
            };

            let http = http.clone();
            let base_url = base_url.clone();
            let api_key = api_key.clone();
            let sem = semaphore.clone();
            let ok = success_count.clone();
            let err = error_count.clone();
            let total = segmented.len();
            let qname_display = seg_bio.qualified_name.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                match post_ingest_prepared(&http, &base_url, &api_key, &request).await {
                    Ok(_) => {
                        let done = ok.fetch_add(1, Ordering::Relaxed) + 1;
                        let errors = err.load(Ordering::Relaxed);
                        print!("\r  [{}/{}] {} ingested ({})", done + errors, total, qname_display, "ok".green());
                    }
                    Err(e) => {
                        let errors = err.fetch_add(1, Ordering::Relaxed) + 1;
                        let done = ok.load(Ordering::Relaxed);
                        print!("\r  [{}/{}] {} ({})", done + errors, total, qname_display, format!("error: {}", e).red());
                    }
                }
            }));
        }
    } else {
        // Whole-biography: single segment per document
        for bio in &biographies {
            let qname = bio
                .qualified_name
                .as_deref()
                .unwrap_or(&bio.item_name)
                .to_string();
            let document_id = format!("grepvec::bio::{}", qname);

            let request = IngestPreparedRequest {
                collection_id: collection_id.clone(),
                document_id,
                segments: vec![Segment {
                    content: bio.content.clone(),
                    label: "biography".to_string(),
                    confidence: 1.0,
                    reasoning: "deterministic biography from grepvec absorption".to_string(),
                    start_paragraph: 0,
                    end_paragraph: 0,
                    metadata: serde_json::json!({
                        "repo": bio.repo_name,
                        "item_type": bio.item_type,
                        "qualified_name": qname,
                    }),
                }],
            };

        let http = http.clone();
        let base_url = base_url.clone();
        let api_key = api_key.clone();
        let sem = semaphore.clone();
        let ok = success_count.clone();
        let err = error_count.clone();
        let total = biographies.len();
        let qname_display = qname.clone();

        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            match post_ingest_prepared(&http, &base_url, &api_key, &request).await {
                Ok(_) => {
                    let done = ok.fetch_add(1, Ordering::Relaxed) + 1;
                    let errors = err.load(Ordering::Relaxed);
                    print!(
                        "\r  [{}/{}] {} ingested ({})",
                        done + errors,
                        total,
                        qname_display,
                        "ok".green()
                    );
                }
                Err(e) => {
                    let errors = err.fetch_add(1, Ordering::Relaxed) + 1;
                    let done = ok.load(Ordering::Relaxed);
                    print!(
                        "\r  [{}/{}] {} ({})",
                        done + errors,
                        total,
                        qname_display,
                        format!("error: {}", e).red()
                    );
                }
            }
        }));
        }
    }

    // Await all biography ingests
    for handle in handles {
        handle.await.ok();
    }
    println!(); // newline after progress

    let bio_ok = success_count.load(Ordering::Relaxed);
    let bio_err = error_count.load(Ordering::Relaxed);

    // Ingest boundary nodes
    let mut boundary_ok = 0usize;
    let mut boundary_err = 0usize;

    if !boundary_nodes.is_empty() {
        println!(
            "\n{} Ingesting {} boundary nodes...",
            "Embed:".green().bold(),
            boundary_nodes.len()
        );

        // Reset counters for boundary phase
        success_count.store(0, Ordering::Relaxed);
        error_count.store(0, Ordering::Relaxed);

        let mut handles = Vec::new();

        for node in &boundary_nodes {
            let document_id = format!("grepvec::boundary::{}", node.name);
            let context = format!("{} ({}) —", node.crate_name, node.category);

            // Element-level segments for boundary nodes, each with crate context prepended
            let mut segments = Vec::new();
            let mut idx: u32 = 0;

            // Identity + description
            if let Some(ref desc) = node.description {
                segments.push(Segment {
                    content: format!("{} {}", context, desc),
                    label: "identity".to_string(),
                    confidence: 1.0,
                    reasoning: "boundary node identity".to_string(),
                    start_paragraph: idx, end_paragraph: idx,
                    metadata: serde_json::json!({"crate_name": node.crate_name, "category": node.category, "item_type": "boundary_node", "segment_label": "identity"}),
                });
                idx += 1;
            }

            // APIs used
            if let Some(ref apis) = node.apis_used {
                if !apis.is_empty() {
                    segments.push(Segment {
                        content: format!("{} APIs used: {}", context, apis.join(", ")),
                        label: "apis".to_string(),
                        confidence: 1.0,
                        reasoning: "boundary node APIs".to_string(),
                        start_paragraph: idx, end_paragraph: idx,
                        metadata: serde_json::json!({"crate_name": node.crate_name, "category": node.category, "item_type": "boundary_node", "segment_label": "apis"}),
                    });
                    idx += 1;
                }
            }

            // Failure impact
            if let Some(ref impact) = node.failure_impact {
                segments.push(Segment {
                    content: format!("{} Failure impact: {}", context, impact),
                    label: "failure_impact".to_string(),
                    confidence: 1.0,
                    reasoning: "boundary node failure impact".to_string(),
                    start_paragraph: idx, end_paragraph: idx,
                    metadata: serde_json::json!({"crate_name": node.crate_name, "category": node.category, "item_type": "boundary_node", "segment_label": "failure_impact"}),
                });
                idx += 1;
            }

            // Dependent repos
            if let Some(ref repos) = node.dependent_repos {
                if !repos.is_empty() {
                    segments.push(Segment {
                        content: format!("{} Used by repos: {}", context, repos.join(", ")),
                        label: "repos".to_string(),
                        confidence: 1.0,
                        reasoning: "boundary node dependent repos".to_string(),
                        start_paragraph: idx, end_paragraph: idx,
                        metadata: serde_json::json!({"crate_name": node.crate_name, "category": node.category, "item_type": "boundary_node", "segment_label": "repos"}),
                    });
                    let _ = idx;
                }
            }

            let request = IngestPreparedRequest {
                collection_id: collection_id.clone(),
                document_id,
                segments,
            };

            let http = http.clone();
            let base_url = base_url.clone();
            let api_key = api_key.clone();
            let sem = semaphore.clone();
            let ok = success_count.clone();
            let err = error_count.clone();
            let total = boundary_nodes.len();
            let name_display = node.name.clone();

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                match post_ingest_prepared(&http, &base_url, &api_key, &request).await {
                    Ok(_) => {
                        let done = ok.fetch_add(1, Ordering::Relaxed) + 1;
                        let errors = err.load(Ordering::Relaxed);
                        print!(
                            "\r  [{}/{}] {} ingested ({})",
                            done + errors,
                            total,
                            name_display,
                            "ok".green()
                        );
                    }
                    Err(e) => {
                        let errors = err.fetch_add(1, Ordering::Relaxed) + 1;
                        let done = ok.load(Ordering::Relaxed);
                        print!(
                            "\r  [{}/{}] {} ({})",
                            done + errors,
                            total,
                            name_display,
                            format!("error: {}", e).red()
                        );
                    }
                }
            }));
        }

        for handle in handles {
            handle.await.ok();
        }
        println!(); // newline after progress

        boundary_ok = success_count.load(Ordering::Relaxed);
        boundary_err = error_count.load(Ordering::Relaxed);
    }

    // Summary
    println!(
        "\n{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!("{}", "  EMBED SUMMARY".cyan().bold());
    println!(
        "{}",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );
    println!(
        "  Biographies ingested: {}",
        bio_ok.to_string().green().bold()
    );
    if bio_err > 0 {
        println!(
            "  Biography errors:     {}",
            bio_err.to_string().red().bold()
        );
    }
    if args.boundary_nodes {
        println!(
            "  Boundary ingested:    {}",
            boundary_ok.to_string().green().bold()
        );
        if boundary_err > 0 {
            println!(
                "  Boundary errors:      {}",
                boundary_err.to_string().red().bold()
            );
        }
    }
    let total_ok = bio_ok + boundary_ok;
    let total_err = bio_err + boundary_err;
    println!(
        "  Total:                {} ingested, {} errors",
        total_ok.to_string().bold(),
        if total_err == 0 {
            "0".green().bold()
        } else {
            total_err.to_string().red().bold()
        }
    );
    println!(
        "{}\n",
        "═══════════════════════════════════════════════════════════════"
            .cyan()
            .bold()
    );

    if total_err > 0 {
        std::process::exit(1);
    }
}

/// POST a single ingest-prepared request to Enscribe.
async fn post_ingest_prepared(
    http: &reqwest::Client,
    base_url: &str,
    api_key: &str,
    request: &IngestPreparedRequest,
) -> Result<Vec<IngestProgressEvent>, String> {
    let url = format!("{}/v1/ingest-prepared", base_url.trim_end_matches('/'));

    // Retry with backoff on rate limiting (429)
    let mut retries = 0;
    loop {
        let resp = http
            .post(&url)
            .header("X-API-Key", api_key)
            .header("Content-Type", "application/json")
            .json(request)
            .send()
            .await
            .map_err(|e| format!("request failed: {}", e))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            retries += 1;
            if retries > 5 {
                return Err("rate limited after 5 retries".to_string());
            }
            // Parse retry_after_seconds from response body if available
            let body = resp.text().await.unwrap_or_default();
            let wait = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["retry_after_seconds"].as_u64())
                .unwrap_or(10);
            let wait = wait.min(60).max(2); // clamp 2-60 seconds
            eprint!(" [rate limited, waiting {}s]", wait);
            tokio::time::sleep(std::time::Duration::from_secs(wait)).await;
            continue;
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(format!("HTTP {} — {}", status, body));
        }

        // Response is an array of progress events; the last one has status "complete"
        return resp.json::<Vec<IngestProgressEvent>>()
            .await
            .map_err(|e| format!("response parse failed: {}", e));
    }
}
