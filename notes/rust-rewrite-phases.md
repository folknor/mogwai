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
- **2c, assembly and the CLI**: artifact assembly, the schema and
  semantic validators, the resource-sampled cost contract, `mogwai
  measure`. GATE, the program's centerpiece: reproduce
  `analysis/mnq-measure-12a.json` typed-canonical-identical from the
  same corpus and walk caches. measure12a.rs and the gen measure
  dispatch are fully retired from cli here.

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
