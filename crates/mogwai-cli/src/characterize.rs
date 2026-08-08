// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai characterize`: the corpus-to-`char_<PAIR>.json` station.
//!
//! This closes the gap that blocked the Python retirement. `mogwai_lab`
//! carried the ported estimand layer as a LIBRARY with no way to run it, and
//! the multi-pair driver (`analysis/run_corpus.py`) had already been deleted,
//! so retiring `analysis/characterize.py` would have left nothing in either
//! language able to produce `char_<PAIR>.json` - the input
//! `mogwai synth fingerprint` reads, and the first step of onboarding any new
//! instrument.
//!
//! It covers BOTH Python entry points deliberately. `characterize.py` took one
//! pair and wrote one report; `run_corpus.py` fanned that across a
//! representative pair set in a process pool and printed a cross-pair table so
//! the stylized facts could be eyeballed for universality. A port of only the
//! first would have re-created the same gap one level down.

use std::path::{Path, PathBuf};

use clap::Args;
use serde_json::Value;

/// `run_corpus.py`'s `DEFAULT_PAIRS`: a representative subset rather than the
/// full corpus. The model is symbol-agnostic, so the corpus only has to show
/// the stylized-fact shape repeats across instruments and bound the
/// per-instrument scalars; a handful of pairs settles both far cheaper than
/// the whole grind.
const DEFAULT_PAIRS: [&str; 8] = [
    "XBTUSD", "ETHUSD", "USDTUSD", "SOLUSD", "ADAUSD", "XDGUSD", "XRPUSD", "DOTUSD",
];

/// `run_corpus.py` capped its process pool here so peak IO stays sane.
const MAX_WORKERS: usize = 6;

/// `characterize.py`'s `DATA_DIR` default. Offline-analysis input only, never
/// a server runtime knob.
const DEFAULT_DATA_DIR: &str = "/home/folk/Kraken";

/// Where `build_fingerprint` looks for the reports. These are gitignored
/// rather than committed, so unlike `synth`'s outputs there is no committed
/// artifact for a bare invocation to clobber.
const DEFAULT_OUT_DIR: &str = "analysis";

#[derive(Args)]
pub(crate) struct CharacterizeArgs {
    /// Pairs to characterize. Omit for the representative default set.
    /// A value containing a path separator or ending in `.csv` is treated as
    /// a path, matching `characterize.py`'s `resolve_path`.
    #[arg(value_name = "PAIR")]
    pairs: Vec<String>,
    /// Directory holding `<PAIR>.csv`. Defaults to `MOGWAI_DATA_DIR`, then
    /// the Python's own default.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Where to write `char_<PAIR>.json`. Defaults to `analysis/`.
    #[arg(long, value_name = "DIR")]
    out_dir: Option<PathBuf>,
    /// Maximum pairs to stream concurrently.
    #[arg(long, default_value_t = MAX_WORKERS)]
    jobs: usize,
}

/// `resolve_path`: a bare symbol becomes `<DATA_DIR>/<SYMBOL>.csv`; anything
/// path-shaped is taken as given.
fn resolve_path(data_dir: &Path, arg: &str) -> PathBuf {
    if arg.contains(std::path::MAIN_SEPARATOR) || arg.ends_with(".csv") {
        let candidate = PathBuf::from(arg);
        if candidate.is_absolute() {
            candidate
        } else {
            data_dir.join(candidate)
        }
    } else {
        data_dir.join(format!("{arg}.csv"))
    }
}

fn field(report: &Value, path: &[&str]) -> f64 {
    let mut cursor = report;
    for key in path {
        cursor = &cursor[*key];
    }
    cursor.as_f64().unwrap_or(f64::NAN)
}

