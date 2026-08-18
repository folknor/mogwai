# mogwai command line

`mogwai` runs one foreground venue for one run. It owns no PID, log, or
configuration files. Logs go to stderr; `RUST_LOG` selects the tracing filter.

`serve` binds the HTTP and WebSocket endpoint on `127.0.0.1` and an EPHEMERAL
port. Neither half is configurable: there is no `--addr`, no way to name a port,
and no way to serve another interface. The kernel allocates the port, so two
concurrent runs on one machine cannot collide on a shared default and cannot be
made to by an operator hand-assigning ports; and loopback is the only interface
because the venue models latency on the sim axis and runs on the same machine as
its client.

The endpoint therefore has to be learned rather than assumed. On boot the venue
writes ONE line of JSON to STDOUT - the version 8 `ReadyRecord`, carrying
`version`, `addr`, `pid`, `run_seed`, `data_origin_ns`, `run_start_ns`,
`run_duration_ns`, `warmup_ns`, `reset_account_on_reconnect`, `account_ttl_ms`
and `version_string` - and that is the only thing it ever writes there. The last
two are the account-persistence policy: whether a returning client gets its own
ledger back, and how long an unattended one survives before the venue collects
it. It reports no symbol. Logs go to stderr, so
the two never interleave. A launcher captures stdout and reads a line; a human
sees the same line in the terminal.

The WebSocket endpoint is `GET /ws?symbol=<symbol>`. The query parameter is
optional; omitting it binds the socket to the run's boot symbol for compatibility
with older clients. A socket owns exactly one river. A supplied symbol is 1 to
32 ASCII letters, digits, dot, dash, or underscore, and matching is case exact.
Malformed symbols are refused with HTTP 400 before the upgrade. Every legal
symbol is resolved; the only other pre-upgrade refusals are a shape this run
cannot fund or make valid, and an account already seated on that river at a
different speed. The upgrade also accepts `?speed=` (absent means the configured
`speed`) and `?duration_ms=`. The first passenger at a given speed places that
cursor; later passengers at the same speed share it; a different speed places a
second cursor on the same water.
A client names that river from its own configuration; the readiness record does
not supply one.

`GET /instruments` reports the configured shapes plus every river materialized
so far. A socket bind or history poll materializes a river and grows the list.
A run retains at most 256 materialized rivers and never evicts them. This is an
operational bound for trusted clients belonging to the run's owner, not a
hostile-client defence.

The history endpoints `GET /trades` and `GET /quotes` both REQUIRE `symbol`;
`start`, `end` and `limit` are optional. They are bounded by the named river's
now. For a seated river that is the last instant its boat published; for a
boatless river it is the venue clock. An omitted or future `end` is clamped to
that ceiling, and a `start` above it - or below the tape origin - is refused
with HTTP 400. A client must therefore read `/clock?symbol=<symbol>` before
constructing a history window. Using boatless `/clock` as the `start` for a
seated river can be ahead of that river and is refused.

A history poll materializes the named river, so the only refusals left are
about the SHAPE rather than about the symbol being unknown: a label that is not
a legal symbol, a shape whose settlement currency this run does not fund, a
shape the resolved configuration makes invalid, and an exhausted river cap.
Each is a 400 naming its reason.

`GET /clock` takes an optional `?symbol=`. Naming a SEATED river answers on its
boat's clock: `server_now_ns` is the last instant that boat published, and
`boat_clock` is true. Naming a boatless river, or omitting the parameter,
answers on the venue clock with `boat_clock` false. `data_origin_ns` and `warmup_ns` are venue facts either
way, identical for every river.

`GET /account` returns the venue-wide ledger. Its `ts_event` is venue time and
the top-level `clock` field is `"venue"`. Pushed account events are stamped on
their boat clocks, so consumers order pulls against pushes by protocol
sequence, never by comparing timestamps across those axes.

