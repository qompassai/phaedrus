//! Integration tests: drive the real binary over stdio against fixture
//! stores. Half validation, half adversarial (incl. the lupin seam rule).
//!
//! WS2 (D-113): the wire is the modern era only — sessions open with
//! `server/discover`, every request carries `_meta`, and legacy
//! `initialize` contact is refused with the typed `-32022`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const FINDINGS: &str = "\
### FND-LUMEN-001 — Parametric body model paper
- **System:** lumen
- **Kind:** paper
- **Source:** arXiv:2511.03589
- **Status:** validated
- **Relevance:** Semantic parameter layer for the factory.
";

const WATCH: &str = "\
## fathom
- **arXiv:** cs.CR — decompilation

## lumen
- **arXiv:** cs.GR — parametric humans
";

const LUPIN_CAPS: &str = "\
## SFW lane — Qompass

| ID | Capability | Revenue form | Proof / state |
|----|-----------|--------------|----------------|
| CAP-LUMEN | lumen — character factory | Product + services | Published |
";

const META: &str = r#""_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientInfo": {"name": "phaedrus-test", "version": "0"}, "io.modelcontextprotocol/clientCapabilities": {}}"#;

fn fixture_root(tag: &str) -> (PathBuf, PathBuf) {
    let base =
        std::env::temp_dir().join(format!("research-feed-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("feed");
    let lupin = base.join("lupin");
    std::fs::create_dir_all(root.join("scans")).unwrap();
    std::fs::create_dir_all(&lupin).unwrap();
    std::fs::write(root.join("FINDINGS.md"), FINDINGS).unwrap();
    std::fs::write(root.join("WATCHLIST.md"), WATCH).unwrap();
    std::fs::write(root.join("DESIGN.md"), "# design\n").unwrap();
    std::fs::write(lupin.join("CAPABILITIES.md"), LUPIN_CAPS).unwrap();
    (root, lupin)
}

fn rpc(root: &Path, lupin: &Path, requests: &[&str]) -> Vec<serde_json::Value> {
    let bin = env!("CARGO_BIN_EXE_phaedrus");
    let mut child = Command::new(bin)
        .args([
            "--root",
            root.to_str().unwrap(),
            "--lupin-root",
            lupin.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let stdin = child.stdin.as_mut().unwrap();
        for req in requests {
            writeln!(stdin, "{req}").unwrap();
        }
    }
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "server exit: {:?}", out.status);
    // rmcp dispatches pipelined requests concurrently, so responses
    // may arrive out of request order (legal JSON-RPC; the baseline
    // answered strictly sequentially). Tests correlate by id: sort
    // the parsed responses before indexing them positionally.
    let mut resps: Vec<serde_json::Value> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    resps.sort_by_key(|r| r["id"].as_i64().unwrap_or(i64::MAX));
    resps
}

fn discover() -> String {
    format!(r#"{{"jsonrpc":"2.0","id":0,"method":"server/discover","params":{{{META}}}}}"#)
}

fn call(id: i64, tool: &str, args: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": args,
                   "_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28",
                             "io.modelcontextprotocol/clientInfo": {"name": "phaedrus-test", "version": "0"},
                             "io.modelcontextprotocol/clientCapabilities": {}}}})
    .to_string()
}

