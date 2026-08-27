# mogwai architecture: the workspace and the offline evidence toolbox

Part of the architecture reference. The map and the reading order are in
`architecture.md`; this file was split out of the one long document on
2026-08-27 as a contiguous slice, with nothing moved, cut or reworded.

## The workspace and the offline evidence toolbox

Seven crates. `mogwai-protocol` owns the wire types and the shipped launcher and
imports nothing else in the workspace. `mogwai-engine` is the venue-agnostic
exchange core. `mogwai-data` owns `TickSource`, the k-way merge and the
`GeneratedSource` synthetic generator fitted to the committed fingerprint.
`mogwai-venue` is a library - it owns the sockets, the clock and the replay
pacing, and ships no binary of its own. `mogwai-cli` is the `mogwai` binary: a
clap dispatcher over `serve` (which does no work itself, just forwards to
`mogwai_venue::serve`) plus every offline subcommand. `serve` is the only one
that binds a socket; the rest are the intake and measurement surface - `gen`,
`tick-composition`, `presets`, `man`, `preflight`, `measure`, `fit`, `cache`,
`synth`, `cadence-feasible`, `characterize`, `select-windows`,
`session-profile`, and the protocol-12 instruments (`count-curve`, `stage-m`,
`minute-range-envelope`, `arrival-control`, `arrival-screen`,
`arrival-envelope-diagnostic`, `tick-composition-ratios`). `mogwai --help` is
the authority on the current set. `mogwai-adapter` is
the lone nautilus-dependent crate, unchanged by anything below.

One binary is a standing decision, not an accident of growth. A split into a
venue binary and a lab binary was proposed and refused 2026-08-20, and a
re-proposal has to answer three things. First, `arrival-control`'s B1 gate
execs `gen` on `current_exe` so the binary generating the byte comparison is
the very binary under test - the driver cannot disagree with itself about which
build ran - and a split reintroduces exactly the build-identity ambiguity that
design forecloses. Second, the size benefit is already banked: the method
lives in `mogwai-lab`, which stays linked into the venue binary regardless,
because `gen` reaches into it and `main` calls `sidecar::init` before the argv
parse; a split relocates thin driver layers while the intake method remains
shipped. Third, the cost is on the order of two hundred path rewrites across
one-shot brick drivers, relocated integration suites, a moved attestation
roster, and a new build-identity mechanism to replace the one the split
destroys. Two potential hard blockers were checked and are not part of the
refusal: the crate direction admits a lab binary, and the `test-seam` cfg
survives a move - the refusal rests on the three arguments, not on a build
obstacle.

