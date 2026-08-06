// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai synth fingerprint` / `mogwai synth cadence` /
//! `mogwai cadence-feasible`: the phase-3a synthesis paths
//! (`notes/rust-rewrite-phases.md`) over `mogwai_lab::fingerprint`/
//! `mogwai_lab::cadence`/`mogwai_lab::cadence_feasible`. Default paths are
//! CWD-relative, matching `mogwai measure`/`mogwai preflight`; everything is
//! overridable. NONE of these subcommands overwrite the committed
//! `analysis/fingerprint.json`/`analysis/cadence.json` by default - `--out`
//! must be given explicitly to write anywhere inside `analysis/`, and the
//! default `--out` for both is a `target/` scratch path.

use std::path::PathBuf;

use clap::{Args, Subcommand};
use mogwai_lab::storage::artifact_path;

#[derive(Subcommand)]
pub(crate) enum SynthCommand {
    /// Synthesize the generator fingerprint from `char_<PAIR>.json` reports
    /// plus `cadence.json` (`analysis/build_fingerprint.py`).
    Fingerprint(FingerprintArgs),
    /// Synthesize the cadence measurement from raw Binance trade archives
    /// (`analysis/build_cadence.py`).
    Cadence(CadenceArgs),
}

#[derive(Args)]
pub(crate) struct FingerprintArgs {
    /// Directory holding `char_<PAIR>.json` reports. Defaults to `analysis/`
    /// relative to the current directory.
    #[arg(long, value_name = "DIR")]
    char_dir: Option<PathBuf>,
    /// The cadence measurement to fold in. Defaults to `analysis/cadence.json`.
    #[arg(long, value_name = "PATH")]
    cadence: Option<PathBuf>,
    /// Where to write the fingerprint. Defaults to a `target/` scratch
    /// path - never the committed `analysis/fingerprint.json` unless given
    /// explicitly.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

const DEFAULT_CHAR_DIR: &str = "analysis";
const DEFAULT_CADENCE: &str = "analysis/cadence.json";
const DEFAULT_FINGERPRINT_OUT: &str = "target/mogwai-synth/fingerprint.json";

#[derive(Args)]
pub(crate) struct CadenceArgs {
    /// Directory holding the raw Binance trade archives
    /// (`<PAIR>-trades-2026-06.zip` for BTCUSDT/ETHUSDT/SOLUSDT). Defaults
    /// to `research/market-data/`.
    #[arg(long, value_name = "DIR")]
    data_dir: Option<PathBuf>,
    /// Where to write the cadence measurement. Defaults to a `target/`
    /// scratch path - never the committed `analysis/cadence.json` unless
    /// given explicitly.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

const DEFAULT_DATA_DIR: &str = "research/market-data";
const DEFAULT_CADENCE_OUT: &str = "target/mogwai-synth/cadence.json";

pub(crate) fn run(command: SynthCommand) -> anyhow::Result<()> {
    match command {
        SynthCommand::Fingerprint(args) => run_fingerprint(args),
        SynthCommand::Cadence(args) => run_cadence(args),
    }
}

fn run_fingerprint(args: FingerprintArgs) -> anyhow::Result<()> {
    let char_dir = args
        .char_dir
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CHAR_DIR));
    let cadence = args
        .cadence
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CADENCE));
    let out = artifact_path(args.out.as_deref(), DEFAULT_FINGERPRINT_OUT);

    let fingerprint = mogwai_lab::fingerprint::build_fingerprint(&char_dir, &cadence)
        .map_err(|e| anyhow::anyhow!("fingerprint synthesis refused: {e}"))?;
    mogwai_lab::aggregate::artifact::write_json_atomic(&out, &fingerprint)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", out.display()))?;

    println!(
        "fingerprint: {} pairs={}",
        out.display(),
        fingerprint["source"]["pairs"]
            .as_array()
            .map_or(0, std::vec::Vec::len)
    );
    Ok(())
}

fn run_cadence(args: CadenceArgs) -> anyhow::Result<()> {
    let data_dir = args
        .data_dir
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR));
    let out = artifact_path(args.out.as_deref(), DEFAULT_CADENCE_OUT);

    let mut cadence = mogwai_lab::cadence::build(&data_dir)
        .map_err(|e| anyhow::anyhow!("cadence synthesis refused: {e}"))?;
    // `provenance.generated_utc` is a live wall-clock stamp `build()` itself
    // deliberately omits (kept pure/deterministic for the parity gate); the
    // CLI, which is the actual write path, stamps it here as seconds since
    // the epoch rather than pulling in a date-formatting crate for one field.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    cadence["provenance"]["generated_utc"] = serde_json::json!(now.as_secs());
    mogwai_lab::aggregate::artifact::write_json_atomic(&out, &cadence)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", out.display()))?;

    println!("cadence: {}", out.display());
    println!(
        "verdict: {}",
        mogwai_lab::cadence_feasible::verdict(&cadence)
    );
    Ok(())
}

#[derive(Args)]
pub(crate) struct CadenceFeasibleArgs {
    /// The cadence measurement to read. Defaults to `analysis/cadence.json`.
    #[arg(long, value_name = "PATH")]
    cadence: Option<PathBuf>,
}

pub(crate) fn run_cadence_feasible(args: CadenceFeasibleArgs) -> anyhow::Result<()> {
    let path = args
        .cadence
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CADENCE));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let cadence: serde_json::Value = serde_json::from_str(&text)?;
    let verdict = mogwai_lab::cadence_feasible::verdict(&cadence);
    println!("parent/child verdict: {verdict}");
    println!(
        "measured density: {}",
        serde_json::to_string(&cadence["targets"]["per_second_counts"])?
    );
    if verdict != "PROCEED" {
        anyhow::bail!("{verdict}");
    }
    Ok(())
}
