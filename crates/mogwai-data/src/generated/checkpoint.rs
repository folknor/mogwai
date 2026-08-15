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
/// `extend_toward` would push past this, `coarsen` halves the count and doubles the
/// spacing, so the index's memory is bounded by `MAX_CHECKPOINTS` generator
/// clones regardless of how long an accelerated session runs - closing the
/// unbounded per-`k`-ticks growth. 4096 keeps coarsening rare (the first only
/// after `4096 * k` ticks, ~34M ticks at the server's K of 8192) so the
/// residual drain stays at the base `k` for any realistic run, while capping
/// worst-case mutable-state memory. Immutable generator scalars and the session
/// calendar are shared by the lead and every snapshot.
pub(super) const MAX_CHECKPOINTS: usize = 4096;

/// One retained walk state.
///
/// `pinned` marks a CONTROL BOUNDARY: the snapshot taken the instant a
/// `FlowSurge` was armed or cleared. Surge state lives in the generator, so a
/// snapshot taken BEFORE an arm replays the span after it unsurged, which is
/// precisely the realization fork the surge-on-the-canonical-tape change exists
/// to remove. A target landing between an arm and the next ordinary snapshot
/// can only be answered correctly from the boundary snapshot, so `coarsen` may
/// not drop it on the ordinary every-other rule.
struct Snapshot {
    source: GeneratedSource,
    pinned: bool,
}

pub struct CheckpointIndex {
    /// A generator advanced to the frontier; cloned to extend the chain and to
    /// hand out positioned sources. Immutable config is reference-counted by
    /// `GeneratedSource`, so cloning the lead copies only mutable walk state.
    lead: GeneratedSource,
    /// Snapshots in ascending `clock_ns`; `[0]` is the origin (pre-first-tick).
    checkpoints: Vec<Snapshot>,
    /// Ticks the lead has advanced since the last snapshot was taken.
    since_snapshot: usize,
    /// Snapshot spacing in ticks.
    k: usize,
    /// Runaway backstop: the most ticks `extend_toward` will walk the lead in a
    /// single call. The server refuses a `start` below `data_origin`, but
    /// nothing rejects an absurd `start` *above* the live frontier (a bogus or
    /// far-future window), and `GeneratedSource::next_tick` never ends - so an
    /// uncapped `extend_toward` would spin the path-dependent walk indefinitely
    /// while holding the shared index mutex. A target past this bound leaves the
    /// frontier short; `try_source_at_or_before` reports that refusal instead of
    /// handing a caller a source which could then seek forever. Sized to the
    /// same budget as the from-origin cap, so every legitimate target (warmup,
    /// live sim-now, a poll's modest per-step delta) sits far inside it.
    max_extend: usize,
    /// Once the paced worker owns the lead, readers may clone it but never
    /// extend it. Otherwise a history request just ahead of the feed steals
    /// ticks the worker has not published.
    live: bool,
}

impl CheckpointIndex {
    /// Advance the canonical frontier by one wire tick while retaining the
    /// checkpoint cadence used by historical readers.
    pub fn next_tick(&mut self) -> Option<crate::TickEvent> {
        let tick = self.lead.next_tick()?;
        self.since_snapshot += 1;
        if self.since_snapshot >= self.k {
            self.snapshot(false);
        }
        Some(tick)
    }

    pub fn arm_flow_surge(
        &mut self,
        start_ns: u64,
        duration_ms: u64,
        rate_mult: f64,
        children_mult: f64,
    ) {
        self.lead
            .arm_flow_surge(start_ns, duration_ms, rate_mult, children_mult);
        self.checkpoint_control_boundary();
    }

    pub fn clear_flow_surge(&mut self) {
        self.lead.clear_flow_surge();
        self.checkpoint_control_boundary();
    }

    #[must_use]
    pub fn fault(&self) -> Option<crate::TickFault> {
        self.lead.fault()
    }

    fn checkpoint_control_boundary(&mut self) {
        self.snapshot(true);
    }

