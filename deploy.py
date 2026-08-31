#!/usr/bin/env python3
"""map-together production deploy tool.

Servers (all root, binary /root/map-together-server, TCP :19796):
    de  MapTogetherDe  map-together-de.xk.io  (Fra1)
    us  MapTogetherUs  map-together-us.xk.io  (Sfo3)
    au  MapTogetherAu  map-together-au.xk.io  (Syd1)

Supervision: systemd unit map-together.service (installed by `setup`).
Logs: journald (`logs`).

Deploy flow (default): cargo build --release, atomic upload to
map-together-server-next, then a flock'd background idle-waiter
(/root/mt-idle-wait.sh) on each host restarts the unit once the server's
own "[stats] players connected: 0 across 0 rooms" journald line (5-min
cadence) has been seen ZERO_SAMPLES consecutive times. "0 rooms" means every
room has already expired (rooms die ~30 min after their last player), so a
stats-triggered restart loses nothing rejoinable. Old binaries without
[stats] logs fall back to "zero established connections on :19796 for
--idle-mins minutes".

One-time migration off the legacy `bash ./run.sh` setup: `setup`.
Runbook: DEPLOYMENT.md.
"""

import argparse
import concurrent.futures
import fcntl
import hashlib
import subprocess
import sys
import tempfile
import time
from pathlib import Path

REPO = Path(__file__).resolve().parent
BIN = REPO / "target/release/map-together-server"

HOSTS = ["de", "us", "au"]
ALIAS = {"de": "MapTogetherDe", "us": "MapTogetherUs", "au": "MapTogetherAu"}
UNIT = "map-together.service"
WAITER = "/root/mt-idle-wait.sh"
LOCAL_LOCK = "/tmp/map-together-deploy.lock"
PORT = 19796


# --------------------------------------------------------------------------- remote assets

SYSTEMD_UNIT = r"""[Unit]
Description=Map Together server (TCP 19796)
After=network-online.target
Wants=network-online.target
# Never give up restarting: a transient port conflict (e.g. a legacy process
# lingering) crash-loops fast enough to trip the default start limit.
StartLimitIntervalSec=0

[Service]
Type=simple
WorkingDirectory=/root
# Promote a staged binary if present; keep the previous one as .prev for rollback.
ExecStartPre=-/bin/sh -c 'if [ -e /root/map-together-server-next ]; then cp -a /root/map-together-server /root/map-together-server.prev 2>/dev/null; mv -f /root/map-together-server-next /root/map-together-server; fi'
ExecStart=/root/map-together-server
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
"""

