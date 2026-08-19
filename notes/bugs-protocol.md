# Bug hunt: mogwai-protocol

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-protocol`: the wire types, `control::Divergence`, and `launch`.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

Round 1 (2026-08-19) closed G and J and round 2 closed H and I; those sections
are deleted rather than annotated, per the discrepancies-doc rule. What they
cost and what binds later work is in `notes/bug-loop-carry-forward.md`.

The hunter read the whole crate (`launch.rs`, `messages.rs`, `control.rs`,
`havoc.rs`, `sizing.rs`, `ready.rs`, `clock.rs`, `decimal.rs`, `lib.rs`) plus
the two things it contracts against outside itself (`mogwai-cli`'s `serve` argv
parsing and `mogwai-server`'s `arm_parent_death_signal`), to check the
launcher's claims rather than assume them. No edits made.

## A. Launcher: a natural exit racing shutdown is silently destroyed - FIXED 2026-08-18

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

FIXED by the second form: the `try_wait` now runs on every path out of the loop,
with the shutdown signal recorded as a flag that breaks AFTER the reap attempt
rather than instead of it. So a bounded run that ended inside the last poll
window is recorded as the successful exit it was, and `exited() == None` after a
shutdown now means what a caller reads it as - the venue was still serving when
it was killed.

## B. Launcher: `shutdown()` cannot report the thing its doc says it exists to report - FIXED 2026-08-18

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

FIXED with the channel, for that reason. The owner thread records a failed kill
or a failed reap into a shared slot, and `terminate` reads it AFTER the join -
before the join would race the write - and returns the new
`LaunchError::Teardown`. The detail is taken rather than copied, so the `Drop`
that follows an explicit `shutdown` does not report the same failure twice. Note
what is deliberately NOT reported: a child already reaped by the loop above makes
kill and wait fail harmlessly, so the record is only kept when nothing was
reaped.

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

## D. Launcher: the readiness wall-clock guarantee is conditional on no descendant holding stdout - FIXED 2026-08-18

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

FIXED by neither of the hunter's two robust forms, and the choice is worth
stating. The timeout arm no longer joins the reader at all: the kill is still
issued, because it releases the reader in every case where the child itself holds
the write end, but the bound is now unconditional rather than contingent on that
working. A stranded reader is one leaked thread, which is the trade the stderr
drain in the same function already makes and for the same reason - the process
gets to keep its promise. `killpg` is the better answer for the venue's own
descendants if it ever grows any, and is still open; it is a bigger change and it
does not help against a wrapper's grandchild that has left the process group.
`docs/cli.md` now states the supported shape (name the binary, or a wrapper that
`exec`s it) and what it costs when a caller does not.

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

REFUSED, with measurement, by `notes/bugs-tests-engine-protocol.md` round 1
(`a3a796d`). The finding's premise - "the child is dead by then" - is FALSE:
`NoRecord` is EOF on the STDOUT pipe, and a venue that closes its own stdout or
hands it to a grandchild reaches that branch ALIVE, so a bounded join would
block rather than being deterministic.
`a_venue_that_closes_stdout_and_lives_is_still_a_prompt_boot_failure` pins it.
Do not reopen. Kept here rather than deleted because the smell reads as live to
anyone who has not seen that measurement.

Original report follows.

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
