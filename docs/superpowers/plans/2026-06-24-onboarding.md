# codi Onboarding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement `codi init` (single onboarding entry point) and `codi doctor` / `codi doctor --fix` (health check + auto-fix), removing the auto-wizard trigger from first launch.

**Architecture:** Two new modules (`init.rs`, `doctor.rs`) in `codi-core` handle all logic; `main.rs` gains two new subcommands and loses the auto-wizard trigger; `setup.rs::detect_ollama` is made public so both modules can reuse it.

**Tech Stack:** Rust, `toml` crate (Value-level manipulation), `serde_json` (`.mcp.json` handling), `anyhow`, existing `codi_core::ollama` and `codi_core::setup` helpers.

## Global Constraints

- `detect_ollama` in `setup.rs` must be changed from `fn` to `pub fn` (Task 1 prerequisite).
- All new modules: never use `deny_unknown_fields`; new structs use `#[serde(default)]` only.
- First-launch message: `println!` (not `eprintln!`), exit code `0`.
- `codi doctor` exit code: `0` when only `[⚠]`/`[ℹ]` items; `1` when any `[✗]` present.
- `--fix` never touches system-level concerns: no Ollama install, no model download, no Goose.
- `.mcp.json` backup on corrupt: write `.mcp.json.bak`, then overwrite `.mcp.json`.
- `claude` CLI absent: severity `Info` (`[ℹ]`) — does not affect exit code.
- `codi init` is idempotent: safe to run multiple times.

---

### Task 1: `init.rs` — implement `codi init`

**Files:**
- Modify: `crates/codi-core/src/setup.rs:163` — change `fn detect_ollama` to `pub fn detect_ollama`
- Create: `crates/codi-core/src/init.rs`
- Modify: `crates/codi-core/src/lib.rs` — add `pub mod init;`

**Interfaces:**
- Produces: `pub fn run_init(repo_root: &Path, rewrite_config: bool) -> Result<()>`
- Produces: `pub fn fill_defaults(existing: toml::Value, defaults: toml::Value) -> (toml::Value, usize)` (returns merged value + count of keys added)

- [ ] **Step 1: Make `detect_ollama` public**

In `crates/codi-core/src/setup.rs`, line 163, change:
```rust
fn detect_ollama() -> Result<String> {
```
to:
```rust
pub fn detect_ollama() -> Result<String> {
```

- [ ] **Step 2: Run tests to confirm no regressions**

```bash
cargo test -p codi-core 2>&1 | tail -5
```
Expected: `test result: ok.`

- [ ] **Step 3: Write failing tests for `fill_defaults` and `ensure_mcp_json`**

Create `crates/codi-core/src/init.rs` with just the test module first:

```rust
use std::path::Path;
use anyhow::{Context, Result};

pub fn run_init(_repo_root: &Path, _rewrite_config: bool) -> Result<()> {
    todo!()
}

pub fn fill_defaults(
    existing: toml::Value,
    defaults: toml::Value,
) -> (toml::Value, usize) {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn fill_defaults_adds_missing_section() {
        let existing: toml::Value = toml::from_str(r#"
[model.local]
model = "qwen2.5:7b"
base_url = "http://localhost:11434/v1"
api_key = ""
"#).unwrap();
        let defaults: toml::Value = toml::from_str(r#"
[model.local]
model = "default-model"
base_url = "http://localhost:11434/v1"
api_key = ""

[routing]
mode = "local-only"
"#).unwrap();
        let (merged, added) = fill_defaults(existing, defaults);
        assert!(added >= 1, "routing section should be added");
        assert!(merged.get("routing").is_some());
        // Existing model preserved
        assert_eq!(
            merged["model"]["local"]["model"].as_str().unwrap(),
            "qwen2.5:7b"
        );
    }

    #[test]
    fn fill_defaults_preserves_existing_values() {
        let existing: toml::Value = toml::from_str(r#"
[routing]
mode = "hybrid"
"#).unwrap();
        let defaults: toml::Value = toml::from_str(r#"
[routing]
mode = "local-only"
"#).unwrap();
        let (merged, added) = fill_defaults(existing, defaults);
        assert_eq!(added, 0);
        assert_eq!(merged["routing"]["mode"].as_str().unwrap(), "hybrid");
    }

    #[test]
    fn fill_defaults_no_change_when_complete() {
        let existing: toml::Value = toml::from_str(r#"
[model.local]
model = "qwen2.5:7b"
base_url = "http://localhost:11434/v1"
api_key = ""
"#).unwrap();
        let defaults = existing.clone();
        let (_, added) = fill_defaults(existing, defaults);
        assert_eq!(added, 0);
    }

    #[test]
    fn ensure_mcp_json_creates_file_when_absent() {
        let dir = tempdir().unwrap();
        ensure_mcp_json(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["mcpServers"]["codi"]["command"].as_str() == Some("codi"));
    }

    #[test]
    fn ensure_mcp_json_leaves_existing_intact_when_codi_present() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        let original = r#"{"mcpServers":{"codi":{"command":"codi","args":["mcp"]},"other":{"command":"other"}}}"#;
        std::fs::write(&path, original).unwrap();
        ensure_mcp_json(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        // File unchanged
        assert_eq!(content, original);
    }

    #[test]
    fn ensure_mcp_json_adds_codi_entry_when_absent() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, r#"{"mcpServers":{"other":{"command":"other"}}}"#).unwrap();
        ensure_mcp_json(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["mcpServers"]["codi"]["command"].as_str() == Some("codi"));
        assert!(json["mcpServers"]["other"].is_object(), "other entry preserved");
    }

    #[test]
    fn ensure_mcp_json_backs_up_corrupt_and_recreates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join(".mcp.json");
        std::fs::write(&path, "this is not json {{{").unwrap();
        ensure_mcp_json(dir.path()).unwrap();
        // Backup written
        let bak = std::fs::read_to_string(dir.path().join(".mcp.json.bak")).unwrap();
        assert_eq!(bak, "this is not json {{{");
        // Fresh file written
        let content = std::fs::read_to_string(&path).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert!(json["mcpServers"]["codi"].is_object());
    }
}
```