WAITER_SH = r"""#!/usr/bin/env bash
# map-together idle-waiter: wait until the server is empty, then restart
# map-together.service so it picks up /root/map-together-server-next.
#
#   mt-idle-wait.sh [ZERO_SAMPLES] [SS_IDLE_MINS] [restart|switch|switch-now]
#
# Primary signal: the server's own "[stats] players connected: N across M rooms"
# journald line (every 5 min). After ZERO_SAMPLES consecutive
# "0 players across 0 rooms" samples it restarts. Rooms expire ~30 min after
# their last player, so "0 across 0 rooms" implies nothing rejoinable remains:
# a stats-triggered restart is lossless.
# Fallback (old binary, no [stats] in journal): zero established TCP
# connections on :19796 for SS_IDLE_MINS minutes.
# switch: migrate a run.sh-based install to systemd (kill old, enable+start
# unit, retire run.sh) instead of a plain restart. switch-now skips the wait.
#
# Only one waiter runs at a time: flock on /run/lock/mt-idle-wait.lock.
set -u
UNIT=${MT_UNIT:-map-together.service}
PORT=${MT_PORT:-19796}
MT_SS_POLL=${MT_SS_POLL:-5}
ZERO_SAMPLES=${1:-2}
SS_IDLE_MINS=${2:-10}
ACTION=${3:-restart}
LOCK=/run/lock/mt-idle-wait.lock
POLL=${MT_POLL:-20}
log() { echo "$(date -Is) $*"; }

exec 9>"$LOCK" || exit 1
flock -n 9 || { log "another idle-waiter already holds $LOCK; exiting"; exit 0; }

switch_action() {
    log "migrating to systemd supervision"
    # The legacy setup is a `while true; do bash ./run.sh; done` loop in an
    # abandoned tmux zsh: kill the loop shell (KILL — interactive zsh ignores
    # TERM) or run.sh respawns and AddrInUse-crash-loops the unit.
    for p in $(pgrep -f 'bash \./run\.sh'); do
        pp=$(ps -o ppid= -p "$p" 2>/dev/null | tr -d ' ')
        [ -n "$pp" ] && [ "$pp" != 1 ] && kill -9 "$pp" 2>/dev/null
        kill "$p" 2>/dev/null
    done
    pkill -f 'bash \./run\.sh' 2>/dev/null || true
    pkill -f '\./map-together-server' 2>/dev/null || true
    sleep 1
    systemctl daemon-reload
    systemctl enable "$UNIT" >/dev/null 2>&1 || true
    if systemctl restart "$UNIT"; then
        sleep 2
        if systemctl is-active --quiet "$UNIT"; then
            mv -f /root/run.sh /root/run.sh.pre-systemd 2>/dev/null || true
            log "migration OK: $UNIT active; run.sh retired"
        else
            log "ERROR: $UNIT not active after start; recent journal:"
            journalctl -u "$UNIT" -o cat -n 15 --no-pager
        fi
    else
        log "ERROR: systemctl restart $UNIT failed"
    fi
}

finish() {
    if [ "$ACTION" = switch ] || [ "$ACTION" = switch-now ]; then
        switch_action
    else
        log "server idle; restarting $UNIT"
        systemctl restart "$UNIT"
        sleep 2
        if systemctl is-active --quiet "$UNIT"; then log "restart OK"; else log "WARN: $UNIT not active after restart"; fi
    fi
}

if [ "$ACTION" = switch-now ]; then finish; exit 0; fi

if journalctl -u "$UNIT" -o cat --since "-3 hours" 2>/dev/null | grep -q '\[stats\] players connected'; then
    log "stats mode: waiting for $ZERO_SAMPLES consecutive '0 players across 0 rooms' samples"
    streak=0
    last_ts=""
    while :; do
        line=$(journalctl -u "$UNIT" -o short-iso --since "-6 min" --no-pager 2>/dev/null | grep '\[stats\] players connected' | tail -1)
        if [ -n "$line" ]; then
            ts=${line%% *}
            if [ "$ts" != "$last_ts" ]; then
                last_ts=$ts
                msg=${line#* }
                case "$msg" in
                    *"players connected: 0 across 0 rooms"*)
                        streak=$((streak + 1))
                        log "[$streak/$ZERO_SAMPLES] empty: $msg"
                        [ "$streak" -ge "$ZERO_SAMPLES" ] && break
                        ;;
                    *)
                        [ "$streak" -gt 0 ] && log "players present; streak reset: $msg"
                        streak=0
                        ;;
                esac
            fi
        fi
        sleep "$POLL"
    done
else
    needed=$((SS_IDLE_MINS * 60))
    dead_ms=$((SS_IDLE_MINS * 60000))
    idle=0
    log "no [stats] in journal (old binary?); connection mode: $SS_IDLE_MINS min with 0 active conns on :$PORT"
    active_conns() {
        ss -Htino state established "( sport = :$PORT )" 2>/dev/null | awk -v d="$dead_ms" '{for(i=1;i<=NF;i++) if ($i ~ /^lastsnd:/) {split($i,a,":"); if (a[2]+0 < d) {c++; break}}} END {print c+0}'
    }
    while :; do
        n=$(active_conns)
        if [ "$n" -eq 0 ]; then
            idle=$((idle + MT_SS_POLL))
            [ $((idle % 60)) -eq 0 ] && log "idle ${idle}s/${needed}s"
            [ "$idle" -ge "$needed" ] && break
        else
            [ "$idle" -gt 0 ] && log "$n active connection(s); idle timer reset"
            idle=0
        fi
        sleep "$MT_SS_POLL"
    done
fi
finish
"""

