# codi — local-first AI coding agent

`codi` is a terminal AI coding agent that runs against a **local LLM** (Ollama or any OpenAI-compatible endpoint) with optional cloud escalation. It understands your repo via a local RAG index, writes and edits code, runs tests, reviews its own diffs — and integrates natively with Claude Code as an MCP server.

```
cd your-repo
codi run "Add a validate_email() function with unit tests"
```

## How it works

`codi` drives [Block's Goose](https://github.com/block/goose) as its agent engine (subprocess), adding:

- **Local-first defaults** — Ollama endpoint, no cloud keys required on first use.
- **First-launch wizard** — detects Ollama, lists installed models, lets you pick one.
- **Hybrid RAG** — BM25 full-text search (+ optional embeddings) over your repo.
- **Routing policy** — keep tasks local, or let `hybrid` mode escalate complex ones to cloud.
- **Self-review** — after a task, `codi review` gives a structured diff review from the model.
- **Self-improvement** — after each `codi run`, signals (lint warnings, test failures, missing tests) are collected, risk-classified, and low-risk fixes applied automatically on isolated branches; high-risk proposals queue for Claude review.
- **Claude Code MCP integration** — expose `run_task / get_diff / run_tests` as native tools so Claude Code can orchestrate the implement → review → fix loop automatically.

---

## Prerequisites

| Tool | Install |
|------|---------|
| Rust ≥ 1.82 | `curl https://sh.rustup.rs -sSf \| sh` |
| Goose (Block) | `brew install block-goose-cli` |
| Ollama | `brew install ollama` |

Pull a model that supports structured tool calls (required for Goose):

```bash
ollama pull gemma4:e4b      # recommended — safe edit behaviour (fails clean, never clobbers)
# or: qwen2.5:7b, llama3.1:8b, mistral:7b
ollama serve                 # keep running in a background terminal
```

> **Model compatibility:** `qwen2.5-coder:7b` does NOT work with Goose (returns tool calls as text). Run `codi model check <name>` to verify any model.

> **Context length (important):** Ollama's default context window (~4k tokens) is too small for agent work — Goose's system prompt alone nearly fills it. The symptom is silent: the model never emits the write tool call, so tasks fail with `no_diff` or produce truncated/hallucinated content. Create a larger-context derivative and use that:
>
> ```bash
> printf 'FROM gemma4:e4b\nPARAMETER num_ctx 16384\n' | ollama create gemma4-e4b-16k -f -
> codi model set gemma4-e4b-16k
> ```
>
> In field testing this single change took the same model from failing at ~20-line files to writing 130-line files byte-perfect.

---

## Install

```bash
git clone https://github.com/anilozsoy/codi
cd codi
cargo install --path crates/codi-cli   # installs `codi` to ~/.cargo/bin
cargo install --path crates/codi-rag   # installs `codi-rag` MCP server
```

---

## Quick start

```bash
cd your-project

# First launch: wizard detects Ollama, lists models, you pick one → writes codi.toml
codi

# One-shot task
codi run "Add a validate_email() function with unit tests"

# One-shot with automatic self-review
codi run --review "Refactor error handling in src/db.rs to use anyhow"

# Interactive REPL
codi

# Review the current git diff
codi review

# Show resolved config
codi config
```

---

## Model management

```bash
# List all models installed in Ollama with tool-call compatibility info
codi model list

# Interactively pick a different model (updates codi.toml)
codi model pick

# Set a model directly
codi model set qwen2.5:14b

# Check if a model supports structured tool_calls (required for Goose)
codi model check qwen2.5:7b
```

---

## Configuration

`codi.toml` in the project root is merged over `~/.config/codi/config.toml`.

```toml
[model.local]
base_url = "http://localhost:11434/v1"
model    = "gemma4-e4b-16k"            # num_ctx-raised derivative — see Prerequisites
api_key  = ""                          # not needed for Ollama

[model.cloud]                          # optional — only used when routing escalates
provider    = "anthropic"
model       = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"

[routing]
mode = "local-only"                    # local-only | hybrid | cloud-preferred

[commands]
test   = "cargo test"
lint   = "cargo clippy"
build  = "cargo build"
format = "cargo fmt"

[rag]
embeddings  = false                    # true → hybrid BM25 + vector retrieval
embed_model = "nomic-embed-text"
db_path     = ".codi/index.sqlite"

[safety]
confirm_commands = true                # set false for CI / unattended runs
confirm_writes   = true

[reliability]
enabled                   = true
model_tier                = ""         # "small" | "medium" | "large" | "" (auto from model name)
verify_artifacts          = true       # verify files actually changed after write tasks
max_retries               = 1          # local retries before escalation
escalate_on_retry_failure = true       # needs [model.cloud] configured, otherwise skipped
log_events                = true
log_path                  = ".codi/reliability.jsonl"
timeout_secs              = 600        # kill a stuck one-shot goose run after this
```

### Reliability layer

Small local models fail in characteristic ways: they write nothing (silent no-op), clobber a file they were asked to append to, or drift off-task. Every `codi run` / `run_task` passes through a guard that catches these instead of reporting false success:

1. **Classify** — write-intent and complexity are detected from the task text. Model tier comes from the model name (`7b` → Small, `14b` → Medium, unknown → Large) and sets the decompose threshold.
2. **Decompose** — multi-file tasks split into one step per file. Every step carries the **full task text**; only the focus differs.
3. **Verify** — after each step: exit code, expected files exist, and the filesystem snapshot actually saw a write (git-independent). Append-style tasks ("append", "do not remove"…) additionally snapshot the target's content first — if the run drops it, the step fails with `content_lost`.
4. **Retry** — failed steps retry with a failure-specific prompt; a `content_lost` retry carries the original content back so the model can restore it.
5. **Escalate** — when retries are exhausted and `[model.cloud]` is configured, the step re-runs against the cloud model.
6. **Log** — every attempt is appended to `.codi/reliability.jsonl` (intermediate failures as `"retrying"`). `codi doctor` summarizes the recent success rate.

```bash
codi doctor        # health check: config, Ollama, MCP registration, reliability stats
cat .codi/reliability.jsonl | jq .
```

### Field notes: choosing a local model

Observed on a real project build (June 2026), worth more than benchmarks:

- **Fix the context window first.** Both models below failed identically on ~20+ line files until `num_ctx` was raised to 16384 — the bottleneck was mechanical, not model intelligence. See the context-length note under Prerequisites.
- **`gemma4:e4b`** — never corrupted a file; its failures were clean no-ops (`no_diff`), which the reliability layer catches and retries. Detected as Large tier, so no decompose overhead. Recommended.
- **`qwen2.5:7b`** — handled single-shot creates well, but on edit/append tasks it twice destroyed file content. Its name pins it to Small tier (threshold 2), so multi-signal tasks constantly decompose. Use only for small, single-purpose steps.

### Routing modes

| Mode | Behaviour |
|------|-----------|
| `local-only` | Always use `model.local` |
| `hybrid` | Local by default; escalates if task contains keywords like "refactor", "architecture", "migration" or is > 600 chars |
| `cloud-preferred` | Use cloud when configured, fall back to local |

### Unattended / CI mode

```bash
codi run -y "Run linter and fix all warnings"
```

or in `codi.toml`:

```toml
[safety]
confirm_commands = false
confirm_writes   = false
```

---

## RAG index

```bash
# Build (or rebuild) the local BM25 index
codi index
codi index --rebuild

# Enable hybrid retrieval (BM25 + cosine over embeddings)
# requires: ollama pull nomic-embed-text
```

In `codi.toml`:

```toml
[rag]
embeddings  = true
embed_model = "nomic-embed-text"
include     = ["src/**", "docs/**", "**/*.md"]
exclude     = ["target/**", "node_modules/**"]
```

---

## Claude Code MCP integration

`codi` can run as an MCP (Model Context Protocol) server, exposing three tools that Claude Code uses to orchestrate an **implement → review → fix** loop automatically.

### Tools exposed

| Tool | Description |
|------|-------------|
| `run_task(task)` | Implement a feature or fix using the local agent (Goose). Goose output streams to your terminal; JSON-RPC stream stays clean. |
| `get_diff(base?)` | Return the current `git diff` for Claude Code to review inline. |
| `run_tests()` | Run the configured test command and return output + exit code. |
| `list_pending_improvements()` | List queued improvement proposals awaiting review. Call after `run_task` to check for auto-collected items. |
| `approve_improvement(id)` | Apply a queued improvement: creates a branch, runs Goose, enforces test+lint gate, commits or rolls back. |
| `dismiss_improvement(id, reason?)` | Dismiss a queued improvement without applying it. Logs the dismissal. |

### Setup (one-time)

```bash
# Install codi (if not already done)
cargo install --path crates/codi-cli

# Register with Claude Code
claude mcp add codi -- codi mcp
```

Alternatively, the `.mcp.json` file in this repo is picked up automatically when you open this project in Claude Code.

### How the loop works

Once registered, Claude Code can autonomously:

```
You:         "Add rate-limiting to the API endpoints"

Claude Code: [run_task] → "Add rate-limiting to the API endpoints"
             (Goose implements — you see its output live in the terminal)

             [get_diff] → reads the full diff
             (Claude Code reviews inline with full context)

             If issues found:
             [run_task] → "Fix: rate limiter uses global state (thread-unsafe).
                           Refactor to per-handler state with Arc<Mutex<...>>"

             [run_tests] → verifies tests pass

             Summary: "Rate limiting added and verified. 3 files changed."
```

You never need to copy-paste diff output or relay review comments manually — Claude Code handles the full loop.

### Starting the server manually (debugging)

```bash
codi mcp
# Speaks JSON-RPC 2.0 on stdin/stdout. Press Ctrl-C to stop.
```

---

## Self-improvement

After each `codi run`, codi automatically collects signals (lint warnings, missing test coverage, agent reliability) and classifies them into improvement proposals.

- **Low-risk proposals** (no blocklist files, no keywords like `security`/`breaking`/`migration`) are applied automatically on isolated `improve/*` branches. Both `cargo test` and `cargo clippy -- -D warnings` must pass; otherwise the branch is rolled back and the proposal queued for review.
- **High-risk proposals** (architecture changes, blocklist files, breaking changes) are queued in `.codi/pending_improvements.json` for human or Claude review.

Configure in `codi.toml`:

```toml
[self_improvement]
enabled             = true
auto_apply_low_risk = true
max_auto_per_run    = 2       # max branches created per run
max_diff_lines      = 300     # rollback if Goose changes more than this
branch_prefix       = "improve"
blocklist = [
    "crates/codi-core/src/routing.rs",
    "crates/codi-core/src/mcp.rs",
    "crates/codi-core/src/engine.rs",
    "crates/codi-core/src/config.rs",
]
```

Claude Code can review queued proposals via the `list_pending_improvements`, `approve_improvement`, and `dismiss_improvement` MCP tools.

---

## Project structure

```
crates/
  codi-cli/         # thin clap CLI binary (codi)
  codi-core/        # config, routing, engine launch, self-review, MCP server,
                    # self-improvement (signals, risk, pending queue, executor)
  codi-rag/         # standalone MCP server: BM25 index + optional embeddings
  codi-mock-server/ # OpenAI-compatible mock for offline/hermetic tests
.mcp.json           # Claude Code MCP registration (project-scoped)
codi.toml           # sample project config with annotations
.codi/              # runtime state (git-ignored)
  pending_improvements.json   # queued improvement proposals (inbox)
  improvement_log.jsonl       # append-only history of all applied/failed/dismissed
```

### Architecture

```
codi run "task"
  │
  ├─ codi-core: load codi.toml, pick provider (routing)
  ├─ codi-core: generate session Goose YAML config
  ├─ codi-rag:  MCP extension → search_context (BM25 + optional embeddings)
  ├─ goose (subprocess): agent loop
  │     model ←→ read_file / write_file / edit_file / execute_command
  ├─ codi-core: git diff → review.rs (optional self-review)
  └─ codi-core: post_run_hook → signals → risk classify
        ├─ Low risk + auto_apply_low_risk=true
        │     git checkout -b improve/<slug>
        │     goose (subprocess): applies fix
        │     cargo test && cargo clippy -D warnings
        │     ✓ pass → git commit "self-improve: ... [auto]"
        │     ✗ fail → rollback branch, log Failed
        └─ High risk / pre-check failed
              → .codi/pending_improvements.json (awaits Claude review)

codi mcp  [MCP server mode for Claude Code]
  │
  ├─ run_task                    → run_session_mcp (goose stdout → stderr, JSON-RPC clean)
  ├─ get_diff                    → git diff HEAD
  ├─ run_tests                   → cfg.commands.test
  ├─ list_pending_improvements   → .codi/pending_improvements.json
  ├─ approve_improvement(id)     → branch → goose → test+lint → commit/rollback
  └─ dismiss_improvement(id)     → remove from queue + log Dismissed
```

---

## Running tests

```bash
cargo test                    # all tests — no model or network needed
cargo test -p codi-core       # config, routing, engine integration
cargo test -p codi-rag        # BM25, chunking, RRF, embeddings
```

---

## Examples

- [`examples/rust-feature.md`](examples/rust-feature.md) — adding a feature to a Rust project
- [`examples/typescript-feature.md`](examples/typescript-feature.md) — TypeScript/Node workflow

---

## Troubleshooting

**`goose` not found**
Make sure Goose is installed and on PATH: `which goose`. Install with `brew install block-goose-cli`.

**Model doesn't respond / tool calls don't work**
Run `codi model check <name>` to verify. Use `gemma4:e4b` or `qwen2.5:7b` — not `qwen2.5-coder:7b`.

**Tasks fail with `no_diff` / model writes nothing on longer tasks**
Almost always Ollama's default ~4k context window: Goose's system prompt + your task overflow it and the model never emits the write tool call. Create a `num_ctx 16384` derivative (see Prerequisites) — don't waste time swapping models first.

**`codi init` says "No function-calling models found" but models are installed**
Tool support is read from Ollama's `/api/show` capabilities (instant). On Ollama versions too old to report capabilities, codi falls back to live inference probes, which can time out when several large models are installed — upgrade Ollama (≥ 0.6).

**Wizard appears unexpectedly in CI**
In headless environments (no HOME dir), the wizard is suppressed automatically. If it still appears, create a minimal `codi.toml`:
```toml
[model.local]
model = "qwen2.5:7b"
```

**MCP server not showing in Claude Code**
Run `claude mcp add codi -- codi mcp` and restart Claude Code. Or check `.mcp.json` is in the project root.

---

## License

Apache 2.0 — same as [Block's Goose](https://github.com/block/goose), which this project drives as a subprocess.
