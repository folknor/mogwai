use std::{
    fs,
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use mogwai_protocol::ClientMessage;
use nix::{
    errno::Errno,
    sys::signal::{Signal, kill},
    unistd::Pid,
};
use tungstenite::Message;

const COMMAND_TIMEOUT: Duration = Duration::from_secs(12);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn daemon_detaches_and_serves_then_stops() {
    let daemon = Daemon::start();

    assert_health(&daemon.addr);
    let pid = read_pid_file(&daemon.pid_file).expect("daemon wrote pid");
    assert!(pid_is_alive(pid), "daemon pid {pid} is not live");

    let output = stop_daemon(&daemon.pid_file);
    assert_success(&output, "stop daemon");
    wait_for_health(&daemon.addr, false, HEALTH_TIMEOUT);
    wait_for_path_gone(&daemon.pid_file, HEALTH_TIMEOUT);
}

#[test]
fn daemon_stops_with_live_subscription() {
    let daemon = Daemon::start();
    let url = format!("ws://{}/ws", daemon.addr);
    let (mut socket, _) = tungstenite::connect(url.as_str()).expect("websocket connect");
    let subscribe = serde_json::to_string(&ClientMessage::Subscribe {
        symbols: vec!["BTCUSDT".into()],
        start_ts: None,
        regime: None,
    })
    .expect("subscribe json");
    socket
        .send(Message::Text(subscribe.into()))
        .expect("send subscribe");

    let output = stop_daemon(&daemon.pid_file);
    assert_success(&output, "stop daemon with live subscription");
    wait_for_health(&daemon.addr, false, HEALTH_TIMEOUT);
    wait_for_path_gone(&daemon.pid_file, HEALTH_TIMEOUT);
}

#[test]
fn serve_refuses_second_start() {
    let daemon = Daemon::start();
    let second_addr = pick_addr();
    let second_log = target_tmp_dir().join(format!("d-{}-second.log", second_addr.port()));
    ignore_error(fs::remove_file(&second_log));

    let output = serve_daemon(&second_addr, &second_log, &daemon.pid_file);

    assert!(
        !output.status.success(),
        "second daemon unexpectedly started\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output),
    );
    assert!(
        stderr(&output).contains("already running"),
        "second daemon did not report the PID lock\nstdout:\n{}\nstderr:\n{}",
        stdout(&output),
        stderr(&output),
    );
    assert!(
        !health_ok(&second_addr),
        "second daemon bound {second_addr} despite shared pid file",
    );
}

#[test]
fn stop_removes_a_stale_pid_file() {
    // A pid file with no live daemon behind it: `stop` acquires the (unheld)
    // lock and unlinks it under the lock, having verified the path still names
    // the inode it locked (the S7 verified-unlink). The file must be gone and the
    // command must succeed.
    let pid_file = target_tmp_dir().join(format!("stale-{}.pid", std::process::id()));
    fs::write(&pid_file, "").expect("write stale pid file");

    let output = stop_daemon(&pid_file);
    assert_success(&output, "stop a stale pid file");
    assert!(
        !pid_file.exists(),
        "stale pid file should be removed: {}",
        pid_file.display()
    );
}

#[test]
fn stop_on_missing_pid_file_is_clean() {
    let pid_file = target_tmp_dir().join(format!("missing-{}.pid", std::process::id()));
    ignore_error(fs::remove_file(&pid_file));

    let output = stop_daemon(&pid_file);
    assert_success(&output, "stop a missing pid file");
    assert!(
        stdout(&output).contains("no daemon running"),
        "stop on a missing pid file reports no daemon: {}",
        stdout(&output)
    );
}

struct Daemon {
    addr: SocketAddr,
    pid_file: PathBuf,
    _log_file: PathBuf,
    _cleanup: Cleanup,
}

impl Daemon {
    fn start() -> Self {
        let addr = pick_addr();
        let tmp = target_tmp_dir();
        let pid_file = tmp.join(format!("d-{}.pid", addr.port()));
        let log_file = tmp.join(format!("d-{}.log", addr.port()));
        cleanup_existing(&pid_file);
        ignore_error(fs::remove_file(&log_file));
        let cleanup = Cleanup {
            pid_file: pid_file.clone(),
        };

        let output = serve_daemon(&addr, &log_file, &pid_file);
        assert_success(&output, "start daemon");
        wait_for_health(&addr, true, HEALTH_TIMEOUT);

        Self {
            addr,
            pid_file,
            _log_file: log_file,
            _cleanup: cleanup,
        }
    }
}

struct Cleanup {
    pid_file: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        cleanup_existing(&self.pid_file);
    }
}

