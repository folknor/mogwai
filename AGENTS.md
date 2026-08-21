# AGENTS.md

## Project

mogwai is a fake broker/exchange that plugs into a nautilus trading system to
exercise the *live* trading path. It synthesizes market data from a committed fingerprint
fitted offline to Kraken trade history (the running venue opens no CSV) and
injects the messy, realistic execution divergences (partial fills, rejects,
delays,
duplicate fills, dropped account updates, venue blackouts) that an in-process
backtest sandbox structurally cannot produce. The broker core never imports
nautilus; the `mogwai-adapter` crate is the lone, deliberate exception - it
depends on the published nautilus crates to ship the
`ExecutionClient`/`DataClient` pair a host constructs to drive the `MOGWAI`
venue over this workspace's native JSON-over-WS protocol.

The INSTRUMENT SET IS OPEN, and THE VENUE DOES NOT GATE ON IT. A symbol
arrives and is served: if a tuned preset exists for that symbol, the preset
drives the tape; if none exists, the DEFAULT tape is served under that symbol.
Nothing refuses a symbol for wanting a fit, and no intake work is a
precondition of serving one. The three shipped presets - MNQ, MES and
BTCUSDT - are the current state, not the end state.

The intake sequence - corpus, measurement, fit, preset - is therefore how a
symbol's tape gets BETTER, never a gate on whether it can be served at all.
Do not read it as an admission rule: earlier prose here and in `docs/presets.md`
claimed that every instrument owes a corpus, a measurement and a fit before the
venue serves it, and that claim was invented. It was back-derived from a
2026-08-09 owner ruling that retired the ETHUSDT and SOLUSDT presets, which was
a narrow measurement-scope amendment - those presets added identity-only rows to
the measurement oracle while contributing no distinct dynamics, so the oracle
lost nothing by dropping them. The record is that ruling in
`notes/protocol-12b-arrival-composition-spec.md`, and it ruled on what the
oracle measures, not on what the venue may serve.

The offline toolbox (`mogwai-lab` and what survives in `analysis/`) is
reusable intake machinery, not one instrument's history: treat a component as
spent only when its QUESTION cannot recur, and keep per-instrument knowledge
in config or a preset rather than hardcoded in the method. MES remains a
stated stopgap borrowing the MNQ fit. See the architecture note on the intake
sequence for what this binds.

## Workspace

A Cargo workspace, seven crates under `crates/`:

- `mogwai-protocol` - the wire types (`ClientMessage`, `VenueMessage`) plus
  `control::Divergence`. The single source of truth both ends serialize against;
  it never imports nautilus. Also carries `launch`, the SHIPPED launcher: it
  lives here rather than in `mogwai-adapter` because this workspace's own test
  binaries drive the venue through it and cannot depend on the adapter, so a
  launcher shipped from there would leave the contract hand-rolled on both sides.
  `mogwai-adapter` re-exports it for consumers that already depend on it.
- `mogwai-engine` - the venue-agnostic exchange core, with the seam that injects
  armed divergences into the event stream.
- `mogwai-data` - the `TickSource` seam and the k-way `MergeSource`. Carries the
  `GeneratedSource` synthetic generator the running venue uses (fitted to the
  committed fingerprint) plus the `KrakenCsvSource` streaming loader kept as the
  offline-analysis lineage.
- `mogwai-server` - the axum LIBRARY that owns the sockets, the clock and replay
  pacing, synthesizing market data per subscription; exposes `/health`, `/ws`,
  `/control/divergence`, `/instruments`, `/trades`, `/quotes`, `/account` and
  `/clock`. Order entry is websocket-only - the `POST /orders` carrier went with
  the HTTP transport profiles. `serve` runs
  ONE venue in the foreground for one run and owns no PID, log or config files:
  it reports its bound address as a single JSON readiness line on stdout,
  and `PR_SET_PDEATHSIG` makes the kernel kill it when its launcher dies. There
  is no daemon mode and no `stop` subcommand - lifecycle is the launcher's job.
  See `docs/cli.md`. It ships no binary: `serve`, `config` and `source` are its
  public surface and `mogwai-cli` calls them.
- `mogwai-cli` - the `mogwai` BINARY. A clap dispatcher plus the offline
  subcommands that never bind a socket: `gen` (the tape generator and its
  measurement consumers, including `measure12a`), `tick-composition`,
  `segments` - a subcommand tree of its own, `cut` turning a delivered TBBO
  month into a returns-space segment library and `tape` composing that library
  into an endless tape dumped as trades or bars in CSV - `presets` and `man`.
  `serve` does no work here - it hands its three arguments to
  `mogwai_server::serve`. The bin TARGET name is `mogwai`, not the package name,
  and that is load-bearing: `brokkr run mogwai` and the shipped launcher both
  exec `target/release/mogwai` by that name. The Python measurement harness
  did too, until phase 4b retired it; renaming the target no longer breaks it,
  because it is gone, but the launcher and every doc still key on the name. The
  socket-backed lifecycle/serving/completion integration tests live here too,
  because only this crate's tests get `CARGO_BIN_EXE_mogwai`.
