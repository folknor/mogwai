# Implementation spec: piece 8, seed derivation gains a symbol dimension

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/todo.md`: piece 8 of the "Landing the grand design: fourteen pieces"
inventory, and item 4 of the "THE SYMBOL IS A REQUEST PARAMETER, NOT AN
IDENTITY THE VENUE OWNS" bullet under Open issues, read together with the
river-and-boat bullet (river identity is seed plus symbol plus resolved
bundle) and the total-symbol-resolution bullet (an unmatched symbol wears the
default preset's shape under its own label).

## 1. The defect, stated as behavior

`RunSeeds::from_run_seed` derives exactly two streams from the run seed - a
tape root and a fill root - and neither carries a symbol term. Piece 7 landed
the keyed `Rivers` registry, so two configured shapes now get two independent
checkpoint chains, two `GeneratedSource` instances and two live frontiers. Both
are constructed with `identity.seeds.tape`, the SAME u64. Where the two shapes'
`GeneratorScalars` coincide - which is the entire `FOOBAR`-wears-the-default
case that piece 13 and the total-resolution ruling make the common case - the
two rivers emit byte-identical prints at byte-identical instants under
different symbol labels.

That is not a cosmetic duplicate. It is a false market: a strategy that
diversifies across two symbols served by one venue is trading one path twice
and will read the resulting correlation as a property of the market rather
than of the venue. The in-tree test
`a_second_river_is_realized_under_its_own_def_and_chain`
(`crates/mogwai-server/src/source.rs`) already records this in its doc comment
- it says outright that no per-print assertion can separate the two
realizations "under piece 8's still-pending symbol term". This spec makes that
assertion possible and then makes it.

## 2. Survey of the ground

### 2.1 What the tape seed reaches

`RunSeeds` lives in `crates/mogwai-protocol/src/seeds.rs`: three public fields
(`run`, `tape`, `fill`), one `const fn from_run_seed`, two domain constants
`DOMAIN_TAPE` / `DOMAIN_FILL` fed through `splitmix64`. The header comment
already binds a change here to a `TAPE_PROTOCOL_VERSION` bump.

Every production reader of the `tape` FIELD, exhaustively:

- `crates/mogwai-server/src/serve.rs` - builds the run's `RunSeeds`, logs
  `tape_seed` on the "run seeds fixed" line, hands the seeds to
  `source::TapeIdentity`.
- `crates/mogwai-server/src/source.rs`, `fn generator` - the ONLY place a
  `GeneratedSource` is built from `TapeIdentity`, called from `Rivers::river`
  under the registry mutex when a river is first materialized.

Test and harness readers of the field:

- `crates/mogwai-server/src/fill_golden.rs` - renders the committed fill
  golden through a real `Rivers` at `GOLDEN_RUN_SEED = 42` on BTCUSDT.
- `crates/mogwai-data/src/generated/tests.rs`, `assert_run_seed_dwell_is_bounded_with_draw`
  - the one `mogwai-data` test that derives its generator seed through
  `RunSeeds` rather than passing a literal.
- `crates/mogwai-server/src/{run.rs,fills.rs}` test helpers construct
  `RunSeeds::from_run_seed(42)` whole and never read `.tape` by name.

NOT readers, and this is the load-bearing half of the survey: every offline
caller in `mogwai-cli` and `mogwai-lab` (`gen`, `measure`, `fit`, `cache`,
`synth`, `arrival-*`, `tick-composition`, `stage_*`) passes a RAW literal or
`--seed` value straight into `GeneratedSource::new*`. `DEFAULT_GEN_SEED` is
such a literal. None of them routes through `RunSeeds`. Therefore no
measurement artifact, no fit, no arrival screen, no `analysis/` JSON and no
`mogwai-lab` manifest moves under this change. The blast radius is the SERVER's
tape and nothing else.

### 2.2 The fill half is already done, and must not be redone

`draw_key` in `crates/mogwai-engine/src/orders.rs` feeds `order.symbol` into
the FNV-1a accumulator alongside `fill_seed`, the client order id, the side,
the normalized price and the band draw. Two symbols therefore already draw
independent band offsets from one run-level `fill` root, and the engine
correctly holds ONE `fill_seed` for the whole venue. Adding a symbol term to
`RunSeeds::fill` would be a second, redundant mixing that moves every existing
fill golden for no behavioral gain. `fill` stays run-level and symbol-free.
This spec touches the fill stream in exactly one way: the golden re-blesses
because the TAPE under it moved, not because the band changed.

### 2.3 What a river is keyed by today

`RiverKey` wraps the resolved profile's `Symbol` and `Rivers::river` looks the
profile up by the requested string, then keys by `profile.def.symbol`. Today
those are the same string for every configured shape, because
`InstrumentProfiles::from_profiles` keys the map by `def.symbol`. Under piece
13's total resolution they diverge: `FOOBAR` resolves to the default preset's
shape, whose `def.symbol` is `BTCUSDT`. The seed term must therefore be the
REQUESTED LABEL, not the resolved shape's symbol - otherwise `FOOBAR` and
`BARFOO` collapse back onto one path and this spec's whole premise is
reintroduced by the next piece. Piece 8 lands the label-keyed derivation now,
while label and def symbol still agree, so piece 13 inherits a correct
derivation instead of a latent collapse.

BUT THE PREPARATION IS ONLY HALF, AND THE OTHER HALF IS PIECE 13'S TO LAND.
`RiverKey` is built from `profile.def.symbol` and this spec deliberately leaves
it alone, which is correct today: `InstrumentProfiles::get` is an exact lookup
keyed by `def.symbol`, so label and key are the same string for every reachable
call. Under piece 13 they are not. `FOOBAR` and `BARFOO` would both resolve to
the default shape and therefore collide on `RiverKey(BTCUSDT)` - ONE map entry,
ONE chain - while `tape_for(label)` would give that single chain a tape root
taken from whichever label materialized it FIRST. A run would then depend on
request arrival order and stop being a pure function of `(seed, config)`, which
is strictly worse than the collapse this spec is pre-empting.

So the binding statement piece 13 inherits is: `RiverKey` MUST widen to the
REQUESTED LABEL in the same change that lands total resolution. Keying the seed
by the label and the registry by the def symbol is coherent only while the two
agree. Piece 8 does not widen the key, because with an exact lookup the widened
key would be indistinguishable from today's and the change would be untestable;
it records the obligation here so the preparation is not read as complete.

### 2.4 Version state

`mogwai_data::TAPE_PROTOCOL_VERSION` is 16. 15 remains reserved for the
protocol-12b arrival MECHANISM landing and is NOT taken here. This landing
takes 17.

## 3. The target

### 3.1 `RunSeeds` (crates/mogwai-protocol/src/seeds.rs)

The `tape` FIELD IS DELETED and replaced by a method. This is the point of the
design, not a style preference: while a symbol-free `u64` named `tape` is
reachable, any future caller can build a river without a symbol term and
nothing will notice. Deleting the field makes the defect unrepresentable and
turns every current call site into a compile error that must name a symbol.

```rust
pub struct RunSeeds {
    /// The seed as drawn or configured. The value reported and reproduced.
    pub run: u64,
    /// Root of the fill band's draw stream, run-level and symbol-free: the
    /// band's key already mixes `order.symbol` in `draw_key`.
    pub fill: u64,
}

