// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! L2 gates: the declared run duration, the announced completion, and the
//! deliberate difference between finishing and being killed.
//!
//! `RunComplete` exists so a client can tell a finished run from a dead one.
//! These tests are therefore as much about what is NOT sent under a signal as
//! about what is sent at the deadline.

mod common;

use std::time::Duration;

use common::{Venue, accelerated_config, bounded_run_config, fast_config, http_get, spawn};
use futures_util::StreamExt;
use mogwai_protocol::{
    ServerMessage,
    launch::{LaunchSpec, StderrSink, launch},
};
use std::sync::{Arc, Mutex};
use tokio_tungstenite::tungstenite::Message;

/// A `/ws` socket as `tokio_tungstenite` hands it back from a connect.
type WsSocket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens a socket onto the run and returns the stream.
async fn connect(venue: &Venue) -> WsSocket {
    let (socket, _) = tokio_tungstenite::connect_async(venue.ws_url())
        .await
        .expect("open a socket onto the run");
    socket
}

/// A `--duration`-bounded run, watched to completion on sockets that were
/// PROVABLY live sessions on it.
struct WatchedRun {
    venue: Venue,
    /// One entry per requested url, in that order.
    seen: Vec<Watched>,
    /// Runs no socket of this test was ever a session on. KEPT ALIVE rather than
    /// dropped as they are discarded, and that is load-bearing rather than
    /// laziness: `common`'s wall budget re-anchors when the LAST live venue goes
    /// away, so releasing a loser mid-test would restart this test's budget and
    /// push its ceiling past the hang watchdog - the one failure mode that
    /// mechanism exists to prevent. They exit on their own at their own declared
    /// deadlines; holding the guards costs a file descriptor.
    _spent: Vec<Venue>,
}

/// How long [`watch_a_bounded_run`] may spend losing the attach race before it
/// gives up and says so.
///
/// A CEILING ON RETRIES IS NOT THE SAME AS A CEILING ON THE LAST ONE, and the
/// difference bites the accelerated caller specifically. This budget is checked
/// BETWEEN attempts, while the surrounding wall budget clamps every bound to
/// anchor plus 13 s; an attempt that re-boots a venue materializing six
/// simulated hours of warmup can cost seconds, so a naive "still under the
/// budget, go again" admits an attempt that runs past the clamp - at which point
/// `common::bounded` refuses with "this test spent its wall budget before this
/// bound was even taken", naming the clamp rather than the race, which is a
/// worse message about the same fact. So the check is made against the LAST
/// ATTEMPT'S MEASURED COST rather than against the instant alone: another
/// attempt is started only if one of the same size still fits.
const ATTACH_RETRY_BUDGET: Duration = Duration::from_secs(8);

