mod fan;
mod scanner;

use anyhow::{Context, Result};
use gd_core::index::PathIndex;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

/// fanotify 不可用時的降級節奏。舊版是 120 秒一次「catchup」— 但 catchup
/// 本身就是全樹走訪(mtime 無法剪枝:新目錄只改直接父目錄的 mtime),等於
/// 每 2 分鐘 readdir 整個 $HOME;cache 冷掉時是數十秒的隨機讀,長期跟前景
/// 搶 I/O 與 page cache。放緩到 30 分鐘,並靠 systemd 的 idle 排程等級
/// (見 gd-daemon.service)確保掃描只用真正閒置的資源。
/// 沒有定期 full rescan:catchup 負責發現新目錄,死路徑由查詢端的
/// lazy 修正(retire_missing)+ 手動 `gd clean` 收拾,不需要定期掃屍。
const CATCHUP_INTERVAL_SECS: u64 = 30 * 60;
/// 降級後重試 fanotify 的間隔:成本只有兩個 syscall,成功就切回事件驅動。
const FANOTIFY_RETRY_SECS: u64 = 30 * 60;
/// 佇列溢位後的補掃,兩次之間的最小間隔。事件洪水常一波接一波(長時間的
/// 大型建置就是連環 burst),補掃又是全樹走訪 — 5 分鐘的下限保證最壞情況
/// 也不會退化成高頻掃;5 分鐘內的索引空窗由 shell hook(你走進去就入庫)
/// 與查詢端 lazy 修正兜底。
const OVERFLOW_CATCHUP_MIN_SECS: u64 = 5 * 60;
/// 溢位補掃的強制上限:補掃原本只在「事件流安靜」(poll timeout)時觸發,
/// 但最需要補掃的事件洪水期間恰恰沒有安靜空檔 → 補掃會被無限延後。
/// 標記起來超過這個時間就不等安靜了,直接補掃。
const OVERFLOW_FORCE_CATCHUP_SECS: u64 = 10 * 60;
/// 停機不足這個秒數就跳過啟動 catchup:`gd --update` 那種幾秒的重啟不值得
/// 一次全樹走訪(這麼短的空窗內的變更,靠 shell hook 與 lazy 修正即可)。
const STARTUP_CATCHUP_MIN_DOWNTIME_SECS: u64 = 60;

fn is_excluded(path: &Path) -> bool {
    path.components().any(|c| {
        if let std::path::Component::Normal(name) = c {
            if let Some(s) = name.to_str() {
                return scanner::EXCLUDE_NAMES.contains(&s);
            }
        }
        false
    })
}

/// fanotify 不可用時 daemon 的行為(`gd config daemon.fallback`):
/// poll = 低頻背景補掃(預設);off = 完全不掃描,索引只靠 shell hook。
fn fallback_mode(data_dir: &Path) -> String {
    gd_core::db::KeyStore::open(Some(data_dir))
        .ok()
        .and_then(|s| s.get_setting("daemon.fallback"))
        .unwrap_or_default()
}

/// 嘗試建立 fanotify watch(init + filesystem mark)。失敗回傳原因,
/// 由呼叫端決定降級或重試 — 不再是致命錯誤,否則沒 setcap 的機器會被
/// systemd 的 Restart=on-failure 拖進 5 秒一次的重啟迴圈。
fn try_fanotify(home: &Path) -> std::io::Result<i32> {
    let fd = fan::init()?;
    match fan::mark_filesystem(fd, home) {
        Ok(()) => Ok(fd),
        Err(e) => {
            unsafe { libc::close(fd) };
            Err(e)
        }
    }
}

/// 把 fanotify 失敗的 errno 翻成人話,讓 journal 直接可診斷。
fn describe_fanotify_error(e: &std::io::Error) -> String {
    match e.raw_os_error() {
        Some(libc::EXDEV) => format!(
            "{e} — btrfs subvolumes report inconsistent fsids, so the kernel \
             rejects FID-mode filesystem marks (known limitation)"
        ),
        Some(libc::EPERM) | Some(libc::EACCES) => format!(
            "{e} — missing capabilities. Run:\n  sudo setcap \
             cap_sys_admin,cap_dac_read_search+ep $(which gd-daemon)"
        ),
        _ => e.to_string(),
    }
}

