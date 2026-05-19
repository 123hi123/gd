use anyhow::{bail, Result};

const BASH_SCRIPT: &str = include_str!("../shell/bash.sh");
const ZSH_SCRIPT: &str = include_str!("../shell/zsh.sh");
const FISH_SCRIPT: &str = include_str!("../shell/fish.fish");
const NU_SCRIPT: &str = include_str!("../shell/nu.nu");
const PWSH_SCRIPT: &str = include_str!("../shell/pwsh.ps1");

pub fn run(shell: Option<&str>) -> Result<()> {
    let shell = match shell {
        Some(s) => s.to_string(),
        None => detect_shell(),
    };

    match shell.as_str() {
        "bash" => print!("{BASH_SCRIPT}"),
        "zsh" => print!("{ZSH_SCRIPT}"),
        "fish" => print!("{FISH_SCRIPT}"),
        "nu" | "nushell" => print!("{NU_SCRIPT}"),
        "powershell" | "pwsh" => print!("{PWSH_SCRIPT}"),
        other => bail!(
            "unknown shell '{other}'. Supported: bash, zsh, fish, nu, powershell\n\
             \n\
             Add to your shell config:\n\
             \n\
             bash:       eval \"$(gd init bash)\"\n\
             zsh:        eval \"$(gd init zsh)\"\n\
             fish:       gd init fish | source\n\
             nushell:    source (gd init nu)\n\
             powershell: Invoke-Expression (gd init powershell)"
        ),
    }

    Ok(())
}

fn detect_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if shell.ends_with("/bash") {
            return "bash".into();
        }
        if shell.ends_with("/zsh") {
            return "zsh".into();
        }
        if shell.ends_with("/fish") {
            return "fish".into();
        }
        if shell.contains("nu") {
            return "nu".into();
        }
    }

    if std::env::var("PSModulePath").is_ok() {
        return "powershell".into();
    }

    eprintln!(
        "could not detect shell. Usage:\n\
         \n\
         bash:       eval \"$(gd init bash)\"\n\
         zsh:        eval \"$(gd init zsh)\"\n\
         fish:       gd init fish | source\n\
         nushell:    source (gd init nu)\n\
         powershell: Invoke-Expression (gd init powershell)"
    );
    "unknown".into()
}
