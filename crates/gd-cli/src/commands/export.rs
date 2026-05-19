use anyhow::Result;
use gd_core::db::KeyStore;

pub fn run(store: &KeyStore) -> Result<()> {
    let json = store.export_json()?;
    println!("{json}");
    Ok(())
}
