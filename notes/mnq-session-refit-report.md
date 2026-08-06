# MNQ session refit report: protocol 11

2026-08-06. The fit report for the protocol-11 landing, written from
`analysis/mnq-fit.json` (the binding artifact) and
`notes/protocol-11-session-repair-spec.md` (the frozen contract and its
RESULT records). This is a `notes/`-class document: transient, no truth
guarantee, nothing durable may cite it.

## What was wrong

The owner's chart review of the protocol-10 tape found Asia and London
roughly 5x too quiet at bar scale while NY looked plausible. Diagnosis,
confirmed numerically and adversarially reviewed: `vol_hour` was fitted
at protocol 8 as a PER-MINUTE quantity (RMS of adjacent NQ one-minute
close returns) but applied PER PARENT EVENT, so the generated
minute-level peak-to-trough compounded the vol curve's 3.4x with the
arrival curve's 27.5x through the sqrt-of-count aggregation - roughly
17x generated against the 3.4x the fit had measured. `intensity_hour`
was independently a volume proxy overstating the true arrival swing.

## What landed

- `session.intensity_hour`: FITTED from July inferred-parent counts,
  conditional on the frozen `dow_weight` (the closed-form estimator that
  avoids double-counting day concentration through the runtime's
  hour-times-day product). 14.5x peak-to-trough. Generated marginal
  reproduction: worst hour 0.63 percent against a 10 percent tolerance.
- `session.vol_hour`: FITTED as the per-parent robust scale
  (one-maximum-trimmed mean absolute adjacent-parent quote-mid log
  return per session-hour cell, nearest-rank median across 22 sessions).
  Nearly flat and slightly INVERTED: overnight 0.99-1.16, cash session
  0.81-0.94. Worst generated hour 6.6 percent against the 0.8-1.25 band.
- `generator.vol_scalar`: DECLARED best candidate 1.357e-5, re-solved
  under the fitted arrays (up from the protocol-10 candidate 8.70e-6 as
  the flattened curve redistributes scale). Its pooled quote-mid RMS
  gate passes; the per-seed minute-range envelope still fails, so the
  provenance stays declared and the miss is protocol-12 evidence.
- `session.dow_weight`: untouched, NQ-bar provenance retained.
- `TAPE_PROTOCOL_VERSION` 10 -> 11; four composition ceilings resized
  under the standing policy (ratios 1.0004-1.13; see
  `reference/performance.md`).

## What did not land, deliberately

The first fit under the unamended rule landed nothing: the hourly
wall-time contour missed its band at the reversion-heavy hours (300 s at
UTC 19, 20, 23; 60 s marginally at 20) while arrival and per-parent
scale matched to fractions of a percent at those same hours. Both
reviewers classified the residual as an hour-dependent
serial-dependence or aggregation-law mismatch - mechanism unselected
among the protocol-12 candidates - and re-signed a narrow amendment
making the hourly wall-time verdicts recorded diagnostics (the pooled
wall-time gates still gate the landing). Protocol 12 inherits the
hourly 60 s and 300 s bands as HARD successor gates beside the
minute-range envelope, plus the top-32 worst-minute location records
the summary now carries per seed. The measured-failure artifact under
the unamended rule is preserved in git history as its own evidence
commit.

## Effect to eyeball

Regenerate the July chart from the landed preset: overnight bars now
carry realistic amplitude (the per-parent scale no longer collapses
Asia and London by the square root of the arrival deficit), the cash
session loses the excess per-event scale the old curve gave it, and the
known limitations remain - no reopen gaps, occasional implausibly tall
minutes (the unfixed tail), and the hourly wall-time contour residuals
at the close-adjacent hours.
