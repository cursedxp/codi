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
                },
                {
                    "name": "list_pending_improvements",
                    "description": "Return all queued improvement proposals awaiting review. Call this after run_task or get_diff to check for pending items. For each item review risk_reason and source_signals, then call approve_improvement or dismiss_improvement.",
                    "inputSchema": { "type": "object", "properties": {} }
                },
                {
                    "name": "approve_improvement",
                    "description": "Apply a queued improvement by id. Creates a branch, runs Goose, then runs tests and lint. Returns Applied (with branch name) or Failed (with reason). Blocklist and quota checks are skipped since you have reviewed it; test and lint gates still apply.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "The improvement id from list_pending_improvements" }
                        },
                        "required": ["id"]
                    }
                },
                {
                    "name": "dismiss_improvement",
                    "description": "Remove a queued improvement without applying it. Records the dismissal in the improvement log.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "id":     { "type": "string", "description": "The improvement id to dismiss" },
                            "reason": { "type": "string", "description": "Optional reason for dismissal" }
                        },
                        "required": ["id"]
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
                "run_task"                   => tool_run_task(cfg, repo_root, &args),
                "get_diff"                   => tool_get_diff(repo_root, &args),
                "run_tests"                  => tool_run_tests(cfg, repo_root),
                "list_pending_improvements"  => tool_list_pending(repo_root),
                "approve_improvement"        => tool_approve(cfg, repo_root, &args),
                "dismiss_improvement"        => tool_dismiss(repo_root, &args),
                other                        => anyhow::bail!("unknown tool: {other}"),
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

fn tool_list_pending(repo_root: &Path) -> Result<Value> {
    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let queue = crate::pending::PendingQueue::load(&pending_path)?;
    let items: Vec<Value> = queue.items().iter().map(|c| serde_json::json!({
        "id":             c.id,
        "description":    c.description,
        "risk":           format!("{:?}", c.risk),
        "risk_reason":    c.risk_reason,
        "context":        c.context,
        "source_signals": c.source_signals,
        "created_at":     c.created_at,
    })).collect();
    let count = items.len();
    let text = serde_json::to_string_pretty(&serde_json::json!({
        "pending": items,
        "count":   count,
    })).context("serializing pending list")?;
    Ok(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
}

fn tool_approve(cfg: &Config, repo_root: &Path, args: &Value) -> Result<Value> {
    let id = args["id"].as_str().context("missing 'id' argument")?;

    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let mut queue = crate::pending::PendingQueue::load(&pending_path)?;
    let candidate = queue.remove(id)
        .ok_or_else(|| anyhow::anyhow!("no pending improvement with id '{id}'"))?;
    queue.save()?;

    let executor = crate::improve::ImprovementExecutor::new(cfg, repo_root);
    let outcome = executor.run_approved(&candidate)?;

    let text = match &outcome {
        crate::improve::Outcome::Applied { branch } =>
            format!("{{\"outcome\":\"Applied\",\"branch\":\"{branch}\",\"tests_passed\":true}}"),
        crate::improve::Outcome::Failed { reason } =>
            format!("{{\"outcome\":\"Failed\",\"reason\":\"{reason}\"}}"),
        crate::improve::Outcome::Skipped { reason } =>
            format!("{{\"outcome\":\"Skipped\",\"reason\":\"{reason}\"}}"),
    };
    Ok(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
}

fn tool_dismiss(repo_root: &Path, args: &Value) -> Result<Value> {
    let id = args["id"].as_str().context("missing 'id' argument")?;
    let reason = args["reason"].as_str().map(|s| s.to_string());

    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let mut queue = crate::pending::PendingQueue::load(&pending_path)?;
    let candidate = queue.remove(id)
        .ok_or_else(|| anyhow::anyhow!("no pending improvement with id '{id}'"))?;
    queue.save()?;

    crate::improve::append_log(repo_root, &crate::improve::LogEntry {
        id: candidate.id.clone(),
        description: candidate.description.clone(),
        risk: format!("{:?}", candidate.risk),
        branch: String::new(),
        outcome: "Dismissed".to_string(),
        reason,
        approved_by_claude: true,
        blocklist_bypassed: false,
        source_signals: candidate.source_signals,
        created_at: candidate.created_at,
        completed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })?;

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": format!("{{\"outcome\":\"Dismissed\",\"id\":\"{id}\"}}") }]
    }))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod mcp_improve_tests {
    use super::*;
    use crate::config::Config;
    use tempfile::tempdir;

    fn init_git(dir: &std::path::Path) {
        for args in [
            vec!["init"],
            vec!["config", "user.email", "t@t.com"],
            vec!["config", "user.name", "T"],
            vec!["commit", "--allow-empty", "-m", "init"],
        ] {
            std::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status().unwrap();
        }
    }

    #[test]
    fn list_pending_returns_empty_on_no_queue_file() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/call", &serde_json::json!({
            "name": "list_pending_improvements",
            "arguments": {}
        })).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["count"], 0);
        assert!(parsed["pending"].as_array().unwrap().is_empty());
    }

    #[test]
    fn approve_unknown_id_returns_error() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/call", &serde_json::json!({
            "name": "approve_improvement",
            "arguments": { "id": "doesnotexist" }
        }));
        assert!(result.is_err());
    }

    #[test]
    fn dismiss_unknown_id_returns_error() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/call", &serde_json::json!({
            "name": "dismiss_improvement",
            "arguments": { "id": "nope", "reason": "not relevant" }
        }));
        assert!(result.is_err());
    }

    #[test]
    fn tools_list_includes_new_tools() {
        let dir = tempdir().unwrap();
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/list", &serde_json::Value::Null).unwrap();
        let tools = result["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"list_pending_improvements"));
        assert!(names.contains(&"approve_improvement"));
        assert!(names.contains(&"dismiss_improvement"));
    }
}
