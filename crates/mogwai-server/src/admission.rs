// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Per-connection admission control for the outbound execution path.
//!
//! A session's outbound machinery is two lanes with explicit accounting, not
//! one bounded channel both the engine and the socket read loop push into:
//!
//! - the HELD lane carries engine output through the `DelayAcks` shift, is
//!   unbounded by channel capacity and bounded by a BYTE budget, because
//!   `AccountState` and the venue-truth snapshots have no per-frame size
//!   ceiling a frame COUNT could stand in for;
//! - the PRIORITY lane carries admission truth (`AdmissionRejected`,
//!   `ProtocolError`), is exempt from `DelayAcks`, is delivered ahead of held
//!   traffic, and is bounded by a FRAME count - legitimate here and nowhere
//!   else, because every frame on it is under
//!   `mogwai_protocol::ADMISSION_FRAME_MAX_BYTES`.
//!
//! Every producer that runs on the socket read loop reserves worst-case
//! capacity BEFORE the engine is allowed to mutate, refuses visibly when the
//! reservation fails, and never awaits a full channel. Both lanes are
//! unbounded-by-channel precisely so a read-loop `send` cannot block; the
//! bounds are the budgets here.

use std::sync::Arc;
use std::time::Instant;

use mogwai_protocol::{
    ClientMessage, ServerMessage,
    sizing::{BOUNDARY_REFUSAL_BYTES, BookShape, worst_case_output_bytes},
    truncate_reason,
};
use tokio::sync::{Semaphore, mpsc};

/// Per-connection byte ceiling on execution output that has been produced but
/// not yet written to the socket. `control::MAX_DIVERGENCE_MS` permits a
/// one-hour `DelayAcks`, so an unbounded held lane turns one stalled connection
/// into process-wide memory exhaustion.
pub(crate) const EXEC_HELD_BUDGET_BYTES: usize = 8 * 1024 * 1024;

/// Per-connection ceiling on QUEUED priority frames. 64 x
/// `ADMISSION_FRAME_MAX_BYTES` = 512 KiB, a real bound.
pub(crate) const ADMISSION_LANE_FRAMES: usize = 64;

/// Outstanding PROMISES of a future priority frame - one per live replay, held
/// for the replay's whole life. Accounted separately from queue depth: drawn
/// from the same 64-slot pool, 64 healthy replays would leave zero capacity for
/// any actual refusal and the 65th subscribe would close a connection whose
/// priority lane is completely empty. Sized at `MAX_SUBSCRIBE_SYMBOLS` because
/// that is exactly how many replays one connection can have.
pub(crate) const ADMISSION_PROMISE_TICKETS: usize = mogwai_protocol::MAX_SUBSCRIBE_SYMBOLS;

/// WS 1013 "Try Again Later": the venue is refusing further work on this
/// connection because its admission path is saturated. Deliberately not a
/// silent stall - a close is a delivery failure, which the honest-content
/// invariant permits, and it is the least ambiguous signal available.
pub(crate) const CLOSE_ADMISSION_OVERLOAD: u16 = 1013;

/// WS 1011 "Internal Error": the venue itself failed, in a way no client
/// behavior caused and none can plan for. Reserved for exactly one condition
/// today - a subscriber's tape ring turned over, so the venue no longer holds
/// market data it already promised to deliver in ascending order.
///
/// This is NOT a divergence. Every modeled pathology in this venue is armed
/// deliberately and the honest floor is that nothing perturbs the stream unless
/// it was asked for (see `reference/havoc.md`). A lagged ring is an unarmed
/// hole, which makes it a venue fault rather than a scenario: the exchange
/// crashed. It ends the connection instead of continuing with a gap, because a
/// forward-validation run that keeps streaming past silently-missing data
/// produces a result that looks fine and is not.
pub(crate) const CLOSE_VENUE_FAULT: u16 = 1011;

