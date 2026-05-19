# gd

**g**o **d**ir — a modern `cd`.

> `gd >= cd`

Type a name, land in the right directory. No full paths, no mental overhead.

## Install

```bash
curl -sSL https://raw.githubusercontent.com/123hi123/gd/main/install.sh | bash
```

Or manually:

```bash
git clone https://github.com/123hi123/gd.git && cd gd
cargo install --path crates/gd-cli
cargo install --path crates/gd-daemon
gd setup
```

`gd setup` will:
- Install the systemd daemon service
- Set `CAP_SYS_ADMIN` on the daemon binary
- Add the shell hook to your rc file (`.zshrc`, `.bashrc`, etc.)
- Ask if you want `alias cd=gd` (recommended)

## How it works

```bash
gd                  # no args → go home (like cd)
gd src              # local ./src/ exists → jump there instantly
gd config           # no local match → fuzzy-search filesystem, pick from TUI
gd proj             # picked before? it remembers — selected dirs always rank first
gd /tmp             # full path → jump directly (like cd /tmp)
gd ../lib           # relative path → jump directly (like cd ../lib)
gd -                # go to previous directory (like cd -)
```

If the argument contains `/`, gd treats it as a path: jump if it exists, fail if it doesn't — no search fallback. Without `/`, it's a search query.

## Ranking

1. **Links** — `gd link editor ~/code/editor` creates a permanent shortcut
2. **Selected** — any dir you've picked via gd before, always above unselected
3. **Visited** — dirs you've `cd`'d into, ranked by recency
4. **Index** — filesystem scan by the background daemon, lowest priority

The key rule: **selected once > never selected**, regardless of match quality.

## Commands

| Command | Description |
|---|---|
| `gd <query>` | Jump to a directory by name |
| `gd link <alias> <path>` | Create a named shortcut |
| `gd unlink <alias>` | Remove a shortcut |
| `gd boost [path]` | Boost a directory's ranking (default: cwd, 5x) |
| `gd unboost <path>` | Remove a boost |
| `gd list` | Show links, boosts, and history stats |
| `gd clean` | Remove entries pointing to dead paths |
| `gd export` | Dump database as JSON |
| `gd doctor` | Check installation health |
| `gd setup` | Install daemon + shell hook + optional cd alias |
| `gd update` | Rebuild and restart daemon (for developers) |

## Architecture

```
gd (CLI)        search index + TUI picker + shell hook
gd-daemon       fanotify filesystem watcher + index builder
```

**gd-daemon** watches the filesystem via Linux's [fanotify](https://man7.org/linux/man-pages/man7/fanotify.7.html) API, building an index of all directories. This lets `gd` search instantly without running `find` or `fd` on every query.

- Runs as a systemd user service (`~/.config/systemd/user/gd-daemon.service`)
- Requires `CAP_SYS_ADMIN` for fanotify access (set automatically by `gd setup`)
- Index stored at `~/.local/share/gd/index` (plain text, one path per line)
- Peak RAM during rescan: ~143MB / idle: ~2MB
- Selection history: `~/.local/share/gd/db.json`

## Shell support

zsh, bash, fish, nushell, powershell. Auto-detected by `gd setup`.

## License

MIT
