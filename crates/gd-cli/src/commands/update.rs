use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 更新前先把「從哪裡 build、build 到哪、裝到哪」全部算好。任何一步失敗都
/// 直接回錯,daemon 連停都還沒停 —— 更新失敗絕不能留下副作用。
struct Plan {
    /// workspace 根目錄(有 Cargo.toml 的那層)
    root: PathBuf,
    /// `<target>/release`
    release_dir: PathBuf,
    /// gd / gd-daemon 要蓋到哪個目錄
    install_dir: PathBuf,
}

impl Plan {
    fn resolve() -> Result<Self> {
        let root = source_root()?;
        let release_dir = find_release_dir(&root)?;
        let install_dir = install_dir(&release_dir)?;
        Ok(Self { root, release_dir, install_dir })
    }
}

pub fn run() -> Result<()> {
    let plan = Plan::resolve()?;
    eprintln!("source:  {}", plan.root.display());
    eprintln!("install: {}", plan.install_dir.display());

    // 先 build 再停 daemon:build 動輒一分鐘以上,daemon 停超過 60 秒重啟時
    // 就會觸發 startup catchup 走整棵樹(見 CLAUDE.md)。build 失敗也不該
    // 碰到 daemon —— 以前在錯誤目錄下跑 update,daemon 被停了、build 炸了,
    // 什麼都沒更新到。
    build(&plan)?;

    eprintln!("stopping gd-daemon...");
    let _ = Command::new("systemctl")
        .args(["--user", "stop", "gd-daemon"])
        .status();

    let result = install(&plan);

    // 中間任何一步失敗都不能就這樣早退:daemon 已經被 stop 掉,而 unit 沒有
    // 任何東西會把它拉回來,索引就這樣停更。無論成敗都先 start,再拋錯。
    eprintln!("restarting gd-daemon...");
    let _ = Command::new("systemctl")
        .args(["--user", "start", "gd-daemon"])
        .status();

    result?;

    // start 的回傳值不可靠(撞到 StartLimitBurst、unit 條件不成立時也可能
    // 回 0),問一次真的有沒有在跑。binary 已經換好了,這裡只警告不報錯。
    let active = Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", "gd-daemon"])
        .status()
        .is_ok_and(|s| s.success());
    if active {
        eprintln!("update complete.");
    } else {
        eprintln!(
            "update complete, but gd-daemon is NOT running.\n  \
             Check:  systemctl --user status gd-daemon\n  \
             Retry:  systemctl --user reset-failed gd-daemon && systemctl --user start gd-daemon"
        );
    }
    Ok(())
}

/// 原始碼在哪。
/// 1. cwd(或任一祖先)是 gd checkout → 用它:worktree / 第二份 clone 要
///    build 的是「你人在的那份」,跟 cargo 自己往上找 Cargo.toml 的行為一致。
/// 2. 不在 checkout 裡 → 用編譯時烙進 binary 的 workspace 路徑(每台機器各自
///    build,烙進去的就是那台機器的 checkout),讓 `gd --update` 在任意目錄都能跑。
///
/// 以前只看 cwd,在任何其他目錄下更新都會炸。
fn source_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    if let Some(root) = cwd.ancestors().find(|d| is_gd_workspace(d)) {
        return Ok(root.to_path_buf());
    }

    let baked: Option<PathBuf> = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .map(Path::to_path_buf);
    if let Some(root) = &baked {
        if is_gd_workspace(root) {
            return Ok(root.clone());
        }
    }

    anyhow::bail!(
        "cannot find the gd source tree.\n  \
         built from: {}\n  \
         cwd:        {}\n  \
         Run `gd --update` from inside a gd checkout (that rebuild bakes in the new location).",
        baked.as_deref().map_or_else(|| "?".to_string(), |p| p.display().to_string()),
        cwd.display()
    )
}

fn is_gd_workspace(dir: &Path) -> bool {
    dir.join("Cargo.toml").is_file() && dir.join("crates/gd-cli/Cargo.toml").is_file()
}

/// 裝到「現在這個 gd 所在的目錄」。tuf 裝在 ~/.local/bin、arch 裝在
/// ~/.cargo/bin;寫死任一個,另一台就會把新版裝到 PATH 找不到的地方,
/// 結果 gd 舊、gd-daemon 新。
/// 目錄名用 PATH 上的寫法(~/.local/bin 可能是 symlink 進 dotfiles,
/// 展開後的實體路徑寫進 unit 就跟使用者認知脫節)。
/// 例外:直接跑 target/release/gd(開發時)—— 那就蓋掉 PATH 上已安裝的 gd;
/// 都沒有才退回 ~/.cargo/bin(首次安裝)。
fn install_dir(release_dir: &Path) -> Result<PathBuf> {
    // 1. PATH 上哪個目錄的 gd 就是現在這個 process → 用那個目錄
    if let Some(dir) = running_gd_dir() {
        return Ok(dir);
    }

    let target_root = release_dir.parent().unwrap_or(release_dir);
    let target_root = fs::canonicalize(target_root).unwrap_or_else(|_| target_root.to_path_buf());

    let exe = std::env::current_exe().context("cannot locate the running gd binary")?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);

    // 2. 不在 PATH 上、也不是 build 產物 → 就地更新自己
    if !exe.starts_with(&target_root) {
        if let Some(dir) = exe.parent() {
            return Ok(dir.to_path_buf());
        }
    }

    // 3. 跑的是 target/release/gd → 蓋掉 PATH 上已安裝的那份
    for dir in &path_dirs() {
        let candidate = dir.join("gd");
        if !candidate.is_file() {
            continue;
        }
        let resolved = fs::canonicalize(&candidate).unwrap_or(candidate);
        if !resolved.starts_with(&target_root) {
            return Ok(dir.clone());
        }
    }

    // 4. 首次安裝
    let home = dirs::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".cargo/bin"))
}

