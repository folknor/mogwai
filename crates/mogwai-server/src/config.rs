// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Run configuration: the `mogwai.toml` schema, instrument-profile
//! construction/validation, and the sim-clock derivation that reads it. Split
//! out of `main.rs` because these are the load-time knobs the rest of the
//! server (the HTTP routes, the websocket replay) only ever reads back
//! through `AppState`/`SimClock`, never mutates.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
};

use mogwai_protocol::{InstrumentDef, SimClock};
use rust_decimal::Decimal;
use tokio::sync::Semaphore;

use crate::admission::AdmissionLimits;
use crate::source;

/// Replay/runtime configuration, loaded from a TOML config file at startup
/// (see `load`); never from ambient environment variables.
#[derive(Debug, Clone, serde::Deserialize)]
// `deny_unknown_fields` makes config.md's "a malformed file is a hard error"
// promise literally true for top-level knobs: a typo'd key (`gap_cap_m = 0`)
// used to fall through to the field default silently, so the operator ran with a
// value they never set (S20). `default` (missing keys keep built-in defaults) and
// `deny` (unknown keys are rejected) are orthogonal and compose. NOTE: this does
// not reach the flattened `InstrumentDef` inside a `[[instrument]]` table - serde
// cannot combine `deny_unknown_fields` with `flatten` - so a typo'd instrument
// field is still tolerated; only the top-level run knobs are guarded here.
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    /// Simulated start instant. `0` keeps the identity wall-time clock.
    pub(crate) sim_epoch_ns: u64,
    /// Wall instant the accelerated clock anchors to. `0` (the default)
    /// anchors at server boot, which REWINDS sim time to `sim_epoch_ns` on
    /// every restart: each boot is a fresh deterministic scenario, and a
    /// client that survives the restart lands on a rewound axis. A nonzero
    /// value pins the anchor so every boot lands on the SAME axis - a
    /// restarted venue resumes at the sim instant a monotonic exchange would
    /// have reached, which is what lets a surviving worker ride through a
    /// venue bounce (see reference/clock.md, "Restarts"). Requires
    /// `sim_epoch_ns` to be set, and must not be in the future at boot.
    pub(crate) wall_anchor_ns: u64,
    /// Replay speed multiplier. `0.0` means unthrottled (stream as fast as the client
    /// drains). `1.0` is the default and paces to real wall-clock gaps; otherwise
    /// inter-tick wall delay = (tick gap) / speed.
    pub(crate) speed: f64,
    /// Maximum wall-clock sleep between two ticks under paced replay, in
    /// milliseconds. `0` disables the cap.
    pub(crate) gap_cap_ms: u64,
    /// Optional server-originated heartbeat cadence in milliseconds. `0`
    /// disables it. When enabled, each websocket session receives liveness
    /// frames that survive `StallData` but not `GoDark`.
    pub(crate) server_heartbeat_ms: u64,
    /// How far before sim-now the synthetic tape begins, in nanoseconds. The boot
    /// `data_origin = sim_now_at_boot - backfill_horizon_ns` is the earliest
    /// instant any source can serve, shared by every symbol's generator so the
    /// data timeline tracks the advertised clock (no frozen origin to drift past).
    /// A request straddling the floor is refused loudly rather than served short,
    /// so this default need not be exact - 24h covers a day's warmup. Documented
    /// next to `sim_epoch_ns`: both anchor the run's timelines.
    pub(crate) backfill_horizon_ns: u64,
    /// Global ceiling on concurrently-live per-symbol replay streams across
    /// every websocket connection. Each subscribed symbol runs on its own OS
    /// thread (see `spawn_replay`), so without a ceiling the aggregate thread
    /// count is `connections * subscribed-symbols` - entirely client-driven,
    /// and a fleet of connections each subscribing the whole catalog can
    /// exhaust the process thread limit (S22a). A subscribe that would exceed
    /// this cap is refused for the over-limit symbols with a `ProtocolError`
    /// diagnostic on the wire, exactly like an unservable subscribe; the
    /// connection stays up and its already-running streams are untouched. `0`
    /// disables the cap (unbounded), matching the `0`-disables convention of
    /// `gap_cap_ms` and `server_heartbeat_ms`.
    pub(crate) max_concurrent_replays: usize,
    /// Per-connection byte ceiling on execution output that has been produced
    /// but not yet written to the socket, i.e. the HELD lane's budget. See
    /// `admission::EXEC_HELD_BUDGET_BYTES`, which is this field's default and
    /// the shipped value.
    ///
    /// Configurable because the venue's own refusal behavior is otherwise
    /// unreachable: at 8 MiB, reaching a refused reservation over a real socket
    /// means driving megabytes of engine output through a stalled connection,
    /// which is a load generator, not a gate. A small budget puts the same
    /// branch one order away. Operators have a legitimate use too - a host
    /// running many connections bounds its aggregate exposure here, since the
    /// budget is per connection and the process-wide ceiling is this times the
    /// connection count.
    pub(crate) exec_held_budget_bytes: usize,
    /// Per-connection ceiling on QUEUED priority (admission-truth) frames. See
    /// `admission::ADMISSION_LANE_FRAMES`. Same reasoning as
    /// `exec_held_budget_bytes`: the overload close is only reachable in a test
    /// when this can be made small.
    pub(crate) admission_lane_frames: usize,
    /// Per-connection pool of outstanding PROMISES of a future priority frame,
    /// one per live replay, accounted separately from queue depth. See
    /// `admission::ADMISSION_PROMISE_TICKETS`.
    pub(crate) admission_promise_tickets: usize,
    /// Optional explicit venue instrument set. When empty, the server seeds the
    /// protocol default set. When present, this is authoritative for
    /// `/instruments`, order validation, and data generation.
    #[serde(rename = "instrument")]
    pub(crate) instruments: Vec<ConfiguredInstrument>,
    /// Initial per-currency account funding, currency -> amount (a decimal
    /// string, like the instrument increments). The venue's equivalent of a
    /// deposit made before the run: the ledger only ever books fill deltas, so
    /// an unfunded account goes negative on its first buy - which a nautilus
    /// CASH account (the adapter's default) refuses to apply, silently
    /// desyncing the consumer from the venue. An ABSENT table keeps the funded
    /// built-in default (matching the committed mogwai.toml); an explicitly
    /// EMPTY `[balances]` table runs the account unfunded on purpose.
    pub(crate) balances: HashMap<String, Decimal>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sim_epoch_ns: 0,
            wall_anchor_ns: 0,
            // Honest-by-default: wall-clock pace the generator's inter-arrival
            // gaps so a no-config server serves a realistic live feed, matching
            // the committed mogwai.toml. 0.0 remains available as an explicit
            // firehose for fast local iteration. Until the coherent simulated
            // clock lands this is the 1x baseline; afterwards it is the 1x point
            // of the acceleration axis.
            speed: 1.0,
            gap_cap_ms: 1000,
            server_heartbeat_ms: 0,
            // 24h: one day of warmup tape behind sim-now. A wrong horizon is made
            // loud (off-tape requests are refused, not silently under-served), so
            // the default's exactness is low-stakes.
            backfill_horizon_ns: 86_400_000_000_000,
            // 1024 concurrent replay threads: generous for the intended
            // single-broadarrow-node deployment (one node subscribes a handful
            // of symbols) while still bounding a runaway fan-out well under a
            // typical process thread limit. Operators driving many connections
            // against a large catalog raise it; `0` lifts it entirely.
            max_concurrent_replays: 1024,
            exec_held_budget_bytes: crate::admission::EXEC_HELD_BUDGET_BYTES,
            admission_lane_frames: crate::admission::ADMISSION_LANE_FRAMES,
            admission_promise_tickets: crate::admission::ADMISSION_PROMISE_TICKETS,
            instruments: Vec::new(),
            balances: default_balances(),
        }
    }
}

