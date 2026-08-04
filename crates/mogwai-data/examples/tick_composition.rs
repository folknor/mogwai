// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Protocol composition and budget-denomination measurement for the BBO layer.

use std::{collections::BTreeMap, env, fs, path::PathBuf};

use mogwai_data::{Fingerprint, GeneratedSource, GeneratorScalars, TickEvent, TickSource};
use rust_decimal::Decimal;
use serde::Serialize;

const DEFAULT_PARENTS: usize = 2_000_000;
const VOL_WINDOW_SECS: u64 = 300;
const WARMUP_SECS: u64 = 86_400;
const WALL_HORIZON_SECS: u64 = 600;
const MAX_MEASURED_SPEED: u64 = 10;

#[derive(Clone, Copy)]
enum Mode {
    Quiet,
    Active,
    Natural,
    Surged,
}

impl Mode {
    fn label(self) -> &'static str {
        match self {
            Self::Quiet => "quiet",
            Self::Active => "active",
            Self::Natural => "natural",
            Self::Surged => "surged",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct Tail {
    p999: f64,
    max: f64,
}

#[derive(Debug, Clone, Serialize)]
struct Reading {
    preset: String,
    seed: u64,
    configuration: String,
    parents: usize,
    ticks_per_parent: Tail,
    ticks_per_vol_window: Tail,
    ticks_per_sim_second: Tail,
    ticks_per_warmup: Tail,
    frames_per_wall_second: BTreeMap<String, Tail>,
}

#[derive(Serialize)]
struct Report {
    tape_protocol_version: u32,
    measured_from_protocol_7_stream: bool,
    projection: String,
    parent_events_per_combination: usize,
    entries: Vec<Reading>,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args().skip(1);
    let mut out = None;
    let mut parents = DEFAULT_PARENTS;
    let mut protocol = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--out" => out = args.next().map(PathBuf::from),
            "--parents" => parents = args.next().ok_or("missing --parents value")?.parse()?,
            "--protocol" => {
                let value: u32 = args.next().ok_or("missing --protocol value")?.parse()?;
                if !matches!(value, 6 | 7) {
                    return Err("--protocol must be 6 or 7".into());
                }
                protocol = Some(value);
            }
            other => return Err(format!("unknown argument {other}").into()),
        }
    }
    let out = out.ok_or("--out PATH is required")?;
    let protocol = protocol.ok_or("--protocol 6|7 is required")?;
    let include_quotes = protocol == 7;
    let fp = Fingerprint::from_repo_json();
    let entries = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for seed in 1..=8 {
            for mode in [Mode::Quiet, Mode::Active, Mode::Natural, Mode::Surged] {
                let fp = &fp;
                handles.push(scope.spawn(move || {
                    ["MNQ", "MES", "BTCUSDT", "ETHUSDT", "SOLUSDT"]
                        .into_iter()
                        .map(|preset| {
                            measure(
                                preset,
                                preset_scalars(preset, fp),
                                seed,
                                mode,
                                parents,
                                include_quotes,
                                fp,
                            )
                        })
                        .collect::<Vec<_>>()
                }));
            }
        }
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("composition worker panicked"))
            .collect::<Vec<_>>()
    });
    let report = Report {
        tape_protocol_version: protocol,
        measured_from_protocol_7_stream: true,
        projection: if include_quotes {
            "all protocol-7 frames".into()
        } else {
            "protocol-6 count projection: trade frames only; protocol 7 quote placement consumes no randomness and does not change timestamps or child counts".into()
        },
        parent_events_per_combination: parents,
        entries,
    };
    fs::write(out, serde_json::to_vec_pretty(&report)?)?;
    Ok(())
}

fn preset_scalars(name: &str, fp: &Fingerprint) -> GeneratorScalars {
    let mut scalars = GeneratorScalars::from_fingerprint_medians(name, fp);
    match name {
        "MNQ" => {
            scalars.modal_tick = Decimal::new(25, 2);
            scalars.price_decimals = 2;
            scalars.start_price = Decimal::from(21_000);
            scalars.latent_size_median = Decimal::ONE;
        }
        "MES" => {
            scalars.modal_tick = Decimal::new(25, 2);
            scalars.price_decimals = 2;
            scalars.start_price = Decimal::from(6_000);
            scalars.latent_size_median = Decimal::ONE;
        }
        _ => {
            scalars.modal_tick = Decimal::new(1, 2);
            scalars.price_decimals = 2;
        }
    }
    scalars
}

