use crate::config::LimonConfig;
use crate::env_transform::ResolvedEnv;
use crate::transform::{transform_luau_with_config, TransformError};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, Default, Clone)]
pub struct ProcessStats {
    pub files_scanned: usize,
    pub files_processed: usize,
    pub files_modified: usize,
    pub files_removed: usize,
}

impl ProcessStats {
    /// Merges counts from another `ProcessStats` instance into `self`.
    pub fn merge(&mut self, other: &ProcessStats) {
        self.files_scanned += other.files_scanned;
        self.files_processed += other.files_processed;
        self.files_modified += other.files_modified;
        self.files_removed += other.files_removed;
    }
}

pub fn is_luau_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|s| s.to_str()) {
        ext.eq_ignore_ascii_case("luau") || ext.eq_ignore_ascii_case("lua")
    } else {
        false
    }
}

pub fn get_relative_path(file_path: &Path, root_dir: &Path) -> PathBuf {
    if let Ok(rel) = file_path.strip_prefix(root_dir) {
        return rel.to_path_buf();
    }
    let canon_file = file_path.canonicalize().unwrap_or_else(|_| file_path.to_path_buf());
    let canon_root = root_dir.canonicalize().unwrap_or_else(|_| root_dir.to_path_buf());
    if let Ok(rel) = canon_file.strip_prefix(&canon_root) {
        return rel.to_path_buf();
    }
    // Fallback: extract filename
    file_path
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| file_path.to_path_buf())
}

fn is_ignored_dir_name(name: &str) -> bool {
    matches!(
        name,
        ".git" | "node_modules" | "target" | ".vscode" | ".idea"
    )
}

pub fn should_ignore(path: &Path, root_dir: &Path, output_dir: Option<&Path>) -> bool {
    let file_name = match path.file_name().and_then(|s| s.to_str()) {
        Some(name) => name,
        None => return false,
    };

    // Ignore VCS / build directories / editor caches
    if is_ignored_dir_name(file_name) {
        return true;
    }

    // Ignore output directory if inside root
    if let Some(out) = output_dir {
        if path == out || path.starts_with(out) {
            return true;
        }

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
    resolved_env: Option<&ResolvedEnv<'_>>,
    stats: &mut ProcessStats,
) -> Result<bool, String> {
    stats.files_scanned += 1;

    let rel_path = get_relative_path(file_path, root_dir);
    let target_path = output_dir.map(|out| out.join(&rel_path));

    // Handle non-Luau files (only copy if output_dir is specified and contents differ)
    if !is_luau_file(file_path) {
        if let Some(ref dest) = target_path
            && dest != file_path
        {
            let need_copy = if dest.exists() {
                match (fs::metadata(file_path), fs::metadata(dest)) {
                    (Ok(src_m), Ok(dst_m)) => src_m.len() != dst_m.len(),
                    _ => true,
                }
            } else {
                true
            };

            if need_copy {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!("Could not create directory '{}': {}", parent.display(), e)
                    })?;
                }
                fs::copy(file_path, dest).map_err(|e| {
                    format!(
                        "Could not copy '{}' to '{}': {}",
                        file_path.display(),
                        dest.display(),
                        e
                    )
                })?;
            }
        }
        return Ok(false);
    }

    stats.files_processed += 1;

    let content = fs::read_to_string(file_path)
        .map_err(|e| format!("Could not read '{}': {}", file_path.display(), e))?;

    let (transformed, modified) = match transform_luau_with_config(&content, config, resolved_env) {
        Ok(res) => res,
        Err(TransformError::Parse(err)) => {
            let err_msg = err
                .iter()
                .map(|e| e.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "[limon] Warning: Failed to parse '{}': {}. Keeping original content.",
                file_path.display(),
                err_msg
            );
            (content.clone(), false)
        }
        Err(TransformError::UnresolvedAliases(unresolved)) => {
            let formatted = unresolved
                .iter()
                .map(|(alias, line)| format!("@{} (line {})", alias, line))
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "Unresolved require alias(es) in '{}': {}",
                file_path.display(),
                formatted
            ));
        }
    };

    if let Some(ref dest) = target_path {
        // Output directory mode
        if dest != file_path {
            let to_write = if modified { &transformed } else { &content };
            let need_write = if dest.exists() {
                match fs::read(dest) {
                    Ok(existing) => existing != to_write.as_bytes(),
                    Err(_) => true,
                }
            } else {
                true
            };

            if need_write {
                if let Some(parent) = dest.parent() {
                    fs::create_dir_all(parent).map_err(|e| {
                        format!("Could not create directory '{}': {}", parent.display(), e)
                    })?;
                }
                fs::write(dest, to_write)
                    .map_err(|e| format!("Could not write to '{}': {}", dest.display(), e))?;
            }
        }
    } else if modified {
        // In-place modification: ONLY write if file content actually changed
        let need_write = match fs::read(file_path) {
            Ok(existing) => existing != transformed.as_bytes(),
            Err(_) => true,
        };

        if need_write {
            fs::write(file_path, &transformed)
                .map_err(|e| format!("Could not write to '{}': {}", file_path.display(), e))?;
        }
    }

    if modified {
        stats.files_modified += 1;
    }

    Ok(modified)
}

