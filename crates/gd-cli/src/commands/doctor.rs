use gd_core::db::KeyStore;
use std::path::Path;

/// 回傳 true 表示所有硬性檢查都通過(呼叫端據此決定 exit code)。
///
/// `data_dir` 是 `--data-dir` 的覆寫值;None 代表用預設位置。daemon 的
/// 狀態檔跟 DB 放在同一個目錄,兩者必須看同一個地方,否則 `--data-dir`
/// 指到副本時 doctor 會一邊報副本的統計、一邊報真實 daemon 的狀態。
pub fn run(store: &KeyStore, data_dir: Option<&Path>) -> bool {
    let mut failed = 0usize;

    eprintln!("links: {}", store.link_count());
    eprintln!("history: {} directories", store.history_count());

    // Daemon 狀態:daemon.pid 判斷存活,daemon.mode(daemon 每次進入
    // 監看/降級迴圈時寫入)判斷是事件驅動還是降級掃描。
    let data_dir = data_dir.map_or_else(
        || dirs::data_dir().unwrap_or_default().join("gd"),
        Path::to_path_buf,
    );
    let pid_running = std::fs::read_to_string(data_dir.join("daemon.pid"))
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(is_gd_daemon);
    // daemon.mode 跟 daemon.pid 一樣是沒人刪的殘留檔,只有在 pid 確定活著時
    // 才採信它;否則一律報 not running,不要拿上一任的模式亂講話。
    let mode = if pid_running {
        std::fs::read_to_string(data_dir.join("daemon.mode"))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
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
        (false, _) => {
            eprintln!("daemon: not running (systemctl --user start gd-daemon)");
            failed += 1;
        }
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

    if failed == 0 {
        eprintln!("all checks passed.");
        true
    } else {
        eprintln!("{failed} check(s) failed.");
        false
    }
}

/// `/proc/<pid>` 存在不代表那個 pid 就是 gd-daemon:daemon.pid 只在乾淨退出時
/// 才刪,被 SIGKILL 就留下殘檔,而該 pid 早晚會被別的行程重用(實測撿到
/// docker-proxy)。必須比對 `/proc/<pid>/comm` 才算數。
fn is_gd_daemon(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/comm"))
        .is_ok_and(|comm| comm.trim() == "gd-daemon")
}
