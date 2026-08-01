// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Process-wide synthesized tapes shared by websocket subscriptions.
//!
//! A `GeneratedSource`'s entire output is a pure function of `(scalars, seed,
//! start_ts, session_profile, regime)`; `scalars`/`session_profile` come from
//! the symbol's `InstrumentProfile`, `seed` from `seed_for(symbol)` and
//! `start_ts` from the boot-derived data origin. Two subscriptions therefore
//! produce byte-identical streams exactly when their `(symbol, data_origin,
//! regime)` triples are equal, which is why that triple - and not `(symbol,
//! data_origin)` - is the sharing key. The regime cannot be lifted out of the
//! key: it is consumed INSIDE the walk (it divides the ACD duration draw,
//! scales and clamps the GARCH return, and a crossed `ReopenGap` advances the
//! generator's own clock), so a shared generator provably cannot carry a
//! per-subscriber regime. Arming a data regime therefore costs a tape, and
//! data-surface havoc stays scoped to the account that armed it by
//! construction rather than by a per-subscriber overlay.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use mogwai_data::TickEvent;
use mogwai_protocol::{MarketRegime, ServerMessage, SimClock};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, broadcast, mpsc};

use crate::{
    config::{Config, now_ns},
    source,
};

/// Cursor sentinels. Both sit at the top of the `u64` range, where a `ts_event`
/// (unix nanos) cannot reach, so the packed cursor stays a single relaxed load.
const STARTING: u64 = u64::MAX - 1;
const POISONED: u64 = u64::MAX;

/// Maximum wall time one slice of a pacing sleep runs before the tape thread
/// re-checks its cancel flag. Bounds how long a cancelled tape can stay parked
/// mid-gap, so a reap completes promptly instead of after the full inter-tick
/// wall gap (which identity pacing caps at `gap_cap_ms`, and accelerated pacing
/// does not cap at all).
pub(crate) const TAPE_SLEEP_POLL: Duration = Duration::from_millis(20);

/// Poll slice of the `speed == 0.0` headroom park. Matches the send-poll
/// cadence the private replay used when it parked on a full connection
/// channel, because this park replaces exactly that backpressure.
const TAPE_HEADROOM_POLL: Duration = Duration::from_millis(5);

/// The identity of a synthesized tape. Two subscriptions produce
/// byte-identical streams exactly when these are equal (see the module doc).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct TapeKey {
    pub(crate) symbol: String,
    pub(crate) data_origin_ns: u64,
    pub(crate) regime: RegimeKey,
}

/// Hashable projection of `Option<MarketRegime>`. `MarketRegime` carries `f64`
/// and so derives neither `Eq` nor `Hash`; the `f64` fields go through
/// `to_bits`, which is the correct comparison here: the generator's arithmetic
/// is a pure function of the bit pattern, so bit-equal regimes yield identical
/// bytes and nothing else may share a tape. Non-finite values cannot reach
/// here - `validate_market_regime` rejects them, and the unfireable-`ReopenGap`
/// strip runs before the key is built - so `to_bits` has no NaN-payload
/// ambiguity to resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum RegimeKey {
    Clean,
    VolStorm {
        vol_mult: u64,
    },
    LiquidityDrought {
        thin_factor: u64,
    },
    SessionEdgeSpike {
        start_hour: u8,
        end_hour: u8,
        extra_vol_mult: u64,
    },
    ReopenGap {
        at_ts: u64,
        halt_secs: u64,
        gap_frac: u64,
    },
}

impl RegimeKey {
    pub(crate) fn from_regime(regime: Option<&MarketRegime>) -> Self {
        match regime {
            None => Self::Clean,
            Some(MarketRegime::VolStorm { vol_mult }) => Self::VolStorm {
                vol_mult: vol_mult.to_bits(),
            },
            Some(MarketRegime::LiquidityDrought { thin_factor }) => Self::LiquidityDrought {
                thin_factor: thin_factor.to_bits(),
            },
            Some(MarketRegime::SessionEdgeSpike {
                start_hour,
                end_hour,
                extra_vol_mult,
            }) => Self::SessionEdgeSpike {
                start_hour: *start_hour,
                end_hour: *end_hour,
                extra_vol_mult: extra_vol_mult.to_bits(),
            },
            Some(MarketRegime::ReopenGap {
                at_ts,
                halt_secs,
                gap_frac,
            }) => Self::ReopenGap {
                at_ts: *at_ts,
                halt_secs: *halt_secs,
                gap_frac: gap_frac.to_bits(),
            },
        }
    }
}