- [ ] **Step 4: Run tests to confirm they fail**

```bash
cargo test -p codi-core init 2>&1 | grep -E "FAILED|error"
```
Expected: compilation error (todo!() panics or missing functions).

- [ ] **Step 5: Implement `fill_defaults` and file helpers**

Replace the todo stubs in `init.rs` with:

```rust
use std::path::Path;
use anyhow::{Context, Result};
use crate::ollama;
use crate::setup::{detect_ollama, pick_model_interactive};

const MCP_JSON_CONTENT: &str = "{\n  \"mcpServers\": {\n    \"codi\": {\n      \"command\": \"codi\",\n      \"args\": [\"mcp\"]\n    }\n  }\n}\n";
const CODI_MCP_KEY: &str = "codi";

pub fn run_init(repo_root: &Path, rewrite_config: bool) -> Result<()> {
    println!("\n┌─────────────────────────────────────┐");
    println!("│  codi init — project setup          │");
    println!("└─────────────────────────────────────┘\n");

    // [1/5] Ollama
    println!("[1/5] Ollama kontrolü");
    let base_url = detect_ollama().map_err(|e| {
        eprintln!("✗ Ollama bulunamadı. Kur: brew install ollama && ollama serve");
        e
    })?;

    // [2/5] Model
    println!("[2/5] Model seçimi");
    let model = select_model(repo_root, &base_url, rewrite_config)?;

    // [3/5] codi.toml
    println!("[3/5] codi.toml");
    write_config(repo_root, &base_url, &model, rewrite_config)?;

    // [4/5] .mcp.json
    println!("[4/5] .mcp.json");
    ensure_mcp_json(repo_root)?;

    // [5/5] MCP registration
    println!("[5/5] MCP kaydı");
    register_mcp_claude();

    println!("\nTamamlandı. Şimdi Claude Code'u bu projede açıp kullanmaya başlayabilirsin.");
    Ok(())
}

fn select_model(repo_root: &Path, base_url: &str, rewrite_config: bool) -> Result<String> {
    let toml_path = repo_root.join("codi.toml");
    if !rewrite_config && toml_path.exists() {
        if let Some(m) = read_model_from_file(&toml_path) {
            if model_is_installed(base_url, &m) {
                println!("  ✓ {} mevcut — korunuyor", m);
                return Ok(m);
            } else {
                println!("  ⚠ {} Ollama'da yüklü değil", m);
            }
        }
    }
    match pick_model_interactive(base_url, "Model seçin:")? {
        Some(p) => Ok(p.model),
        None => anyhow::bail!("setup cancelled by user"),
    }
}

fn write_config(repo_root: &Path, base_url: &str, model: &str, rewrite_config: bool) -> Result<()> {
    let toml_path = repo_root.join("codi.toml");

    let mut fresh = crate::config::Config::default();
    fresh.model.local.base_url = base_url.to_string();
    fresh.model.local.model = model.to_string();

    if rewrite_config || !toml_path.exists() {
        let content = fresh.to_toml().context("serializing config")?;
        std::fs::write(&toml_path, content).context("writing codi.toml")?;
        if rewrite_config {
            println!("  ✓ codi.toml yeniden oluşturuldu");
        } else {
            println!("  ✓ codi.toml oluşturuldu");
        }
        return Ok(());
    }

    // Merge mode
    let existing_str = std::fs::read_to_string(&toml_path).context("reading codi.toml")?;
    let existing: toml::Value = toml::from_str(&existing_str).context("parsing codi.toml")?;
    let defaults: toml::Value = toml::Value::try_from(&fresh).context("serializing defaults")?;

    let (merged, added) = fill_defaults(existing, defaults);

    // Validate merged result; surface unknown-field errors as warnings
    let merged_str = toml::to_string_pretty(&merged).context("serializing merged config")?;
    if let Err(e) = crate::config::Config::from_toml(&merged_str) {
        let msg = e.to_string();
        if msg.contains("unknown field") {
            println!("  ⚠ {}", msg);
        }
    }

    std::fs::write(&toml_path, &merged_str).context("writing codi.toml")?;
    if added == 0 {
        println!("  ✓ codi.toml — değiştirilmedi");
    } else {
        println!("  ✓ codi.toml — {} eksik alan eklendi", added);
    }
    Ok(())
}

/// Fill missing keys from `defaults` into `existing`, recursing into tables.
/// Returns (merged, count_of_keys_added).
/// Existing values are never overwritten.
pub fn fill_defaults(existing: toml::Value, defaults: toml::Value) -> (toml::Value, usize) {
    match (existing, defaults) {
        (toml::Value::Table(mut ex_t), toml::Value::Table(def_t)) => {
            let mut added = 0usize;
            for (key, def_val) in def_t {
                match ex_t.remove(&key) {
                    Some(ex_val) => {
                        let (merged, a) = fill_defaults(ex_val, def_val);
                        added += a;
                        ex_t.insert(key, merged);
                    }
                    None => {
                        added += 1;
                        ex_t.insert(key, def_val);
                    }
                }
            }
            (toml::Value::Table(ex_t), added)
        }
        (existing, _) => (existing, 0),
    }
}

pub fn ensure_mcp_json(repo_root: &Path) -> Result<()> {
    let path = repo_root.join(".mcp.json");

    if !path.exists() {
        std::fs::write(&path, MCP_JSON_CONTENT).context("writing .mcp.json")?;
        println!("  ✓ .mcp.json oluşturuldu");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path).context("reading .mcp.json")?;
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(mut json) => {
            let has_codi = json
                .get("mcpServers")
                .and_then(|s| s.get(CODI_MCP_KEY))
                .is_some();
            if has_codi {
                println!("  ✓ .mcp.json — değiştirilmedi");
            } else {
                if let Some(servers) = json
                    .as_object_mut()
                    .and_then(|o| o.get_mut("mcpServers"))
                    .and_then(|s| s.as_object_mut())
                {
                    servers.insert(
                        CODI_MCP_KEY.to_string(),
                        serde_json::json!({"command": "codi", "args": ["mcp"]}),
                    );
                } else {
                    json["mcpServers"] = serde_json::json!({
                        CODI_MCP_KEY: {"command": "codi", "args": ["mcp"]}
                    });
                }
                let pretty = serde_json::to_string_pretty(&json).context("serializing .mcp.json")?;
                std::fs::write(&path, pretty).context("writing .mcp.json")?;
                println!("  ✓ .mcp.json — codi kaydı eklendi");
            }
        }
        Err(_) => {
            std::fs::write(repo_root.join(".mcp.json.bak"), &content)
                .context("writing .mcp.json.bak")?;
            std::fs::write(&path, MCP_JSON_CONTENT).context("writing .mcp.json")?;
            println!("  ⚠ .mcp.json bozuktu — .mcp.json.bak olarak yedeklendi, yeniden oluşturuldu");
        }
    }
    Ok(())
}

fn register_mcp_claude() {
    let claude_found = std::process::Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();

    if !claude_found {
        println!("  [ℹ] MCP kaydı atlandı — claude CLI yüklü değil.");
        println!("      Manuel kayıt: claude mcp add codi -- codi mcp");
        return;
    }

    let ok = std::process::Command::new("claude")
        .args(["mcp", "add", "codi", "--", "codi", "mcp"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if ok {
        println!("  ✓ MCP kaydı yapıldı (claude mcp add codi)");
    } else {
        println!("  ⚠ MCP kaydı başarısız.");
        println!("      Manuel kayıt: claude mcp add codi -- codi mcp");
    }
}

fn read_model_from_file(toml_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(toml_path).ok()?;
    let val: toml::Value = toml::from_str(&content).ok()?;
    val.get("model")
        .and_then(|m| m.get("local"))
        .and_then(|l| l.get("model"))
        .and_then(|v| v.as_str())
        .map(String::from)
}

fn model_is_installed(base_url: &str, model: &str) -> bool {
    ollama::list_models(base_url)
        .map(|ms| ms.iter().any(|m| m.name == model))
        .unwrap_or(false)
}
```

