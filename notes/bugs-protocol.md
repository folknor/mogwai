# Bug hunt: mogwai-protocol

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-protocol`: the wire types, `control::Divergence`, and `launch`.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

The hunter read the whole crate (`launch.rs`, `messages.rs`, `control.rs`,
`havoc.rs`, `sizing.rs`, `ready.rs`, `clock.rs`, `decimal.rs`, `lib.rs`) plus
the two things it contracts against outside itself (`mogwai-cli`'s `serve` argv
parsing and `mogwai-server`'s `arm_parent_death_signal`), to check the
launcher's claims rather than assume them. No edits made.

## A. Launcher: a natural exit racing shutdown is silently destroyed

`own_venue`, the parking loop:

```rust
loop {
    match shutdown_rx.recv_timeout(OWNER_POLL) {
        Ok(()) | Err(RecvTimeoutError::Disconnected) => break,   // skips try_wait
        Err(RecvTimeoutError::Timeout) => {}
    }
    if let Ok(Some(status)) = child.try_wait() { /* record exit */ break; }
}
drop(child.kill());
drop(child.wait());
```

The shutdown break jumps straight past the `try_wait` that is the ONLY place
`VenueExit` is ever written. So if the venue completed its declared run
(announced `RunComplete`, exited 0) at any point inside the current 200 ms or
shorter poll window, and the caller drops or `shutdown()`s the handle in that
window, the exit status is thrown away: `exited()` returns `None` forever and
the child is `kill()`ed as though it were still healthy.

This matters precisely because `VenueExit`'s own doc says its reason for
existing is that "a venue given a duration ends itself... That is a SUCCESSFUL
run rather than a death, and telling the two apart is why this is not just 'is
it alive'." A launcher that runs a fixed-duration venue and then tears down -
the normal shape - has a 200 ms window on every run where it cannot tell a
completed run from a killed one. The fix is one `try_wait` before the `break`,
or moving the `try_wait` above the `match`.

Confidence: high. This is a frontier-family shape in the sense `AGENTS.md` names
- a terminal-state record that is only written on one of two paths out of the
loop.

## B. Launcher: `shutdown()` cannot report the thing its doc says it exists to report

```rust
/// `Drop` does the same and ignores the outcome, which is right for an
/// unwinding path and wrong for an orderly shutdown that wants to report
/// that the venue would not die.
pub fn shutdown(mut self) -> Result<(), LaunchError> { self.terminate() }
```

`terminate()` only ever returns `Err(OwnerDied)` - an owning-thread PANIC. The
venue-would-not-die case lives in `own_venue`, where both `child.kill()` and
`child.wait()` are `drop(...)`ed unconditionally, and `own_venue` returns `()`.
There is no channel back. So `shutdown()` returns `Ok(())` for: a kill that
failed with EPERM, a `wait()` that errored, and a child that was already a
zombie of a wedged uninterruptible state. The stated contract is not kept; the
only difference between `shutdown()` and `drop` today is that one surfaces a
launcher-internal panic.

Either give the owner thread a result channel carrying the reap outcome, or
delete the claim from the doc. The hunter would give it the channel -
`shutdown()` returning a typed result is the whole reason it is separate from
`Drop`.

## C. Launcher: `Drop` blocks a runtime worker, in a module whose premise is async hosts

The module doc's first load-bearing property is about async applications ("in an
async application, spawning from a pool task is both the natural thing to write
and the wrong thing to write"). But `LaunchedVenue::drop` calls `terminate()`
calls `owner.join()`, and the owner can be sitting in
`recv_timeout(OWNER_POLL)` - so dropping the handle blocks the calling thread
for up to 200 ms plus the child's `wait()`. Dropped on a tokio worker (which is
where a nautilus host will drop it), that stalls the reactor. Nothing in the
docs warns about it, and unlike the spawn hazard it cannot be fixed by the
caller without knowing the internals.

The clean fix is structural: have the owner park on a channel with no timeout
and learn about a natural child exit via SIGCHLD or a dedicated waiter, so
shutdown latency is a wakeup rather than a poll interval. Given pre-1.0, the
hunter would do that rather than shrink `OWNER_POLL` and call it addressed.

## D. Launcher: the readiness wall-clock guarantee is conditional on no descendant holding stdout

`the_ready_bound_returns_on_time_against_a_silent_venue` pins that `launch`
returns on its own bound. It holds because on timeout the owner does
`child.kill()` - which closes the child's stdout write end and releases the
reader - and then `reader.join()`.

But `kill()` reaches the direct child only. Any descendant that inherited the
stdout pipe keeps the write end open, `read_until` never returns, and
`reader.join()` at the end of that arm blocks FOREVER - reproducing exactly the
"reported its timeout, then never returned" failure the test was written to
prevent, one process-layer deeper. The test's own fixture is written as
`#!/bin/sh\nexec sleep 60\n`, and the comment explains the `exec` was chosen
because the stock binaries reject the argv - so the fixture dodges the hazard by
accident rather than covering it.

