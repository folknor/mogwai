// SPDX-FileCopyrightText: 2026 folknor
// SPDX-License-Identifier: AGPL-3.0-only

//! Run configuration: the `mogwai.toml` schema, instrument-profile
//! construction/validation, and the sim-clock derivation that reads it. Split
//! out of `main.rs` because these are the load-time knobs the rest of the
//! venue (the HTTP routes, the websocket replay) only ever reads back
//! through `AppState`/`SimClock`, never mutates.

use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::Context as _;

use mogwai_protocol::{InstrumentClass, InstrumentDef, MarketRegime, SimClock, WireAssetClass};
use rust_decimal::Decimal;

use crate::admission::AdmissionLimits;
use crate::source;

/// The shipped fanout ring depth, PER BOAT since piece 9 gave each boat its own
/// `broadcast::Sender`. Protocol 8 measured 1,048,576 as the smallest power of
/// two holding its worst p99.9 wall-second of frame work; the later 4,194,304
/// run-wide value was a sizing defect already mispriced at one boat, since a
/// tokio broadcast ring is eagerly allocated at roughly 40 bytes per slot and
/// that depth costs on the order of 170 MB - per boat, once boats are plural.
///
/// Public so the `ring_sizing` benchmark measures the depth that SHIPS rather
/// than a copy of it that can drift.
pub const DEFAULT_FANOUT_DEPTH: usize = 1_048_576;

/// Replay/runtime configuration, loaded from a TOML config file at startup
/// (see `load`); never from ambient environment variables.
#[derive(Debug, Clone, serde::Deserialize)]
// `deny_unknown_fields` makes config.md's "a malformed file is a hard error"
// promise literally true for top-level knobs: a typo'd key (`warmup_n = 0`)
// used to fall through to the field default silently, so the operator ran with a
// value they never set (S20). `default` (missing keys keep built-in defaults) and
// `deny` (unknown keys are rejected) are orthogonal and compose. The
// Instrument overlay keys are guarded one step later, by
// `deny_unknown_fields` on `ConfiguredInstrument`, which sees the RESOLVED
// table.
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Simulated duration of one venue run. Zero means the launcher owns
    /// shutdown; a non-zero duration is announced as a clean completion.
    pub(crate) run_duration_ns: u64,
    /// The account this run's single ledger is reported under.
    ///
    /// One venue is one run is one ledger, so this NAMES the account rather than
    /// selecting one - there is nothing to look up and nothing to refuse. It is
    /// configurable anyway because the consumer asserts it: a host holds an
    /// account of its own naming and compares it against what the venue reports,
    /// so a venue that insists on its own label is a venue that host cannot use.
    ///
    /// The default is `MOGWAI-001` and `validate_account_id` requires the
    /// `ISSUER-NUMBER` shape. That is a nautilus rule rather than a wire rule -
    /// `mogwai_protocol::AccountId` accepts a bare word quite happily - but
    /// mogwai is a nautilus venue, and an id nautilus cannot construct makes
    /// every run using it dead on arrival at the consumer. Refusing at boot
    /// costs a line; the alternative was discovered a minute into a run.
    pub(crate) account_id: String,
    /// Whether a returning client gets its ledger back, or a clean one.
    ///
    /// FALSE BY DEFAULT, meaning accounts PERSIST: a socket presenting an id
    /// that has traded resumes that ledger, with its positions and order history
    /// intact. That is what makes a reconnect a continuation rather than a new
    /// trader, and it is what the restart scenarios need - kill a worker holding
    /// a position, start it again, find the book where it was left.
    ///
    /// It is also a behaviour an operator must be TOLD about rather than
    /// discover, since a stale account silently inherited is a run measuring
    /// something nobody asked for. The readiness record carries it, so a
    /// launcher sees it without reading a log line.
    ///
    /// Set true and every connection opens a clean ledger under whatever id it
    /// names, which is what a batch that reuses ids across independent
    /// experiments wants.
    #[serde(default)]
    pub(crate) reset_account_on_reconnect: bool,
    /// How long an UNATTENDED account survives before the venue collects it, in
    /// WALL milliseconds. `0` (the default) means never.
    ///
    /// An account whose last connection went away freezes and is resumable, so
    /// a killed worker can come back to its own book. That also makes it state
    /// with no lifecycle: nothing else would ever remove it, and a long-lived
    /// shared exchange serving a batch of subagents would accumulate one ledger
    /// per id anybody ever presented, for the life of the process.
    ///
    /// WALL rather than simulated, because a frozen account has no simulated
    /// clock: the boat that carried one wound down with the last socket. The
    /// span an operator means here is "how long do I let a worker be down", which
    /// is a wall question anyway.
    ///
    /// Set it LONGER than the slowest restart any consumer performs. A collected
    /// account is gone: the next socket presenting that id opens a clean ledger,
    /// which is the correct behaviour and a surprising one to discover mid-run,
    /// so the readiness record carries the setting.
    #[serde(default)]
    pub(crate) account_ttl_ms: u64,
    pub(crate) oms_type: mogwai_protocol::OmsType,
    /// How many trailing-volatility horizons wide the fill band is. An order's
    /// trigger is drawn uniformly from `0 ..= band_ticks` ticks AWAY from its
    /// stated price, and `band_ticks` is this multiplier times the tape's
    /// realized volatility scaled to `FILL_HORIZON_NS`.
    ///
    /// `0.005` is the RAW-FILL-CADENCE calibration, selected by
    /// `fills::vol_probe`'s PROCEED rule - the smallest multiplier whose median
    /// implied band lands in the 3-to-100-tick usable window. On the committed
    /// BTCUSDT profile it reads a median implied band of 4 ticks and a p90 of 8.
    ///
    /// It replaces `0.5`, which was calibrated against the PRINT-layer tape where
    /// a 300 s window carried ~32 returns. The same window now carries ~15,700,
    /// so the estimator's horizon return rose by two orders of magnitude and
    /// `0.5` implied a median band of 439 ticks with a p90 of 703 - above the
    /// `fill_band_max_ticks` clamp of 200 at nearly every instant. A clamp-
    /// saturated band draws uniformly across the full clamp range regardless of
    /// what the tape is doing, which is the mirror image of the inert `u = 0`
    /// band: in neither case does the tape decide the fill.
    ///
    /// The probe is the provenance. Re-run it (`test -p mogwai-server vol_probe`
    /// in the focused runner) and read the selection off its table rather than
    /// trusting this comment if the fingerprint or the cadence moves again; the
    /// golden
    /// `tests/golden/fill_distribution.json` is blessed against whatever this
    /// default is and has to be re-blessed with it.
    ///
    /// The probe's OTHER reading is good news and closes an open item: cold-
    /// window refusals are 0 of 128 sampled instants, against a 29.5% refusal
    /// rate at the print-layer cadence.
    ///
    /// `0.0` is legal and gives the strict-through-at-the-stated-price venue.
    /// That is the DEGENERATE CASE of this model, not a compatibility mode:
    /// there is no switch that restores the counter it replaced.
    pub(crate) fill_band_vol_mult: f64,
    /// Ceiling on the drawn band, in ticks. Truncates a reading rather than the
    /// multiplier, so a genuine volatility spike can widen the band past its
    /// median while a mispriced estimate cannot make a fill a coin flip. The
    /// default `200` sits just above the 100-tick ceiling of usefulness, so it
    /// only ever truncates readings the calibration would already have rejected.
    pub(crate) fill_band_max_ticks: u32,
    /// How often, in sim milliseconds, the RUN re-checks its resting limits
    /// against the tape. Zero is refused because the fill band is always on and
    /// every resting trigger needs the sweep to advance.
    pub(crate) fill_sweep_interval_ms: u64,
    /// Run seed to reproduce. Absent, one pasteable 63-bit seed is drawn once
    /// at launch.
    pub(crate) seed: Option<u64>,
    /// Replay speed multiplier. `0.0` means unthrottled (stream as fast as the client
    /// drains). `1.0` is the default and paces to real wall-clock gaps; otherwise
    /// inter-tick wall delay = (tick gap) / speed.
    pub(crate) speed: f64,
    /// Optional venue-originated heartbeat cadence in milliseconds. `0`
    /// disables it. When enabled, each websocket session receives liveness
    /// frames that survive `StallData` but not `GoDark`. Formerly
    /// `server_heartbeat_ms`; the old key is no longer accepted.
    pub(crate) venue_heartbeat_ms: u64,
    /// Uniform servable simulated-history span, in nanoseconds.
    /// `data_origin = run_start_ns - warmup_ns` is the earliest instant the
    /// tape can serve. The boot river is materialized before readiness; every
    /// other river materializes the span on first read. A request below the
    /// floor is refused loudly rather than
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
    /// The default is a measured backstop against a wedged consumer, not a
    /// modeled pathology. Protocol 8's composition run raised it to 1,048,576:
    /// twice the worst measured p99.9 expansion of wall-second frame work over
    /// the prior 262,144, rounded up to a power of two. That holds 0.114 wall
    /// seconds at the worst measured rate, up from the 0.030 the prior value
    /// held, so the resize lengthens the horizon rather than shortening it.
    /// UNLIKE every neighbouring count knob, `0` is NOT "unbounded" here:
    /// `broadcast::channel(0)` panics, so `validate()` rejects it at load.
    pub(crate) fanout_depth: usize,
    // (see `DEFAULT_FANOUT_DEPTH` for the shipped value and its derivation)
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
    /// Per-connection ceiling on parsed order commands waiting for the one
    /// sequential dispatcher. One COMMAND, not one payload.
    pub(crate) pending_command_acts: usize,
    /// Process-wide ceiling on queued or executing commands across every
    /// websocket connection. See `admission::GLOBAL_PENDING_COMMAND_SLOTS`.
    pub(crate) global_pending_command_acts: usize,
    /// The symbol whose river is boarded before readiness and holds a boat for
    /// process life. Absent, the default bundle's symbol stands. It is also the
    /// request default: a socket or poll that names no symbol gets this one.
    pub symbol: Option<String>,
    /// Operator knobs applied to every resolved symbol, exactly as written.
    #[serde(rename = "instrument")]
    pub instrument: Option<toml::Table>,
    /// Operator knobs applied to individual symbols, exactly as written. Same
    /// overlay shape as `instrument`, including its own `preset` and `override`
    /// sub-table, and applied after it. Keyed case-insensitively.
    pub symbols: HashMap<String, toml::Table>,
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
    /// Named risk policies a client can ask for by name instead of restating.
    ///
    /// THE SAME IDEA AS AN INSTRUMENT PRESET: a named bundle of knobs a user
    /// could set by hand, carrying no authority and conferring no status. What
    /// differs is REGISTRATION. Instrument presets are compiled in, which is
    /// defensible while there are three of them; account policies track funded
    /// account programmes, of which there are hundreds, and their rules change
    /// without notice. Nobody can follow that in a release cycle, so these are
    /// read from the operator's config at boot and a registered name SHADOWS a
    /// shipped one.
    #[serde(default)]
    pub(crate) account_policies: HashMap<String, mogwai_protocol::risk::AccountPolicy>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            run_duration_ns: 0,
            account_id: DEFAULT_ACCOUNT_ID.to_owned(),
            reset_account_on_reconnect: false,
            // Never collected by default, which is the conservative direction:
            // an operator who has not thought about restart windows must not
            // lose a book to a timer they did not set.
            account_ttl_ms: 0,
            oms_type: mogwai_protocol::OmsType::Netting,
            fill_band_vol_mult: 0.005,
            fill_band_max_ticks: 200,
            fill_sweep_interval_ms: 100,
            seed: None,
            // Honest-by-default: wall-clock pace the generator's inter-arrival
            // gaps so a no-config venue serves a realistic live feed, matching
            // the committed mogwai.toml. 0.0 remains available as an explicit
            // firehose for fast local iteration. Until the coherent simulated
            // clock lands this is the 1x baseline; afterwards it is the 1x point
            // of the acceleration axis.
            speed: 1.0,
            venue_heartbeat_ms: 0,
            // 24h: one day of warmup tape behind sim-now. A wrong horizon is made
            // loud (off-tape requests are refused, not silently under-served), so
            // the default's exactness is low-stakes.
            warmup_ns: 86_400_000_000_000,
            // One ring PER BOAT. Protocol 8 measured 1,048,576 as the smallest
            // power of two holding its worst p99.9 wall-second frame work.
            // Keeping the later 4,194,304 run-wide allocation per boat would
            // multiply roughly 170 to 270 MiB of eager state by boat count.
            fanout_depth: DEFAULT_FANOUT_DEPTH,
            zero_speed_stall_ms: 5000,
            exec_held_budget_bytes: crate::admission::EXEC_HELD_BUDGET_BYTES,
            admission_lane_frames: crate::admission::ADMISSION_LANE_FRAMES,
            pending_command_acts: crate::admission::PENDING_COMMAND_SLOTS,
            global_pending_command_acts: crate::admission::GLOBAL_PENDING_COMMAND_SLOTS,
            symbol: None,
            instrument: None,
            symbols: HashMap::new(),
            regime: None,
            balances: default_balances(),
            account_policies: HashMap::new(),
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
///
/// It takes a configured shape's derived definition, not an unresolved config
/// table. Every explicitly configured shape is checked before the run boots.
pub(crate) fn refuse_unfunded_settlement(cfg: &Config, def: &InstrumentDef) -> anyhow::Result<()> {
    if cfg.balances.is_empty() {
        tracing::warn!(
            "account is UNFUNDED (empty balances table): funds checks are off, and the \
             first buy drives the quote leg negative - a nautilus cash account will \
             refuse every snapshot after it"
        );
        return Ok(());
    }
    // A hard boot error rather than a warning: an explicitly configured shape
    // that cannot buy is a misconfiguration, not a trading outcome.
    let currency = def.class.settlement_currency();
    if !cfg.balances.contains_key(currency) {
        anyhow::bail!(
            "instrument {} quote currency {} is unfunded; every buy in this run \
             would be rejected for insufficient balance - add {} to [balances]",
            def.symbol,
            currency,
            currency
        );
    }
    Ok(())
}

