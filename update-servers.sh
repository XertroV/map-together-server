#!/usr/bin/env bash
cargo build --release || exit 1
(scp target/release/map-together-server MapTogetherDe:./ || scp target/release/map-together-server MapTogetherDe:./map-together-server-next) &
(scp target/release/map-together-server MapTogetherUs:./ || scp target/release/map-together-server MapTogetherUs:./map-together-server-next) &
(scp target/release/map-together-server MapTogetherAu:./ || scp target/release/map-together-server MapTogetherAu:./map-together-server-next)
wait
