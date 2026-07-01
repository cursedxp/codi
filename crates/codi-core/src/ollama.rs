//! Ollama discovery: list installed models and verify tool-call support.

use anyhow::{Context, Result};
use serde::Deserialize;

/// A model returned by Ollama's /api/tags endpoint.
#[derive(Debug, Clone)]
pub struct OllamaModel {
    pub name: String,
    pub size_gb: f64,
    /// None = not yet checked, Some(true/false) = checked.
    pub tool_calls: Option<bool>,
    /// Whether this model is in our known-good list for coding tasks.
    pub known_coding: bool,
}

impl OllamaModel {
    /// Short display label, e.g. "qwen2.5:7b  (4.7 GB) ✓ tools".
    pub fn label(&self) -> String {
        let tool_icon = match self.tool_calls {
            Some(true) => "✓ tools",
            Some(false) => "✗ tools",
            None => "? tools",
        };
        let coding = if self.known_coding { " ★" } else { "" };
        format!(
            "{:<30} {:>6.1} GB  {}{}",
            self.name, self.size_gb, tool_icon, coding
        )
    }
}

// ---------------------------------------------------------------------------
// Known good models
// ---------------------------------------------------------------------------

/// Models known to work well with Goose tool calling.
/// Listed in roughly descending order of coding capability.
const KNOWN_CODING_MODELS: &[&str] = &[
    "qwen2.5:72b",
    "qwen2.5:32b",
    "qwen2.5:14b",
    "qwen2.5:7b",
    "qwen2.5:3b",
    "llama3.3:70b",
    "llama3.1:70b",
    "llama3.1:8b",
    "llama3.2:3b",
    "codestral:22b",
    "deepseek-coder-v2:16b",
    "mistral:7b",
    "mistral-nemo:latest",
    "phi4:14b",
    "phi4-mini:3.8b",
    "gemma3:27b",
    "gemma3:9b",
];

fn is_known_coding(name: &str) -> bool {
    let lower = name.to_lowercase();
    // Match exact name (e.g. "qwen2.5:7b") or dash-variant (e.g. "qwen2.5:7b-q4_K_M").
    KNOWN_CODING_MODELS
        .iter()
        .any(|k| lower == *k || lower.starts_with(&format!("{k}-")))
        || lower.contains("coder")
        || lower.contains("code")
        || lower.contains("codestral")
        || lower.contains("deepseek")
        || lower.contains("qwen")
        || lower.contains("llama")
        || lower.contains("mistral")
        || lower.contains("phi")
        || lower.contains("gemma")
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

/// Strip trailing `/v1` and slashes so we can append `/api/tags` on the root.
fn ollama_root(base_url: &str) -> String {
    base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .to_string()
}

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    name: String,
    size: u64,
}

/// Return true if Ollama is reachable at `base_url`.
pub fn is_running(base_url: &str) -> bool {
    let url = format!("{}/api/tags", ollama_root(base_url));
    reqwest::blocking::get(&url)
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Tool support read from `/api/show` capabilities — instant metadata, no
/// model load. Returns None when this Ollama version doesn't report
/// capabilities (pre-0.6), so the caller can fall back to an inference probe.
fn capabilities_tool_support(base_url: &str, model: &str) -> Option<bool> {
    let url = format!("{}/api/show", ollama_root(base_url));
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"model": model}))
        .send()
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let json: serde_json::Value = resp.json().ok()?;
    let caps = json.get("capabilities")?.as_array()?;
    Some(caps.iter().any(|c| c.as_str() == Some("tools")))
}

