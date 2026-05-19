use std::collections::BTreeSet;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

pub fn search_file(data_dir: &Path, query: &str) -> Vec<PathBuf> {
    let file_path = data_dir.join("index");
    let Ok(file) = fs::File::open(&file_path) else {
        return Vec::new();
    };

    let query_lower = query.to_lowercase();
    let reader = io::BufReader::with_capacity(64 * 1024, file);

    reader
        .lines()
        .map_while(Result::ok)
        .filter(|line| {
            if line.is_empty() {
                return false;
            }
            let basename = line.rsplit('/').next().unwrap_or("");
            basename.to_lowercase().contains(&query_lower)
        })
        .map(PathBuf::from)
        .collect()
}

pub fn index_exists(data_dir: &Path) -> bool {
    let file_path = data_dir.join("index");
    file_path.exists() && fs::metadata(&file_path).is_ok_and(|m| m.len() > 0)
}

pub struct PathIndex {
    paths: BTreeSet<PathBuf>,
    file_path: PathBuf,
    dirty: bool,
}

impl PathIndex {
    pub fn open(data_dir: &Path) -> Self {
        let file_path = data_dir.join("index");
        let paths = if file_path.exists() {
            load_from_file(&file_path).unwrap_or_default()
        } else {
            BTreeSet::new()
        };

        Self {
            paths,
            file_path,
            dirty: false,
        }
    }

    pub fn add(&mut self, path: PathBuf) {
        if self.paths.insert(path) {
            self.dirty = true;
        }
    }

    pub fn remove(&mut self, path: &Path) {
        if self.paths.remove(path) {
            self.dirty = true;
        }
    }

    pub fn search(&self, query: &str) -> Vec<PathBuf> {
        let query_lower = query.to_lowercase();
        self.paths
            .iter()
            .filter(|p| {
                let basename = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("");
                basename.to_lowercase().contains(&query_lower)
            })
            .cloned()
            .collect()
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn flush(&mut self) -> io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        self.save()?;
        self.dirty = false;
        Ok(())
    }

    pub fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.file_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let tmp = self.file_path.with_extension("tmp");
        let mut file = fs::File::create(&tmp)?;
        for path in &self.paths {
            writeln!(file, "{}", path.display())?;
        }
        file.sync_all()?;
        fs::rename(&tmp, &self.file_path)?;
        Ok(())
    }

    pub fn replace_all(&mut self, paths: BTreeSet<PathBuf>) {
        self.paths = paths;
        self.dirty = true;
    }

    pub fn file_path(&self) -> &Path {
        &self.file_path
    }

    pub fn exists(&self) -> bool {
        self.file_path.exists()
    }
}

fn load_from_file(path: &Path) -> io::Result<BTreeSet<PathBuf>> {
    let file = fs::File::open(path)?;
    let reader = io::BufReader::new(file);
    let mut set = BTreeSet::new();
    for line in reader.lines() {
        let line = line?;
        if !line.is_empty() {
            set.insert(PathBuf::from(line));
        }
    }
    Ok(set)
}
