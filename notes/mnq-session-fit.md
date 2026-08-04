# MNQ session profile fit, from the NQ one-minute archive

Offline fit report, 2026-08-04. Produced by
`python3 analysis/fit_session_profile.py fit`.

This is a `notes/`-class document: transient, no truth guarantee, nothing
durable may cite it. It records what was measured, the contract that was fixed
before measuring, and the decision the measurement mechanically produced.

---

## 1. Archive identity and coverage

| | |
|---|---|
| file | `research/market-data/nq-1m_bk.zip` |
| symbol | NQ, continuous, ratio back-adjusted |
| rows | 5,891,412 |
| span | 2008-12-11 to 2026-07-08 |
| format | no header, `DD/MM/YYYY;HH:MM;O;H;L;C;V`, semicolon-delimited, LF |
| timestamps | CME local civil time, observing DST |

Ratio back-adjustment destroys price level and the tick grid while preserving
returns exactly. That is why this archive can source `intensity_hour` and
`vol_hour` and could never source `modal_tick` or a `zero_change_frac` analogue.

## 2. The calendar contract, corrected before fitting

The fit is conditional on the market being open, so the calendar is an input to
the estimator rather than a downstream consumer of it. The shipped MNQ calendar
was wrong and was corrected first.

It previously declared `18:00 -> 17:00` with a `16:15 - 16:30` halt, described in
its own comment as exchange-local Chicago hours. CME equity index trades
`17:00 -> 16:00` Central with a `15:15 - 15:30` halt. The old figures were that
same schedule rendered one hour forward - the winter UTC instants shown on a
permanent `-300` clock - and so described no civil session CME publishes.

Corrected contract, permanent CDT civil time:

```
utc_offset_minutes       = -300
session                  = 17:00 -> 16:00
equity-index halt        = 15:15 -> 15:30
daily break              = 16:00 -> 17:00
settlement_minute_of_day = 900          # 15:00 Central
```

`settlement_minute_of_day` moved 960 -> 900. That is the same real instant
re-expressed, not a new choice, and it is forced: `SessionCalendar::is_open`
treats windows as half-open, so under the corrected close 960 is the exclusive
end of the `15:30 - 16:00` window, and `validate` refuses a settlement minute
that is open on no day. Left at 960 the preset would fail to load, taking MES
with it by inheritance.

Open minutes per week are unchanged at 6,825 of 10,080.

**Stated limitation.** The offset is fixed and DST transitions are unmodelled,
so the model week is CDT all year. Roughly half of any real year is CST, and
those sessions occur one UTC hour later than this table places them. That is a
limitation of the fixed-offset model, not an approximation that averages out.

### Why civil-phase alignment

Historical civil labels are preserved and read against the fixed offset, so CST
and CDT observations land on the same canonical session phase and no season
disproportionately supplies the boundary buckets. Measured against the old
calendar the opposite reading looked better, which was evidence about the
calendar rather than about the alignment - that table matched CST instants, so
only the winter half could align with it:

| | old calendar, instant | old calendar, civil | corrected calendar, civil |
|---|---:|---:|---:|
| rows outside the calendar | 175,394 | 247,289 | **44,737** |
| eligible sessions | 4,246 | 4,180 | **4,376** |
| excluded as early close | 293 | 359 | **163** |

The early-close count is the sanity signal. Under the old table ordinary
sessions looked truncated because the calendar's close sat an hour late. 163
exclusions across 4,539 observed sessions over 17.6 years is the right order for
CME holiday and half-day sessions; 293 was not.

## 3. Exposure preflight

`python3 analysis/fit_session_profile.py preflight`

```json
{
  "alignment": "civil",
  "calendar_open_minutes_per_week": 6825,
  "cdt_rows": 3866005,
  "cst_rows_remapped": 2025407,
  "early_close_excluded": 163,
  "eligible_minutes_present": 5683836,
  "eligible_open_minutes_expected": 5973240,
  "missing_minutes_inside_eligible_sessions": 289404,
  "present_zero_volume_rows": 0,
  "rows": 5891412,
  "rows_outside_declared_calendar": 44737,
  "sessions_eligible": 4376,
  "sessions_observed": 4539
}
```