- [ ] **Step 6: Add `pub mod init;` to `lib.rs`**

In `crates/codi-core/src/lib.rs`, add after the existing pub mod lines:
```rust
pub mod init;
```

- [ ] **Step 7: Run tests**

```bash
cargo test -p codi-core init 2>&1 | tail -10
```
Expected: all 6 init tests pass (`test result: ok. 6 passed`).

- [ ] **Step 8: Full test suite**

```bash
cargo test -p codi-core 2>&1 | tail -5
```
Expected: `test result: ok.`

- [ ] **Step 9: Commit**

```bash
git add crates/codi-core/src/setup.rs crates/codi-core/src/init.rs crates/codi-core/src/lib.rs
git commit -m "feat(init): add codi init command with TOML merge and .mcp.json management"
```

---

### Task 2: `doctor.rs` — implement `codi doctor` / `codi doctor --fix`

**Files:**
- Create: `crates/codi-core/src/doctor.rs`
- Modify: `crates/codi-core/src/lib.rs` — add `pub mod doctor;`

**Interfaces:**
- Consumes: `crate::ollama::{is_running, list_models}`, `crate::init::ensure_mcp_json`
- Produces:
  - `pub enum Severity { Ok, Error, Warning, Info }`
  - `pub struct CheckResult { pub name: &'static str, pub severity: Severity, pub detail: String, pub suggestion: Option<String>, pub fixable: bool }`
  - `pub fn run_doctor(repo_root: &Path) -> Result<Vec<CheckResult>>`
  - `pub fn run_doctor_fix(repo_root: &Path) -> Result<Vec<CheckResult>>`
  - `pub fn print_doctor_report(checks: &[CheckResult]) -> bool` — returns `true` if any `[✗]`