/// List all models installed in Ollama, keeping only those with tool-call
/// support. Support is read from `/api/show` capabilities (instant); only when
/// that's unavailable does it fall back to a live inference probe. The probe
/// loads the model into memory — probing N models in parallel used to thrash
/// Ollama into 15s timeouts and report ZERO tool-capable models.
pub fn list_models(base_url: &str) -> Result<Vec<OllamaModel>> {
    let url = format!("{}/api/tags", ollama_root(base_url));

    let resp: TagsResponse = reqwest::blocking::get(&url)
        .context("connecting to Ollama")?
        .json()
        .context("parsing Ollama model list")?;

    let base_url = base_url.to_string();
    let raw: Vec<TagModel> = resp.models;

    // Run tool-call checks in parallel — one thread per model.
    let handles: Vec<_> = raw
        .into_iter()
        .map(|m| {
            let bu = base_url.clone();
            std::thread::spawn(move || {
                let supports = capabilities_tool_support(&bu, &m.name)
                    .unwrap_or_else(|| check_tool_calls(&bu, &m.name));
                OllamaModel {
                    known_coding: is_known_coding(&m.name),
                    name: m.name,
                    size_gb: m.size as f64 / 1e9,
                    tool_calls: Some(supports),
                }
            })
        })
        .collect();

    let mut models: Vec<OllamaModel> = handles
        .into_iter()
        .filter_map(|h| h.join().ok())
        .filter(|m| m.tool_calls == Some(true))
        .collect();

    // Sort: known coding models first, then by size descending.
    models.sort_by(|a, b| {
        b.known_coding
            .cmp(&a.known_coding)
            .then(b.size_gb.partial_cmp(&a.size_gb).unwrap_or(std::cmp::Ordering::Equal))
    });

    Ok(models)
}

/// Like `check_tool_calls` but returns the underlying error so callers can
/// distinguish "Ollama unreachable" from "model doesn't support tool_calls".
pub fn check_tool_calls_result(base_url: &str, model: &str) -> Result<bool> {
    check_tool_calls_inner(base_url, model)
}

/// Send a minimal tool-calling request and check if the model returns a
/// structured `tool_calls` field (vs. plain text). Fast: small prompt.
pub fn check_tool_calls(base_url: &str, model: &str) -> bool {
    match check_tool_calls_inner(base_url, model) {
        Ok(result) => result,
        Err(e) => {
            tracing::debug!("tool-call check failed for {model}: {e}");
            false
        }
    }
}

fn check_tool_calls_inner(base_url: &str, model: &str) -> anyhow::Result<bool> {
    // base_url is like "http://localhost:11434/v1" → append /chat/completions
    let url = format!("{}/chat/completions", base_url.trim_end_matches('/'));
    let body = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": "Call the ping tool."}],
        "tools": [{
            "type": "function",
            "function": {
                "name": "ping",
                "description": "Respond to a ping.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "message": {"type": "string"}
                    },
                    "required": ["message"]
                }
            }
        }]
    });

    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let resp = client
        .post(&url)
        .header("Authorization", "Bearer ollama")
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .with_context(|| format!("POST {url}"))?;

    let status = resp.status();
    let text = resp.text().context("reading response body")?;

    if !status.is_success() {
        anyhow::bail!("HTTP {status}: {text}");
    }

    let json: serde_json::Value =
        serde_json::from_str(&text).context("parsing JSON response")?;

    let has_tool_calls = json["choices"][0]["message"]["tool_calls"]
        .as_array()
        .map(|a| !a.is_empty())
        .unwrap_or(false);

    tracing::debug!(
        model,
        url,
        has_tool_calls,
        finish_reason = json["choices"][0]["finish_reason"].as_str().unwrap_or("?"),
        "tool-call check"
    );

    Ok(has_tool_calls)
}

/// Verify tool-call support for each model in the list and return a new list
/// with `tool_calls` populated. Skips models larger than `max_size_gb` to
/// avoid slow inference on machines with limited RAM.
pub fn verify_tool_calls(
    base_url: &str,
    models: Vec<OllamaModel>,
    max_size_gb: f64,
) -> Vec<OllamaModel> {
    models
        .into_iter()
        .map(|mut m| {
            if m.size_gb <= max_size_gb {
                m.tool_calls = Some(check_tool_calls(base_url, &m.name));
            }
            m
        })
        .collect()
}
