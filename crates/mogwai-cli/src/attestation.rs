// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! THE SECOND OF THE TWO GUARDS on the tree-state seam, in one place so the
//! set of artifact writers that carry it is countable.
//!
//! `mogwai_lab::ledger`'s `TreeOracle` seam is compiled out of a production
//! build - it lives behind the lab's `test-seam` feature, which `mogwai-cli`
//! enables only in `[dev-dependencies]`. That is the first guard. It is not
//! the only one needed, because an `--all-features` build switches the seam
//! back on, and one installed double would then attest BOTH ends of an
//! artifact's provenance claim: the `require_clean_tree` that opens the run
//! and the `fresh_tree_state` re-attestation that precedes the write.
//!
//! So every command that writes `binding.harness_tree_commit` calls
//! [`refuse_scripted_tree_attestation`] immediately before its write. The
//! guard was originally inlined in two of them and the module comment on the
//! seam claimed the set was two; it is SIX - `arrival_control`,
//! `arrival_screen`, `fit`, `measure`, `minute_range_envelope` and
//! `arrival_envelope_diagnostic` - and a bare copy per writer is exactly the
//! shape that lets the seventh be forgotten. Adding a writer means calling
//! this; `tree_writers_all_refuse_a_scripted_attestation` in this module
//! holds the roster against the sources so a new call site that forgets is
//! named rather than assumed. It sees the five writers that serialize the
//! binding HERE; `fit` is the sixth and is invisible to it, because the
//! artifact it binds is serialized by `mogwai_lab::fit::driver` out of the
//! commit `fit::run` hands it. A writer that delegates its serialization the
//! same way is likewise the reviewer's to catch.
//!
//! AND THE SEAM IS NOT THE ONLY WAY THE CLAIM GOES FALSE. A writer that takes
//! `require_clean_tree` at entry and then runs for minutes binds a commit that
//! may no longer be checked out by the time the bytes land - no double
//! required, just a `git checkout` in another shell.
//! `every_tree_attested_writer_re_attests_before_its_write` holds that half:
//! every entry-gated writer re-reads the tree immediately before its write and
//! declares the artifact unbound when it moved. `fit` and
//! `minute_range_envelope` were missing it, and `fit`'s own module doc claimed
//! it carried "the same contract `mogwai measure` carries" while not carrying
//! it.

use anyhow::{Result, bail};

