# Implementation spec: the gate ceiling

Written against `reference/technical-implementation-spec.md`, from the Tests
and tooling section of `notes/todo.md` (the parallel-safety triage entry) and
the priority-order paragraph at the top of that file, which names this slate:
"the gate-ceiling work - declared durations, reconnect-ladder spacing as a
parameter, and whatever turns `test_threads = 16` red. Force-multiplying
rather than product work; interleave it while slate 1 waits on a gate."

This document is transient. It is deleted in the same commit that lands its
final brick. The durable facts it produces land in `brokkr.toml`'s own
comments, `reference/performance.md`, and the code sites it touches.

## The goal, stated once

The gate profile runs at `test_threads = 8`, a measured compromise: at 16 the
run goes red as a wrong answer rather than a watchdog timeout, so the ceiling
is set by our least robust test rather than by the machine. The goal is a gate
that is green and trustworthy at 16, and a gate wall that stops paying for
time no assertion needs: the 164 s serial execution profile found its
concentration in declared `--duration` runs and reconnect ladders spending
their attempts - durations a test asked the venue for, not sleeps.

Explicitly not the goal: shaving sleeps. The triage in `notes/todo.md` is
done and binding - the eighteen poll intervals, the six negative-observation
windows, and the three duration-is-the-subject tests are all correct as they
stand, the convertible class is empty, and two sites are blocked on a decision
or a missing seam. This spec does not reopen any of that.

## Survey of the ground

What is true in the tree today, verified at the sites rather than inherited
from the entry's prose.

- **The gate's parallelism.** `brokkr.toml` `[test.profiles.gate]` and
  `[test.profiles.dev]` both set `test_threads = 8`. The gate comment records:
  serial 3m01s with 164 s of test execution over 1,608 tests, top 20 tests 54
  percent of it, at 8 about a minute with per-test times unmoved. The comment
  ends "the rest of that story, and the work that would let this go higher, is
  G1 in `notes/bugs.md`" - a dangling pointer, since that file was adjudicated
  and deleted on 2026-08-26. The todo entry adds two process facts that bind
  brick 1: the 74s-serial-versus-53s-flat claim is unsupported and must be
  re-run rather than inherited, and `test_threads = 8` itself went red once
  after three green runs, so nothing here is settled by a small number of
  passes.

- **The declared-duration family.** `crates/mogwai-cli/tests/completion.rs`
  runs `--duration`-bounded venues through `watch_a_bounded_run`, which
  already establishes its premise correctly (every socket proven a live
  passenger, budget-ended drains discarded, losers kept alive so the wall
  budget does not re-anchor). The fixture is
  `crates/mogwai-cli/tests/configs/bounded-run.toml`: `speed = 1.0`,
  deliberately, so a declared 2 s of simulated time is 2 s of wall - the
  fixture's own comment explains why `speed = 0.0` is a trap for this family
  (the announcement queues behind an unpaced backlog). The comment also
  carries the measured margins at seed 42: the sparse boot river serves its
  first content frame 1.031 s after attach, which is also its longest gap, so
  a declared 2 s holds about a second of margin. Each such test costs its
  declared duration in wall, by construction, plus relaunches when the attach
  race is lost.

- **The declared-duration family, counted rather than characterized.** The
  earlier draft of this spec called this family "the wall concentration"
  without counting it. Counted: `bounded_run_config()` has exactly three call
  sites in `completion.rs`, and all three declare `"2s"` - about 6 s of the
  164 s serial profile. The 30 s accelerated run is on `accelerated_config()`
  at speed 100 and costs about 0.3 s of wall; the remaining declared 2 s
  bounded run in that file is on `fast_config()`. Neither is a
  `bounded-run.toml` consumer, so brick 3 does not touch either. The prize
  brick 3 can address is therefore on the order of 3 s serial, not tens of
  seconds, and how much of that survives parallel scheduling - a wall-waiting
  test overlaps another test's compute - is itself unknown until brick 1
  measures it.