CHECK_SH = r"""U=${U:-map-together.service}
p() { printf '%s\n' "$*"; }
# is-active prints "inactive" AND exits nonzero for a stopped unit, so a bare
# `|| echo not-installed` prints both; only fall back when it printed nothing.
a=$(systemctl is-active "$U" 2>/dev/null); [ -n "$a" ] || a=not-installed
p "unit: $a$(systemctl is-enabled "$U" >/dev/null 2>&1 && printf ' (enabled)')"
p "since: $(systemctl show -p ActiveEnterTimestamp --value "$U" 2>/dev/null)"
b=$(stat -c '%y' /root/map-together-server 2>/dev/null | cut -d. -f1)
p "binary: $b md5=$(md5sum /root/map-together-server 2>/dev/null | cut -c1-32)"
n=$(stat -c '%y' /root/map-together-server-next 2>/dev/null | cut -d. -f1)
p "staged: ${n:-none} md5=$(md5sum /root/map-together-server-next 2>/dev/null | cut -c1-32)"
p "prev: $(ls /root/map-together-server.prev >/dev/null 2>&1 && echo present || echo none)"
p "run.sh: $(pgrep -fc 'bash \./run\.sh' 2>/dev/null || echo 0) old-proc, file $(ls /root/run.sh >/dev/null 2>&1 && echo present || echo absent)"
p "conns: $(ss -Htn state established "( sport = :19796 )" 2>/dev/null | wc -l) total, $(ss -Htino state established "( sport = :19796 )" 2>/dev/null | awk -v d=600000 '{for(i=1;i<=NF;i++) if ($i ~ /^lastsnd:/) {split($i,a,":"); if (a[2]+0 < d) {c++; break}}} END {print c+0}') active (traffic <10min)"
echo "stats:"
journalctl -u "$U" -o cat -n 8 --no-pager 2>/dev/null | grep '\[stats\]' | tail -2 | sed 's/^/  /'
if flock -n /run/lock/mt-idle-wait.lock -c true 2>/dev/null; then echo "waiter: not running"; else echo "waiter: RUNNING"; fi
tail -3 /root/mt-idle-wait.log 2>/dev/null | sed 's/^/  /'
"""


# --------------------------------------------------------------------------- helpers

def sh(cmd, timeout=300, input=None):
    return subprocess.run(cmd, text=True, capture_output=True, timeout=timeout, input=input)


def ssh(host, remote_cmd, timeout=60):
    r = sh(["ssh", "-o", "BatchMode=yes", ALIAS[host], remote_cmd], timeout=timeout)
    if r.returncode != 0:
        raise RuntimeError(f"ssh {ALIAS[host]} {remote_cmd!r}: {r.stderr.strip() or r.stdout.strip()}")
    return r.stdout.strip()


def ssh_bash(host, script, timeout=120):
    r = subprocess.run(["ssh", "-o", "BatchMode=yes", ALIAS[host], "bash -s"],
                       input=script, text=True, capture_output=True, timeout=timeout)
    if r.returncode != 0:
        raise RuntimeError(f"ssh {ALIAS[host]}: {r.stderr.strip() or r.stdout.strip()}")
    return r.stdout


def scp_to(host, src: Path, dst: str, timeout=300):
    r = sh(["scp", "-q", "-o", "BatchMode=yes", str(src), f"{ALIAS[host]}:{dst}"], timeout=timeout)
    if r.returncode != 0:
        raise RuntimeError(f"scp {ALIAS[host]}:{dst}: {r.stderr.strip()}")


def put_file(host, content: str, dst: str):
    with tempfile.NamedTemporaryFile("w", suffix=".tmp", delete=False) as f:
        f.write(content)
        tmp = Path(f.name)
    try:
        scp_to(host, tmp, dst)
    finally:
        tmp.unlink(missing_ok=True)


def die(msg):
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def parse_hosts(s):
    hosts = [h.strip() for h in s.split(",") if h.strip()] if s != "all" else list(HOSTS)
    for h in hosts:
        if h not in ALIAS:
            die(f"unknown host {h!r} (known: {', '.join(HOSTS)}, or 'all')")
    return hosts


def md5_of(p: Path):
    return hashlib.md5(p.read_bytes()).hexdigest() if p.exists() else None


