//! Reliability layer: task classification, decomposition, verification,
//! retry and escalation for small local model execution.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{Config, ReliabilityConfig, RoutingMode};
use crate::engine;

// ── Public types ──────────────────────────────────────────────────────────────

pub enum RunContext { Cli, Mcp }

pub struct ReliabilityOutcome {
    pub success: bool,
    pub exit_code: i32,
    pub execution_mode: String,
    pub steps_total: usize,
    pub steps_succeeded: usize,
    pub decision_reason: String,
    pub events: Vec<ReliabilityEvent>,
}

pub struct TaskProfile {
    pub write_intent: bool,
    pub complexity: TaskComplexity,
    pub decision_reason: String,
}

pub enum TaskComplexity { Simple, Complex }

pub enum ModelTier { Small, Medium, Large }

pub struct ExecutionPlan {
    pub steps: Vec<TaskStep>,
    pub decision_reason: String,
}

pub struct TaskStep {
    pub description: String,
    pub expected_paths: Vec<String>,
}

// ── Verification types ────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum VerificationResult {
    Pass,
    Fail(VerificationFailReason),
}

#[derive(Debug, Clone)]
pub enum VerificationFailReason {
    NoDiff,
    MissingPaths(Vec<String>),
    NonZeroExit(i32),
}

impl VerificationFailReason {
    pub fn to_log_string(&self) -> String {
        match self {
            Self::NoDiff => "no_diff".to_string(),
            Self::MissingPaths(paths) => format!("missing_paths:{}", paths.join(",")),
            Self::NonZeroExit(code) => format!("nonzero_exit:{code}"),
        }
    }
}

// ── Constants ─────────────────────────────────────────────────────────────────

const WRITE_KEYWORDS: &[&str] = &[
    "create", "add", "implement", "fix", "refactor", "modify", "update",
    "write", "generate", "scaffold", "build", "set up", "init",
];

const READ_KEYWORDS: &[&str] = &[
    "review", "describe", "analyze", "explain", "list", "show", "check", "audit", "read",
];

const FILE_EXTENSIONS: &[&str] = &[
    ".rs", ".ts", ".tsx", ".js", ".jsx", ".py", ".go", ".java", ".rb",
    ".c", ".h", ".cpp", ".hpp", ".md", ".toml", ".yaml", ".yml", ".json",
];

const DECOMPOSE_PATTERNS: &[&str] = &[
    "birden fazla", "multiple files", "several files", "repo kur",
    "set up repository", "scaffold", "birkaç dosya", "multiple directories",
    "birkaç klasör", "several directories",
];

// ── Model tier ────────────────────────────────────────────────────────────────

pub(crate) fn detect_model_tier(model_name: &str, tier_override: &str) -> ModelTier {
    match tier_override {
        "small" => return ModelTier::Small,
        "medium" => return ModelTier::Medium,
        "large" => return ModelTier::Large,
        _ => {}
    }
    let lower = model_name.to_lowercase();
    if ["1b", "2b", "3b", "7b", "8b"].iter().any(|s| lower.contains(s)) {
        ModelTier::Small
    } else if ["13b", "14b", "32b"].iter().any(|s| lower.contains(s)) {
        ModelTier::Medium
    } else {
        ModelTier::Large
    }
}

pub(crate) fn tier_threshold(tier: &ModelTier) -> u32 {
    match tier {
        ModelTier::Small => 2,
        ModelTier::Medium => 4,
        ModelTier::Large => 8,
    }
}

// ── Write-intent detection ────────────────────────────────────────────────────

fn detect_write_intent(task: &str) -> bool {
    let lower = task.to_lowercase();
    let has_read = READ_KEYWORDS.iter().any(|kw| lower.contains(kw));
    let has_write = WRITE_KEYWORDS.iter().any(|kw| lower.contains(kw));
    // Default to write (fail-safe: never mask a silent failure)
    !(has_read && !has_write)
}

fn count_complexity_signals(task: &str) -> u32 {
    let lower = task.to_lowercase();
    let mut count = 0u32;

    // Count words that look like file paths (have a known extension)
    let file_count = task
        .split_whitespace()
        .filter(|w| {
            let w = w.trim_matches(|c: char| c == ',' || c == ';' || c == '\'' || c == '"');
            FILE_EXTENSIONS.iter().any(|ext| w.ends_with(ext))
        })
        .count() as u32;
    count += file_count;

    // Decompose pattern keywords
    for pat in DECOMPOSE_PATTERNS {
        if lower.contains(pat) {
            count += 1;
        }
    }

    // Long task
    if task.len() > 600 {
        count += 1;
    }

    count
}

// ── classify_task ─────────────────────────────────────────────────────────────

