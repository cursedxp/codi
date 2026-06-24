# Self-Improvement Layer — Design Spec

**Date:** 2026-06-24
**Status:** Approved

---

## Goal

Add an event-driven self-improvement layer to Kodi so that, after each `codi run`, signals from the session are collected, risk-classified, and acted upon automatically — low-risk improvements applied immediately on isolated branches, high-risk candidates queued for Claude review and approval.

The loop is a natural extension of the existing development flow. No daemon, no scheduler, no separate subsystem.

---

## Scope

- New modules: `signals.rs`, `risk.rs`, `improve.rs`, `pending.rs`
- Minimal changes to: `engine.rs`, `mcp.rs`, `config.rs`
- No new crates, no new binaries
- v1: `Low | High` risk only (classifier extensible to `Medium` later)

Out of scope for v1: time-based scheduling, Medium risk tier, full SQLite signal history.

---

## Architecture

### New modules (`codi-core/src/`)

| Module | Responsibility |
|---|---|
| `signals.rs` | Collect signals from test output, lint, diff stats, agent reliability |
| `risk.rs` | Classify candidates as `Low` or `High`; produce `ImprovementCandidate` |
| `improve.rs` | Execute auto-improvement: branch → Goose → test/lint gate → commit or rollback |
| `pending.rs` | Read/write `.codi/pending_improvements.json` (inbox only, no history) |

### Changes to existing files

| File | Change |
|---|---|
| `engine.rs` | Call `post_run_hook()` after `run_session()` returns |
| `mcp.rs` | Add three new MCP tools |
| `config.rs` | Add `SelfImprovementConfig`, extend `Config` struct |

---

## Data Flow

```
codi run "task"
  └─ run_session() completes
       └─ post_run_hook(test_output, lint_output, diff, exit_code)
             ├─ SignalCollector → Vec<Signal>
             ├─ RiskClassifier  → Vec<ImprovementCandidate { desc, risk: Low|High }>
             │
             ├─ Low risk (auto_apply_low_risk = true):
             │     pre-check: clean git state, blocklist, quota
             │     git checkout -b improve/<slug>
             │     run_session_mcp(description + context_hint)
             │     post-Goose diff size check (> max_diff_lines → rollback)
             │     cargo test && cargo clippy -- -D warnings
             │     ✓ pass → git commit "self-improve: <desc> [auto]"
             │     ✗ fail → git checkout -, git branch -D improve/<slug>
             │     → improvement_log.jsonl entry either way
             │
             └─ High risk (always) / Low risk pre-check failed:
                   append to .codi/pending_improvements.json
                   Claude reads via list_pending_improvements()
                   approve_improvement(id) → same branch+test flow
                   dismiss_improvement(id, reason?) → log + remove from queue
```

---

## Signal Types

```rust
enum SignalKind {
    // Code quality
    LintWarning { category: String, detail: String },
    TestFailure  { test_name: String, module: String },
    DiffWithoutTest,        // changed code with no corresponding test change
                            // — negative signal: blocks low-risk auto-apply alone
    TodoFixme    { text: String, file: String, context_radius: usize },

    // Agent reliability (separate category — not code quality)
    AgentReliability { exit_code: i32, tool_failures: Vec<String> },
}
```

**`DiffWithoutTest` weight:** On its own it does not promote a candidate to High, but it blocks automatic low-risk application. The candidate goes to the pending queue instead.

**`TodoFixme` scope:** Changed files first; `context_radius` field allows future extension to neighbouring modules without schema change.

---

## Risk Classifier

### `RiskLevel` enum

```rust
enum RiskLevel {
    Low,
    High,
    // Medium variant reserved; never produced in v1
}
```

The classifier is structured so adding `Medium` in a later version requires only new rule cases, not a schema change.

### Low-risk rules (all must hold)

- No blocklist file in candidate context
- Diff estimate ≤ 50 lines, single module
- No public interface change (see High-risk definition below)
- `DiffWithoutTest` signal absent
- No `AgentReliability` failures in the triggering session

### High-risk rules (any one is sufficient)

- Candidate context includes a blocklist file
- Heuristic diff estimate > 200 lines (based on affected file count and signal density), or spans more than one crate — the actual post-Goose line count is enforced separately by `max_diff_lines` in `improve.rs`
- Public interface change: `pub fn`/`pub struct` signature, config schema field, CLI subcommand/arg, or MCP tool signature
- Signal description contains: `security`, `architecture`, `api`, `breaking`, `migration`
- `AgentReliability.tool_failures` non-empty

---

## `ImprovementCandidate`

```rust
struct ImprovementCandidate {
    id: String,                  // UUID v4
    description: String,         // task text sent to Goose
    risk: RiskLevel,
    risk_reason: String,         // human-readable explanation
    source_signals: Vec<Signal>,
    context: String,             // file paths relevant to this candidate
    created_at: u64,             // unix timestamp
}
```

---

## Auto-Improvement Executor (`improve.rs`)

### Pre-checks (low-risk only; failure → pending queue)

1. `git status --porcelain` is empty (clean working tree)
2. No blocklist file in `candidate.context`
3. `auto_per_run_count < max_auto_per_run`

### Branch naming

```
improve/<first-8-of-id>-<3-word-slug-from-description>
```

Example: `improve/a1b2c3d4-add-missing-test`

### Post-Goose diff check

After Goose finishes, run `git diff HEAD --stat` and count changed lines. If the total exceeds `max_diff_lines`, trigger rollback immediately (before tests run).