There is no flag for this and nothing to opt into. The record used to be gated
behind `--ready-fd <FD>`, which took an unvalidated fd number: a number naming
some other inherited fd wrote the record into whatever that was and then closed
it, while the launcher waited forever on a pipe that got neither a line nor an
EOF. Stdout cannot be misaddressed.

```sh
mogwai serve --config run.toml
```

`--config PATH` is optional and otherwise uses built-in defaults. It never
consults the working directory. `--duration DURATION` overrides
`run_duration_ns` for this invocation, and `--duration 0s` means what
`run_duration_ns = 0` means - NO declared completion, run until the launcher
ends it. It briefly meant the opposite here, producing a venue that announced
readiness and exited before anyone could connect. There is no `--seed` flag: a
reproduced path is a written-down act, so the seed is overridden through the
config file's `seed` key alone; when absent, one is drawn at launch and
reported back in the readiness record's `run_seed`, the value that with the
config, the fingerprint and `version_string` reproduces every served path; the
requested symbol label is the fifth input that selects one path from the run.

`mogwai --version` prints semver, build hash, build time and the tape
protocol version on one
line; the same string is what the readiness record reports as
`version_string`, so an operator can tell whether two runs' tapes are even
comparable before comparing seeds.

A Rust consumer should not implement any of what follows. `mogwai_protocol::launch`
(re-exported as `mogwai_adapter::launch`) ships the launcher: `launch(LaunchSpec)`
returns a `LaunchedVenue` guard that owns the child, exposes `addr()` /
`base_url()` / `record()`, and kills and reaps on drop. It spawns from a
dedicated OS thread that it parks for the run, drains stderr from the moment of
spawn, bounds the readiness read, and checks the schema version before any other
field is read - which is the whole of the contract below. mogwai's own lifecycle
gates drive the venue through it, so it cannot drift from the binary it launches.
The prose remains because a launcher in another language still has to implement
it.

Three details of that guard are worth stating, because a caller writes code
against each and cannot see any of them from the signature. `shutdown()` reports
a venue that would NOT die - a failed signal or a failed reap comes back as
`LaunchError::Teardown`, so the "shut it down and report a failure to do so"
check callers write is real, and a caller that ignores it may launch a
replacement into an address the old venue still holds. `exited()` records the
child's status whenever the guard reaps one, including on the shutdown path, so
a bounded run that completed just before teardown reports its successful exit
rather than `None`; a `None` from a guard that has been shut down means the venue
was killed while still serving. And the readiness bound holds even against a
child that misbehaves: on expiry the launcher reports the timeout without waiting
on its own reader thread, so a `LaunchSpec::binary` naming a wrapper script that
starts the venue without `exec` - leaving a grandchild holding the inherited
stdout - costs a leaked thread rather than a launcher hung immediately after
deciding to report a timeout. Point it at the binary, or at a wrapper that
`exec`s it, all the same: only then does killing the child close stdout, and only
then is the readiness read released by the kill rather than by the timeout.

The venue otherwise inherits the launcher's environment. `LaunchSpec::env` sets
variables on top of it, and the one that usually wants setting is `RUST_LOG`: the
venue's `mogwai=info` default applies only when `RUST_LOG` is unset, so a caller
that READS the venue's log has its filter chosen by whatever the surrounding
process exported. Pin it rather than inherit it.

The launcher starts `mogwai serve` as its direct child with stdout captured,
reads exactly one JSON `ReadyRecord` line, checks `version`, and uses `addr` for
both clients. It takes any required river name from its own configuration. That
read blocks for as long as warmup generation takes, which is
proportional to `warmup_ns` and the tape's cadence; a launcher wanting a bound
sets its own timeout and treats expiry as a boot failure. Stdout closing without
a line is a boot failure, and the child's stderr and exit status say why. It keeps the child as a direct child: Linux parent-death
handling terminates the venue if that launcher dies. On a `RunComplete` frame,
the process exits successfully; otherwise the launcher terminates and reaps it.