/// The account a run reports when its config does not name one.
///
/// `MOGWAI-001` rather than `MOGWAI` because nautilus parses an `AccountId` as
/// `ISSUER-NUMBER` and rejects a bare word. The venue's own wire type is happy
/// either way, which is exactly why this needs stating: nothing inside mogwai
/// notices the difference, and the consumer notices it as a run that cannot
/// start.
pub(crate) const DEFAULT_ACCOUNT_ID: &str = "MOGWAI-001";

/// Validates the run's account id.
///
/// Two rules. It must be a legal `mogwai_protocol::AccountId`, which is the wire
/// constraint. And it must carry an `ISSUER-NUMBER` split, which is NOT a wire
/// constraint but a nautilus one: `AccountId::new` panics on a value with no
/// `-`, so a venue reporting one produces a host that cannot construct the
/// account it is being told about. mogwai does not import nautilus here and so
/// checks the shape by hand; the alternative is a run that boots cleanly, serves
/// happily, and is refused by its consumer a minute later with an error naming
/// neither this file nor this key.
pub(crate) fn validate_account_id(cfg: &Config) -> anyhow::Result<()> {
    let id = cfg.account_id.trim();
    mogwai_protocol::AccountId::parse(id)
        .map_err(|err| anyhow::anyhow!("account_id {id:?} is not a legal account id: {err}"))?;
    let (issuer, number) = id.split_once('-').ok_or_else(|| {
        anyhow::anyhow!(
            "account_id {id:?} has no '-'. A nautilus AccountId is ISSUER-NUMBER, so a \
             consumer cannot construct this one - try {DEFAULT_ACCOUNT_ID}"
        )
    })?;
    if issuer.is_empty() || number.is_empty() {
        anyhow::bail!(
            "account_id {id:?} must have a non-empty issuer and number either side of the '-'"
        );
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
/// The floor binds operator config, not the type: the venue's own tests
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

/// An interval this slow is functionally the "sweep disabled" case and deserves
/// to be named as such rather than passing silently under the one-hour ceiling.
pub(crate) const SLOW_SWEEP_WARN_MS: u64 = 60_000;

/// Boot gate for the fill-band knobs, alongside `validate_admission_limits`.
///
/// The upper validation bounds are generous on purpose - an operator
/// deliberately exploring a pathological band is a legitimate experiment - while
/// the DEFAULTS are what have to be defensible. A zero sweep interval is refused
/// outright rather than warned about: the band is always on, and a venue that
/// accepts resting limits nothing will ever advance is a black hole.
pub(crate) fn validate_fill_band(cfg: &Config) -> anyhow::Result<()> {
    if !cfg.fill_band_vol_mult.is_finite() || !(0.0..=1_000.0).contains(&cfg.fill_band_vol_mult) {
        anyhow::bail!("fill_band_vol_mult must be finite and between 0 and 1000");
    }
    if !(1..=10_000).contains(&cfg.fill_band_max_ticks) {
        anyhow::bail!("fill_band_max_ticks must be between 1 and 10000");
    }
    if cfg.fill_sweep_interval_ms > mogwai_protocol::control::MAX_DIVERGENCE_MS {
        anyhow::bail!("fill_sweep_interval_ms must be at most MAX_DIVERGENCE_MS");
    }
    if cfg.fill_sweep_interval_ms == 0 {
        anyhow::bail!("fill_sweep_interval_ms must be > 0");
    }
    if cfg.fill_sweep_interval_ms > SLOW_SWEEP_WARN_MS {
        tracing::warn!(
            interval_ms = cfg.fill_sweep_interval_ms,
            "fill sweep interval is very slow"
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
    }
}

impl Config {
    /// The boot symbol the operator named, if any. Absent, the chosen bundle's
    /// own symbol stands, which is what makes a no-config run BTCUSDT.
    pub fn boot_symbol(&self) -> Option<&str> {
        self.symbol.as_deref()
    }

    /// True when no operator knobs apply to the boot symbol.
    pub fn boot_symbol_carries_no_knobs(&self) -> bool {
        self.overlays_for(self.boot_symbol()).is_empty()
    }

    /// Default knobs followed by the matching symbol-specific knobs, each
    /// paired with the config path it came from so a resolution error or an
    /// addition log can name the table the operator wrote. Empty tables are
    /// omitted and symbol matching is ASCII case-insensitive. This is the ONE
    /// spelling of which overlays a symbol resolves through: the boot guard
    /// and the resolution itself both ask it, so they cannot drift.
    pub(crate) fn overlays_for(&self, symbol: Option<&str>) -> Vec<(String, toml::Table)> {
        let mut overlays = Vec::new();
        if let Some(table) = self.instrument.clone().filter(|table| !table.is_empty()) {
            overlays.push(("instrument".to_owned(), table));
        }
        if let Some(symbol) = symbol
            && let Some((key, table)) = self
                .symbols
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case(symbol))
                .filter(|(_, table)| !table.is_empty())
        {
            overlays.push((format!("symbols.{key}"), table.clone()));
        }
        overlays
    }

    /// Load run config from a TOML file. `path` is the parsed `--config <path>`
    /// argument when passed; omission uses built-in defaults. A requested file
    /// must exist and parse - consulting a cwd-relative `mogwai.toml` would
    /// make one run depend on its launcher's ambient working directory.
    /// Replaces the former MOGWAI_REPLAY_SPEED and MOGWAI_GAP_CAP_MS
    /// environment variables - run knobs belong in explicit input, not ambient
    /// environment.
    pub fn load(path: Option<PathBuf>) -> anyhow::Result<Self> {
        let cfg: Self = match path {
            Some(path) => {
                let text = std::fs::read_to_string(path)?;
                let raw: toml::Table = toml::from_str(&text)?;
                toml::Value::Table(raw).try_into()?
            }
            None => Self::default(),
        };
        // Validate every knob this type owns here. The clock and resolved
        // instrument validations stay with their builders because they need
        // boot-time inputs; `build_instrument_profiles` owns symbol resolution
        // and validation of the operator's raw instrument table.
        validate_balances(&cfg)?;
        validate_account_id(&cfg)?;
        validate_admission_limits(&cfg)?;
        validate_fill_band(&cfg)?;
        validate_speed(&cfg)?;
        validate_symbol_keys(&cfg)?;
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

fn validate_speed(cfg: &Config) -> anyhow::Result<()> {
    if !cfg.speed.is_finite() || cfg.speed < 0.0 {
        anyhow::bail!("speed must be finite and non-negative");
    }
    Ok(())
}

/// The preset every unmatched symbol is served under. It is spot, makes no
/// calendar claim about an unfitted symbol, settles in the funded USDT default,
/// and was fitted from trade-level archives.
pub const DEFAULT_PRESET: &str = "BTCUSDT";
/// The shape an arbitrary unconfigured label resolves to under this config.
///
/// NO PROBE LABEL. Resolving the fallback under some sentinel string needs that
/// string to be wire-ILLEGAL to be collision-proof, and a wire-illegal symbol
/// does not survive `profile_from_configured`, which enforces `MAX_SYMBOL_LEN`
/// on the resolved def. A wire-legal sentinel would instead be nameable by a
/// client and shadowable by a `[symbols.*]` key, which is the collision the
/// sentinel existed to prevent.
///
/// `None` is exact and needs neither. `bundle_name` picks a bundle from the
/// symbol only when the symbol NAMES A PRESET, and `overlays_for` attaches a
/// `[symbols.*]` table only when one matches - so for a label that is neither,
/// resolution is `[instrument]` over the operator's preset or `DEFAULT_PRESET`,
/// which is precisely what `None` resolves. The symbol the shape ends up
/// wearing does not enter the settlement currency.
fn unconfigured_fallback_shape(cfg: &Config) -> anyhow::Result<source::InstrumentProfile> {
    profile_for(cfg, None)
}

/// The shipped presets. The one spelling of the registry: name, text.
const PRESETS: [(&str, &str); 3] = [
    ("MNQ", include_str!("../presets/mnq.toml")),
    ("MES", include_str!("../presets/mes.toml")),
    ("BTCUSDT", include_str!("../presets/btcusdt.toml")),
];

pub fn preset_names() -> [&'static str; 3] {
    PRESETS.map(|(name, _)| name)
}

fn preset_text(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_uppercase();
    PRESETS
        .iter()
        .find_map(|(candidate, text)| (*candidate == name).then_some(*text))
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Provenance {
    Fitted {
        corpus: String,
        window: String,
        #[serde(default)]
        accepted_diagnostics: Vec<String>,
    },
    Derived {
        from: Vec<String>,
        #[serde(default)]
        accepted_diagnostics: Vec<String>,
    },
    Declared {
        rationale: String,
        #[serde(default)]
        accepted_diagnostics: Vec<String>,
    },
}

impl Provenance {
    fn accepted_diagnostics(&self) -> &[String] {
        match self {
            Self::Fitted {
                accepted_diagnostics,
                ..
            }
            | Self::Derived {
                accepted_diagnostics,
                ..
            }
            | Self::Declared {
                accepted_diagnostics,
                ..
            } => accepted_diagnostics,
        }
    }
}

fn flatten_knobs(
    prefix: &str,
    table: &toml::Table,
    paths: &mut std::collections::BTreeSet<String>,
) {
    for (key, value) in table {
        if key == "preset" || key == "override" {
            continue;
        }
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if let Some(child) = value.as_table() {
            flatten_knobs(&path, child, paths);
        } else {
            paths.insert(path);
        }
    }
}

fn effective_preset(name: &str) -> anyhow::Result<(toml::Table, toml::Table)> {
    let text =
        preset_text(name).ok_or_else(|| anyhow::anyhow!("unknown instrument preset {name}"))?;
    let raw: toml::Table = toml::from_str(text)?;
    let mut instrument = raw
        .get("instrument")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| anyhow::anyhow!("preset {name} has no instrument table"))?
        .clone();
    let own_provenance = raw
        .get("provenance")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| anyhow::anyhow!("preset {name} has no provenance table"))?;
    let (mut merged, mut provenance) = if let Some(parent) = instrument.remove("preset") {
        let parent = parent
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("preset {name} instrument.preset must be a string"))?;
        effective_preset(parent)?
    } else {
        (toml::Table::new(), toml::Table::new())
    };
    let overrides = instrument
        .remove("override")
        .and_then(|value| value.as_table().cloned())
        .unwrap_or_default();
    for (key, value) in instrument {
        if merged.insert(key.clone(), value).is_some() {
            anyhow::bail!("preset {name} restates inherited key {key}; use instrument.override");
        }
    }
    for (path, value) in overrides {
        replace_dotted(&mut merged, &path, value)?;
    }
    for (path, value) in own_provenance {
        provenance.insert(path.clone(), value.clone());
    }
    validate_provenance(name, &merged, &provenance)?;
    Ok((merged, provenance))
}

fn validate_provenance(
    name: &str,
    instrument: &toml::Table,
    provenance: &toml::Table,
) -> anyhow::Result<()> {
    let mut knobs = std::collections::BTreeSet::new();
    flatten_knobs("", instrument, &mut knobs);
    let declared = provenance
        .keys()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    if knobs != declared {
        let missing = knobs.difference(&declared).cloned().collect::<Vec<_>>();
        let extra = declared.difference(&knobs).cloned().collect::<Vec<_>>();
        anyhow::bail!(
            "preset {name} provenance is incomplete: missing {missing:?}, extra {extra:?}"
        );
    }
    for (path, value) in provenance {
        let entry: Provenance = value.clone().try_into().map_err(|error| {
            anyhow::anyhow!("preset {name} provenance for {path} is invalid: {error}")
        })?;
        match &entry {
            Provenance::Fitted { corpus, window, .. }
                if corpus.trim().is_empty() || window.trim().is_empty() =>
            {
                anyhow::bail!("preset {name} provenance for {path} requires corpus and window")
            }
            Provenance::Derived { from, .. }
                if from.is_empty() || from.iter().any(|source| !knobs.contains(source)) =>
            {
                anyhow::bail!("preset {name} provenance for {path} has invalid derived sources")
            }
            Provenance::Declared { rationale, .. } if rationale.trim().is_empty() => {
                anyhow::bail!("preset {name} provenance for {path} requires a rationale")
            }
            _ => {}
        }
        if entry
            .accepted_diagnostics()
            .iter()
            .any(|code| code.trim().is_empty())
        {
            anyhow::bail!("preset {name} provenance for {path} accepts an empty diagnostic")
        }
    }
    Ok(())
}

#[cfg(test)]
fn accepted_preset_diagnostics(
    name: &str,
    provenance: &toml::Table,
) -> anyhow::Result<std::collections::BTreeSet<(String, String)>> {
    let mut accepted = std::collections::BTreeSet::new();
    for (path, value) in provenance {
        let entry: Provenance = value.clone().try_into().map_err(|error| {
            anyhow::anyhow!("preset {name} provenance for {path} is invalid: {error}")
        })?;
        for code in entry.accepted_diagnostics() {
            if code.trim().is_empty() {
                anyhow::bail!("preset {name} provenance for {path} accepts an empty diagnostic")
            }
            if !accepted.insert((path.clone(), code.clone())) {
                anyhow::bail!("preset {name} provenance for {path} accepts diagnostic {code} twice")
            }
        }
    }
    Ok(accepted)
}

#[cfg(test)]
fn assert_preset_diagnostics(
    name: &str,
    profile: &source::InstrumentProfile,
    fp: &mogwai_data::Fingerprint,
    provenance: &toml::Table,
) -> anyhow::Result<()> {
    let grid = mogwai_data::SizeGrid::from_def(&profile.def);
    let actual = profile
        .scalars
        .empirical_diagnostics(fp, grid.multiplier)
        .into_iter()
        .chain(
            profile
                .scalars
                .size_diagnostics(grid.min_size, grid.integral),
        )
        .map(|diagnostic| {
            (
                format!("generator.{}", diagnostic.field),
                diagnostic.code.to_string(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    let accepted = accepted_preset_diagnostics(name, provenance)?;
    if actual != accepted {
        let unaccepted = actual.difference(&accepted).cloned().collect::<Vec<_>>();
        let stale = accepted.difference(&actual).cloned().collect::<Vec<_>>();
        anyhow::bail!(
            "shipped preset {name} diagnostics disagree with provenance: unaccepted {unaccepted:?}, stale acceptances {stale:?}"
        );
    }
    Ok(())
}

pub fn preset_document(name: &str) -> Option<&'static str> {
    preset_text(name)
}

/// The preset a symbol and an operator choice select, by the three-step
/// precedence: the operator's explicit `preset`, then a preset whose name
/// matches the symbol, then [`DEFAULT_PRESET`]. TOTAL over symbol strings.
/// This is the ONE spelling of the precedence; `base_bundle` and every message
/// naming the chosen bundle read it from here so the two cannot disagree.
fn bundle_name<'a>(symbol: Option<&'a str>, operator_preset: Option<&'a str>) -> &'a str {
    operator_preset
        .or_else(|| symbol.filter(|symbol| preset_text(symbol).is_some()))
        .unwrap_or(DEFAULT_PRESET)
}

fn base_bundle(
    symbol: Option<&str>,
    operator_preset: Option<&str>,
) -> anyhow::Result<(toml::Table, toml::Table)> {
    effective_preset(bundle_name(symbol, operator_preset))
}

/// Resolves anonymous overlays - the outermost is the `[instrument]` default
/// table and any further one a per-symbol table whose key is not known here.
/// `Config`-driven resolution goes through `overlays_for`, which carries the
/// real path of each table.
fn resolve_instrument(
    symbol: Option<&str>,
    overlays: Vec<toml::Table>,
) -> anyhow::Result<toml::Table> {
    let named = overlays
        .into_iter()
        .enumerate()
        .map(|(index, overlay)| {
            let source = if index == 0 {
                "instrument"
            } else {
                "symbols.*"
            };
            (source.to_owned(), overlay)
        })
        .collect();
    resolve_instrument_named(symbol, named)
}

fn resolve_instrument_named(
    symbol: Option<&str>,
    overlays: Vec<(String, toml::Table)>,
) -> anyhow::Result<toml::Table> {
    // The pre-class shape was seven flat fields. `deny_unknown_fields` would
    // refuse it anyway, but with a bare "unknown field `base`" that names
    // neither the replacement nor the shape it takes. Refuse it here instead,
    // where the message can say what to write.
    let mut prepared = Vec::with_capacity(overlays.len());
    let mut operator_preset = None;
    for (source, mut overlay) in overlays {
        for removed in ["base", "quote"] {
            if overlay.contains_key(removed) {
                anyhow::bail!(
                    "{source}.{removed} was replaced by the [{source}.class] table; write \
                     kind = \"spot\" with base and quote under [{source}.class]"
                );
            }
        }
        if overlay.contains_key("symbol") {
            anyhow::bail!(
                "{source}.symbol was replaced by the top-level symbol key; the [instrument] \
                 and [symbols.*] tables carry knobs, not an instrument"
            );
        }
        if let Some(value) = overlay.remove("preset") {
            operator_preset = Some(
                value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("{source}.preset must be a string"))?
                    .to_owned(),
            );
        }
        prepared.push((source, overlay));
    }
    let requested_symbol = symbol.map(str::to_owned);
    let bundle = bundle_name(symbol, operator_preset.as_deref());
    let (mut merged, _provenance) = base_bundle(symbol, operator_preset.as_deref())?;
    for (source, overlay) in prepared {
        apply_overlay(&mut merged, overlay, bundle, symbol, &source)?;
    }
    if let Some(symbol) = requested_symbol {
        merged.insert("symbol".into(), toml::Value::String(symbol));
    }
    Ok(merged)
}

fn apply_overlay(
    merged: &mut toml::Table,
    mut operator: toml::Table,
    bundle: &str,
    requested: Option<&str>,
    source: &str,
) -> anyhow::Result<()> {
    let overrides = operator
        .remove("override")
        .and_then(|value| value.as_table().cloned())
        .unwrap_or_default();
    for (path, value) in overrides {
        // Protocol-12b's arrival seam is intentionally absent from every
        // shipped preset until Brick S.  It is nevertheless a valid
        // instrument-resolved override, unlike a misspelled path.
        if path == "generator.arrival"
            && !merged["generator"]
                .as_table()
                .is_some_and(|g| g.contains_key("arrival"))
        {
            merged
                .get_mut("generator")
                .and_then(toml::Value::as_table_mut)
                .expect("every preset has a generator table")
                .insert("arrival".to_string(), value);
            continue;
        }
        replace_dotted_for_bundle(merged, &path, value, bundle)?;
    }
    // A top-level key is the operator's explicit choice. It REPLACES a knob the
    // bundle sets, and where the bundle sets no such key it ADDS one: the
    // optional sections (`margin`, `fees`, `calendar`) are absent from most
    // bundles, and no shipped preset sets `fees` at all, so the
    // must-already-exist rule would make them unreachable from every config.
    // The typo guard survives without it - `deny_unknown_fields` on
    // `ConfiguredInstrument` refuses any key that is not a field, and a knob
    // that contradicts the bundle's class (a margin table over spot) is refused
    // by `validate_instrument_options`. A key spelled with a dot is a PATH,
    // as under `[instrument.override]`, and keeps the strict guard.
    for (path, value) in operator {
        if path.contains('.') || merged.contains_key(&path) {
            replace_dotted_for_bundle(merged, &path, value, bundle)?;
        } else {
            tracing::info!(path, override_value = %value, bundle, symbol = requested, overlay = source, "instrument bundle addition");
            merged.insert(path, value);
        }
    }
    Ok(())
}

fn replace_dotted_for_bundle(
    table: &mut toml::Table,
    path: &str,
    value: toml::Value,
    bundle_name: &str,
) -> anyhow::Result<()> {
    replace_dotted(table, path, value).map_err(|error| {
        anyhow::anyhow!("{error}; chosen bundle {bundle_name} does not include that knob")
    })
}

fn replace_dotted(table: &mut toml::Table, path: &str, value: toml::Value) -> anyhow::Result<()> {
    let mut parts = path.split('.').peekable();
    let mut current = table;
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            let old = current
                .get_mut(part)
                .ok_or_else(|| anyhow::anyhow!("preset does not set override path {path}"))?;
            tracing::info!(path, preset_value = %old, override_value = %value, "instrument preset override");
            *old = value;
            return Ok(());
        }
        current = current
            .get_mut(part)
            .and_then(toml::Value::as_table_mut)
            .ok_or_else(|| anyhow::anyhow!("preset does not set override path {path}"))?;
    }
    anyhow::bail!("empty preset override path")
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
pub struct ConfiguredInstrument {
    pub(crate) symbol: mogwai_protocol::Symbol,
    pub(crate) class: ConfiguredClass,
    pub(crate) price_precision: u8,
    pub(crate) size_precision: u8,
    pub(crate) price_increment: Decimal,
    pub(crate) size_increment: Decimal,
    pub(crate) margin: Option<ConfiguredMargin>,
    pub(crate) fees: Option<ConfiguredFees>,
    pub(crate) generator: Option<mogwai_data::GeneratorScalars>,
    #[serde(default = "default_session_profile")]
    pub(crate) session: mogwai_data::SessionProfile,
    pub(crate) calendar: Option<mogwai_data::SessionCalendar>,
}

fn default_session_profile() -> mogwai_data::SessionProfile {
    mogwai_data::Fingerprint::from_repo_json().session_profile
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ConfiguredClass {
    Spot {
        base: String,
        quote: String,
    },
    /// A share, held as a position and paid for in `currency`. NOT a spot pair
    /// with the ticker as its base: see `InstrumentClass::Equity`.
    /// `lot_size` is the round lot an order must be a multiple of, `borrowable`
    /// the shares this account may be short (absent means the venue models no
    /// borrow constraint, `0` states a hard-to-borrow name), and
    /// `settlement_ns` the span sale proceeds are held unsettled for - a fixed
    /// sim span rather than N sessions.
    Equity {
        currency: String,
        #[serde(default = "one")]
        multiplier: Decimal,
        #[serde(default = "one")]
        lot_size: Decimal,
        #[serde(default)]
        borrowable: Option<Decimal>,
        #[serde(default)]
        settlement_ns: u64,
    },
    Future {
        underlying: String,
        settlement_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
    /// A perpetual swap. `funding_interval_ns` and `funding_rate` are what make
    /// it one rather than a future; the eight-hour default is the near-universal
    /// convention, and a zero rate is legal and means a venue where longs and
    /// shorts happen to be balanced. `index_symbol` names the mark this
    /// perpetual funds against; absent, the rate stays at `funding_rate`.
    /// `funding_clamp` caps the computed rate; zero means no cap.
    Perpetual {
        underlying: String,
        settlement_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
        #[serde(default = "eight_hours_ns")]
        funding_interval_ns: u64,
        #[serde(default)]
        funding_rate: Decimal,
        #[serde(default)]
        index_symbol: Option<String>,
        #[serde(default)]
        funding_clamp: Decimal,
    },
    /// A coin-margined contract: quoted in `quote_currency`, settled in
    /// `settlement_currency`. The two must differ, or it is a linear contract
    /// and should be configured as one.
    Inverse {
        underlying: String,
        settlement_currency: String,
        quote_currency: String,
        multiplier: Decimal,
        asset_class: WireAssetClass,
    },
}

/// One share per contract, which is every venue that lists shares.
fn one() -> Decimal {
    Decimal::ONE
}

/// Eight hours, the funding interval essentially every perpetual venue uses.
fn eight_hours_ns() -> u64 {
    8 * 3_600 * 1_000_000_000
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredMargin {
    /// Read as a fixed amount of settlement currency per contract, or as a
    /// FRACTION OF NOTIONAL, according to `basis`. So `initial = 2000` under the
    /// default per-contract basis is CME's dollar performance bond, and
    /// `initial = 0.1` under `notional` is ten-times leverage.
    pub(crate) initial_per_contract: Decimal,
    pub(crate) maintenance_per_contract: Decimal,
    #[serde(default)]
    pub(crate) breach_action: BreachAction,
    #[serde(default)]
    pub(crate) basis: MarginBasis,
}

#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BreachAction {
    #[default]
    Refuse,
    Liquidate,
}

/// How a margin requirement is derived. See `mogwai_engine::MarginBasis`; this
/// is its config face, kept separate so the wire vocabulary and the engine's are
/// free to diverge.
#[derive(Debug, Clone, Copy, Default, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MarginBasis {
    /// A fixed amount per contract. Exchange-listed futures, and the default
    /// because every shipped preset states a performance bond that way.
    #[default]
    PerContract,
    /// A fraction of notional, so the requirement moves with the price. Forex,
    /// crypto margin, and Reg-T equity margin.
    Notional,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfiguredFees {
    pub(crate) maker: FeeRate,
    pub(crate) taker: FeeRate,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(tag = "basis", rename_all = "snake_case")]
pub(crate) enum FeeRate {
    BasisPoints { rate: Decimal },
    PerContract { amount: Decimal },
}

impl ConfiguredInstrument {
    /// The wire/engine-facing definition this table describes.
    pub(crate) fn def(&self) -> InstrumentDef {
        InstrumentDef {
            symbol: std::sync::Arc::clone(&self.symbol),
            class: match &self.class {
                ConfiguredClass::Spot { base, quote } => InstrumentClass::Spot {
                    base: base.clone(),
                    quote: quote.clone(),
                },
                ConfiguredClass::Future {
                    underlying,
                    settlement_currency,
                    multiplier,
                    asset_class,
                } => InstrumentClass::Future {
                    underlying: underlying.clone(),
                    settlement_currency: settlement_currency.clone(),
                    multiplier: *multiplier,
                    asset_class: *asset_class,
                },
                ConfiguredClass::Equity {
                    currency,
                    multiplier,
                    lot_size,
                    borrowable,
                    settlement_ns,
                } => InstrumentClass::Equity {
                    currency: currency.clone(),
                    multiplier: *multiplier,
                    lot_size: *lot_size,
                    borrowable: *borrowable,
                    settlement_ns: *settlement_ns,
                },
                ConfiguredClass::Perpetual {
                    underlying,
                    settlement_currency,
                    multiplier,
                    asset_class,
                    funding_interval_ns,
                    funding_rate,
                    index_symbol,
                    funding_clamp,
                } => InstrumentClass::Perpetual {
                    underlying: underlying.clone(),
                    settlement_currency: settlement_currency.clone(),
                    multiplier: *multiplier,
                    asset_class: *asset_class,
                    funding_interval_ns: *funding_interval_ns,
                    funding_rate: *funding_rate,
                    index_symbol: index_symbol.clone(),
                    funding_clamp: *funding_clamp,
                },
                ConfiguredClass::Inverse {
                    underlying,
                    settlement_currency,
                    quote_currency,
                    multiplier,
                    asset_class,
                } => InstrumentClass::Inverse {
                    underlying: underlying.clone(),
                    settlement_currency: settlement_currency.clone(),
                    quote_currency: quote_currency.clone(),
                    multiplier: *multiplier,
                    asset_class: *asset_class,
                },
            },
            price_precision: self.price_precision,
            size_precision: self.size_precision,
            price_increment: self.price_increment,
            size_increment: self.size_increment,
        }
    }
}

/// Resolves and validates EVERY shape this config can reach - the one it boots
/// and every `[symbols.*]` table, funding included - then returns only the boot
/// profile. The non-boot profiles are dropped: the point is the refusal. A
/// typo or an unfunded settlement currency under a table the run does not boot
/// would otherwise survive startup and surface as a runtime rejection that
/// looks like a trading outcome, which the funding ruling forbids.
pub fn build_instrument_profiles(cfg: &Config) -> anyhow::Result<source::InstrumentProfiles> {
    validate_symbol_keys(cfg)?;
    // The boot shape leads. With no `symbol` key that is `None`, which resolves
    // the default bundle, so the default shape is swept exactly when it is the
    // shape the run can reach. Configured keys follow in a stable order, so a
    // config with two bad tables always names the same one first.
    let mut configured: Vec<&str> = cfg
        .symbols
        .keys()
        .map(String::as_str)
        .filter(|key| {
            cfg.boot_symbol()
                .is_none_or(|boot| !key.eq_ignore_ascii_case(boot))
        })
        .collect();
    configured.sort_unstable();
    let mut resolved = Vec::new();
    for symbol in std::iter::once(cfg.boot_symbol()).chain(configured.into_iter().map(Some)) {
        let named = symbol.unwrap_or(DEFAULT_PRESET);
        let profile = profile_for(cfg, symbol)
            .with_context(|| format!("configured symbol {named} is invalid"))?;
        refuse_unfunded_settlement(cfg, &profile.def)
            .with_context(|| format!("configured symbol {named} cannot be funded"))?;
        resolved.push(profile);
    }
    // The reachable shape set is CLOSED at boot and wider than the configured
    // one: client-driven resolution can select any shipped preset by name, plus
    // the default bundle under the `[instrument]` overlay. Boot RESOLVES all of
    // them and records the unfundable ones; it does not refuse over them, since
    // that would force a BTCUSDT-only operator to fund USD forever. A request
    // landing on a barred shape is refused at bind instead.
    //
    // Empty balances mean funds checks are OFF for the whole run (see
    // `refuse_unfunded_settlement`), so nothing can be barred either - barring
    // there would refuse binds for a currency the engine never charges.
    let mut funding_barred = std::collections::HashSet::new();
    if !cfg.balances.is_empty() {
        let reachable = preset_names()
            .into_iter()
            .map(|name| {
                profile_for(cfg, Some(name))
                    .with_context(|| format!("reachable preset shape {name} is invalid"))
            })
            .chain([unconfigured_fallback_shape(cfg)
                .context("the unconfigured fallback shape is invalid")]);
        for profile in reachable {
            let currency = profile?.def.class.settlement_currency().to_owned();
            if !cfg.balances.contains_key(&currency) {
                funding_barred.insert(currency);
            }
        }
    }
    Ok(source::InstrumentProfiles::from_config(
        Arc::new(cfg.clone()),
        resolved,
        funding_barred,
    ))
}

fn validate_symbol_keys(cfg: &Config) -> anyhow::Result<()> {
    let mut normalized = HashMap::<String, &str>::new();
    for key in cfg.symbols.keys() {
        let folded = key.to_ascii_uppercase();
        if let Some(previous) = normalized.insert(folded, key) {
            anyhow::bail!(
                "symbols tables {previous:?} and {key:?} differ only in case; symbol table keys must be unique case-insensitively"
            );
        }
    }
    Ok(())
}

/// Renders a scalar refusal's optional detail as a trailing clause, or nothing
/// when the field name is the whole story. Kept out of the config-path position
/// so an operator reading `generator.<field>` sees only what they must edit.
fn detail_suffix(err: &mogwai_data::ScalarError) -> String {
    err.detail
        .map(|detail| format!(": {detail}"))
        .unwrap_or_default()
}

/// One validated [`source::InstrumentProfile`] from a deserialized
/// `[instrument]` table. Factored out of `build_instrument_profiles` so the
/// offline `gen` command can build the SAME profile from an embedded preset
/// rather than re-deriving the scalar defaulting, the modal-tick agreement
/// checks and the three validations - which is how `gen` came to be able to
/// chart only the built-in BTCUSDT venue.
fn profile_from_configured(
    configured: &ConfiguredInstrument,
    fp: &mogwai_data::Fingerprint,
) -> anyhow::Result<source::InstrumentProfile> {
    let def = configured.def();
    validate_instrument_def(&def)?;
    validate_instrument_options(configured, &def)?;

    let mut scalars = configured.generator.clone().unwrap_or_else(|| {
        mogwai_data::GeneratorScalars::from_fingerprint_medians(&def.symbol, fp)
    });
    scalars.symbol = def.symbol.to_string();
    if configured.generator.is_none() {
        scalars.modal_tick = def.price_increment;
        scalars.price_decimals = u32::from(def.price_precision);
        scalars.top_sizes = mogwai_data::TopOfBookSizes::uncalibrated(
            mogwai_data::SizeGrid::from_def(&def).min_size,
        );
    }
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
    for size in [scalars.top_sizes.bid, scalars.top_sizes.ask] {
        if size.normalize().scale() > u32::from(def.size_precision)
            || size % def.size_increment != Decimal::ZERO
        {
            anyhow::bail!(
                "instrument {} generator.top_sizes must be representable at size_precision and on size_increment",
                def.symbol
            );
        }
    }
    scalars.validate().map_err(|err| {
        // The bare `field` is the config PATH and must stay in the path
        // position; the optional detail says which of that field's checks
        // refused and is appended after the verb. Rendering the whole `Display`
        // inline produced `generator.children_single_frac (floor-branch active
        // solve infeasible) failed validation`, where the parenthetical reads
        // as part of the path an operator is about to go edit.
        anyhow::anyhow!(
            "instrument {} generator.{} failed validation{}",
            def.symbol,
            err.field,
            detail_suffix(&err)
        )
    })?;
    let size_grid = mogwai_data::SizeGrid::from_def(&def);
    scalars
        .validate_size_grid(size_grid.min_size)
        .map_err(|err| {
            anyhow::anyhow!(
                "instrument {} generator.{} failed size-grid validation{}",
                def.symbol,
                err.field,
                detail_suffix(&err)
            )
        })?;
    for diagnostic in scalars.empirical_diagnostics(fp, size_grid.multiplier) {
        tracing::warn!(
            code = diagnostic.code,
            field = diagnostic.field,
            corpus = diagnostic.corpus,
            symbol = %def.symbol,
            "generator scalar sits outside its empirical corpus range"
        );
    }
    for diagnostic in scalars.size_diagnostics(size_grid.min_size, size_grid.integral) {
        tracing::warn!(
            code = diagnostic.code,
            field = diagnostic.field,
            symbol = %def.symbol,
            "generator size grid hides almost all latent size variation"
        );
    }
    // Contextual, matching what `GeneratedSource` will apply later. This is the
    // gate a preset actually meets first: a calendar-conditional profile carries
    // relative factors that satisfy no particular sum, and validating it through
    // the legacy path would reject it here long before the source ever saw it.
    configured
        .session
        .validate_for(configured.calendar.is_some())
        .map_err(|err| anyhow::anyhow!(session_error_message(&def.symbol, err)))?;
    if let Some(calendar) = &configured.calendar {
        calendar.validate().map_err(|err| {
            anyhow::anyhow!(
                "instrument {} calendar failed validation: {}",
                def.symbol,
                err.0
            )
        })?;
    }

    Ok(source::InstrumentProfile::new(
        def,
        scalars,
        configured.session.clone(),
        configured.margin.clone(),
        configured.fees.clone(),
        configured.calendar.clone(),
    ))
}

/// The profile a named embedded preset resolves to, for callers that have no
/// operator config: `mogwai gen --symbol MNQ`. Goes through `effective_preset`
/// so preset inheritance (MES over MNQ) and the provenance completeness check
/// apply exactly as they do at boot.
pub fn profile_from_preset(name: &str) -> anyhow::Result<source::InstrumentProfile> {
    let (merged, _provenance) = effective_preset(name)?;
    let configured: ConfiguredInstrument = merged
        .try_into()
        .with_context(|| format!("preset {name} does not deserialize as an instrument"))?;
    profile_from_configured(&configured, mogwai_data::Fingerprint::repo())
}

/// The profile a symbol resolves to without an operator overlay. Total over
/// symbol strings: an unmatched symbol uses the default bundle under its own
/// name.
pub fn profile_for_symbol(symbol: &str) -> anyhow::Result<source::InstrumentProfile> {
    profile_for_config(Some(symbol), Vec::new())
}

/// The validated profile a symbol resolves to under this config. This is the
/// seam slice 2 needs: when the symbol arrives per request, the venue calls it
/// with the requested symbol and nothing else changes.
pub fn profile_for(
    cfg: &Config,
    symbol: Option<&str>,
) -> anyhow::Result<source::InstrumentProfile> {
    let merged = resolve_instrument_named(symbol, cfg.overlays_for(symbol))?;
    profile_from_merged(merged)
}

/// The validated profile selected by a symbol and ordered operator overlays.
pub fn profile_for_config(
    symbol: Option<&str>,
    overlays: Vec<toml::Table>,
) -> anyhow::Result<source::InstrumentProfile> {
    let merged = resolve_instrument(symbol, overlays)?;
    profile_from_merged(merged)
}

fn profile_from_merged(merged: toml::Table) -> anyhow::Result<source::InstrumentProfile> {
    let configured: ConfiguredInstrument = merged
        .try_into()
        .context("the resolved [instrument] table is not a valid instrument")?;
    profile_from_configured(&configured, mogwai_data::Fingerprint::repo())
}

fn validate_instrument_options(
    configured: &ConfiguredInstrument,
    def: &InstrumentDef,
) -> anyhow::Result<()> {
    match (&def.class, &configured.margin) {
        (InstrumentClass::Spot { .. }, Some(_)) => anyhow::bail!(
            "instrument {} margin is valid only for a future",
            def.symbol
        ),
        (InstrumentClass::Future { .. }, None) => {
            anyhow::bail!("instrument {} future requires a margin table", def.symbol)
        }
        // A future with a margin table: an initial below maintenance is a
        // config that opens every position already in breach.
        (_, Some(margin))
            if margin.maintenance_per_contract <= Decimal::ZERO
                || margin.initial_per_contract < margin.maintenance_per_contract =>
        {
            anyhow::bail!(
                "instrument {} margin initial_per_contract must be at least positive maintenance_per_contract",
                def.symbol
            )
        }
        _ => {}
    }
    if let Some(fees) = &configured.fees {
        for rate in [fees.maker, fees.taker] {
            match rate {
                FeeRate::BasisPoints { rate }
                    if rate < Decimal::ZERO || rate > Decimal::from(1000) =>
                {
                    anyhow::bail!(
                        "instrument {} fee basis points must be between 0 and 1000",
                        def.symbol
                    )
                }
                FeeRate::PerContract { amount } if amount < Decimal::ZERO => anyhow::bail!(
                    "instrument {} per-contract fee must not be negative",
                    def.symbol
                ),
                _ => {}
            }
        }
    }
    Ok(())
}

/// The checks every DERIVATIVE class shares: a usable underlying, a usable
/// settlement currency, a positive multiplier, and whole-contract sizing.
fn validate_derivative(
    def: &InstrumentDef,
    underlying: &str,
    settlement_currency: &str,
    multiplier: Decimal,
    whole_contracts: bool,
) -> anyhow::Result<()> {
    if underlying.trim().is_empty()
        || !underlying.is_ascii()
        || underlying.len() > mogwai_protocol::MAX_SYMBOL_LEN
    {
        anyhow::bail!(
            "instrument {} underlying must be non-blank ASCII within MAX_SYMBOL_LEN",
            def.symbol
        );
    }
    if settlement_currency.trim().is_empty()
        || settlement_currency.len() > mogwai_protocol::MAX_CURRENCY_LEN
    {
        anyhow::bail!("instrument {} settlement_currency is invalid", def.symbol);
    }
    if multiplier <= Decimal::ZERO || multiplier.scale() > 9 {
        anyhow::bail!(
            "instrument {} multiplier must be positive with scale <= 9",
            def.symbol
        );
    }
    // EXCHANGE-LISTED derivatives trade whole contracts; a CME future or a
    // coin-margined contract denominated in fixed quote units cannot be
    // fractionally sized. A crypto PERPETUAL can and routinely is - Binance
    // sizes BTCUSDT.P in thousandths - so requiring whole contracts of one would
    // refuse the most common perpetual on the largest venue.
    if whole_contracts && (def.size_increment != Decimal::ONE || def.size_precision != 0) {
        anyhow::bail!(
            "instrument {} derivative size_increment must be 1 and size_precision must be 0",
            def.symbol
        );
    }
    Ok(())
}

pub(crate) fn validate_instrument_def(def: &InstrumentDef) -> anyhow::Result<()> {
    if def.symbol.trim().is_empty() {
        anyhow::bail!("instrument symbol must not be empty");
    }
    match &def.class {
        InstrumentClass::Spot { base, quote } => {
            if base.trim().is_empty() {
                anyhow::bail!("instrument {} base must not be empty", def.symbol);
            }
            if quote.trim().is_empty() {
                anyhow::bail!("instrument {} quote must not be empty", def.symbol);
            }
            if base.len() > mogwai_protocol::MAX_CURRENCY_LEN
                || quote.len() > mogwai_protocol::MAX_CURRENCY_LEN
            {
                anyhow::bail!(
                    "instrument {} base/quote exceeds MAX_CURRENCY_LEN ({})",
                    def.symbol,
                    mogwai_protocol::MAX_CURRENCY_LEN
                );
            }
        }
        InstrumentClass::Future {
            underlying,
            settlement_currency,
            multiplier,
            ..
        } => {
            if underlying.trim().is_empty()
                || !underlying.is_ascii()
                || underlying.len() > mogwai_protocol::MAX_SYMBOL_LEN
            {
                anyhow::bail!(
                    "instrument {} future underlying must be non-blank ASCII within MAX_SYMBOL_LEN",
                    def.symbol
                );
            }
            if settlement_currency.trim().is_empty()
                || settlement_currency.len() > mogwai_protocol::MAX_CURRENCY_LEN
            {
                anyhow::bail!("instrument {} settlement_currency is invalid", def.symbol);
            }
            if *multiplier <= Decimal::ZERO || multiplier.scale() > 9 {
                anyhow::bail!(
                    "instrument {} multiplier must be positive with scale <= 9",
                    def.symbol
                );
            }
            if def.size_increment != Decimal::ONE || def.size_precision != 0 {
                anyhow::bail!(
                    "instrument {} futures size_increment must be 1 and size_precision must be 0",
                    def.symbol
                );
            }
        }
        InstrumentClass::Equity {
            currency,
            multiplier,
            lot_size,
            borrowable,
            settlement_ns: _,
        } => {
            if currency.trim().is_empty() || currency.len() > mogwai_protocol::MAX_CURRENCY_LEN {
                anyhow::bail!("instrument {} equity currency is invalid", def.symbol);
            }
            if *multiplier <= Decimal::ZERO || multiplier.scale() > 9 {
                anyhow::bail!(
                    "instrument {} multiplier must be positive with scale <= 9",
                    def.symbol
                );
            }
            // A lot is a whole number of shares, and a zero or fractional one
            // would make every order either unrepresentable or unconstrained.
            if *lot_size <= Decimal::ZERO || lot_size.fract() != Decimal::ZERO {
                anyhow::bail!(
                    "instrument {} lot_size must be a positive whole number of shares",
                    def.symbol
                );
            }
            // A NEGATIVE borrow is not a smaller one, it is a nonsense: the
            // field states how many shares may be shorted, and zero already
            // says none.
            if borrowable.is_some_and(|shares| shares < Decimal::ZERO) {
                anyhow::bail!(
                    "instrument {} borrowable must not be negative; state 0 for a name that \
                     cannot be borrowed at all",
                    def.symbol
                );
            }
            // Whole shares. Fractional-share programmes exist, but every lot,
            // borrow and settlement convention an equity surface owes is stated
            // in shares, so admitting a fraction here would be admitting a
            // quantity nothing downstream can size against.
            if def.size_increment != Decimal::ONE || def.size_precision != 0 {
                anyhow::bail!(
                    "instrument {} equity size_increment must be 1 and size_precision must be 0",
                    def.symbol
                );
            }
        }
        InstrumentClass::Perpetual {
            underlying,
            settlement_currency,
            multiplier,
            funding_interval_ns,
            funding_rate,
            index_symbol,
            funding_clamp,
            ..
        } => {
            validate_derivative(def, underlying, settlement_currency, *multiplier, false)?;
            if *funding_interval_ns == 0 {
                anyhow::bail!(
                    "instrument {} funding_interval_ns must be positive; a perpetual that never \
                     funds is a future, and should be configured as one",
                    def.symbol
                );
            }
            // A rate above 100 percent per interval is a configuration mistake
            // rather than a market: the largest funding any venue has printed is
            // orders of magnitude below it, and the arithmetic is applied to
            // notional, so a typo here empties an account in one interval.
            if funding_rate.abs() > Decimal::ONE {
                anyhow::bail!(
                    "instrument {} funding_rate must be within +/-1 (100 percent per interval)",
                    def.symbol
                );
            }
            if *funding_clamp < Decimal::ZERO || *funding_clamp > Decimal::ONE {
                anyhow::bail!(
                    "instrument {} funding_clamp must be in 0..=1; zero means no cap",
                    def.symbol
                );
            }
            if let Some(index) = index_symbol {
                if index.trim().is_empty() {
                    anyhow::bail!(
                        "instrument {} index_symbol must be a non-empty symbol if set",
                        def.symbol
                    );
                }
                if mogwai_protocol::validate_wire_symbol(index).is_err() {
                    anyhow::bail!(
                        "instrument {} index_symbol {index} is not a legal symbol",
                        def.symbol
                    );
                }
            }
        }
        InstrumentClass::Inverse {
            underlying,
            settlement_currency,
            quote_currency,
            multiplier,
            ..
        } => {
            validate_derivative(def, underlying, settlement_currency, *multiplier, true)?;
            if quote_currency.trim().is_empty()
                || quote_currency.len() > mogwai_protocol::MAX_CURRENCY_LEN
            {
                anyhow::bail!("instrument {} quote_currency is invalid", def.symbol);
            }
            if quote_currency == settlement_currency {
                anyhow::bail!(
                    "instrument {} is inverse but quotes and settles in the same currency; that \
                     is a linear contract, and should be configured as one",
                    def.symbol
                );
            }
        }
    }
    // Symbol, base and quote all reach the wire - the symbol on every tick,
    // order event and position row, the currencies on every balance row - so
    // the sizing constants the admission reservation is built on are only upper
    // bounds if the configured strings are capped. Startup is the right place
    // to refuse: a connection can then never out-produce its own reservation.
    //
    // THE SAME VALIDATOR THE WIRE USES, and that is the point rather than an
    // implementation detail. A configured symbol is reached by clients through
    // `/trades`, `/quotes` and order entry, and `validate_submit_order` holds
    // those to the URL-safe alphabet - so a config checked only for LENGTH could
    // name a symbol the venue serves and no client can trade or fetch, with both
    // validators green and neither able to see the other's rule. Ruled
    // 2026-08-20: one alphabet, read from one function, on both sides. It
    // refuses nothing any shipped preset or test config does.
    if let Err(why) = mogwai_protocol::validate_wire_symbol(&def.symbol) {
        anyhow::bail!(
            "instrument {} symbol is not a legal symbol: {why}",
            def.symbol
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

/// Nanoseconds since the Unix epoch - the venue's clock, fed into the engine.
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

/// The run's clock. The epoch is fixed by config alone, while the wall anchor
/// decides only when a tick is delivered. `speed == 0.0` is unpaced delivery,
/// but the axis advances at wall rate for deadlines, sweeps and volatility.
pub(crate) fn build_run_clock(cfg: &Config, boot_wall_ns: u64) -> anyhow::Result<SimClock> {
    validate_speed(cfg)?;
    Ok(SimClock {
        sim_epoch_ns: source::TAPE_ORIGIN_NS.saturating_add(cfg.warmup_ns),
        wall_anchor_ns: boot_wall_ns,
        speed: if cfg.speed == 0.0 { 1.0 } else { cfg.speed },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_account(id: &str) -> Config {
        Config {
            account_id: id.to_owned(),
            ..Config::default()
        }
    }

    /// The protocol-11 fanout policy exception, pinned: the standing resize
    /// formula proposed 16,777,216 and both reviewers rejected it (eagerly
    /// allocated ring state for a 1.4 percent measured ratio, and the
    /// rejected capacity deterministically breaks the accept-before-fill
    /// serving invariant: a socket whose ring lapses can observe a fill for
    /// an order whose accept it never received). A later mechanical
    /// application of a generated proposal must fail HERE and be argued,
    /// not slip through as bookkeeping.
    #[test]
    fn the_fanout_default_carries_the_protocol_11_exception() {
        assert_eq!(Config::default().fanout_depth, 1_048_576);
    }

    #[test]
    fn invalid_speed_is_rejected_by_load_time_validation() {
        let mut cfg = Config {
            speed: f64::NAN,
            ..Config::default()
        };
        assert!(validate_speed(&cfg).is_err());
        cfg.speed = -1.0;
        assert!(validate_speed(&cfg).is_err());
    }

    /// The venue reported a bare `MOGWAI` for one release, which is a legal
    /// `mogwai_protocol::AccountId` and an ILLEGAL nautilus one - so every run
    /// booted cleanly and was refused by its consumer, which could not name an
    /// account that satisfied both sides. The wire type will not catch this
    /// because by its own rules there is nothing wrong; only this check will.
    #[test]
    fn an_account_id_nautilus_cannot_parse_is_refused_at_load() {
        let err = validate_account_id(&with_account("MOGWAI"))
            .expect_err("a bare word is not a nautilus AccountId");
        let message = err.to_string();
        assert!(message.contains("ISSUER-NUMBER"), "{message}");
        assert!(message.contains(DEFAULT_ACCOUNT_ID), "{message}");

        for empty_side in ["-001", "MOGWAI-"] {
            assert!(
                validate_account_id(&with_account(empty_side)).is_err(),
                "{empty_side} has an empty side"
            );
        }
    }

    #[test]
    fn a_host_supplied_account_id_is_accepted() {
        // The point of the key: a consumer holds an account of its own naming
        // and asserts the venue reports the same one.
        for id in [DEFAULT_ACCOUNT_ID, "MOGWAI-QA2", "SANDBOX-001"] {
            validate_account_id(&with_account(id)).unwrap_or_else(|err| panic!("{id}: {err}"));
        }
    }

    #[test]
    fn the_default_account_id_satisfies_its_own_rule() {
        validate_account_id(&Config::default()).expect("the shipped default must be usable");
    }

    fn future_configured() -> ConfiguredInstrument {
        let profile = profile_for_symbol("BTCUSDT").expect("BTCUSDT preset must resolve");
        ConfiguredInstrument {
            symbol: "MNQ".into(),
            class: ConfiguredClass::Future {
                underlying: "NQ".into(),
                settlement_currency: "USD".into(),
                multiplier: Decimal::from(2),
                asset_class: WireAssetClass::Index,
            },
            price_precision: 2,
            size_precision: 0,
            price_increment: Decimal::new(25, 2),
            size_increment: Decimal::ONE,
            margin: Some(ConfiguredMargin {
                initial_per_contract: Decimal::from(2000),
                maintenance_per_contract: Decimal::from(1800),
                breach_action: BreachAction::Refuse,
                basis: MarginBasis::PerContract,
            }),
            fees: None,
            generator: Some(profile.scalars),
            session: profile.session,
            calendar: None,
        }
    }

    #[test]
    fn a_future_without_a_generator_derives_top_sizes_from_its_grid() {
        let mut configured = future_configured();
        configured.generator = None;

        let mut fp = mogwai_data::Fingerprint::from_repo_json();
        fp.cadence.targets.mean_trade_notional.anchor = 100_000.0;
        let profile = profile_from_configured(&configured, &fp)
            .expect("an absent generator uses instrument-grid defaults");

        assert_eq!(profile.scalars.top_sizes.bid, Decimal::ONE);
        assert_eq!(profile.scalars.top_sizes.ask, Decimal::ONE);
    }

    #[test]
    fn a_configured_seed_at_the_signed_maximum_round_trips() {
        let text = format!("seed = {}", i64::MAX);
        let cfg: Config = toml::from_str(&text).expect("maximum signed seed parses");
        assert_eq!(cfg.seed, Some(i64::MAX as u64));
        for _ in 0..128 {
            assert!(rand::random::<u64>() >> 1 <= i64::MAX as u64);
        }
    }

    #[test]
    fn a_config_naming_a_removed_clock_key_is_refused() {
        let err = toml::from_str::<Config>("sim_epoch_ns = 1")
            .expect_err("removed clock key must be refused");
        assert!(err.to_string().contains("sim_epoch_ns"), "{err}");
    }

    /// The heartbeat knob was `server_heartbeat_ms` until the venue ruling
    /// retired `server` as a name for the process. `deny_unknown_fields` is
    /// what makes the break loud for an operator carrying an old file, and
    /// this pins that: a silently ignored key would run the venue with
    /// heartbeats off while the file says they are on.
    #[test]
    fn a_config_naming_the_retired_heartbeat_key_is_refused() {
        let err = toml::from_str::<Config>("server_heartbeat_ms = 1000")
            .expect_err("the retired heartbeat key must be refused");
        assert!(err.to_string().contains("server_heartbeat_ms"), "{err}");
        let cfg: Config =
            toml::from_str("venue_heartbeat_ms = 1000").expect("the current key parses");
        assert_eq!(cfg.venue_heartbeat_ms, 1000);
    }

    /// An unfunded quote currency means every buy in that shape rejects for
    /// insufficient balance for the whole run. That is a misconfigured run, so
    /// it fails BOOT rather than warning - and it is checked for every
    /// configured shape, not only the one the boot river carries.
    #[test]
    fn an_unfunded_quote_currency_refuses_boot() {
        let cfg = Config {
            balances: HashMap::from([("EUR".to_string(), Decimal::from(1))]),
            ..Config::default()
        };
        let defs = mogwai_protocol::default_instruments();
        let err =
            refuse_unfunded_settlement(&cfg, &defs[0]).expect_err("an unfunded quote refuses boot");
        assert!(err.to_string().contains("unfunded"), "{err}");

        refuse_unfunded_settlement(&Config::default(), &defs[0])
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
        refuse_unfunded_settlement(&cfg, &mogwai_protocol::default_instruments()[0])
            .expect("an explicitly unfunded run is allowed");
    }

    #[test]
    fn a_future_with_a_non_unit_size_increment_refuses_boot() {
        let mut configured = future_configured();
        configured.size_increment = Decimal::from(2);
        let err = validate_instrument_def(&configured.def()).expect_err("non-unit increment");
        assert!(err.to_string().contains("size_increment"), "{err}");
    }

    #[test]
    fn a_future_with_an_unfunded_settlement_currency_refuses_boot() {
        let configured = future_configured();
        let cfg = Config {
            balances: HashMap::from([("EUR".into(), Decimal::ONE)]),
            ..Config::default()
        };
        let err = refuse_unfunded_settlement(&cfg, &configured.def())
            .expect_err("unfunded settlement must refuse");
        assert!(err.to_string().contains("USD"), "{err}");
    }

    #[test]
    fn a_margin_table_with_initial_below_maintenance_refuses_boot() {
        let mut configured = future_configured();
        configured.margin.as_mut().unwrap().initial_per_contract = Decimal::from(1700);
        let err = validate_instrument_options(&configured, &configured.def())
            .expect_err("initial below maintenance");
        assert!(err.to_string().contains("initial_per_contract"), "{err}");
    }

    #[test]
    fn a_negative_fee_rate_refuses_boot() {
        let mut configured = future_configured();
        configured.fees = Some(ConfiguredFees {
            maker: FeeRate::PerContract {
                amount: -Decimal::ONE,
            },
            taker: FeeRate::PerContract {
                amount: Decimal::ZERO,
            },
        });
        let err = validate_instrument_options(&configured, &configured.def())
            .expect_err("negative fee must refuse");
        assert!(err.to_string().contains("negative"), "{err}");
    }

    #[test]
    fn a_config_with_top_level_base_and_quote_refuses_boot_naming_the_class_table() {
        // Through the loader's own path, because that is where the legible
        // message lives; a raw `toml::from_str::<Config>` answers "unknown
        // field `base`", which names neither the replacement table nor its
        // shape, and asserting on THAT would pass no matter what we told the
        // operator.
        let table: toml::Table = toml::from_str("base = \"BTC\"\nquote = \"USDT\"\n").unwrap();
        let message = resolve_instrument(None, vec![table])
            .expect_err("the removed flat class shape must refuse")
            .to_string();
        assert!(message.contains("instrument.class"), "{message}");
        assert!(message.contains("kind = \"spot\""), "{message}");
    }

    #[test]
    fn every_shipped_preset_parses_and_validates() {
        let fp = mogwai_data::Fingerprint::from_repo_json();
        for name in ["MNQ", "MES", "BTCUSDT"] {
            let (_, provenance) = effective_preset(name).unwrap();
            let table =
                toml::Table::from_iter([("preset".into(), toml::Value::String(name.into()))]);
            let resolved = resolve_instrument(None, vec![table]).unwrap();
            let configured: ConfiguredInstrument = toml::Value::Table(resolved).try_into().unwrap();
            validate_instrument_def(&configured.def()).unwrap();
            validate_instrument_options(&configured, &configured.def()).unwrap();
            let profile = profile_from_configured(&configured, &fp).unwrap();
            assert_preset_diagnostics(name, &profile, &fp, &provenance).unwrap();
        }
    }

    #[test]
    fn an_unmatched_symbol_resolves_through_the_default_preset() {
        let resolved = profile_for_symbol("FOOBAR").expect("unmatched symbols are total");
        let default = profile_from_preset(DEFAULT_PRESET).expect("default preset resolves");
        assert_eq!(resolved.def.symbol.as_ref(), "FOOBAR");
        assert_eq!(resolved.def.class, default.def.class);
        assert_eq!(resolved.def.price_precision, default.def.price_precision);
        assert_eq!(resolved.def.size_precision, default.def.size_precision);
        assert_eq!(resolved.def.price_increment, default.def.price_increment);
        assert_eq!(resolved.def.size_increment, default.def.size_increment);
    }

    #[test]
    fn a_symbol_naming_a_preset_selects_it() {
        let resolved = profile_for_symbol("MNQ").unwrap();
        let preset = profile_from_preset("MNQ").unwrap();
        assert_eq!(resolved.def, preset.def);
        assert_eq!(
            format!("{:?}", resolved.scalars),
            format!("{:?}", preset.scalars)
        );
    }

    #[test]
    fn an_operator_preset_beats_a_matching_symbol() {
        let operator = toml::from_str("preset = \"BTCUSDT\"").unwrap();
        let resolved = profile_for_config(Some("MNQ"), vec![operator]).unwrap();
        assert_eq!(resolved.def.symbol.as_ref(), "MNQ");
        assert!(matches!(resolved.def.class, InstrumentClass::Spot { .. }));
    }

    #[test]
    fn a_top_level_key_overrides_the_resolved_bundle() {
        let operator = toml::from_str(
            "margin = { initial_per_contract = \"2100\", maintenance_per_contract = \"1800\", breach_action = \"liquidate\" }",
        )
        .unwrap();
        let resolved = profile_for_config(Some("MNQ"), vec![operator]).unwrap();
        assert_eq!(
            resolved.margin.unwrap().initial_per_contract,
            Decimal::from(2100)
        );
    }

    #[test]
    fn a_top_level_optional_section_the_bundle_lacks_is_added() {
        // No shipped preset sets `fees`, so a must-already-exist rule would put
        // a fee schedule out of every operator's reach. `tests/configs/fees.toml`
        // is exactly this shape and drives the smoke test's fees arm.
        let operator = toml::from_str(
            "[fees.maker]\nbasis = \"per_contract\"\namount = \"0.20\"\n\
             [fees.taker]\nbasis = \"per_contract\"\namount = \"0.25\"\n",
        )
        .unwrap();
        let resolved =
            profile_for_config(Some("MNQ"), vec![operator]).expect("a fee schedule is addable");
        assert!(resolved.fees.is_some());
    }

    #[test]
    fn a_top_level_key_that_is_not_a_field_is_still_refused() {
        let operator = toml::from_str("price_precison = 3").unwrap();
        let error = profile_for_config(Some("MNQ"), vec![operator])
            .expect_err("a typo at the top level must not boot");
        assert!(
            format!("{error:#}").contains("price_precison"),
            "the refusal must name the key: {error:#}"
        );
    }

    #[test]
    fn a_top_level_override_of_a_coupled_key_must_state_both() {
        let lone = toml::from_str("price_precision = 3").unwrap();
        let error = profile_for_config(Some("MNQ"), vec![lone]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("generator.price_decimals must equal price_precision"),
            "{error}"
        );
        let paired =
            toml::from_str("price_precision = 3\n[override]\n\"generator.price_decimals\" = 3")
                .unwrap();
        profile_for_config(Some("MNQ"), vec![paired]).expect("both coupled halves boot");
    }

    #[test]
    fn an_override_path_the_bundle_does_not_set_is_still_refused() {
        let operator = toml::from_str("[override]\n\"class.typo\" = \"x\"").unwrap();
        let error = profile_for_config(Some("FOOBAR"), vec![operator]).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("class.typo"), "{message}");
        assert!(message.contains(DEFAULT_PRESET), "{message}");
    }

    #[test]
    fn an_explicit_unknown_preset_is_still_an_error() {
        let explicit = toml::from_str("preset = \"NOPE\"").unwrap();
        assert!(profile_for_config(Some("MNQ"), vec![explicit]).is_err());
        profile_for_symbol("NOPE").expect("an unmatched symbol is legal");
    }

    #[test]
    fn a_lowercase_symbol_matches_its_preset() {
        let resolved = profile_for_symbol("mnq").unwrap();
        let preset = profile_from_preset("MNQ").unwrap();
        assert_eq!(resolved.def.symbol.as_ref(), "mnq");
        assert_eq!(resolved.def.class, preset.def.class);
        assert_eq!(resolved.scalars.modal_tick, preset.scalars.modal_tick);
    }

    #[test]
    fn a_boot_symbol_at_top_level_selects_its_preset() {
        let cfg: Config = toml::from_str("symbol = \"MNQ\"").unwrap();
        assert_eq!(
            profile_for(&cfg, cfg.boot_symbol()).unwrap().def,
            profile_from_preset("MNQ").unwrap().def
        );
    }

    #[test]
    fn default_knobs_apply_to_an_unmatched_symbol() {
        let cfg: Config =
            toml::from_str("symbol = \"FOOBAR\"\n[instrument]\nprice_increment = \"0.02\"\n")
                .unwrap();
        let profile = profile_for(&cfg, cfg.boot_symbol()).unwrap();
        assert_eq!(profile.def.symbol.as_ref(), "FOOBAR");
        assert_eq!(profile.def.price_increment, Decimal::new(2, 2));
    }

    #[test]
    fn per_symbol_knobs_beat_default_knobs() {
        let cfg: Config = toml::from_str(
            "symbol = \"MNQ\"\n[instrument]\nprice_increment = \"0.50\"\n[symbols.MNQ]\nprice_increment = \"0.25\"\n",
        )
        .unwrap();
        assert_eq!(
            profile_for(&cfg, cfg.boot_symbol())
                .unwrap()
                .def
                .price_increment,
            Decimal::new(25, 2)
        );
    }

    #[test]
    fn a_per_symbol_preset_beats_the_default_preset_key() {
        let cfg: Config = toml::from_str(
            "symbol = \"X\"\n[instrument]\npreset = \"MNQ\"\n[symbols.X]\npreset = \"MES\"\n",
        )
        .unwrap();
        let profile = profile_for(&cfg, cfg.boot_symbol()).unwrap();
        assert_eq!(profile.def.symbol.as_ref(), "X");
        assert_eq!(
            profile.def.class,
            profile_from_preset("MES").unwrap().def.class
        );
    }

    #[test]
    fn a_config_carrying_two_symbol_tables_parses_and_resolves_both() {
        let cfg: Config = toml::from_str(
            "[symbols.MNQ]\nmargin = { initial_per_contract = \"2100\", maintenance_per_contract = \"1800\", breach_action = \"liquidate\" }\n[symbols.BTCUSDT]\nprice_increment = \"0.02\"\n",
        )
        .unwrap();
        assert_eq!(
            profile_for(&cfg, Some("MNQ"))
                .unwrap()
                .margin
                .unwrap()
                .initial_per_contract,
            Decimal::from(2100)
        );
        assert_eq!(
            profile_for(&cfg, Some("BTCUSDT"))
                .unwrap()
                .def
                .price_increment,
            Decimal::new(2, 2)
        );
    }

    /// ONE ALPHABET, READ BY BOTH SIDES. A configured symbol is reached by
    /// clients through `/trades`, `/quotes` and order entry, and the wire holds
    /// those to the URL-safe alphabet. A config checked only for LENGTH could
    /// therefore name a shape the venue serves and no client can trade, with
    /// both validators green and neither able to see the other's rule.
    ///
    /// Asserted against `validate_wire_symbol`'s OWN verdict rather than
    /// against a second copy of the alphabet spelled out here: a hand-built
    /// case list on this side would pin config against itself, which is exactly
    /// the drift the shared validator exists to prevent.
    #[test]
    fn a_configured_symbol_is_held_to_the_alphabet_the_wire_enforces() {
        let cfg: Config = toml::from_str("").unwrap();
        for illegal in ["MNQ!", "MNQ ", "MN/Q", "MNQ#1"] {
            assert!(
                mogwai_protocol::validate_wire_symbol(illegal).is_err(),
                "the premise of this case is that the wire refuses {illegal}"
            );
            // Named rather than unwrapped: the Ok side is a whole
            // `InstrumentProfile`, and `unwrap_err` would report this failure as
            // several screens of scalars with the point buried in them.
            let Err(error) = profile_for(&cfg, Some(illegal)) else {
                panic!(
                    "{illegal} resolved to a served shape, so the venue would serve a symbol no \
                     client can trade or fetch"
                );
            };
            let error = error.to_string();
            assert!(
                error.contains("not a legal symbol"),
                "a symbol no client could trade was configured anyway: {error}"
            );
        }
        // The negative half, without which the loop above passes for a config
        // path that refuses every symbol it is given.
        assert!(
            profile_for(&cfg, Some("MNQ")).is_ok(),
            "a legal symbol must still resolve"
        );
    }

    #[test]
    fn a_typo_in_a_per_symbol_override_is_still_refused() {
        let cfg: Config =
            toml::from_str("[symbols.MNQ.override]\n\"class.typo\" = \"x\"\n").unwrap();
        let error = profile_for(&cfg, Some("MNQ")).unwrap_err().to_string();
        assert!(
            error.contains("class.typo") && error.contains("MNQ"),
            "{error}"
        );
    }

    #[test]
    fn a_lowercase_boot_symbol_finds_its_uppercase_symbols_table() {
        let cfg: Config = toml::from_str(
            "symbol = \"mnq\"\n[symbols.MNQ]\nmargin = { initial_per_contract = \"2100\", maintenance_per_contract = \"1800\", breach_action = \"liquidate\" }\n",
        )
        .unwrap();
        assert_eq!(
            profile_for(&cfg, cfg.boot_symbol())
                .unwrap()
                .margin
                .unwrap()
                .initial_per_contract,
            Decimal::from(2100)
        );
    }

    #[test]
    fn two_symbol_tables_differing_only_in_case_are_refused() {
        let cfg: Config = toml::from_str(
            "[balances]\n[symbols.mnq]\npreset = \"MNQ\"\n[symbols.MNQ]\npreset = \"MES\"\n",
        )
        .unwrap();
        let error = build_instrument_profiles(&cfg).unwrap_err().to_string();
        assert!(error.contains("mnq") && error.contains("MNQ"), "{error}");
    }

    #[test]
    fn an_invalid_non_boot_symbol_table_refuses_at_boot() {
        let cfg: Config = toml::from_str(
            "symbol = \"MNQ\"\n[balances]\nUSD = \"1\"\n[symbols.X.override]\n\"class.typo\" = \"x\"\n",
        )
        .unwrap();
        let error = format!("{:#}", build_instrument_profiles(&cfg).unwrap_err());
        assert!(error.contains("configured symbol X"), "{error}");
        assert!(error.contains("class.typo"), "{error}");
    }

    #[test]
    fn an_unfunded_non_boot_symbol_refuses_at_boot() {
        let cfg: Config = toml::from_str(
            "symbol = \"BTCUSDT\"\n[balances]\nUSDT = \"1\"\n[symbols.X]\npreset = \"MNQ\"\n",
        )
        .unwrap();
        let error = format!("{:#}", build_instrument_profiles(&cfg).unwrap_err());
        assert!(
            error.contains("configured symbol X cannot be funded"),
            "{error}"
        );
        assert!(error.contains("unfunded"), "{error}");
    }

    /// The boot shape's own funding is checked by the boot sweep. `serve` used
    /// to ask this separately over the single built def; the sweep replaced
    /// that call, so it has to cover the boot shape whether or not the symbol
    /// also has a `[symbols.*]` table.
    #[test]
    fn an_unfunded_boot_symbol_refuses_at_boot() {
        let cfg: Config = toml::from_str("symbol = \"MNQ\"\n[balances]\nUSDT = \"1\"\n").unwrap();
        let error = format!("{:#}", build_instrument_profiles(&cfg).unwrap_err());
        assert!(error.contains("unfunded"), "{error}");
    }

    #[test]
    fn an_unfunded_preset_shape_is_barred_not_refused() {
        let profiles =
            build_instrument_profiles(&Config::default()).expect("USDT-only boot succeeds");
        let error = profiles.resolve("MNQ").expect_err("USD preset is barred");
        assert!(
            matches!(error, source::ResolveRefusal::FundingBarred { ref currency, .. } if currency == "USD")
        );
    }

    #[test]
    fn funding_every_preset_bars_nothing() {
        let mut cfg = Config::default();
        cfg.balances.insert("USD".to_owned(), Decimal::ONE);
        let profiles = build_instrument_profiles(&cfg).unwrap();
        for symbol in preset_names() {
            profiles.resolve(symbol).unwrap();
        }
        profiles.resolve("FOOBAR").unwrap();
    }

    #[test]
    fn a_no_config_run_serves_the_default_preset() {
        let profiles = build_instrument_profiles(&Config::default()).unwrap();
        let resolved = profiles
            .configured(DEFAULT_PRESET)
            .expect("one default profile");
        let preset = profile_from_preset(DEFAULT_PRESET).unwrap();
        assert_eq!(resolved.def, preset.def);
        assert_eq!(
            format!("{:?}", resolved.scalars),
            format!("{:?}", preset.scalars)
        );
        assert_eq!(
            format!("{:?}", resolved.session),
            format!("{:?}", preset.session)
        );
    }

    #[test]
    fn every_shipped_preset_quotes_a_positive_integral_width() {
        for name in ["MNQ", "MES", "BTCUSDT"] {
            let profile = profile_from_preset(name).unwrap();
            assert!(profile.scalars.quoted_width.ticks().get() > 0, "{name}");
        }
    }

    #[test]
    fn quote_seam_provenance_matches_the_protocol_10_landing() {
        // The placeholder era is over for the futures: MNQ's quote seams
        // are FITTED from the July TBBO month and MES inherits them loudly
        // as the standing stopgap; the crypto preset remains uncalibrated,
        // since no quote evidence covers spot. The fitted corpus strings
        // must name the MNQ evidence, so an MES reader can see the borrow.
        use mogwai_data::CalibrationProvenance;
        let name = "BTCUSDT";
        let profile = profile_from_preset(name).unwrap();
        assert_eq!(
            profile.scalars.quoted_width.provenance(),
            &CalibrationProvenance::Uncalibrated,
            "{name} quoted width"
        );
        assert_eq!(
            profile.scalars.top_sizes.provenance,
            CalibrationProvenance::Uncalibrated,
            "{name} top sizes"
        );
        assert_eq!(
            profile.scalars.trade_displacement_ticks.provenance(),
            &CalibrationProvenance::Uncalibrated,
            "{name} trade displacement"
        );
        for name in ["MNQ", "MES"] {
            let profile = profile_from_preset(name).unwrap();
            for (seam, provenance) in [
                ("quoted width", profile.scalars.quoted_width.provenance()),
                (
                    "trade displacement",
                    profile.scalars.trade_displacement_ticks.provenance(),
                ),
                ("top sizes", &profile.scalars.top_sizes.provenance),
            ] {
                match provenance {
                    CalibrationProvenance::Fitted { corpus } => assert!(
                        corpus.contains("MNQ.v.0"),
                        "{name} {seam}: the fitted corpus must name the MNQ \
                         evidence, got {corpus:?}"
                    ),
                    other => panic!(
                        "{name} {seam}: expected fitted provenance, got \
                         {other:?}"
                    ),
                }
            }
        }
    }

    #[test]
    fn quote_sizes_are_on_the_instrument_grid() {
        for name in ["MNQ", "MES", "BTCUSDT"] {
            let profile = profile_from_preset(name).unwrap();
            for size in [profile.scalars.top_sizes.bid, profile.scalars.top_sizes.ask] {
                assert!(size >= profile.def.size_increment, "{name}: {size}");
                assert_eq!(
                    size % profile.def.size_increment,
                    Decimal::ZERO,
                    "{name}: {size}"
                );
                if profile.def.class.is_future() {
                    assert_eq!(size.fract(), Decimal::ZERO, "{name}: {size}");
                }
            }
        }
    }

    #[test]
    fn uncalibrated_top_sizes_are_configuration_not_an_override_signal() {
        let profile = profile_from_preset("MNQ").unwrap();
        let mut scalars = profile.scalars.clone();
        scalars.top_sizes = mogwai_data::TopOfBookSizes::uncalibrated(Decimal::TEN);
        let rebuilt = crate::source::InstrumentProfile::new(
            profile.def,
            scalars,
            profile.session,
            profile.margin,
            profile.fees,
            profile.calendar,
        );
        assert_eq!(rebuilt.scalars.top_sizes.bid, Decimal::TEN);
        assert_eq!(rebuilt.scalars.top_sizes.ask, Decimal::TEN);
    }

    /// Both directions of the provenance contract: a diagnostic that fires
    /// without an acceptance refuses, and an acceptance that no longer fires
    /// refuses as stale.
    ///
    /// The arms moved on 2026-08-08 when `empirical_ranges.modal_tick.max`
    /// was corrected from 0.25 to 0.1. Before that, MNQ's 0.25 tick sat
    /// exactly ON the inclusive ceiling, so it cleared the range check and
    /// the shipped preset carried no acceptance for it - which is what let
    /// this test use `modal_tick` as its example of an UNACCEPTED diagnostic.
    /// With the corrected ceiling the tick is genuinely outside the crypto
    /// corpus envelope, the diagnostic fires honestly, and the preset accepts
    /// it in provenance. So the unaccepted arm now needs a different knob.
    #[test]
    fn shipped_preset_diagnostics_require_exact_provenance_acceptance() {
        let fp = mogwai_data::Fingerprint::from_repo_json();
        let (instrument, mut provenance) = effective_preset("MNQ").unwrap();
        let configured: ConfiguredInstrument = toml::Value::Table(instrument).try_into().unwrap();
        let mut profile = profile_from_configured(&configured, &fp).unwrap();

        // As shipped: the tick is outside the corpus range, the diagnostic
        // fires, and provenance accepts it. This is the accepted arm.
        assert_preset_diagnostics("MNQ", &profile, &fp, &provenance).unwrap();

        // UNACCEPTED: drop the acceptance the preset ships and the same
        // diagnostic must refuse.
        let entry = provenance
            .get_mut("generator.modal_tick")
            .and_then(toml::Value::as_table_mut)
            .unwrap();
        entry.remove("accepted_diagnostics");
        let error = assert_preset_diagnostics("TEST", &profile, &fp, &provenance).unwrap_err();
        assert!(error.to_string().contains("unaccepted"), "{error}");

        // STALE: put the acceptance back, then move the tick to a value
        // INSIDE the corpus range so the diagnostic stops firing. An
        // acceptance for a diagnostic that no longer fires is itself a
        // refusal - provenance may not claim a warning it does not carry.
        let entry = provenance
            .get_mut("generator.modal_tick")
            .and_then(toml::Value::as_table_mut)
            .unwrap();
        entry.insert(
            "accepted_diagnostics".into(),
            toml::Value::Array(vec![toml::Value::String(
                "outside-empirical-corpus-range".into(),
            )]),
        );
        // The corpus median, comfortably inside [1e-7, 0.1].
        profile.scalars.modal_tick = Decimal::new(1, 4);
        let error = assert_preset_diagnostics("TEST", &profile, &fp, &provenance).unwrap_err();
        assert!(error.to_string().contains("stale acceptances"), "{error}");
    }

    #[test]
    fn every_shipped_preset_declares_provenance_for_every_knob_it_sets() {
        for name in ["MNQ", "MES", "BTCUSDT"] {
            let (instrument, provenance) = effective_preset(name).unwrap();
            validate_provenance(name, &instrument, &provenance).unwrap();
        }
    }

    #[test]
    fn a_preset_with_incomplete_provenance_refuses_boot() {
        let instrument: toml::Table =
            toml::from_str("symbol = \"MNQ\"\nprice_precision = 2").unwrap();
        let provenance: toml::Table =
            toml::from_str("symbol = { kind = \"declared\", rationale = \"test symbol\" }")
                .unwrap();
        let error = validate_provenance("BROKEN", &instrument, &provenance).unwrap_err();
        assert!(error.to_string().contains("price_precision"), "{error}");
    }

    #[test]
    fn an_instrument_table_naming_a_symbol_is_refused() {
        let table = toml::Table::from_iter([
            ("preset".into(), toml::Value::String("MNQ".into())),
            ("symbol".into(), toml::Value::String("MNQ".into())),
        ]);
        let error = resolve_instrument(None, vec![table]).unwrap_err();
        assert!(
            error.to_string().contains("top-level symbol key"),
            "{error}"
        );
    }

    #[test]
    fn an_override_of_a_path_the_preset_does_not_set_refuses_boot() {
        let table: toml::Table =
            toml::from_str("preset = \"MNQ\"\n[override]\n\"class.typo\" = \"x\"\n").unwrap();
        assert!(
            resolve_instrument(None, vec![table])
                .unwrap_err()
                .to_string()
                .contains("class.typo")
        );
    }

    #[test]
    fn an_override_table_entry_wins_and_is_logged_with_both_values() {
        let table: toml::Table =
            toml::from_str("preset = \"MNQ\"\n[override]\n\"class.multiplier\" = \"3\"\n").unwrap();
        let configured: ConfiguredInstrument =
            toml::Value::Table(resolve_instrument(None, vec![table]).unwrap())
                .try_into()
                .unwrap();
        assert_eq!(configured.def().class.multiplier(), Decimal::from(3));
    }

    #[test]
    fn fitted_mnq_effective_values_are_the_artifact_values() {
        // The protocol-10 landing pin: every fitted MNQ effective value is
        // the literal the fit artifact analysis/mnq-fit.json recorded for
        // job GLBX-20260805-HAPEWPABKG. A calibration-loop iteration that
        // moves any candidate value must re-bless this test in the same
        // change, so the preset can never drift from the artifact silently.
        let profile = profile_from_preset("MNQ").unwrap();
        let s = &profile.scalars;
        assert_eq!(s.mean_event_duration_s, 0.060859305487494256);
        assert_eq!(s.children_mean, 1.1711127211559897);
        assert_eq!(s.children_single_frac, 0.9048983982868222);
        assert_eq!(s.levels_mean, 1.1215513514243831);
        assert_eq!(s.start_price, Decimal::new(2828400, 2));
        assert_eq!(s.quoted_width.ticks().get(), 2);
        assert_eq!(s.top_sizes.bid, Decimal::from(3));
        assert_eq!(s.top_sizes.ask, Decimal::from(3));
        assert_eq!(s.trade_displacement_ticks.ticks(), 0.5161290322580645);
        // The three declared knobs carry the frozen solvers' BEST
        // CANDIDATES as the closest representable approximations (the size
        // family missed one gate, p99 10 vs bound 9.6; vol_scalar is the
        // protocol-11 re-solve under the fitted session arrays, passing its
        // pooled RMS gate but not the minute-range envelope), and
        // size_round_frac stays structurally unidentifiable. All four are
        // pinned so any later fit must show up here.
        assert_eq!(s.latent_size_median, Decimal::new(1097264, 6));
        assert_eq!(s.size_log_sigma, 0.9333333333333333);
        assert_eq!(s.vol_scalar, 0.000013570223097752063);
        assert_eq!(s.size_round_frac, 0.20856767610054022);

        // The protocol-11 session arrays: the fitted per-parent hourly
        // scale and the conditional arrival curve, materialized exactly as
        // analysis/mnq-fit.json recorded them, with dow_weight untouched
        // at its NQ-bar values.
        assert_eq!(
            profile.session.intensity_hour,
            [
                0.788959, 0.606801, 0.476029, 0.404415, 0.33337, 0.389075, 0.370589, 0.385735,
                0.425944, 0.289761, 0.286266, 0.428253, 0.677683, 3.200686, 4.138113, 2.546961,
                1.723787, 1.314281, 1.292247, 1.624038, 0.470938, 1.0, 0.33058, 0.357795,
            ]
        );
        assert_eq!(
            profile.session.vol_hour,
            [
                1.110005, 1.07247, 1.059488, 0.991918, 1.024188, 1.091588, 1.090355, 1.119903,
                1.1612, 1.12323, 1.075567, 1.044929, 1.036215, 0.975501, 0.939166, 0.871242,
                0.845352, 0.82815, 0.81192, 0.840464, 0.939231, 1.0, 1.001102, 0.931623,
            ]
        );
        assert_eq!(
            profile.session.dow_weight,
            [1.5179, 0.9080, 0.9865, 1.0157, 1.0535, 1.0225, 1.0000]
        );

        // The provenance map is half the landing: values alone could pass
        // while a knob silently reverts to declared. Every landed path must
        // read fitted with the exact Brick L corpus literal and window.
        let (_, provenance) = effective_preset("MNQ").unwrap();
        let corpus = "MNQ.v.0 GLBX.MDP3 TBBO, job GLBX-20260805-HAPEWPABKG";
        let window = "2026-07 full month, 22 usable sessions";
        for path in [
            "generator.mean_event_duration_s",
            "generator.children_mean",
            "generator.children_single_frac",
            "generator.levels_mean",
            "generator.start_price",
            "generator.quoted_width.ticks",
            "generator.top_sizes.bid",
            "generator.top_sizes.ask",
            "generator.trade_displacement_ticks.ticks",
        ] {
            let entry = provenance
                .get(path)
                .and_then(toml::Value::as_table)
                .unwrap();
            assert_eq!(
                entry.get("kind").and_then(toml::Value::as_str),
                Some("fitted"),
                "{path}"
            );
            assert_eq!(
                entry.get("corpus").and_then(toml::Value::as_str),
                Some(corpus),
                "{path}"
            );
            assert_eq!(
                entry.get("window").and_then(toml::Value::as_str),
                Some(window),
                "{path}"
            );
        }
        // The protocol-11 session provenance: both refitted arrays read
        // fitted with the July TBBO corpus and window; dow_weight keeps its
        // NQ-bar lineage.
        for (path, needle) in [
            ("session.intensity_hour", "conditional hour parameter"),
            ("session.vol_hour", "per-parent trimmed mean absolute"),
        ] {
            let entry = provenance
                .get(path)
                .and_then(toml::Value::as_table)
                .unwrap();
            assert_eq!(
                entry.get("kind").and_then(toml::Value::as_str),
                Some("fitted"),
                "{path}"
            );
            let corpus_text = entry.get("corpus").and_then(toml::Value::as_str).unwrap();
            assert!(corpus_text.starts_with(corpus), "{path}");
            assert!(corpus_text.contains(needle), "{path}");
            assert_eq!(
                entry.get("window").and_then(toml::Value::as_str),
                Some(window),
                "{path}"
            );
        }
        let dow = provenance
            .get("session.dow_weight")
            .and_then(toml::Value::as_table)
            .unwrap();
        assert!(
            dow.get("corpus")
                .and_then(toml::Value::as_str)
                .unwrap()
                .starts_with("NQ one-minute"),
            "dow_weight keeps its NQ-bar provenance"
        );
        // The declared set is asserted just as exactly: the size pair,
        // vol_scalar and the unidentifiable round fraction must all read
        // declared - a fitted claim appearing on any of them is drift.
        for path in [
            "generator.latent_size_median",
            "generator.size_log_sigma",
            "generator.vol_scalar",
            "generator.size_round_frac",
        ] {
            let entry = provenance
                .get(path)
                .and_then(toml::Value::as_table)
                .unwrap();
            assert_eq!(
                entry.get("kind").and_then(toml::Value::as_str),
                Some("declared"),
                "{path}"
            );
        }
    }

    #[test]
    fn mes_inherits_the_mnq_fit_loudly() {
        // MES borrows the MNQ fit as a stated stopgap (fit spec section 6):
        // every generator value except the identity overrides must equal
        // MNQ's fitted effective values EXACTLY, and the fitted corpus
        // strings must name the MNQ evidence so no MES corpus is implied.
        // The named ES/MES purchase is the route to ending the borrow; the
        // NQ/MNQ proxy FAIL proves family resemblance is not
        // interchangeability, so nothing here claims transfer validity.
        let mnq = profile_from_preset("MNQ").unwrap();
        let mes = profile_from_preset("MES").unwrap();
        assert_eq!(mes.def.symbol.as_ref(), "MES");
        assert_eq!(mes.scalars.symbol, "MES");
        assert_eq!(mes.scalars.start_price, Decimal::from(6000));
        assert_eq!(mes.def.class.multiplier(), Decimal::from(5));
        match &mes.def.class {
            mogwai_protocol::InstrumentClass::Future { underlying, .. } => {
                assert_eq!(underlying, "ES");
            }
            other => panic!("MES must be a future, got {other:?}"),
        }
        let borrowed = |s: &mogwai_data::GeneratorScalars| {
            (
                s.modal_tick,
                s.price_decimals,
                s.mean_event_duration_s,
                s.children_mean,
                s.children_single_frac,
                s.levels_mean,
                s.size_round_frac,
                s.latent_size_median,
                s.size_log_sigma,
                s.vol_scalar,
            )
        };
        assert_eq!(borrowed(&mes.scalars), borrowed(&mnq.scalars));
        assert_eq!(
            mes.scalars.quoted_width.ticks(),
            mnq.scalars.quoted_width.ticks()
        );
        assert_eq!(
            mes.scalars.quoted_width.provenance(),
            mnq.scalars.quoted_width.provenance()
        );
        assert_eq!(mes.scalars.top_sizes.bid, mnq.scalars.top_sizes.bid);
        assert_eq!(mes.scalars.top_sizes.ask, mnq.scalars.top_sizes.ask);
        assert_eq!(
            mes.scalars.top_sizes.provenance,
            mnq.scalars.top_sizes.provenance
        );
        assert_eq!(
            mes.scalars.trade_displacement_ticks.ticks(),
            mnq.scalars.trade_displacement_ticks.ticks()
        );
        assert_eq!(
            mes.scalars.trade_displacement_ticks.provenance(),
            mnq.scalars.trade_displacement_ticks.provenance()
        );
        // The session tables and calendar carry neither PartialEq nor
        // Serialize; their Debug renderings pin the inheritance just as
        // tightly - every field is a plain number.
        assert_eq!(format!("{:?}", mes.session), format!("{:?}", mnq.session));
        assert_eq!(format!("{:?}", mes.calendar), format!("{:?}", mnq.calendar));

        // The borrow must be loud in the provenance map too: every borrowed
        // entry is MNQ's verbatim - a MIXTURE of fitted entries naming the
        // MNQ corpus and declared entries carrying the solver's best
        // candidates - and the identity overrides stay declared.
        let (_, mnq_prov) = effective_preset("MNQ").unwrap();
        let (_, mes_prov) = effective_preset("MES").unwrap();
        for (path, entry) in &mnq_prov {
            if matches!(
                path.as_str(),
                "symbol"
                    | "class.underlying"
                    | "class.multiplier"
                    | "generator.symbol"
                    | "generator.start_price"
            ) {
                continue;
            }
            assert_eq!(mes_prov.get(path), Some(entry), "{path}");
        }
        for path in [
            "symbol",
            "class.underlying",
            "class.multiplier",
            "generator.symbol",
            "generator.start_price",
        ] {
            let entry = mes_prov.get(path).and_then(toml::Value::as_table).unwrap();
            assert_eq!(
                entry.get("kind").and_then(toml::Value::as_str),
                Some("declared"),
                "{path}"
            );
        }
    }

    #[test]
    fn the_mnq_preset_reads_two_dollars_per_point_and_fifty_cents_per_tick() {
        let table = toml::Table::from_iter([("preset".into(), toml::Value::String("MNQ".into()))]);
        let configured: ConfiguredInstrument =
            toml::Value::Table(resolve_instrument(None, vec![table]).unwrap())
                .try_into()
                .unwrap();
        let def = configured.def();
        assert_eq!(def.class.multiplier(), Decimal::from(2));
        assert_eq!(def.tick_value(), Decimal::new(50, 2));
    }

    #[test]
    fn a_boot_symbol_that_is_not_first_alphabetically_is_the_one_readied() {
        let cfg: Config = toml::from_str("symbol = \"MNQ\"\n[balances]\nUSD = \"100000\"\nUSDT = \"100000\"\n[symbols.BTCUSDT]\npreset = \"BTCUSDT\"\n").unwrap();
        let profiles = build_instrument_profiles(&cfg).unwrap();
        assert_eq!(profiles.instrument_defs().len(), 2);
        assert_eq!(
            profiles
                .boot_symbol_def(cfg.boot_symbol())
                .unwrap()
                .symbol
                .as_ref(),
            "MNQ"
        );
    }

    #[test]
    fn an_unset_boot_symbol_resolves_the_default_shape_among_several() {
        let cfg: Config = toml::from_str(
            "[balances]\nUSD = \"100000\"\nUSDT = \"100000\"\n[symbols.MNQ]\npreset = \"MNQ\"\n",
        )
        .unwrap();
        let profiles = build_instrument_profiles(&cfg).unwrap();
        assert_eq!(profiles.instrument_defs().len(), 2);
        assert_eq!(
            profiles.boot_symbol_def(None).unwrap().symbol.as_ref(),
            "BTCUSDT"
        );
    }
}
