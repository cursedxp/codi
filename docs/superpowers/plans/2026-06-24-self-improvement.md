# Kodi Self-Improvement Layer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an event-driven self-improvement layer that collects signals after each `codi run`, classifies them as Low/High risk, auto-applies Low-risk improvements on isolated git branches with mandatory test+lint gates, and queues High-risk candidates for Claude approval via three new MCP tools.

**Architecture:** A `post_run_hook()` added to `engine.rs` is called from `cmd_run` in `main.rs` after each one-shot session. It runs signal collection (lint warnings, diff stats, test-gap detection), risk classification, and dispatches Low-risk candidates to `ImprovementExecutor` (branch → Goose → test → commit/rollback) and High-risk ones to the `PendingQueue`. Three new MCP tools let Claude read and act on the pending queue.

**Tech Stack:** Rust 1.82, `anyhow`, `serde`/`serde_json`, `std::process::Command` for git/cargo subprocesses; existing `run_session_mcp` for Goose invocation; no new crate dependencies.

## Global Constraints

- Rust edition 2021, rust-version 1.82 — no new crates beyond the workspace
- IDs generated from `SystemTime` — no `uuid` or `rand` crates
- `SelfImprovementConfig` uses `#[serde(default)]` without `deny_unknown_fields` so future config fields don't break existing installs
- `post_run_hook` fires only in `Cmd::Run` — not in the interactive REPL, MCP server mode, or `codi review`
- Auto-improvement branches always use the `branch_prefix` from config
- Rollback always checks out the original branch **before** deleting the improvement branch
- Test + lint gate is mandatory even for Claude-approved (`approve_improvement`) items
- `.codi/pending_improvements.json` = inbox only — items removed (not status-changed) on approve or dismiss
- `.codi/improvement_log.jsonl` = append-only history — never truncated

---

## File Map

| Action | Path | Responsibility |
|---|---|---|
| Modify | `crates/codi-core/src/config.rs` | Add `SelfImprovementConfig`; add field to `Config` |
| Create | `crates/codi-core/src/signals.rs` | `SignalKind`, `Signal`, `SignalSet`, `collect_signals()`, `parse_diff_line_count()` |
| Create | `crates/codi-core/src/risk.rs` | `RiskLevel`, `ImprovementCandidate`, `classify()` |
| Create | `crates/codi-core/src/pending.rs` | `PendingQueue` — atomic JSON inbox persistence |
| Create | `crates/codi-core/src/improve.rs` | `ImprovementExecutor`, `Outcome`, `LogEntry`, git helpers, `append_log()` |
| Modify | `crates/codi-core/src/engine.rs` | Add `post_run_hook()`, `git_changed_files()`, `run_clippy_capture()` |
| Modify | `crates/codi-core/src/mcp.rs` | Add 3 new MCP tools to `tools/list` and dispatch |
| Modify | `crates/codi-core/src/lib.rs` | Export new modules (one per task, progressively) |
| Modify | `crates/codi-cli/src/main.rs` | Call `post_run_hook` after `run_session` in `cmd_run` |
| Modify | `codi.toml` | Add `[self_improvement]` example section |

---

### Task 1: `SelfImprovementConfig`

**Files:**
- Modify: `crates/codi-core/src/config.rs`
- Modify: `codi.toml`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub struct SelfImprovementConfig` with `Default`
  - `Config.self_improvement: SelfImprovementConfig`

- [ ] **Step 1: Write the failing tests**

  Add inside the existing `#[cfg(test)] mod tests` block in `config.rs`:

```rust
#[test]
fn self_improvement_defaults() {
    let c = Config::default();
    assert!(c.self_improvement.enabled);
    assert!(c.self_improvement.auto_apply_low_risk);
    assert_eq!(c.self_improvement.max_auto_per_run, 2);
    assert_eq!(c.self_improvement.max_diff_lines, 300);
    assert_eq!(c.self_improvement.branch_prefix, "improve");
    assert_eq!(c.self_improvement.blocklist.len(), 4);
}

#[test]
fn self_improvement_toml_roundtrip() {
    let c = Config::default();
    let toml = c.to_toml().unwrap();
    let back = Config::from_toml(&toml).unwrap();
    assert_eq!(c.self_improvement, back.self_improvement);
}

#[test]
fn self_improvement_partial_override() {
    let c = Config::from_toml(r#"
[self_improvement]
enabled = false
max_auto_per_run = 5
"#).unwrap();
    assert!(!c.self_improvement.enabled);
    assert_eq!(c.self_improvement.max_auto_per_run, 5);
    // unset field inherits default
    assert_eq!(c.self_improvement.branch_prefix, "improve");
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p codi-core self_improvement
```

Expected: compile error — `no field 'self_improvement' on type 'Config'`

- [ ] **Step 3: Implement `SelfImprovementConfig`**

  Add before `impl Config` in `config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct SelfImprovementConfig {
    pub enabled: bool,
    pub auto_apply_low_risk: bool,
    pub max_auto_per_run: usize,
    pub max_diff_lines: usize,
    pub branch_prefix: String,
    pub blocklist: Vec<String>,
}

impl Default for SelfImprovementConfig {
    fn default() -> Self {
        SelfImprovementConfig {
            enabled: true,
            auto_apply_low_risk: true,
            max_auto_per_run: 2,
            max_diff_lines: 300,
            branch_prefix: "improve".to_string(),
            blocklist: vec![
                "crates/codi-core/src/routing.rs".to_string(),
                "crates/codi-core/src/mcp.rs".to_string(),
                "crates/codi-core/src/engine.rs".to_string(),
                "crates/codi-core/src/config.rs".to_string(),
            ],
        }
    }
}
```

  Add the field to `Config` struct:

```rust
pub struct Config {
    pub model: ModelConfig,
    pub routing: RoutingConfig,
    pub commands: Commands,
    pub rag: RagConfig,
    pub safety: SafetyConfig,
    pub goose_bin: Option<String>,
    pub self_improvement: SelfImprovementConfig,  // ← new
}
```

  Add the field to `Config`'s `Default::default()`:

```rust
impl Default for Config {
    fn default() -> Self {
        Config {
            model: ModelConfig::default(),
            routing: RoutingConfig::default(),
            commands: Commands::default(),
            rag: RagConfig::default(),
            safety: SafetyConfig::default(),
            goose_bin: None,
            self_improvement: SelfImprovementConfig::default(),  // ← new
        }
    }
}
```

