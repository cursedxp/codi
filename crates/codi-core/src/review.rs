//! Self-review: run `git diff`, build a structured reviewer prompt, and invoke
//! Goose (local model) for a one-shot review session.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::engine::{run_session, SessionMode};

/// Output from a self-review run.
#[derive(Debug)]
pub struct ReviewResult {
    /// The raw git diff that was reviewed.
    pub diff: String,
    /// Exit code from the Goose review session.
    pub exit_code: i32,
}

/// Capture `git diff` in `repo_root` and feed it to a local Goose one-shot
/// session that reviews the changes.
///
/// `auto_refine`: if true, a second pass asks the model to apply its own
/// suggestions (--refine mode). Not implemented in the prototype — placeholder.
pub fn run_review(cfg: &Config, repo_root: &Path, auto_refine: bool) -> Result<ReviewResult> {
    let diff = git_diff(repo_root)?;

    if diff.trim().is_empty() {
        tracing::info!("no git diff found — nothing to review");
        return Ok(ReviewResult {
            diff,
            exit_code: 0,
        });
    }

    let prompt = build_review_prompt(&diff, auto_refine);
    let code = run_session(
        cfg,
        &prompt,
        SessionMode::OneShot(prompt.clone()),
        None,
        repo_root,
        "",
    )?;

    Ok(ReviewResult {
        diff,
        exit_code: code,
    })
}

fn git_diff(repo_root: &Path) -> Result<String> {
    let out = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(repo_root)
        .output()
        .context("running git diff")?;

    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn build_review_prompt(diff: &str, refine: bool) -> String {
    let mode = if refine {
        "After your review, output a unified diff of any fixes as a code block."
    } else {
        "Provide a written review only — do not modify any files."
    };

    format!(
        "You are a senior engineer performing a self-review of the following code change.\n\
         {mode}\n\
         Review dimensions: correctness, architecture, style, test coverage, potential bugs, \
         security concerns, and any areas that need follow-up.\n\n\
         ```diff\n{diff}\n```"
    )
}
