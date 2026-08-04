# Making the tape MNQ-ish: what we have, what it costs, what to buy

Working research report. Started 2026-08-03 as a narrow question about which
CME tick-data windows to buy with 125 dollars of Databento credit; rewritten
2026-08-04 after that question turned out to be the wrong one.

This is a `notes/`-class document: transient, no truth guarantee, nothing
durable may cite it. It records a decision, the evidence behind it, and the
reasoning that got there, so that someone picking it up cold can act without
re-deriving anything and can see which steps were wrong the first time.

---

## Contents

- [0. Summary](#0-summary)
- [1. The question, correctly stated](#1-the-question-correctly-stated)
- [2. How the tape is generated today](#2-how-the-tape-is-generated-today)
- [3. What MNQ actually gets today](#3-what-mnq-actually-gets-today)
- [4. How good is the tape, honestly](#4-how-good-is-the-tape-honestly)
- [5. The evidence base](#5-the-evidence-base)
- [6. What each quantity needs, and where it can come from](#6-what-each-quantity-needs-and-where-it-can-come-from)
- [7. Findings from the CME bar archives](#7-findings-from-the-cme-bar-archives)
- [8. Databento: facts established](#8-databento-facts-established)
- [9. Candidate baskets, priced](#9-candidate-baskets-priced)
- [10. Objections to the original basket](#10-objections-to-the-original-basket)
- [11. Recommended sequence](#11-recommended-sequence)
  - [11.1 The spread experiment contract](#111-the-spread-experiment-contract)
- [12. Corrections: things that were wrong along the way](#12-corrections-things-that-were-wrong-along-the-way)
- [13. Scripts](#13-scripts)
- [14. Open items](#14-open-items)

---

## 0. Summary

The tape's PROCESS is good. Its PARAMETERS are Bitcoin's, and for the MNQ and
MES presets that is true of everything except the tick size, the price precision
and the session calendar.

That is expected rather than broken: the five presets are aspirational, added
recently to facilitate exactly the tapes this report is about. They fill in the
slots a contract specification can supply and honestly decline to fill the rest,
which their all-`declared` provenance maps say out loud.

The original framing of this report - "which CME windows maximise value for 125
dollars" - assumed the outstanding work was buying trade-level evidence for
those empty slots. Reading the generator says otherwise. Sorted by what actually
stands in the way:

| Finding | Nature | What it needs |
|---|---|---|
| Size was configured through a crypto mean-notional proxy | **resolved 2026-08-04**: native-unit `latent_size_median` plus grid-aware validation | no purchase required |
| The venue originally published no quotes and conflated a trade displacement with spread | **B resolved at protocol 7; A remains**: the observable BBO and separate calibration seams exist, while the displacement response is still static | CME TBBO can now fit quoted width and displacement separately; free Binance quotes cover derivatives, not the shipped spot presets |
| Session profile is crypto's flat 24/7 curve | wrong data in a fillable slot | NQ 1-minute bars ALREADY ON DISK, free |
| Price level, cadence, sizes are crypto's | unfilled slots | tick data, eventually |

The structural spread blocker is resolved: protocol 7 publishes a BBO and gives
purchased quote data separate width and displacement seams. The remaining
question is calibration rather than where the evidence could land. See
[3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book). The
size blocker was resolved after this report exposed that `typical_notional` was
both denominated in the wrong unit and, exactly, the arithmetic mean notional of
a right-skewed lognormal. The session profile is the largest VISIBLE defect and
is free to fix from bars already owned. Only the fourth row is the question this
report originally set out to answer, and it is the least urgent of the four.

Recommendation: spend nothing until steps 1 to 5c of
[section 11](#11-recommended-sequence) are done. They cost zero dollars, use
data already on disk, and they change what the money should buy.

---

## 1. The question, correctly stated

The question is NOT "which CME tick-data windows should we buy."

The question is: **what would make the MNQ preset a defensible approximation of
a live MNQ trading environment, and which parts of that require buying data?**

Four constraints frame it, stated by the repository owner on 2026-08-04:

1. Binance public data (`data.binance.vision`) is free and downloadable at will.
2. A complete Kraken trade-history dump is already on disk, 43 GB.
3. There is 125 dollars of free Databento credit, and willingness to spend
   roughly another 100.
4. The target is an MNQ-ish tape.
5. A tape must never simulate half days or holidays, but MUST simulate weekends
   and sessions when the operator configures them. "Only NY open to NY close"
   and "continuous Asia session" must both be generatable.
6. No tape will ever be perfect. It is an approximation. The question is how
   good an approximation the current one is.

Critically, the Kraken fitting was an accident of availability. It was done with
what was on hand, at a time when the Binance archives were not known to be
public. It was never a judgement that Kraken was the best candidate corpus. That
reframes the fingerprint from "our chosen reference" to "our first draft."

---

## 2. How the tape is generated today

Read this section before proposing any change to the fit. Everything below is
from the shipped source, not from documentation.

### 2.1 Where the code lives

| Path | Role |
|---|---|
| `crates/mogwai-data/src/generated/source.rs` | `GeneratedSource`, the core walk |
| `crates/mogwai-data/src/generated/consts.rs` | every tuning constant |
| `crates/mogwai-data/src/generated/fingerprint.rs` | config schema plus validation |
| `crates/mogwai-data/src/generated/session.rs` | `SessionModulator`, the hour/day curves |
| `crates/mogwai-data/src/generated/calendar.rs` | `SessionCalendar`, hard open/closed |
| `crates/mogwai-data/src/generated/regime.rs` | the armed-divergence overlay |
| `analysis/fingerprint.json` | the committed fit, compiled in via `include_str!` |
| `crates/mogwai-server/presets/*.toml` | the five shipped instrument presets |
| `crates/mogwai-server/src/source.rs` | how a preset becomes a running generator |

`TAPE_PROTOCOL_VERSION` is currently **7**. Any change to a generator constant,
the arrival clock, a GARCH parameter, the fingerprint, seed derivation, the fill
band's draw or the tape origin must bump it. Nothing detects a missed bump.

### 2.2 The walk

`GeneratedSource` is a path-dependent walk. Same seed plus same tape anchor
yields the same stream byte for byte; `Clone` is the substrate of the
checkpointed seek, so a clone taken at tick N reproduces ticks N+1 onward
exactly. There is a pinned golden sequence (`clean_regime_is_byte_identical`)
that must be re-blessed on any intentional mechanism change.

One tick is produced in five stages.

**Stage 1, arrival.** Inter-arrival gaps come from a Weibull innovation
(`ARRIVAL_WEIBULL_SHAPE = 1.0`, so exponential) modulated by a two-state Markov
chain. The chain has a stationary quiet share `ARRIVAL_QUIET_FRACTION = 0.35`
and persistence `ARRIVAL_STATE_PERSISTENCE = 0.90`; quiet-state mean gaps are
`ARRIVAL_QUIET_ACTIVE_RATIO = 150` times longer than active ones. The burst
clustering lives in the state chain, not the marginal, which is why the shape
parameter is exactly 1.0 - a sub-unit shape would double-count it.

This replaced an ACD clock. The comment in `consts.rs` records why: a 500-tuple
ACD refit still produced a median 13 fills per second against a measured 4, and
17 percent empty seconds against a measured 13.4 percent. That was judged a
process-SHAPE miss rather than a calibration miss, which is the evidence the
spec required before permitting a family replacement.

**Stage 2, session modulation.** `SessionModulator` holds three precomputed
arrays from the fingerprint's `SessionProfile`: `intensity_hour[24]` and
`dow_weight[7]` (fractions, re-centred on 1.0 by multiplying by 24 and 7) and
`vol_hour[24]` (a per-mean RMS ratio, used raw). The arrival multiplier is
`intensity_hour[h] * dow_weight[d]`; a duration is DIVIDED by it. Civil fields
are derived by integer division on the unix second, with no chrono dependency.

There are two code paths, gated on `SESSION_CLOSED_ARR_MULT = 0.01`. Above the
gate, the multiplier is sampled once at the instant the gap opens and stretches
the entire draw. Below it, the draw is treated as a BUDGET of un-modulated
seconds and integrated hour by hour, because dividing a whole draw by a
near-zero share overshoots wildly (a share of 1e-6 turns a 7-second draw into 80
days) and can saturate the f64-to-u64 cast at `u64::MAX`, pinning the clock and
breaking the strict monotonicity that `MergeSource` and `seek_to` rely on.

A documented residual: only gaps that OPEN below the gate take the integrating
path. A gap opening in an open hour still crosses a later closed window at its
open-hour rate, so a tick can print inside a closed window. This is left in
place deliberately because fixing it would change every boundary-crossing gap
and break the golden stream.

**Stage 3, the latent mid.** A GARCH(1,1) recursion with Student-t innovations
(`STUDENT_T_DF = 4.0`) drives a log-price. `GARCH_ARCH = 0.12`,
`GARCH_GARCH = 0.875`, so persistence is 0.995 - very close to integrated, which
is what supplies the long memory in `abs_return_acf` out to lag 50. Two clamps
apply: a FEEDBACK clamp on the return that re-enters the recursion, and a
REALIZED clamp on the return that actually moves the mid. They are equal except
inside a `SessionEdgeSpike` window, which is exactly how a spike leaves zero
trace on the recursion state outside its own hour. `MAX_ABS_RETURN = 0.00002`
and `GARCH_SIGMA_CAP = 0.00001` bound both.

Session and regime volatility compose MULTIPLICATIVELY here. Inside the regime
envelope, a `SessionEdgeSpike` composes ADDITIVELY onto a storm baseline. Both
conventions rely on the neutral value being 1.0 and are deliberate.

**Stage 4, the trade displacement.** The venue constructs and publishes the
parent event's governing top of book before placing the trade; see
[3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book). `BounceState`
is a two-regime chain over the aggressor side. In the low regime the side flips
with probability `BOUNCE_LOW_FLIP_PROB =
0.02`; in the high regime, `BOUNCE_HIGH_FLIP_PROB = 0.25`. Regime transitions
are `BOUNCE_LOW_TO_HIGH_PROB = 0.01` and `BOUNCE_HIGH_TO_LOW_PROB = 0.022`,
giving a stationary high share of 0.3125 and a mean high-regime run of about 33
trades. That run length is what sets the abs-return ACF timescale: too long
flattens the lag1-to-lag10 decay above the lag10 ceiling, too short starves the
lag50 floor.

The TRADE PRICE is displaced by the per-instrument
`trade_displacement_ticks` from the published book midpoint on the aggressor's
side, then rounded once to the price grid. The shipped displacement remains a
static, explicitly uncalibrated half tick. It is separate from the quoted width:
a trade may execute inside, at, or outside the displayed spread. Protocol 7
pins that distinction with independently settable width and displacement tests.

**Stage 5, the sweep.** A parent event spawns `children_mean` child fills that
walk the price grid, stepping a level with probability `level_step_prob`,
multiplied by `HIGH_REGIME_LEVEL_STEP_MULT = 2.4` or
`LOW_REGIME_LEVEL_STEP_MULT = 0.3` by bounce regime. Children are spaced
`INTRA_EVENT_STEP_NS = 1000` apart and capped at `CHILD_CAP = 4096`. Sweep size
is conditioned on the arrival state (`ARRIVAL_QUIET_CHILDREN_MULT = 0.20`,
`ARRIVAL_ACTIVE_CHILDREN_MULT = 1.4307692...`), with the active multiplier fully
determined by the identity that the two must preserve the declared
unconditional mean.

At the end of a sweep the bounce drift is RE-CENTRED on the residual between the
last printed level and the latent mid, at `DRIFT_RECENTER_FRAC = 0.16`. Without
this, a burst's walk would be a permanent uncorrected excursion and the
event-layer return ACF would not stay negative.

A single-child event in the low regime re-prints the previous event's last price
with probability `EVENT_PRICE_REPEAT_PROB = 0.8`. This exists solely to hit
`zero_change_frac`: at the raw-fill layer most prints are children repeating
their parent's level by construction, but the EVENT series would otherwise
almost never repeat, because every parent re-prices off a fresh latent mid.

### 2.3 The critical split: shape versus scalars

This is the single most important architectural fact for the purchase decision.

`consts.rs` states it explicitly: the whole cadence block is "module-level
process SHAPE - deliberately NOT per-instrument, per section 10's stopping
rule."

So the parameterisation divides in two:

**Module-level, shared by every instrument, never per-instrument:** the arrival
chain, the GARCH coefficients, the bounce chain, `SIZE_LOG_SIGMA`,
`EVENT_PRICE_REPEAT_PROB`, the level-step multipliers, the clamps. Roughly
thirty constants.

**Per-instrument, in `GeneratorScalars`:** `symbol`, `modal_tick`,
`price_decimals`, `mean_event_duration_s`, `children_mean`,
`children_single_frac`, `levels_mean`, `size_round_frac`, `start_price`,
`latent_size_median`, `vol_scalar`, `quoted_width`, `top_sizes`,
`trade_displacement_ticks`. Fourteen values.

Plus `SessionProfile` (55 numbers) and `SessionCalendar` per instrument.

An MNQ preset is therefore: fourteen scalars, three session arrays, one
calendar. Buying tick data buys evidence for the fourteen and the three; it also
lets you CHECK whether the shared shape constants, fitted on crypto, survive
contact with an index future. The second is the deeper question and nobody has
asked it.

### 2.4 The fingerprint

`analysis/fingerprint.json` was fitted to **8 Kraken pairs** (ADAUSD, DOTUSD,
ETHUSD, SOLUSD, USDTUSD, XBTUSD, XDGUSD, XRPUSD), **298,003,956 trades**,
anchored on XBTUSD.

Its structure is genuinely good and should be kept: each target carries an
`anchor` (the deepest series) plus a `range` of min/median/max across pairs,
and the range IS the tolerance. That design generalises to any corpus. Re-anchor
on NQ, keep other instruments as the range, and the machinery is unchanged.

Golden targets, which the synthetic stream must reproduce:

| Target | Anchor | min | median | max |
|---|---|---|---|---|
| `duration_dispersion_cv2` | 12.6366 | 11.8507 | 33.7824 | 187.368 |
| `return_acf_lag1` | -0.19697 | -0.19697 | -0.14129 | -0.05748 |
| `abs_return_acf.lag1` | 0.30741 | 0.15242 | 0.24671 | 0.30741 |
| `abs_return_acf.lag10` | 0.15649 | 0.01779 | 0.11566 | 0.15649 |
| `abs_return_acf.lag50` | 0.12252 | 0.03887 | 0.09254 | 0.12252 |
| `zero_change_frac` | 0.47376 | 0.33620 | 0.40242 | 0.75108 |

Cadence targets, which become the per-instrument scalars:

| Target | Anchor | min | median | max |
|---|---|---|---|---|
| `mean_event_duration_s` | 0.17104 | 0.09834 | 0.17104 | 0.77438 |
| `children_mean` | 8.4905 | 4.3026 | 6.9119 | 12.7358 |
| `children_single_frac` | 0.55868 | 0.25867 | 0.49305 | 0.83803 |
| `levels_mean` | 2.2471 | 1.0 | 1.7442 | 3.3707 |
| `mean_trade_notional` | 310.75 | 100.78 | 191.40 | 466.13 |
| `duration_dispersion_cv2` | 4.6188 | 2.3841 | 4.6188 | 15.015 |
| `duration_acf_lag1` | 0.32204 | 0.18587 | 0.32204 | 0.50472 |
| `duration_acf_lag5` | 0.22388 | 0.11896 | 0.19759 | 0.33582 |

`per_second_counts`: mean 49.64, median 4, p95 257, zero fraction 0.13346.

The original `scalar_ranges` both recorded these observations and used them as
admission gates:

| Observed quantity | min | median | max |
|---|---|---|---|
| `modal_tick` | 1e-07 | 0.0001 | **0.25** |
| `price_decimals` | 1 | 4 | 7 |
| `mean_event_duration_s` | 0.09834 | 0.17104 | 0.77438 |
| `children_mean` | 4.3026 | 6.9119 | 12.7358 |
| `children_single_frac` | 0.25867 | 0.49305 | 0.83803 |
| `levels_mean` | 1.0 | 1.7442 | 3.3707 |
| `mean_trade_notional` (derived reporting only) | 100.78 | 191.40 | **466.13** |
| `size_round_frac` | 0.12179 | 0.20857 | 0.27080 |

Both bolded ceilings matter in [section 3](#3-what-mnq-actually-gets-today).

The session profile, as committed:

```
intensity_hour x24   1.117 0.966 0.880 0.817 0.789 0.749 0.777 0.810
                     0.869 0.830 0.846 0.878 1.035 1.157 1.314 1.316
                     1.332 1.160 1.133 1.119 1.141 1.047 0.997 0.922
dow_weight    x7     Sun 0.799  Mon 1.105  Tue 1.104  Wed 1.106
                     Thu 1.056  Fri 1.050  Sat 0.779
```

Peak-to-trough on the hour curve is **1.78x**. The weekend trades at about 78
percent of a weekday. Both are correct for 24/7 crypto and wrong for a CME
product by a wide margin.

### 2.5 The calendar

`SessionCalendar` is separate from `SessionProfile` and does something the
profile cannot: it expresses HARD closure. Weekly windows on a 10,080-minute
grid, an `utc_offset_minutes` for exchange-local time, `is_open`,
`next_open_ns`, and `settlement_instants`. Validation rejects overlapping
windows, more than one wrapping window, and a settlement minute outside every
window.

It repeats weekly and carries no date-specific logic. That is exactly the
requirement "weekends and sessions, never holidays or half days", expressed
structurally rather than as a rule someone must remember. Arbitrary
`open_windows` mean NY-open-to-NY-close and continuous-Asia are both just
configuration.

**Requirement 5 is already built.** It is a configuration surface, not
outstanding work.

The shipped MNQ preset already carries a full CME calendar: Sunday 18:00 through
Friday 17:00 Chicago, the daily 16:15-16:30 maintenance halt, the 17:00-18:00
break, a genuinely shut Saturday, and settlement at local minute 960.

### 2.6 The two session mechanisms

There are now two ways to express "quiet or closed":

- near-zero shares in `SessionProfile`, handled by `closed_window_gap_ns`;
- hard windows in `SessionCalendar`, handled by a jump to `next_open_ns` in
  `begin_event`.

`consts.rs` documents the first as the intended mechanism ("Trading hours,
maintenance breaks and closed weekends are expressed as NEAR-ZERO hour/day
shares in a custom SessionProfile - not a separate code path"), but the calendar
exists and runs. Whether they compose correctly, or double-count a closure, is
[open item 14.5](#14-open-items). It was not resolved here.

---

## 3. What MNQ actually gets today

### 3.0 The presets are scaffolding, and read as such

The five shipped presets are ASPIRATIONAL. They were added recently to
facilitate the new tapes this report is about, not as a claim that those tapes
already exist. Read section 3 with that in mind: what follows is not a list of
regressions, it is an inventory of which slots are filled, which are waiting,
and - the part that matters - which will REFUSE to be filled when someone tries.

The provenance maps are honest about this. `mnq.toml` marks every entry
`kind = "declared"` with a rationale, because contract specifications are
knowable today and cadence is not. `btcusdt.toml` carries the one
`kind = "fitted"` entry in the set. Nothing is claiming a standing it does not
have, which is why `docs/cli.md` can say that reading the provenance is how the
asymmetry becomes visible.

So the useful question is not "what is broken" but: **when the fit is attempted,
what stops it?** Three things do, and only one of them is a data problem.

| Finding | Status |
|---|---|
| Crypto price level, cadence, session profile | slot awaiting the fit; expected |
| Size collapsed to a constant 1 contract | resolved by native-unit latent size configuration |
| Crypto empirical ranges rejected truthful futures | resolved by separating diagnostics from admission |
| Observable BBO, configurable width, size and trade displacement | resolved at protocol 7; the seams exist and await TBBO fitting |

The structural spread blocker is resolved. The quote and trade-displacement
seams now exist at the protocol boundary; buying TBBO supplies evidence for
their values rather than waiting on another design decision. The size and
empirical-range blockers were also resolved without buying data.

### 3.1 How a preset resolves

`crates/mogwai-server/src/source.rs`, `default_profile`:

```rust
let mut scalars = GeneratorScalars::from_fingerprint_medians(&def.symbol, fp);
scalars.modal_tick = def.price_increment;
scalars.price_decimals = u32::from(def.price_precision);
scalars.top_sizes = TopOfBookSizes::uncalibrated(
    SizeGrid::from_def(&def).min_size,
);
InstrumentProfile::new(def, scalars, fp.session_profile.clone(), ...)
```

`config.rs` confirms the same for a configured instrument: if the preset has no
`[instrument.generator]` table, the scalars come from `from_fingerprint_medians`
and `modal_tick`, `price_decimals` and `top_sizes` are derived from the
instrument definition and its size grid.

**`crates/mogwai-server/presets/mnq.toml` has an `[instrument.generator]` table
but no `[instrument.session]` table.** Its generator values are declared
explicitly rather than inherited from `from_fingerprint_medians`; the table
also provides the uncalibrated quote-width, quote-size and trade-displacement
seams added at protocol 7. It carries the instrument definition, margin policy
and CME calendar alongside those declared generator values. The missing fit is
an evidential gap, not an absent configuration table.

So the shipped MNQ preset resolves to:

| Parameter | MNQ gets | Standing |
|---|---|---|
| `modal_tick` | 0.25 | filled, from the contract spec |
| `price_decimals` | 2 | filled, from the contract spec |
| `calendar` | full CME hours | filled, from published CME hours |
| `start_price` | 60000 (`START_PRICE_USD`) | unfilled; that is Bitcoin's level |
| `latent_size_median` | 1 contract | filled as a declared pre-grid center, see 3.3 |
| `mean_event_duration_s` | 0.17104 | unfilled, crypto anchor |
| `children_mean` | 8.4905 | unfilled, crypto anchor |
| `children_single_frac` | 0.55868 | unfilled, crypto anchor |
| `levels_mean` | 2.2471 | unfilled, crypto anchor |
| `size_round_frac` | 0.20857 | unfilled, crypto median |
| `vol_scalar` | 1e-06 | unfilled, crypto constant |
| `session_profile` | crypto 24/7 curve | unfilled; free to fill, see 5.3 |
| every shape constant | crypto fit | shared by design; untested off crypto |

**Every preset - MNQ, MES, BTCUSDT, ETHUSDT, SOLUSDT - runs identical tape
dynamics.** A preset changes the price grid, the size grid, the instrument
class, the margin table and the calendar. It does not change the process. For
the crypto presets that is nearly right, since the fingerprint was fitted on
crypto; for the index futures it is the whole of the outstanding work.

### 3.2 The size distribution collapses to a constant

Before the resolution, `GeneratedSource::try_with_clamp_override` computed:

```
size_median = typical_notional / (start_price * multiplier) / exp(SIZE_LOG_SIGMA^2 / 2)
```

For MNQ before the fix: `310.75 / (60000 * 2) / 1.9374 = 0.001337` contracts.

Sizes are then drawn `LogNormal(ln(0.001337), 1.15)`, and on a futures grid
`next_size` rounds to zero decimals half-away-from-zero and floors at
`min_size = 1`. The probability a draw reaches 0.5 and rounds to 1 rather than
being floored is about 1e-7.

**Every MNQ trade prints exactly 1 contract.** `SIZE_LOG_SIGMA` is inert. Any
execution behaviour that depends on trade size - queue position, partial fills
against available size, size-dependent slippage - has no variation to work with.

MEASURED, once `gen` could build an MNQ source
([3.6](#36-the-offline-generator-can-now-chart-a-preset)):

```
$ mogwai gen --symbol MNQ --type trades --length 1d
trades in 1 sim-day: 4,583,372
distinct sizes:      1
size histogram:      [('1', 4583372)]
price min/max:       59089.00 / 60094.25
```

Every one of 4.58 million trades is a single contract, and the price walks
around 60,000 rather than the Nasdaq's level. The trade count is its own
finding: 4.58 million per day is about 53 per second sustained, which is the
crypto arrival rate, not an index future's.

The test suite already half-knows this. `contract_size_median_tracks_notional_...`
uses an overridden notional of 200,000 to get a median of 0.476 contracts, and
the comment on the round-lot test says plainly that "the floor is one contract
and the target here is 0.476 of one, so ANY grid passes it, including a broken
one that returns the floor for every draw."

### 3.3 Resolved: size is expressed in native units

To make MNQ's median trade one contract at NQ 21,000:

```
typical_notional = 1 * 21000 * 2 * exp(1.15^2 / 2) = 81,354 USD
```

That calculation exposed the proxy rather than supplying its replacement. The
field was exactly arithmetic mean trade notional because construction divided
by `exp(1.15^2 / 2)`. It is now removed. `latent_size_median` states the
continuous lognormal center directly in the instrument's native size unit,
before minimum-size flooring, grid quantization and round-lot snapping. MNQ
declares one contract.

Empirical `scalar_ranges` are now corpus-labelled `empirical_ranges`. They
produce warnings and select defaults but do not admit instruments. Hard checks
come from the mechanism: native-unit size coherence, decimal representability,
sweep relationships and volatility headroom. The shipped-preset test makes all
unaccepted warnings fatal for committed presets, and also rejects stale
provenance acceptances after a warning disappears. Operator configs retain
warnings.

`modal_tick.max` is 0.25 and MNQ's tick is exactly 0.25, so it passes by
coincidence, inclusively, sitting exactly on the ceiling. An instrument with a
coarser tick (ES at 0.25 is fine, but a 1.0-tick product is not) would fail.

The test configs now state `latent_size_median = "1"`. Their deliberately 24/7
calendar remains a fixture simplification rather than the shipped MNQ calendar.

### 3.4 The session profile is the wrong shape

Covered in [2.4](#24-the-fingerprint). Crypto's hour curve spans 1.78x
peak-to-trough. An index future's spans something closer to 20x to 50x between
the cash open and the Asian overnight, and drops to zero across the weekend and
the daily maintenance break.

The MNQ preset's CME calendar closes the market correctly, so the weekend and
the halt ARE handled. What is not handled is the SHAPE of activity inside the
open hours: a flat crypto curve says the 03:00 Chicago hour is as busy as the
08:30 hour, which is wrong by more than an order of magnitude.

### 3.5 Resolved: the venue publishes an observable top of book

RESOLVED 2026-08-04 at tape protocol 7. `GeneratedSource` now emits one BBO
before every parent burst, `/quotes` scans the deterministic history, a
connecting WebSocket receives the current BBO snapshot, and the adapter retains
and replays it when the host activates quote delivery. Quoted width, top sizes,
and trade displacement are separate per-instrument calibration seams. Their
shipped values are explicitly uncalibrated placeholders pending CME TBBO.

The remainder of this section records the diagnosis that motivated the layer
and the distinction the implementation preserves.

Three distinct mechanisms are easy to conflate here, and earlier drafts of this
report conflated two of them into one. They are separate, and the defect is
bigger than the constant that first drew attention.

**Before protocol 7, nothing in the workspace constructed a `QuoteTick`.** The wire type was
defined in `mogwai-protocol`, `TickEvent::Quote` is defined in `mogwai-data`,
the server's tape loop relayed that variant, and `mogwai-adapter` converted it to
a nautilus `QuoteTick`, but no source produced one. The `/quotes` handler returned
an empty vector unconditionally. Protocol 7 closed each of those gaps.

This was the load-bearing finding, and earlier drafts of this report missed it
entirely because they trusted a constant's name. Purchased quote data had no
landing site in the venue's output until protocol 7 added this layer. Fitting a
spread to a venue that published none would have left the fit visible only in
the trade prints.

**`trade_displacement_ticks` is a trade displacement, not a spread.** Until
2026-08-04 its default was exposed only as `HALF_SPREAD_TICKS`, later renamed
`TRADE_BOUNCE_HALF_WIDTH_TICKS`, and that original name is the whole reason this
took as long to see as it did. Protocol 7 moved the value into per-instrument
`GeneratorScalars`; the constant now supplies only its explicitly uncalibrated
half-tick default. `next_price` reads the configured value and displaces the
TRADE PRICE that far from the governing book midpoint on the aggressor's side.
Two consecutive opposite-sided prints against an unchanged midpoint land two
displacements apart; that separation is a property of the print series, not of
the displayed width.

The configured value remains static over a run, so the displacement is
identical in every regime, but it can differ by instrument. Real MNQ trades
close to the touch most of the time and both widens its book and displaces its
prints further under stress - which is precisely when execution quality
collapses and precisely what a divergence-injecting venue exists to exercise.
The tape now expresses a displayed book and independent static displacement,
but neither responds dynamically to that stress.

The per-instrument configuration seam now exists. Making its response dynamic
is a mutation, not a restructuring. An earlier draft claimed "there is nowhere
to put a regime-varying spread"; that overstated the obstacle for the
displacement and understated it for the quote layer, which genuinely did not
exist then.

**The blocker therefore splits in two.**

- **A. Volatility-dependent aggressor-side trade displacement.** Drive the
  configured displacement from the volatility state the mid already carries.
  Protocol 7 created the per-instrument seam; making its response dynamic
  changes the stream and needs another protocol bump.
- **B. An observable top-of-book market. RESOLVED.** Protocol 7 synthesizes and
  emits `QuoteTick`, serves bounded history, and preserves the current book
  through server and adapter subscription races.

A remains a valid tape-fidelity improvement. B now makes spread observable at
the protocol boundary without pretending that its placeholders are calibrated.

**The two observables are not one parameter.** TBBO identifies the trade
displacement only RELATIVE TO the contemporaneous quote midpoint, and it must
never be assumed that the displacement equals half the quoted width:

```
quoted spread    = ask - bid
effective spread = 2 * aggressor_sign * (trade_price - quote_mid)
```

A trade can execute inside, at, or outside the displayed spread. The model needs
two separate response functions, even where both are driven by the same causal
volatility state. Purchased quote data accordingly has two legitimate landing
sites - fitting the observable BBO, and fitting executions relative to that BBO -
and a design that collapses them into one number cannot represent a market where
they diverge.

**The server's fill band.** `fill_band_vol_mult` in `mogwai-server` is a
different thing: an order's trigger is drawn uniformly from `0 ..= band_ticks`
ticks AWAY from its stated price, where `band_ticks` is this multiplier times
the tape's realized volatility scaled to `FILL_HORIZON_NS`.

This is NOT an asserted number, contrary to what the first draft of this report
said. It was selected by `fills::vol_probe`'s PROCEED rule: the smallest
multiplier whose median implied band lands in a 3-to-100-tick usable window. On
the committed BTCUSDT profile it reads a median implied band of 4 ticks and a
p90 of 7. It replaced `0.5`, which had been calibrated against the PRINT-layer
tape where a 300-second window carried about 32 returns; the same window now
carries about 15,700, so the estimator's horizon return rose by two orders of
magnitude and `0.5` implied a median band of 439 ticks against a
`fill_band_max_ticks` clamp of 200. A clamp-saturated band draws uniformly
across the whole clamp range regardless of the tape, which is the mirror image
of an inert zero band: in neither case does the tape decide the fill.

So the band IS calibrated, and rigorously - but to a USABILITY criterion (does
the implied band land in a range where the tape can still decide fills), not to
any measured market quantity. Nothing in the pipeline has ever compared it to a
real quoted or effective spread.

That is the honest case for quote data: not that the band is arbitrary, but that
its calibration target is internal. CME TBBO is what would let the band be
anchored to a measured spread. Free Binance quotes do not substitute for the
shipped spot presets - see
[5.2.1](#521-correction-free-historical-quote-truth-does-not-cover-the-shipped-presets).

### 3.6 The offline generator can now chart a preset

RESOLVED 2026-08-04, and it turned up a second bug on the way.

`mogwai gen` used to build from `InstrumentProfiles::defaults()`, and
`default_instruments()` returns exactly one instrument, BTCUSDT, so any preset
symbol failed:

```
Error: unknown symbol MNQ: the built-in venue does not list it
```

`--symbol` now falls back to an embedded preset, resolved through the same
`effective_preset` path the server boots from, so preset inheritance (MES over
MNQ) and the provenance completeness check apply identically. The built-in venue
is still consulted first, which keeps `--symbol BTCUSDT` byte-identical.

The second bug: **`gen` never applied the instrument's `SessionCalendar`.** The
served path (`source::generator`) has always chained `.with_calendar(..)`;
the offline dump did not. A session-bearing instrument therefore printed
straight through its own closed weekend and daily maintenance halt, so any chart
produced from it would have misrepresented the tape even once the symbol
resolved. Now applied. The difference is immediate:

```
$ mogwai gen --symbol MNQ --type bars --interval 1m --length 3d
bars: 4320, empty (zero-volume): 1650 = 38.2%
```

Those 38.2 percent are the CME calendar - the shut Saturday, the Friday
evening close and the daily 16:15-16:30 halt - which is exactly what
requirement 5 asks a tape to express and what the crypto session profile alone
could never produce.

---

## 4. How good is the tape, honestly

Assessed from `docs/example-generated-bars.png`, which is a BTCUSDT run:
5,760 bars, zero empty.

**What is genuinely good.** The price path is convincing. Volatility clusters
visibly, there is a sharp dislocation and recovery around 15:00 that looks like
a real liquidation cascade rather than a Gaussian outlier, and the small-scale
structure has the right roughness. That is what GARCH persistence of 0.995 with
Student-t(4) innovations buys, and it is not easy to get. The cadence work is
real: the arrival chain was replaced on measured evidence, not on taste, and the
constants carry their derivations in comments.

For a crypto instrument this is a good approximation. The honest answer to "how
good is it" is: as a BTCUSDT tape, good. As an MNQ tape, it is a BTCUSDT tape.

**What is visibly wrong for an index future.** Volume in that chart is flat
across all 24 hours and across day boundaries. There is no open, no close, no
overnight lull. The header says zero empty windows; real MNQ overnight has
plenty of empty one-minute windows. And the price level is 60,000, which is
Bitcoin's, not the Nasdaq's.

An experienced futures trader would identify that chart as not-a-future within
about two seconds, and the tell would be the volume profile, not the price path.

That ordering matters for spending: the microstructure moments this report
originally proposed to buy are the part that is already good. The session shape,
which is the part that is obviously wrong, is free to fix.

---

## 5. The evidence base

### 5.1 Kraken trade history, 43 GB, on disk

`/home/folk/Kraken/` holds a per-pair CSV dump of Kraken trade history, 43 GB
total, hundreds of pairs. The large ones:

| File | Size |
|---|---|
| `XBTEUR.csv` | 3.0 GB |
| `XBTUSD.csv` | 2.6 GB |
| `ETHUSD.csv` | 1.7 GB |
| `SOLUSD.csv` | 671 MB |

This is the fingerprint's source corpus. Trade-level, years deep, free, already
owned.

The schema is confirmed from the shipped parser (`KrakenCsvSource` in
`mogwai-data/src/lib.rs`), not from opening the files: no header, one file per
pair with the symbol taken from the file stem, three columns `time,price,volume`
where time is unix SECONDS, optionally fractional. So there is no aggressor side
and no quotes. Every current golden target is computable; spread and signed
order flow are not.

Hundreds of pairs at different price levels and tick sizes is a far better
testbed for the "is `zero_change_frac` an instrument constant or a tick-to-price
ratio" question than any single purchased instrument.

### 5.2 Binance archives, free, partially on disk

`research/market-data/` (gitignored) holds, from `data.binance.vision`:

```
BTCUSDT-trades-2026-06.zip     914 MB
ETHUSDT-trades-2026-06.zip     906 MB
SOLUSDT-trades-2026-06.zip     271 MB
BTCUSDT-aggTrades-2026-06.zip  498 MB
ETHUSDT-aggTrades-2026-06.zip  466 MB
SOLUSDT-aggTrades-2026-06.zip   91 MB
BTCUSDT/ETHUSDT/SOLUSDT-1s-2026-04 .. 2026-08   (1-second klines)
CME_MINI_MNQ1!, 15S_aedc1.csv
CME_MINI_MES1!, 15S_d2dee.csv
BINANCE_BTCUSDT, 15S_213af.csv
```

One thing makes this materially better than the Kraken corpus for fitting.
Binance `trades` carries `isBuyerMaker`, the AGGRESSOR SIDE, which Kraken's dump
does not. Signed order flow drives adverse selection, and the generator already
models an aggressor-side chain it currently has no way to validate.

The documented spot trade schema, confirmed against the vendor's own
`binance/binance-public-data` documentation rather than inferred:

```
tradeId, price, qty, quoteQty, time, isBuyerMaker, isBestMatch
```

Spot timestamps are MICROSECONDS from 2025 onward, which is a trap for any
parser that assumes milliseconds.

### 5.2.1 Correction: free historical quote truth does NOT cover the shipped presets

An earlier draft of this report claimed the archive publishes `bookTicker`, and
concluded that spread ground truth was free for three of the five shipped
presets. **That was wrong, and it conflated two different markets.**

The official archive documents exactly three historical SPOT datasets:
`aggTrades`, `klines` and `trades`. Historical spot `bookTicker` is not among
them. Archived `bookTicker` is a DERIVATIVES dataset. The shipped BTCUSDT,
ETHUSDT and SOLUSDT presets are spot instruments, so the archived quotes do not
describe the instruments the presets model.

Binance spot does expose live `<symbol>@bookTicker` updates over websocket, with
update id, symbol, and bid and ask price and quantity - but that is a live
stream, not a historical archive.

So there are two free experiments, and they have DIFFERENT STANDING. Conflating
them is what produced the error:

| | What it validates | What it does not |
|---|---|---|
| **USD-M futures archive**: pair futures `trades` with futures `bookTicker` | the Roll-versus-truth methodology, and volatility stratification | it is not direct evidence for the shipped SPOT presets |
| **Live spot collection**: record spot `trades` and `<symbol>@bookTicker` concurrently | matches the shipped presets exactly | requires accumulating a new synchronized corpus over time, so it is not available today |

The methodology experiment is available immediately and is worth running on its
own terms. Direct evidence for the spot presets requires building a collector
and waiting.

The archive is downloadable at will. Throttling thresholds are unknown. The
archived futures `bookTicker` column layout is still unverified and must be read
off a downloaded daily file rather than assumed - assuming a layout is precisely
the mistake corrected above.

### 5.3 CME 1-minute bar archives, on disk

Four vendor archives in `research/market-data`, about 9 euro each:

| File | Symbol | Rows | Span | Adjustment |
|---|---|---|---|---|
| `nq-1m_bk.zip` | NQ | 5,891,412 | 2008-12-11 to 2026-07-08 | ratio back-adjusted |
| `es-1m_bk.zip` | ES | 6,148,265 | 2008-12-11 to 2026-07-08 | ratio back-adjusted |
| `cl-1m_bk.zip` | CL | 6,014,451 | 2008-11-20 to 2026-07-08 | ratio back-adjusted |
| `gc-1m.zip` | GC | 6,631,639 | 2007-04-01 to 2026-07-30 | RAW, not adjusted |

Format, no header: `DD/MM/YYYY;HH:MM;O;H;L;C;V`, semicolon-delimited. NQ, ES and
CL use `HH:MM` with LF endings; GC uses `HH:MM:SS` with CRLF. Timestamps are CME
time (US Central), confirmed by the owner and independently by the near-empty
16:00 hour and by a roughly 3,660-second gap appearing about 2,130 times per
file, which is the daily maintenance break.

The `_bk` suffix means back-adjusted. CL is the proof: it shows 173 dollars in
November 2008 when WTI traded near 55, because crude is persistently in contango
and the cumulative roll factor compounds. NQ and ES sit close to their real 2008
levels because index futures carry a small basis. Ratio adjustment preserves
RETURNS exactly, so regime stratification is unaffected; what dies is price
LEVEL and the tick grid. NQ, ES and CL can therefore never source a `modal_tick`
or a `zero_change_frac` analogue. GC could, being raw.

**These bars can source the entire session profile.** `intensity_hour`,
`vol_hour` and `dow_weight` are hour-of-day and day-of-week aggregates: volume
per minute gives intensity, RMS return per minute gives the vol curve,
day-of-week volume gives the day weights. Seventeen years of NQ minutes is a far
larger sample than any month of ticks. This is the free fix for the largest
visible defect.

### 5.4 What no free source supplies

CME quotes. Kraken has no quotes; Binance quotes are for crypto; the CME bar
archives have no bid or ask. Both fitted quantities need them, and need them
jointly: the quoted width of the synthesized BBO, and the trade displacement
measured against that BBO's contemporaneous midpoint. If either is to be a
fitted, regime-varying quantity for MNQ specifically, that is what the money is
for. Protocol 7 supplies the quote layer and separate configuration seams for
both. Neither can be fitted for MNQ without the joint trade-and-quote evidence -
see [3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book).

---

## 6. What each quantity needs, and where it can come from

| Quantity | Level | Free source available? | Notes |
|---|---|---|---|
| `intensity_hour`, `vol_hour`, `dow_weight` | per-instrument | **yes, NQ 1m bars on disk** | the biggest visible win |
| `SessionCalendar` | per-instrument | already correct in the preset | done |
| `modal_tick`, `price_decimals` | per-instrument | contract spec | done |
| `start_price` | per-instrument | any NQ quote | trivial |
| `latent_size_median` | per-instrument | contract spec plus trade-size evidence | representable; declared as 1 pending a fit |
| `mean_event_duration_s` | per-instrument | no; needs trade counts | bars carry volume, not trade count |
| `children_mean`, `children_single_frac`, `levels_mean` | per-instrument | no; needs ticks | sweep structure is sub-bar |
| `size_round_frac` | per-instrument | no; needs ticks | round-lot mass |
| `duration_dispersion_cv2`, `duration_acf_*` | shape check | no; needs ticks | crypto-fitted, untested off crypto |
| `return_acf_lag1` | shape check | no; needs ticks | also the Roll spread estimator input |
| `abs_return_acf` lag 1/10/50 | shape check | partly | long memory is scale-free; minute-scale GARCH persistence is estimable, lag indices differ |
| `zero_change_frac` | shape check | no; needs ticks AND a raw tick grid | see [section 7](#7-findings-from-the-cme-bar-archives) |
| Trade displacement (`trade_displacement_ticks`) and any variation | per-instrument seam, plus a shape check | not for the shipped spot presets: archived Binance quotes are derivatives, and live spot quotes need a collector | CME needs TBBO; measured against the contemporaneous quote mid, not half the quoted width |
| Quoted BBO width and top-of-book sizes | per-instrument seams | no | protocol 7 carries both; sizes remain an explicit uncalibrated input and are not derived from trade sizes |

The direction of error matters and is one-sided: every bar-derived substitute
for a tick-level quantity makes the tape MORE REGULAR than reality. Clustered
arrivals and bounce are what make fills bad, so a preset fitted from bars would
bias forward-test claims optimistically and systematically - the same defect the
zero-commission argument was killed for.

---

## 7. Findings from the CME bar archives

### 7.1 `zero_change_frac` is not an instrument constant

Yearly medians of the minute-scale zero-change fraction:

| | 2009-2014 | 2026 | change |
|---|---|---|---|
| NQ | ~0.25 | 0.021 | 12x down |
| ES | ~0.39 | 0.120 | 3x down |
| GC | ~0.18 | 0.031 | 6x down |
| CL | ~0.16 | 0.092 | roughly flat |

The cause is mechanical and CL is the control that proves it. NQ rose from 1,400
to 29,400 (21x) against a fixed 0.25 tick; GC rose from 671 to 4,300 (6.4x)
against a fixed 0.1 tick; both saw the zero-change fraction fall by nearly the
same factor as their price rose. CL's level did not systematically rise and its
zero-change fraction did not systematically fall.

So this quantity tracks the tick-size-to-price-and-volatility ratio, not the
instrument. Two consequences:

- The committed anchor 0.4738 is a Kraken-at-that-price-level number. Any preset
  carrying it as a fitted constant drifts out of calibration as the synthetic
  price wanders. Whether the generator should DERIVE it from the tick grid is
  [open item 14.3](#14-open-items).
- Old data measures a structurally different market, which is why any basket
  should weight recent months and carry at most one historical probe.

By contrast `volume_cv`, the arrival-burstiness proxy, is stable (1.8-1.9 early,
1.5-1.6 now), so cadence constants are plausibly era-portable. That is what
justifies buying any old data at all.

**This finding can be pushed much further for free.** Hundreds of Kraken pairs
sit at different price levels and tick sizes. A derivation of `zero_change_frac`
from the tick-to-price ratio can be developed and validated across all of them
at zero cost. That reduces the purchased GC probe from load-bearing evidence to
a cross-asset-class confirmation.

### 7.2 Method for window selection

Bars cannot see microstructure, so they are used as a SAMPLING FRAME rather than
as a source of constants. `analysis/select_windows.py` computes per-session
features (realized volatility, hourly vol-of-vol, volume level, per-minute
volume burstiness, zero-change fraction, overnight gap), aggregates to monthly
medians, z-scores across months, and runs farthest-point selection plus a
volatility-stratified pick within the current era.

Stated limitation, which must survive into any preset's provenance: the strata
are chosen on VOLATILITY AND VOLUME regimes, not on microstructure regimes. The
tick data itself is what reports whether those coincided. Either answer is a
finding.

**This assumption is testable for free and has not been tested.** Build 1-minute
bars FROM the Kraken ticks, run the `select_windows.py` feature pipeline on
them, then compute the tick-level targets directly and check whether the
bar-derived stratification predicts the microstructure. Same instrument, same
period, same venue, no vendor mismatch. If it fails, the volatility-stratified
window selection underpinning every basket below is unjustified and the money
should go to contiguous recent months instead.

---

## 8. Databento: facts established

All via free metadata calls.

- Dataset `GLBX.MDP3`. Coverage starts **2010-06-06** for `trades`, `tbbo`,
  `mbp-1`, `mbp-10`, `definition`, `statistics`. `mbo` only from 2017-05-21.
- Pricing is per byte within a schema, and the per-GB rate differs BETWEEN
  schemas (the vendor lists OHLCV-1s at 70, trades at 28 and MBO at 1.8 dollars
  per GB). Comparing those rates directly is the trap: MBO is cheapest per GB
  and by far the most expensive per unit of coverage, because it carries orders
  of magnitude more records. Measured here: **trades ~1.25 dollars per million
  records, tbbo ~2.09** (exactly 1.667x at identical record counts, since TBBO
  is one record per trade with the book top attached). Both work out to about
  **26 dollars per GB**, so the whole trades-versus-tbbo price difference is
  that a TBBO record is 1.667x larger, not a different rate.
- Scope multipliers, same window: the whole CME book is about 6.4x four
  continuous symbols; four symbols is about 2.5x NQ alone.
- `definition` and `statistics` on a narrow scope are effectively free (0.00 to
  0.02 dollars) and are needed to resolve the front month and to recover the
  tick grid the adjusted bars destroyed.
- `c.0` is CALENDAR ranked and holds the nearest expiry until it expires; `v.0`
  follows volume. They are byte-identical in non-roll months and differ by 9-16
  percent of records in the four quarterly roll months, where `c.0` tracks the
  contract liquidity is leaving. **Use `v.0`.**
- Date-only bounds are interpreted as UTC. Since a CME session runs 17:00 to
  16:00 Central, date bounds clip 5-6 hours off the opening session and pull in
  the same slice of the session after the end. Bounds must be explicit UTC
  instants derived from the Central session boundary, DST included.
- MNQ and MES did not list until May 2019, confirmed empirically: the 2011-08
  and 2016-01 windows price identically with and without them.
- Micros are expensive out of proportion to their apparent usefulness, adding 22
  to 61 dollars per month and rising over time, because MNQ and MES now print
  more trades than NQ, ES, CL and GC combined.

---

## 9. Candidate baskets, priced

All priced against the live API on 2026-08-04 and cached in
`analysis/databento_cache.json`. Symbol `NQ.v.0` unless stated, dataset
`GLBX.MDP3`, bounds as explicit UTC instants. Sizes are DBN-encoded, which is
what a batch download transfers; CSV output is several times larger.

### 9.1 Basket A, the original (`plan nqv basket`)

| Window | Stratum | Schema | Cost | Size |
|---|---|---|---|---|
| 2024-05 | calmest current-era, p0 | tbbo | 16.29 | 0.62 GB |
| 2024-09 | p60 | trades | 8.64 | 0.33 GB |
| 2025-11 | p80 | trades | 10.91 | 0.42 GB |
| 2025-04 | current-era stress peak, p100 | tbbo | 23.88 | 0.92 GB |
| 2026-03 | recent high-vol | trades | 10.98 | 0.42 GB |
| 2026-06 | most recent | trades | 11.56 | 0.44 GB |
| 2020-03 | extreme dislocation | tbbo | 22.70 | 0.87 GB |
| 2011-08 | drift probe, pre-micro era | trades | 6.42 | 0.25 GB |
| `MNQ.v.0` 2024-05-06 to 05-20 | paired micro comparison | trades | 6.19 | ~0.15 GB |
| all windows | `definition` + `statistics` | | 0.03 | negligible |
| | | **total** | **117.59** | **~4.4 GB** |

The nine-NQ-window variant that keeps 2024-11 instead of the MNQ pair totals
120.08 dollars and 4.60 GB.

### 9.2 Basket B, depth over length (`plan nqv depth`)

| Window | Stratum | Schema | Cost | Size |
|---|---|---|---|---|
| 2024-05 full | p0 anchor, full month for the ACF tail | tbbo | 16.29 | 0.62 GB |
| 2025-04 full | p100 anchor, full month for the ACF tail | tbbo | 23.88 | 0.92 GB |
| 2024-09 2wk | p60 | tbbo | 7.92 | 0.30 GB |
| 2025-11 2wk | p80 | tbbo | 9.20 | 0.35 GB |
| 2026-03 2wk | recent high-vol | tbbo | 9.27 | 0.36 GB |
| 2026-06 3wk | most recent | tbbo | 12.10 | 0.46 GB |
| 2020-03 2wk | the dislocation itself | tbbo | 8.75 | 0.34 GB |
| 2011-08 2wk | drift probe; drift needs no spread | trades | 3.57 | 0.14 GB |
| | | **total** | **90.99** | **3.49 GB** |

Seven TBBO windows instead of three, for 26.60 less than Basket A.

### 9.3 The paired contract test (`plan pairv pair`)

| Window | Purpose | Schema | Cost | Size |
|---|---|---|---|---|
| `NQ.v.0` + `MNQ.v.0` 2024-05-06 to 05-20 | contract vs market, BOTH legs | trades | 10.02 | 0.38 GB |

8,007,933 records across both legs. In 2024 MNQ and NQ print comparable trade
counts, so the comparison is balanced rather than one leg swamping the other.

### 9.4 The second tick grid (`plan gcv grid`)

| Window | Purpose | Schema | Cost | Size |
|---|---|---|---|---|
| `GC.v.0` 2024-05 2wk | second tick grid, calm | trades | 1.20 | 0.05 GB |
| `GC.v.0` 2025-04 2wk | second tick grid, stressed | trades | 1.61 | 0.06 GB |
| | | **total** | **2.81** | **0.11 GB** |

GC prints under a million trades per fortnight against NQ's four million, which
is why this is nearly free. Its value has dropped since the Kraken corpus was
found (see [7.1](#71-zero_change_frac-is-not-an-instrument-constant)): it is now
a cross-asset-class confirmation rather than the only available second grid.

### 9.5 Combined

Basket B plus the paired test plus the GC probe: **103.82 dollars, ~3.98 GB**,
leaving 21.18 of the free credit unspent.

---

## 10. Objections to the original basket

Raised on review, 2026-08-04, before the generator was read. They survive that
reading, but they are now second-order to [section 3](#3-what-mnq-actually-gets-today).

### 10.1 The invalidating test was scheduled last

`docs/presets.md` ships MNQ and MES; NQ is not a preset and is not on the
roadmap. Basket A spends about 111 dollars on NQ to fit a preset for MNQ, and
spends 6.19 testing whether that substitution is legitimate - after the other 95
percent is committed.

The failure is asymmetric. MNQ now prints more trades than NQ, ES, CL and GC
combined, so a refit on MNQ costs a multiple of a refit on NQ, and there would
be nothing left to pay for it. Buy the paired comparison first: 10.02 dollars,
both legs, same instants, same volume ranking.

### 10.2 Month length was over-bought, schema depth under-bought

Full months were adopted because narrowing to one symbol made them affordable,
not because an estimator demanded them. For a coefficient of variation, a lag-1
return ACF or a log-size sigma, ten sessions is already past the point where
standard error binds.

TBBO was meanwhile rationed because it costs 1.667x - and TBBO is the only thing
in the basket that can address the spread question of
[3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book). Rationing the only external evidence for
the one quantity with no external calibration at all is the wrong economy.

The honest counter: the `abs_return_acf` tail at lag 50 and the GARCH
persistence term are the one target family where contiguous sample LENGTH binds
rather than session count. Basket B keeps 2024-05 and 2025-04 at full months for
exactly that reason and shortens the rest.

### 10.3 Headroom was too thin

117.59 of 125 leaves 7.41, on a purchase whose top identified risk is a bounds
mistake across a DST-shifting Central session boundary. One botched window and
there is no re-buy. Staging fixes this at no cost.

### 10.4 Kept despite doubt

2020-03 is 19 percent of Basket A spent on a five-year-old regime, and
[7.1](#71-zero_change_frac-is-not-an-instrument-constant) argues old data
measures a different market. It stays: mogwai exists to make execution look BAD,
and March 2020 is the only window where the tape shows what that looks like.

### 10.5 ES and CL stay dropped

Neither is a preset target, both are back-adjusted so neither can source a tick
grid, and nothing in the fit wants them.

---

## 11. Recommended sequence

Ordered by value per dollar and by what unblocks what. Steps 1 to 3 cost
nothing.

**Step 1. Refit the session profile from bars already on disk.** Compute
`intensity_hour`, `vol_hour` and `dow_weight` from `nq-1m_bk.zip`, pair with the
CME calendar already in `mnq.toml`. This fixes the largest visible defect
([3.4](#34-the-session-profile-is-the-wrong-shape)) for zero dollars.
Bumps `TAPE_PROTOCOL_VERSION`.

**Step 2. Make MNQ chartable.** Teach `mogwai gen` to load a preset, or add MNQ
to the default instrument list, so the claims in section 3 can be seen rather
than derived. Regenerate `docs/example-generated-bars.png`'s MNQ counterpart.
This is what turns this document's arithmetic into evidence.

**Step 3. Fix the notional blocker. RESOLVED 2026-08-04.** The generator now
accepts `latent_size_median` in native size units, separates mechanism gates
from corpus diagnostics, and rejects the old unit-mismatch signature against
the instrument's minimum tradable size. No purchase was required.

**Step 4. Validate the sampling frame, free.** Build 1-minute bars from Kraken
ticks, run the `select_windows.py` pipeline, and check whether bar-derived
strata predict tick-level microstructure ([7.2](#72-method-for-window-selection)).
If they do not, every basket below needs re-selecting.

**Step 5a. Decompose the SYNTHETIC spread. RESOLVED 2026-08-04.** Protocol 7
reports the shipped tape against its published book and retains the latent-mid
comparison as a separate unobservable model diagnostic. The output schema is
the contract both real-data experiments below must match:

- configured quoted width and independently configured trade displacement;
- effective spread against the published midpoint, with the latent-mid-relative
  quantity retained alongside;
- repeated and non-repeated parents reported separately against the frozen book;
- parent-layer price-change covariance;
- parent-layer Roll spread, in price AND in ticks;
- an explicit UNAVAILABLE result when the covariance is non-negative, never a
  zero or a silently dropped row;
- the opposite-side parent separation distribution;
- `zero_change_frac`;
- the conditional return scale and tick-traversal strata.

A protocol-5 run of this produced a configured 1 tick, an almost-sure 2-tick
grid separation and a 1.48-tick Roll estimate - three numbers, none agreeing.
**That result is historical context only and must not be used as calibration
evidence.** The protocol 7 decomposition has now been regenerated at the
observable referent and remains a report, not calibration evidence by itself.

**Step 5b. Calibrate the estimator against real quotes, free.** Per
[5.2.1](#521-correction-free-historical-quote-truth-does-not-cover-the-shipped-presets)
this is TWO experiments with different standing. The USD-M futures archive pairs
`trades` with `bookTicker` today and validates the Roll-versus-truth methodology
and the volatility stratification, but it is not direct evidence for the spot
presets. Direct evidence needs a live spot collector recording `trades` and
`<symbol>@bookTicker` concurrently, accumulated over time.

Either way the mechanism is the same: `return_acf_lag1` IS the Roll estimator's
input, since effective spread is approximately `2 * sqrt(-cov(dP_t, dP_t-1))`.
Measure how well the inference recovers true effective spread and how its bias
behaves across volatility regimes. If it is good enough, buy CME `trades` and
infer the spread, which converts Basket B into something far cheaper. If it is
not, the case for TBBO is established with evidence rather than asserted.

**Step 5c. Build the market before buying data to fit it, free. RESOLVED
2026-08-04.** Tape protocol 7 emits a deterministic BBO before every parent
burst, serves bounded quote history, snapshots the book at connection, and
replays it through the adapter. The three new per-instrument quantities remain
explicitly uncalibrated, so step 5b now has separate landing sites for quoted
width and effective spread rather than one conflated target.

**Step 6. Buy the paired test, 10.02 dollars.** Answers whether process
constants are contract-specific or market-specific, and whether NQ's moments
fall inside the existing cross-pair tolerances. If they do, the fingerprint is
already MNQ-ish within its own stated band and the rest of the budget buys
refinement rather than correction.

**Step 7. Spend the rest with evidence.** Basket B, or whatever steps 4 and 5
have reshaped it into, plus the GC probe if the Kraken cross-grid work in
[7.1](#71-zero_change_frac-is-not-an-instrument-constant) leaves a
cross-asset-class question open.

---

### 11.1 The spread experiment contract

Written BEFORE the data is parsed, deliberately. Every estimator decision below
is one that a file discovery could otherwise silently redefine after synthetic
results already exist, at which point the comparison stops meaning anything.

#### Parent inference

The synthetic tape knows its own burst structure (`burst.remaining`). Real data
does not, so it must infer the grouping - and the inference rule is part of the
estimator, not an implementation detail.

- A parent groups CONTIGUOUS rows sharing both timestamp and aggressor side.
- Grouping NEVER combines non-contiguous rows that happen to share a timestamp.
  Two separate events at the same millisecond are two events.
- First and last child follow that inferred ordering.
- TIMESTAMP RESOLUTION and the group-size distribution are reported, not
  assumed. A millisecond archive merges events a microsecond tape separates, so
  resolution is part of the contract and a resolution mismatch between two
  corpora invalidates their comparison.

#### Estimators

- `roll_first_child`, `roll_last_child` and `roll_all_prints` stay SEPARATE. No
  blending, no "best" one.
- Roll is UNAVAILABLE, never zero, when the covariance is non-negative.
- A covariance PAIR contributes `dP_t * dP_{t-1}` and is assigned to a stratum
  by the LATER of its two changes, `dP_t`, so a stratum boundary cannot claim a
  pair that straddles it.
- **The volatility that assigns it must be computed from information available
  strictly BEFORE `dP_t`.** It may include `dP_{t-1}`, matching what a
  conditional volatility process would know. Including `dP_t` stratifies on one
  of the two terms being multiplied and mechanically amplifies the very
  relationship the matrix measures. The first implementation of this harness got
  it wrong by indexing the pair's third PRICE, whose trailing window contains
  `dP_t`; correcting it moved the extreme cells from 3.1x and 16.9x to 2.5x and
  12.1x, so the contamination was real and material but not the whole effect.
- Every cell reports both its sample count and its covariance-pair count. These
  differ, and quoting one for the other overstates the evidence.
- Sparse cells FAIL CLOSED. An unstable estimate printed without comment is
  worse than a hole, because it will be read.

#### Quote alignment

- Join the LATEST quote whose transaction timestamp is no later than the trade
  timestamp. Never the nearest quote in either direction: that permits
  lookahead, and lookahead in a spread study manufactures the result.
- Record QUOTE AGE for every match.
- An equal-timestamp join is SEQUENCING-AMBIGUOUS when the timestamp resolution
  cannot order the two streams, and is labelled as such rather than treated as
  a perfect match.
- Locked and crossed books, negative effective spreads, missing quotes,
  duplicate update ids and out-of-order rows are REPORTED, never silently
  cleaned. Each is evidence about the join; removing them removes the evidence.
- Quote-age strata are chosen only AFTER inspecting timestamp resolution.
  Absolute age summaries and zero-age frequency are preserved first, because
  otherwise millisecond ties masquerade as perfect synchronization.

#### Per-trade quantities

```
mid              = (bid + ask) / 2
quoted_spread    = ask - bid
aggressor_sign   = +1 buyer initiated, -1 seller initiated
effective_spread = 2 * aggressor_sign * (trade_price - mid)
```

Negative effective spreads are NOT clamped. They are evidence of a stale quote,
sequencing ambiguity or price improvement, and clamping them erases exactly the
diagnostic that says the join is wrong.

#### Volatility stratification, and the two axes

One distinction must stay explicit: realized volatility is UNSUITABLE as the
generator's spread-response input - it would couple the spread to the same
estimator the fill band already uses - but it is entirely APPROPRIATE as an
analysis stratum. Real data has no GARCH state, so the comparison needs an
observable measure both corpora can carry.

Hence two axes, never conflated:

| Axis | Available on | Meaning |
|---|---|---|
| `model_return_scale_stratum` | generator only | the conditional return scale, model state |
| `observable_trailing_vol_stratum` | both | trailing realized volatility, computed identically |

The experiment then measures how well the observable axis RECOVERS the
model-state ordering, which is a result rather than an assumption.

Rules for the observable axis:

- Trailing observations only. No lookahead.
- The horizon is FIXED BEFORE results are examined.
- It is stated whether returns are raw-print or parent-event returns.
- Boundaries are computed GLOBALLY from the analysis corpus and then reused
  unchanged across every sampling convention, so conventions stay comparable.
- At least calm, middle, stressed and extreme strata, with the exact quantile
  boundaries recorded.

#### The synthetic quote-age dimension

The synthetic matrix has no historical quote staleness. Protocol 7 labels its
quote-age dimension `contemporaneous_model_quote`, because every parent carries
the book published immediately before it. This is not zero milliseconds: that
would imply an observed transport timestamp and invite comparison against real
zero-age joins, which are a different thing.

#### Discovery result, 2026-08-04: the coverage windows do not align

Established from the archive index before downloading anything. This answers
file contract 6 at the index level, and the answer is no.

`data/futures/um/daily/` exposes nine datasets, including both `trades` and
`bookTicker`, so the futures methodology experiment is viable in principle. But
for BTCUSDT:

| dataset | first | last | status |
|---|---|---|---|
| `bookTicker` | 2023-05-16 | **2024-03-30** | ended |
| `trades` | (not probed back) | 2026-07-31 present | current |

`BTCUSDT-bookTicker-2026-07-31.zip` returns 404 while the matching `trades` file
returns 200 at 24,391,078 bytes. A full listing from 2024-01 forward is NOT
truncated and contains nothing after 2024-03-30, so this is the end of coverage
rather than a paging artifact.

A matched pair does exist inside the overlap: `trades` for 2024-03-30 returns
200 at 12,403,562 bytes.

#### Scope constraints, accepted 2026-08-04 and recorded BEFORE downloading

2024-03-30 is accepted for a narrow methodology experiment. What it is permitted
to conclude is deliberately smaller than what it will be tempting to conclude,
so the limits are written down first:

- **Historical USD-M BTCUSDT methodology evidence only.**
- **One completed matched UTC day: 2024-03-30.**
- The date was selected by COVERAGE, being the latest available `bookTicker`
  day, and NOT by market conditions. It is not a chosen regime.
- No temporal generalization, and no transfer to CME.
- Archive age is a LIMITATION, never an input to any fit.
- Any result that motivates calibration still requires current,
  instrument-relevant quote data.

The permitted question is exactly two things: whether the three trade-derived
Roll conventions diverge from contemporaneous quoted and effective spread truth,
and whether their apparent volatility dependence survives quote-age
stratification.

It must NOT estimate a correction factor, calibrate the spread response,
represent current Binance conditions, or substitute for CME TBBO. The synthetic
result already establishes that trade-only estimation is unsuitable for fitting
the response; this experiment's narrower job is to show how that failure
presents against observed quotes in a real event stream, where quote staleness,
sequencing ambiguity and locked or crossed books exist and the synthetic tape
has none of them.

#### The decision that was taken here

The stated rule
was not to substitute another stream, cadence, endpoint or date range without
revisiting the experiment design, and using 2024-03-30 is a date-range
substitution. It is a mild one - the futures experiment validates METHODOLOGY
rather than supplying preset evidence, and a Roll-versus-truth comparison does
not obviously decay with age - but the decision belongs upstream of the parser,
not inside it.

What the choice actually is: accept a quote corpus that ends in March 2024 and
is therefore roughly two years stale relative to the `trades` data and to any
CME window under consideration, or revisit the design. Nothing in the archive
offers a more recent free quote source for this venue.

#### Retained archives, downloaded and verified 2026-08-04

Downloaded unchanged into `research/market-data/` (gitignored), with the
published checksum files, and verified before anything read them. Both pass.

| file | bytes | sha256 verified |
|---|---|---|
| `BTCUSDT-trades-2024-03-30.zip` | 12,403,562 | OK |
| `BTCUSDT-bookTicker-2024-03-30.zip` | 87,758,829 | OK |

```
https://data.binance.vision/data/futures/um/daily/trades/BTCUSDT/BTCUSDT-trades-2024-03-30.zip
https://data.binance.vision/data/futures/um/daily/trades/BTCUSDT/BTCUSDT-trades-2024-03-30.zip.CHECKSUM
https://data.binance.vision/data/futures/um/daily/bookTicker/BTCUSDT/BTCUSDT-bookTicker-2024-03-30.zip
https://data.binance.vision/data/futures/um/daily/bookTicker/BTCUSDT/BTCUSDT-bookTicker-2024-03-30.zip.CHECKSUM
```

The archives are kept as downloaded. Nothing is transformed before the six file
contracts below are recorded, so a parsing decision cannot be justified by a
file that was already reshaped to suit it.

Note the download volume is limited only by disk space, not by policy. The
one-day scope is an ANALYTICAL constraint - methodology evidence, a
coverage-selected date, no temporal generalization - and expanding it is a
design decision rather than a resource one.

#### The six file contracts, ESTABLISHED 2026-08-04

From the files, via `python3 analysis/inspect_archive.py inspect <zip>`, which
streams members from inside the archive and never extracts. Facts only; the
schema adopted below is stated separately as a decision.

| | `trades` | `bookTicker` |
|---|---|---|
| member | `BTCUSDT-trades-2024-03-30.csv` | `BTCUSDT-bookTicker-2024-03-30.csv` |
| uncompressed | 78,335,375 | 697,855,247 |
| member sha256 | `512b5d1e...caea629` | `0fe4cc97...91c85cb` |
| rows incl. header | 1,469,268 | 7,398,593 |

**1. Header.** Both present.

```
trades:     id,price,qty,quote_qty,time,is_buyer_maker
bookTicker: update_id,best_bid_price,best_bid_qty,best_ask_price,best_ask_qty,transaction_time,event_time
```

Note the futures `trades` layout is **not** the spot layout: six columns, and no
`isBestMatch`. Assuming the documented spot schema would have mis-parsed every
row.

**2. Column order and timestamp units.** Field counts are stable (6 and 7, no
variation, no malformed rows). Every timestamp is a 13-digit MILLISECOND value.
Not microseconds - the microsecond change applies to spot from 2025, and this is
2024 futures.

**3. Transaction versus publication time.** `bookTicker` carries BOTH, named and
distinct: first row `transaction_time` 1711756800002 against `event_time`
1711756800008. **`trades` carries only ONE timestamp**, so the trade side has no
transaction-versus-publication distinction available. That is a limitation of
the data, recorded rather than resolved by assumption.

**4. Ordering.** Both files are sorted. `update_id` is strictly increasing with
zero regressions; both timestamp columns are non-decreasing with zero
regressions.

**5. Duplicates and gaps, counted separately.** Zero adjacent full-row
duplicates in either file. `update_id` has 7,084,263 gaps greater than one and
no ties - ids are NOT contiguous, which is a fact rather than a defect.
Timestamp ties are extensive:

| column | adjacent ties | max tie run |
|---|---|---|
| `trades.time` | 986,862 | **349** |
| `bookTicker.transaction_time` | 2,732,402 | 87 |
| `bookTicker.event_time` | 3,122,752 | 58 |

**6. Coverage overlap**, on transaction-time candidates only:

```
trades.time                 2024-03-30T00:00:00.014Z .. 23:59:59.126Z
bookTicker.transaction_time 2024-03-30T00:00:00.002Z .. 23:59:59.980Z
```

Quotes bracket trades at both ends, so every trade in the day has an eligible
prior quote and no trade falls into `no_quote_before` for want of coverage.

#### What these facts mean for the estimator, stated as decisions

- Millisecond resolution is the binding constraint. The parent-inference rule
  groups contiguous rows sharing timestamp and side, and up to **349 trades
  share a single millisecond** here. Bursts a microsecond tape would separate
  are merged, so the synthetic and archive group-size distributions are NOT
  directly comparable and must both be reported.
- Because `trades` has one timestamp and `bookTicker` has two, equal-millisecond
  joins cannot be ordered between the streams. Those matches take the
  `sequencing_ambiguous` label the join contract already defines, and their
  share is a reported quantity rather than a nuisance to be cleaned.
- Quote age will be quantized to milliseconds, so a zero-age join means
  same-millisecond, NOT synchronized. This is exactly why the contract requires
  absolute age summaries and zero-age frequency before any age stratum is
  chosen.

#### The six file contracts, as originally specified

Not from documentation, and not from assumption - assumption is what produced
the spot-versus-derivatives error in
[5.2.1](#521-correction-free-historical-quote-truth-does-not-cover-the-shipped-presets):

1. whether files carry a header;
2. exact column order and timestamp units;
3. which book timestamp is exchange transaction time versus event or
   publication time;
4. whether rows are sorted and update ids monotonic;
5. whether duplicate update ids or timestamps occur;
6. whether daily trade and quote coverage boundaries align.

#### The output

The deliverable is not "does Roll recover spread". It is a matrix:

```
sampling convention x volatility stratum x quote-age stratum x quoted vs effective truth
```

which answers three separable questions: whether `roll_first_child` is a useful
BIASED PROXY, whether `roll_last_child` measures sweep intensity rather than
spread, and whether either relationship is stable enough to inform the CME
purchase design.

#### What the synthetic run already establishes

Protocol 7's MNQ-shaped run has a configured one-tick quoted width, an overall
first-child effective spread of 1.409 ticks against the published midpoint, and
no lookahead in the stratification:

| convention | calm | middle | stressed | extreme |
|---|---|---|---|---|
| `roll_first_child` | 0.987 | 1.324 | 1.911 | 2.633 |
| `roll_last_child` | 1.077 | 5.549 | 10.078 | 12.297 |

Stated carefully, because the tempting reading is stronger than the evidence:

- First-child's rising estimate proves that EXCLUDING THE SWEEP does not remove
  the artifact. Bounce, latent movement, grid phase, repetition and volatility
  conditioning all remain in it.
- Last-child's much steeper rise is CONSISTENT WITH additional sweep-walk
  contamination.
- The two agreeing in calm conditions and diverging under stress supports that
  explanation but does not identify sweep walking as the only difference
  between them.

The purchase consequence is direct and does not depend on resolving which
mechanism dominates: **a trade-derived proxy cannot serve as the dependent
variable in a volatility-to-spread fit, because it generates a strong slope
against a constant-spread ground truth.** That is the evidential case for buying
quotes, and it replaces the assertion this report started with.

On `roll_all_prints`, scope the claim to what was actually tested. The
UNSTRATIFIED estimator is unavailable on both synthetic grids, and that is what
the harness asserts. Its stratified behaviour is an observation on one grid, not
a property: it currently reads unavailable in all four strata, but an earlier
run appeared to produce a numeric calm estimate, and that turned out to be an
artifact of stratifying a print-denominated horizon at parent-denominated
boundaries. The corrected mapping assigns every print change the parent
volatility known before it, which also removed a collapsed 34-pair extreme cell
that had looked like scarcity and was a scale mismatch. Generalizing an
availability result across every print-layer subset is exactly the step that
mistake would have justified.

#### Order of work

1. This contract into the report. **Done.**
2. Observable-volatility stratification added to the protocol-6 harness.
   **Done.**
3. Freeze the SEMANTIC CORE, not the physical schema. The futures files may add
   transaction time, publication or event time, update ids and other provenance
   fields with no synthetic analogue, so the column set cannot be fixed before
   they are inspected. Version the complete schema after step 5, extending it
   with source-specific timing fields while the shared analytical outputs stay
   unchanged.
3b. The adversarial join fixture, BEFORE any download. **Done** -
   `analysis/asof_join.py selftest`, 24 checks. It joins TYPED RECORDS and knows
   nothing about Binance column positions, so a temporal-join defect and a
   parsing defect cannot be mistaken for one another. It pins transaction versus
   publication time as distinct values, equality accepted at the boundary, a
   future quote never selected even when far closer, newest-eligible-wins, quote
   age as `trade.time - quote.transaction_time`, tie resolution only through a
   documented update-id rule with ambiguity labelled rather than guessed, trades
   before the first quote and stale quotes failing closed into named categories,
   no borrowing across coverage boundaries, and input-order independence.

   Fixture values are chosen so every off-by-one and wrong-column choice yields
   a DIFFERENT answer: transaction and event times differ per quote, ages are
   all distinct, ids are far from timestamps, and ids are spaced with gaps so
   the resolvable-tie case is representable at all. Writing it caught a fixture
   defect of exactly that kind - consecutive ids made the resolvable tie
   impossible to express, which presented as a code failure.

   This exists because the last three defects in the synthetic harness - the
   first/last child convention, the stratification lookahead and the
   print-to-parent scale mismatch - were all found by reading rather than by any
   test. A join defect will produce a plausible number, not a failure.

4. Download one futures `trades` day and one matching `bookTicker` day.
   Deliberately narrow: ONE completed UTC day, ONE USD-M perpetual symbol,
   matching dates, original ZIPs and checksums retained, and NO transformation
   before headers, row counts, first and last timestamps, timestamp units,
   ordering, duplicates and coverage overlap are recorded.
5. Inspect and record the six file contracts.
6. Implement a fail-closed parser and the strict as-of join.
7. One-day smoke analysis before expanding coverage.

---

## 12. Corrections: things that were wrong along the way

Recorded so they are not re-derived.

- **The framing was wrong for most of this report's life.** It optimised "which
  CME windows for 125 dollars" without checking whether buying CME data was the
  binding constraint. It is not; see section 3. This is the same failure as the
  whole-book price quotes below: anchoring on a framing and optimising inside
  it.
- **2011-08 and 2016-01 are NOT free.** The vendor calculator showed 0 dollars;
  the API says 6.42 and 4.73 for NQ, 38.49 and 17.56 for four symbols. Coverage
  exists and symbology resolves cleanly in both months, so the zero was a bad
  query.
- **2025-09 and 2025-01 were selection artifacts** and do not survive. Three
  bugs produced them: raw GC roll jumps entering the feature vector, session
  features unnormalised for early closes, and a median that took the upper
  middle value on even-sized months. All fixed. The strata that survived every
  fix round are 2024-05, 2024-09, 2025-11 and 2025-04.
- **"Take the last two weeks to avoid the roll" was wrong.** Third Fridays are
  2024-09-20, 2026-03-20 and 2026-06-19, so those slices straddled expiry. Moot
  now that windows sit on `v.0`.
- **The whole-book price quotes were never the right comparison.** The owner's
  original figures (17.78 for 2025-09 and so on) match a narrow scope, not the
  entire CME catalog.
- **"No free data can supply spread" was wrong, and so was the correction to
  it.** The original claim was too broad: it is true for CME only. But the
  correction then asserted that `data.binance.vision` publishes `bookTicker`
  covering the shipped presets, and that conflated spot with derivatives. The
  documented historical SPOT datasets are `aggTrades`, `klines` and `trades`
  only; archived `bookTicker` is derivatives data, and the three shipped crypto
  presets are spot. Free historical quote truth therefore does NOT cover them -
  see [5.2.1](#521-correction-free-historical-quote-truth-does-not-cover-the-shipped-presets).
  The lesson is procedural rather than factual: the claim was flagged
  "unverified" in this very document and then relied on anyway in the
  recommended sequence. A flag is not a substitute for checking.
- **The GC probe was over-sold.** It was argued as the only way to obtain a
  second tick grid. The Kraken corpus supplies hundreds of tick grids for free.
  GC remains cheap and worth having, but as confirmation, not as evidence of
  record.
- **"The fill band is asserted, not fitted" was wrong.** `fill_band_vol_mult`
  was selected by `fills::vol_probe`'s documented PROCEED rule, with the
  provenance recorded in `config.rs` and `fill_golden.rs`. The accurate
  criticism is narrower and is stated in
  [3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book): it is calibrated to an internal
  usability window rather than to any measured market quantity. The claim also
  conflated it with the generator's trade displacement, which is a different
  mechanism in a different crate.
- **"The generator places quotes half a tick either side of the mid" was wrong
  when written, and it was the load-bearing error of the original spread
  investigation.** At that time nothing in the workspace constructed a
  `QuoteTick`; the venue published no bid, ask or top of book, and `/quotes`
  returned empty by construction. What the constant then called
  `HALF_SPREAD_TICKS` actually did was displace the
  TRADE price from the latent mid on the aggressor's side; it is now called
  `TRADE_BOUNCE_HALF_WIDTH_TICKS`. The report read the constant's name and inferred a
  mechanism from it, then built a section, a blocker row, an open item and an
  entire experiment contract on top of that inference - none of which checked
  whether a quote was ever emitted. The lesson is the same one as the
  `bookTicker` entry above, one layer deeper: a name is a claim, and an unchecked
  claim propagates further the earlier it enters the document.

  Worse, the correct fact was already written down. `notes/todo.md` states that
  the running server "serves no quotes (`/quotes` is always empty)", and
  `http.rs` says the same in a comment at the handler. Two existing records
  contradicted this report and neither was consulted, because the error entered
  as an inference about a constant rather than as a question about behavior - and
  an inference does not prompt a lookup the way a question does.
- **"There is nowhere to put a regime-varying spread" was wrong in both
  directions.** For the trade displacement it overstated the obstacle:
  `trade_bounce_ticks` was already a per-instance field on `BounceState`, read
  fresh at every event, so varying it was a mutation and not a restructuring.
  For the quote layer it badly understated the obstacle: that layer did not
  exist at all then. Protocol 7 has since added it. The two were one sentence
  because the report had not yet separated the two observables.
- **The identifiability claim for TBBO was too strong.** An earlier framing
  implied the trade displacement could be read off the quoted spread. It cannot:
  TBBO identifies the displacement only relative to the CONTEMPORANEOUS quote
  midpoint, and a trade may execute inside, at, or outside the displayed spread.
  Quoted width and effective spread are distinct observables requiring separate
  response functions even under a shared volatility state.

---

## 13. Scripts

All stdlib-only, no dependencies. The Databento SDK is not packaged for Debian
and PEP 668 blocks a system pip, so `databento_price.py` speaks the REST API
directly with `urllib`.

```
python3 analysis/inspect_cme_bars.py                  # format and integrity validation
python3 analysis/select_windows.py features           # cache per-session metrics
python3 analysis/select_windows.py select|drift|plan  # selection, era drift, stratified plan
python3 analysis/databento_price.py info              # coverage and schemas
python3 analysis/databento_price.py resolve           # symbology across eras
python3 analysis/databento_price.py price <scopes> [schemas]
python3 analysis/databento_price.py plan nqv basket   # the original basket
python3 analysis/databento_price.py plan nqv depth    # the shortened, all-TBBO variant
python3 analysis/databento_price.py plan pairv pair   # the NQ/MNQ paired test alone
python3 analysis/databento_price.py plan gcv grid     # the GC second-tick-grid probe
python3 analysis/databento_price.py cache             # what the response cache holds
python3 analysis/asof_join.py selftest                # the quote-join contract
python3 analysis/roll_estimator.py conformance        # Python half of the shared fixture
```

The Rust-side diagnostics for [3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book):

```
brokkr test -p mogwai-data a_quote_precedes_every_parent_burst
brokkr test -p mogwai-data the_trade_displacement_never_varies
brokkr test -p mogwai-data synthetic_spread_decomposition_at_protocol_seven
brokkr run mogwai -- tick-composition --out-6 analysis/tick-composition-protocol-6.json --out-7 analysis/tick-composition-protocol-7.json
python3 analysis/tick_composition_ratios.py
```

The first two are fast and run under a plain `brokkr check`; the third is
`#[ignore]`d as a report and prints the decomposition and both stratified
matrices.

The Rust half of that same fixture runs as
`brokkr test -p mogwai-data stratified_roll_matches_the_shared`. Both read
`analysis/spread_conformance.json`, which is DATA rather than code, so neither
language can quietly redefine the estimator while still passing its own tests.

The implementations stay separate on purpose: Rust tests generator truth, Python
handles archive analysis, and unifying them before the file contract is known
would add integration work without reducing parser or join risk. The fixture is
what turns "the same estimator on both corpora" from a hope into a contract. If
the archive pipeline later becomes production infrastructure, shared Rust code
may be worth it; for a one-day evidence run, a shared fixture is the cleaner
boundary.

Scopes: `book`, `parent`, `continuous`, `micros`, `targets`, `nq`, `nqv`,
`nqmnq`, `pairv`, `gcv`, `equity`. Plans: `basket`, `depth`, `pair`, `grid`.
Key read from `research/databento.key` or `DATABENTO_API_KEY`.

`analysis/cme_daily_features.json` is a regenerable cache from the `features`
phase, not source.

Every successful Databento response is cached to
`analysis/databento_cache.json`, keyed by endpoint and sorted parameters, so a
re-run is free and offline: a `plan nqv` sweep is 63 round trips and a full
`price` sweep is several hundred, at roughly a second each. Errors and
unparseable bodies are deliberately NOT cached, so a transient network failure
cannot freeze into a permanent wrong number for a window. `--refresh` bypasses
and overwrites, which is what to use when re-pricing immediately before a
purchase: vendor rates are not eternal and a cached quote is only as good as its
timestamp, which `cache` prints. The file is regenerable, not source. It cannot
contain the key, because the key appears only in error bodies and those are
never written.

**Hard invariant, verified by two independent reviews: `databento_price.py` only
ever calls `metadata.*` and `symbology.resolve`, both free. There is no
reachable path to `timeseries.get_range` or `batch.submit_job`.** It cannot
spend money or download data.

---

## 14. Open items

### 14.1 The downloader does not exist

Building it was offered and not authorised. It should use `batch.submit_job`
rather than streaming, be dry-run by default behind an explicit confirm flag,
re-price immediately before submitting and refuse above a cap, record job IDs so
a re-run never re-buys, land files in `research/market-data`, and fail closed on
partial success. Undecided: DBN (compact, needs a reader) versus CSV (larger,
directly consumable by the existing stdlib analysis code).

### 14.2 Native-unit size configuration

RESOLVED 2026-08-04. See [3.3](#33-resolved-size-is-expressed-in-native-units).

### 14.3 Should `zero_change_frac` be derived rather than fitted?

Per [7.1](#71-zero_change_frac-is-not-an-instrument-constant). A generator-design
question that affects `TAPE_PROTOCOL_VERSION`. The Kraken corpus can answer it
for free across hundreds of tick grids; a purchased second grid is confirmation
only.

### 14.4 The quote layer has landed; calibration remains separate

Item B is RESOLVED at tape protocol 7. The emitted BBO uses an exact integer-tick
width, top sizes on the instrument grid, quote-before-parent ordering, a frozen
book for compatible repeats, bounded history, and connect-time replay through
the adapter. Width, size, and displacement provenance is uncalibrated by type.
The static volatility response in item A and the joint CME TBBO fit remain the
separate calibration item identified throughout this report.

Split into two items per [3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book), whose
sequencing is fixed:

**A. The trade displacement does not vary within a run.** The per-instrument
`trade_displacement_ticks` value is static. Protocol 7 moved it into
`GeneratorScalars`; making its response dynamic is a mutation rather than a
restructuring and needs another `TAPE_PROTOCOL_VERSION` bump. The static
behavior is pinned by `the_trade_displacement_never_varies`, which drives
20,000 events on both grids, requires the high bounce regime to have been
entered, and asserts the displacement never moves - so the first commit that
makes it respond to volatility must fail this test and rewrite it deliberately.

**B. The observable market has landed. RESOLVED 2026-08-04.** Protocol 7
constructs a `QuoteTick` before every parent burst, `/quotes` serves bounded
history, each WebSocket receives an atomic current-book snapshot, and the
adapter retains and replays that book when its host activates quote delivery.
The old absence test became `a_quote_precedes_every_parent_burst`.

The landed contract deliberately does not invent an independent quote clock. It
emits a pre-trade BBO update at each parent event, in this order:

1. Advance the latent mid and the volatility state.
2. Construct bid and ask from the quote-spread response.
3. Emit the quote.
4. Generate the parent trade burst against that same quote state.
5. Keep sweep children under the same parent quote unless the model explicitly
   changes it.

That yields strict sequencing, a contemporaneous midpoint for every print,
deterministic history, and a measurable effective spread, without inventing
asynchronous quote dynamics in a first implementation. An initial quote on
subscription is also required, so a host never has to wait for the first trade to
discover the market.

**Top-of-book sizes remain an uncalibrated schema input.** They are not derived
from trade sizes. When no `[instrument.generator]` table exists, the synthesized
profile derives the placeholder from the instrument's minimum representable
size. Inside an explicit generator table, omission uses serde's
`TopOfBookSizes::default()`, which is one unit. `mes.toml` omits the key in its
own override file but does not reach that default: the effective MES preset
inherits MNQ's explicit one-unit value during preset merging. Both defaulting
routes carry `Uncalibrated` provenance, and configured values are preserved, so
TBBO evidence has an explicit landing site later.

The implementation order. The first two steps LANDED 2026-08-04 and are kept
here so the sequence reads whole, not as remaining work:

1. ~~Correct the report terminology and remove every claim that the generator
   already places quotes.~~ Done: this section, [3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book)
   and [12](#12-corrections-things-that-were-wrong-along-the-way).
2. ~~Rename and diagnose the existing trade-bounce mechanism without changing
   behavior.~~ Done: `TRADE_BOUNCE_HALF_WIDTH_TICKS`, plus the two pinning tests
   named above.
3. ~~Specify and land BBO emission ordering, history behavior, snapshot replay,
   and quote-size requirements.~~ Done at protocol 7.
4. Use TBBO to measure quoted spread and effective spread SEPARATELY against the
   same causal volatility state.
5. Jointly solve quote width, trade displacement, bounce transitions, repetition
   and traversal.
6. Land A's dynamic calibration separately; B's structural layer is already
   present and must not be made conditional on that later fit.

Related but separate: `fill_band_vol_mult` is calibrated to an internal
usability window rather than to measured spread
([3.5](#35-resolved-the-venue-publishes-an-observable-top-of-book)). Deciding whether it SHOULD be anchored
to a measured quantity is a design question that also precedes the purchase.

### 14.5 The two session mechanisms may double-count

`SessionProfile` near-zero shares and `SessionCalendar` hard windows both
express closure, and both run. Whether they compose or double-count was not
determined. See [2.6](#26-the-two-session-mechanisms).

### 14.6 Unverified claims in this document

- The archived USD-M futures `bookTicker` COLUMN LAYOUT. The spot `trades`
  schema is now confirmed against vendor documentation, and the spot-versus-
  derivatives coverage question is resolved in
  [5.2.1](#521-correction-free-historical-quote-truth-does-not-cover-the-shipped-presets),
  but the futures quote layout is still assumed. It must be read off one
  downloaded daily file before a parser is written against it, because assuming
  a layout is exactly the error that section documents.

### 14.7 GC gap features are not fully comparable

The other three archives are back-adjusted and GC is raw. The roll-trim removes
the single largest squared return per session, which handles `rv`, but `gap`
still sees adjustment boundaries.

### 14.8 Provenance obligations

Refitting anything bumps `TAPE_PROTOCOL_VERSION`. A preset's provenance must
record which windows were bought, on which symbol (`v.0`, not `c.0`), and how
the strata were chosen, since that choice is part of the fit.

The MNQ preset's provenance map is currently all `kind = "declared"`, which is
correct today: nothing in it is fitted, and it says so. The measure of this
whole project succeeding is how many of those entries become
`kind = "fitted"` with a named corpus and window, in the manner of
`btcusdt.toml`'s one fitted entry. That is a better completion criterion than
any dollar figure, and it should be the thing tracked.