Two properties of that parent-death handling decide whether a launcher is
written correctly, and neither is guessable from the outside.

`PR_SET_PDEATHSIG` fires on the death of the parent THREAD, not the parent
process. A launcher that spawns the venue from a worker thread and then lets
that thread finish gets its venue terminated mid-run while the launcher itself
is perfectly healthy. Spawn from a thread that outlives the run - the main
thread, or a pool thread deliberately parked for the run's duration. At fleet
scale, spawning from a short-lived pool task is the natural thing to write and
the wrong thing to write.

The venue refuses to start if its launcher is already gone. `--launcher-pid PID`
is how it knows: told the launcher's own pid, it checks it still HAS that parent
before serving, and exits nonzero otherwise. The shipped launcher always passes
it; a launcher in another language should too. Without it the venue can only
notice a launcher that dies DURING its startup - comparing its parent before and
after arming the signal - which is blind to one already gone before the first
instruction ran, and that is the case a launcher that spawns and exits produces
every time.

Capturing stdout is the second guard and the reason the contract says to capture
rather than merely suggesting it. The readiness line is written to a pipe whose
read end died with the launcher, so the write fails and the venue exits - later
than the pid check, since it comes after warmup, but before it has served
anything. A launcher that neither passes its pid nor captures stdout has neither
guard, and will leave a venue serving under init for its whole declared duration.

A launcher sees either refusal the same way as any other boot failure - stdout
closes with no line - so no special handling is needed for them.

## Which run is at an address

`GET /health` reports `run_seed`, identifying the RUN rather than the process.

A port identifies nothing over time. It is ephemeral, and this venue frees it
BEFORE it exits: a declared completion stops the accept loop first, then drains
live connections for up to the shutdown grace, so the address is available while
the process is still alive. A consumer watching for child exit sees nothing
during that window, and a client that only knows where to dial cannot tell its
own run from whatever answers there next.

A venue whose live connections do NOT drain within that grace exits NONZERO. It
used to log a warning and exit 0, which made an abandoned connection
indistinguishable from a clean teardown to a launcher inspecting exit status. A
client that holds `/ws` open past the venue's completion is what produces it, so
a consumer that wants a clean exit closes its sockets when it sees `RunComplete`
rather than waiting to be dropped.

The readiness record already carries `run_seed`, so a launcher can bind its
clients to the run it started rather than to the address it landed on. The
nautilus adapter does this through `MogwaiDataClientConfig::for_run` /
`MogwaiExecClientConfig::for_run`, which check `/health` on every connect and
refuse - terminally, logging `venue identity mismatch` - when the address is
serving a different run.

BIND TO THE RUN, NOT THE ADDRESS. This is a usage contract rather than a knob to
tune, because the identity is always available: `addr` and `run_seed` arrive in
the SAME readiness record, on the same line. So there is no shape of deployment
that can know where to dial and not know what it is dialling.

- If you LAUNCH the venue, you parsed that record to get the address. Build the
  client config with `for_run`, not `for_addr`.
- If you hand ONE venue's address to several clients, hand them the `run_seed`
  with it. You are already distributing per-client configuration - the account
  id each one must name - and the seed rides the same path.

`for_addr` is the lossy path: it takes an address alone and sets no expected
seed, so the client cannot tell its venue from whatever answers there next. It
is not a client that COULDN'T check, it is a config that dropped the identity one
call earlier, so a client built that way logs a warning naming the fix.

What the contract buys is worth stating plainly, because the failure is silent
rather than loud. A recycled port is most likely held by ANOTHER MOGWAI VENUE -
they all bind ephemeral loopback ports - and a sibling venue speaks the wire
perfectly: it accepts the subscription, serves a tape, takes orders. The run then
completes green against the wrong river, and the seed recorded against its result
is not the seed that produced it. Checking the run turns that into a refusal at
connect.

