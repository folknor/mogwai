# Bug hunt: mogwai-protocol

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Cross-scope: finding 1 below is reported more fully from the server side in
`bugs-server-transport.md` finding 1; finding 2 is corrected there by finding 6
(the `--duration 0s` behaviour is "fall back to the config", not "run forever").

## 1. `AdmissionSubject` echoes an unbounded client id - `ADMISSION_FRAME_MAX_BYTES` is not a bound (high confidence)

`crates/mogwai-protocol/src/messages.rs:175` claims 4096 bytes is an upper bound
on any `EventKind::Admission` frame, and `crates/mogwai-server/src/admission.rs:43`
builds `ADMISSION_LANE_FRAMES = 64` on top of it. That derivation only holds if
every string in an admission frame is capped.

`ExecLanes::emit_admission` truncates `reason` on `ProtocolError`,
`AdmissionRejected` and `HavocDiagnostic` - deliberately, "rather than at each of
the dozen call sites, which is what actually makes `ADMISSION_FRAME_MAX_BYTES`
hold". It does not touch `AdmissionRejected.subject`, and `http::admission_subject`
clones `client_order_id` / `request_id` verbatim:

```rust
ClientMessage::SubmitOrder(o) => AdmissionSubject::Submit {
    client_order_id: o.client_order_id.clone(),
},
```

Every other echo path in the same file goes through `truncate_client_id`, with a
comment explicitly about the "8 MiB `client_order_id` into an 8 MiB
`OrderRejected`" failure. The subject is the one that was missed.

Two reachable paths:

- `http.rs` `boundary_outcome`: the command is already known malformed - that is
  how you get here with an over-length id in the first place - and if
  `try_reserve_boundary()` fails, the refusal is an `AdmissionRejected` carrying
  the full id. Reaching it just needs concurrent malformed submits to exhaust the
  boundary byte budget.
- `ws.rs` `dispatch_command`: the pending-act-capacity refusal is built from the
  raw `cmd` before `boundary_error` ever runs, so no validation has happened at
  all. Needs an armed `CommandLatency` plus saturated act capacity - a normal
  havoc scenario.

No inbound WS frame-size limit is configured anywhere in `mogwai-server`
(grepped for `max_frame_size` / `max_message_size` / `WebSocketConfig` - nothing),
so the ceiling is tungstenite's 64 MiB default message. A client can queue up to
64 priority-lane frames of ~64 MiB each: ~4 GiB where the code claims 512 KiB.

The protocol crate's own test (`admission_frames_fit_their_ceiling`) and the
server's (`admission_reasons_are_truncated_at_the_lane`) both only vary `reason`.
Neither samples a long subject id, which is why this survived.

Fix, structurally: `AdmissionSubject` should not be constructible with an
over-length id. Either truncate in `emit_admission` alongside `reason` (cheap,
matches the existing "do it at the lane" argument), or - better, given pre-1.0 -
make `AdmissionSubject`'s id fields a `BoundedId` newtype whose only constructor
truncates, so the invariant is in the type rather than in a match arm someone can
forget. The current design has the same invariant restated at ~6 call sites and
enforced at 5 of them.

## 2. `LaunchSpec.duration = Some(Duration::ZERO)` silently means "run forever" (high confidence)

`format_duration(Duration::ZERO)` yields `"0s"`, pinned by a test.
`mogwai-server/src/main.rs` does `.filter(|ns| *ns != 0)`, so `--duration 0s`
decodes to `None` = no declared completion. The server comment even says "Zero
means the same thing here as it does in the config file: NO declared completion."

So a launcher that computes a duration and lands on zero gets an unbounded run
instead of an immediate one, with no error anywhere. `LaunchSpec::duration` is
documented as "overriding the config's run duration. Typed, so a malformed
duration is impossible" - the type-safety claim is exactly what makes this trap
invisible. `launch` refuses `ready_timeout: ZERO` with a named error
(`ZeroReadyTimeout`); `duration: ZERO` deserves the same treatment, or the doc
must say it means "no duration".

See `bugs-server-transport.md` finding 6: the server-side reading is that
`--duration 0s` actually falls back to the config value, which is a third
behaviour and matches neither doc.

## 3. `validate_divergence` is not the authoritative guard it claims to be (medium)

Its doc says it "is the authoritative guard that rejects the misconfiguration
early". Two gaps:

- `PartialFillNext.client_order_id` and `CancelOpenOrderSilently.client_order_id`
  are length-unchecked. The blank-id check exists precisely because "a partial
  targeting a blank id can never match an order - it would sit in the armed queue
  forever as a dead entry, and no API flushes engine-side single-shots." An id
  longer than `MAX_CLIENT_ID_LEN` has exactly the same property:
  `validate_client_order_id` rejects every submit that could carry it, so the arm
  is permanently inert. The stated reasoning applies and the check is missing.
  Add `validate_client_order_id` to both arms.
- `RejectNextSubmit.reason` is unbounded here. It happens to be safe because
  `http.rs` truncates at the arming boundary, but that means the protocol crate's
  "authoritative guard" is not the thing enforcing the invariant
  `ORDER_EVENT_MAX_BYTES` depends on. If a second arming path is ever added (the
  adapter relaying `HavocSpec.server`, a config-loaded divergence list), it
  bypasses the truncation and voids the order-event reservation. The truncation
  belongs in `validate_divergence`'s crate, not in one HTTP handler.

## 4. `read_ready` has no line-length bound (medium, adversarial)

`launch.rs`: `BufReader::read_line(&mut line)` on the child's stdout, unbounded. A
venue binary that is wrong (or a `LaunchSpec::binary` pointed at something
hostile) writing gigabytes with no newline OOMs the launcher process. The module
doc goes to length about the ready read being unbounded in time and bounding it;
the size dimension is unaddressed. `ReadyRecord` has a known small shape - cap it
(a few KiB) with `take()` and report `Malformed`.