fn main() -> Result<()> {
    // flag::register 只會把旗標設成 true,所以旗標語意必須是「收到終止訊號」。
    // 若寫成「還在執行中」(初始 true),handler 會退化成 no-op,卻又蓋掉 SIGTERM
    // 的預設終止行為 → 進程完全免疫 SIGTERM,只能等 systemd 補 SIGKILL。
    let term = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&term))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&term))?;

    let home = dirs::home_dir().context("cannot determine home directory")?;
    let data_dir = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("gd");
    std::fs::create_dir_all(&data_dir)?;

    let pid_file = data_dir.join("daemon.pid");
    let timestamp_file = data_dir.join("daemon.timestamp");

    std::fs::write(&pid_file, std::process::id().to_string())?;

    unsafe { libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) };

    let mount_fd = fan::open_mount_fd(&home)
        .context("cannot open home directory for mount fd")?;

    let fallback = fallback_mode(&data_dir);
    let fallback_off = fallback == "off";

    // btrfs 子卷的 fsid 不一致,核心會用 EXDEV 拒絕 FID 模式的 filesystem
    // mark(FID 模式又不允許 mount mark,dirent 事件必須 FID 模式,所以
    // 沒有第二條路)→ 這類機器降級,之後定期重試。
    let mut fan_fd = match try_fanotify(&home) {
        Ok(fd) => Some(fd),
        Err(e) => {
            eprintln!(
                "gd-daemon: fanotify unavailable: {}",
                describe_fanotify_error(&e)
            );
            eprintln!(
                "gd-daemon: degrading to '{}' mode; will retry fanotify every {} min \
                 (see: gd config daemon.fallback)",
                if fallback_off { "off" } else { "poll" },
                FANOTIFY_RETRY_SECS / 60
            );
            None
        }
    };

    // 一決定模式就先寫 daemon.mode:gd setup 裝完要立刻回報實際狀態、
    // gd doctor 隨時要能查,不能等首次建索引(可能數十秒)之後才有得讀。
    write_mode(
        &data_dir,
        match (fan_fd.is_some(), fallback_off) {
            (true, _) => "fanotify",
            (false, true) => "off",
            (false, false) => "poll",
        },
    );

    let index = PathIndex::open(&data_dir);

    // 任何一次掃描被終止訊號打斷就設起來,收尾時據此決定時間戳怎麼寫
    // (見 write_timestamp_incomplete)。掃描現在可中斷了,所以「中斷」
    // 必須是一個會被記住的狀態,否則索引缺口會被乾淨的時間戳掩蓋。
    let scan_incomplete = AtomicBool::new(false);

    // daemon.fallback=off 只約束「降級狀態」:fanotify 正常時,啟動的
    // bootstrap/catchup 是事件驅動模式的一部分(一次性、有界),照跑。
    let no_scan = fallback_off && fan_fd.is_none();

    // bootstrap 排在 no_scan 之前:daemon.fallback=off 管的是「持續的背景
    // 掃描」,不是「一次性的初始建索引」— 沒有索引的 gd 根本不能用。
    if !index.has_data() {
        eprintln!("gd-daemon: no index, scanning {}...", home.display());
        let r = scanner::full_scan(&home, &index, &term)?;
        if r.aborted {
            scan_incomplete.store(true, Ordering::Relaxed);
            eprintln!("gd-daemon: scan interrupted by shutdown signal.");
        } else {
            eprintln!("gd-daemon: indexed {} dirs.", r.count);
        }
    } else if no_scan {
        eprintln!(
            "gd-daemon: daemon.fallback=off — {} dirs indexed, skipping background scan.",
            index.len()
        );
    } else if let Some(ts) = read_timestamp(&timestamp_file) {
        // 上次乾淨關閉:補上停機期間新增的目錄 — 但短暫重啟(gd --update)
        // 不值得為十幾秒的空窗走訪整棵樹。
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        // ts 在未來(時鐘倒退、VM snapshot 還原、RTC 沒電)→ 時間戳不可信,
        // 算出來的 downtime 會是 0 而錯誤地跳過 catchup,一律照掃。
        let clock_skew = ts > now;
        let downtime = now.saturating_sub(ts);
        if !clock_skew && downtime < STARTUP_CATCHUP_MIN_DOWNTIME_SECS {
            eprintln!("gd-daemon: downtime {downtime}s, skipping catchup.");
        } else {
            if clock_skew {
                eprintln!(
                    "gd-daemon: last-shutdown timestamp is {}s in the future \
                     (clock went backwards); catching up anyway.",
                    ts - now
                );
            }
            eprintln!("gd-daemon: catching up since last shutdown...");
            match scanner::catchup_scan(&home, &index, &term) {
                Ok(r) if r.aborted => {
                    scan_incomplete.store(true, Ordering::Relaxed);
                    eprintln!("gd-daemon: scan interrupted by shutdown signal.")
                }
                Ok(r) => eprintln!("gd-daemon: added {} new dirs.", r.count),
                Err(e) => {
                    eprintln!("gd-daemon: catchup failed ({e}), doing full scan...");
                    let r = scanner::full_scan(&home, &index, &term)?;
                    if r.aborted {
                        scan_incomplete.store(true, Ordering::Relaxed);
                        eprintln!("gd-daemon: scan interrupted by shutdown signal.");
                    } else {
                        eprintln!("gd-daemon: indexed {} dirs.", r.count);
                    }
                }
            }
        }
    } else {
        eprintln!("gd-daemon: index exists.");
    }

    unsafe { libc::malloc_trim(0) };

    // 主迴圈:事件驅動優先;降級時走 fallback_loop,重試成功就切回來。
    while !term.load(Ordering::Relaxed) {
        match fan_fd {
            Some(fd) => {
                write_mode(&data_dir, "fanotify");
                event_loop(&term, &scan_incomplete, fd, mount_fd, &home, &index);
                break; // event_loop 只在收到終止訊號時返回
            }
            None => {
                write_mode(&data_dir, if fallback_off { "off" } else { "poll" });
                match fallback_loop(&term, &scan_incomplete, &home, &index, fallback_off) {
                    Some(fd) => {
                        // mark 已掛上、事件開始排隊,再補一次 catchup 蓋住
                        // 輪詢空窗期的變更 — 順序不能反,否則有縫。
                        eprintln!("gd-daemon: fanotify recovered, switching to event mode.");
                        // 模式先寫:catchup 可能跑數十秒,這期間 gd doctor
                        // 讀到的必須是「已經是 fanotify」,不能還停在 poll。
                        write_mode(&data_dir, "fanotify");
                        match scanner::catchup_scan(&home, &index, &term) {
                            Ok(r) if r.aborted => {
                                scan_incomplete.store(true, Ordering::Relaxed);
                                eprintln!("gd-daemon: scan interrupted by shutdown signal.")
                            }
                            Ok(r) if r.count > 0 => {
                                eprintln!("gd-daemon: catchup added {} dirs.", r.count)
                            }
                            Ok(_) => {}
                            Err(e) => eprintln!("gd-daemon: catchup error: {e}"),
                        }
                        fan_fd = Some(fd);
                    }
                    None => break, // 收到終止訊號
                }
            }
        }
    }

    if let Err(e) = index.flush() {
        eprintln!("gd-daemon: final flush error: {e}");
    }
    if scan_incomplete.load(Ordering::Relaxed) {
        write_timestamp_incomplete(&timestamp_file);
    } else {
        write_timestamp(&timestamp_file);
    }
    let _ = std::fs::remove_file(&pid_file);
    if let Some(fd) = fan_fd {
        unsafe { libc::close(fd) };
    }
    unsafe { libc::close(mount_fd) };
    eprintln!("gd-daemon: stopped.");
    Ok(())
}

