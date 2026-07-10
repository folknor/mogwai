//! Run configuration: the `mogwai.toml` schema, instrument-profile
//! construction/validation, and the sim-clock derivation that reads it. Split
//! out of `main.rs` because these are the load-time knobs the rest of the
//! server (the HTTP routes, the websocket replay) only ever reads back
//! through `AppState`/`SimClock`, never mutates.

use std::{collections::HashSet, path::PathBuf, sync::Arc};

use mogwai_protocol::{InstrumentDef, SimClock};
use rust_decimal::Decimal;
use tokio::sync::Semaphore;

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
    /// Optional explicit venue instrument set. When empty, the server seeds the
    /// protocol default set. When present, this is authoritative for
    /// `/instruments`, order validation, and data generation.
    #[serde(rename = "instrument")]
    pub(crate) instruments: Vec<ConfiguredInstrument>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            sim_epoch_ns: 0,
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
            instruments: Vec::new(),
        }
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
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(toml::from_str(&text)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
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

pub(crate) fn build_sim_clock(cfg: &Config, wall_anchor_ns: u64) -> anyhow::Result<SimClock> {
    if !cfg.speed.is_finite() {
        anyhow::bail!("speed must be finite");
    }
    if cfg.sim_epoch_ns == 0 {
        if cfg.speed != 1.0 && cfg.speed != 0.0 {
            anyhow::bail!("sim_epoch_ns must be set when speed is neither 0.0 nor 1.0");
        }
        return Ok(SimClock::identity());
    }
    if cfg.speed <= 0.0 {
        anyhow::bail!("speed must be > 0.0 when sim_epoch_ns is set");
    }
    Ok(SimClock {
        sim_epoch_ns: cfg.sim_epoch_ns,
        wall_anchor_ns,
        speed: cfg.speed,
    })
}
