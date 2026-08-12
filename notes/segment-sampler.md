# The session-segment sampler: the standing direction

Written 2026-08-12, the owner's ruling from the day the charts were
finally looked at. `notes/`-class, but this one carries the DIRECTION
until it lands; read it before touching tape work.

## The product statement, in the owner's terms

The tape is a COMPOSABLE SESSION-SEGMENT SAMPLER, not a month
imitator. The owner wants, as configs of one machine:

- an endless Asia-session tape
- an endless London-session tape
- an NY session looping from 09:00 (lead-in so a strategy can
  prepare) to NY lunch
- an NY session from 10:30 to close
- a 4-year full-CME-calendar tape, all sessions
- each with feature permutations toggleable per config

## The machine

- SEGMENT LIBRARY: session slices (Asia, London, NY-morning,
  NY-afternoon, full day) cut from the 11 delivered real months
  AND/OR from the generator, behind one segment interface (the
  existing `TickSource` seam).
- COMPOSER: sequence, loop, or sample segments per seed, re-anchored
  in RETURNS SPACE at every seam - absolute price level is an
  integration constant (owner ruling), which is what makes endless
  looping seamless.
- FEATURE INJECTORS as config knobs: reopen gaps, open ignition,
  day-factor chaining, calendar, macro spikes - each on/off.
- Divergence injection (havoc) unchanged on top.

THE ONE OPEN DESIGN DECISION: real-resampled segments vs
generated-with-features. Real segments carry the within-session
texture that five fitted mechanism families failed to imitate;
bounded variety (roughly 230 real sessions, reordered and
re-anchored). Generated segments are unlimited but carry every
catalogued defect. Both fit behind the same interface; they are not
exclusive.

SLICE 1: endless-Asia from resampled real segments - the smallest
thing exercising cut, re-anchor, loop, serve - ending as a CHART for
the owner's eye. Days, not weeks. No preregistration liturgy.

## The owner's observed defect list (the first eyeball-gate criteria)

From direct comparison of generated tapes (96b1d66 and HEAD - which
look THE SAME, 201 commits apart) against real April 2026 in the same
viewer:

1. The cash open does not ignite - the generator smears the 09:30 ET
   open across its hour because the session profile is hourly.
2. REOPEN GAPS dominate, not settlement flurries: Asia frequently
   opens with literal 300-point gaps across the daily close (owner's
   example: real 2026-04-10 20:59 UTC bar vs the following 22:00
   session-open bar). The clean generated tape has NO reopen gaps at
   all.
3. Generated VOLUME looks uniform across all sessions - suspicious
   and unverified; check the generated per-hour volume profile
   against the observed one before assuming the session profile
   actually modulates what the chart shows.
4. Macro-release spikes: rarely noticed by the owner's eye; low
   priority.
5. Times in owner communication: the owner is in Norway (CET/CEST)
   and the viewer shows its own frame - always state times in UTC
   and say what they are in the viewer's frame.

## What this supersedes

The arrival-successor contract reached its terminal Tier 2 outcome
(`no_one_month_slow_confirmation_design`, recorded in
`notes/stage-m-preregistration.md` and the Tier 2 artifact). Its
named owner exits were pending when this direction arrived and are
LIKELY MOOTED by it: the segment sampler gets within-session realism
from real data rather than confirming an imitated slow component.
The Stage M evidence (count curves, DST phase, calendar structure,
the diagnosis) remains the reference for the generated-segment path
and for sequencing day factors. Nothing statistical is un-learned;
the acceptance framing changes from statistical gates to the owner's
eye plus strategy usefulness.

## The process reset this rides on

Recorded in CLAUDE.md the same day: Codex scoped to correctness
review, never direction; every measurement demand names the decision
it changes; a rendered chart judged by the owner is a standing gate;
the system budget is MINUTES of owner attention at real forks. The
week that preceded this note is the case study for why.
