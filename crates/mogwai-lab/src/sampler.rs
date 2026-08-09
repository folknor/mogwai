// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Slice 2c-ii of the retired rewrite plan: `_ResourceSampler`, ported
//! from `analysis/mnq_fit.py` - a 1 s background thread sampling this
//! process tree's RSS and a set of on-disk scratch paths, peaks retained.
//! The Python comment about the walk subprocesses being children is now
//! moot (the in-process port runs no subprocess for the walks), but the
//! process-tree walk stays general rather than narrowing to "just read
//! `/proc/self/status`" - a future caller may still spawn children.
//!
//! Any death of the sampling thread VOIDS the cost attestation: [`stop`]
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
