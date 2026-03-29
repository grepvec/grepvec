# grepvec — Code Intelligence

## What This Is

grepvec parses codebases with tree-sitter, extracts structural relationships (what calls what, what depends on what), and makes them searchable by keyword and by meaning. It serves AI agents and developers through a single CLI binary.

## Binary Architecture

Two binaries from one repo:
- `grepvec` — the CLI (search, context, read, absorb, init, mcp-server, etc.)
- `grepvec-ui` — 3D force-directed code graph visualization (eframe/wgpu)

## The Research Loop

The core workflow. Agents should use this instead of grep:
```
grepvec search "query"    → ranked results (keyword + neural)
grepvec context "name"    → callers, callees, graph neighborhood
grepvec read "name"       → exact source code for that item
```

## Project Structure

```
src/bin/grepvec.rs       — CLI entry point, subcommand dispatch, credential loading
src/bin/grepvec_ui.rs    — 3D visualization (eframe/egui/wgpu)
src/cli/*.rs             — one module per subcommand
src/canvas/*.rs          — sphere layout, classification, rendering
src/inventory/*.rs       — parsing, DB storage, biography generation, scope
src/enscribe_embed.rs    — Enscribe API client
src/agent_memory.rs      — agent memory via Enscribe
src/memory.rs            — memory store abstraction
```

## Data Stores

- **Postgres (Neon.tech)** — system of record: items, edges, biographies, boundary nodes
- **Enscribe / local Qdrant** — optional vector backend for neural search

## Credential Loading

`~/.grepvec/credentials` is auto-loaded at startup. Format:
```toml
[postgres]
url = "postgresql://..."

[enscribe]
api_key = "ensk_..."
base_url = "http://localhost:3000"
```

Environment variables override the file. Neural search auto-enables when Enscribe credentials are present.

## Configuration

`.grepvec/scope.toml` defines which repos are in scope. Created by `grepvec init`.

## Key Commands

| Command | Purpose |
|---------|---------|
| `grepvec init` | One-command setup: detect repos, create config, absorb, generate biographies |
| `grepvec search` | Keyword (tsvector) + neural search over biographies |
| `grepvec context` | Biography + N-hop graph neighborhood |
| `grepvec read` | Precise source code by item name |
| `grepvec refresh` | Session-start hook (incremental absorb) |
| `grepvec absorb` | Parse + store code inventory |
| `grepvec document` | Generate biographies |
| `grepvec reconcile` | Cross-repo edge resolution |
| `grepvec boundary` | External dependency management |
| `grepvec embed` | Bulk embed biographies to vector backend |
| `grepvec remember` | Agent memory write/recall |
| `grepvec mcp-server` | MCP protocol server for agent tool discovery |

## Agent Discovery

grepvec is discoverable by AI agents through two mechanisms:
- **MCP** — `.mcp.json` registers `grepvec_search`, `grepvec_context`, `grepvec_read` as native tools
- **Subagent** — `.grepvec/agent.md` (symlinked to `.claude/agents/grepvec.md`) provides instructions

Both are created by `grepvec init`.

## License

Elastic License 2.0 — source-available, free to use, no hosted service competition.

## Vector Backend Options

- **Enscribe** (managed) — API key, zero infrastructure, $5/dev/month + usage
- **Local** (self-hosted) — Docker + BGE + Qdrant, free, you manage it

Enscribe integration is not yet available for public use.
