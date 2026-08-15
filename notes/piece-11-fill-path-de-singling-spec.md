# Piece 11: fill-path de-singling - implementation spec

Written against `reference/technical-implementation-spec.md`. Spawned from
`notes/todo.md`: piece 11 of "Landing the grand design: fourteen pieces", and
the design bullets it points at under Open issues - "THE SYMBOL IS A REQUEST
PARAMETER, NOT AN IDENTITY THE VENUE OWNS" (the paragraph beginning "What DOES
remain single-symbol in the fill path is slice-2 work") and "THE RIVER AND THE
BOAT: how N tapes are shared".

## 1. What piece 11 asked for, and what is already done

The piece as originally scoped had two limbs. One has been absorbed by earlier
landings; only the other remains.

ALREADY DONE, verified in the tree, not to be re-litigated by this spec:

- THE SWEEP WATERMARK IS PER BOAT. `last_swept_ns` is a field on `Boat`
  (`boatyard.rs`), initialized to the yard's origin at placement and advanced
  in `sweeper.rs` under `frontier_after`. There is no process-wide settlement
  frontier left. The todo's own text says the "process-wide `last_swept_ns`"
  framing is stale, and the code agrees.
- THE SWEEP CADENCE IS PER BOAT. `spawn_fill_sweeper` keeps an
  earliest-deadline map keyed by `BoatKey` and re-arms each boat on its own
  `SimClock`, floored at `MIN_SWEEP_WALL`. One task, per-boat due instants.
- THE SWEEP WALK IS PER SYMBOL, and was never singular. `pending_scans` are
  filtered per boat, `fills::scan_triggers` walks once per symbol for all of
  that symbol's scans, and marks/settlements resolve per symbol through
  `profiles`.
- THE HISTORY LOCK IS PER RIVER. `Rivers` keys one lock per river, so two
  symbols synthesizing concurrently do not queue on each other. This is what
  makes the remaining contention below purely an artifact of the memo, not of
  the tape machinery under it.
- THE ENGINE PASS RUNS ON THE BOAT'S CLOCK (`apply_engine_pass_on_clock`), and
  marks/settlements are exact-instant reads that deliberately bypass the memo.

WHAT REMAINS, and it is the whole of this spec: `fills::MarketReadingCache` is
ONE entry behind ONE process-wide `Mutex`, held across the `read_market` walk.
Its consequences, stated as defects rather than as inefficiency:

1. MUTUAL EVICTION. The entry carries a `symbol` field and is compared on it,
   so two symbols submitting alternately inside one sweep-interval bucket each
   evict the other and every submit pays a full miss - the ~9.8 ms
   `VOL_WINDOW_NS` walk measured by
   `read_market_latency_stays_within_submit_budget`. The memo's entire benefit
   disappears at exactly the point the venue starts serving more than one
   symbol, which is the point piece 7/9/10 have now reached.
2. CROSS-SYMBOL SERIALIZATION. The lock is held ACROSS the walk on purpose (so
   two callers in the same bucket pay for one walk), but the lock is global.
   With (1) guaranteeing a miss, a submit on MNQ blocks a submit on BTCUSDT for
   the length of a 300-second window walk on a river it has no relationship
   with. Every other per-river seam - the boatyard, the river locks, the sweep
   watermark - has already been de-singled; this is the last shared mutex on
   the order-entry hot path.

OUT OF SCOPE, named so it is not read as deferral:

- Lever one of the `read_market_latency_stays_within_submit_budget` KEEP/REVERT
  rule (re-scoping `VOL_WINDOW_NS` or the estimator). It moves the estimator's
  identity and re-blesses the fill golden; it is its own todo item and is
  untouched here. This spec changes WHO pays a miss, never what a reading is.
- Putting the reading instant on `OrderFilled` (the exactly-stated slippage
  contract). Separate todo item, unaffected either way.
- The boatless-river sweep gap created by piece 9 (a resting order on a river
  whose boat wound down is not swept). Explicitly still open, explicitly not
  this piece's - the todo says so and piece 10 left it alone too.
