// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Launcher-side harness for the venue lifecycle gates.
//!
//! Every test in `tests/` drives the venue through `mogwai_protocol::launch`,
//! the shipped launcher - the same code path a consumer uses, rather than a
//! second hand-rolled implementation of the same contract.
//!
//! That is deliberate and is the reason the launcher lives in `mogwai-protocol`
//! rather than in `mogwai-adapter`: the venue's own gates cannot depend on the
//! adapter (it would pull nautilus into the venue's test graph, and the adapter
//! is the consumer of the thing under test), so a launcher shipped from there
//! would leave mogwai re-deriving the contract by hand forever. Here, a change
//! to the handshake breaks these tests immediately instead of breaking a
//! consumer later.

// A shared test-support module: not every item is used by every test binary,
// and nothing here is reachable outside the crate.
#![allow(dead_code, unreachable_pub)]

use std::{
    cell::Cell,
    ffi::OsString,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use mogwai_protocol::{
    ReadyRecord,
    launch::{LaunchSpec, LaunchedVenue, StderrSink, VenueExit, launch},
};

/// The per-test hang watchdog every lane in this workspace runs under. A test
/// that crosses it is blamed by name, its process group killed, and the sweep
/// failed - so whatever the test itself was about to say is never said.
///
/// It is stated here because it is the ceiling on every deadline below it, and
/// because the deadlines in these files were routinely set above it: a 30 s
/// bound inside a 20 s watchdog is not a bound at all, it is decoration, and the
/// failure it was written to report arrives as an unattributed kill instead of
/// as the message its author wrote. That is the wrong-answer class these gates
/// exist to remove, reintroduced by the harness that runs them.
pub const HANG_WATCHDOG: Duration = Duration::from_secs(20);

/// What is left to the panic, the unwind and the venue teardowns that run after
/// a deadline fires. A failure that reports and is then killed is no better than
/// one that was only killed, so the budget below stops this far short of the
/// watchdog.
const WATCHDOG_HEADROOM: Duration = Duration::from_secs(4);

/// The wall one test may spend and still fail through its own deadline.
///
/// Derived from [`HANG_WATCHDOG`] rather than written down beside it, because
/// two independent constants stating one relationship drift and nothing detects
/// it. The relationship is the whole point: every deadline this module hands out
/// has to fire before the watchdog does.
///
/// Every deadline handed out by [`deadline`] and [`wall_deadline`] is clamped to
/// it, so a test with several sequential phases cannot overrun by summing
/// generous per-phase bounds. The slowest gate in these files measures under
/// 10 s, so this is not tight.
pub const TEST_WALL_BUDGET: Duration =
    Duration::from_secs(HANG_WATCHDOG.as_secs() - WATCHDOG_HEADROOM.as_secs());

/// The slice of [`TEST_WALL_BUDGET`] no ordinary phase may spend, held back for
/// the teardown waits that run last.
///
/// Sequential phases share one budget, so without a reservation the last phase
/// absorbs every shortfall the phases before it left: a drain that legitimately
/// ran long would leave `wait_for_exit` a fraction of a second and the test
/// would then report "the venue did not exit", which is a false statement about
/// the venue produced by the phase before it. [`wall_deadline`] clamps to the
/// budget minus this reserve; [`teardown_deadline`] clamps to the whole budget,
/// so the wait that reports on the venue's exit always has this much wall of its
/// own.
const TEARDOWN_RESERVE: Duration = Duration::from_secs(3);

thread_local! {
    /// When the current test started spending its budget.
    ///
    /// A thread-local is not what makes this per-test, and the reset below is
    /// not belt-and-braces. Libtest's per-test thread is an implementation
    /// detail that has changed: it once ran a test inline on the calling thread
    /// when `concurrency == 1`, and `--test-threads=1` is exactly what this
    /// workspace's focused runner always passes - the only sanctioned way to
    /// run these `#[ignore]`d gates. Today's libtest spawns a named thread per test
    /// regardless of the concurrency level - measured on this tree, with the
    /// reset disabled, every test still read a fresh anchor - so the sharing a
    /// 2026-08-18 review predicted does not happen here, today. It would be a
    /// silent, wholesale suite failure if it came back: a stale anchor puts the
    /// ceiling in the past, every bound fires on its first poll, and each test
    /// then reports whatever its own bound was named for. Nothing detects that,
    /// so the budget does not rest on the detail.
    ///
    /// [`Venue`]'s `Drop` therefore clears the anchor when the last venue goes,
    /// and [`spawn`] sets it when it finds none. Note the asymmetry, which
    /// [`open_budget`] argues in full: the clear is what makes a test boundary,
    /// and the set never moves an anchor that is already running.
    static BUDGET_ANCHOR: Cell<Option<Instant>> = const { Cell::new(None) };
    /// Venues alive on this thread. A 0 here means no test is mid-flight, which
    /// is what makes the anchor reset above a test boundary rather than a guess.
    /// No test in these files drops a venue and then carries on, which is what
    /// keeps a mid-test reset - which would push the ceiling past the watchdog -
    /// out of reach.
    static LIVE_VENUES: Cell<usize> = const { Cell::new(0) };
}

/// The instant this test's budget started, set on the first launch or the first
/// deadline taken - whichever comes first.
fn budget_anchor() -> Instant {
    BUDGET_ANCHOR.with(|cell| match cell.get() {
        Some(at) => at,
        None => {
            let now = Instant::now();
            cell.set(Some(now));
            now
        }
    })
}

/// Begins a test's budget if none is running. Called by [`spawn`] before it
/// launches, so the anchor is the launching test's own start rather than an
/// earlier test's.
///
/// It only ever sets an absent anchor, and never moves one forward. Moving one
/// forward is the failure this whole mechanism exists to prevent, wearing the
/// costume of the reset that prevents it: a re-anchor mid-test puts the ceiling
/// past [`HANG_WATCHDOG`], so the bound that should have reported arrives as an
/// unattributed kill instead. The carry-forward's rule - "a test must not drop a
/// venue and then carry on" - covered only the tracked case, and there is an
/// untracked one it cannot see: `a_faulted_venue_exits_nonzero_and_an_exhausted_
/// one_does_not` drives two venues through `launch` directly, so [`LIVE_VENUES`]
/// never leaves zero while up to ten seconds of budget are spent, and the
/// `spawn` that follows would have re-anchored onto a test already most of the
/// way through its watchdog. Bookkeeping that counts only what it was told about
/// cannot be the sole guard; refusing to move the anchor at all does not need to
/// be told.
///
/// What this gives up is the cross-test reset for a test that takes a deadline
/// and never spawns - `Venue`'s `Drop` is what clears the anchor, so such a test
/// leaves one behind. That direction is deliberately the one to lose: under
/// today's libtest each test gets its own thread and its own thread-local, so it
/// is unreachable; and if that ever changes, an inherited anchor fails loudly
/// through [`bounded`]'s refusal, naming the budget, while a forward-moved one
/// fails silently as a kill.
fn open_budget() {
    LIVE_VENUES.with(|live| {
        if live.get() == 0 {
            BUDGET_ANCHOR.with(|cell| {
                if cell.get().is_none() {
                    cell.set(Some(Instant::now()));
                }
            });
        }
    });
}

/// A wall deadline `cap` from now, clamped so it lands inside
/// [`TEST_WALL_BUDGET`] and leaves [`TEARDOWN_RESERVE`] behind.
///
/// Take every bound from here rather than writing `Instant::now() + cap`: the
/// clamp is what keeps a deadline reportable. A bound the watchdog reaches
/// first cannot report anything.
pub fn wall_deadline(cap: Duration) -> Instant {
    bounded(cap, TEARDOWN_RESERVE)
}

/// [`wall_deadline`] for a wait that runs last, entitled to the reserve.
pub fn teardown_deadline(cap: Duration) -> Instant {
    bounded(cap, Duration::ZERO)
}

/// The clamp itself. Refuses rather than returning an instant in the past: a
/// deadline already spent makes every `timeout_at` below it fire on its first
/// poll, and the test then reports whatever that bound was named for - which is
/// a statement about the phase before it, wearing the name of this one.
fn bounded(cap: Duration, reserve: Duration) -> Instant {
    let anchor = budget_anchor();
    let ceiling = anchor + (TEST_WALL_BUDGET - reserve);
    let now = Instant::now();
    assert!(
        now < ceiling,
        "this test spent its {TEST_WALL_BUDGET:?} wall budget before this {cap:?} bound was even \
         taken - {:?} has passed since it started, and {reserve:?} of that budget is reserved for \
         teardown. Nothing asserted below this line is a statement about the venue: an earlier \
         phase used the whole budget.",
        now.saturating_duration_since(anchor)
    );
    std::cmp::min(now + cap, ceiling)
}

/// [`wall_deadline`] on tokio's clock, for `timeout_at`.
pub fn deadline(cap: Duration) -> tokio::time::Instant {
    tokio::time::Instant::from_std(wall_deadline(cap))
}

/// A launched venue plus the record it reported. The inner guard kills and reaps
/// on drop, so a failing assertion cannot leak a listening process into the rest
/// of the suite.
pub struct Venue {
    inner: LaunchedVenue,
    pub record: ReadyRecord,
    /// Boot river resolved from the same config this harness launches.
    ///
    /// A venue reports no symbol, and that is a ruling rather than an omission:
    /// under the boatyard model a venue serves many rivers, so a symbol on the
    /// readiness record could only lie about which one a consumer gets. The
    /// harness therefore resolves the boot river the way `serve.rs` does -
    /// through the venue's own profile resolution, not the raw config key, which
    /// diverges from the served spelling under case-insensitive preset matching.
    /// That resolution is pinned against a literal by
    /// `preset_only_config_resolves_the_default_river`, which exists for exactly
    /// this reason; without it the recomputation here would be answering itself.
    pub symbol: String,
    /// This venue's stderr, line by line. Captured for every venue: a run that
    /// dies after readiness discards its diagnostics otherwise, and those are
    /// the failure modes these gates actually hit.
    pub log: CapturedLog,
}

impl Drop for Venue {
    fn drop(&mut self) {
        // The last venue going away is this thread's test boundary: the next
        // `spawn` starts a fresh budget rather than inheriting this test's.
        LIVE_VENUES.with(|live| {
            let left = live.get().saturating_sub(1);
            live.set(left);
            if left == 0 {
                BUDGET_ANCHOR.with(|cell| cell.set(None));
            }
        });
        if std::thread::panicking() {
            let lines = self.log.snapshot();
            let tail = lines.len().saturating_sub(LOG_TAIL_ON_PANIC);
            eprintln!(
                "--- venue {} stderr, last {} of {} lines ---",
                self.record.pid,
                lines.len() - tail,
                lines.len()
            );
            for line in &lines[tail..] {
                eprintln!("  {line}");
            }
        }
    }
}

/// How much of a failing venue's log is printed. The whole buffer would bury the
/// assertion that failed; the tail is where the venue said why.
const LOG_TAIL_ON_PANIC: usize = 40;

impl Venue {
    pub fn http_base(&self) -> String {
        self.inner.http_base()
    }

    pub fn ws_url(&self) -> String {
        format!("{}/ws", self.inner.base_url())
    }

    pub fn ws_url_for(&self, symbol: &str) -> String {
        format!("{}/ws?symbol={symbol}", self.inner.base_url())
    }

    /// Waits for exit, failing the test if the venue outlives `timeout`.
    ///
    /// The bound is clamped to the test's remaining budget, so a venue that
    /// never exits is reported here, by name, rather than as a watchdog kill.
    /// It draws on [`TEARDOWN_RESERVE`] as well, because this wait runs last and
    /// would otherwise absorb every overrun the phases before it left - and
    /// "the venue did not exit" is a claim about the venue, which a starved
    /// teardown has no standing to make.
    /// Which bound fired is reported, and that is not cosmetic. The old message
    /// read "within `{timeout}` (or the test's remaining budget)", and the two
    /// readings are opposite verdicts: the venue outliving its shutdown grace is
    /// a defect in the venue, while the clamp landing early is a statement about
    /// how much budget the phases before this one spent on a loaded host. One
    /// intermittent in `lifecycle.rs` was filed against the venue for two
    /// documents of this arc on exactly that ambiguity. The waited-for time and
    /// the requested cap are both printed, so the next reader compares them
    /// instead of guessing.
    pub fn wait_for_exit(&mut self, timeout: Duration) -> VenueExit {
        let waited_from = Instant::now();
        let deadline = teardown_deadline(timeout);
        let clamped = deadline < waited_from + timeout;
        loop {
            if let Some(exit) = self.inner.exited() {
                return exit;
            }
            assert!(
                Instant::now() < deadline,
                "venue did not exit. It was waited on for {:?}{}",
                waited_from.elapsed(),
                if clamped {
                    format!(
                        " - which is the test's remaining budget, not the {timeout:?} asked for, \
                         so an earlier phase of this test spent the budget and this is not \
                         evidence about the venue's shutdown"
                    )
                } else {
                    format!(
                        " - the full {timeout:?} asked for, with budget to spare, so the venue \
                         itself did not stop"
                    )
                }
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// True once the process has exited, without blocking.
    pub fn has_exited(&mut self) -> bool {
        self.inner.exited().is_some()
    }
}

/// Path to the venue binary this test binary was built alongside.
pub fn venue_binary() -> &'static str {
    env!("CARGO_BIN_EXE_mogwai")
}

/// A spec pointing at the binary this test binary was built alongside, with the
/// log discarded but retained for a boot failure.
///
/// This is the launcher-contract spec, for the two gates that drive `launch`
/// themselves and read [`mogwai_protocol::launch::LaunchError`]. `NoRecord`
/// carries the venue's own stderr independently of the sink, so a boot refusal
/// still says why.
///
/// Every other caller wants [`spawn`], whose spec captures. A venue that dies
/// after readiness - which is most of what fails in these files - has no
/// `LaunchError` to carry its diagnostics, so discarding them there loses the
/// only account of what happened.
pub fn spec(extra_args: &[&str]) -> LaunchSpec {
    let mut spec = LaunchSpec {
        binary: Some(OsString::from(venue_binary())),
        stderr: StderrSink::Discard,
        ..LaunchSpec::default()
    };
    // The gates state their arguments the way the CLI does, and this maps them
    // onto the launcher's typed fields - which is also a small check that the
    // two agree about what `serve` accepts.
    let mut args = extra_args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .unwrap_or_else(|| panic!("{flag} takes a value"));
        match *flag {
            "--config" => spec.config = Some(PathBuf::from(*value)),
            "--duration" => {
                spec.duration = Some(
                    humantime::parse_duration(value)
                        .unwrap_or_else(|err| panic!("{value} is not a duration: {err}")),
                );
            }
            other => panic!("the harness does not model {other}"),
        }
    }
    spec
}

/// The launcher contract, executed through the shipped launcher. Panics with the
/// boot failure - including the venue's own stderr - if it does not come up.
///
/// The venue's stderr is captured, into [`Venue::log`], and printed on panic.
/// Two distinct things want it, which is why it is the default rather than an
/// opt-in second launch path:
///
/// - Triage. `LaunchError::NoRecord` carries stderr on a boot refusal, but a
///   venue that dies after readiness has no `LaunchError` at all, and that is
///   most of what fails here. Discarded, those diagnostics are gone.
/// - The one property no wire surface states. A venue decision visible only as
///   a log line - `market order has no market reading` is the case this was
///   built for - cannot be observed through `/ws` or any route, and a test that
///   settles for a nearby wire observable ends up asserting something the defect
///   it names would not break. Prefer a wire observable wherever one exists: a
///   log line is not a contract. The line each such test reads is named in it.
///
/// The filter is pinned, not inherited. `init_stderr_logging` falls back to
/// `mogwai=info` only when `RUST_LOG` is absent, so a developer or a CI job
/// exporting `RUST_LOG=mogwai=error` would silence every line a caller scores
/// on - and a caller scoring an absence would then pass vacuously, which is the
/// defect class this capture exists to close. It would also hand a failing test
/// an empty triage dump, which is the same vacuity wearing the other costume.
/// `RUST_LOG` is therefore set on the venue rather than left to the ambient
/// environment, and [`CapturedLog::await_positive_control`] proves the plumbing
/// separately for anyone concluding anything from an absence.
pub fn spawn(extra_args: &[&str]) -> Venue {
    // The first venue of a test starts that test's wall budget. See
    // `BUDGET_ANCHOR`: under `--test-threads=1` the thread is shared, so an
    // anchor left by an earlier test would silently shrink this one's.
    open_budget();
    let lines = std::sync::Arc::new(Mutex::new(Vec::new()));
    let sink = std::sync::Arc::clone(&lines);
    let mut spec = spec(extra_args);
    spec.env
        .push((OsString::from("RUST_LOG"), OsString::from(CAPTURED_FILTER)));
    // The callback runs on the draining thread, so it stays a push onto a
    // `Vec` - anything blocking here reintroduces the wedge `StderrSink` exists
    // to prevent.
    spec.stderr = StderrSink::Lines(Box::new(move |line| {
        if let Ok(mut lines) = sink.lock() {
            lines.push(line);
        }
    }));
    let symbol = default_symbol(spec.config.as_deref());
    // A launch that panics leaves the count at zero on purpose: the next
    // `spawn`, in the next test, then re-anchors instead of inheriting the
    // anchor this failed one opened.
    let inner = launch(spec).expect("the venue launches and reports ready");
    LIVE_VENUES.with(|live| live.set(live.get() + 1));
    Venue {
        record: inner.record().clone(),
        symbol,
        inner,
        log: CapturedLog { lines },
    }
}

/// The filter pinned on every launched venue. Matches `init_stderr_logging`'s
/// own no-`RUST_LOG` fallback, so a captured venue logs exactly what an
/// uncaptured one does - the pin removes the ambient environment's vote, it does
/// not raise the level.
const CAPTURED_FILTER: &str = "mogwai=info";

/// A venue's stderr, line by line, as [`spawn`] captured it.
pub struct CapturedLog {
    lines: std::sync::Arc<Mutex<Vec<String>>>,
}

impl CapturedLog {
    /// A line the venue is known to emit, so that an absence elsewhere in this
    /// buffer means the venue did not say it rather than that nothing was ever
    /// captured. Emitted after the readiness line goes out on stdout, so it is
    /// polled to a deadline rather than read once.
    ///
    /// Every conclusion drawn from an absence in this buffer owes a call to
    /// this first. Without it a broken filter, a broken pipe or a dead capture
    /// thread all render as "the venue never warned", which is indistinguishable
    /// from the property holding.
    pub fn await_positive_control(&self) {
        const CONTROL: &str = "mogwai listening";
        let deadline = wall_deadline(Duration::from_secs(10));
        while Instant::now() < deadline {
            if self.contains(CONTROL) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        panic!(
            "the venue's log never carried `{CONTROL}` under RUST_LOG={CAPTURED_FILTER}, so this \
             capture is empty for a reason that has nothing to do with the property under test, \
             and no absence in it means anything. Captured: {:?}",
            self.snapshot()
        );
    }

    /// The buffer, whatever state the capture thread left it in.
    ///
    /// Never `expect` here. `Venue`'s `Drop` reads this while the thread is
    /// panicking, so a panic raised on that path is a panic during unwinding,
    /// which aborts - and the abort would replace the assertion failure this
    /// dump exists to explain with an unattributed crash. A poisoned lock still
    /// holds every line captured before the poisoning, which is exactly what
    /// triage wants.
    fn read(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.lines
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Whether any captured line contains `needle`.
    pub fn contains(&self, needle: &str) -> bool {
        self.read().iter().any(|line| line.contains(needle))
    }

    /// Whether any captured line contains both needles - the shape a per-order
    /// log query takes, where the message names the decision and a field names
    /// which order it was about.
    pub fn contains_both(&self, first: &str, second: &str) -> bool {
        self.read()
            .iter()
            .any(|line| line.contains(first) && line.contains(second))
    }

    /// Everything captured so far, for a panic message.
    pub fn snapshot(&self) -> Vec<String> {
        self.read().clone()
    }
}

/// The boot river, resolved the way `serve.rs` resolves it.
///
/// This recomputes the venue's own answer, and that is deliberate rather than an
/// oversight to fix by putting the symbol back on the readiness record. Dropping
/// it from the record was a ruling: under the boatyard model a venue serves many
/// rivers, so a record naming one could only lie about which river a consumer
/// gets, and `ReadyRecord::VERSION` 6 was the designed loud break for consumers
/// still reading it. This resolution is what that landing put here in its place,
/// and it must go through the profile resolution rather than the raw config key,
/// which diverges from the served spelling under case-insensitive preset
/// matching and answers BTCUSDT for a venue serving MNQ.
///
/// What keeps the recomputation from answering itself is that it is pinned
/// against a literal by `preset_only_config_resolves_the_default_river`, whose bite
/// was proven when it landed. Do not remove that test, and do not let it become
/// the only config in the tree whose boot river is non-trivial without noticing.
fn default_symbol(config_path: Option<&std::path::Path>) -> String {
    use mogwai_venue::config;

    let cfg = config::Config::load(config_path.map(PathBuf::from))
        .expect("the harness launches only configs the venue accepts");
    config::build_instrument_profiles(&cfg)
        .expect("the harness launches only configs the venue accepts")
        .default_symbol_def(cfg.default_symbol())
        .expect("the boot shape resolves for a config the venue accepts")
        .symbol
        .to_string()
}

/// Step 3 of the contract: check `version` first, refuse a record this
/// launcher does not understand, and only then read the rest.
pub fn parse_ready(line: &str) -> ReadyRecord {
    assert!(
        !line.is_empty(),
        "venue closed the ready pipe without a line"
    );
    let probe: serde_json::Value =
        serde_json::from_str(line).expect("the readiness record is one line of JSON");
    assert_eq!(
        probe.get("version").and_then(serde_json::Value::as_u64),
        Some(u64::from(ReadyRecord::VERSION)),
        "unsupported readiness record version"
    );
    serde_json::from_str(line).expect("the readiness record parses as a ReadyRecord")
}

/// A venue with a small warmup, so a lifecycle gate is not paying for a day of
/// tape it never reads.
pub fn fast_config() -> String {
    format!("{}/tests/configs/fast.toml", env!("CARGO_MANIFEST_DIR"))
}

/// A venue with a configured maker/taker fee table, so a fill's commission is
/// a nonzero wire observable - the only reading of an armed `FeeSurcharge`
/// that does not reach into the engine.
pub fn fees_config() -> String {
    format!("{}/tests/configs/fees.toml", env!("CARGO_MANIFEST_DIR"))
}

/// A venue whose unattended accounts are TTL-collected within a few hundred
/// milliseconds, so the freeze has an observable consequence a socket test can
/// read.
pub fn account_ttl_config() -> String {
    format!(
        "{}/tests/configs/account-ttl.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

pub fn two_symbols_config() -> String {
    format!(
        "{}/tests/configs/two-symbols.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue serving a perpetual whose funding interval is one simulated second,
/// so a gate can cross a funding instant without waiting eight sim-hours.
pub fn perpetual_config() -> String {
    format!(
        "{}/tests/configs/perpetual.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

pub fn mnq_preset_config() -> String {
    format!(
        "{}/tests/configs/mnq-preset.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue that materializes no warmup, so its tape worker starts with the
/// canonical frontier sitting exactly at the tape origin.
pub fn no_warmup_config() -> String {
    format!(
        "{}/tests/configs/no-warmup.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue whose warmup ends at the first scheduled close, leaving real prints
/// in the readable past while every order submitted after readiness is closed.
pub fn scheduled_close_config() -> String {
    format!(
        "{}/tests/configs/scheduled-close.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue whose run begins inside a scheduled close with no readable print.
pub fn empty_scheduled_close_config() -> String {
    format!(
        "{}/tests/configs/empty-scheduled-close.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue whose tape is paced, so a suppression window is observable as
/// silence rather than drowned in an unpaced backlog.
pub fn paced_config() -> String {
    format!("{}/tests/configs/paced.toml", env!("CARGO_MANIFEST_DIR"))
}

/// A venue whose fanout ring is small enough that the lag policy is reachable
/// without driving hours of tape through a stalled socket.
pub fn tiny_fanout_config() -> String {
    format!(
        "{}/tests/configs/tiny-fanout.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue whose resting limits use the volatility-scaled fill band.
pub fn band_config() -> String {
    format!("{}/tests/configs/band.toml", env!("CARGO_MANIFEST_DIR"))
}

/// The declared-duration gates' venue: paced at real time, and two rivers.
///
/// Paced because those gates wait on a socket for the `RunComplete` the venue
/// writes at its deadline, and on an unpaced venue that frame is queued behind
/// the entire backlog the run generated flat out - over 1.4 million frames in
/// the 2 s of a declared run, measured - which the consumer then cannot drain
/// inside its wall budget. The file itself carries the reasoning.
pub fn bounded_run_config() -> String {
    format!(
        "{}/tests/configs/bounded-run.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A venue with a large warmup and an accelerated clock, for the slow-start gate.
pub fn accelerated_config() -> String {
    format!(
        "{}/tests/configs/accelerated.toml",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// A private scratch directory for a test that writes files.
///
/// Two ways a scratch directory collides, and this closes both. A test that
/// clears its directory before use - which every one of them must, or it reads
/// the last run's output - deletes whatever it finds there, so a name two tests
/// share is one test destroying another's output mid-run, silently, and only
/// under a thread count that puts them in flight together.
///
/// - Across processes the name carries this process's pid, so two concurrent
///   invocations of the same test binary, or a test binary running beside the
///   `#[ignore]`d gate that used to write a fixed path under `target/`, cannot
///   land on one directory at all. That collision is not detected, it is
///   unrepresentable.
/// - Within a process the names are claimed in a registry and a second claim of
///   one name panics, naming both callers' shared key. Two tests in one binary
///   share a pid, so nothing about the path can separate them; what is available
///   is refusing the ambiguity out loud instead of resolving it by deletion.
///
/// `name` is therefore free to be readable rather than unique-by-construction:
/// nothing rests on a human noticing that all of them differ.
///
/// It returns a guard because the pid suffix removed the reuse, and reuse was
/// the only thing bounding the growth of `target/tmp`. A fixed name was one
/// directory rewritten every run; `<name>-<pid>` is a fresh directory per run
/// that nothing ever revisits, so without a `Drop` every invocation of every
/// test binary would deposit another synthesized corpus and another written
/// report there, forever. Hold the guard for the test's lifetime - dropping it
/// early removes the directory out from under the code still writing to it.
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn join(&self, tail: impl AsRef<Path>) -> PathBuf {
        self.path.join(tail)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Kept on the panic path. The directory holds the corpus the test
        // synthesized and the report the binary wrote, which is the whole
        // evidence of a failure; deleting it would leave a maintainer with the
        // assertion message and nothing to read. On the passing path there is
        // nothing to look at and the removal is what keeps `target/tmp` from
        // growing without bound.
        if std::thread::panicking() {
            eprintln!(
                "[scratch] kept for triage because this test panicked: {}",
                self.path.display()
            );
            return;
        }
        std::fs::remove_dir_all(&self.path).ok();
    }
}

pub fn scratch(name: &str) -> Scratch {
    static CLAIMED: Mutex<Vec<String>> = Mutex::new(Vec::new());
    {
        let mut claimed = CLAIMED
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert!(
            !claimed.iter().any(|held| held == name),
            "two tests in this binary both claimed the scratch name {name:?}; they would share one \
             directory and each would delete the other's output on entry. Give one of them a \
             different name."
        );
        claimed.push(name.to_string());
    }
    let dir =
        PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("{name}-{}", std::process::id()));
    if dir.exists() {
        // Only reachable when a pid is reused across runs and the guard below
        // did not run - a killed process, or a panic, which keeps the directory
        // on purpose. Either way the contents are the stale output this clear
        // exists for.
        std::fs::remove_dir_all(&dir).expect("clearing the scratch directory");
    }
    std::fs::create_dir_all(&dir).expect("creating the scratch directory");
    Scratch { path: dir }
}

/// One blocking HTTP GET, returning the status code and the body. Hand-rolled
/// rather than pulling an HTTP consumer into the dev-dependencies: the venue's
/// request surface is four routes and a status line.
pub fn http_get(base: &str, path: &str) -> (u16, String) {
    let authority = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(authority).expect("connect to the venue");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("send request");
    split_response(&read_response(&mut stream, path))
}

/// Reads one `Connection: close` response to the end, bounded by the test's
/// remaining budget rather than by a per-`read` timeout.
///
/// `set_read_timeout` bounds one syscall, not the exchange: a venue dribbling a
/// byte at a time resets it on every read and the route is then unbounded no
/// matter what the timeout says. The bound that matters is the whole read, so it
/// is a deadline loop, and it names the route and the reason - a bare
/// `read_response: Resource temporarily unavailable` names neither, which is the
/// wrong-answer class these gates exist to remove.
fn read_response(stream: &mut TcpStream, path: &str) -> Vec<u8> {
    let deadline = wall_deadline(Duration::from_secs(10));
    let mut raw = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let now = Instant::now();
        assert!(
            now < deadline,
            "the test's wall budget expired while reading {path}; {} bytes arrived first",
            raw.len()
        );
        stream
            .set_read_timeout(Some(std::cmp::max(
                deadline.saturating_duration_since(now),
                Duration::from_millis(1),
            )))
            .expect("read timeout");
        match stream.read(&mut chunk) {
            Ok(0) => return raw,
            Ok(read) => raw.extend_from_slice(&chunk[..read]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => {}
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                panic!(
                    "the venue stopped answering {path} with {} bytes delivered and the test's \
                     wall budget spent",
                    raw.len()
                );
            }
            Err(err) => panic!("reading {path}: {err}"),
        }
    }
}

/// Splits a raw response into its status code and body.
fn split_response(raw: &[u8]) -> (u16, String) {
    let text = String::from_utf8_lossy(raw).to_string();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .expect("a status line");
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    (status, body)
}

/// One blocking HTTP POST of a JSON body, returning the status code and body.
/// Same reasoning as `http_get`, plus a `Content-Type` and a length.
pub fn http_post_json(base: &str, path: &str, body: &str) -> (u16, String) {
    let authority = base.trim_start_matches("http://");
    let mut stream = TcpStream::connect(authority).expect("connect to the venue");
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: {authority}\r\nContent-Type: application/json\r\n\
         Content-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        len = body.len()
    );
    stream.write_all(request.as_bytes()).expect("send request");
    split_response(&read_response(&mut stream, path))
}
