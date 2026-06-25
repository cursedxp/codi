# Reliability Layer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a `reliability.rs` module that wraps task execution with complexity classification, optional decomposition, artifact verification, and retry/cloud-escalation — making small local model file generation reliable.

**Architecture:** New `run_reliable_session()` in `reliability.rs` wraps `engine::run_session_mcp()` (unchanged) via a classify → execute → verify → retry/escalate chain. `main.rs` and `mcp.rs` call `run_reliable_session()` instead of the engine directly. Config, signals, and doctor are extended to support visibility.

**Tech Stack:** Rust, serde_json (already in Cargo.toml), tempfile (already in dev-dependencies), std::process::Command for git checks, append-mode file I/O for `.codi/reliability.jsonl`.

## Global Constraints

- `engine.rs` must NOT be modified at any point
- All new logic must be unit-testable independently
- `enabled = false` in `[reliability]` must bypass ALL reliability logic with zero overhead
- Log path must be relative; reject `..` and absolute paths at load time
- Follow existing patterns: `anyhow` errors, serde derive, `#[serde(default, deny_unknown_fields)]`
- Run `cargo test` after every task; all existing tests must remain passing

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/codi-core/src/config.rs` | Modify | Add `ReliabilityConfig` struct + `reliability` field on `Config` |
| `crates/codi-core/src/signals.rs` | Modify | Add `VerificationFail` and `EscalationTriggered` signal variants |
| `crates/codi-core/src/reliability.rs` | Create | All reliability logic: classify, decompose, verify, execute_with_guard, log, public entry point |
| `crates/codi-core/src/doctor.rs` | Modify | Add `CheckId::ReliabilityLog` check |
| `crates/codi-core/src/lib.rs` | Modify | `pub mod reliability;` |
| `crates/codi-cli/src/main.rs` | Modify | Replace `run_session` calls with `run_reliable_session` |
| `crates/codi-core/src/mcp.rs` | Modify | Replace `run_session_mcp` call with `run_reliable_session` |

---

## Task 1: ReliabilityConfig in config.rs

**Files:**
- Modify: `crates/codi-core/src/config.rs`

**Interfaces:**
- Produces: `pub struct ReliabilityConfig` with fields listed below
- Produces: `Config.reliability: ReliabilityConfig`

- [ ] **Step 1: Write failing tests**

Add to the `tests` module at the bottom of `config.rs`:

```rust
#[test]
fn reliability_defaults() {
    let c = Config::default();
    assert!(c.reliability.enabled);
    assert!(c.reliability.verify_artifacts);
    assert_eq!(c.reliability.max_retries, 1);
    assert!(c.reliability.escalate_on_retry_failure);
    assert!(c.reliability.log_events);
    assert_eq!(c.reliability.log_path, ".codi/reliability.jsonl");
    assert!(c.reliability.decompose_threshold.is_none());
    assert!(c.reliability.model_tier.is_empty());
}

#[test]
fn reliability_toml_roundtrip() {
    let c = Config::default();
    let toml = c.to_toml().unwrap();
    let back = Config::from_toml(&toml).unwrap();
    assert_eq!(c.reliability, back.reliability);
}