/// PATH 上哪個目錄的 `gd` 就是現在這個 process。回傳 PATH 上的寫法,不展開
/// symlink:`current_exe()` 在 Linux 是 /proc/self/exe,已經被 kernel 解到實體
/// 路徑(tuf 的 ~/.local/bin 是 symlink 進 dotfiles),把實體路徑寫進 unit,
/// dotfiles 一搬家 unit 就指到死路,而 PATH 寫法會跟著 symlink 走。
/// `gd setup` 也用這個找 gd-daemon,兩邊寫進 unit 的 `ExecStart` 才會一致。
pub fn running_gd_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let exe = fs::canonicalize(&exe).unwrap_or(exe);
    path_dirs()
        .into_iter()
        .find(|dir| fs::canonicalize(dir.join("gd")).is_ok_and(|p| p == exe))
}

fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).filter(|d| d.is_absolute()).collect())
        .unwrap_or_default()
}

fn build(plan: &Plan) -> Result<()> {
    eprintln!("building release...");
    let build = Command::new("cargo")
        .args(["build", "--release", "--all"])
        .current_dir(&plan.root)
        .status()
        .context("cargo build failed")?;

    if !build.success() {
        anyhow::bail!("build failed");
    }
    Ok(())
}

/// 停 daemon 之後、起 daemon 之前的所有工作。抽成獨立函式是為了讓呼叫端能
/// 在失敗路徑上也保證把 daemon 拉回來。
fn install(plan: &Plan) -> Result<()> {
    fs::create_dir_all(&plan.install_dir)
        .with_context(|| format!("cannot create {}", plan.install_dir.display()))?;

    // 兩個 binary 要嘛一起換、要嘛都不換:先全部 copy 成 .new,都成功了才
    // 逐一 rename。任何一個 copy 失敗(ENOSPC…)就把 staged 檔全清掉 ——
    // 不留混版(gd 新、gd-daemon 舊),也不留半截 .new 在安裝目錄裡。
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    for name in ["gd", "gd-daemon"] {
        let src = plan.release_dir.join(name);
        let dst = plan.install_dir.join(name);
        if !src.is_file() {
            discard_staged(&staged);
            anyhow::bail!("build did not produce {}", src.display());
        }
        match stage_binary(&src, &dst) {
            Ok(pair) => staged.push(pair),
            Err(e) => {
                discard_staged(&staged);
                return Err(e).with_context(|| format!("failed to install {name}"));
            }
        }
    }
    for (i, (new, dst)) in staged.iter().enumerate() {
        if let Err(e) = fs::rename(new, dst) {
            discard_staged(&staged[i..]);
            return Err(e).with_context(|| format!("rename over {}", dst.display()));
        }
    }
    for name in ["gd", "gd-daemon"] {
        eprintln!("updated {}", plan.install_dir.join(name).display());
    }

    // Re-set capability (a fresh inode has none)
    let daemon_bin = plan.install_dir.join("gd-daemon");
    let cap = Command::new("sudo")
        .args(["setcap", "cap_sys_admin,cap_dac_read_search+ep"])
        .arg(&daemon_bin)
        .status();
    match cap {
        Ok(s) if s.success() => {}
        _ => eprintln!(
            "warning: setcap failed. Run: sudo setcap cap_sys_admin,cap_dac_read_search+ep {}",
            daemon_bin.display()
        ),
    }

    // Refresh the systemd unit (resource limits travel with the binary, and
    // ExecStart must point at wherever we just installed)
    let home = dirs::home_dir().context("cannot determine home directory")?;
    match crate::commands::setup::install_service_unit(&home, Some(&daemon_bin)) {
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

/// 把 `src` copy 成 `<dst>.new`,回傳 (staged, 真正的 dst)。`gd --update` 幾乎
/// 都是由已安裝的 gd 自己跑的,而 `fs::copy` 覆寫一個正在執行的映像會拿到
/// ETXTBSY(26);之後用 rename 蓋過 `dst` —— rename 是原子的、對執行中的映像
/// 合法(process 繼續用舊 inode),而且 `dst` 從頭到尾都存在,不會有「gd 剛好
/// 不見」的空窗。`dst` 若是 symlink(或位在 symlink 目錄下)就寫到它真正指向
/// 的檔案,不要把 symlink 本身換成實體檔。copy 失敗時自己的 .new 會被清掉。
fn stage_binary(src: &Path, dst: &Path) -> Result<(PathBuf, PathBuf)> {
    let real = fs::canonicalize(dst).unwrap_or_else(|_| dst.to_path_buf());
    let staged = real.with_extension("new");
    if let Err(e) = fs::copy(src, &staged) {
        let _ = fs::remove_file(&staged);
        return Err(e).with_context(|| format!("copy to {}", staged.display()));
    }
    Ok((staged, real))
}

fn discard_staged(staged: &[(PathBuf, PathBuf)]) {
    for (new, _) in staged {
        let _ = fs::remove_file(new);
    }
}

fn find_release_dir(root: &Path) -> Result<PathBuf> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(root)
        .output()
        .context("cargo metadata failed")?;

    if !output.status.success() {
        anyhow::bail!(
            "cargo metadata failed in {}:\n{}",
            root.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("parse cargo metadata")?;

    let target = json["target_directory"]
        .as_str()
        .context("no target_directory in metadata")?;

    Ok(PathBuf::from(target).join("release"))
}
