# Glossary scope pass 5: where the water is made

Inventory of `mogwai-data` and `mogwai-lab` - the `TickSource` seam and its two
tape origins, the checkpoint and segment machinery, the arrival, cadence and
generation vocabulary, `TAPE_PROTOCOL_VERSION` and what it governs; and the
lab's corpus, measurement, fingerprint, cadence, fit, storage and sidecar
surfaces - measured against `reference/glossary.md` as it stands 2026-08-21
(the revision that added Boarding, renamed Client to Consumer, and clarified
River and Divergence). Nothing was edited but this file.

Scope split: `notes/glossary-scope-1.md` covers `mogwai-protocol`, `-2.md` the
venue's external surface, `-3.md` the venue's internal domain model, `-4.md`
the engine's public API. Their rows are not re-derived. Where this surface
CONTRADICTS or EXTENDS one of them it is marked **[extends P1/P2/P3/P4]** or
**[contradicts]** and says how.

Direction key: **1** a glossary term doing a job that is not that term's; **2**
a job the glossary already names, under a different word; **3** load-bearing
and undefined; **structural** a glossary claim about the water checked against
the code that makes it; **inherited** vocabulary this project does not own.

Reach key: `local` crate-local; `cross` across a crate boundary; `artifact` a
committed JSON key or an on-disk format an operator or another implementation
reads; `wire` externally visible through the venue.

**Second independent pass reconciliation (2026-08-21).** The second pass read
the revised glossary and the two crates before opening this report. That order
was deliberate because the first report was an anchoring hazard. Its changes
are marked **[P2 ADDED]**, **[P2 CHANGED]**, and **[P2 REFUTED]**. Refuted rows
remain in place with the code reading that defeats them.

---

## The headline

The fourth question - has the generating vocabulary drifted from the serving
vocabulary - is answerable in one measurement, and the answer is more extreme
than a drift.

