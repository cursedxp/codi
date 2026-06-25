//! Engine: maps a `Config` to a session-scoped Goose configuration and
//! launches the `goose` binary (REPL or one-shot).
//!
//! `codi` drives Goose as a subprocess rather than linking its crates. This
//! keeps our build fast and hermetic and insulates us from upstream churn.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::config::{Config, SafetyConfig};
use crate::routing::{pick_provider, Provider};

/// How to launch Goose.
pub enum SessionMode {
    /// Launch the interactive REPL. Goose inherits stdin/stdout/stderr.
    Interactive,
    /// Run a single task and exit. Goose stdout/stderr are passed through.
    OneShot(String),
}

/// Build the session-scoped Goose config YAML, write it to a temp file, and
/// launch the `goose` binary with that config.
///
/// Returns the exit status code.
pub fn run_session(
    cfg: &Config,
    task: &str,
    mode: SessionMode,
    rag_socket: Option<&str>,
    repo_root: &Path,
    context_snippets: &str,
) -> Result<i32> {
    let goose_bin = locate_goose(cfg)?;
    let provider = pick_provider(cfg, task);

    let session_dir = repo_root.join(".codi").join("session");
    std::fs::create_dir_all(&session_dir)
        .context("creating .codi/session/ dir")?;

    let goose_cfg_path = session_dir.join("goose-session.yaml");
    let goose_cfg = build_goose_config(&cfg.safety, &provider, rag_socket, context_snippets);
    std::fs::write(&goose_cfg_path, goose_cfg)
        .context("writing session goose config")?;

    let mut cmd = build_command(&goose_bin, &goose_cfg_path, &mode, repo_root);

    tracing::debug!(
        goose_bin = %goose_bin.display(),
        config = %goose_cfg_path.display(),
        "launching goose"
    );

    let status = cmd
        .status()
        .with_context(|| format!("failed to execute goose binary at {}", goose_bin.display()))?;

    Ok(status.code().unwrap_or(1))
}

/// Locate the goose binary: first try the explicit config path, then PATH.
pub fn locate_goose(cfg: &Config) -> Result<PathBuf> {
    if let Some(explicit) = &cfg.goose_bin {
        let p = PathBuf::from(explicit);
        if p.exists() {
            return Ok(p);
        }
        bail!("configured goose_bin '{}' does not exist", explicit);
    }

    which_goose().context(
        "could not find a 'goose' binary on PATH. \
         Install Block's Goose (https://github.com/block/goose) \
         or set goose_bin in codi.toml."
    )
}

fn which_goose() -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|path_var| {
        std::env::split_paths(&path_var).find_map(|dir| {
            let candidate = dir.join("goose");
            candidate.is_file().then_some(candidate)
        })
    })
}

/// Build the session-scoped Goose config as YAML.
///
/// Goose supports a `--config` flag that points to a YAML file. We generate
/// a minimal one that sets the provider/model and registers the RAG MCP
/// extension when the RAG socket is available.
fn build_goose_config(
    safety: &SafetyConfig,
    provider: &Provider,
    rag_socket: Option<&str>,
    context_snippets: &str,
) -> String {
    let provider_section = match provider {
        Provider::Local(m) => {
            // Goose reads OPENAI_BASE_URL for custom endpoints; OPENAI_API_KEY
            // is required but can be any non-empty string for Ollama.
            let key = if m.api_key.is_empty() { "ollama".to_string() } else { m.api_key.clone() };
            format!(
                "GOOSE_PROVIDER: openai\nOPENAI_BASE_URL: {}\nGOOSE_MODEL: {}\nOPENAI_API_KEY: {}\n",
                m.base_url, m.model, key
            )
        }
        Provider::Cloud(m) => {
            let api_key_val = std::env::var(&m.api_key_env).unwrap_or_default();
            match m.provider.as_str() {
                "anthropic" => format!(
                    "GOOSE_PROVIDER: anthropic\nGOOSE_MODEL: {}\nANTHROPIC_API_KEY: {}\n",
                    m.model, api_key_val
                ),
                _ => format!(
                    "GOOSE_PROVIDER: openai\nOPENAI_BASE_URL: {}\nGOOSE_MODEL: {}\nOPENAI_API_KEY: {}\n",
                    m.base_url.as_deref().unwrap_or("https://api.openai.com"), m.model, api_key_val
                ),
            }
        }
    };

    // Build MCP extensions list
    let mut extensions = String::new();
    if let Some(socket) = rag_socket {
        extensions.push_str(&format!(
            "extensions:\n  codi-rag:\n    type: stdio\n    cmd: codi-rag\n    args: [\"--mcp\", \"--db\", \"{socket}\"]\n    enabled: true\n"
        ));
    }

    // Inject retrieval context as a hint comment at the top of the system note
    let context_note = if context_snippets.is_empty() {
        String::new()
    } else {
        format!(
            "# Retrieved context:\n{}\n",
            context_snippets
        )
    };

    let safety_note = format!(
        "# safety: confirm_commands={} confirm_writes={}\n",
        safety.confirm_commands, safety.confirm_writes
    );

    format!(
        "# codi session config — auto-generated, do not edit manually\n{safety_note}{provider_section}{extensions}{context_note}"
    )
}

