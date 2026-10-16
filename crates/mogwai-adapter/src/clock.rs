// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, ensure};
use mogwai_protocol::{SimClock, VenueClock};
use nautilus_common::{
    clock::{CallbackRegistry, Clock, validate_and_prepare_time_alert, validate_and_prepare_timer},
    runner::{TimeEventMessage, TimeEventSender, try_get_time_event_sender},
    timer::{TimeEvent, TimeEventCallback, TimerSchedule, create_valid_interval},
};
use nautilus_core::{DurationNanos, UUID4, UnixNanos, correctness::check_valid_string_utf8};
use nautilus_network::http::HttpClient;
use ustr::Ustr;

use crate::client::join_url;

/// Nanoseconds in one second.
///
/// Every timestamp this adapter handles is a `u64` of nanoseconds, so the
/// conversion appears wherever a wall or simulated span meets a seconds-shaped
/// value from a config knob or a nautilus API. Written once because the literal
/// is nine zeroes: a typo is a factor of ten in a quota interval or a timeout,
/// which is a wrong answer rather than a compile error.
pub(crate) const NANOS_PER_SEC: u64 = 1_000_000_000;

/// The nautilus `Clock` mogwai injects into a live node, reading and
/// firing on the shared simulated axis (`reference/clock.md`).
///
/// Host requirement: its timers fire from a `tokio::task::spawn_local` task (see
/// `MogwaiTimer::start`), so every timer-setting call (`set_timer_ns`,
/// `set_timer_schedule`, `set_time_alert_ns`) must always run on a thread driving a `tokio::task::LocalSet`,
/// or `spawn_local` panics. That is the only way a `TimeEventCallback::RustLocal`
/// (an `Rc`, `!Send` since nautilus PR 4496) - e.g. a bar aggregator's callback -
/// can be dispatched at all: the fire loop stays on the callback's owner thread
/// where nautilus's thread-local time-event registry resolves it. Broadarrow's
/// current-thread worker satisfies this; the unit tests wrap their bodies in a
/// `LocalSet`.
#[derive(Debug)]
pub struct MogwaiClock {
    sim: SimClock,
    timers: BTreeMap<Ustr, MogwaiTimer>,
    callbacks: CallbackRegistry,
    sender: Option<Arc<dyn TimeEventSender>>,
}

impl MogwaiClock {
    #[must_use]
    pub fn new(sim: SimClock, sender: Option<Arc<dyn TimeEventSender>>) -> Self {
        Self {
            sim,
            timers: BTreeMap::new(),
            callbacks: CallbackRegistry::new(),
            sender,
        }
    }

    fn clear_expired_timers(&mut self) {
        self.timers.retain(|_, timer| !timer.is_expired());
    }

    fn replace_existing_timer_if_needed(&mut self, name: &Ustr) {
        if let Some(mut timer) = self.timers.remove(name) {
            timer.cancel();
        }
    }
}

impl Clock for MogwaiClock {
    fn timestamp_ns(&self) -> UnixNanos {
        sim_now(self.sim)
    }

    fn timestamp_us(&self) -> u64 {
        self.timestamp_ns().as_micros()
    }

    fn timestamp_ms(&self) -> u64 {
        self.timestamp_ns().as_millis()
    }

    fn timestamp(&self) -> f64 {
        self.timestamp_ns().as_f64() / NANOS_PER_SEC as f64
    }

    fn timer_names(&self) -> Vec<&str> {
        self.timers
            .iter()
            .filter(|(_, timer)| !timer.is_expired())
            .map(|(name, _)| name.as_str())
            .collect()
    }

    fn timer_count(&self) -> usize {
        self.timers
            .iter()
            .filter(|(_, timer)| !timer.is_expired())
            .count()
    }

    fn timer_exists(&self, name: &Ustr) -> bool {
        // Filters expired timers exactly like `timer_names`/`timer_count`
        // above, mirroring nautilus `LiveClock` (common/src/live/clock.rs),
        // where all three introspection surfaces agree that a fired-and-done
        // timer no longer exists. LiveClock used to leave `timer_exists` as a
        // bare `contains_key` while the other two filtered; that asymmetry was
        // reported upstream and fixed, and this mirror follows it.
        self.timers
            .get(name)
            .is_some_and(|timer| !timer.is_expired())
    }

    fn register_default_handler(&mut self, callback: TimeEventCallback) {
        self.callbacks.register_default_handler(callback);
    }

    fn cancel_default_handler(&mut self) {
        self.callbacks.cancel_default_handler();
    }

    fn cancel_callbacks(&mut self) {
        self.callbacks.clear();
    }

    fn set_time_alert_ns(
        &mut self,
        name: &str,
        alert_time_ns: UnixNanos,
        callback: Option<TimeEventCallback>,
        allow_past: Option<bool>,
    ) -> anyhow::Result<()> {
        let ts_now = self.timestamp_ns();
        let (name, alert_time_ns) =
            validate_and_prepare_time_alert(name, alert_time_ns, allow_past, ts_now)?;

        // Check before replacing, as `set_timer_ns` below and nautilus
        // `LiveClock` do: a call that fails for want of a callback must leave
        // the named timer it would have replaced running.
        ensure!(
            callback.is_some() || self.callbacks.has_any_callback(&name),
            "No callbacks provided"
        );
        self.replace_existing_timer_if_needed(&name);

        let callback = if let Some(callback) = callback {
            self.callbacks.register_callback(name, callback.clone());
            callback
        } else {
            self.callbacks
                .get_callback(&name)
                .expect("callback was validated")
        };

        let mut timer = MogwaiTimer::new(
            MogwaiTimerSpec {
                name,
                cadence: Cadence::Fixed(alert_time_ns.as_u64().saturating_sub(ts_now.as_u64())),
                start_time_ns: ts_now,
                stop_time_ns: Some(alert_time_ns),
                callback,
                // An alert exactly at (or adjusted-to) now has a zero interval
                // that `create_valid_interval` coerces to 1ns, which would push
                // the scheduled fire past the stop bound and the pre-fire stop
                // check would swallow the alert. Fire immediately instead so
                // the event lands exactly on the inclusive stop boundary,
                // mirroring nautilus `LiveClock::set_time_alert_ns`
                // (common/src/live/clock.rs).
                fire_immediately: alert_time_ns == ts_now,
            },
            self.sim,
            self.sender.clone(),
        );
        timer.start(false);

        self.clear_expired_timers();
        self.timers.insert(name, timer);
        Ok(())
    }

