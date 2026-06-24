# Coding Standards Injection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Inject Andrej Karpathy's 4 coding principles into every Goose session at the system level so the agent writes high-quality code by default, without bloating the task text.

**Architecture:** A new `standards.rs` module holds the `CODING_STANDARDS` string constant. `engine.rs`'s `build_command()` passes it to Goose via the `--system <TEXT>` flag (discovered via `goose run --help`). This is a one-time session-level injection — Goose loads it before any task runs and it persists for the whole session.

**Tech Stack:** Rust, Goose 1.38.0 (`goose run --system` flag), existing `codi-core` crate.

## Global Constraints

- No new dependencies — pure Rust string constant, no file I/O at runtime.
- `--system` flag applies to `goose run` (OneShot mode) only; `goose session` does not support it. All current production paths (CLI `run`, MCP `run_task`, interactive REPL via per-input OneShot) use OneShot, so full coverage is achieved.
- `cargo test` must remain green after every task.
- Standards text must not appear in the task `--text` argument — only in `--system`.

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/codi-core/src/standards.rs` | Create | `CODING_STANDARDS: &str` constant — the 4 Karpathy rules |
| `crates/codi-core/src/lib.rs` | Modify line 9 | Add `pub mod standards;` |
| `crates/codi-core/src/engine.rs` | Modify `build_command` + `run_session_mcp` | Pass `--system CODING_STANDARDS` to Goose for OneShot invocations |
| `crates/codi-core/tests/integration_engine.rs` | Modify `fake_goose` fn + add test | Verify `--system` arg is present and contains standards |

---

## Task 1 — Create the standards constant

**Files:**
- Create: `crates/codi-core/src/standards.rs`
- Modify: `crates/codi-core/src/lib.rs`

**Interfaces:**
- Produces: `codi_core::standards::CODING_STANDARDS: &'static str`

- [ ] **Step 1: Write the failing test**

Add a test module at the bottom of the new `standards.rs` (it doesn't exist yet, so write the test inline with the implementation in Step 2):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standards_contains_all_four_rules() {
        assert!(!CODING_STANDARDS.is_empty());
        assert!(CODING_STANDARDS.contains("Think Before Coding"));
        assert!(CODING_STANDARDS.contains("Simplicity First"));
        assert!(CODING_STANDARDS.contains("Surgical Changes"));
        assert!(CODING_STANDARDS.contains("Goal-Driven Execution"));
    }
}
```

- [ ] **Step 2: Create `crates/codi-core/src/standards.rs` with the constant and the test**

```rust
/// Andrej Karpathy's 4 coding principles, injected into every Goose session
/// via `--system`. The agent sees these before any task is sent.
pub const CODING_STANDARDS: &str = "\
## Coding Standards (always apply)

### 1. Think Before Coding
State your assumptions explicitly before writing any code. If multiple \
interpretations of the task exist, present them — don't pick one silently. \
If a simpler approach exists, say so. If something is unclear, stop and ask.

### 2. Simplicity First
Write the minimum code that solves the problem. No features beyond what was \
asked. No abstractions for single-use code. No flexibility or configurability \
that wasn't requested. No error handling for impossible scenarios. \
If you write 200 lines and it could be 50, rewrite it.

### 3. Surgical Changes
Touch only what the task requires. Don't improve adjacent code, comments, or \
formatting unless they are broken by your change. Match the existing style. \
Remove imports, variables, and functions made unused by YOUR changes only — \
never remove pre-existing dead code unless explicitly asked.

