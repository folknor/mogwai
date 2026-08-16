# Spec: piece 13, the consumer surface

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/todo.md`: the section "Landing the grand design: fourteen pieces", item
13, and the Open issues bullets it points at - "THE SYMBOL IS A REQUEST
PARAMETER, NOT AN IDENTITY THE VENUE OWNS", "SYMBOL RESOLUTION IS TOTAL, AND THE
DEFAULT PRESET IS THE SHAPE CONTRACT", "FUNDING: CLOSED, and it stays a BOOT
check", "`/instruments` RETURNS THE RESOLVED CONFIGURATION", and the ruling of
2026-08-16 that SUBSCRIBING TO AN UNCONFIGURED SYMBOL IS A SUPPORTED SESSION.

## What lands

One sentence: after this, a client may bind, poll, subscribe and trade ANY
wire-legal symbol against a running venue, the venue resolves it to the default
bundle wearing the requested label, `/instruments` reports every shape resolved
so far, and no layer - server or adapter - refuses a symbol for the sole reason
that nobody configured it.

Three obligations from the inventory item, plus the two the ruling attached:

1. `/instruments` returns the resolved configuration, which after the ruling
   means the CONFIGURED shapes plus every shape whose RIVER this run has
   MATERIALIZED - a history poll or a socket bind, both of which spend the
   capped resource. A memo hit that materialized nothing does not advertise
   (adjudication ruling 3).
2. The adapter's refuse-unlisted-symbols subscription guard goes, and the
   adapter learns an unlisted symbol's shape by re-reading `/instruments` after
   the socket has bound it - behind a READINESS BARRIER, never as a post-hoc
   reseed racing the delivery pump (adjudication, directions).
3. The runtime funds rejection names its currency.
4. Socket and history resolution become TOTAL server-side, with one
   pre-trade refusal class: a shape whose currency the run cannot fund is
   FUNDING-BARRED and refused at bind (adjudication ruling 1).
5. The `RiverKey` widens to the requested label in the same change - the
   obligation pieces 8 and 9 recorded, and the thing that makes 4 sound.

This document is the CONSOLIDATED spec: reviews R1 and R2 are folded in and
the adjudication R3 is binding. Findings rejected on the adjudication's
authority are recorded at the end under "Rejected findings".

## Survey of the ground

Read out of the tree, not from memory.

### Resolution already exists and is already total

`mogwai_server::config::profile_for(cfg, Some(symbol))` is total over symbol
strings today. `resolve_instrument_named` picks the base bundle through
`bundle_name` (operator `preset` key, else a preset whose name matches the
symbol, else `DEFAULT_PRESET` = `BTCUSDT`), applies the `[instrument]` overlay
and any case-insensitively matching `[symbols.*]` overlay from
`Config::overlays_for`, and then OVERWRITES the `symbol` field with the
requested string. `config.rs` already carries tests for the total cases:
`profile_for_symbol("FOOBAR")`, `profile_for_symbol("NOPE")`,
`profile_for_symbol("mnq")`.

So the shape machinery is done. What is not done is that NOTHING AT RUNTIME
CALLS IT. Every serving path goes through a map built once at boot.

### The boot-fixed map, and every caller of it

`config::build_instrument_profiles(cfg)` resolves the boot symbol plus each
`[symbols.*]` key and hands the vector to
`source::InstrumentProfiles::from_profiles`. That type holds
`by_symbol: HashMap<Symbol, InstrumentProfile>` plus `boot: Option<Symbol>`, and
exposes `get` (exact case), `iter`, `served_symbols` (sorted, for refusals),
`instrument_defs` (sorted, for `/instruments`) and `boot_symbol_def`.

Callers of the lookup, all of which become total or explicitly configured-only:

- `source::Rivers::resolve_profile` - the single lookup in river resolution,
  and its own doc comment already says "Piece 13 makes this function return the
  default shape for an absent symbol".
- `Rivers::new` precomputes `keys: HashMap<Symbol, RiverKey>` for the configured
  profiles only; `Rivers::river` refuses any key whose symbol is not in that
  map, and refuses again if the passed key differs from the cached one.
- `Rivers::history_source` returns `History::Unconfigured` for an absent symbol;
  `arm_flow_surge` / `clear_flow_surge` return `false`.
- `run::Run::ensure_instrument` returns `false` for an absent symbol, and the
  order path deliberately falls through to the engine's unknown-instrument
  rejection.
- `http::unserved_symbol_refusal` - 400 for `/trades` and `/quotes`, and the
  error arm of `http::resolve_socket_symbol` for `/ws`.
- `http::process_order_cmd` gates its price-less-market-order refusal on
  `profiles().get(...).is_some()`, precisely so an unconfigured symbol keeps the
  engine's unknown-instrument story.
- `serve.rs` uses `profiles.get(boot)` for the advisory warmup projection and
  `rivers.resolve_profile(boot)` for the boot boat.
- `http::instruments` is `Json(state.rivers.profiles().instrument_defs())`.
- `fills.rs::history_or_warn` is the PRODUCTION consumer of
  `History::Unconfigured` the first draft missed (R1). Its doc comment states
  the distinction deliberately: an unconfigured symbol is the ordinary `None`
  and needs no log, a genuine `Err` is a `warn` naming the symbol. Collapsing
  the variant without replacing that distinction turns every resolver refusal
  on the sweep path into a per-pass warn.
- `Rivers::river` re-derives the profile from `key.symbol()` and its `None`
  becomes `"river {} is not configured"` in `place_cursor` / `ensure_reach` -
  the exact phrase this landing retires (R1).

Two construction surfaces widen with the type and are CROSS-CRATE public API,
which the first draft called "mechanical churn" without counting (R1):

- `InstrumentProfiles::from_profiles(Vec<InstrumentProfile>)` - six sites:
  `fill_golden.rs`, `run.rs`, two in `fills.rs`, `http.rs`, and
  `mogwai-cli/src/gen.rs`.
- `config::build_instrument_profiles(&Config)` - `serve.rs`,
  `mogwai-cli/src/gen.rs`, `mogwai-lab/src/fit/walk.rs`,
  `mogwai-cli/tests/common/mod.rs`, plus several `config.rs` tests.

And `Fingerprint::from_repo_json` returns `Self` BY VALUE, with at least one
caller binding it `mut` (`config.rs`, the fingerprint-mutating test) and
dozens of read-only callers across `mogwai-data`. It cannot simply "gain a
`OnceLock`" (R1): that is a signature change to a public function used across
crates.

### The funding check, and the hole the ruling opens

`build_instrument_profiles` calls `refuse_unfunded_settlement` for every
configured shape. With the boot symbol absent from config, `boot_symbol()` is
`None`, the default bundle is swept, and the default shape's currency IS
checked. But a config that boots `MNQ` with `[balances] USD = ...` and no USDT
line passes boot today and, after this landing, will serve `FOOBAR` on the
BTCUSDT bundle whose settlement currency is unfunded - a runtime funds rejection
caused by configuration, which is exactly what the funding ruling forbids.

The first draft of this spec then claimed the reachable currency set was still
closed by ONE probe of the default bundle. BOTH REVIEWS FALSIFIED THAT, and
they are right. `bundle_name` selects a bundle when the requested symbol NAMES
A PRESET, and `PRESETS` is the registry `MNQ`, `MES`, `BTCUSDT`
(`config.rs`). Once resolution is client-driven, EVERY shipped preset name is
client-reachable: a run booting BTCUSDT with only USDT funded will serve a
client-requested `MNQ` on the MNQ bundle, whose settlement currency USD was
never swept. The reachable shape set is therefore CONFIGURED SHAPES + EVERY
EMBEDDED PRESET + THE DEFAULT BUNDLE UNDER THE `[instrument]` OVERLAY - three
sources, not one.

It is still a CLOSED set, and it is still known at boot: no client input can
add a shape to it, only select one from it. What does not follow is that boot
must REFUSE over all three. Boot-refusing a run whose balances do not fund
every shipped preset would force a BTCUSDT-only operator to fund USD forever,
which is worse than the hole. The adjudication's ruling 1 splits it:
boot-refuse over configured shapes and the default bundle exactly as today,
RESOLVE the rest at boot, mark the unfundable ones FUNDING-BARRED, and refuse
a request that lands on one AT BIND. See "Funding: boot check plus bind
refusal" below.

### The runtime funds rejection

Audited. `mogwai-engine/src/orders.rs` rejects with
`format!("insufficient {currency} balance")` at every site: the submit-time
check (three arms), `validate_fill_funds` (two arms), and the amend path (two
arms, one spelling `{settlement}`). The obligation in the todo is a
CONFIRM-WHEN-IN-THERE line, not a change, and it is already satisfied. The one
neighbouring message that does NOT name a currency is the margin-breach refusal,
`"margin breach: account equity below maintenance requirement"`, which reads as
a funds outcome to a consumer and names no unit.

### The adapter

`MogwaiDataClient::subscribe_symbol` (`client/data.rs`) does two checks. The
first compares the requested symbol against `config.symbol`, the symbol this
connection's `/ws` URL bound - that one is RIGHT and stays. The second refuses
when the seeded instrument map is non-empty and lacks the symbol, with the body
"this venue run serves {served:?}, not {symbol}; one run is one instrument and
cannot be asked for another" - written for the one-instrument era, wrong under
the ruling, and the guard the todo names. Its test is
`subscribe_refuses_an_instrument_outside_the_bound_symbol`, which actually
exercises the FIRST check; nothing pins the second.

And the pre-dial seed is not the only ordering that matters. In `data.rs`
the READER and the LATENCY PUMP are both spawned before `wait_connected`
returns, and the trade and quote handlers BLACK-HOLE any frame whose
instrument def is missing (`instrument_def(...) else { return }`). So a
reseed placed after `wait_connected` races the pump: the socket attaches to
the live tape on upgrade, and the first frames can land before the second
`/instruments` read completes. R2 caught this and it is real - the reseed
needs a readiness barrier, not a placement.

In `exec.rs` the ordering is the other way round and the first draft got it
backwards: `seed_instruments` runs, the initial ACCOUNT SNAPSHOT is pulled and
booked, and only THEN is the reader spawned and `wait_connected` awaited. A
reseed "after `wait_connected` and before the snapshot" is therefore not an
insertion, it is a lifecycle reorder that moves the snapshot after the socket.
The adjudication preserves the snapshot-before-socket ordering.

Both clients seed instruments in `connect()` BEFORE the websocket is dialled:
`data.rs` calls `seed_instruments` then `emit_seeded_instruments` and only then
builds `ws_url` and spawns the reader; `exec.rs` calls `seed_instruments` before
its own connect and before the account pull. So an unconfigured symbol cannot
appear in that seed - the server has not registered it yet, because binding is
what registers it. This is the ordering the ruling's "resolve from
`/instruments` AFTER binding" clause is about, and it is the real work on the
adapter side; deleting the guard alone would leave the client subscribed with no
def, dropping every frame at `instrument_def`.

`scripts/smoke.py` no longer asserts a single instrument - it resolves the boot
symbol from the list plus the config key and asserts membership. No change owed.

## The target

### `InstrumentProfiles` becomes a total, memoizing resolver

```rust
pub struct InstrumentProfiles {
    cfg: Arc<Config>,
    configured: HashMap<Symbol, Arc<InstrumentProfile>>,
    boot: Option<Symbol>,
    resolved: Mutex<HashMap<Symbol, Arc<InstrumentProfile>>>,
}

