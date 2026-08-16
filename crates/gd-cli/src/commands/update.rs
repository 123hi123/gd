use anyhow::{Context, Result};
use std::fs;
use std::path::Path;
use std::process::Command;

pub fn run() -> Result<()> {
    eprintln!("stopping gd-daemon...");
    let _ = Command::new("systemctl")
        .args(["--user", "stop", "gd-daemon"])
        .status();

    let result = build_and_install();

    // 中間任何一步失敗都不能就這樣早退:daemon 已經被 stop 掉,而 unit 沒有
    // 任何東西會把它拉回來,索引就這樣停更。無論成敗都先 start,再拋錯。
    eprintln!("restarting gd-daemon...");
    let _ = Command::new("systemctl")
        .args(["--user", "start", "gd-daemon"])
        .status();

    result?;

    eprintln!("update complete.");
    Ok(())
}

/// 停 daemon 之後、起 daemon 之前的所有工作。抽成獨立函式是為了讓呼叫端能
/// 在失敗路徑上也保證把 daemon 拉回來。
fn build_and_install() -> Result<()> {
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
            install_binary(&src, &dst)
                .with_context(|| format!("failed to copy {name}"))?;
            eprintln!("updated {}", dst.display());
        }
    }

    // Re-set capability
    let daemon_bin = cargo_bin.join("gd-daemon");
    let cap = Command::new("sudo")
        .args(["setcap", "cap_sys_admin,cap_dac_read_search+ep"])
        .arg(&daemon_bin)
        .status();
    match cap {
        Ok(s) if s.success() => {}
        _ => eprintln!("warning: setcap failed. Run: sudo setcap cap_sys_admin,cap_dac_read_search+ep {}", daemon_bin.display()),
    }

    // Refresh the systemd unit (resource limits travel with the binary)
    match crate::commands::setup::install_service_unit(&home) {
        Ok(p) => {
            eprintln!("refreshed {}", p.display());
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .status();
        }
        Err(e) => eprintln!("warning: could not refresh service unit: {e}"),
    }

    Ok(())
}

/// 把 `src` 裝到 `dst`。`gd update` 幾乎都是由 `~/.cargo/bin/gd` 自己跑的,
/// 而 `fs::copy` 覆寫一個正在執行的映像會拿到 ETXTBSY(26) —— 也就是說標準
/// 部署流程 100% 會失敗在第一個檔案上。做法:先把舊檔 rename 成 `<name>.old`
/// (rename 對執行中的映像是合法的,執行中的 process 繼續用舊 inode),再寫入
/// 新檔;成功後刪掉 `.old`(刪不掉就算了,inode 還被 hold 著,下次會被蓋掉)。
/// rename 失敗(例如 dst 本來就不存在)就直接走原本的 copy。
fn install_binary(src: &Path, dst: &Path) -> Result<()> {
    let backup = dst.with_extension("old");
    let renamed = fs::rename(dst, &backup).is_ok();

    match fs::copy(src, dst) {
        Ok(_) => {
            if renamed {
                let _ = fs::remove_file(&backup);
            }
            Ok(())
        }
        Err(e) => {
            // 新檔沒寫成,把舊檔搬回來——別讓使用者連舊版都沒得用
            if renamed {
                let _ = fs::rename(&backup, dst);
            }
            Err(e.into())
        }
    }
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
