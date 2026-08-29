//! MCP stdio transport: newline-delimited JSON-RPC 2.0.
//! Default listed tools = `explore` + `status` only (codegraph's one-tool idea).
//! Narrow tools stay callable but hidden unless `CODESCRATCH_MCP_TOOLS` lists them.
//! Hand-rolled (no SDK dep) — only a handful of methods, keeps the binary lean.

use crate::query;
use anyhow::Result;
use serde_json::{json, Value};
use std::io::{BufRead, Write};
use std::path::Path;

pub fn serve(root: &Path) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg): Result<Value, _> = serde_json::from_str(&line) else {
            continue;
        };
        // Notifications have no id → nothing to answer.
        let Some(id) = msg.get("id").cloned() else {
            continue;
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let reply = match method {
            "initialize" => ok(id, initialize()),
            "tools/list" => ok(id, json!({ "tools": tool_specs() })),
            "tools/call" => match call_tool(root, &msg) {
                Ok(text) => ok(id, tool_result(&text, false)),
                Err(e) => ok(id, tool_result(&format!("error: {e}"), true)),
            },
            "ping" => ok(id, json!({})),
            _ => err(id, -32601, "method not found"),
        };
        writeln!(out, "{}", reply)?;
        out.flush()?;
    }
    Ok(())
}

fn initialize() -> Value {
    json!({
        "protocolVersion": "2024-11-05",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "codescratch", "version": "0.1.0" }
    })
}

fn listed() -> Vec<&'static str> {
    let mut v = vec!["explore", "status"];
    if let Ok(extra) = std::env::var("CODESCRATCH_MCP_TOOLS") {
        for name in extra.split(',').map(|s| s.trim()) {
            if matches!(name, "search" | "explore" | "status") && !v.contains(&name) {
                // leak-free: map &str to 'static by matching known names
                match name {
                    "search" => v.push("search"),
                    _ => {}
                }
            }
        }
    }
    v
}

fn tool_specs() -> Vec<Value> {
    listed()
        .into_iter()
        .map(|name| match name {
            "explore" => json!({
                "name": "explore",
                "description": "One question → trust banner + verbatim source + calls + callers (blast radius) for a symbol. Start here.",
                "inputSchema": {
                    "type": "object",
                    "properties": { "symbol": { "type": "string", "description": "symbol name to explore" } },
                    "required": ["symbol"]
                }
            }),
            "status" => json!({
                "name": "status",
                "description": "Trust banner: freshness × coverage × graph quality. Call when unsure the graph exists.",
                "inputSchema": { "type": "object", "properties": {} }
            }),
            "search" => json!({
                "name": "search",
                "description": "Fuzzy find a symbol by name (FTS).",
                "inputSchema": {
                    "type": "object",
                    "properties": { "query": { "type": "string" } },
                    "required": ["query"]
                }
            }),
            _ => json!({}),
        })
        .collect()
}

fn call_tool(root: &Path, msg: &Value) -> Result<String> {
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
    let args = params.get("arguments").cloned().unwrap_or(json!({}));
    match name {
        "status" => query::status(root),
        "explore" => {
            let sym = args.get("symbol").and_then(|s| s.as_str()).unwrap_or("");
            query::explore(root, sym)
        }
        "search" => {
            let q = args.get("query").and_then(|s| s.as_str()).unwrap_or("");
            query::search(root, q)
        }
        other => Ok(format!("unknown tool `{other}`")),
    }
}

fn tool_result(text: &str, is_error: bool) -> Value {
    json!({ "content": [ { "type": "text", "text": text } ], "isError": is_error })
}

fn ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn err(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
