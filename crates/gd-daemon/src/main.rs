mod fan;
mod scanner;

use anyhow::{Context, Result};
use gd_core::index::PathIndex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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

fn is_excluded(path: &std::path::Path) -> bool {
    path.components().any(|c| {
        if let std::path::Component::Normal(name) = c {
            if let Some(s) = name.to_str() {
                return EXCLUDE_NAMES.contains(&s);
            }
        }
        false
    })
}

fn main() -> Result<()> {
    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();
    signal_hook::flag::register(signal_hook::consts::SIGTERM, r.clone())?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, r)?;

    let home = dirs::home_dir().context("cannot determine home directory")?;
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gd");
    std::fs::create_dir_all(&data_dir)?;

    let pid_file = data_dir.join("daemon.pid");
    let timestamp_file = data_dir.join("daemon.timestamp");

    std::fs::write(&pid_file, std::process::id().to_string())?;

    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };

    let fan_fd = fan::init()
        .context("fanotify_init failed. Is CAP_SYS_ADMIN set?\n  sudo setcap cap_sys_admin,cap_dac_read_search+ep $(which gd-daemon)")?;

    let mount_fd = fan::open_mount_fd(&home)
        .context("cannot open home directory for mount fd")?;

    fan::mark_filesystem(fan_fd, &home)
        .context("fanotify_mark failed")?;

    let index = PathIndex::open(&data_dir);

    if !index.has_data() {
        eprintln!("gd-daemon: no index, scanning {}...", home.display());
        let count = scanner::full_scan(&home, &index)?;
        eprintln!("gd-daemon: indexed {count} dirs.");
    } else {
        let last_ts = read_timestamp(&timestamp_file);
        if let Some(since) = last_ts {
            eprintln!("gd-daemon: catching up since last shutdown...");
            match scanner::catchup_scan(&home, &index, since) {
                Ok(added) => eprintln!("gd-daemon: added {added} new dirs."),
                Err(e) => {
                    eprintln!("gd-daemon: catchup failed ({e}), doing full scan...");
                    let count = scanner::full_scan(&home, &index)?;
                    eprintln!("gd-daemon: indexed {count} dirs.");
                }
            }
        } else {
            eprintln!("gd-daemon: index exists.");
        }
    }

    unsafe { libc::malloc_trim(0) };
    eprintln!("gd-daemon: {} dirs indexed. Watching.", index.len());

    // Event loop
    let mut last_flush = Instant::now();

    while running.load(Ordering::Relaxed) {
        match fan::poll_events(fan_fd, 2000) {
            Ok(true) => {
                let events = fan::read_events(fan_fd, mount_fd);
                for event in events {
                    match event {
                        fan::DirEvent::Created(path) => {
                            if !is_excluded(&path) {
                                index.add(path);
                            }
                        }
                        fan::DirEvent::Deleted(path) => {
                            index.remove(&path);
                        }
                    }
                }

                if last_flush.elapsed().as_secs() >= 5 {
                    if let Err(e) = index.flush() {
                        eprintln!("gd-daemon: flush error: {e}");
                    }
                    last_flush = Instant::now();
                }
            }
            Ok(false) => {
                if let Err(e) = index.flush() {
                    eprintln!("gd-daemon: flush error: {e}");
                }
                last_flush = Instant::now();
            }
            Err(_) => {}
        }
    }

    if let Err(e) = index.flush() {
        eprintln!("gd-daemon: final flush error: {e}");
    }
    write_timestamp(&timestamp_file);
    let _ = std::fs::remove_file(&pid_file);
    unsafe { libc::close(mount_fd) };
    eprintln!("gd-daemon: stopped.");
    Ok(())
}

fn read_timestamp(path: &std::path::Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn write_timestamp(path: &std::path::Path) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = std::fs::write(path, now.to_string());
}
