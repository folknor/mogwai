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
brokkr run mogwai -- serve --config run.toml
```

`--config PATH` is optional and otherwise uses built-in defaults. It never
consults the working directory. `--duration DURATION` overrides
`run_duration_ns` for this invocation. There is no `--seed` flag: a
reproduced path is a written-down act, so the seed is overridden through the
config file's `seed` key alone; when absent, one is drawn at launch and
reported back in the readiness record's `run_seed`, the value that with the
config, the fingerprint and `version_string` reproduces the served path.

`mogwai --version` prints semver, build hash, build time and the tape
generation process's version (`mogwai_data::TAPE_PROTOCOL_VERSION`) on one
line; the same string is what the readiness record reports as
`version_string`, so an operator can tell whether two runs' tapes are even
comparable before comparing seeds.

The launcher starts `mogwai serve` as its direct child with stdout captured,
reads exactly one JSON `ReadyRecord` line, checks `version`, and uses `addr` for
both clients. That read blocks for as long as warmup generation takes, which is
proportional to `warmup_ns` and the tape's cadence; a launcher wanting a bound
sets its own timeout and treats expiry as a boot failure. Stdout closing without
a line is a boot failure, and the child's stderr and exit status say why. It keeps the child as a direct child: Linux parent-death
handling terminates the venue if that launcher dies. On a `RunComplete` frame,
the process exits successfully; otherwise the launcher terminates and reaps it.

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