/// The funded built-in default: 1,000,000 USDT, the quote currency of the
/// built-in BTCUSDT instrument. Mirrors the committed mogwai.toml so a
/// no-config checkout serves an account that can actually trade; operators
/// running a custom instrument set fund their own quote currencies via the
/// `[balances]` table (or set it empty to run unfunded deliberately).
fn default_balances() -> HashMap<String, Decimal> {
    HashMap::from([("USDT".to_string(), Decimal::from(1_000_000))])
}

/// Warns at boot about funding gaps the funded-account enforcement will turn
/// into rejections. A funded venue refuses any order its free balance cannot
/// cover, so an instrument whose QUOTE currency carries no funding can never
/// buy - every order on it rejects with "insufficient balance", and without
/// this warning the first sign is a rejected order minutes into a run. (Base
/// funding is only needed by sell-first strategies, so its absence is not
/// warned - a long-only run acquires base through its own buys.) The
/// deliberately unfunded account gets one warning stating the consequence
/// instead: funds are unenforced, and the first buy books a negative quote
/// leg a nautilus cash consumer will refuse to apply.
pub(crate) fn warn_unfunded_quotes(cfg: &Config, defs: &[InstrumentDef]) {
    if cfg.balances.is_empty() {
        tracing::warn!(
            "account is UNFUNDED (empty balances table): funds checks are off, and the \
             first buy drives the quote leg negative - a nautilus cash account will \
             refuse every snapshot after it"
        );
        return;
    }
    for def in defs {
        if !cfg.balances.contains_key(&def.quote) {
            tracing::warn!(
                symbol = %def.symbol,
                quote = %def.quote,
                "instrument quote currency is unfunded; every buy on it will be \
                 rejected with insufficient balance - add it to [balances]"
            );
        }
    }
}

