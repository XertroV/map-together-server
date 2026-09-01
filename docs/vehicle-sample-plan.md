# VehicleSample: replay-quality vehicle updates (protocol v6)

Status: IMPLEMENTED (2026-09-01) — server: commit `78f468c` (tests green);
plugin: branch `vehicle-sample` commit `22a88eb` (openplanet-lsp clean,
**awaiting in-game unittest build + lifecycle validation** before merging to
master). Supersedes `VehiclePos` (msg 15). Ships in
**protocol v6** — v6 is still unreleased (only v5 clients are live), so
VehicleSample rides the same bump as PlayerTestMode with no extra version.
The 2026-08-30 bench (`MapTogether_VehiclePosRoundTrip`,
formerly `MapTogether_VehicleMsgSerializationBench`) validated the perf
groundwork: ~12us serialize / ~11.5us deserialize for 15 floats — this plan
is the "future 50ms vehicle-update message" that bench was the stand-in for.

## Goals

- Enough state to *replay* a remote car mostly faithfully (lag and collisions
  aside): smooth path through corners and bounces, not point-teleports.
- Receiver buffers a little future so it can interpolate through jitter.
- Extensible: braking, gas, sparks, effects, wheel status/contact can be
  added later without breaking older v6 clients or touching the server.
- **Primary constraint: pack/unpack speed on the plugin side.** Packed
  little-endian binary, no JSON, no bit-twiddling cleverness that costs CPU.
- Payload is versioned independently of the protocol version.

## Wire format

Outer framing is the standard ephemeral message, new id:

```
[u8 msg = 24 (VEHICLE_SAMPLE)] [u32 payloadLen] [payload] … relayed with [lp pid][u64 serverTsMs]
```

- Protocol: **v6** (`0xFF 06 80`, already defined and accepted server-side;
  unreleased, so no compat burden from existing v6 clients).
- `min_proto_version() == 6`: the server's existing write-loop gate keeps it
  away from v5 clients (they crash on unknown msg types).
- **The server treats the payload as an opaque byte blob**: read, length-cap,
  relay verbatim. It never parses `fmt`, so payload evolution (fmt 2, 3, …)
  requires **no server deploy**. Ephemeral: never stored in room history.

### Payload, fmt 1 (64 bytes, little-endian, append-only)

| off | type | field | notes |
|----:|------|-------|-------|
| 0 | u8 | fmt | payload format version, starts at 1 |
| 1 | u8 | flags | reserved, 0 in fmt 1 (future: presence bits for optional sections) |
| 2 | u32 | tMs | sender `Time::Now` (monotonic ms; wraps ~49d, receiver handles wrap). **Epoch is per-sender** (game-launch relative): only deltas within one player's stream mean anything — never compare across players or to local time except through the per-player offset estimator |
| 6 | 3×f32 | pos | world position |
| 18 | 3×f32 | dir | forward vector (from vis iso4 / AsyncState) |
| 30 | 3×f32 | up | up vector; receiver rebuilds `left = up × dir` (orthonormalized) |
| 42 | 3×f32 | vel | world velocity m/s — drives Hermite interpolation |
| 54 | f32 | steer | InputSteer [-1, 1] |
| 58 | u8 | gas | InputGasPedal quantized 0–255 |
| 59 | u8 | brake | InputBrakePedal 0–255 (0 = not braking) |
| 60 | u8 | wheelContact | bits 0–3 = FL,FR,RL,RR ground contact; bits 4–7 reserved |
| 61 | u8 | misc | bit0 handbrake, bit1 turbo, bit2 in-water; rest reserved |
| 62 | u16 | rpm | raw engine RPM (0 if unavailable); uint16 already covers typical TM range (<65k) — engine audio/anim later |

**Versioning rule (append-only):** new fields are appended, `fmt` bumps.
A reader that knows fmt N reads its known layout and ignores the tail (outer
`payloadLen` tells it where the message ends); a reader seeing fmt > its max
parses the fmt it knows (layouts are strict prefixes). No TLV, no per-field
tags — packing stays a straight run of writes.

Planned fmt-2 candidates (appended): 4×u8 wheel slip, 4×u8 damper length
0–255, u8 gear, u8 effects bits (sparks left/right, smoke, …).

### Why these choices

- **dir+up instead of full iso4** (saves 3 floats + 3 writes): position is
  separate anyway; `left` is one cross product on receive. No mat→quat
  conversion cost on pack (quat rejected for pack-side CPU + AS awkwardness).
- **vel on the wire** is what makes replay smooth: cubic Hermite through two
  samples' pos+vel follows the cornering arc instead of chording it.