- [ ] **Step 1: Write failing tests**

Create `crates/codi-core/src/doctor.rs` with just a test skeleton:

```rust
use std::path::Path;
use anyhow::Result;

pub enum Severity { Ok, Error, Warning, Info }

pub struct CheckResult {
    pub name: &'static str,
    pub severity: Severity,
    pub detail: String,
    pub suggestion: Option<String>,
    pub fixable: bool,
}

pub fn run_doctor(_repo_root: &Path) -> Result<Vec<CheckResult>> { todo!() }
pub fn run_doctor_fix(_repo_root: &Path) -> Result<Vec<CheckResult>> { todo!() }
pub fn print_doctor_report(_checks: &[CheckResult]) -> bool { todo!() }

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn init_toml(dir: &std::path::Path, model: &str) {
        let content = format!(
            "[model.local]\nmodel = \"{model}\"\nbase_url = \"http://localhost:11434/v1\"\napi_key = \"\"\n"
        );
        std::fs::write(dir.join("codi.toml"), content).unwrap();
    }

    #[test]
    fn check_toml_missing_returns_error() {
        let dir = tempdir().unwrap();
        let checks = run_doctor(dir.path()).unwrap();
        let toml_check = checks.iter().find(|c| c.name == "codi.toml").unwrap();
        assert!(matches!(toml_check.severity, Severity::Error));
    }

    #[test]
    fn check_toml_present_returns_ok() {
        let dir = tempdir().unwrap();
        init_toml(dir.path(), "qwen2.5:7b");
        let checks = run_doctor(dir.path()).unwrap();
        let toml_check = checks.iter().find(|c| c.name == "codi.toml").unwrap();
        assert!(matches!(toml_check.severity, Severity::Ok));
    }

    #[test]
    fn check_mcp_json_missing_returns_error() {
        let dir = tempdir().unwrap();
        let checks = run_doctor(dir.path()).unwrap();
        let mcp_check = checks.iter().find(|c| c.name == ".mcp.json").unwrap();
        assert!(matches!(mcp_check.severity, Severity::Error));
        assert!(mcp_check.fixable);
    }

    #[test]
    fn check_mcp_json_present_returns_ok() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"codi":{"command":"codi","args":["mcp"]}}}"#,
        ).unwrap();
        let checks = run_doctor(dir.path()).unwrap();
        let mcp_check = checks.iter().find(|c| c.name == ".mcp.json").unwrap();
        assert!(matches!(mcp_check.severity, Severity::Ok));
    }

    #[test]
    fn check_mcp_json_corrupt_returns_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "not json {{{").unwrap();
        let checks = run_doctor(dir.path()).unwrap();
        let mcp_check = checks.iter().find(|c| c.name == ".mcp.json").unwrap();
        assert!(matches!(mcp_check.severity, Severity::Error));
    }

    #[test]
    fn no_errors_means_exit_0() {
        let dir = tempdir().unwrap();
        // Provide a valid .mcp.json; skip Ollama/claude checks (they'll be Info/Warning from missing env)
        std::fs::write(
            dir.path().join(".mcp.json"),
            r#"{"mcpServers":{"codi":{"command":"codi","args":["mcp"]}}}"#,
        ).unwrap();
        let checks = run_doctor(dir.path()).unwrap();
        // print_doctor_report returns true only when there's a [✗]
        // The mcp.json check should be Ok now; other checks may be Error (Ollama) in test env
        // We just verify the contract: no [✗] → returns false
        let has_errors = checks.iter().any(|c| matches!(c.severity, Severity::Error));
        let report_result = print_doctor_report(&checks);
        assert_eq!(report_result, has_errors);
    }

    #[test]
    fn doctor_fix_creates_mcp_json() {
        let dir = tempdir().unwrap();
        run_doctor_fix(dir.path()).unwrap();
        assert!(dir.path().join(".mcp.json").exists());
    }

    #[test]
    fn self_improvement_absent_is_warning_not_error() {
        let dir = tempdir().unwrap();
        // codi.toml without [self_improvement]
        init_toml(dir.path(), "qwen2.5:7b");
        let checks = run_doctor(dir.path()).unwrap();
        let si_check = checks.iter().find(|c| c.name == "self_improvement");
        if let Some(c) = si_check {
            assert!(matches!(c.severity, Severity::Warning));
        }
    }
}
```

