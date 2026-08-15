# Bug hunt: mogwai-server transport, admission and lifecycle

Scope: `main.rs`, `run.rs`, `http.rs`, `ws.rs`, `admission.rs`, `config.rs`,
`man.rs`, `man/`.

Hunter: Claude Opus, single coverage.

CLOSED, 2026-08-15. Every finding, every lesser observation and the rewrite
proposal is resolved: findings 1 through 5 by round 1, findings 6 through 9 and
the observations by round 2, and the rewrite section by round 1's per-connection
sequential dispatcher. Finding 7 was DEAD on arrival (round 1 had already
corrected the 512 KiB arithmetic to 256 KiB); the rewrite's second half - making
`AdmissionSubject` unconstructible from a raw `String` - was DECLINED, with the
invariant held at serialization instead, and that decision plus its residual is
recorded in `notes/bugs-protocol.md` where finding 1 is struck.

What each round landed, what it declined and why, and every bite check, is in
`notes/bug-loop-carry-forward.md`. Nothing here is open; do not re-open a
finding from this file's git history without re-reading that record first.