/// Launches a `--duration`-bounded venue, opens the named sockets and drains
/// every one of them to completion - relaunching until each socket was a LIVE
/// SESSION on the run it is reporting about.
///
/// THE WRONG ANSWER THIS REMOVES. A declared duration is a WALL sleep started at
/// readiness (`serve.rs` sleeps `sim.wall_duration(remaining)` and then completes
/// the run) and the launcher returns AT readiness, so every test of this family
/// connects into a span already running down. Under parallel execution the
/// connect can lose: the venue is already tearing down, the connection is
/// accepted and dropped without a session ever running, and the test then fails
/// on "the run announces its completion on the wire". That is a WRONG ANSWER
/// about the venue rather than a timeout, which is exactly why it read like a
/// real regression - measured at `test_threads = 16`, where the test finished
/// EARLY, at 2.016s against its usual 2.215s, having never seen the frame.
///
/// THE PREMISE IS "THIS SOCKET WAS A SESSION", NOT "THIS SOCKET ATTACHED IN
/// TIME", and the difference is not pedantry - the first version of this helper
/// used the second and STILL FAILED under the gate. Attaching late is not the
/// defect: `ws.rs` checks `already_complete` when the session starts and
/// announces to a socket that arrived after the run finished, so a late attach
/// is served. What produces nothing is a connection that never became a session
/// at all, and the only sound evidence either way is the venue having written
/// SOMETHING on that socket. A run where some socket saw no frame is DISCARDED,
/// so the test can only ever make a statement about a run it was actually
/// watching.
///
/// A LONGER DURATION IS NOT THE FIX and neither is a shorter connect. Both are
/// bets on a margin, and a margin is what a crowded host takes away; this family
/// was parked rather than retuned for exactly that reason.
///
/// A PASSENGER-SCOPED `?duration_ms=` WOULD NOT DO IT EITHER, and it is worth
/// saying so because it looks like the obvious answer: that deadline starts at
/// UPGRADE, so the race really is gone - but it closes ONE SOCKET and leaves the
/// run going, which is the property
/// `a_passenger_duration_closes_one_socket_and_leaves_the_boat_running` in
/// `serving.rs` exists to pin. No test here is about a passenger. One asserts
/// that the VENUE exits 0 at its declared deadline, which a passenger duration
/// does not cause, and another that the run-wide announcement reaches EVERY open
/// socket, which a per-socket deadline cannot express at all. Substituting it
/// would leave both names attached to different properties.
async fn watch_a_bounded_run(
    config: &str,
    duration: &str,
    urls: impl Fn(&Venue) -> Vec<String>,
) -> WatchedRun {
    let mut spent: Vec<Venue> = Vec::new();
    let mut give_up: Option<std::time::Instant> = None;
    loop {
        let attempt_started = std::time::Instant::now();
        let venue = spawn(&["--config", config, "--duration", duration]);
        // Taken after the first launch, so the retry budget runs from the same
        // instant the test's own wall budget does.
        let give_up = *give_up.get_or_insert_with(|| common::wall_deadline(ATTACH_RETRY_BUDGET));

        // THE WANTED COUNT COMES FROM THE REQUEST, NEVER FROM WHAT WAS
        // ACHIEVED, and that is the whole guard rather than a style point. An
        // earlier shape compared the drained count against `sockets.len()`,
        // which on the losing branch this function exists to detect - the very
        // first connect refused, so `sockets` empty - reduced to `0 == 0` with
        // an `all` over an EMPTY iterator, which is `true`. It returned success
        // carrying nothing, and every caller then index-panicked on `seen[0]`:
        // the unattributed failure this helper was written to eliminate,
        // reachable on exactly the race its docstring describes.
        let requested = urls(&venue);
        let wanted = requested.len();
        assert!(
            wanted > 0,
            "a watched run needs at least one socket; with none, every check below is vacuous"
        );

        let mut sockets = Vec::new();
        for url in &requested {
            // A refused or rejected upgrade is the run having ended under us. It
            // is not asserted on: this is the losing branch.
            match tokio_tungstenite::connect_async(url).await {
                Ok((socket, _)) => sockets.push(socket),
                Err(_) => break,
            }
        }

        let seen = if sockets.len() == wanted {
            // All drained CONCURRENTLY. Draining one while another sits parked
            // would let the parked one be evicted by the bounded fanout ring and
            // report that as the announcement never arriving.
            futures_util::future::join_all(
                sockets
                    .iter_mut()
                    .map(|socket| drain_to_completion(socket, Duration::from_secs(30))),
            )
            .await
        } else {
            Vec::new()
        };

        // TWO CONDITIONS, AND THE SECOND WAS MISSING FOR A ROUND. The first is
        // the premise: every socket was a live session, evidenced by a content
        // frame. The second is that every drain got to WATCH THE RUN END -
        // `drain_to_completion`'s deadline is clamped to this test's remaining
        // wall budget, so an attempt begun late enough is cut off mid-run and
        // returns a perfectly plausible-looking reading with no announcement in
        // it. That reading passed the frame count, was accepted, and the caller
        // reported the missing announcement as a venue defect - the same wrong
        // answer this helper exists to eliminate, arriving through the helper's
        // own acceptance test. A budget-ended drain is not evidence either way,
        // so the run is discarded exactly like a lost attach.
        if seen.len() == wanted
            && seen
                .iter()
                .all(|watched| watched.content_frames > 0 && watched.ending != Ending::Deadline)
        {
            return WatchedRun {
                venue,
                seen,
                _spent: spent,
            };
        }

        drop(sockets);
        spent.push(venue);
        // The room a further attempt needs is what the last one COST, not a
        // single instant: see [`ATTACH_RETRY_BUDGET`]. Without the cost term
        // this hands the wall clamp an attempt it cannot finish and the clamp's
        // message replaces this one.
        let attempt_cost = attempt_started.elapsed();
        assert!(
            std::time::Instant::now() + attempt_cost < give_up,
            "after {} launches this test never fully WATCHED a {duration} run - either some \
             socket was accepted and written nothing at all, which is a run already tearing down \
             when this test reached it, or a drain hit the clamped wall deadline before the run \
             ended, which observes only part of it. Endings on the last attempt: {:?}. That is a \
             statement about how loaded this host is, not about the venue, and a run this test \
             never watched to its end cannot be reported on either way. The last attempt cost \
             {attempt_cost:?}, which is what a further one is budgeted at.",
            spent.len(),
            seen.iter()
                .map(|watched| (watched.ending, watched.content_frames))
                .collect::<Vec<_>>()
        );
    }
}