**The finding that determines the estimator: `present_zero_volume_rows` is 0.**
The archive omits minutes with no trades rather than emitting a zero-volume row.
Exposure therefore MUST come from the calendar. Deriving it from row presence
would shrink each quiet hour's denominator in exact proportion to its own
quietness and compress the peak-to-trough ratio the whole fit exists to measure.

Exposure checks exactly: 4,376 eligible sessions x 1,365 open minutes =
5,973,240. The 289,404 missing minutes inside eligible sessions are genuinely
quiet overnight minutes and are exposed with zero volume. Ineligible sessions -
full holidays, which appear as absent, and early closes, which appear truncated -
contribute to neither numerator nor exposure, because the model deliberately
carries no date-specific exceptions and letting a half-session contribute would
leak holiday behaviour into ordinary weekday and hour factors.

## 4. Preregistered constants

Fixed in `analysis/fit_session_profile.py` before the estimator was first run.

```python
INTERACTION_RATIO_LIMIT     = 1.25
MAX_MATERIAL_EXPOSURE_SHARE = 0.05
ERAS = (("early", 2009, 2014), ("middle", 2015, 2019), ("recent", 2020, 2026))
DESIGNATED_FIT_ERA          = "recent"
EARLY_CLOSE_TOLERANCE_MINUTES = 60
```

`R = 1.25` is calibrated to the defect being corrected - a flat 1.78x curve
against a real order-of-magnitude intraday swing - rather than to brick 7's
1.05, which answered a different question about whether a budget constant needed
resizing at all. The material-share allowance lets a handful of genuine
weekday-specific interactions coexist with a useful separable model.

Separability rule:

```
cell_error[h,d] = observed_rate[h,d] / fitted_rate[h,d]
material cell   = cell_error outside [1/R, R]
material share  = exposure in material cells / total open exposure
```

Era stability uses the SAME rule and the same two constants, comparing each
cell's runtime multiplier between the full and designated fits and weighting by
the designated era's exposure. No threshold entered after results existed.

## 5. Estimator

Multiplicative Poisson rate with an exposure offset,

```
E[volume[h,d]] = exposure[h,d] * alpha * hour[h] * day[d]
```

fitted by alternating closed-form updates with `alpha` present in both
denominators and the composite renormalized to an exposure-weighted mean of one
each sweep. That identification leaves `alpha` carrying corpus-wide contracts per
open minute while `hour` and `day` remain purely relative, matching runtime
semantics. Saturday is pinned to 1.0 and never updated: with the shipped
calendar its exposure is zero in every cell, so its update is 0/0 and its value
is a declared convention.

`vol_hour` is the RMS of close-to-close minute returns per UTC hour, normalized
to an observation-weighted mean of one. A return is formed only between two
adjacent present open minutes inside one eligible session, which excludes
closure crossings, the halt, the overnight break and every missing minute at
once. The roll trim follows the existing `select_windows.py` convention: drop the
single largest squared return per session. Per open item 14.7 that handles
realized volatility while gap features still see adjustment boundaries, so it is
a mitigation rather than a cure.

## 6. Results

Preregistered header: `R=1.25, max material exposure share=0.05,
designated era=recent, alignment=civil`.

| scope | alpha | sweeps | material share | material cells | separable | peak-to-trough fitted | observed |
|---|---:|---:|---:|---:|---|---:|---:|
| full | 290.0524 | 21 | 0.0000 | 0 | PASS | 36.45x | 36.45x |
| early 2009-2014 | 186.8553 | 25 | 0.0336 | 4 | PASS | 117.55x | 113.53x |
| middle 2015-2019 | 239.7084 | 20 | 0.0087 | 1 | PASS | 37.99x | 37.99x |
| recent 2020-2026 | 424.8419 | 21 | 0.0000 | 0 | PASS | 27.51x | 27.51x |

