// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Slice 2c-ii of the retired rewrite plan: `_ResourceSampler`, ported
//! from the retired Python fit implementation - a 1 s background thread sampling this
//! process tree's RSS and a set of on-disk scratch paths, peaks retained.
//! The Python comment about the walk subprocesses being children is now
//! moot (the in-process port runs no subprocess for the walks), but the
//! process-tree walk stays general rather than narrowing to "just read
//! `/proc/self/status`" - a future caller may still spawn children.
//!
//! Any death of the sampling thread voids the cost attestation:
//! [`ResourceSampler::stop`]
//! refuses rather than reporting a peak measured over a partial window.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::error::{LabError, LabResult};

/// `VmRSS` summed over `pid` and its live descendants, one sample. A
/// vanished process (the sampler races exits) reads as zero rather than
/// aborting the walk.
fn tree_rss_bytes(pid: u32) -> u64 {
    let mut total: u64 = 0;
    let mut stack = vec![pid];
    while let Some(p) = stack.pop() {
        if let Ok(status) = std::fs::read_to_string(format!("/proc/{p}/status")) {
            for line in status.lines() {
                if let Some(rest) = line.strip_prefix("VmRSS:") {
                    let kb: u64 = rest
                        .split_whitespace()
                        .next()
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    total += kb * 1024;
                    break;
                }
            }
        } else {
            continue;
        }
        if let Ok(children) = std::fs::read_to_string(format!("/proc/{p}/task/{p}/children")) {
            stack.extend(
                children
                    .split_whitespace()
                    .filter_map(|c| c.parse::<u32>().ok()),
            );
        }
    }
    total
}

/// Total on-disk size over `paths`, tolerating a per-file race (a scratch
/// temporary can appear and vanish between listing and stat-ing it) rather
/// than treating a single missing file as fatal.
fn scratch_bytes(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|p| std::fs::metadata(p).ok())
        .map(|m| m.len())
        .sum()
}

/// A directory's current files, resolved fresh at sample time (the Python
/// re-lists `MEASURE12A_CACHE_DIR` on every sample rather than snapshotting
/// it once, so files that appear mid-run are counted).
fn scratch_paths(fixed: &[PathBuf], scan_dir: Option<&std::path::Path>) -> Vec<PathBuf> {
    let mut out = fixed.to_vec();
    if let Some(dir) = scan_dir
        && let Ok(entries) = std::fs::read_dir(dir)
    {
        out.extend(
            entries
                .filter_map(std::result::Result::ok)
                .map(|e| e.path()),
        );
    }
    out
}

