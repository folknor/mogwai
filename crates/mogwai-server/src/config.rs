// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Run configuration: the `mogwai.toml` schema, instrument-profile
//! construction/validation, and the sim-clock derivation that reads it. Split
//! out of `main.rs` because these are the load-time knobs the rest of the
//! server (the HTTP routes, the websocket replay) only ever reads back
//! through `AppState`/`SimClock`, never mutates.

use std::{collections::HashMap, path::PathBuf};

use mogwai_protocol::{InstrumentDef, MarketRegime, SimClock};
use rust_decimal::Decimal;

use crate::admission::AdmissionLimits;
use crate::source;

/// Replay/runtime configuration, loaded from a TOML config file at startup
/// (see `load`); never from ambient environment variables.
#[derive(Debug, Clone, serde::Deserialize)]
// `deny_unknown_fields` makes config.md's "a malformed file is a hard error"
// promise literally true for top-level knobs: a typo'd key (`gap_cap_m = 0`)
// used to fall through to the field default silently, so the operator ran with a
// value they never set (S20). `default` (missing keys keep built-in defaults) and
// `deny` (unknown keys are rejected) are orthogonal and compose. Each
// `[[instrument]]` table is guarded the same way, by `ConfiguredInstrument`.
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    /// Simulated duration of one venue run. Zero means the launcher owns
    /// shutdown; a non-zero duration is announced as a clean completion.
    pub(crate) run_duration_ns: u64,
    /// Trades that must print THROUGH a resting limit's price before the venue
    /// fills it. `0` - the default - is the bookless venue's historical
    /// behaviour: a limit fills on submit at its own price, untouched by the
    /// tape. Any positive value turns resting limits into orders the market has
    /// to come to, gated on TRADED prices rather than quotes, because this
    /// venue's corpus, generator and `/quotes` surface are all trades-only.
    pub(crate) penetration_ticks: u32,
    /// How often, in sim milliseconds, the RUN re-checks its resting limits
    /// against the tape. Read only when `penetration_ticks > 0`. `0` disables
    /// the sweep, and with the sweep off a gated resting order can NEVER fill:
    /// a submit seeds only its own order, so nothing else ever advances a
    /// penetration count. Boot refuses that combination rather than shipping a
    /// venue that accepts limits it will never execute.
    pub(crate) fill_sweep_interval_ms: u64,
    /// Simulated start instant. `0` keeps the identity wall-time clock.
    pub(crate) sim_epoch_ns: u64,
    /// Wall instant the accelerated clock anchors to. `0` (the default)
    /// anchors at this run's boot, so every run is a fresh deterministic
    /// scenario starting at `sim_epoch_ns`. A nonzero value pins the anchor, so
    /// separate runs launched at different wall instants land on the SAME
    /// affine axis - which is how two runs are made comparable, not how one
    /// venue resumes: a process serves one run and there is no restart.
    /// Requires `sim_epoch_ns` to be set, and must not be in the future at
    /// boot.
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
    /// Simulated history generated eagerly at boot, in nanoseconds.
    /// `data_origin = run_start_ns - warmup_ns` is the earliest instant the
    /// tape can serve, and the whole span is MATERIALIZED before the readiness
    /// record is written (see `source::materialize_warmup`) rather than merely
    /// permitted. A request below the floor is refused loudly rather than
    /// served short, so this default need not be exact - 24h covers a day's
    /// warmup. Formerly `backfill_horizon_ns`, which bounded what a client was
    /// allowed to ASK for; with warmup declared and generated those are the
    /// same number.
    pub(crate) warmup_ns: u64,
    /// Depth of each tape's bounded broadcast ring, in pre-serialized frames.
    /// A subscriber that falls further behind than this has its CONNECTION
    /// KILLED as a venue fault (`admission::CLOSE_VENUE_FAULT`), because the
    /// venue has lost market data it already promised to deliver in ascending
    /// order and an unarmed hole is not something this venue serves. Stalling
    /// the tape instead is not an option either - every other subscriber on the
    /// symbol shares it.
    ///
    /// The default 4096 is roughly eight simulated hours at the tape's mean
    /// cadence, so on a loopback deployment this is unreachable short of a
    /// client stalling for hours: it is a backstop against a wedged consumer,
    /// NOT a tuning knob for a modeled pathology. Ordinary slow-consumer
    /// behavior belongs to the armed havoc surfaces instead.
    /// UNLIKE every neighbouring count knob, `0` is NOT "unbounded" here:
    /// `broadcast::channel(0)` panics, so `validate()` rejects it at load.
    pub(crate) fanout_depth: usize,
    /// How long a `speed = 0` tape parks waiting for ring headroom before
    /// giving up on its slowest subscriber and letting that subscriber lag.
    /// Only consulted when `speed == 0.0`, where the throttle moves from the
    /// connection to the tape: long enough that a healthy in-process client is
    /// never the reason a firehose stalls, short enough that a dead client
    /// costs one stall and is then refused.
    pub(crate) zero_speed_stall_ms: u64,
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
    /// Per-connection ceiling on order commands detached by an armed
    /// `CommandLatency` act delay and not yet acted on. One COMMAND, not one
    /// payload. See `admission::PENDING_ACT_SLOTS`; lowering it is how the smoke
    /// test reaches the refusal.
    pub(crate) pending_command_acts: usize,
    /// Process-wide ceiling on the same across every websocket connection. See
    /// `admission::GLOBAL_PENDING_ACT_SLOTS`.
    pub(crate) global_pending_command_acts: usize,
    /// The one instrument this run serves. Absent, the server seeds the
    /// built-in default profile. Present, it is authoritative for
    /// `/instruments`, order validation, and data generation. A table, not a
    /// list: a run serves exactly one instrument, so `[[instrument]]` fails to
    /// parse rather than silently serving whichever entry sorted first.
    #[serde(rename = "instrument")]
    pub(crate) instrument: Option<ConfiguredInstrument>,
    /// Market regime for this run's tape. Formerly the one knob a consumer
    /// picked for itself per subscription; with no subscriptions left it is
    /// boot config, chosen by whoever launches the run. Absent means the
    /// generator's unmodified baseline.
    pub(crate) regime: Option<MarketRegime>,
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
            run_duration_ns: 0,
            penetration_ticks: 0,
            fill_sweep_interval_ms: 100,
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
            warmup_ns: 86_400_000_000_000,
            // ~8 simulated hours of the default cadence, ~5 wall minutes at
            // speed 100. A subscriber that cannot keep up with 4096 queued
            // pre-serialized frames is not a subscriber whose feed is
            // meaningful.
            fanout_depth: 4096,
            zero_speed_stall_ms: 5000,
            exec_held_budget_bytes: crate::admission::EXEC_HELD_BUDGET_BYTES,
            admission_lane_frames: crate::admission::ADMISSION_LANE_FRAMES,
            pending_command_acts: crate::admission::PENDING_ACT_SLOTS,
            global_pending_command_acts: crate::admission::GLOBAL_PENDING_ACT_SLOTS,
            instrument: None,
            regime: None,
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