/// What draining one socket to the end of the run observed.
struct Watched {
    /// The announcement's `(sim_now_ns, elapsed_ns)`, if one arrived at all.
    announcement: Option<(u64, u64)>,
    /// Whether the venue closed the socket.
    closed: bool,
    /// HOW MANY CONTENT FRAMES THE VENUE WROTE - `Text` only, which is the
    /// venue's entire vocabulary for a session. Zero separates "this connection
    /// was never a session" from "the venue served this socket and never
    /// announced", which is the whole difference between a loaded host and a
    /// defect. See [`watch_a_bounded_run`].
    ///
    /// CONTROL FRAMES ARE NOT SESSION EVIDENCE and counting them broke the
    /// premise the counter exists to establish: a connection upgraded and then
    /// CLOSED by a venue already tearing down writes exactly one frame, the
    /// close, and a peer Ping does the same, so an all-frames count reported the
    /// losing branch as a live session and the caller then panicked asserting
    /// "the venue had already served 1 frames on, so this was a live session" -
    /// the exact falsehood the counter rules out.
    content_frames: usize,
    /// HOW THE DRAIN ENDED, and it is the second half of the session premise
    /// rather than diagnostics. A drain cut off by [`common::deadline`]'s clamp
    /// has not observed the end of the run at all, so an absent announcement in
    /// that reading says nothing about the venue - and the caller reports it as
    /// the venue never announcing. See [`Ending`].
    ending: Ending,
}

/// How a drain stopped. The distinction that matters is VENUE-ENDED versus
/// BUDGET-ENDED: the first three are the venue speaking, the fourth is this
/// test running out of wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    /// The venue sent a close frame.
    Closed,
    /// The stream ended without one.
    StreamEnded,
    /// The transport failed.
    Failed,
    /// The read deadline was reached with the socket still live. NOT EVIDENCE
    /// ABOUT THE RUN.
    Deadline,
}

/// Drains until a `RunComplete` arrives or the socket ends, reporting what was
/// seen AND how the stream ended.
///
/// THE ENDING IS RECORDED BECAUSE THE OLD LOOP ERASED IT. `while let
/// Ok(Some(Ok(m)))` folds a close, a transport error, a clean stream end and a
/// timeout into one "the loop finished", which is the arc's standing rule - a
/// drain that does not record how the stream ended is not a drain - in the one
/// place the rule had not been applied. It cost a real gate failure: the drain
/// deadline is CLAMPED to the test's remaining wall budget, so on a loaded host
/// a run whose attach retries had eaten the budget was drained for a fraction of
/// its declared duration, and `run_complete_reaches_every_open_socket` reported
/// "the first socket saw the completion announcement, after 15 content frames" -
/// a property failure produced by an exhausted budget. Fifteen frames of a paced
/// 2 s MNQ run is about 0.4 s of it.
async fn drain_to_completion(socket: &mut WsSocket, timeout: Duration) -> Watched {
    let mut announcement = None;
    let mut content_frames = 0;
    // Clamped to the test's remaining budget: a 30 s bound inside a 20 s per-test
    // watchdog cannot report anything, because the watchdog reaches it first and
    // the failure arrives as an unattributed kill.
    let deadline = common::deadline(timeout);
    let ending = loop {
        let message = match tokio::time::timeout_at(deadline, socket.next()).await {
            Err(_) => break Ending::Deadline,
            Ok(None) => break Ending::StreamEnded,
            Ok(Some(Err(_))) => break Ending::Failed,
            Ok(Some(Ok(message))) => message,
        };
        match message {
            // THE SUBSTRING IS A PRE-FILTER, NOT THE ASSERTION, and it is here
            // for throughput rather than convenience. The one caller still on an
            // UNPACED `speed = 0.0` venue - the sigterm gate - takes over a
            // million frames on one socket in two seconds, measured, and
            // `serde_json::from_str::<ServerMessage>` on every one of them makes
            // the DRAIN the bottleneck: a frame the test is waiting for is
            // queued behind the whole backlog and the test spends its wall
            // budget parsing tape it does not care about, then reports the
            // absence. That is the wrong answer this family keeps producing,
            // arriving by a third route. The bounded-run callers are PACED and
            // carry about a hundred frames, so the filter is free there rather
            // than load-bearing. Candidates are still parsed and still
            // destructured, so nothing is concluded from the substring itself.
            Message::Text(text) => {
                content_frames += 1;
                if text.contains("RunComplete")
                    && let Ok(ServerMessage::RunComplete {
                        sim_now_ns,
                        elapsed_ns,
                    }) = serde_json::from_str::<ServerMessage>(&text)
                {
                    announcement = Some((sim_now_ns, elapsed_ns));
                }
            }
            Message::Close(frame) => {
                if let Some(frame) = frame {
                    assert_eq!(
                        u16::from(frame.code),
                        1000,
                        "a completed run closes with WS 1000, not a fault code"
                    );
                }
                break Ending::Closed;
            }
            _ => {}
        }
    };
    Watched {
        announcement,
        closed: ending == Ending::Closed,
        content_frames,
        ending,
    }
}

