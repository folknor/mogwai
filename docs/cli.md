# mogwai command line

`mogwai` runs one foreground venue for one run. It owns no PID, log, or
configuration files. Logs go to stderr; `RUST_LOG` selects the tracing filter.

`serve` binds the HTTP and WebSocket endpoint on `127.0.0.1` and an ephemeral
port. Neither half is configurable: there is no `--addr`, no way to name a port,
and no way to serve another interface. The kernel allocates the port, so two
concurrent runs on one machine cannot collide on a shared default and cannot be
made to by an operator hand-assigning ports; and loopback is the only interface
because the venue models latency on the sim axis and runs on the same machine as
its consumer.

The endpoint therefore has to be learned rather than assumed. On boot the venue
writes a single line of JSON to `stdout` - the version 8 `ReadyRecord`, carrying
`version`, `addr`, `pid`, `run_seed`, `data_origin_ns`, `run_start_ns`,
`run_duration_ns`, `warmup_ns`, `reset_account_on_reconnect`, `account_ttl_ms`
and `version_string` - and in practice that is the only thing it ever writes
there, though the shipped launcher does not require it: it drains stdout for the
whole run and discards everything past the record, so a stray write is ignored
rather than taking `EPIPE` mid-run. The last
two are the account-persistence policy: whether a returning consumer gets its own
ledger back, and how long an unattended one survives before the venue collects
it. It reports no symbol. Logs go to stderr, so
the two never interleave. A launcher captures stdout and reads a line; a human
sees the same line in the terminal.

The WebSocket endpoint is `GET /ws?symbol=<symbol>`. The query parameter is
optional; omitting it binds the socket to the run's default symbol for compatibility
with older consumers. A socket owns exactly one river. A supplied symbol is 1 to
32 ASCII letters, digits, dot, dash, or underscore, and matching is case exact.
Malformed symbols are refused with HTTP 400 before the upgrade. Every legal
symbol is resolved; the only other pre-upgrade refusals are a shape this run
cannot fund or make valid, and an account already riding that river at a
different speed. The upgrade also accepts `?speed=` (absent means the configured
`speed`) and `?duration_ms=`. The first passenger at a given speed places that
cursor; later passengers at the same speed share it; a different speed places a
second cursor on the same water.
A consumer names that river from its own configuration; the readiness record does
not supply one.

`GET /instruments` reports the configured shapes plus every river materialized
so far. A socket bind or history poll materializes a river and grows the list.
A run retains at most 256 materialized rivers and never evicts them. This is an
operational bound for trusted consumers belonging to the run's owner, not a
hostile-consumer defence.

A consumer reads its history over its own socket, never over these routes. Send
`QueryHistory` with a `request_id` and a `kind` of `Trades` or `Quotes`; the
venue answers a `HistoryPage` carrying that `request_id`, one bounded page of
rows, the session's `cutoff`, and a `continuation` to hand back for the next
page until `complete` is true. A refusal comes back as a correlated
`HistoryRejected` rather than as an empty page, because a consumer cannot tell
an empty page from a quiet market.

The request names no symbol, and that is the point: your connection already
resolved one river at upgrade, so a request carried on it cannot name the wrong
one. Once generator havoc entered river identity a label names several rivers,
and a poll naming a symbol names none of them - a passenger reading surged water
would backfill the clean river's prints and fold bars from a market it is not in.

To splice a backfill onto live: start buffering the live frames your socket is
already receiving before you send the first `QueryHistory` - a frame can arrive
between your decision to backfill and the command reaching the venue - then,
once the session completes, drop buffered rows of that kind at or below `cutoff`
and keep the rest. That admits overlap and forbids gaps, which is the right way
round: a river never prints two rows of one kind at one instant, so an overlap is
removable and a gap is not.

The `cutoff` is fixed at the first page and every later page of the session
carries the same one, so pagination cannot chase a moving present. The
`continuation` is opaque - hand it back unread. It is the venue's own
bookkeeping, and treating it as anything else is relying on something the venue
has not promised.

`GET /operator/trades` and `GET /operator/quotes` are the operator's view, and
the path says so because prose cannot: a route that kept its old spelling would
have gone on answering a consumer plausibly while its meaning changed
underneath. They both require
`symbol`; `start`, `end` and `limit` are optional. They are bounded by the run
clock alone, taken as one snapshot when the request is admitted and consulting no boat.
An omitted end is clamped to it and so is a stated one - an explicit `end` is a
bound on the window, never permission to cross the run present - and a `start`
above it, or below the tape origin, is refused with HTTP 400. Read `GET /clock`
once and pass its `venue_now_ns` as `end` on every page of a paginated window, or
the window grows as you read it.

