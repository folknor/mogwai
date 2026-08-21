// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai measure`: slice 2c-ii of the retired rewrite plan, ported
//! from `analysis/mnq_fit.py`'s `mode_measure12a`. The live Brick M run -
//! the observed pass over the delivered corpus, the eight FINAL walks run
//! IN-PROCESS through the lab engine and content-compared (cost excluded)
//! against the read-only Brick G cache, phase-2b/2c-i aggregation and
//! assembly, both validators, the fresh-tree gate and the atomic artifact
//! write.
//!
//! Lives here rather than in `mogwai-lab` because the generated side needs
//! `mogwai-venue` preset resolution, same reason `crates/mogwai-cli/tests/
//! parity12a.rs`'s generated gate lives in this crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, anyhow, bail};
use clap::Args;
use mogwai_data::{TickEvent, TickSource};
use mogwai_lab::aggregate::artifact::{
    GeneratedSeed, assemble_measure12a_artifact, load_brick_g_walks, measure12a_schema_errors,
    measure12a_semantic_errors, write_json_atomic,
};
use mogwai_lab::aggregate::bootstrap::bootstrap_multiplicities;
use mogwai_lab::kernel::typed_canon;
use mogwai_lab::ledger::{fresh_tree_state, require_clean_tree, verify_input};
use mogwai_lab::measure12a::generated::GeneratedAcc;
use mogwai_lab::measure12a::observed;
use mogwai_lab::preflight::require_preflight;
use mogwai_lab::sampler::ResourceSampler;
use mogwai_lab::stream::{data_files, parse_stream};
use mogwai_lab::subcontract::{
    FINAL_END_NS, FINAL_LENGTH, FINAL_SEEDS, FINAL_START_NS, SUMMARY_WARMUP,
};
use serde_json::Value;

const DEFAULT_CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const DEFAULT_LEDGER: &str = "analysis/databento-jobs.json";
const DEFAULT_PREFLIGHT: &str = "analysis/out/mnq-fit-preflight.json";
const DEFAULT_CACHE_DIR: &str = "analysis/out";
const DEFAULT_OUT: &str = "analysis/mnq-measure-12a.json";
const WALK_CACHE_SUBDIR: &str = "measure12a-cache";
const OBSERVED_CACHE_NAME: &str = "mnq-measure12a-observed.json";

#[derive(Args)]
pub struct MeasureArgs {
    /// The delivered corpus directory.
    #[arg(long, value_name = "DIR")]
    corpus: Option<PathBuf>,
    /// The Databento job ledger. Read-only.
    #[arg(long, value_name = "PATH")]
    ledger: Option<PathBuf>,
    /// The committed preflight artifact this run's file hashes must match.
    #[arg(long, value_name = "PATH")]
    preflight: Option<PathBuf>,
    /// The cache root carrying the Brick G walk cache
    /// (`<dir>/measure12a-cache/`) and the observed cross-check cache
    /// (`<dir>/mnq-measure12a-observed.json`). Defaults to the historical
    /// `analysis/out` layout when run in-repo, else the phase-1 storage
    /// cache root.
    #[arg(long, value_name = "DIR")]
    cache_dir: Option<PathBuf>,
    /// Where to write the section-10 artifact. An ARTIFACT (storage
    /// policy): never cached, never auto-deleted.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

pub struct MeasureConfig {
    pub corpus: PathBuf,
    pub ledger: PathBuf,
    pub preflight_path: PathBuf,
    pub walk_cache_dir: PathBuf,
    pub observed_cache_path: PathBuf,
    pub out: PathBuf,
}

impl MeasureConfig {
    #[must_use]
    pub fn resolve(
        corpus: Option<PathBuf>,
        ledger: Option<PathBuf>,
        preflight: Option<PathBuf>,
        cache_dir: Option<PathBuf>,
        out: Option<PathBuf>,
    ) -> Self {
        let cache_dir = cache_dir.unwrap_or_else(|| {
            let repo_default = PathBuf::from(DEFAULT_CACHE_DIR);
            if repo_default.is_dir() {
                repo_default
            } else {
                mogwai_lab::storage::cache_root(None).join("measure12a")
            }
        });
        Self {
            corpus: corpus.unwrap_or_else(|| PathBuf::from(DEFAULT_CORPUS)),
            ledger: ledger.unwrap_or_else(|| PathBuf::from(DEFAULT_LEDGER)),
            preflight_path: preflight.unwrap_or_else(|| PathBuf::from(DEFAULT_PREFLIGHT)),
            walk_cache_dir: cache_dir.join(WALK_CACHE_SUBDIR),
            observed_cache_path: cache_dir.join(OBSERVED_CACHE_NAME),
            out: out.unwrap_or_else(|| PathBuf::from(DEFAULT_OUT)),
        }
    }
}

pub struct MeasureOutcome {
    pub artifact: Value,
    pub cost: Value,
    pub work: MeasureWork,
}

/// The run's work size, in the units the measured phases actually scale with.
///
/// Carried out of the run rather than recovered from the artifact because the
/// artifact does not keep it: `generated.per_seed` records reduced BLOCKS, and
/// the session count that produced them - the thing a walk's wall is
/// proportional to - is gone by the time the artifact exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct MeasureWork {
    /// Sessions the observed pass measured, from the preflight artifact.
    pub usable_sessions: usize,
    /// Generated seeds walked (or served pre-attested).
    pub seeds: usize,
    /// Generated sessions summed across those seeds.
    pub sessions: usize,
}

