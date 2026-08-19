# Bug hunt: mogwai-protocol

Reconnaissance report, 2026-08-18. One Opus hunter, read-only, scope
`crates/mogwai-protocol`: the wire types, `control::Divergence`, and `launch`.

Not verified by the orchestrator. Findings may be wrong; the fix pass decides.
Confidence labels are the hunter's own.

Round 1 (2026-08-19) closed G and J and round 2 closed H and I; those sections
are deleted rather than annotated, per the discrepancies-doc rule. Round 3
closed E and F and REFUSED C with measurement; those three keep their sections
because two of them changed the launcher's public surface and the third is a
refusal, which stays with its reasoning. What they cost and what binds later
work is in `notes/bug-loop-carry-forward.md`.

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

## C. Launcher: `Drop` blocks a runtime worker - REFUSED with measurement, 2026-08-19

The finding's mechanism is FALSE and the recommended structural rewrite would
have been paid for nothing. `LaunchedVenue::drop` does not wait out
`OWNER_POLL`: `terminate` drops the shutdown SENDER, which DISCONNECTS the
channel, and `Receiver::recv_timeout` returns `Err(Disconnected)` at once rather
than at the end of the interval. Shutdown latency is ALREADY a wakeup. The poll
interval bounds only how late the owner notices a venue that ended ON ITS OWN,
which is not on any teardown path.

MEASURED, against a scripted venue held past two completed poll windows so the
owner was provably parked mid-interval: 314 us and 258 us for the whole drop -
signal, kill, reap, join - against an `OWNER_POLL` of 200 ms. Three orders of
magnitude below the claim.

The reachability half was checked too, and it does not rescue the finding.
`mogwai-adapter` NEVER HOLDS A `LaunchedVenue` - it re-exports `launch` and
nothing more, so no client of the venue drops one. The real consumer is
broadarrow's `ba-worker`, which does hold one across an async run on a
`current_thread` runtime and calls `shutdown()` from `async fn run` at the end.
So a drop on a reactor thread is genuinely reachable; it just costs sub-
millisecond, at teardown, after the node has stopped.

What was owed and is now paid: the property was undocumented and INVISIBLE. An
owner loop rewritten as `sleep(owner_poll); try_recv()` reads as an equivalent
refactor and passes every other test in the module.
`tearing_down_a_healthy_venue_is_a_wakeup_not_a_poll_interval` pins it, and its
SHAPE was the round's cold-review find. The first cut ran at the production
`OWNER_POLL` of 200 ms and asserted teardown under a quarter of it - a 50 ms
wall-clock budget in the parallel dev lane, which is exactly what
`tape_lateness_under_acceleration` was deleted for (50 ms asserted, 311 ms
measured in RELEASE under a load average of 1.46), and `brokkr.toml`'s policy
for that class is `#[ignore]` plus the `timing` sweep. Neither routing was
taken: the test WIDENS THE SIGNAL instead, running the owner at a ten-second
interval so the two outcomes are sub-millisecond and ten seconds, with the bound
at two - four orders of magnitude above the honest cost, a fifth of the defect,
and the same loose-upper-bound class as the module's existing 5 s and 10 s
assertions. It also asserts `completed_polls() == 0`, which is the clock-free
half: no window expired, so the reap was the shutdown arm's. Bite-checked by
rewriting the owner loop as `sleep(owner_poll); try_recv()`: fails at 9.951 s and
9.950 s across both sweeps. `LaunchedVenue`'s doc and `docs/cli.md`
now state the real cost, the two clocks, and the one thing no launcher can bound
(a venue that refuses `SIGKILL`).

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

## E. Launcher: the venue is forbidden from writing to stdout - FIXED 2026-08-19

The mechanism was real and is now MEASURED rather than argued. FIXED by
DRAINING, which was the choice between the finding's two options, and the reason
is that one of them removes the requirement and the other only documents it.

Stating the prohibition on the venue side buys a test that fires when someone
adds a `println!` to the serve path - but the rule it would enforce exists only
because the launcher chose to close a pipe, it constrains a whole crate forever
in service of one library's convenience, and it is enforceable only over code
inside this workspace. A venue is a process; a launcher that cannot survive it
writing to its own stdout is the defective party. So the readiness reader now
`io::copy`s stdout into `io::sink()` after sending the record, to EOF, which
arrives when the venue dies. Cost: one thread per venue for the run, exactly the
shape and lifetime the stderr drain already has. The module doc's "the only
thing it ever writes there" is a DESCRIPTION again.

The reader is consequently never joined on ANY path. It was already unjoined on
the timeout arm (finding D), so the `reader_is_released` flag and its branch are
gone; `read_ready` takes `&mut impl Read` so the handle survives the call.

