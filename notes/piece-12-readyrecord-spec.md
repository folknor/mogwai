# Piece 12: `ReadyRecord` under per-boat clocks

Implementation specification.

Written against `reference/technical-implementation-spec.md` (the contract this
document is judged by). Spawned from `notes/todo.md`: the section "Landing the
grand design: fourteen pieces", item 12 (`ReadyRecord` under per-boat clocks),
and the two design bullets that item points at under "Open issues" - the still
open schema question numbered 2 in "STILL OPEN, both surfaced 2026-08-15", and
the boatyard/per-boat-clock bullet that records what pieces 9, 10 and 11
landed.

## 1. What the item asks, restated

`ReadyRecord` (`crates/mogwai-protocol/src/ready.rs`, `VERSION = 5`) is the one
JSON line the venue writes to stdout at boot (`serve.rs` writes it on the
process's own locked stdout; `ready.rs`'s module doc calls the same channel the
launcher's "inherited ready fd", which is the older spelling of the same thing -
this spec says stdout, matching `launch.rs` and the code, and 3.7 aligns the
stale phrasing rather than propagating both). It carries `version`, `addr`,
`pid`, `symbol`, `run_seed`, `data_origin_ns`, `run_start_ns`,
`run_duration_ns`, `warmup_ns`, `version_string`.

The TODO names one certainty and one open question:

- CERTAIN: `symbol` must go, bumping `VERSION` to 6. A venue under the
  many-boats-on-many-rivers model has no symbol, so a single `symbol` field can
  only lie. Attach does not need it - broadarrow keys on endpoint plus
  `run_seed`, which remain correct venue identity.
- OPEN: `data_origin_ns`, `run_start_ns` and `warmup_ns` were written when a
  run had one clock. With one clock per boat, are they still venue properties
  or have they become river properties?

The knock-on the TODO raises and then closes itself - that a river's path might
depend on when its boat was placed - is CLOSED by piece 9's fixed placement
origin and is not re-opened here.

## 2. Survey of the ground

### 2.1 What the three time fields actually are, in the landed code

Traced through `crates/mogwai-server/src/serve.rs`, `run.rs`, `boatyard.rs`
and `source.rs` as they stand after pieces 9, 10 and 11:

- `data_origin_ns` is `source::TAPE_ORIGIN_NS`, a compile-time constant.
  `Run::data_origin_ns()` returns that constant and nothing else.
  `Rivers::history_source` defaults an absent `start` to the same constant for
  EVERY river. There is exactly one tape origin, shared by every river in the
  process.
- `warmup_ns` is `cfg.warmup_ns`, a single top-level config key. There is no
  per-symbol warmup knob.
- `run_start_ns` is `TAPE_ORIGIN_NS.saturating_add(cfg.warmup_ns)`, computed
  once in `serve_async` and passed to `Run::new` as `started_ns`. `Run::new`
  hands that same value to `Boatyard::new` as its `origin_ns`, and
  `Boatyard::board` places EVERY boat at `self.origin_ns`: the cursor is
  `place_cursor(&river, origin_ns)` and the boat's `SimClock` gets
  `sim_epoch_ns: self.origin_ns`. It is also the epoch the venue deadline is
  measured from (`deadline_ns = started_ns + run_duration_ns`).

So the survey's verdict, which is the answer to the open question: **all three
remain venue properties.** Per-boat clocks made the WALL ANCHOR per boat
(`wall_anchor_ns: now_ns()` at placement) and the SPEED per boat
(`BoatKey.speed_micros`); they did not make the sim epoch, the tape origin or
the warmup span per boat. Every boat on every river is placed at the one
`run_start_ns`, on a tape whose origin is the one `TAPE_ORIGIN_NS`.

### 2.2 The one thing that IS river-scoped, and is not in the record

Warmup MATERIALIZATION is per river and lazy. `serve_async` calls
`Rivers::ensure_reach(boot_symbol, run_start_ns)` for the boot river only;
`Rivers::ensure_reach`'s own doc comment states that nothing pre-warms a
non-boot river and that the first distant read on a cold river pays the whole
walk from `TAPE_ORIGIN_NS` synchronously.

This makes FIVE pieces of shipped prose false as written. The first two were
found in the original survey; R1 and R2 found the other three, and the audit is
now the whole set of places that assert eager materialization or that the record
names a symbol:

- `ServerClock::warmup_ns`'s doc (`crates/mogwai-protocol/src/clock.rs`) says
  the whole span "is materialized at boot and held for the life of the
  process". True of the boot river, false of every other one.
- `Run::warmup_ns`'s doc says "Sim span of history generated eagerly at boot".
  Same defect.