pub enum ResolveRefusal {
    IllegalSymbol(String),
    Invalid(anyhow::Error),
    FundingBarred { symbol: String, currency: String, balances_key: String },
}

impl InstrumentProfiles {
    pub(crate) fn resolve(&self, symbol: &str)
        -> Result<Arc<InstrumentProfile>, ResolveRefusal>;
    pub(crate) fn configured(&self, symbol: &str) -> Option<Arc<InstrumentProfile>>;
    pub fn configured_symbols(&self) -> Vec<&str>;     // was served_symbols
    pub fn boot_symbol_def(&self, symbol: Option<&str>) -> anyhow::Result<InstrumentDef>;
}
```

`resolve` is: configured hit, else memo hit, else `validate_wire_symbol`, else
`config::profile_for(&self.cfg, Some(symbol))`, else the funding-barred check
(below), memoized under the EXACT requested string. The profile is handed out
as `Arc` because a `&` into a mutex-guarded map cannot be returned; every
current `&InstrumentProfile` caller either clones today (`run.rs`) or reads a
few scalars, so the churn is mechanical - but it is churn across six
`from_profiles` sites and five `build_instrument_profiles` sites, enumerated
in the survey and again in B2.

NO CAP LIVES HERE. Per adjudication ruling 2 the profile memo is CHEAP - two
small structs per symbol - and uncapped; the cap charges at river
materialization, in `Rivers`. `instrument_defs()` also moves off this type,
because under ruling 3 the advertised set is the materialized set, which only
`Rivers` knows.

ATOMICITY (R2). `resolve` performs the memo check, the construction and the
insertion under ONE acquisition protocol, not three: take the lock, look up,
and on a miss either construct under the lock or - if the TOML merge is judged
too long to hold it - construct outside and re-check on reinsertion with
`entry().or_insert_with`-style semantics, ALWAYS returning the `Arc` that is
retained in the map, never the locally constructed loser. Otherwise concurrent
callers get `Arc`s that fail `Arc::ptr_eq`, which is a property the tests
assert and the river keying quietly depends on.

`configured_symbols` keeps `served_symbols`'s `Vec<&str>` return; the first
draft changed it to `Vec<String>` for no stated reason (R1) and the rename is
the whole change.

Case: resolution stays EXACT-CASE at the map, matching today's `/ws` and history
rule, while `overlays_for` keeps matching `[symbols.*]` case-insensitively. So
`mnq` resolves the MNQ overlay under the label `mnq` and is a DIFFERENT river
from `MNQ`. That is the symbol-is-a-label model applied consistently, and it is
stated in the docs brick rather than left to be discovered.

### The cap sits on river materialization

Both reviews rejected the first draft's placement of the cap at memoization,
and the adjudication (ruling 2) settles it. The expensive resource is the
RIVER - a permanent checkpoint chain, and on a bind a boat and its paced task
(piece 9 landed no river eviction). The memo is not expensive. Charging the
memo let 256 junk `GET /trades?symbol=...` polls, which board no boat and
register nothing, permanently exhaust the run's budget.

So: `MAX_MATERIALIZED_RIVERS = 256`, a `mogwai-server` constant, charged in
`Rivers` at the point a river is CREATED - history reads and socket binds
alike, since both materialize. The refusal is a 400 naming the cap and the
current count. There is no eviction: an evicted river would resurrect at a
different position, which is a worse lie than a refusal.

Exhaustion by 256 genuinely materialized rivers is ACCEPTED as an operational
contract, not a hole to close. R2 is right that a hostile client can still
spend the budget by materializing 256 rivers; this venue serves the owner's
own agents, fire-and-forget, one consumer population per run, and the docs
(B8) state the cap and the trust assumption in so many words rather than
leaving the reader to infer a security posture the venue does not have.

`MaterializeRefusal::CapacityExhausted { cap, count }` is a distinct refusal
from `ResolveRefusal`, because it arises one layer down and only on the
materializing paths.

Cost: `profile_for` re-parses `analysis/fingerprint.json` through
`Fingerprint::from_repo_json` on every call. Once per symbol per run is
tolerable, but it also sits under `profile_from_configured`'s default path;
put a NEW `&'static Fingerprint` accessor over a `OnceLock<Fingerprint>` beside
`from_repo_json` (whose signature is untouched, see B1) and move the resolution
path onto it, so resolution is a TOML merge and nothing more.