/// How long the overload close may take to reach a peer before the connection
/// is torn down anyway. WALL time, not sim time: this is a transport deadline.
/// The writer may already be parked in `sink.send()` against a full TCP window,
/// in which case the close is never written; a reasoned close is best-effort by
/// nature, but releasing the connection's resources is not.
pub(crate) const CLOSE_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// The three per-connection budget sizes, together, so a session can be built
/// with something other than the defaults.
///
/// They exist as a value rather than as the three constants read directly
/// because the socket-level behavior they govern - a refused reservation, a
/// saturated priority lane, an overload close against a peer that stopped
/// reading - is only reachable end to end when the budgets can be made small.
/// Driving `EXEC_HELD_BUDGET_BYTES` (8 MiB) or `ADMISSION_LANE_FRAMES` (64
/// queued refusals against a live socket) at their production sizes is not a
/// test, it is a load generator: it would take megabytes of engine output and a
/// TCP window's worth of timing luck to reach the very branch under test. With
/// injectable sizes the same branches are reached in milliseconds and
/// deterministically, so the invariants get gates instead of arguments.
///
/// `Default` IS production: every non-test construction path goes through
/// `Config`, whose own defaults are these constants, so the shipped venue
/// behaves exactly as the constants say.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AdmissionLimits {
    pub(crate) held_budget_bytes: usize,
    pub(crate) lane_frames: usize,
    pub(crate) promise_tickets: usize,
}

impl Default for AdmissionLimits {
    fn default() -> Self {
        Self {
            held_budget_bytes: EXEC_HELD_BUDGET_BYTES,
            lane_frames: ADMISSION_LANE_FRAMES,
            promise_tickets: ADMISSION_PROMISE_TICKETS,
        }
    }
}

/// A lane's receiver is gone, i.e. the connection is already tearing down. Not
/// a capacity condition - the producer treats it as terminal rather than
/// pretending the path is infallible.
#[derive(Debug)]
pub(crate) struct LaneClosed;

impl std::fmt::Debug for CloseSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CloseSpec")
            .field("code", &self.code)
            .field("reason", &self.reason)
            .finish()
    }
}

/// Byte budget for produced-but-unwritten execution output.
///
/// A `Semaphore` whose permits ARE bytes. Permits are forgotten on reservation
/// and returned explicitly, rather than held as RAII guards on the producer's
/// stack, because a frame's charge outlives the producer: it belongs to the
/// frame and travels with it to the writer.
#[derive(Clone)]
pub(crate) struct ByteBudget {
    permits: Arc<Semaphore>,
}

impl ByteBudget {
    pub(crate) fn new(bytes: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(bytes)),
        }
    }

    /// Reserve `bytes`, or fail. Never blocks - this runs on the read loop.
    /// A request that does not fit `u32` (tokio's permit count) returns `None`
    /// rather than truncating: a truncating cast would grant a reservation
    /// SMALLER than the output it is supposed to dominate, which is the one
    /// failure mode this type exists to rule out.
    pub(crate) fn try_reserve(&self, bytes: usize) -> Option<Reservation> {
        let n = u32::try_from(bytes).ok()?;
        Arc::clone(&self.permits)
            .try_acquire_many_owned(n)
            .ok()?
            .forget();
        Some(Reservation {
            budget: self.clone(),
            reserved: bytes,
        })
    }

    pub(crate) fn release(&self, bytes: usize) {
        self.permits.add_permits(bytes);
    }

    /// Bytes still available. Test/diagnostic only.
    #[cfg(test)]
    pub(crate) fn available(&self) -> usize {
        self.permits.available_permits()
    }
}

/// A granted reservation. `split` carves a per-frame charge out of it as each
/// frame is serialized; dropping it releases everything still unspent, so a
/// producer that returns early or panics cannot leak budget.
pub(crate) struct Reservation {
    budget: ByteBudget,
    reserved: usize,
}