### 6.1 full corpus

```
hour factors and open-minute exposure:
  00        0.1958  exposure       262560      12        0.7277  exposure       262560
  01        0.1821  exposure       262560      13        3.4052  exposure       262560
  02        0.1427  exposure       262560      14        3.7758  exposure       262560
  03        0.1109  exposure       262560      15        2.6281  exposure       262560
  04        0.1036  exposure       262560      16        1.9530  exposure       262560
  05        0.1282  exposure       262560      17        1.7866  exposure       262560
  06        0.1955  exposure       262560      18        2.0049  exposure       262560
  07        0.3625  exposure       262560      19        2.8692  exposure       262560
  08        0.3167  exposure       262560      20        0.9111  exposure       196920
  09        0.2599  exposure       262560      21     unexposed
  10        0.2701  exposure       262560      22        0.1562  exposure       262560
  11        0.3636  exposure       262560      23        0.1305  exposure       262560

day factors and open-minute exposure:
  Sun        1.5082  exposure        98880       Thu        1.0507  exposure      1204020
  Mon        0.9123  exposure      1134600       Fri        1.0099  exposure      1073190
  Tue        0.9919  exposure      1235970       Sat        1.0000  CONVENTIONAL, zero exposure
  Wed        1.0230  exposure      1226580

hour x day residual matrix, observed/fitted (dot = unexposed, star = material):
             Sun       Mon       Tue       Wed       Thu       Fri       Sat
  00           .    1.186     0.943     0.950     0.969     0.985          .
  01           .    1.152     0.957     0.976     0.976     0.963          .
  02           .    1.091     0.955     0.975     1.020     0.973          .
  03           .    1.068     0.971     0.942     1.004     1.029          .
  04           .    1.036     0.980     0.956     1.013     1.022          .
  05           .    1.045     0.972     0.980     0.988     1.024          .
  06           .    1.082     0.977     0.965     1.012     0.978          .
  07           .    1.087     1.010     0.964     0.991     0.962          .
  08           .    1.077     1.015     0.973     0.997     0.950          .
  09           .    1.019     1.026     0.954     1.009     0.995          .
  10           .    0.994     1.021     0.954     1.004     1.028          .
  11           .    1.024     1.012     0.981     1.005     0.983          .
  12           .    0.808     0.897     0.959     1.079     1.230          .
  13           .    1.029     1.006     0.964     0.980     1.028          .
  14           .    1.007     1.022     0.968     0.997     1.008          .
  15           .    1.007     1.014     0.959     1.002     1.020          .
  16           .    1.000     1.012     0.955     1.021     1.014          .
  17           .    0.981     1.019     0.975     1.036     0.986          .
  18           .    0.930     0.969     1.224     0.973     0.885          .
  19           .    1.004     0.984     1.044     0.982     0.985          .
  20           .    0.983     0.990     1.027     1.048     0.945          .
  21           .         .         .         .         .         .         .
  22      1.128     0.915     0.966     1.007     0.928          .         .
  23      0.846     1.039     1.086     1.089     1.002          .         .
```

### 6.2 early, 2009-2014

The only scope showing material interaction: four cells, 3.36% of exposure,
inside the 5% allowance. Preserved so the observation is not lost. It cannot
change the outcome and was deliberately not investigated further - expanding the
analysis after seeing results is the discipline this preregistration exists to
prevent. Optional research, not a blocker.

