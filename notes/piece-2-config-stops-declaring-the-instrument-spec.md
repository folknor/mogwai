# Implementation spec: piece 2, config stops declaring the instrument

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/todo.md`: piece 2 of "Landing the grand design: fourteen pieces", and
the design bullets it points at under Open issues - "THE SYMBOL IS A REQUEST
PARAMETER, NOT AN IDENTITY THE VENUE OWNS" item 2, "SYMBOL RESOLUTION IS TOTAL,
AND THE DEFAULT PRESET IS THE SHAPE CONTRACT", and the piece-4 owner ruling of
2026-08-15 that slice 1 KEEPS A BOOT SYMBOL.

## 1. What this piece is, in one paragraph

`Config` today carries ONE `[instrument]` table whose doc says "the one
instrument this run serves". That table conflates three separate things: WHICH
symbol the run serves, WHAT KNOBS the operator wants applied, and WHICH SYMBOL
those knobs are about. This piece splits them. After it, the config names a
boot symbol as a scalar, carries a default knob overlay that applies to every
symbol, and carries per-symbol overlays keyed by symbol string. Nothing in the
config declares an instrument any more: the instrument is what a symbol
RESOLVES to, and the config only supplies overlays for that resolution to read.
The run still serves exactly one symbol - that is the piece-4 ruling, and this
spec does not touch it.

The parse pin `a_config_naming_two_instruments_fails_to_parse` is deleted
because the sentence it pins ("a run serves ONE instrument, so `[instrument]`
is a table") stops being the config's claim.

## 2. Survey of the ground

Everything below was read in the tree on 2026-08-15.

### The config type

`crates/mogwai-server/src/config.rs`:

- `Config.instrument: Option<toml::Table>` - `#[serde(rename = "instrument")]`,
  public field, "the operator's instrument table exactly as written, unresolved
  and unvalidated". `Config` itself is `#[serde(default, deny_unknown_fields)]`.
- `Config::instrument_symbol(&self) -> Option<&str>` - reads `instrument.symbol`.
- `Config::instrument_table(&self) -> toml::Table` - clones `instrument` or empty.
- `build_instrument_profiles(cfg)` - the ONE call site of both accessors:
  `profile_for_config(cfg.instrument_symbol(), cfg.instrument_table())`, wrapped
  into a one-element `InstrumentProfiles`.

### The resolution path, which is already the right shape

- `bundle_name(symbol, operator_preset)` - the one spelling of the three-step
  precedence: operator `preset`, then a preset named by the symbol, then
  `DEFAULT_PRESET` ("BTCUSDT"). TOTAL over symbol strings.
- `base_bundle` -> `effective_preset(name)` - preset inheritance (MES over MNQ),
  `override` dotted paths, provenance completeness.
- `resolve_instrument(symbol, operator)` - the function this piece restructures.
  It currently, in order: refuses the retired flat `base`/`quote` keys; removes
  and reads `preset`; reads and removes `symbol`, falling back to the passed-in
  symbol; picks the bundle; applies `[instrument.override]` dotted paths (with
  the protocol-12b `generator.arrival` insertion exception); applies remaining
  top-level operator keys (a dotted key or a key the bundle already sets is a
  strict REPLACE via `replace_dotted_for_bundle`, an unknown non-dotted key is a
  logged ADDITION); re-inserts the requested symbol.
- `profile_for_config(symbol, operator)` -> `ConfiguredInstrument` (deny-unknown)
  -> `profile_from_configured` (def validation, generator scalar defaulting and
  agreement checks, session/calendar validation).
- `profile_for_symbol(symbol)` = `profile_for_config(Some(symbol), empty)`.
  Twelve call sites across `mogwai-server`, `mogwai-cli` and tests use
  `profile_for_symbol` / `profile_from_preset`; NONE of them goes through
  `Config`, so none is affected by this piece.

### Everything that reads the field or the file shape

- `crates/mogwai-server/src/serve.rs` - calls `build_instrument_profiles`, then
  `instrument_defs().into_iter().next()` for the single def. UNTOUCHED here;
  the `.next()` collapse is piece 7.
- `crates/mogwai-cli/src/gen.rs` `profile_from_config` - refuses a scratch
  config with `cfg.instrument.is_none()`, then asserts exactly one resolved def.
