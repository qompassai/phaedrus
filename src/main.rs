//! phaedrus — MCP server for the research feed (stdio, JSON-RPC).
//!
//! Serves the estate's literature watch: the curated findings store
//! and the watchlist, plus the one-way revenue seam into lupin
//! (draft-only; lupin owns its ledger). Named by the Owner (2026-10-19)
//! for Pirsig's Phaedrus — the pursuit of Quality before adoption.

mod store;

use serde_json::{json, Value};
use std::env;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use store::{load, revenue_draft, summary, Store};

struct Config {
    root: PathBuf,
    lupin_root: PathBuf,
}

fn tool_schemas() -> Value {
    json!({"tools": [
        {"name": "finding_list",
         "description": "List curated findings, optionally filtered by system.",
         "inputSchema": {"type": "object", "properties": {"system": {"type": "string"}}}},
        {"name": "finding_get",
         "description": "Get one curated finding by id.",
         "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}},
        {"name": "research_submit",
         "description": "File a CANDIDATE (finding, roadmap item, or test proposal) into the scan inbox. Never writes the curated store; Pax validates.",
         "inputSchema": {"type": "object", "properties": {
             "kind": {"type": "string", "enum": ["finding", "roadmap", "test"]},
             "system": {"type": "string"}, "title": {"type": "string"},
             "source": {"type": "string"}, "summary": {"type": "string"}},
             "required": ["kind", "system", "title", "source", "summary"]}},
        {"name": "watchlist_list",
         "description": "List the watchlist (per-system venues and terms).",
         "inputSchema": {"type": "object", "properties": {"system": {"type": "string"}}}},
        {"name": "capability_context",
         "description": "Read lupin's capability registry (read-only) as planning vocabulary: capability ids + names, optionally keyword-filtered.",
         "inputSchema": {"type": "object", "properties": {"query": {"type": "string"}}}},
        {"name": "revenue_candidate_draft",
         "description": "Draft a lupin opportunity_submit payload for a revenue-shaped finding. Drafts only — this server never writes lupin's files.",
         "inputSchema": {"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}},
        {"name": "feed_summary",
         "description": "Counts: findings by system/kind, systems watched, parse health.",
         "inputSchema": {"type": "object", "properties": {}}}
    ]})
}

fn text_result(value: &Value, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": value.to_string()}], "isError": is_error})
}

fn err_result(msg: &str) -> Value {
    text_result(&json!({"error": msg}), true)
}

fn call_tool(cfg: &Config, name: &str, args: &Value) -> Value {
    let data: Store = load(&cfg.root);
    match name {
        "finding_list" => {
            let system = args.get("system").and_then(|v| v.as_str());
            let items: Vec<Value> = data
                .findings
                .iter()
                .filter(|f| system.map(|s| f.system == s.to_lowercase()).unwrap_or(true))
                .map(|f| {
                    json!({"id": f.id, "title": f.title, "system": f.system,
                           "kind": f.kind, "source": f.source, "status": f.status})
                })
                .collect();
            text_result(
                &json!({"findings": items, "parse_errors": data.parse_errors}),
                false,
            )
        }
        "finding_get" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            match data.findings.iter().find(|f| f.id == id) {
                Some(f) => text_result(
                    &json!({"id": f.id, "title": f.title, "system": f.system,
                            "kind": f.kind, "source": f.source, "status": f.status,
                            "relevance": f.relevance}),
                    false,
                ),
                None => err_result("finding not found"),
            }
        }
        "research_submit" => {
            let kind = args.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if !["finding", "roadmap", "test"].contains(&kind) {
                return err_result("kind must be finding, roadmap, or test");
            }
            let system = args.get("system").and_then(|v| v.as_str()).unwrap_or("");
            let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("");
            let summary_text = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            if system.is_empty() || title.is_empty() || source.is_empty() || summary_text.is_empty()
            {
                return err_result("system, title, source, and summary are all required");
            }
            let scans = cfg.root.join("scans");
            if fs::create_dir_all(&scans).is_err() {
                return err_result("cannot create scans directory");
            }
            let inbox = scans.join("SCAN-INBOX.md");
            let entry = format!(
                "\n### CANDIDATE ({kind}) — {title}\n- **System:** {system}\n- **Source:** {source}\n- **Summary:** {summary_text}\n- **Status:** candidate (unvalidated — filed by MCP caller)\n"
            );
            match OpenOptions::new().create(true).append(true).open(&inbox) {
                Ok(mut f) => match f.write_all(entry.as_bytes()) {
                    Ok(()) => text_result(
                        &json!({"filed": inbox.display().to_string(),
                                "note": "candidate only; Pax validates before any curated entry"}),
                        false,
                    ),
                    Err(e) => err_result(&format!("inbox write failed: {e}")),
                },
                Err(e) => err_result(&format!("inbox open failed: {e}")),
            }
        }
        "watchlist_list" => {
            let system = args.get("system").and_then(|v| v.as_str());
            let items: Vec<Value> = data
                .watchlist
                .iter()
                .filter(|w| system.map(|s| w.system == s.to_lowercase()).unwrap_or(true))
                .map(|w| json!({"system": w.system, "venue": w.venue, "terms": w.terms}))
                .collect();
            text_result(&json!({"watchlist": items}), false)
        }
        "capability_context" => {
            // Read-only seam into lupin's registry.
            let caps_text =
                fs::read_to_string(cfg.lupin_root.join("CAPABILITIES.md")).unwrap_or_default();
            let query = args
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_lowercase();
            let mut items = Vec::new();
            for line in caps_text.lines() {
                let trimmed = line.trim();
                if !trimmed.starts_with("| CAP-") {
                    continue;
                }
                let cells: Vec<&str> = trimmed
                    .trim_matches('|')
                    .split('|')
                    .map(|c| c.trim())
                    .collect();
                if cells.len() < 2 {
                    continue;
                }
                if !query.is_empty()
                    && !format!("{} {}", cells[0], cells[1])
                        .to_lowercase()
                        .contains(&query)
                {
                    continue;
                }
                items.push(json!({"id": cells[0], "name": cells[1]}));
            }
            text_result(&json!({"capabilities": items}), false)
        }
        "revenue_candidate_draft" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            match data.findings.iter().find(|f| f.id == id) {
                Some(f) => text_result(&revenue_draft(f), false),
                None => err_result("finding not found"),
            }
        }
        "feed_summary" => text_result(&summary(&data), false),
        _ => err_result("unknown tool"),
    }
}

fn handle(cfg: &Config, req: &Value) -> Option<Value> {
    let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}, "resources": {}},
            "serverInfo": {"name": "phaedrus", "version": "0.1.0"}
        }),
        "ping" => json!({}),
        "tools/list" => tool_schemas(),
        "tools/call" => {
            let params = req.get("params").cloned().unwrap_or(json!({}));
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            call_tool(cfg, name, &args)
        }
        "resources/list" => json!({"resources": [
            {"uri": "research://findings", "name": "Curated findings", "mimeType": "text/markdown"},
            {"uri": "research://watchlist", "name": "Watchlist", "mimeType": "text/markdown"},
            {"uri": "research://design", "name": "Feed design", "mimeType": "text/markdown"}
        ]}),
        "resources/read" => {
            let uri = req
                .get("params")
                .and_then(|p| p.get("uri"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let file = match uri {
                "research://findings" => "FINDINGS.md",
                "research://watchlist" => "WATCHLIST.md",
                "research://design" => "DESIGN.md",
                _ => {
                    return Some(json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32602, "message": "unknown resource uri"}}))
                }
            };
            let text = fs::read_to_string(cfg.root.join(file)).unwrap_or_default();
            json!({"contents": [{"uri": uri, "mimeType": "text/markdown", "text": text}]})
        }
        _ => {
            if id.is_null() {
                return None;
            }
            return Some(json!({"jsonrpc": "2.0", "id": id,
                "error": {"code": -32601, "message": "method not found"}}));
        }
    };
    if id.is_null() {
        return None;
    }
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

