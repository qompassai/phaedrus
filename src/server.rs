//! The rmcp server: tool/resource surface and handler dispatch.
//!
//! Domain logic does not move in the WS2 migration — the seven tool
//! handlers and the three resource reads below are the hand-rolled
//! server's, verbatim in behavior (same argument handling — missing
//! or non-string fields degrade to empty, unknown arguments are
//! ignored — same payload construction, same failure taxonomy):
//! domain failures return as `isError` content carrying
//! `{"error": msg}`; an unknown resource URI is the one protocol
//! error (`-32602`). The lupin seam is unchanged: capability_context
//! reads lupin's registry read-only, and revenue_candidate_draft
//! only drafts a payload — this server never writes lupin's files.
//! Only the framing changes sides of the seam: rmcp owns the
//! envelope, the era guard (see `guard.rs`) owns the stdio front
//! door, and this type owns the surface.
//!
//! Modern-only posture lives in two places: the era guard refuses
//! `initialize` before the SDK sees it, and
//! [`ServerHandler::supported_protocol_versions`] pins this handler
//! to 2026-07-28, so `server/discover` advertises exactly one
//! version and the SDK validates every request's `_meta` against it.
//!
//! 0.3.0 adds the P-1 research-query path (the fathom YouTube-RE
//! spec's proposal, built as specified): `research_submit` accepts
//! `kind: "query"` with an optional caller `ref` and returns the
//! filed query's candidate id (`QRY-<SYSTEM>-<NNN>`), and the new
//! `query_status` tool correlates that id (or the ref) with the
//! curated answer. Curation itself is unchanged and still human:
//! the curator's `FINDINGS.md` entry carries `**Answers:**` and/or
//! the carried-through `**Ref:**`, and that record — never the
//! server — is what makes a query "answered". All pre-existing
//! tools behave exactly as before.

use std::borrow::Cow;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeResult, ListResourcesResult, ListToolsResult, PaginatedRequestParams,
    ProtocolVersion, ReadResourceRequestParams, ReadResourceResponse, ReadResourceResult, Resource,
    ResourceContents, ServerCapabilities, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Value, json};

use crate::store::{
    Store, collapse_ws, load, next_query_id, normalized_title, revenue_draft, summary,
    validate_query_id, validate_ref,
};

/// Server configuration (the baseline's CLI surface, unchanged):
/// the feed root plus lupin's root for the read-only seam.
pub struct Config {
    pub root: PathBuf,
    pub lupin_root: PathBuf,
}