- [ ] **Step 2: Run tests to confirm they fail**

```bash
cargo test -p codi-core doctor 2>&1 | grep -E "FAILED|error\[" | head -5
```
Expected: errors (todo!() panics or missing impl).

- [ ] **Step 3: Implement `doctor.rs`**

Replace the todo stubs with:

```rust
use std::path::Path;
use anyhow::Result;
use crate::ollama;

pub enum Severity {
    Ok,
    Error,
    Warning,
    Info,
}

pub struct CheckResult {
    pub name: &'static str,
    pub severity: Severity,
    pub detail: String,
    pub suggestion: Option<String>,
    pub fixable: bool,
}

pub fn run_doctor(repo_root: &Path) -> Result<Vec<CheckResult>> {
    let mut checks = Vec::new();

    // [1] codi.toml
    let toml_path = repo_root.join("codi.toml");
    if toml_path.exists() {
        let model = read_model_from_toml(&toml_path).unwrap_or_default();
        let detail = if model.is_empty() {
            "mevcut".to_string()
        } else {
            format!("mevcut (model: {model})")
        };
        checks.push(CheckResult {
            name: "codi.toml",
            severity: Severity::Ok,
            detail,
            suggestion: None,
            fixable: false,
        });
    } else {
        checks.push(CheckResult {
            name: "codi.toml",
            severity: Severity::Error,
            detail: "dosya yok".to_string(),
            suggestion: Some("codi init çalıştır".to_string()),
            fixable: false, // --fix does not create codi.toml; user must run codi init
        });
    }

    // [2] Ollama + [3] model installed
    let base_url = "http://localhost:11434/v1";
    if ollama::is_running(base_url) {
        let model = read_model_from_toml(&toml_path).unwrap_or_default();
        if model.is_empty() {
            checks.push(CheckResult {
                name: "Ollama",
                severity: Severity::Ok,
                detail: "çalışıyor".to_string(),
                suggestion: None,
                fixable: false,
            });
        } else {
            let installed = ollama::list_models(base_url)
                .map(|ms| ms.iter().any(|m| m.name == model))
                .unwrap_or(false);
            if installed {
                checks.push(CheckResult {
                    name: "Ollama",
                    severity: Severity::Ok,
                    detail: format!("çalışıyor — {model} yüklü"),
                    suggestion: None,
                    fixable: false,
                });
            } else {
                checks.push(CheckResult {
                    name: "Ollama",
                    severity: Severity::Ok,
                    detail: "çalışıyor".to_string(),
                    suggestion: None,
                    fixable: false,
                });
                checks.push(CheckResult {
                    name: "model",
                    severity: Severity::Error,
                    detail: format!("{model} Ollama'da yüklü değil"),
                    suggestion: Some(format!("ollama pull {model}")),
                    fixable: false,
                });
            }
        }
    } else {
        checks.push(CheckResult {
            name: "Ollama",
            severity: Severity::Error,
            detail: "ulaşılamıyor (http://localhost:11434)".to_string(),
            suggestion: Some("ollama serve".to_string()),
            fixable: false,
        });
    }

    // [4] .mcp.json
    checks.push(check_mcp_json(repo_root));

    // [5] MCP registration (claude CLI)
    checks.push(check_claude_mcp_registration());

    // [6] self_improvement config
    if toml_path.exists() {
        let content = std::fs::read_to_string(&toml_path).unwrap_or_default();
        let has_si = content.contains("[self_improvement]");
        if !has_si {
            checks.push(CheckResult {
                name: "self_improvement",
                severity: Severity::Warning,
                detail: "config yok — varsayılan devre dışı".to_string(),
                suggestion: Some("codi.toml'a [self_improvement] ekle".to_string()),
                fixable: false,
            });
        }
    }

    Ok(checks)
}

pub fn run_doctor_fix(repo_root: &Path) -> Result<Vec<CheckResult>> {
    let mut checks = run_doctor(repo_root)?;

    for check in &mut checks {
        if !matches!(check.severity, Severity::Error) || !check.fixable {
            continue;
        }
        match check.name {
            ".mcp.json" => {
                match crate::init::ensure_mcp_json(repo_root) {
                    Ok(()) => {
                        check.severity = Severity::Ok;
                        check.detail = "düzeltildi".to_string();
                        check.suggestion = None;
                    }
                    Err(e) => {
                        check.detail = format!("düzeltme başarısız: {e:#}");
                    }
                }
            }
            "MCP kaydı" => {
                let ok = std::process::Command::new("claude")
                    .args(["mcp", "add", "codi", "--", "codi", "mcp"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok {
                    check.severity = Severity::Ok;
                    check.detail = "MCP kaydı yapıldı".to_string();
                    check.suggestion = None;
                } else {
                    check.detail = format!("{} (düzeltme başarısız)", check.detail);
                }
            }
            _ => {}
        }
    }

    Ok(checks)
}

pub fn print_doctor_report(checks: &[CheckResult]) -> bool {
    println!("codi doctor — project health check\n");
    let mut error_count = 0usize;
    let mut warning_count = 0usize;
    let mut fixable_count = 0usize;

    for c in checks {
        let symbol = match c.severity {
            Severity::Ok => "✓",
            Severity::Error => {
                error_count += 1;
                if c.fixable {
                    fixable_count += 1;
                }
                "✗"
            }
            Severity::Warning => {
                warning_count += 1;
                "⚠"
            }
            Severity::Info => "ℹ",
        };
        println!("[{symbol}] {:<20} {}", c.name, c.detail);
        if let Some(ref sug) = c.suggestion {
            println!("    → {sug}");
        }
    }

    println!();
    let mut parts: Vec<String> = Vec::new();
    if error_count > 0 {
        parts.push(format!("{error_count} sorun bulundu"));
    }
    if warning_count > 0 {
        parts.push(format!("{warning_count} uyarı"));
    }
    if !parts.is_empty() {
        println!("{}", parts.join(", "));
    }
    if fixable_count > 0 {
        println!("Otomatik düzeltilebilir: codi doctor --fix");
    }

    error_count > 0
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn check_mcp_json(repo_root: &Path) -> CheckResult {
    let path = repo_root.join(".mcp.json");
    if !path.exists() {
        return CheckResult {
            name: ".mcp.json",
            severity: Severity::Error,
            detail: "dosya yok".to_string(),
            suggestion: Some("codi doctor --fix".to_string()),
            fixable: true,
        };
    }
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => {
            return CheckResult {
                name: ".mcp.json",
                severity: Severity::Error,
                detail: "okunamadı".to_string(),
                suggestion: Some("codi doctor --fix".to_string()),
                fixable: true,
            };
        }
    };
    match serde_json::from_str::<serde_json::Value>(&content) {
        Ok(json) => {
            let has_codi = json.get("mcpServers").and_then(|s| s.get("codi")).is_some();
            if has_codi {
                CheckResult {
                    name: ".mcp.json",
                    severity: Severity::Ok,
                    detail: "mevcut — codi kaydı var".to_string(),
                    suggestion: None,
                    fixable: false,
                }
            } else {
                CheckResult {
                    name: ".mcp.json",
                    severity: Severity::Error,
                    detail: "codi kaydı eksik".to_string(),
                    suggestion: Some("codi doctor --fix".to_string()),
                    fixable: true,
                }
            }
        }
        Err(_) => CheckResult {
            name: ".mcp.json",
            severity: Severity::Error,
            detail: "bozuk JSON".to_string(),
            suggestion: Some("codi doctor --fix (yedekler ve yeniden oluşturur)".to_string()),
            fixable: true,
        },
    }
}

fn check_claude_mcp_registration() -> CheckResult {
    // Check if claude CLI is present
    let claude_found = std::process::Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok();

    if !claude_found {
        return CheckResult {
            name: "MCP kaydı",
            severity: Severity::Info,
            detail: "claude CLI yüklü değil — MCP isteğe bağlı".to_string(),
            suggestion: Some("claude mcp add codi -- codi mcp".to_string()),
            fixable: false,
        };
    }

    // Check if codi is registered
    let output = std::process::Command::new("claude")
        .args(["mcp", "list"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            if stdout.contains("codi") {
                CheckResult {
                    name: "MCP kaydı",
                    severity: Severity::Ok,
                    detail: "codi kayıtlı".to_string(),
                    suggestion: None,
                    fixable: false,
                }
            } else {
                CheckResult {
                    name: "MCP kaydı",
                    severity: Severity::Error,
                    detail: "codi kayıtlı değil".to_string(),
                    suggestion: Some("codi doctor --fix".to_string()),
                    fixable: true,
                }
            }
        }
        Err(_) => CheckResult {
            name: "MCP kaydı",
            severity: Severity::Info,
            detail: "kontrol edilemedi".to_string(),
            suggestion: Some("claude mcp add codi -- codi mcp".to_string()),
            fixable: false,
        },
    }
}

fn read_model_from_toml(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let val: toml::Value = toml::from_str(&content).ok()?;
    val.get("model")
        .and_then(|m| m.get("local"))
        .and_then(|l| l.get("model"))
        .and_then(|v| v.as_str())
        .map(String::from)
}
```

