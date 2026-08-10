// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Reproducible quick and full panels for Stage A throughput work.

use std::path::{Path, PathBuf};
use std::time::Instant;

use mogwai_lab::arrival_screen::{
    Family, STAGE_A_BUDGET_S, ScheduledCell, ScreenContext, evaluate_cell_with_work,
    evaluate_cells_parallel,
};
use mogwai_lab::ledger::sha256_file;
use mogwai_lab::sampler::ResourceSampler;
use mogwai_lab::sidecar;
use mogwai_lab::stage_a_batch::{
    BatchManifest, PILOT_SCHEMA_VERSION, PILOT_SELECTION_SEED, PILOT_WALK_SEED, PanelCell,
    PilotArtifact, PilotReading, SampleKind, build_manifest, parse_manifest, pilot_plan,
    resolve_cell,
};

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOC: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

const MEASURE: &str = "analysis/mnq-measure-12a.json";
const MANIFEST: &str = "analysis/stage-a-batch-manifest.json";
const PILOT: &str = "target/stage-a-batch-pilot.json";
const FROZEN_PILOT: &str = "analysis/stage-a-batch-pilot.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Pilot,
    Manifest,
    Quick,
    Full,
}

#[derive(Debug)]
struct Args {
    mode: Mode,
    jobs: usize,
    measure: PathBuf,
    manifest: PathBuf,
    pilot: PathBuf,
    output: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct CellRun {
    panel: PanelCell,
    cost_s: f64,
    parents: u64,
    prints: u64,
    admissible: bool,
    refused: bool,
}

#[derive(Debug, Clone, Copy, Default)]
struct Estimate {
    maximum_cap_mean_serial_cpu_s: f64,
    maximum_cap_p90_serial_cpu_s: f64,
    maximum_cap_mean_scheduled_wall_s: f64,
    maximum_cap_p90_scheduled_wall_s: f64,
    maximum_cap_p90_tail_utilization: f64,
}

#[derive(Debug, Clone, Copy)]
enum CostStatistic {
    Mean,
    P90,
}

#[derive(Debug, Clone, Copy, Default)]
struct ValidationCoverage {
    comparable_strata: usize,
    singleton_strata: usize,
    half_a_weighted_s: f64,
    half_b_weighted_s: f64,
}

fn usage() -> &'static str {
    "usage: stage_a_batch_bench <pilot|manifest|quick|full> [--jobs N] \
     [--measure PATH] [--manifest PATH] [--pilot PATH] [--output PATH]"
}

fn parse_args() -> Result<Args, String> {
    let mut values = std::env::args().skip(1);
    let mode = match values.next().as_deref() {
        Some("pilot") => Mode::Pilot,
        Some("manifest") => Mode::Manifest,
        Some("quick") => Mode::Quick,
        Some("full") => Mode::Full,
        Some("--help" | "-h") => {
            println!("{}", usage());
            std::process::exit(0);
        }
        _ => return Err(usage().into()),
    };
    let mut args = Args {
        mode,
        jobs: std::thread::available_parallelism().map_or(1, usize::from),
        measure: PathBuf::from(MEASURE),
        manifest: PathBuf::from(MANIFEST),
        pilot: PathBuf::from(PILOT),
        output: None,
    };
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--jobs" => {
                args.jobs = value
                    .parse()
                    .map_err(|_| format!("invalid --jobs value {value}"))?;
                if args.jobs == 0 {
                    return Err("--jobs must be positive".into());
                }
            }
            "--measure" => args.measure = value.into(),
            "--manifest" => args.manifest = value.into(),
            "--pilot" => args.pilot = value.into(),
            "--output" => args.output = Some(value.into()),
            _ => return Err(format!("unknown argument {flag}\n{}", usage())),
        }
    }
    Ok(args)
}