impl Reservation {
    /// Split a charge off this reservation for one produced frame, keeping the
    /// rest reserved for the frames still to be serialized. Whatever is left
    /// when the reservation drops goes back to the budget, which is how the
    /// unused remainder of a worst-case reservation is returned the moment the
    /// real batch is known.
    ///
    /// A batch asking for more than was reserved would mean the sizing model
    /// failed to dominate real engine output - the bound void, not merely
    /// tight. It is a debug assertion; in release the frame is charged what is
    /// left (never negative, never a double release) plus a loud log.
    fn split(&mut self, bytes: usize) -> HeldCharge {
        debug_assert!(
            bytes <= self.reserved,
            "produced {bytes} bytes against {} remaining reserved: the sizing \
             model does not dominate engine output",
            self.reserved
        );
        if bytes > self.reserved {
            tracing::error!(
                wanted = bytes,
                remaining = self.reserved,
                "produced output exceeded its admission reservation"
            );
        }
        let bytes = bytes.min(self.reserved);
        self.reserved -= bytes;
        HeldCharge {
            budget: self.budget.clone(),
            bytes,
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if self.reserved != 0 {
            self.budget.release(self.reserved);
        }
    }
}

/// Bytes charged against a connection's held budget, released when the frame
/// carrying it is dropped - which the writer does once the frame has been
/// written to the socket OR dropped by an armed havoc window, and which happens
/// for free when the writer task dies with frames still in hand. Release on
/// drop rather than an explicit call is what makes those exits correct without
/// a single call site remembering to.
pub(crate) struct HeldCharge {
    budget: ByteBudget,
    bytes: usize,
}

impl Drop for HeldCharge {
    fn drop(&mut self) {
        self.budget.release(self.bytes);
    }
}

/// Frame budget, same discipline in units of frames. Used twice per connection:
/// once for priority QUEUE depth, once for outstanding PROMISES.
#[derive(Clone)]
pub(crate) struct FrameBudget {
    permits: Arc<Semaphore>,
}

impl FrameBudget {
    pub(crate) fn new(frames: usize) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(frames)),
        }
    }
    pub(crate) fn try_ticket(&self) -> Option<Ticket> {
        Arc::clone(&self.permits)
            .try_acquire_owned()
            .ok()
            .map(|permit| Ticket { _permit: permit })
    }
}

/// One reserved frame slot. Single-use BY TYPE: spending it consumes it, which
/// is what pins the "a replay can emit at most one diagnostic in its life"
/// claim that a promise's correctness rests on. A ticket may outlive its
/// producer - the subscribe path reserves one for a replay's possible dead-seek
/// diagnostic and hands it to the fanout task, which releases it on exit if
/// it never fires - so the slot is released by dropping, wherever that happens.
pub(crate) struct Ticket {
    /// Never read: the permit's whole job is to be RELEASED when this value is
    /// dropped, wherever that turns out to be.
    _permit: tokio::sync::OwnedSemaphorePermit,
}

/// A frame already rendered to its wire bytes, plus the facts the writer needs.
/// Serializing at the PRODUCER (the tape thread, the admission path, the
/// order path) is what makes the byte budget exact rather than estimated: the
/// charge is the true length of what goes on the socket. It also moves JSON
/// cost off the single writer task.
pub(crate) struct OutboundFrame {
    pub(crate) payload: Arc<str>,
    pub(crate) is_market_data: bool,
    /// Held-lane byte charge, released when this frame is dropped. `None` for
    /// frames that were never charged: market data and heartbeats. Never read -
    /// it travels with the frame so the budget is released exactly once, at
    /// whichever of the writer's exits this frame reaches.
    #[allow(dead_code, reason = "released by Drop, never read")]
    pub(crate) charge: Option<HeldCharge>,
    /// Priority-lane queue slot, released when this frame is dropped. Held for
    /// the frame's whole queued life rather than released at enqueue, which
    /// would make `ADMISSION_LANE_FRAMES` bound nothing at all. Never read, for
    /// the same reason as `charge`.
    #[allow(dead_code, reason = "released by Drop, never read")]
    pub(crate) slot: Option<Ticket>,
}

/// What the writer accepts. `Close` exists so the overload path can state a
/// reason on the socket: the writer owns the sink, so no other task can send a
/// close frame.
pub(crate) enum Outbound {
    Frame(OutboundFrame),
    Close(CloseSpec),
}

pub(crate) struct CloseSpec {
    pub(crate) code: u16,
    pub(crate) reason: String,
}