/// Validates the `[balances]` funding table: currencies must be non-blank,
/// within `MAX_CURRENCY_LEN`, and amounts non-negative. Zero is allowed (it
/// pins a currency into every snapshot without funding it); negative initial
/// funding has no venue meaning and is refused at startup like every other
/// malformed knob.
///
/// The length cap is what makes `sizing::BALANCE_ROW_MAX_BYTES` an upper bound:
/// a configured currency reaches the wire on every `AccountState` balance row,
/// and the connection's admission reservation is sized against that constant.
/// Operator config fails STARTUP rather than a connection, so the venue never
/// runs in a state where its own reservations under-count.
pub(crate) fn validate_balances(cfg: &Config) -> anyhow::Result<()> {
    for (currency, amount) in &cfg.balances {
        if currency.trim().is_empty() {
            anyhow::bail!("balances currency must not be blank");
        }
        if currency.len() > mogwai_protocol::MAX_CURRENCY_LEN {
            anyhow::bail!(
                "balances currency {currency} exceeds MAX_CURRENCY_LEN ({})",
                mogwai_protocol::MAX_CURRENCY_LEN
            );
        }
        if *amount < Decimal::ZERO {
            anyhow::bail!("balances.{currency} must not be negative");
        }
    }
    Ok(())
}

/// Validates the three admission budgets an operator can set.
///
/// A budget of zero refuses every command the venue will ever be sent, which is
/// not a small venue but a dead one; a held budget below
/// `sizing::BOUNDARY_REFUSAL_BYTES` cannot even reserve the single frame a
/// malformed-order refusal produces, so every order-entry command - valid or
/// not - would come back as an admission refusal. Both are misconfigurations an
/// operator would otherwise discover only as a venue that answers nothing, so
/// they fail STARTUP instead.
///
/// The floor binds operator config, not the type: the server's own tests
/// construct `Config` directly with budgets deliberately below it, which is how
/// a refused reservation and a saturated priority lane are reached over a real
/// socket at all.
pub(crate) fn validate_admission_limits(cfg: &Config) -> anyhow::Result<()> {
    if cfg.exec_held_budget_bytes < mogwai_protocol::sizing::BOUNDARY_REFUSAL_BYTES {
        anyhow::bail!(
            "exec_held_budget_bytes must be at least {} - the worst case of a single \
             boundary refusal - or every order-entry command is refused for capacity",
            mogwai_protocol::sizing::BOUNDARY_REFUSAL_BYTES
        );
    }
    if u32::try_from(cfg.exec_held_budget_bytes).is_err() {
        anyhow::bail!("exec_held_budget_bytes must fit in 32 bits");
    }
    if cfg.admission_lane_frames == 0 {
        anyhow::bail!("admission_lane_frames must be at least 1");
    }
    if cfg.admission_promise_tickets == 0 {
        anyhow::bail!("admission_promise_tickets must be at least 1");
    }
    Ok(())
}

/// The per-connection budget sizes every websocket session is built with.
pub(crate) fn build_admission_limits(cfg: &Config) -> AdmissionLimits {
    AdmissionLimits {
        held_budget_bytes: cfg.exec_held_budget_bytes,
        lane_frames: cfg.admission_lane_frames,
        promise_tickets: cfg.admission_promise_tickets,
    }
}

impl Config {
    /// Load run config from a TOML file. `path` is the parsed `--config <path>`
    /// argument when passed, otherwise `mogwai.toml` in the working directory.
    /// A missing file yields built-in defaults so the server still starts with
    /// no config present; a malformed file is a hard error rather than a silent
    /// fallback. Replaces the former MOGWAI_REPLAY_SPEED and MOGWAI_GAP_CAP_MS
    /// environment variables - run knobs belong in explicit input, not ambient
    /// environment.
    pub(crate) fn load(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let path = path.unwrap_or_else(|| PathBuf::from("mogwai.toml"));
        let cfg: Self = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        // Validate here, not at the call site, so a validated `Config` is the
        // only kind `load` ever hands out - a future second consumer cannot
        // forget the check. (The clock and instrument validations stay with
        // their builders: they need boot-time inputs this loader lacks.)
        validate_balances(&cfg)?;
        validate_admission_limits(&cfg)?;
        Ok(cfg)
    }
}