- **Sender timestamp** (`tMs`) is the interpolation clock. The server's
  appended `serverTsMs` is arrival time — network-jittered, fallback only.
  Each sender's clock has its own arbitrary starting offset, which is why the
  receiver keeps a per-player offset estimate rather than any shared clock.
- **f32 everywhere it moves**: quantizing floats costs pack CPU; only
  naturally-byte-sized things (pedals, bitfields) are bytes.
- ~64 B at 20 Hz = 1.3 KB/s/player. Ten players ≈ 13 KB/s — irrelevant.

## Plugin: sourcing (send side)

In `EditorFeed::CheckUpdateVehicle`, with two cadence changes: **no random
jitter** on the vehicle tick (decision 2), and the interval scales with the
number of players concurrently sending samples, not room size (decision 4):
`intervalMs = clamp(15 × max(1, nActiveVehicleSenders), 50, 100)` —
full 20 Hz up to 3 concurrent drivers, degrading only past that.

- iso4 via the existing single `Dev::GetOffsetIso4(vis, O_VEHICLEVIS_ISO4)`
  call; extract pos/dir/up columns.
- `vis.AsyncState`: WorldVel, InputSteer, InputGasPedal, InputBrakePedal /
  InputIsBraking, per-wheel ground contact (`FLGroundContactMaterial` etc. —
  verify in-game; damper-length threshold is the fallback), turbo/water bits,
  RPM. "Most recently available" == AsyncState at the send tick.
- Pack with the existing scratch pattern (persistent `Dev::Allocate` buffer,
  reused `MemoryBuffer`, `Seek(0)` instead of realloc). Budget ≈ 25 field
  writes ≈ 20–25 us per sample (bench-extrapolated), once per ≥50 ms. Nil.
- **Transition (dual-send):** a v6 plugin sends BOTH VehiclePos (60 B, for
  live v5 peers) and VehicleSample each tick; the server gates each by
  receiver proto. v6 receivers ignore VehiclePos from a player whose
  VehicleSample was seen in the last 2 s (v5 senders still only send
  VehiclePos). Drop the dual-send in a later bump once v5 share dies.
  (Server-side down-conversion was rejected: it would force the server to
  parse the payload, killing opacity.)

## Plugin: replay buffer + interpolation (receive side)

Per remote player, a `VehicleReplay` (replaces single `lastVehiclePos`):

- **Ring buffer** of the last 32 samples, preallocated in place (no per-msg
  allocations, no handle churn — decode straight into the ring slot).
- **Clock mapping (per player):** `offsetEst` = EWMA over
  `(localRecvMs − sample.tMs)`, kept per player — every sender's `Time::Now`
  epoch is different (game-launch relative), so there is no shared clock,
  only per-stream offsets. Render target time
  `t = localNowMs − offsetEst − delayMs`.
  `delayMs = clamp(2.5 × median inter-sample gap, 120, 500)` — the "buffer a
  little future" that absorbs jitter; adapts to the room's actual cadence
  (cadence scales with player count, so delay must too).
- **Interpolation** between the bracketing samples a, b at t:
  - position: cubic Hermite on pos+vel (follows corners/bounces);
  - teleport guard: if |Δpos| ≫ |vel|·Δt, snap instead of swooping;
  - orientation: nlerp dir and up, then orthonormalize (Gram–Schmidt +
    cross) — slerp is unnecessary at 50–150 ms spacing and costs more;
  - pedals/steer: lerp steer, step-hold the rest; contact/effects bits are
    events — applied at sample boundaries, never interpolated.
- **Underrun:** extrapolate `pos += vel·dt` for ≤150 ms, then hold and mark
  stale (fade the label; later: park the ghost).
- Consumers: today's NVG label reads interpolated pos (replacing the
  exp-smoothing hack in `RenderNvg`); a future ghost-car/model renderer reads
  the full interpolated state. The message is designed for the second one.
- Unpack budget: 10 players × 20 Hz ≈ 200 unpacks/s ≈ 5 ms/s total. Interp is
  ~50 flops per player per frame. Both negligible.

## Server changes (small, self-contained)

- No version-byte changes: v6 is already accepted and is already CURR.
- `MAPPING_MSG_VEHICLE_SAMPLE = 24`; read arm: length-checked (cap 512 B)
  opaque read → `MapAction::VehicleSample(Vec<u8>)`; `is_ephemeral() = true`;
  `min_proto_version() = 6`; write arm: emit bytes verbatim + standard
  pid/timestamp trailer, with the OnceCell buffer cache like other ephemerals.