/// One pre-serialized market-data frame, broadcast to every attached
/// subscriber. Serialization happens ONCE per tape tick rather than once per
/// subscriber - the second half of the win, after thread count. `Clone` is not
/// optional: `broadcast::Receiver::recv` clones out of the ring, which is
/// precisely why `payload` is an `Arc<str>` and not a `String`.
#[derive(Clone)]
pub(crate) struct TapeFrame {
    /// 1-based commit index. `ts_event` is the wire-visible ordering key, but
    /// the zero-speed headroom throttle needs to measure a subscriber's lag in
    /// FRAMES against `fanout_depth`, and a nanosecond timestamp cannot be
    /// compared to a ring depth.
    pub(crate) seq: u64,
    pub(crate) ts_event: u64,
    pub(crate) payload: Arc<str>,
}

/// What the tape thread has committed so far. A bare `u64` with a `u64::MAX`
/// sentinel is not sufficient: "no frame yet" and "poisoned" are distinct
/// attach seams, and reading either as a timestamp makes a fresh subscriber
/// either backfill forever or drop every live frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CursorState {
    /// The tape thread has committed no frame yet; its seek may still be in
    /// flight. There is nothing to backfill toward.
    Starting,
    Live(u64),
    /// The tape's positioning seek exhausted its budget, or the symbol has no
    /// source at all. Every subscriber to this key gets the same answer, which
    /// is the answer each would have computed privately.
    Poisoned,
}

fn unpack_cursor(value: u64) -> CursorState {
    match value {
        STARTING => CursorState::Starting,
        POISONED => CursorState::Poisoned,
        ts => CursorState::Live(ts),
    }
}

pub(crate) struct Tape {
    tx: broadcast::Sender<TapeFrame>,
    /// Last frame the tape thread has COMMITTED to broadcasting, as a packed
    /// [`CursorState`]. Stored BEFORE the send, deliberately: an attacher takes
    /// `rx = tx.subscribe()` and this cursor under the registry lock while the
    /// tape thread runs free, so with store-then-send it can read a cursor for
    /// a frame it also receives - a duplicate, which the `ts_event >
    /// high_water` filter drops. Send-then-store would instead let a frame go
    /// out before the `subscribe()` while the cursor still reads the previous
    /// one: a GAP, which nothing can recover and which breaks the ascending
    /// `ts_event` contract the adapter's poll cursor relies on, silently.
    cursor: Arc<AtomicU64>,
    /// Commit counter feeding `TapeFrame::seq`. Zero means "nothing committed".
    committed: Arc<AtomicU64>,
    /// `ts_event` of the FIRST committed frame, zero until then. With `cursor`
    /// and `committed` it yields the tape's realized cadence, which is the only
    /// way a fanout task can convert a backfill span in nanoseconds into a
    /// count of frames to compare against the ring depth.
    first_ts: Arc<AtomicU64>,
    /// The configured ring depth, so a lease can size a backfill against it.
    depth: usize,
    /// The sim-now this tape seeked to at creation. Its first frame is the
    /// first tick at or after this instant, which is what lets a subscriber
    /// that attached BEFORE any commit start its own backfill immediately,
    /// bounded at `seek_target_ns - 1`, instead of waiting on the live cadence
    /// to learn where the tape begins.
    seek_target_ns: u64,
    cancel: Arc<AtomicBool>,
    /// Woken on cancel and on every commit, so a fanout task waiting for the
    /// tape's startup state is interrupted promptly rather than at the next
    /// frame (which on an idle identity tape is seconds away).
    wake: Arc<Notify>,
    /// Only consulted when `speed == 0.0`: the per-lease "last forwarded seq"
    /// cells the zero-speed tape throttles against. Registered at attach and
    /// removed at lease drop.
    subscriber_seqs: Mutex<Vec<Arc<AtomicU64>>>,
}

