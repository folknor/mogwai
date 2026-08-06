# The Python-to-Rust rewrite, phased

Owner-defined program, 2026-08-06. Transient notes-class plan; the
parity gates below are the verification story - the Python originals
stay runnable beside the port until phase 4, so every phase is
checkable against a committed artifact rather than against review
effort. Deep verification lands at the parity gates and the final
codex pass, not per-move. Script triage grounding this plan:
`notes/python-script-triage.md`.

Agent assignments (owner rule 2026-08-06: sonnet wherever it can
handle the phase - faster, slightly dumber - opus where silent
subtlety lives): SONNET for phases 1, 2c, 3a and 4 (spec-shaped
porting and wiring that fails loudly at its parity gate); OPUS for
2a (float conventions and the two-implementation unification - the
silent-rot surface), 2b (Amendment-D refusal-ownership and
fail-closed ladder semantics) and 3b (the CRN solve invariants).
Phase 0 ran on opus.

Crate names, owner-picked register: `mogwai-cli` (the one bin, target
name `mogwai` - load-bearing for brokkr run, the launcher, mnq_fit.py
and the docs) and `mogwai-lab` (the method library). No custody crate:
the Databento tooling stays Python permanently (owner ruling
2026-08-06 - it went through its reviews and works); the lab carries a
small custody module for hash/ledger READING only.

## Phase 0 - carve mogwai-cli (in flight)

`main.rs`, `gen.rs`, `measure12a.rs` and the CLI-facing modules move
from `mogwai-server` to a new `crates/mogwai-cli`; the server becomes
a library crate exposing the minimal surface. No behavior change, no
touch to `mogwai-data`. Gate: the existing suite green including the
socket-backed binaries (`brokkr check --gate`, minus its one
pre-existing documented failure) and the named measure12a release
tests; `target/release/mogwai` still exists under that exact name.
Committed on green without deep audit - mechanical, history-recoverable.

## Phase 1 - mogwai-lab: the corpus layer

TBBO csv.zst stream contract (parse refusals and all), parent
inference, session/segment math UNIFIED (today it exists three times:
mnq_fit.py, the moved gen.rs session_segment_at, mogwai-data's
calendar), delivered-file hash verification and ledger binding,
preflight with the purity gates. Gate: `mogwai preflight` reproduces
the committed `analysis/out/mnq-fit-preflight.json` from the delivered
July corpus exactly - rows 35,187,061, book counts, file hashes,
usable sessions, subcontract hash.

Phase 1 also lands the STORAGE POLICY (owner requirement 2026-08-06:
the repo-tree-plus-git-commit scheme does not translate to a
cargo-installed binary). Three classes, never mixed:

- ARTIFACTS (preflight, measurement, fit outputs): the user's files,
  written to --out or the working directory, never cached, never
  auto-deleted.
- CACHE (recomputable, keyed - walk summaries, measure12a walk
  records): default $XDG_CACHE_HOME/mogwai/ falling back to
  ~/.cache/mogwai/, overridable by MOGWAI_CACHE_DIR or --cache-dir.
  Keys replace the git commit with a PROVENANCE TOKEN the binary
  carries intrinsically: crate version + TAPE_PROTOCOL_VERSION +
  fingerprint hash + full command + subcontract hash, with a build
  script folding the git sha in when built from a tree so repo dev
  keeps current invalidation strength. Stale-provenance entries are
  unreachable by construction and are pruned automatically on write;
  a `mogwai cache` subcommand (stats / clean / clean --stale) covers
  the manual case. The 14,288-file mnq-fit-scratch is the cautionary
  exhibit.
- SCRATCH (per-run temporaries): a run-scoped directory under the
  cache root, deleted on clean exit, ignorable on crash.

Repo dev pins MOGWAI_CACHE_DIR repo-local (the parity gates of
phases 1-2c read the Python-era caches at their current analysis/out
paths via --cache-dir), so the project's all-data-in-the-project rule
holds without being the installed default.

## Phase 2 - the 12a measurement engine (three launches)

The Python/Rust twin (mnq_fit.py blocks vs measure12a.rs) dies here,
taking its cross-language float-divergence defect class with it.
Subdivided on the intermediate caches Brick M already produces, so
every launch has a parity gate on day one. mnq_fit.py stands at
9,515 lines; each launch below is a 1,500-3,000-line scope, and the
2,000-line selftest does not port as a unit - it dissolves into each
subphase's own Rust tests.