```
hour factors and open-minute exposure:
  00        0.0589  exposure        89580      12        0.6751  exposure        89580
  01        0.0577  exposure        89580      13        3.6610  exposure        89580
  02        0.0488  exposure        89580      14        4.0557  exposure        89580
  03        0.0398  exposure        89580      15        2.6868  exposure        89580
  04        0.0357  exposure        89580      16        1.9197  exposure        89580
  05        0.0414  exposure        89580      17        1.8133  exposure        89580
  06        0.0886  exposure        89580      18        2.1687  exposure        89580
  07        0.2648  exposure        89580      19        3.2931  exposure        89580
  08        0.2107  exposure        89580      20        1.1936  exposure        67185
  09        0.1807  exposure        89580      21     unexposed
  10        0.1905  exposure        89580      22        0.0507  exposure        89580
  11        0.2809  exposure        89580      23        0.0345  exposure        89580

day factors and open-minute exposure:
  Sun        1.9617  exposure        33960       Thu        1.0452  exposure       410910
  Mon        0.9280  exposure       389415       Fri        0.9857  exposure       362295
  Tue        1.0014  exposure       421665       Sat        1.0000  CONVENTIONAL, zero exposure
  Wed        1.0302  exposure       419700

hour x day residual matrix, observed/fitted (dot = unexposed, star = material):
             Sun       Mon       Tue       Wed       Thu       Fri       Sat
  00           .    1.252*    0.878     0.899     0.998     1.016          .
  01           .    1.249     0.924     0.931     1.011     0.918          .
  02           .    1.174     0.974     0.900     1.014     0.965          .
  03           .    1.016     1.007     0.984     0.967     1.031          .
  04           .    0.972     0.992     0.907     1.109     1.017          .
  05           .    0.945     1.030     0.929     0.989     1.109          .
  06           .    1.097     1.003     0.983     0.980     0.949          .
  07           .    1.092     1.029     0.950     0.955     0.989          .
  08           .    1.032     1.022     0.949     1.047     0.952          .
  09           .    0.926     1.052     0.954     1.036     1.022          .
  10           .    0.928     0.971     0.972     1.063     1.059          .
  11           .    0.910     1.020     1.011     1.043     1.001          .
  12           .    0.776*    0.847     0.928     1.135     1.302*         .
  13           .    1.035     0.985     0.953     0.963     1.076          .
  14           .    1.014     0.995     0.979     1.003     1.012          .
  15           .    1.009     1.006     0.952     1.009     1.028          .
  16           .    1.029     0.995     0.972     0.997     1.013          .
  17           .    0.978     1.042     1.011     1.023     0.937          .
  18           .    0.947     1.016     1.212     0.963     0.837          .
  19           .    1.006     1.006     1.028     0.990     0.968          .
  20           .    0.998     1.030     0.968     1.062     0.938          .
  21           .         .         .         .         .         .         .
  22      1.142     0.897     0.926     1.017     0.895          .         .
  23      0.791*    1.077     1.100     1.145     1.059          .         .
```

### 6.3 middle, 2015-2019

