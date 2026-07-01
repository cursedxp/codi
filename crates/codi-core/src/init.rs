use std::path::Path;
use anyhow::{Context, Result};
use crate::ollama;
use crate::setup::{detect_ollama, pick_model_interactive};

const MCP_JSON_CONTENT: &str = "{\n  \"mcpServers\": {\n    \"codi\": {\n      \"command\": \"codi\",\n      \"args\": [\"mcp\"]\n    }\n  }\n}\n";
const CODI_MCP_KEY: &str = "codi";
const CODI_MD_START: &str = "<!-- codi:start -->";
const CODI_MD_END: &str = "<!-- codi:end -->";

/// Orchestration guidance for Claude Code. codi runs a weak local model, so a
/// large task must be decomposed to fit the model's capacity. This block is
/// delimited so `codi init` can refresh it in place on re-run.
const CLAUDE_MD_BODY: &str = "## codi — yerel AI kodlama ajanı

Bu projede kod işlerini codi'ye (yerel LLM) devret. codi zayıf bir yerel modelle
çalışır — büyük bir task'ı tek seferde veremezsin, modele sığacak parçalara bölmen
gerekir.

**İş akışı:**

1. **Modeli tanı.** Devretmeden önce `codi model list` çalıştır (uyumluluk + boyut
   gösterir). Model `7b` veya altıysa (Small tier) yalnızca küçük, tek amaçlı
   adımları güvenle tamamlar.
2. **Task'ı modele göre böl.** İşi en küçük anlamlı adımlara ayır: bir dosya, bir
   fonksiyon, ya da küçük bir kayıt grubu. Small modele büyük çok parçalı task verme.
3. **Adımları tek tek ver.** Her `run_task` çağrısında hedef dosyayı açıkça belirt
   ki codi doğrulayabilsin.
4. **Her adımı doğrula.** `get_diff` ile ya da dosyayı okuyarak kontrol et. Çıktı
   bozuk/eksikse adımı daha da böl ve tekrar dene.

**Araçlar:**
- **Kod yaz / refactor / bug fix** → `run_task` (tek küçük adım)
- **Değişiklikleri incele** → `get_diff`
- **Testleri çalıştır** → `run_tests`

**Roller:** Claude = planla, böl, incele, doğrula. codi = adımı yerel LLM ile uygula.";

/// The full delimited block written to CLAUDE.md.
fn codi_block() -> String {
    format!("{CODI_MD_START}\n{CLAUDE_MD_BODY}\n{CODI_MD_END}\n")
}

