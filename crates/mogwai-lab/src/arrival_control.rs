// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol 12b brick N: the deterministic hourly re-centring negative
//! control. The mechanism is `notes/protocol-12b-arrival-composition-spec.md`
//! section 5.5 and brick N; gates B1 to B7 are that document's section 10.2,
//! verbatim. Every constant here is lifted, never chosen.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;

use mogwai_data::{TickEvent, TickSource};
use mogwai_venue::source::InstrumentProfile;
use serde_json::{Value, json};

use crate::aggregate::RefusalRec;
use crate::aggregate::context::ObsContext;
use crate::error::{LabError, LabResult};
use crate::fit::walk::{
    OverrideValue, Overrides, parse_duration, profile_from_config, run_summary_walk,
    scratch_config_text,
};
use crate::measure12a::generated::GeneratedAcc;

/// The 12b section 7 and 16 seed sets, verbatim. Pairwise disjoint from every
/// other 12b seed set, and in particular from CONFIRMATION_SEEDS = 1..8: the
/// ratios are measured on the fit seeds and the corrected curve is judged out
/// of sample on the test seeds.
pub const CONTROL_FIT_SEEDS: [u64; 4] = [301, 302, 303, 304];
pub const CONTROL_TEST_SEEDS: [u64; 4] = [305, 306, 307, 308];
/// 12b section 16, verbatim: B6's per-hour per-seed mean-rate band.
pub const MEAN_RATE_BAND: (f64, f64) = (0.98, 1.02);
/// 12b section 16, verbatim: B7's per-hour per-seed zero-count band.
pub const ZERO_COUNT_BAND: (f64, f64) = (0.8, 1.25);
/// 12b section 1.2 obligation 2 and 10.2 B3. Inherited, not relaxable.
pub const WALLTIME_BAND: (f64, f64) = (0.8, 1.25);
pub const WALLTIME_HORIZONS_S: [i64; 2] = [60, 300];

/// The gate index set: the hours the instrument's calendar actually opens, in
/// ascending order. An hour with zero scheduled minutes is outside the set -
/// not refused, not skipped, absent, exactly as B8 is absent by
/// inapplicability. MNQ yields 23 hours; hour 21 UTC is the daily break and
/// no session ever exposes it, so judging it would fail B6 and B7 by
/// construction whatever the mechanism does.
pub fn gate_hours(profile: &InstrumentProfile) -> LabResult<Vec<i64>> {
    let calendar = profile
        .calendar
        .as_ref()
        .ok_or_else(|| LabError::refusal("the MNQ preset carries no session calendar"))?;
    let mut exposed = [false; 24];
    for minute in 0..10_080_u64 {
        let ns = minute * 60_000_000_000;
        if calendar.is_open(ns) {
            exposed[((ns / 3_600_000_000_000) % 24) as usize] = true;
        }
    }
    Ok(exposed
        .iter()
        .enumerate()
        .filter_map(|(h, yes)| yes.then_some(h as i64))
        .collect())
}

/// One hour's rate reading on one context.
#[derive(Clone, Debug)]
pub struct HourRate {
    pub hour: i64,
    pub parents: i64,
    pub scheduled_minutes: i64,
    pub mean: Option<f64>,
}

/// Mean parents per scheduled minute, per hour, over a whole context.
/// Scheduled, not populated: an empty minute is a zero, not an absence, so the
/// denominator is the block2 `(hour, 60)` `scheduled_windows` sum rather than
/// the number of block1 rows that happen to carry mass.
pub fn hourly_mean_parents(ctx: &ObsContext) -> BTreeMap<i64, HourRate> {
    let mut out = BTreeMap::new();
    for hour in 0..24_i64 {
        let parents: i64 = ctx
            .per_session()
            .iter()
            .flat_map(|r| r["block1_hist"].as_array().into_iter().flatten())
            .filter(|r| r["hour"].as_i64() == Some(hour))
            .map(|r| r["n"].as_i64().unwrap_or(0) * r["count"].as_i64().unwrap_or(0))
            .sum();
        let scheduled_minutes: i64 = ctx.b2_scheduled(hour, 60).into_iter().flatten().sum();
        let mean = (scheduled_minutes > 0).then_some(parents as f64 / scheduled_minutes as f64);
        out.insert(
            hour,
            HourRate {
                hour,
                parents,
                scheduled_minutes,
                mean,
            },
        );
    }
    out
}

/// The 1 s zero-count fraction per hour: block2's `(hour, 1)` `zero_windows`
/// summed over sessions, over the summed `scheduled_windows`. `None` where any
/// session failed to serialize the cell - a field absence is a refusal at the
/// gate, never a silently smaller denominator.
pub fn hourly_zero_second_fraction(ctx: &ObsContext) -> BTreeMap<i64, Option<f64>> {
    (0..24_i64)
        .map(|hour| {
            let mut zero = 0_i64;
            let mut scheduled = 0_i64;
            let mut missing = false;
            for rec in ctx.per_session() {
                match rec["block2"].get(hour.to_string()).and_then(|v| v.get("1")) {
                    Some(cell) => {
                        zero += cell["zero_windows"].as_i64().unwrap_or(0);
                        scheduled += cell["scheduled_windows"].as_i64().unwrap_or(0);
                    }
                    None => missing = true,
                }
            }
            let value = (!missing && scheduled > 0).then_some(zero as f64 / scheduled as f64);
            (hour, value)
        })
        .collect()
}

