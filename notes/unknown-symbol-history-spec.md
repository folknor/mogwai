# Implementation spec: unknown-symbol semantics for `/trades` and `/quotes`

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. Spawned from `notes/todo.md`: piece 5 of
"Landing the grand design: fourteen pieces", and the design bullets that piece
points to under "Open issues" - THE SYMBOL IS A REQUEST PARAMETER, NOT AN
IDENTITY THE VENUE OWNS (its "ALSO SLICE 1" paragraph), SYMBOL RESOLUTION IS
TOTAL, AND THE DEFAULT PRESET IS THE SHAPE CONTRACT, and THE RIVER AND THE
BOAT (for what is deliberately left to slice 2).

## The item, and the ruling it implements

`/trades` and `/quotes` today answer an EMPTY `200` for a symbol the run does
not serve, while the same two handlers go to considerable length to refuse an
off-tape window with a loud `400` precisely so an impossible request stays
distinguishable from "no trades happened". The empty page is the exact failure
the window refusals exist to prevent, arriving through the other parameter.

OWNER RULING, 2026-08-15, recorded in `notes/todo.md` piece 5: in slice 1 an
unknown symbol is a `400` NAMING THE SERVED SYMBOL. Unreachability - the
slice-2 world where every string resolves and therefore no symbol can be
unknown - is a slice-2 question and is out of scope here.

THIS IS NOT AN ADMISSION RULE, and the distinction must survive into the code
comments and the prose. AGENTS.md states that the instrument set is open and
the venue does not gate on it: `profile_for_symbol("NOPE")` is legal precisely
because an unmatched symbol serves the DEFAULT tape. What this refusal gates
on is the symbol THIS RUN BOOTED, never whether a preset exists for the
requested string. A run booted on `MNQ` refuses `BTCUSDT` even though
`BTCUSDT` is a shipped preset, and a run booted on `NOPE` serves `NOPE` off
the default tape. Every sentence this spec adds to a doc comment, a refusal
body or `docs/` says "not served by THIS RUN" and never "no preset exists",
so a future reader cannot read the item back as a gate on the instrument set.