Never warm a passenger from them. They serve the unarmed river of a label, on
the run clock, and three things follow that a consumer must not inherit: a
passenger carrying a generator arm is on different water entirely; a passenger on
a slow boat is behind the run clock, so these routes can hand it rows that are
still its future; and a passenger under `GoDark`, `StallData` or a ring overrun
has an observed market that deliberately differs from the clean river, which an
HTTP fetch would silently repair.

On an unpaced run (`speed = 0.0`) the tape outruns the run clock, so history can
trail what your socket has already been delivered live. That is deliberate: a
boat-free bound and a delivery-shaped bound cannot both exist, and refusing to
serve past the run present is what keeps a strategy from reading its own
future.

A history poll materializes the named river, so the only refusals left are
about the shape rather than about the symbol being unknown: a label that is not
a legal symbol, a shape whose settlement currency this run does not fund, a
shape the resolved configuration makes invalid, and an exhausted river cap.
Each is a 400 naming its reason.

A `/ws` upgrade that cannot be served splits by whose fault it is, and the
status is the machine-readable half of that answer. A 400 means changing the
request could make it work: a malformed symbol, an unfunded settlement currency,
an invalid shape, an exhausted river cap. A 503 means the venue could not
produce water its own configuration had already validated. Nothing about the
request will fix that, so a consumer meeting one must stop rather than retry:
the run has latched a terminal fault, `GET /health` reports it with
`kind: "materialize"`, and recovery means a new run. Every upgrade that had
joined the same placement receives that same answer rather than each re-running
the failure.

`GET /clock` takes no parameters and always answers on the run's clock:
`venue_now_ns` is the affine map read at the wall, and `data_origin_ns` and
`warmup_ns` are venue facts identical for every river. It took `symbol` and
`speed` and answered on a named river's boat, and both are now refused with a
400 rather than ignored - a route that cannot tell who is asking must not report
whether a boat exists, what cadence it runs, or how far it has delivered, since
none of that is knowledge a caller has about its own connection. Your own
delivery instant arrives stamped on the frames your socket receives.

`GET /account` returns one account's ledger. `?account=` names it and an
omitted one resolves the run's default account, which is the same resolution a
socket does, so a consumer that named no account on either surface sees one
ledger. An id nobody has traded under is answered rather than refused, with the
opening balances a ledger under that id would carry; asking does not open the
account. Its `ts_event` is venue time and
the top-level `clock` field is `"venue"`. Pushed account events are stamped on
their boat clocks, so consumers order pulls against pushes by protocol
sequence, never by comparing timestamps across those axes. Opening an account
on your own terms, the freeze and the risk policies are in `docs/accounts.md`.

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
`run_duration_ns = 0` means - no declared completion, run until the launcher
ends it. It briefly meant the opposite here, producing a venue that announced
readiness and exited before anyone could connect.

`--symbol SYMBOL` is the transient launcher's known bind symbol. It makes that
label the unnamed socket's default and resolves its funding before readiness,
so a one-consumer run cannot boot successfully and fail its first bind over a
missing settlement currency. A shared `mogwai serve` leaves it unset and keeps
the open instrument-set policy described above.

`--duration` is humantime, and it is the only duration on this binary that is.
It reads `0s`, `1500ms` and `1ns`, which is what lets the shipped launcher
render any `Duration` it holds. `gen`'s `--length`, `--interval` and
`--burn-in` take an in-house grammar instead: a positive count and one of
`s m h d w mo y`. It refuses a zero count, so `--length 0d` is an error rather
than the endless run `--duration 0s` asks for, and it has `mo` and no `ms`, so
`--length 1500ms` is refused for an unknown unit rather than read as
milliseconds. `30m` is the same half hour to both. The two grammars are
deliberate rather than an oversight, and
`crates/mogwai-cli/src/main.rs`'s `serve_argv_parses_in_the_venues_own_grammar`
and `the_in_house_gen_grammar_cannot_read_what_the_launcher_renders` pin the
split from both sides. There is no `--seed` flag: a
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