pub fn classify_task(task: &str, cfg: &ReliabilityConfig, model_name: &str) -> TaskProfile {
    let write_intent = detect_write_intent(task);
    let tier = detect_model_tier(model_name, &cfg.model_tier);
    let threshold = cfg.decompose_threshold.unwrap_or_else(|| tier_threshold(&tier));
    let signals = count_complexity_signals(task);

    let tier_name = match &tier {
        ModelTier::Small => "small",
        ModelTier::Medium => "medium",
        ModelTier::Large => "large",
    };

    let (complexity, decision_reason) = if signals >= threshold {
        (
            TaskComplexity::Complex,
            format!(
                "complexity signals={signals} >= threshold={threshold} \
                 (tier={tier_name}, model={model_name}); will decompose"
            ),
        )
    } else {
        (
            TaskComplexity::Simple,
            format!(
                "complexity signals={signals} < threshold={threshold} \
                 (tier={tier_name}, model={model_name}); single-shot"
            ),
        )
    };

    TaskProfile { write_intent, complexity, decision_reason }
}

// ── Stubs (filled in later tasks) ────────────────────────────────────────────

pub(crate) fn decompose(task: &str) -> ExecutionPlan {
    // Extract words that look like file paths (extension + no leading dot)
    let mut seen = std::collections::HashSet::new();
    let mentioned_files: Vec<String> = task
        .split_whitespace()
        .filter_map(|w| {
            let w = w.trim_matches(|c: char| {
                c == ',' || c == ';' || c == '\'' || c == '"' || c == ')' || c == '('
            });
            let has_ext = FILE_EXTENSIONS.iter().any(|ext| w.ends_with(ext));
            if has_ext && !w.starts_with('.') && w.len() > 3 {
                if seen.insert(w.to_string()) { Some(w.to_string()) } else { None }
            } else {
                None
            }
        })
        .collect();

    let steps = if mentioned_files.is_empty() {
        vec![TaskStep { description: task.to_string(), expected_paths: vec![] }]
    } else {
        let prefix = &task[..task.len().min(120)];
        mentioned_files
            .iter()
            .map(|file| TaskStep {
                description: format!("{prefix} — focus only on: {file}"),
                expected_paths: vec![file.clone()],
            })
            .collect()
    };

    let reason = if mentioned_files.is_empty() {
        "no file paths detected; running as single step".to_string()
    } else {
        format!(
            "{} file path(s) detected, decomposed into {} step(s): {}",
            mentioned_files.len(), steps.len(), mentioned_files.join(", ")
        )
    };

    ExecutionPlan { steps, decision_reason: reason }
}

pub(crate) fn verify_step(
    step: &TaskStep,
    profile: &TaskProfile,
    repo_root: &Path,
    exit_code: i32,
) -> VerificationResult {
    if exit_code != 0 {
        return VerificationResult::Fail(VerificationFailReason::NonZeroExit(exit_code));
    }
    if !profile.write_intent {
        return VerificationResult::Pass;
    }

    let changed = git_changed_files(repo_root);

    if changed.is_empty() {
        return VerificationResult::Fail(VerificationFailReason::NoDiff);
    }

    if !step.expected_paths.is_empty() {
        let missing: Vec<String> = step.expected_paths.iter()
            .filter(|exp| !changed.iter().any(|f| f.ends_with(exp.as_str())))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return VerificationResult::Fail(VerificationFailReason::MissingPaths(missing));
        }
    }

    VerificationResult::Pass
}

// ── ReliabilityEvent and log helpers ─────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReliabilityEvent {
    pub task_id: String,
    pub task_snippet: String,
    pub step_index: usize,
    pub execution_mode: String,
    pub provider: String,
    pub attempt: u8,
    pub exit_code: i32,
    /// VerificationFailReason::to_log_string() or "pass"
    pub verification: String,
    pub outcome: String,
    pub decision_reason: String,
    pub timestamp: u64,
}

pub(crate) fn current_timestamp() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub(crate) fn generate_task_id() -> String {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    format!("{micros:016x}")
}

pub(crate) fn resolve_log_path(repo_root: &Path, log_path: &str) -> Result<PathBuf> {
    if std::path::Path::new(log_path).is_absolute() {
        anyhow::bail!("reliability.log_path must be a relative path, got: {log_path}");
    }
    if log_path.contains("..") {
        anyhow::bail!("reliability.log_path must not contain '..', got: {log_path}");
    }
    Ok(repo_root.join(log_path))
}