/// Build the global replay-stream permit pool from the configured cap. A cap of
/// `0` maps to `Semaphore::MAX_PERMITS`, i.e. effectively unbounded, so the
/// acquire path stays uniform (a permit is always taken; unlimited just never
/// runs dry) rather than branching on an `Option` at every subscribe. See
/// `Config::max_concurrent_replays` for the rationale.
pub(crate) fn build_replay_permits(cfg: &Config) -> Arc<Semaphore> {
    let permits = if cfg.max_concurrent_replays == 0 {
        Semaphore::MAX_PERMITS
    } else {
        cfg.max_concurrent_replays
    };
    Arc::new(Semaphore::new(permits))
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct ConfiguredInstrument {
    #[serde(flatten)]
    pub(crate) def: InstrumentDef,
    pub(crate) generator: mogwai_data::GeneratorScalars,
    pub(crate) session: mogwai_data::SessionProfile,
}

pub(crate) fn build_instrument_profiles(
    cfg: &Config,
) -> anyhow::Result<source::InstrumentProfiles> {
    if cfg.instruments.is_empty() {
        return Ok(source::InstrumentProfiles::defaults());
    }

    let fp = mogwai_data::Fingerprint::from_repo_json();
    let mut seen = HashSet::new();
    let mut profiles = Vec::with_capacity(cfg.instruments.len());

    for configured in &cfg.instruments {
        let def = &configured.def;
        validate_instrument_def(def)?;

        if !seen.insert(def.symbol.clone()) {
            anyhow::bail!("duplicate instrument symbol {}", def.symbol);
        }

        let mut scalars = configured.generator.clone();
        scalars.symbol = def.symbol.clone();
        if scalars.modal_tick != def.price_increment {
            anyhow::bail!(
                "instrument {} generator.modal_tick must equal price_increment",
                def.symbol
            );
        }
        if scalars.price_decimals != u32::from(def.price_precision) {
            anyhow::bail!(
                "instrument {} generator.price_decimals must equal price_precision",
                def.symbol
            );
        }
        scalars.validate(&fp).map_err(|err| {
            anyhow::anyhow!(
                "instrument {} generator.{} failed validation",
                def.symbol,
                err.field
            )
        })?;
        configured
            .session
            .validate()
            .map_err(|err| anyhow::anyhow!(session_error_message(&def.symbol, err)))?;

        profiles.push(source::InstrumentProfile::new(
            def.clone(),
            scalars,
            configured.session.clone(),
        ));
    }

    Ok(source::InstrumentProfiles::from_profiles(profiles))
}

pub(crate) fn validate_instrument_def(def: &InstrumentDef) -> anyhow::Result<()> {
    if def.symbol.trim().is_empty() {
        anyhow::bail!("instrument symbol must not be empty");
    }
    if def.base.trim().is_empty() {
        anyhow::bail!("instrument {} base must not be empty", def.symbol);
    }
    if def.quote.trim().is_empty() {
        anyhow::bail!("instrument {} quote must not be empty", def.symbol);
    }
    // Symbol, base and quote all reach the wire - the symbol on every tick,
    // order event and position row, the currencies on every balance row - so
    // the sizing constants the admission reservation is built on are only upper
    // bounds if the configured strings are capped. Startup is the right place
    // to refuse: a connection can then never out-produce its own reservation.
    if def.symbol.len() > mogwai_protocol::MAX_SYMBOL_LEN {
        anyhow::bail!(
            "instrument {} symbol exceeds MAX_SYMBOL_LEN ({})",
            def.symbol,
            mogwai_protocol::MAX_SYMBOL_LEN
        );
    }
    if def.base.len() > mogwai_protocol::MAX_CURRENCY_LEN
        || def.quote.len() > mogwai_protocol::MAX_CURRENCY_LEN
    {
        anyhow::bail!(
            "instrument {} base/quote exceeds MAX_CURRENCY_LEN ({})",
            def.symbol,
            mogwai_protocol::MAX_CURRENCY_LEN
        );
    }
    if def.price_increment <= Decimal::ZERO {
        anyhow::bail!("instrument {} price_increment must be positive", def.symbol);
    }
    if def.size_increment <= Decimal::ZERO {
        anyhow::bail!("instrument {} size_increment must be positive", def.symbol);
    }
    if !on_increment(
        def.price_increment,
        Decimal::new(1, u32::from(def.price_precision)),
    ) {
        anyhow::bail!(
            "instrument {} price_increment violates price_precision",
            def.symbol
        );
    }
    if !on_increment(
        def.size_increment,
        Decimal::new(1, u32::from(def.size_precision)),
    ) {
        anyhow::bail!(
            "instrument {} size_increment violates size_precision",
            def.symbol
        );
    }
    Ok(())
}

fn on_increment(value: Decimal, increment: Decimal) -> bool {
    increment > Decimal::ZERO && (value / increment).fract() == Decimal::ZERO
}

/// Render a `SessionProfileError` for the operator. `mogwai_data` reuses the
/// element `index` field with an `usize::MAX` sentinel to mean a whole-array
/// normalization (sum) violation, which has no single offending element - so the
/// per-element "session.intensity_hour[N] must be finite and > 0" message would
/// otherwise print the raw sentinel as `intensity_hour[18446744073709551615]`.
/// Branch on the sentinel to tell the sum story honestly (F14); every genuine
/// per-element failure keeps the indexed message.
pub(crate) fn session_error_message(symbol: &str, err: mogwai_data::SessionProfileError) -> String {
    if err.index == usize::MAX {
        format!(
            "instrument {symbol} session.{} does not sum to a valid normalization",
            err.field
        )
    } else {
        format!(
            "instrument {symbol} session.{}[{}] must be finite and > 0",
            err.field, err.index
        )
    }
}

/// Nanoseconds since the Unix epoch - the server's clock, fed into the engine.
///
/// Thin local alias over [`mogwai_protocol::now_unix_nanos`], the shared
/// saturating clock reader the adapter also uses: a backward clock step
/// (NTP/leap) that puts `now` before the epoch saturates to 0 rather than
/// panicking every order path and divergence arm, and the `u128` nanosecond
/// count is clamped to `u64::MAX` rather than silently truncated. Kept as a
/// local name so the call sites below stay unchanged.
pub(crate) fn now_ns() -> u64 {
    mogwai_protocol::now_unix_nanos()
}

pub(crate) fn sim_now_ns(sim: SimClock) -> u64 {
    sim.sim_ns(now_ns())
}

pub(crate) fn sim_duration_from_millis(ms: u64) -> u64 {
    ms.saturating_mul(1_000_000)
}

/// Convert a millisecond window into an absolute sim-time unix-ns deadline.
pub(crate) fn window_until_ns(now: u64, ms: u64) -> u64 {
    now.saturating_add(ms.saturating_mul(1_000_000))
}

/// Derives the run's `SimClock` from config. `boot_wall_ns` is the wall
/// instant of this boot; it anchors the clock unless the config pins
/// `wall_anchor_ns` explicitly. The pinned form is what makes sim time
/// monotonic ACROSS restarts: with the default boot anchor, every boot
/// re-anchors and so rewinds sim-now back to `sim_epoch_ns`, while a pinned
/// anchor puts every boot on the same affine axis (reference/clock.md,
/// "Restarts"). A pinned anchor in the future is refused rather than served:
/// `sim_ns` clamps pre-anchor reads to the epoch, so the venue would sit
/// frozen at `sim_epoch_ns` until the wall catches up - a misconfiguration
/// (most likely a seconds-vs-nanos slip), not a schedulable start.
pub(crate) fn build_sim_clock(cfg: &Config, boot_wall_ns: u64) -> anyhow::Result<SimClock> {
    if !cfg.speed.is_finite() {
        anyhow::bail!("speed must be finite");
    }
    if cfg.sim_epoch_ns == 0 {
        if cfg.wall_anchor_ns != 0 {
            anyhow::bail!(
                "wall_anchor_ns requires sim_epoch_ns (the identity clock has no anchor)"
            );
        }
        if cfg.speed != 1.0 && cfg.speed != 0.0 {
            anyhow::bail!("sim_epoch_ns must be set when speed is neither 0.0 nor 1.0");
        }
        return Ok(SimClock::identity());
    }
    if cfg.speed <= 0.0 {
        anyhow::bail!("speed must be > 0.0 when sim_epoch_ns is set");
    }
    let wall_anchor_ns = if cfg.wall_anchor_ns == 0 {
        boot_wall_ns
    } else {
        if cfg.wall_anchor_ns > boot_wall_ns {
            anyhow::bail!(
                "wall_anchor_ns {} is in the future (wall now {}); \
                 a pinned anchor must be a past instant",
                cfg.wall_anchor_ns,
                boot_wall_ns
            );
        }
        cfg.wall_anchor_ns
    };
    Ok(SimClock {
        sim_epoch_ns: cfg.sim_epoch_ns,
        wall_anchor_ns,
        speed: cfg.speed,
    })
}
