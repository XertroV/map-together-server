# map-together-server task runner

set shell := ["bash", "-euo", "pipefail", "-c"]

# Limit cargo parallelism (system etiquette).
export CARGO_BUILD_JOBS := "2"

# Show available recipes.
default:
    @just --list

# Start the local TCP server (listens on 0.0.0.0:19796).
# Extra args are forwarded, e.g. `just dev -- --dump-macroblocks`.
dev *args:
    cargo run --bin map-together-server -j2 -- {{ args }}

# Build the server binary.
build:
    cargo build --bin map-together-server -j2

# Run the test suite.
test:
    cargo test -j2 -- --test-threads=2

# Type-check without building binaries.
check:
    cargo check -j2

# Dev: wait 0 connections 10 min, then restart dev unit. Args pass through.
restart-when-idle *args:
    ./restart-when-idle.sh {{ args }}

# Build, upload, and schedule an idle-restart on all prod servers (see DEPLOYMENT.md).
deploy *args:
    python3 deploy.py deploy {{ args }}

# Per-host deploy status: unit, binary freshness, connections, last [stats], waiter.
deploy-check *args:
    python3 deploy.py check {{ args }}

# Journald logs of the prod unit (e.g. `just deploy-logs -f --hosts de`).
deploy-logs *args:
    python3 deploy.py logs {{ args }}
