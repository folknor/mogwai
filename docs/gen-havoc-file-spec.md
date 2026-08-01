# `mogwai gen --havoc <file>` - implementation spec

## Standing references

- Contract this spec is written against:
  `reference/technical-implementation-spec.md`.
- Prerequisite it builds on: `docs/gen-cli-spec.md` (the `mogwai gen` command,
  now landed in `crates/mogwai-server/src/gen.rs`). This spec adds one flag to it.
- Originating item: `docs/gen-cli-spec.md`'s "OUT of scope" list names the
  `--havoc <file>` convenience explicitly as a deferred follow-on. This is it.

## Goal

Add a `--havoc <PATH>` flag to `mogwai gen` that reads a full `HavocSpec` (the
same config object broadarrow builds per venue) from a JSON file and applies the
one surface that reshapes an offline generated tape - its `data` field (the
`MarketRegime`) - warning on stderr that the other three surfaces do not apply to
a tape dump. It validates the WHOLE spec with the same gates broadarrow uses, so
a file broadarrow would reject is rejected here too (an honest preview), even
though only `data` takes effect. This lets an operator point `gen` at a
hand-written or exported `HavocSpec` and preview its market-regime tape effect
without hand-extracting the regime into `--regime`.

## Stopping rule / scope

IN scope:
- A `--havoc <PATH>` flag on `GenArgs`, mutually exclusive with `--regime`.
- Reading the file as JSON into a `HavocSpec`; validating ALL FOUR surfaces with
  the re-exported `validate_client_havoc` / `validate_conn_havoc` /
  `validate_divergence` / `validate_market_regime`; and feeding the spec's `data`
  regime into the generator exactly where `--regime` feeds today.
- A one-line stderr note when the loaded spec carries any of the three
  offline-inapplicable surfaces (`client`, `conn`, or a non-empty `server`), so
  the operator is not misled into thinking transport/execution havoc took effect.

OUT of scope (named, not deferred):
- TOML input. `HavocSpec.data` is `MarketRegime`, an internally-tagged enum that
  round-trips cleanly through `serde_json` (the format `--regime` already uses)
  but is finicky through `toml`. JSON keeps one format for both regime inputs.
- Actually APPLYING the `client`/`conn`/`server` surfaces (the transport-
  corruption `--as-received` lens and any server-divergence replay). They stay
  out per the parent spec's decision; here they are validated and noted, not run.
- Config-driven INSTRUMENTS (`--config mogwai.toml`). A separate deferred item.

## Survey of the ground

The landed CLI (`crates/mogwai-server/src/gen.rs`):
- `GenArgs` already has `#[arg(long)] regime: Option<String>` and `#[arg(long)]
  out: Option<PathBuf>` (so `PathBuf` and the clap `long` idiom are in use, and
  `anyhow::Context` and `std::path::PathBuf` are already imported).
- `build_source(args: &GenArgs) -> anyhow::Result<GeneratedSource>` resolves the
  regime via the verbatim line `let regime =
  args.regime.as_deref().map(parse_regime).transpose()?;` and passes it to
  `try_new_with_session_profile`. `parse_regime(raw: &str) ->
  anyhow::Result<MarketRegime>` does `serde_json::from_str` then
  `validate_market_regime`. This is the single seam the new flag hooks into.
- `run`/`run_into` write ONLY CSV to the chosen sink (`--out` file or stdout).
  Diagnostics therefore go to stderr (`eprintln!`), which cannot touch the sink
  under either sink choice, so the CSV is never corrupted.

Types (`mogwai-protocol`, all re-exported at the crate root, verified against the
tree):
- `HavocSpec { client: ClientHavoc, server: Vec<control::Divergence>, data:
  Option<MarketRegime>, conn: ConnHavoc }`, deriving `Deserialize`, `Default`,
  `PartialEq`, with `#[serde(default)]` on every field, so a JSON file naming
  only `data` loads with the other three defaulted (exercised by the existing
  `havoc_spec_defaults_from_empty_object` protocol test).
