# Glossary scope 7 - the operator contract

Inventory of everything a human types or reads at the command line and in
`mogwai.toml`, held against `reference/glossary.md` as revised 2026-08-21
(Boarding is an entry, Client became Consumer, River / Divergence / Session /
Eviction clarified). Nothing was edited outside this file.

Surface covered: `mogwai-cli`'s subcommand tree, every clap flag and its help
text, `mogwai-venue`'s `Config` schema and its validation messages, the
committed `mogwai.toml`, the shipped preset TOMLs and their `[provenance]`
vocabulary, the account-policy names, `mogwai man`'s topic list, and
`docs/cli.md`, `docs/config.md`, `docs/presets.md`.

Revision marks in this second, independent pass are **[added]** and
**[changed]**. The independent reading was completed before the first-pass
report was consulted, specifically to resist anchoring on its counts and
verdicts.

Direction key: **1** a glossary term used for something that is not that
term's job; **2** a job the glossary already names, under a different word;
**3** load-bearing and undefined; **4** an entry the code has not caught up
with - the roadmap; **inherited** vocabulary this project does not own.

Reach on this surface is almost uniformly **externally visible**: a config key
and a subcommand are typed by a human who has no type to hover and no doc
comment in reach. Where a row says crate-local it is a doc comment a
maintainer reads, not something an operator types.

## Where this scope sits relative to scopes 1-6

It **extends** and does not contradict them:

- Scope 1's `reset_account_on_reclaim` proposal and its Freeze/"unattended"
  row are confirmed from the operator side, where the offending words are a
  CONFIG KEY and a committed comment rather than a wire field - which raises
  their cost, because renaming a config key is a migration and renaming a
  struct field is not.
- Scope 1's `HavocSpec.client` / `.server` rows were refused as out of scope
  there and scope 6 found them adapter-side. This scope establishes the third
  and decisive fact: those field names are typed by an operator into a JSON
  file that `mogwai gen --havoc <PATH>` reads and `docs/havoc.md` documents.
  They are operator vocabulary, not internal naming.
- Scope 6's finding that `note_account_label`'s "One venue is now one run is
  one LEDGER" is retired architecture stated as fact is confirmed at TWO more
  sites on this surface, one of them the committed `mogwai.toml` an operator
  copies.
- Scope 3's "claim" overload and scope 1's scan/sweep pair reappear here as
  `fill_sweep_interval_ms`, a config key.
- **[changed]** New to this scope, and the largest vocabulary collision:
  unqualified `session` overwhelmingly means the market's trading day on the
  operator surface, while the glossary spends Session on the socket identity.
  The first pass was wrong that the socket sense is absent: `docs/config.md`
  teaches `/ws?session=` explicitly, and `docs/cli.md` also calls live
  WebSockets sessions in its shutdown contract. No prior scope had both senses
  in view.

---

# Direction 4 - entries the code has not caught up with

