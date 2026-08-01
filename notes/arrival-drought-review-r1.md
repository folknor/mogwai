# R1 - Opus review of notes/arrival-drought-elimination-spec.md

## Verdict

The spec is well-constructed as a document - survey, decisions, landings, and stopping rule all check out against the code, and I verified essentially every code claim in §2 as accurate (the ACD recursion at `source.rs:232-268`, `AcdClock` fields, the un-modulated feedback invariant, the regime plumbing, the twelve-field `Measured`, the `[1.0, 1000.0]` validation). But its **empirical premise is falsified by data already committed in this repo**, and under its own L1 threshold the item closes. That is a bug in the spec, not a nit.

---

### bug - the premise "real BTCUSD never prints 15-18 h near-silent stretches" is false

`analysis/char_XBTUSD.json` already holds the answer L1 proposes to go measure. Decoding the committed `duration.log_hist` (40 log bins over 1e-3..86400 s, 81.8M gaps, span 4194 days):

| bin range (s) | count |
|---|---|
| 13895 .. 21942 | 551 |
| 21942 .. 34649 | 298 |
| 34649 .. 54714 | 142 |
| 54714 .. 86400+ | **94** |

So the real anchor prints ~1085 gaps above 4 h and ~94 gaps in the 15-24 h band - precisely the shape the spec says "real BTCUSD never prints". Worse, `log_bin` returns `nbins - 1` for any value `>= hi`, so the top bin is **saturated**: those 94 could be days long (Kraken's 2013-2015 era and its outage history). `max_gap_s` on the anchor is therefore near-certainly far above the spec's 6 h close-threshold.

Consequence: §5 L1 says "If the regenerated anchor report shows `max_gap_s >= 21600` (6 h) or `empty_hour_frac >= 0.05` on XBTUSD, the premise is false ... L2 is never laid." The spec is written to kill itself. Its stated calibration ("minutes-scale max gaps and `empty_hour_frac` ~0") is off by three orders of magnitude.

The item is still real - a *self-prolonging* desert that compounds per tick is not the same defect as a genuine venue outage - but the spec must reframe the target as *conditional dwell* (how long a quiet stretch persists once entered) rather than "no long gaps exist", and must decide explicitly whether the corpus is scoped to a recent window or whether outage gaps are censored. As written, D6's "the anchor series itself satisfies dispersion, ACF and realistic dwell simultaneously" is asserted, not shown, and the histogram argues against it.

### bug - D3 picks the anchor for exactly the reason that makes it the wrong choice

D3 argues the cross-pair range must not be used because "it includes near-dead pairs whose dwell is exactly the behavior being evicted". The data inverts this. Top-4-bin counts per pair:

XBTUSD `[551, 298, 142, 94]`, ETHUSD `[135, 87, 52, 53]`, XDGUSD `[157, 46, 13, 7]`, then ADAUSD/DOTUSD/SOLUSD/XRPUSD/USDTUSD all in the single digits.

XBTUSD is the **most** desert-prone pair in the corpus, by an order of magnitude. Reading the anchor makes the dwell gate the loosest of the eight available, not the strictest. The same inversion already shows in dispersion: anchor 4608.9 **is** the band max; the 131.7 floor is DOTUSD. Which undercuts D4's supporting claim that "the dispersion floor requires the big-gap mass to exist" - that floor is another pair's number and polices nothing about the anchor.

### bug - `session_curves` matches no test

L2's gate list contains `brokkr test -p mogwai-data session_curves`, and §2.2 calls it "the `#[ignore]`d `session_curves` test". The test is `session_modulation_reproduces_curves` (`tests.rs:970`); the only `session_curves` substring in the file is the helper fn `measure_session_curves` and the `SessionCurves` type. A substring filter of `session_curves` matches zero tests - a gate that is green because it ran nothing. Given that this test is `#[ignore]`d and thus invisible to `brokkr check`, this is the one gate in L2 that actually adds coverage, and it silently doesn't.

### gap - D5's retune levers work against each other

D5 orders the search "tau first to land dwell, then restore degraded duration statistics", with "dispersion low -> lower `ACD_WEIBULL_SHAPE` toward 0.55". But `max_gap_s` is `psi * eps` and `eps` is the Weibull draw: lowering the shape fattens the eps tail and directly re-inflates the max over a 2M draw. Step two undoes step one. The spec presents the loop as sequential and never states the coupling. Relatedly, D5's "tau directly bounds `max_gap_s`" overstates the mechanism: tau bounds the *persistence* of an excursion (hence `empty_hour_frac`) but a single draw's gap is still `psi * eps` with an unbounded heavy tail, so tau bounds `max_gap_s` only indirectly through psi.

### gap - `max_gap_s` is not comparable across sample sizes

It is a single order statistic. The corpus max is over 81.8M gaps spanning 4194 days; the gate's is over 2M gaps spanning ~160 simulated days - a 40x sample-size mismatch, which biases the corpus number upward for reasons unrelated to realism. D4's flat `2.0 *` slack is asserted without addressing this. `gap_p999_s`, which the spec explicitly demotes to "documentation value, not gated on", is the statistic that *is* sample-size stable and is the better gate. Note also that reading p999 off the existing 40-bin log histogram gives ~58% bin width (factor e^0.457 per bin), so it is coarse for a bound but fine as a target if the histogram is refined.

### gap - the generator does not run at the anchor's cadence

`GeneratorScalars::xbtusd_anchor` seeds `mean_duration_s` from `fp.scalar_ranges.mean_duration_s.median` = **7.19 s** (`fingerprint.rs:207`), while XBTUSD's own measured mean is **4.44 s**. The spec's §2.1 phrasing "the fingerprint's fitted ~7 s" is right about the constant but glosses that the tape is 1.6x slower than the anchor by construction. Gating dwell against anchor-derived absolutes (D3) while running at the cross-pair-median cadence bakes in a systematic ~1.6x handicap that the flat 2.0x slack silently absorbs, undocumented.

### gap - the doc sweep in 4.7 is incomplete

Beyond `reference/architecture.md`'s "Tape arrival droughts" section (line 253), droughts are treated as ambient default behavior at `architecture.md:453` ("a generation became current DURING a tape arrival drought") and `architecture.md:604` ("inside one of the tape's arrival droughts a request for N bars typically..."). Both are outside the named section and both become wrong under this landing. `notes/todo.md:111-114` (AD12) also encodes the old framing.

### smell - `empty_hour_frac` populations are not the same on both sides

The corpus figure is over a 4194-day span dominated by the sparse 2013-2015 era; the gate figure is over a ~160-day 2M-tick draw starting at `start_ts 0` under the session envelope. These are different populations, and the spec compares them with a `+0.01` absolute tolerance as if they were not. The denominator choice (`(last_ts - first_ts) // 3600`) is fine and O(1)-compatible as claimed.

### smell - D2's mean preservation holds only for constant `w`

With `w` constant the fixed point is exactly `mean_s` (`x = m + w*phi*(x - m)` gives `x = m` for any `w`), which is a genuinely nice property. But `w = exp(-prev_duration_s / tau)` is correlated with the state it damps, so `E[psi]` will sit below `mean_s` by a Jensen term that grows as the gap distribution fattens. D2 asserts mean preservation without this caveat. The retune does absorb it - but it means the landed `ACD_*` constants drift away from "fitted" toward "compensating", and the `consts.rs` comment rewrite D5 promises should say so.

### nit

- `AGENTS.md` still describes `docs/` as "the transient TODO"; `docs/` is empty and both the todo and this spec live in `notes/`. Stale.
- §2.2's "`Measured` has twelve fields" is exactly right (verified `tests.rs:1041-1054`), as is the `[131.7 .. 4608.9]` band and the `DURATION_ACF_ABS_TOL = 0.14` reading - the survey is otherwise unusually accurate.

---

**Recommendation.** Do not lay L1 as written; its threshold will close the item on data the repo already contains. Decode the committed `char_*.json` histograms first (no corpus disk needed, no code change), then rewrite §1's premise and D3/D4/D6 around a conditional-dwell statistic - `gap_p999_s` or a run-length measure - rather than `max_gap_s` against the single most desert-prone pair in the corpus.

Key files: `/home/folk/Programs/mogwai/notes/arrival-drought-elimination-spec.md`, `/home/folk/Programs/mogwai/analysis/char_XBTUSD.json`, `/home/folk/Programs/mogwai/analysis/characterize.py`, `/home/folk/Programs/mogwai/crates/mogwai-data/src/generated/source.rs`, `/home/folk/Programs/mogwai/crates/mogwai-data/src/generated/tests.rs`, `/home/folk/Programs/mogwai/crates/mogwai-data/src/generated/fingerprint.rs`.