pub(crate) fn run(args: CharacterizeArgs) -> anyhow::Result<()> {
    let data_dir = args
        .data_dir
        .or_else(|| std::env::var_os("MOGWAI_DATA_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
    let out_dir = args
        .out_dir
        .unwrap_or_else(|| PathBuf::from(DEFAULT_OUT_DIR));
    let requested: Vec<String> = if args.pairs.is_empty() {
        DEFAULT_PAIRS.iter().map(|p| (*p).to_string()).collect()
    } else {
        args.pairs.clone()
    };

    // `run_corpus.py` skipped missing or empty inputs by name rather than
    // failing the whole sweep, so one absent pair does not cost the others.
    let mut usable: Vec<(String, PathBuf)> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for pair in &requested {
        let path = resolve_path(&data_dir, pair);
        match std::fs::metadata(&path) {
            Ok(meta) if meta.len() > 0 => usable.push((pair.clone(), path)),
            _ => skipped.push(pair.clone()),
        }
    }
    if !skipped.is_empty() {
        println!("skipped (missing or empty): {}", skipped.join(", "));
    }
    if usable.is_empty() {
        anyhow::bail!("no usable pairs under {}", data_dir.display());
    }
    std::fs::create_dir_all(&out_dir)
        .map_err(|e| anyhow::anyhow!("creating {}: {e}", out_dir.display()))?;
    println!(
        "characterizing {} pair(s) with {} worker(s): {}",
        usable.len(),
        args.jobs.max(1).min(usable.len()),
        usable
            .iter()
            .map(|(p, _)| p.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );

    let jobs = args.jobs.max(1).min(usable.len());
    let queue = std::sync::Mutex::new(usable.into_iter().collect::<Vec<_>>());
    let results = std::sync::Mutex::new(Vec::<(String, Value)>::new());
    std::thread::scope(|scope| {
        for _ in 0..jobs {
            scope.spawn(|| {
                loop {
                    let Some((pair, path)) = queue.lock().expect("queue lock").pop() else {
                        return;
                    };
                    let started = std::time::Instant::now();
                    match mogwai_lab::characterize::characterize(&path) {
                        Ok(mut report) => {
                            // `elapsed_s` is a live wall-clock measurement the
                            // library deliberately omits, so the estimand layer
                            // stays a pure function of its input and the parity
                            // comparison has no live field to exclude. The CLI
                            // is the write path, so it stamps it here - the same
                            // split `synth cadence` uses for `generated_utc`.
                            report["elapsed_s"] = serde_json::json!(
                                (started.elapsed().as_secs_f64() * 100.0).round() / 100.0
                            );
                            let out = out_dir.join(format!("char_{pair}.json"));
                            match mogwai_lab::aggregate::artifact::write_json_atomic(&out, &report)
                            {
                                Ok(()) => results
                                    .lock()
                                    .expect("results lock")
                                    .push((pair.clone(), report)),
                                Err(e) => eprintln!("{pair}: writing {}: {e}", out.display()),
                            }
                        }
                        Err(e) => eprintln!("{pair}: {e}"),
                    }
                }
            });
        }
    });

    let mut reports = results
        .into_inner()
        .map_err(|_| anyhow::anyhow!("a characterize worker panicked"))?;
    if reports.len() < requested.len() - skipped.len() {
        anyhow::bail!("one or more pairs failed to characterize; see the messages above");
    }
    // `run_corpus.py` ordered the table by trade count, descending.
    reports.sort_by(|a, b| {
        field(&b.1, &["n_trades"])
            .partial_cmp(&field(&a.1, &["n_trades"]))
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    println!("\n== cross-pair stylized facts (confirm universality) ==");
    println!(
        "{:<9}{:>13}  {:>7}  {:>6}  {:>6}  {:>5}  {:>10}  {:>4}  {:>5}",
        "pair", "trades", "disp", "ret1", "|ret|1", "zchg", "tick", "pdec", "rnd"
    );
    for (pair, report) in &reports {
        println!(
            "{:<9}{:>13}  {:>7.0}  {:>6.2}  {:>6.2}  {:>5.2}  {:>10}  {:>4}  {:>5.2}",
            pair,
            field(report, &["n_trades"]) as i64,
            field(report, &["duration", "dwell", "dispersion_index"]),
            report["returns"]["acf"][0].as_f64().unwrap_or(f64::NAN),
            report["returns"]["abs_acf"][0].as_f64().unwrap_or(f64::NAN),
            field(report, &["returns", "zero_change_frac"]),
            report["returns"]["modal_tick"],
            report["returns"]["price_decimals_mode"],
            field(report, &["size", "round_frac"]),
        );
    }
    println!(
        "\ndisp=era-windowed duration dispersion index (Poisson=1); \
         ret1/|ret|1=lag-1 ACF; zchg=zero-price-change frac; rnd=size <=2 decimals frac"
    );
    println!("reports: {}", out_dir.display());
    Ok(())
}