```
hour factors and open-minute exposure:
  00        0.2081  exposure        74820      12        0.6465  exposure        74820
  01        0.2079  exposure        74820      13        3.4887  exposure        74820
  02        0.1584  exposure        74820      14        3.7745  exposure        74820
  03        0.1135  exposure        74820      15        2.6978  exposure        74820
  04        0.0994  exposure        74820      16        1.9525  exposure        74820
  05        0.1156  exposure        74820      17        1.7141  exposure        74820
  06        0.1938  exposure        74820      18        2.0047  exposure        74820
  07        0.3708  exposure        74820      19        2.8266  exposure        74820
  08        0.3090  exposure        74820      20        0.9381  exposure        56115
  09        0.2701  exposure        74820      21     unexposed
  10        0.2725  exposure        74820      22        0.1426  exposure        74820
  11        0.3573  exposure        74820      23        0.1229  exposure        74820

day factors and open-minute exposure:
  Sun        1.3633  exposure        28080       Thu        1.0508  exposure       344745
  Mon        0.9145  exposure       322050       Fri        1.0009  exposure       308760
  Tue        0.9904  exposure       349440       Sat        1.0000  CONVENTIONAL, zero exposure
  Wed        1.0327  exposure       349080

hour x day residual matrix, observed/fitted (dot = unexposed, star = material):
             Sun       Mon       Tue       Wed       Thu       Fri       Sat
  00           .    1.065     0.964     0.995     1.001     0.985          .
  01           .    1.061     0.937     1.012     1.020     0.977          .
  02           .    1.062     0.948     0.986     1.018     0.996          .
  03           .    0.973     0.983     0.990     0.996     1.055          .
  04           .    0.946     1.011     1.010     0.994     1.030          .
  05           .    1.036     0.936     1.015     0.987     1.032          .
  06           .    1.064     0.994     0.973     1.018     0.961          .
  07           .    1.084     0.962     0.987     1.010     0.970          .
  08           .    1.048     1.014     1.012     0.989     0.943          .
  09           .    0.968     1.010     0.973     1.081     0.960          .
  10           .    0.962     1.053     0.929     1.029     1.024          .
  11           .    0.971     1.037     0.973     1.067     0.944          .
  12           .    0.860     0.912     0.921     0.996     1.299*         .
  13           .    1.017     1.021     0.960     0.968     1.040          .
  14           .    1.017     1.026     0.964     1.010     0.987          .
  15           .    1.021     1.010     0.974     1.002     0.997          .
  16           .    1.003     1.017     0.957     1.008     1.017          .
  17           .    0.992     1.014     0.957     1.053     0.983          .
  18           .    0.924     0.954     1.236     0.982     0.880          .
  19           .    1.001     0.983     1.038     0.968     1.011          .
  20           .    1.017     0.950     1.006     1.044     0.982          .
  21           .         .         .         .         .         .         .
  22      1.164     0.943     0.897     1.020     0.930          .         .
  23      0.810     1.040     1.062     1.075     1.061          .         .
```

### 6.4 recent, 2020-2026 - the selected fit

```
hour factors and open-minute exposure:
  00        0.2463  exposure        97380      12        0.7852  exposure        97380
  01        0.2217  exposure        97380      13        3.2665  exposure        97380
  02        0.1742  exposure        97380      14        3.6634  exposure        97380
  03        0.1389  exposure        97380      15        2.5743  exposure        97380
  04        0.1332  exposure        97380      16        1.9670  exposure        97380
  05        0.1692  exposure        97380      17        1.8072  exposure        97380
  06        0.2400  exposure        97380      18        1.9361  exposure        97380
  07        0.3993  exposure        97380      19        2.7106  exposure        97380
  08        0.3635  exposure        97380      20        0.7826  exposure        73035
  09        0.2880  exposure        97380      21     unexposed
  10        0.3018  exposure        97380      22        0.2052  exposure        97380
  11        0.4005  exposure        97380      23        0.1732  exposure        97380

day factors and open-minute exposure:
  Sun        1.5179  exposure        36480       Thu        1.0535  exposure       445515
  Mon        0.9080  exposure       419040       Fri        1.0225  exposure       398400
  Tue        0.9865  exposure       460890       Sat        1.0000  CONVENTIONAL, zero exposure
  Wed        1.0157  exposure       455070

hour x day residual matrix, observed/fitted (dot = unexposed, star = material):
             Sun       Mon       Tue       Wed       Thu       Fri       Sat
  00           .    1.234     0.943     0.940     0.953     0.972          .
  01           .    1.189     0.971     0.968     0.953     0.952          .
  02           .    1.101     0.958     0.982     1.020     0.955          .
  03           .    1.116     0.964     0.922     1.009     1.010          .
  04           .    1.081     0.971     0.946     1.008     1.010          .
  05           .    1.066     0.978     0.978     0.987     1.003          .
  06           .    1.094     0.969     0.961     1.013     0.979          .
  07           .    1.092     1.025     0.959     0.993     0.947          .
  08           .    1.103     1.014     0.965     0.988     0.947          .
  09           .    1.069     1.027     0.947     0.973     0.998          .
  10           .    1.028     1.023     0.961     0.978     1.017          .
  11           .    1.081     1.001     0.976     0.969     0.989          .
  12           .    0.803     0.909     0.985     1.090     1.180          .
  13           .    1.031     1.008     0.971     0.994     1.003          .
  14           .    0.999     1.032     0.964     0.990     1.017          .
  15           .    0.999     1.019     0.955     0.999     1.028          .
  16           .    0.987     1.017     0.946     1.036     1.012          .
  17           .    0.976     1.012     0.967     1.034     1.006          .
  18           .    0.925     0.952     1.223     0.972     0.910          .
  19           .    1.002     0.972     1.056     0.984     0.986          .
  20           .    0.950     0.986     1.073     1.042     0.937          .
  21           .         .         .         .         .         .         .
  22      1.115     0.907     0.995     1.007     0.926          .         .
  23      0.864     1.033     1.095     1.092     0.972          .         .
```

