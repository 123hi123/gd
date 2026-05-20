# gd — smarter cd

A directory jumper that finds dirs by basename, ranked by selection history.

## Architecture

```
gd (CLI)        — search index + TUI picker + shell hook
gd-daemon       — fanotify filesystem watcher + index builder (CAP_SYS_ADMIN + CAP_DAC_READ_SEARCH)
```

## Key paths

- DB: `~/.local/share/gd/gd.db` (SQLite — unified index + history + links + boosts)
- Service: `~/.config/systemd/user/gd-daemon.service`

## Development workflow

**After any code change, run `gd update` in the project directory to deploy to the local system.**

`gd update` does: stop daemon → cargo build --release → copy binaries → setcap → restart daemon (no full rescan).

If `gd update` is not yet installed (first time), run manually:
```bash
systemctl --user stop gd-daemon
cargo build --release --all
cp -f target/release/gd ~/.cargo/bin/gd
cp -f target/release/gd-daemon ~/.cargo/bin/gd-daemon
sudo setcap cap_sys_admin,cap_dac_read_search+ep ~/.cargo/bin/gd-daemon
systemctl --user start gd-daemon
```

## Search priority (TUI ordering)

1. **Links** — `gd link <alias> <path>` manual bindings (score: MAX)
2. **History (selected)** — paths picked via gd before, ranked by selection count (score: 1000+)
3. **History (visited)** — paths cd'd into but never selected via gd (score: visits × decay)
4. **Index/scan** — from daemon's filesystem index, lowest priority (score: 0.1–0.5)

## Constraints

- Daemon RAM: ~15MB (SQLite-backed, no in-memory index)
- Query latency: <25ms
- Search matches **basename only**, not full path
- fanotify requires CAP_SYS_ADMIN + CAP_DAC_READ_SEARCH on gd-daemon binary
