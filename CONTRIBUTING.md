# Contributing to mogwai

Thanks for your interest in improving mogwai.

## Contributor License Agreement

**By submitting a pull request you agree to the [CLA](CLA.md); the CLA bot will
ask you to confirm on your first PR.** The CLA assigns copyright in your
contribution to the project owner so the project can stay dual-licensed
(AGPL-3.0-only plus separate commercial licenses); in return you keep a full
license back to your own contributions. Acceptance is required before a first
contribution is merged, regardless of its size.

**Copyleft-licensed third-party code cannot be accepted without prior written
approval.** Any third-party code you include must be your own work or under a
permissive license (MIT, BSD, Apache-2.0) that permits relicensing; GPL, LGPL,
AGPL, or MPL code needs the owner's approval first, because it would undercut
the commercial-licensing option. New dependencies are checked against
[`deny.toml`](deny.toml); a non-permissive license fails that gate.

## Building and testing

The four broker crates build nautilus-free; `mogwai-adapter` needs a sibling
`../nautilus_trader` checkout (default-features off, no pyo3). Use `brokkr`, not
raw `cargo`:

```sh
brokkr check   # gremlins + clippy + tests, changed-files scope
brokkr test -p <crate> <NAME>   # focused single-test runner
```

See [`AGENTS.md`](AGENTS.md) for the full codebase conventions and build rules.

## New source files

Every source file carries an SPDX header:

```
SPDX-FileCopyrightText: 2026 folknor
SPDX-License-Identifier: AGPL-3.0-only
```
