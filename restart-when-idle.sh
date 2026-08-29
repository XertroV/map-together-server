#!/usr/bin/env bash
# Wait until the map-together server has had zero player connections for
# IDLE_MINS (default 10), then restart it (picks up a freshly built binary).
#
# Run this after `cargo build` to deploy without kicking anyone.
# Note: rooms are memory-only. Rooms with no players expire server-side
# after 30 min, so `--idle-mins 30` guarantees the restart loses nothing.
set -euo pipefail

IDLE_MINS=10
PORT=19796
RESTART_CMD=${RESTART_CMD:-"systemctl --user restart map-together-dev.service"}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --idle-mins) IDLE_MINS="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        *) echo "usage: $0 [--idle-mins N] [--port P]  (RESTART_CMD env overrides restart command)"; exit 1 ;;
    esac
done

conns() { ss -Htn state established "( sport = :$PORT )" | wc -l; }

needed=$(( IDLE_MINS * 60 ))
idle=0
echo "waiting for $IDLE_MINS min with 0 connections on :$PORT, then: $RESTART_CMD"
while true; do
    n=$(conns)
    if [[ "$n" -eq 0 ]]; then
        idle=$(( idle + 5 ))
        if (( idle >= needed )); then break; fi
    else
        if (( idle > 0 )); then echo "$(date -Is) $n connection(s); idle timer reset"; fi
        idle=0
    fi
    sleep 5
done
echo "$(date -Is) idle for $IDLE_MINS min; restarting"
eval "$RESTART_CMD"
echo "$(date -Is) restarted"
