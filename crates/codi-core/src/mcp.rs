//! MCP (Model Context Protocol) server mode for codi.
//!
//! `codi mcp` speaks JSON-RPC 2.0 over stdio. Claude Code (or any MCP client)
//! adds this server to its config and gains three tools:
//!
//! - `run_task`   — implement a feature or fix via the local AI agent (Goose)
//! - `get_diff`   — read the current git diff for review
//! - `run_tests`  — run the project's configured test suite

use std::io::{BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::Value;

use crate::config::Config;
use crate::engine::run_session_mcp;

/// Start the MCP stdio server. Blocks until stdin is closed.
pub fn serve(cfg: &Config, repo_root: &Path) -> Result<()> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line.context("reading mcp stdin")?;
        if line.trim().is_empty() {
            continue;
        }

        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let err = serde_json::json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": { "code": -32700, "message": format!("parse error: {e}") }
                });
                writeln!(out, "{}", serde_json::to_string(&err).unwrap())?;
                out.flush()?;
                continue;
            }
        };

        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_default();

        let resp = match dispatch(cfg, repo_root, method, &params) {
            Ok(r) => serde_json::json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err(e) => serde_json::json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32603, "message": e.to_string() }
            }),
        };

        writeln!(out, "{}", serde_json::to_string(&resp).unwrap())?;
        out.flush()?;
    }

    Ok(())
}

fn dispatch(cfg: &Config, repo_root: &Path, method: &str, params: &Value) -> Result<Value> {
    match method {
        "initialize" | "notifications/initialized" => Ok(serde_json::json!({
            "protocolVersion": "2024-11-05",
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "codi", "version": env!("CARGO_PKG_VERSION") }
        })),

        "tools/list" => Ok(serde_json::json!({
            "tools": [
                {
                    "name": "run_task",
                    "description": "Run a coding task using the local AI agent (codi/Goose). Use for implementing features, bug fixes, refactors, or applying review findings. Goose streams its output to the user's terminal via stderr.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "task": {
                                "type": "string",
                                "description": "The coding task to perform in natural language"
                            }
                        },
                        "required": ["task"]
                    }
                },
                {
                    "name": "get_diff",
                    "description": "Return the current git diff. Call after run_task to review what the agent changed.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "base": {
                                "type": "string",
                                "description": "Base git ref (default: HEAD). Use 'HEAD~1' to include the last commit."
                            }
                        }
                    }
                },
                {
                    "name": "run_tests",
                    "description": "Run the project's test suite as configured in codi.toml [commands].test. Returns combined stdout/stderr and the exit code.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {}
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
                "run_task"   => tool_run_task(cfg, repo_root, &args),
                "get_diff"   => tool_get_diff(repo_root, &args),
                "run_tests"  => tool_run_tests(cfg, repo_root),
                other        => anyhow::bail!("unknown tool: {other}"),
            }
        }

        other => anyhow::bail!("unknown method: {other}"),
    }
}

// ---------------------------------------------------------------------------
// Tool implementations
// ---------------------------------------------------------------------------

fn tool_run_task(cfg: &Config, repo_root: &Path, args: &Value) -> Result<Value> {
    let task = args["task"].as_str().context("missing 'task' argument")?;

    let exit_code = run_session_mcp(cfg, task, None, repo_root, "")?;

    let message = if exit_code == 0 {
        "Task complete. Call get_diff to review the changes.".to_string()
    } else {
        format!("Agent exited with code {exit_code}. Check terminal output for details.")
    };

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": format!("{message}\nexit_code: {exit_code}") }]
    }))
}

fn tool_get_diff(repo_root: &Path, args: &Value) -> Result<Value> {
    let base = args["base"].as_str().unwrap_or("HEAD");

    let diff_out = std::process::Command::new("git")
        .args(["diff", base])
        .current_dir(repo_root)
        .output()
        .context("running git diff")?;

    let diff = String::from_utf8_lossy(&diff_out.stdout).to_string();

    let files_out = std::process::Command::new("git")
        .args(["diff", "--name-only", base])
        .current_dir(repo_root)
        .output()
        .context("running git diff --name-only")?;

    let files: Vec<&str> = std::str::from_utf8(&files_out.stdout)
        .unwrap_or("")
        .lines()
        .filter(|l| !l.is_empty())
        .collect();

    let text = if diff.is_empty() {
        format!("No changes relative to {base}.")
    } else {
        format!("Changed files: {}\n\n{diff}", files.join(", "))
    };

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}

fn tool_run_tests(cfg: &Config, repo_root: &Path) -> Result<Value> {
    let test_cmd = match &cfg.commands.test {
        Some(cmd) if !cmd.is_empty() => cmd.clone(),
        _ => {
            return Ok(serde_json::json!({
                "content": [{ "type": "text", "text": "No test command configured. Set [commands].test in codi.toml." }]
            }));
        }
    };

    let mut parts = test_cmd.split_whitespace();
    let program = parts.next().context("empty test command")?;
    let args: Vec<&str> = parts.collect();

    let output = std::process::Command::new(program)
        .args(&args)
        .current_dir(repo_root)
        .output()
        .with_context(|| format!("running: {test_cmd}"))?;

    let exit_code = output.status.code().unwrap_or(1);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let text = format!(
        "exit_code: {exit_code}\n\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": text }]
    }))
}
