//! Signal collection from post-run artifacts.

use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SignalKind {
    LintWarning { category: String, detail: String },
    TestFailure { test_name: String, module: String },
    DiffWithoutTest,
    /// context_radius: how many neighbouring modules to scan (0 = changed files only).
    TodoFixme { text: String, file: String, context_radius: usize },
    /// Separate from code-quality signals — tracks agent execution health.
    AgentReliability { exit_code: i32, tool_failures: Vec<String> },
    VerificationFail {
        task_snippet: String,
        missing_paths: Vec<String>,
        reason: String,
    },
    EscalationTriggered {
        reason: String,
        escalation_provider: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub kind: SignalKind,
    pub severity: u8,
}

#[derive(Debug, Default)]
pub struct SignalSet {
    pub signals: Vec<Signal>,
}

impl SignalSet {
    pub fn push(&mut self, kind: SignalKind) {
        self.signals.push(Signal { kind, severity: 1 });
    }
}

/// Collect signals from a completed Goose session.
///
/// `clippy_output` is the captured stderr of `cargo clippy --message-format=short`.
/// `diff_changed_files` is output of `git diff --name-only HEAD` split into lines.
/// `goose_exit_code` is the exit code returned by `run_session_mcp`.
pub fn collect_signals(
    _repo_root: &Path,
    clippy_output: &str,
    diff_changed_files: &[String],
    goose_exit_code: i32,
) -> SignalSet {
    let mut set = SignalSet::default();

    // Lint warnings and TODO/FIXME: parse clippy short format
    for line in clippy_output.lines() {
        if line.contains("TODO") || line.contains("FIXME") {
            // Extract file from line (path before first ':')
            let file = line.split(':').next().unwrap_or("").to_string();
            set.push(SignalKind::TodoFixme {
                text: line.to_string(),
                file,
                context_radius: 3,
            });
        } else if let Some(idx) = line.find(": warning: ") {
            let detail = line[idx + ": warning: ".len()..].to_string();
            let category = detail
                .rfind('[')
                .and_then(|s| detail[s..].rfind(']').map(|e| detail[s + 1..s + e].to_string()))
                .unwrap_or_else(|| "clippy".to_string());
            set.push(SignalKind::LintWarning { category, detail });
        }
    }

    // Test ↔ diff overlap: fires when changed files contain no test files.
    if !diff_changed_files.is_empty() {
        let has_test = diff_changed_files.iter().any(|f| is_test_file(f));
        if !has_test {
            set.push(SignalKind::DiffWithoutTest);
        }
    }

    // Agent reliability: non-zero exit code.
    if goose_exit_code != 0 {
        set.push(SignalKind::AgentReliability {
            exit_code: goose_exit_code,
            tool_failures: vec![],
        });
    }

    set
}

/// Returns true if `path` is a test file.
fn is_test_file(path: &str) -> bool {
    path.contains("/tests/")
        || path.contains("_test.rs")
        || path.contains("_tests.rs")
        || path.ends_with("test.rs")
}

/// Parse total changed lines from `git diff --shortstat` output.
/// Example input: " 2 files changed, 18 insertions(+), 8 deletions(-)"
pub fn parse_diff_line_count(shortstat: &str) -> usize {
    let mut total = 0usize;
    for part in shortstat.split(',') {
        let part = part.trim();
        if part.contains("insertion") || part.contains("deletion") {
            if let Some(n) = part.split_whitespace().next() {
                total += n.parse::<usize>().unwrap_or(0);
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn empty_inputs_produce_no_signals() {
        let s = collect_signals(Path::new("/tmp"), "", &[], 0);
        assert!(s.signals.is_empty());
    }

    #[test]
    fn clippy_warning_parsed() {
        let clippy = "crates/codi-core/src/routing.rs:45:5: warning: function `is_complex` has a cognitive complexity of 25 [clippy::cognitive_complexity]";
        let s = collect_signals(Path::new("/tmp"), clippy, &[], 0);
        let warnings: Vec<_> = s.signals.iter()
            .filter(|sig| matches!(&sig.kind, SignalKind::LintWarning { .. }))
            .collect();
        assert_eq!(warnings.len(), 1);
        if let SignalKind::LintWarning { detail, .. } = &warnings[0].kind {
            assert!(detail.contains("cognitive_complexity"));
        } else {
            panic!("expected LintWarning");
        }
    }

    #[test]
    fn diff_without_test_fires_when_no_test_file_changed() {
        let changed = vec!["crates/codi-core/src/routing.rs".to_string()];
        let s = collect_signals(Path::new("/tmp"), "", &changed, 0);
        assert!(s.signals.iter().any(|sig| matches!(sig.kind, SignalKind::DiffWithoutTest)));
    }

    #[test]
    fn diff_without_test_does_not_fire_when_test_file_changed() {
        let changed = vec![
            "crates/codi-core/src/routing.rs".to_string(),
            "crates/codi-core/tests/integration_engine.rs".to_string(),
        ];
        let s = collect_signals(Path::new("/tmp"), "", &changed, 0);
        assert!(!s.signals.iter().any(|sig| matches!(sig.kind, SignalKind::DiffWithoutTest)));
    }

    #[test]
    fn nonzero_exit_code_produces_agent_reliability_signal() {
        let s = collect_signals(Path::new("/tmp"), "", &[], 1);
        assert!(s.signals.iter().any(|sig| {
            matches!(&sig.kind, SignalKind::AgentReliability { exit_code, .. } if *exit_code == 1)
        }));
    }

    #[test]
    fn parse_diff_line_count_insertions_and_deletions() {
        assert_eq!(parse_diff_line_count(" 2 files changed, 18 insertions(+), 8 deletions(-)"), 26);
    }

    #[test]
    fn parse_diff_line_count_insertions_only() {
        assert_eq!(parse_diff_line_count(" 1 file changed, 5 insertions(+)"), 5);
    }

    #[test]
    fn parse_diff_line_count_empty() {
        assert_eq!(parse_diff_line_count(""), 0);
    }

    #[test]
    fn verification_fail_signal_is_constructible() {
        let s = SignalSet {
            signals: vec![Signal {
                kind: SignalKind::VerificationFail {
                    task_snippet: "create src/foo.rs".to_string(),
                    missing_paths: vec!["src/foo.rs".to_string()],
                    reason: "missing_paths".to_string(),
                },
                severity: 1,
            }],
        };
        assert!(matches!(&s.signals[0].kind, SignalKind::VerificationFail { .. }));
    }

    #[test]
    fn escalation_triggered_signal_is_constructible() {
        let s = SignalSet {
            signals: vec![Signal {
                kind: SignalKind::EscalationTriggered {
                    reason: "retry failed".to_string(),
                    escalation_provider: "cloud(claude-sonnet-4-6)".to_string(),
                },
                severity: 1,
            }],
        };
        assert!(matches!(&s.signals[0].kind, SignalKind::EscalationTriggered { .. }));
    }
}