- Piece 12 (`ReadyRecord` under per-boat clocks) and piece 13 (the consumer
  surface). No overlap.
- The bucket-staleness behaviour contract. Bucketing by sweep interval stays
  exactly as documented on `MarketReadingCache`; only its OWNERSHIP moves.

## 2. The survey: every reading call site, traced

`MarketReadingCache::read` has exactly ONE production caller. That is the fact
the whole design rests on, so it is traced rather than asserted:

- `serve.rs` constructs `Arc<MarketReadingCache>` and stores it as
  `AppState::market_readings`.
- `http.rs::market_reading` clones that `Arc` into a `spawn_blocking` and calls
  `read`. Nothing else calls it.
- `http.rs::market_reading` is called from exactly one place,
  `process_order_cmd`, which is called from exactly one place,
  `ws::dispatch_command`, which is spawned by `spawn_command_dispatcher` from
  `handle_socket` - a websocket session that OWNS a `Ticket`, hence an
  `Arc<Boat>`.
- The symbol the reading is taken for is always the socket's river. A submit's
  wire symbol is checked against `socket_symbol` and refused on mismatch BEFORE
  the reading (the placement-is-the-contract check in `process_order_cmd`); a
  price amend resolves to `socket_symbol` by construction.

Therefore: every market reading in the venue is taken by a passenger, for that
passenger's own river, on that boat's clock. There is no boatless reading path
to preserve, and no second consumer to keep an `AppState`-level cache alive
for. `read_last` (marks, settlements, the price-less market stamp fallback) is
uncached and stays uncached.

Other readers of the type, all test-local and all unaffected by a move that
keeps the type's API shape: `fills.rs` tests
`market_reading_cache_recovers_after_a_poisoned_lock` and
`read_market_latency_stays_within_submit_budget` construct a bare
`MarketReadingCache::default()` and call `read`; both take a one-line
constructor change under section 3.1 and keep their assertions. The comment in
`mogwai-cli/tests/serving.rs` describing how a market submit is decided remains
true verbatim.

## 3. The target

ONE boat owns ONE memo. `MarketReadingCache` stops being a run-level singleton
and becomes a field of `Boat`, alongside `last_swept_ns` - the two per-boat
pieces of fill-path state then sit together and are governed by the same
lifetime.

Why the boat and not a keyed map in `AppState`: a `Mutex<HashMap<..>>` of
per-symbol entries would need eviction (an unbounded map keyed by an
open symbol set is a leak, and the symbol set is open by design), would still
take one global lock to reach a per-symbol lock, and would duplicate a lifetime
the boatyard already tracks exactly. A boat is placed per river, wound down
when its last passenger leaves, and is already the thing that owns "now" for
that river. The memo dying with the boat is correct: the bucket it holds is
computed on that boat's clock and means nothing after the boat is gone.

One boat per river is guaranteed by the boatyard's own registry - `boats` is
keyed by `RiverKey`, and a second speed on a seated river is refused with
`BoardRefusal::SpeedInUse` - so "the boat's memo" is unambiguously "the river's
memo".

### 3.1 Concrete artifacts

`crates/mogwai-server/src/fills.rs`:

```rust
pub(crate) struct MarketReadingCache {
    /// The one river this memo is allowed to hold a reading for. Set at
    /// construction, never compared per read: it is the cache's identity, not
    /// a per-entry tag.
    symbol: String,
    entry: Mutex<Option<CachedMarketReading>>,
    #[cfg(test)]
    walks: std::sync::atomic::AtomicU64,
}

struct CachedMarketReading {
    bucket_ns: u64,
    mult_bits: u64,
    max_ticks: u32,
    reading: Option<MarketReading>,
}

impl MarketReadingCache {
    pub(crate) fn for_symbol(symbol: &str) -> Self;

    pub(crate) fn read(
        &self,
        ts: u64,
        rivers: &source::Rivers,
        mult: f64,
        max_ticks: u32,
        interval_ms: u64,
    ) -> Option<MarketReading>;

    /// Walks actually performed. The memo's hit/miss split is invisible in the
    /// returned value - a hit and a miss return the same reading - so this is
    /// the only way a test can gate the memo without timing it. The increment
    /// happens INSIDE the entry lock, on the same critical section that
    /// performs the walk, so a concurrent reader cannot observe a count that
    /// disagrees with the number of `read_market` calls made.
    #[cfg(test)]
    pub(crate) fn walks(&self) -> u64;
}
```

