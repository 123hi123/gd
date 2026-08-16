use anyhow::Result;
use gd_core::db::KeyStore;
use gd_core::path::display_with_tilde;

pub fn run(store: &mut KeyStore) -> Result<()> {
    let report = store.clean();

    if report.removed_links.is_empty() && report.removed_history.is_empty() && report.removed_index == 0
    {
        eprintln!("all paths are valid, nothing to clean.");
        return Ok(());
    }

    // 失效 link 和有歷史的路徑逐筆印:這些是使用者自己認得的東西,通常也很少。
    // 純索引列(daemon 掃出來的)可能上萬筆,只報總數,不然直接洗版。
    for (alias, path) in &report.removed_links {
        eprintln!("unlinked {alias} → {}", display_with_tilde(path));
    }
    for path in &report.removed_history {
        eprintln!("removed {}", display_with_tilde(path));
    }

    eprintln!(
        "scanned {} path(s); removed {} index-only, {} with history, {} link(s).",
        report.scanned,
        report.removed_index,
        report.removed_history.len(),
        report.removed_links.len()
    );

    store.save()?;
    Ok(())
}
