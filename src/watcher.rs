use crate::config::LimonConfig;
use crate::processor::{is_luau_file, process_project, process_single_file, should_ignore, ProcessStats};
use notify::{Event, RecursiveMode, Result as NotifyResult, Watcher};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;
use std::time::Duration;

pub fn watch_project(
    root_dir: &Path,
    output_dir: Option<&Path>,
    explicit_config_path: Option<&Path>,
) -> Result<(), String> {
    let mut config_path_opt = explicit_config_path.map(|p| p.to_path_buf());
    let (mut config, loaded_config_path) = LimonConfig::find_and_load(root_dir, explicit_config_path)?;
    if config_path_opt.is_none() {
        config_path_opt = loaded_config_path;
    }

    let final_output = output_dir
        .map(|p| p.to_path_buf())
        .or_else(|| config.output.as_ref().map(PathBuf::from));

    println!("[limon] Starting initial process pass on '{}'...", root_dir.display());
    if let Some(ref out) = final_output {
        println!("[limon] Output directory: '{}'", out.display());
    } else {
        println!("[limon] Output mode: in-place");
    }

    let initial_stats = process_project(root_dir, final_output.as_deref(), &config)?;
    println!(
        "[limon] Initial processing complete: scanned {}, processed {}, modified {}, skipped without .luaurc {}",
        initial_stats.files_scanned,
        initial_stats.files_processed,
        initial_stats.files_modified,
        initial_stats.files_skipped_no_luaurc
    );

    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res: NotifyResult<Event>| {
        if let Ok(event) = res {
            let _ = tx.send(event);
        }
    })
    .map_err(|e| format!("Failed to create file watcher: {}", e))?;

    watcher
        .watch(root_dir, RecursiveMode::Recursive)
        .map_err(|e| format!("Failed to watch directory '{}': {}", root_dir.display(), e))?;

    println!("[limon] Watching '{}' for changes (Press Ctrl+C to exit)...", root_dir.display());

    let mut cache = HashMap::new();

    while let Ok(first_event) = rx.recv() {

        // Debounce window (100ms)
        std::thread::sleep(Duration::from_millis(100));

        let mut changed_paths = HashSet::new();
        for path in first_event.paths {
            changed_paths.insert(path);
        }

        while let Ok(event) = rx.try_recv() {
            for path in event.paths {
                changed_paths.insert(path);
            }
        }

        let mut needs_full_reprocess = false;
        let mut files_to_update = Vec::new();

        for path in changed_paths {
            if should_ignore(&path, root_dir, final_output.as_deref()) {
                continue;
            }

            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");

            if file_name == "limon.toml" || file_name == ".luaurc" {
                needs_full_reprocess = true;
                break;
            }

            if is_luau_file(&path) || path.is_file() {
                files_to_update.push(path);
            }
        }

        if needs_full_reprocess {
            println!("[limon] Configuration changed (.luaurc / limon.toml). Reloading...");
            cache.clear();
            if let Ok((reloaded_cfg, _)) = LimonConfig::find_and_load(root_dir, config_path_opt.as_deref()) {
                config = reloaded_cfg;
            }
            match process_project(root_dir, final_output.as_deref(), &config) {
                Ok(stats) => {
                    println!(
                        "[limon] Full re-process complete: modified {} of {} files",
                        stats.files_modified, stats.files_processed
                    );
                }
                Err(err) => {
                    eprintln!("[limon] Re-process error: {}", err);
                }
            }
        } else {
            for file in files_to_update {
                if !file.exists() {
                    // File was deleted
                    if let Some(ref out) = final_output {
                        let rel = file.strip_prefix(root_dir).unwrap_or(&file);
                        let dest = out.join(rel);
                        if dest.exists() {
                            let _ = std::fs::remove_file(&dest);
                            println!("[limon] Deleted output file: '{}'", dest.display());
                        }
                    }
                    continue;
                }

                let mut stats = ProcessStats::default();
                match process_single_file(
                    &file,
                    root_dir,
                    final_output.as_deref(),
                    &config,
                    &mut cache,
                    &mut stats,
                ) {
                    Ok(modified) => {
                        let rel = file.strip_prefix(root_dir).unwrap_or(&file);
                        if modified {
                            println!("[limon] Transformed: '{}'", rel.display());
                        } else if final_output.is_some() {
                            println!("[limon] Synchronized: '{}'", rel.display());
                        }
                    }
                    Err(err) => {
                        eprintln!("[limon] Error processing '{}': {}", file.display(), err);
                    }
                }
            }
        }
    }

    Ok(())
}