fn build_command(
    goose_bin: &Path,
    cfg_path: &Path,
    mode: &SessionMode,
    repo_root: &Path,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(goose_bin);
    cmd.current_dir(repo_root);
    // Goose reads provider env vars; we write them into the YAML and also
    // set them explicitly here to ensure they take effect.
    set_env_from_yaml_if_needed(&mut cmd, cfg_path);
    cmd.env("OLLAMA_NUM_CTX", "8192");

    match mode {
        SessionMode::Interactive => {
            // `goose session` (or bare `goose`) starts the interactive REPL.
            cmd.arg("session");
            cmd.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
        SessionMode::OneShot(task) => {
            // `goose run --text "<task>"` runs non-interactively.
            cmd.args(["run", "--text", task]);
            cmd.arg("--system").arg(crate::standards::CODING_STANDARDS);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
    }
    cmd
}

/// Append `--provider <p> --model <m>` so CLI flags override both the global
/// Goose config file (~/.config/goose/config.yaml) and env vars.
/// Local models always use the openai-compatible endpoint via OPENAI_BASE_URL.
fn append_provider_flags(cmd: &mut std::process::Command, provider: &Provider) {
    match provider {
        Provider::Local(m) => {
            cmd.args(["--provider", "openai", "--model", &m.model]);
        }
        Provider::Cloud(m) => {
            cmd.args(["--provider", &m.provider, "--model", &m.model]);
        }
    }
}

/// Goose primarily reads provider settings from environment variables.
/// Parse our generated YAML's simple `KEY: value` lines and set them.
fn set_env_from_yaml_if_needed(cmd: &mut std::process::Command, yaml_path: &Path) {
    let Ok(text) = std::fs::read_to_string(yaml_path) else { return };
    for line in text.lines() {
        if line.starts_with('#') || line.starts_with(' ') {
            continue;
        }
        if let Some((key, val)) = line.split_once(": ") {
            let key = key.trim();
            let val = val.trim();
            if key == key.to_uppercase().as_str() {
                cmd.env(key, val);
            }
        }
    }
}

/// Like `run_session` but for MCP server mode: Goose's stdout is piped to our
/// stderr so the JSON-RPC stdout stream isn't corrupted. The user still sees
/// Goose's live output on their terminal via stderr.
pub fn run_session_mcp(
    cfg: &Config,
    task: &str,
    rag_socket: Option<&str>,
    repo_root: &Path,
    context_snippets: &str,
) -> Result<i32> {
    let goose_bin = locate_goose(cfg)?;
    let provider = pick_provider(cfg, task);

    let session_dir = repo_root.join(".codi").join("session");
    std::fs::create_dir_all(&session_dir).context("creating .codi/session/ dir")?;

    let goose_cfg_path = session_dir.join("goose-session.yaml");
    let goose_cfg = build_goose_config(&cfg.safety, &provider, rag_socket, context_snippets);
    std::fs::write(&goose_cfg_path, &goose_cfg).context("writing session goose config")?;

    let mut cmd = std::process::Command::new(&goose_bin);
    cmd.current_dir(repo_root);
    set_env_from_yaml_if_needed(&mut cmd, &goose_cfg_path);
    // Ollama's default context window (2048 tokens) is too small: Goose's system
    // prompt + tool schemas consume ~1.5k tokens, leaving almost nothing for the
    // task. Truncated schemas cause silent tool-call failures.
    cmd.env("OLLAMA_NUM_CTX", "8192");
    cmd.args(["run", "--text", task]);
    // CLI flags override both Goose's global config file and env vars, ensuring
    // the model selected in codi.toml is actually used.
    append_provider_flags(&mut cmd, &provider);
    cmd.arg("--system").arg(crate::standards::CODING_STANDARDS);
    // Pipe goose stdout → a thread that echoes it to our stderr.
    // This keeps the MCP JSON-RPC stream on stdout clean.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    tracing::debug!(
        goose_bin = %goose_bin.display(),
        "launching goose (mcp mode)"
    );

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to execute goose at {}", goose_bin.display()))?;

    if let Some(goose_stdout) = child.stdout.take() {
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for line in BufReader::new(goose_stdout).lines().map_while(Result::ok) {
                eprintln!("{line}");
            }
        });
    }

    let status = child
        .wait()
        .with_context(|| format!("waiting for goose at {}", goose_bin.display()))?;

    Ok(status.code().unwrap_or(1))
}

