# Corpus formats

What the vendor feeds actually contain, and where reading them plainly gets the
wrong answer. Every entry here cost either money or days to learn, and every one
of them is true independent of what any generator does with the data.

Durable, so the must-be-true rule applies: an entry that stops matching a vendor's
format is a defect to fix here, not a historical note to leave standing.

## Databento DBN, CME

- The side alphabet is `B` = buy, `A` = sell, `N` = none
  (`research/dbn/rust/dbn/src/enums.rs`). Reading it as B/S silently classifies
  every sell as unknown and manufactures a failure that looks like a data problem.
- Use `v.0` (volume-ranked continuous), never `c.0` (calendar-ranked). The two
  disagree around every roll, and the disagreement is largest exactly where
  activity is highest.
- Date-only bounds are interpreted UTC, which clips the CME session boundary at
  16:00 Central. Bounds must be explicit UTC instants with the daylight offset
  resolved for the month in question, or the first and last hour of each session
  are silently absent.
- Delivered CME csv echoes the continuous label (`NQ.v.0`) in the symbol column,
  so a guard that reads the symbol to detect a minority contract is blind there.
  `instrument_id` is the sharper witness.

## CME session calendar

The corrected calendar, established after an earlier one described a session CME
does not publish: the session closes 16:00 Central, not 17:00; the settlement
minute is 900.

The lab's session frame (`mogwai_lab::session`) still carves a 15:15 to 15:30
halt out of every session and asserts it is exactly fifteen minutes long. The
exchange's own `status` feed shows no such halt: for ES on 2026-08-18 it records
trading from 17:00, closed at 16:00, pre-open quoting at 16:45 and the
no-cancel period at 16:59:30, and nothing between; real MNQ prints in every one
of those fifteen minutes. The venue's MNQ preset dropped the halt at tape
protocol 31. The lab frame is owed the same correction; until it lands, the
lab's `overnight` and `post_halt` segments meet at a boundary the exchange
does not have, and fifteen real minutes a session are excluded from every
lab measurement.

Two consequences that bite anything sampling real sessions:

- A window overlapping the lab frame's 15:15 halt carries the halt's hole.
  Nothing downstream can detect it once the window is cut, so the refusal
  belongs at the window table.
- Civil-day arithmetic admits every day including weekends and holidays. What
  removes them is that they collect no prints, which means emptiness is doing a
  calendar's job. That fails on thin sessions: 2026-04-03 ny-morning is Good
  Friday and carries 4,408 ticks against a typical day's 400,000. Non-empty, so
  invisible to an empty-segment rule, and sampled uniformly it contributes a
  thirty-minute stub as readily as a full session. A median-fraction threshold is
  what catches it.

## Binance

- Monthly spot trades are seven columns and carry no header row. The daily
  futures archives are six columns and do carry one. Assuming either schema for
  the other mis-parses every row.
- Millisecond timestamps destroy parent inference in an activity-dependent way.
  Measured over BTCUSDT 2026-06 (128,668,052 rows): 15,154,297 native parents
  collapse to 12,502,352 at millisecond resolution, a 17.5 percent loss. The loss
  is not uniform - merge rate climbs monotonically from 4.5 percent in the
  quietest activity decile to 23.0 percent in the busiest. Any analysis that
  strata on activity and reads millisecond timestamps is reading a gradient it
  created. Five of seven cadence and sweep statistics move by roughly 21 percent
  under the same collapse.

## Kraken

Whole-second timestamps on every row. Kraken can never adjudicate anything
sub-second, which is why the cadence lineage is Binance. It remains usable for
per-print statistics that do not depend on intra-second ordering - a price
equality fraction, for instance, is unharmed.