struct Entry {
    tape: Arc<Tape>,
    refs: usize,
    handle: Option<JoinHandle<()>>,
    /// Released when the entry leaves the map, not when the thread exits: the
    /// cap counts REACHABLE tapes. Holding it to thread exit would let a
    /// cap-of-one resubscribe be refused by its own dying predecessor; the
    /// bounded overshoot is the reaper's queue depth, and every overshooting
    /// thread is already cancelled and exits within one [`TAPE_SLEEP_POLL`].
    _permit: OwnedSemaphorePermit,
}

struct ReapRequest(JoinHandle<()>);

pub(crate) struct TapeRegistry {
    inner: Mutex<HashMap<TapeKey, Entry>>,
    permits: Arc<Semaphore>,
    fanout_depth: usize,
    reaper: mpsc::UnboundedSender<ReapRequest>,
}

/// A subscriber's hold on a tape. Dropping it decrements the refcount and, at
/// zero, cancels the tape and hands its thread to the reaper.
pub(crate) struct TapeLease {
    registry: Arc<TapeRegistry>,
    key: TapeKey,
    pub(crate) tape: Arc<Tape>,
    pub(crate) rx: broadcast::Receiver<TapeFrame>,
    /// Cursor snapshot taken under the registry lock at attach, as a state
    /// rather than a sentinel-bearing integer.
    pub(crate) attach: CursorState,
    /// This subscriber's own progress cell, published for the zero-speed
    /// throttle and deregistered on drop.
    pub(crate) progress: Arc<AtomicU64>,
}

impl TapeLease {
    /// Interrupts a wait on the tape's startup state: cancel, or a fresh commit.
    pub(crate) fn wake(&self) -> &Notify {
        &self.tape.wake
    }

    /// The tape's fanout ring depth: how many committed frames a subscriber may
    /// fall behind before the ring ejects it.
    pub(crate) fn ring_depth(&self) -> usize {
        self.tape.depth
    }

    /// The instant this tape seeked to; its first frame is at or after it.
    pub(crate) fn seek_target_ns(&self) -> u64 {
        self.tape.seek_target_ns
    }
}

impl Tape {
    pub(crate) fn cursor_state(&self) -> CursorState {
        unpack_cursor(self.cursor.load(Ordering::Relaxed))
    }

    /// How many frames behind the live cursor `ts` sits, measured against this
    /// tape's own realized cadence. `None` while the tape has committed too
    /// little to have a cadence - a caller must then not guess, because the
    /// only thing this feeds is a refusal.
    pub(crate) fn frames_behind(&self, ts: u64) -> Option<u64> {
        let CursorState::Live(cursor) = self.cursor_state() else {
            return None;
        };
        let first = self.first_ts.load(Ordering::Relaxed);
        let committed = self.committed.load(Ordering::Relaxed);
        if first == 0 || committed < 2 || cursor <= first {
            return None;
        }
        let cadence = ((cursor - first) / (committed - 1)).max(1);
        Some(cursor.saturating_sub(ts) / cadence)
    }

    /// Commit one synthetic frame without starting a generator thread.
    ///
    /// Deliberately test-only: production frames must originate from
    /// `spawn_tape`, which owns the pacing and the source lifetime.
    #[cfg(test)]
    pub(crate) fn publish_for_test(&self, ts_event: u64) {
        let seq = self.committed.fetch_add(1, Ordering::Relaxed) + 1;
        self.cursor.store(ts_event, Ordering::Relaxed);
        drop(self.tx.send(TapeFrame {
            seq,
            ts_event,
            payload: Arc::from("{}"),
        }));
        self.wake.notify_waiters();
    }
}

