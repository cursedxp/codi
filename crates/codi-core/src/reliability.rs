//! Reliability layer: task classification, decomposition, verification,
//! retry and escalation for small local model execution.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

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
}

pub struct TaskProfile {
    pub write_intent: bool,
    pub complexity: TaskComplexity,
    pub decision_reason: String,
    pub verify_artifacts: bool,
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
    /// Append-style task: pre-existing content of these files vanished.
    ContentLost(Vec<String>),
}

impl VerificationFailReason {
    pub fn to_log_string(&self) -> String {
        match self {
            Self::NoDiff => "no_diff".to_string(),
            Self::MissingPaths(paths) => format!("missing_paths:{}", paths.join(",")),
            Self::NonZeroExit(code) => format!("nonzero_exit:{code}"),
            Self::ContentLost(paths) => format!("content_lost:{}", paths.join(",")),
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
    // Param-count token must not be preceded by another digit: plain
    // contains("7b") would classify a 27b model as Small.
    let has_size = |needle: &str| {
        lower.match_indices(needle).any(|(i, _)| {
            !lower[..i].ends_with(|c: char| c.is_ascii_digit())
        })
    };
    if ["1b", "2b", "3b", "7b", "8b"].iter().any(|s| has_size(s)) {
        ModelTier::Small
    } else if ["13b", "14b", "32b"].iter().any(|s| has_size(s)) {
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

/// Words that look like file paths (known extension, no leading dot), in
/// first-mention order, deduplicated.
pub(crate) fn extract_file_mentions(task: &str) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    task.split_whitespace()
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
        .collect()
}

/// First `max` bytes of `s`, cut back to a char boundary — plain byte slicing
/// panics mid-UTF-8 (task text is often Turkish).
pub(crate) fn snippet(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

fn count_complexity_signals(task: &str) -> u32 {
    let lower = task.to_lowercase();
    let mut count = 0u32;

    // Distinct file targets only: repeated mentions of the same file (or a
    // verbatim payload quoting its own filename) are one target, not extra work.
    count += extract_file_mentions(task).len() as u32;

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

    TaskProfile { write_intent, complexity, decision_reason, verify_artifacts: cfg.verify_artifacts }
}

// ── Stubs (filled in later tasks) ────────────────────────────────────────────

pub(crate) fn decompose(task: &str) -> ExecutionPlan {
    let mentioned_files = extract_file_mentions(task);

    let steps = if mentioned_files.is_empty() {
        vec![TaskStep { description: task.to_string(), expected_paths: vec![] }]
    } else if mentioned_files.len() == 1 {
        // Single target: nothing to split. Keep the full task text — truncating
        // it would drop the payload the model is asked to write.
        vec![TaskStep {
            description: task.to_string(),
            expected_paths: mentioned_files.clone(),
        }]
    } else {
        // Each step carries the COMPLETE task text (payload included); only
        // the focus differs. Truncation here previously destroyed verbatim
        // content and forced the model to hallucinate it.
        mentioned_files
            .iter()
            .map(|file| TaskStep {
                description: format!("{task}\n\nFocus only on: {file}"),
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

/// Verify a completed step. `changed_detected` is the filesystem-snapshot
/// verdict for whether the run wrote anything (git-independent).
pub(crate) fn verify_step(
    step: &TaskStep,
    profile: &TaskProfile,
    repo_root: &Path,
    exit_code: i32,
    changed_detected: bool,
) -> VerificationResult {
    if exit_code != 0 {
        return VerificationResult::Fail(VerificationFailReason::NonZeroExit(exit_code));
    }
    if !profile.write_intent || !profile.verify_artifacts {
        return VerificationResult::Pass;
    }

    // When explicit target files are known, they must exist — but existence
    // alone can't prove work on a PRE-existing file (an edit that wrote
    // nothing leaves the file present). Require the snapshot to have seen a
    // write as well.
    if !step.expected_paths.is_empty() {
        let missing: Vec<String> = step.expected_paths.iter()
            .filter(|exp| !repo_root.join(exp.as_str()).exists())
            .cloned()
            .collect();
        if !missing.is_empty() {
            return VerificationResult::Fail(VerificationFailReason::MissingPaths(missing));
        }
        if !changed_detected {
            return VerificationResult::Fail(VerificationFailReason::NoDiff);
        }
        return VerificationResult::Pass;
    }

    // No explicit targets: rely on the snapshot change verdict. No git involved.
    if !changed_detected {
        return VerificationResult::Fail(VerificationFailReason::NoDiff);
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

/// Directories never scanned by the filesystem snapshot — build output, VCS
/// metadata, and codi's own state, none of which represent task work.
const VERIFY_EXCLUDE_DIRS: &[&str] = &[".git", ".codi", "target", "node_modules", "dist", "build"];

/// Snapshot the modification time of every file under `repo_root`, skipping
/// excluded directories. Used to detect whether a step wrote anything —
/// filesystem truth, independent of git.
fn snapshot_mtimes(repo_root: &Path) -> HashMap<PathBuf, SystemTime> {
    let mut map = HashMap::new();
    let mut stack = vec![repo_root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_dir() {
                let name = entry.file_name();
                if VERIFY_EXCLUDE_DIRS.iter().any(|e| name.as_os_str() == std::ffi::OsStr::new(e)) {
                    continue;
                }
                stack.push(entry.path());
            } else if ft.is_file() {
                if let Ok(mt) = entry.metadata().and_then(|m| m.modified()) {
                    map.insert(entry.path(), mt);
                }
            }
        }
    }
    map
}

/// True if any file was added, modified (newer mtime), or removed between two
/// snapshots.
fn tree_changed(before: &HashMap<PathBuf, SystemTime>, after: &HashMap<PathBuf, SystemTime>) -> bool {
    after.iter().any(|(p, mt)| match before.get(p) {
        Some(old) => mt > old,
        None => true,
    }) || before.keys().any(|p| !after.contains_key(p))
}

// ── Append-clobber guard ─────────────────────────────────────────────────────

const APPEND_MARKERS: &[&str] = &[
    "append", "do not remove", "don't remove", "keep existing",
    "keep the existing", "without removing", "after the existing",
];

/// Heuristic: the task asks to extend existing content, not replace it.
fn is_append_task(desc: &str) -> bool {
    let lower = desc.to_lowercase();
    APPEND_MARKERS.iter().any(|m| lower.contains(m))
}

/// Files larger than this are not content-guarded (cost cap).
const CONTENT_GUARD_MAX_BYTES: u64 = 512 * 1024;

/// Contents of already-existing expected files, captured before the model
/// runs, so append-style tasks can be checked for clobbering afterwards.
fn snapshot_expected_contents(repo_root: &Path, paths: &[String]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for p in paths {
        let full = repo_root.join(p);
        let small = std::fs::metadata(&full)
            .map(|m| m.is_file() && m.len() <= CONTENT_GUARD_MAX_BYTES)
            .unwrap_or(false);
        if small {
            if let Ok(content) = std::fs::read_to_string(&full) {
                if !content.is_empty() {
                    map.insert(p.clone(), content);
                }
            }
        }
    }
    map
}

/// Files whose pre-run content is no longer present verbatim.
fn lost_content_files(repo_root: &Path, before: &HashMap<String, String>) -> Vec<String> {
    let mut lost: Vec<String> = before
        .iter()
        .filter(|(p, old)| {
            let new = std::fs::read_to_string(repo_root.join(p.as_str())).unwrap_or_default();
            !new.contains(old.as_str())
        })
        .map(|(p, _)| p.clone())
        .collect();
    lost.sort();
    lost
}

/// Run one engine attempt while snapshotting the tree before and after, so we
/// can tell whether the model actually changed any files without relying on git.
fn run_and_detect(
    cfg: &Config,
    task: &str,
    repo_root: &Path,
    ctx: &RunContext,
) -> Result<(i32, bool)> {
    let before = snapshot_mtimes(repo_root);
    let exit = run_engine(cfg, task, repo_root, ctx)?;
    let after = snapshot_mtimes(repo_root);
    Ok((exit, tree_changed(&before, &after)))
}

#[allow(clippy::too_many_arguments)]
fn build_event(
    task_id: &str,
    task_snippet: &str,
    step_index: usize,
    execution_mode: &str,
    provider: &str,
    attempt: u8,
    exit_code: i32,
    verification: &VerificationResult,
    outcome: &str,
    decision_reason: &str,
) -> ReliabilityEvent {
    let verification_str = match verification {
        VerificationResult::Pass => "pass".to_string(),
        VerificationResult::Fail(r) => r.to_log_string(),
    };
    ReliabilityEvent {
        task_id: task_id.to_string(),
        task_snippet: task_snippet.to_string(),
        step_index,
        execution_mode: execution_mode.to_string(),
        provider: provider.to_string(),
        attempt,
        exit_code,
        verification: verification_str,
        outcome: outcome.to_string(),
        decision_reason: decision_reason.to_string(),
        timestamp: current_timestamp(),
    }
}

#[allow(clippy::too_many_arguments)]
fn execute_with_guard(
    step: &TaskStep,
    profile: &TaskProfile,
    cfg: &Config,
    repo_root: &Path,
    task_id: &str,
    step_index: usize,
    execution_mode_str: &str,
    max_retries: u8,
    ctx: &RunContext,
) -> (bool, Vec<ReliabilityEvent>) {
    let local_provider = format!("local({})", cfg.model.local.model);
    let task_snippet = snippet(&step.description, 120);
    let mut events: Vec<ReliabilityEvent> = Vec::new();

    // Content guard: for append-style tasks, capture what the expected files
    // already hold, so a "successful" run that clobbered them still fails.
    let guard_appends =
        profile.write_intent && profile.verify_artifacts && is_append_task(&step.description);
    let before_contents = if guard_appends {
        snapshot_expected_contents(repo_root, &step.expected_paths)
    } else {
        HashMap::new()
    };
    let verify_full = |exit_code: i32, changed: bool| -> VerificationResult {
        let v = verify_step(step, profile, repo_root, exit_code, changed);
        if !matches!(v, VerificationResult::Pass) {
            return v;
        }
        let lost = lost_content_files(repo_root, &before_contents);
        if lost.is_empty() {
            VerificationResult::Pass
        } else {
            VerificationResult::Fail(VerificationFailReason::ContentLost(lost))
        }
    };

    // Whether a failed attempt will be followed by another (retry or cloud).
    let escalation_available =
        cfg.reliability.escalate_on_retry_failure && cfg.model.cloud.is_some();

    // Attempt 1 — local model
    let (exit_code, changed) = match run_and_detect(cfg, &step.description, repo_root, ctx) {
        Ok(pair) => pair,
        Err(e) => {
            tracing::error!(step = step_index, error = %e, "engine error on attempt 1");
            let event = build_event(
                task_id, task_snippet, step_index, execution_mode_str,
                &local_provider, 1, -1,
                &VerificationResult::Fail(VerificationFailReason::NonZeroExit(-1)),
                "fail", &profile.decision_reason,
            );
            let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
            events.push(event);
            return (false, events);
        }
    };
    let v1 = verify_full(exit_code, changed);

    if matches!(v1, VerificationResult::Pass) {
        let event = build_event(
            task_id, task_snippet, step_index, execution_mode_str,
            &local_provider, 1, exit_code, &v1, "success", &profile.decision_reason,
        );
        let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
        events.push(event);
        return (true, events);
    }

    // Log initial failure
    let fail_reason = match &v1 {
        VerificationResult::Fail(r) => r.clone(),
        _ => unreachable!(),
    };
    tracing::warn!(step = step_index, reason = %fail_reason.to_log_string(), "step failed verification");

    // Log attempt 1's failure now if another attempt follows — otherwise the
    // log starts at attempt 2 and hides the original break. When nothing
    // follows, the final "fail" event below records it instead.
    if max_retries > 0 || escalation_available {
        let event = build_event(
            task_id, task_snippet, step_index, execution_mode_str,
            &local_provider, 1, exit_code, &v1, "retrying", &profile.decision_reason,
        );
        let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
        events.push(event);
    }

    // Local retry loop: attempts 2..=(1 + max_retries)
    let mut last_fail_reason = fail_reason;
    let mut last_exit_code = exit_code;

    for retry_num in 1..=(max_retries as u32) {
        let attempt_num = (1 + retry_num) as u8;
        let retry_prompt = match &last_fail_reason {
            VerificationFailReason::NoDiff => format!(
                "Previous attempt wrote no files. Focus only on: {}",
                step.description
            ),
            VerificationFailReason::MissingPaths(_) => format!(
                "Previous attempt did not create all expected files. Focus only on: {}",
                step.description
            ),
            VerificationFailReason::NonZeroExit(code) => format!(
                "Previous attempt failed with exit code {code}. Try again: {}",
                step.description
            ),
            VerificationFailReason::ContentLost(files) => {
                let mut originals = String::new();
                for f in files {
                    if let Some(old) = before_contents.get(f) {
                        originals.push_str(&format!(
                            "\n--- original content of {f} ---\n{}\n",
                            snippet(old, 6_000)
                        ));
                    }
                }
                format!(
                    "Previous attempt deleted existing content from: {}. Rewrite the \
                     file so it keeps ALL of the original content below AND adds what \
                     the task asks.{originals}\nTask: {}",
                    files.join(", "),
                    step.description
                )
            }
        };

        let (retry_exit, retry_changed) = match run_and_detect(cfg, &retry_prompt, repo_root, ctx) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(step = step_index, attempt = attempt_num, error = %e, "engine error on retry");
                let event = build_event(
                    task_id, task_snippet, step_index, execution_mode_str,
                    &local_provider, attempt_num, -1,
                    &VerificationResult::Fail(VerificationFailReason::NonZeroExit(-1)),
                    "fail", &profile.decision_reason,
                );
                let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
                events.push(event);
                return (false, events);
            }
        };
        let v_retry = verify_full(retry_exit, retry_changed);

        if matches!(v_retry, VerificationResult::Pass) {
            let event = build_event(
                task_id, task_snippet, step_index, execution_mode_str,
                &local_provider, attempt_num, retry_exit, &v_retry, "retry_success",
                &profile.decision_reason,
            );
            let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
            events.push(event);
            return (true, events);
        }

        last_exit_code = retry_exit;
        last_fail_reason = match v_retry {
            VerificationResult::Fail(r) => r,
            _ => unreachable!(),
        };
        tracing::warn!(
            step = step_index, attempt = attempt_num,
            reason = %last_fail_reason.to_log_string(), "retry failed"
        );
        // Same rule as attempt 1: log now only if another attempt follows.
        if retry_num < max_retries as u32 || escalation_available {
            let event = build_event(
                task_id, task_snippet, step_index, execution_mode_str,
                &local_provider, attempt_num, retry_exit,
                &VerificationResult::Fail(last_fail_reason.clone()),
                "retrying", &profile.decision_reason,
            );
            let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
            events.push(event);
        }
    }

    // All local retries exhausted — try cloud escalation if configured
    let escalation_attempt = (2u32 + max_retries as u32) as u8;
    if escalation_available {
        let cloud_label = cfg.model.cloud.as_ref()
            .map(|c| format!("cloud({}/{})", c.provider, c.model))
            .unwrap_or_else(|| "cloud".to_string());

        tracing::warn!(step = step_index, provider = %cloud_label, "escalating to cloud");

        let mut cloud_cfg = cfg.clone();
        cloud_cfg.routing.mode = RoutingMode::CloudPreferred;

        let (esc_exit, esc_changed) = match run_and_detect(&cloud_cfg, &step.description, repo_root, ctx) {
            Ok(pair) => pair,
            Err(e) => {
                tracing::error!(step = step_index, error = %e, "engine error on cloud escalation");
                let event = build_event(
                    task_id, task_snippet, step_index, execution_mode_str,
                    &cloud_label, escalation_attempt, -1,
                    &VerificationResult::Fail(VerificationFailReason::NonZeroExit(-1)),
                    "escalation_fail", &profile.decision_reason,
                );
                let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
                events.push(event);
                return (false, events);
            }
        };
        let v_esc = verify_full(esc_exit, esc_changed);

        let (outcome, ok) = match &v_esc {
            VerificationResult::Pass => ("escalation_success", true),
            VerificationResult::Fail(_) => ("escalation_fail", false),
        };
        let event = build_event(
            task_id, task_snippet, step_index, execution_mode_str,
            &cloud_label, escalation_attempt, esc_exit, &v_esc, outcome, &profile.decision_reason,
        );
        let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
        events.push(event);
        return (ok, events);
    }

    // No cloud — log final local failure
    let final_v = VerificationResult::Fail(last_fail_reason);
    let event = build_event(
        task_id, task_snippet, step_index, execution_mode_str,
        &local_provider, escalation_attempt.saturating_sub(1), last_exit_code,
        &final_v, "fail", &profile.decision_reason,
    );
    let _ = append_reliability_log(repo_root, &cfg.reliability, &event);
    events.push(event);
    (false, events)
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
        });
    }

    let profile = classify_task(task, &cfg.reliability, &cfg.model.local.model);
    let task_id = generate_task_id();

    let (execution_mode, steps) = match profile.complexity {
        TaskComplexity::Simple => (
            "single_shot".to_string(),
            // Extract targets here too: without them, verification degrades to
            // "any file anywhere changed", which passes on any write at all.
            vec![TaskStep {
                description: task.to_string(),
                expected_paths: extract_file_mentions(task),
            }],
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

    for (i, step) in steps.iter().enumerate() {
        let (success, _events) = execute_with_guard(
            step,
            &profile,
            cfg,
            repo_root,
            &task_id,
            i,
            &execution_mode,
            cfg.reliability.max_retries,
            &ctx,
        );
        if success {
            steps_succeeded += 1;
        } else {
            tracing::error!(step = i, "step failed");
            return Ok(ReliabilityOutcome {
                success: false,
                exit_code: 1,
                execution_mode,
                steps_total,
                steps_succeeded,
                decision_reason: profile.decision_reason,
            });
        }
    }

    Ok(ReliabilityOutcome {
        success: steps_succeeded == steps_total,
        exit_code: 0,
        execution_mode,
        steps_total,
        steps_succeeded,
        decision_reason: profile.decision_reason,
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
            decision_reason: "test".to_string(), verify_artifacts: true }
    }
    fn read_profile() -> TaskProfile {
        TaskProfile { write_intent: false, complexity: TaskComplexity::Simple,
            decision_reason: "test".to_string(), verify_artifacts: true }
    }

    #[test]
    fn verify_nonzero_exit_always_fails() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let step = TaskStep { description: "x".to_string(), expected_paths: vec![] };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 1, false),
            VerificationResult::Fail(VerificationFailReason::NonZeroExit(1))
        ));
    }

    #[test]
    fn verify_read_intent_empty_diff_passes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let step = TaskStep { description: "review code".to_string(), expected_paths: vec![] };
        assert!(matches!(verify_step(&step, &read_profile(), dir.path(), 0, false), VerificationResult::Pass));
    }

    #[test]
    fn verify_write_intent_empty_diff_fails() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let step = TaskStep { description: "create foo.rs".to_string(), expected_paths: vec![] };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 0, false),
            VerificationResult::Fail(VerificationFailReason::NoDiff)
        ));
    }

    #[test]
    fn verify_write_intent_with_changed_file_passes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        write_file(dir.path(), "src/foo.rs", "fn hello() {}");
        let step = TaskStep { description: "create src/foo.rs".to_string(), expected_paths: vec![] };
        assert!(matches!(verify_step(&step, &write_profile(), dir.path(), 0, true), VerificationResult::Pass));
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
        assert!(matches!(verify_step(&step, &write_profile(), dir.path(), 0, true), VerificationResult::Pass));
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
        let result = verify_step(&step, &write_profile(), dir.path(), 0, true);
        assert!(matches!(
            result,
            VerificationResult::Fail(VerificationFailReason::MissingPaths(ref p))
            if p.contains(&"src/foo.rs".to_string())
        ));
    }

    // The target project is NOT a git repo (no init_git). git-diff is blind
    // here, so verification must rely on the filesystem snapshot instead of
    // false-failing with no_diff.
    #[test]
    fn verify_non_git_repo_with_written_expected_file_passes() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "data.js", "export const X = 1;");
        let step = TaskStep {
            description: "create data.js".to_string(),
            expected_paths: vec!["data.js".to_string()],
        };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 0, true),
            VerificationResult::Pass
        ));
    }

    // A pre-existing expected file with no detected write is NOT proof of
    // work: an edit task where the model wrote nothing must fail.
    #[test]
    fn verify_expected_path_present_but_unchanged_fails_no_diff() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "data.js", "old content");
        let step = TaskStep {
            description: "update data.js".to_string(),
            expected_paths: vec!["data.js".to_string()],
        };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 0, false),
            VerificationResult::Fail(VerificationFailReason::NoDiff)
        ));
    }

    #[test]
    fn verify_non_git_repo_missing_expected_file_still_fails() {
        let dir = tempdir().unwrap();
        // nothing written
        let step = TaskStep {
            description: "create data.js".to_string(),
            expected_paths: vec!["data.js".to_string()],
        };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 0, false),
            VerificationResult::Fail(VerificationFailReason::MissingPaths(_))
        ));
    }

    #[test]
    fn verify_non_git_repo_no_expected_paths_does_not_false_fail() {
        let dir = tempdir().unwrap();
        // Write intent, no explicit expected paths, no git repo, but a change was
        // detected by the filesystem snapshot: must pass.
        let step = TaskStep { description: "create data.js".to_string(), expected_paths: vec![] };
        assert!(matches!(
            verify_step(&step, &write_profile(), dir.path(), 0, true),
            VerificationResult::Pass
        ));
    }

    // ── filesystem snapshot change-detection (git-independent) ────────────────

    fn t(secs: u64) -> std::time::SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    #[test]
    fn tree_changed_detects_new_file() {
        let before = std::collections::HashMap::new();
        let mut after = std::collections::HashMap::new();
        after.insert(std::path::PathBuf::from("/x/data.js"), t(100));
        assert!(tree_changed(&before, &after));
    }

    #[test]
    fn tree_changed_detects_modified_file() {
        let mut before = std::collections::HashMap::new();
        before.insert(std::path::PathBuf::from("/x/data.js"), t(100));
        let mut after = std::collections::HashMap::new();
        after.insert(std::path::PathBuf::from("/x/data.js"), t(200));
        assert!(tree_changed(&before, &after));
    }

    #[test]
    fn tree_changed_detects_deletion() {
        let mut before = std::collections::HashMap::new();
        before.insert(std::path::PathBuf::from("/x/data.js"), t(100));
        let after = std::collections::HashMap::new();
        assert!(tree_changed(&before, &after));
    }

    #[test]
    fn tree_changed_false_when_identical() {
        let mut before = std::collections::HashMap::new();
        before.insert(std::path::PathBuf::from("/x/data.js"), t(100));
        let after = before.clone();
        assert!(!tree_changed(&before, &after));
    }

    #[test]
    fn snapshot_includes_files_and_skips_excluded_dirs() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "data.js", "x");
        write_file(dir.path(), "target/junk.o", "x");
        write_file(dir.path(), "node_modules/pkg/index.js", "x");
        let snap = snapshot_mtimes(dir.path());
        assert!(snap.keys().any(|p| p.ends_with("data.js")), "real file present");
        assert!(!snap.keys().any(|p| p.ends_with("junk.o")), "target/ excluded");
        assert!(!snap.keys().any(|p| p.ends_with("index.js")), "node_modules/ excluded");
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

    // Regression: contains("7b") used to classify 27b models as Small.
    #[test]
    fn twenty_seven_b_model_is_not_small() {
        assert!(matches!(detect_model_tier("gemma2:27b", ""), ModelTier::Large));
        assert!(matches!(detect_model_tier("qwen2.5:72b", ""), ModelTier::Large));
    }

    // gemma4:e4b and its num_ctx derivative must stay Large (decompose-free).
    #[test]
    fn e4b_derivative_is_large() {
        assert!(matches!(detect_model_tier("gemma4:e4b", ""), ModelTier::Large));
        assert!(matches!(detect_model_tier("gemma4-e4b-16k", ""), ModelTier::Large));
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

    // Regression: decompose used to truncate the step description to the
    // first 120 bytes, dropping any verbatim payload past that point — the
    // model then hallucinated the content and verification still passed.
    #[test]
    fn decompose_preserves_full_task_text_in_steps() {
        let payload = "x".repeat(500);
        let task = format!(
            "Overwrite data.js with EXACTLY this content: {payload} and also update app.js"
        );
        let plan = decompose(&task);
        assert_eq!(plan.steps.len(), 2);
        for step in &plan.steps {
            assert!(
                step.description.contains(&payload),
                "step description must carry the full payload"
            );
        }
    }

    // Regression: a single-file task must decompose to ONE step holding the
    // unmodified task text (previously it was truncated to 120 bytes).
    #[test]
    fn decompose_single_file_keeps_task_verbatim() {
        let payload = "y".repeat(400);
        let task = format!("Overwrite data.js with EXACTLY: {payload}");
        let plan = decompose(&task);
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].description, task);
        assert_eq!(plan.steps[0].expected_paths, vec!["data.js".to_string()]);
    }

    // Regression: repeated mentions of the same file (e.g. a verbatim payload
    // quoting its own filename) counted as separate complexity signals and
    // wrongly triggered decomposition of single-target tasks.
    #[test]
    fn complexity_signals_dedup_repeated_file_mentions() {
        // "data.js" three times + short task → 1 signal, stays Simple on small tier
        let p = classify_task(
            "update data.js so data.js exports X; data.js only",
            &default_cfg(), "qwen2.5:7b",
        );
        assert!(matches!(p.complexity, TaskComplexity::Simple));
    }

    #[test]
    fn extract_file_mentions_dedups_and_orders() {
        let files = extract_file_mentions("create data.js and app.js then edit data.js");
        assert_eq!(files, vec!["data.js".to_string(), "app.js".to_string()]);
    }

    // snippet() must never panic on a multibyte boundary (Turkish task text).
    #[test]
    fn snippet_is_char_boundary_safe() {
        let s = "ğ".repeat(100); // 2 bytes each
        let cut = snippet(&s, 121); // byte 121 is mid-char
        assert!(cut.len() <= 121);
        assert!(s.starts_with(cut));
        assert_eq!(snippet("short", 120), "short");
    }

    // ── Append-clobber guard ──────────────────────────────────────────────────

    #[test]
    fn is_append_task_detects_markers() {
        assert!(is_append_task("Append one new const USAGE to data.js"));
        assert!(is_append_task("Add lines after the existing ones, do not remove them"));
        assert!(!is_append_task("Overwrite data.js with EXACTLY this content"));
    }

    #[test]
    fn lost_content_detected_when_file_clobbered() {
        let dir = tempdir().unwrap();
        write_file(dir.path(), "data.js", "const SERVICES = [1];\n");
        let before = snapshot_expected_contents(dir.path(), &["data.js".to_string()]);
        assert_eq!(before.len(), 1);

        // Simulate the model replacing instead of appending
        write_file(dir.path(), "data.js", "const USAGE = [2];\n");
        assert_eq!(lost_content_files(dir.path(), &before), vec!["data.js".to_string()]);

        // A true append keeps the original content
        write_file(dir.path(), "data.js", "const SERVICES = [1];\nconst USAGE = [2];\n");
        assert!(lost_content_files(dir.path(), &before).is_empty());
    }

    #[test]
    fn snapshot_expected_contents_skips_missing_files() {
        let dir = tempdir().unwrap();
        let before = snapshot_expected_contents(dir.path(), &["nope.js".to_string()]);
        assert!(before.is_empty());
        // and nothing captured → nothing can be "lost"
        assert!(lost_content_files(dir.path(), &before).is_empty());
    }

    #[test]
    fn content_lost_log_string() {
        assert_eq!(
            VerificationFailReason::ContentLost(vec!["data.js".to_string()]).to_log_string(),
            "content_lost:data.js"
        );
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
        let mut cfg = Config::default();
        cfg.reliability.enabled = false;

        // With reliability disabled, run_reliable_session calls run_engine which
        // calls engine::run_session — goose likely absent in CI, so we expect
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

    // ── Task 7: execute_with_guard internal logic tests ──────────────────────
    // Note: execute_with_guard now handles routing internally and calls the real
    // engine. Unit tests for retry/escalation logic are exercised via the
    // disabled path and verify_step tests above. The enabled path requires Goose
    // to be installed; we document that constraint here and test what we can.

    /// Verifies that build_event produces a well-formed ReliabilityEvent.
    #[test]
    fn build_event_produces_correct_fields() {
        let v = VerificationResult::Pass;
        let event = build_event(
            "task-abc", "create src/foo.rs", 0, "single_shot",
            "local(qwen2.5:7b)", 1, 0, &v, "success", "signals=0",
        );
        assert_eq!(event.task_id, "task-abc");
        assert_eq!(event.provider, "local(qwen2.5:7b)");
        assert_eq!(event.attempt, 1);
        assert_eq!(event.exit_code, 0);
        assert_eq!(event.verification, "pass");
        assert_eq!(event.outcome, "success");
        assert_eq!(event.step_index, 0);
        assert_eq!(event.execution_mode, "single_shot");
    }

    #[test]
    fn build_event_fail_reason_encoded_correctly() {
        let v = VerificationResult::Fail(VerificationFailReason::NoDiff);
        let event = build_event(
            "t1", "create foo.rs", 0, "single_shot",
            "cloud(anthropic/claude-3-5-sonnet)", 3, 0, &v, "escalation_fail", "retry exhausted",
        );
        assert_eq!(event.verification, "no_diff");
        assert_eq!(event.outcome, "escalation_fail");
        assert_eq!(event.provider, "cloud(anthropic/claude-3-5-sonnet)");
        assert_eq!(event.attempt, 3);
    }
}
