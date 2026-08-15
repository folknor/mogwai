# Implementation spec: the symbol resolves the instrument (slice 1, pieces 1 and 3)

Written against `reference/technical-implementation-spec.md`, which is the
contract this document is judged by. Spawned from `notes/todo.md`, section
"Landing the grand design: fourteen pieces", pieces 1 and 3, and the design
bullet under "Open issues" headed THE SYMBOL IS A REQUEST PARAMETER, NOT AN
IDENTITY THE VENUE OWNS (items 1 and 3 of that bullet's change list).

## 1. What this lands, in one paragraph

Today an operator selects the tape's knobs by writing `preset = "MNQ"` in the
`[instrument]` table, and spells the instrument definition out field by field
when they do not. After this landing the SYMBOL selects the knobs: resolution
is TOTAL over strings, three-step - the operator's explicit choice beats a
preset whose name matches the symbol, which beats THE DEFAULT PRESET - and
`InstrumentDef` stops being a config entry and becomes the derived output of
that resolution. `[instrument]` shrinks to a symbol plus an optional overlay.
No string is refused for wanting a preset.

## 2. Scope, and the stopping rule

IN SCOPE: piece 1 (preset selection by symbol lookup, plus designating the
default preset) and piece 3 (`InstrumentDef` derived, and its downstream)
of the fourteen-piece inventory. These are one landing because piece 1 has no
observable effect without piece 3: a symbol that selects a bundle but cannot
supply the definition still needs the definition spelled out.

EXPLICITLY OUT OF SCOPE, and not deferral - each is its own numbered piece:

- Piece 2, config stops declaring the instrument (default knobs plus
  PER-SYMBOL overrides, `[[instrument]]` accepted). This spec keeps ONE
  `[instrument]` table naming ONE boot symbol. The refusal of `[[instrument]]`
  stays and `a_config_naming_two_instruments_fails_to_parse` stays green.
- Piece 4, boot ordering. RULED by the owner 2026-08-15: slice 1 keeps a boot
  symbol, warmup and `INDEX` initialize from it unchanged. This spec obeys that
  ruling and moves nothing in `materialize_warmup`.
- Piece 5, `/trades` and `/quotes` unknown-symbol semantics (the 400 naming the
  served symbol). One run still serves one symbol here, so the existing
  behaviour is untouched.
- Pieces 6-12, the whole of slice 2: no `/ws` symbol carrier, no de-singling of
  `RunIndex`/`BOOT`, no symbol dimension in `RunSeeds`, no lazy engine
  registration, no per-boat clocks.
- Piece 13's consumer surface, except where `/instruments` mechanically reports
  the derived def (it already reports `profiles.instrument_defs()`; that call
  site does not change).

The blast radius is WIDER than an early draft of this document claimed, and the
correction is load-bearing enough to state up front: deleting
`InstrumentProfiles::defaults()` touches THIRTEEN call sites across FOUR crates,
`mogwai-lab` among them. The full disposition is the table in Brick C. The files
that change:

- `crates/mogwai-server/src/`: `config.rs`, `source.rs`, `serve.rs`,
  `fill_golden.rs`, `fills.rs`, `http.rs`, `run.rs`, `tape.rs`
- `crates/mogwai-cli/src/`: `gen.rs`, `measure.rs`, `stage_m_tier2.rs`,
  `arrival_control.rs`; and `crates/mogwai-cli/tests/parity12a.rs`
- `crates/mogwai-lab/src/arrival_control.rs`
- `crates/mogwai-data/src/lib.rs` (the version constant, Brick D)
- one committed golden, `mogwai.toml`, `docs/presets.md`, `docs/config.md`,
  `reference/architecture.md`

Nothing in `mogwai-engine`, `mogwai-protocol` or `mogwai-adapter` changes;
`mogwai-data` changes only by the version bump.

## 3. Survey of the ground

### 3.1 How a knob bundle is chosen today

`Config::load` (`config.rs`) reads the TOML into a `toml::Table`, and if an
`[instrument]` table exists rewrites it in place through
`resolve_instrument_table`. That function:

- refuses the pre-class flat `base`/`quote` keys with a message naming the
  replacement;
- returns the operator table UNCHANGED if it carries no `preset` key - so a
  preset-less config must spell out every mandatory field itself;
- otherwise resolves the named preset through `effective_preset` (recursive
  parent resolution, restated-key refusal, `instrument.override` replacement,
  provenance completeness validation), then refuses any operator key stated
  outside `instrument.override`, then applies the operator's overrides through
  `replace_dotted` (which refuses a path the preset does not set, with the one
  hardcoded exception for `generator.arrival`).

The result deserializes into `ConfiguredInstrument`, whose `symbol`, `class`,
`price_precision`, `size_precision`, `price_increment` and `size_increment` are
all MANDATORY and whose `deny_unknown_fields` is load-bearing (it is what turned
a typo'd `price_precison` from a silently defaulted precision into a boot
failure).

`preset_text` is the whole registry: an uppercasing `match` over three
`include_str!` bodies, `MNQ`, `MES`, `BTCUSDT`. `preset_document` exposes it to
`mogwai presets`.

### 3.2 How the definition reaches the run today

`build_instrument_profiles(&cfg)`:

- with NO `[instrument]` table, returns `InstrumentProfiles::defaults()`, which
  maps `mogwai_protocol::default_instruments()` (one hardcoded BTCUSDT def,
  price precision 2, size precision 8, `1e-2`/`1e-8`) through
  `source::default_profile`. That builds scalars from
  `GeneratorScalars::from_fingerprint_medians`, forces `modal_tick` and
  `price_decimals` off the def, installs `TopOfBookSizes::uncalibrated`, takes
  the fingerprint's session profile, and carries NO margin, fees or calendar.
  This is a SECOND, unnamed knob bundle living beside the three presets, and it
  is not the BTCUSDT preset.
- with an `[instrument]` table, returns exactly one profile from
  `profile_from_configured`, which validates the def, validates the
  margin/fee options, defaults or accepts the generator scalars, enforces
  `modal_tick == price_increment` and `price_decimals == price_precision`,
  enforces top-size representability, runs `scalars.validate`,
  `validate_size_grid`, the empirical/size diagnostics, `session.validate_for`
  and `calendar.validate`.

`profile_from_preset(name)` is the same pipeline fed by `effective_preset`, for
callers with no operator config. It ERRORS on an unknown name - and that error
is the offline half of the assumption this spec removes: `mogwai gen --symbol
FOOBAR` fails today (`gen.rs::resolve_profile` tries the built-in venue set
first, then `profile_from_preset`).

`serve_async` takes `profiles.instrument_defs().into_iter().next()` as THE
instrument, calls `refuse_unfunded_settlement` with it, hands its symbol to
`materialize_warmup` and its def to `Run::new`. The `.next()` collapse is
piece 8 of the design bullet and stays; after this landing the map provably
holds exactly one entry, which is a strict improvement on today (where the
no-config path builds the map from a `Vec` whose length is merely assumed to be
one).

`source::index` keys the process-global `INDEX` by symbol and refuses any other
symbol; `BOOT` carries `RunSeeds` with no symbol term. Neither is touched.

### 3.3 Facts that constrain the design

1. The committed default balances are `USDT = 1000000`, and
   `refuse_unfunded_settlement` HARD FAILS boot when the resolved def's
   settlement currency is unfunded. Any default preset settling in something
   other than USDT breaks every no-config checkout.
2. The MNQ and MES presets are FUTURES: they carry a session calendar, a margin
   table and a USD settlement currency. A future without a margin table is
   refused by `validate_instrument_options`.
3. The BTCUSDT preset is spot, fitted from trade-level Binance archives, and
   carries no session calendar - it is 24/7, so it makes no calendar claim
   about a symbol nobody fitted.
4. `default_profile`'s bundle is NOT the BTCUSDT preset's bundle. Adopting the
   preset as the default moves generated bytes on the no-config path.
5. `fill_golden.rs` renders its committed golden from
   `InstrumentProfiles::defaults()`, so it is the gate that will observe (4).
6. `mogwai_data::TAPE_PROTOCOL_VERSION` is 14, and its doc comment reserves 15
   for the protocol-12b mechanism landing.
7. A stale comment on `refuse_unfunded_settlement` claims
   `InstrumentProfiles::defaults()` "carries all three shipped presets while
   `serve` picks the first". It carries one hardcoded def and never touched the
   presets. LATERAL FINDING, corrected by Brick E below.

## 4. The target

### 4.1 The resolution, stated exactly

This is the AUTHORITATIVE API. An earlier draft declared three signatures that
did not typecheck against their own call sites - `base_bundle` returning a bare
`Table` while the caller wrote `?.0`, `base_bundle` taking `&str` while the
caller passed an absent symbol, and `profile_for_symbol` declared with one
argument and called with two. Every declaration below is the one to implement;
where the prose downstream disagrees, this block wins.

```rust
/// The preset every unmatched symbol is served under.
pub const DEFAULT_PRESET: &str = "BTCUSDT";

/// The bundle a symbol resolves to, before the operator's own keys, paired
/// with its provenance table exactly as `effective_preset` returns them.
/// TOTAL over strings: never returns an error for an unrecognized symbol.
/// `symbol` is `None` when the operator wrote no `[instrument]` table and no
/// `symbol` key; the chosen bundle's own symbol then stands.
fn base_bundle(symbol: Option<&str>, operator_preset: Option<&str>)
    -> anyhow::Result<(toml::Table, toml::Table)>;

/// The one resolved instrument this run serves, as a TOML table ready to
/// deserialize into `ConfiguredInstrument`. `operator` is the `[instrument]`
/// table as written, INCLUDING its `preset` key, which this function removes.
/// An empty table is the no-config case and is legal.
fn resolve_instrument(symbol: Option<&str>, operator: toml::Table)
    -> anyhow::Result<toml::Table>;
```

TWO public entry points, not one, because the two callers want different
things and an earlier draft conflated them:

```rust
/// The profile a SYMBOL resolves to, with no operator overlay. Total: an
/// unmatched symbol resolves through DEFAULT_PRESET under its own name.
/// This is the offline entry point - `gen.rs::resolve_profile` calls it.
pub fn profile_for_symbol(symbol: &str) -> anyhow::Result<source::InstrumentProfile>;

/// The profile a boot config resolves to: the symbol chooses the bundle and
/// the operator table overlays it. `symbol: None` plus an empty table is the
/// no-config run, which resolves to DEFAULT_PRESET under its own symbol.
/// `profile_for_symbol(s)` is exactly `profile_for_config(Some(s), Table::new())`
/// and must be implemented as that call, so the two cannot drift.
pub fn profile_for_config(symbol: Option<&str>, operator: toml::Table)
    -> anyhow::Result<source::InstrumentProfile>;
```

`base_bundle` picks ONE bundle, whole, by this precedence:

1. `operator_preset` - the operator wrote `preset = "X"`. An unknown `X` is
   still a hard error: the operator named something that does not exist, and
   the three-step is about SYMBOLS, not about silently forgiving a typo in an
   explicit selection.
2. `preset_text(symbol).is_some()` - the symbol names a shipped preset
   (case-insensitively, as `preset_text` already matches).
3. `DEFAULT_PRESET`.

Whole-bundle, not key-by-key merged, and this is a DECISION worth stating:
merging MNQ's future class over BTCUSDT's fitted spot generator scalars would
produce an instrument no corpus ever described, with a margin table attached to
a tick grid it was not fitted against. A preset is a coherent bundle; layer 2
of the precedence replaces layer 3 entirely.

`resolve_instrument` then, over the chosen bundle:

- applies the operator's `instrument.override` paths through the existing
  `replace_dotted`, preserving the refuse-an-unknown-path behaviour and the
  `generator.arrival` exception verbatim;
- applies the operator's remaining top-level keys as REPLACEMENTS, which is the
  behaviour change: today a top-level key beside a preset is refused with
  "preset key {key} must be stated under instrument.override". See 4.3.
- forces `symbol` to the requested symbol, LAST, so the bundle's own symbol
  never wins. `FOOBAR` resolved through BTCUSDT is a BTCUSDT-shaped tape called
  `FOOBAR`, exactly as the design bullet states.

### 4.2 `InstrumentDef` becomes derived

`ConfiguredInstrument` stays the deserialization target and keeps
`deny_unknown_fields` - it is now the shape of a RESOLVED table, never of what
the operator typed. `def()` is unchanged, and so is every downstream consumer:
the derivation happens before deserialization, in `toml::Table` space, which is
where the existing preset machinery already works.

`build_instrument_profiles` becomes total:

```rust
pub fn build_instrument_profiles(cfg: &Config) -> anyhow::Result<source::InstrumentProfiles> {
    let profile = profile_for_config(cfg.instrument_symbol(), cfg.instrument_table())?;
    Ok(source::InstrumentProfiles::from_profiles(vec![profile]))
}
```

with no `else` branch and no `InstrumentProfiles::defaults()` call. There is one
path, and a no-config run takes the same one an operator config takes, with
`DEFAULT_PRESET` and its own symbol.

`profile_from_preset(name)` keeps its signature and meaning (an EXPLICIT preset
by name, still erroring on an unknown name - the offline commands that call it
with a literal `"MNQ"` want the error). The two new siblings are
`profile_for_symbol` and `profile_for_config`, declared in 4.1.

`source::InstrumentProfiles::defaults()` and `source::default_profile` are
DELETED. `mogwai_protocol::default_instruments()` survives - `mogwai-engine`
uses it for its own seeding and tests - but the server stops calling it, which
is the point: the venue's default is now a named, provenanced preset rather than
a hardcoded literal in the wire crate.

### 4.3 The one behaviour change beyond the precedence

Today, a config carrying `preset = "MNQ"` and a top-level `price_precision`
refuses boot and tells you to write it under `instrument.override`. That rule
exists to stop a silent last-writer-wins between two things that both look like
definitions. Under symbol-selects-the-preset there is no longer any such thing
as a config without a bundle underneath it, so the rule would make EVERY
top-level key illegal in EVERY config - the operator could never set anything
directly, which contradicts the design bullet's third precedence layer ("knobs
the operator set explicitly").

The resolution: a top-level key is a legal explicit override and wins, and it is
LOGGED at boot exactly as `replace_dotted` logs an override (path, bundle value,
operator value). `instrument.override` survives unchanged for the DOTTED paths
it is the only way to reach (`generator.mean_event_duration_s`), and remains the
documented way to state a deep override. The refusal that is retained: an
override path the bundle does not set is still an error, so a typo is still
loud. `deny_unknown_fields` on `ConfiguredInstrument` still catches a typo at
the top level.

### 4.4 What a top-level override can and cannot reach

`replace_dotted` REFUSES a path the chosen bundle does not already set. That is
the typo guard and it is kept, but it has a consequence 4.3 skipped: an operator
key is an override of an EXISTING knob, never an addition of a new one. Over the
BTCUSDT default bundle - spot, no calendar, no margin table - `margin`, `fees`
and `session.calendar` are therefore UNREACHABLE from the operator's config.
There is no way to overlay a future onto the default bundle.

RULED: that is the intended behaviour for slice 1 and is not a defect to fix
here. The bundle is coherent by construction (4.1); a config that bolts a margin
table onto spot-fitted generator scalars produces exactly the incoherent
instrument whole-bundle selection exists to prevent. An operator who wants a
future names a futures preset explicitly or ships their own. The refusal message
must SAY this rather than only naming the path, so the operator is not left
guessing: when the refused path is one the bundle structurally lacks, the error
names the chosen bundle and says the knob is not part of it.

`generator.arrival` keeps its one hardcoded exception verbatim.

### 4.5 Provenance under a forced symbol

The presets carry a `[provenance]` table and `effective_preset` validates its
completeness. BTCUSDT declares
`"symbol" = { kind = "fitted", corpus = "Binance trades", window = "30 days" }`.
Forcing `symbol = "FOOBAR"` onto that bundle leaves a run claiming FOOBAR's
symbol was fitted from Binance trades, which is false.

RULED: the provenance table is not carried into the run's `InstrumentProfile`
for the `symbol` key - `resolve_instrument` DISCARDS the provenance table, as
`resolve_instrument_table` already does today, so nothing downstream reads a
false claim. What must not happen is a later piece resurfacing provenance on
`/instruments` without revisiting this: when piece 13 exposes provenance,
`symbol` provenance for a resolved-through-default symbol is `declared`, with
the rationale that the symbol is the client's string and only the KNOBS carry a
corpus. Recorded here so the question is not rediscovered.

Two things this is NOT. The completeness check cannot be tripped by an operator
key: `validate_provenance` runs INSIDE `effective_preset`, before any overlay is
applied, and the overlay never touches the provenance table. And the knobs'
provenance stays true under a forced symbol - the tape really is the fitted
BTCUSDT tape.

### 4.6 The `Config::load` validation boundary moves, deliberately

`Config::load` carries an explicit promise in its own comment: it validates
"here, not at the call site, so a validated `Config` is the only kind `load`
ever hands out - a future second consumer cannot forget the check". Today it
honours that for the instrument by resolving the table INLINE, so a typo'd
`price_precison` fails at load. Making `Config.instrument` a raw `toml::Table`
defers `deny_unknown_fields` and every profile validation to
`build_instrument_profiles`, which weakens that promise. `Config.instrument` is
a `pub` field, so this is a cross-crate change to `mogwai-cli` and
`mogwai-lab`.

RULED: accept the move, and pay for it in three places rather than pretending it
did not happen.

1. Rewrite the `Config::load` comment. It still validates every knob it owns;
   what it no longer promises is a RESOLVED instrument. Name
   `build_instrument_profiles` as where that happens and why (the resolution
   needs the boot symbol, which is the same reason the clock and instrument
   validations already live with their builders).
2. Audit the two existing consumers, both of which only branch on
   `cfg.instrument.is_none()` and so keep compiling while changing meaning:
   - `gen.rs::profile_from_config` bails when there is no `[instrument]` table,
     because a scratch profile must configure one. Under the new model an absent
     table resolves to DEFAULT_PRESET, which is exactly the silent-ignore this
     guard exists to prevent. The guard STAYS, and its message is reworded from
     "the built-in default venue would ignore your scalars" to "an absent table
     resolves to DEFAULT_PRESET and would ignore your scalars".
   - `mogwai-lab/src/fit/walk.rs` does the same branch; give it the same
     treatment and check its message.
3. Name the type honestly. `Config.instrument` is documented as THE OPERATOR'S
   TABLE AS WRITTEN, unresolved and unvalidated, with a pointer to
   `build_instrument_profiles`. No new raw/resolved type pair - that is a
   heavier refactor than slice 1 earns, and with exactly two consumers the audit
   above is the cheaper closure.

## 5. The bricks

Each brick is a coherent landing. The suite is green at every boundary.

### Brick A - the registry and the default

In `config.rs`:

- Add `pub const DEFAULT_PRESET: &str = "BTCUSDT";` with a doc comment stating
  the three grounds from 3.3: it is spot (no calendar claim about an unfitted
  symbol), it settles in USDT which the committed `[balances]` funds, and it is
  fitted from trade-level archives rather than derived from bars.
- Collapse the registry to ONE static table. An earlier draft said to add
  `preset_names()` and "make `preset_text` match over it", which cannot be
  written: Rust match patterns cannot be derived from array elements or from a
  function result, so that form would leave the names spelled twice and defeat
  its own purpose. The implementable form:

  ```rust
  /// The shipped presets. The ONE spelling of the registry: name, text.
  const PRESETS: [(&str, &str); 3] = [
      ("MNQ", include_str!("../presets/mnq.toml")),
      ("MES", include_str!("../presets/mes.toml")),
      ("BTCUSDT", include_str!("../presets/btcusdt.toml")),
  ];

  pub fn preset_names() -> [&'static str; 3];       // maps PRESETS
  fn preset_text(name: &str) -> Option<&'static str>; // case-insensitive search
  ```

  `preset_text` keeps its exact current semantics: `name.to_ascii_uppercase()`
  compared against each entry's name, `None` for no match. The uppercasing is
  load-bearing for 4.1's case-insensitive symbol match and for the
  `a_lowercase_symbol_matches_its_preset` test.
- `mogwai-cli/src/tick_composition.rs` has its own `PRESETS: [&str; 3]` used at
  six sites for iteration and `.len()`; point it at `preset_names()`. Both uses
  survive the change from a constant to a function result, but every site needs
  the call added. `mogwai-lab`'s `RETIRED_PRESETS` is a different list and is
  left alone.

GATE: `brokkr check`. No behaviour moves; this brick only adds names.

### Brick B - `base_bundle` and the total resolution

In `config.rs`, replace `resolve_instrument_table` with `resolve_instrument` per
4.1. Concretely:

1. Keep the `base`/`quote` refusal at the top, unchanged.
2. `let operator_preset = operator.remove("preset")`, string-typed as today.
3. `let symbol: Option<&str> = operator.get("symbol")` as a string, falling back
   to the `symbol` argument; ABSENT in both means the chosen bundle's own symbol
   (so a no-config run is `BTCUSDT`, byte-compatible in naming with today).
   This is why `base_bundle` and `resolve_instrument` both take `Option<&str>`
   and not `&str`.
4. `let (mut merged, _provenance) = base_bundle(symbol, operator_preset)?;` -
   the provenance table is discarded here exactly as `resolve_instrument_table`
   discards it today. See 4.5 for why that discard is the ruling, not an
   oversight.
5. Apply `operator["override"]` paths through `replace_dotted`, keeping the
   `generator.arrival` exception verbatim.
6. Apply the operator's remaining top-level keys as replacements, each through
   `replace_dotted` so an unknown key is still refused by name and each
   application is logged. This restricts the operator to knobs the chosen bundle
   already sets - see 4.4, which rules that restriction IN and requires the
   refusal message to name the bundle. `symbol` is exempt from the
   path-must-exist rule and is applied last and unconditionally.

`Config::load` keeps its shape but must now resolve even when no `[instrument]`
table exists, because the no-config path must reach `DEFAULT_PRESET`. The
cleanest form: `Config::load` leaves `cfg.instrument` as `Option<toml::Table>`
raw, and `build_instrument_profiles` does the resolution. That is a type change
on `Config.instrument`, from `Option<ConfiguredInstrument>` to
`Option<toml::Table>` - legal, pre-1.0, and it puts the boundary where it
belongs: `Config` carries what the operator WROTE, `build_instrument_profiles`
derives what the run SERVES.

Two accessors on `Config` for the split:

```rust
pub(crate) fn instrument_symbol(&self) -> Option<&str>;
pub(crate) fn instrument_table(&self) -> toml::Table; // empty when absent
```

GATES:

- `brokkr check`
- New unit tests in `config.rs`, each named for what it pins:
  - `an_unmatched_symbol_resolves_through_the_default_preset` - resolve
    `FOOBAR`, assert the resolved def's symbol is `FOOBAR` and its class,
    increments and precisions equal the BTCUSDT preset's.
  - `a_symbol_naming_a_preset_selects_it` - resolve `MNQ` with no operator
    preset, assert the def matches `profile_from_preset("MNQ")`'s def exactly.
  - `an_operator_preset_beats_a_matching_symbol` - symbol `MNQ`, `preset =
    "BTCUSDT"`, assert the resolved class is the BTCUSDT preset's spot class and
    the symbol is still `MNQ`.
  - `a_top_level_key_overrides_the_resolved_bundle` - symbol `MNQ`, and the
    overridden key is one that is INDEPENDENTLY settable. NOT `price_precision`:
    an earlier draft named it, and that config cannot boot.
    `profile_from_configured` enforces `generator.price_decimals ==
    price_precision` and `generator.modal_tick == price_increment`, so a
    lone `price_precision = 3` over MNQ resolves as a table and then hard-fails
    profile construction - the test would either prove nothing about boot or
    fail outright. Use an uncoupled field, and assert through a BUILT PROFILE
    rather than the intermediate table, so the test proves the advertised boot
    behaviour.
  - `a_top_level_override_of_a_coupled_key_must_state_both` - the coupling made
    explicit: `price_precision = 3` alone is refused with the existing
    `generator.price_decimals must equal price_precision` message, while
    `price_precision = 3` together with `generator.price_decimals = 3` under
    `instrument.override` boots. Slice 1 does NOT add normalization that derives
    the generator fields from the def; the coupling is stated and pinned, and
    deriving it is a separate decision for a later piece.
  - `an_override_path_the_bundle_does_not_set_is_still_refused` - the typo
    guard, asserting the error names the path.
  - `an_explicit_unknown_preset_is_still_an_error` - `preset = "NOPE"` fails,
    while `symbol = "NOPE"` succeeds. One test, both halves, because the
    asymmetry is the decision.
  - `a_lowercase_symbol_matches_its_preset` - `mnq` resolves to the MNQ bundle
    (matching `preset_text`'s existing uppercasing) but keeps the symbol string
    `mnq` verbatim, because the symbol is the client's string.

BITE-CHECK each of these per the standing rule: revert the production change as
a text edit, observe the named failure, restore it as a text edit. Never
`git checkout --`.

### Brick C - `InstrumentDef` derived, defaults path deleted

- `ConfiguredInstrument` fields stay mandatory (the resolved table always
  supplies them; a bundle that does not is a broken preset and should fail
  loudly).
- Add `profile_for_symbol` and `profile_for_config` per 4.1, the former
  implemented as a call to the latter.
- Rewrite `build_instrument_profiles` per 4.2, with no `defaults()` branch.
- Delete `source::InstrumentProfiles::defaults` and `source::default_profile`,
  and the now-unused `default_instruments` import in `source.rs`.

THE CALLER DISPOSITION. Deleting `defaults()` is not compile cleanup: it is a
decision per call site, and the sites SPLIT CLEANLY IN TWO by which key they
ask for. `defaults()` only ever contains BTCUSDT, so every `.get("MNQ")` is a
provably dead branch whose `map_or_else`/`unwrap_or_else` fallback to
`profile_from_preset("MNQ")` is the only live arm. Those migrations move no
byte. Every `.get("BTCUSDT")` and every whole-map use, by contrast, moves from
the median-derived bundle to the fitted preset and DOES move bytes.

| Call site | Key | Disposition |
|---|---|---|
| `mogwai-cli/src/measure.rs` | MNQ | dead branch: collapse to `profile_from_preset("MNQ")`. Byte-neutral. |
| `mogwai-cli/src/stage_m_tier2.rs` | MNQ | same collapse. Byte-neutral. |
| `mogwai-cli/tests/parity12a.rs` | MNQ | same collapse. Byte-neutral - the oracle does not move. |
| `mogwai-lab/src/arrival_control.rs` `default_mnq_profile` | MNQ | same collapse, and rewrite the two doc comments at the function and at `control_generated_pass` that cite "`defaults()` with the preset as the fallback" as a contract pin against `run_final_walk`. That contract survives; its spelling does not. |
| `mogwai-cli/src/gen.rs` `resolve_profile` | any | becomes one `profile_for_symbol` call. |
| `mogwai-cli/src/gen.rs` `gen_reproduces_the_default_profile_walk_via_build_source` | BTCUSDT | MOVES BYTES. Repoint at `profile_for_symbol("BTCUSDT")`; any pinned value in the test re-blesses. |
| `mogwai-server/src/fill_golden.rs` | whole map | MOVES BYTES. The golden re-bless below. Also uses `default_instruments()` directly twice - repoint both at the resolved def. |
| `mogwai-server/src/fills.rs` `test_profiles` | whole map | MOVES BYTES. Shared fixture: check every test that reaches it. |
| `mogwai-server/src/tape.rs` (three fixtures) | whole map | MOVES BYTES if any assertion is pinned to generated output. |
| `mogwai-server/src/run.rs` | whole map | same. |
| `mogwai-server/src/http.rs` | whole map | same. |

The MNQ collapses are a FREEBIE worth taking: they delete a dead branch and
simplify four files. Do them first, as their own compile-green step, so the
byte-moving half is reviewed alone.

`defaults()` is infallible while `profile_for_symbol` returns `Result`, so every
migrated fixture gains an `expect`. Give each one a message naming the preset,
not a bare `unwrap`.

- `fill_golden.rs`: point it at
  `InstrumentProfiles::from_profiles(vec![config::profile_for_symbol("BTCUSDT")?])`
  and state in its module doc that the golden is now rendered over the shipped
  BTCUSDT PRESET rather than the retired median-derived default.
- `gen.rs::resolve_profile` loses its two-step (`defaults()` then
  `profile_from_preset`) and becomes one call to `profile_for_symbol`. Its doc
  comment - "a built-in venue symbol first, then an embedded preset ... keeps
  `--symbol BTCUSDT` byte-identical" - is now false and is replaced by a
  statement that any string resolves, and that `--symbol BTCUSDT` now renders
  the preset. Its `with_context` "unknown symbol" message is DEAD under a total
  resolution and is deleted, not reworded.
- `gen.rs::profile_from_config` and `mogwai-lab/src/fit/walk.rs` keep their
  `cfg.instrument.is_none()` guards with reworded messages, per 4.6 item 2.

GATES:

- `brokkr check --all` (the deletion touches several crates; do not let the
  changed-files scope hide a caller).
- The golden WILL move, because the default bundle changed from the
  median-derived scalars to the fitted BTCUSDT preset. RE-BLESS knowingly:
  delete `crates/mogwai-server/tests/golden/fill_distribution.json`, run
  `brokkr test -p mogwai-server fill_distribution_matches_the_golden` (which
  writes it and panics by design), inspect the regenerated artifact, rerun the
  same command to confirm green, and commit the artifact IN THE SAME CHANGE.
  Do not widen the comparison into a tolerance.
- THE GOLDEN IS NOT THE ONLY ARTIFACT THAT MOVES. `fills.rs::test_profiles`,
  the three `tape.rs` fixtures, `run.rs`, `http.rs` and
  `gen_reproduces_the_default_profile_walk_via_build_source` all generate from
  the median-derived BTCUSDT profile today. Before repointing them, enumerate
  every assertion that reaches those fixtures and decide it explicitly: a test
  asserting on STRUCTURE (a def is present, a symbol matches) is unaffected; a
  test asserting on GENERATED VALUES re-blesses knowingly in this same change.
  Run `brokkr check --all` after the repoint and read every failure as a
  question, not as a number to update.
- New test `a_no_config_run_serves_the_default_preset` in `config.rs`: build
  profiles from `Config::default()` and assert the resulting profile equals
  `profile_from_preset(DEFAULT_PRESET)`'s profile. This is the test that would
  have caught the two-bundles-in-the-tree situation the survey found.
- `brokkr test -p mogwai-cli a_scratch_config_profile_matches_the_served_profile`
  - it asserts a `preset = "MNQ"` scratch config resolves identically to the
  embedded preset, which must stay true through the rewrite.

### Brick D - the tape protocol version

The default bundle moved, so a no-config run's generated bytes move. Bump
`mogwai_data::TAPE_PROTOCOL_VERSION`, and extend its doc comment with the
reason in the same voice as its neighbours: "15 is the arrival-mechanism
reservation; 16 makes the shipped BTCUSDT preset the default bundle, so a
no-config run's tape is the fitted one rather than the fingerprint-median one."

Take 16, not 15, because the doc comment on 14 explicitly reserves 15 for the
12b mechanism landing. AT LANDING TIME, re-read that comment: if 12b has landed
and taken 15, 16 is correct as written; if 12b has been retired or renumbered,
take the next unclaimed integer and update the reservation sentence. Do not
discover this at commit time - it is one grep of
`crates/mogwai-data/src/lib.rs`.

ONE EXACT-VERSION GATE IS NOT AN ORDINARY ASSERTION AND MUST BE MIGRATED
DELIBERATELY. `crates/mogwai-cli/src/arrival_control.rs` computes
`let version_ok = mogwai_data::TAPE_PROTOCOL_VERSION == 14;` inside
`b1_supporting_check`, and that boolean is ANDed into protocol-12b's B1
frozen-path verdict - it is production-facing control logic, not a test. Its doc
comment names 14 as "the pre-mechanism identity" and recites the 13-then-14
history. Bumping the constant flips that verdict to failing.

The migration, and it is a DECISION the implementer must make rather than a
mechanical edit: the check's purpose is that the tape identity has not moved
since the 12b baseline. Making the BTCUSDT preset the default moves it, so B1's
baseline genuinely no longer holds and papering over that with `== 16` would be
a false pass. Either

- re-baseline: change the constant to the new version AND update the doc
  comment's history sentence, which asserts that this landing is an acceptable
  new B1 baseline; or
- if 12b's control run is still open against the old baseline, LAND SLICE 1
  AFTER IT, or park B1's verdict explicitly with a comment saying why.

Whichever is chosen, state it in the commit message. Do not discover this at
`brokkr check` time.

GATE: `brokkr check`. Any test asserting the constant fails by name and is
updated in the same change. `crates/mogwai-cli/tests/lifecycle.rs` formats the
constant into a version-banner assertion and will need no edit, but confirm it.

### Brick E - the prose, landed WITH the code

Per piece 14 of the inventory, the durable documents are part of the landing,
not a follow-up. Bundle them into the code commits (never a markdown-only
commit).

- `docs/presets.md`: the whole "Selecting a preset in your config" section is
  rewritten. `preset = "MNQ"` remains legal and remains the way to say "serve
  the MNQ bundle under a different symbol", but the headline becomes: name your
  symbol, and if a preset carries that name you get it. Add the three-step
  precedence explicitly. The "Presets are a convenience, not a requirement"
  section is now understated and becomes a statement of the model: any string is
  served, unmatched strings get the default preset's knobs under their own name.
  Name `BTCUSDT` as the default and say why.
- `docs/config.md`: the `[instrument]` table is documented as an OVERLAY - the
  symbol plus whatever the operator wants to differ - not a definition. State
  that top-level keys are now legal overrides and are logged.
- `reference/architecture.md`: the instrument-resolution paragraph states that
  `InstrumentDef` is derived from the symbol and the config, that there is one
  resolution path and no second default bundle, and that the venue refuses no
  symbol.
- `mogwai.toml`: the `[instrument]` comment currently says "Omitted, the
  built-in BTCUSDT profile applies". Replace with the resolution rule, and show
  the new minimal form (`symbol = "FOOBAR"`).
- `mogwai-lab/src/arrival_control.rs`: the two doc comments (on
  `control_generated_pass` and on `default_mnq_profile`) cite
  "`InstrumentProfiles::defaults()` with the preset as the fallback" as the
  contract pin against `mogwai_cli::measure::run_final_walk`. The pin survives -
  both sides still resolve the MNQ preset - but the spelling must be rewritten
  or the comment cites a deleted function.
- `config.rs`: the `Config::load` validation-boundary comment, per 4.6 item 1,
  and the `Config.instrument` field doc, per 4.6 item 3.
- `config.rs`: correct the stale `refuse_unfunded_settlement` doc comment
  identified in 3.3 item 7 - `defaults()` never carried three presets, and after
  Brick C it does not exist. The comment's actual load-bearing point (check the
  SERVED instrument's currency, not the whole table) stays.
- `notes/todo.md`: mark pieces 1 and 3 landed in the fourteen-piece inventory,
  and strike items 1 and 3 from the design bullet's change list. Leave items
  2, 4, 5, 6, 7, 8 standing.

GATE: `brokkr check` (gremlins runs over markdown too - no em-dashes, no fancy
quotes), plus a read of `docs/presets.md` against the shipped behaviour.

### Brick F - the end-to-end proof

The unit tests prove resolution; this proves a run SERVES an unfitted symbol.

- Add `crates/mogwai-cli/tests/configs/unmatched-symbol.toml`: an
  `[instrument]` table containing only `symbol = "FOOBAR"`, plus whatever the
  sibling configs in that directory set for pacing.
- Add an integration test in `crates/mogwai-cli/tests/serving.rs` named
  `a_symbol_no_preset_covers_is_served_under_the_default_bundle`: boot the
  server on that config, `GET /instruments`, assert exactly one def, symbol
  `FOOBAR`, class/increments equal to the BTCUSDT preset's, then drain the `/ws`
  socket to a deadline and assert trades arrive under the symbol `FOOBAR`.
  The drain-to-a-deadline discipline is mandatory: a `/ws` socket is attached to
  the live tape on upgrade, so the test may never assert on THE NEXT frame.

GATES:

- `brokkr test -p mogwai-cli a_symbol_no_preset_covers_is_served_under_the_default_bundle --debug`
  (a subprocess-lifecycle test, so the dev profile is the right build).
- `brokkr run mogwai -- serve` in one shell and `python3 scripts/smoke.py` in
  another, to prove the live WS and control plane path is untouched.
- `brokkr check --gate` before the commit if anything in `mogwai-adapter` was
  touched. Nothing here should touch it; run it anyway if `git diff --name-only`
  says otherwise.

## 6. Keep/revert

The unit of keep-or-revert is Bricks B, C and D together, plus the golden
re-bless: they are one semantic change and the suite is not meaningfully green
in between (Brick C without D ships a moved tape under an unchanged version;
Brick D without C bumps a version nothing moved). Bricks A, E and F are
independently keepable.

The revert verdict is read off two things: the re-blessed golden must show a
plausible fitted-BTCUSDT distribution rather than a degenerate one (a cell whose
`filled` count collapses to zero or saturates at `samples` is a red flag worth
stopping on), and `scripts/smoke.py` must pass unchanged. There is no
performance dimension here - resolution happens once at boot, in TOML space,
and no hot path is touched.

## 7. Decisions this document makes, collected

1. THE DEFAULT PRESET IS `BTCUSDT`. Spot (so no calendar or margin claim is
   imposed on an unfitted symbol), settling in the currency the committed
   `[balances]` funds, and fitted from trade-level archives.
2. The bundle is chosen WHOLE, not merged key-by-key across presets.
3. An explicitly named unknown preset is still an error; an unmatched SYMBOL is
   not. The asymmetry is deliberate and is pinned by a test.
4. Preset name matching stays case-insensitive; the served symbol string stays
   verbatim.
5. Top-level `[instrument]` keys become legal explicit overrides, logged.
   `instrument.override` survives for dotted paths and keeps its
   unknown-path refusal.
6. `Config.instrument` becomes the raw `toml::Table` the operator wrote;
   derivation moves to `build_instrument_profiles`.
7. `source::default_profile` and `InstrumentProfiles::defaults` are deleted -
   one resolution path, not two.
8. `TAPE_PROTOCOL_VERSION` goes to 16, re-checked against the 12b reservation at
   landing time, and `arrival_control.rs`'s `== 14` B1 gate is re-baselined or
   parked with a stated reason (Brick D).
9. A top-level override reaches only knobs the chosen bundle already sets; a
   future cannot be overlaid onto the spot default. Ruled in, with a refusal
   message that names the bundle (4.4).
10. The provenance table stays discarded at resolution, so no false fitted claim
    reaches the run; the `declared`-provenance answer is recorded for piece 13
    (4.5).
11. `Config::load` stops promising a resolved instrument. Both existing
    `cfg.instrument.is_none()` consumers keep their guards with reworded
    messages; no raw/resolved type split (4.6).
12. The def/generator couplings (`price_decimals == price_precision`,
    `modal_tick == price_increment`) are PINNED, not normalized away. A coupled
    override must state both halves.
13. The preset registry becomes one static `(name, text)` table searched
    case-insensitively, not a `match` plus a parallel name array.

## 8. Review disposition

Two independent reviews (`notes/slice1-spec-review-R1.md`, Claude; and
`notes/slice1-spec-review-R2.md`, codex gpt-5.6-sol) were validated against the
source. Every finding either landed above or is rejected here with a reason. The
two agreed on three findings - the understated blast radius, the
`profile_for_symbol` arity contradiction, and the non-bootable
`price_precision` test - which are folded once each.

Landed: blast radius and caller disposition (R1.1, R1.2, R2.1, into section 2
and Brick C); the API signatures (R1.3, R1.4, R2.2, into 4.1); the coupled-key
test (R1.5, R2.3, into Brick B); top-level override reachability (R1.6, into
4.4); the false symbol provenance (R1.7 first half, into 4.5); the validation
boundary (R1.8, R2.4, into 4.6); the dead `.get("MNQ")` branch and the
infallible-to-`Result` unwrap churn and the `arrival_control` doc comments
(R1.9, into Brick C and Brick E); the unimplementable `preset_names()` match
(R2.5, into Brick A); the `== 14` B1 gate (R2.6, into Brick D).

REJECTED:

- R1.7, second half - that an operator-set top-level key "may trip the
  provenance completeness check". It cannot. `validate_provenance` runs inside
  `effective_preset`, strictly before any overlay is applied, and the overlay
  never touches the provenance table. The first half of that finding is real
  and landed in 4.5.
- R2.1's framing that `parity12a.rs` "intentionally expresses the old
  built-in-first resolution" and that migrating it "could move unrelated oracle
  behavior". The requirement - a caller-by-caller disposition - is right and is
  met, but this specific worry is unfounded: `defaults()` contains only
  BTCUSDT, so `.get("MNQ")` is `None` at every one of its four call sites and
  the preset fallback is the only arm that has ever executed. The migration is
  provably byte-neutral there, which is why Brick C splits the sites by key.
- R2.1's citation that the spec "explicitly says `mogwai-data` does not
  change". The omission was `mogwai-lab`, not `mogwai-data`; `mogwai-data` is
  genuinely untouched except by the Brick D version bump, which the spec always
  called for. Recorded so the corrected radius is not read as broader than it
  is.