fn main() {
    let mut root: Option<PathBuf> = env::var_os("RESEARCH_ROOT").map(PathBuf::from);
    let mut lupin_root: Option<PathBuf> = env::var_os("LUPIN_ROOT").map(PathBuf::from);
    let mut argv = env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--root" => root = argv.next().map(PathBuf::from),
            "--lupin-root" => lupin_root = argv.next().map(PathBuf::from),
            other => eprintln!("phaedrus: ignoring unknown arg {other}"),
        }
    }
    let home = env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let root = root.unwrap_or_else(|| home.join("workspace").join("research-feed-20261019"));
    let lupin_root =
        lupin_root.unwrap_or_else(|| home.join("workspace").join("revenue-alignment-20261019"));
    let cfg = Config { root, lupin_root };

    let stdin = std::io::stdin();
    let mut reader = BufReader::new(stdin.lock());
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let response = match serde_json::from_str::<Value>(trimmed) {
                    Ok(req) => handle(&cfg, &req),
                    Err(_) => Some(json!({"jsonrpc": "2.0", "id": Value::Null,
                        "error": {"code": -32700, "message": "parse error"}})),
                };
                if let Some(resp) = response {
                    let _ = writeln!(out, "{resp}");
                    let _ = out.flush();
                }
            }
            Err(_) => break,
        }
    }
}