impl Drop for TapeLease {
    fn drop(&mut self) {
        {
            let mut seqs = self
                .tape
                .subscriber_seqs
                .lock()
                .expect("tape subscriber cursors poisoned");
            if let Some(at) = seqs
                .iter()
                .position(|cell| Arc::ptr_eq(cell, &self.progress))
            {
                seqs.swap_remove(at);
            }
        }
        let mut inner = self
            .registry
            .inner
            .lock()
            .expect("tape registry lock poisoned");
        // A poisoned tape removes its own entry, so the key may already be
        // gone; the refcount it carried died with it.
        let Some(entry) = inner.get_mut(&self.key) else {
            return;
        };
        // Distinct tapes can share a key over time (poison, then a retry that
        // re-inserts): only decrement the entry this lease actually holds.
        if !Arc::ptr_eq(&entry.tape, &self.tape) {
            return;
        }
        entry.refs -= 1;
        if entry.refs > 0 {
            return;
        }
        let mut entry = inner.remove(&self.key).expect("entry was present");
        entry.tape.cancel.store(true, Ordering::Relaxed);
        entry.tape.wake.notify_waiters();
        if let Some(handle) = entry.handle.take() {
            // Joining here would block a tokio worker; leaving the thread
            // unjoined would reproduce the detached-thread leak. The reaper
            // joins it on `spawn_blocking` instead.
            drop(self.registry.reaper.send(ReapRequest(handle)));
        }
    }
}

pub(crate) struct TapeSpawn {
    /// The full table, not one profile: the tape body calls
    /// `source::build_live_source`, which takes `&InstrumentProfiles`.
    pub(crate) profiles: Arc<source::InstrumentProfiles>,
    /// `symbol` and `data_origin_ns` are read back off the [`TapeKey`] passed
    /// alongside this, so they are deliberately absent here.
    pub(crate) regime: Option<MarketRegime>,
    pub(crate) sim: SimClock,
    pub(crate) speed: f64,
    pub(crate) gap_cap_ms: u64,
    pub(crate) fanout_depth: usize,
    pub(crate) zero_speed_stall_ms: u64,
}

#[derive(Debug)]
pub(crate) struct TapeCapacity;

/// Best-effort human-readable text from a `JoinHandle::join` panic payload -
/// the common `&str` and `String` shapes, else a placeholder.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("non-string panic payload")
}