impl CloseSpec {
    pub(crate) fn overload(reason: impl Into<String>) -> Self {
        Self {
            code: CLOSE_ADMISSION_OVERLOAD,
            reason: truncate_reason(reason.into()),
        }
    }

    /// The venue failed on its own account; see `CLOSE_VENUE_FAULT`.
    pub(crate) fn venue_fault(reason: impl Into<String>) -> Self {
        Self {
            code: CLOSE_VENUE_FAULT,
            reason: truncate_reason(reason.into()),
        }
    }
}

/// An engine-produced frame awaiting its `DelayAcks` deadline, anchored at the
/// instant the batch arrived so the pump stays a pure time shift.
pub(crate) struct HeldFrame {
    pub(crate) arrived: Instant,
    pub(crate) frame: OutboundFrame,
}

/// The session's outbound execution machinery: one value threaded through the
/// read loop, the replay spawns and the pump, replacing the bare
/// `mpsc::Sender<(Instant, ServerMessage)>` that both stalled the reader and
/// hid the two lanes behind one channel. Every method is non-blocking.
#[derive(Clone)]
pub(crate) struct ExecLanes {
    held_tx: mpsc::UnboundedSender<HeldFrame>,
    held_budget: ByteBudget,
    prio_tx: mpsc::UnboundedSender<Outbound>,
    prio_budget: FrameBudget,
    promise_budget: FrameBudget,
}

/// The receiving halves of a detached `ExecLanes`, kept alive by its owner.
/// Sending into a lane whose receiver was never created returns `Err` and would
/// strand forgotten permits, so the receivers are handed back rather than
/// dropped on the floor.
pub(crate) struct LaneReceivers {
    #[allow(dead_code, reason = "kept alive so sends succeed; read only by tests")]
    pub(crate) held_rx: mpsc::UnboundedReceiver<HeldFrame>,
    #[allow(dead_code, reason = "kept alive so sends succeed; read only by tests")]
    pub(crate) prio_rx: mpsc::UnboundedReceiver<Outbound>,
}

impl ExecLanes {
    pub(crate) fn new(
        held_tx: mpsc::UnboundedSender<HeldFrame>,
        prio_tx: mpsc::UnboundedSender<Outbound>,
        limits: AdmissionLimits,
    ) -> Self {
        Self {
            held_tx,
            prio_tx,
            held_budget: ByteBudget::new(limits.held_budget_bytes),
            prio_budget: FrameBudget::new(limits.lane_frames),
            promise_budget: FrameBudget::new(limits.promise_tickets),
        }
    }

    /// Lanes with no socket behind them, for the HTTP order path (constructed
    /// FRESH PER REQUEST and dropped with it) and for tests. Budgets are the
    /// ordinary sizes, not a saturating sentinel: a sentinel would have to
    /// survive the `u32` conversion in `try_reserve` and would make this the
    /// one path where the reservation arithmetic is untested, while a
    /// per-request budget can never refuse a single command anyway because it
    /// starts empty of charges.
    pub(crate) fn detached() -> (Self, LaneReceivers) {
        Self::detached_with(AdmissionLimits::default())
    }

    /// `detached`, at budgets of the caller's choosing. Tests only in practice:
    /// it is how a unit test reaches a refusal without producing 8 MiB.
    pub(crate) fn detached_with(limits: AdmissionLimits) -> (Self, LaneReceivers) {
        let (held_tx, held_rx) = mpsc::unbounded_channel();
        let (prio_tx, prio_rx) = mpsc::unbounded_channel();
        (
            Self::new(held_tx, prio_tx, limits),
            LaneReceivers { held_rx, prio_rx },
        )
    }

    /// Reserve worst-case output for `cmd` against `shape`. `None` means the
    /// caller must refuse WITHOUT letting the engine see the command.
    pub(crate) fn reserve(&self, cmd: &ClientMessage, shape: &BookShape) -> Option<Reservation> {
        self.held_budget
            .try_reserve(worst_case_output_bytes(cmd, shape))
    }

