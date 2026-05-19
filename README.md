<div align="center">

# gd

**g**o **d**ir — a modern `cd`.

`gd >= cd`

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Shell: zsh | bash | fish | nu | pwsh](https://img.shields.io/badge/shell-zsh%20%7C%20bash%20%7C%20fish%20%7C%20nu%20%7C%20pwsh-green.svg)](#shell-support)

[English](README.md) | [繁體中文](README.zh-TW.md)

Type a name, land in the right directory. No full paths, no mental overhead.

</div>

---

## Why gd?

| | `cd` | `gd` |
|---|---|---|
| Local dirs | `cd src` | `gd src` |
| Go home | `cd` | `gd` |
| Previous dir | `cd -` | `gd -` |
| Full path | `cd /tmp` | `gd /tmp` |
| Fuzzy search | - | `gd conf` |
| History ranking | - | `gd proj` (remembers your picks) |
| Shortcuts | - | `gd link k ~/code/kernel` |
| Boost dirs | - | `gd boost ~/work` |

Everything `cd` does, plus smart search when you need it.

## Quick start

```bash
curl -sSL https://raw.githubusercontent.com/123hi123/gd/main/install.sh | bash
```

<details>
<summary>Manual install</summary>

```bash
git clone https://github.com/123hi123/gd.git && cd gd
cargo install --path crates/gd-cli
cargo install --path crates/gd-daemon
gd setup
```

</details>

`gd setup` handles everything:

- Install the systemd daemon service
- Set `CAP_SYS_ADMIN` on the daemon binary
- Add the shell hook to your rc file
- Ask if you want `alias cd=gd` (recommended)

## Usage

```bash
gd                  # go home
gd src              # local ./src/ exists? jump instantly
gd config           # no local match? fuzzy-search, pick from TUI
gd proj             # picked before? ranked first — always
gd /tmp             # full path? jump directly
gd ../lib           # relative path? jump directly
gd -                # previous directory
```

**The rule**: argument contains `/` = path mode (jump or fail). No `/` = search mode.

## Ranking

| Priority | Source | Description |
|---|---|---|
| 1 | **Link** | `gd link editor ~/code/editor` — permanent shortcut |
| 2 | **Selected** | Dirs you've picked via gd, always above unselected |
| 3 | **Visited** | Dirs you've `cd`'d into, ranked by recency |
| 4 | **Index** | Filesystem scan by background daemon |

> **Selected once > never selected**, regardless of match quality.

## Commands

```
gd <query>              search and jump
gd link <alias> <path>  create shortcut
gd unlink <alias>       remove shortcut
gd boost [path]         boost ranking (default: cwd, 5x)
gd unboost <path>       remove boost
gd list                 show links, boosts, stats
gd clean                remove dead entries
gd export               dump database as JSON
gd doctor               check installation health
gd setup                install daemon + hook + cd alias
gd update               rebuild and restart (developers)
```

## Architecture

```
                    +-----------+
  gd <query> ----→ | gd (CLI)  | ----→ print path → shell cd
                    +-----------+
                         |
                    read index + db
                         |
                    +-----------+
                    | gd-daemon | ← fanotify filesystem watcher
                    +-----------+
                         |
                    ~/.local/share/gd/
                    ├── index     (directory list)
                    └── db.json   (links + history + boosts)
```

**gd-daemon** uses Linux [fanotify](https://man7.org/linux/man-pages/man7/fanotify.7.html) to watch the filesystem in real-time, keeping the index fresh without periodic `find` scans.

| | |
|---|---|
| Service | `~/.config/systemd/user/gd-daemon.service` |
| Capability | `CAP_SYS_ADMIN` (fanotify) |
| RAM (idle) | ~2 MB |
| RAM (rescan) | ~143 MB |
| Query latency | < 25 ms |

## Shell support

zsh, bash, fish, nushell, powershell — auto-detected by `gd setup`.

## License

MIT
