# Bug hunt: mogwai-server transport, admission and lifecycle

Scope: `main.rs`, `run.rs`, `http.rs`, `ws.rs`, `admission.rs`, `config.rs`,
`man.rs`, `man/`.

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Cross-scope: finding 1 is the fuller writeup of `bugs-protocol.md` finding 1;
finding 6 corrects `bugs-protocol.md` finding 2.

## 1. `AdmissionSubject` is an unbounded client-controlled string on the priority lane - CONFIRMED, and worse than reported

`http::admission_subject` clones `client_order_id` / `request_id` verbatim.
`ExecLanes::emit_admission` rewrites only the `reason` field of `AdmissionRejected`;
`subject` passes through untouched. So the priority lane's whole bound is void: it is
a frame count (`ADMISSION_LANE_FRAMES = 64`), and its legitimacy rests entirely on the
claim in the module doc that "every frame on it is under `ADMISSION_FRAME_MAX_BYTES`"
(4096).

Two reachable paths, both from a plain client:

- `ws.rs`, the pending-act-exhausted refusal. This runs before `process_order_cmd`, so
  no boundary validation has happened at all. Requirements: an armed `CommandLatency`
  with a non-zero act ms, plus 256 in-flight commands (`PENDING_ACT_SLOTS`) or 4096
  process-wide. Then every subsequent submit carrying a 60 MiB `client_order_id`
  produces a 60 MiB `AdmissionRejected` charged as one 4 KiB slot. 64 slots x 60 MiB =
  ~3.8 GiB queued per connection against a budget that claims 256 KiB.
- `http.rs` `boundary_outcome` - the over-length id has already been detected by
  `boundary_error`, and the code then echoes it at full length into the subject. This
  is the same mistake `boundary_refusal` immediately below it exists to prevent, with a
  five-line comment explaining exactly why: "Echoing it at full length would turn an
  8 MiB `client_order_id` into an 8 MiB `OrderRejected`, recreating exactly the
  unbounded frame the cap exists to prevent." The `NotAdmitted` sibling does the thing
  the comment forbids.

Three other `http.rs` call sites are safe by accident - they sit after the boundary
check, so the id is at most 64 bytes there. The safety is not structural; nothing in
the type system or in `emit_admission` enforces it.

The existing test `admission_reasons_are_truncated_at_the_lane` only exercises
`ProtocolError`, so the gap has no coverage.

Fix, structurally rather than by patching four call sites: truncate in
`emit_admission`'s `AdmissionSubject` arm (it already destructures the message), or
better, make `AdmissionSubject`'s constructors in `mogwai-protocol` the only way to
build one and have them truncate. The current shape - a public struct-variant enum
anyone can build with a raw `String` - is what lets a call site forget.

## 2. No inbound websocket message-size limit - CONFIRMED

`ws_upgrade` takes the `WebSocketUpgrade` extractor and calls `on_upgrade` with no
`.max_message_size()` / `.max_frame_size()` / `.max_write_buffer_size()`. Nothing else
in the crate configures them (no occurrences anywhere in `crates/`). axum 0.8's
defaults are tungstenite's: 64 MiB message, 16 MiB frame. Every frame is fully
buffered by tungstenite before `serde_json::from_str` sees it, and then the parsed
`ClientMessage` is cloned into a spawned task (see finding 3). A `MAX_CLIENT_ID_LEN`
of 64 bytes is enforced after the venue has already materialized a 64 MiB `String`.

Given the venue is loopback-only and one-client-per-run this is not the top risk, but
the 64 MiB ceiling is what makes finding 1 severe, and it is set by a dependency
default nobody chose. A `max_message_size` on the order of 64 KiB is generous for this
protocol (largest legal `ClientMessage` is a submit: a few hundred bytes).

## 3. The inbound command path has no backpressure whatsoever - the largest hole in this scope

`ws.rs`: `tokio::spawn(dispatch_command(command, state.clone(), lanes.clone()))`,
unconditionally, once per inbound text frame. The read loop never awaits anything the
command path controls.

- The `act_budget` / `pending_acts` semaphores in `dispatch_command` are acquired only
  when `act_ms > 0`. With no `CommandLatency` armed - i.e. the default configuration -
  every bound is skipped and a client can spawn unbounded tasks by pipelining frames.
- Each spawned task holds the `ClientMessage`, and then issues a `spawn_blocking` for
  `market_reading`, then queues on `run.engine.lock()`. So a burst of N commands
  produces N live tasks, N blocking-pool submissions, and N waiters on the engine
  mutex.
- The admission byte budget does not help: `lanes.reserve()` happens after the engine
  lock is taken, i.e. after all the queueing cost has already been paid.

