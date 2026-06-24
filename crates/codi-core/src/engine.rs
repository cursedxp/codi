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
        Provider::Local(m) => format!(
            "GOOSE_PROVIDER: openai\nGOOSE_OPENAI_HOST: {}\nGOOSE_MODEL: {}\nGOOSE_API_KEY: {}\n",
            m.base_url, m.model, m.api_key
        ),
        Provider::Cloud(m) => {
            let api_key_val = std::env::var(&m.api_key_env).unwrap_or_default();
            match m.provider.as_str() {
                "anthropic" => format!(
                    "GOOSE_PROVIDER: anthropic\nGOOSE_MODEL: {}\nANTHROPIC_API_KEY: {}\n",
                    m.model, api_key_val
                ),
                _ => format!(
                    "GOOSE_PROVIDER: openai\nGOOSE_MODEL: {}\nGOOSE_API_KEY: {}\n",
                    m.model, api_key_val
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
            cmd.stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
    }
    cmd
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
