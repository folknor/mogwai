# Test doctrine

The distilled lessons of the 2026-08 bug-hunt arcs, kept durable because they
recur. Three arcs, all closed with zero open findings: a seven-document loop,
then an eleven-document arc over five test-suite reports and six
production-code ones, closed 2026-08-20, then a five-document production
bug-hunt arc - protocol and CLI, venue serving, venue mechanics, engine,
adapter - closed 2026-08-26. Each arc's round-by-round
carry-forward was deleted once this distillation landed; a live arc keeps a
live carry-forward (`notes/bug-loop-carry-forward.md` existing is the normal
state during an arc), and it is deleted when that arc closes and whatever
still binds has been folded in here.

This document is binding. Read it before laying, bite-checking or judging any
test, and before closing any finding. `AGENTS.md` carries the short form of
the rules that must never be skipped in any session; this is the full
statement, and where the two differ this one governs.

## The three defect families

Three families account for most of the serious finds.

- **The frontier family**, six instances. A watermark, cursor or frontier may
  only advance over work whose success the same expression checked. A lookup
  that legitimately returns nothing is exactly as dangerous as a panic, and
  the inverse failure - a fence with no recovery that wedges the watermark
  forever - is the same family. Treat any watermark assignment not guarded by
  the success of the work it covers as a defect on sight. The adapter's
  history pagination states the cursor form of the rule: a timestamp-only
  cursor may advance onto an instant only once every row at that instant has
  been seen; `ExecState::admit_account_snapshot` states the whole-work form,
  where a snapshot that lost rows to a `filter_map` may not retire the
  well-formed ones. The rule generalizes past cursors to writes: the adapter's
  receipt book files a command before queueing its frame and retires it only
  in the expression that saw `writer.send` return `Ok`, so what survives an
  abort is exactly what the venue never saw.
- **The guard-scope family**, three instances. A permit, lock or guard whose
  scope ends before the work it protects is the frontier defect in reverse,
  and it is visible by asking what is still resident when the guard drops. A
  guard is not scoped to the work by being alive while the work runs - it
  must be owned by the task doing the work, because the awaiting future can
  be dropped first (hyper drops handler futures on client disconnect; a
  running blocking task cannot be cancelled).
- **The vacuous-gate family**, roughly forty-four instances across eleven
  documents and by far the largest: a thing that reads as gated and is not.
  Every instance is cheap to find once the shape is named, and invisible
  otherwise, because both halves are green. The sub-shapes, which are the
  reusable part:
  - a guard that reports success on the branch it was built to catch (a drain
    stopped before another task could record the close it was watching for);
  - `all()` / `any()` over a collection whose emptiness is the failure mode -
    `all` over an empty iterator is `true`, so the helper returned success
    carrying nothing on exactly the race its doc described;
  - a control that is itself vacuous: a scanner blind to half the syntax it
    exists to scan (`#[test]` is not a substring of `#[tokio::test]`, so a
    source gate read no async test in the workspace and reported zero
    offenders), or a skip/only agreement checked by comparing two strings
    rather than resolving both against the tests;
  - a normalization applied to both sides of an equality that exists to detect
    that very difference;
  - a two-sided contract pinned on one side only - the eviction close's test
    hand-built the prefixed reason while the venue's own bytes were never held
    against the classifier that reads them;
  - a doc, comment or help text describing a gate wider or narrower than the
    gate - a comment promising a test runs a derivation it does not run, a
    constructor's doc asserting an invariant the constructor does not enforce,
    a helper whose comment states a set wider than its call sites (a runtime
    guard installed at two of the six sites that owed it, under a comment
    saying two);
  - and a fix that trades one blind spot for another.

  When a comment says a function guarantees something, either the function
  guarantees it or the comment is a defect; there is no third reading, and the
  cheap fix is almost always to move the guarantee into the function. Prose is
  the only artifact here with no compiler.

## Test rules

A dozen non-biting tests among what the arcs paid for.

- Bite-check every new regression test: revert the production fix as a text
  edit, observe the named failure, restore it as a text edit. Never restore
  with `git checkout -- <path>` - the tree routinely carries other uncommitted
  work in the same file, and that command destroyed it twice. The hazards, all
  paid for at least once:
  - Read which assertion fired, not merely that the test went red, and check it
    can fire only for the reason you mean. Four rail assertions survived a
    bite-check because an earlier guard inside the measurement loop failed
    first; a loop bounded by an assertion may never evaluate the property
    asserted after it; and a substring two messages share is not a
    discriminator.
  - Confirm the perturbed code can reach the assertion. A close-frame test that
    sent `Message::Close(None)` passed against its own defect in 0.02 s and
    looked like a clean pass, because the old code did not match that arm
    either. A test can be vacuous against its own defect while naming it
    correctly, and "red / green" cannot tell the difference.
  - Gut the callee, not the call site. Emptying a call site does not compile
    under dead-code deny, so the perturbation goes in the callee, imports
    included.
  - Derive the witness from the system when the defect has measure zero on the
    obvious inputs, and beware a perturbation whose outcome is a coin flip - a
    test resting on `HashMap` iteration order passed against its defect half
    the time.
  - And where a clean reversion is impossible, or the test cannot bite today,
    say so in the record. An honest "this cannot bite, and here is what does
    gate it" is worth a great deal; a perturbation that proves nothing looks
    exactly like a test that cannot fail.