### 6.5 vol_hour by scope

Per-mean RMS return ratio, with return counts. Recent-era roll trim removed one
return from each of 1,623 sessions; 1,187 pairs were skipped as non-adjacent.

| hour | full | early | middle | **recent** | recent returns |
|---:|---:|---:|---:|---:|---:|
| 00 | 0.7327 | 0.6263 | 0.7726 | **0.7733** | 97,239 |
| 01 | 0.7071 | 0.6283 | 0.8011 | **0.7113** | 97,227 |
| 02 | 0.6430 | 0.6176 | 0.6760 | **0.6349** | 97,172 |
| 03 | 0.5707 | 0.5754 | 0.6050 | **0.5533** | 97,106 |
| 04 | 0.5668 | 0.5686 | 0.5800 | **0.5591** | 97,044 |
| 05 | 0.6327 | 0.5758 | 0.6039 | **0.6666** | 97,129 |
| 06 | 0.7233 | 0.6779 | 0.7458 | **0.7446** | 97,124 |
| 07 | 0.9433 | 0.9402 | 0.9875 | **0.9350** | 97,113 |
| 08 | 0.8378 | 0.8244 | 0.8438 | **0.8506** | 97,166 |
| 09 | 0.7564 | 0.7782 | 0.7694 | **0.7417** | 97,208 |
| 10 | 0.7541 | 0.7575 | 0.7380 | **0.7625** | 97,162 |
| 11 | 0.8053 | 0.7923 | 0.7507 | **0.8393** | 97,170 |
| 12 | 1.0353 | 1.0180 | 0.9357 | **1.0956** | 96,963 |
| 13 | 1.7278 | 1.6561 | 1.7398 | **1.7876** | 96,637 |
| 14 | 1.7980 | 1.7173 | 1.8114 | **1.8702** | 97,083 |
| 15 | 1.4256 | 1.3097 | 1.4815 | **1.4981** | 97,291 |
| 16 | 1.2333 | 1.1338 | 1.2758 | **1.2985** | 97,334 |
| 17 | 1.2494 | 1.1831 | 1.2446 | **1.3097** | 97,307 |
| 18 | 1.3967 | 1.4005 | 1.4156 | **1.3897** | 97,297 |
| 19 | 1.5335 | 1.4960 | 1.5523 | **1.5489** | 97,265 |
| 20 | 0.9390 | 0.9835 | 0.9322 | **0.9138** | 71,312 |
| 21 | 1.0000 | 1.0000 | 1.0000 | **1.0000** | CONVENTIONAL, none |
| 22 | 0.7771 | 0.6640 | 0.7388 | **0.8323** | 95,590 |
| 23 | 0.6390 | 0.5711 | 0.6536 | **0.6596** | 97,152 |

## 7. Outcome, selected mechanically

```
era stability: full vs recent, divergent exposure share 0.2283 over 26 cells
               -> ERA-DEPENDENT

Outcome 2: the full corpus misrepresents the designated era. Fit recent only.
```

22.83% of recent-era exposure sits in cells where the full-corpus multiplier
differs from the recent-era one by more than 25%, against a 5% allowance.

