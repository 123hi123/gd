use gd_core::index::PathIndex;
use jwalk::WalkDir;
use std::path::Path;

const EXCLUDE_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".cache",
    "__pycache__",
    ".venv",
    "venv",
    ".tox",
    "dist",
    "build",
    ".gradle",
    ".m2",
    "vendor",
    ".npm",
    ".cargo",
    ".rustup",
    ".nvm",
    ".conda",
    "snap",
];

pub fn full_scan(root: &Path, index: &PathIndex) -> std::io::Result<usize> {
    index.mark_all_not_indexed();
    index.begin_bulk();
    let mut count = 0usize;

    for entry in WalkDir::new(root)
        .skip_hidden(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(2))
        .process_read_dir(|_, _, _, entries| {
            entries.retain(|e| {
                if let Ok(entry) = e {
                    if let Some(name) = entry.file_name().to_str() {
                        if EXCLUDE_NAMES.contains(&name) {
                            return false;
                        }
                    }
                }
                true
            });
        })
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_dir() {
            index.add(entry.path().to_path_buf());
            count += 1;
        }
    }

    index.end_bulk();
    index.cleanup_stale();
    Ok(count)
}

pub fn catchup_scan(root: &Path, index: &PathIndex, since: u64) -> std::io::Result<usize> {
    index.begin_bulk();
    let mut added = 0usize;

    for entry in WalkDir::new(root)
        .skip_hidden(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(2))
        .process_read_dir(|_, _, _, entries| {
            entries.retain(|e| {
                if let Ok(entry) = e {
                    if let Some(name) = entry.file_name().to_str() {
                        if EXCLUDE_NAMES.contains(&name) {
                            return false;
                        }
                    }
                }
                true
            });
        })
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_dir() {
            if let Ok(meta) = entry.path().metadata() {
                if let Ok(mtime) = meta.modified() {
                    let mtime_secs = mtime
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    if mtime_secs > since {
                        index.add(entry.path().to_path_buf());
                        added += 1;
                    }
                }
            }
        }
    }

    index.end_bulk();
    Ok(added)
}
