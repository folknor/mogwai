# Protocol landings 8, 10, 11: the consolidated record

Written 2026-08-09, consolidating five retired documents:
`mnq-session-fit.md`, `mnq-tbbo-fit-spec.md`,
`mnq-generator-successor-spec.md`, `protocol-11-session-repair-spec.md`
and `mnq-session-refit-report.md`. Their full text lives in git history;
this page carries only what still bears weight - the evidence behind
values currently shipping, and the obligations later work inherits.
Where a frozen document (the 12a spec) cites one of the retired files,
the citation resolves to git history.

This is a `notes/`-class document: transient, no truth guarantee,
nothing durable may cite it.

The binding chain for protocol 12b does NOT depend on this file: the
hourly wall-time hard gates protocol 11 bequeathed are restated in
`notes/protocol-12a-measurement-spec.md` section 1.2, which is the
contract of record.

## Protocol 8: the session profile, fitted from NQ bars (2026-08-04)

`intensity_hour`, `vol_hour` and `dow_weight` fitted from the
`nq-1m_bk.zip` 1-minute archive against the CORRECTED CME calendar
(17:00 -> 16:00 Central, 15:15-15:30 halt, settlement minute 900; the
prior calendar was the same schedule rendered one hour forward and
described no session CME publishes). Estimator: multiplicative Poisson
rate with a calendar exposure offset - exposure MUST come from the
calendar because the archive omits zero-volume minutes, and deriving it
from row presence would compress the very peak-to-trough the fit
measures. Era stability failed preregistered (22.83 percent divergent
exposure vs a 5 percent allowance; peak-to-trough fell 117.55x ->
37.99x -> 27.51x across eras), so the RECENT era 2020-2026 was selected
mechanically. Headline: 27.51x against the crypto curve's 1.78x.

SUPERSEDED IN PART: protocol 11 refit `intensity_hour` and `vol_hour`
from the July MNQ TBBO month. What still descends from this fit is
`session.dow_weight` alone - `[1.5179, 0.9080, 0.9865, 1.0157, 1.0535,
1.0225, 1.0000]`, Saturday conventional at zero exposure, Sunday on an
order of magnitude less exposure than any weekday. The caveat prose
lives above `[instrument.session]` in `presets/mnq.toml`; the fitter
survives as `mogwai session-profile preflight|fit` with its
preregistered constants in `mogwai-lab/src/session_profile.rs`.

## Protocol 10: the July TBBO fit and its successor (2026-08-05/06)

Corpus: the delivered July 2026 MNQ TBBO month, job
GLBX-20260805-HAPEWPABKG, 22 usable sessions, 35,187,061 rows.

The FIRST fit stopped its own landing: the cadence family failed
representability wholesale. Mechanism, confirmed to 0.02 percent: at
observed `children_mean` 1.1711 the quiet-state multiplier draws an
impossible sub-one mean that `SweepShape` clamps to one, breaking the
mean-preserving identity - realized mean inflates to ~1.44 at ANY
configured value. July MNQ is a nearly-single-child tape (90.5 percent
one-print parents) the crypto-fitted conditioning could not express.

The successor landed floor-aware child conditioning (branch selected
once per instrument on the base mean; crypto presets byte-identical
through it), a per-instrument `size_log_sigma` (fitted 0.9333 with
median 1.097264, landed DECLARED - generated p99 10 vs observed 8),
minute-range envelope gates from preregistered session-block resampling,
and the `gen --type trace` forensic instrument (`VolTrace`,
observation-only, tape byte-identical). Landed as protocol 10 at
`3a48f32` plus an evidence child commit; the landing_set was the eight
cadence, quote and anchor targets. Quote family fitted clean: width
mode 2 ticks, top sizes 3x3, displacement scalar 0.5161, start price
28284.00. `vol_scalar` landed declared (8.701e-6): the per-seed
minute-range envelope failed around it - per-seed maxima to 4,333 ticks
against the real month's 968 - which is the t(4)/GARCH cluster-tail
evidence protocol 12 inherited. The frozen trace of the former
420.75-point minute's window found ZERO clamp hits on all three rails:
the tail is an unconstrained volatility-cluster phenomenon, not a rail
artifact. Composition ceilings were resized (CHECKPOINT_K 16,777,216,
SWEEP_DRAIN_BUDGET 5,799,000,000, warmup reach 667,299,000,000,
fanout_depth 4,194,304; derivation in `reference/performance.md`).

Reviewed gate exception, still cited by `notes/todo.md`: during the
landing loop `tape_lateness_under_acceleration` ran red and paired
measurement established the failure as ENVIRONMENTAL - candidate and
parent indistinguishable under identical load (release N=5 each, one
pass apiece) while the canonical oracle proved crypto frames
byte-identical. The 50 ms release threshold stays authoritative and
unrelaxed; the debug-lane mismatch became the todo item.

## Protocol 11: the session calibration repair (2026-08-06)

The defect: `vol_hour` was fitted at protocol 8 as a PER-MINUTE
quantity and applied PER PARENT EVENT, so generated minute-level
volatility compounded the vol curve with sqrt of the 27.5x arrival
swing - Asia and London roughly 5x too quiet at bar scale (the owner's
chart finding). `intensity_hour` was independently a volume proxy
overstating the true arrival swing.

The refit, from the same July TBBO month: `intensity_hour` from
inferred-parent counts, solved CONDITIONALLY on the frozen `dow_weight`
(a marginal normalization would double-count day concentration) -
14.5x peak-to-trough, generated worst hour 0.63 percent. `vol_hour` as
the per-parent robust scale (one-maximum-trimmed mean absolute
adjacent-parent quote-mid log return per session-hour cell, nearest-rank
median across sessions) - nearly FLAT and slightly inverted (overnight
0.99-1.16, cash 0.81-0.94): the per-minute proxy's 3.4x swing was
almost entirely arrival-density double-counting. `vol_scalar` re-solved
to 1.357e-5, landed declared-best-candidate (pooled RMS passes, the
minute-range envelope still fails - protocol 12's standing evidence).
`TAPE_PROTOCOL_VERSION` 11.

The Brick V amendment, whose obligations outlived the spec: the first
fit under the unamended rule landed NOTHING - the hourly 60 s / 300 s
wall-time contour failed at the reversion-heavy hours (300 s at UTC 19,
20, 23; 60 s marginally at 20) while arrival and per-parent scale
matched to fractions of a percent at those same hours. Both reviewers
reclassified the hourly contour as a recorded DIAGNOSTIC for the
protocol-11 landing and a HARD GATE for the successor; the
measured-failure artifact is preserved in git history as its own
evidence commit. That inheritance is binding through 12a section 1.2.
Composition: three ceilings resized, and the mechanically proposed
fanout resize received a reviewed exception - the proposed capacity
deterministically breaks the accept-before-fill serving invariant, the
open investigation `notes/todo.md` carries.
