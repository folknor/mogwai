//! The venue-agnostic exchange core: open orders, accounts, and the divergence
//! injection layer. Protocol gateways (native JSON-over-WS, or a future Binance
//! facade) drive this engine and serialize whatever it emits.
//!
//! The engine is intentionally synchronous and side-effect free: `process` takes
//! a [`ClientMessage`] and returns the [`ServerMessage`]s to send. The server
//! owns sockets, timers and the clock; the engine owns matching and state. This
//! keeps the divergence behaviour deterministic and unit-testable.

use std::collections::VecDeque;

use mogwai_protocol::{
    ClientMessage, ClientOrderId, OrderFilled, ServerMessage, SubmitOrder, VenueOrderId,
    control::Divergence,
};
use rust_decimal::Decimal;

/// A resting order tracked by the venue.
#[derive(Debug, Clone)]
pub struct OpenOrder {
    pub venue_order_id: VenueOrderId,
    pub submit: SubmitOrder,
    pub leaves_qty: Decimal,
}

#[derive(Debug, Default)]
pub struct Engine {
    open: Vec<OpenOrder>,
    /// Armed divergences, consumed as their trigger fires.
    armed: VecDeque<Divergence>,
    seq: u64,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    /// Arm a divergence to fire on the next matching trigger (control plane).
    pub fn arm(&mut self, d: Divergence) {
        self.armed.push_back(d);
    }

    /// Monotonic id source; the server stamps real timestamps.
    fn next_id(&mut self, prefix: &str) -> String {
        self.seq += 1;
        format!("{prefix}-{}", self.seq)
    }

    /// Process one client message, emitting the resulting execution events.
    ///
    /// `ts` is supplied by the caller (the server's clock) so the engine stays
    /// free of wall-clock access and remains deterministic in tests.
    pub fn process(&mut self, msg: ClientMessage, ts: u64) -> Vec<ServerMessage> {
        match msg {
            ClientMessage::SubmitOrder(order) => self.on_submit(order, ts),
            ClientMessage::CancelOrder { client_order_id } => self.on_cancel(client_order_id, ts),
            // Subscriptions are intercepted by the server for replay control.
            // Modifies are not wired yet. This keeps the match exhaustive.
            ClientMessage::Subscribe { .. }
            | ClientMessage::Unsubscribe { .. }
            | ClientMessage::ModifyOrder { .. } => Vec::new(),
        }
    }

    fn on_submit(&mut self, order: SubmitOrder, ts: u64) -> Vec<ServerMessage> {
        // Divergence: reject the next submit outright.
        if matches!(
            self.armed.front(),
            Some(Divergence::RejectNextSubmit { .. })
        ) {
            let Some(Divergence::RejectNextSubmit { reason }) = self.armed.pop_front() else {
                unreachable!()
            };
            return vec![ServerMessage::OrderRejected {
                client_order_id: order.client_order_id,
                reason,
                ts_event: ts,
            }];
        }

        let venue_order_id = self.next_id("V");
        let mut out = vec![ServerMessage::OrderAccepted {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            ts_event: ts,
        }];

        // Divergence: partial-fill the next order, leaving the remainder resting.
        let fill_fraction = match self.armed.front() {
            Some(Divergence::PartialFillNext {
                client_order_id,
                fraction,
            }) if *client_order_id == order.client_order_id => {
                let f = *fraction;
                self.armed.pop_front();
                f
            }
            _ => Decimal::ONE,
        };

        let last_qty = order.quantity * fill_fraction;
        let leaves_qty = order.quantity - last_qty;
        let last_px = order.price.unwrap_or(Decimal::ZERO);

        out.push(ServerMessage::OrderFilled(OrderFilled {
            client_order_id: order.client_order_id.clone(),
            venue_order_id: venue_order_id.clone(),
            trade_id: self.next_id("T"),
            symbol: order.symbol.clone(),
            side: order.side,
            last_qty,
            last_px,
            leaves_qty,
            commission: Decimal::ZERO,
            ts_event: ts,
        }));

        if leaves_qty > Decimal::ZERO {
            self.open.push(OpenOrder {
                venue_order_id,
                submit: order,
                leaves_qty,
            });
        }

        out
    }

    fn on_cancel(&mut self, client_order_id: ClientOrderId, ts: u64) -> Vec<ServerMessage> {
        if let Some(pos) = self
            .open
            .iter()
            .position(|o| o.submit.client_order_id == client_order_id)
        {
            let o = self.open.remove(pos);
            vec![ServerMessage::OrderCanceled {
                client_order_id,
                venue_order_id: o.venue_order_id,
                ts_event: ts,
            }]
        } else {
            vec![ServerMessage::OrderRejected {
                client_order_id,
                reason: "unknown order".into(),
                ts_event: ts,
            }]
        }
    }

    pub fn open_orders(&self) -> &[OpenOrder] {
        &self.open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mogwai_protocol::{OrderType, Side, TimeInForce};
    use rust_decimal::prelude::FromPrimitive;

    fn order(id: &str, qty: i64) -> SubmitOrder {
        SubmitOrder {
            client_order_id: id.into(),
            symbol: "BTCUSDT".into(),
            side: Side::Buy,
            order_type: OrderType::Limit,
            quantity: Decimal::from(qty),
            price: Some(Decimal::from(100)),
            time_in_force: TimeInForce::Gtc,
        }
    }

    #[test]
    fn submit_fully_fills_by_default() {
        let mut e = Engine::new();
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        assert!(matches!(out[0], ServerMessage::OrderAccepted { .. }));
        let ServerMessage::OrderFilled(f) = &out[1] else {
            panic!("expected fill")
        };
        assert_eq!(f.leaves_qty, Decimal::ZERO);
        assert!(e.open_orders().is_empty());
    }

    #[test]
    fn armed_partial_leaves_remainder_resting() {
        let mut e = Engine::new();
        e.arm(Divergence::PartialFillNext {
            client_order_id: "O1".into(),
            fraction: Decimal::from_f64(0.3).unwrap(),
        });
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        let ServerMessage::OrderFilled(f) = &out[1] else {
            panic!("expected fill")
        };
        assert_eq!(f.last_qty, Decimal::from(3));
        assert_eq!(f.leaves_qty, Decimal::from(7));
        assert_eq!(e.open_orders().len(), 1);
    }

    #[test]
    fn armed_reject_blocks_submit() {
        let mut e = Engine::new();
        e.arm(Divergence::RejectNextSubmit {
            reason: "risk".into(),
        });
        let out = e.process(ClientMessage::SubmitOrder(order("O1", 10)), 1);
        assert_eq!(out.len(), 1);
        assert!(matches!(out[0], ServerMessage::OrderRejected { .. }));
    }
}
