//! Risk classification for self-improvement candidates.

use serde::{Deserialize, Serialize};

use crate::config::SelfImprovementConfig;
use crate::signals::{Signal, SignalKind, SignalSet};

const HIGH_RISK_KEYWORDS: &[&str] = &[
    "security", "architecture", "api-breaking", "breaking", "migration",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RiskLevel {
    Low,
    High,
    // Medium is reserved for a future version — never produced in v1.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImprovementCandidate {
    pub id: String,
    pub description: String,
    pub risk: RiskLevel,
    pub risk_reason: String,
    pub source_signals: Vec<Signal>,
    /// Comma-separated file paths relevant to this candidate.
    pub context: String,
    pub created_at: u64,
}

/// Classify signals into improvement candidates.
/// Returns an empty vec when `cfg.enabled` is false or no signals are actionable.
pub fn classify(
    signals: &SignalSet,
    cfg: &SelfImprovementConfig,
    changed_files: &[String],
) -> Vec<ImprovementCandidate> {
    if !cfg.enabled || signals.signals.is_empty() {
        return vec![];
    }

    // Separate AgentReliability signals to combine them into one candidate.
    let mut reliability_signals: Vec<Signal> = Vec::new();
    let mut other_signals: Vec<(usize, &Signal)> = Vec::new();
    let mut index = 0usize;

    for signal in &signals.signals {
        if matches!(signal.kind, SignalKind::AgentReliability { .. }) {
            reliability_signals.push(signal.clone());
        } else {
            other_signals.push((index, signal));
            index += 1;
        }
    }

    let mut candidates: Vec<ImprovementCandidate> = Vec::new();

    // Process non-reliability signals
    let context_all = changed_files.join(", ");
    for (i, signal) in &other_signals {
        if let Some(candidate) = signal_to_candidate(signal, cfg, &context_all, *i) {
            candidates.push(candidate);
        }
    }

    // Combine all AgentReliability signals into one candidate
    if !reliability_signals.is_empty() {
        let agent_index = other_signals.len();
        let candidate = reliability_candidate(reliability_signals, cfg, &context_all, agent_index);
        candidates.push(candidate);
    }

    // Re-assign IDs based on final position in Vec
    let now_micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    for (i, c) in candidates.iter_mut().enumerate() {
        c.id = format!("{:016x}{:04x}", now_micros, i & 0xffff);
    }

    candidates
}

fn signal_to_candidate(
    signal: &Signal,
    cfg: &SelfImprovementConfig,
    context: &str,
    index: usize,
) -> Option<ImprovementCandidate> {
    let id = generate_id(index);
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    match &signal.kind {
        SignalKind::LintWarning { detail, .. } => {
            let description = format!("Fix lint warning: {detail}");
            let (risk, risk_reason) = classify_risk(&description, context, cfg);
            Some(ImprovementCandidate {
                id,
                description,
                risk,
                risk_reason,
                source_signals: vec![signal.clone()],
                context: context.to_string(),
                created_at,
            })
        }

        SignalKind::TestFailure { test_name, module } => {
            let description = format!("Fix failing test: {test_name} in {module}");
            let (risk, risk_reason) = classify_risk(&description, module, cfg);
            Some(ImprovementCandidate {
                id,
                description,
                risk,
                risk_reason,
                source_signals: vec![signal.clone()],
                context: module.clone(),
                created_at,
            })
        }

        SignalKind::DiffWithoutTest => {
            let description =
                "Add tests for changed files without test coverage".to_string();
            Some(ImprovementCandidate {
                id,
                description,
                risk: RiskLevel::Low,
                risk_reason: "no test file in diff".to_string(),
                source_signals: vec![signal.clone()],
                context: context.to_string(),
                created_at,
            })
        }

        SignalKind::TodoFixme { text, file, .. } => {
            let description = format!("Address TODO/FIXME: {text} in {file}");
            let (risk, risk_reason) = classify_risk(&description, file, cfg);
            Some(ImprovementCandidate {
                id,
                description,
                risk,
                risk_reason,
                source_signals: vec![signal.clone()],
                context: file.clone(),
                created_at,
            })
        }

        // VerificationFail: not yet turned into improvement candidate
        SignalKind::VerificationFail { .. } => None,

        // EscalationTriggered: not yet turned into improvement candidate
        SignalKind::EscalationTriggered { .. } => None,

        // AgentReliability handled separately
        SignalKind::AgentReliability { .. } => None,
    }
}

fn reliability_candidate(
    signals: Vec<Signal>,
    cfg: &SelfImprovementConfig,
    context: &str,
    index: usize,
) -> ImprovementCandidate {
    let id = generate_id(index);
    let created_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Collect all tool_failures across all AgentReliability signals
    let mut all_tool_failures: Vec<String> = Vec::new();
    let mut any_nonzero_exit = false;
    for sig in &signals {
        if let SignalKind::AgentReliability {
            exit_code,
            tool_failures,
        } = &sig.kind
        {
            if *exit_code != 0 {
                any_nonzero_exit = true;
            }
            all_tool_failures.extend(tool_failures.clone());
        }
    }

    let description = "Improve agent reliability".to_string();

    // High risk if tool_failures non-empty
    let (risk, risk_reason) = if !all_tool_failures.is_empty() {
        (
            RiskLevel::High,
            format!(
                "agent reliability signal: tool_failures=[{}]",
                all_tool_failures.join(", ")
            ),
        )
    } else if any_nonzero_exit {
        // Also check blocklist and keywords per standard rules
        let (r, rr) = classify_risk(&description, context, cfg);
        // Non-zero exit + not otherwise high: still use standard classify
        (r, format!("agent reliability signal: non-zero exit; {rr}"))
    } else {
        classify_risk(&description, context, cfg)
    };

    ImprovementCandidate {
        id,
        description,
        risk,
        risk_reason,
        source_signals: signals,
        context: context.to_string(),
        created_at,
    }
}

/// Determine risk level and reason for a candidate based on description, context, and config.
fn classify_risk(
    description: &str,
    context: &str,
    cfg: &SelfImprovementConfig,
) -> (RiskLevel, String) {
    let lower = description.to_lowercase();

    // Rule 3: description contains high-risk keyword
    for kw in HIGH_RISK_KEYWORDS {
        if lower.contains(kw) {
            return (
                RiskLevel::High,
                format!("high-risk keyword '{kw}' in description"),
            );
        }
    }

    // Rule 2: blocklist file appears in context
    for blocked in &cfg.blocklist {
        if context.contains(blocked.as_str()) {
            return (
                RiskLevel::High,
                format!("context contains blocklist file '{blocked}'"),
            );
        }
    }

    (RiskLevel::Low, "lint-only change in non-blocklist file".to_string())
}

fn generate_id(index: usize) -> String {
    let micros = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_micros() as u64;
    format!("{:016x}{:04x}", micros, index & 0xffff)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::SelfImprovementConfig;
    use crate::signals::{Signal, SignalKind, SignalSet};

    fn default_cfg() -> SelfImprovementConfig {
        SelfImprovementConfig::default()
    }

    fn set_of(kinds: Vec<SignalKind>) -> SignalSet {
        SignalSet {
            signals: kinds
                .into_iter()
                .map(|kind| Signal { kind, severity: 1 })
                .collect(),
        }
    }

    #[test]
    fn empty_signals_produce_no_candidates() {
        let c = classify(&SignalSet::default(), &default_cfg(), &[]);
        assert!(c.is_empty());
    }

    #[test]
    fn lint_warning_in_non_blocklist_file_is_low_risk() {
        let set = set_of(vec![SignalKind::LintWarning {
            category: "clippy::unused_variable".to_string(),
            detail: "unused variable `x` [clippy::unused_variable]".to_string(),
        }]);
        let candidates = classify(
            &set,
            &default_cfg(),
            &["crates/codi-core/src/review.rs".to_string()],
        );
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].risk, RiskLevel::Low));
    }

    #[test]
    fn blocklist_file_promotes_to_high_risk() {
        let set = set_of(vec![SignalKind::LintWarning {
            category: "clippy".to_string(),
            detail: "complex function [clippy::cognitive_complexity]".to_string(),
        }]);
        let candidates = classify(
            &set,
            &default_cfg(),
            &["crates/codi-core/src/routing.rs".to_string()],
        );
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].risk, RiskLevel::High));
        assert!(candidates[0].risk_reason.contains("blocklist"));
    }

    #[test]
    fn diff_without_test_is_low_risk() {
        let set = set_of(vec![SignalKind::DiffWithoutTest]);
        let candidates = classify(
            &set,
            &default_cfg(),
            &["crates/codi-core/src/review.rs".to_string()],
        );
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].risk, RiskLevel::Low));
        assert!(candidates[0].risk_reason.contains("no test"));
    }

    #[test]
    fn high_risk_keyword_in_lint_detail_promotes_to_high() {
        let set = set_of(vec![SignalKind::LintWarning {
            category: "clippy".to_string(),
            detail: "security: avoid unsafe block".to_string(),
        }]);
        let candidates = classify(&set, &default_cfg(), &[]);
        assert!(matches!(candidates[0].risk, RiskLevel::High));
        assert!(candidates[0].risk_reason.contains("keyword"));
    }

    #[test]
    fn agent_reliability_failure_is_high_risk() {
        let set = set_of(vec![SignalKind::AgentReliability {
            exit_code: 1,
            tool_failures: vec!["write_file".to_string()],
        }]);
        let candidates = classify(&set, &default_cfg(), &[]);
        assert!(!candidates.is_empty());
        assert!(matches!(candidates[0].risk, RiskLevel::High));
    }

    #[test]
    fn candidate_ids_are_unique() {
        let set = set_of(vec![
            SignalKind::LintWarning {
                category: "a".to_string(),
                detail: "warn1".to_string(),
            },
            SignalKind::LintWarning {
                category: "b".to_string(),
                detail: "warn2".to_string(),
            },
        ]);
        let candidates = classify(&set, &default_cfg(), &[]);
        let ids: std::collections::HashSet<_> = candidates.iter().map(|c| &c.id).collect();
        assert_eq!(ids.len(), candidates.len());
    }

    #[test]
    fn disabled_config_produces_no_candidates() {
        let mut cfg = default_cfg();
        cfg.enabled = false;
        let set = set_of(vec![SignalKind::DiffWithoutTest]);
        let candidates = classify(&set, &cfg, &[]);
        assert!(candidates.is_empty());
    }

    #[test]
    fn multiple_agent_reliability_signals_produce_one_candidate() {
        let set = set_of(vec![
            SignalKind::AgentReliability {
                exit_code: 1,
                tool_failures: vec!["read_file".to_string()],
            },
            SignalKind::AgentReliability {
                exit_code: 2,
                tool_failures: vec!["write_file".to_string()],
            },
        ]);
        let candidates = classify(&set, &default_cfg(), &[]);
        assert_eq!(candidates.len(), 1);
        assert!(matches!(candidates[0].risk, RiskLevel::High));
        assert_eq!(candidates[0].source_signals.len(), 2);
    }

    #[test]
    fn test_failure_produces_candidate_with_correct_description() {
        let set = set_of(vec![SignalKind::TestFailure {
            test_name: "my_test".to_string(),
            module: "crate::foo".to_string(),
        }]);
        let candidates = classify(&set, &default_cfg(), &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].description,
            "Fix failing test: my_test in crate::foo"
        );
    }

    #[test]
    fn todo_fixme_produces_candidate_with_correct_description() {
        let set = set_of(vec![SignalKind::TodoFixme {
            text: "clean this up".to_string(),
            file: "src/foo.rs".to_string(),
            context_radius: 0,
        }]);
        let candidates = classify(&set, &default_cfg(), &[]);
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0].description,
            "Address TODO/FIXME: clean this up in src/foo.rs"
        );
    }
}