    fn set_timer_ns(
        &mut self,
        name: &str,
        interval_ns: DurationNanos,
        start_time_ns: Option<UnixNanos>,
        stop_time_ns: Option<UnixNanos>,
        callback: Option<TimeEventCallback>,
        allow_past: Option<bool>,
        fire_immediately: Option<bool>,
    ) -> anyhow::Result<()> {
        let ts_now = self.timestamp_ns();
        let (name, start_time_ns, stop_time_ns, _allow_past, fire_immediately) =
            validate_and_prepare_timer(
                name,
                interval_ns,
                start_time_ns,
                stop_time_ns,
                allow_past,
                fire_immediately,
                ts_now,
            )?;

        ensure!(
            callback.is_some() || self.callbacks.has_any_callback(&name),
            "No callbacks provided"
        );
        self.replace_existing_timer_if_needed(&name);

        let callback = if let Some(callback) = callback {
            self.callbacks.register_callback(name, callback.clone());
            callback
        } else {
            self.callbacks
                .get_callback(&name)
                .expect("callback was validated")
        };
        let interval_ns = create_valid_interval(interval_ns).get();

        let mut timer = MogwaiTimer::new(
            MogwaiTimerSpec {
                name,
                cadence: Cadence::Fixed(interval_ns),
                start_time_ns,
                stop_time_ns,
                callback,
                fire_immediately,
            },
            self.sim,
            self.sender.clone(),
        );
        timer.start(true);

        self.clear_expired_timers();
        self.timers.insert(name, timer);
        Ok(())
    }

    // A schedule timer differs from the fixed-interval one above in one
    // deliberate way, and the reason is what the schedule's start means.
    // `set_timer_ns` takes an explicit start, so a past start is a request
    // to replay from there and the timer replays every elapsed interval.
    // A schedule's start is its anchor - a `CalendarSchedule` origin such as
    // a local 09:30 on some earlier date - and a host that anchors a daily
    // grid a year back is not asking for a year of events at arming. So
    // arming positions the timer on the first deadline at or after sim-now,
    // while a task that falls behind once armed still replays what it
    // missed in order, exactly as the fixed timer does: on the sim axis a
    // stall is not a reason to drop an event whose `ts_event` is known.
    // This is not nautilus `LiveTimer`'s behaviour: its past-due adjustment
    // applies only to fixed intervals, so a live schedule timer fires a stale
    // first deadline once, stamped in the past, and continues from there.
    // Never emitting a stale-stamped event at arming is the choice here.
    fn set_timer_schedule(
        &mut self,
        name: &str,
        schedule: Arc<dyn TimerSchedule>,
        stop_time_ns: Option<UnixNanos>,
        callback: Option<TimeEventCallback>,
        allow_past: Option<bool>,
        fire_immediately: Option<bool>,
    ) -> anyhow::Result<()> {
        let ts_now = self.timestamp_ns();
        let (name, start_time_ns, stop_time_ns, fire_immediately, first_event_ns) =
            validate_and_prepare_schedule(
                name,
                schedule.as_ref(),
                stop_time_ns,
                allow_past,
                fire_immediately,
                ts_now,
            )?;

        let first_fire_ns = if fire_immediately {
            // A past anchor is stamped now, as an alert in the past is.
            if start_time_ns > ts_now {
                start_time_ns
            } else {
                ts_now
            }
        } else if first_event_ns < ts_now {
            // `next_after` is strictly after its argument; one nanosecond
            // back makes a deadline landing exactly on now eligible.
            let from = UnixNanos::from(ts_now.as_u64().saturating_sub(1));
            schedule
                .next_after(from, &mut 0)
                .filter(|next| *next > from)
                .ok_or_else(|| {
                    anyhow::anyhow!("Timer '{name}' has no deadline at or after the current time")
                })?
        } else {
            first_event_ns
        };

        ensure!(
            callback.is_some() || self.callbacks.has_any_callback(&name),
            "No callbacks provided"
        );
        self.replace_existing_timer_if_needed(&name);

        let callback = if let Some(callback) = callback {
            self.callbacks.register_callback(name, callback.clone());
            callback
        } else {
            self.callbacks
                .get_callback(&name)
                .expect("callback was validated")
        };

        let mut timer = MogwaiTimer::new(
            MogwaiTimerSpec {
                name,
                cadence: Cadence::Schedule {
                    schedule,
                    first_fire_ns: first_fire_ns.as_u64(),
                },
                start_time_ns,
                stop_time_ns,
                callback,
                fire_immediately,
            },
            self.sim,
            self.sender.clone(),
        );
        timer.start(true);

        self.clear_expired_timers();
        self.timers.insert(name, timer);
        Ok(())
    }

    fn next_time_ns(&self, name: &str) -> Option<UnixNanos> {
        // Filters expired timers like `timer_exists`, and like nautilus
        // `LiveClock`: a fired-and-done timer has no next time, where the
        // bare timer would report its last scheduled one.
        self.timers
            .get(&Ustr::from(name))
            .filter(|timer| !timer.is_expired())
            .map(MogwaiTimer::next_time_ns)
    }

    fn cancel_timer(&mut self, name: &str) {
        if let Some(mut timer) = self.timers.remove(&Ustr::from(name)) {
            timer.cancel();
        }
    }

    fn cancel_timers(&mut self) {
        for timer in self.timers.values_mut() {
            timer.cancel();
        }
        self.timers.clear();
    }

    fn reset(&mut self) {
        self.cancel_timers();
        self.callbacks.clear();
    }
}

#[derive(Debug)]
struct MogwaiTimer {
    name: Ustr,
    cadence: Cadence,
    stop_time_ns: Option<UnixNanos>,
    next_time_ns: Arc<AtomicU64>,
    callback: TimeEventCallback,
    sim: SimClock,
    sender: Option<Arc<dyn TimeEventSender>>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
    canceled: bool,
}

/// How a timer finds the fire after the one it just emitted.
#[derive(Clone, Debug)]
enum Cadence {
    /// A fixed span in nanoseconds (an alert is a one-shot of this shape).
    Fixed(u64),
    /// Deadlines supplied by the schedule. `first_fire_ns` is resolved by
    /// the arming call, which knows sim-now; the timer cannot recompute it.
    Schedule {
        schedule: Arc<dyn TimerSchedule>,
        first_fire_ns: u64,
    },
}

struct MogwaiTimerSpec {
    name: Ustr,
    cadence: Cadence,
    start_time_ns: UnixNanos,
    stop_time_ns: Option<UnixNanos>,
    callback: TimeEventCallback,
    fire_immediately: bool,
}

/// Mirror of nautilus `validate_and_prepare_schedule` (common/src/clock/mod.rs),
/// which is crate-private; only its fixed-interval wrapper
/// `validate_and_prepare_timer` is public, and that cannot take a schedule.
/// The checks and their order are the original's. Returns the interned name,
/// the schedule's start, the stop, the resolved `fire_immediately`, and the
/// first deadline the schedule yields after its start (the start itself when
/// firing immediately), which the caller needs for its arming position.
fn validate_and_prepare_schedule(
    name: &str,
    schedule: &dyn TimerSchedule,
    stop_time_ns: Option<UnixNanos>,
    allow_past: Option<bool>,
    fire_immediately: Option<bool>,
    ts_now: UnixNanos,
) -> anyhow::Result<(Ustr, UnixNanos, Option<UnixNanos>, bool, UnixNanos)> {
    check_valid_string_utf8(name, stringify!(name))?;

    let name = Ustr::from(name);
    let allow_past = allow_past.unwrap_or(true);
    let fire_immediately = fire_immediately.unwrap_or(false);

    // An explicit epoch start stays at the epoch: unlike `set_timer_ns`, zero
    // is not read as "no start".
    let start_time_ns = schedule.start_time_ns();
    let next_event_time = if fire_immediately {
        start_time_ns
    } else {
        schedule
            .next_after(start_time_ns, &mut 0)
            .filter(|next| *next > start_time_ns)
            .ok_or_else(|| {
                anyhow::anyhow!("Timer '{name}' first event time exceeds UnixNanos range")
            })?
    };

    if !allow_past && next_event_time < ts_now {
        anyhow::bail!(
            "Timer '{name}' next event time {} would be in the past (current time is {ts_now})",
            next_event_time.to_rfc3339(),
        );
    }

    if let Some(stop_time) = stop_time_ns {
        if stop_time <= start_time_ns {
            anyhow::bail!(
                "Timer '{name}' stop time {} must be after start time {}",
                stop_time.to_rfc3339(),
                start_time_ns.to_rfc3339(),
            );
        }

        if !allow_past && stop_time <= ts_now {
            anyhow::bail!(
                "Timer '{name}' stop time {} is in the past (current time is {ts_now})",
                stop_time.to_rfc3339(),
            );
        }
    }

    Ok((
        name,
        start_time_ns,
        stop_time_ns,
        fire_immediately,
        next_event_time,
    ))
}