/// Refuses boot on funding gaps the funded-account enforcement would turn into
/// rejections. A funded venue refuses any order its free balance cannot
/// cover, so an instrument whose QUOTE currency carries no funding can never
/// buy - every order on it rejects with "insufficient balance", and without
/// this warning the first sign is a rejected order minutes into a run. (Base
/// funding is only needed by sell-first strategies, so its absence is not
/// warned - a long-only run acquires base through its own buys.) The
/// deliberately unfunded account gets one warning stating the consequence
/// instead: funds are unenforced, and the first buy books a negative quote
/// leg a nautilus cash consumer will refuse to apply.
pub(crate) fn refuse_unfunded_quotes(cfg: &Config, defs: &[InstrumentDef]) -> anyhow::Result<()> {
    if cfg.balances.is_empty() {
        tracing::warn!(
            "account is UNFUNDED (empty balances table): funds checks are off, and the \
             first buy drives the quote leg negative - a nautilus cash account will \
             refuse every snapshot after it"
        );
        return Ok(());
    }
    // A hard boot error rather than a warning: with ONE instrument per run, an
    // unfunded quote currency means every buy in the whole run rejects for
    // insufficient balance. That is a misconfigured run, not a caution, and it
    // is cheaper to refuse at boot than to discover it minutes in.
    for def in defs {
        if !cfg.balances.contains_key(&def.quote) {
            anyhow::bail!(
                "instrument {} quote currency {} is unfunded; every buy in this run \
                 would be rejected for insufficient balance - add {} to [balances]",
                def.symbol,
                def.quote,
                def.quote
            );
        }
    }
    Ok(())
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

/// Validates the admission budgets an operator can set.
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
    // A zero budget would refuse every delayed command, so the control could be
    // armed but never served.
    if cfg.pending_command_acts == 0 {
        anyhow::bail!("pending_command_acts must be at least 1");
    }
    // A global ceiling below the per-connection one would make the
    // per-connection budget unreachable, and therefore a lie.
    if cfg.global_pending_command_acts < cfg.pending_command_acts {
        anyhow::bail!("global_pending_command_acts must be at least pending_command_acts");
    }
    Ok(())
}