#[test]
fn reliability_partial_override() {
    let c = Config::from_toml(r#"
[reliability]
enabled = false
max_retries = 0
model_tier = "small"
"#).unwrap();
    assert!(!c.reliability.enabled);
    assert_eq!(c.reliability.max_retries, 0);
    assert_eq!(c.reliability.model_tier, "small");
    assert!(c.reliability.verify_artifacts); // inherits default
    assert_eq!(c.reliability.log_path, ".codi/reliability.jsonl");
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p codi-core reliability_defaults 2>&1 | tail -5
```
Expected: FAIL — `Config` has no `reliability` field yet.

- [ ] **Step 3: Add ReliabilityConfig struct and Config field**

In `config.rs`, after the `SelfImprovementConfig` impl block and before `impl Config`, add:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ReliabilityConfig {
    pub enabled: bool,
    /// If Some, overrides the tier-derived threshold.
    pub decompose_threshold: Option<u32>,
    /// "small" | "medium" | "large" | "" (auto-detect from model name).
    pub model_tier: String,
    pub verify_artifacts: bool,
    pub max_retries: u8,
    pub escalate_on_retry_failure: bool,
    pub log_events: bool,
    /// Relative path from repo root. Must not contain '..' or be absolute.
    pub log_path: String,
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        ReliabilityConfig {
            enabled: true,
            decompose_threshold: None,
            model_tier: String::new(),
            verify_artifacts: true,
            max_retries: 1,
            escalate_on_retry_failure: true,
            log_events: true,
            log_path: ".codi/reliability.jsonl".to_string(),
        }
    }
}
```

Then add `pub reliability: ReliabilityConfig,` to the `Config` struct (after `self_improvement`):

```rust
pub struct Config {
    pub model: ModelConfig,
    pub routing: RoutingConfig,
    pub commands: Commands,
    pub rag: RagConfig,
    pub safety: SafetyConfig,
    pub goose_bin: Option<String>,
    pub self_improvement: SelfImprovementConfig,
    pub reliability: ReliabilityConfig,
}
```

- [ ] **Step 4: Run all tests**

```bash
cargo test -p codi-core 2>&1 | tail -10
```
Expected: All 3 new reliability tests pass; all existing tests still pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/config.rs
git commit -m "feat(config): add ReliabilityConfig with defaults"
```

---

## Task 2: Signal types in signals.rs

**Files:**
- Modify: `crates/codi-core/src/signals.rs`

**Interfaces:**
- Produces: `SignalKind::VerificationFail { task_snippet: String, missing_paths: Vec<String>, reason: String }`
- Produces: `SignalKind::EscalationTriggered { reason: String, escalation_provider: String }`

- [ ] **Step 1: Write failing tests**

Add to `tests` module in `signals.rs`:

```rust
#[test]
fn verification_fail_signal_is_constructible() {
    let s = SignalSet {
        signals: vec![Signal {
            kind: SignalKind::VerificationFail {
                task_snippet: "create src/foo.rs".to_string(),
                missing_paths: vec!["src/foo.rs".to_string()],
                reason: "missing_paths".to_string(),
            },
            severity: 1,
        }],
    };
    assert!(matches!(&s.signals[0].kind, SignalKind::VerificationFail { .. }));
}

#[test]
fn escalation_triggered_signal_is_constructible() {
    let s = SignalSet {
        signals: vec![Signal {
            kind: SignalKind::EscalationTriggered {
                reason: "retry failed".to_string(),
                escalation_provider: "cloud(claude-sonnet-4-6)".to_string(),
            },
            severity: 1,
        }],
    };
    assert!(matches!(&s.signals[0].kind, SignalKind::EscalationTriggered { .. }));
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p codi-core verification_fail_signal escalation_triggered 2>&1 | tail -5
```
Expected: FAIL — variants don't exist yet.

- [ ] **Step 3: Add signal variants to SignalKind**

Extend the `SignalKind` enum in `signals.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SignalKind {
    LintWarning { category: String, detail: String },
    TestFailure { test_name: String, module: String },
    DiffWithoutTest,
    TodoFixme { text: String, file: String, context_radius: usize },
    AgentReliability { exit_code: i32, tool_failures: Vec<String> },
    VerificationFail {
        task_snippet: String,
        missing_paths: Vec<String>,
        reason: String,
    },
    EscalationTriggered {
        reason: String,
        escalation_provider: String,
    },
}
```

- [ ] **Step 4: Run all tests**

```bash
cargo test -p codi-core 2>&1 | tail -5
```
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/signals.rs
git commit -m "feat(signals): add VerificationFail and EscalationTriggered variants"
```

---

## Task 3: reliability.rs — core types, classify_task(), and lib.rs

**Files:**
- Create: `crates/codi-core/src/reliability.rs`
- Modify: `crates/codi-core/src/lib.rs`

**Interfaces:**
- Produces: `pub enum RunContext { Cli, Mcp }`
- Produces: `pub struct ReliabilityOutcome { success: bool, exit_code: i32, execution_mode: String, steps_total: usize, steps_succeeded: usize, decision_reason: String }`
- Produces: `pub struct TaskProfile { pub write_intent: bool, pub complexity: TaskComplexity, pub decision_reason: String }`
- Produces: `pub enum TaskComplexity { Simple, Complex }`
- Produces: `pub enum ModelTier { Small, Medium, Large }`
- Produces: `pub struct ExecutionPlan { pub steps: Vec<TaskStep>, pub decision_reason: String }`
- Produces: `pub struct TaskStep { pub description: String, pub expected_paths: Vec<String> }`
- Produces: `pub fn classify_task(task: &str, cfg: &ReliabilityConfig, model_name: &str) -> TaskProfile`
- Produces: `pub(crate) fn detect_model_tier(model_name: &str, tier_override: &str) -> ModelTier`
- Produces: `pub(crate) fn tier_threshold(tier: &ModelTier) -> u32`

- [ ] **Step 1: Create reliability.rs with stubs and tests**

Create `crates/codi-core/src/reliability.rs`:

```rust
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
```

- [ ] **Step 2: Add pub mod reliability to lib.rs**

In `crates/codi-core/src/lib.rs`, add:

```rust
pub mod reliability;
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p codi-core 2>&1 | tail -15
```
Expected: All classify_task and tier tests pass; stubs for `decompose` and `run_reliable_session` don't panic because they're not called.

- [ ] **Step 4: Commit**

```bash
git add crates/codi-core/src/reliability.rs crates/codi-core/src/lib.rs
git commit -m "feat(reliability): add core types, classify_task, detect_model_tier"
```

---

## Task 4: reliability.rs — decompose()

**Files:**
- Modify: `crates/codi-core/src/reliability.rs`

**Interfaces:**
- Consumes: `TaskStep`, `ExecutionPlan` (defined Task 3)
- Produces: `pub(crate) fn decompose(task: &str) -> ExecutionPlan`

- [ ] **Step 1: Write failing tests**

Add to `tests` module in `reliability.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p codi-core decompose 2>&1 | tail -5
```
Expected: FAIL with `not yet implemented`.

- [ ] **Step 3: Implement decompose()**

Replace the `decompose` stub in `reliability.rs`:

```rust
pub(crate) fn decompose(task: &str) -> ExecutionPlan {
    // Extract words that look like file paths (extension + no leading dot)
    let mut seen = std::collections::HashSet::new();
    let mentioned_files: Vec<String> = task
        .split_whitespace()
        .filter_map(|w| {
            let w = w.trim_matches(|c: char| {
                c == ',' || c == ';' || c == '\'' || c == '"' || c == ')'|| c == '('
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
```

- [ ] **Step 4: Run all tests**

```bash
cargo test -p codi-core 2>&1 | tail -10
```
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/reliability.rs
git commit -m "feat(reliability): add decompose() — rule-based task decomposition"
```

---

## Task 5: reliability.rs — VerificationResult and verify_step()

**Files:**
- Modify: `crates/codi-core/src/reliability.rs`

**Interfaces:**
- Produces: `pub enum VerificationResult { Pass, Fail(VerificationFailReason) }`
- Produces: `pub enum VerificationFailReason { NoDiff, MissingPaths(Vec<String>), NonZeroExit(i32) }`
- Produces: `impl VerificationFailReason { pub fn to_log_string(&self) -> String }`
- Produces: `pub(crate) fn verify_step(step: &TaskStep, profile: &TaskProfile, repo_root: &Path, exit_code: i32) -> VerificationResult`

- [ ] **Step 1: Write failing tests**

Add to `tests` module in `reliability.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p codi-core verify_nonzero verify_read verify_write verify_expected verification_fail 2>&1 | tail -5
```
Expected: FAIL — types not defined yet.

- [ ] **Step 3: Implement VerificationResult types and verify_step()**

Add after the `TaskStep` struct in `reliability.rs`:

```rust
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
```

Add after `decompose()`:

```rust
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

    // Untracked new files (git diff HEAD won't show them)
    let untracked: Vec<String> = std::process::Command::new("git")
        .args(["status", "--short"])
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
```

- [ ] **Step 4: Run all tests**

```bash
cargo test -p codi-core 2>&1 | tail -10
```
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/reliability.rs
git commit -m "feat(reliability): add VerificationResult and verify_step()"
```

---

## Task 6: reliability.rs — ReliabilityEvent and append_reliability_log()

**Files:**
- Modify: `crates/codi-core/src/reliability.rs`

**Interfaces:**
- Produces: `pub struct ReliabilityEvent` (all fields listed below)
- Produces: `pub(crate) fn append_reliability_log(repo_root: &Path, cfg: &ReliabilityConfig, event: &ReliabilityEvent) -> Result<()>`
- Produces: `pub(crate) fn resolve_log_path(repo_root: &Path, log_path: &str) -> Result<PathBuf>`
- Produces: `pub(crate) fn current_timestamp() -> u64`
- Produces: `pub(crate) fn generate_task_id() -> String`

- [ ] **Step 1: Write failing tests**

Add to `tests` module in `reliability.rs`:

```rust
use std::io::Write as _;

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
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p codi-core reliability_event append_log resolve_log 2>&1 | tail -5
```
Expected: FAIL.

- [ ] **Step 3: Implement ReliabilityEvent and log helpers**

Add to `reliability.rs` after `git_changed_files`, before the `tests` module:

```rust
use std::path::PathBuf;

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
    use std::io::Write;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening {}", log_path.display()))?
        .write_all(line.as_bytes())
        .context("writing reliability event")
}
```

Make sure `use std::path::PathBuf;` and `use anyhow::Context;` are at the top of the file (add if missing).

- [ ] **Step 4: Run all tests**

```bash
cargo test -p codi-core 2>&1 | tail -10
```
Expected: All tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/reliability.rs
git commit -m "feat(reliability): add ReliabilityEvent and append_reliability_log()"
```

---

## Task 7: reliability.rs — execute_with_guard() and run_reliable_session()

**Files:**
- Modify: `crates/codi-core/src/reliability.rs`

**Interfaces:**
- Consumes: `engine::run_session_mcp()`, `engine::run_session()`, `engine::SessionMode`, `TaskProfile`, `TaskStep`, `VerificationResult`, `VerificationFailReason`, `ReliabilityEvent`, `append_reliability_log()`, `classify_task()`, `decompose()`, `verify_step()`
- Produces: fully implemented `pub fn run_reliable_session(cfg, task, repo_root, ctx) -> Result<ReliabilityOutcome>`

- [ ] **Step 1: Write compile-time tests**

Add to `tests` module:

```rust
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
```

- [ ] **Step 2: Run tests**

```bash
cargo test -p codi-core reliability_outcome_struct classify_before 2>&1 | tail -5
```
Expected: PASS (ReliabilityOutcome was defined in Task 3 stub; classify_task is implemented).

- [ ] **Step 3: Implement run_engine() helper**

Add to `reliability.rs` after `git_changed_files`:

```rust
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
```

- [ ] **Step 4: Implement execute_with_guard()**

Add after `run_engine`:

```rust
#[allow(clippy::too_many_arguments)]
fn execute_with_guard(
    cfg: &Config,
    step: &TaskStep,
    step_index: usize,
    profile: &TaskProfile,
    execution_mode: &str,
    repo_root: &Path,
    task_id: &str,
    ctx: &RunContext,
) -> Result<()> {
    let provider_str = format!("local({})", cfg.model.local.model);

    // Attempt 1 — local model
    let exit_code = run_engine(cfg, &step.description, repo_root, ctx)?;
    let v1 = verify_step(step, profile, repo_root, exit_code);

    if matches!(v1, VerificationResult::Pass) {
        append_reliability_log(repo_root, &cfg.reliability, &ReliabilityEvent {
            task_id: task_id.to_string(),
            task_snippet: step.description[..step.description.len().min(120)].to_string(),
            step_index, execution_mode: execution_mode.to_string(),
            provider: provider_str, attempt: 1, exit_code,
            verification: "pass".to_string(), outcome: "success".to_string(),
            decision_reason: profile.decision_reason.clone(),
            timestamp: current_timestamp(),
        })?;
        return Ok(());
    }

    let fail1 = match &v1 { VerificationResult::Fail(r) => r.to_log_string(), _ => unreachable!() };
    tracing::warn!(step = step_index, reason = %fail1, "step failed verification");

    // Attempt 2 — local retry with narrowed prompt
    if cfg.reliability.max_retries > 0 {
        let retry_prompt = format!(
            "Previous attempt produced no file changes (reason: {fail1}). \
             Try again and make sure to write the required files. Task: {}",
            step.description
        );
        let retry_exit = run_engine(cfg, &retry_prompt, repo_root, ctx)?;
        let v2 = verify_step(step, profile, repo_root, retry_exit);

        if matches!(v2, VerificationResult::Pass) {
            append_reliability_log(repo_root, &cfg.reliability, &ReliabilityEvent {
                task_id: task_id.to_string(),
                task_snippet: step.description[..step.description.len().min(120)].to_string(),
                step_index, execution_mode: execution_mode.to_string(),
                provider: provider_str, attempt: 2, exit_code: retry_exit,
                verification: "pass".to_string(), outcome: "retry_success".to_string(),
                decision_reason: profile.decision_reason.clone(),
                timestamp: current_timestamp(),
            })?;
            return Ok(());
        }

        let fail2 = match &v2 { VerificationResult::Fail(r) => r.to_log_string(), _ => unreachable!() };
        tracing::warn!(step = step_index, reason = %fail2, "retry failed");

        // Attempt 3 — cloud escalation
        if cfg.reliability.escalate_on_retry_failure && cfg.model.cloud.is_some() {
            let cloud_label = cfg.model.cloud.as_ref()
                .map(|c| format!("cloud({}/{})", c.provider, c.model))
                .unwrap_or_else(|| "cloud".to_string());

            tracing::warn!(step = step_index, provider = %cloud_label, "escalating to cloud");

            let mut cloud_cfg = cfg.clone();
            cloud_cfg.routing.mode = RoutingMode::CloudPreferred;

            let esc_exit = run_engine(&cloud_cfg, &step.description, repo_root, ctx)?;
            let v3 = verify_step(step, profile, repo_root, esc_exit);

            let (v3_str, outcome, ok) = match &v3 {
                VerificationResult::Pass => ("pass".to_string(), "escalation_success".to_string(), true),
                VerificationResult::Fail(r) => (r.to_log_string(), "escalation_fail".to_string(), false),
            };

            append_reliability_log(repo_root, &cfg.reliability, &ReliabilityEvent {
                task_id: task_id.to_string(),
                task_snippet: step.description[..step.description.len().min(120)].to_string(),
                step_index, execution_mode: execution_mode.to_string(),
                provider: cloud_label, attempt: 3, exit_code: esc_exit,
                verification: v3_str, outcome,
                decision_reason: profile.decision_reason.clone(),
                timestamp: current_timestamp(),
            })?;

            if ok { return Ok(()); }
            anyhow::bail!(
                "step {step_index} failed after local retry and cloud escalation \
                 (retry_reason={fail2})"
            );
        }

        // No cloud — log failure and bail
        append_reliability_log(repo_root, &cfg.reliability, &ReliabilityEvent {
            task_id: task_id.to_string(),
            task_snippet: step.description[..step.description.len().min(120)].to_string(),
            step_index, execution_mode: execution_mode.to_string(),
            provider: provider_str, attempt: 2, exit_code: retry_exit,
            verification: fail2.clone(), outcome: "fail".to_string(),
            decision_reason: profile.decision_reason.clone(),
            timestamp: current_timestamp(),
        })?;
        anyhow::bail!("step {step_index} failed after retry (reason={fail2})");
    }

    // max_retries = 0: log first failure and bail
    append_reliability_log(repo_root, &cfg.reliability, &ReliabilityEvent {
        task_id: task_id.to_string(),
        task_snippet: step.description[..step.description.len().min(120)].to_string(),
        step_index, execution_mode: execution_mode.to_string(),
        provider: provider_str, attempt: 1, exit_code,
        verification: fail1.clone(), outcome: "fail".to_string(),
        decision_reason: profile.decision_reason.clone(),
        timestamp: current_timestamp(),
    })?;
    anyhow::bail!("step {step_index} failed (no retries): {fail1}");
}
```

- [ ] **Step 5: Implement run_reliable_session()**

Replace the `run_reliable_session` stub:

```rust
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
            vec![TaskStep { description: task.to_string(), expected_paths: vec![] }],
        ),
        TaskComplexity::Complex => {
            let plan = decompose(task);
            tracing::info!(steps = plan.steps.len(), reason = %plan.decision_reason, "task decomposed");
            ("decomposed".to_string(), plan.steps)
        }
    };

    let steps_total = steps.len();
    let mut steps_succeeded = 0usize;

    for (i, step) in steps.iter().enumerate() {
        match execute_with_guard(cfg, step, i, &profile, &execution_mode, repo_root, &task_id, &ctx) {
            Ok(()) => steps_succeeded += 1,
            Err(e) => {
                tracing::error!(step = i, error = %e, "step failed");
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
```

- [ ] **Step 6: Run all tests**

```bash
cargo test -p codi-core 2>&1 | tail -10
```
Expected: All tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/codi-core/src/reliability.rs
git commit -m "feat(reliability): add execute_with_guard() and run_reliable_session()"
```

---

## Task 8: doctor.rs — ReliabilityLog check

**Files:**
- Modify: `crates/codi-core/src/doctor.rs`

**Interfaces:**
- Consumes: `.codi/reliability.jsonl` (JSONL, each line a JSON object with `outcome` and `verification` fields)
- Produces: `CheckId::ReliabilityLog` variant + `check_reliability_log(repo_root) -> CheckResult`

- [ ] **Step 1: Write failing tests**

Add to `tests` module in `doctor.rs`:

```rust
#[test]
fn reliability_log_missing_returns_info() {
    let dir = tempdir().unwrap();
    let checks = run_doctor(dir.path()).unwrap();
    let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
    assert!(matches!(c.severity, Severity::Info));
}

#[test]
fn reliability_log_all_success_returns_ok() {
    let dir = tempdir().unwrap();
    let log_dir = dir.path().join(".codi");
    std::fs::create_dir_all(&log_dir).unwrap();
    let log_path = log_dir.join("reliability.jsonl");
    let success_line = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"pass","outcome":"success","decision_reason":"ok","timestamp":1}"#;
    use std::io::Write as _;
    for _ in 0..5 {
        writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{success_line}").unwrap();
    }
    let checks = run_doctor(dir.path()).unwrap();
    let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
    assert!(matches!(c.severity, Severity::Ok), "detail: {}", c.detail);
}

#[test]
fn reliability_log_silent_failures_returns_error() {
    let dir = tempdir().unwrap();
    let log_dir = dir.path().join(".codi");
    std::fs::create_dir_all(&log_dir).unwrap();
    let log_path = log_dir.join("reliability.jsonl");
    let success = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"pass","outcome":"success","decision_reason":"ok","timestamp":1}"#;
    let fail   = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"no_diff","outcome":"fail","decision_reason":"ok","timestamp":1}"#;
    use std::io::Write as _;
    for line in [success, success, fail, fail, fail] {
        writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{line}").unwrap();
    }
    let checks = run_doctor(dir.path()).unwrap();
    let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
    assert!(matches!(c.severity, Severity::Error), "detail: {}", c.detail);
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p codi-core reliability_log 2>&1 | tail -5
```
Expected: FAIL — `CheckId::ReliabilityLog` not defined.

- [ ] **Step 3: Add CheckId variant**

Add `ReliabilityLog` to the `CheckId` enum in `doctor.rs`:

```rust
pub enum CheckId {
    CodiToml,
    Ollama,
    Model,
    McpJson,
    McpRegistration,
    SelfImprovement,
    ClaudeMd,
    ReliabilityLog,
}
```

- [ ] **Step 4: Add check call to run_doctor()**

At the end of `run_doctor()`, before `Ok(checks)`, add:

```rust
// [8] reliability log
checks.push(check_reliability_log(repo_root));
```

- [ ] **Step 5: Implement check_reliability_log()**

Add before the `tests` module in `doctor.rs`:

```rust
fn check_reliability_log(repo_root: &Path) -> CheckResult {
    let log_path = repo_root.join(".codi/reliability.jsonl");

    if !log_path.exists() {
        return CheckResult {
            id: CheckId::ReliabilityLog,
            name: "reliability",
            severity: Severity::Info,
            detail: "henüz log yok — reliability katmanı henüz çalışmadı".to_string(),
            suggestion: None,
            fixable: false,
        };
    }

    let content = std::fs::read_to_string(&log_path).unwrap_or_default();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    let last_20: Vec<&serde_json::Value> = events.iter().rev().take(20).collect();
    let total = last_20.len();

    if total == 0 {
        return CheckResult {
            id: CheckId::ReliabilityLog,
            name: "reliability",
            severity: Severity::Info,
            detail: "log dosyası boş".to_string(),
            suggestion: None,
            fixable: false,
        };
    }

    let succeeded = last_20.iter().filter(|e| {
        matches!(
            e.get("outcome").and_then(|v| v.as_str()).unwrap_or(""),
            "success" | "retry_success" | "escalation_success"
        )
    }).count();

    let silent_failures = last_20.iter().filter(|e| {
        e.get("verification").and_then(|v| v.as_str()).unwrap_or("").contains("no_diff")
    }).count();

    let escalations = last_20.iter().filter(|e| {
        e.get("outcome").and_then(|v| v.as_str()).unwrap_or("").contains("escalation")
    }).count();

    let success_pct = (succeeded * 100) / total;
    let detail = format!(
        "%{success_pct} başarı ({succeeded}/{total}), {silent_failures} sessiz başarısızlık, {escalations} eskalasyon"
    );

    let severity = if silent_failures > 0 || success_pct < 70 {
        Severity::Error
    } else if success_pct < 90 {
        Severity::Warning
    } else {
        Severity::Ok
    };

    CheckResult {
        id: CheckId::ReliabilityLog,
        name: "reliability",
        severity,
        detail,
        suggestion: if matches!(severity, Severity::Ok) {
            None
        } else {
            Some("cat .codi/reliability.jsonl | jq .".to_string())
        },
        fixable: false,
    }
}
```

- [ ] **Step 6: Run all tests**

```bash
cargo test -p codi-core 2>&1 | tail -10
```
Expected: All tests pass including all 3 new reliability_log tests.

- [ ] **Step 7: Commit**

```bash
git add crates/codi-core/src/doctor.rs
git commit -m "feat(doctor): add ReliabilityLog check from .codi/reliability.jsonl"
```

---

## Task 9: Wire up main.rs and mcp.rs

**Files:**
- Modify: `crates/codi-cli/src/main.rs`
- Modify: `crates/codi-core/src/mcp.rs`

**Interfaces:**
- Consumes: `codi_core::reliability::{run_reliable_session, RunContext, ReliabilityOutcome}` (Task 7)

- [ ] **Step 1: Update main.rs imports**

In `crates/codi-cli/src/main.rs`, add to the `use codi_core` block:

```rust
use codi_core::reliability::{run_reliable_session, RunContext};
```

- [ ] **Step 2: Replace run_session in cmd_run()**

Find in `main.rs`:

```rust
fn cmd_run(cfg: &Config, repo_root: &std::path::Path, task: &str, review: bool) -> Result<()> {
    println!("Provider: {}", pick_provider_label(cfg, task));
    let code = run_session(
        cfg,
        task,
        SessionMode::OneShot(task.to_string()),
        None,
        repo_root,
        "",
    )?;
    if code != 0 {
        eprintln!("goose exited with code {code}");
    }
```

Replace with:

```rust
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
```

- [ ] **Step 3: Replace run_session in run_interactive()**

Find in `main.rs`:

```rust
        let code =
            run_session(cfg, task, SessionMode::OneShot(task.to_string()), None, repo_root, "")?;
        if code != 0 {
            eprintln!("goose exited with code {code}");
        }
```

Replace with:

```rust
        let outcome = run_reliable_session(cfg, task, repo_root, RunContext::Cli)?;
        let code = outcome.exit_code;
        if !outcome.success {
            eprintln!("task failed ({})", outcome.decision_reason);
        }
```

- [ ] **Step 4: Clean up unused imports in main.rs**

```bash
cargo build -p codi-cli 2>&1 | grep "unused import"
```

Remove `run_session` and `SessionMode` from the `use codi_core::engine::` import line if the compiler warns. The line should look like:

```rust
use codi_core::engine::{pick_provider_label, post_run_hook};
```

(Keep `pick_provider_label` and `post_run_hook` — they're still used.)

- [ ] **Step 5: Update mcp.rs**

In `crates/codi-core/src/mcp.rs`, add import:

```rust
use crate::reliability::{run_reliable_session, RunContext};
```

Find `tool_run_task`:

```rust
fn tool_run_task(cfg: &Config, repo_root: &Path, args: &Value) -> Result<Value> {
    let task = args["task"].as_str().context("missing 'task' argument")?;

    let exit_code = run_session_mcp(cfg, task, None, repo_root, "")?;

    let message = if exit_code == 0 {
        "Task complete. Call get_diff to review the changes.".to_string()
    } else {
        format!("Agent exited with code {exit_code}. Check terminal output for details.")
    };

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": format!("{message}\nexit_code: {exit_code}") }]
    }))
}
```

Replace with:

```rust
fn tool_run_task(cfg: &Config, repo_root: &Path, args: &Value) -> Result<Value> {
    let task = args["task"].as_str().context("missing 'task' argument")?;

    let outcome = run_reliable_session(cfg, task, repo_root, RunContext::Mcp)?;

    let message = if outcome.success {
        format!(
            "Task complete ({} step(s), mode={}). Call get_diff to review the changes.",
            outcome.steps_total, outcome.execution_mode
        )
    } else {
        format!(
            "Task failed ({}/{} steps, mode={}, reason={}). Check terminal output for details.",
            outcome.steps_succeeded, outcome.steps_total,
            outcome.execution_mode, outcome.decision_reason
        )
    };

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": format!("{message}\nexit_code: {}", outcome.exit_code) }]
    }))
}
```

- [ ] **Step 6: Clean up unused imports in mcp.rs**

```bash
cargo build -p codi-core 2>&1 | grep "unused import"
```

Remove `use crate::engine::run_session_mcp;` if no longer used directly.

- [ ] **Step 7: Build and run all tests**

```bash
cargo build 2>&1 | tail -5
```
Expected: Compiles cleanly, no warnings about unused imports.

```bash
cargo test 2>&1 | tail -10
```
Expected: All tests pass across all crates.

- [ ] **Step 8: Commit**

```bash
git add crates/codi-cli/src/main.rs crates/codi-core/src/mcp.rs
git commit -m "feat(wiring): call run_reliable_session from main.rs and mcp.rs"
```

---

## Self-Review

**Spec coverage:**

| Requirement | Task |
|-------------|------|
| classify_task() write_intent + complexity | 3 |
| Model tier detection + config override | 3 |
| decompose() — rule-based, deterministic, no model call | 4 |
| verify_step() — aggressive write, conservative read | 5 |
| VerificationResult enum, string-serialized to log | 5 + 6 |
| decision_reason in TaskProfile + ReliabilityEvent | 3 + 6 |
| ReliabilityEvent + append_reliability_log() | 6 |
| log_path: relative only, reject `..` and absolute | 6 |
| execute_with_guard: retry → cloud escalation → explicit fail | 7 |
| run_reliable_session: enabled=false fast path | 7 |
| CheckId::ReliabilityLog in doctor | 8 |
| doctor: success %, silent failure count, escalation count | 8 |
| main.rs wiring (cmd_run + run_interactive) | 9 |
| mcp.rs wiring (tool_run_task) | 9 |
| engine.rs untouched | all — never modified |
| signals.rs VerificationFail + EscalationTriggered | 2 |
| lib.rs pub mod reliability | 3 |

**Type consistency:**
- `TaskProfile.write_intent: bool` → used in `verify_step` (Task 5) ✓
- `TaskComplexity::Simple / Complex` → matched in `run_reliable_session` (Task 7) ✓
- `VerificationFailReason.to_log_string()` → used in `execute_with_guard` (Task 7) ✓
- `ReliabilityEvent` field names → consistent between Task 6 (definition) and Task 8 (doctor JSON parsing by string key) ✓
- `RunContext::Cli / Mcp` → matched in `run_engine` (Task 7) and wired in Task 9 ✓

**No placeholders found.** All code blocks contain actual Rust. ✓

**One implementation note:** `git_changed_files` (Task 5) captures both tracked modifications (`git diff HEAD --name-only`) and untracked new files (`git status --short` with `??` prefix) — this handles the case where Goose writes new files without staging them.
