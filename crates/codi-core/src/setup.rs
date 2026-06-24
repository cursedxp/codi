//! First-launch setup wizard and model picker.
//!
//! Called automatically when no `codi.toml` exists, or explicitly via
//! `codi model` / `codi model set <name>`.

use std::io::{self, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::ollama::{is_running, list_models};

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
    let picked = pick_model_interactive(&base_url, "Select a model to use:")?;

    let mut cfg = Config::default();
    cfg.model.local.base_url = picked.base_url.clone();
    cfg.model.local.model = picked.model.clone();

    let toml_path = repo_root.join("codi.toml");
    let toml_str = cfg.to_toml().context("serializing config")?;
    std::fs::write(&toml_path, &toml_str)
        .with_context(|| format!("writing {}", toml_path.display()))?;

    println!(
        "\n✓ Config written to {}",
        toml_path.display()
    );
    println!("  Model : {}", picked.model);
    println!("  Endpoint: {}\n", picked.base_url);

    Ok(cfg)
}

/// Update the model in an existing `codi.toml` (or the default config path).
/// If `model_name` is `None`, runs an interactive picker first.
pub fn set_model(repo_root: &Path, model_name: Option<&str>) -> Result<()> {
    // Load existing config so we don't blow away other settings.
    let mut cfg = Config::load(repo_root).unwrap_or_default();
    let base_url = cfg.model.local.base_url.clone();

    let model = match model_name {
        Some(name) => name.to_string(),
        None => {
            let picked = pick_model_interactive(&base_url, "Choose a new model:")?;
            picked.model
        }
    };

    cfg.model.local.model = model.clone();

    let toml_path = repo_root.join("codi.toml");
    let toml_str = cfg.to_toml().context("serializing config")?;
    std::fs::write(&toml_path, &toml_str)
        .with_context(|| format!("writing {}", toml_path.display()))?;

    println!("✓ Model updated → {model}  ({})", toml_path.display());
    Ok(())
}

/// Print available models without changing anything.
pub fn list_available_models(base_url: &str) -> Result<()> {
    if !is_running(base_url) {
        bail!("Ollama is not running at {base_url}. Start it with: ollama serve");
    }
    let models = list_models(base_url)?;
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
pub fn check_model(base_url: &str, model: &str) {
    use crate::ollama::check_tool_calls;
    print!("Checking tool-call support for {model} … ");
    io::stdout().flush().ok();
    if check_tool_calls(base_url, model) {
        println!("✓  works with codi/Goose");
    } else {
        println!("✗  returns text (not structured tool_calls) — will not work");
        println!("   Try: qwen2.5:7b, llama3.1:8b, mistral:7b");
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Detect Ollama at the default or configured URL. Returns the base_url.
fn detect_ollama() -> Result<String> {
    let candidates = [
        "http://localhost:11434/v1",
        "http://127.0.0.1:11434/v1",
    ];
    for url in candidates {
        if is_running(url) {
            println!("✓ Ollama detected at {url}");
            return Ok(url.to_string());
        }
    }
    bail!(
        "Ollama not found. Start it with:\n\n  ollama serve\n\n\
         Then re-run codi, or set [model.local] base_url manually in codi.toml."
    )
}

/// Show an interactive numbered list and return the user's choice.
pub fn pick_model_interactive(base_url: &str, prompt: &str) -> Result<PickResult> {
    if !is_running(base_url) {
        bail!("Ollama is not running at {base_url}");
    }

    let models = list_models(base_url)?;
    if models.is_empty() {
        bail!(
            "No models installed in Ollama.\n\
             Pull one first, e.g.:\n\n  ollama pull qwen2.5:7b\n"
        );
    }

    println!("\n{prompt}");
    println!("{}", "─".repeat(58));
    println!(
        "  {:<4} {:<30} {:>6}  {}",
        "#", "Model", "Size", "Tools  ★=coding"
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
            "q" | "Q" => bail!("Cancelled."),
            "c" | "C" => {
                print!("Model name to check: ");
                io::stdout().flush()?;
                let mut name = String::new();
                io::stdin().read_line(&mut name)?;
                check_model(base_url, name.trim());
                // Re-show the list
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
                        return Ok(PickResult {
                            model: chosen.name.clone(),
                            base_url: base_url.to_string(),
                        });
                    }
                }
                println!("  → Please enter a number between 1 and {}.", models.len());
            }
        }
    }
}

/// True if neither the repo-level nor the user-level config file exists.
pub fn is_first_launch(repo_root: &Path) -> bool {
    let repo_cfg = repo_root.join("codi.toml");
    let user_cfg = crate::config::user_config_path();
    !repo_cfg.exists() && user_cfg.map(|p| !p.exists()).unwrap_or(true)
}