1. **THE WATER VOCABULARY DOES NOT EXIST IN THE CRATES THAT MAKE THE WATER.**
   **[P2 CHANGED]** Grepped over the current production sources of both crates:
   `river` appears once, in the `TAPE_PROTOCOL_VERSION` history; `boat`,
   `boarding`, `passenger`, `boatyard`, and `seat` appear zero times. Including
   tests adds one more `river`. The first pass's count of four is stale, but its
   substance holds. The two crates that synthesize every byte the venue ever
   serves speak a completely disjoint vocabulary from the six glossary entries
   that describe what is served. The generator's own word for "one river's
   water" is REALIZATION (`CheckpointIndex`: "shared per realization (one
   symbol's clean tape)"); the server's word for the same object is River. No
   document connects the two, and neither word is defined where the other one
   lives. This is not a rename: it is the seam between the two halves of the
   system having no shared noun at all.

2. **`warmup` MEANS TWO DIFFERENT THINGS, AND BOTH SENSES ARE IN
   `mogwai-lab`.** The glossary's Warmup is the uniformly servable simulated
   history from `data_origin_ns` to `run_start_ns` - the thing history answers
   from. In `fit::walk`, `arrival_control` and `arrival_screen`, `warmup` is a
   BURN-IN PREFIX: a duration string, the walk is built at `start - warmup` and
   the prefix is discarded so path-dependent state is stationary before the
   measured window opens. It is a field on `GeneratedBinding`, a component of
   the fit's cache key, and a `binding.generated.warmup` key in a committed
   artifact. Meanwhile `tick_composition_ratios` carries `warmup_baseline` and
   `warmup_materialization_ticks`, which ARE the glossary sense - the ceiling
   on materializing a river's warmup. One crate, one word, two unrelated
   quantities, one of them in a cache key and one of them sizing a server
   constant. Nothing anywhere states that they are different.

3. **`divergence` MEANS FLOAT DISAGREEMENT here, roughly ten durable sites, in
   the same two crates where it also means armed havoc.** `measure12a`'s module
   doc ("the cross-language float-divergence defect class"), `kernel.rs` ("so a
   future divergence stays visible"), `stream.rs`, `aggregate/monthly.rs`,
   `select_windows.rs`, `cadence_feasible.rs`. Ten metres away in the same
   crates, `dynamics.rs`, `consts.rs`, `regime.rs`, `bars.rs` and
   `TickFault::Injected` use the glossary sense. The glossary defines
   Divergence as one armed havoc injection and the classification test for it;
   a reader of `measure12a` who applies that definition gets nonsense.

4. **[P2 CHANGED] The glossary's Divergence entry says generator arms are part
   of river identity. The implementation deliberately mutates a canonical
   boatless river instead, and `CheckpointIndex`'s pinned-snapshot machinery
   exists because it does.** `GeneratedSource::arm_flow_surge` writes
   a `SurgeWindow` into a live source; `CheckpointIndex::arm_flow_surge` arms
   the lead and then takes a PINNED control-boundary snapshot, whose whole
   documented purpose is that "a snapshot taken BEFORE an arm replays the span
   after it unsurged, which is precisely the realization fork the
   surge-on-the-canonical-tape change exists to remove." The generating side
   does not merely fail to fork - it has a named mechanism, a snapshot pin
   class, a coarsen exemption and a walk-back floor, all built to make the
   non-forking model correct. The adjacent glossary claim that nobody already
   reading has water mutated is separately true: the HTTP route refuses the arm
   when the river has a seated boat. **[extends P3 headline 4, and settles it: P3
   found the fork missing in `mogwai-venue`; the fork is not missing, it was
   deliberately removed, and the code that removed it is here.]** The glossary
   is what is wrong.

5. **[P2 REFUTED] `TAPE_PROTOCOL_VERSION` governs two generation origins, but
   the glossary is not obliged to call both venue rivers.** Version 20's own
   doc does call `segment::SegmentSource` "a second tape ORIGIN beside
   `GeneratedSource`", so the constant genuinely covers both. The first pass
   then crossed a boundary the code does not cross: the only non-test user of
   `SegmentSource` is `mogwai-cli/src/segments.rs`, where `mogwai segments tape`
   writes a bounded CSV dump. `mogwai-venue::source` constructs only
   `GeneratedSource`, and its `River` stores a `CheckpointIndex`. Therefore
   `SegmentSource` is an offline generation origin, not a second kind of served
   river. The real vocabulary defect is narrower: `TAPE_PROTOCOL_VERSION` is
   generation-process identity while the glossary defines Tape as a paced
   delivery stream. The constant needs a generation identity entry or a less
   overloaded name; River and Warmup do not need to describe an offline
   composer.

6. **`mogwai_lab::ledger` is not a ledger.** The glossary defines Ledger as one
   `mogwai-engine` instance owned by one account. `mogwai-lab`'s `ledger`
   module is the Databento DELIVERY MANIFEST gate - `LEDGER_KEY`,
   `verify_input(directory, ledger_path)`, `input_entry_job_id`. Both meanings
   are reachable from a crate that depends on `mogwai-venue`.

---

## Structural - glossary claims about the water, checked where it is made

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| River ("the generated market-data sequence for one resolved instrument shape") | `generated/checkpoint.rs` `CheckpointIndex` doc: "The index is shared per realization (one symbol's clean tape)"; "The realization is preserved byte-for-byte" | doc prose, type | REALIZATION is this crate's name for exactly what the glossary calls a river | cross | 2 | The two halves of the venue have two nouns for one object and neither document names the other. Either River reaches down here (my preference - the server already keys on `RiverKey` and the generator is what a river IS), or the glossary's River entry says "implemented as a `GeneratedSource` realization" so a reader can cross the seam. Today a reader tracing "what is a river made of" hits a wall at the crate boundary. |
| River / Divergence ("generator arms... are part of river identity... Nothing mutates water someone is already reading") | `generated/source.rs` `arm_flow_surge` / `clear_flow_surge`; `TickSource::arm_flow_surge` default; server seated-boat guard | trait method, impls, control route | writes a `SurgeWindow` into a live but boatless canonical source; the trait's own doc says "Arm a simulated-time flow surge. Non-generated sources ignore it." | cross | structural | **[P2 CHANGED, extends P3, and refutes the glossary's fork model rather than the code]** The mutate-in-place model is baked into the seam, and the server guard prevents it from reaching a source with current readers. Thus one half of the glossary sentence is wrong and one half holds: generator havoc does not create an identity-keyed river, but it also does not mutate water under a seated boat. Rewrite Divergence and Boarding around the actual pre-reader canonical mutation. |
| Divergence (same) | `generated/checkpoint.rs` `Snapshot { pinned }`, `checkpoint_control_boundary`, `coarsen`'s pin exemption, `try_source_before_target`'s FENCE/floor flag | type, field, methods, doc prose | the whole apparatus that makes an in-place surge answerable from history | cross | structural | This is the positive evidence for the row above and the best-reasoned code in the crate. A control boundary is a first-class concept - a snapshot that may not be coarsened away, a floor a walk-back may not retreat past, and a caller-visible boolean meaning "you are fenced, read the consumed print off the snapshot itself." None of it has a durable home, and it is the mechanism the corrected Divergence entry must cite. |
| Divergence / `FaultTape` | `lib.rs` `TickFault::Injected` and its doc | variant, doc prose | an operator-asked-for terminal fault, existing because every real `ArrivalRefusal` route was bounded at admission | cross, wire | - | **No defect, recorded as the model case.** The variant doc states the glossary's Divergence sense precisely, names why the arm exists, and says why it carries no detail. It is also the only place in either crate where "divergence" unambiguously means the glossary's word. |
| Tape ("what a boat publishes - the paced frame stream") | `TAPE_PROTOCOL_VERSION` doc, ~60 lines of version history | constant, doc prose | the IDENTITY OF THE GENERATION PROCESS - "not of any one path" | cross | 1 | The glossary's Tape is a delivery object (paced, per boat, broadcast). This constant names the PROCESS that manufactures the sequence a tape delivers, which is a third thing beside River (the sequence) and Tape (the delivery). Three concepts, two words. Name the process - "tape generation", "the generator's identity" - and stop letting one word carry the sequence, the delivery and the recipe. |
| Tape (same) | `segment.rs` `SegmentSource` doc: "An endless tape composed from a segment library" | type doc | the composed SEQUENCE, not a paced delivery | cross | 1 | Second instance of the same slip and this one is the crate's own summary sentence for a public type. It means river, not tape. |
| Tape / River (one origin assumed) | `segment.rs` `SegmentSource`, `SegmentCompose`, `SegmentLibrary`, `SEGMENT_LIBRARY_VERSION`; sole caller `mogwai-cli/src/segments.rs` | types, constants, call sites | an offline CSV composer from real cut session slices, integrated in returns space and looped for a caller-selected tick count | cross, artifact | structural | **[P2 REFUTED]** This is not a river. The server never constructs `SegmentSource`; only `mogwai segments tape` does, and that command bounds the loop with `--ticks`. The glossary describes the running venue, so its River and Warmup entries correctly omit this offline toolbox. Keep the row because it catches the first pass's unsupported inference. What remains is the Tape word collision and the undocumented breadth of `TAPE_PROTOCOL_VERSION`, not a missing composed-river design. |
| Warmup ("the uniformly servable simulated history from `data_origin_ns` through `run_start_ns`") | `fit/walk.rs` `parse_duration`/`python_cache_key(..., warmup, ...)`, `run_walk`'s "build the generator at `start - warmup`"; `arrival_control.rs` `GeneratedBinding.warmup`; `arrival_screen.rs` `warmup_ns` | fields, parameters, cache key, artifact key | A BURN-IN PREFIX discarded before the measured window opens | cross, artifact | 1 | Headline 2. Load-bearing in three ways at once: it is a refusal ("the warmup underflows the start"), a component of a content-addressed cache key, and a key in a committed binding artifact. Rename to `burn_in` on this side and leave Warmup to the glossary; the artifact key change is a version bump on an offline artifact, which is cheap. |
| Warmup (same) | `tick_composition_ratios.rs` `warmup_baseline`, `warmup_materialization_ticks`, `ticks_per_warmup`, `warmup_window_count` | fields, artifact keys | the GLOSSARY sense - how many ticks materializing a river's warmup costs, and the constant that bounds it | cross, artifact | - | The correct use, in the same crate as the incorrect one, four modules away. Recorded so the rename above does not sweep these up by grep. |
| Boat ("carrying its own `SimClock`... a boat is an implementation cache with no semantics of its own: the tape is deterministic and exogenous") | `generated/source.rs` `GeneratedSource`'s `Clone` doc: "the generator is a path-dependent walk whose entire future is a pure function of its current state" | type doc | the determinism the Boat entry's no-semantics claim rests on | cross | - | **No defect, and this is the sentence that MAKES the glossary's Boat claim true.** The claim lives in `reference/glossary.md` and its proof lives in a `#[derive(Clone)]` comment in another crate. The Boat entry should cite the property by name. |
| Boat (same, "nothing a consumer can measure reveals whether it shares a hull") | `generated/source.rs` `arm_flow_surge`; `checkpoint.rs` pinning; `mogwai-venue/src/http.rs` seated-boat refusal | methods, control route | an accepted surge mutates a boatless materialized river; the route refuses when any boat on that river is seated | cross | structural | **[P2 CHANGED]** The first pass's passenger-sharing scenario is unreachable. The control route says `"river {symbol} has a seated boat"` and refuses before arming, so nobody already reading observes the mutation. This does not rescue the glossary's identity model: the accepted path mutates an existing canonical river and pins its boundary instead of resolving a new river key at boarding. Correct Divergence and Boarding against that boatless, pre-reader mutation model; do not claim current passengers contaminate one another. |
| Session calendar ("the weekly open windows in exchange-local time. A scheduled close is configuration and the market is genuinely shut inside it, as distinct from `ReopenGap`") | `generated/calendar.rs` `SessionCalendar`, `WeeklyWindow`, `is_open`, `next_open_ns`, `settlement_instants` | type, methods | exactly the entry, plus `settlement_minute_of_day` | cross, artifact | - | **No defect; the model case for the whole pass.** The entry, the type and the validator agree, including the `ReopenGap` distinction, which `source.rs::begin_event` enforces by re-applying the calendar AFTER an unscheduled halt ("The calendar remains authoritative over whether a tick may print"). Recorded because it is the only water-side entry this pass can confirm outright. |
| Session calendar (its neighbour) | `generated/fingerprint.rs` `SessionProfile`, `session.rs` `SessionModulator`, `validate_for(calendar_owns_closure)` | types, method | relative arrival and volatility WHILE OPEN, normalized over the calendar's open minutes | cross, artifact | 3 | The ownership split - "the calendar owns whether an event may exist, and this owns how intense it is given that it may" - is a genuine two-party contract stated in a private field comment in `session.rs` and restated in `session_profile.rs`'s module doc. The glossary defines one party and not the other, so nothing durable says what a profile may not do. |
| Contract size grid ("how the generator turns a notional target into a printable size, derived from the instrument's own sizing") | `generated/source.rs` `SizeGrid`, `SizeGrid::from_def`, `SizeGrid::spot` | type, constructors | exactly the entry, including the perpetual correction the doc records | cross | - | **No defect.** The entry and `from_def`'s doc agree down to the reasoning ("a crypto perp sizes fractionally"). The entry says "spot is a 1e-8 grid; a future is whole contracts floored at one" and the code derives from `size_increment`/`size_precision` rather than from the class, which the doc explains; the entry should say derived-from-sizing, not per-class, or it will read as stale on the next look. |
| Multiplier / Tick value | `SizeGrid.multiplier` = `def.class.multiplier()` | field | currency units per 1.0 of price, used here as a SIZE grid multiplier | cross | 1 | The glossary's Multiplier is a notional conversion and `SizeGrid` stores it beside `integral` and `min_size` as if it were a size-grid parameter. It is carried but never used to snap a size (`materialize_size` reads only `integral` and `min_size`). Either it is dead weight in this struct or its role needs stating; today it invites a reader to believe sizes are multiplied by it. |

---

## Direction 1 - a glossary term doing a job that is not its own

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Divergence | `measure12a/mod.rs` module doc "the cross-language float-divergence defect class"; `measure12a/mod.rs` "A divergence of that class (3.55e-15 relative)"; `kernel.rs` "so a future divergence stays visible", "whether the divergence is a lone last-ulp", "the gate reports divergences that exist only"; `stream.rs` "The divergence is safe by construction"; `aggregate/monthly.rs` "A key-set divergence refuses"; `select_windows.rs`; `cadence_feasible.rs` "divergence survived here once already" | doc prose, comments | TWO NUMBERS DISAGREEING | cross | 1 | Headline 3. Ten durable sites, one crate away from the arm sense and sometimes in the same file tree. "Disagreement", "drift" or "mismatch" says it and none of them is taken. This is the largest single-word collision found in five passes after `seat`. |
| Ledger | `mogwai-lab/src/ledger.rs`, `verify_input(directory, ledger_path)`, `input_entry_job_id(ledger_path, ledger_key)`, `subcontract::LEDGER_KEY = "mnqv\|2026-07.full\|tbbo"` | module, functions, constant, artifact key | the DATABENTO DELIVERY MANIFEST (`analysis/databento-jobs.json`) and the key of one delivery in it | cross, artifact | 1 | Headline 6. `LEDGER_KEY` is also a frozen subcontract key, so the rename costs a subcontract hash re-bless - which is exactly the sanctioned way to move a frozen constant. `delivery`/`DELIVERY_KEY`, or `manifest`. Note the module ALSO carries the git tree gate, which is a third unrelated job under one file name. |
| Venue | `trigger.rs` module doc "used by the venue's trigger-price fill model"; `VOL_WINDOW_NS` doc "a venue whose fill model changes shape per deployment"; `MIN_VOL_SAMPLES` doc "the most permissive fill regime the venue has" | doc prose | mogwai itself - the glossary sense | cross | - | Correct usage. Recorded because the next three rows are the same word meaning a real exchange. |
| Venue | `generated/source.rs` `SizeGrid::from_def` doc "the most common perpetual on the largest venue"; `consts.rs` `EVENT_PRICE_REPEAT_PROB` doc "A venue whose top of book does not move"; `generated/numeric.rs` "Snap a size draw to the venue's round-lot grid"; `segments.rs` "while the venue is shut", "any level this venue serves" | doc prose | A REAL EXCHANGE (Binance, CME), or the instrument's own exchange | cross | 1 | The glossary's Venue is one running instance of mogwai. In the crates that model real markets the word slides to mean the real thing being modelled, sometimes in the same paragraph as the mogwai sense. "Exchange" for the real one; keep Venue for mogwai. `segments.rs`'s "while the venue is shut" means CME. |
| Session | `mogwai-lab/src/session.rs` `SessionSegment`, `assign_session`, `segment_origin_ns`, `SESSION_OPEN_LOCAL_MIN`, "session"/"overnight"/"post_halt" | module, types, functions, constants | THE CME TRADE DATE'S OWN SESSION - 17:00 local open to 16:00 close, with a halt | cross, artifact | 1 | **[extends P1/P3/P4's session cluster]** A FOURTH bare "session". P4 counted three clocks under one bare word (the consumer identity, the instrument calendar window, `reset_minute_utc`). This is the fifth: a futures trade date. Unlike the others it is inherited exchange vocabulary and should be QUARANTINED rather than renamed - but the glossary must then say so, because "session" is currently the most overloaded word in the workspace and nothing tells a reader which senses are ours to fix. |
| Session | `mogwai-lab/src/segments.rs` `SessionWindow`, `ASIA`, `LONDON`, `NY_MORNING`, `NY_AFTERNOON` | type, constants | A NAMED SLICE of that trade date, by offset from the reopen | cross, artifact | 1 | Sixth sense, and this one is minted here rather than inherited: "the first nine hours of the CME session" is this project's own carve-up. `TradingWindow` or `DaypartWindow` costs nothing and removes a sense. |
| Session | `generated/session.rs` `SessionModulator`, `SessionProfile`, `arrival_mult`, `vol_mult` | module, types | the WEEKLY INTENSITY CURVE, hour-of-day times day-of-week | cross, artifact | 1 | Seventh sense. It is neither a calendar window, nor a trade date, nor a slice - it is a shape function over civil time. `IntensityProfile` / `IntensityModulator` would separate it from the calendar it is deliberately distinct from, and the distinction is the load-bearing one in `validate_for`. |
| Segment | `mogwai-lab/src/session.rs` `SessionSegment.segment: "overnight" \| "post_halt"` | field, values | the two OPEN PIECES of one trade date, split by the 15:15 halt | cross, artifact | 1 | First of three segments. |
| Segment | `mogwai-lab/src/segments.rs` `Segment`, `mogwai-data/src/segment.rs` `Segment`, `SegmentLibrary`, `SegmentSource` | types, module, artifact | ONE CUT SESSION SLICE stored in returns space | cross, artifact | 1 | Second sense, and it is the one that reaches a served tape. A `Segment` here is a whole session window's worth of trades; a `SessionSegment` there is a piece of a session. Two public types in one crate whose names invert each other's meaning. |
| Segment | `generated/arrival.rs` `next_segment_end`, "segment" in the baseline integral walk | fn, local reasoning | A PIECE OF THE INTEGRATION GRID - the span to the next cell, hour or calendar boundary | local | 1 | Third sense, private but load-bearing: the arrival kernel's whole budget traversal is expressed in these. "Cell span" or "step". |
| Consumer | `lib.rs` `TickSource::fault` doc: "Consumers must query this after a terminal `None`"; `generated/tests.rs` "A CONSUMER'S CORRECTNESS" | doc prose | THE CALLER OF THIS TRAIT - the server's boat, the lab's walk | cross | 1 | **[extends P4's Client row from the other side]** The glossary's Consumer is the program driving the venue (broadarrow). Here it means whoever holds a `&mut dyn TickSource`, which is always mogwai itself. "Caller" or "reader". Note P4 found `mogwai-engine` using the FORBIDDEN word `client` for the same kind of role; this crate avoids that and lands on the wrong glossary word instead. |
| Client | `bars.rs` `fold_trade` doc: "the adapter's live path runs trades through the client `HavocFilter`"; "a real client-side aggregator would also suffer"; test doc "whenever client `reorder_prob` is armed" | doc prose | the ADAPTER, i.e. the consumer's side of the wire | cross | 1 | **[extends P4]** The glossary grants `client` exactly two inherited exceptions and this is neither: `HavocFilter` is mogwai's own type in `mogwai-adapter`. "Adapter-side" is the accurate word and it is also more informative, because the whole point of the paragraph is that the reorder happens BEFORE aggregation on the adapter's side of the socket. |
| Run | `generated/source.rs` "a fresh stream prints a long Buyer run before the first flip"; `consts.rs` "flat runs"; `arrival.rs` "the walk runs" | doc prose | a consecutive sequence of like prints | cross | - | Not a finding - "run" as a statistical run is standard and unambiguous in context. Recorded so it is not swept into a rename of the glossary's Run. |
| Freeze / frozen | `generated/source.rs` `begin_event`'s `frozen_book`, `repeat_is_compatible(price_ticks, frozen_book, aggressor)`; `subcontract.rs` "the frozen measurement sub-contract"; `arrival.rs` "the frozen Stage A API" | parameter, doc prose | (a) the PREVIOUS event's retained book, (b) an artifact that may not change | cross | 1 | The glossary's Freeze is an account state. Two more senses here, neither related and neither related to each other. The book one is the worse of the two: `frozen_book` is a parameter name on a public-adjacent predicate and it means "the book from last time", which is "previous", not "frozen". |

---

## Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| River | `checkpoint.rs` "realization"; `source.rs` "the walk", "the stream"; `lib.rs` "a pure path-dependent walk" | doc prose | one river's water | cross | 2 | Headline 1. Four words - realization, walk, stream, tape - for the object the glossary calls a River, none of which is River. Pick one for the SEQUENCE and one for the PROCESS that emits it, and say which is a river. |
| Boat | `lib.rs` `TickSource::seek_to` doc: "the server's checkpointed seek does this"; `checkpoint.rs` `lead`, `frontier_ns` | doc prose, fields | the paced reader's positioning apparatus | cross | 2 | **[extends P3's Cursor row]** P3 found `mogwai-venue` calling a boat a "cursor". Here the same object is the LEAD and its position is the FRONTIER. Four names across two crates for one paced read: boat, cursor, lead, frontier. The `lead` name is good and local; the point is that nothing says a boat's read position and the index's lead are the same walk. |
| Warmup / data origin | `lib.rs` `TAPE_PROTOCOL_VERSION` doc "tape anchor"; `source.rs` `start_ts` "the tape anchor RegimeState needs"; `checkpoint.rs` "`origin` must be a fresh source at the tape origin" | doc prose, parameters | `data_origin_ns` - where the walk begins | cross | 2 | **[extends P2/P3]** P2 asked for `TAPE_ORIGIN_NS` to be renamed to the wire spelling; P3 found the server wrapping it in a method that exists only to translate. This crate adds a THIRD spelling, "tape anchor", used interchangeably with "tape origin" in adjacent paragraphs of the same doc comment. One name. |
| Warmup / materialization | `checkpoint.rs` `extend_toward`, `max_extend`, "the runaway backstop"; `try_source_at_or_before`'s refusal | method, field, doc prose | how far the shared lead may be walked in one call, and the refusal when a target is past it | cross | 3/2 | The glossary's Tape entry says a non-boot river "is synthesized then - so the first requester pays that river's warmup latency inside its own request". This is the mechanism and the BOUND on it, and the bound produces a refusal a requester can receive. "Reach" is the server's word (P3 found `MaterializeRefusal::Reach`), "extend" is this crate's. Same quantity, two crates, two words, and only one of them appears in the glossary's account of who pays. |
| Divergence / FlowSurge | `generated/arrival.rs` `RuntimeModifiers { rate_mult, children_mult, pending_reopen }`, `RuntimeModifiers::NEUTRAL` | type, fields, constant | the resolved effect of every armed generator divergence, as seen by the arrival kernel | cross | 2 | Good name, undefined anywhere durable, and it is the ONLY place in the workspace where the generator-arm set is expressed as a single value with a documented neutral element. A corrected Divergence entry should cite `RuntimeModifiers` as what "a generator arm" resolves to. |
| Session calendar / ReopenGap | `generated/regime.rs` `Reopen`, `take_reopen_crossed`, `pending_reopen`, `reopen_frontier_ns` | type, methods, field | the unscheduled halt-and-gap, and the frontier that guarantees it is tested over contiguous spans | cross | 3/2 | The glossary distinguishes a scheduled close from `ReopenGap` in one clause. The whole crossing mechanism - contiguous tested spans, fail-closed on an already-elapsed arm, the arrival kernel and the regime having to AGREE about a crossing (`expect("arrival kernel and regime disagree about reopen crossing")`) - is undefined. `reopen_frontier_ns`'s field comment is a textbook statement of `AGENTS.md`'s frontier family and lives nowhere a reader of that rule would find it. |

