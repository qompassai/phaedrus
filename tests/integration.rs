//! Integration tests: drive the real binary over stdio against fixture
//! stores. Half validation, half adversarial (incl. the lupin seam rule).

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
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn call(id: i64, tool: &str, args: serde_json::Value) -> String {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
        "params": {"name": tool, "arguments": args}})
    .to_string()
}

fn result_text(resp: &serde_json::Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

// ---------- validation ----------

#[test]
fn initialize_lists_seven_tools() {
    let (root, lupin) = fixture_root("init");
    let resps = rpc(
        &root,
        &lupin,
        &[
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        ],
    );
    assert_eq!(resps[0]["result"]["serverInfo"]["name"], "phaedrus");
    assert_eq!(resps[1]["result"]["tools"].as_array().unwrap().len(), 7);
}

#[test]
fn capability_context_reads_lupin_registry() {
    let (root, lupin) = fixture_root("caps");
    let req = call(1, "capability_context", serde_json::json!({}));
    let resps = rpc(&root, &lupin, &[&req]);
    let text = result_text(&resps[0]);
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
    let resps = rpc(&root, &lupin, &[&req]);
    assert!(result_text(&resps[0]).contains("filed"));
    let inbox = std::fs::read_to_string(root.join("scans/SCAN-INBOX.md")).unwrap();
    assert!(inbox.contains("CANDIDATE (test)"));
}

// ---------- adversarial ----------

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
    let _ = rpc(&root, &lupin, &[&req]);
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
    let resps = rpc(&root, &lupin, &[&req]);
    let text = result_text(&resps[0]);
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
    let resps = rpc(&root, &lupin, &[&bad_kind, &bad_id]);
    assert_eq!(resps[0]["result"]["isError"], true);
    assert_eq!(resps[1]["result"]["isError"], true);
}
