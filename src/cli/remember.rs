//! grepvec Remember subcommand
//!
//! Records and recalls agent memories via Enscribe.

use clap::{Args, Subcommand, ValueEnum};
use colored::Colorize;
use crate::agent_memory::{AgentMemory, AgentMemoryConfig};
use crate::enscribe_embed::{EnscribeClient, EnscribeConfig, MemoryKind, MemoryLane};
use crate::memory::MemoryStore;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Args)]
pub struct RememberArgs {
    #[command(subcommand)]
    command: RememberCommand,
}

#[derive(Subcommand)]
enum RememberCommand {
    /// Write a memory entry to Enscribe
    Write {
        /// Memory lane
        #[arg(long)]
        lane: LaneArg,

        /// Memory kind
        #[arg(long)]
        kind: KindArg,

        /// The memory text to record
        message: String,

        /// Session ID (required when lane is session)
        #[arg(long)]
        session_id: Option<String>,

        /// Project ID (required when lane is project)
        #[arg(long)]
        project_id: Option<String>,
    },

    /// Recall memories by semantic search
    Recall {
        /// Search query
        query: String,

        /// Filter to a specific lane
        #[arg(long)]
        lane: Option<LaneArg>,

        /// Maximum number of results
        #[arg(long, default_value = "10")]
        limit: u32,

        /// Session ID (used when lane is session)
        #[arg(long)]
        session_id: Option<String>,

        /// Project ID (used when lane is project)
        #[arg(long)]
        project_id: Option<String>,
    },
}

#[derive(Clone, Debug, ValueEnum)]
enum LaneArg {
    Session,
    Project,
    Knowledge,
}

#[derive(Clone, Debug, ValueEnum)]
enum KindArg {
    Decision,
    Summary,
    Error,
    Trace,
}

impl KindArg {
    fn into_memory_kind(&self) -> MemoryKind {
        match self {
            KindArg::Decision => MemoryKind::Decision,
            KindArg::Summary => MemoryKind::Summary,
            KindArg::Error => MemoryKind::Error,
            KindArg::Trace => MemoryKind::Trace,
        }
    }
}

