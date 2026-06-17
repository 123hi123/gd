use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "gd",
    about = "A smarter cd — find directories by name, ranked by how often you pick them",
    version,
    arg_required_else_help = false
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Directory name keywords to search (space-separated, all must match)
    #[arg(value_name = "QUERY", conflicts_with = "command")]
    pub query: Vec<String>,

    /// Override data directory
    #[arg(long, global = true)]
    pub data_dir: Option<PathBuf>,

    /// Disable colors
    #[arg(long, global = true)]
    pub no_color: bool,

    /// JSON output
    #[arg(long, global = true)]
    pub json: bool,
}

#[derive(Subcommand)]
pub enum Command {
    /// Jump to a directory matching query (used by shell wrapper)
    Jump {
        /// Directory name keywords to search
        #[arg(required = true)]
        query: Vec<String>,
    },

    /// Record a cd visit (called by shell hook, not for users)
    #[command(hide = true)]
    Hook {
        /// The directory that was cd'd into
        path: PathBuf,
    },

    /// Bind a name to a directory — `gd link <name> <path>`
    ///
    /// Takes two arguments, in this order:
    ///   1. <name>  the shortcut you'll type later (e.g. `dl`)
    ///   2. <path>  the directory it should point at (e.g. ~/下載)
    ///
    /// Afterwards `gd <name>` jumps straight there, ranked top of the list.
    ///
    /// Example:
    ///   gd link dl ~/下載      # then `gd dl` → ~/下載
    #[command(verbatim_doc_comment)]
    Link {
        /// 1st arg — the shortcut name you'll type later (e.g. `dl`)
        #[arg(value_name = "name")]
        alias: String,
        /// 2nd arg — the directory that name points at (e.g. ~/下載)
        #[arg(value_name = "path")]
        path: PathBuf,
    },

    /// Remove a link
    Unlink {
        /// Alias to remove
        alias: String,
    },

    /// Get or set preferences (e.g. the picker layout)
    ///
    /// gd config                      show every setting and its value
    /// gd config layout               show one setting
    /// gd config layout traditional   switch the picker to the full-screen
    ///                                classic list (default: inline)
    #[command(verbatim_doc_comment)]
    Config {
        /// A setting name, optionally followed by a new value
        #[arg(value_name = "key/value")]
        args: Vec<String>,
    },

    /// List links and history stats
    List,

    /// Remove entries pointing to non-existent paths
    Clean,

    /// Export database as JSON to stdout
    Export,

    /// Print shell init script
    Init {
        /// Shell: bash | zsh | fish | nu | powershell
        shell: Option<String>,
    },

    /// Check installation health
    Doctor,

    /// Install systemd service and shell hook
    Setup,

    /// Rebuild and restart daemon without full rescan
    Update,

    /// Boost a directory and all paths below it
    Boost {
        /// Path to boost (defaults to current directory)
        path: Option<PathBuf>,
        /// Score multiplier (default: 5)
        #[arg(short, long, default_value = "5")]
        weight: f64,
    },

    /// Remove a boost
    Unboost {
        /// Path to unboost
        path: PathBuf,
    },
}
