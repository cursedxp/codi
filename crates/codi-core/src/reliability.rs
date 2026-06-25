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

pub(crate) fn decompose(_task: &str) -> ExecutionPlan { unimplemented!() }

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
}
