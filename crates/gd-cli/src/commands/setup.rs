use anyhow::{Context, Result};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

const SERVICE_CONTENT: &str = include_str!("../shell/gd-daemon.service");

/// 寫入(或覆寫)systemd user unit。`gd setup` 與 `gd --update` 共用:
/// unit 裡的資源限制(Nice/IOSchedulingClass 等)也是程式的一部分,
/// 更新 binary 時必須一起跟上。ExecStart 指向實際裝好的 daemon —— 每台機器
/// 的安裝目錄不同(~/.cargo/bin、~/.local/bin),範本寫死的路徑只是預設。
pub fn install_service_unit(home: &Path, daemon_bin: Option<&Path>) -> Result<PathBuf> {
    let service_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&service_dir)?;
    let service_path = service_dir.join("gd-daemon.service");
    fs::write(&service_path, render_service_unit(home, daemon_bin))?;
    Ok(service_path)
}

fn render_service_unit(home: &Path, daemon_bin: Option<&Path>) -> String {
    let Some(bin) = daemon_bin else {
        return SERVICE_CONTENT.to_string();
    };
    // 家目錄底下的路徑保留 %h 寫法,跟範本一致
    let exec = match bin.strip_prefix(home) {
        Ok(rel) => format!("%h/{}", rel.display()),
        Err(_) => bin.display().to_string(),
    };
    let mut out = String::with_capacity(SERVICE_CONTENT.len() + 32);
    for line in SERVICE_CONTENT.lines() {
        if line.starts_with("ExecStart=") {
            out.push_str("ExecStart=");
            out.push_str(&exec);
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

pub fn run() -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;

    // 1. Find the daemon binary — the unit's ExecStart has to point at it
    let daemon_bin = find_daemon_binary(&home);

    // 2. Install systemd user service
    let service_path = install_service_unit(&home, daemon_bin.as_deref())?;
    eprintln!("installed {}", service_path.display());

    // 3. Set CAP_SYS_ADMIN on daemon binary
    if let Some(ref bin) = daemon_bin {
        eprintln!("setting CAP_SYS_ADMIN on {}...", bin.display());
        let status = Command::new("sudo")
            .args(["setcap", "cap_sys_admin,cap_dac_read_search+ep"])
            .arg(bin)
            .status();
        match status {
            Ok(s) if s.success() => eprintln!("capability set."),
            _ => eprintln!("warning: failed to set capability. Run manually:\n  sudo setcap cap_sys_admin,cap_dac_read_search+ep {}", bin.display()),
        }
    } else {
        eprintln!("warning: gd-daemon binary not found. Install it first:\n  cargo install --path crates/gd-daemon");
    }

    // 4. Enable and start service
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    // daemon.mode 是上一任 daemon 留下的殘留檔,沒人會刪。啟動前先清掉,
    // report_watch_mode() 讀到的才保證是「這次」啟動寫的:實測 unit 被
    // ConditionPathExists 擋掉時 `systemctl enable --now` 仍回 exit 0,
    // 不清就會拿舊檔去報一個根本沒起來的 daemon 的模式。
    let _ = fs::remove_file(daemon_mode_path());

    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", "gd-daemon"])
        .status();

    match enable {
        Ok(s) if s.success() => {
            eprintln!("gd-daemon service enabled and started.");
            report_watch_mode();
        }
        _ => eprintln!("warning: could not enable service. Try:\n  systemctl --user enable --now gd-daemon"),
    }

    // 5. Install shell hook
    let shell = detect_current_shell();
    let rc_path = shell_rc_path(&home, &shell);

    if let Some(ref rc) = rc_path {
        install_shell_hook(rc, &shell)?;

        // 6. Ask about cd alias
        eprintln!();
        eprintln!("gd fully covers cd and adds smart search on top.");
        eprint!("replace cd with gd? (alias cd=gd) [Y/n] ");
        io::stderr().flush().ok();

        let answer = read_answer();
        if answer {
            install_cd_alias(rc, &shell)?;
            eprintln!("cd is now gd. You can remove it from {} anytime.", rc.display());
        }
    } else {
        eprintln!();
        eprintln!("could not detect shell rc file. Add manually:");
        print_manual_instructions();
    }

    eprintln!();
    eprintln!("setup complete. Restart your shell or run: exec {shell}");

    Ok(())
}

/// 裝完不能只說 setup complete:fanotify 掛不掛得上是機器性質(btrfs 子卷
/// 家目錄就是掛不上),使用者必須「當場」知道 daemon 實際跑在哪個模式,
/// 而不是日後從 iotop 發現它在降級掃描。daemon 一啟動就寫 daemon.mode
/// (早於首次建索引),這裡最多等 5 秒再回報;之後隨時可用 gd doctor 查。
fn daemon_mode_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_default()
        .join("gd")
        .join("daemon.mode")
}

fn report_watch_mode() {
    let mode_file = daemon_mode_path();

    let mut mode = String::new();
    for _ in 0..20 {
        std::thread::sleep(std::time::Duration::from_millis(250));
        if let Ok(m) = fs::read_to_string(&mode_file) {
            let m = m.trim().to_string();
            if !m.is_empty() {
                mode = m;
                break;
            }
        }
    }

    match mode.as_str() {
        "fanotify" => eprintln!("watch mode: fanotify — event-driven, no background scanning."),
        "poll" => eprintln!(
            "watch mode: DEGRADED — fanotify is unavailable on this filesystem \
             (btrfs subvolume homes are the usual cause).\n  \
             The daemon will instead rescan at idle priority every 30 min \
             and keep retrying fanotify.\n  \
             Details:           journalctl --user -u gd-daemon\n  \
             Disable scanning:  gd config daemon.fallback off"
        ),
        "off" => eprintln!(
            "watch mode: fanotify unavailable; background scanning disabled \
             (daemon.fallback=off) — the index grows only from your shell visits."
        ),
        _ => eprintln!(
            "watch mode: not reported yet — the first index build may still be \
             running, or the daemon did not actually start.\n  \
             Check:  systemctl --user status gd-daemon\n  \
             Later:  gd doctor"
        ),
    }
}

fn detect_current_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if shell.ends_with("/zsh") { return "zsh".into(); }
        if shell.ends_with("/bash") { return "bash".into(); }
        if shell.ends_with("/fish") { return "fish".into(); }
        if shell.contains("nu") { return "nu".into(); }
    }
    if std::env::var("PSModulePath").is_ok() {
        return "powershell".into();
    }
    "unknown".into()
}

