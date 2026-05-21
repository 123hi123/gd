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

    /// Link an alias to a specific path
    Link {
        /// Short alias name
        alias: String,
        /// Full path to link
        path: PathBuf,
    },

    /// Remove a link
    Unlink {
        /// Alias to remove
        alias: String,
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
