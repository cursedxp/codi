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
                detail: "açıkça yapılandırılmamış — varsayılan değerler kullanılıyor".to_string(),
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
    let output = std::process::Command::new("claude")
        .args(["mcp", "list"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => CheckResult {
            name: "MCP kaydı",
            severity: Severity::Info,
            detail: "claude CLI yüklü değil — MCP isteğe bağlı".to_string(),
            suggestion: Some("claude mcp add codi -- codi mcp".to_string()),
            fixable: false,
        },
        Err(_) => CheckResult {
            name: "MCP kaydı",
            severity: Severity::Info,
            detail: "kontrol edilemedi".to_string(),
            suggestion: Some("claude mcp add codi -- codi mcp".to_string()),
            fixable: false,
        },
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
        init_toml(dir.path(), "qwen2.5:7b");
        let checks = run_doctor(dir.path()).unwrap();
        let si_check = checks.iter().find(|c| c.name == "self_improvement")
            .expect("self_improvement check must be present when codi.toml has no [self_improvement] section");
        assert!(matches!(si_check.severity, Severity::Warning));
    }
}