/// The median over the four fit seeds. 5.5 says "median over seeds" and the
/// seed count is even, so the convention is pinned here rather than left to the
/// implementer: average the two middle readings - the ordinary arithmetic
/// median, not lower-median, not upper-median. Non-finite readings are dropped
/// first, and fewer than four finite readings yields `None`, which leaves that
/// hour's curve unchanged in `recentred_curve`.
pub fn seed_median(readings: &[Option<f64>]) -> Option<f64> {
    let mut xs: Vec<f64> = readings
        .iter()
        .flatten()
        .copied()
        .filter(|x| x.is_finite())
        .collect();
    if xs.len() != readings.len() || xs.is_empty() {
        return None;
    }
    xs.sort_by(f64::total_cmp);
    let middle = xs.len() / 2;
    if xs.len().is_multiple_of(2) {
        Some((xs[middle - 1] + xs[middle]) / 2.0)
    } else {
        Some(xs[middle])
    }
}

/// The section 5.5 correction, closed form, one pass, no search: divide each
/// hour by its ratio and rescale to sum to 1. An hour absent from `ratios` is
/// carried through unchanged - the control corrects what it measured and
/// invents nothing where it did not. The rescale is canonical serialization for
/// the `SessionProfile` schema and moves no generated rate, because
/// `SessionModulator`'s `arrival_normalizer` divides the scale straight back
/// out.
pub fn recentred_curve(old: &[f64; 24], ratios: &BTreeMap<i64, f64>) -> [f64; 24] {
    let mut out = *old;
    for (hour, ratio) in ratios {
        if (0..24).contains(hour) && ratio.is_finite() && *ratio > 0.0 {
            out[*hour as usize] /= ratio;
        }
    }
    let sum = crate::kernel::py_sum(out.iter().copied());
    for x in &mut out {
        *x /= sum;
    }
    out
}

/// `K` of the spec's section 2.4: the exposure-weighted normalizer ratio
/// between the shipped and the re-centred curve, over the calendar's open
/// minutes of one week, formed the way `SessionModulator::new` forms its
/// normalizer. Both curves are divided by their own sum first, so `K` is
/// scale-invariant and is exactly 1 when the per-hour ratio is constant -
/// without that, the sum-to-1 rescale alone would report the shipped curve's
/// mean-1 scale factor of about 23.86 and carry no information. Reported in the
/// artifact so a uniform B6 offset is readable; never applied, because dividing
/// it out would be a second iteration of a frozen one-iteration mechanism.
pub fn normalizer_drift(
    old: &[f64; 24],
    new: &[f64; 24],
    profile: &InstrumentProfile,
) -> LabResult<f64> {
    let calendar = profile
        .calendar
        .as_ref()
        .ok_or_else(|| LabError::refusal("the MNQ preset carries no session calendar"))?;
    let old_sum = crate::kernel::py_sum(old.iter().copied());
    let new_sum = crate::kernel::py_sum(new.iter().copied());
    let mut old_n = 0.0;
    let mut new_n = 0.0;
    let mut open = 0.0;
    for minute in 0..10_080_u64 {
        let ns = minute * 60_000_000_000;
        if calendar.is_open(ns) {
            let hour = ((ns / 3_600_000_000_000) % 24) as usize;
            let dow = (((ns / 86_400_000_000_000) + 4) % 7) as usize;
            let d = profile.session.dow_weight[dow];
            old_n += old[hour] / old_sum * d;
            new_n += new[hour] / new_sum * d;
            open += 1.0;
        }
    }
    if open == 0.0 || new_n == 0.0 {
        return Err(LabError::refusal("the calendar has no open minutes"));
    }
    Ok((old_n / open) / (new_n / open))
}

/// One control walk's two passes over one tape. The two passes exist because
/// `GeneratedAcc` and `summary::summarize` each own their own tick loop and
/// teeing them would need a new source adapter this brick may not add; the
/// generator is deterministic in profile, seed and start, so the second pass
/// replays the first byte for byte.
pub struct ControlWalk {
    pub seed: u64,
    pub ctx: ObsContext,
    pub summary: Value,
    pub cost_s: f64,
}

