# Changelog (agent-facing protocol/conventions)

- 2026-08-29: Protocol v7 - new ephemeral msg PlayerTestMode=23 (test-mode enter/leave + car-skin url). Player.proto_version captured at handshake; MapAction::min_proto_version() gate in the write loop stops relaying unknown types to old clients (they crash on them). v5/v6 clients still accepted. Deploy servers BEFORE releasing the v7 plugin.
