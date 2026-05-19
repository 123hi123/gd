<div align="center">

<img src="assets/logo.png" alt="gd logo" width="120">

# gd

**g**o **d**ir — a modern `cd`.

`gd >= cd`

[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Rust](https://img.shields.io/badge/rust-1.70%2B-orange.svg)](https://www.rust-lang.org/)
[![Shell: zsh | bash | fish | nu | pwsh](https://img.shields.io/badge/shell-zsh%20%7C%20bash%20%7C%20fish%20%7C%20nu%20%7C%20pwsh-green.svg)](#shell-support)

[English](README.md) | [繁體中文](README.zh-TW.md)

Type a name, land in the right directory. No full paths, no mental overhead.

<img src="assets/concept.png" alt="gd concept — type a name, land in the right place" width="600">

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

## Use cases

**Mobile SSH + AI coding**

You're on a phone, SSH'd into a VPS. Typing `/home/deploy/projects/myapp-backend/src` on a touch keyboard is miserable. With gd:

```bash
gd myapp        # jump — you remember the name, not the path
claude           # launch Claude Code and start coding
```

You have a vague memory of the name? Good enough. gd finds it. You used it before? It's already at the top.

**Deep project trees**

Monorepo with 200 packages, microservices spread across `/opt`, `/srv`, `/home`. You don't maintain a mental map — gd does it for you:

```bash
gd auth-service  # don't care where it lives
gd payments      # picked it last week? still ranked first
```

**Ditch the file manager**

You're on your local machine, no SSH. You open a terminal — and never leave it. Instead of clicking through Nautilus/Dolphin/Thunar to find that folder, just type the name:

```bash
gd Downloads    # no more ~/Downloads in the address bar
gd wallpapers   # buried in ~/Pictures/2024/wallpapers? don't care
gd taxes        # ~/Documents/finance/2025/taxes — gd knows
```

Move, copy, preview — all from the terminal:

```bash
gd projects     # jump to your project folder
ls              # see what's there
gd taxes        # jump to taxes, grab a file
cp report.pdf ~/Desktop/
gd projects     # back in one command
```

Once you alias `cd=gd`, your terminal *becomes* the file manager. Every directory you visit is remembered and ranked — the more you use it, the fewer keystrokes you need.

**Server admin**

Jumping between `/etc/nginx`, `/var/log/app`, `/opt/services/monitoring`:

```bash
gd link ng /etc/nginx
gd link logs /var/log/app
gd ng           # instant
```

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

<img src="assets/banner.png" alt="gd architecture — filesystem constellation" width="700">

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

**gd-daemon** uses Linux [fanotify](https://man7.org/linux/man-pages/man7/fanotify.7.html) to watch the filesystem in real-time. Directory creates, deletes, and moves are tracked incrementally — no periodic `find` scans.

| | |
|---|---|
| Service | `~/.config/systemd/user/gd-daemon.service` |
| Capability | `CAP_SYS_ADMIN` + `CAP_DAC_READ_SEARCH` |
| RAM | ~45 MB |
| Query latency | < 25 ms |

> 1 hour uptime, 7 seconds CPU — event-driven, not polling.
>
> <img src="assets/daemon-health.png" alt="daemon health: 1h uptime, 7s CPU" width="450">

## Shell support

zsh, bash, fish, nushell, powershell — auto-detected by `gd setup`.

## License

MIT