Four details of that guard are worth stating, because a caller writes code
against each and cannot see any of them from the signature. Dropping it blocks -
it signals the owning thread, which kills and reaps the child, and joins it - but
the cost is a signal round trip, measured at a few hundred microseconds against a
healthy venue, never the 200 ms interval at which the owner polls for a venue that
ended on its own. Those are different clocks: the shutdown channel disconnects
when the guard drops and the owner wakes on that at once, so no teardown ever
waits out a poll window. Sub-millisecond is short enough to drop on an async
worker; what no launcher can bound is a venue that refuses `SIGKILL`, and a
caller for whom that matters drops the guard off its reactor. `shutdown()` reports
a venue that would not die - a failed signal or a failed reap comes back as
`LaunchError::Teardown`, so the "shut it down and report a failure to do so"
check callers write is real, and a caller that ignores it may launch a
replacement into an address the old venue still holds. `exited()` records the
child's status whenever the guard reaps one, including on the shutdown path, so
a bounded run that completed just before teardown reports its successful exit
rather than `None`; a `None` from a guard that has been shut down means the venue
was killed while still serving. And the readiness bound holds even against a
child that misbehaves: the launcher places its child in a new process group and
signals the group on expiry, which collects helpers the venue spawns and wrapper
grandchildren that remain in the inherited group. It cannot collect a
grandchild which deliberately leaves that group while retaining stdout. The
launcher still reports the timeout without joining its reader, so that narrower
case costs a leaked reader thread rather than a launcher hung after deciding to
report a timeout. The group has one consequence worth knowing: the venue is no
longer a member of the launcher's process group, so a terminal's Ctrl-C reaches
the launcher and not the venue. Teardown signals the group explicitly and
`PR_SET_PDEATHSIG` covers the launcher dying outright, so nothing is left
relying on that path.

The launcher's less visible bounds are deliberate too. Its owner loop checks
for child exit on both the shutdown and polling paths and records whether the
child was already reaped before it attempts teardown, so a naturally completed
run cannot be turned into a false teardown failure by a second kill. Readiness
is capped at the child stream before buffering, so an overlong first line
cannot make the parser's refusal merely post-hoc. Captured stderr keeps its
head and tail under one fixed bound, inserting one elision marker when the
middle is first discarded; the boot error at the head therefore survives a
long backtrace. Timeout and launcher-thread failure both flow through the same
unconditional kill and reap before `launch` returns an error.

`--launcher-pid` is parsed as an `i32` because that is the operating system pid
type, while the shipped launcher can only render its own non-negative process
id. A manually supplied zero or negative value is compared with `getppid()` and
refused before any signal target is used, so it cannot acquire the special
process-group meaning a negative argument would have to `kill`.

The venue otherwise inherits the launcher's environment. `LaunchSpec::env` sets
variables on top of it, and the one that usually wants setting is `RUST_LOG`: the
venue's `mogwai=info` default applies only when `RUST_LOG` is unset, so a caller
that reads the venue's log has its filter chosen by whatever the surrounding
process exported. Pin it rather than inherit it.

The launcher starts `mogwai serve` as its direct child with stdout captured,
reads exactly one JSON `ReadyRecord` line, checks `version`, and uses `addr` for
both clients. It takes any required river name from its own configuration. That
read blocks for as long as warmup generation takes, which is
proportional to `warmup_ns` and the river's cadence; a launcher wanting a bound
sets its own timeout and treats expiry as a boot failure. Stdout closing without
a line is a boot failure, and the child's stderr and exit status say why. It keeps the child as a direct child: Linux parent-death
handling terminates the venue if that launcher dies. On a `RunComplete` frame,
the process exits successfully; otherwise the launcher terminates and reaps it.

Two properties of that parent-death handling decide whether a launcher is
written correctly, and neither is guessable from the outside.

`PR_SET_PDEATHSIG` fires on the death of the parent thread, not the parent
process. A launcher that spawns the venue from a worker thread and then lets
that thread finish gets its venue terminated mid-run while the launcher itself
is perfectly healthy. Spawn from a thread that outlives the run - the main
thread, or a pool thread deliberately parked for the run's duration. At fleet
scale, spawning from a short-lived pool task is the natural thing to write and
the wrong thing to write.