/// The declared duration is a contract: the venue serves exactly that much sim
/// time, says so, and exits 0 on its own without anybody signalling it.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn venue_announces_run_complete_and_exits_zero_at_the_declared_sim_deadline() {
    let mut run =
        watch_a_bounded_run(&bounded_run_config(), "2s", |venue| vec![venue.ws_url()]).await;
    assert_eq!(
        run.venue.record.run_duration_ns,
        Some(2_000_000_000),
        "the readiness record reports the declared duration"
    );

    let watched = &run.seen[0];
    let (_, elapsed_ns) = watched.announcement.unwrap_or_else(|| {
        panic!(
            "the run announced no completion on a socket the venue had already served {} content \
             frames on and whose drain ended {:?}, so this was a live session watched to the end \
             and not a connect that lost a race",
            watched.content_frames, watched.ending
        )
    });
    assert!(
        elapsed_ns >= 2_000_000_000,
        "the run served {elapsed_ns} ns of a declared 2s"
    );
    assert!(watched.closed, "the announcement is followed by a close");

    let status = run.venue.wait_for_exit(Duration::from_secs(20));
    assert_eq!(
        status.code,
        Some(0),
        "a planned completion is exit 0, not a crash"
    );
}

/// A source refusal is a venue failure, not an ordinary finite replay.  This
/// is deliberately an end-to-end gate because it pins the fault side channel,
/// its ERROR diagnostic and the binary's exit status together.  Sampling
/// `/health` for a terminal fault is intentionally not gated: the process may
/// exit before a client can observe that transient state.
#[test]
#[ignore = "binds a loopback listener"]
fn a_faulted_venue_exits_nonzero_and_an_exhausted_one_does_not() {
    // The null side of the field, on a HEALTHY venue, taken first. It is a
    // separate launch rather than a phase of the faulted one below because a
    // venue whose source faults may be gone before any client can poll it - so
    // "`fault` is null before the fault" is not observable on that process, and
    // asserting it there would be a race dressed as a property.
    //
    // Its stderr is DISCARDED, deliberately: nothing here reads the healthy
    // venue's diagnostics. This launch used to carry a capturing sink and an
    // `Arc<Mutex<Vec<_>>>` that was shadowed a dozen lines below and never read
    // once - a buffer filling for no reader, which reads to a maintainer as
    // though the assertion below were scoring it.
    let healthy = launch(LaunchSpec {
        binary: Some(common::venue_binary().into()),
        config: Some(common::fast_config().into()),
        stderr: StderrSink::Discard,
        ..LaunchSpec::default()
    })
    .expect("a healthy venue reports readiness");
    let (_, healthy_health) = http_get(&format!("http://{}", healthy.addr()), "/health");
    assert!(healthy_health.contains("\"fault\":null"));
    drop(healthy);

    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let diagnostics_for_sink = Arc::clone(&diagnostics);
    let faulted = launch(LaunchSpec {
        binary: Some(common::venue_binary().into()),
        config: Some(
            format!(
                "{}/tests/configs/arrival-fault.toml",
                env!("CARGO_MANIFEST_DIR")
            )
            .into(),
        ),
        stderr: StderrSink::Lines(Box::new(move |line| {
            diagnostics_for_sink
                .lock()
                .expect("diagnostic lock")
                .push(line);
        })),
        ..LaunchSpec::default()
    })
    .expect("fault venue reaches readiness");
    let deadline = common::wall_deadline(Duration::from_secs(10));
    let exit = loop {
        if let Some(exit) = faulted.exited() {
            break exit;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "faulted venue did not exit"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_ne!(exit.code, Some(0), "a source fault is a nonzero venue exit");
    assert!(
        diagnostics
            .lock()
            .expect("diagnostic lock")
            .iter()
            .any(|line| line.contains("tape source faulted")),
        "the terminal source fault emits the ERROR diagnostic"
    );

    let mut bounded = spawn(&["--config", &fast_config(), "--duration", "2s"]);
    let (_, health) = http_get(&bounded.http_base(), "/health");
    assert!(health.contains("\"fault\":null"));
    assert_eq!(bounded.wait_for_exit(Duration::from_secs(20)).code, Some(0));
}

/// The `watch` fanout is the thing under test: with no registry of connections,
/// every open socket must still see the announcement.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn run_complete_reaches_every_open_socket() {
    let mut run = watch_a_bounded_run(&bounded_run_config(), "2s", |venue| {
        vec![venue.ws_url(), venue.ws_url()]
    })
    .await;
    let (left, right) = (&run.seen[0], &run.seen[1]);

    assert!(
        left.announcement.is_some(),
        "the first socket saw the completion announcement, after {} content frames and a {:?} \
         ending",
        left.content_frames,
        left.ending
    );
    assert!(
        right.announcement.is_some(),
        "the second socket saw it too - the fanout reaches every connection - after {} content \
         frames and a {:?} ending",
        right.content_frames,
        right.ending
    );
    assert!(
        left.closed && right.closed,
        "both sockets were closed with WS 1000"
    );

    assert_eq!(
        run.venue.wait_for_exit(Duration::from_secs(20)).code,
        Some(0)
    );
}

#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn run_complete_is_stamped_on_the_receiving_sockets_clock() {
    // Same family as the two above, and LATENT rather than caught: it was never
    // on the parked list only because it had not lost the race yet.
    //
    // WHAT MAKES THE TWO INSTANTS DIFFER IS THE BOATS' WALL ANCHOR, not their
    // speed - both are at 1.0 here, and `boatyard.rs` gives every boat the same
    // `sim_epoch_ns` and a `wall_anchor_ns` taken when the boat is built, so two
    // boats built at different instants read different sim-now at one wall
    // instant. The `assert_ne!` below is sensitive to exactly that, which is
    // what a shared-clock regression would erase.
    //
    // THIS IS THE ONE CALLER PACING PUTS A LIVENESS REQUIREMENT ON, since the
    // watcher discards a run where any socket saw no content frame and a river
    // too quiet to print inside the declared window would discard every run and
    // blame host load. Measured rather than argued, six runs at seed 42 and
    // deterministic to the millisecond: MNQ prints 89 content frames, the first
    // 171 ms after attach, longest gap 519 ms; the sparser BTCUSDT boot river
    // prints 16, the first 1.031 s after attach, which is also its longest gap.
    // Both fit the declared 2 s, and it is the BOOT river that carries the thin
    // margin - about a second - not MNQ. The fixture carries the same numbers.
    let mut run = watch_a_bounded_run(&bounded_run_config(), "2s", |venue| {
        vec![
            format!("{}?symbol={}", venue.ws_url(), venue.symbol),
            format!("{}?symbol=MNQ&speed=1", venue.ws_url()),
        ]
    })
    .await;
    let left = run.seen[0]
        .announcement
        .expect("boot boat receives completion");
    let right = run.seen[1]
        .announcement
        .expect("second boat receives completion");
    assert_ne!(
        left.0, right.0,
        "each socket must receive its own boat instant"
    );
    assert_ne!(
        left.1, right.1,
        "each socket must receive its own covered span"
    );
    assert_eq!(
        run.venue.wait_for_exit(Duration::from_secs(20)).code,
        Some(0)
    );
}