- `mogwai-lab` - the corpus-to-fingerprint method library the 2026-08
  Python-to-Rust rewrite absorbed from `analysis/` (the rewrite's phase
  records are retired to git history):
  TBBO/Binance-trades corpus parsing, the protocol-12a measurement engine,
  fingerprint/cadence synthesis and the protocol-11 fit. Depends on
  `mogwai-data`, `mogwai-protocol` and `mogwai-server` (session-summary work
  resolves an `InstrumentProfile` through `Config::load`); `mogwai-server`
  depends on none of it, so there is no cycle. `mogwai-cli` calls it for
  `preflight`, `measure`, `fit`, `cache` and `synth`. It also carries
  `sidecar`, the observation-only benchmarking channels (stderr `key=value`
  scalars and the marker/counter FIFO) the benched commands report through;
  see `reference/performance.md`.
- `mogwai-adapter` - the nautilus venue adapter: the `MogwaiDataClientFactory` /
  `MogwaiExecutionClientFactory`, their configs, and the client pair a host
  registers for the `MOGWAI` venue. The only crate that depends on nautilus -
  the published crates.io crates pinned in its `Cargo.toml`, default-features
  off, no pyo3; the other six build nautilus-free.

`scripts/` holds the end-to-end smoke test and the harness-bug flush the
orchestration loop uses (codex is now driven by the `review` tool, configured
from `.review.toml`, not by wrapper scripts);
`analysis/` is the offline Python that fits the fingerprint; `notes/` holds the
transient work items and plans. The durable documentation is split by subject:
`docs/` is how the venue is USED (`cli`, `config`, `havoc`, `presets`,
`oms-types`) and `reference/` is how it is BUILT and why (`architecture`,
`clock`, `glossary`, `performance`, `technical-implementation-spec`). See the
Document folders section below for what each folder may and may not contain.

## Rules

### General rules

- Don't use gremlins! Em-dash, en-dash, strange quotes, whatever - they're all verboten.
- No all-caps. Not for emphasis, not for a term of art, not for a warning.
  Write the sentence so the emphasis is carried by the words and where they sit.
  A document that shouts every important thing has no way left to mark the one
  thing that matters, and prose full of capitals is read by nobody twice.
- Don't remind the user of the rules. They wrote them, so they know them.
- The user can exempt you from any rule at any time.

### Tape protocol version

Any change to the tape generation path MUST bump
`mogwai_data::TAPE_PROTOCOL_VERSION`. This includes a generator constant, an
arrival-clock or GARCH parameter, the committed fingerprint, seed derivation,
the fill band's draw, or the tape origin. Nothing can detect that a
determinism-affecting change should have bumped the version and did not.

The bump is owed by the COMMIT of a changed artifact, not by a change to the
code that could produce one. `mogwai-lab` depends on `mogwai-data` and never
the reverse, and the generator reads `analysis/fingerprint.json` through
`include_str!` rather than calling the synthesis - so editing the synthesis
cannot move a tape byte until a regenerated artifact actually differs.

PROSE STATING A LIVE IDENTITY IS GATED, because three bumps in a row left
durable statements naming a superseded one. Two phrasings, and only these two,
are checked against the constant by
`crates/mogwai-data/tests/tape_version_prose.rs`, over every markdown file in
the repository:

- ``TAPE_PROTOCOL_VERSION` is N` - N is the identity right now.
- ``TAPE_PROTOCOL_VERSION` next takes N` - N is the next unspent identity.

Write a live claim in one of those forms and the next bump cannot forget it.
Every other phrasing is read as a HISTORICAL record of a past landing and is
left alone, which is what keeps a frozen spec's "stays 11" from being rewritten
out from under the contract it froze.

ONE RECORDED EXEMPTION, granted 2026-08-08 and deliberately narrow. The
correction of `analysis/fingerprint.json` at
`empirical_ranges.modal_tick.max` from 0.25 to 0.1 landed WITHOUT a bump.
Grounds: exactly one JSON leaf changed, an exhaustive reader audit found its
sole consumer to be `Scalars::empirical_diagnostics`, which emits advisory
diagnostics and never reaches the generator, and fresh Python and Rust
synthesis agree on every other leaf. Version 12 was at that time reserved for
the protocol-12b mechanism change, so bumping here would have spent a tape
identity on a change that provably moves no generated byte. Since then 12 went
to the arrival-frame calibration repair and 13 to the fill-band decimal
normalization and 14 to the calendar-aware `ReopenGap` crossing repair, so the
mechanism landing took 15 and later tape changes carried it further, so
`TAPE_PROTOCOL_VERSION` next takes 23; the reservation moves,
the exemption's grounds do not. This exemption does NOT generalize to any
other fingerprint leaf:
the rule stays unconditional, and the next leaf needs its own audit and its
own ruling. If a non-tape artifact revision ever needs an identity of its own,
give it a content hash or a method version rather than overloading this
constant.

### The bit-exactness era is closed (owner ruling, 2026-08-09)

The Python-to-Rust rewrite's parity contract and its pinned CPython
conventions existed to prove the port against the Python oracle. That program
completed and the Python retired, so they are HISTORY, not a standing
constraint on new or optimized code:

- Bit-exactness toward the Python-era committed artifacts is no longer an
  obligation. Floats may drift. A change that moves bits in a committed
  artifact is closed by RE-BLESSING the artifact, never by preserving CPython
  arithmetic. The pinned conventions (compensated `py_sum`/`py_fsum`, exact
  integer variance, insertion-ordered accumulation, the ported Mersenne
  Twister, the CPython float repr) remain in the tree as the record of how
  parity was proven; none of them binds future work, and replacing one for
  performance needs no ruling beyond the re-bless it forces.
