//! grepvec Init subcommand — One-command project setup
//!
//! Creates all configuration files, runs initial absorption, and sets up
//! both MCP and subagent discovery. After `grepvec init`, the developer's
//! agentic tools have grepvec available automatically.

use clap::Args;
use colored::Colorize;
use std::path::{Path, PathBuf};

#[derive(Args)]
pub struct InitArgs {
    /// Repository directories to absorb (auto-detected if not provided)
    #[arg(long)]
    repos: Vec<String>,

    /// Postgres connection URL (auto-provisioned via Docker if not provided)
    #[arg(long)]
    db_url: Option<String>,

    /// Use managed backend (Enscribe API key required)
    #[arg(long)]
    managed: bool,

    /// Enscribe API key (optional, enables neural search)
    #[arg(long)]
    enscribe_key: Option<String>,

    /// Enscribe API base URL
    #[arg(long, default_value = "http://localhost:3000")]
    enscribe_url: String,

    /// Enscribe collection ID (optional)
    #[arg(long)]
    collection_id: Option<String>,

    /// Skip absorption (only create config files)
    #[arg(long)]
    config_only: bool,
}

const AGENT_INSTRUCTIONS: &str = r#"# grepvec — Code Intelligence

grepvec provides structural code intelligence: what exists, what calls what, what depends on what. Use grepvec instead of grep or filesystem browsing. grepvec returns only signal — no noise, no context window pollution.

## Setup

Credentials are auto-loaded from `~/.grepvec/credentials`. No environment variables needed. If `grepvec refresh` works, you're ready.

## The Research Loop

**Always use this pattern. Do not grep the filesystem.**

```bash
# 1. Search — find what you're looking for (neural + keyword, automatic)
grepvec search "how does document ingestion work"

# 2. Context — understand the item's relationships
grepvec context "api::ingest::ingest_documents"

# 3. Read — get the exact source code
grepvec read "api::ingest::ingest_documents"
```

`grepvec search` returns ranked results with names, file paths, callers, and callees. Neural search runs automatically when Enscribe credentials are configured.

`grepvec context` returns the full graph neighborhood: who calls this function, what it calls downstream, external dependencies, file location, and line count.

`grepvec read` fetches the precise source code for an item — exactly the lines for that function, struct, or impl. No more, no less. Use `-C 10` for 10 lines of surrounding context.

## Why This Matters

Every grep match, every wrong file opened, every irrelevant function read — that's context window pollution. grepvec eliminates this: search returns structural signal, context shows the graph, read delivers exact code. Your context window stays clean. Your reasoning stays sharp.

## Session Start

```bash
grepvec refresh
```

Incremental — sub-second when nothing changed. Absorbs new code, regenerates stale biographies.

## Other Commands

```bash
grepvec boundary list              # external dependencies
grepvec boundary gaps              # unresolved edges
grepvec absorb --repo <name>       # re-parse a repo
grepvec document --all             # regenerate all biographies
grepvec reconcile --edges --report # cross-repo edge resolution
```

## Scope

Defined in `.grepvec/scope.toml`. The inventory is a 1:1 mirror of the codebase.
"#;

/// Detected repository info.
struct DetectedRepo {
    name: String,
    path: PathBuf,
    language: String,
}

