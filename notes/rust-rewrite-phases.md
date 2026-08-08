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

## Phase 0 - carve mogwai-cli (LANDED)

`main.rs`, `gen.rs`, `measure12a.rs` and the CLI-facing modules move
from `mogwai-server` to a new `crates/mogwai-cli`; the server becomes
a library crate exposing the minimal surface. No behavior change, no
touch to `mogwai-data`. Gate: the existing suite green including the
socket-backed binaries (`brokkr check --gate`, minus its one
pre-existing documented failure) and the named measure12a release
tests; `target/release/mogwai` still exists under that exact name.
Committed on green without deep audit - mechanical, history-recoverable.

## Phase 1 - mogwai-lab: the corpus layer (LANDED)

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

## Phase 2 - the 12a measurement engine (three launches) (LANDED)

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

    UPDATE: the owner ran the golden gate from clean HEAD b5e67a1
    (after fixing a CWD-relative default-path bug in the test's own
    input resolution) and the observed pass completed in 84 s, then
    `crates/mogwai-lab/src/aggregate/monthly.rs:374` panicked with
    `"a window map"`. Root cause, found and fixed: it was NOT a
    types-vs-canon blind spot - explicitly ruling that out because the
    coordinator asked me to name it if it were, and it matters for
    2c-iii. `run_measure12a_observed` (in `measure.rs`) had its own
    hand-rolled monthly-block assembly that called
    `pool_block2`/`aggregate_block3`/`aggregate_block4` directly on the
    WHOLE per-session records instead of on each record's
    `block2`/`block3`/`block4` sub-object. `rec.as_object()` still
    succeeds (a whole session record is an object too), so nothing
    refused until the pooler tried to read `segments` (an array
    sibling field) as an hour's window map. `blocks_from_sessions`
    already exists and does the sub-object extraction correctly - the
    2b and 2c-i gates go through it via `assemble::measure`, which is
    exactly why they never hit this: this was a fresh, wrong
    reimplementation in 2c-ii's new glue code, not a divergence
    between the live-built and cache-parsed `Value` trees, and not
    something the 2a typed-canon parity gate could ever have caught
    (canon compares the value AT an agreed-upon path; this bug indexed
    the wrong path entirely, one level up). Fixed by deleting the
    hand-rolled version and calling `blocks_from_sessions` directly, as
    every other call site does. Regression test added:
    `crates/mogwai-cli/src/measure.rs`'s
    `blocks_from_sessions_does_not_choke_on_a_live_shaped_session_record`
    - a minimal but realistically-shaped session record (carrying the
    array-typed `segments`/`permutations` siblings the bug tripped on)
    fed through `blocks_from_sessions`, plus a documented
    `catch_unwind` proving the OLD call shape does panic, so the trap
    stays visible if anyone reintroduces it. `brokkr check` green (519
    passed, 95 ignored) with the fix in place. Not committed (owner
    re-runs the golden gate after committing).

    GOLDEN GATE PASSED, 2026-08-06, from clean HEAD 0cf2d49. The
    monolithic test exceeds brokkr's hard 280 s per-test ceiling
    (observed 83-86 s plus eight ~25 s walks plus bootstrap), so the
    orchestrator ran the identical driver through the release binary
    against scratch cache copies and compared with the PYTHON
    harness's own _typed_canon under the three live-field
    exclusions: typed-canon identical, verdict no-family-eligible.
    Live cost of the Rust run: observed 83.0 s, generated 202.1 s,
    bootstrap 3.1 s, total 288.1 s, peak RSS 2.93 GiB (inside the
    4 GiB budget; the in-process walks hold full Value trees where
    the Python sampled subprocesses), scratch 93 MB. OBLIGATION for
    2c-iii: restructure the gate test to fit the 280 s ceiling -
    split the observed pass and the walks into separate parity
    tests, or gate the driver over pre-attested cached walks with
    the live-walk attestation kept as its own per-seed tests like
    2a's.
  - **2c-iii, retirement**: cli's measure12a.rs deleted, the gen
    measure dispatch re-routed through the lab engine with the CLI
    surface unchanged, the four named tests re-anchored under their
    spec-runbook names, the 2a parity gates re-run, and python
    cost12a as the end-to-end proof the harness still drives the
    re-routed binary.

    LANDED 2026-08-06. `crates/mogwai-cli/src/measure12a.rs` (2,076
    lines, the CLI-local `Measure12aAcc` twin) is deleted.
    `gen.rs`'s `run_measure12a` now drives
    `mogwai_lab::measure12a::generated::GeneratedAcc` - the same
    construction `parity12a.rs`'s `run_final_walk` and
    `measure.rs`'s in-process attestation already use - and appends
    `cost` (`walk_s`/`self_peak_rss_bytes`, VmHWM) itself, since
    `GeneratedAcc::finish` deliberately returns
    `{seed, per_session, forensic}` without it (the caller alone
    knows the walk's wall time). `self_peak_rss_bytes` is a
    faithful copy of the retired module's own (VmHWM over
    `/proc/self/status`) - distinct in kind from
    `mogwai_lab::sampler::ResourceSampler`'s process-tree sampling,
    which a single offline walk has no need of. The `gen --type
    measure12a` CLI surface (flags, output shape, cost fields) is
    UNCHANGED. `gen.rs`'s own `session_segment_at` and `summarize`
    stay untouched - protocol-11 machinery out of this slice's scope
    per the phase doc, smallest honest diff.

    The four spec-runbook-named tests are re-anchored, exact names
    kept: `measure12a_matches_independent_recompute` already lived in
    `mogwai_lab::measure12a::tests` since phase 2a (nothing to move);
    `measure12a_selection_is_deterministic`,
    `measure12a_consumer_leaves_tape_byte_identical` and
    `arch_coefficients_match_the_shipped_recursion` stay in `gen.rs`
    (they exercise the CLI's own `run_measure12a`/`build_source`
    plumbing, not lab-internal machinery) but now drive
    `GeneratedAcc`/`ARCH_12A`/`GARCH_12A` from
    `mogwai_lab::measure12a::generated` instead of the deleted
    module. All four pass under their exact names.

    THE GATE-RESTRUCTURE OBLIGATION from 2c-ii: added
    `mogwai_cli::measure::WalkSource` (`LiveAttested` /
    `PreAttestedCacheOnly`) and `run_measure_with(cfg, walk_source)`;
    `run_measure` and the CLI both still use `LiveAttested`
    unconditionally, so production behavior is unchanged.
    `crates/mogwai-cli/tests/parity12a_ii.rs` is now two tests:
    `parity12a_ii_fast_matches_the_committed_artifact_over_cached_walks`
    (observed pass LIVE, the eight walks taken straight from the
    Brick G cache with neither a fresh walk nor the attestation
    compare - honestly labeled as proving nothing about walk
    determinism on its own, because the nine `parity12a.rs` gates
    already prove that independently) at ~85 s, well inside the
    ceiling; and
    `parity12a_ii_live_full_run_matches_the_committed_artifact`, the
    real `LiveAttested` path, kept `#[ignore]`d with its doc comment
    naming the release-binary invocation and pointing at this
    section for the measured cost fields, since it still exceeds the
    280 s ceiling by construction. Together the two suites cover
    everything the monolithic test did; nothing is weakened, the
    coverage moved to gates that fit.

    Verification, all from THIS uncommitted tree (git-dirty, so
    `parity12a_ii_fast`'s own live run correctly refuses exactly as
    2c-ii's did - confirmed, not worked around):
    `brokkr check` green (512 passed, 96 ignored); `brokkr check
    --gate` green modulo the one documented pre-existing failure
    (`tape_lateness_under_acceleration`, unrelated - a p99-lateness
    budget in `serving.rs`); all nine 2a parity gates re-run
    individually with `--timeout 279` and PASS
    (`parity12a_observed_per_session_matches_the_committed_artifact`
    82.5 s, `parity12a_generated_seed_1`..`_8` ~26 s each) - proving
    the re-route changed nothing; `parity12a_aggregate` (2b) and
    `parity12a_i` (2c-i) re-run and PASS. `python3 analysis/mnq_fit.py
    cost12a`: `runtime_ratio` 0.8750380890532778 against budget 1.5 -
    matches the old twin's own ~0.875 to the reported digits, the
    end-to-end proof the Python harness drives the re-routed binary
    unchanged.

    Not committed. Phase 2c (assembly, validators, the live run, the
    CLI, retirement) is now COMPLETE pending the owner's final
    commit and clean-tree confirmation of the two restructured
    `parity12a_ii` gates.

## Phase 3 - fit and synthesis (two launches) (LANDED)

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
  roll_estimator -> stays Python). RULED 2026-08-08: both ABSORB, and
  the framing of that second question was a false dichotomy -
  `tick_composition_ratios.py` is neither, it is the sizing policy for
  four shipped constants plus a protocol-landing gate. Deferred to 4b,
  since 3b had already landed when the rulings came.

  LANDED 2026-08-06 (3a only; 3b unstarted). `mogwai_lab::characterize`
  carries the phase-0 estimand layer (`lvl_bin`/`histogram_quantile`/
  `LevelVisits`/`AutoCorr`/`log_bin`/`decimals_used`/`dwell_stats`/
  `characterize`), a byte-level port of `analysis/characterize.py`.
  `mogwai_lab::cadence` carries `build_cadence.py`'s synthesis
  (`band`/`solve_shape`/`build`) plus the internal machinery it needs
  from `probe_binance_trades.py` (`EventStats`, a byte-line Binance
  trades-zip `probe`) - the Python probe files themselves are
  untouched, per the phase-1 triage's KEEP ruling.
  `mogwai_lab::cadence_feasible` carries `check_cadence_feasible.py`'s
  `next_count` and the structural `verdict` (see the scope note
  below). `mogwai_lab::fingerprint` carries `build_fingerprint.py`'s
  `level_verdict`/`level_queue`/`load_reports`/`load_cadence`/
  `build_fingerprint` (the `findings.md` side artifact is not ported -
  a human-readable report, not gated). CLI: `mogwai synth fingerprint`,
  `mogwai synth cadence`, `mogwai cadence-feasible`, all under
  `crates/mogwai-cli/src/synth.rs`; default paths are CWD-relative
  like `measure`/`preflight`'s, and NONE of the three write into
  `analysis/` by default (`--out` defaults to a `target/mogwai-synth/`
  scratch path) so a bare invocation can never clobber the committed
  artifacts.

  GATES, run against what was actually on disk:

  1. **Fingerprint synthesis (binding).** Ran against the real local
     inputs: the eight gitignored `analysis/char_*.json` reports and
     the committed `analysis/cadence.json`. Value-identical
     (typed-canon) on every leaf except one:
     `empirical_ranges.modal_tick.max` reads `0.25` in the committed
     `fingerprint.json` but every currently-committed `char_*.json`'s
     `returns.modal_tick` tops out at `0.1` (XBTUSD). This is
     CONFIRMED input drift, not a port defect: running
     `analysis/build_fingerprint.py` itself, unmodified, against
     today's `char_*.json` files reproduces `0.1`, not `0.25` (checked
     directly, then the regenerated file was reverted with `git
     checkout` immediately - the committed `fingerprint.json` was
     never touched). The eight `char_*.json` files were regenerated
     locally at some point after the commit that produced
     `fingerprint.json`, moving the anchor's modal tick without
     anyone re-running `build_fingerprint.py`. Byte-identical output
     was not attempted (the Python's `json.dump(fingerprint, f,
     indent=2)` carries no `sort_keys`, so byte fidelity would need a
     hand-written insertion-ordered pretty-printer instead of
     `serde_json::Value`'s alphabetical `Map`; the brief's own floor
     is "value-identical typed-canon minimum", which this clears).
     Gate: `crates/mogwai-lab/tests/parity3a.rs`'s
     `parity3a_fingerprint_matches_the_committed_artifact`, PASS
     (with the one documented, verified leaf excluded from the
     assertion by name, not swept under a blanket tolerance).
  2. **Cadence synthesis.** The raw archives ARE present:
     `research/market-data/{BTCUSDT,ETHUSDT,SOLUSDT}-trades-2026-06.zip`
     (9.8/4.5/3.0 GB). Ran the live path, not the degraded one.
     `parity3a_cadence_matches_the_committed_artifact` streams all
     three archives (~230M rows total) and reproduces every leaf of
     the committed `analysis/cadence.json` typed-canon-identically
     except `provenance.generated_utc`, a live wall-clock stamp
     excluded the same way the 12a gates exclude `cost` - PASS,
     ~69 s release.
  3. **check_cadence_feasible.** PORTED IN PART, gated on what was
     ported. `next_count` and the structural `verdict()` (the
     PROCEED/CLOSE/STOP AND ASK threshold read directly off
     `cadence.json`'s `children_mean`/`children_single_frac` anchors)
     are exact ports; `parity3a_cadence_feasible_verdict_matches_the_committed_cadence`
     reproduces `PROCEED` over the committed `cadence.json`, matching
     the Python's own printed verdict. NOT ported: the default
     (no-flag) CLI path's 3,000,000-event Markov density
     re-simulation (`simulate_markov`), which draws from
     `random.Random(42)` through `weibullvariate` - bit-exact
     reproduction would need a from-scratch port of CPython's
     Mersenne Twister and `random.weibullvariate`/`math.gamma`, out
     of this slice's budget. This is a real scope gap, not a
     rounding-convention one: `notes/todo.md` should carry it if a
     later phase wants the full density recheck ported. The
     structural verdict is what the phase-3a brief calls binding for
     this script ("the L0 structural-proceed verdict"); the
     stochastic recheck is a secondary diagnostic in the Python
     itself (`SystemExit` only on ITS OWN failure, never gating the
     structural verdict).
  4. **characterize.** No full-corpus gate (Kraken corpus outside the
     repo, per brief). `crates/mogwai-lab/src/characterize/tests.rs`
     reproduces every assertion in `analysis/test_characterize.py`'s
     `BinningTests`/`QuantileTests`/`VisitClosureTests`/
     `EraWindowTests`/`ReportTests` (17 tests), plus
     `crates/mogwai-lab/src/fingerprint.rs`'s `tests` module covers
     `LevelQueueTests` (7 tests) and `crates/mogwai-lab/src/cadence.rs`'s
     covers the `CadenceTests` shape/grouping assertions (`solve_shape`,
     `EventStats` grouping) - the raw-archive `probe` test itself
     is covered live by gate 2 instead of a synthetic zip fixture.
     `check_cadence_feasible.py`'s inverse-CDF sampler assertion
     (`next_count`) is covered in `cadence_feasible.rs`. All pass.

  Conventions pinned, beyond the phase-2 ones this slice inherited
  unchanged (`kernel::py_sum`, insertion-ordered maps, `float_roundtrip`,
  `py_float_repr`): `build_fingerprint.py`'s `rng()` is Python's
  dynamically-typed `min()`/`max()` - an all-integer input list keeps
  `min`/`max` as JSON integers, only `statistics.median`'s true division
  always yields a float. `mogwai_lab::fingerprint`'s `rng_typed` (over
  `serde_json::Value`, not `f64`) is the port of that; the plain-`f64`
  `rng` wrapper exists for the (majority) case where the Python's own
  inputs were already floats. `hour_vol`/`avg_curves`/`EventStats::report`'s
  `sum(...)`-over-floats spots now route through `kernel::py_sum`, per the
  phase-2b pin; `hour_shares`/`dow_weights` sum integer counts, where a
  naive fold is exact and was left alone.

  Two crate additions: `zip = { version = "2", default-features = false,
  features = ["deflate"] }` in `mogwai-lab` (streaming-read the Binance
  archives; no compression-side feature needed since this crate only
  reads). `mogwai-lab/tests/parity3a.rs` joins `parity12a*.rs` under the
  `parity3a_*` naming convention, `#[ignore]`d for the same reason -
  needs local corpus/archive state, not sandbox-safe by default.

  Lateral finding: the committed `fingerprint.json`/`char_*.json`
  drift above means ANY future regeneration of `fingerprint.json` from
  today's `char_*.json` will legitimately change
  `empirical_ranges.modal_tick.max` from `0.25` to `0.1` - worth a
  conscious re-commit decision (owner call, not this slice's to make)
  rather than being discovered as a surprise diff later. Not filed to
  `notes/todo.md` as a parity-frozen defect since it is a stale-input
  fact, not a Python/Rust behavioral divergence.

  LANDED 2026-08-07 (3b, the last porting phase). `mogwai_lab::fit`
  carries the protocol-11 fit in seven modules: `observe` (the one
  streaming corpus pass with its three chains, the protocol-11
  session-refit cells, `Quantiles`/`Acf`/`dist_stats`/`hist_quantile`
  and `minute_range_envelope`), `curves` (the exposure tables,
  `hourly_robust_curve`, `normalize_hour_curve`, `materialize_curve`,
  `curve_triple`/`curve_pair`, `fit_vol_hour`, `fit_intensity_hour`,
  `observed_walltime_curves`), `solve` (the CRN `trisect`/`coarse_grid`/
  `solve_scalar` with the invariants as unit tests), `walk` (the cached
  `gen --type summary` evaluation), `driver` (`run_fit`: the vol solve,
  the family probes, `judge`, the verdicts and tolerance classes, the
  `session_refit` record builder), `diagnostics` (`build_diagnostics`)
  and `mtrand`. `mogwai_lab::session_profile` is
  `analysis/fit_session_profile.py`. CLI: `mogwai fit` with
  `--corpus/--ledger/--preflight/--cache-dir/--cache-commit/--out` under
  `crates/mogwai-cli/src/fit.rs`, carrying `mode_fit`'s clean-tree
  binding; `--out` defaults under `target/` so a bare invocation can
  never clobber the committed `analysis/mnq-fit.json`.

  THE SUMMARIZE MOVE. `gen.rs`'s `summarize` and its protocol-11 session
  cells, top-minute records and `session_segment_at` are now
  `mogwai_lab::summary`; `gen.rs` keeps `write_summary` and the CLI
  surface and calls straight into it. This is the one place `mogwai-lab`
  depends on `mogwai-server`: a walk's instrument is an
  `InstrumentProfile`, resolved through the server's own `Config::load`
  exactly as `mnq_fit.py`'s `--config` scratch walks were. No cycle -
  `mogwai-server` does not depend on the lab. `gen --type summary` is
  byte-identical across the move, pinned by a before/after run of the
  release binary (MNQ, seed 7, start 1782856800000000000, length 2d,
  warmup 3d) captured from clean HEAD and re-captured after: `cmp`
  clean, twice (once after the port, once after the clippy pass).

  GATES, all run:

  1. **The binding one.** `crates/mogwai-cli/tests/parity3b.rs`'s
     `parity3b_fit_matches_the_committed_artifact_over_the_python_walk_cache`,
     `#[ignore]`d, 82 s release. `run_fit` over the delivered July corpus,
     replaying the Python-era cache at `analysis/out/mnq-fit-scratch`
     under the artifact's own bound commit, reproduces
     `analysis/mnq-fit.json` typed-canon-identically. WALK CACHE: 132
     lookups, 132 Python-layout hits, 0 misses - every walk the solve
     needed (64 SEARCH coarse-grid evaluations over two seeds, 8 arrival
     probe walks, 3 x 8 shared FINAL probe walks that dedupe onto the 8
     combined walks, plus the trisection tail) resolved from cache, so
     nothing was re-walked. The Python key derivation is ported in
     `fit::walk::python_cache_key`.

     EXCLUSIONS, three, each verified rather than assumed:
     `binding.harness_tree_commit` (the committed artifact binds the
     commit the Python ran from; any Rust run binds its own HEAD);
     `binding.subcontract_hash` and `binding.preflight_artifact_hash`,
     both CONFIRMED stale-input drift of the same class as phase 3a's
     `fingerprint.json` finding. The artifact records subcontract
     `35e5b033...`, which is the sub-contract as it stood at the
     protocol-11 fit; the protocol-12a constants joined
     `SUBCONTRACT_KEYS` afterwards, so `mnq_fit.py`'s OWN
     `subcontract_hash()` returns `1ca79d9c...` today - byte-identical
     to what this port computes AND to what the committed
     `analysis/out/mnq-fit-preflight.json` already records. Likewise the
     artifact hashes the preflight FILE as `adf6b8e7...` while the file
     on disk today hashes to `96013588...`, again what both the Python
     and the port compute now. Verified by running the Python directly.
     `binding.file_hashes` - the field that actually binds the corpus -
     IS compared and matches. Nothing else is excluded: `solves` with
     its `evaluations`/`termination`/`final_score`, every `session_refit`
     record, `landing_rule`, `verdicts`, `diagnostics` and the whole
     `observed` block are compared byte-of-meaning. `mnq-fit.json`
     carries no `cost` object, so the timing exclusions 12a needed have
     no counterpart here.

  2. **fit_session_profile.** The NQ archive IS on disk
     (`research/market-data/nq-1m_bk.zip`, 72 MB), so the live path ran.
     `crates/mogwai-lab/tests/parity3b_session_profile.rs`, two ignored
     tests, 2 s release. The preflight report is field-for-field
     identical to `python3 analysis/fit_session_profile.py preflight`
     (5,891,412 rows, 4,539 sessions observed, 4,376 eligible, 163 early
     closes, 2,025,407 CST rows, 289,404 missing minutes, zero
     zero-volume rows). The fit reproduces all four scopes against a
     direct run of the Python: alpha 290.0524/186.8553/239.7084/424.8419,
     sweeps 21/25/20/21, material shares 0.0000/0.0336/0.0087/0.0000
     over 0/4/1/0 cells, peak-to-trough 36.45/117.55/37.99/27.51, era
     stability 0.2283 over 26 cells -> ERA-DEPENDENT, Outcome 2.

     HONEST SCOPE FINDING on "the three fitted entries". The preset's
     provenance table names three `[instrument.session]` entries, but
     only ONE still descends from this script: `session.dow_weight`,
     which the gate reproduces exactly as the shipped
     `[1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000]` (the fit
     returns `1.5178908567396936, 0.9080179424286638, 0.9865270760379059,
     1.0156734577180422, 1.0534691194906738, 1.0224559786097835, 1.0`,
     which is those values before the preset's four-decimal rounding).
     `session.intensity_hour` and `session.vol_hour` were RE-PROVENANCED
     to the July MNQ TBBO corpus by the protocol-11 refit - their NQ-bar
     ancestors were overwritten - so no currently-committed value of
     either can be reproduced from this archive by anyone, Python or
     Rust. Gate 1 reproduces both, from the corpus that actually fitted
     them.

  3. **The solve invariants**, ported one for one from the Python
     selftest's solve-mechanics section into
     `mogwai_lab::fit::solve`'s tests: trisection convergence on a plain
     objective, the flat-objective tie-break to the smaller candidate,
     the boundary winner refining its single inside neighbour interval,
     seeded endpoints never re-evaluated AND the fresh interior pair
     (not the seeds) deciding the bracket, the objective threshold
     stopping after the coarse grid at exactly 11 evaluations,
     log-domain relative termination reading the log span directly, and
     end-to-end CRN determinism (two runs name the identical candidate
     sequence). Plus one the Python has no counterpart for: the coarse
     grid's endpoints in both domains, because prewarm and solve must
     name bit-identical candidates or every cache lookup misses. The
     tolerance-class battery (inclusive boundaries in relative,
     absolute, ceiling, band and exact) lives in `fit::tests`.

  4. **The move.** `brokkr check` green - 580 passed, 101 ignored - with
     every pre-existing summary test (`summary_matches_an_independent_
     tick_walk`, `minute_ranges_match_an_independent_bar_pass`,
     `halt_boundaries_never_borrow_the_pre_halt_mid`,
     `session_segment_at_agrees_with_mogwai_lab`, the top-minute and
     protocol-9 oracle tests) still passing against the moved module,
     plus the `cmp`-clean byte comparison above.

  TWO CONVENTIONS PINNED, both found by the gate rather than by reading:

  - **CPython's `sum()` over floats is compensated, its `+=` loops are
    not** - the phase-2b pin, and 3b found three more sites it applies
    to: `fit_intensity_hour`'s per-hour `weighted` day-factor sum (a
    naive fold moved the last ulp of EVERY normalized intensity value,
    and through the materialized array the whole candidate curve),
    `pooled`'s `mid_return_sumsq` fold across seeds, and
    `generated_evidence`'s pooled wall-time `sumsq`. `normalize_hour_
    curve`'s own `num`/`den` accumulation is a Python `+=` loop and
    stays a naive fold.
  - **CPython's `int / int` is correctly rounded from EXACT operands.**
    New this phase: `kernel::py_int_div`. The fit's pooled
    `mean_event_duration_s` divides a nanosecond gap sum of order 2e16 -
    past 2^53 - by an eligible-gap count, and `a as f64 / b as f64`
    rounds the numerator to binary64 BEFORE dividing, landing one ulp
    off the committed artifact. Python never pre-rounds. Used at the
    three gap-sum division sites; every other integer division in the
    fit has operands well inside 2^53 and is left alone.

  Also this phase: `kernel::py_int_div` and `fit::mtrand`, a port of
  CPython's Mersenne Twister (`init_by_array` seeding, `getrandbits`'s
  word layout, `_randbelow`'s rejection sampler and `choice`), needed
  because `minute_range_envelope` draws 22,000 session labels under
  `random.Random(1)` and its output IS the bound the three minute-range
  gates judge against. Pinned against CPython's own stream, not against
  the implementation. `storage::CacheStore` gained `Clone`;
  `mogwai-lab` gained `#![recursion_limit = "512"]` because the
  artifact's record literals are wide `json!` blocks. `brokkr.toml`'s
  complete profile now skips `parity3a_` and `parity3b_` alongside
  `parity12a_`, for the same reason: local data no clone carries.

  OWNER RULINGS, assessed here in phase 3b and RULED 2026-08-08. Both
  assessments below are superseded and both were wrong in the same way:
  they priced each script against a CLOSED corpus. The corpus is open -
  MNQ plus the three crypto pairs is the current state, not the end
  state - so "no purchase decision is on record as coming" and "still an
  owner call" both dissolve. Every new symbol re-runs the intake
  sequence. The authorized work is the 4b scope block in this file; the
  rulings and their grounds are indexed in
  `notes/rust-rewrite-review-dossier.md` section 10; the corrections to
  the specific reasoning are noted inline below.

  - **select_windows.py** (370 lines, `features`/`select`/`drift`/`plan`).
    Absorbing it is a genuinely small port - four phases over the four
    committed CME bar archives, a cached `cme_daily_features.json`
    intermediate, z-scores over eligible months and a
    farthest-point-first selection - and this phase's
    `session_profile.rs` already lands most of its infrastructure (the
    same zip-streaming bar reader, the same 17:00 session-date
    convention, the same roll trim). Its own docstring calls the
    `DATABENTO_START` constant load-bearing beyond eligibility, which is
    exactly the kind of silent re-centring a port must not perturb, so
    it needs a gate: `analysis/targets-frozen.json` is committed and the
    `select` phase is deterministic given the feature cache, so
    reproducing that file from the cache would be a real one. Against
    that: `cme_daily_features.json` is NOT committed (the triage's own
    lateral finding about regenerable caches), so a from-archives gate
    is the only fully honest one, and no purchase decision is on record
    as coming. The honest verdict looks like either "absorb with a
    targets-frozen gate, budget one small slice" or "re-sentence to
    KEEP until a purchase question actually returns"; both are
    defensible and the choice is about whether more sampling-frame
    buying is expected, which is owner knowledge, not code knowledge.

    RULED: ABSORB WHOLE, all four phases. The owner knowledge the
    assessment asked for is that more buying IS expected, for every
    instrument added from here. Two factual corrections to the paragraph
    above. `targets-frozen.json` cannot be the gate - it is the BTCUSDT
    microstructure target set, one of two hash-pinned frozen INPUTS to
    the sampling-frame experiment, and the `select` phase neither reads
    nor produces it; the from-archives gate is therefore not merely the
    most honest one, it is the only one, and it must be blessed before
    the port can be matched. And the assessment never registers that the
    stratification method was preregistered-TESTED and REJECTED on
    BTCUSDT (`analysis/association-result.json`), which is nonetheless
    not grounds to drop `select`/`plan`: one observation on one crypto
    pair, and running it on the next instrument is how it becomes a law
    or a local fact.
  - **tick_composition_ratios.py** (680 lines). Reading it settles the
    triage's ambiguity in one direction: it contains NO independent
    estimator. It reads two committed
    `analysis/tick-composition-protocol-N.json` fixtures that the RUST
    side produced (`mogwai tick-composition`), checks their `pairing_id`
    relationship, and applies a budget-policy arithmetic over frozen
    per-mode baseline tables to print resize proposals. That is a report
    generator over Rust output, not a second implementation of a shared
    contract - the opposite of `roll_estimator.py`, whose whole point is
    that two languages independently compute the same estimator over one
    shared fixture. So the ABSORB reading is the one the code supports,
    and the natural shape is a `--report` mode on the existing
    `tick-composition` subcommand, which already owns the fixtures. The
    one thing absorbing costs is the frozen baseline tables: they are
    historical records ("frozen once its mode's resize has landed"), so
    they must move as data, not be re-derived. Still an owner call
    because the triage flagged it as one.

    RULED: ABSORB, but as its OWN subcommand, and the "no independent
    estimator" premise is refuted. The budget-policy arithmetic IS the
    estimator: worst p99.9 ratio, two-times headroom, power-of-two or
    next-million rounding, then the larger of that and the required
    reach - and it decides four SHIPPED constants (`CHECKPOINT_K`, the
    sweep drain budget, the warmup materialization ceiling,
    `fanout_depth`). Calling that a report generator confuses the fact
    that its INPUTS are Rust-produced with the claim that its OUTPUT is
    descriptive. It is also a protocol-landing gate: three acceptance
    checks refuse before any ratio is computed, plus a whole-tree
    finite-and-positive leaf validator and a 27-check selftest.
    `reference/performance.md` cites it at five sites, and it is where
    the rejected protocol-11 fanout proposal came from. So NOT a
    `--report` mode on `tick-composition`: producer and gate stay
    separate, or one command measures a fixture and blesses it. The
    baselines-as-data constraint is correct and is kept; add that
    `CALENDAR_FREE`/`CALENDAR_BEARING` become preset-derived, since
    hardcoded preset tuples do not survive an open instrument set.

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

  THE REVIEW RAN 2026-08-08 AND REFUSED THE SIGNATURE. Codex session
  019fe03a, `review bare --profile deep`, 26m29s, pointed at the dossier.
  Verdict: it would not sign the Python retirement in the current tree.
  It confirmed `brokkr check` green at 582 tests, re-ran the focused 12a,
  fingerprint and cadence parity gates, independently reproduced the
  session preflight exactly, and audited every named gate exclusion -
  finding no numerical divergence disguised as one. The refusal is about
  what the gates do NOT cover.

  Three of its findings were then argued to consensus in a spar session
  (019fe054). Recorded because two of them moved:

  - **The hardcoding half of its intake blocker is WITHDRAWN.** It had
    blocked partly because `cadence.rs`, `fingerprint.rs` and
    `session_profile.rs` fix pairs, archive month, anchors and the MNQ
    preset in source. But the Python fixes exactly the same things in
    exactly the same places - `build_cadence.py:19` and `:62`,
    `build_fingerprint.py:30`, `fit_session_profile.py:39` - so the port
    mirrored the debt rather than adding it, and retiring the Python
    removes no parameterization that exists today. Agreed disposition:
    forward work toward the open-instrument goal, NOT a retirement
    blocker. What survives from that blocker is the CLI half, below.
  - **The parity contract was made explicit** and now lives durably in
    `reference/architecture.md`. It is what separates the acceptable
    accidental agreements from the blocking ones, and it settles the
    fail-open decoding findings in one stroke.
  - **The Markov gap is a port, not an owner ruling.** Phase 3a recorded
    it as out of budget because it needed a from-scratch CPython
    Mersenne Twister; phase 3b then BUILT that (`fit::mtrand`), so what
    remains is `random()`, `weibullvariate` and a `gamma` the default
    path calls at shape 1. I argued verdict equivalence might suffice
    for a stochastic band check; that was refuted with evidence - codex
    ran the default path and the simulated median lands on 3, exactly
    the allowed lower boundary of measured median minus one, so a
    different stream giving 2 flips the verdict. Bit-exact it is.

  4b SCOPE, as it stands 2026-08-08. Order matters: items 1 to 4 BLOCK
  the retirement and must land before any absorbed script moves.

  1. **CLOSED 2026-08-08.** `mogwai characterize` lands and covers both
     Python entry points - the per-corpus case and the `run_corpus.py`
     multi-pair fan-out - so `char_*.json` production survives the
     retirement in Rust. All eight reports were regenerated through it,
     which is what exposed item 4's stale leaf. The original text
     follows.

     **add a `mogwai characterize` subcommand.** There is
     none. `mogwai_lab::characterize` is a gated library with no CLI
     driver, and its Python driver `run_corpus.py` was deleted at
     `9170f45`. The chain only works today because `characterize.py`
     runs standalone (`python3 analysis/characterize.py <pair>` writes
     `analysis/char_<PAIR>.json`). Retire it as planned and NOTHING in
     either language produces `char_*.json` - which
     `mogwai synth fingerprint` reads as its input, and which is the
     first step of onboarding any new instrument. Retiring
     `characterize.py` before this lands severs the intake chain.
     Note the shape: the Python has both a per-corpus entry point and
     (formerly) a multi-pair driver, so the subcommand needs to cover
     the `run_corpus.py` fan-out too, not just the single-file case.

  1b. **CLOSED 2026-08-08.** `mogwai session-profile preflight` and
     `... fit` land, and the preflight reproduces the Python
     field-for-field: 5,891,412 rows, 4,539 sessions observed, 4,376
     eligible, 163 early closes, 2,025,407 CST rows remapped, 289,404
     missing minutes, zero zero-volume rows. Both entry points also lost
     their hardcoded MNQ preset - it is a `--preset` argument now, since
     the calendar is an INPUT to the estimator rather than a consumer of
     it. That was the cheapest available dent in the parameterization
     debt: one argument, taken while the file was open anyway. The
     original text follows.

     **the session-profile fit has no CLI surface either.**
     Same class as the above, raised by the review. `mogwai-lab` carries
     `session_profile.rs`, but the command enum exposes only the
     protocol-11 `fit`; `analysis/fit_session_profile.py preflight` and
     `fit` have no shipped equivalent. Retiring that script removes a
     runnable fitting capability from the tool.

  1c. **CLOSED 2026-08-08.** The simulation is ported and wired into the
     default path, and `mogwai cadence-feasible` now reproduces
     `python3 analysis/check_cadence_feasible.py` field for field at the
     full 3,000,000 events - mean 51.019534657973, median 3, p95 357,
     zero_frac 0.129516386850407, and both gap ACFs to full precision.
     `PyRandom` gained `random()` and `weibullvariate`, both pinned
     against the CPython stream by prefix tests, and a 5,000-event
     fixture pins the simulation itself so a draw-consumption or
     state-update difference fails loudly. Strict schema refusal landed
     with it. Note the margin the review predicted: the simulated median
     is 3 against a measured 4, exactly the band's lower edge, so
     verdict-only equivalence really would have been one draw from
     flipping. STILL OPEN, and tracked as capability rather than a
     retirement blocker: `--fit` and `--fit-markov`, which need
     `math.gamma` at arbitrary shape. The original text follows.

     **`cadence-feasible` does not preserve the Python
     command, and can pass open.** Two defects, both found by the
     review. First, the Python default path runs the 3,000,000-event
     Markov density re-simulation and exits nonzero when the realized
     density misses the feasibility bands
     (`check_cadence_feasible.py:275`) - it is a GATE, not the secondary
     diagnostic the phase-3a record called it, so `mogwai
     cadence-feasible` can exit 0 where the script exits nonzero. The
     `--fit` and `--fit-markov` modes are absent too. Port it, to this
     standard, agreed in the spar: `PyRandom::random()` bit-exact
     against CPython with pinned prefix tests; `weibullvariate(1.0, 1.0)`
     consuming the same draws and producing the same values, also
     prefix-tested; a small-event simulation fixture that would expose
     draw-consumption, bucketing or state-update differences; and the
     full default run matching Python density output AND exit status on
     the committed `cadence.json`. Pin the discrete outputs exactly
     (median, p95, bucket and event counts); if a continuous diagnostic
     cannot be made exactly equal across platform `log`/`exp`, state the
     ULP rule explicitly rather than loosening silently.
     Second, `cadence_feasible.rs:71` substitutes `0.0` for missing or
     nonnumeric fields, so a document carrying `children_mean.anchor`
     but no `children_single_frac` returns PROCEED where Python raises.
     Strict schema refusal.

  1d. **CLOSED 2026-08-08.** `py_fsum` landed beside `py_sum` in
     `kernel` and the two summation paths stay distinct; `sqrt` was kept
     as the approved semantic change with a discriminating test pinning
     it against `powf(0.5)`; the loader now fails closed on a missing
     `pair` or `n_trades`. The first `py_fsum` folded partials
     left-to-right and was wrong - CPython walks them largest-down with
     an explicit half-even correction, and only the embedded reference
     values caught it. Pinning against the committed artifact would have
     passed while being wrong, which is the blind spot this item named.
     The original text follows.

     **fail-open decoding and silent float divergence in the
     fingerprint loader.** Per the parity contract in
     `reference/architecture.md`. The silent-divergence half: Python
     computes the hour RMS with `** 0.5` and normalizes with
     `statistics.fmean` (which is `math.fsum`-backed on 3.14.6), while
     the port uses `sqrt()` and `py_sum` over the length - both can
     differ by one ULP, and codex constructed positive three-value
     inputs that do. The committed `char_*.json` happen not to expose
     it. Ordering is conditional the same way: Python keeps glob
     filename insertion order while `load_reports` returns a pair-sorted
     `BTreeMap`, equal today only because the filenames are well formed.
     The fail-open half: a missing `pair` becomes `""` and a missing
     `n_trades` becomes zero, where Python fails closed. Fix to Python
     semantics and pin with fixtures chosen to DISTINGUISH the
     implementations - re-passing the committed artifact proves nothing,
     since it is what the blind spot is made of.

     RESOLVED in the spar, and the two halves go OPPOSITE ways. Measured
     first: `x ** 0.5` differs from `sqrt(x)` in 1,618 of two million
     draws over the realistic domain, about one in 1,236, and `hour_vol`
     takes 192 square roots across the eight pairs - so a fresh
     characterization run has roughly a 14 percent chance of exposing it.
     Not a corner case.
     - **Square root: APPROVED SEMANTIC CHANGE, keep `sqrt`.** CPython
       delegates the finite case to platform libm `pow`, so matching it
       bug-for-bug would make a committed artifact that is compiled into
       the generator by `include_str` a function of the libm belonging to
       whoever regenerated it. `sqrt` is correctly rounded under IEEE 754
       and identical on every conforming platform. Add a discriminating
       test at a value where `powf(0.5)` and `sqrt` differ, so nobody
       later "restores parity", and narrow the cross-language exception
       to the affected `session_profile.vol_hour` values while requiring
       every other field to stay identical.
     - **fmean: FAITHFUL PORT.** `statistics.fmean` is `fsum(data) / n`,
       Shewchuk exact summation, and matching it introduces no platform
       dependence. Add `py_fsum` beside `py_sum` in `kernel`, pin
       cancellation, halfway rounding, order, overflow, infinity and NaN
       against the helper's supported contract. Line 168 of
       `build_fingerprint.py` uses builtin `sum` and line 170 uses
       `fmean` INSIDE THE SAME FUNCTION, so the two paths must stay
       distinct - routing both through one helper is the bug.
     - **The version bump is owed by the COMMIT, not by the code
       change.** LANDED 2026-08-08 with no bump, and that was argued and
       agreed: the premise that regenerating would move `vol_hour` turned
       out to be false. The parity gate reproduces the committed artifact
       byte for byte after both changes, so today's corpus discriminates
       at none of the 192 square roots or the eight normalizations.
       Structurally too, the synthesis is off the tape path - `mogwai-lab`
       depends on `mogwai-data` and never the reverse, and the generator
       reads `analysis/fingerprint.json` through `include_str!` rather
       than calling the synthesis - so editing `fingerprint.rs` cannot
       move a tape byte until a regenerated artifact actually differs.
       The runtime function `generator code plus embedded fingerprint
       plus seed plus config` is unchanged; only the offline `corpus plus
       method` function moved.
       WHAT THIS MEANT FOR ITEM 4, and it was then ruled the other way:
       this text argued the `modal_tick.max` re-commit owes the bump
       because the rule names the committed fingerprint without
       qualifying which leaf moved. `7852e2f` landed WITHOUT one, under a
       narrow exemption now recorded durably in `AGENTS.md` - one leaf,
       an exhaustive reader audit finding `empirical_diagnostics` as its
       sole consumer, and version 12 already reserved for 12b, so a bump
       would have spent a tape identity on a provably inert change and
       pushed the real one to 13. `AGENTS.md` is the source of truth on
       that ruling and states it does not generalize to another leaf.

  1e. **CLOSED 2026-08-08. `brokkr check --gate` is GREEN**: 674 passed,
     0 orphaned. The orphaned test is un-skipped and runs. The lateness
     failure is QUARANTINED rather than fixed or relaxed, on the argument
     that the debug lane could never validly judge a release wall-clock
     contract, so its red was not evidence about the property - running
     it there was itself a changed measuring instrument. The 50 ms
     assertion is untouched, the test stays directly runnable, and the
     exclusion claims nothing about whether this host meets the budget.
     The environment sensitivity is worse than previously recorded and
     stays open in `notes/todo.md`: a release rerun failed at 311 ms with
     a load average of 1.46 across 32 CPUs, so a load-average precheck is
     not a sufficient admission test. The original text follows.

     **`brokkr check --gate` is red**, for two independent
     reasons. `tape_lateness_under_acceleration` measured 90.2 ms
     against its 50 ms ceiling and 163.2 ms on a focused debug rerun;
     this predates the rewrite and is the recorded profile-mismatch item
     in `notes/todo.md`, so it needs dispositioning with evidence rather
     than fixing here. Separately, and new: the coverage audit reports
     `parity3a_cadence_feasible_verdict_matches_the_committed_cadence`
     as ORPHANED, because `brokkr.toml`'s broad `parity3a_` skip catches
     that cheap non-ignored test along with the corpus-dependent ones.
     A gate nobody runs is not a gate.
  1f. **THE SECOND REVIEW PASS, 2026-08-08. REFUSED AGAIN**, four
     findings, all CLOSED the same day. Session 019fe13a, 17m25s. Both
     judgement calls were accepted: the lateness quarantine as a correct
     debug-versus-release measurement boundary, with the release property
     left explicitly uncertified - its own release rerun failed at
     360.0 ms p99 under load average 1.36, which independently confirms
     the sensitivity is not load-driven and kills the precheck idea for
     good - and the `TAPE_PROTOCOL_VERSION` exemption, verified the strong
     way by running a fresh eight-pair characterization and synthesis and
     getting a value identical to the committed fingerprint.

     THE SHAPE ALL FOUR SHARED, which is the part worth keeping: each of
     the five earlier closures held on the committed corpus and failed one
     layer below where its gate looked. Same ground as the first refusal.
     The closures had been verified against the artifacts instead of
     against the contract.

     - **`mogwai characterize` broke the path-shaped input it
       advertises.** The output name was formatted from the raw CLI
       argument rather than the report's own `pair`, so
       `characterize path/to/KEUR.csv` wrote
       `char_path/to/KEUR.csv.json` - a nested directory `load_reports`
       cannot see, since it scans only the output directory. The command
       exited zero and the intake chain broke downstream. Item 1 read as
       closed because the subcommand existed; the write path had no test
       at all. Fixed by deriving the name from `report["pair"]`, matching
       `characterize.py:247` and `:487`, with three CLI tests driving the
       real binary - path-shaped, bare-symbol, and one asserting
       `load_reports` can actually see the result, since that is the
       consumer whose failure the defect produced. An attempt to argue
       this was an ADDED-and-broken capability rather than a lost one
       failed on the Python: it accepts a path and names the report after
       the file.
     - **Both modes tie-broke the wrong way.** `max_by_key` returns the
       LAST maximal element where CPython's `max` returns the first, so
       `modal_tick` disagreed on any tie - and `characterize/mod.rs`
       carried a comment at the container declaration explaining that
       insertion order was load-bearing for exactly this, which the call
       site then discarded. Half a fix shipped. `price_decimals_mode` was
       worse: a `HashMap`, so nondeterministic between runs of the same
       input rather than merely divergent. Both fold first-wins now,
       pinned by order-reversed fixtures. Valid input; the eight
       committed reports contain no tie, so no gate could have seen it.
     - **The fingerprint fail-open fix stopped at the loader.** `8fb8d69`
       made `pair` and `n_trades` strict and left `level_verdict` and
       `level_queue` substituting `0.0` for `single_print_frac`,
       `vol_dispersion` and `size_dispersion`, where
       `build_fingerprint.py:61-64` indexes directly and raises. The
       direction is what makes it a defect: two of the four conditions are
       lower bounds, so a missing field cleared its own condition and the
       substitution manufactured `proceed: true` on input the Python
       refuses to score. Now refuses per field, with each field dropped
       individually in the test - a fixture missing all three would start
       passing the moment any one of them refused - plus a case pinning
       that dropping the field which made a condition FAIL does not
       rescue it into a pass.
     - **`gap_cv2` deviated one ULP from `statistics.pvariance`, silently,
       behind a `1e-12` band.** Ruled an APPROVED DEVIATION after
       argument, on reachability: `density_passes` decides the nonzero
       exit and does not read the field, whose only Python consumer is
       the unported `--fit-markov` ranking score. Both sides moved to get
       there. The reviewer accepted reachability as what makes a deviation
       approvable, and held that an UNSTATED deviation blocks regardless -
       so the same difference blocks while silent and does not once stated
       and bounded. It also corrected the premise behind the cheap fix
       offered: CPython does not round the deviation before squaring, it
       evaluates `(n * sum(x^2) - sum(x)^2) / n^2` as an exact rational
       and rounds once, so the proposed two-double expansion would not
       have been parity. Closed by extracting `gap_pvariance` as one
       documented site, replacing the tolerance with BIT-EXACT pins on
       `mean`, `zero_frac`, `gap_mean` and both ACFs, and giving
       `gap_cv2` a named one-ULP bound with a three-gap fixture built to
       discriminate. The ACFs stay exact deliberately: adjacency to a
       deviating field is not grounds to loosen them.

     ALSO CORRECTED, and it was the dossier's own error rather than a code
     defect: the packet claimed the cadence-feasible parity gate covered
     the full 3,000,000-event density run. It calls `verdict()` and
     asserts `PROCEED`, nothing more. The 3M agreement is real but was
     established by hand, so the density report had no gate - which is how
     an unstated ULP divergence survived in the one test that touched it.
     `brokkr check --gate` after all four: 682 passed, 0 orphaned.

     WHAT THIS BUYS: `--fit-markov`'s port now owes either exact rational
     accumulation or a fresh ruling, recorded here because it may not
     inherit the approval above - the score divides `gap_cv2` by a cadence
     anchor and sorts a grid by the sum, so one ULP can reorder adjacent
     grid points and change which constants the mode proposes.

  1g. **THE THIRD REVIEW PASS, 2026-08-08. REFUSED**, on two findings,
     both now closed. Session 019fe13a, 6m05s. It re-ran every focused
     test behind the second pass's closures and found no further defect in
     the first three, and ruled the TBBO short-row panic explicitly NOT
     grounds for refusal - it mirrors the Python today and item 6 is
     correctly ordered before item 7 - while holding that a signature
     stays conditional on fixing both the width check and the panicking
     numeric conversion before the scripts move.

     - **The one-ULP bound on `gap_cv2` was FALSE, and the failure was
       mine in the exact shape I had just written up as the standing
       lesson.** I established the bound over a constructed three-gap
       vector and the 5,000-event artifact, then stated it as a universal
       envelope over all valid input. `--events 14` is valid on both sides
       and gives two ULPs, with every other reported field still agreeing
       bit for bit. The reviewer warned in the same breath against simply
       restating the bound as two, which would repeat the error at a
       larger number, and that warning was well made: a search found
       three, and then three NEARLY-EQUAL gaps turned out to be wrong by a
       FACTOR OF THREE - a 200 percent relative error, because the true
       variance is a difference of quantities agreeing in all but the last
       two bits. So the deviation is UNBOUNDED and the ULP framing was
       wrong in kind, not in degree. Closed by claiming no ceiling at all:
       `gap_pvariance` now documents the deviation as algorithmic and
       unbounded, states what IS guaranteed instead (the value is a
       deterministic, platform-independent function of its input, since
       `py_fsum` is exact and the IEEE operations are correctly rounded),
       keeps the tolerance resting solely on reachability, and turns
       `--fit-markov` from a caveat into a hard gate. The three cases are
       pinned as regression pins on our own values plus recorded CPython
       observations, never as bounds.

       A SECOND ERROR INSIDE THE FIX, worth the space because it is the
       same disease in miniature: the first version of the
       factor-of-three test asserted a huge ULP distance against a
       hand-rolled "exact" reference that was itself a naive sum, sitting
       1.5e14 ULPs from both real values. It passed while measuring
       nothing. Replaced with CPython's value pinned by bit pattern, and
       the inputs are hex float literals so the case cannot drift through
       decimal parsing.

     - **The green gate did not reproduce.** `brokkr check --gate` stopped
       at `clippy::semicolon_if_nothing_returned` in `characterize.rs`.
       Cause was ordering: I ran the gate, THEN `brokkr fmt`, which
       wrapped a match arm into a block and introduced the lint, and I
       never re-ran. The 682-pass claim described a tree that no longer
       existed when I reported it. The reviewer also noted the claim sat
       on an uncommitted tree and so was attached to no reviewable commit.
       Fixed, and the gate is now run AFTER formatting as a matter of
       order: 684 passed, 0 orphaned.
  1h. **THE FOURTH REVIEW PASS, 2026-08-08. REFUSED, and RULED: build the
     exact variance accumulator.** The ruling is the valuable part, so it
     is recorded as reasoning rather than as an instruction. Reachability
     proves `gap_cv2` cannot change the density exit status; it does NOT
     make a printed number sound, because CLI output has human and
     external consumers outside the in-tree call graph. Determinism is not
     correctness. Suppressing the field was rejected too, since that would
     remove Python capability rather than preserve it. So the closure is
     to preserve it ACCURATELY.

     It also found the platform-independence claim made for the float
     version was not true as implemented: `f64::powi` sat in the same
     expression, and its precision is documented as varying by platform
     and Rust version. Two more `powi(2)` sites were in the ACF
     denominator and the `gap_cv2` division. All are explicit
     multiplication now, which is correctly rounded everywhere.

     `crates/mogwai-lab/src/exact.rs` computes the same rational CPython
     does. Every finite binary64 is an integer times a power of two, so
     against the sample's smallest exponent both sums become exact
     integers, `n*Q - S^2` is an exact integer, and the ONLY rounding is
     the final division by `n^2` - once, to nearest, ties to even. It
     carries a minimal arbitrary-precision natural (add, subtract,
     schoolbook multiply, shift, divide by one limb) rather than pulling a
     general-purpose bignum dependency for a single function.

     ORDER OF WORK, because it is the whole methodological point of this
     entry: the identity `(n*sum(x^2) - (sum x)^2) / n^2` was verified
     against `statistics.pvariance` over 2,005 cases BEFORE the module was
     written. It is not an inference from reading CPython's source, and it
     is not a hypothesis the tests were then written to confirm.

     The evidence is a generated sweep rather than fixtures, since
     hand-picked fixtures are exactly how three ULP ceilings got claimed
     and refuted. `scripts/gen_pvariance_cases.py` emits 820 cases across
     seven families - deliberately including the clustered and
     adjacent-neighbour ones that broke the float version - with inputs
     and expectations both as raw bit patterns so no decimal float parser
     sits on the critical path of an arithmetic test. All 820 pass exactly.

     `gap_cv2` itself is now pinned bit-for-bit against CPython at five
     places, the reviewer's own list: the nearly-equal factor-of-three
     vector, the original three-gap vector, the real `--events 14` path,
     the 5,000-event fixture and the DEFAULT 3,000,000-event run. That
     last one closes the hole this whole thread came from - the default
     density report previously had no gate of its own, since the parity
     test beside it asserts only the structural verdict. Gate cost of
     adding it: about three seconds.

     `docs/cli.md`'s claim of field-for-field identity with the Python is
     TRUE again, and now says what backs it. It had been falsified by the
     committed counterexamples, which the reviewer caught as a durable
     document asserting something the tree disproved.

     WHAT THIS RETIRES: `--fit-markov` no longer owes a ruling before it
     can consume `gap_cv2`. The debt is gone rather than deferred, which
     is what the reviewer meant by removing it before it can enter the
     ranking path.
  1i. **THE FIFTH REVIEW PASS, 2026-08-08. REFUSED on one arithmetic
     boundary, now closed.** `population_variance` DOUBLE-ROUNDED whenever
     the exact result is a nonzero subnormal: it rounded the quotient to
     53 bits and then scaled by powers of two, and the scaling rounded a
     second time on the way down. Every subnormal is an integer multiple
     of 2^-1074, so its rounding position is pinned at that floor rather
     than at 53 significant bits. Five specific finite inputs came out one
     ULP below `statistics.pvariance`.

     The reviewer supplied the counterexample as bit patterns, ran CPython
     for the reference independently, and - the part worth noting - had
     transcribed the Rust rounding path to confirm the second value and
     checked that transcription against all 820 committed cases first. It
     did not report a difference it had not reproduced from both sides.

     Closed by choosing the rounding position as
     `max(leading - 52, -1074)` and assembling the bit pattern directly
     instead of scaling, so exactly one rounding happens for every output
     class. `scale_by_power_of_two` is deleted rather than fixed; it was
     the wrong shape, not merely wrong.

     WHY THE SWEEP MISSED IT, which is the reusable part. The 820 cases
     included 39 zero results, and those exercise underflow TO zero - a
     different class from correct rounding WITHIN the subnormal range,
     which no family produced at all. The generated sweep was honest about
     its families and simply had no member of that output class. A
     required `subnormal` family now covers it, 120 cases straddling the
     smallest-subnormal and subnormal/normal boundaries, and the test
     asserts the family's expectations really ARE nonzero subnormals -
     without that, a regenerated fixture could satisfy the family check
     while testing nothing, which is the same trap as the naive-reference
     test from the pass before.

     Sweep is 940 cases now, all exact. Everything else in the pass came
     back clean: the integer identity, the normal-range rounding, all
     three `powi` removals, the 3,000,000-event pin, and the two
     judgement calls I had flagged for overruling - keeping
     `gap_pvariance` as a documented wrapper, and keeping the density pins
     beside the implementation while `parity3a` stays honestly scoped to
     the structural verdict - were both confirmed as the right choice.
  1j. **THE SIXTH REVIEW PASS, 2026-08-08. REFUSED on a debug-only
     boundary, now closed.** The direct-assembly branch keys off
     `round_position == -1074`, and that condition covers more than the
     subnormals: `round_position` is `max(leading - 52, -1074)`, so it
     pins to the floor for every result whose leading bit sits at or below
     2^-1022 - which is the whole LOWEST NORMAL BINADE as well, from
     2^-1022 up to just under 2^-1021. The assembly is correct for all of
     it, because binary64 encodes both ranges as the same integer multiple
     of 2^-1074. The `debug_assert` I put beside it was not: it read
     `mantissa <= 1 << 52`, one binade too narrow, so DEBUG builds
     panicked on values release computed correctly.

     A wrong bound is worse than no bound, precisely because it fails in
     the configuration meant to be the stricter one. Widened to
     `mantissa < 1 << 53` with the branch's real span written out.

     TWO TESTS OF MINE FAILED TO CATCH IT, in different ways, and both are
     worth naming. The boundary test drove `x^2/4` with `x = 2^-510`,
     which lands EXACTLY on the join where the mantissa is exactly 2^52 -
     satisfying an assertion that was wrong for everything above it. And
     the `subnormal` generator family said in prose that it straddled the
     subnormal/normal boundary while its filter kept only results strictly
     below `MIN_POSITIVE`, so every case sat on one side. The prose was
     aspirational and the code was not, which no amount of rereading the
     prose would have revealed.

     Closed with a `lowest-binade` family, 60 cases, AIMED rather than
     sampled: ordinary series around 1e-154 land subnormal far more often
     than not, so the family is built from the `x^2/4` identity with `x`
     in `[2^-510, 2^-509.5)`, which puts the result in the binade by
     construction. The test asserts those expectations really are in
     `[2^-1022, 2^-1021)`, the same guard the subnormal family already
     carries. Sweep is 1,000 cases.

     Everything else came back clean, including a 200,000-case release
     comparison the reviewer ran independently.
  1k. **SIGNED, 2026-08-08, at `5d3a0af`.** Session 019fe13a signs the
     Python retirement after six refusals. Its evidence: clean committed
     tree, the interior lowest-normal-binade debug test passing, all 60
     generated lowest-binade expectations independently verified to fall
     in the required range, the 1,000-case CPython differential sweep
     passing bit for bit, and `brokkr check --gate` at 698 passed, 25
     ignored, 0 orphaned. It also reported no dossier briefing failure.

     THE SIGNATURE IS CONDITIONAL, and the conditions are binding on
     everything below rather than advisory:

     - Phase 4b keeps this ORDER: item 6 (TBBO row-width validation and
       fallible numeric refusal) while the Python is still runnable, then
       item 5 (per-mode subcontract hashing), then item 2
       (`select_windows.py`), then item 3 (`tick_composition_ratios.py`),
       and item 7 moves the absorbed scripts only after every preceding
       gate is green. The ordering is not arbitrary: item 6 loses its
       reference the moment item 7 runs, and item 5 settles provenance
       structure before more absorbed methods are added to it rather than
       migrating hashes twice.
     - ANY new parity deviation, or any change to the retirement SCOPE,
       reopens the gate. A signature against this tree is not a signature
       against a later one.
     - It does NOT certify the release lateness budget. That property
       stays explicitly uncertified; see 1e and `notes/todo.md`.
     - It accepts the `TAPE_PROTOCOL_VERSION` exemption ONLY for the
       audited `modal_tick.max` correction, and does not generalize it -
       matching the wording already in `AGENTS.md`.
  2. **IN PROGRESS. The blessing is DONE, 2026-08-08; the port is not.**
     This item's distinguishing problem is that it has no frozen artifact
     to port against - the script prints its results and writes only
     `cme_daily_features.json`, a regenerable gitignored cache, and
     `targets-frozen.json` is the BTCUSDT target set it never touches. So
     the reference had to be MADE, from the current Python, before any
     porting: `scripts/bless_select_windows.py` writes
     `analysis/select-windows-blessed.json`, committed.

     It IMPORTS `select_windows` rather than reimplementing it, so the
     artifact is the Python's own arithmetic and not a second opinion
     about it, and it captures the deterministic STRUCTURE rather than the
     printed tables - per-month medians, the eligible span, the z-scored
     vectors with their key order, the seeds and the selection. A port
     matched against printed text would be pinned to formatting; one
     matched against the structure reproduces the tables for free. Floats
     are exact decimal round-trips, so the eventual comparison is
     bit-exact rather than close.

     Run over the four archives: 210 months, 193 eligible from
     2010-06 to 2026-06, seeds 2020-03 and 2026-06, selection 2011-08,
     2014-07, 2017-10, 2019-05, 2020-03, 2020-04, 2025-04, 2026-03,
     2026-06. Stable across re-runs. The archives' sizes and SHA-256
     digests are recorded in the artifact's provenance, because the cache
     is gitignored and the archives sit outside the tree - without that, a
     later mismatch could not be told apart from the inputs having
     changed.

     THE PORT LANDED the same day: `mogwai_lab::select_windows` plus
     `mogwai select-windows features|select|drift|plan`. All four phases
     reproduce the blessed artifact BIT-EXACTLY - month table, eligible
     span, z-scored vectors in key order, seeds, pick order, percentiles,
     drift years and plan strata - and the `select` and `plan` CLI output
     is byte-identical to the Python's.

     Both recorded traps survived contact. `DATABENTO_START` re-centres
     rather than filters, and `drift` uses a DIFFERENT median from
     `monthly` - the upper middle on an even count - so it gets its own
     comparison rather than being assumed to follow from the month table
     matching. Two more turned up: CPython's `round` is half-to-EVEN and
     `phase_plan` indexes with it, and the hourly volatility buckets and
     month table must both stay insertion-ordered because they are term
     orders for `py_sum`.

     A NEW PARITY DEVIATION, DISCLOSED, since the signature says any such
     deviation reopens the gate. The blessed gate passed on the first run,
     but comparing the CACHES directly - which the blessed artifact cannot
     see, being derived from them - found ELEVEN of 111,396 values
     differing by one or two ULPs, all `volume_cv` or `vol_of_vol`. Cause,
     confirmed on the actual failing session rather than inferred:
     the Python squares with `** 2`, which routes through libm's `pow` and
     is NOT correctly rounded, disagreeing with the correctly rounded
     product about one value in 1,163. A single IEEE multiply always is
     correctly rounded, and exact rational arithmetic confirms the
     multiply is the right answer. So the port is correct and CPython
     carries a libm artifact - the same shape as `x ** 0.5` against `sqrt`
     in the fingerprint work, and approved on the same ground: matching
     bug-for-bug would make this tool's output a function of whichever
     libm the machine carries.

     STATED PLAINLY BECAUSE IT MATTERS: no month median moves on today's
     archives, so the blessed gate passes - BY COINCIDENCE, not by
     construction. A different corpus could put one of those eleven
     sessions on a median's middle.

     RULED 2026-08-08, session 019fe1ff, and the ruling was SOUGHT rather
     than assumed. The first version of this work approved the deviation
     in its own source comment, citing the `sqrt` precedent - which
     inherited that ruling's reasoning while helping itself to its
     authority. The signature says any new parity deviation reopens the
     gate, so the ruling was not mine to make. What came back:

     - **The multiply is approved**, on a ground I had not argued: the
       tool being OFFLINE makes correctness MORE important, not less,
       because a purchase decision should not depend on the host libm when
       a correctly rounded portable operation exists.
     - **The blessed artifact stays PYTHON-DERIVED.** I had offered
       re-blessing from the Rust as the alternative; it was refused, and
       the reasoning is better than mine. On today's archives a Rust
       re-blessing would produce identical monthly values, so it would
       change the claimed provenance, record no numerical correction,
       preserve the same blind spot, and destroy the independent Python
       oracle - all without gaining coverage. The correct shape is TWO
       LAYERS: keep the Python-derived artifact as the legacy behavioural
       reference, and add a lower-layer Rust reference that pins the
       corrections where they actually occur.
     - **The signature is reopened**, and not renewed by the approval
       alone. The retirement SCOPE is unchanged.

     TWO FURTHER DEFECTS came back with it, both real, both now closed:

     1. **The gate did not verify archive provenance.** It compared
        session COUNTS while its own assertion message claimed that
        established the archives were the blessed ones. It does not:
        archives can change while the qualifying-session count holds, and
        changes confined to discarded sessions are precisely the class the
        monthly artifact cannot see. It now verifies the recorded SHA-256
        digests and byte sizes.
     2. **A FIFTH ordering trap, and a correctness bug rather than a
        rounding one.** `parse_line` built a minute-resolution stamp and
        discarded seconds, while the Python compares whole `datetime`s and
        derives missing minutes from the full difference. Two valid rows
        at `17:00:00` and `17:00:30` therefore diverged: the Python takes
        both, the port dropped the second as a duplicate. Related, and
        also fixed: the Python's `datetime` refuses `31/02` and a seconds
        field of 60, while the port validated the day against `1..=31`
        and never checked seconds at all. The committed archives are
        minute-aligned and well formed, so NO corpus gate could have
        exposed either - they are pinned by synthetic discriminators now,
        including leap-year cases in both directions.

     The manifest is `analysis/select-windows-cache-deviations.json`, and
     its gate re-derives the difference set rather than trusting the file:
     exactly the recorded eleven, no fewer so a correction cannot vanish,
     no more so a new divergence cannot hide among the approved ones.

     The original text follows.

     **ABSORB `select_windows.py` WHOLE**, all four phases, as the
     bar-frame intake station on top of `session_profile.rs`'s existing
     archive, session and eligibility machinery. No frozen artifact
     exists to gate against: `cme_daily_features.json` is a regenerable
     gitignored cache and `targets-frozen.json` is the BTCUSDT
     microstructure target set this script never touches. So bless a
     selection artifact from the current Python over the four CME zips
     FIRST, then match the port to it. The BTCUSDT rejection
     (`analysis/association-result.json`) travels as a recorded prior on
     `select`/`plan`, not as grounds to drop them - and per the
     preregistration's section 7.1, only the `rv`-rank association was
     ever tested; the five-feature farthest-point selection the code
     actually implements was never on trial in either direction.
     Re-running that test on a second instrument additionally needs
     `build_bars.py`, `build_targets.py`, `spearman_association.py` and
     `run_association.py` resurrected from `9170f45`.
  3. **DONE 2026-08-08.** `mogwai_lab::tick_composition_ratios` plus
     `mogwai tick-composition-ratios compare|verify89-identity|modes`.
     All four modes and both gate paths reproduce the blessed reference
     BIT-EXACTLY, and the `compare` output is byte-identical to the
     Python's - integral rendering included, since the proposals are
     integers by construction and printing `67108864.0` for a value
     somebody pastes into a source constant is a small lie about what it
     is.

     Every constraint recorded below held:

     - **Its own subcommand**, not a `--report` mode. `tick-composition`
       measures a tape; this turns a measurement into constants and
       refuses landings. Fused, one invocation could measure a fixture and
       bless it in the same breath.
     - **The per-mode baselines are committed DATA**, frozen history, with
       a test pinning them and a second asserting `max_extend_ticks` is
       `1 << 30` in every mode - it is a per-lock runaway backstop rather
       than a reach ceiling and is deliberately never scaled.
     - **`CALENDAR_FREE`/`CALENDAR_BEARING` are now DERIVED**, from whether
       a preset carries a calendar, through the server's own loader. The
       preset list comes from the FIXTURE rather than a constant, which is
       the point: a sixth instrument appears there and gets classified,
       where the Python's literal tuples would have left it in neither
       class and the acceptance gate would have checked nothing for it
       while still passing. A test proves the derivation reproduces the
       Python's tuples exactly - without it the claim would be untested
       prose, since the parity gate feeds the blessed lists in
       deliberately.
     - **The rejected protocol-11 fanout proposal carries forward as
       DATA**, in `REJECTED_PROPOSALS`. `independent_10_11` still
       mechanically proposes 16,777,216, and the CLI now prints a refusal
       note when it does. A sizing tool that cannot remember a refusal
       will keep making it.
     - **The five `reference/performance.md` citations moved** in the same
       commit, plus a sixth in `docs/cli.md` that the note did not count.

     THE GATE IS NOT `#[ignore]`d, unlike items 1 and 2. Every input is
     committed - six fixtures plus the blessed artifact - so it runs on any
     clone, every time. A sizing policy behind four shipped constants
     should not be checked only on the machine holding a data delivery.

     The blessing captures the two gate paths that produce no ratios by
     their VERDICT, since that is their whole claim, and the port is
     additionally shown to REFUSE a self-comparison - a gate that only
     ever passes proves nothing about what it rejects.

     The original text follows.

     **ABSORB `tick_composition_ratios.py` as its OWN subcommand**, not
     a `--report` mode on `tick-composition`: it is the sizing policy
     behind four shipped constants (`CHECKPOINT_K`, the sweep drain
     budget, the warmup materialization ceiling, `fanout_depth`) plus
     three protocol-landing acceptance gates, so producer and gate stay
     separate or one command measures a fixture and blesses it.
     Constraints: per-mode baseline tables move as committed DATA and
     are never re-derived (the file's own comment records that sharing
     one table silently under-proposed two constants while every
     assertion passed); `CALENDAR_FREE`/`CALENDAR_BEARING` become
     preset-derived, since hardcoded preset tuples do not survive an
     open instrument set; and the REJECTED protocol-11 fanout proposal
     carries forward as data, or the first 12b run mechanically
     re-proposes 16,777,216 and re-litigates a settled ruling.
     `reference/performance.md` cites this script by name at five sites;
     those citations move in the same commit.
  4. **CLOSED 2026-08-08 at `7852e2f`.** The fingerprint is re-committed
     at `modal_tick.max` 0.1, rebuilt from all eight freshly regenerated
     reports; the diff is exactly one leaf, and XBTUSD, the anchor,
     reports 0.1 directly. What the stale value bought was a false
     negative - MNQ's 0.25 tick sat exactly ON the inclusive ceiling, so
     the preset cleared the corpus-range check by coincidence. The preset
     now accepts the diagnostic in provenance and MES inherits it; the
     inverted provenance test is rewritten around both directions of the
     contract. The parity gate's `allowed` list is DELETED rather than
     updated, and its absence is the evidence. No version bump, per the
     exemption above. Regenerating the reports also showed the staleness
     is broader than this leaf: two `n_hist` bins disagreed with their
     sums matching exactly, which looked like a binning defect in the
     port until a fresh Python run reproduced the Rust numbers. The
     original text follows.

     **RE-COMMIT `analysis/fingerprint.json`** regenerated from today's
     `char_*.json`, accepting `empirical_ranges.modal_tick.max` 0.25 to
     0.1. No tape byte depends on it - the artifact's own `_doc` says
     the ranges are diagnostics only and `.max` is read at exactly one
     site, `Scalars::empirical_diagnostics`. Land together:
     `generator.modal_tick` in `presets/mnq.toml` gains
     `accepted_diagnostics = ["outside-empirical-corpus-range"]`,
     matching what that preset already declares for three other fields;
     the third arm of
     `shipped_preset_diagnostics_require_exact_provenance_acceptance`
     inverts and must be rewritten against a value genuinely in range;
     and `crates/mogwai-lab/tests/parity3a.rs`'s `allowed` exception is
     DELETED, its disappearance being the proof. Verify whether MES
     declares the knob or inherits it. Record the regeneration
     provenance in the commit - `char_*.json` is gitignored, so the
     inputs cannot be reconstructed from the tree.
  5. **DONE 2026-08-08.** `Mode::Protocol11`/`Mode::Protocol12a` plus
     `subcontract_hash_for` and `subcontract_dumps_for`. The flat
     `subcontract_hash` is UNTOUCHED, deliberately: it is what
     cross-language parity is checked against and `mnq_fit.py` has no
     per-mode equivalent, so the scoped hashes are additive and apply to
     artifacts written from here on. `mnq-fit.json`'s committed
     `binding.subcontract_hash` 35e5b033 is likewise untouched, per the
     owner's ruling - it records what the protocol-11 fit actually ran
     under, and rewriting it would assert a binding that never happened.

     THE BOUNDARY IS TAKEN FROM THE PYTHON, not invented here.
     `mnq_fit.py`'s `SUBCONTRACT_KEYS` carries its own
     `# Protocol 12a` section marker, and everything after it is the 12a
     set - 40 keys. That marker is the only place in either language that
     records which mode reads which constant, so it is the authority.

     A NOTE ON THE TEST, because the obvious one is worthless. The natural
     assertion - "every key belongs to exactly one mode" - cannot fail,
     since `Protocol11` is DEFINED as "not in the 12a list". It would be a
     tautology in the costume of a partition proof, and this program has
     already shipped two tests that passed while measuring nothing. The
     real risk is MISCLASSIFICATION, so the test parses `mnq_fit.py`'s key
     list at the section marker and compares the two sides, plus asserts
     every named key actually exists in the sub-contract so a typo cannot
     move a key into protocol-11 while both lists still agree. That test
     dies with the Python at item 7, which is correct: after that
     `PROTOCOL_12A_KEYS` IS the authority and there is nothing left to
     check it against. Landing it before then is the point of the
     signature's ordering.

     The original text follows.

     **Scope the subcontract hash BY MODE.** One flat
     `SUBCONTRACT_KEYS` namespace means any constant edit retroactively
     unbinds every prior fit, including for constants that fit never
     read. With one instrument that is a curiosity; with a dozen, adding
     one 12b constant silently unbinds every committed fit artifact and
     the next reader concludes the corpus is stale when nothing moved.
     (`mnq-fit.json`'s own binding hashes STAY as committed - 35e5b033
     accurately records what the protocol-11 fit ran under. A fresh fit
     buys extensibility, not correctness, and is owner-authorized
     whenever convenient to bundle.)
  6. **DONE 2026-08-08.** The parity-frozen TBBO defect is fixed, first
     in the signature's required order and deliberately while
     `mnq_fit.py` is still runnable as the reference it diverges from.

     Both halves, which are independent - fixing either alone leaves the
     other reachable. `ColumnIndices` now carries `required_width`, one
     past the largest index any of the ten columns will dereference, and
     the row loop refuses a short row ONCE before the first indexed
     access. That ordering is the point: the panic was at
     `parts[idx.ts_event]`, earlier than any conversion, so fallible
     parsing could never have reached it. Separately `parse_field_i64`
     returns `LabResult` and names both the column and the offending
     value; the six call sites route through one local macro rather than
     six identical matches, since `Iterator::next` cannot use `?`.

     The divergence from the Python is confined to malformed input, which
     is what makes it safe to land before retirement: the parity gates
     compare output over well-formed corpora where neither refusal is
     reachable. Confirmed rather than assumed -
     `parity12a_observed_per_session_matches_the_committed_artifact`
     re-passes over the real 22-session corpus in 84.4 s.

     Fixtures are real `.csv.zst` files, not synthesized in process,
     because the defect lived in the streaming reader and a fixture that
     bypassed the decoder would not have exercised it. Each carries a
     WELL-FORMED row ahead of the bad one, with a third test asserting
     that row parses - without it, a reader that refused everything would
     pass both refusal tests.
  7. **DONE 2026-08-08. THE PYTHON HAS RETIRED.** Eight absorbed scripts
     moved to the gitignored `research/dead/`: `mnq_fit.py`,
     `characterize.py`, `build_cadence.py`, `build_fingerprint.py`,
     `check_cadence_feasible.py`, `fit_session_profile.py`,
     `select_windows.py`, `tick_composition_ratios.py`. Git history is the
     real archive.

     FIVE PREPARATIONS LANDED FIRST, all of them ruled by the review that
     refused to sign item 7 as originally specified - it would have
     orphaned a parity gate, left a retained test module unimportable,
     left three dead executables in `scripts/`, and left binding
     documentation false. Every one had to happen while the Python could
     still be captured:

     1. **The full Python cache is COMMITTED**, as
        `analysis/select-windows-python-cache.json`. My proposal was to
        reduce the cache-deviation gate to a snapshot of the eleven known
        corrections; that was refused, and correctly. An eleven-row
        snapshot confirms the known corrections still hold but CANNOT
        notice a twelfth deviation appearing, which is the entire property
        the re-derivation existed for. 3.7 MB buys keeping it. The gate
        still rebuilds the Rust cache from the four archives, still
        verifies the archive digests, and still requires all 111,396
        comparisons with exactly eleven differences.
     2. **The subcontract cross-check is DELETED**, not redirected.
        `the_twelve_a_classification_matches_the_python_section_marker`
        drew its authority from `mnq_fit.py`'s `# Protocol 12a` marker;
        pointing it at the Rust would have made it assert a list matches
        itself while still looking like a cross-check. What remains is
        narrower and honest: every classified key exists, protocol 11
        retains keys of its own, and the set is pinned at the 40 keys the
        Python marker last validated - verified against the Python one
        final time before the move. A change there is now a
        classification decision with no oracle, and needs its own
        argument.
     3. **`test_characterize.py` is TRIMMED to its four retained tests.**
        This was a missed runtime dependency and the sharpest catch of the
        review: the module imported four retiring modules at load, so
        after the move the WHOLE file would have failed to import and
        taken the four probe tests - over scripts that explicitly stay
        Python - down with it. A retained suite silently not running
        because of what was removed around it.
     4. **Three spent helpers retired with the material they import**:
        `bless_select_windows.py`, `bless_tick_composition_ratios.py` and
        `probe_cme_square.py`. Their purposes are complete - the two
        blessings produced committed artifacts, the probe established the
        squaring cause now recorded in the manifest. Leaving executables
        in `scripts/` that fail only because their dependencies were
        deliberately removed is not an acceptable retirement state.
        `compare_cme_caches.py` is retained and now DEFAULTS to the
        committed cache, since it imports nothing retiring.
     5. **Three false documentation claims corrected**: `AGENTS.md` and
        `mogwai-cli/Cargo.toml` both said the Python execs
        `target/release/mogwai`, and `mogwai-lab/src/lib.rs` called the
        removed file "the Python reference for everything here" and gave
        it the tie-break. That lib.rs header now states what replaced it:
        this crate IS the reference, what survives of the oracle is
        committed artifacts and git history, and a question those cannot
        answer needs its own argument rather than an appeal to a program
        that no longer runs. Two prose references in the retained
        `databento_price.py` were repointed as well.

     AFTER THE MOVE, every gate still runs: `brokkr check --gate` at 722
     passed and 0 orphaned, both `select_windows` corpus gates, and the
     84-second observed 12a parity gate over the real 22-session corpus.
     `python3 -m unittest analysis.test_characterize` passes its four.

  NOT retiring, and the phase should not discover this late: the eleven
  permanently-KEEP scripts of the "Permanently out of scope" section
  below, plus `probe_binance_klines.py` (a live `test_characterize.py`
  import, mis-filed DEAD by the triage) and `test_characterize.py`
  itself, whose four surviving tests assert behavior in scripts that
  stay Python.

  LANDED 2026-08-07 (4a). No deletions, per the phase's own constraint -
  `mnq_fit.py` and every absorbed script stay in place; `mogwai-protocol`
  received the one permitted comment-only touch. `reference/architecture.md`
  gained a "The workspace and the offline evidence toolbox" section: the
  seven-crate layout, `mogwai-lab`'s dependency direction (depends on
  `mogwai-data`/`mogwai-protocol`/`mogwai-server`; `mogwai-server` depends on
  none of it, no cycle), the parity-gate testing story and the storage
  policy as built. `docs/cli.md` gained an "offline evidence toolbox"
  section covering `preflight`/`measure`/`fit`/`synth fingerprint`/
  `synth cadence`/`cadence-feasible`/`cache` in the file's existing prose
  register. `AGENTS.md`'s workspace section gained the `mogwai-lab` entry
  (seven crates, matching the `mogwai-cli` entry's style) and corrected "the
  other five" to "the other six" build nautilus-free. `reference/
  performance.md` was checked against the moves and needed no change - every
  path and target name it cites (`mogwai-server/src/serve.rs`,
  `brokkr run mogwai -- tick-composition`, `analysis/mnq-fit.json`,
  `tick_composition_ratios.py`) still resolves exactly as stated.

  `crates/mogwai-protocol/src/launch.rs`'s stale doc comment (flagged by
  phase 0) is fixed: "the real venue in `mogwai-server`'s lifecycle gates"
  now reads "`mogwai-cli`'s lifecycle gates", where the socket-backed
  integration tests actually live post-phase-0.

  `notes/rust-rewrite-review-dossier.md` is new: the program-level map for
  the codex review pass - phase/gate/commit table, every pinned
  cross-language convention with its story, every parity gate and its
  exclusions, the parity-frozen defect (section 4), the documented scope
  gaps including the cadence-feasible Markov re-simulation, the phase-2b
  spec-thinness notes (rung 4a/5a completeness gates, the unpaired
  `a_print_excess`) that had been recorded in the 2b commit message but
  never copied into this file, the phase-2a accidental-agreement findings
  (floor-division vs refusal, segment order), both drift findings awaiting
  owner decisions, the standing-process deviations this program ran under,
  and the open owner decisions already in `notes/todo.md`.

  `test_characterize.py` dissolution VERIFIED by direct correspondence
  rather than by re-trusting the phase-3a landing claim: of its 31 tests
  across 7 classes, 27 assert behavior inside the ABSORB set and all 27 now
  have a verified Rust counterpart; 4 assert behavior in scripts the triage
  correctly kept Python (`probe_binance_klines.py`/`probe_binance_aggtrades.py`),
  outside the dissolution's scope. Two Python assertions had no prior Rust
  counterpart and are added this phase: `fingerprint.rs::the_committed_cadence_is_loadable`
  (`load_cadence()` against the real committed `analysis/cadence.json`) and
  `cadence.rs::probe_returns_structured_result_over_a_synthetic_fixture` (a
  byte-for-byte port of the Python's synthetic 3-row Binance-trades-zip
  fixture, pinning the small-N event-grouping distinction the phase-3a
  landing record's "covered live by gate 2" claim did not actually cover).
  Full mapping table in the dossier's section 11.

  No parity-frozen defect table was added to `notes/todo.md`: the only
  entry in that class (the TBBO stream contract's unguarded numeric
  conversions, phase 1) already reads as a decision-ready single item: a
  table would have one row. The dossier indexes it in section 4 instead.

  `brokkr check` green with the two added tests; no touch to
  `mogwai-data`/`mogwai-engine`/`mogwai-adapter` or any Python source. Not
  committed. The program now STOPS at the codex gate per the owner ruling
  above.

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