- [ ] **Step 4: Add `[self_improvement]` example to `codi.toml`**

  Append at the end of `codi.toml`:

```toml
[self_improvement]
enabled             = true
auto_apply_low_risk = true    # false → all candidates go to pending queue for review
max_auto_per_run    = 2       # max auto-improvements triggered per codi run
max_diff_lines      = 300     # auto-improvements producing larger diffs are rolled back
branch_prefix       = "improve"

# Files that are never auto-modified — always require Claude review/approval
blocklist = [
    "crates/codi-core/src/routing.rs",
    "crates/codi-core/src/mcp.rs",
    "crates/codi-core/src/engine.rs",
    "crates/codi-core/src/config.rs",
]
```

- [ ] **Step 5: Run all tests in codi-core**

```bash
cargo test -p codi-core
```

Expected: all existing tests still pass plus the 3 new `self_improvement_*` tests.

- [ ] **Step 6: Commit**

```bash
git add crates/codi-core/src/config.rs codi.toml
git commit -m "feat(config): add SelfImprovementConfig with defaults and TOML support"
```

---

### Task 2: Signal Types and Collector (`signals.rs`)

**Files:**
- Create: `crates/codi-core/src/signals.rs`
- Modify: `crates/codi-core/src/lib.rs` — add `pub mod signals;`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub enum SignalKind { LintWarning { category, detail }, TestFailure { test_name, module }, DiffWithoutTest, TodoFixme { text, file, context_radius }, AgentReliability { exit_code, tool_failures } }`
  - `pub struct Signal { pub kind: SignalKind }`
  - `pub struct SignalSet { pub signals: Vec<Signal> }`
  - `pub fn collect_signals(repo_root: &Path, clippy_output: &str, diff_changed_files: &[String], goose_exit_code: i32) -> SignalSet`
  - `pub fn parse_diff_line_count(shortstat: &str) -> usize`

- [ ] **Step 1: Write the failing tests**

  Create `crates/codi-core/src/signals.rs` containing only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn empty_inputs_produce_no_signals() {
        let s = collect_signals(Path::new("/tmp"), "", &[], 0);
        assert!(s.signals.is_empty());
    }

    #[test]
    fn clippy_warning_parsed() {
        let clippy = "crates/codi-core/src/routing.rs:45:5: warning: function `is_complex` has a cognitive complexity of 25 [clippy::cognitive_complexity]";
        let s = collect_signals(Path::new("/tmp"), clippy, &[], 0);
        let warnings: Vec<_> = s.signals.iter()
            .filter(|sig| matches!(&sig.kind, SignalKind::LintWarning { .. }))
            .collect();
        assert_eq!(warnings.len(), 1);
        if let SignalKind::LintWarning { detail, .. } = &warnings[0].kind {
            assert!(detail.contains("cognitive_complexity"));
        } else {
            panic!("expected LintWarning");
        }
    }

    #[test]
    fn diff_without_test_fires_when_no_test_file_changed() {
        let changed = vec!["crates/codi-core/src/routing.rs".to_string()];
        let s = collect_signals(Path::new("/tmp"), "", &changed, 0);
        assert!(s.signals.iter().any(|sig| matches!(sig.kind, SignalKind::DiffWithoutTest)));
    }

    #[test]
    fn diff_without_test_does_not_fire_when_test_file_changed() {
        let changed = vec![
            "crates/codi-core/src/routing.rs".to_string(),
            "crates/codi-core/tests/integration_engine.rs".to_string(),
        ];
        let s = collect_signals(Path::new("/tmp"), "", &changed, 0);
        assert!(!s.signals.iter().any(|sig| matches!(sig.kind, SignalKind::DiffWithoutTest)));
    }

    #[test]
    fn nonzero_exit_code_produces_agent_reliability_signal() {
        let s = collect_signals(Path::new("/tmp"), "", &[], 1);
        assert!(s.signals.iter().any(|sig| {
            matches!(&sig.kind, SignalKind::AgentReliability { exit_code, .. } if *exit_code == 1)
        }));
    }

    #[test]
    fn parse_diff_line_count_insertions_and_deletions() {
        assert_eq!(parse_diff_line_count(" 2 files changed, 18 insertions(+), 8 deletions(-)"), 26);
    }

    #[test]
    fn parse_diff_line_count_insertions_only() {
        assert_eq!(parse_diff_line_count(" 1 file changed, 5 insertions(+)"), 5);
    }

    #[test]
    fn parse_diff_line_count_empty() {
        assert_eq!(parse_diff_line_count(""), 0);
    }
}
```

- [ ] **Step 2: Add `pub mod signals;` to `lib.rs` and run to see tests fail**

  Add to `crates/codi-core/src/lib.rs`:
```rust
pub mod signals;
```

```bash
cargo test -p codi-core signals
```

Expected: compile error — items not yet defined.

- [ ] **Step 3: Implement `signals.rs`**

  Replace the file contents with the full implementation:

```rust
//! Signal collection from post-run artifacts.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SignalKind {
    LintWarning { category: String, detail: String },
    TestFailure  { test_name: String, module: String },
    DiffWithoutTest,
    /// context_radius: how many neighbouring modules to scan (0 = changed files only).
    TodoFixme    { text: String, file: String, context_radius: usize },
    /// Separate from code-quality signals — tracks agent execution health.
    AgentReliability { exit_code: i32, tool_failures: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub kind: SignalKind,
}

#[derive(Debug, Default)]
pub struct SignalSet {
    pub signals: Vec<Signal>,
}

impl SignalSet {
    pub fn push(&mut self, kind: SignalKind) {
        self.signals.push(Signal { kind });
    }
}

/// Collect signals from a completed Goose session.
///
/// `clippy_output` is the captured stderr of `cargo clippy --message-format=short`.
/// `diff_changed_files` is output of `git diff --name-only HEAD` split into lines.
/// `goose_exit_code` is the exit code returned by `run_session_mcp`.
pub fn collect_signals(
    _repo_root: &Path,
    clippy_output: &str,
    diff_changed_files: &[String],
    goose_exit_code: i32,
) -> SignalSet {
    let mut set = SignalSet::default();

    // Lint warnings: parse clippy short format — "path:line:col: warning: detail [lint]"
    for line in clippy_output.lines() {
        if let Some(idx) = line.find(": warning: ") {
            let detail = line[idx + ": warning: ".len()..].to_string();
            let category = detail
                .rfind('[')
                .and_then(|s| detail[s..].rfind(']').map(|e| detail[s + 1..s + e].to_string()))
                .unwrap_or_else(|| "clippy".to_string());
            set.push(SignalKind::LintWarning { category, detail });
        }
    }

    // Test ↔ diff overlap: fires when changed files contain no test files.
    if !diff_changed_files.is_empty() {
        let has_test = diff_changed_files.iter().any(|f| is_test_file(f));
        if !has_test {
            set.push(SignalKind::DiffWithoutTest);
        }
    }

    // Agent reliability: non-zero exit code.
    if goose_exit_code != 0 {
        set.push(SignalKind::AgentReliability {
            exit_code: goose_exit_code,
            tool_failures: vec![],
        });
    }

    set
}

/// Returns true if `path` is a test file.
fn is_test_file(path: &str) -> bool {
    path.contains("/tests/")
        || path.contains("_test.rs")
        || path.contains("_tests.rs")
        || path.ends_with("test.rs")
}

/// Parse total changed lines from `git diff --shortstat` output.
/// Example input: " 2 files changed, 18 insertions(+), 8 deletions(-)"
pub fn parse_diff_line_count(shortstat: &str) -> usize {
    let mut total = 0usize;
    for part in shortstat.split(',') {
        let part = part.trim();
        if part.contains("insertion") || part.contains("deletion") {
            if let Some(n) = part.split_whitespace().next() {
                total += n.parse::<usize>().unwrap_or(0);
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    // test code from Step 1
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p codi-core signals
```