pub struct ResourceSampler {
    peak_rss: Arc<AtomicU64>,
    peak_scratch: Arc<AtomicU64>,
    failed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ResourceSampler {
    /// Starts the 1 s background sample loop. `fixed_scratch_paths` are
    /// always sampled (the observed cache file); `scan_dir`, if given, is
    /// re-listed on every sample (the walk cache directory).
    #[must_use]
    pub fn start(fixed_scratch_paths: Vec<PathBuf>, scan_dir: Option<PathBuf>) -> Self {
        let peak_rss = Arc::new(AtomicU64::new(0));
        let peak_scratch = Arc::new(AtomicU64::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let pid = std::process::id();

        let (rss2, scratch2, failed2, stop2) = (
            Arc::clone(&peak_rss),
            Arc::clone(&peak_scratch),
            Arc::clone(&failed),
            Arc::clone(&stop),
        );
        let handle = std::thread::spawn(move || {
            // `std::fs` calls can only fail with an `io::Error`, which this
            // loop already treats as "sample as zero and continue" (a
            // vanished file, not a sampler death) - so the Python's
            // catch-all "any exception voids the run" degrades here to
            // "there is no way for this loop to panic", and `failed` exists
            // for future sampling that CAN fail outright.
            while !stop2.load(Ordering::Relaxed) {
                sample_once(
                    pid,
                    &fixed_scratch_paths,
                    scan_dir.as_deref(),
                    &rss2,
                    &scratch2,
                );
                std::thread::sleep(Duration::from_secs(1));
            }
            let _ = &failed2; // reserved for a future fallible sample source
        });

        Self {
            peak_rss,
            peak_scratch,
            failed,
            stop,
            handle: Some(handle),
        }
    }

    /// Take a synchronous sample and return the peak RSS observed so far.
    /// Long-running drivers call this at work-unit boundaries so a ceiling is
    /// enforced during the run rather than merely reported after it.
    pub fn sample_peak_rss(
        &self,
        fixed_scratch_paths: &[PathBuf],
        scan_dir: Option<&std::path::Path>,
    ) -> u64 {
        sample_once(
            std::process::id(),
            fixed_scratch_paths,
            scan_dir,
            &self.peak_rss,
            &self.peak_scratch,
        );
        self.peak_rss.load(Ordering::Relaxed)
    }

    /// Stops the loop, joins the thread, takes one final sample (matching
    /// the Python's `stop()` doing one last `self.sample()`), and returns
    /// `(peak_rss_bytes, peak_scratch_bytes)`. Refuses if the sampling
    /// thread died.
    pub fn stop(
        mut self,
        fixed_scratch_paths: &[PathBuf],
        scan_dir: Option<&std::path::Path>,
    ) -> LabResult<(u64, u64)> {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            h.join()
                .map_err(|_| LabError::refusal("the resource sampler thread panicked"))?;
        }
        if self.failed.load(Ordering::Relaxed) {
            return Err(LabError::refusal("the resource sampler died"));
        }
        sample_once(
            std::process::id(),
            fixed_scratch_paths,
            scan_dir,
            &self.peak_rss,
            &self.peak_scratch,
        );
        Ok((
            self.peak_rss.load(Ordering::Relaxed),
            self.peak_scratch.load(Ordering::Relaxed),
        ))
    }
}

/// The guard-scope rule applied to a thread. `stop` is the only thing that
/// ever set `stop` and joined, and it sits at the end of `arrival_control`'s
/// `run_with` - so every `?` between the `start` and it left the 1 Hz sample
/// loop running for the life of the process. That was latent while the
/// early-return paths were only reachable from a real operator run that then
/// exits; it is not latent any more, because the round-2 clean-direction
/// assertion drives one of those early returns in-process, inside a test
/// binary that goes on to run every other test in the crate.
///
/// Idempotent with `stop`, which takes the handle before this runs.
impl Drop for ResourceSampler {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            // A panicked sampler thread is `stop`'s verdict to deliver, not a
            // destructor's: unwinding out of `drop` during another unwind
            // aborts the process.
            drop(h.join());
        }
    }
}

fn sample_once(
    pid: u32,
    fixed: &[PathBuf],
    scan_dir: Option<&std::path::Path>,
    peak_rss: &AtomicU64,
    peak_scratch: &AtomicU64,
) {
    let rss = tree_rss_bytes(pid);
    peak_rss.fetch_max(rss, Ordering::Relaxed);
    let scratch = scratch_bytes(&scratch_paths(fixed, scan_dir));
    peak_scratch.fetch_max(scratch, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sampler dropped without `stop` - which is every `?` between the
    /// `start` and the `stop` in `arrival_control`'s `run_with` - takes its
    /// thread with it. Asserted on the thread rather than on the absence of an
    /// error: the leak is silent by construction, and only the loop's own exit
    /// distinguishes "cleaned up" from "still sampling forever".
    #[test]
    fn a_dropped_sampler_stops_its_thread_rather_than_leaking_it() {
        let sampler = ResourceSampler::start(Vec::new(), None);
        let stop = Arc::clone(&sampler.stop);
        assert!(
            sampler.handle.is_some(),
            "the sample loop is running before the drop"
        );
        assert!(!stop.load(Ordering::Relaxed), "the loop starts unstopped");
        drop(sampler);
        assert!(
            stop.load(Ordering::Relaxed),
            "the drop must ask the sample loop to finish"
        );
        // The join is not asserted, and saying so is better than a predicate
        // that cannot fail: a `JoinHandle` consumed inside `drop` leaves this
        // test nothing to observe it through. The flag is the half that is
        // observable, and it is the half that was missing.
    }

    #[test]
    fn samples_this_process_and_stops_cleanly() {
        let sampler = ResourceSampler::start(Vec::new(), None);
        std::thread::sleep(Duration::from_millis(50));
        let (rss, scratch) = sampler.stop(&[], None).expect("the sampler did not die");
        assert!(rss > 0, "this process's own RSS must be nonzero");
        assert_eq!(scratch, 0, "no scratch paths were given");
    }

    #[test]
    fn scratch_bytes_tolerates_a_missing_file() {
        let missing = PathBuf::from("target/mogwai-lab-sampler-test-does-not-exist");
        assert_eq!(scratch_bytes(&[missing]), 0);
    }
}
