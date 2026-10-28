//! Findings-store and watchlist parsing for the research feed.
//!
//! The markdown files are the source of truth. Parsing is fail-closed:
//! a finding without a system or a source is skipped and counted.

use serde::Serialize;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    pub id: String,
    pub title: String,
    pub system: String,
    pub kind: String,
    pub source: String,
    pub status: String,
    pub relevance: String,
    /// P-1 correlation (optional, curator-written): the query ids
    /// this finding answers, from the `**Answers:**` field.
    pub answers: Vec<String>,
    /// P-1 correlation (optional, curator-written): the caller ref
    /// carried from the query this finding answers, from `**Ref:**`.
    pub answers_ref: Option<String>,
}

/// A research query filed through `research_submit` (kind
/// `"query"`), parsed back from the scan inbox. Queries live in the
/// inbox — the uncurated side — by design: filing one never touches
/// the curated store, and a human still curates every answer.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Query {
    pub id: String,
    pub title: String,
    pub system: String,
    pub caller_ref: Option<String>,
    pub source: String,
    pub summary: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct WatchEntry {
    pub system: String,
    pub venue: String,
    pub terms: String,
}

#[derive(Clone, Debug, Default)]
pub struct Store {
    pub findings: Vec<Finding>,
    pub watchlist: Vec<WatchEntry>,
    pub queries: Vec<Query>,
    pub parse_errors: usize,
    /// Malformed query entries skipped at load (fail-closed, like
    /// `parse_errors`). Recorded for diagnosis and asserted by the
    /// store tests; deliberately not added to any tool payload —
    /// the existing payloads are parity-frozen.
    #[allow(dead_code)]
    pub query_parse_errors: usize,
}

/// The title normal form used for query identity: case-folded,
/// whitespace-collapsed. Two filings are "the same question" when
/// system and normalized title agree.
pub fn normalized_title(title: &str) -> String {
    title
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Collapse all whitespace runs (including newlines) to single
/// spaces. Query entries are parsed back from a line-oriented
/// inbox format, so filed fields must not carry raw newlines.
pub fn collapse_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A caller ref is a correlation key that gets embedded in
/// markdown records and matched byte-for-byte later, so its
/// alphabet is deliberately small: ASCII alphanumerics plus
/// `-._:/` (enough for namespaced refs like
/// `fathom:video/<digest>:u3`), 1–128 bytes, nothing that can
/// break a markdown field or a line.
pub fn validate_ref(caller_ref: &str) -> Result<(), String> {
    if caller_ref.is_empty() {
        return Err("ref is empty".to_string());
    }
    if caller_ref.len() > 128 {
        return Err("ref exceeds 128 bytes".to_string());
    }
    if let Some(c) = caller_ref
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || "-._:/".contains(*c)))
    {
        return Err(format!("ref contains disallowed character {c:?}"));
    }
    Ok(())
}

/// Query ids are server-minted (`QRY-<SYSTEM>-<NNN>`); a caller
/// presenting one for correlation must at least present the shape.
pub fn validate_query_id(id: &str) -> Result<(), String> {
    let well_formed = id.len() > 4
        && id.len() <= 80
        && id.starts_with("QRY-")
        && id[4..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-');
    if well_formed {
        Ok(())
    } else {
        Err("malformed query id".to_string())
    }
}

/// Allocate the next query id for a system: `QRY-<SYSTEM>-<NNN>`,
/// one past the highest suffix already filed for that system.
pub fn next_query_id(queries: &[Query], system: &str) -> String {
    let key: String = system
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '-'
            }
        })
        .collect();
    let key = key.trim_matches('-');
    let key = if key.is_empty() { "QUERY" } else { key };
    let prefix = format!("QRY-{key}-");
    let mut max_seen = 0u32;
    for q in queries {
        if let Some(suffix) = q.id.strip_prefix(&prefix)
            && let Ok(n) = suffix.parse::<u32>()
        {
            max_seen = max_seen.max(n);
        }
    }
    format!("{prefix}{:03}", max_seen + 1)
}

/// Extract the value following `**Label:**` up to the next field marker
/// or end of line (same convention as lupin's ledger parser).
fn field(block: &str, label: &str) -> String {
    let needle = format!("**{label}:**");
    let Some(start) = block.find(&needle) else {
        return String::new();
    };
    let rest = &block[start + needle.len()..];
    let end = rest
        .find("**")
        .unwrap_or(rest.len())
        .min(rest.find('\n').unwrap_or(rest.len()));
    rest[..end].replace("**", "").trim().to_string()
}