The symbol MOVES from the entry to the cache: `CachedMarketReading::symbol` and
the per-read `cached.symbol == symbol` comparison are deleted, together with
the `symbol.to_owned()` allocation per miss, and one owned `String` is stored
once at construction instead. The `symbol` PARAMETER of `read` is deleted with
it - `read_market` still needs a symbol to reach the river and the profile, and
now takes the cache's own.

This is deliberately NOT "delete the field because a boat is one river". A
cache that takes a symbol it does not check is a type whose correctness lives
three files away in the boatyard's placement invariant: a future caller that
reuses one cache across two symbols would get a SILENTLY WRONG reading rather
than a miss, and the type is still cheap to construct in a test. Binding the
symbol into the cache keeps the whole allocation saving, keeps the walk's
symbol argument, and makes the mis-keying unrepresentable rather than merely
unreachable. `Default` is therefore NOT derived; the two in-crate tests that
construct `MarketReadingCache::default()`
(`market_reading_cache_recovers_after_a_poisoned_lock` and
`read_market_latency_stays_within_submit_budget`) switch to
`MarketReadingCache::for_symbol("BTCUSDT")`, which is the symbol they already
read for.

`mult_bits` and `max_ticks` stay in the entry. They are run-global config
today, but the grand design's precedence rule (default knobs < preset knobs <
operator knobs, all per symbol) makes them per-symbol quantities in waiting,
and keeping the guard costs a `u64` compare.

`crates/mogwai-server/src/boatyard.rs`:

```rust
pub(crate) struct Boat {
    key: BoatKey,
    pub(crate) speed: f64,
    pub(crate) sim: SimClock,
    pub(crate) tape: Arc<Tape>,
    pub(crate) published_ns: Arc<AtomicU64>,
    pub(crate) last_swept_ns: AtomicU64,
    /// This river's acceptance-time market reading, memoized per sweep-interval
    /// bucket on THIS boat's clock. Per boat because the bucket is a function
    /// of the boat's clock and the walk it saves is a walk of this river only:
    /// a run-level memo held one entry, so two symbols evicted each other into
    /// a guaranteed miss and then serialized on the walk behind one mutex.
    pub(crate) market_readings: crate::fills::MarketReadingCache,
    worker: Mutex<Option<JoinHandle<()>>>,
    cancel: Arc<AtomicBool>,
}
```

Constructed as `MarketReadingCache::for_symbol(river.symbol())` in
`Boatyard::board`, next to
`last_swept_ns: AtomicU64::new(self.origin_ns)`. Not an `Arc`: the field is
reached through the `Arc<Boat>` every caller already holds.

`crates/mogwai-server/src/http.rs`:

- `AppState::market_readings` is DELETED.
- `market_reading` takes the boat instead of reaching into state:

```rust
async fn market_reading(
    msg: ClientMessage,
    state: &AppState,
    boat: &Arc<Boat>,
    ts: u64,
    socket_symbol: &mogwai_protocol::Symbol,
) -> (ClientMessage, Option<mogwai_engine::MarketReading>)
```

  The `spawn_blocking` closure captures `Arc<Boat>` (cloned) rather than
  `Arc<MarketReadingCache>` and calls `boat.market_readings.read(..)`. Nothing
  else about the function changes - same bucket arithmetic, same
  `read_last` fallback, same price-less-market stamp, same failure handling.

