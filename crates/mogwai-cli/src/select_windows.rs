// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai select-windows`: the bar-frame intake station.
//!
//! The CLI half of the retired Python window-selection implementation. The library computes; this
//! renders, caches and drives. The split matters for the same reason it did for
//! `characterize`: the estimand layer stays a pure function of its input, so the
//! parity gate has no live field to exclude.
//!
//! Four phases, matching the Python's, and cached so selection can be re-run
//! without re-reading 1.5 GB of archives.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use mogwai_lab::select_windows as sw;

/// The retired Python window-selection implementation's `MARKET_DATA`,
/// relative to the repository root.
const DEFAULT_MARKET_DATA: &str = "research/market-data";

/// The retired Python window-selection implementation's `CACHE`.
const DEFAULT_CACHE: &str = "analysis/cme_daily_features.json";

#[derive(Args)]
pub(crate) struct SelectWindowsArgs {
    #[command(subcommand)]
    command: Phase,
    /// Directory holding the one-minute archives.
    #[arg(long, value_name = "DIR", global = true)]
    market_data: Option<PathBuf>,
    /// Where the daily feature cache lives.
    #[arg(long, value_name = "FILE", global = true)]
    cache: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Phase {
    /// One streaming pass per archive, writing the daily feature cache.
    Features,
    /// Pick the windows: farthest-point over z-scored monthly features.
    Select,
    /// Yearly medians, answering whether the proxies are era-stable.
    Drift,
    /// Budget-aware plan: current-era strata plus historical drift probes.
    Plan,
}

fn cache_path(args: &SelectWindowsArgs) -> PathBuf {
    args.cache
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CACHE))
}

fn market_data(args: &SelectWindowsArgs) -> PathBuf {
    args.market_data
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MARKET_DATA))
}

/// The cache's on-disk shape, matching what the retired Python
/// window-selection implementation's `features` mode
/// writes: `{symbol: {session: {feature: value}}}`.
type CacheJson = std::collections::BTreeMap<String, serde_json::Map<String, serde_json::Value>>;

fn write_cache(path: &PathBuf, cache: &sw::Cache) -> anyhow::Result<()> {
    let mut out = serde_json::Map::new();
    for (symbol, sessions) in cache {
        let mut days = serde_json::Map::new();
        for (day, f) in sessions {
            days.insert(
                day.clone(),
                serde_json::json!({
                    "rv": f.rv,
                    "vol_of_vol": f.vol_of_vol,
                    "volume": f.volume,
                    "volume_cv": f.volume_cv,
                    "zero_change": f.zero_change,
                    "gap": f.gap,
                }),
            );
        }
        out.insert(symbol.clone(), serde_json::Value::Object(days));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(
        path,
        serde_json::to_string(&serde_json::Value::Object(out))?,
    )?;
    Ok(())
}

/// Reads the cache back in `ARCHIVES` order.
///
/// Symbol order is not presentation: it is the month table's first-seen order,
/// which is the term order of every z-score sum. Reading in a different order
/// would move the last ulp of a z-score, so the order comes from `ARCHIVES`
/// rather than from however the file happens to be laid out.
fn read_cache(path: &PathBuf) -> anyhow::Result<sw::Cache> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(
            "{}: {e}; run `mogwai select-windows features` first",
            path.display()
        )
    })?;
    let parsed: CacheJson = serde_json::from_str(&text)?;
    let mut cache = sw::Cache::new();
    for (symbol, _) in sw::ARCHIVES {
        let Some(days) = parsed.get(symbol) else {
            anyhow::bail!("{}: cache carries no {symbol} sessions", path.display());
        };
        let mut sessions = sw::SymbolSessions::new();
        for (day, value) in days {
            let get = |key: &str| -> anyhow::Result<f64> {
                value
                    .get(key)
                    .and_then(serde_json::Value::as_f64)
                    .ok_or_else(|| anyhow::anyhow!("{symbol} {day}: missing numeric `{key}`"))
            };
            sessions.push((
                day.clone(),
                sw::DayFeatures {
                    rv: get("rv")?,
                    vol_of_vol: get("vol_of_vol")?,
                    volume: get("volume")?,
                    volume_cv: get("volume_cv")?,
                    zero_change: get("zero_change")?,
                    gap: get("gap")?,
                },
            ));
        }
        cache.push((symbol.to_string(), sessions));
    }
    Ok(cache)
}