- What replaces bit-exactness is a two-part contract. DETERMINISM PER BINARY:
  the same seed, config and binary produce the same tape and the same
  measurement, always. STATISTICAL GATES STAY GREEN: the realism gates, fit
  tolerances and representability verdicts are the correctness story, and a
  change that moves generated behavior re-runs them rather than being
  forbidden. Cross-version, cross-toolchain and cross-platform bit
  reproducibility is explicitly NOT promised.
- Exact-equality gates (goldens, transcripts, `cmp`-based identity checks)
  remain useful as free refactoring checks: a change that claims to move
  nothing can prove it cheaply. Where a change legitimately moves output, the
  gate is re-blessed knowingly in the same change, never widened into a
  tolerance to keep an old blessing alive.
- `TAPE_PROTOCOL_VERSION` bumps are FREE: no consumer has ever depended on a
  tape identity, so the unconditional bump rule above stays (it costs one
  integer edit) and forecloses nothing.
- RUNTIME COST IS A FIRST-CLASS CONCERN. A multi-hour computation is
  presumptively a defect to optimize before it is run, not a budget to
  provision for. Performance work may change generated bytes freely under the
  contract above; in particular, algebraic float operations (stable from Rust
  1.98) and `target-cpu` SIMD are sanctioned wherever a serial recursion does
  not forbid reordering, with the re-bless and gate re-runs they imply.

### Standing lessons from the 2026-08 bug-hunt arcs

Two arcs, both closed with zero open findings: a seven-document loop, then an
ELEVEN-DOCUMENT ARC over `notes/bugs-*.md` - five test-suite reports, then six
production-code ones - closed 2026-08-20. What recurred enough to bind future
work is kept here; each arc's round-by-round carry-forward was deleted once its
distillation landed. A LIVE ARC KEEPS A LIVE ONE - the orchestration loop needs
somewhere to put state no agent in it can see, since no agent observes any round
but its own - so `notes/bug-loop-carry-forward.md` existing is the normal state
during an arc, and it is deleted when that arc closes and whatever still binds
has been folded in here. THREE DEFECT FAMILIES account for most of the serious
finds:

- THE FRONTIER FAMILY, six instances. A watermark, cursor or frontier may
  only advance over work whose success the same expression checked. A lookup
  that legitimately returns nothing is exactly as dangerous as a panic, and
  the inverse failure - a fence with no recovery that wedges the watermark
  forever - is the same family. Treat any watermark assignment not guarded by
  the success of the work it covers as a defect on sight. The adapter's
  history pagination states the cursor form of the rule: a timestamp-only
  cursor may advance onto an instant only once every row at that instant has
  been seen; `ExecState::admit_account_snapshot` states the whole-work form,
  where a snapshot that lost rows to a `filter_map` may not retire the
  well-formed ones. The rule generalizes past cursors to WRITES: the adapter's
  receipt book files a command before queueing its frame and retires it only in
  the expression that saw `writer.send` return `Ok`, so what survives an abort
  is exactly what the venue never saw.
- THE GUARD-SCOPE FAMILY, three instances. A permit, lock or guard whose
  scope ends before the work it protects is the frontier defect in reverse,
  and it is visible by asking WHAT IS STILL RESIDENT when the guard drops. A
  guard is not scoped to the work by being alive while the work runs - it
  must be OWNED by the task doing the work, because the awaiting future can
  be dropped first (hyper drops handler futures on client disconnect; a
  running blocking task cannot be cancelled).
- THE VACUOUS-GATE FAMILY, roughly forty-four instances across eleven
  documents and by far the largest: A THING THAT READS AS GATED AND IS NOT.
  Every instance is cheap to find once the shape is named, and invisible
  otherwise, because both halves are green. The sub-shapes, which are the
  reusable part:
  - a guard that reports SUCCESS on the branch it was built to catch (a drain
    stopped before another task could record the close it was watching for);
  - `all()` / `any()` over a collection whose EMPTINESS is the failure mode -
    `all` over an empty iterator is `true`, so the helper returned success
    carrying nothing on exactly the race its doc described;
  - a control that is itself vacuous: a scanner blind to half the syntax it
    exists to scan (`#[test]` is not a substring of `#[tokio::test]`, so a
    source gate read no async test in the workspace and reported zero
    offenders), or a skip/only agreement checked by comparing two strings
    rather than resolving both against the tests;
  - a normalization applied to BOTH sides of an equality that exists to detect
    that very difference;
  - a two-sided contract PINNED ON ONE SIDE ONLY - the eviction close's test
    hand-built the prefixed reason while the venue's own bytes were never held
    against the classifier that reads them;
  - a doc, comment or help text describing a gate WIDER OR NARROWER than the
    gate - a comment promising a test runs a derivation it does not run, a
    constructor's doc asserting an invariant the constructor does not enforce,
    a helper whose comment states a set wider than its call sites (a runtime
    guard installed at two of the six sites that owed it, under a comment
    saying two);
  - and a fix that trades one blind spot for another.
  WHEN A COMMENT SAYS A FUNCTION GUARANTEES SOMETHING, either the function
  guarantees it or the comment is a defect; there is no third reading, and the
  cheap fix is almost always to MOVE THE GUARANTEE INTO THE FUNCTION.
  Prose is the only artifact here with no compiler.

