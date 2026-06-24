# codi — local-first AI coding agent

`codi` is a Rust-native terminal coding agent that runs against a **small local LLM** (via any OpenAI-compatible endpoint, e.g. Ollama) with optional cloud escalation. It understands your repo via a local RAG index, can write and edit code, run tests and linters, and review its own diffs — all from the terminal.

## How it works

```
Open any repo → run codi → talk to an AI teammate that reads, writes, and runs code
```

`codi` drives [Block's Goose](https://github.com/block/goose) as its agent engine (via subprocess), so you get file read/write/patch, shell execution, and a full agent loop for free. `codi` adds:

- **Local-first defaults** — points Goose at your Ollama endpoint, no cloud keys required.
- **Hybrid RAG** — BM25 full-text search (+ optional embedding similarity) over your repo, served as a Goose MCP extension.
- **Routing policy** — keep everything local, or let `hybrid` mode escalate complex tasks to a cloud model.
- **Self-review** — after a task completes, run `codi review` to get a structured diff review from the local model.

## Prerequisites

1. **Rust** — `rustup` ≥ 1.82.
2. **Goose** — install Block's Goose and ensure `goose` is on your PATH:
   ```
   brew tap block/goose
   brew install goose
   # or:
   curl -fsSL https://github.com/block/goose/releases/latest/download/install.sh | sh
   ```
3. **Ollama** (recommended for local model):
   ```
   brew install ollama
   ollama pull qwen2.5-coder:7b
   ollama serve   # in a background terminal
   ```

## Build

```
git clone https://github.com/your-org/codi
cd codi
cargo build --release
# Binary at target/release/codi — copy it anywhere on PATH:
cp target/release/codi ~/.local/bin/codi
# Also build the RAG MCP server:
cp target/release/codi-rag ~/.local/bin/codi-rag
```

## Quick start

```
cd your-project
cp /path/to/codi/codi.toml .
# Edit codi.toml to match your project's test/lint commands

# 1. Index the repo (fast, incremental)
codi index

# 2a. Interactive REPL
codi

# 2b. One-shot task
codi run "Add a validate_email() function with unit tests"

# 2c. One-shot with self-review
codi run --review "Refactor error handling in src/db.rs to use anyhow"

# 3. Review recent changes
codi review

# 4. Show resolved config
codi config
```

## Configuration

`codi.toml` is read from the project root, merged over `~/.config/codi/config.toml`.
See [`codi.toml`](./codi.toml) for the annotated example.

### Key settings

| Key | Default | Description |
|-----|---------|-------------|
| `model.local.base_url` | `http://localhost:11434/v1` | Ollama or any OpenAI-compatible endpoint |
| `model.local.model` | `qwen2.5-coder:7b` | Model name to request |
| `model.cloud.*` | unset | Optional cloud model (API key via env var) |
| `routing.mode` | `local-only` | `local-only` / `hybrid` / `cloud-preferred` |
| `commands.test` | unset | Command codi can run to execute tests |
| `rag.embeddings` | `false` | Enable hybrid BM25 + vector retrieval |
| `safety.confirm_commands` | `true` | Ask before running shell commands |

### Unattended / CI mode

```
codi run -y "Run linter and fix all warnings"
# or set in config:
[safety]
confirm_commands = false
confirm_writes   = false
```

### Hybrid routing

```toml
[model.cloud]
provider    = "anthropic"
model       = "claude-sonnet-4-5"
api_key_env = "ANTHROPIC_API_KEY"

[routing]
mode = "hybrid"
```

With `hybrid`, simple tasks (short, keyword-simple) stay on the local model.
Tasks containing words like "refactor", "architecture", "migration", or long descriptions
are escalated to the cloud model.

### Embedding-based hybrid RAG

```toml
[rag]
embeddings  = true
embed_model = "nomic-embed-text"   # must be available at model.local.base_url
```

Requires the local endpoint to serve `/v1/embeddings`. Ollama supports this:
```
ollama pull nomic-embed-text
```

## Project structure

```
crates/
  codi-cli/         # thin clap CLI binary (`codi`)
  codi-core/        # config, routing, engine launch, self-review
  codi-rag/         # MCP server: index/search (BM25 + optional embeddings)
  codi-mock-server/ # OpenAI-compatible mock for offline tests
```

## Running tests

```
cargo test           # all unit + integration tests (no model/network needed)
cargo test -p codi-core   # just core logic
cargo test -p codi-rag    # just RAG: BM25, chunking, RRF, embeddings
```

## Examples

- [`examples/rust-feature.md`](examples/rust-feature.md) — adding a feature to a Rust project
- [`examples/typescript-feature.md`](examples/typescript-feature.md) — working on a TypeScript/Node project

## Architecture

```
codi run "task"
  │
  ├─ codi-core: load codi.toml, pick provider (routing.rs)
  ├─ codi-core: generate session Goose config + register codi-rag MCP ext
  ├─ codi-rag:  index_repo / search_context (BM25 + optional embeddings)
  ├─ codi-core: inject top snippets into system prompt
  ├─ goose (subprocess): agent loop
  │     model ←→ file read/write/patch, execute_command
  └─ codi-core: git diff → review.rs → self-review (optional)
```
