// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! The armed-divergence queue: arming, the explicit flush, and the
//! first-applicable-entry consumption the submit path drives.

use mogwai_protocol::control::Divergence;

use crate::{Engine, MAX_ARMED_DIVERGENCES};

impl Engine {
    /// Arm a divergence to fire on the next matching trigger (control plane).
    ///
    /// Returns the entry SHED to make room, if the queue was already at
    /// `MAX_ARMED_DIVERGENCES`. The caller is expected to relay that upward:
    /// an arming ack that says only "accepted" while an older armed divergence
    /// was silently discarded is an ack that lies about what it did, and a
    /// scenario then spends its debugging budget on "why did my armed partial
    /// never fire" instead of "my queue overflowed". `None` means nothing was
    /// displaced (including for the server-owned variants this drops outright,
    /// which never enter the queue at all).
    pub fn arm(&mut self, d: Divergence) -> Option<Divergence> {
        match d {
            // Server-owned temporal/control divergences have no engine-side
            // trigger, so `take_armed` would never consume them. Dropping them
            // here keeps them from accumulating as dead entries in the armed
            // queue.
            // `CancelOpenOrderSilently` is immediate-action, not armed: the
            // server routes it to `cancel_open_order_silently` at post time.
            // Reaching `arm` with it would leak a dead queue entry, so it is
            // dropped alongside the server-owned temporal variants.
            // `FlowSurge` is named here for the same reason and one more: it is
            // the first arm that reaches into GENERATOR state (a sim-time window
            // on the tape source), so it has no engine-side trigger at all.
            // Listing it explicitly is what stops a future enum variant from
            // falling through into engine behaviour by accident.
            Divergence::DelayAcks { .. }
            | Divergence::CommandLatency { .. }
            | Divergence::GoDark { .. }
            | Divergence::StallData { .. }
            | Divergence::FlowSurge { .. }
            | Divergence::FeeSurcharge { .. }
            | Divergence::ClearDivergences
            | Divergence::CancelOpenOrderSilently { .. } => None,
            other => {
                // Bound the queue so control-plane arms cannot accumulate
                // without limit. At the cap, shed the OLDEST entry: a
                // never-triggered targeted `PartialFillNext` sits at the front
                // (its order may never arrive), so dropping the front sheds the
                // accumulated stale leftovers rather than the arm just
                // requested. See `MAX_ARMED_DIVERGENCES` and `clear_armed`.
                let shed = if self.armed.len() >= MAX_ARMED_DIVERGENCES {
                    let shed = self.armed.pop_front();
                    tracing::warn!(
                        cap = MAX_ARMED_DIVERGENCES,
                        ?shed,
                        "armed divergence queue at capacity; dropped the oldest entry"
                    );
                    shed
                } else {
                    None
                };
                self.armed.push_back(other);
                shed
            }
        }
    }

    /// Flush every engine-side armed divergence, draining the queue outright.
    ///
    /// The single-shot divergences (`PartialFillNext`, `RejectNextSubmit`,
    /// `DuplicateNextFill`, `DropNextAccountUpdate`) normally self-disarm on
    /// their own trigger, but a TARGETED `PartialFillNext` whose order never
    /// arrives has no trigger and would sit armed forever - a leftover from one
    /// scenario can otherwise ambush a later scenario that reuses the same order
    /// id. This is the explicit escape hatch a harness calls between scenarios
    /// to guarantee a clean slate.
    ///
    /// Deliberately distinct from the wire `control::Divergence::ClearDivergences`,
    /// which clears ONLY the server-owned temporal windows (`DelayAcks`,
    /// `GoDark`, `StallData`) and by documented contract leaves the engine-side
    /// single-shots alone. Widening that wire variant to also flush the engine
    /// queue would change its contract; this crate-local method flushes without
    /// touching the wire, so a future server that wants a full reset on
    /// `ClearDivergences` can call this alongside its atomics rather than
    /// overloading the variant.
    pub fn clear_armed(&mut self) {
        self.armed.clear();
    }

    /// Consume the first armed divergence that *applies* to the current event,
    /// leaving every non-matching entry in place and still armed.
    ///
    /// Consumption used to peek only `front()`, which head-of-line-blocked the
    /// whole queue: a `PartialFillNext` targeted at an order other than the one
    /// being processed sat at `front()` forever (its target may never arrive),
    /// silently disarming every engine-side divergence queued behind it
    /// (`DuplicateNextFill`, `DropNextAccountUpdate`, another `PartialFillNext`).
    /// Scanning for the first *applicable* entry instead lets a still-waiting
    /// targeted `PartialFillNext` stay armed until its order shows up without
    /// stalling the divergences behind it. Order is otherwise preserved: the
    /// first applicable entry wins, so two divergences that both apply to the
    /// same event still fire in arm order.
    pub(crate) fn take_armed(
        &mut self,
        applies: impl Fn(&Divergence) -> bool,
    ) -> Option<Divergence> {
        let pos = self.armed.iter().position(applies)?;
        self.armed.remove(pos)
    }
}