/// Where a run's eight `generated_seeds` come from. The production CLI
/// always uses [`WalkSource::LiveAttested`]; [`WalkSource::PreAttestedCacheOnly`]
/// exists ONLY for the parity gate, which cannot fit the full live walk set
/// (eight in-process month-long walks, ~26 s apiece per the phase-2a gate
/// timings) inside the runner's hard per-test ceiling alongside the ~85 s
/// observed pass. It proves nothing about walk determinism itself - that is
/// what the nine per-seed 2a parity gates (`parity12a_generated_seed_1..8`,
/// `parity12a_observed_*`) each prove independently, well under the
/// ceiling - so using it does not weaken what the suite as a whole covers,
/// it relocates the coverage to gates that already fit.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum WalkSource {
    /// The real `mogwai measure` behavior: run each FINAL walk fresh,
    /// in-process, and content-compare (cost excluded) against the
    /// read-only Brick G record before trusting it.
    LiveAttested,
    /// TEST-ONLY. Skip the walk and the attestation entirely; use the
    /// cached Brick G record's `per_session`/`forensic`/`cost` as-is.
    /// `cost.generated_s` reads as `0.0` under this mode - there is no
    /// live generated-side measurement to report.
    PreAttestedCacheOnly,
}

pub fn run(args: MeasureArgs) -> anyhow::Result<()> {
    let cfg = MeasureConfig::resolve(
        args.corpus,
        args.ledger,
        args.preflight,
        args.cache_dir,
        args.out,
    );
    let outcome = run_measure_with(&cfg, WalkSource::LiveAttested)?;
    let ladder = &outcome.artifact["ladder"];
    println!("artifact -> {}", cfg.out.display());
    println!("cost: {}", serde_json::to_string(&outcome.cost)?);
    println!("eligible: {}", ladder["eligible"]);
    println!("selected: {}", ladder["selected"]);
    println!("verdict: {}", ladder["verdict"]);
    report_cost(&outcome);
    Ok(())
}

/// The benchmark row for a measure run.
///
/// THE ONE SELF-REPORTING WORKLOAD, and the reason is this run's shape rather
/// than a preference: everything before the observed marker is corpus
/// verification and cache loading - real work, but not the work being
/// optimized - and an externally timed wall would fold a multi-gigabyte hash
/// pass into every reading of the measurement engine. `cost.total_s` is the
/// three measured phases and nothing else, so that is what goes out as
/// `elapsed_ms`.
///
/// Work sizes beside it: the session and seed counts a wall must be read
/// against. Both are contract-fixed today (23 sessions, eight seeds), which is
/// exactly why they are worth recording - a wall that halves while `sessions`
/// changes is not a speedup, and the counter is what makes that visible
/// instead of plausible.
fn report_cost(outcome: &MeasureOutcome) {
    let seconds = |key: &str| outcome.cost[key].as_f64().unwrap_or(f64::NAN);
    let integer = |value: &Value| value.as_i64().unwrap_or(0);
    // Fractional milliseconds: the scrape keeps the exact microseconds, and
    // rounding here would throw away the resolution a comparison uses.
    mogwai_lab::sidecar::kv("elapsed_ms", format!("{:.3}", seconds("total_s") * 1e3));
    for (key, phase) in [
        ("observed_ms", "observed_s"),
        ("generated_ms", "generated_s"),
        ("bootstrap_ms", "bootstrap_s"),
    ] {
        mogwai_lab::sidecar::kv(key, format!("{:.3}", seconds(phase) * 1e3));
    }
    mogwai_lab::sidecar::report("peak_rss_bytes", integer(&outcome.cost["peak_rss_bytes"]));
    mogwai_lab::sidecar::report("scratch_bytes", integer(&outcome.cost["scratch_bytes"]));

    let count = |value: usize| i64::try_from(value).unwrap_or(i64::MAX);
    mogwai_lab::sidecar::report("seeds", count(outcome.work.seeds));
    mogwai_lab::sidecar::report("sessions", count(outcome.work.sessions));
    mogwai_lab::sidecar::report("usable_sessions", count(outcome.work.usable_sessions));
}

