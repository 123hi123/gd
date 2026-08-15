use gd_core::index::PathIndex;
use jwalk::WalkDir;
use std::path::Path;

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

pub fn full_scan(root: &Path, index: &PathIndex) -> std::io::Result<usize> {
    index.mark_all_not_indexed();
    index.begin_bulk();
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

    index.end_bulk();
    index.cleanup_stale();
    Ok(count)
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
pub fn catchup_scan(root: &Path, index: &PathIndex) -> std::io::Result<usize> {
    index.begin_bulk();
    let mut added = 0usize;

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
    }

    index.end_bulk();
    Ok(added)
}
