# The count-curve measurement: a preregistration

FROZEN 2026-08-11, signed by codex session
019fefe4-b680-7e70-8a8e-9df36e0beecf over three rounds: the substance,
then six specification closures (the exact decomposition, the frozen
resampling algorithm, the uncertainty output, the input binding and
backcheck, null and population handling, and the narrowed covariance
claim), then three literal ones (both tolerances as numbers, the standard
error formula, inputs identified rather than merely recorded). Nothing
about it may change after its first inspection - that is the whole point
of writing it first.

`notes/`-class: transient, no truth guarantee, nothing durable cites it.

## Why this exists

Protocol 12b closed on `no-arrival-admissible-candidate-in-frozen-search-space`
and its postmortem produced a diagnosis nobody had measured: the generated
arrival process carries its clustering at the WRONG SCALE.

```text
generated/observed Fano        1 s   3.545      5 s   1.277      60 s  0.253
absolute Fano, hour 19    obs 23.02 gen 35.91          obs 414.10 gen 40.49
Fano growth 1 s to 60 s   observed 12.9x to 32.4x   generated 1.07x to 1.53x
```

The generated Fano curve is FLAT. The observed one compounds by more than
an order of magnitude. Every gate in 12a and 12b judged a single horizon -
A3 at one second, the ladder at sixty - so no statistic anywhere could see
a curve that is level when it should climb.

The owner goal is a plausible MNQ tape AT ALL INTERVALS, 15 s and 1 min
named explicitly. A criterion for that cannot be a point statistic. But the
criterion is NOT frozen here: this document freezes only the MEASUREMENT,
because the count windows currently measured are 1, 5 and 60 s and a curve
through three points cannot locate where covariance accumulates.

## What is being measured, and why each piece

`COUNT_WINDOWS_S` extends to `{1, 5, 15, 60, 300}` on BOTH sides. The two
added windows are not decoration: 15 s is named in the owner goal, and 300 s
is where the observed curve should start to saturate if the correlation
scale is minutes.

The Fano curve is an INTEGRATED COVARIANCE curve, which is why extending it
localizes the defect. For second-level counts of mean `mu` and lag
covariance `gamma_k`:

```text
F(T) = ( T * gamma_0 + 2 * sum over k = 1..T-1 of (T - k) * gamma_k )
       / ( T * mu )
```

So `F` at five horizons is five partial sums of the same lag structure.

THE CLAIM THAT BUYS, STATED NARROWLY. Five points are COARSE integrated-
covariance constraints. A plateau indicates that most covariance mass has
accumulated, under an approximately stationary interpretation; it does NOT
prove `gamma_k` becomes zero at that horizon, and this document does not
claim it does. That is also why item 5's decomposition is not optional: the
pooled identity above mixes genuine within-session covariance with
session-rate heterogeneity, and only the decomposition separates them.

## The frozen procedure

1. HORIZONS `{1, 5, 15, 60, 300}` seconds, frozen now. No horizon is added,
   removed or reweighted after any inspection.
2. NESTED SCHEDULING. Every window is half-open, segment-origin aligned,
   attributed by endpoint hour, and a window crossing a UTC-hour boundary is
   excluded - the frozen 12a section 3.2 rule, unchanged, so the new windows
   are comparable with the committed ones rather than a second convention.
3. STRATA. All 23 traded hours, reported per hour. HOUR 20 IS ITS OWN
   STRATUM and is never pooled: it is the partial-session hour, 59,378
   scheduled one-second windows against 79,178 elsewhere, and it produced
   the binding incompatibility in the 12b close.
4. UNCERTAINTY, algorithm frozen and not merely its unit. The observed side
   uses THE FIRST 2,000 REPLICATES OF THE EXISTING 12a SECTION 6.1
   bootstrap - the circular five-session block resample, with its session
   ordering, its `splitmix64` seed derivation, its wrapping and its
   22-session pseudo-month, unchanged. Reusing it rather than inventing a
   second convention preserves within-session dependence AND short
   cross-session dependence, and means no new bootstrap has to be argued.
   The generated side reports per-seed values across the eight committed
   seeds.
5. THE DECOMPOSITION, defined exactly, because "within versus between" is
   not a specification. For session `s` with `n_s` windows, session mean
   `mu_s`, pooled mean `mu`, and `N` windows in total:

   ```text
   SS_W = sum over s, i of (x_si - mu_s)^2
   SS_B = sum over s of n_s * (mu_s - mu)^2
   V_W  = SS_W / N          V_B = SS_B / N
   F_W  = V_W / mu          F_B = V_B / mu
   ```

   POPULATION convention throughout, matching 12a. The identity
   `F_total = F_within + F_between` MUST hold and is CHECKED rather than
   assumed, at the frozen tolerance

   ```text
   abs(F_total - F_within - F_between) <= 1e-10 * max(1, abs(F_total))
   ```

   Computed SEPARATELY WITHIN EACH GENERATED SEED; seeds are never treated
   as sessions.

   This is the piece that keeps the measurement honest: session-to-session
   rate heterogeneity would otherwise masquerade as minute-scale
   correlation and send a successor after the wrong mechanism entirely.