The roadmap. Every row is a gap in the code, never a case against the entry.

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Venue, Run, Ledger, Account | `mogwai.toml`, `account_id` comment: "The account this run's ledger is reported under. Names it rather than selects it - **there is one ledger**" | doc prose in the file an operator copies | asserts the run has exactly one account and one ledger | externally visible | 4 | **The single most misleading sentence on the operator surface.** Venue says "one run, many rivers, many accounts"; Ledger says one engine per account, created on first sight of an id. The committed example config tells every new operator the opposite, in the comment attached to the key they are most likely to edit. The behaviour is already many-account; the file is stale. Rewrite: "the DEFAULT account - the one a socket naming none is served under." |
| Venue, Ledger | `crates/mogwai-venue/src/config.rs`, `Config::account_id` doc: "The account this run's **single ledger** is reported under. **One venue is one run is one ledger**, so this NAMES the account rather than selecting one - there is nothing to look up and nothing to refuse." | field doc (rendered into rustdoc; the source of the `mogwai.toml` comment) | same false premise | externally visible | 4 | Same defect at its origin, and the same sentence scope 6 found in `client/exec.rs`. Three copies of a retired architectural claim, in three crates. `docs/config.md` already documents the correct behaviour ("this selects a default rather than declaring the venue's one account"), so the doc and the code comment disagree about a fact an operator acts on. |
| Venue, Ledger, Account | `docs/cli.md`, `GET /account` paragraph: "returns the **venue-wide ledger**" | doc prose | collapses all accounts into one run-wide ledger | externally visible | 4 | **[added]** The endpoint takes `?account=` and the glossary says every account owns its own ledger. This phrase is the same retired single-ledger premise in a third operator-facing form. Say that the endpoint returns the named account's ledger, defaulting to the configured default account. |
| Boot symbol / boot river | `Config::symbol` (top-level `symbol` key) | config key | the symbol whose river is boarded before readiness AND the default for a request naming none | externally visible | 4 and 2 | The glossary has a name for this and the key does not use it. Worse, the key does two jobs the glossary distinguishes: it seeds the boot river, and it is the request default. `boot_symbol` is the honest name. This is a migration, so it wants to land with a deprecation alias rather than a silent break - but pre-1.0, the break is acceptable. |
| Boarding | `Config::symbol` doc: "The symbol whose river is **boarded** before readiness" | field doc | the run placing the boot boat | externally visible | 4 | The Boot symbol entry is explicit that "the run boards nothing, because boarding is a passenger's act and a run takes no seat". The field doc says the run boards. The entry is the end state; the doc owes the correction: "the symbol whose river is materialized and given a boat before readiness". |
| Freeze | `account_ttl_ms` doc and `docs/config.md`: "How long an **UNATTENDED** account survives" | config key doc, doc prose | the Freeze TTL | externally visible | 4 and 1 | Scope 1 found this on `ReadyRecord`; it is the same word at the operator's end, where `docs/config.md` then says "While unattended an account is FROZEN" one sentence later, giving the operator two words for one state in one paragraph. Use Freeze once. Consider `freeze_ttl_ms` for the key itself. |
| Seat, Connection | `docs/config.md`: "Two sockets on one account cannot ride two cadences of one river ... That seat belongs to the SOCKET" | doc prose | the per-connection seat refusal | externally visible | 4 | **[changed, refuted]** The first-pass reachability finding was wrong. `docs/cli.md` explicitly documents `GET /ws?...&speed=`, and a URL query parameter is something an operator can type even if the shipped adapter cannot emit it. The row is retained because the worked reading refutes it: the behaviour is reachable and conforms to Seat. The adapter gap belongs to its own scope, not this one. |
| River, Served symbol | `docs/presets.md`: "Requested labels are case-exact river identities. Preset names and `[symbols.*]` overlay keys are matched case-insensitively" | doc prose | two matching rules, one surface | externally visible | 4 | **[changed]** The behaviour is correct and load-bearing. This is not a glossary defect under the requested test: River defines the identity and need not enumerate the symbol parser's case rule. Keep the operator documentation; optionally add the case fact to Served symbol, but do not report its absence as a defective definition. |
| Divergence, Boarding | `[regime]` config key; `gen --regime` / `gen --havoc` | config key, flags | the run-wide market regime | externally visible | 4 | **[changed]** The first half stands and the parenthetical is refuted. The glossary is the end state: generator arms are divergences, their carrier decides nothing, and boarding resolves every water-mutating knob into river identity. The current run-wide `[regime]` cannot express a later passenger's resolved divergence or a per-symbol arm. The code and config schema owe that behaviour, likely through a per-symbol or boarding-time resolved divergence surface. It is not evidence against the entry. |
| Account policy | `[balances]` versus `[account_policies.<name>]` | config keys | opening funding, and risk rules | externally visible | 4 | The Account policy entry defines a policy as "**opening balance plus risk rules**". On this surface those are two unrelated tables: `[balances]` is run-wide opening funding for every account, `[account_policies]` carries rules only and no balance. Scope 1 found the same split in `risk::AccountPolicy`. The operator surface makes the gap concrete: there is no way to register a named policy that also states its opening equity, which is exactly what a funded-account programme IS. Either fold `balances` into the policy table or correct the entry - and the first reading is the operative one. |
| Account | `docs/config.md`: "A SHARED VENUE ... an id identifies a TRADER, and two clients presenting one id ARE one trader" | doc prose | the bearer-token property | externally visible | 4 | Conformant in substance, but it introduces **trader** as the thing an account id identifies, a word the glossary does not define and which collides with nautilus's `TraderId` (a different value; scope 6 found `MogwaiExecClientConfig::default()` hardcoding `MOGWAI-001` as a `TraderId`). Either glossary Trader or say "an account". |

---

# Direction 1 - a glossary term doing a job that is not its own

## The `session` collision - report this as a cluster

The glossary spends **Session** on the `/ws?session=` identity and gives the
market's trading day only the narrower two-word term **Session calendar**.
**[changed]** The first pass overstated the collision by saying the socket
sense was absent. It is not: the operator can type `/ws?session=`, and
`docs/config.md` devotes six lexical sites to that identity/eviction contract.
The collision is nevertheless lopsided and real.

Worked count, using case-insensitive lexical occurrences of `session`,
`sessions`, and hyphenated/underscored compounds, then classifying each in
context:

- `docs/cli.md`: 26 occurrences, 22 market-day and 4 live-WebSocket senses.
- `docs/config.md`: 11 occurrences, 5 market-day and 6 socket-identity senses.
- `docs/presets.md`: 7 occurrences, all market-day.
- the printed MNQ preset: 29 occurrences, all market-day, including the
  `[instrument.session]` table and three `session.*` provenance keys. The MES
  and BTCUSDT preset documents add none.