pub fn run(args: InitArgs) {
    println!("\n{}\n", "GREPVEC INIT".cyan().bold());

    // Step 1: Detect or use provided repo paths
    let repos = if args.repos.is_empty() {
        detect_repos()
    } else {
        args.repos
            .iter()
            .map(|r| {
                let path = PathBuf::from(r).canonicalize().unwrap_or_else(|_| PathBuf::from(r));
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| r.clone());
                let language = detect_language(&path);
                DetectedRepo {
                    name,
                    path,
                    language,
                }
            })
            .collect()
    };

    if repos.is_empty() {
        eprintln!(
            "  {} No repositories detected. Use --repos to specify paths.",
            "Error:".red().bold()
        );
        std::process::exit(1);
    }

    println!("  {}:", "Detected repos".bold());
    for repo in &repos {
        println!(
            "    {} ({}) [{}]",
            repo.name.green(),
            repo.path.display().to_string().dimmed(),
            repo.language.cyan()
        );
    }
    println!();

    // Step 2: Create ~/.grepvec/ directory
    let home_gv = dirs::home_dir()
        .expect("Cannot determine home directory")
        .join(".grepvec");
    if !home_gv.exists() {
        std::fs::create_dir_all(&home_gv).expect("Failed to create ~/.grepvec/");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home_gv, std::fs::Permissions::from_mode(0o700)).ok();
        }
    }

    // Step 3: Provision database + create credentials
    let cred_path = home_gv.join("credentials");
    if cred_path.exists() {
        println!(
            "  {} {}",
            "Exists:".yellow().bold(),
            "~/.grepvec/credentials (not overwritten)"
        );
    } else {
        let db_url = if let Some(ref url) = args.db_url {
            // Explicit DB URL provided
            println!("  {} Using provided database URL", "Database:".cyan().bold());
            url.clone()
        } else if args.managed {
            // Managed mode — require explicit URL or Enscribe setup (future)
            eprintln!(
                "  {} --managed requires --db-url (managed provisioning coming soon)",
                "Error:".red().bold()
            );
            std::process::exit(1);
        } else {
            // Default: local Postgres via Docker
            provision_local_postgres()
        };

        let enscribe_key = args.enscribe_key.as_deref().unwrap_or("");
        let enscribe_url = &args.enscribe_url;

        let cred_content = format!(
            r#"# grepvec credentials — do not commit to version control

[postgres]
url = "{}"

[enscribe]
api_key = "{}"
base_url = "{}"
"#,
            db_url, enscribe_key, enscribe_url
        );

        std::fs::write(&cred_path, &cred_content).expect("Failed to write ~/.grepvec/credentials");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cred_path, std::fs::Permissions::from_mode(0o600)).ok();
        }
        println!("  {} ~/.grepvec/credentials", "Created:".green().bold());
    }

    // Step 4: Create .grepvec/ directory in cwd
    let cwd = std::env::current_dir().expect("Cannot determine current directory");
    let gv_dir = cwd.join(".grepvec");
    std::fs::create_dir_all(&gv_dir).expect("Failed to create .grepvec/");

    // Step 5: Create .grepvec/scope.toml
    let scope_path = gv_dir.join("scope.toml");
    if scope_path.exists() {
        println!(
            "  {} {}",
            "Exists:".yellow().bold(),
            ".grepvec/scope.toml (not overwritten)"
        );
    } else {
        let scope = crate::inventory::scope::ShiftScope {
            repos: repos
                .iter()
                .map(|r| crate::inventory::scope::RepoScope {
                    name: r.name.clone(),
                    path: r.path.display().to_string(),
                    language: r.language.clone(),
                    last_sha: None,
                })
                .collect(),
            enscribe: args.collection_id.as_ref().map(|id| {
                crate::inventory::scope::EnscribeScope {
                    collection: id.clone(),
                    voices: Vec::new(),
                }
            }),
        };

        crate::inventory::scope::write_scope(&cwd, &scope)
            .expect("Failed to write .grepvec/scope.toml");

        println!(
            "  {} .grepvec/scope.toml ({} repos)",
            "Created:".green().bold(),
            repos.len()
        );
    }

    // Step 6: Create .grepvec/agent.md
    let agent_path = gv_dir.join("agent.md");
    if agent_path.exists() {
        println!(
            "  {} {}",
            "Exists:".yellow().bold(),
            ".grepvec/agent.md (not overwritten)"
        );
    } else {
        std::fs::write(&agent_path, AGENT_INSTRUCTIONS).expect("Failed to write .grepvec/agent.md");
        println!("  {} .grepvec/agent.md", "Created:".green().bold());
    }

    // Step 7: Create .claude/agents/grepvec.md symlink
    let claude_agents_dir = cwd.join(".claude").join("agents");
    let symlink_path = claude_agents_dir.join("grepvec.md");
    if symlink_path.exists() {
        println!(
            "  {} {}",
            "Exists:".yellow().bold(),
            ".claude/agents/grepvec.md (not overwritten)"
        );
    } else {
        std::fs::create_dir_all(&claude_agents_dir)
            .expect("Failed to create .claude/agents/");
        // Relative symlink: from .claude/agents/ to ../../.grepvec/agent.md
        let target = Path::new("../../.grepvec/agent.md");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, &symlink_path)
                .expect("Failed to create symlink .claude/agents/grepvec.md");
        }
        #[cfg(not(unix))]
        {
            // On non-Unix, copy instead of symlink
            std::fs::copy(&agent_path, &symlink_path)
                .expect("Failed to copy agent.md to .claude/agents/grepvec.md");
        }
        println!(
            "  {} .claude/agents/grepvec.md {} .grepvec/agent.md",
            "Created:".green().bold(),
            "→".dimmed()
        );
    }

    // Step 8: Create .mcp.json
    let mcp_path = cwd.join(".mcp.json");
    if mcp_path.exists() {
        println!(
            "  {} {}",
            "Exists:".yellow().bold(),
            ".mcp.json (not overwritten)"
        );
    } else {
        let exe_path = std::env::current_exe()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| "grepvec".to_string());

        let mcp_config = serde_json::json!({
            "mcpServers": {
                "grepvec": {
                    "command": exe_path,
                    "args": ["mcp-server"]
                }
            }
        });

        let mcp_content =
            serde_json::to_string_pretty(&mcp_config).expect("Failed to serialize .mcp.json");
        std::fs::write(&mcp_path, &mcp_content).expect("Failed to write .mcp.json");
        println!("  {} .mcp.json (MCP server)", "Created:".green().bold());
    }

    // Step 9: Run absorption (unless --config-only)
    if args.config_only {
        println!(
            "\n  {} Skipping absorption (--config-only)",
            "Note:".yellow().bold()
        );
    } else {
        let exe_path = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("grepvec"));

        println!("\n  {}...", "Absorbing".cyan().bold());

        let absorb_status = std::process::Command::new(&exe_path)
            .args(["absorb", "--all"])
            .status();

        match absorb_status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!(
                    "  {} grepvec absorb exited with {}",
                    "Warning:".yellow().bold(),
                    status
                );
            }
            Err(e) => {
                eprintln!(
                    "  {} Failed to run grepvec absorb: {}",
                    "Warning:".yellow().bold(),
                    e
                );
            }
        }

        println!("\n  {}...", "Generating biographies".cyan().bold());

        let doc_status = std::process::Command::new(&exe_path)
            .args(["document", "--all"])
            .status();

        match doc_status {
            Ok(status) if status.success() => {}
            Ok(status) => {
                eprintln!(
                    "  {} grepvec document exited with {}",
                    "Warning:".yellow().bold(),
                    status
                );
            }
            Err(e) => {
                eprintln!(
                    "  {} Failed to run grepvec document: {}",
                    "Warning:".yellow().bold(),
                    e
                );
            }
        }
    }

    println!(
        "\n  {}\n",
        "grepvec is ready. Your agents will discover it automatically."
            .green()
            .bold()
    );
}

