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
use mogwai_protocol::{ServerClock, SimClock};
use nautilus_common::{
    clock::{CallbackRegistry, Clock, validate_and_prepare_time_alert, validate_and_prepare_timer},
    live::get_runtime,
    runner::{TimeEventSender, try_get_time_event_sender},
    timer::{TimeEvent, TimeEventCallback, TimeEventHandler, create_valid_interval},
};
use nautilus_core::{UUID4, UnixNanos};
use nautilus_network::http::HttpClient;
use ustr::Ustr;

use crate::client::join_url;

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
        self.timestamp_ns().as_f64() / 1_000_000_000.0
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

    fn get_handler(&self, event: TimeEvent) -> TimeEventHandler {
        self.callbacks.get_handler(event)
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

        self.replace_existing_timer_if_needed(&name);
        ensure!(
            callback.is_some() || self.callbacks.has_any_callback(&name),
            "No callbacks provided"
        );

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
                interval_ns: alert_time_ns.as_u64().saturating_sub(ts_now.as_u64()),
                start_time_ns: ts_now,
                stop_time_ns: Some(alert_time_ns),
                callback,
                // An alert exactly at (or adjusted-to) now has a zero interval
                // that `create_valid_interval` coerces to 1ns, which would push
                // the scheduled fire PAST the stop bound and the pre-fire stop
                // check would swallow the alert. Fire immediately instead so
                // the event lands exactly ON the inclusive stop boundary,
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
        interval_ns: u64,
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
                interval_ns,
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
        self.timers
            .get(&Ustr::from(name))
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
    interval_ns: u64,
    stop_time_ns: Option<UnixNanos>,
    next_time_ns: Arc<AtomicU64>,
    callback: TimeEventCallback,
    sim: SimClock,
    sender: Option<Arc<dyn TimeEventSender>>,
    task_handle: Option<tokio::task::JoinHandle<()>>,
    canceled: bool,
}

struct MogwaiTimerSpec {
    name: Ustr,
    interval_ns: u64,
    start_time_ns: UnixNanos,
    stop_time_ns: Option<UnixNanos>,
    callback: TimeEventCallback,
    fire_immediately: bool,
}

