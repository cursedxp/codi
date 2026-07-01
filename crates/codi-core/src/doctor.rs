use std::path::Path;
use anyhow::Result;
use crate::config::Config;
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
    ClaudeMd,
    ReliabilityLog,
}

pub struct CheckResult {
    pub id: CheckId,
    pub name: &'static str,
    pub severity: Severity,
    pub detail: String,
    pub suggestion: Option<String>,
    pub fixable: bool,
}

pub fn run_doctor(repo_root: &Path, cfg: &Config) -> Result<Vec<CheckResult>> {
    let mut checks = Vec::new();

    // [1] codi.toml
    let toml_path = repo_root.join("codi.toml");
    if toml_path.exists() {
        let model = read_model_from_toml(&toml_path).unwrap_or_default();
        let detail = if model.is_empty() {
            "present".to_string()
        } else {
            format!("present (model: {model})")
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
            detail: "missing".to_string(),
            suggestion: Some("run codi init".to_string()),
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
                detail: "running".to_string(),
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
                    detail: format!("running \u{2014} {model} installed"),
                    suggestion: None,
                    fixable: false,
                });
            } else {
                checks.push(CheckResult {
                    id: CheckId::Ollama,
                    name: "Ollama",
                    severity: Severity::Ok,
                    detail: "running".to_string(),
                    suggestion: None,
                    fixable: false,
                });
                checks.push(CheckResult {
                    id: CheckId::Model,
                    name: "model",
                    severity: Severity::Error,
                    detail: format!("{model} not installed in Ollama"),
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
            detail: "unreachable (http://localhost:11434)".to_string(),
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
                detail: "not explicitly configured \u{2014} using defaults".to_string(),
                suggestion: Some("codi.toml'a [self_improvement] ekle".to_string()),
                fixable: false,
            });
        }
    }

    // [7] CLAUDE.md
    checks.push(check_claude_md(repo_root));

    // [8] reliability log
    checks.push(check_reliability_log(repo_root, cfg));

    Ok(checks)
}

