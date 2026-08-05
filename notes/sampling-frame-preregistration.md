# Preregistration: does a bar-derived sampling frame predict microstructure?

Written BEFORE any archive is downloaded, parsed or inspected. Every threshold,
boundary and acceptance rule below is fixed now, and changing one after seeing a
result invalidates the acceptance claim it supports. This is the same discipline
`analysis/fit_session_profile.py` carries as named constants at the top of the
file, and for the same reason: the session fit only caught the 36.45x-versus-
27.51x era trap because the threshold was fixed first.

Written against `reference/technical-implementation-spec.md`. Spawned from
`DATA-PURCHASE-REPORT.md` section 11 step 4, whose method sketch is section 7.2.

This is a `notes/`-class document: transient, no truth guarantee, nothing
durable may cite it.

---

## 1. The question, and why it gates money

Every candidate basket in `DATA-PURCHASE-REPORT.md` section 9 selects CME
windows by VOLATILITY AND VOLUME strata computed from 1-minute bars. Section 7.2
states the limitation plainly: the strata are chosen on volatility and volume
regimes, not on microstructure regimes, and the tick data itself is what reports
whether those coincided.

That assumption has never been tested. If it is false, the roughly 100 dollars
of Basket B is sampling the wrong windows, and the correct purchase is
contiguous recent months instead.

**Primary question.** Do bar-derived volatility strata produce a material,
ordered separation in the tick-level target vector, on months held out of the
stratum calibration?

**Decision borne.** A pass justifies volatility-stratified purchasing and Basket
B's selection. A failure rejects it and switches the purchase to contiguous
recent months. The fallback is retained exactly as section 9 states it, which is
what makes this test decision-bearing rather than caveat-producing.

---

## 2. Corpus, and why not Kraken

**Primary corpus: Binance SPOT BTCUSDT monthly `trades` archives.**

Kraken was the obvious candidate and was rejected on measured evidence, recorded
here so it is not re-proposed. `analysis/probe_timestamp_precision.py`, run
2026-08-05:

```
XBTUSD  81,810,187 rows  0 decimals  finest resolution 1 s
ETHUSD  53,416,611 rows  0 decimals  finest resolution 1 s
```

Whole-second stamping on every row of both pairs. The consequence is not that
Kraken is noisier, it is that Kraken cannot express half the target set:

- **Cadence dies.** A whole-second corpus cannot adjudicate sub-second arrival
  structure. The repo already established that the duration ACF collapses from
  0.1603 to 0.0012 once same-timestamp trades are treated as one arrival, which
  is why the raw-fill cadence work fitted the arrival clock against Binance.
- **Sweep structure dies worse than it is missing.** The event-grouping rule
  available without an aggressor side is "consecutive rows sharing a timestamp".
  At one-second resolution that measures trades-per-second, and trades-per-second
  is mechanically increasing in volume - which is one of the stratifying
  features. It would separate across strata for an arithmetic reason and read as
  a pass.

So on Kraken the two execution-relevant families are exactly the two that cannot
be measured, and the four that survive are the price-derived ones. A Kraken
result could REJECT the sampling frame but could never VALIDATE it, and an
asymmetric test is too weak to gate the purchase.

Binance spot is microsecond-stamped from 2025 onward, carries `isBuyerMaker` so
the primary same-timestamp-and-side grouping rule works, and is the corpus the
fingerprint's cadence targets were actually fitted on
(`analysis/probe_binance_trades.py`). Download volume is limited by disk rather
than policy.

**Secondary corpus: Kraken, as negative control and long-history diagnostic**,
for the four surviving price-and-size families only. It does NOT contribute to
the primary aggregate and CANNOT rescue a Binance failure. Its only jobs are to
show whether the four families behave consistently across two venues, and to
extend those four across a longer history than Binance offers.

**Replication corpus: Binance spot ETHUSDT.** NOT inspected, NOT parsed and NOT
measured until the BTCUSDT pass/fail decision is frozen and written into this
document. This is the guard against turning a replication check into a second
draw at the primary result.

Stated as inspection rather than download because `ETHUSDT-trades-2026-06.zip`
is ALREADY on disk from earlier work. Presence is not a violation; reading it
is. The guard binds the analyst, not the filesystem.

### 2.1 What is already on disk, and what it changes

`research/market-data/` (gitignored) already holds, from earlier work:

| file | bearing on this experiment |
|---|---|
| `BTCUSDT-trades-2026-06.zip`, 914,498,374 bytes | one of the required months; **carries no `.CHECKSUM`** |
| `ETHUSDT-trades-2026-06.zip`, 905,842,175 bytes | replication corpus, embargoed per above |
| `SOLUSDT-trades-2026-06.zip` | not in this design |
| `BTCUSDT-aggTrades-2026-06.zip` | not an input; the primary grouping rule needs raw trades |
| `BTCUSDT-1s-2026-04/05/06.zip` plus daily 1s for 2026-07 | NOT an input, see below |
| `BTCUSDT-trades-2024-03-30.zip` and its `bookTicker` pair | the spread experiment's corpus, unrelated |

Three consequences.

**The download is 18 months, not 19.** 2026-06 is already held. At roughly 900 MB
per month that is about 16 GB to fetch.

**The held BTCUSDT month is UNVERIFIED and currently fails diagnostic 4.** Unlike
the 2024-03-30 pair, which was retained with its published `.CHECKSUM` and
verified, the 2026-06 archives were downloaded without one. Under the fail-closed
rule that month cannot enter an aggregate on the strength of existing on disk.
The downloader therefore fetches the missing `.CHECKSUM` for every already-present
archive and verifies it, and treats a present-but-unverified file as absent
rather than as done. "The file is there" is not evidence about its contents,
which is the whole point of retaining checksums in the first place.

**The 1-second klines are not an input.** Brick 3 builds bars from the same ticks
the tick side reads, deliberately, so that a disagreement cannot be blamed on two
aggregation conventions. The klines do earn a secondary job: for 2026-06, the one
month where held klines and held trades overlap, tick-built 1-minute bars are
cross-checked against vendor 1-second klines aggregated to 1 minute. That is an
INDEPENDENT check on bar construction and a stronger gate for brick 3 than
self-consistency against the tick side alone.