/// Above this no realistic tape ever fills a resting order and the venue would
/// silently be a black hole; refusing at boot says so out loud.
pub(crate) const MAX_PENETRATION_TICKS: u32 = 1_000;
/// An interval this slow is functionally the "sweep disabled" case and deserves
/// to be named as such rather than passing silently under the one-hour ceiling.
pub(crate) const SLOW_SWEEP_WARN_MS: u64 = 60_000;

/// Boot gate for the penetration knobs, alongside `validate_admission_limits`
/// and the admission limits.
pub(crate) fn validate_penetration(cfg: &Config) -> anyhow::Result<()> {
    if cfg.penetration_ticks > MAX_PENETRATION_TICKS {
        anyhow::bail!("penetration_ticks must be at most {MAX_PENETRATION_TICKS}");
    }
    if cfg.fill_sweep_interval_ms > mogwai_protocol::control::MAX_DIVERGENCE_MS {
        anyhow::bail!("fill_sweep_interval_ms must be at most MAX_DIVERGENCE_MS");
    }
    if cfg.penetration_ticks > 0 && cfg.fill_sweep_interval_ms == 0 {
        anyhow::bail!("fill_sweep_interval_ms must be > 0 when penetration_ticks is enabled");
    }
    if cfg.penetration_ticks > 0 && cfg.fill_sweep_interval_ms > SLOW_SWEEP_WARN_MS {
        tracing::warn!(
            interval_ms = cfg.fill_sweep_interval_ms,
            "penetration fill sweep interval is very slow"
        );
    }
    Ok(())
}

/// The per-connection budget sizes every websocket session is built with.
pub(crate) fn build_admission_limits(cfg: &Config) -> AdmissionLimits {
    AdmissionLimits {
        held_budget_bytes: cfg.exec_held_budget_bytes,
        lane_frames: cfg.admission_lane_frames,
        promise_tickets: crate::admission::ADMISSION_PROMISE_TICKETS,
        pending_act_slots: cfg.pending_command_acts,
    }
}

impl Config {
    /// Load run config from a TOML file. `path` is the parsed `--config <path>`
    /// argument when passed; omission uses built-in defaults. A requested file
    /// must exist and parse - consulting a cwd-relative `mogwai.toml` would
    /// make one run depend on its launcher's ambient working directory.
    /// Replaces the former MOGWAI_REPLAY_SPEED and MOGWAI_GAP_CAP_MS
    /// environment variables - run knobs belong in explicit input, not ambient
    /// environment.
    pub(crate) fn load(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let cfg: Self = match path {
            Some(path) => toml::from_str(&std::fs::read_to_string(path)?)?,
            None => Self::default(),
        };
        // Validate here, not at the call site, so a validated `Config` is the
        // only kind `load` ever hands out - a future second consumer cannot
        // forget the check. (The clock and instrument validations stay with
        // their builders: they need boot-time inputs this loader lacks.)
        validate_balances(&cfg)?;
        validate_admission_limits(&cfg)?;
        validate_penetration(&cfg)?;
        // Unlike every neighbouring count knob, 0 is not "unbounded" here:
        // `broadcast::channel(0)` panics, so the key is named in a load error
        // rather than crashing the first subscribe.
        if cfg.fanout_depth == 0 {
            anyhow::bail!(
                "fanout_depth must be greater than zero; unlike the count caps, 0 does not mean unbounded here"
            );
        }
        Ok(cfg)
    }
}

