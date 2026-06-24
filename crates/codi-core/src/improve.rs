//! Auto-improvement executor: branch → Goose → test/lint gate → commit or rollback.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::risk::ImprovementCandidate;
use crate::signals::Signal;

#[derive(Debug)]
pub enum Outcome {
    Applied { branch: String },
    Failed { reason: String },
    Skipped { reason: String },
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub description: String,
    pub risk: String,
    pub branch: String,
    pub outcome: String,
    pub reason: Option<String>,
    pub approved_by_claude: bool,
    pub blocklist_bypassed: bool,
    pub source_signals: Vec<Signal>,
    pub created_at: u64,
    pub completed_at: u64,
}

pub struct ImprovementExecutor<'a> {
    pub cfg: &'a Config,
    pub repo_root: &'a Path,
}

impl<'a> ImprovementExecutor<'a> {
    pub fn new(cfg: &'a Config, repo_root: &'a Path) -> Self {
        ImprovementExecutor { cfg, repo_root }
    }

    /// Run a low-risk auto-improvement. Enforces blocklist, quota, and clean-state checks.
    /// Increments `auto_count` only on successful application.
    pub fn run(
        &self,
        candidate: &ImprovementCandidate,
        auto_count: &mut usize,
    ) -> Result<Outcome> {
        // Pre-check 1: quota
        if *auto_count >= self.cfg.self_improvement.max_auto_per_run {
            return Ok(Outcome::Skipped {
                reason: format!(
                    "max_auto_per_run ({}) reached",
                    self.cfg.self_improvement.max_auto_per_run
                ),
            });
        }

        // Pre-check 2: blocklist
        for blocked in &self.cfg.self_improvement.blocklist {
            if candidate.context.contains(blocked.as_str()) {
                return Ok(Outcome::Skipped {
                    reason: format!("context contains blocklist file '{blocked}'"),
                });
            }
        }

        // Pre-check 3: dirty git tree
        if !git_is_clean(self.repo_root)? {
            return Ok(Outcome::Skipped {
                reason: "git working tree is not clean".to_string(),
            });
        }

        // All pre-checks passed — execute and only increment count on successful application
        let branch = branch_name(candidate, &self.cfg.self_improvement.branch_prefix);
        let result = self.execute(candidate, &branch, false, false)?;

        if matches!(result, Outcome::Applied { .. }) {
            *auto_count += 1;
        }

        Ok(result)
    }

    /// Run a Claude-approved improvement. Skips blocklist and quota; test+lint gate still applies.
    pub fn run_approved(&self, candidate: &ImprovementCandidate) -> Result<Outcome> {
        let blocklist_bypassed = self.cfg.self_improvement.blocklist
            .iter()
            .any(|b| candidate.context.contains(b.as_str()));

        if !git_is_clean(self.repo_root)? {
            return Ok(Outcome::Failed {
                reason: "git working tree is dirty; cannot apply improvement".into(),
            });
        }

        let branch = branch_name(candidate, &self.cfg.self_improvement.branch_prefix);
        self.execute(candidate, &branch, true, blocklist_bypassed)
    }

