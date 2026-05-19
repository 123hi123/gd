mod fan;
mod scanner;

use anyhow::{Context, Result};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

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

    let index_path = data_dir.join("index");
    let pid_file = data_dir.join("daemon.pid");
    let timestamp_file = data_dir.join("daemon.timestamp");

    std::fs::write(&pid_file, std::process::id().to_string())?;

    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };

    let fan_fd = fan::init()
        .context("fanotify_init failed. Is CAP_SYS_ADMIN set?\n  sudo setcap cap_sys_admin+ep $(which gd-daemon)")?;

    fan::mark_filesystem(fan_fd, &home)
        .context("fanotify_mark failed")?;

    // Initial scan or catchup
    if index_path.exists() {
        let last_ts = read_timestamp(&timestamp_file);
        if let Some(since) = last_ts {
            eprintln!("gd-daemon: catching up since last shutdown...");
            match scanner::catchup_scan_to_file(&home, &index_path, &index_path, since) {
                Ok(added) => eprintln!("gd-daemon: added {added} new dirs. Watching."),
                Err(e) => {
                    eprintln!("gd-daemon: catchup failed ({e}), doing full scan...");
                    let count = scanner::full_scan_to_file(&home, &index_path)?;
                    eprintln!("gd-daemon: indexed {count} dirs. Watching.");
                }
            }
        } else {
            eprintln!("gd-daemon: index exists. Watching for changes.");
        }
    } else {
        eprintln!("gd-daemon: no index, scanning {}...", home.display());
        let count = scanner::full_scan_to_file(&home, &index_path)?;
        eprintln!("gd-daemon: indexed {count} dirs. Watching.");
    }

    // Event loop
    let mut first_event_at: Option<Instant> = None;

    while running.load(Ordering::Relaxed) {
        match fan::poll_events(fan_fd, 2000) {
            Ok(true) => {
                fan::drain(fan_fd).ok();
                if first_event_at.is_none() {
                    first_event_at = Some(Instant::now());
                }
            }
            Ok(false) => {}
            Err(_) => {}
        }

        if let Some(first) = first_event_at {
            if first.elapsed() > Duration::from_secs(3) {
                fan::drain(fan_fd).ok();
                match scanner::full_scan_to_file(&home, &index_path) {
                    Ok(count) => eprintln!("gd-daemon: reindexed ({count} dirs)"),
                    Err(e) => eprintln!("gd-daemon: rescan error: {e}"),
                }
                first_event_at = None;
            }
        }
    }

    // Write shutdown timestamp so next startup can do catchup
    write_timestamp(&timestamp_file);
    let _ = std::fs::remove_file(&pid_file);
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
