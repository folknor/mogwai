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
writes ONE line of JSON to STDOUT - the `ReadyRecord`, carrying `addr` among the
rest - and that is the only thing it ever writes there. Logs go to stderr, so
the two never interleave. A launcher captures stdout and reads a line; a human
sees the same line in the terminal.

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
config, the fingerprint and `version_string` reproduces the served path.

`mogwai --version` prints semver, build hash, build time and the tape
generation process's version (`mogwai_data::TAPE_PROTOCOL_VERSION`) on one
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

The launcher starts `mogwai serve` as its direct child with stdout captured,
reads exactly one JSON `ReadyRecord` line, checks `version`, and uses `addr` for
both clients. That read blocks for as long as warmup generation takes, which is
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

The readiness record already carries `run_seed`, so a launcher can bind its
clients to the run it started rather than to the address it landed on. The
nautilus adapter does this through `MogwaiDataClientConfig::for_run` /
`MogwaiExecClientConfig::for_run`, which check `/health` on every connect and
refuse - terminally, logging `venue identity mismatch` - when the address is
serving a different run. A venue that cannot be probed at all is used rather
than judged: an unanswered probe is a transport failure, not a wrong venue.

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

`gen` remains the offline generator command.

`man` renders the bundled reference docs. Bare, it lists the topics; with one -
`mogwai man cli` - it renders that document to the terminal, colour dropped when
stdout is not a TTY or `NO_COLOR` is set. The docs travel inside the binary via
`include_str!`, so an installed `mogwai` documents itself with no source tree
present. The user-facing docs only: the process documents are not bundled.

There is no `stop` subcommand.