pub fn run_doctor_fix(repo_root: &Path, cfg: &Config) -> Result<Vec<CheckResult>> {
    let mut checks = run_doctor(repo_root, cfg)?;

    for check in &mut checks {
        if !matches!(check.severity, Severity::Error) || !check.fixable {
            continue;
        }
        match check.id {
            CheckId::McpJson => {
                match crate::init::ensure_mcp_json(repo_root) {
                    Ok(()) => {
                        check.severity = Severity::Ok;
                        check.detail = "fixed".to_string();
                        check.suggestion = None;
                    }
                    Err(e) => {
                        check.detail = format!("fix failed: {e:#}");
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
                    check.detail = "MCP registered".to_string();
                    check.suggestion = None;
                } else {
                    check.detail = format!("{} (fix failed)", check.detail);
                }
            }
            CheckId::ClaudeMd => {
                match crate::init::ensure_claude_md(repo_root) {
                    Ok(()) => {
                        check.severity = Severity::Ok;
                        check.detail = "fixed".to_string();
                        check.suggestion = None;
                    }
                    Err(e) => {
                        check.detail = format!("fix failed: {e:#}");
                    }
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
        parts.push(format!("{error_count} error(s) found"));
    }
    if warning_count > 0 {
        parts.push(format!("{warning_count} warning(s)"));
    }
    if !parts.is_empty() {
        println!("{}", parts.join(", "));
    }
    if fixable_count > 0 {
        println!("Auto-fixable: codi doctor --fix");
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
            detail: "missing".to_string(),
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
                detail: "unreadable".to_string(),
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
                    detail: "present \u{2014} codi entry found".to_string(),
                    suggestion: None,
                    fixable: false,
                }
            } else {
                CheckResult {
                    id: CheckId::McpJson,
                    name: ".mcp.json",
                    severity: Severity::Error,
                    detail: "codi entry missing".to_string(),
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
            suggestion: Some("codi doctor --fix (backs up and recreates)".to_string()),
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
            name: "MCP registration",
            severity: Severity::Info,
            detail: "claude CLI not installed \u{2014} MCP optional".to_string(),
            suggestion: Some("claude mcp add codi -- codi mcp".to_string()),
            fixable: false,
        },
        Err(_) => CheckResult {
            id: CheckId::McpRegistration,
            name: "MCP registration",
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
                    name: "MCP registration",
                    severity: Severity::Ok,
                    detail: "codi registered".to_string(),
                    suggestion: None,
                    fixable: false,
                }
            } else {
                CheckResult {
                    id: CheckId::McpRegistration,
                    name: "MCP registration",
                    severity: Severity::Error,
                    detail: "codi not registered".to_string(),
                    suggestion: Some("codi doctor --fix".to_string()),
                    fixable: true,
                }
            }
        }
    }
}

fn check_claude_md(repo_root: &Path) -> CheckResult {
    let path = repo_root.join("CLAUDE.md");
    if !path.exists() {
        return CheckResult {
            id: CheckId::ClaudeMd,
            name: "CLAUDE.md",
            severity: Severity::Error,
            detail: "missing".to_string(),
            suggestion: Some("codi doctor --fix".to_string()),
            fixable: true,
        };
    }
    let content = std::fs::read_to_string(&path).unwrap_or_default();
    if content.contains("## codi") {
        CheckResult {
            id: CheckId::ClaudeMd,
            name: "CLAUDE.md",
            severity: Severity::Ok,
            detail: "codi delegation guidance present".to_string(),
            suggestion: None,
            fixable: false,
        }
    } else {
        CheckResult {
            id: CheckId::ClaudeMd,
            name: "CLAUDE.md",
            severity: Severity::Error,
            detail: "codi section missing".to_string(),
            suggestion: Some("codi doctor --fix".to_string()),
            fixable: true,
        }
    }
}

fn check_reliability_log(repo_root: &Path, cfg: &Config) -> CheckResult {
    let log_path = match crate::reliability::resolve_log_path(repo_root, &cfg.reliability.log_path) {
        Ok(p) => p,
        Err(_) => {
            return CheckResult {
                id: CheckId::ReliabilityLog,
                name: "reliability",
                severity: Severity::Info,
                detail: "invalid log_path config".to_string(),
                suggestion: None,
                fixable: false,
            };
        }
    };

    if !log_path.exists() {
        return CheckResult {
            id: CheckId::ReliabilityLog,
            name: "reliability",
            severity: Severity::Info,
            detail: "no log yet — reliability layer has not run".to_string(),
            suggestion: None,
            fixable: false,
        };
    }

    let content = std::fs::read_to_string(&log_path).unwrap_or_default();
    let events: Vec<serde_json::Value> = content
        .lines()
        .filter(|l| !l.is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    // "retrying" events are intermediate attempts, not terminal outcomes —
    // counting them would drag the success rate for tasks that recovered.
    let last_20: Vec<&serde_json::Value> = events
        .iter()
        .rev()
        .filter(|e| e.get("outcome").and_then(|v| v.as_str()) != Some("retrying"))
        .take(20)
        .collect();
    let total = last_20.len();

    if total == 0 {
        return CheckResult {
            id: CheckId::ReliabilityLog,
            name: "reliability",
            severity: Severity::Info,
            detail: "log file is empty".to_string(),
            suggestion: None,
            fixable: false,
        };
    }

    let succeeded = last_20.iter().filter(|e| {
        matches!(
            e.get("outcome").and_then(|v| v.as_str()).unwrap_or(""),
            "success" | "retry_success" | "escalation_success"
        )
    }).count();

    let silent_failures = last_20.iter().filter(|e| {
        e.get("verification").and_then(|v| v.as_str()).unwrap_or("").contains("no_diff")
    }).count();

    let escalations = last_20.iter().filter(|e| {
        e.get("outcome").and_then(|v| v.as_str()).unwrap_or("").contains("escalation")
    }).count();

    let success_pct = (succeeded * 100) / total;

    let severity = if silent_failures > 0 || success_pct < 70 {
        Severity::Error
    } else if success_pct < 90 {
        Severity::Warning
    } else {
        Severity::Ok
    };

    let detail = if success_pct == 100 {
        format!("100% success ({succeeded}/{total}) — last {total} events")
    } else {
        let mut parts = vec![format!("{success_pct}% success ({succeeded}/{total})")];
        if silent_failures > 0 {
            parts.push(format!("{silent_failures} silent failures"));
        }
        if escalations > 0 {
            parts.push(format!("{escalations} escalations"));
        }
        parts.join(", ")
    };

    let suggestion = if matches!(severity, Severity::Ok) {
        None
    } else {
        Some("cat .codi/reliability.jsonl | jq .".to_string())
    };

    CheckResult {
        id: CheckId::ReliabilityLog,
        name: "reliability",
        severity,
        detail,
        suggestion,
        fixable: false,
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
    use crate::config::Config;
    use tempfile::tempdir;

    fn default_cfg() -> Config { Config::default() }

    fn init_toml(dir: &std::path::Path, model: &str) {
        let content = format!(
            "[model.local]\nmodel = \"{model}\"\nbase_url = \"http://localhost:11434/v1\"\napi_key = \"\"\n"
        );
        std::fs::write(dir.join("codi.toml"), content).unwrap();
    }

    #[test]
    fn check_toml_missing_returns_error() {
        let dir = tempdir().unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let toml_check = checks.iter().find(|c| c.name == "codi.toml").unwrap();
        assert!(matches!(toml_check.severity, Severity::Error));
        assert_eq!(toml_check.id, CheckId::CodiToml);
    }

    #[test]
    fn check_toml_present_returns_ok() {
        let dir = tempdir().unwrap();
        init_toml(dir.path(), "qwen2.5:7b");
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let toml_check = checks.iter().find(|c| c.name == "codi.toml").unwrap();
        assert!(matches!(toml_check.severity, Severity::Ok));
    }

    #[test]
    fn check_mcp_json_missing_returns_error() {
        let dir = tempdir().unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
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
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let mcp_check = checks.iter().find(|c| c.id == CheckId::McpJson).unwrap();
        assert!(matches!(mcp_check.severity, Severity::Ok));
    }

    #[test]
    fn check_mcp_json_corrupt_returns_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(".mcp.json"), "not json {{{").unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
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
                detail: "present".to_string(),
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
            detail: "missing".to_string(),
            suggestion: None,
            fixable: true,
        }];
        assert!(print_doctor_report(&checks), "error present must return true");
    }

    #[test]
    fn doctor_fix_creates_mcp_json_and_marks_ok() {
        let dir = tempdir().unwrap();
        let checks = run_doctor_fix(dir.path(), &default_cfg()).unwrap();
        assert!(dir.path().join(".mcp.json").exists(), "file must be created");
        let mcp = checks.iter().find(|c| c.id == CheckId::McpJson)
            .expect("McpJson check must be present");
        assert!(matches!(mcp.severity, Severity::Ok), "severity must be Ok after fix");
    }

    #[test]
    fn self_improvement_absent_is_warning_not_error() {
        let dir = tempdir().unwrap();
        init_toml(dir.path(), "qwen2.5:7b");
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let si_check = checks.iter().find(|c| c.id == CheckId::SelfImprovement)
            .expect("self_improvement check must be present when codi.toml has no [self_improvement] section");
        assert!(matches!(si_check.severity, Severity::Warning));
    }

    #[test]
    fn check_claude_md_missing_returns_error() {
        let dir = tempdir().unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd).unwrap();
        assert!(matches!(c.severity, Severity::Error));
        assert!(c.fixable);
    }

    #[test]
    fn check_claude_md_without_codi_section_returns_error() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# My Project\n").unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd).unwrap();
        assert!(matches!(c.severity, Severity::Error));
        assert!(c.fixable);
    }

    #[test]
    fn check_claude_md_with_codi_section_returns_ok() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("CLAUDE.md"), "# Project\n\n## codi\n\nContent.\n").unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd).unwrap();
        assert!(matches!(c.severity, Severity::Ok));
    }

    #[test]
    fn doctor_fix_creates_claude_md_and_marks_ok() {
        let dir = tempdir().unwrap();
        let checks = run_doctor_fix(dir.path(), &default_cfg()).unwrap();
        assert!(dir.path().join("CLAUDE.md").exists(), "CLAUDE.md must be created");
        let c = checks.iter().find(|c| c.id == CheckId::ClaudeMd)
            .expect("ClaudeMd check must be present");
        assert!(matches!(c.severity, Severity::Ok), "severity must be Ok after fix");
    }

    #[test]
    fn reliability_log_missing_returns_info() {
        let dir = tempdir().unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
        assert!(matches!(c.severity, Severity::Info));
    }

    #[test]
    fn reliability_log_all_success_returns_ok() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join(".codi");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("reliability.jsonl");
        let success_line = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"pass","outcome":"success","decision_reason":"ok","timestamp":1}"#;
        use std::io::Write as _;
        for _ in 0..5 {
            writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{success_line}").unwrap();
        }
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
        assert!(matches!(c.severity, Severity::Ok), "detail: {}", c.detail);
    }

    #[test]
    fn reliability_log_silent_failures_returns_error() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join(".codi");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("reliability.jsonl");
        let success = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"pass","outcome":"success","decision_reason":"ok","timestamp":1}"#;
        let fail   = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"no_diff","outcome":"fail","decision_reason":"ok","timestamp":1}"#;
        use std::io::Write as _;
        for line in [success, success, fail, fail, fail] {
            writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{line}").unwrap();
        }
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
        assert!(matches!(c.severity, Severity::Error), "detail: {}", c.detail);
    }

    #[test]
    fn reliability_log_empty_file_returns_info() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join(".codi");
        std::fs::create_dir_all(&log_dir).unwrap();
        std::fs::write(log_dir.join("reliability.jsonl"), "").unwrap();
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
        assert!(matches!(c.severity, Severity::Info), "detail: {}", c.detail);
    }

    #[test]
    fn reliability_log_below_70_pct_returns_error() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join(".codi");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("reliability.jsonl");
        let success = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"pass","outcome":"success","decision_reason":"ok","timestamp":1}"#;
        let fail   = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":1,"verification":"nonzero_exit:1","outcome":"fail","decision_reason":"ok","timestamp":1}"#;
        use std::io::Write as _;
        // 2 success, 4 fail → 33% success < 70%
        for line in [success, success, fail, fail, fail, fail] {
            writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{line}").unwrap();
        }
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
        assert!(matches!(c.severity, Severity::Error), "detail: {}", c.detail);
    }

    #[test]
    fn reliability_log_between_70_and_90_pct_returns_warning() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join(".codi");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("reliability.jsonl");
        let success = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":0,"verification":"pass","outcome":"success","decision_reason":"ok","timestamp":1}"#;
        let fail   = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"local","attempt":1,"exit_code":1,"verification":"nonzero_exit:1","outcome":"fail","decision_reason":"ok","timestamp":1}"#;
        use std::io::Write as _;
        // 8 success, 2 fail → 80% → Warning (≥70 but <90)
        for _ in 0..8 { writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{success}").unwrap(); }
        for _ in 0..2 { writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{fail}").unwrap(); }
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
        assert!(matches!(c.severity, Severity::Warning), "detail: {}", c.detail);
    }

    #[test]
    fn reliability_log_escalation_count_in_detail() {
        let dir = tempdir().unwrap();
        let log_dir = dir.path().join(".codi");
        std::fs::create_dir_all(&log_dir).unwrap();
        let log_path = log_dir.join("reliability.jsonl");
        let escalation = r#"{"task_id":"t","task_snippet":"x","step_index":0,"execution_mode":"single_shot","provider":"cloud","attempt":3,"exit_code":0,"verification":"pass","outcome":"escalation_success","decision_reason":"ok","timestamp":1}"#;
        use std::io::Write as _;
        for _ in 0..5 {
            writeln!(std::fs::OpenOptions::new().create(true).append(true).open(&log_path).unwrap(), "{escalation}").unwrap();
        }
        let checks = run_doctor(dir.path(), &default_cfg()).unwrap();
        let c = checks.iter().find(|c| c.id == CheckId::ReliabilityLog).unwrap();
        // 5 escalation_success events → succeeded=5, total=5 → Ok severity
        // But escalation count (5) should appear in the detail
        assert!(c.detail.contains('5'), "detail should mention escalation count: {}", c.detail);
    }
}