6. ALSO REPORTED at each horizon, so that equal second moments cannot hide
   unequal distributions: zero probability, the count mean, and EXACT
   NEAREST-RANK p99 as the sole upper quantile. p99 rather than p99.9,
   which at a 300 s window is effectively an extreme order statistic, and
   rather than adding p90, which this focused measurement does not need.
   p99 will be noisy and discrete at 300 s and the session bootstrap is
   what exposes that.
7. NULL AND POPULATION HANDLING, reused from 12a exactly: population
   variance; zero windows INCLUDED; nearest-rank quantiles over the pooled
   exact histogram; Fano null when the mean is zero; and no session or
   window dropped beyond section 3.2 eligibility.
8. OUTPUT, frozen so a later reader cannot re-cut the uncertainty. Observed:
   the original point estimate, the bootstrap standard error, and
   nearest-rank 2.5 and 97.5 percentiles. The standard error is the sample
   standard deviation of the 2,000 replicate estimates,

   ```text
   SE = sqrt( sum over b of (theta_b - theta_bar)^2 / 1999 )
   ```

   Generated: ALL EIGHT per-seed values, plus the median across seeds and
   the min/max spread. Generated seeds are NEVER pooled before Fano or a
   quantile is computed.
9. INPUT BINDING BY CONTENT IDENTITY (AMENDED 2026-08-11, see the
   amendment note below). Binding by PATH is insufficient here because the
   historical and current files are not byte-identical:

   ```text
   crates/mogwai-server/presets/mnq.toml
     historical (12a, 1e9506c)  46622ce226922d96457fcc0ea57411b63b5d7f0f
     current (bound by Stage 0) c1b352efbc35c878dd3cc75cb282fa29fde57f6a

   analysis/fingerprint.json
     historical (12a, 1e9506c)  f63d9570d5cad4b2ca6c109a439dbbc48311c122
     current (bound by Stage 0) 19238d94ab0747f86fcdd4635889964e576972db
   ```

   The PRESET difference is provenance-only. The FINGERPRINT difference is
   the separately audited `modal_tick.max` correction, 0.25 to 0.1, which
   carries the one recorded exemption from the version-bump rule on the
   argued ground that its sole consumer emits advisory diagnostics and
   never reaches the generator. Both facts are recorded; neither is assumed.

   Stage 0 binds to the EXACT CURRENT BLOBS above. The comparator is the
   per-seed 1, 5 and 60 second records in the protocol-11 artifact produced
   at `1e9506c`. Exposure, warmup and seeds 1 through 8 come from that
   artifact's `binding.generated`, which does record them. Output path is
   fixed at `analysis/out/count-curve-measurement.json`. The executing
   commit and `TAPE_PROTOCOL_VERSION` are recorded as PROVENANCE; what
   proves method continuity is Stage 0, not the commit string.

10. STAGE 0, THE STANDALONE GENERATED BACKCHECK, run and adjudicated
   BEFORE anything else is built or inspected. GENERATED SIDE ONLY - eight
   month-scale walks, about five minutes. It recomputes the 1 s, 5 s and
   60 s statistics under the bound inputs and compares against the
   committed 12a values at EXACT EQUALITY on `scheduled_windows`,
   `zero_windows` and `count_hist`, PER SEED.

   The observed side is NOT checked here; it is Stage 1 below, because it
   costs a corpus pass and cannot be part of a five-minute preflight.

   WHAT STAGE 0 DOES AND DOES NOT TEST, stated precisely because the
   temptation to overclaim it is strong: Stage 0 tests the joint
   consequence of the version and audited content changes on the frozen
   Block 2 arrival-count projection. Passing SUPPORTS but does not PROVE
   either full tape-byte assertion - parent timestamps, child structure,
   prices or sizes could all change while those count histograms stayed
   identical. Failure is potentially an owner-level contradiction of a
   recorded claim and requires the frozen causal adjudication below.

   THREE FROZEN OUTCOMES:

   ```text
   passed_exactly        proceed to the full measurement
   execution_input_mismatch
                         the run did not use the bound blobs,
                         configuration, exposure or seeds; REFUSE without
                         interpreting any output
   generated_identity_mismatch_unattributed
                         record the first divergent seed, hour, horizon,
                         field, expected and actual; STOP and return for
                         adjudication
   ```

   A MISMATCH IS NOT SELF-ATTRIBUTING, and the preregistration forbids
   guessing at its cause. A scalar comparison cannot distinguish
   measurement-implementation drift, a current-versus-historical input
   difference, another generator-path change since `1e9506c`, cross-binary
   or cross-toolchain floating-point drift (which the standing contract
   does NOT forbid), or leakage of the protocol-12 arrival-frame repair
   into the legacy path.

   The adjudication procedure, frozen: compare the historical and current
   PARENT STREAMS or tape transcripts under the bound inputs. Identical
   streams with differing Block 2 records is a MEASUREMENT defect.
   Differing streams is a legacy tape-identity regression that contradicts
   the recorded committed-preset identity claim at the observable level.
   Only an earliest-divergence or controlled code comparison may attribute
   that regression to the arrival-frame repair specifically. If attribution
   cannot be completed the outcome stays `unattributed`; protocol-11 output
   is NEVER silently regenerated to make the comparison pass.

