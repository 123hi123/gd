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
- Search matches **basename only**, not full path
- `in_index` means exactly one thing: **the daemon's index currently contains
  this path**. It is NOT a "this path is dead" flag — a row the shell hook just
  recorded is legitimately `in_index = 0` and alive. Never use it to decide
  whether a path still exists; stat it. (`gd clean` used to get this wrong and
  was consequently a no-op over 368k index rows.)
- fanotify requires CAP_SYS_ADMIN + CAP_DAC_READ_SEARCH on gd-daemon binary
- fanotify unavailable (e.g. btrfs subvolume home → EXDEV): daemon degrades to
  a catchup rescan every 30 min at idle CPU/IO priority and retries fanotify
  every 30 min. `gd config daemon.fallback off` disables scanning entirely.
  NEVER shorten these intervals: a "catchup" is a full-tree walk (mtime cannot
  prune the walk — a new dir only touches its direct parent's mtime), so
  frequent catchups = constant whole-$HOME readdir storms that fight the
  foreground for IO and page cache.
- No periodic full rescan anywhere. Dead paths retire lazily at query time
  (`retire_missing`: index-only rows deleted, history rows marked out-of-index;
  `gd clean` for a full sweep). Event-mode gaps are self-healing: FAN_Q_OVERFLOW
  triggers one catchup after the burst settles (≥5 min apart); startup catchup
  covers daemon downtime (skipped when downtime < 60 s, so `gd update` restarts
  don't walk the tree). Scans are rare, so they run fast (parallelism =
  min(cores, 8)) — the idle scheduling class in the unit is what keeps them
  invisible, not artificial slowness.
- Scans are interruptible and that has a consequence: a scan cut short by
  SIGTERM writes `daemon.timestamp = 0` instead of the real time, so the next
  start is guaranteed to run a catchup. Never "simplify" that back into an
  unconditional `write_timestamp()` — a clean-looking timestamp over a
  half-built index means the gap is never filled.
- Deleting a directory removes its **whole subtree** from the index, not just
  the one row. `rm -rf` loses the children's events (their paths are rebuilt
  from the parent's file handle, and the parent is usually already gone by the
  time the daemon drains the queue), so the parent's event has to clean up
  after them. Subtree predicates use range bounds (`path >= 'p/' AND
  path < 'p0'`), never `LIKE 'p/%'` — SQLite's case-insensitive LIKE cannot use
  the primary-key index and degrades to a full table scan per event.