- Tests: opaque round-trip, v5 gating (no msg 24 to v5), oversize rejection,
  short-payload safety. Deploy servers before releasing the v6 plugin (the
  CHANGELOG already encodes this ordering for v6).

## Bench (restore the pattern you remembered)

Re-add the batch bench in `NetMsg_Test.as` for VehicleSample:
`BENCH VehicleSample serialize/deserialize us/iter` lines + wire-size assert
(64) + round-trip field equality, alongside a ring-buffer + Hermite interp
unit test with synthetic jittered samples (assert continuity and that a
buffered corner path bends, not chords).

## Decisions (Max, 2026-09-01)

1. **RPM in fmt 1: yes** (u16 `rpm`, raw — no /50; uint16 max is 65535).
2. **Send jitter: none for VehicleSample.** The vehicle tick drops its random
   delay entirely (cursor updates keep theirs); timestamped + buffered interp
   removes the anti-burst rationale.
3. **Dual-send: yes.** Verified: the live v5 client's reader cleanly
   consumes-and-skips unknown length-prefixed msg types (skip path predates
   the v5 release — the server-side "old clients crash" comment is
   pessimistic for v5). The server still gates msg 24 off v5 regardless:
   ungated it would warn-spam v5 logs at 20 Hz and waste bandwidth. Dual-send
   exists so v5 peers keep *seeing* cars, not to protect them from crashes.
4. **Cadence scales with concurrent vehicle senders, not room size:**
   `intervalMs = clamp(15 × max(1, nActiveVehicleSenders), 50, 100)` where
   nActiveVehicleSenders = players whose VehicleSample was seen in the last
   ~2 s (locally: we're sending iff in test mode). Room size no longer
   throttles car updates; ceiling tunable after playtests.
5. **Renderer split confirmed:** this plan delivers the message + replay
   layer (NVG label as first consumer); the ghost-car model renderer is a
   separate follow-up project.

**dir+up vs full iso4 (asked & settled):** full iso4 costs +3 float
writes/reads per msg (~2.4 us each side) and +12 B for a left column the
receiver would discard anyway — the interpolator rebuilds the orientation
frame every rendered frame (nlerp → normalize → cross) regardless of wire
format. Reconstruction is exact to float rounding (game matrices are
orthonormal). dir+up stays.

--- SUMMARY ---

- **New ephemeral msg `VehicleSample` (id 24, protocol v6 — unreleased, so
  it rides the PlayerTestMode bump)** replacing
  VehiclePos: packed little-endian binary, ~64 B, self-versioned via a
  leading `fmt` byte with **append-only layout evolution** (old readers read
  their prefix, outer length lets them skip the tail — no TLV, no server
  redeploys, packing stays a straight run of writes).
- **fmt 1 contents:** sender timestamp, pos, dir+up (left rebuilt by cross
  product — cheaper to pack than iso4 or quat), world vel (enables Hermite
  interpolation through corners), steer f32, gas/brake bytes, wheel-contact
  bitmask, misc bits (handbrake/turbo/water), RPM. Effects/slip/dampers/gear
  are named fmt-2 appends.
- **Receiver replay layer:** per-player preallocated 32-sample ring buffer;
  EWMA sender-clock mapping; adaptive render delay (~2.5× sample gap, min
  120 ms) to buffer future samples against jitter; cubic-Hermite position +
  nlerp'd orthonormalized orientation; ≤150 ms velocity extrapolation on
  underrun, then fade. NVG labels consume it now; a ghost-car renderer later.
- **Server stays dumb:** relays the payload as an opaque length-capped blob
  gated to v6+ via the existing `min_proto_version` mechanism — payload
  version bumps never touch the server.
- **Perf:** ~25 field writes ≈ 20–25 us/pack at ≥50 ms cadence; unpack ~same;
  bench restored as greppable BENCH lines with a 64-byte wire-size assert.
- **Transition:** v6 plugins dual-send VehiclePos + VehicleSample so live v5
  peers keep seeing cars; v6 receivers prefer VehicleSample. Server deploys
  before the v6 plugin release. Sender timestamps have per-player epochs
  (game-launch relative) — all clock mapping is per-stream.
- **Decisions locked (2026-09-01):** RPM in fmt 1; zero send jitter for
  VehicleSample; dual-send kept (v5 verifiably skips unknown msgs cleanly,
  but stays gated to avoid 20 Hz warn-spam); cadence 15 ms × concurrent
  vehicle senders clamped 50–100 ms (20 Hz up to 3 drivers), not room size; ghost-car renderer is a
  separate follow-up; dir+up beats full iso4 (left is rebuilt per-frame by
  the interpolator anyway).
