use anyhow::Result;
use gd_core::db::KeyStore;
use gd_core::path::display_with_tilde;

pub fn run(store: &KeyStore, json: bool) -> Result<()> {
    if json {
        let j = store.export_json()?;
        println!("{j}");
        return Ok(());
    }

    let links = store.list_links();
    if links.is_empty() {
        eprintln!("no links.");
    } else {
        eprintln!("links:");
        for (alias, path) in links {
            let status = if path.exists() { "" } else { " (missing)" };
            eprintln!("  {alias} → {}{status}", display_with_tilde(path));
        }
    }

    let boosts = store.list_boosts();
    if !boosts.is_empty() {
        eprintln!("boosts:");
        for (path, weight) in boosts {
            eprintln!("  {} (×{weight})", display_with_tilde(path));
        }
    }

    eprintln!("{} directories in history.", store.history_count());

    Ok(())
}