## Whether a run is worth keeping

`GET /health` carries an optional `fault` object, present when a SEATED
RIVER's tape has faulted. It names the river in `symbol`, the refusal in
`kind`, and the simulated instant in `clock_ns`.

A run seats a boat per river and every boat owns its own tape, so rivers fault
independently. The field reports the faulted river with the smallest symbol,
which makes it stable across polls of the same run and still answers the
question a fleet poller has - is ANY river faulted. It does not enumerate a
second simultaneous fault, because one faulted river already condemns the run:
a poller that sees this field at all should discard the run rather than trust
its output. A run is fire-and-forget, so discarding one is cheap and
reproducing it means a fresh instance with the same seed and config.

Absence of the field is not a promise that the venue is healthy in every other
sense - it reports tape faults, not consumer-side backlog - and a faulted run
may also die on its own terminal-fault path, which is separate. What the field
buys is seeing the fault BEFORE that.

Two outcomes are deliberately NOT a mismatch, and they are reported as different
things because they are different things. A probe that gets no usable answer -
the request failed, or returned an error status, or returned something that is
not JSON - is a transport failure, indistinguishable from the socket failing the
same way, so the connection proceeds. A probe that IS answered by a venue
reporting no `run_seed` is version skew: nothing failed, the venue simply
predates run identity, and the log says so rather than blaming the network.

A launcher that CAPTURES the child's stderr must also DRAIN it, continuously,
from the moment of spawn. Logs go to stderr by design, a pipe holds roughly
64 KiB, and a full pipe blocks the writer - so an undrained capture wedges the
venue mid-run, which is indistinguishable from a hung venue at the socket. A
launcher that does not want the log should redirect it to a file or to the null
device rather than to a pipe it never reads. `scripts/smoke.py` drains it on a
thread, which is the reference form.

`presets` prints the embedded instrument presets. Bare, it lists the names;
with one - `mogwai presets MNQ` - it prints that preset's TOML, including the
`[provenance]` map declaring where every knob came from. The presets ship
inside the binary, so a fresh clone needs no data directory, and the provenance
is what makes the asymmetry visible: the crypto cadence knobs are fitted
against trade-level archives, while the index-future cadence is derived from
bar counts and its clustering constants are declared with a rationale saying
they come from nowhere at all. Choosing a preset without reading that is
choosing a number you have not been told the standing of.

`gen` remains the offline generator command. `--symbol` (default `BTCUSDT`)
resolves against the built-in venue first and then against the embedded presets,
so
`mogwai gen --symbol MNQ --type bars --interval 1m --length 3d` charts the index
future rather than failing on an unknown symbol. `--config PATH` resolves the
instrument from an operator TOML instead, through the same load path a served
config takes; it is mutually exclusive with an explicit `--symbol`. A preset
carries its session
calendar into the dump, which is what makes a futures tape show its closed
weekend and its daily maintenance halt as zero-volume runs; before this the
offline path dropped the calendar and printed straight through both, so a chart
taken from it disagreed with the served tape it was supposed to illustrate.

`tick-composition` is the offline measurement the tape budget constants are
derived from. It walks all three presets across eight seeds and four arrival
configurations and writes ONE fixture, named by `--out` and stamped with the
live tape protocol version. Each preset is resolved the way `serve` resolves
it, so the futures are measured on their own size grid and session calendar. It
is a long run - about an hour at the default 2,000,000 parent events per
combination, nearly all of it the maximum-surge arm - and `--jobs` bounds its
concurrency; `--parents` shortens it for a smoke run, at which point the output
is no longer the shipped fixture. The destination is staged and renamed at the
end, and the document carries a pairing identifier naming the traversal that
produced it.