Test and process rules the arcs paid for, a dozen non-biting tests among them:

- BITE-CHECK EVERY NEW REGRESSION TEST: revert the production fix as a TEXT
  EDIT, observe the named failure, restore it as a text edit. Never restore
  with `git checkout -- <path>` - the tree routinely carries other uncommitted
  work in the same file, and that command destroyed it twice. The hazards, all
  paid for at least once:
  - READ WHICH ASSERTION FIRED, not merely that the test went red, and check it
    can ONLY fire for the reason you mean. Four rail assertions survived a
    bite-check because an EARLIER guard inside the measurement loop failed
    first; a loop bounded by an assertion may never evaluate the property
    asserted after it; and a substring two messages share is not a
    discriminator.
  - CONFIRM THE PERTURBED CODE CAN REACH THE ASSERTION. A close-frame test that
    sent `Message::Close(None)` passed against its own defect in 0.02 s and
    looked like a clean pass, because the old code did not match that arm
    either. A test can be VACUOUS AGAINST ITS OWN DEFECT while naming it
    correctly, and "red / green" cannot tell the difference.
  - GUT THE CALLEE, NOT THE CALL SITE. Emptying a call site does not compile
    under dead-code deny, so the perturbation goes in the callee, imports
    included.
  - DERIVE THE WITNESS FROM THE SYSTEM when the defect has MEASURE ZERO on the
    obvious inputs, and beware a perturbation whose outcome is a coin flip - a
    test resting on `HashMap` iteration order passed against its defect half
    the time.
  - AND WHERE A CLEAN REVERSION IS IMPOSSIBLE, OR THE TEST CANNOT BITE TODAY,
    SAY SO IN THE RECORD. An honest "this cannot bite, and here is what does
    gate it" is worth a great deal; a perturbation that proves nothing looks
    exactly like a test that cannot fail.
- DIAGNOSTIC RULES, from findings that were argued wrong before they were
  measured right:
  - A DIAGNOSIS THAT MUTATES IS NOT A DIAGNOSIS, and a fix that makes a
    property true on one clock does not make it true on the OBSERVABLE.
  - WHEN A FIX SPLITS A PREDICATE INTO TWO HALVES, every other reader of the
    old half is a call site of the fix. When a rule is added to both sides FOR
    A REASON, the next rule in the same commit inherits that reason. A rule's
    blast radius is established AT THE CALL SITES, never by grepping.
  - THE SITE THAT DECIDES A BEHAVIOUR MAY NOT MENTION THE THING IT DECIDES FOR,
    so reach it through the behaviour rather than through a name you can grep.
    Read the validator, not the path.
  - CHECK THE CALLEE'S `unreachable!`s BEFORE REUSING IT FROM A NEW CALL SITE.
    A panic path genuinely unreachable from one caller is a live hazard from
    the next, and the compiler says nothing.
  - RESOLVE A VERSION-HISTORY SENTENCE AGAINST `git show <ref>:<path>`, never
    against memory - one such sentence shipped false, and the prose gate is
    blind to historical phrasings by design. GREP THE NUMBER, NOT THE FILE: a
    figure corrected in the documents a reviewer was looking at is not
    corrected everywhere it was written.
- A TEST THAT NAMES WHICH BRANCH RAN NEEDS AN OBSERVABLE, NOT A SLEEP. A wait
  of some fraction of a poll interval does not select an arm, it BETS on the
  scheduler, and the losing bet is usually the silent one: the other arm does
  equivalent work, the assertion passes, and the test stops testing the fix
  without ever failing. Give the production code a counter or a state the test
  can read, and where the interval itself is the race, make the interval a
  parameter and pass one the test cannot lose to. The launcher's owner loop is
  the worked example: `LaunchedVenue::polls` plus `launch_with_poll`.
- THE LANE AND PROFILE SPLITS BITE, and they are two splits, not one. `brokkr
  check` runs tests in DEV and MULTI-THREADED; `brokkr test` runs them in
  RELEASE at `--test-threads=1`. A test pinning `debug_assertions` behaviour
  must be gated `#[cfg(debug_assertions)]` or the release sweep fails it; a
  test whose bite depends on optimization must be checked in release; and a
  green run in one lane is not evidence about the other, so bite-check in the
  lane that is disputed.
  LIBTEST SPAWNS A FRESH NAMED THREAD PER TEST UNCONDITIONALLY on any threaded
  target - the name is how a panic is attributed to a test - so
  `--test-threads` caps how many run AT ONCE and not whether a thread is made.
  A `thread_local!` is therefore per-test-isolated in EVERY lane. Measured
  twice, and a cold reviewer's contrary model (that the serial runner runs
  tests inline on the main thread) was refuted by probe rather than by
  argument. TREAT IT AS TRUE TODAY AND NOT AS A CONTRACT: it is an
  implementation detail that has changed once, the failure if it changes back
  is silent and wholesale, and nothing detects it - which is why the adapter's
  `common::owns_a_fresh_exec_sink_on_every_lane` asserts the premise directly.
  A PATH IS PER-PROCESS OR IT IS A SHARED RESOURCE: a test that wrote and then
  exec'd a FIXED path aborted a parallel sweep with `ETXTBSY` when another
  process ran the same test. And when comparing flake records, THE THREAD COUNT
  IS PART OF THE RECORD; three green runs are not evidence about an
  intermittent race.