impl TapeRegistry {
    pub(crate) fn new(
        cfg: &Config,
        permits: Arc<Semaphore>,
    ) -> (Arc<Self>, tokio::task::JoinHandle<()>) {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let registry = Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            permits,
            fanout_depth: cfg.fanout_depth,
            reaper: tx,
        });
        // Joins off the async worker: joining inside `Drop` would block a tokio
        // worker, and leaving the thread unjoined would reproduce the
        // detached-thread leak (E.7).
        let reaper = tokio::spawn(async move {
            while let Some(ReapRequest(handle)) = rx.recv().await {
                match tokio::task::spawn_blocking(move || handle.join()).await {
                    Ok(Ok(())) => {}
                    // A panicked tape thread ended the feed of every subscriber
                    // attached to it; swallowing the payload here would leave
                    // them with an idle-but-healthy socket and no sign of it.
                    Ok(Err(panic)) => tracing::error!(
                        panic = %panic_message(&panic),
                        "tape thread panicked; its market-data feed ended silently"
                    ),
                    Err(e) => tracing::warn!(%e, "tape join task panicked"),
                }
            }
        });
        (registry, reaper)
    }

    /// Construct a registry for synchronous unit tests which never attach a
    /// live tape. Their assertions exercise HTTP/config code and have no Tokio
    /// runtime in which a reaper task could be born.
    #[cfg(test)]
    pub(crate) fn for_tests(cfg: &Config, permits: Arc<Semaphore>) -> Arc<Self> {
        let (reaper, _rx) = mpsc::unbounded_channel();
        Arc::new(Self {
            inner: Mutex::new(HashMap::new()),
            permits,
            fanout_depth: cfg.fanout_depth,
            reaper,
        })
    }

    /// Attach to the tape for `key`, creating and starting it if absent.
    ///
    /// `Err(TapeCapacity)` when a NEW tape is needed and the permit pool is
    /// exhausted; attaching to an existing tape never consumes a permit and so
    /// can never be refused for capacity. A tape already `Poisoned` is returned
    /// as a lease whose `attach` is `Poisoned` - this call does NOT itself
    /// report the dead seek, because the seek runs on the tape thread and may
    /// still be in flight when it returns. Resolving that is the fanout task's
    /// job, so an attach during a seek in flight and an attach after it failed
    /// get the identical answer.
    pub(crate) fn attach(
        self: &Arc<Self>,
        key: TapeKey,
        spawn: TapeSpawn,
    ) -> Result<TapeLease, TapeCapacity> {
        self.attach_with(key, Some(spawn))
    }

    /// Attach a lease to a tape whose frames are driven directly by a test
    /// (`spawn = None` starts no generator thread). This isolates broadcast and
    /// fanout behavior from synthesis and sockets.
    #[cfg(test)]
    pub(crate) fn attach_inert(self: &Arc<Self>, key: TapeKey) -> Result<TapeLease, TapeCapacity> {
        self.attach_with(key, None)
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "a poisoned registry lock is unrecoverable state, not a capacity refusal: \
                  reporting it as one would tell a client to retry into a broken venue"
    )]
    fn attach_with(
        self: &Arc<Self>,
        key: TapeKey,
        spawn: Option<TapeSpawn>,
    ) -> Result<TapeLease, TapeCapacity> {
        let mut inner = self.inner.lock().expect("tape registry lock poisoned");
        if !inner.contains_key(&key) {
            // Sampled HERE, not on the tape thread, so an attacher can read it
            // off the tape the instant the entry exists.
            let seek_target = spawn
                .as_ref()
                .map_or_else(now_ns, |spawn| spawn.sim.sim_ns(now_ns()));
            let permit = Arc::clone(&self.permits)
                .try_acquire_owned()
                .map_err(|_| TapeCapacity)?;
            let (tx, _) = broadcast::channel(self.fanout_depth);
            let tape = Arc::new(Tape {
                tx,
                cursor: Arc::new(AtomicU64::new(STARTING)),
                committed: Arc::new(AtomicU64::new(0)),
                first_ts: Arc::new(AtomicU64::new(0)),
                depth: self.fanout_depth,
                seek_target_ns: seek_target,
                cancel: Arc::new(AtomicBool::new(false)),
                wake: Arc::new(Notify::new()),
                subscriber_seqs: Mutex::new(Vec::new()),
            });
            let handle = spawn.map(|spawn| {
                spawn_tape(Arc::clone(&tape), key.clone(), spawn, Arc::downgrade(self))
            });
            inner.insert(
                key.clone(),
                Entry {
                    tape,
                    refs: 0,
                    handle,
                    _permit: permit,
                },
            );
        }
        let entry = inner.get_mut(&key).expect("entry was just inserted");
        entry.refs += 1;
        let tape = Arc::clone(&entry.tape);
        // Under the same lock as the refcount bump, so a reap cannot interleave.
        let attach = unpack_cursor(tape.cursor.load(Ordering::Relaxed));
        let rx = tape.tx.subscribe();
        // Seeded at the tape's current commit count, not 0: a fresh attacher is
        // not "behind" everything the tape produced before it existed, and a
        // zero seed would stall a zero-speed tape for every new subscriber.
        let progress = Arc::new(AtomicU64::new(tape.committed.load(Ordering::Relaxed)));
        tape.subscriber_seqs
            .lock()
            .expect("tape subscriber cursors poisoned")
            .push(Arc::clone(&progress));
        Ok(TapeLease {
            registry: Arc::clone(self),
            key,
            tape,
            rx,
            attach,
            progress,
        })
    }

    /// Retire a tape whose seek died, so a later subscribe for the same key
    /// retries the seek rather than inheriting a dead entry. Called by the tape
    /// thread on its way out, which is why the handle is dropped rather than
    /// reaped: the thread cannot join itself, and it is already returning.
    fn poison(&self, key: &TapeKey, tape: &Arc<Tape>) {
        let mut inner = self.inner.lock().expect("tape registry lock poisoned");
        if inner.get(key).is_some_and(|e| Arc::ptr_eq(&e.tape, tape)) {
            inner.remove(key);
        }
    }

    /// How many tapes are currently REACHABLE (in the map). Plain
    /// `pub(crate)`, not `#[cfg(test)]`: the tape tests live in `main.rs`, a
    /// different module, and gating the accessor on test configuration would
    /// couple the registry's API surface to it for no benefit.
    #[allow(dead_code, reason = "read by the tape tests in main.rs")]
    pub(crate) fn live_tapes(&self) -> usize {
        self.inner
            .lock()
            .expect("tape registry lock poisoned")
            .len()
    }
}