fn shell_rc_path(home: &std::path::Path, shell: &str) -> Option<PathBuf> {
    match shell {
        "zsh" => Some(home.join(".zshrc")),
        "bash" => {
            let bashrc = home.join(".bashrc");
            let profile = home.join(".bash_profile");
            if bashrc.exists() { Some(bashrc) }
            else if profile.exists() { Some(profile) }
            else { Some(bashrc) }
        }
        "fish" => Some(home.join(".config/fish/config.fish")),
        "nu" => Some(home.join(".config/nushell/config.nu")),
        "powershell" => {
            if let Ok(profile) = std::env::var("PROFILE") {
                Some(PathBuf::from(profile))
            } else {
                Some(home.join(".config/powershell/Microsoft.PowerShell_profile.ps1"))
            }
        }
        _ => None,
    }
}

fn init_line(shell: &str) -> Option<String> {
    match shell {
        "zsh" => Some(r#"eval "$(gd init zsh)""#.into()),
        "bash" => Some(r#"eval "$(gd init bash)""#.into()),
        "fish" => Some("gd init fish | source".into()),
        "nu" => Some("source (gd init nu)".into()),
        "powershell" => Some("Invoke-Expression (gd init powershell)".into()),
        _ => None,
    }
}

fn cd_alias_line(shell: &str) -> Option<String> {
    match shell {
        "zsh" | "bash" => Some("alias cd=gd".into()),
        "fish" => Some("alias cd gd".into()),
        "nu" => Some("alias cd = gd".into()),
        "powershell" => Some("Set-Alias -Name cd -Value gd -Option AllScope".into()),
        _ => None,
    }
}

fn rc_contains(rc: &std::path::Path, needle: &str) -> bool {
    fs::read_to_string(rc)
        .map(|content| content.contains(needle))
        .unwrap_or(false)
}

fn append_to_rc(rc: &std::path::Path, line: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(rc)?;
    writeln!(file)?;
    writeln!(file, "{line}")?;
    Ok(())
}

fn install_shell_hook(rc: &std::path::Path, shell: &str) -> Result<()> {
    let Some(line) = init_line(shell) else { return Ok(()); };

    if rc_contains(rc, "gd init") {
        eprintln!("shell hook already in {}", rc.display());
    } else {
        append_to_rc(rc, &line)?;
        eprintln!("added shell hook to {}", rc.display());
    }
    Ok(())
}

fn install_cd_alias(rc: &std::path::Path, shell: &str) -> Result<()> {
    let Some(line) = cd_alias_line(shell) else { return Ok(()); };

    if rc_contains(rc, &line) {
        eprintln!("cd alias already in {}", rc.display());
    } else {
        append_to_rc(rc, &line)?;
        eprintln!("added cd alias to {}", rc.display());
    }
    Ok(())
}

/// stdin 是不是 tty。README 主推的 `curl -sSL ... | install.sh | bash` 會讓
/// stdin 變成 pipe,`read_line` 立刻回 Ok(0)(EOF)——舊版把它當成「按 Enter
/// 接受預設」,使用者根本沒被問就被裝上 alias cd=gd。
fn stdin_is_interactive() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::isatty(0) != 0 }
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// `[Y/n]` 的回答。非互動(pipe / CI / EOF / 讀取失敗)一律回 false:
/// 沒人在場回答時就不做有副作用的事。
fn read_answer() -> bool {
    if !stdin_is_interactive() {
        eprintln!();
        eprintln!("non-interactive input — skipping the cd alias (nothing was changed).");
        eprintln!("  to enable it: run `gd setup` in a terminal, or add `alias cd=gd` to your shell rc");
        return false;
    }

    let stdin = io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        // Ok(0) 是 EOF:沒有答案,不能當成同意
        Ok(0) | Err(_) => false,
        Ok(_) => {
            let trimmed = line.trim().to_lowercase();
            trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
        }
    }
}

