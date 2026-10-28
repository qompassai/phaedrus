//! phaedrus — MCP server for the research feed (stdio).
//!
//! Serves the estate's literature watch: the curated findings store
//! and the watchlist, plus the one-way revenue seam into lupin
//! (draft-only; lupin owns its ledger). Named by the Owner (2026-10-19)
//! for Pirsig's Phaedrus — the pursuit of Quality before adoption.
//!
//! As of 0.2.0 (WS2 of the MCP standardization program, decision
//! D-113) this server runs on rmcp, the official Rust MCP SDK, and
//! speaks the modern protocol era only (2026-07-28): sessions open
//! with `server/discover`, and every request carries its protocol
//! version in `_meta` — there is no `initialize` handshake. Legacy
//! contact is refused at the stdio front door (see `guard`) with
//! the typed `-32022` unsupported-version error, naming 2026-07-28
//! as the one supported version. The findings/watchlist markdown
//! files and the on-disk inbox format are unchanged.
//!
//! 0.3.0 adds the P-1 research-query path: `research_submit`
//! accepts `kind: "query"` (with an optional caller `ref`) and
//! returns the filed query's candidate id, and `query_status`
//! correlates an id or ref with its curated answer. Curation
//! stays human; the server only makes the linkage addressable.
//!
//! Usage: `phaedrus [--root <dir>] [--lupin-root <dir>]`. The roots
//! default to `$RESEARCH_ROOT` / `$LUPIN_ROOT`, then
//! `$HOME/workspace/research-feed-20261019` and
//! `$HOME/workspace/revenue-alignment-20261019`.

#![forbid(unsafe_code)]

mod guard;
mod server;
mod store;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::transport::async_rw::AsyncRwTransport;
use server::Config;
use tokio::sync::Mutex;

/// Duplex buffer between the era guard and the SDK service. Request
/// lines can reach the 4 MiB line bound; the SDK consumes
/// incrementally, so this only needs to cover response bursts.
const DUPLEX_BUFFER_BYTES: usize = 1024 * 1024;

/// Parse the baseline's CLI: `--root`, `--lupin-root`, the
/// `RESEARCH_ROOT` / `LUPIN_ROOT` fallbacks, unknown args warned
/// about and ignored (the baseline's behavior).
fn parse_config(args: &[String]) -> Config {
    let mut root: Option<PathBuf> = std::env::var_os("RESEARCH_ROOT").map(PathBuf::from);
    let mut lupin_root: Option<PathBuf> = std::env::var_os("LUPIN_ROOT").map(PathBuf::from);
    let mut argv = args.iter();
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--root" => root = argv.next().map(PathBuf::from),
            "--lupin-root" => lupin_root = argv.next().map(PathBuf::from),
            other => eprintln!("phaedrus: ignoring unknown arg {other}"),
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let root = root.unwrap_or_else(|| home.join("workspace").join("research-feed-20261019"));
    let lupin_root =
        lupin_root.unwrap_or_else(|| home.join("workspace").join("revenue-alignment-20261019"));
    Config { root, lupin_root }
}

/// Wire the pieces together: the guard owns stdio, the SDK service
/// sits behind an in-process duplex, and a pump copies its responses
/// back to stdout through the same lock the guard writes with.
async fn run(cfg: Config) -> std::io::Result<()> {
    let (client_side, server_side) = tokio::io::duplex(DUPLEX_BUFFER_BYTES);
    let (server_read, server_write) = tokio::io::split(server_side);
    let (client_read, client_write) = tokio::io::split(client_side);
    let stdout: guard::SharedStdout = Arc::new(Mutex::new(tokio::io::stdout()));

    let ordering = guard::Ordering::new();
    let handler = server::PhaedrusServer::new(cfg);
    let serve = tokio::spawn(async move {
        let transport = AsyncRwTransport::new_server(server_read, server_write);
        match handler.serve(transport).await {
            Ok(running) => running
                .waiting()
                .await
                .map(|_| ())
                .map_err(|e| std::io::Error::other(e.to_string())),
            // The client closed the connection before selecting a
            // lifecycle (a probe, a spawn check, a client that only
            // ever wanted the refusal): the baseline treated bare
            // EOF as a clean end of session, and so do we.
            Err(rmcp::service::ServerInitializeError::ConnectionClosed(_)) => Ok(()),
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    });
    let mut pump = tokio::spawn(guard::pump(
        client_read,
        Arc::clone(&stdout),
        Arc::clone(&ordering),
    ));

    // When stdin reaches EOF the guard shuts the duplex write side
    // down, which ends the SDK service; the pump ends when the
    // service's transport drops. The pump wait is bounded as a
    // backstop: a response still in flight when the service ends
    // gets its chance to drain, but a wedged pump cannot hang exit.
    let guard_result = guard::run(tokio::io::stdin(), client_write, stdout, ordering).await;
    let serve_result = match serve.await {
        Ok(result) => result,
        Err(e) => Err(std::io::Error::other(e.to_string())),
    };
    if tokio::time::timeout(std::time::Duration::from_secs(2), &mut pump)
        .await
        .is_err()
    {
        pump.abort();
    }
    guard_result.and(serve_result)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cfg = parse_config(&args);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("phaedrus: cannot start runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cfg)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("phaedrus: serve loop ended: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_config_accepts_forms() {
        let cfg = parse_config(&[]);
        assert!(cfg.root.ends_with("research-feed-20261019"));
        assert!(cfg.lupin_root.ends_with("revenue-alignment-20261019"));
        let cfg = parse_config(&[
            "--root".to_string(),
            "/tmp/feed".to_string(),
            "--lupin-root".to_string(),
            "/tmp/lupin".to_string(),
        ]);
        assert_eq!(cfg.root, PathBuf::from("/tmp/feed"));
        assert_eq!(cfg.lupin_root, PathBuf::from("/tmp/lupin"));
    }
}