- **2a, the deterministic kernel and block engine**: splitmix64 and
  tuple_mix, Fisher-Yates, nearest-rank / weighted-nearest-rank / the
  ceil-n-over-2 median, typed-canonical serialization, the nano-unit
  log-mid arithmetic, and the UNIFIED Blocks 1-4 session engine (the
  _M12aSession port merged with measure12a.rs) plus permutation
  records and Block-5 forensics. Gate: per-session records
  typed-canon-identical to the observed cache
  (analysis/out/mnq-measure12a-observed.json) on the observed side
  and to the walk caches (analysis/out/measure12a-cache/, cost
  excluded) on the generated side.

  LANDED 2026-08-06. `mogwai_lab::kernel` carries the deterministic
  kernel and the typed-canonical comparator;
  `mogwai_lab::measure12a` carries the one engine, with `observed`
  and `generated` as its two front-ends over a shared `SessionAcc`
  and `forensic` as Block 5. The gates are
  `crates/mogwai-cli/tests/parity12a.rs`, ignored and named
  `parity12a_*`; they live in the cli because the generated side
  needs `mogwai-server` preset resolution and the lab must not
  depend on it. Both passed: observed 22/22 sessions in 83 s,
  generated seeds 1-8 in ~26 s each. `brokkr.toml`'s complete
  profile skips `parity12a_` - they are minutes apiece and need the
  delivered corpus on disk.

  Two findings from the gate work, both fixed rather than mirrored:
  serde_json's DEFAULT float parser is not correctly rounded (it
  lands one ULP off on values the 12a records actually carry), so
  the workspace now pins its `float_roundtrip` feature; and
  `subcontract.rs` carried a second hand-written CPython float
  `repr` with the wrong exponent-switch threshold, now deleted in
  favour of the kernel's one copy.
- **2b, aggregation and inference**: monthly pooling, ObsContext, the
  10,000-replicate bootstrap, family envelopes, closures, the count
  substitution, forensic subchecks, the ladder, the union-zero
  histogram centralization. Gate: reproduce the committed artifact's
  bootstrap and ladder sections from 2a's records.

  LANDED 2026-08-06. `mogwai_lab::aggregate` carries it in seven
  modules: `monthly` (the block pooling, the permutation combination
  and the union-zero central blocks), `bootstrap` (the fixed-seed
  circular block draw, the ISO-week folds, the weighted median and
  `QuantileSupport`), `context` (`ObsContext` and its vote caches,
  including the two STRICT no-K-of-N accessors), `family` (the stat
  closures, the Q1 helpers, the 6.4 inventories and the Amendment-D
  envelope), `countsub` (the 5.2 substitution and the 5.3 gap
  closures), `ladder` (the six rungs, the closures, the forensic
  subchecks) and `assemble` (the 2b-owned artifact sections, shaped
  so 2c pastes rather than re-derives). `expected_scheduled_windows`
  and the label-derived segment bounds joined `session.rs`.

  The gate is `crates/mogwai-cli/tests/parity12a_aggregate.rs`,
  ignored and named `parity12a_aggregate_*`: from the cached observed
  per-session records, the eight walk records and
  `bootstrap_multiplicities(22)` it reproduces `observed.monthly`,
  `observed.permutations_monthly`, the whole `bootstrap` section, the
  whole `ladder` section, every seed's `blocks` and
  `count_substitution`, `generated.central` and
  `diagnostics.worsening_23` typed-canon-identically. It passes in
  2.2 s against the Python's roughly 10 s, and asserts the artifact's
  dark corners by name - the incomplete arrival family with its five
  force-refused conditional metrics, the null `worsening_23` from an
  unfired reversion rung, the no-family-eligible verdict.

  One finding, and it is the kind this program exists to catch:
  **CPython's builtin `sum()` over floats is not a naive left fold.**
  Since 3.12 it applies improved Kahan-Babuska - Neumaier -
  compensated summation. A plain fold reproduced every point estimate
  but landed the bootstrap `se` several ulps off, and through the
  simultaneous critical value that error reached every interval in
  every family. `mogwai_lab::kernel::py_sum` is that compensated
  summation, used wherever the Python writes `sum(...)` over floats:
  `stdev_ddof1` and the count-substitution weight sums. A Python `+=`
  loop is a naive fold and must stay one, so the two spellings are
  now semantically different in the port exactly as they are in
  CPython.

  A second, quieter hazard the port had to pin: several float
  accumulations walk a `dict` in INSERTION order, so
  `aggregate::monthly::PooledHist` is an insertion-ordered map rather
  than a `BTreeMap`. Sorting those walks moves the last ulp of
  `counterfactual_exceed_968`.
