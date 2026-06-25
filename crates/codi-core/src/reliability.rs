//! Reliability layer: task classification, decomposition, verification,
//! retry and escalation for small local model execution.

use std::path::Path;

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

pub fn run_reliable_session(
    _cfg: &Config,
    _task: &str,
    _repo_root: &Path,
    _ctx: RunContext,
) -> Result<ReliabilityOutcome> {
    unimplemented!()
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
}