It emitted two files until protocol 8. Protocol 6 was a count PROJECTION of the
protocol-7 stream - quote placement draws no randomness, so one traversal
carried both - but that is specific to those two versions. A session profile
divides the duration draw and scales the return, so protocol 8's tape has
different timestamps and prices and cannot be projected from protocol 7. Version
pairs are compared by `mogwai tick-composition-ratios compare`, whose `--mode
projection` keeps the 6-to-7 contract and whose `--mode independent` compares
two separately measured tapes. It is a SEPARATE subcommand rather than a report
mode here: `tick-composition` measures a tape, and that command turns a
measurement into proposed constants, so fusing them would let one invocation
measure a fixture and bless it in the same breath.

From protocol 9 onward, the command consumes compact parent summaries from the
same stochastic kernel as the served wire path. The kernel still advances every
child draw and every path-dependent price and clock state, but the measurement
does not construct symbols or decimal payloads it never reads. Fixed-stride
child timestamps are added to per-second bins as short runs. The wire path and
the compact path are continuation-tested from identical initial states.

`man` renders the bundled reference docs. Bare, it lists the topics; with one -
`mogwai man cli` - it renders that document to the terminal, colour dropped when
stdout is not a TTY or `NO_COLOR` is set. The topics are `cli`, `config`,
`architecture`, `havoc`, `clock`, `presets` and `oms-types`. The docs travel
inside the binary, so an installed `mogwai` documents itself with no source tree
present. The durable documents only: transient working notes are not bundled.

There is no `stop` subcommand.

## The offline evidence toolbox

The remaining subcommands are the 2026-08 Python-to-Rust rewrite's absorbed
half of `analysis/`: the corpus-to-fingerprint method library
(`mogwai_lab`), reached the same way `gen`/`tick-composition` are - offline,
no socket bound. Each writes an ARTIFACT (the storage policy's term: the
user's own file, written to `--out` or a working-directory default, never
cached, never auto-deleted) and every default path is chosen so a bare
invocation can never overwrite a committed `analysis/` file by accident.

`characterize` is the intake station: it streams a trade corpus into
`char_<PAIR>.json` stylized-fact reports, which is what `synth fingerprint`
reads and the first step of onboarding any instrument. With no arguments it
sweeps a representative pair set concurrently and prints the cross-pair table
used to eyeball whether the stylized facts repeat across instruments; name
pairs to narrow it. `--data-dir` defaults to `MOGWAI_DATA_DIR`, `--out-dir` to
`analysis/`, and `--jobs` caps concurrency. A bare symbol resolves to
`<DATA_DIR>/<SYMBOL>.csv`; anything path-shaped is taken as given. Missing or
empty inputs are skipped by name rather than failing the sweep.

`session-profile preflight` and `session-profile fit` are the
calendar-conditional session fit over a one-minute bar archive. The preflight
is a GATE, not a summary: it answers whether the archive carries zero-volume
rows, which decides whether exposure may come from row presence or must come
from the calendar - deriving it from row presence would shrink each quiet
hour's denominator in proportion to its own quietness and compress the
peak-to-trough ratio the fit exists to measure. Run it before trusting any
fit. `--preset` selects the session calendar the fit is conditional on, which
is an INPUT to the estimator rather than a consumer of it; `--alignment`
chooses between reading historical civil labels against the preset's fixed
offset (`civil`, the default, so CST and CDT land on the same session phase)
and reading them as instants.

`segments` is the session-segment sampler: it builds a tape out of REAL
session slices instead of synthesizing one. Two halves, both offline.

`segments cut` carves one session window out of a delivered TBBO month into a
segment library:

```text
brokkr run mogwai -- segments cut --symbol MNQ --month 2026-04 --window asia --out analysis/out/asia-mnq-2026-04.json
```

`--window` is one of four, each stated in EXCHANGE-LOCAL time and anchored on
the trade date's own 17:00 reopen, so all four are DST-correct without a second
calendar:

| window | exchange-local | New York |
|---|---|---|
| `asia` | 17:00 to 02:00 | 18:00 to 03:00 |
| `london` | 02:00 to 08:00 | 03:00 to 09:00 |
| `ny-morning` | 08:00 to 11:00 | 09:00 to 12:00, cash open with a lead-in, to lunch |
| `ny-afternoon` | 09:30 to 15:00 | 10:30 to 16:00, to the cash close |

A window overlapping the 15:15 trading halt is REFUSED rather than cut: it
would carry the halt's fifteen-minute hole invisibly into every loop of the
composed tape. The corpus directory is
resolved under `--root` from the conventional
`<symbol>v/<month>.<state>.tbbo` layout, or named outright with `--dir`. The
library holds NO absolute prices: every segment is a sequence of log returns
against its own previous trade, plus one measured `open_gap_ret` recording the
jump from the last print before the window to the first print inside it. Stderr
reports the work size - segments cut, ticks in them, and how many carried a
measured open gap.

A holiday half-session is dropped, by name, on stderr. `--min-ticks-fraction`
is the rule: a session carrying less than that fraction of the month's MEDIAN
session is a stub rather than a session, and sampling would otherwise draw it
as often as a full day. The 2026-04-03 `ny-morning` slice is the worked
example - Good Friday, 4,408 ticks against a 400,000-tick typical day, non-empty
and so invisible to every other rule. Pass 0 to keep every non-empty slice.

`segments tape` composes that library into an endless single-session tape and
dumps it as CSV:

```text
brokkr run mogwai -- segments tape --library analysis/out/asia-mnq-2026-04.json --type bars --interval-s 60 --ticks 3000000 --out analysis/out/asia-endless.csv
```

Because the library is in returns space, composing is integration: the tape
carries a running price each stored return multiplies, so a segment boundary
needs no level reconciliation and any slice can follow any other. `--start-price`
is therefore an integration constant that scales the whole tape and changes no
return. `--seed` fixes the draw order, `--in-order` cycles the library instead
of sampling it with replacement, and `--no-reopen-gaps` suppresses the measured
gap at each seam - which is the A/B against the fitted generator, since that
generator produces no reopen gaps at all. The source is endless, so a dump is
bounded by `--ticks` rather than by exhaustion.

The bars output carries the same header `gen --type bars` emits, so one tool
charts either:

```text
python3 analysis/plot_tape.py --csv analysis/out/asia-endless.csv --out analysis/out/asia-endless.html
```

`preflight` runs the fail-closed TBBO corpus contract check against a
delivered corpus directory and writes a hash-bound preflight artifact -
`--corpus`, `--ledger` (read-only) and `--out` all default to the paths
`analysis/mnq_fit.py` used. This command is the frozen July parity path.

For a Stage M new-design month, produce its calendar-bound inventory and then
run Tier 1a with the matching arguments:

```text
brokkr run mogwai -- stage-m preflight --month 202509 --corpus research/market-data/databento/mnqv/2025-09.manifest.tbbo --ledger analysis/databento-jobs.json --ledger-key 'mnqv|2025-09.manifest|tbbo'
brokkr run mogwai -- stage-m month --month 202509 --corpus research/market-data/databento/mnqv/2025-09.manifest.tbbo --ledger analysis/databento-jobs.json --ledger-key 'mnqv|2025-09.manifest|tbbo'
```

Both commands default to `analysis/out/stage-m/<YYYYMM>/preflight.json` and
the preflight defaults to `analysis/databento-calendar.json`. Full-session
dates are candidates, half-session dates are recorded as
`early_close_excluded`, and closures are absent from the inventory. The
artifact records the usable count and calendar hash; it does not apply the
July-only 18-session floor. `measure` runs the protocol-12a section-10
measurement gate: the live observed pass over the corpus plus the eight
in-process attestation walks, assembled and validated into the artifact
shape `analysis/mnq-measure-12a.json` names, and refuses outright over a
dirty working tree because the artifact's `binding.harness_tree_commit` must
name exactly the code that ran. `fit` runs the protocol-11 session
calibration - the observed corpus pass, the closed-form session refits, the
CRN `vol_scalar` solve and the family probes - under the same clean-tree
binding, writing under `target/` by default rather than over the committed
`analysis/mnq-fit.json`.