- **One of those three call sites is mixed-speed.** The second boat in
  `run_complete_is_stamped_on_the_receiving_sockets_clock` is opened as
  `?symbol=MNQ&speed=1`. Both the test and the fixture comment are explicit
  that what separates the two boats is their wall anchor and not their speed -
  the fixture comment says so in as many words, correcting an earlier version
  of itself that claimed otherwise. Any change to the fixture's speed
  interacts with that query string; brick 3 carries the consequences.

- **The watcher does not report its discards.** `watch_a_bounded_run` keeps
  discarded venues in a private `_spent` field and names the launch count only
  inside the retry-budget assertion, on failure. On success nothing is emitted
  and nothing is returned, so a passing run that relaunched twice is
  indistinguishable from one that did not. Wall time does not separate them
  either: attempt cost, venue materialization and host scheduling all move it
  by more than an attempt costs. Any brick asserting on relaunch behavior owes
  an observable first.

- **The dev lane's 8 is not independent evidence.** `[test.profiles.dev]`
  justifies its 8 as "the same 8 the gate runs", and its next clause is the
  load-bearing one: that lane never includes the ignored socket and lifecycle
  tests, "which are where the measured cliff at 16 lived". Dev inherited the
  number for a reason that does not apply to it, so a green dev lane at 16 is
  not evidence about the ceiling.

- **The reconnect ladder is already a parameter.** The spacing lives in
  `ConnHavoc` (`crates/mogwai-protocol/src/havoc.rs`): initial 1,000 ms, max
  10,000 ms, factor 2.0, jitter 0, attempts uncapped by default, consumed by
  `ReconnectPolicy::from_conn` in `crates/mogwai-adapter/src/lifecycle.rs` and
  threadable through both client configs' `havoc: Option<HavocSpec>` field
  (`spec.conn`). So the todo's question - "whether the ladder's attempt
  spacing can be a parameter the test passes" - is answered yes in the
  mechanism already; what remains is that not every test which forces a redial
  passes one. `havoc.rs`'s dial-cap test already does (30 ms rungs, capped at
  3, with its own doc explaining the margin arithmetic); any socket test whose
  scenario kills a connection and waits for the re-dial while holding
  `ConnHavoc::default()` is spending one-second-plus rungs of pure spacing.
  Which tests those are is a measurement, not a guess - brick 1 names them.

- **What guards this ground.** `reference/test-doctrine.md` binds every test
  this spec touches or lays. The completion family's history is the template
  and the warning both: the failure at 16 was a wrong answer that read like a
  regression (a run that completed before the socket existed), fixed by
  establishing the premise rather than by widening a margin, because "a margin
  is exactly what a crowded host takes away". Two prose-scanning tests
  (`live_fact_prose.rs`, `tape_version_prose.rs`) read every markdown file, so
  this spec's own prose and the brokkr.toml comment edits stay inside their
  conventions.

- **What this spec cannot move.** Nothing here touches the tape generation
  path: no generator constant, fingerprint leaf, seed derivation, or fill
  draw. No `TAPE_PROTOCOL_VERSION` bump is owed by any brick, and no chart
  verdict either - the standing chart gate covers tape generation and no tape
  byte moves. Fixture and profile changes move test statistics only.

## The bricks, in landing order

The suite is green at every boundary. Bricks 2 through 4 are independent of
each other and may land in any order between bricks 1 and 5; brick 5 is last
because it is the keep/revert verdict on everything before it.

### Brick 1: the instrument - a truthful profile and a 16-thread hunt

The decision this measurement changes: which tests get which repair in bricks
2 through 4, and whether brick 5's `test_threads = 16` is accepted. Without
it the spec would be aiming at a top-20 list from an earlier tree.

1. Re-run the gate profile the same way the 164 s figure was produced and
   record the current per-test wall concentration. Where a single suspect
   needs pricing, `brokkr test -p <package> <NAME>` prints a wall-time footer
   per run.