That is 63 market-day occurrences against 10 socket senses in the durable
operator documents and printed preset. Excluding the verbose printed preset,
the three documents alone contain 34 market-day occurrences, which explains
the first pass's "roughly 35" impression. The typed/help vocabulary adds
`session-profile`, `[instrument.session]` / `[symbols.*.session]`, and the
segment command's session-window help on the market-day side. The socket side
adds the literal query parameter `/ws?session=`. The correct claim is therefore
"about 34 in the three documents, 63 when printed preset prose is counted, and
the socket sense is present ten times", not "roughly 35 and absent".

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Session | `mogwai session-profile preflight` / `session-profile fit` | subcommand | the calendar-conditional fit of the trading day's intensity curve | externally visible | 1 | Means Session calendar, not Session. |
| Session | `[instrument.session]` / `[symbols.<SYM>.session]` with `intensity_hour`, `dow_weight`, `vol_hour` | config keys | hour and day-of-week weights WITHIN an open trading day | externally visible | 1 | Same. The neighbouring `[instrument.calendar]` is what the glossary calls the Session calendar, so on this surface `session` and `calendar` are two halves of one glossary term and neither is named for it. |
| Session | `segments cut --window`, the four window names, "session window", "session slice", "session library", "holiday half-session", `--min-ticks-fraction`'s "fraction of the month's MEDIAN session" | flag, help text, doc prose | one trading day, or a slice of one | externally visible | 1 | Same. |
| Session | `docs/cli.md`, `docs/config.md`, `docs/presets.md` passim: "the published CME Sunday-evening-through-Friday-evening session", "per-session Blocks 1-4", "the closed-form session refits", "session-counted form" | doc prose | the trading day | externally visible | 1 | Roughly thirty sites across the three operator documents. |
| Session | glossary **Session** entry; `docs/config.md`: "A CLIENT IS NOT A SOCKET, and `/ws?session=` is what says so"; literal `/ws?session=` | glossary entry, doc prose, query parameter | the self-asserted socket identity | externally visible | 1 | **[changed, refuted in part]** This is not an operator-absent sense: an operator can type the query parameter, and the config guide explains it across six occurrences. It is still the minority sense, and the guide usually qualifies it as "session id". |
| | | | | | | **[changed] Cluster verdict: rename the glossary's Session entry to Session id, not the operator surface off the trading sense.** `session` for a trading day is domain-standard futures vocabulary, evidenced here by calendar, half-session, session-hour, session refit, session window and per-session measurement uses. Renaming 34 document occurrences, 29 printed-preset occurrences, config tables, provenance keys and command names would replace a standard market term with an invented euphemism. `Session id` already appears naturally in `docs/config.md`, precisely disambiguates the identity, and leaves **Session calendar** as the schedule of market sessions. The wire may keep its short `session` query spelling; wire spelling does not require the glossary headword to be equally ambiguous. |