- RUN THE SOCKET SUITES AFTER ANY CHANGE TO THE SERVING PATH. `brokkr check` is
  blind to roughly thirty tests that bind loopback listeners, and a real
  regression shipped through that gap and stayed red across four commits -
  eviction on the default account closing a client's own second socket, which
  only the socket suite surfaced. `brokkr check --gate` is the invocation that
  covers them, and it is the one to run. It was red on master through
  2026-08-16 for a coverage-audit reason that was never a property of this
  workspace - a brokkr bug reporting every `mogwai-data` test as orphaned under
  the `instrumented` sweep - and a brokkr build dated 2026-08-17 fixed it. If it
  ever goes red that way again, suspect the tool before the tree - BUT CHECK
  FOR A CRASHED TEST FIRST, because the two readings are indistinguishable in
  the output. A single test aborting the parallel sweep produces the same
  wall of orphaned `mogwai-data` tests, and a real regression can hide behind
  either reading; 2026-08-19 saw exactly that, from an `ETXTBSY` on a test that
  wrote and then exec'd a FIXED path while the gate's second sweep ran the same
  test in another process. A path is per-process or it is a shared resource.
  Meanwhile run the socket suites by name:
  `brokkr test -p mogwai-adapter "" --debug` and
  `brokkr test -p mogwai-cli socket --debug`.
- COMMIT OR STASH BEFORE READING A `brokkr test -p mogwai-cli ""` RESULT. That
  filter catches `arrival_control_refuses_a_tree_that_changed_during_the_run`,
  which refuses a DIRTY TREE by design and FAILS rather than skips, so at a
  glance it is indistinguishable from a real regression.
- AUDIT THE SEAM ITSELF: a test double must be verified against the real
  endpoint's semantics, not against what the test needs. A stub that replays
  queued responses whatever the client asked for is blind by construction;
  serve real semantics and record the requests so tests can assert the
  request sequence too.
- A test observing only an ERROR cannot distinguish a bound from a check
  performed after the damage; assert on the resource the finding named.
- TWO IMPLEMENTATIONS A GATE COMPARES ARE PINNED BY ONE SHARED FIXTURE, never
  by a hand-built case list on either side. Where a gate holds a corpus
  measurement against a synthetic one, the quantity is computed TWICE, and if
  the two conventions drift the gate silently compares different quantities and
  still passes - the failure is invisible because both halves are green. A
  fixture built on one side cannot catch this: it pins that implementation
  against ITSELF. The convention, three instances so far, is a versioned
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
  WHERE THE TWO SIDES SHARE A CRATE GRAPH, A MODULE IS THE CHEAPER FORM OF THE
  SAME DISCIPLINE. `mogwai_protocol::close` carries the WS close vocabulary -
  the code, the reason constants and `classify` - and the venue WRITES its
  reasons from those constants while the adapter READS them through `classify`,
  so neither side holds a literal the other could drift from. The gate on such
  a contract must run one side's REAL OUTPUT through the other side's reader: a
  test that hand-builds the input it then classifies pins one side against
  itself, which is exactly how a missing eviction prefix survived with both
  halves green.
  NOTHING DETECTS A MISSING FIXTURE. The next cross-implementation gate is
  caught by this habit or not at all, which is the same shape as the open item
  on durable prose asserting a live fact. AND NO FIXTURE DETECTS ITS OWN
  TOLERANCE QUIETLY WIDENING: the version is a schema version, a tolerance
  edit weakens both implementations at once, and the second implementation is
  structurally blind to it - review a tolerance change as a contract change,
  because no gate will.
  AND THE FIXTURE IS NOT ALWAYS THE ANSWER. A two-copy gate whose copies are
  the SAME algorithm rather than two conventions for one quantity is closed by
  deleting a copy, not by anchoring both - `mogwai-lab`'s `summary` and
  `session` session-segment math was one such, collapsed in the lab/cli
  test-hunt. And where the two sides produce a whole accumulator record rather
  than a statistic, no units-neutral statement of it exists; anchor the
  INPUTS instead, which is usually where the drift is.
- TWO CONSTANTS ENCODING ONE QUANTITY ARE THE SAME DEFECT WITHOUT THE GATE,
  and they are easier to miss because neither looks like an implementation.
  `mogwai-lab`'s `subcontract` carries the final measurement window's length
  twice - as a nanosecond difference and as a seconds string - read by
  different consumers, so editing one moved the window the exposure walk
  measures while every gate stayed green. Where neither encoding can be
  derived from the other, assert the identity between them, and make a test
  that claims to catch a constant's drift read THE ENCODING ITS SUBJECT READS.
  A frozen-snapshot hash over all the constants is not this gate: the
  sanctioned way to move one re-blesses the hash in the same change.
- A test on a `/ws` socket may never assert on THE NEXT frame: every socket
  is attached to the live tape on upgrade, so drain to a deadline. AND A DRAIN
  THAT DOES NOT RECORD HOW THE STREAM ENDED IS NOT A DRAIN: `while let
  Ok(Some(Ok(m)))` folds a close, a stream end, a transport error and a
  deadline into "the loop finished", and a truncated drain satisfies an
  ABSENCE assertion for free.