### `Rivers` widens its key cache

`keys` becomes `Mutex<HashMap<Symbol, RiverKey>>`, seeded from the configured
profiles at construction and filled on first resolve of any other symbol.
`resolve_profile(symbol) -> Result<Arc<InstrumentProfile>, ResolveRefusal>`
delegates to `InstrumentProfiles::resolve`; `resolve_key(profile)` reads or
inserts the memo. `Rivers::river` keeps its "the passed key must equal this
symbol's key" guard, now against the widened cache - it still catches a stale
key from a bundle this run no longer resolves, and it no longer means
"unconfigured". The materialization cap is charged here, at river creation,
under the same lock that inserts the river, so the check and the insertion
cannot interleave.

`RiverKey::resolve` itself needs NO change: it already hashes
`profile.def.symbol` and `identity.seeds.tape_for(&profile.def.symbol)`, and the
resolved profile for `FOOBAR` carries `symbol = "FOOBAR"`. That is the widening
the piece-8 and piece-9 landings asked for - it falls out of the profile being
resolved under the requested label, which is why the label must never be
normalized on the way in.

`Rivers::river` gets a designed signature, which the first draft left out
(R1). It resolves, so it can now fail for reasons an `Option` cannot carry and
it can spend a cap slot:

```rust
pub(crate) fn river(&self, key: &RiverKey) -> Result<Arc<River>, MaterializeRefusal>;

pub enum MaterializeRefusal {
    Resolve(ResolveRefusal),
    CapacityExhausted { cap: usize, count: usize },
    KeyMismatch { symbol: String },
}
```

