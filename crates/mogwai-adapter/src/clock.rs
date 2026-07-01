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
        self.timers.contains_key(name)
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
                fire_immediately: false,
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
            loop {
                let fire_ns = next_time.load(Ordering::SeqCst);
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

                let next = fire_ns.saturating_add(interval_ns);
                next_time.store(next, Ordering::SeqCst);
                if stop_time_ns.is_some_and(|stop| next >= stop.as_u64()) {
                    break;
                }
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
}