2. Edit `[test.profiles.gate]` to `test_threads = 16` locally (not committed
   in this brick - it is the probe, and brick 5 is where the setting lands or
   does not) and run `brokkr check --gate` repeatedly. The todo's standing
   caution sets the bar: three passes are not evidence about an intermittent
   race, so the hunt runs until it has either a stable set of failures or a
   pass count that would have caught the 8-thread flake (ten runs minimum;
   more if any run is red).
3. Enumerate brick 4's candidate set, which is a source question rather than a
   profile: which socket tests force a redial while holding
   `ConnHavoc::default()`, and what each one's ladder actually costs. Without
   this as a named product, brick 4 can be entered with an empty candidate set
   and read as complete. If the set is empty, brick 4 does not land, and that
   is a result rather than a failure.
4. For every red, capture the failing test's own output and classify it:
   wrong answer versus watchdog timeout, and against the doctrine's defect
   families - the completion family's lost-attach shape (a premise the test
   assumed rather than established), a shared-resource collision, or a genuine
   venue defect surfaced by load. A wrong answer under load is presumed to be
   the test's premise until traced; a venue defect found here is a lateral
   finding that gets its own entry, not a silent fix inside this slate.
5. Price the declared-duration family against the gate, not against itself:
   its summed wall as a fraction of the gate wall, at 8 and at 16. This is
   what says whether brick 3 is worth landing at all.
6. Record, in this document while it lives: the failure set, the per-test
   top-20, the ladder candidate set, the family's share of the gate wall, and
   the re-measured serial-lane number that replaces the unsupported 74/53
   pair.

On the pass count, since brick 5 asks the owner to ratify it. Ten is not a
derived number and this spec does not pretend it is. What ten independent
green runs excludes, if failures were independent per run: a per-run failure
rate of 26 percent or higher is ruled out at 95 percent confidence, and
nothing weaker is. The standing counterexample is exactly in the range that
leaves open - `test_threads = 8` went red after three green runs. So ten runs
is a floor chosen for cost, not a bound that retires the flake risk, and the
honest statement of brick 5's evidence is "no failure in ten" rather than
"green". The hunt is itself a wall cost the slate has to carry: ten
`brokkr check --gate` runs at the current parallel wall is roughly ten
minutes of machine time per configuration, and brick 5 re-runs the count on
its own tree. Record the observed per-run wall in step 6 so the count can be
raised knowingly if the owner wants a tighter bound.

Gate for this brick: it is a measurement; its product is the classified list.
No code moves.

### Brick 2: whatever turns 16 red

One landing per root cause, each shaped like the completion family's repair:
establish the premise the test was betting on, never widen a margin or retune
a duration. The concrete pattern to reuse is `watch_a_bounded_run` in
`crates/mogwai-cli/tests/completion.rs` - discard and relaunch until the run
observed is provably the run reported on, with the retry budget checked
against the last attempt's measured cost so the wall clamp's message never
replaces the test's own.

Per repair:

- The fix is to the test or its fixture unless brick 1 classified it as a
  venue defect, in which case the venue fix is its own landing with its own
  regression test, bite-checked per the doctrine (revert the production fix as
  a text edit, observe the named failure, restore it as a text edit).
- Gate: the repaired test at both parallelism levels.
  `brokkr test -p <package> <NAME> -N 20` for the focused evidence, then
  `brokkr check --gate` (still at 8 in the committed tree) for the boundary.
- A repair that touches `mogwai-adapter` is verified with
  `brokkr check --gate`, never plain `brokkr check` - the socket binaries are
  invisible to the plain form.

This brick is complete when the brick-1 hunt, re-run at 16, is green across
the full pass count. If a failure resists diagnosis, the honest landing is the
diagnosis recorded at the test site and the ceiling staying at 8 - brick 5
then does not land, and the spec closes with the profile and ladder wins from
bricks 3 and 4 only. That is the stopping rule, not a deferral: the ceiling
item in `notes/todo.md` stays open, rewritten to name the one test and the
observed failure.

