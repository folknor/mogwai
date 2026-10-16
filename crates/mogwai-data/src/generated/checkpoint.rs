// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use super::source::GeneratedSource;

/// Turns the from-origin seek from O(distance) into O(K): instead of re-walking
/// the path-dependent generator from the tape origin to a target, snapshot the
/// walk (a `GeneratedSource` clone) every `K` records or so and, to reach a
/// target, resume from the latest snapshot strictly before it and replay only
/// the residual.
///
/// Snapshots sit at parent boundaries only. The lead advances whole parents -
/// it resolves one, counts its records and moves on - so it never holds a
/// partly drained parent, never clones itself to try a parent and roll back,
/// and never resolves a parent twice. A snapshot is taken at the first
/// boundary after at least `k` records, so consecutive snapshots are at most
/// `k + MAX_PARENT_RECORDS - 1` records apart; each records its cumulative
/// record position so that claim can be checked rather than assumed.
///
/// The index is shared per realization (one symbol's clean tape) and extended
/// lazily and monotonically, so the O(span) walk to the frontier is paid once
/// across all of that realization's seeks, never per request.
///
/// The realization is preserved byte-for-byte: a snapshot is the exact walk
/// state at a boundary, so resuming it and replaying yields the same records a
/// from-origin run would (`checkpoint_resume_is_byte_identical` pins the
/// resume path directly).
/// Hard ceiling on the number of snapshots one `CheckpointIndex` retains. Once
/// a snapshot would push past this, `coarsen` halves the count and doubles the
/// spacing, so the index's memory is bounded by `MAX_CHECKPOINTS` generator
/// clones regardless of how long an accelerated session runs. 4096 keeps
/// coarsening rare (the first only after roughly `4096 * k` records) while
/// capping worst-case mutable-state memory. Immutable generator scalars, the
/// record scale and the session calendar are shared by the lead and every
/// snapshot.
pub(super) const MAX_CHECKPOINTS: usize = 4096;

/// One retained walk state at a parent boundary, and the cumulative number of
/// records the walk had emitted when it was taken.
struct Snapshot {
    source: GeneratedSource,
    position: u64,
}

pub struct CheckpointIndex {
    /// A generator advanced to the frontier, always at a parent boundary;
    /// cloned to extend the chain and to hand out positioned sources.
    lead: GeneratedSource,
    /// Records the lead has emitted since the origin.
    lead_position: u64,
    /// Snapshots in ascending `clock_ns`; `[0]` is the origin (pre-first-record).
    checkpoints: Vec<Snapshot>,
    /// Records the lead has advanced since the last snapshot was taken.
    since_snapshot: usize,
    /// Snapshot spacing in records: a snapshot is due at the first parent
    /// boundary at or past this many records since the last.
    k: usize,
    /// Runaway backstop: the lead starts a new parent only while a single
    /// `extend_toward` call has walked fewer than this many records, so one
    /// call walks at most `max_extend + MAX_PARENT_RECORDS - 1`. The venue
    /// refuses a `start` below `data_origin`, but nothing rejects an absurd
    /// `start` above the live frontier, and the generator never ends - so an
    /// uncapped extension would spin the walk indefinitely while holding the
    /// shared index mutex. A target past this bound leaves the frontier short;
    /// `try_source_before` reports that refusal instead of handing a caller a
    /// source which could then seek forever.
    max_extend: usize,
}

impl CheckpointIndex {
    #[must_use]
    pub fn fault(&self) -> Option<crate::TickFault> {
        crate::RiverSource::fault(&self.lead)
    }

    /// Retain the lead's current walk state and reset the snapshot cadence.
    fn snapshot(&mut self) {
        debug_assert!(self.lead.at_parent_boundary());
        self.checkpoints.push(Snapshot {
            source: self.lead.clone(),
            position: self.lead_position,
        });
        self.since_snapshot = 0;
        if self.checkpoints.len() > MAX_CHECKPOINTS {
            self.coarsen();
        }
    }

