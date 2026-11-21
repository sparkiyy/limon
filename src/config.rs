use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

fn default_true() -> bool {
    true
}

/// Supported value types for environment variable casting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EnvValueType {
    Auto,
    String,
    #[serde(alias = "boolean")]
    Bool,
    Number,
}

/// Configuration for `[env]` static environment variable injection and dead-code pruning.
#[derive(Debug, Clone, Deserialize)]
pub struct EnvConfig {
    /// Enable/disable env injection (defaults to `true` whenever `[env]` is present).
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Global automatic casting of env strings (`"true"`/`"false"` -> bool, `"nil"` -> nil,
    /// strict JSON-style finite numbers -> number, else string). Defaults to `true`.
    #[serde(default = "default_true")]
    pub cast: bool,

    /// Statically analyze expressions containing injected env vars and prune dead branches.
    /// Defaults to `true`.
    #[serde(default = "default_true", alias = "dead_code", alias = "prune_branches")]
    pub dead_code_elimination: bool,

    /// When removing dead code, preserve original line numbers with blank lines.
    /// Defaults to `true`.
    #[serde(default = "default_true")]
    pub preserve_lines: bool,

    /// Optional per-variable type overrides: `"string"`, `"bool"`/`"boolean"`, `"number"`, `"auto"`.
    #[serde(default)]
    pub types: HashMap<String, EnvValueType>,

    /// Optional fallback values used when an env var is unset in the OS environment.
    #[serde(default)]
    pub defaults: HashMap<String, toml::Value>,

    /// Required environment variables that must be present in the OS environment.
    #[serde(default)]
    pub required: Vec<String>,
}

impl Default for EnvConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cast: true,
            dead_code_elimination: true,
            preserve_lines: true,
            types: HashMap::new(),
            defaults: HashMap::new(),
            required: Vec::new(),
        }
    }
}

impl EnvConfig {
    /// Validates the `[env]` configuration fields (default value types).
    pub fn validate(&self) -> Result<(), String> {
        for (key, val) in &self.defaults {
            match val {
                toml::Value::Boolean(_) | toml::Value::Integer(_) | toml::Value::String(_) => {}
                toml::Value::Float(f) if f.is_finite() => {}
                toml::Value::Float(_) => {
                    return Err(format!(
                        "Invalid [env.defaults] value for '{}': non-finite float is not supported.",
                        key
                    ));
                }
                other => {
                    return Err(format!(
                        "Invalid [env.defaults] value for '{}': expected string, boolean, or number, got {}.",
                        key,
                        other.type_str()
                    ));
                }
            }
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LimonConfig {
    pub output: Option<String>,
    #[serde(default)]
    pub clean: bool,
    #[serde(default)]
    pub strict: bool,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
    pub env: Option<EnvConfig>,
}

impl LimonConfig {
    /// Returns the active `[env]` config if present and enabled.
    pub fn active_env(&self) -> Option<&EnvConfig> {
        self.env.as_ref().filter(|e| e.enabled)
    }

    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Could not read '{}': {}", path.display(), e))?;
        let config: Self = toml::from_str(&content)
            .map_err(|e| format!("Error parsing TOML in '{}': {}", path.display(), e))?;
        if let Some(ref env) = config.env {
            env.validate()?;
        }
        Ok(config)
    }

    pub fn find_and_load(
        root_dir: &Path,
        explicit_config: Option<&Path>,
    ) -> Result<(Self, Option<PathBuf>), String> {
        if let Some(explicit) = explicit_config {
            let config = Self::load_from_file(explicit)?;
            return Ok((config, Some(explicit.to_path_buf())));
        }

        let candidates = [
            root_dir.join("limon.toml"),
            PathBuf::from("limon.toml"),
        ];

        for candidate in candidates {
            if candidate.is_file() {
                let config = Self::load_from_file(&candidate)?;
                return Ok((config, Some(candidate)));
            }
        }

        Ok((Self::default(), None))
    }
}
