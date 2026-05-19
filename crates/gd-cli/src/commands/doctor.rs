use gd_core::db::KeyStore;

pub fn run(store: &KeyStore) {
    eprintln!("links: {}", store.link_count());
    eprintln!("history: {} directories", store.history_count());

    // Check fd availability
    let has_fd = std::process::Command::new("fd")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());

    if has_fd {
        eprintln!("scanner: fd (fast)");
    } else {
        eprintln!("scanner: find (fallback — install fd for better performance)");
    }

    if let Ok(shell) = std::env::var("SHELL") {
        let name = if shell.ends_with("/bash") {
            "bash"
        } else if shell.ends_with("/zsh") {
            "zsh"
        } else if shell.ends_with("/fish") {
            "fish"
        } else {
            "unknown"
        };

        if name != "unknown" {
            eprintln!("shell: {name}");
            eprintln!("  hint: ensure eval \"$(gd init {name})\" is in your shell config");
        }
    }

    eprintln!("all checks passed.");
}