fn run_pilot(args: &Args) -> Result<(), String> {
    let measurement_sha256 = sha256_file(&args.measure).map_err(|error| error.to_string())?;
    let context = ScreenContext::open(&args.measure, None)
        .map_err(|error| error.to_string())?
        .measured();
    let plan = pilot_plan();
    let started = Instant::now();
    let mut readings = Vec::with_capacity(plan.len());
    for (index, planned) in plan.iter().enumerate() {
        let cell = resolve_cell(planned.family, planned.level, &planned.lattice)
            .map_err(|error| error.to_string())?;
        let evaluation = evaluate_cell_with_work(&context, &cell, &[PILOT_WALK_SEED])
            .map_err(|error| error.to_string())?;
        readings.push(PilotReading {
            family: planned.family,
            level: planned.level,
            lattice: planned.lattice.clone(),
            stratum: planned.stratum.clone(),
            cost_s: evaluation.verdict.cost_s,
            parents: evaluation.parents,
            prints: evaluation.prints,
        });
        eprintln!(
            "pilot_cell={}/{} family={} level={} cost_s={:.6}",
            index + 1,
            plan.len(),
            planned.family.as_str(),
            planned.level,
            evaluation.verdict.cost_s
        );
    }
    let artifact = PilotArtifact {
        schema_version: PILOT_SCHEMA_VERSION,
        selection_seed: PILOT_SELECTION_SEED,
        walk_seed: PILOT_WALK_SEED,
        measurement_sha256,
        readings,
    };
    let output = args.output.as_deref().unwrap_or(&args.pilot);
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&artifact).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    eprintln!("pilot_cells={}", artifact.readings.len());
    eprintln!("elapsed_s={:.6}", started.elapsed().as_secs_f64());
    eprintln!("pilot_output={}", output.display());
    Ok(())
}

fn generate_manifest(args: &Args) -> Result<(), String> {
    let measurement_sha256 = sha256_file(&args.measure).map_err(|error| error.to_string())?;
    let pilot_bytes = std::fs::read(&args.pilot).map_err(|error| error.to_string())?;
    let pilot: PilotArtifact =
        serde_json::from_slice(&pilot_bytes).map_err(|error| error.to_string())?;
    let manifest = build_manifest(measurement_sha256, &pilot).map_err(|error| error.to_string())?;
    let output = args.output.as_deref().unwrap_or(&args.manifest);
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&manifest).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    if args.output.is_none() {
        std::fs::write(
            FROZEN_PILOT,
            serde_json::to_vec(&pilot).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        eprintln!("pilot_artifact={FROZEN_PILOT}");
    }
    eprintln!("panel_sha256={}", manifest.plan_sha256);
    eprintln!("quick_cells={}", manifest.quick.len());
    eprintln!("full_cells={}", manifest.full.len());
    eprintln!("manifest_output={}", output.display());
    Ok(())
}

fn load_manifest(path: &Path, measure: &Path) -> Result<BatchManifest, String> {
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    let manifest = parse_manifest(&bytes).map_err(|error| error.to_string())?;
    let measurement_sha256 = sha256_file(measure).map_err(|error| error.to_string())?;
    if manifest.measurement_sha256 != measurement_sha256 {
        return Err(format!(
            "manifest measurement {} does not match {}",
            manifest.measurement_sha256, measurement_sha256
        ));
    }
    if manifest.tape_protocol_version != mogwai_data::TAPE_PROTOCOL_VERSION
        || manifest.arrival_kernel_version != mogwai_data::ARRIVAL_KERNEL_VERSION
    {
        return Err("manifest generator identity does not match this binary".into());
    }
    Ok(manifest)
}

fn run_panel(args: &Args, manifest: &BatchManifest) -> Result<Vec<CellRun>, String> {
    let panel = match args.mode {
        Mode::Quick => manifest.quick.clone(),
        Mode::Full => manifest.full.clone(),
        Mode::Pilot | Mode::Manifest => unreachable!(),
    };
    let jobs = args.jobs.min(panel.len());
    let context = ScreenContext::open(&args.measure, None)
        .map_err(|error| error.to_string())?
        .measured();
    let projection = context
        .parallel_projection()
        .map_err(|error| error.to_string())?;
    let scheduled: Vec<_> = panel
        .iter()
        .map(|cell| ScheduledCell {
            cell: cell.cell.clone(),
            seeds: cell.seeds.clone(),
        })
        .collect();
    let evaluations = evaluate_cells_parallel(&context, &projection, &scheduled, jobs, None)
        .map_err(|error| error.to_string())?;
    Ok(panel
        .into_iter()
        .zip(evaluations)
        .map(|(panel, evaluation)| CellRun {
            panel,
            cost_s: evaluation.verdict.cost_s,
            parents: evaluation.parents,
            prints: evaluation.prints,
            admissible: evaluation.verdict.admissible,
            refused: !evaluation.verdict.refusals.is_empty(),
        })
        .collect())
}

fn p90(costs: &[f64]) -> f64 {
    let mut ordered = costs.to_vec();
    ordered.sort_by(f64::total_cmp);
    let rank = (ordered.len() * 9).div_ceil(10).saturating_sub(1);
    ordered[rank]
}