    /// Retain the lead's current walk state and reset the snapshot cadence.
    fn snapshot(&mut self, pinned: bool) {
        self.checkpoints.push(Snapshot {
            source: self.lead.clone(),
            pinned,
        });
        self.since_snapshot = 0;
        if self.checkpoints.len() > MAX_CHECKPOINTS {
            self.coarsen();
        }
    }

    /// Build an index over the realization `origin` heads. `origin` must be a
    /// fresh source at the tape origin (no ticks drawn yet); its pre-first-tick
    /// state becomes checkpoint 0. `max_extend` bounds the per-call walk (see the
    /// field doc) - pass the caller's from-origin seek cap.
    #[must_use]
    pub fn new(origin: GeneratedSource, k: usize, max_extend: usize) -> Self {
        assert!(k > 0, "checkpoint spacing must be positive");
        assert!(max_extend > 0, "extension cap must be positive");
        Self {
            checkpoints: vec![Snapshot {
                source: origin.clone(),
                pinned: true,
            }],
            lead: origin,
            since_snapshot: 0,
            k,
            max_extend,
            live: false,
        }
    }

    /// Extend the snapshot chain until it covers `target`, advancing the lead and
    /// snapshotting every `k` ticks. Monotonic: a later, further target only does
    /// the new delta, so the from-origin walk is paid once across all seeks. The
    /// walk is bounded by `max_extend` per call (the runaway backstop); a target
    /// beyond that leaves the lead short, which `try_source_at_or_before`
    /// REFUSES rather than papering over. Nothing caps the seek downstream of a
    /// positioned source, so handing one out short is what would hang.
    pub fn extend_toward(&mut self, target: u64) -> usize {
        let mut walked = 0usize;
        while self.lead.clock_ns() < target {
            if walked >= self.max_extend {
                break;
            }
            let remaining_to_snapshot = self.k - self.since_snapshot;
            let remaining_budget = self.max_extend - walked;
            if self.lead.at_parent_boundary() {
                let mut advanced = self.lead.clone();
                let parent = advanced.advance_parent();
                let parent_ticks = 1usize.saturating_add(parent.child_count as usize);
                let parent_end = parent.parent_ts_ns.saturating_add(
                    u64::from(parent.child_count.saturating_sub(1))
                        .saturating_mul(parent.child_stride_ns),
                );
                if parent_end < target
                    && parent_ticks <= remaining_to_snapshot
                    && parent_ticks <= remaining_budget
                {
                    self.lead = advanced;
                    walked += parent_ticks;
                    self.since_snapshot += parent_ticks;
                    if self.since_snapshot == self.k {
                        self.snapshot(false);
                    }
                    continue;
                }
            }
            if self.lead.next_tick().is_none() {
                break;
            }
            walked += 1;
            self.since_snapshot += 1;
            if self.since_snapshot >= self.k {
                self.snapshot(false);
            }
        }
        walked
    }

