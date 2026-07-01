# codi — Local-model orchestration & honest verification

Date: 2026-07-01
Status: proposed

## Problem

codi drives a **weak local LLM** (e.g. `qwen2.5:7b` via Ollama+Goose). Two recurring failures:

1. **Weak model can't hold a whole task.** Given "build 25 records / a multi-part file", the local model produces placeholders, incomplete output, and bugs. Not a codi code bug — a capacity limit. Every user machine will have a small local model; we cannot assume strong hardware.
2. **Dishonest verification.** `verify_step` decides "did work happen?" purely from `git diff`. In a non-git project the diff is empty, so a file that WAS written is reported as `no_diff` → "fail" (false negative). A doc/prompt cannot fix this — the binary must report the truth.

## Key architectural insight

The user already runs **Claude Code as the cloud brain**. Claude Code plans the project and can decompose it into local-model-sized pieces, then feed each piece to codi over MCP (`run_task`). codi is the **local executor + honest reporter**, not the planner.

Therefore:
- **Planning / capacity-aware decomposition lives in Claude Code**, driven by instructions in the target project's `CLAUDE.md`. No LLM planner in codi's Rust.
- codi does NOT need a new MCP `model_capability` tool — `codi models` already exists and Claude Code can run it via Bash.
- codi's only hard requirement is to **execute one small step and report success/failure honestly**, regardless of git.

Split: ~80% documentation (CLAUDE.md), ~20% code (verification honesty).

## Deliverable A — Orchestration guidance in CLAUDE.md (`codi init`)

`ensure_claude_md` (init.rs:203) already injects a `## codi` section. Expand its content (`CLAUDE_MD_SECTION`, init.rs:9) with a capacity-aware workflow that tells Claude Code how to drive codi:

Content to add (Turkish, matching existing tone):
- **Before delegating:** run `codi models` to see the local model tier. `7b` and below = Small → the model can only handle tiny, single-purpose steps.
- **Decompose to fit the model.** Break the task into the smallest useful steps: one file, one function, or a small batch of records per `run_task` call. Never hand a large multi-part task to a Small model in one shot.
- **Feed steps one at a time**, naming the exact target file in each `run_task` so codi can verify it.
- **Verify each step** with `get_diff` / by reading the file before moving on. If a step comes back malformed, split it further and retry.
- **Roles:** Claude = plan, decompose, review, verify. codi = execute one small step on the local LLM.

**Idempotent update.** Today `ensure_claude_md` skips the file if the `## codi` marker is already present, so existing projects never receive updated guidance. Change it to own a delimited block:

```
<!-- codi:start -->
## codi — ...
...
<!-- codi:end -->
```

On re-run, **replace** the content between the markers with the current section (preserving everything outside). New files get the block; files with the old bare `## codi` marker (no delimiters) get the block appended once, then are delimiter-managed thereafter. This makes `codi init` refresh the guidance as it evolves.

## Deliverable B — git-independent verification (code)

Goal: `verify_step` tells the truth whether or not the target project is a git repo.

**Already done this session** (in git working tree, verify_step, reliability.rs:240):
- When `expected_paths` is set, verify against the **filesystem** (`repo_root.join(path).exists()`), not just git-diff. This closes the reported false negative for the normal decomposed-step case (Claude names the target file).
- `NoDiff` only fires inside a real git repo (`is_git_repo`), so a non-git write no longer false-fails.
- Tests: `verify_non_git_repo_with_written_expected_file_passes`, `verify_non_git_repo_missing_expected_file_still_fails`, `verify_non_git_repo_no_expected_paths_does_not_false_fail`.

**Remaining gap:** the no-`expected_paths` + non-git case currently returns `Pass` because we cannot prove a diff. That is "no false fail", but not real detection — a step that wrote nothing would falsely pass. Since Claude Code's decomposed steps will normally carry `expected_paths`, this is a low-risk edge.

**Phase 2 — filesystem snapshot (DECIDED: include now):**
- In `execute_with_guard`, before calling `run_engine`, capture a snapshot of the tree: `HashMap<PathBuf, SystemTime>` of file mtimes, skipping `.git`, `.codi`, `target`, `node_modules`, `dist`, `build`.
- After the run, recompute; `changed = any path added or any mtime increased`.
- Pass this boolean into `verify_step` as the git-independent replacement for the `git_changed_files().is_empty()` signal. `MissingPaths` still checks each `expected_path` via filesystem existence (already implemented).
- This makes verification fully git-independent and honest for every case, at the cost of one bounded tree walk per step.

Interface: change `verify_step(step, profile, repo_root, exit_code)` →
`verify_step(step, profile, repo_root, exit_code, changed_detected: bool)`, where the caller computes `changed_detected` from the before/after snapshot. The internal `git_changed_files` helper is removed from the verify path (snapshot is authoritative); keep it only if still used elsewhere.

## Non-goals (explicitly dropped)

- MCP `model_capability` tool — redundant with `codi models`.
- LLM-based planner inside codi's Rust — planning stays in Claude Code.
- Content-quality verification (lint/build in verify_step) — deferred by user earlier; can revisit.
- Cloud escalation config — separate, user handles model choice.

## Testing

- **A:** unit tests on `ensure_claude_md` — new file gets delimited block; existing bare-marker file gets block once; re-run replaces block content and preserves surrounding text; content outside markers untouched.
- **B (if phase-2):** unit tests on the snapshot helper — detects a new file, detects a modified file (mtime bump), ignores excluded dirs; `verify_step` passes/fails correctly in a non-git dir with no expected_paths.
- Full `cargo test -p codi-core` green; no new clippy warnings in changed files.