- **2c, assembly and the CLI** - resized 2026-08-06 after the first
  2c launch correctly refused the scope as exceeding the per-launch
  budget; now THREE slices, each with its own gate:
  - **2c-i, assembly and validators**: assemble_measure12a_artifact
    with the 10,000-replicate hard gate, the recursive schema
    validator, the semantic gates, load_brick_g_walks, json_safe
    atomic writing. Gate: assembling FROM THE CACHES (observed half
    plus the eight walk records - no live corpus pass, no walks)
    reproduces the committed artifact typed-canon-identically minus
    cost and binding.harness_tree_commit; the validator returns empty
    on it and rejects the mutation battery.

    LANDED 2026-08-06. `mogwai_lab::aggregate::artifact` carries
    `assemble_measure12a_artifact` (the hard 10,000-replicate gate)
    over `assemble_measure12a_body` (the internal, ungated form a
    truncated fixture goes through directly), the recursive
    `measure12a_schema_errors`, `measure12a_semantic_errors`,
    `load_brick_g_walks` (seed-indexed, refusing absence/ambiguity/
    malformed types) and `json_safe`/`write_json_atomic`. It lives
    inside `mogwai-lab` rather than `mogwai-cli` because this slice
    needs no `mogwai-server` preset resolution - assembly is a pure
    function of the cached records - so the gate is
    `crates/mogwai-lab/tests/parity12a_i.rs`, ignored and named
    `parity12a_i_*`; it passes in ~6.5 s
    (`brokkr test -p mogwai-lab parity12a_i`), reading the observed
    cache and the eight Brick G walk records and reassembling the
    artifact with a provisional cost record. The negative batteries
    (truncated-bootstrap refusal on the public entrypoint, mixed-type
    usable-list refusal, an injected uncontracted field, and the
    six-mutation review battery) are asserted in the same test against
    the real assembled artifact as the mutation baseline, rather than
    against a hand-built Python-selftest-style fixture.

    One three-way finding, resolved rather than escalated: the
    coordinator's brief excludes "the cost object" from the compare,
    naming the top-level `cost` key; the committed artifact's
    `generated.per_seed[*].cost` (`walk_s`/`rss_bytes`, pasted verbatim
    from the Brick G cache file) diverges from what is on disk in
    `analysis/out/measure12a-cache/` TODAY, because that per-seed cost
    is itself a live wall-time/RSS measurement of whichever walk most
    recently populated the cache file for that seed - the same
    reproducibility class as the top-level `cost` object, not a
    section-10 statistic. The gate excludes it too, for the same
    reason the brief excludes the top-level object; every other field,
    including `binding.generated` and every other `binding` key, is
    compared byte-of-meaning.
  - **2c-ii, the live run**: the resource sampler, fresh-tree gate,
    in-process attestation walks, the observed pass, `mogwai
    measure`. Gate: the full live run from the clean tree reproduces
    the artifact minus the honestly-live fields, budgets measured.

    LANDED (code) 2026-08-06, GATE NOT YET DEMONSTRATED PASSING - see
    below. `mogwai_lab::sampler::ResourceSampler` carries the 1 s
    background RSS/scratch sampler (`/proc/<pid>/status` VmRSS summed
    over the live process tree, `/proc/<pid>/task/<pid>/children` for
    descendants - moot for the in-process walks today, kept general
    for a future subprocess caller); a dead sampling thread refuses
    `stop()` rather than reporting a partial-window peak.
    `mogwai_lab::preflight::require_preflight` joined the existing
    `run_preflight`/`write_json_atomic` (the `(preflight JSON, sha256
    of the artifact file bytes)` cross-check `run_measure12a_observed`
    needs). `crates/mogwai-cli/src/measure.rs` is the driver -
    `MeasureConfig`/`run_measure` - wired as `mogwai measure` with
    `--corpus`/`--ledger`/`--preflight`/`--cache-dir`/`--out`, and
    `crates/mogwai-cli/src/lib.rs` is new (the binary previously had
    no library target) purely so the golden-gate test can call
    `run_measure` directly instead of shelling `target/release/mogwai`
    - every other subcommand stays module-private under `main.rs`.
    `run_measure` ports `mode_measure12a` field-for-field: Brick G
    loads read-only first, the live observed pass cross-checks and
    then overwrites the observed cache, the eight FINAL walks replay
    in-process through `measure12a::generated::GeneratedAcc` (the same
    construction `parity12a.rs`'s `run_final_walk` uses) and
    content-compare cost-excluded against Brick G, the input-side
    population gates, the two-phase cost finalization (provisional
    record assembled in, throwaway `json_safe` serialization pass
    under the running sampler, THEN the sampler stops and the record
    is finalized and the artifact is RE-ASSEMBLED so the pasted `cost`
    is the final one, not the provisional one), both validators, the
    fresh-tree recheck against the bound commit, the atomic write.

    The gate is `crates/mogwai-cli/tests/parity12a_ii.rs`, ignored and
    named `parity12a_ii_*`. It calls `run_measure` directly with `--out`
    redirected to a scratch path under `target/` and `--cache-dir`
    redirected to a scratch COPY of the real `analysis/out` caches (so
    the mandatory observed-cache rewrite never touches the committed
    file's bytes), then compares the result against the committed
    `analysis/mnq-measure-12a.json` with the same three live-field
    exclusions 2c-i's gate uses (top-level `cost`,
    `binding.harness_tree_commit`, each seed's own `cost`).

    STRUCTURAL FINDING, not a Python/Rust disagreement: `run_measure`
    faithfully ports `require_clean_tree` - it refuses outright over
    ANY working-tree diff, by design, because `binding.harness_tree_commit`
    must name exactly the code that ran. That is unavoidably true of
    THIS session's own tree: slice 2c-ii's own new/changed files
    (`measure.rs`, `sampler.rs`, `lib.rs`, the `preflight.rs`/`lib.rs`
    additions) are uncommitted while this slice is under review, so
    `mogwai measure` correctly refuses with "the working tree is
    dirty" when run now - the same chicken-and-egg an artifact-binding
    contract always has during its own development, and the same
    reason slice 2c-i's code had to land at HEAD before this slice
    could be assigned. I confirmed the refusal fires with the exact
    Python-matching message (proving that gate wired correctly) rather
    than fabricating a bypass. `brokkr check` is green (518 passed, 95
    ignored) and `cargo clippy` is clean over both crates. The true
    golden-gate PASS - with the measured cost fields and the verdict
    line - needs one release run of
    `brokkr test -p mogwai-cli parity12a_ii` from a clean tree AFTER
    this slice's diff lands at HEAD; nothing about the port is
    provisional pending that run, only the demonstration is.
  - **2c-iii, retirement**: cli's measure12a.rs deleted, the gen
    measure dispatch re-routed through the lab engine with the CLI
    surface unchanged, the four named tests re-anchored under their
    spec-runbook names, the 2a parity gates re-run, and python
    cost12a as the end-to-end proof the harness still drives the
    re-routed binary.

## Phase 3 - fit and synthesis (two launches)

- **3a, characterize and synthesis**: characterize (the estimand
  definitions everything imports), build_cadence +
  check_cadence_feasible, build_fingerprint. Gates: reproduce
  `cadence.json` and `fingerprint.json` from their recorded inputs.
- **3b, the fits**: fit_session_profile and the protocol-11 fit mode
  with the CRN solve machinery (in-process walks instead of
  subprocess orchestration). Gates: reproduce `analysis/mnq-fit.json`
  and the preset session arrays. select_windows lands here or is
  re-sentenced; tick_composition_ratios gets its owner ruling (report
  generator -> absorb; independent conformance leg like
  roll_estimator -> stays Python).

## Phase 4 - review, then retirement (strictly in that order)

Owner ruling 2026-08-06: the Python does NOT retire until AFTER the
codex program-level review passes. The reviewer needs the originals
to review against, and every parity gate loses its reference the
moment the scripts move. Phase 4 therefore runs in two halves:

- 4a, reviewable-state prep: docs/cli.md, reference/architecture.md
  and AGENTS.md updated to the crate reality; test_characterize.py's
  coverage dissolved into lab unit tests (the file itself stays); the
  parity-frozen defect list in notes/todo.md swept into a decision
  table. The program then STOPS at the codex gate - the accumulated
  review debt (this program plus the measurement-era relaxations)
  goes to codex in one pass when the quota returns.
- 4b, post-signature retirement: only after codex signs do
  mnq_fit.py and the absorbed scripts move to `research/dead/` and
  the parity-frozen defects get their real fixes.

Adjudication provision (owner, 2026-08-06): when a phase agent hits
a contradiction between the Python, the cached artifacts and the
frozen spec - or any call the orchestrator cannot settle from the
record - a FABLE agent is launched to adjudicate, briefed to read
DATA-PURCHASE-REPORT.md for the full program context. Its ruling is
recorded where the contradiction was found; genuinely owner-shaped
calls (money, spec amendments) still go to the owner.

Parity-frozen defects fall due here too: bugs found during porting
that were deliberately MIRRORED into the Rust so the parity gates
stay honest, each carried in notes/todo.md until the Python retires
and the fix stops being a parity break. First entry: the unguarded
numeric conversions in the TBBO stream contract (parse_stream /
stream.rs - a malformed field crashes instead of refusing; found by
the phase-1 port). Anything phases 2-3 add to that class lands in
todo.md with the same tag and gets swept as part of this phase.

## Permanently out of scope

databento_price.py / databento_download.py / pair_harness.py and its
probe libraries (audited money tools, owner ruling); plot_tape.py
(dev visualization); roll_estimator.py and asof_join.py (deliberate
cross-language conformance legs - absorbing them defeats their
purpose); the three scripts/probe_* lifecycle probes (engine
regression tools). Protocol 12b is a separate track, gated on codex
capacity, orthogonal to this program.