- PROCESS FACTS, which govern how much a green anything is worth:
  - A CLOSE PASS OVER A WHOLE ARC IS NOT OPTIONAL, because the round shape -
    fix pass, gate, cold review, then a fix-and-commit agent that closes the
    review's findings AND commits - leaves THE SECOND HALF OF EVERY COMMIT
    UNREVIEWED. Eleven documents, eleven close passes, and the defect was in
    that half ELEVEN TIMES OUT OF ELEVEN. Point the close pass at the durable
    prose the round touched and at THE CALL SITES OF WHATEVER THE ROUND'S LAST
    FIX INSTALLED: that code is the least-examined in the arc and it is the
    code closing a finding.
  - A GREEN GATE PROVES NOTHING ABOUT A TEST THAT CANNOT FAIL, and a consensus
    review gate converges to the verifier's utility function - a clean cold
    review was followed by a serious find in a later pass five times. A green
    review is evidence, not proof.
  - MEASUREMENT OVERTURNS CONFIDENT ARGUMENT, in both directions and
    repeatedly: cold reviewers were refuted by probes, and reports arrived
    whose findings were already fixed in the tree they were written against.
    Measure the disputed thing in the disputed lane before conceding or
    dismissing.
  - THE CARRY-FORWARD IS THE ARTIFACT MOST LIKELY TO BE SKIPPED, because the
    agent that lands the code and the report is the last hand on both and no
    agent in the loop reads the carry-forward back. A round whose lesson is
    not written there did not happen as far as the next arc is concerned.
  - `analysis/asia_jump_probe.py` IS THE OWNER'S UNTRACKED WORK IN PROGRESS.
    Some twenty cold reviewers have now raised it unprompted and several
    proposed deciding its fate; the standing answer is that it is out of scope
    and stays untracked, and it is never swept into a commit. One thing from
    that noise binds: its percentile convention does not match
    `mogwai_lab::kernel::nearest_rank_list`, so a number out of that script
    must never land in a durable document labelled "p95" and be compared
    against a Rust-computed one - two implementations of one quantity, no
    shared fixture, both sides green because neither is checked.
  - `reference/INVENTORY.md` DOES NOT EXIST in this repository. The generic
    orchestration workflow names it as a mandatory read; it is an
    unsubstituted variable from another repo. Do not put it in a brief. The
    real contracts are `AGENTS.md` and `CLAUDE.md`, then
    `reference/architecture.md`, `clock.md`, `glossary.md`, `performance.md`
    and `technical-implementation-spec.md`.

### Reading vs depending on nautilus_trader and broadarrow

The broadarrow adapter (and any spec or implementation that touches the nautilus
or broadarrow APIs) has two distinct access paths - never conflate them:

- Read the source from the in-tree copies `research/nautilus_trader` and
  `research/broadarrow`. Agents cannot read anything outside this repo, so these
  copies are the only place to study those APIs.
- Build against the PUBLISHED crates.io release, version-pinned in
  `mogwai-adapter/Cargo.toml` with default-features off so no pyo3 or Python
  linkage is pulled in. A build needs no sibling checkout: cargo fetches the
  five nautilus crates like any other dependency and `Cargo.lock` pins them by
  version and checksum. broadarrow is never a build input at all - it is the
  consumer that depends on this workspace, not the reverse. `research/` is
  read-only reference, never a build input; `members = ["crates/*"]` already
  excludes it, so no workspace `exclude` is needed.

The adapter previously path-depended a sibling `../nautilus_trader` checkout
because the published release carried bugs this project hits. Those fixes landed
in 0.61, so the manifest pins a published release - 0.62.0 as of 2026-08-21 -
and the build is reproducible from a fresh clone. The `research/` copy is kept
at the matching upstream ref, so what you read there is what compiles.

Every implementation spec that references these APIs states both paths: the
implementer reads from `research/` and builds against the pinned release. The
two are kept in sync, so what you read in `research/` is what compiles.

### Bash rules

- Never chain commands with `&&`.
- Never chain commands with `;`.
- Never chain/pipe commands with `|`.
- Never capture stdout into env vars (`UUID=$(...)`).
- Never read or write from `/tmp`. All data lives in the project.
- Never run raw `cargo`, `curl`, `pkill`. Use `brokkr`.

## Commands

Use `brokkr` (not `cargo`) for check/test. By default output is filtered to changed files and capped at 20 diagnostics per phase.

- `brokkr check` - gremlins + clippy + all tests (changed-files scope). Does NOT
  run the four socket-backed adapter test binaries (`adapter_smoke`,
  `data_client_transport`, `havoc`, `reconciliation`): they are `#[ignore]`d
  because they bind real loopback listeners, so an environment without sockets
  would fail them for reasons unrelated to the code. Fast and sandbox-safe, and
  blind to roughly 30 tests.