- [ ] **Step 4: Add `pub mod doctor;` to `lib.rs`**

In `crates/codi-core/src/lib.rs`, add:
```rust
pub mod doctor;
```

- [ ] **Step 5: Run doctor tests**

```bash
cargo test -p codi-core doctor 2>&1 | tail -10
```
Expected: all 8 doctor tests pass.

- [ ] **Step 6: Full test suite**

```bash
cargo test -p codi-core 2>&1 | tail -5
```
Expected: `test result: ok.`

- [ ] **Step 7: Commit**

```bash
git add crates/codi-core/src/doctor.rs crates/codi-core/src/lib.rs
git commit -m "feat(doctor): add codi doctor / codi doctor --fix health check command"
```

---

### Task 3: `main.rs` — wire `Init` and `Doctor` subcommands, remove auto-wizard

**Files:**
- Modify: `crates/codi-cli/src/main.rs`

**Interfaces:**
- Consumes: `codi_core::init::run_init`, `codi_core::doctor::{run_doctor, run_doctor_fix, print_doctor_report}`
- The existing `first_launch_wizard` import is removed from the `setup` import group.

- [ ] **Step 1: Update imports at the top of `main.rs`**

Change the existing `use codi_core::setup::{...}` import — remove `first_launch_wizard`:

```rust
use codi_core::{
    config::Config,
    doctor::{print_doctor_report, run_doctor, run_doctor_fix},
    engine::{pick_provider_label, post_run_hook, run_session, SessionMode},
    init::run_init,
    mcp,
    review::run_review,
    setup::{check_model, is_first_launch, list_available_models, set_model},
};
```