The venue refuses to start if its launcher is already gone. `--launcher-pid PID`
is how it knows: told the launcher's own pid, it checks it still has that parent
before serving, and exits nonzero otherwise. The shipped launcher always passes
it; a launcher in another language should too. Without it the venue can only
notice a launcher that dies during its startup - comparing its parent before and
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

`GET /health` reports `run_seed`, identifying the run rather than the process.
It says nothing about which rivers the run is carrying or at what cadence, and
it is the only route that answers without an identity, so it must not: a caller
that names no account is told whether the run is alive and faulted, and nothing
about anybody's boats.

The fill sweeper's progress is reported per account instead. The `GET /account`
body carries `sweep_passes`, one row per boat the named account is seated on,
sorted by `symbol` and carrying a monotonic `completed` count. The
count advances only after the whole pass over that boat finishes, so an operator
or a test can wait for engine work - a fill walk, a settlement, a funding charge
- rather than infer it from elapsed wall or simulated time. There is no cadence
field because one ledger carries one cadence per river, so the symbol already
names the seat. An account seated nowhere reports an empty list.

A port identifies nothing over time. It is ephemeral, and this venue frees it
before it exits: a declared completion stops the accept loop first, then drains
live passengers for up to the shutdown grace, so the address is available while
the process is still alive. A consumer watching for child exit sees nothing
during that window, and a consumer that only knows where to dial cannot tell its
own run from whatever answers there next.

That drain covers passengers, and saying so is not redundant: an
upgraded connection stops being an HTTP connection at the 101, so a venue that
waited only on its accept loop would consider itself drained while passengers were
still mid-frame - and it did, which is how a completed run's `RunComplete` and
its WS 1000 close went missing on a loaded host, leaving the peer with a reset
instead of an announcement. A declared completion now waits for the passengers
themselves, inside the same grace.

A signal does not, and that is the deliberate difference. A signal means the
launcher ended the run rather than the run completing, so no `RunComplete` is
published and nothing tells a passenger to end; waiting for one would idle out
the whole grace on any venue with a socket attached. A signalled venue takes its
sockets with it.

A venue whose live passengers do not drain within that grace exits nonzero. It
used to log a warning and exit 0, which made an abandoned connection
indistinguishable from a clean teardown to a launcher inspecting exit status. A
consumer that holds `/ws` open past the venue's completion is what produces it, so
a consumer that wants a clean exit closes its sockets when it sees `RunComplete`
rather than waiting to be dropped.

The readiness record already carries `run_seed`, so a launcher can bind its
consumers to the run it started rather than to the address it landed on. The
nautilus adapter does this through `MogwaiDataClientConfig::for_run` /
`MogwaiExecClientConfig::for_run`, which check `/health` on every connect and
refuse - terminally, logging `venue identity mismatch` - when the address is
serving a different run.

Bind to the run, never to the address. This is a usage contract rather than a knob to
tune, because the identity is always available: `addr` and `run_seed` arrive in
the same readiness record, on the same line. So there is no shape of deployment
that can know where to dial and not know what it is dialling.

- If you launch the venue, you parsed that record to get the address. Build the
  client config with `for_run`, not `for_addr`.
- If you hand one venue's address to several consumers, hand them the `run_seed`
  with it. You are already distributing per-consumer configuration - the account
  id each one must name - and the seed rides the same path.

`for_addr` is the lossy path: it takes an address alone and sets no expected
seed, so the consumer cannot tell its venue from whatever answers there next. It
is not a consumer that could not check, it is a config that dropped the identity one
call earlier, so a consumer built that way logs a warning naming the fix.

What the contract buys is worth stating plainly, because the failure is silent
rather than loud. A recycled port is most likely held by another mogwai venue -
they all bind ephemeral loopback ports - and a sibling venue speaks the wire
perfectly: it accepts the subscription, serves a tape, takes orders. The run then
completes green against the wrong river, and the seed recorded against its result
is not the seed that produced it. Checking the run turns that into a refusal at
connect.

## Whether a run is worth keeping

`GET /health` carries an optional `fault` object, present when a boated
river's tape has faulted. It names the river in `symbol`, the refusal in
`kind`, and the simulated instant in `clock_ns`.

The `status` field follows it, and no longer reads `"ok"` unconditionally: it
is `"ok"` while no fault is reported and `"faulted"` once one is. Those two
words are the whole vocabulary, so a poller that only reads `status` sees a
faulted run rather than the constant a burnt-out venue used to answer with.
Reading `fault` is still what tells you which river and why.

