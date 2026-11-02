#!/usr/bin/env bash
set -uo pipefail
cd -- "$(dirname -- "${BASH_SOURCE[0]}")" || exit 1
cargo build --release || exit 1

# Stage directly: overwriting a running executable is expected to fail on Linux.
# Rename only after a complete upload, so a partial transfer cannot be promoted.
stage() {
    local host=$1
    if scp -q target/release/map-together-server "$host:/root/map-together-server-next.part" &&
        ssh -o BatchMode=yes "$host" 'mv -f /root/map-together-server-next.part /root/map-together-server-next'; then
        printf '[%s] OK: uploaded and staged; restart still required.\n' "$host"
    else
        printf '[%s] FAILED: could not stage the new binary.\n' "$host" >&2
        return 1
    fi
}

hosts=(MapTogetherDe MapTogetherUs MapTogetherAu)
pids=()
for host in "${hosts[@]}"; do
    stage "$host" &
    pids+=("$!")
done

failed=0
for pid in "${pids[@]}"; do
    wait "$pid" || failed=1
done
if (( failed )); then
    printf 'Upload failed on one or more servers; see FAILED above.\n' >&2
    exit 1
fi
printf 'All servers staged. Running processes are unchanged.\n'
printf 'Use just deploy --skip-build to schedule restarts once idle.\n'