/// The venue-side generator loop: one paced walk per tape key, serialized once
/// and broadcast to every attached fanout task.
fn spawn_tape(
    tape: Arc<Tape>,
    key: TapeKey,
    spawn: TapeSpawn,
    registry: Weak<TapeRegistry>,
) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let poison = |tape: &Arc<Tape>| {
            tape.cursor.store(POISONED, Ordering::Relaxed);
            tape.wake.notify_waiters();
            if let Some(registry) = registry.upgrade() {
                registry.poison(&key, tape);
            }
        };
        let symbols = [key.symbol.clone()];
        // Seek to the sim-now the registry sampled when this tape was created,
        // exactly as a private replay seeked at subscribe time: a fresh
        // subscriber's first live tick lands at the clock, with no accelerated
        // catch-up firehose and no identity backfill replayed at real-time
        // gaps. Read off the tape rather than re-sampled, so the value an
        // attacher bounds its backfill against is the value actually seeked.
        let sim_now = tape.seek_target_ns;
        let Some(mut source) = source::build_live_source(
            &symbols,
            None,
            spawn.regime,
            &spawn.profiles,
            key.data_origin_ns,
            sim_now,
        ) else {
            tracing::warn!(symbol = %key.symbol, "no source for tape symbol");
            poison(&tape);
            return;
        };
        // The generated tape never runs dry on its own, so a `None` FIRST tick
        // has exactly one meaning: the positioning seek could not reach its
        // target within its budget. The tape is dead before it starts.
        let Some(first) = source.next_tick() else {
            tracing::warn!(
                symbol = %key.symbol,
                sim_now,
                data_origin = key.data_origin_ns,
                budget = source::MAX_HISTORY_SEEK_TICKS,
                "tape could not be positioned; the seek exhausted its tick budget"
            );
            poison(&tape);
            return;
        };
        // Every pacing deadline is anchored to ONE monotonic `Instant` paired
        // with ONE wall-clock read, taken here. Nothing after this line reads
        // `CLOCK_REALTIME` again, so an NTP/leap step mid-session can neither
        // stall the feed nor make it burst.
        let wall_anchor = now_ns();
        let instant_anchor = Instant::now();
        // Identity pacing measures the gap from the PREVIOUS tick; with no
        // previous tick the first would emit unpaced, stamped a few seconds
        // past the clock. Seeding with the seek target paces it to its own
        // timestamp instead. The accelerated branch ignores this.
        let mut prev_ts = spawn.sim.is_identity().then_some(sim_now);
        let mut next_deadline = None;
        let mut next = Some(first);
        while let Some(tick) = next {
            if tape.cancel.load(Ordering::Relaxed) {
                break;
            }
            pace(
                &tape.cancel,
                &spawn,
                tick.ts_event(),
                &mut prev_ts,
                &mut next_deadline,
                wall_anchor,
                instant_anchor,
            );
            if tape.cancel.load(Ordering::Relaxed) {
                break;
            }
            // A zero-speed tape has no connection backpressure to park on - a
            // shared tape must never be stalled by one subscriber's socket - so
            // the throttle lives here instead, against the SLOWEST attached
            // lease. Applied before committing, so the ring is not overwritten
            // while every subscriber is still reading as fast as it can.
            if spawn.speed == 0.0 {
                await_headroom(&tape, &spawn);
            }
            if tape.cancel.load(Ordering::Relaxed) {
                break;
            }
            let ts_event = tick.ts_event();
            let msg = match tick {
                TickEvent::Trade(t) => ServerMessage::Trade(t),
                TickEvent::Quote(q) => ServerMessage::Quote(q),
            };
            let payload = match serde_json::to_string(&msg) {
                Ok(payload) => payload,
                Err(e) => {
                    tracing::error!(%e, "could not serialize market-data frame");
                    break;
                }
            };
            let seq = tape.committed.fetch_add(1, Ordering::Relaxed) + 1;
            if seq == 1 {
                tape.first_ts.store(ts_event, Ordering::Relaxed);
            }
            // Store the cursor BEFORE the send; see `Tape::cursor`.
            tape.cursor.store(ts_event, Ordering::Relaxed);
            // `send` returns `Err` whenever the receiver count is zero, which
            // happens routinely between the last lease drop and the reap
            // raising `cancel`. That is not an exit condition: the loop exits
            // on `cancel` only.
            drop(tape.tx.send(TapeFrame {
                seq,
                ts_event,
                payload: Arc::from(payload),
            }));
            tape.wake.notify_waiters();
            next = source.next_tick();
        }
    })
}

