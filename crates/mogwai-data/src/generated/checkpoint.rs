// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use crate::TickSource;

use super::source::GeneratedSource;

/// Turns the from-origin seek from O(distance) into O(K): instead of re-walking
/// the path-dependent generator from the tape origin to a target, snapshot the
/// walk (a `GeneratedSource` clone) every `K` ticks and, to reach a target,
/// resume from the latest snapshot at or before it and replay only the residual
/// (< K ticks).
///
/// This lifts the accelerated uptime ceiling Landing 1 priced: a from-origin
/// seek to sim-now grows with session length and eventually blows the backstop
/// cap, but a resume-and-replay is flat in `K` no matter how far the session has
/// run. The index is shared per realization (one symbol's clean tape) and
/// extended lazily and monotonically, so the O(span) walk to the frontier is
/// paid once across all of that realization's seeks, never per request.
///
/// The realization is preserved byte-for-byte: a snapshot is the exact walk
/// state, so resuming it and replaying yields the same ticks a from-origin run
/// would (the golden sequence is unchanged, and `checkpoint_resume_is_byte_identical`
/// pins the resume path directly).
/// Hard ceiling on the number of snapshots one `CheckpointIndex` retains. Once
/// `extend_to` would push past this, `coarsen` halves the count and doubles the
/// spacing, so the index's memory is bounded by `MAX_CHECKPOINTS` generator
/// clones regardless of how long an accelerated session runs - closing the
/// unbounded per-`k`-ticks growth. 4096 keeps coarsening rare (the first only
/// after `4096 * k` ticks, ~34M ticks at the server's K = 8192) so the residual
/// drain stays at the base `k` for any realistic run, while capping worst-case
/// memory at a few tens of MB.
pub(super) const MAX_CHECKPOINTS: usize = 4096;

pub struct CheckpointIndex {
    /// A generator advanced to the frontier; cloned to extend the chain and to
    /// hand out positioned sources. Carries the immutable config every snapshot
    /// shares.
    lead: GeneratedSource,
    /// Snapshots in ascending `clock_ns`; `[0]` is the origin (pre-first-tick).
    checkpoints: Vec<GeneratedSource>,
    /// Ticks the lead has advanced since the last snapshot was taken.
    since_snapshot: usize,
    /// Snapshot spacing in ticks.
    k: usize,
    /// Runaway backstop: the most ticks `extend_to` will walk the lead in a
    /// single call. The server refuses a `start` below `data_origin`, but
    /// nothing rejects an absurd `start` *above* the live frontier (a bogus or
    /// far-future window), and `GeneratedSource::next_tick` never ends - so an
    /// uncapped `extend_to` would spin the path-dependent walk indefinitely
    /// while holding the shared index mutex. A target past this bound leaves the
    /// frontier short; the caller's own `BoundedSeek` then caps too and the seek
    /// yields an empty page instead of hanging. Sized to the same budget as the
    /// from-origin cap, so every legitimate target (warmup, live sim-now, a
    /// poll's modest per-step delta) sits far inside it.
    max_extend: usize,
}

impl CheckpointIndex {
    /// Build an index over the realization `origin` heads. `origin` must be a
    /// fresh source at the tape origin (no ticks drawn yet); its pre-first-tick
    /// state becomes checkpoint 0. `max_extend` bounds the per-call walk (see the
    /// field doc) - pass the caller's from-origin seek cap.
    #[must_use]
    pub fn new(origin: GeneratedSource, k: usize, max_extend: usize) -> Self {
        assert!(k > 0, "checkpoint spacing must be positive");
        assert!(max_extend > 0, "extension cap must be positive");
        Self {
            checkpoints: vec![origin.clone()],
            lead: origin,
            since_snapshot: 0,
            k,
            max_extend,
        }
    }