The module doc in `admission.rs` claims "Every producer that runs on the socket read
loop reserves worst-case capacity BEFORE the engine is allowed to mutate, refuses
visibly when the reservation fails, and never awaits a full channel." The first and
third clauses hold; the missing one is that nothing bounds how many producers exist.
The whole admission design bounds output and leaves input unbounded.

The hunter would restructure this rather than add a semaphore. The per-connection
command path should be a single sequential task draining a small bounded queue fed by
the read loop, with the read loop refusing (priority-lane `AdmissionRejected`) when the
queue is full. That gets you the bound and fixes finding 4 in the same move. The
`act_budget` / `pending_acts` pair then becomes redundant machinery that can go away -
its only job today is bounding the fan-out that a sequential dispatcher structurally
cannot create.

## 4. Command arrival order is not preserved - a real determinism hazard on the replay axis

Because each command is a detached task, the order in which commands reach
`engine.process_with_market` is not the order they arrived on the socket. Two
independent reorderings:

1. `market_reading` goes through `spawn_blocking`; two tasks complete in whatever order
   the blocking pool returns them.
2. `dispatch_command`'s act sleep uses `run.act_ms(class)` read per command at dispatch
   time, and the classes have different values, so a `Cancel` with `cancel_act_ms = 0`
   overtakes a `Submit` with `submit_act_ms = 500` that arrived first. That one is
   arguably intended (act latency is a modeled divergence), but path (1) is not modeled
   anything.

Mitigating: `draw_trigger`/`draw_key` in `mogwai-engine/src/orders.rs` is a pure
function of the order's own fields plus `fill_seed`, so the fill band draw is
order-independent. That is real insulation and it is why this has not bitten yet.

Not mitigated: `ts_event` (each task re-samples `sim_now_ns` three times), venue order
id allocation, netting position sequencing, and balance sequencing. A cancel that races
ahead of its own submit produces `OrderCancelRejected` where a sequential venue
produces a clean cancel. Under `speed = 10` and a loaded blocking pool this is not
exotic.

Confidence: high that the reordering exists; medium on how much observable divergence
it produces today, precisely because of the pure-draw insulation.

## 5. A client that connects after the run completes is never told, and never closed

`main.rs` spawns the deadline task: `completing_run.complete(...)` then
`stop_tx.send(true)`. The `main.rs` comment asserts the invariant:

> The deadline task announces completion on every open socket and only then stops the
> accept loop, so no connection is accepted after the announcement it would never see.

The ordering achieves the opposite of what the comment claims. `handle_socket` does
`let mut completion = state.run.completion()` and then only ever awaits
`completion.changed()`. `watch::Sender::subscribe` marks the current value as seen, so
a receiver created after `complete()` fired will never observe it. Any connection
accepted (or whose `/ws` upgrade task is scheduled) in the window between `complete()`
and axum actually ceasing to accept gets a socket that emits no `RunComplete`, sends no
close frame, and hangs until `SHUTDOWN_GRACE` force-exits the process - at which point
it is a bare TCP teardown, exactly the outcome `RunComplete` exists to distinguish from
a crash.

Trivially fixed by checking `*completion.borrow_and_update()` once before entering the
loop. The ordering in `main.rs` should also be inverted (stop accepting, then announce)
if the comment's invariant is the one actually wanted.

## 6. `--duration 0s` does not do what `docs/cli.md` says

`main.rs` maps `Some(0)` to `None`, then does
`duration_override_ns.or_else(|| (cfg.run_duration_ns != 0).then_some(cfg.run_duration_ns))`.
So `--duration 0s` is not "no declared completion" - it is "fall back to the config
file". With `run_duration_ns = 600_000_000_000` in `run.toml`,
`mogwai serve --config run.toml --duration 0s` runs for ten minutes.

`docs/cli.md` states flatly: "`--duration DURATION` overrides `run_duration_ns` for
this invocation, and `--duration 0s` means what `run_duration_ns = 0` means - NO
declared completion, run until the launcher ends it." Both halves are false in the
presence of a non-zero config duration. The `main.rs` comment makes the same wrong
claim about itself.

(The `mogwai-protocol` hunter's framing - "`--duration 0s` decodes to `None`, meaning
run forever" - is itself imprecise: it only runs forever when the config duration is
also zero.)

The `.filter()` and the `.or_else()` are fighting each other. Correct shape is a
three-state override: `Option<Option<u64>>`, where `Some(None)` is an explicit
"indefinite" that beats the config.

## 7. `ADMISSION_LANE_FRAMES` arithmetic in its own doc comment is wrong