def acquire_local_lock():
    fd = open(LOCAL_LOCK, "w")
    try:
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        fd.close()
        die(f"another deploy is already running (lock {LOCAL_LOCK} held); wait for it or remove the file if stale")
    return fd


def build_release():
    print("building (cargo build --release -j2) ...")
    r = subprocess.run(["cargo", "build", "--release", "-j2"], cwd=REPO)
    if r.returncode != 0:
        die("cargo build failed")


def stage_binary(host, local_md5):
    """Upload the binary atomically (write .part, then rename)."""
    scp_to(host, BIN, "/root/map-together-server-next.part")
    ssh(host, "mv -f /root/map-together-server-next.part /root/map-together-server-next")
    remote_md5 = ssh(host, "md5sum /root/map-together-server-next | cut -c1-32")
    if remote_md5 != local_md5:
        raise RuntimeError(f"{host}: staged md5 mismatch (local {local_md5}, remote {remote_md5})")
    return remote_md5


def install_waiter(host):
    # Upload to .new and mv: a running waiter keeps its old inode across the
    # rename, whereas scp-ing over the live path corrupts bash mid-execution
    # (bash reads scripts incrementally).
    put_file(host, WAITER_SH, WAITER + ".new")
    ssh(host, f"chmod +x {WAITER}.new && mv -f {WAITER}.new {WAITER}")


def unit_installed(host):
    return ssh(host, f"test -f /etc/systemd/system/{UNIT} && echo y || echo n") == "y"


def spawn_waiter_bg(host, remote_cmd):
    """nohup the waiter on the host so it survives this ssh session exiting.
    Truncates the log first and waits for fresh output so we never report a
    previous waiter's lines as this one's status."""
    ssh(host, ": > /root/mt-idle-wait.log")
    out = ssh(host, f"nohup {remote_cmd} > /root/mt-idle-wait.log 2>&1 < /dev/null & echo started pid $!")
    tail = ""
    for _ in range(15):
        time.sleep(1)
        tail = ssh(host, "tail -3 /root/mt-idle-wait.log 2>/dev/null") or ""
        if tail.strip():
            break
    return out, tail


def verify_unit_up(host):
    for _ in range(8):
        try:
            out = ssh(host, f"systemctl is-active --quiet {UNIT} && ss -Htln '( sport = :{PORT} )' | grep -q . && echo UP || echo DOWN")
            if out == "UP":
                return True
        except RuntimeError:
            pass
        time.sleep(2)
    return False


# --------------------------------------------------------------------------- commands

def deploy_host(host, args, local_md5):
    res = {"host": host, "staged": None, "mode": None, "note": ""}
    staged = stage_binary(host, local_md5)
    res["staged"] = staged
    if not unit_installed(host):
        if args.restart:
            raise RuntimeError(f"{host}: {UNIT} not installed — run `setup` first (--restart needs systemd)")
        res["mode"] = "staged-only"
        res["note"] = f"{UNIT} not installed: binary staged, NO idle-restart scheduled. Run `setup`."
        return res
    # Always refresh: installing only-if-missing left a pre-lastsnd-filter
    # waiter live on the hosts, where ghost connections blocked it forever.
    install_waiter(host)
    # A live legacy `bash run.sh` process still owns :19796; a plain unit
    # restart would crash-loop on AddrInUse. The waiter's `switch` action
    # kills the legacy processes first (same lossless idle wait).
    legacy = ssh(host, r"pgrep -f 'bash \./run\.sh' >/dev/null && echo y || echo n") == "y"
    if args.restart:
        if legacy:
            raise RuntimeError(f"{host}: legacy run.sh still running — use `setup --now` (or drop --restart) so it is killed before the unit starts")
        ssh(host, f"systemctl restart {UNIT}")
        if not verify_unit_up(host):
            raise RuntimeError(f"{host}: unit not active/listening after restart — check `logs`")
        res["mode"] = "restarted-now"
    else:
        action = " switch" if legacy else ""
        _, tail = spawn_waiter_bg(host, f"{WAITER} {args.zero_samples} {args.idle_mins}{action}")
        if "already holds" in tail:
            res["mode"] = "waiter-already-running"
            res["note"] = "an idle-waiter was already scheduled; not starting a second"
        else:
            res["mode"] = "idle-waiter-started" + (" (switch: retiring legacy run.sh)" if legacy else "")
            res["note"] = tail.splitlines()[-1]
    return res