    /// Build an index over the realization `origin` heads. `origin` must be a
    /// fresh source at the tape origin (no records drawn yet); its
    /// pre-first-record state becomes checkpoint 0. `max_extend` bounds the
    /// per-call walk (see the field doc) - pass the caller's from-origin seek
    /// cap.
    #[must_use]
    pub fn new(origin: GeneratedSource, k: usize, max_extend: usize) -> Self {
        assert!(k > 0, "checkpoint spacing must be positive");
        assert!(max_extend > 0, "extension cap must be positive");
        assert!(
            origin.at_parent_boundary(),
            "the origin must sit at a parent boundary"
        );
        Self {
            checkpoints: vec![Snapshot {
                source: origin.clone(),
                position: 0,
            }],
            lead: origin,
            lead_position: 0,
            since_snapshot: 0,
            k,
            max_extend,
        }
    }

    /// Extend the snapshot chain until it covers `target`, advancing the lead
    /// one whole parent at a time and snapshotting at the first boundary past
    /// every `k` records. Monotonic: a later, further target only does the new
    /// delta, so the from-origin walk is paid once across all seeks.
    ///
    /// A parent is started only while the call has walked fewer than
    /// `max_extend` records, and the records it emits are all charged, so the
    /// returned count is the work actually done - at most one parent past the
    /// cap. A target beyond the cap leaves the lead short, which
    /// `try_source_before` refuses outright rather than papering over. A
    /// refusal from the walk produced no parent, so nothing is charged and no
    /// snapshot is taken; the fault stays latched on the lead.
    pub fn extend_toward(&mut self, target: u64) -> usize {
        self.extend_toward_within(target, self.max_extend)
    }

    /// `extend_toward` under a caller's own cap as well as the index's: a
    /// parent is started only while this call has walked fewer than
    /// `min(cap, max_extend)` records. A caller enforcing a cumulative ceiling
    /// across calls passes what remains of it, so the cumulative walk exceeds
    /// that ceiling by at most one parent rather than by one whole call.
    pub fn extend_toward_within(&mut self, target: u64, cap: usize) -> usize {
        let cap = cap.min(self.max_extend);
        let mut walked = 0usize;
        while self.lead.clock_ns() < target && walked < cap {
            let Ok(emitted) = self.lead.skip_parent() else {
                break;
            };
            walked += emitted;
            self.lead_position += emitted as u64;
            self.since_snapshot += emitted;
            if self.since_snapshot >= self.k {
                self.snapshot();
            }
        }
        walked
    }

    /// Halve the snapshot count once it exceeds `MAX_CHECKPOINTS` by dropping
    /// every other checkpoint and doubling the spacing `k`. This is what makes
    /// the index's memory a hard ceiling (`MAX_CHECKPOINTS` generator clones)
    /// over any session length, rather than a clone per `k` records growing
    /// without bound.
    ///
    /// It is correctness-preserving: every retained checkpoint is still the
    /// exact walk state at its boundary, so resuming from the coarser grid and
    /// replaying reproduces the identical tape - dropping an intermediate
    /// snapshot only lengthens the residual drain, it never changes which
    /// records are emitted. The origin (index 0) is always retained as the
    /// pre-first-record fallback. After a coarsening the gap between retained
    /// snapshots is bounded by the sum of the two gaps it merged, each at most
    /// the spacing then in force plus one parent's footprint, and the spacing
    /// grows only logarithmically in session length.
    fn coarsen(&mut self) {
        let before = self.checkpoints.len();
        let mut idx = 0usize;
        self.checkpoints.retain(|_| {
            let keep = idx.is_multiple_of(2);
            idx += 1;
            keep
        });
        if self.checkpoints.len() < before {
            self.k = self.k.saturating_mul(2);
        }
        // A single shift, not one per dropped snapshot: a drain of the range
        // removes exactly the oldest snapshots after the origin in one pass.
        let excess = self.checkpoints.len().saturating_sub(MAX_CHECKPOINTS);
        if excess > 0 {
            self.checkpoints.drain(1..=excess);
        }
    }