---

## 3. Span, fixed by preflight

**CANDIDATE SPAN, decided 2026-08-05: the 19 native-microsecond months,
2025-01 through 2026-07, with the fixed 12/7 chronological split.** Metadata
established it - [3.4](#34-result-2026-08-05-the-millisecond-design-is-rejected)
measured the resolution boundary at exactly 2025-01 and rejected the
millisecond representation that older months would require.

Candidate is not confirmed. **Archive-by-archive preflight determines the
CONFIRMED usable span**, and a failed month TRUNCATES contiguity rather than
being bridged over. `MIN_HELD_OUT_MONTHS` below remains the stop condition if
diagnostics cut the held-out side short.

There is no longer any reason to seek more history. Older months exist in
quantity - 89 of them - but only in the millisecond representation the fidelity
gate rejected, and the continuous statistic of
[7.1](#71-the-continuous-redesign-preregistered) was chosen precisely so that
all seven native-resolution held-out months carry weight.

The span is otherwise the **maximal contiguous set of complete calendar months
whose archives are uniformly microsecond-stamped**, confirmed by preflight and
recorded here before any target is computed.

`PREREGISTERED_SPLIT_MONTHS = 12`. The first 12 months of the confirmed span are
the CALIBRATION span; the remainder is the HELD-OUT span. On the expected 19
months that is 2025-01..2025-12 calibration and 2026-01..2026-07 held out. The
boundary is fixed by month count, before any target separation is computed, so a
short span shortens the held-out side rather than moving the boundary to suit a
result.

`MIN_HELD_OUT_MONTHS = 5`. If preflight confirms fewer than 17 usable contiguous
months, the experiment STOPS and the span question returns here for a decision
rather than proceeding on a thin held-out set.

### 3.1 Power: the gate is more likely to bite than not

`MIN_HELD_OUT_MONTHS` protects against a short span. It does NOT protect against
the thing that actually threatens the verdict, which is that quartile boundaries
are fixed on the calibration span and held-out cell occupancy is therefore a
RANDOM quantity rather than a design quantity.

Priced by `analysis/stratum_occupancy.py` before any download, at the expected
12 calibration and 7 held-out months:

```
held-out occupancy of ONE stratum:
  exactly 0: 0.1335
  exactly 1: 0.3115
  exactly 2: 0.3115
P(calm and extreme both reach the floor) = 0.2546
P(the experiment stops on this gate)     = 0.7454

held-out months needed to clear the gate:
  80%: 14    90%: 17    95%: 20
```

**So the preregistered span has a roughly 75 percent chance of stopping without a
verdict**, and that is the OPTIMISTIC figure: it assumes each held-out month
lands in a stratum independently, while real volatility is persistent month to
month, so true occupancy is more clustered and the odds are worse.

This is recorded before spending 16 GB of download and hours of compute on a
design whose most likely outcome is "insufficient data to decide". Two honest
responses exist and the choice is a decision, not a detail:

- **Accept the odds.** Run the 19-month microsecond span, and stop if the gate
  bites. Cheap in bandwidth, likely wasteful in time.
- **Extend the span by lowering resolution uniformly.** Binance spot carries
  MILLISECOND timestamps before the 2025 microsecond change, and millisecond is
  still three orders of magnitude finer than the Kraken second that disqualified
  it. Truncating every month uniformly to milliseconds is a UNIFORM degradation,
  which is categorically different from the mid-span resolution TRANSITION that
  diagnostic 1 correctly fails closed: the group-size distribution shifts once,
  by construction, for every month equally. That unlocks the pre-2025 archive
  and with it enough held-out months to clear the gate at 90 or 95 percent.
  The costs are coarser parent grouping in F1 and F2, more download, and a
  calibration span reaching into a structurally different crypto era.

**The millisecond design is NOT approved**, and the reason is sharper than the
costs above. Uniform truncation removes the mid-span CONTRACT change but does
not remove resolution-induced CONFOUNDING. In an active month, truncation merges
more independent events than in a quiet month, because more trades fall inside
each millisecond. F1 and especially F2 would then separate mechanically with
volume - and volume is a stratifying feature. That is the Kraken failure of
section 2 in weaker form: a bias that manufactures the separation the experiment
exists to measure.

A uniform bias could be tolerable. An ACTIVITY-DEPENDENT bias cannot be, at any
magnitude that matters, because it produces a confident pass on an artifact.

### 3.2 The resolution-fidelity gate, preregistered before it is computed

Measured on ONE verified month at native microsecond resolution against the same
month uniformly truncated to milliseconds. Thresholds fixed here, before the
comparison runs.

```
MAX_PARENT_COUNT_LOSS            = 0.10   # native parents lost to merging
MAX_MULTI_NATIVE_PARENT_FRAC     = 0.10   # ms parents holding >1 native parent
MAX_SCALE_TARGET_RELATIVE_SHIFT  = 0.10   # F1/F2 targets on a positive scale
MAX_BOUNDED_TARGET_ABSOLUTE_SHIFT= 0.05   # F1/F2 targets living in [0, 1]
MAX_MERGE_RATE_DECILE_SPREAD     = 0.05   # THE decisive one
```

`MAX_MERGE_RATE_DECILE_SPREAD` is the gate that matters and is the direct
operationalization of the confounding argument. Per-minute merge rate is
`1 - ms_parents / native_parents`. Minutes are bucketed into deciles by trade
count; the gate is the difference in mean merge rate between the top and bottom
activity decile. Merging is definitionally somewhat activity-dependent, so the
question is never whether the rank correlation is nonzero - it is whether the
MAGNITUDE of the dependence is large enough to move F1 and F2 across strata.
Five percentage points across the full activity range is the line.

Scale targets: `mean_event_duration_s`, `duration_dispersion_cv2`,
`children_mean`, `levels_mean`. Bounded targets: `children_single_frac`,
`duration_acf_lag1`, `duration_acf_lag5`.

**If the gate fails**, the millisecond design is rejected and the 75 percent stop
probability is NOT accepted either. Acceptance is instead redesigned around a
CONTINUOUS held-out association statistic using every month - rank correlation
between the monthly stratum-defining feature and each target - which needs no
quartile cells and therefore cannot be starved by sparse ones. That redesign
returns here for preregistration before it is run.

**If the gate passes**, the span moves to 40 calibration plus 20 held-out months,
subject to confirmed archive coverage and disk size, which clears the occupancy
gate at 95 percent.

### 3.3 Authorized immediately, and nothing further

1. Metadata survey: archive coverage, sizes, and the ACTUAL timestamp transition
   month. The vendor documents microseconds "from 2025", which is not a month,
   and diagnostic 1 exists because that boundary must be measured.
2. Checksum backfill and verification for the held `BTCUSDT-trades-2026-06`
   archive.
3. The one-month resolution-fidelity probe above.

**No bulk download. Brick 1 remains blocked** until the gate above is computed
and ruled on.

### 3.4 RESULT, 2026-08-05: the millisecond design is REJECTED

All three authorized steps ran. Findings, in order.

**Archive coverage.** `python3 analysis/binance_archive.py index`: BTCUSDT spot
monthly `trades` runs **2017-08 through 2026-07, 108 contiguous months, no
holes, 78.5 GB total**. Disk is not the constraint.

**Resolution transition.** `python3 analysis/binance_archive.py transition`:
**2024-12 is the last millisecond month, 2025-01 the first microsecond month.**
The vendor's "from 2025" is exactly 2025-01. That gives 19 native-microsecond
months and 89 millisecond months. Sampled one day per month, so the boundary
month would need a full probe before entering a uniform-resolution span.

**Checksum backfill.** `BTCUSDT-trades-2026-06.zip` now carries its published
`.CHECKSUM` and verifies OK at 914,498,374 bytes. It is no longer
present-but-unverified and may be read.

**The fidelity gate FAILS on every check.** `analysis/resolution_fidelity.py`
over that verified month, 128,668,052 rows, 15,154,297 native parents against
12,502,352 millisecond parents:

```
FAIL  parent_count_loss            0.174996  limit 0.10
FAIL  multi_native_parent_frac     0.107981  limit 0.10
FAIL  merge_rate_decile_spread     0.184596  limit 0.05
FAIL  target_shifts                       5  limit 0
```

Five of the seven F1 and F2 targets move past their thresholds, and they move a
long way: `children_mean` 8.49 to 10.29, `mean_event_duration_s` 0.1710 to
0.2073, `duration_dispersion_cv2` 4.619 to 3.636, all roughly 21 percent. Only
the two duration ACF lags survive.

**The decisive check is the decile table, and it is monotone:**

```
d0     399 trades/min   merge rate 0.0452
d1     825                          0.0734
d2    1175                          0.0994
d3    1520                          0.1121
d4    1905                          0.1307
d5    2360                          0.1451
d6    2925                          0.1615
d7    3735                          0.1790
d8    5120                          0.1974
d9    9821                          0.2298
```

Merge rate rises monotonically across all ten activity deciles, from 4.5 percent
in the quietest to 23.0 percent in the busiest - a fivefold gradient, spread
0.185 against a 0.05 limit. This is not a uniform bias that shifts every month
equally. It is an activity-dependent bias, and since activity is what the strata
are cut on, it would manufacture separation in F1 and F2 precisely where the
experiment looks for it. A pass under this design would have been an artifact.

**Ruling.** The millisecond design is rejected. Per section 3.2 the 75 percent
stop probability is NOT accepted as the alternative either. Acceptance is
redesigned around a CONTINUOUS held-out association statistic using every month,
which needs no quartile cells and cannot be starved by sparse ones. That
redesign is preregistered in a new section before it runs; sections 5.2 and 7 as
written are superseded by it.

**Incidental validation.** The native microsecond column reproduces the
committed cadence fingerprint to five decimals - `mean_event_duration_s`
0.171041 against 0.17104, `children_mean` 8.490533 against 8.4905,
`children_single_frac` 0.558683 against 0.55868, `levels_mean` 2.247111 against
2.2471, `duration_dispersion_cv2` 4.618828 against 4.6188. `analysis/cadence.json`
names anchor BTCUSDT and source `BTCUSDT-trades-2026-06.zip` at the same
128,668,052 rows. The probe measures what the fingerprint measured, which is the
strongest available evidence that it is not quietly redefining a parent.

---

### 3.5 Step 2 complete, 2026-08-05: the corpus is on disk and verified

`python3 analysis/binance_archive.py fetch --i-have-ruled-on-the-fidelity-gate`.
Coverage was confirmed for all 19 months by HEAD before any byte was written,
totalling 17.1 GB. 18 months were fetched; 2026-06 was skipped as already
verified. Result: **19/19 verified, no partial files remaining.**

Verified three independent times over: the HEAD survey's `Content-Length`, the
inline post-download digest, and a separate `verify` pass that recomputes every
SHA-256 from disk. All three agree on every byte count.

The download path enforces its gates in code rather than by intention. Archives
stream to a `.part` name and are renamed only after their digest matches, so the
rename is the commit point and a killed transfer cannot leave something a later
step mistakes for a finished archive - the exact confusion the unchecksummed
2026-06 file had already created in milder form. A missing published
`.CHECKSUM`, a digest mismatch, a byte count disagreeing with `Content-Length`,
or an absent month all exit non-zero. `fetch` also refuses any symbol other than
BTCUSDT, so the ETHUSDT embargo survives a session boundary instead of depending
on anyone remembering it.

Nothing has been parsed or measured. Step 3 preflight is next and is what
confirms the usable contiguous span and fixes the 12/7 boundary.

### 3.6 Step 3 complete, 2026-08-05: CONFIRMED SPAN and FIXED BOUNDARY

`python3 -u analysis/preflight.py run` then `report`. One streaming traversal
per archive, 4 workers, all 19 months.

**All 19 months pass. The confirmed contiguous span is the full candidate span,
2025-01 through 2026-07, with no truncation.**

```
calibration  2025-01 .. 2025-12   (12 months)
held out     2026-01 .. 2026-07   ( 7 months)
```

**This boundary is now FIXED and ACCEPTED**, with the
[3.7](#37-deviation-diagnostic-2-is-reclassified-and-why) deviation recorded. No
target has been computed. Every month is uniformly 16-digit microsecond, field
counts are stable, zero malformed rows, zero ordering regressions.

The integrated pass was validated against the standalone instruments before the
batch ran, and 2026-06 then reproduced through the batch path as well:
128,668,052 rows and 15,154,297 timestamp-and-side parents, matching
`cadence.json` exactly by both routes.

Corpus scale, which matters for the association test: 2,190,226,814 rows across
the span, monthly rows ranging from 64,657,874 in 2025-09 to 185,158,422 in
2026-02. That is a wide spread, which is what gives the monthly `rv` score
something to rank.

### 3.7 DEVIATION: diagnostic 2 is reclassified, and why

Recorded rather than quietly passed over. Preregistration
[section 9](#9-acceptance-diagnostics-per-month) diagnostic 2 requires that "a
month whose distribution shifts discontinuously against its neighbours is a
parser or contract anomaly, not a market regime, and fails closed."

`preflight.py` does not implement that check. It CANNOT: the per-month worker
sees one archive and the comparison is inherently cross-month. `report` prints
the distributions but applies no rule to them. So diagnostic 2 currently has no
gate, and the other five diagnostics carry the whole verdict.

The honest problem is that operationalizing it NOW is compromised. The data is
already visible, and any threshold chosen after seeing it is chosen to admit
what was seen - the exact failure preregistration exists to prevent. What the
distributions show:

```
group-size p95 drifts 27 -> 48 across the span, gradually and near-monotonically
group-size p999 drifts 121 -> 152, likewise
max timestamp tie run ranges 2,722 .. 24,433 with no trend
```

The p95 and p999 drift is gradual rather than discontinuous, and its direction
is consistent across both statistics, which reads as market evolution rather
than a contract change - and a contract change would in any case have shown up
in the field count or digit width, both of which are constant. The tie-run
figure is an extreme-value statistic over ~100M rows, so its spread is expected
noise; 2025-10's 24,433 is a single burst rather than a distribution shift, and
that month's p95 and p999 sit squarely with its neighbours.

**RULING, 2026-08-05: diagnostic 2 is reclassified. It was not subsumed - it was
given the wrong AUTHORITY.**

The five gated diagnostics adjudicate machine-checkable contract violations:
field count, timestamp width, ordering, duplicates, malformed rows. Each has a
correct answer that does not depend on interpreting the market. Group-size
movement is categorically different. It cannot distinguish a parser fault or a
feed-contract change from genuine market evolution, because both produce the
same shape of signal. Granting it automatic exclusion authority through an
unspecified threshold was the error, and it is the reason no threshold was ever
written: none was writable.

So diagnostic 2 becomes a REQUIRED DESCRIPTIVE DIAGNOSTIC. It must be computed
and reported for every month - that obligation is unchanged and is met. An
apparent discontinuity STOPS PROGRESSION FOR INVESTIGATION. It carries no
automatic numeric exclusion rule, and a month is never dropped by it alone.

This is a methodological correction, not a finding that the diagnostic is
useless. Its value is making a semantic feed change VISIBLE - a shift in what a
row means, which no field count would catch. Its limitation is that visibility
alone cannot identify a cause, and only a cause justifies exclusion.

**Recorded honestly: the original fail-closed requirement was not operationalized
before observation.** The threshold did not exist when the data was first read,
so no blind test of it was ever possible on BTCUSDT. Nothing here recovers that.

**Observed values and the judgement reached**, stated so a later reader can
disagree with the reasoning rather than only with the conclusion:

- p95 group size drifts 27 to 48 across the span, gradually and near-monotonically.
- p999 drifts 121 to 152, in the same direction.
- Max timestamp tie run ranges 2,722 to 24,433 with no trend; the isolated high
  value is 2025-10.

Judgement: none of these is a discontinuity. The p95 and p999 movement is
gradual and directionally consistent across both statistics, which reads as
market evolution; a feed-contract change would be abrupt and would in any case
have moved field count or digit width, both of which are constant across all 19
months. The tie-run figure is an extreme-value statistic over roughly 100 million
rows per month, so a wide spread is expected; 2025-10 is a single burst rather
than a distribution shift, and that month's p95 and p999 sit squarely with its
neighbours. Progression is therefore not stopped.

**No threshold is invented for ETHUSDT.** It would not repair the BTCUSDT
preregistration, and imposing a number derived from Bitcoin's group-size
behaviour onto a structurally different instrument would encode an arbitrary
rule while appearing principled.

Section 9's diagnostic 2 is amended to match this ruling.

### 3.8 Brick 3 gate PASSES, 2026-08-05: bar construction is verified

`python3 -u analysis/build_bars.py crosscheck`. Tick-built 1-minute bars for
2026-06 against the held vendor 1-second klines aggregated to the same grid.

```
built 43,200 tick minutes from 128,668,052 rows
aggregated 43,200 vendor minutes
minutes only in tick bars   0
minutes only in vendor bars 0
minutes compared            43,200
open/high/low/close/volume/count mismatches: 0 / 0 / 0 / 0 / 0 / 0
```

**Exact on every field of every minute, zero permitted mismatch honoured.** No
tolerance was widened and none was needed.

What this actually validates, which is more than "the code runs": the bin
boundary convention (left-closed right-open on the UTC epoch), truncation rather
than rounding, `Decimal` parsing straight from the source strings, the trade
COUNT matching the vendor's own notion of a trade, and lossless order-independent
volume summation. An independent source agreeing to the last decimal on 43,200
minutes is far stronger evidence than self-consistency against the tick side,
which would have passed even with the binning wrong.

One convention decision was forced while reading the kline archive and is
recorded because it changes numbers: a kline second with `count == 0` is skipped
rather than folded in. Including its flat OHLC would invent prices the tape never
printed and would corrupt the minute's high and low. The exact agreement is
evidence the decision matches the vendor's own aggregation.

Target vectors are unblocked by this gate.

### 3.9 DEVIATION: F3-F6 estimator conformance is POST-OBSERVATION

Recorded in full because the standing of an evidence claim is being reduced,
and a reduced claim that is not written down reads later as a full one.

**What happened, in order.** The brick 4 equivalence gate was written to print
the seven ungated targets alongside the seven gated ones. It ran on 2026-06 and
those seven real market values were displayed and read:

```
size_round_frac  size_dispersion  return_acf_lag1
abs_acf_lag1     abs_acf_lag10    abs_acf_lag50     zero_change_frac
```

Only THEN were synthetic conformance fixtures for F3-F6 built. The batch had
also been started before those fixtures existed, and was stopped; no per-month
target artifact was opened.

**The sequencing error, named precisely.** Importing an estimator unchanged
prevents implementation DRIFT. It does not validate the estimator against a
known answer, and `build_targets.py` said as much in its `UNGATED_TARGETS`
comment - the gap was noticed and then walked past, which is worse than not
noticing it.

**Standing lost.** F3-F6 estimator conformance cannot be described as
preregistered or blind. `analysis/conformance_f3_f6.py` and its frozen
expectations validate implementation behaviour against independently derived
answers, and that is all they do. Writing fixtures now cannot recover blindness.

**Standing retained, and why.** The Spearman thresholds and acceptance rules of
[7.1](#71-the-continuous-redesign-preregistered) are UNCHANGED and remain
preregistered. Exactly one month was observed - 2026-06, in the held-out span -
and a single level reveals nothing about its rank among the other six, nor about
any correlation. No calibration-month value, no cross-month rank and no
association result was seen. The F1/F2 equivalence gate is also undamaged: it
compares against `cadence.json` values recorded long before this experiment, so
observing them proves nothing that could have been tuned to.

**The fixtures.** Eight cases, expectations derived from explicit formulas or
hand-auditable sequences and never from production output, tolerances fixed
before the estimators ran: exact for `zero_change_frac` and `size_round_frac`,
1e-9 relative for `size_dispersion`, 1e-12 relative for the ACFs. Unavailable
cases are covered - insufficient length, an empty size histogram, and a
constant series - and each asserts `None` rather than a substituted number.
Fixtures run through the real entry point as genuine ZIP archives, so no
production code was reshaped to be testable. Result: 19 checks, 0 failed.

**A second, smaller deviation inside the first.** The first freeze carried one
WRONG expectation, `abs_acf_lag1 = 0.0` on the alternating fixture, from a bad
hand-derivation: it assumed `AutoCorr`'s `var <= 0` guard fires for a constant
`|return|` series. It does not. The expectations were re-frozen after that
assertion was removed and replaced.

The correction was NOT to adopt the observed 1.0. That value is decided by
catastrophic cancellation, so pinning it would test the FPU rather than the
estimator. The ill-conditioned assertion was deleted and a well-conditioned
fixture added in its place - `|return|` alternating between ln2 and ln4, where
alternating a and b gives cov = -((a-b)/2)^2 = -var and therefore acf(1) = -1 by
algebra. It lands at 5.55e-15 relative, comfortably inside the pre-fixed 1e-12.

**Finding worth keeping, and its exact standing.** The zero-variance guard fires
only for an EXACTLY REPRESENTABLE constant. A series constant at an irrational
value leaves a tiny positive float residue as its variance, so the guard misses
and the answer comes from catastrophic cancellation.

The two halves have different standing and the distinction matters:

- The exactly-zero path IS PINNED. `constant_price` asserts
  `return_acf_lag1 == 0.0`, so that is a regression contract and a change to the
  guard breaks the fixture.
- The irrational-constant path is a RECORDED LIMITATION, not pinned. The
  ill-conditioned assertion was deleted precisely because its value is decided
  by rounding, so nothing protects that behaviour and nothing should - pinning
  it would test the FPU rather than the estimator.

`AutoCorr` MUST NOT be changed during this experiment. Its bit-exact F1/F2
lineage against `cadence.json` matters more here than the degenerate case, and
real monthly series carry positive return variance and come nowhere near it. Any
numerical-stability fix is separate future work requiring explicit
cadence-impact analysis, since `AutoCorr` also computes the F1 duration ACFs and
`duration_acf_lag1` and `duration_acf_lag5` are gated targets. Tracked in
`notes/todo.md`.

### 3.10 DEVIATION: the preflight schema contract was corrected AFTER the batch

Recorded 2026-08-05, before the association harness runs. Surfaced by a
performance regression rather than by any diagnostic: an optimization capped
`split` at five commas on the documented six-column layout, the F1/F2
equivalence gate failed, and the investigating scan
(`analysis/side_predicate_scan.py`, retained until this record landed) showed
every row of the 2026-06 archive carries SEVEN fields. The vendor's own
documentation lists the seventh trailing column, `is_best_match`; preflight's
`EXPECTED_COLUMNS` recorded six.

**What the version 2 gate actually checked, and the gap.** `stable_field_count`
proved every row shared ONE shape but never compared that shape against the
recorded schema, so `{7: rows}` passed while the file said six. The header
check tested two positions rather than the full column list, and neither
header nor layout agreement was part of `diagnostics_verdict` at all. The gate
therefore accepted stable seven-column data while documenting six columns.

**Why no rows were reread.** The version 2 artifacts retained the complete raw
facts - the header observation, the full field-count histogram, the row count,
the published archive SHA-256 - so the corrected contract could be
re-adjudicated blind, from recorded evidence only. Schema version 3 corrects
`EXPECTED_COLUMNS` to seven, requires the field-count histogram to contain
exactly the schema's width, folds header and layout agreement into the
fail-closed verdict, and adds a `migrate` mode that refuses when a required
fact is absent, preserves archive identity verbatim, and records
`migrated_from_schema_version = 2`.

**One amendment to the repair as specified.** The instruction was to require
exact normalized header equality. The recorded facts show the monthly spot
dumps carry NO header row - all nineteen artifacts record `present: false`,
and the first line of the archives themselves is data (the headered files in
`DATA-PURCHASE-REPORT.md` section 11.1 are the DAILY futures archives, a
different product). A hard header requirement would have refused all nineteen
months for want of a row the vendor never writes. The landed contract is:
header absent is the accepted vendor format; a header that IS present must
equal `EXPECTED_COLUMNS` exactly.

**Outcome and standing.** All nineteen months pass the corrected verdict,
19/19, `migrated`. The confirmed span and the calibration boundary are
unchanged: 2025-01 through 2026-07, first twelve calibrate. No target value
was read during the migration - it adjudicates layout facts only - so the
freeze-without-inspection contract is intact. The parse defect this exposed
never reached a frozen number: the equivalence gate caught it at the first
seven gated targets and the fix was proven bit-exact before the batch ran.

## 4. Unit of analysis

The **calendar month**, matching the CME window-selection machinery in
`analysis/select_windows.py`, which aggregates per-session features to monthly
medians and z-scores across months.

---

## 5. The two sides of the comparison

### 5.1 Bar side: the sampling frame under test

1. Build 1-minute bars from the SAME ticks the tick side reads. Not from Binance
   klines. Using a vendor-aggregated bar would introduce a second aggregation
   convention and make a disagreement unattributable.
2. Run the `select_windows.py` feature pipeline over those bars.
3. Aggregate to monthly medians, z-score across the CALIBRATION months only.
4. Assign months to strata.

**Named limitation, preregistered rather than discovered.** The CME feature
vector is six features: `rv`, `vol_of_vol`, `volume`, `volume_cv`,
`zero_change`, `gap`. `gap` is the absolute overnight gap into a session and is
undefined for a 24/7 market. The crypto run therefore uses **five features and
drops `gap`**, and what is validated is a five-feature variant of the CME rule.
The `full_session` early-close normalization is likewise CME-specific and is
replaced by a constant 1440 minutes per UTC day. Both differences are limitations
of the transfer, are stated in the result, and are not repaired by tuning.

The daily unit for crypto is the **UTC calendar day**, since there is no session.

### 5.2 Strata, with no lookahead

`STRATA = ("calm", "middle", "stressed", "extreme")`, assigned on the monthly
`rv` z-score.

`STRATUM_QUANTILES = (0.25, 0.50, 0.75)`.

Boundaries are computed on the **CALIBRATION span only** and then applied
UNCHANGED to the held-out months. Recomputing boundaries on the held-out span
would let the validation set define its own strata, which is the lookahead this
split exists to prevent. This mirrors the rule the spread experiment contract
already fixes in report section 11.1: boundaries computed globally once, then
reused unchanged across conventions.

### 5.3 Tick side: the target vector

Computed per month, directly from the ticks, using the estimators already in the
repo so this experiment cannot quietly redefine them:
`analysis/probe_binance_trades.py` for the event-inferred families and
`analysis/characterize.py` for the price and size families.

Parent inference uses the PRIMARY rule from `probe_binance_trades.py`:
consecutive rows sharing both timestamp and taker side. The secondary
timestamp-only rule is reported alongside it as a sensitivity, never blended
into the primary result.

---

## 6. Target families, and why the weighting is by family

Six families. **Acceptance is weighted by FAMILY, never by raw metric count** -
otherwise three absolute-return ACF lags outvote all of sweep structure merely
because that family reports more numbers.

| # | Family | Targets |
|---|---|---|
| F1 | cadence | `mean_event_duration_s`, `duration_dispersion_cv2`, `duration_acf_lag1`, `duration_acf_lag5` |
| F2 | sweep structure | `children_mean`, `children_single_frac`, `levels_mean` |
| F3 | size shape | `size_round_frac`, `size_dispersion` |
| F4 | return ACF | `return_acf_lag1` |
| F5 | absolute-return ACF | `abs_acf_lag1`, `abs_acf_lag10`, `abs_acf_lag50` |
| F6 | zero-change | `zero_change_frac` |

Every target is reported SEPARATELY alongside the aggregate. The aggregate never
replaces the per-target table. This is what prevents a persuasive aggregate from
hiding that only the volatility-derived families separated while the
execution-relevant ones did not.

---

## 7. Acceptance, preregistered

```
EFFECT_SIZE_THRESHOLD           = 0.5    # pooled standard deviations
DECIDING_STRATA                 = ("calm", "extreme")
MIN_MONTHS_PER_DECIDING_STRATUM = 2
FAMILY_MAJORITY                 = "strict majority of the family's own targets"
AGGREGATE_MAJORITY              = "strict majority of the six families"
MANDATORY_FAMILIES              = ("F1 cadence", "F2 sweep structure")
DIRECTION_MUST_REPRODUCE        = True
DECIDING_SPAN                   = "held-out"
```

> **SUPERSEDED 2026-08-05 by [section 7.1](#71-the-continuous-redesign-preregistered).**
> The quartile-cell design below is retained for the record because
> [3.4](#34-result-2026-08-05-the-millisecond-design-is-rejected) rejected the
> only span that could have populated its cells. It is not the acceptance rule.

**A target separates** when the EXTREME and CALM strata differ by at least
`EFFECT_SIZE_THRESHOLD` pooled standard deviations, computed across the months
in those two strata.

The deciding contrast is extreme versus calm, not stressed versus calm. Basket B
buys a p0 calm anchor and a p100 stress anchor - literally the two extremes - so
the contrast that decides the purchase should be the contrast the purchase makes.
An earlier draft of this section compared stressed against calm and left the
extreme stratum outside acceptance entirely, which tested neither the ordered
separation this experiment claims to measure nor the windows the money buys.

**Cell occupancy gate, fail-closed.** Both deciding strata must contain at least
`MIN_MONTHS_PER_DECIDING_STRATUM` held-out months, checked SEPARATELY for calm
and for extreme. Below that, a pooled standard deviation is undefined at one
observation and meaningless at any small count, so the run does NOT report a
pass or a fail - it STOPS and the span question returns to section 3 for a
decision. Two is weak and is a floor against undefined variance, not a claim of
adequate power. See [section 3.1](#31-power-the-gate-is-more-likely-to-bite-than-not).

**A family passes** when a strict majority of its own targets separate AND the
sign of the calm-to-stressed shift agrees between the calibration and held-out
spans for those targets. F4 and F6 are single-target families, so their majority
is that one target; this is exactly why family weighting rather than metric
counting is required.

**The experiment passes** when a strict majority of the six families pass AND
**F1 and F2 each pass individually**. The mandatory-family rule exists because a
majority aggregate alone could approve a sampling frame that predicts price
regimes while missing the execution mechanisms the purchase is meant to
calibrate. That would be the worst outcome available: a confident pass on
precisely the wrong thing.

Not every target is required to move. Some describe different mechanisms and
have no reason to track volatility.

**The final pass/fail is read from the HELD-OUT span.** The calibration span
selects strata and supplies the direction each target must reproduce; it does
not decide. Judging separation on the same months that defined the strata
demonstrates association, not that the rule generalizes to unseen CME months,
which is the only thing the purchase actually needs.

---

### 7.1 The continuous redesign, preregistered

This is the OPERATIVE acceptance rule. It replaces the quartile-cell contrast
because [3.4](#34-result-2026-08-05-the-millisecond-design-is-rejected) rejected
the millisecond span, leaving 19 microsecond months, and
[3.1](#31-power-the-gate-is-more-likely-to-bite-than-not) prices the cell design
at a 75 percent chance of stopping on those. A rank association uses every
held-out month, needs no cells, and therefore cannot be starved by sparse ones.

```
ASSOCIATION              = Spearman rho
BAR_SCORE                = monthly realized volatility
CALIBRATION_MIN_ABS_RHO  = 0.50
HELD_OUT_MIN_ABS_RHO     = 0.70
DIRECTION_MUST_REPRODUCE = True
DECIDING_SPAN            = "held-out"
```

Per target:

1. Compute Spearman rho against monthly `rv` SEPARATELY on the 12 calibration
   months and the 7 held-out months.
2. The target PASSES when calibration `|rho| >= 0.50`, held-out `|rho| >= 0.70`,
   and the two signs agree.
3. Ties take AVERAGE ranks. A target that is constant over a span has no defined
   rank correlation and returns UNAVAILABLE, which FAILS. It never returns zero -
   a zero would read as "measured no association" when the truth is "could not
   measure", and the report's own history records that silently substituting a
   number for an absence is how an estimator lies.
4. Report exact permutation p-values. There is NO p-value acceptance gate: at
   n=7 the minimum attainable two-sided exact p is 2/5040, and conventional
   filtering at that size mostly restates sample scarcity rather than measuring
   evidence. The p-values are reported so the scarcity is visible, not so it can
   be laundered into significance.

   Exactness is bounded by arithmetic and the boundary is preregistered rather
   than discovered at runtime. `EXACT_PERMUTATION_MAX_N = 10`: at or below it
   every permutation is enumerated, so the held-out span at n=7 is always exact
   at 5,040 permutations. Above it, enumeration is infeasible - the calibration
   span at n=12 would need 479,001,600 - so the p-value comes from
   `MONTE_CARLO_PERMUTATIONS = 1_000_000` draws under
   `PERMUTATION_SEED = 20260805` and is LABELLED `monte_carlo` in the output.
   The two are never reported under one name. This matters only for reportage:
   the deciding span is held out, and held out is always exact.
5. Family-majority weighting and mandatory F1/F2 passage are RETAINED exactly as
   [section 6](#6-target-families-and-why-the-weighting-is-by-family) and
   [section 7](#7-acceptance-preregistered) state them. Only the per-target
   separation criterion changes.
6. **Leave-one-month-out sensitivity** on the held-out rho: recompute it seven
   times, each omitting one month, and report the range plus whether any single
   omission reverses the sign. DIAGNOSTIC, not a gate. At n=7 one month is 14
   percent of the sample and a sign that flips on one deletion is not a finding,
   which is worth seeing even when the threshold is met.

**What this validates, and what it does not.** A rank association against a
single continuous score validates the VOLATILITY-STRATIFIED part of Basket B,
and specifically its p0 calm and p100 stress anchors, which are chosen on `rv`.
It does NOT validate the full five-feature farthest-point selection, which
picks months on the geometry of `rv`, `vol_of_vol`, `volume`, `volume_cv` and
`zero_change` jointly. Treating one continuous score as validation of the whole
feature geometry would overclaim, and the result must say so. The windows in
Basket B chosen by farthest-point rather than by volatility rank remain
unvalidated by this experiment either way it lands.

## 8. Failure rules

- **BTCUSDT fails.** Volatility-stratified purchasing is REJECTED. The purchase
  switches to contiguous recent months. ETHUSDT cannot rescue it and is not
  consulted.
- **BTCUSDT passes, ETHUSDT does not replicate.** The result is
  INSTRUMENT-SPECIFIC. Basket B is not treated as generally justified, and the
  report records the asymmetry rather than the pass.
- **Kraken disagrees with Binance on the four shared families.** A diagnostic
  finding about venue dependence. It does not change the primary verdict in
  either direction, because Kraken cannot speak to F1 or F2 at all.

---

## 9. Acceptance diagnostics, per month

Recorded for every month and checked BEFORE any target from that month enters an
aggregate:

1. **Timestamp resolution**, via `analysis/probe_timestamp_precision.py`. Any
   month that is not uniformly microsecond-stamped FAILS CLOSED and is excluded.
   A resolution TRANSITION anywhere inside the span fails that month closed and
   truncates the contiguous span at that point.
2. **Inferred-parent group-size distribution**, under both grouping rules.
   REQUIRED DESCRIPTIVE DIAGNOSTIC, amended 2026-08-05 - see
   [3.7](#37-deviation-diagnostic-2-is-reclassified-and-why). It must be computed
   and reported for every month. An apparent discontinuity against neighbouring
   months STOPS PROGRESSION FOR INVESTIGATION; it carries NO automatic numeric
   exclusion rule and never drops a month by itself. The original text made this
   fail closed against a threshold that was never written, because group-size
   movement cannot separate a parser fault from market evolution and no such
   threshold is writable. Its real value is making a semantic feed change
   visible; identifying the cause is a human step.
3. **The six file contracts** from report section 11.1, per archive, via
   `analysis/inspect_archive.py`: header presence, column order and timestamp
   units, transaction versus event time, ordering, duplicates, coverage
   boundaries. The spot monthly layout is NOT assumed from the documented spot
   schema - report section 11.1 records that the futures files turned out to
   carry six columns rather than the documented seven, and assuming the
   documented layout would have mis-parsed every row.
4. **Checksum verification** against the published `.CHECKSUM` before anything
   reads the archive. An archive already on disk WITHOUT a retained checksum is
   treated as absent until its checksum is fetched and verified; see
   [2.1](#21-what-is-already-on-disk-and-what-it-changes). Presence is not
   verification.

A month failing any diagnostic is excluded and NAMED in the result. Exclusions
are never silent, and an excluded month inside the span truncates contiguity
rather than being skipped over.

---

## 10. Bricks

Each names its gate. `analysis/` is stdlib-only, no dependencies.

1. **`analysis/fetch_binance_months.py`** - a fail-closed downloader for spot
   monthly archives over `urllib`, mirroring the discipline used for the
   2024-03-30 futures pair: original ZIPs and `.CHECKSUM` files retained
   unchanged in `research/market-data/` (gitignored), verified before anything
   reads them, no transformation before the contracts are recorded, and a re-run
   never re-downloads a verified file. It backfills the missing `.CHECKSUM` for
   archives already on disk and verifies them rather than assuming them good.
   *Gate:* checksum verification passes for every retained archive; a
   deliberately corrupted fixture is refused; a present-but-unverified archive is
   reported as unverified rather than skipped as done.
2. **Preflight** - resolution, contracts and group-size distributions per month;
   emits the confirmed span and the calibration/held-out boundary.
   *Gate:* `python3 analysis/probe_timestamp_precision.py` and
   `python3 analysis/inspect_archive.py inspect <zip>` per archive. The span is
   written into section 3 of this document before section 5 runs.
3. **Bar construction plus the five-feature pipeline** over the same ticks.
   *Gate, part one:* bars rebuilt from a single month reproduce that month's
   total volume and trade count exactly from the tick side.
   *Gate, part two:* the 2026-06 tick-built 1-minute bars agree with the held
   vendor 1-second klines aggregated to one minute. An independent source rather
   than a self-consistency check, which is what makes it worth having.

   "Agree" is preregistered, because a tolerance chosen after seeing the
   disagreement is not a gate:

   | dimension | rule |
   |---|---|
   | binning | 1-minute bins on the UTC epoch, left-closed right-open, `[t, t+60)`, keyed by bin OPEN time |
   | source timestamp | the trade `time` column, truncated to the bin; no rounding |
   | fields compared | `open`, `high`, `low`, `close`, `volume`, `trade_count` |
   | numeric type | `decimal.Decimal` parsed from the source strings, never float |
   | tolerance | EXACT on every field |
   | empty minutes | a minute absent from the klines must be a zero-trade minute on the tick side, and the reverse |
   | permitted mismatch rate | ZERO |

   Exactness is affordable specifically because `Decimal` summation over the
   decimal-string quantities is order-independent and lossless, so the usual
   reason to allow a floating-point tolerance does not apply. OHLC are selected
   values rather than sums and cannot drift at all.

   If the two sides disagree, that is a FINDING about a convention difference -
   bin boundary, self-trade handling, or which trades the vendor counts - to be
   identified and recorded, never a tolerance to widen until it passes. A
   convention difference discovered here would also mean the CME bar archives
   may carry one, which is worth knowing before the strata are trusted.
4. **Per-month target vector**, reusing the existing estimators.
   *Gate:* the full-file target vector recomputed over a single month matches
   the existing whole-corpus estimator run over that month's rows.
5. **Separation, split and the acceptance rule.**
   *Gate:* a synthetic fixture with a known planted separation is recovered, and
   a fixture with no separation is correctly failed. Written BEFORE the real
   result, so a permissive rule cannot be discovered by running it.
6. **Result into `DATA-PURCHASE-REPORT.md`** section 7.2 and section 11 step 4,
   with the verdict and the fallback decision.

Brick 5's fixture is the one that matters most. The report records three
separate occasions where a defect produced a plausible number rather than a
failure, and an acceptance rule is exactly the kind of code that fails silently
by being too permissive.

---

## 11. Stopping rule, and what is out of scope

- No CME data is bought on the strength of this result alone. A pass unblocks
  step 6 of report section 11, the 10.02 dollar paired NQ/MNQ test, which is
  still the next purchase.
- This experiment does not fit anything. It produces a verdict about a selection
  rule. No generator constant, preset value or fingerprint entry changes as a
  result, and `TAPE_PROTOCOL_VERSION` is untouched.
- It does not transfer a crypto microstructure result to CME. What transfers is
  the METHOD question - whether bar-derived volatility strata track
  microstructure at all - on the only corpus that can answer it. That the answer
  comes from crypto is a limitation, stated in the result, and is the same
  limitation the fingerprint already carries.
- ETHUSDT stays UNINSPECTED and UNMEASURED until the BTCUSDT verdict is frozen
  here. Its 2026-06 archive is already on disk, so the embargo is on reading it,
  never on its presence; see [2.1](#21-what-is-already-on-disk-and-what-it-changes).