`a_venue_that_writes_to_stdout_after_readiness_is_not_broken` pins it, and the
bite-check is the finding's own mechanism executing: with the drain replaced by
`drop(stdout)` the `/bin/sh` fixture - which does NOT ignore `SIGPIPE` - dies of
the signal, and the test fails on `VenueExit { success: false, code: None }`
against the `Some(0)` the script asked for, both sweeps. That is direct
observation of the EPIPE-mid-run failure the finding described.

## F. Launcher: `LaunchError::Spawn` for an internal thread failure - FIXED 2026-08-19

FIXED with the variant the finding asked for. `LaunchError::Thread { what:
&'static str, source }`, where `what` is the thread's ROLE - "owns the venue for
the run", "reads the venue's readiness line" - and cannot be mistaken for a
path. `Display` places the limit locally ("a limit of THIS process - threads,
memory or an RLIMIT - and not a problem with the venue binary or its
configuration") and `source()` reports the io error.

THE MESSAGE USED TO END "nothing was spawned", AND THAT WAS FALSE AT ONE OF THE
TWO SITES - cold review caught it. It holds for the owning thread, which is
created before any `Command::spawn`; the readiness reader is created INSIDE
`own_venue` with a venue process already running, so the sentence told the
operator no venue had started when one had, in a change whose whole point is
that the message must not send them after the wrong remedy. It now says "no
venue is left running: one already started for this launch has been killed and
reaped", which is true at both - the reader site falls through to the same
unconditional kill-and-reap every other failure path in `own_venue` reaches, and
the owning-thread site never spawned one.

NOTHING BROKE, AND THE NEXT VARIANT CANNOT. Adding a variant is a public API
change, so the readers were checked: `mogwai-cli`'s `lifecycle.rs` matches only
`NoRecord` and `Timeout`, neither exhaustively; broadarrow's `ba-worker` and
`run-prep` render the error through `Display` and never match a variant. That
grep is manual, unrepeatable and blind to any consumer not vendored here, so
`LaunchError` is `#[non_exhaustive]` now - the one-line durable fix, free while
the crate is pre-1.0, which the first pass had left on the table.

`a_failed_launcher_thread_does_not_blame_the_binary` pins THE RENDERING, and the
first pass recorded a bite-check that cannot have run: it claimed restoring the
`Spawn { binary: "<owning thread>" }` CONSTRUCTION failed the test, but the test
builds `LaunchError::Thread` literally and never calls `launch`, so a call-site
revert leaves it green. The honest statement, and the check that was actually
run: reverting the `Display` ARM to the old `Spawn` wording fails it on "a
thread failure must not send the operator after a binary path", both sweeps.
WHAT NO TEST HOLDS is the coupling - that the two thread-creation sites raise
this variant - because reaching either branch means driving the process to its
thread limit. That is held STRUCTURALLY instead, and this round made it so:
`spawn_launcher_thread` is now the module's only thread-creation call and the
only constructor of `LaunchError::Thread`.

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

## L. `Symbol = Arc<str>` buys nothing on decode - REFUSED with measurement, 2026-08-19

The MECHANISM IS TRUE AND UNDERSTATED, and the change is still refused. Both
halves of the recommendation - the allocation and the type-level bound - were
measured or audited rather than argued, and neither carries the change. The
type-level half did, however, name a REAL GAP that this round closed by other
means; see the correctness half below.

THE ALLOCATION HALF, measured by
`crates/mogwai-protocol/examples/symbol_decode_probe.rs` (2,000,000 iterations,
release, host `bygg`, run three times; the numbers and their reading are in
`reference/performance.md`):

| arm | ns / frame | allocations / frame |
|---|---:|---:|
| landed `ServerMessage::from_json_str` | 239, 220, 219 | 2 |
| payload struct, `Symbol = Arc<str>` (today) | 110, 115, 109 | 2 |
| payload struct, inline 32-byte `Copy` symbol | 103, 111, 103 | 0 |
| `mogwai_adapter::convert::trade_id`, ONE trade | 162, 154, 153 | 5 |

The hunter said ONE allocation; it is TWO - serde takes a `String` and then
copies into a fresh `Arc`, and the inline type removes both. So the finding
undersold its own mechanism, and the wall it buys is 4 to 7 ns per frame.

AN EARLIER TABLE HERE SAID 19 ns, and the cold review found the probe biased in
two directions at once: only the inline arm's `len` field was observed (so its
32-byte copy could be elided) and only the inline arm validated. Both arms now
`black_box` the whole decoded tuple and neither validates, so the delta is
representation only - and the alphabet check the proposal would add is a cost
NOT counted, making the figure an upper bound on the saving.

WHAT REFUSES IT IS THE FOURTH ROW. The adapter's socket reader is the ONLY
decoder of `ServerMessage` in existence - `from_json_str` and `from_json_slice`
have exactly one call site each, both in `lifecycle.rs`'s read loop - and the
first thing it does with a decoded trade is `convert::trade_id`, which
`format!`s all five fields and costs ~155 ns and FIVE allocations, before
`handler().await`, the nautilus event construction, the tungstenite framing and
the `Message::Text` `String` the frame already arrived in. Five is a floor: the
probe omits `TradeId::new_checked`, which interns the string. A ~5 ns saving
inside a ~220 ns decode whose immediate consumer spends ~155 ns and 5
allocations is not a per-tick win anyone can observe. At the paced rate the venue
actually serves (measured elsewhere in this arc: MNQ prints ~89 content frames
in a declared 2 s) the saving is well under a microsecond per second of trading.