- A regression whose defect signature is "the waiter parks" owes a
  non-blocking assertion. A test that waits on the channel it expects to be
  dead reproduces the hang instead of reporting it: the adapter's
  data-teardown bite spent brokkr's whole per-test timeout in
  `oneshot::blocking_recv` where `try_recv` against `TryRecvError::Closed`
  says the same thing in microseconds. A test that reproduces a hang is not a
  test that reports one.
- A golden can be byte-identical across two mechanisms that differ below its
  observation interval, which makes it a gate over a dead code path without
  ever failing: forcing every fill-band draw to zero left every field of the
  committed schema-2 fill golden unchanged, so the golden would have passed
  with the band deleted. Where the downstream observable quantizes the
  distinction away, record the mechanism's direct output in the artifact, and
  prove the recording bites by perturbing the draw in its callee and reading
  which assertion fires.
- A test that names which branch ran needs an observable, not a sleep. A wait
  of some fraction of a poll interval does not select an arm, it bets on the
  scheduler, and the losing bet is usually the silent one: the other arm does
  equivalent work, the assertion passes, and the test stops testing the fix
  without ever failing. Give the production code a counter or a state the test
  can read, and where the interval itself is the race, make the interval a
  parameter and pass one the test cannot lose to. The launcher's owner loop is
  the worked example: `LaunchedVenue::polls` plus `launch_with_poll`.
- Not every wait in a test is wall fat, and a sweep aimed at the gate wall is
  exactly where the difference gets lost. The 2026-08 parallel-safety triage
  classified every test-side wait in `crates/` and only one class was
  convertible, which it then emptied. What remains, and what a later sweep must
  leave alone: a poll interval inside a deadline-bounded loop, where the loop
  ends on a condition and the interval only decides how often it is asked; a
  negative-observation window, where the assertion is that something does not
  happen for a span, so shortening the span weakens the test; and a case where
  the duration is itself the thing under test. What is convertible is spacing a
  test pays for and never observes - reconnect-ladder rungs are the worked
  example, passed as a `ConnHavoc` through a client config's `havoc` field
  rather than inherited from `ConnHavoc::default()`, with the doc comment
  deriving the assertion's remaining margin from the rung arithmetic. The two
  shapes can sit in one test: the adapter's close-after-trades replay watch in
  `havoc.rs` passes a 20 ms rung for the setup redial and keeps its 400 ms
  silence window untouched, because that window opens only after the redial is
  established.
- The lane and profile splits bite, and they are two splits, not one. `brokkr
  check` runs tests in dev and multi-threaded; `brokkr test` runs them at
  `--test-threads=1`, dev by default with `--release` on demand. A test
  pinning `debug_assertions` behaviour must be gated `#[cfg(debug_assertions)]`
  or a release sweep fails it; a test whose bite depends on optimization must
  be checked in release; and a green run in one lane is not evidence about the
  other, so bite-check in the lane that is disputed.
  Libtest spawns a fresh named thread per test unconditionally on any threaded
  target - the name is how a panic is attributed to a test - so
  `--test-threads` caps how many run at once and not whether a thread is made.
  A `thread_local!` is therefore per-test-isolated in every lane. Measured
  twice, and a cold reviewer's contrary model (that the serial runner runs
  tests inline on the main thread) was refuted by probe rather than by
  argument. Treat it as true today and not as a contract: it is an
  implementation detail that has changed once, the failure if it changes back
  is silent and wholesale, and nothing detects it - which is why the adapter's
  `common::owns_a_fresh_exec_sink_on_every_lane` asserts the premise directly.
  A path is per-process or it is a shared resource: a test that wrote and then
  exec'd a fixed path aborted a parallel sweep with `ETXTBSY` when another
  process ran the same test. And when comparing flake records, the thread count
  is part of the record; three green runs are not evidence about an
  intermittent race.
