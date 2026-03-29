//! MCP (Model Context Protocol) server for grepvec.
//!
//! Implements JSON-RPC 2.0 over stdio so AI agents (Claude Code, etc.) can
//! discover and invoke grepvec tools via the standard MCP handshake.
//!
//! Protocol flow:
//!   1. Client sends `initialize` -> we respond with capabilities
//!   2. Client sends `notifications/initialized` -> no response
//!   3. Client sends `tools/list` -> we return tool definitions
//!   4. Client sends `tools/call` -> we shell out to `grepvec <subcommand>` and return output

use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::process::Command;

/// Tool definitions served to MCP clients.
fn tool_definitions() -> Value {
    json!({
        "tools": [
            {
                "name": "grepvec_search",
                "description": "Search code biographies by keyword and semantic similarity. Returns ranked results with function names, file paths, callers, callees, and structural context. Use this instead of grep — it returns only signal, no noise.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "Search query — can be keywords or natural language questions like 'how does ingest work'"
                        },
                        "repo": {
                            "type": "string",
                            "description": "Optional: filter to a specific repository name"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum results to return (default 10)"
                        }
                    },
                    "required": ["query"]
                }
            },
            {
                "name": "grepvec_context",
                "description": "Get the full graph neighborhood for a code item: biography, callers, callees, external dependencies, file location. Use after grepvec_search to understand an item's relationships.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Item name or qualified name (e.g., 'api::ingest::ingest_documents')"
                        },
                        "repo": {
                            "type": "string",
                            "description": "Optional: filter to a specific repository"
                        },
                        "hops": {
                            "type": "integer",
                            "description": "Graph neighborhood depth (default 1)"
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "grepvec_read",
                "description": "Fetch the exact source code for a code item by name. Returns only the lines for that function, struct, or impl — no surrounding noise. Use after grepvec_search or grepvec_context to read the actual implementation.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Item name or qualified name to read"
                        },
                        "repo": {
                            "type": "string",
                            "description": "Optional: filter to a specific repository"
                        },
                        "context_lines": {
                            "type": "integer",
                            "description": "Lines of surrounding context before and after (default 0)"
                        }
                    },
                    "required": ["name"]
                }
            },
            {
                "name": "grepvec_refresh",
                "description": "Session-start hook: incrementally absorb code changes and regenerate stale biographies. Run this at the start of a session.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        ]
    })
}

/// Execute a grepvec tool by shelling out to the grepvec binary.
fn execute_tool(name: &str, args: &Value) -> String {
    let exe = std::env::current_exe().unwrap_or_else(|_| "grepvec".into());

    let mut cmd = Command::new(&exe);

    match name {
        "grepvec_search" => {
            cmd.arg("search");
            if let Some(query) = args.get("query").and_then(|v| v.as_str()) {
                cmd.arg(query);
            }
            if let Some(repo) = args.get("repo").and_then(|v| v.as_str()) {
                cmd.arg("--repo").arg(repo);
            }
            if let Some(limit) = args.get("limit").and_then(|v| v.as_i64()) {
                cmd.arg("--limit").arg(limit.to_string());
            }
            // Disable neural for MCP (avoid double latency)
            cmd.arg("--no-neural");
        }
        "grepvec_context" => {
            cmd.arg("context");
            if let Some(n) = args.get("name").and_then(|v| v.as_str()) {
                cmd.arg(n);
            }
            if let Some(repo) = args.get("repo").and_then(|v| v.as_str()) {
                cmd.arg("--repo").arg(repo);
            }
            if let Some(hops) = args.get("hops").and_then(|v| v.as_i64()) {
                cmd.arg("--hops").arg(hops.to_string());
            }
        }
        "grepvec_read" => {
            cmd.arg("read");
            if let Some(n) = args.get("name").and_then(|v| v.as_str()) {
                cmd.arg(n);
            }
            if let Some(repo) = args.get("repo").and_then(|v| v.as_str()) {
                cmd.arg("--repo").arg(repo);
            }
            if let Some(ctx) = args.get("context_lines").and_then(|v| v.as_i64()) {
                cmd.arg("-C").arg(ctx.to_string());
            }
        }
        "grepvec_refresh" => {
            cmd.arg("refresh");
        }
        _ => return format!("Unknown tool: {}", name),
    }

    match cmd.output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() && stdout.is_empty() {
                stderr.to_string()
            } else {
                stdout.to_string()
            }
        }
        Err(e) => format!("Failed to execute: {}", e),
    }
}

/// Run the MCP server. Synchronous stdin/stdout loop.
pub fn run() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        if line.trim().is_empty() {
            continue;
        }

        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("MCP: failed to parse JSON: {}", e);
                continue;
            }
        };

        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id");

        // Notifications (no id) don't get responses
        if id.is_none() {
            continue;
        }

        let response = match method {
            "initialize" => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "grepvec", "version": "0.1.0" }
                    }
                })
            }
            "tools/list" => {
                let tools = tool_definitions();
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": tools
                })
            }
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(json!({}));
                let tool_name = params
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let tool_args = params.get("arguments").cloned().unwrap_or(json!({}));

                let output = execute_tool(tool_name, &tool_args);

                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [
                            {
                                "type": "text",
                                "text": output
                            }
                        ]
                    }
                })
            }
            _ => {
                json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32601,
                        "message": format!("Unknown method: {}", method)
                    }
                })
            }
        };

        // Write compact JSON, one line per response
        if let Ok(json_str) = serde_json::to_string(&response) {
            let _ = writeln!(stdout, "{}", json_str);
            let _ = stdout.flush();
        }
    }
}