def cmd_deploy(args):
    hosts = parse_hosts(args.hosts)
    if not args.skip_build:
        build_release()
    if not BIN.exists():
        die(f"{BIN} missing (build failed, or drop --skip-build)")
    local_md5 = md5_of(BIN)
    print(f"binary: {BIN} md5={local_md5}")
    lock = acquire_local_lock()
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(hosts)) as ex:
            futures = {ex.submit(deploy_host, h, args, local_md5): h for h in hosts}
            results = []
            for fut in concurrent.futures.as_completed(futures):
                h = futures[fut]
                try:
                    results.append(fut.result())
                except Exception as e:  # noqa: BLE001 — one bad host must not sink the rest
                    print(f"[{h}] ERROR: {e}", file=sys.stderr)
                    results.append({"host": h, "error": str(e)})
    finally:
        lock.close()

    print("\n=== deploy summary ===")
    for r in sorted(results, key=lambda r: r["host"]):
        if "error" in r:
            print(f"  {r['host']}: FAILED — {r['error']}")
        else:
            print(f"  {r['host']}: staged {r['staged']} | {r['mode']} | {r['note']}")
    if not args.restart and any(r.get("mode") in ("idle-waiter-started", "waiter-already-running") for r in results):
        print("\nservers will restart on their own once empty; watch with: deploy.py check  /  deploy.py logs")


def setup_host(host, args, local_md5):
    res = {"host": host}
    stage_binary(host, local_md5)
    put_file(host, SYSTEMD_UNIT, f"/etc/systemd/system/{UNIT}")
    install_waiter(host)
    ssh(host, f"systemctl daemon-reload && systemctl enable {UNIT} >/dev/null 2>&1 || true")
    if unit_active(host):
        res["mode"] = "already-migrated"
        res["note"] = f"{UNIT} already active; binary+unit+waiter refreshed"
        return res
    if args.now:
        out = ssh(host, f"{WAITER} {args.zero_samples} {args.idle_mins} switch-now", timeout=120)
        res["mode"] = "switched-now"
        res["note"] = out.splitlines()[-1] if out else "(no output)"
    else:
        _, tail = spawn_waiter_bg(host, f"{WAITER} {args.zero_samples} {args.idle_mins} switch")
        if "already holds" in tail:
            res["mode"] = "switch-NOT-scheduled"
            res["note"] = "another waiter holds the lock; re-run setup later"
        else:
            res["mode"] = "switch-scheduled"
            res["note"] = tail.splitlines()[-1]
    return res


def unit_active(host):
    return ssh(host, f"systemctl is-active --quiet {UNIT} 2>/dev/null && echo y || echo n") == "y"


def cmd_setup(args):
    hosts = parse_hosts(args.hosts)
    if not args.skip_build:
        build_release()
    if not BIN.exists():
        die(f"{BIN} missing")
    local_md5 = md5_of(BIN)
    print(f"binary: {BIN} md5={local_md5}")
    mode = "immediately (--now)" if args.now else f"once idle ({args.idle_mins} min with 0 conns; old binary has no [stats] logs)"
    print(f"plan: stage binary, install {UNIT} + {WAITER}, switch to systemd {mode}\n")
    lock = acquire_local_lock()
    try:
        with concurrent.futures.ThreadPoolExecutor(max_workers=len(hosts)) as ex:
            futures = {ex.submit(setup_host, h, args, local_md5): h for h in hosts}
            results = []
            for fut in concurrent.futures.as_completed(futures):
                h = futures[fut]
                try:
                    results.append(fut.result())
                except Exception as e:  # noqa: BLE001
                    print(f"[{h}] ERROR: {e}", file=sys.stderr)
                    results.append({"host": h, "error": str(e)})
    finally:
        lock.close()

    print("\n=== setup summary ===")
    for r in sorted(results, key=lambda r: r["host"]):
        if "error" in r:
            print(f"  {r['host']}: FAILED — {r['error']}")
        else:
            print(f"  {r['host']}: {r['mode']} | {r['note']}")
    print("\ncheck progress: deploy.py check  /  deploy.py logs")