fn build_lane(lane: &LaneArg, session_id: Option<&str>, project_id: Option<&str>) -> MemoryLane {
    match lane {
        LaneArg::Session => {
            let id = session_id
                .unwrap_or("default-session")
                .to_string();
            MemoryLane::Session { session_id: id }
        }
        LaneArg::Project => {
            let id = project_id
                .unwrap_or("default-project")
                .to_string();
            MemoryLane::Project { project_id: id }
        }
        LaneArg::Knowledge => MemoryLane::Knowledge,
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn build_agent_memory() -> AgentMemory {
    let api_key = match std::env::var("ENSCRIBE_API_KEY") {
        Ok(k) if !k.is_empty() => k,
        _ => {
            eprintln!("{} ENSCRIBE_API_KEY not set", "Error:".red().bold());
            std::process::exit(1);
        }
    };

    let base_url = std::env::var("ENSCRIBE_BASE_URL")
        .unwrap_or_else(|_| "http://localhost:3000".to_string());

    let openai_key = std::env::var("ENSCRIBE_OPENAI_KEY").ok();

    let tenant_id = std::env::var("ENSCRIBE_TENANT_ID")
        .unwrap_or_else(|_| "grepvec-local".to_string());

    let node_id = std::env::var("GREPVEC_NODE_ID")
        .unwrap_or_else(|_| "grepvec-agent".to_string());

    let agent_id = std::env::var("GREPVEC_AGENT_ID")
        .unwrap_or_else(|_| "grepvec-remember-cli".to_string());

    let config = EnscribeConfig {
        base_url,
        api_key,
        openai_key,
    };
    let client = EnscribeClient::new(config);
    let store = MemoryStore::new(client, tenant_id);

    AgentMemory::new(
        store,
        AgentMemoryConfig {
            node_id,
            agent_id,
            dsl_version: "0.1".to_string(),
            source: "grepvec-remember-cli".to_string(),
        },
    )
}

pub async fn run(args: RememberArgs) {
    match args.command {
        RememberCommand::Write {
            lane,
            kind,
            message,
            session_id,
            project_id,
        } => {
            let memory = build_agent_memory();
            let memory_lane = build_lane(&lane, session_id.as_deref(), project_id.as_deref());
            let memory_kind = kind.into_memory_kind();
            let timestamp_ms = now_epoch_ms();
            let created_at = format!("{:013}", timestamp_ms);

            println!(
                "{} Writing {} {} to {} lane...",
                "grepvec remember".cyan().bold(),
                kind.into_memory_kind().as_str().bold(),
                "memory".dimmed(),
                format!("{:?}", lane).to_lowercase().bold(),
            );

            match memory
                .record(
                    memory_lane,
                    memory_kind,
                    &message,
                    created_at,
                    timestamp_ms,
                    None,
                )
                .await
            {
                Ok(response) => {
                    println!(
                        "{} Recorded (processed={}, embeddings={}, tokens={})",
                        "OK:".green().bold(),
                        response.processed_count,
                        response.new_embeddings_count,
                        response.total_tokens_used,
                    );
                }
                Err(err) => {
                    eprintln!("{} {}", "Error:".red().bold(), err);
                    std::process::exit(1);
                }
            }
        }

        RememberCommand::Recall {
            query,
            lane,
            limit,
            session_id,
            project_id,
        } => {
            let memory = build_agent_memory();

            println!(
                "{} Searching for: {}",
                "grepvec remember".cyan().bold(),
                query.bold(),
            );

            if let Some(ref lane_filter) = lane {
                // Search a specific lane directly via the store
                let memory_lane = build_lane(lane_filter, session_id.as_deref(), project_id.as_deref());

                match memory
                    .store()
                    .recall_lane(
                        &memory_lane,
                        &std::env::var("GREPVEC_NODE_ID").unwrap_or_else(|_| "grepvec-agent".to_string()),
                        &query,
                        limit,
                        None,
                    )
                    .await
                {
                    Ok(results) => {
                        if results.is_empty() {
                            println!("{} No memories found.", "Info:".yellow().bold());
                            return;
                        }
                        println!(
                            "{} {} result(s)\n",
                            "Found:".green().bold(),
                            results.len(),
                        );
                        for (i, snippet) in results.iter().enumerate() {
                            println!(
                                "{}",
                                format!("--- Result {} ---", i + 1).dimmed()
                            );
                            println!("  {} {:.4}", "score:".bold(), snippet.score);
                            if let Some(ref header) = snippet.header {
                                println!("  {} {}", "kind:".bold(), header.memory_kind);
                                println!("  {} {}", "created:".bold(), header.created_at);
                                println!("  {} {}", "agent:".bold(), header.agent_id);
                            }
                            println!("  {} {}", "body:".bold(), snippet.body);
                            println!();
                        }
                    }
                    Err(err) => {
                        eprintln!("{} {}", "Error:".red().bold(), err);
                        std::process::exit(1);
                    }
                }
            } else {
                // Search across all lanes via recall()
                let report = memory
                    .recall(
                        session_id.as_deref(),
                        project_id.as_deref(),
                        &query,
                    )
                    .await;

                for failure in &report.failures {
                    eprintln!(
                        "{} {} lane: {}",
                        "Warning:".yellow().bold(),
                        failure.lane,
                        failure.message,
                    );
                }

                let results = &report.results;
                let display_count = if limit > 0 {
                    (limit as usize).min(results.len())
                } else {
                    results.len()
                };

                if results.is_empty() {
                    println!("{} No memories found.", "Info:".yellow().bold());
                    return;
                }

                println!(
                    "{} {} result(s)\n",
                    "Found:".green().bold(),
                    display_count,
                );

                for (i, snippet) in results.iter().take(display_count).enumerate() {
                    println!(
                        "{}",
                        format!("--- Result {} ---", i + 1).dimmed()
                    );
                    println!("  {} {:.4}", "score:".bold(), snippet.score);
                    println!("  {} {}", "document:".bold(), snippet.document_id);
                    if let Some(ref header) = snippet.header {
                        println!("  {} {}", "lane:".bold(), header.memory_type);
                        println!("  {} {}", "kind:".bold(), header.memory_kind);
                        println!("  {} {}", "created:".bold(), header.created_at);
                        println!("  {} {}", "agent:".bold(), header.agent_id);
                    }
                    println!("  {} {}", "body:".bold(), snippet.body);
                    println!();
                }
            }
        }
    }
}