/// The `[instrument]` table: an `InstrumentDef` spelled out inline, plus its
/// generator and session profiles.
///
/// The def's seven fields are RESTATED here rather than pulled in with
/// `#[serde(flatten)]` because serde cannot combine `flatten` with
/// `deny_unknown_fields` - under the flattened form every unknown key in the
/// table was swallowed, so a typo'd `price_precison` silently ran the venue at
/// the field's default precision instead of failing the boot (the instrument
/// half of S20; the top-level knobs on `Config` were already guarded). Spelling
/// the fields out is what buys the `deny` below.
///
/// Drift between this list and `InstrumentDef` is compile-caught, not a
/// maintenance hazard: `def` builds the struct literal, so a field added
/// upstream fails to build here until it is mirrored.
///
/// NOTE the guard stops at this table's own keys. `generator` and `session`
/// deserialize into `GeneratorScalars` / `SessionProfile`, which are shared with
/// the committed fingerprint JSON parse and so are deliberately NOT denied here,
/// meaning a typo inside those sub-tables is still tolerated. Their VALUES are
/// validated at load (`build_instrument_profiles` runs `scalars.validate` and
/// `session.validate`), so the exposure is a silently defaulted field, not a
/// nonsense one.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConfiguredInstrument {
    pub(crate) symbol: mogwai_protocol::Symbol,
    pub(crate) base: String,
    pub(crate) quote: String,
    pub(crate) price_precision: u8,
    pub(crate) size_precision: u8,
    pub(crate) price_increment: Decimal,
    pub(crate) size_increment: Decimal,
    pub(crate) generator: mogwai_data::GeneratorScalars,
    pub(crate) session: mogwai_data::SessionProfile,
}

impl ConfiguredInstrument {
    /// The wire/engine-facing definition this table describes.
    pub(crate) fn def(&self) -> InstrumentDef {
        InstrumentDef {
            symbol: self.symbol.clone(),
            base: self.base.clone(),
            quote: self.quote.clone(),
            price_precision: self.price_precision,
            size_precision: self.size_precision,
            price_increment: self.price_increment,
            size_increment: self.size_increment,
        }
    }
}

pub(crate) fn build_instrument_profiles(
    cfg: &Config,
) -> anyhow::Result<source::InstrumentProfiles> {
    let Some(configured) = &cfg.instrument else {
        return Ok(source::InstrumentProfiles::defaults());
    };
    let fp = mogwai_data::Fingerprint::from_repo_json();
    let mut profiles = Vec::with_capacity(1);

    {
        let def = configured.def();
        validate_instrument_def(&def)?;

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
            def,
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
/// `wall_anchor_ns` explicitly. With the default boot anchor every run starts
/// at `sim_epoch_ns`; a pinned anchor instead puts every run launched against
/// it on the same affine axis, which is what makes two runs' timestamps
/// comparable. A pinned anchor in the future is refused rather than served:
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A run serves ONE instrument, so `[instrument]` is a table. A file
    /// carrying the old `[[instrument]]` list must fail loudly at parse rather
    /// than silently serving whichever entry happened to sort first.
    #[test]
    fn a_config_naming_two_instruments_fails_to_parse() {
        let two = "\n[[instrument]]\nsymbol = \"BTCUSDT\"\n[[instrument]]\nsymbol = \"ETHUSDT\"\n";
        let err = toml::from_str::<Config>(two).expect_err("a list of instruments is refused");
        assert!(
            err.to_string().contains("instrument"),
            "the parse error names the offending key: {err}"
        );
    }

    /// With one instrument per run, an unfunded quote currency means every buy
    /// in the whole run rejects for insufficient balance. That is a
    /// misconfigured run, so it fails BOOT rather than warning.
    #[test]
    fn an_unfunded_quote_currency_refuses_boot() {
        let cfg = Config {
            balances: HashMap::from([("EUR".to_string(), Decimal::from(1))]),
            ..Config::default()
        };
        let defs = mogwai_protocol::default_instruments();
        let err = refuse_unfunded_quotes(&cfg, &defs).expect_err("an unfunded quote refuses boot");
        assert!(err.to_string().contains("unfunded"), "{err}");

        refuse_unfunded_quotes(&Config::default(), &defs)
            .expect("the shipped defaults fund their own quote currency");
    }

    /// An empty `[balances]` table is the deliberate unfunded run, not a
    /// misconfiguration: funds checks are off entirely, so there is nothing to
    /// refuse.
    #[test]
    fn an_explicitly_unfunded_account_still_boots() {
        let cfg = Config {
            balances: HashMap::new(),
            ..Config::default()
        };
        refuse_unfunded_quotes(&cfg, &mogwai_protocol::default_instruments())
            .expect("an explicitly unfunded run is allowed");
    }
}