/// The exposure contract, parsed from `binding.generated` of the 12a artifact.
/// Refuses rather than defaulting on any missing field: an exposure that is
/// guessed is not the exposure the observed side was measured under.
#[derive(Clone, Debug)]
pub struct GeneratedBinding {
    pub window_start_ns: u64,
    pub window_length_ns: u64,
    pub burn_in: String,
}
impl GeneratedBinding {
    pub fn from_measure12a(a: &Value) -> LabResult<Self> {
        let g = &a["binding"]["generated"];
        Ok(Self {
            window_start_ns: g["window_start_ns"]
                .as_u64()
                .ok_or_else(|| LabError::refusal("binding.generated.window_start_ns is missing"))?,
            window_length_ns: g["window_length_ns"].as_u64().ok_or_else(|| {
                LabError::refusal("binding.generated.window_length_ns is missing")
            })?,
            // The burn-in prefix, under the binding's frozen `warmup` key.
            burn_in: g["warmup"]
                .as_str()
                .ok_or_else(|| LabError::refusal("binding.generated.warmup is missing"))?
                .to_string(),
        })
    }
    /// `window_length_ns` as the duration string `run_summary_walk` takes,
    /// `"<n>s"`, refusing a length that is not a whole number of seconds.
    pub fn length_arg(&self) -> LabResult<String> {
        if !self.window_length_ns.is_multiple_of(1_000_000_000) {
            return Err(LabError::refusal(
                "window length is not a whole number of seconds",
            ));
        }
        Ok(format!("{}s", self.window_length_ns / 1_000_000_000))
    }
}

/// One seed, one curve, both passes. `curve` is `None` for the shipped curve
/// (the ratio measurement) and `Some(new_curve)` for the corrected one (the
/// judgement). The exposure is 12b section 8, read from the 12a binding rather
/// than restated: MNQ, no divergence, regime neutral, vol trace on, the walk
/// starting at `window_start - burn_in` and the half-open measured window.
pub fn control_walk(
    scratch_dir: &Path,
    binding: &GeneratedBinding,
    curve: Option<&[f64; 24]>,
    seed: u64,
) -> LabResult<ControlWalk> {
    let started = Instant::now();
    let generated = control_generated_pass(scratch_dir, binding, curve, seed)?;
    let per_session = generated["per_session"]
        .as_array()
        .ok_or_else(|| LabError::refusal("generated walk has no per_session"))?
        .clone();
    let mut overrides = Overrides::new();
    if let Some(c) = curve {
        overrides.insert(
            "session.intensity_hour".to_string(),
            OverrideValue::Floats(c.to_vec()),
        );
    }
    let summary = run_summary_walk(
        scratch_dir,
        &overrides,
        seed as i64,
        binding.window_start_ns as i64,
        &binding.length_arg()?,
        &binding.burn_in,
    )?;
    Ok(ControlWalk {
        seed,
        ctx: ObsContext::new(per_session),
        summary,
        cost_s: started.elapsed().as_secs_f64(),
    })
}

/// The `GeneratedAcc` half of a control walk, on its own, returning
/// `GeneratedAcc::finish`'s record verbatim. Separate from [`control_walk`] for
/// one reason beyond tidiness: it is the side the spec's section 2.7 equality
/// pin compares against `mogwai_cli::measure::run_final_walk`, and that pin
/// already costs a month-long walk on each side - running the `summarize` pass
/// too would double a cost the pin has no use for.
///
/// This is the second copy of the 12b section 8 exposure contract (the first is
/// `run_final_walk`, which lives in `mogwai-cli` and is therefore unreachable
/// from this crate). Every element is matched deliberately: the MNQ profile
/// resolved through the shipped MNQ preset, with
/// `calendar.utc_offset_minutes` as the accumulator's hour offset,
/// `scalars.modal_tick` as its tick size, the vol trace enabled before the
/// loop, the walk starting at `window_start - burn_in` and the half-open
/// measured window. The scratch `[instrument.override]` curve is the only
/// intended difference, and it applies to the corrected-curve pass alone.
pub fn control_generated_pass(
    scratch_dir: &Path,
    binding: &GeneratedBinding,
    curve: Option<&[f64; 24]>,
    seed: u64,
) -> LabResult<Value> {
    let mut overrides = Overrides::new();
    if let Some(c) = curve {
        overrides.insert(
            "session.intensity_hour".to_string(),
            OverrideValue::Floats(c.to_vec()),
        );
    }
    // Section 2.7: the shipped-curve pass resolves MNQ exactly as
    // `mogwai_cli::measure::run_final_walk` does, through the shipped MNQ
    // preset, and the scratch `[instrument.override]`
    // is the one difference the corrected-curve pass introduces.
    let profile = if overrides.is_empty() {
        default_mnq_profile()?
    } else {
        std::fs::create_dir_all(scratch_dir)?;
        let config = scratch_dir.join(format!("arrival-control-{seed}.toml"));
        std::fs::write(&config, scratch_config_text(&overrides))?;
        let resolved = profile_from_config(&config)?;
        drop(std::fs::remove_file(&config));
        resolved
    };
    let calendar = profile
        .calendar
        .as_ref()
        .ok_or_else(|| LabError::refusal("the MNQ preset carries no session calendar"))?;
    let burn_in_ns = u64::try_from(parse_duration(&binding.burn_in)?)
        .map_err(|_| LabError::refusal("the burn-in is negative"))?;
    let start = binding.window_start_ns;
    let end = start + binding.window_length_ns;
    let walk_start = start
        .checked_sub(burn_in_ns)
        .ok_or_else(|| LabError::refusal("the burn-in underflows the start"))?;
    let mut source = mogwai_data::GeneratedSource::try_new_with_session_profile(
        profile.scalars.clone(),
        seed,
        walk_start,
        mogwai_venue::source::fingerprint(),
        &profile.session,
        None,
        mogwai_data::SizeGrid::from_def(&profile.def),
        profile.calendar.clone(),
    )
    .map_err(|e| LabError::refusal(format!("building the generator: {e:?}")))?;
    source.enable_vol_trace();
    let mut acc = GeneratedAcc::new(
        seed,
        start,
        end,
        i32::from(calendar.utc_offset_minutes),
        profile.scalars.modal_tick,
    );
    while let Some(event) = source.next_tick() {
        if event.ts_event() >= end {
            break;
        }
        match event {
            TickEvent::Quote(q) => {
                let trace = source.take_vol_trace();
                acc.push_quote(&q, trace)?;
            }
            TickEvent::Trade(t) => acc.push_trade(&t)?,
        }
    }
    acc.finish()
}

