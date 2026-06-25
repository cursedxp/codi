use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use codi_core::{
    config::Config,
    doctor::{print_doctor_report, run_doctor, run_doctor_fix},
    engine::{pick_provider_label, post_run_hook},
    init::run_init,
    mcp,
    reliability::{run_reliable_session, RunContext},
    review::run_review,
    setup::{check_model, is_first_launch, list_available_models, set_model},
};

#[derive(Parser)]
#[command(
    name = "codi",
    about = "Local-first AI coding agent powered by a small local LLM",
    version
)]
struct Cli {
    /// Repository root (defaults to current directory).
    #[arg(long, global = true)]
    repo: Option<PathBuf>,

    /// Skip all confirmation prompts.
    #[arg(long, short = 'y', global = true)]
    yes: bool,

    /// Deprecated: accepted but ignored for backwards compatibility.
    #[arg(long, global = true, hide = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a single task and exit (non-interactive).
    Run {
        /// The task to perform, in natural language.
        task: String,
        /// Print self-review of the diff after completion.
        #[arg(long)]
        review: bool,
    },
    /// Index the repository for RAG context retrieval.
    Index {
        /// Rebuild the index from scratch.
        #[arg(long)]
        rebuild: bool,
    },
    /// Self-review recent changes via `git diff`.
    Review {
        /// Also apply the model's own suggestions.
        #[arg(long)]
        refine: bool,
    },
    /// Show the resolved configuration (merged repo + user level).
    Config,
    /// View or change the active model.
    Model {
        #[command(subcommand)]
        action: Option<ModelCmd>,
    },
    /// Start the MCP stdio server (used by Claude Code and other MCP clients).
    Mcp,
    /// Run project health checks (and optionally auto-fix safe issues).
    Doctor {
        /// Auto-fix safe issues (e.g. missing .mcp.json).
        #[arg(long)]
        fix: bool,
    },
    /// Set up this project for use with codi (one-time or re-run safely).
    Init {
        /// Overwrite codi.toml from scratch instead of merging.
        #[arg(long)]
        rewrite_config: bool,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// List all models installed in Ollama with compatibility info.
    List,
    /// Interactively pick a model (same as `codi model` with no subcommand).
    Pick,
    /// Set the model directly without the interactive picker.
    Set {
        /// Model name, e.g. qwen2.5:14b
        name: String,
    },
    /// Check whether a model supports structured tool_calls.
    Check {
        /// Model name to test.
        name: String,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_target(false)
        .init();

    let cli = Cli::parse();

    let repo_root = cli
        .repo
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let repo_root = repo_root.canonicalize().context("resolving repo root")?;

    // ── First-launch: no config → print onboarding prompt and exit ──────────
    if is_first_launch(&repo_root) {
        let skip = matches!(
            &cli.command,
            Some(Cmd::Model { .. }) | Some(Cmd::Mcp) | Some(Cmd::Doctor { .. }) | Some(Cmd::Init { .. })
        );
        if !skip {
            println!("Bu proje henüz yapılandırılmamış. Başlamak için:\n\n  codi init\n");
            return Ok(());
        }
    }

    let mut cfg = Config::load(&repo_root)?;
    if cli.yes {
        cfg.safety.confirm_commands = false;
        cfg.safety.confirm_writes = false;
    }

    match cli.command {
        None => {
            run_interactive(&cfg, &repo_root)?;
        }
        Some(Cmd::Run { task, review }) => {
            cmd_run(&cfg, &repo_root, &task, review)?;
        }
        Some(Cmd::Index { rebuild }) => {
            cmd_index(&cfg, &repo_root, rebuild)?;
        }
        Some(Cmd::Review { refine }) => {
            cmd_review(&cfg, &repo_root, refine)?;
        }
        Some(Cmd::Config) => {
            cmd_show_config(&cfg)?;
        }
        Some(Cmd::Model { action }) => {
            cmd_model(&cfg, &repo_root, action)?;
        }
        Some(Cmd::Mcp) => {
            // Skip the first-launch wizard check — MCP mode must be non-interactive.
            mcp::serve(&cfg, &repo_root)?;
        }
        Some(Cmd::Doctor { fix }) => {
            cmd_doctor(&cfg, &repo_root, fix)?;
        }
        Some(Cmd::Init { rewrite_config }) => {
            run_init(&repo_root, rewrite_config)?;
        }
    }

    Ok(())
}

// ── Subcommand implementations ───────────────────────────────────────────────

fn run_interactive(cfg: &Config, repo_root: &std::path::Path) -> Result<()> {
    println!(
        "codi {} — {}",
        env!("CARGO_PKG_VERSION"),
        pick_provider_label(cfg, "")
    );
    println!("Type your task and press Enter. Ctrl-C to exit.\n");

    let mut rl = rustyline::DefaultEditor::new().context("init readline")?;
    loop {
        let line = match rl.readline("codi> ") {
            Ok(l) => l,
            Err(rustyline::error::ReadlineError::Interrupted)
            | Err(rustyline::error::ReadlineError::Eof) => break,
            Err(e) => return Err(e.into()),
        };
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        let _ = rl.add_history_entry(task);
        let outcome = run_reliable_session(cfg, task, repo_root, RunContext::Cli)?;
        if !outcome.success {
            eprintln!("task failed ({})", outcome.decision_reason);
        }
    }
    Ok(())
}

fn cmd_run(cfg: &Config, repo_root: &std::path::Path, task: &str, review: bool) -> Result<()> {
    println!("Provider: {}", pick_provider_label(cfg, task));
    let outcome = run_reliable_session(cfg, task, repo_root, RunContext::Cli)?;
    if !outcome.success {
        eprintln!(
            "task failed (exit={}, mode={}, steps={}/{}, reason={})",
            outcome.exit_code, outcome.execution_mode,
            outcome.steps_succeeded, outcome.steps_total,
            outcome.decision_reason,
        );
    }
    let code = outcome.exit_code;
    if review {
        println!("\n--- Self-review ---");
        let result = run_review(cfg, repo_root, false)?;
        if result.exit_code != 0 {
            eprintln!("review exited with code {}", result.exit_code);
            std::process::exit(result.exit_code);
        }
    }
    // Post-run hook: signal collection and self-improvement (non-fatal).
    if let Err(e) = post_run_hook(cfg, repo_root, code) {
        tracing::warn!("self-improvement hook failed: {e:#}");
    }
    Ok(())
}

fn cmd_index(cfg: &Config, repo_root: &std::path::Path, rebuild: bool) -> Result<()> {
    let db = repo_root.join(&cfg.rag.db_path);
    if rebuild && db.exists() {
        std::fs::remove_file(&db).context("removing old index")?;
    }
    println!("Indexing {} → {} (RAG not yet wired in M3+)", repo_root.display(), db.display());
    Ok(())
}

fn cmd_review(cfg: &Config, repo_root: &std::path::Path, refine: bool) -> Result<()> {
    let result = run_review(cfg, repo_root, refine)?;
    if result.diff.trim().is_empty() {
        println!("No changes to review (git diff is empty).");
    } else {
        println!("Review complete (exit code {}).", result.exit_code);
    }
    Ok(())
}

fn cmd_show_config(cfg: &Config) -> Result<()> {
    println!("{}", cfg.to_toml().context("serializing config")?);
    Ok(())
}

fn cmd_model(
    cfg: &Config,
    repo_root: &std::path::Path,
    action: Option<ModelCmd>,
) -> Result<()> {
    let base_url = &cfg.model.local.base_url;

    match action {
        // `codi model` or `codi model pick` → interactive picker
        None | Some(ModelCmd::Pick) => {
            println!("Current model: {}\n", cfg.model.local.model);
            set_model(repo_root, None)?;
        }
        Some(ModelCmd::List) => {
            list_available_models(base_url)?;
        }
        Some(ModelCmd::Set { name }) => {
            set_model(repo_root, Some(&name))?;
        }
        Some(ModelCmd::Check { name }) => {
            check_model(base_url, &name)?;
        }
    }
    Ok(())
}

fn cmd_doctor(cfg: &Config, repo_root: &std::path::Path, fix: bool) -> Result<()> {
    let checks = if fix {
        run_doctor_fix(repo_root, cfg)?
    } else {
        run_doctor(repo_root, cfg)?
    };
    let has_errors = print_doctor_report(&checks);
    if has_errors {
        std::process::exit(1);
    }
    Ok(())
}