Relatedly, `LaunchError::Malformed { line }` embeds the raw untrusted line in a
`Display` message with no cap.

## 5. Stale / wrong derivations in `sizing.rs` (low, but it is the file whose whole job is being right)

The constants are numerically safe today; the derivations the module doc insists
on ("every constant below carries a field-by-field derivation from the struct it
bounds") have drifted from the structs:

- `ORDER_EVENT_MAX_BYTES` enumerates `OrderFilled`'s fields and charges
  `3 * MAX_CLIENT_ID_LEN`. `OrderFilled` carries four id-shaped strings -
  `client_order_id`, `venue_order_id`, `trade_id`, and `position_id` (which is
  client-supplied and capped at `MAX_CLIENT_ID_LEN` by `validate_submit_order`).
  `position_id` is not in the enumeration. It survives only because the same
  constant charges an unused `MAX_REASON_LEN` (`ESC * 512 = 3072`) that
  `OrderFilled` has no field for. `FILL_ROW_MAX_BYTES` right below it charges 4 -
  the two disagree about the same struct.
- `POSITION_ROW_MAX_BYTES` says "`symbol`, `quantity`, `avg_px` - two decimals...
  rounded to 128" and the constant is `256 + ESC * (SYMBOL + CLIENT_ID)`.
  `Position` now also has `position_id`, `mark_px` and `unrealized_pnl`. The
  comment describes a struct that no longer exists.
- `ADMISSION_FRAME_MAX_BYTES`' derivation charges a `MAX_SYMBOL_LEN`;
  `AdmissionRejected` has no symbol field.

Since the argument is "a finite test matrix samples an upper bound, it cannot
prove one", a derivation that no longer matches the struct is the only thing
standing between the code and an unproven bound.

## 6. `AccountId`'s invariant is bypassable (low)

`pub struct AccountId(pub String)` with a public field and a `parse` that enforces
length and charset. `account_state_max_bytes` charges `ESC * MAX_ACCOUNT_ID_LEN` on
the strength of that cap. Nothing prevents `AccountId(giant_string)`. Today every
construction goes through `parse` (checked across server, engine and adapter), so
this is latent, not live. Make the field private; the `serde(transparent)`
`Deserialize` derive is also a hole - it will happily decode an over-length id
straight past `parse`.

## 7. Every wire frame pays serde's internally-tagged buffering (perf, structural)

Both `ClientMessage` and `ServerMessage` are `#[serde(tag = "type")]`. serde's
internally-tagged deserialization cannot stream: it buffers the entire object into
`serde::__private::de::Content` (a `Vec` of owned keys and values) to find the tag,
then replays it into the variant. That is a full allocating intermediate
representation per frame.

This is on the hottest path in the system: the adapter deserializes one
`ServerMessage` per trade tick, and the venue is meant to run accelerated. If the
tick path ever shows up in a profile, this is where the time is. Options, roughly
in increasing payoff: hand-write `Deserialize` for `ServerMessage` peeking the tag
off a `RawValue` map (keeps the wire byte-identical, which matters because several
tests pin exact JSON text), or move to a binary framing entirely. Not profiled -
this flags the structural cause, not a measured number.

Two smaller adjacent smells: `TradeTick.symbol` is a `String` allocated and
serialized per tick on a venue that serves exactly one instrument; and outbound
frames are serialized individually into `Arc<str>` rather than batched.

## 8. Seed derivation lives outside the crate its versioning rule names (low, process)

`AGENTS.md` states that "seed derivation" is a tape-protocol-affecting change that
MUST bump `mogwai_data::TAPE_PROTOCOL_VERSION`. `RunSeeds::from_run_seed` and
`DOMAIN_TAPE` live in `mogwai-protocol/src/seeds.rs`, which has no reference to
that rule or that constant - and `mogwai-protocol` does not depend on
`mogwai-data`, so no compiler or test can connect them. `seeds.rs` should carry
the pointer in a comment at minimum; the golden test
`derived_streams_differ_and_are_stable` pins the values but says nothing about the
version bump.

## 9. Reproducibility gap in the shipped launcher (low, design)

`ReadyRecord.run_seed` is documented as "the value that, with the config,
fingerprint and `version_string`, reproduces this path", and `launch` logs it
specifically so a consumer cannot lose it. But `LaunchSpec` has no `seed` field and
`mogwai serve` has no `--seed` flag (only `--config`, `--duration`,
`--launcher-pid`) - the seed is config-file-only. So a launcher that observes an
interesting `run_seed` cannot feed it back through the shipped launcher without
authoring a TOML file itself. Either `LaunchSpec` grows a `seed: Option<u64>` and
`serve` a `--seed`, or the reproducibility story should say "write it into a
config".

## Checked and found sound

`clock.rs` (the f64-precision caveat is documented and the anchors correctly stay
`u64`; `validate_sim_clock`'s note about non-finite `f64` failing to round-trip
through JSON is correct); `decimal.rs`; `RunSeeds` domain separation (splitmix64 is
a bijection, so `tape != fill` is structural, not luck); `trades_through` /
`touches_trigger` strictness and the argument for keeping them separate functions;
`ServerMessage::category` covering all variants exhaustively with `is_market_data`
correctly narrower; `ConnHavoc`'s container-level `#[serde(default)]` and the
two-sided reconnect-spin gate; `worst_case_output_bytes` matching all five
`ClientMessage` variants; the `own_venue` thread architecture (the dedicated-thread
`PR_SET_PDEATHSIG` reasoning, the readiness deadline living with the `Child` owner,
and the decision not to join the drain thread are all correctly argued and
correctly implemented).
