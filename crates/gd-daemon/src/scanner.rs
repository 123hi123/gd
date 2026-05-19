use jwalk::WalkDir;
use std::fs;
use std::io::{BufWriter, Write};
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
    ".local",
    ".nvm",
    ".conda",
    "snap",
];

/// Stream scan results directly to file. Never holds all paths in memory.
/// Returns the number of directories found.
pub fn full_scan_to_file(root: &Path, output: &Path) -> std::io::Result<usize> {
    let tmp = output.with_extension("tmp");
    let file = fs::File::create(&tmp)?;
    let mut writer = BufWriter::with_capacity(128 * 1024, file);
    let mut count = 0usize;

    for entry in WalkDir::new(root)
        .skip_hidden(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(2)) // limit threads
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
            writeln!(writer, "{}", entry.path().display())?;
            count += 1;
        }
    }

    writer.flush()?;
    drop(writer);
    fs::rename(&tmp, output)?;
    Ok(count)
}

/// Quick catchup scan: only find dirs modified since a given timestamp.
/// Walks the full tree but only adds dirs with mtime > since.
pub fn catchup_scan_to_file(root: &Path, existing_index: &Path, output: &Path, since: u64) -> std::io::Result<usize> {
    // Start from existing index
    fs::copy(existing_index, output)?;

    let file = fs::File::options().append(true).open(output)?;
    let mut writer = BufWriter::new(file);
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
                        writeln!(writer, "{}", entry.path().display())?;
                        added += 1;
                    }
                }
            }
        }
    }

    writer.flush()?;
    Ok(added)
}