/// Auto-detect repositories in the current directory and one level up.
fn detect_repos() -> Vec<DetectedRepo> {
    let cwd = std::env::current_dir().unwrap_or_default();
    let mut repos = Vec::new();
    let mut seen = std::collections::HashSet::new();

    // Check current directory itself
    if is_repo_root(&cwd) {
        let canonical = cwd.canonicalize().unwrap_or_else(|_| cwd.clone());
        if seen.insert(canonical.clone()) {
            repos.push(DetectedRepo {
                name: canonical
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "project".to_string()),
                language: detect_language(&canonical),
                path: canonical,
            });
        }
    }

    // Check immediate subdirectories
    check_children(&cwd, &mut repos, &mut seen);

    // Check one level up
    if let Some(parent) = cwd.parent() {
        if is_repo_root(parent) {
            let canonical = parent.canonicalize().unwrap_or_else(|_| parent.to_path_buf());
            if seen.insert(canonical.clone()) {
                repos.push(DetectedRepo {
                    name: canonical
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| "project".to_string()),
                    language: detect_language(&canonical),
                    path: canonical,
                });
            }
        }
        // Check siblings (children of parent)
        check_children(parent, &mut repos, &mut seen);
    }

    repos
}

/// Check immediate children of a directory for repo markers.
fn check_children(
    dir: &Path,
    repos: &mut Vec<DetectedRepo>,
    seen: &mut std::collections::HashSet<PathBuf>,
) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && is_repo_root(&path) {
                let canonical = path.canonicalize().unwrap_or(path);
                if seen.insert(canonical.clone()) {
                    repos.push(DetectedRepo {
                        name: canonical
                            .file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "unknown".to_string()),
                        language: detect_language(&canonical),
                        path: canonical,
                    });
                }
            }
        }
    }
}

