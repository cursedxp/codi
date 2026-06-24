# Design: Built-in Coding Standards Injection

**Date:** 2026-06-24  
**Status:** Approved

---

## Problem

The local agent (Goose) writes code without any baseline quality contract. It may over-engineer, add speculative features, or make unrelated changes — common LLM coding failure modes. Every task starts from scratch with no shared understanding of what "good code" means in this project.

## Goal

Bake Andrej Karpathy's 4 coding principles into codi as a permanent, session-level default. The agent knows the rules before the first task arrives — no per-task repetition, no context bloat.

---

## Decision: What, Where, How

**What:** Karpathy's 4 rules (Think Before Coding, Simplicity First, Surgical Changes, Goal-Driven Execution) stored as a Rust string constant in codi source.

**Where:** Injected into the Goose session config YAML as an `instructions:` field — the Goose-native mechanism for session-level system instructions.

**How:** `build_goose_config()` in `engine.rs` appends the instructions block. One injection per session start. Goose loads it before any task runs.

**Scope:** Global default — applies to every `codi run`, every `codi mcp run_task`, every interactive REPL session. No per-project config, no opt-out flag (high quality is always the goal).

---

## Architecture

### New file: `crates/codi-core/src/standards.rs`

Holds one public constant:

```rust
pub const CODING_STANDARDS: &str = r#"
## Coding Standards (always apply)

### 1. Think Before Coding
State assumptions explicitly. If multiple interpretations exist, present them — don't pick silently. If a simpler approach exists, say so. If something is unclear, stop and ask.

### 2. Simplicity First
Minimum code that solves the problem. No features beyond what was asked. No abstractions for single-use code. No error handling for impossible scenarios. If 200 lines could be 50, rewrite it.

### 3. Surgical Changes
Touch only what the task requires. Don't improve adjacent code, comments, or formatting. Match existing style. Remove only imports/variables made unused by YOUR changes — not pre-existing dead code.

### 4. Goal-Driven Execution
Before starting, define a verifiable success criterion:
- "Add validation" → "Write failing tests, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
For multi-step tasks, state a brief plan with a verify step for each item.
"#;
```

### Modified: `crates/codi-core/src/engine.rs`

`build_goose_config()` adds an `instructions:` block to the generated YAML:

```yaml
# codi session config — auto-generated
GOOSE_PROVIDER: openai
OPENAI_BASE_URL: http://localhost:11434/v1
GOOSE_MODEL: qwen2.5:7b
OPENAI_API_KEY: ollama
instructions: |
  ## Coding Standards (always apply)
  ...
```

### Modified: `crates/codi-core/src/lib.rs`

```rust
pub mod standards;
```

---

## Data Flow

```
codi run "add feature X"
  │
  ├─ engine.rs: build_goose_config()
  │     → includes standards::CODING_STANDARDS as instructions: field
  │     → writes session YAML to .codi/session/goose-session.yaml
  │
  └─ goose run --text "add feature X"
        → reads session YAML at startup
        → standards active for entire session, before first task
        → model writes code according to the 4 rules
```

Same path applies to `codi mcp run_task` — it calls `run_session_mcp` which also calls `build_goose_config`.

---

## What Does NOT Change

- `codi.toml` — no new fields
- `CLAUDE.md` — user's project-specific review rules, separate concern
- MCP tools (`run_task`, `get_diff`, `run_tests`) — signatures unchanged
- Per-task prompt — not touched, no context bloat

---

## Files Changed

| File | Change |
|------|--------|
| `crates/codi-core/src/standards.rs` | New — `CODING_STANDARDS` constant |
| `crates/codi-core/src/lib.rs` | Add `pub mod standards;` |
| `crates/codi-core/src/engine.rs` | `build_goose_config()` appends instructions block |

---

## Testing

- `cargo test` must pass with no changes (standards are a string constant, no new logic paths)
- Manual smoke test: `codi run "add a hello() function"` — inspect `.codi/session/goose-session.yaml` to verify `instructions:` block is present
- Qualitative: agent output should show restraint (no unnecessary abstractions, matches existing style)