`place_cursor` and `ensure_reach` render that refusal instead of
`"river {symbol} is not configured"`. THAT WORDING IS RETIRED EVERYWHERE -
it is the sentence this whole landing exists to make false. `KeyMismatch`
keeps the stale-key guard with an honest message: the passed key does not
match the key this run resolved for that symbol.

`History::Unconfigured` disappears. `history_source` returns
`Result<Box<dyn TickSource>, MaterializeRefusal>` and its refusals are the
resolver's and the cap's, so the `build_history_source` test helper collapses
to the same shape.

`fills.rs::history_or_warn` keeps a deliberate QUIET path, narrowed (R1,
adjudication): `MaterializeRefusal::Resolve(ResolveRefusal::FundingBarred)`
is the ordinary `None`, since a barred shape is a standing configuration
state and warning once per sweep pass is noise. Everything else - an illegal
symbol reaching the sweep, an invalid resolved shape, cap exhaustion, a key
mismatch - is a genuine `Err` and warns naming the symbol, exactly as the
function's doc comment already promises. The comment is rewritten to state the
new distinction rather than the retired one.

### The server surfaces

- `/ws`: `resolve_socket_symbol` keeps the wire-legality check and the
  truncated echo, drops the profile lookup, and returns the requested label
  (or the boot symbol when the query names none). Its DOC COMMENT still claims
  non-boot rivers are refused there; that prose is stale on landing and is
  swept with the code (R1). `ws_upgrade` then resolves through
  `Run::ensure_instrument`, whose refusal becomes a 400 carrying the reason:
  illegal symbol, invalid resolved shape, FUNDING-BARRED (naming the symbol,
  the settlement currency and the `[balances]` key that would fund it), or cap
  exhausted (naming the cap and the count).
- `/trades`, `/quotes`: `unserved_symbol_refusal` is DELETED. The remaining
  refusals are wire-legality, the off-tape window check
  (`history_start_refusal`, unchanged) and the resolver's own.