Expected: all 8 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/signals.rs crates/codi-core/src/lib.rs
git commit -m "feat(signals): add signal types and collect_signals()"
```

---

### Task 3: Risk Classifier (`risk.rs`)

**Files:**
- Create: `crates/codi-core/src/risk.rs`
- Modify: `crates/codi-core/src/lib.rs` — add `pub mod risk;`

**Interfaces:**
- Consumes: `signals::{Signal, SignalKind, SignalSet}`, `config::SelfImprovementConfig`
- Produces:
  - `pub enum RiskLevel { Low, High }` — Medium reserved, never produced in v1
  - `pub struct ImprovementCandidate { id, description, risk, risk_reason, source_signals, context, created_at }`
  - `pub fn classify(signals: &SignalSet, cfg: &SelfImprovementConfig, changed_files: &[String]) -> Vec<ImprovementCandidate>`

- [ ] **Step 1: Write the failing tests**

  Create `crates/codi-core/src/risk.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SelfImprovementConfig;
    use crate::signals::{Signal, SignalKind, SignalSet};

    fn default_cfg() -> SelfImprovementConfig { SelfImprovementConfig::default() }

    fn set_of(kinds: Vec<SignalKind>) -> SignalSet {
        SignalSet { signals: kinds.into_iter().map(|kind| Signal { kind }).collect() }
    }

    #[test]
    fn empty_signals_produce_no_candidates() {
        let c = classify(&SignalSet::default(), &default_cfg(), &[]);
        assert!(c.is_empty());
    }

    #[test]
    fn lint_warning_in_non_blocklist_file_is_low_risk() {
        let set = set_of(vec![SignalKind::LintWarning {
            category: "clippy::unused_variable".to_string(),
            detail: "unused variable `x` [clippy::unused_variable]".to_string(),
        }]);
        let candidates = classify(&set, &default_cfg(), &["crates/codi-core/src/review.rs".to_string()]);
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].risk, RiskLevel::Low));
    }

    #[test]
    fn blocklist_file_promotes_to_high_risk() {
        let set = set_of(vec![SignalKind::LintWarning {
            category: "clippy".to_string(),
            detail: "complex function [clippy::cognitive_complexity]".to_string(),
        }]);
        let candidates = classify(&set, &default_cfg(), &["crates/codi-core/src/routing.rs".to_string()]);
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].risk, RiskLevel::High));
        assert!(candidates[0].risk_reason.contains("blocklist"));
    }

    #[test]
    fn diff_without_test_is_always_high_risk() {
        let set = set_of(vec![SignalKind::DiffWithoutTest]);
        let candidates = classify(&set, &default_cfg(), &["crates/codi-core/src/review.rs".to_string()]);
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].risk, RiskLevel::High));
        assert!(candidates[0].risk_reason.contains("no test"));
    }

    #[test]
    fn high_risk_keyword_in_lint_detail_promotes_to_high() {
        let set = set_of(vec![SignalKind::LintWarning {
            category: "clippy".to_string(),
            detail: "security: avoid unsafe block".to_string(),
        }]);
        let candidates = classify(&set, &default_cfg(), &[]);
        assert!(matches!(candidates[0].risk, RiskLevel::High));
        assert!(candidates[0].risk_reason.contains("keyword"));
    }

    #[test]
    fn agent_reliability_failure_is_high_risk() {
        let set = set_of(vec![SignalKind::AgentReliability {
            exit_code: 1,
            tool_failures: vec!["write_file".to_string()],
        }]);
        let candidates = classify(&set, &default_cfg(), &[]);
        assert!(!candidates.is_empty());
        assert!(matches!(candidates[0].risk, RiskLevel::High));
    }

    #[test]
    fn candidate_ids_are_unique() {
        let set = set_of(vec![
            SignalKind::LintWarning { category: "a".to_string(), detail: "warn1".to_string() },
            SignalKind::LintWarning { category: "b".to_string(), detail: "warn2".to_string() },
        ]);
        let candidates = classify(&set, &default_cfg(), &[]);
        let ids: std::collections::HashSet<_> = candidates.iter().map(|c| &c.id).collect();
        assert_eq!(ids.len(), candidates.len());
    }
}
```

- [ ] **Step 2: Add `pub mod risk;` to `lib.rs` and run to see tests fail**

```bash
cargo test -p codi-core risk
```

- [ ] **Step 3: Implement `risk.rs`**

```rust
//! Risk classification for self-improvement candidates.

use serde::{Deserialize, Serialize};

use crate::config::SelfImprovementConfig;
use crate::signals::{Signal, SignalKind, SignalSet};

