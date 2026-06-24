//! Routing policy: picks local vs cloud model per task.
//!
//! The heuristic classifier runs only when `mode = "hybrid"`. It looks for
//! signals that a task requires deeper reasoning (length, multi-file scope,
//! "architecture"-level keywords) and escalates to the cloud model if a
//! cloud model is configured. Everything else stays local.

use crate::config::{Config, CloudModel, LocalModel, RoutingMode};

/// Which provider/model should handle this task.
#[derive(Debug, Clone, PartialEq)]
pub enum Provider {
    Local(LocalModel),
    Cloud(CloudModel),
}

/// Keywords that suggest a task is architecturally complex.
const COMPLEX_KEYWORDS: &[&str] = &[
    "architect",
    "refactor",
    "redesign",
    "migration",
    "across module",
    "across file",
    "codebase",
    "entire",
    "all files",
    "large",
    "security",
    "performance audit",
];

/// Pick the provider for `task`. Returns `Provider::Local` when cloud is not
/// configured regardless of mode. Escalation only happens in `Hybrid` mode when
/// the heuristic classifier flags the task as complex AND a cloud model exists.
pub fn pick_provider(cfg: &Config, task: &str) -> Provider {
    match cfg.routing.mode {
        RoutingMode::LocalOnly => Provider::Local(cfg.model.local.clone()),
        RoutingMode::CloudPreferred => match &cfg.model.cloud {
            Some(cloud) => Provider::Cloud(cloud.clone()),
            None => Provider::Local(cfg.model.local.clone()),
        },
        RoutingMode::Hybrid => {
            if let Some(cloud) = &cfg.model.cloud {
                if is_complex(task) {
                    return Provider::Cloud(cloud.clone());
                }
            }
            Provider::Local(cfg.model.local.clone())
        }
    }
}

/// Cheap heuristic: a task is "complex" if it is long or mentions one of the
/// complexity keywords (case-insensitive). No ML required.
fn is_complex(task: &str) -> bool {
    let lower = task.to_lowercase();
    if task.len() > 600 {
        return true;
    }
    COMPLEX_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{CloudModel, Config, RoutingConfig, RoutingMode};

    fn cfg_with_cloud(mode: RoutingMode) -> Config {
        let mut c = Config::default();
        c.routing = RoutingConfig { mode };
        c.model.cloud = Some(CloudModel {
            provider: "anthropic".to_string(),
            model: "claude-test".to_string(),
            base_url: None,
            api_key_env: "ANTHROPIC_API_KEY".to_string(),
        });
        c
    }

    #[test]
    fn local_only_always_local() {
        let cfg = cfg_with_cloud(RoutingMode::LocalOnly);
        let p = pick_provider(&cfg, "refactor the entire architecture");
        assert!(matches!(p, Provider::Local(_)));
    }

    #[test]
    fn cloud_preferred_picks_cloud() {
        let cfg = cfg_with_cloud(RoutingMode::CloudPreferred);
        let p = pick_provider(&cfg, "add a hello() function");
        assert!(matches!(p, Provider::Cloud(_)));
    }

    #[test]
    fn hybrid_simple_task_stays_local() {
        let cfg = cfg_with_cloud(RoutingMode::Hybrid);
        let p = pick_provider(&cfg, "add a println to main.rs");
        assert!(matches!(p, Provider::Local(_)));
    }

    #[test]
    fn hybrid_complex_task_escalates() {
        let cfg = cfg_with_cloud(RoutingMode::Hybrid);
        let p = pick_provider(&cfg, "refactor auth module across all files");
        assert!(matches!(p, Provider::Cloud(_)));
    }

    #[test]
    fn hybrid_without_cloud_stays_local() {
        let cfg = Config::default(); // no cloud
        let p = pick_provider(&cfg, "refactor the entire codebase architecture");
        assert!(matches!(p, Provider::Local(_)));
    }
}
