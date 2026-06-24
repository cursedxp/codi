//! `codi-rag` — standalone MCP server that provides repository context retrieval.
//!
//! When launched by Goose as an MCP extension it speaks JSON-RPC over stdio.
//! It also exposes a simple `index` subcommand for one-shot indexing from the
//! codi CLI.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use codi_rag::{db, index::IndexConfig, index::index_repo, search::bm25_search, search::format_context};

#[derive(Parser)]
#[command(name = "codi-rag", about = "codi repository RAG index and MCP server")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Index a repository into the SQLite store.
    Index {
        /// Path to the repository root (defaults to current dir).
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Path to the SQLite database.
        #[arg(long, default_value = ".codi/index.sqlite")]
        db: PathBuf,
        /// Rebuild the index from scratch.
        #[arg(long)]
        rebuild: bool,
    },
    /// Search the existing index and print results.
    Search {
        query: String,
        #[arg(long, default_value = ".codi/index.sqlite")]
        db: PathBuf,
        #[arg(long, default_value_t = 5)]
        k: usize,
    },
    /// Run as an MCP server on stdio (used by Goose as an extension).
    Mcp {
        #[arg(long, default_value = ".codi/index.sqlite")]
        db: PathBuf,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        Cmd::Index { repo, db, rebuild } => {
            if rebuild && db.exists() {
                std::fs::remove_file(&db).context("removing old index")?;
            }
            let mut conn = db::open(&db)?;
            let cfg = IndexConfig {
                extensions: vec![
                    "rs", "ts", "tsx", "js", "jsx", "py", "go", "md", "toml", "yaml", "yml",
                ]
                .into_iter()
                .map(String::from)
                .collect(),
                exclude: vec!["target".to_string(), "node_modules".to_string()],
                max_chunk_chars: 1200,
            };
            let n = index_repo(&mut conn, &repo, &cfg)?;
            eprintln!("Indexed {n} chunks into {}", db.display());
        }
        Cmd::Search { query, db, k } => {
            let conn = db::open(&db)?;
            let chunks = bm25_search(&conn, &query, k)?;
            println!("{}", format_context(&chunks));
        }
        Cmd::Mcp { db } => {
            let conn = db::open(&db)?;
            mcp_server_loop(conn)?;
        }
    }

    Ok(())
}

/// Minimal MCP server loop over stdio (JSON-RPC 2.0 line-delimited).
fn mcp_server_loop(conn: rusqlite::Connection) -> Result<()> {
    use std::io::{BufRead, Write};

    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line.context("reading mcp stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        let req: serde_json::Value =
            serde_json::from_str(&line).context("parsing mcp request")?;

        let id = req.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let method = req
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_default();

        let result = dispatch(&conn, method, &params);

        let resp = match result {
            Ok(r) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32603, "message": e.to_string() }
            }),
        };

        let line = serde_json::to_string(&resp).unwrap();
        writeln!(out, "{line}")?;
        out.flush()?;
    }

    Ok(())
}

fn dispatch(
    conn: &rusqlite::Connection,
    method: &str,
    params: &serde_json::Value,
) -> Result<serde_json::Value> {
    match method {
        "initialize" | "notifications/initialized" => {
            Ok(serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "codi-rag", "version": env!("CARGO_PKG_VERSION") }
            }))
        }
        "tools/list" => Ok(serde_json::json!({
            "tools": [
                {
                    "name": "search_context",
                    "description": "Search the repository index for snippets relevant to a query.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "query": { "type": "string", "description": "The search query" },
                            "k": { "type": "integer", "description": "Max results (default 5)" }
                        },
                        "required": ["query"]
                    }
                },
                {
                    "name": "get_file",
                    "description": "Read a file from the repository.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": { "type": "string" }
                        },
                        "required": ["path"]
                    }
                }
            ]
        })),
        "tools/call" => {
            let name = params
                .get("name")
                .and_then(|n| n.as_str())
                .context("missing tool name")?;
            let args = params.get("arguments").cloned().unwrap_or_default();
            match name {
                "search_context" => {
                    let query = args["query"].as_str().context("missing query")?;
                    let k = args["k"].as_u64().unwrap_or(5) as usize;
                    let chunks = bm25_search(conn, query, k)?;
                    let text = format_context(&chunks);
                    Ok(serde_json::json!({
                        "content": [{ "type": "text", "text": text }]
                    }))
                }
                "get_file" => {
                    let path = args["path"].as_str().context("missing path")?;
                    let content = std::fs::read_to_string(path)
                        .with_context(|| format!("reading {path}"))?;
                    Ok(serde_json::json!({
                        "content": [{ "type": "text", "text": content }]
                    }))
                }
                other => anyhow::bail!("unknown tool: {other}"),
            }
        }
        other => anyhow::bail!("unknown method: {other}"),
    }
}
