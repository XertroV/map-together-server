# Deploying map-together-server

Servers, tooling, and the idle-restart deploy flow. Quick reference:

```bash
just deploy            # build + upload + background idle-waiter (recommended)
just deploy-check      # per-host status
just deploy-logs       # journald logs
just restart-when-idle # local/dev: wait 0 conns, restart the dev unit
```

## Servers

| Host | SSH alias       | Address                    | Region |
|------|-----------------|----------------------------|--------|
| de   | `MapTogetherDe` | `map-together-de.xk.io` (134.122.76.190) | Fra1 |
| us   | `MapTogetherUs` | `map-together-us.xk.io` (144.126.218.228) | Sfo3 |
| au   | `MapTogetherAu` | `map-together-au.xk.io` (170.64.198.163) | Syd1 |

All three: `root`, key auth, binary at `/root/map-together-server`, config at
`/root/config.json` (loaded from CWD), TCP :19796. 1 vCPU / 1 GB boxes
(Ubuntu), fine for the current player counts.

Locally-built binaries run on them: our builds need at most `GLIBC_2.34`,
servers have glibc 2.38 (Ubuntu 23.10), and we only link system libssl3 /
libcrypto3 (both present). Verified 2026-08-30. If Ubuntu or rust ever bumps
past this, `deploy.py` surfaces the failure at first boot — check
`just deploy-logs` after any toolchain change.

## State of play (2026-08-30)

Servers were historically run via `bash /root/run.sh` — a loop-less script
that `mv`s `map-together-server-next` over `map-together-server` and execs
the binary — started from an interactive SSH session. That means: **no crash
restart, no logs, and the process dies if the session scope is reaped.**
`deploy.py setup` migrates them to a systemd unit (`map-together.service`)
with `Restart=always`, journald log capture, and a stale-binary promotion
hook. Check current migration status with `just deploy-check` (look for
`run.sh: N old-proc` and `unit:` lines).

## The idle-restart deploy flow

Room state is memory-only. Rooms expire ~30 min after their last player
leaves, so restarting when the server has **no players AND no rooms** is
provably lossless — anyone rejoining later couldn't have rejoined the old
room anyway.

The new-binary [stats] logging (5-min cadence) is what makes this precise:
the server logs `[stats] players connected: N across M rooms` and the deploy
tool waits for consecutive `0 across 0 rooms` samples before restarting.

```bash
just deploy                # default: stage binary + start idle-waiter per host
python3 deploy.py deploy --hosts de,us     # subset
python3 deploy.py deploy --restart         # kick everyone, deploy right now
python3 deploy.py deploy --zero-samples 3  # more margin (each sample = 5 min)
```

Flow per host:

1. `cargo build --release` (local).
2. Upload to `/root/map-together-server-next.part` then rename to
   `/root/map-together-server-next` (atomic; running server unaffected).
3. md5-verify the staged binary.
4. `nohup /root/mt-idle-wait.sh` — a background waiter, flock-guarded
   (`/run/lock/mt-idle-wait.lock`, only one ever runs per host):
   - watches journald for `[stats]` lines; after `--zero-samples` (default 2)
     consecutive `players connected: 0 across 0 rooms` samples → restarts the
     unit. Any players or live rooms reset the streak. 2 samples ≈ 10 min.
   - old binaries that don't log `[stats]` fall back to "zero **active**
     connections on :19796 for `--idle-mins` min" (default 10). "Active" =
     socket saw traffic within the idle window (`lastsnd`); the old binary
     never closes dead sockets, so scanner/zombie connections would otherwise
     block a restart forever.
5. On restart, the unit's `ExecStartPre` promotes the staged binary
   (`-next` → live, previous kept as `.prev`), then starts the new binary.
6. Verify: `just deploy-check` (shows unit state, binary md5 vs local,
   connections, last [stats] lines, waiter state).

`--restart` skips the waiter and restarts immediately (kicks players) — for
emergency fixes. Restarting when 0 players are connected but rooms are still
alive is *mostly* safe (players can rejoin the room), but only the stats
signal guarantees losslessness.

## One-time migration: run.sh → systemd

```bash
python3 deploy.py setup            # waits for idle, then switches (default)
python3 deploy.py setup --now      # switch immediately (kicks players)
```

Stages the binary, installs `/etc/systemd/system/map-together.service` +
the waiter, then either immediately or once idle: kills the `run.sh` process
tree, enables + starts the unit, and retires `run.sh` to
`run.sh.pre-systemd`. The unit itself is idempotent — re-running `setup`
just refreshes binary/unit/waiter. If anything goes wrong:

```bash
ssh MapTogetherDe                      # or Us / Au
systemctl status map-together.service
journalctl -u map-together.service -n 50   # unit logs
cp -a map-together-server.prev map-together-server   # rollback binary
systemctl restart map-together.service
```

## Ops cheat sheet

```bash
python3 deploy.py check --hosts au   # status (staged binary? waiter running?)
python3 deploy.py logs -f --hosts de # tail unit logs
ssh MapTogetherUs 'systemctl restart map-together.service'   # manual restart
ssh MapTogetherUs 'tail -20 /root/mt-idle-wait.log'          # waiter progress
```

Manual idle-restart without the tool (the `restart-when-idle.sh` script this
flow grew out of): `just restart-when-idle --idle-mins 30` on the dev box
(restarts the dev unit; 30 min guarantees room expiry).

## Files

| Path | What |
|------|------|
| `deploy.py` | deploy tool (this whole flow; `--dry-run` available) |
| `update-servers.sh` | legacy: build + scp over live servers (pre-idle-restart) |
| `restart-when-idle.sh` | dev-box variant: wait 0 conns, `systemctl --user restart map-together-dev` |
| `justfile` | `just deploy` / `deploy-check` / `deploy-logs` / `restart-when-idle` |

## Local dev

`just dev` runs the server in the foreground on :19796; `restart-when-idle.sh`
restarts the local `map-together-dev.service` user unit once idle. Same
binary flag applies: `--exit-when-idle-mins N` makes the server exit(0)
itself after N idle minutes (supervisor restarts it) — opt-in, not set in
production; the deploy flow above supersedes it there.