- `/instruments`: `Rivers::instrument_defs()` returns the CONFIGURED shapes
  unioned with the MATERIALIZED rivers, sorted by symbol (adjudication
  ruling 3; R1 caught that the draft's memo union advertised labels nothing
  had registered). A client that binds `FOOBAR`, or merely polls
  `/trades?symbol=FOOBAR`, then sees `FOOBAR` with the default bundle's class,
  currency, grid and multiplier - because both materialize. A resolve that
  materialized nothing does not advertise. The list therefore grows exactly
  when the capped resource is spent, which is the same event, and `docs/cli.md`
  says so in those terms.
- Order path: `Run::ensure_instrument` returns
  `Result<Arc<InstrumentProfile>, ResolveRefusal>`; a successful resolve
  registers the def and installs margin and fees exactly as today, guarded on
  the registration having been new. `process_order_cmd`'s price-less-market
  guard swaps `profiles().get(..).is_some()` for "the resolve succeeded", so an
  ILLEGAL or CAP-REFUSED symbol keeps the engine's unknown-instrument story and
  a resolvable one gets the honest synthesis-failure refusal.
- Havoc: `arm_flow_surge` / `clear_flow_surge` resolve the same way, so
  generator havoc can be armed on an unconfigured symbol's river at placement -
  which is what piece 9's fork-at-placement narrowing requires. `broadcast` and
  `materialized_symbols` are untouched.

### Funding: boot check plus bind refusal

Adjudication ruling 1, which replaces the first draft's single-probe design.
Three-way contract:

1. BOOT REFUSES exactly what it refuses today. A CONFIGURED shape, or the
   DEFAULT BUNDLE, whose settlement or quote currency has no `[balances]` line
   is a configuration error and `build_instrument_profiles` returns `Err`.
   Unchanged in scope; no operator who boots today stops booting.
2. BOOT RESOLVES every embedded preset shape - the `PRESETS` registry, `MNQ`,
   `MES`, `BTCUSDT` - plus the unconfigured fallback shape, and records the
   ones whose currency is unfunded as FUNDING-BARRED. Recorded, NOT refused: a
   BTCUSDT-only operator must not be forced to fund USD forever.
3. A REQUEST THAT RESOLVES TO A BARRED SHAPE IS REFUSED AT BIND - socket bind,
   history read, order - with a 400 naming the symbol, the shape's settlement
   currency and the `[balances]` key that would fund it. Loud, pre-trade, and
   configuration-class.

That preserves the distinction the FUNDING: CLOSED ruling exists to protect: a
runtime FUNDS rejection on a SERVED shape still means depletion and only
depletion. It is no longer true that "no runtime path can reject for funding
reasons an operator could have fixed" in the old one-line form, because the
reachable shape set is wider than the configured one; the amended form is that
such a case is refused at bind as a configuration error, never as a fill-time
funds rejection. The FUNDING: CLOSED bullet in `notes/todo.md` is amended by
the reconciliation step to state this three-way contract and why the old
grounds broke.

The barred set is computed ONCE at boot and stored on `InstrumentProfiles`,
keyed by bundle name; `resolve` consults it after building the profile and
returns `ResolveRefusal::FundingBarred` rather than an `Arc`. The set is closed
at boot because the shape set is - client input selects from it, never adds to
it.

The fallback shape still needs a label to resolve under, and `config.rs` gains:

```rust
/// The label the boot sweep resolves the unconfigured fallback shape under.
/// DELIBERATELY LONGER THAN `MAX_SYMBOL_LEN` (32), so no client can name it
/// and no `[symbols.*]` key colliding with it can ever be requested.
pub(crate) const UNCONFIGURED_PROBE_SYMBOL: &str =
    "MOGWAI-UNCONFIGURED-FALLBACK-SHAPE-PROBE";
```

Per adjudication ruling 4 the probe is WIRE-ILLEGAL by construction (length),
so collision is impossible and the first draft's `validate_symbol_keys` refusal
of the probe key is DROPPED as dead weight - it was also a gratuitous
restriction on an otherwise legal label (R1).

The label does not affect the answer: `bundle_name` selects the bundle from the
symbol only when it NAMES A PRESET, and the probe names none, so the probe
resolves default-bundle-plus-`[instrument]`-overlay, which is exactly what an
arbitrary non-preset string resolves to.

### Funds rejections name their currency

Already true for all seven sites. Landed here: a regression test pinning the
wording, and the margin-breach refusal widened to
`format!("margin breach: account equity below maintenance requirement in {currency}")`
so the one neighbouring rejection a consumer reads as a funds outcome carries
its unit too.

### The adapter

- `subscribe_symbol` loses the served-set block entirely. The bound-symbol check
  stays and is the only local gate: a subscription for a symbol other than the
  one this connection bound can still never be delivered.
- Both clients gain a post-bind reseed BEHIND A READINESS BARRIER. A bare
  post-hoc reseed is rejected: R2 showed it races the delivery pump, which is
  running before `wait_connected` returns and black-holes frames whose def is
  missing. The sequence is BIND THE SOCKET, HOLD INBOUND DELIVERY, RESEED AND
  EMIT THE INSTRUMENT, RELEASE DELIVERY - so no frame can reach a handler
  before the def it needs is in the cache.