- `process_order_cmd` replaces its `sim: SimClock` parameter with
  `boat: &Arc<Boat>` and reads `let sim = boat.sim;` at the top. This is not
  incidental tidying: the reading and the clock that dates it must come from
  the same boat, and passing them separately is a seam where they can disagree.
  Every existing `sim_now_ns(sim)` call inside is unchanged.

`crates/mogwai-server/src/ws.rs`:

- `dispatch_command` and `spawn_command_dispatcher` carry `Arc<Boat>` in place
  of `SimClock`; `handle_socket` passes `Arc::clone(session.ticket.boat())`
  where it currently passes `boat_sim`. `boat_sim` stays for the writer, the
  exec pump, the heartbeat and the completion frames - those want a clock, not
  a boat, and there is no reason to hand them more.
- Holding an `Arc<Boat>` in the dispatcher does NOT extend the boat's life as a
  passenger: passenger counting is the `Ticket`'s, and wind-down cancels the
  tape worker and removes the seat regardless of outstanding `Arc<Boat>`
  clones. That is the WHOLE argument, and it is sufficient. An earlier draft of
  this spec added a second, stronger claim - that the dispatcher is joined
  during teardown, before the session's `Ticket` drops, making the clone
  strictly shorter-lived than the seat - and that claim is FALSE: teardown calls
  `dispatcher.abort()` and never awaits the handle, so cancellation is not
  observed before `handle_socket` returns and drops the ticket, and a
  `spawn_blocking` reading already in flight can outlive the abort while still
  holding its cloned boat. The claim is struck rather than repaired: nothing in
  this piece depends on it, and awaiting the dispatcher during teardown would be
  a lifecycle change well outside section 7's stopping rule. What an outliving
  `Arc<Boat>` can do is keep the boat's allocation (and its memo) alive past the
  seat; it cannot keep a passenger seated, cannot resurrect the cancelled tape
  worker, and cannot make a stale reading reachable, because the only way back
  to that memo is through a live socket's ticket.

`crates/mogwai-server/src/serve.rs`:

- The `let market_readings = Arc::new(fills::MarketReadingCache::default());`
  line and the `market_readings` field in the `AppState` literal are deleted.
  Nothing replaces them; the boot boat placed just above already carries one.

### 3.2 What the change buys, stated as behaviour

- Two symbols submitting inside one bucket each get their OWN hit path. Neither
  evicts the other; the memo works at N symbols exactly as it worked at one.
- A miss on one river no longer blocks a submit on another. The only lock a
  reading now takes is its own boat's, plus the per-river history lock the walk
  already took.
- Same-boat semantics are UNCHANGED, deliberately: two passengers on one boat
  in one bucket still share one walk, because the boat's lock is still held
  across it. That was the memo's point and it survives the move intact.
- No behaviour change to what a reading IS. Same bucket width, same staleness
  bound, same refusal semantics, same bracketed end-to-end contract.

## 4. Landing order and the keep/revert unit

ONE commit. The move is not decomposable into green intermediate states: the
field cannot be added to `Boat` and used from `http.rs` while `AppState` still
owns the authoritative one without running two memos over the same river, which
is a worse state than either end. The change is mechanical, confined to FIVE
production files - `fills.rs`, `boatyard.rs`, `http.rs`, `ws.rs`, `serve.rs` -
plus their tests and the durable prose of section 6, and its gates are named
per brick below. Any edit outside that list has left the piece; see section 7.

Brick order inside the commit:

1. `fills.rs`: move the symbol from `CachedMarketReading` onto
   `MarketReadingCache`, delete the per-read comparison and the `read` symbol
   parameter, replace `Default` with `for_symbol`, and repoint the two
   in-crate constructors; add the `#[cfg(test)]` walk counter (incremented
   inside the entry lock) and its accessor; rewrite the type doc comment (the
   "one-entry memo" and "the lock is held ACROSS the walk deliberately"
   paragraphs both need the per-boat framing; the staleness paragraph stays as
   written).
2. `boatyard.rs`: add the field and construct it in `board`.
3. `http.rs`: delete the `AppState` field, re-signature `market_reading` and
   `process_order_cmd`.