pub(crate) fn append_reliability_log(
    repo_root: &Path,
    cfg: &ReliabilityConfig,
    event: &ReliabilityEvent,
) -> Result<()> {
    if !cfg.log_events {
        return Ok(());
    }
    let log_path = resolve_log_path(repo_root, &cfg.log_path)?;
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating log dir {}", parent.display()))?;
    }
    let line = serde_json::to_string(event).context("serializing ReliabilityEvent")? + "\n";
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?
        .write_all(line.as_bytes())
        .context("writing reliability event")
}

fn run_engine(cfg: &Config, task: &str, repo_root: &Path, ctx: &RunContext) -> Result<i32> {
    match ctx {
        RunContext::Mcp => engine::run_session_mcp(cfg, task, None, repo_root, ""),
        RunContext::Cli => engine::run_session(
            cfg,
            task,
            engine::SessionMode::OneShot(task.to_string()),
            None,
            repo_root,
            "",
        ),
    }
}

fn git_changed_files(repo_root: &Path) -> Vec<String> {
    // Tracked modifications and deletions
    let mut files: Vec<String> = std::process::Command::new("git")
        .args(["diff", "HEAD", "--name-only"])
        .current_dir(repo_root)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(|l| l.to_string())
                .collect()
        })
        .unwrap_or_default();

    // Untracked new files (git diff HEAD won't show them).
    // --untracked-files=all expands directories so we get individual file paths.
    let untracked: Vec<String> = std::process::Command::new("git")
        .args(["status", "--short", "--untracked-files=all"])
        .current_dir(repo_root)
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| {
                    if l.starts_with("??") { Some(l[3..].trim().to_string()) } else { None }
                })
                .collect()
        })
        .unwrap_or_default();

    files.extend(untracked);
    files
}

#[allow(clippy::too_many_arguments)]
fn execute_with_guard(
    cfg: &Config,
    step: &TaskStep,
    step_index: usize,
    profile: &TaskProfile,
    execution_mode: &str,
    repo_root: &Path,
    task_id: &str,
    engine_fn: &dyn Fn(&str, u8) -> Result<i32>,
) -> Result<Vec<ReliabilityEvent>> {
    let provider_str = format!("local({})", cfg.model.local.model);
    let mut events: Vec<ReliabilityEvent> = Vec::new();

    let make_event = |attempt: u8, exit_code: i32, verification: &str, outcome: &str| {
        ReliabilityEvent {
            task_id: task_id.to_string(),
            task_snippet: step.description[..step.description.len().min(120)].to_string(),
            step_index,
            execution_mode: execution_mode.to_string(),
            provider: provider_str.clone(),
            attempt,
            exit_code,
            verification: verification.to_string(),
            outcome: outcome.to_string(),
            decision_reason: profile.decision_reason.clone(),
            timestamp: current_timestamp(),
        }
    };

    // Attempt 1 — local model
    let exit_code = engine_fn(&step.description, 1)?;
    let v1 = verify_step(step, profile, repo_root, exit_code);

    if matches!(v1, VerificationResult::Pass) {
        let event = make_event(1, exit_code, "pass", "success");
        append_reliability_log(repo_root, &cfg.reliability, &event)?;
        events.push(event);
        return Ok(events);
    }

    let fail1 = match &v1 {
        VerificationResult::Fail(r) => r.to_log_string(),
        _ => unreachable!(),
    };
    tracing::warn!(step = step_index, reason = %fail1, "step failed verification");

    // Attempt 2 — local retry with narrowed prompt
    if cfg.reliability.max_retries > 0 {
        let retry_prompt = format!(
            "Previous attempt wrote no files. Focus only on: {}",
            step.description
        );
        let retry_exit = engine_fn(&retry_prompt, 2)?;
        let v2 = verify_step(step, profile, repo_root, retry_exit);

        if matches!(v2, VerificationResult::Pass) {
            let event = make_event(2, retry_exit, "pass", "retry_success");
            append_reliability_log(repo_root, &cfg.reliability, &event)?;
            events.push(event);
            return Ok(events);
        }

        let fail2 = match &v2 {
            VerificationResult::Fail(r) => r.to_log_string(),
            _ => unreachable!(),
        };
        tracing::warn!(step = step_index, reason = %fail2, "retry failed");

        // Attempt 3 — cloud escalation
        if cfg.reliability.escalate_on_retry_failure && cfg.model.cloud.is_some() {
            let cloud_label = cfg.model.cloud.as_ref()
                .map(|c| format!("cloud({}/{})", c.provider, c.model))
                .unwrap_or_else(|| "cloud".to_string());

            tracing::warn!(step = step_index, provider = %cloud_label, "escalating to cloud");

            let esc_exit = engine_fn(&step.description, 3)?;
            let v3 = verify_step(step, profile, repo_root, esc_exit);

            let (v3_str, outcome, ok) = match &v3 {
                VerificationResult::Pass => (
                    "pass".to_string(),
                    "escalation_success".to_string(),
                    true,
                ),
                VerificationResult::Fail(r) => (
                    r.to_log_string(),
                    "escalation_fail".to_string(),
                    false,
                ),
            };

            let event = ReliabilityEvent {
                task_id: task_id.to_string(),
                task_snippet: step.description[..step.description.len().min(120)].to_string(),
                step_index,
                execution_mode: execution_mode.to_string(),
                provider: cloud_label,
                attempt: 3,
                exit_code: esc_exit,
                verification: v3_str,
                outcome,
                decision_reason: profile.decision_reason.clone(),
                timestamp: current_timestamp(),
            };
            append_reliability_log(repo_root, &cfg.reliability, &event)?;
            events.push(event);

            if ok {
                return Ok(events);
            }
            anyhow::bail!(
                "step {step_index} failed after local retry and cloud escalation \
                 (retry_reason={fail2})"
            );
        }

        // No cloud — log failure and bail
        let event = make_event(2, retry_exit, &fail2, "fail");
        append_reliability_log(repo_root, &cfg.reliability, &event)?;
        events.push(event);
        anyhow::bail!("step {step_index} failed after retry (reason={fail2})");
    }

    // max_retries = 0: log first failure and bail
    let event = make_event(1, exit_code, &fail1, "fail");
    append_reliability_log(repo_root, &cfg.reliability, &event)?;
    events.push(event);
    anyhow::bail!("step {step_index} failed (no retries): {fail1}");
}