Why the status is `400` and not `404` or `422`: the ruling names it, and the
existing two refusals in these handlers already answer `400` for a request
that names a window the venue cannot serve. A symbol the venue cannot serve is
the same kind of impossible request through a sibling parameter, and answering
it with a different status would make the handler's refusal vocabulary depend
on which parameter was wrong. One vocabulary, one status. (A stale comment in
`mogwai-adapter/src/client/shared.rs` on `ensure_on_tape` speculates about "a
server `422`"; that speculation is void and this spec corrects it.)

## Survey of the ground

Everything below was read in the tree, not recalled.

**The two handlers**, `mogwai-server/src/http.rs`, `trades` and `quotes`. Both
take `axum::extract::Query<HistoryQuery>` (`symbol: String`, optional `start`,
`end`, `limit`), admit through `admit_history` (a fail-fast semaphore, `503`
when four syntheses are already in flight), refuse an off-tape `start`, clamp
`end` to sim-now, then move `symbol` and a cloned `Arc<InstrumentProfiles>`
into a `spawn_blocking` closure that calls `bounded_trades` / `bounded_quotes`
and serializes the page. The admission permit RIDES the blocking closure - it
is moved in, not merely held across the await - because hyper drops the
handler future on client disconnect while the blocking task runs to
completion. Nothing in this spec may move the permit back out of that closure.

**The asymmetry between the two handlers is already a latent drift.** `quotes`
calls the shared helper `history_start_refusal(start, data_origin, sim_now)`;
`trades` carries the same two refusals INLINE, in two hand-written blocks with
their own `tracing::warn!` calls (`refusing off-tape trades window` and
`refusing future trades window`). The two message texts agree today by
coincidence of maintenance. This spec lands the symbol refusal in one shared
helper and, in the same landing, collapses the inline copies in `trades` onto
the existing shared helper - the duplication is precisely the drift risk the
new refusal would otherwise double.

**And the asymmetry is an observability asymmetry too.** `trades` logs both
window refusals; `history_start_refusal`, which is all `quotes` has, only
builds a string, so `/quotes` refusals are today invisible in the log. A naive
collapse of `trades` onto the helper would therefore DELETE two log lines and
level the two handlers DOWN. The de-duplication in brick 3 moves the two
`tracing::warn!` calls INTO `history_start_refusal`, so both handlers gain
them and the endpoints level UP. This is the one place where the spec's
"nothing else changes" has an exception, and it is deliberate.

**Where the empty page comes from.** `bounded_trades` and `bounded_quotes` do
`let Some(mut merged) = source::build_history_source(symbol, start, profiles)
else { return Vec::new() }`. `build_history_source` starts with
`source::index(symbol, profiles)?`, whose first line is
`profiles.get(symbol)?`. So the miss is `InstrumentProfiles::get` returning
`None`, three frames below the handler, silently converted to an empty vector.

**What the served set is, in slice 1.** `config::build_instrument_profiles`
resolves and validates EVERY shape the config can reach - the boot shape plus
every `[symbols.*]` table, funding included - and then returns
`InstrumentProfiles::from_profiles(vec![profile])` with the BOOT profile only.
So the map has exactly one entry for the life of the process. `serve.rs` takes
`profiles.instrument_defs().into_iter().next()` for `Run::instrument`, so
`state.run.instrument.symbol` is that same one key, and the readiness record's
`symbol` field is its string.

**The second gate below the profiles lookup**, which this spec does NOT
touch. `source::index` also refuses a symbol that resolves through `profiles`
but differs from the symbol `INDEX` was initialized with at warmup, returning
`None` the same silent way. In slice 1 those two sets are the same single
symbol, so the profiles lookup is a complete guard. In slice 2 they diverge
and the `INDEX` refusal becomes a distinct failure needing its own answer;
that is piece 7's work, named here so it is excluded rather than forgotten.

**Case.** Preset RESOLUTION is case-insensitive (`config` resolves `mnq` to
the MNQ preset, pinned by a test), but `InstrumentProfiles::get` is an exact
`HashMap<Symbol, _>` lookup. The guard must therefore be the EXACT SAME
lookup the synthesis performs, never a case-insensitive comparison against the
served name: a case-insensitive guard would pass `mnq` through to a
`bounded_trades` that still misses, restoring the empty `200` this item
exists to kill. Stated as a rule for the implementer: THE GUARD IS THE
SYNTHESIS'S OWN LOOKUP, HOISTED - not a second predicate that agrees with it.

**Consumers.**
- `mogwai-adapter`: `fetch_trades` / `fetch_quotes` (`client/data.rs`) already
  `ensure!(response.status.is_success(), ...)` and surface a non-2xx as an
  `anyhow` error carrying the status code, so a `400` propagates loudly with
  no adapter change. The adapter's own `ensure_on_tape` pre-check is about
  `start`, not `symbol`, and is untouched. Adapter integration tests drive a
  stub HTTP server in `tests/common/mod.rs`, never the real handler, so none
  of them observes this change.
- `scripts/smoke.py` requests history only as `venue.symbol`, read from the
  readiness record.
- `crates/mogwai-cli/tests/serving.rs` likewise always passes
  `venue.record.symbol`.
- In-crate unit tests call `bounded_trades` / `bounded_quotes` directly with
  `"BTCUSDT"` against `generated_profiles()`.

So NOTHING in the tree currently requests an unserved symbol. There is no
existing assertion of the empty-`200` behavior to re-bless; the change adds a
refusal to a path no test exercises today, which is why this spec's first
brick is the instrument that would have caught it.

## The target, as concrete artifacts

One new accessor, one new refusal helper, two call sites, one de-duplication.

**1. `InstrumentProfiles::served_symbols`** in
`crates/mogwai-server/src/source.rs`:

```rust
/// Every symbol this run can synthesize, sorted, for refusal messages that
/// must name what IS servable. Sorted because a refusal body is read by a
/// human and diffed by a test; `HashMap` order is neither.
pub fn served_symbols(&self) -> Vec<&str>
```

Implemented over `self.by_symbol.keys()` with `.map(|symbol| &**symbol)` and
`sort_unstable`. Write the deref as `&**symbol`, NOT `symbol.as_ref()`:
`Symbol` is `Arc<str>`, so `as_ref` is ambiguous at inference time between
`AsRef<str>` and the `Arc`'s other `AsRef` impls, while `&**symbol` names the
`str` unambiguously. The return borrows from `&self`, which constrains the
caller to format while the `InstrumentProfiles` reference is live - fine for
`history_symbol_refusal`, which formats immediately.

It exists so the refusal names the served set from the SAME
structure the synthesis reads, rather than from `Run::instrument` - a second
source that agrees only by construction and would stop agreeing in slice 2.

**2. `history_symbol_refusal`** in `crates/mogwai-server/src/http.rs`, beside
`history_start_refusal`:

```rust
/// The one unknown-symbol decision both history endpoints make.
///
/// `Some(body)` is a `400`. The predicate is `InstrumentProfiles::get` - the
/// exact lookup `bounded_trades` and `bounded_quotes` perform three frames
/// down - hoisted so the miss is a named refusal instead of an empty `200`
/// the caller cannot tell from "no trades happened". That is the same
/// principle as the off-tape-window refusals above, applied to the other
/// parameter, and it is why the guard must never be a case-insensitive or
/// otherwise looser comparison: a guard that admits what the synthesis
/// misses restores the silent empty page.
fn history_symbol_refusal(symbol: &str, profiles: &source::InstrumentProfiles) -> Option<String>
```

Body text, exactly:

```
requested symbol {symbol} is not served by this run; this run serves {served}
```

Both clauses say "this run", not "this venue": the refusal's scope is ONE
PROCESS booted on one symbol, and "venue" would read as a claim about the
`MOGWAI` venue's instrument set - the admission-rule misreading the ruling
section forecloses.

where `served` is `profiles.served_symbols().join(", ")`. In slice 1 that
renders as one name, which is what the ruling means by "naming the served
symbol"; the plural form is already correct when slice 2 widens the map, and
costs nothing now.

The helper also logs, matching the existing refusals:
`tracing::warn!(%symbol, "refusing unserved history symbol")`.

**3. Call sites.** In BOTH `trades` and `quotes`, immediately after the
`HistoryQuery` destructure and before the `start` refusals:

```rust
if let Some(body) = history_symbol_refusal(&symbol, &profiles) {
    return Err((StatusCode::BAD_REQUEST, body));
}
```

Placed after `admit_history` for the same reason the existing refusals are:
one refusal block, one order, and the permit is released on the error return
by ordinary drop. It is placed BEFORE the `start` refusals because a wrong
symbol makes the window question meaningless - a request naming both an
unserved symbol and an off-tape start should be told about the symbol.

**4. The de-duplication.** `trades` drops its two inline refusal blocks and
calls `history_start_refusal(start, data_origin, sim_now)`, as `quotes`
already does. In the same edit, `history_start_refusal` GAINS the two
`tracing::warn!` calls the inline blocks carried, on the same two branches -
`tracing::warn!(start, data_origin, "refusing off-tape history window")` and
`tracing::warn!(start, sim_now, "refusing future history window")`. The word
`trades` leaves the message because the helper now serves both endpoints;
carry the endpoint in a field if the implementer wants it back, but do not
leave two texts. Without this move the collapse deletes `/trades`'s
observability, and the new `history_symbol_refusal` would be the only refusal
in the family that logs - the inconsistency this section exists to remove.

The prose currently carried in those inline blocks - why the
origin floor is unreachable at `TAPE_ORIGIN_NS == 0` and kept anyway, and why
a future `end` is CLAMPED while a future `start` is REFUSED - is load-bearing
and moves: the origin/ceiling reasoning onto `history_start_refusal`'s doc
comment, the clamp asymmetry staying at the `let end = ...` line in both
handlers where the clamp is.

Nothing else changes. `HistoryQuery`, `bounded_trades`, `bounded_quotes`,
`build_history_source`, `source::index`, `admit_history`, `HistoryPage` and
the permit's ownership are all untouched, and the `Vec::new()` early returns
inside the two `bounded_*` functions STAY - they are now unreachable from the
handlers but remain the correct answer for their direct in-crate callers, and
deleting them would push a `panic` or an `Option` into a path this item has no
business changing.

## The bricks, in landing order

Each is a keep/revert unit; the suite is green at every boundary.

### Brick 1 - the instrument that prices the item

No test in the tree observes the current empty-`200`, so the change cannot be
proven to bite before it is written. Land the test FIRST, against the current
behavior, asserting the CURRENT (wrong) answer, and confirm it passes. This
is a scaffold within one landing, not a shipped test: brick 3 rewrites its
assertions to the refusal. Its purpose is to prove the request actually
reaches the handler and that the empty page is real rather than assumed.

New test in `crates/mogwai-cli/tests/serving.rs`, beside
`trades_after_sim_now_are_refused_with_400`:

```rust
#[test]
#[ignore = "binds a loopback listener"]
fn history_for_an_unserved_symbol_is_refused_with_400() { ... }
```

In its brick-1 form it spawns `fast_config()`, requests
`/trades?symbol=NOT-A-SYMBOL&start=0&limit=5`, and asserts `200` with body
`[]`. Run it, see it green, and keep the run's output.

GATE: `brokkr test -p mogwai-cli history_for_an_unserved_symbol --debug`
(`--debug` because this is a subprocess-lifecycle test where release LTO
dominates wall time and optimization changes nothing under test; the runner
passes `--include-ignored`, which these `#[ignore]`d socket tests require).

### Brick 2 - the accessor and the helper

Land `InstrumentProfiles::served_symbols` and `history_symbol_refusal`, with
pure unit tests in `http.rs`'s existing `#[cfg(test)] mod
history_admission_tests` - NOT `mod calendar_tests`. `calendar_tests` is the
tempting home because it owns `generated_profiles()` (a one-entry
`InstrumentProfiles` keyed `BTCUSDT`), but `history_start_refusal`'s tests
live in `history_admission_tests`, and splitting the two halves of one refusal
family across two modules is the same drift this landing is closing.
`generated_profiles()` is `pub(super)`-visible within the file's test modules
or is moved up to be so; either way one helper, one module for the family.

Prefer a DIRECTLY BUILT `InstrumentProfiles::from_profiles(...)` over
`generated_profiles()` where the test allows it: `generated_profiles()` calls
`source::set_boot_for_test` (process-global state) and resolves a real preset,
which is far more machinery than a pure accessor and a `HashMap` lookup need.

- `history_symbol_refusal("BTCUSDT", &profiles)` is `None`.
- `history_symbol_refusal("NOT-A-SYMBOL", ...)` is `Some(body)` where the body
  contains both `NOT-A-SYMBOL` and `BTCUSDT`.
- `history_symbol_refusal("btcusdt", ...)` is `Some(_)`. This is the
  case-exactness pin, and it is the one assertion a future reader is most
  likely to "fix": it exists because `InstrumentProfiles::get` is exact, so
  the guard must be too, or the empty page returns for lowercase requests.
- `served_symbols()` on a one-entry map equals `["BTCUSDT"]`.
- THE SORT ASSERTION, which a one-entry map cannot make. Build an
  `InstrumentProfiles::from_profiles` with at least two profiles inserted in
  NON-lexical order (say `MNQ` then `BTCUSDT`) and assert
  `served_symbols() == ["BTCUSDT", "MNQ"]` exactly. `served_symbols` exists to
  make a multi-symbol refusal body deterministic, and every assertion above it
  passes against an implementation that never sorts; this one does not. Assert
  the exact ordered vector, not `contains`.

The helper is not yet called from either handler, so no behavior moves.

GATE: `brokkr check`.

### Brick 3 - wire both handlers, and the de-duplication

Land call site 3 and de-duplication 4 above, and rewrite brick 1's test to
its shipped form:

- `/trades?symbol=NOT-A-SYMBOL&start=0&limit=5` is `400`, and the body
  contains `NOT-A-SYMBOL` and the served symbol from `venue.record.symbol`.
- the same for `/quotes`, so the two handlers cannot drift.
- a lowercase spelling of `venue.record.symbol` (skip the assertion if the
  served symbol has no case, i.e. `to_lowercase() == symbol`, which it does
  have for every shipped preset) is also `400` - the end-to-end half of the
  case pin.
- the served symbol itself, same window, is still `200` - so the guard
  refuses the unserved case rather than everything.

Add a step to `scripts/smoke.py`'s history section, next to the existing
warmup fetch: request `/trades?symbol=NOT-A-SYMBOL&start={floor}&limit=5` and
assert the venue answers `400`. The smoke test is the only gate that drives
the real socket path end to end with an adapter-shaped client, and this
contract is one a host will hit.

IT IS NOT ONE LINE, and writing it as one produces a smoke test that dies on
the exception it means to assert. `Venue.http()` is `urlopen` plus
`json.loads`: it returns the decoded body and no status, and a `400` raises
`urllib.error.HTTPError` before any of that. The refusal body is PLAIN TEXT,
not JSON, so even a caught response must not be fed to `json.loads`. Write it
in the shape the file already uses for exactly this - the window-boundary
refusals a little further down wrap the call in `try` / `except
urllib.error.HTTPError as err`, raise an `AssertionError` if no exception
arrived, and assert on `err.code`. Follow that pattern, or add a status-aware
GET helper beside the existing `post` (which already returns a code by
catching `HTTPError` itself) and use it; do not change `Venue.http`'s
signature, which many call sites depend on.

BITE-CHECK, mandatory and by text edit: after the test is green, revert the
two `if let Some(body) = history_symbol_refusal(...)` blocks by DELETING those
lines in the editor, re-run the gate, observe the named failure ("a `200` was
returned where `400` was expected"), then restore them by typing them back.
Never restore with `git checkout -- crates/mogwai-server/src/http.rs`; the
tree routinely carries other uncommitted work in that file and that command
has destroyed it before.

GATES, all three:
- `brokkr check`
- `brokkr test -p mogwai-cli history_for_an_unserved_symbol --debug`
- `python3 scripts/smoke.py`, ALONE. Do not start a server first: the script
  spawns `mogwai serve` as its own direct child and reads the address off that
  child's readiness line, so a separately launched foreground venue is never
  contacted and only confuses the operator about which process answered.

Use `brokkr check --gate` for the COMMIT, not plain `brokkr check`. Brick 4
edits `crates/mogwai-adapter/src/client/shared.rs` and
`crates/mogwai-adapter/src/client/data.rs`, and the project rule keys on the
commit touching `mogwai-adapter` at all, not on whether the edit changes
behavior. The edits are doc comments and cannot break the four socket-backed
adapter binaries, but the rule exists because two regressions shipped red
through that gap, and running it costs one command.

### Brick 4 - the durable prose

Per piece 14, the writing lands WITH the code, in the same commit as brick 3.

- `docs/config.md`, the paragraph beginning "`/clock` names the resulting
  `data_origin_ns` and `warmup_ns`": add that `/trades` and `/quotes` also
  refuse a symbol the run does not serve with a `400` naming the served
  symbol, and state why - so an impossible request stays distinguishable from
  a quiet market. Name the exactness: the symbol must match the served
  instrument's symbol exactly, case included, even though CONFIG resolves
  preset names case-insensitively. Say that the served spelling is THE ONE THE
  CONFIG WROTE, and that a client should read it from the readiness record or
  `/instruments` rather than type it. Do NOT write that the served symbol is
  the preset's canonical spelling - it is not; see the correction under "What
  could break". Say that the refusal is about this run's symbol, never about
  whether a preset exists for the requested string.
- `reference/architecture.md`, where the endpoint list lives: one sentence
  that the history endpoints refuse rather than empty-page on all three
  impossible-request axes (before the origin, past the clock, not this run's
  symbol).
- `crates/mogwai-adapter/src/client/shared.rs`: correct the `ensure_on_tape`
  doc comment's "or, post-Landing-2, a server `422`" to the `400` this venue
  actually answers.
- `crates/mogwai-adapter/src/client/data.rs`: the SECOND live `422` comment,
  in the history-drop reasoning ("a server 422 (off-tape) or any fetch error
  must be ..."), says the same void thing. Correct it in this landing too -
  fixing one and leaving the other is worse than fixing neither, because the
  survivor then reads as a deliberate distinction. Grep `422` across
  `crates/mogwai-adapter/src` before calling this done; the only other hit is
  the FNV offset basis constant `0xcbf2_9ce4_8422_2325` in `convert.rs`, which
  is not a status code and must not be touched.

Both are doc-comment-only and change no adapter behavior, but they touch the
crate, so the commit takes `brokkr check --gate` as stated in brick 3.

## Stopping rule

IN SCOPE: the unknown-symbol answer on `/trades` and `/quotes`, the helper
that makes it one decision for both, the de-duplication of the `start`
refusals that the new helper would otherwise double, and the prose.

OUT OF SCOPE, each named so it is excluded rather than deferred:

- The `source::index` `INDEX`-mismatch refusal. In slice 1 it cannot be
  reached with a symbol that passed the profiles lookup. It is piece 7.
- `/instruments` returning the resolved configuration, and the adapter's
  subscription guard. Those are piece 13, a separate landing.
- Whether an unknown symbol becomes UNREACHABLE under total resolution. That
  is the slice-2 question the ruling explicitly deferred; when it lands, this
  refusal is deleted rather than amended, and the test written in brick 3 is
  the thing that has to be re-blessed to say so.
- Any change to `build_instrument_profiles`, to the boot symbol, or to which
  preset is the designated default.
- Any change to tape generation. Nothing here reaches the generator, so
  `TAPE_PROTOCOL_VERSION` is NOT bumped by this item, and a bump appearing in
  this landing is a mistake.

## What could break, and why it will not

- **A host that relied on the empty page.** None exists: the adapter never
  requests a symbol it did not read from `/instruments`, and the survey found
  no caller anywhere in the tree that passes an unserved symbol.
- **The `503` capacity contract.** Unchanged - the refusal returns after
  admission, so a refused request holds a slot for the duration of a string
  format, and the permit-rides-the-closure invariant is not touched.
- **Case-sensitivity surprise for an operator who wrote `mnq` in config.**
  Config resolution stays case-insensitive; only the REQUEST is exact. An
  EARLIER DRAFT OF THIS SPEC CLAIMED the served symbol is "the preset's
  canonical spelling"; THAT IS FALSE and the claim is struck.
  `resolve_instrument_named` ends by writing the REQUESTED spelling back over
  the merged table, so the profile carries the operator's verbatim string -
  `config.rs`'s own test `a_lowercase_symbol_matches_its_preset` asserts the
  resolved def's symbol is literally `mnq`. So an operator who writes
  `symbol = "mnq"` gets a run that serves `mnq`, and after this landing that
  run `400`s a request for `MNQ`.

  The behavior is still right, but for a different reason than the struck one:
  whatever spelling the config wrote is the spelling PUBLISHED in the
  readiness record and by `/instruments`, so a client that reads the served
  symbol rather than typing one is unaffected, and one that types a different
  case now learns so loudly instead of receiving a silent empty page. This is
  strictly better than today, where `mnq`-configured runs already empty-page
  `MNQ` requests with no signal at all. The `docs/config.md` paragraph in
  brick 4 must state the published-spelling reason and must not repeat the
  canonical-spelling one.

## Review record

Two independent reviews of the first draft - R1 (Claude Opus, eight findings)
and R2 (codex gpt-5.6-sol deep, four findings) - were validated against the
tree and consolidated here. R1's finding 1 and R2's finding 2 are the same
finding (the deleted trades warnings); R1's finding 3 and R2's finding 1 are
the same finding (the smoke step is not one line), with R2 additionally
catching the unused foreground server in the gate. Every remaining finding was
folded in above: the false canonical-spelling claim, the `--gate` requirement,
the unit-test module split and the heavyweight fixture, the `&**symbol` deref,
the open-instrument-set tension, the "this venue" wording, the second `422`
comment in `data.rs`, and the missing sort assertion.

NOTHING WAS REJECTED. Each finding was checked in the tree before folding: the
two `tracing::warn!` calls exist in `trades` and `history_start_refusal` has
none; `resolve_instrument_named` writes the requested spelling back over the
merged table; `Venue.http` raises on a `400` and `smoke.py` spawns its own
child; both adapter `422` comments are live; `generated_profiles` sits in
`calendar_tests` while `history_start_refusal`'s tests sit in
`history_admission_tests`. The two reviews agreed with each other and with the
tree on the core decisions - the `400` status, the exact lookup as the guard,
the placement after admission and before the start refusals, the permit's
ownership, and no `TAPE_PROTOCOL_VERSION` bump - and none of those moved.