Today `mogwai serve` forks nothing, so this is latent. It stops being latent the
moment anyone points `LaunchSpec::binary` at a wrapper script without `exec`, or
the venue gains a helper subprocess. Robust forms: put the child in its own
process group and `killpg`, or make the reader's `stdout` handle droppable from
the owner thread so the read is released regardless of who holds the writer.

Confidence: high that the mechanism is real, medium that it is reachable in
current use.

## E. Launcher: the venue is now forbidden from ever writing to stdout again, and nothing says so

After the readiness line is read, the reader thread's `stdout` handle is
dropped, closing the read end. Any subsequent stdout write from the venue gets
EPIPE (Rust ignores SIGPIPE). The module doc states the venue "writes exactly
one line of JSON to stdout, and that is the only thing it ever writes there" as
a description of current behaviour - but it is in fact now a HARD REQUIREMENT
IMPOSED BY THE LAUNCHER, and the venue side has no test pinning it. A future
`println!` in the serve path becomes an EPIPE-on-write failure mid-run with a
cause nobody will find. Worth either keeping the pipe drained-and-discarded for
the run, or stating the prohibition on the venue side where a violator would
read it.

## F. Launcher: `LaunchError::Spawn` for an internal thread failure renders as user-hostile nonsense

Two sites construct `Spawn { binary: OsString::from("<owning thread>") }` and
`"<readiness reader thread>"`. `Display` then produces: "could not spawn the
venue binary \<owning thread\>: ... If the binary is elsewhere, point the
launcher at it - whatever your launcher calls that setting, it ends up as
LaunchSpec::binary". The operator is told to reconfigure a binary path in
response to EAGAIN on thread creation. This deserves its own variant.

## G. `format_duration` - checked, and it holds (with one caveat)

The hunter chased this because `format_duration` emits `ms` and `ns` units and
`0s`, and `mogwai-cli`'s `parse_duration` in `gen.rs` accepts only
`s m h d w mo y`, rejects `count == 0`, and would read `"1500ms"` as
unknown-unit `"ms"` (it has `"mo"`, not `"ms"`). That would have broken every
non-second duration and the documented `Some(Duration::ZERO)` override.

It does not bite, because `serve` uses `humantime::Duration` via clap, not
`gen`'s parser - humantime takes `ms`, `ns` and `0s`. But the two duration
grammars in one binary differ on exactly the units the shipped launcher emits,
and nothing pins that `serve`'s parser is the humantime one. If `serve` is ever
switched to the in-house parser for consistency, the launcher breaks silently on
`Duration::ZERO`, every millisecond value, and every nanosecond value. A test
asserting `parse(format_duration(d)) == d` across the same cases
`durations_render_in_the_coarsest_exact_unit` enumerates would close it; right
now `format_duration`'s claim ("every value renders into something the venue
accepts") is asserted only against itself.

The `u64::try_from(nanos)` fallback to `{}s` is fine - humantime accepts
`u64::MAX` seconds.

## H. `validate_submit_order`: two prose contracts the code does not keep

- The `post_only` field doc says "Legal on Limit and StopLimit only." The code
  allows `Limit` plus `rests_after_trigger()` =
  `StopLimit | LimitIfTouched | TrailingStopLimit`. The doc is two types short.
- The refusal message reads "post_only is legal only on orders that rest as a
  limit", but `MarketToLimit` - whose entire documented behaviour is "take what
  is available at the touch and REST THE REMAINDER" as a limit - is refused by
  it. The rule may well be right; the stated reason is false for that one type,
  which is the kind of thing that gets "fixed" wrongly later by someone reading
  the message.

## I. `MarketToLimit` plus `Ioc`/`Fok` is accepted at the wire gate and is self-contradictory

`MarketToLimit`'s doc argues it exists precisely because "an IOC market cannot
[rest its remainder], since IOC cancels its remainder instead of resting it."
Yet `validate_submit_order` only refuses `Ioc` and `Fok` on `is_conditional()`
types, and `MarketToLimit` is not conditional. So the wire admits an order whose
type says "rest the remainder" and whose TIF says "cancel the remainder", and
the crate has no statement of which wins. Either refuse the combination here or
state the precedence. Medium confidence this is worth a refusal rather than an
engine-side rule - the argument the type's own doc makes is the argument for
refusing it.

## J. Decimal decode is asymmetric with encode, unremarked

`rust_decimal` with `serde-with-str`: every `Decimal` SERIALIZES as a JSON
string (`"quantity":"2"`, pinned by `client_and_server_messages_round_trip`),
but the default `Deserialize` also accepts JSON NUMBERS, going through `f64`. So
a client sending `{"quantity": 0.1}` or a 20-significant-digit number gets a
silently f64-rounded price or quantity, while a client sending `"0.1"` gets the
exact decimal. On a venue whose whole point is exercising a live execution path,
a price that decodes differently depending on how the peer spelled it is a real
hazard, and it is nowhere documented - the byte-form round-trip test only
exercises the string spelling. Either add
`#[serde(with = "rust_decimal::serde::str")]` on the wire fields to refuse
numeric spellings outright, or pin the numeric-acceptance behaviour with a test
so it is a chosen contract rather than a dependency default.