/// Refuse to write a provenance-bearing artifact when the tree readings this
/// thread would get do not come from `git` itself.
///
/// In a production build this is a call to a `const`-true function and
/// optimizes away; it costs a branch only in a build that compiled the seam.
pub fn refuse_scripted_tree_attestation() -> Result<()> {
    if !mogwai_lab::ledger::tree_readings_are_production() {
        bail!(
            "a scripted tree reader is installed; an artifact binding may only be attested by git \
             itself"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::rc::Rc;

    use mogwai_lab::ledger::{ScriptedTree, install_tree_oracle};

    /// The guard itself, in both directions.
    #[test]
    fn a_scripted_reader_is_refused_and_a_real_one_is_not() {
        super::refuse_scripted_tree_attestation().expect("git itself attests");
        let _guard = install_tree_oracle(Rc::new(ScriptedTree::clean("c1ean")));
        let err = super::refuse_scripted_tree_attestation()
            .expect_err("a scripted reader may not attest a binding");
        assert!(err.to_string().contains("scripted tree reader"), "{err}");
    }

    /// THE ROSTER IS HELD AGAINST THE SOURCES, because nothing else can
    /// detect the defect this module exists to close: a command that writes
    /// `harness_tree_commit` and never calls the guard reads as gated and is
    /// not. Every source in this crate that writes the key must also name the
    /// guard, and the check is on the FILE SET rather than on a list of
    /// expected names, so a new writer is caught without anybody remembering
    /// to extend a list.
    ///
    /// The sources are read from `CARGO_MANIFEST_DIR`, which is this crate's
    /// directory whether the test runs as a unit test or an integration one.
    #[test]
    fn tree_writers_all_refuse_a_scripted_attestation() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut writers = Vec::new();
        let mut ungated = Vec::new();
        for entry in std::fs::read_dir(&src).expect("the crate's src directory") {
            let path = entry.expect("a directory entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().expect("a file name").to_string_lossy();
            if name == "attestation.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a source file");
            // The WRITE of the binding, not a mention of it: the test modules
            // and the doc comments in this crate name both keys repeatedly.
            // Either key counts - `arrival_envelope_diagnostic` names its
            // commit field `commit` and carries `clean_tree` beside it.
            if !text.contains("\"harness_tree_commit\":") && !text.contains("\"clean_tree\":") {
                continue;
            }
            writers.push(name.to_string());
            if !text.contains("refuse_scripted_tree_attestation") {
                ungated.push(name.to_string());
            }
        }
        assert!(
            ungated.is_empty(),
            "these commands write a tree-attested binding without refusing a scripted reader: \
             {ungated:?}"
        );
        // And the roster is not empty, so a rename of the key cannot turn this
        // into a test over nothing.
        assert!(
            writers.len() >= 4,
            "the roster collapsed to {writers:?}; the binding key was probably renamed"
        );
    }

    /// THE OTHER HALF OF THE SAME CONTRACT, and it went missing in two writers
    /// of six before anything looked. A tree-attested binding names the commit
    /// a gate read at the TOP of a run that then takes minutes; the claim is
    /// only true if the tree is read again immediately before the write and
    /// the run refuses when it moved. `measure`, `arrival-control`,
    /// `arrival-screen` and `arrival-envelope-diagnostic` did this; `fit` and
    /// `minute-range-envelope` took the entry gate and wrote whatever it said,
    /// while `fit`'s own module doc asserted it carried "the same contract
    /// `mogwai measure` carries". A doc stating an attestation the code does
    /// not perform is worse than no doc.
    ///
    /// THE ROSTER IS KEYED ON THE GUARD CALL, not on the binding key, and that
    /// is deliberate: `fit.rs` does not contain `"harness_tree_commit"` at
    /// all, because `mogwai_lab::fit::driver` serializes it out of the commit
    /// `fit::run` hands over, so the roster above cannot see it and this one
    /// can. The two together cover the set from both directions.
    #[test]
    fn every_tree_attested_writer_re_attests_before_its_write() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut writers = Vec::new();
        let mut unattested = Vec::new();
        for entry in std::fs::read_dir(&src).expect("the crate's src directory") {
            let path = entry.expect("a directory entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                continue;
            }
            let name = path.file_name().expect("a file name").to_string_lossy();
            if name == "attestation.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("a source file");
            if !text.contains("refuse_scripted_tree_attestation()") {
                continue;
            }
            // THE DEFECT IS AN ENTRY GATE WHOSE VERDICT IS RESTATED AT THE
            // WRITE, so a writer that takes no entry gate is a different
            // shape and not one of these. `arrival_envelope_diagnostic` reads
            // the tree ONCE, immediately before assembling, and records
            // `clean_tree: <what it read>` rather than a constant `true` - a
            // MEASUREMENT, which cannot go stale between a gate and a write
            // because there is no gate. Requiring a refusal there would
            // demand it invent a claim it deliberately does not make.
            if !text.contains("require_clean_tree()") {
                continue;
            }
            writers.push(name.to_string());
            // The re-read, and the refusal that makes reading it mean
            // something. A `fresh_tree_state` whose verdict is discarded is
            // the same defect wearing the fix's clothes.
            if !text.contains("fresh_tree_state()") || !text.contains("the artifact is unbound") {
                unattested.push(name.to_string());
            }
        }
        assert!(
            unattested.is_empty(),
            "these commands bind a tree-attested artifact from a gate taken at entry and never \
             re-attest before the write, so the commit they name may not be what is checked out \
             by the time the bytes land: {unattested:?}"
        );
        assert!(
            writers.len() >= 4,
            "the roster collapsed to {writers:?}; the guard or the entry gate was probably renamed"
        );
    }
}