---

## Direction 3 - load-bearing and undefined

The clusters matter more than the rows. This surface's undefined vocabulary is
larger than any prior pass's because the glossary describes the water only from
the venue's side.

### The generation cluster - parent, child, sweep, burst, event, print, level

This is the vocabulary of what a tape IS, and the glossary contains none of it.
A reader of `/trades` sees prints; a reader of this crate sees parents with
children; nothing connects them.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `source.rs` `ParentSummary { parent_ts_ns, child_count, child_stride_ns }`, `advance_parent`, `begin_event`, `step_child`, `next_child` | type, methods | ONE PARENT EVENT and its fixed-stride run of child prints - the unit of everything the generator does | cross | 3 | The single most load-bearing undefined vocabulary on this surface. A parent is one market event; its children are the individual prints of the sweep it caused; `INTRA_EVENT_STEP_NS` (1 us) is the stride between them, and that stride is what makes the trade stream strictly increasing, which `a_river_never_prints_two_trades_at_one_instant` pins and a timestamp-only history cursor DEPENDS ON. That dependency is a consumer-visible contract stated in a private function's doc comment. |
| - | `dynamics.rs` `SweepShape { q, m, level_step_prob }`, `SweepBurst`, `next_count`, `next_count_scaled`, `CHILD_CAP`, `truncated` | types, methods, constant | the child-count mixture and the in-flight burst | cross | 3 | `SweepShape` is PUBLIC (`pub use generated::SweepShape`) and its doc explains a closed-form mixture solve, a degeneracy condition, a clamp on a scaled mean and a truncation counter the realism gate reads - all excellent, all invisible from any durable document. A caller outside the crate can construct one and has no document telling it what `q` and `m` mean. |
| - | `source.rs` "print", "the print series", `last_trade_price`, `last_event_price_ticks` | doc prose, methods | one emitted trade | cross | 3 | "Print" is used throughout both crates and in `mogwai-engine` (P4's band cluster) as the word for an emitted trade, and it is defined nowhere. It is good, standard and worth adopting explicitly - the glossary's water section never names the atom the tape is made of. |
| - | `consts.rs` `HIGH_REGIME_LEVEL_STEP_MULT`, `levels_mean`, `level_step_prob`, `lvl_bin` (lab) | constants, fields | how far up the price grid a sweep walks | cross, artifact | 3 | "Level" here is a PRICE GRID STEP inside a sweep. In `mogwai-lab/src/fingerprint.rs` `level_queue` / `level_verdict` it is a corpus statistic. In `mogwai-engine` it is a book level. Three senses, and this one is the one the tape's shape depends on. |
| - | `dynamics.rs` `BounceState`, `next_side`, `next_drift`, "the high/low bounce regime", `drift_ticks`, `drift_hot`, `DRIFT_RECENTER_FRAC` | type, methods, constants | the alternating-side print process and its same-direction excursion | cross | 3 | An entire price-formation model - two regimes, a flip probability per regime, a drift that accumulates within an event and is RE-CENTRED at every parent boundary - with no durable description. `drift_ticks`'s field doc is the only account of the re-centring, and it is the reason the event-layer return ACF is negative, which is a fingerprint target. |
| - | `consts.rs` `TRADE_BOUNCE_HALF_WIDTH_TICKS` and its "This is NOT a spread" argument | constant, doc prose | the displacement of a print from the drifted latent mid, on the aggressor's side | cross | 3 | The best-argued constant in the workspace and its argument is now partly FALSE - see lateral 2. The distinction it draws (quoted width vs effective spread vs print separation) is real, load-bearing and belongs in `reference/`, not in a comment whose supporting claim has expired. |
| - | `source.rs` `next_latent_mid`, `GarchVol.mid`, "the latent mid", `MID_CEILING`, "the walking mid" | methods, fields, constant | the unobservable price the book is placed around | cross | 3 | LATENT is used for two different unobservables in one crate - the latent MID here and the latent INTENSITY `x` in `arrival.rs` (`latent_x`, `ARRIVAL_X_CEILING`) - plus `latent_size_median` in the config schema, a third. Three latents. Each is fine alone; together they make "the latent" ambiguous in every sentence that uses it bare. |
| - | `source.rs` `VolTrace` and its sixteen fields; `enable_vol_trace`, `take_vol_trace` | type, methods | one parent event's volatility intermediates, observed off the real path | cross | 3 | Public, `Serialize`, sixteen fields, zero durable documentation, and its own doc says it exists for "the 420.75-point minute's diagnosis". A consumer of this type has no way to know what `garch_scale` is NOT (`GarchStep`'s doc: "a SCALE parameter - it is NOT the conditional standard deviation"). That warning is on the private struct; the public one repeats the field without it. |
| - | `quote.rs` `PublishedBook`, `place_book`, `book_mid_ticks`, `QuotedWidth`, `TopOfBookSizes`, `TradeDisplacement` | types, functions | the observable top of book and the three seams that calibrate it | cross, artifact | 3 | A whole quote layer, public, config-deserialized, with a calibration-provenance discipline - and the glossary's water section does not mention that the venue publishes quotes at all. Note `QuotedWidth` and `TradeDisplacement` are deliberately independent observables ("requiring displacement to fit inside half the width would collapse those seams back into one"), which is a real modelling ruling with no home. |
| - | `quote.rs` `CalibrationProvenance { Uncalibrated, Fitted { corpus } }` | enum | whether a knob was fitted to a named corpus or is a placeholder | cross, artifact | 3 | A genuine data-quality contract on the config surface - a preset can declare which of its numbers are real - and `GeneratorScalars::validate` enforces a non-empty corpus for exactly three fields. "Provenance" also names two unrelated things in `mogwai-lab` (`storage::ProvenanceToken`, a cache key; `segments::LibraryProvenance`, a delivery record). Three provenances. |

### The arrival cluster - kernel, family, cell, latent, intensity, budget, refusal

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `arrival.rs` `ArrivalConfig` / `ArrivalKernel` / `ArrivalState` / `ArrivalEnv` / `CadenceWalk` / `CadenceParts` / `ParentDraw` | enums, structs | the protocol-12b integrated arrival mechanism, config to draw | cross, artifact | 3 | Seven public types for one mechanism, all undefined outside a `notes/` spec that nothing durable may cite. The config/kernel/state/env split is the good part - config is what an operator writes, kernel is the resolved parameters, state is what mutates, env is the immutable baseline - and it is a distinction nothing states. |
| - | `arrival.rs` `ArrivalRefusal { NoOpenExposure, IntensityCeiling, NonFiniteState }` | enum, variants | the three ways a parent draw gives up, each becoming a TERMINAL `TickFault` and thus a dead venue | cross, wire | 3 | **[extends P1/P4's refusal taxonomy]** These are the only non-injected routes to a terminal tape fault in the entire product. A consumer whose strategy dies mid-run because a `LogOuCox` latent went non-finite gets a fault whose vocabulary appears in no document. `NoOpenExposure` in particular is subtle: it means the kernel walked its whole 366-day limit without accumulating enough exposure, which is what a too-thin latent or an all-closed calendar produces. |
| - | `arrival.rs` `ARRIVAL_X_CEILING`, `MAX_LOG_OU_SIGMA_Y`, `fingerprint.rs` `MAX_MEAN_EVENT_DURATION_S` | constants | the three admission ceilings that bound per-draw cost | cross | 3 | All three docs are exemplary - measured tables, a named knee, an explicit statement that the bound is a cost policy rather than a boundary the measurement picks out. `ARRIVAL_X_CEILING`'s doc also states the two-implementations rule correctly ("a second copy of the literal is exactly the twin-value defect this workspace keeps finding"). This is the material a performance/limits reference should absorb; today it is three doc comments. |
| - | `arrival.rs` `cell_index`, `cell_start`, `CADENCE_STEP_NS`, `next_segment_end`, `baseline_integral`, "budget" | fns, constants | the piecewise-constant integration grid the kernel walks to place a parent | local | 3 | "Budget" here is an exponential random variable consumed by exposure - a time-change construction. `arrival_screen` and `stage_a_batch` use "budget" for WALL-CLOCK SECONDS a screening run may spend (`STAGE_A_BUDGET_S`), and `mogwai-venue` uses it for outbound BYTES (P3's admission cluster). Three budgets, all load-bearing, one word. |
| - | `arrival.rs` `ARRIVAL_KERNEL_VERSION` | constant | identity of the cadence draw, for Stage A cache entries | cross, artifact | 3 | A FOURTH identity constant beside `TAPE_PROTOCOL_VERSION`, `SEGMENT_LIBRARY_VERSION` and `mogwai-lab`'s five `*_VERSION` constants, with its own bump rule stated in its own doc. Nothing enumerates them or says which governs what. `AGENTS.md` binds one of them unconditionally and is silent on the rest. |
| - | `consts.rs` `ARRIVAL_MEAN_CAL` vs `cadence_base_mean_s`'s "The mean is BARE, deliberately" | constant, fn, doc prose | a calibration correction that applies to the shipped sampling scheme and MUST NOT apply to the integrated frame | cross | 3 | Two arrival paths with two different mean conventions, and the whole correctness of the 12b calibration amendment rests on one of them not inheriting the other's correction. It is stated once, in `cadence_base_mean_s`'s doc. This is the "two constants encoding one quantity" family inverted: one constant that must reach exactly one of two callers. |

### The fill-band cluster, generating half

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `trigger.rs` `VOL_WINDOW_NS`, `FILL_HORIZON_NS`, `MIN_VOL_SAMPLES`, `VolReading`, `vol_reading` | constants, type, fn | the trailing realized volatility that SIZES the venue's fill band | cross | 3 | **[extends P1's and P4's band cluster]** P4 called the band the largest undefined vocabulary on the engine's constructor path. This is where the number comes from, and it is a public API with three public constants that set "the estimator's identity". `horizon_return` is "the number the band formula multiplies" - i.e. one function in `mogwai-data` and one in `mogwai-engine` jointly define how permissive fills are, and neither is described anywhere durable. |
| - | `trigger.rs` `Walk { hits, reached_ns, drained }`, `TriggerScan`, `scan_triggers` | types, fn | the batched trigger walk and its PROVED frontier | cross | 3 | `reached_ns`'s contract - "a timestamp this walk has drained COMPLETELY", established only by an event with a LATER timestamp or by the source ending - is the cleanest statement of `AGENTS.md`'s frontier family anywhere in the workspace, and it includes an explicit note about the one branch where it is asserted rather than proved and what would invalidate that branch. It belongs in `reference/`. |
| - | `trigger.rs` the six-bound pre-filter and its `TriggerToward` / `TriggerTouch` comment | comment | which extreme each (kind, side) group's predicate opens toward | cross | 3 | The comment names a real trap: "`TriggerToward` - the TOUCHED-order family, not to be confused with `TriggerTouch`, which is the STOP family despite the name." Two `ScanKind` variants whose names mislead, documented at a call site in another crate. **[extends P1's tape-walk cluster]** - the fix is in `mogwai-protocol`, and this is the site that pays for it. |

### The composer cluster

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | `segment.rs` "seam", `at_seam`, `take_seam`, `seam_gap_ns` | field, method, config | the boundary between two composed segments, where the dead time and the gap return land | cross, artifact | 3 | "Seam" also means the engine/server division of labour (P4), the arrival mechanism's config attachment point (`GeneratorScalars.arrival` doc: "the protocol-12b arrival seam"), and the `TickSource` abstraction itself (`lib.rs`: "the `TickSource` seam"). Four seams. |
| - | `segment.rs` `integrate`, "returns space", "integration constant", `clamps` | fn, doc prose, field | composition as integration of log returns, with absolute level as a free constant | cross | 3 | An owner ruling (2026-08-12) stated in two module docs and in no durable document. It is the reason an endless single-session tape is expressible at all, and it is the premise the whole segment sampler rests on. |
| - | `segment.rs` `open_gap_ret`, `reopen_gaps`, "the feature injectors of the direction note" | field, config, doc prose | a measured real reopen gap, toggled on at the seam | cross, artifact | 3 | "Reopen gap" here is a MEASURED CORPUS QUANTITY landing at a composition seam. `MarketRegime::ReopenGap` is an ARMED HAVOC DIVERGENCE. Same two words, one crate apart, one of them in the glossary's Divergence list. A reader arming a `ReopenGap` and reading `reopen_gaps: true` will assume they are the same knob; they share no code. |
| - | `segment.rs` `clock_exhausted`, "the ONLY reason this source ever returns `None`" | field, method, doc prose | the composer's single terminal condition | cross | 3 | See lateral 3: this is a terminal condition that the `TickSource` trait's own fault channel cannot carry, so it is invisible to every consumer that does not know to call `SegmentSource::clock_exhausted` by name. |

### The lab's measurement vocabulary

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| - | crate root; `stream`, `preflight`, `ledger`, `characterize`, `segments` | modules, paths, artifact provenance | the delivered historical observations and their manifest, from which measurements and segment libraries are derived | cross, artifact | 3 | **[P2 ADDED]** Corpus is the root noun of the lab API and of `CalibrationProvenance::Fitted { corpus }`, but the glossary has no entry for it. Define it as observed offline input, not as a served-symbol admission requirement. That last clause matters because the project contract explicitly says intake improves a tape and never gates whether a symbol is served. |
| - | `fingerprint.rs` `build_fingerprint`; `mogwai_data::Fingerprint::from_repo_json`; `analysis/fingerprint.json` | function, public type, committed artifact | the committed synthesis input distilled from corpus measurements: fitted ranges, targets, cadence, and session profile | cross, artifact | 3 | **[P2 ADDED]** Fingerprint is load-bearing across both crates and governs generator configuration, yet it is undefined. It is neither one tape realization nor its identity. Define the artifact and its relation to corpus, preset, and generated river so the intake sequence has nouns at every handoff. |
| Cadence | `cadence.rs`; `arrival.rs` `CadenceWalk` and `CadenceParts`; fit target names; glossary Boat and Seat prose | module, public types, artifact keys, glossary prose | in the lab and generator, the stochastic timing and density of parent events; in the glossary, sometimes the boat's delivery speed | cross, artifact | 1/3 | **[P2 ADDED]** Two quantities share the word. Generator cadence changes event timestamps and therefore water. Boat speed changes pacing only and therefore does not. The glossary says "one ledger still carries one cadence" while defining speed as the seat restriction, which invites exactly this confusion. Reserve cadence for generated arrival timing and call the boat quantity delivery speed, or define both qualified forms. |
| - | `fit::driver::run_fit`, `fit::targets`, `fit::within`, `FitConfig`, `WalkCache` | module, functions, public type | the offline solve that walks generated summaries, compares them with observed targets, and emits fitted generator parameters | cross, artifact | 3 | **[P2 ADDED]** Fit is a whole public workflow rather than a generic verb here. Its inputs, output, cache identity, and relation to the fingerprint are load-bearing and absent from the glossary. Define Fit as the solve between measurement and fingerprint; do not let it be inferred from old phase notes. |
| - | `error.rs` `LabError::Refusal` / `LabError::Harness` and "Refusals are the interface" | enum, variants, doc prose | a fail-closed input-contract violation, as distinct from an environment failure | cross | 3 | The lab has a REFUSAL discipline of its own, distinct from the venue's refusals (P2's taxonomy) and from `ArrivalRefusal` and `ScreenRefusal` and `SegmentError::Refusal`. Five refusal vocabularies in one workspace. The concept is the same in all five - the system declining rather than failing - and it deserves one definition. |
| - | `storage.rs` ARTIFACT / CACHE / SCRATCH, `ProvenanceToken`, `CacheStore::clean_stale`, `ScratchDir` | module, types | the three-class file policy, never mixed | cross | 3 | A genuine operator-facing contract (where the lab writes, what it deletes, what `MOGWAI_CACHE_DIR` does) documented only in a module doc. `docs/cli.md` is where a user would look. "Stale provenance" is a good, precise coinage: an entry unreachable by construction because it does not name the current token. |
| - | `sidecar.rs` MARKER / COUNTER / KV, `report`, "OBSERVATION ONLY, structurally" | module, functions, doc prose | the three benchmarking channels and why they are not interchangeable | cross | 3 | `reference/performance.md` is named as the durable home in `AGENTS.md` and this module's own doc is better than what a reader would find there. The structural claim - "it has no return value a caller could branch on", so a measurement channel cannot change what is measured - is a design invariant nothing enforces beyond the signatures. |
| - | `arrival_screen.rs` `Family`, `Cell`, `LatticeCell`, `EvaluatedCell`, `CellVerdict`, `ScheduledCell`, `PanelCell`, `PilotCell`, `refinement_round`, `admissible_regions` | types, fns | the parameter-space search: a family, a point in its lattice, and the verdict at that point | cross, artifact | 3 | EIGHT cell types across two modules. `Cell` is a parameter point here; in `aggregate` a "cell" is a MEASUREMENT CELL (an hour, a bin) whose votes are resampled. Two unrelated cell vocabularies in one crate, both central. Also note `ScreenRefusal` is a ninth refusal shape. |
| - | `arrival_screen.rs` / `stage_a_batch.rs` STAGE A, STAGE B, BRICK K, BRICK N, "gates B1 to B7", "A1 to A4" | doc prose, constants | the protocol-12b programme's own phase and gate names | cross, artifact | 3 | These are `notes/`-defined identifiers embedded in durable code, artifact keys and public constant names (`STAGE_A_SEEDS`, `STAGE_B_BUDGET_S`, `gate_b6`). Nothing durable may cite `notes/`, so a reader of `gate_b6` has no legal path to what B6 IS. Either the gate definitions graduate to `reference/` or the functions carry their own statements. |
| - | `stage_a_batch.rs` `StratumId`, `PanelCell`, `ProbabilityRatio`, `SampleKind`, `RefinementCap`, `SelectionSeeds`, `MANIFEST_SCHEMA_VERSION` and four sibling version constants | types, constants | the sampling plan for the screen | cross, artifact | 3/inherited | Stratification and pilot sampling are inherited statistics (quarantined below); the MANIFEST and its five independent version constants are this project's. Five versions on one artifact, each with its own bump condition, is the twin-value family waiting to happen. |
| - | `subcontract.rs` "the frozen measurement sub-contract", `SUBCONTRACT_KEYS`, `subcontract_hash` | module, constants, fn | the set of measurement constants whose values are bound into a hash | cross, artifact | 3 | The mechanism `AGENTS.md` warns about by name ("`mogwai-lab`'s `subcontract` carries the final measurement window's length twice"). The word "subcontract" for "the frozen parameter set a measurement is bound to" is opaque and undefined; "measurement binding" or "frozen parameters" says it. |
| - | `preflight.rs` `PreflightArtifact`, `run_preflight`, `require_preflight` | types, fns | the fail-closed pass over a delivered corpus, before a byte is measured | cross, artifact | 3 | "Preflight" is a good word with no definition. It is also a GATE with a `require_` form, i.e. downstream commands refuse without it - an operator-visible ordering constraint that `docs/cli.md` owes. |
| - | `ledger.rs` `TreeOracle`, `TreeQuery`, `require_clean_tree`, `tree_readings_are_production` | trait, enum, fns | the git-cleanliness gate a measurement run binds its commit through | cross | 3 | A real reproducibility contract - a run binds a commit only after checking the tree was clean, and the ORDER of the two reads is the contract. Also the source of the `arrival_control_refuses_a_tree_that_changed_during_the_run` test `AGENTS.md` warns about. Undefined, and living in a module named for something else entirely. |

---

## Inherited - quarantine, do not rename

| term | site | source |
|---|---|---|
| GARCH, ARCH, persistence, unconditional variance, innovation, Student-t, degrees of freedom, kurtosis | `dynamics.rs`, `consts.rs` | econometrics |
| Weibull, LogNormal, ChiSquared, Normal, Exp, Gamma, Poisson, StandardNormal, inverse CDF, geometric mixture, Bernoulli | `source.rs`, `arrival.rs`, `dynamics.rs` | probability / `rand_distr` |
| MMPP, Cox process, log-OU, Ornstein-Uhlenbeck, self-exciting / Hawkes, shot noise, latent intensity, time change, occupancy, `tau_s` | `arrival.rs` `ArrivalConfig` families | point-process literature |
| autocorrelation / ACF, lag, dispersion, CV squared, RMS, p95 / p999, nearest-rank quantile, Wasserstein, bootstrap, replicate, stratum, pilot sample, leave-one-out, multiplicity vector | `characterize`, `aggregate`, `kernel`, `arrival_screen`, `stage_a_batch` | statistics |
| Roll estimator, effective spread, quoted width, aggressor, tick rule, uptick / downtick, top of book, BBO, TBBO, DBN aggressor alphabet B/A/N, trade date, session open / halt / settlement | `lib.rs`, `quote.rs`, `stream.rs`, `session.rs`, `segments.rs` | market microstructure / Databento / CME |
| OHLCV, bar, bar interval, window close | `bars.rs` | universal; the anchoring matches nautilus `get_bar_interval_ns` |
| `TradeTick`, `QuoteTick`, `AggressorSide`, `Symbol`, `InstrumentDef`, `MarketRegime`, `Hit`, `ScanKind`, `Side`, `Decimal`, `splitmix64` | throughout | `mogwai-protocol` / nautilus / passes 1-4's quarantine |
| ChaCha12, `SeedableRng`, Mersenne Twister / `mtrand`, Fisher-Yates, splitmix64, sha256, XXH128 | `source.rs`, `fit/mtrand.rs`, `kernel.rs` | published algorithms |
| CPython `repr`, `statistics.pvariance`, `json.dumps`, Neumaier compensation, `py_sum` / `py_fsum` | `exact.rs`, `kernel.rs`, `subcontract.rs` | CPython, retained as the record of how parity was proven |
| Kraken, Binance, Databento, CME, MNQ / MES / NQ, `XBTUSD` | throughout | real venues and instruments |

Note `warmup`, `divergence`, `ledger`, `session`, `segment` and `venue` are NOT
inherited in the senses flagged above - each is this project's own choice in at
least one of its collisions, which is why every one of them is fixable.

---

## Lateral findings

Ordered by how much I think they matter.

**1. `generated/arrival.rs`'s module doc is flatly false and it is the first
thing a reader of the arrival mechanism reads.** It says: "This module is
deliberately not connected to [`super::GeneratedSource`] yet: Brick K only
establishes and tests the cadence contract." `source.rs::begin_event` branches
on `self.arrival_kernel.is_some()` and calls `begin_integrated_event`, which
drives `ArrivalKernel::next_parent` and is the path
`TAPE_PROTOCOL_VERSION` 12 and 22 both moved. The module is connected, is
shipped, is `TAPE_PROTOCOL_VERSION`-bearing, and its own header says it is not.
This is the vacuous-gate prose shape at its purest - a durable statement
describing a wiring the code no longer has - and it would send a reader hunting
for a mechanism they are standing in.

**2. `TRADE_BOUNCE_HALF_WIDTH_TICKS`'s doc rests on a claim that has expired.**
The argument is excellent and the conclusion is probably still right, but the
supporting sentence reads: "The generator constructs no `QuoteTick` anywhere:
there is no bid, no ask and no top of book, and `mogwai-venue`'s `/quotes`
route returns empty by construction." `source.rs` constructs `QuoteTick` in
both `begin_event` and `begin_integrated_event`; `quote.rs` exists and models a
`PublishedBook` with a `QuotedWidth`; `GeneratorScalars` carries `quoted_width`
and `top_sizes` as config; and pass 2 inventoried `/quotes` as a live route.
The distinction the constant draws (print separation is not a quoted width) is
now MORE important, not less, because both quantities exist and are configured
separately - so the fix is to re-argue it against the world that exists rather
than to delete it.

**3. `SegmentSource`'s terminal condition is invisible through the trait it
implements.** Its own doc is emphatic: "It has exactly ONE terminal condition,
the nanosecond clock running out of range, so a `None` from this source is
never ordinary end-of-stream and a consumer that reports it as one is reporting
the wrong thing." It does not override `TickSource::fault`, so the default
`None` - documented in `lib.rs` as meaning "ordinary exhaustion" - is exactly
what every consumer receives. `MergeSource` would treat it as a clean end and
emit nothing further without latching. **[P2 CHANGED]** The current CLI caller
does inspect `clock_exhausted()` and reports the right refusal, and the server
never holds this type, so this is not presently a venue failure. It remains a
public trait-contract defect: any generic `TickSource` caller sees ordinary
exhaustion. `TickFault` needs a clock-exhaustion variant, or this source should
not implement a trait whose terminal vocabulary it cannot honor.

**4. [P2 REFUTED] `SegmentSource` is an effectively-infinite source with no seek
bound and no checkpoint index, but no current caller seeks it.** `lib.rs`'s
`seek_to` doc warns explicitly: "a source whose
`next_tick` never returns `None` (e.g. `GeneratedSource`) spins forever if
`start_ts` is unreachable... callers driving an effectively-infinite source must
wrap it with their own bound (the server's checkpointed seek does this)." The
composer is the second such source and `CheckpointIndex` cannot wrap it. The
first pass treated possible future serving as current behavior. The actual CLI
caller starts at `config.start_ns` and advances exactly `args.ticks`, so it is
bounded and never calls `seek_to`. Keep the API smell: the trait exposes an
unsafe default to this type, and serving it later would require a new
checkpoint abstraction. Do not report an existing unbounded seek.

**5. [P2 REFUTED] `SegmentSource` inherits no-op generator-divergence methods,
but it is not a served source.**
`arm_flow_surge` and `clear_flow_surge` fall through to the trait's no-op
defaults ("Non-generated sources ignore it"). No control-plane path holds a
`SegmentSource`, so no arm is accepted and dropped today. The public trait does
make the future trap cheap to create. If the composer ever becomes servable,
the serving integration must either implement generator arms or refuse them.

**6. `try_source_at_or_before` is named for a boundary its doc says it does not
use.** The method doc says "positioned at the latest checkpoint STRICTLY before
`target`", and `try_source_before_target`'s comment spends a paragraph
explaining why the partition must be `<` and not `<=` - a real defect that lost
one tick per ~K seeks. The public name says `at_or_before`. Its sibling
`source_at_or_before` inherits the name. A reader reasoning about boundary
behaviour from the name gets the exact wrong answer, and the wrong answer is
the bug that was fixed.

**7. Two `SEGMENT_LIBRARY_VERSION` constants encode one quantity, and the
anchor holds - narrowly.** `mogwai_data::segment` and `mogwai_lab::segments`
each declare `= 1`, with a comment saying "Must track". `AGENTS.md` names this
family. It is genuinely anchored: `analysis/segment_library_conformance.json`
carries `"version": 1` and the data side's `the_conformance_fixture_composes`
loads and validates it, so a drift on that side fails. Recorded as a
NON-finding with a caveat - the fixture's version field is what makes it safe,
and nothing states that, so a future fixture edit that dropped or bumped the
field would silently unanchor both constants at once.

**8. `mogwai_lab::summary::SessionSegment` and `mogwai_lab::session::
SessionSegment` are two public types with one name in one crate, and it is
deliberate.** `summary`'s doc explains at length why the second copy of the
BODY was deleted and why the two STRUCTS remain distinct. The reasoning is
sound. But two public types with the same name in one crate, distinguished only
by module path, is a real readability cost, and the four shared field names
make a wrong-import compile. Renaming the narrow one (`SegmentBounds`) keeps
every word of the reasoning and removes the collision.

**9. `mogwai-lab`'s `ledger` module carries two unrelated jobs.** The corpus
delivery-manifest gate (`verify_input`, `input_entry_job_id`) and the git tree
gate (`TreeOracle`, `require_clean_tree`, `fresh_tree_state`) share one file
and one module name, and only the first is what "ledger" names. Whatever the
first is renamed to, the second wants its own module.

**10. `GeneratorScalars::validate` enforces a non-empty `Fitted { corpus }` for
three fields and the enum permits it everywhere.** The check iterates
`quoted_width`, `top_sizes`, `trade_displacement_ticks` explicitly. Any future
field carrying a `CalibrationProvenance` is unvalidated by default, and nothing
detects the omission - the "runtime guard installed at some of the sites that
owed it" shape `AGENTS.md` names. A non-empty corpus is a property of the TYPE,
so it belongs in a constructor or a `Deserialize` impl, not in a caller's list.

**11. `KrakenCsvSource` does not implement `seek_to` or `fault`, and
`MergeSource::starting_at` calls `seek_to` on it.** Harmless - the default
drains one tick at a time and the file is finite - but it means a merge over
CSV sources pays O(rows skipped) per child at construction with no bound and no
progress reporting, over files documented as "multiple GB". The offline intake
is the only caller. Recorded as a cost question rather than a defect.

**12. `SizeGrid.multiplier` is written and never read.** `from_def` sets it
from `def.class.multiplier()` and `spot()` sets it to one; `materialize_size`,
`validate_size_grid` and every other consumer read only `integral` and
`min_size`. It is a public field on a public `Copy` type, so it is part of the
API surface, and its presence implies sizes are scaled by it. Either wire it or
drop it.

**13. `mogwai-data` has no logging dependency and uses `eprintln!` as its
diagnostic channel at three sites** (`KrakenCsvSource`'s two skip paths,
`RegimeState`'s dropped-`ReopenGap` notice), each with a comment saying "No
tracing dep in this crate, so stderr is the visible channel." The last of those
fires inside a SERVING path: a river whose `ReopenGap` was already elapsed
prints to the venue's stderr at river construction. That is an operator-visible
message with no level, no structure and no route into whatever the venue logs
to. Three sites is enough to be a convention, and a convention that bypasses
the venue's logging is worth a ruling.

---

## What I would do with this

Four moves, in order.

1. **Decide whether the water words reach the generator, and write the answer
   down.** This is the finding, not a rename: two crates make every byte the
   venue serves and share no noun with the six glossary entries that describe
   it. **[P2 CHANGED]** River should reach down to the served
   `GeneratedSource` realization, and the server already has `RiverKey`.
   `SegmentSource` stays outside that rename because it is an offline composer,
   not a venue river. Whatever is decided, the Tape entry
   needs a companion saying what makes the sequence, because
   `TAPE_PROTOCOL_VERSION` is the identity of a process the glossary has no
   word for.

2. **Correct the Divergence entry against the code, then spend the word once.**
   The fork model is not implemented and was deliberately replaced by the
   canonical-tape surge with pinned control boundaries; `CheckpointIndex`'s own
   docs are most of the corrected entry already. In the same pass, take
   "divergence" back from the ten float-disagreement sites - it is the cheapest
   large collision on this surface and none of the ten sites resists a
   substitution.

3. **Split `warmup`.** Two unrelated quantities under one word inside one
   crate, one of them in a content-addressed cache key and one of them sizing a
   shipped server constant. `burn_in` for the measurement prefix, Warmup for
   the glossary sense. The artifact-key change is an offline re-bless.

4. **Give the generation vocabulary a durable home, and put the frontier
   statement in it.** Parent, child, sweep, burst, print, level, latent mid,
   latent intensity, band, horizon return, seam, realization: this is what a
   tape is made of, it is all public or public-adjacent, and it is documented
   only in field comments. Passes 1 through 4 all converged on a
   `reference/wire-vocabulary.md`; this is its fourth counterpart and it should
   absorb `trigger.rs`'s `reached_ns` contract, which is the best statement of
   `AGENTS.md`'s frontier family anywhere in the tree and currently sits in a
   comment above a `for` loop.