`synth fingerprint` and `synth cadence` are the fingerprint and cadence
synthesis paths (`analysis/build_fingerprint.py`/`build_cadence.py`):
`fingerprint` reads `char_<PAIR>.json` reports plus a cadence measurement,
`cadence` streams raw Binance trade archives. Neither writes into
`analysis/` unless `--out` names a path there explicitly - the bare default
is a `target/mogwai-synth/` scratch path. `cadence-feasible` reads a cadence
measurement and prints the `check_cadence_feasible.py` L0
structural-proceed verdict (PROCEED/CLOSE/STOP AND ASK) read off its
`children_mean`/`children_single_frac` anchors, exiting nonzero on anything
but PROCEED. It then re-simulates the arrival clock over `--events`
(3,000,000 by default, matching the Python) and exits nonzero when the
realized per-second density misses the feasibility bands - that
simulation is a GATE, not a diagnostic, and skipping it would let this
subcommand exit 0 where the script exits 1. `--skip-density` stops after
the structural verdict for callers who only want the L0 reading; the
Python has no such flag, so leaving it off is the matching behaviour.
`--fingerprint` names the session profile the arrival clock consults.

The simulation reproduces CPython's stream draw for draw, so its output is
identical to `python3 analysis/check_cadence_feasible.py` field for field
rather than merely close - bit for bit, including `gap_cv2`, which requires
computing the population variance as the exact rational CPython's
`statistics.pvariance` uses rather than in floating point. That identity is
pinned at the default 3,000,000 events, at `--events 14`, and over a
generated 820-case sweep against CPython. Not yet ported: the `--fit` and
`--fit-markov` grid searches, which are candidate-search tools rather than
gates.

`cache` is the manual-case cover for the storage policy's CACHE class:
`mogwai cache stats` reports entry/file/byte counts under the cache root
(`$XDG_CACHE_HOME/mogwai/`, `~/.cache/mogwai/`, `MOGWAI_CACHE_DIR` or
`--cache-dir`), `mogwai cache clean` removes every provenance directory, and
`mogwai cache clean --stale` removes only the ones that do not match the
CURRENT provenance token - the same pruning a cache write already does
automatically, exposed for manual use.

### `mogwai arrival-control`

Runs protocol 12b brick N's deterministic hourly re-centring negative control.
It checks the per-symbol pre-landing legacy tapes (three since the 2026-08-09
preset retirement) and the standing build gate's
transcript before walking fit seeds 301 through 304 and test seeds 305 through
308. The command reads `analysis/mnq-measure-12a.json` and
`analysis/mnq-minute-range-envelope.json`, and writes the hash-bound result to
`analysis/mnq-arrival-control.json`. It refuses a dirty tree and requires the
per-symbol parent-build baselines under
`analysis/out/arrival-control-b1-baseline/` (three since the 2026-08-09
preset retirement; the committed brick N artifact recorded five).

Gate B5 is EVIDENCE THIS COMMAND READS, never a check it runs. Run
`brokkr check --gate --json` yourself first and capture its output to
`analysis/out/arrival-control-b5-gate.log` (or pass `--b5-log`); the command
records that transcript's digest and the machine-readable summary on its last
line, and passes B5 only on `verdict: complete`. A transcript with no summary
line - one from a run that died partway, or one captured without `--json` - is
refused rather than read as a pass. The
venue binary never invokes the build tool: a clone without it could not produce
an artifact at all, and because everything here runs through `brokkr run`, a
spawned gate would block forever on the workspace lock its own parent holds. A
workspace textlint rule forbids the tool's name in Rust source so this cannot
come back.