    /// Extend the snapshot chain until it covers `target`, advancing the lead and
    /// snapshotting every `k` ticks. Monotonic: a later, further target only does
    /// the new delta, so the from-origin walk is paid once across all seeks. The
    /// walk is bounded by `max_extend` per call (the runaway backstop); a target
    /// beyond that leaves the lead short and the caller's seek caps the rest.
    fn extend_to(&mut self, target: u64) {
        let mut walked = 0usize;
        while self.lead.clock_ns() < target {
            if walked >= self.max_extend {
                break;
            }
            if self.lead.next_tick().is_none() {
                break;
            }
            walked += 1;
            self.since_snapshot += 1;
            if self.since_snapshot >= self.k {
                self.checkpoints.push(self.lead.clone());
                self.since_snapshot = 0;
                if self.checkpoints.len() > MAX_CHECKPOINTS {
                    self.coarsen();
                }
            }
        }
    }

    /// Halve the snapshot count once it exceeds `MAX_CHECKPOINTS` by dropping
    /// every other checkpoint and doubling the spacing `k`. This is what makes
    /// the index's memory a HARD ceiling (`MAX_CHECKPOINTS` generator clones)
    /// over any session length, rather than a clone per `k` ticks growing
    /// without bound.
    ///
    /// It is correctness-preserving: every retained checkpoint is still the
    /// EXACT walk state at its `clock_ns`, so resuming from the coarser grid and
    /// replaying reproduces the identical tape - dropping an intermediate
    /// snapshot only lengthens the residual drain (`source_at_or_before` now
    /// resumes up to the new, larger `k` ticks before the target), it never
    /// changes which ticks are emitted. The origin (index 0) is always retained
    /// as the pre-first-tick fallback. The residual drain stays bounded by the
    /// caller's `BoundedSeek`; `k` grows only logarithmically in session length
    /// (a doubling costs `MAX_CHECKPOINTS * k` more ticks), so it never
    /// realistically approaches that cap.
    fn coarsen(&mut self) {
        let mut idx = 0usize;
        self.checkpoints.retain(|_| {
            let keep = idx.is_multiple_of(2);
            idx += 1;
            keep
        });
        self.k = self.k.saturating_mul(2);
    }

    /// A fresh generator positioned at the latest checkpoint strictly before
    /// `target` (or the origin when nothing is). The caller drains it forward to
    /// the exact target (< K ticks) via the normal seek; the returned source is
    /// an independent clone, so the shared index is untouched by that replay.
    pub fn source_at_or_before(&mut self, target: u64) -> GeneratedSource {
        self.extend_to(target);
        // Strictly-before partition (`<`, not `<=`): a checkpoint's `clock_ns`
        // is the `ts_event` of the last tick it has ALREADY consumed, so a
        // checkpoint whose `clock_ns` EQUALS the target has the boundary tick
        // behind it. Resuming there and seeking to `target` (the trait-default
        // seek returns the first tick with `ts_event >= target`) would skip
        // that boundary tick, while a from-origin seek returns it - the two
        // paths the byte-identical guarantee promises are one tape would
        // disagree by exactly one tick. The collision is not hypothetical:
        // snapshots land on every K-th tick's exact `ts_event`, and pollers
        // legitimately pass an emitted tick's exact `ts_event` as the seek
        // target, so under `<=` one tick per ~K such seeks would vanish. With
        // `<` the resume point sits strictly before the target and the
        // residual replay re-emits the boundary tick itself
        // (`checkpoint_resume_at_exact_boundary_ts_returns_boundary_tick` pins
        // this). When `target` is at or before the origin's clock the
        // partition point is 0 and the `saturating_sub` keeps us on the
        // origin, which has emitted nothing and is therefore always a safe
        // resume point.
        let idx = self
            .checkpoints
            .partition_point(|c| c.clock_ns() < target)
            .saturating_sub(1);
        self.checkpoints[idx].clone()
    }

    /// Number of snapshots held (origin included). For tests and the measurement.
    #[must_use]
    pub fn checkpoint_count(&self) -> usize {
        self.checkpoints.len()
    }
}