fn serve_daemon(addr: &SocketAddr, log_file: &Path, pid_file: &Path) -> Output {
    let child = Command::new(bin())
        .arg("serve")
        .arg("--addr")
        .arg(addr.to_string())
        .arg("--log-file")
        .arg(log_file)
        .arg("--pid-file")
        .arg(pid_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mogwai serve");
    wait_output(child, COMMAND_TIMEOUT)
}

fn stop_daemon(pid_file: &Path) -> Output {
    let child = Command::new(bin())
        .arg("stop")
        .arg("--pid-file")
        .arg(pid_file)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mogwai stop");
    wait_output(child, COMMAND_TIMEOUT)
}

fn wait_output(mut child: Child, timeout: Duration) -> Output {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(_status) = child.try_wait().expect("poll child") {
            return child.wait_with_output().expect("collect child output");
        }
        if Instant::now() >= deadline {
            ignore_error(child.kill());
            return child
                .wait_with_output()
                .expect("collect timed out child output");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn cleanup_existing(pid_file: &Path) {
    if !pid_file.exists() {
        ignore_error(fs::remove_file(pid_file));
        return;
    }
    let output = stop_daemon(pid_file);
    if !output.status.success()
        && let Some(pid) = read_pid_file(pid_file)
    {
        ignore_error(kill(Pid::from_raw(pid), Signal::SIGKILL));
    }
    ignore_error(fs::remove_file(pid_file));
}

fn pick_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind probe listener");
    listener.local_addr().expect("probe listener address")
}

fn target_tmp_dir() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    fs::create_dir_all(&dir).expect("target tmp dir");
    dir
}

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_mogwai")
}

fn assert_success(output: &Output, action: &str) {
    assert!(
        output.status.success(),
        "{action} failed\nstatus: {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output),
    );
}

fn assert_health(addr: &SocketAddr) {
    assert!(
        health_ok(addr),
        "health endpoint did not return ok at {addr}"
    );
}

fn wait_for_health(addr: &SocketAddr, expected: bool, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if health_ok(addr) == expected {
            return;
        }
        if Instant::now() >= deadline {
            panic!("health state at {addr} did not become {expected}");
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn wait_for_path_gone(path: &Path, timeout: Duration) {
    let deadline = Instant::now() + timeout;
    loop {
        if !path.exists() {
            return;
        }
        if Instant::now() >= deadline {
            panic!("{} still exists", path.display());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn health_ok(addr: &SocketAddr) -> bool {
    match health_response(addr) {
        Ok(response) => response.contains("200 OK") && response.ends_with("ok"),
        Err(_err) => false,
    }
}

fn health_response(addr: &SocketAddr) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(addr, Duration::from_millis(200))?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(b"GET /health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    Ok(response)
}

fn read_pid_file(path: &Path) -> Option<i32> {
    let text = fs::read_to_string(path).ok()?;
    text.trim().parse().ok()
}

fn pid_is_alive(pid: i32) -> bool {
    match kill(Pid::from_raw(pid), None) {
        Ok(()) | Err(Errno::EPERM) => true,
        Err(_err) => false,
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ignore_error<T, E>(result: Result<T, E>) {
    if let Err(err) = result {
        let _ignored = err;
    }
}
