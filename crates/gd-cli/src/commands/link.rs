use anyhow::Result;
use gd_core::db::KeyStore;
use gd_core::path::display_with_tilde;
use std::path::Path;

pub fn add(store: &mut KeyStore, alias: &str, path: &Path) -> Result<()> {
    store.add_link(alias, path)?;
    store.save()?;

    let canonical = std::fs::canonicalize(path)?;
    eprintln!("linked {alias} → {}", display_with_tilde(&canonical));
    Ok(())
}

pub fn remove(store: &mut KeyStore, alias: &str) -> Result<()> {
    store.remove_link(alias)?;
    store.save()?;
    eprintln!("unlinked '{alias}'");
    Ok(())
}