- Run the socket suites after any change to the serving path. `brokkr check` is
  blind to roughly thirty tests that bind loopback listeners, and a real
  regression shipped through that gap and stayed red across four commits -
  eviction on the default account closing a consumer's own second socket, which
  only the socket suite surfaced. `brokkr check --gate` is the invocation that
  covers them, and it is the one to run. If it ever reports a wall of orphaned
  `mogwai-data` tests, suspect a crashed test before the tool and the tool
  before the tree - a single test aborting the parallel sweep produces the
  same wall a coverage-audit tool bug once did, and a real regression can hide
  behind either reading; 2026-08-19 saw exactly that, from an `ETXTBSY` on a
  test that wrote and then exec'd a fixed path while the gate's second sweep
  ran the same test in another process. The socket suites are also runnable by
  name: `brokkr test -p mogwai-adapter "" --debug` and
  `brokkr test -p mogwai-cli socket --debug`.
- Commit or stash before reading a `brokkr test -p mogwai-cli ""` result. That
  filter catches `arrival_control_refuses_a_tree_that_changed_during_the_run`,
  which refuses a dirty tree by design and fails rather than skips, so at a
  glance it is indistinguishable from a real regression.
- Audit the seam itself: a test double must be verified against the real
  endpoint's semantics, not against what the test needs. A stub that replays
  queued responses whatever the caller asked for is blind by construction;
  serve real semantics and record the requests so tests can assert the
  request sequence too.
- A test observing only an error cannot distinguish a bound from a check
  performed after the damage; assert on the resource the finding named.