### Test + lint gate

Both must pass. Either failure triggers rollback.

```
<commands.test>                    (default: cargo test)
cargo clippy -- -D warnings
```

### Commit message format

```
self-improve: <description> [auto]
```

### Rollback sequence

```
git checkout <previous-branch>
git branch -D improve/<slug>
```

Branch is never left in a broken state; the delete happens only after the checkout succeeds.

---

## Pending Queue

**File:** `.codi/pending_improvements.json`
**Semantics:** Inbox only. Item is removed on `approve` or `dismiss`; history lives in `improvement_log.jsonl`.

### Item format

```json
{
  "id": "d4e5f6",
  "description": "Make routing.rs heuristic config-driven",
  "risk": "High",
  "risk_reason": "routing.rs is on blocklist; 'architecture' keyword matched",
  "source_signals": [
    { "kind": "LintWarning", "detail": "clippy::cognitive_complexity in routing.rs" },
    { "kind": "AgentReliability", "exit_code": 0, "tool_failures": [] }
  ],
  "context": "crates/codi-core/src/routing.rs",
  "created_at": 1750000000
}
```

---

## Improvement Log

**File:** `.codi/improvement_log.jsonl`
**Semantics:** Append-only history. Never purged automatically.

### Entry format

```json
{
  "id": "a1b2c3",
  "description": "Add missing test for bm25_search edge case",
  "risk": "Low",
  "branch": "improve/a1b2c3d4-add-missing-test",
  "outcome": "Applied | Failed | Dismissed",
  "reason": "cargo test exited with code 1",
  "approved_by_claude": false,
  "blocklist_bypassed": false,
  "source_signals": ["TestCoverage", "DiffWithoutTest"],
  "created_at": 1750000000,
  "completed_at": 1750000042
}
```

`approved_by_claude` is `true` for any item processed via `approve_improvement()`.
`blocklist_bypassed` is `true` when `approve_improvement()` processes a candidate whose context included a blocklist file.

---

## New MCP Tools

### `list_pending_improvements()`

Returns the current inbox. Tool description instructs Claude: *"Call this after run_task or get_diff to check for queued improvement proposals. Review risk_reason and source_signals for each item, then approve_improvement or dismiss_improvement."*

```json
{
  "pending": [ { "id": "...", "description": "...", "risk_reason": "...", "context": "...", "source_signals": [...] } ],
  "count": 1
}
```

### `approve_improvement(id: String)`

Runs the branch → Goose → test/lint → commit/rollback flow. Blocklist and `max_auto_per_run` checks are skipped (Claude has reviewed). Test + lint gate and `max_diff_lines` check still apply.

Success response:
```json
{ "outcome": "Applied", "branch": "improve/d4e5f6-routing-config-driven", "tests_passed": true }
```

Failure response:
```json
{ "outcome": "Failed", "reason": "cargo test exited with code 1 — branch deleted" }
```

### `dismiss_improvement(id: String, reason?: String)`

Removes item from queue. Logs `outcome: Dismissed` with optional reason to `improvement_log.jsonl`.

---

## Guardrail Matrix

| Guardrail | Low-risk auto | `approve_improvement()` | Failure action |
|---|---|---|---|
| Clean git state | ✓ enforced | ✓ enforced | → pending queue |
| Blocklist check | ✓ enforced | ✗ skipped | → pending queue |
| `max_auto_per_run` quota | ✓ enforced | ✗ skipped | → pending queue |
| `max_diff_lines` | ✓ enforced | ✓ enforced | → rollback + pending |
| `cargo test` passes | ✓ enforced | ✓ enforced | → rollback |
| `cargo clippy -D warnings` | ✓ enforced | ✓ enforced | → rollback |
| Same `id` not retried | ✓ enforced | N/A | → silently skipped |

---

## Configuration

### `codi.toml`

```toml
[self_improvement]
enabled            = true
auto_apply_low_risk = true
max_auto_per_run   = 2
max_diff_lines     = 300
branch_prefix      = "improve"
blocklist = [
    "crates/codi-core/src/routing.rs",
    "crates/codi-core/src/mcp.rs",
    "crates/codi-core/src/engine.rs",
    "crates/codi-core/src/config.rs",
]
```

All fields optional; values above are defaults. `enabled = false` disables the entire self-improvement subsystem.

### `SelfImprovementConfig` (Rust)

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
```

---

## Invariants

1. Auto-improvement never commits to `main` or any non-`improve/*` branch.
2. A rollback always checks out the previous branch before deleting the improvement branch.
3. Test + lint gate is mandatory even for Claude-approved items.
4. The pending queue contains only active items; history is in `improvement_log.jsonl` only.
5. The same candidate `id` is never auto-retried after failure.
6. `auto_per_run_count` resets per `post_run_hook` invocation, not globally.

---

## Future Extension Points

- **Medium risk tier:** Add `RiskLevel::Medium` with human-in-loop (not Claude, not auto); classifier produces it when exactly one high-risk rule fires.
- **Context radius for TODO/FIXME:** `context_radius` field in `TodoFixme` signal enables neighbouring-module scanning without schema change.
- **Time-based scheduling:** `post_run_hook` can be extracted into a standalone `codi improve --now` command and called from a cron or `launchd` plist without architectural change.
- **Signal history:** Replace `.codi/pending_improvements.json` + `.codi/improvement_log.jsonl` with a SQLite-backed `SignalStore` if query patterns justify it.
