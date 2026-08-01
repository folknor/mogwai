// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Out-of-band control plane: arm deterministic divergences for tests.
//!
//! This is the reason mogwai exists as an external process - it can emit ugly,
//! realistic event streams an in-process matching engine never would, to drive
//! broadarrow's `classify` → brake/quarantine/restart layer.

use crate::{ClientOrderId, Decimal, Deserialize, Serialize};

/// Upper bound on any single divergence's `ms` window, enforced by
/// `validate_divergence`.
///
/// One hour is far longer than any test blackout, data-stall, or ack-delay
/// scenario needs, and `3_600_000 * 1_000_000` ns is well below `u64::MAX`,
/// so validated temporal windows cannot saturate writer deadlines.
pub const MAX_DIVERGENCE_MS: u64 = 3_600_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Divergence {
    /// Fill the next matching order only `fraction` of the way, leaving the rest open.
    PartialFillNext {
        client_order_id: ClientOrderId,
        fraction: Decimal,
    },
    /// Reject the next submitted order with `reason`.
    RejectNextSubmit { reason: String },
    /// Delay every outbound execution event by `ms`, bounded by
    /// `MAX_DIVERGENCE_MS`. Arm with `ms: 0` to clear, or post
    /// `ClearDivergences`.
    DelayAcks { ms: u64 },
    /// Per-command venue latency: how long the venue takes to ACT on each order
    /// command, and how long it then takes to ACK what it did.
    ///
    /// Every field is milliseconds on the sim axis, bounded by
    /// `MAX_DIVERGENCE_MS`, and ADDS to any armed `DelayAcks` rather than
    /// replacing it (the same composition rule `BASELINE_LATENCY` states for the
    /// adapter's inbound latency) - though that addition happens in the WS pump,
    /// which is the only place `DelayAcks` applies at all. An arm REPLACES all
    /// six values; an omitted field is zero.
    CommandLatency {
        #[serde(default)]
        submit_act_ms: u64,
        #[serde(default)]
        modify_act_ms: u64,
        #[serde(default)]
        cancel_act_ms: u64,
        #[serde(default)]
        submit_ack_ms: u64,
        #[serde(default)]
        modify_ack_ms: u64,
        #[serde(default)]
        cancel_ack_ms: u64,
    },
    /// Emit the next fill event twice.
    DuplicateNextFill,
    /// Swallow the next fill-driven account-state update (induce account drift).
    DropNextAccountUpdate,
    /// Stop sending anything for `ms` (simulate a venue blackout), bounded
    /// by `MAX_DIVERGENCE_MS`. Frames produced during the window are
    /// dropped, not buffered. Post `ClearDivergences` to lift the window
    /// early.
    GoDark { ms: u64 },
    /// Suppress only market-data frames (`Trade` / `Quote`) for `ms`,
    /// leaving every execution frame alive. Bounded by
    /// `MAX_DIVERGENCE_MS`. Frames produced during the window are dropped,
    /// not buffered. Post `ClearDivergences` to lift the window early.
    ///
    /// Unlike `GoDark`, this keeps the socket healthy while only channel
    /// data is withheld, especially when paired with the server
    /// `Heartbeat`.
    StallData { ms: u64 },
    /// Clear the server-owned temporal windows: cancel any armed
    /// `DelayAcks`, any armed `GoDark`, any armed `StallData`, and every
    /// `CommandLatency` field.
    ///
    /// This does not flush engine-side single-shot divergences
    /// (`PartialFillNext`, `RejectNextSubmit`, `DuplicateNextFill`,
    /// `DropNextAccountUpdate`), which self-disarm on their own trigger.
    ///
    /// Nor does it lift a `CommandLatency` ACT delay the venue has already begun
    /// serving: that command sleeps out its full window and then mutates. A
    /// queued ACK hold IS lifted, because the writer reads that window per event
    /// at dequeue. Clearing governs commands the venue has not started acting on
    /// yet; it is not a time machine.
    ClearDivergences,
    /// Cancel a RESTING order server-side, immediately, emitting NO lifecycle
    /// event - the out-of-band cancel with a lost `OrderCanceled` that the
    /// consumer's reconciliation poll exists to catch. Unlike the armed
    /// single-shot divergences this is not queued for a trigger: it acts on
    /// the book the moment it is posted (there is no client action to key
    /// off), frees the order's reservation, and leaves the client believing
    /// the order still rests until it reconciles - a `QueryOrders` reply
    /// truthfully reports the order `Canceled` from then on. Posting it for
    /// an id that is not currently resting is refused with a 4xx, so a
    /// scenario cannot silently arm a no-op.
    CancelOpenOrderSilently { client_order_id: ClientOrderId },
}