## The retired words, at the operator's end

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Venue | `server_heartbeat_ms` (config key), and its doc "Optional **server**-originated heartbeat cadence" | config key | the venue-originated liveness frame | externally visible | 1 | **The clear case under the standing ruling: a `server_*` config key naming the venue is a defect.** `venue_heartbeat_ms`. It is also the only `server_*` key in the schema, so the migration is one key. `docs/config.md` repeats the word ("sets the server-originated liveness cadence"). |
| Consumer | `reset_account_on_reconnect` doc: "Whether a returning **client** gets its ledger back"; `docs/config.md`: "decides whether a returning **client** gets its ledger back or a clean one" | config key doc, doc prose | the party that reconnects | externally visible | 1 | `client` is retired. Consumer for the party, Session for the identity that makes the return a continuation. Scope 1 additionally proposes `reset_account_on_reclaim` for the key, which this scope endorses: the venue tracks no prior connection, so "reconnect" describes a mechanism that does not exist. |
| Consumer | `Config::speed` doc: "stream as fast as the **client** drains"; `zero_speed_stall_ms` doc: "a healthy in-process **client**", "a dead **client**"; `warmup_ns` doc: "bounded what a **client** was allowed to ASK for"; `account_policies` doc: "Named risk policies a **client** can ask for" | config key docs | the connected party | externally visible | 1 | Five sites in one file. All are Consumer, or Connection where the subject is one socket's drain rate (`speed`, `zero_speed_stall_ms` are per-connection facts). |
| Consumer | `docs/config.md` passim: "A CLIENT IS NOT A SOCKET", "A client that wants its own size opens an account", "sockets presenting the SAME one are one client", "a client that reaches the wrong venue", "what a nautilus host configures"; `docs/cli.md`: "the venue ... runs on the same machine as its client", "compatibility with older clients", "a client takes the labels it wants from its own configuration", "for_addr is the lossy path ... the client cannot tell its venue" | doc prose | the counterparty | externally visible | 1 | Roughly twenty-five sites across the two documents. The `docs/config.md` account-id section is the densest: it is a well-written contract in a retired vocabulary, and the Session and Eviction entries now say the same things in the correct words. This is a rewrite of two sections, not a find-and-replace: several sentences are load-bearing about the SESSION rather than the consumer, and blind substitution would make them wrong in the other direction. |
| Venue | `mogwai gen --config` help: "through the **server's** REAL `Config::load`"; `--seed` help: "The running **server** draws or configures its own run seed"; `--havoc` help: "a file the **server** would reject is rejected here"; `gen.rs` module doc "no server, no sockets"; `main.rs` module doc "the clap dispatcher over the venue **server**" | flag help text | the venue | externally visible (`--help`) | 1 | Every one is `venue`. `main.rs`'s "the venue server" is the compound the ruling specifically retires. |
| Consumer / Venue | `mogwai gen --havoc` help: "the **client**, **conn**, and **server** surfaces do not affect an offline tape dump"; the stderr note "the client, conn, and server surfaces are ignored"; the error strings "invalid **client** havoc", "invalid **server** divergence" | flag help, stderr, error text | the three non-generator halves of a `HavocSpec` JSON file an operator writes | externally visible | 1 | **This is where the `HavocSpec` field names become operator vocabulary**, which scopes 1 and 6 could not establish from their side. An operator types `{"client": {...}, "server": [...]}` into a file. Under the ruling both words are retired: `client` here is the ADAPTER's own inbound filter (scope 6's `inbound_havoc`) and `server` is the venue's armed divergences. Proposed: `inbound`, `transport`, `venue`, `data` - and the rename lands in `mogwai-protocol`, the adapter, `gen`, and `docs/havoc.md` together, because they are one word each. |
| Tape | `mogwai segments tape`; `--start-price` help "The **tape's** price level anchor"; `docs/cli.md` "composes that library into an endless single-session **tape** and dumps it as CSV" | subcommand, flag help, doc prose | a CSV file of composed returns, written offline, that no boat ever publishes | externally visible | 1 | The Tape entry defines a tape as what a BOAT publishes to its passengers. A subcommand that writes a file is not that. This is the same borrowing scope 6 found in the adapter's `ensure_on_tape`, and here it is a SUBCOMMAND NAME - the most expensive kind of wrong word, because it is what the operator types. `segments compose` says what it does. |
| Tape | `gen`'s output, `docs/cli.md` "charts the index future", "a chart taken from it disagreed with the served tape it was supposed to illustrate" | doc prose | the offline generator's dump versus a served river | externally visible | 1 | The document itself distinguishes "the served tape" from what `gen` prints, which is the correct distinction and the reason the word should not be spent on the offline dump at all. Both `gen` and `segments tape` produce a RIVER's realization; only a boat publishes a tape. |
| Warmup | `gen --warmup` help: "Warm-up span generated BEFORE --start ... warm-up observations are discarded" | flag help | a burn-in span for an offline accumulator | externally visible | 1 | Warmup in the glossary is `data_origin_ns .. run_start_ns`, the servable history every river owes. `gen --warmup` is estimator burn-in, discarded rather than served - the opposite property. Scope 6 found the adapter borrowing the same word for a history request. Three jobs, one word. `--burn-in`. |
| River / Boat | `docs/cli.md`: "The first passenger at a given speed places that **cursor**; later passengers at the same speed share it; a different speed places a second **cursor** on the same water" | doc prose | a boat | externally visible | 1 | The glossary's word is Boat, and the same paragraph in `docs/config.md` correctly says "a second boat on the same river". `cursor` also has a live second job (`docs/config.md`: "an order on a symbol no cursor is reading is cancelled"; scope 6: the adapter's history `cursor`). Three senses. Say Boat. |
| Boarding | `docs/config.md`: "an order on a symbol no cursor is reading is cancelled rather than left resting"; "the RETURNING boat's clock" | doc prose | the boat the returning socket boarded | externally visible | 1 | The Seat entry states this rule correctly in the current vocabulary ("what its book holds off the river the returning socket joins is retired"). The config document predates it and reads as if boats returned. Rewrite against Seat and Boarding. |
| Divergence | `docs/config.md`: "`[regime]` selects the single run-wide market regime"; `gen --regime` help "Data-regime havoc" | config key, flag help | the current carrier and type name for generator divergences | externally visible | 1 | **[changed, refuted]** The first-pass verdict inverted the contract. Carrier is explicitly irrelevant under Divergence and Boarding, so arrival through a config table does not make a generator arm a different concept from one posted to the control plane. The code owes one resolved Divergence vocabulary and boarding-time behaviour. `regime` is current implementation vocabulary to retire or subordinate, not grounds to split the glossary term. |

## Overloaded operator words

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Preset | `mogwai presets`, `preset = "MNQ"`, `[symbols.<SYM>].preset` | subcommand, config key | an instrument bundle | externally visible | 1 | Correct and glossed by implication (Served symbol). Recorded as the anchor for the next row. |
| Preset | `policy_preset` (POST field), `[account_policies.<name>]` (config table), `docs/config.md` "a policy preset" | wire field, config key | a named risk-rule bundle | externally visible | 1 and 2 | **Two registries, three names.** The thing is registered under `[account_policies]`, requested as `policy_preset`, and called "a policy preset" in prose - while `Config::account_policies`' own doc says it is "THE SAME IDEA AS AN INSTRUMENT PRESET". Pick one: either `[policy_presets]` and `policy_preset`, or `[account_policies]` and `account_policy`. Today an operator registers under one word and requests under another. |
| Provenance | preset `[provenance]` maps, `kind = "fitted" / "derived" / "declared"`, `accepted_diagnostics`; `mogwai presets MNQ` output | config keys, command output | per-knob evidence standing | externally visible | 1 | The good one, and worth an entry - `docs/presets.md` correctly makes it the reason to read a preset before trusting it. |
| Provenance | `mogwai cache stats` output "provenance dirs:", `cache clean --stale --keep <TOKEN>`, "provenance token" | command output, flag | a cache directory keyed by the command that produced it | externally visible | 1 | **The same word for evidence standing and for a cache key**, both operator-facing, both printed by `mogwai` subcommands. The cache sense is a content-addressed key; call it that. `mogwai cache stats --entries` printing "entry: <token>" while the summary line says "provenance dirs" already shows the surface cannot decide. |
| Breach | `[symbols.X.margin].breach_action` = `refuse` / `liquidate`; account-policy `on_breach` = `lock_until_reset` / `terminate` | config keys | what happens when a threshold is crossed | externally visible | 1 | Two spellings and two disjoint value vocabularies for one idea, in one file. The glossary's Account policy entry names only the second. An operator who has learned `breach_action = "liquidate"` will write it under a risk rule and be refused. Unify the key name (`on_breach` reads better) and glossary the four actions as one set. |
| Sweep | `fill_sweep_interval_ms`; `docs/config.md` "how often the run re-checks its resting limits against the tape" | config key | the resting-order tape walk | externally visible | 1 and 3 | Scope 1 found `scan` and `sweep` used for one walk inside `mogwai-protocol`. The operator-typed name is `sweep`, which settles which word wins; `ScanKind` and friends should follow it, and the glossary owes the entry (the Fill band cluster below). Note the second, unrelated sweep on this surface: the account sweeper (`docs/config.md`: a frozen account "is not swept"). |
| Fanout | `fanout_depth` | config key | the per-boat broadcast ring depth in pre-serialized frames | externally visible | 1 and 3 | "Fanout" names a delivery topology; the knob sizes a RING. The glossary has Boat ("its own broadcast ring") and no entry for the ring itself. `ring_depth` or `boat_ring_frames` says what the operator is sizing, and the doc's own "Depth of each tape's bounded broadcast ring" already reaches for the right words. |