def cmd_check(args):
    hosts = parse_hosts(args.hosts)
    local_md5 = md5_of(BIN)

    def one(h):
        out = ssh_bash(h, CHECK_SH, timeout=90)
        return h, out

    with concurrent.futures.ThreadPoolExecutor(max_workers=len(hosts)) as ex:
        results = list(ex.map(one, hosts))
    for h, out in sorted(results):
        print(f"=== {h} ({ALIAS[h]}) ===")
        for line in out.splitlines():
            print(f"  {line}")
        m = [l for l in out.splitlines() if l.startswith("binary:") and "md5=" in l]
        if m and local_md5:
            up_to_date = local_md5 in m[0]
            print(f"  vs local build: {'✓ current' if up_to_date else '→ differs (stale, or newer than local)'}")
        print()


def cmd_logs(args):
    for h in parse_hosts(args.hosts):
        print(f"=== {h} ({ALIAS[h]}) ===")
        cmd = f"journalctl -u {UNIT} -o cat -n {args.lines} --no-pager"
        if args.follow:
            cmd += " -f"
        r = subprocess.run(["ssh", ALIAS[h], cmd])
        if r.returncode != 0:
            print(f"  (no logs — is {UNIT} installed/migrated yet?)")
        print()


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)

    d = sub.add_parser("deploy", help="build + upload + background idle-waiter (or --restart to restart now)")
    d.add_argument("--hosts", default="all", help="comma list among de,us,au (default all)")
    d.add_argument("--restart", action="store_true", help="restart immediately instead of waiting for idle (kicks players)")
    d.add_argument("--zero-samples", type=int, default=2, help="consecutive '0 players across 0 rooms' [stats] samples before restart (default 2; 1 is already lossless, more = extra margin)")
    d.add_argument("--idle-mins", type=int, default=10, help="fallback: minutes of 0 connections before restart, old binaries only (default 10)")
    d.add_argument("--skip-build", action="store_true", help="use existing target/release binary")
    d.add_argument("--dry-run", action="store_true", help="show what would be done")
    d.set_defaults(fn=cmd_deploy)

    s = sub.add_parser("setup", help="one-time migration: install systemd unit + waiter, switch over from run.sh")
    s.add_argument("--hosts", default="all")
    s.add_argument("--now", action="store_true", help="switch immediately instead of waiting for idle (kicks players)")
    s.add_argument("--zero-samples", type=int, default=2)
    s.add_argument("--idle-mins", type=int, default=10, help="minutes of 0 connections before switching (default 10)")
    s.add_argument("--skip-build", action="store_true")
    s.set_defaults(fn=cmd_setup)

    c = sub.add_parser("check", help="per-host status: unit, binary freshness, connections, last [stats], waiter")
    c.add_argument("--hosts", default="all")
    c.set_defaults(fn=cmd_check)

    l = sub.add_parser("logs", help="journald logs of the unit")
    l.add_argument("--hosts", default="all")
    l.add_argument("--follow", "-f", action="store_true")
    l.add_argument("--lines", "-n", type=int, default=50)
    l.set_defaults(fn=cmd_logs)

    args = ap.parse_args()
    if args.cmd == "deploy" and args.dry_run:
        hosts = parse_hosts(args.hosts)
        md5 = md5_of(BIN)
        print(f"dry-run for hosts: {', '.join(hosts)}")
        print(f"  would use binary {BIN} md5={md5 or '(MISSING — would build)'}")
        for h in hosts:
            print(f"  [{h}] scp binary → map-together-server-next.part; mv → map-together-server-next")
            if args.restart:
                print(f"  [{h}] systemctl restart {UNIT} (immediate, kicks players)")
            else:
                print(f"  [{h}] nohup {WAITER} {args.zero_samples} {args.idle_mins}  (flock'd bg; restarts once [stats] shows 0 players + 0 rooms ×{args.zero_samples})")
        return
    args.fn(args)


if __name__ == "__main__":
    main()