### Brick 3: the declared durations

**Entry condition, and it is a real gate.** This brick lands only if brick 1's
step 5 finds the `bounded-run.toml` family in the gate's top 20, or otherwise
shows it holding a share of the gate wall worth a fixture edit. The counted
prize is about 3 s serial across three call sites, and a fixture edit that
touches a clock-sensitive test is not free. If brick 1 prices it below that
bar, the honest landing is a line in the gate comment recording the family's
measured cost and no fixture change.

The family's wall cost is `declared sim duration / speed` per watched run,
plus relaunches. The declared duration itself is not fat to trim - the boot
river's measured 1.031 s first-frame gap at seed 42 means a declared window
much under 2 s starts discarding runs on liveness, which the fixture comment
already establishes. The lever that remains is pacing: at `speed = 2.0` the
same 2 s of simulated time - same frames, same seed, same announcement
semantics - costs 1 s of wall, and every measured margin in the fixture
scales with it (the first-frame gap becomes about 515 ms against a 1 s wall
window, the same ratio).

The recommendation, and it is one recommendation rather than a menu: set
`speed = 2.0` in `bounded-run.toml`, repair the mixed-speed site below, update
the fixture's comment, and leave every declared `--duration` string as it is.
What bounds the speed at 2 rather than 10 is the same fact that parked this
family once: absolute wall margins are what a crowded host takes away, and
the discard-and-relaunch guard converts a too-thin margin into relaunch churn
that costs more wall than it saves.

**The mixed-speed site, and it is a vacuous-gate hazard.** The second boat in
`run_complete_is_stamped_on_the_receiving_sockets_clock` is pinned at
`speed=1` by its own query string, and the test's premise is that the two
boats differ by wall anchor alone - both at 1.0 - because that is the only
difference a shared-clock regression could erase. Move the fixture to 2.0 and
leave the query alone, and the two boats differ for a second, trivial reason:
both `assert_ne!`s then pass whatever the clock does, which is the
vacuous-gate family landed by a fixture edit. Worse, that boat would then
advance at speed 1 through a run that ends after about one wall second, so it
covers roughly one simulated second rather than two - its frame count and its
own margins do not scale with the fixture, and the "same frames, same seed"
claim above holds only for the boats that inherit the fixture speed. The
repair is to move the query to `speed=2` or drop the speed parameter so the
boat inherits the fixture, and either way the changed test is bite-checked as
a new test: revert the fixture as a text edit, observe the named failure,
restore it as a text edit. Its liveness margins are measured fresh for both
boats rather than divided down from the existing table.

**The skew arithmetic, derived rather than borrowed.** An earlier draft
justified the headroom with "the largest measured shortfall (18 ms of 30 s)".
That reading comes from the accelerated caller on `accelerated_config()` at
speed 100, a fixture this brick does not touch, so it says nothing about the
bounded-run family. The bounded-run family's own measured shortfall is 1.7 ms
of 2 s. The derivation that matters: `boat_skew_floor` allows one percent of
the declared sim duration, a fixed 20 ms of sim time at a declared 2 s, while
the announcement trails by the placement gap times `speed` - so doubling the
speed roughly doubles the sim-time shortfall, to about 3.4 ms against the same
20 ms allowance. That is still about six times of headroom, but it is the
number this brick is spending, and it shrinks rather than scales. Re-verify it
against a fresh multi-run shortfall reading rather than against this estimate.

**Comment scoping, so a sweep does not rewrite what is still true.** Only the
fixture comment's margin table is in play - the frame counts, first-frame
offsets and longest gaps at seed 42, restated as measured at the new speed.
The `speed = 0.0` trap paragraph and its arithmetic (1,475,111 frames against
a drain of about 111,000 a second) are about `fast.toml` and stay true at any
bounded-run speed; leave them. The two-boat paragraph is edited only insofar
as the query string moves.