const HIGH_RISK_KEYWORDS: &[&str] = &[
    "security", "architecture", "api", "breaking", "migration",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RiskLevel {
    Low,
    High,
    // Medium is reserved for a future version — never produced in v1.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImprovementCandidate {
    pub id: String,
    pub description: String,
    pub risk: RiskLevel,
    pub risk_reason: String,
    pub source_signals: Vec<Signal>,
    /// Comma-separated file paths relevant to this candidate.
    pub context: String,
    pub created_at: u64,
}

/// Classify signals into improvement candidates.
/// Returns an empty vec when `cfg.enabled` is false or no signals are actionable.
pub fn classify(
    signals: &SignalSet,
    cfg: &SelfImprovementConfig,
    changed_files: &[String],
) -> Vec<ImprovementCandidate> {
    if !cfg.enabled || signals.signals.is_empty() {
        return vec![];
    }
    let context = changed_files.join(", ");
    signals.signals.iter().enumerate()
        .filter_map(|(i, signal)| signal_to_candidate(signal, cfg, &context, i))
        .collect()
}

fn signal_to_candidate(
    signal: &Signal,
    cfg: &SelfImprovementConfig,
    context: &str,
    index: usize,
) -> Option<ImprovementCandidate> {
    let id = generate_id(index);
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    match &signal.kind {
        SignalKind::LintWarning { detail, .. } => {
            let (risk, risk_reason) = lint_risk(detail, context, cfg);
            Some(ImprovementCandidate {
                id, description: format!("Fix clippy warning: {detail}"),
                risk, risk_reason,
                source_signals: vec![signal.clone()],
                context: context.to_string(), created_at,
            })
        }

        SignalKind::DiffWithoutTest => Some(ImprovementCandidate {
            id,
            description: format!("Add unit tests for recently changed code: {context}"),
            risk: RiskLevel::High,
            risk_reason: "no test file in diff — auto-apply blocked; needs review".to_string(),
            source_signals: vec![signal.clone()],
            context: context.to_string(), created_at,
        }),

        SignalKind::AgentReliability { exit_code, tool_failures } => {
            let description = if tool_failures.is_empty() {
                format!("Investigate agent reliability issue (exit_code={exit_code})")
            } else {
                format!("Investigate agent tool failures: {}", tool_failures.join(", "))
            };
            Some(ImprovementCandidate {
                id, description,
                risk: RiskLevel::High,
                risk_reason: format!("agent reliability signal: exit_code={exit_code}"),
                source_signals: vec![signal.clone()],
                context: context.to_string(), created_at,
            })
        }

        SignalKind::TodoFixme { text, file, .. } => Some(ImprovementCandidate {
            id,
            description: format!("Address TODO/FIXME in {file}: {text}"),
            risk: RiskLevel::Low,
            risk_reason: "TODO/FIXME comment in changed file".to_string(),
            source_signals: vec![signal.clone()],
            context: file.clone(), created_at,
        }),

        SignalKind::TestFailure { test_name, module } => Some(ImprovementCandidate {
            id,
            description: format!("Fix failing test `{test_name}` in module `{module}`"),
            risk: RiskLevel::High,
            risk_reason: "test failure indicates broken functionality".to_string(),
            source_signals: vec![signal.clone()],
            context: module.clone(), created_at,
        }),
    }
}

fn lint_risk(detail: &str, context: &str, cfg: &SelfImprovementConfig) -> (RiskLevel, String) {
    let lower = detail.to_lowercase();
    for kw in HIGH_RISK_KEYWORDS {
        if lower.contains(kw) {
            return (RiskLevel::High, format!("high-risk keyword '{kw}' in lint warning"));
        }
    }
    for blocked in &cfg.blocklist {
        if context.contains(blocked.as_str()) {
            return (RiskLevel::High, format!("context contains blocklist file '{blocked}'"));
        }
    }
    (RiskLevel::Low, "lint-only change in non-blocklist file".to_string())
}

fn generate_id(index: usize) -> String {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    format!("{:016x}{:04x}", micros, index & 0xffff)
}

#[cfg(test)]
mod tests {
    // test code from Step 1
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p codi-core risk
```

Expected: all 7 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/risk.rs crates/codi-core/src/lib.rs
git commit -m "feat(risk): add risk classifier producing Low/High ImprovementCandidates"
```

---

### Task 4: Pending Queue (`pending.rs`)

**Files:**
- Create: `crates/codi-core/src/pending.rs`
- Modify: `crates/codi-core/src/lib.rs` — add `pub mod pending;`

**Interfaces:**
- Consumes: `risk::ImprovementCandidate`
- Produces:
  - `pub struct PendingQueue`
  - `impl PendingQueue { fn load(path: &Path) -> Result<Self>; fn items(&self) -> &[ImprovementCandidate]; fn push(&mut self, candidate) -> Result<()>; fn remove(&mut self, id: &str) -> Option<ImprovementCandidate>; fn save(&self) -> Result<()> }`

- [ ] **Step 1: Write the failing tests**

  Create `crates/codi-core/src/pending.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk::{ImprovementCandidate, RiskLevel};
    use tempfile::tempdir;

    fn candidate(id: &str, desc: &str) -> ImprovementCandidate {
        ImprovementCandidate {
            id: id.to_string(), description: desc.to_string(),
            risk: RiskLevel::High, risk_reason: "test".to_string(),
            source_signals: vec![], context: "src/lib.rs".to_string(),
            created_at: 0,
        }
    }

    #[test]
    fn load_missing_file_returns_empty_queue() {
        let dir = tempdir().unwrap();
        let q = PendingQueue::load(&dir.path().join("pending.json")).unwrap();
        assert!(q.items().is_empty());
    }

    #[test]
    fn push_save_and_reload_round_trips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending.json");
        let mut q = PendingQueue::load(&path).unwrap();
        q.push(candidate("abc", "fix something")).unwrap();
        q.save().unwrap();

        let q2 = PendingQueue::load(&path).unwrap();
        assert_eq!(q2.items().len(), 1);
        assert_eq!(q2.items()[0].id, "abc");
        assert_eq!(q2.items()[0].description, "fix something");
    }

    #[test]
    fn remove_returns_item_and_shrinks_queue() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("pending.json");
        let mut q = PendingQueue::load(&path).unwrap();
        q.push(candidate("id1", "task 1")).unwrap();
        q.push(candidate("id2", "task 2")).unwrap();

        let removed = q.remove("id1");
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().id, "id1");
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].id, "id2");
    }

    #[test]
    fn remove_unknown_id_returns_none() {
        let dir = tempdir().unwrap();
        let mut q = PendingQueue::load(&dir.path().join("p.json")).unwrap();
        assert!(q.remove("nope").is_none());
    }

    #[test]
    fn duplicate_id_is_silently_ignored() {
        let dir = tempdir().unwrap();
        let mut q = PendingQueue::load(&dir.path().join("p.json")).unwrap();
        q.push(candidate("dup", "first")).unwrap();
        q.push(candidate("dup", "second")).unwrap();
        assert_eq!(q.items().len(), 1);
        assert_eq!(q.items()[0].description, "first");
    }
}
```

- [ ] **Step 2: Add `pub mod pending;` to `lib.rs` and run to see tests fail**

```bash
cargo test -p codi-core pending
```

- [ ] **Step 3: Implement `pending.rs`**

```rust
//! Pending improvement queue — JSON inbox.
//!
//! Only active items live here. Items are removed (not status-changed) on
//! approve or dismiss. History lives in `.codi/improvement_log.jsonl`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::risk::ImprovementCandidate;

pub struct PendingQueue {
    path: PathBuf,
    items: Vec<ImprovementCandidate>,
}

impl PendingQueue {
    /// Load the queue. Returns an empty queue when the file does not exist.
    pub fn load(path: &Path) -> Result<Self> {
        let items = if path.exists() {
            let text = std::fs::read_to_string(path)
                .with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_str::<Vec<ImprovementCandidate>>(&text)
                .with_context(|| format!("parsing {}", path.display()))?
        } else {
            vec![]
        };
        Ok(PendingQueue { path: path.to_path_buf(), items })
    }

    pub fn items(&self) -> &[ImprovementCandidate] { &self.items }

    /// Push a candidate. Silently ignores duplicates (matching `id`).
    pub fn push(&mut self, candidate: ImprovementCandidate) -> Result<()> {
        if !self.items.iter().any(|c| c.id == candidate.id) {
            self.items.push(candidate);
        }
        Ok(())
    }

    /// Remove and return the candidate with `id`, or `None` if not found.
    pub fn remove(&mut self, id: &str) -> Option<ImprovementCandidate> {
        self.items.iter().position(|c| c.id == id).map(|i| self.items.remove(i))
    }

    /// Write current items to disk atomically (write-then-rename).
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&self.items)
            .context("serializing pending queue")?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("renaming to {}", self.path.display()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    // test code from Step 1
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p codi-core pending
```

Expected: all 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/pending.rs crates/codi-core/src/lib.rs
git commit -m "feat(pending): add PendingQueue with atomic JSON inbox persistence"
```

---

### Task 5: Auto-Improvement Executor (`improve.rs`)

**Files:**
- Create: `crates/codi-core/src/improve.rs`
- Modify: `crates/codi-core/src/lib.rs` — add `pub mod improve;`

**Interfaces:**
- Consumes: `risk::{ImprovementCandidate, RiskLevel}`, `config::Config`, `signals::Signal`, `engine::run_session_mcp`
- Produces:
  - `pub enum Outcome { Applied { branch }, Failed { reason }, Skipped { reason } }`
  - `pub struct LogEntry { id, description, risk, branch, outcome, reason, approved_by_claude, blocklist_bypassed, source_signals, created_at, completed_at }`
  - `pub struct ImprovementExecutor<'a>`
  - `impl ImprovementExecutor { fn new(cfg, repo_root) -> Self; fn run(&self, candidate, auto_count: &mut usize) -> Result<Outcome>; fn run_approved(&self, candidate) -> Result<Outcome> }`
  - `pub fn append_log(repo_root: &Path, entry: &LogEntry) -> Result<()>`
  - `pub fn branch_name(candidate: &ImprovementCandidate, prefix: &str) -> String` (pub for tests)

- [ ] **Step 1: Write the failing tests**

  Create `crates/codi-core/src/improve.rs` with only:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::risk::{ImprovementCandidate, RiskLevel};
    use tempfile::tempdir;

    fn candidate(id: &str, context: &str) -> ImprovementCandidate {
        ImprovementCandidate {
            id: id.to_string(), description: "add a missing test".to_string(),
            risk: RiskLevel::Low, risk_reason: "lint only".to_string(),
            source_signals: vec![], context: context.to_string(), created_at: 0,
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
                .status().unwrap();
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
    fn append_log_creates_file_and_appends_jsonl() {
        let dir = tempdir().unwrap();
        let entry = LogEntry {
            id: "log1".to_string(), description: "test entry".to_string(),
            risk: "Low".to_string(), branch: "improve/log1-test".to_string(),
            outcome: "Applied".to_string(), reason: None,
            approved_by_claude: false, blocklist_bypassed: false,
            source_signals: vec![], created_at: 0, completed_at: 1,
        };
        append_log(dir.path(), &entry).unwrap();
        append_log(dir.path(), &entry).unwrap();

        let content = std::fs::read_to_string(dir.path().join(".codi/improvement_log.jsonl")).unwrap();
        assert_eq!(content.lines().count(), 2);
        // each line must be valid JSON
        for line in content.lines() {
            serde_json::from_str::<serde_json::Value>(line).unwrap();
        }
    }
}
```

- [ ] **Step 2: Add `pub mod improve;` to `lib.rs` and run to see tests fail**

```bash
cargo test -p codi-core improve
```

- [ ] **Step 3: Implement `improve.rs`**

```rust
//! Auto-improvement executor: branch → Goose → test/lint gate → commit or rollback.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::risk::ImprovementCandidate;
use crate::signals::Signal;

#[derive(Debug)]
pub enum Outcome {
    Applied { branch: String },
    Failed  { reason: String },
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
    cfg: &'a Config,
    repo_root: &'a Path,
}

impl<'a> ImprovementExecutor<'a> {
    pub fn new(cfg: &'a Config, repo_root: &'a Path) -> Self {
        ImprovementExecutor { cfg, repo_root }
    }

    /// Run a low-risk auto-improvement. Enforces blocklist, quota, and clean-state checks.
    /// Increments `auto_count` on successful application.
    pub fn run(&self, candidate: &ImprovementCandidate, auto_count: &mut usize) -> Result<Outcome> {
        if *auto_count >= self.cfg.self_improvement.max_auto_per_run {
            return Ok(Outcome::Skipped {
                reason: format!("max_auto_per_run ({}) reached", self.cfg.self_improvement.max_auto_per_run),
            });
        }
        for blocked in &self.cfg.self_improvement.blocklist {
            if candidate.context.contains(blocked.as_str()) {
                return Ok(Outcome::Skipped {
                    reason: format!("context contains blocklist file '{blocked}'"),
                });
            }
        }
        if !git_is_clean(self.repo_root)? {
            return Ok(Outcome::Skipped { reason: "git working tree is not clean".to_string() });
        }
        let branch = branch_name(candidate, &self.cfg.self_improvement.branch_prefix);
        let outcome = self.execute(candidate, &branch, false, false)?;
        if matches!(outcome, Outcome::Applied { .. }) {
            *auto_count += 1;
        }
        Ok(outcome)
    }

    /// Run a Claude-approved improvement. Skips blocklist and quota; test+lint gate still applies.
    pub fn run_approved(&self, candidate: &ImprovementCandidate) -> Result<Outcome> {
        if !git_is_clean(self.repo_root)? {
            return Ok(Outcome::Failed { reason: "git working tree is not clean".to_string() });
        }
        let blocklist_bypassed = self.cfg.self_improvement.blocklist.iter()
            .any(|b| candidate.context.contains(b.as_str()));
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
        let now_secs = || std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        git_create_branch(self.repo_root, branch)?;

        let task = format!(
            "{}\n\nFiles to focus on: {}",
            candidate.description, candidate.context
        );
        crate::engine::run_session_mcp(self.cfg, &task, None, self.repo_root, "")?;

        // Post-Goose diff size check
        let shortstat = git_shortstat(self.repo_root)?;
        let diff_lines = crate::signals::parse_diff_line_count(&shortstat);
        if diff_lines > self.cfg.self_improvement.max_diff_lines {
            let reason = format!(
                "diff too large ({diff_lines} lines > max {}); rolled back",
                self.cfg.self_improvement.max_diff_lines
            );
            git_rollback(self.repo_root, &original_branch, branch)?;
            append_log(self.repo_root, &LogEntry {
                id: candidate.id.clone(), description: candidate.description.clone(),
                risk: format!("{:?}", candidate.risk), branch: branch.to_string(),
                outcome: "Failed".to_string(), reason: Some(reason.clone()),
                approved_by_claude, blocklist_bypassed,
                source_signals: candidate.source_signals.clone(),
                created_at: candidate.created_at, completed_at: now_secs(),
            })?;
            return Ok(Outcome::Failed { reason });
        }

        // Test + lint gate (both required)
        let test_ok = run_test_gate(self.cfg, self.repo_root);
        let lint_ok = run_lint_gate(self.repo_root);

        if !test_ok || !lint_ok {
            let reason = if !test_ok {
                "test gate failed; rolled back".to_string()
            } else {
                "lint gate failed (cargo clippy -D warnings); rolled back".to_string()
            };
            git_rollback(self.repo_root, &original_branch, branch)?;
            append_log(self.repo_root, &LogEntry {
                id: candidate.id.clone(), description: candidate.description.clone(),
                risk: format!("{:?}", candidate.risk), branch: branch.to_string(),
                outcome: "Failed".to_string(), reason: Some(reason.clone()),
                approved_by_claude, blocklist_bypassed,
                source_signals: candidate.source_signals.clone(),
                created_at: candidate.created_at, completed_at: now_secs(),
            })?;
            return Ok(Outcome::Failed { reason });
        }

        git_commit(self.repo_root, &format!("self-improve: {} [auto]", candidate.description))?;

        append_log(self.repo_root, &LogEntry {
            id: candidate.id.clone(), description: candidate.description.clone(),
            risk: format!("{:?}", candidate.risk), branch: branch.to_string(),
            outcome: "Applied".to_string(), reason: None,
            approved_by_claude, blocklist_bypassed,
            source_signals: candidate.source_signals.clone(),
            created_at: candidate.created_at, completed_at: now_secs(),
        })?;

        Ok(Outcome::Applied { branch: branch.to_string() })
    }
}

// ── Branch name ──────────────────────────────────────────────────────────────

pub fn branch_name(candidate: &ImprovementCandidate, prefix: &str) -> String {
    let short_id = &candidate.id[..candidate.id.len().min(8)];
    let slug = slugify(&candidate.description, 4);
    format!("{prefix}/{short_id}-{slug}")
}

fn slugify(s: &str, max_words: usize) -> String {
    s.split_whitespace()
        .take(max_words)
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

// ── Git helpers ──────────────────────────────────────────────────────────────

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
    std::process::Command::new("git")
        .args(["checkout", original])
        .current_dir(repo_root)
        .status()
        .context("git checkout (rollback)")?;
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
    anyhow::ensure!(add.success(), "git add failed");
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

// ── Test + lint gates ────────────────────────────────────────────────────────

fn run_test_gate(cfg: &Config, repo_root: &Path) -> bool {
    let Some(cmd) = &cfg.commands.test else { return false };
    if cmd.is_empty() { return false; }
    let mut parts = cmd.split_whitespace();
    let Some(prog) = parts.next() else { return false };
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

// ── Log writer ───────────────────────────────────────────────────────────────

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

#[cfg(test)]
mod tests {
    // test code from Step 1
}
```

- [ ] **Step 4: Run tests**

```bash
cargo test -p codi-core improve
```

Expected: all 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/improve.rs crates/codi-core/src/lib.rs
git commit -m "feat(improve): add ImprovementExecutor with branch/test/rollback flow and log writer"
```

---

### Task 6: `post_run_hook` and Engine Wiring

**Files:**
- Modify: `crates/codi-core/src/engine.rs`
- Modify: `crates/codi-cli/src/main.rs`

**Interfaces:**
- Consumes: `signals::collect_signals`, `risk::classify`, `improve::ImprovementExecutor`, `pending::PendingQueue`
- Produces: `pub fn post_run_hook(cfg: &Config, repo_root: &Path, goose_exit_code: i32) -> Result<()>`

- [ ] **Step 1: Write failing tests**

  Add a new test module at the bottom of `engine.rs`:

```rust
#[cfg(test)]
mod hook_tests {
    use super::*;
    use crate::config::Config;
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

    #[test]
    fn hook_is_noop_when_self_improvement_disabled() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let mut cfg = Config::default();
        cfg.self_improvement.enabled = false;
        assert!(post_run_hook(&cfg, dir.path(), 0).is_ok());
        assert!(!dir.path().join(".codi/pending_improvements.json").exists());
    }

    #[test]
    fn hook_is_noop_when_exit_code_zero_and_no_changes() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        // No changed files, exit_code=0, no clippy warnings → no signals
        assert!(post_run_hook(&cfg, dir.path(), 0).is_ok());
    }
}
```

- [ ] **Step 2: Run to see tests fail**

```bash
cargo test -p codi-core hook_tests
```

- [ ] **Step 3: Implement `post_run_hook` in `engine.rs`**

  Add after the existing `pub fn pick_provider_label` function:

```rust
/// Called after each one-shot `run_session` in `cmd_run`. Collects signals,
/// classifies risk, auto-applies Low-risk improvements, queues High-risk ones.
/// No-op when `cfg.self_improvement.enabled` is false.
pub fn post_run_hook(cfg: &Config, repo_root: &Path, goose_exit_code: i32) -> Result<()> {
    if !cfg.self_improvement.enabled {
        return Ok(());
    }

    let changed_files = git_changed_files(repo_root);
    let clippy_output = run_clippy_capture(repo_root);

    let signals = crate::signals::collect_signals(
        repo_root, &clippy_output, &changed_files, goose_exit_code,
    );
    if signals.signals.is_empty() {
        return Ok(());
    }

    let candidates = crate::risk::classify(&signals, &cfg.self_improvement, &changed_files);
    if candidates.is_empty() {
        return Ok(());
    }

    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let mut queue = crate::pending::PendingQueue::load(&pending_path)?;
    let executor = crate::improve::ImprovementExecutor::new(cfg, repo_root);
    let mut auto_count = 0usize;

    for candidate in candidates {
        let is_high = candidate.risk == crate::risk::RiskLevel::High;
        if is_high || !cfg.self_improvement.auto_apply_low_risk {
            tracing::info!(id = %candidate.id, "queuing improvement: {}", candidate.description);
            queue.push(candidate)?;
        } else {
            tracing::info!(id = %candidate.id, "attempting auto-improvement: {}", candidate.description);
            match executor.run(&candidate, &mut auto_count)? {
                crate::improve::Outcome::Applied { branch } => {
                    tracing::info!("self-improvement applied on branch '{branch}'");
                }
                crate::improve::Outcome::Failed { reason } | crate::improve::Outcome::Skipped { reason } => {
                    tracing::warn!("self-improvement did not apply ({reason}); queuing");
                    queue.push(candidate)?;
                }
            }
        }
    }

    queue.save()
}

fn git_changed_files(repo_root: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("git")
        .args(["diff", "--name-only", "HEAD"])
        .current_dir(repo_root)
        .output()
    else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

fn run_clippy_capture(repo_root: &Path) -> String {
    let Ok(out) = std::process::Command::new("cargo")
        .args(["clippy", "--message-format=short"])
        .current_dir(repo_root)
        .output()
    else { return String::new() };
    // clippy writes human-readable warnings to stderr
    String::from_utf8_lossy(&out.stderr).to_string()
}
```

- [ ] **Step 4: Wire into `cmd_run` in `main.rs`**

  Add `post_run_hook` to the import at the top of `main.rs`:

```rust
use codi_core::engine::{pick_provider_label, post_run_hook, run_session, SessionMode};
```

  Add the hook call at the end of `cmd_run`, after the optional review block:

```rust
fn cmd_run(cfg: &Config, repo_root: &std::path::Path, task: &str, review: bool) -> Result<()> {
    println!("Provider: {}", pick_provider_label(cfg, task));
    let code = run_session(cfg, task, SessionMode::OneShot(task.to_string()), None, repo_root, "")?;
    if code != 0 {
        eprintln!("goose exited with code {code}");
    }
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
        tracing::warn!("post_run_hook error (non-fatal): {e}");
    }
    Ok(())
}
```

- [ ] **Step 5: Run tests and build**

```bash
cargo test -p codi-core
cargo build -p codi-cli
```

Expected: all tests pass; binary builds without errors.

- [ ] **Step 6: Commit**

```bash
git add crates/codi-core/src/engine.rs crates/codi-cli/src/main.rs
git commit -m "feat(engine): add post_run_hook() wired into cmd_run for event-driven self-improvement"
```

---

### Task 7: MCP Tools

**Files:**
- Modify: `crates/codi-core/src/mcp.rs`

**Interfaces:**
- Consumes: `pending::PendingQueue`, `improve::{ImprovementExecutor, LogEntry, Outcome, append_log}`, `risk::ImprovementCandidate`
- Produces: three new tools in `tools/list` dispatch and three new tool handler functions

- [ ] **Step 1: Write failing tests**

  Add a new test module at the bottom of `mcp.rs`:

```rust
#[cfg(test)]
mod mcp_improve_tests {
    use super::*;
    use crate::config::Config;
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

    #[test]
    fn list_pending_returns_empty_on_no_queue_file() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/call", &serde_json::json!({
            "name": "list_pending_improvements",
            "arguments": {}
        })).unwrap();
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(text).unwrap();
        assert_eq!(parsed["count"], 0);
        assert!(parsed["pending"].as_array().unwrap().is_empty());
    }

    #[test]
    fn approve_unknown_id_returns_error() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/call", &serde_json::json!({
            "name": "approve_improvement",
            "arguments": { "id": "doesnotexist" }
        }));
        assert!(result.is_err());
    }

    #[test]
    fn dismiss_unknown_id_returns_error() {
        let dir = tempdir().unwrap();
        init_git(dir.path());
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/call", &serde_json::json!({
            "name": "dismiss_improvement",
            "arguments": { "id": "nope", "reason": "not relevant" }
        }));
        assert!(result.is_err());
    }

    #[test]
    fn tools_list_includes_new_tools() {
        let dir = tempdir().unwrap();
        let cfg = Config::default();
        let result = dispatch(&cfg, dir.path(), "tools/list", &serde_json::Value::Null).unwrap();
        let tools = result["tools"].as_array().unwrap();
        let names: Vec<_> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert!(names.contains(&"list_pending_improvements"));
        assert!(names.contains(&"approve_improvement"));
        assert!(names.contains(&"dismiss_improvement"));
    }
}
```

- [ ] **Step 2: Run to see tests fail**

```bash
cargo test -p codi-core mcp_improve_tests
```

Expected: the three new tool names are not found.

- [ ] **Step 3: Add the three tools to the `tools/list` response in `mcp.rs`**

  In the `"tools/list"` arm of `dispatch`, append to the JSON array after `run_tests`:

```rust
// Append inside the "tools" JSON array:
{
    "name": "list_pending_improvements",
    "description": "Return all queued improvement proposals awaiting review. Call this after run_task or get_diff to check for pending items. For each item review risk_reason and source_signals, then call approve_improvement or dismiss_improvement.",
    "inputSchema": { "type": "object", "properties": {} }
},
{
    "name": "approve_improvement",
    "description": "Apply a queued improvement by id. Creates a branch, runs Goose, then runs tests and lint. Returns Applied (with branch name) or Failed (with reason). Blocklist and quota checks are skipped since you have reviewed it; test and lint gates still apply.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "id": { "type": "string", "description": "The improvement id from list_pending_improvements" }
        },
        "required": ["id"]
    }
},
{
    "name": "dismiss_improvement",
    "description": "Remove a queued improvement without applying it. Records the dismissal in the improvement log.",
    "inputSchema": {
        "type": "object",
        "properties": {
            "id":     { "type": "string", "description": "The improvement id to dismiss" },
            "reason": { "type": "string", "description": "Optional reason for dismissal" }
        },
        "required": ["id"]
    }
}
```

- [ ] **Step 4: Add the three arms to the `tools/call` dispatch**

  In the inner match inside `"tools/call"`:

```rust
"list_pending_improvements" => tool_list_pending(repo_root),
"approve_improvement"       => tool_approve(cfg, repo_root, &args),
"dismiss_improvement"       => tool_dismiss(repo_root, &args),
```

- [ ] **Step 5: Implement the three tool functions**

  Add at the bottom of `mcp.rs`:

```rust
fn tool_list_pending(repo_root: &Path) -> Result<Value> {
    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let queue = crate::pending::PendingQueue::load(&pending_path)?;
    let items: Vec<Value> = queue.items().iter().map(|c| serde_json::json!({
        "id":            c.id,
        "description":   c.description,
        "risk":          format!("{:?}", c.risk),
        "risk_reason":   c.risk_reason,
        "context":       c.context,
        "source_signals": c.source_signals,
        "created_at":    c.created_at,
    })).collect();
    let count = items.len();
    let text = serde_json::to_string_pretty(&serde_json::json!({
        "pending": items,
        "count":   count,
    })).context("serializing pending list")?;
    Ok(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
}

fn tool_approve(cfg: &Config, repo_root: &Path, args: &Value) -> Result<Value> {
    let id = args["id"].as_str().context("missing 'id' argument")?;

    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let mut queue = crate::pending::PendingQueue::load(&pending_path)?;
    let candidate = queue.remove(id)
        .ok_or_else(|| anyhow::anyhow!("no pending improvement with id '{id}'"))?;
    queue.save()?;

    let executor = crate::improve::ImprovementExecutor::new(cfg, repo_root);
    let outcome = executor.run_approved(&candidate)?;

    let text = match &outcome {
        crate::improve::Outcome::Applied { branch } =>
            format!("{{\"outcome\":\"Applied\",\"branch\":\"{branch}\",\"tests_passed\":true}}"),
        crate::improve::Outcome::Failed { reason } =>
            format!("{{\"outcome\":\"Failed\",\"reason\":\"{reason}\"}}"),
        crate::improve::Outcome::Skipped { reason } =>
            format!("{{\"outcome\":\"Skipped\",\"reason\":\"{reason}\"}}"),
    };
    Ok(serde_json::json!({ "content": [{ "type": "text", "text": text }] }))
}

fn tool_dismiss(repo_root: &Path, args: &Value) -> Result<Value> {
    let id = args["id"].as_str().context("missing 'id' argument")?;
    let reason = args["reason"].as_str().map(|s| s.to_string());

    let pending_path = repo_root.join(".codi/pending_improvements.json");
    let mut queue = crate::pending::PendingQueue::load(&pending_path)?;
    let candidate = queue.remove(id)
        .ok_or_else(|| anyhow::anyhow!("no pending improvement with id '{id}'"))?;
    queue.save()?;

    crate::improve::append_log(repo_root, &crate::improve::LogEntry {
        id: candidate.id.clone(),
        description: candidate.description.clone(),
        risk: format!("{:?}", candidate.risk),
        branch: String::new(),
        outcome: "Dismissed".to_string(),
        reason,
        approved_by_claude: true,
        blocklist_bypassed: false,
        source_signals: candidate.source_signals,
        created_at: candidate.created_at,
        completed_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    })?;

    Ok(serde_json::json!({
        "content": [{ "type": "text", "text": format!("Dismissed improvement '{id}'.") }]
    }))
}
```

- [ ] **Step 6: Run all tests**

```bash
cargo test -p codi-core
cargo build -p codi-cli
```

Expected: all tests pass (including the 4 new mcp_improve_tests); binary builds.

- [ ] **Step 7: Commit**

```bash
git add crates/codi-core/src/mcp.rs
git commit -m "feat(mcp): add list_pending_improvements, approve_improvement, dismiss_improvement"
```

---

## Self-Review

| Spec requirement | Covered by |
|---|---|
| `SelfImprovementConfig` with all fields and defaults | Task 1 |
| All 5 `SignalKind` variants defined | Task 2 |
| `DiffWithoutTest` as blocking negative signal | Task 2 (detect) + Task 3 (→ High risk) |
| `AgentReliability` as separate signal category | Task 2 |
| `TodoFixme` with `context_radius` field | Task 2 (type defined; content scanning is a v2 addition — `context_radius` field is present for forward compatibility) |
| `RiskLevel::Low | High` (Medium reserved, never produced) | Task 3 |
| `ImprovementCandidate` with all spec fields | Task 3 |
| Blocklist file → High risk | Task 3 (`lint_risk` function) |
| High-risk keywords in signal detail → High | Task 3 |
| Public interface change → High risk | Task 3 (via keyword heuristic; AST-level detection is a v2 addition) |
| `PendingQueue`: inbox-only semantics, duplicate guard, atomic save | Task 4 |
| Branch creation, Goose invocation, diff-size check, test+lint gate, rollback | Task 5 |
| `branch_prefix` config respected | Task 5 |
| `LogEntry` with `approved_by_claude` and `blocklist_bypassed` | Task 5 |
| `improvement_log.jsonl` append-only writer | Task 5 |
| `post_run_hook` wired into `cmd_run` only (not REPL, not MCP, not review) | Task 6 |
| Clippy captured for lint signals | Task 6 |
| Changed files from `git diff --name-only HEAD` | Task 6 |
| `list_pending_improvements` MCP tool with source_signals in response | Task 7 |
| `approve_improvement` — skips blocklist/quota, keeps test+lint gate | Task 7 |
| `dismiss_improvement` — optional reason, writes log entry | Task 7 |
| High-risk queue-then-approve flow | Tasks 6 + 7 combined |

**Two intentional v1 deferrals (acceptable per YAGNI):**
- `TodoFixme` content scanning: the type and `context_radius` field exist; detection of TODO comments in file content is not implemented (trivial to add in a follow-up task).
- Precise public-interface-change detection: uses keyword heuristics (`"api"`, `"breaking"`) rather than AST analysis. Sufficient for v1.

No other gaps.
