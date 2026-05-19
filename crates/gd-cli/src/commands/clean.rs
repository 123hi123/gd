use anyhow::Result;
use gd_core::db::KeyStore;
use gd_core::path::display_with_tilde;

pub fn run(store: &mut KeyStore) -> Result<()> {
    let (removed_links, removed_history) = store.clean();

    if removed_links.is_empty() && removed_history.is_empty() {
        eprintln!("all paths are valid, nothing to clean.");
        return Ok(());
    }

    for (alias, path) in &removed_links {
        eprintln!("unlinked {alias} → {}", display_with_tilde(path));
    }
    for path in &removed_history {
        eprintln!("removed {}", display_with_tilde(path));
    }
    eprintln!(
        "{} link(s) + {} history path(s) removed.",
        removed_links.len(),
        removed_history.len()
    );

    store.save()?;
    Ok(())
}
