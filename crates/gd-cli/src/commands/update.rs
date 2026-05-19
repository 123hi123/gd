use anyhow::{Context, Result};
use std::process::Command;

pub fn run() -> Result<()> {
    eprintln!("stopping gd-daemon...");
    let _ = Command::new("systemctl")
        .args(["--user", "stop", "gd-daemon"])
        .status();

    eprintln!("building release...");
    let build = Command::new("cargo")
        .args(["build", "--release", "--all"])
        .status()
        .context("cargo build failed")?;

    if !build.success() {
        anyhow::bail!("build failed");
    }

    let home = dirs::home_dir().context("cannot determine home directory")?;
    let cargo_bin = home.join(".cargo/bin");
    let target_dir = find_target_dir()?;

    // Copy binaries
    for name in ["gd", "gd-daemon"] {
        let src = target_dir.join(name);
        let dst = cargo_bin.join(name);
        if src.exists() {
            std::fs::copy(&src, &dst)
                .with_context(|| format!("failed to copy {name}"))?;
            eprintln!("updated {}", dst.display());
        }
    }

    // Re-set capability
    let daemon_bin = cargo_bin.join("gd-daemon");
    let cap = Command::new("sudo")
        .args(["setcap", "cap_sys_admin+ep"])
        .arg(&daemon_bin)
        .status();
    match cap {
        Ok(s) if s.success() => {}
        _ => eprintln!("warning: setcap failed. Run: sudo setcap cap_sys_admin+ep {}", daemon_bin.display()),
    }

    // Restart daemon — it will do a catchup scan, not full scan
    eprintln!("restarting gd-daemon...");
    let _ = Command::new("systemctl")
        .args(["--user", "start", "gd-daemon"])
        .status();

    eprintln!("update complete.");
    Ok(())
}

fn find_target_dir() -> Result<std::path::PathBuf> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .output()
        .context("cargo metadata failed")?;

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("parse cargo metadata")?;

    let target = json["target_directory"]
        .as_str()
        .context("no target_directory in metadata")?;

    Ok(std::path::PathBuf::from(target).join("release"))
}
