use serde::Deserialize;
use std::collections::{HashMap, HashSet};
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

#[derive(Debug, Clone, Deserialize, Default)]
struct LuaurcFile {
    #[serde(default)]
    aliases: HashMap<String, String>,
}

/// Strips `//` and `/* ... */` comments from a JSON-like string
pub fn strip_json_comments(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escape = false;

    while let Some(c) = chars.next() {
        if in_string {
            result.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
            result.push(c);
        } else if c == '/' && chars.peek() == Some(&'/') {
            chars.next(); // Consume second '/'
            for sc in chars.by_ref() {
                if sc == '\n' {
                    result.push('\n');
                    break;
                }
            }
        } else if c == '/' && chars.peek() == Some(&'*') {
            chars.next(); // Consume '*'
            while let Some(mc) = chars.next() {
                if mc == '*' && chars.peek() == Some(&'/') {
                    chars.next(); // Consume '/'
                    break;
                }
            }
        } else {
            result.push(c);
        }
    }

    result
}

/// Resolves the valid `.luaurc` aliases for a given file by looking upwards in the directory tree.
/// Returns `None` if no `.luaurc` file exists for this file in its directory or ancestor directories up to `root_dir`.
/// In Roblox environment, `@game` is default and filtered out.
pub fn resolve_luaurc_aliases(
    file_path: &Path,
    root_dir: &Path,
    cache: &mut HashMap<PathBuf, Option<HashSet<String>>>,
) -> Option<HashSet<String>> {
    let parent_dir = file_path.parent().unwrap_or(root_dir);

    if let Some(cached) = cache.get(parent_dir) {
        return cached.clone();
    }

    let canonical_root = root_dir.canonicalize().unwrap_or_else(|_| root_dir.to_path_buf());
    let mut current_dir = parent_dir.canonicalize().unwrap_or_else(|_| parent_dir.to_path_buf());

    // Collect all .luaurc files from current_dir upwards to canonical_root
    let mut luaurc_paths = Vec::new();

    loop {
        let candidate = current_dir.join(".luaurc");
        if candidate.is_file() {
            luaurc_paths.push(candidate);
        }

        if current_dir == canonical_root {
            break;
        }

        match current_dir.parent() {
            Some(parent) => current_dir = parent.to_path_buf(),
            None => break,
        }
    }

    if luaurc_paths.is_empty() {
        cache.insert(parent_dir.to_path_buf(), None);
        return None;
    }

    // Merge from root down to child (reverse order of collected)
    luaurc_paths.reverse();
    let mut merged_aliases = HashMap::new();

    for luaurc_path in luaurc_paths {
        if let Ok(raw_content) = fs::read_to_string(&luaurc_path) {
            let cleaned = strip_json_comments(&raw_content);
            if let Ok(parsed) = serde_json::from_str::<LuaurcFile>(&cleaned) {
                for (alias, path) in parsed.aliases {
                    merged_aliases.insert(alias, path);
                }
            }
        }
    }

    // Roblox `@game` is default and should be ignored
    merged_aliases.remove("game");

    let aliases_set: HashSet<String> = merged_aliases.into_keys().collect();
    let result = Some(aliases_set);
    cache.insert(parent_dir.to_path_buf(), result.clone());
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_comments() {
        let input = r#"
        {
            // This is a line comment
            "aliases": {
                /* block comment */
                "pkg": "src/shared/Packages",
                "url": "http://example.com" // url with slashes
            }
        }
        "#;
        let stripped = strip_json_comments(input);
        let parsed: LuaurcFile = serde_json::from_str(&stripped).unwrap();
        assert_eq!(parsed.aliases.get("pkg").unwrap(), "src/shared/Packages");
        assert_eq!(parsed.aliases.get("url").unwrap(), "http://example.com");
    }
}
