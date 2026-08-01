// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Per-account ledger registry. Shared market data remains on `AppState`.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
};

use mogwai_engine::Engine;
use mogwai_protocol::{AccountId, CommandClass, InstrumentDef};
use rust_decimal::Decimal;
use serde::Serialize;
use tokio::sync::{Mutex as AsyncMutex, Notify};

pub(crate) struct AccountSlot {
    pub(crate) id: AccountId,
    pub(crate) generation: u64,
    pub(crate) tombstoned: AtomicBool,
    pub(crate) closed: Notify,
    pub(crate) engine: AsyncMutex<Engine>,
    pub(crate) delay_ms: AtomicU64,
    /// The six armed `CommandLatency` windows, in sim-ms, `0` when nothing is
    /// armed. ACT delays are how long the venue takes to touch the book after
    /// the command reaches it; ACK delays are how long it then takes to report
    /// what it did, on top of any armed `DelayAcks`. Read through `act_ms` and
    /// `ack_ms` rather than matched on at each call site.
    pub(crate) submit_act_ms: AtomicU64,
    pub(crate) modify_act_ms: AtomicU64,
    pub(crate) cancel_act_ms: AtomicU64,
    pub(crate) submit_ack_ms: AtomicU64,
    pub(crate) modify_ack_ms: AtomicU64,
    pub(crate) cancel_ack_ms: AtomicU64,
    pub(crate) dark_until_ns: AtomicU64,
    pub(crate) stall_until_ns: AtomicU64,
    pub(crate) sessions: AtomicUsize,
    pub(crate) last_seen_ns: AtomicU64,
}

pub(crate) struct SessionLease {
    slot: Arc<AccountSlot>,
}
impl SessionLease {
    pub(crate) fn acquire(slot: Arc<AccountSlot>, now_ns: u64) -> Self {
        slot.sessions.fetch_add(1, Ordering::Relaxed);
        slot.last_seen_ns.store(now_ns, Ordering::Relaxed);
        Self { slot }
    }
    pub(crate) fn slot(&self) -> &Arc<AccountSlot> {
        &self.slot
    }
}
impl Drop for SessionLease {
    fn drop(&mut self) {
        self.slot.sessions.fetch_sub(1, Ordering::Relaxed);
    }
}

impl AccountSlot {
    /// Armed ACT delay in sim-ms for `class`, `0` when nothing is armed.
    pub(crate) fn act_ms(&self, class: CommandClass) -> u64 {
        match class {
            CommandClass::Submit => self.submit_act_ms.load(Ordering::Relaxed),
            CommandClass::Modify => self.modify_act_ms.load(Ordering::Relaxed),
            CommandClass::Cancel => self.cancel_act_ms.load(Ordering::Relaxed),
        }
    }