fn validate_clean_target(root_dir: &Path, output_dir: &Path) -> Result<PathBuf, String> {
    if !output_dir.is_dir() {
        return Err(format!(
            "Output path '{}' exists and is not a directory.",
            output_dir.display()
        ));
    }

    let canon_root = root_dir
        .canonicalize()
        .map_err(|e| format!("Could not canonicalize '{}': {}", root_dir.display(), e))?;
    let canon_out = output_dir
        .canonicalize()
        .map_err(|e| format!("Could not canonicalize '{}': {}", output_dir.display(), e))?;

    if canon_root == canon_out || canon_root.starts_with(&canon_out) {
        return Err(format!(
            "Refusing to clean output directory '{}' because it contains the input directory '{}'.",
            output_dir.display(),
            root_dir.display()
        ));
    }

    Ok(canon_out)
}

fn clean_orphaned_outputs(
    root_dir: &Path,
    output_dir: &Path,
    expected_rel_paths: &HashSet<PathBuf>,
) -> Result<usize, String> {
    if !output_dir.exists() {
        return Ok(0);
    }

    let canon_out = validate_clean_target(root_dir, output_dir)?;

    let mut files_to_check = Vec::new();
    let mut subdirs = Vec::new();
    let mut dirs_to_visit = vec![output_dir.to_path_buf()];

    while let Some(dir) = dirs_to_visit.pop() {
        let entries = fs::read_dir(&dir)
            .map_err(|e| format!("Could not read output directory '{}': {}", dir.display(), e))?;

        for entry in entries {
            let entry = entry.map_err(|e| format!("Error reading output entry: {}", e))?;
            let path = entry.path();
            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if is_ignored_dir_name(file_name) {
                continue;
            }

            if path.is_dir() {
                subdirs.push(path.clone());
                dirs_to_visit.push(path);
            } else if path.is_file() {
                files_to_check.push(path);
            }
        }
    }

    let mut removed = 0usize;
    for file in files_to_check {
        let rel = if let Ok(r) = file.strip_prefix(output_dir) {
            r.to_path_buf()
        } else if let Ok(canon_file) = file.canonicalize() {
            if let Ok(r) = canon_file.strip_prefix(&canon_out) {
                r.to_path_buf()
            } else {
                continue;
            }
        } else {
            continue;
        };

        if !expected_rel_paths.contains(&rel) {
            fs::remove_file(&file).map_err(|e| {
                format!("Could not remove orphaned file '{}': {}", file.display(), e)
            })?;
            removed += 1;
        }
    }

    // Remove empty subdirectories deepest-first (never removing output_dir itself)
    subdirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    for dir in subdirs {
        let is_empty = fs::read_dir(&dir)
            .map_err(|e| format!("Could not inspect directory '{}': {}", dir.display(), e))?
            .next()
            .is_none();
        if is_empty {
            fs::remove_dir(&dir).map_err(|e| {
                format!("Could not remove empty directory '{}': {}", dir.display(), e)
            })?;
        }
    }

    Ok(removed)
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

    if config.clean
        && let Some(out) = output_dir
        && out.exists()
    {
        validate_clean_target(root_dir, out)?;
    }

    let env_vars: HashMap<String, String> = std::env::vars().collect();
    let resolved_env = match config.active_env() {
        Some(env_cfg) => Some(ResolvedEnv::new(env_cfg, &env_vars)?),
        None => None,
    };

    // Phase 1: Scan input files
    let effective_root = if root_dir.is_file() {
        root_dir.parent().unwrap_or(root_dir)
    } else {
        root_dir
    };

    let mut files = Vec::new();
    if root_dir.is_file() {
        files.push(root_dir.to_path_buf());
    } else {
        let mut dirs_to_visit = vec![root_dir.to_path_buf()];
        while let Some(current_dir) = dirs_to_visit.pop() {
            let entries = fs::read_dir(&current_dir).map_err(|e| {
                format!("Could not read directory '{}': {}", current_dir.display(), e)
            })?;

            for entry in entries {
                let entry = entry.map_err(|e| format!("Error reading directory entry: {}", e))?;
                let path = entry.path();

                if should_ignore(&path, root_dir, output_dir) {
                    continue;
                }

                if path.is_dir() {
                    dirs_to_visit.push(path);
                } else if path.is_file() {
                    files.push(path);
                }
            }
        }
    }
    files.sort();

    let mut expected_rel_paths = HashSet::with_capacity(files.len());
    let mut created_dirs = HashSet::new();
    for path in &files {
        let rel = get_relative_path(path, effective_root);
        if let Some(out) = output_dir {
            let dest = out.join(&rel);
            if let Some(parent) = dest.parent()
                && created_dirs.insert(parent.to_path_buf())
            {
                fs::create_dir_all(parent).map_err(|e| {
                    format!("Could not create directory '{}': {}", parent.display(), e)
                })?;
            }
        }
        expected_rel_paths.insert(rel);
    }

    // Phase 2: Process files (sequentially for tiny batches, scoped threads otherwise)
    let available = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    let num_workers = available.min(files.len()).max(1);

    if num_workers <= 1 || files.len() < 4 {
        for path in &files {
            process_single_file(
                path,
                effective_root,
                output_dir,
                config,
                resolved_env.as_ref(),
                &mut stats,
            )?;
        }
    } else {
        let next_idx = AtomicUsize::new(0);
        let mut errors: Vec<(usize, String)> = Vec::new();

        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(num_workers);
            for _ in 0..num_workers {
                handles.push(s.spawn(|| {
                    let mut local_stats = ProcessStats::default();
                    let mut local_errors = Vec::new();
                    loop {
                        let idx = next_idx.fetch_add(1, Ordering::Relaxed);
                        if idx >= files.len() {
                            break;
                        }
                        if let Err(err) = process_single_file(
                            &files[idx],
                            effective_root,
                            output_dir,
                            config,
                            resolved_env.as_ref(),
                            &mut local_stats,
                        ) {
                            local_errors.push((idx, err));
                        }
                    }
                    (local_stats, local_errors)
                }));
            }

            for handle in handles {
                let (local_stats, local_errors) = handle.join().expect("worker thread panicked");
                stats.merge(&local_stats);
                errors.extend(local_errors);
            }
        });

        if !errors.is_empty() {
            errors.sort_by_key(|(idx, _)| *idx);
            let combined = errors
                .into_iter()
                .map(|(_, msg)| msg)
                .collect::<Vec<_>>()
                .join("\n");
            return Err(combined);
        }
    }

    // Phase 3: Orphan output cleanup
    if config.clean
        && let Some(out) = output_dir
    {
        stats.files_removed = clean_orphaned_outputs(root_dir, out, &expected_rel_paths)?;
    }

    Ok(stats)
}