Gate: first, the discard observable. The keep/revert criterion below asserts
on relaunches, and today a successful relaunch is invisible - `_spent` is
private, nothing is reported on success, and wall time cannot separate a
relaunch from materialization or scheduling variance. So this brick either
exposes the discarded-attempt count on `WatchedRun` and asserts on it, or
drops the relaunch clause from its criterion; asserting on it as the spec
originally read would be a vacuous gate of exactly the kind the doctrine names.
Then `brokkr test -p mogwai-cli completion -N 20` green with the discard count
observed at zero in the common case, then `brokkr check --gate`, then the
brick-1 hunt's 16-thread configuration re-run over this family specifically.
Keep/revert verdict, stated against the gate rather than against the family:
the gate wall moves measurably and the observed discard count does not rise.
A family-relative halving is not the criterion - it can be satisfied while the
gate wall does not move at all. If relaunch churn appears, revert the fixture
change entirely; a partial speed is a new measurement, not a fallback.

### Brick 4: reconnect-ladder spacing as a passed parameter

The candidate set is brick 1's step 3, and this brick does not begin before
that step has an answer written down. An empty set closes this brick with
that fact recorded; it is not entered, swept and declared done against nothing.

For each socket test in that set: pass a
`ConnHavoc` through the client config's existing `havoc` field with rungs
sized to the test's own margins, following the worked example already in
`crates/mogwai-adapter/tests/havoc.rs` (the dial-cap test: 30 ms initial and
max, capped attempts, and a doc comment deriving the assertion's margin from
the rung arithmetic - that derivation is part of the pattern, not decoration,
because a fast ladder narrows the gap between the legal outcomes and the
doc is what stops a later reader widening a window back over it).

Constraints carried from the triage, restated because a sweep is exactly
where they get violated:

- The six negative-observation windows are not convertible; their duration is
  the subject. The three duration-under-test cases likewise. Neither class is
  touched even where a fast ladder would let the window shrink.
- The two blocked sites stay blocked: `serving.rs`'s 500 ms market-reading
  spacing is the assertion's flake margin, and `data_client_transport.rs`'s
  segmented-head 20 ms sleep waits on a seam that does not exist. If this
  brick happens to build a seam that unblocks the latter, that is a lateral
  win to take - but it is not owed, and the spec does not gate on it.
- `validate_conn_havoc`'s rules bind every table this brick writes: factor
  finite and at least 1.0, no mixed-zero bounds, max at least initial.

Gate per converted test: `brokkr test -p mogwai-adapter <NAME> -N 20`, then
`brokkr check --gate`. A conversion that changes what a test can observe (a
redial landing inside a window that previously could not see one) is treated
as a new test and bite-checked.

### Brick 5: the ceiling itself, and the durable prose

The keep/revert landing that the rest of the spec exists to earn. It is two
commits, not one, and the split is what makes the revert path executable -
see the revert path below.

Independent of all of it, and landable at any point in the slate: the gate
comment's `notes/bugs.md` G1 pointer is a wrong reference today, since that
file was adjudicated and deleted. It is one line, it depends on no brick, and
it should not wait for this one.

First commit - the setting and the prose that describes it:

1. `[test.profiles.gate]` and `[test.profiles.dev]` move to
   `test_threads = 16`. One landing, both lanes: moving them together is
   right, but the rewritten dev comment must stop implying the two numbers
   are one measurement. Dev's exclusion of the ignored socket and lifecycle
   tests is exactly why its lane never saw the cliff, so it never had
   independent evidence for 8 and does not acquire any for 16.
2. The gate comment is rewritten to the facts brick 1 measured: the new
   serial and parallel walls, the new top-20 concentration, the fate of the
   cliff, and the pass count the 16 rests on stated as "no failure in N"
   rather than as settled. The unsupported 74/53 pair is replaced by the
   re-measured number or removed.
3. `reference/performance.md` takes the durable numbers under its annotation
   discipline: the gate walls before and after, at 8 and 16, and the
   declared-duration family's cost before and after brick 3.