impl MogwaiTimer {
    fn new(spec: MogwaiTimerSpec, sim: SimClock, sender: Option<Arc<dyn TimeEventSender>>) -> Self {
        let interval_ns = create_valid_interval(spec.interval_ns).get();
        let next_time = if spec.fire_immediately {
            spec.start_time_ns.as_u64()
        } else {
            spec.start_time_ns.as_u64().saturating_add(interval_ns)
        };
        Self {
            name: spec.name,
            interval_ns,
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
        let name = self.name;
        let interval_ns = self.interval_ns;
        let stop_time_ns = self.stop_time_ns;
        let next_time = Arc::clone(&self.next_time_ns);
        let callback = self.callback.clone();
        let sender = self.sender.clone();
        let sim = self.sim;

        let handle = get_runtime().spawn(async move {
            // Past-start catch-up (AE15) is a DELIBERATE deviation from nautilus
            // `LiveTimer`. `LiveTimer::start` CAS-adjusts an observed next_time
            // that is `<= now` forward to now (with a warning) and then fires
            // once and continues from now (common/src/live/timer.rs). This loop
            // does NOT skip forward: for a timer armed with an explicit past
            // start (allow_past), `sleep_until_sim` returns immediately for each
            // already-elapsed fire, so the timer replays one event per elapsed
            // interval as fast as the runtime schedules it, each carrying its
            // historically-correct sim `ts_event`. On a simulation axis that
            // deterministic replay is more faithful than collapsing the missed
            // fires into a single "now" event, so mogwai keeps the burst by
            // design rather than mirroring the live clock here.
            loop {
                let fire_ns = next_time.load(Ordering::SeqCst);

                // Check-then-fire: never emit an event scheduled past the stop
                // bound. The bound is enforced on the scheduled `ts_event`
                // (inclusive at the stop boundary, so a fire landing exactly ON
                // the stop is emitted and then the timer expires), matching
                // nautilus `LiveTimer`'s `should_fire_scheduled_time` gate
                // (common/src/live/timer.rs) and `TestTimer`'s property-tested
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

                if let Some(sender) = sender.as_ref() {
                    sender.send(TimeEventHandler::new(event, callback.clone()));
                } else if callback.is_local() {
                    // A `RustLocal` callback wraps an `Rc`, sound to create,
                    // clone, drop, and invoke only from its originating
                    // thread (nautilus_common::timer's own doc comment on
                    // `TimeEventCallback::is_local`). This loop runs inside a
                    // spawned tokio task, which the multi-threaded runtime is
                    // free to schedule onto any worker thread, so without a
                    // sender to hand the event back to the originating
                    // thread there is no safe way to invoke a local callback
                    // here. Drop it with a warning rather than risk a
                    // cross-thread `Rc` race.
                    tracing::warn!(
                        timer = %name,
                        "dropping time event: no time event sender and callback is thread-local"
                    );
                } else {
                    callback.call(event);
                }

                if !repeat {
                    break;
                }

                // Advance to the next scheduled fire; the pre-fire stop check
                // at the top of the loop decides whether it is emitted. The
                // atomic stores the advanced value even when it lies past the
                // stop, matching LiveTimer's `next_time_ns` bookkeeping.
                let next = fire_ns.saturating_add(interval_ns);
                next_time.store(next, Ordering::SeqCst);
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

pub(crate) async fn fetch_clock(http: &HttpClient, http_base: &str) -> anyhow::Result<ServerClock> {
    let response = http
        .get(
            join_url(http_base, "clock"),
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
    let http = HttpClient::new(
        std::collections::HashMap::new(),
        Vec::new(),
        Vec::new(),
        None,
        Some(mogwai_protocol::DEFAULT_REQUEST_TIMEOUT_SECS),
        None,
    )
    .context("create HTTP client")?;
    // The node clock only needs the affine map; the tape boundary in the
    // `ServerClock` envelope is for the data client's warmup guard, not the clock.
    let sim = fetch_clock(&http, http_base).await?.sim;
    Ok(move || {
        // `try_get_time_event_sender()` is re-read on EVERY clock creation (each
        // call of this closure), not frozen when the factory future resolved, so
        // any clock the node builds after the runner binds its senders is wired
        // to the sender. The canonical live path guarantees that ordering:
        // `AsyncRunner::bind_senders()` runs BEFORE `NautilusKernel` first
        // invokes `clock_factory.clock()` (live/src/node/builder.rs constructs
        // the runner and binds senders, then builds the kernel, which calls the
        // factory in system/src/kernel.rs), so the thread-local sender is
        // already set for the kernel clock and for every component clock created
        // afterward on the same runner thread. A senderless `MogwaiClock` can
        // therefore only arise OFF this path (e.g. a unit test that never binds
        // a runner), where the drop-with-warn fallback in `MogwaiTimer::start`
        // is the correct behavior - AE16.
        //
        // A per-FIRE re-lookup (re-querying the sender each time a timer fires
        // rather than at clock creation) is NOT a viable "improvement":
        // `TIME_EVENT_SENDER` is a thread-local bound on the runner thread,
        // while the timer loop runs inside a spawned tokio task the
        // multi-threaded runtime may place on any worker thread, where
        // `try_get_time_event_sender()` is always `None`. Capturing the sender
        // here, on the thread that constructs the clock, is the only point at
        // which the thread-local is observable at all.
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
        sync::Mutex,
        time::{Duration, Instant},
    };

    use super::*;

    #[derive(Debug)]
    struct CollectingSender {
        events: Arc<Mutex<Vec<TimeEvent>>>,
    }

    impl TimeEventSender for CollectingSender {
        fn send(&self, handler: TimeEventHandler) {
            let TimeEventHandler { event, callback } = handler;
            callback.call(event.clone());
            self.events.lock().expect("events lock").push(event);
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

    #[tokio::test]
    async fn alert_timer_fires_with_sim_event_timestamp() {
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
        let target = clock.timestamp_ns().as_u64() + 20_000_000;
        let started = Instant::now();

        clock
            .set_time_alert_ns("once", UnixNanos::from(target), None, None)
            .expect("timer arms");

        while events.lock().expect("events lock").is_empty() {
            assert!(started.elapsed() < Duration::from_millis(200));
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        let event = events.lock().expect("events lock")[0].clone();
        assert_eq!(event.ts_event, UnixNanos::from(target));
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[tokio::test]
    async fn past_alert_adjusted_to_now_fires_immediately() {
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
        // A repeating timer whose stop bound sits BELOW start + interval never
        // fires: the loop checks the stop against the scheduled fire BEFORE
        // emitting, so no event carries a ts_event past the stop. Mirrors the
        // fixed nautilus LiveTimer (which used to fire once past stop) and
        // TestTimer's `ts_event <= stop_time_ns` invariant. Pin the shape so a
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
                interval_ns,
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
        // AE15 (deliberate deviation from nautilus LiveTimer): a repeating timer
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
                interval_ns,
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
}