    fn execute(
        &self,
        candidate: &ImprovementCandidate,
        branch: &str,
        approved_by_claude: bool,
        blocklist_bypassed: bool,
    ) -> Result<Outcome> {
        let original_branch = git_current_branch(self.repo_root)?;

        let now_secs = || {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
        };

        git_create_branch(self.repo_root, branch)?;

        let task = format!(
            "{}\n\nFiles to focus on: {}",
            candidate.description, candidate.context
        );
        crate::engine::run_session_mcp(self.cfg, &task, None, self.repo_root, "")?;

        // Post-session diff size check
        let shortstat = git_shortstat(self.repo_root)?;
        let diff_lines = crate::signals::parse_diff_line_count(&shortstat);
        if diff_lines > self.cfg.self_improvement.max_diff_lines {
            let reason = format!(
                "diff too large ({diff_lines} lines > max {}); rolled back",
                self.cfg.self_improvement.max_diff_lines
            );
            git_rollback(self.repo_root, &original_branch, branch)?;
            append_log(
                self.repo_root,
                &LogEntry {
                    id: candidate.id.clone(),
                    description: candidate.description.clone(),
                    risk: format!("{:?}", candidate.risk),
                    branch: branch.to_string(),
                    outcome: "Failed".to_string(),
                    reason: Some(reason.clone()),
                    approved_by_claude,
                    blocklist_bypassed,
                    source_signals: candidate.source_signals.clone(),
                    created_at: candidate.created_at,
                    completed_at: now_secs(),
                },
            )?;
            return Ok(Outcome::Failed { reason });
        }

        // Test gate
        let test_ok = run_test_gate(self.cfg, self.repo_root);
        if !test_ok {
            let reason = "test gate failed; rolled back".to_string();
            git_rollback(self.repo_root, &original_branch, branch)?;
            append_log(
                self.repo_root,
                &LogEntry {
                    id: candidate.id.clone(),
                    description: candidate.description.clone(),
                    risk: format!("{:?}", candidate.risk),
                    branch: branch.to_string(),
                    outcome: "Failed".to_string(),
                    reason: Some(reason.clone()),
                    approved_by_claude,
                    blocklist_bypassed,
                    source_signals: candidate.source_signals.clone(),
                    created_at: candidate.created_at,
                    completed_at: now_secs(),
                },
            )?;
            return Ok(Outcome::Failed { reason });
        }

        // Lint gate
        let lint_ok = run_lint_gate(self.repo_root);
        if !lint_ok {
            let reason =
                "lint gate failed (cargo clippy -D warnings); rolled back".to_string();
            git_rollback(self.repo_root, &original_branch, branch)?;
            append_log(
                self.repo_root,
                &LogEntry {
                    id: candidate.id.clone(),
                    description: candidate.description.clone(),
                    risk: format!("{:?}", candidate.risk),
                    branch: branch.to_string(),
                    outcome: "Failed".to_string(),
                    reason: Some(reason.clone()),
                    approved_by_claude,
                    blocklist_bypassed,
                    source_signals: candidate.source_signals.clone(),
                    created_at: candidate.created_at,
                    completed_at: now_secs(),
                },
            )?;
            return Ok(Outcome::Failed { reason });
        }

        git_commit(
            self.repo_root,
            &format!("self-improve: {} [auto]", candidate.description),
        )?;

        append_log(
            self.repo_root,
            &LogEntry {
                id: candidate.id.clone(),
                description: candidate.description.clone(),
                risk: format!("{:?}", candidate.risk),
                branch: branch.to_string(),
                outcome: "Applied".to_string(),
                reason: None,
                approved_by_claude,
                blocklist_bypassed,
                source_signals: candidate.source_signals.clone(),
                created_at: candidate.created_at,
                completed_at: now_secs(),
            },
        )?;

        Ok(Outcome::Applied {
            branch: branch.to_string(),
        })
    }
}

// ── Branch name ───────────────────────────────────────────────────────────────

/// Derive a git branch name for a candidate.
///
/// Format: `{prefix}/{id[..8]}-{slug}` where slug is the first 3 words of the
/// description, lowercased, non-alphanumeric chars replaced with `-`.
pub fn branch_name(candidate: &ImprovementCandidate, prefix: &str) -> String {
    let short_id = &candidate.id[..candidate.id.len().min(8)];
    let slug = slugify(&candidate.description, 3);
    format!("{prefix}/{short_id}-{slug}")
}

fn slugify(s: &str, max_words: usize) -> String {
    s.split_whitespace()
        .take(max_words)
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

// ── Git helpers ───────────────────────────────────────────────────────────────

fn git_is_clean(repo_root: &Path) -> Result<bool> {
    let out = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo_root)
        .output()
        .context("git status --porcelain")?;
    Ok(out.stdout.is_empty())
}