fn synthetic_level_tasks(
    results: &[CellRun],
    manifest: &BatchManifest,
    family: Family,
    level: u8,
    statistic: CostStatistic,
) -> Vec<f64> {
    let mut tasks = Vec::new();
    for stratum in manifest
        .strata
        .iter()
        .filter(|stratum| stratum.id.family == family && stratum.id.level == level)
    {
        let mut anchors: Vec<_> = results
            .iter()
            .filter(|run| run.panel.stratum == stratum.id && run.panel.kind == SampleKind::Anchor)
            .collect();
        anchors.sort_by(|left, right| left.panel.lattice.cmp(&right.panel.lattice));
        tasks.extend(anchors.into_iter().map(|run| run.cost_s));

        let mut samples: Vec<_> = results
            .iter()
            .filter(|run| {
                run.panel.stratum == stratum.id && run.panel.kind == SampleKind::Probability
            })
            .collect();
        samples.sort_by(|left, right| left.panel.lattice.cmp(&right.panel.lattice));
        let sample_costs: Vec<_> = samples.iter().map(|run| run.cost_s).collect();
        let count = usize::try_from(stratum.probability_population_size)
            .expect("probability population fits usize");
        match statistic {
            CostStatistic::Mean => {
                for index in 0..count {
                    tasks.push(sample_costs[index % sample_costs.len()]);
                }
            }
            CostStatistic::P90 => {
                tasks.extend(std::iter::repeat_n(p90(&sample_costs), count));
            }
        }
    }
    tasks
}

fn representative_tasks(population: &[f64], count: usize) -> Vec<f64> {
    if count == 0 {
        return Vec::new();
    }
    (0..count)
        .map(|index| population[index * population.len() / count])
        .collect()
}

fn scheduled_wall(tasks: &[f64], jobs: usize) -> f64 {
    let mut workers = vec![0.0f64; jobs];
    for &duration in tasks {
        let worker = workers
            .iter()
            .enumerate()
            .min_by(|left, right| left.1.total_cmp(right.1).then(left.0.cmp(&right.0)))
            .map(|(index, _)| index)
            .expect("at least one worker");
        workers[worker] += duration;
    }
    workers.into_iter().max_by(f64::total_cmp).unwrap_or(0.0)
}

fn estimate_statistic(
    results: &[CellRun],
    manifest: &BatchManifest,
    jobs: usize,
    statistic: CostStatistic,
) -> (f64, f64, f64) {
    let mut coarse_tasks = Vec::new();
    for family in Family::ALL {
        coarse_tasks.extend(synthetic_level_tasks(
            results, manifest, family, 0, statistic,
        ));
    }
    let mut serial_cpu_s: f64 = coarse_tasks.iter().sum();
    let mut scheduled_work_s = serial_cpu_s;
    let mut scheduled_wall_s = scheduled_wall(&coarse_tasks, jobs);

    // The frozen cap is TOTAL across the two rounds. Each family is replayed
    // independently across every feasible split, preserving its round
    // barrier. Summing the four worst family replays also preserves the
    // current driver's family serialization, so the budget-facing result is
    // conservative relative to a future cross-family scheduler.
    for family in Family::ALL {
        let cap = usize::try_from(
            manifest
                .refinement_caps
                .iter()
                .find(|entry| entry.family == family)
                .expect("every family has a cap")
                .cap,
        )
        .expect("refinement cap fits usize");
        let level_1 = synthetic_level_tasks(results, manifest, family, 1, statistic);
        let level_2 = synthetic_level_tasks(results, manifest, family, 2, statistic);
        let mut worst_serial = 0.0f64;
        let mut worst_wall = 0.0f64;
        let mut worst_wall_work = 0.0f64;
        let minimum_round_1 = cap.saturating_sub(level_2.len());
        let maximum_round_1 = cap.min(level_1.len());
        for round_1_count in minimum_round_1..=maximum_round_1 {
            let round_1 = representative_tasks(&level_1, round_1_count);
            let round_2 = representative_tasks(&level_2, cap - round_1_count);
            let work = round_1.iter().chain(&round_2).sum::<f64>();
            let wall = scheduled_wall(&round_1, jobs) + scheduled_wall(&round_2, jobs);
            worst_serial = worst_serial.max(work);
            if wall > worst_wall {
                worst_wall = wall;
                worst_wall_work = work;
            }
        }
        serial_cpu_s += worst_serial;
        scheduled_work_s += worst_wall_work;
        scheduled_wall_s += worst_wall;
    }
    let jobs = f64::from(u32::try_from(jobs).expect("worker count fits u32"));
    let tail_utilization = scheduled_work_s / (scheduled_wall_s * jobs);
    (serial_cpu_s, scheduled_wall_s, tail_utilization)
}