### 4. Goal-Driven Execution
Before implementing, define a verifiable success criterion. \
For bug fixes: write a failing test first, then make it pass. \
For features: write tests for the expected behaviour, then implement. \
For multi-step tasks, state a brief plan with a verify step per item.\
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standards_contains_all_four_rules() {
        assert!(!CODING_STANDARDS.is_empty());
        assert!(CODING_STANDARDS.contains("Think Before Coding"));
        assert!(CODING_STANDARDS.contains("Simplicity First"));
        assert!(CODING_STANDARDS.contains("Surgical Changes"));
        assert!(CODING_STANDARDS.contains("Goal-Driven Execution"));
    }
}
```

- [ ] **Step 3: Add `pub mod standards;` to `crates/codi-core/src/lib.rs`**

Insert after `pub mod setup;` (the last line of the module list):

```rust
pub mod standards;
```

The full module list should look like:
```rust
pub mod config;
pub mod engine;
pub mod mcp;
pub mod ollama;
pub mod review;
pub mod routing;
pub mod setup;
pub mod standards;
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p codi-core standards
```

Expected output:
```
test standards::tests::standards_contains_all_four_rules ... ok
```

- [ ] **Step 5: Commit**

```bash
git add crates/codi-core/src/standards.rs crates/codi-core/src/lib.rs
git commit -m "feat: add CODING_STANDARDS constant (Karpathy 4 rules)"
```

---

## Task 2 — Inject standards into Goose via `--system`

**Files:**
- Modify: `crates/codi-core/src/engine.rs`
- Modify: `crates/codi-core/tests/integration_engine.rs`

**Interfaces:**
- Consumes: `codi_core::standards::CODING_STANDARDS` from Task 1
- No change to public function signatures — this is an internal change to how Goose is invoked

- [ ] **Step 1: Update `fake_goose` helper to also echo CLI args**

In `crates/codi-core/tests/integration_engine.rs`, replace the `fake_goose` function:

```rust
fn fake_goose(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("goose");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(
        f,
        "#!/bin/sh\n\
         echo \"fake-goose: GOOSE_MODEL=$GOOSE_MODEL GOOSE_OPENAI_HOST=$GOOSE_OPENAI_HOST\"\n\
         echo \"fake-goose-args: $@\"\n\
         exit 0"
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    path
}
```

- [ ] **Step 2: Write the failing integration test**

Add this test to `crates/codi-core/tests/integration_engine.rs`:

```rust
#[test]
fn engine_passes_system_standards_to_goose() {
    let dir = tempfile::tempdir().unwrap();
    let goose = fake_goose(dir.path());

    // Capture fake-goose stdout so we can inspect args.
    let goose_output_file = dir.path().join("goose-out.txt");
    let script_path = goose.to_str().unwrap();
    let out_path = goose_output_file.to_str().unwrap();

    // Rewrite fake-goose to write args to a file for inspection.
    std::fs::write(
        &goose,
        format!(
            "#!/bin/sh\necho \"$@\" > {out_path}\nexit 0\n"
        ),
    ).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&goose, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let mut cfg = Config::default();
    cfg.goose_bin = Some(script_path.to_string());
    cfg.safety.confirm_commands = false;
    cfg.safety.confirm_writes = false;

    run_session(
        &cfg,
        "add a hello function",
        SessionMode::OneShot("add a hello function".to_string()),
        None,
        dir.path(),
        "",
    ).unwrap();

    let args = std::fs::read_to_string(&goose_output_file).unwrap();
    assert!(
        args.contains("--system"),
        "goose should be called with --system flag, got: {args}"
    );
    assert!(
        args.contains("Think Before Coding"),
        "system arg should contain Karpathy rule 1, got: {args}"
    );
    assert!(
        args.contains("Simplicity First"),
        "system arg should contain Karpathy rule 2, got: {args}"
    );
}
```

- [ ] **Step 3: Run test to verify it fails**

```bash
cargo test -p codi-core engine_passes_system_standards_to_goose
```

Expected: FAIL — `goose should be called with --system flag`

- [ ] **Step 4: Add `--system` to `build_command` in `engine.rs`**

In `crates/codi-core/src/engine.rs`, find the `build_command` function and add the `--system` arg to the `OneShot` branch:

```rust
fn build_command(
    goose_bin: &Path,
    cfg_path: &Path,
    mode: &SessionMode,
    repo_root: &Path,
) -> std::process::Command {
    let mut cmd = std::process::Command::new(goose_bin);
    cmd.current_dir(repo_root);
    set_env_from_yaml_if_needed(&mut cmd, cfg_path);

    match mode {
        SessionMode::Interactive => {
            cmd.arg("session");
            cmd.stdin(Stdio::inherit())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
        SessionMode::OneShot(task) => {
            cmd.args(["run", "--text", task]);
            cmd.arg("--system").arg(crate::standards::CODING_STANDARDS);
            cmd.stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit());
        }
    }
    cmd
}
```

- [ ] **Step 5: Add `--system` to `run_session_mcp` in `engine.rs`**

In `run_session_mcp`, find the block that builds the command args and add `--system` after `--text`:

```rust
    cmd.args(["run", "--text", task]);
    cmd.arg("--system").arg(crate::standards::CODING_STANDARDS);
    // Pipe goose stdout → a thread that echoes it to our stderr.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
```

- [ ] **Step 6: Run the failing test to verify it now passes**

```bash
cargo test -p codi-core engine_passes_system_standards_to_goose
```

Expected: PASS

- [ ] **Step 7: Run the full test suite**

```bash
cargo test
```

Expected: all 21+ tests pass, 0 failures.

- [ ] **Step 8: Smoke test — inspect the Goose invocation**

```bash
# Temporarily add --dry-run or just run and observe
codi run "say hello"
# In the Goose output you should see the agent respecting simplicity
# and surgical changes rather than over-engineering the response.
```

Verify `.codi/session/goose-session.yaml` still writes cleanly (it should be unchanged — standards go via CLI arg, not YAML).

- [ ] **Step 9: Commit**

```bash
git add crates/codi-core/src/engine.rs \
        crates/codi-core/tests/integration_engine.rs
git commit -m "feat: inject Karpathy coding standards into every Goose session via --system"
```

- [ ] **Step 10: Push**

```bash
git push
```

---

## Self-Review

**Spec coverage:**
- ✓ Standards baked into codi (not per-project config)
- ✓ Session-level injection via `--system` (not per-task text prefix)
- ✓ No task text bloat
- ✓ Covers `codi run`, `codi mcp run_task`, interactive REPL (all use OneShot)
- ✓ `CLAUDE.md` and `codi.toml` unchanged

**No placeholders:** All steps have actual code. ✓

**Type consistency:** `CODING_STANDARDS` is `&'static str`, consumed as `crate::standards::CODING_STANDARDS` — same reference in both `build_command` and `run_session_mcp`. ✓