/// The driver: `mode_measure12a`, callable directly (with `cfg.out`
/// overridden to a scratch path) by the golden-gate parity test as well as
/// by the CLI. WRITES the artifact atomically to `cfg.out` on success.
/// Always runs the real live walk attestation - see [`run_measure_with`]
/// for the test-only pre-attested seam.
pub fn run_measure(cfg: &MeasureConfig) -> anyhow::Result<MeasureOutcome> {
    run_measure_with(cfg, WalkSource::LiveAttested)
}

/// [`run_measure`], parameterized over [`WalkSource`].
pub fn run_measure_with(
    cfg: &MeasureConfig,
    walk_source: WalkSource,
) -> anyhow::Result<MeasureOutcome> {
    let harness_commit = require_clean_tree().map_err(|e| anyhow!("{e}"))?;

    // The Brick G references load READ-ONLY before anything runs.
    let brick_g = load_brick_g_walks(&cfg.walk_cache_dir)
        .map_err(|e| anyhow!("loading the Brick G walk cache: {e}"))?;

    let preflight_bytes = std::fs::read(&cfg.preflight_path)
        .with_context(|| format!("reading {}", cfg.preflight_path.display()))?;
    let preflight_json: Value = serde_json::from_slice(&preflight_bytes)?;
    let usable: Vec<Value> = preflight_json["usable_sessions"]
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow!("preflight artifact carries no usable_sessions array"))?;

    let sampler = ResourceSampler::start(
        vec![cfg.observed_cache_path.clone()],
        Some(cfg.walk_cache_dir.clone()),
    );

    // -- Observed pass, LIVE (the authoritative input). The pre-existing
    // observed cache is a MANDATORY structural cross-check: absence or
    // divergence refuses.
    mogwai_lab::sidecar::marker("observed");
    let t0 = std::time::Instant::now();
    let observed = run_measure12a_observed(&cfg.corpus, &cfg.ledger, &cfg.preflight_path)?;
    let observed_s = t0.elapsed().as_secs_f64();
    if !cfg.observed_cache_path.exists() {
        bail!(
            "no cached observed half to cross-check against ({}); the Brick G observed pass \
             must exist",
            cfg.observed_cache_path.display()
        );
    }
    let cached_obs: Value = serde_json::from_slice(&std::fs::read(&cfg.observed_cache_path)?)?;
    if typed_canon(&cached_obs) != typed_canon(&observed) {
        bail!("the live observed pass diverges from the cached observed half");
    }
    write_json_atomic(&cfg.observed_cache_path, &observed)
        .map_err(|e| anyhow!("writing {}: {e}", cfg.observed_cache_path.display()))?;
    println!("observed pass: {observed_s:.1}s");

    // -- The eight FINAL walks as cost-attestation replays, IN-PROCESS: run
    // through the lab engine and content-compare (cost excluded) against
    // the read-only Brick G record. Under `PreAttestedCacheOnly` (test-only
    // seam, see `WalkSource`) the walk and the attestation are both
    // SKIPPED and the cached record is used as-is.
    mogwai_lab::sidecar::marker("walks");
    let t1 = std::time::Instant::now();
    let mut generated_seeds: Vec<GeneratedSeed> = Vec::with_capacity(FINAL_SEEDS.len());
    for &seed in FINAL_SEEDS {
        let attested = brick_g
            .get(&seed)
            .ok_or_else(|| anyhow!("Brick G cache is missing seed {seed}"))?;
        match walk_source {
            WalkSource::LiveAttested => {
                let seed_u64 =
                    u64::try_from(seed).map_err(|_| anyhow!("seed {seed} is not positive"))?;
                let replay = run_final_walk(seed_u64)?;
                let mut replayed = replay.clone();
                replayed.as_object_mut().expect("a record").remove("cost");
                let mut reference = attested.clone();
                reference.as_object_mut().expect("a record").remove("cost");
                if typed_canon(&replayed) != typed_canon(&reference) {
                    bail!("seed {seed} replay diverges from the cached Brick G walk");
                }
                let session_count = replay["per_session"].as_array().map_or(0, Vec::len);
                println!("seed {seed} replay attested: {session_count} complete sessions");
                generated_seeds.push(GeneratedSeed {
                    seed,
                    per_session: replay["per_session"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                    forensic: replay["forensic"].clone(),
                    cost: attested["cost"].clone(),
                });
            }
            WalkSource::PreAttestedCacheOnly => {
                generated_seeds.push(GeneratedSeed {
                    seed,
                    per_session: attested["per_session"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                    forensic: attested["forensic"].clone(),
                    cost: attested["cost"].clone(),
                });
            }
        }
    }
    let generated_s = match walk_source {
        WalkSource::LiveAttested => t1.elapsed().as_secs_f64(),
        WalkSource::PreAttestedCacheOnly => 0.0,
    };

    // -- Input-side population gates.
    for g in &generated_seeds {
        session_dates_are_23_sorted_unique(&g.per_session)
            .with_context(|| format!("seed {}", g.seed))?;
    }
    let mut distinct_calendars: std::collections::BTreeSet<Vec<Option<&str>>> =
        std::collections::BTreeSet::new();
    for g in &generated_seeds {
        distinct_calendars.insert(
            g.per_session
                .iter()
                .map(|r| r["session_date"].as_str())
                .collect(),
        );
    }
    if distinct_calendars.len() != 1 {
        bail!("the generated seeds disagree on session dates");
    }

    // -- Assembly with a provisional MUTABLE cost record (two-phase: the
    // bootstrap clock stops after assembly, then the fields finalize in
    // place before validation).
    mogwai_lab::sidecar::marker("bootstrap");
    let t2 = std::time::Instant::now();
    let n_usable = usable.len();
    let mults = bootstrap_multiplicities(n_usable);
    let mut cost = serde_json::json!({
        "observed_s": observed_s, "generated_s": generated_s,
        "bootstrap_s": 0.0, "total_s": 0.0,
        "peak_rss_bytes": 0, "scratch_bytes": 0,
    });
    let binding_extra = serde_json::json!({
        "harness_tree_commit": harness_commit,
        "generated": {
            "seeds": FINAL_SEEDS,
            "window_start_ns": FINAL_START_NS,
            "window_length_ns": FINAL_END_NS - FINAL_START_NS,
            "warmup": SUMMARY_WARMUP,
        },
    });
    let artifact =
        assemble_measure12a_artifact(&observed, &generated_seeds, &binding_extra, &mults, &cost)
            .map_err(|e| anyhow!("assembly refused: {e}"))?;
    // A throwaway serialization pass realizes the late json_safe memory
    // peak while the sampler still runs and BEFORE the cost freezes.
    drop(serde_json::to_string(
        &mogwai_lab::aggregate::artifact::json_safe(artifact.clone()),
    ));
    let bootstrap_s = t2.elapsed().as_secs_f64();
    let total_s = observed_s + generated_s + bootstrap_s;

    let (peak_rss, peak_scratch) = sampler
        .stop(
            std::slice::from_ref(&cfg.observed_cache_path),
            Some(&cfg.walk_cache_dir),
        )
        .map_err(|e| anyhow!("{e}"))?;
    cost["bootstrap_s"] = serde_json::json!(bootstrap_s);
    cost["total_s"] = serde_json::json!(total_s);
    cost["peak_rss_bytes"] = serde_json::json!(peak_rss);
    cost["scratch_bytes"] = serde_json::json!(peak_scratch);

    // Re-assemble now that `cost` is final - the artifact pastes `cost`
    // verbatim, so the provisional one above must be swapped for the
    // finalized record before validation and the write.
    let artifact =
        assemble_measure12a_artifact(&observed, &generated_seeds, &binding_extra, &mults, &cost)
            .map_err(|e| anyhow!("assembly refused: {e}"))?;

    let mut errs = measure12a_schema_errors(&artifact);
    errs.extend(measure12a_semantic_errors(&artifact, &usable));
    if !errs.is_empty() {
        bail!(
            "the measure12a artifact violates the contract: {}",
            errs.iter().take(10).cloned().collect::<Vec<_>>().join("; ")
        );
    }

    // The same second guard the other tree-attested writers carry: the
    // binding below claims git attested this run, and one installed double
    // would otherwise forge both the opening `require_clean_tree` and this
    // re-attestation.
    crate::attestation::refuse_scripted_tree_attestation()?;
    let (head, clean) = fresh_tree_state().map_err(|e| anyhow!("{e}"))?;
    if !clean || head != harness_commit {
        bail!("the tree changed during the measure12a run; the artifact is unbound");
    }

    write_json_atomic(&cfg.out, &artifact)
        .map_err(|e| anyhow!("writing {}: {e}", cfg.out.display()))?;

    Ok(MeasureOutcome {
        artifact,
        cost,
        work: MeasureWork {
            usable_sessions: n_usable,
            seeds: generated_seeds.len(),
            sessions: generated_seeds.iter().map(|g| g.per_session.len()).sum(),
        },
    })
}

/// The input-side population gate on one seed's generated `per_session` array:
/// 23 sessions, every `session_date` a string, all distinct, IN ASCENDING
/// ORDER.
///
/// THE ORDER HALF WAS DECORATION UNTIL 2026-08-20. The gate sorted its own copy
/// of the dates before comparing it against the sorted-deduped copy, so the
/// comparison could only ever detect a DUPLICATE - two sorted vectors of the
/// same multiset are equal by construction - while the refusal it raised said
/// "not 23 sorted unique". A shuffled calendar passed it. The dates are
/// compared in the order they arrive now, which is the only comparison that
/// states what the message claims, and the count, duplicate and order refusals
/// are three distinct messages so a failure says which one fired. Extracted
/// from `run_measure_with` so the
/// claim is testable at all: it sat mid-way through a multi-minute walk driver
/// with no reachable seam.
fn session_dates_are_23_sorted_unique(per_session: &[Value]) -> anyhow::Result<()> {
    let dates: Vec<Option<&str>> = per_session
        .iter()
        .map(|r| r.get("session_date").and_then(Value::as_str))
        .collect();
    if dates.iter().any(Option::is_none) {
        bail!("carries non-string session dates");
    }
    let as_read: Vec<&str> = dates.iter().map(|d| d.unwrap_or_default()).collect();
    if as_read.len() != 23 {
        bail!("carries {} session dates, not 23", as_read.len());
    }
    // THE THREE REFUSALS ARE SEPARATE MESSAGES ON PURPOSE. A shared one reads
    // as a contradiction on the ordering path - an out-of-order calendar of 23
    // distinct dates would report "carries 23 sessions, not 23 sorted unique" -
    // and, worse, it leaves a test unable to name which half it selected.
    let mut ascending = as_read.clone();
    ascending.sort_unstable();
    ascending.dedup();
    if ascending.len() != as_read.len() {
        bail!("carries duplicate session dates: {as_read:?}");
    }
    if as_read != ascending {
        bail!("carries session dates out of ascending order: {as_read:?}");
    }
    Ok(())
}

/// `run_measure12a_observed`: the observed half - per-session sufficient
/// records plus the monthly aggregates, bound to the input and
/// sub-contract hashes.
pub(crate) fn run_measure12a_observed(
    corpus: &Path,
    ledger: &Path,
    preflight_path: &Path,
) -> anyhow::Result<Value> {
    let hashes: BTreeMap<String, String> =
        verify_input(corpus, ledger).map_err(|e| anyhow!("verifying the delivered corpus: {e}"))?;
    let (preflight, preflight_hash) = require_preflight(&hashes, preflight_path)
        .map_err(|e| anyhow!("checking the preflight artifact: {e}"))?;
    let usable: Vec<String> = preflight["usable_sessions"]
        .as_array()
        .ok_or_else(|| anyhow!("preflight artifact carries no usable_sessions array"))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect();

    let files = data_files(corpus).map_err(|e| anyhow!("listing the corpus: {e}"))?;
    let per_session = observed::observe(parse_stream(files), &usable)
        .map_err(|e| anyhow!("the observed pass refused: {e}"))?;

    // `blocks_from_sessions` extracts each session's `block2`/`block3`/
    // `block4` SUB-OBJECT before pooling - passing the whole per-session
    // records to `pool_block2` et al. directly (as this function used to)
    // is a defect: `rec.as_object()` still succeeds (the whole session
    // record IS an object), so it silently iterates `session_date`,
    // `segments`, `permutations`, etc. as if they were per-hour window
    // maps, and panics the moment it reaches a key whose value is not an
    // object (found live: the 2c-ii golden-gate run over the real corpus,
    // `monthly.rs:374`, "a window map"). The 2b/2c-i gates never exercised
    // this path because they always went through `blocks_from_sessions`
    // already; this was the one call site that had grown its own
    // hand-rolled (and wrong) copy.
    let monthly = mogwai_lab::aggregate::monthly::blocks_from_sessions(&per_session)
        .map_err(|e| anyhow!("pooling the monthly blocks: {e}"))?;
    let perms: Vec<&[Value]> = per_session
        .iter()
        .map(|r| {
            r["permutations"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
        })
        .collect();
    let permutations_monthly = mogwai_lab::aggregate::monthly::aggregate_permutations(&perms);

    Ok(serde_json::json!({
        "binding": {
            "job_id": mogwai_lab::subcontract::JOB_ID,
            "subcontract_hash": mogwai_lab::subcontract::subcontract_hash(),
            "preflight_artifact_hash": preflight_hash,
            "file_hashes": hashes,
            "tape_protocol_version": 11,
        },
        "per_session": per_session,
        "monthly": monthly,
        "permutations_monthly": permutations_monthly,
    }))
}

pub(crate) fn run_observed_with_count_windows(
    corpus: &Path,
    ledger: &Path,
    preflight_path: &Path,
    windows: &'static [i64],
) -> anyhow::Result<Value> {
    let hashes: BTreeMap<String, String> =
        verify_input(corpus, ledger).map_err(|e| anyhow!("verifying the delivered corpus: {e}"))?;
    let (preflight, preflight_hash) = require_preflight(&hashes, preflight_path)
        .map_err(|e| anyhow!("checking the preflight artifact: {e}"))?;
    let usable = preflight["usable_sessions"]
        .as_array()
        .ok_or_else(|| anyhow!("preflight artifact carries no usable_sessions array"))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    let files = data_files(corpus).map_err(|e| anyhow!("listing the corpus: {e}"))?;
    let per_session = observed::observe_with_count_windows(parse_stream(files), &usable, windows)
        .map_err(|e| anyhow!("the observed count-curve pass refused: {e}"))?;
    Ok(serde_json::json!({
        "binding": {"preflight_artifact_hash": preflight_hash, "file_hashes": hashes},
        "per_session": per_session,
    }))
}

pub(crate) fn run_observed_with_count_windows_ordered(
    month: u64,
    corpus: &Path,
    ledger: &Path,
    preflight_path: &Path,
    windows: &'static [i64],
    ledger_key: &str,
) -> anyhow::Result<(Value, Vec<mogwai_lab::measure12a::OrderedCount>)> {
    let hashes: BTreeMap<String, String> =
        mogwai_lab::ledger::verify_input_entry(corpus, ledger, ledger_key)
            .map_err(|e| anyhow!("verifying the delivered corpus: {e}"))?;
    let (preflight, preflight_hash) = require_preflight(&hashes, preflight_path)
        .map_err(|e| anyhow!("checking the preflight artifact: {e}"))?;
    let usable = preflight["usable_sessions"]
        .as_array()
        .ok_or_else(|| anyhow!("preflight artifact carries no usable_sessions array"))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    let files = data_files(corpus).map_err(|e| anyhow!("listing the corpus: {e}"))?;
    let frame = mogwai_lab::session::ScheduleFrame::stage_m(Path::new(
        "analysis/tz-america-chicago-2026c.json",
    ))
    .map_err(|e| anyhow!("loading the Stage M schedule frame: {e}"))?;
    let (per_session, ordered) = observed::observe_with_count_windows_ordered_frame(
        parse_stream(files),
        &usable,
        windows,
        Some(&frame),
    )
    .map_err(|e| anyhow!("the Stage M observed pass refused: {e}"))?;
    Ok((
        serde_json::json!({"binding":{"month":month,"preflight_artifact_hash":preflight_hash,"file_hashes":hashes,"schedule_frame":preflight["schedule_frame"]},"per_session":per_session}),
        ordered,
    ))
}

pub(crate) fn run_observed_ordered(
    corpus: &Path,
    ledger: &Path,
    preflight_path: &Path,
) -> anyhow::Result<(Value, Vec<mogwai_lab::measure12a::OrderedCount>)> {
    let hashes: BTreeMap<String, String> =
        verify_input(corpus, ledger).map_err(|e| anyhow!("verifying the delivered corpus: {e}"))?;
    let (preflight, preflight_hash) = require_preflight(&hashes, preflight_path)
        .map_err(|e| anyhow!("checking the preflight artifact: {e}"))?;
    let usable = preflight["usable_sessions"]
        .as_array()
        .ok_or_else(|| anyhow!("preflight artifact carries no usable_sessions array"))?
        .iter()
        .map(|v| v.as_str().unwrap_or_default().to_string())
        .collect::<Vec<_>>();
    let files = data_files(corpus).map_err(|e| anyhow!("listing the corpus: {e}"))?;
    let (per_session, ordered) = observed::observe_ordered(parse_stream(files), &usable)
        .map_err(|e| anyhow!("the ordered-count observed pass refused: {e}"))?;
    Ok((
        serde_json::json!({
            "binding": {"preflight_artifact_hash": preflight_hash, "file_hashes": hashes},
            "per_session": per_session,
        }),
        ordered,
    ))
}

/// One FINAL walk, constructed exactly the way `gen.rs`'s `build_source`
/// does and exactly as `crates/mogwai-cli/tests/parity12a.rs`'s
/// `run_final_walk` does: the committed MNQ preset, no overrides, the walk
/// starting at `FINAL_START_NS - SUMMARY_WARMUP` with the vol trace
/// enabled, measuring `[FINAL_START_NS, FINAL_START_NS + FINAL_LENGTH)`.
pub fn run_final_walk(seed: u64) -> anyhow::Result<Value> {
    run_final_walk_with_count_windows(seed, mogwai_lab::subcontract::COUNT_WINDOWS_S)
}

pub(crate) fn run_final_walk_with_count_windows(
    seed: u64,
    windows: &'static [i64],
) -> anyhow::Result<Value> {
    let profile = mogwai_venue::config::profile_from_preset("MNQ").map_err(|e| anyhow!("{e}"))?;
    let calendar = profile
        .calendar
        .as_ref()
        .ok_or_else(|| anyhow!("the MNQ preset carries no session calendar"))?;
    let offset = i32::from(calendar.utc_offset_minutes);

    let start =
        u64::try_from(FINAL_START_NS).map_err(|_| anyhow!("FINAL_START_NS is not positive"))?;
    let length_s: u64 = FINAL_LENGTH
        .trim_end_matches('s')
        .parse()
        .map_err(|e| anyhow!("FINAL_LENGTH is not seconds: {e}"))?;
    let end = start + length_s * 1_000_000_000;
    let warmup_days: u64 = SUMMARY_WARMUP
        .trim_end_matches('d')
        .parse()
        .map_err(|e| anyhow!("SUMMARY_WARMUP is not days: {e}"))?;
    let walk_start = start - warmup_days * 86_400 * 1_000_000_000;

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
    .map_err(|e| anyhow!("building the generator: {e:?}"))?;
    source.enable_vol_trace();

    let mut acc = GeneratedAcc::new_with_count_windows(
        seed,
        start,
        end,
        offset,
        profile.scalars.modal_tick,
        windows,
    );
    while let Some(event) = source.next_tick() {
        if event.ts_event() >= end {
            break;
        }
        match event {
            TickEvent::Quote(q) => {
                let trace = source.take_vol_trace();
                acc.push_quote(&q, trace).map_err(|e| anyhow!("{e}"))?;
            }
            TickEvent::Trade(t) => acc.push_trade(&t).map_err(|e| anyhow!("{e}"))?,
        }
    }
    acc.finish()
        .map_err(|e| anyhow!("the generated measurement pass refused: {e}"))
}

#[cfg(test)]
mod tests {
    use super::session_dates_are_23_sorted_unique;

    fn sessions(dates: &[&str]) -> Vec<serde_json::Value> {
        dates
            .iter()
            .map(|d| serde_json::json!({ "session_date": d }))
            .collect()
    }

    fn calendar() -> Vec<String> {
        (1..=23).map(|d| format!("2026-01-{d:02}")).collect()
    }

    /// The gate's refusal says "23 sorted unique" and all three words must be
    /// load-bearing. The ORDER one was not: the gate sorted its own copy first,
    /// so a shuffled calendar of 23 distinct dates passed - and a sorted
    /// comparison against a sorted-deduped copy can, by construction, only
    /// report duplicates.
    #[test]
    fn the_population_gate_refuses_a_calendar_that_is_out_of_order() {
        let calendar = calendar();
        let refs: Vec<&str> = calendar.iter().map(String::as_str).collect();
        session_dates_are_23_sorted_unique(&sessions(&refs))
            .expect("an ascending calendar of 23 distinct dates passes");

        // Each arm is selected BY ITS OWN MESSAGE. A substring two refusals
        // share cannot tell which half fired, and this arc has already been
        // burned by a bite-check that asserted on exactly that.
        let mut shuffled = refs.clone();
        shuffled.swap(3, 17);
        let err = session_dates_are_23_sorted_unique(&sessions(&shuffled))
            .expect_err("a calendar out of ascending order must be refused");
        assert!(
            err.to_string().contains("out of ascending order"),
            "the ordering refusal is its own message: {err}"
        );

        // The three halves that already bit, kept beside it so a later
        // tightening cannot trade one for another.
        let mut duplicated = refs.clone();
        duplicated[22] = duplicated[21];
        let err = session_dates_are_23_sorted_unique(&sessions(&duplicated))
            .expect_err("a duplicated session date must be refused");
        assert!(
            err.to_string().contains("duplicate session dates"),
            "the duplicate refusal is its own message: {err}"
        );
        let err = session_dates_are_23_sorted_unique(&sessions(&refs[..22]))
            .expect_err("22 sessions must be refused");
        assert!(
            err.to_string().contains("carries 22 session dates, not 23"),
            "the count refusal is its own message: {err}"
        );
        let missing = vec![serde_json::json!({ "session_date": 7 })];
        let err = session_dates_are_23_sorted_unique(&missing)
            .expect_err("a non-string session date must be refused");
        assert!(
            err.to_string().contains("non-string session dates"),
            "the type refusal is its own message: {err}"
        );
    }

    /// Regression for the 2c-ii golden-gate finding: `run_measure12a_observed`
    /// once passed whole PER-SESSION records (carrying sibling non-object
    /// fields like `segments` and `permutations`, both arrays) straight into
    /// `pool_block2`/`aggregate_block3`/`aggregate_block4`, which expect the
    /// `block2`/`block3`/`block4` SUB-OBJECT. `rec.as_object()` still
    /// succeeded (a whole session record is an object too), so nothing
    /// refused until the pooler tried to treat `segments` (an array) as an
    /// hour's window map and panicked - a defect the 2b/2c-i gates never hit
    /// because they always went through `blocks_from_sessions`, which
    /// extracts the named sub-objects first.
    ///
    /// This is a real crash the 2a per-session typed-canon parity gate could
    /// never catch: canon compares the VALUE at each already-agreed-upon
    /// path, so a call site indexing the WRONG path entirely is invisible to
    /// it. The regression here is a shape-strict smoke test over a
    /// minimal but realistically-shaped live per-session record - one
    /// that carries the array-typed sibling fields the bug tripped on -
    /// fed through the exact function `run_measure12a_observed` calls.
    #[test]
    fn blocks_from_sessions_does_not_choke_on_a_live_shaped_session_record() {
        let session = serde_json::json!({
            "session_date": "2026-01-01",
            "segments": [{"segment_index": 0, "open_ns": 0, "close_ns": 1}],
            "block1_hist": [],
            "block2": {},
            "block3": {
                "cells": {}, "pairs": {}, "lag1_parent_autocorr": {}, "hour20_labels": {},
            },
            "block4": {
                "all": {
                    "residual_count": 0, "warmup_excluded": 0, "zero_fraction": null,
                    "nz_abs_p90": null, "nz_abs_p99": null, "nz_abs_p999": null,
                    "ratio_p99_p90": null, "ratio_p999_p99": null,
                    "exceed_4": null, "exceed_8": null, "exceed_16": null,
                },
            },
            "permutations": [],
            "refusals": [],
        });
        let per_session = vec![session];

        // The bug: calling the block-2/3/4 poolers directly on the WHOLE
        // per-session records panics on the first non-object sibling field
        // (`segments` is an array) - documented here so the trap stays
        // visible, not exercised as the "fix".
        let whole_records: Vec<&serde_json::Value> = per_session.iter().collect();
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            drop(mogwai_lab::aggregate::monthly::pool_block2(&whole_records));
        }))
        .is_err();
        assert!(
            panicked,
            "pool_block2 over whole per-session records is expected to panic on a non-object \
             sibling field - if it no longer does, this test's premise is stale"
        );

        // The fix: `blocks_from_sessions` extracts each named sub-object
        // before pooling, so it must succeed over the same live-shaped
        // records `run_measure12a_observed` builds.
        let monthly = mogwai_lab::aggregate::monthly::blocks_from_sessions(&per_session)
            .expect("blocks_from_sessions must not choke on a live-shaped session record");
        assert_eq!(
            monthly["block2"],
            serde_json::json!({}),
            "an empty per-session block2 pools to an empty monthly block2"
        );
        for key in ["block1", "block2", "block3", "block4"] {
            assert!(monthly.get(key).is_some(), "monthly is missing {key}");
        }
    }
}