- Two implementations a gate compares are pinned by one shared fixture, never
  by a hand-built case list on either side. Where a gate holds a corpus
  measurement against a synthetic one, the quantity is computed twice, and if
  the two conventions drift the gate silently compares different quantities and
  still passes - the failure is invisible because both halves are green. A
  fixture built on one side cannot catch this: it pins that implementation
  against itself. The convention, three instances so far, is a versioned
  language-neutral JSON fixture under `analysis/` carrying a `_doc`, `units`
  and `rules` block and stating the contract in a form neither side's units
  privilege, read by both sides: `spread_conformance.json` for the
  stratified Roll estimator and `dwell_conformance.json` for the empty-hour
  dwell statistics, both `include_str!`d, and
  `segment_library_conformance.json` for the segment library, which both
  `mogwai-data`'s `segment.rs` and `mogwai-lab`'s `segments.rs` load by path
  at test time. Keep the implementations separate - collapsing them usually
  means a dependency in the wrong direction - and keep the fixture shared. A
  rule one side genuinely owns (the lab dwell's era clamp) stays a local test
  beside it, because a shared fixture that cannot express it must not imply it
  was checked.
  Where the two sides share a crate graph, a module is the cheaper form of the
  same discipline. `mogwai_protocol::close` carries the WS close vocabulary -
  the code, the reason constants and `classify` - and the venue writes its
  reasons from those constants while the adapter reads them through `classify`,
  so neither side holds a literal the other could drift from. The gate on such
  a contract must run one side's real output through the other side's reader: a
  test that hand-builds the input it then classifies pins one side against
  itself, which is exactly how a missing eviction prefix survived with both
  halves green.
  Nothing detects a missing fixture. The next cross-implementation gate is
  caught by this habit or not at all. And no fixture detects its own
  tolerance quietly widening: the version is a schema version, a tolerance
  edit weakens both implementations at once, and the second implementation is
  structurally blind to it - review a tolerance change as a contract change,
  because no gate will.
  And the fixture is not always the answer. A two-copy gate whose copies are
  the same algorithm rather than two conventions for one quantity is closed by
  deleting a copy, not by anchoring both - `mogwai-lab`'s `summary` and
  `session` session-segment math was one such, collapsed in the lab/cli
  test-hunt. And where the two sides produce a whole accumulator record rather
  than a statistic, no units-neutral statement of it exists; anchor the
  inputs instead, which is usually where the drift is.
- Two constants encoding one quantity are the same defect without the gate,
  and they are easier to miss because neither looks like an implementation.
  `mogwai-lab`'s `subcontract` carries the final measurement window's length
  twice - as a nanosecond difference and as a seconds string - read by
  different consumers, so editing one moved the window the exposure walk
  measures while every gate stayed green. Where neither encoding can be
  derived from the other, assert the identity between them, and make a test
  that claims to catch a constant's drift read the encoding its subject reads.
  A frozen-snapshot hash over all the constants is not this gate: the
  sanctioned way to move one re-blesses the hash in the same change.

## Diagnostic rules

From findings that were argued wrong before they were measured right.

- A diagnosis that mutates is not a diagnosis, and a fix that makes a
  property true on one clock does not make it true on the observable.
- When a fix splits a predicate into two halves, every other reader of the
  old half is a call site of the fix. When a rule is added to both sides for
  a reason, the next rule in the same commit inherits that reason. A rule's
  blast radius is established at the call sites, never by grepping.
- The site that decides a behaviour may not mention the thing it decides for,
  so reach it through the behaviour rather than through a name you can grep.
  Read the validator, not the path.
- Check the callee's `unreachable!`s before reusing it from a new call site.
  A panic path genuinely unreachable from one caller is a live hazard from
  the next, and the compiler says nothing.
- A validator reached at two points in a message's life, with a different
  truth at each point, must take the point as a required argument with no
  default. The worked example is `mogwai_protocol::SubmitPhase`: pre-stamp a
  market order must carry no price and post-stamp it must carry the one the
  venue stamped, so a phase-blind validator was correct at the wire and
  silently rejected every market-entry bracket at the engine. Guessing either
  way is a defect, which is why neither a `Default` nor an inferred phase is
  allowed to exist.
- Before deciding what a guard admits, read the callee's own transition table
  in its source, and write the guard as an enumeration of that table rather
  than as a negation of a nearby predicate. `!status.is_closed()` and "only
  the states an initial submit is rejected from" were both wrong about
  nautilus's `Rejected` arms - the negation admitted three statuses the FSM
  refuses, the intuition dropped two the engine really rejects from. And key
  the guard on the origin of its evidence where the origins carry different
  weight: a verdict the far end sent and a guess synthesized from a local
  receipt book must not be allowed to close the same states, because the
  guess may not overrule state evidence against it.
- Resolve a version-history sentence against `git show <ref>:<path>`, never
  against memory - one such sentence shipped false, and the prose gate is
  blind to historical phrasings by design. Grep the number, not the file: a
  figure corrected in the documents a reviewer was looking at is not
  corrected everywhere it was written.

## Process facts

These govern how much a green anything is worth.

- A close pass over a whole arc is not optional, because the round shape -
  fix pass, gate, cold review, then a fix-and-commit agent that closes the
  review's findings and commits - leaves the second half of every commit
  unreviewed. Eleven documents, eleven close passes, and the defect was in
  that half eleven times out of eleven. Point the close pass at the durable
  prose the round touched and at the call sites of whatever the round's last
  fix installed: that code is the least-examined in the arc and it is the
  code closing a finding.
- A green gate proves nothing about a test that cannot fail, and a consensus
  review gate converges to the verifier's utility function - a clean cold
  review was followed by a serious find in a later pass five times. A green
  review is evidence, not proof.
- Measurement overturns confident argument, in both directions and
  repeatedly: cold reviewers were refuted by probes, and reports arrived
  whose findings were already fixed in the tree they were written against.
  Measure the disputed thing in the disputed lane before conceding or
  dismissing.
- A green workspace suite says nothing about the layer above the code the
  tests import. The scripts in `scripts/` drive the venue as a consumer does,
  through its wire, and no Rust test imports them - so a wire change can move
  every Rust caller with it, take the whole suite green, and leave a script
  posting a body the venue now refuses. That is not hypothetical: it happened
  to the divergence control plane, and underneath it `scripts/smoke.py` had
  been pinning a readiness-record version two bumps stale, which killed every
  mode of it at boot with nothing to notice because nothing ran it. A change to
  a wire shape therefore owes a sweep of the scripts, not only of the crates.
  The standing gate is `brokkr`'s `control-plane-shapes` script check, which
  boots a venue and posts a body for every divergence kind - the point being
  that it sends the thing rather than grepping for it, since the question is
  what the venue accepts and only the venue can answer that.
- A finding transcribed from an older note is a hypothesis, not a defect: in
  the 2026-08-24 bugs arc eleven ledger entries proved stale on contact with
  the code, having been carried forward from notes the tree had already
  overtaken. The rate is structural - the ledger is edited when findings are
  filed, and the tree moves whether or not anyone edits the ledger - so verify
  every entry at its site before fixing it, and reconcile the ledger in the
  same commit that closes the work rather than in a later sweep.
- A finding is closed by code or by a verified claim, never by a sentence
  alone. Twice in one arc a fix pass closed a finding by writing prose whose
  central factual claim was false on arrival - once into
  `reference/architecture.md` about the very ordering the finding named, once
  into a doc comment asserting a malformed event was unreachable when every
  field on its path is public. A durable claim closing a finding is owed the
  same verification as a code change, and where the claim is "this cannot
  happen", the cheap fix is almost always to make the code refuse it so the
  sentence stops carrying the load.
- Read the test count after every fix pass and ask which findings it covers,
  not only how many tests moved. Twice in one arc a fix pass claimed coverage
  the workspace count showed had not appeared, and a green gate detected
  neither; twice more the count was right and the summary's "added" was not.
  A flat count is a finding; so is a count that moved by less than the
  findings claimed.
- The carry-forward is the artifact most likely to be skipped, because the
  agent that lands the code and the report is the last hand on both and no
  agent in the loop reads the carry-forward back. A round whose lesson is
  not written there did not happen as far as the next arc is concerned.