/// A signal is not a planned completion and must not be reported as one. This
/// is the whole reason `RunComplete` exists, so it is pinned directly.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn sigterm_closes_without_announcing_run_complete() {
    let mut venue = spawn(&["--config", &fast_config()]);
    let mut socket = connect(&venue).await;

    // The condition this needs is "this socket is attached and being served",
    // and the venue states it by serving a frame. Waiting a fixed 300 ms for it
    // meant a slow host signalled a socket that was not attached yet, and the
    // absent `RunComplete` below - the whole point of the test - would then have
    // been the race rather than the venue's behaviour.
    let deadline = common::deadline(Duration::from_secs(10));
    loop {
        let message = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect(
                "the socket was never served a frame, so the signal below would land on a \
                     socket that was not attached and the absent RunComplete would be that race",
            )
            .expect("the venue closed the socket before serving it anything")
            .expect("a well-formed frame");
        if matches!(message, Message::Text(_)) {
            break;
        }
    }
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(i32::try_from(venue.record.pid).expect("pid fits")),
        nix::sys::signal::Signal::SIGTERM,
    )
    .expect("signal the venue");

    let watched = drain_to_completion(&mut socket, Duration::from_secs(20)).await;
    // THE ABSENCE IS ONLY EVIDENCE IF THE DRAIN RAN OUT OF VENUE RATHER THAN
    // OUT OF BUDGET. A signalled venue ends this socket - the assertion below
    // reads "nothing planned was announced before it died", and a drain cut off
    // by the clamped deadline would satisfy it without the venue having died at
    // all. Checked FIRST, so an exhausted budget names itself instead of
    // arriving as a claim about the venue.
    assert_ne!(
        watched.ending,
        Ending::Deadline,
        "the drain hit its wall deadline with the socket still live, so the absent announcement \
         below is this test running out of budget rather than the venue dying unannounced"
    );
    assert!(
        watched.announcement.is_none(),
        "a signalled venue announced a planned completion it never had: {:?}",
        watched.announcement
    );
    venue.wait_for_exit(Duration::from_secs(20));
}