---

# Direction 2 - a job the glossary already names, under a different word

| term | site | kind | what it means there | reach | direction | verdict |
|---|---|---|---|---|---|---|
| Boot symbol | `mogwai.toml` comment: "The **boot symbol** selects a matching preset"; `docs/config.md`: "The top-level `symbol` names only the boot river" | doc prose | the glossary term, used correctly | externally visible | 2 | Recorded as conformant: the prose uses Boot symbol and Boot river properly. Only the KEY does not (direction-4 row above). |
| Warmup | `warmup_ns`, and its recorded rename from `backfill_horizon_ns` | config key | the glossary's Warmup span | externally visible | 2 | Fully conformant, including the migration note in both the doc comment and `docs/config.md`. The best-executed rename on this surface and the model for the ones proposed here: state the old key, state that the value is unchanged. |
| Run | `run_duration_ns`, `--duration`, "one foreground run" | config key, flag, doc prose | the glossary's Run | externally visible | 2 | Conformant. |
| Server mode | `mogwai serve` | subcommand | the venue on the command line | externally visible | 2 | Explicitly not a defect under the ruling. Recorded so a later sweep does not touch it. |
| Consumer / Session | `docs/config.md`: "A session id is the **client's own string**: sockets presenting the SAME one are one client and coexist" | doc prose | the Session entry's whole contract | externally visible | 2 | The rule is right, the nouns are retired, and it contains the same error scope 6 found in the adapter: it says sockets sharing a session are "one client", where the venue's actual discriminator is the session and the consumer is something the venue cannot perceive at all. Rewrite from the Session and Eviction entries rather than substituting words. |
| Freeze | `docs/config.md`: "While unattended an account is FROZEN: it is not swept, its positions do not mark, its funding does not accrue and its policy cannot liquidate it" | doc prose | the Freeze entry | externally visible | 2 | Conformant and better than the entry in one respect: it enumerates what does not happen. Worth folding INTO the Freeze entry. |
| Materialize | `docs/config.md` and `docs/presets.md`: "materializes a river", "every river materialized so far", "a run retains at most 256 materialized rivers" | doc prose | creating a river on first reference | externally visible | 2 | The Boarding entry's phrase is "a key naming none creates the river". Two words for one act, one of them the operator-facing one. `materialize` is the better word and the glossary should adopt it - the River entry can then say a river is materialized at boarding or at first history read. |
| Cadence | `docs/config.md`: "one ledger carries one cadence"; preset `mean_event_duration_s` "cadence knobs"; `synth cadence`, `cadence-feasible` | doc prose, subcommands | delivery speed in the first sense, ARRIVAL RATE in the second | externally visible | 2 and 1 | Two jobs. The Seat entry uses cadence for delivery speed; the preset and `synth` vocabulary uses it for the generator's inter-arrival rate, which is river identity, not delivery. Say "speed" for the first and keep cadence for the generator, or the reverse - but not both. |

---

# Direction 3 - load-bearing and undefined

Reported at cluster level first. These are whole vocabularies an operator must
learn from nothing, and unlike the code-side clusters in scopes 1 and 6 there
is no doc comment to fall back on.

