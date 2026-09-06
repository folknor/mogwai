// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Re-bless `analysis/stage-a-batch-manifest.json` after a tape protocol
//! bump.
//!
//! The manifest hashes over `TAPE_PROTOCOL_VERSION`, so every bump moves its
//! identity by construction while the panel itself stays put. This rewrites
//! the version fields to the live constants and recomputes `plan_sha256`, and
//! nothing else: a diff of the result against the committed file must show
//! exactly those lines. The self-consistency test then carries the new digest
//! as its literal, so update that literal from what this prints. Run it as
//! the `rebless_stage_a_manifest` example of this crate.

use std::error::Error;
use std::path::PathBuf;

use mogwai_lab::stage_a_batch::{BatchManifest, manifest_hash, validate_manifest};

fn main() -> Result<(), Box<dyn Error>> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../analysis/stage-a-batch-manifest.json");
    let bytes = std::fs::read(&path)?;
    let mut manifest: BatchManifest = serde_json::from_slice(&bytes)?;
    let before = manifest.plan_sha256.clone();
    manifest.tape_protocol_version = mogwai_data::TAPE_PROTOCOL_VERSION;
    manifest.arrival_kernel_version = mogwai_data::ARRIVAL_KERNEL_VERSION;
    manifest.plan_sha256 = manifest_hash(&manifest)?;
    validate_manifest(&manifest)?;
    let mut text = serde_json::to_string_pretty(&manifest)?;
    text.push('\n');
    std::fs::write(&path, text)?;
    println!(
        "tape protocol {} kernel {}: plan_sha256 {} -> {}",
        manifest.tape_protocol_version,
        manifest.arrival_kernel_version,
        before,
        manifest.plan_sha256
    );
    Ok(())
}
