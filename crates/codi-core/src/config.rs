//! Configuration schema and loading for codi.
//!
//! Config is layered: a user-level file (`~/.config/codi/config.toml`) provides
//! defaults, and a repo-level `./codi.toml` overrides it. Both are optional —
//! [`Config::default`] yields a working local-first configuration that targets a
//! local Ollama-style endpoint.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Top-level codi configuration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub model: ModelConfig,
    pub routing: RoutingConfig,
    pub commands: Commands,
    pub rag: RagConfig,
    pub safety: SafetyConfig,
    /// Path to the `goose` binary. If unset, codi looks it up on `PATH`.
    pub goose_bin: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            model: ModelConfig::default(),
            routing: RoutingConfig::default(),
            commands: Commands::default(),
            rag: RagConfig::default(),
            safety: SafetyConfig::default(),
            goose_bin: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct ModelConfig {
    /// The default brain: a local, OpenAI-compatible endpoint.
    pub local: LocalModel,
    /// Optional cloud model used only when routing escalates.
    pub cloud: Option<CloudModel>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        ModelConfig {
            local: LocalModel::default(),
            cloud: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct LocalModel {
    pub base_url: String,
    pub model: String,
    /// Usually empty/unused for Ollama; kept for OpenAI-compatible servers.
    pub api_key: String,
}

impl Default for LocalModel {
    fn default() -> Self {
        LocalModel {
            base_url: "http://localhost:11434/v1".to_string(),
            model: "qwen2.5-coder:7b".to_string(),
            api_key: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CloudModel {
    /// Logical provider name (e.g. "anthropic", "openai", "deepseek").
    pub provider: String,
    pub model: String,
    /// Optional explicit base URL (for OpenAI-compatible gateways).
    #[serde(default)]
    pub base_url: Option<String>,
    /// Environment variable that holds the API key.
    pub api_key_env: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RoutingMode {
    /// Always use the local model.
    LocalOnly,
    /// Use local by default, escalate hard tasks to cloud when configured.
    Hybrid,
    /// Prefer cloud when configured, fall back to local.
    CloudPreferred,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RoutingConfig {
    pub mode: RoutingMode,
}

impl Default for RoutingConfig {
    fn default() -> Self {
        RoutingConfig {
            mode: RoutingMode::LocalOnly,
        }
    }
}

/// Project commands codi can run. All optional; empty string means "unset".
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Commands {
    pub test: Option<String>,
    pub lint: Option<String>,
    pub build: Option<String>,
    pub format: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct RagConfig {
    /// Glob patterns (relative to repo root) to include.
    pub include: Vec<String>,
    /// Glob patterns to exclude (in addition to .gitignore).
    pub exclude: Vec<String>,
    /// File extensions to index (without the dot).
    pub extensions: Vec<String>,
    /// Whether to compute embeddings for hybrid retrieval.
    pub embeddings: bool,
    /// Embedding model name (used against the local `/embeddings` endpoint).
    pub embed_model: String,
    /// Where the SQLite index lives, relative to repo root.
    pub db_path: String,
    /// Maximum characters per indexed chunk.
    pub max_chunk_chars: usize,
}

impl Default for RagConfig {
    fn default() -> Self {
        RagConfig {
            include: vec![
                "src/**".to_string(),
                "docs/**".to_string(),
                "README*".to_string(),
                "**/*.md".to_string(),
            ],
            exclude: vec![
                "target/**".to_string(),
                "node_modules/**".to_string(),
                ".git/**".to_string(),
                ".codi/**".to_string(),
            ],
            extensions: vec![
                "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "rb", "c", "h",
                "cpp", "hpp", "md", "toml", "yaml", "yml", "json",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            embeddings: false,
            embed_model: "nomic-embed-text".to_string(),
            db_path: ".codi/index.sqlite".to_string(),
            max_chunk_chars: 1200,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct SafetyConfig {
    /// Ask before running shell commands.
    pub confirm_commands: bool,
    /// Ask before writing files.
    pub confirm_writes: bool,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        SafetyConfig {
            confirm_commands: true,
            confirm_writes: true,
        }
    }
}

impl Config {
    /// Parse a config from a TOML string.
    pub fn from_toml(s: &str) -> Result<Config> {
        toml::from_str(s).context("failed to parse codi config TOML")
    }

    /// Serialize this config to a TOML string.
    pub fn to_toml(&self) -> Result<String> {
        toml::to_string_pretty(self).context("failed to serialize config")
    }

    /// Load configuration by layering the user-level file under the repo-level
    /// file. Missing files are treated as empty. `repo_root` is the directory
    /// that may contain `codi.toml`.
    pub fn load(repo_root: &Path) -> Result<Config> {
        let user = user_config_path();
        Self::load_from(repo_root, user.as_deref())
    }

    /// Like [`Config::load`] but with an explicit user-config path (testable).
    pub fn load_from(repo_root: &Path, user_path: Option<&Path>) -> Result<Config> {
        // Start from defaults, overlay user file, then repo file.
        let mut value = toml::Value::try_from(Config::default())
            .context("failed to convert default config to TOML value")?;

        if let Some(up) = user_path {
            if up.exists() {
                let text = std::fs::read_to_string(up)
                    .with_context(|| format!("reading user config {}", up.display()))?;
                let overlay: toml::Value = toml::from_str(&text)
                    .with_context(|| format!("parsing user config {}", up.display()))?;
                merge(&mut value, overlay);
            }
        }

        let repo_file = repo_root.join("codi.toml");
        if repo_file.exists() {
            let text = std::fs::read_to_string(&repo_file)
                .with_context(|| format!("reading {}", repo_file.display()))?;
            let overlay: toml::Value = toml::from_str(&text)
                .with_context(|| format!("parsing {}", repo_file.display()))?;
            merge(&mut value, overlay);
        }

        value
            .try_into()
            .context("merged config did not match the codi schema")
    }
}

/// The default user-level config path, if a home/config dir is resolvable.
pub fn user_config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("codi").join("config.toml"))
}

/// Deep-merge `overlay` into `base`. Tables are merged key-by-key; any other
/// value type in `overlay` replaces the corresponding value in `base`.
fn merge(base: &mut toml::Value, overlay: toml::Value) {
    match (base, overlay) {
        (toml::Value::Table(base_t), toml::Value::Table(over_t)) => {
            for (k, v) in over_t {
                match base_t.get_mut(&k) {
                    Some(existing) => merge(existing, v),
                    None => {
                        base_t.insert(k, v);
                    }
                }
            }
        }
        (base_slot, overlay_val) => {
            *base_slot = overlay_val;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_local_first() {
        let c = Config::default();
        assert_eq!(c.routing.mode, RoutingMode::LocalOnly);
        assert_eq!(c.model.local.base_url, "http://localhost:11434/v1");
        assert!(c.model.cloud.is_none());
        assert!(c.safety.confirm_writes);
    }

    #[test]
    fn roundtrips_through_toml() {
        let c = Config::default();
        let s = c.to_toml().unwrap();
        let back = Config::from_toml(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn repo_overrides_user_and_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("user.toml");
        std::fs::write(
            &user,
            r#"
[routing]
mode = "hybrid"

[model.local]
model = "user-model"
"#,
        )
        .unwrap();

        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("codi.toml"),
            r#"
[model.local]
model = "repo-model"
base_url = "http://localhost:9999/v1"
"#,
        )
        .unwrap();

        let cfg = Config::load_from(&repo, Some(&user)).unwrap();
        // repo overrides user for model
        assert_eq!(cfg.model.local.model, "repo-model");
        assert_eq!(cfg.model.local.base_url, "http://localhost:9999/v1");
        // user value survives where repo is silent
        assert_eq!(cfg.routing.mode, RoutingMode::Hybrid);
        // default survives where both are silent
        assert!(cfg.safety.confirm_commands);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = Config::from_toml("nonsense_key = 1").unwrap_err();
        assert!(err.to_string().contains("parse"));
    }
}