impl MogwaiTimer {
    fn new(spec: MogwaiTimerSpec, sim: SimClock, sender: Option<Arc<dyn TimeEventSender>>) -> Self {
        let (cadence, next_time) = match spec.cadence {
            Cadence::Fixed(interval_ns) => {
                let interval_ns = create_valid_interval(DurationNanos::new(interval_ns)).get();
                let next_time = if spec.fire_immediately {
                    spec.start_time_ns.as_u64()
                } else {
                    spec.start_time_ns.as_u64().saturating_add(interval_ns)
                };
                (Cadence::Fixed(interval_ns), next_time)
            }
            Cadence::Schedule {
                schedule,
                first_fire_ns,
            } => (
                Cadence::Schedule {
                    schedule,
                    first_fire_ns,
                },
                first_fire_ns,
            ),
        };
        Self {
            name: spec.name,
            cadence,
            stop_time_ns: spec.stop_time_ns,
            next_time_ns: Arc::new(AtomicU64::new(next_time)),
            callback: spec.callback,
            sim,
            sender,
            task_handle: None,
            canceled: false,
        }
    }

    fn next_time_ns(&self) -> UnixNanos {
        UnixNanos::from(self.next_time_ns.load(Ordering::SeqCst))
    }

    fn is_expired(&self) -> bool {
        self.canceled
            || self
                .task_handle
                .as_ref()
                .is_some_and(tokio::task::JoinHandle::is_finished)
    }

    fn start(&mut self, repeat: bool) {
        // Restart semantics: a still-active task from a previous `start` must
        // not race the new one over the shared `next_time_ns`. Abort it first
        // (a bare reassignment of `task_handle` only detaches the old task, it
        // does not stop it). Any event that task already handed to the sender
        // stays queued on the runner channel and still dispatches, matching
        // nautilus `LiveTimer::start` (common/src/live/timer.rs).
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }

        let name = self.name;
        let cadence = self.cadence.clone();
        let stop_time_ns = self.stop_time_ns;
        let next_time = Arc::clone(&self.next_time_ns);
        let callback = self.callback.clone();
        let sender = self.sender.clone();
        let sim = self.sim;

        // `spawn_local`, never the multi-threaded runtime: the fire loop must run
        // on the same thread that binds and drains the time-event sender. That
        // is the only way a `TimeEventCallback::RustLocal` (an `Rc`, `!Send`
        // since nautilus PR 4496 removed its unsound `Send` impl) can be
        // dispatched at all - nautilus routes local callbacks through a
        // thread-local registry keyed by owner `ThreadId`, and that machinery
        // (`register_time_event_callback`, `TimeEventCallbackToken`,
        // `TimeEventMessage::registered`) is crate-private, so an out-of-tree
        // adapter cannot cross the worker/drain boundary the way `LiveTimer`
        // does. Running on the owner thread lets `TimeEventMessage::new`
        // register the callback where the runner will later drain it, so its
        // owner-thread check passes (common/src/runner.rs). Bar-aggregation
        // timers register exactly such local callbacks on this clock, so this
        // is load-bearing, not a corner case. This requires the host to run a
        // `LocalSet` on that thread; a nautilus live worker does, and the unit
        // tests below wrap their bodies in one.
        let handle = tokio::task::spawn_local(async move {
            // Past-start catch-up is a deliberate deviation from nautilus
            // `LiveTimer`. `LiveTimer::start` CAS-adjusts an observed next_time
            // that is `<= now` forward to now (with a warning) and then fires
            // once and continues from now (common/src/live/timer.rs). This loop
            // never skips forward: for a timer armed with an explicit past
            // start (allow_past), `sleep_until_sim` returns immediately for each
            // already-elapsed fire, so the timer replays one event per elapsed
            // interval, each carrying its historically-correct sim `ts_event`.
            // On a simulation axis that deterministic replay is more faithful
            // than collapsing the missed fires into a single "now" event, so
            // mogwai keeps the burst by design rather than mirroring the live
            // clock here.
            //
            // A schedule timer replays missed fires in flight the same way;
            // only its arming position skips (see `set_timer_schedule`).
            //
            // The occurrence hint starts at zero on every `start`, which the
            // `TimerSchedule` contract requires every schedule to tolerate.
            let mut schedule_index = 0_u64;
            loop {
                let fire_ns = next_time.load(Ordering::SeqCst);

                // Check-then-fire: never emit an event scheduled past the stop
                // bound. The bound is enforced on the scheduled `ts_event`
                // (inclusive at the stop boundary, so a fire landing exactly on
                // the stop is emitted and then the timer expires), matching
                // nautilus `LiveTimer`'s `should_fire_scheduled_time` gate
                // (common/src/live/timer.rs) and `VirtualTimer`'s property-tested
                // `ts_event <= stop_time_ns` invariant. LiveTimer used to be
                // fire-then-check and could emit one event past its stop; that
                // was reported upstream and fixed, and this mirror follows it.
                if stop_time_ns.is_some_and(|stop| fire_ns > stop.as_u64()) {
                    break;
                }

                sleep_until_sim(sim, fire_ns).await;
                let ts_event = UnixNanos::from(fire_ns);
                let ts_init = sim_now(sim);
                let event = TimeEvent::new(name, UUID4::new(), ts_event, ts_init);

                // Advance the schedule and dispatch. The atomic stores the next
                // fire even when it lies past the stop; the pre-fire check at the
                // top of the loop decides whether that fire is emitted, matching
                // LiveTimer's `next_time_ns` bookkeeping. A one-shot never
                // advances (it breaks below before looping).
                //
                // The advance order follows nautilus `LiveTimer` and matters only
                // for a reentrant callback that re-reads `next_time_ns` or re-arms
                // this timer: the sender path advances before the send, so a
                // synchronous `TimeEventSender` (the trait permits one - the test
                // mock is synchronous) observes the next fire, not the one being
                // emitted; the senderless path advances after the inline invoke,
                // so a reentrant callback there still sees the pre-advance value.
                // Production's async sender queues, so it already observes the
                // advance before the runner drains and dispatches.
                //
                // A schedule can run out: no representable deadline after this
                // one makes it the final event, and the timer ends after
                // dispatching it with `next_time_ns` left at that last fire,
                // as `LiveTimer` leaves an exhausted timer.
                let next = match &cadence {
                    Cadence::Fixed(interval_ns) => Some(fire_ns.saturating_add(*interval_ns)),
                    Cadence::Schedule { schedule, .. } => schedule
                        .next_after(ts_event, &mut schedule_index)
                        .map(u64::from)
                        .filter(|next| *next > fire_ns),
                };
                if let Some(sender) = sender.as_ref() {
                    if let (true, Some(next)) = (repeat, next) {
                        next_time.store(next, Ordering::SeqCst);
                    }
                    // `TimeEventMessage::new` wraps a `Rust`/`Python` callback in
                    // a `Direct` message and registers a `RustLocal` one in the
                    // thread-local registry on this same thread; the runner drains and
                    // runs it back on the same thread, so both dispatch correctly.
                    sender.send(TimeEventMessage::new(event, callback.clone()));
                } else {
                    // No sender bound (a senderless clock off the canonical live
                    // path, e.g. a test that never binds a runner). Invoke inline
                    // on this task's own thread - sound for a local callback too.
                    // This inline path still requires the host `LocalSet` the
                    // `spawn_local` task runs in.
                    callback.call(event);
                    if let (true, Some(next)) = (repeat, next) {
                        next_time.store(next, Ordering::SeqCst);
                    }
                }

                if !repeat || next.is_none() {
                    break;
                }

                // Sharing the runner's single thread (spawn_local), a past-start
                // catch-up burst fires without ever awaiting `sleep_until_sim`,
                // which would monopolize the executor and starve the very drain
                // that dispatches the events just sent. Yield once per fire so
                // the runner interleaves them.
                tokio::task::yield_now().await;
            }
        });
        self.task_handle = Some(handle);
        self.canceled = false;
    }

    fn cancel(&mut self) {
        if let Some(handle) = self.task_handle.take() {
            handle.abort();
        }
        self.canceled = true;
    }
}

