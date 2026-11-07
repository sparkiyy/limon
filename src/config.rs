use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Default)]
pub struct LimonConfig {
    pub output: Option<String>,
    #[serde(default)]
    pub aliases: HashMap<String, String>,
}

impl LimonConfig {
    pub fn load_from_file(path: &Path) -> Result<Self, String> {
        let content = fs::read_to_string(path)
            .map_err(|e| format!("Could not read '{}': {}", path.display(), e))?;
        toml::from_str(&content)
            .map_err(|e| format!("Error parsing TOML in '{}': {}", path.display(), e))
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
