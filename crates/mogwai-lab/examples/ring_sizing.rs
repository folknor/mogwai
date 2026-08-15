// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Measures the eager resident and allocator cost of one shipped per-boat ring.

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static ALLOC: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

/// The SHIPPED per-boat depth, read from the config default rather than
/// restated: a measurement that can drift from the knob it verifies measures
/// nothing.
fn depth() -> usize {
    mogwai_server::config::DEFAULT_FANOUT_DEPTH
}

fn rss_bytes() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").expect("read /proc/self/status");
    status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .and_then(|value| value.parse::<u64>().ok())
        .expect("VmRSS is present")
        * 1024
}

#[cfg_attr(feature = "hotpath", hotpath::measure)]
fn allocate_ring(depth: usize) -> tokio::sync::broadcast::Sender<u64> {
    tokio::sync::broadcast::channel(depth).0
}

fn main() {
    #[cfg(feature = "hotpath")]
    let _guard = hotpath::HotpathGuardBuilder::new("main").build();
    mogwai_lab::sidecar::init();
    let depth = depth();
    let before = rss_bytes();
    let ring = allocate_ring(depth);
    let after = rss_bytes();
    mogwai_lab::sidecar::kv("ring_depth", depth);
    mogwai_lab::sidecar::kv("ring_resident_bytes", after.saturating_sub(before));
    std::hint::black_box(ring);
}