fn git_current_branch(repo_root: &Path) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(repo_root)
        .output()
        .context("git branch --show-current")?;
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git_create_branch(repo_root: &Path, branch: &str) -> Result<()> {
    let s = std::process::Command::new("git")
        .args(["checkout", "-b", branch])
        .current_dir(repo_root)
        .status()
        .context("git checkout -b")?;
    anyhow::ensure!(s.success(), "failed to create branch '{branch}'");
    Ok(())
}

fn git_rollback(repo_root: &Path, original: &str, improve: &str) -> Result<()> {
    // CRITICAL ORDER: checkout original first, then delete branch
    let status = std::process::Command::new("git")
        .args(["checkout", original])
        .current_dir(repo_root)
        .status()
        .context("git checkout (rollback)")?;
    anyhow::ensure!(status.success(), "git checkout {} failed during rollback", original);
    std::process::Command::new("git")
        .args(["branch", "-D", improve])
        .current_dir(repo_root)
        .status()
        .context("git branch -D (rollback)")?;
    Ok(())
}

fn git_commit(repo_root: &Path, message: &str) -> Result<()> {
    let add = std::process::Command::new("git")
        .args(["add", "-A"])
        .current_dir(repo_root)
        .status()
        .context("git add -A")?;
    anyhow::ensure!(add.success(), "git add -A failed");
    let commit = std::process::Command::new("git")
        .args(["commit", "-m", message])
        .current_dir(repo_root)
        .status()
        .context("git commit")?;
    anyhow::ensure!(commit.success(), "git commit failed (nothing staged?)");
    Ok(())
}