Second commit, only after the committed 16 has survived the full brick-1 pass
count on the landed tree:

4. `notes/todo.md`'s triage entry is removed per that file's own rule, with
   the surviving facts placed where they now live: the leave-them
   classifications become comments at the sites that need defending (the
   negative-observation windows largely carry them already), and the two
   blocked sites keep their entries only if still blocked. The separate
   budget-marker entry (a brokkr feature) stays - see the exclusions.
5. This spec document is deleted in the same commit.

Gate: `brokkr check --gate` at the committed 16, run the full brick-1 pass
count, on the first commit's tree. Revert path, and this is why the commits
are split: if the committed 16 produces any red the hunt did not, the setting
reverts to 8 while the diagnosis reopens as a brick-2 case - and both the
brick-2 process description in this document and the todo triage entry are
still there to reopen into, because the second commit has not landed. The
prose landings survive with the numbers restated at 8. Should a red arrive
after the second commit, the revert commit re-files the todo entry explicitly,
naming the one test and the observed failure; it does not rely on the deleted
document.

## Excluded, and why each is exclusion rather than deferral

- **The budget-marker convention** (`notes/todo.md`, "a budget-carrying test
  cannot be routed into the `timing` sweep automatically"): a brokkr feature
  and a new cross-project convention; this tree's half is adoption once the
  tool can enumerate the marker. A genuinely separate item with its own entry.
- **The two blocked sleep sites**: blocked on a decision and a missing seam
  respectively, named in brick 4's constraints; unblocking them is not what
  this slate is for.
- **Production pacing sleeps** (the roughly forty-four launcher, adapter
  clock, boat and sweeper sites): they price no test and are out of scope by
  the triage's own finding.
- **Everything tape-gated**: no brick here touches generation, so the
  segment-sampler gate and its cluster are unaffected and unaffecting.

## Review dispositions

Two independent reviews were folded in above. Everything either raised is
accepted and now lives in the brick it belongs to, with two qualifications
worth recording so they are not re-argued.

- **Accepted in full**: the mixed-speed `speed=1` query site (raised by both
  reviews, one framing it as a vacuous gate and the other as unscaled boat
  margins - both are true and both are in brick 3); the 18 ms shortfall being
  borrowed from a fixture brick 3 does not touch; the uncounted prize; the
  missing ladder candidate set in brick 1; the unpriced and underived pass
  count; brick 5's revert path outliving its own documents; the dev lane's 8
  not being evidence; the fixture comment's trap paragraph needing scope;
  the G1 pointer not needing to wait; and the watcher's unobservable discard
  count making brick 3's original criterion vacuous.

- **Accepted with the reasoning trimmed**: one review argued the parallel
  saving from brick 3 is "near zero" because a wall-waiting test overlaps
  another test's compute. Plausible and probably right, but it is an argument
  rather than a measurement, and asserting it here would repeat the error the
  review was correcting. What landed instead is brick 1 step 5, which prices
  the family against the gate wall at both parallelism levels, and brick 3's
  entry condition, which consumes that price.

- **Not folded as a spec change**: the demand that this document state a
  confidence bound for the pass count was folded, but as a plain statement of
  what ten runs does and does not exclude rather than as a derived design of
  the bar. A bar derived from a target failure rate would need a failure-rate
  target nobody has, and the counterexample in the tree (red after three
  green) is the reason the honest phrasing is "no failure in N" rather than
  any confidence claim about the gate as a whole.

## What this spec needs from the owner

One decision, at one point: whether the brick-1 evidence bar - no failure
across the stated pass count at 16, which excludes a per-run failure rate of
26 percent or worse and nothing weaker - is accepted as sufficient to move the
committed ceiling in brick 5, or whether the count should be raised at the
wall cost brick 1 will have measured. Everything else is correctness work
inside a slate the priority ruling already authorized. No chart is owed, no
measurement beyond those named, and every named measurement states the
decision it changes.
