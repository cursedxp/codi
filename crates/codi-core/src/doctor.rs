use std::path::Path;
use anyhow::Result;
use crate::ollama;

pub enum Severity {
    Ok,
    Error,
    Warning,
    Info,
}

#[derive(Debug, PartialEq)]
pub enum CheckId {
    CodiToml,
    Ollama,
    Model,
    McpJson,
    McpRegistration,
    SelfImprovement,
}

pub struct CheckResult {
    pub id: CheckId,
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
            id: CheckId::CodiToml,
            name: "codi.toml",
            severity: Severity::Ok,
            detail,
            suggestion: None,
            fixable: false,
        });
    } else {
        checks.push(CheckResult {
            id: CheckId::CodiToml,
            name: "codi.toml",
            severity: Severity::Error,
            detail: "dosya yok".to_string(),
            suggestion: Some("codi init \u{00e7}al\u{0131}\u{015f}t\u{0131}r".to_string()),
            fixable: false, // --fix does not create codi.toml; user must run codi init
        });
    }

    // [2] Ollama + [3] model installed
    let base_url = "http://localhost:11434/v1";
    if ollama::is_running(base_url) {
        let model = read_model_from_toml(&toml_path).unwrap_or_default();
        if model.is_empty() {
            checks.push(CheckResult {
                id: CheckId::Ollama,
                name: "Ollama",
                severity: Severity::Ok,
                detail: "\u{00e7}al\u{0131}\u{015f}\u{0131}yor".to_string(),
                suggestion: None,
                fixable: false,
            });
        } else {
            let installed = ollama::list_models(base_url)
                .map(|ms| ms.iter().any(|m| m.name == model))
                .unwrap_or(false);
            if installed {
                checks.push(CheckResult {
                    id: CheckId::Ollama,
                    name: "Ollama",
                    severity: Severity::Ok,
                    detail: format!("\u{00e7}al\u{0131}\u{015f}\u{0131}yor \u{2014} {model} y\u{00fc}kl\u{00fc}"),
                    suggestion: None,
                    fixable: false,
                });
            } else {
                checks.push(CheckResult {
                    id: CheckId::Ollama,
                    name: "Ollama",
                    severity: Severity::Ok,
                    detail: "\u{00e7}al\u{0131}\u{015f}\u{0131}yor".to_string(),
                    suggestion: None,
                    fixable: false,
                });
                checks.push(CheckResult {
                    id: CheckId::Model,
                    name: "model",
                    severity: Severity::Error,
                    detail: format!("{model} Ollama'da y\u{00fc}kl\u{00fc} de\u{011f}il"),
                    suggestion: Some(format!("ollama pull {model}")),
                    fixable: false,
                });
            }
        }
    } else {
        checks.push(CheckResult {
            id: CheckId::Ollama,
            name: "Ollama",
            severity: Severity::Error,
            detail: "ula\u{015f}\u{0131}lam\u{0131}yor (http://localhost:11434)".to_string(),
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
                id: CheckId::SelfImprovement,
                name: "self_improvement",
                severity: Severity::Warning,
                detail: "a\u{00e7}\u{0131}k\u{00e7}a yap\u{0131}land\u{0131}r\u{0131}lmam\u{0131}\u{015f} \u{2014} varsay\u{0131}lan de\u{011f}erler kullan\u{0131}l\u{0131}yor".to_string(),
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
        match check.id {
            CheckId::McpJson => {
                match crate::init::ensure_mcp_json(repo_root) {
                    Ok(()) => {
                        check.severity = Severity::Ok;
                        check.detail = "d\u{00fc}zeltildi".to_string();
                        check.suggestion = None;
                    }
                    Err(e) => {
                        check.detail = format!("d\u{00fc}zeltme ba\u{015f}ar\u{0131}s\u{0131}z: {e:#}");
                    }
                }
            }
            CheckId::McpRegistration => {
                let ok = std::process::Command::new("claude")
                    .args(["mcp", "add", "codi", "--", "codi", "mcp"])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok {
                    check.severity = Severity::Ok;
                    check.detail = "MCP kayd\u{0131} yap\u{0131}ld\u{0131}".to_string();
                    check.suggestion = None;
                } else {
                    check.detail = format!("{} (d\u{00fc}zeltme ba\u{015f}ar\u{0131}s\u{0131}z)", check.detail);
                }
            }
            _ => {}
        }
    }

    Ok(checks)
}

pub fn print_doctor_report(checks: &[CheckResult]) -> bool {
    println!("codi doctor \u{2014} project health check\n");
    let mut error_count = 0usize;
    let mut warning_count = 0usize;
    let mut fixable_count = 0usize;

    for c in checks {
        let symbol = match c.severity {
            Severity::Ok => "\u{2713}",
            Severity::Error => {
                error_count += 1;
                if c.fixable {
                    fixable_count += 1;
                }
                "\u{2717}"
            }
            Severity::Warning => {
                warning_count += 1;
                "\u{26a0}"
            }
            Severity::Info => "\u{2139}",
        };
        println!("[{symbol}] {:<20} {}", c.name, c.detail);
        if let Some(ref sug) = c.suggestion {
            println!("    \u{2192} {sug}");
        }
    }

    println!();
    let mut parts: Vec<String> = Vec::new();
    if error_count > 0 {
        parts.push(format!("{error_count} sorun bulundu"));
    }
    if warning_count > 0 {
        parts.push(format!("{warning_count} uyard\u{0131}"));
    }
    if !parts.is_empty() {
        println!("{}", parts.join(", "));
    }
    if fixable_count > 0 {
        println!("Otomatik d\u{00fc}zeltilebilir: codi doctor --fix");
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
            id: CheckId::McpJson,
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
                id: CheckId::McpJson,
                name: ".mcp.json",
                severity: Severity::Error,
                detail: "okunamad\u{0131}".to_string(),
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
                    id: CheckId::McpJson,
                    name: ".mcp.json",
                    severity: Severity::Ok,
                    detail: "mevcut \u{2014} codi kayd\u{0131} var".to_string(),
                    suggestion: None,
                    fixable: false,
                }
            } else {
                CheckResult {
                    id: CheckId::McpJson,
                    name: ".mcp.json",
                    severity: Severity::Error,
                    detail: "codi kayd\u{0131} eksik".to_string(),
                    suggestion: Some("codi doctor --fix".to_string()),
                    fixable: true,
                }
            }
        }
        Err(_) => CheckResult {
            id: CheckId::McpJson,
            name: ".mcp.json",
            severity: Severity::Error,
            detail: "bozuk JSON".to_string(),
            suggestion: Some("codi doctor --fix (yedekler ve yeniden olu\u{015f}turur)".to_string()),
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
            id: CheckId::McpRegistration,
            name: "MCP kayd\u{0131}",
            severity: Severity::Info,
            detail: "claude CLI y\u{00fc}kl\u{00fc} de\u{011f}il \u{2014} MCP iste\u{011f}e ba\u{011f}l\u{0131}".to_string(),
            suggestion: Some("claude mcp add codi -- codi mcp".to_string()),
            fixable: false,
        },
        Err(_) => CheckResult {
            id: CheckId::McpRegistration,
            name: "MCP kayd\u{0131}",
            severity: Severity::Info,
            detail: "kontrol edilemedi".to_string(),
            suggestion: Some("claude mcp add codi -- codi mcp".to_string()),
            fixable: false,
        },
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let codi_registered = stdout.lines().any(|line| {
                let word = line.split_whitespace().next().unwrap_or("");
                word == "codi" || word == "codi:"
            });
            if codi_registered {
                CheckResult {
                    id: CheckId::McpRegistration,
                    name: "MCP kayd\u{0131}",
                    severity: Severity::Ok,
                    detail: "codi kay\u{0131}tl\u{0131}".to_string(),
                    suggestion: None,
                    fixable: false,
                }
            } else {
                CheckResult {
                    id: CheckId::McpRegistration,
                    name: "MCP kayd\u{0131}",
                    severity: Severity::Error,
                    detail: "codi kay\u{0131}tl\u{0131} de\u{011f}il".to_string(),
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
        assert_eq!(toml_check.id, CheckId::CodiToml);
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
        let mcp_check = checks.iter().find(|c| c.id == CheckId::McpJson).unwrap();
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
        let mcp_check = checks.iter().find(|c| c.id == CheckId::McpJson).unwrap();
        assert!(matches!(mcp_check.severity, Severity::Ok));
    }

    #[test]
    fn check_mcp_json_corrupt_returns_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "not json {{{").unwrap();
        let checks = run_doctor(dir.path()).unwrap();
        let mcp_check = checks.iter().find(|c| c.id == CheckId::McpJson).unwrap();
        assert!(matches!(mcp_check.severity, Severity::Error));
    }

    #[test]
    fn print_doctor_report_returns_false_when_no_errors() {
        let checks = vec![
            CheckResult {
                id: CheckId::CodiToml,
                name: "codi.toml",
                severity: Severity::Ok,
                detail: "mevcut".to_string(),
                suggestion: None,
                fixable: false,
            },
            CheckResult {
                id: CheckId::SelfImprovement,
                name: "self_improvement",
                severity: Severity::Warning,
                detail: "configured".to_string(),
                suggestion: None,
                fixable: false,
            },
        ];
        assert!(!print_doctor_report(&checks), "no errors must return false");
    }

    #[test]
    fn print_doctor_report_returns_true_when_error_present() {
        let checks = vec![CheckResult {
            id: CheckId::McpJson,
            name: ".mcp.json",
            severity: Severity::Error,
            detail: "dosya yok".to_string(),
            suggestion: None,
            fixable: true,
        }];
        assert!(print_doctor_report(&checks), "error present must return true");
    }

    #[test]
    fn doctor_fix_creates_mcp_json_and_marks_ok() {
        let dir = tempdir().unwrap();
        let checks = run_doctor_fix(dir.path()).unwrap();
        assert!(dir.path().join(".mcp.json").exists(), "file must be created");
        let mcp = checks.iter().find(|c| c.id == CheckId::McpJson)
            .expect("McpJson check must be present");
        assert!(matches!(mcp.severity, Severity::Ok), "severity must be Ok after fix");
    }

    #[test]
    fn self_improvement_absent_is_warning_not_error() {
        let dir = tempdir().unwrap();
        init_toml(dir.path(), "qwen2.5:7b");
        let checks = run_doctor(dir.path()).unwrap();
        let si_check = checks.iter().find(|c| c.id == CheckId::SelfImprovement)
            .expect("self_improvement check must be present when codi.toml has no [self_improvement] section");
        assert!(matches!(si_check.severity, Severity::Warning));
    }
}