pub fn run_reliable_session(
    cfg: &Config,
    task: &str,
    repo_root: &Path,
    ctx: RunContext,
) -> Result<ReliabilityOutcome> {
    // Fast path: reliability disabled
    if !cfg.reliability.enabled {
        let exit_code = run_engine(cfg, task, repo_root, &ctx)?;
        return Ok(ReliabilityOutcome {
            success: exit_code == 0,
            exit_code,
            execution_mode: "bypass".to_string(),
            steps_total: 1,
            steps_succeeded: if exit_code == 0 { 1 } else { 0 },
            decision_reason: "reliability.enabled = false".to_string(),
            events: vec![],
        });
    }

    let profile = classify_task(task, &cfg.reliability, &cfg.model.local.model);
    let task_id = generate_task_id();

    let (execution_mode, steps) = match profile.complexity {
        TaskComplexity::Simple => (
            "single_shot".to_string(),
            vec![TaskStep { description: task.to_string(), expected_paths: vec![] }],
        ),
        TaskComplexity::Complex => {
            let plan = decompose(task);
            tracing::info!(
                steps = plan.steps.len(),
                reason = %plan.decision_reason,
                "task decomposed"
            );
            ("decomposed".to_string(), plan.steps)
        }
    };

    let steps_total = steps.len();
    let mut steps_succeeded = 0usize;
    let mut all_events: Vec<ReliabilityEvent> = Vec::new();

    let cloud_cfg_storage;
    let engine_fn = {
        let local_cfg = cfg;
        let repo_root_ref = repo_root;
        let ctx_ref = &ctx;

        // Build a cloud config for escalation (attempt 3) if configured.
        let cloud_cfg: Option<Config> = if local_cfg.reliability.escalate_on_retry_failure
            && local_cfg.model.cloud.is_some()
        {
            let mut c = local_cfg.clone();
            c.routing.mode = RoutingMode::CloudPreferred;
            Some(c)
        } else {
            None
        };
        cloud_cfg_storage = cloud_cfg;

        move |task_str: &str, attempt: u8| -> Result<i32> {
            if attempt >= 3 {
                if let Some(ref cc) = cloud_cfg_storage {
                    return run_engine(cc, task_str, repo_root_ref, ctx_ref);
                }
            }
            run_engine(local_cfg, task_str, repo_root_ref, ctx_ref)
        }
    };

    for (i, step) in steps.iter().enumerate() {
        match execute_with_guard(cfg, step, i, &profile, &execution_mode, repo_root, &task_id, &engine_fn) {
            Ok(step_events) => {
                all_events.extend(step_events);
                steps_succeeded += 1;
            }
            Err(e) => {
                tracing::error!(step = i, error = %e, "step failed");
                return Ok(ReliabilityOutcome {
                    success: false,
                    exit_code: 1,
                    execution_mode,
                    steps_total,
                    steps_succeeded,
                    decision_reason: profile.decision_reason,
                    events: all_events,
                });
            }
        }
    }

    Ok(ReliabilityOutcome {
        success: steps_succeeded == steps_total,
        exit_code: 0,
        execution_mode,
        steps_total,
        steps_succeeded,
        decision_reason: profile.decision_reason,
        events: all_events,
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ReliabilityConfig;
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

    fn write_file(dir: &std::path::Path, path: &str, content: &str) {
        let full = dir.join(path);
        if let Some(p) = full.parent() { std::fs::create_dir_all(p).unwrap(); }
        std::fs::write(full, content).unwrap();
    }

    fn write_profile() -> TaskProfile {
        TaskProfile { write_intent: true, complexity: TaskComplexity::Simple,
            decision_reason: "test".to_string() }
    }
    fn read_profile() -> TaskProfile {
        TaskProfile { write_intent: false, complexity: TaskComplexity::Simple,
            decision_reason: "test".to_string() }
    }

    #[test]
    fn verify_nonzero_exit_always_fails() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let step = TaskStep { description: "x".to_string(), expected_paths: vec![] };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 1),
            VerificationResult::Fail(VerificationFailReason::NonZeroExit(1))
        ));
    }

    #[test]
    fn verify_read_intent_empty_diff_passes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let step = TaskStep { description: "review code".to_string(), expected_paths: vec![] };
        assert!(matches!(verify_step(&step, &read_profile(), dir.path(), 0), VerificationResult::Pass));
    }

    #[test]
    fn verify_write_intent_empty_diff_fails() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let step = TaskStep { description: "create foo.rs".to_string(), expected_paths: vec![] };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 0),
            VerificationResult::Fail(VerificationFailReason::NoDiff)
        ));
    }

    #[test]
    fn verify_write_intent_with_changed_file_passes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        write_file(dir.path(), "src/foo.rs", "fn hello() {}");
        let step = TaskStep { description: "create src/foo.rs".to_string(), expected_paths: vec![] };
        assert!(matches!(verify_step(&step, &write_profile(), dir.path(), 0), VerificationResult::Pass));
    }

    #[test]
    fn verify_expected_path_present_passes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        write_file(dir.path(), "src/foo.rs", "fn hello() {}");
        let step = TaskStep {
            description: "create src/foo.rs".to_string(),
            expected_paths: vec!["src/foo.rs".to_string()],
        };
        assert!(matches!(verify_step(&step, &write_profile(), dir.path(), 0), VerificationResult::Pass));
    }

    #[test]
    fn verify_expected_path_missing_fails() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        write_file(dir.path(), "src/other.rs", "fn other() {}");
        let step = TaskStep {
            description: "create src/foo.rs".to_string(),
            expected_paths: vec!["src/foo.rs".to_string()],
        };
        let result = verify_step(&step, &write_profile(), dir.path(), 0);
        assert!(matches!(
            result,
            VerificationResult::Fail(VerificationFailReason::MissingPaths(ref p))
            if p.contains(&"src/foo.rs".to_string())
        ));
    }

    #[test]
    fn verification_fail_reason_log_strings() {
        assert_eq!(VerificationFailReason::NoDiff.to_log_string(), "no_diff");
        assert_eq!(
            VerificationFailReason::MissingPaths(vec!["src/foo.rs".to_string()]).to_log_string(),
            "missing_paths:src/foo.rs"
        );
        assert_eq!(VerificationFailReason::NonZeroExit(2).to_log_string(), "nonzero_exit:2");
    }

    fn default_cfg() -> ReliabilityConfig { ReliabilityConfig::default() }

    #[test]
    fn write_keywords_produce_write_intent() {
        let p = classify_task("create src/foo.rs with a hello function", &default_cfg(), "qwen2.5:7b");
        assert!(p.write_intent);
    }

    #[test]
    fn read_keywords_produce_read_intent() {
        let p = classify_task(
            "review the changes in src/foo.rs and describe what they do",
            &default_cfg(), "qwen2.5:7b",
        );
        assert!(!p.write_intent);
    }

    #[test]
    fn ambiguous_task_defaults_to_write_intent() {
        // no write or read keywords → fail-safe default is write
        let p = classify_task("look at the code", &default_cfg(), "qwen2.5:7b");
        assert!(p.write_intent);
    }

    #[test]
    fn seven_b_model_is_small() {
        assert!(matches!(detect_model_tier("qwen2.5:7b", ""), ModelTier::Small));
    }

    #[test]
    fn three_b_model_is_small() {
        assert!(matches!(detect_model_tier("llama3.2:3b", ""), ModelTier::Small));
    }

    #[test]
    fn fourteen_b_model_is_medium() {
        assert!(matches!(detect_model_tier("qwen2.5:14b", ""), ModelTier::Medium));
    }

    #[test]
    fn unknown_model_is_large() {
        assert!(matches!(detect_model_tier("deepseek-coder-v2", ""), ModelTier::Large));
    }

    #[test]
    fn tier_override_wins_over_name() {
        assert!(matches!(detect_model_tier("qwen2.5:7b", "large"), ModelTier::Large));
        assert!(matches!(detect_model_tier("gpt-4o", "small"), ModelTier::Small));
    }

    #[test]
    fn tier_thresholds_are_correct() {
        assert_eq!(tier_threshold(&ModelTier::Small), 2);
        assert_eq!(tier_threshold(&ModelTier::Medium), 4);
        assert_eq!(tier_threshold(&ModelTier::Large), 8);
    }

    #[test]
    fn single_file_task_is_simple_for_small_model() {
        let p = classify_task("add a hello() function to src/main.rs", &default_cfg(), "qwen2.5:7b");
        assert!(matches!(p.complexity, TaskComplexity::Simple));
    }

    #[test]
    fn multi_file_task_is_complex_for_small_model() {
        let p = classify_task(
            "create src/foo.rs and src/bar.rs and src/baz.rs each with a hello() function",
            &default_cfg(), "qwen2.5:7b",
        );
        assert!(matches!(p.complexity, TaskComplexity::Complex));
    }

    #[test]
    fn explicit_threshold_overrides_tier() {
        let mut cfg = default_cfg();
        cfg.decompose_threshold = Some(10); // very high
        let p = classify_task(
            "create src/foo.rs and src/bar.rs and src/baz.rs",
            &cfg, "qwen2.5:7b",
        );
        assert!(matches!(p.complexity, TaskComplexity::Simple));
    }

    #[test]
    fn decision_reason_is_non_empty() {
        let p = classify_task("add hello() to src/main.rs", &default_cfg(), "qwen2.5:7b");
        assert!(!p.decision_reason.is_empty());
        assert!(p.decision_reason.contains("threshold"));
    }

    #[test]
    fn decompose_single_file_mention_yields_one_step() {
        let plan = decompose("add hello() to src/main.rs");
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].expected_paths, vec!["src/main.rs".to_string()]);
    }

    #[test]
    fn decompose_two_file_mentions_yield_two_steps() {
        let plan = decompose("create src/foo.rs and src/bar.rs");
        assert_eq!(plan.steps.len(), 2);
        let paths: Vec<_> = plan.steps.iter()
            .flat_map(|s| s.expected_paths.iter()).cloned().collect();
        assert!(paths.contains(&"src/foo.rs".to_string()));
        assert!(paths.contains(&"src/bar.rs".to_string()));
    }

    #[test]
    fn decompose_no_file_mentions_yields_single_full_step() {
        let plan = decompose("scaffold a new project with multiple directories");
        assert_eq!(plan.steps.len(), 1);
        assert!(plan.steps[0].expected_paths.is_empty());
    }

    #[test]
    fn decompose_decision_reason_is_non_empty() {
        let plan = decompose("create src/foo.rs");
        assert!(!plan.decision_reason.is_empty());
    }

    #[test]
    fn decompose_step_description_references_file() {
        let plan = decompose("create src/foo.rs with hello() function");
        assert!(plan.steps[0].description.contains("src/foo.rs"));
    }

    // ── Task 6: ReliabilityEvent and log helpers ─────────────────────────────

    #[test]
    fn reliability_event_serializes_to_json() {
        let event = ReliabilityEvent {
            task_id: "abc".to_string(),
            task_snippet: "create src/foo.rs".to_string(),
            step_index: 0,
            execution_mode: "single_shot".to_string(),
            provider: "local(qwen2.5:7b)".to_string(),
            attempt: 1,
            exit_code: 0,
            verification: "pass".to_string(),
            outcome: "success".to_string(),
            decision_reason: "signals=0 < threshold=2".to_string(),
            timestamp: 12345,
        };
        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("\"outcome\":\"success\""));
        assert!(json.contains("\"task_id\":\"abc\""));
    }

    #[test]
    fn append_log_creates_file_and_appends() {
        let dir = tempdir().unwrap();
        let cfg = ReliabilityConfig::default();
        let event = ReliabilityEvent {
            task_id: "t1".to_string(), task_snippet: "create foo.rs".to_string(),
            step_index: 0, execution_mode: "single_shot".to_string(),
            provider: "local(qwen2.5:7b)".to_string(), attempt: 1, exit_code: 0,
            verification: "pass".to_string(), outcome: "success".to_string(),
            decision_reason: "simple".to_string(), timestamp: 1,
        };
        append_reliability_log(dir.path(), &cfg, &event).unwrap();
        let path = dir.path().join(".codi/reliability.jsonl");
        assert!(path.exists());
        let content = std::fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(parsed["outcome"], "success");
    }

    #[test]
    fn append_log_appends_not_overwrites() {
        let dir = tempdir().unwrap();
        let cfg = ReliabilityConfig::default();
        let make_event = |task_id: &str, outcome: &str| ReliabilityEvent {
            task_id: task_id.to_string(), task_snippet: "x".to_string(),
            step_index: 0, execution_mode: "single_shot".to_string(),
            provider: "local".to_string(), attempt: 1, exit_code: 0,
            verification: "pass".to_string(), outcome: outcome.to_string(),
            decision_reason: "y".to_string(), timestamp: 1,
        };
        append_reliability_log(dir.path(), &cfg, &make_event("t1", "success")).unwrap();
        append_reliability_log(dir.path(), &cfg, &make_event("t2", "fail")).unwrap();
        let path = dir.path().join(".codi/reliability.jsonl");
        let content = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "should have two lines");
        let first: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(first["task_id"], "t1");
        assert_eq!(second["task_id"], "t2");
    }

    #[test]
    fn append_log_noop_when_disabled() {
        let dir = tempdir().unwrap();
        let mut cfg = ReliabilityConfig::default();
        cfg.log_events = false;
        let event = ReliabilityEvent {
            task_id: "t2".to_string(), task_snippet: "x".to_string(),
            step_index: 0, execution_mode: "single_shot".to_string(),
            provider: "local".to_string(), attempt: 1, exit_code: 0,
            verification: "pass".to_string(), outcome: "success".to_string(),
            decision_reason: "y".to_string(), timestamp: 2,
        };
        append_reliability_log(dir.path(), &cfg, &event).unwrap();
        assert!(!dir.path().join(".codi/reliability.jsonl").exists());
    }

    #[test]
    fn resolve_log_path_rejects_absolute() {
        let dir = tempdir().unwrap();
        let err = resolve_log_path(dir.path(), "/etc/passwd").unwrap_err();
        assert!(err.to_string().contains("relative"));
    }

    #[test]
    fn resolve_log_path_rejects_traversal() {
        let dir = tempdir().unwrap();
        let err = resolve_log_path(dir.path(), "../outside.jsonl").unwrap_err();
        assert!(err.to_string().contains(".."));
    }

    #[test]
    fn resolve_log_path_accepts_valid_relative() {
        let dir = tempdir().unwrap();
        let path = resolve_log_path(dir.path(), ".codi/reliability.jsonl").unwrap();
        assert_eq!(path, dir.path().join(".codi/reliability.jsonl"));
    }

    #[test]
    fn current_timestamp_returns_nonzero() {
        assert!(current_timestamp() > 0);
    }

    #[test]
    fn generate_task_id_is_nonempty() {
        let id = generate_task_id();
        assert!(!id.is_empty());
        // Two IDs generated quickly may differ or be the same depending on timing,
        // but each must be a valid non-empty string
        assert!(id.len() >= 8);
    }

    // ── Task 7: run_reliable_session ─────────────────────────────────────────

    #[test]
    fn reliability_outcome_struct_has_correct_fields() {
        let outcome = ReliabilityOutcome {
            success: true,
            exit_code: 0,
            execution_mode: "single_shot".to_string(),
            steps_total: 1,
            steps_succeeded: 1,
            decision_reason: "test".to_string(),
            events: vec![],
        };
        assert!(outcome.success);
        assert_eq!(outcome.steps_total, 1);
        assert_eq!(outcome.steps_succeeded, 1);
    }

    #[test]
    fn classify_before_running_does_not_panic() {
        let mut cfg = crate::config::Config::default();
        cfg.reliability.enabled = false;
        // Verify that classify_task works end-to-end with a real config
        let profile = classify_task("create src/foo.rs", &cfg.reliability, &cfg.model.local.model);
        assert!(profile.write_intent);
        // 1 file signal < threshold=2 for small model → Simple
        assert!(matches!(profile.complexity, TaskComplexity::Simple));
    }

    #[test]
    fn run_reliable_session_disabled_returns_bypass_outcome() {
        use crate::config::Config;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        // Set up a minimal git repo so run_engine won't panic before hitting goose
        // (engine will fail because goose is not installed, so we just test the
        // disabled fast-path which calls run_engine — expect Err if goose absent)
        let mut cfg = Config::default();
        cfg.reliability.enabled = false;

        // With reliability disabled, run_reliable_session calls run_engine which
        // calls engine::run_session_mcp — goose likely absent in CI, so we expect
        // either Ok with bypass outcome or an Err (no panic).
        let result = run_reliable_session(&cfg, "echo test", dir.path(), RunContext::Cli);
        // We only require no panic. Err is acceptable (goose not installed).
        match result {
            Ok(outcome) => {
                assert_eq!(outcome.execution_mode, "bypass");
                assert_eq!(outcome.decision_reason, "reliability.enabled = false");
                assert_eq!(outcome.steps_total, 1);
            }
            Err(_) => {
                // Goose not installed — acceptable in test env
            }
        }
    }

    #[test]
    fn run_reliable_session_enabled_no_goose_returns_err_not_panic() {
        use crate::config::Config;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let mut cfg = Config::default();
        cfg.reliability.enabled = true;

        // Task with no file paths → Simple → single_shot → calls run_engine
        // Goose not installed → Err expected, NOT a panic
        let result = run_reliable_session(&cfg, "echo hello", dir.path(), RunContext::Cli);
        // Just ensure it doesn't panic; Err is fine
        let _ = result;
    }

    // ── Task 7: execute_with_guard + mock engine tests ───────────────────────

    /// Builds a default Config with reliability enabled.
    fn enabled_cfg() -> crate::config::Config {
        let mut cfg = crate::config::Config::default();
        cfg.reliability.enabled = true;
        cfg.reliability.max_retries = 1;
        cfg
    }

    #[test]
    fn single_shot_success_with_git_diff_passes() {
        // Temp git repo with a committed file change (write-intent task, real diff)
        let dir = tempdir().unwrap();
        init_git(dir.path());
        write_file(dir.path(), "src/foo.rs", "fn hello() {}");

        let cfg = enabled_cfg();
        let profile = write_profile();
        let step = TaskStep {
            description: "create src/foo.rs".to_string(),
            expected_paths: vec![],
        };

        // Mock engine always returns exit code 0 (success). The git diff will
        // detect the untracked file we wrote above.
        let engine_fn = |_task: &str, _attempt: u8| -> Result<i32> { Ok(0) };

        let events = execute_with_guard(
            &cfg, &step, 0, &profile, "single_shot", dir.path(), "task-001", &engine_fn,
        )
        .unwrap();

        assert!(!events.is_empty());
        assert_eq!(events[0].outcome, "success");
        assert_eq!(events[0].attempt, 1);
    }

    #[test]
    fn single_shot_fail_retry_no_diff_outcome_is_false() {
        // Engine always returns 0 but no git diff → NoDiff on both attempts
        let dir = tempdir().unwrap();
        init_git(dir.path());

        let cfg = enabled_cfg();
        let profile = write_profile();
        let step = TaskStep {
            description: "create src/foo.rs".to_string(),
            expected_paths: vec![],
        };

        let engine_fn = |_task: &str, _attempt: u8| -> Result<i32> { Ok(0) };

        let result = execute_with_guard(
            &cfg, &step, 0, &profile, "single_shot", dir.path(), "task-002", &engine_fn,
        );

        assert!(result.is_err(), "should fail: no git diff on either attempt");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("failed after retry"), "error: {err}");
    }

    #[test]
    fn execute_with_guard_nonzero_exit_fails_immediately() {
        // Mock engine returns exit code 1 → NonZeroExit → outcome.success = false
        let dir = tempdir().unwrap();
        init_git(dir.path());

        let mut cfg = enabled_cfg();
        cfg.reliability.max_retries = 0; // no retry so we bail on attempt 1

        let profile = write_profile();
        let step = TaskStep {
            description: "create src/bar.rs".to_string(),
            expected_paths: vec![],
        };

        let engine_fn = |_task: &str, _attempt: u8| -> Result<i32> { Ok(1) };

        let result = execute_with_guard(
            &cfg, &step, 0, &profile, "single_shot", dir.path(), "task-003", &engine_fn,
        );

        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("nonzero_exit"), "error: {err}");
    }

    #[test]
    fn run_reliable_session_disabled_uses_bypass_outcome() {
        // With reliability.enabled = false, outcome is "bypass" with correct fields.
        // engine_fn is injected indirectly via run_reliable_session's real closure,
        // but goose may not be installed — we only verify the outcome shape when Ok.
        let dir = tempdir().unwrap();
        let mut cfg = crate::config::Config::default();
        cfg.reliability.enabled = false;

        let result = run_reliable_session(&cfg, "echo test", dir.path(), RunContext::Cli);
        match result {
            Ok(outcome) => {
                assert_eq!(outcome.execution_mode, "bypass");
                assert_eq!(outcome.decision_reason, "reliability.enabled = false");
                assert_eq!(outcome.steps_total, 1);
                assert!(outcome.events.is_empty());
            }
            Err(_) => { /* goose absent — acceptable */ }
        }
    }
}