    /// Armed ACK delay in sim-ms for `class`, `0` when nothing is armed.
    pub(crate) fn ack_ms(&self, class: CommandClass) -> u64 {
        match class {
            CommandClass::Submit => self.submit_ack_ms.load(Ordering::Relaxed),
            CommandClass::Modify => self.modify_ack_ms.load(Ordering::Relaxed),
            CommandClass::Cancel => self.cancel_ack_ms.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone)]
pub(crate) struct AccountTemplate {
    pub(crate) instruments: Arc<[InstrumentDef]>,
    pub(crate) balances: HashMap<String, Decimal>,
}
#[derive(Debug)]
pub(crate) enum RegistryError {
    AtCapacity,
}
pub(crate) struct AccountRegistry {
    slots: Mutex<HashMap<AccountId, Arc<AccountSlot>>>,
    next_generation: AtomicU64,
    template: AccountTemplate,
    max_accounts: usize,
}
#[derive(Debug, Clone, Serialize)]
pub(crate) struct AccountSummary {
    pub(crate) account_id: AccountId,
    pub(crate) generation: u64,
    pub(crate) sessions: usize,
    pub(crate) open_orders: usize,
    pub(crate) last_seen_ns: u64,
}

impl AccountRegistry {
    pub(crate) fn new(template: AccountTemplate, max_accounts: usize) -> Self {
        Self {
            slots: Mutex::new(HashMap::new()),
            next_generation: AtomicU64::new(1),
            template,
            max_accounts,
        }
    }
    #[allow(
        clippy::unwrap_in_result,
        reason = "a poisoned registry cannot safely continue"
    )]
    pub(crate) fn acquire(
        &self,
        id: &AccountId,
        now_ns: u64,
    ) -> Result<Arc<AccountSlot>, RegistryError> {
        let mut slots = self.slots.lock().expect("account registry poisoned");
        if let Some(slot) = slots.get(id) {
            slot.last_seen_ns.store(now_ns, Ordering::Relaxed);
            return Ok(Arc::clone(slot));
        }
        if self.max_accounts != 0 && slots.len() >= self.max_accounts {
            return Err(RegistryError::AtCapacity);
        }
        let slot = Arc::new(AccountSlot {
            id: id.clone(),
            generation: self.next_generation.fetch_add(1, Ordering::Relaxed),
            tombstoned: AtomicBool::new(false),
            closed: Notify::new(),
            engine: AsyncMutex::new(Engine::with_instruments_and_balances(
                id.clone(),
                self.template.instruments.to_vec(),
                self.template.balances.clone(),
            )),
            delay_ms: AtomicU64::new(0),
            submit_act_ms: AtomicU64::new(0),
            modify_act_ms: AtomicU64::new(0),
            cancel_act_ms: AtomicU64::new(0),
            submit_ack_ms: AtomicU64::new(0),
            modify_ack_ms: AtomicU64::new(0),
            cancel_ack_ms: AtomicU64::new(0),
            dark_until_ns: AtomicU64::new(0),
            stall_until_ns: AtomicU64::new(0),
            sessions: AtomicUsize::new(0),
            last_seen_ns: AtomicU64::new(now_ns),
        });
        slots.insert(id.clone(), Arc::clone(&slot));
        Ok(slot)
    }
    #[allow(dead_code)]
    pub(crate) fn get(&self, id: &AccountId) -> Option<Arc<AccountSlot>> {
        self.slots
            .lock()
            .expect("account registry poisoned")
            .get(id)
            .cloned()
    }
    pub(crate) fn destroy(&self, id: &AccountId) -> bool {
        let slot = self
            .slots
            .lock()
            .expect("account registry poisoned")
            .remove(id);
        if let Some(slot) = slot {
            slot.tombstoned.store(true, Ordering::Relaxed);
            slot.closed.notify_waiters();
            true
        } else {
            false
        }
    }
    pub(crate) fn reap_idle(&self, now_ns: u64, idle_ns: u64) -> Vec<AccountId> {
        if idle_ns == 0 {
            return vec![];
        }
        let mut slots = self.slots.lock().expect("account registry poisoned");
        let ids: Vec<_> = slots
            .iter()
            .filter(|(_, s)| {
                s.sessions.load(Ordering::Relaxed) == 0
                    && now_ns.saturating_sub(s.last_seen_ns.load(Ordering::Relaxed)) >= idle_ns
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in &ids {
            if let Some(slot) = slots.remove(id) {
                slot.tombstoned.store(true, Ordering::Relaxed);
                slot.closed.notify_waiters();
            }
        }
        ids
    }
    #[allow(dead_code)]
    pub(crate) fn len(&self) -> usize {
        self.slots.lock().expect("account registry poisoned").len()
    }
    pub(crate) async fn summaries(&self) -> Vec<AccountSummary> {
        let slots: Vec<_> = self
            .slots
            .lock()
            .expect("account registry poisoned")
            .values()
            .cloned()
            .collect();
        let mut out = Vec::with_capacity(slots.len());
        for slot in slots {
            let open_orders = slot.engine.lock().await.book_shape().open_orders;
            out.push(AccountSummary {
                account_id: slot.id.clone(),
                generation: slot.generation,
                sessions: slot.sessions.load(Ordering::Relaxed),
                open_orders,
                last_seen_ns: slot.last_seen_ns.load(Ordering::Relaxed),
            });
        }
        out.sort_by(|a, b| a.account_id.cmp(&b.account_id));
        out
    }
}

impl AccountSlot {
    /// Resolves when this slot is torn down, immediately if it already was.
    ///
    /// `Notify::notify_waiters` wakes only the waiters already registered, so a
    /// session that starts waiting after `destroy` has fired would sleep
    /// forever - and a session that never touches the engine (a market-data-only
    /// socket) would then keep the removed `Engine` alive for the daemon's life.
    /// `enable()` registers this future BEFORE the tombstone is read, and
    /// `destroy`/`reap_idle` set the tombstone BEFORE they notify, so the two
    /// orders interlock: whichever side goes first, the waiter observes it.
    pub(crate) async fn wait_for_teardown(&self) {
        let notified = self.closed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.tombstoned.load(Ordering::Relaxed) {
            return;
        }
        notified.await;
    }

    /// A slot with no registry behind it, for the writer/pump tests that need
    /// the havoc atomics and nothing else. Its engine carries the same id as the
    /// slot, so a snapshot taken through it is still self-describing.
    #[allow(dead_code)]
    pub(crate) fn detached_for_tests() -> Arc<Self> {
        let id = AccountId::parse("TEST-001").expect("static account id");
        Arc::new(Self {
            id: id.clone(),
            generation: 1,
            tombstoned: AtomicBool::new(false),
            closed: Notify::new(),
            engine: AsyncMutex::new(Engine::with_instruments_and_balances(
                id,
                Vec::new(),
                HashMap::new(),
            )),
            delay_ms: AtomicU64::new(0),
            submit_act_ms: AtomicU64::new(0),
            modify_act_ms: AtomicU64::new(0),
            cancel_act_ms: AtomicU64::new(0),
            submit_ack_ms: AtomicU64::new(0),
            modify_ack_ms: AtomicU64::new(0),
            cancel_ack_ms: AtomicU64::new(0),
            dark_until_ns: AtomicU64::new(0),
            stall_until_ns: AtomicU64::new(0),
            sessions: AtomicUsize::new(0),
            last_seen_ns: AtomicU64::new(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Weak;

    fn registry(max_accounts: usize) -> AccountRegistry {
        AccountRegistry::new(
            AccountTemplate {
                instruments: Arc::from(mogwai_protocol::default_instruments()),
                balances: HashMap::new(),
            },
            max_accounts,
        )
    }

    fn id(raw: &str) -> AccountId {
        AccountId::parse(raw).unwrap()
    }

    #[test]
    fn teardown_actually_drops_the_slot() {
        let registry = registry(0);
        let account = id("ACC-A");
        let slot = registry.acquire(&account, 1).unwrap();
        let weak: Weak<AccountSlot> = Arc::downgrade(&slot);
        assert!(registry.destroy(&account));
        drop(slot);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn teardown_of_unknown_account_is_404() {
        assert!(!registry(0).destroy(&id("UNKNOWN")));
    }

    #[test]
    fn idle_account_is_reaped() {
        let registry = registry(0);
        let account = id("IDLE");
        registry.acquire(&account, 1).unwrap();
        assert_eq!(registry.reap_idle(101, 100), vec![account]);
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn a_reaped_account_is_not_the_one_that_was_reactivated() {
        let registry = registry(0);
        let account = id("RACE");
        let original = registry.acquire(&account, 1).unwrap();
        let original_generation = original.generation;
        assert_eq!(registry.reap_idle(101, 100), vec![account.clone()]);
        let reactivated = registry.acquire(&account, 102).unwrap();
        assert_ne!(reactivated.generation, original_generation);
        assert!(
            reactivated
                .engine
                .blocking_lock()
                .account_snapshot(102)
                .positions
                .is_empty()
        );
    }

    #[test]
    fn account_with_a_live_session_is_never_reaped() {
        let registry = registry(0);
        let account = id("LIVE");
        let slot = registry.acquire(&account, 1).unwrap();
        let _lease = SessionLease::acquire(slot, 1);
        assert!(registry.reap_idle(101, 100).is_empty());
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn a_dropped_lease_leaks_no_session_count() {
        // Every abnormal socket exit - a panic in the writer, close-on-overload,
        // an early return on a failed subscribe - runs Drop and nothing else. A
        // leaked increment is invisible until the account proves unreapable
        // hours later, so the RAII pair gets its own gate.
        let registry = registry(0);
        let slot = registry.acquire(&id("LEASE"), 1).unwrap();
        {
            let _lease = SessionLease::acquire(Arc::clone(&slot), 1);
            assert_eq!(slot.sessions.load(Ordering::Relaxed), 1);
        }
        assert_eq!(slot.sessions.load(Ordering::Relaxed), 0);
        assert_eq!(registry.reap_idle(101, 100), vec![id("LEASE")]);
    }

    #[tokio::test]
    async fn a_session_wakes_on_a_teardown_that_precedes_its_wait() {
        // `notify_waiters` wakes only already-registered waiters, so a session
        // that begins waiting AFTER the destroy must still resolve - otherwise
        // a socket that connected microseconds before a DELETE sleeps forever,
        // pinning the removed engine's retention.
        let registry = registry(0);
        let slot = registry.acquire(&id("GONE"), 1).unwrap();
        assert!(registry.destroy(&id("GONE")));
        tokio::time::timeout(std::time::Duration::from_secs(5), slot.wait_for_teardown())
            .await
            .expect("a tombstoned slot resolves its teardown immediately");
    }

    #[test]
    fn new_account_beyond_max_accounts_is_refused() {
        let registry = registry(2);
        registry.acquire(&id("ACC-A"), 1).unwrap();
        registry.acquire(&id("ACC-B"), 1).unwrap();
        assert!(matches!(
            registry.acquire(&id("ACC-C"), 1),
            Err(RegistryError::AtCapacity)
        ));
        assert_eq!(registry.len(), 2);
    }

    #[tokio::test]
    async fn registry_holds_the_target_account_count() {
        let registry = registry(0);
        for n in 0..2_000 {
            registry.acquire(&id(&format!("ACC-{n}")), n).unwrap();
        }
        assert_eq!(registry.len(), 2_000);
        assert_eq!(registry.summaries().await.len(), 2_000);
    }
}
