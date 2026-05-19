use anyhow::{Context, Result};
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::process::Command;

const SERVICE_CONTENT: &str = include_str!("../shell/gd-daemon.service");

pub fn run() -> Result<()> {
    let home = dirs::home_dir().context("cannot determine home directory")?;

    // 1. Install systemd user service
    let service_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&service_dir)?;
    let service_path = service_dir.join("gd-daemon.service");
    fs::write(&service_path, SERVICE_CONTENT)?;
    eprintln!("installed {}", service_path.display());

    // 2. Set CAP_SYS_ADMIN on daemon binary
    let daemon_bin = find_daemon_binary(&home);
    if let Some(ref bin) = daemon_bin {
        eprintln!("setting CAP_SYS_ADMIN on {}...", bin.display());
        let status = Command::new("sudo")
            .args(["setcap", "cap_sys_admin+ep"])
            .arg(bin)
            .status();
        match status {
            Ok(s) if s.success() => eprintln!("capability set."),
            _ => eprintln!("warning: failed to set capability. Run manually:\n  sudo setcap cap_sys_admin+ep {}", bin.display()),
        }
    } else {
        eprintln!("warning: gd-daemon binary not found. Install it first:\n  cargo install --path crates/gd-daemon");
    }

    // 3. Enable and start service
    let _ = Command::new("systemctl")
        .args(["--user", "daemon-reload"])
        .status();

    let enable = Command::new("systemctl")
        .args(["--user", "enable", "--now", "gd-daemon"])
        .status();

    match enable {
        Ok(s) if s.success() => eprintln!("gd-daemon service enabled and started."),
        _ => eprintln!("warning: could not enable service. Try:\n  systemctl --user enable --now gd-daemon"),
    }

    // 4. Install shell hook
    let shell = detect_current_shell();
    let rc_path = shell_rc_path(&home, &shell);

    if let Some(ref rc) = rc_path {
        install_shell_hook(rc, &shell)?;

        // 5. Ask about cd alias
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

fn read_answer() -> bool {
    let stdin = io::stdin();
    let mut line = String::new();
    if stdin.lock().read_line(&mut line).is_ok() {
        let trimmed = line.trim().to_lowercase();
        trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
    } else {
        true
    }
}

fn print_manual_instructions() {
    eprintln!("  zsh:        eval \"$(gd init zsh)\"");
    eprintln!("  bash:       eval \"$(gd init bash)\"");
    eprintln!("  fish:       gd init fish | source");
    eprintln!("  nushell:    source (gd init nu)");
    eprintln!("  powershell: Invoke-Expression (gd init powershell)");
}

fn find_daemon_binary(home: &std::path::Path) -> Option<PathBuf> {
    let candidates = [
        home.join(".cargo/bin/gd-daemon"),
        PathBuf::from("/usr/local/bin/gd-daemon"),
        PathBuf::from("/usr/bin/gd-daemon"),
    ];
    candidates.into_iter().find(|p| p.exists())
}