- [ ] **Step 2: Add `Init` and `Doctor` to `Cmd` enum**

In the `Cmd` enum, add after `Mcp`:

```rust
    /// Set up this project for use with codi (one-time or re-run safely).
    Init {
        /// Overwrite codi.toml from scratch instead of merging.
        #[arg(long)]
        rewrite_config: bool,
    },
    /// Check project health. Add --fix to apply safe auto-fixes.
    Doctor {
        /// Apply safe project-level fixes automatically.
        #[arg(long)]
        fix: bool,
    },
```

- [ ] **Step 3: Replace the first-launch auto-wizard block**

Find this block in `main()` (lines 104-117):

```rust
    // ── First-launch: no config anywhere → run the wizard ───────────────────
    if is_first_launch(&repo_root) {
        // Skip wizard for non-interactive subcommands.
        let skip = matches!(&cli.command, Some(Cmd::Model { .. }) | Some(Cmd::Mcp));
        if !skip {
            if let Err(e) = first_launch_wizard(&repo_root) {
                // User cancelled intentionally — exit cleanly.
                if e.to_string().contains("cancelled") {
                    return Ok(());
                }
                return Err(e);
            }
        }
    }
```

Replace with:

```rust
    // ── First-launch: no config → print onboarding prompt and exit ──────────
    if is_first_launch(&repo_root) {
        let skip = matches!(
            &cli.command,
            Some(Cmd::Model { .. }) | Some(Cmd::Mcp) | Some(Cmd::Init { .. }) | Some(Cmd::Doctor { .. })
        );
        if !skip {
            println!("Bu proje henüz yapılandırılmamış. Başlamak için:\n\n  codi init\n");
            return Ok(());
        }
    }
```

