use gd_core::db::KeyStore;

pub fn run(store: &KeyStore) {
    eprintln!("links: {}", store.link_count());
    eprintln!("history: {} directories", store.history_count());

    // Daemon 狀態:daemon.pid 判斷存活,daemon.mode(daemon 每次進入
    // 監看/降級迴圈時寫入)判斷是事件驅動還是降級掃描。
    let data_dir = dirs::data_dir().unwrap_or_default().join("gd");
    let pid_running = std::fs::read_to_string(data_dir.join("daemon.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(|pid| std::path::Path::new(&format!("/proc/{pid}")).exists());
    let mode = std::fs::read_to_string(data_dir.join("daemon.mode"))
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    match (pid_running, mode.as_str()) {
        (true, "fanotify") => eprintln!("daemon: running (fanotify, event-driven)"),
        (true, "poll") => eprintln!(
            "daemon: running — DEGRADED: fanotify unavailable, low-frequency \
             idle-priority rescan (why: journalctl --user -u gd-daemon; \
             disable scanning: gd config daemon.fallback off)"
        ),
        (true, "off") => eprintln!(
            "daemon: running — fanotify unavailable, scanning disabled \
             (daemon.fallback=off); index grows only from shell visits"
        ),
        (true, _) => eprintln!("daemon: running"),
        (false, _) => eprintln!("daemon: not running (systemctl --user start gd-daemon)"),
    }

    // Check fd availability
    let has_fd = std::process::Command::new("fd")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());

    if has_fd {
        eprintln!("scanner: fd (fast)");
    } else {
        eprintln!("scanner: find (fallback — install fd for better performance)");
    }

    if let Ok(shell) = std::env::var("SHELL") {
        let name = if shell.ends_with("/bash") {
            "bash"
        } else if shell.ends_with("/zsh") {
            "zsh"
        } else if shell.ends_with("/fish") {
            "fish"
        } else {
            "unknown"
        };

        if name != "unknown" {
            eprintln!("shell: {name}");
            eprintln!("  hint: ensure eval \"$(gd init {name})\" is in your shell config");
        }
    }

    eprintln!("all checks passed.");
}