| cluster | sites | reach | verdict |
|---|---|---|---|
| **The overlay / bundle / preset resolution cluster** | `[instrument]`, `[symbols.<SYM>]`, `preset`, `[instrument.override]`, "bundle", "overlay", "default overlay", "explicit choice", "dotted path", "restates inherited key", the four-level precedence rule stated three times in two documents | externally visible | The single hardest thing to learn on this surface and the glossary is silent on all of it. `docs/presets.md` and `docs/config.md` each state the precedence in different words ("a preset bundle, then default `[instrument]` knobs, then matching `[symbols.<SYM>]` knobs" versus "An explicit per-symbol `preset` beats a default `[instrument]` preset, which beats a preset matching the symbol, which beats the BTCUSDT default") and they are not obviously the same rule. Owed: **Bundle**, **Overlay**, **Override**, and ONE canonical precedence statement both documents cite instead of restating. The glossary's Served symbol entry gestures at it in one sentence and stops. |
| **The evidence-toolbox cluster** | `preflight`, `measure`, `fit`, `characterize`, `count-curve`, `stage-m` (with `preflight`, `month`, `backcheck`, `reverify-amendment2`, `schedule-equivalence`, `promote-july`, `exchangeability`, `power`, `summarize`, `tier2`), `minute-range-envelope`, `arrival-control`, `arrival-screen`, `arrival-envelope-diagnostic`, `select-windows`, `tick-composition`, `tick-composition-ratios`, `synth`, `cadence-feasible`; and in help text "protocol-12a section-10", "Brick B4", "Brick G", "Stage A", "Stage M", "Tier 1a", "protocol 12b brick N", "Amendment 2", "L0 structural-proceed" | externally visible (`--help`, `docs/cli.md`) | **Half of `mogwai --help` is protocol jargon whose referents are retired notes.** `AGENTS.md` records the phase documents as retired to git history, and `docs/cli.md` is durable prose citing them by number. An operator cannot resolve "Brick B4" or "Amendment 2" from anything in the tree. Two moves, and the first is not optional: **name each command by what it measures**, in its own help text, without a brick number; and give the toolbox one entry defining Corpus, Measurement, Fit, Fingerprint, Artifact and Binding - the sequence `AGENTS.md` already calls the intake sequence and the glossary does not carry. Structurally, this whole tree probably wants to live under one `mogwai lab <cmd>` parent rather than eighteen top-level subcommands beside `serve`, `gen` and `presets`: the operator running a venue and the operator fitting an instrument are different people, and today they read one flat `--help`. |
| **The artifact / binding / clean-tree cluster** | "artifact", "hash-bound", "binding.harness_tree_commit", "refuses a dirty tree", "re-attests", "atomic write", "unbinds the artifact", "pairing identifier", "verdict", "gate versus diagnostic" | externally visible | A real and well-designed contract - a command's output is bound to the exact tree that produced it - stated only in `docs/cli.md` prose, per command, in slightly different words each time. Owed: **Artifact**, **Binding**, **Verdict**, and the gate-versus-diagnostic distinction, which `docs/cli.md` makes explicitly ("The preflight is a GATE, not a summary"; "that simulation is a GATE, not a diagnostic"; "Gate B5 is EVIDENCE THIS COMMAND READS, never a check it runs") and which nothing defines. |
| **The fill-band cluster** | `fill_band_vol_mult`, `fill_band_max_ticks`, `fill_sweep_interval_ms`; "band", "band_ticks", "trigger price", "drawn band", "trailing realized volatility scaled to a 60-second horizon", "through-at-the-stated-price", "the degenerate case", "slipped fill" | externally visible | Three config keys an operator must set, resting on a model the glossary never states. Scope 1 found the same gap on the wire side ("band", "drawn band trigger", `RunSeeds.fill`). It is a `TAPE_PROTOCOL_VERSION` subject and the whole fill realism story. Owed: **Fill band**, **Trigger**, **Sweep**, and the Through/Touch pair scope 1 asked for. |
| **The admission / lane / budget cluster** | `admission_lane_frames`, `exec_held_budget_bytes`, `pending_command_acts`, `global_pending_command_acts`, `zero_speed_stall_ms`; "the HELD lane's budget", "priority lane", "admission-truth frames", "one COMMAND, not one payload", "act", "queued or executing", "`AdmissionRejected` carries `retryable`", "503 history request capacity exhausted", "the fail-fast bound" | externally visible | **Five config keys naming concepts defined nowhere**, and `lane`, `held`, `act` and `budget` are exactly the words scope 1 flagged as used by the glossary and defined by nothing. "Act" is the worst: `pending_command_acts` is a key an operator types, and the only explanation of what an act is lives in `Divergence::CommandLatency`'s `*_act_ms` doc comment in another crate. This is the cluster where the code-side and operator-side inventories agree most exactly; one entry set closes both. |
| **The history-capacity cluster** | `docs/config.md`'s four-slot paragraph: "four `/trades` or `/quotes` requests at a time per run", "a slot is held until the response has been WRITTEN", "a waiter holds no page", "128 requests in the building", "a boot storm", "resolve the request EMPTY", "a QUIET WINDOW" | externally visible | An excellent and consequential contract - a refused warmup reaching a strategy as a silent empty window - with no configuration key, no glossary entry, and no name. It also has NO OPERATOR HANDLE at all: the four-slot ceiling, the 30-second wait and the 128-request bound are hardcoded, in a document that tells the operator to "stagger its boots" instead. Either give them keys beside the other admission bounds or say plainly that they are fixed. |
| **The offline-corpus cluster** | `--corpus`, `--ledger`, `--ledger-key`, `--root`, `--dir`, `--data-dir`, `--tz-authority`, `--month`, `--window`, `--library`, `--min-ticks-fraction`, "delivered TBBO month", "corpus root", "the conventional `<symbol>v/<month>.<state>.tbbo` layout", "returns space", "segment", "seam", "open_gap_ret", "stub session" | externally visible | The segment/corpus vocabulary is coherent within itself and undefined outside it. Two words carry real weight and are pure jargon: **ledger** here means a JSON manifest of data-purchase jobs, which COLLIDES HEAD-ON with the glossary's Ledger (an account's engine), and **library** means a JSON file of return sequences. `--ledger` on `preflight` / `stage-m` and Ledger in the glossary are two unrelated things an operator meets in one tool. Rename the flag (`--jobs-manifest`) - the glossary term is load-bearing and older. |
| **The cache-storage cluster** | `mogwai cache stats` / `clean`, `--cache-dir`, `--stale`, `--keep <TOKEN>`, `MOGWAI_CACHE_DIR`, "the storage policy's CACHE class", "the storage policy's term: ARTIFACT", "provenance token", "sub-contract hash" | externally visible | `docs/cli.md` cites "the storage policy" three times as a named authority ("the ARTIFACT class", "the CACHE class", "the standing storage policy") and no document in `docs/` or `reference/` defines it. A durable document citing an authority that does not exist in the tree is the vacuous-reference shape. Either write the storage policy down in `reference/` or stop citing it as a proper noun. |

