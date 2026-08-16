use gd_core::index::PathIndex;
use jwalk::WalkDir;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

/// 掃描迴圈裡「檢查終止訊號 + 提交一批」的間隔(以走過的 entry 計)。
/// 2000 筆大約是毫秒級的一次 COMMIT,既讓前景查詢有機會插進來拿寫鎖,
/// 也讓 SIGTERM 在肉眼無感的時間內生效。
const BATCH_ENTRIES: usize = 2000;

/// 掃描結果。
pub struct ScanResult {
    pub count: usize,
    /// 收到終止訊號而提早收工 — 呼叫端據此決定要不要繼續做收尾動作。
    pub aborted: bool,
}

/// 掃描的平行度:快狠準 — 掃描本來就稀少(bootstrap、溢位補掃、降級模式
/// 30 分鐘一次),而 unit 的 idle CPU/IO 排程保證「系統閒著就全速跑、
/// 前景一動就讓路」,所以放心吃滿核心(上限 8),把冷 cache 的全樹走訪
/// 從幾十秒壓進幾秒,而不是細水長流地污染 page cache。
fn scan_threads() -> usize {
    std::thread::available_parallelism().map_or(2, |n| n.get().min(8))
}

/// 掃描與 fanotify 事件共用的排除清單(比對任一路徑元件名)。
/// 除了套件/建置產物,也排除 Steam、flatpak(~/.var)、瀏覽器 profile
/// 這類巨型且不會是跳轉目標的樹 — 降級輪詢時每省一棵都是實打實的 I/O。
pub const EXCLUDE_NAMES: &[&str] = &[
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
    ".var",
    ".steam",
    ".mozilla",
    ".thunderbird",
    ".wine",
];

pub fn full_scan(root: &Path, index: &PathIndex, term: &AtomicBool) -> std::io::Result<ScanResult> {
    index.mark_all_not_indexed();
    index.begin_bulk();
    let mut count = 0usize;
    let mut processed = 0usize;
    let mut aborted = false;

    for entry in WalkDir::new(root)
        .skip_hidden(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(scan_threads()))
        .process_read_dir(|_, _, _, entries| {
            entries.retain(|e| {
                if let Ok(entry) = e {
                    if let Some(name) = entry.file_name().to_str() {
                        if EXCLUDE_NAMES.contains(&name) {
                            return false;
                        }
                    }
                }
                true
            });
        })
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_dir() {
            index.add(entry.path().to_path_buf());
            count += 1;
        }

        // 分批提交:整趟走訪若包在一個交易裡,會從第一次寫入起獨佔寫鎖
        // 到掃完(實測數十秒),前景的 gd 查詢/記錄全被卡死甚至靜默丟棄。
        processed += 1;
        if processed % BATCH_ENTRIES == 0 {
            index.commit_batch();
            if term.load(Ordering::Relaxed) {
                aborted = true;
                break;
            }
        }
    }

    index.end_bulk();
    // 中斷時大量還活著的目錄還沒被重新標記成 in_index,
    // 此時 cleanup_stale 會把它們一次刪光 — 絕對不能跑。
    if !aborted {
        index.cleanup_stale();
    }
    Ok(ScanResult { count, aborted })
}

/// 補掃:全樹走訪,把索引裡缺席的目錄補進去。
///
/// 不做 per-dir stat。舊版用 mtime > since 過濾,但 mtime 無法剪枝
/// (新目錄只更新「直接父目錄」的 mtime,祖先不變,所以無論如何都得
/// 走完整棵樹),等於在全樹 readdir 之外、每個目錄還多付一次 statx,
/// 只為了省下便宜的 SQLite 查詢。現在改由 add_if_missing 的主鍵探測
/// 判斷新舊:已在索引的目錄一次寫入都不產生。
/// 刪除與改名的舊路徑不歸這裡管 — 查詢端碰到就 lazy 退場
/// (retire_missing),徹底清掃是手動 `gd clean` 的職責。
pub fn catchup_scan(
    root: &Path,
    index: &PathIndex,
    term: &AtomicBool,
) -> std::io::Result<ScanResult> {
    index.begin_bulk();
    let mut added = 0usize;
    let mut processed = 0usize;
    let mut aborted = false;

    for entry in WalkDir::new(root)
        .skip_hidden(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(scan_threads()))
        .process_read_dir(|_, _, _, entries| {
            entries.retain(|e| {
                if let Ok(entry) = e {
                    if let Some(name) = entry.file_name().to_str() {
                        if EXCLUDE_NAMES.contains(&name) {
                            return false;
                        }
                    }
                }
                true
            });
        })
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_dir() && index.add_if_missing(entry.path().to_path_buf()) {
            added += 1;
        }

        // 同 full_scan:分批提交壓低寫鎖持有時間,順便當中斷檢查點。
        // catchup 中斷是無害的 — 沒補到的目錄下次 catchup 再補。
        processed += 1;
        if processed % BATCH_ENTRIES == 0 {
            index.commit_batch();
            if term.load(Ordering::Relaxed) {
                aborted = true;
                break;
            }
        }
    }

    index.end_bulk();
    Ok(ScanResult {
        count: added,
        aborted,
    })
}

/// 索引 `root` 及其整棵子樹(rename 進來的目錄用)。rename 是原子操作,
/// kernel 不會補送子目錄的 CREATE 事件,所以搬進來的子樹要自己走一遍。
/// 有界且通常很小,不需要分批提交;但仍吃 EXCLUDE_NAMES 過濾。
pub fn scan_subtree(root: &Path, index: &PathIndex) -> usize {
    let mut count = 0usize;

    for entry in WalkDir::new(root)
        .skip_hidden(false)
        .parallelism(jwalk::Parallelism::RayonNewPool(scan_threads()))
        .process_read_dir(|_, _, _, entries| {
            entries.retain(|e| {
                if let Ok(entry) = e {
                    if let Some(name) = entry.file_name().to_str() {
                        if EXCLUDE_NAMES.contains(&name) {
                            return false;
                        }
                    }
                }
                true
            });
        })
        .into_iter()
        .flatten()
    {
        if entry.file_type().is_dir() {
            index.add(entry.path().to_path_buf());
            count += 1;
        }
    }

    count
}