/// Park a `speed == 0.0` tape while the slowest attached subscriber is more
/// than half a ring behind, so an unthrottled tape does not overwrite frames
/// its readers are actively draining. A subscriber that is not merely slow but
/// STOPPED stops advancing its cell, so the park times out at
/// `zero_speed_stall_ms` and the tape resumes - one dead client costs one stall
/// and is then ejected by the ring as `FeedLagged`, rather than stalling the
/// tape forever.
fn await_headroom(tape: &Tape, spawn: &TapeSpawn) {
    let lead = (spawn.fanout_depth / 2).max(1) as u64;
    let deadline = Instant::now() + Duration::from_millis(spawn.zero_speed_stall_ms);
    loop {
        if tape.cancel.load(Ordering::Relaxed) {
            return;
        }
        let committed = tape.committed.load(Ordering::Relaxed);
        let slowest = tape
            .subscriber_seqs
            .lock()
            .expect("tape subscriber cursors poisoned")
            .iter()
            .map(|cell| cell.load(Ordering::Relaxed))
            .min();
        // No subscriber left: the tape is about to be reaped, so do not park.
        let Some(slowest) = slowest else { return };
        if committed.saturating_sub(slowest) <= lead {
            return;
        }
        let now = Instant::now();
        if now >= deadline {
            return;
        }
        std::thread::sleep((deadline - now).min(TAPE_HEADROOM_POLL));
    }
}

/// Pace one tick: deadline pacing against the simulated clock in accelerated
/// mode, chained gap pacing capped at `gap_cap_ms` in identity mode, and no
/// pacing at all at `speed == 0.0` (whose throttle is [`await_headroom`]).
fn pace(
    cancel: &AtomicBool,
    spawn: &TapeSpawn,
    ts: u64,
    prev: &mut Option<u64>,
    deadline: &mut Option<u64>,
    wall_anchor: u64,
    instant_anchor: Instant,
) {
    let target = if !spawn.sim.is_identity() {
        Some(spawn.sim.wall_ns(ts))
    } else if spawn.speed > 0.0 {
        // Pace at nanosecond resolution: dividing down to whole milliseconds
        // would collapse the micros-apart bursts the generator emits into
        // zero-delay sends, so the realized timeline would not track
        // original_timeline / speed.
        let wait = prev.map(|p| {
            let mut wait_ns = (ts.saturating_sub(p) as f64 / spawn.speed) as u64;
            if spawn.gap_cap_ms > 0 {
                wait_ns = wait_ns.min(spawn.gap_cap_ms.saturating_mul(1_000_000));
            }
            wait_ns
        });
        *prev = Some(ts);
        // Chained deadlines rather than a fresh relative sleep per tick: a
        // per-tick relative sleep never accounts for the time spent generating
        // and broadcasting the previous tick, so realized spacing would
        // progressively lag wall-clock time over a long session.
        wait.map(|wait_ns| {
            let d = deadline.unwrap_or(wall_anchor).saturating_add(wait_ns);
            *deadline = Some(d);
            d
        })
    } else {
        None
    };
    if let Some(target) = target {
        sleep_until_wall_cancellable(target, wall_anchor, instant_anchor, cancel);
    }
}

/// Sleep until `target_wall_ns` - a unix-ns wall deadline mapped through the
/// caller's `(wall_anchor, instant_anchor)` pairing onto the monotonic clock -
/// in [`TAPE_SLEEP_POLL`] slices, returning early once `cancel` is raised.
///
/// A single uninterruptible `thread::sleep` would leave a cancelled tape parked
/// for the whole remaining gap, and every unsubscribe/resubscribe/disconnect on
/// a connection waits behind that.
pub(crate) fn sleep_until_wall_cancellable(
    target_wall_ns: u64,
    wall_anchor: u64,
    instant_anchor: Instant,
    cancel: &AtomicBool,
) {
    let due = instant_anchor + Duration::from_nanos(target_wall_ns.saturating_sub(wall_anchor));
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let now = Instant::now();
        if now >= due {
            return;
        }
        std::thread::sleep((due - now).min(TAPE_SLEEP_POLL));
    }
}
