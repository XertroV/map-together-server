#!/usr/bin/env bash
cargo build --release || exit 1
scp target/release/map-together-server MapTogetherDe:./ &
scp target/release/map-together-server MapTogetherUs:./ &
scp target/release/map-together-server MapTogetherAu:./
wait