- `data.rs::connect` keeps the pre-dial `seed_instruments` /
  `emit_seeded_instruments` (the executor's presence guard needs the configured
  defs before any subscription). The barrier is a gate the latency pump awaits
  before forwarding its first delivery: the pump is spawned as today, the
  reader may enqueue, and nothing is handed to a handler until the reseed has
  completed after `wait_connected`. `cache_instruments` overwrites by key, and
  `emit_seeded_instruments` re-emitting an unchanged def is idempotent at the
  nautilus cache. Queueing rather than dropping is the point: the venue starts
  the tape at upgrade and those frames are real.
- `exec.rs::connect` PRESERVES its existing ordering - seed, account snapshot,
  then spawn the reader and `wait_connected` (adjudication). The first draft
  put the reseed after `wait_connected` and before the snapshot, which is not
  an insertion but a lifecycle reorder moving the snapshot behind the socket
  (R2). The exec reseed therefore goes after `wait_connected` behind the same
  delivery barrier, and the snapshot keeps resolving against the pre-dial seed
  exactly as it does today.
- `ensure_instrument` (refetch-all-then-error) is unchanged and remains the
  self-heal for anything that arrives later.
- The stale "one run is one instrument" wording in the surrounding comments goes
  with the guard.

## Bricks, in landing order

Each brick is one commit; the suite is green at every boundary.

**B1. Cache the fingerprint parse.** `Fingerprint::from_repo_json` KEEPS ITS
SIGNATURE and its by-value return (dozens of callers across `mogwai-data`, at
least one binding it `mut`); a new `&'static Fingerprint` accessor is added
BESIDE it over a process-wide `OnceLock`, and only the resolution path is moved
onto the accessor. So per-symbol resolution costs a TOML merge. No behavior
change, and no cross-crate signature churn (R1, adjudication).
Gate: `brokkr check`.

**B2. `InstrumentProfiles` becomes the total resolver.** New fields, `resolve` /
`configured` / `configured_symbols`, the `Arc<InstrumentProfile>` handout,
`ResolveRefusal`, atomic memoization under one lock. NO CAP here - it lands in
B3 on materialization. `build_instrument_profiles` takes `Arc<Config>`.
NOTHING calls `resolve` yet - existing callers move to `configured`, so serving
behavior is bit-identical.