4. `ws.rs`: thread `Arc<Boat>` through the dispatcher.
5. `serve.rs`: delete the construction.
6. Tests, section 5.
7. Docs, section 6.

There is no tape-generation change anywhere in this list - no generator
constant, no fingerprint, no seed derivation, no fill-band draw, no tape origin.
`TAPE_PROTOCOL_VERSION` is NOT bumped, and this sentence is the record of that
being a decision rather than an omission. The fill BAND is drawn from a reading
whose value is byte-identical before and after; only the identity of the caller
that pays for computing it moves.

## 5. Verification

### 5.1 The bite test for mutual eviction (new)

`crates/mogwai-server/src/boatyard.rs`, in `mod tests`, because it needs two
seated boats:

```
#[tokio::test]
async fn two_boats_do_not_evict_each_other_s_market_reading()
```

Uses the existing two-symbol fixture (`fills::test_rivers_with_a_second_symbol`,
reached the way `yard()` reaches `test_rivers`), boards a boat on each river at
speed 1.0, then interleaves reads at ONE instant: A, B, A, B.

THE INSTANT IS NOT THE ORIGIN. `yard()` places at `source::TAPE_ORIGIN_NS`, and
a `VOL_WINDOW_NS` (300 s) walk backwards from the origin has no tape behind it,
so the reads would memoize a refusal rather than a walk and the counters would
pass while proving nothing about the saving. Read at
`source::TAPE_ORIGIN_NS + 86_400_000_000_000`, the instant the `fills.rs` tests
already use for exactly this reason, and assert the reading is `Some` before
asserting on the counters. Note also that in the dev profile the walks this
test forces are two real 15,000-print walks and are not free.

Asserts `a.market_readings.walks() == 1` and `b.market_readings.walks() == 1`.

THE BITE CHECK NEEDS A DIFFERENT MUTATION THAN THE OBVIOUS ONE, and the
distinction is the whole reason this subsection exists. Once the cache is a
field of `Boat` there are two counters BECAUSE there are two caches, so:

- Restoring the old per-entry `symbol` field and its comparison alone leaves
  both counters at 1. It does not bite.
- Restoring the old SHARED cache deletes the two independent counters the test
  reads. One shared cache under an A, B, A, B interleave reports FOUR walks on
  one counter, not "2 and 2" - the assertion the test makes is not even
  expressible against the reverted code.

So the honest instrument is a test-only shared-cache injection rather than a
production revert: the test constructs ONE `MarketReadingCache::for_symbol(..)`,
performs the same A, B, A, B interleave through it with the per-read symbol
argument restored as a TEXT EDIT, and observes 4 walks where the per-boat form
observes 1 and 1. Perform it, record it, and restore as a text edit; never
`git checkout -- <path>`.

Read this test for what it actually is: with the cache on `Boat`, "two boats do
not evict each other" is a TYPE-LEVEL fact, and the test is a regression fence
against someone re-introducing sharing, not a discovery instrument. That is
acceptable, but it must not be mistaken for a bite check of the defect in
section 1, which no longer has a production expression.

### 5.2 The memo still memoizes on one boat (new, same module)

```
#[tokio::test]
async fn one_boat_pays_for_one_walk_per_bucket()
```

Two reads at two instants inside one `interval_ms` bucket on one boat, then a
third in the next bucket: `walks()` reads 1, then 2. This is the half section
5.1 cannot see - a memo that never caches at all also passes 5.1. Same instant
discipline as 5.1: base the three reads at
`source::TAPE_ORIGIN_NS + 86_400_000_000_000`, not at the yard's origin, or the
test proves only that a `None` is cached.

Command for both: `brokkr test -p mogwai-server boats_do_not_evict` and
`brokkr test -p mogwai-server one_boat_pays_for_one_walk_per_bucket`.

### 5.2a What is NOT gated by a test, stated plainly

