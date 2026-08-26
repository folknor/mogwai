// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! `mogwai preflight`: the fail-closed TBBO corpus contract
//! (`mogwai_lab::preflight`), the parity gate of
//! the retired rewrite plan, phase 1 - must reproduce
//! `analysis/out/mnq-fit-preflight.json` value-identically against the
//! delivered July corpus.

use std::path::PathBuf;

use clap::Args;
use mogwai_lab::preflight::{run_preflight, write_json_atomic};
use mogwai_lab::storage::artifact_path;

#[derive(Args)]
pub(crate) struct PreflightArgs {
    /// The delivered corpus directory. Defaults to the July MNQ delivery
    /// path, relative to the current directory.
    #[arg(long, value_name = "DIR")]
    corpus: Option<PathBuf>,
    /// The Databento jobs manifest. Read-only: this never writes it.
    #[arg(long, value_name = "PATH")]
    jobs_manifest: Option<PathBuf>,
    /// Where to write the preflight artifact. It is the user's own file:
    /// never cached, never auto-deleted.
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

const DEFAULT_CORPUS: &str = "research/market-data/databento/mnqv/2026-07.full.tbbo";
const DEFAULT_JOBS_MANIFEST: &str = "analysis/databento-jobs.json";
const DEFAULT_OUT: &str = "analysis/out/mnq-fit-preflight.json";

pub(crate) fn run(args: PreflightArgs) -> anyhow::Result<()> {
    let corpus = args.corpus.unwrap_or_else(|| PathBuf::from(DEFAULT_CORPUS));
    let jobs_manifest = args
        .jobs_manifest
        .unwrap_or_else(|| PathBuf::from(DEFAULT_JOBS_MANIFEST));
    let out = artifact_path(args.out.as_deref(), DEFAULT_OUT);

    let artifact = run_preflight(&corpus, &jobs_manifest)
        .map_err(|e| anyhow::anyhow!("preflight refused: {e}"))?;
    write_json_atomic(&out, &artifact)
        .map_err(|e| anyhow::anyhow!("writing {}: {e}", out.display()))?;

    let summary = serde_json::json!({
        "job_id": artifact.job_id,
        "rows": artifact.rows,
        "parents_seen": artifact.parents_seen,
        "unsided_share": artifact.unsided_share,
        "invalid_width_share": artifact.invalid_width_share,
        "rows_outside_declared_sessions": artifact.rows_outside_declared_sessions,
        "usable_sessions": artifact.usable_sessions.len(),
        "excluded_sessions": artifact.excluded_sessions.len(),
        "subcontract_hash": artifact.subcontract_hash,
    });
    println!("{}", serde_json::to_string_pretty(&summary)?);
    println!("preflight PASS -> {}", out.display());
    Ok(())
}