- [ ] **Step 4: Add match arms for `Init` and `Doctor`**

In the `match cli.command` block, add after the `Some(Cmd::Mcp)` arm:

```rust
        Some(Cmd::Init { rewrite_config }) => {
            run_init(&repo_root, rewrite_config)?;
        }
        Some(Cmd::Doctor { fix }) => {
            let checks = if fix {
                run_doctor_fix(&repo_root)?
            } else {
                run_doctor(&repo_root)?
            };
            let has_errors = print_doctor_report(&checks);
            if has_errors {
                std::process::exit(1);
            }
        }
```

- [ ] **Step 5: Compile check**

```bash
cargo build -p codi-cli 2>&1 | grep -E "^error" | head -10
```
Expected: no output (clean build).

- [ ] **Step 6: Smoke test the new subcommands appear in help**

```bash
cargo run -p codi-cli -- --help 2>&1 | grep -E "init|doctor"
```
Expected:
```
  init     Set up this project for use with codi
  doctor   Check project health
```

- [ ] **Step 7: Full test suite**

```bash
cargo test 2>&1 | tail -8
```
Expected: all workspaces pass.

- [ ] **Step 8: Commit**

```bash
git add crates/codi-cli/src/main.rs
git commit -m "feat: add Init and Doctor subcommands; replace auto-wizard with onboarding prompt"
```

---

## Self-Review

**Spec coverage check:**

| Spec requirement | Task |
|-----------------|------|
| `codi init` — Ollama check (abort on fail) | Task 1: `run_init` step 1 |
| Model verify vs Ollama, fallback to picker | Task 1: `select_model` |
| `codi.toml` merge (fill missing, preserve existing) | Task 1: `fill_defaults` + `write_config` |
| `codi.toml` — unknown field warning | Task 1: `Config::from_toml` validation pass |
| `--rewrite-config` flag | Task 1: `write_config` branch |
| `.mcp.json` create / add entry / backup corrupt | Task 1: `ensure_mcp_json` |
| `claude` absent → `[ℹ]` info (not error) | Task 2: `check_claude_mcp_registration` |
| `claude` present, not registered → `[✗]` fixable | Task 2: `check_claude_mcp_registration` |
| `codi doctor` exit 0 / exit 1 semantics | Task 2: `print_doctor_report` return value, Task 3: `std::process::exit(1)` |
| `--fix` for `.mcp.json` and MCP registration | Task 2: `run_doctor_fix` |
| `--fix` does NOT fix Ollama / model / codi.toml | Task 2: `fixable: false` on those checks |
| `[self_improvement]` absent → `[⚠]` warning | Task 2: SI check |
| Remove auto-wizard trigger | Task 3: replaced block |
| First-launch message: `println!`, exit 0 | Task 3: `println!` + `return Ok(())` |
| `Init` / `Doctor` skip first-launch prompt | Task 3: added to skip list |

**Type consistency:** `ensure_mcp_json` is `pub` in `init.rs` and called as `crate::init::ensure_mcp_json` in `doctor.rs` — matches Task 1 interface.

**No placeholders:** all steps have complete code.