    /// Walk toward `target`, unless the paced worker owns the lead.
    /// The check and extension share one mutable borrow so a reader cannot
    /// observe a cold index and then extend it after live activation.
    pub fn extend_toward_unless_live(&mut self, target: u64) -> Option<usize> {
        (!self.live).then(|| self.extend_toward(target))
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
    /// coarsened `k` grows only logarithmically in session length (a doubling
    /// costs `MAX_CHECKPOINTS * k` more ticks).
    ///
    /// PINNED snapshots are exempt from the every-other rule, because dropping
    /// one is NOT correctness-preserving: a control boundary is the only
    /// snapshot carrying the surge state a target just past an arm must replay
    /// under. The exemption cannot break the memory ceiling - if pins alone fill
    /// the chain the tail below drops the OLDEST of them, trading replay
    /// fidelity for the most ancient surge windows rather than the bound - and
    /// `k` doubles only when the every-other pass actually removed something,
    /// so pin pressure cannot inflate the residual drain.
    fn coarsen(&mut self) {
        let before = self.checkpoints.len();
        let mut idx = 0usize;
        self.checkpoints.retain(|snapshot| {
            let keep = snapshot.pinned || idx.is_multiple_of(2);
            idx += 1;
            keep
        });
        if self.checkpoints.len() < before {
            self.k = self.k.saturating_mul(2);
        }
        while self.checkpoints.len() > MAX_CHECKPOINTS {
            self.checkpoints.remove(1);
        }
    }

    /// A fresh generator positioned at the latest checkpoint strictly before
    /// `target` (or the origin when nothing is). The caller drains it forward to
    /// the exact target (< K ticks) via the normal seek; the returned source is
    /// an independent clone, so the shared index is untouched by that replay.
    pub fn try_source_at_or_before(&mut self, target: u64) -> Option<GeneratedSource> {
        self.try_source_before_target(target, 0)
            .map(|(source, _)| source)
    }

    /// The same positioning, but `back` snapshots EARLIER than the strict
    /// partition point, and reporting whether the result is the EARLIEST
    /// snapshot the walk-back may use - the origin, or the nearest `FlowSurge`
    /// control boundary when one fences the retreat.
    ///
    /// A positioned source only re-emits ticks the resume point had not yet
    /// consumed, so a reader recovering state the snapshot itself consumed - the
    /// last trade price at or before `target`, say - can find nothing to read
    /// even though the answer exists. That reader is not wrong to have asked,
    /// and must not be told "no such print": it walks back until it finds one or
    /// until it hits the floor. Returning the floor flag is what lets the caller
    /// stop without guessing at the chain's length; a FENCED caller then reads
    /// the consumed print off the boundary snapshot's own state
    /// (`GeneratedSource::last_trade_price`) rather than being refused forever.
    pub fn try_source_before_target(
        &mut self,
        target: u64,
        back: usize,
    ) -> Option<(GeneratedSource, bool)> {
        if !self.live {
            self.extend_toward(target);
        }
        if self.lead.clock_ns() < target {
            return None;
        }
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
            .iter()
            .rposition(|checkpoint| checkpoint.source.clock_ns() < target)
            .unwrap_or(0);
        // The walk-back stops at the nearest CONTROL BOUNDARY at or below the
        // partition point. Resuming earlier than an arm and replaying across it
        // would regenerate the span unsurged - the exact realization fork the
        // canonical-tape surge removes - so a reader hunting backwards for a
        // consumed print must not be handed a source that would answer from a
        // different tape. The origin is pinned, so an index that never armed a
        // surge walks all the way back as before.
        let floor = self.checkpoints[..=idx]
            .iter()
            .rposition(|checkpoint| checkpoint.pinned)
            .unwrap_or(0);
        let idx = idx.saturating_sub(back).max(floor);
        Some((self.checkpoints[idx].source.clone(), idx == floor))
    }

    /// Position at a reachable target, panicking rather than returning a
    /// short source that an unbounded downstream seek could walk forever.
    pub fn source_at_or_before(&mut self, target: u64) -> GeneratedSource {
        self.try_source_at_or_before(target)
            .expect("checkpoint extension cap left target unreachable")
    }

    /// Number of snapshots held (origin included). For tests and the measurement.
    #[must_use]
    pub fn checkpoint_count(&self) -> usize {
        self.checkpoints.len()
    }

    /// Timestamp reached by the shared lead after the latest bounded extension.
    #[must_use]
    pub fn frontier_ns(&self) -> u64 {
        self.lead.clock_ns()
    }

    /// Transfer frontier advancement from warmup/history positioning to the
    /// paced worker. Idempotent so startup validation may call it once only.
    pub fn activate_live(&mut self) {
        self.live = true;
    }

    /// Snapshots retained as `FlowSurge` control boundaries, origin included.
    #[cfg(test)]
    pub(crate) fn pinned_count(&self) -> usize {
        self.checkpoints
            .iter()
            .filter(|checkpoint| checkpoint.pinned)
            .count()
    }

    #[cfg(test)]
    pub(crate) fn frontier_source(&self) -> GeneratedSource {
        self.lead.clone()
    }
}