The cause is a large monotone structural drift:

| scope | peak-to-trough |
|---|---:|
| early 2009-2014 | 117.55x |
| middle 2015-2019 | 37.99x |
| recent 2020-2026 | 27.51x |
| full corpus | 36.45x |

Intraday concentration has fallen roughly fourfold across the archive as
overnight electronic trading grew relative to the cash session. Fitting the full
corpus would have shipped a 36.45x curve for a market currently running 27.51x -
a 32% error in the headline quantity, and the same trap section 7.1 of the
purchase report documented for `zero_change_frac`.

**Against the defect being corrected**: the shipped crypto curve is 1.78x
peak-to-trough. MNQ's measured recent-era curve is 27.51x. The session profile
is roughly 15x too flat.

## 8. What is adopted

Recent era, 2020-2026. Values are the fitted factors verbatim, already
normalized to an exposure-weighted composite mean of one over open minutes,
which is exactly what a calendar-aware runtime normalizer reproduces.

```
intensity_hour = [
  0.2463, 0.2217, 0.1742, 0.1389, 0.1332, 0.1692, 0.2400, 0.3993,
  0.3635, 0.2880, 0.3018, 0.4005, 0.7852, 3.2665, 3.6634, 2.5743,
  1.9670, 1.8072, 1.9361, 2.7106, 0.7826, 1.0000, 0.2052, 0.1732,
]

dow_weight = [ 1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000 ]
             #   Sun     Mon     Tue     Wed     Thu     Fri     Sat

vol_hour = [
  0.7733, 0.7113, 0.6349, 0.5533, 0.5591, 0.6666, 0.7446, 0.9350,
  0.8506, 0.7417, 0.7625, 0.8393, 1.0956, 1.7876, 1.8702, 1.4981,
  1.2985, 1.3097, 1.3897, 1.5489, 0.9138, 1.0000, 0.8323, 0.6596,
]
```

## 9. Caveats that belong in provenance

**Hour 21 UTC is unexposed and its 1.0 is a convention, not a measurement.** It
is the 16:00-17:00 Central daily break, closed every day of the model week, so
no observation can inform it. **Hour 20 is partially exposed** - 73,035 open
minutes against 97,380 for a fully exposed hour - because the break consumes
part of it. Its factor is estimated from the open portion, which is the correct
treatment, but it rests on three quarters of the evidence its neighbours have.

**Saturday's day factor is conventional** for the same reason: zero exposure
under the shipped calendar, so its update is 0/0 and 1.0 is declared rather than
estimated.

**Sunday rests on an order of magnitude less exposure than any weekday** -
36,480 open minutes against roughly 400,000 - because the model week opens
Sunday 17:00. Its 1.5179 is identified but carries materially wider uncertainty,
which is why exposure is printed beside every factor rather than implied.

**`intensity_hour` is a contract-VOLUME intensity used as an arrival-count
proxy.** Bars carry volume, not trade counts, and volume is count times mean
size. Mean trade size is larger during the cash session, so this fit
overstates peak-to-trough relative to the true arrival-rate curve. **27.51x is an
upper bound on the arrival swing, not an estimate of it.** The direction of the
bias is known; its magnitude is not.

**DST is unmodelled and the omission is one-sided**, per section 2.

## 10. Obligation this places on the paired purchase

The 10.02 dollar `NQ.v.0` + `MNQ.v.0` paired window in the purchase report
acquires a second job beyond the contract-versus-market question it was bought
for. It carries trade counts, so it can measure the volume-to-arrival proxy
directly.

**It must compare volume-per-minute and trade-count-per-minute SESSION CURVES,
not merely their aggregate moments.** The quantity in question is the ratio of
the two peak-to-trough figures, which an aggregate comparison cannot see. If the
count curve is materially flatter than the volume curve, `intensity_hour` as
fitted here is biased by that factor and the provenance claim in section 9
becomes a measured correction rather than a stated direction.