Alongside the per-symbol byte comparisons, and never in place of them, it
reads the diff from the baseline commit (`--b1-baseline-commit`, default
`HEAD~1`) to HEAD and records whether it touched any tape-bearing area - the
data crate, the protocol crate, the shipped preset bundles, or
`analysis/fingerprint.json`; touching any of them, or a tape protocol version
that has MOVED since the baseline commit, fails the tape-identity gate. The
accepted identity is not written down anywhere: the check reads
`TAPE_PROTOCOL_VERSION` out of the baseline commit itself and compares it
against the running binary's, so what it asserts is that no bump landed in the
range the baseline tapes came from. It used to name a literal, which went three
bumps stale and made the gate unpassable. Test-only files inside those paths
are reported separately and do not fail it, since a `cfg(test)` module
contributes no byte to a shipped tape.

The artifact records a verdict of `negative-control-passed` or
`negative-control-failed` together with the failing gate names, the per-hour
ratios each of the four fit seeds contributed, the corrected curve and the
normalizer drift the correction leaves behind. Like `measure12a`, it re-reads
git immediately before the atomic write, so a HEAD move or an edit during the
several minutes of walking unbinds the artifact rather than being recorded as
a clean run. The corrected curve reaches the generator through an in-memory
scratch override and is written to no preset; no tape byte moves.

### `mogwai arrival-screen`

Runs protocol 12b's Stage A necessary-condition screen: a corpus-free,
no-running-generator-change grid walk over the four candidate arrival
families (event-time Markov renewal, wall-time MMPP, log-OU Cox and
discrete self-exciting) that advances every admissible family-region pair
and selects none. It reads `analysis/mnq-measure-12a.json` (`--measure`)
for the observed parent-count marginal and the exposure binding, and
writes the hash-bound result to `analysis/mnq-arrival-screen.json`
(`--out`). `--cache` points at the walk cache root, defaulting to the
standing storage policy. `--jobs N` bounds concurrent `(cell, seed)`
projection workers and defaults to the machine's reported parallelism,
capped at 16 workers to avoid the measured SMT-contention regression.
Verdict reduction and budget enforcement stay on the coordinator, and
the final artifact order is independent of worker completion order.

`--cost-probe` runs brick A0: one grid cell per family, at two seeds,
measured against that family's own wall-time and peak-RSS budget. It
writes no artifact and requires no clean tree, since its whole purpose is
to be run before the grid that produces one. A per-cell budget miss fails
the command and stops for an owner ruling on the per-cell price; the full
run additionally enforces a total wall-time and peak-RSS ceiling across
the whole grid, stopping without writing an artifact if either is
crossed. The cost probe remains serial because it prices each cell rather
than scheduler throughput.

A full run requires a clean tree before reading any input and re-attests
it immediately before serializing, exactly as `arrival-control` does. The
command lands no generator change and moves no tape byte: the run records
whatever `TAPE_PROTOCOL_VERSION` reads at run time in the artifact's binding
block, so the artifact states the identity rather than this page doing it.

### The remaining offline commands

These are evidence tools with narrower audiences. Each writes an artifact and
documents its own arguments under `--help`.

- `count-curve` - the signed count-curve preregistration's generated-only
  Stage 0 backcheck.
- `stage-m` - Tier 1 per-month measurement and its ladder: `preflight`,
  `month`, `backcheck`, `reverify-amendment2`, `schedule-equivalence`,
  `promote-july`, `exchangeability`, `power`, `summarize` and `tier2`.
- `minute-range-envelope` - builds the bound minute-range-envelope artifact
  `arrival-control` reads.
- `arrival-envelope-diagnostic` - evaluates coarse envelopes the official
  screen skipped, without changing that screen's verdict.
- `select-windows` - bar-frame intake: which tick-data windows to buy,
  stratified on what a one-minute bar can measure.
- `tick-composition-ratios` - turns composition fixtures into the four
  proposed sized constants, with `compare` for version pairs.