/// Decision 8. A large warmup, a fast clock and a short declared duration: if
/// the deadline were measured from boot instead of from the post-warmup
/// `started_ns`, the accelerated duration would elapse DURING warmup and the
/// run would be over before the launcher could connect. It must instead serve
/// its whole declared duration after readiness.
#[tokio::test]
#[ignore = "binds a loopback listener"]
async fn a_short_accelerated_run_is_not_over_before_it_is_ready() {
    // THE TIGHTEST WINDOW IN THE FAMILY, and the one whose `expect` below WAS
    // the premise rather than the property: 30 declared simulated seconds at
    // speed 100 is 0.3 s of wall, so this had the least margin of any of them
    // and was never parked only because it had not yet lost. The premise is
    // established by the watcher instead, and a run no socket of this test was a
    // session on is discarded rather than reported as the deadline epoch being
    // wrong.
    let mut run =
        watch_a_bounded_run(&accelerated_config(), "30s", |venue| vec![venue.ws_url()]).await;
    let record = run.venue.record.clone();

    // The run start is the post-warmup instant, so the whole declared warmup
    // sits behind it on the sim axis.
    assert_eq!(
        record.run_start_ns - record.data_origin_ns,
        record.warmup_ns
    );

    let watched = &run.seen[0];
    let (sim_now_ns, elapsed_ns) = watched.announcement.unwrap_or_else(|| {
        panic!(
            "the run announced no completion on a socket the venue had already served {} content \
             frames on and whose drain ended {:?}, so this was a live session watched to the end \
             and not a connect that lost a race",
            watched.content_frames, watched.ending
        )
    });

    assert!(
        elapsed_ns >= 30_000_000_000,
        "the run served only {elapsed_ns} ns of a declared 30s; the deadline epoch \
         is boot rather than the post-warmup start"
    );
    assert!(
        sim_now_ns >= record.run_start_ns + 30_000_000_000,
        "sim-now at completion is at least the declared duration past run_start_ns"
    );
    assert_eq!(
        run.venue.wait_for_exit(Duration::from_secs(20)).code,
        Some(0)
    );
}