Both new tests call `boat.market_readings.read(..)` directly, so NOTHING in
section 5.1 or 5.2 exercises the threading change in `http.rs` and `ws.rs`. A
regression in which `market_reading` reaches a boat other than the ticket's, or
the dispatcher is handed the wrong boat, passes both tests green. That threading
is gated by `brokkr check` COMPILING plus the end-to-end reading contract in
5.3, and by nothing else. The residual risk is small - `handle_socket` has the
ticket in hand and there is no second boat in scope to pass by mistake - and the
piece accepts it rather than growing a socket-level fixture. This paragraph is
the record of that being a decision.

### 5.3 Existing gates that must stay green unchanged

- `brokkr check` - gremlins, clippy and the whole dev-profile suite. This is the
  gate for the signature churn in `http.rs`/`ws.rs`/`serve.rs`: everything that
  compiles the router and the socket path is in it.
- `market_reading_cache_recovers_after_a_poisoned_lock` (`fills.rs`) - the
  poison recovery is untouched by the move and must still pass:
  `brokkr test -p mogwai-server market_reading_cache_recovers_after_a_poisoned_lock`.
- The end-to-end reading contract, which is the real semantic gate on this
  change - it asserts the bracketed fill statement for both the priced and
  price-less market paths:
  `brokkr test -p mogwai-cli a_market_submit_takes_a_reading_on_both_the_priced_and_priceless_paths`.
- The live path:  `brokkr run mogwai -- serve`, then `python3 scripts/smoke.py`.
- Adapter-visible surface is untouched, but the socket path is: run
  `brokkr check --gate` before committing, per the standing rule.

### 5.4 The latency instrument, and its re-reading

`read_market_latency_stays_within_submit_budget` is `#[ignore]`d and constructs
its own single-symbol cache (a one-line change from `default()` to
`for_symbol("BTCUSDT")`, per brick 1), so it neither breaks nor needs a
behavioural change. IT IS NOT A GATE ON THIS PIECE and must not be cited as one:
a bare single-symbol cache cannot observe an ownership move that only shows up
at two symbols, so it would read the same whether the change landed correctly,
landed wrongly, or never landed. It SHOULD be re-run
once after the landing, because its recorded numbers are the baseline this
piece's claim is measured against:

`brokkr test -p mogwai-server read_market_latency_stays_within_submit_budget --timeout 280`

EXPECTED: unchanged - miss median ~9.8 ms, hit ~0.1 ms on host `bygg`. This
change does not touch the walk. A moved number means something else moved and
is a stop-and-investigate, not a re-bless. Do NOT edit its recorded measured
state in the comment on the strength of one re-run.

PROCEED/CLOSE THRESHOLD: none applies. This spec is not justified by an
estimated throughput win, so it does not owe a measurement-first landing - it is
justified by a correctness-shaped defect (one symbol's submit blocking another's
and both losing the memo), which the counter tests in 5.1 and 5.2 fence
directly - subject to 5.2a on what they do not reach.

## 6. Durable prose owed with the code

Per piece 14's standing rule, written WITH the change, in the same commit:

- `reference/architecture.md`. An earlier draft wrote this obligation as
  "wherever the boat's per-river state is enumerated, add `market_readings` to
  that list" - and NO SUCH LIST EXISTS in `architecture.md` or `clock.md`, so as
  written the obligation resolves to nothing and would be discharged as a
  silent no-op. Two concrete edits replace it:
  - CREATE the enumeration. Add the boat's per-river state - `sim`, `tape`,
    `published_ns`, `last_swept_ns`, and now `market_readings` - as a short
    named list in the boat/river passage, with the one-sentence reason for the
    memo (the bucket is a function of the boat's clock, and the walk it saves
    is a walk of one river).
  - REPAIR the stale paragraph in the same passage. It currently claims "Boat
    placement has not [landed], so exactly one paced tape is placed at boot and
    socket resolution still accepts only the run's boot symbol". Pieces 7, 9
    and 10 falsified that: `handle_socket` resolves the query symbol, calls
    `ensure_instrument`, resolves a `RiverKey` and boards a boat per river. The
    related "the top-level boot symbol is a slice-1 lifecycle artifact while one
    run still serves one symbol" sentence later in the same file is stale for
    the same reason. This commit is already editing that file, and leaving a
    known-false claim in a must-be-true document beside a true new one is worse
    than not touching it.
