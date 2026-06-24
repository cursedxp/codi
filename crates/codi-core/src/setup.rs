//! First-launch setup wizard and model picker.
//!
//! Called automatically when no `codi.toml` exists, or explicitly via
//! `codi model` / `codi model set <name>`.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use toml::Value;

use crate::config::Config;
use crate::ollama::{check_tool_calls_result, is_running, list_models};

/// Result of the model picker: the chosen model name.
pub struct PickResult {
    pub model: String,
    pub base_url: String,
}

/// Run the first-launch wizard: detect Ollama, list models, let the user pick,
/// write `codi.toml` in `repo_root`. Returns the resulting config.
pub fn first_launch_wizard(repo_root: &Path) -> Result<Config> {
    println!("\n┌─────────────────────────────────────────┐");
    println!("│  codi — first launch setup               │");
    println!("└─────────────────────────────────────────┘\n");

    let base_url = detect_ollama()?;
    let picked = match pick_model_interactive(&base_url, "Select a model to use:")? {
        Some(p) => p,
        None => {
            println!("Setup cancelled. Run 'codi model' when you're ready to choose a model.");
            bail!("setup cancelled by user");
        }
    };

    let mut cfg = Config::default();
    cfg.model.local.base_url = picked.base_url.clone();
    cfg.model.local.model = picked.model.clone();

    let toml_path = repo_root.join("codi.toml");
    let toml_str = cfg.to_toml().context("serializing config")?;
    std::fs::write(&toml_path, &toml_str)
        .with_context(|| format!("writing {}", toml_path.display()))?;

    println!("\n✓ Config written to {}", toml_path.display());
    println!("  Model : {}", picked.model);
    println!("  Endpoint: {}\n", picked.base_url);

    Ok(cfg)
}

/// Update the model in an existing `codi.toml`.  Only the `[model.local].model`
/// key is touched — all other settings in the repo file are preserved, and
/// user-level config is never serialised into the repo file.
/// If `model_name` is `None`, runs an interactive picker first.
pub fn set_model(repo_root: &Path, model_name: Option<&str>) -> Result<()> {
    let toml_path = repo_root.join("codi.toml");

    // Read only the repo-level file so we never leak user-level settings.
    let existing = if toml_path.exists() {
        std::fs::read_to_string(&toml_path)
            .with_context(|| format!("reading {}", toml_path.display()))?
    } else {
        String::new()
    };

    let mut doc: Value = if existing.trim().is_empty() {
        Value::Table(toml::Table::new())
    } else {
        toml::from_str(&existing).context("parsing codi.toml")?
    };

    // Derive base_url from the repo file (fall back to Ollama default).
    let base_url = doc
        .get("model")
        .and_then(|m| m.get("local"))
        .and_then(|l| l.get("base_url"))
        .and_then(|v| v.as_str())
        .unwrap_or("http://localhost:11434/v1")
        .to_string();

    let model = match model_name {
        Some(name) => name.to_string(),
        None => {
            match pick_model_interactive(&base_url, "Choose a new model:")? {
                Some(p) => p.model,
                None => return Ok(()), // user pressed q — exit cleanly
            }
        }
    };

    // Surgically update only [model.local].model.
    {
        let table = doc
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("codi.toml is not a TOML table"))?;
        let model_tbl = table
            .entry("model".to_string())
            .or_insert(Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("[model] is not a table"))?;
        let local_tbl = model_tbl
            .entry("local".to_string())
            .or_insert(Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or_else(|| anyhow::anyhow!("[model.local] is not a table"))?;
        local_tbl.insert("model".to_string(), Value::String(model.clone()));
    }

    let toml_str = toml::to_string_pretty(&doc).context("serializing config")?;
    std::fs::write(&toml_path, &toml_str)
        .with_context(|| format!("writing {}", toml_path.display()))?;

    println!("✓ Model updated → {model}  ({})", toml_path.display());
    Ok(())
}

/// Print available models without changing anything.
pub fn list_available_models(base_url: &str) -> Result<()> {
    // Single round-trip to /api/tags via list_models.
    let models = list_models(base_url)
        .with_context(|| format!("Ollama is not running at {base_url}. Start it with: ollama serve"))?;
    if models.is_empty() {
        println!("No models installed. Pull one with: ollama pull qwen2.5:7b");
        return Ok(());
    }

    println!("\nInstalled models  (★ = known coding model)");
    println!("{}", "─".repeat(58));
    for m in &models {
        println!("  {}", m.label());
    }
    println!();
    println!("Tip: check tool-call support with:");
    println!("  codi model check <name>");
    Ok(())
}

