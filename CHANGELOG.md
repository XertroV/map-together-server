# Changelog (agent-facing protocol/conventions)

- 2026-08-29: PlayerTestMode folded into protocol v6 (only v5 is live, so v6/v7 collapse into one unreleased bump; 0xFF 07 80 version bytes are no longer accepted). Ephemeral msg PlayerTestMode=23 (test-mode enter/leave + car-skin url); Player.proto_version captured at handshake; MapAction::min_proto_version() gate in the write loop stops relaying unknown types to old clients (they crash on them). v5 clients still accepted. Deploy servers BEFORE releasing the v6 plugin.