fn months(args: &SelectWindowsArgs) -> anyhow::Result<sw::MonthTable> {
    let cache = read_cache(&cache_path(args))?;
    Ok(sw::monthly(&cache)?)
}

pub(crate) fn run(args: &SelectWindowsArgs) -> anyhow::Result<()> {
    match &args.command {
        Phase::Features => {
            let dir = market_data(args);
            let mut cache = sw::Cache::new();
            for (symbol, file) in sw::ARCHIVES {
                eprintln!("scanning {file} ...");
                let sessions = sw::build_features(&dir.join(file))?;
                eprintln!("  {} sessions", sessions.len());
                cache.push((symbol.to_string(), sessions));
            }
            let path = cache_path(args);
            write_cache(&path, &cache)?;
            println!("wrote {}", path.display());
        }
        Phase::Select => {
            let months = months(args)?;
            let selection = sw::select(&months)?;
            println!(
                "eligible months: {}  ({} .. {})",
                selection.eligible_count, selection.eligible_first, selection.eligible_last
            );
            println!();
            println!(
                "{:<9} {:>8} {:>8} {:>8} {:>8} {:>8}",
                "month", "NQ.rv", "NQ.vcv", "NQ.zero", "CL.rv", "GC.rv"
            );
            let table: std::collections::HashMap<&str, &sw::MonthRow> =
                months.iter().map(|(m, r)| (m.as_str(), r)).collect();
            let mut sorted = selection.chosen.clone();
            sorted.sort();
            for month in &sorted {
                let row = table[month.as_str()];
                println!(
                    "{month:<9} {:>8.4} {:>8.3} {:>8.3} {:>8.4} {:>8.4}",
                    row["NQ.rv"],
                    row["NQ.volume_cv"],
                    row["NQ.zero_change"],
                    row["CL.rv"],
                    row["GC.rv"]
                );
            }
            println!();
            println!("percentile of each pick within the eligible span (NQ realized vol):");
            for (month, percentile) in sw::nq_rv_percentiles(&months, &selection.chosen)? {
                println!("  {month:<9}  {percentile:>5.1}%");
            }
        }
        Phase::Drift => {
            let months = months(args)?;
            println!(
                "{:<6} {:>12} {:>10} {:>14} {:>12} {:>12} {:>12}",
                "year", "NQ.zero", "NQ.vcv", "NQ.volume", "ES.zero", "CL.zero", "GC.zero"
            );
            for (year, values) in sw::drift(&months)? {
                println!(
                    "{year:<6} {:>12.4} {:>10.3} {:>14.0} {:>12.4} {:>12.4} {:>12.4}",
                    values[0], values[1], values[2], values[3], values[4], values[5]
                );
            }
        }
        Phase::Plan => {
            let months = months(args)?;
            let plan = sw::plan(&months)?;
            let table: std::collections::HashMap<&str, &sw::MonthRow> =
                months.iter().map(|(m, r)| (m.as_str(), r)).collect();
            println!(
                "current-era pool: {} .. {}  ({} months, last 30 ELIGIBLE not calendar)",
                plan.pool_first, plan.pool_last, plan.pool_len
            );
            println!();
            println!("stratified by NQ realized vol within that pool:");
            for (percentile, month) in &plan.stratified {
                let row = table[month.as_str()];
                println!(
                    "  p{percentile:<3}  {month:<9}  NQ.rv {:>7.4}  CL.rv {:>7.4}  GC.rv {:>7.4}  \
                     NQ.vcv {:>5.2}",
                    row["NQ.rv"], row["CL.rv"], row["GC.rv"], row["NQ.volume_cv"]
                );
            }
            println!();
            println!("historical drift probes (highest NQ.rv per era):");
            for era in &plan.eras {
                println!(
                    "  {}..{}  stress {:<9} NQ.rv {:>7.4}   calm {:<9} NQ.rv {:>7.4}",
                    era.lo, era.hi, era.stress, era.stress_rv, era.calm, era.calm_rv
                );
            }
        }
    }
    Ok(())
}