    /// A fresh generator positioned at the latest checkpoint whose `clock_ns`
    /// is strictly less than `target`, or the origin when none is. The
    /// boundary is strict, never a `<=` partition on `clock_ns` - see
    /// `try_source_before_target`, which carries the argument in full.
    ///
    /// The caller drains the result forward to the exact target via the
    /// normal seek; the returned source is an independent clone, so the shared
    /// index is untouched by that replay.
    pub fn try_source_before(&mut self, target: u64) -> Option<GeneratedSource> {
        self.try_source_before_target(target, 0)
            .map(|(source, _)| source)
    }

    /// The same positioning, but `back` snapshots earlier than the strict
    /// partition point, and reporting whether the result is the origin - the
    /// earliest snapshot there is, and the only floor the walk-back has.
    ///
    /// A positioned source only re-emits records the resume point had not yet
    /// consumed, so a reader recovering state the snapshot itself consumed - the
    /// last trade price at or before `target`, say - can find nothing to read
    /// even though the answer exists. That reader is not wrong to have asked,
    /// and must not be told "no such print": it walks back until it finds one or
    /// until it reaches the origin. Returning the origin flag is what lets the
    /// caller stop without guessing at the chain's length, and the origin is a
    /// complete answer rather than a fence to work around: it has consumed
    /// nothing, so replaying from it re-emits every record the river ever had.
    pub fn try_source_before_target(
        &mut self,
        target: u64,
        back: usize,
    ) -> Option<(GeneratedSource, bool)> {
        self.extend_toward(target);
        if self.lead.clock_ns() < target {
            return None;
        }
        // Strictly-before partition (`<`, not `<=`). A boundary snapshot's
        // `clock_ns` is the end of the span its last parent reserved, an upper
        // bound on every record it has consumed - on the discrete-book path
        // it can sit on a reserved but suppressed quote slot, which makes it
        // conservative, never wrong. Selecting `clock_ns < target` therefore
        // proves the resume point has consumed nothing at or after `target`.
        // A snapshot whose clock equals the target may have consumed a record
        // at exactly that instant, and resuming there and seeking would skip
        // it while a from-origin seek returns it; pollers legitimately pass an
        // emitted record's exact instant as a target, so `<=` would drop
        // records (`checkpoint_resume_at_exact_boundary_ts_returns_boundary_tick`
        // pins this). The origin has consumed nothing, so it is always a safe
        // resume point whatever its clock.
        let idx = self
            .checkpoints
            .iter()
            .rposition(|checkpoint| checkpoint.source.clock_ns() < target)
            .unwrap_or(0);
        let idx = idx.saturating_sub(back);
        Some((self.checkpoints[idx].source.clone(), idx == 0))
    }

    /// The same strictly-before positioning as `try_source_before`, on a
    /// reachable target, panicking rather than returning a short source that an
    /// unbounded downstream seek could walk forever.
    pub fn source_before(&mut self, target: u64) -> GeneratedSource {
        self.try_source_before(target)
            .expect("checkpoint extension cap left target unreachable")
    }

    /// Number of snapshots held (origin included). For tests and the measurement.
    #[must_use]
    pub fn checkpoint_count(&self) -> usize {
        self.checkpoints.len()
    }

    /// The cumulative record position of every retained snapshot, in order.
    /// For the gates on the spacing claim.
    #[must_use]
    pub fn checkpoint_positions(&self) -> Vec<u64> {
        self.checkpoints
            .iter()
            .map(|checkpoint| checkpoint.position)
            .collect()
    }

    /// The reserved instant of every retained snapshot, in order.
    #[must_use]
    pub fn checkpoint_clocks(&self) -> Vec<u64> {
        self.checkpoints
            .iter()
            .map(|checkpoint| checkpoint.source.clock_ns())
            .collect()
    }

    /// Timestamp reached by the shared lead after the latest bounded extension.
    #[must_use]
    pub fn frontier_ns(&self) -> u64 {
        self.lead.clock_ns()
    }
}