- `crates/mogwai-lab/src/fit/walk.rs` `profile_from_config` - the same two
  checks, same wording.
- The scratch-config WRITERS, which emit config text rather than read it.
  `fit/walk.rs` builds `["[instrument]", "preset = \"MNQ\"",
  "[instrument.override]", ...]` and a test in the same file asserts that exact
  prefix string; `gen.rs`'s scratch helpers emit the same preset-only shape,
  once as a literal and once from a closure. All of them are preset-only
  overlays carrying no `symbol`, so they stay VALID under the new shape and
  migrate by doing nothing - but an implementer has to look at them to know
  that, which is why they are surveyed here. `gen.rs` also has a test asserting
  its refusal message contains the literal `[instrument]`, so the reworded
  refusal in brick A must keep that substring or that test is rewritten with it.
- `mogwai.toml` (repo root) - commented `[instrument]` / `symbol = "FOOBAR"`.
- `crates/mogwai-cli/tests/configs/`: `mnq.toml` (`[instrument] symbol = "MNQ"`
  plus class, margin, fees, generator, session, calendar);
  `fees.toml` (the SAME shape as `mnq.toml`, `[instrument] symbol = "MNQ"` with
  class, margin, `fees.maker`, `fees.taker`, generator and calendar - it drives
  the smoke test's fees arm and a `config.rs` test comment names it by path);
  `unmatched-symbol.toml` (`[instrument] symbol = "FOOBAR"`);
  `mnq-preset.toml` and `arrival-fault.toml` and `no-warmup.toml`
  (`[instrument] preset = "..."`, and `arrival-fault.toml` also
  `[instrument.override]` with the `generator.arrival` path).
- `docs/config.md` - "One optional `[instrument]` table overlays the
  symbol-resolved bundle ... `[[instrument]]` is not accepted", plus the
  `[instrument.class]`, `[instrument.margin]`, `[instrument.fees.*]`,
  `[instrument.calendar]`, `[instrument.override]` sections.
- `docs/presets.md` - refers to the operator `[instrument]` overlay.

### What is NOT ground for this piece

The sweeper is already symbol-keyed. `RunIndex`, `BOOT`, seed derivation, the
`/ws` symbol carrier, lazy engine registration, the `MarketReadingCache` single
entry and `last_swept_ns` are slice-2 pieces (6 through 12). The `/trades` and
`/quotes` unknown-symbol 400 is piece 5. `refuse_unfunded_settlement` keeps its
signature and its per-def question; what changes here is only that 3.5 calls it
once per CONFIGURED shape instead of once for the boot def.

## 3. The target artifact

### 3.1 The file shape

```toml
# The one symbol this run serves. Slice 1 only: slice 2 makes the symbol a
# request parameter and this key becomes the default for a request that
# carries none. Absent, the default preset's own symbol stands (BTCUSDT).
symbol = "MNQ"

# Default knobs. Applied to EVERY symbol the run resolves, on top of whatever
# preset that symbol selects. Declares no instrument and names no symbol.
[instrument]
fees = { ... }

# Per-symbol knobs. Same overlay shape as [instrument], including its own
# `preset` and `override` sub-table. Wins over [instrument] key by key.
[symbols.MNQ]
preset = "MNQ"

[symbols.MNQ.override]
"generator.arrival" = { family = "log_ou_cox", sigma_y = 3.0, tau_s = 1.0 }
```

`[[instrument]]` still fails to parse, because an array of tables does not
deserialize into `Option<toml::Table>`. That is now an incidental consequence of
the type rather than a pinned promise, which is exactly why the pin test goes.

`[instrument].symbol` becomes a REFUSED key with a message that names the
replacement, joining `base` and `quote` in the removed-key guard - a silently
ignored `symbol` inside the overlay would serve a different tape than the
operator wrote.

### 3.2 The precedence, one sentence

Resolved preset bundle (operator `preset`, else preset named by the symbol,
else `DEFAULT_PRESET`) < `[instrument]` default knobs < `[symbols.<SYM>]` knobs.

This is the todo's three-layer precedence with the operator layer split in two
by specificity. The bundle choice itself reads the per-symbol `preset` first and
the default overlay's `preset` second, so `[instrument] preset = "MNQ"` makes
every unmatched symbol MNQ-shaped and `[symbols.FOOBAR] preset = "MES"` still
overrides it for FOOBAR.

### 3.3 The types and signatures

In `crates/mogwai-server/src/config.rs`:

```rust
pub struct Config {
    // ... unchanged knobs ...

    /// The one symbol this run serves. See doc text in 3.1.
    pub symbol: Option<String>,

    /// Operator knobs applied to every resolved symbol, exactly as written.
    #[serde(rename = "instrument")]
    pub instrument: Option<toml::Table>,

    /// Operator knobs applied to one symbol, exactly as written. Same overlay
    /// shape as `instrument`; applied after it.
    #[serde(rename = "symbols")]
    pub symbols: HashMap<String, toml::Table>,
}
```

THE `[symbols.*]` KEY IS MATCHED CASE-INSENSITIVELY, by uppercasing both the
table key and the lookup symbol. This is a deliberate decision, not an
accident of `HashMap`: `preset_text` already uppercases, so `symbol = "mnq"`
selects the MNQ preset (pinned by `a_lowercase_symbol_matches_its_preset`), and
a raw `HashMap` lookup would silently miss `[symbols.MNQ]` for that same run -
the overlay would vanish with no error, which is the failure mode this piece
exists to remove. Two table keys that uppercase to the same string (`[symbols.mnq]`
and `[symbols.MNQ]`) are a CONFIGURATION ERROR refused at load with both
spellings named; picking one silently is the same silent-ignore in a new
costume. Test: `a_lowercase_boot_symbol_finds_its_uppercase_symbols_table`, and
`two_symbol_tables_differing_only_in_case_are_refused`.

`Default` adds `symbol: None` and `symbols: HashMap::new()`.

```rust
impl Config {
    /// The symbol this run boots. Absent, the default bundle's own symbol
    /// stands, which is what makes a no-config run BTCUSDT.
    pub fn boot_symbol(&self) -> Option<&str>;

    /// True when the BOOT symbol resolves through no operator overlay at all -
    /// neither the default table nor a `[symbols.*]` table that matches it.
    /// The scratch-config guard in `gen` and `fit::walk` asks this.
    pub fn boot_symbol_carries_no_knobs(&self) -> bool {
        self.overlays_for(self.boot_symbol()).is_empty()
    }

    /// The overlays this symbol resolves through, outermost first: the default
    /// knobs, then that symbol's own table. Empty entries are omitted.
    pub(crate) fn overlays_for(&self, symbol: Option<&str>) -> Vec<toml::Table>;
}
```

`boot_symbol` is `pub`, not `pub(crate)`: `profile_for` is `pub` and takes a
symbol, so the pair an out-of-crate caller composes to reproduce the boot
resolution has to be reachable from outside the crate too.

THE GUARD ASKS ABOUT THE BOOT SYMBOL, NOT ABOUT THE FILE. The obvious spelling -
"did the operator write any instrument knobs anywhere" - reintroduces the exact
silent-ignore the guard was written to stop. A scratch config of

```toml
[symbols.MNQ]
price_increment = "0.25"
```

carries knobs, so a file-scoped question passes it, while `boot_symbol()` is
`None`, the run resolves the BTCUSDT default, and every scratch knob is
discarded unread. Asking `overlays_for(boot_symbol())` refuses it, and it is
the same call the resolution itself makes, so the guard cannot drift from what
runs. Test: `a_scratch_config_whose_only_knobs_are_for_another_symbol_is_refused`,
in both `gen.rs` and `fit/walk.rs`.

`instrument_symbol` and `instrument_table` are DELETED, not deprecated.

The resolution entry points:

```rust
/// The validated profile a symbol resolves to under this config.
pub fn profile_for(cfg: &Config, symbol: Option<&str>)
    -> anyhow::Result<source::InstrumentProfile>;

/// The validated profile selected by a boot symbol and a stack of operator
/// overlays applied in order.
pub fn profile_for_config(symbol: Option<&str>, overlays: Vec<toml::Table>)
    -> anyhow::Result<source::InstrumentProfile>;

/// Resolves and validates EVERY configured shape, boots one. See 3.5.
pub fn build_instrument_profiles(cfg: &Config)
    -> anyhow::Result<source::InstrumentProfiles>;
```

`build_instrument_profiles` is not a one-liner alias: its body is
`InstrumentProfiles::from_profiles(vec![profile_for(cfg, cfg.boot_symbol())?])`
after the 3.5 validation sweep, because `profile_for` yields one
`InstrumentProfile` and the return type is the set.

`profile_for_config` changes its second parameter from one table to a Vec of
tables. `profile_for_symbol(symbol)` keeps its signature and becomes
`profile_for_config(Some(symbol), Vec::new())`, so all twelve external call
sites are untouched.

`profile_for` is the seam slice 2 needs: when the symbol arrives per request,
the server calls `profile_for(cfg, Some(requested))` and nothing else changes.
It is built now, with the boot symbol as its only caller, rather than left to
be discovered.

### 3.4 The resolution restructure

`resolve_instrument` splits in two so an overlay can be applied more than once:

```rust
fn resolve_instrument(symbol: Option<&str>, overlays: Vec<toml::Table>)
    -> anyhow::Result<toml::Table>;

/// Applies one operator overlay onto an already-resolved bundle table.
/// `preset` and `symbol` have been removed by the caller. `requested` is the
/// symbol being resolved and `source` names where the overlay came from
/// (`instrument` or `symbols.<SYM>`); both exist only so the addition log can
/// say which write it is reporting.
fn apply_overlay(
    merged: &mut toml::Table,
    overlay: toml::Table,
    bundle: &str,
    requested: Option<&str>,
    source: &str,
) -> anyhow::Result<()>;
```

The two trailing parameters are load-bearing for the logging requirement below
and cannot be recovered inside the function: the requested symbol is inserted
into `merged` only AFTER every overlay has run, so reading `merged["symbol"]`
would report the PRESET's symbol - for an unmatched symbol like FOOBAR that is
BTCUSDT, naming the wrong run in the log line meant to disambiguate it.

`resolve_instrument` does, in order:

1. For each overlay: refuse `base` / `quote` with the existing message; refuse
   `symbol` with `instrument.symbol was replaced by the top-level symbol key;
   the [instrument] and [symbols.*] tables carry knobs, not an instrument`;
   remove and type-check `preset`.
2. Pick the bundle with the SINGLE existing spelling of the precedence: take the
   last present `preset` among the overlays in order (so the per-symbol table
   beats the default table) and pass it straight into
   `bundle_name(symbol, winner)`, which still resolves operator-preset, then
   symbol-named preset, then `DEFAULT_PRESET`. `bundle_name` is called
   unconditionally on one path; picking the bundle directly when a `preset` is
   present and falling back to `bundle_name` otherwise would re-spell the first
   step of the precedence in a second place, which this spec forbids elsewhere.
3. `base_bundle` once, producing `merged`.
4. `apply_overlay(&mut merged, overlay, bundle, symbol, source)` for each
   overlay in order, `source` being `instrument` or `symbols.<SYM>`.
5. Insert the requested symbol into `merged` if one was given.

`apply_overlay` is today's body verbatim from the `override` extraction through
the top-level-key loop: the `generator.arrival` insertion exception, the
`replace_dotted_for_bundle` strict path for dotted-or-existing keys, and the
logged addition for a new non-dotted key. Both semantics survive re-application:
a key the default overlay ADDED is present when the per-symbol overlay runs, so
the per-symbol write takes the strict REPLACE path and any typo in it is still
caught by `replace_dotted`. The `tracing::info!` addition log gains a `symbol`
field carrying `requested` and an `overlay` field carrying `source`, so two
overlays writing the same knob are distinguishable in the log.

Applying a default overlay across symbols can produce a contradiction - a
`class` table declaring spot on top of the MNQ future bundle, say. That is the
operator's explicit instruction and it is REFUSED, never silently absorbed, and
it is refused at TWO distinct points, which matters for slice 2. An `override`
path or a dotted key the chosen bundle does not set is refused at RESOLVE time
by `replace_dotted_for_bundle`, with the bundle named. A shape that resolves but
does not cohere - a margin table on a spot class, a future with no margin, a key
that is not a `ConfiguredInstrument` field - is refused at VALIDATION time by
`validate_instrument_options` and `deny_unknown_fields`. In slice 1 both are
boot failures and the distinction is invisible; in slice 2, where `profile_for`
runs per request, a default overlay that contradicts one symbol's bundle becomes
a per-request failure for that symbol alone while the run keeps serving others.
Section 3.5 is what keeps that from being a surprise for CONFIGURED symbols. No
new guard is needed and none is added.

### 3.5 Every configured shape is validated at boot

`build_instrument_profiles` resolves and validates the default shape and EVERY
`[symbols.*]` shape, then constructs and returns only the boot profile. The
non-boot profiles are dropped on the floor; the point is the refusal, not the
value.

This is owed by the funding ruling of 2026-08-15 (`notes/todo.md`, FUNDING:
CLOSED): the set of reachable shapes is closed at boot precisely because it is
the set the operator explicitly configured plus the default preset, and a shape
whose settlement or quote currency has no ledger line is a configuration error
that refuses at boot so that a runtime funds rejection means depletion and only
depletion. This piece is what first lets an operator configure a shape other
than the boot one, so it is this piece that owes the sweep. Without it a typo or
an unfunded currency under `[symbols.X]` survives boot and surfaces only when
slice 2 first requests X - a runtime failure that looks like a trading outcome,
which is exactly the collapse the ruling forbids.

Concretely, before returning: for each of `None` (the default shape) and each
configured symbol key, call `profile_for(cfg, sym)` and then
`refuse_unfunded_settlement(cfg, &profile.def)`. Any error propagates with the
symbol named. `refuse_unfunded_settlement` keeps its existing signature and its
empty-balances warning short-circuit; it is simply called once per configured
shape instead of once. Its comment about "ONE instrument per run" is reworded to
say the run refuses any configured shape it could not fund.

Tests: `an_invalid_non_boot_symbol_table_refuses_at_boot` (a bogus `override`
path under `[symbols.X]` with `symbol = "MNQ"` at top level fails
`build_instrument_profiles`, and the message names X) and
`an_unfunded_non_boot_symbol_refuses_at_boot` (balances funding only the boot
symbol's currency, plus a `[symbols.X]` shape settling in another).

## 4. The bricks

Each brick is a landing that leaves `brokkr check` green. They are ordered so no
boundary between them is red.

### Brick A - split the config fields

Add `Config.symbol` and `Config.symbols`, keep `Config.instrument`, add
`boot_symbol`, `boot_symbol_carries_no_knobs` and `overlays_for`, delete
`instrument_symbol` and `instrument_table`. Change `profile_for_config` to take
`Vec<toml::Table>`, add `profile_for`, restructure `resolve_instrument` into
`resolve_instrument` + `apply_overlay` per 3.4, add the 3.5 boot sweep, and
repoint `build_instrument_profiles` accordingly.

Rewrite the in-crate unit tests that pass a single table to
`profile_for_config`. There are EIGHT such call sites in the `config.rs` test
module and the rewrite is TWO edits each, not one: every one of them writes
`symbol = "MNQ"` (or `"FOOBAR"`) as the overlay's first key, and 3.1 makes that
key REFUSED, so passing the table through unchanged fails on the refusal rather
than on its assertion. Each becomes: DROP the `symbol` line from the overlay
text - the symbol is already the first argument in every one of them - and wrap
the table in a one-element vec. The affected tests are
`an_operator_preset_beats_a_matching_symbol`,
`a_top_level_key_overrides_the_resolved_bundle`,
`a_top_level_optional_section_the_bundle_lacks_is_added`,
`a_top_level_key_that_is_not_a_field_is_still_refused`,
`a_top_level_override_of_a_coupled_key_must_state_both` (two call sites, `lone`
and `paired`), `an_override_path_the_bundle_does_not_set_is_still_refused` and
`an_explicit_unknown_preset_is_still_an_error`. Their ASSERTIONS do not change,
and that is the load-bearing part: the single-overlay path must behave exactly
as before.

Repoint `gen.rs` and `fit/walk.rs` from `cfg.instrument.is_none()` to
`cfg.boot_symbol_carries_no_knobs()`, and reword their refusal messages to name
both tables. Keep the literal substring `[instrument]` in `gen.rs`'s message, or
update the test that asserts on it in the same edit.

The scratch-config WRITERS in `fit/walk.rs` and `gen.rs` need no change: they
emit preset-only `[instrument]` overlays carrying no `symbol`, which stay valid.
Confirm that rather than assume it - the `walk.rs` test asserting the exact
prefix `[instrument]\npreset = "MNQ"\n[instrument.override]\n` is the check.

Delete `a_config_naming_two_instruments_fails_to_parse`.

New tests, all in the `config.rs` test module:

- `a_boot_symbol_at_top_level_selects_its_preset` - `Config` parsed from
  `symbol = "MNQ"` resolves to the same profile as `profile_from_preset("MNQ")`.
- `an_instrument_table_naming_a_symbol_is_refused` - the error text names the
  top-level `symbol` key.
- `default_knobs_apply_to_an_unmatched_symbol` - `symbol = "FOOBAR"` plus
  `[instrument]` setting one knob resolves to the default preset's shape with
  that knob changed and the symbol FOOBAR.
- `per_symbol_knobs_beat_default_knobs` - `[instrument]` and `[symbols.MNQ]`
  both set `price_increment`; the resolved profile carries the per-symbol one.
- `a_per_symbol_preset_beats_the_default_preset_key` - `[instrument] preset =
  "MNQ"` with `[symbols.X] preset = "MES"` resolves X to the MES bundle.
- `a_config_carrying_two_symbol_tables_parses_and_resolves_both` - the
  replacement for the deleted pin: `[symbols.MNQ]` and `[symbols.BTCUSDT]` in
  one file both load, and `profile_for` returns each one's own shape. This is
  the test that proves the removed assumption is gone rather than merely
  unasserted.
- `a_typo_in_a_per_symbol_override_is_still_refused` - a bogus dotted path under
  `[symbols.MNQ.override]` errors and the message names the chosen bundle.
- `a_lowercase_boot_symbol_finds_its_uppercase_symbols_table` and
  `two_symbol_tables_differing_only_in_case_are_refused` - the case rule of 3.3.
- `a_scratch_config_whose_only_knobs_are_for_another_symbol_is_refused` - the
  boot-scoped guard of 3.3, one copy in `gen.rs` and one in `fit/walk.rs`.
- `an_invalid_non_boot_symbol_table_refuses_at_boot` and
  `an_unfunded_non_boot_symbol_refuses_at_boot` - the boot sweep of 3.5.

Gate:

    brokkr check

and focused, while iterating:

    brokkr test -p mogwai-server a_config_carrying_two_symbol_tables_parses_and_resolves_both
    brokkr test -p mogwai-server per_symbol_knobs_beat_default_knobs

Bite-check every new test per the standing rule: revert the production change as
a text edit, observe the named failure, restore it as a text edit. Never with
`git checkout -- <path>`.

### Brick B - migrate the shipped and test configs

- `mogwai.toml`: commented `symbol = "FOOBAR"` at top level, a commented
  `[instrument]` described as default knobs, and a commented `[symbols.MNQ]`
  example. Update the `[balances]` comment, which currently says "the quote
  currency of the configured instrument": it becomes "of the symbol this run
  serves".
- `crates/mogwai-cli/tests/configs/mnq.toml`: `symbol = "MNQ"` at top level and
  the whole knob block moved under `[symbols.MNQ]` (and `[symbols.MNQ.class]`,
  `[symbols.MNQ.margin]`, `[symbols.MNQ.fees.maker]`, `.taker`,
  `[symbols.MNQ.generator]`, `[symbols.MNQ.session]`, `[symbols.MNQ.calendar]`).
  The inner `generator.symbol = "MNQ"` stays - it is a generator scalar, not the
  instrument symbol. This file is deliberately migrated to the PER-SYMBOL form
  rather than the default form, so the integration suite drives the new path
  end to end.
- `crates/mogwai-cli/tests/configs/fees.toml`: the same migration as `mnq.toml`
  and easy to miss - it is not referenced by any Rust suite name, only by a
  `config.rs` test COMMENT and by the smoke test's fees arm, so neither
  `brokkr test -p mogwai-cli serving` nor `lifecycle` will catch it and only the
  smoke run below will. Migrate it to the per-symbol form alongside `mnq.toml`
  and fix the comment in `a_top_level_optional_section_the_bundle_lacks_is_added`
  that describes its shape.
- `crates/mogwai-cli/tests/configs/unmatched-symbol.toml`: `symbol = "FOOBAR"`
  at top level, `[instrument]` deleted.
- `mnq-preset.toml`, `no-warmup.toml`, `arrival-fault.toml`: keep `[instrument]`
  with its `preset` key and, for `arrival-fault.toml`, its `[instrument.override]`
  - these carry no symbol, so they are already valid default-knob overlays and
  migrate by doing nothing. They are the coverage that the DEFAULT overlay path
  works, which is why they are not all moved to `[symbols.*]`.

Gate:

    brokkr check

plus the socket-backed serving path, which `brokkr check` cannot reach:

    brokkr test -p mogwai-cli serving
    brokkr test -p mogwai-cli lifecycle

and the live end-to-end path, run in two shells:

    brokkr run mogwai -- serve --config mogwai.toml
    python3 scripts/smoke.py

### Brick C - the durable prose

Piece 14 says the prose lands WITH the code, so it is a brick here and not a
follow-up.

- `docs/config.md`: replace "One optional `[instrument]` table overlays the
  symbol-resolved bundle ... `[[instrument]]` is not accepted" with the three
  layers of 3.2 and the top-level `symbol` key. Every `[instrument.X]` heading
  gains its `[symbols.<SYM>.X]` twin, stated once as a rule rather than
  duplicated per section. State that `[instrument].symbol` is refused and why,
  state the case rule of 3.3, and state the boot sweep of 3.5 - an operator has
  to know that a `[symbols.X]` table they never serve can still fail the boot.
  THREE sentences in that file assert the old shape, not two: the "One optional
  `[instrument]` table" opener, the `[[instrument]]` refusal beside it, and
  "`[instrument]` may be as small as a symbol. That symbol selects a matching
  preset" further down. The third is the one a mechanical search for
  `[[instrument]]` misses, and it becomes false outright.
- `docs/presets.md`: the operator-overlay paragraph gains the per-symbol form
  and the "a preset is a named knob bundle, not an admission record" framing
  already settled in `AGENTS.md`.
- `reference/architecture.md`: one paragraph stating that the config declares no
  instrument, that a symbol resolves totally through preset-or-default, and that
  the boot symbol is a slice-1 artifact scheduled to become a request parameter.
  This is the durable home for the sentence `notes/todo.md` currently owns.
- `notes/todo.md`: strike piece 2 from the inventory the same way pieces 1 and 3
  were struck, leaving the numbering intact so the piece-4, piece-5 and piece-13
  cross-references still resolve, and delete item 2 from the "what has to
  change" list in the symbol bullet.

Gate: `brokkr check` (the gremlin scan covers markdown), plus a read of
`docs/config.md` against a config the reader could write from it alone.

## 5. Verification, per the contract

| Change | Gate | Command |
|---|---|---|
| Config parse and precedence | the new `config.rs` unit tests | `brokkr check` |
| Single-overlay behavior unchanged | the eight existing `profile_for_config` call sites, overlay text losing only its `symbol` line, assertions untouched | `brokkr check` |
| Every configured shape refuses at boot | the two 3.5 tests | `brokkr check` |
| Scratch knobs cannot be silently ignored | the boot-scoped guard test in `gen` and in `fit::walk` | `brokkr check` |
| The fees arm still loads | the migrated `fees.toml`, reachable only from the smoke run | `python3 scripts/smoke.py` |
| Preset provenance and diagnostics still hold | the shipped-preset test battery in `config.rs` | `brokkr check` |
| Boot path serves the configured symbol | the socket-backed CLI suites | `brokkr test -p mogwai-cli serving` |
| Live WS and control plane | the smoke test | `brokkr run mogwai -- serve --config mogwai.toml` then `python3 scripts/smoke.py` |
| Scratch-config refusals in `gen` and `fit` | their own tests, plus a manual `mogwai gen --config` against a migrated file | `brokkr check` |

No adapter file is touched, so `brokkr check --gate` is not required by this
piece; run plain `brokkr check`. No tape byte moves - resolution produces the
same `InstrumentProfile` for the same inputs and no generator constant, seed
derivation or fingerprint leaf changes - so NO `TAPE_PROTOCOL_VERSION` BUMP IS
OWED. The seed's symbol dimension that does owe one is piece 8, and it is not
here. Prove the no-move claim rather than asserting it: the fill golden
(`crates/mogwai-server/tests/golden/fill_distribution.json`) and every
determinism test are exact-equality gates and must pass UNCHANGED and
UNRE-BLESSED. A re-bless demanded by this piece means the restructure moved
something it claimed not to, and is a revert signal, not a bless signal.

## 6. Keep/revert path

Three landings, each coherent and each green. Brick A is the intrusive one: it
lands whole - the field split, the resolution restructure, the deleted pin and
the new tests together - and is kept or reverted on `brokkr check` plus the
unchanged fill golden. Brick B is kept or reverted on the socket suites and the
smoke run. Brick C carries no gate but the gremlin scan and a read.

There is no feature flag, no env var, and no compatibility path for the old file
shape. An operator with an old file moves `symbol` out of `[instrument]`; the
refusal message tells them so by name. Pre-1.0, that is the whole migration
story.

## 7. Stopping rule

IN: `Config`'s instrument-facing fields and accessors, `resolve_instrument` and
its callers inside `config.rs`, the boot validation sweep of 3.5 and the call
sites of `refuse_unfunded_settlement`, the two scratch-config guards in `gen.rs`
and `fit/walk.rs`, the scratch-config WRITERS in those same two files (surveyed
and confirmed unchanged, not ignored), the shipped and test TOML files including
`fees.toml`, and the prose in `docs/` and `reference/`.

OUT, and deliberately: `serve.rs`'s `.next()` collapse over the profile set
(piece 7); `RunIndex`, `BOOT` and `materialize_warmup`, which keep initializing
from the boot symbol unchanged (the piece-4 ruling); the `/trades` and
`/quotes` unknown-symbol 400 (piece 5); the `/ws` symbol carrier (piece 6); seed
derivation (piece 8); `/instruments` and the adapter's subscription guard
(piece 13). `InstrumentProfiles` stays a one-entry map and `Engine::build`
keeps receiving one instrument. This piece makes the CONFIG stop declaring an
instrument; it does not make the RUN serve more than one.

## 8. Review disposition

Two independent reviews of the pre-amendment draft (`notes/piece2-spec-review-R1.md`,
a Claude pass, and `notes/piece2-spec-review-R2.md`, a codex deep pass) raised
nine numbered findings between them, six from R1 (whose sixth bundles four
separate edges) and three from R2. Counted at the level of a distinct defect
that is eleven, one of which both reviews found independently. ALL of them were
validated against the tree on 2026-08-15 and are folded above; NONE was
rejected. Where they overlap, the consolidated form is what stands here.

| Finding | Where it landed |
|---|---|
| The eight existing `profile_for_config` tests all pass `symbol` inside the overlay, so "assertions do not change" understated the rewrite (R1-1) | Brick A, with the call sites named |
| `fees.toml` shares `mnq.toml`'s shape and was missing from brick B (R1-2) | Section 2 survey and brick B, with the note that no Rust suite reaches it |
| The scratch guard must ask about the BOOT symbol's overlays, not about the file (R1-3, R2-1 - the same defect found twice, from a `[symbols.X]`-only config in both) | 3.3, renamed to `boot_symbol_carries_no_knobs` |
| `[symbols.*]` case matching was unstated and contradicted `preset_text`'s uppercasing (R1-4) | 3.3, decided case-insensitive with a collision refusal |
| The scratch-config WRITERS were unsurveyed (R1-5) | Section 2 survey, brick A and the stopping rule |
| Step 2 re-spelled the bundle precedence outside `bundle_name` (R1-6a) | 3.4 step 2, now one unconditional call |
| `build_instrument_profiles` as written was a type mismatch, and `boot_symbol` was too private to compose the advertised seam (R1-6b) | 3.3 |
| `docs/config.md` has a THIRD stale sentence ("as small as a symbol") (R1-6c) | Brick C |
| Bundle contradictions refuse at resolve time as well as validation time, which becomes a per-request failure in slice 2 (R1-6d) | 3.4 closing paragraph |
| Configured non-boot shapes could survive boot, against the FUNDING: CLOSED ruling (R2-2) | New section 3.5 |
| `apply_overlay`'s signature could not supply the promised `symbol` log field (R2-3) | 3.4, two added parameters and why |