11. STAGE 1, THE OBSERVED BACKCHECK, run during the full implementation and
   BEFORE any 15 s or 300 s result is inspected. Same exact-equality test
   on `scheduled_windows`, `zero_windows` and `count_hist` at 1, 5 and
   60 s, observed side. A mismatch is `observed_method_mismatch` and STOPS
   the measurement.

12. THE FINAL ARTIFACT RETAINS THE STAGE 0 RESULT rather than overwriting
   it, so a reader of the finished measurement can see the preflight that
   licensed it without going to another file.

## What this measurement may NOT do

- It may not propose, screen or rank a mechanism. It is a measurement.
- It may not become an acceptance criterion by default. The successor
  criterion is preregistered SEPARATELY, after this measurement is read,
  and derived from the owner goal rather than from what any candidate
  passes - the section 11 discipline that 12b's close turned on.
- It may not be re-cut after inspection. If it proves to need a sixth
  horizon or a different stratum, that is a new preregistration, dated, and
  the reason is recorded.

## What a successor criterion will eventually have to constrain

Recorded here so the measurement is designed to feed it, NOT frozen:
absolute Fano at each horizon; growth between adjacent horizons as log
slopes; zero probability and an upper count quantile; fine-scale return
centre and normalized tail shape as SEPARATE quantities; and simultaneous
behavior across hours with hour 20 reported apart.

## What is deliberately still open

The fine-interval return SHAPE defect - `rms_scale / robust_scale` running
1.411 at 1 s, decaying to 1.016 at 300 s - is established but NOT
established as independent of the arrival process. Too many empty windows
and overly concentrated bursts could produce it without any mark-level
defect at all. This measurement does not settle that, and no successor may
assume it either way.

## Amendment, 2026-08-11: the input binding was invalid as signed

Recorded rather than quietly fixed, because the defect is instructive: the
signed text bound generator configuration and fingerprint identity to
"the ones recorded" in `analysis/mnq-measure-12a.json`, and that artifact
records NEITHER. Its binding block is `file_hashes`, `generated` (seeds,
warmup, window), `harness_tree_commit`, `job_id`,
`preflight_artifact_hash`, `subcontract_hash` and `tape_protocol_version`.
An implementer would have had to substitute the current configuration
silently - the exact failure the input binding existed to prevent. The
implementing session refused to proceed and was right to.

Worse, and missed by the author entirely: that artifact records
`tape_protocol_version` 11 while the current binary is 12, so the
generated half of the backcheck compares across a protocol bump. That is
not fatal - the 12b record asserts the bump moved no committed preset's
tape, and `presets/mnq.toml` declares no arrival seam - but it is an
ASSERTION the signed text would have had an implementer bet the whole
measurement on without saying so.

Item 9 is rewritten to bind by CONTENT IDENTITY, item 10 becomes STAGE 0
with three frozen outcomes and a frozen adjudication procedure, item 11
separates the observed backcheck as STAGE 1 because it costs a corpus pass
and cannot sit inside a five-minute preflight, and an earlier proposal to
force a mismatch into one of two causes is REJECTED: at least five causes
are available and a scalar comparison distinguishes none of them.

Amendment signed by codex session
019fefe4-b680-7e70-8a8e-9df36e0beecf, 2026-08-11.

## Cost, before it is authorized

The observed side is one streaming pass over the delivered July MNQ TBBO
corpus, 873 MB on host `bygg`; the 12a Brick M record prices the observed
pass at about 334 s. The generated side is eight month-scale walks at about
25 s each. Both are within an hour, but the owner approves anything at that
scale before it runs.