/// Split a markdown store into `(header, block)` pairs at `### `
/// headings, skipping HTML comments (the shared splitter for the
/// findings store and the inbox's query entries).
fn split_blocks(text: &str) -> Vec<(String, String)> {
    let mut current: Option<(String, String)> = None;
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut in_comment = false;
    for line in text.lines() {
        if line.contains("<!--") {
            in_comment = true;
        }
        if line.contains("-->") {
            in_comment = false;
            continue;
        }
        if in_comment {
            continue;
        }
        if line.starts_with("### ") {
            if let Some(done) = current.take() {
                blocks.push(done);
            }
            current = Some((
                line.trim_start_matches('#').trim().to_string(),
                String::new(),
            ));
        } else if let Some((_, block)) = &mut current {
            if line.starts_with("## ") {
                blocks.push(current.take().unwrap());
            } else {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    if let Some(done) = current.take() {
        blocks.push(done);
    }
    blocks
}

fn parse_findings(text: &str, errors: &mut usize) -> Vec<Finding> {
    let mut out = Vec::new();
    for (header, block) in split_blocks(text) {
        let mut parts = header.splitn(2, " — ");
        let id = parts.next().unwrap_or("").trim().to_string();
        let title = parts.next().unwrap_or("").trim().to_string();
        if !id.starts_with("FND-") {
            continue;
        }
        let system = field(&block, "System");
        let source = field(&block, "Source");
        if system.is_empty() || source.is_empty() {
            *errors += 1;
            continue; // fail closed: a finding without provenance is a rumor
        }
        let answers: Vec<String> = field(&block, "Answers")
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        let carried_ref = field(&block, "Ref");
        out.push(Finding {
            id,
            title,
            system: system.to_lowercase(),
            kind: field(&block, "Kind").to_lowercase(),
            source,
            status: field(&block, "Status").to_lowercase(),
            relevance: field(&block, "Relevance"),
            answers,
            answers_ref: if carried_ref.is_empty() {
                None
            } else {
                Some(carried_ref)
            },
        });
    }
    out
}

/// Parse the query entries out of the scan inbox. Only
/// `CANDIDATE (query)` blocks are queries; the other candidate
/// kinds sharing the inbox are ignored. Fail-closed like the
/// findings parser: a query entry without a server-shaped id or a
/// system is skipped and counted, never approximated.
fn parse_queries(text: &str, errors: &mut usize) -> Vec<Query> {
    let mut out: Vec<Query> = Vec::new();
    for (header, block) in split_blocks(text) {
        let Some(title) = header.strip_prefix("CANDIDATE (query) — ") else {
            continue;
        };
        let id = field(&block, "Id");
        let system = field(&block, "System");
        if !id.starts_with("QRY-") || system.is_empty() {
            *errors += 1;
            continue;
        }
        if out.iter().any(|q: &Query| q.id == id) {
            continue; // first filing wins; a repeated id is one query
        }
        let carried_ref = field(&block, "Ref");
        out.push(Query {
            id,
            title: title.trim().to_string(),
            system: system.to_lowercase(),
            caller_ref: if carried_ref.is_empty() {
                None
            } else {
                Some(carried_ref)
            },
            source: field(&block, "Source"),
            summary: field(&block, "Summary"),
        });
    }
    out
}

fn parse_watchlist(text: &str) -> Vec<WatchEntry> {
    let mut out = Vec::new();
    let mut system = String::new();
    for line in text.lines() {
        if let Some(name) = line.strip_prefix("## ") {
            system = name.trim().to_lowercase();
            continue;
        }
        let trimmed = line.trim();
        if system.is_empty() || !trimmed.starts_with("- **") {
            continue;
        }
        // "- **arXiv:** cs.CR — decompilation ..." → venue + terms
        let Some(close) = trimmed.find(":**") else {
            continue;
        };
        let venue = trimmed[4..close].to_lowercase();
        let terms = trimmed[close + 3..].trim().to_string();
        out.push(WatchEntry {
            system: system.clone(),
            venue,
            terms,
        });
    }
    out
}

pub fn load(root: &Path) -> Store {
    let mut errors = 0usize;
    let mut query_errors = 0usize;
    let findings_text = fs::read_to_string(root.join("FINDINGS.md")).unwrap_or_default();
    let watch_text = fs::read_to_string(root.join("WATCHLIST.md")).unwrap_or_default();
    let inbox_text =
        fs::read_to_string(root.join("scans").join("SCAN-INBOX.md")).unwrap_or_default();
    Store {
        findings: parse_findings(&findings_text, &mut errors),
        watchlist: parse_watchlist(&watch_text),
        queries: parse_queries(&inbox_text, &mut query_errors),
        parse_errors: errors,
        query_parse_errors: query_errors,
    }
}

pub fn summary(store: &Store) -> serde_json::Value {
    let mut by_system: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    for f in &store.findings {
        *by_system.entry(f.system.clone()).or_default() += 1;
        *by_kind.entry(f.kind.clone()).or_default() += 1;
    }
    let mut watched: Vec<String> = store
        .watchlist
        .iter()
        .map(|w| w.system.clone())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    watched.sort();
    serde_json::json!({
        "findings_by_system": by_system,
        "findings_by_kind": by_kind,
        "systems_watched": watched,
        "watch_entries": store.watchlist.len(),
        "parse_errors": store.parse_errors,
    })
}

/// Format a lupin candidate payload for a revenue-shaped finding. This
/// tool only DRAFTS: the caller submits through lupin's own
/// opportunity_submit, so the research server never writes lupin's
/// files (the seam rule in DESIGN.md).
pub fn revenue_draft(f: &Finding) -> serde_json::Value {
    serde_json::json!({
        "submit_via": "lupin.opportunity_submit",
        "arguments": {
            "title": format!("Research-derived: {}", f.title),
            "source": f.source,
            "lane": "sfw",
            "why_it_fits": format!(
                "Finding {} ({}, {}) — {}", f.id, f.system, f.kind, f.relevance
            )
        },
        "note": "Draft only. Lupin validates money claims; this server does not write lupin's ledger or inbox."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINDINGS_FIXTURE: &str = "\
### FND-LUMEN-001 — Parametric body model paper
- **System:** lumen
- **Kind:** paper
- **Source:** arXiv:2511.03589
- **Status:** validated
- **Relevance:** Semantic parameter layer for the factory.

### FND-FATHOM-001 — Broken finding
- **Kind:** repo
- **Status:** validated

### FND-PHLOW-001 — Agent eval harness
- **System:** phlow
- **Kind:** repo
- **Source:** gh: example/evals
- **Status:** seed
- **Relevance:** Trajectory scoring for the reviewer stage.
";

    const WATCH_FIXTURE: &str = "\
## fathom
- **arXiv:** cs.CR — decompilation
- **GitHub:** topics decompiler

## lumen
- **arXiv:** cs.GR — parametric humans
";

    #[test]
    fn parses_valid_findings() {
        let mut errors = 0;
        let findings = parse_findings(FINDINGS_FIXTURE, &mut errors);
        assert_eq!(findings.len(), 2);
        assert_eq!(findings[0].system, "lumen");
        assert_eq!(findings[0].source, "arXiv:2511.03589");
        assert_eq!(findings[1].id, "FND-PHLOW-001");
    }

    #[test]
    fn finding_without_provenance_fails_closed() {
        let mut errors = 0;
        let findings = parse_findings(FINDINGS_FIXTURE, &mut errors);
        assert_eq!(errors, 1);
        assert!(findings.iter().all(|f| f.id != "FND-FATHOM-001"));
    }

    #[test]
    fn watchlist_parses_systems_and_venues() {
        let watch = parse_watchlist(WATCH_FIXTURE);
        assert_eq!(watch.len(), 3);
        assert_eq!(watch[0].system, "fathom");
        assert_eq!(watch[0].venue, "arxiv");
        assert_eq!(watch[2].system, "lumen");
    }

    #[test]
    fn revenue_draft_points_at_lupin_submit() {
        let f = Finding {
            id: "FND-LUMEN-001".into(),
            title: "Parametric body model paper".into(),
            system: "lumen".into(),
            kind: "paper".into(),
            source: "arXiv:2511.03589".into(),
            status: "validated".into(),
            relevance: "Semantic parameter layer.".into(),
            answers: Vec::new(),
            answers_ref: None,
        };
        let draft = revenue_draft(&f);
        assert_eq!(draft["submit_via"], "lupin.opportunity_submit");
        assert_eq!(draft["arguments"]["lane"], "sfw");
    }

    #[test]
    fn comment_blocks_are_not_findings() {
        let text = "<!--\n### FND-FAKE-001 — Template\n- **System:** x\n- **Source:** y\n-->\n";
        let mut errors = 0;
        assert!(parse_findings(text, &mut errors).is_empty());
    }

    const FINDINGS_WITH_LINKS: &str = "\
### FND-FATHOM-007 — Pak v11 header layout
- **System:** fathom
- **Kind:** repo
- **Source:** gh: example/pak
- **Status:** validated
- **Relevance:** Answers the filed layout question.
- **Answers:** QRY-FATHOM-001, QRY-FATHOM-002
- **Ref:** fathom:video/pak:u1

### FND-FATHOM-008 — Unlinked finding
- **System:** fathom
- **Kind:** paper
- **Source:** arXiv:0000.00000
- **Status:** validated
- **Relevance:** Answers nothing filed.
- **Answers:** QRY-FATHOM-999
";

    #[test]
    fn finding_link_fields_parse() {
        let mut errors = 0;
        let findings = parse_findings(FINDINGS_WITH_LINKS, &mut errors);
        assert_eq!(errors, 0);
        assert_eq!(
            findings[0].answers,
            vec!["QRY-FATHOM-001".to_string(), "QRY-FATHOM-002".to_string()]
        );
        assert_eq!(
            findings[0].answers_ref.as_deref(),
            Some("fathom:video/pak:u1")
        );
        assert_eq!(findings[1].answers, vec!["QRY-FATHOM-999".to_string()]);
        assert_eq!(findings[1].answers_ref, None);
    }

    const INBOX_FIXTURE: &str = "\
### CANDIDATE (finding) — An ordinary candidate
- **System:** lumen
- **Source:** arXiv:0000.00000
- **Summary:** not a query; the query parser ignores it
- **Status:** candidate (unvalidated — filed by MCP caller)

### CANDIDATE (query) — Which pak version ships in the build?
- **Id:** QRY-FATHOM-001
- **System:** fathom
- **Ref:** fathom:video/pak:u1
- **Source:** youtube: example
- **Summary:** version unknown from frames
- **Status:** query (filed by MCP caller; awaiting curation)

### CANDIDATE (query) — Id-less hand-written entry
- **System:** fathom
- **Source:** youtube: example
- **Summary:** no server id; fails closed
- **Status:** query (filed by MCP caller; awaiting curation)
";

    #[test]
    fn queries_parse_from_inbox() {
        let mut errors = 0;
        let queries = parse_queries(INBOX_FIXTURE, &mut errors);
        assert_eq!(queries.len(), 1);
        assert_eq!(errors, 1); // the id-less entry fails closed
        assert_eq!(queries[0].id, "QRY-FATHOM-001");
        assert_eq!(queries[0].system, "fathom");
        assert_eq!(
            queries[0].caller_ref.as_deref(),
            Some("fathom:video/pak:u1")
        );
        assert_eq!(queries[0].title, "Which pak version ships in the build?");
    }

    #[test]
    fn ref_and_id_validation() {
        assert!(validate_ref("fathom:video/abc123:u3").is_ok());
        assert!(validate_ref("plain-ref_1.2").is_ok());
        assert!(validate_ref("").is_err());
        assert!(validate_ref("has space").is_err());
        assert!(validate_ref("line\nbreak").is_err());
        assert!(validate_ref("mark**down").is_err());
        assert!(validate_ref(&"x".repeat(129)).is_err());
        assert!(validate_query_id("QRY-FATHOM-001").is_ok());
        assert!(validate_query_id("FND-FATHOM-001").is_err());
        assert!(validate_query_id("QRY-").is_err());
        assert!(validate_query_id("QRY-HAS SPACE-1").is_err());
    }

    #[test]
    fn query_id_allocation_is_per_system_and_monotonic() {
        let mut errors = 0;
        let queries = parse_queries(INBOX_FIXTURE, &mut errors);
        assert_eq!(next_query_id(&queries, "fathom"), "QRY-FATHOM-002");
        assert_eq!(next_query_id(&queries, "lumen"), "QRY-LUMEN-001");
        assert_eq!(next_query_id(&[], "phlow"), "QRY-PHLOW-001");
    }

    #[test]
    fn load_reads_queries_and_counts_their_parse_errors() {
        let root = std::env::temp_dir().join(format!("phaedrus-store-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("scans")).unwrap();
        std::fs::write(root.join("FINDINGS.md"), FINDINGS_FIXTURE).unwrap();
        std::fs::write(root.join("WATCHLIST.md"), WATCH_FIXTURE).unwrap();
        std::fs::write(root.join("scans").join("SCAN-INBOX.md"), INBOX_FIXTURE).unwrap();
        let store = load(&root);
        assert_eq!(store.findings.len(), 2);
        assert_eq!(store.queries.len(), 1);
        assert_eq!(store.query_parse_errors, 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn title_normal_form_folds_case_and_whitespace() {
        assert_eq!(
            normalized_title("  Which   Pak\nVersion? "),
            "which pak version?"
        );
        assert_eq!(collapse_ws("a\n  b\tc"), "a b c");
    }
}