    /// Reserve capacity for a protocol-boundary refusal, whose worst case is a
    /// constant because it is one order-shaped frame and no `AccountState`.
    /// Used by the pre-engine refusal paths, which have no `BookShape` in hand.
    pub(crate) fn try_reserve_boundary(&self) -> Option<Reservation> {
        self.held_budget.try_reserve(BOUNDARY_REFUSAL_BYTES)
    }

    /// Reserve one priority-lane QUEUE slot. `None` is the overload condition.
    pub(crate) fn reserve_admission(&self) -> Option<Ticket> {
        self.prio_budget.try_ticket()
    }

    /// Reserve a PROMISE of future priority capacity, held for a replay's whole
    /// life. Drawn from its own pool: a promise is not a queued frame, it is
    /// capacity that is only ever spent by converting the ticket into a queued
    /// frame, and that conversion re-checks the queue budget.
    pub(crate) fn reserve_promise(&self) -> Option<Ticket> {
        self.promise_budget.try_ticket()
    }

    /// Emit an admission frame against a queue slot. Never blocks.
    ///
    /// The slot travels with the frame and is released when the writer drops
    /// it, so the budget is what it claims: a ceiling on QUEUED priority
    /// frames. The reason is truncated here rather than at each of the dozen
    /// call sites, which is what actually makes `ADMISSION_FRAME_MAX_BYTES`
    /// hold for every one of them.
    pub(crate) fn emit_admission(
        &self,
        slot: Ticket,
        msg: ServerMessage,
    ) -> Result<(), LaneClosed> {
        let msg = match msg {
            ServerMessage::ProtocolError { reason, ts_event } => ServerMessage::ProtocolError {
                reason: truncate_reason(reason),
                ts_event,
            },
            ServerMessage::AdmissionRejected {
                subject,
                reason,
                ts_event,
            } => ServerMessage::AdmissionRejected {
                subject,
                reason: truncate_reason(reason),
                ts_event,
            },
            // No free-text `reason` field, BY DESIGN: every outcome is a typed
            // `SubscriptionIssue` and the entry list is capped at
            // `MAX_SUBSCRIPTION_ISSUES_LISTED`, which is what keeps the frame
            // under `ADMISSION_FRAME_MAX_BYTES` without a truncation step.
            // Named explicitly rather than left to the catch-all so the next
            // reason-carrying variant is not silently emitted untruncated.
            issues @ ServerMessage::SubscriptionIssues { .. } => issues,
            other => other,
        };
        // Unreachable by construction (every field is a String, a number or a
        // plain enum), and log-and-drop only for that reason - see the
        // honest-content invariant. If it ever becomes reachable it is a
        // connection-fatal error, not a skip.
        let payload = match serde_json::to_string(&msg) {
            Ok(payload) => payload,
            Err(e) => {
                tracing::error!(%e, "could not serialize an admission frame");
                return Ok(());
            }
        };
        self.prio_tx
            .send(Outbound::Frame(OutboundFrame {
                payload: Arc::from(payload),
                is_market_data: false,
                charge: None,
                slot: Some(slot),
            }))
            .map_err(|_| LaneClosed)
    }

    /// Queue the reasoned close. It needs no lane slot: it is sent after the
    /// read loop has already stopped taking new work, and the writer stops on
    /// it.
    pub(crate) fn send_close(&self, close: CloseSpec) -> Result<(), LaneClosed> {
        self.prio_tx
            .send(Outbound::Close(close))
            .map_err(|_| LaneClosed)
    }

