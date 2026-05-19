use std::path::{Path, PathBuf};

pub fn normalize(path: &Path) -> std::io::Result<PathBuf> {
    let expanded = expand_tilde(path);
    let canonical = std::fs::canonicalize(&expanded)?;
    Ok(canonical)
}

pub fn display_with_tilde(path: &Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(stripped) = path.strip_prefix(&home) {
            return format!("~/{}", stripped.display());
        }
    }
    path.display().to_string()
}

fn expand_tilde(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("~/") || s == "~" {
        if let Some(home) = dirs::home_dir() {
            return home.join(s.strip_prefix("~/").unwrap_or(""));
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tilde_display() {
        if let Some(home) = dirs::home_dir() {
            let p = home.join("foo/bar");
            assert_eq!(display_with_tilde(&p), "~/foo/bar");
        }
    }

    #[test]
    fn non_home_path_unchanged() {
        let p = Path::new("/tmp/something");
        assert_eq!(display_with_tilde(p), "/tmp/something");
    }
}