A run places a boat per river and every boat owns its own tape, so rivers fault
independently. The field reports the faulted river with the smallest symbol,
which makes it stable across polls of the same run and still answers the
question a fleet poller has - whether any river at all is faulted. It does not enumerate a
second simultaneous fault, because one faulted river already condemns the run:
a poller that sees this field at all should discard the run rather than trust
its output. A run is fire-and-forget, so discarding one is cheap and
reproducing it means a fresh instance with the same seed and config.

Absence of the field is not a promise that the venue is healthy in every other
sense - it reports tape faults, not a connection's delivery backlog - and a faulted run
may also die on its own terminal-fault path, which is separate. What the field
buys is seeing the fault before that.

Two outcomes are deliberately not a mismatch, and they are reported as different
things because they are different things. A probe that gets no usable answer -
the request failed, or returned an error status, or returned something that is
not JSON - is a transport failure, indistinguishable from the socket failing the
same way, so the connection proceeds. A probe that is answered by a venue
reporting no `run_seed` is version skew: nothing failed, the venue simply
predates run identity, and the log says so rather than blaming the network.

A launcher that captures the child's stderr must also drain it, continuously,
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
config takes; it is mutually exclusive with an explicit `--symbol`. Being the
served load path rather than an instrument reader has a consequence worth
knowing before you reach for it: the config is validated whole, so an offline
chart can be refused over account funding. A `[balances]` table that does not
carry the resolved instrument's settlement currency fails
`refuse_unfunded_settlement` and the command exits without generating
anything, exactly as `serve` would refuse to boot on it. Fund the currency, or
chart the instrument through `--symbol`, which resolves no config at all. A preset
carries its session
calendar into the dump, which is what makes a futures river show its closed
weekend and its daily maintenance halt as zero-volume runs; before this the
offline path dropped the calendar and printed straight through both, so a chart
taken from it disagreed with the served river it was supposed to illustrate.

`tick-composition` is the offline measurement the tape budget constants are
derived from. It walks all three presets across eight seeds and four arrival
configurations and writes exactly one fixture, named by `--out` and stamped with the
live tape protocol version. Each preset is resolved the way `serve` resolves
it, so the futures are measured on their own size grid and session calendar. It
is a long run - about an hour at the default 2,000,000 parent events per
combination, nearly all of it the maximum-surge arm - and `--jobs` bounds its
concurrency; `--parents` shortens it for a smoke run, at which point the output
is no longer the shipped fixture. The destination is staged and renamed at the
end, and the document carries a pairing identifier naming the traversal that
produced it.

It emitted two files until protocol 8. Protocol 6 was a count projection of the
protocol-7 stream - quote placement draws no randomness, so one traversal
carried both - but that is specific to those two versions. A session profile
divides the duration draw and scales the return, so protocol 8's tape has
different timestamps and prices and cannot be projected from protocol 7. Version
pairs are compared by `mogwai tick-composition-ratios compare`, whose `--mode
projection` keeps the 6-to-7 contract and whose `--mode independent` compares
two separately measured tapes. It is a separate subcommand rather than a report
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
`architecture`, `havoc`, `clock`, `presets`, `oms-types`, `order-lists` and
`adapter-lifecycle`. The docs travel
inside the binary, so an installed `mogwai` documents itself with no source tree
present. The durable documents only: transient working notes are not bundled.

There is no `stop` subcommand.

## The offline evidence toolbox