/// Check if a directory looks like a project/repo root.
fn is_repo_root(dir: &Path) -> bool {
    let markers = ["Cargo.toml", "package.json", "pyproject.toml", "requirements.txt"];
    markers.iter().any(|m| dir.join(m).exists())
}

/// Provision a local Postgres database via Docker.
/// Returns the connection URL.
fn provision_local_postgres() -> String {
    println!("  {} Setting up local Postgres via Docker...", "Database:".cyan().bold());

    // Check if Docker is available
    let docker_check = std::process::Command::new("docker")
        .arg("info")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    match docker_check {
        Ok(status) if status.success() => {}
        _ => {
            eprintln!(
                "  {} Docker is required for local mode but was not found.",
                "Error:".red().bold()
            );
            eprintln!("  Install Docker: https://docs.docker.com/get-docker/");
            eprintln!("  Or provide a Postgres URL: grepvec init --db-url postgresql://...");
            std::process::exit(1);
        }
    }

    // Check if grepvec-db container already exists and is running
    let ps_output = std::process::Command::new("docker")
        .args(["ps", "-a", "--filter", "name=grepvec-db", "--format", "{{.Status}}"])
        .output();

    if let Ok(output) = ps_output {
        let status_str = String::from_utf8_lossy(&output.stdout);
        let status_str = status_str.trim();
        if !status_str.is_empty() {
            if status_str.starts_with("Up") {
                println!("    {} grepvec-db container already running", "✓".green());
                return "postgresql://grepvec:grepvec@localhost:5432/grepvec".to_string();
            } else {
                // Container exists but stopped — start it
                println!("    Starting existing grepvec-db container...");
                let start = std::process::Command::new("docker")
                    .args(["start", "grepvec-db"])
                    .stdout(std::process::Stdio::null())
                    .status();
                if let Ok(s) = start {
                    if s.success() {
                        // Wait for Postgres to be ready
                        wait_for_postgres();
                        println!("    {} grepvec-db container started", "✓".green());
                        return "postgresql://grepvec:grepvec@localhost:5432/grepvec".to_string();
                    }
                }
            }
        }
    }

    // No existing container — create one
    println!("    Pulling postgres:16-alpine...");
    let pull = std::process::Command::new("docker")
        .args(["pull", "postgres:16-alpine"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if let Ok(s) = pull {
        if !s.success() {
            eprintln!("  {} Failed to pull postgres:16-alpine", "Warning:".yellow().bold());
        }
    }

    println!("    Starting grepvec-db container...");
    let run = std::process::Command::new("docker")
        .args([
            "run", "-d",
            "--name", "grepvec-db",
            "-p", "5432:5432",
            "-e", "POSTGRES_DB=grepvec",
            "-e", "POSTGRES_USER=grepvec",
            "-e", "POSTGRES_PASSWORD=grepvec",
            "-v", "grepvec-pgdata:/var/lib/postgresql/data",
            "--restart", "unless-stopped",
            "postgres:16-alpine",
        ])
        .output();

    match run {
        Ok(output) if output.status.success() => {
            wait_for_postgres();
            println!("    {} grepvec-db container running (port 5432)", "✓".green());
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("is already in use") {
                println!("    {} port 5432 already in use — using existing Postgres", "✓".green());
            } else {
                eprintln!(
                    "  {} Failed to start container: {}",
                    "Error:".red().bold(),
                    stderr.trim()
                );
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("  {} Docker run failed: {}", "Error:".red().bold(), e);
            std::process::exit(1);
        }
    }

    "postgresql://grepvec:grepvec@localhost:5432/grepvec".to_string()
}

/// Wait for Postgres to accept connections (up to 15 seconds).
fn wait_for_postgres() {
    for _ in 0..30 {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let check = std::process::Command::new("docker")
            .args(["exec", "grepvec-db", "pg_isready", "-U", "grepvec"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Ok(s) = check {
            if s.success() {
                return;
            }
        }
    }
    eprintln!(
        "  {} Postgres did not become ready within 15 seconds",
        "Warning:".yellow().bold()
    );
}

/// Detect the primary language from project files.
fn detect_language(dir: &Path) -> String {
    if dir.join("Cargo.toml").exists() {
        "rust".to_string()
    } else if dir.join("package.json").exists() {
        "typescript".to_string()
    } else if dir.join("pyproject.toml").exists() || dir.join("requirements.txt").exists() {
        "python".to_string()
    } else {
        "rust".to_string()
    }
}