- `brokkr check --all` - show every diagnostic, no cap, no scope filter
- `brokkr check -p <crate>` - scope to one package (e.g. `-p mogwai-engine`). You generally do not want to run this; a single `brokkr check` is faster than 2-3 `-p` runs, and brokkr intelligently filters which warnings and errors to show you
- `brokkr check -- --test <file>` - forward args to `cargo test` (args after the second `--` go to the test binary)
- `brokkr test -p <crate> <NAME>` - release-mode focused single-test runner. Always passes `--release --include-ignored --nocapture --test-threads=1`. `<NAME>` is a case-sensitive substring filter (matches both unit and integration tests). Streams the test's own stdout/stderr live and prints a `[test] PASS/FAIL` footer with wall time. Defaults to `--all-features`.
  - `-p, --package <PKG>` - cargo package. Required in this workspace - no default package, and overrides `[test] default_package` in `brokkr.toml` if set.
  - `-N, --repeat <N>` - run the test N times per sweep (flaky-test hunting).
  - `-j, --jobs <N>` - parallel cargo compile jobs.
  - `--raw` - bypass output filtering, print everything cargo emits.
  - `--debug` - build and run the test in dev profile instead of release. Use this for subprocess-lifecycle / IPC / boot-path tests where release-LTO compile time (3-4 min for the full workspace) dominates wall time and the optimization level doesn't change the behavior under test. `BROKKR_TEST_BIN_DIR` points at `<target>/debug` accordingly.
  - Example: `brokkr test -p mogwai-data memory_source_replays_in_time_order` or `brokkr test -p mogwai-data parses_integer_and_fractional_timestamps -N 5`.
- `brokkr run [NAME] [ARGS]...` - runs a bin or example by TARGET NAME, discovered from cargo metadata across the whole workspace. This venue is `brokkr run mogwai -- serve` (`mogwai` is the bin target, not the package). A bare `brokkr run` lists what is runnable, or runs `[bin] default` if set. Arguments after `--` are forwarded raw to the program; brokkr's own `--debug`/`--release` go before the name. Use instead of `cargo run` for the same reason as `brokkr check`/`brokkr test`.

## Benchmarking

`brokkr mogwai` measures, `brokkr results` queries the durable record, and
`brokkr sidecar` queries the profiler store. In depth: `brokkr man mogwai`
and `brokkr man results`. The design and what is deliberately deferred:
`notes/benchmarking-design.md`. What each surface emits and the annotation
discipline: `reference/performance.md`.

There are no layers and no frozen workloads. The argv is composed at the
CALL SITE and captured verbatim in the row, so selecting an arm is a
query rather than a name lookup. Recording needs a clean tree: `--force`
runs a dirty tree but stores no `results.db` row (its sidecar data
survives, reachable as the `dirty` pseudo-UUID).

### The two surfaces

**Argv-shaped**, through the shipped `target/release/mogwai`, needing no
registration - `gen` and its `--type` variants, `tick-composition`,
`preflight`, `measure`, `fit`, `cache`, `synth`, `arrival-screen`. The
argv goes after `--`, raw:

    brokkr mogwai --bench 3 -- gen --type summary --symbol MNQ

Benching the release binary measures what ships, startup and argument
parsing included, which is the honest end-to-end number.

**Harness-shaped**, through a cargo example, for the loops that have no
command line - the engine's matching loop and divergence seam, the
`TickSource` implementations, the arrival draw, the screen's projection,
and eventually the serving path and the adapter. These resolve by NAME
against `[mogwai.targets.*]` in `brokkr.toml`:

    brokkr mogwai screen_projection --hotpath
    brokkr mogwai arrival_walk --alloc

Harnesses take an argv too, after `--`, because every surface here is
config-shaped (preset, window, seed, cell). Bare `brokkr mogwai` lists
both kinds. Adding a surface to the measurable set is registering a
target.

Currently registered:

| target | example | features |
|---|---|---|
| `arrival_walk` | `mogwai-data/arrival_walk_bench` | `hotpath` |
| `screen_projection` | `mogwai-lab/screen_projection_bench` | `hotpath` |
| `ring_sizing` | `mogwai-lab/ring_sizing` | `hotpath` |
| `stage_a_batch` | `mogwai-lab/stage_a_batch_bench` | none |
| `envelope_evaluation` | `mogwai-lab/envelope_evaluation_bench` | none |
| `tag_decode_probe` | `mogwai-protocol/tag_decode_probe` | none |
| `tape_lateness` | `mogwai-cli/tape_lateness_bench` | none |

`tape_lateness` is the one that used to be a TEST. It asserted a 50 ms p99 on
paced delivery, which is a statement about the host rather than about the code,
and it was excluded from every lane that would have run it. The reading is worth
recording every run; the threshold was never portable. That is the general
shape: a wall-clock budget with no admission test is a measurement, not a gate.

### The three modes, uniform over both surfaces

- `--bench [N]` - N runs (default 3), lockfile, stores a row. A plain run
  stores nothing.
- `--hotpath [N]` - function-level timing.
- `--alloc [N]` - per-function allocation bytes, exclusive of nested
  calls.

`--hotpath` and `--alloc` are INERT without the feature that compiles the
instrumentation in, which is what the registry's `features` field exists
for. The registered features are a UNION with what the mode and call site
add, never a replacement: `--hotpath` contributes `hotpath`, `--alloc`
contributes `hotpath-alloc`, and a call-site `--features` adds an arm.

The consequence worth knowing, and it bites: **a target registering
`hotpath` has its `--bench` walls measured on an instrumented build.**
Register the feature on the target that needs it, not on every target. A
harness whose canonical output is a wall rather than a profile should
carry no registered features and no cargo `required-features`.