Confidence: high on the mechanism, medium on whether the project considers
tolerance a bug. The hunter would refuse numbers - this is the "single source of
truth both ends serialize against", and one-of-two-accepted-spellings with
different precision is not a source of truth.

## K. `sizing.rs`: two bounds have derivations but no run derivation

`order_event_bound_covers_both_maximal_lifecycle_frames` exists specifically
because the module doc's claim is that EVERY constant carries a field-by-field
derivation, and both halves of `ORDER_EVENT_MAX_BYTES` had drifted from theirs.
But `ORDER_STATUS_ROW_MAX_BYTES` and `FILL_ROW_MAX_BYTES` - the two rows that
multiply by `open_orders + closed_orders` and `recorded_fills`, i.e. the two
whose failure scales - have no such test. `ORDER_STATUS_ROW_MAX_BYTES` computes
to 1856 against a maximal `OrderStatusInfo` the hunter hand-counts at roughly
1760: it holds, with about 5 percent headroom, and the next field added to
`OrderStatusInfo` voids it with nothing to catch it. `FILL_ROW_MAX_BYTES`'s
comment also says "rounded to 320" while the constant is 384 - the comment was
not updated with the value, which is the same drift the existing test was
written to stop.

Construct both maximal rows the way the `OrderFilled` and `OrderRejected` test
does. This is the cheapest possible extension of a test that already exists.

Related, smaller: `ADMISSION_FRAME_MAX_BYTES`'s doc says the figure is derived
from `JSON_ESCAPE_FACTOR * (MAX_CLIENT_ID_LEN + MAX_REASON_LEN + MAX_SYMBOL_LEN)
+ ADMISSION_ENVELOPE_BYTES`, but `AdmissionRejected` carries no symbol and the
test's `analytic` correctly omits it. Stale term in the prose.

## L. Structural and performance opportunity: `Symbol = Arc<str>` buys nothing on decode

`ServerMessage::from_json_str` exists to skip serde's internally-tagged content
buffer on the `Trade` and `Quote` hot path - a real optimization. But `Symbol`
is `Arc<str>`, and serde's `rc` feature deserializes `Arc<str>` by ALLOCATING A
FRESH `Arc` PER FRAME. So every decoded tick still does one heap allocation plus
a refcount block for a string that is one of a handful of values, 32 ASCII bytes
or fewer, validated by `validate_wire_symbol` against a fixed alphabet. The
`Arc` sharing only pays off AFTER decode, inside one process.

Given the tag-probe work already done here, the obvious next move is an inline
fixed-capacity symbol type (32 bytes, `Copy`, no allocation, `Deserialize`
straight off the borrowed str) or a decode-side interner. That removes an
allocation per tick on the one path the crate already treats as hot, and it
makes `MAX_SYMBOL_LEN` a property of the type instead of a validator someone has
to remember to call. Pre-1.0, changing `Symbol` is a mechanical workspace-wide
edit.

## M. Smell: the 50 ms sleep in the `NoRecord` path

```rust
Err(LaunchError::NoRecord { .. }) => { std::thread::sleep(Duration::from_millis(50)); ... }
```

A fixed sleep hoping the drain thread has delivered the explanation. It is racy
in both directions - too short on a loaded box, and 50 ms of dead time on every
boot failure. The drain thread is right there and the child is dead by then;
joining it (bounded) would be deterministic. It is not joined today for a stated
and correct reason (a caller callback in a destructor), but that reason applies
to `Drop`, not to this pre-report path - here the join could be bounded and the
outcome ignored.

## Nothing wrong found in

`ready.rs` (schema, version guard and byte-form pin are consistent, and
`parse_ready` correctly reads `version` off the raw JSON before trusting any
other field - the reasoning in that comment is right and the test covers both
the stale-field and both-directions cases); `control.rs` and
`validate_divergence` (bounds match the documented ceilings, degenerate values
rejected rather than armed inert); `clock.rs`; `decimal.rs`; the
`AdmissionSubject` truncating `Serialize` (the residual - an in-memory subject
may hold an over-length id - is disclosed rather than hidden, and the bound is
held where it matters, at the wire).

Checked against `AGENTS.md` and clean: no tape-generation path here, so no
`TAPE_PROTOCOL_VERSION` obligation; `launch.rs`'s guard scoping is genuinely
correct - the owner thread OWNS the `Child` for the whole run rather than merely
being alive during it, which is the exact distinction the guard-scope family
names, and the comment at the `boot_rx.recv()` explains why the deadline lives
in the owner rather than the caller. That reasoning is sound and is the
strongest part of the module.