/// MNQ as `mogwai_cli::measure::run_final_walk` resolves it: the shipped preset.
fn default_mnq_profile() -> LabResult<InstrumentProfile> {
    mogwai_venue::config::profile_from_preset("MNQ")
        .map_err(|e| LabError::refusal(format!("resolving the MNQ preset: {e}")))
}

/// One gate's verdict, in the artifact's own shape.
pub struct GateRec {
    pub name: &'static str,
    pub passed: bool,
    pub evidence: Value,
    pub refusals: Vec<RefusalRec>,
}
/// The shared per-hour per-seed ratio-band gate behind B6 and B7. A cell that
/// cannot be evaluated is a recorded refusal and any refusal fails the gate:
/// 12b section 8 forbids dropping an inconvenient hour, and a refusal is
/// exactly the state a dropped hour would hide.
fn ratio_gate(
    name: &'static str,
    obs: &BTreeMap<i64, Option<f64>>,
    tests: &[ControlWalk],
    hours: &[i64],
    band: (f64, f64),
    getter: impl Fn(&ObsContext) -> BTreeMap<i64, Option<f64>>,
) -> GateRec {
    let mut refusals = Vec::new();
    let mut rows = Vec::new();
    let mut passed = true;
    for t in tests {
        let generated = getter(&t.ctx);
        for &h in hours {
            match (
                obs.get(&h).copied().flatten(),
                generated.get(&h).copied().flatten(),
            ) {
                (Some(o), Some(g)) if o > 0.0 => {
                    let r = g / o;
                    let ok = r >= band.0 && r <= band.1;
                    passed &= ok;
                    rows.push(json!({"seed":t.seed,"hour":h,"ratio":r,"passed":ok}));
                }
                (o, g) => {
                    passed = false;
                    let why = match (o, g) {
                        (None, _) => "the observed reading is absent at an hour the calendar opens",
                        (Some(o), None) if o > 0.0 => "the generated reading is absent",
                        (Some(_), _) => "the observed reading is zero, so no ratio exists",
                    };
                    refusals.push(RefusalRec::new(
                        name,
                        format!("seed {} hour {h}", t.seed),
                        why,
                    ));
                }
            }
        }
    }
    GateRec {
        name,
        passed: passed && refusals.is_empty(),
        evidence: Value::Array(rows),
        refusals,
    }
}