The remaining subcommands are the 2026-08 Python-to-Rust rewrite's absorbed
half of `analysis/`: the corpus-to-fingerprint method library
(`mogwai_lab`), reached the same way `gen`/`tick-composition` are - offline,
no socket bound. Each writes an artifact (the storage policy's term: the
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
is a gate, not a summary: it answers whether the archive carries zero-volume
rows, which decides whether exposure may come from row presence or must come
from the calendar - deriving it from row presence would shrink each quiet
hour's denominator in proportion to its own quietness and compress the
peak-to-trough ratio the fit exists to measure. Run it before trusting any
fit. `--preset` selects the session calendar the fit is conditional on, which
is an input to the estimator rather than a consumer of it; `--alignment`
chooses between reading historical civil labels against the preset's fixed
offset (`civil`, the default, so CST and CDT land on the same session phase)
and reading them as instants.

`segments` is the session-segment sampler: it builds a river out of real
session slices instead of synthesizing one. Two halves, both offline.

`segments cut` carves one session window out of a delivered TBBO month into a
segment library:

```text
brokkr run mogwai -- segments cut --symbol MNQ --month 2026-04 --window asia --out analysis/out/asia-mnq-2026-04.json
```

`--window` is one of four, each stated in exchange-local time and anchored on
the trade date's own 17:00 reopen, so all four are DST-correct without a second
calendar:

| window | exchange-local | New York |
|---|---|---|
| `asia` | 17:00 to 02:00 | 18:00 to 03:00 |
| `london` | 02:00 to 08:00 | 03:00 to 09:00 |
| `ny-morning` | 08:00 to 11:00 | 09:00 to 12:00, cash open with a lead-in, to lunch |
| `ny-afternoon` | 09:30 to 15:00 | 10:30 to 16:00, to the cash close |

A window overlapping the 15:15 trading halt is refused rather than cut: it
would carry the halt's fifteen-minute hole invisibly into every loop of the
composed river. The corpus directory is
resolved under `--root` from the conventional
`<symbol>v/<month>.<state>.tbbo` layout, or named outright with `--dir`. The
library holds no absolute price anywhere in it: every segment is a sequence of
log returns
against its own previous trade, plus one measured `open_gap_ret` recording the
jump from the last print before the window to the first print inside it. Stderr
reports the work size - segments cut, ticks in them, and how many carried a
measured open gap.

A holiday half-session is dropped, by name, on stderr. `--min-ticks-fraction`
is the rule: a session carrying less than that fraction of the month's median
session is a stub rather than a session, and sampling would otherwise draw it
as often as a full day. The 2026-04-03 `ny-morning` slice is the worked
example - Good Friday, 4,408 ticks against a 400,000-tick typical day, non-empty
and so invisible to every other rule. Pass 0 to keep every non-empty slice.

`segments compose` realizes that library as an endless single-session river and
dumps that composition as CSV:

```text
brokkr run mogwai -- segments compose --library analysis/out/asia-mnq-2026-04.json --type bars --interval-s 60 --ticks 3000000 --out analysis/out/asia-endless.csv
```

Because the library is in returns space, composing is integration: the river
carries a running price each stored return multiplies, so a segment boundary
needs no level reconciliation and any slice can follow any other. `--start-price`
is therefore an integration constant that scales the whole river and changes no
return. `--seed` fixes the draw order, `--in-order` cycles the library instead
of sampling it with replacement, and `--no-reopen-gaps` suppresses the measured
gap at each seam - which is the A/B against the fitted generator, since that
generator produces no reopen gaps at all. The source is endless, so a dump is
bounded by `--ticks` rather than by exhaustion.

Two bounds the composer enforces, both reported. The running level is held
inside the band the fitted generator uses - the library's own `tick_size` at the
bottom and the generator's `MID_CEILING` at the top, read from that one constant
rather than restated here, so no third copy of the number can go stale - because
an endless integration has nothing to re-anchor it, and an unbounded walk prints
zeros at the bottom (a non-positive price) and blows up the decimal arithmetic at
the top. `composed_price_clamps=` on stderr counts the hits: nonzero means the
prices past that point are the rail rather than the walk, and the river wants a
different `--start-price` or a different library rather than a wider band. And
the river ends rather than freezing if `--start` plus the composed span leaves the
nanosecond clock no room, naming that as the reason; the composer has no other
terminal condition.

The bars output carries the same header `gen --type bars` emits, so one tool
charts either:

```text
python3 analysis/plot_tape.py --csv analysis/out/asia-endless.csv --out analysis/out/asia-endless.html
```

`preflight` runs the fail-closed TBBO corpus contract check against a
delivered corpus directory and writes a hash-bound preflight artifact -
`--corpus`, `--jobs-manifest` (read-only) and `--out` all default to the paths
the retired Python fit implementation used. This command is the frozen July parity path.

For a Stage M new-design month, produce its calendar-bound inventory and then
run Tier 1a with the matching arguments:

```text
brokkr run mogwai -- stage-m preflight --month 202509 --corpus research/market-data/databento/mnqv/2025-09.manifest.tbbo --jobs-manifest analysis/databento-jobs.json --delivery-key 'mnqv|2025-09.manifest|tbbo'
brokkr run mogwai -- stage-m month --month 202509 --corpus research/market-data/databento/mnqv/2025-09.manifest.tbbo --jobs-manifest analysis/databento-jobs.json --delivery-key 'mnqv|2025-09.manifest|tbbo'
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
binding, writing to `analysis/out/mnq-fit.json` by default rather than over the
committed `analysis/mnq-fit.json`.

`synth fingerprint` and `synth cadence` are the fingerprint and cadence
synthesis paths, ported from the retired Python fingerprint and cadence
implementations:
`fingerprint` reads `char_<PAIR>.json` reports plus a cadence measurement,
`cadence` streams raw Binance trade archives. Neither writes into
`analysis/` unless `--out` names a path there explicitly - the bare default
is `analysis/out/`, which is gitignored. `cadence-feasible` reads a cadence
measurement and prints the retired Python cadence-feasibility
implementation's L0 structural-proceed verdict (`PROCEED`, `CLOSE` or `STOP AND ASK`) read off its
`children_mean`/`children_single_frac` anchors, exiting nonzero on anything
but `PROCEED`. It then re-simulates the arrival clock over `--events`
(3,000,000 by default, matching the Python) and exits nonzero when the
realized per-second density misses the feasibility bands - that
simulation is a gate, not a diagnostic, and skipping it would let this
subcommand exit 0 where the script exits 1. `--skip-density` stops after
the structural verdict for callers who only want the L0 reading; the
Python has no such flag, so leaving it off is the matching behaviour.
`--fingerprint` names the session profile the arrival clock consults.

The simulation reproduces CPython's stream draw for draw, so its output is
identical to the retired Python cadence-feasibility implementation field for field
rather than merely close - bit for bit, including `gap_cv2`, which requires
computing the population variance as the exact rational CPython's
`statistics.pvariance` uses rather than in floating point. That identity is
pinned at the default 3,000,000 events, at `--events 14`, and over a
generated 820-case sweep against CPython. Not yet ported: the `--fit` and
`--fit-markov` grid searches, which are candidate-search tools rather than
gates.

`cache` is the manual-case cover for the storage policy's cache class:
`mogwai cache stats` reports entry/file/byte counts under the cache root
(`$XDG_CACHE_HOME/mogwai/`, `~/.cache/mogwai/`, `MOGWAI_CACHE_DIR` or
`--cache-dir`), and `mogwai cache clean` removes every provenance directory.

`mogwai cache clean --stale --keep <TOKEN>` removes every provenance directory
except the named one - the same pruning a cache write already does
automatically, exposed for manual use. The token is named rather than derived,
and `mogwai cache stats --entries` prints the ones present. A cache entry's
provenance token binds the command that produced it (its kernel version, window
start, length and burn-in, and its own sub-contract hash), so a `cache`
invocation has nothing to compute one from; `--stale` without `--keep` refuses
rather than guessing. It used to guess - it folded its own argv into a token,
matched nothing, and deleted the whole cache on the invocation meant to be the
safe one.

A `--keep` naming a token that is not present refuses too, and prints the
candidates. The prune only ever compares names, so a mistyped token keeps
nothing and clears everything - the same data loss the required `--keep` was
introduced to prevent, one keystroke away instead of unconditional.

### `mogwai arrival-control`

Runs protocol 12b brick N's deterministic hourly re-centring negative control.
It checks the per-symbol pre-landing legacy tapes and the standing build gate's
transcript before walking fit seeds 301 through 304 and test seeds 305 through
308. The command reads `analysis/mnq-measure-12a.json` and
`analysis/mnq-minute-range-envelope.json`, and writes the hash-bound result to
`analysis/mnq-arrival-control.json`. It refuses a dirty tree and requires the
per-symbol parent-build baselines under
`analysis/out/arrival-control-b1-baseline/`.

Three baselines are required, one per symbol gate B1 walks: `BTCUSDT.csv`,
`MES.csv` and `MNQ.csv`, named by `B1_SYMBOLS` in
`crates/mogwai-cli/src/arrival_control.rs`. A missing, unreadable or
zero-length one refuses B1 rather than passing it. The committed brick N
artifact recorded five, which is history: it predates the 2026-08-09
retirement of the ETHUSDT and SOLUSDT presets, and a tree carrying those two
extra files is not a tree the command reads them from.

Gate B5 is evidence this command reads, never a check it runs. Run
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
that has moved since the baseline commit, fails the tape-identity gate. The
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