fn measure(
    preset: &str,
    scalars: GeneratorScalars,
    seed: u64,
    mode: Mode,
    parents: usize,
    include_quotes: bool,
    fp: &Fingerprint,
) -> Reading {
    let peak_hour = fp
        .session_profile
        .intensity_hour
        .iter()
        .enumerate()
        .max_by(|left, right| left.1.total_cmp(right.1))
        .map_or(0, |(hour, _)| hour as u64);
    let start_ns = peak_hour * 3_600_000_000_000;
    let fanout_end_ns =
        start_ns.saturating_add(WALL_HORIZON_SECS * MAX_MEASURED_SPEED * 1_000_000_000);
    let mut source = GeneratedSource::new(scalars, seed, start_ns, fp, None);
    match mode {
        Mode::Quiet => source.set_arrival_quiet_for_measurement(Some(true)),
        Mode::Active => source.set_arrival_quiet_for_measurement(Some(false)),
        Mode::Natural => {}
        Mode::Surged => source.arm_flow_surge(start_ns, u64::MAX, 1_000.0, 100.0),
    }
    let mut per_parent = Vec::with_capacity(parents);
    let mut per_second: BTreeMap<u64, u64> = BTreeMap::new();
    let mut fanout_second: BTreeMap<u64, u64> = BTreeMap::new();
    let mut current = 0_u64;
    let mut seen_parents = 0_usize;
    loop {
        let event = source.next_tick().expect("generated source is infinite");
        if matches!(event, TickEvent::Quote(_)) {
            if seen_parents > 0 && per_parent.len() < parents {
                per_parent.push(current);
            }
            if per_parent.len() == parents && event.ts_event() >= fanout_end_ns {
                break;
            }
            seen_parents += 1;
            current = 0;
        }
        if include_quotes || matches!(event, TickEvent::Trade(_)) {
            if per_parent.len() < parents {
                current += 1;
                *per_second
                    .entry(event.ts_event() / 1_000_000_000)
                    .or_default() += 1;
            }
            if event.ts_event() < fanout_end_ns {
                *fanout_second
                    .entry(event.ts_event() / 1_000_000_000)
                    .or_default() += 1;
            }
        }
    }
    let seconds = dense_seconds(&per_second);
    let sim_counts: Vec<u64> = seconds.iter().map(|entry| entry.1).collect();
    let vol_counts = rolling_counts(&seconds, VOL_WINDOW_SECS);
    let warmup_counts = rolling_counts(&seconds, WARMUP_SECS);
    let frames_per_wall_second = [1_u64, 10]
        .into_iter()
        .map(|speed| {
            let mut bins: BTreeMap<u64, u64> = BTreeMap::new();
            for (sim_second, count) in &fanout_second {
                *bins
                    .entry((*sim_second - start_ns / 1_000_000_000) / speed)
                    .or_default() += count;
            }
            let values: Vec<u64> = (0..WALL_HORIZON_SECS)
                .map(|wall_second| bins.get(&wall_second).copied().unwrap_or_default())
                .collect();
            (format!("{speed}.0"), tail(&values))
        })
        .collect();
    Reading {
        preset: preset.into(),
        seed,
        configuration: mode.label().into(),
        parents,
        ticks_per_parent: tail(&per_parent),
        ticks_per_vol_window: tail(&vol_counts),
        ticks_per_sim_second: tail(&sim_counts),
        ticks_per_warmup: tail(&warmup_counts),
        frames_per_wall_second,
    }
}

fn dense_seconds(sparse: &BTreeMap<u64, u64>) -> Vec<(u64, u64)> {
    let Some((&first, _)) = sparse.first_key_value() else {
        return Vec::new();
    };
    let last = *sparse.last_key_value().expect("non-empty map").0;
    (first..=last)
        .map(|second| (second, sparse.get(&second).copied().unwrap_or_default()))
        .collect()
}

fn rolling_counts(seconds: &[(u64, u64)], width: u64) -> Vec<u64> {
    let mut out = Vec::with_capacity(seconds.len());
    let mut start = 0;
    let mut sum = 0_u64;
    for end in 0..seconds.len() {
        sum += seconds[end].1;
        while seconds[start].0 + width <= seconds[end].0 {
            sum -= seconds[start].1;
            start += 1;
        }
        out.push(sum);
    }
    out
}

fn tail(values: &[u64]) -> Tail {
    if values.is_empty() {
        return Tail {
            p999: 0.0,
            max: 0.0,
        };
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() - 1) as f64 * 0.999;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;
    let fraction = rank - lower as f64;
    let p999 = sorted[lower] as f64 + (sorted[upper] as f64 - sorted[lower] as f64) * fraction;
    Tail {
        p999,
        max: *sorted.last().unwrap() as f64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p999_interpolates_instead_of_collapsing_to_the_maximum() {
        let values = (1..=600).collect::<Vec<_>>();
        let result = tail(&values);
        assert_eq!(result.p999, 599.401);
        assert_eq!(result.max, 600.0);
    }

    #[test]
    fn dense_seconds_represents_quiet_seconds_as_zero() {
        let sparse = BTreeMap::from([(10, 3), (12, 4)]);
        assert_eq!(dense_seconds(&sparse), vec![(10, 3), (11, 0), (12, 4)]);
    }
}