`admission.rs`: "64 x `ADMISSION_FRAME_MAX_BYTES` = 512 KiB, a real bound."
`ADMISSION_FRAME_MAX_BYTES` is 4096, so 64 x 4 KiB = 256 KiB, not 512 KiB. Cosmetic on
its own, but it is the sentence that justifies using a frame count instead of a byte
budget, and it is off by 2x - worth correcting alongside finding 1, since a fix to
finding 1 is what makes the number mean anything.

## 8. `/quotes` is missing the off-tape floor refusal that `/trades` has

`trades` refuses `start < data_origin` with a 400 and a named floor. `quotes` has only
the sim-now ceiling check; the origin floor is absent. Today `TAPE_ORIGIN_NS == 0`
makes both unreachable, and the `/trades` comment explicitly keeps its branch alive for
the day the origin moves - at which point `/quotes` will silently serve an empty 200
that a warmup cannot distinguish from "no quotes happened", which is precisely the
failure mode the `/trades` comment describes.

## 9. History endpoints and order entry share one blocking pool and one per-symbol index lock

`/trades` and `/quotes` are unauthenticated, uncapped in concurrency, and each
`spawn_blocking`s a synthesis of up to `MAX_HISTORY_LIMIT = 50_000` ticks (~7 MB of
JSON per `mogwai-protocol/src/lib.rs`) buffered whole into a `Vec` before
serialization. The tokio runtime is built with `new_multi_thread().enable_all()` and no
`max_blocking_threads`, so the default 512 applies: ~3.5 GB resident at saturation.

Worse than the memory: `build_history_source` -> `source_at_or_before` takes the same
per-symbol index mutex that `market_reading` takes on the order-entry path, and a run
is one instrument, so it is one lock. A stream of `/trades` requests serializes order
entry behind history synthesis. The comment in `http.rs` acknowledges the coupling ("no
longer than the symbol's shared index itself requires") but frames it as bounded; with
one symbol it is not bounded by anything but request rate.

This is loopback-only, so it is not an internet-facing DoS - but a misbehaving or
looping consumer wedges its own order path, and the symptom (order acks stop arriving)
looks nothing like the cause.

## 10. Lesser observations

- `speed` is validated after the warmup burn. `build_run_clock` (the only place `speed`
  is checked, `config.rs`) is called after the `materialize_warmup` `spawn_blocking`.
  `speed = -1` or `speed = nan` fails boot only after minutes of warmup synthesis.
  `Config::load` runs four validators; this one should join them. `warmup_ns`,
  `server_heartbeat_ms`, `zero_speed_stall_ms` and `run_duration_ns` get no validation
  at all.
- Binary websocket frames are silently ignored. `ws.rs` `Some(Ok(_)) => {}` swallows
  `Message::Binary`. A client sending JSON as binary gets neither a `ProtocolError` nor
  a close - it just sees its orders vanish. Given the venue's honest-content posture,
  that should be a `ProtocolError`.
- `serve_until_drained` returns `Ok(())` on the forced-exit branch. A venue that
  abandoned undrained connections exits 0, indistinguishable from a clean drain to a
  launcher inspecting exit status. Only the WARN log distinguishes them.
- A signalled (as opposed to completed) venue sends no `RunComplete` and no close
  frame. Only the deadline task calls `run.complete()`; `shutdown_signal()` goes
  straight to axum's graceful shutdown. Sockets are torn down bare. `docs/cli.md`
  covers this ("otherwise the launcher terminates and reaps it") so it may be intended,
  but the asymmetry is undocumented in the code.
- `refuse_unfunded_settlement` iterates `defs`, which is always one element (one
  instrument per run, enforced by `ConfiguredInstrument` being a table not a list). The
  loop is dead generality.
- `ActDelay` is a single-variant enum (`Paid`) threaded through `process_order_cmd` as
  `_act_delay`, unused. It was a proof marker for the two-carrier era; with
  websocket-only order entry there is one caller and nothing to prove. Delete it.

## What the hunter would rewrite rather than patch

Items 1, 3 and 4 are one structural problem wearing three hats: the read loop treats
each frame as an independent unit of work and hands it, unvalidated and unaccounted, to
a detached task. Replacing the per-frame `tokio::spawn` with a single per-connection
sequential dispatcher fed by a small bounded queue fixes the backpressure hole and the
ordering hazard together, and it collapses `act_budget` + `pending_acts` +
`PENDING_ACT_SLOTS` + `GLOBAL_PENDING_ACT_SLOTS` and two config knobs into nothing.
Separately, `AdmissionSubject` should stop being constructible from a raw `String` -
that is the only change that makes the priority lane's frame-count bound honest by
construction rather than by four call sites remembering.