### Miscellaneous undefined nouns an operator types or reads

`speed` (defined operationally in `docs/config.md`, never glossed; and
`speed = 0.0` "is unpaced delivery, not a stopped clock" is a distinction with
no entry), `regime`, `provenance kind`, `diagnostic` versus `warning` versus
`refusal` (three refusal severities on the boot path with no stated ladder),
`funding-barred` (a real boot state, named once, in two documents, defined in
neither), `river cap` / "the 257th is refused loudly", `boot storm`,
`firehose`, `parent` and `child` (the arrival model's core nouns, appearing in
`--parents` and in preset knobs `children_mean` / `children_single_frac`, with
no entry anywhere - an operator sizing `tick-composition --parents` is typing
a word from an unstated model), `latent size` versus observed size (the preset
doc is careful about the difference and nothing defines either), `modal_tick`,
`vol_scalar`, `quoted_width`, `top_sizes`, `trade_displacement_ticks` - the
last five being preset knobs an operator may override and which only the
provenance map explains.

---

# Inherited - quarantine, do not rename

- `oms_type`, `netting`, `hedging`, and the `oms-types` man topic - nautilus.
- `asset_class` values `fx` / `equity` / `commodity` / `index` /
  `cryptocurrency` - nautilus `AssetClass`.
- `price_precision`, `size_precision`, `price_increment`, `size_increment`,
  `multiplier`, `underlying`, `settlement_currency`, `base`, `quote` -
  nautilus `Instrument` / universal.
- The `ISSUER-NUMBER` account-id shape, and `MOGWAI-001` - a nautilus
  `AccountId` constraint, correctly documented as such in both the field doc
  and `docs/config.md`.
- `spot` / `future` / `perpetual` / `inverse` / `equity` class kinds, "notional",
  "maintenance" and "initial" margin, "round lot", "T+N", "locate",
  "hard-to-borrow", "Reg-T", "performance bond", "variation margin", "OHLCV",
  "BBO", "TBBO", "funding rate", "basis points", "maker" / "taker",
  "drawdown", "high-water mark" - universal derivatives and market-data
  vocabulary.
- `RUST_LOG`, `NO_COLOR`, `XDG_CACHE_HOME`, `PR_SET_PDEATHSIG`, `SIGKILL`,
  `EPIPE`, `ETXTBSY` - platform and convention.
- `humantime` duration grammar on `serve --duration` - a dependency's grammar,
  and deliberately NOT `gen`'s (pinned by test; see the lateral findings).
- `asia` / `london` / `ny-morning` / `ny-afternoon` - trading-desk vocabulary
  for CME session slices.
- "brokkr", "clap", "TOML", "XDG" - tooling.

---

# Lateral findings

Ordered by how much I think they matter.

1. **[changed] `preflight` is three different subcommands and `fit` is two,
   not three each.** Top-level
   `mogwai preflight` is the TBBO corpus contract check; `mogwai
   session-profile preflight` is the zero-volume-row gate; `mogwai stage-m
   preflight` is the calendar-bound inventory. `mogwai fit` is the protocol-11
   session calibration; `mogwai session-profile fit` is the
   calendar-conditional session fit. Five commands, two repeated leaf names,
   all operator-typed and all producing plausible evidence JSON, whether to a
   file or stdout. `docs/cli.md` documents them in separate paragraphs without
   a collision warning. The first-pass plural claim was directionally right
   and numerically wrong for `fit`. The correctness risk stands: choosing the
   wrong qualified command can succeed and leave plausible output for a
   different question. Rename the leaves for their jobs, for example corpus
   preflight, bar-exposure preflight, calendar inventory, tape fit and
   session-profile fit.

2. **`gen --type`'s help text names two of its five values.** "What to emit:
   raw trades, or aggregated OHLCV bars." The enum also carries `summary`,
   `trace` and `measure12a`, each with a real doc comment clap renders
   underneath. The summary line is narrower than the gate it describes - the
   vacuous-prose shape - and it is the first line of `--help` an operator
   reads. `GenArgs`' own struct doc has the same problem: "Dumps the offline
   generator as CSV", where three of five modes emit JSON.

