# Phaedrus

An MCP server for the research feed: a curated literature-watch
findings store and watchlist, plus a one-way, draft-only revenue seam
into [lupin](https://github.com/qompassai/lupin) (the revenue-alignment
server, which owns its own ledger).

Phaedrus is a single Rust binary. It speaks MCP over stdio
(NDJSON JSON-RPC) to the client and works on plain files under a data
root — no database, no network listener.

## How it runs

- **Data root** (findings store, watchlist, scan inbox):
  `--root <path>` or the `RESEARCH_ROOT` environment variable
  (default `~/workspace/research-feed-20261019`).
- **Lupin root** (read-only — capability context and draft generation):
  `--lupin-root <path>` or `LUPIN_ROOT`
  (default `~/workspace/revenue-alignment-20261019`).
- Writes are deliberately narrow: `research_submit` files a candidate
  into the scan inbox and never touches the curated store, and
  `revenue_candidate_draft` only *drafts* a lupin
  `opportunity_submit` payload — this server never writes lupin's
  files.

## Tools

| Tool | What it returns |
| --- | --- |
| `capability_context` | Lupin's capability registry (read-only) as planning vocabulary: capability ids + names, optionally keyword-filtered |
| `feed_summary` | Counts: findings by system/kind, systems watched, parse health |
| `finding_get` | One curated finding by id |
| `finding_list` | Curated findings, optionally filtered by system |
| `research_submit` | File a candidate (finding, roadmap item, or test proposal) into the scan inbox; never writes the curated store |
| `revenue_candidate_draft` | A draft lupin `opportunity_submit` payload for a revenue-shaped finding (drafts only) |
| `watchlist_list` | The watchlist (per-system venues and terms) |

## Client config

```json
{
  "mcpServers": {
    "phaedrus": {
      "command": "/path/to/phaedrus",
      "args": []
    }
  }
}
```

## Registry

`server.json` in this repo is the MCP Registry record for
`ai.qompass/phaedrus`. The server is distributed as an MCPB bundle
attached to the GitHub releases (Linux x64).

## License

Apache-2.0 — see [LICENSE](./LICENSE).