pub fn run_init(repo_root: &Path, rewrite_config: bool) -> Result<()> {
    println!("\n┌─────────────────────────────────────┐");
    println!("│  codi init — project setup          │");
    println!("└─────────────────────────────────────┘\n");

    // [1/6] Ollama
    println!("[1/6] Ollama kontrolü");
    let base_url = detect_ollama().map_err(|e| {
        println!("✗ Ollama bulunamadı. Kur: brew install ollama && ollama serve");
        e
    })?;
    println!("  ✓ Ollama çalışıyor ({})", base_url);

    // [2/6] Model
    println!("[2/6] Model seçimi");
    let model = select_model(repo_root, &base_url, rewrite_config)?;

    // [3/6] codi.toml
    println!("[3/6] codi.toml");
    write_config(repo_root, &base_url, &model, rewrite_config)?;

    // [4/6] .mcp.json
    println!("[4/6] .mcp.json");
    ensure_mcp_json(repo_root)?;

    // [5/6] MCP registration
    println!("[5/6] MCP kaydı");
    register_mcp_claude();

    // [6/6] CLAUDE.md
    println!("[6/6] CLAUDE.md");
    ensure_claude_md(repo_root)?;

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
    // MCP runs non-interactively (stdin=null); confirmation prompts would block forever.
    fresh.safety.confirm_writes = false;
    fresh.safety.confirm_commands = false;

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

pub(crate) fn ensure_claude_md(repo_root: &Path) -> Result<()> {
    let path = repo_root.join("CLAUDE.md");
    let block = codi_block();

    if !path.exists() {
        std::fs::write(&path, &block).context("writing CLAUDE.md")?;
        println!("  \u{2713} CLAUDE.md olu\u{015f}turuldu");
        return Ok(());
    }

    let content = std::fs::read_to_string(&path).context("reading CLAUDE.md")?;

    // If a delimited codi block already exists, replace it in place so the
    // guidance stays current on re-runs.
    if let (Some(start), Some(end_pos)) = (content.find(CODI_MD_START), content.find(CODI_MD_END)) {
        let end = end_pos + CODI_MD_END.len();
        if start < end {
            let mut updated = String::with_capacity(content.len());
            updated.push_str(&content[..start]);
            updated.push_str(block.trim_end_matches('\n'));
            updated.push_str(&content[end..]);
            if updated == content {
                println!("  \u{2713} CLAUDE.md \u{2014} de\u{011f}i\u{015f}tirilmedi");
            } else {
                std::fs::write(&path, &updated).context("writing CLAUDE.md")?;
                println!("  \u{2713} CLAUDE.md \u{2014} codi b\u{00f6}l\u{00fc}m\u{00fc} g\u{00fc}ncellendi");
            }
            return Ok(());
        }
    }

    // Legacy pre-delimiter section ("## codi ..."): migrate it in place to the
    // delimited block instead of appending a duplicate. The section runs from its
    // heading to the next top-level "## " heading (or EOF).
    if let Some(hstart) = content.find("## codi") {
        let search_from = hstart + "## codi".len();
        let hend = content[search_from..]
            .find("\n## ")
            .map(|i| search_from + i + 1)
            .unwrap_or(content.len());
        let mut updated = String::with_capacity(content.len());
        updated.push_str(&content[..hstart]);
        updated.push_str(block.trim_end_matches('\n'));
        if hend < content.len() {
            updated.push_str("\n\n");
            updated.push_str(content[hend..].trim_start_matches('\n'));
        } else {
            updated.push('\n');
        }
        std::fs::write(&path, &updated).context("writing CLAUDE.md")?;
        println!("  \u{2713} CLAUDE.md \u{2014} eski codi b\u{00f6}l\u{00fc}m\u{00fc} g\u{00fc}ncellendi");
        return Ok(());
    }

    // No codi block yet: append one, preserving existing content.
    use std::io::Write as IoWrite;
    let sep = if content.ends_with('\n') { "\n" } else { "\n\n" };
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .context("opening CLAUDE.md for append")?;
    file.write_all(format!("{sep}{block}").as_bytes()).context("appending to CLAUDE.md")?;
    println!("  \u{2713} CLAUDE.md \u{2014} codi b\u{00f6}l\u{00fc}m\u{00fc} eklendi");
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

    #[test]
    fn ensure_claude_md_creates_file_when_absent() {
        let dir = tempdir().unwrap();
        ensure_claude_md(dir.path()).unwrap();
        let content = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert!(content.contains(CODI_MD_START), "must contain start marker");
        assert!(content.contains(CODI_MD_END), "must contain end marker");
        assert!(content.contains("run_task"), "must mention run_task");
        assert!(content.contains("codi model list"), "must include capacity guidance");
    }

    #[test]
    fn ensure_claude_md_appends_block_when_no_codi_present() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "# My Project\n\nExisting content.\n").unwrap();
        ensure_claude_md(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.starts_with("# My Project"), "existing content preserved");
        assert!(content.contains(CODI_MD_START), "codi block appended");
    }

    #[test]
    fn ensure_claude_md_replaces_block_and_preserves_outside() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let original = format!(
            "# My Project\n\n{CODI_MD_START}\nOLD STALE CONTENT\n{CODI_MD_END}\n\n## Other\nkeep me\n"
        );
        std::fs::write(&path, &original).unwrap();
        ensure_claude_md(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("OLD STALE CONTENT"), "stale block replaced");
        assert!(content.contains("codi model list"), "fresh guidance present");
        assert!(content.contains("# My Project"), "content before block preserved");
        assert!(content.contains("## Other\nkeep me"), "content after block preserved");
        // Exactly one marker pair.
        assert_eq!(content.matches(CODI_MD_START).count(), 1, "single start marker");
        assert_eq!(content.matches(CODI_MD_END).count(), 1, "single end marker");
    }

    #[test]
    fn ensure_claude_md_migrates_legacy_bare_section() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let legacy = "# My Project\n\n## codi — AI coding agent\n\nEski içerik, run_task falan.\n\n## Other\nkeep me\n";
        std::fs::write(&path, legacy).unwrap();
        ensure_claude_md(dir.path()).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains(CODI_MD_START), "delimited block added");
        assert!(content.contains("codi model list"), "fresh guidance present");
        assert!(!content.contains("Eski içerik"), "legacy section removed");
        assert_eq!(content.matches("## codi").count(), 1, "no duplicate codi heading");
        assert!(content.contains("# My Project"), "title preserved");
        assert!(content.contains("## Other\nkeep me"), "sibling section preserved");
    }

    #[test]
    fn ensure_claude_md_is_idempotent_on_rerun() {
        let dir = tempdir().unwrap();
        ensure_claude_md(dir.path()).unwrap();
        let first = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        ensure_claude_md(dir.path()).unwrap();
        let second = std::fs::read_to_string(dir.path().join("CLAUDE.md")).unwrap();
        assert_eq!(first, second, "re-running must not change the file");
    }
}