3. **`docs/cli.md` says the `ReadyRecord` is "version 8" as a live fact.** It
   enumerates eleven fields and pins the schema version in durable prose. The
   `TAPE_PROTOCOL_VERSION` prose gate covers exactly two phrasings for exactly
   one constant, so nothing checks this number - it is the same
   durable-prose-asserting-a-live-fact shape `AGENTS.md` records as an open
   item, on a different constant. Either derive it or hold it against
   `ReadyRecord::VERSION` in a test.

4. **`docs/cli.md`'s `arrival-control` section states a count three times and
   parenthesizes the same correction twice.** "three since the 2026-08-09
   preset retirement" appears twice in one paragraph, once adding "the
   committed brick N artifact recorded five". A reader cannot tell whether the
   command needs three baselines or five, and the answer decides whether the
   command can run at all.

5. **`mogwai cache stats` prints a label no document uses.** `docs/cli.md`
   says it "reports entry/file/byte counts"; the command prints "provenance
   dirs:", "files:", "bytes:". An operator grepping for "entries" finds them
   only under `--entries`, where each line is prefixed `entry:`. One word, two
   spellings, in one command's output.

6. **The `[instrument]` versus `[instrument.override]` rule is stated
   correctly in `docs/presets.md` and incompletely in `docs/config.md`.**
   presets.md: "`[instrument.override]` is still the only way to reach a
   dotted path", plus the refusal on overriding an unset dotted path.
   config.md compresses this into one sentence and omits that a top-level key
   may ADD an optional section while a dotted override may not. The two
   documents are the operator's only guide to the single most error-prone
   table in the file.

7. **`docs/config.md`'s account-id section has no owner among the three
   documents.** It carries the WHO OWES AN ACCOUNT ID contract, the session
   contract, the bearer-token property and the misdial bound - four durable
   contracts, in a document titled "run configuration", attached to a key
   whose actual job is to name a default. Structurally this belongs in a
   `docs/accounts.md` or in `reference/`, cited from the key.

8. **`mogwai man` bundles seven topics and deliberately excludes the
   glossary**, on the stated grounds that it is "a working aid" for
   contributors. If the renames this inventory proposes land, that reasoning
   inverts: the glossary becomes the operator's key to a vocabulary they type,
   and an installed `mogwai` with no source tree beside it would ship every
   document that USES the words and not the one that defines them. Worth
   revisiting after the ruling, not before.

9. **`mogwai.toml`'s `[balances]` comment states a rule narrower than the
   code.** "The quote currency of the symbol this run serves MUST appear here,
   or boot is refused." The actual rule is the SETTLEMENT currency (quote for
   spot, `settlement_currency` for a future), and it refuses only for
   configured shapes while recording shipped presets as funding-barred.
   `docs/presets.md` gets this right in detail. The example file an operator
   copies gets it wrong in the direction that makes MNQ look servable on the
   default balances.

10. **Two duration grammars in one binary, pinned by test and documented
    nowhere an operator reads.** `serve --duration` takes `humantime` (`0s`,
    `1500ms`, `1ns`); `gen --length` / `--interval` / `--warmup` take an
    in-house grammar accepting only `s m h d w mo y`, refusing zero, and
    reading `1500ms` as an unknown unit. `main.rs` carries two excellent tests
    and a long comment about it. `docs/cli.md` mentions `--duration 0s`
    without noting that `gen` cannot read it. An operator will type `--length
    30m` and `--duration 30m` on the same day and be right both times, then
    type `--length 0d` and be refused for a reason no help text states.

11. **`--jobs` means two different things.** On `characterize` and
    `tick-composition` it caps corpus/traversal workers; on `arrival-screen`
    it bounds `(cell, seed)` projection workers "capped at 16 to avoid the
    measured SMT-contention regression". Same flag name, different bound,
    different default. Not a defect, but the caps are undocumented in one case
    and load-bearing in the other.

12. **`mogwai gen --config` resolves through the venue's real `Config::load`,
    which means an operator's offline chart can be refused by a rule about
    ACCOUNT FUNDING.** `docs/cli.md` presents `--config` as an instrument
    selector. The boot path it borrows also validates `[balances]` against the
    settlement currency. Worth stating, because the failure names a currency
    when the operator was asking for a chart.

---

# Open, for the owner, not to be defaulted

- **The Session ruling.** Rename the glossary's Session entry to Session id
  and free `session` for the market-day sense the entire operator surface
  already uses, or hold the entry and rename `session-profile`,
  `[instrument.session]`, the segment window vocabulary and ~30 prose sites.
  I recommend the first strongly and say so as a recommendation, not a menu:
  the wire field is `session`, the operator word is `session`, and the two
  senses are separated by a single noun ("id") at the one site that defines
  them.
- **Whether the evidence toolbox stays on the `mogwai` binary's top level.**
  Eighteen subcommands whose audience is one person fitting one instrument sit
  beside the three an operator running a venue uses. `mogwai lab <cmd>` is the
  structural move; it is a break, and pre-1.0 that is cheap.
- **Whether `[balances]` belongs inside an account policy.** The glossary
  already says a policy is opening balance plus risk rules. Folding them is a
  schema change to a table operators already write.
- **`--ledger`.** The flag and the glossary term are unrelated and both
  load-bearing. Renaming the flag is cheap and renaming the concept is not, so
  this is a ruling only in the sense that someone must say so out loud before
  the intake documents are rewritten around it.