fn print_manual_instructions() {
    eprintln!("  zsh:        eval \"$(gd init zsh)\"");
    eprintln!("  bash:       eval \"$(gd init bash)\"");
    eprintln!("  fish:       gd init fish | source");
    eprintln!("  nushell:    source (gd init nu)");
    eprintln!("  powershell: Invoke-Expression (gd init powershell)");
}

/// 先看 PATH 上「就是現在這個 gd」的那個目錄(`gd setup` 幾乎都是裝好的 gd
/// 自己跑的,兩個 binary 一起裝;用 PATH 寫法、不展開 symlink,跟 `gd --update`
/// 寫進 unit 的路徑一致),再看常見安裝位置。
/// 故意不看 `current_exe` 旁邊的檔:從 target/release/gd 跑 setup 會把 unit 綁到
/// build 產物上,下次 cargo build 一換 inode,capability 就掉了。
fn find_daemon_binary(home: &Path) -> Option<PathBuf> {
    let on_path = crate::commands::update::running_gd_dir().map(|dir| dir.join("gd-daemon"));
    let candidates = [
        on_path,
        Some(home.join(".cargo/bin/gd-daemon")),
        Some(home.join(".local/bin/gd-daemon")),
        Some(PathBuf::from("/usr/local/bin/gd-daemon")),
        Some(PathBuf::from("/usr/bin/gd-daemon")),
    ];
    candidates.into_iter().flatten().find(|p| p.is_file())
}