Other flags that matter: `--commit <REF>` builds and benches an old
commit, `--dry-run` validates argv and path resolution without building,
and `--stop <MARKER>` kills the child when a sidecar marker fires, for
benching one phase.

### Reading the record

`brokkr results` prints the last `-n` rows (default 20), newest first.
Filters AND together: `--commit`, `--command`, `--mode` (`bench`,
`hotpath`, `alloc`), `--dataset`, and repeatable `--meta KEY=VALUE` /
`--env NAME=VALUE`. Both `--meta` and `--env` EXCLUDE rows missing the
key, so an arm defined by an unset variable needs an explicit baseline
value recorded on the off runs.

`--grep` / `--grep-v` match against the whole INVOCATION - subprocess
argv, brokkr argv, and each captured env var rendered as `NAME=VALUE`.
Repeatable, `git log --grep` style: every `--grep` must match, any
`--grep-v` hit excludes. That composition is the only way to select an
arm defined by an ABSENT flag.

`brokkr results <uuid-prefix>` resolves to one row and prints a labelled
block: full `cli_args`, the brokkr invocation, per-iteration walls for a
`--bench N` row, the scraped counters, the `prev.*` provenance of what
ran immediately before (often the explanation for an outlier), and the
hotpath and alloc tables when the mode captured them. `--top N` caps the
functions shown.

`brokkr results --compare A B` pairs rows on
`(command, mode, input_file, brokkr_args, env_fingerprint)`, with
`--commit` and `--verbose` stripped from `brokkr_args` first. Pairs whose
host conditions or captured env differed are ANNOTATED rather than
rendered as a clean delta.

### Counters, and why they are not optional

Every external run scrapes the winning run's stderr for `key=value`
counters. `--compare` reports the ones that moved:

    counters: cells_evaluated 5000 -> 4000, prints 1240 -> 1180

A wall alone cannot distinguish "the code got faster" from "the code did
less". This is what turns "12 % faster" into "12 % faster on 8 % fewer
cells", and it is why a benched surface should emit its WORK SIZE. It is
reported, never fatal. Where a moved count really does invalidate a
series - a seeded tape whose draw moved - that owes a
`TAPE_PROTOCOL_VERSION` bump, which is unconditional and cannot be waived
per comparison.

### The sidecar

`brokkr sidecar <uuid>` queries `.brokkr/sidecar.db` - the observation-only
channels the benched commands report through (`mogwai-lab`'s `sidecar`
module). A UUID prefix is required; find one with `brokkr results`. Views:
`--markers` and `--durations` (START/END pair timings), `--counters`
(with `--grep` to filter a noisy dump), `--samples` (the raw 100 ms /proc
stream, with `--phase`, `--range`, `--where`, `--fields`, `--every`,
`--head`, `--tail`), `--stat <FIELD>` for min/max/avg/p50/p95, `--stalls`
for `*_wait_ns` rollups, and `--compare A B` phase-aligned. `--human`
renders a table instead of JSONL.

### Datasets

`[<host>.datasets.<name>]` in `brokkr.toml` records an out-of-git input
by path and XXH128 digest - whether the bytes moved under a recorded row.
This is NOT a substitute for the run's own content verification, which
asks whether the data is what the ledger says. Register the path, run
`brokkr env`, and paste back the digest it computed from disk. A `path`
may name a directory, which digests as the sorted fold of
`<relative path>\0<file digest>`, so a rename or a layout change moves
it.

## Document folders

The standing layout, across every project. Three live folders plus one retired,
split by durability first, subject second.

| Folder | Contents | Rule |
|---|---|---|
| `reference/` | Durable in-repo reference for anyone working on or with the code - how the thing is built and why: `architecture.md`, `technical-implementation-spec.md`, `performance.md` (the durable record of measured numbers over time), invariants, protocol contracts | Citable from source as a source of truth. What it says must be true. |
| `docs/` | Durable in-repo documentation of how the thing is used - guides, CLI reference, the consumer-facing API surface. Sometimes exposed as a hand-edited VitePress gh-pages site | Same must-be-true rule. |
| `notes/` | Transient - work items (`todo.md`), future plans, hypotheticals, bug reports, research, analysis. Things that will die | No truth guarantee. Nothing durable cites it. |
| `plans/` | Retired | Plan documents are transient: they go in `notes/`. |

`reference/` and `docs/` are both durable and both binding. The difference is
subject, not audience: `reference/` covers how the thing is built and why - what
you need in order to change it safely - while `docs/` covers how it is used. A
developer or library consumer reads both. Where a project publishes a site,
`docs/` is what gets published; the folder means the same thing either way.
`notes/` is neither durable nor binding, which is the whole point of keeping it
separate: a document that may be wrong must not sit where a document that must
be right is expected.

The dependency direction is therefore one-way. `notes/` may cite `docs/` and
`reference/`; nothing durable may cite `notes/` - not a code comment, not
`docs/`, not `reference/`. A code comment must carry its full context, because
it outlives the note.

**Root-level convention files are exempt.** `AGENTS.md`, `CLAUDE.md`,
`README.md`, `LICENSE`, `CHANGELOG.md` and their kin are found by tooling and by
convention at the repository root, and stay there. These folders govern
documents we chose where to put, not files whose location is dictated.

In `notes/`, `docs/` and `reference/` alike, avoid citing source line numbers -
they drift fast.