fn request(id: i64, method: &str, params: serde_json::Value) -> String {
    let mut params = params;
    params["_meta"] = serde_json::json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "phaedrus-test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}});
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

fn result_text(resp: &serde_json::Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

// ---------- validation ----------

#[test]
fn discover_lists_eight_tools() {
    // Seven baseline tools + query_status (P-1, 0.3.0).
    let (root, lupin) = fixture_root("init");
    let list = request(2, "tools/list", serde_json::json!({}));
    let resps = rpc(&root, &lupin, &[&discover(), &list]);
    assert_eq!(
        resps[0]["result"]["supportedVersions"],
        serde_json::json!(["2026-07-28"])
    );
    assert_eq!(
        resps[0]["result"]["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "phaedrus"
    );
    let tools = resps[1]["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 8);
    assert!(tools.iter().any(|t| t["name"] == "query_status"));
}

#[test]
fn resources_round_trip() {
    let (root, lupin) = fixture_root("resources");
    let list = request(2, "resources/list", serde_json::json!({}));
    let read = request(
        3,
        "resources/read",
        serde_json::json!({"uri": "research://findings"}),
    );
    let resps = rpc(&root, &lupin, &[&discover(), &list, &read]);
    let resources = resps[1]["result"]["resources"].as_array().unwrap();
    assert_eq!(resources.len(), 3);
    assert_eq!(resources[0]["uri"], "research://findings");
    let contents = resps[2]["result"]["contents"].as_array().unwrap();
    assert!(
        contents[0]["text"]
            .as_str()
            .unwrap()
            .contains("FND-LUMEN-001")
    );
}

#[test]
fn capability_context_reads_lupin_registry() {
    let (root, lupin) = fixture_root("caps");
    let req = call(1, "capability_context", serde_json::json!({}));
    let resps = rpc(&root, &lupin, &[&discover(), &req]);
    let text = result_text(&resps[1]);
    assert!(text.contains("CAP-LUMEN"));
}

#[test]
fn submit_files_candidate_inbox() {
    let (root, lupin) = fixture_root("submit");
    let req = call(
        1,
        "research_submit",
        serde_json::json!({"kind": "test", "system": "lumen", "title": "SSIM threshold study",
            "source": "arXiv:0000.00000", "summary": "threshold sensitivity for render gates"}),
    );
    let resps = rpc(&root, &lupin, &[&discover(), &req]);
    assert!(result_text(&resps[1]).contains("filed"));
    let inbox = std::fs::read_to_string(root.join("scans/SCAN-INBOX.md")).unwrap();
    assert!(inbox.contains("CANDIDATE (test)"));
}

// ---------- adversarial ----------

#[test]
fn legacy_contact_is_refused_and_the_server_survives() {
    let (root, lupin) = fixture_root("refusal");
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#;
    let no_meta = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
    let summary = call(3, "feed_summary", serde_json::json!({}));
    let resps = rpc(&root, &lupin, &[init, no_meta, &discover(), &summary]);
    // rpc() sorts by id: [discover(0), init(1), no-meta(2), summary(3)].
    assert_eq!(resps[1]["error"]["code"], -32022);
    assert_eq!(resps[1]["error"]["data"]["requested"], "2024-11-05");
    assert_eq!(
        resps[1]["error"]["data"]["supported"],
        serde_json::json!(["2026-07-28"])
    );
    assert!(resps[2]["error"].is_object(), "no-meta response: {resps:?}");
    assert!(result_text(&resps[3]).contains("findings_by_system"));
}

#[test]
fn submit_never_modifies_curated_store() {
    let (root, lupin) = fixture_root("immutable");
    let before = std::fs::read(root.join("FINDINGS.md")).unwrap();
    let req = call(
        1,
        "research_submit",
        serde_json::json!({"kind": "finding", "system": "lumen", "title": "Sneaky",
            "source": "arXiv:1111.11111", "summary": "tries to become curated"}),
    );
    let _ = rpc(&root, &lupin, &[&discover(), &req]);
    assert_eq!(before, std::fs::read(root.join("FINDINGS.md")).unwrap());
}

#[test]
fn revenue_draft_writes_nothing_anywhere() {
    let (root, lupin) = fixture_root("seam");
    let caps_before = std::fs::read(lupin.join("CAPABILITIES.md")).unwrap();
    let findings_before = std::fs::read(root.join("FINDINGS.md")).unwrap();
    let req = call(
        1,
        "revenue_candidate_draft",
        serde_json::json!({"id": "FND-LUMEN-001"}),
    );
    let resps = rpc(&root, &lupin, &[&discover(), &req]);
    let text = result_text(&resps[1]);
    assert!(text.contains("lupin.opportunity_submit")); // the draft points at lupin's tool
    assert_eq!(
        caps_before,
        std::fs::read(lupin.join("CAPABILITIES.md")).unwrap()
    );
    assert_eq!(
        findings_before,
        std::fs::read(root.join("FINDINGS.md")).unwrap()
    );
    assert!(!lupin.join("scans").exists()); // no inbox appeared on lupin's side
}

#[test]
fn bad_kind_and_missing_finding_error_cleanly() {
    let (root, lupin) = fixture_root("errors");
    let bad_kind = call(
        1,
        "research_submit",
        serde_json::json!({"kind": "gossip", "system": "lumen", "title": "x",
            "source": "y", "summary": "z"}),
    );
    let bad_id = call(2, "finding_get", serde_json::json!({"id": "FND-NOPE-999"}));
    let resps = rpc(&root, &lupin, &[&discover(), &bad_kind, &bad_id]);
    assert_eq!(resps[1]["result"]["isError"], true);
    assert_eq!(resps[2]["result"]["isError"], true);
}

#[test]
fn adversarial_frames_keep_framing_contract() {
    let (root, lupin) = fixture_root("adversarial");
    let oversize = "x".repeat(5 * 1024 * 1024);
    let summary = call(9, "feed_summary", serde_json::json!({}));
    let resps = rpc(
        &root,
        &lupin,
        &[
            &discover(),
            "this is not json",
            "[1,2,3]",
            &oversize,
            &summary,
        ],
    );
    // rpc() sorts by id with null-id framing errors last (stable, so
    // they keep arrival order among themselves):
    // [discover(0), summary(9), parse-error, -32600, oversize-error].
    assert_eq!(resps[2]["error"]["code"], -32700);
    assert_eq!(resps[2]["error"]["message"], "parse error");
    assert_eq!(resps[3]["error"]["code"], -32600);
    assert_eq!(resps[4]["error"]["code"], -32700);
    assert_eq!(
        resps[4]["error"]["message"],
        "request line exceeds size bound"
    );
    assert!(result_text(&resps[1]).contains("findings_by_system"));
}

#[test]
fn notification_shaped_call_is_not_executed() {
    let (root, lupin) = fixture_root("notify");
    let notif = r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"research_submit","arguments":{"kind":"finding","system":"lumen","title":"Ghost","source":"arXiv:0000.00001","summary":"x"}}}"#;
    let summary = call(2, "feed_summary", serde_json::json!({}));
    let resps = rpc(&root, &lupin, &[&discover(), notif, &summary]);
    assert_eq!(resps.len(), 2);
    assert!(!root.join("scans/SCAN-INBOX.md").exists());
}

// ---------- P-1: research queries (validation) ----------

fn submit_query(id: i64, system: &str, title: &str, caller_ref: Option<&str>) -> String {
    let mut args = serde_json::json!({"kind": "query", "system": system, "title": title,
        "source": "youtube: example", "summary": "what the video does not establish"});
    if let Some(r) = caller_ref {
        args["ref"] = serde_json::json!(r);
    }
    call(id, "research_submit", args)
}

fn status_call(id: i64, args: serde_json::Value) -> String {
    call(id, "query_status", args)
}

fn payload(resp: &serde_json::Value) -> serde_json::Value {
    serde_json::from_str(&result_text(resp)).unwrap()
}

/// The human curation act, simulated the way it really happens:
/// a person appends a validated entry to FINDINGS.md by hand.
fn curate(root: &Path, entry: &str) {
    let mut f = std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("FINDINGS.md"))
        .unwrap();
    writeln!(f, "{entry}").unwrap();
}

const CURATED_ANSWER: &str = "
### FND-FATHOM-007 — Pak v11 header layout
- **System:** fathom
- **Kind:** repo
- **Source:** gh: example/pak
- **Status:** validated
- **Relevance:** Settles the filed layout question.
- **Answers:** QRY-FATHOM-001
- **Ref:** fathom:video/pak:u1
";

#[test]
fn query_submit_returns_candidate_id_and_files_inbox() {
    let (root, lupin) = fixture_root("q-submit");
    let findings_before = std::fs::read(root.join("FINDINGS.md")).unwrap();
    let req = submit_query(
        1,
        "fathom",
        "Which pak version ships?",
        Some("fathom:video/pak:u1"),
    );
    let resps = rpc(&root, &lupin, &[&discover(), &req]);
    let body = payload(&resps[1]);
    assert_eq!(body["id"], "QRY-FATHOM-001");
    assert_eq!(body["ref"], "fathom:video/pak:u1");
    let inbox = std::fs::read_to_string(root.join("scans/SCAN-INBOX.md")).unwrap();
    assert!(inbox.contains("CANDIDATE (query)"));
    assert!(inbox.contains("**Id:** QRY-FATHOM-001"));
    assert!(inbox.contains("**Ref:** fathom:video/pak:u1"));
    // Filing a query never touches the curated store.
    assert_eq!(
        findings_before,
        std::fs::read(root.join("FINDINGS.md")).unwrap()
    );
}

#[test]
fn query_status_pending_then_answered_by_curation() {
    let (root, lupin) = fixture_root("q-lifecycle");
    let submit = submit_query(
        1,
        "fathom",
        "Which pak version ships?",
        Some("fathom:video/pak:u1"),
    );
    let _ = rpc(&root, &lupin, &[&discover(), &submit]);
    let by_id = status_call(1, serde_json::json!({"id": "QRY-FATHOM-001"}));
    let resps = rpc(&root, &lupin, &[&discover(), &by_id]);
    let body = payload(&resps[1]);
    assert_eq!(body["state"], "pending");
    assert_eq!(body["answers"], serde_json::json!([]));

    curate(&root, CURATED_ANSWER);
    let by_id = status_call(1, serde_json::json!({"id": "QRY-FATHOM-001"}));
    let by_ref = status_call(2, serde_json::json!({"ref": "fathom:video/pak:u1"}));
    let both = status_call(
        3,
        serde_json::json!({"id": "QRY-FATHOM-001", "ref": "fathom:video/pak:u1"}),
    );
    let resps = rpc(&root, &lupin, &[&discover(), &by_id, &by_ref, &both]);
    for resp in &resps[1..] {
        let body = payload(resp);
        assert_eq!(body["state"], "answered");
        assert_eq!(body["answers"], serde_json::json!(["FND-FATHOM-007"]));
    }
}

#[test]
fn query_answered_by_multiple_findings() {
    let (root, lupin) = fixture_root("q-multi");
    let submit = submit_query(1, "fathom", "Which pak version ships?", None);
    let _ = rpc(&root, &lupin, &[&discover(), &submit]);
    curate(&root, CURATED_ANSWER);
    curate(
        &root,
        "
### FND-FATHOM-008 — Pak footer cross-check
- **System:** fathom
- **Kind:** paper
- **Source:** arXiv:0000.00000
- **Status:** validated
- **Relevance:** Second answer to the same question, linked by id only.
- **Answers:** QRY-FATHOM-001
",
    );
    let by_id = status_call(1, serde_json::json!({"id": "QRY-FATHOM-001"}));
    let resps = rpc(&root, &lupin, &[&discover(), &by_id]);
    let body = payload(&resps[1]);
    assert_eq!(body["state"], "answered");
    assert_eq!(
        body["answers"],
        serde_json::json!(["FND-FATHOM-007", "FND-FATHOM-008"])
    );
}

#[test]
fn ref_only_curation_answers_a_caller_holding_only_the_ref() {
    // The question predates query ids (or was filed elsewhere);
    // the curated finding carries the ref, and that alone links.
    let (root, lupin) = fixture_root("q-refonly");
    curate(
        &root,
        "
### FND-FATHOM-009 — Answer carried by ref alone
- **System:** fathom
- **Kind:** repo
- **Source:** gh: example/pak
- **Status:** validated
- **Relevance:** Linked by ref, no query record exists.
- **Ref:** ext:question-9
",
    );
    let by_ref = status_call(1, serde_json::json!({"ref": "ext:question-9"}));
    let resps = rpc(&root, &lupin, &[&discover(), &by_ref]);
    let body = payload(&resps[1]);
    assert_eq!(body["state"], "answered");
    assert_eq!(body["id"], serde_json::Value::Null);
    assert_eq!(body["answers"], serde_json::json!(["FND-FATHOM-009"]));
}

// ---------- P-1: research queries (adversarial) ----------

#[test]
fn duplicate_ref_is_idempotent_for_the_same_question_only() {
    let (root, lupin) = fixture_root("q-dupref");
    let first = submit_query(1, "fathom", "Which pak version ships?", Some("dup:1"));
    let _ = rpc(&root, &lupin, &[&discover(), &first]);
    // Same ref, same question under title normalization → same id.
    let again = submit_query(1, "Fathom", "which   pak version ships?", Some("dup:1"));
    let resps = rpc(&root, &lupin, &[&discover(), &again]);
    assert_eq!(payload(&resps[1])["id"], "QRY-FATHOM-001");
    let inbox = std::fs::read_to_string(root.join("scans/SCAN-INBOX.md")).unwrap();
    assert_eq!(inbox.matches("CANDIDATE (query)").count(), 1);
    // Same ref, different question → refused, nothing filed.
    let hijack = submit_query(1, "fathom", "A different question entirely", Some("dup:1"));
    let resps = rpc(&root, &lupin, &[&discover(), &hijack]);
    assert_eq!(resps[1]["result"]["isError"], true);
    assert!(result_text(&resps[1]).contains("ref already in use"));
    let inbox = std::fs::read_to_string(root.join("scans/SCAN-INBOX.md")).unwrap();
    assert_eq!(inbox.matches("CANDIDATE (query)").count(), 1);
}

#[test]
fn query_status_unknown_and_malformed_keys_error_cleanly() {
    let (root, lupin) = fixture_root("q-unknown");
    // Filed in separate sessions: pipelined calls may dispatch
    // concurrently, so one session could swap the id order.
    let a = submit_query(1, "fathom", "Question A", Some("key:a"));
    let _ = rpc(&root, &lupin, &[&discover(), &a]);
    let b = submit_query(1, "fathom", "Question B", Some("key:b"));
    let _ = rpc(&root, &lupin, &[&discover(), &b]);
    let unknown_id = status_call(1, serde_json::json!({"id": "QRY-FATHOM-999"}));
    let unknown_ref = status_call(2, serde_json::json!({"ref": "never:filed"}));
    let wrong_kind_id = status_call(3, serde_json::json!({"id": "FND-FATHOM-001"}));
    let spaced_ref = status_call(4, serde_json::json!({"ref": "has space"}));
    let no_key = status_call(5, serde_json::json!({}));
    let mismatched = status_call(
        6,
        serde_json::json!({"id": "QRY-FATHOM-001", "ref": "key:b"}),
    );
    let resps = rpc(
        &root,
        &lupin,
        &[
            &discover(),
            &unknown_id,
            &unknown_ref,
            &wrong_kind_id,
            &spaced_ref,
            &no_key,
            &mismatched,
        ],
    );
    for resp in &resps[1..] {
        assert_eq!(resp["result"]["isError"], true, "response: {resp}");
    }
    assert!(result_text(&resps[1]).contains("query not found"));
    assert!(result_text(&resps[2]).contains("query not found"));
    assert!(result_text(&resps[3]).contains("malformed query id"));
    assert!(result_text(&resps[4]).contains("malformed ref"));
    assert!(result_text(&resps[5]).contains("id or ref is required"));
    assert!(result_text(&resps[6]).contains("different queries"));
}

#[test]
fn malformed_refs_are_refused_at_submit() {
    let (root, lupin) = fixture_root("q-badref");
    let spaced = submit_query(1, "fathom", "Q", Some("has space"));
    let newline = submit_query(2, "fathom", "Q", Some("line\nbreak"));
    let long = submit_query(3, "fathom", "Q", Some(&"x".repeat(129)));
    let numeric = call(
        4,
        "research_submit",
        serde_json::json!({"kind": "query", "system": "fathom", "title": "Q",
            "source": "s", "summary": "s", "ref": 42}),
    );
    let resps = rpc(
        &root,
        &lupin,
        &[&discover(), &spaced, &newline, &long, &numeric],
    );
    for resp in &resps[1..] {
        assert_eq!(resp["result"]["isError"], true, "response: {resp}");
        assert!(result_text(resp).contains("malformed ref"));
    }
    // Nothing was filed under any of them.
    let inbox = root.join("scans/SCAN-INBOX.md");
    if inbox.exists() {
        assert!(
            !std::fs::read_to_string(inbox)
                .unwrap()
                .contains("CANDIDATE (query)")
        );
    }
}

#[test]
fn dangling_answers_link_corrupts_nothing() {
    let (root, lupin) = fixture_root("q-dangling");
    // A curated finding claiming to answer a query nobody filed.
    curate(
        &root,
        "
### FND-FATHOM-010 — Answers a question nobody asked
- **System:** fathom
- **Kind:** paper
- **Source:** arXiv:0000.00000
- **Status:** validated
- **Relevance:** Its Answers link dangles.
- **Answers:** QRY-FATHOM-999
",
    );
    let submit = submit_query(1, "fathom", "A real question", None);
    let _ = rpc(&root, &lupin, &[&discover(), &submit]);
    let real = status_call(1, serde_json::json!({"id": "QRY-FATHOM-001"}));
    let dangling = status_call(2, serde_json::json!({"id": "QRY-FATHOM-999"}));
    let summary = call(3, "feed_summary", serde_json::json!({}));
    let resps = rpc(&root, &lupin, &[&discover(), &real, &dangling, &summary]);
    // The real query is not "answered" by the dangling link...
    assert_eq!(payload(&resps[1])["state"], "pending");
    // ...the dangling id itself has no query record to correlate...
    assert_eq!(resps[2]["result"]["isError"], true);
    assert!(result_text(&resps[2]).contains("query not found"));
    // ...and the rest of the surface is unaffected.
    assert!(result_text(&resps[3]).contains("findings_by_system"));
}

#[test]
fn unknown_resource_uri_is_invalid_params() {
    let (root, lupin) = fixture_root("baduri");
    let read = request(
        2,
        "resources/read",
        serde_json::json!({"uri": "research://bogus"}),
    );
    let resps = rpc(&root, &lupin, &[&discover(), &read]);
    assert_eq!(resps[1]["error"]["code"], -32602);
    assert_eq!(resps[1]["error"]["message"], "unknown resource uri");
}