/// The tool catalog: names, descriptions, and input schemas are the
/// baseline's, byte-for-byte — rmcp accepts raw schemas, so no
/// schemars translation stands between the baseline and the wire.
fn tool_definitions() -> Vec<Tool> {
    let specs: Vec<(&str, &str, Value)> = vec![
        (
            "finding_list",
            "List curated findings, optionally filtered by system.",
            json!({"type": "object", "properties": {"system": {"type": "string"}}}),
        ),
        (
            "finding_get",
            "Get one curated finding by id.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        ),
        (
            "research_submit",
            "File a CANDIDATE (finding, roadmap item, test proposal, or query) into the scan inbox. Never writes the curated store; Pax validates. A query returns its candidate id; correlate its curated answer with query_status.",
            json!({"type": "object", "properties": {
                "kind": {"type": "string", "enum": ["finding", "roadmap", "test", "query"]},
                "system": {"type": "string"}, "title": {"type": "string"},
                "source": {"type": "string"}, "summary": {"type": "string"},
                "ref": {"type": "string"}},
                "required": ["kind", "system", "title", "source", "summary"]}),
        ),
        (
            "query_status",
            "Correlate a filed research query with its curated answer: pass the query id returned by research_submit (or the caller ref filed with it) and get its state — pending, or answered with the curated finding id(s) that answer it.",
            json!({"type": "object", "properties": {
                "id": {"type": "string"}, "ref": {"type": "string"}}}),
        ),
        (
            "watchlist_list",
            "List the watchlist (per-system venues and terms).",
            json!({"type": "object", "properties": {"system": {"type": "string"}}}),
        ),
        (
            "capability_context",
            "Read lupin's capability registry (read-only) as planning vocabulary: capability ids + names, optionally keyword-filtered.",
            json!({"type": "object", "properties": {"query": {"type": "string"}}}),
        ),
        (
            "revenue_candidate_draft",
            "Draft a lupin opportunity_submit payload for a revenue-shaped finding. Drafts only — this server never writes lupin's files.",
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        ),
        (
            "feed_summary",
            "Counts: findings by system/kind, systems watched, parse health.",
            json!({"type": "object", "properties": {}}),
        ),
    ];
    specs
        .into_iter()
        .map(|(name, description, schema)| {
            let Value::Object(map) = schema else {
                unreachable!("catalog schemas are objects");
            };
            Tool::new(name, description, map)
        })
        .collect()
}

/// The resource catalog (the baseline's, in order).
fn resource_definitions() -> Vec<Resource> {
    [
        ("research://findings", "Curated findings"),
        ("research://watchlist", "Watchlist"),
        ("research://design", "Feed design"),
    ]
    .into_iter()
    .map(|(uri, name)| Resource::new(uri, name).with_mime_type("text/markdown"))
    .collect()
}

fn text_result(value: &Value, is_error: bool) -> CallToolResult {
    let content = vec![ContentBlock::text(value.to_string())];
    if is_error {
        CallToolResult::error(content)
    } else {
        CallToolResult::success(content)
    }
}

fn err_result(msg: &str) -> CallToolResult {
    text_result(&json!({"error": msg}), true)
}

/// Serializes query filings: allocating an id and checking a ref
/// are read-modify-write over the inbox, and rmcp may dispatch
/// calls concurrently — without this, two callers could mint the
/// same id or claim the same ref in one breath.
static SUBMIT_LOCK: Mutex<()> = Mutex::new(());

/// The P-1 filing path (`research_submit`, kind `"query"`). Files
/// the question into the same scan inbox as any candidate — with a
/// server-minted id and the caller's ref recorded — and returns
/// the id. Writes nothing anywhere else; the curated store is not
/// touched (the baseline's rule, kept).
fn submit_query(
    cfg: &Config,
    args: &Value,
    system: &str,
    title: &str,
    source: &str,
    summary_text: &str,
) -> CallToolResult {
    // A supplied ref must be usable: it is the caller's correlation
    // key, matched byte-for-byte at curation time, so a malformed
    // one is refused rather than silently dropped.
    let caller_ref: Option<String> = match args.get("ref") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => match validate_ref(s) {
            Ok(()) => Some(s.clone()),
            Err(e) => return err_result(&format!("malformed ref: {e}")),
        },
        Some(_) => return err_result("malformed ref: ref must be a string"),
    };
    // Query entries are parsed back from the line-oriented inbox,
    // so their fields are whitespace-collapsed at filing time (the
    // candidate path keeps the baseline's verbatim text).
    let system = collapse_ws(system);
    let title = collapse_ws(title);
    let source = collapse_ws(source);
    let summary_text = collapse_ws(summary_text);
    if system.is_empty() || title.is_empty() || source.is_empty() || summary_text.is_empty() {
        return err_result("system, title, source, and summary are all required");
    }
    let scans = cfg.root.join("scans");
    let inbox = scans.join("SCAN-INBOX.md");
    let _guard = SUBMIT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // Fresh load under the lock: the id space and the ref registry
    // are whatever the inbox says right now, not what the
    // dispatch-time load saw.
    let data = load(&cfg.root);
    if let Some(r) = &caller_ref
        && let Some(existing) = data
            .queries
            .iter()
            .find(|q| q.caller_ref.as_deref() == Some(r.as_str()))
    {
        if existing.system == system.to_lowercase()
            && normalized_title(&existing.title) == normalized_title(&title)
        {
            return text_result(
                &json!({"filed": inbox.display().to_string(),
                        "id": existing.id, "ref": r,
                        "note": "query already filed under this ref; returning its id"}),
                false,
            );
        }
        return err_result("ref already in use by another query");
    }
    let id = next_query_id(&data.queries, &system);
    if fs::create_dir_all(&scans).is_err() {
        return err_result("cannot create scans directory");
    }
    let ref_line = caller_ref
        .as_ref()
        .map(|r| format!("- **Ref:** {r}\n"))
        .unwrap_or_default();
    let entry = format!(
        "\n### CANDIDATE (query) — {title}\n- **Id:** {id}\n- **System:** {system}\n{ref_line}- **Source:** {source}\n- **Summary:** {summary_text}\n- **Status:** query (filed by MCP caller; awaiting curation)\n"
    );
    match OpenOptions::new().create(true).append(true).open(&inbox) {
        Ok(mut f) => match f.write_all(entry.as_bytes()) {
            Ok(()) => text_result(
                &json!({"filed": inbox.display().to_string(),
                        "id": id, "ref": caller_ref,
                        "note": "query only; Pax validates before any curated entry — correlate the answer with query_status"}),
                false,
            ),
            Err(e) => err_result(&format!("inbox write failed: {e}")),
        },
        Err(e) => err_result(&format!("inbox open failed: {e}")),
    }
}