The cross-crate churn is enumerated, not waved at (R1). Every
`from_profiles(vec![...])` site - `fill_golden.rs`, `run.rs`, two in
`fills.rs`, `http.rs`, `mogwai-cli/src/gen.rs` - and every
`build_instrument_profiles` caller - `serve.rs`, `mogwai-cli/src/gen.rs`,
`mogwai-lab/src/fit/walk.rs`, `mogwai-cli/tests/common/mod.rs`, the `config.rs`
tests - is touched by name. THE TEST RIGS STAY NON-TOTAL EXPLICITLY: rather
than defaulting a `cfg` into `from_profiles` and silently making six rigs
total-resolving, `from_profiles` constructs a resolver with NO config that
answers `configured` only and refuses `resolve`, and a separate constructor
takes the `Arc<Config>`. `#[derive(Clone)]` goes; the type is already held
behind `Arc` at every serving site.
Gate: `brokkr check`, plus new unit tests in `config.rs` / `source.rs`:
`resolve_is_total_over_wire_legal_symbols` (an unconfigured symbol resolves to
the default bundle's class and currency under its own label),
`resolve_memoizes_one_profile_per_symbol` (two resolves return `Arc`s that are
`Arc::ptr_eq`), `resolve_refuses_an_illegal_symbol`, and
`concurrent_resolves_of_one_symbol_share_a_profile` (N threads on the same
label, every returned `Arc` `ptr_eq` to the retained one - R2's atomicity
finding).

**B3. `Rivers` widens, and the cap lands on materialization.** Lazy `keys`,
`resolve_profile` delegating to `resolve`, `History::Unconfigured` deleted,
`Rivers::river`'s new `Result<Arc<River>, MaterializeRefusal>` signature,
`MAX_MATERIALIZED_RIVERS` charged under the river-insert lock, the
`"is not configured"` wording retired from `place_cursor` and `ensure_reach`,
and `fills.rs::history_or_warn` rewritten to the narrowed quiet path.
Still no total serving path: `/ws`, `/trades`, `/quotes` keep their refusals
for now, so the widening lands under the existing tests.
RE-ANCHORED ASSERTIONS, named as such (R1): `boatyard.rs`'s
`resolve_profile("SECOND").unwrap()` and the `http.rs` refusal tests keep
passing but for a NEW reason. Each gets a comment saying what it now pins, so
neither is mistaken later for a live guard on the retired behavior.
Gate: `brokkr check`, plus `source.rs` tests
`an_unconfigured_symbol_keys_its_own_river` (the `RiverKey` for `FOOBAR` differs
from the default preset's own key and from a second unconfigured label),
`resolving_the_same_symbol_twice_returns_one_river`,
`materialization_refuses_past_the_cap` (asserts the river-map LEN, not merely
that an error came back - a test observing only an error cannot tell a bound
from a check made after the damage), and
`concurrent_materialization_at_the_cap_boundary_holds_the_bound` (R2).

**B4. Funding: boot sweep over every preset, plus the barred set.** Probe
constant (wire-illegal by length), the boot resolution of every `PRESETS` entry
and the fallback shape, the FUNDING-BARRED set stored on `InstrumentProfiles`,
and `ResolveRefusal::FundingBarred`. Boot REFUSAL scope is unchanged:
configured shapes and the default bundle only. No `validate_symbol_keys`
change (adjudication ruling 4).
Gate: `brokkr check`, plus `config.rs` tests
`a_run_that_cannot_fund_the_default_bundle_refuses_at_boot` (unchanged
behavior, re-anchored), `an_unfunded_preset_shape_is_barred_not_refused` (boot
BTCUSDT funded only in USDT, assert boot SUCCEEDS and `MNQ` is barred naming
USD), and `funding_every_preset_bars_nothing`.
Bite-check: revert the barred-set population as a text edit, watch
`an_unfunded_preset_shape_is_barred_not_refused` fail on the barred assertion,
restore it as a text edit. Never `git checkout -- <path>`.

**B5. The server surfaces go total, WITH the socket-backed proof.** B5 and B6
merge (R2 caught that the old B5 was gated by a test the old B6 had not yet
written, contradicting the green-boundary rule). `/ws`, `/trades`, `/quotes`,
`Run::ensure_instrument`, `process_order_cmd`'s guard, `unserved_symbol_refusal`
deleted, the bind refusal for funding-barred shapes, `/instruments` unioned with
the MATERIALIZED set, and the stale `resolve_socket_symbol` doc comment swept.
This is the intrusive one, and it is the keep/revert unit for the serving
contract.

The test lands in the same commit, ideally written first: a new integration
test in `mogwai-cli` (`CARGO_BIN_EXE_mogwai` lives only there), file
`crates/mogwai-cli/tests/unconfigured_symbol.rs`, `#[ignore]`d like its
socket-bound neighbours.
`a_run_serves_a_symbol_nobody_configured` - boot a venue on the default config,
`GET /instruments` and assert the unconfigured label is ABSENT (and note in the
test why: nothing has materialized it yet, and a later poll WOULD), open
`/ws?symbol=FOOBAR`, drain frames to a deadline (never assert on the NEXT frame:
every socket attaches to the live tape on upgrade) until a trade or quote
carrying `FOOBAR` arrives, then `GET /instruments` again and assert `FOOBAR` is
present with the default preset's class, currency and price increment, and
finally `GET /trades?symbol=FOOBAR` and assert a non-empty history.
A second case, `a_history_poll_alone_materializes_and_advertises`, pins
ruling 3's other half.
That test IS the instrument for the whole landing: no existing harness can
observe "servable but unconfigured".
Open dependency to confirm while implementing, not to assume (R1): the live
`FOOBAR` frames require BOATYARD PLACEMENT for a brand-new non-boot river to
succeed and wind up inside the deadline. If it does not work today, that is
this brick's work, not a test-timing problem to paper over.
Gates:
- `brokkr check`.
- `brokkr test -p mogwai-cli a_run_serves_a_symbol_nobody_configured`.
- The live path: `brokkr run mogwai -- serve` in one shell, then
  `python3 scripts/smoke.py`.
Bite-check: text-edit `resolve` back to a configured-only lookup, watch this
test fail on the upgrade, restore.

**B6. Funds-rejection wording.** The margin-breach currency, plus
`brokkr test -p mogwai-engine insufficient` over a test asserting a depletion
rejection names the currency.
Gate: `brokkr check`.

**B7. The adapter.** Guard deleted, the readiness barrier and post-bind reseed
in both clients (exec keeps snapshot-before-socket), comments corrected,
`subscribe_refuses_an_instrument_outside_the_bound_symbol` kept (it exercises
the bound-symbol check, which survives), and a new
`subscribe_accepts_a_symbol_absent_from_the_seeded_set` pinning the deletion.
Gates: `brokkr check --gate` - the plain check cannot see `adapter_smoke`,
`data_client_transport`, `havoc` or `reconciliation`, and two regressions have
shipped red through that gap. Plus
`brokkr test -p mogwai-adapter adapter_smoke` and a new case inside it driving a
data client whose `config.symbol` is an unconfigured label through connect,
subscribe and a delivered trade - which is the only place the reseed ordering is
observable. The barrier needs its own bite: a case in which the venue's FIRST
frame after upgrade carries the unconfigured symbol must still be DELIVERED,
not black-holed, which is what distinguishes the barrier from the racy
post-hoc reseed.

**B8. The prose.** `docs/presets.md` (the symbol is a label; the three-step
resolution and its total third step; the default preset as the SHAPE CONTRACT
for every unnamed symbol; exact-case labels versus case-insensitive overlay
matching), `docs/config.md` (the boot sweep, the funding-barred set, the
bind-time refusal and what it says), `docs/cli.md` (`/instruments` semantics:
configured plus MATERIALIZED, that a history poll grows the list, the 256-river
cap and the trusted-input operational contract), `reference/architecture.md`
(the resolver, the uncapped memo, the cap on materialization, the widened
`RiverKey`, and why sharing stays sound). Bundled with B7's code per the
standing rule that markdown never commits alone.
Gate: `brokkr check` (gremlins runs over the docs).

## Stopping rule

Out of scope, named rather than deferred:

- **Piece 14's full durable sweep.** B9 writes the prose for the decisions THIS
  spec implements. The rest of the fourteen-piece prose debt - one clock per
  boat, the sharing key, exogeneity and no-queue-competition - is piece 14's.
- **The boatless-river sweep gap** that piece 9 opened and piece 10 left open: a
  resting order on a river whose boat wound down is not swept. This landing
  makes it reachable by more symbols but does not create it and does not fix it.
  R1 is right that the 256-river cap is now the only thing bounding how many
  rivers can reach it; that is another reason the cap sits on materialization
  rather than on the memo, and it stays a piece-14-or-later item.
- **River eviction.** The cap bounds the damage; eviction needs per-boat
  temporal ownership the engine does not have, and an evicted river would
  resurrect at a different position - a worse lie than a refusal
  (adjudication ruling 2).
- **Any defence against a hostile client.** Exhaustion of the 256 rivers by a
  caller that materializes them deliberately is ACCEPTED (ruling 2). The venue
  serves the owner's own agents, one consumer population per run.
- **Distinct-speed cohabitation** and **mid-run generator havoc on shared
  water**, both explicitly reversible narrowings from piece 9.
- **broadarrow's item 4.** Their `run_prep::mogwai_facts` refuses a
  `/instruments` answer of anything but exactly one instrument. Their build
  breaking loudly when this lands is the designed handoff, not a regression to
  absorb here.
- **Tape generation.** Nothing in this spec touches a generator constant, the
  fingerprint, seed derivation or the fill band's draw, and no committed
  artifact moves: an unconfigured symbol gets a NEW tape, never a changed one,
  and its seed comes from the symbol dimension piece 8 already landed. No
  `TAPE_PROTOCOL_VERSION` bump is owed. Stated explicitly because the rule is
  unconditional and nothing can detect a missed bump.

## Rejected findings

Every finding in R1 and R2 was checked against the tree and every one was
substantively valid. What is rejected is not a finding but an ALTERNATIVE some
of them proposed, where the adjudication picked the other branch.

1. **R1#1's "boot-sweep every embedded preset AND REFUSE"** and R2#1's first
   option. The diagnosis is accepted in full and drives the new funding
   section; the remedy is rejected per ADJUDICATION RULING 1, because
   boot-refusing over shapes no configured symbol uses would force a
   BTCUSDT-only operator to fund USD forever. Boot resolves them and bars the
   unfunded ones instead.
2. **R2#1's second option, "on-demand resolution always uses the default shape
   even when the label names a preset".** Rejected per RULING 1, which keeps
   the reachable shape set as configured plus every preset plus the default
   bundle. It would also make a client-requested `MNQ` silently a BTCUSDT
   shape wearing an MNQ label, which is a worse surprise than a bind refusal
   and contradicts `bundle_name`'s documented three-step precedence.
3. **R1#2's "let the profile memo be cheap AND EVICTABLE".** The cheap-and-
   uncapped half is adopted; EVICTION is rejected per RULING 2. No eviction
   policy is built anywhere in this landing.
4. **R2#3's "ownership or reclamation policy" for the cap.** Rejected per
   RULING 2: the exhaustion case is accepted as an operational contract and
   documented as one, which is the third branch R2 itself offered.
5. **R1#3's "or the prose must state that polling also grows the list".**
   Rejected in favour of the other branch it offered: RULING 3 makes the union
   the MATERIALIZED set, so polling grows the list because it materializes, and
   the prose says that rather than excusing a memo-shaped list.
6. **R2#2's "alternatively, add a server operation that resolves the symbol
   before the websocket starts streaming".** Rejected in favour of the
   readiness barrier, per the adjudication's directions. A pre-bind resolve
   endpoint would add a second way to materialize a river - a second place to
   charge the cap and a second thing to keep consistent with `/instruments` -
   to solve an ordering problem the barrier solves inside the client.
7. **R1#8's belt-and-braces reading.** Adopted, not rejected: the probe becomes
   wire-ILLEGAL and the `validate_symbol_keys` change is dropped (RULING 4).
   Recorded here because it removes work the first draft specified.