- `ClientHavoc` derives `Default`; `ConnHavoc` provides `Default` via a MANUAL
  `impl` (its defaults are the production-shaped honest lifecycle, so an
  explicit-but-honest `conn` block reads as unarmed). Both plus `HavocSpec` are
  `PartialEq`, so "is this surface armed?" is `!= Default::default()` for
  `client`/`conn` and `!is_empty()` for `server`.
- `validate_client_havoc`, `validate_conn_havoc`, `validate_divergence`,
  `validate_market_regime` are all `pub` and re-exported (`lib.rs`). There is no
  single spec-level `validate_havoc` in the protocol (that lives adapter-side),
  so the whole-spec validation here calls the four field validators.

Dependencies: `mogwai-server` already depends on `serde_json`, `anyhow`,
`mogwai-protocol`. No manifest change.

## Target - concrete artifacts

All edits in `crates/mogwai-server/src/gen.rs`.

`GenArgs` gains one field:

```rust
    /// Read a full HavocSpec from this JSON file and apply its `data` market
    /// regime. The whole spec is validated (a file broadarrow would reject is
    /// rejected here), but the client, conn, and server surfaces do not affect an
    /// offline tape dump and are noted on stderr. Mutually exclusive with
    /// --regime.
    #[arg(long, value_name = "PATH", conflicts_with = "regime")]
    havoc: Option<PathBuf>,
```

Regime resolution is factored out of `build_source`. The FILE read is a thin
wrapper; the parse/validate/select is a PURE function over the text, so tests
drive it with string literals and touch no filesystem (this is why there is no
temp file: `env!("CARGO_TARGET_TMPDIR")` is not set for a bin crate's unit-test
module, so a temp-file approach would not even compile there).

```rust
/// The market regime to build the generator with, from `--regime` (inline JSON)
/// or `--havoc <file>` (a HavocSpec JSON whose `data` surface is used), or
/// neither. `--regime`/`--havoc` are clap-exclusive, so at most one is set.
fn resolve_regime(args: &GenArgs) -> anyhow::Result<Option<MarketRegime>> {
    if let Some(path) = &args.havoc {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading havoc file {}", path.display()))?;
        return resolve_havoc_regime(&text)
            .with_context(|| format!("in havoc file {}", path.display()));
    }
    args.regime.as_deref().map(parse_regime).transpose()
}

/// Parse a HavocSpec from JSON `text`, validate the whole spec, note the
/// offline-inapplicable surfaces on stderr, and return the `data` regime. Pure
/// over the text (no filesystem), so tests drive it directly.
fn resolve_havoc_regime(text: &str) -> anyhow::Result<Option<MarketRegime>> {
    let spec: HavocSpec = serde_json::from_str(text).context("parsing havoc JSON")?;
    validate_client_havoc(&spec.client).map_err(|e| anyhow::anyhow!("invalid client havoc: {e}"))?;
    validate_conn_havoc(&spec.conn).map_err(|e| anyhow::anyhow!("invalid conn havoc: {e}"))?;
    for div in &spec.server {
        validate_divergence(div).map_err(|e| anyhow::anyhow!("invalid server divergence: {e}"))?;
    }
    if let Some(regime) = &spec.data {
        validate_market_regime(regime).map_err(|e| anyhow::anyhow!("invalid regime: {e}"))?;
    }
    if havoc_has_offline_inapplicable_surfaces(&spec) {
        eprintln!(
            "note: --havoc applies only the data (market regime) surface offline; \
             the client, conn, and server surfaces are ignored"
        );
    }
    Ok(spec.data)
}

/// True when a loaded HavocSpec carries a surface a tape dump cannot honor.
fn havoc_has_offline_inapplicable_surfaces(spec: &HavocSpec) -> bool {
    spec.client != ClientHavoc::default()
        || spec.conn != ConnHavoc::default()
        || !spec.server.is_empty()
}
```