- `reference/clock.md` - it states no per-quantity bucketing table today, so if
  the repair above is enough there is nothing owed here. If a bucketing
  statement is added, the acceptance-time reading's bucket is a boat-clock
  quantity.
- `docs/` needs no change: the reading's staleness bound, the band's meaning and
  every wire contract are identical.
- `notes/todo.md` - piece 11 marked landed in the fourteen-piece inventory the
  way pieces 7, 9 and 10 were (detail to git history), and the "What DOES remain
  single-symbol in the fill path" paragraph under the symbol-is-a-request-
  parameter bullet rewritten to say it no longer does. The boatless-river sweep
  gap paragraph stays exactly as it is - this piece does not touch it.

## 7. Stopping rule

The teardown stops at the boundary of `MarketReadingCache`'s ownership and the
signatures needed to reach it from the socket. It does not touch: `read_market`
or `read_last` themselves, `VOL_WINDOW_NS`, the sweeper, the engine, the
protocol, the boatyard's placement/wind-down mechanics, the river registry, or
any generated byte. If the implementation finds itself editing `sweeper.rs` or
`mogwai-data`, it has left the piece. It also does NOT change socket teardown to
await the dispatcher, however tempting section 3.1's struck lifetime claim makes
that: it is a lifecycle change with its own reasoning to do, and nothing here
depends on it.

## 8. Review disposition

Two independent reviews of the pre-revision spec (Claude Opus, and codex
gpt-5.6-sol on the deep profile). Every finding of both was checked against the
tree; ALL were valid and ALL are folded in above. Recorded so a later reader
does not re-derive them:

- BOTH reviews, independently, on the section 5.1 bite check: the prescribed
  mutation cannot produce the stated failure. Folded into 5.1 as a test-only
  shared-cache injection, plus the honest statement that the defect has no
  production expression once the cache is a `Boat` field.
- Claude, on deleting the `symbol` field: it trades a self-checking type for a
  caller convention held three files away. Folded into 3.1 by binding the symbol
  to the CACHE (`for_symbol`) instead of deleting it.
- Claude, on 5.2 at the fixture origin: a walk backwards from
  `TAPE_ORIGIN_NS` has no tape behind it, so the counters would gate a cached
  refusal. Folded into 5.1 and 5.2 as an explicit late instant and a `Some`
  assertion.
- Claude, on the untested threading: neither new test touches `http.rs` or
  `ws.rs`. Folded in as the new 5.2a, which accepts the risk in writing rather
  than growing a fixture.
- Claude, on section 6: the named doc obligation resolves to a list that does
  not exist, and `architecture.md` carries a paragraph pieces 7/9/10 already
  falsified. Folded into 6 as two concrete edits.
- codex, on the dispatcher lifetime: teardown calls `dispatcher.abort()` and
  never awaits, so the "joined before the ticket drops" proof is false. The
  claim is STRUCK in 3.1 rather than repaired, with the reason.
- Both, on scope accounting: "four files" versus five named. Corrected in 4.
- Claude, on the walk counter: the increment must sit inside the entry lock.
  Folded into the 3.1 doc comment.
- Claude, on 5.4: the latency test cannot observe this change either way.
  Folded into 5.4, which now says outright that it is a baseline refresh and not
  a gate.

NOTHING WAS REJECTED. Two observations were noted but are not findings and
carry no spec edit: Claude's point that the no-bump argument is stronger than
section 4 states (`read_market` is always called with `bucket_ns`, never `ts`,
so a hit and a miss are byte-identical by construction) - section 4's weaker
argument is already sound, so it stands as written; and codex's closing
agreement that the underlying design matches the call graph and the per-river
lifecycle, which both reviews reached independently.