/// The P-1 correlation read. A query is `answered` exactly when the
/// human curation record says so: a curated finding whose
/// `**Answers:**` names the query id, or whose carried `**Ref:**`
/// equals the query's ref. Nothing here curates, infers, or
/// text-matches titles — linkage is by id and ref only.
fn query_status(cfg: &Config, args: &Value) -> CallToolResult {
    let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let caller_ref = args.get("ref").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() && caller_ref.is_empty() {
        return err_result("id or ref is required");
    }
    if !id.is_empty()
        && let Err(e) = validate_query_id(id)
    {
        return err_result(&e);
    }
    if !caller_ref.is_empty()
        && let Err(e) = validate_ref(caller_ref)
    {
        return err_result(&format!("malformed ref: {e}"));
    }
    let data = load(&cfg.root);
    let by_id = if id.is_empty() {
        None
    } else {
        data.queries.iter().find(|q| q.id == id)
    };
    let by_ref = if caller_ref.is_empty() {
        None
    } else {
        data.queries
            .iter()
            .find(|q| q.caller_ref.as_deref() == Some(caller_ref))
    };
    if let (Some(a), Some(b)) = (by_id, by_ref)
        && a.id != b.id
    {
        return err_result("id and ref refer to different queries");
    }
    if let Some(q) = by_id.or(by_ref) {
        let answers: Vec<&str> = data
            .findings
            .iter()
            .filter(|f| {
                f.answers.iter().any(|a| a == &q.id)
                    || (q.caller_ref.is_some() && f.answers_ref == q.caller_ref)
            })
            .map(|f| f.id.as_str())
            .collect();
        return text_result(
            &json!({"id": q.id, "ref": q.caller_ref, "system": q.system,
                    "title": q.title,
                    "state": if answers.is_empty() { "pending" } else { "answered" },
                    "answers": answers}),
            false,
        );
    }
    // No query record under that key. The ref is the spec's
    // correlation carrier, so a curated finding carrying it still
    // answers a caller who holds only the ref (e.g. the question
    // was filed before ids existed). Ids get no such fallback: a
    // finding naming an unfiled id is a dangling link, and the
    // honest answer for that id is "not found".
    if !caller_ref.is_empty() {
        let answering: Vec<&crate::store::Finding> = data
            .findings
            .iter()
            .filter(|f| f.answers_ref.as_deref() == Some(caller_ref))
            .collect();
        if !answering.is_empty() {
            return text_result(
                &json!({"id": Value::Null, "ref": caller_ref,
                        "system": answering[0].system, "title": Value::Null,
                        "state": "answered",
                        "answers": answering.iter().map(|f| f.id.as_str()).collect::<Vec<_>>()}),
                false,
            );
        }
    }
    err_result("query not found")
}

