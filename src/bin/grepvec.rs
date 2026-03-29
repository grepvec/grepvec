use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "grepvec")]
#[command(about = "grepvec — Code Intelligence")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Parse and store code inventory
    Absorb(grepvec::cli::absorb::AbsorbArgs),
    /// Resolve cross-repo edges
    Reconcile(grepvec::cli::reconcile::ReconcileArgs),
    /// Generate biographies
    Document(grepvec::cli::document::DocumentArgs),
    /// Search code biographies
    Search(grepvec::cli::search::SearchArgs),
    /// Biography + graph neighborhood
    Context(grepvec::cli::context::ContextArgs),
    /// Read source code for a specific item
    Read(grepvec::cli::read::ReadArgs),
    /// Session-start hook
    Refresh(grepvec::cli::refresh::RefreshArgs),
    /// Boundary node management
    Boundary(grepvec::cli::boundary::BoundaryArgs),
    /// Embed biographies to Enscribe
    Embed(grepvec::cli::embed::EmbedArgs),
    /// Agent memory write/recall
    Remember(grepvec::cli::remember::RememberArgs),
    /// MCP server for AI agent tool discovery
    #[command(name = "mcp-server")]
    McpServer,
    /// Initialize grepvec for a project
    Init(grepvec::cli::init::InitArgs),
}

/// Load credentials from ~/.grepvec/credentials and .grepvec/scope.toml.
/// Sets environment variables as defaults (env vars still override).
fn load_config() {
    // 1. Load ~/.grepvec/credentials (TOML: [enscribe] api_key, base_url; [postgres] url)
    let cred_path = dirs().map(|p| p.join("credentials"));
    if let Some(path) = cred_path {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(doc) = content.parse::<toml::Table>() {
                    // [enscribe] section
                    if let Some(enscribe) = doc.get("enscribe").and_then(|v| v.as_table()) {
                        set_default("ENSCRIBE_API_KEY", enscribe.get("api_key"));
                        set_default("ENSCRIBE_BASE_URL", enscribe.get("base_url"));
                    }
                    // [postgres] section
                    if let Some(postgres) = doc.get("postgres").and_then(|v| v.as_table()) {
                        set_default("TOWER_DB_URL", postgres.get("url"));
                    }
                    // [vector] section
                    if let Some(vector) = doc.get("vector").and_then(|v| v.as_table()) {
                        set_default("GREPVEC_VECTOR_BACKEND", vector.get("backend"));
                        set_default("GREPVEC_QDRANT_URL", vector.get("qdrant_url"));
                        set_default("GREPVEC_BGE_URL", vector.get("bge_url"));
                    }
                }
            }
        }
    }

    // 2. Load .grepvec/scope.toml for collection ID (walk up from cwd)
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(scope_path) = grepvec::inventory::scope::find_scope_file(&cwd) {
            if let Ok(scope) = grepvec::inventory::scope::read_scope(&scope_path) {
                if let Some(ref ens) = scope.enscribe {
                    // Make collection available for neural search
                    set_default("GREPVEC_COLLECTION", Some(&toml::Value::String(ens.collection.clone())));
                }
            }
        }
    }
}

/// Set an env var only if it's not already set.
fn set_default(key: &str, val: Option<&toml::Value>) {
    if std::env::var(key).is_err() {
        if let Some(toml::Value::String(s)) = val {
            std::env::set_var(key, s);
        }
    }
}

/// Return the ~/.grepvec/ directory path.
fn dirs() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".grepvec"))
}

#[tokio::main]
async fn main() {
    load_config();

    let cli = Cli::parse();
    match cli.command {
        Commands::Absorb(args) => grepvec::cli::absorb::run(args).await,
        Commands::Reconcile(args) => grepvec::cli::reconcile::run(args).await,
        Commands::Document(args) => grepvec::cli::document::run(args).await,
        Commands::Search(args) => grepvec::cli::search::run(args).await,
        Commands::Context(args) => grepvec::cli::context::run(args).await,
        Commands::Read(args) => grepvec::cli::read::run(args).await,
        Commands::Refresh(args) => grepvec::cli::refresh::run(args).await,
        Commands::Boundary(args) => grepvec::cli::boundary::run(args).await,
        Commands::Embed(args) => grepvec::cli::embed::run(args).await,
        Commands::Remember(args) => grepvec::cli::remember::run(args).await,
        Commands::McpServer => grepvec::cli::mcp::run(),
        Commands::Init(args) => grepvec::cli::init::run(args),
    }
}