/// Generate a description of what `provider` is, for display to the user.
pub fn provider_label(provider: &Provider) -> String {
    match provider {
        Provider::Local(m) => format!("local ({} @ {})", m.model, m.base_url),
        Provider::Cloud(m) => format!("cloud ({}/{})", m.provider, m.model),
    }
}

/// Convenience: pick provider for `task` and return its label string.
pub fn pick_provider_label(cfg: &Config, task: &str) -> String {
    let provider = pick_provider(cfg, task);
    provider_label(&provider)
}

/// Construct a Value suitable for passing to the model as system/context JSON.
/// This is used by review.rs to inject diff content.
pub fn make_review_payload(diff: &str, extra: Value) -> Value {
    json!({
        "diff": diff,
        "extra": extra
    })
}

/// Called after each one-shot `run_session` in `cmd_run`. Collects signals,
/// classifies risk, auto-applies Low-risk improvements, queues High-risk ones.
/// No-op when `cfg.self_improvement.enabled` is false.
pub fn post_run_hook(cfg: &Config, repo_root: &Path, goose_exit_code: i32) -> Result<()> {
    if !cfg.self_improvement.enabled {
        return Ok(());
    }

    let changed_files = git_changed_files(repo_root);
    let clippy_output = run_clippy_capture(repo_root);

    let signals = crate::signals::collect_signals(
        repo_root, &clippy_output, &changed_files, goose_exit_code,
    );
    if signals.signals.is_empty() {
        return Ok(());
    }

    let candidates = crate::risk::classify(&signals, &cfg.self_improvement, &changed_files);
    if candidates.is_empty() {
        return Ok(());
    }

    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let mut queue = crate::pending::PendingQueue::load(&pending_path)?;
    let executor = crate::improve::ImprovementExecutor::new(cfg, repo_root);
    let mut auto_count = 0usize;

    for candidate in candidates {
        let is_high = candidate.risk == crate::risk::RiskLevel::High;
        if is_high || !cfg.self_improvement.auto_apply_low_risk {
            tracing::info!(id = %candidate.id, "queuing improvement: {}", candidate.description);
            queue.push(candidate)?;
        } else {
            tracing::info!(id = %candidate.id, "attempting auto-improvement: {}", candidate.description);
            match executor.run(&candidate, &mut auto_count)? {
                crate::improve::Outcome::Applied { branch } => {
                    tracing::info!("self-improvement applied on branch '{branch}'");
                }
                crate::improve::Outcome::Failed { reason }
                | crate::improve::Outcome::Skipped { reason } => {
                    tracing::warn!("self-improvement did not apply ({reason}); queuing");
                    queue.push(candidate)?;
                }
            }
        }
    }

    queue.save()
}

fn git_changed_files(repo_root: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(repo_root)
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

fn run_clippy_capture(repo_root: &Path) -> String {
    let Ok(out) = std::process::Command::new("cargo")
        .args(["clippy", "--message-format=short"])
        .current_dir(repo_root)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
    else {
        return String::new();
    };
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[cfg(test)]
mod hook_tests {
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
                .status()
                .unwrap();
        }
    }

    #[test]
    fn hook_is_noop_when_self_improvement_disabled() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let mut cfg = Config::default();
        cfg.self_improvement.enabled = false;
        assert!(post_run_hook(&cfg, dir.path(), 0).is_ok());
        assert!(!dir.path().join(".codi/pending_improvements.json").exists());
    }

    #[test]
    fn hook_is_noop_when_exit_code_zero_and_no_changes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        // No changed files, exit_code=0, no clippy warnings → no signals
        assert!(post_run_hook(&cfg, dir.path(), 0).is_ok());
    }
}