/// The baseline's dispatch, unchanged: every call reloads the
/// store from disk, and the one write files a candidate into the
/// feed's own scans/ inbox — never the curated store.
fn call_tool(cfg: &Config, name: &str, args: &Value) -> CallToolResult {
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
            if !["finding", "roadmap", "test", "query"].contains(&kind) {
                return err_result("kind must be finding, roadmap, test, or query");
            }
            let system = args.get("system").and_then(|v| v.as_str()).unwrap_or("");
            let title = args.get("title").and_then(|v| v.as_str()).unwrap_or("");
            let source = args.get("source").and_then(|v| v.as_str()).unwrap_or("");
            let summary_text = args.get("summary").and_then(|v| v.as_str()).unwrap_or("");
            if system.is_empty() || title.is_empty() || source.is_empty() || summary_text.is_empty()
            {
                return err_result("system, title, source, and summary are all required");
            }
            if kind == "query" {
                return submit_query(cfg, args, system, title, source, summary_text);
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
        "query_status" => query_status(cfg, args),
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

/// The baseline's resource read, unchanged: an unknown URI is
/// the surface's one protocol error; a missing file reads as empty.
fn read_resource(cfg: &Config, uri: &str) -> Result<ReadResourceResult, ErrorData> {
    let file = match uri {
        "research://findings" => "FINDINGS.md",
        "research://watchlist" => "WATCHLIST.md",
        "research://design" => "DESIGN.md",
        _ => {
            return Err(ErrorData::invalid_params("unknown resource uri", None));
        }
    };
    let text = fs::read_to_string(cfg.root.join(file)).unwrap_or_default();
    Ok(ReadResourceResult::new(vec![
        ResourceContents::TextResourceContents {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text,
            meta: None,
        },
    ]))
}

/// The server handler. The baseline served sequentially and every
/// call reloads state from disk, so dispatch is stateless; it runs
/// on a blocking thread so file IO cannot stall the runtime.
pub struct PhaedrusServer {
    cfg: Arc<Config>,
}

impl PhaedrusServer {
    pub fn new(cfg: Config) -> Self {
        PhaedrusServer { cfg: Arc::new(cfg) }
    }
}

impl ServerHandler for PhaedrusServer {
    fn get_info(&self) -> InitializeResult {
        // Both model types are non-exhaustive: start from the
        // default and set exactly the fields this server reports.
        // The baseline reported no instructions; none are set.
        let mut server_info = Implementation::default();
        server_info.name = "phaedrus".to_string();
        server_info.version = env!("CARGO_PKG_VERSION").to_string();
        let mut info = InitializeResult::default();
        info.protocol_version = ProtocolVersion::V_2026_07_28;
        info.capabilities = ServerCapabilities::builder()
            .enable_tools()
            .enable_resources()
            .build();
        info.server_info = server_info;
        info
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(&[ProtocolVersion::V_2026_07_28])
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult {
            tools: tool_definitions(),
            ..Default::default()
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.to_string();
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let cfg = Arc::clone(&self.cfg);
        let outcome = tokio::task::spawn_blocking(move || call_tool(&cfg, &name, &arguments)).await;
        match outcome {
            Ok(result) => Ok(result.into()),
            Err(join) => Err(ErrorData::internal_error(
                format!("tool task failed: {join}"),
                None,
            )),
        }
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult {
            resources: resource_definitions(),
            ..Default::default()
        })
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let cfg = Arc::clone(&self.cfg);
        let outcome = tokio::task::spawn_blocking(move || read_resource(&cfg, &request.uri)).await;
        match outcome {
            Ok(result) => result.map(ReadResourceResponse::from),
            Err(join) => Err(ErrorData::internal_error(
                format!("resource task failed: {join}"),
                None,
            )),
        }
    }
}