- `Run::started_ns`'s doc (`crates/mogwai-server/src/run.rs`) says the instant
  is "set after warmup was materialized". Same defect, and worse: `started_ns`
  is computed as `TAPE_ORIGIN_NS + cfg.warmup_ns` BEFORE any warmup runs, so the
  clause is false even for the boot river.
- `Config::warmup_ns`'s doc (`crates/mogwai-server/src/config.rs`) says the whole
  span "is MATERIALIZED before the readiness record is written (see
  `Rivers::ensure_reach`) rather than merely permitted". True of the boot river
  only.
- `docs/config.md` repeats that claim in durable prose ("what changed is that the
  venue now MATERIALIZES it at boot instead of merely permitting requests into
  it"), and separately tells clients to read the served symbol "from the
  readiness record or `/instruments`". The second half is the sentence this
  piece invalidates: after version 6 the record carries no symbol, so
  `/instruments` becomes the only answer that endpoint sentence can name.

The honest statement, and the one this spec writes: `warmup_ns` is a
SERVABILITY span, uniform across rivers - every river can be read back to
`data_origin_ns` and every boat is placed one `warmup_ns` after it. WHERE the
span is already materialized is a latency property of one river, not a
servability property of the venue, and it is deliberately not on the wire (a
launcher cannot act on it, and it changes during the run as rivers are read).

### 2.2b What the record's `symbol` was actually the source of truth FOR

The field was not decorative, and this is the finding that reshapes 3.5 and 3.6.
`serve.rs` builds it from `instrument.symbol` - the symbol of the RESOLVED
`InstrumentDef`, obtained through
`profiles.boot_symbol_def(cfg.boot_symbol())` - not from the raw config key.
Those two strings differ in two supported cases:

- CASE. Preset lookup is ASCII case-insensitive, so `symbol = "mnq"` resolves
  `[symbols.MNQ]` and the venue serves, and refuses history for, the canonical
  spelling. `docs/config.md` states this exactly, which is why it directs
  clients at the record or `/instruments` rather than at their own typing.
- ABSENCE. An absent top-level `symbol` does NOT mean `DEFAULT_PRESET`:
  `[instrument] preset = "MNQ"` with no top-level key boots MNQ.
  `source.rs`'s doc on `InstrumentProfiles::boot` says so in as many words, and
  `Config::boot_symbol()` returns only the optional raw key, never the resolved
  boot instrument.

So any replacement for the removed field must resolve the boot symbol the way
`serve.rs` does, through `build_instrument_profiles` + `boot_symbol_def`, or ask
`/instruments`. Reading `cfg.symbol` and defaulting to `DEFAULT_PRESET` - which
an earlier draft of 3.5 and 3.6 did - is the "type it" path `docs/config.md`
warns against, and it is silently right only for configs whose key is present
and canonically spelled, which is every config in the tree's test corpus today
and therefore an accident rather than a property.

### 2.3 Every consumer of the record

`grep ReadyRecord` plus `grep record.symbol` across the tree, excluding
`research/` and `target/`:

| Site | Uses | Effect of dropping `symbol` |
|---|---|---|
| `crates/mogwai-protocol/src/ready.rs` | defines it; `ready_record_round_trips` pins the exact JSON bytes | golden string edit, `VERSION` edit |
| `crates/mogwai-protocol/src/launch.rs` | PRODUCTION: the `mogwai venue up` tracing event logs `symbol = %record.symbol`. TESTS: the `record_json` fixture contains `"symbol"`, and `a_current_record_parses` asserts `record.symbol == "BTCUSDT"`. Also the `parse_ready` version gate | the log field and the assertion must both go or be replaced, or the crate does not compile; fixture edit. `deny_unknown_fields` is NOT set on the struct, so an old record with `symbol` would still parse - the version gate is what refuses it, and it fires first. See 3.3 |
| `crates/mogwai-server/src/serve.rs` | constructs the record with `symbol: instrument.symbol.to_string()` | field deleted at the one construction site |
| `crates/mogwai-adapter/src/config.rs` | `MogwaiDataClientConfig::for_run` and `MogwaiExecClientConfig::for_run` both set `symbol: Some(record.symbol.clone())`; unit test asserts `Some("MNQ")` | both stop setting it; see 3.4 |
| `crates/mogwai-cli/tests/common/mod.rs` | exposes `record` wholesale | gains a `symbol` field of its own; see 3.5 |
| `crates/mogwai-cli/tests/serving.rs` | ~35 uses of `venue.record.symbol` as "the symbol this venue serves" | mechanical rename to `venue.symbol` |
| `crates/mogwai-cli/tests/completion.rs` | one use, same meaning | same rename |
| `scripts/smoke.py` | `READY_VERSION = 5`; `self.symbol = record["symbol"]`; asserts `/instruments` has exactly one entry equal to it | version bump plus config-derived symbol; see 3.6 |
| `docs/config.md` | durable prose telling clients to read the served spelling "from the readiness record or `/instruments`", plus the eager-materialization claim | the record half of that sentence becomes false; `/instruments` becomes the sole named answer. Corrected in 3.7 |
| `crates/mogwai-server/src/config.rs` | `Config::warmup_ns`'s doc claims the whole span is materialized before readiness | doc correction, 3.7 |
| `notes/todo.md` | the hardcoded-value inventory records `ReadyRecord::VERSION = 5` | inventory line moves to 6, 3.7 |
| `README.md`, `docs/cli.md`, `reference/glossary.md` | prose about the record; none names `symbol` | prose additions only |

Nothing in `mogwai-engine`, `mogwai-data` or `mogwai-lab` touches the record.

### 2.4 Cross-repo

broadarrow consumes the venue but is not a build input here, and per the TODO
it keys attach on endpoint plus `run_seed`. Both survive. A broadarrow build
that reads `record.symbol` breaks loudly at the version gate rather than
silently, which is the designed handoff already stated for piece 13. This spec
does not edit `research/`.

## 3. The target, brick by brick

One coherent landing. The record is a wire type with a version gate: a tree in
which the protocol says 6 and `serve.rs` still writes `symbol` does not
compile, and a tree in which the Rust side says 6 while `smoke.py` says 5
fails its very first assertion. There is therefore no orderable split, and
sections 3.1 through 3.7 are the internal order of ONE commit, not a sequence
of landings. The keep/revert unit is that commit.

### 3.1 `crates/mogwai-protocol/src/ready.rs` - the schema

Final shape:

```rust
pub struct ReadyRecord {
    pub version: u32,
    pub addr: SocketAddr,
    pub pid: u32,
    pub run_seed: u64,
    pub data_origin_ns: u64,
    pub run_start_ns: u64,
    pub run_duration_ns: Option<u64>,
    pub warmup_ns: u64,
    pub version_string: String,
}

impl ReadyRecord {
    pub const VERSION: u32 = 6;
}
```

`symbol` deleted. Field ORDER is otherwise unchanged, because the round-trip
test pins the serialized byte order and a gratuitous reorder makes the diff
unreadable for no gain.

The doc comments are the substance of this brick, and they are what the open
question resolves into. Written on the type:

- On the struct: this record describes A VENUE, not a river and not a boat. A
  venue serves any number of rivers, each carrying at most one boat with its
  own wall anchor and its own speed; nothing that varies per river or per boat
  appears here. Venue identity for attach is `addr` plus `run_seed`.
- On `run_seed`: with the config, the fingerprint and `version_string` it
  reproduces every river the venue can serve. A boat is always placed at the
  river's origin rather than at sim-now-at-placement, so a river's path is a
  pure function of its key and this seed, regardless of when - or whether -
  anyone boards it.
- On `data_origin_ns`: the earliest `ts_event` any river can serve. One tape
  origin per venue, identical for every river, so this is a venue fact under
  per-boat clocks and not a per-river one.
- On `run_start_ns`: the venue's PLACEMENT ORIGIN. Every boat on every river
  is placed here, whenever it boards, which is what makes a boat's path
  independent of its boarding instant. It is also the epoch
  `run_duration_ns` is measured from. Both roles are the same number by
  construction (`TAPE_ORIGIN_NS + warmup_ns`), and the field carries both
  deliberately.
- On `warmup_ns`: the sim distance from `data_origin_ns` to `run_start_ns`,
  uniform across rivers. Every river is SERVABLE back to `data_origin_ns`.
  Whether a given river's span is already materialized is a latency property
  of that river - the boot river's is generated before this record is written,
  every other river's on first read - and is deliberately not reported: a
  launcher cannot act on it and it changes during the run.

REJECTED ALTERNATIVE, recorded so it is not re-proposed: replacing `symbol`
with `boot_symbol: Option<String>`. A boot symbol does still exist in the
landed slice-1 state (the top-level `symbol` config key; `/ws` with no
`?symbol=` binds it). It is rejected because that key is a COMPATIBILITY SHIM
of the `/ws` carrier, not venue identity: it says which river an
under-specified socket lands on, which is a fact about a client that failed to
name its river, not a fact about the venue. Every consumer that needs a symbol
already has one - from its own config, its own slate, or `/instruments` - and
putting the shim on the wire would keep the tests and the smoke script reading
their intent out of the venue's mouth, which is exactly the coupling this
piece exists to cut.

Also REJECTED: adding a river list. It would be a snapshot of the configured
set, which is `/instruments`' answer under piece 13, and duplicating it in a
boot-time record creates two sources of truth that drift the moment a river is
materialized.

### 3.2 `crates/mogwai-protocol/src/ready.rs` - the test

`ready_record_round_trips` drops `symbol` from the constructed value and from
the expected JSON string, which becomes:

```
{"version":6,"addr":"127.0.0.1:41235","pid":42,"run_seed":7,"data_origin_ns":1,"run_start_ns":2,"run_duration_ns":null,"warmup_ns":1,"version_string":"test"}
```

This is an exact-equality golden re-blessed knowingly in the change that moves
it, per the standing rule in `AGENTS.md`.

Add one test beside it, `a_record_carrying_a_symbol_is_refused_by_version`,
asserting that a JSON body at `ReadyRecord::VERSION - 1` WITH a `"symbol"` key
fails `launch::parse_ready` with `LaunchError::Version` carrying
`reported: ReadyRecord::VERSION - 1` and `understood: ReadyRecord::VERSION` -
the variant's real name and field names. It lives in `launch.rs`'s test module,
where `parse_ready` is reachable.

Two corrections to what this test claims, both from R1 and both worth having in
writing because they bound its value:

- It is NEARLY REDUNDANT with the existing refusal loop, which already iterates
  `VERSION + 1` and `VERSION - 1`, and `VERSION - 1` IS 5 after the bump. The
  only delta is the extra `symbol` key, which serde ignores either way. Its
  honest purpose is therefore to document SERDE LENIENCY - that
  `deny_unknown_fields` is absent, so a stale record's removed field parses fine
  and the version gate is the only thing refusing it - and its doc comment says
  exactly that rather than implying it adds version coverage.
- Its version numbers are DERIVED from `ReadyRecord::VERSION`, never hardcoded
  to 5 and 6. A hardcoded `reported: 5` stops meaning "the previous schema" at
  version 7 and starts silently testing ancient history.

### 3.3 `crates/mogwai-protocol/src/launch.rs`

CORRECTION, R1 and R2 both: an earlier draft said "no production code in this
file reads `symbol`". That is FALSE. There are three edits here, two of them
compile-breaking:

1. PRODUCTION. The `mogwai venue up` tracing event in the launcher's boot path
   emits `symbol = %record.symbol`. The field is deleted and REPLACED by
   `run_start_ns = record.run_start_ns` - the announcement exists so a consumer
   cannot forget what it attached to, and under version 6 the venue's identity
   is `addr` plus `run_seed` plus its clock origins, all of which the event
   already carries or now gains. It does not gain a symbol from anywhere else:
   the launcher does not know one, which is the entire point of the piece.
2. TESTS. `a_current_record_parses` asserts `record.symbol == "BTCUSDT"`. The
   assertion is deleted; the test keeps its remaining field assertions, which
   are what make it a schema test rather than a symbol test.
3. `record_json` in the tests drops its `"symbol"` line. The
   `ReadyRecord::VERSION + 1` / `- 1` refusal test needs no change (6 +/- 1 are
   both non-current).

### 3.4 `crates/mogwai-adapter/src/config.rs`

Both `for_run` constructors stop setting `symbol`, becoming:

```rust
pub fn for_run(record: &mogwai_protocol::ReadyRecord, account_id: AccountId) -> Self {
    Self {
        expected_run_seed: Some(record.run_seed),
        ..Self::for_addr(record.addr, account_id)
    }
}
```

`symbol: None` already means "take the venue's boot river" in both configs'
`ws_url()`, so the resulting behavior is unchanged for a single-river venue
and becomes HONEST for a multi-river one: the adapter no longer claims to have
been told which river it belongs on when nobody told it.

A host that wants a specific river must be able to say so in one expression.
`symbol` is already a plain `pub` field on both configs, so struct-update syntax
already suffices and the builder below is CONVENIENCE, not necessity (R1). It is
kept in scope anyway, deliberately and narrowly: it is the one API affordance
that replaces what `for_run` used to do for the caller, so removing the implicit
symbol without offering the explicit one would leave the diff a pure subtraction
at the call site. Nothing else in this piece adds API. Add on both configs:

```rust
#[must_use]
pub fn with_symbol(mut self, symbol: impl Into<String>) -> Self {
    self.symbol = Some(symbol.into());
    self
}
```

Doc: names the river this client's socket binds, matching `/ws?symbol=`.
Absent, the socket takes the venue's boot river. Its value is the host's own
choice - it is no longer derivable from the readiness record, because a venue
serves any river its config resolves.

The existing unit test `for_run_carries_the_records_symbol`, which asserts
`data.symbol.as_deref() == Some("MNQ")` and the same for exec, is renamed
`for_run_binds_the_run_and_names_no_river` and rewritten: `for_run` now
yields `None`, and a
second assertion covers `for_run(...).with_symbol("MNQ")` yielding
`Some("MNQ")` and a `ws_url()` ending `/ws?symbol=MNQ`. The fixture
`ready_record()` in that module drops its `symbol` field and moves to
`VERSION` 6 through the constant it already uses.

### 3.5 `crates/mogwai-cli/tests/common/mod.rs`

The harness is where the coupling actually bites: 35-odd call sites read
`venue.record.symbol` meaning "the symbol this venue serves". They must keep
working, and they must get their answer from the CONFIG rather than from the
venue's mouth.

`Venue` gains a field:

```rust
pub struct Venue {
    inner: LaunchedVenue,
    pub record: ReadyRecord,
    /// The boot river this venue's config selects, resolved by the harness from
    /// the same config it launched with. NOT read from the readiness record:
    /// a venue serves many rivers and reports no symbol (`ReadyRecord` 6), so a
    /// test that wants one names it the way a real client does - from its own
    /// configuration.
    pub symbol: String,
    pub ready_at: Instant,
}
```

`spawn` resolves it before returning, from the `--config` value it was handed,
THROUGH THE SAME RESOLUTION `serve.rs` PERFORMS - not through the raw
`Config::boot_symbol()` key, which is the wrong string in the two cases 2.2b
names:

```rust
fn boot_symbol(config: Option<&std::path::Path>) -> String {
    use mogwai_server::config;
    let cfg = config::Config::load(config.map(PathBuf::from))
        .expect("the harness launches only configs the venue accepts");
    config::build_instrument_profiles(&cfg)
        .expect("the harness launches only configs the venue accepts")
        .boot_symbol_def(cfg.boot_symbol())
        .expect("the boot shape resolves for a config the venue accepts")
        .symbol
        .to_string()
}
```

`Config`, `Config::load`, `Config::boot_symbol`, `build_instrument_profiles` and
`InstrumentProfiles::boot_symbol_def` are all already `pub` in
`mogwai_server::config` / `mogwai_server::source`, and `mogwai-cli` already
depends on `mogwai-server`, so this needs no new dependency, no new public API
and no TOML parsing of our own. It is line-for-line what `serve.rs` does before
building the record, which is the point: the harness and the venue cannot
disagree about what the boot symbol IS - including the preset-only config, the
non-canonical-case config, and the no-config case.

Add one harness fixture config exercising the preset-only shape - a
`mnq-preset.toml` carrying `[instrument] preset = "MNQ"` and no top-level
`symbol` - and one `serving.rs` test that spawns with it and asserts
`venue.symbol == "MNQ"` and that `/instruments` serves it. This is the
regression that bites the rejected `cfg.boot_symbol().unwrap_or(DEFAULT_PRESET)`
implementation, which would answer BTCUSDT for a venue serving MNQ. Without it
the whole tree's configs agree with the wrong implementation by accident.

`spawn` already walks `extra_args` to build the `LaunchSpec`; it reads the
resolved `spec.config` rather than re-scanning the slice.

`serving.rs` and `completion.rs`: mechanical `venue.record.symbol` ->
`venue.symbol`. Uses of `venue.record.run_start_ns` and
`venue.record.data_origin_ns` are untouched - those fields survive with
unchanged meaning.

### 3.6 `scripts/smoke.py`

Three edits:

1. `READY_VERSION = 6`.
2. `self.symbol` stops reading the record. Python has no access to the venue's
   resolution - no `mogwai config` subcommand exists to print a resolved boot
   symbol, and re-implementing preset lookup, case folding and the
   `[instrument] preset` fallback in `tomllib` would be a second, drifting copy
   of exactly the logic 2.2b says must not be re-typed. So the smoke does NOT
   derive its symbol from the config alone. It asks the venue, through
   `/instruments`, and uses the config only as the case-insensitive SELECTOR of
   which served river is its boot river:

   ```python
   # The venue reports no symbol (ReadyRecord 6): it serves any river its
   # config resolves. The served spelling is the venue's to state, so we take
   # it from /instruments and use the config key only to pick which entry is
   # the boot river - matching case-insensitively, because preset lookup is.
   def boot_key(config: str | None) -> str | None:
       if config is None:
           return None
       with open(config, "rb") as handle:
           doc = tomllib.load(handle)
       return doc.get("symbol") or doc.get("instrument", {}).get("preset")

   def boot_symbol(config: str | None, served: list[str]) -> str:
       key = boot_key(config)
       if key is None:
           assert len(served) == 1, f"no boot key, and the venue serves {served}"
           return served[0]
       matches = [entry for entry in served if entry.lower() == key.lower()]
       assert len(matches) == 1, f"config boot key {key} matches {matches} of {served}"
       return matches[0]
   ```

   `tomllib` is standard library since 3.11, so this keeps the script's
   no-third-party-dependency rule. Nothing here duplicates `DEFAULT_PRESET`: the
   earlier draft's Python constant is DROPPED, and with it the drift risk that
   constant carried, because the default now arrives as the venue's own answer
   on `/instruments`.
3. The `/instruments` assertion. Today it is `len(instruments) == 1` plus an
   equality against the record's symbol. It becomes:

   ```python
   served = [entry["symbol"] for entry in instruments]
   assert venue.symbol in served, (
       f"the venue serves {served}, the boot river is {venue.symbol}"
   )
   ```

   CORRECTION to the earlier justification, R1: the reason given was that
   `two-symbols.toml` already violates `len == 1`. That config exists but
   `smoke.py` never loads it, so it justifies nothing here. The real reason the
   `len == 1` half goes is that it is a slice-1 assumption piece 13 rewrites
   wholesale, and this piece already stops the script from knowing the symbol
   independently of `/instruments` - keeping a count assertion whose companion
   equality is now sourced FROM the same response would be asserting the
   response against itself. What survives is the selector check in edit 2, which
   is the real bite: it fails loudly if the config's boot key resolves to no
   served river or to more than one.

### 3.7 The durable prose

The standing item in `notes/todo.md` requires `reference/` and `docs/` writing
WITH the code, not afterwards, and the same file records the bug-hunt finding
that durable prose asserting a live constant goes stale silently. In this
commit:

- `reference/glossary.md`: the `ReadyRecord` line gains that the record
  describes a venue and names no symbol, and that venue identity for attach is
  `addr` plus `run_seed`. The `Warmup` entry changes "materialized simulated
  history" to the servability wording of 3.1 - the boot river's span is
  materialized before readiness, every other river's on first read.
- `reference/clock.md`: the sentence "`warmup_ns` is the materialized
  simulated interval before `run_start_ns`" takes the same correction, and the
  section gains one sentence that `run_start_ns` is the placement origin every
  boat gets, so per-boat clocks differ in wall anchor and speed but never in
  sim epoch.
- `reference/architecture.md`: the `run_start_ns = TAPE_ORIGIN_NS + warmup_ns`
  paragraph gains the venue-versus-river statement - one tape origin, one
  placement origin, one warmup span, N rivers, at most one boat per river with
  its own wall anchor and speed - and states that this is why the readiness
  record carries the three time fields and no symbol.
- `docs/cli.md`: the readiness-record paragraphs state the field list at
  version 6 and that the venue reports no symbol; a client names its river on
  `/ws?symbol=` from its own configuration. The existing `/ws` paragraph
  already says the parameter is optional and defaults to the boot symbol, so
  it needs only the cross-reference.
- `README.md`: step 2 of the hand-rolled launcher contract already says "check
  its `version` first, then use its `addr`". Add that the record names no
  instrument, so a launcher that needs one takes it from its own config.
- `docs/config.md`: the eager-materialization sentence takes the servability
  correction of 2.2, and the "clients should read it from the readiness record
  or `/instruments`" sentence drops the record - after version 6 the record
  carries no symbol and `/instruments` is the only place the served spelling is
  stated. This file is DURABLE and binding, so leaving it is knowingly shipping
  a false instruction to consumers.
- `crates/mogwai-protocol/src/clock.rs`: `ServerClock::warmup_ns`'s doc loses
  the false "materialized at boot and held for the life of the process" claim
  (2.2) and takes the servability wording.
- `crates/mogwai-server/src/run.rs`: `Run::warmup_ns`'s doc takes the same
  correction, and `Run::started_ns`'s doc loses "set after warmup was
  materialized" - the value is computed before any warmup runs.
- `crates/mogwai-server/src/config.rs`: `Config::warmup_ns`'s doc loses the
  "MATERIALIZED before the readiness record is written" claim and takes the
  servability wording.
- `crates/mogwai-protocol/src/ready.rs`: the module doc's "inherited ready fd"
  becomes stdout, matching what `serve.rs` writes and what `launch.rs` reads
  (section 1). One spelling, since this commit is already rewriting the type's
  docs.

`notes/todo.md` itself: piece 12 is REMOVED ENTIRELY from the fourteen-piece
list per that file's own rule, replaced by the one-line landed note the
neighbouring pieces use, and open question 2 in the "STILL OPEN" bullet is
deleted (question 1, the unconfigured-symbol session, stays - it is piece
13's). The hardcoded-value inventory later in that same file records
`ReadyRecord::VERSION = 5`; it moves to 6 in this commit, which is the point of
keeping the inventory. The boatless-river sweep gap recorded in the boatyard bullet is NOT
touched: it is named there as explicitly out of pieces 9, 10 and 11, and it is
out of this one too (section 5).

### 3.8 `TAPE_PROTOCOL_VERSION`

NOT bumped. Nothing here touches the tape generation path: no generator
constant, no arrival-clock or GARCH parameter, no fingerprint, no seed
derivation, no fill band, no tape origin. `ReadyRecord::VERSION` is this
record's own identity and moves 5 -> 6; the two constants are independent and
this change moves no generated byte. (The standing rule's own escape hatch -
give a non-tape artifact its own version rather than overloading the tape
constant - is exactly what `ReadyRecord::VERSION` already is.)

## 4. Verification

Every gate, with the exact command.

1. **Wire protocol - the record's bytes.** The re-blessed golden and the new
   version-refusal test.

       brokkr test -p mogwai-protocol ready_record_round_trips
       brokkr test -p mogwai-protocol a_record_carrying_a_symbol_is_refused_by_version

   Re-bless expectation: the golden JSON string in `ready_record_round_trips`
   is edited in this commit to the exact bytes in 3.2. Its `version` field
   reads 6 and the string contains no `symbol` key.

2. **Bite check on the new test**, per the standing rule - and it needs the
   right lever. Reverting `ReadyRecord::VERSION` to 5 does NOT isolate the new
   test (R1): it also fails the re-blessed golden and the existing
   `VERSION +/- 1` refusal loop, so an observed red proves nothing about the
   test under check, and with the version derived rather than hardcoded (3.2)
   the new test would not even fail. The isolating lever is the GATE ITSELF:
   comment out the raw-JSON `version` check in `parse_ready` AS A TEXT EDIT and
   confirm that `a_record_carrying_a_symbol_is_refused_by_version` is the test
   that turns red - a stale body then parses cleanly, because serde ignores the
   unknown `symbol` key, which is precisely the leniency the test documents.
   Restore it as a text edit. Never with `git checkout -- <path>`.

   Bite-check the preset-only harness test of 3.5 the same way: replace the
   `build_instrument_profiles` / `boot_symbol_def` resolution with
   `cfg.boot_symbol().unwrap_or(DEFAULT_PRESET)` as a text edit, confirm the
   `mnq-preset.toml` test fails with BTCUSDT against a venue serving MNQ, then
   restore. A resolution helper nothing can falsify is the accident 2.2b names.

3. **Adapter configs.** The `for_run` change and the new builder.

       brokkr test -p mogwai-adapter for_run_binds_the_run_and_names_no_river

4. **The whole tree, including the socket-backed adapter binaries.** This
   change touches `mogwai-adapter`, so the plain check is not sufficient:

       brokkr check --gate

5. **The launcher contract end to end**, which is what actually exercises the
   harness rewrite of 3.5 against a real process:

       brokkr check -- --test serving
       brokkr check -- --test completion
       brokkr check -- --test lifecycle

   Only `serving.rs` and `completion.rs` read `venue.record.symbol` today;
   `lifecycle` does NOT (R1 corrected the earlier claim). It is still run, for a
   different and real reason: it is the third consumer of the `common/mod.rs`
   harness that 3.5 changes the constructor of, so it is the binary most likely
   to break on a `Venue` field addition it never asked for. These are not
   `#[ignore]`d, so gate 4 also
   runs them; the per-binary form above is for iterating on 3.5 without a
   whole-tree sweep between edits. `brokkr check` runs them in the dev
   profile, which is what these subprocess-lifecycle tests want - release-LTO
   compile time dominates their wall time and optimization changes nothing
   under test.

6. **The live end-to-end path, and the only gate that covers `smoke.py`.**
   The script builds and spawns the venue itself, so nothing needs launching
   first:

       python3 scripts/smoke.py
       python3 scripts/smoke.py futures

   The `futures` mode is included because it runs the `mnq.toml` config, which
   is the case where the config's boot key is present and non-default; the
   default mode covers the no-config branch of `boot_symbol`, where the venue's
   `/instruments` answer is the only source of the symbol. Between them both
   branches of the helper in 3.6 edit 2 are executed.

7. **No new instrument is owed.** Every gate above is an existing command; no
   behavior in this change lacks one. Nothing here is a throughput or volume
   claim, so no measurement gates it and no proceed/close threshold applies.

## 5. Stopping rule

IN scope: the `ReadyRecord` schema, its version, its doc comments, the one
construction site, every consumer listed in 2.3, and the durable prose in 3.7
plus the two false in-code warmup claims found in 2.2.

OUT of scope, named rather than deferred - each belongs to a different item
that already exists:

- **Piece 13**, the consumer surface: `/instruments` returning the resolved
  configuration, the adapter's subscription guard, and the runtime funds
  rejection naming its currency. This spec touches `/instruments` only by
  weakening one smoke assertion that would otherwise die for the wrong reason,
  and does not change the endpoint.
- **Open question 1**, whether subscribing to an unconfigured symbol is a
  supported session. Untouched; it gates the adapter guard, not the record.
- **The boatless-river sweep gap** recorded in the boatyard bullet of
  `notes/todo.md`: a resting order on a river whose boat wound down is not
  swept. Explicitly left open by pieces 9 and 10 and left open here.
- **The boot symbol itself.** Slice 1 keeps it (owner ruling, piece 4), `/ws`
  with no `?symbol=` still binds it, and this spec REMOVES it from the wire
  record without removing it from the config. Those are different questions:
  the config key tells an under-specified socket where to land, the record
  field claimed to describe the venue.
- **`research/` and broadarrow.** Read-only reference; their build breaking at
  the version gate is the designed handoff.

## 6. Review reconciliation

Two reviews of the pre-revision draft (`notes/piece12-spec-review-R1.md`,
Claude; `notes/piece12-spec-review-R2.md`, codex deep). Every finding was
re-verified against the landed code before folding. Both reviews independently
found the same two serious defects, which is the strongest signal in the pair.

FOLDED, with where each landed:

1. `launch.rs` production log reads `record.symbol` (R1 + R2) - 2.3, 3.3. The
   draft's "no production code in this file reads `symbol`" was flatly false and
   would not have compiled.
2. `a_current_record_parses` asserts `record.symbol` (R1 + R2) - 2.3, 3.3.
3. Boot-symbol resolution cannot use the raw `Config::boot_symbol()` key
   (R2 high, sharpened by R1's case argument) - new 2.2b, rewritten 3.5 and 3.6,
   plus a preset-only regression config and its bite check. Verified: an absent
   top-level `symbol` does not mean `DEFAULT_PRESET`, and `serve.rs` builds the
   record from the resolved `InstrumentDef`.
4. `docs/config.md` is an unlisted durable consumer, both for its
   eager-materialization claim and for directing clients at the record for the
   served spelling (R1 + R2) - 2.2, 2.3, 3.7.
5. `Config::warmup_ns`'s doc carries the same false claim (R2) - 2.2, 3.7.
6. `Run::started_ns`'s doc carries it too, and is false even for the boot river
   (R1) - 2.2, 3.7.
7. The new refusal test is near-redundant with the existing `VERSION - 1` loop,
   and gate 2's bite check did not isolate it (R1) - 3.2 reframes the test as a
   serde-leniency document with derived versions, and gate 2 switches its lever
   to the version check in `parse_ready`.
8. Gate 5's stated reason for running `lifecycle` was wrong (R1) - gate 5 now
   gives the real one.
9. `notes/todo.md`'s hardcoded-value inventory pins `VERSION = 5` (R1) - 3.7.
10. 3.6's `two-symbols.toml` justification is irrelevant - smoke never loads it
    (R1) - 3.6 edit 3 now gives the real reason.
11. `with_symbol` is convenience, not necessity (R1) - 3.4 says so and states
    why it stays in scope anyway.

REJECTED or ADJUSTED, with why:

- R1's stdout-versus-"inherited ready fd" item is ADJUSTED rather than folded as
  stated. R1 read it as the spec propagating a wrong spelling; the code shows
  the spec is the correct one - `serve.rs` writes the record on locked stdout
  and `launch.rs` reads stdout. The stale phrasing is `ready.rs`'s own module
  doc, so it is corrected in 3.7 instead of the spec being changed.
- R1's suggestion that the harness could instead take the symbol from
  `/instruments` is REJECTED for the RUST harness and ACCEPTED for the Python
  one. The Rust side can call the venue's own resolution directly, which is
  strictly better than asking the running process, because it fails before spawn
  and cannot be satisfied by a venue that resolved the wrong thing. Python has no
  such call, so there `/instruments` is the honest source.
- R2's alternative "or the server needs to expose a public resolved-boot-symbol
  operation" is REJECTED as unnecessary: `build_instrument_profiles` and
  `boot_symbol_def` are already `pub` and already compose into that operation in
  three lines. Adding a convenience wrapper would be new public API for a piece
  whose whole shape is subtraction.
- R2's framing of its first two findings as BLOCKING is accepted for finding 2
  (it does not compile) and downgraded for finding 1: the resolution defect is
  real and folded, but it is a correctness defect in a test harness, not
  something that blocks the landing's shape. Nothing in the central decision -
  drop `symbol`, bump to 6, keep the three time fields as venue properties, no
  `TAPE_PROTOCOL_VERSION` bump - was challenged by either review, and both
  explicitly endorse it.
