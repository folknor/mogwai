# Bug hunt: mogwai-server transport, admission and lifecycle

Scope: `main.rs`, `run.rs`, `http.rs`, `ws.rs`, `admission.rs`, `config.rs`,
`man.rs`, `man/`.

Hunter: Claude Opus, single coverage. Findings are a work document, not a
contract - they may be wrong.

Cross-scope: finding 1 is the fuller writeup of `bugs-protocol.md` finding 1;
finding 6 corrects `bugs-protocol.md` finding 2.

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