impl Drop for MogwaiTimer {
    fn drop(&mut self) {
        self.cancel();
    }
}

async fn sleep_until_sim(sim: SimClock, target_ns: u64) {
    let wall_deadline = sim.wall_ns(target_ns);
    let wall_now = mogwai_protocol::now_unix_nanos();
    if wall_deadline > wall_now {
        tokio::time::sleep(Duration::from_nanos(wall_deadline - wall_now)).await;
    }
}

fn sim_now(sim: SimClock) -> UnixNanos {
    UnixNanos::from(sim.sim_ns(mogwai_protocol::now_unix_nanos()))
}

pub(crate) async fn fetch_clock(http: &HttpClient, http_base: &str) -> anyhow::Result<VenueClock> {
    let response = http
        .get(
            join_url(
                http_base,
                mogwai_protocol::routes::segment(mogwai_protocol::routes::CLOCK),
            ),
            None,
            None,
            Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
            None,
        )
        .await
        .context("fetch clock")?;
    ensure!(
        response.status.is_success(),
        "fetch clock returned {}",
        response.status.as_u16()
    );
    serde_json::from_slice(&response.body).context("decode clock")
}

pub async fn mogwai_clock_factory(
    http_base: &str,
) -> anyhow::Result<impl Fn() -> Rc<RefCell<dyn Clock>> + 'static> {
    let http = HttpClient::builder()
        .timeout_secs(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS)
        .build()
        .context("create HTTP client")?;
    // The node clock only needs the affine map; the tape boundary in the
    // `VenueClock` envelope is for the data client's history guard, not the clock.
    let sim = fetch_clock(&http, http_base).await?.sim;
    Ok(move || {
        // `try_get_time_event_sender()` is re-read on every clock creation (each
        // call of this closure), not frozen when the factory future resolved, so
        // any clock the node builds after the runner binds its senders is wired
        // to the sender. The canonical live path guarantees that ordering:
        // `AsyncRunner::bind_senders()` runs before `NautilusKernel` first
        // invokes `clock_factory.clock()` (live/src/node/builder.rs constructs
        // the runner and binds senders, then builds the kernel, which calls the
        // factory in system/src/kernel.rs), so the thread-local sender is
        // already set for the kernel clock and for every component clock created
        // afterward on the same runner thread. A senderless `MogwaiClock` can
        // therefore only arise off this path (e.g. a unit test that never binds
        // a runner), where the inline-invoke fallback in `MogwaiTimer::start`
        // is the correct behavior.
        //
        // Capturing the sender here (rather than per fire) also pins this clock
        // permanently to the runner channel that was bound when it was created,
        // so it cannot be misrouted if another runner later overwrites the
        // thread-local sender. The timer fire loop runs on this same thread via
        // `spawn_local` (see `MogwaiTimer::start`), so `try_get_time_event_sender()`
        // would in fact be observable at fire time too; capturing once is the
        // deliberate choice, not a workaround for a worker-thread blind spot.
        let clock: Rc<RefCell<dyn Clock>> = Rc::new(RefCell::new(MogwaiClock::new(
            sim,
            try_get_time_event_sender(),
        )));
        clock
    })
}

#[cfg(test)]
mod tests {
    use std::{
        cell::Cell,
        sync::Mutex,
        time::{Duration, Instant},
    };

    use tokio::task::LocalSet;

    use super::*;

    #[derive(Debug)]
    struct CollectingSender {
        events: Arc<Mutex<Vec<TimeEvent>>>,
    }

    impl TimeEventSender for CollectingSender {
        fn send(&self, message: TimeEventMessage) {
            // Dispatch on this same thread exactly as the live runner's drain does:
            // the timer fire loop is a `spawn_local` task on the same thread, so
            // a `RustLocal` message registered at fire time dispatches here with
            // its owner-thread check satisfied. `dispatch` returns false only when
            // it could not run the callback (e.g. a registered message drained off
            // its owner thread), so assert it ran - a silent drop is the RustLocal
            // regression this guards. Record the event only after a successful
            // dispatch, so the recorded order matches the runner's.
            let event = message.event().clone();
            assert!(
                message.dispatch(),
                "time event message failed to dispatch on the owner thread"
            );
            self.events.lock().expect("events lock").push(event);
        }
    }

    // Unlike `CollectingSender`, this only enqueues; the test drains and
    // dispatches from its outer `LocalSet` future. That models nautilus's real
    // producer(fire task)/drain(node-run future) split - both on the one
    // LocalSet thread - which the synchronous `CollectingSender` cannot.
    #[derive(Debug)]
    struct ChannelSender {
        tx: Mutex<std::sync::mpsc::Sender<TimeEventMessage>>,
    }

    impl TimeEventSender for ChannelSender {
        fn send(&self, message: TimeEventMessage) {
            self.tx
                .lock()
                .expect("tx lock")
                .send(message)
                .expect("channel send");
        }
    }

    #[test]
    fn timestamp_ns_reads_affine_sim_time() {
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: 1_000,
            wall_anchor_ns: wall.saturating_sub(10),
            speed: 2.0,
        };
        let clock = MogwaiClock::new(sim, None);

