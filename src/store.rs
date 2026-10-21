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
    pub parse_errors: usize,
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

fn parse_findings(text: &str, errors: &mut usize) -> Vec<Finding> {
    let mut out = Vec::new();
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
    for (header, block) in blocks {
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
        out.push(Finding {
            id,
            title,
            system: system.to_lowercase(),
            kind: field(&block, "Kind").to_lowercase(),
            source,
            status: field(&block, "Status").to_lowercase(),
            relevance: field(&block, "Relevance"),
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
    let findings_text = fs::read_to_string(root.join("FINDINGS.md")).unwrap_or_default();
    let watch_text = fs::read_to_string(root.join("WATCHLIST.md")).unwrap_or_default();
    Store {
        findings: parse_findings(&findings_text, &mut errors),
        watchlist: parse_watchlist(&watch_text),
        parse_errors: errors,
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
}