/// B6, mean-rate preservation: per hour and per seed, generated over observed
/// mean parents per scheduled minute inside `MEAN_RATE_BAND`.
pub fn gate_b6(obs: &ObsContext, tests: &[ControlWalk], hours: &[i64]) -> LabResult<GateRec> {
    let o = hourly_mean_parents(obs)
        .into_iter()
        .map(|(h, r)| (h, r.mean))
        .collect();
    Ok(ratio_gate("B6", &o, tests, hours, MEAN_RATE_BAND, |c| {
        hourly_mean_parents(c)
            .into_iter()
            .map(|(h, r)| (h, r.mean))
            .collect()
    }))
}
/// B7, sub-second composition: per hour and per seed, the 1 s zero-count
/// fraction ratio inside `ZERO_COUNT_BAND`.
pub fn gate_b7(obs: &ObsContext, tests: &[ControlWalk], hours: &[i64]) -> LabResult<GateRec> {
    let o = hourly_zero_second_fraction(obs);
    Ok(ratio_gate(
        "B7",
        &o,
        tests,
        hours,
        ZERO_COUNT_BAND,
        hourly_zero_second_fraction,
    ))
}
/// B3, the wall-time contour: hourly 60 s and 300 s `robust_scale` ratios
/// inside `WALLTIME_BAND` at every gate hour and every seed, on the
/// protocol-11 estimator. `b3_robust_strict` refuses on any missing or
/// non-finite session vote, and that strictness is not softened here.
pub fn gate_b3(obs: &ObsContext, tests: &[ControlWalk], hours: &[i64]) -> LabResult<GateRec> {
    let mut refusals = Vec::new();
    let mut rows = Vec::new();
    let mut passed = true;
    for t in tests {
        for &h in hours {
            for horizon in WALLTIME_HORIZONS_S {
                match (
                    obs.b3_robust_strict(h, horizon, &obs.ones()),
                    t.ctx.b3_robust_strict(h, horizon, &t.ctx.ones()),
                ) {
                    (Some(o), Some(g)) if o > 0.0 => {
                        let r = g / o;
                        let ok = r >= WALLTIME_BAND.0 && r <= WALLTIME_BAND.1;
                        passed &= ok;
                        rows.push(json!({"seed":t.seed,"hour":h,"horizon_s":horizon,"ratio":r,"passed":ok}));
                    }
                    _ => {
                        passed = false;
                        refusals.push(RefusalRec::new(
                            "B3",
                            format!("seed {} hour {h} horizon {horizon}", t.seed),
                            "robust scale cannot be evaluated",
                        ));
                    }
                }
            }
        }
    }
    Ok(GateRec {
        name: "B3",
        passed: passed && refusals.is_empty(),
        evidence: Value::Array(rows),
        refusals,
    })
}
/// B2, support and conditional adequacy - the gate 12b exists to move, since
/// the committed 12a artifact records the shipped generator refusing 22 of 24
/// hours here. `hours` is unused by design: 10.2 words this gate over all 24
/// hours the substitution implicates, not over the calendar-exposed index set,
/// and the substitution refuses on its own support rule rather than per hour.
pub fn gate_b2(obs: &ObsContext, tests: &[ControlWalk], _hours: &[i64]) -> LabResult<GateRec> {
    use crate::aggregate::countsub::{count_substitution, obs_shares_under, support_refusals_of};
    use crate::aggregate::family::conditional_adequacy_bins;
    use crate::aggregate::monthly::pool_session_hists;
    let shares = obs_shares_under(obs, &obs.ones());
    let mut refs = Vec::new();
    let mut records = Vec::new();
    for t in tests {
        let rec = count_substitution(&pool_session_hists(t.ctx.per_session())?, &shares);
        refs.extend(support_refusals_of(&rec));
        records.push(json!({"seed":t.seed,"substitution":rec}));
    }
    let contexts: Vec<ObsContext> = tests
        .iter()
        .map(|t| ObsContext::new(t.ctx.per_session().to_vec()))
        .collect();
    let cond = conditional_adequacy_bins(obs, &contexts);
    let bad = cond.iter().any(|b| b.required && !b.supported);
    let cond_json:Vec<Value>=cond.iter().map(|b|json!({"hour":b.hour,"bin":b.bin_name,"required":b.required,"supported":b.supported})).collect();
    Ok(GateRec {
        name: "B2",
        passed: refs.is_empty() && !bad,
        evidence: json!({"seeds":records,"conditional_adequacy":cond_json}),
        refusals: refs,
    })
}
/// B4, the two-sided minute-range envelope. The bounds are read from brick B4's
/// committed artifact and never recomputed, per that brick's amendment 2.
pub fn gate_b4(envelope: &Value, tests: &[ControlWalk]) -> LabResult<GateRec> {
    use crate::fit::observe::nearest_rank_of;
    // The protocol-11 comparison tolerance, applied on both sides of the band
    // so one band is not judged with two arithmetics. It is 1e-12 absolute and
    // every quantity here is an integer tick count, so it changes no verdict.
    use crate::fit::solve::SLACK;
    let e = &envelope["envelope"];
    let lo = e["p99_lower"]
        .as_f64()
        .ok_or_else(|| LabError::refusal("envelope p99_lower missing"))?;
    let hi = e["p99"]
        .as_f64()
        .ok_or_else(|| LabError::refusal("envelope p99 missing"))?;
    let p999 = e["p99.9"]
        .as_f64()
        .ok_or_else(|| LabError::refusal("envelope p99.9 missing"))?;
    let max = e["max"]
        .as_f64()
        .ok_or_else(|| LabError::refusal("envelope max missing"))?;
    let mut rows = Vec::new();
    let mut passed = true;
    let mut refs = Vec::new();
    for t in tests {
        let hist: BTreeMap<i64, i64> =
            serde_json::from_value(t.summary["minute_range_ticks_hist"].clone())?;
        let p99 = nearest_rank_of(&hist, 0.99)?;
        let q999 = nearest_rank_of(&hist, 0.999)?;
        // A summary that did not serialize its per-seed maximum is a recorded
        // refusal, not a NaN that quietly compares false.
        let Some(mx) = t.summary["minute_range_max_ticks"].as_f64() else {
            passed = false;
            refs.push(RefusalRec::new(
                "B4",
                format!("seed {}", t.seed),
                "the summary carries no minute_range_max_ticks",
            ));
            continue;
        };
        let ok = p99 as f64 + SLACK >= lo
            && p99 as f64 <= hi + SLACK
            && q999 as f64 <= p999 + SLACK
            && mx <= max + SLACK;
        passed &= ok;
        rows.push(json!({"seed":t.seed,"p99":p99,"p99.9":q999,"max":mx,"passed":ok}));
    }
    Ok(GateRec {
        name: "B4",
        passed: passed && refs.is_empty(),
        evidence: Value::Array(rows),
        refusals: refs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_seed_median_averages_the_two_middle_readings() {
        assert_eq!(
            seed_median(&[Some(4.0), Some(1.0), Some(3.0), Some(2.0)]),
            Some(2.5)
        );
        assert_eq!(seed_median(&[Some(1.0), Some(2.0), Some(3.0), None]), None);
        assert_eq!(
            seed_median(&[Some(1.0), Some(2.0), Some(3.0), Some(f64::NAN)]),
            None
        );
        // The Stage A coarse pass runs two seeds, so the helper had to lose
        // its fixed arity of four. Two readings average; three take the
        // middle; an empty set is `None` rather than a division by zero.
        assert_eq!(seed_median(&[Some(1.0), Some(4.0)]), Some(2.5));
        assert_eq!(seed_median(&[Some(5.0), Some(1.0), Some(3.0)]), Some(3.0));
        assert_eq!(seed_median(&[Some(1.0), None]), None);
        assert_eq!(seed_median(&[]), None);
    }

    #[test]
    fn the_recentred_curve_divides_by_the_ratio_and_sums_to_one() {
        let old = [1.0; 24];
        let ratios = BTreeMap::from([(0, 2.0)]);
        let got = recentred_curve(&old, &ratios);
        assert_eq!(got[1] / got[0], 2.0);
        assert!((crate::kernel::py_sum(got) - 1.0).abs() <= f64::EPSILON);
    }

    #[test]
    fn the_recentred_curve_is_scale_invariant_before_the_rescale() {
        let a = [1.0; 24];
        let b = [17.0; 24];
        let ratios = BTreeMap::from([(3, 0.5), (20, 2.0)]);
        assert_eq!(recentred_curve(&a, &ratios), recentred_curve(&b, &ratios));
    }

    #[test]
    fn the_binding_renders_its_window_length_as_whole_seconds() {
        let whole = GeneratedBinding {
            window_start_ns: 0,
            window_length_ns: 2_674_800_000_000_000,
            burn_in: "3d".into(),
        };
        assert_eq!(whole.length_arg().unwrap(), "2674800s");
        let partial = GeneratedBinding {
            window_length_ns: 1,
            ..whole
        };
        assert!(partial.length_arg().is_err());
    }

    /// The committed observed side, reused as the base for the synthetic
    /// generated contexts below: a control test that judges hand-rolled block
    /// records proves nothing about the shapes a real walk produces.
    fn observed_ctx() -> ObsContext {
        let artifact: Value =
            serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json")).unwrap();
        ObsContext::new(
            artifact["observed"]["per_session"]
                .as_array()
                .unwrap()
                .clone(),
        )
    }

    fn walk_from(ctx: ObsContext, summary: Value) -> ControlWalk {
        ControlWalk {
            seed: 305,
            ctx,
            summary,
            cost_s: 0.0,
        }
    }

    /// A summary whose nearest-rank 0.99 and 0.999 land exactly on the two
    /// requested tick counts: 1000 minutes total, so rank 990 is the single
    /// `p99` entry and rank 999 falls inside the ten-wide `p999` tail.
    fn summary_at(p99: i64, p999: i64, max: i64) -> Value {
        let hist = BTreeMap::from([(1_i64, 989_i64), (p99, 1), (p999, 10)]);
        json!({"minute_range_ticks_hist": hist, "minute_range_max_ticks": max})
    }

    fn envelope_fixture() -> Value {
        json!({"envelope": {"p99_lower": 210, "p99": 250, "p99.9": 399, "max": 968}})
    }

    #[test]
    fn hourly_mean_parents_divides_by_scheduled_not_populated_minutes() {
        // Two sessions, one hour, three block1 rows of which one is the empty
        // minute. The mean is 6 parents over 120 scheduled minutes, not over
        // the rows that happen to carry mass.
        let session = |date: &str| {
            json!({
                "session_date": date,
                "block1_hist": [
                    {"hour": 9, "n": 1, "count": 2},
                    {"hour": 9, "n": 0, "count": 57},
                    {"hour": 9, "n": 1, "count": 1},
                ],
                "block2": {"9": {"60": {"scheduled_windows": 60, "zero_windows": 0}}},
            })
        };
        let ctx = ObsContext::new(vec![session("2026-01-01"), session("2026-01-02")]);
        let rates = hourly_mean_parents(&ctx);
        assert_eq!(rates[&9].parents, 6);
        assert_eq!(rates[&9].scheduled_minutes, 120);
        assert!((rates[&9].mean.unwrap() - 0.05).abs() < 1e-12);
        // An hour the records never mention has no denominator and no mean.
        assert_eq!(rates[&10].mean, None);
    }

    #[test]
    fn an_unexposed_hour_is_not_a_refusal() {
        let obs = observed_ctx();
        let profile = mogwai_venue::config::profile_from_preset("MNQ").unwrap();
        let hours = gate_hours(&profile).unwrap();
        let tests = [walk_from(
            ObsContext::new(obs.per_session().to_vec()),
            Value::Null,
        )];
        for gate in [gate_b6, gate_b7] {
            let rec = gate(&obs, &tests, &hours).unwrap();
            assert!(rec.passed, "{} failed on an identical context", rec.name);
            assert!(rec.refusals.is_empty(), "{} refused an hour", rec.name);
            let cells = rec.evidence.as_array().unwrap();
            assert!(cells.iter().all(|c| c["hour"] != 21));
            assert_eq!(cells.len(), hours.len());
        }
    }

    #[test]
    fn a_gate_with_a_refusal_fails_rather_than_passing_vacuously() {
        let obs = observed_ctx();
        let profile = mogwai_venue::config::profile_from_preset("MNQ").unwrap();
        let hours = gate_hours(&profile).unwrap();

        // B6 and B7: one gate hour's block2 cell removed from every generated
        // session, so no ratio can be formed there.
        let mut holed = obs.per_session().to_vec();
        for rec in &mut holed {
            rec["block2"].as_object_mut().unwrap().remove("9");
        }
        let holed = [walk_from(ObsContext::new(holed), Value::Null)];
        for gate in [gate_b6, gate_b7] {
            let rec = gate(&obs, &holed, &hours).unwrap();
            assert!(!rec.passed, "{} passed vacuously", rec.name);
            assert!(!rec.refusals.is_empty(), "{} recorded no refusal", rec.name);
        }

        // B3: block3 removed, so the strict estimator has no session votes.
        let mut no_b3 = obs.per_session().to_vec();
        for rec in &mut no_b3 {
            rec["block3"] = json!({});
        }
        let no_b3 = [walk_from(ObsContext::new(no_b3), Value::Null)];
        let rec = gate_b3(&obs, &no_b3, &hours).unwrap();
        assert!(!rec.passed);
        assert!(!rec.refusals.is_empty());

        // B4: a summary that serialized no per-seed maximum.
        let mut summary = summary_at(220, 300, 400);
        summary
            .as_object_mut()
            .unwrap()
            .remove("minute_range_max_ticks");
        let no_max = [walk_from(ObsContext::new(Vec::new()), summary)];
        let rec = gate_b4(&envelope_fixture(), &no_max).unwrap();
        assert!(!rec.passed);
        assert!(!rec.refusals.is_empty());

        // B2: a generated side with no parent mass at all cannot support the
        // observed shares, and the substitution says so rather than passing.
        let mut empty = obs.per_session().to_vec();
        for rec in &mut empty {
            rec["block1_hist"] = json!([]);
        }
        let empty = [walk_from(ObsContext::new(empty), Value::Null)];
        let rec = gate_b2(&obs, &empty, &hours).unwrap();
        assert!(!rec.passed);
        assert!(!rec.refusals.is_empty());
    }

    #[test]
    fn gate_b4_is_two_sided() {
        let env = envelope_fixture();
        let below = [walk_from(
            ObsContext::new(Vec::new()),
            summary_at(209, 300, 400),
        )];
        assert!(!gate_b4(&env, &below).unwrap().passed);
        let at_lower = [walk_from(
            ObsContext::new(Vec::new()),
            summary_at(210, 300, 400),
        )];
        assert!(gate_b4(&env, &at_lower).unwrap().passed);
        let at_upper = [walk_from(
            ObsContext::new(Vec::new()),
            summary_at(250, 399, 968),
        )];
        assert!(gate_b4(&env, &at_upper).unwrap().passed);
        let above = [walk_from(
            ObsContext::new(Vec::new()),
            summary_at(251, 300, 400),
        )];
        assert!(!gate_b4(&env, &above).unwrap().passed);
    }

    /// Section 2.5's determinism claim: the two accumulator passes drive one
    /// generator over one tape, so they agree on how many parents they saw.
    /// Run at one day, which is short relative to the artifact run's month but
    /// is not a round number chosen for comfort: it is the shortest window that
    /// measures anything here, and shortening it does not weaken the pin, it
    /// empties it.
    ///
    /// `window_start_ns` is 2026-06-30T22:00:00Z, which is an MNQ session open,
    /// and the accumulator's `per_session` records are emitted per completed
    /// session. So a window that ends mid-session yields no records at all.
    /// Measured, at this start: 1 h, 6 h and 12 h each give zero hours and zero
    /// parents; 24 h gives 23 hours (hour 21 is the unexposed one) and
    /// 1,455,942 parents. The cliff is the session boundary, not a gradient.
    ///
    /// The sibling `arrival_control_refuses_a_tree_that_changed_during_the_run`
    /// argues for a one-hour window, and that argument does not transfer: it
    /// pins step ordering and needs only that the run reach its gates, whereas
    /// this pin's content is that two passes agree over the tape they walked.
    ///
    /// It is not `#[ignore]`d and does not want to be: it measures comfortably
    /// inside the per-test watchdog in every sweep. No number is pinned here on
    /// purpose - a wall stated in a doc comment has to be re-measured forever to
    /// stay true, which is the same ground on which this round deleted a stale
    /// cost string from `protocol9_tape_oracle`.
    #[test]
    fn the_control_walk_pair_replays_one_tape() {
        let binding = GeneratedBinding {
            window_start_ns: 1_782_856_800_000_000_000,
            window_length_ns: 86_400_000_000_000,
            burn_in: "0s".into(),
        };
        // The workspace scratch, not a relative `target/`: a unit test's
        // working directory is its crate, so the relative form wrote
        // `crates/mogwai-lab/target/`, which the root .gitignore hides and
        // `cargo clean` never reaches. Hold the guard - it is what removes the
        // directory, on the panic path included.
        let scratch = crate::storage::unit_test_scratch("arrival-control-walk");
        let walk = control_walk(scratch.path(), &binding, None, 301).unwrap();
        let by_hour = hourly_mean_parents(&walk.ctx);
        // The coverage assertion, not just a nonzero total: a shortened window
        // that still catches one busy hour would satisfy `parents > 0` and
        // quietly reduce this from an agreement-over-the-tape pin to an
        // agreement-over-one-hour pin. The expected count is derived from the
        // preset rather than written down, so a legitimate MNQ calendar change
        // fails at `gate_hours_excludes_the_unexposed_hour`, which owns that
        // fact, instead of failing here behind a message about window length.
        let profile = mogwai_venue::config::profile_from_preset("MNQ").unwrap();
        let exposed = gate_hours(&profile).unwrap().len();
        let covered = by_hour.values().filter(|r| r.parents > 0).count();
        assert_eq!(
            covered, exposed,
            "the walk must cover every exposed hour; a window that does not is \
             a different and weaker pin"
        );
        let acc_parents: i64 = by_hour.values().map(|r| r.parents).sum();
        assert!(acc_parents > 0, "the walk produced no parents");
        // Compared against the summary's session cells, not its top-level
        // parent count. Both passes measure under the lab's session frame,
        // which carves a 15:15 Chicago halt the exchange does not have and
        // the venue's MNQ calendar stopped carving at tape protocol 31; the
        // cells and `GeneratedAcc` both drop a parent that opens in that
        // hole, while the top-level count is the whole tape and now runs
        // ahead by exactly those parents.
        let cell_parents: i64 = walk.summary["session_cells"]
            .as_array()
            .expect("session cells")
            .iter()
            .flat_map(|cell| {
                cell["parent_count_by_hour"]
                    .as_array()
                    .expect("a 24-hour cell")
                    .iter()
                    .map(|count| count.as_i64().expect("a parent count"))
                    .collect::<Vec<_>>()
            })
            .sum();
        assert_eq!(
            acc_parents, cell_parents,
            "the two passes disagree about the tape they walked"
        );
    }

    #[test]
    fn gate_hours_excludes_the_unexposed_hour() {
        let profile = mogwai_venue::config::profile_from_preset("MNQ").unwrap();
        let hours = gate_hours(&profile).unwrap();
        assert_eq!(hours.len(), 23);
        assert!(!hours.contains(&21));
    }

    #[test]
    fn normalizer_drift_is_one_for_a_constant_ratio() {
        let profile = mogwai_venue::config::profile_from_preset("MNQ").unwrap();
        let old = profile.session.intensity_hour;
        let new = old.map(|x| x / 7.0);
        assert!((normalizer_drift(&old, &new, &profile).unwrap() - 1.0).abs() < 1e-12);
    }

    /// This is not a two-copy gate owing a `*_conformance.json`, though it has
    /// been graded as one. `hourly_zero_second_fraction` is the sole
    /// implementation of the quantity; the test recomputes it from a different
    /// field of the committed artifact (production sums the per-hour
    /// `zero_windows`, this sums `count_hist["0"]`), and two independent
    /// producer fields agreeing is an external anchor, not a second copy of
    /// one convention. A fixture would buy nothing the count histogram does
    /// not already buy, because the histogram is not written by the code under
    /// test.
    #[test]
    fn the_zero_fraction_matches_the_count_hist() {
        let artifact: Value =
            serde_json::from_str(include_str!("../../../analysis/mnq-measure-12a.json")).unwrap();
        let ctx = ObsContext::new(
            artifact["observed"]["per_session"]
                .as_array()
                .unwrap()
                .clone(),
        );
        let direct = hourly_zero_second_fraction(&ctx);
        for hour in 0..24_i64 {
            let mut zero = 0_i64;
            let mut scheduled = 0_i64;
            let mut missing = false;
            for rec in ctx.per_session() {
                match rec["block2"].get(hour.to_string()).and_then(|v| v.get("1")) {
                    Some(cell) => {
                        zero += cell["count_hist"]["0"].as_i64().unwrap_or(0);
                        scheduled += cell["scheduled_windows"].as_i64().unwrap();
                    }
                    None => missing = true,
                }
            }
            let histogram = (!missing && scheduled > 0).then_some(zero as f64 / scheduled as f64);
            assert_eq!(direct[&hour], histogram);
        }
    }
}
