# grepvec

**grep finds strings. grepvec finds meaning.**

grepvec is a code intelligence tool for developers and AI agents. It parses your codebase, maps every function, struct, and relationship, and makes it all searchable — by keyword and by meaning.

Your agent asks "how does authentication work?" and gets the exact functions, their callers, their dependencies, and the precise source code. No grep. No filesystem browsing. No context window pollution.

```
grepvec search "how does authentication work"
grepvec context "api::hmac_auth::validate_signature"
grepvec read "api::hmac_auth::validate_signature"
```

Three commands. From question to source code. Only signal, no noise.

---

## Why

AI agents are powerful reasoners — but they're only as good as what's in their context window. Every irrelevant grep match, every wrong file opened, every tangential function read is **context pollution**. The agent reasons over noise. Its decisions degrade.

grepvec eliminates this. It gives your agent a structural map of the codebase: what exists, what calls what, what depends on what. The agent gets precisely the code it needs and nothing else. Clean context leads to better reasoning. Better reasoning compounds with every subsequent query.

**grepvec doesn't make your agent faster. It makes your agent smarter.**

---

## How It Works

grepvec parses your codebase with [tree-sitter](https://tree-sitter.github.io/tree-sitter/), extracts every function, struct, enum, trait, and impl, maps all call relationships, and stores the structural graph in Postgres. It then generates a **biography** for each code item — a deterministic summary containing the item's name, signature, callers, callees, external dependencies, and location.

These biographies are searchable by keyword (tsvector full-text search) and optionally by meaning (vector embeddings via [Enscribe](https://enscribe.dev) or local BGE + Qdrant).

### The Research Loop

```bash
# 1. Search — find what you're looking for
grepvec search "how does document ingestion work"

# 2. Context — understand the item's relationships
grepvec context "api::ingest::ingest_documents"

# 3. Read — get the exact source code
grepvec read "api::ingest::ingest_documents"
```

No grep. No filesystem browsing. The agent never touches a file it doesn't need.

### Agent Integration

grepvec ships with an **MCP server** for tools that support the Model Context Protocol (Claude Code, Cursor, etc.). Your agent sees `grepvec_search`, `grepvec_context`, and `grepvec_read` as native tools — no configuration, no Bash wrapping.

For tools without MCP support, grepvec installs a **subagent instruction file** that teaches the agent the research loop.

Both are configured automatically by `grepvec init`.

---

## Quick Start

```bash
# Download grepvec (Linux x86_64)
curl -L https://grepvec.io/install | sh

# Initialize in your project directory
cd your-project
grepvec init --db-url "postgresql://..."

# Search your codebase
grepvec search "error handling"
grepvec context "handle_error"
grepvec read "handle_error"
```

### What `grepvec init` does

- Detects your repositories and languages
- Creates `~/.grepvec/credentials` (database connection)
- Creates `.grepvec/scope.toml` (project configuration)
- Parses your codebase and generates biographies
- Configures MCP server (`.mcp.json`) for agent tool discovery
- Configures subagent instructions (`.grepvec/agent.md`) for non-MCP agents

One command. Your agents have code intelligence.

---

## Vector Search Backend

grepvec's keyword search (tsvector) works out of the box with just Postgres. For semantic/neural search — where natural language queries like "how does failure handling work" return relevant code even when no keywords match — you need a vector search backend.

**Two options:**

| | Enscribe (managed) | Local (self-hosted) |
|---|---|---|
| **Setup** | API key | Docker + BGE model + Qdrant |
| **Infrastructure** | Zero — it's a service | You manage it |
| **Cost** | $5/dev/month + usage | Free (your hardware) |
| **Features** | Voice profiles, eval framework, observability, multi-tenant | Raw vector search |
| **Best for** | Teams, production, zero-ops | Solo devs, air-gapped, evaluation |

```bash
# Option 1: Enscribe (managed)
grepvec init --backend enscribe --enscribe-key ensk_...

# Option 2: Local BGE + Qdrant
grepvec init --backend local
```

Both options use the same `grepvec search` command. Switch anytime.

---

## Visualization

`grepvec-ui` renders your codebase as a 3D force-directed graph. Every node is a code item. Edges are call relationships. Clusters form naturally around tightly-coupled modules.

- Node **size** scales by connection count
- Node **color** shows architectural layer or behavioral role
- Node **shape** indicates type: diamonds (enums), hexagons (impls), circles (traits)
- Circle-select a region to drill into a focused sub-graph
- Select any node to see its full biography

Pure Rust (eframe + wgpu). GPU-accelerated. ~60fps. Renders identically on Linux, macOS, and Windows.

---

## Languages

grepvec uses tree-sitter for parsing. Currently supported:

- **Rust** (primary, most mature)
- **TypeScript / JavaScript**
- **Python**

The core engine is language-agnostic. Adding a new language means adding its tree-sitter grammar and any language-specific norms for edge detection and biography generation.

---

## Architecture

```
grepvec (CLI)                          grepvec-ui (visualization)
  ├── grepvec search                     ├── 3D force-directed sphere
  ├── grepvec context                    ├── ForceAtlas2 layout
  ├── grepvec read                       ├── Area selection + drill-in
  ├── grepvec refresh                    └── Biography panel
  ├── grepvec absorb
  ├── grepvec init
  └── grepvec mcp-server
         │
         ├── Postgres (structural data)
         │   items, edges, biographies
         │   tsvector full-text search
         │
         └── Vector backend (optional)
             Enscribe API  —or—  local BGE + Qdrant
             neural/semantic search
```

---

## Development Status

> **grepvec is in active development. It is not yet production-ready.**

### What works today

- Tree-sitter parsing for Rust, TypeScript, Python
- Structural inventory: ~5,000 items, ~9,000 edges across 4 test repositories
- Biography generation with caller/callee/dependency relationships
- Keyword search with tsvector ranking
- Graph neighborhood queries (grepvec context)
- Precise source code extraction (grepvec read)
- MCP server for AI agent tool discovery
- 3D visualization (grepvec-ui)
- Incremental absorption (sub-second refresh when code hasn't changed)
- Stale item cleanup (Postgres is always 1:1 with the codebase)
- Schema migration system

### What's in progress

- `grepvec init` interactive setup flow
- VectorBackend trait with Enscribe and local implementations
- Platform adapters for macOS and Windows (Linux works today)
- Git hooks for automatic re-absorption on commit

### What's not yet available

- **Enscribe.io integration is not yet available for public use.** The Enscribe API key option in the configuration is for internal development. Public availability will be announced at [enscribe.dev](https://enscribe.dev).
- **Local BGE + Qdrant backend** is planned but not yet implemented.
- **Pre-built binaries** are not yet available. You must build from source.
- **The install script at grepvec.io/install** does not yet exist.

---

## Building from Source

```bash
git clone https://github.com/grepvec/grepvec.git
cd grepvec
cargo build --release

# The binaries are at:
#   target/release/grepvec
#   target/release/grepvec-ui
```

Requires Rust 1.75+ and a Postgres database (Neon.tech free tier works).

---

## License

[Elastic License 2.0 (ELv2)](LICENSE)

You may use, copy, and distribute grepvec freely. You may not provide it as a hosted/managed service. You may not fork or redistribute modified versions. Source code is available for review and issue reporting at [github.com/grepvec/grepvec](https://github.com/grepvec/grepvec).
