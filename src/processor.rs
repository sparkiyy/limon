use crate::config::LimonConfig;
use crate::transform::transform_luau;
use std::fs;
use std::path::Path;

#[derive(Debug, Default, Clone)]
pub struct ProcessStats {
    pub files_scanned: usize,
    pub files_processed: usize,
    pub files_modified: usize,
}

pub fn is_luau_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        ext.eq_ignore_ascii_case("luau") || ext.eq_ignore_ascii_case("lua")
    } else {
        false
    }
}

pub fn should_ignore(path: &Path, root_dir: &Path, output_dir: Option<&Path>) -> bool {
    let file_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(name) => name,
        None => return false,
    };

    // Ignore VCS / common build directories
    if file_name == ".git" || file_name == "node_modules" || file_name == "target" {
        return true;
    }

    // Ignore output directory if inside root
    if let Some(out) = output_dir {
        let abs_path = path.canonicalize().unwrap_or_else(|_| {
            if path.is_absolute() {
                path.to_path_buf()
            } else {
                root_dir.join(path)
            }
        });
        let abs_out = out.canonicalize().unwrap_or_else(|_| {
            if out.is_absolute() {
                out.to_path_buf()
            } else {
                root_dir.join(out)
            }
        });
        if abs_path == abs_out || abs_path.starts_with(&abs_out) {
            return true;
        }
    }

    false
}

pub fn process_single_file(
    file_path: &Path,
    root_dir: &Path,
    output_dir: Option<&Path>,
    config: &LimonConfig,
    stats: &mut ProcessStats,
) -> Result<bool, String> {
    stats.files_scanned += 1;

    let rel_path = file_path
        .strip_prefix(root_dir)
        .unwrap_or(file_path);

    let target_path = output_dir.map(|out| out.join(rel_path));

    // Non-Luau file handling
    if !is_luau_file(file_path) {
        if let Some(ref dest) = target_path {
            if let Some(parent) = dest.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Could not create directory '{}': {}", parent.display(), e))?;
            }
            fs::copy(file_path, dest)
                .map_err(|e| format!("Could not copy '{}' to '{}': {}", file_path.display(), dest.display(), e))?;
        }
        return Ok(false);
    }

    stats.files_processed += 1;

    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Could not read '{}': {}", file_path.display(), e))?;

    let (transformed, modified) = match transform_luau(&content, &config.aliases) {
        Ok(res) => res,
        Err(err) => {
            let err_msg = err.iter().map(|e| e.to_string()).collect::<Vec<_>>().join(", ");
            eprintln!(
                "[limon] Warning: Failed to parse '{}': {}. Keeping original content.",
                file_path.display(),
                err_msg
            );
            (content.clone(), false)
        }
    };

    if modified {
        stats.files_modified += 1;
    }

    if let Some(ref dest) = target_path {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create directory '{}': {}", parent.display(), e))?;
        }
        let to_write = if modified { &transformed } else { &content };
        fs::write(dest, to_write)
            .map_err(|e| format!("Could not write to '{}': {}", dest.display(), e))?;
    } else if modified {
        // In-place modification
        fs::write(file_path, &transformed)
            .map_err(|e| format!("Could not write to '{}': {}", file_path.display(), e))?;
    }

    Ok(modified)
}

pub fn process_project(
    root_dir: &Path,
    output_dir: Option<&Path>,
    config: &LimonConfig,
) -> Result<ProcessStats, String> {
    let mut stats = ProcessStats::default();

    if !root_dir.exists() {
        return Err(format!("Directory '{}' does not exist.", root_dir.display()));
    }

    if root_dir.is_file() {
        process_single_file(
            root_dir,
            root_dir.parent().unwrap_or(root_dir),
            output_dir,
            config,
            &mut stats,
        )?;
        return Ok(stats);
    }

    // Traverse directory recursively
    let mut dirs_to_visit = vec![root_dir.to_path_buf()];

    while let Some(current_dir) = dirs_to_visit.pop() {
        let entries = fs::read_dir(&current_dir)
            .map_err(|e| format!("Could not read directory '{}': {}", current_dir.display(), e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Error reading directory entry: {}", e))?;
            let path = entry.path();

            if should_ignore(&path, root_dir, output_dir) {
                continue;
            }

            if path.is_dir() {
                dirs_to_visit.push(path);
            } else if path.is_file() {
                process_single_file(
                    &path,
                    root_dir,
                    output_dir,
                    config,
                    &mut stats,
                )?;
            }
        }
    }

    Ok(stats)
}