fn git_shortstat(repo_root: &Path) -> Result<String> {
    let out = std::process::Command::new("git")
        .args(["diff", "HEAD", "--shortstat"])
        .current_dir(repo_root)
        .output()
        .context("git diff HEAD --shortstat")?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

// ── Test + lint gates ─────────────────────────────────────────────────────────

fn run_test_gate(cfg: &Config, repo_root: &Path) -> bool {
    let Some(cmd) = &cfg.commands.test else {
        return false;
    };
    if cmd.is_empty() {
        return false;
    }
    let mut parts = cmd.split_whitespace();
    let Some(prog) = parts.next() else {
        return false;
    };
    std::process::Command::new(prog)
        .args(parts.collect::<Vec<_>>())
        .current_dir(repo_root)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_lint_gate(repo_root: &Path) -> bool {
    std::process::Command::new("cargo")
        .args(["clippy", "--", "-D", "warnings"])
        .current_dir(repo_root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ── Log writer ────────────────────────────────────────────────────────────────

/// Append one log entry to `{repo_root}/.codi/improvement_log.jsonl`.
/// Creates the file and parent directory if they do not exist.
/// Never truncates — always appends.
pub fn append_log(repo_root: &Path, entry: &LogEntry) -> Result<()> {
    let log_path = repo_root.join(".codi/improvement_log.jsonl");
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).context("creating .codi dir")?;
    }
    let mut line = serde_json::to_string(entry).context("serializing log entry")?;
    line.push('\n');
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?;
    file.write_all(line.as_bytes()).context("writing log entry")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::risk::{ImprovementCandidate, RiskLevel};
    use tempfile::tempdir;

    fn candidate(id: &str, context: &str) -> ImprovementCandidate {
        ImprovementCandidate {
            id: id.to_string(),
            description: "add a missing test".to_string(),
            risk: RiskLevel::Low,
            risk_reason: "lint only".to_string(),
            source_signals: vec![],
            context: context.to_string(),
            created_at: 0,
        }
    }

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
    fn branch_name_uses_prefix_id_and_slug() {
        let c = candidate("abc12345def", "add a missing test");
        let name = branch_name(&c, "improve");
        assert!(name.starts_with("improve/abc12345"));
        assert!(name.contains("add"));
    }

    #[test]
    fn pre_check_skips_when_blocklist_file_in_context() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default(); // blocklist contains routing.rs
        let c = candidate("x1", "crates/codi-core/src/routing.rs");
        let executor = ImprovementExecutor::new(&cfg, dir.path());
        let mut count = 0usize;
        let outcome = executor.run(&c, &mut count).unwrap();
        assert!(matches!(outcome, Outcome::Skipped { .. }));
        assert_eq!(count, 0);
    }

    #[test]
    fn pre_check_skips_when_quota_exceeded() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let mut cfg = Config::default();
        cfg.self_improvement.max_auto_per_run = 1;
        let c = candidate("x2", "src/lib.rs");
        let executor = ImprovementExecutor::new(&cfg, dir.path());
        let mut count = 1usize; // already at limit
        let outcome = executor.run(&c, &mut count).unwrap();
        assert!(matches!(outcome, Outcome::Skipped { .. }));
    }

    #[test]
    fn pre_check_skips_on_dirty_git_state() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        std::fs::write(dir.path().join("dirty.txt"), "dirty").unwrap();
        let cfg = Config::default();
        let c = candidate("x3", "src/lib.rs");
        let executor = ImprovementExecutor::new(&cfg, dir.path());
        let mut count = 0usize;
        let outcome = executor.run(&c, &mut count).unwrap();
        assert!(matches!(outcome, Outcome::Skipped { .. }));
    }

    #[test]
    fn rollback_restores_original_branch_and_deletes_improve_branch() {
        let dir = tempdir().unwrap();
        // Init repo with an actual file so we can commit
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().unwrap();
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "test"])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().unwrap();
        std::fs::write(dir.path().join("README.md"), "hello").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().unwrap();

        // Capture original branch name
        let original_out = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir.path())
            .output().unwrap();
        let original = String::from_utf8_lossy(&original_out.stdout).trim().to_string();

        // Create the improve branch
        let improve_branch = "improve/test-branch";
        std::process::Command::new("git")
            .args(["checkout", "-b", improve_branch])
            .current_dir(dir.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status().unwrap();

        // Execute rollback via the module's private helper
        git_rollback(dir.path(), &original, improve_branch).unwrap();

        // Assert we are back on the original branch
        let current_out = std::process::Command::new("git")
            .args(["rev-parse", "--abbrev-ref", "HEAD"])
            .current_dir(dir.path())
            .output().unwrap();
        let current = String::from_utf8_lossy(&current_out.stdout).trim().to_string();
        assert_eq!(current, original, "should be back on the original branch");

        // Assert the improve branch no longer exists
        let list_out = std::process::Command::new("git")
            .args(["branch", "--list", improve_branch])
            .current_dir(dir.path())
            .output().unwrap();
        let listed = String::from_utf8_lossy(&list_out.stdout).trim().to_string();
        assert!(listed.is_empty(), "improve branch should have been deleted");
    }

    #[test]
    fn append_log_creates_file_and_appends_jsonl() {
        let dir = tempdir().unwrap();
        let entry = LogEntry {
            id: "log1".to_string(),
            description: "test entry".to_string(),
            risk: "Low".to_string(),
            branch: "improve/log1-test".to_string(),
            outcome: "Applied".to_string(),
            reason: None,
            approved_by_claude: false,
            blocklist_bypassed: false,
            source_signals: vec![],
            created_at: 0,
            completed_at: 1,
        };
        append_log(dir.path(), &entry).unwrap();
        append_log(dir.path(), &entry).unwrap();

        let content = std::fs::read_to_string(dir.path().join(".codi/improvement_log.jsonl"))
            .unwrap();
        assert_eq!(content.lines().count(), 2);
        // each line must be valid JSON
        for line in content.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }
}
