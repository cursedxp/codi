# Example: Adding a feature to a Rust project

```
# Navigate to your project
cd my-rust-project

# Copy codi.toml (or create one)
cp /path/to/codi/codi.toml .
# Edit codi.toml to set your Ollama endpoint/model and test command

# Index the repo (first time or after big changes)
codi index

# One-shot: ask for a feature
codi run "Add a `Config::from_env()` constructor that reads DATABASE_URL from the environment and returns an error if it is missing."

# One-shot with auto self-review
codi run --review "Refactor the error handling in src/db.rs to use anyhow::Context everywhere instead of unwrap()"

# Interactive REPL (type tasks, Ctrl-C to exit)
codi

# Just review recent changes (after manual edits or a previous run)
codi review

# Unattended mode (skip confirmation prompts)
codi run -y "Add unit tests for the parser module"
```