        assert!(clock.timestamp_ns().as_u64() >= 1_020);
    }

    // Each timer test runs its body on a `LocalSet` because `MogwaiTimer::start`
    // now uses `spawn_local` (see its doc comment); the free `spawn_local`
    // panics without a local task set, which a current-thread live worker
    // provides in production but `#[tokio::test]` does not.
    #[tokio::test]
    async fn alert_timer_fires_with_sim_event_timestamp() {
        LocalSet::new()
            .run_until(alert_timer_fires_with_sim_event_timestamp_inner())
            .await;
    }

    async fn alert_timer_fires_with_sim_event_timestamp_inner() {
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 10.0,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        clock.register_default_handler(TimeEventCallback::from(|_| {}));
        // One budget, and it discriminates. This used to arm 20 ms of sim time
        // and hold two contradictory ceilings - a 200 ms one in the poll loop
        // and a 50 ms one after it - neither of which could see the property.
        // At speed 10 the fire is 2 ms of wall, so a timer that ignored `speed`
        // and slept the sim interval raw would land at ~20 ms and pass the
        // tighter of the two. Arming 500 ms of sim instead puts the honest fire
        // at ~50 ms of wall (measured 4-8 ms at the old scale, so the poll's own
        // 1 ms granularity and dev-build slop are the only additions) and the
        // speed-blind failure at ~500 ms. The single ceiling below sits between
        // them, and the loop's own assertion is itself the bound - reaching the
        // end of the loop is what proves it.
        //
        // The improvement is that the bound can see the property at all, not
        // that it is roomier in relative terms: 250 ms over a ~54 ms honest fire
        // is ~4.6x, where the old 50 ms over a 4-8 ms fire was 6-12x. What grew
        // is the absolute slack a scheduler stall has to eat through - 196 ms
        // rather than 42 ms - and that is the trade worth making under an
        // eight-way gate. The relative figure got smaller on purpose.
        let target = clock.timestamp_ns().as_u64() + 500_000_000;
        let speed_bound = Duration::from_millis(250);
        let started = Instant::now();

        clock
            .set_time_alert_ns("once", UnixNanos::from(target), None, None)
            .expect("timer arms");

        while events.lock().expect("events lock").is_empty() {
            assert!(
                started.elapsed() < speed_bound,
                "the alert armed 500ms of SIM time at speed 10, so it owes ~50ms of \
                 wall; {:?} without a fire says the timer slept the sim interval \
                 raw instead of scaling it by `speed`",
                started.elapsed()
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let event = events.lock().expect("events lock")[0].clone();
        assert_eq!(event.ts_event, UnixNanos::from(target));
    }

    #[tokio::test]
    async fn past_alert_adjusted_to_now_fires_immediately() {
        LocalSet::new()
            .run_until(past_alert_adjusted_to_now_fires_immediately_inner())
            .await;
    }

    async fn past_alert_adjusted_to_now_fires_immediately_inner() {
        // A past alert is adjusted to now by validate_and_prepare_time_alert
        // (allow_past defaults to true), making stop == start. The zero
        // interval is coerced to 1ns, so without fire_immediately the only
        // scheduled fire would sit past the stop and the pre-fire stop check
        // would swallow the alert entirely. Pin that the alert fires exactly
        // on the inclusive stop boundary instead, mirroring nautilus
        // LiveClock::set_time_alert_ns.
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 1.0,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        clock.register_default_handler(TimeEventCallback::from(|_| {}));
        let target = clock.timestamp_ns().as_u64().saturating_sub(5_000_000);
        let started = Instant::now();

        clock
            .set_time_alert_ns("immediate", UnixNanos::from(target), None, None)
            .expect("timer arms");

        while events.lock().expect("events lock").is_empty() {
            assert!(
                started.elapsed() < Duration::from_millis(200),
                "the adjusted-to-now alert never fired"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let event = events.lock().expect("events lock")[0].clone();
        assert!(
            event.ts_event.as_u64() >= target,
            "the fire carries the adjusted (now) timestamp, not the past target"
        );
    }

    #[tokio::test]
    async fn cancel_timer_stops_future_events() {
        LocalSet::new()
            .run_until(cancel_timer_stops_future_events_inner())
            .await;
    }

    async fn cancel_timer_stops_future_events_inner() {
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 1.0,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        clock.register_default_handler(TimeEventCallback::from(|_| {}));
        let target = clock.timestamp_ns().as_u64() + 50_000_000;

        clock
            .set_time_alert_ns("cancel", UnixNanos::from(target), None, None)
            .expect("timer arms");
        clock.cancel_timer("cancel");
        tokio::time::sleep(Duration::from_millis(80)).await;

        assert!(events.lock().expect("events lock").is_empty());
    }

    #[tokio::test]
    async fn repeating_timer_never_fires_past_stop_bound() {
        LocalSet::new()
            .run_until(repeating_timer_never_fires_past_stop_bound_inner())
            .await;
    }

    async fn repeating_timer_never_fires_past_stop_bound_inner() {
        // A repeating timer whose stop bound sits below start + interval never
        // fires: the loop checks the stop against the scheduled fire before
        // emitting, so no event carries a ts_event past the stop. Mirrors the
        // fixed nautilus LiveTimer (which used to fire once past stop) and
        // VirtualTimer's `ts_event <= stop_time_ns` invariant. Pin the shape so a
        // regression back to fire-then-check trips this test.
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 10.0,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        clock.register_default_handler(TimeEventCallback::from(|_| {}));
        let now = clock.timestamp_ns().as_u64();
        let interval_ns = 40_000_000; // 40ms sim (4ms wall at speed 10)
        let stop = now + 10_000_000; // 10ms sim: above start, below start + interval

        clock
            .set_timer_ns(
                "past-stop",
                DurationNanos::new(interval_ns),
                None,
                Some(UnixNanos::from(stop)),
                None,
                None,
                None,
            )
            .expect("timer arms");

        // The would-be first fire sits at start + interval (4ms wall at speed
        // 10); wait an order of magnitude past it before asserting silence.
        tokio::time::sleep(Duration::from_millis(40)).await;

        let fired = events.lock().expect("events lock").clone();
        assert!(
            fired.is_empty(),
            "no fire may land past the stop bound, got {fired:?}"
        );
        assert_eq!(
            clock.timer_count(),
            0,
            "the timer expires without ever firing"
        );
    }

    #[tokio::test]
    async fn past_start_repeating_timer_replays_a_catch_up_burst() {
        LocalSet::new()
            .run_until(past_start_repeating_timer_replays_a_catch_up_burst_inner())
            .await;
    }

    async fn past_start_repeating_timer_replays_a_catch_up_burst_inner() {
        // A deliberate deviation from nautilus LiveTimer: a repeating timer
        // armed with an explicit past start replays one fire per elapsed
        // interval, each carrying its historically-correct ts_event, instead of
        // skipping to now and firing once (LiveTimer's CAS-to-now). Pin the
        // catch-up so the deviation stays intentional and tested.
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 1.0,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        clock.register_default_handler(TimeEventCallback::from(|_| {}));
        let now = clock.timestamp_ns().as_u64();
        let interval_ns = 20_000_000; // 20ms
        let start = now - 100_000_000; // 100ms in the past
        let stop = start + 3 * interval_ns; // start + 60ms, still in the past
        let started = Instant::now();

        clock
            .set_timer_ns(
                "catchup",
                DurationNanos::new(interval_ns),
                Some(UnixNanos::from(start)),
                Some(UnixNanos::from(stop)),
                None,
                None,
                None,
            )
            .expect("timer arms");

        let deadline = Instant::now() + Duration::from_millis(500);
        while events.lock().expect("events lock").len() < 3 {
            assert!(
                Instant::now() < deadline,
                "catch-up burst did not replay the elapsed intervals"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // Confirm no fourth fire past the stop bound.
        tokio::time::sleep(Duration::from_millis(30)).await;

        let fired = events.lock().expect("events lock").clone();
        assert_eq!(
            fired.len(),
            3,
            "exactly the three past intervals at or below stop replay (the stop boundary is inclusive)"
        );
        assert_eq!(fired[0].ts_event, UnixNanos::from(start + interval_ns));
        assert_eq!(fired[1].ts_event, UnixNanos::from(start + 2 * interval_ns));
        assert_eq!(fired[2].ts_event, UnixNanos::from(stop));
        // The past fires do not wait wall time, so the burst is near-instant
        // rather than paced one interval apart - the catch-up, not a skip-to-now.
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "past fires replay as a burst, not paced by the interval"
        );
    }

    #[tokio::test]
    async fn timer_exists_excludes_a_fired_and_done_timer_like_count_and_names() {
        LocalSet::new()
            .run_until(timer_exists_excludes_a_fired_and_done_timer_like_count_and_names_inner())
            .await;
    }

    async fn timer_exists_excludes_a_fired_and_done_timer_like_count_and_names_inner() {
        // All three introspection surfaces agree: a fired-and-finished one-shot
        // no longer exists, is not counted, and is not named. Mirrors the fixed
        // nautilus LiveClock (whose timer_exists used to be a bare contains_key
        // while the other two filtered expired timers). Pin the consistency.
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 10.0,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        clock.register_default_handler(TimeEventCallback::from(|_| {}));
        let target = clock.timestamp_ns().as_u64() + 20_000_000; // 20ms sim (2ms wall)

        clock
            .set_time_alert_ns("oneshot", UnixNanos::from(target), None, None)
            .expect("timer arms");

        // Poll until the fired one-shot's task finishes and the count reflects
        // its expiry, then assert timer_exists agrees.
        let deadline = Instant::now() + Duration::from_millis(500);
        while clock.timer_count() != 0 {
            assert!(
                Instant::now() < deadline,
                "the expired one-shot was never dropped from timer_count"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        let name = Ustr::from("oneshot");
        assert!(
            !clock.timer_exists(&name),
            "timer_exists excludes the fired one-shot, consistent with the other surfaces"
        );
        assert!(
            clock.timer_names().is_empty(),
            "timer_names excludes the expired timer"
        );
    }

    #[tokio::test]
    async fn local_callback_dispatches_through_sender_on_owner_thread() {
        LocalSet::new()
            .run_until(local_callback_dispatches_through_sender_on_owner_thread_inner())
            .await;
    }

    async fn local_callback_dispatches_through_sender_on_owner_thread_inner() {
        // Regression guard for nautilus PR 4496. A `RustLocal` callback is an
        // `Rc`, `!Send`, so it cannot cross into a runtime worker; the fire loop
        // therefore runs on the owner thread (`spawn_local`), where
        // `TimeEventMessage::new` registers it in the thread-local registry and
        // the sender drains and dispatches it on the same thread, satisfying the
        // owner-thread check. Bar aggregation registers exactly this callback
        // kind. Before the fix the event was dropped and this counter stayed 0.
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 10.0,
        };
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        let count = Rc::new(Cell::new(0_u32));
        let cb_count = Rc::clone(&count);
        let callback: Rc<dyn Fn(TimeEvent)> = Rc::new(move |_| cb_count.set(cb_count.get() + 1));
        let target = clock.timestamp_ns().as_u64() + 20_000_000;
        let started = Instant::now();

        clock
            .set_time_alert_ns(
                "local",
                UnixNanos::from(target),
                Some(TimeEventCallback::RustLocal(callback)),
                None,
            )
            .expect("timer arms");

        while events.lock().expect("events lock").is_empty() {
            assert!(
                started.elapsed() < Duration::from_millis(200),
                "the local-callback alert never fired"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        assert_eq!(
            count.get(),
            1,
            "the RustLocal callback ran on the owner thread instead of being dropped"
        );
    }

    #[tokio::test]
    async fn local_callback_dispatches_across_the_queue_drain_boundary() {
        LocalSet::new()
            .run_until(local_callback_dispatches_across_the_queue_drain_boundary_inner())
            .await;
    }

    async fn local_callback_dispatches_across_the_queue_drain_boundary_inner() {
        // Stronger than the synchronous-mock test: the sender only enqueues, and
        // the drain happens here in the outer future - modeling the real
        // producer(fire task)/drain(node-run future) split. Both run on the one
        // LocalSet thread, so a RustLocal message registered at fire time still
        // dispatches here with its owner-thread check satisfied.
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 10.0,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let sender = Arc::new(ChannelSender { tx: Mutex::new(tx) });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        let count = Rc::new(Cell::new(0_u32));
        let cb_count = Rc::clone(&count);
        let callback: Rc<dyn Fn(TimeEvent)> = Rc::new(move |_| cb_count.set(cb_count.get() + 1));
        let target = clock.timestamp_ns().as_u64() + 20_000_000;

        clock
            .set_time_alert_ns(
                "queued-local",
                UnixNanos::from(target),
                Some(TimeEventCallback::RustLocal(callback)),
                None,
            )
            .expect("timer arms");

        let started = Instant::now();
        let message = loop {
            if let Ok(message) = rx.try_recv() {
                break message;
            }
            assert!(
                started.elapsed() < Duration::from_millis(200),
                "the fire task never queued a message"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        };

        assert!(
            message.dispatch(),
            "the queued local message dispatches on its owner thread"
        );
        assert_eq!(count.get(), 1);
    }

    #[tokio::test]
    async fn queued_local_message_dispatches_after_cancel() {
        LocalSet::new()
            .run_until(queued_local_message_dispatches_after_cancel_inner())
            .await;
    }

    async fn queued_local_message_dispatches_after_cancel_inner() {
        // Canceling a timer aborts its fire task but must not strip the lease from
        // a message already queued: nautilus keeps queued events dispatchable
        // across cancel. Receive the queued local message, cancel, then dispatch
        // and assert the callback still runs.
        let wall = mogwai_protocol::now_unix_nanos();
        let sim = SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 10.0,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let sender = Arc::new(ChannelSender { tx: Mutex::new(tx) });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        let count = Rc::new(Cell::new(0_u32));
        let cb_count = Rc::clone(&count);
        let callback: Rc<dyn Fn(TimeEvent)> = Rc::new(move |_| cb_count.set(cb_count.get() + 1));
        let target = clock.timestamp_ns().as_u64() + 20_000_000;

        clock
            .set_time_alert_ns(
                "cancel-queued",
                UnixNanos::from(target),
                Some(TimeEventCallback::RustLocal(callback)),
                None,
            )
            .expect("timer arms");

        let started = Instant::now();
        let message = loop {
            if let Ok(message) = rx.try_recv() {
                break message;
            }
            assert!(
                started.elapsed() < Duration::from_millis(200),
                "the fire task never queued a message"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        };

        clock.cancel_timer("cancel-queued");

        assert!(
            message.dispatch(),
            "a message queued before cancel still dispatches"
        );
        assert_eq!(count.get(), 1);
    }

    // Deadlines at fixed absolute instants, deliberately not arithmetic: a
    // timer that advanced by a constant instead of asking the schedule would
    // land off this list.
    #[derive(Debug)]
    struct ListSchedule {
        start_ns: u64,
        deadlines: Vec<u64>,
    }

    impl TimerSchedule for ListSchedule {
        fn start_time_ns(&self) -> UnixNanos {
            UnixNanos::from(self.start_ns)
        }

        fn next_after(&self, time_ns: UnixNanos, _index: &mut u64) -> Option<UnixNanos> {
            self.deadlines
                .iter()
                .copied()
                .find(|deadline| *deadline > time_ns.as_u64())
                .map(UnixNanos::from)
        }
    }

    // An arithmetic grid anchored at `start_ns`, for the past-anchor cases.
    #[derive(Debug)]
    struct GridSchedule {
        start_ns: u64,
        step_ns: u64,
    }

    impl TimerSchedule for GridSchedule {
        fn start_time_ns(&self) -> UnixNanos {
            UnixNanos::from(self.start_ns)
        }

        fn next_after(&self, time_ns: UnixNanos, _index: &mut u64) -> Option<UnixNanos> {
            let t = time_ns.as_u64();
            let next = if t < self.start_ns {
                self.start_ns
            } else {
                self.start_ns + ((t - self.start_ns) / self.step_ns + 1) * self.step_ns
            };
            Some(UnixNanos::from(next))
        }
    }

    fn unit_sim() -> SimClock {
        let wall = mogwai_protocol::now_unix_nanos();
        SimClock {
            sim_epoch_ns: wall,
            wall_anchor_ns: wall,
            speed: 1.0,
        }
    }

    fn collecting_clock(sim: SimClock) -> (MogwaiClock, Arc<Mutex<Vec<TimeEvent>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let sender = Arc::new(CollectingSender {
            events: Arc::clone(&events),
        });
        let mut clock = MogwaiClock::new(sim, Some(sender));
        clock.register_default_handler(TimeEventCallback::from(|_| {}));
        (clock, events)
    }

    #[tokio::test]
    async fn schedule_timer_fires_on_the_schedules_deadlines_and_ends_when_exhausted() {
        LocalSet::new()
            .run_until(
                schedule_timer_fires_on_the_schedules_deadlines_and_ends_when_exhausted_inner(),
            )
            .await;
    }

    async fn schedule_timer_fires_on_the_schedules_deadlines_and_ends_when_exhausted_inner() {
        // The timer asks the schedule for every deadline after the first, so an
        // irregular list comes out exactly, and a schedule with nothing left
        // ends the timer after its final event with no stop bound involved.
        let (mut clock, events) = collecting_clock(unit_sim());
        let start = clock.timestamp_ns().as_u64();
        let deadlines = vec![start + 50_000_000, start + 55_000_000, start + 80_000_000];
        let schedule = Arc::new(ListSchedule {
            start_ns: start,
            deadlines: deadlines.clone(),
        });

        clock
            .set_timer_schedule("list", schedule, None, None, None, None)
            .expect("schedule timer arms");
        assert_eq!(
            clock.next_time_ns("list"),
            Some(UnixNanos::from(deadlines[0])),
            "the first fire is the schedule's first deadline after its start"
        );

        let deadline = Instant::now() + Duration::from_millis(500);
        while clock.timer_count() != 0 {
            assert!(
                Instant::now() < deadline,
                "the exhausted schedule timer never ended"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        let fired: Vec<u64> = events
            .lock()
            .expect("events lock")
            .iter()
            .map(|event| event.ts_event.as_u64())
            .collect();
        assert_eq!(fired, deadlines);
    }

    #[tokio::test]
    async fn schedule_timer_stop_bound_is_inclusive() {
        LocalSet::new()
            .run_until(schedule_timer_stop_bound_is_inclusive_inner())
            .await;
    }

    async fn schedule_timer_stop_bound_is_inclusive_inner() {
        let (mut clock, events) = collecting_clock(unit_sim());
        let start = clock.timestamp_ns().as_u64();
        let schedule = Arc::new(GridSchedule {
            start_ns: start,
            step_ns: 50_000_000,
        });
        let stop = start + 150_000_000;

        clock
            .set_timer_schedule(
                "grid",
                schedule,
                Some(UnixNanos::from(stop)),
                None,
                None,
                None,
            )
            .expect("schedule timer arms");

        let deadline = Instant::now() + Duration::from_millis(500);
        while clock.timer_count() != 0 {
            assert!(
                Instant::now() < deadline,
                "the schedule timer never reached its stop bound"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        let fired: Vec<u64> = events
            .lock()
            .expect("events lock")
            .iter()
            .map(|event| event.ts_event.as_u64())
            .collect();
        assert_eq!(
            fired,
            vec![start + 50_000_000, start + 100_000_000, stop],
            "the grid fires up to and including the stop, and nothing past it"
        );
    }

    #[tokio::test]
    async fn schedule_timer_arms_after_a_past_anchor_instead_of_replaying() {
        LocalSet::new()
            .run_until(schedule_timer_arms_after_a_past_anchor_instead_of_replaying_inner())
            .await;
    }

    async fn schedule_timer_arms_after_a_past_anchor_instead_of_replaying_inner() {
        // The schedule's start is an anchor, not a replay request: a grid
        // anchored a second back must not emit the fifty elapsed deadlines at
        // arming. The first event lands on the grid at or after arming time.
        let (mut clock, events) = collecting_clock(unit_sim());
        let armed = clock.timestamp_ns().as_u64();
        let step = 20_000_000;
        let schedule = Arc::new(GridSchedule {
            start_ns: armed - 1_000_000_000,
            step_ns: step,
        });

        clock
            .set_timer_schedule("anchored", schedule, None, None, None, None)
            .expect("schedule timer arms");

        let deadline = Instant::now() + Duration::from_millis(500);
        while events.lock().expect("events lock").is_empty() {
            assert!(Instant::now() < deadline, "the anchored timer never fired");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        let first = events.lock().expect("events lock")[0].ts_event.as_u64();
        assert!(
            first >= armed,
            "the first fire is at or after arming, got {first} against {armed}: \
             the elapsed grid was replayed"
        );
        assert!(
            first < armed + 2 * step,
            "the first fire is the next grid point, not a later one"
        );
        assert_eq!(
            (first - (armed - 1_000_000_000)) % step,
            0,
            "the fire sits on the schedule's grid"
        );
    }

    #[tokio::test]
    async fn schedule_timer_fire_immediately_fires_at_now_for_a_past_anchor() {
        LocalSet::new()
            .run_until(schedule_timer_fire_immediately_fires_at_now_for_a_past_anchor_inner())
            .await;
    }

    async fn schedule_timer_fire_immediately_fires_at_now_for_a_past_anchor_inner() {
        let (mut clock, events) = collecting_clock(unit_sim());
        let armed = clock.timestamp_ns().as_u64();
        let schedule = Arc::new(GridSchedule {
            start_ns: armed - 1_000_000_000,
            step_ns: 20_000_000,
        });

        clock
            .set_timer_schedule("now", schedule, None, None, None, Some(true))
            .expect("schedule timer arms");

        let deadline = Instant::now() + Duration::from_millis(500);
        while events.lock().expect("events lock").is_empty() {
            assert!(Instant::now() < deadline, "the immediate timer never fired");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let first = events.lock().expect("events lock")[0].ts_event.as_u64();
        assert!(
            first >= armed,
            "the immediate fire is stamped now, not the stale anchor, got {first} against {armed}"
        );
    }

    #[tokio::test]
    async fn schedule_timer_rejects_what_the_nautilus_validation_rejects() {
        LocalSet::new()
            .run_until(schedule_timer_rejects_what_the_nautilus_validation_rejects_inner())
            .await;
    }

    async fn schedule_timer_rejects_what_the_nautilus_validation_rejects_inner() {
        let (mut clock, _events) = collecting_clock(unit_sim());
        let now = clock.timestamp_ns().as_u64();
        let grid = |start_ns| {
            Arc::new(GridSchedule {
                start_ns,
                step_ns: 1_000_000_000,
            })
        };

        // First deadline in the past with past times disallowed.
        let err = clock
            .set_timer_schedule(
                "past",
                grid(now - 10_000_000_000),
                None,
                None,
                Some(false),
                None,
            )
            .expect_err("a past first deadline is refused when allow_past is false");
        assert!(err.to_string().contains("would be in the past"), "{err}");

        // Stop not after start.
        let err = clock
            .set_timer_schedule(
                "stop",
                grid(now + 1_000_000_000),
                Some(UnixNanos::from(now + 1_000_000_000)),
                None,
                None,
                None,
            )
            .expect_err("a stop at the start is refused");
        assert!(
            err.to_string().contains("must be after start time"),
            "{err}"
        );

        // Stop in the past with past times disallowed.
        // The first deadline (start + 10s) is in the future, so only the stop
        // check can refuse this one.
        let sparse = Arc::new(GridSchedule {
            start_ns: now - 1_000_000_000,
            step_ns: 10_000_000_000,
        });
        let err = clock
            .set_timer_schedule(
                "stop-past",
                sparse,
                Some(UnixNanos::from(now - 500_000_000)),
                None,
                Some(false),
                None,
            )
            .expect_err("a past stop is refused when allow_past is false");
        assert!(err.to_string().contains("stop time"), "{err}");

        // A schedule with no deadline after its start.
        let empty = Arc::new(ListSchedule {
            start_ns: now,
            deadlines: Vec::new(),
        });
        let err = clock
            .set_timer_schedule("empty", empty, None, None, None, None)
            .expect_err("a schedule with no first deadline is refused");
        assert!(err.to_string().contains("exceeds UnixNanos range"), "{err}");

        assert_eq!(clock.timer_count(), 0, "no refused call leaves a timer");
    }

    #[tokio::test]
    async fn schedule_timer_without_a_callback_keeps_the_timer_it_would_replace() {
        LocalSet::new()
            .run_until(schedule_timer_without_a_callback_keeps_the_timer_it_would_replace_inner())
            .await;
    }

    async fn schedule_timer_without_a_callback_keeps_the_timer_it_would_replace_inner() {
        let mut clock = MogwaiClock::new(unit_sim(), None);
        let now = clock.timestamp_ns().as_u64();
        let schedule = || {
            Arc::new(GridSchedule {
                start_ns: now,
                step_ns: 60_000_000_000,
            })
        };
        clock
            .set_timer_schedule(
                "keep",
                schedule(),
                None,
                Some(TimeEventCallback::from(|_| {})),
                None,
                None,
            )
            .expect("schedule timer arms");
        let armed_for = clock.next_time_ns("keep");
        clock.cancel_callbacks();

        let err = clock
            .set_timer_schedule("keep", schedule(), None, None, None, None)
            .expect_err("no callback is registered for the name or by default");

        assert!(err.to_string().contains("No callbacks provided"), "{err}");
        assert!(clock.timer_exists(&Ustr::from("keep")));
        assert_eq!(clock.next_time_ns("keep"), armed_for);
    }

    #[tokio::test]
    async fn next_time_ns_is_none_for_a_fired_and_done_timer() {
        LocalSet::new()
            .run_until(next_time_ns_is_none_for_a_fired_and_done_timer_inner())
            .await;
    }

    async fn next_time_ns_is_none_for_a_fired_and_done_timer_inner() {
        // `next_time_ns` documents "the active timer": once a one-shot has
        // fired and its task finished, it reports nothing, in line with
        // `timer_exists`, `timer_count` and nautilus `LiveClock`.
        let (mut clock, _events) = collecting_clock(unit_sim());
        let target = clock.timestamp_ns().as_u64() + 20_000_000;

        clock
            .set_time_alert_ns("oneshot", UnixNanos::from(target), None, None)
            .expect("timer arms");
        assert_eq!(
            clock.next_time_ns("oneshot"),
            Some(UnixNanos::from(target)),
            "an active timer reports its next fire"
        );

        let deadline = Instant::now() + Duration::from_millis(500);
        while clock.timer_count() != 0 {
            assert!(Instant::now() < deadline, "the one-shot never finished");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }

        assert_eq!(clock.next_time_ns("oneshot"), None);
    }

    #[tokio::test]
    async fn alert_without_a_callback_keeps_the_timer_it_would_replace() {
        LocalSet::new()
            .run_until(alert_without_a_callback_keeps_the_timer_it_would_replace_inner())
            .await;
    }

    async fn alert_without_a_callback_keeps_the_timer_it_would_replace_inner() {
        let mut clock = MogwaiClock::new(unit_sim(), None);
        let target = clock.timestamp_ns().as_u64() + 60_000_000_000;
        clock
            .set_time_alert_ns(
                "keep",
                UnixNanos::from(target),
                Some(TimeEventCallback::from(|_| {})),
                None,
            )
            .expect("timer arms");
        // Drops the named callback just registered; no default exists, so the
        // next call has nothing to run.
        clock.cancel_callbacks();

        let err = clock
            .set_time_alert_ns("keep", UnixNanos::from(target + 1), None, None)
            .expect_err("no callback is registered for the name or by default");

        assert!(err.to_string().contains("No callbacks provided"), "{err}");
        assert!(
            clock.timer_exists(&Ustr::from("keep")),
            "the failed call must not have cancelled the running alert"
        );
        assert_eq!(clock.next_time_ns("keep"), Some(UnixNanos::from(target)));
    }
}