/// Run a one-off tool-call compatibility check for a single model.
/// Returns an error if Ollama is unreachable, distinguishing network failures
/// from genuine model incompatibility.
pub fn check_model(base_url: &str, model: &str) -> Result<()> {
    print!("Checking tool-call support for {model} … ");
    io::stdout().flush()?;
    match check_tool_calls_result(base_url, model)
        .with_context(|| format!("connecting to Ollama at {base_url}"))?
    {
        true => println!("✓  works with codi/Goose"),
        false => {
            println!("✗  returns text (not structured tool_calls) — will not work");
            println!("   Try: qwen2.5:7b, llama3.1:8b, mistral:7b");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Detect Ollama at the default or configured URL. Returns the base_url.
pub(crate) fn detect_ollama() -> Result<String> {
    let candidates = [
        "http://localhost:11434/v1",
        "http://127.0.0.1:11434/v1",
    ];
    for url in candidates {
        if is_running(url) {
            return Ok(url.to_string());
        }
    }
    bail!(
        "Ollama not found. Start it with:\n\n  ollama serve\n\n\
         Then re-run codi, or set [model.local] base_url manually in codi.toml."
    )
}

/// Show an interactive numbered list and return the user's choice.
/// Returns `Ok(None)` if the user pressed 'q' to cancel — not an error.
pub fn pick_model_interactive(base_url: &str, prompt: &str) -> Result<Option<PickResult>> {
    // Single round-trip: list_models already checks connectivity.
    let models = list_models(base_url)
        .with_context(|| format!("Ollama is not running at {base_url}. Start it with: ollama serve"))?;
    if models.is_empty() {
        bail!(
            "No models installed in Ollama.\n\
             Pull one first, e.g.:\n\n  ollama pull qwen2.5:7b\n"
        );
    }

    println!("\n{prompt}");
    println!("{}", "─".repeat(58));
    println!(
        "  {:<4} {:<30} {:>6}  Tools  ★=coding",
        "#", "Model", "Size"
    );
    println!("{}", "─".repeat(58));
    for (i, m) in models.iter().enumerate() {
        println!("  [{:>2}] {}", i + 1, m.label());
    }
    println!("{}", "─".repeat(58));
    println!("  [ c] Check tool-call support for a model");
    println!("  [ q] Quit without changing anything");
    println!();

    loop {
        print!("Enter number (or c/q): ");
        io::stdout().flush()?;

        let mut line = String::new();
        io::stdin().read_line(&mut line)?;
        let trimmed = line.trim();

        match trimmed {
            "q" | "Q" => {
                println!("Cancelled.");
                return Ok(None);
            }
            "c" | "C" => {
                print!("Model name to check: ");
                io::stdout().flush()?;
                let mut name = String::new();
                io::stdin().read_line(&mut name)?;
                if let Err(e) = check_model(base_url, name.trim()) {
                    eprintln!("Error: {e}");
                }
                println!();
                for (i, m) in models.iter().enumerate() {
                    println!("  [{:>2}] {}", i + 1, m.label());
                }
                println!();
            }
            s => {
                if let Ok(n) = s.parse::<usize>() {
                    if n >= 1 && n <= models.len() {
                        let chosen = &models[n - 1];
                        return Ok(Some(PickResult {
                            model: chosen.name.clone(),
                            base_url: base_url.to_string(),
                        }));
                    }
                }
                println!("  → Please enter a number between 1 and {}.", models.len());
            }
        }
    }
}

/// True if neither the repo-level nor the user-level config file exists.
/// Returns `false` when the home directory is unavailable (CI, containers)
/// to avoid blocking the wizard on stdin in headless environments.
pub fn is_first_launch(repo_root: &Path) -> bool {
    let repo_cfg = repo_root.join("codi.toml");
    let user_cfg = crate::config::user_config_path();
    !repo_cfg.exists() && user_cfg.map(|p| !p.exists()).unwrap_or(false)
}
