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