`build_source` changes its one regime line from the inline
`args.regime.as_deref().map(parse_regime).transpose()?` to `resolve_regime(args)?`.
Imports gain `mogwai_protocol::{ClientHavoc, ConnHavoc, HavocSpec,
validate_client_havoc, validate_conn_havoc, validate_divergence}` (joining the
existing `MarketRegime, validate_market_regime` import).

No other file changes: `run`/`write_*` are untouched, and the clap
`conflicts_with = "regime"` (clap derives the arg id from the field name
`regime`, unrenamed) makes `--regime`/`--havoc` together a parse error with no
manual check.

## Landing

One additive landing (one flag, two factored functions, one call-site swap): it
compiles and the suite stays green in a single commit.

Ordered bricks:
1. Add `resolve_regime`, `resolve_havoc_regime`, and
   `havoc_has_offline_inapplicable_surfaces`; repoint `build_source`; add the
   `havoc` field to `GenArgs`; add the imports.
2. Extend `#[cfg(test)] mod tests` per Verification.

## Verification per brick

New behavior; instruments built here. `resolve_havoc_regime` and the surface
predicate are pure and tested with string literals (no filesystem, no temp dir);
the file wrapper's read-error path is tested with a path that does not exist (a
READ, never a write, so the repo's no-`/tmp`-writes rule is not engaged); the
clap exclusivity is tested through a `Parser` wrapper.

- `resolve_havoc_regime`, string-driven cases:
  `{"data":{"type":"LiquidityDrought","thin_factor":5.0}}` -> `Some(LiquidityDrought
  { thin_factor: 5.0 })`; `{"client":{"drop_prob":0.5}}` -> `Ok(None)` (valid
  client, no data); `{"data":{"type":"LiquidityDrought","thin_factor":0.0}}` ->
  `Err` (out-of-range data); `{"client":{"drop_prob":7.0}}` -> `Err` (the
  whole-spec validation rejecting an inapplicable-but-INVALID surface, the smell
  fix); `not json` -> `Err`; `{"data":{"type":"Nonsense"}}` -> `Err` (unknown
  tag). Command:
  `brokkr test -p mogwai-server resolve_havoc_regime`
- File wrapper: `resolve_regime` on a `GenArgs` whose `havoc` points at a
  nonexistent path returns `Err` (the `reading havoc file` context). Command:
  `brokkr test -p mogwai-server havoc_file_missing`
- `havoc_has_offline_inapplicable_surfaces`: `HavocSpec::default()` -> `false`; a
  spec with a non-default `client`, or a non-empty `server`, or a non-default
  `conn` -> `true`; a spec carrying only `data` -> `false`. Command:
  `brokkr test -p mogwai-server havoc_has_offline`
- Clap exclusivity: both `--regime` and `--havoc` together is a parse error.
  `GenArgs` derives `Args`, not `Parser`, so the test wraps it in a throwaway
  `#[derive(clap::Parser)] struct Wrap { #[command(flatten)] g: GenArgs }` and
  asserts `Wrap::try_parse_from(["gen", "--regime", "{...}", "--havoc", "p"])`
  is `Err`. Command:
  `brokkr test -p mogwai-server havoc_regime_conflict`
- Whole-landing gate (gremlins, clippy, all crates' tests):
  `brokkr check`
- Manual eyeball (not a gate): with an operator-supplied `spec.json` carrying a
  `LiquidityDrought` data surface, `brokkr run mogwai -- gen --type
  bars --length 1d --interval 5m --havoc spec.json` prints a visibly thinner tape
  than the same run without it.

## Keep / revert

Kept only if `brokkr check` is green and the `resolve_havoc_regime` tests pin the
six file-content cases plus the surface predicate, the file-missing wrapper, and
the clap conflict. Purely additive (one flag threading into the existing regime
seam); a no-`--havoc` run resolves the regime exactly as before, which the
pre-existing `gen_*` tests still gate. If any pre-existing `gen` test changes
result, the factoring leaked and the landing is reverted whole.