/// fanotify 事件迴圈。只在收到終止訊號時返回。
fn event_loop(
    term: &AtomicBool,
    scan_incomplete: &AtomicBool,
    fan_fd: i32,
    mount_fd: i32,
    home: &Path,
    index: &PathIndex,
) {
    eprintln!("gd-daemon: {} dirs indexed. Watching.", index.len());

    let mut last_flush = Instant::now();
    // 佇列溢位 = 有事件被 kernel 丟掉(npm install、rm -rf 大樹這種洪水;
    // 排除清單只擋入庫,擋不住事件送達)。這是事件模式唯一需要 rescan 的
    // 常態情境:標記起來,等事件流安靜(poll timeout)再補掃(限流;
    // 洪水久久不停就到 OVERFLOW_FORCE_CATCHUP_SECS 硬上)。
    // 漏掉的刪除事件不用管 — 查詢端的 lazy 修正會收拾。
    let mut overflow_pending = false;
    let mut overflow_since: Option<Instant> = None;
    let mut last_overflow_catchup: Option<Instant> = None;

    while !term.load(Ordering::Relaxed) {
        // 這輪是不是 poll 逾時(= 事件流安靜)。溢位補掃優先挑這種時候跑。
        let mut idle = false;

        match fan::poll_events(fan_fd, 2000) {
            Ok(true) => {
                let (events, overflow) = fan::read_events(fan_fd, mount_fd);
                if overflow && !overflow_pending {
                    overflow_pending = true;
                    overflow_since = Some(Instant::now());
                    eprintln!(
                        "gd-daemon: fanotify queue overflow — events lost, \
                         catchup scheduled after the burst settles."
                    );
                }
                for event in events {
                    match event {
                        fan::DirEvent::Created(path) => {
                            // FAN_MARK_FILESYSTEM 的範圍是整個 superblock:
                            // 若 / 和 /home 同一個檔案系統,會收到家目錄以外
                            // 的事件 — 一律忽略,索引只收 $HOME 底下的路徑。
                            if path.starts_with(home) && !is_excluded(&path) {
                                index.add(path);
                            }
                        }
                        fan::DirEvent::Deleted(path) => {
                            if path.starts_with(home) {
                                // 用 remove_subtree 而不是 remove:rmdir 要求目錄
                                // 是空的,所以索引裡任何殘留的子項都是漏收的
                                // delete 事件(`rm -rf` 時子項事件要靠父目錄的
                                // file handle 還原路徑,父目錄先被刪掉就解不開
                                // → 靜默丟棄)。刪父時順手清子樹把這個洞補起來。
                                // 成本是 PK index 的一次範圍掃,不是全表掃
                                // (見 index.rs 的 subtree_bounds)。
                                index.remove_subtree(&path);
                            }
                        }
                        fan::DirEvent::Renamed(old, new) => {
                            match (old.starts_with(home), new.starts_with(home)) {
                                (true, true) => {
                                    if is_excluded(&new) {
                                        // Moved into an excluded dir (e.g. node_modules):
                                        // no per-child delete events arrive, so drop the
                                        // old subtree by prefix.
                                        index.remove_subtree(&old);
                                    } else if index.rename(&old, &new) == 0 {
                                        // Source wasn't indexed (e.g. moved in from an
                                        // excluded/unwatched location): index the new dir
                                        // *and* its subtree — rename is atomic, so the
                                        // kernel sends no CREATE for the children.
                                        scanner::scan_subtree(&new, index);
                                    }
                                }
                                // 搬出家目錄 = 對索引而言整棵消失。
                                (true, false) => index.remove_subtree(&old),
                                // 搬進家目錄 = 新目錄(連同整棵子樹,
                                // 原子 rename 不會補送子目錄的 CREATE)。
                                (false, true) => {
                                    if !is_excluded(&new) {
                                        scanner::scan_subtree(&new, index);
                                    }
                                }
                                (false, false) => {}
                            }
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
                idle = true;
            }
            Err(_) => {}
        }

        // 補溢位期間漏掉的新目錄。優先等事件流安靜(idle)再跑,但洪水
        // 可能一直不給空檔 — 標記超過 OVERFLOW_FORCE_CATCHUP_SECS 就硬上,
        // 否則補掃會被無限延後。兩次補掃之間仍受最小間隔節流。
        let forced = overflow_since
            .map_or(false, |t| t.elapsed().as_secs() >= OVERFLOW_FORCE_CATCHUP_SECS);
        if overflow_pending
            && (idle || forced)
            && last_overflow_catchup
                .map_or(true, |t| t.elapsed().as_secs() >= OVERFLOW_CATCHUP_MIN_SECS)
        {
            match scanner::catchup_scan(home, index, term) {
                Ok(r) if r.aborted => {
                    scan_incomplete.store(true, Ordering::Relaxed);
                    eprintln!("gd-daemon: scan interrupted by shutdown signal.")
                }
                Ok(r) => {
                    eprintln!("gd-daemon: overflow catchup done, {} dirs added.", r.count);
                }
                Err(e) => eprintln!("gd-daemon: overflow catchup error: {e}"),
            }
            overflow_pending = false;
            overflow_since = None;
            last_overflow_catchup = Some(Instant::now());
            unsafe { libc::malloc_trim(0) };
        }
    }
}

/// 目前的監看模式,寫給 `gd doctor` 讀(fanotify / poll / off)。
fn write_mode(data_dir: &Path, mode: &str) {
    let _ = std::fs::write(data_dir.join("daemon.mode"), mode);
}

/// 降級迴圈。`off` = 完全不掃描,只定期重試 fanotify;否則低頻輪詢:
/// 每 CATCHUP_INTERVAL_SECS 一次 catchup 發現新目錄。沒有定期 full
/// rescan — 死路徑由查詢端 lazy 退場(retire_missing),徹底清掃交給
/// 手動 `gd clean`。回傳 Some(fd) = fanotify 重試成功;None = 收到終止訊號。
fn fallback_loop(
    term: &AtomicBool,
    scan_incomplete: &AtomicBool,
    home: &Path,
    index: &PathIndex,
    off: bool,
) -> Option<i32> {
    if off {
        eprintln!(
            "gd-daemon: {} dirs indexed. Fallback 'off': no background scanning; \
             the index grows only from your shell visits.",
            index.len()
        );
    } else {
        eprintln!(
            "gd-daemon: {} dirs indexed. Low-frequency polling (catchup every {} min; \
             deleted paths retire lazily at query time).",
            index.len(),
            CATCHUP_INTERVAL_SECS / 60
        );
    }

    let mut last_catchup = Instant::now();
    let mut last_retry = Instant::now();

    while !term.load(Ordering::Relaxed) {
        // 2 秒一醒只為了 SIGTERM 反應速度;醒著什麼都不做,成本可忽略。
        std::thread::sleep(std::time::Duration::from_secs(2));

        if last_retry.elapsed().as_secs() >= FANOTIFY_RETRY_SECS {
            last_retry = Instant::now();
            if let Ok(fd) = try_fanotify(home) {
                return Some(fd);
            }
            // 失敗保持安靜:原因在啟動時已記錄過,btrfs 這類原因不會自己消失。
        }

        if off {
            continue;
        }

        if last_catchup.elapsed().as_secs() >= CATCHUP_INTERVAL_SECS {
            match scanner::catchup_scan(home, index, term) {
                Ok(r) if r.aborted => {
                    scan_incomplete.store(true, Ordering::Relaxed);
                    eprintln!("gd-daemon: scan interrupted by shutdown signal.")
                }
                Ok(r) if r.count > 0 => {
                    eprintln!("gd-daemon: catchup added {} dirs.", r.count);
                    if let Err(e) = index.flush() {
                        eprintln!("gd-daemon: flush error: {e}");
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("gd-daemon: catchup error: {e}"),
            }
            last_catchup = Instant::now();
            unsafe { libc::malloc_trim(0) };
        }
    }
    None
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

/// 掃描被終止訊號打斷時的收尾:時間戳寫 0,而不是「現在」。
///
/// 時間戳的語意是「索引在這個時刻是完整的」。掃描中途被打斷代表索引有
/// 缺口,此時若照常寫下現在時間,下次啟動算出的 downtime 會很小 →
/// 跳過 catchup(fanotify 模式又沒有任何定期補掃)→ 缺口永遠補不回來。
/// 寫 0 讓下次啟動必定 downtime 超標而跑一次 catchup。
/// 不能改成「刪掉時間戳檔」— 那會落進 `index exists` 分支,反而完全不掃。
fn write_timestamp_incomplete(path: &std::path::Path) {
    let _ = std::fs::write(path, "0");
}