`mogwai-lab` is the fifth non-adapter crate: the corpus-to-fingerprint method
library the 2026-08 Python-to-Rust rewrite absorbed from `analysis/` (the
rewrite program's phase records and per-script scope rulings are retired to
git history) - streaming TBBO/Binance-trades
parsing, the protocol-12a measurement engine, aggregation and bootstrap,
fingerprint and cadence synthesis, and the protocol-11 session-calibration
fit. Its dependency direction is one-way and asymmetric: `mogwai-lab` depends
on `mogwai-data`, `mogwai-protocol` and also `mogwai-venue` (session-summary work
needs to resolve an `InstrumentProfile` through `Config::load` exactly as the
Python's `--config` scratch walks did), but `mogwai-venue` depends on none of
it - there is no cycle, and `mogwai-lab` stays out of the tape-generation path
`TAPE_PROTOCOL_VERSION` scopes, the same reason `measure12a.rs` was
consumer-only inside `mogwai-venue` before the rewrite moved it. `mogwai-cli`
depends on `mogwai-lab` for the pieces that need no `mogwai-venue` preset
resolution
(preflight, cache, most of measure/fit/synth) and calls straight into
`mogwai-venue` for the generated side of measurement.

The instrument set is open, and that is why `mogwai-lab` is a library rather
than a folder of scripts. A symbol is a request string, never an admission
identity. `InstrumentDef` is derived through one path from the symbol and the
operator overlay: an explicit preset, a matching preset, or the NVDA default
bundle. No second hardcoded default bundle exists, and no symbol is refused for
wanting a fit. The four shipped presets - NVDA, MNQ, MES and BTCUSDT - are the
current state, not the end state.

Config declares no closed instrument set. It supplies a default knob overlay
and optional case-insensitive per-symbol overlays for total symbol resolution.
The top-level default symbol is what a request carrying no symbol binds - a
carrier convenience for consumers that predate the parameter, not a privileged
river. It is materialized on first request like any other, and other request
symbols materialize their own rivers in the same run.

The intake sequence therefore makes a river better and gates nothing:
survey what cheap data exists, decide whether a paid corpus is worth buying
and which windows of it, buy, preflight, measure, characterize, fit, ship a
preset with its provenance. The offline toolbox is that sequence made
reusable, and the two consequences bind anything built on it. A component is
spent only when its question cannot recur, never merely because the MNQ pass
answered it - an archive inspector or a corpus driver is idle between
instruments, not dead. And per-instrument knowledge belongs in config or a
preset rather than a hardcoded list in the method: a preset tuple naming
today's three symbols is a defect the fourth exposes. The corollary for
evidence is that a finding measured on one instrument is one observation, not
a law, until a second instrument either reproduces it or does not - which is
why methods a preregistered test rejected are kept runnable rather than
deleted.

The second consequence is the direction of travel, not a met invariant, and
stating it as met would be false. The offline toolbox still fixes
per-instrument choices in source, faithfully mirroring the Python it was
ported from rather than introducing the debt: `cadence.rs` fixes the pair set
and the archive month and takes BTCUSDT as anchor, `fingerprint.rs` takes
`XBTUSD` as anchor, and both `session_profile.rs` entry points resolve the MNQ
preset. None of these is reachable as an input. Retiring the Python removes no
parameterization that exists today - it was equally hardcoded - so closing
this is forward work rather than a porting debt, and it is what a second
instrument will force.

The parity contract a port is held to, stated once because every case
otherwise gets argued from scratch: for every valid input, the Rust must
either produce output equivalent to the implementation it replaces or embody
an explicitly approved semantic change. It may additionally reject inputs
outside the declared input contract. It may never silently accept malformed
input, and it may never silently change results for valid input.

The line that follows from it, and the reason it is worth writing down: a
gate passing on the committed fixtures is evidence about those fixtures, not
proof of equivalence over the contract. So a Rust refusal where the original
proceeded is a loud narrowing and needs only to be recorded; a Rust result
that differs on some valid input the fixtures happen not to contain is a
silent mismatch and must be fixed or approved; and a Rust default where the
original raised is silent acceptance of malformed input, the worst of the
three, because it manufactures an answer. Fixing the third class by making
the committed artifact pass again is not a fix - the repair needs a fixture
chosen to distinguish the implementations, or the blind spot survives.

The rewrite's parity gates are the porting program's whole verification
story: every absorbed Python computation is checked against a committed JSON
artifact - `mnq-fit-preflight.json`, the observed and generated halves of
`mnq-measure-12a.json`, `cadence.json`, `fingerprint.json`, `mnq-fit.json` -
typed-canon-identically, with named, individually-verified exclusions for
genuinely live fields (wall-clock cost, the binding harness commit) rather
than a blanket tolerance. The gates live under
`crates/mogwai-lab/tests/parity3a*.rs`/`parity3b*.rs` and
`crates/mogwai-cli/tests/parity12a*.rs`/`parity3b.rs`, `#[ignore]`d because
they need local corpus or archive state on disk, and are excluded from
`brokkr.toml`'s complete profile by the shared `parity12a_`/`parity3a_`/
`parity3b_` naming prefix. The program-level review dossier - every gate,
every pinned cross-language convention (compensated float summation,
insertion-ordered accumulation, the ported CPython float repr and Mersenne
Twister, and the rest) and every owner decision the review adjudicated - is
retired to git history; the review signed and the program is complete.

The storage policy `mogwai_lab::storage` implements keeps three classes of
on-disk data apart, never mixed. Artifacts (preflight, measurement and fit
outputs) are the user's files: written to `--out` or a subcommand's own
working-directory default, never cached, never auto-deleted. Cache
(recomputable, keyed data - walk summaries, measure12a walk records) lives
under `$XDG_CACHE_HOME/mogwai/` (falling back to `~/.cache/mogwai/`),
overridable by `MOGWAI_CACHE_DIR` or `--cache-dir`, keyed by a
`ProvenanceToken` folding in the crate version, `TAPE_PROTOCOL_VERSION`, the
fingerprint hash, the full invoked command line, the measurement
sub-contract hash and (when built from a tree) the git sha; entries under a
stale token are unreachable by construction and pruned automatically on
write, with `mogwai cache stats`, `mogwai cache stats --entries`,
`mogwai cache clean` and `mogwai cache clean --stale --keep TOKEN` covering
the manual case. `--keep` is required with `--stale`, and the token must name
a directory that is actually present: a cache entry's provenance token binds
the command that produced it, which a `cache` invocation cannot derive, and a
token matching nothing keeps nothing - so both the missing and the mistyped
token refuse rather than pruning the lot. `stats --entries` prints the
candidates. Scratch (per-run temporaries) is a run-scoped directory under the
cache root with a leaf unique to the process, removed when its guard drops -
so two concurrent runs, or the two sweeps a full gate runs at once, cannot
share one. Repo development pins `MOGWAI_CACHE_DIR` to the Python-era
`analysis/out` layout so the phase 1-3b parity gates read the caches those
scripts already produced; that pin is not the installed default.
