# mogwai command line

`mogwai` runs one foreground venue for one run. It owns no PID, log, or
configuration files. Logs go to stderr; `RUST_LOG` selects the tracing filter.

`serve` binds the HTTP and WebSocket endpoint. Its default address is
`127.0.0.1:0`. An ephemeral address requires `--ready-fd`; an explicit nonzero
port does not.

```sh
brokkr run mogwai -- serve --config run.toml --ready-fd 3
```

`--config PATH` is optional and otherwise uses built-in defaults. It never
consults the working directory. `--duration DURATION` overrides
`run_duration_ns` for this invocation.

The launcher creates a pipe, starts `mogwai serve --ready-fd 3` as its direct
child, reads exactly one JSON `ReadyRecord` line, checks `version`, and uses
`addr` for both clients. That read blocks for as long as warmup generation
takes, which is proportional to `warmup_ns` and the tape's cadence; a launcher
wanting a bound sets its own timeout and treats expiry as a boot failure. A
pipe that closes without a line is a boot failure, and the child's stderr and
exit status say why. It keeps the child as a direct child: Linux parent-death
handling terminates the venue if that launcher dies. On a `RunComplete` frame,
the process exits successfully; otherwise the launcher terminates and reaps it.

A launcher that CAPTURES the child's stderr must also DRAIN it, continuously,
from the moment of spawn. Logs go to stderr by design, a pipe holds roughly
64 KiB, and a full pipe blocks the writer - so an undrained capture wedges the
venue mid-run, which is indistinguishable from a hung venue at the socket. A
launcher that does not want the log should redirect it to a file or to the null
device rather than to a pipe it never reads. `scripts/smoke.py` drains it on a
thread, which is the reference form.

`gen` remains the offline generator command. There are no `stop` or `man`
subcommands, and documentation is not compiled into the binary.