fn estimate(results: &[CellRun], manifest: &BatchManifest, jobs: usize) -> Estimate {
    let (mean_serial, mean_wall, _) =
        estimate_statistic(results, manifest, jobs, CostStatistic::Mean);
    let (p90_serial, p90_wall, p90_utilization) =
        estimate_statistic(results, manifest, jobs, CostStatistic::P90);
    Estimate {
        maximum_cap_mean_serial_cpu_s: mean_serial,
        maximum_cap_p90_serial_cpu_s: p90_serial,
        maximum_cap_mean_scheduled_wall_s: mean_wall,
        maximum_cap_p90_scheduled_wall_s: p90_wall,
        maximum_cap_p90_tail_utilization: p90_utilization,
    }
}

fn validation_coverage(results: &[CellRun], manifest: &BatchManifest) -> ValidationCoverage {
    let mut validation = ValidationCoverage::default();
    for stratum in &manifest.strata {
        let mut samples: Vec<_> = results
            .iter()
            .filter(|run| {
                run.panel.stratum == stratum.id && run.panel.kind == SampleKind::Probability
            })
            .collect();
        samples.sort_by(|left, right| left.panel.lattice.cmp(&right.panel.lattice));
        if samples.len() < 2 {
            validation.singleton_strata += 1;
            continue;
        }
        validation.comparable_strata += 1;
        let mut half_a = Vec::new();
        let mut half_b = Vec::new();
        for (index, sample) in samples.into_iter().enumerate() {
            if index % 2 == 0 {
                half_a.push(sample.cost_s);
            } else {
                half_b.push(sample.cost_s);
            }
        }
        let population = f64::from(stratum.probability_population_size);
        validation.half_a_weighted_s +=
            population * half_a.iter().sum::<f64>() / half_a.len() as f64;
        validation.half_b_weighted_s +=
            population * half_b.iter().sum::<f64>() / half_b.len() as f64;
    }
    validation
}

