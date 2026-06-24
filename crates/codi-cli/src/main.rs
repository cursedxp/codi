use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use codi_core::{
    config::Config,
    engine::{pick_provider_label, run_session, SessionMode},
    review::run_review,
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

    /// Config file (defaults to ./codi.toml then ~/.config/codi/config.toml).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Skip all confirmation prompts.
    #[arg(long, short = 'y', global = true)]
    yes: bool,

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

    let mut cfg = Config::load(&repo_root).context("loading codi config")?;
    if cli.yes {
        cfg.safety.confirm_commands = false;
        cfg.safety.confirm_writes = false;
    }

    match cli.command {
        None => {
            // Default: interactive REPL.
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
    }

    Ok(())
}

fn run_interactive(cfg: &Config, repo_root: &std::path::Path) -> Result<()> {
    println!(
        "codi {} — {}",
        env!("CARGO_PKG_VERSION"),
        pick_provider_label(cfg, "")
    );
    println!("Type your task and press Enter. Ctrl-C to exit.\n");

    // Use rustyline for a basic REPL with history.
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

        // Launch a one-shot Goose session for this task; loop for next task.
        let code = run_session(cfg, task, SessionMode::OneShot(task.to_string()), None, repo_root, "")?;
        if code != 0 {
            eprintln!("goose exited with code {code}");
        }
    }
    Ok(())
}

fn cmd_run(cfg: &Config, repo_root: &std::path::Path, task: &str, review: bool) -> Result<()> {
    println!("Provider: {}", pick_provider_label(cfg, task));

    // TODO M3: call codi-rag to retrieve snippets and pass as context_snippets
    let context_snippets = "";

    let code = run_session(
        cfg,
        task,
        SessionMode::OneShot(task.to_string()),
        None,
        repo_root,
        context_snippets,
    )?;

    if code != 0 {
        eprintln!("goose exited with code {code}");
    }

    if review {
        println!("\n--- Self-review ---");
        let result = run_review(cfg, repo_root, false)?;
        println!("Review exit code: {}", result.exit_code);
    }

    Ok(())
}

fn cmd_index(cfg: &Config, repo_root: &std::path::Path, rebuild: bool) -> Result<()> {
    // TODO M3: call codi-rag to index the repo.
    let db = repo_root.join(&cfg.rag.db_path);
    if rebuild && db.exists() {
        std::fs::remove_file(&db).context("removing old index")?;
    }
    println!("Indexing {} → {} (BM25)", repo_root.display(), db.display());
    println!("(RAG indexer not yet wired — coming in M3)");
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
    let s = cfg.to_toml().context("serializing config")?;
    println!("{s}");
    Ok(())
}
