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
        println!("✗ Ollama bulunamadı. Kur: brew install ollama && ollama serve");
        e
    })?;
    println!("  ✓ Ollama çalışıyor ({})", base_url);

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

    let (mut merged, added) = fill_defaults(existing, defaults);

    // Overwrite model fields with the user-selected values (fill_defaults never overwrites,
    // but we need to apply the new model selection even on re-runs).
    if let toml::Value::Table(ref mut t) = merged {
        let model_tbl = t
            .entry("model".to_string())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let toml::Value::Table(ref mut mt) = model_tbl {
            let local_tbl = mt
                .entry("local".to_string())
                .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
            if let toml::Value::Table(ref mut lt) = local_tbl {
                lt.insert("model".to_string(), toml::Value::String(model.to_string()));
                lt.insert("base_url".to_string(), toml::Value::String(base_url.to_string()));
            }
        }
    }

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
pub(crate) fn fill_defaults(existing: toml::Value, defaults: toml::Value) -> (toml::Value, usize) {
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

pub(crate) fn ensure_mcp_json(repo_root: &Path) -> Result<()> {
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
    let probe = std::process::Command::new("claude")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    if matches!(&probe, Err(e) if e.kind() == std::io::ErrorKind::NotFound) {
        println!("  [\u{2139}] MCP kayd\u{0131} atland\u{0131} \u{2014} claude CLI y\u{00fc}kl\u{00fc} de\u{011f}il.");
        println!("      Manuel kay\u{0131}t: claude mcp add codi -- codi mcp");
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

    #[test]
    fn ensure_mcp_json_handles_json_without_mcp_servers_key() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), r#"{"version": 1}"#).unwrap();
        ensure_mcp_json(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join(".mcp.json")).unwrap();
        let json: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(json["mcpServers"]["codi"]["command"].as_str(), Some("codi"));
    }

    #[test]
    fn write_config_merge_overwrites_model_selection() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("codi.toml"), concat!(
            "[model.local]\n",
            "model = \"old-model\"\n",
            "base_url = \"http://localhost:11434/v1\"\n",
            "api_key = \"\"\n",
        )).unwrap();
        write_config(dir.path(), "http://localhost:11434/v1", "new-model", false).unwrap();
        let content = std::fs::read_to_string(dir.path().join("codi.toml")).unwrap();
        assert!(content.contains("new-model"), "model must be overwritten");
        assert!(!content.contains("old-model"), "old model must not remain");
    }
}