    /// Hand the engine's real batch to the held lane under `reservation`,
    /// charging actual serialized bytes per frame and releasing the remainder.
    /// Infallible on capacity by construction - the reservation dominates the
    /// batch - and fallible only on a lane whose receiver is already gone.
    pub(crate) fn submit_produced(
        &self,
        mut reservation: Reservation,
        arrived: Instant,
        events: Vec<ServerMessage>,
    ) -> Result<(), LaneClosed> {
        for event in events {
            // Same construction argument as `emit_admission`: unreachable, so
            // logged and skipped rather than given a fallible signature that
            // every call site would have to invent a meaning for.
            let payload = match serde_json::to_string(&event) {
                Ok(payload) => payload,
                Err(e) => {
                    tracing::error!(%e, "could not serialize an execution frame");
                    continue;
                }
            };
            let charge = reservation.split(payload.len());
            self.held_tx
                .send(HeldFrame {
                    arrived,
                    frame: OutboundFrame {
                        payload: Arc::from(payload),
                        is_market_data: false,
                        charge: Some(charge),
                        slot: None,
                    },
                })
                .map_err(|_| LaneClosed)?;
        }
        // Whatever the batch did not use goes back when `reservation` drops.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::{AdmissionSubject, ServerMessage};

    fn refusal() -> ServerMessage {
        ServerMessage::AdmissionRejected {
            subject: AdmissionSubject::Frame,
            reason: "bad frame".into(),
            ts_event: 1,
        }
    }

    /// The budget arithmetic under `held_budget_is_returned_on_write_and_on_disconnect`
    /// (the socket-level gate in `main.rs`), at the level where the two release
    /// paths are separable: a frame's charge coming back when the writer drops
    /// it, and a reservation's remainder coming back when its holder goes away.
    #[test]
    fn held_budget_returns_on_charge_release_and_on_drop() {
        let budget = ByteBudget::new(10);
        let mut reservation = budget.try_reserve(10).expect("full budget reserves");
        let charge = reservation.split(6);
        drop(reservation); // the unused remainder goes back immediately
        assert!(budget.try_reserve(5).is_none(), "charged bytes remain held");
        assert!(budget.try_reserve(4).is_some(), "the remainder came back");
        drop(charge);
        assert!(
            budget.try_reserve(10).is_some(),
            "writer release restores full budget"
        );

        let reservation = budget
            .try_reserve(10)
            .expect("reserve for disconnected session");
        drop(reservation);
        assert!(
            budget.try_reserve(10).is_some(),
            "teardown releases an uncharged reservation"
        );
    }

    #[test]
    fn submit_produced_charges_every_frame_and_returns_the_remainder() {
        let (lanes, mut rx) = ExecLanes::detached();
        let events = vec![
            ServerMessage::Heartbeat { ts_event: 1 },
            ServerMessage::Heartbeat { ts_event: 2 },
        ];
        let expected: usize = events
            .iter()
            .map(|e| serde_json::to_string(e).expect("serializes").len())
            .sum();
        let reservation = lanes
            .held_budget
            .try_reserve(EXEC_HELD_BUDGET_BYTES)
            .expect("whole budget");
        lanes
            .submit_produced(reservation, Instant::now(), events)
            .expect("held lane accepts the batch");
        assert_eq!(
            lanes.held_budget.available(),
            EXEC_HELD_BUDGET_BYTES - expected,
            "exactly the produced bytes stay charged"
        );
        let mut frames = Vec::new();
        while let Ok(frame) = rx.held_rx.try_recv() {
            frames.push(frame);
        }
        assert_eq!(frames.len(), 2);
        drop(frames);
        assert_eq!(
            lanes.held_budget.available(),
            EXEC_HELD_BUDGET_BYTES,
            "dropping the written frames returns their charge"
        );
    }

    #[test]
    fn a_queued_admission_frame_holds_its_slot_until_it_is_written() {
        let (lanes, mut rx) = ExecLanes::detached();
        let mut queued = Vec::new();
        for _ in 0..ADMISSION_LANE_FRAMES {
            let ticket = lanes.reserve_admission().expect("priority slot");
            lanes.emit_admission(ticket, refusal()).expect("queues");
        }
        assert!(
            lanes.reserve_admission().is_none(),
            "a queued frame keeps its slot: the lane is full"
        );
        while let Ok(frame) = rx.prio_rx.try_recv() {
            queued.push(frame);
        }
        assert_eq!(queued.len(), ADMISSION_LANE_FRAMES);
        assert!(
            lanes.reserve_admission().is_none(),
            "dequeued-but-unwritten frames still hold their slots"
        );
        drop(queued);
        assert!(
            lanes.reserve_admission().is_some(),
            "writing (dropping) the frames returns their slots"
        );
    }

    /// The lane arithmetic behind `admission_lane_overload_closes_with_a_reason`
    /// (the socket-level gate in `main.rs`): a full queue refuses a ticket, and
    /// the close that answers it needs no ticket of its own.
    #[test]
    fn a_full_lane_refuses_a_ticket_and_still_takes_the_close() {
        let (lanes, mut rx) = ExecLanes::detached();
        let mut held = Vec::new();
        for _ in 0..ADMISSION_LANE_FRAMES {
            held.push(lanes.reserve_admission().expect("priority slot"));
        }
        assert!(
            lanes.reserve_admission().is_none(),
            "queue capacity is bounded"
        );
        // A full lane is the overload condition; the close itself needs no slot.
        lanes
            .send_close(CloseSpec::overload("execution admission lane saturated"))
            .expect("the close jumps the queue");
        let Ok(Outbound::Close(close)) = rx.prio_rx.try_recv() else {
            panic!("expected a reasoned close on the priority lane");
        };
        assert_eq!(close.code, CLOSE_ADMISSION_OVERLOAD);
        assert!(!close.reason.is_empty());
    }

    #[test]
    fn many_live_replays_do_not_exhaust_the_priority_queue() {
        let (lanes, _rx) = ExecLanes::detached();
        let promises: Vec<_> = (0..=ADMISSION_LANE_FRAMES)
            .map(|_| lanes.reserve_promise().expect("live replay promise"))
            .collect();
        assert_eq!(promises.len(), ADMISSION_LANE_FRAMES + 1);
        assert!(
            lanes.reserve_admission().is_some(),
            "promises do not consume queue slots"
        );
    }

    #[test]
    fn the_promise_pool_covers_a_full_subscribe() {
        let (lanes, _rx) = ExecLanes::detached();
        let promises: Vec<_> = (0..mogwai_protocol::MAX_SUBSCRIBE_SYMBOLS)
            .map(|_| {
                lanes
                    .reserve_promise()
                    .expect("one promise per live replay")
            })
            .collect();
        assert!(
            lanes.reserve_promise().is_none(),
            "the promise pool is bounded"
        );
        drop(promises);
        assert!(
            lanes.reserve_promise().is_some(),
            "an ending replay returns its promise"
        );
    }

    #[test]
    fn admission_refusal_is_not_held_by_delay_acks() {
        let (lanes, mut rx) = ExecLanes::detached();
        let ticket = lanes.reserve_admission().expect("priority capacity");
        lanes
            .emit_admission(ticket, refusal())
            .expect("admission bypasses the held lane");
        assert!(rx.prio_rx.try_recv().is_ok());
        assert!(
            rx.held_rx.try_recv().is_err(),
            "admission never enters the delayed lane"
        );
    }

    #[test]
    fn admission_reasons_are_truncated_at_the_lane() {
        let (lanes, mut rx) = ExecLanes::detached();
        let ticket = lanes.reserve_admission().expect("priority capacity");
        lanes
            .emit_admission(
                ticket,
                ServerMessage::ProtocolError {
                    reason: "x".repeat(1024 * 1024),
                    ts_event: 1,
                },
            )
            .expect("queues");
        let Ok(Outbound::Frame(frame)) = rx.prio_rx.try_recv() else {
            panic!("expected a priority frame");
        };
        assert!(
            frame.payload.len() <= mogwai_protocol::ADMISSION_FRAME_MAX_BYTES,
            "the lane truncates, so the frame ceiling holds at every call site"
        );
    }

    #[test]
    fn reservation_failure_leaves_nothing_charged() {
        let budget = ByteBudget::new(1);
        assert!(budget.try_reserve(EXEC_HELD_BUDGET_BYTES).is_none());
        assert!(
            budget.try_reserve(usize::MAX).is_none(),
            "a request that cannot fit u32 is refused, never truncated"
        );
        assert_eq!(budget.available(), 1, "a failed reservation takes nothing");
    }
}