THE `ServerMessage` INVENTORY IS NARROW ON PURPOSE AND MUST BE READ THAT WAY.
Symbols also reach the adapter through five UNTAGGED HTTP decodes that are not
`ServerMessage` and validate nothing: `client/shared.rs` (instruments), two in
`client/data.rs` (trades and quotes), `client/exec.rs` and `clock.rs`. Same
deliberate posture as the decode bullet below, but a claim about "the decoders"
has to name them.

THE CORRECTNESS HALF - `MAX_SYMBOL_LEN` as a property of the type rather than a
validator someone must remember to call - is the stronger argument, and the
first audit of it OVERSTATED WHAT IT HAD CHECKED. It claimed three kinds of
ingress, all validating. There are four kinds, they check three different
things, and ONE OF THEM WAS A LIVE GAP, closed in this round's commit:

- ORDER ENTRY. `SubmitOrder.symbol` is the client-inbound symbol
  (`Subscribe` was retired with the subscription model, and
  `client_and_server_messages_round_trip` pins the decoder's refusal of it),
  and `ClientMessage::SubmitOrderGroup` carries up to `MAX_GROUP_ORDERS` more,
  which reach the same validator through `validate_submit_group`.
  `http::boundary_error` - the ONE gate the websocket order carrier uses for
  every command, and the only caller - runs `validate_submit_order`.
  THAT VALIDATOR CHECKED ONLY `symbol.len() > MAX_SYMBOL_LEN`, so the EMPTY
  string and any byte outside the wire alphabet were admitted here while this
  section said the ingress was closed. It runs `validate_wire_symbol` now,
  pinned over both carriers by
  `an_order_entry_symbol_is_judged_by_the_wire_alphabet`. The `sizing.rs` row
  bounds only ever needed the length, so nothing downstream loosened; nothing
  in this tree submits a symbol the alphabet refuses (audited: `MNQ`,
  `BTCUSDT`, `BTCUSDT.P`, `BTCUSD-INV`, `XBTUSD`, `EURUSD`, `AAPL`), and an
  unknown symbol was already refused by the engine's instrument lookup.
- URL-CARRIED SYMBOLS. `http.rs` and `source.rs` each call
  `validate_wire_symbol`.
- CONFIG INSTRUMENTS. `config.rs` does NOT - it validates an instrument's
  `index_symbol` with `validate_wire_symbol` and its own `symbol` with only a
  non-empty check and `MAX_SYMBOL_LEN`. So a configured instrument may carry a
  symbol order entry now refuses. Operator-supplied rather than client-supplied,
  and a `mogwai-server` decision, so it is filed in `notes/todo.md` rather than
  tightened from this document.
- THE ADAPTER'S DECODE. Here a symbol genuinely is unvalidated, and the adapter
  handles it DELIBERATELY AND FALLIBLY: `convert::instrument_id` uses
  `NautilusSymbol::new_checked` precisely because "`mogwai_protocol::Symbol` is
  an unvalidated `Arc<str>` straight off the wire", with a doc comment saying
  so and giving the reason (the conversion runs in an unsupervised spawned
  task, so a hostile symbol must drop one frame rather than the task). A
  validating `Deserialize` would move that refusal from the conversion to the
  frame - a BEHAVIOUR CHANGE to the adapter's hostile-venue handling, not the
  closing of a hole.

So the type-level bound would have caught the order-entry gap for free, which is
the honest point in its favour and the reason the finding is only PARTLY refused
- the change is refused, the concern behind it was real. What closed the gap was
a one-line call to the validator that already existed. What the type would have
cost is a mechanical workspace-wide edit across the wire
types, the engine, the server, the data crate and the adapter, plus a new
hand-written `Deserialize`, `Display`, `Borrow<str>` and serialization surface
for a type `Arc<str>` gives for free. That is the fourth structural
recommendation this arc has refused on a measurement (after the shared-venue
split, the `StubState` split and the per-case slack ceiling).

NO `TAPE_PROTOCOL_VERSION` BUMP IS OWED: nothing in the tape-generation path was
touched, and the probe is an example target.

WHAT THE ROUND OWES AND PAID, per the arc's standing rule that a refused finding
usually names a property nobody had written down: the probe is kept, and
`reference/performance.md` carries the four-arm table with the reading. The next
reader who notices the `Arc` finds the number instead of re-deriving the
argument. It is deliberately NOT registered as a `brokkr mogwai` target - it
settles one decision rather than opening a series - and `brokkr.toml` says so
beside the other deliberate non-registration.

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