impl RunSeeds {
    #[must_use]
    pub const fn from_run_seed(run: u64) -> Self {
        Self { run, fill: splitmix64(run ^ DOMAIN_FILL) }
    }

    /// Root of the tape generator's stream for one requested SYMBOL LABEL.
    ///
    /// The label, not the resolved shape's symbol: two labels that resolve to
    /// the same default shape are different rivers and must not share a path.
    #[must_use]
    pub fn tape_for(&self, symbol: &str) -> u64 {
        let mut hash = FNV_OFFSET;
        let mut feed = |bytes: &[u8]| {
            for byte in bytes {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
        };
        feed(&splitmix64(self.run ^ DOMAIN_TAPE).to_le_bytes());
        feed(&[0]);
        feed(symbol.as_bytes());
        splitmix64(hash)
    }
}
```

`FNV_OFFSET = 0xcbf2_9ce4_8422_2325`, `FNV_PRIME = 0x0000_0100_0000_01b3`,
written as named constants in this file (the engine's copy in `orders.rs` is
left alone - `mogwai-protocol` has no workspace dependencies and sharing them
would invert the dependency direction for two integers).

Why this construction, so no successor re-derives it:

- The run-seed root is mixed FIRST and as bytes, so the symbol term cannot be
  cancelled by an adversarial run seed and two run seeds never share a symbol's
  path.
- The `0` separator is NOT a length delimiter and must not be described as one.
  It separates the FIXED-WIDTH root from the one variable-length field that
  follows it, which is all that is needed here and all it does: with exactly
  one trailing variable field, `root || 0 || symbol` is unambiguous whatever
  bytes the symbol contains. It does NOT make the encoding extensible. Append a
  second variable field and `("A", "B")` collides with `("A\0B", "")` again,
  because a delimiter only disambiguates when the fields cannot contain it.
  Piece 9 therefore does not inherit a prepared encoding: when the sharing key
  widens it owes a LENGTH-PREFIXED encoding (feed each field's `u64` length
  before its bytes), and that is a derivation change with its own
  `TAPE_PROTOCOL_VERSION` bump. The separator here is a one-field convenience,
  claimed as nothing more.
- The final `splitmix64` avalanches the FNV output, which is weak in its low
  bits, before it becomes a generator seed.
- Not `const fn`: `&str` iteration is fine in const but the closure is not, and
  no caller needs const evaluation. `from_run_seed` stays `const`.

`RunSeeds` is `pub` and re-exported from `mogwai-protocol`'s root, so deleting
the `tape` field is a BREAKING CHANGE to the published surface, not an internal
edit. Accepted knowingly: no in-tree consumer outside `mogwai-server` reads the
field, `mogwai-adapter` does not touch `RunSeeds` at all, and the whole point of
the deletion is that an external caller reconstructing a symbol-free tape root
is exactly the defect. Consumers in the broadarrow direction depend on the wire
types, not on this struct's fields.

### 3.2 `TapeIdentity` and `Rivers` (crates/mogwai-server/src/source.rs)

`TapeIdentity` is unchanged in shape - it still carries `RunSeeds` and the
regime - but `fn generator` gains the label:

```rust
fn generator(label: &str, profile: &InstrumentProfile, identity: TapeIdentity) -> GeneratedSource
```

and passes `identity.seeds.tape_for(label)` where it passed
`identity.seeds.tape`. `Rivers::river` already holds the requested `symbol:
&str`; it passes that value, NOT `profile.def.symbol`, per 2.3.

`Rivers::river` logs once, at `info`:
`tracing::info!(symbol = label, tape_seed, "river materialized")`. A run
serving several rivers must be reproducible from its log alone, and the run
seed plus this line is what makes that true.

NOT INSIDE THE `or_insert_with` CLOSURE. That closure runs under the registry
mutex, in the one function whose comments make lock discipline load-bearing
("lock ordering is registry then release, river"), and a `tracing` call runs
arbitrary subscriber work - formatting, a file write, a blocking appender -
inside the critical section that every first reader of every symbol contends
on. Instead: compute `tape_seed` before taking the lock, set a `materialized`
flag from inside the closure, `drop(rivers)` as the function already does, and
emit the line after the drop when the flag is set. The line is still emitted
exactly once per river, because only one closure body ever runs per key.

### 3.3 `serve.rs`

The "run seeds fixed" line drops `tape_seed` (there is no run-level tape seed
any more) and keeps `run_seed` and `fill_seed`. The per-river line in 3.2 is
where a tape seed is now reported, which is honest: a tape seed is a property
of a river, not of a run.

### 3.4 The version constant

`crates/mogwai-data/src/lib.rs`: `TAPE_PROTOCOL_VERSION = 17`, with a doc
paragraph in the existing style recording that 17 gives every river its own
tape root keyed by the requested symbol label, that every server-generated
tape moves for every symbol including single-symbol runs, that the offline
`mogwai-lab` and `mogwai-cli` seeds are untouched because they never routed
through `RunSeeds`, and that 15 remains the arrival-mechanism reservation.

## 4. Landing sequence

ONE keep/revert unit, one commit. The version bump, the derivation change, the
wiring and the golden re-bless cannot be split without leaving a boundary at
which the suite is red or the tape identity lies about itself. Within the
commit the order of work is:

1. `seeds.rs`: delete the `tape` field, add `tape_for`, add the constants and
   the comment block. Amend `derived_streams_differ_and_are_stable` per 5.1 and
   add the new tests of 5.1.
2. `source.rs`: `fn generator` takes the label; `Rivers::river` passes the
   requested string and logs the materialization line AFTER dropping the
   registry lock, per 3.2.
3. `serve.rs`: drop `tape_seed` from the boot log line.
4. `crates/mogwai-data/src/generated/tests.rs`: the dwell helper switches to
   `RunSeeds::from_run_seed(run_seed).tape_for("BTCUSDT")` - it already builds
   its scalars from the BTCUSDT def, so the label is that def's symbol.
5. `lib.rs`: bump to 17 with its paragraph.
6. Re-bless `crates/mogwai-server/tests/golden/fill_distribution.json` by the
   documented procedure (delete, rerun the test, inspect the diff, rerun).
7. The prose of section 6, in the same commit.

## 5. Gates, per brick, with the exact command

### 5.1 The derivation itself - new tests in `seeds.rs`

Three tests, all in `crates/mogwai-protocol/src/seeds.rs`:

- `tape_roots_differ_by_symbol_under_one_run_seed`: for run seed 0, 1 and
  `u64::MAX`, assert `tape_for("MNQ")`, `tape_for("MES")`,
  `tape_for("BTCUSDT")`, `tape_for("FOOBAR")` and `tape_for("")` are pairwise
  distinct, and that each differs from `fill`.
- `tape_roots_differ_by_run_seed_under_one_symbol`: `tape_for("MNQ")` differs
  between run seeds 0 and 1.
- `symbol_tape_roots_are_stable`: pin the exact u64 for
  `(0, "BTCUSDT")`, `(1, "MNQ")` and `(u64::MAX, "FOOBAR")` as hex literals.
  These are computed once during implementation and written down; they are the
  regression fence that makes any later re-derivation visibly a tape change.
  The existing `derived_streams_differ_and_are_stable` keeps its `fill`
  vectors unchanged, which is itself the evidence that the fill stream did not
  move.

The edit to `derived_streams_differ_and_are_stable` is a DELETION, and stating
it is part of the work: its `expected` table is `(run, tape, fill)` triples and
its two closing `assert_ne!` pairs read `.tape`, so the tape column and the
tape-vs-fill and tape-across-seeds assertions all go with the field. Only the
`fill` column and its `assert_ne` survive. What replaces the deleted coverage is
the three new tests, and the replacement is not equivalent in KIND: the file's
header carries a STRUCTURAL bijection argument for `tape != fill` (splitmix64 is
a bijection on u64, and `run ^ DOMAIN_TAPE != run ^ DOMAIN_FILL` for all `run`),
and that argument no longer covers the tape side, because `tape_for` composes
FNV over variable-length input ahead of the final splitmix64 and FNV is not
injective on u64.

So the header comment is amended alongside the field: keep the bijection
paragraph as the account of the `fill` root, and add that the per-symbol tape
roots are related by a HASH, that collisions therefore exist in principle, and
that the pairwise-distinctness assertions below are SAMPLES rather than the
proof the fill side gets. The file's existing comment discipline - which exists
precisely because a reader meeting three sampled assertions suspects an unproven
collision - requires saying so rather than letting the new assertions inherit
the old paragraph's authority.

The claim the comment must land on is only that no REALISTIC symbol pair
collides - not that none can.

    brokkr check -p mogwai-protocol

### 5.2 The behavioral claim - the two-river test

`a_second_river_is_realized_under_its_own_def_and_chain` in `source.rs`
currently documents that it CANNOT assert distinct prints. Its doc comment is
rewritten and the assertion is added: walk both `BTCUSDT` and `SECOND` from
`TAPE_ORIGIN_NS` through `history_source`, collect the first 32 trades of
each, and assert the two price/timestamp sequences are NOT equal. The test bites only if the
two shapes in `test_rivers_with_a_second_symbol` really are identical apart
from the label - and THAT PREMISE IS UNVERIFIED AND PROBABLY FALSE AS THE
FIXTURE STANDS.

The two are not constructed the same way. BTCUSDT arrives through
`config::profile_for_symbol` and therefore through `profile_from_configured`,
which - because `btcusdt.toml` carries no `[generator]` table - overwrites
`modal_tick`, `price_decimals` and `top_sizes` with values derived from the def,
`top_sizes` becoming `TopOfBookSizes::uncalibrated(SizeGrid::from_def(&def)
.min_size)`. The `SECOND` profile in `fills.rs` is hand-built from raw
`GeneratorScalars::from_fingerprint_medians("BTCUSDT", fingerprint())` with a
different `price_precision` and none of that patching, so at minimum `top_sizes`
differs between the two. The fixture's own doc comment asserts the two "draw the
SAME numbers", which is a claim about the draw and not about the emitted prints.

The implementer therefore owes an ORDERED check, and it is the gate on this
test's value:

1. BEFORE touching the derivation, write the assertion and run it. The two
   32-trade sequences MUST come out EQUAL. That is the pre-change state the
   whole spec asserts exists.
2. If they are already unequal, the seed term is not what the test would be
   observing and the new assertion is VACUOUS. Do not accept the green. Fix the
   fixture first - build `SECOND` through the same `profile_from_configured`
   path (a configured instrument differing only in `symbol`), so the shapes are
   identical by construction rather than by inspection - and return to step 1.
3. Only once step 1 shows equal sequences does the derivation change go in and
   the assertion flip to `assert_ne`.

BITE CHECK, mandatory and by TEXT EDIT: revert `tape_for(label)` to
`tape_for("")` in `fn generator`, observe this test fail on equal sequences,
restore by text edit. Never `git checkout -- source.rs`.

    brokkr test -p mogwai-server a_second_river_is_realized_under_its_own_def_and_chain

### 5.3 The fill golden - a KNOWING re-bless

`fill_distribution_matches_the_golden` renders through a real `Rivers`, so its
BTCUSDT tape moves and the golden must change. This is a re-bless, not a
tolerance widening. Two things are checked before accepting the new file:

- The cell STRUCTURE is identical - same schema, same symbol, same
  `data_origin_ns`, `sweep_interval_ns`, `horizon_ns`, `orders_per_offset`,
  `accept_stride_ns`, same set of cells with the same offsets. Only the
  measured latency/fill statistics move.
- The band's own shape did not degenerate: the zero-tick cell still fills at
  the front of the horizon and the wide-offset cells still show the monotone
  latency ordering the golden was built to pin. If that ordering breaks, the
  cause is not the seed and the landing stops.

Procedure is the one in the file's header: delete the golden, run the test to
regenerate, inspect the diff, run again to confirm green.

    brokkr test -p mogwai-server fill_distribution_matches_the_golden

### 5.4 The whole workspace, including the version-carrying paths

`lifecycle.rs` asserts the readiness banner contains `tape <VERSION>`, so it
tracks the constant automatically; several `mogwai-cli` and `mogwai-lab`
manifests embed the constant and compare it (`stage_a_batch`'s manifest check,
the cache manifest, `arrival_control`'s `version_ok` which pins the literal
16).

`arrival_control` NEEDS MORE THAN A LITERAL BUMP, and this is the one place the
spec was wrong rather than merely thin. `b1_supporting_check` ANDs two things
into B1's verdict: `version_ok`, which pins `TAPE_PROTOCOL_VERSION == 16`, AND
`shipping.is_empty()`, where `shipping` is every path in
`<baseline_commit>..HEAD` that starts with one of `FROZEN_PATHS` and is not
classified non-shipping by `is_non_shipping`. `crates/mogwai-protocol/` is a
frozen path and `src/seeds.rs` is shipping code, so this landing puts a shipping
frozen path into the diff and B1 REFUSES no matter what `version_ok` says.
Changing 16 to 17 cannot make it pass.

The re-baseline is therefore three edits, not one, and it is a NARROW one made
knowingly:

- `version_ok` moves to 17.
- The doc comment's prose baseline moves with it. It states the accepted
  baseline in words - "`TAPE_PROTOCOL_VERSION` is the accepted baseline 16", and
  the "re-baselined from 14 to 16" sentence with its slice-1 grounds - and a
  literal updated without its prose is exactly the silent drift the comment
  exists to prevent. Add the piece-8 grounds in the same style: 17 is the
  symbol-keyed tape root; it moves the SERVER's tape for every symbol and moves
  no offline byte, because every `mogwai-cli` and `mogwai-lab` seed is a literal
  or a `--seed` value passed straight to `GeneratedSource::new*` and none routes
  through `RunSeeds` (2.1, verified in 5.6).
- B1's baseline commit and its baseline tapes are RE-TAKEN at this landing, so
  the `<baseline_commit>..HEAD` range no longer contains the protocol change.
  This is what actually clears the shipping check, and it is legitimate here for
  the same reason the version bump is: the frozen arrival paths regenerate
  byte-identically across this change, which 5.6 proves independently. Record
  the new baseline commit where the old one is recorded.

Do NOT teach `is_non_shipping` that `seeds.rs` is exempt. It is shipping code by
any honest reading, the classifier is deliberately mechanical, and widening it
to admit one file would make every future protocol change invisible to B1.

Any cached `mogwai-lab` artifacts keyed on the old version invalidate themselves
by design and regenerate.

    brokkr check
    brokkr check --gate

The second is owed because nothing here touches `mogwai-adapter`; run it
anyway per the standing pre-commit rule for any commit that could reach it,
and skip it only if `git diff --name-only` shows no adapter path.

### 5.5 The live path

The server's boot log, the per-river line and the fill path all change, so the
end-to-end path is exercised once:

    python3 scripts/smoke.py

AND THAT IS THE WHOLE COMMAND. An earlier draft prefixed `brokkr run mogwai --
serve`, which is not runnable as a step: `serve` runs ONE venue in the
FOREGROUND and never returns, so the sequence would block before reaching the
smoke run and would serve no purpose if it did - `smoke.py` is the
launcher-contract reference and spawns `mogwai serve` as its own DIRECT child.

That leaves a real gap, because the smoke's child is a subprocess whose stderr
the harness captures into `stderr_lines` and drains on a thread; the log lines
this landing changes never reach a terminal. Eyeballing them is not available,
so the evidence has to be ASSERTED where the lines actually are. Extend
`smoke.py` to assert against the captured stderr:

- the readiness/boot "run seeds fixed" line carries `run_seed` and `fill_seed`
  and NO `tape_seed`;
- exactly ONE `river materialized` line appears, carrying the served symbol and
  a `tape_seed`. Exactly one, not "one per symbol": the smoke already asserts
  `len(instruments) == 1`, so a second line would mean a river was materialized
  for a symbol the run does not serve.

Expected: smoke green with those two assertions in it. If log inspection by
hand is wanted anyway, run the venue by itself in a terminal you are willing to
lose - it is not a step in this sequence.

### 5.6 No offline artifact moved - the claim of 2.1, verified rather than
asserted

The PRIMARY gate here is a grep, not a sample. 2.1's claim is a statement about
REACHABILITY, and reachability is provable: no file outside
`crates/mogwai-protocol/` and `crates/mogwai-server/` names `RunSeeds` at all,
with the single exception of `crates/mogwai-data/src/generated/tests.rs` (a test,
updated in step 4 of section 4). So make that the gate:

    grep -rn RunSeeds crates

The verdict is that every hit is in `mogwai-protocol`, `mogwai-server` or that
one `mogwai-data` test. A hit anywhere in `mogwai-cli` or `mogwai-lab` falsifies
2.1 outright and the landing stops. This is stronger than any single generated
comparison, because it covers every offline command at once rather than the one
that happened to be run.

The gen comparison stays, as CORROBORATION rather than as the gate - it catches
an indirect route a grep for one type name would miss:

    brokkr mogwai --bench 1 -- gen --type summary --symbol MNQ

Run before and after the change on an otherwise clean tree and compare the
emitted summary. It must be byte-identical apart from the
`tape_protocol_version` field, which moves 16 to 17. If any generated number
moves, 2.1 is wrong, some offline path does route through `RunSeeds`, and the
landing stops until that path is found.

## 6. The prose owed, written with the code

Per the standing "EVERY DECISION IN THIS BLOCK OWES `reference/` AND `docs/`
PROSE" item, in the same commit:

- `reference/architecture.md`, the run-seed paragraph: a run still draws ONE
  seed, but the tape root is now per-river, derived from the run seed and the
  REQUESTED SYMBOL LABEL, while the fill root stays run-level because the
  band's draw key already carries the order's symbol. A run is a pure function
  of `(seed, config)` still; a RIVER is a pure function of
  `(seed, label, resolved bundle)`. Add the version-17 paragraph beside the
  existing 12/13/14/15 ones.
- `docs/config.md`, the `seed` paragraph: same statement in user terms - one
  seed reproduces the whole venue, and each symbol you ask for gets its own
  path off it, so two symbols served by one venue are genuinely different
  tapes even when they resolve to the same shape.
- `docs/cli.md`, the readiness-record paragraph that says `run_seed` is "the
  value that with the config, the fingerprint and `version_string` reproduces
  the served path". Singular "path" is what stops being true here: the same
  four inputs now reproduce every served path, one per symbol requested, with
  the requested LABEL as the fifth input selecting among them. One sentence,
  and it is not optional - that paragraph is the reproducibility contract a
  launcher reads.
- `docs/presets.md`: one sentence where the default-preset shape contract is
  described - an unmatched symbol wears the default preset's SHAPE but never
  its PATH, because the label enters the seed.

## 7. Stopping rule

IN scope: the derivation, its call sites, the version bump, the golden
re-bless, the tests above and the prose above.

OUT of scope, named so the absence is not read as deferral:

- Piece 9's sharing key. The seed term added here is the SYMBOL LABEL only.
  Composition, speed and generator-level havoc enter the BOAT key, not the
  river's tape root, and piece 9 owns that. The `0` separator in 3.1 is the
  only accommodation made for it.
- Piece 12's `ReadyRecord` question (dropping `symbol`, and what a seed
  reproduces once a venue is many rivers). This spec deliberately keeps
  `ReadyRecord` untouched; it becomes coherent under piece 12, not here.
- Piece 13's total resolution. Until it lands, `Rivers::river` still refuses
  an unconfigured symbol, so `tape_for` is only ever called with a label that
  matches a configured shape. The label-not-def-symbol choice in 2.3 is the
  full extent of the preparation made for it - and it carries the OBLIGATION
  stated in 2.3: piece 13 must widen `RiverKey` from `profile.def.symbol` to the
  requested label in the same change, or the label-keyed tape root it inherits
  becomes arrival-order-dependent. That is named here so a successor reading
  "prepared for piece 13" does not read it as "safe under piece 13".
- Piece 9's encoding. The `0` separator is a one-field convenience, not an
  extensible framing; the length-prefixed encoding a multi-field sharing key
  needs is piece 9's, with its own version bump. See 3.1.
- The fill stream. Argued closed in 2.2, not deferred.
- Any change to `mogwai-lab` or `mogwai-cli` seeding. Argued out of the blast
  radius in 2.1 and verified in 5.6.