fn report_panel(
    args: &Args,
    manifest: &BatchManifest,
    results: &[CellRun],
    elapsed_s: f64,
    peak_rss: u64,
) {
    let mode = if args.mode == Mode::Quick {
        "quick"
    } else {
        "full"
    };
    let task_cpu_s: f64 = results.iter().map(|run| run.cost_s).sum();
    let seed_walks: usize = results.iter().map(|run| run.panel.seeds.len()).sum();
    let parents: u64 = results.iter().map(|run| run.parents).sum();
    let prints: u64 = results.iter().map(|run| run.prints).sum();
    let inadmissible = results.iter().filter(|run| !run.admissible).count();
    let refusals = results.iter().filter(|run| run.refused).count();

    sidecar::kv("panel_sha256", &manifest.plan_sha256);
    sidecar::kv("tape_protocol_version", manifest.tape_protocol_version);
    sidecar::kv("arrival_kernel_version", manifest.arrival_kernel_version);
    sidecar::kv("instrument_set", instrument_set());
    sidecar::kv("panel_mode", mode);
    sidecar::kv("jobs", args.jobs.min(results.len()));
    sidecar::kv("cells", results.len());
    sidecar::kv("seed_walks", seed_walks);
    sidecar::report("parents", i64::try_from(parents).expect("parents fit"));
    sidecar::report("prints", i64::try_from(prints).expect("prints fit"));
    sidecar::kv("inadmissible_cells", inadmissible);
    sidecar::kv("refused_cells", refusals);
    sidecar::kv("wall_s", format_args!("{elapsed_s:.6}"));
    sidecar::kv("task_cpu_s", format_args!("{task_cpu_s:.6}"));
    sidecar::kv(
        "effective_concurrency",
        format_args!("{:.6}", task_cpu_s / elapsed_s),
    );
    sidecar::kv("peak_rss_bytes", peak_rss);

    for family in Family::ALL {
        for level in 0..=2 {
            let group: Vec<_> = results
                .iter()
                .filter(|run| run.panel.family == family && run.panel.level == level)
                .collect();
            let prefix = format!("{}_level_{level}", family.as_str());
            let group_seed_walks = group.iter().map(|run| run.panel.seeds.len()).sum::<usize>();
            let group_parents = group.iter().map(|run| run.parents).sum::<u64>();
            let group_cpu_s = group.iter().map(|run| run.cost_s).sum::<f64>();
            sidecar::kv(&format!("{prefix}_cells"), group.len());
            sidecar::kv(&format!("{prefix}_seed_walks"), group_seed_walks);
            sidecar::kv(&format!("{prefix}_parents"), group_parents);
            sidecar::kv(
                &format!("{prefix}_prints"),
                group.iter().map(|run| run.prints).sum::<u64>(),
            );
            sidecar::kv(
                &format!("{prefix}_task_cpu_s"),
                format_args!("{group_cpu_s:.6}"),
            );
            if group_parents > 0 {
                sidecar::kv(
                    &format!("{prefix}_s_per_parent"),
                    format_args!("{:.12}", group_cpu_s / group_parents as f64),
                );
            }
            if group_seed_walks > 0 {
                sidecar::kv(
                    &format!("{prefix}_s_per_seed_walk"),
                    format_args!("{:.9}", group_cpu_s / group_seed_walks as f64),
                );
            }
        }
    }

    if args.mode == Mode::Full {
        let jobs = args.jobs.min(results.len());
        let estimate = estimate(results, manifest, jobs);
        sidecar::kv(
            "maximum_cap_mean_serial_cpu_s",
            format_args!("{:.6}", estimate.maximum_cap_mean_serial_cpu_s),
        );
        sidecar::kv(
            "maximum_cap_p90_serial_cpu_s",
            format_args!("{:.6}", estimate.maximum_cap_p90_serial_cpu_s),
        );
        sidecar::kv(
            "maximum_cap_mean_scheduled_wall_s",
            format_args!("{:.6}", estimate.maximum_cap_mean_scheduled_wall_s),
        );
        sidecar::kv(
            "maximum_cap_p90_scheduled_wall_s",
            format_args!("{:.6}", estimate.maximum_cap_p90_scheduled_wall_s),
        );
        sidecar::kv(
            "maximum_cap_p90_tail_utilization",
            format_args!("{:.6}", estimate.maximum_cap_p90_tail_utilization),
        );
        sidecar::kv("stage_a_budget_s", STAGE_A_BUDGET_S);
        sidecar::kv(
            "maximum_cap_budget_margin_s",
            format_args!(
                "{:.6}",
                STAGE_A_BUDGET_S - estimate.maximum_cap_p90_scheduled_wall_s
            ),
        );
        sidecar::kv(
            "maximum_cap_budget_fraction",
            format_args!(
                "{:.6}",
                estimate.maximum_cap_p90_scheduled_wall_s / STAGE_A_BUDGET_S
            ),
        );
        let validation = validation_coverage(results, manifest);
        sidecar::kv("validation_comparable_strata", validation.comparable_strata);
        sidecar::kv("validation_singleton_strata", validation.singleton_strata);
        sidecar::kv("design_based_interval_available", 0);
        if validation.comparable_strata > 0 {
            let relative_gap = (validation.half_a_weighted_s - validation.half_b_weighted_s).abs()
                / ((validation.half_a_weighted_s + validation.half_b_weighted_s) * 0.5);
            sidecar::kv(
                "validation_comparable_half_a_weighted_s",
                format_args!("{:.6}", validation.half_a_weighted_s),
            );
            sidecar::kv(
                "validation_comparable_half_b_weighted_s",
                format_args!("{:.6}", validation.half_b_weighted_s),
            );
            sidecar::kv(
                "validation_comparable_half_relative_gap",
                format_args!("{relative_gap:.6}"),
            );
        }
    }
}

const fn instrument_set() -> &'static str {
    if cfg!(feature = "hotpath-alloc") {
        "hotpath_alloc"
    } else if cfg!(feature = "hotpath") {
        "hotpath"
    } else {
        "none"
    }
}

fn benchmark(args: &Args) -> Result<(), String> {
    let manifest = load_manifest(&args.manifest, &args.measure)?;
    let sampler = ResourceSampler::start(Vec::new(), None);
    sidecar::marker(if args.mode == Mode::Quick {
        "quick"
    } else {
        "full"
    });
    let started = Instant::now();
    let results = run_panel(args, &manifest)?;
    let elapsed_s = started.elapsed().as_secs_f64();
    let (peak_rss, _) = sampler.stop(&[], None).map_err(|error| error.to_string())?;
    report_panel(args, &manifest, &results, elapsed_s, peak_rss);
    Ok(())
}

fn main() {
    sidecar::init();
    #[cfg(feature = "hotpath")]
    let _guard = hotpath::HotpathGuardBuilder::new("main").build();

    let result = parse_args().and_then(|args| match args.mode {
        Mode::Pilot => run_pilot(&args),
        Mode::Manifest => generate_manifest(&args),
        Mode::Quick | Mode::Full => benchmark(&args),
    });
    if let Err(error) = result {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
